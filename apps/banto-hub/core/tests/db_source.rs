//! 外部 DB 連携 S2 の統合テスト（docs/banto-hub-external-db-design.md §7 の
//! スライス表「S2 の完了条件: ローカル PostgreSQL に対する統合テスト -
//! 正常・NULL・0 行・クエリエラー・接続断からの復帰」）。
//!
//! **実 PostgreSQL が要る。** `BANTO_TEST_PG_URL` が設定されていなければ
//! 各テストは `eprintln!("skipped: BANTO_TEST_PG_URL unset")` を出して即
//! 成功する（この workspace の他の実機依存テスト -
//! `banto_hub_core::db_source` の接続テスト、`banto-broker` の実 PLC 例 -
//! と同じゲート方式）。CI では `.github/workflows/ci.yml` の
//! `Rust (db-source / PostgreSQL)` ジョブが `ubuntu-latest` +
//! `services: postgres` で設定する（Rust 本体ジョブは `windows-latest` で
//! サービスコンテナが使えないため、別ジョブに分けてある）。
//!
//! ローカルでは:
//!
//! ```text
//! BANTO_TEST_PG_URL=postgres://postgres:postgres@127.0.0.1:5432/postgres \
//!   cargo test -p banto-hub-core --test db_source
//! ```
//!
//! ## DDL をほとんど使わない理由
//!
//! ほとんどのシナリオは `SELECT 1.5 AS a, true AS b, ...` のように**表を
//! 一切参照しない SELECT** で書ける - 権限も後始末も要らず、並行実行して
//! も衝突しない。表が要るのは「一度読めた後に読めなくなる」ケース
//! （Stale → Bad の梯子）だけで、そこはテストごとに固有名の表を作って
//! 最後に落とす。
//!
//! `tests/t11_batch_tags.rs` と同じ理由で `TestApp`/`get_json`/`write_json`
//! 相当をこのファイル内に複製している（各 `tests/*.rs` は独立クレートと
//! してコンパイルされ、private helper を共有できない）。`TempEnv` は
//! `tests/common/mod.rs` に集約済み。

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
    TagInput, TagService, DB_TAG_KIND, POSTGRES_PROTOCOL,
};
use banto_tstore::SystemClock;
use serde_json::{json, Value};
use sqlx::postgres::PgConnectOptions;
use sqlx::SqlitePool;
use tokio::sync::broadcast;
use tower::ServiceExt;

mod common;
use common::TempEnv;

const CLIENT_HEADER: (&str, &str) = ("X-Banto-Client", "banto");
const TEMP_ENV_PREFIX: &str = "banto-hub-db-source-it";

/// このファイルのグループ周期。クエリタイムアウトは設計どおり
/// `period_ms * 0.8`（= 400ms）になるので、ループバックの PostgreSQL に
/// 対しては十分な余裕がある一方、テスト全体は数秒で終わる。
const PERIOD_MS: i64 = 500;

/// `wait_until` の上限。CI のコンテナ起動直後でも数周期は待てる長さ。
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

/// `BANTO_TEST_PG_URL` が無ければ `None`（呼び出し側が skip する）。
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
        // `PgConnectOptions` はパスワードを読み出せない（write-only な
        // builder フィールド）ので URL から取り出す - `db_source` の
        // ユニットテストと同じ小道具。
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

// --- テストハーネス ----------------------------------------------------------

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

fn pg_conn_input(name: &str, target: &PgTarget) -> PlcConnectionInput {
    PlcConnectionInput {
        name: name.to_string(),
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
    }
}

fn query_group_input(name: &str, conn_id: i64, sql: &str) -> CollectionGroupInput {
    CollectionGroupInput {
        name: name.to_string(),
        plc_connection_id: conn_id,
        period_ms: PERIOD_MS,
        enabled: true,
        default_writable: true,
        query_sql: Some(sql.to_string()),
    }
}

fn db_tag_input(name: &str, group_id: i64, column: &str) -> TagInput {
    TagInput {
        name: name.to_string(),
        collection_group_id: group_id,
        address: column.to_string(),
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
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

struct TestApp {
    _server: banto_server::RunningServer,
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

impl TestApp {
    /// DB Source の接続タスクを止める（テスト終了時に実 PostgreSQL への
    /// ポーリングを残さない - `crate::runtime::RunningHub::shutdown` が
    /// 本番でやっているのと同じこと。`Drop` は同期なのでここは明示呼び）。
    async fn stop_db_source(&self) {
        self.manager.db_source_engine().shutdown().await;
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
        _server: server,
        router,
        token,
        pool,
        manager,
        _env: env,
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

/// `/api/v1/*` への書き込みはセッショントークンでは行えない
/// （`session_token_cannot_write`）ので、write スコープ付きの API キーを
/// 発行する - `tests/computed.rs` の同名ヘルパと同じ。
async fn issue_key(app: &TestApp, name: &str, scopes: &[&str]) -> String {
    let (status, body) = write_json(
        &app.router,
        "POST",
        "/api/api-keys",
        &app.token,
        json!({ "name": name, "scopes": scopes }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body:?}");
    body["key"]
        .as_str()
        .expect("a freshly issued key should carry its secret")
        .to_string()
}

/// 1タグ分の `(value, quality)` を `/api/v1/values/{external}` から読む。
async fn value_of(app: &TestApp, external: &str) -> (Option<f64>, String) {
    let (status, body) = get_json(
        &app.router,
        &format!("/api/v1/values/{external}"),
        &app.token,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    (
        body["v"].as_f64(),
        body["q"].as_str().unwrap_or_default().to_string(),
    )
}

/// レジストリを直接叩いて「接続1つ + グループ1つ + タグ N 本」を作り、
/// rebuild する（REST 経由の登録は S3 の UI が来るまで使わない）。
async fn seed(app: &TestApp, conn: PlcConnectionInput, sql: &str, columns: &[&str]) -> i64 {
    let conn = PlcConnectionService::new(app.pool.clone())
        .create(conn)
        .await
        .expect("postgres connection should be created");
    let group = CollectionGroupService::new(app.pool.clone())
        .create(query_group_input("q1", conn.id, sql))
        .await
        .expect("query group should be created");
    for column in columns {
        TagService::new(app.pool.clone())
            .create(db_tag_input(column, group.id, column))
            .await
            .unwrap_or_else(|e| panic!("db tag {column} should be created: {e:?}"));
    }
    app.manager.rebuild().await.expect("rebuild after seeding");
    group.id
}

// ---------------------------------------------------------------------------
// 1. 正常・NULL・未対応型（設計 §4.3 の表・§4.4）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn values_qualities_and_unsupported_columns_from_a_real_postgresql() {
    let target = require_pg!();
    let app = test_app("values").await;
    seed(
        &app,
        pg_conn_input("erp", &target),
        "SELECT 1.5 AS a, true AS b, NULL::int AS c, now() AS ts, 'x'::text AS s",
        &["a", "b", "c", "ts", "s"],
    )
    .await;

    let ready = wait_until(WAIT, || async {
        value_of(&app, "erp.q1.a").await.1 == "good"
    })
    .await;
    assert!(
        ready,
        "the db source should have produced a value for erp.q1.a"
    );

    // 数値: numeric → float8。
    assert_eq!(value_of(&app, "erp.q1.a").await, (Some(1.5), "good".into()));
    // bool → 1.0/0.0。
    assert_eq!(value_of(&app, "erp.q1.b").await, (Some(1.0), "good".into()));
    // NULL 列はタグ単位で Bad（§4.3）。
    assert_eq!(value_of(&app, "erp.q1.c").await, (None, "bad".into()));
    // timestamptz → epoch ミリ秒。
    let (ts, quality) = value_of(&app, "erp.q1.ts").await;
    assert_eq!(quality, "good");
    let now_ms = chrono_free_now_ms();
    let ts = ts.expect("timestamp column should carry a value");
    assert!(
        (ts - now_ms as f64).abs() < 3_600_000.0,
        "epoch ms out of range: {ts} vs {now_ms}"
    );
    // text 列は v1 非対応 → タグ単位で Bad（§4.4・§6-5）。
    assert_eq!(value_of(&app, "erp.q1.s").await, (None, "bad".into()));

    app.stop_db_source().await;
}

/// `std::time::SystemTime` から epoch ms（このクレートは chrono を持たない）。
fn chrono_free_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before 1970")
        .as_millis() as i64
}

// ---------------------------------------------------------------------------
// 2. 0 行（§4.3「0 行 → グループ単位 Bad」）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zero_rows_marks_the_whole_group_bad() {
    let target = require_pg!();
    let app = test_app("zero-rows").await;
    seed(
        &app,
        pg_conn_input("erp", &target),
        "SELECT 1 AS a WHERE false",
        &["a"],
    )
    .await;

    let reported = wait_until(WAIT, || async {
        let (_, body) = get_json(&app.router, "/api/status", &app.token).await;
        body["dbSource"][0]["groups"][0]["rowCountLast"] == json!(0)
    })
    .await;
    assert!(reported, "the status section should report 0 rows");
    assert_eq!(value_of(&app, "erp.q1.a").await, (None, "bad".into()));

    let (_, body) = get_json(&app.router, "/api/status", &app.token).await;
    assert_eq!(body["dbSource"][0]["state"], json!("connected"));
    assert!(body["dbSource"][0]["groups"][0]["lastError"]
        .as_str()
        .unwrap_or_default()
        .contains("0 行"));

    app.stop_db_source().await;
}

// ---------------------------------------------------------------------------
// 3. クエリエラー（§4.3「クエリエラー → グループ単位」）
// ---------------------------------------------------------------------------

/// 存在しない表を参照する SQL は**起動時の describe で**落ちる（設計 §4.2
/// の describe + 生成ラッパー方式の帰結、`crate::db_source::convert` の
/// モジュール doc comment）。その場合そのグループは「SQL を投げない」形に
/// なり、配下の全タグが Bad・グループの `lastError` に理由が残る - 他の
/// グループ/接続には波及しない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_statement_that_cannot_be_prepared_marks_its_group_bad_without_killing_the_connection() {
    let target = require_pg!();
    let app = test_app("statement-error").await;
    seed(
        &app,
        pg_conn_input("erp", &target),
        "SELECT * FROM banto_s2_missing_table",
        &["a"],
    )
    .await;

    let reported = wait_until(WAIT, || async {
        let (_, body) = get_json(&app.router, "/api/status", &app.token).await;
        body["dbSource"][0]["groups"][0]["lastError"].is_string()
    })
    .await;
    assert!(reported, "the group's lastError should be filled in");

    assert_eq!(value_of(&app, "erp.q1.a").await, (None, "bad".into()));
    let (_, body) = get_json(&app.router, "/api/status", &app.token).await;
    // 接続そのものは生きている（文の失敗を接続断に格上げしない）。
    assert_eq!(body["dbSource"][0]["state"], json!("connected"));

    app.stop_db_source().await;
}

/// 一度読めた後に読めなくなる（表を DROP する）場合は §4.3 の梯子どおり
/// 「直前値 + Stale」→「2周期連続で Bad」へ落ちる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_query_that_starts_failing_goes_stale_then_bad() {
    let target = require_pg!();
    // 固定のリテラル文（sqlx 0.9 の `SqlSafeStr` は `format!` で組み立てた
    // 動的文字列を受け付けない - このテストは表名を組み立てる必要が無い）。
    let admin = sqlx::postgres::PgPool::connect(&target.url)
        .await
        .expect("connect to BANTO_TEST_PG_URL");
    let _ = sqlx::query("DROP TABLE IF EXISTS banto_s2_stale_ladder")
        .execute(&admin)
        .await;
    sqlx::query("CREATE TABLE banto_s2_stale_ladder (a double precision)")
        .execute(&admin)
        .await
        .expect("create the scenario table");
    sqlx::query("INSERT INTO banto_s2_stale_ladder (a) VALUES (7.25)")
        .execute(&admin)
        .await
        .expect("seed one row");

    let app = test_app("stale-ladder").await;
    seed(
        &app,
        pg_conn_input("erp", &target),
        "SELECT a FROM banto_s2_stale_ladder",
        &["a"],
    )
    .await;

    let good = wait_until(WAIT, || async {
        value_of(&app, "erp.q1.a").await == (Some(7.25), "good".into())
    })
    .await;
    assert!(good, "the tag should first read Good");

    sqlx::query("DROP TABLE banto_s2_stale_ladder")
        .execute(&admin)
        .await
        .expect("drop the scenario table");

    // 1回目の失敗: 直前値を残したまま Stale。
    let stale = wait_until(WAIT, || async {
        let (value, quality) = value_of(&app, "erp.q1.a").await;
        quality == "stale" && value == Some(7.25)
    })
    .await;
    assert!(
        stale,
        "the first failure must keep the previous value as Stale"
    );

    // 2周期連続で失敗したら Bad。
    let bad = wait_until(WAIT, || async {
        value_of(&app, "erp.q1.a").await == (None, "bad".into())
    })
    .await;
    assert!(bad, "two consecutive failures must degrade to Bad");

    app.stop_db_source().await;
    let _ = sqlx::query("DROP TABLE IF EXISTS banto_s2_stale_ladder")
        .execute(&admin)
        .await;
    admin.close().await;
}

// ---------------------------------------------------------------------------
// 4. 接続断（§4.3「接続断（バックオフ中）→ 接続単位 Bad」・§4.2 バックオフ）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unreachable_endpoint_marks_every_tag_bad_and_reports_backoff() {
    let target = require_pg!();
    let app = test_app("unreachable").await;
    let mut conn = pg_conn_input("erp", &target);
    // 誰も listen していないポート（設計 §4.2「DB 停止で Hub 本体を
    // 止めない」の受け入れ条件そのもの - Hub は動き続ける）。
    conn.port = 1;
    seed(&app, conn, "SELECT 1.5 AS a, 2.5 AS b", &["a", "b"]).await;

    let backing_off = wait_until(WAIT, || async {
        let (_, body) = get_json(&app.router, "/api/status", &app.token).await;
        body["dbSource"][0]["state"] == json!("backoff")
            && body["dbSource"][0]["consecutiveFailures"]
                .as_i64()
                .unwrap_or(0)
                >= 1
    })
    .await;
    assert!(backing_off, "an unreachable endpoint should report backoff");

    assert_eq!(value_of(&app, "erp.q1.a").await, (None, "bad".into()));
    assert_eq!(value_of(&app, "erp.q1.b").await, (None, "bad".into()));

    // 秘密が状態に漏れない（`crate::db_source::status` のモジュール doc）。
    let (_, body) = get_json(&app.router, "/api/status", &app.token).await;
    let rendered = body.to_string();
    if let Some(password) = target.password.as_deref() {
        assert!(
            !rendered.contains(password),
            "the dbSource status must never carry the plaintext password: {rendered}"
        );
    }

    // Hub 本体は健全なまま（他の API がそのまま応答する）。
    let (status, _) = get_json(&app.router, "/api/v1/tags", &app.token).await;
    assert_eq!(status, StatusCode::OK);

    app.stop_db_source().await;
}

// ---------------------------------------------------------------------------
// 5. 稼働中の構成変更（§4.5）: pending queue 経由でタグを1本足す
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tag_added_through_the_pending_queue_starts_getting_values() {
    let target = require_pg!();
    let app = test_app("catalog-change").await;
    let group_id = seed(
        &app,
        pg_conn_input("erp", &target),
        "SELECT 1.5 AS a, 2.5 AS b",
        &["a"],
    )
    .await;

    let good = wait_until(WAIT, || async {
        value_of(&app, "erp.q1.a").await == (Some(1.5), "good".into())
    })
    .await;
    assert!(good, "the seeded tag should read Good first");

    // 収集稼働中は構成変更が pending queue へ積まれる（設計 §4.5 -
    // DB Source も PLC 側と同じこの1本の経路に乗る）。
    let (status, _) = write_json(
        &app.router,
        "POST",
        "/api/collection/start",
        &app.token,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = write_json(
        &app.router,
        "POST",
        "/api/tags",
        &app.token,
        json!({
            "name": "b",
            "collectionGroupId": group_id,
            "address": "b",
            "dataType": "f32",
            "tagKind": "db",
            "enabled": true,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body:?}");
    let pending_id = body["pending"]["id"]
        .as_i64()
        .unwrap_or_else(|| panic!("pending id should exist: {body:?}"));

    let (status, _) = write_json(
        &app.router,
        "POST",
        "/api/collection/stop",
        &app.token,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = write_json(
        &app.router,
        "POST",
        &format!("/api/pending-changes/{pending_id}/apply"),
        &app.token,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");

    // 適用後、新しいタグが値を取り始める（`commit_catalog` が
    // `DbSourceEngine::commit` を呼ぶ - §4.5）。
    let good = wait_until(WAIT, || async {
        value_of(&app, "erp.q1.b").await == (Some(2.5), "good".into())
    })
    .await;
    assert!(good, "the newly applied tag should start getting values");
    // 既存タグも生き続けている。
    assert_eq!(value_of(&app, "erp.q1.a").await, (Some(1.5), "good".into()));

    app.stop_db_source().await;
}

// ---------------------------------------------------------------------------
// 6. 状態 API（§4.2 の運転状態）と 7. 書き込み拒否（§6-10）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_exposes_the_db_source_section_and_writes_are_rejected() {
    let target = require_pg!();
    let app = test_app("status-and-write").await;
    seed(
        &app,
        pg_conn_input("erp", &target),
        "SELECT 1.5 AS a",
        &["a"],
    )
    .await;

    // `connected` はプールが張れた時点で立つので、最初の1周期が終わった
    // ことまで確かめる（`lastOkAt` はポーリング成功でしか埋まらない）。
    let polled = wait_until(WAIT, || async {
        let (_, body) = get_json(&app.router, "/api/status", &app.token).await;
        body["dbSource"][0]["state"] == json!("connected")
            && body["dbSource"][0]["groups"][0]["lastOkAt"]
                .as_i64()
                .is_some()
    })
    .await;
    assert!(polled, "the db source should report a completed poll");

    let (_, body) = get_json(&app.router, "/api/status", &app.token).await;
    let entry = &body["dbSource"][0];
    assert_eq!(entry["connectionName"], json!("erp"));
    assert!(entry["lastPollAt"].as_i64().is_some());
    assert_eq!(entry["consecutiveFailures"], json!(0));
    assert_eq!(entry["groups"][0]["groupName"], json!("q1"));
    assert!(entry["groups"][0]["lastOkAt"].as_i64().is_some());
    assert_eq!(entry["groups"][0]["rowCountLast"], json!(1));

    // `/api/v1/status` 側は snake_case で同じ内容。
    let (_, v1) = get_json(&app.router, "/api/v1/status", &app.token).await;
    assert_eq!(v1["db_source"][0]["state"], json!("connected"));

    // catalog の `value_source` は `db`（§6-11）。
    let (_, catalog) = get_json(&app.router, "/api/v1/tags", &app.token).await;
    let tag = catalog["tags"]
        .as_array()
        .expect("tags array")
        .iter()
        .find(|t| t["external_name"] == json!("erp.q1.a"))
        .expect("the db tag should be in the catalog");
    assert_eq!(tag["value_source"], json!("db"));
    assert_eq!(tag["tag_kind"], json!("db"));
    assert_eq!(tag["writable"], json!(false));

    // §6-10「v1 は読み取り専用」: write スコープ付きの API キーでも 403
    // （`db` タグは `writable = false` に正規化されており、write_path の
    // gate 2 と S2 で足した明示ゲートの両方が拒否する）。
    let key = issue_key(&app, "writer", &["write:erp.q1.a"]).await;
    let (status, body) = write_json(
        &app.router,
        "POST",
        "/api/v1/values/erp.q1.a",
        &key,
        json!({ "v": 99.0 }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body:?}");
    assert_eq!(body["error"], json!("not_writable"));

    app.stop_db_source().await;
}
