//! MCP（Model Context Protocol）サーバーの骨組み。banto-hub と ChronoGazer
//! （予定）が共有する（#1161 PR2、設計書 `docs/chronogazer/chronogazer-agent-api-design.md`
//! §5.1）。
//!
//! ## 何を持ち、何を持たないか
//!
//! この crate は **`POST /mcp` に載せる JSON-RPC 2.0 の骨組み** だけを持つ:
//!
//! - 本文の受け付け（[`handle_request`]）: 本文のバイト列を自分で読み、不正な
//!   JSON でも `-32700` を JSON-RPC の形で返す。`jsonrpc` が `"2.0"` で `method`
//!   が文字列かを確かめ（外れたら `-32600`）、`id` の無い通知は 202 で本文なし、
//!   成功もエラーも HTTP 200。
//! - `initialize`（`protocolVersion` を echo、無ければ [`DEFAULT_PROTOCOL_VERSION`]）・
//!   `ping`・`tools/list`・`tools/call`・それ以外は `-32601`。
//! - JSON-RPC のエラー型 [`RpcError`] とコード定数。
//! - **ツール表** [`ToolRegistry`]。
//! - ツールの結果の形（[`tool_ok`]・[`tool_error`]・[`banto_error_tool_error`] ほか）と、
//!   引数の確かめの補助（[`require_all_fields`]）。
//!
//! **認証・監査・スコープの確かめ・ツールの中身・アプリの状態の型は各アプリに残す**。
//! アプリは axum のハンドラで認証を済ませたうえで、状態 `S` と主体 `C` を
//! [`handle_request`] に渡すだけにする。この crate は `S`・`C` の中身を知らない。
//!
//! ## なぜ自前 JSON-RPC か
//!
//! `rmcp` 等の MCP SDK は入れない（HUB のオーナー決定 2026-09-04、
//! `apps/banto-hub/core/src/mcp.rs` 冒頭）。必要なのは JSON-RPC 2.0 のごく一部を
//! 1 本の HTTP エンドポイントへ載せること（Streamable HTTP のうち非ストリーミングの
//! `application/json` サブセット）だけで、`serde_json` と axum 以上の依存を増やす
//! 理由がない。
//!
//! ## なぜツール表か
//!
//! 以前の HUB は `tool_definitions()`（定義の JSON の `Vec`）と
//! `handle_tools_call` の `match`（名前から処理への振り分け）の 2 か所でツールを
//! 持っていて、足すときに両方を直す必要があり、片方を忘れても型では気づけなかった。
//! [`ToolRegistry`] は 1 つのツールを「定義の JSON（`name` を含む）」と「処理」の
//! 組として 1 か所に置き、`tools/list`（登録順）と `tools/call`（名前引き）を同じ表から
//! 作る。名前の重複・`name` の欠けは作るときに panic する（起動時に必ず気づく）。

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use banto_core::{BantoError, FieldError};
use serde_json::{json, Value};

/// クライアントが `initialize` で `protocolVersion` を送ってこなかった場合の
/// 既定値。仕様は固定していないので、実装時点の MCP 仕様の一版を素直に
/// 選んだだけ - クライアントが値を送ってくればそれをそのまま echo する。
pub const DEFAULT_PROTOCOL_VERSION: &str = "2025-06-18";

// --- JSON-RPC エラーコード（JSON-RPC 2.0 仕様の予約範囲） -------------------

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;

/// `tools/call` のドメイン上の失敗（スコープ不足・対象不明・ゲート拒否など）
/// ではなく、JSON-RPC そのものが壊れている場合のエラー。ドメイン上の失敗は
/// [`tool_error`] で `isError: true` の結果にする。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(INVALID_PARAMS, message)
    }

    pub fn method_not_found(message: impl Into<String>) -> Self {
        Self::new(METHOD_NOT_FOUND, message)
    }
}

// --- ツール表 -----------------------------------------------------------

/// ツールの処理が返す future。`'a` は状態・主体の借用の寿命。
pub type ToolFuture<'a> = Pin<Box<dyn Future<Output = Result<Value, RpcError>> + Send + 'a>>;

/// ツールの処理: `(状態, 主体, arguments)`。`arguments` は `params.arguments`
/// そのまま（無ければ `None`）。ドメイン上の失敗は `Ok(tool_error(..))`、
/// JSON-RPC の不正だけを `Err` にする。
pub type ToolHandler<S, C> = for<'a> fn(&'a S, &'a C, Option<Value>) -> ToolFuture<'a>;

struct Tool<S, C> {
    definition: Value,
    handler: ToolHandler<S, C>,
}

/// ツール表。状態 `S`・主体 `C` はアプリが決める。1 回だけ作って使い回す。
pub struct ToolRegistry<S, C> {
    server_name: String,
    server_version: String,
    tools: Vec<Tool<S, C>>,
    by_name: HashMap<String, usize>,
}

impl<S, C> ToolRegistry<S, C> {
    /// `server_name`・`server_version` は `initialize` の `serverInfo`。版は
    /// アプリの `CARGO_PKG_VERSION` を渡す（この crate の `env!` を使うと
    /// アプリの版と食い違う）。
    pub fn new(server_name: impl Into<String>, server_version: impl Into<String>) -> Self {
        Self {
            server_name: server_name.into(),
            server_version: server_version.into(),
            tools: Vec::new(),
            by_name: HashMap::new(),
        }
    }

    /// ツールを 1 つ足す。`definition` は `tools/list` にそのまま載せる JSON
    /// （`name`・`description`・`inputSchema`）。`name` が文字列でない・空、
    /// または既に登録済みなら panic する。
    #[must_use]
    pub fn tool(mut self, definition: Value, handler: ToolHandler<S, C>) -> Self {
        let name = definition
            .get("name")
            .and_then(Value::as_str)
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| panic!("MCP tool definition without a name: {definition}"))
            .to_string();
        if self
            .by_name
            .insert(name.clone(), self.tools.len())
            .is_some()
        {
            panic!("MCP tool registered twice: {name}");
        }
        self.tools.push(Tool {
            definition,
            handler,
        });
        self
    }

    /// 登録したツールの数。
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    fn initialize(&self, params: Option<Value>) -> Value {
        let protocol_version = params
            .as_ref()
            .and_then(|p| p.get("protocolVersion"))
            .and_then(Value::as_str)
            .unwrap_or(DEFAULT_PROTOCOL_VERSION);
        json!({
            "protocolVersion": protocol_version,
            "capabilities": { "tools": {} },
            "serverInfo": {
                "name": self.server_name,
                "version": self.server_version,
            },
        })
    }

    fn tools_list(&self) -> Value {
        let tools: Vec<&Value> = self.tools.iter().map(|t| &t.definition).collect();
        json!({ "tools": tools })
    }

    async fn tools_call(
        &self,
        state: &S,
        ctx: &C,
        params: Option<Value>,
    ) -> Result<Value, RpcError> {
        let params = params.ok_or_else(|| RpcError::invalid_params("params is required"))?;
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::invalid_params("params.name (string) is required"))?;
        let arguments = params.get("arguments").cloned();
        let Some(&index) = self.by_name.get(name) else {
            return Err(RpcError::invalid_params(format!("unknown tool: {name}")));
        };
        (self.tools[index].handler)(state, ctx, arguments).await
    }

    async fn dispatch(
        &self,
        state: &S,
        ctx: &C,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, RpcError> {
        match method {
            "initialize" => Ok(self.initialize(params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(self.tools_list()),
            "tools/call" => self.tools_call(state, ctx, params).await,
            other => Err(RpcError::method_not_found(format!(
                "unknown method: {other}"
            ))),
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

/// `POST /mcp` の本文を受けて応答を作る。アプリの axum ハンドラは、認証を
/// 済ませたうえでこれを呼ぶだけにする。
///
/// 本文を `axum::Json` で受けない理由: 不正な JSON でも `-32700` を JSON-RPC の
/// 形で返したいため、生のバイト列を自分で `serde_json::from_slice` する。
pub async fn handle_request<S, C>(
    registry: &ToolRegistry<S, C>,
    state: &S,
    ctx: &C,
    body: &[u8],
) -> Response {
    let raw: Value = match serde_json::from_slice(body) {
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
    // （`notifications/initialized` など）。
    let Some(id) = id else {
        return StatusCode::ACCEPTED.into_response();
    };

    match registry.dispatch(state, ctx, &method, params).await {
        Ok(result) => json_rpc_success(id, result),
        Err(err) => json_rpc_error(id, err.code, &err.message),
    }
}

// --- tools/call 結果の組み立て ---------------------------------------------

/// `{ content: [{type:"text", text}], isError }` - ドメイン上の失敗は
/// すべてこの形（JSON-RPC エラーにしない）。
pub fn tool_result(text: String, is_error: bool) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    })
}

/// 成功: JSON を文字列にした `text`。
pub fn tool_ok(payload: Value) -> Value {
    tool_result(payload.to_string(), false)
}

/// 失敗: `isError: true` の `text`。
pub fn tool_error(message: impl Into<String>) -> Value {
    tool_result(message.into(), true)
}

/// `Vec<FieldError>` を単一の [`tool_error`] テキスト（`field: message` を
/// `; ` で連結）へ整形する。MCP の結果は単一メッセージなので、REST のように
/// 配列では返せない。
pub fn field_errors_tool_error(errors: Vec<FieldError>) -> Value {
    let joined = errors
        .into_iter()
        .map(|err| format!("{}: {}", err.field, err.message))
        .collect::<Vec<_>>()
        .join("; ");
    tool_error(format!("入力が不正です: {joined}"))
}

/// サービス層の `BantoError` を [`tool_error`] に変換する。`Validation` なら
/// [`field_errors_tool_error`] と同じ整形で field 詳細を展開し、それ以外の
/// バリアントは `Display` をそのまま使う。
pub fn banto_error_tool_error(err: &BantoError) -> Value {
    match err {
        BantoError::Validation { field_errors } => field_errors_tool_error(field_errors.clone()),
        other => tool_error(format!("{other}")),
    }
}

// --- 引数の確かめの補助 -----------------------------------------------------

/// 「全項目指定（PUT 置換）」のツールで、`arguments` に必須キーが全部あるかを
/// デシリアライズの前に確かめる。`#[serde(default)]` を持つ型へ直接通すと、
/// 省略した項目が黙って既定値に上書きされてしまうため（inputSchema の
/// `required` だけでは、それを無視するクライアントを防げない）。
/// 欠けがあれば `Some(tool_error)` を返す。
pub fn require_all_fields(arguments: &Value, required: &[&str]) -> Option<Value> {
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

#[cfg(test)]
mod tests {
    use super::*;

    struct St;
    struct Cx;

    fn echo<'a>(_s: &'a St, _c: &'a Cx, args: Option<Value>) -> ToolFuture<'a> {
        Box::pin(async move { Ok(tool_ok(json!({ "args": args }))) })
    }
    fn fail<'a>(_s: &'a St, _c: &'a Cx, _args: Option<Value>) -> ToolFuture<'a> {
        Box::pin(async { Ok(tool_error("nope")) })
    }
    fn bad<'a>(_s: &'a St, _c: &'a Cx, _args: Option<Value>) -> ToolFuture<'a> {
        Box::pin(async { Err(RpcError::invalid_params("arguments is required")) })
    }

    fn def(name: &str) -> Value {
        json!({ "name": name, "description": "d", "inputSchema": { "type": "object" } })
    }

    fn registry() -> ToolRegistry<St, Cx> {
        ToolRegistry::new("test-app", "9.9.9")
            .tool(def("echo"), echo)
            .tool(def("fail"), fail)
            .tool(def("bad"), bad)
    }

    async fn post(body: &[u8]) -> (StatusCode, Value) {
        let response = handle_request(&registry(), &St, &Cx, body).await;
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn call(value: Value) -> (StatusCode, Value) {
        post(&serde_json::to_vec(&value).unwrap()).await
    }

    fn rpc(method: &str, params: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params })
    }

    #[tokio::test]
    async fn malformed_json_is_parse_error_with_http_200() {
        let (status, body) = post(b"{ nope").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["id"], Value::Null);
        assert_eq!(body["error"]["code"], PARSE_ERROR);
        assert_eq!(body["error"]["message"], "Parse error");
    }

    #[tokio::test]
    async fn invalid_envelope_is_invalid_request_and_keeps_the_id() {
        let (status, body) = call(json!({ "jsonrpc": "1.0", "id": 5, "method": "ping" })).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["id"], 5);
        assert_eq!(body["error"]["code"], INVALID_REQUEST);
        let (_, body) = call(json!({ "jsonrpc": "2.0" })).await;
        assert_eq!(body["id"], Value::Null);
        assert_eq!(body["error"]["code"], INVALID_REQUEST);
        let (_, body) = call(json!([1, 2])).await;
        assert_eq!(body["error"]["code"], INVALID_REQUEST);
    }

    #[tokio::test]
    async fn notification_without_id_is_202_with_empty_body() {
        let response = handle_request(
            &registry(),
            &St,
            &Cx,
            br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(bytes.is_empty());
    }

    #[tokio::test]
    async fn unknown_method_is_method_not_found() {
        let (status, body) = call(rpc("bogus/x", json!({}))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["error"]["code"], METHOD_NOT_FOUND);
        assert_eq!(body["error"]["message"], "unknown method: bogus/x");
    }

    #[tokio::test]
    async fn initialize_echoes_the_version_and_defaults_without_one() {
        let (_, body) = call(rpc(
            "initialize",
            json!({ "protocolVersion": "2024-11-05" }),
        ))
        .await;
        assert_eq!(body["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(body["result"]["capabilities"], json!({ "tools": {} }));
        assert_eq!(body["result"]["serverInfo"]["name"], "test-app");
        assert_eq!(body["result"]["serverInfo"]["version"], "9.9.9");
        let (_, body) = call(rpc("initialize", json!({}))).await;
        assert_eq!(body["result"]["protocolVersion"], DEFAULT_PROTOCOL_VERSION);
        let (_, body) = call(json!({ "jsonrpc": "2.0", "id": 2, "method": "initialize" })).await;
        assert_eq!(body["result"]["protocolVersion"], DEFAULT_PROTOCOL_VERSION);
    }

    #[tokio::test]
    async fn ping_returns_an_empty_object() {
        let (_, body) = call(rpc("ping", json!({}))).await;
        assert_eq!(body, json!({ "jsonrpc": "2.0", "id": 1, "result": {} }));
    }

    #[tokio::test]
    async fn tools_list_returns_definitions_in_registration_order() {
        let (_, body) = call(rpc("tools/list", json!({}))).await;
        let names: Vec<&str> = body["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["echo", "fail", "bad"]);
        assert_eq!(body["result"]["tools"][0], def("echo"));
    }

    #[tokio::test]
    async fn tools_call_success_and_is_error_are_both_results() {
        let (status, body) = call(rpc(
            "tools/call",
            json!({ "name": "echo", "arguments": { "a": 1 } }),
        ))
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["result"]["isError"], false);
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(text).unwrap(),
            json!({ "args": { "a": 1 } })
        );
        let (_, body) = call(rpc("tools/call", json!({ "name": "fail" }))).await;
        assert_eq!(body["result"]["isError"], true);
        assert_eq!(body["result"]["content"][0]["text"], "nope");
    }

    #[tokio::test]
    async fn tools_call_envelope_errors_are_invalid_params() {
        let (_, body) = call(rpc("tools/call", json!({ "name": "nothing" }))).await;
        assert_eq!(body["error"]["code"], INVALID_PARAMS);
        assert_eq!(body["error"]["message"], "unknown tool: nothing");
        let (_, body) = call(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call" })).await;
        assert_eq!(body["error"]["message"], "params is required");
        let (_, body) = call(rpc("tools/call", json!({}))).await;
        assert_eq!(body["error"]["message"], "params.name (string) is required");
        let (_, body) = call(rpc("tools/call", json!({ "name": "bad" }))).await;
        assert_eq!(body["error"]["code"], INVALID_PARAMS);
        assert_eq!(body["error"]["message"], "arguments is required");
    }

    #[test]
    #[should_panic(expected = "registered twice: echo")]
    fn duplicate_tool_names_panic() {
        let _ = ToolRegistry::<St, Cx>::new("a", "1")
            .tool(def("echo"), echo)
            .tool(def("echo"), fail);
    }

    #[test]
    #[should_panic(expected = "without a name")]
    fn a_definition_without_a_name_panics() {
        let _ = ToolRegistry::<St, Cx>::new("a", "1").tool(json!({ "description": "x" }), echo);
    }

    #[test]
    fn banto_validation_errors_are_expanded_to_field_messages() {
        let err = BantoError::Validation {
            field_errors: vec![
                FieldError {
                    field: "name".into(),
                    message: "empty".into(),
                },
                FieldError {
                    field: "port".into(),
                    message: "range".into(),
                },
            ],
        };
        let v = banto_error_tool_error(&err);
        assert_eq!(v["isError"], true);
        assert_eq!(
            v["content"][0]["text"],
            "入力が不正です: name: empty; port: range"
        );
    }

    #[test]
    fn require_all_fields_reports_missing_keys() {
        assert!(require_all_fields(&json!({ "a": 1, "b": 2 }), &["a", "b"]).is_none());
        let v = require_all_fields(&json!({ "a": 1 }), &["a", "b", "c"]).unwrap();
        assert!(v["content"][0]["text"]
            .as_str()
            .unwrap()
            .ends_with("不足: b, c"));
        assert_eq!(
            require_all_fields(&json!([]), &["a"]).unwrap()["isError"],
            true
        );
    }
}
