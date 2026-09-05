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
use banto_tags::{CollectionGroupService, PlcConnectionService, TagService, TagUpdateError};

use crate::api_keys::{ApiKeyContext, ApiKeyLookup, ApiKeysService};
use crate::audit::{AuditEntry, AuditLogService};
use crate::commissioning::CommissioningState;
use crate::controller::{CollectionController, CollectionState, RunMode};
use crate::hub::CollectorManager;
use crate::mqtt::MqttPublisher;
use crate::pending_changes::PendingChangesService;
use crate::rest::{
    bearer_token, commit_catalog_and_notify, compute_pending_base_fingerprint, compute_status,
    parse_requested_value, preflight_transaction, run_plc_connection_test, unauthorized_response,
    CollectionGroupPayload, PlcConnectionPayload, PlcConnectionTestPayload, TagPayload,
    TagSpaceState,
};
use crate::system_info::SystemInfoSampler;
use crate::test_output::TestOutputControl;
use crate::write_audit::WriteAuditService;
use crate::write_control::{persist_enabled, WriteControl};
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
    /// T21 S1-d（docs/banto-hub-t21-design.md §4・§5）: タグ CRUD 用。
    /// `TagService::new(manager.pool())`で構築（REST の
    /// `tag_registry_router`と同じ生成方法）。
    tags: TagService,
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
    // T21 S1-d: REST の `tag_registry_router`と同じ生成方法。
    let tags = TagService::new(manager.pool());
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
        tags,
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
        // --- T21 S1-c（docs/banto-hub-t21-design.md §4・§5）: 構成補助ツール
        // 第二弾(接続 update/test・グループ CRUD)。S1-b と同じゲート
        // (admin スコープ必須・有効化ガードなし)。
        json!({
            "name": "update_connection",
            "description": "既存の PLC 接続を更新する(admin スコープ必須)。更新は全項目指定が必須(PUT 置換。省略項目は既定値で上書きされるため許可しない)。収集中は直接反映せず、未適用キュー(pending queue)に保存する。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "更新する接続の id。" },
                    "name": { "type": "string", "description": "接続名(一意)。" },
                    "protocol": {
                        "type": "string",
                        "enum": ["modbus-tcp", "slmp"],
                        "description": "modbus-tcp または slmp。",
                    },
                    "host": { "type": "string", "description": "接続先ホスト名/IP。" },
                    "port": { "type": "integer", "description": "接続先ポート番号。" },
                    "unitId": { "type": "integer", "description": "Modbus のユニット ID。" },
                    "enabled": { "type": "boolean", "description": "接続を有効にするか。" },
                    "simulation": {
                        "type": "boolean",
                        "description": "true でシミュレータ接続にする。",
                    },
                    "wordOrder": {
                        "type": "string",
                        "enum": ["low_high", "high_low"],
                        "description": "SLMP のワード順。",
                    },
                },
                "required": [
                    "id",
                    "name",
                    "protocol",
                    "host",
                    "port",
                    "unitId",
                    "enabled",
                    "simulation",
                    "wordOrder",
                ],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "test_connection",
            "description": "保存前に PLC 接続の疎通を確認する(admin スコープ必須)。TCP 接続だけでなく実プロトコルで軽い読み出しを1回試みる。レジストリへの保存は行わない副作用の無い操作。virtual/シミュレーション接続はテスト対象外(即座に ok:false)。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "protocol": {
                        "type": "string",
                        "enum": ["modbus-tcp", "slmp", "virtual"],
                        "description": "virtual は常に ok:false(テスト対象外)。",
                    },
                    "host": { "type": "string", "description": "接続先ホスト名/IP。" },
                    "port": { "type": "integer", "description": "接続先ポート番号。" },
                    "unitId": { "type": "integer", "description": "既定値 1(Modbus のみ使用)。" },
                    "simulation": {
                        "type": "boolean",
                        "description": "true の場合は常に ok:false(内蔵シミュレータは常に成功するためテスト不要)。既定値 false。",
                    },
                    "connectionId": {
                        "type": "integer",
                        "description": "保存済み接続を編集中にテストする場合のみ指定。SLMP で既存の broker セッションがあればそれを再利用し、2本目をダイヤルしない。",
                    },
                },
                "required": ["protocol", "host", "port"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "list_groups",
            "description": "収集グループの一覧を返す(admin スコープ必須)。",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "create_group",
            "description": "収集グループを新規作成する(admin スコープ必須)。収集中は直接反映せず、未適用キュー(pending queue)に保存する。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "グループ名(接続内で一意)。" },
                    "plcConnectionId": { "type": "integer", "description": "所属する接続の id。" },
                    "periodMs": { "type": "integer", "description": "収集周期(ミリ秒)。" },
                    "enabled": { "type": "boolean", "description": "既定値 true。" },
                    "defaultWritable": {
                        "type": "boolean",
                        "description": "このグループへ新規登録するタグの writable 既定値。既定値 true。",
                    },
                },
                "required": ["name", "plcConnectionId", "periodMs"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "update_group",
            "description": "既存の収集グループを更新する(admin スコープ必須)。更新は全項目指定が必須(PUT 置換。省略項目は既定値で上書きされるため許可しない)。収集中は直接反映せず、未適用キュー(pending queue)に保存する。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "更新するグループの id。" },
                    "name": { "type": "string", "description": "グループ名(接続内で一意)。" },
                    "plcConnectionId": { "type": "integer", "description": "所属する接続の id。" },
                    "periodMs": { "type": "integer", "description": "収集周期(ミリ秒)。" },
                    "enabled": { "type": "boolean", "description": "グループを有効にするか。" },
                    "defaultWritable": {
                        "type": "boolean",
                        "description": "このグループへ新規登録するタグの writable 既定値。",
                    },
                },
                "required": [
                    "id",
                    "name",
                    "plcConnectionId",
                    "periodMs",
                    "enabled",
                    "defaultWritable",
                ],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "delete_group",
            "description": "収集グループを削除する(admin スコープ必須)。配下のタグも一括削除するが、収集済み履歴データは残る。不可逆操作のため confirm:true が必須。収集中は直接反映せず、未適用キュー(pending queue)に保存する。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "削除するグループの id。" },
                    "confirm": {
                        "type": "boolean",
                        "description": "true を明示しないと拒否される(不可逆操作の確認)。",
                    },
                },
                "required": ["id", "confirm"],
                "additionalProperties": false,
            },
        }),
        // --- T21 S1-d（docs/banto-hub-t21-design.md §4・§5）: 構成補助ツール
        // 第三弾（タグ CRUD）。S1-b/S1-c と同じゲート(admin スコープ必須・
        // 有効化ガードなし)。`create_tag`/`update_tag`/`delete_tag`の
        // properties は REST の `TagPayload`（`crate::rest`）と同じ wire
        // フィールド一式。
        json!({
            "name": "get_tag",
            "description": "指定した1タグの全フィールド(revision 含む)を返す(admin スコープ必須)。update_tag は全項目指定の PUT 置換のため、事前にこれで現在値を読んでから必要な項目だけ変更して送り返す read-modify-write に使う。副作用なし・監査対象外。list_tags(read スコープの一覧・サブセット)とは別物。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "取得するタグの id。" },
                },
                "required": ["id"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "create_tag",
            "description": "タグを新規作成する(admin スコープ必須)。収集中は直接反映せず、未適用キュー(pending queue)に保存する。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "タグ名(グループ内で一意)。" },
                    "collectionGroupId": { "type": "integer", "description": "所属する収集グループの id。" },
                    "address": { "type": "string", "description": "PLC アドレス。" },
                    "dataType": { "type": "string", "description": "データ型。" },
                    "stringLength": {
                        "type": ["integer", "null"],
                        "description": "dataType が string のとき必須(1-128、16bit ワード数)。未使用時は null。",
                    },
                    "stringEncoding": {
                        "type": "string",
                        "description": "string タグの文字コード。既定値 utf8。",
                    },
                    "rawLo": { "type": ["number", "null"], "description": "スケーリング入力下限。未使用時は null。" },
                    "rawHi": { "type": ["number", "null"], "description": "スケーリング入力上限。未使用時は null。" },
                    "engLo": { "type": ["number", "null"], "description": "スケーリング後工学値下限。未使用時は null。" },
                    "engHi": { "type": ["number", "null"], "description": "スケーリング後工学値上限。未使用時は null。" },
                    "unit": { "type": ["string", "null"], "description": "工学単位。未使用時は null。" },
                    "decimals": { "type": "integer", "description": "表示小数桁数。既定値 0。" },
                    "thresholdH": { "type": ["number", "null"], "description": "しきい値 H。未使用時は null。" },
                    "thresholdHh": { "type": ["number", "null"], "description": "しきい値 HH。未使用時は null。" },
                    "thresholdL": { "type": ["number", "null"], "description": "しきい値 L。未使用時は null。" },
                    "thresholdLl": { "type": ["number", "null"], "description": "しきい値 LL。未使用時は null。" },
                    "enabled": { "type": "boolean", "description": "既定値 false。" },
                    "writable": { "type": "boolean", "description": "書き込み許可。既定値 false。" },
                    "tagKind": {
                        "type": "string",
                        "description": "plc/computed/internal のいずれか。既定値 plc。",
                    },
                    "expression": { "type": ["string", "null"], "description": "computed タグの計算式。未使用時は null。" },
                    "retain": { "type": "boolean", "description": "internal タグの再起動時復元。既定値 false。" },
                },
                "required": ["name", "collectionGroupId", "address", "dataType"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "update_tag",
            "description": "既存のタグを更新する(admin スコープ必須)。更新は全項目指定が必須(PUT 置換。省略項目は既定値で上書きされるため許可しない) - get_tag で現在値を取得してから全項目を送り返すこと。expectedRevision を付けると楽観ロックになり、他者が先に更新していた場合は revision_conflict エラーで拒否される(get_tag で最新の revision を取り直して再試行)。収集中は直接反映せず、未適用キュー(pending queue)に保存する。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "更新するタグの id。" },
                    "name": { "type": "string", "description": "タグ名(グループ内で一意)。" },
                    "collectionGroupId": { "type": "integer", "description": "所属する収集グループの id。" },
                    "address": { "type": "string", "description": "PLC アドレス。" },
                    "dataType": { "type": "string", "description": "データ型。" },
                    "stringLength": {
                        "type": ["integer", "null"],
                        "description": "dataType が string のとき必須(1-128、16bit ワード数)。未使用時は null。",
                    },
                    "stringEncoding": { "type": "string", "description": "string タグの文字コード。" },
                    "rawLo": { "type": ["number", "null"], "description": "スケーリング入力下限。未使用時は null。" },
                    "rawHi": { "type": ["number", "null"], "description": "スケーリング入力上限。未使用時は null。" },
                    "engLo": { "type": ["number", "null"], "description": "スケーリング後工学値下限。未使用時は null。" },
                    "engHi": { "type": ["number", "null"], "description": "スケーリング後工学値上限。未使用時は null。" },
                    "unit": { "type": ["string", "null"], "description": "工学単位。未使用時は null。" },
                    "decimals": { "type": "integer", "description": "表示小数桁数。" },
                    "thresholdH": { "type": ["number", "null"], "description": "しきい値 H。未使用時は null。" },
                    "thresholdHh": { "type": ["number", "null"], "description": "しきい値 HH。未使用時は null。" },
                    "thresholdL": { "type": ["number", "null"], "description": "しきい値 L。未使用時は null。" },
                    "thresholdLl": { "type": ["number", "null"], "description": "しきい値 LL。未使用時は null。" },
                    "enabled": { "type": "boolean", "description": "タグを有効にするか。" },
                    "writable": { "type": "boolean", "description": "書き込み許可。" },
                    "tagKind": { "type": "string", "description": "plc/computed/internal のいずれか。" },
                    "expression": { "type": ["string", "null"], "description": "computed タグの計算式。未使用時は null。" },
                    "retain": { "type": "boolean", "description": "internal タグの再起動時復元。" },
                    "expectedRevision": {
                        "type": ["integer", "null"],
                        "description": "楽観ロック用(get_tag で取得した現在の revision)。省略するか null にするとロック無しで上書きする。",
                    },
                },
                "required": [
                    "id",
                    "name",
                    "collectionGroupId",
                    "address",
                    "dataType",
                    "stringLength",
                    "stringEncoding",
                    "rawLo",
                    "rawHi",
                    "engLo",
                    "engHi",
                    "unit",
                    "decimals",
                    "thresholdH",
                    "thresholdHh",
                    "thresholdL",
                    "thresholdLl",
                    "enabled",
                    "writable",
                    "tagKind",
                    "expression",
                    "retain",
                ],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "delete_tag",
            "description": "タグを削除する(admin スコープ必須)。タグは末端リソースのため配下は無い(cascade ではない)が、収集済み履歴データは残る。不可逆操作のため confirm:true が必須。収集中は直接反映せず、未適用キュー(pending queue)に保存する。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "削除するタグの id。" },
                    "confirm": {
                        "type": "boolean",
                        "description": "true を明示しないと拒否される(不可逆操作の確認)。",
                    },
                },
                "required": ["id", "confirm"],
                "additionalProperties": false,
            },
        }),
        // --- T21 S2-a（docs/tag-server-design.md §8想定の可逆ランタイム
        // 制御。ここまでの構成 CRUD 系ツールと違い、レジストリ mutation
        // ではなく collection controller / write_control を直接叩く
        // ランタイム制御 - pending queue も preflight も通らない
        // （[`tool_set_collection`]/[`tool_set_write_control`]のdoc comment
        // 参照）。可逆操作のため confirm は不要（設計の confirm 必須は
        // delete 系の不可逆操作限定）。
        json!({
            "name": "set_collection",
            "description": "収集を開始/停止する(admin スコープ必須)。start は実機収集(configured モード)を開始する。既知の運用癖: 収集開始は write_enabled を False にリセットするため、書き込みを行うなら開始後に set_write_control で改めて有効化すること。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["start", "stop"],
                        "description": "start で収集開始(configured モード)、stop で収集停止。",
                    },
                },
                "required": ["action"],
                "additionalProperties": false,
            },
        }),
        json!({
            "name": "set_write_control",
            "description": "書き込み受付の有効/無効を切り替える(admin スコープ必須)。収集開始は write_enabled を False にリセットするため、収集開始直後に書き込みを行うにはこのツールで改めて enabled:true にする必要がある。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "enabled": {
                        "type": "boolean",
                        "description": "true で書き込み受付を有効化、false で無効化。",
                    },
                },
                "required": ["enabled"],
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
        "update_connection" => tool_update_connection(state, ctx, arguments).await?,
        "test_connection" => tool_test_connection(state, ctx, arguments).await?,
        "list_groups" => tool_list_groups(state, ctx).await,
        "create_group" => tool_create_group(state, ctx, arguments).await?,
        "update_group" => tool_update_group(state, ctx, arguments).await?,
        "delete_group" => tool_delete_group(state, ctx, arguments).await?,
        "get_tag" => tool_get_tag(state, ctx, arguments).await?,
        "create_tag" => tool_create_tag(state, ctx, arguments).await?,
        "update_tag" => tool_update_tag(state, ctx, arguments).await?,
        "delete_tag" => tool_delete_tag(state, ctx, arguments).await?,
        "set_collection" => tool_set_collection(state, ctx, arguments).await?,
        "set_write_control" => tool_set_write_control(state, ctx, arguments).await?,
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
///
/// `resource`は REST の各ハンドラの`record_write`呼び出しに渡す
/// resource 文字列と同じ値を呼び出し元が渡す（接続系ツールは
/// `"plc_connections"`、グループ系ツールは`"collection_groups"` - T21
/// S1-c で S1-b 当時の接続専用固定値から一般化した）。
async fn audit_config_action(
    state: &McpState,
    ctx: &ApiKeyContext,
    action: &str,
    resource: &'static str,
    entity_id: Option<&str>,
    detail: Option<Value>,
) {
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(ctx.name.as_str()),
            actor_role: Some("api_key"),
            action,
            resource,
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
            "plc_connections",
            None,
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
        "plc_connections",
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
            "plc_connections",
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
        "plc_connections",
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

// ---------------------------------------------------------------------------
// T21 S1-c（docs/banto-hub-t21-design.md §3・§4）: 構成補助ツール第二弾
// （接続 update/test・グループ CRUD）。S1-b（[`tool_create_connection`]/
// [`tool_delete_connection`]）と全く同じ書き方をそのまま踏襲する - ゲート
// （admin スコープ・監査・delete 系 confirm）・mutation フロー（tx →
// preflight → commit → catalog commit、収集中は pending queue）のどちらも
// 二重実装しない。
// ---------------------------------------------------------------------------

/// Copilot 指摘（PR #268）対応: `update_connection`/`update_group`は
/// フォームと同じ「全項目指定(PUT 置換)」の設計だが、`PlcConnectionPayload`/
/// `CollectionGroupPayload`は`#[serde(default = ...)]`を持つため、
/// `serde_json::from_value`に直接通すと省略フィールドが黙って既定値に
/// 上書きされてしまう（例: `update_group`で`enabled`省略→既定 true に
/// 戻る）。inputSchema の`required`だけでは MCP クライアントが無視した
/// 場合に防げないため、デシリアライズ前に`arguments`（JSON object）へ
/// この関数で必須キーの充足を確認し、1つでも欠けていれば拒否する。
fn require_all_fields(arguments: &Value, required: &[&str]) -> Option<Value> {
    let Some(obj) = arguments.as_object() else {
        return Some(tool_error(
            "missing_fields: arguments はオブジェクトである必要があります。",
        ));
    };
    let missing: Vec<&str> = required
        .iter()
        .filter(|key| !obj.contains_key(**key))
        .copied()
        .collect();
    if missing.is_empty() {
        None
    } else {
        Some(tool_error(format!(
            "missing_fields: update は全項目指定が必要です(省略項目の既定値上書きを防ぐため)。不足: {}",
            missing.join(", ")
        )))
    }
}

/// [`tool_update_connection`]の必須キー一覧 - `PlcConnectionPayload`の
/// wire フィールド（camelCase）全部 + `id`。inputSchema の
/// `update_connection.required`と同期させること。
const UPDATE_CONNECTION_REQUIRED_FIELDS: [&str; 9] = [
    "id",
    "name",
    "protocol",
    "host",
    "port",
    "unitId",
    "enabled",
    "simulation",
    "wordOrder",
];

/// [`tool_update_group`]の必須キー一覧 - `CollectionGroupPayload`の
/// wire フィールド（camelCase）全部 + `id`。inputSchema の
/// `update_group.required`と同期させること。
const UPDATE_GROUP_REQUIRED_FIELDS: [&str; 6] = [
    "id",
    "name",
    "plcConnectionId",
    "periodMs",
    "enabled",
    "defaultWritable",
];

/// [`tool_update_tag`]の必須キー一覧 - `TagPayload`の wire フィールド
/// （camelCase）全部 + `id`。`expectedRevision`は楽観ロック用の任意項目
/// なので含めない（設計 §4 実装指示 T21-S1d 参照）。inputSchema の
/// `update_tag.required`と同期させること。
const UPDATE_TAG_REQUIRED_FIELDS: [&str; 22] = [
    "id",
    "name",
    "collectionGroupId",
    "address",
    "dataType",
    "stringLength",
    "stringEncoding",
    "rawLo",
    "rawHi",
    "engLo",
    "engHi",
    "unit",
    "decimals",
    "thresholdH",
    "thresholdHh",
    "thresholdL",
    "thresholdLl",
    "enabled",
    "writable",
    "tagKind",
    "expression",
    "retain",
];

// --- 12. update_connection ----------------------------------------------------

/// `crate::rest::plc_connections_update`（admin REST）と全く同じ mutation
/// フロー - [`tool_create_connection`]と同型（`update_tx`を使う点と`id`引数
/// を取る点だけが違う）。`arguments`には`id`と`PlcConnectionPayload`の各
/// フィールドを同じオブジェクトに混在させる（`PlcConnectionPayload`は
/// `deny_unknown_fields`ではないので、`id`が混ざっていても
/// `serde_json::from_value`は無視して問題なくデシリアライズできる）。
async fn tool_update_connection(
    state: &McpState,
    ctx: &ApiKeyContext,
    arguments: Option<Value>,
) -> Result<Value, RpcError> {
    if let Err(err) = require_admin_scope(ctx) {
        return Ok(err);
    }
    let arguments = arguments.ok_or_else(|| RpcError::invalid_params("arguments is required"))?;
    if let Some(err) = require_all_fields(&arguments, &UPDATE_CONNECTION_REQUIRED_FIELDS) {
        return Ok(err);
    }
    let id = arguments
        .get("id")
        .and_then(Value::as_i64)
        .ok_or_else(|| RpcError::invalid_params("arguments.id (integer) is required"))?;
    let input: PlcConnectionPayload = serde_json::from_value(arguments)
        .map_err(|err| RpcError::invalid_params(format!("接続の入力が不正です: {err}")))?;

    let status = state.status.controller.status();
    if status.state != CollectionState::Stopped {
        let payload = json!({ "id": id, "input": input });
        let base_fingerprint = compute_pending_base_fingerprint(
            &state.plc_connections,
            &state.collection_groups,
            "plc_connections.update",
            &payload,
        )
        .await;
        let pending = match state
            .pending_changes
            .create_pending(
                "plc_connections.update",
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
            "update",
            "plc_connections",
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
    let updated = match state
        .plc_connections
        .update_tx(&mut tx, id, input.into())
        .await
    {
        Ok(updated) => updated,
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
        "update",
        "plc_connections",
        Some(&id.to_string()),
        Some(json!({ "name": updated.name, "enabled": updated.enabled })),
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
    Ok(tool_ok(json!({ "updated": updated })))
}

// --- 13. test_connection ----------------------------------------------------

/// `crate::rest::plc_connections_test`（admin REST）をそのまま呼ぶだけ -
/// 疎通確認本体（virtual/simulation の即時拒否・プロトコル別ダイヤル・
/// broker セッション再利用・エラー分類・所要時間計測）は一切再実装せず
/// [`run_plc_connection_test`]（REST ハンドラと共有、`crate::rest`）へ
/// 委譲する。レジストリへの書き込みが一切発生しない読み取り専用の疎通確認
/// なので、REST 同様 pending queue・commit・監査のいずれも行わない。
async fn tool_test_connection(
    state: &McpState,
    ctx: &ApiKeyContext,
    arguments: Option<Value>,
) -> Result<Value, RpcError> {
    if let Err(err) = require_admin_scope(ctx) {
        return Ok(err);
    }
    let arguments = arguments.ok_or_else(|| RpcError::invalid_params("arguments is required"))?;
    let payload: PlcConnectionTestPayload = serde_json::from_value(arguments)
        .map_err(|err| RpcError::invalid_params(format!("接続テストの入力が不正です: {err}")))?;

    let result = run_plc_connection_test(&state.manager, &payload).await;
    Ok(tool_ok(
        serde_json::to_value(result).unwrap_or_else(|_| json!({})),
    ))
}

// --- 14. list_groups ----------------------------------------------------

async fn tool_list_groups(state: &McpState, ctx: &ApiKeyContext) -> Value {
    if let Err(err) = require_admin_scope(ctx) {
        return err;
    }
    match state.collection_groups.list(ListParams::default()).await {
        Ok(result) => tool_ok(json!({ "groups": result.rows })),
        Err(err) => tool_error(format!("グループ一覧の取得に失敗しました: {err}")),
    }
}

// --- 15. create_group ----------------------------------------------------

/// `crate::rest::collection_groups_create`（admin REST）と全く同じ
/// mutation フロー - [`tool_create_connection`]と同型（対象サービスが
/// `state.collection_groups`、source/resource が`"collection_groups"`
/// になる点だけが違う）。
async fn tool_create_group(
    state: &McpState,
    ctx: &ApiKeyContext,
    arguments: Option<Value>,
) -> Result<Value, RpcError> {
    if let Err(err) = require_admin_scope(ctx) {
        return Ok(err);
    }
    let arguments = arguments.ok_or_else(|| RpcError::invalid_params("arguments is required"))?;
    let input: CollectionGroupPayload = serde_json::from_value(arguments)
        .map_err(|err| RpcError::invalid_params(format!("グループの入力が不正です: {err}")))?;

    let status = state.status.controller.status();
    if status.state != CollectionState::Stopped {
        let payload = json!({ "input": input });
        let base_fingerprint = compute_pending_base_fingerprint(
            &state.plc_connections,
            &state.collection_groups,
            "collection_groups.create",
            &payload,
        )
        .await;
        let pending = match state
            .pending_changes
            .create_pending(
                "collection_groups.create",
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
            "collection_groups",
            None,
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
    let created = match state
        .collection_groups
        .create_tx(&mut tx, input.into())
        .await
    {
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
        "collection_groups",
        Some(&created.id.to_string()),
        Some(json!({ "name": created.name, "enabled": created.enabled })),
    )
    .await;
    commit_catalog_and_notify(
        &state.manager,
        &state.status.controller,
        &state.events,
        "collection_groups",
        snapshot,
        state.legacy_live_reconfigure,
    )
    .await;
    Ok(tool_ok(json!({ "created": created })))
}

// --- 16. update_group ----------------------------------------------------

/// `crate::rest::collection_groups_update`（admin REST）と全く同じ
/// mutation フロー - [`tool_update_connection`]と同型。
async fn tool_update_group(
    state: &McpState,
    ctx: &ApiKeyContext,
    arguments: Option<Value>,
) -> Result<Value, RpcError> {
    if let Err(err) = require_admin_scope(ctx) {
        return Ok(err);
    }
    let arguments = arguments.ok_or_else(|| RpcError::invalid_params("arguments is required"))?;
    if let Some(err) = require_all_fields(&arguments, &UPDATE_GROUP_REQUIRED_FIELDS) {
        return Ok(err);
    }
    let id = arguments
        .get("id")
        .and_then(Value::as_i64)
        .ok_or_else(|| RpcError::invalid_params("arguments.id (integer) is required"))?;
    let input: CollectionGroupPayload = serde_json::from_value(arguments)
        .map_err(|err| RpcError::invalid_params(format!("グループの入力が不正です: {err}")))?;

    let status = state.status.controller.status();
    if status.state != CollectionState::Stopped {
        let payload = json!({ "id": id, "input": input });
        let base_fingerprint = compute_pending_base_fingerprint(
            &state.plc_connections,
            &state.collection_groups,
            "collection_groups.update",
            &payload,
        )
        .await;
        let pending = match state
            .pending_changes
            .create_pending(
                "collection_groups.update",
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
            "update",
            "collection_groups",
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
    let updated = match state
        .collection_groups
        .update_tx(&mut tx, id, input.into())
        .await
    {
        Ok(updated) => updated,
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
        "update",
        "collection_groups",
        Some(&id.to_string()),
        Some(json!({ "name": updated.name, "enabled": updated.enabled })),
    )
    .await;
    commit_catalog_and_notify(
        &state.manager,
        &state.status.controller,
        &state.events,
        "collection_groups",
        snapshot,
        state.legacy_live_reconfigure,
    )
    .await;
    Ok(tool_ok(json!({ "updated": updated })))
}

// --- 17. delete_group ----------------------------------------------------

/// `crate::rest::collection_groups_delete`（admin REST）と全く同じ
/// mutation フロー（`cascade_delete_tx` - 配下のタグごと削除・履歴は残す）。
/// [`tool_delete_connection`]と同型。不可逆操作のため
/// `arguments.confirm == true` を要求する。
async fn tool_delete_group(
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
            "collection_groups.delete",
            &payload,
        )
        .await;
        let pending = match state
            .pending_changes
            .create_pending(
                "collection_groups.delete",
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
            "collection_groups",
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
    let cascade = match state.collection_groups.cascade_delete_tx(&mut tx, id).await {
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
    let cascade_detail = json!({ "deletedTags": cascade.deleted_tags });
    audit_config_action(
        state,
        ctx,
        "delete",
        "collection_groups",
        Some(&id.to_string()),
        Some(json!({ "cascade": cascade_detail.clone() })),
    )
    .await;
    commit_catalog_and_notify(
        &state.manager,
        &state.status.controller,
        &state.events,
        "collection_groups",
        snapshot,
        state.legacy_live_reconfigure,
    )
    .await;
    Ok(tool_ok(
        json!({ "deleted": true, "id": id, "cascade": cascade_detail }),
    ))
}

// ---------------------------------------------------------------------------
// T21 S1-d（docs/banto-hub-t21-design.md §3・§4）: 構成補助ツール第三弾
// （タグ CRUD）。S1-b/S1-c（[`tool_create_connection`]/[`tool_update_connection`]/
// [`tool_delete_connection`]）と全く同じ書き方をそのまま踏襲する - ゲート
// （admin スコープ・監査・delete 系 confirm・update 系 全項目必須）・
// mutation フロー（tx → preflight → commit → catalog commit、収集中は
// pending queue）のどちらも二重実装しない。タグは末端リソースなので
// delete は非 cascade（`TagService::delete_tx`、[`crate::rest::tags_delete`]
// と同じ）。`update_tag`だけは他の update 系と異なり
// `TagService::update_tx`が`TagUpdateError`（`Banto`/`RevisionConflict`の
// 2バリアント）を返す - REST の `tags_update`（`crate::rest`）の match を
// そのままミラーする。
// ---------------------------------------------------------------------------

// --- 18. get_tag ----------------------------------------------------------

/// RMW（read-modify-write）用の読み取り専用ツール - `update_tag`が全項目
/// 指定の PUT 置換を要求するため、クライアントはまずこれで現在の全
/// フィールド（`revision`含む）を取得してから必要な項目だけ変更して
/// 送り返す。副作用が無いので監査もしない（[`tool_list_connections`]と
/// 同じ「読み取り系は監査しない」規律）。read スコープの一覧ツール
/// [`tool_list_tags`]とは別物（admin・単一・全フィールド）。
async fn tool_get_tag(
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

    match state.tags.get(id).await {
        Ok(tag) => Ok(tool_ok(json!({ "tag": tag }))),
        Err(err) => Ok(tool_error(format!("タグの取得に失敗しました: {err}"))),
    }
}

// --- 19. create_tag ----------------------------------------------------

/// `crate::rest::tags_create`（admin REST）と全く同じ mutation フロー -
/// [`tool_create_connection`]と同型（対象サービスが`state.tags`、
/// source/resource が`"tags"`になる点だけが違う）。
async fn tool_create_tag(
    state: &McpState,
    ctx: &ApiKeyContext,
    arguments: Option<Value>,
) -> Result<Value, RpcError> {
    if let Err(err) = require_admin_scope(ctx) {
        return Ok(err);
    }
    let arguments = arguments.ok_or_else(|| RpcError::invalid_params("arguments is required"))?;
    let input: TagPayload = serde_json::from_value(arguments)
        .map_err(|err| RpcError::invalid_params(format!("タグの入力が不正です: {err}")))?;

    let status = state.status.controller.status();
    if status.state != CollectionState::Stopped {
        let payload = json!({ "input": input });
        let base_fingerprint = compute_pending_base_fingerprint(
            &state.plc_connections,
            &state.collection_groups,
            "tags.create",
            &payload,
        )
        .await;
        let pending = match state
            .pending_changes
            .create_pending(
                "tags.create",
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
            "tags",
            None,
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
    let created = match state.tags.create_tx(&mut tx, input.into()).await {
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
        "tags",
        Some(&created.id.to_string()),
        Some(json!({ "name": created.name, "enabled": created.enabled })),
    )
    .await;
    commit_catalog_and_notify(
        &state.manager,
        &state.status.controller,
        &state.events,
        "tags",
        snapshot,
        state.legacy_live_reconfigure,
    )
    .await;
    Ok(tool_ok(json!({ "created": created })))
}

// --- 20. update_tag ----------------------------------------------------

/// `crate::rest::tags_update`（admin REST）と全く同じ mutation フロー -
/// [`tool_update_connection`]と同型だが、`TagService::update_tx`が
/// `TagUpdateError`（`Banto`/`RevisionConflict`の2バリアント）を返す点が
/// 違う - REST の `tags_update` の match をそのままミラーする
/// （[`TagUpdateError`]のdoc comment参照）。
async fn tool_update_tag(
    state: &McpState,
    ctx: &ApiKeyContext,
    arguments: Option<Value>,
) -> Result<Value, RpcError> {
    if let Err(err) = require_admin_scope(ctx) {
        return Ok(err);
    }
    let arguments = arguments.ok_or_else(|| RpcError::invalid_params("arguments is required"))?;
    if let Some(err) = require_all_fields(&arguments, &UPDATE_TAG_REQUIRED_FIELDS) {
        return Ok(err);
    }
    let id = arguments
        .get("id")
        .and_then(Value::as_i64)
        .ok_or_else(|| RpcError::invalid_params("arguments.id (integer) is required"))?;
    let input: TagPayload = serde_json::from_value(arguments)
        .map_err(|err| RpcError::invalid_params(format!("タグの入力が不正です: {err}")))?;

    let status = state.status.controller.status();
    if status.state != CollectionState::Stopped {
        let payload = json!({ "id": id, "input": input });
        let base_fingerprint = compute_pending_base_fingerprint(
            &state.plc_connections,
            &state.collection_groups,
            "tags.update",
            &payload,
        )
        .await;
        let pending = match state
            .pending_changes
            .create_pending(
                "tags.update",
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
            "update",
            "tags",
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
    let updated = match state.tags.update_tx(&mut tx, id, input.into()).await {
        Ok(updated) => updated,
        // T18-1と同じ楽観ロック競合 - REST の `tags_update` の match と
        // 同じ2分岐（[`TagUpdateError`]のdoc comment参照）。
        Err(TagUpdateError::RevisionConflict(current)) => {
            let _ = tx.rollback().await;
            return Ok(tool_error(format!(
                "revision_conflict: タグが他で更新されています。get_tag で最新の revision を取得して再試行してください(現在の revision: {})。",
                current.revision
            )));
        }
        Err(TagUpdateError::Banto(err)) => {
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
        "update",
        "tags",
        Some(&id.to_string()),
        Some(json!({ "name": updated.name, "enabled": updated.enabled })),
    )
    .await;
    commit_catalog_and_notify(
        &state.manager,
        &state.status.controller,
        &state.events,
        "tags",
        snapshot,
        state.legacy_live_reconfigure,
    )
    .await;
    Ok(tool_ok(json!({ "updated": updated })))
}

// --- 21. delete_tag ----------------------------------------------------

/// `crate::rest::tags_delete`（admin REST）と全く同じ mutation フロー -
/// [`tool_delete_connection`]と同型だが、タグは末端リソースのため
/// `TagService::delete_tx`は cascade を持たない（[`banto_tags::TagService`]
/// のdoc comment「No delete guard is needed here」参照）。不可逆操作のため
/// `arguments.confirm == true` を要求する。
async fn tool_delete_tag(
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
            "tags.delete",
            &payload,
        )
        .await;
        let pending = match state
            .pending_changes
            .create_pending(
                "tags.delete",
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
            "tags",
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
    if let Err(err) = state.tags.delete_tx(&mut tx, id).await {
        let _ = tx.rollback().await;
        return Ok(tool_error(format!("{err}")));
    }
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
    audit_config_action(state, ctx, "delete", "tags", Some(&id.to_string()), None).await;
    commit_catalog_and_notify(
        &state.manager,
        &state.status.controller,
        &state.events,
        "tags",
        snapshot,
        state.legacy_live_reconfigure,
    )
    .await;
    Ok(tool_ok(json!({ "deleted": true, "id": id })))
}

// --- T21 S2-a: ランタイム制御ツール（構成 CRUD とは別系統） ---------------
//
// `set_collection`/`set_write_control`はレジストリ mutation ではなく
// `CollectionController`/`WriteControl`というライブ状態を直接切り替える
// ランタイム制御 - ここまでの接続/グループ/タグ系ツールと違い、
// pending queue（`compute_pending_base_fingerprint`/`create_pending`）も
// `preflight_transaction`/`commit_catalog_and_notify`も一切通らない（それらは
// レジストリの内容が変わる mutation 専用の経路）。REST の
// `crate::rest::collection_start`/`collection_stop`/`write_control_set`と
// 全く同じ呼び出し（`state.status.controller.start/stop`・
// `state.write_control.enable/disable` + `persist_enabled`）を行い、
// 監査の宛先だけが違う（[`audit_config_action`]のdoc comment参照）。
// 可逆操作なので他の構成ツールと違い confirm は要求しない（設計の
// confirm 必須は delete 系の不可逆操作限定）。

/// `crate::rest::CollectionStatusResponse`と同じ形に整形する
/// （camelCase・フィールド構成を REST と揃えて MCP/REST 間で表現を一致
/// させる）。
fn collection_status_json(status: &crate::controller::CollectionStatus) -> Value {
    json!({
        "state": status.state.as_str(),
        "mode": status.mode.as_str(),
        "runId": status.run_id,
        "configuredRevision": status.configured_revision,
        "runningRevision": status.running_revision,
        "lastError": status.last_error,
    })
}

// --- 19. set_collection -----------------------------------------------------

async fn tool_set_collection(
    state: &McpState,
    ctx: &ApiKeyContext,
    arguments: Option<Value>,
) -> Result<Value, RpcError> {
    if let Err(err) = require_admin_scope(ctx) {
        return Ok(err);
    }
    let arguments = arguments.ok_or_else(|| RpcError::invalid_params("arguments is required"))?;
    let action = arguments
        .get("action")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("arguments.action (string) is required"))?;

    // `crate::rest::collection_start`/`collection_stop`と同じ呼び分け
    // （このモジュールの doc comment「§3.7」節と同じ「REST と全く同じ
    // 呼び出し」規律）。
    let status = match action {
        "start" => state.status.controller.start(RunMode::Configured).await,
        "stop" => state.status.controller.stop().await,
        other => {
            return Ok(tool_error(format!(
                "arguments.action は start または stop を指定してください(受け取った値: {other})"
            )));
        }
    };
    audit_config_action(
        state,
        ctx,
        action,
        "collection",
        None,
        Some(collection_status_json(&status)),
    )
    .await;
    Ok(tool_ok(collection_status_json(&status)))
}

// --- 20. set_write_control ---------------------------------------------------

async fn tool_set_write_control(
    state: &McpState,
    ctx: &ApiKeyContext,
    arguments: Option<Value>,
) -> Result<Value, RpcError> {
    if let Err(err) = require_admin_scope(ctx) {
        return Ok(err);
    }
    let arguments = arguments.ok_or_else(|| RpcError::invalid_params("arguments is required"))?;
    let enabled = arguments
        .get("enabled")
        .and_then(Value::as_bool)
        .ok_or_else(|| RpcError::invalid_params("arguments.enabled (boolean) is required"))?;

    // `crate::rest::write_control_set`と全く同じ呼び出し（ライブフラグの
    // 切り替え + 表示専用の永続値更新）- 永続化の失敗は REST と同じく
    // eprintln で握って処理を続ける（`crate::write_control::persist_enabled`
    // のdoc comment参照: 表示専用の永続値であり次回起動時のライブフラグには
    // 影響しないため、致命的に扱う必要がない）。
    if enabled {
        state.write_control.enable();
    } else {
        state.write_control.disable();
    }
    if let Err(err) = persist_enabled(&state.manager.pool(), enabled, Some(ctx.name.as_str())).await
    {
        eprintln!("banto-hub: 書き込み受付状態の永続化に失敗しました(MCP): {err}");
    }

    audit_config_action(
        state,
        ctx,
        if enabled { "enable" } else { "disable" },
        "write_control",
        Some("1"),
        Some(json!({ "writeEnabled": enabled })),
    )
    .await;
    Ok(tool_ok(
        json!({ "writeEnabled": state.write_control.is_enabled() }),
    ))
}
