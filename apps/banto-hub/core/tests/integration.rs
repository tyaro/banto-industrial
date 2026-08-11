//! T0-1 の統合テスト: 実際の axum `Router`（`tower::ServiceExt::oneshot`
//! 経由）+ Modbus TCP シミュレータ（`banto_plc::modbus::simulator`）を使った
//! E2E。足場（`TempEnv`/`fast_options`/`wait_until`）は
//! `crates/banto-collect/tests/integration.rs` を流用している。

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
use banto_hub_core::hub::CollectorManager;
use banto_hub_core::rest::api_router;
use banto_hub_core::settings::{SettingsService, DEFAULT_PORT, DEFAULT_RETENTION_DAYS};
use banto_hub_core::users::UsersService;
use banto_plc::modbus::simulator::Simulator;
use banto_plc::slmp::address::SlmpDevice;
use banto_plc::slmp::simulator::Simulator as SlmpSimulator;
use banto_server::{AuthState, Identity};
use banto_tags::{
    CollectionGroupInput, CollectionGroupService, PlcConnectionInput, PlcConnectionService,
    TagInput, TagService,
};
use banto_tstore::SystemClock;
use serde_json::Value;
use sqlx::SqlitePool;
use tokio::sync::broadcast;
use tower::ServiceExt;

mod common;
use common::TempEnv;

const CLIENT_HEADER: (&str, &str) = ("X-Banto-Client", "banto");

/// Temp-dir prefix passed to `TempEnv::new` - identifies this file's
/// directories among any left behind by a panicking test (see
/// `tests/common/mod.rs`'s module doc for why `TempEnv::drop`'s retry can't
/// always save a panicking test).
const TEMP_ENV_PREFIX: &str = "banto-hub-it";

/// Fast timings so the E2E tests finish quickly (mirrors
/// `banto-collect`'s own `fast_options`).
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

fn conn_input(name: &str, port: u16) -> PlcConnectionInput {
    PlcConnectionInput {
        name: name.to_string(),
        protocol: "modbus-tcp".to_string(),
        host: "127.0.0.1".to_string(),
        port: port as i64,
        unit_id: 1,
        enabled: true,
        simulation: false,

        word_order: "low_high".to_string(),
    }
}

/// I8 (2026-08-05, crates/banto-collect の SLMP 対応): the `"slmp"` twin of
/// [`conn_input`], used by [`e2e_read_slmp_via_rest_after_rebuild`].
fn slmp_conn_input(name: &str, port: u16) -> PlcConnectionInput {
    PlcConnectionInput {
        protocol: "slmp".to_string(),
        ..conn_input(name, port)
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

fn tag_input(name: &str, group_id: i64, address: &str, data_type: &str) -> TagInput {
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
        enabled: true,
        writable: false,
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

/// Everything one E2E test needs: a logged-in admin token, the assembled
/// `Router`, the registry pool (to seed I1 rows directly, faster/less
/// verbose than round-tripping every fixture through REST), the
/// `CollectorManager` (to call `rebuild()` directly rather than only via a
/// CRUD write, per T0-1's "再構築" test case), and the owning `TempEnv`
/// (kept alive for the whole test via the return tuple).
struct TestApp {
    router: Router,
    token: String,
    pool: SqlitePool,
    manager: Arc<CollectorManager>,
    /// T2-2 (docs/tag-server-design.md §6-5): the broker session directory
    /// `manager` was built with - exposed so a test can inspect broker-side
    /// state directly (e.g. `sessions.status_watch`) without going through
    /// REST, the way `e2e_slmp_session_survives_a_rebuild_via_broker` below
    /// does.
    sessions: Arc<HubSessions>,
    _env: TempEnv,
}

// `tests/common/mod.rs`'s module doc ("Why `TestApp` also needs
// `shutdown_test_app`"): without this, the background collector tasks
// `manager` spawned stay alive past this test's scope and keep the
// registry `SqlitePool` connections they hold checked out, so
// `TempEnv::drop`'s retry can never succeed.
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
    // T2-4: WriteControl always constructs disabled (docs/tag-server-design.md
    // §6-6) - no persisted state to read for a fresh test DB either way.
    let write_control = Arc::new(banto_hub_core::write_control::WriteControl::new(false));
    let write_audit = banto_hub_core::write_audit::WriteAuditService::new(pool.clone());
    let mqtt = Arc::new(banto_hub_core::mqtt::MqttPublisher::new(manager.clone()));
    let api_keys = ApiKeysService::new(pool.clone());
    // T4: this file exercises T0/T1 REST/WS behaviour only (`tests/grpc.rs`
    // covers gRPC) - `api_router`'s T4 arguments (the REST/gRPC-shared
    // rate_limiter and `GrpcServer`) are still required, so construct them
    // without ever calling `apply` (never binds a port).
    let rate_limiter = Arc::new(tokio::sync::Mutex::new(
        banto_hub_core::write_rate::WriteRateLimiter::new(
            banto_hub_core::write_rate::WriteRateLimitConfig::default(),
        ),
    ));
    let grpc_service = banto_hub_core::grpc::GrpcService::new(
        manager.clone(),
        api_keys.clone(),
        audit.clone(),
        write_audit.clone(),
        write_control.clone(),
        rate_limiter.clone(),
        events_tx.clone(),
    );
    let grpc_server = Arc::new(banto_hub_core::grpc::GrpcServer::new(grpc_service));
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
        write_control,
        write_audit,
        mqtt,
        grpc_server,
        rate_limiter,
        banto_hub_core::profile_paths::DEFAULT_PROFILE_ID.to_string(),
    );

    TestApp {
        router,
        token,
        pool,
        manager,
        sessions,
        _env: env,
    }
}

async fn get_json(router: &Router, path: &str, token: &str) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            HttpRequest::get(path)
                .header("Authorization", format!("Bearer {token}"))
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

/// `POST`/`PUT` through the admin surface - needs both the bearer token AND
/// the `X-Banto-Client` CSRF header (unlike `/api/v1/*`).
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

// ---------------------------------------------------------------------------
// 1. E2E 読み取り: シミュレータ -> レジストリ -> rebuild -> /api/v1/values/{tag}
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_read_via_rest_after_rebuild() {
    let app = test_app("e2e-read").await;
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 1234); // 40001

    let conn = PlcConnectionService::new(app.pool.clone())
        .create(conn_input("line1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("temp01", group.id, "40001", "i16"))
        .await
        .unwrap();

    app.manager.rebuild().await.expect("rebuild after seeding");

    assert!(
        wait_until(Duration::from_secs(3), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get("tag:1"))
                .map(|s| s.value)
                == Some(Some(1234.0))
        })
        .await,
        "collector should observe the simulator value"
    );

    let (status, json) =
        get_json(&app.router, "/api/v1/values/line1.fast.temp01", &app.token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["tag"], "line1.fast.temp01");
    assert_eq!(json["v"], 1234.0);
    assert_eq!(json["q"], "good");

    sim.stop();
}

// ---------------------------------------------------------------------------
// 1b. E2E 読み取り (SLMP, I8): シミュレータ -> レジストリ -> rebuild ->
//     /api/v1/values/{tag} - the SLMP twin of `e2e_read_via_rest_after_rebuild`,
//     proving banto-collect's SLMP wiring reaches all the way through the hub.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_read_slmp_via_rest_after_rebuild() {
    let app = test_app("e2e-read-slmp").await;
    let sim = SlmpSimulator::start().await;
    sim.set_word(SlmpDevice::D, 100, 4321);

    let conn = PlcConnectionService::new(app.pool.clone())
        .create(slmp_conn_input("line1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("temp01", group.id, "D100", "i16"))
        .await
        .unwrap();

    app.manager.rebuild().await.expect("rebuild after seeding");

    assert!(
        wait_until(Duration::from_secs(3), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get("tag:1"))
                .map(|s| s.value)
                == Some(Some(4321.0))
        })
        .await,
        "collector should observe the SLMP simulator value"
    );

    let (status, json) =
        get_json(&app.router, "/api/v1/values/line1.fast.temp01", &app.token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["tag"], "line1.fast.temp01");
    assert_eq!(json["v"], 4321.0);
    assert_eq!(json["q"], "good");

    sim.stop();
}

// ---------------------------------------------------------------------------
// 1c. E2E ブローカーセッション維持 (T2-2, docs/tag-server-design.md §6-5):
//     SLMP 接続の読み取りが banto-broker 経由であることと、rebuild を跨いでも
//     同じセッション（同じ broker タスク）が維持されることを検証する。
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_slmp_session_survives_a_rebuild_via_broker() {
    let app = test_app("e2e-slmp-broker-session").await;
    let sim = SlmpSimulator::start().await;
    sim.set_word(SlmpDevice::D, 100, 111);

    let conn = PlcConnectionService::new(app.pool.clone())
        .create(slmp_conn_input("line1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("t1", group.id, "D100", "u16"))
        .await
        .unwrap();

    app.manager.rebuild().await.expect("first rebuild");

    // The value is readable through REST - proof the read actually went
    // through the broker-backed `BrokerReadClient`
    // (`CollectorManager::rebuild`'s client factory routes every SLMP
    // connection through `banto_broker` - see hub.rs's doc comment).
    assert!(
        wait_until(Duration::from_secs(3), || async {
            let (status, body) =
                get_json(&app.router, "/api/v1/values/line1.fast.t1", &app.token).await;
            status == StatusCode::OK && body["v"] == 111.0
        })
        .await,
        "t1 should read 111 through the broker-backed SLMP session"
    );

    // Exactly one broker session exists for this connection.
    assert_eq!(app.sessions.connection_count(), 1);
    let mut status = app
        .sessions
        .status_watch(conn.id)
        .expect("a broker session should exist for this connection");
    status
        .wait_for(|s| *s == banto_broker::BrokerConnectionStatus::Connected)
        .await
        .expect("broker session should report Connected");

    // Simulate an unrelated registry edit (a second tag added to the SAME
    // connection's group) and rebuild again - the whole point of T2-2 is
    // that this must NOT reopen the SLMP session.
    sim.set_word(SlmpDevice::D, 102, 222);
    TagService::new(app.pool.clone())
        .create(tag_input("t2", group.id, "D102", "u16"))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("second rebuild");

    // Still exactly one broker session: `ensure_connection` reused the
    // existing task instead of spawning a second one for the same
    // connection id (`HubSessions`'s "Session sync policy" doc comment).
    assert_eq!(
        app.sessions.connection_count(),
        1,
        "the SLMP session must survive the rebuild, not be reopened"
    );

    // Value continuity through the surviving session: both the pre-existing
    // tag and the newly-added one read correctly after the rebuild.
    assert!(
        wait_until(Duration::from_secs(3), || async {
            let (s1, b1) = get_json(&app.router, "/api/v1/values/line1.fast.t1", &app.token).await;
            let (s2, b2) = get_json(&app.router, "/api/v1/values/line1.fast.t2", &app.token).await;
            s1 == StatusCode::OK && b1["v"] == 111.0 && s2 == StatusCode::OK && b2["v"] == 222.0
        })
        .await,
        "both tags should read correctly after the rebuild - the session never dropped"
    );

    // `/api/v1/status` sources an SLMP connection's status from the broker
    // (rest.rs's `v1_status` doc comment) and reports it connected
    // throughout, matching the session continuity proven above.
    let (status_code, status_json) = get_json(&app.router, "/api/v1/status", &app.token).await;
    assert_eq!(status_code, StatusCode::OK);
    let entry = status_json["connections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == conn.id)
        .expect("connection should appear in /api/v1/status");
    assert_eq!(entry["status"], "connected");

    sim.stop();
}

// ---------------------------------------------------------------------------
// 2. catalog: 外部名・address・安定ID・revision
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn catalog_exposes_external_name_address_and_stable_ids() {
    let app = test_app("catalog").await;
    let sim = Simulator::start().await;

    let conn = PlcConnectionService::new(app.pool.clone())
        .create(conn_input("line1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    let tag = TagService::new(app.pool.clone())
        .create(tag_input("temp01", group.id, "40001", "i16"))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild");

    let (status, json) = get_json(&app.router, "/api/v1/tags", &app.token).await;
    assert_eq!(status, StatusCode::OK);
    // `test_app()` already did one rebuild against the still-empty registry
    // (revision 1); seeding the fixture and rebuilding again here is a
    // second generation.
    assert_eq!(json["revision"], 2);
    let tags = json["tags"].as_array().unwrap();
    assert_eq!(tags.len(), 1);
    let entry = &tags[0];
    assert_eq!(entry["external_name"], "line1.fast.temp01");
    assert_eq!(entry["address"], "40001");
    assert_eq!(entry["ids"], serde_json::json!([conn.id, group.id, tag.id]));
    assert_eq!(entry["enabled"], true);
    // T2-3 (docs/tag-server-design.md §4/§6 item 1): catalog exposes
    // per-tag write opt-in - `tag_input()`'s fixture defaults to
    // `writable: false`, same pre-T2 behaviour as every other catalog field.
    assert_eq!(entry["writable"], false);

    sim.stop();
}

// ---------------------------------------------------------------------------
// 3. 未定義タグ 404 / 未収集(無効)タグは bad
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn undefined_tag_is_404_and_disabled_tag_reads_bad() {
    let app = test_app("undefined-disabled").await;
    let sim = Simulator::start().await;

    let conn = PlcConnectionService::new(app.pool.clone())
        .create(conn_input("line1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    let mut disabled = tag_input("offline", group.id, "40002", "i16");
    disabled.enabled = false;
    TagService::new(app.pool.clone())
        .create(disabled)
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild");

    let (status, _json) = get_json(&app.router, "/api/v1/values/nope.nope.nope", &app.token).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, json) =
        get_json(&app.router, "/api/v1/values/line1.fast.offline", &app.token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["q"], "bad");
    assert!(json["v"].is_null());

    sim.stop();
}

// ---------------------------------------------------------------------------
// 4. CRUD 経由の再構築: revision 増加、新タグが catalog に出る
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn creating_a_tag_via_rest_bumps_revision_and_appears_in_catalog() {
    let app = test_app("crud-rebuild").await;
    let sim = Simulator::start().await;

    let (status, conn_json) = write_json(
        &app.router,
        "POST",
        "/api/plc-connections",
        &app.token,
        serde_json::json!({ "name": "line1", "host": "127.0.0.1", "port": sim.addr.port() }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn_json:?}");
    let conn_id = conn_json["id"].as_i64().unwrap();

    let (status, before) = get_json(&app.router, "/api/v1/status", &app.token).await;
    assert_eq!(status, StatusCode::OK);
    let revision_after_connection = before["revision"].as_u64().unwrap();
    assert!(revision_after_connection >= 1);

    let (status, group_json) = write_json(
        &app.router,
        "POST",
        "/api/collection-groups",
        &app.token,
        serde_json::json!({ "name": "fast", "plcConnectionId": conn_id, "periodMs": 100 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{group_json:?}");
    let group_id = group_json["id"].as_i64().unwrap();

    let (status, tag_json) = write_json(
        &app.router,
        "POST",
        "/api/tags",
        &app.token,
        serde_json::json!({
            "name": "temp01",
            "collectionGroupId": group_id,
            "address": "40001",
            "dataType": "i16",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{tag_json:?}");

    let (status, after) = get_json(&app.router, "/api/v1/status", &app.token).await;
    assert_eq!(status, StatusCode::OK);
    let revision_after_tag = after["revision"].as_u64().unwrap();
    assert!(
        revision_after_tag > revision_after_connection,
        "creating the tag (which makes the group collectible) should bump revision again"
    );

    let (status, tags) = get_json(&app.router, "/api/v1/tags", &app.token).await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = tags["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["external_name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"line1.fast.temp01"));

    sim.stop();
}

// ---------------------------------------------------------------------------
// 4b. T2-3: writable フラグの REST 経由の作成・catalog 反映・既存ペイロード互換
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn creating_a_tag_with_writable_true_appears_writable_in_catalog() {
    let app = test_app("crud-writable").await;
    let sim = Simulator::start().await;

    let (_, conn_json) = write_json(
        &app.router,
        "POST",
        "/api/plc-connections",
        &app.token,
        serde_json::json!({ "name": "line1", "host": "127.0.0.1", "port": sim.addr.port() }),
    )
    .await;
    let conn_id = conn_json["id"].as_i64().unwrap();

    let (_, group_json) = write_json(
        &app.router,
        "POST",
        "/api/collection-groups",
        &app.token,
        serde_json::json!({ "name": "fast", "plcConnectionId": conn_id, "periodMs": 100 }),
    )
    .await;
    let group_id = group_json["id"].as_i64().unwrap();

    // Explicit `"writable": true` - the opt-in this endpoint's `TagPayload`
    // added in T2-3 (design §6 item 1/§10-2).
    let (status, tag_json) = write_json(
        &app.router,
        "POST",
        "/api/tags",
        &app.token,
        serde_json::json!({
            "name": "setpoint01",
            "collectionGroupId": group_id,
            "address": "40001",
            "dataType": "i16",
            "writable": true,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{tag_json:?}");
    assert_eq!(tag_json["writable"], true);

    let (status, tags) = get_json(&app.router, "/api/v1/tags", &app.token).await;
    assert_eq!(status, StatusCode::OK);
    let entry = tags["tags"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["external_name"] == "line1.fast.setpoint01")
        .expect("the writable tag should appear in the catalog");
    assert_eq!(
        entry["writable"], true,
        "catalog should surface the writable flag (design §4)"
    );

    sim.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn creating_a_tag_with_a_pre_t2_payload_still_works_and_defaults_writable_to_false() {
    let app = test_app("crud-legacy-payload").await;
    let sim = Simulator::start().await;

    let (_, conn_json) = write_json(
        &app.router,
        "POST",
        "/api/plc-connections",
        &app.token,
        serde_json::json!({ "name": "line1", "host": "127.0.0.1", "port": sim.addr.port() }),
    )
    .await;
    let conn_id = conn_json["id"].as_i64().unwrap();

    let (_, group_json) = write_json(
        &app.router,
        "POST",
        "/api/collection-groups",
        &app.token,
        serde_json::json!({ "name": "fast", "plcConnectionId": conn_id, "periodMs": 100 }),
    )
    .await;
    let group_id = group_json["id"].as_i64().unwrap();

    // Design §10-2: "既存の API クライアントのペイロードは無変更で通る" - no
    // `writable`/`tagKind`/`expression`/`retain` field at all, exactly what a
    // pre-T2-3 client still sends.
    let (status, tag_json) = write_json(
        &app.router,
        "POST",
        "/api/tags",
        &app.token,
        serde_json::json!({
            "name": "temp01",
            "collectionGroupId": group_id,
            "address": "40001",
            "dataType": "i16",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{tag_json:?}");
    assert_eq!(tag_json["writable"], false);
    assert_eq!(tag_json["tagKind"], "plc");

    sim.stop();
}

// ---------------------------------------------------------------------------
// 5. 不正構成で旧構成維持: last_error が /api/v1/status に出る + 旧タグは読める
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_invalid_config_keeps_the_old_collector_and_surfaces_last_config_error() {
    let app = test_app("invalid-config").await;
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 777); // 40001

    let conn = PlcConnectionService::new(app.pool.clone())
        .create(conn_input("line1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("good_tag", group.id, "40001", "i16"))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("first rebuild");

    assert!(
        wait_until(Duration::from_secs(3), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get("tag:1"))
                .map(|s| s.value)
                == Some(Some(777.0))
        })
        .await,
        "good_tag should read its value before the bad rebuild"
    );
    let (status, before) = get_json(&app.router, "/api/v1/status", &app.token).await;
    assert_eq!(status, StatusCode::OK);
    let revision_before = before["revision"].as_u64().unwrap();

    // "99999" has an unknown Modbus area prefix - passes banto-tags'
    // non-empty-only validation but fails at `build_config` time (same
    // fixture `banto-collect`'s own `invalid_address_is_a_config_error`
    // test uses).
    TagService::new(app.pool.clone())
        .create(tag_input("bad_tag", group.id, "99999", "i16"))
        .await
        .unwrap();
    let err = app
        .manager
        .rebuild()
        .await
        .expect_err("an unparsable address should fail rebuild");
    assert!(!err.is_empty());

    let (status, after) = get_json(&app.router, "/api/v1/status", &app.token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(after["revision"].as_u64().unwrap(), revision_before);
    assert!(!after["last_config_error"].is_null());

    // The old collector (with only good_tag) must still be running and
    // readable through REST.
    //
    // Bound-wait before the hard assert (H7 ⑤, same pattern as 2a96f20):
    // quality is derived at *read* time as period(100ms) x
    // STALE_PERIOD_FACTOR(2.5) = 250ms of grace (`banto_collect::current`),
    // not pushed by the collector. The old collector's own polling task was
    // never touched by the rejected rebuild above (it keeps running
    // unchanged), but on a busy CI runner its scheduling can still lag past
    // that 250ms grace window between the failed `rebuild()` call and this
    // read, making good_tag transiently look "stale" even though the
    // collector never actually stopped. We only care that good_tag is (or
    // promptly becomes) good/777 again, not the exact timing, so absorb the
    // scheduling jitter with a bound-wait immediately before the existing
    // hard assertions rather than weakening them.
    assert!(
        wait_until(Duration::from_secs(8), || async {
            let (s, v) = get_json(
                &app.router,
                "/api/v1/values/line1.fast.good_tag",
                &app.token,
            )
            .await;
            s == StatusCode::OK && v["v"] == 777.0 && v["q"] == "good"
        })
        .await,
        "good_tag should remain/become good after the rejected rebuild, even under scheduling jitter"
    );
    let (status, value) = get_json(
        &app.router,
        "/api/v1/values/line1.fast.good_tag",
        &app.token,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["v"], 777.0);
    assert_eq!(value["q"], "good");

    sim.stop();
}

// ---------------------------------------------------------------------------
// 6. settings の既定値
// ---------------------------------------------------------------------------

// `tests/common/mod.rs`'s module doc: `TempEnv::drop`'s retry needs a
// multi-thread runtime, so - unlike most standalone `#[tokio::test]`
// functions that don't touch a `TempEnv` - this one can't use the bare
// single-threaded default.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_defaults_are_port_8722_and_retention_7_days() {
    let env = TempEnv::new(TEMP_ENV_PREFIX, "settings-defaults");
    let pool = init_db(env.registry_path()).await.expect("init_db");
    let settings = SettingsService::new(pool);

    assert_eq!(DEFAULT_PORT, 8722);
    assert_eq!(DEFAULT_RETENTION_DAYS, 7);

    let server = settings.server_config().await.unwrap();
    assert_eq!(server.port, DEFAULT_PORT);
    assert_eq!(server.bind, "127.0.0.1");

    let store = settings.store_config().await.unwrap();
    assert_eq!(store.retention_days, DEFAULT_RETENTION_DAYS);
    assert_eq!(store.data_dir, "./data");
}
