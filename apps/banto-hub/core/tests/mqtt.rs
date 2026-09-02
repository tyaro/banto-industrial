//! T3 の統合テスト（docs/tag-server-design.md §5.3）: 実際の axum `Router`
//! と Modbus TCP シミュレータ（`tests/integration.rs`と同じ）、そして
//! **in-process MQTT ブローカー（`rumqttd`）**を使った E2E。`tests/write.rs`/
//! `tests/stream.rs`と同じ理由で `fast_options`/`wait_until`等をこのファイル
//! 内に複製している（各 `tests/*.rs` は独立クレートとしてコンパイルされ、
//! private helper を共有できない）。`TempEnv` は `tests/common/mod.rs` に
//! 集約済み（2026-08-08、テスト一時ディレクトリリークの根治）。
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
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request as HttpRequest, StatusCode};
use axum::Router;
use banto_collect::{BackoffConfig, CollectorOptions};
use banto_hub_core::api_keys::ApiKeysService;
use banto_hub_core::audit::AuditLogService;
use banto_hub_core::broker_glue::{HubSessions, SlmpSimRegistry};
use banto_hub_core::commissioning::CommissioningService;
use banto_hub_core::computed::{ComputedEngine, ServerTagStore};
use banto_hub_core::controller::{CollectionController, CollectionState, RunMode};
use banto_hub_core::db::init_db;
use banto_hub_core::grpc::{GrpcServer, GrpcService};
use banto_hub_core::hub::CollectorManager;
use banto_hub_core::mqtt::MqttPublisher;
use banto_hub_core::rest::{api_router, api_router_with_controller};
use banto_hub_core::settings::SettingsService;
use banto_hub_core::test_output::TestOutputControl;
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

mod common;
use common::TempEnv;

const CLIENT_HEADER: (&str, &str) = ("X-Banto-Client", "banto");

/// Temp-dir prefix passed to `TempEnv::new` (see `tests/common/mod.rs`).
const TEMP_ENV_PREFIX: &str = "banto-hub-mqtt-it";

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

        word_order: "low_high".to_string(),
    }
}

fn group_input(name: &str, conn_id: i64, period_ms: i64) -> CollectionGroupInput {
    CollectionGroupInput {
        name: name.to_string(),
        plc_connection_id: conn_id,
        period_ms,
        enabled: true,
        default_writable: true,
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
        sessions,
        sim_registry,
        computed,
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
    let settings = SettingsService::new(pool.clone());
    let commissioning = CommissioningService::load(settings, users.clone())
        .await
        .expect("CommissioningService::load");
    commissioning
        .lock_down()
        .await
        .expect("lock_down the test environment");

    let router = api_router(
        users,
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
        token,
        pool,
        manager,
        _env: env,
    }
}

/// T15-3（設計 §6.3）: [`TestApp`]は`MqttPublisher::new`（コントローラ非注入 -
/// このモジュールの他のテストは`AllSimulation`/テスト出力を一切対象としない
/// ため常に`PublishTarget::Normal`扱いでよい）を使うが、テスト出力トピック
/// は`Running`+`AllSimulation`+`TestOutputControl`有効時のみ選ばれる
/// （`crate::mqtt::eval_target`参照）ので、この構成では検証できない。
/// このテスト専用に、`MqttPublisher::new_with_controller`+
/// `api_router_with_controller`で実際の`CollectionController`/
/// `TestOutputControl`を配線した別構成を用意する。
struct TestOutputTestApp {
    router: Router,
    token: String,
    pool: SqlitePool,
    manager: Arc<CollectorManager>,
    controller: Arc<CollectionController>,
    test_output: Arc<TestOutputControl>,
    _env: TempEnv,
}

impl Drop for TestOutputTestApp {
    fn drop(&mut self) {
        common::shutdown_test_app(&self.manager, &self.pool);
    }
}

async fn test_output_test_app(label: &str) -> TestOutputTestApp {
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
        sessions,
        sim_registry,
        computed,
    ));
    manager.rebuild().await.expect("initial rebuild");

    let (events_tx, _rx) = broadcast::channel(16);
    let write_control = Arc::new(WriteControl::new(false));
    let test_output = Arc::new(TestOutputControl::new());
    let controller = Arc::new(CollectionController::new(
        manager.clone(),
        write_control.clone(),
        test_output.clone(),
    ));
    let write_audit = WriteAuditService::new(pool.clone());
    let mqtt = Arc::new(MqttPublisher::new_with_controller(
        manager.clone(),
        controller.clone(),
        test_output.clone(),
    ));
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

    let router = api_router_with_controller(
        users,
        audit,
        PlcConnectionService::new(pool.clone()),
        CollectionGroupService::new(pool.clone()),
        TagService::new(pool.clone()),
        api_keys,
        manager.clone(),
        controller.clone(),
        auth,
        commissioning,
        events_tx,
        false,
        write_control,
        write_audit,
        mqtt,
        grpc_server,
        rate_limiter,
        test_output.clone(),
        banto_hub_core::profile_paths::DEFAULT_PROFILE_ID.to_string(),
    );

    TestOutputTestApp {
        router,
        token,
        pool,
        manager,
        controller,
        test_output,
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

/// `PUT /api/mqtt-settings`を叩いて即時適用させる。`router`/`token`を直接
/// 取るのは、T15-3 のテスト出力テストが`TestApp`とは別の構成
/// （`TestOutputTestApp`、下記）を使うため - 両方から共有できるようにした。
async fn put_mqtt_settings(
    router: &Router,
    token: &str,
    broker_port: u16,
    client_id: &str,
    min_interval_ms: i64,
    enabled: bool,
) -> (StatusCode, Value) {
    write_json(
        router,
        "PUT",
        "/api/mqtt-settings",
        token,
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

/// [`put_mqtt_settings`]と同じ理由で`router`/`token`を直接取る。
async fn status_mqtt_connected(router: &Router, token: &str) -> bool {
    let (status, json) = get_json(router, "/api/v1/status", token).await;
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
        // period 1000ms（他テストの 100ms より意図的に長い）: このテストは
        // 発行ペイロードの品質が "good" であることまで assert する（下記）。
        // 品質は読み出し時判定で period × 2.5 より古いサンプルは stale に
        // なるため、100ms（閾値 250ms）だと混雑した CI ランナーの停滞だけで
        // stale に落ちて flake する（CI 初走行 2026-08-07 で実際に発生）。
        // 1000ms なら閾値 2.5 秒になり、検証意図を弱めずランナー耐性が上がる。
        .create(group_input("fast", conn.id, 1000))
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

    let (status, _body) = put_mqtt_settings(
        &app.router,
        &app.token,
        broker_port,
        "hub-happy-path",
        100,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    assert!(
        wait_until(Duration::from_secs(6), || async {
            status_mqtt_connected(&app.router, &app.token).await
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

    let (status, _) = put_mqtt_settings(
        &app.router,
        &app.token,
        broker_port,
        "hub-retain",
        100,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        wait_until(Duration::from_secs(6), || async {
            status_mqtt_connected(&app.router, &app.token).await
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
    let (status, _) = put_mqtt_settings(
        &app.router,
        &app.token,
        broker_port,
        "hub-throttle",
        MIN_INTERVAL_MS,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        wait_until(Duration::from_secs(6), || async {
            status_mqtt_connected(&app.router, &app.token).await
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
    // ペイロード文字列全体への部分文字列一致(contains)だと、エポックミリ秒の
    // タイムスタンプに偶然 "200" という数字列が含まれた場合に誤検知するため、
    // JSON をパースして "v" フィールドの値そのものを比較する。
    assert!(
        !after.iter().any(|(_, payload)| {
            let value: Value = serde_json::from_str(payload).expect("payload should be JSON");
            value["v"] == 200.0
        }),
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
    let (status, body) = put_mqtt_settings(
        &app.router,
        &app.token,
        broker_port,
        "hub-enable-later",
        100,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], true);
    assert!(
        body.get("password").is_none(),
        "the response must never echo a password field"
    );

    assert!(
        wait_until(Duration::from_secs(6), || async {
            status_mqtt_connected(&app.router, &app.token).await
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
// T15-3: テスト出力専用トピック（設計 §6.3）
// ---------------------------------------------------------------------------

/// `POST /api/test-output/enable`|`disable`のレスポンス
/// (`crate::rest::TestOutputStatusEntry`)。
async fn post_test_output(router: &Router, token: &str, action: &str) -> (StatusCode, Value) {
    write_json(
        router,
        "POST",
        &format!("/api/test-output/{action}"),
        token,
        json!({}),
    )
    .await
}

/// `AllSimulation`かつ`Running`でなければ有効化を拒否し、有効化後は通常
/// トピックを一切汚さず専用トピック（`{prefix}/test/{run_id}/...`）だけに
/// `simulation=true`・一致する`run_id`付きで`retain=false`発行する - 実装
/// 指示のテスト計画1〜3を1本の統合テストにまとめたもの。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_output_topics_carry_simulation_payloads_only_while_armed_during_all_simulation() {
    let broker_port = start_test_broker().await;
    let app = test_output_test_app("test-output-happy-path").await;

    let conn = PlcConnectionService::new(app.pool.clone())
        .create(conn_input("line1", 1)) // AllSimulation はホスト/ポートに接続しない
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

    let (status, _) = put_mqtt_settings(
        &app.router,
        &app.token,
        broker_port,
        "hub-test-output",
        0,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // 収集停止中は有効化の前提を満たさない(`Running`+`AllSimulation`必須)。
    let (status, body) = post_test_output(&app.router, &app.token, "enable").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "test_output_not_available");

    let run_status = app.controller.start(RunMode::AllSimulation).await;
    assert_eq!(run_status.state, CollectionState::Running);
    let run_id = run_status
        .run_id
        .expect("AllSimulation run should have a run_id");

    assert!(
        wait_until(Duration::from_secs(6), || async {
            status_mqtt_connected(&app.router, &app.token).await
        })
        .await,
        "mqtt should connect once enabled, independent of test-output"
    );

    // 有効化前: `AllSimulation`中なので通常トピックには何も来ない
    // (既存 PR #95 挙動)し、まだ有効化していないのでテスト出力トピックにも
    // 何も来ない。
    let nothing = collect_messages(
        broker_port,
        "sub-before-enable",
        "banto/#",
        1,
        Duration::from_millis(500),
    )
    .await;
    let stray_state = nothing
        .iter()
        .filter(|(topic, _)| topic != "banto/$state")
        .count();
    assert_eq!(
        stray_state, 0,
        "no tag topic (normal or test) should publish before test-output is enabled: {nothing:?}"
    );

    // 有効化: `enabled: true`・`run_id`が一致するレスポンス。
    let (status, body) = post_test_output(&app.router, &app.token, "enable").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], true);
    assert_eq!(body["run_id"], run_id);

    let live = LiveSubscriber::subscribe(broker_port, "sub-test-live", "banto/#").await;
    let test_topic = format!("banto/test/{run_id}/line1/fast/temp01");
    // 初回サイクルは `q:"bad", v:null`(シミュレーション値サンプル前)で
    // publish されることがあるため、トピック到達だけでなく最新payloadの
    // `v`が数値になる(=goodなサンプル到達)まで待つ。
    assert!(
        wait_until(Duration::from_secs(6), || async {
            let messages = live.snapshot().await;
            payload_json(&messages, &test_topic)
                .map(|payload| payload["v"].is_number())
                .unwrap_or(false)
        })
        .await,
        "the test-output topic should carry a numeric sample once armed"
    );

    let messages = live.snapshot().await;
    let payload = payload_json(&messages, &test_topic).expect("test-output payload");
    assert_eq!(payload["simulation"], true);
    assert_eq!(payload["run_id"], run_id);
    assert!(payload["v"].is_number(), "payload: {payload:?}");

    // 通常トピックには一度も来ていないこと(`AllSimulation`中は抑止のまま)。
    assert!(
        !messages
            .iter()
            .any(|(topic, _)| topic == "banto/line1/fast/temp01"),
        "the normal topic must stay silent during all-simulation: {messages:?}"
    );

    // 明示的な disable: 以後テスト出力トピックへの新規発行が止まる。
    let (status, body) = post_test_output(&app.router, &app.token, "disable").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], false);

    let baseline = live.snapshot().await.len();
    tokio::time::sleep(Duration::from_millis(700)).await;
    let after_disable = live.snapshot().await.split_off(baseline);
    assert!(
        after_disable.is_empty(),
        "no further test-output publish should happen after disable: {after_disable:?}"
    );

    // retain=false: 発行が止まった後に**新規**購読しても、broker には
    // テスト出力トピックの最終値が残っていない(通常トピックの
    // `retain_delivers_last_value_to_a_fresh_subscriber`と対照的な挙動 -
    // 実装指示「retain=false always」)。発行中の新規購読では「たまたま
    // タイミング内に生きた発行が来た」だけで retain の有無を判別できない
    // ため、発行が完全に止まった後で確認する。
    let fresh = collect_messages(
        broker_port,
        "sub-test-fresh",
        &test_topic,
        1,
        Duration::from_millis(700),
    )
    .await;
    assert!(
        fresh.is_empty(),
        "a fresh subscriber must NOT receive a retained test-output message: {fresh:?}"
    );

    let (status, body) = get_json(&app.router, "/api/v1/status", &app.token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["test_output"]["enabled"], false);
    assert_eq!(body["test_output"]["run_id"], Value::Null);
}

/// 収集停止は明示的な`disable`と同じ結果になる - `CollectionController`の
/// `stop_locked`が`test_output.disable()`する（設計「停止／終了／切替後に
/// 必ず無効へ戻る」）。停止後は再度`AllSimulation`が`Running`に戻るまで
/// 有効化が拒否される。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_output_auto_disables_on_stop_and_re_enable_is_rejected_until_all_simulation_runs_again(
) {
    let broker_port = start_test_broker().await;
    let app = test_output_test_app("test-output-stop-clears").await;

    let conn = PlcConnectionService::new(app.pool.clone())
        .create(conn_input("line1", 1))
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

    let (status, _) = put_mqtt_settings(
        &app.router,
        &app.token,
        broker_port,
        "hub-test-output-stop",
        0,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let run_status = app.controller.start(RunMode::AllSimulation).await;
    let run_id = run_status
        .run_id
        .expect("AllSimulation run should have a run_id");
    assert!(
        wait_until(Duration::from_secs(6), || async {
            status_mqtt_connected(&app.router, &app.token).await
        })
        .await
    );

    let (status, body) = post_test_output(&app.router, &app.token, "enable").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["run_id"], run_id);
    assert!(app.test_output.is_active_for(Some(run_id)));

    let test_topic = format!("banto/test/{run_id}/line1/fast/temp01");
    let live = LiveSubscriber::subscribe(broker_port, "sub-test-stop-live", "banto/#").await;
    assert!(
        wait_until(Duration::from_secs(6), || async {
            live.snapshot()
                .await
                .iter()
                .any(|(topic, _)| topic == &test_topic)
        })
        .await,
        "the test-output topic should receive a publish once armed"
    );

    app.controller.stop().await;

    // 設計「停止...後に必ず無効へ戻る」: ライブフラグ自身がクリアされる。
    assert!(!app.test_output.is_active_for(Some(run_id)));
    let (status, body) = get_json(&app.router, "/api/v1/status", &app.token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["test_output"]["enabled"], false);

    // 停止直後は`Stopped`なので再有効化は前提条件を満たさず拒否される。
    let (status, body) = post_test_output(&app.router, &app.token, "enable").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "test_output_not_available");

    let baseline = live.snapshot().await.len();
    tokio::time::sleep(Duration::from_millis(700)).await;
    let after_stop = live.snapshot().await.split_off(baseline);
    assert!(
        after_stop.is_empty(),
        "no further test-output publish should happen after the collection stops: {after_stop:?}"
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
