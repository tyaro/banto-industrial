//! T1 の統合テスト: 実サーバー（`banto_server::start` を ephemeral port
//! （`port: 0`）で起動）+ 実 WebSocket クライアント（`tokio-tungstenite`、
//! dev-dependency のみ - `apps/banto-hub/core/Cargo.toml` 参照）+ Modbus TCP
//! シミュレータ（`banto_plc::modbus::simulator`）による E2E。
//!
//! `tests/integration.rs`（T0-1/T0-2 の REST 統合テスト）は `tower::oneshot`
//! で `Router` を直接叩くが、WebSocket のアップグレードには実際の TCP
//! 接続 + HTTP Upgrade ハンドシェイクが要る（`oneshot` はこれを完走できない）
//! ため、このファイルだけ `banto_server::start` で実ポートを bind する -
//! `TempEnv`/`fast_options`/`wait_until` 等の足場は同ファイルから輸入した
//! （各 `tests/*.rs` は独立バイナリとしてコンパイルされ、ヘルパーは共有され
//! ないため複製している）。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::Router;
use banto_collect::{BackoffConfig, CollectorOptions};
use banto_hub_core::api_keys::ApiKeysService;
use banto_hub_core::audit::AuditLogService;
use banto_hub_core::db::init_db;
use banto_hub_core::hub::CollectorManager;
use banto_hub_core::rest::api_router;
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

type WsStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// 設計 §5.2 要件6の送信キュー容量（`stream.rs::OUTBOUND_QUEUE_CAPACITY`）と
/// 同じ値。テスト8（バックプレッシャ）で「キューが埋まりきる」量の目安に
/// 使う。
const OUTBOUND_QUEUE_CAPACITY: usize = 256;

struct TempEnv {
    root: PathBuf,
}

impl TempEnv {
    fn new(label: &str) -> Self {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "banto-hub-ws-it-{}-{label}-{id}",
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

fn conn_input(name: &str, port: u16) -> PlcConnectionInput {
    PlcConnectionInput {
        name: name.to_string(),
        protocol: "modbus-tcp".to_string(),
        host: "127.0.0.1".to_string(),
        port: port as i64,
        unit_id: 1,
        enabled: true,
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
    api_keys: ApiKeysService,
    _env: TempEnv,
}

impl TestApp {
    fn ws_url(&self, path: &str) -> String {
        format!("ws://127.0.0.1:{}{path}", self.server.local_addr().port())
    }
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
    let token = auth
        .login("admin", "password123")
        .await
        .expect("admin login");

    let sessions = std::sync::Arc::new(banto_hub_core::broker_glue::HubSessions::new(
        banto_broker::BackoffConfig::default(),
    ));
    let manager = std::sync::Arc::new(CollectorManager::new(
        pool.clone(),
        env.data_dir(),
        std::sync::Arc::new(SystemClock),
        fast_options(),
        sessions,
    ));
    manager.rebuild().await.expect("initial rebuild");

    let api_keys = ApiKeysService::new(pool.clone());
    let (events_tx, _rx) = broadcast::channel(16);
    let router: Router = api_router(
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
        .issue("writer-only", vec!["write:line1.fast.temp01".to_string()])
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

    assert!(
        wait_until(Duration::from_secs(3), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get("tag:1"))
                .is_some()
        })
        .await,
        "collector should be connected before we open the ws (so plc_connected doesn't race the subscribe below)"
    );

    let mut ws = connect_ws(&app.ws_url("/api/v1/stream"), Some(&app.token))
        .await
        .expect("ws handshake should succeed");

    sim.stop();

    let event = recv_matching(&mut ws, |m| {
        m["op"] == "event" && m["kind"] == "plc_disconnected"
    })
    .await;
    assert_eq!(event["connection"], "line1");
}

// ---------------------------------------------------------------------------
// 8. バックプレッシャ切断: 送信キューが満杯になったら切断される
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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
