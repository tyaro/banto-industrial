//! #341（オーナー決定 2026-09-09 / 2026-09-14、`docs/tag-server-design.md`
//! §4.3）の E2E テスト: **走行中の収集を止めずに**構成変更が実行構成へ
//! 反映されることを、実際に `CollectionController` を `Running`
//! （`RunMode::AllSimulation`）にした状態で確認する。
//!
//! ここが押さえるのは、単体テスト（`crate::rest` の
//! `*_while_running_and_commissioning_is_applied_immediately`、pending
//! change の適用）では踏めない次の3点:
//!
//! 1. 追加したタグが**収集を再起動せずに**実際に値を取り始める
//!    （`runId` が変わらないことで「止めて再開した」ではないことを示す）。
//! 2. 既存タグの値が巻き添えで Bad にならない（設計 §4.3 の影響半径 (c)。
//!    `Collector::apply_config` の部分適用が効いていることの証拠）。
//! 3. **run mode のオーバーライドが尊重される** - `AllSimulation` で
//!    稼働中の live re-apply が、レジストリの `simulation: false` を
//!    そのまま読んで実機（このテストでは誰も listen していないポート）へ
//!    ダイヤルし直してしまわない。`CollectorManager::rebuild` は run mode
//!    を見ないので、もし live re-apply の primitive にそれを選んでいたら
//!    このテストは両方のタグが Bad になって落ちる
//!    （`CollectionController::commit_catalog_and_apply_live` の doc
//!    comment「なぜ `commit_catalog` + `apply_run` で、`rebuild` では
//!    ないのか」）。
//!
//! 試運転中（即時反映）とロックダウン済み（pending queue + 明示適用）の
//! 両方で同じ結果になることを、同じ検証で2本並べて確認する。
//!
//! `tests/t9_simulation.rs`と同じ理由（各 `tests/*.rs`は独立したクレートと
//! してコンパイルされ、private helper を共有できない）で `fast_options`/
//! `wait_until`/`TestApp` 相当をこのファイル内に複製している
//! （`t9_simulation.rs`のものをベースにした）。

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
use banto_server::{AuthState, Identity};
use banto_tags::{CollectionGroupService, PlcConnectionService, TagService};
use banto_tstore::SystemClock;
use serde_json::{json, Value};
use sqlx::SqlitePool;
use tokio::sync::broadcast;
use tower::ServiceExt;

mod common;
use common::TempEnv;

const CLIENT_HEADER: (&str, &str) = ("X-Banto-Client", "banto");

/// Temp-dir prefix passed to `TempEnv::new` (see `tests/common/mod.rs`).
const TEMP_ENV_PREFIX: &str = "banto-hub-341-it";

/// 誰も listen していないポート。`AllSimulation` の上書きが効いている限り
/// ここへは一度もダイヤルされない - 効いていなければ全タグが Bad になる。
const UNREACHABLE_PORT: u16 = 15941;

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
    /// ロックダウン済みの挙動を見るテストが、router 構築後に
    /// `lock_down()` するためのハンドル（`CommissioningState` は
    /// `Arc<AtomicBool>`共有なので router 側にも即座に伝わる -
    /// `tests/mcp.rs`と同じ使い方）。
    commissioning: CommissioningService,
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

    let settings = SettingsService::new(pool.clone());
    // 既定は**試運転モードのまま**（`tests/mcp.rs`と同じ方針）- ロックダウン
    // 済みの挙動を見るテストだけが `app.commissioning.lock_down()` を呼ぶ。
    let commissioning = CommissioningService::load(settings, users.clone())
        .await
        .expect("CommissioningService::load");

    let router = api_router(
        users,
        audit,
        PlcConnectionService::new(pool.clone()),
        CollectionGroupService::new(pool.clone()),
        TagService::new(pool.clone()),
        api_keys,
        manager.clone(),
        auth,
        commissioning.clone(),
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
        commissioning,
        _env: env,
    }
}

async fn get_json(router: &Router, path: &str, bearer: &str) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            HttpRequest::get(path)
                .header("Authorization", format!("Bearer {bearer}"))
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

/// admin 管理系エンドポイント用（CSRF ヘッダ必須）。
async fn admin_write(
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
    let request = match &body {
        Some(body) => {
            builder = builder.header("content-type", "application/json");
            builder
                .body(Body::from(serde_json::to_vec(body).unwrap()))
                .unwrap()
        }
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

/// 接続・グループ・タグ t1 を **admin REST 経由で**作ってから、
/// `POST /api/collection/start-all-simulation` で全シミュレーション運転を
/// 開始し、t1 が good になるまで待つ。戻り値は `(group_id, run_id)`。
///
/// REST 経由で作るのは、`commit_catalog_and_notify`（= catalog のコミット）
/// を通す唯一の経路だから - サービス層を直接叩くと catalog が空のままで
/// `/api/v1/values/{tag}` が 404 になる（本番でも同じ: catalog を進めるのは
/// レジストリ mutation ハンドラと起動時の `commit_catalog` だけ）。
async fn seed_and_start_all_simulation(app: &TestApp) -> (i64, u64) {
    let (status, conn) = admin_write(
        &app.router,
        "POST",
        "/api/plc-connections",
        &app.admin_token,
        Some(json!({
            "name": "line1",
            "protocol": "slmp",
            "host": "127.0.0.1",
            "port": UNREACHABLE_PORT,
            "unitId": 1,
            "enabled": true,
            // わざと「実機」のまま - `AllSimulation` の非永続オーバーライド
            // だけが値の出所をシミュレータへ差し替える（そこが肝）。
            "simulation": false,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");
    let conn_id = conn["id"].as_i64().expect("connection id");

    let (status, group) = admin_write(
        &app.router,
        "POST",
        "/api/collection-groups",
        &app.admin_token,
        Some(json!({
            "name": "fast",
            "plcConnectionId": conn_id,
            "periodMs": 100,
            "enabled": true,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{group:?}");
    let group_id = group["id"].as_i64().expect("group id");

    // D5/D6 は hub 内蔵 SLMP シミュレータの RAMP_ADDRESS_COUNT(16) の範囲内
    // （`tests/t9_simulation.rs`と同じ根拠）。
    let (status, tag) = admin_write(
        &app.router,
        "POST",
        "/api/tags",
        &app.admin_token,
        Some(json!({
            "name": "t1",
            "collectionGroupId": group_id,
            "address": "D5",
            "dataType": "u16",
            "enabled": true,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{tag:?}");

    let (status, started) = admin_write(
        &app.router,
        "POST",
        "/api/collection/start-all-simulation",
        &app.admin_token,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{started:?}");
    assert_eq!(started["state"], "running", "{started:?}");
    assert_eq!(started["mode"], "all_simulation", "{started:?}");
    let run_id = started["runId"].as_u64().expect("runId");

    assert!(
        wait_until(Duration::from_secs(10), || async {
            let (status, json) =
                get_json(&app.router, "/api/v1/values/line1.fast.t1", &app.admin_token).await;
            status == StatusCode::OK && json["q"] == "good"
        })
        .await,
        "all-simulation で起動した既存タグが good になること（ここが Bad の         ままなら、そもそも AllSimulation の上書きが効いていない）"
    );

    (group_id, run_id)
}

/// 新タグ `t2` が**収集を止めずに**値を取り始め、既存タグ `t1` が巻き添えで
/// Bad にならず、run mode / `runId` も変わっていないことを確認する。
async fn assert_applied_live_without_stopping(app: &TestApp, run_id_before: u64) {
    // catalog（`/api/v1/tags`）に現れる。
    let (status, tags) = get_json(&app.router, "/api/v1/tags", &app.admin_token).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        tags["tags"]
            .as_array()
            .map(|rows| rows.iter().any(|t| t["external_name"] == "line1.fast.t2"))
            .unwrap_or(false),
        "追加したタグが catalog に現れること: {tags:?}"
    );

    // 値を取り始める = 走行中の `Collector` のタスク集合が更新された。
    assert!(
        wait_until(Duration::from_secs(10), || async {
            let (status, json) = get_json(
                &app.router,
                "/api/v1/values/line1.fast.t2",
                &app.admin_token,
            )
            .await;
            status == StatusCode::OK && json["q"] == "good"
        })
        .await,
        "収集を止めずに追加したタグが good な値を返すこと"
    );

    // 既存タグは巻き添えで Bad にならない（設計 §4.3 の影響半径）。
    let (status, t1) = get_json(
        &app.router,
        "/api/v1/values/line1.fast.t1",
        &app.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(t1["q"], "good", "既存タグが Bad になってはいけない: {t1:?}");

    // 収集は止まっておらず、run mode も変わっていない（= 実機へダイヤルし
    // 直していない）。`runId` が同じことが「止めて再開したのではない」証拠。
    let (status, runtime) = get_json(&app.router, "/api/status", &app.admin_token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(runtime["collectionState"], "running", "{runtime:?}");
    assert_eq!(runtime["collectionMode"], "all_simulation", "{runtime:?}");
    assert_eq!(
        runtime["runId"].as_u64(),
        Some(run_id_before),
        "live re-apply は収集を再起動しない（runId が変わらない）: {runtime:?}"
    );
    assert!(runtime["lastConfigError"].is_null(), "{runtime:?}");
    // 部分適用そのものの証拠: 触った接続だけが replaced になり、列が増えた
    // ぶん writer もローテーションされている。
    assert_eq!(
        runtime["lastApply"]["replaced"],
        json!(["conn:1"]),
        "{runtime:?}"
    );
    assert_eq!(runtime["lastApply"]["writerRotated"], true, "{runtime:?}");

    // #341 で発覚した `banto_collect::Collector::apply_config` の
    // writer 配布バグ（`watch::Sender::send` は受信者 0 のとき値を更新せず
    // 握り潰される → 新タスクが古い writer を掴み、列数不一致で履歴が一切
    // 書けなくなる。同 fn の step 4 のコメント参照）の回帰テスト。値が
    // good になった時点で既に append は走っているが、失敗の記録は非同期に
    // 出るので数周期ぶん待ってから確認する。
    tokio::time::sleep(Duration::from_millis(400)).await;
    let append_failures: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM collect_events WHERE kind = 'append_failure_entered'",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(
        append_failures, 0,
        "live re-apply の後も履歴書き込みが失敗していないこと"
    );
}

/// 試運転中（未ロックダウン）: 収集中の `POST /api/tags` が queue を経由せず
/// その場で走行中の収集へ反映される。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn commissioning_tag_added_while_running_is_collected_without_stopping() {
    let app = test_app("commissioning-live").await;
    let (group_id, run_id) = seed_and_start_all_simulation(&app).await;

    let (status, created) = admin_write(
        &app.router,
        "POST",
        "/api/tags",
        &app.admin_token,
        Some(json!({
            "name": "t2",
            "collectionGroupId": group_id,
            "address": "D6",
            "dataType": "u16",
            "enabled": true,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "試運転中は 202（queue）ではなく即時反映: {created:?}"
    );
    assert_eq!(created["name"], "t2", "{created:?}");

    let pending_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pending_changes")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(pending_count, 0, "試運転中は pending queue を経由しない");

    assert_applied_live_without_stopping(&app, run_id).await;
}

/// ロックダウン済み: 同じ `POST /api/tags` は pending queue へ積まれ、人が
/// 明示適用したときに**収集を止めずに**同じ結果へ到達する
/// （2026-09-14 オーナー回答「適用も無停止」）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn locked_down_pending_apply_while_running_reaches_the_same_state_without_stopping() {
    let app = test_app("locked-down-live").await;
    // 試運転中に配線を仕込んでから**ロックダウンする**（実運用の順序その
    // もの）。`CommissioningState` は `Arc<AtomicBool>` 共有なので、router
    // 構築後のロックダウンもそのまま効く。
    let (group_id, run_id) = seed_and_start_all_simulation(&app).await;
    app.commissioning.lock_down().await.expect("lock_down");

    let (status, queued) = admin_write(
        &app.router,
        "POST",
        "/api/tags",
        &app.admin_token,
        Some(json!({
            "name": "t2",
            "collectionGroupId": group_id,
            "address": "D6",
            "dataType": "u16",
            "enabled": true,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "ロックダウン済みは収集中なら queue: {queued:?}"
    );
    let pending_id = queued["pending"]["id"].as_i64().expect("pending id");

    // まだ反映されていない（明示適用のワンクッションが効いている）。
    let (status, not_yet) = get_json(
        &app.router,
        "/api/v1/values/line1.fast.t2",
        &app.admin_token,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "適用前のタグは catalog に存在しない: {not_yet:?}"
    );

    let (status, applied) = admin_write(
        &app.router,
        "POST",
        &format!("/api/pending-changes/{pending_id}/apply"),
        &app.admin_token,
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "明示適用は収集中でも通る（409 は撤廃）: {applied:?}"
    );
    assert_eq!(applied["state"], "applied", "{applied:?}");

    assert_applied_live_without_stopping(&app, run_id).await;
}
