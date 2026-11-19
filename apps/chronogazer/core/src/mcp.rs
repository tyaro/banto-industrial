//! ChronoGazer の MCP（`POST /mcp`、#1161 PR5b、docs/chronogazer/chronogazer-agent-api-design.md
//! §5.3）。
//!
//! JSON-RPC の受け付け・`initialize`・`ping`・`tools/list`・`tools/call` の形は
//! HUB と共有する crate `banto_mcp` に任せ、ここにはツール表（[`tool_registry`]）と
//! ツールの中身・認証の層だけを置く。
//!
//! ## 認証（§5.3・§7）
//!
//! `/mcp` は `api_router` の CSRF の層の**外**にあり、受けるのは
//! `Authorization: Bearer cg_…` のキーだけ（`crate::rest::actor::require_mcp_api_key`）。
//! セッションのトークン（閲覧公開・この PC からの接続の grant を含む）・ほかの
//! アプリのキー・bearer 無しは 401。キーの照合は REST の `/api/*` と同じ関数を
//! 通るので、失効・期限切れ（401 と `denied`）・無い・形の誤り（401、記録なし）・
//! 照合できない（500）は REST と同じ。キーはブラウザが勝手に付けないので CSRF の
//! ヘッダーは求めない。公開範囲は REST の `/api/*` と同じ（`api_router` を載せる
//! listener すべて）。
//!
//! ## スコープ（§4.2）
//!
//! - **読み取りのツール**は ops を通らないので、ここで `Actor::can_read` を確かめる。
//!   確かめを忘れられないよう、読み取りの処理の関数は主体を**受け取らない**形
//!   （`fn(&McpState, Option<Value>)`）にし、表に載せる口を [`read_tool!`] だけに
//!   した（主体を確かめずに中身を呼ぶ道が型の上で無い）。キーは必ず `read` か
//!   `config` を持つので実際には通るが、足りなければ `denied`（`{tool}`・`mcp`）を
//!   記録して `isError` にする。
//! - **設定を変えるツール・収集の操作**は `crate::ops` の関数を
//!   `Via::Mcp { tool }` で呼ぶ（[`config_tool!`]）。権限の確かめ・前検査・監査・
//!   `registry_changed` はその関数の中にあり、ここでは書かない。監査は
//!   `origin: "mcp"`・`actor_role: "api_key"`・`actor_username: 名前（prefix）`、
//!   action・resource・entity_id・detail は REST と同じ。
//!
//! どちらのマクロも、ツール名を 1 回だけ書き、`tools/list` の定義の `name` と
//! `denied` の `{tool}`（`Via::Mcp`）の両方に使う（名前がずれない）。
//!
//! ## 結果の形
//!
//! 成功は JSON を文字列にした `text`（共有 crate の `tool_ok`）。作成は
//! `{created}`、更新は `{updated}`、削除は `{deleted: true, id}`（HUB と同じ）。
//! 収集が読む設定を変えるツール（接続先・収集グループ・記録項目・しきい値）の
//! 結果には `restartRequired: true` を足す: ChronoGazer はそれらを変えても収集へ
//! 自動では反映せず、`restart_collection` で反映するため（§5.3）。表示グループは
//! 表示のための設定で収集は読まないので付けない - ただし作成・更新でペンの Hub の
//! タグを記録項目として登録したとき（#1179 PR5、結果の `registeredTags` が空でない）
//! は記録項目を変えたので付ける。収集の操作の結果にも付けない。
//! 削除は `confirm: true` を要する（HUB と同じ）。
//!
//! 更新のツール（`update_connection`・`update_group`・`update_tag`・
//! `update_display_group`・`set_threshold`）は入力の全項目を必須にする（HUB と同じ。
//! 書き忘れた項目が既定値で黙って上書きされるのを防ぐ。楽観ロックの
//! `expectedRevision` だけは省略できる）。作成は REST と同じ既定値。
//!
//! 履歴（`/api/collect/history`）・イベントは入れない（§5.3）。

use std::sync::LazyLock;

use axum::body::Bytes;
use axum::extract::State;
use axum::middleware;
use axum::response::Response;
use axum::routing::post;
use axum::{Extension, Router};
use banto_core::{BantoError, ListParams};
use banto_mcp::{
    banto_error_tool_error, require_all_fields, tool_error, tool_ok, RpcError, ToolRegistry,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use crate::audit::AuditLogService;
use crate::collect::COLLECT_AUDIT_RESOURCE;
use crate::display_groups::{
    DisplayGroupPayload, DisplayGroupSaveResponse, AUDIT_RESOURCE as DISPLAY_GROUPS_RESOURCE,
};
use crate::ops::{CollectOperation, CollectOps, DisplayGroupOps, RegistryOps, ThresholdOps, Via};
use crate::rest::actor::{require_mcp_api_key, Actor, ApiKeyGate};
use crate::rest::{CollectionGroupPayload, PlcConnectionPayload, TagPayload};
use crate::tag_thresholds::{TagThresholdsPayload, AUDIT_RESOURCE as TAG_THRESHOLDS_RESOURCE};

/// `initialize` の `serverInfo.name`。
const SERVER_NAME: &str = "chronogazer";

const PLC_CONNECTIONS_RESOURCE: &str = "plc_connections";
const COLLECTION_GROUPS_RESOURCE: &str = "collection_groups";
const TAGS_RESOURCE: &str = "tags";

/// スコープが足りないときのツールのエラー（`BantoError::Forbidden`）。
const FORBIDDEN_MESSAGE: &str =
    "forbidden: このキーのスコープではこの操作はできません（設定の変更と収集の操作には config が要ります）。";

/// ツールの状態。REST と同じ ops（`crate::rest::api_router` が同じサービスから作る）。
#[derive(Clone)]
pub(crate) struct McpState {
    pub(crate) registry: RegistryOps,
    pub(crate) display_groups: DisplayGroupOps,
    pub(crate) thresholds: ThresholdOps,
    pub(crate) collect: CollectOps,
    pub(crate) audit: AuditLogService,
}

/// `POST /mcp` のルーター。`api_router` の層の外に merge する（モジュール doc）。
pub(crate) fn mcp_router(state: McpState, gate: ApiKeyGate) -> Router {
    Router::new()
        .route("/mcp", post(mcp_handler))
        .with_state(state)
        .layer(middleware::from_fn_with_state(gate, require_mcp_api_key))
}

async fn mcp_handler(
    State(state): State<McpState>,
    Extension(actor): Extension<Actor>,
    body: Bytes,
) -> Response {
    banto_mcp::handle_request(&TOOLS, &state, &actor, &body).await
}

/// ツール表（1 回だけ作る）。
static TOOLS: LazyLock<ToolRegistry<McpState, Actor>> = LazyLock::new(tool_registry);

/// 読み取りのツールを表に足す。処理 `$f` は主体を受け取らず、ここで
/// `can_read` を確かめてから呼ぶ（モジュール doc「スコープ」）。
macro_rules! read_tool {
    ($registry:expr, $name:literal, $resource:expr, $description:expr, $schema:expr, $f:path) => {
        $registry.tool(
            json!({ "name": $name, "description": $description, "inputSchema": $schema }),
            |s, a, args| {
                Box::pin(async move {
                    if let Some(denied) = check_read(s, a, $name, $resource).await {
                        return Ok(denied);
                    }
                    $f(s, args).await
                })
            },
        )
    };
}

/// 設定を変えるツール・収集の操作を表に足す。処理 `$f` は ops を
/// `Via::Mcp { tool: $name }` で呼ぶ（権限の確かめ・監査は ops の中）。
macro_rules! config_tool {
    ($registry:expr, $name:literal, $description:expr, $schema:expr, $f:path) => {
        $registry.tool(
            json!({ "name": $name, "description": $description, "inputSchema": $schema }),
            |s, a, args| {
                Box::pin(async move {
                    let via = Via::Mcp { tool: $name };
                    $f(s, a, &via, args).await
                })
            },
        )
    };
}

/// 読み取りの権限（`read`・`config`）。足りなければ `denied` を記録してツールの
/// エラーを返す。
async fn check_read(state: &McpState, actor: &Actor, tool: &str, resource: &str) -> Option<Value> {
    if actor.can_read() {
        return None;
    }
    actor
        .record_denied(&state.audit, &Via::Mcp { tool }, resource)
        .await;
    Some(tool_error(FORBIDDEN_MESSAGE))
}

// --- 入力のスキーマ ---------------------------------------------------------

fn no_args() -> Value {
    json!({ "type": "object", "properties": {}, "additionalProperties": false })
}

fn id_only(description: &str) -> Value {
    json!({
        "type": "object",
        "properties": { "id": { "type": "integer", "description": description } },
        "required": ["id"],
        "additionalProperties": false,
    })
}

fn delete_args(description: &str) -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": { "type": "integer", "description": description },
            "confirm": {
                "type": "boolean",
                "description": "不可逆操作の確認。true が必須。",
            },
        },
        "required": ["id", "confirm"],
        "additionalProperties": false,
    })
}

/// `properties` に `id` を足し、`required` の先頭に置く（更新のツール）。
fn with_id(mut schema: Value, description: &str) -> Value {
    schema["properties"]["id"] = json!({ "type": "integer", "description": description });
    let mut required = vec![json!("id")];
    if let Some(rest) = schema["required"].as_array() {
        required.extend(rest.iter().cloned());
    }
    schema["required"] = Value::Array(required);
    schema
}

/// 更新のツールの必須の項目（`id`・`tagId` を除く入力の全項目。スキーマの
/// `required` と [`require_update_fields`] の両方に使う）。
const UPDATE_CONNECTION_FIELDS: [&str; 6] = [
    "id",
    "name",
    "protocol",
    "enabled",
    "simulation",
    "settings",
];
const UPDATE_GROUP_FIELDS: [&str; 5] = ["id", "name", "plcConnectionId", "periodMs", "enabled"];
const UPDATE_TAG_FIELDS: [&str; 12] = [
    "id",
    "name",
    "collectionGroupId",
    "address",
    "dataType",
    "rawLo",
    "rawHi",
    "engLo",
    "engHi",
    "unit",
    "decimals",
    "enabled",
];
const UPDATE_DISPLAY_GROUP_FIELDS: [&str; 5] = ["id", "name", "kind", "attributes", "pens"];
const SET_THRESHOLD_FIELDS: [&str; 5] = [
    "tagId",
    "thresholdLl",
    "thresholdL",
    "thresholdH",
    "thresholdHh",
];

/// 更新のツールの入力に全項目があるか。足りなければ `missing_fields` のツールの
/// エラー（ops に触れない・監査しない）。スコープ不足の主体には確かめず素通りさせ、
/// ops が `denied` を記録して断る（項目の不足より先にスコープで止める）。
fn require_update_fields(actor: &Actor, args: &Value, required: &[&str]) -> Option<Value> {
    if actor.can_configure() {
        require_all_fields(args, required)
    } else {
        None
    }
}

/// [`with_id`] に加えて、`required` を `fields`（更新のツールの全項目）にする。
fn with_id_all_required(schema: Value, description: &str, fields: &[&str]) -> Value {
    let mut schema = with_id(schema, description);
    schema["required"] = json!(fields);
    schema
}

/// 接続先（REST の `PlcConnectionPayload`）。
fn connection_schema(update: bool) -> Value {
    let simulation = if update {
        "true でシミュレータ接続にする。"
    } else {
        "true でシミュレータ接続にする。省略は false。"
    };
    let enabled = if update {
        "有効にするか。"
    } else {
        "有効にするか。省略は true。"
    };
    let schema = json!({
        "type": "object",
        "properties": {
            "name": { "type": "string", "description": "接続先の名前（一意）。" },
            "protocol": {
                "type": "string",
                "description": "list_drivers で得られるドライバの id（slmp・modbus-tcp など）。",
            },
            "enabled": { "type": "boolean", "description": enabled },
            "simulation": { "type": "boolean", "description": simulation },
            "settings": {
                "type": "object",
                "description": "接続の設定（REST の /api/plc-connections と同じ形。キーはドライバのスキーマ、list_drivers を参照）。丸ごと置き換え。",
            },
        },
        "required": ["name", "protocol"],
        "additionalProperties": false,
    });
    if update {
        with_id_all_required(schema, "更新する接続先の id。", &UPDATE_CONNECTION_FIELDS)
    } else {
        schema
    }
}

/// 収集グループ（REST の `CollectionGroupPayload`）。
fn group_schema(update: bool) -> Value {
    let enabled = if update {
        "有効にするか。"
    } else {
        "有効にするか。省略は true。"
    };
    let schema = json!({
        "type": "object",
        "properties": {
            "name": { "type": "string", "description": "収集グループの名前。" },
            "plcConnectionId": { "type": "integer", "description": "接続先の id。" },
            "periodMs": { "type": "integer", "description": "収集の周期（ミリ秒）。" },
            "enabled": { "type": "boolean", "description": enabled },
        },
        "required": ["name", "plcConnectionId", "periodMs"],
        "additionalProperties": false,
    });
    if update {
        with_id_all_required(schema, "更新する収集グループの id。", &UPDATE_GROUP_FIELDS)
    } else {
        schema
    }
}

/// 記録項目（REST の `TagPayload`。しきい値は set_threshold で設定する）。
fn tag_schema(update: bool) -> Value {
    let enabled = if update {
        "有効にするか。"
    } else {
        "有効にするか。省略は true。"
    };
    let decimals = if update {
        "小数の桁数。"
    } else {
        "小数の桁数。省略は既定値。"
    };
    let mut schema = json!({
        "type": "object",
        "properties": {
            "name": { "type": "string", "description": "記録項目の名前。" },
            "collectionGroupId": { "type": "integer", "description": "収集グループの id。" },
            "address": { "type": "string", "description": "機器のアドレス（D100 など）。" },
            "dataType": { "type": "string", "description": "データ型（int16・float32 など）。" },
            "rawLo": { "type": ["number", "null"], "description": "スケーリングの生の値の下限。" },
            "rawHi": { "type": ["number", "null"], "description": "スケーリングの生の値の上限。" },
            "engLo": { "type": ["number", "null"], "description": "スケーリングの工学値の下限。" },
            "engHi": { "type": ["number", "null"], "description": "スケーリングの工学値の上限。" },
            "unit": { "type": ["string", "null"], "description": "単位。" },
            "decimals": { "type": "integer", "description": decimals },
            "enabled": { "type": "boolean", "description": enabled },
        },
        "required": ["name", "collectionGroupId", "address", "dataType"],
        "additionalProperties": false,
    });
    if update {
        schema["properties"]["expectedRevision"] = json!({
            "type": "integer",
            "description": "楽観ロック: get_tag で読んだ revision。省略すると版を確かめない（これだけ省略できる）。",
        });
        with_id_all_required(schema, "更新する記録項目の id。", &UPDATE_TAG_FIELDS)
    } else {
        schema
    }
}

/// 表示グループ（`DisplayGroupPayload`）。
fn display_group_schema(update: bool) -> Value {
    let mut schema = json!({
        "type": "object",
        "properties": {
            "name": { "type": "string", "description": "表示グループの名前。" },
            "kind": {
                "type": "string",
                "enum": ["trend", "digital", "bar", "gauge"],
                "description": "表示の種別。",
            },
            "attributes": {
                "type": "object",
                "description": "種別ごとの属性（trend の timeWindowSec など）。",
            },
            "pens": {
                "type": "array",
                "description": "ペン（表示する記録項目）の並び。各ペンは tagId か hubTag のどちらか 1 つを指定する。",
                "items": {
                    "type": "object",
                    "properties": {
                        "tagId": {
                            "type": "integer",
                            "description": "登録済みの記録項目の id。hubTag と同時には指定できない。",
                        },
                        "hubTag": {
                            "type": "object",
                            "description": "Hub のカタログのタグ（tagId の代わり）。登録済みの外部名なら既存の記録項目を参照し、未登録なら hubTagGroup の収集グループに記録項目として登録する（表示グループの保存と全部か無しか）。",
                            "properties": {
                                "externalName": { "type": "string", "description": "Hub のタグの外部名（同一性の鍵）。" },
                                "name": { "type": "string", "description": "記録項目の名前（Hub のタグ名）。" },
                                "dataType": { "type": "string", "description": "データ型（数値と bit だけ登録できる）。" },
                                "unit": { "type": ["string", "null"], "description": "単位。" },
                                "decimals": { "type": ["integer", "null"], "description": "小数桁（0〜6 に丸める）。" },
                            },
                            "required": ["externalName", "name", "dataType"],
                            "additionalProperties": false,
                        },
                        "colorSlot": {
                            "type": ["integer", "null"],
                            "description": "系列色の枠（1〜16）。省略は既定。",
                        },
                    },
                    "additionalProperties": false,
                },
            },
            "hubTagGroup": {
                "type": "object",
                "description": "未登録の hubTag を登録する先。Hub 接続の既存の収集グループ（collectionGroupId）か新しいグループ（newGroup）のどちらか 1 つ。未登録の hubTag が無ければ使わない。",
                "properties": {
                    "collectionGroupId": { "type": "integer", "description": "Hub 接続の下の既存の収集グループの id。" },
                    "newGroup": {
                        "type": "object",
                        "description": "新しく作る収集グループ（Hub 接続が無ければ暗黙に作る）。",
                        "properties": {
                            "name": { "type": "string", "description": "収集グループの名前。" },
                            "periodMs": { "type": "integer", "description": "収集の周期（ミリ秒）。" },
                        },
                        "required": ["name", "periodMs"],
                        "additionalProperties": false,
                    },
                },
                "additionalProperties": false,
            },
        },
        "required": ["name", "kind"],
        "additionalProperties": false,
    });
    if update {
        schema["properties"]["expectedRevision"] = json!({
            "type": "integer",
            "description": "楽観ロック: get_display_group で読んだ revision。省略すると版を確かめない（これだけ省略できる）。",
        });
        with_id_all_required(
            schema,
            "更新する表示グループの id。",
            &UPDATE_DISPLAY_GROUP_FIELDS,
        )
    } else {
        schema["properties"]["sortOrder"] = json!({
            "type": "integer",
            "description": "並び順。省略は末尾。",
        });
        schema
    }
}

fn threshold_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "tagId": { "type": "integer", "description": "記録項目の id。" },
            "thresholdLl": { "type": ["number", "null"], "description": "下下限。null は設定なし。省略できない。" },
            "thresholdL": { "type": ["number", "null"], "description": "下限。null は設定なし。省略できない。" },
            "thresholdH": { "type": ["number", "null"], "description": "上限。null は設定なし。省略できない。" },
            "thresholdHh": { "type": ["number", "null"], "description": "上上限。null は設定なし。省略できない。" },
            "expectedRevision": {
                "type": "integer",
                "description": "楽観ロック: get_threshold で読んだ revision。省略すると版を確かめない。",
            },
        },
        "required": SET_THRESHOLD_FIELDS,
        "additionalProperties": false,
    })
}

// --- ツール表 ---------------------------------------------------------------

/// ツール表。並びが `tools/list` の順になる
/// （`tests/snapshots/mcp_tools_list.json` で固定）。
fn tool_registry() -> ToolRegistry<McpState, Actor> {
    let r = ToolRegistry::new(SERVER_NAME, env!("CARGO_PKG_VERSION"));

    // 読み取り（read・config）。
    let r = read_tool!(
        r,
        "get_status",
        COLLECT_AUDIT_RESOURCE,
        "収集の状態を返す（REST の GET /api/collect と同じ）。",
        no_args(),
        tool_get_status
    );
    let r = read_tool!(
        r,
        "list_connections",
        PLC_CONNECTIONS_RESOURCE,
        "接続先の一覧を返す（秘密の値は返さない）。",
        no_args(),
        tool_list_connections
    );
    let r = read_tool!(
        r,
        "get_connection",
        PLC_CONNECTIONS_RESOURCE,
        "接続先を 1 件返す（秘密の値は返さない）。",
        id_only("接続先の id。"),
        tool_get_connection
    );
    let r = read_tool!(
        r,
        "list_drivers",
        PLC_CONNECTIONS_RESOURCE,
        "使えるドライバ（接続先の protocol）と設定のスキーマを返す。",
        no_args(),
        tool_list_drivers
    );
    let r = read_tool!(
        r,
        "list_groups",
        COLLECTION_GROUPS_RESOURCE,
        "収集グループの一覧を返す。",
        no_args(),
        tool_list_groups
    );
    let r = read_tool!(
        r,
        "get_group",
        COLLECTION_GROUPS_RESOURCE,
        "収集グループを 1 件返す。",
        id_only("収集グループの id。"),
        tool_get_group
    );
    let r = read_tool!(
        r,
        "list_tags",
        TAGS_RESOURCE,
        "記録項目の一覧を返す。",
        no_args(),
        tool_list_tags
    );
    let r = read_tool!(
        r,
        "get_tag",
        TAGS_RESOURCE,
        "記録項目を 1 件返す（revision は update_tag の expectedRevision に使う）。",
        id_only("記録項目の id。"),
        tool_get_tag
    );
    let r = read_tool!(
        r,
        "list_display_groups",
        DISPLAY_GROUPS_RESOURCE,
        "表示グループの一覧を返す（並び順）。",
        no_args(),
        tool_list_display_groups
    );
    let r = read_tool!(
        r,
        "get_display_group",
        DISPLAY_GROUPS_RESOURCE,
        "表示グループを 1 件返す。",
        id_only("表示グループの id。"),
        tool_get_display_group
    );
    let r = read_tool!(
        r,
        "list_thresholds",
        TAG_THRESHOLDS_RESOURCE,
        "しきい値の設定がある記録項目のしきい値を返す。",
        no_args(),
        tool_list_thresholds
    );
    let r = read_tool!(
        r,
        "get_threshold",
        TAG_THRESHOLDS_RESOURCE,
        "記録項目 1 つのしきい値を返す（設定が無ければ 4 つとも null・revision 0）。",
        json!({
            "type": "object",
            "properties": { "tagId": { "type": "integer", "description": "記録項目の id。" } },
            "required": ["tagId"],
            "additionalProperties": false,
        }),
        tool_get_threshold
    );
    let r = read_tool!(
        r,
        "read_values",
        COLLECT_AUDIT_RESOURCE,
        "記録項目ごとの現在値を返す（REST の GET /api/collect/values と同じ。品質と時刻つき）。",
        no_args(),
        tool_read_values
    );

    // 設定の変更（config）。収集が読む設定（接続先・収集グループ・記録項目・
    // しきい値）の結果に restartRequired: true。表示グループには付けない（Hub の
    // タグを記録項目として登録したときだけ付ける）。
    let r = config_tool!(
        r,
        "create_connection",
        "接続先を作る（config スコープ）。収集へは restart_collection で反映する。",
        connection_schema(false),
        tool_create_connection
    );
    let r = config_tool!(
        r,
        "update_connection",
        "接続先を更新する（config スコープ。全項目の置き換えで、省略できない）。収集へは restart_collection で反映する。",
        connection_schema(true),
        tool_update_connection
    );
    let r = config_tool!(
        r,
        "delete_connection",
        "接続先を削除する（config スコープ）。不可逆のため confirm:true が必須。収集へは restart_collection で反映する。",
        delete_args("削除する接続先の id。"),
        tool_delete_connection
    );
    let r = config_tool!(
        r,
        "create_group",
        "収集グループを作る（config スコープ）。収集へは restart_collection で反映する。",
        group_schema(false),
        tool_create_group
    );
    let r = config_tool!(
        r,
        "update_group",
        "収集グループを更新する（config スコープ。全項目の置き換えで、省略できない）。収集へは restart_collection で反映する。",
        group_schema(true),
        tool_update_group
    );
    let r = config_tool!(
        r,
        "delete_group",
        "収集グループを削除する（config スコープ）。不可逆のため confirm:true が必須。収集へは restart_collection で反映する。",
        delete_args("削除する収集グループの id。"),
        tool_delete_group
    );
    let r = config_tool!(
        r,
        "create_tag",
        "記録項目を作る（config スコープ。しきい値は set_threshold で設定する）。収集へは restart_collection で反映する。",
        tag_schema(false),
        tool_create_tag
    );
    let r = config_tool!(
        r,
        "update_tag",
        "記録項目を更新する（config スコープ。全項目の置き換えで、expectedRevision 以外は省略できない。get_tag の値を全項目送り返す）。収集へは restart_collection で反映する。",
        tag_schema(true),
        tool_update_tag
    );
    let r = config_tool!(
        r,
        "delete_tag",
        "記録項目を削除する（config スコープ。表示グループのペンに使われていれば断る）。不可逆のため confirm:true が必須。収集へは restart_collection で反映する。",
        delete_args("削除する記録項目の id。"),
        tool_delete_tag
    );
    let r = config_tool!(
        r,
        "create_display_group",
        "表示グループを作る（config スコープ）。ペンに Hub のカタログのタグ（hubTag）を指定すると、未登録のものは hubTagGroup に記録項目として同じ保存の中で登録する（そのときは結果に registeredTags と restartRequired: true が付き、収集へは restart_collection で反映する）。",
        display_group_schema(false),
        tool_create_display_group
    );
    let r = config_tool!(
        r,
        "update_display_group",
        "表示グループを更新する（config スコープ。全項目の置き換えで、expectedRevision 以外は省略できない。並び順は reorder_display_groups で変える。表示のための設定で、収集の再起動は要らない。ただし hubTag のペンで記録項目を登録したときは create_display_group と同じく registeredTags と restartRequired: true が付く）。",
        display_group_schema(true),
        tool_update_display_group
    );
    let r = config_tool!(
        r,
        "delete_display_group",
        "表示グループを削除する（config スコープ）。不可逆のため confirm:true が必須。",
        delete_args("削除する表示グループの id。"),
        tool_delete_display_group
    );
    let r = config_tool!(
        r,
        "reorder_display_groups",
        "表示グループを並べ替える（config スコープ）。ids はすべての表示グループの id を新しい順に並べたもの。",
        json!({
            "type": "object",
            "properties": {
                "ids": {
                    "type": "array",
                    "items": { "type": "integer" },
                    "description": "すべての表示グループの id（新しい順）。",
                },
            },
            "required": ["ids"],
            "additionalProperties": false,
        }),
        tool_reorder_display_groups
    );
    let r = config_tool!(
        r,
        "set_threshold",
        "記録項目 1 つのしきい値を全項目置き換える（config スコープ。4 つのしきい値は省略できず、設定しない欄は null を送る）。収集へは restart_collection で反映する。",
        threshold_schema(),
        tool_set_threshold
    );

    // 収集の操作（config）。restartRequired は付けない。
    let r = config_tool!(
        r,
        "start_collection",
        "収集を開始する（config スコープ）。",
        no_args(),
        tool_start_collection
    );
    let r = config_tool!(
        r,
        "stop_collection",
        "収集を停止する（config スコープ）。",
        no_args(),
        tool_stop_collection
    );
    config_tool!(
        r,
        "restart_collection",
        "収集を再起動する（config スコープ）。接続先・収集グループ・記録項目の変更を収集へ反映する唯一の口。",
        no_args(),
        tool_restart_collection
    )
}

// --- 補助 -------------------------------------------------------------------

/// ops・サービスの失敗をツールのエラーにする。スコープ不足（`Forbidden`）だけ
/// 案内の文にする。
fn op_error(err: &BantoError) -> Value {
    match err {
        BantoError::Forbidden => tool_error(FORBIDDEN_MESSAGE),
        other => banto_error_tool_error(other),
    }
}

/// 結果を `tool_ok` にする（失敗は [`op_error`]）。
fn reply<T>(result: Result<T, BantoError>, wrap: impl FnOnce(T) -> Value) -> Value {
    match result {
        Ok(value) => tool_ok(wrap(value)),
        Err(err) => op_error(&err),
    }
}

/// 表示グループの作成・更新の結果: Hub のタグを記録項目として登録したときだけ
/// `restartRequired: true` を足す（モジュール doc「結果の形」）。
fn display_group_saved(payload: Value, saved: &DisplayGroupSaveResponse) -> Value {
    if saved.registered_tags.is_empty() {
        payload
    } else {
        changed(payload)
    }
}

/// 収集が読む設定を変えるツールの結果（`restartRequired: true` を足す）。
fn changed(mut payload: Value) -> Value {
    payload["restartRequired"] = json!(true);
    payload
}

fn arguments(args: Option<Value>) -> Result<Value, RpcError> {
    args.ok_or_else(|| RpcError::invalid_params("arguments is required"))
}

fn int_arg(args: &Value, key: &str) -> Result<i64, RpcError> {
    args.get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| RpcError::invalid_params(format!("arguments.{key} (integer) is required")))
}

fn id_arg(args: Option<Value>) -> Result<i64, RpcError> {
    int_arg(&arguments(args)?, "id")
}

/// 削除の引数: `id` と `confirm: true`。`confirm` が無ければツールのエラー
/// （ops に触れない・監査しない。HUB と同じ）。
fn delete_target(args: Option<Value>) -> Result<Result<i64, Value>, RpcError> {
    let args = arguments(args)?;
    let id = int_arg(&args, "id")?;
    if args.get("confirm").and_then(Value::as_bool) == Some(true) {
        Ok(Ok(id))
    } else {
        Ok(Err(tool_error(
            "confirm_required: 削除は不可逆操作です。arguments.confirm:true を指定してください。",
        )))
    }
}

/// `arguments` から `key`（`id`・`tagId`）を外した残りを payload として読む。
fn payload_without<T: DeserializeOwned>(
    mut args: Value,
    key: &str,
    what: &str,
) -> Result<T, RpcError> {
    if let Some(object) = args.as_object_mut() {
        object.remove(key);
    }
    serde_json::from_value(args)
        .map_err(|err| RpcError::invalid_params(format!("{what}の入力が不正です: {err}")))
}

fn payload<T: DeserializeOwned>(args: Option<Value>, what: &str) -> Result<T, RpcError> {
    serde_json::from_value(arguments(args)?)
        .map_err(|err| RpcError::invalid_params(format!("{what}の入力が不正です: {err}")))
}

// --- 読み取りのツール（主体を受け取らない。`read_tool!` だけから呼ぶ） --------

async fn tool_get_status(state: &McpState, _args: Option<Value>) -> Result<Value, RpcError> {
    Ok(tool_ok(json!(state.collect.collect.state_view())))
}

async fn tool_read_values(state: &McpState, _args: Option<Value>) -> Result<Value, RpcError> {
    Ok(tool_ok(json!(state.collect.collect.values_response())))
}

async fn tool_list_connections(state: &McpState, _args: Option<Value>) -> Result<Value, RpcError> {
    Ok(reply(
        state.registry.list_connections().await,
        |rows| json!({ "connections": rows }),
    ))
}

async fn tool_get_connection(state: &McpState, args: Option<Value>) -> Result<Value, RpcError> {
    let id = id_arg(args)?;
    Ok(reply(
        state.registry.get_connection(id).await,
        |row| json!({ "connection": row }),
    ))
}

async fn tool_list_drivers(state: &McpState, _args: Option<Value>) -> Result<Value, RpcError> {
    Ok(tool_ok(json!({
        "drivers": banto_tags::driver_catalog::driver_list(&state.registry.drivers),
    })))
}

async fn tool_list_groups(state: &McpState, _args: Option<Value>) -> Result<Value, RpcError> {
    let result = state
        .registry
        .collection_groups
        .list(ListParams::default())
        .await;
    Ok(reply(result, |page| json!({ "groups": page.rows })))
}

async fn tool_get_group(state: &McpState, args: Option<Value>) -> Result<Value, RpcError> {
    let id = id_arg(args)?;
    Ok(reply(
        state.registry.collection_groups.get(id).await,
        |group| json!({ "group": group }),
    ))
}

async fn tool_list_tags(state: &McpState, _args: Option<Value>) -> Result<Value, RpcError> {
    let result = state.registry.tags.list(ListParams::default()).await;
    Ok(reply(result, |page| json!({ "tags": page.rows })))
}

async fn tool_get_tag(state: &McpState, args: Option<Value>) -> Result<Value, RpcError> {
    let id = id_arg(args)?;
    Ok(reply(
        state.registry.tags.get(id).await,
        |tag| json!({ "tag": tag }),
    ))
}

async fn tool_list_display_groups(
    state: &McpState,
    _args: Option<Value>,
) -> Result<Value, RpcError> {
    Ok(reply(
        state.display_groups.display_groups.list().await,
        |groups| json!({ "displayGroups": groups }),
    ))
}

async fn tool_get_display_group(state: &McpState, args: Option<Value>) -> Result<Value, RpcError> {
    let id = id_arg(args)?;
    Ok(reply(
        state.display_groups.display_groups.get(id).await,
        |group| json!({ "displayGroup": group }),
    ))
}

async fn tool_list_thresholds(state: &McpState, _args: Option<Value>) -> Result<Value, RpcError> {
    Ok(reply(
        state.thresholds.tag_thresholds.list().await,
        |rows| json!({ "thresholds": rows }),
    ))
}

async fn tool_get_threshold(state: &McpState, args: Option<Value>) -> Result<Value, RpcError> {
    let tag_id = int_arg(&arguments(args)?, "tagId")?;
    Ok(reply(
        state.thresholds.tag_thresholds.get(tag_id).await,
        |row| json!({ "threshold": row }),
    ))
}

// --- 設定を変えるツール（ops を `Via::Mcp` で呼ぶ） ---------------------------

async fn tool_create_connection(
    state: &McpState,
    actor: &Actor,
    via: &Via<'_>,
    args: Option<Value>,
) -> Result<Value, RpcError> {
    let input: PlcConnectionPayload = payload(args, "接続先")?;
    Ok(reply(
        state.registry.create_connection(actor, via, input).await,
        |created| changed(json!({ "created": created })),
    ))
}

async fn tool_update_connection(
    state: &McpState,
    actor: &Actor,
    via: &Via<'_>,
    args: Option<Value>,
) -> Result<Value, RpcError> {
    let args = arguments(args)?;
    if let Some(err) = require_update_fields(actor, &args, &UPDATE_CONNECTION_FIELDS) {
        return Ok(err);
    }
    let id = int_arg(&args, "id")?;
    // `PlcConnectionPayload` は `deny_unknown_fields` なので `id` を外してから読む。
    let input: PlcConnectionPayload = payload_without(args, "id", "接続先")?;
    Ok(reply(
        state
            .registry
            .update_connection(actor, via, id, input)
            .await,
        |updated| changed(json!({ "updated": updated })),
    ))
}

async fn tool_delete_connection(
    state: &McpState,
    actor: &Actor,
    via: &Via<'_>,
    args: Option<Value>,
) -> Result<Value, RpcError> {
    let id = match delete_target(args)? {
        Ok(id) => id,
        Err(refused) => return Ok(refused),
    };
    Ok(reply(
        state.registry.delete_connection(actor, via, id).await,
        |()| changed(json!({ "deleted": true, "id": id })),
    ))
}

async fn tool_create_group(
    state: &McpState,
    actor: &Actor,
    via: &Via<'_>,
    args: Option<Value>,
) -> Result<Value, RpcError> {
    let input: CollectionGroupPayload = payload(args, "収集グループ")?;
    Ok(reply(
        state.registry.create_group(actor, via, input).await,
        |created| changed(json!({ "created": created })),
    ))
}

async fn tool_update_group(
    state: &McpState,
    actor: &Actor,
    via: &Via<'_>,
    args: Option<Value>,
) -> Result<Value, RpcError> {
    let args = arguments(args)?;
    if let Some(err) = require_update_fields(actor, &args, &UPDATE_GROUP_FIELDS) {
        return Ok(err);
    }
    let id = int_arg(&args, "id")?;
    let input: CollectionGroupPayload = payload_without(args, "id", "収集グループ")?;
    Ok(reply(
        state.registry.update_group(actor, via, id, input).await,
        |updated| changed(json!({ "updated": updated })),
    ))
}

async fn tool_delete_group(
    state: &McpState,
    actor: &Actor,
    via: &Via<'_>,
    args: Option<Value>,
) -> Result<Value, RpcError> {
    let id = match delete_target(args)? {
        Ok(id) => id,
        Err(refused) => return Ok(refused),
    };
    Ok(reply(
        state.registry.delete_group(actor, via, id).await,
        |()| changed(json!({ "deleted": true, "id": id })),
    ))
}

async fn tool_create_tag(
    state: &McpState,
    actor: &Actor,
    via: &Via<'_>,
    args: Option<Value>,
) -> Result<Value, RpcError> {
    let input: TagPayload = payload(args, "記録項目")?;
    Ok(reply(
        state.registry.create_tag(actor, via, input).await,
        |created| changed(json!({ "created": created })),
    ))
}

async fn tool_update_tag(
    state: &McpState,
    actor: &Actor,
    via: &Via<'_>,
    args: Option<Value>,
) -> Result<Value, RpcError> {
    let args = arguments(args)?;
    if let Some(err) = require_update_fields(actor, &args, &UPDATE_TAG_FIELDS) {
        return Ok(err);
    }
    let id = int_arg(&args, "id")?;
    let input: TagPayload = payload_without(args, "id", "記録項目")?;
    Ok(reply(
        state.registry.update_tag(actor, via, id, input).await,
        |updated| changed(json!({ "updated": updated })),
    ))
}

async fn tool_delete_tag(
    state: &McpState,
    actor: &Actor,
    via: &Via<'_>,
    args: Option<Value>,
) -> Result<Value, RpcError> {
    let id = match delete_target(args)? {
        Ok(id) => id,
        Err(refused) => return Ok(refused),
    };
    Ok(reply(
        state.registry.delete_tag(actor, via, id).await,
        |()| changed(json!({ "deleted": true, "id": id })),
    ))
}

async fn tool_create_display_group(
    state: &McpState,
    actor: &Actor,
    via: &Via<'_>,
    args: Option<Value>,
) -> Result<Value, RpcError> {
    let input: DisplayGroupPayload = payload(args, "表示グループ")?;
    Ok(reply(
        state.display_groups.create(actor, via, &input).await,
        |created| display_group_saved(json!({ "created": created }), &created),
    ))
}

async fn tool_update_display_group(
    state: &McpState,
    actor: &Actor,
    via: &Via<'_>,
    args: Option<Value>,
) -> Result<Value, RpcError> {
    let args = arguments(args)?;
    if let Some(err) = require_update_fields(actor, &args, &UPDATE_DISPLAY_GROUP_FIELDS) {
        return Ok(err);
    }
    let id = int_arg(&args, "id")?;
    let input: DisplayGroupPayload = payload_without(args, "id", "表示グループ")?;
    Ok(reply(
        state.display_groups.update(actor, via, id, &input).await,
        |updated| display_group_saved(json!({ "updated": updated }), &updated),
    ))
}

async fn tool_delete_display_group(
    state: &McpState,
    actor: &Actor,
    via: &Via<'_>,
    args: Option<Value>,
) -> Result<Value, RpcError> {
    let id = match delete_target(args)? {
        Ok(id) => id,
        Err(refused) => return Ok(refused),
    };
    Ok(reply(
        state.display_groups.delete(actor, via, id).await,
        |()| json!({ "deleted": true, "id": id }),
    ))
}

async fn tool_reorder_display_groups(
    state: &McpState,
    actor: &Actor,
    via: &Via<'_>,
    args: Option<Value>,
) -> Result<Value, RpcError> {
    let input: crate::display_groups::ReorderPayload = payload(args, "並べ替え")?;
    Ok(reply(
        state.display_groups.reorder(actor, via, &input.ids).await,
        |groups| json!({ "displayGroups": groups }),
    ))
}

async fn tool_set_threshold(
    state: &McpState,
    actor: &Actor,
    via: &Via<'_>,
    args: Option<Value>,
) -> Result<Value, RpcError> {
    let args = arguments(args)?;
    if let Some(err) = require_update_fields(actor, &args, &SET_THRESHOLD_FIELDS) {
        return Ok(err);
    }
    let tag_id = int_arg(&args, "tagId")?;
    let input: TagThresholdsPayload = payload_without(args, "tagId", "しきい値")?;
    Ok(reply(
        state.thresholds.update(actor, via, tag_id, &input).await,
        |updated| changed(json!({ "updated": updated })),
    ))
}

async fn run_collect(
    state: &McpState,
    actor: &Actor,
    via: &Via<'_>,
    operation: CollectOperation,
) -> Result<Value, RpcError> {
    Ok(reply(
        state.collect.run(actor, via, operation).await,
        |outcome| json!(outcome),
    ))
}

async fn tool_start_collection(
    state: &McpState,
    actor: &Actor,
    via: &Via<'_>,
    _args: Option<Value>,
) -> Result<Value, RpcError> {
    run_collect(state, actor, via, CollectOperation::Start).await
}

async fn tool_stop_collection(
    state: &McpState,
    actor: &Actor,
    via: &Via<'_>,
    _args: Option<Value>,
) -> Result<Value, RpcError> {
    run_collect(state, actor, via, CollectOperation::Stop).await
}

async fn tool_restart_collection(
    state: &McpState,
    actor: &Actor,
    via: &Via<'_>,
    _args: Option<Value>,
) -> Result<Value, RpcError> {
    run_collect(state, actor, via, CollectOperation::Restart).await
}
