//! T4 の統合テスト（docs/tag-server-design.md §5.4「gRPC（T4）」)。
//!
//! `tests/write.rs`/`tests/stream.rs`/`tests/integration.rs` と同じ理由
//! （各 `tests/*.rs` は独立クレートとしてコンパイルされ、private helper を
//! 共有できない）で `fast_options`/`wait_until`/`issue_key` 等をこのファイル
//! 内に複製している。`TempEnv` は `tests/common/mod.rs` に集約済み
//! （2026-08-08、テスト一時ディレクトリリークの根治）。gRPC クライアントは
//! `tonic` 本体が生成する `TagServiceClient`
//! （`banto_hub_core::grpc::tagserver_v1`）をそのまま使う - 実装指示どおり
//! dev-dependency は追加していない。
//!
//! テスト構成（実装指示のテスト計画1〜6 + H3 の bind 設定化分（2026-08-08
//! オーナー決定、docs/improvement-plan.md H3）に対応）:
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
//! 7. H3: `grpc.bind` の既定は `127.0.0.1`、`PUT` で明示指定した bind が
//!    `GET`/`GrpcServer::apply`に反映される、`bind` 省略は現在値を維持、
//!    不正な bind は 422 で拒否・保存されない、DB に不正値が直接書かれた
//!    状態で `apply` してもプロセスは落ちない

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
use banto_hub_core::settings::SettingsService;
use banto_hub_core::test_output::TestOutputControl;
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

mod common;
use common::TempEnv;

const CLIENT_HEADER: (&str, &str) = ("X-Banto-Client", "banto");

/// Temp-dir prefix passed to `TempEnv::new` (see `tests/common/mod.rs`).
const TEMP_ENV_PREFIX: &str = "banto-hub-grpc-it";

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
    controller: Arc<CollectionController>,
    // T15-3: `controller`が保持するものと同一の `Arc` - `StreamValues`の
    // `test_output=true`をテストから直接有効化/無効化するため。
    test_output: Arc<TestOutputControl>,
    grpc_server: Arc<GrpcServer>,
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
    let test_output = Arc::new(TestOutputControl::new());
    let controller = Arc::new(CollectionController::new(
        manager.clone(),
        write_control.clone(),
        test_output.clone(),
    ));
    let status = controller.start(RunMode::Configured).await;
    assert_eq!(status.state, CollectionState::Running);

    let grpc_service = GrpcService::new(
        manager.clone(),
        api_keys.clone(),
        audit.clone(),
        write_audit.clone(),
        write_control.clone(),
        rate_limiter.clone(),
        events_tx.clone(),
    )
    .with_controller(controller.clone())
    .with_test_output(test_output.clone());
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
        write_control.clone(),
        write_audit,
        mqtt,
        grpc_server.clone(),
        rate_limiter,
        banto_hub_core::profile_paths::DEFAULT_PROFILE_ID.to_string(),
    );

    TestApp {
        router,
        admin_token,
        pool,
        manager,
        write_control,
        controller,
        test_output,
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

/// admin 系ルーター(CSRF ヘッダ必須)への `GET` - `admin_post`/`admin_put`
/// と同じ雛形(H3、2026-08-08 オーナー決定: bind 設定のテストで GET を
/// 複数回使うため、既存の重複していたインライン版から関数化した)。
async fn admin_get(router: &Router, path: &str, token: &str) -> (StatusCode, JsonValue) {
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
            bind: "127.0.0.1".to_string(),
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

/// Reads `stream.message()` in a loop until a batch whose single value
/// equals `expected` arrives, skipping any interim batch that doesn't
/// (H7 ⑤ - see the spurious-stale comment at this helper's call site in
/// `stream_values_sends_initial_snapshot_then_on_change`). Each individual
/// read is itself bounded by the remaining time to `deadline`, so a
/// genuinely stuck stream still fails promptly with a clear message instead
/// of hanging past the overall deadline.
async fn drain_until_value(
    stream: &mut tonic::Streaming<ValueBatch>,
    deadline: tokio::time::Instant,
    expected: f64,
) -> ValueBatch {
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            remaining > Duration::ZERO,
            "timed out waiting for a ValueBatch carrying value {expected} \
             (only saw batches with other values - e.g. spurious quality-only changes)"
        );
        let batch: ValueBatch = tokio::time::timeout(remaining, stream.message())
            .await
            .expect("should receive a batch within the overall deadline")
            .expect("stream should not error")
            .expect("stream should not end");
        assert_eq!(
            batch.values.len(),
            1,
            "expected exactly one value in the batch: {batch:?}"
        );
        if batch.values[0].value == Some(tag_value::Value::Num(expected)) {
            return batch;
        }
        // Not the batch we're waiting for (e.g. a spurious quality-only
        // on_change) - keep draining.
    }
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
        wait_until(Duration::from_secs(10), || async {
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
        wait_until(Duration::from_secs(10), || async {
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
                test_output: false,
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

    // Spurious-stale race (H7 ⑤): quality is derived at *read* time as
    // period(100ms) x STALE_PERIOD_FACTOR(2.5) = 250ms of grace
    // (`banto_collect::current`), not pushed by the collector. The 250ms
    // eval tick (`crate::subscribe_core`, shared with the WS transport -
    // src/stream.rs's module doc) re-derives quality for the *still-10*
    // sample on every tick; under CI scheduling jitter that tick can observe
    // the old sample crossing the staleness threshold (Good -> Stale) before
    // the collector has actually picked up the new value 99 written above.
    // on_change fires on that quality-only transition exactly like it would
    // for a value change, so a spurious `ValueBatch` (value still 10, only
    // quality differs) can arrive on the stream before the real 10 -> 99
    // one. A single `stream.message()` read right after `set_word` is
    // therefore not reliable - drain until the batch that actually carries
    // 99 arrives, skipping any interim quality-only batches, bounded by an
    // overall deadline so a truly stuck stream still fails.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    let changed = drain_until_value(&mut stream, deadline, 99.0).await;
    assert_eq!(changed.values.len(), 1);
    assert_eq!(changed.values[0].value, Some(tag_value::Value::Num(99.0)));

    sim.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_values_ends_when_all_simulation_starts() {
    let app = test_app("stream-values-all-simulation").await;
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
        wait_until(Duration::from_secs(10), || async {
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
                test_output: false,
            },
            &key,
        ))
        .await
        .expect("stream_values should succeed before all-simulation")
        .into_inner();
    assert!(
        stream
            .message()
            .await
            .expect("initial stream read should not error")
            .is_some(),
        "configured stream should send an initial batch"
    );

    let status = app.controller.start(RunMode::AllSimulation).await;
    assert_eq!(status.state, CollectionState::Running);
    assert_eq!(status.mode, RunMode::AllSimulation);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            remaining > Duration::ZERO,
            "stream should end after all-simulation starts"
        );
        let next = tokio::time::timeout(remaining, stream.message())
            .await
            .expect("stream should end after all-simulation starts")
            .expect("stream termination should not be a gRPC error");
        if next.is_none() {
            break;
        }
        // A batch already queued just before the lifecycle notification may
        // still drain; the stream must close before any later tick can emit.
    }

    let err = client
        .stream_values(bearer_request(
            StreamValuesRequest {
                tags: vec![external_name],
                mode: SubscribeMode::OnChange as i32,
                interval_ms: 0,
                test_output: false,
            },
            &key,
        ))
        .await
        .expect_err("new normal stream must be disabled during all-simulation");
    assert_eq!(err.code(), tonic::Code::Unavailable);
    assert_eq!(err.message(), "simulation_output_disabled");

    sim.stop();
}

// ---------------------------------------------------------------------------
// T15-3: StreamValues(test_output=true) - 専用 stream namespace
// ---------------------------------------------------------------------------

/// `test_output=true`は`TestOutputControl`が現在の run_id に対して明示的に
/// `enable`されるまで honored されない - `Stopped`でも`AllSimulation`の
/// `Running`でも、`enable`前は常に`test_output_disabled`（`FAILED_PRECONDITION`）
/// で拒否される（`crate::grpc::stream_values`の doc comment「T15-3」参照）。
/// `enable`後は初回バッチに`simulation=true`・一致する`run_id`が乗る。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_values_test_output_requires_enabling_before_it_is_honored() {
    let app = test_app("stream-values-test-output-gate").await;
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

    let (key, _id) = issue_key(&app.router, &app.admin_token, "reader", &["read"]).await;
    let (_port, mut client) = start_grpc_and_connect(&app.grpc_server).await;

    // Stopped(既定) + test_output=true -> 拒否。
    let err = client
        .stream_values(bearer_request(
            StreamValuesRequest {
                tags: vec![external_name.clone()],
                mode: SubscribeMode::OnChange as i32,
                interval_ms: 0,
                test_output: true,
            },
            &key,
        ))
        .await
        .expect_err("test_output=true without an active TestOutputControl must be rejected");
    assert_eq!(err.code(), tonic::Code::FailedPrecondition);
    assert_eq!(err.message(), "test_output_disabled");

    // AllSimulation を起動しても、明示的に enable するまでは依然拒否
    // (`TestOutputControl`自身は mode を見ない - REST の
    // `POST /api/test-output/enable`が有効化時に前提条件を検査する側
    // であって、gRPC 側のゲートは`is_active_for`だけを見る設計 -
    // `crate::grpc::test_output_active_run_id`の doc comment参照)。
    let status = app.controller.start(RunMode::AllSimulation).await;
    assert_eq!(status.state, CollectionState::Running);
    let run_id = status
        .run_id
        .expect("AllSimulation run should have a run_id");

    let err = client
        .stream_values(bearer_request(
            StreamValuesRequest {
                tags: vec![external_name.clone()],
                mode: SubscribeMode::OnChange as i32,
                interval_ms: 0,
                test_output: true,
            },
            &key,
        ))
        .await
        .expect_err("test_output=true is still rejected until explicitly enabled");
    assert_eq!(err.code(), tonic::Code::FailedPrecondition);
    assert_eq!(err.message(), "test_output_disabled");

    // 通常 stream（test_output=false）は既存 PR #95 挙動どおり
    // `AllSimulation`中は拒否されたままであること(回帰確認)。
    let err = client
        .stream_values(bearer_request(
            StreamValuesRequest {
                tags: vec![external_name.clone()],
                mode: SubscribeMode::OnChange as i32,
                interval_ms: 0,
                test_output: false,
            },
            &key,
        ))
        .await
        .expect_err("normal stream must remain disabled during all-simulation");
    assert_eq!(err.code(), tonic::Code::Unavailable);
    assert_eq!(err.message(), "simulation_output_disabled");

    // enable() すれば通り、バッチに simulation=true・一致する run_id が乗る。
    app.test_output.enable(run_id);
    let mut stream = client
        .stream_values(bearer_request(
            StreamValuesRequest {
                tags: vec![external_name],
                mode: SubscribeMode::OnChange as i32,
                interval_ms: 0,
                test_output: true,
            },
            &key,
        ))
        .await
        .expect("test_output=true should succeed once TestOutputControl is armed for this run_id")
        .into_inner();
    let batch = stream
        .message()
        .await
        .expect("stream should not error")
        .expect("initial snapshot should be sent");
    assert!(
        batch.simulation,
        "test-output batches must set simulation=true"
    );
    assert_eq!(batch.run_id, Some(run_id));

    sim.stop();
}

/// テスト出力 stream は、明示的な`disable`（設計「明示操作でも無効化」）でも
/// 収集停止（設計「停止／終了／切替後に必ず無効へ戻る」・
/// `CollectionController::stop_locked`が`test_output.disable()`する）でも
/// 終了する - どちらも`TestOutputControl::is_active_for`をこのバッチの
/// `run_id`に対して false にする、という同じ経路（`crate::grpc::stream_values`
/// の spawn したタスクの doc comment「T15-3」参照）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_values_test_output_ends_when_disabled_or_collection_stops() {
    for scenario in ["explicit_disable", "collection_stop"] {
        let app = test_app(&format!("stream-values-test-output-end-{scenario}")).await;
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

        let status = app.controller.start(RunMode::AllSimulation).await;
        let run_id = status
            .run_id
            .expect("AllSimulation run should have a run_id");
        app.test_output.enable(run_id);

        let (key, _id) = issue_key(&app.router, &app.admin_token, "reader", &["read"]).await;
        let (_port, mut client) = start_grpc_and_connect(&app.grpc_server).await;

        let mut stream = client
            .stream_values(bearer_request(
                StreamValuesRequest {
                    tags: vec![external_name],
                    mode: SubscribeMode::OnChange as i32,
                    interval_ms: 0,
                    test_output: true,
                },
                &key,
            ))
            .await
            .expect("test_output stream should succeed while armed")
            .into_inner();

        let initial = stream
            .message()
            .await
            .expect("stream should not error")
            .expect("initial snapshot should be sent");
        assert!(initial.simulation);
        assert_eq!(initial.run_id, Some(run_id));

        match scenario {
            "explicit_disable" => app.test_output.disable(),
            "collection_stop" => {
                app.controller.stop().await;
            }
            other => unreachable!("unexpected scenario: {other}"),
        }

        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            assert!(
                remaining > Duration::ZERO,
                "test-output stream should end after {scenario}"
            );
            let next = tokio::time::timeout(remaining, stream.message())
                .await
                .unwrap_or_else(|_| panic!("test-output stream should end after {scenario}"))
                .expect("stream termination should not be a gRPC error");
            if next.is_none() {
                break;
            }
        }

        sim.stop();
    }
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
                test_output: false,
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
        wait_until(Duration::from_secs(10), || async {
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
        wait_until(Duration::from_secs(10), || async {
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
    let (status, body) = admin_get(&app.router, "/api/grpc-settings", &app.admin_token).await;
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

// ---------------------------------------------------------------------------
// 7. bind 設定(H3、2026-08-08 オーナー決定、docs/improvement-plan.md H3):
//    既定 127.0.0.1、PUT で明示指定した bind が GET/apply に反映される、
//    省略すると現在値を維持する、不正な bind は 422 で拒否される、DB に
//    直接不正値が書かれていても apply がプロセスを落とさない
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grpc_settings_default_bind_is_loopback() {
    let app = test_app("grpc-bind-default").await;
    let (status, body) = admin_get(&app.router, "/api/grpc-settings", &app.admin_token).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["bind"], "127.0.0.1");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grpc_settings_put_bind_is_reflected_in_get() {
    let app = test_app("grpc-bind-put-get").await;
    let port = free_port();

    // enabled: false なので実際に bind は試みない - ローカルに存在しない
    // IP でも安全に「設定として保存されるか」だけを検証できる。
    let (status, body) = admin_put(
        &app.router,
        "/api/grpc-settings",
        &app.admin_token,
        json!({ "enabled": false, "bind": "10.20.30.40", "port": port }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["bind"], "10.20.30.40");

    let (status, body) = admin_get(&app.router, "/api/grpc-settings", &app.admin_token).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["bind"], "10.20.30.40");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grpc_settings_put_without_bind_keeps_existing_value() {
    let app = test_app("grpc-bind-omit").await;
    let port1 = free_port();
    let port2 = free_port();

    let (status, body) = admin_put(
        &app.router,
        "/api/grpc-settings",
        &app.admin_token,
        json!({ "enabled": false, "bind": "10.20.30.41", "port": port1 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["bind"], "10.20.30.41");

    // 2回目の PUT では `bind` キー自体を送らない(`None` = 現在値を維持、
    // `GrpcSettingsBody::bind`のdoc comment参照)。`port` だけが変わり、
    // `bind` は直前の値のままのはず。
    let (status, body) = admin_put(
        &app.router,
        "/api/grpc-settings",
        &app.admin_token,
        json!({ "enabled": false, "port": port2 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["bind"], "10.20.30.41");
    assert_eq!(body["port"], port2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grpc_settings_put_invalid_bind_is_rejected_and_not_persisted() {
    let app = test_app("grpc-bind-invalid").await;
    let port = free_port();

    let (status, body) = admin_put(
        &app.router,
        "/api/grpc-settings",
        &app.admin_token,
        json!({ "enabled": false, "bind": "abc", "port": port }),
    )
    .await;
    // `BantoError::Validation` は他の admin 設定 PUT(例: `PUT
    // /api/mqtt-settings`の`put_mqtt_settings_rejects_qos_2_and_enabling_without_a_host`、
    // `tests/mqtt.rs`参照)と同じく 422 に写像される。
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    assert_eq!(body["kind"], "validation");
    assert_eq!(body["field_errors"][0]["field"], "bind");

    // 拒否された PUT は保存されていない - GET は既定値(127.0.0.1)のまま。
    let (status, body) = admin_get(&app.router, "/api/grpc-settings", &app.admin_token).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["bind"], "127.0.0.1");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grpc_apply_with_invalid_bind_does_not_crash_and_leaves_port_unbound() {
    let app = test_app("grpc-invalid-bind-apply").await;
    let port = free_port();

    // `GrpcServer::apply`を直接、DB 経由の PUT バリデーションを迂回して
    // 呼ぶ - 「DB に不正な文字列が直接書き込まれた場合」(既存 DB を手で
    // 触った、将来のマイグレーション不備等)を模す(`GrpcServer::apply`の
    // doc comment参照)。panic せず、ただ起動しないだけであることを
    // 確認する - この関数自体が最後まで実行できていること自体が
    // 「プロセスが落ちない」ことの証拠になる。
    app.grpc_server
        .apply(&GrpcSettings {
            enabled: true,
            bind: "not-an-ip-address".to_string(),
            port,
        })
        .await;

    assert!(
        std::net::TcpStream::connect_timeout(
            &format!("127.0.0.1:{port}").parse().unwrap(),
            Duration::from_millis(200),
        )
        .is_err(),
        "invalid bind should not have started listening"
    );

    // 続けて有効な設定を apply しても正常に動く - `running` の状態が
    // 壊れたまま残っていないことを確認する。
    let (_port, mut client) = start_grpc_and_connect(&app.grpc_server).await;
    let err = client
        .get_catalog(tonic::Request::new(GetCatalogRequest {
            connection: String::new(),
            group: String::new(),
        }))
        .await
        .expect_err("no metadata should still be unauthenticated after recovering");
    assert_eq!(err.code(), tonic::Code::Unauthenticated);
}

// ---------------------------------------------------------------------------
// 8. H10 ③(Option B、docs/h10-3-read-scope-proposal.md §5・§6): per-tag
//    read スコープ。GetCatalog は絞らない(素の read/read:{tag} いずれでも
//    全タグ)。ReadValues/StreamValues はスコープ外を除く。
// ---------------------------------------------------------------------------

/// このセクション共通のフィクスチャ: `line1.fast.temp01`(tag:1)・
/// `line2.slow.press01`(tag:2)を、同一シミュレータの別アドレスに割り当てて
/// 別接続・別グループで作り rebuild する。`make_tag`はグループ名を
/// `"fast"`固定で作るため2回呼ぶとグループ名の `UNIQUE` 制約に衝突する -
/// ここでは`slmp_conn_input`/`group_input`/`tag_input`を直接使い、2本目の
/// グループ名を`"slow"`にして衝突を避ける(`make_tag`自体は既存の呼び出し元
/// 全てが1テストにつき1回しか呼ばないため、シグネチャは変更しない)。戻り値
/// は `(line1.fast.temp01, line2.slow.press01)` の外部名。
async fn seed_two_connections_two_tags(app: &TestApp, sim_port: u16) -> (String, String) {
    let conn1 = PlcConnectionService::new(app.pool.clone())
        .create(slmp_conn_input("line1", sim_port))
        .await
        .unwrap();
    let group1 = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn1.id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("temp01", group1.id, "D100", "u16", false, true)) // tag:1
        .await
        .unwrap();

    let conn2 = PlcConnectionService::new(app.pool.clone())
        .create(slmp_conn_input("line2", sim_port))
        .await
        .unwrap();
    let group2 = CollectionGroupService::new(app.pool.clone())
        .create(group_input("slow", conn2.id, 1000))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("press01", group2.id, "D200", "u16", false, true)) // tag:2
        .await
        .unwrap();

    app.manager.rebuild().await.expect("rebuild after seeding");
    (
        "line1.fast.temp01".to_string(),
        "line2.slow.press01".to_string(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_catalog_with_a_read_colon_key_still_returns_every_tag() {
    let app = test_app("h10-3-catalog").await;
    let sim = Simulator::start().await;

    let (name1, name2) = seed_two_connections_two_tags(&app, sim.addr.port()).await;

    let scope = format!("read:{name1}");
    let (key, _id) = issue_key(
        &app.router,
        &app.admin_token,
        "line1-reader",
        &[scope.as_str()],
    )
    .await;
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
        .expect("get_catalog should succeed with any read scope (Option B)")
        .into_inner();

    let names: Vec<&str> = catalog
        .tags
        .iter()
        .map(|t| t.external_name.as_str())
        .collect();
    assert!(names.contains(&name1.as_str()));
    assert!(
        names.contains(&name2.as_str()),
        "catalog must stay unfiltered even for a per-tag-scoped key (Option B): {names:?}"
    );

    sim.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_values_with_a_read_colon_key_is_limited_to_the_in_scope_tag() {
    let app = test_app("h10-3-read-values").await;
    let sim = Simulator::start().await;
    sim.set_word(SlmpDevice::D, 100, 111);
    sim.set_word(SlmpDevice::D, 200, 222);

    let (name1, name2) = seed_two_connections_two_tags(&app, sim.addr.port()).await;

    assert!(
        wait_until(Duration::from_secs(10), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get("tag:1"))
                .and_then(|s| s.value)
                .is_some()
                && app
                    .manager
                    .current_values()
                    .and_then(|c| c.get("tag:2"))
                    .and_then(|s| s.value)
                    .is_some()
        })
        .await,
        "collector should have picked up both seeded values"
    );

    let scope = format!("read:{name1}");
    let (key, _id) = issue_key(
        &app.router,
        &app.admin_token,
        "line1-reader",
        &[scope.as_str()],
    )
    .await;
    let (_port, mut client) = start_grpc_and_connect(&app.grpc_server).await;

    // 明示指定: 自分のタグは読める。
    let values = client
        .read_values(bearer_request(
            ReadValuesRequest {
                tags: vec![name1.clone()],
            },
            &key,
        ))
        .await
        .expect("read_values for the in-scope tag should succeed")
        .into_inner();
    assert_eq!(values.values.len(), 1);
    assert_eq!(values.values[0].value, Some(tag_value::Value::Num(111.0)));

    // 明示指定: スコープ外を挙げたら PERMISSION_DENIED(REST の 403 に対応)。
    let err = client
        .read_values(bearer_request(
            ReadValuesRequest {
                tags: vec![name2.clone()],
            },
            &key,
        ))
        .await
        .expect_err("an out-of-scope explicit tag should be denied");
    assert_eq!(err.code(), tonic::Code::PermissionDenied);

    // 暗黙(tags 省略、全件相当): スコープ外は黙って除かれる。
    let values = client
        .read_values(bearer_request(ReadValuesRequest { tags: vec![] }, &key))
        .await
        .expect("read_values with tags omitted should succeed")
        .into_inner();
    assert_eq!(values.values.len(), 1);
    assert_eq!(values.values[0].tag, name1);

    sim.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_values_with_a_read_colon_key_only_resolves_the_in_scope_tag() {
    let app = test_app("h10-3-stream-values").await;
    let sim = Simulator::start().await;
    sim.set_word(SlmpDevice::D, 100, 10);
    sim.set_word(SlmpDevice::D, 200, 20);

    let (name1, _name2) = seed_two_connections_two_tags(&app, sim.addr.port()).await;

    assert!(
        wait_until(Duration::from_secs(10), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get("tag:1"))
                .and_then(|s| s.value)
                .is_some()
                && app
                    .manager
                    .current_values()
                    .and_then(|c| c.get("tag:2"))
                    .and_then(|s| s.value)
                    .is_some()
        })
        .await,
        "collector should have picked up both seeded values"
    );

    let scope = format!("read:{name1}");
    let (key, _id) = issue_key(
        &app.router,
        &app.admin_token,
        "line1-reader",
        &[scope.as_str()],
    )
    .await;
    let (_port, mut client) = start_grpc_and_connect(&app.grpc_server).await;

    let mut stream = client
        .stream_values(bearer_request(
            StreamValuesRequest {
                tags: vec!["*".to_string()],
                mode: SubscribeMode::OnChange as i32,
                interval_ms: 0,
                test_output: false,
            },
            &key,
        ))
        .await
        .expect("wildcard stream_values should succeed")
        .into_inner();

    let initial: ValueBatch = stream
        .message()
        .await
        .expect("stream should not error")
        .expect("initial snapshot should be sent");
    assert_eq!(
        initial.values.len(),
        1,
        "wildcard subscription must resolve only the in-scope tag: {:?}",
        initial.values
    );
    assert_eq!(initial.values[0].tag, name1);

    // スコープ外の値変更ではストリームに何も届かない(タイムアウトで確認 -
    // `tests/stream.rs`の`assert_no_more_data_for`と同じ意図)。
    sim.set_word(SlmpDevice::D, 200, 999);
    let silence = tokio::time::timeout(Duration::from_millis(600), stream.message()).await;
    assert!(
        silence.is_err(),
        "an out-of-scope tag change must not produce a ValueBatch: {silence:?}"
    );

    // スコープ内の値変更は引き続き届く。旧値(10)を載せた spurious な
    // quality-only on_change バッチが先に届きうるレース(H7 ⑤ - grpc.rs の
    // `drain_until_value` / `stream_values_sends_initial_snapshot_then_on_change`
    // 参照)を、値 77 を載せたバッチが来るまで drain して吸収する。全体
    // deadline で truly-stuck なストリームは依然として明確に失敗する。
    sim.set_word(SlmpDevice::D, 100, 77);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    let changed = drain_until_value(&mut stream, deadline, 77.0).await;
    assert_eq!(changed.values.len(), 1);
    assert_eq!(changed.values[0].tag, name1);

    sim.stop();
}
