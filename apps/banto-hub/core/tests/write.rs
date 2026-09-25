//! T2-4 の統合テスト（docs/tag-server-design.md §6 全体）: `banto-plc-write`
//! の読み書き両対応シミュレータ + `banto-broker` 経由での
//! `POST /api/v1/values/{tag}` 安全ゲート一式の E2E。
//!
//! `tests/integration.rs`/`tests/stream.rs` と同じ理由（各 `tests/*.rs` は
//! 独立したクレートとしてコンパイルされ、private helper を共有できない）で
//! `fast_options`/`wait_until` 相当をこのファイル内に複製している。
//! `TempEnv` は `tests/common/mod.rs` に集約済み（2026-08-08、テスト一時
//! ディレクトリリークの根治）。
//!
//! テスト構成（実装指示 §6 のテスト計画1〜4に対応。5「再起動安全」は
//! `banto_hub_core::write_control` 自身の単体テスト
//! （`a_new_write_control_from_a_persisted_enabled_state_is_disabled` 等）で
//! 既に確認済みなのでここでは重複させない）:
//! 1. E2E ハッピーパス
//! 2. ゲート網羅
//! 3. レート制限（タグ毎・全体）
//! 4. log-before-write（成功/失敗）

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
use banto_hub_core::db::init_db;
use banto_hub_core::grpc::{GrpcServer, GrpcService};
use banto_hub_core::hub::CollectorManager;
use banto_hub_core::rest::api_router;
use banto_hub_core::settings::SettingsService;
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

mod common;
use common::TempEnv;

const CLIENT_HEADER: (&str, &str) = ("X-Banto-Client", "banto");

/// Temp-dir prefix passed to `TempEnv::new` (see `tests/common/mod.rs`).
const TEMP_ENV_PREFIX: &str = "banto-hub-write-it";

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
        database: None,
        username: None,
        password: None,
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
        default_writable: true,
        query_sql: None,
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
        string_encoding: "utf8".to_string(),
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

struct TestApp {
    router: Router,
    admin_token: String,
    pool: SqlitePool,
    manager: Arc<CollectorManager>,
    write_control: Arc<WriteControl>,
    /// #131 (2026-09-01): the SAME `Arc` `manager`'s internals hold - needed
    /// so a test can reach `HubSessions::write_handle_for` directly, the way
    /// `tests/t15_write_peek.rs`/`tests/t9_simulation.rs` already do, to
    /// exercise the broker session below `write_path::execute_write`'s own
    /// gates.
    sessions: Arc<HubSessions>,
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
    let admin_token = auth
        .login("admin", "password123")
        .await
        .expect("admin login");

    let sessions = Arc::new(HubSessions::new(banto_broker::BackoffConfig::default()));
    let sim_registry = Arc::new(BrokerSimRegistry::new());
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
        grpc_server,
        rate_limiter,
        banto_hub_core::profile_paths::DEFAULT_PROFILE_ID.to_string(),
    );

    TestApp {
        router,
        admin_token,
        pool,
        manager,
        write_control,
        sessions,
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
        wait_until(Duration::from_secs(10), || async {
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
// 1b. #131 (2026-09-01): the same E2E happy path, but for a Modbus TCP tag -
//    proves banto-broker's new "modbus-tcp" driver actually lets a Modbus
//    connection's writable tag be written through
//    POST /api/v1/values/{tag}, landing on the wire via
//    `banto_plc_write::modbus::simulator::Simulator`. #131 left Modbus reads
//    on banto-collect's own direct `ModbusTcpClient`, so the collection
//    read-back below used to travel over a SECOND socket to the same external
//    simulator process (the two-socket tradeoff docs/tag-server-design.md §6
//    item 5 accepted at the time). **#337 (2026-09-08) reversed that**: reads
//    and writes now share the one broker session, exactly like the SLMP test
//    above, so this test is now a genuine "読み書き単一セッション" round trip
//    for Modbus too (see `banto_hub_core::broker_glue::hub_client_factory`'s
//    doc comment, and `tests/integration.rs`'s
//    `modbus_collection_uses_exactly_one_socket` for the socket-count
//    assertion itself).
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_modbus_write_then_collection_reads_the_value_back() {
    let app = test_app("e2e-modbus-happy").await;
    let sim = banto_plc_write::modbus::simulator::Simulator::start().await;

    let (tag_id, external_name) = make_tag(
        &app,
        "line1",
        "modbus-tcp",
        sim.addr.port(),
        "temp01",
        "40001",
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
        sim.get_holding_register(0),
        1234,
        "value must land on the wire (via the broker's modbus-tcp driver)"
    );

    // 収集が同じシミュレータから読み戻すことの確認。#131 当時これは
    // 「書き込みは broker、読み取りは直接クライアント」の別々のソケット
    // 経由だったが、#337(2026-09-08)以降は同じ broker セッション1本を
    // 共有する - SLMP と同じ「読み書き単一セッション」の往復になった。
    let tag_key = format!("tag:{tag_id}");
    assert!(
        wait_until(Duration::from_secs(10), || async {
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
// 1c. #131 (2026-09-01) simulation-safety regression test for the Part 1 fix
//    to `BrokerSimRegistry::resolve`: a Modbus connection with
//    `simulation = true` must have its broker session dial the in-process
//    MODBUS-speaking simulator `BrokerSimRegistry` substitutes, NOT the
//    connection's configured (real, unreachable in this test) host/port, and
//    NOT (the bug that fix closed) an SLMP-speaking simulator mismatched
//    against the Modbus wire protocol the broker's `ModbusSession` actually
//    speaks.
//
//    #363 (2026-09-15 オーナー決定) rewrote what "success" means here. The
//    original version of this test could not use `POST /api/v1/values/{tag}`
//    at all, because `write_path::execute_write`'s old gate 4
//    (`WriteRejection::SimulationWriteRejected`) rejected every write to a
//    `simulation = true` PLC tag, and the in-process simulator was read-only
//    on the wire - so the strongest available assertion was a *well-formed*
//    `WriteResult::Bad(ModbusException { code: 1 })` from the broker-direct
//    path (proof the broker had dialed a peer that speaks valid Modbus TCP
//    framing). Both halves of that are gone: the gate is removed and
//    `banto_plc::modbus::simulator::Simulator` now serves FC5/6/15/16 and
//    holds what was written against the ramp task. So this test now takes
//    the ordinary REST route and asserts the whole thing end to end - the
//    write is accepted, it lands on the substituted Modbus simulator, and it
//    reads back through `read-now` - which subsumes the old assertion (a
//    value can only read back if the broker dialed a real Modbus-speaking
//    peer) while also covering the new contract.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn simulated_modbus_connection_write_lands_on_the_in_process_simulator_and_reads_back() {
    let app = test_app("e2e-modbus-sim-write").await;

    let conn = PlcConnectionService::new(app.pool.clone())
        .create(PlcConnectionInput {
            simulation: true,
            // Port 1 is a privileged port nothing in this test suite listens
            // on - connecting to it fails fast (connection refused) rather
            // than hanging, so if the Part 1 fix ever regressed and this
            // dialed the connection's own host/port instead of the
            // simulator, the test would fail fast rather than time out.
            ..modbus_conn_input("line1", 1)
        })
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    // Offset 16 ("40017") is deliberately outside
    // `banto_collect::simulation::RAMP_ADDRESS_COUNT` (16) so this test says
    // nothing about the held set either way - that is
    // `tests/t9_simulation_write.rs`'s subject. Here the point is only that
    // the substituted simulator speaks Modbus writes at all.
    TagService::new(app.pool.clone())
        .create(tag_input("temp01", group.id, "40017", "u16", true, true))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild");
    app.write_control.enable();

    assert!(
        wait_until(Duration::from_secs(10), || async {
            app.manager
                .broker_status(conn.id)
                .map(|s| s == banto_broker::BrokerConnectionStatus::Connected)
                .unwrap_or(false)
        })
        .await,
        "the broker session should reach Connected against the substituted simulator, not hang \
         trying to dial the unreachable real host/port"
    );

    let (key, _id) = issue_key(
        &app.router,
        &app.admin_token,
        "writer",
        &["write:line1.fast.temp01", "read:line1.fast.temp01"],
    )
    .await;
    let (status, body) = v1_post(
        &app.router,
        "/api/v1/values/line1.fast.temp01",
        &key,
        json!({ "v": 4242 }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "#363: a write to a simulated connection is applied, not rejected: {body:?}"
    );
    assert_eq!(body["result"], "ok");

    // The broker session is still peekable and Modbus-speaking (the #131
    // property), and a read through it returns what was just written.
    assert!(
        app.sessions.write_handle_for(conn.id).is_some(),
        "a live broker session should be peekable for this connection"
    );
    let (status, read_now) = get_json(
        &app.router,
        "/api/v1/values/line1.fast.temp01/read-now",
        &key,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{read_now:?}");
    assert_eq!(
        read_now["v"], 4242.0,
        "the written value must read back off the in-process simulator"
    );
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

/// #131 (2026-09-01) regression note: this test used to be named
/// `gate_modbus_connection_is_501_write_unsupported_protocol` and asserted a
/// Modbus tag write was rejected at the protocol gate (`write_path.rs` gate
/// 5, formerly a literal `conn.protocol != "slmp"` check) with a 501. That
/// premise is now false - `banto_broker` registered a `"modbus-tcp"` driver
/// (#131), `write_path::execute_write`'s protocol gate is now
/// `banto_broker::is_supported_protocol`, and a Modbus tag no longer trips it
/// at all. Renamed (matching how `crates/banto-broker`'s own
/// `supervisor_rejects_a_modbus_tcp_connection` was renamed to
/// `supervisor_rejects_an_unsupported_protocol_connection` in the same PR)
/// to pin down what actually happens now instead of deleting the coverage:
/// the connection points at a port nothing listens on (15099, same as
/// before), so the write now passes the protocol gate, reaches the broker
/// session (which `rebuild` spawned but which never manages to connect), and
/// fails at the broker call itself with `WriteRejection::WriteFailed` (502) -
/// proving the gate genuinely passed rather than the write succeeding for
/// the wrong reason (e.g. some other gate silently absorbing the rejection).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gate_modbus_connection_now_passes_the_protocol_gate_and_fails_only_at_the_broker_call() {
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
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body:?}");
    assert_eq!(body["error"], "write_failed");
}

/// #131 (2026-09-01) gate coverage: `write_path::execute_write`'s protocol
/// gate (gate 5) still rejects a connection whose protocol has no
/// `banto_broker` driver - proven here with `"virtual"`
/// (`banto_tags::ALLOWED_PROTOCOLS` allows it at the registry layer, but
/// `banto_broker::is_supported_protocol` does not, so it is a genuine
/// still-unsupported protocol string, unlike `"modbus-tcp"` above). A `plc`
/// tag cannot normally be placed under any `"virtual"`-protocol connection
/// (`banto_tags::tag::validate_tag_kind_placement` rejects it unconditionally
/// - "plc タグは予約接続（calc/mem）配下に作成できません" - regardless of the
/// connection's name), so this test bypasses the registry service layer with
/// a raw SQL `UPDATE` after creating an ordinary Modbus connection/group/tag
/// through the normal API, mirroring this codebase's existing convention for
/// exercising a defensive/otherwise-unreachable-via-CRUD branch (see
/// `crates/banto-tags/src/plc_connection.rs`'s
/// `the_sql_check_accepts_nothing_beyond_allowed_protocols`-style tests).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gate_unsupported_broker_protocol_is_501_write_unsupported_protocol() {
    let app = test_app("gate-unsupported-protocol").await;
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

    // Bypass PlcConnectionService (which enforces ALLOWED_PROTOCOLS = the SQL
    // CHECK's list, `"modbus-tcp" | "slmp" | "virtual"`) to land on a
    // protocol string the SQL CHECK still allows but `banto_broker` has no
    // driver for - "virtual" is the only such string in ALLOWED_PROTOCOLS
    // today, so this is not a fully synthetic value, just one the registry
    // layer would otherwise never let a `plc`-kind tag's connection use.
    sqlx::query("UPDATE plc_connections SET protocol = 'virtual' WHERE name = 'line1'")
        .execute(&app.pool)
        .await
        .expect("hand-edit the connection's protocol");
    app.manager
        .rebuild()
        .await
        .expect("rebuild after hand-edit");

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
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{body:?}");
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

/// T20-3a 監査対応(2026-09-05)の回帰ガード: バッチの事前ゲート
/// all-or-nothing を実現するため、gate 1〜4・値型 present を
/// `resolve_write_target` へ、gate 7(値変換・型対称性)を `convert_value`
/// へそれぞれ抽出したリファクタの初版は、`execute_write`(単票)でも
/// gate 7 を gate 5(write_control off)より前に動かしてしまい、
/// 「write_control off ＋ 型不一致の値」が本来の 503(writes_disabled)では
/// なく 422(unsupported_value_type)になる回帰を生んでいた。この場合の
/// 意味論は「書き込みが無効なら値の妥当性を見る前に拒否する」(gate 7 は
/// gate 5/6 の**後**)であるべきで、`execute_write`は現在この元の順序
/// (resolve → gate 5 → gate 6 → convert_value → gate 8)を厳密に守って
/// いる - この回帰が再発したら 422 で失敗する形で固定する(数値タグに
/// bool を送るケースを使う - 単体では `gate_bool_value_to_a_numeric_tag_is_422`
/// が 422 を返すことを別途固定している、その対になるテスト)。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gate_writes_disabled_wins_over_a_type_mismatched_value_and_stays_503() {
    let app = test_app("gate-writes-disabled-vs-type-mismatch").await;
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
    // write_control は既定 disabled のまま(app.write_control.enable() を呼ばない)。
    let (key, _id) = issue_key(
        &app.router,
        &app.admin_token,
        "writer",
        &["write:line1.fast.temp01"],
    )
    .await;

    // "temp01" は数値タグ(u16)だが、真偽値(型不一致 - 単体なら 422 の原因)
    // を送る。write_control が off の間は、値の型を見るより先に 503 で
    // 拒否されるのが元の(かつ正しい)挙動。
    let (status, body) = v1_post(
        &app.router,
        &format!("/api/v1/values/{external_name}"),
        &key,
        json!({ "v": true }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "writes_disabled must win over a type-mismatched value (422 would be the regression): {body:?}"
    );
    assert_eq!(body["error"], "writes_disabled");
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

/// #340 レビュー対応（2026-09-14）: `enabled_persisted` が次回起動時の
/// ライブ値そのものになったため、enable は永続化に成功したときだけ
/// ライブフラグを立てる。`persist_enabled` を強制失敗させ、500
/// `write_control_persist_failed` を返しつつライブフラグは disabled の
/// ままであることを確認する（ハンドラが panic しないことも同時に確認する）。
///
/// banto v1.7.0 #204 以降、管理 REST のセッションも**要求ごとに DB
/// （`users`）と照合する**ため、以前のように pool を丸ごと閉じると、
/// ハンドラに届く前に認証の照合が失敗する（500、セッションは残す）。MCP 版
/// （`tests/mcp.rs`）と同じく `write_control_state` テーブルだけを drop し、
/// `users`（→認証）と `audit_log`（→監査）は生かしたまま `persist_enabled`
/// の UPDATE だけを失敗させる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rest_enable_returns_500_and_stays_disabled_when_persistence_fails() {
    let app = test_app("write-control-persist-fail-enable").await;
    assert!(!app.write_control.is_enabled());

    sqlx::query("DROP TABLE write_control_state")
        .execute(&app.pool)
        .await
        .expect("drop write_control_state");

    let (status, body) =
        admin_post_empty(&app.router, "/api/write-control/enable", &app.admin_token).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body:?}");
    assert_eq!(body["error"], "write_control_persist_failed");
    assert!(
        !app.write_control.is_enabled(),
        "enable must not flip the live flag when persistence fails"
    );
}

/// #340 レビュー対応（2026-09-14）: disable（非常停止）はライブフラグを
/// 先に落とすため、永続化が失敗しても書き込みは止まったままになる -
/// 500 `write_control_persist_failed` を返しつつライブフラグは disabled
/// （止まっている）ことを確認する。上のテストと同じ理由（#204 の要求ごとの
/// 照合）で、pool を閉じずに `write_control_state` だけを drop する。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rest_disable_returns_500_but_stays_disabled_when_persistence_fails() {
    let app = test_app("write-control-persist-fail-disable").await;
    app.write_control.enable();
    assert!(app.write_control.is_enabled());

    sqlx::query("DROP TABLE write_control_state")
        .execute(&app.pool)
        .await
        .expect("drop write_control_state");

    let (status, body) =
        admin_post_empty(&app.router, "/api/write-control/disable", &app.admin_token).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body:?}");
    assert_eq!(body["error"], "write_control_persist_failed");
    assert!(
        !app.write_control.is_enabled(),
        "disable must fail closed (live flag stays off) even when persistence fails"
    );
}

// ---------------------------------------------------------------------------
// T20 ①a (docs/banto-hub-t20-design.md §3.1, 案A): 文字列タグへの単票書き込み
// (`POST /api/v1/values/{tag}`)。`make_tag`/`tag_input`(数値/bit 専用)とは別
// に string タグ専用のフィクスチャを立て、UTF-8/Shift-JIS それぞれで
// シミュレータのワイヤに正しいバイト列が届くことを確認する - `encode.rs`の
// ユニットテストが証明済みの符号化ロジックが、実際に REST -> write_path ->
// broker -> banto-plc-write という実経路を通っても壊れないことの E2E 証拠。
// 記録計(banto-collect の`current_values`)は数値専用のまま(案Aの境界)なので、
// 数値タグの happy path テストと違って読み戻し検証は行わない。
// ---------------------------------------------------------------------------

/// `tag_input`(数値/bit 専用)の string タグ版。`writable`/`enabled`は常に
/// true(このファイルの string タグテストは全て書き込み成功系のため)。
fn string_tag_input(
    name: &str,
    group_id: i64,
    address: &str,
    string_length: i64,
    string_encoding: &str,
) -> TagInput {
    TagInput {
        string_length: Some(string_length),
        string_encoding: string_encoding.to_string(),
        ..tag_input(name, group_id, address, "string", true, true)
    }
}

/// `make_tag`の string タグ版。
async fn make_string_tag(
    app: &TestApp,
    conn_name: &str,
    port: u16,
    tag_name: &str,
    address: &str,
    string_length: i64,
    string_encoding: &str,
) -> String {
    let conn = PlcConnectionService::new(app.pool.clone())
        .create(slmp_conn_input(conn_name, port))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(string_tag_input(
            tag_name,
            group.id,
            address,
            string_length,
            string_encoding,
        ))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild");
    format!("{conn_name}.fast.{tag_name}")
}

/// The raw bytes landed at `words` consecutive `D`-device words on `sim`,
/// low byte first per word (mirrors `encode_string_value`'s packing - see
/// that function's doc comment in `banto-plc-write/src/encode.rs`).
fn read_wire_bytes(sim: &Simulator, start: u32, words: u16) -> Vec<u8> {
    (0..words as u32)
        .flat_map(|i| sim.get_word(SlmpDevice::D, start + i).to_le_bytes())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn string_tag_write_lands_utf8_bytes_on_the_wire() {
    let app = test_app("string-write-utf8").await;
    let sim = Simulator::start().await;

    let words = 5u16; // "テスト" is 9 UTF-8 bytes; 5 words (10 bytes) leaves 1 NUL pad byte.
    let external_name = make_string_tag(
        &app,
        "line1",
        sim.addr.port(),
        "recipe",
        "D3000",
        words as i64,
        "utf8",
    )
    .await;
    app.write_control.enable();

    let (key, _id) = issue_key(
        &app.router,
        &app.admin_token,
        "writer",
        &["write:line1.fast.recipe"],
    )
    .await;

    let (status, body) = v1_post(
        &app.router,
        &format!("/api/v1/values/{external_name}"),
        &key,
        json!({ "v": "テスト" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"], "ok");

    let landed = read_wire_bytes(&sim, 3000, words);
    let mut expected = "テスト".as_bytes().to_vec();
    expected.resize(words as usize * 2, 0x00);
    assert_eq!(
        landed, expected,
        "UTF-8 bytes must land on the wire, NUL-padded, low byte first"
    );

    sim.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn string_tag_write_lands_shift_jis_bytes_on_the_wire() {
    let app = test_app("string-write-sjis").await;
    let sim = Simulator::start().await;

    let words = 4u16; // "テスト" is 6 Shift-JIS bytes; 4 words (8 bytes) leaves 2 NUL pad bytes.
    let external_name = make_string_tag(
        &app,
        "line1",
        sim.addr.port(),
        "recipe",
        "D3000",
        words as i64,
        "shift_jis",
    )
    .await;
    app.write_control.enable();

    let (key, _id) = issue_key(
        &app.router,
        &app.admin_token,
        "writer",
        &["write:line1.fast.recipe"],
    )
    .await;

    let (status, body) = v1_post(
        &app.router,
        &format!("/api/v1/values/{external_name}"),
        &key,
        json!({ "v": "テスト" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"], "ok");

    let landed = read_wire_bytes(&sim, 3000, words);
    // "テスト" in Shift-JIS = [0x83, 0x65, 0x83, 0x58, 0x83, 0x67] (same
    // fixed value `banto-plc-write/src/encode.rs`'s own unit tests pin down -
    // hardcoded here rather than pulling in `encoding_rs` as a direct
    // dependency of this test crate just to recompute it).
    let mut expected: Vec<u8> = vec![0x83, 0x65, 0x83, 0x58, 0x83, 0x67];
    expected.resize(words as usize * 2, 0x00);
    assert_eq!(
        landed, expected,
        "Shift-JIS bytes must land on the wire, NUL-padded, low byte first"
    );

    sim.stop();
}

/// gate 7 の型対称性(string タグには文字列のみ): 数値を string タグへ書こう
/// とすると 422 `unsupported_value_type`(convert_value のdoc comment参照)。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn string_tag_rejects_a_numeric_value_as_422() {
    let app = test_app("string-write-type-mismatch").await;
    let sim = Simulator::start().await;

    let external_name =
        make_string_tag(&app, "line1", sim.addr.port(), "recipe", "D3000", 4, "utf8").await;
    app.write_control.enable();

    let (key, _id) = issue_key(
        &app.router,
        &app.admin_token,
        "writer",
        &["write:line1.fast.recipe"],
    )
    .await;

    let (status, body) = v1_post(
        &app.router,
        &format!("/api/v1/values/{external_name}"),
        &key,
        json!({ "v": 123 }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    assert_eq!(body["error"], "unsupported_value_type");

    sim.stop();
}

/// 型対称性の逆方向: 文字列を数値タグへ書こうとすると 422。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn numeric_tag_rejects_a_string_value_as_422() {
    let app = test_app("numeric-write-rejects-string").await;
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
        json!({ "v": "not a number" }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    assert_eq!(body["error"], "unsupported_value_type");

    sim.stop();
}

/// 文字列長超過は 422 相当の範囲エラー(`value_out_of_range`)- broker を
/// 呼ぶ前の事前チェック(`build_plc_string_write_request`のdoc comment参照)
/// で弾かれ、ワイヤには一切触れない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn string_tag_write_over_capacity_is_a_value_out_of_range_error() {
    let app = test_app("string-write-overflow").await;
    let sim = Simulator::start().await;

    // 1 word = 2 bytes capacity; "ABC" (3 ASCII bytes) does not fit.
    let external_name =
        make_string_tag(&app, "line1", sim.addr.port(), "recipe", "D3000", 1, "utf8").await;
    app.write_control.enable();

    let (key, _id) = issue_key(
        &app.router,
        &app.admin_token,
        "writer",
        &["write:line1.fast.recipe"],
    )
    .await;

    // Seed the word so we can prove it was never touched.
    sim.set_word(SlmpDevice::D, 3000, 0xBEEF);

    let (status, body) = v1_post(
        &app.router,
        &format!("/api/v1/values/{external_name}"),
        &key,
        json!({ "v": "ABC" }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    assert_eq!(body["error"], "value_out_of_range");
    assert_eq!(
        sim.get_word(SlmpDevice::D, 3000),
        0xBEEF,
        "an out-of-range string write must never touch the wire"
    );

    sim.stop();
}

// ---------------------------------------------------------------------------
// #431: DB が応答しないときの緊急停止（banto v1.7.0 #204 の照合の例外）

/// `users` を一時的に読めなくする（照合だけを失敗させる。`write_control_state`
/// と `audit_log` は生かすので、停止の永続化と監査は動く）。
async fn break_users(pool: &SqlitePool) {
    sqlx::query("ALTER TABLE users RENAME TO users_away")
        .execute(pool)
        .await
        .expect("rename users");
}

async fn restore_users(pool: &SqlitePool) {
    sqlx::query("ALTER TABLE users_away RENAME TO users")
        .execute(pool)
        .await
        .expect("restore users");
}

/// #431: DB がセッションを照合できないあいだも、それまで有効だった管理者の
/// セッションなら緊急停止（disable）は通り、監査に「照合できなかったため、
/// 停止のみ例外で許可」の印が残る。再開（enable）は例外にしない（500 のまま、
/// 書き込みは止まったまま）。知らないトークンは例外でも通さない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disable_is_accepted_while_the_session_cannot_be_verified_but_enable_is_not() {
    let app = test_app("write-control-unverified-stop").await;
    app.write_control.enable();
    assert!(app.write_control.is_enabled());

    break_users(&app.pool).await;

    let (status, body) =
        admin_post_empty(&app.router, "/api/write-control/enable", &app.admin_token).await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "enable is not an exception: {body:?}"
    );

    let (status, _) = admin_post_empty(
        &app.router,
        "/api/write-control/disable",
        "not-a-known-token",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "unknown token is never excepted"
    );
    assert!(app.write_control.is_enabled());

    let (status, body) =
        admin_post_empty(&app.router, "/api/write-control/disable", &app.admin_token).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["write_enabled"], false);
    assert!(!app.write_control.is_enabled(), "writes are stopped");

    let (status, body) =
        admin_post_empty(&app.router, "/api/write-control/enable", &app.admin_token).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body:?}");
    assert!(!app.write_control.is_enabled(), "enable stays refused");

    let detail: String = sqlx::query_scalar(
        "SELECT detail FROM audit_log WHERE action = 'disable' AND resource = 'write_control' \
         ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&app.pool)
    .await
    .expect("disable audit row");
    let detail: Value = serde_json::from_str(&detail).unwrap();
    assert_eq!(
        detail["sessionCheck"], "unverified_stop_exception",
        "{detail}"
    );
    assert_eq!(detail["enabled"], false);

    // The session survived the outage: once the DB answers, it works again
    // (and a normal disable carries no exception mark).
    restore_users(&app.pool).await;
    let (status, _) =
        admin_post_empty(&app.router, "/api/write-control/disable", &app.admin_token).await;
    assert_eq!(status, StatusCode::OK);
    let detail: String = sqlx::query_scalar(
        "SELECT detail FROM audit_log WHERE action = 'disable' AND resource = 'write_control' \
         ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    let detail: Value = serde_json::from_str(&detail).unwrap();
    assert!(detail.get("sessionCheck").is_none(), "{detail}");
}

/// #431: 失効が**確認できた**セッションは、DB が応答していても緊急停止を
/// 拒否する（例外は「照合できない」ときだけ）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revoked_session_cannot_stop_writes() {
    let app = test_app("write-control-revoked-stop").await;
    app.write_control.enable();

    let (status, body) = admin_post(
        &app.router,
        "/api/users",
        &app.admin_token,
        json!({
            "username": "admin2",
            "password": "password123",
            "displayName": "Admin 2",
            "role": "admin",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let admin2_id = body["id"].as_i64().unwrap();
    let (status, body) = admin_post(
        &app.router,
        "/api/auth/login",
        "",
        json!({ "username": "admin2", "password": "password123" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let admin2_token = body["token"].as_str().unwrap().to_string();

    let (status, _) = admin_post(
        &app.router,
        &format!("/api/users/{admin2_id}/reset-password"),
        &app.admin_token,
        json!({ "newPassword": "reset-password1" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) =
        admin_post_empty(&app.router, "/api/write-control/disable", &admin2_token).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(
        app.write_control.is_enabled(),
        "a revoked session stops nothing"
    );
}
