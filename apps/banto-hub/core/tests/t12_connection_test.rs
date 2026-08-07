//! T12 の統合テスト(docs/ux-plan.md §4「PLC 接続テストボタン」、
//! `apps/banto-hub/core/src/rest.rs`の`plc_connections_test`ハンドラ)。
//!
//! `tests/t9_simulation.rs`/`tests/integration.rs`と同じ理由(各`tests/*.rs`は
//! 独立したクレートとしてコンパイルされ、private helper を共有できない)で
//! `TempEnv`/`fast_options`/`wait_until`/`TestApp`/`test_app`/`get_json`/
//! `admin_write`相当をこのファイル内に複製している(`t9_simulation.rs`の
//! ものをベースにした)。broker 経由 SLMP 接続のセットアップは
//! `tests/integration.rs`の`e2e_slmp_session_survives_a_rebuild_via_broker`を
//! 参考にしている。
//!
//! テスト構成:
//! 1. Modbus 成功(シミュレータへ疎通)
//! 2. Modbus 失敗(閉じたポート、kind は "tcp" か "timeout" のどちらかを許容)
//! 3. SLMP 成功(直接ダイヤル、connectionId なし)
//! 4. SLMP 成功(broker 経由の既存セッション再利用、2本目をダイヤルしない
//!    ことを`connection_count()`の不変性で確認)
//! 5. virtual 拒否
//! 6. simulation 拒否
//! 7. 権限(viewer は 403、CSRF ヘッダ無しは拒否)

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request as HttpRequest, StatusCode};
use axum::Router;
use banto_collect::{BackoffConfig, CollectorOptions};
use banto_hub_core::api_keys::ApiKeysService;
use banto_hub_core::audit::AuditLogService;
use banto_hub_core::broker_glue::{HubSessions, SlmpSimRegistry};
use banto_hub_core::computed::{ComputedEngine, ServerTagStore};
use banto_hub_core::db::init_db;
use banto_hub_core::grpc::{GrpcServer, GrpcService};
use banto_hub_core::hub::CollectorManager;
use banto_hub_core::rest::api_router;
use banto_hub_core::users::{Role, UsersService};
use banto_hub_core::write_audit::WriteAuditService;
use banto_hub_core::write_control::WriteControl;
use banto_hub_core::write_rate::{WriteRateLimitConfig, WriteRateLimiter};
use banto_plc::modbus::simulator::Simulator as ModbusSimulator;
use banto_plc::slmp::simulator::Simulator as SlmpSimulator;
use banto_server::{AuthState, Identity};
use banto_tags::{
    CollectionGroupInput, CollectionGroupService, PlcConnectionInput, PlcConnectionService,
    TagInput, TagService,
};
use banto_tstore::SystemClock;
use serde_json::{json, Value};
use sqlx::SqlitePool;
use tokio::sync::broadcast;
use tower::ServiceExt;

const CLIENT_HEADER: (&str, &str) = ("X-Banto-Client", "banto");

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempEnv {
    root: PathBuf,
}

impl TempEnv {
    fn new(label: &str) -> Self {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "banto-hub-t12-it-{}-{label}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("create temp env");
        Self { root }
    }

    fn registry_path(&self) -> PathBuf {
        self.root.join("registry.sqlite3")
    }

    fn data_dir(&self) -> PathBuf {
        self.root.join("data")
    }
}

impl Drop for TempEnv {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

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

fn slmp_conn_input(name: &str, port: u16) -> PlcConnectionInput {
    PlcConnectionInput {
        name: name.to_string(),
        protocol: "slmp".to_string(),
        host: "127.0.0.1".to_string(),
        port: port as i64,
        unit_id: 1,
        enabled: true,
        simulation: false,
    }
}

fn group_input(name: &str, conn_id: i64, period_ms: i64) -> CollectionGroupInput {
    CollectionGroupInput {
        name: name.to_string(),
        plc_connection_id: conn_id,
        period_ms,
        enabled: true,
    }
}

fn tag_input(name: &str, group_id: i64, address: &str, data_type: &str) -> TagInput {
    TagInput {
        name: name.to_string(),
        collection_group_id: group_id,
        address: address.to_string(),
        data_type: data_type.to_string(),
        string_length: None,
        raw_lo: None,
        raw_hi: None,
        eng_lo: None,
        eng_hi: None,
        unit: None,
        decimals: 0,
        threshold_h: None,
        threshold_hh: None,
        threshold_l: None,
        threshold_ll: None,
        enabled: true,
        writable: false,
        tag_kind: "plc".to_string(),
        expression: None,
        retain: false,
    }
}

struct TestApp {
    router: Router,
    admin_token: String,
    /// Clone of the `AuthState` the router was built with (before it was
    /// moved into `api_router`) - `AuthState` is `Clone` and both clones
    /// share the same underlying session store, so logging in additional
    /// users (e.g. the viewer in the RBAC test) through this clone produces
    /// tokens the router actually recognizes.
    auth: AuthState,
    users: UsersService,
    pool: SqlitePool,
    manager: Arc<CollectorManager>,
    /// T2-2/T12: exposed so a test can assert broker session identity
    /// directly (`connection_count`) - here to prove the connection-test
    /// endpoint's broker-reuse path does not dial a second session.
    sessions: Arc<HubSessions>,
    _env: TempEnv,
}

async fn test_app(label: &str) -> TestApp {
    let env = TempEnv::new(label);
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

    let router = api_router(
        users.clone(),
        audit,
        PlcConnectionService::new(pool.clone()),
        CollectionGroupService::new(pool.clone()),
        TagService::new(pool.clone()),
        api_keys,
        manager.clone(),
        auth,
        events_tx,
        false,
        write_control,
        write_audit,
        mqtt,
        grpc_server,
        rate_limiter,
    );

    TestApp {
        router,
        admin_token,
        auth: auth_for_test,
        users,
        pool,
        manager,
        sessions,
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

/// ケース7後半(CSRF ヘッダ無し)専用 - `admin_write`と違い
/// `X-Banto-Client`ヘッダを意図的に付けない。
async fn write_without_csrf_header(
    router: &Router,
    path: &str,
    token: &str,
    body: Value,
) -> StatusCode {
    let response = router
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(path)
                .header("Authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    response.status()
}

/// `POST /api/plc-connections/test`のペイロード(`PlcConnectionTestPayload`の
/// JSON表現、camelCase)。
fn test_payload(protocol: &str, host: &str, port: u16, simulation: bool) -> Value {
    json!({
        "protocol": protocol,
        "host": host,
        "port": port,
        "unitId": 1,
        "simulation": simulation,
    })
}

// ---------------------------------------------------------------------------
// 1. Modbus 成功
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn modbus_test_reports_ok_for_a_reachable_simulator() {
    let app = test_app("t12-modbus-ok").await;
    let sim = ModbusSimulator::start().await;

    let (status, body) = admin_write(
        &app.router,
        "POST",
        "/api/plc-connections/test",
        &app.admin_token,
        test_payload("modbus-tcp", "127.0.0.1", sim.addr.port(), false),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], true, "{body:?}");
    assert!(body["error"].is_null(), "{body:?}");
    assert!(body["elapsedMs"].is_u64(), "{body:?}");
}

// ---------------------------------------------------------------------------
// 2. Modbus 失敗(閉じたポート)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn modbus_test_reports_failure_for_an_unreachable_port() {
    let app = test_app("t12-modbus-fail").await;

    // `TcpListener::bind`してすぐ drop すると、直後は高確率で閉じたポートに
    // なる(ux-plan.md実装指示のとおり)。
    let closed_port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.local_addr().expect("local_addr").port()
    };

    let (status, body) = admin_write(
        &app.router,
        "POST",
        "/api/plc-connections/test",
        &app.admin_token,
        test_payload("modbus-tcp", "127.0.0.1", closed_port, false),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], false, "{body:?}");
    let kind = body["error"]["kind"].as_str().expect("error.kind");
    assert!(
        kind == "tcp" || kind == "timeout",
        "expected tcp or timeout, got {kind} ({body:?})"
    );
}

// ---------------------------------------------------------------------------
// 3. SLMP 成功(直接ダイヤル、connectionId なし)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slmp_test_direct_dial_reports_ok_without_connection_id() {
    let app = test_app("t12-slmp-direct-ok").await;
    let sim = SlmpSimulator::start().await;

    let (status, body) = admin_write(
        &app.router,
        "POST",
        "/api/plc-connections/test",
        &app.admin_token,
        test_payload("slmp", "127.0.0.1", sim.addr.port(), false),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], true, "{body:?}");
    assert!(body["error"].is_null(), "{body:?}");

    sim.stop();
}

// ---------------------------------------------------------------------------
// 4. SLMP 成功(broker 経由の既存セッション再利用、2本目をダイヤルしない)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slmp_test_reuses_existing_broker_session_without_dialing_a_second_connection() {
    let app = test_app("t12-slmp-broker-reuse").await;
    let sim = SlmpSimulator::start().await;

    let conn = PlcConnectionService::new(app.pool.clone())
        .create(slmp_conn_input("line1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("t1", group.id, "D100", "u16"))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild after seeding");

    // Wait for the broker session to actually come up (mirrors
    // `tests/integration.rs::e2e_slmp_session_survives_a_rebuild_via_broker`).
    let mut status_watch = app
        .sessions
        .status_watch(conn.id)
        .expect("a broker session should exist for this connection after rebuild");
    status_watch
        .wait_for(|s| *s == banto_broker::BrokerConnectionStatus::Connected)
        .await
        .expect("broker session should report Connected");

    assert_eq!(app.sessions.connection_count(), 1);

    // Test the connection with `connectionId` set - this must reuse the
    // existing broker session rather than dialing a second one (the whole
    // point of T12's broker-reuse design: the real R08ENCPU only accepts one
    // concurrent SLMP session).
    let (status, body) = admin_write(
        &app.router,
        "POST",
        "/api/plc-connections/test",
        &app.admin_token,
        json!({
            "protocol": "slmp",
            "host": "127.0.0.1",
            "port": sim.addr.port(),
            "unitId": 1,
            "simulation": false,
            "connectionId": conn.id,
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], true, "{body:?}");
    assert!(body["error"].is_null(), "{body:?}");

    // No second session was dialed.
    assert_eq!(
        app.sessions.connection_count(),
        1,
        "the connection test must reuse the existing broker session, not open a second one"
    );

    sim.stop();
}

// ---------------------------------------------------------------------------
// 5. virtual 拒否
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn virtual_protocol_is_rejected_as_unsupported() {
    let app = test_app("t12-virtual").await;

    let (status, body) = admin_write(
        &app.router,
        "POST",
        "/api/plc-connections/test",
        &app.admin_token,
        test_payload("virtual", "127.0.0.1", 1, false),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], false, "{body:?}");
    assert_eq!(body["error"]["kind"], "unsupported", "{body:?}");
}

// ---------------------------------------------------------------------------
// 6. simulation 拒否
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn simulation_flag_is_rejected_as_unsupported() {
    let app = test_app("t12-simulation").await;

    let (status, body) = admin_write(
        &app.router,
        "POST",
        "/api/plc-connections/test",
        &app.admin_token,
        test_payload("modbus-tcp", "127.0.0.1", 1, true),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], false, "{body:?}");
    assert_eq!(body["error"]["kind"], "unsupported", "{body:?}");
}

// ---------------------------------------------------------------------------
// 7. 権限: viewer は 403、CSRF ヘッダ無しは拒否
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn viewer_role_is_forbidden_and_missing_csrf_header_is_rejected() {
    let app = test_app("t12-rbac").await;

    app.users
        .create_user("viewer1", "password123", "閲覧者", Role::Viewer)
        .await
        .expect("create viewer user");
    // `app.auth` is a clone of the same `AuthState` the router was built
    // with (shares the session store), so logging in here produces a token
    // the router actually recognizes - see `TestApp::auth`'s doc comment.
    let viewer_token = app
        .auth
        .login("viewer1", "password123")
        .await
        .expect("viewer login");

    let (status, body) = admin_write(
        &app.router,
        "POST",
        "/api/plc-connections/test",
        &viewer_token,
        test_payload("modbus-tcp", "127.0.0.1", 1, false),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body:?}");

    // Admin token but no CSRF header - rejected by the shared
    // `require_banto_client_header` layer on the whole admin router.
    let status = write_without_csrf_header(
        &app.router,
        "/api/plc-connections/test",
        &app.admin_token,
        test_payload("modbus-tcp", "127.0.0.1", 1, false),
    )
    .await;
    assert_ne!(
        status,
        StatusCode::OK,
        "a request without the X-Banto-Client CSRF header must be rejected"
    );
}
