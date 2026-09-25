//! #363 の E2E テスト（オーナー決定 2026-09-15、
//! `apps/banto-hub/core/src/write_path.rs` の「撤去: 旧ゲート4」節）:
//! シミュレーションデバイスのタグへの外部書き込みは拒否されず、PC 上の
//! in-process シミュレータへ反映され、読み戻せる。
//!
//! 旧ゲート4（`WriteRejection::SimulationWriteRejected`、REST 503
//! `simulation_write_rejected`）は撤去済みで、実機向けの護り
//! （per-tag `writable`・`write` スコープ・レート制限・値検査・
//! log-before-write・`write_enabled`）はそのまま適用される - このファイルが
//! 固定するのは「拒否されない」「シミュレータに実際に届く」「書いた番地は
//! ランプ波に上書きされず保持される（held）」「隣の番地はランプで動き
//! 続ける」「監査に `target` が残る」の5点。
//!
//! `tests/t9_simulation.rs`/`tests/t15_write_peek.rs` と同じ理由（各
//! `tests/*.rs` は独立クレートとしてコンパイルされ private helper を共有
//! できない）で `fast_options`/`wait_until`/`TestApp` 相当をこのファイル内に
//! 複製している。`TestApp` は `tests/t15_write_peek.rs` と同じ
//! `api_router_with_controller`（本番構成の入口）で組み立てる - 全
//! シミュレーション運転（`RunMode::AllSimulation`）を書き込み経路から
//! 観測するには、`tests/write.rs` のレガシー互換 `api_router`
//! （`collection_controller` ゲート未設定 = `collection_mode` が常に `None`）
//! では足りないため。`TempEnv` は `tests/common/mod.rs` に集約済み。
//!
//! テスト構成:
//! 1. 接続単位 `simulation: true` の SLMP 接続へ数値・ビットを書く
//!    （収集 Running 中、`write:<外部名>` スコープの API キー）。
//! 2. 同じことを Modbus 接続で。
//! 3. `simulation: false` の接続でも、全シミュレーション運転
//!    （`RunMode::AllSimulation`）中は同じく書けて読み戻せる。
//! 4. 実機接続（`simulation: false`・誰も listen していないポート）への
//!    書き込みは従来どおり失敗し、監査に `target: plc` が残る。

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
use banto_hub_core::controller::{CollectionController, CollectionState, RunMode};
use banto_hub_core::db::init_db;
use banto_hub_core::grpc::{GrpcServer, GrpcService};
use banto_hub_core::hub::CollectorManager;
use banto_hub_core::rest::api_router_with_controller;
use banto_hub_core::settings::SettingsService;
use banto_hub_core::users::UsersService;
use banto_hub_core::write_audit::WriteAuditService;
use banto_hub_core::write_control::WriteControl;
use banto_hub_core::write_rate::{WriteRateLimitConfig, WriteRateLimiter};
use banto_server::{AuthState, Identity};
use banto_tags::{
    CollectionGroupInput, CollectionGroupService, PlcConnectionInput, PlcConnectionService,
    TagInput, TagService,
};
use banto_tstore::SystemClock;
use serde_json::{json, Value};
use sqlx::SqlitePool;
use tokio::sync::broadcast;
use tokio::sync::Mutex as AsyncMutex;
use tower::ServiceExt;

mod common;
use common::TempEnv;

/// Temp-dir prefix passed to `TempEnv::new` (see `tests/common/mod.rs`).
const TEMP_ENV_PREFIX: &str = "banto-hub-363-it";

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

fn conn_input(name: &str, protocol: &str, port: u16, simulation: bool) -> PlcConnectionInput {
    PlcConnectionInput {
        name: name.to_string(),
        protocol: protocol.to_string(),
        host: "127.0.0.1".to_string(),
        port: port as i64,
        unit_id: 1,
        enabled: true,
        simulation,
        word_order: "low_high".to_string(),
        database: None,
        username: None,
        password: None,
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

fn tag_input(
    name: &str,
    group_id: i64,
    address: &str,
    data_type: &str,
    writable: bool,
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
        enabled: true,
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
    controller: Arc<CollectionController>,
    _env: TempEnv,
}

// See `tests/common/mod.rs`'s module doc ("Why `TestApp` also needs
// `shutdown_test_app`") for why this is required, not optional.
impl Drop for TestApp {
    fn drop(&mut self) {
        common::shutdown_test_app(&self.manager, &self.pool);
    }
}

/// Unlike `tests/t15_write_peek.rs`'s twin, this one does **not** start the
/// controller - each test starts it in the run mode it is about
/// (`Configured` or `AllSimulation`).
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
        sessions,
        sim_registry,
        computed,
    ));
    manager.rebuild().await.expect("initial rebuild");

    let (events_tx, _rx) = broadcast::channel(16);
    let write_control = Arc::new(WriteControl::new(false));
    let write_audit = WriteAuditService::new(pool.clone());
    let mqtt = Arc::new(banto_hub_core::mqtt::MqttPublisher::new(manager.clone()));
    let api_keys = ApiKeysService::new(pool.clone());
    let rate_limiter = Arc::new(AsyncMutex::new(WriteRateLimiter::new(
        WriteRateLimitConfig::default(),
    )));
    let controller = Arc::new(CollectionController::new(manager.clone()));

    let grpc_service = GrpcService::new(
        manager.clone(),
        api_keys.clone(),
        audit.clone(),
        write_audit.clone(),
        write_control.clone(),
        rate_limiter.clone(),
        events_tx.clone(),
    )
    .with_controller(controller.clone());
    let grpc_server = Arc::new(GrpcServer::new(grpc_service));

    let settings = SettingsService::new(pool.clone());
    let commissioning = CommissioningService::load(settings, users.clone())
        .await
        .expect("CommissioningService::load");
    commissioning
        .lock_down()
        .await
        .expect("lock_down the test environment");

    let router = api_router_with_controller(
        users,
        audit,
        PlcConnectionService::new(pool.clone()),
        CollectionGroupService::new(pool.clone()),
        TagService::new(pool.clone()),
        api_keys,
        manager.clone(),
        controller.clone(),
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
        controller,
        _env: env,
    }
}

async fn admin_post(router: &Router, path: &str, token: &str, body: Value) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            HttpRequest::post(path)
                .header("Authorization", format!("Bearer {token}"))
                .header("X-Banto-Client", "banto")
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

/// `/api/v1/*` 用（CSRF ヘッダ不要 - 設計 §5.1/§5.6）。
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

async fn issue_key(router: &Router, admin_token: &str, scopes: &[&str]) -> String {
    let (status, body) = admin_post(
        router,
        "/api/api-keys",
        admin_token,
        json!({ "name": "writer", "scopes": scopes }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body:?}");
    body["key"].as_str().unwrap().to_string()
}

/// `hub_write_audit` の最新行のうち `tag_id` に一致するものの `detail`。
async fn audit_detail(app: &TestApp, tag_id: i64) -> String {
    let (status, listed) = admin_post(
        &app.router,
        "/api/write-audit/list",
        &app.admin_token,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{listed:?}");
    let rows = listed["rows"].as_array().unwrap();
    let row = rows
        .iter()
        .find(|r| r["tagId"].as_i64() == Some(tag_id))
        .unwrap_or_else(|| panic!("an audit row for tag {tag_id} should exist: {listed:?}"));
    row["detail"]
        .as_str()
        .unwrap_or_else(|| panic!("the audit row should carry a detail: {row:?}"))
        .to_string()
}

async fn read_now(router: &Router, external_name: &str, key: &str) -> Value {
    let (status, body) = get_json(
        router,
        &format!("/api/v1/values/{external_name}/read-now"),
        key,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    body["v"].clone()
}

/// 収集キャッシュ（通常ポーリング）の現在値。
fn cached_value(app: &TestApp, tag_id: i64) -> Option<f64> {
    app.manager
        .current_values()
        .and_then(|c| c.get(&format!("tag:{tag_id}")))
        .and_then(|s| s.value)
}

/// ランプ周期（`banto_collect::simulation::RAMP_PERIOD_MS` = 100ms）の
/// 数倍。「書いた値が保持されている」ことを主張する前に、ランプ波タスクが
/// 何周も回ったことを保証するための待ち時間。
const SEVERAL_RAMP_PERIODS: Duration = Duration::from_millis(800);

// ---------------------------------------------------------------------------
// 1/2. 接続単位 simulation=true への書き込み（SLMP / Modbus）。
// ---------------------------------------------------------------------------

/// 共通本体: `protocol` のシミュレーション接続に「書く番地」「隣の番地」
/// 「ビット」の3タグを作り、数値とビットを書いて、(a) 200 ok、(b) 書いた
/// 値がランプ周期の数倍待っても保持される（held）、(c) 隣の番地はランプで
/// 動き続ける、(d) 監査に `target: simulator` が残る、を確認する。
async fn simulated_connection_write_is_applied_and_held(
    label: &str,
    protocol: &str,
    word_address: &str,
    neighbour_address: &str,
    bit_address: &str,
) {
    let app = test_app(label).await;

    // host/port はダミー - `BrokerSimRegistry` が実際のダイヤル先を in-process
    // シミュレータへ差し替える（port 1 は誰も listen していない特権ポート
    // なので、差し替えが壊れたら即座に接続拒否で落ちる）。
    let conn = PlcConnectionService::new(app.pool.clone())
        .create(conn_input("simline", protocol, 1, true))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    let tags = TagService::new(app.pool.clone());
    let word_tag = tags
        .create(tag_input("held", group.id, word_address, "u16", true))
        .await
        .unwrap();
    let neighbour_tag = tags
        .create(tag_input(
            "neighbour",
            group.id,
            neighbour_address,
            "u16",
            false,
        ))
        .await
        .unwrap();
    tags.create(tag_input("flag", group.id, bit_address, "bit", true))
        .await
        .unwrap();

    app.manager.rebuild().await.expect("rebuild after seeding");
    let status = app.controller.start(RunMode::Configured).await;
    assert_eq!(status.state, CollectionState::Running, "{status:?}");
    app.write_control.enable();

    let key = issue_key(
        &app.router,
        &app.admin_token,
        &[
            "write:simline.fast.held",
            "write:simline.fast.flag",
            "read:simline.fast.held",
            "read:simline.fast.flag",
            "read:simline.fast.neighbour",
        ],
    )
    .await;

    // ランプ波が回り始めるまで待つ（値が読めない状態で書いても意味が無い）。
    assert!(
        wait_until(Duration::from_secs(10), || async {
            cached_value(&app, neighbour_tag.id).is_some()
        })
        .await,
        "the in-process simulator should start serving values"
    );

    let (status, body) = v1_post(
        &app.router,
        "/api/v1/values/simline.fast.held",
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

    let (status, body) = v1_post(
        &app.router,
        "/api/v1/values/simline.fast.flag",
        &key,
        json!({ "v": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");

    // 収集が書いた値を読み戻すまで待つ（ここで初めて「シミュレータに実際に
    // 届いた」ことが証明される）。
    assert!(
        wait_until(Duration::from_secs(10), || async {
            cached_value(&app, word_tag.id) == Some(4242.0)
        })
        .await,
        "collection should read back the value the write endpoint just wrote"
    );

    // ランプ周期の数倍待っても保持されている = held が効いている。
    let before = cached_value(&app, neighbour_tag.id).expect("neighbour has a value");
    tokio::time::sleep(SEVERAL_RAMP_PERIODS).await;
    assert_eq!(
        cached_value(&app, word_tag.id),
        Some(4242.0),
        "the written address must stay held against the 100ms ramp task"
    );
    assert_eq!(
        read_now(&app.router, "simline.fast.held", &key).await,
        json!(4242.0),
        "read-now must see the held value too"
    );
    assert_eq!(
        read_now(&app.router, "simline.fast.flag", &key).await,
        json!(true),
        "a written bit is held the same way"
    );

    // 隣の番地は held ではないので、ランプ波で動き続けている。
    assert!(
        wait_until(Duration::from_secs(10), || async {
            cached_value(&app, neighbour_tag.id) != Some(before)
        })
        .await,
        "the neighbouring address must keep ramping - holding is per-address"
    );

    assert!(
        audit_detail(&app, word_tag.id)
            .await
            .contains(r#""target":"simulator""#),
        "the log-before-write audit row must record that this write went to the simulator"
    );

    app.controller.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn simulated_slmp_connection_write_is_applied_and_held_against_the_ramp() {
    // D5/D6/M3 はいずれも `banto_collect::simulation::RAMP_ADDRESS_COUNT`
    // (16) の範囲内 - ランプ波が毎周期上書きしにくる番地をわざと選んでいる
    // （held が効いていなければこのテストは落ちる）。
    simulated_connection_write_is_applied_and_held("363-slmp", "slmp", "D5", "D6", "M3").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn simulated_modbus_connection_write_is_applied_and_held_against_the_ramp() {
    // 40006/40007 -> holding register offset 5/6、00004 -> coil offset 3。
    // いずれもランプ波のウィンドウ内（上の SLMP 版と同じ意図）。
    simulated_connection_write_is_applied_and_held(
        "363-modbus",
        "modbus-tcp",
        "40006",
        "40007",
        "00004",
    )
    .await;
}

// ---------------------------------------------------------------------------
// 3. 全シミュレーション運転中は simulation=false の接続でも書ける。
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn all_simulation_run_applies_writes_to_the_simulator_too() {
    let app = test_app("363-all-sim").await;

    // `simulation: false` の「実機」接続。全シミュレーション運転中は
    // `BrokerSimRegistry` がこの接続も in-process シミュレータへ差し替える
    // ので、port 1（誰も listen していない）でも Connected になる。
    let conn = PlcConnectionService::new(app.pool.clone())
        .create(conn_input("realline", "slmp", 1, false))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    let tag = TagService::new(app.pool.clone())
        .create(tag_input("held", group.id, "D7", "u16", true))
        .await
        .unwrap();

    app.manager.rebuild().await.expect("rebuild after seeding");
    let status = app.controller.start(RunMode::AllSimulation).await;
    assert_eq!(status.state, CollectionState::Running, "{status:?}");
    assert_eq!(status.mode, RunMode::AllSimulation);
    app.write_control.enable();

    let key = issue_key(
        &app.router,
        &app.admin_token,
        &["write:realline.fast.held", "read:realline.fast.held"],
    )
    .await;

    assert!(
        wait_until(Duration::from_secs(10), || async {
            cached_value(&app, tag.id).is_some()
        })
        .await,
        "all-simulation should start serving values for this connection"
    );

    let (status, body) = v1_post(
        &app.router,
        "/api/v1/values/realline.fast.held",
        &key,
        json!({ "v": 1234 }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "#363: all-simulation writes are applied, not rejected: {body:?}"
    );

    assert!(
        wait_until(Duration::from_secs(10), || async {
            cached_value(&app, tag.id) == Some(1234.0)
        })
        .await,
        "collection should read the written value back off the all-simulation simulator"
    );
    tokio::time::sleep(SEVERAL_RAMP_PERIODS).await;
    assert_eq!(
        read_now(&app.router, "realline.fast.held", &key).await,
        json!(1234.0),
        "the written address must stay held under all-simulation too"
    );

    assert!(
        audit_detail(&app, tag.id)
            .await
            .contains(r#""target":"simulator""#),
        "a write during all-simulation is audited as going to the simulator, even though the \
         connection itself is not marked simulation"
    );

    app.controller.stop().await;
}

// ---------------------------------------------------------------------------
// 4. 実機接続への書き込みは従来どおり失敗し、監査は target: plc。
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_real_connection_write_still_fails_and_is_audited_as_plc() {
    let app = test_app("363-real").await;

    // 誰も listen していないポート（bind してすぐ drop する定石）。
    let closed_port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };

    let conn = PlcConnectionService::new(app.pool.clone())
        .create(conn_input("realline", "slmp", closed_port, false))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    let tag = TagService::new(app.pool.clone())
        .create(tag_input("temp01", group.id, "D100", "u16", true))
        .await
        .unwrap();

    app.manager.rebuild().await.expect("rebuild after seeding");
    let status = app.controller.start(RunMode::Configured).await;
    assert_eq!(status.state, CollectionState::Running, "{status:?}");
    app.write_control.enable();

    let key = issue_key(
        &app.router,
        &app.admin_token,
        &["write:realline.fast.temp01"],
    )
    .await;
    let (status, body) = v1_post(
        &app.router,
        "/api/v1/values/realline.fast.temp01",
        &key,
        json!({ "v": 7 }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body:?}");
    assert_eq!(body["error"], "write_failed");

    let detail = audit_detail(&app, tag.id).await;
    assert!(
        detail.contains(r#""target":"plc""#),
        "a write aimed at a real device is audited as such: {detail}"
    );
    assert!(
        detail.contains(r#""detail""#),
        "the failure reason must survive alongside the target: {detail}"
    );

    app.controller.stop().await;
}
