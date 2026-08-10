//! T7-2 の E2E テスト（docs/tag-server-design.md §4.3、実装指示のテスト計画
//! 1・2・3・5 に対応 - 4「不正変更の all-or-nothing 維持」は
//! `tests/integration.rs::an_invalid_config_keeps_the_old_collector_and_surfaces_last_config_error`
//! が新実装（`apply_config` 経由の rebuild）下でも通り続けることで確認済み
//! （このファイルに複製しない）。6「既存全テストを壊さない」はこのクレート・
//! `banto-collect`・`banto-broker`・`relay-wright-core` の既存スイートを流す
//! ことで確認する（このファイル自体はそれに寄与しない）。
//!
//! `tests/integration.rs`/`tests/computed.rs` と同じ理由で `fast_options`/
//! `wait_until`/`TestApp` 相当をこのファイル内に複製している（各
//! `tests/*.rs` は独立クレートとしてコンパイルされ、private helper を
//! 共有できない）。WS ヘルパ（`connect_ws`/`send_json`/`recv_matching`）は
//! `tests/computed.rs` と同型。`TempEnv` は `tests/common/mod.rs` に集約済み
//! （2026-08-08、テスト一時ディレクトリリークの根治）。
//!
//! テスト構成:
//! 1. 本命: 無関係接続の無停止 - シミュレータ2台(A: modbus, B: slmp)稼働中に
//!    B へタグ追加(REST CRUD) → A の PlcDisconnected が出ない・
//!    `/api/v1/values` で A が good 継続・revision 増加・WS の
//!    `config_changed` 受信・B の新タグが catalog/values に現れる・
//!    `last_apply` が B=replaced/A=unchanged を反映
//! 2. 演算タグのみの変更(§4.3(a)): calc タグ追加 → `last_apply` の
//!    added/removed/replaced が全空(Collector 無接触)・revision 増加・
//!    演算値が出る
//! 3. 接続削除: B 削除 → A 無停止・B の broker セッションが
//!    `HubSessions::connection_count` から消える・`/api/v1/status` から
//!    B が消える
//! 5. `/api/v1/status` の `last_apply` が実態を反映（起動直後の初回成功 →
//!    null、apply_config 実行 → 内容を反映、空構成への遷移 → null に戻る）

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
use banto_hub_core::users::UsersService;
use banto_hub_core::write_audit::WriteAuditService;
use banto_hub_core::write_control::WriteControl;
use banto_hub_core::write_rate::{WriteRateLimitConfig, WriteRateLimiter};
use banto_plc::modbus::simulator::Simulator;
use banto_plc::slmp::address::SlmpDevice;
use banto_plc::slmp::simulator::Simulator as SlmpSimulator;
use banto_server::{start, AuthState, Identity, ServerConfig};
use banto_tags::{
    CollectionGroupInput, CollectionGroupService, PlcConnectionInput, PlcConnectionService,
    TagInput, TagService, CALC_CONNECTION_NAME, VIRTUAL_PROTOCOL,
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
const TEMP_ENV_PREFIX: &str = "banto-hub-t7-2-it";

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

fn conn_input(name: &str, port: u16) -> PlcConnectionInput {
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

fn slmp_conn_input(name: &str, port: u16) -> PlcConnectionInput {
    PlcConnectionInput {
        protocol: "slmp".to_string(),
        ..conn_input(name, port)
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
        expected_revision: None,
    }
}

/// Poll `predicate` every 20ms until it returns true or `timeout` elapses.
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
    token: String,
    pool: SqlitePool,
    manager: Arc<CollectorManager>,
    /// T2-2/T7-2 (docs/tag-server-design.md §6-5/§4.3): exposed so a test can
    /// check broker-side session bookkeeping directly (e.g.
    /// `HubSessions::connection_count`) without going through REST.
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

impl TestApp {
    fn ws_url(&self, path: &str) -> String {
        format!("ws://127.0.0.1:{}{path}", self.server.local_addr().port())
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
    let token = auth
        .login("admin", "password123")
        .await
        .expect("admin login");

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
        users,
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
        token,
        pool,
        manager,
        sessions,
        _env: env,
    }
}

async fn get_json(router: &Router, path: &str, token: &str) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            HttpRequest::get(path)
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
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

/// `POST`/`PUT`/`DELETE` through the admin surface - needs both the bearer
/// token AND the `X-Banto-Client` CSRF header (unlike `/api/v1/*`).
async fn write_json(
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

fn str_array_contains(value: &Value, needle: &str) -> bool {
    value
        .as_array()
        .expect("expected a JSON array")
        .iter()
        .any(|v| v.as_str() == Some(needle))
}

// ---------------------------------------------------------------------------
// 1. 本命: 無関係接続の無停止 (docs/tag-server-design.md §4.3(c))
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unrelated_connection_is_uninterrupted_by_a_partial_reconfigure() {
    let app = test_app("unrelated-uninterrupted").await;

    // A: modbus, already collecting.
    let sim_a = Simulator::start().await;
    sim_a.set_holding_register(0, 1111); // 40001
    let conn_a = PlcConnectionService::new(app.pool.clone())
        .create(conn_input("line_a", sim_a.addr.port()))
        .await
        .unwrap();
    let group_a = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast_a", conn_a.id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("ta1", group_a.id, "40001", "i16"))
        .await
        .unwrap();

    // B: slmp, already collecting one tag - a second tag is added to its
    // SAME group via REST CRUD below (the change under test).
    let sim_b = SlmpSimulator::start().await;
    sim_b.set_word(SlmpDevice::D, 100, 2222);
    let conn_b = PlcConnectionService::new(app.pool.clone())
        .create(slmp_conn_input("line_b", sim_b.addr.port()))
        .await
        .unwrap();
    let group_b = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast_b", conn_b.id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("tb1", group_b.id, "D100", "u16"))
        .await
        .unwrap();

    app.manager.rebuild().await.expect("initial rebuild");

    assert!(
        wait_until(Duration::from_secs(3), || async {
            let (s, v) =
                get_json(&app.router, "/api/v1/values/line_a.fast_a.ta1", &app.token).await;
            s == StatusCode::OK && v["v"] == 1111.0 && v["q"] == "good"
        })
        .await,
        "A should be collecting before the change under test"
    );
    assert!(
        wait_until(Duration::from_secs(3), || async {
            let (s, v) =
                get_json(&app.router, "/api/v1/values/line_b.fast_b.tb1", &app.token).await;
            s == StatusCode::OK && v["v"] == 2222.0 && v["q"] == "good"
        })
        .await,
        "B should be collecting before the change under test"
    );

    // WS subscribed BEFORE the change, so the config_changed frame it
    // receives can only be caused by the tag creation below.
    let mut ws = connect_ws(&app.ws_url("/api/v1/stream"), &app.token).await;
    send_json(
        &mut ws,
        json!({ "op": "subscribe", "id": 1, "tags": ["line_a.fast_a.ta1"], "mode": "on_change" }),
    )
    .await;
    let _initial = recv_matching(&mut ws, |v| v["op"] == "data" && v["id"] == 1).await;

    let revision_before = app.manager.revision();
    // Live event subscription started right before the change - a broadcast
    // receiver only ever observes events sent AFTER it subscribes, so this
    // cannot miss anything the change under test causes, and there is no
    // historical backlog to drain first.
    let mut events_rx = app.manager.subscribe_events();

    // The change under test: B にタグ追加(REST CRUD).
    sim_b.set_word(SlmpDevice::D, 102, 3333);
    let (status, tag_json) = write_json(
        &app.router,
        "POST",
        "/api/tags",
        &app.token,
        json!({
            "name": "tb2",
            "collectionGroupId": group_b.id,
            "address": "D102",
            "dataType": "u16",
            "enabled": true,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{tag_json:?}");

    // revision 増加
    assert!(
        app.manager.revision() > revision_before,
        "revision should advance after the CRUD write"
    );

    // A の PlcDisconnected イベントが出ない - the POST handler already
    // awaited the triggered rebuild (and therefore `apply_config`) to
    // completion before responding, so every event it could have caused is
    // already in the channel by now; no extra wait/timeout needed.
    let conn_a_key = format!("conn:{}", conn_a.id);
    let mut a_disconnected = false;
    while let Ok(evt) = events_rx.try_recv() {
        if evt.kind == banto_collect::EventKind::PlcDisconnected
            && evt.connection_key.as_deref() == Some(conn_a_key.as_str())
        {
            a_disconnected = true;
        }
    }
    assert!(
        !a_disconnected,
        "connection A must not disconnect from an unrelated apply_config change"
    );

    // /api/v1/values で A が good 継続 - A の Collector タスク自体は
    // apply_config に触れられていない（`unchanged`）が、quality は読み取り時に
    // 「period(100ms) x STALE_PERIOD_FACTOR(2.5)」= 250ms 以内の更新有無で
    // 都度導出される（`banto_collect::current`）。CPU 負荷が高いランナーでは
    // 直前の書き込み処理と competing する他プロセスのせいで A の巡回タスク
    // 自体のスケジューリングがこの猶予を超えて遅れることがあり、一度も
    // 切断していなくても読み取りタイミング次第で一時的に stale に見えうる
    // （フル並列スイートで実際に観測: v["q"] == "stale"）。反応時間ではなく
    // A が最終的に good を保つことを確認したいので、bound-wait を挟む。
    assert!(
        wait_until(Duration::from_secs(8), || async {
            let (s, v) =
                get_json(&app.router, "/api/v1/values/line_a.fast_a.ta1", &app.token).await;
            s == StatusCode::OK && v["v"] == 1111.0 && v["q"] == "good"
        })
        .await,
        "A should remain good after the unrelated reconfigure, even under scheduling jitter"
    );
    let (status, val_a) =
        get_json(&app.router, "/api/v1/values/line_a.fast_a.ta1", &app.token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(val_a["v"], 1111.0);
    assert_eq!(val_a["q"], "good");

    // WS の config_changed 受信
    let changed = recv_matching(&mut ws, |v| v["op"] == "config_changed").await;
    assert!(changed["revision"].as_u64().unwrap() >= app.manager.revision());

    // B の新タグが catalog/values に現れる
    let (status, tags) = get_json(&app.router, "/api/v1/tags", &app.token).await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = tags["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["external_name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"line_b.fast_b.tb2"));
    assert!(
        wait_until(Duration::from_secs(3), || async {
            let (s, v) =
                get_json(&app.router, "/api/v1/values/line_b.fast_b.tb2", &app.token).await;
            s == StatusCode::OK && v["v"] == 3333.0 && v["q"] == "good"
        })
        .await,
        "B's newly added tag should read through the fresh (replaced) task"
    );

    // last_apply が実態を反映: B (既存接続にタグ追加→plan 変更) は
    // replaced、A は無変更のまま unchanged - added/removed は空。
    let (status, status_json) = get_json(&app.router, "/api/v1/status", &app.token).await;
    assert_eq!(status, StatusCode::OK);
    let last_apply = &status_json["last_apply"];
    assert!(!last_apply.is_null(), "last_apply should be present");
    assert_eq!(last_apply["added"].as_array().unwrap().len(), 0);
    assert_eq!(last_apply["removed"].as_array().unwrap().len(), 0);
    assert!(
        str_array_contains(&last_apply["replaced"], &format!("conn:{}", conn_b.id)),
        "B should be classified as replaced: {last_apply:?}"
    );
    assert!(
        str_array_contains(&last_apply["unchanged"], &conn_a_key),
        "A should be classified as unchanged: {last_apply:?}"
    );

    let _ = ws.close(None).await;
    sim_a.stop();
    sim_b.stop();
}

// ---------------------------------------------------------------------------
// 2. 演算タグのみの変更 (docs/tag-server-design.md §4.3(a))
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_side_tag_only_change_never_touches_the_collector() {
    let app = test_app("server-side-only").await;

    let sim = Simulator::start().await;
    sim.set_holding_register(0, 55);
    let conn = PlcConnectionService::new(app.pool.clone())
        .create(conn_input("line1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("t1", group.id, "40001", "i16"))
        .await
        .unwrap();

    let calc_conn = PlcConnectionService::new(app.pool.clone())
        .create(virtual_conn_input(CALC_CONNECTION_NAME))
        .await
        .unwrap();
    let calc_group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("x", calc_conn.id, 1_000))
        .await
        .unwrap();

    app.manager.rebuild().await.expect("initial rebuild");
    assert!(
        wait_until(Duration::from_secs(3), || async {
            let (s, v) = get_json(&app.router, "/api/v1/values/line1.fast.t1", &app.token).await;
            s == StatusCode::OK && v["q"] == "good"
        })
        .await,
        "the real PLC connection should be collecting before the change under test"
    );

    let revision_before = app.manager.revision();

    // 演算タグ追加(REST CRUD) - a pure constant expression is enough to
    // prove the server-only path; it does not need to reference the PLC
    // tag above.
    let (status, tag_json) = write_json(
        &app.router,
        "POST",
        "/api/tags",
        &app.token,
        json!({
            "name": "k",
            "collectionGroupId": calc_group.id,
            "address": "",
            "dataType": "f32",
            "enabled": true,
            "tagKind": "computed",
            "expression": "1 + 1",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{tag_json:?}");

    // revision 増加
    assert!(app.manager.revision() > revision_before);

    // ApplyReport (`last_apply`) の added/removed/replaced が全空
    // (Collector 無接触 - `line1` は `unchanged` のまま、`build_config`/
    // `store_config` は元々 virtual 接続を除外するので、その config は
    // 演算タグの追加前後で1バイトも変わらない)。
    let (status, status_json) = get_json(&app.router, "/api/v1/status", &app.token).await;
    assert_eq!(status, StatusCode::OK);
    let last_apply = &status_json["last_apply"];
    assert!(!last_apply.is_null(), "last_apply should be present");
    assert_eq!(
        last_apply["added"].as_array().unwrap().len(),
        0,
        "{last_apply:?}"
    );
    assert_eq!(
        last_apply["removed"].as_array().unwrap().len(),
        0,
        "{last_apply:?}"
    );
    assert_eq!(
        last_apply["replaced"].as_array().unwrap().len(),
        0,
        "{last_apply:?}"
    );
    assert_eq!(
        last_apply["writer_rotated"], false,
        "a server-side-only tag never touches the tstore schema: {last_apply:?}"
    );
    assert!(
        str_array_contains(&last_apply["unchanged"], &format!("conn:{}", conn.id)),
        "the real PLC connection should be listed unchanged, never touched: {last_apply:?}"
    );

    // 演算値が出る - drive one eval tick (the 250ms background loop only
    // runs in `bin/banto-hub.rs`, not this test harness - mirrors
    // `tests/computed.rs::TestApp::drive_eval_tick`).
    let map = app.manager.tag_map();
    let current = app.manager.current_values();
    let now_ms = app.manager.clock().now_ms();
    app.manager
        .computed_engine()
        .evaluate_tick(&map, current.as_ref(), now_ms);

    let (status, val) = get_json(&app.router, "/api/v1/values/calc.x.k", &app.token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(val["v"], 2.0);
    assert_eq!(val["q"], "good");

    sim.stop();
}

// ---------------------------------------------------------------------------
// 3. 接続削除: 無関係無停止 + broker セッション整理
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_a_connection_untracks_its_broker_session_and_leaves_others_running() {
    let app = test_app("delete-connection").await;

    let sim_a = Simulator::start().await;
    sim_a.set_holding_register(0, 77);
    let conn_a = PlcConnectionService::new(app.pool.clone())
        .create(conn_input("line_a", sim_a.addr.port()))
        .await
        .unwrap();
    let group_a = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast_a", conn_a.id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("ta1", group_a.id, "40001", "i16"))
        .await
        .unwrap();

    let sim_b = SlmpSimulator::start().await;
    sim_b.set_word(SlmpDevice::D, 100, 999);
    let conn_b = PlcConnectionService::new(app.pool.clone())
        .create(slmp_conn_input("line_b", sim_b.addr.port()))
        .await
        .unwrap();
    let group_b = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast_b", conn_b.id, 100))
        .await
        .unwrap();
    let tag_b = TagService::new(app.pool.clone())
        .create(tag_input("tb1", group_b.id, "D100", "u16"))
        .await
        .unwrap();

    app.manager.rebuild().await.expect("initial rebuild");

    assert!(
        wait_until(Duration::from_secs(3), || async {
            let (s, v) =
                get_json(&app.router, "/api/v1/values/line_a.fast_a.ta1", &app.token).await;
            s == StatusCode::OK && v["q"] == "good"
        })
        .await
    );
    assert!(
        wait_until(Duration::from_secs(3), || async {
            let (s, v) =
                get_json(&app.router, "/api/v1/values/line_b.fast_b.tb1", &app.token).await;
            s == StatusCode::OK && v["q"] == "good"
        })
        .await
    );
    assert_eq!(
        app.sessions.connection_count(),
        1,
        "exactly one broker session should exist, for B"
    );

    // B 削除: FK 制約(ON DELETE RESTRICT)があるので tag → group → connection
    // の順で消す。
    let (status, _) = write_json(
        &app.router,
        "DELETE",
        &format!("/api/tags/{}", tag_b.id),
        &app.token,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = write_json(
        &app.router,
        "DELETE",
        &format!("/api/collection-groups/{}", group_b.id),
        &app.token,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // The group deletion above is the point where B actually drops out of
    // `build_config`'s output (a connection with zero collectible groups is
    // dropped from `CollectorConfig`, per `banto_collect::build_config`'s own
    // doc comment) - so THIS rebuild's `last_apply` is the one that shows B
    // as `removed`, not the connection-row deletion below (which changes
    // nothing further in the collect config, since B was already gone from
    // it here).
    let (status, status_json) = get_json(&app.router, "/api/v1/status", &app.token).await;
    assert_eq!(status, StatusCode::OK);
    let last_apply = &status_json["last_apply"];
    assert!(
        str_array_contains(&last_apply["removed"], &format!("conn:{}", conn_b.id)),
        "B should be classified as removed once its last collectible group is gone: {last_apply:?}"
    );
    assert!(
        str_array_contains(&last_apply["unchanged"], &format!("conn:{}", conn_a.id)),
        "A should remain unchanged throughout: {last_apply:?}"
    );

    let (status, _) = write_json(
        &app.router,
        "DELETE",
        &format!("/api/plc-connections/{}", conn_b.id),
        &app.token,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // A 無停止: 値が引き続き読める - 上の
    // unrelated_connection_is_uninterrupted_by_a_partial_reconfigure と同じ
    // 理由（quality は読み取り時に period x STALE_PERIOD_FACTOR の猶予で
    // 都度導出されるため、負荷の高いランナーでは A のタスク自体は無停止でも
    // 巡回が猶予を超えて遅れ一時的に stale と見えうる）で bound-wait する。
    assert!(
        wait_until(Duration::from_secs(8), || async {
            let (s, v) =
                get_json(&app.router, "/api/v1/values/line_a.fast_a.ta1", &app.token).await;
            s == StatusCode::OK && v["q"] == "good"
        })
        .await,
        "A should remain good after B's connection is deleted, even under scheduling jitter"
    );
    let (status, val_a) =
        get_json(&app.router, "/api/v1/values/line_a.fast_a.ta1", &app.token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(val_a["q"], "good");

    // B の broker セッションが整理される (HubSessions のセッション数で確認) -
    // this is keyed off the registry's connection list, not the collect
    // config, so it only happens once the connection ROW itself (not just
    // its groups/tags) is gone.
    assert!(
        wait_until(Duration::from_secs(3), || async {
            app.sessions.connection_count() == 0
        })
        .await,
        "B's broker session should be untracked after the connection is deleted"
    );

    // status から B が消える。
    let (status, status_json) = get_json(&app.router, "/api/v1/status", &app.token).await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<i64> = status_json["connections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_i64().unwrap())
        .collect();
    assert!(!ids.contains(&conn_b.id));
    assert!(ids.contains(&conn_a.id));

    sim_a.stop();
    sim_b.stop();
}

// ---------------------------------------------------------------------------
// 5. /api/v1/status の last_apply が実態を反映
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_last_apply_reflects_fresh_start_apply_and_stop() {
    let app = test_app("last-apply-status").await;

    // 起動直後の初回成功(この時点では空構成 - test_app 内の initial rebuild)
    // - apply_config を経由していないので last_apply は null。
    let (status, status_json) = get_json(&app.router, "/api/v1/status", &app.token).await;
    assert_eq!(status, StatusCode::OK);
    assert!(status_json["last_apply"].is_null());

    // A を登録して rebuild - Collector がまだ無いので
    // start_with_client_factory(新規起動)経由 - これも apply_config を
    // 経由しないので last_apply はまだ null のまま。
    let sim_a = Simulator::start().await;
    sim_a.set_holding_register(0, 1);
    let conn_a = PlcConnectionService::new(app.pool.clone())
        .create(conn_input("line_a", sim_a.addr.port()))
        .await
        .unwrap();
    let group_a = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast_a", conn_a.id, 100))
        .await
        .unwrap();
    let tag_a = TagService::new(app.pool.clone())
        .create(tag_input("ta1", group_a.id, "40001", "i16"))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("fresh start rebuild");

    let (status, status_json) = get_json(&app.router, "/api/v1/status", &app.token).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        status_json["last_apply"].is_null(),
        "a fresh Collector start never goes through apply_config: {status_json:?}"
    );

    // B を登録して rebuild - 今度は Collector が既に生きているので
    // apply_config 経由 - B が added として現れる。
    let sim_b = Simulator::start().await;
    sim_b.set_holding_register(0, 2);
    let conn_b = PlcConnectionService::new(app.pool.clone())
        .create(conn_input("line_b", sim_b.addr.port()))
        .await
        .unwrap();
    let group_b = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast_b", conn_b.id, 100))
        .await
        .unwrap();
    let tag_b = TagService::new(app.pool.clone())
        .create(tag_input("tb1", group_b.id, "40001", "i16"))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("apply_config rebuild");

    let (status, status_json) = get_json(&app.router, "/api/v1/status", &app.token).await;
    assert_eq!(status, StatusCode::OK);
    let last_apply = &status_json["last_apply"];
    assert!(!last_apply.is_null());
    assert!(str_array_contains(
        &last_apply["added"],
        &format!("conn:{}", conn_b.id)
    ));

    // B を削除して rebuild - apply_config 経由で B が removed として現れる。
    TagService::new(app.pool.clone())
        .delete(tag_b.id)
        .await
        .unwrap();
    CollectionGroupService::new(app.pool.clone())
        .delete(group_b.id)
        .await
        .unwrap();
    PlcConnectionService::new(app.pool.clone())
        .delete(conn_b.id)
        .await
        .unwrap();
    app.manager.rebuild().await.expect("removal rebuild");

    let (status, status_json) = get_json(&app.router, "/api/v1/status", &app.token).await;
    assert_eq!(status, StatusCode::OK);
    let last_apply = &status_json["last_apply"];
    assert!(!last_apply.is_null());
    assert!(str_array_contains(
        &last_apply["removed"],
        &format!("conn:{}", conn_b.id)
    ));

    // A も削除して空構成に遷移 - 「空構成への遷移」分岐は apply_config を
    // 経由しないので last_apply は null に戻る。
    TagService::new(app.pool.clone())
        .delete(tag_a.id)
        .await
        .unwrap();
    CollectionGroupService::new(app.pool.clone())
        .delete(group_a.id)
        .await
        .unwrap();
    PlcConnectionService::new(app.pool.clone())
        .delete(conn_a.id)
        .await
        .unwrap();
    app.manager.rebuild().await.expect("stop rebuild");

    let (status, status_json) = get_json(&app.router, "/api/v1/status", &app.token).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        status_json["last_apply"].is_null(),
        "transitioning to the empty state must clear last_apply: {status_json:?}"
    );

    sim_a.stop();
    sim_b.stop();
}
