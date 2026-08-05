//! T3 の統合テスト（docs/tag-server-design.md §5.3）: 実際の axum `Router`
//! と Modbus TCP シミュレータ（`tests/integration.rs`と同じ）、そして
//! **in-process MQTT ブローカー（`rumqttd`）**を使った E2E。`tests/write.rs`/
//! `tests/stream.rs`と同じ理由で `TempEnv`/`fast_options`/`wait_until`等を
//! このファイル内に複製している（各 `tests/*.rs` は独立クレートとして
//! コンパイルされ、private helper を共有できない）。
//!
//! テスト構成（実装指示のテスト計画1〜5に対応。6「既存全テストを壊さない」
//! は `cargo test -p banto-hub-core` 全体で確認する - このファイル単体の
//! 責務ではない）:
//! 1. E2E ハッピーパス（`$state` の online retain も同じ購読で拾う）
//! 2. retain: 値発行後の新規購読が即座に最終値を受信
//! 3. （1に同居）$state の online retain
//! 4. スロットル: min_interval_ms 内の連続変化が抑止され、明けに最新値が届く
//! 5. enabled=false では何も発行されない・PUT で有効化すると発行開始
//!
//! ## rumqttd の起動安定化（実装指示「ポート 0 相当の空きポート取得等」）
//!
//! [`free_port`]で一度 `TcpListener`を bind→即 drop して空きポート番号を
//! 取得し、その番号で`rumqttd::Broker`を起動する（`bind(":0")`を直接
//! 渡せる API が無いための回避策 - `rumqttd::Config`はポート番号そのものを
//! 要求する）。取得直後に別プロセスがそのポートを奪う理論上の競合や、
//! ブローカーの起動が遅れるケースに備えて、[`start_test_broker`]は起動後に
//! 実際に TCP 接続できるかを probe し、失敗したら新しいポートで最大3回
//! 再試行する。

use std::collections::HashMap;
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
use banto_hub_core::broker_glue::HubSessions;
use banto_hub_core::db::init_db;
use banto_hub_core::grpc::{GrpcServer, GrpcService};
use banto_hub_core::hub::CollectorManager;
use banto_hub_core::mqtt::MqttPublisher;
use banto_hub_core::rest::api_router;
use banto_hub_core::users::UsersService;
use banto_hub_core::write_audit::WriteAuditService;
use banto_hub_core::write_control::WriteControl;
use banto_hub_core::write_rate::{WriteRateLimitConfig, WriteRateLimiter};
use banto_plc::modbus::simulator::Simulator;
use banto_server::{AuthState, Identity};
use banto_tags::{
    CollectionGroupInput, CollectionGroupService, PlcConnectionInput, PlcConnectionService,
    TagInput, TagService,
};
use banto_tstore::SystemClock;
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS};
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
            "banto-hub-mqtt-it-{}-{label}-{id}",
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

/// Poll `predicate` every 20ms until it returns true or `timeout` elapses -
/// `tests/integration.rs`等と同じパターン。
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

// --- in-process MQTT ブローカー(rumqttd) -----------------------------------

/// このモジュールの doc comment「rumqttd の起動安定化」参照 - `bind(":0")`
/// して即 `drop`し、空いていたポート番号だけを取り出す。
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a free port");
    listener.local_addr().expect("local_addr").port()
}

fn rumqttd_config(port: u16) -> rumqttd::Config {
    let router = rumqttd::RouterConfig {
        max_connections: 100,
        max_outgoing_packet_count: 200,
        max_segment_size: 1024 * 1024,
        max_segment_count: 10,
        ..Default::default()
    };
    let mut v4 = HashMap::new();
    v4.insert(
        "1".to_string(),
        rumqttd::ServerSettings {
            name: "v4-1".to_string(),
            listen: format!("127.0.0.1:{port}")
                .parse()
                .expect("valid listen addr"),
            tls: None,
            next_connection_delay_ms: 1,
            connections: rumqttd::ConnectionSettings {
                connection_timeout_ms: 5000,
                max_payload_size: 20480,
                max_inflight_count: 100,
                auth: None,
                external_auth: None,
                dynamic_filters: true,
            },
        },
    );
    rumqttd::Config {
        id: 0,
        router,
        v4: Some(v4),
        v5: None,
        ws: None,
        cluster: None,
        console: None,
        bridge: None,
        prometheus: None,
        metrics: None,
    }
}

/// `rumqttd::Broker::start`は同期・ブロッキング関数で内部に自前の tokio
/// ランタイムを持つ（`rumqttd`本体の`main.rs`が`#[tokio::main]`を被せず
/// 素の`fn main`から直接呼んでいるのと同じ形 - このテストクレートの調査で
/// 確認済み）。テストの非同期ランタイムをブロックしないよう、素の
/// `std::thread::spawn`に載せる（プロセス終了まで生存させっぱなしでよい -
/// テストプロセスごとに使い捨て）。
///
/// このモジュールの doc comment「rumqttd の起動安定化」参照 - 起動後に実際
/// に TCP 接続できるまで probe し、失敗したら新しいポートで最大3回まで
/// 再試行する。
async fn start_test_broker() -> u16 {
    for attempt in 1..=3 {
        let port = free_port();
        let mut broker = rumqttd::Broker::new(rumqttd_config(port));
        std::thread::spawn(move || {
            let _ = broker.start();
        });

        let up = wait_until(Duration::from_secs(4), || async move {
            tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_ok()
        })
        .await;
        if up {
            return port;
        }
        eprintln!(
            "banto-hub mqtt test: rumqttd がポート {port} で起動確認できません(試行 {attempt}/3) - 新しいポートで再試行します"
        );
    }
    panic!("rumqttd が3回の試行後も起動しませんでした");
}

// --- MQTT 購読テストクライアント(rumqttc) -----------------------------------

/// `filter`を購読し、`min_count`件受信するか`timeout`が尽きるまで
/// `(topic, payload)`を集める。
async fn collect_messages(
    port: u16,
    client_id: &str,
    filter: &str,
    min_count: usize,
    timeout: Duration,
) -> Vec<(String, String)> {
    let mut options = MqttOptions::new(client_id, "127.0.0.1", port);
    options.set_keep_alive(Duration::from_secs(5));
    let (client, mut eventloop) = AsyncClient::new(options, 64);
    client
        .subscribe(filter, QoS::AtLeastOnce)
        .await
        .expect("subscribe");

    let mut out = Vec::new();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if out.len() >= min_count {
            break;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, eventloop.poll()).await {
            Ok(Ok(Event::Incoming(Packet::Publish(publish)))) => {
                out.push((
                    publish.topic,
                    String::from_utf8_lossy(&publish.payload).to_string(),
                ));
            }
            Ok(Ok(_)) => {}
            Ok(Err(_)) | Err(_) => break,
        }
    }
    out
}

fn payload_json(messages: &[(String, String)], topic: &str) -> Option<Value> {
    messages
        .iter()
        .rev()
        .find(|(t, _)| t == topic)
        .map(|(_, payload)| serde_json::from_str(payload).expect("payload should be JSON"))
}

/// [`collect_messages`]は毎回**新規接続**するため、その接続時点で既に
/// retain されている値を即座に受け取ってしまう(retain の意味論そのもの -
/// `retain_delivers_last_value_to_a_fresh_subscriber`はまさにこれを検証
/// する)。スロットルテストのように「特定の時点より後に届いたメッセージ
/// だけ」を時系列で区別したい場合は使えない - 1本の接続を張りっぱなしに
/// して、届いた順に蓄積するこちらを使う。
struct LiveSubscriber {
    messages: Arc<tokio::sync::Mutex<Vec<(String, String)>>>,
    _task: tokio::task::JoinHandle<()>,
}

impl LiveSubscriber {
    async fn subscribe(port: u16, client_id: &str, filter: &str) -> Self {
        let mut options = MqttOptions::new(client_id, "127.0.0.1", port);
        options.set_keep_alive(Duration::from_secs(5));
        let (client, mut eventloop) = AsyncClient::new(options, 64);
        client
            .subscribe(filter, QoS::AtLeastOnce)
            .await
            .expect("subscribe");

        let messages = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let messages_for_task = messages.clone();
        let task = tokio::spawn(async move {
            loop {
                match eventloop.poll().await {
                    Ok(Event::Incoming(Packet::Publish(publish))) => {
                        messages_for_task.lock().await.push((
                            publish.topic,
                            String::from_utf8_lossy(&publish.payload).to_string(),
                        ));
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        });
        Self {
            messages,
            _task: task,
        }
    }

    /// これまでに届いたメッセージの、その時点でのスナップショット(到着順)。
    async fn snapshot(&self) -> Vec<(String, String)> {
        self.messages.lock().await.clone()
    }
}

// --- hub テストアプリ --------------------------------------------------------

struct TestApp {
    router: Router,
    token: String,
    pool: SqlitePool,
    manager: Arc<CollectorManager>,
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
    let token = auth
        .login("admin", "password123")
        .await
        .expect("admin login");

    let sessions = Arc::new(HubSessions::new(banto_broker::BackoffConfig::default()));
    let manager = Arc::new(CollectorManager::new(
        pool.clone(),
        env.data_dir(),
        Arc::new(SystemClock),
        fast_options(),
        sessions,
    ));
    manager.rebuild().await.expect("initial rebuild");

    let (events_tx, _rx) = broadcast::channel(16);
    let write_control = Arc::new(WriteControl::new(false));
    let write_audit = WriteAuditService::new(pool.clone());
    let mqtt = Arc::new(MqttPublisher::new(manager.clone()));
    let api_keys = ApiKeysService::new(pool.clone());
    // T4: このファイルは gRPC 自体をテストしない(`tests/grpc.rs`の責務)が、
    // `api_router`の T4 引数（REST/gRPC で共有する rate_limiter・
    // `GrpcServer`)は必須のため、`apply`を呼ばない(listen しない)だけの
    // 構築に留める。
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

    TestApp {
        router,
        token,
        pool,
        manager,
        _env: env,
    }
}

/// `/api/v1/*`向け - CSRF ヘッダ不要（`crate::rest`の doc comment「二系統に
/// 分かれたルーター」参照）。管理系(`/api/mqtt-settings`等)の GET には使え
/// ない - そちらは [`get_json_admin`] を使うこと。
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

/// 管理系(`/api/mqtt-settings`等)向け - `GET`でも CSRF ヘッダが必要
/// （`require_banto_client_header`はメソッドを問わず管理系ルーター全体に
/// 掛かる - `crate::rest::tests::admin_routes_require_the_csrf_header`と
/// 同じ規律）。
async fn get_json_admin(router: &Router, path: &str, token: &str) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            HttpRequest::get(path)
                .header("Authorization", format!("Bearer {token}"))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
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

/// `PUT /api/mqtt-settings`を叩いて即時適用させる。
async fn put_mqtt_settings(
    app: &TestApp,
    broker_port: u16,
    client_id: &str,
    min_interval_ms: i64,
    enabled: bool,
) -> (StatusCode, Value) {
    write_json(
        &app.router,
        "PUT",
        "/api/mqtt-settings",
        &app.token,
        json!({
            "enabled": enabled,
            "host": "127.0.0.1",
            "port": broker_port,
            "clientId": client_id,
            "username": null,
            "password": null,
            "prefix": "banto",
            "qos": 1,
            "minIntervalMs": min_interval_ms,
        }),
    )
    .await
}

async fn status_mqtt_connected(app: &TestApp) -> bool {
    let (status, json) = get_json(&app.router, "/api/v1/status", &app.token).await;
    assert_eq!(status, StatusCode::OK);
    json["mqtt"]["connected"].as_bool().unwrap_or(false)
}

// ---------------------------------------------------------------------------
// 1〜3. E2E ハッピーパス: 値発行 + retain + $state online
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_publishes_values_with_retain_and_online_state() {
    let broker_port = start_test_broker().await;
    let app = test_app("e2e-happy-path").await;
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 1234); // 40001

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
        wait_until(Duration::from_secs(6), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get("tag:1"))
                .map(|s| s.value)
                == Some(Some(1234.0))
        })
        .await,
        "collector should observe the simulator value before enabling MQTT"
    );

    let (status, _body) = put_mqtt_settings(&app, broker_port, "hub-happy-path", 100, true).await;
    assert_eq!(status, StatusCode::OK);

    assert!(
        wait_until(Duration::from_secs(6), || async {
            status_mqtt_connected(&app).await
        })
        .await,
        "/api/v1/status should report mqtt.connected=true once the publisher connects"
    );

    // $state（online, retain）と tag 値の一斉発行(接続時)の両方を1つの
    // 購読(banto/#)で拾う - 設計 §5.3「LWT: ...online(接続時発行)」
    // 「接続時は全タグの現在値を一斉発行」。
    let messages =
        collect_messages(broker_port, "sub-1", "banto/#", 2, Duration::from_secs(6)).await;

    let state_payload = messages
        .iter()
        .find(|(topic, _)| topic == "banto/$state")
        .map(|(_, payload)| payload.as_str());
    assert_eq!(state_payload, Some("online"));

    let value_json = payload_json(&messages, "banto/line1/fast/temp01")
        .expect("temp01 should have been published");
    assert_eq!(value_json["v"], 1234.0);
    assert_eq!(value_json["q"], "good");
    assert!(value_json["t"].as_i64().is_some());
}

// ---------------------------------------------------------------------------
// 2. retain: 発行後の新規購読が即座に最終値を受信
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retain_delivers_last_value_to_a_fresh_subscriber() {
    let broker_port = start_test_broker().await;
    let app = test_app("retain").await;
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
        wait_until(Duration::from_secs(6), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get("tag:1"))
                .map(|s| s.value)
                == Some(Some(42.0))
        })
        .await
    );

    let (status, _) = put_mqtt_settings(&app, broker_port, "hub-retain", 100, true).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        wait_until(Duration::from_secs(6), || async {
            status_mqtt_connected(&app).await
        })
        .await
    );

    // ここで最初の購読者が既に一斉発行(retain)を受け取っているはず -
    // これを待ち切ってから「まだ誰も購読していない状態から新規購読する」
    // という retain の趣旨に沿ったテストにする。
    let first = collect_messages(
        broker_port,
        "sub-first",
        "banto/line1/fast/temp01",
        1,
        Duration::from_secs(6),
    )
    .await;
    assert_eq!(
        payload_json(&first, "banto/line1/fast/temp01").unwrap()["v"],
        42.0
    );

    // 新規購読者(このテストで初めて接続する完全に別のクライアント) -
    // 発行側は何も追加発行していないのに、即座に retain 済みの最終値が届く。
    let fresh = collect_messages(
        broker_port,
        "sub-fresh",
        "banto/line1/fast/temp01",
        1,
        Duration::from_secs(4),
    )
    .await;
    assert_eq!(
        payload_json(&fresh, "banto/line1/fast/temp01").unwrap()["v"],
        42.0,
        "a fresh subscriber should immediately receive the retained last value"
    );
}

// ---------------------------------------------------------------------------
// 4. スロットル: min_interval_ms 内の連続変化は抑止され、明けに最新値が届く
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn throttle_suppresses_rapid_changes_and_sends_the_latest_value_once_the_window_opens() {
    let broker_port = start_test_broker().await;
    let app = test_app("throttle").await;
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
        wait_until(Duration::from_secs(6), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get("tag:1"))
                .map(|s| s.value)
                == Some(Some(100.0))
        })
        .await
    );

    const MIN_INTERVAL_MS: i64 = 1000;
    let (status, _) =
        put_mqtt_settings(&app, broker_port, "hub-throttle", MIN_INTERVAL_MS, true).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        wait_until(Duration::from_secs(6), || async {
            status_mqtt_connected(&app).await
        })
        .await
    );

    // 1本の接続を張りっぱなしにして時系列順に蓄積する([`LiveSubscriber`]
    // の doc comment参照 - `collect_messages`の毎回新規接続だと、接続時点で
    // 既に retain されている値を「新着」と誤認してしまう)。
    let live =
        LiveSubscriber::subscribe(broker_port, "sub-throttle", "banto/line1/fast/temp01").await;

    // 初回の一斉発行(v=100、既に retain 済みの可能性が高い)を待ち切る -
    // これがこのタグの「直近発行時刻」の基準になる。
    assert!(
        wait_until(Duration::from_secs(6), || async {
            payload_json(&live.snapshot().await, "banto/line1/fast/temp01")
                .map(|v| v["v"] == 100.0)
                .unwrap_or(false)
        })
        .await,
        "the initial forced publish (v=100) should arrive before the throttle races start"
    );
    let baseline_count = live.snapshot().await.len();

    // スロットル窓(1000ms)の中で2回連続変化させる - 中間値(200)は
    // スロットルで抑止され、最終値(300)だけが窓明け後に届くはず。
    sim.set_holding_register(0, 200);
    tokio::time::sleep(Duration::from_millis(150)).await;
    sim.set_holding_register(0, 300);

    // 窓が明ける(概ね+1000ms)まで待つ - 固定 sleep ではなく wait_until
    // にして、CI の CPU 負荷でタイミングがずれても(評価ループ自体が
    // `MissedTickBehavior::Delay`で遅延を許容する設計 - `crate::mqtt`の
    // モジュール doc comment参照)flake しないようにする。上限5秒。
    assert!(
        wait_until(Duration::from_secs(5), || async {
            live.snapshot().await.len() > baseline_count
        })
        .await,
        "the throttled publish should eventually arrive once the window opens"
    );
    // 直後にもう1本余分に届く(意図しない重複発行)ケースを拾えるよう、
    // 少し待ってから確定させる。
    tokio::time::sleep(Duration::from_millis(300)).await;
    let after: Vec<(String, String)> = live.snapshot().await.split_off(baseline_count);

    // 抑止された中間値(200)は一度も発行されていないこと。
    assert!(
        !after.iter().any(|(_, payload)| payload.contains("200")),
        "an intermediate value suppressed by the throttle must never be published: {after:?}"
    );
    // スロットル明け最初の tick で最新値(300)だけが1回届いていること
    // (スロットルは抑止であって間引き配信ではない - 200 が来ないのと対称に、
    // 300 も複数回重複しては来ない)。
    assert_eq!(
        after.len(),
        1,
        "exactly one publish should land once the throttle window opens: {after:?}"
    );
    let latest: Value = serde_json::from_str(&after[0].1).expect("payload should be JSON");
    assert_eq!(latest["v"], 300.0);
}

// ---------------------------------------------------------------------------
// 5. enabled=false では何も発行されない・PUT で有効化すると発行開始(即時適用)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disabled_publishes_nothing_and_enabling_via_put_starts_publishing_immediately() {
    let broker_port = start_test_broker().await;
    let app = test_app("disabled-then-enabled").await;
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 7);

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
        wait_until(Duration::from_secs(6), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get("tag:1"))
                .map(|s| s.value)
                == Some(Some(7.0))
        })
        .await
    );

    // enabled=false のまま(既定) - status は connected=false のはず。
    let (status, body) = get_json(&app.router, "/api/v1/status", &app.token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["mqtt"]["enabled"], false);
    assert_eq!(body["mqtt"]["connected"], false);

    let nothing = collect_messages(
        broker_port,
        "sub-disabled",
        "banto/#",
        1,
        Duration::from_millis(500),
    )
    .await;
    assert!(
        nothing.is_empty(),
        "no publish should happen while mqtt.enabled=false: {nothing:?}"
    );

    // PUT で有効化(即時適用) - 再起動なしに発行が始まる。
    let (status, body) = put_mqtt_settings(&app, broker_port, "hub-enable-later", 100, true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], true);
    assert!(
        body.get("password").is_none(),
        "the response must never echo a password field"
    );

    assert!(
        wait_until(Duration::from_secs(6), || async {
            status_mqtt_connected(&app).await
        })
        .await
    );

    let messages = collect_messages(
        broker_port,
        "sub-after-enable",
        "banto/line1/fast/temp01",
        1,
        Duration::from_secs(6),
    )
    .await;
    assert_eq!(
        payload_json(&messages, "banto/line1/fast/temp01").unwrap()["v"],
        7.0
    );
}

// ---------------------------------------------------------------------------
// GET /api/mqtt-settings は password を返さない
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_mqtt_settings_never_returns_a_password_field() {
    let app = test_app("get-settings-no-password").await;

    let (status, _) = write_json(
        &app.router,
        "PUT",
        "/api/mqtt-settings",
        &app.token,
        json!({
            "enabled": false,
            "host": "broker.local",
            "port": 1883,
            "clientId": "hub-1",
            "username": "user1",
            "password": "s3cret",
            "prefix": "banto",
            "qos": 1,
            "minIntervalMs": 1000,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = get_json_admin(&app.router, "/api/mqtt-settings", &app.token).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.get("password").is_none());
    assert_eq!(body["username"], "user1");
    assert_eq!(body["host"], "broker.local");

    // 空文字パスワードで PUT しても既存のパスワードは変更なし(直接読める
    // 手段は無いので、少なくとも「保存自体は成功する」ことだけ確認する -
    // 実値の維持は `settings::tests::mqtt_config_*` の単体テストで担保済み)。
    let (status, _) = write_json(
        &app.router,
        "PUT",
        "/api/mqtt-settings",
        &app.token,
        json!({
            "enabled": false,
            "host": "broker.local",
            "port": 1883,
            "clientId": "hub-1",
            "username": "user1",
            "password": "",
            "prefix": "banto",
            "qos": 1,
            "minIntervalMs": 1000,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

// ---------------------------------------------------------------------------
// バリデーション
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn put_mqtt_settings_rejects_qos_2_and_enabling_without_a_host() {
    let app = test_app("validation").await;

    let (status, body) = write_json(
        &app.router,
        "PUT",
        "/api/mqtt-settings",
        &app.token,
        json!({
            "enabled": false,
            "host": "",
            "port": 1883,
            "clientId": "hub-1",
            "username": null,
            "password": null,
            "prefix": "banto",
            "qos": 2,
            "minIntervalMs": 1000,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["kind"], "validation");

    let (status, body) = write_json(
        &app.router,
        "PUT",
        "/api/mqtt-settings",
        &app.token,
        json!({
            "enabled": true,
            "host": "",
            "port": 1883,
            "clientId": "hub-1",
            "username": null,
            "password": null,
            "prefix": "banto",
            "qos": 1,
            "minIntervalMs": 1000,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["kind"], "validation");
}
