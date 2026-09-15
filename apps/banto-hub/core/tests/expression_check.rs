//! #342 段階A の統合テスト: `POST /api/tags/expression/check`
//! （`crate::rest::tags_expression_check` / `crate::rest::evaluate_expression_check`）
//! と、同じロジックを共有する MCP ツール `check_expression`
//! （`crate::mcp::tool_check_expression`）。
//!
//! `tests/computed.rs`/`tests/mcp.rs` と同じ理由でこのファイル自身が
//! `test_app`/`admin_post`/`mcp_post` 等を複製している（各 `tests/*.rs` は
//! 独立クレートとしてコンパイルされ、private helper を共有できない -
//! `tests/computed.rs` 冒頭の doc comment参照）。PLC 実機・シミュレータは
//! 一切使わない - この API が読むのは `calc`/`mem`（仮想接続）の
//! 演算タグ・内部タグだけで十分にすべての分岐を再現できる。
//!
//! テスト構成（実装指示「サーバー側テスト」1〜8 に対応）:
//! 1. OK: 有効な式 → `resultType`/`refs`/`preview.evaluated:true`
//! 2. 構文エラー: `pos` が既知の位置を指す（`crates/banto-expr/tests/compile.rs`
//!    の `syntax_error_position_points_at_offending_token` と同じ式）
//! 3. 型エラー: `kind = "type_mismatch"` + `pos`（同ファイルの
//!    `type_mismatch_error_position_points_at_binary_expression_start` と
//!    同じ式）
//! 4. 存在しないタグ参照: `kind = "unknown_tag"` + 外部名を含むメッセージ
//! 5. 循環: `externalName` を自分自身にした自己参照
//! 6. 参照が Bad: `preview.evaluated:false` + `reason`
//! 7. `SourceTooLong`: `pos: null`
//! 8. MCP `check_expression` 経由でも 1・2 と同じ結果になること

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
use banto_server::{AuthState, Identity};
use banto_tags::{
    CollectionGroupInput, CollectionGroupService, PlcConnectionInput, PlcConnectionService,
    TagInput, TagService, CALC_CONNECTION_NAME, MEM_CONNECTION_NAME, VIRTUAL_PROTOCOL,
};
use banto_tstore::SystemClock;
use serde_json::{json, Value};
use sqlx::SqlitePool;
use tokio::sync::broadcast;
use tower::ServiceExt;

mod common;
use common::TempEnv;

const CLIENT_HEADER: (&str, &str) = ("X-Banto-Client", "banto");
const TEMP_ENV_PREFIX: &str = "banto-hub-expression-check-it";

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

fn virtual_conn_input(name: &str) -> PlcConnectionInput {
    PlcConnectionInput {
        name: name.to_string(),
        protocol: VIRTUAL_PROTOCOL.to_string(),
        host: String::new(),
        port: 0,
        unit_id: 1,
        enabled: true,
        simulation: false,
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

fn computed_tag_input(name: &str, group_id: i64, expression: &str) -> TagInput {
    TagInput {
        name: name.to_string(),
        collection_group_id: group_id,
        address: String::new(),
        data_type: "f32".to_string(),
        string_length: None,
        string_encoding: "utf8".to_string(),
        raw_lo: None,
        raw_hi: None,
        eng_lo: None,
        eng_hi: None,
        unit: None,
        decimals: 2,
        threshold_h: None,
        threshold_hh: None,
        threshold_l: None,
        threshold_ll: None,
        enabled: true,
        writable: false,
        tag_kind: "computed".to_string(),
        expression: Some(expression.to_string()),
        retain: false,
        expected_revision: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn internal_tag_input(
    name: &str,
    group_id: i64,
    data_type: &str,
    unit: Option<&str>,
    writable: bool,
) -> TagInput {
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
        unit: unit.map(str::to_string),
        decimals: 2,
        threshold_h: None,
        threshold_hh: None,
        threshold_l: None,
        threshold_ll: None,
        enabled: true,
        writable,
        tag_kind: "internal".to_string(),
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

    // T6-2 (design §4.2/§4.3(a)): the real binary auto-provisions these at
    // startup - this harness does the same directly against the registry
    // (`tests/computed.rs::test_app` と同じ手順).
    for name in [CALC_CONNECTION_NAME, MEM_CONNECTION_NAME] {
        PlcConnectionService::new(pool.clone())
            .create(virtual_conn_input(name))
            .await
            .expect("virtual connection should be provisioned");
    }

    manager.rebuild().await.expect("initial rebuild");

    let api_keys = ApiKeysService::new(pool.clone());
    let (events_tx, _rx) = broadcast::channel(16);
    let write_control = Arc::new(WriteControl::new(false));
    write_control.enable();
    let write_audit = WriteAuditService::new(pool.clone());
    let mqtt = Arc::new(banto_hub_core::mqtt::MqttPublisher::new(manager.clone()));
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
        write_control,
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
        _env: env,
    }
}

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

fn tools_call(name: &str, arguments: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": name, "arguments": arguments },
    })
}

/// `mem` 接続下にグループを1本立て、内部タグを1件作る共通フィクスチャ。
/// 戻り値は `(group_id, external_name)`。
async fn make_mem_group(app: &TestApp, group_name: &str) -> i64 {
    let mem_id = PlcConnectionService::new(app.pool.clone())
        .list(banto_core::ListParams::default())
        .await
        .unwrap()
        .rows
        .into_iter()
        .find(|c| c.name == MEM_CONNECTION_NAME)
        .unwrap()
        .id;
    CollectionGroupService::new(app.pool.clone())
        .create(group_input(group_name, mem_id, 1_000))
        .await
        .unwrap()
        .id
}

async fn make_calc_group(app: &TestApp, group_name: &str) -> i64 {
    let calc_id = PlcConnectionService::new(app.pool.clone())
        .list(banto_core::ListParams::default())
        .await
        .unwrap()
        .rows
        .into_iter()
        .find(|c| c.name == CALC_CONNECTION_NAME)
        .unwrap()
        .id;
    CollectionGroupService::new(app.pool.clone())
        .create(group_input(group_name, calc_id, 1_000))
        .await
        .unwrap()
        .id
}

// ---------------------------------------------------------------------------
// 1. OK: 有効な式 → resultType/refs/preview.evaluated:true
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ok_expression_returns_result_type_refs_and_evaluated_preview() {
    let app = test_app("ok").await;
    let group = make_mem_group(&app, "x").await;
    TagService::new(app.pool.clone())
        .create(internal_tag_input("a", group, "f32", Some("degC"), true))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild after seeding");

    let key = issue_key(&app.router, &app.admin_token, "writer", &["write:mem.x.a"]).await;
    let (status, body) = v1_post(
        &app.router,
        "/api/v1/values/mem.x.a",
        &key,
        json!({ "v": 23.5 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");

    let (status, body) = admin_post(
        &app.router,
        "/api/tags/expression/check",
        &app.admin_token,
        json!({ "expression": "mem.x.a + 1" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], true, "{body:?}");
    assert_eq!(body["resultType"], "num");
    assert_eq!(body["refs"].as_array().unwrap().len(), 1);
    assert_eq!(body["refs"][0]["name"], "mem.x.a");
    assert_eq!(body["refs"][0]["dataType"], "f32");
    assert_eq!(body["refs"][0]["unit"], "degC");
    assert_eq!(body["refs"][0]["tagKind"], "internal");
    assert_eq!(body["preview"]["evaluated"], true, "{body:?}");
    assert_eq!(body["preview"]["value"].as_f64(), Some(24.5));
    assert!(body["preview"]["reason"].is_null());
    assert!(body["error"].is_null());
}

// ---------------------------------------------------------------------------
// 2. 構文エラー: pos が既知の位置を指す
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn syntax_error_reports_the_offending_position() {
    let app = test_app("syntax-error").await;

    // `crates/banto-expr/tests/compile.rs::syntax_error_position_points_at_offending_token`
    // と同じ式 - `banto_expr::compile("1 + ")` は Syntax { pos: 4 } を返す。
    let (status, body) = admin_post(
        &app.router,
        "/api/tags/expression/check",
        &app.admin_token,
        json!({ "expression": "1 + " }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], false, "{body:?}");
    assert!(body["resultType"].is_null());
    assert_eq!(body["refs"].as_array().unwrap().len(), 0);
    assert_eq!(body["preview"]["evaluated"], false);
    assert_eq!(body["error"]["kind"], "syntax");
    assert_eq!(body["error"]["pos"], 4);
}

// ---------------------------------------------------------------------------
// 3. 型エラー: kind = type_mismatch + pos
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn type_mismatch_reports_kind_and_position() {
    let app = test_app("type-mismatch").await;

    // `crates/banto-expr/tests/compile.rs::type_mismatch_error_position_points_at_binary_expression_start`
    // と同じ式 - `banto_expr::compile("1 + true")` は TypeMismatch { pos: 0 }。
    let (status, body) = admin_post(
        &app.router,
        "/api/tags/expression/check",
        &app.admin_token,
        json!({ "expression": "1 + true" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], false, "{body:?}");
    assert_eq!(body["error"]["kind"], "type_mismatch");
    assert_eq!(body["error"]["pos"], 0);
}

// ---------------------------------------------------------------------------
// 4. 存在しないタグ参照: kind = unknown_tag + 外部名を含むメッセージ
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_tag_reference_is_rejected() {
    let app = test_app("unknown-tag").await;

    let (status, body) = admin_post(
        &app.router,
        "/api/tags/expression/check",
        &app.admin_token,
        json!({ "expression": "line1.fast.nope + 1" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], false, "{body:?}");
    // コンパイル自体は成功しているので resultType は埋まる。
    assert_eq!(body["resultType"], "num");
    assert_eq!(body["error"]["kind"], "unknown_tag");
    assert!(body["error"]["pos"].is_null());
    let message = body["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("line1.fast.nope"),
        "message should mention the missing external name: {message}"
    );
}

// ---------------------------------------------------------------------------
// 5. 循環: externalName を自分自身にした自己参照
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn self_referencing_expression_with_external_name_is_a_cycle() {
    let app = test_app("cycle").await;
    let group = make_calc_group(&app, "y").await;
    TagService::new(app.pool.clone())
        .create(computed_tag_input("c", group, "1"))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild after seeding");

    let (status, body) = admin_post(
        &app.router,
        "/api/tags/expression/check",
        &app.admin_token,
        json!({ "expression": "calc.y.c + 1", "externalName": "calc.y.c" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], false, "{body:?}");
    assert_eq!(body["error"]["kind"], "cycle");
    assert!(body["error"]["pos"].is_null());
    let message = body["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("calc.y.c"),
        "message should mention the cyclic path: {message}"
    );

    // externalName が無ければ「単に既存タグを参照する式」であり循環では
    // ない - 同じ式でも externalName 抜きなら ok:true になることの確認
    // （循環判定が externalName 依存であることの反証テスト）。
    let (status, body) = admin_post(
        &app.router,
        "/api/tags/expression/check",
        &app.admin_token,
        json!({ "expression": "calc.y.c + 1" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], true, "{body:?}");
}

// ---------------------------------------------------------------------------
// 6. 参照が Bad: preview.evaluated:false + reason
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bad_reference_skips_preview_evaluation() {
    let app = test_app("bad-ref").await;
    let group = make_mem_group(&app, "x").await;
    TagService::new(app.pool.clone())
        .create(internal_tag_input("b", group, "f32", None, true))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild after seeding");
    // `mem.x.b` には一度も書き込んでいない -> Bad（値なし）。

    let (status, body) = admin_post(
        &app.router,
        "/api/tags/expression/check",
        &app.admin_token,
        json!({ "expression": "mem.x.b + 1" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], true, "{body:?}"); // 式自体は正しい
    assert_eq!(body["preview"]["evaluated"], false);
    assert!(body["preview"]["value"].is_null());
    let reason = body["preview"]["reason"].as_str().unwrap();
    assert!(
        reason.contains("mem.x.b"),
        "reason should mention the offending reference: {reason}"
    );
}

// ---------------------------------------------------------------------------
// 7. SourceTooLong: pos: null
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_too_long_has_no_position() {
    let app = test_app("source-too-long").await;

    // `banto_expr::MAX_SOURCE_CHARS` (1024) を超える式。
    let long_expression = "1".repeat(1025);
    assert!(long_expression.chars().count() > banto_expr::MAX_SOURCE_CHARS);

    let (status, body) = admin_post(
        &app.router,
        "/api/tags/expression/check",
        &app.admin_token,
        json!({ "expression": long_expression }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], false, "{body:?}");
    assert_eq!(body["error"]["kind"], "source_too_long");
    assert!(body["error"]["pos"].is_null(), "{body:?}");
}

// ---------------------------------------------------------------------------
// 8. MCP `check_expression` 経由でも 1・2 と同じ結果になること
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_check_expression_matches_rest_for_ok_and_syntax_error() {
    let app = test_app("mcp-parity").await;
    let group = make_mem_group(&app, "x").await;
    TagService::new(app.pool.clone())
        .create(internal_tag_input("a", group, "f32", Some("degC"), true))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild after seeding");

    let write_key = issue_key(&app.router, &app.admin_token, "writer", &["write:mem.x.a"]).await;
    let (status, body) = v1_post(
        &app.router,
        "/api/v1/values/mem.x.a",
        &write_key,
        json!({ "v": 23.5 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");

    let admin_key = issue_key(&app.router, &app.admin_token, "admin-key", &["admin"]).await;

    // 1. OK と同じ式・同じ結果。
    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call("check_expression", json!({ "expression": "mem.x.a + 1" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["ok"], true, "{payload:?}");
    assert_eq!(payload["resultType"], "num");
    assert_eq!(payload["refs"][0]["name"], "mem.x.a");
    assert_eq!(payload["refs"][0]["unit"], "degC");
    assert_eq!(payload["preview"]["evaluated"], true);
    assert_eq!(payload["preview"]["value"].as_f64(), Some(24.5));

    // 2. 構文エラーと同じ式・同じ結果。
    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call("check_expression", json!({ "expression": "1 + " })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["ok"], false, "{payload:?}");
    assert_eq!(payload["error"]["kind"], "syntax");
    assert_eq!(payload["error"]["pos"], 4);
}

// ---------------------------------------------------------------------------
// admin スコープが無い MCP 呼び出しは拒否される（`tag_tools_without_admin_scope_are_rejected`
// と同じ規律 - 書き込みではないが admin スコープ必須である点の確認）。
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_check_expression_without_admin_scope_is_rejected() {
    let app = test_app("mcp-no-admin").await;
    let read_key = issue_key(&app.router, &app.admin_token, "reader", &["read"]).await;

    let (status, body) = mcp_post(
        &app.router,
        Some(&read_key),
        tools_call("check_expression", json!({ "expression": "1 + 1" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], true, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("missing_admin_scope"), "{text}");
}
