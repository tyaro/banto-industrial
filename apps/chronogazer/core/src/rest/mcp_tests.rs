//! #1161 PR5b: `POST /mcp` のテスト（docs/chronogazer/chronogazer-agent-api-design.md §8 の
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
        self.issue_with_expiry(name, scopes, None).await
    }

    /// 期限（エポックミリ秒）つきで発行する。サービスは期限を確かめない
    /// （「未来に限る」のは REST だけ）ので、過去の期限で期限切れのキーも作れる。
    async fn issue_with_expiry(
        &self,
        name: &str,
        scopes: &[&str],
        expires_at_ms: Option<i64>,
    ) -> (String, String) {
        let issued = self
            .keys
            .issue(
                name,
                scopes.iter().map(|s| s.to_string()).collect(),
                expires_at_ms,
            )
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

/// 更新のツールが求める全項目つきの接続先（`simulation` も書く）。
fn slmp_connection_full(name: &str) -> Value {
    let mut value = slmp_connection(name);
    value["simulation"] = json!(false);
    value
}

/// 収集が読む設定を変えるツール（結果に `restartRequired: true` が付く物）。
/// 表示グループは収集が読まないので付けない。
const CHANGING_TOOLS: [&str; 10] = [
    "create_connection",
    "update_connection",
    "delete_connection",
    "create_group",
    "update_group",
    "delete_group",
    "create_tag",
    "update_tag",
    "delete_tag",
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
    let mut update = slmp_connection_full("c1-renamed");
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
        json!({
            "id": group_id, "name": "g1b", "plcConnectionId": conn_id, "periodMs": 2000,
            "enabled": true
        })
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
            "dataType": "i16", "unit": "℃", "expectedRevision": revision,
            "rawLo": null, "rawHi": null, "engLo": null, "engHi": null,
            "decimals": 1, "enabled": true
        })
    );
    let got = ok!("get_tag", json!({ "id": tag_id }));
    assert_eq!(got["tag"]["unit"], "℃");
    ok!("list_tags", json!({}));

    // しきい値。
    let set = ok!(
        "set_threshold",
        json!({
            "tagId": tag_id, "thresholdLl": null, "thresholdL": null,
            "thresholdH": 80.0, "thresholdHh": 90.0
        })
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
        json!({
            "id": dg_id, "name": "L1b", "kind": "digital", "attributes": {}, "pens": []
        })
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

    // restartRequired は収集が読む設定を変えるツールだけ（表示グループには付けない）。
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

/// 更新のツールは入力の全項目が必須。欠けた更新はフィールドのエラー（`isError`・
/// `missing_fields`）になり、何も変わらず監査も残らない。`expectedRevision` と
/// 作成のツールは省略できる。
#[tokio::test]
async fn updates_require_every_field_and_a_partial_update_changes_nothing() {
    let fx = fixture().await;
    let (key, _) = fx.issue("agent", &["config"]).await;
    let conn = fx
        .call_ok(&key, "create_connection", slmp_connection("c1"))
        .await;
    let conn_id = conn["created"]["id"].as_i64().unwrap();
    let group = fx
        .call_ok(
            &key,
            "create_group",
            json!({ "name": "g1", "plcConnectionId": conn_id, "periodMs": 1000 }),
        )
        .await;
    let group_id = group["created"]["id"].as_i64().unwrap();
    let tag = fx
        .call_ok(
            &key,
            "create_tag",
            json!({ "name": "t1", "collectionGroupId": group_id, "address": "D100",
                    "dataType": "i16", "unit": "℃", "decimals": 2 }),
        )
        .await;
    let tag_id = tag["created"]["id"].as_i64().unwrap();
    let dg = fx
        .call_ok(
            &key,
            "create_display_group",
            json!({ "name": "L1", "kind": "trend", "pens": [{ "tagId": tag_id }] }),
        )
        .await;
    let dg_id = dg["created"]["id"].as_i64().unwrap();
    fx.call_ok(
        &key,
        "set_threshold",
        json!({ "tagId": tag_id, "thresholdLl": null, "thresholdL": null,
                "thresholdH": 80.0, "thresholdHh": null }),
    )
    .await;
    let before = fx.audit_rows().await.len();

    // 各ツールで、全項目から 1 つずつ外した入力がすべて断られる。
    let full: [(&str, Value, &[&str]); 5] = [
        (
            "update_connection",
            json!({ "id": conn_id, "name": "c1", "protocol": "slmp", "enabled": true,
                    "simulation": false, "settings": { "host": "127.0.0.1", "port": 5000, "cpu_type": "Q" } }),
            &[
                "id",
                "name",
                "protocol",
                "enabled",
                "simulation",
                "settings",
            ],
        ),
        (
            "update_group",
            json!({ "id": group_id, "name": "g1", "plcConnectionId": conn_id, "periodMs": 1000,
                    "enabled": true }),
            &["id", "name", "plcConnectionId", "periodMs", "enabled"],
        ),
        (
            "update_tag",
            json!({ "id": tag_id, "name": "t1", "collectionGroupId": group_id, "address": "D100",
                    "dataType": "i16", "rawLo": null, "rawHi": null, "engLo": null, "engHi": null,
                    "unit": null, "decimals": 0, "enabled": true }),
            &[
                "id",
                "name",
                "collectionGroupId",
                "address",
                "dataType",
                "rawLo",
                "rawHi",
                "engLo",
                "engHi",
                "unit",
                "decimals",
                "enabled",
            ],
        ),
        (
            "update_display_group",
            json!({ "id": dg_id, "name": "L1", "kind": "trend", "attributes": {}, "pens": [] }),
            &["id", "name", "kind", "attributes", "pens"],
        ),
        (
            "set_threshold",
            json!({ "tagId": tag_id, "thresholdLl": null, "thresholdL": null,
                    "thresholdH": null, "thresholdHh": null }),
            &[
                "tagId",
                "thresholdLl",
                "thresholdL",
                "thresholdH",
                "thresholdHh",
            ],
        ),
    ];
    for (tool, args, fields) in &full {
        for field in *fields {
            let mut partial = args.clone();
            partial.as_object_mut().unwrap().remove(*field);
            let result = fx.call(&key, tool, partial).await;
            assert_eq!(result["isError"], true, "{tool} without {field}: {result}");
            let text = result_text(&result);
            assert!(text.starts_with("missing_fields"), "{tool}: {text}");
            assert!(text.contains(field), "{tool}: {text}");
        }
    }
    assert_eq!(fx.audit_rows().await.len(), before, "nothing is audited");

    // 何も変わっていない。
    let got = fx.call_ok(&key, "get_tag", json!({ "id": tag_id })).await;
    assert_eq!(got["tag"]["name"], "t1");
    assert_eq!(got["tag"]["unit"], "℃");
    assert_eq!(got["tag"]["decimals"], 2);
    let got = fx
        .call_ok(&key, "get_threshold", json!({ "tagId": tag_id }))
        .await;
    assert_eq!(got["threshold"]["thresholdH"], 80.0);
    let got = fx
        .call_ok(&key, "get_display_group", json!({ "id": dg_id }))
        .await;
    assert_eq!(got["displayGroup"]["pens"].as_array().unwrap().len(), 1);

    // 全項目があれば通る（`expectedRevision` は無くてよい）。
    for (tool, args, _) in &full {
        fx.call_ok(&key, tool, args.clone()).await;
    }
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

/// §8: 期限切れは 401、一時停止（tripped）は 403 `key_tripped`。どちらも
/// `resource: "api_keys"` の `denied` を `origin: "mcp"` で残す（REST と同じ照合）。
#[tokio::test]
async fn expired_and_tripped_keys_are_refused_and_audited_as_mcp() {
    let fx = fixture().await;
    let (expired, _) = fx.issue_with_expiry("expired", &["config"], Some(1)).await;
    let (tripped, _) = fx.issue("tripped", &["config"]).await;
    let tripped_id = fx
        .keys
        .list()
        .await
        .unwrap()
        .into_iter()
        .find(|k| k.name == "tripped")
        .unwrap()
        .id;
    fx.keys.trip(tripped_id).await.unwrap();

    let (status, _) = fx.rpc(&expired, "ping", json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, body) = fx.rpc(&tripped, "ping", json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body, json!({ "error": "key_tripped" }));

    let rows = fx.audit_rows().await;
    for (reason, name) in [("expired", "expired"), ("tripped", "tripped")] {
        let entry = rows
            .iter()
            .find(|r| {
                r.action == "denied" && r.resource == "api_keys" && detail_of(r)["reason"] == reason
            })
            .unwrap_or_else(|| panic!("no {reason} entry: {rows:?}"));
        assert_eq!(entry.origin, "mcp", "{reason}");
        assert_eq!(entry.result, "denied", "{reason}");
        let detail = detail_of(entry);
        assert_eq!(detail["name"], name);
        assert_eq!(detail["path"], "/mcp");
    }
    // ツールも走らない（一時停止のキーで tools/call しても 403）。
    let (status, body) = fx
        .rpc(
            &tripped,
            "tools/call",
            json!({ "name": "list_connections", "arguments": {} }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

/// §8: キーを照合できない（DB の障害）ときは 500 で、JSON-RPC の応答もツールも
/// 無く、`api_keys` の `denied` も残さない（誰か分からない）。直れば同じキーで通る。
#[tokio::test]
async fn an_unverifiable_key_is_500_without_running_tools_or_auditing() {
    let fx = fixture().await;
    let (key, _) = fx.issue("agent", &["config"]).await;
    let (status, _) = fx.rpc(&key, "ping", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let before = fx.audit_rows().await.len();

    sqlx::query("ALTER TABLE api_keys RENAME TO api_keys_gone")
        .execute(&fx.pool)
        .await
        .unwrap();
    let (status, body) = fx
        .rpc(
            &key,
            "tools/call",
            json!({ "name": "create_connection", "arguments": slmp_connection("x") }),
        )
        .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert!(
        body.get("result").is_none() && body.get("jsonrpc").is_none(),
        "{body}"
    );
    // 監査は読め、api_keys の行は増えず、ツールも走っていない。
    let rows = fx.audit_rows().await;
    assert_eq!(rows.len(), before, "{rows:?}");
    assert!(rows.iter().all(|r| r.resource != "api_keys"), "{rows:?}");

    sqlx::query("ALTER TABLE api_keys_gone RENAME TO api_keys")
        .execute(&fx.pool)
        .await
        .unwrap();
    let listed = fx.call_ok(&key, "list_connections", json!({})).await;
    assert_eq!(listed["connections"], json!([]));
}

/// 設定を変えるツール（読み取りの 13 個以外のすべて）に**正しい入力**を作る。
/// ID は準備で作った実在の物。ここに無いツールがあれば、呼ぶ側のテストが落ちる
/// （ツールを足したときにスコープの確かめから漏れないように）。
struct ValidArgs {
    conn_id: i64,
    group_id: i64,
    tag_id: i64,
    revision: i64,
    dg_id: i64,
}

impl ValidArgs {
    fn for_tool(&self, tool: &str) -> Option<Value> {
        let mut update_connection = slmp_connection_full("renamed");
        update_connection["id"] = json!(self.conn_id);
        Some(match tool {
            "start_collection" | "stop_collection" | "restart_collection" => json!({}),
            "create_connection" => slmp_connection("new-conn"),
            "update_connection" => update_connection,
            "delete_connection" => json!({ "id": self.conn_id, "confirm": true }),
            "create_group" => {
                json!({ "name": "new-g", "plcConnectionId": self.conn_id, "periodMs": 1000 })
            }
            "update_group" => json!({
                "id": self.group_id, "name": "g-renamed", "plcConnectionId": self.conn_id,
                "periodMs": 2000, "enabled": true
            }),
            "delete_group" => json!({ "id": self.group_id, "confirm": true }),
            "create_tag" => json!({
                "name": "new-t", "collectionGroupId": self.group_id, "address": "D200",
                "dataType": "i16"
            }),
            "update_tag" => json!({
                "id": self.tag_id, "name": "t-renamed", "collectionGroupId": self.group_id,
                "address": "D100", "dataType": "i16", "unit": "℃",
                "expectedRevision": self.revision, "rawLo": null, "rawHi": null,
                "engLo": null, "engHi": null, "decimals": 1, "enabled": true
            }),
            "delete_tag" => json!({ "id": self.tag_id, "confirm": true }),
            "set_threshold" => json!({
                "tagId": self.tag_id, "thresholdLl": null, "thresholdL": null,
                "thresholdH": 80.0, "thresholdHh": 90.0
            }),
            "create_display_group" => json!({ "name": "new-dg", "kind": "gauge" }),
            "update_display_group" => json!({
                "id": self.dg_id, "name": "dg-renamed", "kind": "digital",
                "attributes": {}, "pens": []
            }),
            "reorder_display_groups" => json!({ "ids": [self.dg_id] }),
            "delete_display_group" => json!({ "id": self.dg_id, "confirm": true }),
            _ => return None,
        })
    }
}

/// 読み取りのツール（スコープ `read` で呼べる物）。
const READ_TOOLS: [&str; 13] = [
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
];

/// 設定と収集の状態の写し（変わっていないことの比較用）。
async fn config_snapshot(fx: &Fixture, key: &str) -> Value {
    let mut out = serde_json::Map::new();
    for tool in [
        "list_connections",
        "list_groups",
        "list_tags",
        "list_thresholds",
        "list_display_groups",
        "get_status",
    ] {
        out.insert(tool.to_string(), fx.call_ok(key, tool, json!({})).await);
    }
    Value::Object(out)
}

/// §8: 設定を変えるツールは、正しい入力でも `read` のキーとスコープの無いキーの
/// どちらでも `forbidden`（`isError`）で断られ、`denied`（`{tool}`・`mcp`）が残り、
/// 設定は変わらない。入力の形の誤りで止まらないので、ops の中のスコープの確かめを
/// 全ツールで直接確かめる。
#[tokio::test]
async fn every_changing_tool_is_refused_with_valid_input_for_read_and_scopeless_keys() {
    let fx = fixture().await;
    let (config_key, _) = fx.issue("setup", &["config"]).await;
    let (read_key, read_label) = fx.issue("reader", &["read"]).await;
    let (none_key, none_label) = fx.issue("nothing", &["read"]).await;
    sqlx::query("UPDATE api_keys SET scopes = '[]' WHERE name = 'nothing'")
        .execute(&fx.pool)
        .await
        .unwrap();

    // 準備（config のキーで、対象になる物を作っておく）。
    let conn = fx
        .call_ok(&config_key, "create_connection", slmp_connection("c1"))
        .await;
    let conn_id = conn["created"]["id"].as_i64().unwrap();
    let group = fx
        .call_ok(
            &config_key,
            "create_group",
            json!({ "name": "g1", "plcConnectionId": conn_id, "periodMs": 1000 }),
        )
        .await;
    let group_id = group["created"]["id"].as_i64().unwrap();
    let tag = fx
        .call_ok(
            &config_key,
            "create_tag",
            json!({
                "name": "t1", "collectionGroupId": group_id, "address": "D100",
                "dataType": "i16"
            }),
        )
        .await;
    let tag_id = tag["created"]["id"].as_i64().unwrap();
    let revision = tag["created"]["revision"].as_i64().unwrap();
    let dg = fx
        .call_ok(
            &config_key,
            "create_display_group",
            json!({ "name": "L1", "kind": "trend" }),
        )
        .await;
    let valid = ValidArgs {
        conn_id,
        group_id,
        tag_id,
        revision,
        dg_id: dg["created"]["id"].as_i64().unwrap(),
    };

    let changing: Vec<String> = fx
        .tool_names(&config_key)
        .await
        .into_iter()
        .filter(|t| !READ_TOOLS.contains(&t.as_str()))
        .collect();
    assert!(changing.len() >= 15, "{changing:?}");
    let before = config_snapshot(&fx, &config_key).await;
    let denied_before = fx
        .audit_rows()
        .await
        .iter()
        .filter(|r| r.action == "denied")
        .count();

    for tool in &changing {
        let args = valid
            .for_tool(tool)
            .unwrap_or_else(|| panic!("ValidArgs has no input for {tool}: add it"));
        for key in [&read_key, &none_key] {
            let result = fx.call(key, tool, args.clone()).await;
            assert_eq!(result["isError"], true, "{tool}: {result}");
            assert!(
                result_text(&result).starts_with("forbidden"),
                "{tool}: {result}"
            );
        }
        let rows = fx.audit_rows().await;
        for label in [&read_label, &none_label] {
            let entry = rows
                .iter()
                .find(|r| {
                    r.action == "denied"
                        && r.actor_username.as_deref() == Some(label.as_str())
                        && detail_of(r)["tool"] == tool.as_str()
                })
                .unwrap_or_else(|| panic!("no denied for {tool} by {label}"));
            assert_eq!(entry.origin, "mcp");
            assert_eq!(detail_of(entry), json!({ "tool": tool }));
        }
    }
    let denied_after = fx
        .audit_rows()
        .await
        .iter()
        .filter(|r| r.action == "denied")
        .count();
    assert_eq!(denied_after - denied_before, changing.len() * 2);
    assert_eq!(config_snapshot(&fx, &config_key).await, before);
}

/// #1179 PR5: `create_display_group` でペンの Hub のタグを登録したときだけ
/// `restartRequired: true`（記録項目を変えたツールと同じ扱い、§5.3）。登録の無い
/// 作成・更新には付かない。監査は記録項目の `create` → 表示グループの `create`
/// （どちらも `mcp`）。
#[tokio::test]
async fn a_config_key_registers_hub_tags_through_create_display_group() {
    let fx = fixture().await;
    let (key, _) = fx.issue("agent", &["config"]).await;
    let hub_pen = json!({ "hubTag": { "externalName": "L.G.a", "name": "a", "dataType": "f32" } });

    let created = fx
        .call_ok(
            &key,
            "create_display_group",
            json!({
                "name": "L1", "kind": "trend", "pens": [hub_pen.clone()],
                "hubTagGroup": { "newGroup": { "name": "hub-g", "periodMs": 1000 } },
            }),
        )
        .await;
    assert_eq!(created["restartRequired"], true, "{created}");
    let registered = &created["created"]["registeredTags"];
    assert_eq!(registered[0]["address"], "L.G.a", "{created}");
    assert_eq!(created["created"]["pens"][0]["tagId"], registered[0]["id"]);

    // 登録済みの外部名は参照だけ: 登録が無いので付かない。
    let again = fx
        .call_ok(
            &key,
            "create_display_group",
            json!({ "name": "L2", "kind": "bar", "pens": [hub_pen] }),
        )
        .await;
    assert!(again.get("restartRequired").is_none(), "{again}");
    assert!(again["created"].get("registeredTags").is_none(), "{again}");
    assert_eq!(
        again["created"]["pens"][0]["tagId"],
        created["created"]["pens"][0]["tagId"]
    );

    let mut rows = fx.audit_rows().await;
    rows.sort_by_key(|row| row.id);
    let got: Vec<(String, String, String)> = rows
        .into_iter()
        .filter(|row| row.result == "ok")
        .filter(|row| row.resource == "tags" || row.resource == "display_groups")
        .map(|row| (row.resource, row.action, row.origin))
        .collect();
    assert_eq!(
        got,
        [
            ("tags", "create"),
            ("display_groups", "create"),
            ("display_groups", "create"),
        ]
        .map(|(r, a)| (r.to_string(), a.to_string(), "mcp".to_string()))
    );
}

/// #1179 PR5: `read` のキーでは Hub のタグ付きの作成も断り（`denied`）、何も登録しない。
#[tokio::test]
async fn a_read_key_cannot_register_hub_tags_through_create_display_group() {
    let fx = fixture().await;
    let (key, _) = fx.issue("reader", &["read"]).await;
    let result = fx
        .call(
            &key,
            "create_display_group",
            json!({
                "name": "L1", "kind": "trend",
                "pens": [{ "hubTag": { "externalName": "L.G.a", "name": "a", "dataType": "f32" } }],
                "hubTagGroup": { "newGroup": { "name": "hub-g", "periodMs": 1000 } },
            }),
        )
        .await;
    assert_eq!(result["isError"], true, "{result}");
    assert!(result_text(&result).starts_with("forbidden"), "{result}");
    let rows = fx.audit_rows().await;
    assert!(rows.iter().any(|r| r.action == "denied"
        && r.resource == "display_groups"
        && detail_of(r)["tool"] == "create_display_group"));
    let tags: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tags")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(tags, 0, "nothing was registered");
}
