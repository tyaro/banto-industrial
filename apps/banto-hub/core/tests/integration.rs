//! T0-1 の統合テスト: 実際の axum `Router`（`tower::ServiceExt::oneshot`
//! 経由）+ Modbus TCP シミュレータ（`banto_plc::modbus::simulator`）を使った
//! E2E。足場（`TempEnv`/`fast_options`/`wait_until`）は
//! `crates/banto-collect/tests/integration.rs` を流用している。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request as HttpRequest, StatusCode};
use axum::Router;
use banto_collect::{BackoffConfig, CollectorOptions};
use banto_hub_core::audit::AuditLogService;
use banto_hub_core::db::init_db;
use banto_hub_core::hub::CollectorManager;
use banto_hub_core::rest::api_router;
use banto_hub_core::settings::{SettingsService, DEFAULT_PORT, DEFAULT_RETENTION_DAYS};
use banto_hub_core::users::UsersService;
use banto_plc::modbus::simulator::Simulator;
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

const CLIENT_HEADER: (&str, &str) = ("X-Banto-Client", "banto");

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A temp directory holding the registry DB and the tstore data dir - the
/// registry must be *file-backed* (not `:memory:`): `CollectorManager`
/// hands out several pool connections concurrently (registry reads, event
/// persistence, per-connection tasks), and each `:memory:` connection is a
/// separate empty database (`crates/banto-collect/tests/integration.rs`'s
/// module doc explains the same constraint).
struct TempEnv {
    root: PathBuf,
}

impl TempEnv {
    fn new(label: &str) -> Self {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let root =
            std::env::temp_dir().join(format!("banto-hub-it-{}-{label}-{id}", std::process::id()));
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

    let manager = Arc::new(CollectorManager::new(
        pool.clone(),
        env.data_dir(),
        Arc::new(SystemClock),
        fast_options(),
    ));
    manager.rebuild().await.expect("initial rebuild");

    let (events_tx, _rx) = broadcast::channel(16);
    let router = api_router(
        users,
        audit,
        PlcConnectionService::new(pool.clone()),
        CollectionGroupService::new(pool.clone()),
        TagService::new(pool.clone()),
        manager.clone(),
        auth,
        events_tx,
        false,
    );

    TestApp {
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

#[tokio::test]
async fn settings_defaults_are_port_8722_and_retention_7_days() {
    let env = TempEnv::new("settings-defaults");
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
