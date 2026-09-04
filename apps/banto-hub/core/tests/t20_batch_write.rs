//! T20-3a の統合テスト（docs/banto-hub-t20-design.md §3.3「レシピ一括
//! 書き込み」）: `POST /api/v1/values/batch` の安全契約を固定する。
//!
//! `tests/write.rs`（単票の書き込みゲート網羅）と同じ理由（各
//! `tests/*.rs` は独立クレートとしてコンパイルされ private helper を
//! 共有できない）で `fast_options`/`TestApp` 相当をこのファイル内に複製
//! している。
//!
//! テスト構成（実装指示のテスト計画1〜8のうち、このファイルが担うもの。
//! 1「単票不変」は `tests/write.rs`/`tests/grpc.rs`/`tests/mcp.rs` が
//! すでに固定しているのでここでは重複させない）:
//! 2. 事前ゲート all-or-nothing（最重要）: 1エントリ NG なら他の正当な
//!    エントリも一切書かれない（監査行数不変・シミュレータ不変で確認）。
//! 3. バッチ成功・同一接続1ジョブ: 複数エントリが実機（シミュレータ）の
//!    ワイヤへ全て届く。
//! 4. 複数接続: 接続ごとにグルーピングされ、それぞれ書き込まれる。
//! 5. write_enabled off で全体拒否・無書込。
//! 6. レート制限超過で全体拒否・無書込（該当エントリのみ監査）。
//! 8. REST: 認証（write スコープ per entry・セッション token 403）、
//!    per-entry 応答、スコープ不足エントリ混在で全体拒否。

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

/// Temp-dir prefix passed to `TempEnv::new` (see `tests/common/mod.rs`).
const TEMP_ENV_PREFIX: &str = "banto-hub-t20-batch-it";

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
    }
}

fn group_input(name: &str, conn_id: i64, period_ms: i64) -> CollectionGroupInput {
    CollectionGroupInput {
        name: name.to_string(),
        plc_connection_id: conn_id,
        period_ms,
        enabled: true,
        default_writable: true,
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

struct TestApp {
    router: Router,
    admin_token: String,
    pool: SqlitePool,
    manager: Arc<CollectorManager>,
    write_control: Arc<WriteControl>,
    _env: TempEnv,
}

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

/// `write-audit/list` の全行数（`hub_write_audit` の総行数の代理 - この
/// crateのテストに管理系の直接カウント API が無いため一覧の長さで見る）。
async fn audit_row_count(router: &Router, admin_token: &str) -> usize {
    let (status, listed) =
        admin_post(router, "/api/write-audit/list", admin_token, json!({})).await;
    assert_eq!(status, StatusCode::OK);
    listed["rows"].as_array().unwrap().len()
}

async fn audit_rows_for_tag(router: &Router, admin_token: &str, tag_id: i64) -> Vec<Value> {
    let (status, listed) =
        admin_post(router, "/api/write-audit/list", admin_token, json!({})).await;
    assert_eq!(status, StatusCode::OK);
    listed["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["tagId"].as_i64() == Some(tag_id))
        .cloned()
        .collect()
}

/// 1本の SLMP 接続 + 1グループを作り、`(group_id, group_name)` を返す -
/// 複数タグを同一接続へ足していくバッチテストの土台。`collection_groups.name`
/// はグローバルに UNIQUE(`crate::hub::TagMap`のdoc comment参照)なので、
/// グループ名は `{conn_name}-fast` にして複数接続を1テストに同居させても
/// 衝突しないようにする。外部名は `{conn.name}.{group.name}.{tag.name}`
/// (`crate::hub::read_current`が組み立てる形)なので、[`create_tag`]は
/// この関数が返す実際のグループ名を使って外部名を組み立てる。
async fn setup_connection(app: &TestApp, conn_name: &str, port: u16) -> (i64, String) {
    let conn = PlcConnectionService::new(app.pool.clone())
        .create(slmp_conn_input(conn_name, port))
        .await
        .unwrap();
    let group_name = format!("{conn_name}-fast");
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input(&group_name, conn.id, 100))
        .await
        .unwrap();
    (group.id, group_name)
}

/// `setup_connection` が返した `(group_id, group_name)` の下へ1本タグを
/// 作り、rebuild まで済ませて `(tag_id, external_name)` を返す。
#[allow(clippy::too_many_arguments)]
async fn create_tag(
    app: &TestApp,
    conn_name: &str,
    group_id: i64,
    group_name: &str,
    tag_name: &str,
    address: &str,
    data_type: &str,
    writable: bool,
    enabled: bool,
) -> (i64, String) {
    let tag = TagService::new(app.pool.clone())
        .create(tag_input(
            tag_name, group_id, address, data_type, writable, enabled,
        ))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild");
    (tag.id, format!("{conn_name}.{group_name}.{tag_name}"))
}

// ---------------------------------------------------------------------------
// 2. 事前ゲート all-or-nothing（最重要）
// ---------------------------------------------------------------------------

/// 3本のタグ(a, b: writable、bad: not-writable)を同一接続に登録し、
/// `[a, bad, b]` をバッチ書き込みする。1件でも NG(`bad`)があるので、
/// a・b も一切書かれないことを、監査行数不変・シミュレータのレジスタ不変
/// で決定的に確認する。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_bad_entry_aborts_the_whole_batch_with_no_audit_rows_and_no_wire_writes() {
    let app = test_app("all-or-nothing").await;
    let sim = Simulator::start().await;

    let (group_id, group_name) = setup_connection(&app, "line1", sim.addr.port()).await;
    let (_tag_a, name_a) = create_tag(
        &app,
        "line1",
        group_id,
        &group_name,
        "a",
        "D100",
        "u16",
        true,
        true,
    )
    .await;
    let (_tag_b, name_b) = create_tag(
        &app,
        "line1",
        group_id,
        &group_name,
        "b",
        "D101",
        "u16",
        true,
        true,
    )
    .await;
    // not-writable タグ - gate 2 で NG。
    let (_tag_bad, name_bad) = create_tag(
        &app,
        "line1",
        group_id,
        &group_name,
        "bad",
        "D102",
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
        &[
            &format!("write:{name_a}"),
            &format!("write:{name_b}"),
            &format!("write:{name_bad}"),
        ],
    )
    .await;

    let audit_before = audit_row_count(&app.router, &app.admin_token).await;

    let (status, body) = v1_post(
        &app.router,
        "/api/v1/values/batch",
        &key,
        json!({
            "writes": [
                { "tag": name_a, "v": 111 },
                { "tag": name_bad, "v": 1 },
                { "tag": name_b, "v": 222 },
            ]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let writes = body["writes"].as_array().unwrap();
    assert_eq!(writes.len(), 3);
    assert_eq!(writes[0]["tag"], name_a);
    assert_eq!(writes[0]["ok"], false);
    assert_eq!(writes[0]["error"], "batch_aborted");
    assert_eq!(writes[1]["tag"], name_bad);
    assert_eq!(writes[1]["ok"], false);
    assert_eq!(writes[1]["error"], "not_writable");
    assert_eq!(writes[2]["tag"], name_b);
    assert_eq!(writes[2]["ok"], false);
    assert_eq!(writes[2]["error"], "batch_aborted");

    // 決定的固定その1: write_audit の行数が増えていない(1件も監査 insert
    // されていない - suppressed 系すら発生しない)。
    let audit_after = audit_row_count(&app.router, &app.admin_token).await;
    assert_eq!(
        audit_before, audit_after,
        "no audit row should be inserted when the batch is aborted pre-gate"
    );

    // 決定的固定その2: シミュレータのレジスタが初期値(0)のまま - a・b
    // いずれも PLC へ届いていない。
    assert_eq!(sim.get_word(SlmpDevice::D, 100), 0, "tag a must not land");
    assert_eq!(sim.get_word(SlmpDevice::D, 101), 0, "tag b must not land");

    sim.stop();
}

/// 2026-09-05 監査対応(修正2): 同一バッチ内に同じ外部名が2回以上現れたら
/// DB にもレート制限にも一切触れず全体を拒否する - 重複していたタグには
/// `duplicate_tag_in_batch`、それ以外(このバッチでは無い)は
/// `batch_aborted`。1件も書かれない(レジスタ不変・監査行数不変)ことも
/// 決定的に確認する。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duplicate_tag_in_the_same_batch_aborts_the_whole_batch() {
    let app = test_app("duplicate-tag").await;
    let sim = Simulator::start().await;

    let (group_id, group_name) = setup_connection(&app, "line1", sim.addr.port()).await;
    let (_tag_a, name_a) = create_tag(
        &app,
        "line1",
        group_id,
        &group_name,
        "a",
        "D100",
        "u16",
        true,
        true,
    )
    .await;
    let (_tag_b, name_b) = create_tag(
        &app,
        "line1",
        group_id,
        &group_name,
        "b",
        "D101",
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
        &[&format!("write:{name_a}"), &format!("write:{name_b}")],
    )
    .await;

    let audit_before = audit_row_count(&app.router, &app.admin_token).await;

    // "a" が2回現れる - 曖昧なレシピ(どちらの値が最終値か不定)。
    let (status, body) = v1_post(
        &app.router,
        "/api/v1/values/batch",
        &key,
        json!({
            "writes": [
                { "tag": name_a, "v": 111 },
                { "tag": name_b, "v": 222 },
                { "tag": name_a, "v": 333 },
            ]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let writes = body["writes"].as_array().unwrap();
    assert_eq!(writes.len(), 3);
    assert_eq!(writes[0]["tag"], name_a);
    assert_eq!(writes[0]["ok"], false);
    assert_eq!(writes[0]["error"], "duplicate_tag_in_batch");
    assert_eq!(writes[1]["tag"], name_b);
    assert_eq!(writes[1]["ok"], false);
    assert_eq!(writes[1]["error"], "batch_aborted");
    assert_eq!(writes[2]["tag"], name_a);
    assert_eq!(writes[2]["ok"], false);
    assert_eq!(writes[2]["error"], "duplicate_tag_in_batch");

    // 1件も書かれていない: 監査行数不変・レジスタ不変。
    let audit_after = audit_row_count(&app.router, &app.admin_token).await;
    assert_eq!(audit_before, audit_after);
    assert_eq!(sim.get_word(SlmpDevice::D, 100), 0);
    assert_eq!(sim.get_word(SlmpDevice::D, 101), 0);

    sim.stop();
}

// ---------------------------------------------------------------------------
// 3. バッチ成功・同一接続1ジョブ
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_connection_batch_lands_all_entries_on_the_wire() {
    let app = test_app("same-conn-success").await;
    let sim = Simulator::start().await;

    let (group_id, group_name) = setup_connection(&app, "line1", sim.addr.port()).await;
    let (_tag_a, name_a) = create_tag(
        &app,
        "line1",
        group_id,
        &group_name,
        "a",
        "D100",
        "u16",
        true,
        true,
    )
    .await;
    let (_tag_b, name_b) = create_tag(
        &app,
        "line1",
        group_id,
        &group_name,
        "b",
        "D101",
        "u16",
        true,
        true,
    )
    .await;
    let (_tag_c, name_c) = create_tag(
        &app,
        "line1",
        group_id,
        &group_name,
        "c",
        "D102",
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
        &[
            &format!("write:{name_a}"),
            &format!("write:{name_b}"),
            &format!("write:{name_c}"),
        ],
    )
    .await;

    let (status, body) = v1_post(
        &app.router,
        "/api/v1/values/batch",
        &key,
        json!({
            "writes": [
                { "tag": name_a, "v": 111 },
                { "tag": name_b, "v": 222 },
                { "tag": name_c, "v": 333 },
            ]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let writes = body["writes"].as_array().unwrap();
    for w in writes {
        assert_eq!(w["ok"], true, "{w:?}");
    }

    assert_eq!(sim.get_word(SlmpDevice::D, 100), 111);
    assert_eq!(sim.get_word(SlmpDevice::D, 101), 222);
    assert_eq!(sim.get_word(SlmpDevice::D, 102), 333);

    sim.stop();
}

// ---------------------------------------------------------------------------
// 4. 複数接続
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_batch_spanning_two_connections_writes_both() {
    let app = test_app("two-conn").await;
    let sim1 = Simulator::start().await;
    let sim2 = Simulator::start().await;

    let (group1, group1_name) = setup_connection(&app, "line1", sim1.addr.port()).await;
    let (group2, group2_name) = setup_connection(&app, "line2", sim2.addr.port()).await;
    let (_tag_a, name_a) = create_tag(
        &app,
        "line1",
        group1,
        &group1_name,
        "a",
        "D100",
        "u16",
        true,
        true,
    )
    .await;
    let (_tag_b, name_b) = create_tag(
        &app,
        "line2",
        group2,
        &group2_name,
        "b",
        "D200",
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
        &[&format!("write:{name_a}"), &format!("write:{name_b}")],
    )
    .await;

    let (status, body) = v1_post(
        &app.router,
        "/api/v1/values/batch",
        &key,
        json!({
            "writes": [
                { "tag": name_a, "v": 41 },
                { "tag": name_b, "v": 42 },
            ]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    for w in body["writes"].as_array().unwrap() {
        assert_eq!(w["ok"], true, "{w:?}");
    }

    assert_eq!(sim1.get_word(SlmpDevice::D, 100), 41);
    assert_eq!(sim2.get_word(SlmpDevice::D, 200), 42);

    sim1.stop();
    sim2.stop();
}

// ---------------------------------------------------------------------------
// 5. write_enabled off
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn writes_disabled_rejects_the_whole_batch_and_audits_each_entry() {
    let app = test_app("writes-disabled").await;
    let sim = Simulator::start().await;

    let (group_id, group_name) = setup_connection(&app, "line1", sim.addr.port()).await;
    let (tag_a, name_a) = create_tag(
        &app,
        "line1",
        group_id,
        &group_name,
        "a",
        "D100",
        "u16",
        true,
        true,
    )
    .await;
    let (tag_b, name_b) = create_tag(
        &app,
        "line1",
        group_id,
        &group_name,
        "b",
        "D101",
        "u16",
        true,
        true,
    )
    .await;

    // write_control は既定 disabled のまま。
    let (key, _id) = issue_key(
        &app.router,
        &app.admin_token,
        "writer",
        &[&format!("write:{name_a}"), &format!("write:{name_b}")],
    )
    .await;

    let (status, body) = v1_post(
        &app.router,
        "/api/v1/values/batch",
        &key,
        json!({
            "writes": [
                { "tag": name_a, "v": 1 },
                { "tag": name_b, "v": 2 },
            ]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    for w in body["writes"].as_array().unwrap() {
        assert_eq!(w["ok"], false, "{w:?}");
        assert_eq!(w["error"], "writes_disabled");
    }

    assert_eq!(sim.get_word(SlmpDevice::D, 100), 0);
    assert_eq!(sim.get_word(SlmpDevice::D, 101), 0);

    let rows_a = audit_rows_for_tag(&app.router, &app.admin_token, tag_a).await;
    assert_eq!(rows_a.len(), 1);
    assert_eq!(rows_a[0]["result"], "suppressed_disabled");
    let rows_b = audit_rows_for_tag(&app.router, &app.admin_token, tag_b).await;
    assert_eq!(rows_b.len(), 1);
    assert_eq!(rows_b[0]["result"], "suppressed_disabled");

    sim.stop();
}

// ---------------------------------------------------------------------------
// 6. レート制限
// ---------------------------------------------------------------------------

/// タグ a を単票書き込みで per_tag_max(既定10)ちょうどまで消費させた後、
/// `[a, b]` のバッチを送る - a は peek で既に超過、b は初めての書き込みだが
/// 「1件でも超過なら全体拒否」でどちらも拒否される。監査は超過した a のみ
/// (§3.3 の設計判断、`crate::write_path::execute_write_batch`のdoc
/// comment参照)。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rate_limit_exceeded_by_one_entry_rejects_the_whole_batch() {
    let app = test_app("rate-limit-batch").await;
    let sim = Simulator::start().await;

    let (group_id, group_name) = setup_connection(&app, "line1", sim.addr.port()).await;
    let (tag_a, name_a) = create_tag(
        &app,
        "line1",
        group_id,
        &group_name,
        "a",
        "D100",
        "u16",
        true,
        true,
    )
    .await;
    let (tag_b, name_b) = create_tag(
        &app,
        "line1",
        group_id,
        &group_name,
        "b",
        "D101",
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
        &[&format!("write:{name_a}"), &format!("write:{name_b}")],
    )
    .await;

    // 既定 per_tag_max=10: 単票で a をちょうど10件消費する。
    for i in 0..10 {
        let (status, body) = v1_post(
            &app.router,
            &format!("/api/v1/values/{name_a}"),
            &key,
            json!({ "v": i }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "seed write {i}: {body:?}");
    }
    let audit_before = audit_row_count(&app.router, &app.admin_token).await;

    let (status, body) = v1_post(
        &app.router,
        "/api/v1/values/batch",
        &key,
        json!({
            "writes": [
                { "tag": name_a, "v": 999 },
                { "tag": name_b, "v": 1 },
            ]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let writes = body["writes"].as_array().unwrap();
    assert_eq!(writes[0]["ok"], false);
    assert_eq!(writes[0]["error"], "rate_limited");
    assert_eq!(writes[1]["ok"], false);
    assert_eq!(writes[1]["error"], "rate_limited");

    // tag b は1件も書かれていない(レジスタは初期値のまま)。
    assert_eq!(sim.get_word(SlmpDevice::D, 101), 0);

    // 監査: トリップの原因になった a には rate_limit_tripped が1行増える
    // が、b には増えない(このモジュールの doc comment 参照)。
    let audit_after = audit_row_count(&app.router, &app.admin_token).await;
    assert_eq!(audit_after, audit_before + 1);
    let rows_b = audit_rows_for_tag(&app.router, &app.admin_token, tag_b).await;
    assert!(
        rows_b.is_empty(),
        "tag b never exceeded on its own peek, so it should get no audit row: {rows_b:?}"
    );
    let rows_a = audit_rows_for_tag(&app.router, &app.admin_token, tag_a).await;
    let tripped_for_a = rows_a
        .iter()
        .filter(|r| r["action"] == "rate_limit_tripped")
        .count();
    assert_eq!(tripped_for_a, 1);

    sim.stop();
}

// ---------------------------------------------------------------------------
// 8. REST 認証
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_token_cannot_use_the_batch_endpoint() {
    let app = test_app("batch-session-token").await;
    let sim = Simulator::start().await;
    let (group_id, group_name) = setup_connection(&app, "line1", sim.addr.port()).await;
    let (_tag_a, name_a) = create_tag(
        &app,
        "line1",
        group_id,
        &group_name,
        "a",
        "D100",
        "u16",
        true,
        true,
    )
    .await;
    app.write_control.enable();

    let (status, body) = v1_post(
        &app.router,
        "/api/v1/values/batch",
        &app.admin_token,
        json!({ "writes": [ { "tag": name_a, "v": 1 } ] }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "session_token_cannot_write");
    sim.stop();
}

/// 1件でも write スコープが無ければ、スコープを持つ他のエントリも一切
/// 書かれず全体を 403 で拒否する(REST 前段の all-or-nothing、設計 §3.3)。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_write_scope_on_one_entry_rejects_the_whole_batch() {
    let app = test_app("batch-scope-mismatch").await;
    let sim = Simulator::start().await;

    let (group_id, group_name) = setup_connection(&app, "line1", sim.addr.port()).await;
    let (tag_a, name_a) = create_tag(
        &app,
        "line1",
        group_id,
        &group_name,
        "a",
        "D100",
        "u16",
        true,
        true,
    )
    .await;
    let (_tag_b, name_b) = create_tag(
        &app,
        "line1",
        group_id,
        &group_name,
        "b",
        "D101",
        "u16",
        true,
        true,
    )
    .await;

    app.write_control.enable();
    // このキーは a への write スコープしか持たない。
    let (key, _id) = issue_key(
        &app.router,
        &app.admin_token,
        "writer",
        &[&format!("write:{name_a}")],
    )
    .await;

    let (status, body) = v1_post(
        &app.router,
        "/api/v1/values/batch",
        &key,
        json!({
            "writes": [
                { "tag": name_a, "v": 1 },
                { "tag": name_b, "v": 2 },
            ]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body:?}");
    assert_eq!(body["error"], "missing_write_scope");

    // a も一切書かれていない(スコープ不足エントリの混在で全体拒否)。
    assert_eq!(sim.get_word(SlmpDevice::D, 100), 0);
    let rows_a = audit_rows_for_tag(&app.router, &app.admin_token, tag_a).await;
    assert!(rows_a.is_empty());

    sim.stop();
}
