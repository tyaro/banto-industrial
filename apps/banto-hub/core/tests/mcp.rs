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
async fn tools_list_returns_the_five_tools() {
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
            "get_server_status",
            "list_tags",
            "read_tag_values",
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
