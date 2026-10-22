//! DB ブリッジ（R2a-3b、docs/banto-hub-ingest-design.md §10.3・§11）の統合テスト。
//!
//! ## 何を繋いでいるか
//!
//! `tests/sidecar.rs` と同じく 1 つのテストプロセスの中で、
//!
//! - **Hub**: `banto_hub_core` を dev-dependency として一時ディレクトリの SQLite で組み立て、
//!   `127.0.0.1:0` に実サーバーとして立てる（プロファイル id は `bridge-it-<pid>`）。
//! - **PostgreSQL**: `BANTO_TEST_PG_URL`。ブリッジが読む `postgres` 接続は、テストごとに作る
//!   使い捨てのロール（`LOGIN` / `NOLOGIN` で「DB の停止」を作る。サーバーを止めずに済み、
//!   並行する他のテストを巻き込まない）。
//! - **ブリッジ（被試験体）**: `banto_hub_sink::source::BridgeSupervisor` をプロセス内で
//!   起こす。Hub とは実 HTTP（`GET /api/ingest/bridge-config`）と実 WebSocket
//!   （`/api/v1/ingest/{id}/stream`）で、DB とは実 PostgreSQL で繋がる。
//!
//! `BANTO_TEST_PG_URL` が無ければ `skipped` を出して成功する（`tests/sidecar.rs` と同じ）。
//! CI は `.github/workflows/ci.yml` の `Rust (db-source / PostgreSQL)` ジョブの
//! `cargo test -p banto-hub-sink` で走る。
//!
//! ## 場面（§11 の R2a-3b の統合）
//!
//! - `bridge_delivers_values_and_follows_collection_ws_and_db_outages`: 値が Hub に届く・
//!   収集の停止で問い合わせを止めて DB のセッションを閉じる・開始の epoch で送り直す・
//!   WS の切断と再接続・DB の停止で `bad`・復帰で Good・設定の変更で取り直す。
//! - `a_bridge_without_the_ingest_scope_never_runs`: スコープの無い接続は動かさない
//!   （DB に繋がない・送信口に繋がない・パスワードを受け取らない）。
//! - `an_unreachable_db_sends_bad_and_reports_it`: DB に届かない（接続の失敗）で全キー
//!   `bad` を送り、`status` で知らせる。
//! - §10.4 の 9（上限ちょうどの 3 クエリで 60 秒間 `rate_limited` / `too_many_items` が
//!   0 件）は、R2a-3c で突き合わせのテスト `tests/bridge_parity.rs` の
//!   `parity_9_three_queries_at_the_registration_limit` へ移した（§10.4 の 11 場面を 1 つの
//!   ファイルに並べるため。60 秒のテストを 2 本にしない）。
//! - `a_config_change_while_disconnected_is_fetched_before_sending` ほか 3 本: §10.3 の
//!   実装の注 (3)（PR #764 のオーナー再レビュー P2）。`welcome` のたびに設定を取り直し、
//!   `welcome` の後に始まった取得が成功するまで送らない。実 Hub は購読の後の変更にしか
//!   `config_changed` を送らず「切断中の変更」を作りにくいので、設定の取得と送信口だけの
//!   偽の Hub（[`FakeBridgeHub`]）に本物の監督と実 PostgreSQL を繋ぐ。
//!
//! 正の条件は [`wait_until`]（上限つきのポーリング）で待つ。負の条件（「繋がない」）だけ
//! 短い固定の待ちを使う。

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
use banto_hub_core::ingest::SenderTransport;
use banto_hub_core::rest::{api_router, user_session_lookup};
use banto_hub_core::settings::SettingsService;
use banto_hub_core::users::UsersService;
use banto_hub_core::write_audit::WriteAuditService;
use banto_hub_core::write_control::WriteControl;
use banto_hub_core::write_rate::{WriteRateLimitConfig, WriteRateLimiter};
use banto_hub_sink::source::{BridgeStop, BridgeSupervisor, SourceState, SourceStatus};
use banto_hub_sink::SidecarConfig;
use banto_server::{start, AuthState, Identity, ServerConfig, SessionValidation};
use banto_tags::{
    CollectionGroupInput, CollectionGroupService, PlcConnectionInput, PlcConnectionService,
    TagInput, TagService, PLC_TAG_KIND, POSTGRES_PROTOCOL, PUSH_PROTOCOL,
};
use banto_tstore::SystemClock;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use sqlx::postgres::{PgConnectOptions, PgPool};
use sqlx::SqlitePool;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::WebSocketStream;
use tower::ServiceExt;

const CLIENT_HEADER: (&str, &str) = ("X-Banto-Client", "banto");

/// ポーリング待ちの上限（DB の再接続のバックオフ 1 → 2 → 4 秒をまたげる長さ）。
const WAIT: Duration = Duration::from_secs(20);

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
    format!("bridge_it_{label}_{}_{nanos}", std::process::id())
}

/// ブリッジだけが使う使い捨てのロール（「DB の停止」を `NOLOGIN` で作るため）。
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
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "CREATE ROLE \"{}\" LOGIN PASSWORD '{}'",
            role.name, role.password
        )))
        .execute(admin)
        .await
        .expect("create role");
        role
    }

    async fn set_login(&self, admin: &PgPool, login: bool) {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "ALTER ROLE \"{}\" {}",
            self.name,
            if login { "LOGIN" } else { "NOLOGIN" }
        )))
        .execute(admin)
        .await
        .expect("alter role");
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
        // 表への権限が残っていると `DROP ROLE` が依存で失敗するので、先に権限を外す。失敗を
        // 握りつぶすと開発用 DB に LOGIN ロールが残り続けるため、ここで落とす。
        for sql in [
            format!("DROP OWNED BY \"{}\"", self.name),
            format!("DROP ROLE \"{}\"", self.name),
        ] {
            sqlx::query(sqlx::AssertSqlSafe(sql.clone()))
                .execute(admin)
                .await
                .unwrap_or_else(|err| panic!("{sql}: {err}"));
        }
    }
}

/// ブリッジ（`application_name` = `banto-hub-sink-bridge {接続名}`）が張っているセッションの数。
async fn bridge_sessions(admin: &PgPool, source_name: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE application_name = $1")
        .bind(format!("banto-hub-sink-bridge {source_name}"))
        .fetch_one(admin)
        .await
        .expect("pg_stat_activity")
}

// --- Hub のテストハーネス（tests/sidecar.rs の TestApp の写し） ---------------

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
    // R2b-1d (docs/banto-hub-connection-settings-design.md §10): the Hub's entrances,
    // plan and preflight read the registry now, so the host-provided protocols
    // (`push` / `postgres` / `virtual`) join the built-ins exactly as the real hosts'
    // `hub_drivers()` do - without them every postgres / push connection here would be
    // refused or excluded as `driverUnavailable`.
    let mut registry = builtin.registry;
    banto_hub_core::register_host_provided(&mut registry)
        .expect("host-provided driver ids are distinct from the built-ins");
    banto_hub_core::broker_glue::HubDrivers::new(Arc::new(registry), builtin.client_factory)
}

async fn test_app() -> TestApp {
    let temp = tempfile::Builder::new()
        .prefix("banto-hub-bridge-it-")
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
    let manager = Arc::new(CollectorManager::new(
        pool.clone(),
        temp.path().join("data"),
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
        format!("bridge-it-{}", std::process::id()),
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

/// 全タグの `(v, q)` を外部名で引く（`GET /api/v1/values`）。
async fn all_values(app: &TestApp) -> std::collections::HashMap<String, (Option<f64>, String)> {
    let (status, body) = request_json(&app.router, "GET", "/api/v1/values", &app.token, None).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    body["values"]
        .as_array()
        .expect("values")
        .iter()
        .map(|entry| {
            (
                entry["tag"].as_str().unwrap_or_default().to_string(),
                (
                    entry["v"].as_f64(),
                    entry["q"].as_str().unwrap_or_default().to_string(),
                ),
            )
        })
        .collect()
}

async fn value_of(app: &TestApp, external: &str) -> (Option<f64>, String) {
    all_values(app)
        .await
        .remove(external)
        .unwrap_or((None, "missing".to_string()))
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
        // Sealed at the entrance the way banto-hub's REST/MCP seal
        // (`platform_default`: DPAPI on Windows, `plain-v1` elsewhere).
        secrets: banto_hub_core::secrets::password_changes(
            &banto_hub_core::secrets::ConnectionSecrets::platform_default(),
            Some(role.password.as_str()),
        )
        .expect("seal the test password"),
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
        secrets: Default::default(),
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

/// 1 グループに置くタグの上限（収集の記録の表は 1 グループ = 1 表で、タグごとに列を持つ。
/// SQLite の列の上限 2,000 に当たらないよう分ける）。
const TAGS_PER_GROUP: usize = 500;

/// `push` 接続の下にグループ `g0`, `g1`, … を作り（[`TAGS_PER_GROUP`] 件ずつ）、キーごとに
/// `plc` タグを置く（i 番目のキーのタグの外部名は [`push_external`]）。
async fn push_tags(app: &TestApp, connection_id: i64, keys: &[(String, &str)]) {
    let groups = CollectionGroupService::new(app.pool.clone());
    let tags = TagService::new(app.pool.clone());
    for (chunk, slice) in keys.chunks(TAGS_PER_GROUP).enumerate() {
        let group = groups
            .create(CollectionGroupInput {
                name: format!("g{chunk}"),
                plc_connection_id: connection_id,
                period_ms: 500,
                enabled: true,
                default_writable: false,
                query_sql: None,
            })
            .await
            .expect("push group");
        for (offset, (key, data_type)) in slice.iter().enumerate() {
            let i = chunk * TAGS_PER_GROUP + offset;
            tags.create(tag_input(
                &format!("t{i}"),
                group.id,
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

fn bridge_toml(port: u16, api_key: &str) -> String {
    format!(
        "hub_url = \"http://127.0.0.1:{port}\"\n\
         api_key = \"{api_key}\"\n\
         config_refresh_secs = 5\n\
         status_push_secs = 1\n"
    )
}

fn spawn_bridge(app: &TestApp, api_key: &str) -> BridgeSupervisor {
    let config = SidecarConfig::from_toml_str(
        &bridge_toml(app.port, api_key),
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

/// 値が Hub に届く・収集の停止と開始・WS の切断と再接続・DB の停止と復帰・設定の変更
/// （§11 の R2a-3b の統合、§10.3）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bridge_delivers_values_and_follows_collection_ws_and_db_outages() {
    let target = require_pg!();
    let admin = PgPool::connect(&target.url).await.expect("connect test pg");
    let role = TestRole::create(&admin, "main").await;
    let app = test_app().await;
    let connections = PlcConnectionService::new(app.pool.clone());
    let pg = connections
        .create(postgres_input("pg-main", &target, &role))
        .await
        .expect("postgres connection");
    let source_name = format!("Bridge-{}", std::process::id());
    let query = |sql| Query {
        name: "line1",
        period_ms: 500,
        sql,
        columns: vec![
            ("a".to_string(), "line1.a".to_string()),
            ("b".to_string(), "line1.b".to_string()),
            ("n".to_string(), "line1.n".to_string()),
        ],
    };
    let sql = "SELECT 42.5::float8 AS a, true AS b, NULL::float8 AS n";
    let push = connections
        .create(push_input(
            &source_name,
            bridge_settings(pg.id, 3_000, &[query(sql)]),
        ))
        .await
        .expect("push connection");
    push_tags(
        &app,
        push.id,
        &[
            ("line1.a".to_string(), "f32"),
            ("line1.b".to_string(), "bit"),
            ("line1.n".to_string(), "f32"),
        ],
    )
    .await;
    app.manager.rebuild().await.expect("rebuild");
    let ext = |tag: &str| format!("{source_name}.g0.{tag}");
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

    // --- 収集の停止中: WS は繋ぐが DB には繋がない -----------------------------
    let bridge = spawn_bridge(&app, &api_key);
    let idle = wait_until(WAIT, || async {
        bridge_status(&bridge, push.id).is_some_and(|s| s.state == SourceState::Idle)
    })
    .await;
    assert!(idle, "{:?}", bridge.statuses());
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    assert_eq!(bridge_sessions(&admin, &source_name).await, 0);
    assert_eq!(value_of(&app, &ext("t0")).await.1, "bad");

    // --- 開始: 値が Hub に届く（Good・bool は 1・NULL は Bad） -------------------
    collection(&app, "start").await;
    let delivered = wait_until(WAIT, || async {
        let values = all_values(&app).await;
        values.get(&ext("t0")) == Some(&(Some(42.5), "good".into()))
            && values.get(&ext("t1")) == Some(&(Some(1.0), "good".into()))
            && values.get(&ext("t2")).map(|v| v.1.as_str()) == Some("bad")
    })
    .await;
    assert!(
        delivered,
        "{:?} / {:?}",
        all_values(&app).await,
        bridge.statuses()
    );
    let first_epoch = bridge_status(&bridge, push.id).unwrap().epoch.unwrap();
    let hub_status = app
        .manager
        .ingest_hub()
        .status()
        .into_iter()
        .find(|s| s.connection_id == push.id)
        .unwrap();
    assert_eq!(hub_status.senders.len(), 1, "{hub_status:?}");
    assert_eq!(hub_status.rate_limited, 0);

    // --- 停止: 問い合わせを止めてプールを閉じる ---------------------------------
    collection(&app, "stop").await;
    let closed = wait_until(WAIT, || async {
        bridge_sessions(&admin, &source_name).await == 0
            && bridge_status(&bridge, push.id).is_some_and(|s| s.state == SourceState::Idle)
    })
    .await;
    assert!(closed, "collecting: false must close the pool");
    assert_eq!(value_of(&app, &ext("t0")).await.1, "bad");

    // --- 再開: 新しい epoch で describe からやり直し、送り直す ---------------------
    collection(&app, "start").await;
    let resumed = wait_until(WAIT, || async {
        value_of(&app, &ext("t0")).await == (Some(42.5), "good".into())
    })
    .await;
    assert!(resumed, "{:?}", bridge.statuses());
    let status = bridge_status(&bridge, push.id).unwrap();
    assert!(status.epoch.unwrap() > first_epoch, "{status:?}");
    assert!(status.pollers_started >= 2, "{status:?}");

    // --- WS の切断と再接続: 同じキーで別の WS を開くと、ブリッジの WS は superseded で
    // 閉じられ、ブリッジは張り直す --------------------------------------------------
    let sessions_before = bridge_status(&bridge, push.id).unwrap().sessions;
    let mut request =
        tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(format!(
            "ws://127.0.0.1:{}/api/v1/ingest/{}/stream",
            app.port, push.id
        ))
        .unwrap();
    request.headers_mut().insert(
        "Authorization",
        format!("Bearer {api_key}").parse().unwrap(),
    );
    let (mut intruder, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("second ws");
    let _welcome = intruder.next().await;
    intruder.close(None).await.ok();
    drop(intruder);
    let reconnected = wait_until(WAIT, || async {
        bridge_status(&bridge, push.id).is_some_and(|s| s.sessions > sessions_before)
            && value_of(&app, &ext("t0")).await == (Some(42.5), "good".into())
    })
    .await;
    assert!(reconnected, "{:?}", bridge.statuses());
    assert!(bridge_status(&bridge, push.id)
        .unwrap()
        .last_close
        .unwrap_or_default()
        .contains("superseded"));

    // --- DB の停止: 全キー bad、状態を status で知らせる。復帰で Good -----------
    role.set_login(&admin, false).await;
    role.terminate_sessions(&admin).await;
    let bad = wait_until(WAIT, || async {
        let values = all_values(&app).await;
        values.get(&ext("t0")).map(|v| v.1.as_str()) == Some("bad")
            && values.get(&ext("t1")).map(|v| v.1.as_str()) == Some("bad")
    })
    .await;
    assert!(bad, "{:?}", all_values(&app).await);
    // 切った後の再接続はロールの拒否（サーバーの応答 = 文の失敗の分類、Hub 内の Source と
    // 同じ `classify`）になるので、文言は「クエリが失敗しました」か「DB に接続できません」。
    let reported = wait_until(WAIT, || async {
        app.manager
            .ingest_hub()
            .sender_status(push.id)
            .is_some_and(|s| s.contains("not permitted to log in"))
    })
    .await;
    let sender_status = app.manager.ingest_hub().sender_status(push.id);
    assert!(reported, "{sender_status:?}");
    // 送り手の status にパスワード・キーが出ない。
    let sender_status = sender_status.unwrap_or_default();
    assert!(!sender_status.contains(&role.password), "{sender_status}");
    assert!(!sender_status.contains(&api_key), "{sender_status}");
    role.set_login(&admin, true).await;
    let recovered = wait_until(WAIT, || async {
        value_of(&app, &ext("t0")).await == (Some(42.5), "good".into())
    })
    .await;
    assert!(recovered, "{:?}", bridge.statuses());

    // --- 設定の変更: Hub が config_changed を知らせ、ブリッジは取り直して（`config_refresh`
    // の 5 秒を待たずに）新しい SQL で送る。ロックダウン済みの Hub は収集中の変更を未適用の
    // キューに積む（202）ので、止めてから変える -------------------------------------
    collection(&app, "stop").await;
    let (status, body) = request_json(
        &app.router,
        "PUT",
        &format!("/api/plc-connections/{}", push.id),
        &app.token,
        Some(
            json!({ "name": source_name, "protocol": "push", "host": "", "port": 0,
            "enabled": true,
            "settings": bridge_settings(pg.id, 3_000,
                &[query("SELECT 43.5::float8 AS a, false AS b, 7::int4 AS n")]) }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    // 指紋が変わったので監督が source を作り直す（新しい WS = 新しい welcome）。
    let sessions_before = bridge_status(&bridge, push.id).unwrap().sessions;
    let refetched = wait_until(Duration::from_secs(4), || async {
        bridge_status(&bridge, push.id)
            .is_some_and(|s| s.sessions == 1 && s.state == SourceState::Idle)
    })
    .await;
    assert!(
        refetched,
        "config_changed must refetch before config_refresh (sessions before {sessions_before}): {:?}",
        bridge.statuses()
    );
    collection(&app, "start").await;
    let switched = wait_until(WAIT, || async {
        let values = all_values(&app).await;
        values.get(&ext("t0")) == Some(&(Some(43.5), "good".into()))
            && values.get(&ext("t1")) == Some(&(Some(0.0), "good".into()))
            && values.get(&ext("t2")) == Some(&(Some(7.0), "good".into()))
    })
    .await;
    assert!(
        switched,
        "{:?} / {:?}",
        all_values(&app).await,
        bridge.statuses()
    );

    // --- 停止: WS を閉じ、DB のセッションも残さない ----------------------------
    assert_eq!(
        bridge.shutdown(Duration::from_secs(10)).await,
        BridgeStop::Stopped
    );
    let gone = wait_until(WAIT, || async {
        bridge_sessions(&admin, &source_name).await == 0
    })
    .await;
    assert!(gone);
    let no_sender = wait_until(WAIT, || async {
        app.manager
            .ingest_hub()
            .status()
            .into_iter()
            .find(|s| s.connection_id == push.id)
            .is_some_and(|s| {
                s.senders
                    .iter()
                    .all(|sender| sender.transport != SenderTransport::Ws)
            })
    })
    .await;
    assert!(no_sender, "the bridge must close its WS on shutdown");

    app.shutdown().await;
    role.drop_role(&admin).await;
    admin.close().await;
}

/// §11 の R2a-3b の監査「スコープの無い接続を動かさない」: `ingest:#{id}` を持たない
/// キーでは、ブリッジは DB にも送信口にも繋がず、状態に「権限なし」と出す。Hub は
/// そのキーにパスワードを返さない。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bridge_without_the_ingest_scope_never_runs() {
    let target = require_pg!();
    let admin = PgPool::connect(&target.url).await.expect("connect test pg");
    let role = TestRole::create(&admin, "noscope").await;
    let app = test_app().await;
    let connections = PlcConnectionService::new(app.pool.clone());
    let pg = connections
        .create(postgres_input("pg-noscope", &target, &role))
        .await
        .expect("postgres connection");
    let source_name = format!("NoScope-{}", std::process::id());
    let push = connections
        .create(push_input(
            &source_name,
            bridge_settings(
                pg.id,
                3_000,
                &[Query {
                    name: "q",
                    period_ms: 500,
                    sql: "SELECT 1.5::float8 AS a",
                    columns: vec![("a".to_string(), "k".to_string())],
                }],
            ),
        ))
        .await
        .expect("push connection");
    push_tags(&app, push.id, &[("k".to_string(), "f32")]).await;
    app.manager.rebuild().await.expect("rebuild");
    collection(&app, "start").await;
    let api_key = issue_key(
        &app,
        "sink-only",
        &["admin".to_string(), "read".to_string()],
    )
    .await;

    // Hub の応答: ingestAllowed false・パスワードなし。
    let (status, body) = request_json(
        &app.router,
        "GET",
        "/api/ingest/bridge-config",
        &api_key,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(
        body["sources"][0]["ingestAllowed"],
        json!(false),
        "{body:?}"
    );
    assert!(body["sources"][0]["db"]["password"].is_null(), "{body:?}");
    assert!(!body.to_string().contains(&role.password));

    let bridge = spawn_bridge(&app, &api_key);
    let parked = wait_until(WAIT, || async {
        bridge_status(&bridge, push.id).is_some_and(|s| s.state == SourceState::NoPermission)
    })
    .await;
    assert!(parked, "{:?}", bridge.statuses());
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    assert_eq!(bridge_sessions(&admin, &source_name).await, 0);
    let hub_status = app
        .manager
        .ingest_hub()
        .status()
        .into_iter()
        .find(|s| s.connection_id == push.id)
        .unwrap();
    assert!(hub_status.senders.is_empty(), "{hub_status:?}");
    let status = bridge_status(&bridge, push.id).unwrap();
    assert!(status.last_status.unwrap_or_default().contains("権限なし"));
    assert_eq!(status.sent_messages, 0);

    assert_eq!(
        bridge.shutdown(Duration::from_secs(10)).await,
        BridgeStop::Stopped
    );
    app.shutdown().await;
    role.drop_role(&admin).await;
    admin.close().await;
}

/// DB に届かない（誰も listen していないポート）: 接続の失敗の分類（`classify` が
/// `Connection`）で全キー `bad` を送り、`status` で「DB に接続できません」と知らせて、
/// バックオフで張り直し続ける（§10.3「DB の接続断: 全キー bad を送ってからバックオフ」）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unreachable_db_sends_bad_and_reports_it() {
    let target = require_pg!();
    let admin = PgPool::connect(&target.url).await.expect("connect test pg");
    let role = TestRole::create(&admin, "unreachable").await;
    let app = test_app().await;
    let connections = PlcConnectionService::new(app.pool.clone());
    // 空いているポートを取ってすぐ閉じる（誰も listen していない）。
    let closed_port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.local_addr().unwrap().port()
    };
    let mut input = postgres_input("pg-unreachable", &target, &role);
    input.host = "127.0.0.1".to_string();
    input.port = i64::from(closed_port);
    let pg = connections
        .create(input)
        .await
        .expect("postgres connection");
    let source_name = format!("Unreachable-{}", std::process::id());
    let push = connections
        .create(push_input(
            &source_name,
            bridge_settings(
                pg.id,
                3_000,
                &[Query {
                    name: "q",
                    period_ms: 500,
                    sql: "SELECT 1.5::float8 AS a",
                    columns: vec![("a".to_string(), "k".to_string())],
                }],
            ),
        ))
        .await
        .expect("push connection");
    push_tags(&app, push.id, &[("k".to_string(), "f32")]).await;
    app.manager.rebuild().await.expect("rebuild");
    collection(&app, "start").await;
    let api_key = issue_key(
        &app,
        "bridge-unreachable",
        &[
            "admin".to_string(),
            "read".to_string(),
            format!("ingest:#{}", push.id),
        ],
    )
    .await;
    let bridge = spawn_bridge(&app, &api_key);
    let hub = app.manager.ingest_hub();
    let reported = wait_until(WAIT, || async {
        let status = hub
            .status()
            .into_iter()
            .find(|s| s.connection_id == push.id)
            .unwrap();
        // `bad` の件を受け付けた（送った）上で、状態に接続の失敗が出ている。
        status.accepted >= 1
            && status
                .sender_status
                .as_deref()
                .is_some_and(|s| s.contains("DB に接続できません"))
    })
    .await;
    assert!(reported, "{:?} / {:?}", hub.status(), bridge.statuses());
    assert_eq!(
        value_of(&app, &push_external(&source_name, 0)).await,
        (None, "bad".into())
    );
    let status = hub.sender_status(push.id).unwrap_or_default();
    assert!(!status.contains(&role.password), "{status}");

    assert_eq!(
        bridge.shutdown(Duration::from_secs(10)).await,
        BridgeStop::Stopped
    );
    app.shutdown().await;
    role.drop_role(&admin).await;
    admin.close().await;
}

// --- 偽の Hub（設定の取得と送信口だけ）: §10.3 の実装の注 (3) ---------------
//
// 実 Hub は購読した後の変更にしか `config_changed` を送らないので、「切断中・最初の取得と
// 最初の `welcome` の間の設定の変更」は実 Hub では作りにくい。設定の取得
// （`GET /api/ingest/bridge-config`）と送信口の WS を 1 つのポートで受ける偽の Hub を立て、
// 本物の監督（`BridgeSupervisor`）と実 PostgreSQL で、`welcome` の後に始まった取得の前に
// 値を送らないこと・取得の後は新しい SQL の値を送ることを見る。

/// 偽の Hub の操作口。
struct FakeBridgeHub {
    /// `GET /api/ingest/bridge-config` が返す本文。
    config: Arc<std::sync::Mutex<Value>>,
    /// `true` の間、設定の取得に 500 を返す。
    fail: Arc<AtomicBool>,
    /// `true` の間、設定の取得の応答を止めておく（取得が遅れている）。
    hold: Arc<AtomicBool>,
    /// 応答した設定の取得の数（成功・失敗とも）。
    fetches: Arc<AtomicU64>,
    sockets: tokio::sync::mpsc::UnboundedReceiver<WebSocketStream<TcpStream>>,
    port: u16,
    task: tokio::task::JoinHandle<()>,
}

impl FakeBridgeHub {
    async fn start(config: Value) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let config = Arc::new(std::sync::Mutex::new(config));
        let fail = Arc::new(AtomicBool::new(false));
        let hold = Arc::new(AtomicBool::new(false));
        let fetches = Arc::new(AtomicU64::new(0));
        let (socket_tx, sockets) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn({
            let (config, fail, hold, fetches) =
                (config.clone(), fail.clone(), hold.clone(), fetches.clone());
            async move {
                while let Ok((stream, _)) = listener.accept().await {
                    let (config, fail, hold, fetches, socket_tx) = (
                        config.clone(),
                        fail.clone(),
                        hold.clone(),
                        fetches.clone(),
                        socket_tx.clone(),
                    );
                    tokio::spawn(async move {
                        if is_config_request(&stream).await {
                            serve_config(stream, &config, &fail, &hold, &fetches).await;
                        } else if let Ok(ws) = tokio_tungstenite::accept_async(stream).await {
                            let _ = socket_tx.send(ws);
                        }
                    });
                }
            }
        });
        Self {
            config,
            fail,
            hold,
            fetches,
            sockets,
            port,
            task,
        }
    }

    fn set_config(&self, config: Value) {
        *self.config.lock().unwrap() = config;
    }

    fn fetches(&self) -> u64 {
        self.fetches.load(Ordering::SeqCst)
    }

    /// ブリッジが張った次の WS。
    async fn next_socket(&mut self) -> WebSocketStream<TcpStream> {
        tokio::time::timeout(WAIT, self.sockets.recv())
            .await
            .expect("the bridge connects to the ingest stream")
            .expect("fake hub running")
    }

    fn spawn_bridge(&self) -> BridgeSupervisor {
        let config = SidecarConfig::from_toml_str(
            &format!(
                "hub_url = \"http://127.0.0.1:{}\"\n\
                 api_key = \"bh_fake.bridge\"\n\
                 config_refresh_secs = 60\n",
                self.port
            ),
            Path::new("banto-hub-sink.toml"),
        )
        .expect("config");
        BridgeSupervisor::spawn(&config).expect("bridge")
    }
}

impl Drop for FakeBridgeHub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// 要求の行が `GET /api/ingest/bridge-config` か（読まずに覗く）。
async fn is_config_request(stream: &TcpStream) -> bool {
    let mut buf = [0u8; 128];
    for _ in 0..200 {
        let n = stream.peek(&mut buf).await.unwrap_or(0);
        if n == buf.len() || buf[..n].contains(&b'\n') {
            return buf[..n].starts_with(b"GET /api/ingest/bridge-config");
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    false
}

async fn serve_config(
    mut stream: TcpStream,
    config: &std::sync::Mutex<Value>,
    fail: &AtomicBool,
    hold: &AtomicBool,
    fetches: &AtomicU64,
) {
    let mut request = Vec::new();
    let mut buf = [0u8; 1024];
    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => request.extend_from_slice(&buf[..n]),
        }
    }
    while hold.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let response = if fail.load(Ordering::SeqCst) {
        "HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
            .to_string()
    } else {
        let body = config.lock().unwrap().to_string();
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
    };
    fetches.fetch_add(1, Ordering::SeqCst);
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

const FAKE_ID: i64 = 7;

/// 偽の Hub が返すブリッジの設定（source 1 つ・キー `k1` 1 つ）。`sql` だけを変えて
/// 「キーは同じで SQL を変えた」を作る。
fn fake_bridge_config(
    target: &PgTarget,
    role: &TestRole,
    name: &str,
    fingerprint: &str,
    sql: &str,
) -> Value {
    json!({ "generatedAt": 0, "sources": [ {
        "connectionId": FAKE_ID, "name": name, "staleTimeoutMs": 3_000,
        "configFingerprint": fingerprint, "ingestAllowed": true,
        "queries": [ { "name": "q", "periodMs": 500, "sql": sql,
                       "columns": [ { "column": "a", "key": "k1" } ] } ],
        "db": { "id": 1, "name": "pg", "enabled": true, "host": target.host,
                "port": target.port, "database": target.database,
                "username": role.name, "password": role.password } } ] })
}

const SQL_A: &str = "SELECT 1.5::float8 AS a";
const SQL_B: &str = "SELECT 2.5::float8 AS a";

async fn fake_welcome(ws: &mut WebSocketStream<TcpStream>, collecting: bool, epoch: u64) {
    let welcome = json!({ "op": "welcome", "connection_id": FAKE_ID, "connection": "fake",
        "collecting": collecting, "epoch": epoch, "stale_timeout_ms": 3_000,
        "limits": { "max_items": 1000, "max_message_bytes": 262_144,
                    "messages_per_sec": 20, "items_per_sec": 5000 } });
    ws.send(WsMessage::Text(welcome.to_string().into()))
        .await
        .unwrap();
}

/// `within` の間に届いた `values` と、その間にブリッジが WS を閉じたか。
async fn fake_values_within(
    ws: &mut WebSocketStream<TcpStream>,
    within: Duration,
) -> (Vec<Value>, bool) {
    let mut out = Vec::new();
    let deadline = tokio::time::Instant::now() + within;
    loop {
        match tokio::time::timeout_at(deadline, ws.next()).await {
            Err(_) => return (out, false),
            Ok(None) | Ok(Some(Err(_))) | Ok(Some(Ok(WsMessage::Close(_)))) => return (out, true),
            Ok(Some(Ok(WsMessage::Text(text)))) => {
                let value: Value = serde_json::from_str(text.as_str()).unwrap();
                if value["op"] == "values" {
                    out.push(value);
                }
            }
            Ok(Some(Ok(_))) => {}
        }
    }
}

/// 次の `values`（[`WAIT`] まで）。
async fn fake_next_values(ws: &mut WebSocketStream<TcpStream>) -> Value {
    let (values, closed) = fake_values_until(ws, |_| true).await;
    assert!(!closed, "the bridge closed the stream: {values:?}");
    values.into_iter().last().expect("values")
}

/// `done` が真になる `values` まで（[`WAIT`] まで）。それまでに届いた `values` を全部返す。
async fn fake_values_until(
    ws: &mut WebSocketStream<TcpStream>,
    done: impl Fn(&Value) -> bool,
) -> (Vec<Value>, bool) {
    let mut out = Vec::new();
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        match tokio::time::timeout_at(deadline, ws.next()).await {
            Err(_) => panic!("values within {WAIT:?}: {out:?}"),
            Ok(None) | Ok(Some(Err(_))) | Ok(Some(Ok(WsMessage::Close(_)))) => return (out, true),
            Ok(Some(Ok(WsMessage::Text(text)))) => {
                let value: Value = serde_json::from_str(text.as_str()).unwrap();
                if value["op"] == "values" {
                    let finished = done(&value);
                    out.push(value);
                    if finished {
                        return (out, false);
                    }
                }
            }
            Ok(Some(Ok(_))) => {}
        }
    }
}

/// `values` の 1 件目（キー `k1`）の (値, 品質, epoch)。
fn k1(value: &Value) -> (Option<f64>, String, u64) {
    let item = &value["items"][0];
    assert_eq!(item["key"], json!("k1"), "{value}");
    (
        item["v"].as_f64(),
        item["q"].as_str().unwrap_or_default().to_string(),
        value["epoch"].as_u64().unwrap(),
    )
}

/// ブリッジが WS を閉じるまで待つ（その間の `values` を返す）。
async fn fake_until_closed(ws: &mut WebSocketStream<TcpStream>) -> Vec<Value> {
    let (values, closed) = fake_values_within(ws, WAIT).await;
    assert!(closed, "the bridge must close the old stream: {values:?}");
    values
}

/// 回帰（PR #764 のオーナー再レビュー P2、§10.3 の実装の注 (3)）: 切断 → 同じキーで SQL を
/// 変更 → `config_changed` を一度も受信しないまま再接続 → 設定の取得の完了まで値を送らない
/// → 取得の後は新しい SQL の値を送る。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_config_change_while_disconnected_is_fetched_before_sending() {
    let target = require_pg!();
    let admin = PgPool::connect(&target.url).await.expect("connect test pg");
    let role = TestRole::create(&admin, "fresh_a").await;
    let name = unique("fresh_a");
    let mut hub =
        FakeBridgeHub::start(fake_bridge_config(&target, &role, &name, "sha256:a", SQL_A)).await;
    let bridge = hub.spawn_bridge();

    let mut ws = hub.next_socket().await;
    fake_welcome(&mut ws, true, 1).await;
    let (_, closed) = fake_values_until(&mut ws, |v| k1(v) == (Some(1.5), "good".into(), 1)).await;
    assert!(!closed);

    // 取得を遅らせ、切断（close なし）→ 切断中に SQL だけを変える（キーは同じ。
    // `config_changed` は送らない）。
    hub.hold.store(true, Ordering::SeqCst);
    drop(ws);
    hub.set_config(fake_bridge_config(&target, &role, &name, "sha256:b", SQL_B));

    // 張り直し（新しい epoch・収集中）: 取得が終わるまで値を送らない。
    let mut ws = hub.next_socket().await;
    fake_welcome(&mut ws, true, 2).await;
    let (during, _) = fake_values_within(&mut ws, Duration::from_millis(1_500)).await;
    assert!(
        during.is_empty(),
        "no values before the post-welcome fetch: {during:?}"
    );
    assert_eq!(
        bridge_status(&bridge, FAKE_ID).map(|s| s.state),
        Some(SourceState::AwaitingConfig),
        "{:?}",
        bridge.statuses()
    );

    // 取得が終わる: SQL が変わったので、監督が古い source（SQL A）を止める - その WS では
    // 何も送らずに閉じる。新しい source（SQL B）が張り直して B の値を送る。
    hub.hold.store(false, Ordering::SeqCst);
    let old = fake_until_closed(&mut ws).await;
    assert!(old.is_empty(), "the stale source sent: {old:?}");
    let mut ws = hub.next_socket().await;
    fake_welcome(&mut ws, true, 2).await;
    let (values, closed) = fake_values_until(&mut ws, |v| k1(v).0.is_some()).await;
    assert!(!closed);
    for value in &values {
        let (v, _, epoch) = k1(value);
        assert_eq!(epoch, 2, "{value}");
        assert_ne!(v, Some(1.5), "a value of the old SQL: {value}");
    }
    assert_eq!(k1(values.last().unwrap()), (Some(2.5), "good".into(), 2));
    let next = fake_next_values(&mut ws).await;
    assert_eq!(k1(&next).0, Some(2.5), "{next}");

    assert_eq!(
        bridge.shutdown(Duration::from_secs(10)).await,
        BridgeStop::Stopped
    );
    role.drop_role(&admin).await;
    admin.close().await;
}

/// 回帰（同上、最初の接続）: 監督が SQL A の設定で source を作った後、最初の `welcome` までに
/// 設定が B に変わる → `welcome` の後に始まった取得が成功するまで値を送らず、その後は B の値。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_config_change_before_the_first_welcome_is_fetched_before_sending() {
    let target = require_pg!();
    let admin = PgPool::connect(&target.url).await.expect("connect test pg");
    let role = TestRole::create(&admin, "fresh_b").await;
    let name = unique("fresh_b");
    let mut hub =
        FakeBridgeHub::start(fake_bridge_config(&target, &role, &name, "sha256:a", SQL_A)).await;
    let bridge = hub.spawn_bridge();

    // source（SQL A）が繋いできた。`welcome` を返す前に設定が B に変わる。
    let mut ws = hub.next_socket().await;
    hub.hold.store(true, Ordering::SeqCst);
    hub.set_config(fake_bridge_config(&target, &role, &name, "sha256:b", SQL_B));
    fake_welcome(&mut ws, true, 1).await;
    let (during, _) = fake_values_within(&mut ws, Duration::from_millis(1_500)).await;
    assert!(
        during.is_empty(),
        "no values before the post-welcome fetch: {during:?}"
    );

    hub.hold.store(false, Ordering::SeqCst);
    let old = fake_until_closed(&mut ws).await;
    assert!(old.is_empty(), "the stale source sent: {old:?}");
    let mut ws = hub.next_socket().await;
    fake_welcome(&mut ws, true, 1).await;
    let (values, closed) = fake_values_until(&mut ws, |v| k1(v).0.is_some()).await;
    assert!(!closed);
    assert!(
        values.iter().all(|v| k1(v).0 != Some(1.5)),
        "a value of the old SQL: {values:?}"
    );
    assert_eq!(k1(values.last().unwrap()), (Some(2.5), "good".into(), 1));

    assert_eq!(
        bridge.shutdown(Duration::from_secs(10)).await,
        BridgeStop::Stopped
    );
    role.drop_role(&admin).await;
    admin.close().await;
}

/// 回帰（同上）: `welcome` の後の取得が失敗している間は送らない（古い設定のまま再開しない）。
/// 取得が成功するようになったら送る。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failing_post_welcome_fetch_keeps_the_bridge_silent() {
    let target = require_pg!();
    let admin = PgPool::connect(&target.url).await.expect("connect test pg");
    let role = TestRole::create(&admin, "fresh_c").await;
    let name = unique("fresh_c");
    let mut hub =
        FakeBridgeHub::start(fake_bridge_config(&target, &role, &name, "sha256:a", SQL_A)).await;
    let bridge = hub.spawn_bridge();

    let mut ws = hub.next_socket().await;
    hub.fail.store(true, Ordering::SeqCst);
    let fetches_before = hub.fetches();
    fake_welcome(&mut ws, true, 1).await;
    let (during, closed) = fake_values_within(&mut ws, Duration::from_secs(3)).await;
    assert!(!closed);
    assert!(
        during.is_empty(),
        "no values while the fetch fails: {during:?}"
    );
    assert!(hub.fetches() > fetches_before, "the welcome must refetch");
    assert_eq!(
        bridge_status(&bridge, FAKE_ID).map(|s| (s.state, s.pollers_started)),
        Some((SourceState::AwaitingConfig, 0)),
        "{:?}",
        bridge.statuses()
    );

    // 取得が直る: 監督のバックオフの取り直しで成功し、送り始める（設定は同じ）。
    hub.fail.store(false, Ordering::SeqCst);
    let (_, closed) = fake_values_until(&mut ws, |v| k1(v) == (Some(1.5), "good".into(), 1)).await;
    assert!(!closed);

    assert_eq!(
        bridge.shutdown(Duration::from_secs(10)).await,
        BridgeStop::Stopped
    );
    role.drop_role(&admin).await;
    admin.close().await;
}

/// 回帰（同上）: 設定が変わっていなければ、張り直しの後の取得が終わり次第すぐ送る
/// （source は作り直さない・WS も張り直さない）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unchanged_config_resumes_promptly_after_a_reconnect() {
    let target = require_pg!();
    let admin = PgPool::connect(&target.url).await.expect("connect test pg");
    let role = TestRole::create(&admin, "fresh_d").await;
    let name = unique("fresh_d");
    let mut hub =
        FakeBridgeHub::start(fake_bridge_config(&target, &role, &name, "sha256:a", SQL_A)).await;
    let bridge = hub.spawn_bridge();

    let mut ws = hub.next_socket().await;
    fake_welcome(&mut ws, true, 1).await;
    let (_, closed) = fake_values_until(&mut ws, |v| k1(v) == (Some(1.5), "good".into(), 1)).await;
    assert!(!closed);
    drop(ws);

    let mut ws = hub.next_socket().await;
    let fetches_before = hub.fetches();
    let welcomed_at = tokio::time::Instant::now();
    fake_welcome(&mut ws, true, 2).await;
    let (_, closed) = fake_values_until(&mut ws, |v| k1(v) == (Some(1.5), "good".into(), 2)).await;
    assert!(!closed, "the source must not be rebuilt");
    assert!(
        welcomed_at.elapsed() < Duration::from_secs(5),
        "resumed after {:?}",
        welcomed_at.elapsed()
    );
    assert!(hub.fetches() > fetches_before, "the welcome must refetch");
    let status = bridge_status(&bridge, FAKE_ID).unwrap();
    assert_eq!(status.sessions, 2, "{status:?}");
    assert_eq!(status.state, SourceState::Running, "{status:?}");

    assert_eq!(
        bridge.shutdown(Duration::from_secs(10)).await,
        BridgeStop::Stopped
    );
    role.drop_role(&admin).await;
    admin.close().await;
}
