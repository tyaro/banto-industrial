//! 外部 DB 連携 S5 の統合テスト（docs/banto-hub-external-db-design.md §7 の
//! スライス表「S5 の完了条件: interval / on_change・DB 停止中のキュー上限・
//! 復帰後の flush・停止時の flush・Hub 再起動への追従」）。
//!
//! ## 何を繋いでいるか
//!
//! 1 つのテストプロセスの中で、
//!
//! - **Hub**: `banto_hub_core` を dev-dependency として使い、一時ディレクトリ
//!   の SQLite で `api_router` を組み立て、`banto_server::start` で
//!   `127.0.0.1:0`（OS が空きポートを選ぶ）に実サーバーとして立てる。
//!   プロファイル id は `sink-it-<pid>` - 実運用の `default` プロファイル
//!   （Windows では `Global\BantoHub.default` の名前付き mutex で排他）を
//!   決して掴まないため。
//! - **DB Source（S2）**: `BANTO_TEST_PG_URL` の PostgreSQL に対して
//!   `SELECT 42.5 AS a` を回す `db` タグ。これが「値の出所」になる。
//! - **サイドカー（S5、被試験体）**: 生成した toml で
//!   `banto_hub_sink::run` を**プロセス内**で回す。Hub とは実 HTTP/WS
//!   （banto-tagclient SDK）で、DB とは実 PostgreSQL で繋がる。
//!
//! つまり「PLC 相当（実 DB の SELECT）→ Hub のタグ空間 → SDK 購読 →
//! 外部 DB への INSERT」という §5 の経路を端から端まで通す。
//!
//! ## 実 PostgreSQL が要る
//!
//! `BANTO_TEST_PG_URL` が無ければ `eprintln!("skipped: BANTO_TEST_PG_URL
//! unset")` を出して即成功する（`banto-hub-core` の `tests/db_source.rs` と
//! 同じゲート方式）。CI では `.github/workflows/ci.yml` の
//! `Rust (db-source / PostgreSQL)` ジョブが設定する。
//!
//! ```text
//! BANTO_TEST_PG_URL=postgres://postgres:postgres@127.0.0.1:5432/postgres \
//!   cargo test -p banto-hub-sink
//! ```
//!
//! ## 決定性
//!
//! 固定の `sleep` で「たぶん終わっているはず」を待たない。すべて
//! [`wait_until`] のポーリング + 明示的なタイムアウトで、条件が満たされた
//! 瞬間に進む。テーブル検査の再検査間隔だけは
//! `banto_hub_sink::SidecarOptions`（設定ファイルには出さない注入口）で
//! 1 秒に縮めてある - 本番の既定は設計どおり 30 秒。

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
use banto_hub_sink::{run, SidecarConfig, SidecarOptions};
use banto_server::{start, AuthState, Identity, ServerConfig};
use banto_tags::{
    CollectionGroupInput, CollectionGroupService, PlcConnectionInput, PlcConnectionService,
    TagInput, TagService, DB_TAG_KIND, POSTGRES_PROTOCOL,
};
use banto_tstore::SystemClock;
use serde_json::{json, Value};
use sqlx::postgres::{PgConnectOptions, PgPool};
use sqlx::{Row as _, SqlitePool};
use tokio::sync::broadcast;
use tokio::sync::Notify;
use tower::ServiceExt;

const CLIENT_HEADER: (&str, &str) = ("X-Banto-Client", "banto");

/// DB Source のポーリング周期（Hub の `period_ms` CHECK の許容値）。
const SOURCE_PERIOD_MS: i64 = 500;

/// sink group の周期（Hub 側の検証は 100ms 以上）。
const SINK_INTERVAL_MS: i64 = 200;

/// ポーリング待ちの上限。CI のコンテナ起動直後でも数周期は待てる長さ。
const WAIT: Duration = Duration::from_secs(15);

// --- BANTO_TEST_PG_URL ゲート ------------------------------------------------

#[derive(Clone)]
struct PgTarget {
    host: String,
    port: i64,
    database: String,
    username: String,
    password: Option<String>,
    url: String,
}

fn pg_target() -> Option<PgTarget> {
    let url = std::env::var("BANTO_TEST_PG_URL").ok()?;
    let options: PgConnectOptions = url
        .parse()
        .expect("BANTO_TEST_PG_URL should be a valid postgres:// URL");
    Some(PgTarget {
        host: options.get_host().to_string(),
        port: options.get_port() as i64,
        database: options.get_database().unwrap_or("postgres").to_string(),
        username: options.get_username().to_string(),
        // `PgConnectOptions` はパスワードを読み出せないので URL から取る
        // （`tests/db_source.rs` と同じ小道具）。
        password: url
            .split_once("://")
            .and_then(|(_, rest)| rest.split_once('@'))
            .and_then(|(userinfo, _)| userinfo.split_once(':'))
            .map(|(_, password)| password.to_string()),
        url,
    })
}

macro_rules! require_pg {
    () => {
        match pg_target() {
            Some(target) => target,
            None => {
                eprintln!("skipped: BANTO_TEST_PG_URL unset");
                return;
            }
        }
    };
}

// --- 小道具 ------------------------------------------------------------------

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
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// テストごとに固有のテーブル名（並行実行しても衝突しない）。
fn unique_table(label: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    format!("sink_it_{label}_{}_{nanos}", std::process::id())
}

/// 設計 §5.3 の推奨 DDL そのもの（サイドカーは DDL を発行しないので、
/// テストが DBA の代わりに作る）。
async fn create_sink_table(pool: &PgPool, table: &str) {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE TABLE \"{table}\" (ts timestamptz NOT NULL, tag_id bigint NOT NULL, \
         external_name text NOT NULL, value double precision, quality text NOT NULL)"
    )))
    .execute(pool)
    .await
    .expect("create sink table");
}

async fn drop_sink_table(pool: &PgPool, table: &str) {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP TABLE IF EXISTS \"{table}\""
    )))
    .execute(pool)
    .await
    .expect("drop sink table");
}

#[derive(Debug, PartialEq)]
struct StoredRow {
    tag_id: i64,
    external_name: String,
    value: Option<f64>,
    quality: String,
}

async fn stored_rows(pool: &PgPool, table: &str) -> Vec<StoredRow> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT tag_id, external_name, value, quality FROM \"{table}\" ORDER BY ts"
    )))
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    rows.into_iter()
        .map(|row| StoredRow {
            tag_id: row.get::<i64, _>("tag_id"),
            external_name: row.get::<String, _>("external_name"),
            value: row.get::<Option<f64>, _>("value"),
            quality: row.get::<String, _>("quality"),
        })
        .collect()
}

// --- Hub のテストハーネス（tests/db_source.rs の TestApp の写し） -------------

struct TestApp {
    server: Option<banto_server::RunningServer>,
    router: Router,
    token: String,
    port: u16,
    pool: SqlitePool,
    manager: Arc<CollectorManager>,
    _temp: tempfile::TempDir,
}

impl TestApp {
    /// DB Source のタスクと収集タスクを止め、SQLite を閉じる。
    /// `TempDir` の後始末が Windows でも成功するよう、明示的に呼ぶ
    /// （`banto-hub-core` の `tests/common/mod.rs` の
    /// 「Why `TestApp` also needs `shutdown_test_app`」参照）。
    async fn shutdown(mut self) {
        self.manager.db_source_engine().shutdown().await;
        self.manager.shutdown().await;
        if let Some(server) = self.server.take() {
            server.stop().await;
        }
        self.pool.close().await;
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

async fn test_app() -> TestApp {
    let temp = tempfile::Builder::new()
        .prefix("banto-hub-sink-it-")
        .tempdir()
        .expect("temp dir");
    let registry_path = temp.path().join("registry.sqlite3");
    let data_dir = temp.path().join("data");
    let pool = init_db(&registry_path).await.expect("init_db");

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
        data_dir,
        Arc::new(SystemClock),
        fast_options(),
        sessions,
        sim_registry,
        computed,
    ));
    manager.rebuild().await.expect("initial rebuild");

    let (events_tx, _rx) = broadcast::channel(16);
    let write_control = Arc::new(WriteControl::new(false));
    write_control.enable();
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
        // 実運用の `default` プロファイルを絶対に名乗らない（このファイルの
        // モジュール doc 参照）。
        format!("sink-it-{}", std::process::id()),
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
    let port = server.local_addr().port();

    TestApp {
        server: Some(server),
        router,
        token,
        port,
        pool,
        manager,
        _temp: temp,
    }
}

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
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
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
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// 接続 + クエリグループ + `db` タグを作って rebuild する（`db` 系の
/// レジストリ項目は S3 の UI 前なので REST ではなくサービス直叩き -
/// `tests/db_source.rs` と同じ）。戻り値は (接続 id, タグ id, 外部名)。
async fn seed_source(app: &TestApp, target: &PgTarget) -> (i64, i64, String) {
    let connection = PlcConnectionService::new(app.pool.clone())
        .create(PlcConnectionInput {
            name: "erp".to_string(),
            protocol: POSTGRES_PROTOCOL.to_string(),
            host: target.host.clone(),
            port: target.port,
            unit_id: 1,
            enabled: true,
            simulation: false,
            word_order: "low_high".to_string(),
            database: Some(target.database.clone()),
            username: Some(target.username.clone()),
            password: target.password.clone(),
        })
        .await
        .expect("postgres connection");
    let group = CollectionGroupService::new(app.pool.clone())
        .create(CollectionGroupInput {
            name: "q1".to_string(),
            plc_connection_id: connection.id,
            period_ms: SOURCE_PERIOD_MS,
            enabled: true,
            default_writable: true,
            query_sql: Some("SELECT 42.5 AS a".to_string()),
        })
        .await
        .expect("query group");
    let tag = TagService::new(app.pool.clone())
        .create(TagInput {
            name: "a".to_string(),
            collection_group_id: group.id,
            address: "a".to_string(),
            data_type: "f32".to_string(),
            string_length: None,
            string_encoding: "utf8".to_string(),
            raw_lo: None,
            raw_hi: None,
            eng_lo: None,
            eng_hi: None,
            unit: None,
            decimals: 3,
            threshold_h: None,
            threshold_hh: None,
            threshold_l: None,
            threshold_ll: None,
            enabled: true,
            writable: false,
            tag_kind: DB_TAG_KIND.to_string(),
            expression: None,
            retain: false,
            expected_revision: None,
        })
        .await
        .expect("db tag");
    app.manager.rebuild().await.expect("rebuild after seeding");
    (connection.id, tag.id, "erp.q1.a".to_string())
}

/// サイドカー専用の API キー: `admin`（`/api/sink/*`）と `read`
/// （SDK の `/api/v1/*` 購読）の両方が要る - `banto_hub_core::api_keys` の
/// `admin` は read/write と直交しているため（`banto_hub_sink::config` の
/// モジュール doc「API キーに必要なスコープ」）。
async fn issue_sidecar_key(app: &TestApp) -> String {
    let (status, body) = write_json(
        &app.router,
        "POST",
        "/api/api-keys",
        &app.token,
        json!({ "name": "sidecar", "scopes": ["admin", "read"] }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body:?}");
    body["key"].as_str().expect("issued key").to_string()
}

async fn create_sink_group(app: &TestApp, db_connection_id: i64, tag_id: i64, table: &str) -> i64 {
    let (status, body) = write_json(
        &app.router,
        "POST",
        "/api/sink/groups",
        &app.token,
        json!({
            "name": "line1-log",
            "dbConnectionId": db_connection_id,
            "mode": "interval",
            "intervalMs": SINK_INTERVAL_MS,
            "tableName": table,
            "storeBad": false,
            "enabled": true,
            "tagIds": [tag_id],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    body["id"].as_i64().expect("sink group id")
}

async fn start_collection(app: &TestApp) {
    let (status, body) = write_json(
        &app.router,
        "POST",
        "/api/collection/start",
        &app.token,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["state"], json!("running"), "{body:?}");
}

/// `GET /api/status` の `sink` 節（S4 が足した節）。
async fn sink_status(app: &TestApp) -> Value {
    let (status, body) = get_json(&app.router, "/api/status", &app.token).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    body["sink"].clone()
}

fn sink_group_state(sink: &Value, group_id: i64) -> Option<String> {
    sink["groups"]
        .as_array()?
        .iter()
        .find(|entry| entry["id"].as_i64() == Some(group_id))
        .and_then(|entry| entry["state"].as_str())
        .map(str::to_string)
}

fn sidecar_toml(port: u16, api_key: &str) -> String {
    format!(
        "hub_url = \"http://127.0.0.1:{port}\"\n\
         api_key = \"{api_key}\"\n\
         config_refresh_secs = 5\n\
         status_push_secs = 1\n\
         queue_max_rows = 1000\n\
         flush_interval_ms = 200\n\
         batch_size = 50\n\
         shutdown_flush_secs = 5\n"
    )
}

/// 統合テスト用の実行時オプション: テーブル検査の再検査を 1 秒に縮める
/// （本番の既定は設計どおり 30 秒 - このファイルのモジュール doc 参照）。
fn fast_sidecar_options() -> SidecarOptions {
    SidecarOptions {
        table_recheck: Duration::from_secs(1),
        health_tick: Duration::from_millis(500),
    }
}

// --- 本体 --------------------------------------------------------------------

/// S5 の完了条件を 1 本で通す:
///
/// 1. `interval` グループが実 PostgreSQL へ行を書く（値・quality・
///    `tag_id`・`external_name` が正しい）
/// 2. `GET /api/status` の `sink.sidecar.state` が `online`、グループが
///    `running`
/// 3. テーブルを落とすとそのグループが `error` + `lastError`
/// 4. 作り直すと自力で復帰し、キューに残っていた行が流れる（at-least-once）
/// 5. 停止すると最後の状態 push をしてから終わる
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sidecar_writes_rows_recovers_from_a_missing_table_and_pushes_a_final_status() {
    let target = require_pg!();
    let db = PgPool::connect(&target.url).await.expect("connect test pg");
    let table = unique_table("main");
    create_sink_table(&db, &table).await;

    let app = test_app().await;
    let (db_connection_id, tag_id, external_name) = seed_source(&app, &target).await;
    let group_id = create_sink_group(&app, db_connection_id, tag_id, &table).await;
    let api_key = issue_sidecar_key(&app).await;
    start_collection(&app).await;

    // サイドカーを **プロセス内** で起動する（設定ファイルは生成した
    // 一時ファイル - 実運用と同じ読み込み経路を通す）。
    let config_dir = tempfile::Builder::new()
        .prefix("banto-hub-sink-cfg-")
        .tempdir()
        .expect("config dir");
    let config_path = config_dir.path().join("banto-hub-sink.toml");
    std::fs::write(&config_path, sidecar_toml(app.port, &api_key)).expect("write toml");
    let config = banto_hub_sink::config::load_config_from(&config_path).expect("load config");

    let shutdown = Arc::new(Notify::new());
    let shutdown_signal = shutdown.clone();
    let sidecar = tokio::spawn(async move {
        run(config, fast_sidecar_options(), async move {
            shutdown_signal.notified().await;
        })
        .await
    });

    // 1. 行が入る。
    let wrote = wait_until(WAIT, || async {
        !stored_rows(&db, &table).await.is_empty()
    })
    .await;
    assert!(wrote, "サイドカーが行を書き込めていない");
    let rows = stored_rows(&db, &table).await;
    let first = &rows[0];
    assert_eq!(first.tag_id, tag_id);
    assert_eq!(first.external_name, external_name);
    assert_eq!(first.value, Some(42.5));
    assert_eq!(first.quality, "good");

    // 2. Hub 側の状態（S4 の `GET /api/status` の sink 節）。
    let online = wait_until(WAIT, || async {
        let sink = sink_status(&app).await;
        sink["sidecar"]["state"] == json!("online")
            && sink_group_state(&sink, group_id).as_deref() == Some("running")
    })
    .await;
    assert!(
        online,
        "sink.sidecar.state=online / group=running にならない: {:?}",
        sink_status(&app).await
    );

    // 3. テーブルを落とす → そのグループだけ error + lastError。
    drop_sink_table(&db, &table).await;
    let errored = wait_until(WAIT, || async {
        let sink = sink_status(&app).await;
        sink_group_state(&sink, group_id).as_deref() == Some("error")
    })
    .await;
    assert!(
        errored,
        "テーブルが無いのに error にならない: {:?}",
        sink_status(&app).await
    );
    let sink = sink_status(&app).await;
    let entry = sink["groups"]
        .as_array()
        .expect("groups")
        .iter()
        .find(|entry| entry["id"].as_i64() == Some(group_id))
        .expect("group entry")
        .clone();
    let last_error = entry["lastError"].as_str().unwrap_or_default().to_string();
    assert!(!last_error.is_empty(), "lastError が空: {entry:?}");
    assert!(
        !last_error.contains(&api_key),
        "lastError に API キーが混ざっている"
    );

    // 4. 作り直すと自力で復帰し、キューに残っていた行が流れる。
    create_sink_table(&db, &table).await;
    let recovered = wait_until(WAIT, || async {
        let sink = sink_status(&app).await;
        sink_group_state(&sink, group_id).as_deref() == Some("running")
            && !stored_rows(&db, &table).await.is_empty()
    })
    .await;
    assert!(
        recovered,
        "テーブルを戻しても復帰しない: {:?}",
        sink_status(&app).await
    );

    // 5. 停止 - 最後の状態 push をしてから終わる。
    let last_seen_before = sink_status(&app).await["sidecar"]["lastSeenAt"]
        .as_i64()
        .expect("lastSeenAt");
    // Hub の時計（ミリ秒）で「後の push」だと分かるよう、1 ミリ秒以上
    // 進めてから止める。
    tokio::time::sleep(Duration::from_millis(5)).await;
    shutdown.notify_one();
    let joined = tokio::time::timeout(Duration::from_secs(30), sidecar).await;
    assert!(joined.is_ok(), "サイドカーが停止しない");
    joined
        .unwrap()
        .expect("sidecar task panicked")
        .expect("sidecar run() should not fail");

    let last_seen_after = sink_status(&app).await["sidecar"]["lastSeenAt"]
        .as_i64()
        .expect("lastSeenAt");
    assert!(
        last_seen_after > last_seen_before,
        "停止時の最後の状態 push が届いていない（{last_seen_before} → {last_seen_after}）"
    );

    drop_sink_table(&db, &table).await;
    db.close().await;
    app.shutdown().await;
}

/// `on_change` モード: 値が変わらない間は行が増えない（設計 §5.2 の
/// 「変化時」と `docs/banto-tagclient-design.md` §4.5「静止した環境では
/// フレームが来ない」の組み合わせ）。DB Source が定数 `SELECT 42.5` を
/// 返し続けるので、最初の 1 行の後は増えないのが正しい。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn on_change_mode_writes_once_for_a_constant_value() {
    let target = require_pg!();
    let db = PgPool::connect(&target.url).await.expect("connect test pg");
    let table = unique_table("onchange");
    create_sink_table(&db, &table).await;

    let app = test_app().await;
    let (db_connection_id, tag_id, _) = seed_source(&app, &target).await;
    let (status, body) = write_json(
        &app.router,
        "POST",
        "/api/sink/groups",
        &app.token,
        json!({
            "name": "line1-changes",
            "dbConnectionId": db_connection_id,
            "mode": "on_change",
            "intervalMs": 100,
            "tableName": table,
            "storeBad": false,
            "enabled": true,
            "tagIds": [tag_id],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let api_key = issue_sidecar_key(&app).await;
    start_collection(&app).await;

    let config_dir = tempfile::Builder::new()
        .prefix("banto-hub-sink-cfg-")
        .tempdir()
        .expect("config dir");
    let config_path = config_dir.path().join("banto-hub-sink.toml");
    std::fs::write(&config_path, sidecar_toml(app.port, &api_key)).expect("write toml");
    let config: SidecarConfig =
        banto_hub_sink::config::load_config_from(&config_path).expect("load config");

    let shutdown = Arc::new(Notify::new());
    let shutdown_signal = shutdown.clone();
    let sidecar = tokio::spawn(async move {
        run(config, fast_sidecar_options(), async move {
            shutdown_signal.notified().await;
        })
        .await
    });

    let wrote = wait_until(WAIT, || async {
        !stored_rows(&db, &table).await.is_empty()
    })
    .await;
    assert!(wrote, "on_change でも初回の値は 1 行書かれる");

    // 値が変わらない間は増えない（quality の揺れが無い限り 1 行のまま）。
    tokio::time::sleep(Duration::from_secs(2)).await;
    let rows = stored_rows(&db, &table).await;
    assert_eq!(
        rows.len(),
        1,
        "定数値なのに行が増えている（on_change の変化検出が効いていない）: {rows:?}"
    );

    shutdown.notify_one();
    let joined = tokio::time::timeout(Duration::from_secs(30), sidecar).await;
    assert!(joined.is_ok(), "サイドカーが停止しない");

    drop_sink_table(&db, &table).await;
    db.close().await;
    app.shutdown().await;
}

/// Hub が起動していない状態で始めても**終了しない**（設計 §5.6
/// 「Hub 未起動なら SDK のバックオフで待つ」）。BANTO_TEST_PG_URL は
/// 要らないので、このテストだけは常に走る。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn starting_without_a_hub_waits_instead_of_exiting() {
    // 誰も待ち受けていないポート（bind → drop で番号だけ拝借する）。
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);

    let config_dir = tempfile::Builder::new()
        .prefix("banto-hub-sink-cfg-")
        .tempdir()
        .expect("config dir");
    let config_path = config_dir.path().join("banto-hub-sink.toml");
    std::fs::write(&config_path, sidecar_toml(port, "bh_absent.key")).expect("write toml");
    let config = banto_hub_sink::config::load_config_from(&config_path).expect("load config");

    let shutdown = Arc::new(Notify::new());
    let shutdown_signal = shutdown.clone();
    let sidecar = tokio::spawn(async move {
        run(config, fast_sidecar_options(), async move {
            shutdown_signal.notified().await;
        })
        .await
    });

    // 数秒回しても終わらない（＝待ち続けている）。
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!sidecar.is_finished(), "Hub 未起動で終了してしまった");

    shutdown.notify_one();
    let joined = tokio::time::timeout(Duration::from_secs(30), sidecar).await;
    assert!(joined.is_ok(), "停止要求で終われない");
    joined
        .unwrap()
        .expect("sidecar task panicked")
        .expect("run() should end cleanly");
}
