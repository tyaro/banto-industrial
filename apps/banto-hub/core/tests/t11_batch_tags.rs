//! T11-1 の統合テスト（docs/ux-plan.md §3「一括登録 API」）:
//! `POST /api/tags/batch` の all-or-nothing 検証・単一トランザクション
//! 適用・rebuild 1回・dry run・重複名検出を、実際の `CollectorManager` +
//! axum ルーター経由で確認する。
//!
//! `tests/t7_partial_reconfig.rs` と同じ理由で `TestApp`/`get_json`/
//! `write_json` 相当をこのファイル内に複製している（各 `tests/*.rs` は
//! 独立クレートとしてコンパイルされ、private helper を共有できない）。
//! `TempEnv` は `tests/common/mod.rs` に集約済み（2026-08-08、テスト一時
//! ディレクトリリークの根治）。
//!
//! 「rebuild が1回だけ走る」の検証方法: `CollectorManager::revision()` は
//! `rebuild()` が成功するたびにちょうど1つ進む（`tests/t7_partial_reconfig.rs`
//! が単発 CRUD の rebuild 確認に使っているのと同じ性質 - `hub.rs::rebuild`
//! の doc comment 参照）。よって「バッチ適用前後で revision がちょうど+1」
//! であることは「rebuild がちょうど1回呼ばれた」ことの直接証拠になる - N
//! 件のタグを `POST /api/tags` で N 回叩けば revision は+Nになるところ、
//! バッチ API は件数によらず常に+1。
//!
//! T18-3b（bulk tag operations）: 末尾に `POST /api/tags/batch-update` の
//! 同種の統合テスト（一括 enabled 切替・グループ移動・不正 id/revision
//! 競合での all-or-nothing rollback・dry run・稼働中キュー）も同居させて
//! いる - `/api/tags/batch`（T11-1、上のセクション群）の update 版で、
//! 検証観点（all-or-nothing・単一トランザクション・rebuild 1回・dry run）
//! が完全に対になるため、専用ファイルへ分けずここに追加した。

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
use banto_hub_core::db::init_db;
use banto_hub_core::grpc::{GrpcServer, GrpcService};
use banto_hub_core::hub::CollectorManager;
use banto_hub_core::rest::api_router;
use banto_hub_core::settings::SettingsService;
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

mod common;
use common::TempEnv;

const CLIENT_HEADER: (&str, &str) = ("X-Banto-Client", "banto");

/// Temp-dir prefix passed to `TempEnv::new` (see `tests/common/mod.rs`).
const TEMP_ENV_PREFIX: &str = "banto-hub-t11-1-it";

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

        word_order: "low_high".to_string(),
    }
}

fn group_input(name: &str, conn_id: i64) -> CollectionGroupInput {
    CollectionGroupInput {
        name: name.to_string(),
        plc_connection_id: conn_id,
        period_ms: 1_000,
        enabled: false,
        default_writable: true,
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
        banto_tags::TagService::new(pool.clone()),
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

/// Seed an enabled connection/group for a test that must reach
/// `banto-collect`'s address/config validation. The ordinary batch fixture is
/// disabled so it never attempts a PLC connection during compatibility
/// rebuilds.
async fn seed_enabled_group(app: &TestApp, label: &str) -> i64 {
    let mut connection = disabled_conn_input(&format!("enabled_conn_{label}"));
    connection.enabled = true;
    let conn = PlcConnectionService::new(app.pool.clone())
        .create(connection)
        .await
        .unwrap();
    let mut group = group_input(&format!("enabled_group_{label}"), conn.id);
    group.enabled = true;
    CollectionGroupService::new(app.pool.clone())
        .create(group)
        .await
        .unwrap()
        .id
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

    let revision_before = app.manager.configured_revision();
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
        app.manager.configured_revision(),
        revision_before + 1,
        "a 3-tag batch must commit the configured catalog exactly once"
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

    let revision_before = app.manager.configured_revision();
    let db_count_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tags")
        .fetch_one(&app.pool)
        .await
        .unwrap();
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

    assert_eq!(app.manager.configured_revision(), revision_before);
    let db_count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tags")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(db_count_after, db_count_before);
    assert_eq!(tag_count(&app).await, before_count);
}

// ---------------------------------------------------------------------------
// 3b. 単票のcatalog preflight失敗はmutationごとrollback
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn single_invalid_address_rolls_back_the_db_and_configured_revision() {
    let app = test_app("single-invalid-address").await;
    let group_id = seed_enabled_group(&app, "single-invalid-address").await;
    let revision_before = app.manager.configured_revision();
    let db_count_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tags")
        .fetch_one(&app.pool)
        .await
        .unwrap();

    let (status, body) = write_json(
        &app.router,
        "POST",
        "/api/tags",
        &app.token,
        tag_payload("bad_address", group_id, "99999"),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    assert_eq!(body["kind"], json!("validation"));

    let db_count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tags")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(db_count_after, db_count_before);
    assert_eq!(app.manager.configured_revision(), revision_before);
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
        field_errors.iter().any(|e| e["field"] == "name"
            && e["message"] == "この収集グループ内では既に使用されています"),
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

// ===========================================================================
// T18-3b: POST /api/tags/batch-update （一括更新: enabled 切替・グループ
// 移動）
// ===========================================================================

/// Row payload for `/api/tags/batch-update`: the same shape as
/// `tag_payload`'s single-row body, plus `id` and an optional
/// `expectedRevision`.
fn tag_batch_update_payload(
    id: i64,
    name: &str,
    group_id: i64,
    address: &str,
    enabled: bool,
    expected_revision: Option<i64>,
) -> Value {
    let mut v = tag_payload(name, group_id, address);
    v["id"] = json!(id);
    v["enabled"] = json!(enabled);
    if let Some(revision) = expected_revision {
        v["expectedRevision"] = json!(revision);
    }
    v
}

async fn create_tag(app: &TestApp, name: &str, group_id: i64, address: &str) -> Value {
    let (status, created) = write_json(
        &app.router,
        "POST",
        "/api/tags",
        &app.token,
        tag_payload(name, group_id, address),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created:?}");
    created
}

// ---------------------------------------------------------------------------
// U1. 全件成功（一括 enabled 切替）+ rebuild が1回だけ走る
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_update_all_succeed_and_rebuilds_exactly_once() {
    let app = test_app("update-all-succeed").await;
    let group_id = seed_group(&app, "u1").await;

    let t1 = create_tag(&app, "ut1", group_id, "40001").await;
    let t2 = create_tag(&app, "ut2", group_id, "40002").await;
    let t3 = create_tag(&app, "ut3", group_id, "40003").await;

    let revision_before = app.manager.configured_revision();

    let (status, body) = write_json(
        &app.router,
        "POST",
        "/api/tags/batch-update",
        &app.token,
        json!({
            "dryRun": false,
            "tags": [
                tag_batch_update_payload(t1["id"].as_i64().unwrap(), "ut1", group_id, "40001", false, Some(t1["revision"].as_i64().unwrap())),
                tag_batch_update_payload(t2["id"].as_i64().unwrap(), "ut2", group_id, "40002", false, Some(t2["revision"].as_i64().unwrap())),
                tag_batch_update_payload(t3["id"].as_i64().unwrap(), "ut3", group_id, "40003", false, Some(t3["revision"].as_i64().unwrap())),
            ],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["count"], json!(3));
    assert_eq!(body["tags"].as_array().unwrap().len(), 3);
    assert_eq!(body["errors"].as_array().unwrap().len(), 0);
    assert!(
        body["tags"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["enabled"] == json!(false)),
        "{body:?}"
    );

    // 3件更新したにもかかわらず revision はちょうど+1 - rebuild が1回だけ
    // 走ったことの直接証拠（T11-1 と同じ検証方法）。
    assert_eq!(
        app.manager.configured_revision(),
        revision_before + 1,
        "a 3-tag batch-update must commit the configured catalog exactly once"
    );

    let (status, fetched) = get_json(
        &app.router,
        &format!("/api/tags/{}", t1["id"].as_i64().unwrap()),
        &app.token,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fetched["enabled"], json!(false));
    assert_eq!(fetched["revision"], json!(2));
}

// ---------------------------------------------------------------------------
// U2. 一括グループ移動
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_update_moves_tags_to_another_collection_group() {
    let app = test_app("update-move-group").await;
    let group_a = seed_group(&app, "u2a").await;
    let group_b = seed_group(&app, "u2b").await;

    let t1 = create_tag(&app, "um1", group_a, "40001").await;
    let t2 = create_tag(&app, "um2", group_a, "40002").await;

    let (status, body) = write_json(
        &app.router,
        "POST",
        "/api/tags/batch-update",
        &app.token,
        json!({
            "dryRun": false,
            "tags": [
                tag_batch_update_payload(t1["id"].as_i64().unwrap(), "um1", group_b, "40001", true, None),
                tag_batch_update_payload(t2["id"].as_i64().unwrap(), "um2", group_b, "40002", true, None),
            ],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["count"], json!(2));

    let (_, fetched1) = get_json(
        &app.router,
        &format!("/api/tags/{}", t1["id"].as_i64().unwrap()),
        &app.token,
    )
    .await;
    let (_, fetched2) = get_json(
        &app.router,
        &format!("/api/tags/{}", t2["id"].as_i64().unwrap()),
        &app.token,
    )
    .await;
    assert_eq!(fetched1["collectionGroupId"], json!(group_b));
    assert_eq!(fetched2["collectionGroupId"], json!(group_b));
}

// ---------------------------------------------------------------------------
// U3. 存在しない id を含む一括更新は全体 rollback（DB 無変更）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_update_rejects_whole_batch_on_a_nonexistent_id() {
    let app = test_app("update-bad-id").await;
    let group_id = seed_group(&app, "u3").await;

    let t1 = create_tag(&app, "ug1", group_id, "40001").await;
    let revision_before = app.manager.revision();

    let (status, body) = write_json(
        &app.router,
        "POST",
        "/api/tags/batch-update",
        &app.token,
        json!({
            "dryRun": false,
            "tags": [
                tag_batch_update_payload(t1["id"].as_i64().unwrap(), "ug1-renamed", group_id, "40001", false, None),
                tag_batch_update_payload(999_999, "ghost", group_id, "40002", false, None),
            ],
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
    assert_eq!(errors[0]["id"], json!(999_999));
    let field_errors = errors[0]["fieldErrors"].as_array().unwrap();
    assert!(
        field_errors.iter().any(|e| e["field"] == "id"),
        "{field_errors:?}"
    );

    // All-or-nothing: t1's individually-fine rename was not written either,
    // and no rebuild happened.
    assert_eq!(app.manager.revision(), revision_before);
    let (_, fetched1) = get_json(
        &app.router,
        &format!("/api/tags/{}", t1["id"].as_i64().unwrap()),
        &app.token,
    )
    .await;
    assert_eq!(fetched1["name"], json!("ug1"));
}

// ---------------------------------------------------------------------------
// U4. revision 競合を含む一括更新は全体 rollback（DB 無変更）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_update_rejects_whole_batch_on_a_stale_expected_revision() {
    let app = test_app("update-stale-revision").await;
    let group_id = seed_group(&app, "u4").await;

    let t1 = create_tag(&app, "us1", group_id, "40001").await;
    let t2 = create_tag(&app, "us2", group_id, "40002").await;
    let stale_revision = t1["revision"].as_i64().unwrap();

    // Another (single-row) update advances t1's revision behind the batch's
    // back.
    let (status, _) = write_json(
        &app.router,
        "PUT",
        &format!("/api/tags/{}", t1["id"].as_i64().unwrap()),
        &app.token,
        tag_payload("us1-bumped", group_id, "40001"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let revision_before = app.manager.revision();

    let (status, body) = write_json(
        &app.router,
        "POST",
        "/api/tags/batch-update",
        &app.token,
        json!({
            "dryRun": false,
            "tags": [
                tag_batch_update_payload(t1["id"].as_i64().unwrap(), "us1-stale", group_id, "40001", false, Some(stale_revision)),
                tag_batch_update_payload(t2["id"].as_i64().unwrap(), "us2-renamed", group_id, "40002", false, None),
            ],
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
            .any(|e| e["field"] == "expectedRevision"),
        "{field_errors:?}"
    );

    // All-or-nothing: t2 (whose row was individually fine) is still
    // untouched, and no rebuild happened for this batch attempt.
    assert_eq!(app.manager.revision(), revision_before);
    let (_, fetched2) = get_json(
        &app.router,
        &format!("/api/tags/{}", t2["id"].as_i64().unwrap()),
        &app.token,
    )
    .await;
    assert_eq!(fetched2["name"], json!("us2"));
}

// ---------------------------------------------------------------------------
// U5. dry run は DB 無変更
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_update_dry_run_never_writes() {
    let app = test_app("update-dry-run").await;
    let group_id = seed_group(&app, "u5").await;
    let t1 = create_tag(&app, "ud1", group_id, "40001").await;

    let revision_before = app.manager.configured_revision();

    let (status, body) = write_json(
        &app.router,
        "POST",
        "/api/tags/batch-update",
        &app.token,
        json!({
            "dryRun": true,
            "tags": [
                tag_batch_update_payload(t1["id"].as_i64().unwrap(), "ud1-preview", group_id, "40001", false, None),
            ],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["dryRun"], json!(true));
    assert_eq!(body["count"], json!(1));
    assert!(
        body.get("tags").is_none(),
        "dry run must not report updated rows: {body:?}"
    );

    assert_eq!(app.manager.configured_revision(), revision_before);
    let (_, fetched1) = get_json(
        &app.router,
        &format!("/api/tags/{}", t1["id"].as_i64().unwrap()),
        &app.token,
    )
    .await;
    assert_eq!(
        fetched1["name"],
        json!("ud1"),
        "dry run must not write anything"
    );
    assert_eq!(fetched1["revision"], json!(1));
}

// ---------------------------------------------------------------------------
// U6. 空配列は no-op 成功
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_update_with_empty_tags_array_is_a_harmless_success() {
    let app = test_app("update-empty-batch").await;
    let revision_before = app.manager.revision();

    let (status, body) = write_json(
        &app.router,
        "POST",
        "/api/tags/batch-update",
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

// ---------------------------------------------------------------------------
// U7. 稼働中は非 dryRun がキューされる（202）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_update_non_dry_run_while_running_is_accepted_and_queued() {
    let app = test_app("update-queued").await;
    let group_id = seed_group(&app, "u7").await;
    let t1 = create_tag(&app, "uq1", group_id, "40001").await;

    let start = app
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/start")
                .header("Authorization", format!("Bearer {}", app.token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(start.status(), StatusCode::OK);

    let response = app
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/tags/batch-update")
                .header("Authorization", format!("Bearer {}", app.token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "dryRun": false,
                        "tags": [
                            tag_batch_update_payload(t1["id"].as_i64().unwrap(), "uq1-updated", group_id, "40001", false, None),
                        ],
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);

    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["queued"], json!(true));

    let queued_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pending_changes")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(queued_count, 1);
}

// ===========================================================================
// T19 S2-c1 (UX-37): POST /api/tags/batch-delete （一括削除）
// ===========================================================================

// ---------------------------------------------------------------------------
// D1. 全件成功 + rebuild が1回だけ走る
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_delete_all_succeed_and_rebuilds_exactly_once() {
    let app = test_app("delete-all-succeed").await;
    let group_id = seed_group(&app, "d1").await;

    let t1 = create_tag(&app, "dt1", group_id, "40001").await;
    let t2 = create_tag(&app, "dt2", group_id, "40002").await;
    let t3 = create_tag(&app, "dt3", group_id, "40003").await;

    let revision_before = app.manager.configured_revision();

    let (status, body) = write_json(
        &app.router,
        "POST",
        "/api/tags/batch-delete",
        &app.token,
        json!({ "ids": [t1["id"], t2["id"]] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["count"], json!(2));
    assert_eq!(body["errors"].as_array().unwrap().len(), 0);

    // 2件削除したにもかかわらず revision はちょうど+1 - rebuild が1回だけ
    // 走ったことの直接証拠（T11-1/T18-3b と同じ検証方法）。
    assert_eq!(
        app.manager.configured_revision(),
        revision_before + 1,
        "a 2-tag batch-delete must commit the configured catalog exactly once"
    );

    let (status, _) = get_json(
        &app.router,
        &format!("/api/tags/{}", t1["id"].as_i64().unwrap()),
        &app.token,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = get_json(
        &app.router,
        &format!("/api/tags/{}", t2["id"].as_i64().unwrap()),
        &app.token,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 対象外だった t3 は残る。
    let (status, fetched3) = get_json(
        &app.router,
        &format!("/api/tags/{}", t3["id"].as_i64().unwrap()),
        &app.token,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fetched3["name"], json!("dt3"));
}

// ---------------------------------------------------------------------------
// D2. 存在しない id を含む一括削除は全体 rollback（DB 無変更）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_delete_rejects_whole_batch_on_a_nonexistent_id() {
    let app = test_app("delete-bad-id").await;
    let group_id = seed_group(&app, "d2").await;

    let t1 = create_tag(&app, "dg1", group_id, "40001").await;
    let revision_before = app.manager.revision();

    let (status, body) = write_json(
        &app.router,
        "POST",
        "/api/tags/batch-delete",
        &app.token,
        json!({ "ids": [t1["id"], 999_999] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], json!(false));
    assert_eq!(body["count"], json!(0));

    let errors = body["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0]["index"], json!(1));
    assert_eq!(errors[0]["id"], json!(999_999));
    let field_errors = errors[0]["fieldErrors"].as_array().unwrap();
    assert!(
        field_errors.iter().any(|e| e["field"] == "id"),
        "{field_errors:?}"
    );

    // All-or-nothing: t1 (individually a valid id) was not deleted either,
    // and no rebuild happened.
    assert_eq!(app.manager.revision(), revision_before);
    let (status, _) = get_json(
        &app.router,
        &format!("/api/tags/{}", t1["id"].as_i64().unwrap()),
        &app.token,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

// ---------------------------------------------------------------------------
// D3. 重複 id を含む一括削除は全体 rollback（DB 無変更）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_delete_rejects_whole_batch_on_a_duplicate_id() {
    let app = test_app("delete-dup-id").await;
    let group_id = seed_group(&app, "d3").await;

    let t1 = create_tag(&app, "dd1", group_id, "40001").await;
    let revision_before = app.manager.revision();

    let (status, body) = write_json(
        &app.router,
        "POST",
        "/api/tags/batch-delete",
        &app.token,
        json!({ "ids": [t1["id"], t1["id"]] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], json!(false));
    let errors = body["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 2, "{errors:?}");

    assert_eq!(app.manager.revision(), revision_before);
    let (status, _) = get_json(
        &app.router,
        &format!("/api/tags/{}", t1["id"].as_i64().unwrap()),
        &app.token,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

// ---------------------------------------------------------------------------
// D4. 空配列は no-op 成功
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_delete_with_empty_ids_array_is_a_harmless_success() {
    let app = test_app("delete-empty-batch").await;
    let revision_before = app.manager.revision();

    let (status, body) = write_json(
        &app.router,
        "POST",
        "/api/tags/batch-delete",
        &app.token,
        json!({ "ids": [] }),
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

// ---------------------------------------------------------------------------
// D5. 稼働中は 202 でキューされる
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_delete_while_running_is_accepted_and_queued() {
    let app = test_app("delete-queued").await;
    let group_id = seed_group(&app, "d5").await;
    let t1 = create_tag(&app, "dq1", group_id, "40001").await;

    let start = app
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/start")
                .header("Authorization", format!("Bearer {}", app.token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(start.status(), StatusCode::OK);

    let response = app
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/tags/batch-delete")
                .header("Authorization", format!("Bearer {}", app.token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .header("content-type", "application/json")
                .body(Body::from(json!({ "ids": [t1["id"]] }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);

    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["queued"], json!(true));

    let queued_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pending_changes")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(queued_count, 1);

    // t1 must still exist - queuing while running must not delete anything
    // itself.
    let (status, _) = get_json(
        &app.router,
        &format!("/api/tags/{}", t1["id"].as_i64().unwrap()),
        &app.token,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

// ---------------------------------------------------------------------------
// D6. 稼働中に積んだ tags.batch_delete の pending を、停止後に適用すると
// 実際にタグが消える（B-2 の非対称バグ再発防止の直接証拠 -
// 実装指示「必ずテストで固定してください」）。
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_delete_pending_apply_after_stop_actually_deletes_the_rows() {
    let app = test_app("delete-pending-apply").await;
    let group_id = seed_group(&app, "d6").await;
    let t1 = create_tag(&app, "dp1", group_id, "40001").await;
    let t2 = create_tag(&app, "dp2", group_id, "40002").await;
    // A tag outside the batch - must survive both queuing and apply.
    let t3 = create_tag(&app, "dp3", group_id, "40003").await;

    let start = app
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/start")
                .header("Authorization", format!("Bearer {}", app.token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(start.status(), StatusCode::OK);

    let queued = app
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/tags/batch-delete")
                .header("Authorization", format!("Bearer {}", app.token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "ids": [t1["id"], t2["id"]] }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(queued.status(), StatusCode::ACCEPTED);
    let queued_bytes = axum::body::to_bytes(queued.into_body(), usize::MAX)
        .await
        .unwrap();
    let queued_body: Value = serde_json::from_slice(&queued_bytes).unwrap();
    let pending_id = queued_body["pending"]["id"]
        .as_i64()
        .expect("pending id should exist");

    let stop = app
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/stop")
                .header("Authorization", format!("Bearer {}", app.token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stop.status(), StatusCode::OK);

    let apply = app
        .router
        .clone()
        .oneshot(
            HttpRequest::post(format!("/api/pending-changes/{pending_id}/apply"))
                .header("Authorization", format!("Bearer {}", app.token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(apply.status(), StatusCode::OK);
    let apply_bytes = axum::body::to_bytes(apply.into_body(), usize::MAX)
        .await
        .unwrap();
    let apply_body: Value = serde_json::from_slice(&apply_bytes).unwrap();
    assert_eq!(apply_body["state"], json!("applied"), "{apply_body:?}");

    // The core assertion this test exists for: applying a pending
    // tags.batch_delete must actually delete the rows, not just mark the
    // pending change "applied" while leaving the registry untouched.
    let (status, _) = get_json(
        &app.router,
        &format!("/api/tags/{}", t1["id"].as_i64().unwrap()),
        &app.token,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "t1 should have been deleted");
    let (status, _) = get_json(
        &app.router,
        &format!("/api/tags/{}", t2["id"].as_i64().unwrap()),
        &app.token,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "t2 should have been deleted");

    let (status, fetched3) = get_json(
        &app.router,
        &format!("/api/tags/{}", t3["id"].as_i64().unwrap()),
        &app.token,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fetched3["name"], json!("dp3"));
}
