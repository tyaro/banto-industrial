//! T15-2 の E2E テスト（docs/banto-hub-desktop-plan.md §9.7「シミュレーション
//! capability プリフライト」）: all-simulation 開始前に「対応 N タグ / 未対応
//! M タグ」を一覧表示する `GET /api/collection/simulation-coverage` の配線を
//! 確認する。
//!
//! `tests/t9_simulation.rs`と同じ理由（各`tests/*.rs`は独立したクレートとして
//! コンパイルされ、private helper を共有できない）で`fast_options`/`TestApp`
//! 相当をこのファイル内に複製している（`t9_simulation.rs`のものをベースに
//! した、SLMP シミュレータ/broker セッション周りの検証は不要なので簡略化）。
//! `TempEnv`は`tests/common/mod.rs`に集約済み。
//!
//! テスト構成:
//! 1. Modbus 接続に「シミュレータのウィンドウ内(対応)」「範囲外(未対応)」
//!    「文字列(未対応)」の3タグ、SLMP 接続に「D(対応)」「D 範囲外(未対応)」
//!    「X(未対応、D/M 以外)」の3タグを登録し、`GET
//!    /api/collection/simulation-coverage`が正しい件数・理由を返すことを
//!    確認する。無効化したタグ(`enabled: false`)は集計にも一覧にも現れない
//!    こと(プリフライトは「有効な物理 PLC タグ」だけを対象にする、
//!    `crates/banto-collect/src/simulation.rs`の T15-2 doc comment参照)も
//!    合わせて確認する。
//! 2. 未対応タグが存在していても`POST /api/collection/start-all-simulation`
//!    は成功する(ブロックしない) - プラン §9.7 の決定「表示のみ、start 自体
//!    は妨げない」の直接確認。

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

/// Temp-dir prefix passed to `TempEnv::new` (see `tests/common/mod.rs`).
const TEMP_ENV_PREFIX: &str = "banto-hub-t15-it";

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

fn tag_input(name: &str, group_id: i64, address: &str, data_type: &str, enabled: bool) -> TagInput {
    // `data_type == "string"` requires a `string_length` (banto-tags
    // validation) - every other data type requires it to be absent.
    let string_length = (data_type == banto_tags::STRING_DATA_TYPE).then_some(8);
    TagInput {
        name: name.to_string(),
        collection_group_id: group_id,
        address: address.to_string(),
        data_type: data_type.to_string(),
        string_length,
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
        writable: false,
        tag_kind: "plc".to_string(),
        expression: None,
        retain: false,
    }
}

struct TestApp {
    router: Router,
    admin_token: String,
    pool: SqlitePool,
    manager: Arc<CollectorManager>,
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

/// admin 管理系の POST/PUT だが body なし（`/api/collection/start*`用）。
async fn admin_post_empty(router: &Router, path: &str, token: &str) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(path)
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

async fn create_connection(pool: &SqlitePool, name: &str, protocol: &str, port: i64) -> i64 {
    PlcConnectionService::new(pool.clone())
        .create(PlcConnectionInput {
            name: name.to_string(),
            protocol: protocol.to_string(),
            host: "127.0.0.1".to_string(),
            port,
            unit_id: 1,
            enabled: true,
            simulation: false,
        })
        .await
        .expect("create connection")
        .id
}

// ---------------------------------------------------------------------------
// 1. GET /api/collection/simulation-coverage: 対応/未対応の集計と理由の確認。
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn simulation_coverage_reports_supported_and_unsupported_tags() {
    let app = test_app("t15-coverage").await;

    // Modbus 接続: 40001(対応/ウィンドウ内)、40017(未対応/ウィンドウ外)、
    // 文字列タグ(未対応、アドレスはウィンドウ内でも data_type=string は
    // 常に非対応)。
    let modbus_conn_id = create_connection(&app.pool, "line1", "modbus-tcp", 15021).await;
    let modbus_group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", modbus_conn_id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("ok_word", modbus_group.id, "40001", "u16", true))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input(
            "out_of_window",
            modbus_group.id,
            "40017",
            "u16",
            true,
        ))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input(
            "str_tag",
            modbus_group.id,
            "40001",
            "string",
            true,
        ))
        .await
        .unwrap();
    // 無効化したタグ(範囲外アドレス)- プリフライトの集計には出てはいけない。
    TagService::new(app.pool.clone())
        .create(tag_input(
            "disabled_out_of_window",
            modbus_group.id,
            "40099",
            "u16",
            false,
        ))
        .await
        .unwrap();

    // SLMP 接続: D0(対応)、D16(未対応/ウィンドウ外)、X0(未対応/D・M 以外)。
    let slmp_conn_id = create_connection(&app.pool, "line2", "slmp", 15022).await;
    let slmp_group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("slmp_fast", slmp_conn_id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("d0", slmp_group.id, "D0", "u16", true))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("d16", slmp_group.id, "D16", "u16", true))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("x0", slmp_group.id, "X0", "bit", true))
        .await
        .unwrap();

    let (status, body) = get_json(
        &app.router,
        "/api/collection/simulation-coverage",
        &app.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");

    // 対応: ok_word, d0 の2件。未対応: out_of_window, str_tag, d16, x0 の4件
    // (disabled_out_of_window はどちらにも入らない)。
    assert_eq!(body["supportedCount"], 2, "{body:?}");
    assert_eq!(body["unsupportedCount"], 4, "{body:?}");

    let unsupported = body["unsupported"].as_array().expect("unsupported array");
    assert_eq!(unsupported.len(), 4);

    let names: Vec<&str> = unsupported
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"out_of_window"));
    assert!(names.contains(&"str_tag"));
    assert!(names.contains(&"d16"));
    assert!(names.contains(&"x0"));
    assert!(
        !names.contains(&"disabled_out_of_window"),
        "disabled tags must not appear in the preflight at all: {unsupported:?}"
    );
    assert!(
        !names.contains(&"ok_word"),
        "a supported tag must not appear in the unsupported list: {unsupported:?}"
    );

    let out_of_window = unsupported
        .iter()
        .find(|entry| entry["name"] == "out_of_window")
        .expect("out_of_window entry");
    assert_eq!(out_of_window["connection"], "line1");
    assert_eq!(out_of_window["group"], "fast");
    assert_eq!(out_of_window["address"], "40017");
    assert_eq!(out_of_window["dataType"], "u16");
    assert!(
        out_of_window["reason"].as_str().unwrap().contains("16"),
        "{out_of_window:?}"
    );

    let str_tag = unsupported
        .iter()
        .find(|entry| entry["name"] == "str_tag")
        .expect("str_tag entry");
    assert!(
        str_tag["reason"].as_str().unwrap().contains("文字列"),
        "{str_tag:?}"
    );

    let x0 = unsupported
        .iter()
        .find(|entry| entry["name"] == "x0")
        .expect("x0 entry");
    assert_eq!(x0["connection"], "line2");
    assert_eq!(x0["address"], "X0");
}

// ---------------------------------------------------------------------------
// 2. 未対応タグが存在していても start-all-simulation はブロックされない
//    (プラン §9.7: プリフライトは表示専用)。
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsupported_tags_do_not_block_starting_all_simulation() {
    let app = test_app("t15-start-all-sim").await;

    let conn_id = create_connection(&app.pool, "line1", "modbus-tcp", 15023).await;
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn_id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("ok_word", group.id, "40001", "u16", true))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("out_of_window", group.id, "40099", "u16", true))
        .await
        .unwrap();

    // 未対応タグが1件あることをまず確認しておく(このテストの前提)。
    let (status, coverage) = get_json(
        &app.router,
        "/api/collection/simulation-coverage",
        &app.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(coverage["unsupportedCount"], 1, "{coverage:?}");

    let (status, started) = admin_post_empty(
        &app.router,
        "/api/collection/start-all-simulation",
        &app.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{started:?}");
    assert_eq!(
        started["state"], "running",
        "start(AllSimulation) must succeed despite an unsupported tag: {started:?}"
    );
    assert_eq!(started["mode"], "all_simulation");
}
