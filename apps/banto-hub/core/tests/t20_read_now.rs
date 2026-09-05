//! T20 ①b の統合テスト（docs/banto-hub-t20-design.md §3.1、案A「分離経路」）:
//! read-on-demand（その場読み）REST `GET /api/v1/values/{tag}/read-now` と
//! MCP `read_tag_now` の E2E。
//!
//! `tests/write.rs`/`tests/mcp.rs` と同じ理由（各 `tests/*.rs` は独立クレート
//! としてコンパイルされ、private helper を共有できない）で `TestApp`/
//! `fast_options`/`make_tag`等をこのファイル内に複製している。`TestApp`は
//! `tests/write.rs`と同じ`api_router`（`CollectionController`ゲート無し）で
//! 組み立てる - read-on-demand は`crate::write_path`と違って
//! `CollectionController`の状態を一切見ないので、このゲートは要らない。

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
    TagInput, TagService, MEM_CONNECTION_NAME, VIRTUAL_PROTOCOL,
};
use banto_tstore::SystemClock;
use serde_json::{json, Value};
use sqlx::SqlitePool;
use tokio::sync::broadcast;
use tower::ServiceExt;

mod common;
use common::TempEnv;

const TEMP_ENV_PREFIX: &str = "banto-hub-t20-read-now-it";

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

/// `tag_input`の string タグ版(`tests/write.rs::string_tag_input`と同型)。
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

/// スケーリング付き数値タグ(read-on-demand が cache 読みと同じ
/// `scale_raw` を適用することの検証用)。
fn scaled_tag_input(name: &str, group_id: i64, address: &str, data_type: &str) -> TagInput {
    TagInput {
        raw_lo: Some(0.0),
        raw_hi: Some(1000.0),
        eng_lo: Some(0.0),
        eng_hi: Some(100.0),
        ..tag_input(name, group_id, address, data_type, false, true)
    }
}

/// internal タグ用の `TagInput`(`tests/t20_batch_write.rs::internal_tag_input`
/// と同型) - read-on-demand の「PLC タグのみ対象」ゲートの検証用。
fn internal_tag_input(name: &str, group_id: i64, data_type: &str) -> TagInput {
    TagInput {
        name: name.to_string(),
        collection_group_id: group_id,
        address: String::new(),
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
        writable: true,
        tag_kind: "internal".to_string(),
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
    /// T15-4/T12 と同じ non-spawning peek を直接操作する(`stop_and_join`で
    /// セッション無し状態を作る)ためのハンドル - `tests/t15_write_peek.rs`
    /// と同じ理由。
    sessions: Arc<HubSessions>,
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

#[allow(clippy::too_many_arguments)]
async fn make_tag(
    app: &TestApp,
    conn_name: &str,
    port: u16,
    tag_name: &str,
    address: &str,
    data_type: &str,
    writable: bool,
    enabled: bool,
) -> (i64, String) {
    let conn = PlcConnectionService::new(app.pool.clone())
        .create(slmp_conn_input(conn_name, port))
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

async fn make_string_tag(
    app: &TestApp,
    conn_name: &str,
    port: u16,
    tag_name: &str,
    address: &str,
    string_length: i64,
    string_encoding: &str,
) -> (i64, String) {
    let conn = PlcConnectionService::new(app.pool.clone())
        .create(slmp_conn_input(conn_name, port))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    let tag = TagService::new(app.pool.clone())
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
    (tag.id, format!("{conn_name}.fast.{tag_name}"))
}

async fn make_scaled_tag(
    app: &TestApp,
    conn_name: &str,
    port: u16,
    tag_name: &str,
    address: &str,
    data_type: &str,
) -> (i64, String) {
    let conn = PlcConnectionService::new(app.pool.clone())
        .create(slmp_conn_input(conn_name, port))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    let tag = TagService::new(app.pool.clone())
        .create(scaled_tag_input(tag_name, group.id, address, data_type))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild");
    (tag.id, format!("{conn_name}.fast.{tag_name}"))
}

/// mem 予約接続配下に internal タグを1本作る(`tests/t20_batch_write.rs`と
/// 同じ手順 - 実バイナリが起動時に自動用意する `mem` 接続をテストハーネス
/// 側で直接プロビジョニングする)。
async fn make_internal_tag(app: &TestApp, tag_name: &str) -> String {
    PlcConnectionService::new(app.pool.clone())
        .create(PlcConnectionInput {
            name: MEM_CONNECTION_NAME.to_string(),
            protocol: VIRTUAL_PROTOCOL.to_string(),
            host: String::new(),
            port: 0,
            unit_id: 1,
            enabled: true,
            simulation: false,
            word_order: "low_high".to_string(),
        })
        .await
        .expect("mem connection should be provisioned");
    let mem_id = PlcConnectionService::new(app.pool.clone())
        .list(banto_core::ListParams::default())
        .await
        .unwrap()
        .rows
        .into_iter()
        .find(|c| c.name == MEM_CONNECTION_NAME)
        .unwrap()
        .id;
    let group_name = "mem-group";
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input(group_name, mem_id, 1_000))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(internal_tag_input(tag_name, group.id, "u16"))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild");
    format!("{MEM_CONNECTION_NAME}.{group_name}.{tag_name}")
}

// ---------------------------------------------------------------------------
// 1. write -> read-now 往復: UTF-8 / Shift-JIS 双方が正しく戻ること
//    (①a の write と①b の read-on-demand を組み合わせた E2E)。
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_now_round_trips_a_utf8_string_written_via_write_path() {
    let app = test_app("read-now-utf8-roundtrip").await;
    let sim = Simulator::start().await;

    let words = 5u16; // "テスト" is 9 UTF-8 bytes; 5 words (10 bytes).
    let (_tag_id, external_name) = make_string_tag(
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

    let key = issue_key(
        &app.router,
        &app.admin_token,
        "rw",
        &["write:line1.fast.recipe", "read:line1.fast.recipe"],
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

    let (status, body) = get_json(
        &app.router,
        &format!("/api/v1/values/{external_name}/read-now"),
        &key,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["tag"], external_name);
    assert_eq!(body["v"], "テスト", "{body:?}");
    assert_eq!(body["q"], "good");

    sim.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_now_round_trips_a_shift_jis_string_written_via_write_path() {
    let app = test_app("read-now-sjis-roundtrip").await;
    let sim = Simulator::start().await;

    let words = 4u16; // "テスト" is 6 Shift-JIS bytes; 4 words (8 bytes).
    let (_tag_id, external_name) = make_string_tag(
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

    let key = issue_key(
        &app.router,
        &app.admin_token,
        "rw",
        &["write:line1.fast.recipe", "read:line1.fast.recipe"],
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

    let (status, body) = get_json(
        &app.router,
        &format!("/api/v1/values/{external_name}/read-now"),
        &key,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["v"], "テスト", "{body:?}");

    sim.stop();
}

// ---------------------------------------------------------------------------
// 2. 数値/bit タグの read-on-demand
// ---------------------------------------------------------------------------

/// 数値タグは cache 読みと同じ `scale_raw` を適用する(オーナー方針、
/// `crate::read_path`のモジュール doc comment「スケーリング」節)。
/// raw_lo=0/raw_hi=1000/eng_lo=0/eng_hi=100 のスケーリングで raw=500 を
/// シミュレータへ直接注入し、read-now が 50.0(工学値)を返すことを確認する
/// (収集ループを一切待たない - read-on-demand はその場で PLC から読む)。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_now_applies_the_same_scaling_as_a_cache_read() {
    let app = test_app("read-now-scaling").await;
    let sim = Simulator::start().await;

    let (_tag_id, external_name) =
        make_scaled_tag(&app, "line1", sim.addr.port(), "temp01", "D100", "u16").await;

    sim.set_word(SlmpDevice::D, 100, 500);

    let key = issue_key(
        &app.router,
        &app.admin_token,
        "reader",
        &["read:line1.fast.temp01"],
    )
    .await;

    let (status, body) = get_json(
        &app.router,
        &format!("/api/v1/values/{external_name}/read-now"),
        &key,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["v"], 50.0, "{body:?}");
    assert_eq!(body["q"], "good");

    sim.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_now_returns_a_bit_value() {
    let app = test_app("read-now-bit").await;
    let sim = Simulator::start().await;

    let (_tag_id, external_name) = make_tag(
        &app,
        "line1",
        sim.addr.port(),
        "alarm",
        "M50",
        "bit",
        false,
        true,
    )
    .await;

    sim.set_bit(SlmpDevice::M, 50, true);

    let key = issue_key(
        &app.router,
        &app.admin_token,
        "reader",
        &["read:line1.fast.alarm"],
    )
    .await;

    let (status, body) = get_json(
        &app.router,
        &format!("/api/v1/values/{external_name}/read-now"),
        &key,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["v"], true, "{body:?}");

    sim.stop();
}

// ---------------------------------------------------------------------------
// 3. current_values/tstore を経由していないことの証明
// ---------------------------------------------------------------------------

/// 案A の核心: 文字列タグは収集パイプラインから意図的にスキップされる
/// (current_values に一切載らない)。read-on-demand は収集キャッシュを
/// 経由しないので、`current_values` にエントリが無い(=収集が一度も
/// この文字列タグをキャッシュしていない)状態でも、read-now は
/// シミュレータの実際の値を返せる。cache 読み(`GET /api/v1/values/{tag}`)
/// が `v: null` のままなことと対比させ、2つの経路が別物であることを
/// 直接示す。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_now_returns_a_value_the_collection_cache_never_had() {
    let app = test_app("read-now-cache-bypass").await;
    let sim = Simulator::start().await;

    let words = 4u16;
    let (tag_id, external_name) = make_string_tag(
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

    let key = issue_key(
        &app.router,
        &app.admin_token,
        "rw",
        &["write:line1.fast.recipe", "read:line1.fast.recipe"],
    )
    .await;

    let (status, body) = v1_post(
        &app.router,
        &format!("/api/v1/values/{external_name}"),
        &key,
        json!({ "v": "ABC" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");

    // 収集ループが多少回るのを待っても、文字列タグの current_values
    // エントリは決して現れない(S1 の string スキップは既存の不変
    // だが、read-on-demand がそれを経由していないことの前提として
    // 明示的に確認する)。
    tokio::time::sleep(Duration::from_millis(300)).await;
    let tag_key = format!("tag:{tag_id}");
    assert!(
        app.manager
            .current_values()
            .and_then(|c| c.get(&tag_key))
            .is_none(),
        "string tags must never appear in the collection cache (S1 skip)"
    );

    // cache 読みはこのタグを never-collected として v: null を返す。
    let (status, cache_body) = get_json(
        &app.router,
        &format!("/api/v1/values/{external_name}"),
        &key,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{cache_body:?}");
    assert!(
        cache_body["v"].is_null(),
        "cache read must not be able to surface a string value: {cache_body:?}"
    );

    // 一方 read-on-demand は current_values を経由せず PLC(シミュレータ)
    // から直接読むので、正しい値が返る。
    let (status, now_body) = get_json(
        &app.router,
        &format!("/api/v1/values/{external_name}/read-now"),
        &key,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{now_body:?}");
    assert_eq!(now_body["v"], "ABC", "{now_body:?}");

    sim.stop();
}

// ---------------------------------------------------------------------------
// 4. ゲート網羅
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_now_is_404_for_an_unknown_tag() {
    let app = test_app("read-now-404").await;
    let key = issue_key(&app.router, &app.admin_token, "reader", &["read"]).await;

    let (status, _body) =
        get_json(&app.router, "/api/v1/values/nope.nope.nope/read-now", &key).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// per-tag read スコープ外は 403(H10 ③と同じ規律、cache 読みの
/// `v1_value_single_unknown_tag_is_404`系テストの姉妹)。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_now_is_403_for_a_tag_outside_the_keys_read_scope() {
    let app = test_app("read-now-403").await;
    let sim = Simulator::start().await;
    let (_tag_id, external_name) = make_tag(
        &app,
        "line1",
        sim.addr.port(),
        "temp01",
        "D100",
        "u16",
        false,
        true,
    )
    .await;

    let key = issue_key(
        &app.router,
        &app.admin_token,
        "reader",
        &["read:line1.fast.other_tag"],
    )
    .await;

    let (status, _body) = get_json(
        &app.router,
        &format!("/api/v1/values/{external_name}/read-now"),
        &key,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    sim.stop();
}

/// internal タグは PLC 接続を経由しないので read-on-demand の対象外
/// (422 `not_plc_backed`) - cache 読み(`GET /api/v1/values/{tag}`)を
/// 使うべき、という設計上のガイドをそのままテストにする。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_now_is_422_for_an_internal_tag() {
    let app = test_app("read-now-internal-422").await;
    let external_name = make_internal_tag(&app, "counter01").await;

    let key = issue_key(&app.router, &app.admin_token, "reader", &["read"]).await;

    let (status, body) = get_json(
        &app.router,
        &format!("/api/v1/values/{external_name}/read-now"),
        &key,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    assert_eq!(body["error"], "not_plc_backed");
}

/// T15-4/T12 と同じ non-spawning peek: セッションが無い(収集停止・
/// `stop_and_join`後)なら新規にはダイヤルせず 503 を返す。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_now_is_503_when_the_broker_session_is_gone() {
    let app = test_app("read-now-no-session").await;
    let sim = Simulator::start().await;
    let (_tag_id, external_name) = make_tag(
        &app,
        "line1",
        sim.addr.port(),
        "temp01",
        "D100",
        "u16",
        false,
        true,
    )
    .await;

    // rebuild が確立したセッションを直接落とす(`tests/t15_write_peek.rs`
    // と同じ手法)。
    let conn_id = PlcConnectionService::new(app.pool.clone())
        .list(banto_core::ListParams::default())
        .await
        .unwrap()
        .rows
        .into_iter()
        .find(|c| c.name == "line1")
        .unwrap()
        .id;
    assert!(
        wait_until(Duration::from_secs(10), || async {
            app.sessions.connection_count() == 1
        })
        .await,
        "rebuild should have established exactly one broker session"
    );
    assert!(app.sessions.stop_and_join(conn_id).await);
    assert_eq!(app.sessions.connection_count(), 0);

    let key = issue_key(
        &app.router,
        &app.admin_token,
        "reader",
        &["read:line1.fast.temp01"],
    )
    .await;

    let (status, body) = get_json(
        &app.router,
        &format!("/api/v1/values/{external_name}/read-now"),
        &key,
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body:?}");
    assert_eq!(body["error"], "no_session");
    assert_eq!(
        app.sessions.connection_count(),
        0,
        "a failed read-now must NOT have dialed a fresh session"
    );

    sim.stop();
}

// ---------------------------------------------------------------------------
// 5. MCP `read_tag_now`
// ---------------------------------------------------------------------------

async fn mcp_post(router: &Router, bearer: Option<&str>, body: Value) -> (StatusCode, Value) {
    let mut request = HttpRequest::post("/mcp").header("content-type", "application/json");
    if let Some(token) = bearer {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    let response = router
        .clone()
        .oneshot(
            request
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

fn rpc(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params })
}

fn tools_call(name: &str, arguments: Value) -> Value {
    rpc(
        "tools/call",
        json!({ "name": name, "arguments": arguments }),
    )
}

/// `read_tag_now` が `crate::read_path::execute_read_now` をそのまま呼んで
/// いる証拠: write→read-now の往復が REST と同じく MCP 経由でも成立する。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_read_tag_now_round_trips_a_string_value() {
    let app = test_app("mcp-read-now-roundtrip").await;
    let sim = Simulator::start().await;

    let words = 5u16;
    let (_tag_id, external_name) = make_string_tag(
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

    let key = issue_key(
        &app.router,
        &app.admin_token,
        "rw",
        &["write:line1.fast.recipe", "read:line1.fast.recipe"],
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

    let (status, body) = mcp_post(
        &app.router,
        Some(&key),
        tools_call("read_tag_now", json!({ "tag": external_name })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["tag"], external_name);
    assert_eq!(parsed["value"], "テスト", "{parsed:?}");

    sim.stop();
}

/// read スコープ不足は`tool_error`(isError:true)で拒否する - REST の 403
/// と同じ意味論を MCP の作法(JSON-RPC error ではなく`isError`)で表す。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_read_tag_now_denies_missing_read_scope() {
    let app = test_app("mcp-read-now-scope").await;
    let sim = Simulator::start().await;
    let (_tag_id, external_name) = make_tag(
        &app,
        "line1",
        sim.addr.port(),
        "temp01",
        "D100",
        "u16",
        false,
        true,
    )
    .await;

    let key = issue_key(
        &app.router,
        &app.admin_token,
        "reader",
        &["read:line1.fast.other_tag"],
    )
    .await;

    let (status, body) = mcp_post(
        &app.router,
        Some(&key),
        tools_call("read_tag_now", json!({ "tag": external_name })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], true, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("missing_read_scope"), "{text}");

    sim.stop();
}
