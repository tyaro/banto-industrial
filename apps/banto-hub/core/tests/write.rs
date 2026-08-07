//! T2-4 の統合テスト（docs/tag-server-design.md §6 全体）: `banto-plc-write`
//! の読み書き両対応シミュレータ + `banto-broker` 経由での
//! `POST /api/v1/values/{tag}` 安全ゲート一式の E2E。
//!
//! `tests/integration.rs`/`tests/stream.rs` と同じ理由（各 `tests/*.rs` は
//! 独立したクレートとしてコンパイルされ、private helper を共有できない）で
//! `TempEnv`/`fast_options`/`wait_until` 相当をこのファイル内に複製している。
//!
//! テスト構成（実装指示 §6 のテスト計画1〜4に対応。5「再起動安全」は
//! `banto_hub_core::write_control` 自身の単体テスト
//! （`a_new_write_control_from_a_persisted_enabled_state_is_disabled` 等）で
//! 既に確認済みなのでここでは重複させない）:
//! 1. E2E ハッピーパス
//! 2. ゲート網羅
//! 3. レート制限（タグ毎・全体）
//! 4. log-before-write（成功/失敗）

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
        // remove_dir_all は Windows では SQLite 接続がハンドルを解放し切る前に
        // 呼ばれて失敗することがあり、その場合ディレクトリが残り続ける。PID
        // だけでは再利用時に古い(既に初期化済みの)ディレクトリと衝突しうる
        // ため、ナノ秒精度のタイムスタンプも一意性キーに含める。
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "banto-hub-write-it-{}-{label}-{id}-{nanos}",
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

fn modbus_conn_input(name: &str, port: u16) -> PlcConnectionInput {
    PlcConnectionInput {
        protocol: "modbus-tcp".to_string(),
        ..slmp_conn_input(name, port)
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

struct TestApp {
    router: Router,
    admin_token: String,
    pool: SqlitePool,
    manager: Arc<CollectorManager>,
    write_control: Arc<WriteControl>,
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
    // T2-4 (docs/tag-server-design.md §6-6): live flag always constructs
    // disabled, regardless of persisted state - each test explicitly calls
    // `write_control.enable()` when it needs writes accepted (or exercises
    // the REST enable endpoint directly, see `rest_enable_disable_round_trip`).
    let write_control = Arc::new(WriteControl::new(false));
    let write_audit = WriteAuditService::new(pool.clone());
    let mqtt = Arc::new(banto_hub_core::mqtt::MqttPublisher::new(manager.clone()));
    let api_keys = ApiKeysService::new(pool.clone());
    // T4: `tests/grpc.rs` が gRPC 経由の書き込みゲートを検証するので、この
    // ファイルは REST 経路のみ - ただし `api_router` の T4 引数（REST/gRPC
    // で共有する rate_limiter・`GrpcServer`)は必須のため構築だけする
    // (`apply`は呼ばない = listen しない)。
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
        write_control.clone(),
        write_audit,
        mqtt,
        grpc_server,
        rate_limiter,
    );

    TestApp {
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

/// admin 管理系エンドポイント用（CSRF ヘッダ必須）。
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

/// `/api/v1/*` 用（CSRF ヘッダ不要 - 設計 §5.1/§5.6）。`bearer` は `bh_...`
/// API キーでもセッション token でもよい。
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

/// admin 管理系エンドポイント用の `POST`（body なし、まだ CSRF は必要）。
async fn admin_post_empty(router: &Router, path: &str, token: &str) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            HttpRequest::post(path)
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
/// external_name)` を返す共通フィクスチャ。
#[allow(clippy::too_many_arguments)]
async fn make_tag(
    app: &TestApp,
    conn_name: &str,
    protocol: &str,
    port: u16,
    tag_name: &str,
    address: &str,
    data_type: &str,
    writable: bool,
    enabled: bool,
) -> (i64, String) {
    let conn_input = if protocol == "slmp" {
        slmp_conn_input(conn_name, port)
    } else {
        modbus_conn_input(conn_name, port)
    };
    let conn = PlcConnectionService::new(app.pool.clone())
        .create(conn_input)
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

// ---------------------------------------------------------------------------
// 1. E2E ハッピーパス: writable SLMP タグへ write スコープキーで書き込み ->
//    シミュレータに値が届く -> 収集で読み戻して /api/v1/values に反映される
//    (読み書き単一セッションの実証)。
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_write_then_collection_reads_the_value_back_through_the_same_broker_session() {
    let app = test_app("e2e-happy").await;
    let sim = Simulator::start().await;

    let (tag_id, external_name) = make_tag(
        &app,
        "line1",
        "slmp",
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

    let (status, body) = v1_post(
        &app.router,
        &format!("/api/v1/values/{external_name}"),
        &key,
        json!({ "v": 1234 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["tag"], external_name);
    assert_eq!(body["result"], "ok");

    assert_eq!(
        sim.get_word(SlmpDevice::D, 100),
        1234,
        "value must land on the wire"
    );

    // 読み書き単一セッションの実証: 収集(banto-collect, broker 経由読み取り)
    // が同じセッションで値を読み戻す。
    let tag_key = format!("tag:{tag_id}");
    assert!(
        wait_until(Duration::from_secs(3), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get(&tag_key))
                .map(|s| s.value)
                == Some(Some(1234.0))
        })
        .await,
        "collection should read back the value the write endpoint just wrote"
    );

    let (status, json) = get_json(
        &app.router,
        &format!("/api/v1/values/{external_name}"),
        &app.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["v"], 1234.0);
    assert_eq!(json["q"], "good");

    sim.stop();
}

// ---------------------------------------------------------------------------
// 2. ゲート網羅
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gate_undefined_tag_is_404() {
    let app = test_app("gate-404").await;
    app.write_control.enable();
    let (key, _id) = issue_key(
        &app.router,
        &app.admin_token,
        "writer",
        &["write:nope.nope.nope"],
    )
    .await;

    let (status, _body) = v1_post(
        &app.router,
        "/api/v1/values/nope.nope.nope",
        &key,
        json!({ "v": 1 }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gate_writable_false_is_403_not_writable() {
    let app = test_app("gate-not-writable").await;
    let sim = Simulator::start().await;
    let (_tag_id, external_name) = make_tag(
        &app,
        "line1",
        "slmp",
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

    let (status, body) = v1_post(
        &app.router,
        &format!("/api/v1/values/{external_name}"),
        &key,
        json!({ "v": 1 }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "not_writable");
    sim.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gate_disabled_tag_is_409_tag_disabled() {
    let app = test_app("gate-disabled").await;
    let sim = Simulator::start().await;
    let (_tag_id, external_name) = make_tag(
        &app,
        "line1",
        "slmp",
        sim.addr.port(),
        "temp01",
        "D100",
        "u16",
        true,
        false,
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

    let (status, body) = v1_post(
        &app.router,
        &format!("/api/v1/values/{external_name}"),
        &key,
        json!({ "v": 1 }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "tag_disabled");
    sim.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gate_modbus_connection_is_501_write_unsupported_protocol() {
    let app = test_app("gate-modbus").await;
    let (_tag_id, external_name) = make_tag(
        &app,
        "line1",
        "modbus-tcp",
        15099,
        "temp01",
        "40001",
        "i16",
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

    let (status, body) = v1_post(
        &app.router,
        &format!("/api/v1/values/{external_name}"),
        &key,
        json!({ "v": 1 }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(body["error"], "write_unsupported_protocol");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gate_writes_disabled_is_503_and_audited() {
    let app = test_app("gate-writes-disabled").await;
    let sim = Simulator::start().await;
    let (tag_id, external_name) = make_tag(
        &app,
        "line1",
        "slmp",
        sim.addr.port(),
        "temp01",
        "D100",
        "u16",
        true,
        true,
    )
    .await;
    // write_control は既定 disabled のまま(app.write_control.enable() を呼ばない)。
    let (key, key_id) = issue_key(
        &app.router,
        &app.admin_token,
        "writer",
        &["write:line1.fast.temp01"],
    )
    .await;

    let (status, body) = v1_post(
        &app.router,
        &format!("/api/v1/values/{external_name}"),
        &key,
        json!({ "v": 1 }),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"], "writes_disabled");

    let (status, listed) = admin_post(
        &app.router,
        "/api/write-audit/list",
        &app.admin_token,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let rows = listed["rows"].as_array().unwrap();
    let row = rows
        .iter()
        .find(|r| r["tagId"].as_i64() == Some(tag_id) && r["apiKeyId"].as_i64() == Some(key_id))
        .expect("a hub_write_audit row should exist");
    assert_eq!(row["action"], "write");
    assert_eq!(row["result"], "suppressed_disabled");
    sim.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gate_read_only_scope_key_cannot_write() {
    let app = test_app("gate-read-only").await;
    let sim = Simulator::start().await;
    let (_tag_id, external_name) = make_tag(
        &app,
        "line1",
        "slmp",
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

    let (status, body) = v1_post(
        &app.router,
        &format!("/api/v1/values/{external_name}"),
        &key,
        json!({ "v": 1 }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "missing_write_scope");
    sim.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gate_session_token_cannot_write() {
    let app = test_app("gate-session-token").await;
    let sim = Simulator::start().await;
    let (_tag_id, external_name) = make_tag(
        &app,
        "line1",
        "slmp",
        sim.addr.port(),
        "temp01",
        "D100",
        "u16",
        true,
        true,
    )
    .await;
    app.write_control.enable();

    let (status, body) = v1_post(
        &app.router,
        &format!("/api/v1/values/{external_name}"),
        &app.admin_token,
        json!({ "v": 1 }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "session_token_cannot_write");
    sim.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gate_mismatched_write_scope_is_403() {
    let app = test_app("gate-mismatch").await;
    let sim = Simulator::start().await;
    let (_tag_id, external_name) = make_tag(
        &app,
        "line1",
        "slmp",
        sim.addr.port(),
        "temp01",
        "D100",
        "u16",
        true,
        true,
    )
    .await;
    app.write_control.enable();
    // このキーは別タグへの write スコープしか持たない。
    let (key, _id) = issue_key(
        &app.router,
        &app.admin_token,
        "writer",
        &["write:line1.fast.other"],
    )
    .await;

    let (status, body) = v1_post(
        &app.router,
        &format!("/api/v1/values/{external_name}"),
        &key,
        json!({ "v": 1 }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "missing_write_scope");
    sim.stop();
}

/// 監査レビュー指摘(2026-08-06)への対応: gate 7 は data_type と
/// リクエスト値の型の対称性を検査する - 暗黙の型変換はしない
/// (`crate::write_path`のモジュール doc comment 参照)。bit タグへ数値を
/// 書く要求(旧実装は `raw != 0.0` で暗黙に bool 化していた)は 422。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gate_numeric_value_to_a_bit_tag_is_422() {
    let app = test_app("gate-num-to-bit").await;
    let sim = Simulator::start().await;
    let (_tag_id, external_name) = make_tag(
        &app,
        "line1",
        "slmp",
        sim.addr.port(),
        "flag",
        "M50",
        "bit",
        true,
        true,
    )
    .await;
    app.write_control.enable();
    let (key, _id) = issue_key(
        &app.router,
        &app.admin_token,
        "writer",
        &["write:line1.fast.flag"],
    )
    .await;

    let (status, body) = v1_post(
        &app.router,
        &format!("/api/v1/values/{external_name}"),
        &key,
        json!({ "v": 1 }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    assert_eq!(body["error"], "unsupported_value_type");
    assert_eq!(body["detail"], "bit タグには true/false を指定してください");
    sim.stop();
}

/// 上のペア: 数値タグへ真偽値を書く要求も同様に 422(型の対称性)。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gate_bool_value_to_a_numeric_tag_is_422() {
    let app = test_app("gate-bool-to-num").await;
    let sim = Simulator::start().await;
    let (_tag_id, external_name) = make_tag(
        &app,
        "line1",
        "slmp",
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

    let (status, body) = v1_post(
        &app.router,
        &format!("/api/v1/values/{external_name}"),
        &key,
        json!({ "v": true }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    assert_eq!(body["error"], "unsupported_value_type");
    assert_eq!(
        body["detail"],
        "数値タグに真偽値は指定できません。数値を指定してください"
    );
    sim.stop();
}

/// bit タグへ真偽値を書く要求は引き続き成功する(型が一致する正常系)。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gate_bool_value_to_a_bit_tag_is_ok() {
    let app = test_app("gate-bool-to-bit").await;
    let sim = Simulator::start().await;
    let (_tag_id, external_name) = make_tag(
        &app,
        "line1",
        "slmp",
        sim.addr.port(),
        "flag",
        "M50",
        "bit",
        true,
        true,
    )
    .await;
    app.write_control.enable();
    let (key, _id) = issue_key(
        &app.router,
        &app.admin_token,
        "writer",
        &["write:line1.fast.flag"],
    )
    .await;

    let (status, body) = v1_post(
        &app.router,
        &format!("/api/v1/values/{external_name}"),
        &key,
        json!({ "v": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"], "ok");
    assert!(sim.get_bit(SlmpDevice::M, 50), "the bit should be set");
    sim.stop();
}

// ---------------------------------------------------------------------------
// 3. レート制限
// ---------------------------------------------------------------------------

/// per_tag 超過(既定 10) -> 429 + キーが tripped -> 以後 read も 403
/// key_tripped -> admin が clear-trip -> 復帰。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn per_tag_rate_limit_trips_the_key_and_admin_clear_trip_recovers() {
    let app = test_app("rate-per-tag").await;
    let sim = Simulator::start().await;
    let (_tag_id, external_name) = make_tag(
        &app,
        "line1",
        "slmp",
        sim.addr.port(),
        "temp01",
        "D100",
        "u16",
        true,
        true,
    )
    .await;
    app.write_control.enable();
    let (key, key_id) = issue_key(
        &app.router,
        &app.admin_token,
        "writer",
        &["write:line1.fast.temp01", "read"],
    )
    .await;

    // 既定 per_tag_max=10: 最初の10件は通る。
    for i in 0..10 {
        let (status, body) = v1_post(
            &app.router,
            &format!("/api/v1/values/{external_name}"),
            &key,
            json!({ "v": i }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "write {i} should succeed: {body:?}");
    }

    // 11件目は超過 -> 429 + トリップ。
    let (status, body) = v1_post(
        &app.router,
        &format!("/api/v1/values/{external_name}"),
        &key,
        json!({ "v": 999 }),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body:?}");
    assert_eq!(body["error"], "rate_limited");

    // 以後、read スコープも持つ同じキーで /api/v1/tags を読もうとしても
    // 403 key_tripped(read/write いずれも拒否)。
    let (status, body) = get_json(&app.router, "/api/v1/tags", &key).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "key_tripped");

    // admin が clear-trip すると復帰する。
    let (status, cleared) = admin_post_empty(
        &app.router,
        &format!("/api/api-keys/{key_id}/clear-trip"),
        &app.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{cleared:?}");
    assert!(cleared["trippedAt"].is_null());

    let (status, _body) = get_json(&app.router, "/api/v1/tags", &key).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "key should read again after clear-trip"
    );

    sim.stop();
}

/// global_max(既定 30)超過: 3タグに10件ずつ(各タグの per_tag_max=10 は
/// 超えない)書き込んだ後、4本目のタグへの1件目が global cap で弾かれる
/// ことを確認する(per_tag ではなく global が効いていることの証明)。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn global_rate_limit_trips_across_tags() {
    let app = test_app("rate-global").await;
    let sim = Simulator::start().await;

    let conn = PlcConnectionService::new(app.pool.clone())
        .create(slmp_conn_input("line1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    let tags = TagService::new(app.pool.clone());
    for (name, addr) in [("a", "D100"), ("b", "D101"), ("c", "D102"), ("d", "D103")] {
        tags.create(tag_input(name, group.id, addr, "u16", true, true))
            .await
            .unwrap();
    }
    app.manager.rebuild().await.expect("rebuild");
    app.write_control.enable();

    let (key, _id) = issue_key(
        &app.router,
        &app.admin_token,
        "writer",
        &[
            "write:line1.fast.a",
            "write:line1.fast.b",
            "write:line1.fast.c",
            "write:line1.fast.d",
        ],
    )
    .await;

    // 3タグ x 10件 = 30件(global_max ちょうど)、いずれも per_tag_max(10)
    // 以内なので個別には超過しない。
    for tag_name in ["a", "b", "c"] {
        for i in 0..10 {
            let (status, body) = v1_post(
                &app.router,
                &format!("/api/v1/values/line1.fast.{tag_name}"),
                &key,
                json!({ "v": i }),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{tag_name}#{i}: {body:?}");
        }
    }

    // 4本目のタグは初めての書き込みだが、global が既に30に達しているので
    // 429 になる。
    let (status, body) = v1_post(
        &app.router,
        "/api/v1/values/line1.fast.d",
        &key,
        json!({ "v": 1 }),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body:?}");
    assert_eq!(body["error"], "rate_limited");

    sim.stop();
}

// ---------------------------------------------------------------------------
// 4. log-before-write
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn successful_write_leaves_an_ok_audit_row() {
    let app = test_app("audit-ok").await;
    let sim = Simulator::start().await;
    let (tag_id, external_name) = make_tag(
        &app,
        "line1",
        "slmp",
        sim.addr.port(),
        "temp01",
        "D100",
        "u16",
        true,
        true,
    )
    .await;
    app.write_control.enable();
    let (key, key_id) = issue_key(
        &app.router,
        &app.admin_token,
        "writer",
        &["write:line1.fast.temp01"],
    )
    .await;

    let (status, _body) = v1_post(
        &app.router,
        &format!("/api/v1/values/{external_name}"),
        &key,
        json!({ "v": 42 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, listed) = admin_post(
        &app.router,
        "/api/write-audit/list",
        &app.admin_token,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let rows = listed["rows"].as_array().unwrap();
    let row = rows
        .iter()
        .find(|r| r["tagId"].as_i64() == Some(tag_id) && r["apiKeyId"].as_i64() == Some(key_id))
        .expect("an audit row for this write should exist");
    assert_eq!(row["action"], "write");
    assert_eq!(row["result"], "ok");
    assert_eq!(row["valueRequested"], 42.0);
    assert_eq!(row["externalNameSnapshot"], external_name);

    sim.stop();
}

/// broker が(未接続で)止まっている状態での書き込みは 502 + audit が
/// failed のまま(log-before-write: 先に failed で挿入され、成功しなければ
/// 更新されない安全側の記録)。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_while_broker_is_down_is_502_and_audit_stays_failed() {
    let app = test_app("audit-failed").await;

    // わざと誰も listen していないポートを使う(TcpListener を bind して
    // すぐ drop することで、テスト実行中は極めて高い確率で未使用のままの
    // ローカルポート番号を得る - シミュレータを一切起動しない)。
    let closed_port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };

    let (tag_id, external_name) = make_tag(
        &app,
        "line1",
        "slmp",
        closed_port,
        "temp01",
        "D100",
        "u16",
        true,
        true,
    )
    .await;
    app.write_control.enable();
    let (key, key_id) = issue_key(
        &app.router,
        &app.admin_token,
        "writer",
        &["write:line1.fast.temp01"],
    )
    .await;

    let (status, body) = v1_post(
        &app.router,
        &format!("/api/v1/values/{external_name}"),
        &key,
        json!({ "v": 7 }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body:?}");
    assert_eq!(body["error"], "write_failed");

    let (status, listed) = admin_post(
        &app.router,
        "/api/write-audit/list",
        &app.admin_token,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let rows = listed["rows"].as_array().unwrap();
    let row = rows
        .iter()
        .find(|r| r["tagId"].as_i64() == Some(tag_id) && r["apiKeyId"].as_i64() == Some(key_id))
        .expect("an audit row for this attempted write should exist");
    assert_eq!(row["action"], "write");
    assert_eq!(
        row["result"], "failed",
        "log-before-write must leave the row failed when the broker is unreachable"
    );
}

// ---------------------------------------------------------------------------
// 補足: 書き込み受付トグルの REST 経路自体(admin 限定・監査・
// /api/v1/status への反映)。
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rest_enable_disable_round_trip_and_reflects_in_status() {
    let app = test_app("write-control-rest").await;

    let (status, json) = get_json(&app.router, "/api/v1/status", &app.admin_token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["write_enabled"], false);
    assert_eq!(json["write_was_enabled_before_restart"], false);

    let (status, body) =
        admin_post_empty(&app.router, "/api/write-control/enable", &app.admin_token).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["write_enabled"], true);

    let (status, json) = get_json(&app.router, "/api/v1/status", &app.admin_token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["write_enabled"], true);

    let (status, body) =
        admin_post_empty(&app.router, "/api/write-control/disable", &app.admin_token).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["write_enabled"], false);

    let (status, json) = get_json(&app.router, "/api/v1/status", &app.admin_token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["write_enabled"], false);
}
