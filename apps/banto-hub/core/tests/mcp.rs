//! T19 S5 の統合テスト（docs/banto-hub-t19-design.md §3.7・UX-41）:
//! `POST /mcp`（自前の最小 JSON-RPC 2.0）の E2E。
//!
//! `tests/write.rs`と同じ`TestApp`パターン（`api_router`で完全な
//! router を組み立て、`tower::ServiceExt::oneshot`で HTTP レベルから叩く）
//! を使う - これにより「`crate::mcp::mcp_router`が`crate::write_path::execute_write`
//! をそのまま呼んでいる（ゲートを再実装していない）」ことを、実際に
//! シミュレータへ値が届くかどうかで直接検証できる（`write.rs`の
//! E2E ハッピーパスと同じ考え方）。
//!
//! `write.rs`と違い、この`test_app`は**ロックダウンしない**（試運転モードの
//! まま）で返す - ロックダウン前後の両方の挙動を1ファイルでテストしたい
//! ため、各テストが必要なときだけ`app.commissioning.lock_down()`を呼ぶ。

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request as HttpRequest, StatusCode};
use axum::Router;
use banto_collect::{BackoffConfig, CollectorOptions};
use banto_core::ListParams;
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
use banto_plc::modbus::simulator::Simulator as ModbusSimulator;
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
const TEMP_ENV_PREFIX: &str = "banto-hub-mcp-it";

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
    write_audit: WriteAuditService,
    /// テストごとに任意のタイミングでロックダウンできるよう、router 構築に
    /// 使った `CommissioningService` と同じインスタンスのハンドルを持つ
    /// （`CommissioningService::state()`が返す`CommissioningState`は
    /// `Arc<AtomicBool>`共有なので、ここから`lock_down()`すれば router 側の
    /// `crate::mcp`が見る状態も即座に変わる）。
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

/// `locked_down = false`（試運転モードのまま）で返す - `write.rs`の
/// `test_app`と違い、ここでは呼び出し元が必要なときだけ
/// `app.commissioning.lock_down().await`する。
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
        write_audit.clone(),
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
        write_audit,
        commissioning,
        _env: env,
    }
}

/// `POST /api/api-keys` 経由でキーを発行し、平文キー全体(`bh_...`)を返す
/// (`write.rs::issue_key`と同型 - CSRF ヘッダ必須の管理系エンドポイント)。
async fn issue_key(router: &Router, admin_token: &str, name: &str, scopes: &[&str]) -> String {
    let response = router
        .clone()
        .oneshot(
            HttpRequest::post("/api/api-keys")
                .header("Authorization", format!("Bearer {admin_token}"))
                .header("X-Banto-Client", "banto")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({ "name": name, "scopes": scopes })).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    body["key"].as_str().unwrap().to_string()
}

/// タグ・グループ・接続を1本作って rebuild まで済ませ、`(tag_id,
/// external_name)` を返す共通フィクスチャ (`write.rs::make_tag`と同型)。
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

/// [`make_tag`]の string タグ版(T20 ①a、docs/banto-hub-t20-design.md
/// §3.1)。`writable`/`enabled`は常に true。
async fn make_string_tag(
    app: &TestApp,
    conn_name: &str,
    port: u16,
    tag_name: &str,
    address: &str,
    string_length: i64,
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
        .create(TagInput {
            string_length: Some(string_length),
            string_encoding: "utf8".to_string(),
            ..tag_input(tag_name, group.id, address, "string", true, true)
        })
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild");
    (tag.id, format!("{conn_name}.fast.{tag_name}"))
}

/// `POST /mcp` を叩く - `bearer` が `None` なら `Authorization` ヘッダ自体を
/// 付けない。JSON-RPC の応答本体を返す(通知の 202 は body が空になりうる
/// ので `Value::Null` にフォールバックする)。
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

async fn write_audit_row_count(app: &TestApp) -> u64 {
    app.write_audit
        .list(ListParams::default())
        .await
        .expect("write_audit.list")
        .total_count
}

// ---------------------------------------------------------------------------
// 1. initialize / tools/list
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initialize_returns_tool_capabilities() {
    let app = test_app("initialize").await;
    let key = issue_key(&app.router, &app.admin_token, "reader", &["read"]).await;

    let (status, body) = mcp_post(
        &app.router,
        Some(&key),
        rpc("initialize", json!({ "protocolVersion": "2025-06-18" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["jsonrpc"], "2.0");
    assert_eq!(body["result"]["protocolVersion"], "2025-06-18");
    assert!(body["result"]["capabilities"]["tools"].is_object());
    assert_eq!(body["result"]["serverInfo"]["name"], "banto-hub");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tools_list_returns_the_twenty_seven_tools() {
    let app = test_app("tools-list").await;
    let key = issue_key(&app.router, &app.admin_token, "reader", &["read"]).await;

    let (status, body) = mcp_post(&app.router, Some(&key), rpc("tools/list", json!({}))).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let tools = body["result"]["tools"].as_array().expect("tools array");
    let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec![
            "create_connection",
            "create_group",
            "create_tag",
            "delete_connection",
            "delete_group",
            "delete_tag",
            "get_grpc_settings",
            "get_mqtt_settings",
            "get_retention",
            "get_server_status",
            "get_tag",
            "list_connections",
            "list_groups",
            "list_tags",
            "read_tag_now",
            "read_tag_values",
            "set_collection",
            "set_grpc_settings",
            "set_mqtt_settings",
            "set_retention",
            "set_write_control",
            "test_connection",
            "update_connection",
            "update_group",
            "update_tag",
            "write_recipe",
            "write_tag_value",
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_method_is_a_json_rpc_error() {
    let app = test_app("unknown-method").await;
    let key = issue_key(&app.router, &app.admin_token, "reader", &["read"]).await;

    let (status, body) = mcp_post(&app.router, Some(&key), rpc("bogus/method", json!({}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["error"]["code"], -32601);
}

// ---------------------------------------------------------------------------
// 2. 認証: キー無し/不正/セッション token はいずれも 401
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_authorization_header_is_401() {
    let app = test_app("auth-missing").await;
    let (status, _) = mcp_post(&app.router, None, rpc("ping", json!({}))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_api_key_is_401() {
    let app = test_app("auth-invalid").await;
    let (status, _) = mcp_post(
        &app.router,
        Some("bh_this_is_not_a_real_key"),
        rpc("ping", json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_token_is_401_not_accepted_for_mcp() {
    let app = test_app("auth-session-token").await;
    // オーナー決定4: セッション token(`bh_`で始まらない)は不可。
    let (status, _) = mcp_post(&app.router, Some(&app.admin_token), rpc("ping", json!({}))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// 3. read-only キー: list_tags/read_tag_values は使えるが write は
//    missing_write_scope の isError。
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_only_key_can_list_and_read_but_not_write() {
    let app = test_app("read-only").await;
    let sim = Simulator::start().await;
    let (_tag_id, external_name) = make_tag(
        &app,
        "line1",
        sim.addr.port(),
        "temp01",
        "D100",
        "u16",
        true,
        true,
    )
    .await;
    let key = issue_key(&app.router, &app.admin_token, "reader", &["read"]).await;

    let (status, body) =
        mcp_post(&app.router, Some(&key), tools_call("list_tags", json!({}))).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false);
    let tags = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(tags.contains(&external_name), "{tags}");

    let (status, body) = mcp_post(
        &app.router,
        Some(&key),
        tools_call("read_tag_values", json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false);

    let (status, body) = mcp_post(
        &app.router,
        Some(&key),
        tools_call(
            "write_tag_value",
            json!({ "tag": external_name, "value": 1 }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], true);
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("missing_write_scope"), "{text}");

    sim.stop();
}

// ---------------------------------------------------------------------------
// 4. per-tag read フィルタ: can_read_value が false のタグは list_tags/
//    read_tag_values に出ない。
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn per_tag_read_scope_filters_both_list_tags_and_read_tag_values() {
    let app = test_app("per-tag-read").await;
    let sim = Simulator::start().await;
    let (_t1, visible) = make_tag(
        &app,
        "line1",
        sim.addr.port(),
        "temp01",
        "D100",
        "u16",
        true,
        true,
    )
    .await;
    // 同じグループの2本目のタグ(別アドレス) - read スコープを1本目にしか
    // 与えないので、こちらは見えないはず。
    let hidden_tag = TagService::new(app.pool.clone())
        .create(tag_input(
            "temp02",
            CollectionGroupService::new(app.pool.clone())
                .list(ListParams::default())
                .await
                .unwrap()
                .rows[0]
                .id,
            "D200",
            "u16",
            true,
            true,
        ))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild");
    let hidden = format!("line1.fast.{}", hidden_tag.name);

    let key = issue_key(
        &app.router,
        &app.admin_token,
        "scoped",
        &[&format!("read:{visible}")],
    )
    .await;

    let (_status, body) =
        mcp_post(&app.router, Some(&key), tools_call("list_tags", json!({}))).await;
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains(&visible), "{text}");
    assert!(!text.contains(&hidden), "{text}");

    let (_status, body) = mcp_post(
        &app.router,
        Some(&key),
        tools_call("read_tag_values", json!({})),
    )
    .await;
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains(&visible), "{text}");
    assert!(!text.contains(&hidden), "{text}");

    sim.stop();
}

// ---------------------------------------------------------------------------
// 5. write スコープ付きキー・ロックダウン前: ゲートに到達していることを
//    not-found / not-writable の拒否と、成功系(シミュレータへの着弾)の
//    両方で確認する。
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_before_lockdown_unknown_tag_reaches_the_gate_as_not_found() {
    let app = test_app("write-not-found").await;
    app.write_control.enable();
    let key = issue_key(
        &app.router,
        &app.admin_token,
        "writer",
        &["write:nope.nope.nope"],
    )
    .await;

    let (status, body) = mcp_post(
        &app.router,
        Some(&key),
        tools_call(
            "write_tag_value",
            json!({ "tag": "nope.nope.nope", "value": 1 }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], true);
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("not_found"), "{text}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_before_lockdown_not_writable_tag_reaches_the_gate() {
    let app = test_app("write-not-writable").await;
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
    app.write_control.enable();
    let key = issue_key(
        &app.router,
        &app.admin_token,
        "writer",
        &["write:line1.fast.temp01"],
    )
    .await;

    let (status, body) = mcp_post(
        &app.router,
        Some(&key),
        tools_call(
            "write_tag_value",
            json!({ "tag": external_name, "value": 1 }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], true);
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("not_writable"), "{text}");
    sim.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_before_lockdown_success_lands_on_the_wire_through_execute_write() {
    let app = test_app("write-success").await;
    let sim = Simulator::start().await;
    let (tag_id, external_name) = make_tag(
        &app,
        "line1",
        sim.addr.port(),
        "temp01",
        "D100",
        "u16",
        true,
        true,
    )
    .await;
    app.write_control.enable();
    let key = issue_key(
        &app.router,
        &app.admin_token,
        "writer",
        &["write:line1.fast.temp01"],
    )
    .await;

    let (status, body) = mcp_post(
        &app.router,
        Some(&key),
        tools_call(
            "write_tag_value",
            json!({ "tag": external_name, "value": 4321 }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("\"result\":\"ok\""), "{text}");

    assert_eq!(
        sim.get_word(SlmpDevice::D, 100),
        4321,
        "MCP write must go through the same execute_write gate as REST and land on the wire"
    );

    let tag_key = format!("tag:{tag_id}");
    assert!(
        wait_until(Duration::from_secs(10), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get(&tag_key))
                .map(|s| s.value)
                == Some(Some(4321.0))
        })
        .await,
        "collection should read back the value MCP just wrote"
    );

    sim.stop();
}

/// T20 ①a (docs/banto-hub-t20-design.md §3.1): `write_tag_value` は文字列
/// タグへの書き込みでも(数値と同様に)`execute_write`をそのまま通り、
/// シミュレータのワイヤへ UTF-8 バイト列が届く。MCP がゲートを独自実装
/// していない証拠(既存の数値テストと対になる)。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_before_lockdown_string_tag_success_lands_utf8_bytes_on_the_wire() {
    let app = test_app("write-success-string").await;
    let sim = Simulator::start().await;
    let (_tag_id, external_name) =
        make_string_tag(&app, "line1", sim.addr.port(), "recipe", "D3000", 5).await;
    app.write_control.enable();
    let key = issue_key(
        &app.router,
        &app.admin_token,
        "writer",
        &["write:line1.fast.recipe"],
    )
    .await;

    let (status, body) = mcp_post(
        &app.router,
        Some(&key),
        tools_call(
            "write_tag_value",
            json!({ "tag": external_name, "value": "テスト" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("\"result\":\"ok\""), "{text}");

    let landed: Vec<u8> = (0..5)
        .flat_map(|i| sim.get_word(SlmpDevice::D, 3000 + i).to_le_bytes())
        .collect();
    let mut expected = "テスト".as_bytes().to_vec();
    expected.resize(10, 0x00);
    assert_eq!(
        landed, expected,
        "MCP string write must go through the same execute_write gate as REST and land on the wire"
    );

    sim.stop();
}

// ---------------------------------------------------------------------------
// 6. ロックダウン後(最重要): execute_write を一切呼ばずアドバイザリのみを
//    返し、実際に値が書き込まれないことを固定する。
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_after_lockdown_is_advisory_only_and_never_calls_execute_write() {
    let app = test_app("write-lockdown").await;
    let sim = Simulator::start().await;
    let (tag_id, external_name) = make_tag(
        &app,
        "line1",
        sim.addr.port(),
        "temp01",
        "D100",
        "u16",
        true,
        true,
    )
    .await;
    app.write_control.enable();
    let key = issue_key(
        &app.router,
        &app.admin_token,
        "writer",
        &["write:line1.fast.temp01"],
    )
    .await;

    app.commissioning
        .lock_down()
        .await
        .expect("lock_down (an admin account already exists)");
    assert!(app.commissioning.is_locked_down());

    let audit_rows_before = write_audit_row_count(&app).await;
    let wire_value_before = sim.get_word(SlmpDevice::D, 100);

    let (status, body) = mcp_post(
        &app.router,
        Some(&key),
        tools_call(
            "write_tag_value",
            json!({ "tag": external_name, "value": 9999 }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(
        body["result"]["isError"], true,
        "locked-down write must be an advisory (isError), never a real write: {body:?}"
    );
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("ロックダウン"), "{text}");
    assert!(text.contains(&external_name), "{text}");
    assert!(text.contains("9999"), "{text}");

    // 決定的な証拠: `execute_write`(gate 8)を呼んでいれば必ず
    // write_audit 行が増える(log-before-write)。増えていない = 呼んでいない。
    assert_eq!(
        write_audit_row_count(&app).await,
        audit_rows_before,
        "execute_write must not run once locked down (no new write_audit row)"
    );
    // 決定的な証拠その2: 実機(シミュレータ)の値も変化しない。
    assert_eq!(
        sim.get_word(SlmpDevice::D, 100),
        wire_value_before,
        "the simulated PLC register must be untouched once locked down"
    );

    let _ = tag_id;
    sim.stop();
}

// ---------------------------------------------------------------------------
// 7. get_server_status: lockedDown を必ず含む。
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_server_status_reports_locked_down() {
    let app = test_app("status-locked-down").await;
    let key = issue_key(&app.router, &app.admin_token, "reader", &["read"]).await;

    let (_status, body) = mcp_post(
        &app.router,
        Some(&key),
        tools_call("get_server_status", json!({})),
    )
    .await;
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let status: Value = serde_json::from_str(text).unwrap();
    assert_eq!(status["lockedDown"], false);

    app.commissioning.lock_down().await.expect("lock_down");

    let (_status, body) = mcp_post(
        &app.router,
        Some(&key),
        tools_call("get_server_status", json!({})),
    )
    .await;
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let status: Value = serde_json::from_str(text).unwrap();
    assert_eq!(status["lockedDown"], true);
}

// ---------------------------------------------------------------------------
// 8. write_recipe (T20 機能③b、レシピ一括書き込み): `write_tag_value`と同じ
//    安全ポリシー(ロックダウン連動・per-tag write スコープ)を
//    `crate::write_path::execute_write_batch`への委譲だけで満たすことを
//    確認する。
// ---------------------------------------------------------------------------

/// `write.rs`/`t20_batch_write.rs`と同じ `make_tag` は接続ごと新規に作るため、
/// レシピテストで「同じグループにもう1本タグが要る」場合はこのヘルパーで
/// 直接 `TagService` から足す(`per_tag_read_scope_filters_...`と同型)。
async fn add_tag_to_first_group(
    app: &TestApp,
    conn_name: &str,
    tag_name: &str,
    address: &str,
    data_type: &str,
    writable: bool,
) -> String {
    let group_id = CollectionGroupService::new(app.pool.clone())
        .list(ListParams::default())
        .await
        .unwrap()
        .rows[0]
        .id;
    let tag = TagService::new(app.pool.clone())
        .create(tag_input(
            tag_name, group_id, address, data_type, writable, true,
        ))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild");
    format!("{conn_name}.fast.{}", tag.name)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_recipe_before_lockdown_writes_multiple_tags_through_execute_write_batch() {
    let app = test_app("recipe-success").await;
    let sim = Simulator::start().await;
    let (_tag_a, name_a) = make_tag(
        &app,
        "line1",
        sim.addr.port(),
        "a",
        "D100",
        "u16",
        true,
        true,
    )
    .await;
    let name_b = add_tag_to_first_group(&app, "line1", "b", "D101", "u16", true).await;

    app.write_control.enable();
    let key = issue_key(
        &app.router,
        &app.admin_token,
        "writer",
        &[&format!("write:{name_a}"), &format!("write:{name_b}")],
    )
    .await;

    let (status, body) = mcp_post(
        &app.router,
        Some(&key),
        tools_call(
            "write_recipe",
            json!({ "writes": [
                { "tag": name_a, "value": 111 },
                { "tag": name_b, "value": 222 },
            ] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["applied"], 2, "{payload:?}");
    let writes = payload["writes"].as_array().unwrap();
    assert_eq!(writes.len(), 2);
    assert_eq!(writes[0]["tag"], name_a);
    assert_eq!(writes[0]["ok"], true);
    assert_eq!(writes[1]["tag"], name_b);
    assert_eq!(writes[1]["ok"], true);

    assert_eq!(
        sim.get_word(SlmpDevice::D, 100),
        111,
        "MCP write_recipe must go through execute_write_batch and land on the wire"
    );
    assert_eq!(sim.get_word(SlmpDevice::D, 101), 222);

    sim.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_recipe_after_lockdown_is_advisory_only_and_never_calls_execute_write_batch() {
    let app = test_app("recipe-lockdown").await;
    let sim = Simulator::start().await;
    let (_tag_id, external_name) = make_tag(
        &app,
        "line1",
        sim.addr.port(),
        "temp01",
        "D100",
        "u16",
        true,
        true,
    )
    .await;
    app.write_control.enable();
    let key = issue_key(
        &app.router,
        &app.admin_token,
        "writer",
        &["write:line1.fast.temp01"],
    )
    .await;

    app.commissioning
        .lock_down()
        .await
        .expect("lock_down (an admin account already exists)");
    assert!(app.commissioning.is_locked_down());

    let audit_rows_before = write_audit_row_count(&app).await;
    let wire_value_before = sim.get_word(SlmpDevice::D, 100);

    let (status, body) = mcp_post(
        &app.router,
        Some(&key),
        tools_call(
            "write_recipe",
            json!({ "writes": [ { "tag": external_name, "value": 9999 } ] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(
        body["result"]["isError"], true,
        "locked-down write_recipe must be an advisory (isError), never a real write: {body:?}"
    );
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("ロックダウン"), "{text}");
    assert!(text.contains(&external_name), "{text}");
    assert!(text.contains("9999"), "{text}");

    // 決定的な証拠: `execute_write_batch`(gate 8)を呼んでいれば必ず
    // write_audit 行が増える(log-before-write)。増えていない = 呼んでいない。
    assert_eq!(
        write_audit_row_count(&app).await,
        audit_rows_before,
        "execute_write_batch must not run once locked down (no new write_audit row)"
    );
    // 決定的な証拠その2: 実機(シミュレータ)の値も変化しない。
    assert_eq!(
        sim.get_word(SlmpDevice::D, 100),
        wire_value_before,
        "the simulated PLC register must be untouched once locked down"
    );

    sim.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_recipe_missing_scope_on_one_entry_rejects_the_whole_batch() {
    let app = test_app("recipe-missing-scope").await;
    let sim = Simulator::start().await;
    let (_tag_a, name_a) = make_tag(
        &app,
        "line1",
        sim.addr.port(),
        "a",
        "D100",
        "u16",
        true,
        true,
    )
    .await;
    let name_b = add_tag_to_first_group(&app, "line1", "b", "D101", "u16", true).await;

    app.write_control.enable();
    // キーは a にしか write スコープを持たない - b が事前段で足切りされる
    // ことを確認する。
    let key = issue_key(
        &app.router,
        &app.admin_token,
        "writer",
        &[&format!("write:{name_a}")],
    )
    .await;

    let audit_before = write_audit_row_count(&app).await;

    let (status, body) = mcp_post(
        &app.router,
        Some(&key),
        tools_call(
            "write_recipe",
            json!({ "writes": [
                { "tag": name_a, "value": 1 },
                { "tag": name_b, "value": 2 },
            ] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], true, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("missing_write_scope"), "{text}");
    assert!(text.contains(&name_b), "{text}");

    assert_eq!(
        write_audit_row_count(&app).await,
        audit_before,
        "execute_write_batch must not run when one entry is missing write scope"
    );
    assert_eq!(sim.get_word(SlmpDevice::D, 100), 0, "tag a must not land");
    assert_eq!(sim.get_word(SlmpDevice::D, 101), 0, "tag b must not land");

    sim.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_recipe_one_bad_entry_aborts_the_whole_batch_with_no_audit_rows_and_no_wire_writes() {
    let app = test_app("recipe-all-or-nothing").await;
    let sim = Simulator::start().await;
    let (_tag_a, name_a) = make_tag(
        &app,
        "line1",
        sim.addr.port(),
        "a",
        "D100",
        "u16",
        true,
        true,
    )
    .await;
    // not-writable タグ - gate 2 で NG。
    let name_bad = add_tag_to_first_group(&app, "line1", "bad", "D101", "u16", false).await;

    app.write_control.enable();
    let key = issue_key(
        &app.router,
        &app.admin_token,
        "writer",
        &[&format!("write:{name_a}"), &format!("write:{name_bad}")],
    )
    .await;

    let audit_before = write_audit_row_count(&app).await;

    let (status, body) = mcp_post(
        &app.router,
        Some(&key),
        tools_call(
            "write_recipe",
            json!({ "writes": [
                { "tag": name_a, "value": 111 },
                { "tag": name_bad, "value": 1 },
            ] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(
        payload["applied"], 0,
        "the pre-gate all-or-nothing abort must apply to zero entries: {payload:?}"
    );
    let writes = payload["writes"].as_array().unwrap();
    assert_eq!(writes[0]["tag"], name_a);
    assert_eq!(writes[0]["ok"], false);
    assert_eq!(writes[0]["error"], "batch_aborted");
    assert_eq!(writes[1]["tag"], name_bad);
    assert_eq!(writes[1]["ok"], false);
    assert_eq!(writes[1]["error"], "not_writable");

    // 決定的固定: 事前ゲート all-or-nothing により1件も監査 insert されず
    // (suppressed 系すら発生しない)、シミュレータのレジスタも初期値のまま。
    assert_eq!(
        write_audit_row_count(&app).await,
        audit_before,
        "no audit row should be inserted when the batch is aborted pre-gate"
    );
    assert_eq!(sim.get_word(SlmpDevice::D, 100), 0, "tag a must not land");

    sim.stop();
}

// ---------------------------------------------------------------------------
// 9. T21 S1-b（docs/banto-hub-t21-design.md §3・§4）: 構成補助ツール（接続
//    CRUD）。`admin` スコープ必須・不可逆操作(delete)は confirm 必須・
//    全操作を origin="mcp" で監査する、の3点を固定する。
// ---------------------------------------------------------------------------

async fn plc_connections_row_count(app: &TestApp) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM plc_connections")
        .fetch_one(&app.pool)
        .await
        .unwrap()
}

/// 最新の audit_log 1行の1列を返す(`id DESC LIMIT 1`) - `AUTOINCREMENT`の`id`は
/// 挿入順と一致する(`AuditLogService::prune`のdoc commentと同じ前提)。
/// `column`は固定文字列(呼び出し元はこのファイル内のみ)なので SQL 文字列
/// 補間で問題ない。
async fn latest_audit_column(app: &TestApp, column: &str) -> Option<String> {
    // AssertSqlSafe: `column` is always a fixed literal passed by call sites
    // in this file (never external input) - same pattern as
    // `banto_tags::plc_connection`'s `COLUMNS`-interpolating queries.
    // `fetch_optional`: audit_log が空(行が無い)場合に `RowNotFound` で
    // panic しないよう `None` を返す - 列自体が NULL 許容なので
    // `Option<Option<String>>` になるが `flatten` して意味を一致させる。
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT {column} FROM audit_log ORDER BY id DESC LIMIT 1"
    )))
    .fetch_optional(&app.pool)
    .await
    .unwrap()
    .flatten()
}

/// `POST /api/collection/start` を叩いて controller を `Running` にする -
/// `crate::rest`の`plc_connections_create_while_running_is_accepted_and_queued`
/// (REST 側の同種テスト)と同じやり方。
async fn start_collection(router: &Router, admin_token: &str) {
    let response = router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/start")
                .header("Authorization", format!("Bearer {admin_token}"))
                .header("X-Banto-Client", "banto")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_tools_without_admin_scope_are_rejected_and_change_nothing() {
    let app = test_app("config-no-admin").await;
    // read/write は持つが admin は持たないキー。
    let key = issue_key(
        &app.router,
        &app.admin_token,
        "reader-writer",
        &["read", "write:line1.fast.temp01"],
    )
    .await;
    let before = plc_connections_row_count(&app).await;

    let (status, body) = mcp_post(
        &app.router,
        Some(&key),
        tools_call(
            "create_connection",
            json!({ "name": "line1", "host": "127.0.0.1", "port": 15022 }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], true);
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("missing_admin_scope"), "{text}");
    assert_eq!(
        plc_connections_row_count(&app).await,
        before,
        "create_connection without admin scope must not touch the DB"
    );

    let (status, body) = mcp_post(
        &app.router,
        Some(&key),
        tools_call("delete_connection", json!({ "id": 1, "confirm": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], true);
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("missing_admin_scope"), "{text}");
    assert_eq!(
        plc_connections_row_count(&app).await,
        before,
        "delete_connection without admin scope must not touch the DB"
    );

    let (status, body) = mcp_post(
        &app.router,
        Some(&key),
        tools_call("list_connections", json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], true);
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("missing_admin_scope"), "{text}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_connections_returns_created_connections() {
    let app = test_app("config-list").await;
    let admin_key = issue_key(&app.router, &app.admin_token, "admin-key", &["admin"]).await;
    PlcConnectionService::new(app.pool.clone())
        .create(slmp_conn_input("line1", 15022))
        .await
        .unwrap();

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call("list_connections", json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    let connections = payload["connections"].as_array().unwrap();
    assert_eq!(connections.len(), 1, "{payload:?}");
    assert_eq!(connections[0]["name"], "line1");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_connection_with_admin_scope_while_stopped_creates_and_audits() {
    let app = test_app("config-create").await;
    let admin_key = issue_key(&app.router, &app.admin_token, "admin-key", &["admin"]).await;
    let before_rows = plc_connections_row_count(&app).await;
    let audit_count_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&app.pool)
        .await
        .unwrap();

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call(
            "create_connection",
            json!({ "name": "line1", "host": "127.0.0.1", "port": 15022, "protocol": "slmp" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["created"]["name"], "line1");

    assert_eq!(plc_connections_row_count(&app).await, before_rows + 1);

    let audit_count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(
        audit_count_after,
        audit_count_before + 1,
        "create_connection via MCP must add exactly one audit_log row"
    );
    assert_eq!(
        latest_audit_column(&app, "actor_username").await.as_deref(),
        Some("admin-key")
    );
    assert_eq!(
        latest_audit_column(&app, "actor_role").await.as_deref(),
        Some("api_key")
    );
    assert_eq!(
        latest_audit_column(&app, "action").await.as_deref(),
        Some("create")
    );
    assert_eq!(
        latest_audit_column(&app, "resource").await.as_deref(),
        Some("plc_connections")
    );
    assert_eq!(
        latest_audit_column(&app, "origin").await.as_deref(),
        Some("mcp")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_connection_without_confirm_is_rejected_and_connection_remains() {
    let app = test_app("config-delete-no-confirm").await;
    let admin_key = issue_key(&app.router, &app.admin_token, "admin-key", &["admin"]).await;
    let conn = PlcConnectionService::new(app.pool.clone())
        .create(slmp_conn_input("line1", 15022))
        .await
        .unwrap();

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call("delete_connection", json!({ "id": conn.id })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], true, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("confirm_required"), "{text}");

    assert_eq!(
        plc_connections_row_count(&app).await,
        1,
        "delete_connection without confirm must not delete the row"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_connection_with_confirm_deletes_and_audits() {
    let app = test_app("config-delete-confirm").await;
    let admin_key = issue_key(&app.router, &app.admin_token, "admin-key", &["admin"]).await;
    let conn = PlcConnectionService::new(app.pool.clone())
        .create(slmp_conn_input("line1", 15022))
        .await
        .unwrap();
    let audit_count_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&app.pool)
        .await
        .unwrap();

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call(
            "delete_connection",
            json!({ "id": conn.id, "confirm": true }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["deleted"], true);

    assert_eq!(
        plc_connections_row_count(&app).await,
        0,
        "delete_connection with confirm:true must delete the row"
    );

    let audit_count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(audit_count_after, audit_count_before + 1);
    assert_eq!(
        latest_audit_column(&app, "actor_username").await.as_deref(),
        Some("admin-key")
    );
    assert_eq!(
        latest_audit_column(&app, "actor_role").await.as_deref(),
        Some("api_key")
    );
    assert_eq!(
        latest_audit_column(&app, "action").await.as_deref(),
        Some("delete")
    );
    assert_eq!(
        latest_audit_column(&app, "resource").await.as_deref(),
        Some("plc_connections")
    );
    assert_eq!(
        latest_audit_column(&app, "origin").await.as_deref(),
        Some("mcp")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_connection_while_collection_running_is_queued_not_applied() {
    let app = test_app("config-create-running").await;
    let admin_key = issue_key(&app.router, &app.admin_token, "admin-key", &["admin"]).await;
    start_collection(&app.router, &app.admin_token).await;
    let before_rows = plc_connections_row_count(&app).await;
    let audit_count_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&app.pool)
        .await
        .unwrap();

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call(
            "create_connection",
            json!({ "name": "line-running", "host": "127.0.0.1", "port": 15022 }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["queued"], true, "{payload:?}");
    assert!(payload["pendingId"].is_number(), "{payload:?}");

    assert_eq!(
        plc_connections_row_count(&app).await,
        before_rows,
        "a queued create must not touch plc_connections directly"
    );
    let pending_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pending_changes")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(pending_count, 1);

    // 設計 §3.3: 構成操作は queued の場合も含めて全て監査する（回帰防止 -
    // `create_connection_with_admin_scope_while_stopped_creates_and_audits`
    // と同じ確認方法）。
    let audit_count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(
        audit_count_after,
        audit_count_before + 1,
        "queued create_connection via MCP must add exactly one audit_log row"
    );
    assert_eq!(
        latest_audit_column(&app, "origin").await.as_deref(),
        Some("mcp")
    );
    assert_eq!(
        latest_audit_column(&app, "actor_username").await.as_deref(),
        Some("admin-key")
    );
}

// ---------------------------------------------------------------------------
// 10. T21 S1-c（docs/banto-hub-t21-design.md §3・§4）: 構成補助ツール第二弾
//    （接続 update/test・グループ CRUD）。9節（S1-b・接続 create/delete）と
//    同じ3点を確認する - `admin` スコープ必須・不可逆操作(delete)は confirm
//    必須・全操作を origin="mcp" で監査する。
// ---------------------------------------------------------------------------

async fn collection_groups_row_count(app: &TestApp) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM collection_groups")
        .fetch_one(&app.pool)
        .await
        .unwrap()
}

/// テスト用に接続を1本作る(グループ系ツールの`plcConnectionId`用)。
async fn create_test_connection(app: &TestApp, name: &str, port: u16) -> i64 {
    PlcConnectionService::new(app.pool.clone())
        .create(slmp_conn_input(name, port))
        .await
        .unwrap()
        .id
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn group_and_update_connection_tools_without_admin_scope_are_rejected_and_change_nothing() {
    let app = test_app("s1c-no-admin").await;
    let key = issue_key(
        &app.router,
        &app.admin_token,
        "reader-writer",
        &["read", "write:line1.fast.temp01"],
    )
    .await;
    let conn_id = create_test_connection(&app, "line1", 15022).await;
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn_id, 100))
        .await
        .unwrap();
    let before_groups = collection_groups_row_count(&app).await;
    let before_conn_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM plc_connections")
        .fetch_one(&app.pool)
        .await
        .unwrap();

    for (name, arguments) in [
        (
            "update_connection",
            json!({ "id": conn_id, "name": "renamed", "host": "127.0.0.1", "port": 15022 }),
        ),
        (
            "create_group",
            json!({ "name": "new-group", "plcConnectionId": conn_id, "periodMs": 100 }),
        ),
        (
            "update_group",
            json!({ "id": group.id, "name": "renamed", "plcConnectionId": conn_id, "periodMs": 200 }),
        ),
        ("delete_group", json!({ "id": group.id, "confirm": true })),
        ("list_groups", json!({})),
    ] {
        let (status, body) = mcp_post(&app.router, Some(&key), tools_call(name, arguments)).await;
        assert_eq!(status, StatusCode::OK, "{name}: {body:?}");
        assert_eq!(body["result"]["isError"], true, "{name}: {body:?}");
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("missing_admin_scope"), "{name}: {text}");
    }

    assert_eq!(
        collection_groups_row_count(&app).await,
        before_groups,
        "none of the rejected tools may touch collection_groups"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM plc_connections")
            .fetch_one(&app.pool)
            .await
            .unwrap(),
        before_conn_count,
        "update_connection without admin scope must not touch plc_connections"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_groups_returns_created_groups() {
    let app = test_app("s1c-list-groups").await;
    let admin_key = issue_key(&app.router, &app.admin_token, "admin-key", &["admin"]).await;
    let conn_id = create_test_connection(&app, "line1", 15022).await;
    CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn_id, 100))
        .await
        .unwrap();

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call("list_groups", json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    let groups = payload["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 1, "{payload:?}");
    assert_eq!(groups[0]["name"], "fast");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_group_with_admin_scope_while_stopped_creates_and_audits() {
    let app = test_app("s1c-create-group").await;
    let admin_key = issue_key(&app.router, &app.admin_token, "admin-key", &["admin"]).await;
    let conn_id = create_test_connection(&app, "line1", 15022).await;
    let before_rows = collection_groups_row_count(&app).await;
    let audit_count_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&app.pool)
        .await
        .unwrap();

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call(
            "create_group",
            json!({ "name": "fast", "plcConnectionId": conn_id, "periodMs": 100 }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["created"]["name"], "fast");

    assert_eq!(collection_groups_row_count(&app).await, before_rows + 1);

    let audit_count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(
        audit_count_after,
        audit_count_before + 1,
        "create_group via MCP must add exactly one audit_log row"
    );
    assert_eq!(
        latest_audit_column(&app, "action").await.as_deref(),
        Some("create")
    );
    assert_eq!(
        latest_audit_column(&app, "resource").await.as_deref(),
        Some("collection_groups")
    );
    assert_eq!(
        latest_audit_column(&app, "origin").await.as_deref(),
        Some("mcp")
    );
    assert_eq!(
        latest_audit_column(&app, "actor_username").await.as_deref(),
        Some("admin-key")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_group_with_admin_scope_updates_and_audits() {
    let app = test_app("s1c-update-group").await;
    let admin_key = issue_key(&app.router, &app.admin_token, "admin-key", &["admin"]).await;
    let conn_id = create_test_connection(&app, "line1", 15022).await;
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn_id, 100))
        .await
        .unwrap();
    let audit_count_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&app.pool)
        .await
        .unwrap();

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call(
            "update_group",
            json!({
                "id": group.id,
                "name": "fast-renamed",
                "plcConnectionId": conn_id,
                "periodMs": 200,
                "enabled": true,
                "defaultWritable": true,
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["updated"]["name"], "fast-renamed");
    assert_eq!(payload["updated"]["periodMs"], 200);

    let stored = CollectionGroupService::new(app.pool.clone())
        .get(group.id)
        .await
        .unwrap();
    assert_eq!(stored.name, "fast-renamed");
    assert_eq!(stored.period_ms, 200);

    let audit_count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(audit_count_after, audit_count_before + 1);
    assert_eq!(
        latest_audit_column(&app, "action").await.as_deref(),
        Some("update")
    );
    assert_eq!(
        latest_audit_column(&app, "resource").await.as_deref(),
        Some("collection_groups")
    );
    assert_eq!(
        latest_audit_column(&app, "origin").await.as_deref(),
        Some("mcp")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_connection_with_admin_scope_updates_and_audits() {
    let app = test_app("s1c-update-conn").await;
    let admin_key = issue_key(&app.router, &app.admin_token, "admin-key", &["admin"]).await;
    let conn_id = create_test_connection(&app, "line1", 15022).await;
    let audit_count_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&app.pool)
        .await
        .unwrap();

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call(
            "update_connection",
            json!({
                "id": conn_id,
                "name": "line1-renamed",
                "protocol": "slmp",
                "host": "127.0.0.1",
                "port": 15099,
                "unitId": 1,
                "enabled": true,
                "simulation": false,
                "wordOrder": "low_high",
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["updated"]["name"], "line1-renamed");
    assert_eq!(payload["updated"]["port"], 15099);

    let stored = PlcConnectionService::new(app.pool.clone())
        .get(conn_id)
        .await
        .unwrap();
    assert_eq!(stored.name, "line1-renamed");
    assert_eq!(stored.port, 15099);

    let audit_count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(audit_count_after, audit_count_before + 1);
    assert_eq!(
        latest_audit_column(&app, "action").await.as_deref(),
        Some("update")
    );
    assert_eq!(
        latest_audit_column(&app, "resource").await.as_deref(),
        Some("plc_connections")
    );
    assert_eq!(
        latest_audit_column(&app, "origin").await.as_deref(),
        Some("mcp")
    );
}

/// Copilot 指摘（PR #268）の回帰防止:`update_connection`は全項目指定の
/// PUT 置換だが、`PlcConnectionPayload`は`#[serde(default = ...)]`を
/// 持つため、省略フィールドをサーバーが拒否しないと黙って既定値で
/// 上書きされてしまう（例: `enabled`省略で既定 true に戻る）。ここでは
/// `enabled`を省いた入力が`missing_fields`で拒否され、対象行が一切
/// 変化しないことを確認する。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_connection_missing_field_is_rejected_and_row_unchanged() {
    let app = test_app("s1c-update-conn-missing-field").await;
    let admin_key = issue_key(&app.router, &app.admin_token, "admin-key", &["admin"]).await;
    let conn_id = create_test_connection(&app, "line1", 15022).await;
    let before = PlcConnectionService::new(app.pool.clone())
        .get(conn_id)
        .await
        .unwrap();
    let audit_count_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&app.pool)
        .await
        .unwrap();

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call(
            "update_connection",
            json!({
                "id": conn_id,
                "name": "line1-renamed",
                "protocol": "slmp",
                "host": "127.0.0.1",
                "port": 15099,
                "unitId": 1,
                // "enabled" を意図的に省略。
                "simulation": false,
                "wordOrder": "low_high",
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], true, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("missing_fields"), "{text}");
    assert!(text.contains("enabled"), "{text}");

    let after = PlcConnectionService::new(app.pool.clone())
        .get(conn_id)
        .await
        .unwrap();
    assert_eq!(after.name, before.name, "row must not change");
    assert_eq!(after.port, before.port, "row must not change");
    assert_eq!(after.enabled, before.enabled, "row must not change");

    let audit_count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(
        audit_count_after, audit_count_before,
        "rejected update must not add an audit_log row"
    );
}

/// Copilot 指摘（PR #268）の回帰防止:`update_group`版。`enabled`を省いた
/// 入力が`missing_fields`で拒否され、対象行が一切変化しないことを確認
/// する（[`update_connection_missing_field_is_rejected_and_row_unchanged`]
/// と同型）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_group_missing_field_is_rejected_and_row_unchanged() {
    let app = test_app("s1c-update-group-missing-field").await;
    let admin_key = issue_key(&app.router, &app.admin_token, "admin-key", &["admin"]).await;
    let conn_id = create_test_connection(&app, "line1", 15022).await;
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn_id, 100))
        .await
        .unwrap();
    let audit_count_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&app.pool)
        .await
        .unwrap();

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call(
            "update_group",
            json!({
                "id": group.id,
                "name": "fast-renamed",
                "plcConnectionId": conn_id,
                "periodMs": 200,
                // "enabled" を意図的に省略。
                "defaultWritable": true,
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], true, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("missing_fields"), "{text}");
    assert!(text.contains("enabled"), "{text}");

    let stored = CollectionGroupService::new(app.pool.clone())
        .get(group.id)
        .await
        .unwrap();
    assert_eq!(stored.name, "fast", "row must not change");
    assert_eq!(stored.period_ms, 100, "row must not change");

    let audit_count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(
        audit_count_after, audit_count_before,
        "rejected update must not add an audit_log row"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_group_without_confirm_is_rejected_and_group_remains() {
    let app = test_app("s1c-delete-group-no-confirm").await;
    let admin_key = issue_key(&app.router, &app.admin_token, "admin-key", &["admin"]).await;
    let conn_id = create_test_connection(&app, "line1", 15022).await;
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn_id, 100))
        .await
        .unwrap();

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call("delete_group", json!({ "id": group.id })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], true, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("confirm_required"), "{text}");

    assert_eq!(
        collection_groups_row_count(&app).await,
        1,
        "delete_group without confirm must not delete the row"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_group_with_confirm_cascades_and_audits() {
    let app = test_app("s1c-delete-group-confirm").await;
    let admin_key = issue_key(&app.router, &app.admin_token, "admin-key", &["admin"]).await;
    let conn_id = create_test_connection(&app, "line1", 15022).await;
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn_id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("t1", group.id, "D100", "u16", false, true))
        .await
        .unwrap();
    let audit_count_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&app.pool)
        .await
        .unwrap();

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call("delete_group", json!({ "id": group.id, "confirm": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["deleted"], true);
    assert_eq!(payload["cascade"]["deletedTags"], 1);

    assert_eq!(
        collection_groups_row_count(&app).await,
        0,
        "delete_group with confirm:true must delete the row"
    );
    let tag_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tags")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(
        tag_count, 0,
        "cascade delete must also remove the group's tags"
    );

    let audit_count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(audit_count_after, audit_count_before + 1);
    assert_eq!(
        latest_audit_column(&app, "action").await.as_deref(),
        Some("delete")
    );
    assert_eq!(
        latest_audit_column(&app, "resource").await.as_deref(),
        Some("collection_groups")
    );
    assert_eq!(
        latest_audit_column(&app, "origin").await.as_deref(),
        Some("mcp")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_group_while_collection_running_is_queued_not_applied() {
    let app = test_app("s1c-create-group-running").await;
    let admin_key = issue_key(&app.router, &app.admin_token, "admin-key", &["admin"]).await;
    let conn_id = create_test_connection(&app, "line1", 15022).await;
    start_collection(&app.router, &app.admin_token).await;
    let before_rows = collection_groups_row_count(&app).await;
    let audit_count_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&app.pool)
        .await
        .unwrap();

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call(
            "create_group",
            json!({ "name": "fast-running", "plcConnectionId": conn_id, "periodMs": 100 }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["queued"], true, "{payload:?}");
    assert!(payload["pendingId"].is_number(), "{payload:?}");

    assert_eq!(
        collection_groups_row_count(&app).await,
        before_rows,
        "a queued create_group must not touch collection_groups directly"
    );
    let pending_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pending_changes")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(pending_count, 1);

    let audit_count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(
        audit_count_after,
        audit_count_before + 1,
        "queued create_group via MCP must add exactly one audit_log row"
    );
    assert_eq!(
        latest_audit_column(&app, "origin").await.as_deref(),
        Some("mcp")
    );
    assert_eq!(
        latest_audit_column(&app, "resource").await.as_deref(),
        Some("collection_groups")
    );
}

// ---------------------------------------------------------------------------
// 11. test_connection（T12 の疎通確認を MCP から呼べるようにしたもの） -
//    `tests/t12_connection_test.rs`の Modbus 成功/失敗ケースと同じ検証方法を
//    流用する(疎通確認本体は`crate::rest::run_plc_connection_test`を共有する
//    だけなので、ここでは「MCP 経由でも同じ結果が返ること」だけを見ればよい)。
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_connection_reports_ok_for_a_reachable_modbus_simulator() {
    let app = test_app("s1c-test-conn-ok").await;
    let admin_key = issue_key(&app.router, &app.admin_token, "admin-key", &["admin"]).await;
    let sim = ModbusSimulator::start().await;

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call(
            "test_connection",
            json!({
                "protocol": "modbus-tcp",
                "host": "127.0.0.1",
                "port": sim.addr.port(),
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["ok"], true, "{payload:?}");
    assert!(payload["error"].is_null(), "{payload:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_connection_reports_failure_for_an_unreachable_port() {
    let app = test_app("s1c-test-conn-fail").await;
    let admin_key = issue_key(&app.router, &app.admin_token, "admin-key", &["admin"]).await;
    let closed_port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.local_addr().expect("local_addr").port()
    };

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call(
            "test_connection",
            json!({
                "protocol": "modbus-tcp",
                "host": "127.0.0.1",
                "port": closed_port,
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["ok"], false, "{payload:?}");
    let kind = payload["error"]["kind"].as_str().expect("error.kind");
    assert!(
        kind == "tcp" || kind == "timeout",
        "expected tcp or timeout, got {kind} ({payload:?})"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_connection_without_admin_scope_is_rejected() {
    let app = test_app("s1c-test-conn-no-admin").await;
    let key = issue_key(
        &app.router,
        &app.admin_token,
        "reader-writer",
        &["read", "write:line1.fast.temp01"],
    )
    .await;

    let (status, body) = mcp_post(
        &app.router,
        Some(&key),
        tools_call(
            "test_connection",
            json!({ "protocol": "modbus-tcp", "host": "127.0.0.1", "port": 1 }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], true, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("missing_admin_scope"), "{text}");
}

// ---------------------------------------------------------------------------
// 12. T21 S1-d（docs/banto-hub-t21-design.md §3・§4）: 構成補助ツール第三弾
//    （タグ CRUD）。9節・10節（S1-b/S1-c）と同じ3点を確認する - `admin`
//    スコープ必須・不可逆操作(delete)は confirm 必須・全操作を
//    origin="mcp" で監査する。加えて `update_tag`固有の2点
//    （全項目必須の回帰防止・楽観ロックの revision_conflict）も確認する。
// ---------------------------------------------------------------------------

async fn tags_row_count(app: &TestApp) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM tags")
        .fetch_one(&app.pool)
        .await
        .unwrap()
}

/// [`tool_update_tag`]の`arguments`を組み立てる - `TagPayload`の wire
/// フィールド全部(`update_tag`が要求する全項目指定)を埋めた上で、
/// `expected_revision`が`Some`なら`expectedRevision`も足す。
fn full_tag_update_args(
    id: i64,
    group_id: i64,
    name: &str,
    address: &str,
    expected_revision: Option<i64>,
) -> Value {
    let mut args = json!({
        "id": id,
        "name": name,
        "collectionGroupId": group_id,
        "address": address,
        "dataType": "u16",
        "stringLength": null,
        "stringEncoding": "utf8",
        "rawLo": null,
        "rawHi": null,
        "engLo": null,
        "engHi": null,
        "unit": null,
        "decimals": 0,
        "thresholdH": null,
        "thresholdHh": null,
        "thresholdL": null,
        "thresholdLl": null,
        "enabled": true,
        "writable": true,
        "tagKind": "plc",
        "expression": null,
        "retain": false,
    });
    if let Some(revision) = expected_revision {
        args["expectedRevision"] = json!(revision);
    }
    args
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tag_tools_without_admin_scope_are_rejected_and_change_nothing() {
    let app = test_app("s1d-no-admin").await;
    let key = issue_key(
        &app.router,
        &app.admin_token,
        "reader-writer",
        &["read", "write:line1.fast.temp01"],
    )
    .await;
    let conn_id = create_test_connection(&app, "line1", 15022).await;
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn_id, 100))
        .await
        .unwrap();
    let tag = TagService::new(app.pool.clone())
        .create(tag_input("temp01", group.id, "D100", "u16", false, true))
        .await
        .unwrap();
    let before_rows = tags_row_count(&app).await;

    for (name, arguments) in [
        ("get_tag", json!({ "id": tag.id })),
        (
            "create_tag",
            json!({
                "name": "temp02",
                "collectionGroupId": group.id,
                "address": "D200",
                "dataType": "u16",
            }),
        ),
        (
            "update_tag",
            full_tag_update_args(tag.id, group.id, "renamed", "D300", None),
        ),
        ("delete_tag", json!({ "id": tag.id, "confirm": true })),
    ] {
        let (status, body) = mcp_post(&app.router, Some(&key), tools_call(name, arguments)).await;
        assert_eq!(status, StatusCode::OK, "{name}: {body:?}");
        assert_eq!(body["result"]["isError"], true, "{name}: {body:?}");
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("missing_admin_scope"), "{name}: {text}");
    }

    assert_eq!(
        tags_row_count(&app).await,
        before_rows,
        "none of the rejected tools may touch tags"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_tag_returns_all_fields_including_revision() {
    let app = test_app("s1d-get-tag").await;
    let admin_key = issue_key(&app.router, &app.admin_token, "admin-key", &["admin"]).await;
    let conn_id = create_test_connection(&app, "line1", 15022).await;
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn_id, 100))
        .await
        .unwrap();
    let tag = TagService::new(app.pool.clone())
        .create(tag_input("temp01", group.id, "D100", "u16", true, true))
        .await
        .unwrap();

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call("get_tag", json!({ "id": tag.id })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["tag"]["id"], tag.id);
    assert_eq!(payload["tag"]["name"], "temp01");
    assert_eq!(payload["tag"]["address"], "D100");
    assert_eq!(payload["tag"]["writable"], true);
    assert_eq!(payload["tag"]["revision"], 1, "{payload:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_tag_with_admin_scope_while_stopped_creates_and_audits() {
    let app = test_app("s1d-create-tag").await;
    let admin_key = issue_key(&app.router, &app.admin_token, "admin-key", &["admin"]).await;
    let conn_id = create_test_connection(&app, "line1", 15022).await;
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn_id, 100))
        .await
        .unwrap();
    let before_rows = tags_row_count(&app).await;
    let audit_count_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&app.pool)
        .await
        .unwrap();

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call(
            "create_tag",
            json!({
                "name": "temp01",
                "collectionGroupId": group.id,
                "address": "D100",
                "dataType": "u16",
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["created"]["name"], "temp01");

    assert_eq!(tags_row_count(&app).await, before_rows + 1);

    let audit_count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(
        audit_count_after,
        audit_count_before + 1,
        "create_tag via MCP must add exactly one audit_log row"
    );
    assert_eq!(
        latest_audit_column(&app, "actor_username").await.as_deref(),
        Some("admin-key")
    );
    assert_eq!(
        latest_audit_column(&app, "actor_role").await.as_deref(),
        Some("api_key")
    );
    assert_eq!(
        latest_audit_column(&app, "action").await.as_deref(),
        Some("create")
    );
    assert_eq!(
        latest_audit_column(&app, "resource").await.as_deref(),
        Some("tags")
    );
    assert_eq!(
        latest_audit_column(&app, "origin").await.as_deref(),
        Some("mcp")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_tag_with_admin_scope_updates_and_audits() {
    let app = test_app("s1d-update-tag").await;
    let admin_key = issue_key(&app.router, &app.admin_token, "admin-key", &["admin"]).await;
    let conn_id = create_test_connection(&app, "line1", 15022).await;
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn_id, 100))
        .await
        .unwrap();
    let tag = TagService::new(app.pool.clone())
        .create(tag_input("temp01", group.id, "D100", "u16", false, true))
        .await
        .unwrap();
    let audit_count_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&app.pool)
        .await
        .unwrap();

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call(
            "update_tag",
            full_tag_update_args(tag.id, group.id, "temp01-renamed", "D200", None),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["updated"]["name"], "temp01-renamed");
    assert_eq!(payload["updated"]["address"], "D200");

    let stored = TagService::new(app.pool.clone()).get(tag.id).await.unwrap();
    assert_eq!(stored.name, "temp01-renamed");
    assert_eq!(stored.address, "D200");
    assert!(stored.writable, "writable:true must be applied");

    let audit_count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(audit_count_after, audit_count_before + 1);
    assert_eq!(
        latest_audit_column(&app, "action").await.as_deref(),
        Some("update")
    );
    assert_eq!(
        latest_audit_column(&app, "resource").await.as_deref(),
        Some("tags")
    );
    assert_eq!(
        latest_audit_column(&app, "origin").await.as_deref(),
        Some("mcp")
    );
}

/// [`update_connection_missing_field_is_rejected_and_row_unchanged`]の
/// タグ版 - `update_tag`は全項目指定の PUT 置換だが、`TagPayload`は
/// `#[serde(default)]`を多数持つため、省略フィールドをサーバーが拒否
/// しないと黙って既定値で上書きされてしまう(回帰防止)。ここでは
/// `enabled`を省いた入力が`missing_fields`で拒否され、対象行が一切
/// 変化しないことを確認する。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_tag_missing_field_is_rejected_and_row_unchanged() {
    let app = test_app("s1d-update-tag-missing-field").await;
    let admin_key = issue_key(&app.router, &app.admin_token, "admin-key", &["admin"]).await;
    let conn_id = create_test_connection(&app, "line1", 15022).await;
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn_id, 100))
        .await
        .unwrap();
    let tag = TagService::new(app.pool.clone())
        .create(tag_input("temp01", group.id, "D100", "u16", false, true))
        .await
        .unwrap();
    let audit_count_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&app.pool)
        .await
        .unwrap();

    let mut arguments = full_tag_update_args(tag.id, group.id, "temp01-renamed", "D200", None);
    arguments
        .as_object_mut()
        .unwrap()
        .remove("enabled")
        .expect("test fixture must include enabled before removal");

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call("update_tag", arguments),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], true, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("missing_fields"), "{text}");
    assert!(text.contains("enabled"), "{text}");

    let stored = TagService::new(app.pool.clone()).get(tag.id).await.unwrap();
    assert_eq!(stored.name, "temp01", "row must not change");
    assert_eq!(stored.address, "D100", "row must not change");

    let audit_count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(
        audit_count_after, audit_count_before,
        "rejected update must not add an audit_log row"
    );
}

/// T18-1 の楽観ロック(`expectedRevision`)を MCP 経由でも確認する:
/// revision が一致していれば成功して +1 され、その後の呼び出しが
/// (先の呼び出しで既に進んでしまった)古い revision を指定すると
/// `revision_conflict`で拒否され、行は変化しない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_tag_optimistic_lock_matching_revision_succeeds_then_stale_revision_conflicts() {
    let app = test_app("s1d-update-tag-revision").await;
    let admin_key = issue_key(&app.router, &app.admin_token, "admin-key", &["admin"]).await;
    let conn_id = create_test_connection(&app, "line1", 15022).await;
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn_id, 100))
        .await
        .unwrap();
    let tag = TagService::new(app.pool.clone())
        .create(tag_input("temp01", group.id, "D100", "u16", false, true))
        .await
        .unwrap();
    assert_eq!(tag.revision, 1);

    // revision が一致する呼び出しは成功して revision が2へ進む。
    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call(
            "update_tag",
            full_tag_update_args(tag.id, group.id, "temp01-v2", "D100", Some(1)),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["updated"]["revision"], 2, "{payload:?}");

    // 同じ(今や古い) revision=1 を指定した2本目の呼び出しは競合として拒否
    // される - 行は1本目の更新結果のまま変化しない。
    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call(
            "update_tag",
            full_tag_update_args(tag.id, group.id, "temp01-v3", "D100", Some(1)),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], true, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("revision_conflict"), "{text}");

    let stored = TagService::new(app.pool.clone()).get(tag.id).await.unwrap();
    assert_eq!(
        stored.name, "temp01-v2",
        "the rejected (stale) update must not overwrite the row"
    );
    assert_eq!(stored.revision, 2, "revision must not advance on conflict");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_tag_without_confirm_is_rejected_and_tag_remains() {
    let app = test_app("s1d-delete-tag-no-confirm").await;
    let admin_key = issue_key(&app.router, &app.admin_token, "admin-key", &["admin"]).await;
    let conn_id = create_test_connection(&app, "line1", 15022).await;
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn_id, 100))
        .await
        .unwrap();
    let tag = TagService::new(app.pool.clone())
        .create(tag_input("temp01", group.id, "D100", "u16", false, true))
        .await
        .unwrap();

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call("delete_tag", json!({ "id": tag.id })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], true, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("confirm_required"), "{text}");

    assert_eq!(
        tags_row_count(&app).await,
        1,
        "delete_tag without confirm must not delete the row"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_tag_with_confirm_deletes_and_audits() {
    let app = test_app("s1d-delete-tag-confirm").await;
    let admin_key = issue_key(&app.router, &app.admin_token, "admin-key", &["admin"]).await;
    let conn_id = create_test_connection(&app, "line1", 15022).await;
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn_id, 100))
        .await
        .unwrap();
    let tag = TagService::new(app.pool.clone())
        .create(tag_input("temp01", group.id, "D100", "u16", false, true))
        .await
        .unwrap();
    let audit_count_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&app.pool)
        .await
        .unwrap();

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call("delete_tag", json!({ "id": tag.id, "confirm": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["deleted"], true);

    assert_eq!(
        tags_row_count(&app).await,
        0,
        "delete_tag with confirm:true must delete the row"
    );

    let audit_count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(audit_count_after, audit_count_before + 1);
    assert_eq!(
        latest_audit_column(&app, "actor_username").await.as_deref(),
        Some("admin-key")
    );
    assert_eq!(
        latest_audit_column(&app, "actor_role").await.as_deref(),
        Some("api_key")
    );
    assert_eq!(
        latest_audit_column(&app, "action").await.as_deref(),
        Some("delete")
    );
    assert_eq!(
        latest_audit_column(&app, "resource").await.as_deref(),
        Some("tags")
    );
    assert_eq!(
        latest_audit_column(&app, "origin").await.as_deref(),
        Some("mcp")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_tag_while_collection_running_is_queued_not_applied() {
    let app = test_app("s1d-create-tag-running").await;
    let admin_key = issue_key(&app.router, &app.admin_token, "admin-key", &["admin"]).await;
    let conn_id = create_test_connection(&app, "line1", 15022).await;
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn_id, 100))
        .await
        .unwrap();
    start_collection(&app.router, &app.admin_token).await;
    let before_rows = tags_row_count(&app).await;
    let audit_count_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&app.pool)
        .await
        .unwrap();

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call(
            "create_tag",
            json!({
                "name": "temp-running",
                "collectionGroupId": group.id,
                "address": "D100",
                "dataType": "u16",
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["queued"], true, "{payload:?}");
    assert!(payload["pendingId"].is_number(), "{payload:?}");

    assert_eq!(
        tags_row_count(&app).await,
        before_rows,
        "a queued create_tag must not touch tags directly"
    );
    let pending_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pending_changes")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(pending_count, 1);

    let audit_count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(
        audit_count_after,
        audit_count_before + 1,
        "queued create_tag via MCP must add exactly one audit_log row"
    );
    assert_eq!(
        latest_audit_column(&app, "origin").await.as_deref(),
        Some("mcp")
    );
    assert_eq!(
        latest_audit_column(&app, "resource").await.as_deref(),
        Some("tags")
    );
}

// ---------------------------------------------------------------------------
// 13. T21 S2-a（`set_collection`/`set_write_control`）: レジストリ mutation
//    ではなくランタイム制御（`CollectionController`/`WriteControl`を直接
//    切り替える）。9〜12節の構成 CRUD ツールと同じく `admin` スコープ必須・
//    全操作を origin="mcp" で監査するが、可逆操作のため confirm は無い。
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_control_tools_without_admin_scope_are_rejected_and_change_nothing() {
    let app = test_app("runtime-no-admin").await;
    // read/write は持つが admin は持たないキー。
    let key = issue_key(
        &app.router,
        &app.admin_token,
        "reader-writer",
        &["read", "write:line1.fast.temp01"],
    )
    .await;

    let (status, body) = mcp_post(
        &app.router,
        Some(&key),
        tools_call("set_collection", json!({ "action": "start" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], true);
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("missing_admin_scope"), "{text}");

    let (status, body) = mcp_post(
        &app.router,
        Some(&key),
        tools_call("set_write_control", json!({ "enabled": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], true);
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("missing_admin_scope"), "{text}");
    assert!(
        !app.write_control.is_enabled(),
        "a rejected set_write_control must not flip the live flag"
    );

    // 収集状態も変化していないことを`get_server_status`(admin+read)で確認する。
    let admin_read_key = issue_key(
        &app.router,
        &app.admin_token,
        "admin-reader",
        &["admin", "read"],
    )
    .await;
    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_read_key),
        tools_call("get_server_status", json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(
        payload["collection_state"], "stopped",
        "a rejected set_collection must not change collection state: {payload:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_collection_start_and_stop_transition_state_and_audit() {
    let app = test_app("runtime-collection-start-stop").await;
    let admin_key = issue_key(&app.router, &app.admin_token, "admin-key", &["admin"]).await;

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call("set_collection", json!({ "action": "start" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["state"], "running", "{payload:?}");
    assert_eq!(payload["mode"], "configured", "{payload:?}");

    assert_eq!(
        latest_audit_column(&app, "action").await.as_deref(),
        Some("start")
    );
    assert_eq!(
        latest_audit_column(&app, "resource").await.as_deref(),
        Some("collection")
    );
    assert_eq!(
        latest_audit_column(&app, "origin").await.as_deref(),
        Some("mcp")
    );
    assert_eq!(
        latest_audit_column(&app, "actor_username").await.as_deref(),
        Some("admin-key")
    );

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call("set_collection", json!({ "action": "stop" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["state"], "stopped", "{payload:?}");

    assert_eq!(
        latest_audit_column(&app, "action").await.as_deref(),
        Some("stop")
    );
    assert_eq!(
        latest_audit_column(&app, "resource").await.as_deref(),
        Some("collection")
    );
    assert_eq!(
        latest_audit_column(&app, "origin").await.as_deref(),
        Some("mcp")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_collection_rejects_an_invalid_action() {
    let app = test_app("runtime-collection-invalid-action").await;
    let admin_key = issue_key(&app.router, &app.admin_token, "admin-key", &["admin"]).await;

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call("set_collection", json!({ "action": "pause" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], true, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("start"), "{text}");
    assert!(text.contains("stop"), "{text}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_write_control_enable_and_disable_persist_and_audit() {
    let app = test_app("runtime-write-control").await;
    let admin_key = issue_key(&app.router, &app.admin_token, "admin-key", &["admin"]).await;

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call("set_write_control", json!({ "enabled": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["writeEnabled"], true, "{payload:?}");
    assert!(app.write_control.is_enabled());

    assert_eq!(
        latest_audit_column(&app, "action").await.as_deref(),
        Some("enable")
    );
    assert_eq!(
        latest_audit_column(&app, "resource").await.as_deref(),
        Some("write_control")
    );
    assert_eq!(
        latest_audit_column(&app, "entity_id").await.as_deref(),
        Some("1")
    );
    assert_eq!(
        latest_audit_column(&app, "origin").await.as_deref(),
        Some("mcp")
    );

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call("set_write_control", json!({ "enabled": false })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["writeEnabled"], false, "{payload:?}");
    assert!(!app.write_control.is_enabled());

    assert_eq!(
        latest_audit_column(&app, "action").await.as_deref(),
        Some("disable")
    );
    assert_eq!(
        latest_audit_column(&app, "resource").await.as_deref(),
        Some("write_control")
    );
}

/// 既知の運用癖（実装指示・`docs/mcp-real-machine-2026-09-04`メモリ参照）:
/// `CollectionController::start`は遷移のたびに`WriteControl::disable`を
/// 呼ぶ（`crate::controller`参照）ため、収集開始直後は書き込み受付が
/// 強制的に無効化される。`set_collection{action:start}`の直後に
/// `write_enabled`が`false`へ戻ること、そこから`set_write_control`で
/// 改めて有効化できることを固定する。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn starting_collection_resets_write_enabled_and_set_write_control_re_enables_it() {
    let app = test_app("runtime-start-resets-write-control").await;
    let admin_key = issue_key(&app.router, &app.admin_token, "admin-key", &["admin"]).await;

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call("set_write_control", json!({ "enabled": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    assert!(app.write_control.is_enabled());

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call("set_collection", json!({ "action": "start" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    assert!(
        !app.write_control.is_enabled(),
        "collection start must reset write_enabled to false (known operational quirk)"
    );

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call("set_write_control", json!({ "enabled": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["writeEnabled"], true, "{payload:?}");
    assert!(app.write_control.is_enabled());
}

// ---------------------------------------------------------------------------
// T21 S2-b: 構成補助ツール（設定 get/set） - gRPC/MQTT/データストア保持。
// `crate::rest`の各設定ハンドラ(`grpc_settings_put`/`mqtt_settings_put`/
// `store_settings_put`)と同じ request/response 型・入力検証を再利用して
// いるので、ここでは「set→get で往復する・監査行が残る・validation は
// tool_error で拒否され状態は変わらない」ことだけを確認する（apply
// 副作用そのものの検証は REST 側テストのスコープ、
// `docs/banto-hub-t21-design.md`実装指示参照）。
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_tools_without_admin_scope_are_rejected_and_change_nothing() {
    let app = test_app("settings-no-admin").await;
    // read/write は持つが admin は持たないキー。
    let key = issue_key(
        &app.router,
        &app.admin_token,
        "reader-writer",
        &["read", "write:line1.fast.temp01"],
    )
    .await;

    let settings = SettingsService::new(app.pool.clone());
    let grpc_before = settings.grpc_config().await.unwrap();
    let mqtt_before = settings.mqtt_config().await.unwrap();
    let store_before = settings.store_config().await.unwrap();

    for (name, args) in [
        ("get_grpc_settings", json!({})),
        ("get_mqtt_settings", json!({})),
        ("get_retention", json!({})),
        (
            "set_grpc_settings",
            json!({ "enabled": true, "bind": "0.0.0.0", "port": 51000 }),
        ),
        (
            "set_mqtt_settings",
            json!({
                "enabled": true,
                "host": "127.0.0.1",
                "port": 1884,
                "clientId": "attacker",
                "prefix": "x",
                "qos": 1,
                "minIntervalMs": 500,
            }),
        ),
        ("set_retention", json!({ "retentionDays": 30 })),
    ] {
        let (status, body) = mcp_post(&app.router, Some(&key), tools_call(name, args)).await;
        assert_eq!(status, StatusCode::OK, "{name}: {body:?}");
        assert_eq!(body["result"]["isError"], true, "{name}: {body:?}");
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("missing_admin_scope"), "{name}: {text}");
    }

    assert_eq!(settings.grpc_config().await.unwrap(), grpc_before);
    assert_eq!(settings.mqtt_config().await.unwrap(), mqtt_before);
    assert_eq!(settings.store_config().await.unwrap(), store_before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_grpc_settings_returns_current_settings() {
    let app = test_app("settings-get-grpc").await;
    let admin_key = issue_key(&app.router, &app.admin_token, "admin-key", &["admin"]).await;

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call("get_grpc_settings", json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["enabled"], false, "{payload:?}");
    assert_eq!(payload["bind"], "127.0.0.1", "{payload:?}");
    assert_eq!(payload["port"], 50051, "{payload:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_mqtt_settings_returns_current_settings_without_password() {
    let app = test_app("settings-get-mqtt").await;
    let admin_key = issue_key(&app.router, &app.admin_token, "admin-key", &["admin"]).await;

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call("get_mqtt_settings", json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["enabled"], false, "{payload:?}");
    assert_eq!(payload["clientId"], "banto-hub", "{payload:?}");
    assert_eq!(payload["prefix"], "banto", "{payload:?}");
    assert_eq!(payload["qos"], 1, "{payload:?}");
    assert!(
        payload.get("password").is_none(),
        "get_mqtt_settings must never return the password: {payload:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_retention_returns_current_settings() {
    let app = test_app("settings-get-retention").await;
    let admin_key = issue_key(&app.router, &app.admin_token, "admin-key", &["admin"]).await;

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call("get_retention", json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["retentionDays"], 7, "{payload:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_retention_persists_and_audits_then_round_trips_through_get() {
    let app = test_app("settings-set-retention").await;
    let admin_key = issue_key(&app.router, &app.admin_token, "admin-key", &["admin"]).await;

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call("set_retention", json!({ "retentionDays": 30 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["retentionDays"], 30, "{payload:?}");

    assert_eq!(
        latest_audit_column(&app, "action").await.as_deref(),
        Some("update")
    );
    assert_eq!(
        latest_audit_column(&app, "resource").await.as_deref(),
        Some("store_settings")
    );
    assert_eq!(
        latest_audit_column(&app, "origin").await.as_deref(),
        Some("mcp")
    );
    assert_eq!(
        latest_audit_column(&app, "actor_username").await.as_deref(),
        Some("admin-key")
    );

    let settings = SettingsService::new(app.pool.clone());
    assert_eq!(
        settings.store_config().await.unwrap().retention_days,
        Some(30)
    );

    // null(省略)は無制限 - REST の`store_settings_put`と同じ規約。
    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call("set_retention", json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["retentionDays"], Value::Null, "{payload:?}");
    assert_eq!(settings.store_config().await.unwrap().retention_days, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_retention_out_of_range_is_rejected_and_unchanged() {
    let app = test_app("settings-retention-range").await;
    let admin_key = issue_key(&app.router, &app.admin_token, "admin-key", &["admin"]).await;
    let settings = SettingsService::new(app.pool.clone());
    let before = settings.store_config().await.unwrap();

    for bad in [0, -1, 3651] {
        let (status, body) = mcp_post(
            &app.router,
            Some(&admin_key),
            tools_call("set_retention", json!({ "retentionDays": bad })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{bad}: {body:?}");
        assert_eq!(body["result"]["isError"], true, "{bad}: {body:?}");
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("retentionDays"), "{bad}: {text}");
    }
    assert_eq!(settings.store_config().await.unwrap(), before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_mqtt_settings_persists_and_audits_then_round_trips_through_get() {
    let app = test_app("settings-set-mqtt").await;
    let admin_key = issue_key(&app.router, &app.admin_token, "admin-key", &["admin"]).await;

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call(
            "set_mqtt_settings",
            json!({
                "enabled": true,
                "host": "mqtt.example.local",
                "port": 1884,
                "clientId": "banto-hub-test",
                "username": "operator",
                "password": "s3cret",
                "prefix": "line1",
                "qos": 1,
                "minIntervalMs": 500,
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["host"], "mqtt.example.local", "{payload:?}");
    assert_eq!(payload["username"], "operator", "{payload:?}");
    assert!(
        payload.get("password").is_none(),
        "set_mqtt_settings response must never echo the password: {payload:?}"
    );

    assert_eq!(
        latest_audit_column(&app, "action").await.as_deref(),
        Some("update")
    );
    assert_eq!(
        latest_audit_column(&app, "resource").await.as_deref(),
        Some("mqtt_settings")
    );
    assert_eq!(
        latest_audit_column(&app, "origin").await.as_deref(),
        Some("mcp")
    );

    let settings = SettingsService::new(app.pool.clone());
    let persisted = settings.mqtt_config().await.unwrap();
    assert_eq!(persisted.host, "mqtt.example.local");
    assert_eq!(persisted.password.as_deref(), Some("s3cret"));

    // 空文字パスワードは「変更なし」- REST の`mqtt_settings_put`と同じ規約。
    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call(
            "set_mqtt_settings",
            json!({
                "enabled": true,
                "host": "mqtt.example.local",
                "port": 1884,
                "clientId": "banto-hub-test",
                "prefix": "line1",
                "qos": 1,
                "minIntervalMs": 500,
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    assert_eq!(
        settings.mqtt_config().await.unwrap().password.as_deref(),
        Some("s3cret"),
        "omitting password must keep the existing one"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_mqtt_settings_rejects_invalid_qos_and_leaves_settings_unchanged() {
    let app = test_app("settings-mqtt-invalid").await;
    let admin_key = issue_key(&app.router, &app.admin_token, "admin-key", &["admin"]).await;
    let settings = SettingsService::new(app.pool.clone());
    let before = settings.mqtt_config().await.unwrap();

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call(
            "set_mqtt_settings",
            json!({
                "enabled": true,
                "host": "mqtt.example.local",
                "port": 1884,
                "clientId": "banto-hub-test",
                "prefix": "line1",
                "qos": 2,
                "minIntervalMs": 500,
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], true, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("qos"), "{text}");
    assert_eq!(settings.mqtt_config().await.unwrap(), before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_grpc_settings_persists_and_audits_then_round_trips_through_get() {
    let app = test_app("settings-set-grpc").await;
    let admin_key = issue_key(&app.router, &app.admin_token, "admin-key", &["admin"]).await;

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call(
            "set_grpc_settings",
            json!({ "enabled": true, "bind": "0.0.0.0", "port": 51000 }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["enabled"], true, "{payload:?}");
    assert_eq!(payload["bind"], "0.0.0.0", "{payload:?}");
    assert_eq!(payload["port"], 51000, "{payload:?}");

    assert_eq!(
        latest_audit_column(&app, "action").await.as_deref(),
        Some("update")
    );
    assert_eq!(
        latest_audit_column(&app, "resource").await.as_deref(),
        Some("grpc_settings")
    );
    assert_eq!(
        latest_audit_column(&app, "origin").await.as_deref(),
        Some("mcp")
    );

    let settings = SettingsService::new(app.pool.clone());
    let persisted = settings.grpc_config().await.unwrap();
    assert_eq!(persisted.bind, "0.0.0.0");
    assert_eq!(persisted.port, 51000);

    // bind 省略時は現在値を維持する - REST の`grpc_settings_put`と同じ規約。
    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call(
            "set_grpc_settings",
            json!({ "enabled": false, "port": 51001 }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], false, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(
        payload["bind"], "0.0.0.0",
        "omitted bind must keep the existing value: {payload:?}"
    );
    assert_eq!(payload["port"], 51001, "{payload:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_grpc_settings_rejects_invalid_bind_and_leaves_settings_unchanged() {
    let app = test_app("settings-grpc-invalid").await;
    let admin_key = issue_key(&app.router, &app.admin_token, "admin-key", &["admin"]).await;
    let settings = SettingsService::new(app.pool.clone());
    let before = settings.grpc_config().await.unwrap();

    let (status, body) = mcp_post(
        &app.router,
        Some(&admin_key),
        tools_call(
            "set_grpc_settings",
            json!({ "enabled": true, "bind": "not-an-ip", "port": 51000 }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"]["isError"], true, "{body:?}");
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("bind"), "{text}");
    assert_eq!(settings.grpc_config().await.unwrap(), before);
}
