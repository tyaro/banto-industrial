//! T6-2 の統合テスト（docs/tag-server-design.md §4.2/§4.3(a)）:
//! 演算タグ・内部タグの hub 配線を実際の axum `Router`
//! （`tower::ServiceExt::oneshot` / 実サーバーの WebSocket）+ Modbus TCP
//! シミュレータで通す E2E。`tests/integration.rs`/`tests/stream.rs`/
//! `tests/write.rs` と同じ理由で `fast_options`/`wait_until` 相当をこの
//! ファイル内に複製している（各 `tests/*.rs` は独立クレートとしてコン
//! パイルされ、private helper を共有できない）。`TempEnv` は
//! `tests/common/mod.rs` に集約済み（2026-08-08、テスト一時ディレクトリ
//! リークの根治）。
//!
//! `ComputedEngine::evaluate_tick` の250ms バックグラウンドループ自体は
//! `bin/banto-hub.rs`（本番プロセス）にしか配線されていない（設計どおり:
//! `crate::subscribe_core`の評価ループが各 transport のタスク内にあるのと
//! 同じ構造 - このモジュールはロジックのみ、起動はしない）。このテストは
//! それを模して [`drive_eval_tick`] を明示的に呼ぶ（`wait_until` の毎回の
//! ポーリングで1 tick 分呼ぶ - 250ms 固定間隔である必要はテスト上ない）。
//!
//! テスト構成（実装指示のテスト計画1〜3・6 に対応。4「文字列タグ参照の拒否」
//! は `banto_hub_core::computed` 自身の単体テストで既に確認済みなので
//! ここでは重複させない。5「WS/gRPC/MQTT」は WS のみ実サーバーで確認 -
//! gRPC/MQTT は `crate::subscribe_core`/`crate::hub::read_current` という
//! **全く同じ共通コード**を通る(このモジュールの目的そのもの)ので、既存の
//! gRPC/MQTT の PLC タグ向け E2E テストと合わせて読み取りパスの単一化を
//! 論証する - 個別に演算タグ版を複製しない判断):
//! 1. E2E: plc タグ + 演算タグ(平均) → 良好、PLC 断で bad
//! 2. 演算タグの連鎖(トポロジカル順)・循環登録の拒否(rebuild 失敗)
//! 3. 内部タグの書き込み→読み取り、computed への書き込みは403、retain
//!    true/false の再起動相当の復元/Bad
//! 4. WS で演算タグの値が流れる(代表1本)

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request as HttpRequest, StatusCode};
use axum::Router;
use banto_collect::{BackoffConfig, CollectorOptions, Quality};
use banto_hub_core::api_keys::ApiKeysService;
use banto_hub_core::audit::AuditLogService;
use banto_hub_core::computed::{load_retained_values, ComputedEngine, ServerTagStore};
use banto_hub_core::db::init_db;
use banto_hub_core::grpc::{GrpcServer, GrpcService};
use banto_hub_core::hub::CollectorManager;
use banto_hub_core::rest::api_router;
use banto_hub_core::users::UsersService;
use banto_hub_core::write_audit::WriteAuditService;
use banto_hub_core::write_control::WriteControl;
use banto_hub_core::write_rate::{WriteRateLimitConfig, WriteRateLimiter};
use banto_plc::modbus::simulator::Simulator;
use banto_server::{start, AuthState, Identity, ServerConfig};
use banto_tags::{
    CollectionGroupInput, CollectionGroupService, PlcConnectionInput, PlcConnectionService,
    TagInput, TagService, CALC_CONNECTION_NAME, MEM_CONNECTION_NAME, VIRTUAL_PROTOCOL,
};
use banto_tstore::SystemClock;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use sqlx::SqlitePool;
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use tower::ServiceExt;

mod common;
use common::TempEnv;

type WsStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

const CLIENT_HEADER: (&str, &str) = ("X-Banto-Client", "banto");

/// Temp-dir prefix passed to `TempEnv::new` (see `tests/common/mod.rs`).
const TEMP_ENV_PREFIX: &str = "banto-hub-computed-it";

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

fn modbus_conn_input(name: &str, port: u16) -> PlcConnectionInput {
    PlcConnectionInput {
        name: name.to_string(),
        protocol: "modbus-tcp".to_string(),
        host: "127.0.0.1".to_string(),
        port: port as i64,
        unit_id: 1,
        enabled: true,
        simulation: false,
    }
}

fn virtual_conn_input(name: &str) -> PlcConnectionInput {
    PlcConnectionInput {
        name: name.to_string(),
        protocol: VIRTUAL_PROTOCOL.to_string(),
        host: String::new(),
        port: 0,
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

fn plc_tag_input(name: &str, group_id: i64, address: &str, data_type: &str) -> TagInput {
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
        expected_revision: None,
    }
}

fn computed_tag_input(name: &str, group_id: i64, expression: &str) -> TagInput {
    TagInput {
        name: name.to_string(),
        collection_group_id: group_id,
        address: String::new(),
        data_type: "f32".to_string(),
        string_length: None,
        raw_lo: None,
        raw_hi: None,
        eng_lo: None,
        eng_hi: None,
        unit: None,
        decimals: 2,
        threshold_h: None,
        threshold_hh: None,
        threshold_l: None,
        threshold_ll: None,
        enabled: true,
        writable: false,
        tag_kind: "computed".to_string(),
        expression: Some(expression.to_string()),
        retain: false,
        expected_revision: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn internal_tag_input(
    name: &str,
    group_id: i64,
    data_type: &str,
    writable: bool,
    retain: bool,
) -> TagInput {
    TagInput {
        name: name.to_string(),
        collection_group_id: group_id,
        address: String::new(),
        data_type: data_type.to_string(),
        string_length: None,
        raw_lo: None,
        raw_hi: None,
        eng_lo: None,
        eng_hi: None,
        unit: None,
        decimals: 2,
        threshold_h: None,
        threshold_hh: None,
        threshold_l: None,
        threshold_ll: None,
        enabled: true,
        writable,
        tag_kind: "internal".to_string(),
        expression: None,
        retain,
        expected_revision: None,
    }
}

async fn wait_until<F, Fut>(timeout: Duration, mut predicate: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if predicate().await {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

struct TestApp {
    server: banto_server::RunningServer,
    router: Router,
    admin_token: String,
    pool: SqlitePool,
    manager: Arc<CollectorManager>,
    write_control: Arc<WriteControl>,
    _env: TempEnv,
}

// See `tests/common/mod.rs`'s module doc ("Why `TestApp` also needs
// `shutdown_test_app`") for why this is required, not optional.
impl Drop for TestApp {
    fn drop(&mut self) {
        common::shutdown_test_app(&self.manager, &self.pool);
    }
}

impl TestApp {
    fn ws_url(&self, path: &str) -> String {
        format!("ws://127.0.0.1:{}{path}", self.server.local_addr().port())
    }

    /// テスト計画の doc comment 参照: 250ms 評価ループ本番の代わりに1 tick
    /// 分だけ手動で回す。
    fn drive_eval_tick(&self) {
        let map = self.manager.tag_map();
        let current = self.manager.current_values();
        let now_ms = self.manager.clock().now_ms();
        self.manager
            .computed_engine()
            .evaluate_tick(&map, current.as_ref(), now_ms);
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

    let sessions = Arc::new(banto_hub_core::broker_glue::HubSessions::new(
        banto_broker::BackoffConfig::default(),
    ));
    let sim_registry = Arc::new(banto_hub_core::broker_glue::SlmpSimRegistry::new());
    let computed = Arc::new(ComputedEngine::new(Arc::new(ServerTagStore::new())));
    let manager = Arc::new(CollectorManager::new(
        pool.clone(),
        env.data_dir(),
        Arc::new(SystemClock),
        fast_options(),
        sessions,
        sim_registry,
        computed,
    ));

    // T6-2 (design §4.2/§4.3(a)): the real binary auto-provisions these at
    // startup (`bin/banto-hub.rs::ensure_virtual_connection`) - this test
    // harness does the same thing directly against the registry, since it
    // never runs that binary.
    for name in [CALC_CONNECTION_NAME, MEM_CONNECTION_NAME] {
        PlcConnectionService::new(pool.clone())
            .create(virtual_conn_input(name))
            .await
            .expect("virtual connection should be provisioned");
    }

    manager.rebuild().await.expect("initial rebuild");

    let api_keys = ApiKeysService::new(pool.clone());
    let (events_tx, _rx) = broadcast::channel(16);
    let write_control = Arc::new(WriteControl::new(false));
    let write_audit = WriteAuditService::new(pool.clone());
    let mqtt = Arc::new(banto_hub_core::mqtt::MqttPublisher::new(manager.clone()));
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
        users,
        audit,
        PlcConnectionService::new(pool.clone()),
        CollectionGroupService::new(pool.clone()),
        TagService::new(pool.clone()),
        api_keys.clone(),
        manager.clone(),
        auth,
        events_tx,
        false,
        write_control.clone(),
        write_audit,
        mqtt,
        grpc_server,
        rate_limiter,
    );

    let server = start(
        ServerConfig {
            bind: "127.0.0.1".to_string(),
            port: 0,
        },
        router.clone(),
    )
    .await
    .expect("server should start");

    TestApp {
        server,
        router,
        admin_token,
        pool,
        manager,
        write_control,
        _env: env,
    }
}

async fn get_json(router: &Router, path: &str, bearer: &str) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            HttpRequest::get(path)
                .header("Authorization", format!("Bearer {bearer}"))
                .body(Body::empty())
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

async fn admin_post(router: &Router, path: &str, token: &str, body: Value) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            HttpRequest::post(path)
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

async fn v1_post(router: &Router, path: &str, bearer: &str, body: Value) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            HttpRequest::post(path)
                .header("Authorization", format!("Bearer {bearer}"))
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

async fn issue_key(router: &Router, admin_token: &str, name: &str, scopes: &[&str]) -> String {
    let (status, body) = admin_post(
        router,
        "/api/api-keys",
        admin_token,
        json!({ "name": name, "scopes": scopes }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body:?}");
    body["key"].as_str().unwrap().to_string()
}

async fn connect_ws(url: &str, token: &str) -> WsStream {
    let mut request = url.into_client_request().expect("valid ws url");
    request.headers_mut().insert(
        "Authorization",
        format!("Bearer {token}").parse().expect("valid header"),
    );
    let (stream, _response) = connect_async(request).await.expect("ws connect");
    stream
}

async fn send_json(ws: &mut WsStream, value: Value) {
    ws.send(WsMessage::Text(value.to_string().into()))
        .await
        .expect("ws send should succeed");
}

async fn recv_matching(ws: &mut WsStream, predicate: impl Fn(&Value) -> bool) -> Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                Some(Ok(WsMessage::Text(text))) => {
                    let value: Value =
                        serde_json::from_str(&text).expect("server should send valid JSON");
                    if predicate(&value) {
                        return value;
                    }
                }
                Some(Ok(WsMessage::Ping(_))) | Some(Ok(WsMessage::Pong(_))) => continue,
                Some(Ok(other)) => panic!("unexpected non-text ws message: {other:?}"),
                Some(Err(err)) => panic!("ws error while waiting for a message: {err}"),
                None => panic!("connection closed while waiting for a message"),
            }
        }
    })
    .await
    .expect("timed out waiting for the expected ws message")
}

// ---------------------------------------------------------------------------
// 1. E2E: plc タグ + 演算タグ(平均) → 良好、PLC 断で bad
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_computed_avg_reflects_plc_inputs_then_goes_bad_on_disconnect() {
    let app = test_app("avg").await;
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 100); // 40001 -> a
    sim.set_holding_register(1, 200); // 40002 -> b

    let conn = PlcConnectionService::new(app.pool.clone())
        .create(modbus_conn_input("line1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    let tag_svc = TagService::new(app.pool.clone());
    tag_svc
        .create(plc_tag_input("a", group.id, "40001", "i16"))
        .await
        .unwrap();
    tag_svc
        .create(plc_tag_input("b", group.id, "40002", "i16"))
        .await
        .unwrap();

    // Look up the pre-provisioned "calc" connection by name via a fresh
    // registry read (the API only offers list/get by id, so filter here).
    let calc_id = PlcConnectionService::new(app.pool.clone())
        .list(banto_core::ListParams::default())
        .await
        .unwrap()
        .rows
        .into_iter()
        .find(|c| c.name == CALC_CONNECTION_NAME)
        .expect("calc connection should be auto-provisioned")
        .id;
    let calc_group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("x", calc_id, 1_000))
        .await
        .unwrap();
    tag_svc
        .create(computed_tag_input(
            "avg",
            calc_group.id,
            "(line1.fast.a + line1.fast.b) / 2",
        ))
        .await
        .unwrap();

    app.manager.rebuild().await.expect("rebuild after seeding");

    // Wait for the plc inputs to be collected AND the computed tag to be
    // evaluated to the expected average (150).
    assert!(
        wait_until(Duration::from_secs(5), || async {
            app.drive_eval_tick();
            let (status, body) =
                get_json(&app.router, "/api/v1/values/calc.x.avg", &app.admin_token).await;
            status == StatusCode::OK && body["v"].as_f64() == Some(150.0) && body["q"] == "good"
        })
        .await,
        "calc.x.avg should settle at (100+200)/2 = 150 with good quality"
    );

    // Now disconnect the PLC - both inputs go Bad, and the computed tag must
    // follow (design §4.2: 入力欠損は Bad).
    sim.stop();

    assert!(
        wait_until(Duration::from_secs(5), || async {
            app.drive_eval_tick();
            let (_status, body) =
                get_json(&app.router, "/api/v1/values/calc.x.avg", &app.admin_token).await;
            body["q"] == "bad"
        })
        .await,
        "calc.x.avg should go bad once its plc inputs disconnect"
    );
}

// ---------------------------------------------------------------------------
// 2. 演算タグの連鎖(トポロジカル順)・循環登録の拒否
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn computed_chain_is_evaluated_in_dependency_order() {
    let app = test_app("chain").await;
    let calc_id = PlcConnectionService::new(app.pool.clone())
        .list(banto_core::ListParams::default())
        .await
        .unwrap()
        .rows
        .into_iter()
        .find(|c| c.name == CALC_CONNECTION_NAME)
        .unwrap()
        .id;
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("x", calc_id, 1_000))
        .await
        .unwrap();
    let tag_svc = TagService::new(app.pool.clone());
    tag_svc
        .create(computed_tag_input("base", group.id, "21"))
        .await
        .unwrap();
    tag_svc
        .create(computed_tag_input("doubled", group.id, "calc.x.base * 2"))
        .await
        .unwrap();

    app.manager.rebuild().await.expect("rebuild should succeed");

    assert!(
        wait_until(Duration::from_secs(5), || async {
            app.drive_eval_tick();
            let (_status, body) = get_json(
                &app.router,
                "/api/v1/values/calc.x.doubled",
                &app.admin_token,
            )
            .await;
            body["v"].as_f64() == Some(42.0) && body["q"] == "good"
        })
        .await,
        "calc.x.doubled should evaluate to base(21) * 2 = 42 via the topological order"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cyclic_pair_of_computed_tags_fails_rebuild_and_keeps_the_old_state() {
    let app = test_app("cycle").await;
    let revision_before = app.manager.revision();

    let calc_id = PlcConnectionService::new(app.pool.clone())
        .list(banto_core::ListParams::default())
        .await
        .unwrap()
        .rows
        .into_iter()
        .find(|c| c.name == CALC_CONNECTION_NAME)
        .unwrap()
        .id;
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("x", calc_id, 1_000))
        .await
        .unwrap();
    let tag_svc = TagService::new(app.pool.clone());
    tag_svc
        .create(computed_tag_input("a", group.id, "calc.x.b + 1"))
        .await
        .unwrap();
    tag_svc
        .create(computed_tag_input("b", group.id, "calc.x.a + 1"))
        .await
        .unwrap();

    let err = app
        .manager
        .rebuild()
        .await
        .expect_err("a cyclic pair of computed tags must fail rebuild");
    assert!(err.contains("循環"), "error should mention 循環: {err}");

    // Old state (whatever revision existed before the cyclic tags were
    // registered) is untouched, and last_config_error carries the failure.
    assert_eq!(app.manager.revision(), revision_before);
    assert_eq!(app.manager.last_error(), Some(err));

    let (status, body) = get_json(&app.router, "/api/v1/status", &app.admin_token).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["last_config_error"].as_str().unwrap().contains("循環"));
}

// ---------------------------------------------------------------------------
// 3. 内部タグ: 書き込み→読み取り、computed への書き込みは403、retain
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn internal_tag_write_then_read_and_computed_write_is_forbidden() {
    let app = test_app("internal-write").await;
    app.write_control.enable();

    let mem_id = PlcConnectionService::new(app.pool.clone())
        .list(banto_core::ListParams::default())
        .await
        .unwrap()
        .rows
        .into_iter()
        .find(|c| c.name == MEM_CONNECTION_NAME)
        .unwrap()
        .id;
    let mem_group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("ui", mem_id, 1_000))
        .await
        .unwrap();
    let tag_svc = TagService::new(app.pool.clone());
    tag_svc
        .create(internal_tag_input(
            "setpoint1",
            mem_group.id,
            "f32",
            true,
            false,
        ))
        .await
        .unwrap();

    let calc_id = PlcConnectionService::new(app.pool.clone())
        .list(banto_core::ListParams::default())
        .await
        .unwrap()
        .rows
        .into_iter()
        .find(|c| c.name == CALC_CONNECTION_NAME)
        .unwrap()
        .id;
    let calc_group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("x", calc_id, 1_000))
        .await
        .unwrap();
    tag_svc
        .create(computed_tag_input("k", calc_group.id, "1"))
        .await
        .unwrap();

    app.manager.rebuild().await.expect("rebuild after seeding");

    let key = issue_key(
        &app.router,
        &app.admin_token,
        "writer",
        &["write:mem.ui.setpoint1", "write:calc.x.k"],
    )
    .await;

    // internal タグへの書き込みは成功し、values/{tag} で読める。
    let (status, body) = v1_post(
        &app.router,
        "/api/v1/values/mem.ui.setpoint1",
        &key,
        json!({ "v": 42.5 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");

    let (status, body) = get_json(
        &app.router,
        "/api/v1/values/mem.ui.setpoint1",
        &app.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["v"].as_f64(), Some(42.5));
    assert_eq!(body["q"], "good");

    // computed タグへの書き込みは常に403(値は式が決める、§4.2表)。
    let (status, body) = v1_post(
        &app.router,
        "/api/v1/values/calc.x.k",
        &key,
        json!({ "v": 99.0 }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body:?}");
    assert_eq!(body["error"], "not_writable");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retain_true_restores_the_value_across_a_restart_and_retain_false_starts_bad() {
    let app = test_app("retain").await;
    app.write_control.enable();

    let mem_id = PlcConnectionService::new(app.pool.clone())
        .list(banto_core::ListParams::default())
        .await
        .unwrap()
        .rows
        .into_iter()
        .find(|c| c.name == MEM_CONNECTION_NAME)
        .unwrap()
        .id;
    let mem_group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("ui", mem_id, 1_000))
        .await
        .unwrap();
    let tag_svc = TagService::new(app.pool.clone());
    tag_svc
        .create(internal_tag_input(
            "retained",
            mem_group.id,
            "f32",
            true,
            true,
        ))
        .await
        .unwrap();
    tag_svc
        .create(internal_tag_input(
            "volatile",
            mem_group.id,
            "f32",
            true,
            false,
        ))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild after seeding");

    let key = issue_key(
        &app.router,
        &app.admin_token,
        "writer",
        &["write:mem.ui.retained", "write:mem.ui.volatile"],
    )
    .await;

    for (name, value) in [("retained", 7.5), ("volatile", 3.0)] {
        let (status, body) = v1_post(
            &app.router,
            &format!("/api/v1/values/mem.ui.{name}"),
            &key,
            json!({ "v": value }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
    }

    // "restart": a brand new ServerTagStore/ComputedEngine/CollectorManager
    // against the SAME registry pool - exactly what a new process would see
    // (design §4.2 "起動時にロードして ServerTagStore を初期化").
    let fresh_server_store = Arc::new(ServerTagStore::new());
    for (tag_id, value, ptime_ms) in load_retained_values(&app.pool).await.unwrap() {
        fresh_server_store.set(
            &format!("tag:{tag_id}"),
            Some(value),
            Quality::Good,
            ptime_ms,
        );
    }
    let fresh_computed = Arc::new(ComputedEngine::new(fresh_server_store));
    let fresh_sessions = Arc::new(banto_hub_core::broker_glue::HubSessions::new(
        banto_broker::BackoffConfig::default(),
    ));
    let fresh_sim_registry = Arc::new(banto_hub_core::broker_glue::SlmpSimRegistry::new());
    let fresh_manager = Arc::new(CollectorManager::new(
        app.pool.clone(),
        app._env.data_dir(),
        Arc::new(SystemClock),
        fast_options(),
        fresh_sessions,
        fresh_sim_registry,
        fresh_computed,
    ));
    fresh_manager.rebuild().await.expect("post-restart rebuild");

    let now_ms = fresh_manager.clock().now_ms();
    let map = fresh_manager.tag_map();
    let server_store = fresh_manager.server_store();

    let retained_entry = map.get("mem.ui.retained").expect("retained tag in catalog");
    let (v, q, _t) = banto_hub_core::hub::read_current(
        retained_entry,
        fresh_manager.current_values().as_ref(),
        &server_store,
        now_ms,
    );
    assert_eq!(v, Some(7.5), "retain=true must survive the restart");
    assert_eq!(q, Quality::Good);

    let volatile_entry = map.get("mem.ui.volatile").expect("volatile tag in catalog");
    let (v, q, _t) = banto_hub_core::hub::read_current(
        volatile_entry,
        fresh_manager.current_values().as_ref(),
        &server_store,
        now_ms,
    );
    assert_eq!(v, None, "retain=false must start absent after a restart");
    assert_eq!(q, Quality::Bad);
}

// ---------------------------------------------------------------------------
// 4. WS で演算タグの値が流れる(代表1本)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ws_stream_carries_a_computed_tag_value() {
    let app = test_app("ws").await;
    let calc_id = PlcConnectionService::new(app.pool.clone())
        .list(banto_core::ListParams::default())
        .await
        .unwrap()
        .rows
        .into_iter()
        .find(|c| c.name == CALC_CONNECTION_NAME)
        .unwrap()
        .id;
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("x", calc_id, 1_000))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(computed_tag_input("k", group.id, "1 + 1"))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild after seeding");

    // Drive one eval tick before subscribing so the initial snapshot already
    // carries a computed value (WS's own 250ms loop would also pick it up on
    // its own, but this keeps the test deterministic and fast).
    app.drive_eval_tick();

    let mut ws = connect_ws(&app.ws_url("/api/v1/stream"), &app.admin_token).await;
    send_json(
        &mut ws,
        json!({ "op": "subscribe", "id": 1, "tags": ["calc.x.k"], "mode": "on_change" }),
    )
    .await;

    // Keep driving eval ticks in the background so the WS's own 250ms
    // evaluation loop (crate::stream, unchanged by this test) always has a
    // fresh ServerTagStore value to read via crate::hub::read_current.
    let ticker_manager = app.manager.clone();
    let ticker = tokio::spawn(async move {
        loop {
            let map = ticker_manager.tag_map();
            let current = ticker_manager.current_values();
            let now_ms = ticker_manager.clock().now_ms();
            ticker_manager
                .computed_engine()
                .evaluate_tick(&map, current.as_ref(), now_ms);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });

    let initial = recv_matching(&mut ws, |v| v["op"] == "data" && v["id"] == 1).await;
    let values = initial["values"].as_array().unwrap();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0]["tag"], "calc.x.k");
    assert_eq!(values[0]["v"].as_f64(), Some(2.0));
    assert_eq!(values[0]["q"], "good");

    ticker.abort();
}
