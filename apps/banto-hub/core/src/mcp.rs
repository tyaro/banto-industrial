//! MCP（Model Context Protocol）サーバー(T19 S5、docs/banto-hub-t19-design.md
//! §3.7・UX-41)。
//!
//! ## なぜ自前 JSON-RPC か（オーナー決定 2026-09-04）
//!
//! `rmcp` 等の MCP SDK は導入しない。この crate の他の外部 IF（REST/gRPC/
//! WebSocket/MQTT）と同じく、必要なのは「JSON-RPC 2.0 のごく一部（
//! `initialize`/`notifications/initialized`/`tools/list`/`tools/call`/
//! `ping`）を1本の HTTP エンドポイントへ載せる」ことだけであり、
//! `serde`/`serde_json` 以上の依存を増やす理由がない。トランスポートも
//! 新規プロセスを起こさず、既存の axum サーバーへ `POST /mcp`
//! （Streamable HTTP のうち非ストリーミング・`application/json`
//! サブセット）として統合する - この crate は通知ストリーム（SSE）を
//! 一切出さないので、それ以上の複雑さは不要。
//!
//! ## §3.7「書き込みは既存ゲートを迂回しない」をこのモジュールでどう守るか
//!
//! `write_tag_value` ツールは `crate::write_path::execute_write` を
//! **そのまま**呼ぶ - `crate::rest::v1_write_value`（REST）・
//! `crate::grpc`（gRPC）と全く同じ形で [`crate::write_path::WriteDeps`] を
//! 組み立てて委譲するだけで、catalog 解決・writable・実効 enabled・
//! シミュレーション・プロトコル対応・受付トグル・レート制限・値変換・
//! log-before-write のどれ一つとしてこのファイルには存在しない
//! （`crate::write_path`のモジュール doc「二重実装は絶対に不可」を
//! そのまま踏襲）。`parse_requested_value`（REST の `{"v": ...}` 変換と
//! 同じ関数、`crate::rest`から`pub(crate)`で借りる）も共有する。
//!
//! `write_recipe` ツール（T20 機能③b、レシピ一括書き込み）も同じ規律 -
//! ゲート本体（重複タグ検出・事前ゲート all-or-nothing・レート制限・
//! commit）を一切再実装せず、`crate::rest::v1_write_values_batch`（REST の
//! `/api/v1/values/batch`）と全く同じ形で
//! [`crate::write_path::execute_write_batch`] へ委譲するだけ（[`tool_write_recipe`]
//! 参照）。
//!
//! ## ロックダウン連動の安全ポリシー（オーナー決定 2026-09-04、最重要）
//!
//! `crate::commissioning::CommissioningState::is_locked_down()` が唯一の
//! 分岐点:
//!
//! - **ロックダウン前**（試運転モード、`false`）: MCP はフル機能。
//!   `write_tag_value`/`write_recipe` は書き込み対象タグそれぞれの
//!   `ctx.has_write_scope(tag)` を検査した後、通常どおり
//!   `execute_write`/`execute_write_batch` を実行する（REST と同じゲート・
//!   監査・レート制限）。
//! - **ロックダウン後**（本番、`true`）: `write_tag_value`/`write_recipe` は
//!   **`execute_write`/`execute_write_batch` を一切呼ばない**。安全方向に
//!   のみ倒し、「何を書き込むべきか」を助言する `isError` の `tools/call`
//!   結果だけを返す - 実際の操作は人が管理 UI から行う前提（このモジュールの
//!   [`write_tag_value_advisory`]/[`write_recipe_advisory`] 参照）。この
//!   チェックは write スコープの有無より**先**に行う - 助言はロックダウン
//!   前から本人が渡した `tag`/`value` をそのまま読み上げるだけなので、
//!   スコープ不足の API キーに見せても新規の情報漏洩にはならない（この
//!   モジュールの doc comment よりも詳しい理由は [`write_tag_value`] 実装
//!   コメント参照）。読み取り系3ツールはロックダウンの前後を問わず同じ
//!   挙動 - `has_any_read`/`can_read_value` によるスコープ判定のみで決まる。
//!
//! ## 認証（設計 §3.7・実装指示 B）
//!
//! `POST /mcp` は有効な `bh_` API キー必須。`crate::rest::require_tag_space_auth`
//! と同じ経路（`ApiKeysService::lookup` + `CollectorManager::clock()`）を
//! 使うが、判定はこのモジュール専用に単純化してある:
//! セッション token・欠如・無効・revoked・tripped・expired は
//! **すべて 401**（REST のように revoked=401/tripped=403 と分けない -
//! MCP クライアントに「認証できたが権限が無い」と「そもそも認証できない」
//! を作り分ける実益が薄いため）。個々のツールのスコープ判定
//! （`has_any_read`/`can_read_value`/`has_write_scope`）は
//! `initialize`/`tools/list`/`ping` には無く、`tools/call` の各ツール実装が
//! 個別に行う。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{middleware, Json, Router};
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tokio::sync::Mutex as AsyncMutex;

use banto_core::ListParams;
use banto_tags::{CollectionGroupService, PlcConnectionService};

use crate::api_keys::{ApiKeyContext, ApiKeyLookup, ApiKeysService};
use crate::audit::{AuditEntry, AuditLogService};
use crate::commissioning::CommissioningState;
use crate::controller::{CollectionController, CollectionState};
use crate::hub::CollectorManager;
use crate::mqtt::MqttPublisher;
use crate::pending_changes::PendingChangesService;
use crate::rest::{
    bearer_token, commit_catalog_and_notify, compute_pending_base_fingerprint, compute_status,
    parse_requested_value, preflight_transaction, unauthorized_response, PlcConnectionPayload,
    TagSpaceState,
};
use crate::system_info::SystemInfoSampler;
use crate::test_output::TestOutputControl;
use crate::write_audit::WriteAuditService;
use crate::write_control::WriteControl;
use crate::write_path::{execute_write, execute_write_batch, WriteDeps};
use crate::write_rate::WriteRateLimiter;
use banto_server::ServerEvent;

/// クライアントが `initialize` で `protocolVersion` を送ってこなかった場合の
/// 既定値。仕様は固定していないので、実装時点の MCP 仕様の一版を素直に
/// 選んだだけ - クライアントが値を送ってくればそれをそのまま echo する
/// ([`handle_initialize`])。
const DEFAULT_PROTOCOL_VERSION: &str = "2025-06-18";

// --- JSON-RPC エラーコード（JSON-RPC 2.0 仕様の予約範囲） -------------------

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// `tools/call` のドメイン上の失敗（スコープ不足・タグ不明・ゲート拒否・
/// ロックダウン時の書き込み）ではなく、JSON-RPC そのものが壊れている場合の
/// エラー - このモジュールの doc comment「認証」節参照。
struct RpcError {
    code: i64,
    message: String,
}

impl RpcError {
    fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: INVALID_PARAMS,
            message: message.into(),
        }
    }

    fn method_not_found(message: impl Into<String>) -> Self {
        Self {
            code: METHOD_NOT_FOUND,
            message: message.into(),
        }
    }
}

// --- 状態 --------------------------------------------------------------

/// `/mcp` の認証ミドルウェア専用 state - `crate::rest::TagSpaceAuthState`と
/// 同じ役割だが、MCP は判定を単純化する（このモジュールの doc comment
/// 「認証」節参照）ので専用の型を持つ。
#[derive(Clone)]
struct McpAuthState {
    api_keys: ApiKeysService,
    manager: Arc<CollectorManager>,
}

/// `mcp_handler`・各ツール実装が必要とする状態一式。書き込み側
/// (`manager`〜`events`)は`crate::rest::WriteState`と同じ組み立て方 -
/// `write_tag_value`がここから[`WriteDeps`]を組み立てて
/// [`execute_write`]へそのまま渡す。`status`は`get_server_status`が
/// `crate::rest::compute_status`をそのまま再利用するための
/// [`TagSpaceState`]（`crate::stream`と同じ「他モジュールと状態型を共有する」
/// 借用パターン）。
#[derive(Clone)]
struct McpState {
    manager: Arc<CollectorManager>,
    /// `crate::rest::WriteState::collection_controller`と同じ意味
    /// （`enforce_collection_state`が`false`の互換モードでは`None`にして
    /// `CollectionNotRunning`ゲートを無効化する）。
    collection_controller: Option<Arc<CollectionController>>,
    api_keys: ApiKeysService,
    write_audit: WriteAuditService,
    write_control: Arc<WriteControl>,
    rate_limiter: Arc<AsyncMutex<WriteRateLimiter>>,
    events: broadcast::Sender<ServerEvent>,
    commissioning: CommissioningState,
    /// `get_server_status`専用 - `crate::rest::compute_status`をそのまま
    /// 呼ぶための借用一式（このモジュールの doc comment参照）。`status.controller`
    /// は構成ツール（[`tool_create_connection`]等）の収集状態判定にも使う -
    /// `collection_controller`と違い`enforce_collection_state`に関わらず
    /// 常に実体を持つ（このモジュールの doc comment「安全境界」節参照）。
    status: TagSpaceState,
    // --- T21 S1-b（docs/banto-hub-t21-design.md §4・§5）: 構成補助ツール
    // （接続 CRUD）用の追加状態。REST の `plc_connections_create`/
    // `plc_connections_delete`（`crate::rest`）と全く同じ mutation フロー
    // （tx → preflight → commit → catalog commit・pending queue）を再利用
    // するための最小限のサービス一式 - 二重実装しない（モジュール doc
    // comment「§3.7」節と同じ規律）。
    /// `PlcConnectionService::new(manager.pool())`で構築（REST の
    /// `tag_registry_router`と同じ生成方法）。
    plc_connections: PlcConnectionService,
    /// [`compute_pending_base_fingerprint`]が要求する2引数目 - 構成ツールは
    /// 今スライスでは接続のみ扱うが、この関数の署名（REST と共有）は
    /// group ソースの分岐も持つため必要。
    collection_groups: CollectionGroupService,
    /// `PendingChangesService::new(manager.pool())`で構築。
    pending_changes: PendingChangesService,
    /// 構成操作の監査（`origin:"mcp"`）用 - 呼び出し元
    /// （`crate::rest::api_router_with_controller_mode`）が REST と同じ
    /// `Arc`/`SqlitePool`ベースの `AuditLogService` をそのまま渡す。
    audit: AuditLogService,
    /// `commit_catalog_and_notify`へそのまま渡す - 呼び出し元が REST と
    /// 同じ値を渡す（`crate::rest::tag_registry_router`の同名フィールドと
    /// 同じ意味）。
    legacy_live_reconfigure: bool,
}

/// `POST /mcp`のルーターを組み立てる。呼び出し元
/// （`crate::rest::api_router_with_controller_mode`）は他の共有 `Arc`
/// （`manager`/`controller`/`write_control`/`write_audit`/`rate_limiter`/
/// `events`/`test_output`/`mqtt`/`system_info`）と**同じインスタンス**を
/// 渡すこと - `current_values`/レート制限/監査/`compute_status`が REST と
/// 一貫するために必須（`crate::rest::tag_space_router`の同種の引数と同じ
/// 共有規律）。
#[allow(clippy::too_many_arguments)]
pub(crate) fn mcp_router(
    manager: Arc<CollectorManager>,
    controller: Arc<CollectionController>,
    api_keys: ApiKeysService,
    write_audit: WriteAuditService,
    write_control: Arc<WriteControl>,
    rate_limiter: Arc<AsyncMutex<WriteRateLimiter>>,
    events: broadcast::Sender<ServerEvent>,
    commissioning: CommissioningState,
    test_output: Arc<TestOutputControl>,
    mqtt: Arc<MqttPublisher>,
    system_info: Arc<SystemInfoSampler>,
    // T14-4 由来: `crate::rest::tag_space_router`の`enforce_collection_state`
    // と同じ意味 - `!legacy_live_reconfigure`を渡す（呼び出し元の責務）。
    enforce_collection_state: bool,
    // T21 S1-b（docs/banto-hub-t21-design.md §5）: 構成補助ツール用に追加。
    // 呼び出し元（`crate::rest::api_router_with_controller_mode`）が REST の
    // 他ルーターと**同じ** `Arc`/値を渡すこと（このモジュールの doc comment
    // 「呼び出し元は...同じインスタンスを渡すこと」と同じ規律）。
    audit: AuditLogService,
    legacy_live_reconfigure: bool,
) -> Router {
    let status = TagSpaceState {
        manager: manager.clone(),
        controller: controller.clone(),
        write_control: write_control.clone(),
        test_output,
        mqtt,
        system_info,
    };
    // T21 S1-b: REST の `tag_registry_router`と同じ生成方法
    // （`PlcConnectionService::new(manager.pool())`等）- `SqlitePool`は
    // `Arc`-backed なので REST 側のサービスと同じ DB を指す別ハンドルに
    // なるだけで、状態は分裂しない。
    let plc_connections = PlcConnectionService::new(manager.pool());
    let collection_groups = CollectionGroupService::new(manager.pool());
    let pending_changes = PendingChangesService::new(manager.pool());
    let state = McpState {
        manager: manager.clone(),
        collection_controller: enforce_collection_state.then_some(controller),
        api_keys: api_keys.clone(),
        write_audit,
        write_control,
        rate_limiter,
        events,
        commissioning,
        status,
        plc_connections,
        collection_groups,
        pending_changes,
        audit,
        legacy_live_reconfigure,
    };
    let auth_state = McpAuthState { api_keys, manager };

    Router::new()
        .route("/mcp", post(mcp_handler))
        .with_state(state)
        .layer(middleware::from_fn_with_state(auth_state, require_mcp_auth))
}

/// `POST /mcp`専用の認証ミドルウェア - このモジュールの doc comment
/// 「認証」節参照。`crate::rest::require_tag_space_auth`と違い、失敗系統は
/// 一律 401 に潰す（revoked/tripped/expired/NotFound/セッション token の
/// 区別を audit へ残す必要はここでは無い - 個々のツールのスコープ拒否は
/// `tools/call`の結果として`isError`で返るので、HTTP 層は「認証できたか」
/// だけを見ればよい）。
async fn require_mcp_auth(
    State(state): State<McpAuthState>,
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let Some(token) = bearer_token(req.headers()).map(str::to_string) else {
        return unauthorized_response();
    };
    // オーナー決定4: セッション token（`bh_`で始まらない）は不可。
    if !token.starts_with("bh_") {
        return unauthorized_response();
    }
    let now_ms = state.manager.clock().now_ms();
    match state.api_keys.lookup(&token, now_ms).await {
        Ok(ApiKeyLookup::Valid(ctx)) => {
            if let Err(err) = state
                .api_keys
                .touch_last_used(ctx.id, now_ms, ctx.last_used_at_ms)
                .await
            {
                eprintln!("banto-hub: MCP API キーの last_used_at 更新に失敗しました: {err}");
            }
            req.extensions_mut().insert(ctx);
            next.run(req).await
        }
        // Revoked/Tripped/Expired/NotFound はいずれも一律 401
        // （このモジュールの doc comment「認証」節参照）。
        Ok(_) => unauthorized_response(),
        Err(err) => {
            eprintln!("banto-hub: MCP 用 API キー照合に失敗しました: {err}");
            unauthorized_response()
        }
    }
}

// --- JSON-RPC envelope ---------------------------------------------------

fn json_rpc_success(id: Value, result: Value) -> Response {
    (
        StatusCode::OK,
        Json(json!({ "jsonrpc": "2.0", "id": id, "result": result })),
    )
        .into_response()
}

fn json_rpc_error(id: Value, code: i64, message: &str) -> Response {
    (
        StatusCode::OK,
        Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message },
        })),
    )
        .into_response()
}

/// `POST /mcp`ハンドラ本体。JSON-RPC の壊れ方に応じて自前でエンベロープを
/// 判定する（`axum::Json`エクストラクタを使わない理由: 不正な JSON でも
/// `-32700`を JSON-RPC の形で返したいため、生バイト列を自分で
/// `serde_json::from_slice`する）。
async fn mcp_handler(
    State(state): State<McpState>,
    Extension(ctx): Extension<ApiKeyContext>,
    body: Bytes,
) -> Response {
    let raw: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return json_rpc_error(Value::Null, PARSE_ERROR, "Parse error"),
    };

    let id = raw.get("id").cloned();
    let jsonrpc_ok = raw.get("jsonrpc").and_then(Value::as_str) == Some("2.0");
    let method = raw
        .get("method")
        .and_then(Value::as_str)
        .map(str::to_string);

    let Some(method) = method.filter(|_| jsonrpc_ok) else {
        return json_rpc_error(
            id.unwrap_or(Value::Null),
            INVALID_REQUEST,
            "Invalid Request",
        );
    };
    let params = raw.get("params").cloned();

    // JSON-RPC 2.0: `id`メンバが無ければ通知 - 応答を一切返さない
    // (`notifications/initialized`/`notifications/*`、実装指示 A 参照)。
    let Some(id) = id else {
        return StatusCode::ACCEPTED.into_response();
    };

    match dispatch_method(&state, &ctx, &method, params).await {
        Ok(result) => json_rpc_success(id, result),
        Err(err) => json_rpc_error(id, err.code, &err.message),
    }
}

async fn dispatch_method(
    state: &McpState,
    ctx: &ApiKeyContext,
    method: &str,
    params: Option<Value>,
) -> Result<Value, RpcError> {
    match method {
        "initialize" => Ok(handle_initialize(params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_definitions() })),
        "tools/call" => handle_tools_call(state, ctx, params).await,
        other => Err(RpcError::method_not_found(format!(
            "unknown method: {other}"
        ))),
    }
}

fn handle_initialize(params: Option<Value>) -> Value {
    let protocol_version = params
        .as_ref()
        .and_then(|p| p.get("protocolVersion"))
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_PROTOCOL_VERSION);
    json!({
        "protocolVersion": protocol_version,
        "capabilities": { "tools": {} },
        "serverInfo": {
            "name": "banto-hub",
            "version": env!("CARGO_PKG_VERSION"),
        },
    })
}

fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "list_tags",
            "description": "タグ catalog を返す。呼び出しキーが読み取り可能なタグのみを含む(read/read:{name}/read:{connection}.{group}.* スコープ)。",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "read_tag_values",
            "description": "現在値を返す。tags を省略すると全タグ(呼び出しキーが読み取り可能なものに絞られる)。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tags": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "外部名 {connection}.{group}.{tag} のリスト。省略時は全タグ。",
                    },
                },
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "read_tag_now",
            "description": "指定した1タグを PLC からその場で直接読む(収集キャッシュ current_values/tstore を経由しない)。収集パイプラインから除外されている文字列タグを読める唯一の手段(T20 ①b)。数値タグは通常の読み取り値と同じスケール済みの工学値を返す。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tag": {
                        "type": "string",
                        "description": "外部名 {connection}.{group}.{tag}",
                    },
                },
                "required": ["tag"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "get_server_status",
            "description": "サーバー状態(収集状態・接続一覧・書き込み受付・ロックダウン有無等)を返す。",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "write_tag_value",
            "description": "タグへ書き込む。ロックダウン後(本番稼働中)は実際には書き込まず、何を書き込むべきかの助言のみを返す - 実行は管理 UI から人が行うこと。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tag": {
                        "type": "string",
                        "description": "外部名 {connection}.{group}.{tag}",
                    },
                    "value": {
                        "description": "書き込む工学値。数値タグには数値、bit タグには真偽値。",
                    },
                },
                "required": ["tag", "value"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "write_recipe",
            "description": "複数タグへ設定値一式(レシピ)を一括書き込みする。ロックダウン後(本番稼働中)は実際には書き込まず、何を書き込むべきかの助言のみを返す - 実行は管理 UI から人が行うこと。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "writes": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "tag": {
                                    "type": "string",
                                    "description": "外部名 {connection}.{group}.{tag}",
                                },
                                "value": {
                                    "description": "書き込む工学値。数値タグには数値、bit タグには真偽値。",
                                },
                            },
                            "required": ["tag", "value"],
                            "additionalProperties": false,
                        },
                        "description": "書き込むタグ/値の組のリスト。",
                    },
                },
                "required": ["writes"],
                "additionalProperties": false,
            },
        }),
        // --- T21 S1-b（docs/banto-hub-t21-design.md §4）: 構成補助ツール
        // （接続 CRUD の第一弾）。いずれも `admin` スコープ必須 - ロック
        // ダウンの前後を問わず常時可（設計 §3.2「有効化ガードは不採用」）。
        json!({
            "name": "list_connections",
            "description": "PLC 接続の一覧を返す(admin スコープ必須)。",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "create_connection",
            "description": "PLC 接続を新規作成する(admin スコープ必須)。収集中は直接反映せず、未適用キュー(pending queue)に保存する。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "接続名(一意)。" },
                    "protocol": {
                        "type": "string",
                        "enum": ["modbus-tcp", "slmp"],
                        "description": "既定値 modbus-tcp。",
                    },
                    "host": { "type": "string", "description": "接続先ホスト名/IP。" },
                    "port": { "type": "integer", "description": "接続先ポート番号。" },
                    "unitId": { "type": "integer", "description": "既定値 1。" },
                    "enabled": { "type": "boolean", "description": "既定値 true。" },
                    "simulation": {
                        "type": "boolean",
                        "description": "true でシミュレータ接続にする。既定値 false。",
                    },
                    "wordOrder": {
                        "type": "string",
                        "enum": ["low_high", "high_low"],
                        "description": "SLMP のワード順。既定値 low_high。",
                    },
                },
                "required": ["name", "host", "port"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "delete_connection",
            "description": "PLC 接続を削除する(admin スコープ必須)。配下のグループ・タグも一括削除するが、収集済み履歴データは残る。不可逆操作のため confirm:true が必須。収集中は直接反映せず、未適用キュー(pending queue)に保存する。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "削除する接続の id。" },
                    "confirm": {
                        "type": "boolean",
                        "description": "true を明示しないと拒否される(不可逆操作の確認)。",
                    },
                },
                "required": ["id", "confirm"],
                "additionalProperties": false,
            },
        }),
    ]
}

async fn handle_tools_call(
    state: &McpState,
    ctx: &ApiKeyContext,
    params: Option<Value>,
) -> Result<Value, RpcError> {
    let params = params.ok_or_else(|| RpcError::invalid_params("params is required"))?;
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("params.name (string) is required"))?;
    let arguments = params.get("arguments").cloned();

    let result = match name {
        "list_tags" => tool_list_tags(state, ctx),
        "read_tag_values" => tool_read_tag_values(state, ctx, arguments)?,
        "read_tag_now" => tool_read_tag_now(state, ctx, arguments).await?,
        "get_server_status" => tool_get_server_status(state, ctx).await,
        "write_tag_value" => tool_write_tag_value(state, ctx, arguments).await?,
        "write_recipe" => tool_write_recipe(state, ctx, arguments).await?,
        "list_connections" => tool_list_connections(state, ctx).await,
        "create_connection" => tool_create_connection(state, ctx, arguments).await?,
        "delete_connection" => tool_delete_connection(state, ctx, arguments).await?,
        other => {
            return Err(RpcError::invalid_params(format!("unknown tool: {other}")));
        }
    };
    Ok(result)
}

// --- tools/call 結果の組み立て ---------------------------------------------

/// `{ content: [{type:"text", text}], isError }` - ドメイン上の失敗
/// （スコープ不足・タグ不明・ゲート拒否・ロックダウン時の書き込み）は
/// すべてこの形（JSON-RPC エラーにしない、実装指示 A 参照）。
fn tool_result(text: String, is_error: bool) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    })
}

fn tool_ok(payload: Value) -> Value {
    tool_result(payload.to_string(), false)
}

fn tool_error(message: impl Into<String>) -> Value {
    tool_result(message.into(), true)
}

const MISSING_READ_SCOPE: &str =
    "read スコープを持つ API キーが必要です(read または read:{name}/read:{connection}.{group}.*)。";

// --- 1. list_tags ----------------------------------------------------------

fn tool_list_tags(state: &McpState, ctx: &ApiKeyContext) -> Value {
    if !ctx.has_any_read() {
        return tool_error(MISSING_READ_SCOPE);
    }
    let map = state.manager.tag_map();
    let tags: Vec<Value> = map
        .iter()
        .filter(|entry| ctx.can_read_value(&entry.external_name))
        .map(|entry| {
            json!({
                "name": entry.external_name,
                "connection": entry.connection,
                "group": entry.group,
                "dataType": entry.data_type,
                "unit": entry.unit,
                "writable": entry.writable,
                "enabled": entry.enabled,
                "tagKind": entry.tag_kind,
            })
        })
        .collect();
    tool_ok(json!({ "tags": tags }))
}

// --- 2. read_tag_values ------------------------------------------------

fn tool_read_tag_values(
    state: &McpState,
    ctx: &ApiKeyContext,
    arguments: Option<Value>,
) -> Result<Value, RpcError> {
    if !ctx.has_any_read() {
        return Ok(tool_error(MISSING_READ_SCOPE));
    }

    let requested_tags = match arguments.as_ref().and_then(|args| args.get("tags")) {
        None | Some(Value::Null) => None,
        Some(Value::Array(items)) => {
            let mut names = Vec::with_capacity(items.len());
            for item in items {
                match item.as_str() {
                    Some(name) => names.push(name.to_string()),
                    None => {
                        return Err(RpcError::invalid_params(
                            "arguments.tags must be an array of strings",
                        ));
                    }
                }
            }
            Some(names)
        }
        Some(_) => {
            return Err(RpcError::invalid_params(
                "arguments.tags must be an array of strings",
            ));
        }
    };

    let map = state.manager.tag_map();
    let names: Vec<String> = match requested_tags {
        Some(names) => names,
        None => map
            .iter()
            .map(|entry| entry.external_name.clone())
            .collect(),
    };
    // H10 ③と同じ規律(`crate::rest::v1_values`参照): per-tag read スコープ外
    // は黙って除く(聞いてもいないのに拒否しない)。
    let names: Vec<String> = names
        .into_iter()
        .filter(|name| ctx.can_read_value(name))
        .collect();

    let now_ms = state.manager.clock().now_ms();
    let current = state.manager.current_values();
    let server_store = state.manager.server_store();

    let values: Vec<Value> = names
        .iter()
        .filter_map(|name| map.get(name).map(|entry| (name, entry)))
        .map(|(name, entry)| {
            let (v, q, t) =
                crate::hub::read_current(entry, current.as_ref(), &server_store, now_ms);
            json!({
                "tag": name,
                "value": v,
                "quality": crate::hub::quality_str(q),
                "timestamp": t,
            })
        })
        .collect();

    Ok(tool_ok(json!({ "values": values })))
}

// --- 2b. read_tag_now (T20 ①b、docs/banto-hub-t20-design.md §3.1) ---------

/// `read_tag_now` - `crate::read_path::execute_read_now`をそのまま呼ぶだけ
/// (`crate::rest::v1_value_read_now`と全く同じ形で委譲する - ①a の
/// `write_tag_value`が`execute_write`へ委譲するのと同じ規律、二重実装
/// しない)。ロックダウンの前後を問わず同じ挙動(このモジュールの doc
/// comment「ロックダウン連動の安全ポリシー」の最後の一文参照 - 読み取り系
/// ツールはロックダウンに関係しない)。
async fn tool_read_tag_now(
    state: &McpState,
    ctx: &ApiKeyContext,
    arguments: Option<Value>,
) -> Result<Value, RpcError> {
    if !ctx.has_any_read() {
        return Ok(tool_error(MISSING_READ_SCOPE));
    }
    let arguments = arguments.ok_or_else(|| RpcError::invalid_params("arguments is required"))?;
    let tag = arguments
        .get("tag")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("arguments.tag (string) is required"))?
        .to_string();

    // H10 ③と同じ規律(`tool_read_tag_values`参照): per-tag read スコープ外
    // は読ませない。
    if !ctx.can_read_value(&tag) {
        return Ok(tool_error(format!(
            "missing_read_scope: タグ '{tag}' を読む read:{{name}}/read:{{connection}}.{{group}}.* スコープを持つ API キーが必要です。"
        )));
    }

    match crate::read_path::execute_read_now(state.manager.as_ref(), &tag).await {
        Ok(value) => Ok(tool_ok(json!({
            "tag": value.tag,
            "value": crate::read_path::plc_value_to_json(&value.value),
        }))),
        Err(rejection) => Ok(tool_error(rejection.to_json().to_string())),
    }
}

// --- 3. get_server_status ------------------------------------------------

async fn tool_get_server_status(state: &McpState, ctx: &ApiKeyContext) -> Value {
    if !ctx.has_any_read() {
        return tool_error(MISSING_READ_SCOPE);
    }
    match compute_status(&state.status).await {
        Ok(status) => {
            let mut value = serde_json::to_value(status).unwrap_or_else(|_| json!({}));
            if let Value::Object(map) = &mut value {
                // UX-41 実装指示: モデルが「今は本番なので助言に留める」と
                // 判断できるよう、コンパクトな状態にも必ず含める。
                map.insert(
                    "lockedDown".to_string(),
                    json!(state.commissioning.is_locked_down()),
                );
            }
            tool_ok(value)
        }
        Err(err) => tool_error(format!("サーバー状態の取得に失敗しました: {}", err.0)),
    }
}

// --- 4. write_tag_value ----------------------------------------------------

async fn tool_write_tag_value(
    state: &McpState,
    ctx: &ApiKeyContext,
    arguments: Option<Value>,
) -> Result<Value, RpcError> {
    let arguments = arguments.ok_or_else(|| RpcError::invalid_params("arguments is required"))?;
    let tag = arguments
        .get("tag")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("arguments.tag (string) is required"))?
        .to_string();
    let value = arguments
        .get("value")
        .cloned()
        .ok_or_else(|| RpcError::invalid_params("arguments.value is required"))?;

    // ロックダウン連動の安全ポリシー(このモジュールの doc comment参照、
    // オーナー決定 2026-09-04・最重要): このチェックはスコープ検査より
    // **先**に行う - `execute_write`を一切呼ばないという安全側の性質は
    // 呼び出し元のスコープに関わらず成立させる(read 専用キーであっても
    // 「本番なので書けない、代わりにこう伝えてください」という助言自体は
    // 見せてよい - 助言は呼び出し元が渡した`tag`/`value`をそのまま読み
    // 上げるだけで、新たな情報を漏らさない)。
    if state.commissioning.is_locked_down() {
        return Ok(tool_error(write_tag_value_advisory(&tag, &value)));
    }

    // ロックダウン前: REST の `v1_write_value` と同一の事前段
    // (`write:{tag}`の完全一致)。
    if !ctx.has_write_scope(&tag) {
        return Ok(tool_error(format!(
            "missing_write_scope: タグ '{tag}' への write:{{tag}} スコープを持つ API キーが必要です。"
        )));
    }

    let requested = parse_requested_value(&value);
    let deps = WriteDeps {
        manager: state.manager.as_ref(),
        collection_controller: state.collection_controller.as_deref(),
        api_keys: &state.api_keys,
        write_audit: &state.write_audit,
        write_control: state.write_control.as_ref(),
        rate_limiter: state.rate_limiter.as_ref(),
        events: &state.events,
    };

    // §3.7・実装指示 「絶対」: ゲート(catalog/writable/enabled/simulation/
    // protocol/write_enabled/rate limit/値変換)は一切再実装せず、REST/gRPC
    // と同じこの1関数へそのまま委譲する。
    match execute_write(&deps, ctx, &tag, requested).await {
        Ok(ok) => Ok(tool_ok(json!({ "tag": ok.tag, "result": "ok" }))),
        Err(rejection) => Ok(tool_error(rejection.to_json().to_string())),
    }
}

/// ロックダウン後の`write_tag_value`が返す助言文言(実装指示の文言の趣旨を
/// そのまま踏襲)。`value`は呼び出し元がそのまま渡した `serde_json::Value`
/// を素直に表示する(文字列なら引用符付きの JSON 表現になるが、モデルへの
/// 助言としては読み取れれば十分 - 数値/真偽値はそのまま読める)。
fn write_tag_value_advisory(tag: &str, value: &Value) -> String {
    format!(
        "本番（ロックダウン済み）では MCP から直接書き込みできません。推奨: タグ `{tag}` に `{value}` を書き込む。実行は管理 UI から人が行ってください。"
    )
}

// --- 5. write_recipe (T20 機能③b、レシピ一括書き込み) -----------------------

/// `write_recipe` の1エントリぶんの引数(`{tag, value}`)。`tag`/`value`
/// のどちらかが欠けている・型が違う要素は個別に`invalid_params`で拒否する -
/// バッチ全体を「JSON-RPC としては壊れている」ことにはしない([`tool_write_tag_value`]
/// の単票引数検証と同じ厳格さを配列の各要素へ適用するだけ)。
async fn tool_write_recipe(
    state: &McpState,
    ctx: &ApiKeyContext,
    arguments: Option<Value>,
) -> Result<Value, RpcError> {
    let arguments = arguments.ok_or_else(|| RpcError::invalid_params("arguments is required"))?;
    let writes = arguments
        .get("writes")
        .and_then(Value::as_array)
        .ok_or_else(|| RpcError::invalid_params("arguments.writes (array) is required"))?;
    if writes.is_empty() {
        return Err(RpcError::invalid_params(
            "arguments.writes must not be empty",
        ));
    }

    let mut parsed: Vec<(String, Value)> = Vec::with_capacity(writes.len());
    for entry in writes {
        let tag = entry
            .get("tag")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::invalid_params("arguments.writes[].tag (string) is required"))?
            .to_string();
        let value = entry
            .get("value")
            .cloned()
            .ok_or_else(|| RpcError::invalid_params("arguments.writes[].value is required"))?;
        parsed.push((tag, value));
    }

    // ロックダウン連動の安全ポリシー(このモジュールの doc comment・
    // [`tool_write_tag_value`]と同型・最重要): このチェックはスコープ検査
    // より**先**に行う - `execute_write_batch`を一切呼ばないという安全側の
    // 性質は呼び出し元のスコープに関わらず成立させる。助言は呼び出し元が
    // 渡した`tag`/`value`をそのまま読み上げるだけで、新たな情報を漏らさない。
    if state.commissioning.is_locked_down() {
        return Ok(tool_error(write_recipe_advisory(&parsed)));
    }

    // ロックダウン前: REST の `v1_write_values_batch` と同一の事前段
    // (`write:{tag}`の完全一致を全エントリぶん検査)。事前ゲート
    // all-or-nothing の思想を認証にも適用する - 1件でもスコープ不足なら
    // `execute_write_batch`へは一切進まず、全体を拒否する。
    let missing_scope: Vec<&str> = parsed
        .iter()
        .filter(|(tag, _)| !ctx.has_write_scope(tag))
        .map(|(tag, _)| tag.as_str())
        .collect();
    if !missing_scope.is_empty() {
        return Ok(tool_error(format!(
            "missing_write_scope: 次のタグへの write:{{tag}} スコープを持つ API キーが必要です: {}",
            missing_scope.join(", ")
        )));
    }

    let entries: Vec<(String, Option<crate::write_path::RequestedValue>)> = parsed
        .iter()
        .map(|(tag, value)| (tag.clone(), parse_requested_value(value)))
        .collect();
    let deps = WriteDeps {
        manager: state.manager.as_ref(),
        collection_controller: state.collection_controller.as_deref(),
        api_keys: &state.api_keys,
        write_audit: &state.write_audit,
        write_control: state.write_control.as_ref(),
        rate_limiter: state.rate_limiter.as_ref(),
        events: &state.events,
    };

    // §3.7・実装指示「絶対」: ゲート本体(重複タグ検出・事前ゲート
    // all-or-nothing・write_control・レート限・commit)は一切再実装せず、
    // REST の `/api/v1/values/batch` と同じこの1関数へそのまま委譲する。
    let outcomes = execute_write_batch(&deps, ctx, entries).await;
    // モデルが「バッチ全体として1件も書けなかったのか、一部成功か」を
    // 一目で判断できるよう、per-entry の配列に加えて成功件数も返す
    // (REST の `BatchWriteValuesResponse` にはこのフィールドは無いが、MCP
    // はモデルへの応答なので明示した方が誤読を避けられる)。
    let applied = outcomes.iter().filter(|o| o.result.is_ok()).count();
    let writes: Vec<Value> = outcomes
        .into_iter()
        .map(|outcome| match outcome.result {
            Ok(()) => json!({ "tag": outcome.tag, "ok": true }),
            Err(rejection) => {
                let mut entry = json!({
                    "tag": outcome.tag,
                    "ok": false,
                    "error": rejection.rest_error_code(),
                });
                if let Some(detail) = rejection.detail() {
                    entry["detail"] = json!(detail);
                }
                entry
            }
        })
        .collect();

    Ok(tool_ok(json!({ "writes": writes, "applied": applied })))
}

/// ロックダウン後の`write_recipe`が返す助言文言 - [`write_tag_value_advisory`]
/// のバッチ版。呼び出し元が渡した`tag`/`value`をそのまま読み上げるだけで、
/// 新たな情報は漏らさない(このモジュールの doc comment参照)。
fn write_recipe_advisory(entries: &[(String, Value)]) -> String {
    let items: Vec<String> = entries
        .iter()
        .map(|(tag, value)| format!("タグ `{tag}` に `{value}` を書き込む"))
        .collect();
    format!(
        "本番（ロックダウン済み）では MCP から直接書き込みできません。推奨レシピ: {}。実行は管理 UI から人が行ってください。",
        items.join("、")
    )
}

// ---------------------------------------------------------------------------
// T21 S1-b（docs/banto-hub-t21-design.md §3・§4）: 構成補助ツール（接続
// CRUD の第一弾）。すべて `admin` スコープを要求する - 有効化ガードは無く
// （設計 §3.2「不採用」オーナー決定）、`admin` スコープ＋監査＋（delete 系は）
// confirm が唯一のガード。mutation 本体（tx・preflight・catalog commit・
// pending queue）は `crate::rest` の `plc_connections_create`/
// `plc_connections_delete`（admin REST）が使うヘルパー
// （[`preflight_transaction`]/[`commit_catalog_and_notify`]/
// [`compute_pending_base_fingerprint`]）をそのまま呼ぶ - 二重実装しない
// （このモジュール doc comment「§3.7」節と同じ規律）。監査だけは MCP 固有
// （`origin:"mcp"`・actor は API キー名）- REST の `record_write`
// （`crate::rest`）と同じ形の `AuditEntry` を [`audit_config_action`] が
// 組み立てる。
// ---------------------------------------------------------------------------

const MISSING_ADMIN_SCOPE: &str = "missing_admin_scope: admin スコープを持つ API キーが必要です。";

/// 構成ツール共通の admin ゲート - [`MISSING_READ_SCOPE`]/`missing_write_scope`
/// と同じ流儀（JSON-RPC エラーにせず `isError` の `tools/call` 結果で返す、
/// [`tool_error`]参照）。ゲート拒否は監査しない（成功した mutation だけを
/// audit_log に残す - REST の `record_write`が成功後にしか呼ばれないのと
/// 同じ規律、[`audit_config_action`]のdoc comment参照）。
fn require_admin_scope(ctx: &ApiKeyContext) -> Result<(), Value> {
    if ctx.has_admin_scope() {
        Ok(())
    } else {
        Err(tool_error(MISSING_ADMIN_SCOPE))
    }
}

/// 収集中に pending queue へ保存できたときの応答文言 - REST の
/// `QueuedPendingChangeResponse.message`（`crate::rest`）と同じ文言にして
/// REST/MCP で表現を揃える。
const QUEUED_MESSAGE: &str = "収集中のため変更を未適用キューに保存しました。";

/// 構成操作の監査（設計 §3.3「全構成操作を audit_log に記録する」）- REST の
/// `record_write`（`crate::rest`）と同じ `AuditEntry` の組み立て方だが、
/// actor は bearer セッションの identity ではなく **API キー名**
/// （`ctx.name`）、`actor_role` は固定文字列 `"api_key"`、`origin` は
/// `"mcp"`（REST は `"rest"`）。呼び出し元は実際に mutation が確定した後
/// （直接コミット・pending queue 投入のいずれかの成功後）にのみ呼ぶこと -
/// pending queue 投入も監査する点は意図的に REST と異なる（REST の
/// `queue_pending_registry_change`は audit_log を書かない - pending_changes
/// テーブル自身が記録になるため - が、設計 §3.3 は MCP に「全構成操作」の
/// audit_log 記録を求めているため、queued 成功もここで監査する）。
async fn audit_config_action(
    state: &McpState,
    ctx: &ApiKeyContext,
    action: &str,
    entity_id: Option<&str>,
    detail: Option<Value>,
) {
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(ctx.name.as_str()),
            actor_role: Some("api_key"),
            action,
            resource: "plc_connections",
            entity_id,
            detail,
            origin: "mcp",
            result: "ok",
        })
        .await;
}

// --- 9. list_connections -----------------------------------------------------

async fn tool_list_connections(state: &McpState, ctx: &ApiKeyContext) -> Value {
    if let Err(err) = require_admin_scope(ctx) {
        return err;
    }
    match state.plc_connections.list(ListParams::default()).await {
        Ok(result) => tool_ok(json!({ "connections": result.rows })),
        Err(err) => tool_error(format!("接続一覧の取得に失敗しました: {err}")),
    }
}

// --- 10. create_connection ----------------------------------------------------

/// `crate::rest::plc_connections_create`（admin REST）と全く同じ mutation
/// フロー - 収集中は [`compute_pending_base_fingerprint`] →
/// `state.pending_changes.create_pending` で pending queue に保存し、停止中は
/// `state.plc_connections.create_tx` → [`preflight_transaction`] →
/// `tx.commit` → [`commit_catalog_and_notify`] を1トランザクションで実行する
/// （順序も REST と同一）。REST との違いは監査の宛先だけ
/// （[`audit_config_action`]のdoc comment参照）。
async fn tool_create_connection(
    state: &McpState,
    ctx: &ApiKeyContext,
    arguments: Option<Value>,
) -> Result<Value, RpcError> {
    if let Err(err) = require_admin_scope(ctx) {
        return Ok(err);
    }
    let arguments = arguments.ok_or_else(|| RpcError::invalid_params("arguments is required"))?;
    let input: PlcConnectionPayload = serde_json::from_value(arguments)
        .map_err(|err| RpcError::invalid_params(format!("接続の入力が不正です: {err}")))?;

    let status = state.status.controller.status();
    if status.state != CollectionState::Stopped {
        let payload = json!({ "input": input });
        let base_fingerprint = compute_pending_base_fingerprint(
            &state.plc_connections,
            &state.collection_groups,
            "plc_connections.create",
            &payload,
        )
        .await;
        let pending = match state
            .pending_changes
            .create_pending(
                "plc_connections.create",
                &payload,
                state.manager.configured_revision() as i64,
                base_fingerprint.as_deref(),
                Some(ctx.name.as_str()),
                Some("api_key"),
            )
            .await
        {
            Ok(pending) => pending,
            Err(err) => {
                return Ok(tool_error(format!(
                    "未適用キューへの保存に失敗しました: {err}"
                )));
            }
        };
        audit_config_action(
            state,
            ctx,
            "create",
            Some(&pending.id.to_string()),
            Some(json!({ "queued": true, "pendingId": pending.id, "name": input.name })),
        )
        .await;
        return Ok(tool_ok(json!({
            "queued": true,
            "pendingId": pending.id,
            "message": QUEUED_MESSAGE,
        })));
    }

    let mut tx = match state.manager.pool().begin().await {
        Ok(tx) => tx,
        Err(err) => {
            return Ok(tool_error(format!(
                "トランザクションの開始に失敗しました: {err}"
            )));
        }
    };
    let created = match state.plc_connections.create_tx(&mut tx, input.into()).await {
        Ok(created) => created,
        Err(err) => {
            let _ = tx.rollback().await;
            return Ok(tool_error(format!("{err}")));
        }
    };
    let snapshot = match preflight_transaction(&mut tx).await {
        Ok(snapshot) => snapshot,
        Err(err) => {
            let _ = tx.rollback().await;
            return Ok(tool_error(format!("{}", err.0)));
        }
    };
    if let Err(err) = tx.commit().await {
        return Ok(tool_error(format!("コミットに失敗しました: {err}")));
    }
    audit_config_action(
        state,
        ctx,
        "create",
        Some(&created.id.to_string()),
        Some(json!({ "name": created.name, "enabled": created.enabled })),
    )
    .await;
    commit_catalog_and_notify(
        &state.manager,
        &state.status.controller,
        &state.events,
        "plc_connections",
        snapshot,
        state.legacy_live_reconfigure,
    )
    .await;
    Ok(tool_ok(json!({ "created": created })))
}

// --- 11. delete_connection ----------------------------------------------------

/// `crate::rest::plc_connections_delete`（admin REST）と全く同じ mutation
/// フロー（`cascade_delete_tx` - T19 S2-b・UX-38: タグ・グループが
/// あっても削除でき、履歴は残す）。不可逆操作のため
/// `arguments.confirm == true` を要求する（設計 §8「delete 系は confirm
/// 必須」）- 拒否した場合は mutation に一切触れない（監査もしない、
/// [`require_admin_scope`]と同じ「ゲート拒否は監査しない」規律）。
async fn tool_delete_connection(
    state: &McpState,
    ctx: &ApiKeyContext,
    arguments: Option<Value>,
) -> Result<Value, RpcError> {
    if let Err(err) = require_admin_scope(ctx) {
        return Ok(err);
    }
    let arguments = arguments.ok_or_else(|| RpcError::invalid_params("arguments is required"))?;
    let id = arguments
        .get("id")
        .and_then(Value::as_i64)
        .ok_or_else(|| RpcError::invalid_params("arguments.id (integer) is required"))?;
    let confirmed = arguments.get("confirm").and_then(Value::as_bool) == Some(true);
    if !confirmed {
        return Ok(tool_error(
            "confirm_required: 削除は不可逆操作です。arguments.confirm:true を指定してください。",
        ));
    }

    let status = state.status.controller.status();
    if status.state != CollectionState::Stopped {
        let payload = json!({ "id": id });
        let base_fingerprint = compute_pending_base_fingerprint(
            &state.plc_connections,
            &state.collection_groups,
            "plc_connections.delete",
            &payload,
        )
        .await;
        let pending = match state
            .pending_changes
            .create_pending(
                "plc_connections.delete",
                &payload,
                state.manager.configured_revision() as i64,
                base_fingerprint.as_deref(),
                Some(ctx.name.as_str()),
                Some("api_key"),
            )
            .await
        {
            Ok(pending) => pending,
            Err(err) => {
                return Ok(tool_error(format!(
                    "未適用キューへの保存に失敗しました: {err}"
                )));
            }
        };
        audit_config_action(
            state,
            ctx,
            "delete",
            Some(&id.to_string()),
            Some(json!({ "queued": true, "pendingId": pending.id })),
        )
        .await;
        return Ok(tool_ok(json!({
            "queued": true,
            "pendingId": pending.id,
            "message": QUEUED_MESSAGE,
        })));
    }

    let mut tx = match state.manager.pool().begin().await {
        Ok(tx) => tx,
        Err(err) => {
            return Ok(tool_error(format!(
                "トランザクションの開始に失敗しました: {err}"
            )));
        }
    };
    let cascade = match state.plc_connections.cascade_delete_tx(&mut tx, id).await {
        Ok(cascade) => cascade,
        Err(err) => {
            let _ = tx.rollback().await;
            return Ok(tool_error(format!("{err}")));
        }
    };
    let snapshot = match preflight_transaction(&mut tx).await {
        Ok(snapshot) => snapshot,
        Err(err) => {
            let _ = tx.rollback().await;
            return Ok(tool_error(format!("{}", err.0)));
        }
    };
    if let Err(err) = tx.commit().await {
        return Ok(tool_error(format!("コミットに失敗しました: {err}")));
    }
    let cascade_detail = json!({
        "deletedGroups": cascade.deleted_groups,
        "deletedTags": cascade.deleted_tags,
    });
    audit_config_action(
        state,
        ctx,
        "delete",
        Some(&id.to_string()),
        Some(json!({ "cascade": cascade_detail.clone() })),
    )
    .await;
    commit_catalog_and_notify(
        &state.manager,
        &state.status.controller,
        &state.events,
        "plc_connections",
        snapshot,
        state.legacy_live_reconfigure,
    )
    .await;
    Ok(tool_ok(
        json!({ "deleted": true, "id": id, "cascade": cascade_detail }),
    ))
}
