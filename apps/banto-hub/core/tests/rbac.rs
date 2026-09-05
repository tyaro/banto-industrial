//! issue #231「role による書き込み拒否テストのカバレッジ欠落」の穴埋め。
//!
//! `apps/banto-hub/core/src/rest.rs`の書き込み系 REST エンドポイントに掛かる
//! 認可(`require_editor`ゲート・`RoleGuard`)を role 別の拒否/許可として
//! **表駆動で REST テストに固定する**(E2E は使わない、機械的なカバレッジ
//! 埋め)。`tests/t12_connection_test.rs`と同じ理由(各`tests/*.rs`は独立した
//! クレートとしてコンパイルされ、private helper を共有できない)で
//! `TestApp`/`test_app`/`admin_write`相当をこのファイル内に複製している
//! (t12 のものをベースにした)。
//!
//! テスト構成:
//! 1. `require_editor`ゲート(rest.rs 全13箇所)- viewer は全対象で 403
//! 2. editor は`require_editor`ゲートを通って実際に書ける(admin も同様)
//! 3. admin 限定ルート(`RoleGuard{min: Role::Admin}`)- editor/viewer は
//!    403、admin は通過(403 以外)

use axum::body::Body;
use axum::http::{Request as HttpRequest, StatusCode};
use axum::Router;
use banto_collect::{BackoffConfig, CollectorOptions};
use banto_hub_core::api_keys::ApiKeysService;
use banto_hub_core::audit::AuditLogService;
use banto_hub_core::broker_glue::{HubSessions, SlmpSimRegistry};
use banto_hub_core::commissioning::CommissioningService;
use banto_hub_core::computed::{ComputedEngine, ServerTagStore};
use banto_hub_core::db::init_db;
use banto_hub_core::grpc::{GrpcServer, GrpcService};
use banto_hub_core::hub::CollectorManager;
use banto_hub_core::rest::api_router;
use banto_hub_core::settings::SettingsService;
use banto_hub_core::users::{Role, UsersService};
use banto_hub_core::write_audit::WriteAuditService;
use banto_hub_core::write_control::WriteControl;
use banto_hub_core::write_rate::{WriteRateLimitConfig, WriteRateLimiter};
use banto_server::{AuthState, Identity};
use banto_tags::{CollectionGroupService, PlcConnectionService, TagService};
use serde_json::{json, Value};
use sqlx::SqlitePool;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tower::ServiceExt;

mod common;
use common::TempEnv;

const CLIENT_HEADER: (&str, &str) = ("X-Banto-Client", "banto");

/// Temp-dir prefix passed to `TempEnv::new` (see `tests/common/mod.rs`).
const TEMP_ENV_PREFIX: &str = "banto-hub-rbac-it";

fn fast_options() -> CollectorOptions {
    CollectorOptions {
        backoff: BackoffConfig {
            base: Duration::from_millis(20),
            max: Duration::from_millis(100),
        },
        connect_timeout: Duration::from_millis(500),
        response_timeout: Duration::from_millis(500),
        writer_options: banto_tstore::WriterOptions {
            max_buffered_rows: 1,
            flush_interval_ms: 0,
        },
    }
}

struct TestApp {
    router: Router,
    admin_token: String,
    /// Clone of the `AuthState` the router was built with (before it was
    /// moved into `api_router`) - `AuthState` is `Clone` and both clones
    /// share the same underlying session store, so logging in additional
    /// users (editor/viewer, below) through this clone produces tokens the
    /// router actually recognizes (mirrors
    /// `t12_connection_test.rs::TestApp::auth`'s doc comment).
    auth: AuthState,
    users: UsersService,
    pool: SqlitePool,
    manager: Arc<CollectorManager>,
    _env: TempEnv,
}

// See `tests/common/mod.rs`'s module doc ("Why `TestApp` also needs
// `shutdown_test_app`") for why this is required, not optional.
impl Drop for TestApp {
    fn drop(&mut self) {
        common::shutdown_test_app(&self.manager, &self.pool);
    }
}

async fn test_app(label: &str) -> TestApp {
    let env = TempEnv::new(TEMP_ENV_PREFIX, label);
    let pool = init_db(env.registry_path()).await.expect("init_db");

    let users = UsersService::new(pool.clone());
    let audit = AuditLogService::new(pool.clone());
    users
        .setup_first_user("admin", "password123", "管理者")
        .await
        .expect("setup_first_user");

    let verify_users = users.clone();
    let auth = AuthState::new(move |u: String, p: String| {
        let users = verify_users.clone();
        Box::pin(async move {
            match users.verify(&u, &p).await {
                Ok(Some(identity)) => Some(Identity {
                    id: identity.username,
                    name: identity.display_name,
                    role: identity.role.to_string(),
                }),
                _ => None,
            }
        })
    });
    let admin_token = auth
        .login("admin", "password123")
        .await
        .expect("admin login");
    let auth_for_test = auth.clone();

    let sessions = Arc::new(HubSessions::new(banto_broker::BackoffConfig::default()));
    let sim_registry = Arc::new(SlmpSimRegistry::new());
    let computed = Arc::new(ComputedEngine::new(Arc::new(ServerTagStore::new())));
    let manager = Arc::new(CollectorManager::new(
        pool.clone(),
        env.data_dir(),
        Arc::new(banto_tstore::SystemClock),
        fast_options(),
        sessions,
        sim_registry,
        computed,
    ));
    manager.rebuild().await.expect("initial rebuild");

    let (events_tx, _rx) = broadcast::channel(16);
    let write_control = Arc::new(WriteControl::new(false));
    let write_audit = WriteAuditService::new(pool.clone());
    let mqtt = Arc::new(banto_hub_core::mqtt::MqttPublisher::new(manager.clone()));
    let api_keys = ApiKeysService::new(pool.clone());
    let rate_limiter = Arc::new(tokio::sync::Mutex::new(WriteRateLimiter::new(
        WriteRateLimitConfig::default(),
    )));
    let grpc_service = GrpcService::new(
        manager.clone(),
        api_keys.clone(),
        audit.clone(),
        write_audit.clone(),
        write_control.clone(),
        rate_limiter.clone(),
        events_tx.clone(),
    );
    let grpc_server = Arc::new(GrpcServer::new(grpc_service));

    let settings = SettingsService::new(pool.clone());
    let commissioning = CommissioningService::load(settings, users.clone())
        .await
        .expect("CommissioningService::load");
    commissioning
        .lock_down()
        .await
        .expect("lock_down the test environment");

    let router = api_router(
        users.clone(),
        audit,
        PlcConnectionService::new(pool.clone()),
        CollectionGroupService::new(pool.clone()),
        TagService::new(pool.clone()),
        api_keys,
        manager.clone(),
        auth,
        commissioning,
        events_tx,
        false,
        write_control,
        write_audit,
        mqtt,
        grpc_server,
        rate_limiter,
        banto_hub_core::profile_paths::DEFAULT_PROFILE_ID.to_string(),
    );

    TestApp {
        router,
        admin_token,
        auth: auth_for_test,
        users,
        pool,
        manager,
        _env: env,
    }
}

/// 管理系エンドポイント用(CSRF ヘッダ必須)- `t12_connection_test.rs`の
/// `admin_write`と同型(role を問わず任意のトークンで叩けるので、この
/// ファイルでは viewer/editor/admin いずれのトークンにも使う)。
async fn admin_write(
    router: &Router,
    method: &str,
    path: &str,
    token: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method(method)
                .uri(path)
                .header("Authorization", format!("Bearer {token}"))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

/// role 別のログイン済みトークン一式。`app.auth`(router と同じセッション
/// ストアを共有する`AuthState`のクローン)経由でログインするので、発行した
/// トークンは router がそのまま認識する(`TestApp::auth`のdoc comment参照)。
struct RoleTokens {
    viewer: String,
    editor: String,
}

async fn role_tokens(app: &TestApp) -> RoleTokens {
    app.users
        .create_user("viewer1", "password123", "閲覧者", Role::Viewer)
        .await
        .expect("create viewer user");
    app.users
        .create_user("editor1", "password123", "編集者", Role::Editor)
        .await
        .expect("create editor user");
    let viewer = app
        .auth
        .login("viewer1", "password123")
        .await
        .expect("viewer login");
    let editor = app
        .auth
        .login("editor1", "password123")
        .await
        .expect("editor login");
    RoleTokens { viewer, editor }
}

/// 有効な PLC 接続 payload(camelCase, `PlcConnectionPayload`)- 実機/
/// シミュレータへの疎通なしで通る(`plc_connections_create`は persist のみ、
/// PLC には繋がない)。
fn valid_connection_payload(name: &str) -> Value {
    json!({ "name": name, "host": "127.0.0.1", "port": 502 })
}

fn valid_group_payload(name: &str, conn_id: i64) -> Value {
    json!({ "name": name, "plcConnectionId": conn_id, "periodMs": 100 })
}

/// `address`は Modbus 参照番号記法(`crates/banto-plc/src/address.rs`の
/// doc comment参照、`40001`=holding register offset 0) - この payload の
/// 接続は`valid_connection_payload`のとおり protocol 省略時既定の
/// `modbus-tcp`なので、SLMP デバイス記法(`D100`等)ではなくこちらを使う
/// (T2 の PUT で `enabled: true` にした瞬間、preflight のアドレス検証に
/// かかるため)。
fn valid_tag_payload(name: &str, group_id: i64) -> Value {
    json!({
        "name": name,
        "collectionGroupId": group_id,
        "address": "40001",
        "dataType": "u16",
    })
}

// ---------------------------------------------------------------------------
// T1: `require_editor`ゲート - viewer は全対象で 403(rest.rs の
// `require_editor`呼び出し全13箇所、method+path)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn viewer_is_forbidden_by_require_editor_on_every_gated_write_endpoint() {
    let app = test_app("rbac-t1-viewer").await;
    let tokens = role_tokens(&app).await;

    // (method, path, payload) - require_editor は handler 冒頭(Json body の
    // デシリアライズ自体は authorization より手前の axum extractor で走る
    // ため、拒否が require_editor 由来であることを確認できるよう、body は
    // 各エンドポイントの型に対して妥当な最小 payload にしている)。
    let cases: Vec<(&str, &str, Value)> = vec![
        (
            "POST",
            "/api/plc-connections",
            valid_connection_payload("viewer-conn"),
        ),
        (
            "PUT",
            "/api/plc-connections/1",
            valid_connection_payload("viewer-conn"),
        ),
        ("DELETE", "/api/plc-connections/1", json!({})),
        (
            "POST",
            "/api/plc-connections/test",
            json!({ "protocol": "modbus-tcp", "host": "127.0.0.1", "port": 502 }),
        ),
        (
            "POST",
            "/api/collection-groups",
            valid_group_payload("viewer-group", 1),
        ),
        (
            "PUT",
            "/api/collection-groups/1",
            valid_group_payload("viewer-group", 1),
        ),
        ("DELETE", "/api/collection-groups/1", json!({})),
        ("POST", "/api/tags", valid_tag_payload("viewer-tag", 1)),
        ("PUT", "/api/tags/1", valid_tag_payload("viewer-tag", 1)),
        ("DELETE", "/api/tags/1", json!({})),
        ("POST", "/api/tags/batch", json!({ "tags": [] })),
        ("POST", "/api/tags/batch-update", json!({ "tags": [] })),
        ("POST", "/api/tags/batch-delete", json!({ "ids": [] })),
    ];

    for (method, path, payload) in cases {
        let (status, body) = admin_write(&app.router, method, path, &tokens.viewer, payload).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{method} {path} should be forbidden for viewer, got {status} ({body:?})"
        );
    }
}

// ---------------------------------------------------------------------------
// T2: editor は`require_editor`ゲートを通って実際に書ける(admin も同様、
// 正の対照)
// ---------------------------------------------------------------------------

/// 接続 -> グループ -> タグ作成 -> タグ更新 -> タグ削除、を通しで実行し、
/// すべて成功することを確認する(`require_editor`ゲートが editor/admin を
/// 通すことの確認 - 収集は`test_app`の既定どおり停止状態)。
async fn write_through_the_require_editor_gate_succeeds(token: &str, app: &TestApp) {
    let (status, conn) = admin_write(
        &app.router,
        "POST",
        "/api/plc-connections",
        token,
        valid_connection_payload("rbac-conn"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");
    let conn_id = conn["id"].as_i64().expect("connection id");

    let (status, group) = admin_write(
        &app.router,
        "POST",
        "/api/collection-groups",
        token,
        valid_group_payload("rbac-group", conn_id),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{group:?}");
    let group_id = group["id"].as_i64().expect("group id");

    let (status, tag) = admin_write(
        &app.router,
        "POST",
        "/api/tags",
        token,
        valid_tag_payload("rbac-tag", group_id),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{tag:?}");
    let tag_id = tag["id"].as_i64().expect("tag id");

    // PUT /api/tags/{id} - 全項目を送る(`TagPayload`の必須+主要フィールド)。
    let (status, updated) = admin_write(
        &app.router,
        "PUT",
        &format!("/api/tags/{tag_id}"),
        token,
        json!({
            "name": "rbac-tag-renamed",
            "collectionGroupId": group_id,
            "address": "40002",
            "dataType": "i16",
            "stringLength": null,
            "stringEncoding": "utf8",
            "rawLo": 0.0,
            "rawHi": 100.0,
            "engLo": 0.0,
            "engHi": 100.0,
            "unit": "%",
            "decimals": 1,
            "thresholdH": null,
            "thresholdHh": null,
            "thresholdL": null,
            "thresholdLl": null,
            "enabled": true,
            "writable": true,
            "tagKind": "plc",
            "expression": null,
            "retain": false,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated:?}");
    assert_eq!(updated["name"], "rbac-tag-renamed", "{updated:?}");

    let (status, deleted) = admin_write(
        &app.router,
        "DELETE",
        &format!("/api/tags/{tag_id}"),
        token,
        json!({}),
    )
    .await;
    assert!(
        status == StatusCode::OK || status == StatusCode::NO_CONTENT,
        "expected 200 or 204, got {status} ({deleted:?})"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn editor_can_write_through_the_require_editor_gate() {
    let app = test_app("rbac-t2-editor").await;
    let tokens = role_tokens(&app).await;
    write_through_the_require_editor_gate_succeeds(&tokens.editor, &app).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_can_also_write_through_the_require_editor_gate() {
    let app = test_app("rbac-t2-admin").await;
    let admin_token = app.admin_token.clone();
    write_through_the_require_editor_gate_succeeds(&admin_token, &app).await;
}

// ---------------------------------------------------------------------------
// T3: admin 限定ルート(`RoleGuard{min: Role::Admin}`)- editor/viewer は
// 403、admin は通過(403 以外)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn editor_is_forbidden_by_admin_only_role_guard_and_admin_is_allowed() {
    let app = test_app("rbac-t3").await;
    let tokens = role_tokens(&app).await;

    // (method, path, payload-for-admin) - 認可は payload 検証より前に発火
    // するので viewer/editor の 403 には payload は無関係(空 object で
    // よい)。admin の正対照だけ、その handler の request 型に対して妥当な
    // payload を渡す(認可を通過した後、業務検証で 4xx にならない範囲)。
    let cases: Vec<(&str, &str, Value)> = vec![
        (
            "POST",
            "/api/api-keys",
            json!({ "name": "rbac-admin-key", "scopes": [] }),
        ),
        ("POST", "/api/collection/start", json!({})),
        ("POST", "/api/write-control/enable", json!({})),
        (
            "PUT",
            "/api/audit-log/config",
            json!({ "retentionDays": 30, "retentionRows": 1000 }),
        ),
        (
            "PUT",
            "/api/grpc-settings",
            json!({ "enabled": false, "port": 50051 }),
        ),
        (
            "PUT",
            "/api/mqtt-settings",
            json!({
                "enabled": false,
                "host": "localhost",
                "port": 1883,
                "clientId": "rbac-test",
                "prefix": "banto",
                "qos": 0,
                "minIntervalMs": 1000,
            }),
        ),
    ];

    for (method, path, admin_payload) in &cases {
        let (editor_status, editor_body) =
            admin_write(&app.router, method, path, &tokens.editor, json!({})).await;
        assert_eq!(
            editor_status,
            StatusCode::FORBIDDEN,
            "{method} {path} should be forbidden for editor (admin-only route), got {editor_status} ({editor_body:?})"
        );

        let (viewer_status, viewer_body) =
            admin_write(&app.router, method, path, &tokens.viewer, json!({})).await;
        assert_eq!(
            viewer_status,
            StatusCode::FORBIDDEN,
            "{method} {path} should be forbidden for viewer (admin-only route), got {viewer_status} ({viewer_body:?})"
        );

        let (admin_status, admin_body) = admin_write(
            &app.router,
            method,
            path,
            &app.admin_token,
            admin_payload.clone(),
        )
        .await;
        assert_ne!(
            admin_status,
            StatusCode::FORBIDDEN,
            "{method} {path} should be allowed (not FORBIDDEN) for admin - the RoleGuard itself \
             should let it through even if the underlying payload is otherwise rejected, got \
             {admin_status} ({admin_body:?})"
        );
    }
}
