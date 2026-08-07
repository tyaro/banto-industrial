//! T11-1 の統合テスト（docs/ux-plan.md §3「一括登録 API」）:
//! `POST /api/tags/batch` の all-or-nothing 検証・単一トランザクション
//! 適用・rebuild 1回・dry run・重複名検出を、実際の `CollectorManager` +
//! axum ルーター経由で確認する。
//!
//! `tests/t7_partial_reconfig.rs` と同じ理由で `TempEnv`/`TestApp`/
//! `get_json`/`write_json` 相当をこのファイル内に複製している（各
//! `tests/*.rs` は独立クレートとしてコンパイルされ、private helper を
//! 共有できない）。
//!
//! 「rebuild が1回だけ走る」の検証方法: `CollectorManager::revision()` は
//! `rebuild()` が成功するたびにちょうど1つ進む（`tests/t7_partial_reconfig.rs`
//! が単発 CRUD の rebuild 確認に使っているのと同じ性質 - `hub.rs::rebuild`
//! の doc comment 参照）。よって「バッチ適用前後で revision がちょうど+1」
//! であることは「rebuild がちょうど1回呼ばれた」ことの直接証拠になる - N
//! 件のタグを `POST /api/tags` で N 回叩けば revision は+Nになるところ、
//! バッチ API は件数によらず常に+1。

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
use banto_server::{start, AuthState, Identity, ServerConfig};
use banto_tags::{
    CollectionGroupInput, CollectionGroupService, PlcConnectionInput, PlcConnectionService,
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
            "banto-hub-t11-1-it-{}-{label}-{id}-{nanos}",
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

/// A disabled connection: the batch tests only care about registry writes
/// and `CollectorManager::revision()`, never about actual PLC connectivity -
/// `enabled: false` keeps `rebuild()` from ever attempting a real socket
/// connect (no simulator needed in this file, unlike `t7_partial_reconfig.rs`).
fn disabled_conn_input(name: &str) -> PlcConnectionInput {
    PlcConnectionInput {
        name: name.to_string(),
        protocol: "modbus-tcp".to_string(),
        host: "127.0.0.1".to_string(),
        port: 1,
        unit_id: 1,
        enabled: false,
        simulation: false,
    }
}

fn group_input(name: &str, conn_id: i64) -> CollectionGroupInput {
    CollectionGroupInput {
        name: name.to_string(),
        plc_connection_id: conn_id,
        period_ms: 1_000,
        enabled: false,
    }
}

struct TestApp {
    #[allow(dead_code)]
    server: banto_server::RunningServer,
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
        banto_tags::TagService::new(pool.clone()),
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
        _env: env,
    }
}

/// `GET /api/tags` lives on the admin surface (`require_banto_client_header`
/// applies to every method on that router, unlike `/api/v1/*` - see
/// `apps/banto-hub/core/src/rest.rs::api_router`'s doc comment), so this
/// helper sends the CSRF header even for a read, unlike
/// `tests/t7_partial_reconfig.rs`'s same-named helper (which only ever GETs
/// `/api/v1/*`).
async fn get_json(router: &Router, path: &str, token: &str) -> (StatusCode, Value) {
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

/// Seed one disabled connection + one disabled group, returning the group id
/// every test in this file attaches its tags to.
async fn seed_group(app: &TestApp, label: &str) -> i64 {
    let conn = PlcConnectionService::new(app.pool.clone())
        .create(disabled_conn_input(&format!("conn_{label}")))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input(&format!("group_{label}"), conn.id))
        .await
        .unwrap();
    group.id
}

fn tag_payload(name: &str, group_id: i64, address: &str) -> Value {
    json!({
        "name": name,
        "collectionGroupId": group_id,
        "address": address,
        "dataType": "i16",
        "enabled": true,
    })
}

async fn tag_count(app: &TestApp) -> usize {
    let (status, tags) = get_json(&app.router, "/api/tags", &app.token).await;
    assert_eq!(status, StatusCode::OK);
    tags.as_array().unwrap().len()
}

// ---------------------------------------------------------------------------
// 1. 全件成功 + rebuild が1回だけ走る (revision がちょうど+1)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_create_all_succeed_and_rebuilds_exactly_once() {
    let app = test_app("all-succeed").await;
    let group_id = seed_group(&app, "a").await;

    let revision_before = app.manager.revision();
    let before_count = tag_count(&app).await;

    let (status, body) = write_json(
        &app.router,
        "POST",
        "/api/tags/batch",
        &app.token,
        json!({
            "tags": [
                tag_payload("bt1", group_id, "40001"),
                tag_payload("bt2", group_id, "40002"),
                tag_payload("bt3", group_id, "40003"),
            ],
            "dryRun": false,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["count"], json!(3));
    assert_eq!(body["tags"].as_array().unwrap().len(), 3);
    assert_eq!(body["errors"].as_array().unwrap().len(), 0);

    // 3件追加したにもかかわらず revision はちょうど+1 - rebuild が (3回で
    // はなく) 1回だけ走ったことの直接証拠。
    assert_eq!(
        app.manager.revision(),
        revision_before + 1,
        "a 3-tag batch must trigger exactly one rebuild, not one per tag"
    );
    assert_eq!(tag_count(&app).await, before_count + 3);
}

// ---------------------------------------------------------------------------
// 2. 1件不正で全体拒否 (DB 無変更・rebuild も走らない)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_create_rejects_whole_batch_on_one_invalid_row() {
    let app = test_app("one-invalid").await;
    let group_id = seed_group(&app, "b").await;

    let revision_before = app.manager.revision();
    let before_count = tag_count(&app).await;

    let mut bad = tag_payload("bt_bad", group_id, "40002");
    bad["dataType"] = json!("f64"); // ALLOWED_DATA_TYPES にない

    let (status, body) = write_json(
        &app.router,
        "POST",
        "/api/tags/batch",
        &app.token,
        json!({
            "tags": [
                tag_payload("bt_ok1", group_id, "40001"),
                bad,
                tag_payload("bt_ok2", group_id, "40003"),
            ],
            "dryRun": false,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], json!(false));
    assert_eq!(body["count"], json!(0));
    assert!(body.get("tags").is_none(), "{body:?}");

    let errors = body["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0]["index"], json!(1));
    let field_errors = errors[0]["fieldErrors"].as_array().unwrap();
    assert!(
        field_errors.iter().any(|e| e["field"] == "dataType"),
        "{field_errors:?}"
    );

    // DB 無変更・rebuild も走っていない(良いタグ2件も含めて何も書かれない)。
    assert_eq!(app.manager.revision(), revision_before);
    assert_eq!(tag_count(&app).await, before_count);
}

// ---------------------------------------------------------------------------
// 3. dry run は DB 無変更
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_dry_run_never_writes() {
    let app = test_app("dry-run").await;
    let group_id = seed_group(&app, "c").await;

    let revision_before = app.manager.revision();
    let before_count = tag_count(&app).await;

    let (status, body) = write_json(
        &app.router,
        "POST",
        "/api/tags/batch",
        &app.token,
        json!({
            "tags": [
                tag_payload("dry1", group_id, "40001"),
                tag_payload("dry2", group_id, "40002"),
            ],
            "dryRun": true,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["dryRun"], json!(true));
    assert_eq!(body["count"], json!(2));
    assert!(
        body.get("tags").is_none(),
        "dry run must not report created rows: {body:?}"
    );

    assert_eq!(app.manager.revision(), revision_before);
    assert_eq!(tag_count(&app).await, before_count);
}

// ---------------------------------------------------------------------------
// 4. 重複名検出: リクエスト内
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_rejects_duplicate_names_within_the_request() {
    let app = test_app("dupe-in-batch").await;
    let group_id = seed_group(&app, "d").await;

    let (status, body) = write_json(
        &app.router,
        "POST",
        "/api/tags/batch",
        &app.token,
        json!({
            "tags": [
                tag_payload("same", group_id, "40001"),
                tag_payload("same", group_id, "40002"),
            ],
            "dryRun": true,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], json!(false));

    let indices: Vec<u64> = body["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["index"].as_u64().unwrap())
        .collect();
    // 両方の行(0番目・1番目)がフラグされる - 「後勝ち」ではなく全行提示。
    assert_eq!(indices, vec![0, 1], "{body:?}");
}

// ---------------------------------------------------------------------------
// 5. 重複名検出: 既存タグとの衝突
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_rejects_a_name_already_used_by_an_existing_tag() {
    let app = test_app("dupe-existing").await;
    let group_id = seed_group(&app, "e").await;

    let (status, created) = write_json(
        &app.router,
        "POST",
        "/api/tags",
        &app.token,
        tag_payload("existing", group_id, "40001"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created:?}");

    let revision_before = app.manager.revision();

    let (status, body) = write_json(
        &app.router,
        "POST",
        "/api/tags/batch",
        &app.token,
        json!({
            "tags": [tag_payload("existing", group_id, "40002")],
            "dryRun": false,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], json!(false));
    let errors = body["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0]["index"], json!(0));
    let field_errors = errors[0]["fieldErrors"].as_array().unwrap();
    assert!(
        field_errors
            .iter()
            .any(|e| e["field"] == "name" && e["message"] == "既に使用されています"),
        "{field_errors:?}"
    );

    // 既存タグとの衝突検出は rebuild を伴わない(何も書かれていない)。
    assert_eq!(app.manager.revision(), revision_before);
}

// ---------------------------------------------------------------------------
// 6. 空配列は no-op 成功
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_with_empty_tags_array_is_a_harmless_success() {
    let app = test_app("empty-batch").await;
    let revision_before = app.manager.revision();

    let (status, body) = write_json(
        &app.router,
        "POST",
        "/api/tags/batch",
        &app.token,
        json!({ "tags": [], "dryRun": false }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["count"], json!(0));
    assert_eq!(
        app.manager.revision(),
        revision_before,
        "no rebuild for an empty batch"
    );
}
