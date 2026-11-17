//! #1161 PR5b: `POST /mcp` のテスト（docs/chronogazer-agent-api-design.md §8 の
//! ChronoGazer の MCP）。
//!
//! どれも**本物の `api_router`**（`crate::rest::tests` のヘルパーが組む物）に HTTP の
//! 要求を通す（`rest/api_key_tests.rs` と同じ作法）。`/mcp` が CSRF の層の外にあり、
//! キーだけを受けることを、層ごと確かめるため。

use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::db::Db;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use banto_core::ListParams;
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::api_keys::ApiKeysService;
use crate::audit::AuditLogService;
use crate::rest::tests::router_with_role_tokens_audit_pool_data_dir_and_spool;
use crate::settings::{ServerSettings, SettingsService};

struct Fixture {
    router: Router,
    audit: AuditLogService,
    keys: ApiKeysService,
    pool: sqlx::SqlitePool,
    admin: String,
}

async fn fixture() -> Fixture {
    let (router, audit, pool, admin, _editor, _viewer) =
        router_with_role_tokens_audit_pool_data_dir_and_spool(
            PathBuf::from("unused-in-tests"),
            None,
        )
        .await;
    Fixture {
        router,
        audit,
        keys: ApiKeysService::new(pool.clone()),
        pool,
        admin,
    }
}

impl Fixture {
    /// キーを発行して `(平文, 監査の主体の表記)` を返す。
    async fn issue(&self, name: &str, scopes: &[&str]) -> (String, String) {
        let issued = self
            .keys
            .issue(name, scopes.iter().map(|s| s.to_string()).collect(), None)
            .await
            .expect("issue");
        let label = format!("{}（{}）", issued.name, issued.prefix);
        (issued.key, label)
    }

    /// `POST /mcp`（CSRF のヘッダーは付けない）。`bearer` が `None` なら
    /// `Authorization` も付けない。本文は生のバイト列。
    async fn post_raw(&self, bearer: Option<&str>, body: &str) -> (StatusCode, Value) {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("Content-Type", "application/json");
        if let Some(bearer) = bearer {
            builder = builder.header("Authorization", format!("Bearer {bearer}"));
        }
        let response = self
            .router
            .clone()
            .oneshot(builder.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn rpc(&self, key: &str, method: &str, params: Value) -> (StatusCode, Value) {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        self.post_raw(Some(key), &body.to_string()).await
    }

    /// `tools/call` の結果（`result`）を返す。HTTP 200 で JSON-RPC のエラーが
    /// 無いことも確かめる。
    async fn call(&self, key: &str, tool: &str, arguments: Value) -> Value {
        let (status, body) = self
            .rpc(
                key,
                "tools/call",
                json!({ "name": tool, "arguments": arguments }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{tool}: {body}");
        assert!(body.get("error").is_none(), "{tool}: {body}");
        body["result"].clone()
    }

    /// 成功したツールの `text` を JSON として返す。
    async fn call_ok(&self, key: &str, tool: &str, arguments: Value) -> Value {
        let result = self.call(key, tool, arguments).await;
        assert_eq!(result["isError"], false, "{tool}: {result}");
        let text = result["content"][0]["text"].as_str().expect("text");
        serde_json::from_str(text).unwrap_or_else(|err| panic!("{tool}: {err}: {text}"))
    }

    async fn audit_rows(&self) -> Vec<crate::audit::AuditLogEntry> {
        self.audit.list(ListParams::default()).await.unwrap().rows
    }

    async fn tool_names(&self, key: &str) -> Vec<String> {
        let (_, body) = self.rpc(key, "tools/list", json!({})).await;
        body["result"]["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    }
}

fn detail_of(entry: &crate::audit::AuditLogEntry) -> Value {
    entry
        .detail
        .as_deref()
        .and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or(Value::Null)
}

fn result_text(result: &Value) -> &str {
    result["content"][0]["text"].as_str().unwrap_or_default()
}

fn slmp_connection(name: &str) -> Value {
    json!({
        "name": name, "protocol": "slmp", "enabled": true,
        "settings": {"host": "127.0.0.1", "port": 5000, "cpu_type": "Q"}
    })
}

/// 設定を変えるツール（結果に `restartRequired: true` が付く物）。
const CHANGING_TOOLS: [&str; 14] = [
    "create_connection",
    "update_connection",
    "delete_connection",
    "create_group",
    "update_group",
    "delete_group",
    "create_tag",
    "update_tag",
    "delete_tag",
    "create_display_group",
    "update_display_group",
    "delete_display_group",
    "reorder_display_groups",
    "set_threshold",
];

/// §8: ツールごとの成功（`tools/list` のすべてを 1 回以上呼ぶ）・設定の変更の
/// 結果に `restartRequired: true`（収集の操作には無い）・監査が `origin: "mcp"`・
/// `api_key`・`名前（prefix）`。CSRF のヘッダーは付けない。
#[tokio::test]
async fn a_config_key_calls_every_tool_and_changes_are_audited_as_mcp() {
    let fx = fixture().await;
    let (key, label) = fx.issue("agent", &["config"]).await;
    let mut called: BTreeSet<String> = BTreeSet::new();
    let mut results: Vec<(String, Value)> = Vec::new();
    macro_rules! ok {
        ($tool:expr, $args:expr) => {{
            let value = fx.call_ok(&key, $tool, $args).await;
            called.insert($tool.to_string());
            results.push(($tool.to_string(), value.clone()));
            value
        }};
    }

    // 収集の操作（レジストリが空なので開始しても機器に繋ぎに行かない）。
    let started = ok!("start_collection", json!({}));
    assert!(started.get("restartRequired").is_none(), "{started}");
    ok!("get_status", json!({}));
    ok!("read_values", json!({}));
    ok!("restart_collection", json!({}));
    ok!("stop_collection", json!({}));

    let drivers = ok!("list_drivers", json!({}));
    assert!(drivers["drivers"].as_array().is_some_and(|d| !d.is_empty()));

    // 接続先。
    let created = ok!("create_connection", slmp_connection("c1"));
    let conn_id = created["created"]["id"].as_i64().expect("id");
    let mut update = slmp_connection("c1-renamed");
    update["id"] = json!(conn_id);
    let updated = ok!("update_connection", update);
    assert_eq!(updated["updated"]["name"], "c1-renamed");
    let got = ok!("get_connection", json!({ "id": conn_id }));
    assert_eq!(got["connection"]["name"], "c1-renamed");
    let listed = ok!("list_connections", json!({}));
    assert_eq!(listed["connections"].as_array().unwrap().len(), 1);

    // 収集グループ。
    let group = ok!(
        "create_group",
        json!({ "name": "g1", "plcConnectionId": conn_id, "periodMs": 1000 })
    );
    let group_id = group["created"]["id"].as_i64().unwrap();
    ok!(
        "update_group",
        json!({ "id": group_id, "name": "g1b", "plcConnectionId": conn_id, "periodMs": 2000 })
    );
    let got = ok!("get_group", json!({ "id": group_id }));
    assert_eq!(got["group"]["periodMs"], 2000);
    ok!("list_groups", json!({}));

    // 記録項目。
    let tag = ok!(
        "create_tag",
        json!({ "name": "t1", "collectionGroupId": group_id, "address": "D100", "dataType": "i16" })
    );
    let tag_id = tag["created"]["id"].as_i64().unwrap();
    let revision = tag["created"]["revision"].as_i64().unwrap();
    ok!(
        "update_tag",
        json!({
            "id": tag_id, "name": "t1b", "collectionGroupId": group_id, "address": "D100",
            "dataType": "i16", "unit": "℃", "expectedRevision": revision
        })
    );
    let got = ok!("get_tag", json!({ "id": tag_id }));
    assert_eq!(got["tag"]["unit"], "℃");
    ok!("list_tags", json!({}));

    // しきい値。
    let set = ok!(
        "set_threshold",
        json!({ "tagId": tag_id, "thresholdH": 80.0, "thresholdHh": 90.0 })
    );
    assert_eq!(set["updated"]["thresholdHh"], 90.0);
    let got = ok!("get_threshold", json!({ "tagId": tag_id }));
    assert_eq!(got["threshold"]["thresholdH"], 80.0);
    let listed = ok!("list_thresholds", json!({}));
    assert_eq!(listed["thresholds"].as_array().unwrap().len(), 1);

    // 表示グループ。
    let dg = ok!(
        "create_display_group",
        json!({ "name": "L1", "kind": "trend", "pens": [{ "tagId": tag_id }] })
    );
    let dg_id = dg["created"]["id"].as_i64().unwrap();
    let dg2 = ok!(
        "create_display_group",
        json!({ "name": "L2", "kind": "gauge" })
    );
    let dg2_id = dg2["created"]["id"].as_i64().unwrap();
    ok!(
        "update_display_group",
        json!({ "id": dg_id, "name": "L1b", "kind": "digital", "pens": [] })
    );
    let got = ok!("get_display_group", json!({ "id": dg_id }));
    assert_eq!(got["displayGroup"]["kind"], "digital");
    let reordered = ok!("reorder_display_groups", json!({ "ids": [dg2_id, dg_id] }));
    assert_eq!(reordered["displayGroups"][0]["id"], dg2_id);
    ok!("list_display_groups", json!({}));

    // 削除（confirm: true）。
    ok!(
        "delete_display_group",
        json!({ "id": dg2_id, "confirm": true })
    );
    ok!(
        "delete_display_group",
        json!({ "id": dg_id, "confirm": true })
    );
    let deleted = ok!("delete_tag", json!({ "id": tag_id, "confirm": true }));
    assert_eq!(deleted["deleted"], true);
    assert_eq!(deleted["id"], tag_id);
    ok!("delete_group", json!({ "id": group_id, "confirm": true }));
    ok!(
        "delete_connection",
        json!({ "id": conn_id, "confirm": true })
    );

    // すべてのツールを呼んだ。
    let all: BTreeSet<String> = fx.tool_names(&key).await.into_iter().collect();
    assert_eq!(called, all, "every tool is called at least once");

    // restartRequired は設定を変えるツールだけ。
    for (tool, value) in &results {
        let expected = CHANGING_TOOLS.contains(&tool.as_str());
        assert_eq!(
            value.get("restartRequired") == Some(&json!(true)),
            expected,
            "{tool}: {value}"
        );
    }

    // 監査: 成功はすべて mcp・api_key・名前（prefix）。REST と同じ action・resource。
    let rows = fx.audit_rows().await;
    let mine: Vec<_> = rows
        .iter()
        .filter(|r| r.actor_username.as_deref() == Some(label.as_str()))
        .collect();
    for entry in &mine {
        assert_eq!(entry.origin, "mcp", "{entry:?}");
        assert_eq!(entry.actor_role.as_deref(), Some("api_key"), "{entry:?}");
        assert_eq!(entry.result, "ok", "{entry:?}");
    }
    let seen: BTreeSet<(String, String)> = mine
        .iter()
        .map(|r| (r.action.clone(), r.resource.clone()))
        .collect();
    for (action, resource) in [
        ("create", "plc_connections"),
        ("update", "plc_connections"),
        ("delete", "plc_connections"),
        ("create", "collection_groups"),
        ("update", "collection_groups"),
        ("delete", "collection_groups"),
        ("create", "tags"),
        ("update", "tags"),
        ("delete", "tags"),
        ("update", "tag_thresholds"),
        ("create", "display_groups"),
        ("update", "display_groups"),
        ("delete", "display_groups"),
        ("reorder", "display_groups"),
        ("start", "collect"),
        ("stop", "collect"),
        ("restart", "collect"),
    ] {
        assert!(
            seen.contains(&(action.to_string(), resource.to_string())),
            "missing {action} {resource}: {seen:?}"
        );
    }
    let conn_update = mine
        .iter()
        .find(|r| r.action == "update" && r.resource == "plc_connections")
        .unwrap();
    assert_eq!(
        conn_update.entity_id.as_deref(),
        Some(conn_id.to_string().as_str())
    );
    assert_eq!(detail_of(conn_update)["name"], "c1-renamed");
}

/// §8: `read` のキーでの設定の変更・収集の操作はツールのエラー（`isError`）で、
/// `denied` が `{tool}`・`mcp` で残る。何も変わらない。
#[tokio::test]
async fn a_read_key_cannot_change_anything_and_the_denial_names_the_tool() {
    let fx = fixture().await;
    let (key, label) = fx.issue("reader", &["read"]).await;

    for (tool, args, resource) in [
        ("create_connection", slmp_connection("x"), "plc_connections"),
        ("delete_tag", json!({ "id": 1, "confirm": true }), "tags"),
        (
            "reorder_display_groups",
            json!({ "ids": [] }),
            "display_groups",
        ),
        ("set_threshold", json!({ "tagId": 1 }), "tag_thresholds"),
        ("stop_collection", json!({}), "collect"),
    ] {
        let result = fx.call(&key, tool, args).await;
        assert_eq!(result["isError"], true, "{tool}: {result}");
        assert!(result_text(&result).starts_with("forbidden"), "{result}");
        let rows = fx.audit_rows().await;
        let entry = rows
            .iter()
            .find(|r| r.action == "denied" && detail_of(r)["tool"] == tool)
            .unwrap_or_else(|| panic!("no denied for {tool}: {rows:?}"));
        assert_eq!(entry.origin, "mcp");
        assert_eq!(entry.resource, resource);
        assert_eq!(entry.result, "denied");
        assert_eq!(entry.actor_username.as_deref(), Some(label.as_str()));
        assert_eq!(entry.actor_role.as_deref(), Some("api_key"));
        assert_eq!(detail_of(entry), json!({ "tool": tool }));
    }
    let listed = fx.call_ok(&key, "list_connections", json!({})).await;
    assert_eq!(listed["connections"], json!([]));
}

/// 読み取りのツールも主体のスコープを確かめる（`read_tool!`）。発行の口では作れない
/// スコープの無いキーを表を直接書き換えて作り、**すべての**ツールが `isError` に
/// なって `denied`（`{tool}`・`mcp`）が残ることを確かめる。読み取りのツールを
/// 確かめずに足したら、ここで落ちる。
#[tokio::test]
async fn a_key_without_scopes_is_refused_by_every_tool() {
    let fx = fixture().await;
    let (key, _) = fx.issue("nothing", &["read"]).await;
    let (config_key, _) = fx.issue("lister", &["config"]).await;
    sqlx::query("UPDATE api_keys SET scopes = '[]' WHERE name = 'nothing'")
        .execute(&fx.pool)
        .await
        .unwrap();

    let names = fx.tool_names(&config_key).await;
    for tool in &names {
        let args = json!({ "id": 1, "tagId": 1, "confirm": true, "ids": [] });
        let (status, body) = fx
            .rpc(
                &key,
                "tools/call",
                json!({ "name": tool, "arguments": args }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{tool}: {body}");
        // 入力の形の誤り（JSON-RPC の -32602）より前にスコープで止まる物と、
        // 入力を読んでから ops で止まる物がある。どちらでも中身には届かない。
        if let Some(error) = body.get("error") {
            assert_eq!(error["code"], -32602, "{tool}: {body}");
            continue;
        }
        let result = &body["result"];
        assert_eq!(result["isError"], true, "{tool}: {result}");
        assert!(
            result_text(result).starts_with("forbidden"),
            "{tool}: {result}"
        );
        let rows = fx.audit_rows().await;
        assert!(
            rows.iter().any(|r| r.action == "denied"
                && r.origin == "mcp"
                && detail_of(r)["tool"] == tool.as_str()),
            "no denied for {tool}"
        );
    }
    // 読み取りのツールは入力を読む前に止まるので、必ず denied が残る。
    let rows = fx.audit_rows().await;
    for tool in [
        "get_status",
        "list_connections",
        "get_connection",
        "list_drivers",
        "list_groups",
        "get_group",
        "list_tags",
        "get_tag",
        "list_display_groups",
        "get_display_group",
        "list_thresholds",
        "get_threshold",
        "read_values",
    ] {
        assert!(
            rows.iter()
                .any(|r| r.action == "denied" && detail_of(r)["tool"] == tool),
            "read tool {tool} did not check the scope"
        );
    }
}

/// §8: `confirm` の無い削除は断り、何も消さず、監査もしない。
#[tokio::test]
async fn deletes_without_confirm_are_refused() {
    let fx = fixture().await;
    let (key, _) = fx.issue("agent", &["config"]).await;
    let created = fx
        .call_ok(&key, "create_connection", slmp_connection("keep"))
        .await;
    let id = created["created"]["id"].as_i64().unwrap();
    let before = fx.audit_rows().await.len();

    for tool in [
        "delete_connection",
        "delete_group",
        "delete_tag",
        "delete_display_group",
    ] {
        for args in [json!({ "id": id }), json!({ "id": id, "confirm": false })] {
            let result = fx.call(&key, tool, args).await;
            assert_eq!(result["isError"], true, "{tool}: {result}");
            assert!(
                result_text(&result).starts_with("confirm_required"),
                "{tool}: {result}"
            );
        }
    }
    assert_eq!(fx.audit_rows().await.len(), before, "nothing is audited");
    let listed = fx.call_ok(&key, "list_connections", json!({})).await;
    assert_eq!(listed["connections"].as_array().unwrap().len(), 1);
}

/// §5.3・§7: `/mcp` は `cg_` のキーだけ。セッションのトークン（ログイン・閲覧公開の
/// grant）・`bh_` のキー・壊れたキー・bearer 無しは 401（記録しない）。失効した
/// キーは 401 で、`denied`（`api_keys`・`mcp`）が残る。
#[tokio::test]
async fn sessions_foreign_broken_and_revoked_keys_are_401() {
    let fx = fixture().await;

    // 閲覧公開を有効にして viewer の grant を取る。
    let settings = SettingsService::new(Db::Sqlite(fx.pool.clone()));
    let config = settings.server_config().await.unwrap();
    settings
        .set_server_config(&ServerSettings {
            viewer_public: true,
            ..config
        })
        .await
        .unwrap();
    let response = fx
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/grant/publicViewer")
                .header("X-Banto-Client", "banto")
                .header("Content-Type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let viewer_grant = serde_json::from_slice::<Value>(&bytes).unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();

    let (good, _) = fx.issue("agent", &["config"]).await;
    let wrong_secret = format!("{}x", &good[..good.len() - 1]);
    let ping = json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }).to_string();
    let before = fx.audit_rows().await.len();
    for bearer in [
        Some(fx.admin.as_str()),
        Some(viewer_grant.as_str()),
        Some("bh_abcdefgh_0123456789abcdefghijklmnopqrstuvwxyzABCDEFG"),
        Some("cg_broken"),
        Some(wrong_secret.as_str()),
        None,
    ] {
        let (status, _) = fx.post_raw(bearer, &ping).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{bearer:?}");
    }
    assert_eq!(fx.audit_rows().await.len(), before, "nothing is audited");

    let (revoked, _) = fx.issue("old", &["config"]).await;
    let revoked_id = fx
        .keys
        .list()
        .await
        .unwrap()
        .into_iter()
        .find(|k| k.name == "old")
        .unwrap()
        .id;
    fx.keys.revoke(revoked_id).await.unwrap();
    let (status, _) = fx.post_raw(Some(&revoked), &ping).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let rows = fx.audit_rows().await;
    let entry = rows
        .iter()
        .find(|r| r.action == "denied" && r.resource == "api_keys")
        .unwrap_or_else(|| panic!("no denied: {rows:?}"));
    assert_eq!(entry.origin, "mcp");
    let detail = detail_of(entry);
    assert_eq!(detail["reason"], "revoked");
    assert_eq!(detail["path"], "/mcp");

    // 良いキーは CSRF のヘッダー無しで通る。
    let (status, body) = fx.post_raw(Some(&good), &ping).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["result"], json!({}));
}

/// 共有 crate の受け付けの確かめ: `initialize`（`serverInfo` は `chronogazer`）・
/// `ping`・通知（202）・未知のツール・未知のメソッド・不正な JSON。
#[tokio::test]
async fn the_json_rpc_envelope_comes_from_the_shared_crate() {
    let fx = fixture().await;
    let (key, _) = fx.issue("agent", &["read"]).await;

    let (status, body) = fx
        .rpc(
            &key,
            "initialize",
            json!({ "protocolVersion": "2025-03-26" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(body["result"]["serverInfo"]["name"], "chronogazer");
    assert_eq!(
        body["result"]["serverInfo"]["version"],
        env!("CARGO_PKG_VERSION")
    );

    let (status, body) = fx.rpc(&key, "ping", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"], json!({}));

    let (status, _) = fx
        .post_raw(
            Some(&key),
            &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }).to_string(),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let (status, body) = fx
        .rpc(&key, "tools/call", json!({ "name": "no_such_tool" }))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["error"]["code"].is_i64(), "{body}");

    let (status, body) = fx.rpc(&key, "resources/list", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["error"]["code"], -32601);

    let (status, body) = fx.post_raw(Some(&key), "{not json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["error"]["code"], -32700);
}

/// `tools/list` の `result` 全体（ツールの名前・順・説明・inputSchema）を
/// `tests/snapshots/mcp_tools_list.json` と完全一致で比べる。再生成は
/// `UPDATE_MCP_TOOLS_SNAPSHOT=1 cargo test -p chronogazer-core mcp_tools_list_snapshot`。
#[tokio::test]
async fn mcp_tools_list_snapshot_matches() {
    let fx = fixture().await;
    let (key, _) = fx.issue("reader", &["read"]).await;
    let (status, body) = fx.rpc(&key, "tools/list", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let actual = body["result"].clone();
    assert!(actual["tools"].is_array(), "{body}");

    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots")
        .join("mcp_tools_list.json");
    if matches!(
        std::env::var("UPDATE_MCP_TOOLS_SNAPSHOT").as_deref(),
        Ok("1")
    ) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut text = serde_json::to_string_pretty(&actual).unwrap();
        text.push('\n');
        std::fs::write(&path, text).unwrap();
        return;
    }
    let expected: Value = serde_json::from_str(
        &std::fs::read_to_string(&path)
            .expect("snapshot missing; run with UPDATE_MCP_TOOLS_SNAPSHOT=1"),
    )
    .expect("snapshot parse");
    assert!(
        actual == expected,
        "tools/list differs from tests/snapshots/mcp_tools_list.json; review the diff and regenerate with UPDATE_MCP_TOOLS_SNAPSHOT=1"
    );
}
