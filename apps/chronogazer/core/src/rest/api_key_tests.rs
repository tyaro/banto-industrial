//! #1161 PR4: API キーで `/api/*` を使うときの受け付け・権限・監査のテスト
//! （docs/chronogazer/chronogazer-agent-api-design.md §8 の ChronoGazer の REST）。
//!
//! どれも**本物の `api_router`**（`crate::rest::tests` のヘルパーが組む物。CSRF の層・
//! 子ルーターごとの `require_auth`・`RoleGuard` をすべて含む）に要求を通す。ハンドラ
//! だけを呼ぶテストでは、キーがセッションの確認と CSRF の確認を越えるか、キーを
//! 通さない集まりで止まるかを確かめられないため（設計書のオーナーレビュー）。

use std::path::PathBuf;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use banto_core::ListParams;
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::api_keys::ApiKeysService;
use crate::audit::AuditLogService;
use crate::rest::tests::router_with_role_tokens_audit_pool_data_dir_and_spool;

const CSRF: (&str, &str) = ("X-Banto-Client", "banto");

struct Fixture {
    router: Router,
    audit: AuditLogService,
    keys: ApiKeysService,
    admin: String,
    editor: String,
    viewer: String,
}

async fn fixture() -> Fixture {
    let (router, audit, pool, admin, editor, viewer) =
        router_with_role_tokens_audit_pool_data_dir_and_spool(
            PathBuf::from("unused-in-tests"),
            None,
        )
        .await;
    Fixture {
        router,
        audit,
        keys: ApiKeysService::new(pool),
        admin,
        editor,
        viewer,
    }
}

impl Fixture {
    /// キーを発行して `(平文, 監査の主体の表記)` を返す。
    async fn issue(
        &self,
        name: &str,
        scopes: &[&str],
        expires_at: Option<i64>,
    ) -> (String, String) {
        let issued = self
            .keys
            .issue(
                name,
                scopes.iter().map(|s| s.to_string()).collect(),
                expires_at,
            )
            .await
            .expect("issue");
        let label = format!("{}（{}）", issued.name, issued.prefix);
        (issued.key, label)
    }

    async fn send(&self, req: Request<Body>) -> (StatusCode, Value) {
        let response = self.router.clone().oneshot(req).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, body)
    }

    async fn audit_rows(&self) -> Vec<crate::audit::AuditLogEntry> {
        self.audit.list(ListParams::default()).await.unwrap().rows
    }
}

/// 監査の `detail`（JSON の文字列）を読む。無ければ `null`。
fn detail_of(entry: &crate::audit::AuditLogEntry) -> Value {
    entry
        .detail
        .as_deref()
        .and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or(Value::Null)
}

/// CSRF のヘッダーを付けない、キー（bearer）だけの要求。
fn key_req(method: &str, path: &str, key: &str, body: Option<Value>) -> Request<Body> {
    let builder = Request::builder()
        .method(method)
        .uri(path)
        .header("Authorization", format!("Bearer {key}"));
    match body {
        Some(body) => builder
            .header("Content-Type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    }
}

/// セッションの要求（`csrf` で CSRF のヘッダーの有無を選ぶ）。
fn session_req(
    method: &str,
    path: &str,
    token: &str,
    csrf: bool,
    body: Option<Value>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("Authorization", format!("Bearer {token}"));
    if csrf {
        builder = builder.header(CSRF.0, CSRF.1);
    }
    match body {
        Some(body) => builder
            .header("Content-Type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    }
}

fn slmp_connection(name: &str) -> Value {
    json!({
        "name": name, "protocol": "slmp", "enabled": true,
        "settings": {"host": "127.0.0.1", "port": 5000, "cpu_type": "Q"}
    })
}

/// §8: キーで読める（`read` だけのキーで、キーを通す集まりの読み取りがすべて通る）。
/// CSRF のヘッダーは付けない。
#[tokio::test]
async fn a_read_key_reads_config_and_collect_state_without_the_csrf_header() {
    let fx = fixture().await;
    let (key, _) = fx.issue("reader", &["read"], None).await;
    for path in [
        "/api/plc-connections",
        "/api/collection-groups",
        "/api/tags",
        "/api/display-groups",
        "/api/tag-thresholds",
        "/api/drivers",
        "/api/config-exclusions",
        "/api/collect",
        "/api/collect/values",
        "/api/collect/connections",
        "/api/collect/events",
    ] {
        let (status, body) = fx.send(key_req("GET", path, &key, None)).await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
    }
}

/// §8: `read` のキーでは変えられない（403 と、キーの主体での `denied` の監査）。
/// レジストリの書き込み（ハンドラの `require_editor`）と収集の操作（ルーターの床）の
/// 両方を確かめる。
#[tokio::test]
async fn a_read_key_cannot_change_anything_and_the_denial_is_audited() {
    let fx = fixture().await;
    let (key, label) = fx.issue("reader", &["read"], None).await;

    let (status, _) = fx
        .send(key_req(
            "POST",
            "/api/plc-connections",
            &key,
            Some(slmp_connection("x")),
        ))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = fx
        .send(key_req("POST", "/api/collect/stop", &key, None))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = fx
        .send(key_req(
            "PUT",
            "/api/display-groups/order",
            &key,
            Some(json!({ "ids": [] })),
        ))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let rows = fx.audit_rows().await;
    for (resource, path) in [
        ("plc_connections", "/api/plc-connections"),
        ("collect", "/api/collect/stop"),
        ("display_groups", "/api/display-groups/order"),
    ] {
        let entry = rows
            .iter()
            .find(|r| r.resource == resource && r.action == "denied")
            .unwrap_or_else(|| panic!("no denied entry for {resource}: {rows:?}"));
        assert_eq!(entry.actor_username.as_deref(), Some(label.as_str()));
        assert_eq!(entry.actor_role.as_deref(), Some("api_key"));
        assert_eq!(entry.origin, "rest");
        assert_eq!(entry.result, "denied");
        let detail = detail_of(entry);
        assert_eq!(detail["path"], path);
    }
    let (status, body) = fx
        .send(key_req("GET", "/api/plc-connections", &key, None))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!([]), "nothing was created");
}

/// §8: `config` のキーで変えられ、監査が `名前（prefix）`・`api_key`・`rest` になる。
/// 収集の操作（停止）も通る。CSRF のヘッダーは付けない。
#[tokio::test]
async fn a_config_key_changes_config_and_the_audit_names_the_key() {
    let fx = fixture().await;
    let (key, label) = fx.issue("agent", &["config"], None).await;

    let (status, created) = fx
        .send(key_req(
            "POST",
            "/api/plc-connections",
            &key,
            Some(slmp_connection("by-key")),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let id = created["id"].as_i64().unwrap();
    let (status, body) = fx
        .send(key_req("POST", "/api/collect/stop", &key, None))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _) = fx
        .send(key_req(
            "DELETE",
            &format!("/api/plc-connections/{id}"),
            &key,
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let rows = fx.audit_rows().await;
    for (action, resource) in [
        ("create", "plc_connections"),
        ("stop", "collect"),
        ("delete", "plc_connections"),
    ] {
        let entry = rows
            .iter()
            .find(|r| r.action == action && r.resource == resource)
            .unwrap_or_else(|| panic!("no {action}/{resource} entry: {rows:?}"));
        assert_eq!(entry.actor_username.as_deref(), Some(label.as_str()));
        assert_eq!(entry.actor_role.as_deref(), Some("api_key"));
        assert_eq!(entry.origin, "rest");
        assert_eq!(entry.result, "ok");
    }
    let create = rows
        .iter()
        .find(|r| r.action == "create" && r.resource == "plc_connections")
        .unwrap();
    assert_eq!(create.entity_id.as_deref(), Some(id.to_string().as_str()));
}

/// §3.3・オーナーの注意 2: キーでは管理系（キーを通さない集まり）に入れない。
/// `config` のキーでも 403 で、`denied` を記録する。HUB 連携の専用の URL
/// （`/api/hub*`・`/api/tags/from-hub`）もここで止まる。
#[tokio::test]
async fn a_key_cannot_enter_the_routes_that_do_not_take_keys() {
    let fx = fixture().await;
    let (key, label) = fx.issue("agent", &["config"], None).await;
    let cases: &[(&str, &str, Option<Value>)] = &[
        ("GET", "/api/users", None),
        (
            "POST",
            "/api/users",
            Some(json!({"username": "x", "password": "password123", "name": "x", "role": "admin"})),
        ),
        ("GET", "/api/settings/server", None),
        ("GET", "/api/settings/auth", None),
        ("GET", "/api/hub", None),
        ("POST", "/api/hub/connect", Some(json!({}))),
        ("GET", "/api/hub/catalog", None),
        ("POST", "/api/hub/legacy-selection/dismiss", None),
        ("POST", "/api/tags/from-hub", Some(json!({}))),
        ("GET", "/api/backups", None),
        ("POST", "/api/backups", None),
        ("POST", "/api/audit-log/list", Some(json!({}))),
        ("GET", "/api/api-keys", None),
        (
            "POST",
            "/api/api-keys",
            Some(json!({"name": "more", "scopes": ["config"]})),
        ),
        ("GET", "/api/auth/status", None),
        ("GET", "/api/auth/identity", None),
        ("POST", "/api/auth/logout", None),
        ("GET", "/api/ui-settings/theme", None),
        ("GET", "/api/events", None),
    ];
    for (method, path, body) in cases {
        let (status, response) = fx.send(key_req(method, path, &key, body.clone())).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}: {response}");
        let rows = fx.audit_rows().await;
        assert!(
            rows.iter().any(|r| r.action == "denied"
                && r.actor_username.as_deref() == Some(label.as_str())
                && r.actor_role.as_deref() == Some("api_key")
                && detail_of(r)["path"] == *path),
            "{method} {path}: no denied entry: {rows:?}"
        );
    }
    // 1 件もキーは増えていない（`POST /api/api-keys` が通っていない）。
    assert_eq!(fx.keys.list().await.unwrap().len(), 1);
}

/// オーナーの注意 2: 汎用の CRUD が扱う「Hub 由来の登録データ」（`protocol: "hub"`
/// の接続先）はキーで作成・読み取り・更新・削除できる。専用の URL を拒否することと
/// 混ぜない。
#[tokio::test]
async fn a_config_key_handles_hub_origin_rows_through_the_generic_crud() {
    let fx = fixture().await;
    let (key, _) = fx.issue("agent", &["config"], None).await;
    let (status, created) = fx
        .send(key_req(
            "POST",
            "/api/plc-connections",
            &key,
            Some(json!({ "name": "Hub", "protocol": "hub", "enabled": false, "settings": {} })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["protocol"], "hub");
    let id = created["id"].as_i64().unwrap();

    let (status, listed) = fx
        .send(key_req("GET", "/api/plc-connections", &key, None))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(listed
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["id"] == id && row["protocol"] == "hub"));

    let (status, updated) = fx
        .send(key_req(
            "PUT",
            &format!("/api/plc-connections/{id}"),
            &key,
            Some(json!({ "name": "Hub2", "protocol": "hub", "enabled": false, "settings": {} })),
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["name"], "Hub2");

    // 同じキーでも専用の URL は 403。
    let (status, _) = fx
        .send(key_req("POST", "/api/tags/from-hub", &key, Some(json!({}))))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _) = fx
        .send(key_req(
            "DELETE",
            &format!("/api/plc-connections/{id}"),
            &key,
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

/// §8: セッションの要求には今どおり CSRF のヘッダーが要る（キーを通す集まりでも、
/// 通さない集まりでも）。キーの要求には要らない（上の各テスト）。
#[tokio::test]
async fn a_session_request_still_needs_the_csrf_header() {
    let fx = fixture().await;
    for path in ["/api/plc-connections", "/api/collect", "/api/users"] {
        let (status, _) = fx
            .send(session_req("GET", path, &fx.admin, false, None))
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path} without the header");
        let (status, _) = fx
            .send(session_req("GET", path, &fx.admin, true, None))
            .await;
        assert_eq!(status, StatusCode::OK, "{path} with the header");
    }
    // セッションの床と監査は今どおり（viewer は書けず、`denied` はアカウントで残る）。
    let (status, _) = fx
        .send(session_req(
            "POST",
            "/api/plc-connections",
            &fx.viewer,
            true,
            Some(slmp_connection("v")),
        ))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = fx
        .send(session_req(
            "POST",
            "/api/plc-connections",
            &fx.editor,
            true,
            Some(slmp_connection("e")),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    let rows = fx.audit_rows().await;
    assert!(rows.iter().any(|r| r.action == "denied"
        && r.actor_username.as_deref() == Some("viewer")
        && r.actor_role.as_deref() == Some("viewer")));
    assert!(rows.iter().any(|r| r.action == "create"
        && r.resource == "plc_connections"
        && r.actor_username.as_deref() == Some("editor")
        && r.actor_role.as_deref() == Some("editor")
        && r.origin == "rest"));
}

/// §8: 失効と期限切れのキーは 401。どちらも `resource: "api_keys"` の `denied` を
/// 記録する（HUB の `/api/v1/*` と同じ）。
#[tokio::test]
async fn revoked_and_expired_keys_are_401_and_audited() {
    let fx = fixture().await;
    let (revoked, _) = fx.issue("old", &["config"], None).await;
    let revoked_id = fx.keys.list().await.unwrap()[0].id;
    fx.keys.revoke(revoked_id).await.unwrap();
    // 期限はサービスでは確かめない（REST だけが「未来に限る」）ので、過去の期限で作る。
    let (expired, _) = fx.issue("expired", &["config"], Some(1)).await;

    for (key, reason, name) in [
        (&revoked, "revoked", "old"),
        (&expired, "expired", "expired"),
    ] {
        let (status, _) = fx.send(key_req("GET", "/api/tags", key, None)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{reason}");
        let rows = fx.audit_rows().await;
        let entry = rows
            .iter()
            .find(|r| {
                r.action == "denied" && r.resource == "api_keys" && detail_of(r)["reason"] == reason
            })
            .unwrap_or_else(|| panic!("no {reason} entry: {rows:?}"));
        let detail = detail_of(entry);
        assert_eq!(detail["name"], name);
        assert_eq!(detail["path"], "/api/tags");
    }
}

/// §8・§4.1: ほかのアプリのキー（`bh_…`）・壊れた `cg_…`・secret の違う `cg_…` は
/// 401 で、セッションには落ちない（認証の要らない `/api/auth/status` でも 401 に
/// なる = セッションの道に入っていない）。誰か分からないので記録しない。
#[tokio::test]
async fn foreign_and_malformed_keys_are_401_without_falling_back_to_the_session() {
    let fx = fixture().await;
    let (good, _) = fx.issue("agent", &["config"], None).await;
    // prefix は合っていて secret だけが違う。
    let wrong_secret = format!("{}x", &good[..good.len() - 1]);
    let before = fx.audit_rows().await.len();

    // CSRF のヘッダーが無くてもセッションとしては扱われず、401 で止まる。
    let (status, _) = fx
        .send(session_req("GET", "/api/auth/status", "x", true, None))
        .await;
    assert_ne!(
        status,
        StatusCode::UNAUTHORIZED,
        "the status route needs no session"
    );

    for bad in [
        "bh_abcdefgh_0123456789abcdefghijklmnopqrstuvwxyzABCDEFG".to_string(),
        "cg_broken".to_string(),
        "cg_".to_string(),
        wrong_secret,
    ] {
        for (path, csrf) in [("/api/tags", false), ("/api/auth/status", true)] {
            let (status, _) = fx.send(session_req("GET", path, &bad, csrf, None)).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{bad} on {path}");
        }
    }
    assert_eq!(fx.audit_rows().await.len(), before, "nothing is audited");
}

/// §3.4: `/api/api-keys*` はログインした管理者のセッションだけ。editor・viewer は
/// 403、認証なしは 401（キーは上の `a_key_cannot_enter_the_routes_that_do_not_take_keys`）。
#[tokio::test]
async fn the_api_keys_routes_are_for_admin_sessions_only() {
    let fx = fixture().await;
    for token in [&fx.editor, &fx.viewer] {
        let (status, _) = fx
            .send(session_req("GET", "/api/api-keys", token, true, None))
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = fx
            .send(session_req(
                "POST",
                "/api/api-keys",
                token,
                true,
                Some(json!({ "name": "x", "scopes": ["read"] })),
            ))
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    let anonymous = Request::builder()
        .method("GET")
        .uri("/api/api-keys")
        .header(CSRF.0, CSRF.1)
        .body(Body::empty())
        .unwrap();
    let (status, _) = fx.send(anonymous).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(fx.keys.list().await.unwrap().is_empty());
}

/// §3.4: 発行・一覧・失効・削除の可否と監査。スコープは `read`・`config` だけ、
/// 期限は未来に限る、有効なキーは消せない（409）、失効したキーは消せる。
#[tokio::test]
async fn an_admin_issues_lists_revokes_and_deletes_keys_and_each_is_audited() {
    let fx = fixture().await;
    let post = |body: Value| session_req("POST", "/api/api-keys", &fx.admin, true, Some(body));

    // スコープの誤り・期限の誤り・空のスコープは発行しない。
    for body in [
        json!({ "name": "bad", "scopes": ["admin"] }),
        json!({ "name": "bad", "scopes": ["write"] }),
        json!({ "name": "bad", "scopes": [] }),
        json!({ "name": "bad", "scopes": ["read"], "expiresAt": 1 }),
    ] {
        let (status, response) = fx.send(post(body.clone())).await;
        assert!(status.is_client_error(), "{body}: {status} {response}");
    }
    assert!(fx.keys.list().await.unwrap().is_empty());

    let future = 4_102_444_800_000_i64; // 2100-01-01
    let (status, issued) = fx
        .send(post(
            json!({ "name": "agent", "scopes": ["read", "config"], "expiresAt": future }),
        ))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{issued}");
    let key = issued["key"].as_str().unwrap().to_string();
    assert!(key.starts_with("cg_"), "{key}");
    let id = issued["id"].as_i64().unwrap();
    let prefix = issued["prefix"].as_str().unwrap().to_string();

    // 発行したキーがそのまま使える。
    let (status, _) = fx.send(key_req("GET", "/api/tags", &key, None)).await;
    assert_eq!(status, StatusCode::OK);

    let (status, listed) = fx
        .send(session_req("GET", "/api/api-keys", &fx.admin, true, None))
        .await;
    assert_eq!(status, StatusCode::OK);
    let row = &listed[0];
    assert_eq!(row["name"], "agent");
    assert_eq!(row["expiresAt"], future);
    assert!(
        row.get("key").is_none() && row.get("keyHash").is_none() && row.get("key_hash").is_none()
    );

    // 有効なキーは消せない。
    let (status, _) = fx
        .send(session_req(
            "DELETE",
            &format!("/api/api-keys/{id}"),
            &fx.admin,
            true,
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);

    let (status, revoked) = fx
        .send(session_req(
            "POST",
            &format!("/api/api-keys/{id}/revoke"),
            &fx.admin,
            true,
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{revoked}");
    assert!(revoked["revokedAt"].is_string());
    let (status, _) = fx.send(key_req("GET", "/api/tags", &key, None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _) = fx
        .send(session_req(
            "DELETE",
            &format!("/api/api-keys/{id}"),
            &fx.admin,
            true,
            None,
        ))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(fx.keys.list().await.unwrap().is_empty());

    let rows = fx.audit_rows().await;
    for action in ["create", "revoke", "delete"] {
        let entry = rows
            .iter()
            .find(|r| r.resource == "api_keys" && r.action == action)
            .unwrap_or_else(|| panic!("no {action} entry: {rows:?}"));
        assert_eq!(entry.actor_username.as_deref(), Some("testadmin"));
        assert_eq!(entry.actor_role.as_deref(), Some("admin"));
        assert_eq!(entry.origin, "rest");
        assert_eq!(entry.entity_id.as_deref(), Some(id.to_string().as_str()));
        let detail = detail_of(entry);
        assert_eq!(detail["prefix"], prefix);
        // 平文のキーは監査に載せない。
        assert!(!detail.to_string().contains(&key));
    }
}

/// #1179 PR5: 表示グループの保存でペンの Hub のタグを登録できる（汎用の
/// `POST /api/tags` で Hub 接続のグループにタグを作れるのと同じ扱い）。`config` の
/// キーでは通り、記録項目の `create` もキーの主体で監査される。`read` のキーでは
/// 403（`denied`）で、何も登録しない。
#[tokio::test]
async fn a_config_key_saves_a_display_group_with_hub_tags_and_a_read_key_cannot() {
    let fx = fixture().await;
    let body = json!({
        "name": "L1", "kind": "trend",
        "pens": [{ "hubTag": { "externalName": "L.G.a", "name": "a", "dataType": "f32" } }],
        "hubTagGroup": { "newGroup": { "name": "hub-g", "periodMs": 1000 } },
    });

    let (reader, reader_label) = fx.issue("reader", &["read"], None).await;
    let (status, _) = fx
        .send(key_req(
            "POST",
            "/api/display-groups",
            &reader,
            Some(body.clone()),
        ))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let rows = fx.audit_rows().await;
    let denied = rows
        .iter()
        .find(|r| r.resource == "display_groups" && r.action == "denied")
        .unwrap_or_else(|| panic!("no denied entry: {rows:?}"));
    assert_eq!(
        denied.actor_username.as_deref(),
        Some(reader_label.as_str())
    );
    let (status, tags) = fx.send(key_req("GET", "/api/tags", &reader, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(tags, json!([]), "nothing was registered");

    let (key, label) = fx.issue("agent", &["config"], None).await;
    let (status, created) = fx
        .send(key_req("POST", "/api/display-groups", &key, Some(body)))
        .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["registeredTags"][0]["address"], "L.G.a");
    assert_eq!(
        created["pens"][0]["tagId"],
        created["registeredTags"][0]["id"]
    );
    let rows = fx.audit_rows().await;
    for resource in ["tags", "display_groups"] {
        let entry = rows
            .iter()
            .find(|r| r.resource == resource && r.action == "create")
            .unwrap_or_else(|| panic!("no create entry for {resource}: {rows:?}"));
        assert_eq!(entry.actor_username.as_deref(), Some(label.as_str()));
        assert_eq!(entry.actor_role.as_deref(), Some("api_key"));
        assert_eq!(entry.origin, "rest");
    }
}
