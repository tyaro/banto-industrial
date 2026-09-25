//! T1 の統合テスト: 実サーバー（`banto_server::start` を ephemeral port
//! （`port: 0`）で起動）+ 実 WebSocket クライアント（`tokio-tungstenite`、
//! dev-dependency のみ - `apps/banto-hub/core/Cargo.toml` 参照）+ Modbus TCP
//! シミュレータ（`banto_plc::modbus::simulator`）による E2E。
//!
//! `tests/integration.rs`（T0-1/T0-2 の REST 統合テスト）は `tower::oneshot`
//! で `Router` を直接叩くが、WebSocket のアップグレードには実際の TCP
//! 接続 + HTTP Upgrade ハンドシェイクが要る（`oneshot` はこれを完走できない）
//! ため、このファイルだけ `banto_server::start` で実ポートを bind する -
//! `fast_options`/`wait_until` 等の足場は同ファイルから輸入した（各
//! `tests/*.rs` は独立バイナリとしてコンパイルされ、ヘルパーは共有されない
//! ため複製している）。`TempEnv` は `tests/common/mod.rs` に集約済み
//! （2026-08-08、テスト一時ディレクトリリークの根治）。

use banto_hub_core::rest::user_session_lookup;
use banto_server::SessionValidation;
use std::time::Duration;

use axum::Router;
use banto_collect::{BackoffConfig, CollectorOptions};
use banto_hub_core::api_keys::ApiKeysService;
use banto_hub_core::audit::AuditLogService;
use banto_hub_core::commissioning::CommissioningService;
use banto_hub_core::computed::{ComputedEngine, ServerTagStore};
use banto_hub_core::controller::{CollectionController, CollectionState, RunMode};
use banto_hub_core::db::init_db;
use banto_hub_core::hub::CollectorManager;
use banto_hub_core::rest::api_router_with_controller;
use banto_hub_core::settings::SettingsService;
use banto_hub_core::stream::{
    StreamRevalidationTiming, COMMISSIONING_ENDED_REASON, REVOKED_CLOSE_CODE,
    SESSION_REVOKED_REASON,
};
use banto_hub_core::users::{Role, UsersService};
use banto_plc::modbus::simulator::Simulator;
use banto_server::{start, AuthState, Identity, ServerConfig};
use banto_tags::{
    CollectionGroupInput, CollectionGroupService, PlcConnectionInput, PlcConnectionService,
    TagInput, TagService,
};
use banto_tstore::SystemClock;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use sqlx::SqlitePool;
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

mod common;
use common::TempEnv;

type WsStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// 設計 §5.2 要件6の送信キュー容量（`stream.rs::OUTBOUND_QUEUE_CAPACITY`）と
/// 同じ値。テスト8（バックプレッシャ）で「キューが埋まりきる」量の目安に
/// 使う。
const OUTBOUND_QUEUE_CAPACITY: usize = 256;

/// Temp-dir prefix passed to `TempEnv::new` (see `tests/common/mod.rs`).
const TEMP_ENV_PREFIX: &str = "banto-hub-ws-it";

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

fn conn_input(name: &str, port: u16) -> PlcConnectionInput {
    PlcConnectionInput {
        name: name.to_string(),
        protocol: "modbus-tcp".to_string(),
        host: "127.0.0.1".to_string(),
        port: port as i64,
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

fn tag_input(name: &str, group_id: i64, address: &str, data_type: &str) -> TagInput {
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
        writable: false,
        tag_kind: "plc".to_string(),
        expression: None,
        retain: false,
        expected_revision: None,
    }
}

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

/// 1テスト分の環境: 実サーバー（`RunningServer`、`server.stop()` で終了）+
/// ログイン済みトークン + `CollectorManager`（rebuild を直接叩くため）+
/// 生成物を握る `TempEnv`。`Simulator` はテスト側で個別に持つ（PLC アドレス
/// をテストごとに変えるため）。
struct TestApp {
    server: banto_server::RunningServer,
    token: String,
    pool: SqlitePool,
    manager: std::sync::Arc<CollectorManager>,
    controller: std::sync::Arc<CollectionController>,
    api_keys: ApiKeysService,
    /// #430: サーバーと同じ `AuthState`（ほかのアカウントのログイン用）。
    auth: AuthState,
    /// #430: アカウントの削除・降格・パスワードのリセット用。
    users: UsersService,
    /// #430: サーバーと同じ router（`/api/auth/change-password` を
    /// `tower::ServiceExt::oneshot` で叩く。トークンの表はサーバーと共有）。
    router: Router,
    /// #440: サーバーと同じ試運転モードの状態（ロックダウンの操作用）。
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

impl TestApp {
    fn ws_url(&self, path: &str) -> String {
        format!("ws://127.0.0.1:{}{path}", self.server.local_addr().port())
    }
}

async fn test_app(label: &str) -> TestApp {
    test_app_with_lock(label, true).await
}

/// [`test_app`]だが試運転モード（未ロックダウン）のまま返す - 管理系WS
/// `/api/tag-stream`（`admin_tag_stream_router`、設計 §5.6・2026-08-31
/// オーナー決定）の試運転モード側の挙動を確認するテスト専用。`tests/rest.rs`
/// 相当が無いこのファイルでは、`apps/banto-hub/core/src/rest.rs`の
/// `test_env_unlocked`と同じ役割をこの関数が担う。
async fn test_app_unlocked(label: &str) -> TestApp {
    test_app_with_lock(label, false).await
}

async fn test_app_with_lock(label: &str, locked_down: bool) -> TestApp {
    test_app_with(label, locked_down, None).await
}

/// #430: 再検証の間隔と上限を短くした [`test_app`]（`StreamRevalidationTiming`
/// を router の extensions に重ねる。本番は 15 秒 / 5 秒）。
async fn test_app_with_revalidation(label: &str, timing: StreamRevalidationTiming) -> TestApp {
    test_app_with(label, true, Some(timing)).await
}

/// #440: [`test_app_with_revalidation`] だが試運転モード（未ロックダウン）のまま。
async fn test_app_unlocked_with_revalidation(
    label: &str,
    timing: StreamRevalidationTiming,
) -> TestApp {
    test_app_with(label, false, Some(timing)).await
}

async fn test_app_with(
    label: &str,
    locked_down: bool,
    revalidation: Option<StreamRevalidationTiming>,
) -> TestApp {
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
    let token = auth
        .login("admin", "password123")
        .await
        .expect("admin login");
    // #430: セッションの失効のテストが、ほかのアカウントでログインし、
    // アカウントを変えるための手綱（`AuthState` はトークンの表を共有する）。
    let auth_handle = auth.clone();
    let users_handle = users.clone();

    let sessions = std::sync::Arc::new(banto_hub_core::broker_glue::HubSessions::new(
        banto_broker::BackoffConfig::default(),
    ));
    let sim_registry = std::sync::Arc::new(banto_hub_core::broker_glue::BrokerSimRegistry::new());
    let computed = std::sync::Arc::new(ComputedEngine::new(std::sync::Arc::new(
        ServerTagStore::new(),
    )));
    let manager = std::sync::Arc::new(CollectorManager::new(
        pool.clone(),
        env.data_dir(),
        std::sync::Arc::new(SystemClock),
        fast_options(),
        sessions,
        sim_registry,
        computed,
    ));
    manager.rebuild().await.expect("initial rebuild");
    let write_control =
        std::sync::Arc::new(banto_hub_core::write_control::WriteControl::new(false));
    let controller = std::sync::Arc::new(CollectionController::new(manager.clone()));
    let status = controller.start(RunMode::Configured).await;
    assert_eq!(status.state, CollectionState::Running);

    let api_keys = ApiKeysService::new(pool.clone());
    let (events_tx, _rx) = broadcast::channel(16);
    // T2-4: WriteControl always constructs disabled (docs/tag-server-design.md
    // §6-6) - not exercised by these WebSocket-subscription tests.
    let write_audit = banto_hub_core::write_audit::WriteAuditService::new(pool.clone());
    let mqtt = std::sync::Arc::new(banto_hub_core::mqtt::MqttPublisher::new(manager.clone()));
    // T4: this file exercises the WebSocket subscription surface only
    // (`tests/grpc.rs` covers gRPC's `StreamValues`) - `api_router`'s T4
    // arguments (the REST/gRPC-shared rate_limiter and `GrpcServer`) are
    // still required, so construct them without ever calling `apply`.
    let rate_limiter = std::sync::Arc::new(tokio::sync::Mutex::new(
        banto_hub_core::write_rate::WriteRateLimiter::new(
            banto_hub_core::write_rate::WriteRateLimitConfig::default(),
        ),
    ));
    let grpc_service = banto_hub_core::grpc::GrpcService::new(
        manager.clone(),
        api_keys.clone(),
        audit.clone(),
        write_audit.clone(),
        write_control.clone(),
        rate_limiter.clone(),
        events_tx.clone(),
    );
    let grpc_server = std::sync::Arc::new(banto_hub_core::grpc::GrpcServer::new(grpc_service));
    let settings = SettingsService::new(pool.clone());
    let commissioning = CommissioningService::load(settings, users.clone())
        .await
        .expect("CommissioningService::load");
    if locked_down {
        commissioning
            .lock_down()
            .await
            .expect("lock_down the test environment");
    }
    let commissioning_handle = commissioning.clone();

    let router: Router = api_router_with_controller(
        users,
        audit,
        PlcConnectionService::new(pool.clone()),
        CollectionGroupService::new(pool.clone()),
        TagService::new(pool.clone()),
        api_keys.clone(),
        manager.clone(),
        controller.clone(),
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
    let router = match revalidation {
        Some(timing) => router.layer(axum::Extension(timing)),
        None => router,
    };
    let router_handle = router.clone();

    let server = start(
        ServerConfig {
            bind: "127.0.0.1".to_string(),
            port: 0,
        },
        router,
    )
    .await
    .expect("server should start");

    TestApp {
        server,
        token,
        pool,
        manager,
        controller,
        api_keys,
        auth: auth_handle,
        users: users_handle,
        router: router_handle,
        commissioning: commissioning_handle,
        _env: env,
    }
}

// --- WS クライアントヘルパー -------------------------------------------------

async fn connect_ws(
    url: &str,
    token: Option<&str>,
) -> Result<WsStream, tokio_tungstenite::tungstenite::Error> {
    let mut request = url.into_client_request().expect("valid ws url");
    if let Some(token) = token {
        request.headers_mut().insert(
            "Authorization",
            format!("Bearer {token}")
                .parse()
                .expect("valid header value"),
        );
    }
    let (stream, _response) = connect_async(request).await?;
    Ok(stream)
}

/// Like [`connect_ws`] but authenticates via `Sec-WebSocket-Protocol:
/// "bearer, {token}"` instead of `Authorization` - exercises
/// `rest.rs::extract_ws_protocol_token`, the fallback added so a browser
/// (which cannot set `Authorization` on a WS handshake) can authenticate
/// `GET /api/v1/stream`. No `Authorization` header is set at all, so a
/// success here proves the subprotocol path alone is sufficient.
async fn connect_ws_via_subprotocol(
    url: &str,
    token: &str,
) -> Result<WsStream, tokio_tungstenite::tungstenite::Error> {
    connect_ws_via_subprotocol_raw(url, &format!("bearer, {token}")).await
}

/// Like [`connect_ws_via_subprotocol`] but takes the raw
/// `Sec-WebSocket-Protocol` header value verbatim - used to send malformed
/// values (e.g. `"bearer"` with no token part) that a real client would
/// never construct via the `['bearer', token]` array form.
async fn connect_ws_via_subprotocol_raw(
    url: &str,
    protocol_header: &str,
) -> Result<WsStream, tokio_tungstenite::tungstenite::Error> {
    let mut request = url.into_client_request().expect("valid ws url");
    request.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        protocol_header.parse().expect("valid header value"),
    );
    let (stream, _response) = connect_async(request).await?;
    Ok(stream)
}

async fn send_json(ws: &mut WsStream, value: Value) {
    ws.send(WsMessage::Text(value.to_string().into()))
        .await
        .expect("ws send should succeed");
}

/// `predicate` を満たす次のテキストメッセージを受信するまで待つ（他の
/// op のメッセージ - `event`/`config_changed`/別 id の `data` 等 - は読み
/// 飛ばす）。5秒でタイムアウトしてテストを失敗させる。
async fn recv_matching(ws: &mut WsStream, predicate: impl Fn(&Value) -> bool) -> Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                Some(Ok(WsMessage::Text(text))) => {
                    let value: Value =
                        serde_json::from_str(&text).expect("server should send valid JSON");
                    if predicate(&value) {
                        return value;
                    }
                }
                Some(Ok(WsMessage::Ping(_))) | Some(Ok(WsMessage::Pong(_))) => continue,
                Some(Ok(other)) => panic!("unexpected non-text ws message: {other:?}"),
                Some(Err(err)) => panic!("ws error while waiting for a message: {err}"),
                None => panic!("connection closed while waiting for a message"),
            }
        }
    })
    .await
    .expect("timed out waiting for the expected ws message")
}

/// ping を送り、その応答として最初に届く `data`（`id` 一致）または `pong`
/// を待つ。`data` が先に来たら即座にテスト失敗させる（テスト5「unsubscribe
/// 後は data が止まる」の中核アサーション）。
async fn assert_no_more_data_for(ws: &mut WsStream, id: i64) {
    send_json(ws, json!({ "op": "ping" })).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                Some(Ok(WsMessage::Text(text))) => {
                    let value: Value =
                        serde_json::from_str(&text).expect("server should send valid JSON");
                    if value["op"] == "data" && value["id"] == id {
                        panic!("unsubscribed id {id} still received data: {value}");
                    }
                    if value["op"] == "pong" {
                        return;
                    }
                    // event/config_changed/別 id の data はそのまま読み飛ばす。
                }
                Some(Ok(WsMessage::Ping(_))) | Some(Ok(WsMessage::Pong(_))) => continue,
                Some(Ok(other)) => panic!("unexpected non-text ws message: {other:?}"),
                Some(Err(err)) => panic!("ws error: {err}"),
                None => panic!("connection closed while waiting for pong"),
            }
        }
    })
    .await
    .expect("timed out waiting for pong");
}

/// #244: 「スコープ外タグの値変更後、そのタグを含む data が来ないこと」を
/// 検証する専用ヘルパー。`assert_no_more_data_for` は「その id への data が
/// 一切来ない」ことを要求するため、スコープ内タグの spurious な on_change
/// 再送（品質のみの再送や旧値の再送。テスト10/11 のコメント、PR #139 参照）
/// を拾って誤って失敗する（#244 のフレーク本体）。真に検証すべきは
/// 「スコープ外タグ(`forbidden_tag`)を含む値が漏れて届かないこと」であって
/// 「一切何も届かないこと」ではないため、スコープ内タグの再送は drain して
/// 読み飛ばし、`forbidden_tag` の出現だけを失敗条件にする。
///
/// ping を送り、`id` 一致の `pong` が来るまで受信ループする。`op == "data"`
/// かつ `id` 一致のメッセージが来た場合、その `values[].tag` に
/// `forbidden_tag` が含まれていればスコープ漏れ＝本物のバグとして即座に
/// panic する。含まれていなければスコープ内タグの spurious 再送と見なして
/// drain し、受信を継続する。全体で5秒のタイムアウト（pong すら来ない＝
/// スタック）で panic する。
async fn assert_no_data_carrying_tag(ws: &mut WsStream, id: i64, forbidden_tag: &str) {
    send_json(ws, json!({ "op": "ping" })).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                Some(Ok(WsMessage::Text(text))) => {
                    let value: Value =
                        serde_json::from_str(&text).expect("server should send valid JSON");
                    if value["op"] == "data" && value["id"] == id {
                        let leaked = value["values"]
                            .as_array()
                            .map(|values| values.iter().any(|v| v["tag"] == forbidden_tag))
                            .unwrap_or(false);
                        if leaked {
                            panic!(
                                "scope leak: id {id} received data carrying forbidden tag \
                                 {forbidden_tag}: {value}"
                            );
                        }
                        // スコープ内タグの spurious な再送。読み飛ばして継続する。
                        continue;
                    }
                    if value["op"] == "pong" {
                        return;
                    }
                    // event/config_changed/別 id の data はそのまま読み飛ばす。
                }
                Some(Ok(WsMessage::Ping(_))) | Some(Ok(WsMessage::Pong(_))) => continue,
                Some(Ok(other)) => panic!("unexpected non-text ws message: {other:?}"),
                Some(Err(err)) => panic!("ws error: {err}"),
                None => panic!("connection closed while waiting for pong"),
            }
        }
    })
    .await
    .expect("timed out waiting for pong");
}

// ---------------------------------------------------------------------------
// 1. subscribe(具体名) → 初期スナップショット → 値変更 → on_change data
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribe_exact_tag_gets_initial_snapshot_then_on_change_data() {
    let app = test_app("exact-on-change").await;
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 100); // 40001

    let conn = PlcConnectionService::new(app.pool.clone())
        .create(conn_input("line1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("temp01", group.id, "40001", "i16"))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild after seeding");

    assert!(
        wait_until(Duration::from_secs(10), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get("tag:1"))
                .map(|s| s.value)
                == Some(Some(100.0))
        })
        .await,
        "collector should observe the initial simulator value"
    );

    let mut ws = connect_ws(&app.ws_url("/api/v1/stream"), Some(&app.token))
        .await
        .expect("ws handshake should succeed");

    send_json(
        &mut ws,
        json!({ "op": "subscribe", "id": 1, "tags": ["line1.fast.temp01"], "mode": "on_change" }),
    )
    .await;

    let snapshot = recv_matching(&mut ws, |m| m["op"] == "data" && m["id"] == 1).await;
    let values = snapshot["values"].as_array().unwrap();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0]["tag"], "line1.fast.temp01");
    assert_eq!(values[0]["v"], 100.0);
    assert_eq!(values[0]["q"], "good");

    sim.set_holding_register(0, 200);

    // #244 と同系統: 値変更直後は旧値(100)を載せた spurious な quality-only
    // on_change 再送が先に届きうる(grpc.rs の drain_until_value・PR #139 参照)
    // ため、述語に新値(200)の照合を足して、それが届くまで drain する。
    let changed = recv_matching(&mut ws, |m| {
        m["op"] == "data" && m["id"] == 1 && m["values"][0]["v"] == 200.0
    })
    .await;
    let values = changed["values"].as_array().unwrap();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0]["v"], 200.0);

    sim.stop();
}

/// 2026-09-15 オーナー決定（#335 追補、「外部出力を PLC への出力と勘違い
/// していた」）: 以前はここで API キー WS が `AllSimulation` 突入時に
/// 強制切断され、新規 subscribe も `simulation_output_disabled` で拒否
/// されていた（旧 PR #95 挙動）。その抑止は撤去済み - API キー WS も
/// run mode によらず配信を継続し、新規 subscribe も通常どおり成功する。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_key_ws_keeps_streaming_normal_output_during_all_simulation() {
    let app = test_app("api-key-all-simulation").await;
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 100);

    let conn = PlcConnectionService::new(app.pool.clone())
        .create(conn_input("line1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("temp01", group.id, "40001", "i16"))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild after seeding");
    assert!(
        wait_until(Duration::from_secs(10), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get("tag:1"))
                .map(|s| s.value)
                == Some(Some(100.0))
        })
        .await,
        "collector should observe the initial simulator value"
    );

    let issued = app
        .api_keys
        .issue("all-simulation-reader", vec!["read".to_string()], None)
        .await
        .expect("issue should succeed");
    let mut api_key_ws = connect_ws(&app.ws_url("/api/v1/stream"), Some(&issued.key))
        .await
        .expect("API-key WS handshake should succeed");
    send_json(
        &mut api_key_ws,
        json!({ "op": "subscribe", "id": 1, "tags": ["line1.fast.temp01"], "mode": "on_change" }),
    )
    .await;
    let snapshot = recv_matching(&mut api_key_ws, |m| m["op"] == "data" && m["id"] == 1).await;
    assert_eq!(snapshot["values"][0]["v"], 100.0);

    let status = app.controller.start(RunMode::AllSimulation).await;
    assert_eq!(status.state, CollectionState::Running);
    assert_eq!(status.mode, RunMode::AllSimulation);

    // 既存の API キー WS 購読は AllSimulation 突入後も生きたまま - ping/pong
    // が通ることで接続が能動的に切断されていないことを確認する。
    send_json(&mut api_key_ws, json!({ "op": "ping" })).await;
    let pong = recv_matching(&mut api_key_ws, |m| m["op"] == "pong").await;
    assert_eq!(pong["op"], "pong", "existing API-key WS must stay open");

    // 新規 API キー WS の subscribe も AllSimulation 中に通常どおり成功する
    // （以前の `simulation_output_disabled` 拒否は撤去済み）。
    let mut new_api_key_ws = connect_ws(&app.ws_url("/api/v1/stream"), Some(&issued.key))
        .await
        .expect("new API-key WS handshake should still succeed");
    send_json(
        &mut new_api_key_ws,
        json!({ "op": "subscribe", "id": 3, "tags": ["line1.fast.temp01"], "mode": "on_change" }),
    )
    .await;
    let snapshot = recv_matching(&mut new_api_key_ws, |m| m["op"] == "data" && m["id"] == 3).await;
    assert_eq!(snapshot["values"][0]["tag"], "line1.fast.temp01");

    sim.stop();
}

// ---------------------------------------------------------------------------
// 2. ワイルドカード `*` 購読 + タグ追加(CRUD) → config_changed → 新タグが
//    以後の data に現れる
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wildcard_subscription_picks_up_a_tag_added_after_config_changed() {
    let app = test_app("wildcard-config-changed").await;
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 1); // 40001
    sim.set_holding_register(1, 2); // 40002

    let conn = PlcConnectionService::new(app.pool.clone())
        .create(conn_input("line1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("a", group.id, "40001", "i16"))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild after seeding");

    assert!(
        wait_until(Duration::from_secs(10), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get("tag:1"))
                .is_some()
        })
        .await
    );

    let mut ws = connect_ws(&app.ws_url("/api/v1/stream"), Some(&app.token))
        .await
        .expect("ws handshake should succeed");

    send_json(
        &mut ws,
        json!({ "op": "subscribe", "id": 7, "tags": ["*"], "mode": "on_change" }),
    )
    .await;
    let snapshot = recv_matching(&mut ws, |m| m["op"] == "data" && m["id"] == 7).await;
    assert!(
        snapshot["values"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["tag"] == "line1.fast.a"),
        "initial snapshot should contain the pre-existing tag: {snapshot}"
    );

    let revision_before = app.manager.revision();
    let created = TagService::new(app.pool.clone())
        .create(tag_input("b", group.id, "40002", "i16"))
        .await
        .unwrap();
    app.manager
        .rebuild()
        .await
        .expect("rebuild after adding tag b");
    assert!(app.manager.revision() > revision_before);

    let config_changed = recv_matching(&mut ws, |m| m["op"] == "config_changed").await;
    assert!(config_changed["revision"].as_u64().unwrap() > revision_before);

    assert!(
        wait_until(Duration::from_secs(10), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get(&format!("tag:{}", created.id)))
                .is_some()
        })
        .await,
        "collector should observe tag b before we wait for it over WS"
    );

    let data_with_b = recv_matching(&mut ws, |m| {
        m["op"] == "data"
            && m["id"] == 7
            && m["values"]
                .as_array()
                .map(|values| values.iter().any(|v| v["tag"] == "line1.fast.b"))
                .unwrap_or(false)
    })
    .await;
    assert!(!data_with_b["values"].as_array().unwrap().is_empty());

    sim.stop();
}

// ---------------------------------------------------------------------------
// 3. interval モード: 指定間隔で data が届く(値不変でも)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interval_mode_sends_data_on_a_schedule_even_without_changes() {
    let app = test_app("interval").await;
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 42);

    let conn = PlcConnectionService::new(app.pool.clone())
        .create(conn_input("line1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("temp01", group.id, "40001", "i16"))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild after seeding");

    assert!(
        wait_until(Duration::from_secs(10), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get("tag:1"))
                .map(|s| s.value)
                == Some(Some(42.0))
        })
        .await
    );

    let mut ws = connect_ws(&app.ws_url("/api/v1/stream"), Some(&app.token))
        .await
        .expect("ws handshake should succeed");

    send_json(
        &mut ws,
        json!({ "op": "subscribe", "id": 3, "tags": ["line1.fast.temp01"], "mode": "interval", "interval_ms": 300 }),
    )
    .await;

    // Initial snapshot (design §5.2 要件5) always fires immediately.
    let first = recv_matching(&mut ws, |m| m["op"] == "data" && m["id"] == 3).await;
    assert_eq!(first["values"][0]["v"], 42.0);
    let t_first = first["t"].as_i64().unwrap();

    // Value never changes, but the next `data` must still arrive on its own
    // (design §5.2 要件3: interval は値不変でも送る) - not an on_change diff.
    let second = recv_matching(&mut ws, |m| m["op"] == "data" && m["id"] == 3).await;
    let t_second = second["t"].as_i64().unwrap();
    assert_eq!(second["values"][0]["v"], 42.0);
    assert!(
        t_second - t_first >= 250,
        "second interval tick should arrive roughly on schedule (>= EVAL_TICK_MS), got {}ms apart",
        t_second - t_first
    );

    sim.stop();
}

// ---------------------------------------------------------------------------
// 4. unknown_tag subscribe → error 応答、購読されない
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribing_an_unknown_tag_is_rejected_and_not_subscribed() {
    let app = test_app("unknown-tag").await;

    let mut ws = connect_ws(&app.ws_url("/api/v1/stream"), Some(&app.token))
        .await
        .expect("ws handshake should succeed");

    send_json(
        &mut ws,
        json!({ "op": "subscribe", "id": 9, "tags": ["nope.nope.nope"], "mode": "on_change" }),
    )
    .await;

    let error = recv_matching(&mut ws, |m| m["op"] == "error").await;
    assert_eq!(error["id"], 9);
    assert_eq!(error["code"], "unknown_tag");

    // Not subscribed: pinging must get pong next, never a `data` for id 9.
    assert_no_more_data_for(&mut ws, 9).await;
}

// ---------------------------------------------------------------------------
// 5. unsubscribe 後は data が止まる(ping/pong で生存確認)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsubscribe_stops_further_data() {
    let app = test_app("unsubscribe").await;
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 1);

    let conn = PlcConnectionService::new(app.pool.clone())
        .create(conn_input("line1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("temp01", group.id, "40001", "i16"))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild after seeding");

    assert!(
        wait_until(Duration::from_secs(10), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get("tag:1"))
                .is_some()
        })
        .await
    );

    let mut ws = connect_ws(&app.ws_url("/api/v1/stream"), Some(&app.token))
        .await
        .expect("ws handshake should succeed");

    send_json(
        &mut ws,
        json!({ "op": "subscribe", "id": 5, "tags": ["line1.fast.temp01"], "mode": "on_change" }),
    )
    .await;
    recv_matching(&mut ws, |m| m["op"] == "data" && m["id"] == 5).await; // initial snapshot

    // Prove data actually flows before we unsubscribe.
    sim.set_holding_register(0, 2);
    // #244 と同系統: 値変更直後は旧値(1)を載せた spurious な quality-only
    // on_change 再送が先に届きうる(grpc.rs の drain_until_value・PR #139 参照)
    // ため、述語に新値(2)の照合を足して、それが届くまで drain する。
    let changed = recv_matching(&mut ws, |m| {
        m["op"] == "data" && m["id"] == 5 && m["values"][0]["v"] == 2.0
    })
    .await;
    assert_eq!(changed["values"][0]["v"], 2.0);

    send_json(&mut ws, json!({ "op": "unsubscribe", "id": 5 })).await;

    // Change the value again - if the subscription were still live this
    // would produce another `data` message for id 5.
    sim.set_holding_register(0, 3);
    tokio::time::sleep(Duration::from_millis(400)).await; // give the eval loop a chance to (wrongly) fire
    assert_no_more_data_for(&mut ws, 5).await;

    sim.stop();
}

// ---------------------------------------------------------------------------
// 6. 認証なし接続 → 401。read スコープなし API キー → 403
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_requires_auth_and_read_scope() {
    let app = test_app("auth").await;

    let err = connect_ws(&app.ws_url("/api/v1/stream"), None)
        .await
        .expect_err("no Authorization header should be rejected");
    match err {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            assert_eq!(response.status().as_u16(), 401, "{response:?}");
        }
        other => panic!("expected an HTTP-level rejection, got {other:?}"),
    }

    let issued = app
        .api_keys
        .issue(
            "writer-only",
            vec!["write:line1.fast.temp01".to_string()],
            None,
        )
        .await
        .expect("issue should succeed");

    let err = connect_ws(&app.ws_url("/api/v1/stream"), Some(&issued.key))
        .await
        .expect_err("a write-only key should be rejected (no read scope)");
    match err {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            assert_eq!(response.status().as_u16(), 403, "{response:?}");
        }
        other => panic!("expected an HTTP-level rejection, got {other:?}"),
    }

    // Sanity: the same admin session token that every other test in this
    // file uses does succeed, so the two rejections above are really about
    // auth/scope and not some other handshake problem.
    let ok = connect_ws(&app.ws_url("/api/v1/stream"), Some(&app.token)).await;
    assert!(ok.is_ok(), "session token should be accepted: {ok:?}");
}

// ---------------------------------------------------------------------------
// 7. PLC 断(シミュレータ stop) → `op: "event"` の plc_disconnected
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plc_disconnect_is_relayed_as_an_event() {
    let app = test_app("event-relay").await;
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 1);

    let conn = PlcConnectionService::new(app.pool.clone())
        .create(conn_input("line1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("temp01", group.id, "40001", "i16"))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild after seeding");

    // H7 ⑤ follow-up (2026-08-09, CI flake on PR #96): the previous
    // pre-check only required `CurrentSample::is_some()`. That can pass on a
    // Bad/no-value bootstrap entry written before the first successful PLC
    // connect (`current.rs::set`). `PlcDisconnected` is emitted only on the
    // drop of a previously-connected session (`task.rs`), so stopping the
    // simulator before the first Good read leaves no event and
    // `recv_matching` times out. Mirror the sibling subscribe test / gRPC
    // `stream_events_relays_plc_disconnected`: wait for the seeded value.
    assert!(
        wait_until(Duration::from_secs(5), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get("tag:1"))
                .and_then(|s| s.value)
                == Some(1.0)
        })
        .await,
        "collector should observe the seeded simulator value before we open the ws (so plc_connected is established and plc_disconnected can fire)"
    );

    let mut ws = connect_ws(&app.ws_url("/api/v1/stream"), Some(&app.token))
        .await
        .expect("ws handshake should succeed");

    // Race (observed under CI load; H7 ⑤): `connect_ws` returning only
    // proves the *client* saw the HTTP 101 upgrade response - it says
    // nothing about whether the server's spawned `handle_socket` task
    // (src/stream.rs) has actually been polled far enough to reach
    // `manager.subscribe_events()`. That call happens unconditionally near
    // the top of `handle_socket`, strictly before its `tokio::select!` loop,
    // but on a busy/oversubscribed runner the task may simply not have been
    // scheduled yet by the time we get here. If `sim.stop()` ran first, the
    // collector's PLC read failure could fire `PlcDisconnected` on the
    // `broadcast` channel before this connection's `events.recv()` is even
    // listening; `broadcast` has no replay for late subscribers, so the
    // event would be lost and `recv_matching` below would block the full 5s
    // and panic (stream.rs:349).
    //
    // Fix: establish a real subscribe + await its initial snapshot first
    // (same round trip as
    // `subscribe_exact_tag_gets_initial_snapshot_then_on_change_data`
    // above). Receiving *any* reply for this subscription id is
    // proof-by-round-trip that `handle_socket` has already executed past
    // `manager.subscribe_events()` (it is called exactly once,
    // unconditionally, strictly before the loop that both handles the
    // incoming "subscribe" message and sends this reply - see
    // src/stream.rs), so only after this can `sim.stop()` run without
    // racing the event subscription. The pre-check above already proved a
    // Good seeded value, so the snapshot here is only used as the
    // subscribe-events liveness round trip (not as a value assertion).
    send_json(
        &mut ws,
        json!({ "op": "subscribe", "id": 1, "tags": ["line1.fast.temp01"], "mode": "on_change" }),
    )
    .await;
    recv_matching(&mut ws, |m| m["op"] == "data" && m["id"] == 1).await;

    sim.stop();

    // Slightly wider than the default 5s recv_matching budget: under a
    // loaded Windows CI runner the disconnect is still bounded by
    // `response_timeout` (500ms in `fast_options`), but draining competing
    // WS frames before the event can stretch wall time.
    let event = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match ws.next().await {
                Some(Ok(WsMessage::Text(text))) => {
                    let value: Value =
                        serde_json::from_str(&text).expect("server should send valid JSON");
                    if value["op"] == "event" && value["kind"] == "plc_disconnected" {
                        return value;
                    }
                }
                Some(Ok(WsMessage::Ping(_))) | Some(Ok(WsMessage::Pong(_))) => continue,
                Some(Ok(other)) => panic!("unexpected non-text ws message: {other:?}"),
                Some(Err(err)) => panic!("ws error while waiting for plc_disconnected: {err}"),
                None => panic!("connection closed while waiting for plc_disconnected"),
            }
        }
    })
    .await
    .expect("timed out waiting for plc_disconnected");
    assert_eq!(event["connection"], "line1");
}

// ---------------------------------------------------------------------------
// 8. バックプレッシャ切断: 送信キューが満杯になったら切断される
// ---------------------------------------------------------------------------

// H7フォローアップ（2026-08-09、フレーク根治）: このテストだけ意図的に
// デフォルトの current_thread（単一スレッド）ランタイムを使う -
// `multi_thread`/`worker_threads = 2` に**戻さないこと**（フレークが
// 再発する）。根本原因: 送信経路は `evaluate()`（`src/stream.rs` の同期関数、
// tick 分岐から呼ばれる）が `enqueue()` 経由で `data_tx.try_send()` を
// `.await` を挟まないタイトループで回すのに対し、`writer_task`
// (`src/stream.rs`) は別 `tokio::spawn` タスクとしてキューを drain する。
// worker_threads = 2 だとこの writer_task が2本目のワーカー上で
// **同時に** drain してしまい、`try_send` が `Full` を一度も観測できず
// バックプレッシャ切断（`enqueue`、code 1013）が発火しないままテストの
// 5秒待ちがタイムアウトする（= フレーク）。current_thread
// （ワーカースレッド1本）ならこのタスクを同時に動かす2本目のスレッドが
// 存在しないため、`evaluate()` の同期・await 無しファンアウトが完走する
// までの間 writer_task は一切 drain できない - よって
// `subscription_count`（`OUTBOUND_QUEUE_CAPACITY + 64` = 320）本の
// `try_send` は容量256のキューに対して257本目で確実にオーバーフローし、
// `enqueue` が確定的に close を発火する。
#[tokio::test]
async fn a_slow_subscriber_gets_disconnected_once_the_outbound_queue_fills() {
    let app = test_app("backpressure").await;
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 0);

    let conn = PlcConnectionService::new(app.pool.clone())
        .create(conn_input("line1", sim.addr.port()))
        .await
        .unwrap();
    // A short period so many on_change data messages can be produced quickly
    // by toggling the register value every eval tick.
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("temp01", group.id, "40001", "i16"))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild after seeding");

    assert!(
        wait_until(Duration::from_secs(10), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get("tag:1"))
                .is_some()
        })
        .await
    );

    let mut ws = connect_ws(&app.ws_url("/api/v1/stream"), Some(&app.token))
        .await
        .expect("ws handshake should succeed");

    // `on_change` produces at most 1 `data` message per subscription per
    // 250ms eval tick (design §5.2 要件2) - a single tag change can never
    // overflow a 256-capacity queue on its own. So instead we open `N` (>
    // `OUTBOUND_QUEUE_CAPACITY`) subscriptions on the same tag: one value
    // change then fans out to all of them inside a *single* `evaluate()`
    // call, which enqueues via non-blocking `try_send` in a tight loop with
    // no `.await` in between (`crate::stream::evaluate`). That makes the
    // overflow deterministic (bounded by subscription count, not by wall-clock
    // timing/network scheduling) rather than a flaky race against how fast
    // the writer task/OS socket happens to drain.
    let subscription_count: i64 = OUTBOUND_QUEUE_CAPACITY as i64 + 64;
    for id in 1..=subscription_count {
        send_json(
            &mut ws,
            json!({ "op": "subscribe", "id": id, "tags": ["line1.fast.temp01"], "mode": "on_change" }),
        )
        .await;
        recv_matching(&mut ws, move |m| m["op"] == "data" && m["id"] == id).await;
    }

    // Deliberately stop reading from here on (that's the point: a slow/stuck
    // subscriber) and flip the value once - `evaluate()`'s next tick tries
    // to enqueue `subscription_count` messages for a queue of capacity
    // `OUTBOUND_QUEUE_CAPACITY`, which must overflow.
    sim.set_holding_register(0, 999);

    // The server must close the connection - reading further eventually
    // yields a Close frame (or the stream simply ends). T18-4b (banto-hub-t18
    // -design.md「T18-4b 選択購読と再接続堅牢化」): the monitor UI shows a
    // dedicated message when the disconnect is specifically the backpressure
    // close code (1013, `stream.rs::BACKPRESSURE_CLOSE_CODE`) rather than a
    // generic reconnect, so this test also pins the wire-level code - not
    // just "some" disconnect happened.
    let close_frame = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                Some(Ok(WsMessage::Close(frame))) => return frame,
                Some(Ok(_)) => continue,
                Some(Err(_)) | None => return None,
            }
        }
    })
    .await
    .unwrap_or(None);

    let frame = close_frame
        .expect("server should disconnect a slow subscriber with a Close frame carrying a code");
    assert_eq!(
        u16::from(frame.code),
        1013,
        "backpressure disconnect should use close code 1013: {frame:?}"
    );

    sim.stop();
}

// ---------------------------------------------------------------------------
// 9. `Sec-WebSocket-Protocol` bearer フォールバック（T10、banto-hub のライブ
//    タグモニタ向けにブラウザ WS 認証の欠落を埋めた変更 -
//    `rest.rs::extract_ws_protocol_token` の doc comment 参照）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn valid_session_token_via_subprotocol_header_authenticates_and_streams_data() {
    let app = test_app("ws-subprotocol-auth-ok").await;
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 42); // 40001

    let conn = PlcConnectionService::new(app.pool.clone())
        .create(conn_input("line1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("temp01", group.id, "40001", "i16"))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild after seeding");

    assert!(
        wait_until(Duration::from_secs(10), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get("tag:1"))
                .map(|s| s.value)
                == Some(Some(42.0))
        })
        .await,
        "collector should observe the initial simulator value"
    );

    // No `Authorization` header at all - only `Sec-WebSocket-Protocol:
    // "bearer, {token}"`, exactly what a browser `new WebSocket(url,
    // ['bearer', token])` call sends.
    let mut ws = connect_ws_via_subprotocol(&app.ws_url("/api/v1/stream"), &app.token)
        .await
        .expect("ws handshake via Sec-WebSocket-Protocol should succeed");

    send_json(
        &mut ws,
        json!({ "op": "subscribe", "id": 1, "tags": ["line1.fast.temp01"], "mode": "on_change" }),
    )
    .await;

    let snapshot = recv_matching(&mut ws, |m| m["op"] == "data" && m["id"] == 1).await;
    let values = snapshot["values"].as_array().unwrap();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0]["tag"], "line1.fast.temp01");
    assert_eq!(values[0]["v"], 42.0);
    assert_eq!(values[0]["q"], "good");

    sim.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_subprotocol_header_is_still_rejected_with_401() {
    let app = test_app("ws-subprotocol-auth-reject").await;

    // Just "bearer" with no second part.
    let err = connect_ws_via_subprotocol_raw(&app.ws_url("/api/v1/stream"), "bearer")
        .await
        .expect_err("a subprotocol header with no token part should be rejected");
    match err {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            assert_eq!(response.status().as_u16(), 401, "{response:?}");
        }
        other => panic!("expected an HTTP-level rejection, got {other:?}"),
    }

    // Completely unrelated subprotocols (no "bearer" first element).
    let err = connect_ws_via_subprotocol_raw(&app.ws_url("/api/v1/stream"), "chat, superchat")
        .await
        .expect_err("unrelated subprotocols should be rejected");
    match err {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            assert_eq!(response.status().as_u16(), 401, "{response:?}");
        }
        other => panic!("expected an HTTP-level rejection, got {other:?}"),
    }

    // No header at all, same as the existing no-Authorization test - sanity
    // check that this test's server setup rejects like every other test.
    let err = connect_ws(&app.ws_url("/api/v1/stream"), None)
        .await
        .expect_err("no auth at all should still be rejected");
    match err {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            assert_eq!(response.status().as_u16(), 401, "{response:?}");
        }
        other => panic!("expected an HTTP-level rejection, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 10. H10 ③(Option B、docs/h10-3-read-scope-proposal.md §5・§6): per-tag
//     read スコープは購読解決(`crate::subscribe_core::resolve`)の結果を
//     交差させる。`read:{tag}` キーはワイルドカード `*` 購読でもスコープ内
//     のタグしか受信できない。素の `read` キーは従来どおり全件受信する
//     (テスト6 `stream_requires_auth_and_read_scope` の拡張 - あちらは
//     「read スコープを一切持たないキーは 403」、ここは「read はあるが
//     タグ単位に絞られたキーの受信内容」)
// ---------------------------------------------------------------------------

/// テスト10共通のフィクスチャ: `line1.fast.temp01`(tag:1)・
/// `line2.slow.press01`(tag:2)を、同一シミュレータの別レジスタに割り当てて
/// 別接続・別グループで作る(1プロセスに複数シミュレータを立てなくても
/// host:port の重複は `plc_connections` に一意制約が無く許容されるため
/// 問題ない)。呼び出し元は続けて `app.manager.rebuild()` を呼ぶこと。
async fn seed_two_connections_two_tags(app: &TestApp, sim_port: u16) {
    let conn1 = PlcConnectionService::new(app.pool.clone())
        .create(conn_input("line1", sim_port))
        .await
        .unwrap();
    let group1 = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn1.id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("temp01", group1.id, "40001", "i16")) // tag:1
        .await
        .unwrap();

    let conn2 = PlcConnectionService::new(app.pool.clone())
        .create(conn_input("line2", sim_port))
        .await
        .unwrap();
    let group2 = CollectionGroupService::new(app.pool.clone())
        .create(group_input("slow", conn2.id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("press01", group2.id, "40002", "i16")) // tag:2
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wildcard_subscription_with_per_tag_read_scope_only_receives_in_scope_tag() {
    let app = test_app("per-tag-read-scope").await;
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 1); // line1.fast.temp01 (40001)
    sim.set_holding_register(1, 2); // line2.slow.press01 (40002)

    seed_two_connections_two_tags(&app, sim.addr.port()).await;
    app.manager.rebuild().await.expect("rebuild after seeding");

    assert!(
        wait_until(Duration::from_secs(10), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get("tag:1"))
                .map(|s| s.value)
                == Some(Some(1.0))
                && app
                    .manager
                    .current_values()
                    .and_then(|c| c.get("tag:2"))
                    .map(|s| s.value)
                    == Some(Some(2.0))
        })
        .await,
        "collector should observe both seeded tags"
    );

    let issued = app
        .api_keys
        .issue(
            "line1-temp01-reader",
            vec!["read:line1.fast.temp01".to_string()],
            None,
        )
        .await
        .expect("issue should succeed");

    let mut ws = connect_ws(&app.ws_url("/api/v1/stream"), Some(&issued.key))
        .await
        .expect("ws handshake should succeed");

    send_json(
        &mut ws,
        json!({ "op": "subscribe", "id": 1, "tags": ["*"], "mode": "on_change" }),
    )
    .await;

    let snapshot = recv_matching(&mut ws, |m| m["op"] == "data" && m["id"] == 1).await;
    let values = snapshot["values"].as_array().unwrap();
    assert_eq!(
        values.len(),
        1,
        "wildcard subscription must resolve only the in-scope tag: {values:?}"
    );
    assert_eq!(values[0]["tag"], "line1.fast.temp01");

    // スコープ外(line2.slow.press01)の値変更は届かない。#244: スコープ内
    // タグの spurious な on_change 再送を拾って誤って落ちないよう、
    // 「スコープ外タグを含むデータが来ないこと」だけを検証する
    // `assert_no_data_carrying_tag` を使う（`assert_no_more_data_for` は
    // 「一切 data が来ないこと」を要求してしまうため使わない）。
    sim.set_holding_register(1, 99);
    tokio::time::sleep(Duration::from_millis(400)).await; // give the eval loop a chance to (wrongly) fire
    assert_no_data_carrying_tag(&mut ws, 1, "line2.slow.press01").await;

    // スコープ内(line1.fast.temp01)の値変更は引き続き届く。旧値(1)を載せた
    // spurious な quality-only on_change バッチが先に届きうるレース(H7 ⑤ -
    // grpc.rs の drain_until_value で直した gRPC 姉妹テストと同一。PR #139 参照)
    // を、値 42 を載せたバッチが来るまで recv_matching の述語で drain して吸収する。
    sim.set_holding_register(0, 42);
    let changed = recv_matching(&mut ws, |m| {
        m["op"] == "data" && m["id"] == 1 && m["values"][0]["v"] == 42.0
    })
    .await;
    let values = changed["values"].as_array().unwrap();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0]["tag"], "line1.fast.temp01");
    assert_eq!(values[0]["v"], 42.0);

    sim.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wildcard_subscription_with_bare_read_scope_receives_every_tag() {
    let app = test_app("bare-read-scope-stream").await;
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 1); // line1.fast.temp01 (40001)
    sim.set_holding_register(1, 2); // line2.slow.press01 (40002)

    seed_two_connections_two_tags(&app, sim.addr.port()).await;
    app.manager.rebuild().await.expect("rebuild after seeding");

    assert!(
        wait_until(Duration::from_secs(10), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get("tag:1"))
                .map(|s| s.value)
                == Some(Some(1.0))
                && app
                    .manager
                    .current_values()
                    .and_then(|c| c.get("tag:2"))
                    .map(|s| s.value)
                    == Some(Some(2.0))
        })
        .await,
        "collector should observe both seeded tags"
    );

    let issued = app
        .api_keys
        .issue("bare-reader", vec!["read".to_string()], None)
        .await
        .expect("issue should succeed");

    let mut ws = connect_ws(&app.ws_url("/api/v1/stream"), Some(&issued.key))
        .await
        .expect("ws handshake should succeed");

    send_json(
        &mut ws,
        json!({ "op": "subscribe", "id": 1, "tags": ["*"], "mode": "on_change" }),
    )
    .await;

    let snapshot = recv_matching(&mut ws, |m| m["op"] == "data" && m["id"] == 1).await;
    let mut tags: Vec<String> = snapshot["values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["tag"].as_str().unwrap().to_string())
        .collect();
    tags.sort();
    assert_eq!(
        tags,
        vec![
            "line1.fast.temp01".to_string(),
            "line2.slow.press01".to_string()
        ],
        "a bare `read` key must keep receiving every tag (S2 backward compat)"
    );

    sim.stop();
}

// ---------------------------------------------------------------------------
// 11. T18-4b（docs/banto-hub-t18-design.md「T18-4b 選択購読と再接続堅牢化」、
//     TAG-UX-H の一部）: グループワイルドカード購読
//     (`"{connection}.{group}.*"`、`subscribe_core.rs::TagPattern::GroupWildcard`)
//     は宣言したグループのタグだけを対象にする - banto-hub のモニタ画面が
//     ツリーでグループを選んだときに送る購読と同じ形。テスト10系と同じ
//     `seed_two_connections_two_tags` フィクスチャ（別接続・別グループの2タグ）
//     を流用し、テスト10 `wildcard_subscription_with_per_tag_read_scope_...`
//     の「スコープ外は届かない/スコープ内は届く」検証パターンをそのまま
//     踏襲する。
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn group_wildcard_subscription_only_receives_its_own_group_tag() {
    let app = test_app("group-wildcard-scope").await;
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 1); // line1.fast.temp01 (40001)
    sim.set_holding_register(1, 2); // line2.slow.press01 (40002)

    seed_two_connections_two_tags(&app, sim.addr.port()).await;
    app.manager.rebuild().await.expect("rebuild after seeding");

    assert!(
        wait_until(Duration::from_secs(10), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get("tag:1"))
                .map(|s| s.value)
                == Some(Some(1.0))
                && app
                    .manager
                    .current_values()
                    .and_then(|c| c.get("tag:2"))
                    .map(|s| s.value)
                    == Some(Some(2.0))
        })
        .await,
        "collector should observe both seeded tags"
    );

    let mut ws = connect_ws(&app.ws_url("/api/v1/stream"), Some(&app.token))
        .await
        .expect("ws handshake should succeed");

    send_json(
        &mut ws,
        json!({ "op": "subscribe", "id": 1, "tags": ["line1.fast.*"], "mode": "on_change" }),
    )
    .await;

    let snapshot = recv_matching(&mut ws, |m| m["op"] == "data" && m["id"] == 1).await;
    let values = snapshot["values"].as_array().unwrap();
    assert_eq!(
        values.len(),
        1,
        "group wildcard subscription must resolve only its own group's tag: {values:?}"
    );
    assert_eq!(values[0]["tag"], "line1.fast.temp01");

    // 別グループ(line2.slow.press01)の値変更は届かない。#244: スコープ内
    // タグの spurious な on_change 再送を拾って誤って落ちないよう、
    // 「スコープ外タグを含むデータが来ないこと」だけを検証する
    // `assert_no_data_carrying_tag` を使う（`assert_no_more_data_for` は
    // 「一切 data が来ないこと」を要求してしまうため使わない）。
    sim.set_holding_register(1, 99);
    tokio::time::sleep(Duration::from_millis(400)).await; // give the eval loop a chance to (wrongly) fire
    assert_no_data_carrying_tag(&mut ws, 1, "line2.slow.press01").await;

    // 自グループ(line1.fast.temp01)の値変更は引き続き届く。旧値(1)を載せた
    // spurious な quality-only on_change バッチが先に届きうるレース(テスト10
    // と同じ理由・同じ対策)を、値 42 を載せたバッチが来るまで recv_matching
    // の述語で drain して吸収する。
    sim.set_holding_register(0, 42);
    let changed = recv_matching(&mut ws, |m| {
        m["op"] == "data" && m["id"] == 1 && m["values"][0]["v"] == 42.0
    })
    .await;
    let values = changed["values"].as_array().unwrap();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0]["tag"], "line1.fast.temp01");
    assert_eq!(values[0]["v"], 42.0);

    sim.stop();
}

// ---------------------------------------------------------------------------
// 12. 管理系 WS `/api/tag-stream`（`admin_tag_stream_router`、設計 §5.6・
//     2026-08-31 オーナー決定「案A」の見落とし是正）: ライブタグモニタ
//     （`tagMonitorAdmin.ts`）が試運転モード中も繋がるようにするための追加。
//     ハンドラ本体は`/api/v1/stream`と共有（`crate::stream::ws_upgrade`）
//     なので、購読プロトコル自体（`on_change`・ワイルドカード等）はテスト
//     1〜11 で既に確認済み - ここでは認証境界だけを確認する。
// ---------------------------------------------------------------------------

/// 試運転モード中は `/api/tag-stream` が `Authorization` も
/// `Sec-WebSocket-Protocol` も無しで接続でき、購読・データ受信まで通しで
/// 動く - `tagMonitorAdmin.ts` が試運転モード中に行う接続と同型。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_tag_stream_allows_unauthenticated_connection_during_commissioning() {
    let app = test_app_unlocked("admin-stream-commissioning").await;
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 7); // 40001

    let conn = PlcConnectionService::new(app.pool.clone())
        .create(conn_input("line1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("temp01", group.id, "40001", "i16"))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild after seeding");

    assert!(
        wait_until(Duration::from_secs(10), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get("tag:1"))
                .map(|s| s.value)
                == Some(Some(7.0))
        })
        .await,
        "collector should observe the initial simulator value"
    );

    // No `Authorization` header, no `Sec-WebSocket-Protocol` offer at all -
    // exactly what `tagMonitorAdmin.ts::connectOnce` sends while
    // `sessionStore.commissioningMode` is true.
    let mut ws = connect_ws(&app.ws_url("/api/tag-stream"), None)
        .await
        .expect("unauthenticated ws handshake should succeed during commissioning mode");

    send_json(
        &mut ws,
        json!({ "op": "subscribe", "id": 1, "tags": ["line1.fast.temp01"], "mode": "on_change" }),
    )
    .await;

    let snapshot = recv_matching(&mut ws, |m| m["op"] == "data" && m["id"] == 1).await;
    let values = snapshot["values"].as_array().unwrap();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0]["tag"], "line1.fast.temp01");
    assert_eq!(values[0]["v"], 7.0);
    assert_eq!(values[0]["q"], "good");

    sim.stop();
}

/// 対照実験: ロックダウン済みでは `/api/tag-stream` も認証が要る -
/// `Authorization`/`Sec-WebSocket-Protocol` どちらも無い接続は401、有効な
/// セッション bearer を `Sec-WebSocket-Protocol: bearer, <token>` で運べば
/// 接続できる（`valid_session_token_via_subprotocol_header_authenticates_and_streams_data`
/// と同型 - `require_auth_or_commissioning`側に追加した
/// `extract_ws_protocol_token`フォールバックの確認）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_tag_stream_requires_auth_when_locked_down() {
    let app = test_app("admin-stream-locked-down").await;

    let err = connect_ws(&app.ws_url("/api/tag-stream"), None)
        .await
        .expect_err("no auth at all should be rejected once locked down");
    match err {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            assert_eq!(response.status().as_u16(), 401, "{response:?}");
        }
        other => panic!("expected an HTTP-level rejection, got {other:?}"),
    }

    // `Authorization` ヘッダも通常どおり通る（ブラウザ以外のクライアント、
    // あるいはこのテストのように直接ヘッダを付けられる場合の経路）。
    let ok = connect_ws(&app.ws_url("/api/tag-stream"), Some(&app.token)).await;
    assert!(
        ok.is_ok(),
        "a valid session token via Authorization should be accepted: {ok:?}"
    );

    // ブラウザの `WebSocket` が実際に使う経路: `Sec-WebSocket-Protocol:
    // bearer, <token>`。
    let ok = connect_ws_via_subprotocol(&app.ws_url("/api/tag-stream"), &app.token).await;
    assert!(
        ok.is_ok(),
        "a valid session token via Sec-WebSocket-Protocol should be accepted: {ok:?}"
    );
}

// ---------------------------------------------------------------------------
// #430: API キーで開いたストリームの接続中の再検証
// ---------------------------------------------------------------------------

/// テストの再検証の間隔と上限（本番は 15 秒 / 5 秒）。
const TEST_REVALIDATION: StreamRevalidationTiming = StreamRevalidationTiming {
    interval: Duration::from_millis(300),
    timeout: Duration::from_millis(500),
};

/// 使えなくしてから close が届くまでの上限: 1 周期 + 照合 1 回の上限 + 余裕。
fn close_bound() -> Duration {
    TEST_REVALIDATION.interval + TEST_REVALIDATION.timeout + Duration::from_secs(1)
}

/// close フレームを待って (code, reason) を返す（テキスト・ping は読み飛ばす）。
async fn wait_for_close(ws: &mut WsStream, bound: Duration) -> (u16, String) {
    tokio::time::timeout(bound, async {
        loop {
            match ws.next().await {
                Some(Ok(WsMessage::Close(Some(frame)))) => {
                    return (u16::from(frame.code), frame.reason.to_string())
                }
                Some(Ok(WsMessage::Close(None))) | None | Some(Err(_)) => {
                    panic!("stream ended without a close frame")
                }
                Some(Ok(_)) => continue,
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no close frame within {bound:?}"))
}

/// タグ `line1.fast.temp01`（値 `value`）を 1 つ用意して、収集が値を読むまで
/// 待つ。
async fn seed_one_tag(app: &TestApp, sim: &Simulator, value: u16) {
    sim.set_holding_register(0, value);
    let conn = PlcConnectionService::new(app.pool.clone())
        .create(conn_input("line1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    TagService::new(app.pool.clone())
        .create(tag_input("temp01", group.id, "40001", "i16"))
        .await
        .unwrap();
    app.manager.rebuild().await.expect("rebuild after seeding");
    assert!(
        wait_until(Duration::from_secs(10), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get("tag:1"))
                .map(|s| s.value)
                == Some(Some(f64::from(value)))
        })
        .await,
        "collector should observe the simulator value"
    );
}

/// API キーで開き、`interval` 購読して最初の値を受け取ったストリーム。
async fn open_interval_stream(app: &TestApp, key: &str) -> WsStream {
    open_interval_stream_at(app, "/api/v1/stream", key).await
}

/// `path` を `token`（API キーまたはセッション）で開き、`interval` 購読して
/// 最初の値を受け取ったストリーム。
async fn open_interval_stream_at(app: &TestApp, path: &str, token: &str) -> WsStream {
    let mut ws = connect_ws(&app.ws_url(path), Some(token))
        .await
        .expect("WS handshake should succeed");
    send_json(
        &mut ws,
        json!({ "op": "subscribe", "id": 1, "tags": ["line1.fast.temp01"], "mode": "interval", "interval_ms": 250 }),
    )
    .await;
    recv_matching(&mut ws, |m| m["op"] == "data" && m["id"] == 1).await;
    ws
}

/// `count` 個の値を受け取る（途中で閉じたら `recv_matching` が失敗させる）。
/// 購読は 250ms ごとなので、`count` 個で `count * 250ms` 以上かかる。
async fn receive_values(ws: &mut WsStream, count: usize) {
    for _ in 0..count {
        recv_matching(ws, |m| m["op"] == "data" && m["id"] == 1).await;
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// 失効・トリップ・期限切れのあと、そのキーで開いていたストリームは 1 周期
/// 以内に 1008 と理由文で閉じる。ほかのキーのストリームは開いたまま。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_key_streams_close_within_an_interval_after_revoke_trip_or_expiry() {
    let app = test_app_with_revalidation("revalidate-api-key", TEST_REVALIDATION).await;
    let read = || vec!["read".to_string()];
    let revoked = app.api_keys.issue("revoked", read(), None).await.unwrap();
    let tripped = app.api_keys.issue("tripped", read(), None).await.unwrap();
    let expired = app.api_keys.issue("expired", read(), None).await.unwrap();
    let kept = app.api_keys.issue("kept", read(), None).await.unwrap();

    let url = app.ws_url("/api/v1/stream");
    let mut revoked_ws = connect_ws(&url, Some(&revoked.key)).await.unwrap();
    let mut tripped_ws = connect_ws(&url, Some(&tripped.key)).await.unwrap();
    let mut expired_ws = connect_ws(&url, Some(&expired.key)).await.unwrap();
    let mut kept_ws = connect_ws(&url, Some(&kept.key)).await.unwrap();

    app.api_keys.revoke(revoked.id).await.unwrap();
    let started = tokio::time::Instant::now();
    let (code, reason) = wait_for_close(&mut revoked_ws, close_bound()).await;
    assert_eq!(
        (code, reason.as_str()),
        (REVOKED_CLOSE_CODE, "api_key_revoked")
    );
    eprintln!("revoked: closed after {:?}", started.elapsed());

    app.api_keys.trip(tripped.id).await.unwrap();
    let (code, reason) = wait_for_close(&mut tripped_ws, close_bound()).await;
    assert_eq!(
        (code, reason.as_str()),
        (REVOKED_CLOSE_CODE, "api_key_tripped")
    );

    sqlx::query("UPDATE api_keys SET expires_at = ? WHERE id = ?")
        .bind((now_ms() - 1).to_string())
        .bind(expired.id)
        .execute(&app.pool)
        .await
        .unwrap();
    let (code, reason) = wait_for_close(&mut expired_ws, close_bound()).await;
    assert_eq!(
        (code, reason.as_str()),
        (REVOKED_CLOSE_CODE, "api_key_expired")
    );

    // 使えるキーのストリームは、いくつもの照合を経ても開いたまま。
    send_json(&mut kept_ws, json!({ "op": "ping" })).await;
    let pong = recv_matching(&mut kept_ws, |m| m["op"] == "pong").await;
    assert_eq!(pong["op"], "pong");
}

/// キーを照合できない（DB エラー）あいだは値の配信が続き、閉じない。DB が
/// 戻った後の照合で失効が分かれば閉じる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_key_stream_keeps_delivering_while_the_key_store_errors() {
    let app = test_app_with_revalidation("revalidate-db-error", TEST_REVALIDATION).await;
    let sim = Simulator::start().await;
    seed_one_tag(&app, &sim, 11).await;
    let issued = app
        .api_keys
        .issue("db-error", vec!["read".to_string()], None)
        .await
        .unwrap();
    let mut ws = open_interval_stream(&app, &issued.key).await;

    // 照合の SELECT が失敗するようにする（`api_keys` の #434 のテストと同じ手）。
    sqlx::query("ALTER TABLE api_keys RENAME TO api_keys_away")
        .execute(&app.pool)
        .await
        .unwrap();
    // 10 個 = 2.5 秒以上。そのあいだに照合（300ms ごと）は何度も失敗する。
    receive_values(&mut ws, 10).await;

    sqlx::query("ALTER TABLE api_keys_away RENAME TO api_keys")
        .execute(&app.pool)
        .await
        .unwrap();
    app.api_keys.revoke(issued.id).await.unwrap();
    let (code, reason) = wait_for_close(&mut ws, close_bound()).await;
    assert_eq!(
        (code, reason.as_str()),
        (REVOKED_CLOSE_CODE, "api_key_revoked")
    );

    sim.stop();
}

/// 照合が返らない（DB の接続をすべて塞ぐ）あいだも値の配信が続き、閉じない。
/// 塞いでいる間に失効させ、塞ぎを解いた後の照合で閉じる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_key_stream_keeps_delivering_while_the_key_check_does_not_return() {
    let app = test_app_with_revalidation("revalidate-db-hang", TEST_REVALIDATION).await;
    let sim = Simulator::start().await;
    seed_one_tag(&app, &sim, 12).await;
    let issued = app
        .api_keys
        .issue("db-hang", vec!["read".to_string()], None)
        .await
        .unwrap();
    let mut ws = open_interval_stream(&app, &issued.key).await;

    // 接続プールを使い切る: 照合は接続の取得で止まり、上限（500ms）で打ち切られる。
    let max = app.pool.options().get_max_connections();
    let mut held = Vec::new();
    for _ in 0..max {
        held.push(
            tokio::time::timeout(Duration::from_secs(5), app.pool.acquire())
                .await
                .expect("acquire should not wait long")
                .expect("acquire"),
        );
    }
    // 12 個 = 3 秒以上。照合（1 周期 800ms: 待ち 300ms + 打ち切り 500ms）は
    // 3 回以上返らない。
    receive_values(&mut ws, 12).await;

    sqlx::query("UPDATE api_keys SET revoked_at = datetime('now') WHERE id = ?")
        .bind(issued.id)
        .execute(&mut *held[0])
        .await
        .unwrap();
    drop(held);
    let (code, reason) = wait_for_close(&mut ws, close_bound()).await;
    assert_eq!(
        (code, reason.as_str()),
        (REVOKED_CLOSE_CODE, "api_key_revoked")
    );

    sim.stop();
}

// ---------------------------------------------------------------------------
// #430: セッションで開いたストリームの接続中の再検証（`AuthState::revalidate`、
// tyaro/banto#239）
// ---------------------------------------------------------------------------

/// このブロックのテストで作るアカウントのパスワード。
const TEST_PASSWORD: &str = "password123";

/// セッションで開けるストリームの経路（ロックダウン後）。
const SESSION_STREAM_PATHS: [&str; 2] = ["/api/tag-stream", "/api/v1/stream"];

/// アカウント `username`（ロール `role`）を作り、ログインしたトークンを返す。
async fn create_and_login(app: &TestApp, username: &str, role: Role) -> (i64, String) {
    let user = app
        .users
        .create_user(username, TEST_PASSWORD, username, role)
        .await
        .expect("create_user");
    (user.id, login_as(app, username).await)
}

/// `username` でもう 1 つセッションを作る（ほかの端末）。
async fn login_as(app: &TestApp, username: &str) -> String {
    app.auth
        .login(username, TEST_PASSWORD)
        .await
        .expect("login should succeed")
}

/// `token` で両方の経路を開く（購読はしない）。
async fn open_session_streams(app: &TestApp, token: &str) -> Vec<WsStream> {
    let mut streams = Vec::new();
    for path in SESSION_STREAM_PATHS {
        streams.push(
            connect_ws(&app.ws_url(path), Some(token))
                .await
                .unwrap_or_else(|err| panic!("session WS handshake on {path}: {err}")),
        );
    }
    streams
}

/// ストリームが開いていて、ループが止まっていない（ping に pong が返る）。
async fn assert_open(ws: &mut WsStream) {
    send_json(ws, json!({ "op": "ping" })).await;
    let pong = recv_matching(ws, |m| m["op"] == "pong").await;
    assert_eq!(pong["op"], "pong");
}

/// すべてのストリームが、1 周期 + 照合 1 回の上限 + 余裕のうちに 1008 /
/// `session_revoked` で閉じる（同時に待つ）。
async fn assert_all_closed_as_session_revoked(streams: &mut [WsStream], what: &str) {
    let closes = futures_util::future::join_all(
        streams
            .iter_mut()
            .map(|ws| wait_for_close(ws, close_bound())),
    )
    .await;
    for (path, (code, reason)) in SESSION_STREAM_PATHS.iter().zip(closes) {
        assert_eq!(
            (code, reason.as_str()),
            (REVOKED_CLOSE_CODE, SESSION_REVOKED_REASON),
            "{what}: {path}"
        );
    }
}

/// `POST /api/auth/change-password` を `token` で呼ぶ（本番の経路。呼んだ
/// トークンだけ新しい世代に付け替えられる）。
async fn change_password_via_rest(app: &TestApp, token: &str, current: &str, new: &str) {
    use tower::ServiceExt;
    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/api/auth/change-password")
        .header("Authorization", format!("Bearer {token}"))
        .header("X-Banto-Client", "banto")
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(
            json!({ "currentPassword": current, "newPassword": new }).to_string(),
        ))
        .unwrap();
    let response = app.router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
}

async fn admin_id(app: &TestApp) -> i64 {
    app.users
        .get_by_username("admin")
        .await
        .unwrap()
        .expect("admin exists")
        .id
}

/// 削除・降格・（ほかの端末からの）パスワード変更・リセットのあと、その
/// アカウントのセッションで開いていたストリーム（`/api/tag-stream` と
/// `/api/v1/stream`）は 1 周期以内に 1008 / `session_revoked` で閉じる。
/// ほかのアカウントのストリームは開いたまま。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_streams_close_within_an_interval_after_delete_demote_or_password_change() {
    let app = test_app_with_revalidation("revalidate-session", TEST_REVALIDATION).await;
    let admin_id = admin_id(&app).await;
    let (deleted_id, deleted) = create_and_login(&app, "s-deleted", Role::Editor).await;
    let (demoted_id, demoted) = create_and_login(&app, "s-demoted", Role::Editor).await;
    let (_, changed) = create_and_login(&app, "s-changed", Role::Viewer).await;
    let changer = login_as(&app, "s-changed").await;
    let (reset_id, reset) = create_and_login(&app, "s-reset", Role::Viewer).await;
    let (_, bystander) = create_and_login(&app, "s-bystander", Role::Viewer).await;

    let mut deleted_ws = open_session_streams(&app, &deleted).await;
    let mut demoted_ws = open_session_streams(&app, &demoted).await;
    let mut changed_ws = open_session_streams(&app, &changed).await;
    let mut reset_ws = open_session_streams(&app, &reset).await;
    let mut bystander_ws = open_session_streams(&app, &bystander).await;
    for ws in deleted_ws
        .iter_mut()
        .chain(demoted_ws.iter_mut())
        .chain(changed_ws.iter_mut())
        .chain(reset_ws.iter_mut())
        .chain(bystander_ws.iter_mut())
    {
        assert_open(ws).await;
    }

    app.users.delete_user(deleted_id, admin_id).await.unwrap();
    assert_all_closed_as_session_revoked(&mut deleted_ws, "delete").await;

    app.users
        .update_user(demoted_id, "s-demoted", Role::Viewer)
        .await
        .unwrap();
    assert_all_closed_as_session_revoked(&mut demoted_ws, "demote").await;

    // ほかの端末（`changer`）からのパスワード変更。
    change_password_via_rest(&app, &changer, TEST_PASSWORD, "newpassword456").await;
    assert_all_closed_as_session_revoked(&mut changed_ws, "password change").await;

    app.users
        .reset_password(reset_id, "resetpassword789")
        .await
        .unwrap();
    assert_all_closed_as_session_revoked(&mut reset_ws, "password reset").await;

    // ほかのアカウントのストリームは、いくつもの照合を経ても開いたまま。
    for ws in &mut bystander_ws {
        assert_open(ws).await;
    }
}

/// 自分のパスワード変更では、変更に使ったセッションで開いたストリームは閉じず、
/// 値の配信が続く（そのトークンは新しい世代に付け替えられる。照合の途中で
/// 付け替えられる場合は `src/stream.rs` の単体テストで決定的に確かめる）。
/// 同じアカウントのほかの端末のストリームは閉じる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_stream_survives_its_own_password_change() {
    let app = test_app_with_revalidation("revalidate-session-self", TEST_REVALIDATION).await;
    let sim = Simulator::start().await;
    seed_one_tag(&app, &sim, 13).await;
    let (_, own) = create_and_login(&app, "s-self", Role::Viewer).await;
    let other = login_as(&app, "s-self").await;

    let mut own_ws = Vec::new();
    for path in SESSION_STREAM_PATHS {
        own_ws.push(open_interval_stream_at(&app, path, &own).await);
    }
    let mut other_ws = open_session_streams(&app, &other).await;

    change_password_via_rest(&app, &own, TEST_PASSWORD, "newpassword456").await;
    // ほかの端末のストリームが閉じた = 変更後の照合が少なくとも 1 回済んだ。
    assert_all_closed_as_session_revoked(&mut other_ws, "the account's other session").await;
    // 自分のストリームは、そのあとも値を受け取り続ける（8 個 = 2 秒以上 =
    // 照合 6 回以上）。
    for ws in &mut own_ws {
        receive_values(ws, 8).await;
        assert_open(ws).await;
    }

    sim.stop();
}

/// アカウントを照合できない（DB エラー）あいだは、セッションで開いた
/// ストリームも値の配信が続き、閉じない。DB が戻った後の照合で失効が分かれば
/// 閉じる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_streams_keep_delivering_while_the_account_store_errors() {
    let app = test_app_with_revalidation("revalidate-session-db-error", TEST_REVALIDATION).await;
    let sim = Simulator::start().await;
    seed_one_tag(&app, &sim, 14).await;
    let admin_id = admin_id(&app).await;
    let (id, token) = create_and_login(&app, "s-db-error", Role::Viewer).await;
    let mut streams = Vec::new();
    for path in SESSION_STREAM_PATHS {
        streams.push(open_interval_stream_at(&app, path, &token).await);
    }

    // 照合の SELECT（`users` を引く）が失敗するようにする。
    sqlx::query("ALTER TABLE users RENAME TO users_away")
        .execute(&app.pool)
        .await
        .unwrap();
    // 各 10 個 = 2.5 秒以上。そのあいだに照合（300ms ごと）は何度も失敗する。
    for ws in &mut streams {
        receive_values(ws, 10).await;
    }

    sqlx::query("ALTER TABLE users_away RENAME TO users")
        .execute(&app.pool)
        .await
        .unwrap();
    app.users.delete_user(id, admin_id).await.unwrap();
    assert_all_closed_as_session_revoked(&mut streams, "delete after the DB is back").await;

    sim.stop();
}

/// 照合が返らない（DB の接続をすべて塞ぐ）あいだも、セッションで開いた
/// ストリームは値の配信が続き、閉じない。塞いでいる間にアカウントを消し、
/// 塞ぎを解いた後の照合で閉じる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_streams_keep_delivering_while_the_account_check_does_not_return() {
    let app = test_app_with_revalidation("revalidate-session-db-hang", TEST_REVALIDATION).await;
    let sim = Simulator::start().await;
    seed_one_tag(&app, &sim, 15).await;
    let (id, token) = create_and_login(&app, "s-db-hang", Role::Viewer).await;
    let mut streams = Vec::new();
    for path in SESSION_STREAM_PATHS {
        streams.push(open_interval_stream_at(&app, path, &token).await);
    }

    // 接続プールを使い切る: 照合は接続の取得で止まり、上限（500ms）で打ち切られる。
    let max = app.pool.options().get_max_connections();
    let mut held = Vec::new();
    for _ in 0..max {
        held.push(
            tokio::time::timeout(Duration::from_secs(5), app.pool.acquire())
                .await
                .expect("acquire should not wait long")
                .expect("acquire"),
        );
    }
    // 各 12 個 = 3 秒以上。照合（1 周期 800ms: 待ち 300ms + 打ち切り 500ms）は
    // 3 回以上返らない。
    for ws in &mut streams {
        receive_values(ws, 12).await;
    }

    sqlx::query("DELETE FROM users WHERE id = ?")
        .bind(id)
        .execute(&mut *held[0])
        .await
        .unwrap();
    drop(held);
    assert_all_closed_as_session_revoked(&mut streams, "delete after the pool is freed").await;

    sim.stop();
}

// ---------------------------------------------------------------------------
// #440: 試運転モードでトークン無しに開いたストリームの接続中の再検証
// ---------------------------------------------------------------------------

/// 試運転モードでトークン無しに開いた `/api/tag-stream` は、試運転モードの
/// あいだは配信が続き（照合は何度も「使える」）、ロックダウンのあと 1 周期
/// 以内に 1008 / `commissioning_ended` で閉じる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_commissioning_stream_closes_within_an_interval_after_lock_down() {
    let app =
        test_app_unlocked_with_revalidation("revalidate-commissioning", TEST_REVALIDATION).await;
    let sim = Simulator::start().await;
    seed_one_tag(&app, &sim, 21).await;

    let mut ws = connect_ws(&app.ws_url("/api/tag-stream"), None)
        .await
        .expect("unauthenticated ws handshake should succeed during commissioning mode");
    send_json(
        &mut ws,
        json!({ "op": "subscribe", "id": 1, "tags": ["line1.fast.temp01"], "mode": "interval", "interval_ms": 250 }),
    )
    .await;
    recv_matching(&mut ws, |m| m["op"] == "data" && m["id"] == 1).await;
    // 10 個 = 2.5 秒以上。そのあいだに照合（300ms ごと）は何度も「使える」。
    receive_values(&mut ws, 10).await;

    app.commissioning
        .lock_down()
        .await
        .expect("lock_down should succeed");
    let started = tokio::time::Instant::now();
    let (code, reason) = wait_for_close(&mut ws, close_bound()).await;
    assert_eq!(
        (code, reason.as_str()),
        (REVOKED_CLOSE_CODE, COMMISSIONING_ENDED_REASON)
    );
    eprintln!("commissioning: closed after {:?}", started.elapsed());

    sim.stop();
}
