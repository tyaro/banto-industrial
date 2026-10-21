//! DB ブリッジと Hub 内の PostgreSQL Source の突き合わせ（R2a-3c、
//! docs/banto-hub-ingest-design.md §10.4。**R2a の完了条件**）。
//!
//! ## 構成（§10.4）
//!
//! `tests/bridge.rs` と同じく 1 つのテストプロセスの中に、
//!
//! - **PostgreSQL**（`BANTO_TEST_PG_URL`）に場面ごとの表と、その表だけを読める使い捨ての
//!   ロールを作る（「DB の停止」はロールの `NOLOGIN` とセッションの切断で作る。サーバーを
//!   止めずに済み、並行する他のテストを巻き込まない）。
//! - **Hub**（プロセス内、一時ディレクトリの SQLite）に、そのロールで繋ぐ `postgres` 接続 P
//!   （グループ `q` の `query_sql` = 場面の SQL、`db` タグ = 列名）と、`push` 接続 Q
//!   （`settings.bridge.db_connection_id = P`、同じ SQL・同じ周期、`columns` = 同じ列、
//!   タグのキー = 列名）を作る。
//! - **ブリッジ**（`banto_hub_sink::source::BridgeSupervisor`、被試験体）を同じプロセスで
//!   起こす。Hub とは実 HTTP・実 WebSocket で、DB とは実 PostgreSQL で繋がる。
//!
//! 両方のタグを Hub の読み出しの唯一の入口 `banto_hub_core::hub::read_current` で読み、
//! **落ち着いた後**（両方が上限つきの待ちで同じ状態に着く）の値と品質を比べる。値は
//! `f64` の**ビット**で比べる（`Reading` の `PartialEq`）。時刻（`ptime`）と
//! `value_source`（`db` / `push`）は比べない。
//!
//! ## 場面と、それを確かめるテスト（§10.4 の表の 11 行）
//!
//! | #   | 場面                                         | テスト                                                    |
//! | --- | -------------------------------------------- | --------------------------------------------------------- |
//! | 1   | int・numeric・float・bool・timestamp の列    | [`parity_1_value_types_match_bit_for_bit`]                |
//! | 2   | NULL の列                                    | [`parity_2_null_columns_are_bad`]                         |
//! | 3   | text の列・SQL に無い列                      | [`parity_3_text_and_missing_columns_are_bad`]             |
//! | 4   | 0 行                                         | [`parity_4_zero_rows_are_bad`]                            |
//! | 5   | クエリの失敗（表を消す）                     | [`parity_5_query_failure_walks_the_same_ladder`]          |
//! | 6   | DB の停止（接続を断つ）                      | [`parity_6_db_outage_is_bad_and_recovers`]                |
//! | 7   | 収集の停止・開始                             | [`parity_7_stop_and_start_never_revive_the_old_value`]    |
//! | 7b  | （ブリッジだけ）開始の直後に DB を止めておく | [`parity_7b_start_with_the_db_down_stays_bad`]            |
//! | 7c  | （ブリッジだけ）`columns` から 1 つ外す      | [`parity_7c_a_key_removed_from_columns_goes_bad`]         |
//! | 8   | （ブリッジだけ）ブリッジを止める             | [`parity_8_a_stopped_bridge_goes_bad_after_the_timeout`]  |
//! | 9   | （ブリッジだけ）見積もりの上限ちょうどの設定 | [`parity_9_three_queries_at_the_registration_limit`]      |
//!
//! 9 は R2a-3b で `tests/bridge.rs` に置いた 60 秒のテストをここへ移し（重複させない）、
//! Hub 内の Source との比較を A の 1 列から全列（18 列）と B・C に広げたもの。拒否される例
//! （A の周期だけ 200 ms）は `banto_tags` の `bridge_load` の単体テスト（R2a-3b）。
//!
//! 5 は 2 つの Source が別々に問い合わせるので「同じ瞬間に同じ」ではなく「同じ状態の並びを
//! 通る」ことを確かめる（§10.4 の注）: [`Recorder`] が両方のタグを 10 ms ごとに読み、
//! 変わったときだけ控えた並びを比べる。4・7・8 も同じ並びで確かめる。
//!
//! ## 周期
//!
//! 場面のクエリ（P のグループと Q の `queries[]`）は 1,000 ms、Q の下の `push` のグループは
//! 200 ms。ブリッジの `stale` は次のクエリの失敗（1 秒後）まで保持値に残り、`push` の
//! グループの tick がそれを 5 回ほど読むので、並びに必ず現れる。`push` のグループを短く
//! しすぎると、収集の現在値の「周期 × 2.5 の間に更新が無ければ Stale」（`banto_collect`
//! の `STALE_PERIOD_FACTOR`）が負荷の高い CI で偽の Stale を作るので、200 ms（500 ms の
//! 猶予）に留める。
//!
//! ## 反証（§10.4・checklist §5、PR に書く）
//!
//! - ブリッジの梯子を 1 か所変える（`apps/banto-hub-sink/src/source/poll.rs` の
//!   `QueryState::on_query_failure` で、2 回目以降の失敗も `Stale` にする）と 5 が落ちる。
//! - `apps/banto-hub/core/Cargo.toml` の `serde_json` から `float_roundtrip` を外すと、1 の
//!   境界値（`1.2000009999999999` と `0.40000020000000003`。ブリッジは ryu の最短表現で
//!   送り、Hub は `serde_json::Value` で読む）が 1 ULP ずれて落ちる。`0.1 + 0.2`
//!   （`0.30000000000000004`）は既定の読み方でも正しく読めるので、それだけでは落ちない。
//!
//! ## 決定性
//!
//! 正の条件は [`wait_until`]（上限つきのポーリング）で待つ。負の条件（「Good に戻らない」
//! 「Bad にならない」）だけ短い固定の観測時間を使い、その間も 50 ms ごとに読み続けて、
//! 一度でも外れたら落とす。
//!
//! `BANTO_TEST_PG_URL` が無ければ `skipped` を出して成功する（`tests/sidecar.rs` と同じ）。
//! CI は `.github/workflows/ci.yml` の `Rust (db-source / PostgreSQL)` ジョブの
//! `cargo test -p banto-hub-sink` で走る。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request as HttpRequest, StatusCode};
use axum::Router;
use banto_collect::{BackoffConfig, CollectorOptions};
use banto_hub_core::api_keys::ApiKeysService;
use banto_hub_core::audit::AuditLogService;
use banto_hub_core::broker_glue::{BrokerSimRegistry, HubSessions};
use banto_hub_core::commissioning::CommissioningService;
use banto_hub_core::computed::{ComputedEngine, ServerTagStore};
use banto_hub_core::db::{init_db, Db};
use banto_hub_core::grpc::{GrpcServer, GrpcService};
use banto_hub_core::hub::{quality_str, read_current, CollectorManager};
use banto_hub_core::rest::{api_router, user_session_lookup};
use banto_hub_core::settings::SettingsService;
use banto_hub_core::users::UsersService;
use banto_hub_core::write_audit::WriteAuditService;
use banto_hub_core::write_control::WriteControl;
use banto_hub_core::write_rate::{WriteRateLimitConfig, WriteRateLimiter};
use banto_hub_sink::source::{BridgeSupervisor, SourceStatus};
use banto_hub_sink::SidecarConfig;
use banto_server::{start, AuthState, Identity, ServerConfig, SessionValidation};
use banto_tags::{
    CollectionGroupInput, CollectionGroupService, PlcConnectionInput, PlcConnectionService,
    TagInput, TagService, DB_TAG_KIND, PLC_TAG_KIND, POSTGRES_PROTOCOL, PUSH_PROTOCOL,
};
use banto_tstore::{SystemClock, TsReader};
use serde_json::{json, Value};
use sqlx::postgres::{PgConnectOptions, PgPool};
use sqlx::SqlitePool;
use tokio::sync::broadcast;
use tower::ServiceExt;

const CLIENT_HEADER: (&str, &str) = ("X-Banto-Client", "banto");

/// ポーリング待ちの上限（DB の再接続のバックオフ 1 → 2 → 4 秒をまたげる長さ）。
const WAIT: Duration = Duration::from_secs(20);

/// 場面のクエリの周期（P のグループと Q の `queries[]`）。
const QUERY_PERIOD_MS: i64 = 1_000;

/// Q の下の `push` のグループの周期（モジュールの doc「周期」）。
const PUSH_GROUP_PERIOD_MS: i64 = 200;

/// 既定の `stale_timeout_ms`（ブリッジは `/ 3` ごとに `ping`）。
const STALE_TIMEOUT_MS: u64 = 3_000;

// --- BANTO_TEST_PG_URL ゲート ------------------------------------------------

#[derive(Clone)]
struct PgTarget {
    host: String,
    port: i64,
    database: String,
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

fn unique(label: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    format!("parity_{label}_{}_{nanos}", std::process::id())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock")
        .as_millis() as i64
}

/// 場面ごとの使い捨てのロール（P とブリッジの両方がこのロールで繋ぐ）。
struct TestRole {
    name: String,
    password: String,
}

impl TestRole {
    async fn create(admin: &PgPool, label: &str) -> Self {
        let role = Self {
            name: unique(label),
            password: format!("pw-{}", unique("secret")),
        };
        execute(
            admin,
            &format!(
                "CREATE ROLE \"{}\" LOGIN PASSWORD '{}'",
                role.name, role.password
            ),
        )
        .await;
        role
    }

    async fn set_login(&self, admin: &PgPool, login: bool) {
        execute(
            admin,
            &format!(
                "ALTER ROLE \"{}\" {}",
                self.name,
                if login { "LOGIN" } else { "NOLOGIN" }
            ),
        )
        .await;
    }

    async fn terminate_sessions(&self, admin: &PgPool) {
        sqlx::query("SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE usename = $1")
            .bind(&self.name)
            .execute(admin)
            .await
            .expect("terminate sessions");
    }

    async fn drop_role(self, admin: &PgPool) {
        self.terminate_sessions(admin).await;
        let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP ROLE IF EXISTS \"{}\"",
            self.name
        )))
        .execute(admin)
        .await;
    }
}

async fn execute(admin: &PgPool, sql: &str) {
    sqlx::query(sqlx::AssertSqlSafe(sql.to_string()))
        .execute(admin)
        .await
        .unwrap_or_else(|err| panic!("{sql}: {err}"));
}

// --- Hub のテストハーネス（tests/bridge.rs の TestApp の写し） -----------------

struct TestApp {
    server: Option<banto_server::RunningServer>,
    router: Router,
    token: String,
    port: u16,
    pool: SqlitePool,
    manager: Arc<CollectorManager>,
    data_dir: PathBuf,
    _temp: tempfile::TempDir,
}

impl TestApp {
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

fn banto_drivers_helper() -> banto_hub_core::broker_glue::HubDrivers {
    let builtin = banto_drivers::builtin();
    banto_hub_core::broker_glue::HubDrivers::new(
        Arc::new(builtin.registry),
        builtin.broker,
        builtin.client_factory,
    )
}

async fn test_app() -> TestApp {
    let temp = tempfile::Builder::new()
        .prefix("banto-hub-bridge-parity-")
        .tempdir()
        .expect("temp dir");
    let pool = init_db(&temp.path().join("registry.sqlite3"))
        .await
        .expect("init_db");
    let users = UsersService::new(Db::Sqlite(pool.clone()));
    let audit = AuditLogService::new(Db::Sqlite(pool.clone()));
    users
        .setup_first_user("admin", "password123", "管理者")
        .await
        .expect("setup_first_user");
    let verify_users = users.clone();
    let verify_users_lookup = users.clone();
    let auth = AuthState::new(
        move |u: String, p: String| {
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
        },
        SessionValidation::lookup(user_session_lookup(verify_users_lookup)),
    );
    let token = auth
        .login("admin", "password123")
        .await
        .expect("admin login");
    let sessions = Arc::new(HubSessions::new(
        banto_broker::BackoffConfig::default(),
        banto_drivers_helper(),
    ));
    let computed = Arc::new(ComputedEngine::new(Arc::new(ServerTagStore::new())));
    let data_dir = temp.path().join("data");
    let manager = Arc::new(CollectorManager::new(
        pool.clone(),
        data_dir.clone(),
        Arc::new(SystemClock),
        fast_options(),
        sessions,
        Arc::new(BrokerSimRegistry::new()),
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
    let settings = SettingsService::new(Db::Sqlite(pool.clone()));
    let commissioning = CommissioningService::load(settings, users.clone(), auth.clone())
        .await
        .expect("CommissioningService::load");
    // 運用どおりロックダウン済み（収集中の構成の変更は pending queue + 明示の適用、7c）。
    commissioning.lock_down().await.expect("lock_down");
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
        format!("bridge-parity-{}", std::process::id()),
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
        data_dir,
        _temp: temp,
    }
}

async fn request_json(
    router: &Router,
    method: &str,
    path: &str,
    token: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = HttpRequest::builder()
        .method(method)
        .uri(path)
        .header("Authorization", format!("Bearer {token}"))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1);
    let body = match body {
        Some(body) => {
            builder = builder.header("content-type", "application/json");
            Body::from(serde_json::to_vec(&body).unwrap())
        }
        None => Body::empty(),
    };
    let response = router
        .clone()
        .oneshot(builder.body(body).unwrap())
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

async fn issue_key(app: &TestApp, name: &str, scopes: &[String]) -> String {
    let (status, body) = request_json(
        &app.router,
        "POST",
        "/api/api-keys",
        &app.token,
        Some(json!({ "name": name, "scopes": scopes })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body:?}");
    body["key"].as_str().expect("issued key").to_string()
}

async fn collection(app: &TestApp, action: &str) {
    let (status, body) = request_json(
        &app.router,
        "POST",
        &format!("/api/collection/{action}"),
        &app.token,
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
}

/// ブリッジの設定（`settings`）の 1 クエリ。
struct Query<'a> {
    name: &'a str,
    period_ms: i64,
    sql: &'a str,
    /// (列, キー)。
    columns: Vec<(String, String)>,
}

fn bridge_settings(db_connection_id: i64, stale_timeout_ms: u64, queries: &[Query<'_>]) -> Value {
    json!({ "stale_timeout_ms": stale_timeout_ms, "bridge": { "kind": "db",
        "db_connection_id": db_connection_id,
        "queries": queries.iter().map(|q| json!({
            "name": q.name, "period_ms": q.period_ms, "sql": q.sql,
            "columns": q.columns.iter().map(|(c, k)| json!({ "column": c, "key": k })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>() } })
}

fn postgres_input(name: &str, target: &PgTarget, role: &TestRole) -> PlcConnectionInput {
    PlcConnectionInput {
        name: name.to_string(),
        protocol: POSTGRES_PROTOCOL.to_string(),
        host: target.host.clone(),
        port: target.port,
        unit_id: 1,
        enabled: true,
        simulation: false,
        word_order: "low_high".to_string(),
        cpu_type: None,
        database: Some(target.database.clone()),
        username: Some(role.name.clone()),
        password: Some(role.password.clone()),
        settings: None,
    }
}

fn push_input(name: &str, settings: Value) -> PlcConnectionInput {
    PlcConnectionInput {
        name: name.to_string(),
        protocol: PUSH_PROTOCOL.to_string(),
        host: String::new(),
        port: 0,
        unit_id: 1,
        enabled: true,
        simulation: false,
        word_order: "low_high".to_string(),
        cpu_type: None,
        database: None,
        username: None,
        password: None,
        settings: Some(settings),
    }
}

fn tag_input(name: &str, group_id: i64, address: &str, data_type: &str, kind: &str) -> TagInput {
    TagInput {
        name: name.to_string(),
        collection_group_id: group_id,
        address: address.to_string(),
        data_type: data_type.to_string(),
        string_length: None,
        string_encoding: "utf8".to_string(),
        raw_lo: None,
        raw_hi: None,
        eng_lo: None,
        eng_hi: None,
        unit: None,
        decimals: 3,
        enabled: true,
        writable: false,
        tag_kind: kind.to_string(),
        expression: None,
        retain: false,
        expected_revision: None,
    }
}

async fn create_group(
    app: &TestApp,
    connection_id: i64,
    name: &str,
    period_ms: i64,
    sql: Option<&str>,
) -> i64 {
    CollectionGroupService::new(app.pool.clone())
        .create(CollectionGroupInput {
            name: name.to_string(),
            plc_connection_id: connection_id,
            period_ms,
            enabled: true,
            default_writable: false,
            query_sql: sql.map(str::to_string),
        })
        .await
        .expect("collection group")
        .id
}

fn spawn_bridge(app: &TestApp, api_key: &str) -> BridgeSupervisor {
    let config = SidecarConfig::from_toml_str(
        &format!(
            "hub_url = \"http://127.0.0.1:{}\"\n\
             api_key = \"{api_key}\"\n\
             config_refresh_secs = 5\n\
             status_push_secs = 1\n",
            app.port
        ),
        Path::new("banto-hub-sink.toml"),
    )
    .expect("config");
    BridgeSupervisor::spawn(&config).expect("bridge")
}

fn bridge_status(bridge: &BridgeSupervisor, connection_id: i64) -> Option<SourceStatus> {
    bridge
        .statuses()
        .into_iter()
        .find(|s| s.connection_id == connection_id)
}

// --- 読み出し（Hub の read_current）と状態の並び ------------------------------

/// 1 つのタグの今の値と品質（`read_current` の結果）。値は `f64` のビットで比べる。
#[derive(Clone, Copy, Debug)]
struct Reading {
    value: Option<f64>,
    quality: &'static str,
}

impl PartialEq for Reading {
    fn eq(&self, other: &Self) -> bool {
        self.quality == other.quality
            && self.value.map(f64::to_bits) == other.value.map(f64::to_bits)
    }
}

fn good(value: f64) -> Reading {
    Reading {
        value: Some(value),
        quality: "good",
    }
}

fn stale(value: f64) -> Reading {
    Reading {
        value: Some(value),
        quality: "stale",
    }
}

const BAD: Reading = Reading {
    value: None,
    quality: "bad",
};

/// 外部名 `external` のタグを、Hub の全 IF が通る `read_current` で読む。
fn read_tag(manager: &CollectorManager, external: &str) -> Reading {
    let map = manager.tag_map();
    let entry = map
        .get(external)
        .unwrap_or_else(|| panic!("tag {external} is not in the catalog"));
    let current = manager.current_values();
    let store = manager.server_store();
    let (value, quality, _) = read_current(entry, current.as_ref(), &store, now_ms());
    Reading {
        value,
        quality: quality_str(quality),
    }
}

/// タグの状態の並び（変わったときだけ控える）を 10 ms ごとに読んで集める。
struct Recorder {
    stop: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<Vec<Vec<Reading>>>,
}

impl Recorder {
    fn start(manager: Arc<CollectorManager>, externals: Vec<String>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let task = tokio::spawn(async move {
            let mut sequences: Vec<Vec<Reading>> = vec![Vec::new(); externals.len()];
            loop {
                for (sequence, external) in sequences.iter_mut().zip(&externals) {
                    let reading = read_tag(&manager, external);
                    if sequence.last() != Some(&reading) {
                        sequence.push(reading);
                    }
                }
                if flag.load(Ordering::SeqCst) {
                    return sequences;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
        Self { stop, task }
    }

    async fn finish(self) -> Vec<Vec<Reading>> {
        self.stop.store(true, Ordering::SeqCst);
        tokio::time::timeout(WAIT, self.task)
            .await
            .expect("the recorder ends")
            .expect("the recorder does not panic")
    }
}

// --- 場面の組み立て ----------------------------------------------------------

/// 場面の表の 1 列（タグの型は P の `db` タグと Q の `push` タグで同じ）。
struct Column {
    name: &'static str,
    data_type: &'static str,
}

const fn col(name: &'static str, data_type: &'static str) -> Column {
    Column { name, data_type }
}

/// 1 つの場面: 表・ロール・Hub（P と Q）・ブリッジ。
struct Fixture {
    app: TestApp,
    admin: PgPool,
    role: TestRole,
    table: String,
    pg_id: i64,
    push_id: i64,
    push_name: String,
    push_group_id: i64,
    /// 列 → Q の `push` タグの id（tstore の列を引くため）。
    push_tag_ids: HashMap<String, i64>,
    columns: Vec<String>,
    sql: String,
    stale_timeout_ms: u64,
    api_key: String,
    bridge: Option<BridgeSupervisor>,
}

impl Fixture {
    /// 表 `{table}`（`ddl` は列の定義、`rows` は `INSERT` の `VALUES` の後ろ、`None` なら
    /// 行なし）を作り、`SELECT {select} FROM {table}` を P と Q の両方に登録して、収集を
    /// 始め、ブリッジを起こす。
    async fn start(
        target: &PgTarget,
        label: &str,
        ddl: &str,
        rows: Option<&str>,
        select: &str,
        columns: &[Column],
        stale_timeout_ms: u64,
    ) -> Self {
        let admin = PgPool::connect(&target.url).await.expect("connect test pg");
        let role = TestRole::create(&admin, label).await;
        let table = unique(&format!("{label}_t"));
        execute(&admin, &format!("CREATE TABLE \"{table}\" ({ddl})")).await;
        if let Some(rows) = rows {
            execute(&admin, &format!("INSERT INTO \"{table}\" VALUES {rows}")).await;
        }
        execute(
            &admin,
            &format!("GRANT SELECT ON \"{table}\" TO \"{}\"", role.name),
        )
        .await;
        let sql = format!("SELECT {select} FROM \"{table}\"");

        let app = test_app().await;
        let connections = PlcConnectionService::new(app.pool.clone());
        let tags = TagService::new(app.pool.clone());
        // P: Hub 内の Source。
        let pg = connections
            .create(postgres_input("pg", target, &role))
            .await
            .expect("postgres connection");
        let db_group = create_group(&app, pg.id, "q", QUERY_PERIOD_MS, Some(&sql)).await;
        for column in columns {
            tags.create(tag_input(
                column.name,
                db_group,
                column.name,
                column.data_type,
                DB_TAG_KIND,
            ))
            .await
            .expect("db tag");
        }
        // Q: ブリッジ（同じ SQL・同じ周期・同じ列、キー = 列名）。
        let push_name = "bridge".to_string();
        let push = connections
            .create(push_input(
                &push_name,
                bridge_settings(
                    pg.id,
                    stale_timeout_ms,
                    &[Query {
                        name: "q",
                        period_ms: QUERY_PERIOD_MS,
                        sql: &sql,
                        columns: columns
                            .iter()
                            .map(|c| (c.name.to_string(), c.name.to_string()))
                            .collect(),
                    }],
                ),
            ))
            .await
            .expect("push connection");
        let push_group_id = create_group(&app, push.id, "g", PUSH_GROUP_PERIOD_MS, None).await;
        let mut push_tag_ids = HashMap::new();
        for column in columns {
            let tag = tags
                .create(tag_input(
                    column.name,
                    push_group_id,
                    column.name,
                    column.data_type,
                    PLC_TAG_KIND,
                ))
                .await
                .expect("push tag");
            push_tag_ids.insert(column.name.to_string(), tag.id);
        }
        app.manager.rebuild().await.expect("rebuild");
        let api_key = issue_key(
            &app,
            "bridge",
            &[
                "admin".to_string(),
                "read".to_string(),
                format!("ingest:#{}", push.id),
            ],
        )
        .await;
        collection(&app, "start").await;
        let bridge = spawn_bridge(&app, &api_key);
        Self {
            app,
            admin,
            role,
            table,
            pg_id: pg.id,
            push_id: push.id,
            push_name,
            push_group_id,
            push_tag_ids,
            columns: columns.iter().map(|c| c.name.to_string()).collect(),
            sql,
            stale_timeout_ms,
            api_key,
            bridge: Some(bridge),
        }
    }

    /// P の `db` タグの外部名。
    fn db_external(column: &str) -> String {
        format!("pg.q.{column}")
    }

    /// Q の `push` タグの外部名。
    fn push_external(&self, column: &str) -> String {
        format!("{}.g.{column}", self.push_name)
    }

    /// (Hub 内の Source, ブリッジ) の今の読み。
    fn pair(&self, column: &str) -> (Reading, Reading) {
        (
            read_tag(&self.app.manager, &Self::db_external(column)),
            read_tag(&self.app.manager, &self.push_external(column)),
        )
    }

    fn pairs(&self) -> Vec<(String, (Reading, Reading))> {
        self.columns
            .iter()
            .map(|c| (c.clone(), self.pair(c)))
            .collect()
    }

    /// 全列が、両方とも `expected` の状態に落ち着くまで待つ（落ち着かなければ落とす）。
    async fn settle(&self, expected: &[(&str, Reading)]) {
        let settled = wait_until(WAIT, || async {
            expected
                .iter()
                .all(|(column, reading)| self.pair(column) == (*reading, *reading))
        })
        .await;
        assert!(
            settled,
            "expected both sources at {expected:?}, got {:?} / bridge {:?}",
            self.pairs(),
            self.bridge.as_ref().map(|b| b.statuses())
        );
    }

    /// 列 `column` の (Hub 内の Source, ブリッジ) の並びを控え始める。
    fn record(&self, column: &str) -> Recorder {
        Recorder::start(
            self.app.manager.clone(),
            vec![Self::db_external(column), self.push_external(column)],
        )
    }

    /// `duration` の間 50 ms ごとに読み、`check` が一度でも偽なら落とす（負の条件）。
    async fn hold(&self, duration: Duration, what: &str, check: impl Fn(&Self) -> bool) {
        let deadline = tokio::time::Instant::now() + duration;
        while tokio::time::Instant::now() < deadline {
            assert!(check(self), "{what}: {:?}", self.pairs());
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn exec(&self, sql: &str) {
        execute(&self.admin, sql).await;
    }

    fn ingest_status(&self) -> banto_hub_core::ingest::IngestConnectionStatus {
        self.app
            .manager
            .ingest_hub()
            .status()
            .into_iter()
            .find(|s| s.connection_id == self.push_id)
            .expect("ingest status of the bridge connection")
    }

    /// tstore に記録された Q の `push` タグ `column` の `(ptime_ms, 値)`（全ファイル、時刻順）。
    /// 収集を止めて（書き手が flush してから）読む。
    async fn recorded(&self, column: &str) -> Vec<(i64, Option<f64>)> {
        let group_key = format!("grp:{}", self.push_group_id);
        let tag_key = format!("tag:{}", self.push_tag_ids[column]);
        let mut rows = Vec::new();
        for file in banto_tstore::list_data_files(&self.app.data_dir).expect("list data files") {
            let reader = TsReader::open(&file.path).await.expect("open data file");
            let Some(group) = reader.group(&group_key) else {
                continue;
            };
            let Some(position) = group.columns.iter().position(|c| c.tag_key == tag_key) else {
                continue;
            };
            for sample in reader
                .read_range(&group_key, i64::MIN, i64::MAX)
                .await
                .expect("read range")
            {
                rows.push((sample.ptime_ms, sample.values[position]));
            }
        }
        rows.sort_by_key(|(ptime, _)| *ptime);
        rows
    }

    async fn finish(mut self) {
        if let Some(bridge) = self.bridge.take() {
            assert!(bridge.shutdown(Duration::from_secs(10)).await);
        }
        self.app.shutdown().await;
        self.role.drop_role(&self.admin).await;
        let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP TABLE IF EXISTS \"{}\"",
            self.table
        )))
        .execute(&self.admin)
        .await;
        self.admin.close().await;
    }
}

// --- 1〜9 ------------------------------------------------------------------

/// 1: int・numeric・float・bool・timestamp の列は、両方とも同じ値（`f64` でビット一致）・
/// Good。値そのものも PostgreSQL の `float8` への変換と一致する。境界値:
/// `1.2000009999999999`（numeric）と `0.40000020000000003`（float8）は `serde_json` の
/// 既定の読み方では 1 ULP ずれる（`float_roundtrip` の反証）。`0.1 + 0.2`・float4 の 0.1・
/// 2^53 + 1 の int8（float8 で 2^53 に丸まる）も並べる。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parity_1_value_types_match_bit_for_bit() {
    let target = require_pg!();
    let fixture = Fixture::start(
        &target,
        "p1",
        "i int4, big int8, n numeric, f float8, r float4, b bool, bf bool, \
         ts timestamptz, tsn timestamp, d date",
        Some(
            "(-123456, 9007199254740993, 1.2000009999999999, 0.40000020000000003, 0.1, \
             true, false, '2026-10-21 12:34:56.789+09', '2026-10-21 12:34:56.789', '2026-10-21')",
        ),
        "i, big, n, f, (0.1::float8 + 0.2::float8) AS f2, r, b, bf, ts, tsn, d",
        &[
            col("i", "f32"),
            col("big", "f32"),
            col("n", "f32"),
            col("f", "f32"),
            col("f2", "f32"),
            col("r", "f32"),
            col("b", "bit"),
            col("bf", "bit"),
            col("ts", "f32"),
            col("tsn", "f32"),
            col("d", "f32"),
        ],
        STALE_TIMEOUT_MS,
    )
    .await;
    #[allow(clippy::excessive_precision)]
    let expected = [
        ("i", good(-123_456.0)),
        ("big", good(9_007_199_254_740_992.0)),
        ("n", good(1.2000009999999999)),
        ("f", good(0.40000020000000003)),
        ("f2", good(0.1 + 0.2)),
        ("r", good(f64::from(0.1_f32))),
        ("b", good(1.0)),
        ("bf", good(0.0)),
        // 2026-10-21T03:34:56.789Z の epoch ミリ秒。
        ("ts", good(1_792_553_696_789.0)),
    ];
    fixture.settle(&expected).await;
    // timestamp（タイムゾーンなし）と date は、セッションのタイムゾーンで解釈される値を
    // 両方が同じ変換で読む（値はサーバーの設定によるので、一致と Good だけを見る）。
    for column in ["tsn", "d"] {
        let settled = wait_until(WAIT, || async {
            let (db, push) = fixture.pair(column);
            db.quality == "good" && db == push
        })
        .await;
        assert!(settled, "{column}: {:?}", fixture.pair(column));
    }
    fixture.finish().await;
}

/// 2: NULL の列は、両方とも Bad（値なし）。同じ行の値のある列は Good のまま。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parity_2_null_columns_are_bad() {
    let target = require_pg!();
    let fixture = Fixture::start(
        &target,
        "p2",
        "a float8, nf float8, nn numeric, nb bool, nt timestamptz",
        Some("(1.5, NULL, NULL, NULL, NULL)"),
        "a, nf, nn, nb, nt",
        &[
            col("a", "f32"),
            col("nf", "f32"),
            col("nn", "f32"),
            col("nb", "bit"),
            col("nt", "f32"),
        ],
        STALE_TIMEOUT_MS,
    )
    .await;
    fixture
        .settle(&[
            ("a", good(1.5)),
            ("nf", BAD),
            ("nn", BAD),
            ("nb", BAD),
            ("nt", BAD),
        ])
        .await;
    fixture.finish().await;
}

/// 3: text の列（未対応型）と SQL に無い列は、両方とも Bad。同じクエリの値の取れる列は
/// Good のまま。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parity_3_text_and_missing_columns_are_bad() {
    let target = require_pg!();
    let fixture = Fixture::start(
        &target,
        "p3",
        "a float8, note text",
        Some("(2.25, 'hello')"),
        "a, note",
        &[col("a", "f32"), col("note", "f32"), col("gone", "f32")],
        STALE_TIMEOUT_MS,
    )
    .await;
    fixture
        .settle(&[("a", good(2.25)), ("note", BAD), ("gone", BAD)])
        .await;
    fixture.finish().await;
}

/// 4: 0 行は、両方とも Bad（梯子に乗せない - Stale を挟まない）。行を戻すと Good。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parity_4_zero_rows_are_bad() {
    let target = require_pg!();
    let fixture = Fixture::start(
        &target,
        "p4",
        "a float8",
        Some("(3.5)"),
        "a",
        &[col("a", "f32")],
        STALE_TIMEOUT_MS,
    )
    .await;
    fixture.settle(&[("a", good(3.5))]).await;
    let recorder = fixture.record("a");
    fixture
        .exec(&format!("DELETE FROM \"{}\"", fixture.table))
        .await;
    fixture.settle(&[("a", BAD)]).await;
    fixture
        .exec(&format!("INSERT INTO \"{}\" VALUES (4.5)", fixture.table))
        .await;
    fixture.settle(&[("a", good(4.5))]).await;
    let sequences = recorder.finish().await;
    let expected = vec![good(3.5), BAD, good(4.5)];
    assert_eq!(sequences[0], expected, "Hub DB Source");
    assert_eq!(sequences[1], expected, "bridge");
    fixture.finish().await;
}

/// 5: クエリの失敗（表を消す）は、両方とも「1 回目に直前値で Stale、2 回目から Bad」。
/// 表を戻すと Good。2 つの Source は別々に問い合わせるので、同じ瞬間ではなく**同じ状態の
/// 並び**を通ることを比べる（§10.4 の注）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parity_5_query_failure_walks_the_same_ladder() {
    let target = require_pg!();
    let fixture = Fixture::start(
        &target,
        "p5",
        "a float8",
        Some("(7.25)"),
        "a",
        &[col("a", "f32")],
        STALE_TIMEOUT_MS,
    )
    .await;
    fixture.settle(&[("a", good(7.25))]).await;
    let recorder = fixture.record("a");
    fixture
        .exec(&format!("DROP TABLE \"{}\"", fixture.table))
        .await;
    fixture.settle(&[("a", BAD)]).await;
    // 表を戻す（同じ名前・同じ列。ロールに読み取りを与え直す）。
    fixture
        .exec(&format!("CREATE TABLE \"{}\" (a float8)", fixture.table))
        .await;
    fixture
        .exec(&format!("INSERT INTO \"{}\" VALUES (8.5)", fixture.table))
        .await;
    fixture
        .exec(&format!(
            "GRANT SELECT ON \"{}\" TO \"{}\"",
            fixture.table, fixture.role.name
        ))
        .await;
    fixture.settle(&[("a", good(8.5))]).await;
    let sequences = recorder.finish().await;
    let expected = vec![good(7.25), stale(7.25), BAD, good(8.5)];
    assert_eq!(sequences[0], expected, "Hub DB Source");
    assert_eq!(
        sequences[1], sequences[0],
        "the bridge must walk the same ladder as the Hub DB Source"
    );
    fixture.finish().await;
}

/// 6: DB の停止（ロールのログインを止めてセッションを切る）は、両方とも Bad。復帰で Good
/// （止めている間に変えた値 = 新しい問い合わせの値）。
///
/// 途中の並びは比べない: セッションを切った後の最初の失敗が「接続の失敗」（全キー Bad）か
/// 「サーバーが応答する失敗（ログインの拒否）」（文の失敗の側、1 回目は Stale）かは、切った
/// 時点でプールの接続が使われていたかどうかの時間で決まる（Hub 内の Source とブリッジは
/// 同じ `classify` を使うが、別々のプールなので同じ瞬間に同じ側に落ちるとは限らない）。
/// 並びの中に、停止前の値以外の値と、Good・Stale・Bad 以外の状態が無いことだけを見る。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parity_6_db_outage_is_bad_and_recovers() {
    let target = require_pg!();
    let fixture = Fixture::start(
        &target,
        "p6",
        "a float8",
        Some("(5.5)"),
        "a",
        &[col("a", "f32")],
        STALE_TIMEOUT_MS,
    )
    .await;
    fixture.settle(&[("a", good(5.5))]).await;
    let recorder = fixture.record("a");
    fixture.role.set_login(&fixture.admin, false).await;
    fixture.role.terminate_sessions(&fixture.admin).await;
    fixture.settle(&[("a", BAD)]).await;
    fixture
        .exec(&format!("UPDATE \"{}\" SET a = 6.5", fixture.table))
        .await;
    fixture.role.set_login(&fixture.admin, true).await;
    fixture.settle(&[("a", good(6.5))]).await;
    let sequences = recorder.finish().await;
    for (side, sequence) in ["Hub DB Source", "bridge"].iter().zip(&sequences) {
        assert_eq!(sequence.first(), Some(&good(5.5)), "{side}: {sequence:?}");
        assert_eq!(sequence.last(), Some(&good(6.5)), "{side}: {sequence:?}");
        assert!(sequence.contains(&BAD), "{side}: {sequence:?}");
        let (outage, _) = sequence.split_at(sequence.len() - 1);
        assert!(
            outage
                .iter()
                .all(|r| *r == good(5.5) || *r == stale(5.5) || *r == BAD),
            "{side}: {sequence:?}"
        );
    }
    fixture.finish().await;
}

/// 7: 収集の停止で両方とも Bad。開始の直後の最初の tick は Bad で、停止前の Good を記録
/// しない。新しい問い合わせの後で Good（止めている間に変えた値）。並びは両方とも
/// 「停止前の値 → Bad → 新しい値」で、開始の後に停止前の値が一度も出ない。tstore にも
/// 開始の後の停止前の値の行が無い（ブリッジのタグ）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parity_7_stop_and_start_never_revive_the_old_value() {
    let target = require_pg!();
    let fixture = Fixture::start(
        &target,
        "p7",
        "a float8",
        Some("(11.5)"),
        "a",
        &[col("a", "f32")],
        STALE_TIMEOUT_MS,
    )
    .await;
    fixture.settle(&[("a", good(11.5))]).await;
    let recorder = fixture.record("a");
    collection(&fixture.app, "stop").await;
    fixture.settle(&[("a", BAD)]).await;
    fixture
        .exec(&format!("UPDATE \"{}\" SET a = 12.5", fixture.table))
        .await;
    let restart_ms = now_ms();
    collection(&fixture.app, "start").await;
    fixture.settle(&[("a", good(12.5))]).await;
    let sequences = recorder.finish().await;
    let expected = vec![good(11.5), BAD, good(12.5)];
    assert_eq!(sequences[0], expected, "Hub DB Source");
    assert_eq!(sequences[1], expected, "bridge");

    // tstore（ブリッジのタグ）: 開始の後の最初の行は値なし、停止前の値の行は無い。
    collection(&fixture.app, "stop").await;
    let rows = fixture.recorded("a").await;
    let before: Vec<_> = rows.iter().filter(|(p, _)| *p < restart_ms).collect();
    let after: Vec<_> = rows.iter().filter(|(p, _)| *p >= restart_ms).collect();
    assert!(
        before.iter().any(|(_, v)| *v == Some(11.5)),
        "the value was recorded before the stop: {before:?}"
    );
    assert_eq!(after.first().map(|(_, v)| *v), Some(None), "{after:?}");
    assert!(
        after.iter().all(|(_, v)| *v != Some(11.5)),
        "no row of the pre-stop value after the restart: {after:?}"
    );
    assert!(after.iter().any(|(_, v)| *v == Some(12.5)), "{after:?}");
    fixture.finish().await;
}

/// 7b（ブリッジだけ）: 収集を止めている間に DB を止め、そのまま開始する。ブリッジは
/// `ping` を送り続ける（Hub から見て Live）が、タグは停止前の値で Good にならず Bad の
/// まま。tstore に開始の後の停止前の値の行が無い。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parity_7b_start_with_the_db_down_stays_bad() {
    let target = require_pg!();
    let fixture = Fixture::start(
        &target,
        "p7b",
        "a float8",
        Some("(21.5)"),
        "a",
        &[col("a", "f32")],
        STALE_TIMEOUT_MS,
    )
    .await;
    fixture.settle(&[("a", good(21.5))]).await;
    collection(&fixture.app, "stop").await;
    fixture.settle(&[("a", BAD)]).await;
    fixture.role.set_login(&fixture.admin, false).await;
    fixture.role.terminate_sessions(&fixture.admin).await;

    let restart_ms = now_ms();
    collection(&fixture.app, "start").await;
    // `stale_timeout_ms`（3 秒）を超えて見続ける: ブリッジの `ping`（1 秒ごと）で Live の
    // まま、値は Bad のまま。
    fixture
        .hold(
            Duration::from_millis(STALE_TIMEOUT_MS + 1_500),
            "the bridge tag must stay Bad while the DB is down",
            |f| f.pair("a").1 == BAD,
        )
        .await;
    let status = fixture.ingest_status();
    assert!(status.live, "the bridge keeps pinging: {status:?}");
    assert!(
        status
            .last_seen_ms
            .is_some_and(|seen| seen >= restart_ms + 1_000),
        "the Hub kept hearing from the bridge after the start: {status:?}"
    );
    assert_eq!(fixture.pair("a").0, BAD, "the Hub DB Source is down too");

    collection(&fixture.app, "stop").await;
    let rows = fixture.recorded("a").await;
    let after: Vec<_> = rows.iter().filter(|(p, _)| *p >= restart_ms).collect();
    assert!(after.len() >= 10, "{after:?}");
    assert!(
        after.iter().all(|(_, v)| v.is_none()),
        "no row of the pre-stop value after the restart: {after:?}"
    );
    fixture.role.set_login(&fixture.admin, true).await;
    fixture.finish().await;
}

/// 7c（ブリッジだけ）: `columns` から 1 つ外す（タグは残す）。外したキーは設定の commit で
/// Bad、残りは次の周期で Good。外したキーの値を送っても `not_in_bridge_set` で断られ、
/// Bad のまま。Hub 内の Source（P は変えていない）は両方の列を読み続ける。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parity_7c_a_key_removed_from_columns_goes_bad() {
    let target = require_pg!();
    let fixture = Fixture::start(
        &target,
        "p7c",
        "a float8, b float8",
        Some("(31.5, 32.5)"),
        "a, b",
        &[col("a", "f32"), col("b", "f32")],
        STALE_TIMEOUT_MS,
    )
    .await;
    fixture
        .settle(&[("a", good(31.5)), ("b", good(32.5))])
        .await;
    let epoch_before = fixture.ingest_status().epoch;

    // ロックダウン済みの Hub は収集中の変更を pending queue に積む（202）。明示の適用で
    // 無停止のまま commit される。
    let settings = bridge_settings(
        fixture.pg_id,
        fixture.stale_timeout_ms,
        &[Query {
            name: "q",
            period_ms: QUERY_PERIOD_MS,
            sql: &fixture.sql,
            columns: vec![("a".to_string(), "a".to_string())],
        }],
    );
    let (status, body) = request_json(
        &fixture.app.router,
        "PUT",
        &format!("/api/plc-connections/{}", fixture.push_id),
        &fixture.app.token,
        Some(
            json!({ "name": fixture.push_name, "protocol": "push", "host": "", "port": 0,
            "enabled": true, "settings": settings }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body:?}");
    let pending_id = body["pending"]["id"].as_i64().expect("pending id");
    let (status, body) = request_json(
        &fixture.app.router,
        "POST",
        &format!("/api/pending-changes/{pending_id}/apply"),
        &fixture.app.token,
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");

    // 外したキーは Bad、残りは（ブリッジが設定を取り直した後の）次の周期で Good。
    let settled = wait_until(WAIT, || async {
        fixture.pair("b").1 == BAD && fixture.pair("a").1 == good(31.5)
    })
    .await;
    assert!(
        settled,
        "{:?} / {:?}",
        fixture.pairs(),
        fixture.bridge.as_ref().map(|b| b.statuses())
    );
    let epoch = fixture.ingest_status().epoch;
    assert!(epoch > epoch_before, "the commit advances the epoch");
    // Hub 内の Source は変えていないので両方の列を読み続ける。
    assert_eq!(fixture.pair("b").0, good(32.5));

    // 外したキーの値を今の epoch で送っても、件ごとに `not_in_bridge_set`。
    let response = reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{}/api/v1/ingest/{}/values",
            fixture.app.port, fixture.push_id
        ))
        .header("Authorization", format!("Bearer {}", fixture.api_key))
        .header("content-type", "application/json")
        .body(json!({ "epoch": epoch, "items": [{ "key": "b", "v": 99.5 }] }).to_string())
        .send()
        .await
        .expect("ingest REST");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
    assert_eq!(body["accepted"], json!(0), "{body:?}");
    assert_eq!(body["rejected"][0]["key"], json!("b"), "{body:?}");
    assert_eq!(
        body["rejected"][0]["code"],
        json!("not_in_bridge_set"),
        "{body:?}"
    );
    fixture
        .hold(
            Duration::from_millis(1_500),
            "the removed key stays Bad",
            |f| f.pair("b").1 == BAD && f.pair("a").1 == good(31.5),
        )
        .await;
    fixture.finish().await;
}

/// 8（ブリッジだけ）: ブリッジを止めると、`stale_timeout_ms` 以内は直前の状態（Good）の
/// まま、その後 Bad（Stale を挟まない）。Hub 内の Source は Good のまま。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parity_8_a_stopped_bridge_goes_bad_after_the_timeout() {
    let target = require_pg!();
    let stale_timeout_ms: u64 = 5_000;
    let mut fixture = Fixture::start(
        &target,
        "p8",
        "a float8",
        Some("(41.5)"),
        "a",
        &[col("a", "f32")],
        stale_timeout_ms,
    )
    .await;
    fixture.settle(&[("a", good(41.5))]).await;
    let recorder = fixture.record("a");
    let stopped_at = tokio::time::Instant::now();
    let bridge = fixture.bridge.take().unwrap();
    assert!(bridge.shutdown(Duration::from_secs(10)).await);
    let went_bad = wait_until(Duration::from_millis(stale_timeout_ms) + WAIT, || async {
        fixture.pair("a").1 == BAD
    })
    .await;
    let elapsed = stopped_at.elapsed();
    assert!(went_bad, "{:?}", fixture.pairs());
    // 最後の通信（周期 1 秒の値）は止める直前のどこか。止めてから `stale_timeout_ms` から
    // 1 周期と余裕を引いた時間より前には Bad にならない。
    assert!(
        elapsed >= Duration::from_millis(stale_timeout_ms - 2_000),
        "went Bad {elapsed:?} after the bridge stopped (stale_timeout {stale_timeout_ms} ms)"
    );
    let status = fixture.ingest_status();
    assert!(!status.live, "{status:?}");
    assert!(status.link_detail.is_some(), "{status:?}");
    let sequences = recorder.finish().await;
    assert_eq!(sequences[0], vec![good(41.5)], "Hub DB Source");
    assert_eq!(sequences[1], vec![good(41.5), BAD], "bridge");
    fixture.finish().await;
}

// --- 9（R2a-3b で tests/bridge.rs に置いた 60 秒のテストを移したもの） ------------

/// 1 グループに置くタグの上限（収集の記録の表は 1 グループ = 1 表で、タグごとに列を持つ。
/// SQLite の列の上限 2,000 に当たらないよう分ける）。
const TAGS_PER_GROUP: usize = 500;

/// `push` 接続の下にグループ `g0`, `g1`, … を作り（[`TAGS_PER_GROUP`] 件ずつ）、キーごとに
/// `plc` タグを置く（i 番目のキーのタグの外部名は [`push_external`]）。
async fn push_tags(app: &TestApp, connection_id: i64, keys: &[(String, &str)]) {
    let tags = TagService::new(app.pool.clone());
    for (chunk, slice) in keys.chunks(TAGS_PER_GROUP).enumerate() {
        let group = create_group(app, connection_id, &format!("g{chunk}"), 500, None).await;
        for (offset, (key, data_type)) in slice.iter().enumerate() {
            let i = chunk * TAGS_PER_GROUP + offset;
            tags.create(tag_input(
                &format!("t{i}"),
                group,
                key,
                data_type,
                PLC_TAG_KIND,
            ))
            .await
            .expect("push tag");
        }
    }
}

/// [`push_tags`] の i 番目のキーのタグの外部名。
fn push_external(source_name: &str, i: usize) -> String {
    format!("{source_name}.g{}.t{i}", i / TAGS_PER_GROUP)
}

/// 9（ブリッジだけ、§11 の R2a-3b「複数クエリの合計」）: 見積もりの上限ちょうどの設定
/// （周期 500 ms のクエリ 3 本、A は 1,800 件で 2 メッセージ、B・C は各 100 件。合計 8
/// メッセージ / 秒・4,000 件 / 秒）で、60 秒の間 `rate_limited` / `too_many_items` が 0 件、
/// 全キーが Good で、Hub 内の Source（同じ SQL の `db` タグ）と同じ値（全キーをビットで）。
///
/// A の 1,800 キーは 18 列に写す（同じ列を複数のキーへ。PostgreSQL の射影は 1,664 列まで
/// なので、ブリッジはラッパー SQL に列を 1 回だけ射影する - §10.3 の実装の注 (4)）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parity_9_three_queries_at_the_registration_limit() {
    let target = require_pg!();
    let admin = PgPool::connect(&target.url).await.expect("connect test pg");
    let role = TestRole::create(&admin, "p9").await;
    let app = test_app().await;
    let connections = PlcConnectionService::new(app.pool.clone());
    let pg = connections
        .create(postgres_input("pg-limit", &target, &role))
        .await
        .expect("postgres connection");
    let source_name = format!("Limit-{}", std::process::id());
    let sql_a = format!(
        "SELECT {}",
        (0..18)
            .map(|c| format!("{}.25::float8 AS a{c}", c + 1))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let sql_b = "SELECT 2.5::float8 AS b";
    let sql_c = "SELECT true AS c";
    let keys_a: Vec<(String, String)> = (0..1_800)
        .map(|i| (format!("a{}", i % 18), format!("A.{i:04}")))
        .collect();
    let keys_b: Vec<(String, String)> = (0..100)
        .map(|i| ("b".to_string(), format!("B.{i:03}")))
        .collect();
    let keys_c: Vec<(String, String)> = (0..100)
        .map(|i| ("c".to_string(), format!("C.{i:03}")))
        .collect();
    let settings = bridge_settings(
        pg.id,
        10_000,
        &[
            Query {
                name: "A",
                period_ms: 500,
                sql: &sql_a,
                columns: keys_a.clone(),
            },
            Query {
                name: "B",
                period_ms: 500,
                sql: sql_b,
                columns: keys_b.clone(),
            },
            Query {
                name: "C",
                period_ms: 500,
                sql: sql_c,
                columns: keys_c.clone(),
            },
        ],
    );
    // 登録時の見積もりがちょうど上限（8 割）であること（拒否の例は単体テスト）。
    let load = banto_tags::bridge_load(
        &banto_tags::validate_push_settings(&settings)
            .expect("the §10.4 #9 configuration is accepted")
            .bridge
            .unwrap(),
    );
    assert_eq!(load.items_per_minute, 4_000 * 60);
    assert_eq!(load.messages_per_minute, 8 * 60);
    let push = connections
        .create(push_input(&source_name, settings))
        .await
        .expect("push connection");
    let all_keys: Vec<(String, String)> = keys_a
        .iter()
        .chain(&keys_b)
        .chain(&keys_c)
        .cloned()
        .collect();
    let typed: Vec<(String, &str)> = all_keys
        .iter()
        .map(|(_, key)| (key.clone(), "f64"))
        .collect();
    push_tags(&app, push.id, &typed).await;
    // Hub 内の Source（同じ SQL の `db` タグ、列ごとに 1 つ）- 値の一致の比較用。
    let tags = TagService::new(app.pool.clone());
    for (group, sql, columns) in [
        (
            "qa",
            sql_a.as_str(),
            (0..18).map(|c| format!("a{c}")).collect::<Vec<_>>(),
        ),
        ("qb", sql_b, vec!["b".to_string()]),
        ("qc", sql_c, vec!["c".to_string()]),
    ] {
        let group_id = create_group(&app, pg.id, group, 500, Some(sql)).await;
        for column in columns {
            tags.create(tag_input(&column, group_id, &column, "f32", DB_TAG_KIND))
                .await
                .expect("db tag");
        }
    }
    app.manager.rebuild().await.expect("rebuild");
    let api_key = issue_key(
        &app,
        "bridge-limit",
        &[
            "admin".to_string(),
            "read".to_string(),
            format!("ingest:#{}", push.id),
        ],
    )
    .await;
    collection(&app, "start").await;
    let bridge = spawn_bridge(&app, &api_key);

    // キー i の (Hub 内の Source の外部名, ブリッジの外部名)。
    let db_external = |column: &str| {
        let group = match column {
            "b" => "qb",
            "c" => "qc",
            _ => "qa",
        };
        format!("pg-limit.{group}.{column}")
    };
    let pairs: Vec<(String, String)> = all_keys
        .iter()
        .enumerate()
        .map(|(i, (column, _))| (db_external(column), push_external(&source_name, i)))
        .collect();
    // 全キーが Good で Hub 内の Source と同じ値（ビット）か。外れたものの最初の 1 つ。
    let first_mismatch = |manager: &CollectorManager| {
        pairs.iter().find_map(|(db, push)| {
            let (db_reading, push_reading) = (read_tag(manager, db), read_tag(manager, push));
            (db_reading.quality != "good" || push_reading != db_reading)
                .then(|| format!("{push}: {push_reading:?} vs {db}: {db_reading:?}"))
        })
    };

    // 立ち上がり: 全キーが Good・同じ値になるまで待つ。
    let ready = wait_until(Duration::from_secs(30), || async {
        first_mismatch(&app.manager).is_none()
    })
    .await;
    assert!(
        ready,
        "{:?} / {:?}",
        first_mismatch(&app.manager),
        bridge.statuses()
    );
    let hub = app.manager.ingest_hub();
    let hub_status = |hub: &banto_hub_core::ingest::IngestHub| {
        hub.status()
            .into_iter()
            .find(|s| s.connection_id == push.id)
            .unwrap()
    };
    let accepted_before = hub_status(&hub).accepted;

    // 60 秒（§10.4 の 9）。途中も全キーが Good・同じ値のまま。
    let started = tokio::time::Instant::now();
    while started.elapsed() < Duration::from_secs(60) {
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert_eq!(first_mismatch(&app.manager), None);
        let status = hub_status(&hub);
        assert_eq!(status.rate_limited, 0, "{status:?}");
        assert_eq!(status.too_many_items, 0, "{status:?}");
    }
    let status = hub_status(&hub);
    assert_eq!(status.rate_limited, 0, "{status:?}");
    assert_eq!(status.too_many_items, 0, "{status:?}");
    assert_eq!(status.rejected, 0, "{status:?}");
    // 60 秒で 4,000 件 / 秒の 9 割以上が受け付けられた（毎周期送れている）。
    let accepted = status.accepted - accepted_before;
    assert!(accepted >= 4_000 * 60 * 9 / 10, "accepted {accepted}");
    let bridge_side = bridge_status(&bridge, push.id).unwrap();
    assert_eq!(bridge_side.rate_limited, 0, "{bridge_side:?}");
    assert_eq!(bridge_side.too_many_items, 0, "{bridge_side:?}");
    assert_eq!(bridge_side.superseded_cycles, 0, "{bridge_side:?}");
    // 1 周期は 4 メッセージ（A が 2 つに分かれる）。
    assert!(
        bridge_side.sent_messages >= bridge_side.sent_items / 1_000,
        "{bridge_side:?}"
    );
    // 値そのもの: A の列 a5 = 6.25、B = 2.5、C（bool）= 1.0。
    let reading = |i: usize| read_tag(&app.manager, &push_external(&source_name, i));
    assert_eq!(reading(5), good(6.25));
    assert_eq!(reading(1_800), good(2.5));
    assert_eq!(reading(1_900), good(1.0));

    assert!(bridge.shutdown(Duration::from_secs(10)).await);
    app.shutdown().await;
    role.drop_role(&admin).await;
    admin.close().await;
}
