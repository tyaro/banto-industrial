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
//! 正の条件（何かが起こるべき）は [`wait_until`] のポーリング + 明示的な
//! タイムアウトで、条件が満たされた瞬間に進む（固定 sleep では待たない）。
//! 負の条件（何も起こらないべき、例えば「値が変わらないなら行が増えない」）
//! は、変化を検出できないため意図的に短い bounded sleep を使う
//! （テスト名：`on_change_mode_writes_once_for_a_constant_value` の
//! 2 秒 sleep、`starting_without_a_hub_waits_instead_of_exiting` の
//! 3 秒 sleep）。テーブル検査の再検査間隔は
//! `banto_hub_sink::SidecarOptions`（設定ファイルには出さない注入口）で
//! 1 秒に縮めてある - 本番の既定は設計どおり 30 秒。

use banto_hub_core::rest::user_session_lookup;
use banto_server::SessionValidation;
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
use banto_hub_core::hub::CollectorManager;
use banto_hub_core::rest::api_router;
use banto_hub_core::settings::SettingsService;
use banto_hub_core::users::UsersService;
use banto_hub_core::write_audit::WriteAuditService;
use banto_hub_core::write_control::WriteControl;
use banto_hub_core::write_rate::{WriteRateLimitConfig, WriteRateLimiter};
use banto_hub_sink::{run, SidecarConfig, SidecarOptions, StopRequest};
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

#[path = "common/tls_fixture.rs"]
mod tls_fixture;
use tls_fixture::TlsFixture;

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

/// R1-3b: the built-in drivers as the `HubDrivers` `HubSessions::new` takes
/// (docs/banto-driver-api-design.md §6.4).
fn banto_drivers_helper() -> banto_hub_core::broker_glue::HubDrivers {
    let builtin = banto_drivers::builtin();
    // R2b-1d (docs/banto-hub-connection-settings-design.md §10): the Hub's entrances,
    // plan and preflight read the registry now, so the host-provided protocols
    // (`push` / `postgres` / `virtual`) join the built-ins exactly as the real hosts'
    // `hub_drivers()` do - without them every postgres / push connection here would be
    // refused or excluded as `driverUnavailable`.
    let mut registry = builtin.registry;
    banto_hub_core::register_host_provided(&mut registry)
        .expect("host-provided driver ids are distinct from the built-ins");
    banto_hub_core::broker_glue::HubDrivers::new(Arc::new(registry))
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
    tls_server: Option<banto_hub_core::runtime::RunningTlsServer>,
    tls_fixture: TlsFixture,
    tls_port: u16,
    router: Router,
    token: String,
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
        if let Some(server) = self.tls_server.take() {
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
    let sim_registry = Arc::new(BrokerSimRegistry::new());
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
    manager
        .commit_catalog(&banto_collect::RegistrySnapshot::load(&pool).await.unwrap())
        .await
        .expect("initial catalog commit");

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
    let tls_fixture = TlsFixture::generate(temp.path());
    let tls_server =
        banto_hub_core::runtime::start_tls_router(router.clone(), tls_fixture.hub_config())
            .await
            .expect("TLS test server should start");
    let tls_port = tls_server.local_addr().port();

    TestApp {
        server: Some(server),
        tls_server: Some(tls_server),
        tls_fixture,
        tls_port,
        router,
        token,
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
/// `postgres` 接続を 1 件作る。`host`/`port` を分けて受けるのは、Sink の
/// 書き先だけをテスト内の TCP プロキシ（[`TcpProxy`]）経由にして DB の
/// 停止を模すため（認証情報は `target` のもの）。
async fn create_postgres_connection(
    app: &TestApp,
    name: &str,
    host: &str,
    port: i64,
    target: &PgTarget,
) -> i64 {
    let connection = PlcConnectionService::new(app.pool.clone())
        .create(PlcConnectionInput {
            name: name.to_string(),
            protocol: POSTGRES_PROTOCOL.to_string(),
            host: host.to_string(),
            port,
            unit_id: 1,
            enabled: true,
            simulation: false,
            word_order: "low_high".to_string(),
            cpu_type: None,
            database: Some(target.database.clone()),
            username: Some(target.username.clone()),
            // Sealed at the entrance the way banto-hub's REST/MCP seal
            // (`platform_default`: DPAPI on Windows, `plain-v1` elsewhere).
            secrets: banto_hub_core::secrets::password_changes(
                &banto_hub_core::secrets::ConnectionSecrets::platform_default(),
                target.password.as_deref(),
            )
            .expect("seal the test password"),
            settings: None,
        })
        .await
        .expect("postgres connection");
    connection.id
}

async fn seed_source(app: &TestApp, target: &PgTarget) -> (i64, i64, String) {
    let (connection_id, tags) = seed_source_with(app, target, "SELECT 42.5 AS a", &["a"]).await;
    let (tag_id, external_name) = tags.into_iter().next().expect("one tag");
    (connection_id, tag_id, external_name)
}

/// DB Source（接続 `erp`・query group `q1`）と、`columns` の列ごとの DB タグを
/// 作る。戻りは接続 id と `(tag_id, external_name)` の並び。
async fn seed_source_with(
    app: &TestApp,
    target: &PgTarget,
    query_sql: &str,
    columns: &[&str],
) -> (i64, Vec<(i64, String)>) {
    let connection_id =
        create_postgres_connection(app, "erp", &target.host, target.port, target).await;
    let group = CollectionGroupService::new(app.pool.clone())
        .create(CollectionGroupInput {
            name: "q1".to_string(),
            plc_connection_id: connection_id,
            period_ms: SOURCE_PERIOD_MS,
            enabled: true,
            default_writable: true,
            query_sql: Some(query_sql.to_string()),
        })
        .await
        .expect("query group");
    let mut tags = Vec::new();
    for column in columns {
        let tag = TagService::new(app.pool.clone())
            .create(TagInput {
                comment: None,
                name: (*column).to_string(),
                collection_group_id: group.id,
                address: (*column).to_string(),
                data_type: "f32".to_string(),
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
                tag_kind: DB_TAG_KIND.to_string(),
                expression: None,
                retain: false,
                expected_revision: None,
            })
            .await
            .expect("db tag");
        tags.push((tag.id, format!("erp.q1.{column}")));
    }
    app.manager
        .commit_catalog(
            &banto_collect::RegistrySnapshot::load(&app.pool)
                .await
                .unwrap(),
        )
        .await
        .expect("catalog commit after seeding");
    (connection_id, tags)
}

/// 127.0.0.1 の空きポートで待ち受け、PostgreSQL へ中継する TCP プロキシ。
/// `stop` で待受と中継中の接続をすべて切り（Sink から見ると DB 停止）、
/// `resume` で同じポートに戻す。
struct TcpProxy {
    port: u16,
    upstream: String,
    listener: Option<tokio::task::JoinHandle<()>>,
    relays: Arc<std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>>,
}

impl TcpProxy {
    async fn start(upstream: String) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind proxy");
        let port = listener.local_addr().expect("proxy addr").port();
        let mut proxy = Self {
            port,
            upstream,
            listener: None,
            relays: Arc::new(std::sync::Mutex::new(Vec::new())),
        };
        proxy.serve(listener);
        proxy
    }

    fn serve(&mut self, listener: tokio::net::TcpListener) {
        let upstream = self.upstream.clone();
        let relays = self.relays.clone();
        self.listener = Some(tokio::spawn(async move {
            loop {
                let Ok((mut client, _)) = listener.accept().await else {
                    return;
                };
                let upstream = upstream.clone();
                let relay = tokio::spawn(async move {
                    let Ok(mut server) = tokio::net::TcpStream::connect(&upstream).await else {
                        return;
                    };
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
                });
                relays.lock().expect("relays").push(relay);
            }
        }));
    }

    /// 待受を閉じ、中継中の接続を全部切る（ソケットは abort で drop される）。
    fn stop(&mut self) {
        if let Some(listener) = self.listener.take() {
            listener.abort();
        }
        for relay in self.relays.lock().expect("relays").drain(..) {
            relay.abort();
        }
    }

    async fn resume(&mut self) {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", self.port))
            .await
            .expect("rebind proxy on the same port");
        self.serve(listener);
    }
}

/// `GET /api/status` の `sink.groups` の 1 件（`queued`・`dropped` を読むため）。
fn sink_group_entry(sink: &Value, group_id: i64) -> Option<Value> {
    sink["groups"]
        .as_array()?
        .iter()
        .find(|entry| entry["id"].as_i64() == Some(group_id))
        .cloned()
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

fn sidecar_toml(tls: &TlsFixture, port: u16, api_key: &str) -> String {
    let base = tls.pinned_config(port, api_key);
    format!(
        "{base}\
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
    std::fs::write(
        &config_path,
        sidecar_toml(&app.tls_fixture, app.tls_port, &api_key),
    )
    .expect("write toml");
    let config = banto_hub_sink::config::load_config_from(&config_path).expect("load config");

    let shutdown = Arc::new(Notify::new());
    let shutdown_signal = shutdown.clone();
    let sidecar = tokio::spawn(async move {
        run(config, fast_sidecar_options(), async move {
            shutdown_signal.notified().await;
            StopRequest::Graceful
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
    std::fs::write(
        &config_path,
        sidecar_toml(&app.tls_fixture, app.tls_port, &api_key),
    )
    .expect("write toml");
    let config: SidecarConfig =
        banto_hub_sink::config::load_config_from(&config_path).expect("load config");

    let shutdown = Arc::new(Notify::new());
    let shutdown_signal = shutdown.clone();
    let sidecar = tokio::spawn(async move {
        run(config, fast_sidecar_options(), async move {
            shutdown_signal.notified().await;
            StopRequest::Graceful
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

/// 2026-11-02 オーナー決定（設計 §5.6「設定変更とキュー」）: DB が落ちている間に
/// sink group へタグを足しても、溜まっていた未送信の行は失われず、DB が戻ると
/// 書かれる。S7 の S-4 で「設定が変わったため、未送信の 5 行を破棄しました」が
/// 出た件の再現と修正の確かめ。
///
/// Sink の書き先だけをテスト内の TCP プロキシ経由にし、プロキシを止めて
/// DB 停止を模す（DB Source は直接繋ぐので値は流れ続け、行はキューに溜まる）。
///
/// 1. 行が書かれる → プロキシを止める → グループが `backoff`、`queued` が増える
/// 2. タグを足す（接続・テーブルは同じ）→ 設定が反映されても `dropped` は 0 の
///    まま `queued` が減らない（旧い作りなら `dropped` が `queued` ぶん増える）
/// 3. プロキシを戻す → 止めた時点で溜まっていた行数以上が書かれ、足したタグの
///    行も現れ（新しい設定で動いている）、`dropped` は 0 のまま
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn adding_a_tag_while_the_db_is_down_keeps_the_queued_rows() {
    let target = require_pg!();
    let db = PgPool::connect(&target.url).await.expect("connect test pg");
    let table = unique_table("keepq");
    create_sink_table(&db, &table).await;
    let mut proxy = TcpProxy::start(format!("{}:{}", target.host, target.port)).await;

    let app = test_app().await;
    let (_, tags) =
        seed_source_with(&app, &target, "SELECT 42.5 AS a, 7.0 AS b", &["a", "b"]).await;
    let (tag_a, _) = tags[0].clone();
    let (tag_b, _) = tags[1].clone();
    let sink_connection_id =
        create_postgres_connection(&app, "sink-db", "127.0.0.1", i64::from(proxy.port), &target)
            .await;
    let group_id = create_sink_group(&app, sink_connection_id, tag_a, &table).await;
    let api_key = issue_sidecar_key(&app).await;
    start_collection(&app).await;

    let config_dir = tempfile::Builder::new()
        .prefix("banto-hub-sink-cfg-")
        .tempdir()
        .expect("config dir");
    let config_path = config_dir.path().join("banto-hub-sink.toml");
    std::fs::write(
        &config_path,
        sidecar_toml(&app.tls_fixture, app.tls_port, &api_key),
    )
    .expect("write toml");
    let config = banto_hub_sink::config::load_config_from(&config_path).expect("load config");
    let config_refresh = config.config_refresh;

    let shutdown = Arc::new(Notify::new());
    let shutdown_signal = shutdown.clone();
    let sidecar = tokio::spawn(async move {
        run(config, fast_sidecar_options(), async move {
            shutdown_signal.notified().await;
            StopRequest::Graceful
        })
        .await
    });

    // 1. プロキシ経由で行が書かれる。
    let wrote = wait_until(WAIT, || async {
        !stored_rows(&db, &table).await.is_empty()
    })
    .await;
    assert!(wrote, "プロキシ経由でサイドカーが行を書き込めていない");

    // DB 停止: プロキシを止める → flusher の INSERT が接続レベルで失敗して
    // `backoff`、行はキューに溜まる。
    proxy.stop();
    let backing_off = wait_until(WAIT, || async {
        let sink = sink_status(&app).await;
        sink_group_state(&sink, group_id).as_deref() == Some("backoff")
            && sink_group_entry(&sink, group_id)
                .and_then(|entry| entry["queued"].as_u64())
                .is_some_and(|queued| queued >= 3)
    })
    .await;
    assert!(
        backing_off,
        "DB を止めても backoff にならない、またはキューが溜まらない: {:?}",
        sink_status(&app).await
    );
    let stored_before = stored_rows(&db, &table).await.len();
    let entry_before = sink_group_entry(&sink_status(&app).await, group_id).expect("group entry");
    let queued_before = entry_before["queued"].as_u64().expect("queued");
    assert_eq!(
        entry_before["dropped"].as_u64(),
        Some(0),
        "{entry_before:?}"
    );

    // 2. タグを足す（接続・テーブルは同じ）。
    let (status, body) = write_json(
        &app.router,
        "PUT",
        &format!("/api/sink/groups/{group_id}"),
        &app.token,
        json!({
            "name": "line1-log",
            "dbConnectionId": sink_connection_id,
            "mode": "interval",
            "intervalMs": SINK_INTERVAL_MS,
            "tableName": table,
            "storeBad": false,
            "enabled": true,
            "tagIds": [tag_a, tag_b],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    // サイドカーが設定を取り直すまで（`config_refresh_secs`）待ってから見る。
    // 旧い作りならここで `dropped` が `queued_before` 以上になり、`queued` は
    // 小さく戻る。
    tokio::time::sleep(config_refresh + Duration::from_secs(2)).await;
    let entry_after = sink_group_entry(&sink_status(&app).await, group_id).expect("group entry");
    assert_eq!(
        entry_after["dropped"].as_u64(),
        Some(0),
        "タグを足しただけで未送信の行を捨てている: {entry_after:?}"
    );
    assert!(
        entry_after["queued"].as_u64().expect("queued") > queued_before,
        "キューが引き継がれていない（{queued_before} → {entry_after:?}）"
    );

    // 3. DB が戻る → 溜まっていた行が書かれ、足したタグの行も現れる。
    proxy.resume().await;
    let recovered = wait_until(Duration::from_secs(60), || async {
        let rows = stored_rows(&db, &table).await;
        rows.len() >= stored_before + queued_before as usize
            && rows.iter().any(|row| row.tag_id == tag_b)
            && sink_group_state(&sink_status(&app).await, group_id).as_deref() == Some("running")
    })
    .await;
    assert!(
        recovered,
        "DB が戻っても溜まっていた行が書かれない（止めた時点 {stored_before}+{queued_before}）: 今 {} 行, {:?}",
        stored_rows(&db, &table).await.len(),
        sink_status(&app).await
    );
    let entry_final = sink_group_entry(&sink_status(&app).await, group_id).expect("group entry");
    assert_eq!(entry_final["dropped"].as_u64(), Some(0), "{entry_final:?}");

    shutdown.notify_one();
    let joined = tokio::time::timeout(Duration::from_secs(30), sidecar).await;
    assert!(joined.is_ok(), "サイドカーが停止しない");
    joined
        .unwrap()
        .expect("sidecar task panicked")
        .expect("sidecar run() should not fail");

    proxy.stop();
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
    let tls = TlsFixture::generate(config_dir.path());
    std::fs::write(&config_path, sidecar_toml(&tls, port, "bh_absent.key")).expect("write toml");
    let config = banto_hub_sink::config::load_config_from(&config_path).expect("load config");

    let shutdown = Arc::new(Notify::new());
    let shutdown_signal = shutdown.clone();
    let sidecar = tokio::spawn(async move {
        run(config, fast_sidecar_options(), async move {
            shutdown_signal.notified().await;
            StopRequest::Graceful
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

/// R2b-1d (docs/banto-hub-connection-settings-design.md §5.4・§9 「読めない」): when the
/// Hub cannot read a `postgres` connection's password (here its sealed row is gone), the
/// sidecar must **not** connect to that DB - not with an empty password either (a `trust`
/// DB would accept it). Runs without `BANTO_TEST_PG_URL`: the "DB" is a plain TCP
/// listener that must never see a connection.
///
/// 1. `GET /api/sink/config` leaves the connection out of `connections` and lists it in
///    `unavailableConnections` with `reason: "missing"`.
/// 2. An **old-shape** client (alpha.33's `SinkConfig`, which knows no
///    `unavailableConnections`) reads the same body: the group's `dbConnectionId` is not
///    among `connections`, so its `start_group` stops at `GroupSetupError::MissingConnection`
///    and never builds a pool.
/// 3. This sidecar marks the group `error` with「Hub でパスワードを読めません」and the
///    listener never accepts a connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unreadable_password_is_listed_and_never_dialled() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind the fake DB");
    listener
        .set_nonblocking(true)
        .expect("non-blocking listener");
    let db_port = listener.local_addr().expect("addr").port();

    let app = test_app().await;
    let target = PgTarget {
        host: "127.0.0.1".to_string(),
        port: i64::from(db_port),
        database: "erp".to_string(),
        username: "app".to_string(),
        password: Some("unreadable-pw".to_string()),
        url: String::new(),
    };
    let (db_connection_id, tag_id, _) = seed_source(&app, &target).await;
    // The sealed row is gone (a DB restored without it, design §5.5): `Unreadable { Missing }`.
    sqlx::query("DELETE FROM plc_connection_secrets")
        .execute(&app.pool)
        .await
        .expect("drop the sealed row");
    let group_id = create_sink_group(&app, db_connection_id, tag_id, "public.never").await;
    let api_key = issue_sidecar_key(&app).await;

    // 1. The new fields.
    let (status, body) = get_json(&app.router, "/api/sink/config", &api_key).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["connections"], json!([]), "{body:?}");
    assert_eq!(
        body["unavailableConnections"],
        json!([{ "id": db_connection_id, "name": "erp", "reason": "missing" }]),
        "{body:?}"
    );
    assert!(!body.to_string().contains("unreadable-pw"));

    // 2. An alpha.33 client: no `unavailableConnections`, and the group's connection is
    // simply absent (never `password: null`).
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct OldSinkConfig {
        groups: Vec<OldGroup>,
        connections: Vec<OldConnection>,
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct OldGroup {
        id: i64,
        db_connection_id: i64,
    }
    #[derive(serde::Deserialize)]
    struct OldConnection {
        id: i64,
    }
    let old: OldSinkConfig = serde_json::from_value(body.clone()).expect("old shape parses");
    let old_group = old
        .groups
        .iter()
        .find(|group| group.id == group_id)
        .expect("the group is still listed");
    assert!(
        !old.connections
            .iter()
            .any(|conn| conn.id == old_group.db_connection_id),
        "an old sidecar must not find the connection (it would MissingConnection, not dial)"
    );

    // 3. This sidecar: the group is `error` with the reason, nothing dials the DB.
    let config_dir = tempfile::Builder::new()
        .prefix("banto-hub-sink-cfg-")
        .tempdir()
        .expect("config dir");
    let config_path = config_dir.path().join("banto-hub-sink.toml");
    std::fs::write(
        &config_path,
        sidecar_toml(&app.tls_fixture, app.tls_port, &api_key),
    )
    .expect("write toml");
    let config = banto_hub_sink::config::load_config_from(&config_path).expect("load config");
    let shutdown = Arc::new(Notify::new());
    let shutdown_signal = shutdown.clone();
    let sidecar = tokio::spawn(async move {
        run(config, fast_sidecar_options(), async move {
            shutdown_signal.notified().await;
            StopRequest::Graceful
        })
        .await
    });

    let reported = wait_until(WAIT, || async {
        let sink = sink_status(&app).await;
        sink["groups"].as_array().is_some_and(|groups| {
            groups.iter().any(|entry| {
                entry["id"].as_i64() == Some(group_id)
                    && entry["state"] == json!("error")
                    && entry["lastError"]
                        .as_str()
                        .is_some_and(|text| text.contains("Hub でパスワードを読めません"))
            })
        })
    })
    .await;
    assert!(
        reported,
        "the group does not report the unreadable password: {:?}",
        sink_status(&app).await
    );
    // Give a would-be dial a moment, then make sure nothing ever connected.
    tokio::time::sleep(Duration::from_millis(500)).await;
    match listener.accept() {
        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {}
        Ok((_, peer)) => panic!("something dialled the unreadable DB from {peer}"),
        Err(err) => panic!("accept failed: {err}"),
    }

    shutdown.notify_one();
    let joined = tokio::time::timeout(Duration::from_secs(30), sidecar).await;
    assert!(joined.is_ok(), "サイドカーが停止しない");
    joined
        .unwrap()
        .expect("sidecar task panicked")
        .expect("sidecar run() should not fail");
    app.shutdown().await;
}
