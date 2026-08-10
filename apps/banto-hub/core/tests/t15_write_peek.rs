//! T15-4 の統合テスト（`crate::broker_glue::HubSessions::write_handle_for`・
//! `crate::hub::CollectorManager::write_broker_handle_peek`・
//! `crate::write_path::write_plc_tag`）: `execute_write` が gate 8 で
//! **新規に broker セッションをダイヤルしないこと** の E2E 実証。
//!
//! ## 再現するレース
//!
//! `execute_write`(`crate::write_path`)の gate 1〜7 は `.await` を挟む
//! (レジストリの再読み込み・レート制限の peek/record・監査行 insert)。その
//! 間に別のタスクが `CollectionController::stop()` を完了させて broker
//! セッションを `stop_and_join` してしまうと、外側の `CollectionNotRunning`
//! ゲート(`execute_write`呼び出し**前**の一点で `CollectionState` を見る
//! だけ)はこれを検知できない。もし gate 8 が(T15-4 以前のように)
//! `ensure_connection` を使っていれば、セッションが無いと分かった瞬間に
//! **実機へ新しい TCP セッションをダイヤルしてしまう** - 収集停止のつもりが
//! 実機へ書き込み用の接続を新規に張ってしまう、というのがこの T15-4 が
//! 塞ぐ穴。
//!
//! このテストは `CollectionController` の状態を意図的に `Running` の
//! ままにして(＝外側ゲートは通過させて)、直接
//! `sessions.stop_and_join(conn.id)` を呼ぶことで、外側ゲートでは
//! 検知できないこのレースそのものを再現する。
//!
//! `tests/write.rs`/`tests/grpc.rs`/`tests/t15_simulation_coverage.rs` と
//! 同じ理由（各 `tests/*.rs` は独立クレートとしてコンパイルされ、private
//! helper を共有できない）で `fast_options`/`wait_until`/`issue_key`/
//! `make_tag` をこのファイル内に複製している。`TestApp` は`tests/grpc.rs`/
//! `tests/stream.rs`と同じく`api_router_with_controller`(実`CollectionController`
//! を使う本番構成の入口)で組み立てる - `tests/write.rs`の`api_router`
//! (レガシー互換、`collection_controller`ゲート未設定)ではこのレースの
//! 前提(外側ゲートが`Running`を返し続ける)を再現できない。

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
use banto_hub_core::controller::{CollectionController, CollectionState, RunMode};
use banto_hub_core::db::init_db;
use banto_hub_core::grpc::{GrpcServer, GrpcService};
use banto_hub_core::hub::CollectorManager;
use banto_hub_core::rest::api_router_with_controller;
use banto_hub_core::test_output::TestOutputControl;
use banto_hub_core::users::UsersService;
use banto_hub_core::write_audit::WriteAuditService;
use banto_hub_core::write_control::WriteControl;
use banto_hub_core::write_rate::{WriteRateLimitConfig, WriteRateLimiter};
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
use tokio::sync::Mutex as AsyncMutex;
use tower::ServiceExt;

mod common;
use common::TempEnv;

/// Temp-dir prefix passed to `TempEnv::new` (see `tests/common/mod.rs`).
const TEMP_ENV_PREFIX: &str = "banto-hub-t15-4-it";

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
    controller: Arc<CollectionController>,
    /// T15-4: `manager`の内部が保持しているのと**同じ** `Arc` -
    /// テストが直接 `stop_and_join` を呼んでレースを模すために必要
    /// (`CollectorManager::sessions`はこのクレート外からは見えない
    /// private フィールドなので、構築時に渡した Arc をここに残しておく)。
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
    let rate_limiter = Arc::new(AsyncMutex::new(WriteRateLimiter::new(
        WriteRateLimitConfig::default(),
    )));
    let test_output = Arc::new(TestOutputControl::new());
    let controller = Arc::new(CollectionController::new(
        manager.clone(),
        write_control.clone(),
        test_output.clone(),
    ));
    // 収集を Running にしておく - このテストの核心は「外側の
    // CollectionNotRunning ゲートは Running のままレースを検知できない」
    // ことの実演なので、後段のテストは `controller.stop()` を呼ばない。
    let status = controller.start(RunMode::Configured).await;
    assert_eq!(status.state, CollectionState::Running);

    let grpc_service = GrpcService::new(
        manager.clone(),
        api_keys.clone(),
        audit.clone(),
        write_audit.clone(),
        write_control.clone(),
        rate_limiter.clone(),
        events_tx.clone(),
    )
    .with_controller(controller.clone())
    .with_test_output(test_output.clone());
    let grpc_server = Arc::new(GrpcServer::new(grpc_service));

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
        events_tx,
        false,
        write_control.clone(),
        write_audit,
        mqtt,
        grpc_server,
        rate_limiter,
        test_output,
    );

    TestApp {
        router,
        admin_token,
        pool,
        manager,
        write_control,
        controller,
        sessions,
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

/// `POST /api/api-keys` 経由でキーを発行し、平文キー全体(`bh_...`)を返す。
async fn issue_key(router: &Router, admin_token: &str, name: &str, scopes: &[&str]) -> String {
    let (status, body) = admin_post(
        router,
        "/api/api-keys",
        admin_token,
        json!({ "name": name, "scopes": scopes }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body:?}");
    body["key"].as_str().unwrap().to_string()
}

/// タグ・グループ・接続を1本作って rebuild まで済ませ、`(connection_id,
/// external_name)` を返す共通フィクスチャ。
async fn make_writable_tag(app: &TestApp, port: u16) -> (i64, String) {
    let conn = PlcConnectionService::new(app.pool.clone())
        .create(slmp_conn_input("line1", port))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("temp01", group.id, "D100", "u16", true, true))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild");
    (conn.id, "line1.fast.temp01".to_string())
}

/// T15-4 の核心シナリオ:
/// 1. 収集稼働中(controller = Running)、broker セッション確立
///    (`connection_count() == 1`)。
/// 2. `controller.stop()`は呼ばず、直接 `sessions.stop_and_join` で
///    セッションだけを落とす - `execute_write`の`.await`の間に
///    `stop_and_join`が先に終わってしまうレースを模す。外側の
///    `CollectionNotRunning`ゲートは `controller` が依然 `Running` を
///    報告するので素通りする。
/// 3. その状態で書き込むと、セッションが既に無い(peek が `None`) -
///    T15-4 前は`ensure_connection`が実機へ新規にダイヤルしていたところ、
///    peek は新規ダイヤルせず`write_failed`(502)で fail closed する。
/// 4. `connection_count() == 0`のまま(=新しいセッションは張られていない)
///    ことを直接確認する - これがこのテストの一番重要な assertion。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_after_session_stop_and_join_fails_closed_without_spawning_a_new_session() {
    let app = test_app("race-fails-closed").await;
    let sim = Simulator::start().await;
    let (conn_id, external_name) = make_writable_tag(&app, sim.addr.port()).await;
    app.write_control.enable();

    assert!(
        wait_until(Duration::from_secs(3), || async {
            app.sessions.connection_count() == 1
        })
        .await,
        "rebuild should have established exactly one broker session"
    );
    assert!(
        wait_until(Duration::from_secs(3), || async {
            app.manager
                .broker_status(conn_id)
                .map(|s| s == banto_broker::BrokerConnectionStatus::Connected)
                .unwrap_or(false)
        })
        .await,
        "the broker session should reach Connected before we race it"
    );

    let key = issue_key(
        &app.router,
        &app.admin_token,
        "writer",
        &["write:line1.fast.temp01"],
    )
    .await;

    // このテストの核心: controller には触れず(Running のまま)、broker
    // セッションだけを直接落とす。
    assert!(app.sessions.stop_and_join(conn_id).await);
    assert_eq!(
        app.sessions.connection_count(),
        0,
        "stop_and_join should have untracked the session"
    );
    assert_eq!(
        app.controller.status().state,
        CollectionState::Running,
        "the outer CollectionNotRunning gate must still see Running - that's the race"
    );

    let (status, body) = v1_post(
        &app.router,
        &format!("/api/v1/values/{external_name}"),
        &key,
        json!({ "v": 999 }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body:?}");
    assert_eq!(body["error"], "write_failed");

    // 一番重要な assertion: peek は新しいセッションを一切張らない。
    assert_eq!(
        app.sessions.connection_count(),
        0,
        "a failed-closed write must NOT have dialed a fresh session to the real PLC"
    );

    sim.stop();
}

/// 対照 (happy path): セッションが生きている間は、peek 経由でも書き込みは
/// 従来どおり成功する - T15-4 は「セッションが無いときに新規ダイヤルしない」
/// だけを変えており、通常経路を壊していないことの確認。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_succeeds_via_peek_while_the_session_is_alive() {
    let app = test_app("peek-happy-path").await;
    let sim = Simulator::start().await;
    let (conn_id, external_name) = make_writable_tag(&app, sim.addr.port()).await;
    app.write_control.enable();

    assert!(
        wait_until(Duration::from_secs(3), || async {
            app.manager
                .broker_status(conn_id)
                .map(|s| s == banto_broker::BrokerConnectionStatus::Connected)
                .unwrap_or(false)
        })
        .await,
        "the broker session should reach Connected"
    );

    let key = issue_key(
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
    assert_eq!(body["result"], "ok");
    assert_eq!(
        app.sessions.connection_count(),
        1,
        "the write should reuse the existing session, not spawn another"
    );

    sim.stop();
}
