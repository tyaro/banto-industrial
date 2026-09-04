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
//! ## ロックダウン連動の安全ポリシー（オーナー決定 2026-09-04、最重要）
//!
//! `crate::commissioning::CommissioningState::is_locked_down()` が唯一の
//! 分岐点:
//!
//! - **ロックダウン前**（試運転モード、`false`）: MCP はフル機能。
//!   `write_tag_value` は `ctx.has_write_scope(tag)` を検査した後、通常どおり
//!   `execute_write` を実行する（REST と同じゲート・監査・レート制限）。
//! - **ロックダウン後**（本番、`true`）: `write_tag_value` は
//!   **`execute_write` を一切呼ばない**。安全方向にのみ倒し、「何を書き込む
//!   べきか」を助言する `isError` の `tools/call` 結果だけを返す - 実際の
//!   操作は人が管理 UI から行う前提（このモジュールの
//!   [`write_tag_value_advisory`] 参照）。このチェックは write スコープの
//!   有無より**先**に行う - 助言はロックダウン前から本人が渡した
//!   `tag`/`value` をそのまま読み上げるだけなので、スコープ不足の API キー
//!   に見せても新規の情報漏洩にはならない（このモジュールの doc comment
//!   よりも詳しい理由は [`write_tag_value`] 実装コメント参照）。読み取り系
//!   3ツールはロックダウンの前後を問わず同じ挙動 - `has_any_read`/
//!   `can_read_value` によるスコープ判定のみで決まる。
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

use crate::api_keys::{ApiKeyContext, ApiKeyLookup, ApiKeysService};
use crate::commissioning::CommissioningState;
use crate::controller::CollectionController;
use crate::hub::CollectorManager;
use crate::mqtt::MqttPublisher;
use crate::rest::{
    bearer_token, compute_status, parse_requested_value, unauthorized_response, TagSpaceState,
};
use crate::system_info::SystemInfoSampler;
use crate::test_output::TestOutputControl;
use crate::write_audit::WriteAuditService;
use crate::write_control::WriteControl;
use crate::write_path::{execute_write, WriteDeps};
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
    /// 呼ぶための借用一式（このモジュールの doc comment参照）。
    status: TagSpaceState,
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
) -> Router {
    let status = TagSpaceState {
        manager: manager.clone(),
        controller: controller.clone(),
        write_control: write_control.clone(),
        test_output,
        mqtt,
        system_info,
    };
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
        "get_server_status" => tool_get_server_status(state, ctx).await,
        "write_tag_value" => tool_write_tag_value(state, ctx, arguments).await?,
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
