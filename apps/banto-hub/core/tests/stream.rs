//! T1 の統合テスト: 実サーバー（`banto_server::start` を ephemeral port
//! （`port: 0`）で起動）+ 実 WebSocket クライアント（`tokio-tungstenite`、
//! dev-dependency のみ - `apps/banto-hub/core/Cargo.toml` 参照）+ Modbus TCP
//! シミュレータ（`banto_plc::modbus::simulator`）による E2E。
//!
//! `tests/integration.rs`（T0-1/T0-2 の REST 統合テスト）は `tower::oneshot`
//! で `Router` を直接叩くが、WebSocket のアップグレードには実際の TCP
//! 接続 + HTTP Upgrade ハンドシェイクが要る（`oneshot` はこれを完走できない）
//! ため、このファイルだけ `banto_server::start` で実ポートを bind する -
//! `fast_options`/`wait_until` 等の足場は同ファイルから輸入した（各
//! `tests/*.rs` は独立バイナリとしてコンパイルされ、ヘルパーは共有されない
//! ため複製している）。`TempEnv` は `tests/common/mod.rs` に集約済み
//! （2026-08-08、テスト一時ディレクトリリークの根治）。

use std::time::Duration;

use axum::Router;
use banto_collect::{BackoffConfig, CollectorOptions};
use banto_hub_core::api_keys::ApiKeysService;
use banto_hub_core::audit::AuditLogService;
use banto_hub_core::computed::{ComputedEngine, ServerTagStore};
use banto_hub_core::controller::{CollectionController, CollectionState, RunMode};
use banto_hub_core::db::init_db;
use banto_hub_core::hub::CollectorManager;
use banto_hub_core::rest::api_router_with_controller;
use banto_hub_core::users::UsersService;
use banto_plc::modbus::simulator::Simulator;
use banto_server::{start, AuthState, Identity, ServerConfig};
use banto_tags::{
    CollectionGroupInput, CollectionGroupService, PlcConnectionInput, PlcConnectionService,
    TagInput, TagService,
};
use banto_tstore::SystemClock;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use sqlx::SqlitePool;
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

mod common;
use common::TempEnv;

type WsStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// 設計 §5.2 要件6の送信キュー容量（`stream.rs::OUTBOUND_QUEUE_CAPACITY`）と
/// 同じ値。テスト8（バックプレッシャ）で「キューが埋まりきる」量の目安に
/// 使う。
const OUTBOUND_QUEUE_CAPACITY: usize = 256;

/// Temp-dir prefix passed to `TempEnv::new` (see `tests/common/mod.rs`).
const TEMP_ENV_PREFIX: &str = "banto-hub-ws-it";

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

/// 1テスト分の環境: 実サーバー（`RunningServer`、`server.stop()` で終了）+
/// ログイン済みトークン + `CollectorManager`（rebuild を直接叩くため）+
/// 生成物を握る `TempEnv`。`Simulator` はテスト側で個別に持つ（PLC アドレス
/// をテストごとに変えるため）。
struct TestApp {
    server: banto_server::RunningServer,
    token: String,
    pool: SqlitePool,
    manager: std::sync::Arc<CollectorManager>,
    controller: std::sync::Arc<CollectionController>,
    api_keys: ApiKeysService,
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

    let sessions = std::sync::Arc::new(banto_hub_core::broker_glue::HubSessions::new(
        banto_broker::BackoffConfig::default(),
    ));
    let sim_registry = std::sync::Arc::new(banto_hub_core::broker_glue::SlmpSimRegistry::new());
    let computed = std::sync::Arc::new(ComputedEngine::new(std::sync::Arc::new(
        ServerTagStore::new(),
    )));
    let manager = std::sync::Arc::new(CollectorManager::new(
        pool.clone(),
        env.data_dir(),
        std::sync::Arc::new(SystemClock),
        fast_options(),
        sessions,
        sim_registry,
        computed,
    ));
    manager.rebuild().await.expect("initial rebuild");
    let write_control =
        std::sync::Arc::new(banto_hub_core::write_control::WriteControl::new(false));
    let test_output = std::sync::Arc::new(banto_hub_core::test_output::TestOutputControl::new());
    let controller = std::sync::Arc::new(CollectionController::new(
        manager.clone(),
        write_control.clone(),
        test_output.clone(),
    ));
    let status = controller.start(RunMode::Configured).await;
    assert_eq!(status.state, CollectionState::Running);

    let api_keys = ApiKeysService::new(pool.clone());
    let (events_tx, _rx) = broadcast::channel(16);
    // T2-4: WriteControl always constructs disabled (docs/tag-server-design.md
    // §6-6) - not exercised by these WebSocket-subscription tests.
    let write_audit = banto_hub_core::write_audit::WriteAuditService::new(pool.clone());
    let mqtt = std::sync::Arc::new(banto_hub_core::mqtt::MqttPublisher::new(manager.clone()));
    // T4: this file exercises the WebSocket subscription surface only
    // (`tests/grpc.rs` covers gRPC's `StreamValues`) - `api_router`'s T4
    // arguments (the REST/gRPC-shared rate_limiter and `GrpcServer`) are
    // still required, so construct them without ever calling `apply`.
    let rate_limiter = std::sync::Arc::new(tokio::sync::Mutex::new(
        banto_hub_core::write_rate::WriteRateLimiter::new(
            banto_hub_core::write_rate::WriteRateLimitConfig::default(),
        ),
    ));
    let grpc_service = banto_hub_core::grpc::GrpcService::new(
        manager.clone(),
        api_keys.clone(),
        audit.clone(),
        write_audit.clone(),
        write_control.clone(),
        rate_limiter.clone(),
        events_tx.clone(),
    );
    let grpc_server = std::sync::Arc::new(banto_hub_core::grpc::GrpcServer::new(grpc_service));
    let router: Router = api_router_with_controller(
        users,
        audit,
        PlcConnectionService::new(pool.clone()),
        CollectionGroupService::new(pool.clone()),
        TagService::new(pool.clone()),
        api_keys.clone(),
        manager.clone(),
        controller.clone(),
        auth,
        events_tx,
        false,
        write_control,
        write_audit,
        mqtt,
        grpc_server,
        rate_limiter,
        test_output,
    );

    let server = start(
        ServerConfig {
            bind: "127.0.0.1".to_string(),
            port: 0,
        },
        router,
    )
    .await
    .expect("server should start");

    TestApp {
        server,
        token,
        pool,
        manager,
        controller,
        api_keys,
        _env: env,
    }
}

// --- WS クライアントヘルパー -------------------------------------------------

async fn connect_ws(
    url: &str,
    token: Option<&str>,
) -> Result<WsStream, tokio_tungstenite::tungstenite::Error> {
    let mut request = url.into_client_request().expect("valid ws url");
    if let Some(token) = token {
        request.headers_mut().insert(
            "Authorization",
            format!("Bearer {token}")
                .parse()
                .expect("valid header value"),
        );
    }
    let (stream, _response) = connect_async(request).await?;
    Ok(stream)
}

/// Like [`connect_ws`] but authenticates via `Sec-WebSocket-Protocol:
/// "bearer, {token}"` instead of `Authorization` - exercises
/// `rest.rs::extract_ws_protocol_token`, the fallback added so a browser
/// (which cannot set `Authorization` on a WS handshake) can authenticate
/// `GET /api/v1/stream`. No `Authorization` header is set at all, so a
/// success here proves the subprotocol path alone is sufficient.
async fn connect_ws_via_subprotocol(
    url: &str,
    token: &str,
) -> Result<WsStream, tokio_tungstenite::tungstenite::Error> {
    connect_ws_via_subprotocol_raw(url, &format!("bearer, {token}")).await
}

/// Like [`connect_ws_via_subprotocol`] but takes the raw
/// `Sec-WebSocket-Protocol` header value verbatim - used to send malformed
/// values (e.g. `"bearer"` with no token part) that a real client would
/// never construct via the `['bearer', token]` array form.
async fn connect_ws_via_subprotocol_raw(
    url: &str,
    protocol_header: &str,
) -> Result<WsStream, tokio_tungstenite::tungstenite::Error> {
    let mut request = url.into_client_request().expect("valid ws url");
    request.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        protocol_header.parse().expect("valid header value"),
    );
    let (stream, _response) = connect_async(request).await?;
    Ok(stream)
}

async fn send_json(ws: &mut WsStream, value: Value) {
    ws.send(WsMessage::Text(value.to_string().into()))
        .await
        .expect("ws send should succeed");
}

/// `predicate` を満たす次のテキストメッセージを受信するまで待つ（他の
/// op のメッセージ - `event`/`config_changed`/別 id の `data` 等 - は読み
/// 飛ばす）。5秒でタイムアウトしてテストを失敗させる。
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

/// ping を送り、その応答として最初に届く `data`（`id` 一致）または `pong`
/// を待つ。`data` が先に来たら即座にテスト失敗させる（テスト5「unsubscribe
/// 後は data が止まる」の中核アサーション）。
async fn assert_no_more_data_for(ws: &mut WsStream, id: i64) {
    send_json(ws, json!({ "op": "ping" })).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                Some(Ok(WsMessage::Text(text))) => {
                    let value: Value =
                        serde_json::from_str(&text).expect("server should send valid JSON");
                    if value["op"] == "data" && value["id"] == id {
                        panic!("unsubscribed id {id} still received data: {value}");
                    }
                    if value["op"] == "pong" {
                        return;
                    }
                    // event/config_changed/別 id の data はそのまま読み飛ばす。
                }
                Some(Ok(WsMessage::Ping(_))) | Some(Ok(WsMessage::Pong(_))) => continue,
                Some(Ok(other)) => panic!("unexpected non-text ws message: {other:?}"),
                Some(Err(err)) => panic!("ws error: {err}"),
                None => panic!("connection closed while waiting for pong"),
            }
        }
    })
    .await
    .expect("timed out waiting for pong");
}

// ---------------------------------------------------------------------------
// 1. subscribe(具体名) → 初期スナップショット → 値変更 → on_change data
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribe_exact_tag_gets_initial_snapshot_then_on_change_data() {
    let app = test_app("exact-on-change").await;
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 100); // 40001

    let conn = PlcConnectionService::new(app.pool.clone())
        .create(conn_input("line1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("temp01", group.id, "40001", "i16"))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild after seeding");

    assert!(
        wait_until(Duration::from_secs(3), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get("tag:1"))
                .map(|s| s.value)
                == Some(Some(100.0))
        })
        .await,
        "collector should observe the initial simulator value"
    );

    let mut ws = connect_ws(&app.ws_url("/api/v1/stream"), Some(&app.token))
        .await
        .expect("ws handshake should succeed");

    send_json(
        &mut ws,
        json!({ "op": "subscribe", "id": 1, "tags": ["line1.fast.temp01"], "mode": "on_change" }),
    )
    .await;

    let snapshot = recv_matching(&mut ws, |m| m["op"] == "data" && m["id"] == 1).await;
    let values = snapshot["values"].as_array().unwrap();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0]["tag"], "line1.fast.temp01");
    assert_eq!(values[0]["v"], 100.0);
    assert_eq!(values[0]["q"], "good");

    sim.set_holding_register(0, 200);

    let changed = recv_matching(&mut ws, |m| m["op"] == "data" && m["id"] == 1).await;
    let values = changed["values"].as_array().unwrap();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0]["v"], 200.0);

    sim.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_key_ws_ends_and_rejects_normal_output_during_all_simulation() {
    let app = test_app("api-key-all-simulation").await;
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 100);

    let conn = PlcConnectionService::new(app.pool.clone())
        .create(conn_input("line1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("temp01", group.id, "40001", "i16"))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild after seeding");
    assert!(
        wait_until(Duration::from_secs(3), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get("tag:1"))
                .map(|s| s.value)
                == Some(Some(100.0))
        })
        .await,
        "collector should observe the initial simulator value"
    );

    let issued = app
        .api_keys
        .issue("all-simulation-reader", vec!["read".to_string()], None)
        .await
        .expect("issue should succeed");
    let mut api_key_ws = connect_ws(&app.ws_url("/api/v1/stream"), Some(&issued.key))
        .await
        .expect("API-key WS handshake should succeed");
    send_json(
        &mut api_key_ws,
        json!({ "op": "subscribe", "id": 1, "tags": ["line1.fast.temp01"], "mode": "on_change" }),
    )
    .await;
    let snapshot = recv_matching(&mut api_key_ws, |m| m["op"] == "data" && m["id"] == 1).await;
    assert_eq!(snapshot["values"][0]["v"], 100.0);

    let mut session_ws = connect_ws(&app.ws_url("/api/v1/stream"), Some(&app.token))
        .await
        .expect("session WS handshake should succeed");
    send_json(
        &mut session_ws,
        json!({ "op": "subscribe", "id": 2, "tags": ["line1.fast.temp01"], "mode": "on_change" }),
    )
    .await;
    recv_matching(&mut session_ws, |m| m["op"] == "data" && m["id"] == 2).await;

    let status = app.controller.start(RunMode::AllSimulation).await;
    assert_eq!(status.state, CollectionState::Running);
    assert_eq!(status.mode, RunMode::AllSimulation);

    let closed = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match api_key_ws.next().await {
                None | Some(Err(_)) | Some(Ok(WsMessage::Close(_))) => break true,
                Some(Ok(_)) => {}
            }
        }
    })
    .await
    .expect("API-key WS should actively end during all-simulation");
    assert!(closed);

    let mut new_api_key_ws = connect_ws(&app.ws_url("/api/v1/stream"), Some(&issued.key))
        .await
        .expect("new API-key WS handshake should still succeed");
    send_json(
        &mut new_api_key_ws,
        json!({ "op": "subscribe", "id": 3, "tags": ["line1.fast.temp01"], "mode": "on_change" }),
    )
    .await;
    let error = recv_matching(&mut new_api_key_ws, |m| m["op"] == "error" && m["id"] == 3).await;
    assert_eq!(error["code"], "simulation_output_disabled");

    send_json(&mut session_ws, json!({ "op": "ping" })).await;
    let pong = recv_matching(&mut session_ws, |m| m["op"] == "pong").await;
    assert_eq!(pong["op"], "pong", "management session WS must remain open");

    sim.stop();
}

// ---------------------------------------------------------------------------
// 2. ワイルドカード `*` 購読 + タグ追加(CRUD) → config_changed → 新タグが
//    以後の data に現れる
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wildcard_subscription_picks_up_a_tag_added_after_config_changed() {
    let app = test_app("wildcard-config-changed").await;
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 1); // 40001
    sim.set_holding_register(1, 2); // 40002

    let conn = PlcConnectionService::new(app.pool.clone())
        .create(conn_input("line1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("a", group.id, "40001", "i16"))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild after seeding");

    assert!(
        wait_until(Duration::from_secs(3), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get("tag:1"))
                .is_some()
        })
        .await
    );

    let mut ws = connect_ws(&app.ws_url("/api/v1/stream"), Some(&app.token))
        .await
        .expect("ws handshake should succeed");

    send_json(
        &mut ws,
        json!({ "op": "subscribe", "id": 7, "tags": ["*"], "mode": "on_change" }),
    )
    .await;
    let snapshot = recv_matching(&mut ws, |m| m["op"] == "data" && m["id"] == 7).await;
    assert!(
        snapshot["values"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["tag"] == "line1.fast.a"),
        "initial snapshot should contain the pre-existing tag: {snapshot}"
    );

    let revision_before = app.manager.revision();
    let created = TagService::new(app.pool.clone())
        .create(tag_input("b", group.id, "40002", "i16"))
        .await
        .unwrap();
    app.manager
        .rebuild()
        .await
        .expect("rebuild after adding tag b");
    assert!(app.manager.revision() > revision_before);

    let config_changed = recv_matching(&mut ws, |m| m["op"] == "config_changed").await;
    assert!(config_changed["revision"].as_u64().unwrap() > revision_before);

    assert!(
        wait_until(Duration::from_secs(3), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get(&format!("tag:{}", created.id)))
                .is_some()
        })
        .await,
        "collector should observe tag b before we wait for it over WS"
    );

    let data_with_b = recv_matching(&mut ws, |m| {
        m["op"] == "data"
            && m["id"] == 7
            && m["values"]
                .as_array()
                .map(|values| values.iter().any(|v| v["tag"] == "line1.fast.b"))
                .unwrap_or(false)
    })
    .await;
    assert!(!data_with_b["values"].as_array().unwrap().is_empty());

    sim.stop();
}

// ---------------------------------------------------------------------------
// 3. interval モード: 指定間隔で data が届く(値不変でも)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interval_mode_sends_data_on_a_schedule_even_without_changes() {
    let app = test_app("interval").await;
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 42);

    let conn = PlcConnectionService::new(app.pool.clone())
        .create(conn_input("line1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("temp01", group.id, "40001", "i16"))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild after seeding");

    assert!(
        wait_until(Duration::from_secs(3), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get("tag:1"))
                .map(|s| s.value)
                == Some(Some(42.0))
        })
        .await
    );

    let mut ws = connect_ws(&app.ws_url("/api/v1/stream"), Some(&app.token))
        .await
        .expect("ws handshake should succeed");

    send_json(
        &mut ws,
        json!({ "op": "subscribe", "id": 3, "tags": ["line1.fast.temp01"], "mode": "interval", "interval_ms": 300 }),
    )
    .await;

    // Initial snapshot (design §5.2 要件5) always fires immediately.
    let first = recv_matching(&mut ws, |m| m["op"] == "data" && m["id"] == 3).await;
    assert_eq!(first["values"][0]["v"], 42.0);
    let t_first = first["t"].as_i64().unwrap();

    // Value never changes, but the next `data` must still arrive on its own
    // (design §5.2 要件3: interval は値不変でも送る) - not an on_change diff.
    let second = recv_matching(&mut ws, |m| m["op"] == "data" && m["id"] == 3).await;
    let t_second = second["t"].as_i64().unwrap();
    assert_eq!(second["values"][0]["v"], 42.0);
    assert!(
        t_second - t_first >= 250,
        "second interval tick should arrive roughly on schedule (>= EVAL_TICK_MS), got {}ms apart",
        t_second - t_first
    );

    sim.stop();
}

// ---------------------------------------------------------------------------
// 4. unknown_tag subscribe → error 応答、購読されない
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribing_an_unknown_tag_is_rejected_and_not_subscribed() {
    let app = test_app("unknown-tag").await;

    let mut ws = connect_ws(&app.ws_url("/api/v1/stream"), Some(&app.token))
        .await
        .expect("ws handshake should succeed");

    send_json(
        &mut ws,
        json!({ "op": "subscribe", "id": 9, "tags": ["nope.nope.nope"], "mode": "on_change" }),
    )
    .await;

    let error = recv_matching(&mut ws, |m| m["op"] == "error").await;
    assert_eq!(error["id"], 9);
    assert_eq!(error["code"], "unknown_tag");

    // Not subscribed: pinging must get pong next, never a `data` for id 9.
    assert_no_more_data_for(&mut ws, 9).await;
}

// ---------------------------------------------------------------------------
// 5. unsubscribe 後は data が止まる(ping/pong で生存確認)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsubscribe_stops_further_data() {
    let app = test_app("unsubscribe").await;
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 1);

    let conn = PlcConnectionService::new(app.pool.clone())
        .create(conn_input("line1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("temp01", group.id, "40001", "i16"))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild after seeding");

    assert!(
        wait_until(Duration::from_secs(3), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get("tag:1"))
                .is_some()
        })
        .await
    );

    let mut ws = connect_ws(&app.ws_url("/api/v1/stream"), Some(&app.token))
        .await
        .expect("ws handshake should succeed");

    send_json(
        &mut ws,
        json!({ "op": "subscribe", "id": 5, "tags": ["line1.fast.temp01"], "mode": "on_change" }),
    )
    .await;
    recv_matching(&mut ws, |m| m["op"] == "data" && m["id"] == 5).await; // initial snapshot

    // Prove data actually flows before we unsubscribe.
    sim.set_holding_register(0, 2);
    let changed = recv_matching(&mut ws, |m| m["op"] == "data" && m["id"] == 5).await;
    assert_eq!(changed["values"][0]["v"], 2.0);

    send_json(&mut ws, json!({ "op": "unsubscribe", "id": 5 })).await;

    // Change the value again - if the subscription were still live this
    // would produce another `data` message for id 5.
    sim.set_holding_register(0, 3);
    tokio::time::sleep(Duration::from_millis(400)).await; // give the eval loop a chance to (wrongly) fire
    assert_no_more_data_for(&mut ws, 5).await;

    sim.stop();
}

// ---------------------------------------------------------------------------
// 6. 認証なし接続 → 401。read スコープなし API キー → 403
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_requires_auth_and_read_scope() {
    let app = test_app("auth").await;

    let err = connect_ws(&app.ws_url("/api/v1/stream"), None)
        .await
        .expect_err("no Authorization header should be rejected");
    match err {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            assert_eq!(response.status().as_u16(), 401, "{response:?}");
        }
        other => panic!("expected an HTTP-level rejection, got {other:?}"),
    }

    let issued = app
        .api_keys
        .issue(
            "writer-only",
            vec!["write:line1.fast.temp01".to_string()],
            None,
        )
        .await
        .expect("issue should succeed");

    let err = connect_ws(&app.ws_url("/api/v1/stream"), Some(&issued.key))
        .await
        .expect_err("a write-only key should be rejected (no read scope)");
    match err {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            assert_eq!(response.status().as_u16(), 403, "{response:?}");
        }
        other => panic!("expected an HTTP-level rejection, got {other:?}"),
    }

    // Sanity: the same admin session token that every other test in this
    // file uses does succeed, so the two rejections above are really about
    // auth/scope and not some other handshake problem.
    let ok = connect_ws(&app.ws_url("/api/v1/stream"), Some(&app.token)).await;
    assert!(ok.is_ok(), "session token should be accepted: {ok:?}");
}

// ---------------------------------------------------------------------------
// 7. PLC 断(シミュレータ stop) → `op: "event"` の plc_disconnected
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plc_disconnect_is_relayed_as_an_event() {
    let app = test_app("event-relay").await;
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 1);

    let conn = PlcConnectionService::new(app.pool.clone())
        .create(conn_input("line1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("temp01", group.id, "40001", "i16"))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild after seeding");

    // H7 ⑤ follow-up (2026-08-09, CI flake on PR #96): the previous
    // pre-check only required `CurrentSample::is_some()`. That can pass on a
    // Bad/no-value bootstrap entry written before the first successful PLC
    // connect (`current.rs::set`). `PlcDisconnected` is emitted only on the
    // drop of a previously-connected session (`task.rs`), so stopping the
    // simulator before the first Good read leaves no event and
    // `recv_matching` times out. Mirror the sibling subscribe test / gRPC
    // `stream_events_relays_plc_disconnected`: wait for the seeded value.
    assert!(
        wait_until(Duration::from_secs(5), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get("tag:1"))
                .and_then(|s| s.value)
                == Some(1.0)
        })
        .await,
        "collector should observe the seeded simulator value before we open the ws (so plc_connected is established and plc_disconnected can fire)"
    );

    let mut ws = connect_ws(&app.ws_url("/api/v1/stream"), Some(&app.token))
        .await
        .expect("ws handshake should succeed");

    // Race (observed under CI load; H7 ⑤): `connect_ws` returning only
    // proves the *client* saw the HTTP 101 upgrade response - it says
    // nothing about whether the server's spawned `handle_socket` task
    // (src/stream.rs) has actually been polled far enough to reach
    // `manager.subscribe_events()`. That call happens unconditionally near
    // the top of `handle_socket`, strictly before its `tokio::select!` loop,
    // but on a busy/oversubscribed runner the task may simply not have been
    // scheduled yet by the time we get here. If `sim.stop()` ran first, the
    // collector's PLC read failure could fire `PlcDisconnected` on the
    // `broadcast` channel before this connection's `events.recv()` is even
    // listening; `broadcast` has no replay for late subscribers, so the
    // event would be lost and `recv_matching` below would block the full 5s
    // and panic (stream.rs:349).
    //
    // Fix: establish a real subscribe + await its initial snapshot first
    // (same round trip as
    // `subscribe_exact_tag_gets_initial_snapshot_then_on_change_data`
    // above). Receiving *any* reply for this subscription id is
    // proof-by-round-trip that `handle_socket` has already executed past
    // `manager.subscribe_events()` (it is called exactly once,
    // unconditionally, strictly before the loop that both handles the
    // incoming "subscribe" message and sends this reply - see
    // src/stream.rs), so only after this can `sim.stop()` run without
    // racing the event subscription. The pre-check above already proved a
    // Good seeded value, so the snapshot here is only used as the
    // subscribe-events liveness round trip (not as a value assertion).
    send_json(
        &mut ws,
        json!({ "op": "subscribe", "id": 1, "tags": ["line1.fast.temp01"], "mode": "on_change" }),
    )
    .await;
    recv_matching(&mut ws, |m| m["op"] == "data" && m["id"] == 1).await;

    sim.stop();

    // Slightly wider than the default 5s recv_matching budget: under a
    // loaded Windows CI runner the disconnect is still bounded by
    // `response_timeout` (500ms in `fast_options`), but draining competing
    // WS frames before the event can stretch wall time.
    let event = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match ws.next().await {
                Some(Ok(WsMessage::Text(text))) => {
                    let value: Value =
                        serde_json::from_str(&text).expect("server should send valid JSON");
                    if value["op"] == "event" && value["kind"] == "plc_disconnected" {
                        return value;
                    }
                }
                Some(Ok(WsMessage::Ping(_))) | Some(Ok(WsMessage::Pong(_))) => continue,
                Some(Ok(other)) => panic!("unexpected non-text ws message: {other:?}"),
                Some(Err(err)) => panic!("ws error while waiting for plc_disconnected: {err}"),
                None => panic!("connection closed while waiting for plc_disconnected"),
            }
        }
    })
    .await
    .expect("timed out waiting for plc_disconnected");
    assert_eq!(event["connection"], "line1");
}

// ---------------------------------------------------------------------------
// 8. バックプレッシャ切断: 送信キューが満杯になったら切断される
// ---------------------------------------------------------------------------

// H7フォローアップ（2026-08-09、フレーク根治）: このテストだけ意図的に
// デフォルトの current_thread（単一スレッド）ランタイムを使う -
// `multi_thread`/`worker_threads = 2` に**戻さないこと**（フレークが
// 再発する）。根本原因: 送信経路は `evaluate()`（`src/stream.rs` の同期関数、
// tick 分岐から呼ばれる）が `enqueue()` 経由で `data_tx.try_send()` を
// `.await` を挟まないタイトループで回すのに対し、`writer_task`
// (`src/stream.rs`) は別 `tokio::spawn` タスクとしてキューを drain する。
// worker_threads = 2 だとこの writer_task が2本目のワーカー上で
// **同時に** drain してしまい、`try_send` が `Full` を一度も観測できず
// バックプレッシャ切断（`enqueue`、code 1013）が発火しないままテストの
// 5秒待ちがタイムアウトする（= フレーク）。current_thread
// （ワーカースレッド1本）ならこのタスクを同時に動かす2本目のスレッドが
// 存在しないため、`evaluate()` の同期・await 無しファンアウトが完走する
// までの間 writer_task は一切 drain できない - よって
// `subscription_count`（`OUTBOUND_QUEUE_CAPACITY + 64` = 320）本の
// `try_send` は容量256のキューに対して257本目で確実にオーバーフローし、
// `enqueue` が確定的に close を発火する。
#[tokio::test]
async fn a_slow_subscriber_gets_disconnected_once_the_outbound_queue_fills() {
    let app = test_app("backpressure").await;
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 0);

    let conn = PlcConnectionService::new(app.pool.clone())
        .create(conn_input("line1", sim.addr.port()))
        .await
        .unwrap();
    // A short period so many on_change data messages can be produced quickly
    // by toggling the register value every eval tick.
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("temp01", group.id, "40001", "i16"))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild after seeding");

    assert!(
        wait_until(Duration::from_secs(3), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get("tag:1"))
                .is_some()
        })
        .await
    );

    let mut ws = connect_ws(&app.ws_url("/api/v1/stream"), Some(&app.token))
        .await
        .expect("ws handshake should succeed");

    // `on_change` produces at most 1 `data` message per subscription per
    // 250ms eval tick (design §5.2 要件2) - a single tag change can never
    // overflow a 256-capacity queue on its own. So instead we open `N` (>
    // `OUTBOUND_QUEUE_CAPACITY`) subscriptions on the same tag: one value
    // change then fans out to all of them inside a *single* `evaluate()`
    // call, which enqueues via non-blocking `try_send` in a tight loop with
    // no `.await` in between (`crate::stream::evaluate`). That makes the
    // overflow deterministic (bounded by subscription count, not by wall-clock
    // timing/network scheduling) rather than a flaky race against how fast
    // the writer task/OS socket happens to drain.
    let subscription_count: i64 = OUTBOUND_QUEUE_CAPACITY as i64 + 64;
    for id in 1..=subscription_count {
        send_json(
            &mut ws,
            json!({ "op": "subscribe", "id": id, "tags": ["line1.fast.temp01"], "mode": "on_change" }),
        )
        .await;
        recv_matching(&mut ws, move |m| m["op"] == "data" && m["id"] == id).await;
    }

    // Deliberately stop reading from here on (that's the point: a slow/stuck
    // subscriber) and flip the value once - `evaluate()`'s next tick tries
    // to enqueue `subscription_count` messages for a queue of capacity
    // `OUTBOUND_QUEUE_CAPACITY`, which must overflow.
    sim.set_holding_register(0, 999);

    // The server must close the connection - reading further eventually
    // yields a Close frame (or the stream simply ends).
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                Some(Ok(WsMessage::Close(_))) | None => return true,
                Some(Ok(_)) => continue,
                Some(Err(_)) => return true,
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(closed, "server should disconnect a slow subscriber");

    sim.stop();
}

// ---------------------------------------------------------------------------
// 9. `Sec-WebSocket-Protocol` bearer フォールバック（T10、banto-hub のライブ
//    タグモニタ向けにブラウザ WS 認証の欠落を埋めた変更 -
//    `rest.rs::extract_ws_protocol_token` の doc comment 参照）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn valid_session_token_via_subprotocol_header_authenticates_and_streams_data() {
    let app = test_app("ws-subprotocol-auth-ok").await;
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 42); // 40001

    let conn = PlcConnectionService::new(app.pool.clone())
        .create(conn_input("line1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("temp01", group.id, "40001", "i16"))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild after seeding");

    assert!(
        wait_until(Duration::from_secs(3), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get("tag:1"))
                .map(|s| s.value)
                == Some(Some(42.0))
        })
        .await,
        "collector should observe the initial simulator value"
    );

    // No `Authorization` header at all - only `Sec-WebSocket-Protocol:
    // "bearer, {token}"`, exactly what a browser `new WebSocket(url,
    // ['bearer', token])` call sends.
    let mut ws = connect_ws_via_subprotocol(&app.ws_url("/api/v1/stream"), &app.token)
        .await
        .expect("ws handshake via Sec-WebSocket-Protocol should succeed");

    send_json(
        &mut ws,
        json!({ "op": "subscribe", "id": 1, "tags": ["line1.fast.temp01"], "mode": "on_change" }),
    )
    .await;

    let snapshot = recv_matching(&mut ws, |m| m["op"] == "data" && m["id"] == 1).await;
    let values = snapshot["values"].as_array().unwrap();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0]["tag"], "line1.fast.temp01");
    assert_eq!(values[0]["v"], 42.0);
    assert_eq!(values[0]["q"], "good");

    sim.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_subprotocol_header_is_still_rejected_with_401() {
    let app = test_app("ws-subprotocol-auth-reject").await;

    // Just "bearer" with no second part.
    let err = connect_ws_via_subprotocol_raw(&app.ws_url("/api/v1/stream"), "bearer")
        .await
        .expect_err("a subprotocol header with no token part should be rejected");
    match err {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            assert_eq!(response.status().as_u16(), 401, "{response:?}");
        }
        other => panic!("expected an HTTP-level rejection, got {other:?}"),
    }

    // Completely unrelated subprotocols (no "bearer" first element).
    let err = connect_ws_via_subprotocol_raw(&app.ws_url("/api/v1/stream"), "chat, superchat")
        .await
        .expect_err("unrelated subprotocols should be rejected");
    match err {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            assert_eq!(response.status().as_u16(), 401, "{response:?}");
        }
        other => panic!("expected an HTTP-level rejection, got {other:?}"),
    }

    // No header at all, same as the existing no-Authorization test - sanity
    // check that this test's server setup rejects like every other test.
    let err = connect_ws(&app.ws_url("/api/v1/stream"), None)
        .await
        .expect_err("no auth at all should still be rejected");
    match err {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            assert_eq!(response.status().as_u16(), 401, "{response:?}");
        }
        other => panic!("expected an HTTP-level rejection, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 10. H10 ③(Option B、docs/h10-3-read-scope-proposal.md §5・§6): per-tag
//     read スコープは購読解決(`crate::subscribe_core::resolve`)の結果を
//     交差させる。`read:{tag}` キーはワイルドカード `*` 購読でもスコープ内
//     のタグしか受信できない。素の `read` キーは従来どおり全件受信する
//     (テスト6 `stream_requires_auth_and_read_scope` の拡張 - あちらは
//     「read スコープを一切持たないキーは 403」、ここは「read はあるが
//     タグ単位に絞られたキーの受信内容」)
// ---------------------------------------------------------------------------

/// テスト10共通のフィクスチャ: `line1.fast.temp01`(tag:1)・
/// `line2.slow.press01`(tag:2)を、同一シミュレータの別レジスタに割り当てて
/// 別接続・別グループで作る(1プロセスに複数シミュレータを立てなくても
/// host:port の重複は `plc_connections` に一意制約が無く許容されるため
/// 問題ない)。呼び出し元は続けて `app.manager.rebuild()` を呼ぶこと。
async fn seed_two_connections_two_tags(app: &TestApp, sim_port: u16) {
    let conn1 = PlcConnectionService::new(app.pool.clone())
        .create(conn_input("line1", sim_port))
        .await
        .unwrap();
    let group1 = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn1.id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("temp01", group1.id, "40001", "i16")) // tag:1
        .await
        .unwrap();

    let conn2 = PlcConnectionService::new(app.pool.clone())
        .create(conn_input("line2", sim_port))
        .await
        .unwrap();
    let group2 = CollectionGroupService::new(app.pool.clone())
        .create(group_input("slow", conn2.id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("press01", group2.id, "40002", "i16")) // tag:2
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wildcard_subscription_with_per_tag_read_scope_only_receives_in_scope_tag() {
    let app = test_app("per-tag-read-scope").await;
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 1); // line1.fast.temp01 (40001)
    sim.set_holding_register(1, 2); // line2.slow.press01 (40002)

    seed_two_connections_two_tags(&app, sim.addr.port()).await;
    app.manager.rebuild().await.expect("rebuild after seeding");

    assert!(
        wait_until(Duration::from_secs(3), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get("tag:1"))
                .map(|s| s.value)
                == Some(Some(1.0))
                && app
                    .manager
                    .current_values()
                    .and_then(|c| c.get("tag:2"))
                    .map(|s| s.value)
                    == Some(Some(2.0))
        })
        .await,
        "collector should observe both seeded tags"
    );

    let issued = app
        .api_keys
        .issue(
            "line1-temp01-reader",
            vec!["read:line1.fast.temp01".to_string()],
            None,
        )
        .await
        .expect("issue should succeed");

    let mut ws = connect_ws(&app.ws_url("/api/v1/stream"), Some(&issued.key))
        .await
        .expect("ws handshake should succeed");

    send_json(
        &mut ws,
        json!({ "op": "subscribe", "id": 1, "tags": ["*"], "mode": "on_change" }),
    )
    .await;

    let snapshot = recv_matching(&mut ws, |m| m["op"] == "data" && m["id"] == 1).await;
    let values = snapshot["values"].as_array().unwrap();
    assert_eq!(
        values.len(),
        1,
        "wildcard subscription must resolve only the in-scope tag: {values:?}"
    );
    assert_eq!(values[0]["tag"], "line1.fast.temp01");

    // スコープ外(line2.slow.press01)の値変更は届かない。
    sim.set_holding_register(1, 99);
    tokio::time::sleep(Duration::from_millis(400)).await; // give the eval loop a chance to (wrongly) fire
    assert_no_more_data_for(&mut ws, 1).await;

    // スコープ内(line1.fast.temp01)の値変更は引き続き届く。
    sim.set_holding_register(0, 42);
    let changed = recv_matching(&mut ws, |m| m["op"] == "data" && m["id"] == 1).await;
    let values = changed["values"].as_array().unwrap();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0]["tag"], "line1.fast.temp01");
    assert_eq!(values[0]["v"], 42.0);

    sim.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wildcard_subscription_with_bare_read_scope_receives_every_tag() {
    let app = test_app("bare-read-scope-stream").await;
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 1); // line1.fast.temp01 (40001)
    sim.set_holding_register(1, 2); // line2.slow.press01 (40002)

    seed_two_connections_two_tags(&app, sim.addr.port()).await;
    app.manager.rebuild().await.expect("rebuild after seeding");

    assert!(
        wait_until(Duration::from_secs(3), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get("tag:1"))
                .map(|s| s.value)
                == Some(Some(1.0))
                && app
                    .manager
                    .current_values()
                    .and_then(|c| c.get("tag:2"))
                    .map(|s| s.value)
                    == Some(Some(2.0))
        })
        .await,
        "collector should observe both seeded tags"
    );

    let issued = app
        .api_keys
        .issue("bare-reader", vec!["read".to_string()], None)
        .await
        .expect("issue should succeed");

    let mut ws = connect_ws(&app.ws_url("/api/v1/stream"), Some(&issued.key))
        .await
        .expect("ws handshake should succeed");

    send_json(
        &mut ws,
        json!({ "op": "subscribe", "id": 1, "tags": ["*"], "mode": "on_change" }),
    )
    .await;

    let snapshot = recv_matching(&mut ws, |m| m["op"] == "data" && m["id"] == 1).await;
    let mut tags: Vec<String> = snapshot["values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["tag"].as_str().unwrap().to_string())
        .collect();
    tags.sort();
    assert_eq!(
        tags,
        vec![
            "line1.fast.temp01".to_string(),
            "line2.slow.press01".to_string()
        ],
        "a bare `read` key must keep receiving every tag (S2 backward compat)"
    );

    sim.stop();
}
