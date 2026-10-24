//! R2b-6d-1（docs/banto-hub-driver-ui-design.md §4.3）: `POST /api/tags/offset-addresses` と
//! `POST /api/tags/parse-addresses` の `index` を、**実ドライバ**（SLMP・Modbus。core は
//! `banto-drivers` に依存できないのでここに置く）で固定する。実際の `CollectorManager` +
//! axum ルーター経由。`TestApp` は `t12_connection_test.rs` と同じ理由で複製している。

use banto_hub_core::db::Db;
use banto_hub_core::rest::user_session_lookup;
use banto_server::SessionValidation;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request as HttpRequest, StatusCode};
use axum::Router;
use banto_collect::{BackoffConfig, CollectorOptions};
use banto_hub_core::api_keys::ApiKeysService;
use banto_hub_core::audit::AuditLogService;
use banto_hub_core::broker_glue::{BrokerSimRegistry, HubSessions};
use banto_hub_core::commissioning::CommissioningService;
use banto_hub_core::computed::{ComputedEngine, ServerTagStore};
use banto_hub_core::db::init_db;
use banto_hub_core::grpc::{GrpcServer, GrpcService};
use banto_hub_core::hub::CollectorManager;
use banto_hub_core::rest::api_router;
use banto_hub_core::settings::SettingsService;
use banto_hub_core::users::UsersService;
use banto_hub_core::write_audit::WriteAuditService;
use banto_hub_core::write_control::WriteControl;
use banto_hub_core::write_rate::{WriteRateLimitConfig, WriteRateLimiter};
use banto_server::{AuthState, Identity};
use banto_tstore::SystemClock;
use serde_json::{json, Value};
use sqlx::SqlitePool;
use tokio::sync::broadcast;
use tower::ServiceExt;

mod common;
use common::TempEnv;

const CLIENT_HEADER: (&str, &str) = ("X-Banto-Client", "banto");

/// Temp-dir prefix passed to `TempEnv::new` (see `tests/common/mod.rs`).
const TEMP_ENV_PREFIX: &str = "banto-hub-offset-addresses-it";

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

    let users = UsersService::new(Db::Sqlite(pool.clone()));
    let audit = AuditLogService::new(Db::Sqlite(pool.clone()));
    users
        .setup_first_user("admin", "password123", "管理者")
        .await
        .expect("setup_first_user");

    let verify_users = users.clone();
    let verify_users_lookup = users.clone();
    let auth = AuthState::new(
        move |u: String, p: String| {
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
        },
        SessionValidation::lookup(user_session_lookup(verify_users_lookup)),
    );
    let admin_token = auth
        .login("admin", "password123")
        .await
        .expect("admin login");

    let sessions = Arc::new(HubSessions::new(
        banto_broker::BackoffConfig::default(),
        common::hub_drivers(),
    ));
    let sim_registry = Arc::new(BrokerSimRegistry::new());
    let computed = Arc::new(ComputedEngine::new(Arc::new(ServerTagStore::new())));
    let manager = Arc::new(CollectorManager::new(
        pool.clone(),
        env.data_dir(),
        Arc::new(SystemClock),
        fast_options(),
        sessions.clone(),
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

    let settings = SettingsService::new(Db::Sqlite(pool.clone()));
    let commissioning = CommissioningService::load(settings, users.clone(), auth.clone())
        .await
        .expect("CommissioningService::load");
    commissioning
        .lock_down()
        .await
        .expect("lock_down the test environment");

    let router = api_router(
        users.clone(),
        audit,
        manager.plc_connections(),
        manager.collection_groups(),
        manager.tags(),
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
        pool,
        manager,
        _env: env,
    }
}

/// admin 管理系エンドポイント用(CSRF ヘッダ必須)。
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

// --- ここからテスト ------------------------------------------------------

async fn create_connection(app: &TestApp, name: &str, protocol: &str, settings: Value) -> i64 {
    let (status, body) = admin_write(
        &app.router,
        "POST",
        "/api/plc-connections",
        &app.admin_token,
        json!({ "name": name, "protocol": protocol, "settings": settings }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    body["id"].as_i64().unwrap()
}

async fn slmp(app: &TestApp) -> i64 {
    create_connection(
        app,
        "q1",
        "slmp",
        json!({ "host": "127.0.0.1", "port": 15007, "cpu_type": "R" }),
    )
    .await
}

async fn modbus(app: &TestApp) -> i64 {
    create_connection(
        app,
        "m1",
        "modbus-tcp",
        json!({ "host": "127.0.0.1", "port": 15502, "unit_id": 1 }),
    )
    .await
}

fn item(conn: i64, address: &str, data_type: &str, n: i64, unit: &str) -> Value {
    json!({ "connectionId": conn, "address": address, "dataType": data_type, "n": n, "unit": unit })
}

async fn offset(app: &TestApp, items: Vec<Value>) -> Vec<Value> {
    let (status, body) = admin_write(
        &app.router,
        "POST",
        "/api/tags/offset-addresses",
        &app.admin_token,
        json!({ "items": items }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    body["items"].as_array().unwrap().clone()
}

fn ok(result: &Value) -> &str {
    assert_eq!(result["ok"], true, "{result:?}");
    result["canonical"].as_str().unwrap()
}

#[tokio::test]
async fn slmp_points_and_words() {
    let app = test_app("slmp").await;
    let c = slmp(&app).await;
    let r = offset(
        &app,
        vec![
            item(c, "X1F", "bit", 1, "points"),
            item(c, "D100.F", "bit", 1, "points"),
            item(c, "D100.5", "bit", 1, "words"),
            item(c, "M100", "bit", 1, "words"),
            item(c, "D100", "f32", 1, "points"),
            item(c, "D100", "f32", 1, "words"),
            item(c, "D100", "u16", 0, "points"),
        ],
    )
    .await;
    assert_eq!(r.len(), 7);
    assert_eq!(ok(&r[0]), "X20");
    assert_eq!(ok(&r[1]), "D101.0");
    assert_eq!(ok(&r[2]), "D101.5");
    assert_eq!(ok(&r[3]), "M101");
    assert_eq!(ok(&r[4]), "D102");
    assert_eq!(ok(&r[5]), "D101");
    assert_eq!(ok(&r[6]), "D100");
    // 動かした後のアドレスの解析結果（parse-addresses と同じ形）。X は 16 進で 0x20 = 32。
    assert_eq!(r[0]["index"], 32, "{:?}", r[0]);
    assert_eq!(r[5]["index"], 101, "{:?}", r[5]);
    assert_eq!(r[5]["wordCount"], 2, "{:?}", r[5]);
}

#[tokio::test]
async fn modbus_points_words_and_range() {
    let app = test_app("modbus").await;
    let c = modbus(&app).await;
    let r = offset(
        &app,
        vec![
            item(c, "400001", "u16", 1, "points"),
            item(c, "400001.3", "bit", 1, "words"),
            item(c, "400001", "f64", 1, "points"),
            item(c, "400001", "u64", 2, "points"),
            item(c, "465536", "u16", 1, "points"),
            item(c, "400001", "u16", 1, "words"),
        ],
    )
    .await;
    assert_eq!(ok(&r[0]), "400002");
    assert_eq!(ok(&r[1]), "400002.3");
    assert_eq!(ok(&r[2]), "400005");
    assert_eq!(ok(&r[3]), "400009");
    assert_eq!(r[4]["ok"], false, "{:?}", r[4]);
    assert!(r[4]["message"].as_str().is_some_and(|m| !m.is_empty()));
    assert_eq!(ok(&r[5]), "400002");
}

#[tokio::test]
async fn modbus_multi_word_tail_must_stay_in_the_register_space() {
    let app = test_app("modbus-tail").await;
    let c = modbus(&app).await;
    let r = offset(
        &app,
        vec![
            item(c, "465533", "f64", 1, "words"),
            item(c, "465530", "f64", 1, "points"),
            item(c, "465534", "f32", 1, "words"),
            item(c, "465534", "f32", 2, "words"),
            item(c, "465529", "f64", 1, "points"),
        ],
    )
    .await;
    assert_eq!(r[0]["ok"], false, "{:?}", r[0]);
    assert_eq!(r[1]["ok"], false, "{:?}", r[1]);
    assert_eq!(ok(&r[2]), "465535");
    assert_eq!(r[3]["ok"], false, "{:?}", r[3]);
    assert_eq!(ok(&r[4]), "465533");
}

#[tokio::test]
async fn mixed_connections_and_non_driver_connections_are_per_item() {
    let app = test_app("mixed").await;
    let q = slmp(&app).await;
    let m = modbus(&app).await;
    let pg = create_connection(
        &app,
        "erp-db",
        "postgres",
        json!({ "host": "10.0.0.50", "port": 5432, "database": "erp", "username": "reader", "password": "s3cret" }),
    )
    .await;
    let r = offset(
        &app,
        vec![
            item(q, "D100", "u16", 1, "points"),
            item(m, "400001", "u16", 1, "points"),
            item(9999, "D100", "u16", 1, "points"),
            item(pg, "D100", "u16", 1, "points"),
            item(q, "D100", "u16", 2, "words"),
            item(q, "zzz", "u16", 1, "points"),
        ],
    )
    .await;
    assert_eq!(r.len(), 6);
    assert_eq!(ok(&r[0]), "D101");
    assert_eq!(ok(&r[1]), "400002");
    assert_eq!(r[2]["ok"], false, "{:?}", r[2]);
    assert_eq!(r[3]["ok"], false, "{:?}", r[3]);
    assert_eq!(ok(&r[4]), "D102");
    assert_eq!(r[5]["ok"], false, "{:?}", r[5]);
}

#[tokio::test]
async fn too_many_items_is_a_validation_error() {
    let app = test_app("limit").await;
    let c = slmp(&app).await;
    let items = vec![item(c, "D100", "u16", 1, "points"); 10_001];
    let (status, _) = admin_write(
        &app.router,
        "POST",
        "/api/tags/offset-addresses",
        &app.admin_token,
        json!({ "items": items }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn parse_addresses_returns_the_numeric_index() {
    let app = test_app("index").await;
    let q = slmp(&app).await;
    let m = modbus(&app).await;
    let x = probe(&app, q, "X1F", "bit").await;
    assert_eq!(x["index"], 31, "{x:?}");
    let d = probe(&app, q, "D100", "u16").await;
    assert_eq!(d["index"], 100, "{d:?}");
    let h = probe(&app, m, "400001", "u16").await;
    assert_eq!(h["ok"], true, "{h:?}");
    assert!(h.get("index").is_some(), "{h:?}");
}

async fn probe(app: &TestApp, conn: i64, address: &str, dt: &str) -> Value {
    let (status, body) = admin_write(
        &app.router,
        "POST",
        "/api/tags/parse-addresses",
        &app.admin_token,
        json!({ "connectionId": conn, "items": [
            { "tagKind": "plc", "address": address, "dataType": dt, "stringLength": null, "stringEncoding": "utf8" }
        ] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    body["items"][0].clone()
}
