//! T9-2 の E2E テスト（docs/ux-plan.md §1「接続単位のシミュレーションモード」、
//! `apps/banto-hub/core/src/broker_glue.rs`の`SlmpSimRegistry`と
//! `apps/banto-hub/core/src/hub.rs`の`CollectorManager::sync_slmp_sessions`の
//! 配線を broker 経由 SLMP 接続で確認する）。
//!
//! `tests/integration.rs`/`tests/t8_bit_access.rs`と同じ理由（各
//! `tests/*.rs`は独立したクレートとしてコンパイルされ、private helper を
//! 共有できない）で`fast_options`/`wait_until`/`TestApp`相当をこのファイル
//! 内に複製している（`t8_bit_access.rs`のものをベースにした）。`TempEnv`は
//! `tests/common/mod.rs`に集約済み（2026-08-08、テスト一時ディレクトリ
//! リークの根治）。
//!
//! テスト構成:
//! 1. `POST /api/plc-connections`（`simulation: true`）で broker 経由 SLMP
//!    接続を作り、グループ・タグを足して rebuild すると、このテスト自身は
//!    SLMP シミュレータを一切起動していないのに `/api/v1/values/{tag}` が
//!    changing な good 品質の値を返す - `SlmpSimRegistry`が
//!    `ensure_connection`より前にシミュレータを起動・アドレス差し替えして
//!    いることの証明。`GET /api/v1/status`/`GET /api/v1/tags`が
//!    `simulation: true`/`false`を正しく報告することも合わせて確認する。
//! 2. 実際の SLMP シミュレータ(`banto_plc::slmp::simulator::Simulator`、
//!    テスト自身が起動)を使う`simulation: false`接続から始め、
//!    `PUT /api/plc-connections/{id}`で`simulation: true`へ切り替えると、
//!    値の出所が(テスト所有の外部シミュレータから)hub 内蔵シミュレータの
//!    ランプ波へ実際に切り替わることを確認する - `SlmpSimRegistry::resolve`
//!    の`changed`検出 +`HubSessions::remove`の組み合わせが正しく機能して
//!    いないと、古いセッションを読み続けて値が変わらないままになる。
//!    `app.sessions.connection_count()`が終始 1 のままであることも確認する
//!    （孤立した二重セッションが増えないこと）。続けて、無関係なタグ追加
//!    （同じグループへの2本目のタグ）で rebuild しても
//!    `connection_count()`が変わらないこと（`SlmpSimRegistry::resolve`の
//!    `changed`判定がダイヤル先が実際には変わっていない限り安定して
//!    `false`であること）も確認する - `tests/integration.rs`の
//!    `e2e_slmp_session_survives_a_rebuild_via_broker`と同型の回帰確認。

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
use banto_plc::slmp::simulator::Simulator as SlmpSimulator;
use banto_server::{AuthState, Identity};
use banto_tags::{
    CollectionGroupInput, CollectionGroupService, PlcConnectionService, TagInput, TagService,
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
const TEMP_ENV_PREFIX: &str = "banto-hub-t9-it";

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
    /// T2-2/T9-2: exposed so a test can assert broker session identity
    /// directly (`connection_count`), the way
    /// `tests/integration.rs::e2e_slmp_session_survives_a_rebuild_via_broker`
    /// does - here to prove a simulation toggle re-points the existing
    /// session rather than leaking a second one.
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
    let admin_token = auth
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
        TagService::new(pool.clone()),
        api_keys,
        manager.clone(),
        auth,
        events_tx,
        false,
        write_control.clone(),
        write_audit,
        mqtt,
        grpc_server,
        rate_limiter,
    );

    TestApp {
        router,
        admin_token,
        pool,
        manager,
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
async fn admin_write(
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

/// `PlcConnectionPayload`（`rest.rs`）の JSON 表現 - camelCase。
fn plc_connection_payload_json(name: &str, port: u16, simulation: bool) -> Value {
    json!({
        "name": name,
        "protocol": "slmp",
        "host": "127.0.0.1",
        "port": port,
        "unitId": 1,
        "enabled": true,
        "simulation": simulation,
    })
}

// ---------------------------------------------------------------------------
// 1. broker 経由 SLMP 接続の simulation=true が実際に効くこと(E2E)。
//    /api/v1/status と /api/v1/tags の simulation フィールドも合わせて確認。
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn broker_routed_slmp_connection_serves_synthetic_ramp_values() {
    let app = test_app("t9-broker-sim").await;

    // POST /api/plc-connections で simulation: true の SLMP 接続を作る - この
    // テスト自身は SLMP シミュレータを一切起動していない(host/port はダミー、
    // どうせ SlmpSimRegistry が実際のダイヤル先を差し替える)。
    let (status, created) = admin_write(
        &app.router,
        "POST",
        "/api/plc-connections",
        &app.admin_token,
        plc_connection_payload_json("simline", 1, true),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created:?}");
    let conn_id = created["id"].as_i64().expect("connection id");

    // 比較対象として simulation: false の通常接続も1本作る(status/catalog の
    // フィールドが両方向とも正しく出ることを見るため)。
    let (status, real_created) = admin_write(
        &app.router,
        "POST",
        "/api/plc-connections",
        &app.admin_token,
        plc_connection_payload_json("realline", 1, false),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{real_created:?}");
    let real_conn_id = real_created["id"].as_i64().expect("real connection id");

    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn_id, 100))
        .await
        .unwrap();
    // D5 は hub 内蔵シミュレータの RAMP_ADDRESS_COUNT(16) の範囲内。
    TagService::new(app.pool.clone())
        .create(tag_input("t1", group.id, "D5", "u16"))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild after seeding");

    // good 品質の値が返るまで待つ - SlmpSimRegistry がシミュレータを起動し、
    // ensure_connection がそのアドレスへ接続していないと、これは Bad のまま
    // タイムアウトする。
    assert!(
        wait_until(Duration::from_secs(3), || async {
            let (status, json) = get_json(
                &app.router,
                "/api/v1/values/simline.fast.t1",
                &app.admin_token,
            )
            .await;
            status == StatusCode::OK && json["q"] == "good"
        })
        .await,
        "the broker-routed simulated connection should serve a good-quality value"
    );

    // ランプ波であることの確認: 一定時間おいてもう一度読み、値が変わって
    // いること(固定値の代用品ではなく、実際に動いているシミュレータである
    // ことの証拠)。
    let (status, first) = get_json(
        &app.router,
        "/api/v1/values/simline.fast.t1",
        &app.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (status, second) = get_json(
        &app.router,
        "/api/v1/values/simline.fast.t1",
        &app.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_ne!(
        first["v"], second["v"],
        "the simulated value should keep changing (ramp wave), not sit fixed"
    );

    // GET /api/v1/status: simulation フィールドが両接続で正しい。
    let (status, status_json) = get_json(&app.router, "/api/v1/status", &app.admin_token).await;
    assert_eq!(status, StatusCode::OK);
    let connections = status_json["connections"].as_array().unwrap();
    let sim_entry = connections
        .iter()
        .find(|c| c["id"] == conn_id)
        .expect("simulated connection should be in /api/v1/status");
    assert_eq!(sim_entry["simulation"], true);
    let real_entry = connections
        .iter()
        .find(|c| c["id"] == real_conn_id)
        .expect("real connection should be in /api/v1/status");
    assert_eq!(real_entry["simulation"], false);

    // GET /api/v1/tags: カタログ側の simulation フィールドも接続を反映する。
    let (status, tags_json) = get_json(&app.router, "/api/v1/tags", &app.admin_token).await;
    assert_eq!(status, StatusCode::OK);
    let tag_entry = tags_json["tags"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["external_name"] == "simline.fast.t1")
        .expect("the simulated connection's tag should be in the catalog");
    assert_eq!(tag_entry["simulation"], true);
}

// ---------------------------------------------------------------------------
// 2. simulation: false -> true の切り替えで、broker セッションの実際の
//    ダイヤル先が(外部シミュレータから)hub 内蔵シミュレータへ実際に切り替わる
//    こと。無関係なタグ追加ではセッションが再生成されないことも確認する。
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn toggling_simulation_repoints_the_broker_session_without_leaking_sessions() {
    let app = test_app("t9-toggle").await;
    let sim = SlmpSimulator::start().await;
    sim.set_word(banto_plc::SlmpDevice::D, 5, 111);

    let (status, created) = admin_write(
        &app.router,
        "POST",
        "/api/plc-connections",
        &app.admin_token,
        plc_connection_payload_json("line1", sim.addr.port(), false),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created:?}");
    let conn_id = created["id"].as_i64().expect("connection id");

    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn_id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("t1", group.id, "D5", "u16"))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild after seeding");

    // Phase 1: simulation=false - values come from the test-owned external
    // simulator.
    assert!(
        wait_until(Duration::from_secs(3), || async {
            let (status, json) = get_json(
                &app.router,
                "/api/v1/values/line1.fast.t1",
                &app.admin_token,
            )
            .await;
            status == StatusCode::OK && json["v"] == 111.0 && json["q"] == "good"
        })
        .await,
        "should read 111 from the external simulator before toggling simulation on"
    );
    assert_eq!(app.sessions.connection_count(), 1);

    // Phase 2: PUT simulation=true (same nominal host/port - SlmpSimRegistry
    // substitutes the actual dial target). The external simulator's value
    // never changes from here on, so if the read stayed on the old session
    // it would keep reading a frozen 111.
    let (status, _updated) = admin_write(
        &app.router,
        "PUT",
        &format!("/api/plc-connections/{conn_id}"),
        &app.admin_token,
        plc_connection_payload_json("line1", sim.addr.port(), true),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    app.manager.rebuild().await.expect("rebuild after toggle");

    assert!(
        wait_until(Duration::from_secs(3), || async {
            let (status, json) = get_json(
                &app.router,
                "/api/v1/values/line1.fast.t1",
                &app.admin_token,
            )
            .await;
            status == StatusCode::OK && json["q"] == "good" && json["v"] != 111.0
        })
        .await,
        "after toggling simulation on, the value must stop tracking the external simulator's \
         frozen 111 and instead come from the hub's own simulator"
    );

    // Exactly one broker session throughout - the toggle re-pointed the
    // existing session (via SlmpSimRegistry::resolve's `changed` detection +
    // HubSessions::remove), it did not leak a second one.
    assert_eq!(
        app.sessions.connection_count(),
        1,
        "toggling simulation must not leak a duplicate broker session"
    );

    // Confirm the new source is actually live (ramp), not a one-off
    // different-but-frozen value.
    let (status, first) = get_json(
        &app.router,
        "/api/v1/values/line1.fast.t1",
        &app.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (status, second) = get_json(
        &app.router,
        "/api/v1/values/line1.fast.t1",
        &app.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_ne!(
        first["v"], second["v"],
        "the hub's own simulator should keep ramping"
    );

    // Regression: an unrelated edit (second tag on the SAME group) must not
    // re-point/reopen the session again - `SlmpSimRegistry::resolve`'s
    // `changed` detection must stay stable (false) when the dial target has
    // not actually changed, mirroring
    // `tests/integration.rs::e2e_slmp_session_survives_a_rebuild_via_broker`.
    TagService::new(app.pool.clone())
        .create(tag_input("t2", group.id, "D6", "u16"))
        .await
        .unwrap();
    app.manager
        .rebuild()
        .await
        .expect("rebuild after unrelated edit");

    assert_eq!(
        app.sessions.connection_count(),
        1,
        "an unrelated tag addition must not reopen the simulated broker session"
    );
    assert!(
        wait_until(Duration::from_secs(3), || async {
            let (status, json) = get_json(
                &app.router,
                "/api/v1/values/line1.fast.t2",
                &app.admin_token,
            )
            .await;
            status == StatusCode::OK && json["q"] == "good"
        })
        .await,
        "the newly-added tag should also read from the still-simulated connection"
    );

    sim.stop();
}
