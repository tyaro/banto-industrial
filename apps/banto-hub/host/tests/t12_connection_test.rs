//! T12 の統合テスト(docs/ux-plan.md §4「PLC 接続テストボタン」、
//! `apps/banto-hub/core/src/rest.rs`の`plc_connections_test`ハンドラ)。
//!
//! `tests/t9_simulation.rs`/`tests/integration.rs`と同じ理由(各`tests/*.rs`は
//! 独立したクレートとしてコンパイルされ、private helper を共有できない)で
//! `fast_options`/`wait_until`/`TestApp`/`test_app`/`get_json`/`admin_write`
//! 相当をこのファイル内に複製している(`t9_simulation.rs`のものをベースに
//! した)。`TempEnv`は`tests/common/mod.rs`に集約済み(2026-08-08、テスト
//! 一時ディレクトリリークの根治)。broker 経由 SLMP 接続のセットアップは
//! `tests/integration.rs`の`e2e_slmp_session_survives_a_rebuild_via_broker`を
//! 参考にしている。
//!
//! テスト構成:
//! 1. Modbus 成功(シミュレータへ疎通)
//! 2. Modbus 失敗(閉じたポート、kind は "tcp" か "timeout" のどちらかを許容)
//! 3. SLMP 成功(直接ダイヤル、connectionId なし)
//! 4. SLMP 成功(broker 経由の既存セッション再利用、2本目をダイヤルしない
//!    ことを`connection_count()`の不変性で確認)
//! 5. Modbus 成功(broker 経由の既存セッション再利用、2本目のソケットを
//!    張らないことを`banto_plc::modbus::simulator::Simulator::connection_count()`
//!    の不変性で確認 - #337、2026-09-08 オーナー決定)
//! 6. virtual 拒否
//! 7. simulation 拒否
//! 8. 権限(viewer は 403、CSRF ヘッダ無しは拒否)
//! 9. R2b-6b: `GET /api/drivers`(機器のドライバだけがレジストリの順に載る・JSON の形・
//!    未認証は 401・viewer は読める)

use banto_hub_core::db::Db;
use banto_hub_core::rest::user_session_lookup;
use banto_server::SessionValidation;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request as HttpRequest, StatusCode};
use axum::Router;
use banto_collect::{BackoffConfig, CollectorOptions};
use banto_driver_api::{
    AddressError, Capabilities, DriverAddress, DriverBase, DriverDescriptor, DriverEntry, DriverId,
    FatalError, FieldError, FieldKind, FrameConnection, FrameDriver, SettingField, Settings,
    SettingsSchema, TagKind,
};
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
use banto_hub_core::users::{Role, UsersService};
use banto_hub_core::write_audit::WriteAuditService;
use banto_hub_core::write_control::WriteControl;
use banto_hub_core::write_rate::{WriteRateLimitConfig, WriteRateLimiter};
use banto_plc::modbus::simulator::Simulator as ModbusSimulator;
use banto_plc::slmp::simulator::Simulator as SlmpSimulator;
use banto_server::{AuthState, Identity};
use banto_tags::{
    CollectionGroupInput, CollectionGroupService, ConnectionSettingsInput, PlcConnectionInput,
    PlcConnectionService, TagInput, TagService,
};
use banto_tstore::SystemClock;
use serde_json::{json, Value};
use sqlx::SqlitePool;
use tokio::sync::broadcast;
use tower::ServiceExt;

mod common;
use common::TempEnv;

const CLIENT_HEADER: (&str, &str) = ("X-Banto-Client", "banto");

/// Temp-dir prefix passed to `TempEnv::new` (see `tests/common/mod.rs`).
const TEMP_ENV_PREFIX: &str = "banto-hub-t12-it";

/// 組み込み Modbus の文法・設定を使い、必須の秘密の欄だけを足すテスト専用ドライバ。
struct RequiredSecretDriver {
    descriptor: DriverDescriptor,
    delegate: DriverEntry,
}

impl DriverBase for RequiredSecretDriver {
    fn descriptor(&self) -> &DriverDescriptor {
        &self.descriptor
    }
    fn validate_config(&self, settings: &Settings) -> Vec<FieldError> {
        self.delegate.base().validate_config(settings)
    }
    fn secret_keys(&self) -> Vec<String> {
        vec!["token".into()]
    }
    fn settings_schema(&self) -> SettingsSchema {
        let mut schema = self.delegate.base().settings_schema();
        schema
            .fields
            .push(SettingField::new("token", "トークン", FieldKind::secret()).required());
        schema
    }
    fn parse_address(&self, text: &str, kind: &TagKind) -> Result<DriverAddress, AddressError> {
        self.delegate.base().parse_address(text, kind)
    }
    fn offset_address(&self, text: &str, kind: &TagKind, n: i64) -> Result<String, AddressError> {
        self.delegate.base().offset_address(text, kind, n)
    }
    fn address_help(&self) -> String {
        self.delegate.base().address_help()
    }
}

impl FrameDriver for RequiredSecretDriver {
    fn create_connection(
        &self,
        _settings: &Settings,
    ) -> Result<Box<dyn FrameConnection>, FatalError> {
        panic!("必須 Secret の保存テストは接続しない")
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

        word_order: "low_high".to_string(),
        cpu_type: None,
        database: None,
        username: None,
        secrets: Default::default(),
        settings: None,
    }
}

/// [`slmp_conn_input`]の Modbus TCP 版(`tests/integration.rs`の`conn_input`と
/// 同じ形) - ケース5(broker セッション再利用)で収集を成立させるために使う。
fn modbus_conn_input(name: &str, port: u16) -> PlcConnectionInput {
    PlcConnectionInput {
        name: name.to_string(),
        protocol: "modbus-tcp".to_string(),
        host: "127.0.0.1".to_string(),
        port: port as i64,
        unit_id: 1,
        enabled: true,
        simulation: false,

        word_order: "low_high".to_string(),
        cpu_type: None,
        database: None,
        username: None,
        secrets: Default::default(),
        settings: None,
    }
}

fn group_input(name: &str, conn_id: i64, period_ms: i64) -> CollectionGroupInput {
    CollectionGroupInput {
        name: name.to_string(),
        plc_connection_id: conn_id,
        period_ms,
        enabled: true,
        default_writable: true,
        query_sql: None,
    }
}

fn tag_input(name: &str, group_id: i64, address: &str, data_type: &str) -> TagInput {
    TagInput {
        name: name.to_string(),
        collection_group_id: group_id,
        address: address.to_string(),
        data_type: data_type.to_string(),
        string_length: None,
        string_encoding: "utf8".to_string(),
        raw_lo: None,
        raw_hi: None,
        eng_lo: None,
        eng_hi: None,
        unit: None,
        decimals: 0,
        enabled: true,
        writable: false,
        tag_kind: "plc".to_string(),
        expression: None,
        retain: false,
        expected_revision: None,
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

// See `tests/common/mod.rs`'s module doc ("Why `TestApp` also needs
// `shutdown_test_app`") for why this is required, not optional.
impl Drop for TestApp {
    fn drop(&mut self) {
        common::shutdown_test_app(&self.manager, &self.pool);
    }
}

async fn test_app(label: &str) -> TestApp {
    test_app_with_drivers(label, false, None).await
}

async fn test_app_with_secret_driver(label: &str, with_secret_driver: bool) -> TestApp {
    test_app_with_drivers(label, with_secret_driver, None).await
}

async fn test_app_with_wasm(label: &str, component: &[u8]) -> TestApp {
    test_app_with_drivers(label, false, Some(component)).await
}

async fn test_app_with_drivers(
    label: &str,
    with_secret_driver: bool,
    wasm_component: Option<&[u8]>,
) -> TestApp {
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
    let auth_for_test = auth.clone();

    let mut drivers = common::hub_drivers();
    if let Some(component) = wasm_component {
        banto_drivers::register_wasm(
            Arc::get_mut(&mut drivers.registry).unwrap(),
            include_bytes!("../../../../crates/banto-driver-wasm/tests/manifest.json"),
            component,
        )
        .expect("register WASM frame driver");
    }
    if with_secret_driver {
        let delegate = drivers.registry.get("modbus-tcp").unwrap().clone();
        Arc::get_mut(&mut drivers.registry)
            .unwrap()
            .register(DriverEntry::frame(Arc::new(RequiredSecretDriver {
                descriptor: DriverDescriptor::new(
                    DriverId::new("secret-test").unwrap(),
                    "0.0.0-test",
                    "必須 Secret のテスト",
                    Capabilities::READ_WRITE,
                ),
                delegate,
            })))
            .unwrap();
    }
    let sessions = Arc::new(HubSessions::new(
        banto_broker::BackoffConfig::default(),
        drivers,
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

async fn admin_get(router: &Router, path: &str, token: &str) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            HttpRequest::builder()
                .uri(path)
                .header("Authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
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

/// `POST /api/plc-connections/test`のペイロード(R2b-6b の `PlcConnectionTestPayload`:
/// `{ protocol, simulation, settings, connectionId? }`。`settings` はドライバのスキーマの
/// 欄で、`unit_id` は Modbus だけ)。
fn test_payload(protocol: &str, host: &str, port: u16, simulation: bool) -> Value {
    let settings = if protocol == "modbus-tcp" {
        json!({ "host": host, "port": port, "unit_id": 1 })
    } else {
        json!({ "host": host, "port": port })
    };
    json!({ "protocol": protocol, "simulation": simulation, "settings": settings })
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
    // 省略時は移行前と同じ R フレーム（ワードのサブコマンド 0x0002）。
    assert_eq!(sim.last_subcommand(), Some(0x0002), "{body:?}");

    sim.stop();
}

/// #605: 直接ダイヤルはフォームの CPU 種別をフレームに載せる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slmp_test_direct_dial_uses_the_requested_cpu_type() {
    let app = test_app("t12-slmp-cpu").await;
    for (cpu, sub) in [("R", 0x0002u16), ("Q", 0x0000u16)] {
        let sim = SlmpSimulator::start().await;
        let mut payload = test_payload("slmp", "127.0.0.1", sim.addr.port(), false);
        payload["settings"]["cpu_type"] = json!(cpu);
        let (status, body) = admin_write(
            &app.router,
            "POST",
            "/api/plc-connections/test",
            &app.admin_token,
            payload,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{cpu} {body:?}");
        assert_eq!(body["ok"], true, "{cpu} {body:?}");
        assert_eq!(sim.last_subcommand(), Some(sub), "{cpu}");
        sim.stop();
    }
}

/// R2b-6b（設計 §5）: 接続テストの `settings` はフォームの値だけで組む。`cpu_type` を省くと、
/// `connectionId` の保存済み接続が Q でも、スキーマの既定（iQ-R）でフレームを組む（#790 の
/// 「保存済みの CPU を引き継ぐ」は MCP の平らな形のアダプタだけに残した）。収集していない
/// 接続（共有セッションが無い）なので直接ダイヤルの経路を通る。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slmp_test_direct_dial_takes_the_omitted_cpu_from_the_schema_default() {
    let app = test_app("t12-slmp-cpu-inherit").await;
    let sim = SlmpSimulator::start().await;
    let service = PlcConnectionService::new(app.pool.clone());
    let mut input = slmp_conn_input("lineq", sim.addr.port());
    input.cpu_type = Some("Q".to_string());
    let slmp_q = service.create(input).await.unwrap();
    let modbus = service
        .create(modbus_conn_input("modbus", sim.addr.port()))
        .await
        .unwrap();
    assert!(
        app.sessions.handle_for(slmp_q.id).is_none(),
        "収集していない"
    );

    for (id, sub, why) in [
        (slmp_q.id, 0x0002u16, "保存済みが Q でもスキーマの既定 R"),
        (modbus.id, 0x0002u16, "プロトコルが違ってもスキーマの既定 R"),
    ] {
        let mut payload = test_payload("slmp", "127.0.0.1", sim.addr.port(), false);
        payload["connectionId"] = json!(id);
        let (status, body) = admin_write(
            &app.router,
            "POST",
            "/api/plc-connections/test",
            &app.admin_token,
            payload,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{why} {body:?}");
        assert_eq!(body["ok"], true, "{why} {body:?}");
        assert_eq!(sim.last_subcommand(), Some(sub), "{why}");
    }
    sim.stop();
}

/// #605: 未知の CPU 種別はダイヤルせず拒否する（R への黙ったフォールバックではない）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slmp_test_rejects_an_unknown_cpu_type() {
    let app = test_app("t12-slmp-cpu-unknown").await;
    let mut payload = test_payload("slmp", "127.0.0.1", 1, false);
    payload["settings"]["cpu_type"] = json!("L");
    let (status, body) = admin_write(
        &app.router,
        "POST",
        "/api/plc-connections/test",
        &app.admin_token,
        payload,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], false, "{body:?}");
    assert_eq!(body["error"]["kind"], "protocol", "{body:?}");
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
    // concurrent SLMP session per port, and this connection's port already
    // has the broker's own session on it).
    let (status, body) = admin_write(
        &app.router,
        "POST",
        "/api/plc-connections/test",
        &app.admin_token,
        json!({ "protocol": "slmp", "simulation": false, "connectionId": conn.id, "settings": { "host": "127.0.0.1", "port": sim.addr.port() } }),
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

/// #605: 保存済みセッションが Q のとき、フォームの CPU が R なら、セッションの
/// 読み（Q のフレーム）をフォームの値の結果として返さない。2 本目もダイヤルせず
/// （1 本しか受けない機器の誤診を避ける）、「保存してからテストしてください」と
/// 返す。同じ CPU（Q）なら従来どおりセッションを再利用して読む。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slmp_test_refuses_a_session_whose_cpu_differs_from_the_form() {
    let app = test_app("t12-slmp-session-cpu").await;
    let sim = SlmpSimulator::start().await;

    let mut input = slmp_conn_input("lineq", sim.addr.port());
    input.cpu_type = Some("Q".to_string());
    let conn = PlcConnectionService::new(app.pool.clone())
        .create(input)
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
    let mut status_watch = app
        .sessions
        .status_watch(conn.id)
        .expect("a broker session should exist");
    status_watch
        .wait_for(|s| *s == banto_broker::BrokerConnectionStatus::Connected)
        .await
        .expect("broker session should report Connected");
    assert_eq!(app.sessions.connection_count(), 1);

    let post = |cpu: &'static str| {
        let router = app.router.clone();
        let token = app.admin_token.clone();
        let port = sim.addr.port();
        let id = conn.id;
        async move {
            admin_write(
                &router,
                "POST",
                "/api/plc-connections/test",
                &token,
                json!({ "protocol": "slmp", "simulation": false, "connectionId": id, "settings": { "host": "127.0.0.1", "port": port, "cpu_type": cpu } }),
            )
            .await
        }
    };

    let (status, body) = post("R").await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], false, "{body:?}");
    assert_eq!(body["error"]["kind"], "protocol", "{body:?}");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(message.contains("保存してからテスト"), "{message}");
    assert_eq!(app.sessions.connection_count(), 1, "2 本目をダイヤルしない");

    let (status, body) = post("Q").await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], true, "{body:?}");
    assert_eq!(sim.last_subcommand(), Some(0x0000), "Q のフレームで読む");
    assert_eq!(app.sessions.connection_count(), 1);

    sim.stop();
}

/// 収集中の共有セッションが張れている状態にして、接続 id を返す（SLMP / Modbus 共通）。
async fn start_collecting(app: &TestApp, input: PlcConnectionInput) -> i64 {
    let conn = PlcConnectionService::new(app.pool.clone())
        .create(input)
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    let (address, data_type) = if conn.protocol == "slmp" {
        ("D100", "u16")
    } else {
        ("40001", "u16")
    };
    TagService::new(app.pool.clone())
        .create(tag_input("t1", group.id, address, data_type))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild after seeding");
    let mut status_watch = app
        .sessions
        .status_watch(conn.id)
        .expect("a broker session should exist");
    status_watch
        .wait_for(|s| *s == banto_broker::BrokerConnectionStatus::Connected)
        .await
        .expect("broker session should report Connected");
    conn.id
}

/// R2b-3a-2（Codex のレビュー）: 収集中の共有セッションと、フォームの host / port /
/// unit_id が食い違うときは、そのセッションの読みをフォームの結果として返さない。
/// 2 本目もダイヤルせず「保存してからテスト」と返す。
async fn assert_form_differing_from_the_session_is_refused(
    name: &str,
    protocol: &str,
    change: impl FnOnce(&mut serde_json::Value),
) {
    let app = test_app(name).await;
    let (port, input, _sim): (u16, PlcConnectionInput, Box<dyn std::any::Any>) =
        if protocol == "slmp" {
            let sim = SlmpSimulator::start().await;
            let port = sim.addr.port();
            (port, slmp_conn_input("line1", port), Box::new(sim))
        } else {
            let sim = ModbusSimulator::start().await;
            let port = sim.addr.port();
            (port, modbus_conn_input("line1", port), Box::new(sim))
        };
    let id = start_collecting(&app, input).await;
    let before = app.sessions.connection_count();
    // The form matches the saved row (Modbus: the saved `low_high`, not the schema default).
    let mut payload = if protocol == "slmp" {
        json!({ "protocol": protocol, "simulation": false, "connectionId": id,
                "settings": { "host": "127.0.0.1", "port": port } })
    } else {
        json!({ "protocol": protocol, "simulation": false, "connectionId": id,
                "settings": { "host": "127.0.0.1", "port": port, "unit_id": 1,
                              "word_order": "low_high" } })
    };
    change(&mut payload);
    let (status, body) = admin_write(
        &app.router,
        "POST",
        "/api/plc-connections/test",
        &app.admin_token,
        payload,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], false, "{body:?}");
    assert_eq!(body["error"]["kind"], "protocol", "{body:?}");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(message.contains("保存してからテスト"), "{message}");
    assert_eq!(
        app.sessions.connection_count(),
        before,
        "2 本目をダイヤルしない"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_refuses_a_session_whose_host_differs_from_the_form() {
    assert_form_differing_from_the_session_is_refused("t12-session-host", "slmp", |p| {
        p["settings"]["host"] = json!("127.0.0.2");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_refuses_a_session_whose_port_differs_from_the_form() {
    assert_form_differing_from_the_session_is_refused("t12-session-port", "slmp", |p| {
        let port = p["settings"]["port"].as_u64().unwrap();
        p["settings"]["port"] = json!(port + 1);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn modbus_test_refuses_a_session_whose_unit_id_differs_from_the_form() {
    assert_form_differing_from_the_session_is_refused("t12-session-unit", "modbus-tcp", |p| {
        p["settings"]["unit_id"] = json!(2);
    })
    .await;
}

// ---------------------------------------------------------------------------
// 5. Modbus 成功(broker 経由の既存セッション再利用、2本目のソケットを張らない)
// ---------------------------------------------------------------------------

/// #337(2026-09-08 オーナー決定): Modbus TCP の収集読み取りも broker
/// セッションへ相乗りするようになり、Modbus も1接続=1ソケットになった。
/// しかし接続テストボタン(`test_modbus_connection`)だけは既存 broker
/// セッションを再利用せず毎回新規ダイヤルしていたため、KM-D1-ETN のような
/// 1対1接続しか受け付けない機器では、収集稼働中に接続テストを押すと2本目が
/// 拒否され、収集は正常なのに「失敗」と誤診断されていた。
///
/// このテストは「収集を実際に稼働させた状態(broker セッションが生きている
/// 状態)で接続テストを実行しても、
/// `banto_plc::modbus::simulator::Simulator::connection_count()`(累計受理
/// ソケット数)が増えない」ことを直接確認する - `app.sessions.connection_count()`
/// (ケース4の SLMP テストが使う broker 側の指標)は直結クライアントの
/// ダイヤルを検出できないため、ここでは実ソケット数を数えるシミュレータ側の
/// カウンタを使う([`tests/integration.rs`]の
/// `modbus_collection_uses_exactly_one_socket`と同じ考え方)。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn modbus_test_reuses_existing_broker_session_without_dialing_a_second_socket() {
    let app = test_app("t12-modbus-broker-reuse").await;
    let sim = ModbusSimulator::start().await;

    let conn = PlcConnectionService::new(app.pool.clone())
        .create(modbus_conn_input("line1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("temp01", group.id, "40001", "u16"))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild after seeding");

    // broker セッションが実際に張られる(= 収集が稼働している)のを待つ
    // (ケース4の SLMP テストと同じ手順)。
    let mut status_watch = app
        .sessions
        .status_watch(conn.id)
        .expect("a broker session should exist for this connection after rebuild");
    status_watch
        .wait_for(|s| *s == banto_broker::BrokerConnectionStatus::Connected)
        .await
        .expect("broker session should report Connected");

    assert_eq!(
        sim.connection_count(),
        1,
        "collection should hold one socket"
    );

    // 接続テストを実行 - `connectionId`を渡し、既存 broker セッションを
    // 再利用させる。
    let (status, body) = admin_write(
        &app.router,
        "POST",
        "/api/plc-connections/test",
        &app.admin_token,
        json!({ "protocol": "modbus-tcp", "simulation": false, "connectionId": conn.id,
                "settings": { "host": "127.0.0.1", "port": sim.addr.port(), "unit_id": 1,
                              "word_order": "low_high" } }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], true, "{body:?}");
    assert!(body["error"].is_null(), "{body:?}");

    // 本命: シミュレータが受理した接続の累計が収集開始時のまま(1本)である
    // こと。修正前はここが2になる(broker セッション + 接続テスト用の直結
    // クライアント)。
    assert_eq!(
        sim.connection_count(),
        1,
        "the connection test must reuse the existing broker session, not open a second socket (#337)"
    );

    sim.stop();
}

/// The canonical component is supplied explicitly after the test host builds
/// it. This runs the ordinary REST connection test through the broker's
/// FrameSession and controlled local Modbus TCP, without a second socket for
/// an active connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_COMPONENT_WASM to a built canonical component"]
async fn wasm_fc3_connection_test_direct_reuse_and_changed_settings() {
    let component = std::fs::read(std::env::var("BANTO_COMPONENT_WASM").expect("component path"))
        .expect("component bytes");
    let app = test_app_with_wasm("t12-wasm-fc3", &component).await;
    let sim = ModbusSimulator::start().await;
    sim.set_holding_registers(0, &[42, 0x1234, 0xabcd]);
    let settings = json!({
        "host": "127.0.0.1", "port": sim.addr.port(), "unit_id": 1,
        "word_order": "low_high"
    });
    let payload = json!({
        "protocol": "modbus-fc3-wasm", "simulation": false, "settings": settings
    });

    // No connectionId: the common test path dials, reads probe 40001, then closes.
    let (status, body) = admin_write(
        &app.router,
        "POST",
        "/api/plc-connections/test",
        &app.admin_token,
        payload.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], true, "{body:?}");
    assert_eq!(sim.connection_count(), 1);

    let saved = app
        .manager
        .plc_connections()
        .create_settings(ConnectionSettingsInput {
            name: "wasm-line".into(),
            protocol: "modbus-fc3-wasm".into(),
            enabled: true,
            simulation: false,
            settings: settings.as_object().unwrap().clone(),
            secrets: Default::default(),
        })
        .await
        .expect("save WASM connection");
    // A saved id with no active broker session still takes the direct path.
    let mut matching = payload.clone();
    matching["connectionId"] = json!(saved.id);
    let (_, body) = admin_write(
        &app.router,
        "POST",
        "/api/plc-connections/test",
        &app.admin_token,
        matching.clone(),
    )
    .await;
    assert_eq!(body["ok"], true, "{body:?}");
    assert_eq!(sim.connection_count(), 2);

    let group =
        CollectionGroupService::with_registry(app.pool.clone(), app.manager.registry().clone())
            .create(group_input("wasm-fast", saved.id, 100))
            .await
            .expect("create group");
    let u16_tag = app
        .manager
        .tags()
        .create(tag_input("wasm-u16", group.id, "40001", "u16"))
        .await
        .expect("create U16 tag");
    let u32_tag = app
        .manager
        .tags()
        .create(tag_input("wasm-u32", group.id, "40002", "u32"))
        .await
        .expect("create U32 tag");
    app.manager.rebuild().await.expect("start WASM collection");
    let mut watch = app.sessions.status_watch(saved.id).expect("broker session");
    tokio::time::timeout(
        Duration::from_secs(5),
        watch.wait_for(|status| *status == banto_broker::BrokerConnectionStatus::Connected),
    )
    .await
    .expect("WASM connection timeout")
    .expect("WASM connection is connected");
    assert_eq!(sim.connection_count(), 3);

    let u16_key = format!("tag:{}", u16_tag.id);
    let u32_key = format!("tag:{}", u32_tag.id);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let u16_value = app
                .manager
                .current_values()
                .and_then(|values| values.get(&u16_key))
                .and_then(|value| value.value);
            let u32_value = app
                .manager
                .current_values()
                .and_then(|values| values.get(&u32_key))
                .and_then(|value| value.value);
            if u16_value == Some(42.0) && u32_value == Some(0xabcd_1234u32 as f64) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("WASM U16/U32 collection values");
    let (status, body) = admin_get(
        &app.router,
        "/api/v1/values/wasm-line.wasm-fast.wasm-u16",
        &app.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["v"], 42.0, "{body:?}");
    assert_eq!(body["q"], "good", "{body:?}");
    let path = "/api/v1/values/wasm-line.wasm-fast.wasm-u32";
    let (status, body) = admin_get(&app.router, path, &app.admin_token).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["v"], 0xabcd_1234u32 as f64, "{body:?}");
    assert_eq!(body["q"], "good", "{body:?}");
    let (status, body) =
        admin_get(&app.router, &format!("{path}/read-now"), &app.admin_token).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["v"], 0xabcd_1234u32 as f64, "{body:?}");
    assert_eq!(body["q"], "good", "{body:?}");

    // Matching saved settings reuse the one active session.
    let (_, body) = admin_write(
        &app.router,
        "POST",
        "/api/plc-connections/test",
        &app.admin_token,
        matching.clone(),
    )
    .await;
    assert_eq!(body["ok"], true, "{body:?}");
    assert_eq!(sim.connection_count(), 3, "must reuse the broker socket");

    // An unsaved form change cannot be tested through that old session.
    matching["settings"]["host"] = json!("127.0.0.2");
    let (_, body) = admin_write(
        &app.router,
        "POST",
        "/api/plc-connections/test",
        &app.admin_token,
        matching,
    )
    .await;
    assert_eq!(body["ok"], false, "{body:?}");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("保存してからテスト"));
    assert_eq!(sim.connection_count(), 3, "must not dial changed settings");
    sim.stop();
}

// ---------------------------------------------------------------------------
// 6. virtual 拒否
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
// 7. simulation 拒否
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

/// R2b-6b（設計 §5・§10）: `simulation: true` は新規（`connectionId` 無し）でも編集中
/// （保存済みの接続の id つき）でも、ダイヤルの前に断る。実機の代わりのシミュレータは
/// 1 本も受けず（ダイヤル 0 回・読み取り 0 回）。保存済みの行は実機（`simulation: false`）の
/// まま、フォームの今の値だけが SIM。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn simulation_true_never_dials_for_a_new_or_an_edited_connection() {
    let app = test_app("t12-simulation-edit").await;
    let sim = ModbusSimulator::start().await;
    let saved = PlcConnectionService::new(app.pool.clone())
        .create(modbus_conn_input("line1", sim.addr.port()))
        .await
        .unwrap();
    assert!(!saved.simulation);
    for connection_id in [None, Some(saved.id)] {
        let mut payload = test_payload("modbus-tcp", "127.0.0.1", sim.addr.port(), true);
        if let Some(id) = connection_id {
            payload["connectionId"] = json!(id);
        }
        let (status, body) = admin_write(
            &app.router,
            "POST",
            "/api/plc-connections/test",
            &app.admin_token,
            payload,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(body["ok"], false, "{body:?}");
        assert_eq!(body["error"]["kind"], "unsupported", "{body:?}");
    }
    assert_eq!(sim.connection_count(), 0, "no dial, no read");
    sim.stop();
}

/// R2b-6b（設計 §5・§10）: 編集中の接続テストは、保存済みの値ではなくフォームの値（変えた
/// ポート）で張る。保存済みの行は閉じたポートを指しているが、フォームはシミュレータの
/// ポートなので通る。収集していない接続なので直接ダイヤルの経路。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_edited_connection_is_tested_with_the_form_values() {
    let app = test_app("t12-edit-values").await;
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let closed_port = closed.local_addr().unwrap().port();
    drop(closed);
    let saved = PlcConnectionService::new(app.pool.clone())
        .create(slmp_conn_input("line1", closed_port))
        .await
        .unwrap();
    let sim = SlmpSimulator::start().await;
    let mut payload = test_payload("slmp", "127.0.0.1", sim.addr.port(), false);
    payload["connectionId"] = json!(saved.id);
    payload["settings"]["cpu_type"] = json!("Q");
    let (status, body) = admin_write(
        &app.router,
        "POST",
        "/api/plc-connections/test",
        &app.admin_token,
        payload,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], true, "{body:?}");
    assert_eq!(sim.last_subcommand(), Some(0x0000), "the form's Q frame");
    sim.stop();
}

/// R2b-6b: 接続テストの `settings` はスキーマに通す。未知のキー・型違い（文字列の
/// ポート）はダイヤルせずに断り、平らな形の古い本文は 422。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_connection_test_refuses_unknown_keys_wrong_types_and_the_flat_shape() {
    let app = test_app("t12-test-schema").await;
    let sim = ModbusSimulator::start().await;
    let mut payload = test_payload("modbus-tcp", "127.0.0.1", sim.addr.port(), false);
    payload["settings"]["port"] = json!(sim.addr.port().to_string());
    payload["settings"]["cpu_type"] = json!("R");
    let (status, body) = admin_write(
        &app.router,
        "POST",
        "/api/plc-connections/test",
        &app.admin_token,
        payload,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], false, "{body:?}");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("settings.port: ") && message.contains("settings.cpu_type: "),
        "{message}"
    );
    assert_eq!(sim.connection_count(), 0, "no dial");

    let (status, body) = admin_write(
        &app.router,
        "POST",
        "/api/plc-connections/test",
        &app.admin_token,
        json!({ "protocol": "modbus-tcp", "host": "127.0.0.1", "port": sim.addr.port() }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    sim.stop();
}

/// R2b-7b: 公開 REST の作成・更新・接続テストは機器ドライバの欄を
/// `settings.<key>`（スキーマの snake_case）で返す。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn driver_connection_field_errors_use_settings_keys() {
    let app = test_app("t12-settings-field-errors").await;
    let (status, body) = admin_write(
        &app.router,
        "POST",
        "/api/plc-connections",
        &app.admin_token,
        json!({
            "name": "bad-driver",
            "protocol": "modbus-tcp",
            "enabled": false,
            "settings": { "port": "502", "unit_id": 256, "unexpected": true }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    let fields: Vec<&str> = body["field_errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|error| error["field"].as_str().unwrap())
        .collect();
    assert_eq!(
        fields,
        [
            "settings.host",
            "settings.port",
            "settings.unit_id",
            "settings.unexpected"
        ]
    );

    let (status, body) = admin_write(
        &app.router,
        "POST",
        "/api/plc-connections",
        &app.admin_token,
        json!({
            "name": "driver-for-update",
            "protocol": "modbus-tcp",
            "enabled": false,
            "settings": { "host": "127.0.0.1", "port": 502, "unit_id": 1 }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let id = body["id"].as_i64().unwrap();
    let (status, body) = admin_write(
        &app.router,
        "PUT",
        &format!("/api/plc-connections/{id}"),
        &app.admin_token,
        json!({
            "name": "driver-for-update",
            "protocol": "modbus-tcp",
            "enabled": false,
            "settings": { "host": "127.0.0.1", "port": 70000, "unit_id": 1 }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    assert_eq!(body["field_errors"][0]["field"], "settings.port");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_required_driver_secret_is_reported_as_a_settings_field() {
    let app = test_app_with_secret_driver("t12-required-secret", true).await;
    let (status, body) = admin_write(
        &app.router,
        "POST",
        "/api/plc-connections",
        &app.admin_token,
        json!({
            "name": "needs-token",
            "protocol": "secret-test",
            "enabled": false,
            "settings": { "host": "127.0.0.1", "port": 502, "unit_id": 1 }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    assert_eq!(
        body["field_errors"][0]["field"], "settings.token",
        "{body:?}"
    );
    assert_eq!(body["field_errors"][0]["message"], "必須です", "{body:?}");

    let (status, body) = admin_write(
        &app.router,
        "POST",
        "/api/plc-connections/test",
        &app.admin_token,
        json!({
            "protocol": "secret-test",
            "simulation": false,
            "settings": { "host": "127.0.0.1", "port": 502, "unit_id": 1 }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], false, "{body:?}");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(message.contains("settings.token: 必須です"), "{message}");
}

// ---------------------------------------------------------------------------
// 8. 権限: viewer は 403、CSRF ヘッダ無しは拒否
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

// ---------------------------------------------------------------------------
// R2b-6b（docs/banto-hub-driver-ui-design.md §4.1）: `GET /api/drivers`。実ドライバの
// レジストリ（`common::hub_drivers()` = 組み込み + ホストの接続）で、機器のドライバだけが
// レジストリの順に載り、JSON の形（6c の UI がそのまま読む）が固定されていること。
// ---------------------------------------------------------------------------

/// `GET /api/drivers`。`token` が `None` なら Authorization を付けない。
async fn get_drivers(router: &Router, token: Option<&str>) -> (StatusCode, Value) {
    let mut request = HttpRequest::builder()
        .method("GET")
        .uri("/api/drivers")
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1);
    if let Some(token) = token {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    let response = router
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drivers_list_has_the_device_drivers_in_registry_order_and_no_host_connections() {
    let app = test_app("r2b6b-drivers").await;
    let (status, body) = get_drivers(&app.router, Some(&app.admin_token)).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let ids: Vec<&str> = body
        .as_array()
        .expect("an array")
        .iter()
        .map(|d| d["id"].as_str().unwrap())
        .collect();
    // The registry's order (`DriverRegistry::ids()`); `postgres`・`push`・`virtual`
    // (`HostProvided`) are registered by `hub_drivers()` but not listed.
    // SLMP first (2026-10-24 owner decision): `banto_drivers::builtin()` registers it first.
    assert_eq!(ids, ["slmp", "modbus-tcp"]);
    let registry_ids: Vec<String> = app
        .manager
        .registry()
        .ids()
        .map(|id| id.to_string())
        .collect();
    for host in ["postgres", "push", "virtual"] {
        assert!(
            registry_ids.iter().any(|id| id == host),
            "{host} should be in the registry for this test to mean anything: {registry_ids:?}"
        );
    }

    // The shape, fixed for the generic form (6c).
    let modbus = &body[1];
    let keys: Vec<&str> = modbus
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let mut keys_sorted = keys.clone();
    keys_sorted.sort_unstable();
    assert_eq!(
        keys_sorted,
        [
            "addressHelp",
            "capabilities",
            "displayName",
            "id",
            "kind",
            "settingsSchema",
            "supportedKinds",
            "transports",
            "vendor",
            "version",
        ]
    );
    assert_eq!(modbus["displayName"], json!("Modbus TCP"));
    assert_eq!(modbus["vendor"], Value::Null);
    assert!(!modbus["version"].as_str().unwrap().is_empty());
    assert_eq!(
        modbus["capabilities"],
        json!({ "read": true, "write": true })
    );
    assert_eq!(modbus["transports"], json!(["tcp"]));
    assert_eq!(modbus["kind"], json!("frame"));
    assert!(!modbus["addressHelp"].as_str().unwrap().is_empty());
    assert_eq!(
        modbus["supportedKinds"],
        json!(["bit", "i16", "u16", "i32", "u32", "f32", "i64", "u64", "f64"])
    );
    let fields = modbus["settingsSchema"]["fields"].as_array().unwrap();
    let field_keys: Vec<&str> = fields.iter().map(|f| f["key"].as_str().unwrap()).collect();
    assert_eq!(field_keys, ["host", "port", "unit_id", "word_order"]);
    assert_eq!(
        fields[0],
        json!({
            "key": "host", "label": "ホスト", "help": null, "kind": { "type": "host" },
            "required": true, "default": null, "group": "basic",
        })
    );
    assert_eq!(
        fields[1],
        json!({
            "key": "port", "label": "ポート", "help": null,
            "kind": { "type": "integer", "min": 1, "max": 65535 },
            "required": true, "default": 502, "group": "basic",
        })
    );
    assert_eq!(fields[3]["kind"]["type"], json!("choice"));
    assert_eq!(
        fields[3]["kind"]["options"][0],
        json!({ "value": "low_high", "label": "low_high（MELSEC標準）" })
    );
    assert_eq!(fields[3]["default"], json!("high_low"));
    assert_eq!(fields[3]["group"], json!("advanced"));

    let slmp = &body[0];
    assert_eq!(slmp["kind"], json!("frame"));
    assert_eq!(
        slmp["supportedKinds"],
        json!(["bit", "i16", "u16", "i32", "u32", "f32", "string"])
    );
    let slmp_keys: Vec<&str> = slmp["settingsSchema"]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["key"].as_str().unwrap())
        .collect();
    assert_eq!(slmp_keys, ["host", "port", "cpu_type", "word_order"]);
    assert_eq!(slmp["settingsSchema"]["fields"][1]["default"], json!(5007));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drivers_list_needs_a_session_and_a_viewer_may_read_it() {
    let app = test_app("r2b6b-drivers-auth").await;
    let (status, _) = get_drivers(&app.router, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = get_drivers(&app.router, Some("not-a-token")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    app.users
        .create_user("viewer-d", "password123", "閲覧者", Role::Viewer)
        .await
        .expect("create viewer user");
    let viewer_token = app
        .auth
        .login("viewer-d", "password123")
        .await
        .expect("viewer login");
    let (status, body) = get_drivers(&app.router, Some(&viewer_token)).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body.as_array().map(Vec::len), Some(2), "{body:?}");
}
