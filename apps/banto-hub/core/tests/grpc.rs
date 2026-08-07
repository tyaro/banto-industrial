//! T4 の統合テスト（docs/tag-server-design.md §5.4「gRPC（T4）」)。
//!
//! `tests/write.rs`/`tests/stream.rs`/`tests/integration.rs` と同じ理由
//! （各 `tests/*.rs` は独立クレートとしてコンパイルされ、private helper を
//! 共有できない）で `TempEnv`/`fast_options`/`wait_until`/`issue_key` 等を
//! このファイル内に複製している。gRPC クライアントは `tonic` 本体が生成する
//! `TagServiceClient`（`banto_hub_core::grpc::tagserver_v1`）をそのまま使う
//! - 実装指示どおり dev-dependency は追加していない。
//!
//! テスト構成（実装指示のテスト計画1〜6に対応）:
//! 1. E2E: シミュレータ + `GetCatalog`/`ReadValues`（値・品質・時刻が REST
//!    と一致 - `crate::hub::effective_sample`を両者が共有するため構造的に
//!    保証される）
//! 2. `StreamValues`: 初期スナップショット → 値変化で `ValueBatch` 受信
//!    （on_change）。ワイルドカード + 構成変更で新タグが現れる
//! 3. `StreamEvents`: PLC 断イベント受信
//! 4. `WriteValue`: ハッピーパス + ゲート代表例（`not_writable` →
//!    `PERMISSION_DENIED`、受付 off → `FAILED_PRECONDITION` + 監査、write
//!    スコープなし → `PERMISSION_DENIED`）。REST 側の write.rs は別ファイル
//!    として引き続き独立に全通過する（`cargo test -p banto-hub-core --test
//!    write`で確認済み）- 両者は `crate::write_path::execute_write` を
//!    共有するので、ゲートの回帰があれば両方で同時に検知される
//! 5. 認証: メタデータなし → `UNAUTHENTICATED`、read キーで `WriteValue`
//!    拒否 → `PERMISSION_DENIED`
//! 6. `grpc.enabled=false` では bind しない、`PUT /api/grpc-settings` で
//!    有効化すると開始する

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
use banto_hub_core::grpc::tagserver_v1::tag_service_client::TagServiceClient;
use banto_hub_core::grpc::tagserver_v1::{
    tag_value, write_value_request, Event, GetCatalogRequest, Quality as PbQuality,
    ReadValuesRequest, StreamEventsRequest, StreamValuesRequest, SubscribeMode, ValueBatch,
    WriteValueRequest,
};
use banto_hub_core::grpc::{GrpcServer, GrpcService};
use banto_hub_core::hub::CollectorManager;
use banto_hub_core::rest::api_router;
use banto_hub_core::settings::GrpcSettings;
use banto_hub_core::users::UsersService;
use banto_hub_core::write_audit::WriteAuditService;
use banto_hub_core::write_control::WriteControl;
use banto_hub_core::write_rate::{WriteRateLimitConfig, WriteRateLimiter};
use banto_plc::slmp::address::SlmpDevice;
use banto_plc_write::slmp::simulator::Simulator;
use banto_server::{AuthState, Identity};
use banto_tags::{
    CollectionGroupInput, CollectionGroupService, PlcConnectionInput, PlcConnectionService,
    TagInput, TagService,
};
use banto_tstore::SystemClock;
use serde_json::{json, Value as JsonValue};
use sqlx::SqlitePool;
use tokio::sync::broadcast;
use tokio::sync::Mutex as AsyncMutex;
use tonic::transport::Channel;
use tower::ServiceExt;

const CLIENT_HEADER: (&str, &str) = ("X-Banto-Client", "banto");

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempEnv {
    root: PathBuf,
}

impl TempEnv {
    fn new(label: &str) -> Self {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        // remove_dir_all は Windows では SQLite 接続がハンドルを解放し切る前に
        // 呼ばれて失敗することがあり、その場合ディレクトリが残り続ける。PID
        // だけでは再利用時に古い(既に初期化済みの)ディレクトリと衝突しうる
        // ため、ナノ秒精度のタイムスタンプも一意性キーに含める。
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "banto-hub-grpc-it-{}-{label}-{id}-{nanos}",
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

#[allow(clippy::too_many_arguments)]
fn tag_input(
    name: &str,
    group_id: i64,
    address: &str,
    data_type: &str,
    writable: bool,
    enabled: bool,
) -> TagInput {
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
        enabled,
        writable,
        tag_kind: "plc".to_string(),
        expression: None,
        retain: false,
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

/// `crate::grpc::GrpcServer::apply`と同じ「空きポート取得」パターン
/// （`tests/mqtt.rs`の`free_port`と同じ理由付け）。
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a free port");
    listener.local_addr().expect("local_addr").port()
}

struct TestApp {
    router: Router,
    admin_token: String,
    pool: SqlitePool,
    manager: Arc<CollectorManager>,
    write_control: Arc<WriteControl>,
    grpc_server: Arc<GrpcServer>,
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
    let mqtt = Arc::new(banto_hub_core::mqtt::MqttPublisher::new(manager.clone()));
    let api_keys = ApiKeysService::new(pool.clone());
    let rate_limiter = Arc::new(AsyncMutex::new(WriteRateLimiter::new(
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
        write_control.clone(),
        write_audit,
        mqtt,
        grpc_server.clone(),
        rate_limiter,
    );

    TestApp {
        router,
        admin_token,
        pool,
        manager,
        write_control,
        grpc_server,
        _env: env,
    }
}

async fn admin_post(
    router: &Router,
    path: &str,
    token: &str,
    body: JsonValue,
) -> (StatusCode, JsonValue) {
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
    let json: JsonValue = serde_json::from_slice(&bytes).unwrap_or(JsonValue::Null);
    (status, json)
}

async fn admin_put(
    router: &Router,
    path: &str,
    token: &str,
    body: JsonValue,
) -> (StatusCode, JsonValue) {
    let response = router
        .clone()
        .oneshot(
            HttpRequest::put(path)
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
    let json: JsonValue = serde_json::from_slice(&bytes).unwrap_or(JsonValue::Null);
    (status, json)
}

/// `POST /api/api-keys` 経由でキーを発行し、平文キー全体(`bh_...`)と id を返す。
async fn issue_key(
    router: &Router,
    admin_token: &str,
    name: &str,
    scopes: &[&str],
) -> (String, i64) {
    let (status, body) = admin_post(
        router,
        "/api/api-keys",
        admin_token,
        json!({ "name": name, "scopes": scopes }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body:?}");
    (
        body["key"].as_str().unwrap().to_string(),
        body["id"].as_i64().unwrap(),
    )
}

/// タグ・グループ・接続を1本作って rebuild まで済ませ、`(tag_id,
/// external_name)` を返す共通フィクスチャ(`tests/write.rs`の`make_tag`と
/// 同型 - SLMP 固定でよい、gRPC 自体はプロトコルを意識しない)。
#[allow(clippy::too_many_arguments)]
async fn make_tag(
    app: &TestApp,
    conn_name: &str,
    port: u16,
    tag_name: &str,
    address: &str,
    data_type: &str,
    writable: bool,
    enabled: bool,
) -> (i64, String) {
    let conn = PlcConnectionService::new(app.pool.clone())
        .create(slmp_conn_input(conn_name, port))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    let tag = TagService::new(app.pool.clone())
        .create(tag_input(
            tag_name, group.id, address, data_type, writable, enabled,
        ))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild");
    (tag.id, format!("{conn_name}.fast.{tag_name}"))
}

/// gRPC サーバーを起動し(空きポート)、実際に接続できるまでリトライする -
/// `Server::serve`が別タスクで走り始めるまでの短い競合を吸収する
/// （`tests/mqtt.rs`のブローカー起動 probe と同じ発想）。
async fn start_grpc_and_connect(grpc_server: &GrpcServer) -> (u16, TagServiceClient<Channel>) {
    let port = free_port();
    grpc_server
        .apply(&GrpcSettings {
            enabled: true,
            port,
        })
        .await;

    let addr = format!("http://127.0.0.1:{port}");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        match TagServiceClient::connect(addr.clone()).await {
            Ok(client) => return (port, client),
            Err(err) => {
                if tokio::time::Instant::now() >= deadline {
                    panic!("gRPC サーバーに接続できませんでした: {err}");
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }
}

fn bearer_request<T>(message: T, token: &str) -> tonic::Request<T> {
    let mut request = tonic::Request::new(message);
    request.metadata_mut().insert(
        "authorization",
        format!("Bearer {token}")
            .parse()
            .expect("valid metadata value"),
    );
    request
}

// ---------------------------------------------------------------------------
// 1. E2E: GetCatalog / ReadValues
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_get_catalog_and_read_values_match_rest_semantics() {
    let app = test_app("e2e-catalog").await;
    let sim = Simulator::start().await;
    sim.set_word(SlmpDevice::D, 100, 1234);

    let (_tag_id, external_name) = make_tag(
        &app,
        "line1",
        sim.addr.port(),
        "temp01",
        "D100",
        "u16",
        false,
        true,
    )
    .await;

    assert!(
        wait_until(Duration::from_secs(3), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get("tag:1"))
                .and_then(|s| s.value)
                .is_some()
        })
        .await,
        "collector should have picked up the seeded value"
    );

    let (key, _id) = issue_key(&app.router, &app.admin_token, "reader", &["read"]).await;
    let (_port, mut client) = start_grpc_and_connect(&app.grpc_server).await;

    let catalog = client
        .get_catalog(bearer_request(
            GetCatalogRequest {
                connection: String::new(),
                group: String::new(),
            },
            &key,
        ))
        .await
        .expect("get_catalog should succeed")
        .into_inner();
    assert_eq!(catalog.revision, app.manager.revision());
    let entry = catalog
        .tags
        .iter()
        .find(|t| t.external_name == external_name)
        .expect("catalog should contain the seeded tag");
    assert_eq!(entry.address, "D100");
    assert_eq!(entry.data_type, "u16");
    assert!(entry.enabled);
    assert!(!entry.writable);

    let values = client
        .read_values(bearer_request(
            ReadValuesRequest {
                tags: vec![external_name.clone()],
            },
            &key,
        ))
        .await
        .expect("read_values should succeed")
        .into_inner();
    assert_eq!(values.values.len(), 1);
    let value = &values.values[0];
    assert_eq!(value.tag, external_name);
    assert_eq!(value.value, Some(tag_value::Value::Num(1234.0)));
    assert_eq!(value.quality, PbQuality::Good as i32);

    // 未知タグは REST と同じく INVALID_ARGUMENT で全体拒否。
    let err = client
        .read_values(bearer_request(
            ReadValuesRequest {
                tags: vec!["nope.nope.nope".to_string()],
            },
            &key,
        ))
        .await
        .expect_err("unknown tag should be rejected");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);

    sim.stop();
}

// ---------------------------------------------------------------------------
// 2. StreamValues: 初期スナップショット → on_change、ワイルドカード + 構成変更
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_values_sends_initial_snapshot_then_on_change() {
    let app = test_app("stream-values").await;
    let sim = Simulator::start().await;
    sim.set_word(SlmpDevice::D, 100, 10);

    let (_tag_id, external_name) = make_tag(
        &app,
        "line1",
        sim.addr.port(),
        "temp01",
        "D100",
        "u16",
        false,
        true,
    )
    .await;

    assert!(
        wait_until(Duration::from_secs(3), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get("tag:1"))
                .and_then(|s| s.value)
                .is_some()
        })
        .await,
        "collector should have picked up the seeded value"
    );

    let (key, _id) = issue_key(&app.router, &app.admin_token, "reader", &["read"]).await;
    let (_port, mut client) = start_grpc_and_connect(&app.grpc_server).await;

    let mut stream = client
        .stream_values(bearer_request(
            StreamValuesRequest {
                tags: vec![external_name.clone()],
                mode: SubscribeMode::OnChange as i32,
                interval_ms: 0,
            },
            &key,
        ))
        .await
        .expect("stream_values should succeed")
        .into_inner();

    let initial: ValueBatch = stream
        .message()
        .await
        .expect("stream should not error")
        .expect("initial snapshot should be sent");
    assert_eq!(initial.values.len(), 1);
    assert_eq!(initial.values[0].value, Some(tag_value::Value::Num(10.0)));

    sim.set_word(SlmpDevice::D, 100, 99);

    let changed: ValueBatch = tokio::time::timeout(Duration::from_secs(3), stream.message())
        .await
        .expect("should receive a change within 3s")
        .expect("stream should not error")
        .expect("stream should not end");
    assert_eq!(changed.values.len(), 1);
    assert_eq!(changed.values[0].value, Some(tag_value::Value::Num(99.0)));

    sim.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_values_wildcard_picks_up_a_tag_added_after_config_changed() {
    let app = test_app("stream-values-wildcard").await;
    let sim = Simulator::start().await;

    let conn = PlcConnectionService::new(app.pool.clone())
        .create(slmp_conn_input("line1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild (no tags yet)");

    let (key, _id) = issue_key(&app.router, &app.admin_token, "reader", &["read"]).await;
    let (_port, mut client) = start_grpc_and_connect(&app.grpc_server).await;

    let mut stream = client
        .stream_values(bearer_request(
            StreamValuesRequest {
                tags: vec!["line1.fast.*".to_string()],
                mode: SubscribeMode::OnChange as i32,
                interval_ms: 0,
            },
            &key,
        ))
        .await
        .expect("wildcard subscribe should succeed (0 matches is not an error)")
        .into_inner();

    let initial: ValueBatch = stream
        .message()
        .await
        .expect("stream should not error")
        .expect("initial snapshot (empty) should still be sent");
    assert!(initial.values.is_empty());

    sim.set_word(SlmpDevice::D, 200, 42);
    TagService::new(app.pool.clone())
        .create(tag_input("temp02", group.id, "D200", "u16", false, true))
        .await
        .unwrap();
    app.manager
        .rebuild()
        .await
        .expect("rebuild after adding tag");

    let appeared: ValueBatch = tokio::time::timeout(Duration::from_secs(3), stream.message())
        .await
        .expect("should receive the newly-matched tag within 3s")
        .expect("stream should not error")
        .expect("stream should not end");
    assert_eq!(appeared.values.len(), 1);
    assert_eq!(appeared.values[0].tag, "line1.fast.temp02");

    sim.stop();
}

// ---------------------------------------------------------------------------
// 3. StreamEvents: PLC 断イベント
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_events_relays_plc_disconnected() {
    let app = test_app("stream-events").await;
    let sim = Simulator::start().await;
    sim.set_word(SlmpDevice::D, 100, 1);

    make_tag(
        &app,
        "line1",
        sim.addr.port(),
        "temp01",
        "D100",
        "u16",
        false,
        true,
    )
    .await;

    assert!(
        wait_until(Duration::from_secs(3), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get("tag:1"))
                .and_then(|s| s.value)
                .is_some()
        })
        .await,
        "collector should be connected before we subscribe (avoids racing plc_connected)"
    );

    let (key, _id) = issue_key(&app.router, &app.admin_token, "reader", &["read"]).await;
    let (_port, mut client) = start_grpc_and_connect(&app.grpc_server).await;

    let mut stream = client
        .stream_events(bearer_request(StreamEventsRequest {}, &key))
        .await
        .expect("stream_events should succeed")
        .into_inner();

    sim.stop();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            remaining > Duration::ZERO,
            "timed out waiting for plc_disconnected"
        );
        let event: Event = tokio::time::timeout(remaining, stream.message())
            .await
            .expect("should receive an event before the deadline")
            .expect("stream should not error")
            .expect("stream should not end");
        if event.kind == "plc_disconnected" {
            assert_eq!(event.connection.as_deref(), Some("line1"));
            break;
        }
    }
}

// ---------------------------------------------------------------------------
// 4. WriteValue: ハッピーパス + ゲート代表例
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_value_happy_path_reaches_the_simulator() {
    let app = test_app("write-happy").await;
    let sim = Simulator::start().await;

    let (_tag_id, external_name) = make_tag(
        &app,
        "line1",
        sim.addr.port(),
        "temp01",
        "D100",
        "u16",
        true,
        true,
    )
    .await;
    app.write_control.enable();

    let (key, _id) = issue_key(
        &app.router,
        &app.admin_token,
        "writer",
        &["write:line1.fast.temp01"],
    )
    .await;
    let (_port, mut client) = start_grpc_and_connect(&app.grpc_server).await;

    let response = client
        .write_value(bearer_request(
            WriteValueRequest {
                tag: external_name,
                value: Some(write_value_request::Value::Num(1234.0)),
            },
            &key,
        ))
        .await
        .expect("write_value should succeed")
        .into_inner();
    assert_eq!(response.result, "ok");

    assert!(
        wait_until(Duration::from_secs(3), || async {
            sim.get_word(SlmpDevice::D, 100) == 1234
        })
        .await,
        "the simulator should observe the written value"
    );

    sim.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_value_not_writable_is_permission_denied() {
    let app = test_app("write-not-writable").await;
    let sim = Simulator::start().await;
    let (_tag_id, external_name) = make_tag(
        &app,
        "line1",
        sim.addr.port(),
        "temp01",
        "D100",
        "u16",
        false,
        true,
    )
    .await;
    app.write_control.enable();

    let (key, _id) = issue_key(
        &app.router,
        &app.admin_token,
        "writer",
        &["write:line1.fast.temp01"],
    )
    .await;
    let (_port, mut client) = start_grpc_and_connect(&app.grpc_server).await;

    let err = client
        .write_value(bearer_request(
            WriteValueRequest {
                tag: external_name,
                value: Some(write_value_request::Value::Num(1.0)),
            },
            &key,
        ))
        .await
        .expect_err("not_writable tag should be rejected");
    assert_eq!(err.code(), tonic::Code::PermissionDenied);

    sim.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_value_writes_disabled_is_failed_precondition_and_audited() {
    let app = test_app("write-disabled").await;
    let sim = Simulator::start().await;
    let (_tag_id, external_name) = make_tag(
        &app,
        "line1",
        sim.addr.port(),
        "temp01",
        "D100",
        "u16",
        true,
        true,
    )
    .await;
    // write_control は起動時 disabled のまま(§6-6) - enable() を呼ばない。

    let (key, _id) = issue_key(
        &app.router,
        &app.admin_token,
        "writer",
        &["write:line1.fast.temp01"],
    )
    .await;
    let (_port, mut client) = start_grpc_and_connect(&app.grpc_server).await;

    let err = client
        .write_value(bearer_request(
            WriteValueRequest {
                tag: external_name,
                value: Some(write_value_request::Value::Num(1.0)),
            },
            &key,
        ))
        .await
        .expect_err("writes-disabled should be rejected");
    assert_eq!(err.code(), tonic::Code::FailedPrecondition);
    assert!(err.message().contains("writes_disabled"));

    // log-before-write の抑制系(suppressed_disabled)が監査に残ることを
    // 確認する(設計 §6-3、REST と同じ共有経路 - `write_path::execute_write`)。
    let (status, body) = admin_post(
        &app.router,
        "/api/write-audit/list",
        &app.admin_token,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let rows = body["rows"].as_array().expect("rows array");
    assert!(
        rows.iter()
            .any(|row| row["result"] == "suppressed_disabled"),
        "expected a suppressed_disabled write-audit row, got: {body:?}"
    );

    sim.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_value_without_write_scope_is_permission_denied() {
    let app = test_app("write-no-scope").await;
    let sim = Simulator::start().await;
    let (_tag_id, external_name) = make_tag(
        &app,
        "line1",
        sim.addr.port(),
        "temp01",
        "D100",
        "u16",
        true,
        true,
    )
    .await;
    app.write_control.enable();

    let (key, _id) = issue_key(&app.router, &app.admin_token, "reader", &["read"]).await;
    let (_port, mut client) = start_grpc_and_connect(&app.grpc_server).await;

    let err = client
        .write_value(bearer_request(
            WriteValueRequest {
                tag: external_name,
                value: Some(write_value_request::Value::Num(1.0)),
            },
            &key,
        ))
        .await
        .expect_err("a read-only key should not be able to write");
    assert_eq!(err.code(), tonic::Code::PermissionDenied);

    sim.stop();
}

// ---------------------------------------------------------------------------
// 5. 認証: メタデータなし / read キーでの WriteValue 拒否
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_authorization_metadata_is_unauthenticated() {
    let app = test_app("auth-missing").await;
    let (_port, mut client) = start_grpc_and_connect(&app.grpc_server).await;

    let err = client
        .get_catalog(tonic::Request::new(GetCatalogRequest {
            connection: String::new(),
            group: String::new(),
        }))
        .await
        .expect_err("no metadata should be unauthenticated");
    assert_eq!(err.code(), tonic::Code::Unauthenticated);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_token_shaped_bearer_is_rejected_as_unauthenticated() {
    // 設計 §5.4「セッション token は gRPC では受けない」- `bh_` で始まらない
    // 値は(たとえ有効なセッション token であっても)拒否する。
    let app = test_app("auth-session-token").await;
    let (_port, mut client) = start_grpc_and_connect(&app.grpc_server).await;

    let err = client
        .get_catalog(bearer_request(
            GetCatalogRequest {
                connection: String::new(),
                group: String::new(),
            },
            &app.admin_token,
        ))
        .await
        .expect_err("a non-bh_ token should be rejected");
    assert_eq!(err.code(), tonic::Code::Unauthenticated);
}

// ---------------------------------------------------------------------------
// 6. grpc.enabled=false では bind しない、PUT で有効化すると開始
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grpc_disabled_by_default_put_enables_it() {
    let app = test_app("grpc-toggle").await;
    let port = free_port();

    // まだ何も apply していない(既定 disabled) - bind されていないはず。
    assert!(
        std::net::TcpStream::connect_timeout(
            &format!("127.0.0.1:{port}").parse().unwrap(),
            Duration::from_millis(200),
        )
        .is_err(),
        "port should not be listening before enabling gRPC"
    );

    let (status, body) = admin_put(
        &app.router,
        "/api/grpc-settings",
        &app.admin_token,
        json!({ "enabled": true, "port": port }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["enabled"], true);
    assert_eq!(body["port"], port);

    let addr = format!("http://127.0.0.1:{port}");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        if TagServiceClient::connect(addr.clone()).await.is_ok() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "gRPC server should have started listening after PUT /api/grpc-settings"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // GET は保存した設定を読み戻せる。
    let (status, body) = {
        let response = app
            .router
            .clone()
            .oneshot(
                HttpRequest::get("/api/grpc-settings")
                    .header("Authorization", format!("Bearer {}", app.admin_token))
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
        let json: JsonValue = serde_json::from_slice(&bytes).unwrap_or(JsonValue::Null);
        (status, json)
    };
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["enabled"], true);
    assert_eq!(body["port"], port);

    // `/api/v1/status`にも反映される(実装指示「/api/v1/status に
    // grpc: { enabled, port } を追加」)。
    let (key, _id) = issue_key(&app.router, &app.admin_token, "reader", &["read"]).await;
    let response = app
        .router
        .clone()
        .oneshot(
            HttpRequest::get("/api/v1/status")
                .header("Authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let status_json: JsonValue = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(status_json["grpc"]["enabled"], true);
    assert_eq!(status_json["grpc"]["port"], port);
}
