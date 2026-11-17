//! `GET /api/openapi.json`（#1161 PR6、docs/chronogazer-agent-api-design.md §6）。
//!
//! ChronoGazer の REST のうち、**API キーを通すルートの集まり**
//! （`rest::api_key_routes` が合成する 4 つ: 収集 `collect_router`・表示グループ・
//! しきい値・レジストリ `tag_registry_router`）だけを utoipa で文書にする。管理系
//! （ユーザー・サーバー設定・HUB 連携 `/api/hub*`・`/api/tags/from-hub`・API キー・
//! 認証・バックアップ・監査ログ・UI 設定・SSE）はキーを通さない（キーで呼ぶと 403）
//! ので載せない。`/mcp` は JSON-RPC で OpenAPI の対象外。Swagger UI は載せない
//! （文書は機械が読むもの。HUB の `rest/openapi.rs` が手本で、違いは UI の有無と、
//! 載せる範囲が「キーを通すルート」に限ることの 2 点）。
//!
//! ## 層の外に置く理由
//!
//! 文書は秘密ではなく、**キーを発行する前に読めなければならない**（どんな API か
//! 分からないとキーのスコープも選べない）。そこで `/api/openapi.json` は `/mcp` と同じく
//! `api_router` の**認証・CSRF・`api_key_gate` の層の外**に merge する。層の内側に
//! 置くと、誤った `cg_…` の bearer が付いただけで 401 になり、キー無しのときは
//! セッションを要求されてしまう。外に置いたことは `rest/openapi_tests.rs` が
//! 「認証なしで 200」「誤った `cg_` の bearer 付きでも 200」で固定する。
//!
//! ## ずれ止め（設計書 §6 の最後）
//!
//! * `tests/snapshots/openapi.json` と `ApiDoc::openapi()` の完全一致
//!   （再生成は `UPDATE_OPENAPI_SNAPSHOT=1`）。
//! * `api_key_routes` の「METHOD path」一覧と `ApiDoc` の paths×methods の完全一致
//!   （キーを通すルートを足して文書を忘れると落ちる）。
//! * `api_router` 全体の「METHOD path」一覧のスナップショット
//!   （`tests/snapshots/routes.txt`、再生成は `UPDATE_ROUTES_SNAPSHOT=1`）。
//!
//! 型のスキーマは、ChronoGazer 内の型は直接 `derive(ToSchema)`、`banto-tags` の型
//! （`Tag`・`CollectionGroup`・`DriverListItem`）は同 crate の optional な `openapi`
//! feature（この crate の `Cargo.toml` で有効にする）で付ける。

use axum::routing::get;
use axum::{Json, Router};
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};

use super::{
    __path_collect_connections_handler, __path_collect_events_handler,
    __path_collect_history_handler, __path_collect_restart_handler, __path_collect_start_handler,
    __path_collect_status_handler, __path_collect_stop_handler, __path_collect_values_handler,
    __path_collection_groups_create, __path_collection_groups_delete, __path_collection_groups_get,
    __path_collection_groups_list, __path_collection_groups_update, __path_config_exclusions_list,
    __path_drivers_list, __path_plc_connections_create, __path_plc_connections_delete,
    __path_plc_connections_get, __path_plc_connections_list, __path_plc_connections_update,
    __path_tags_create, __path_tags_delete, __path_tags_get, __path_tags_list, __path_tags_update,
    CollectionGroupPayload, PlcConnectionPayload, PlcConnectionResponse, TagPayload,
};
use super::{display_groups, tag_thresholds};
use crate::collect::{
    CollectEventList, CollectEventRow, CollectHistory, CollectOutcome, CollectorState,
    CollectorStateView, ConnectionStatusView, ConnectionView, CurrentSampleView, ExclusionUnitView,
    ExclusionView, HistoryPoint, HistorySeries, QualityView, Readout, ValuesResponse,
};
use crate::display_groups::{
    DisplayAttributes, DisplayGroup, DisplayGroupPayload, DisplayKind, Pen, PenPayload,
    ReorderPayload,
};
use crate::tag_thresholds::{TagThresholds, TagThresholdsPayload};

/// `security(("api_key" = []))` が指す HTTP bearer の定義を足し（`components` は
/// `#[openapi]` の属性だけでは作れない）、スキーマの rustdoc 由来の説明を取り除く
/// （[`strip_schema_descriptions`]）。
struct SecurityAddon;

/// `components.schemas` の中の、文字列の `description` を取り除く（再帰）。
///
/// 型・フィールドの rustdoc は utoipa が `description` に写すが、このクレートの
/// rustdoc は**内部向けの設計メモ**（ファイルパス・レビューの経緯・リンク記法）で、
/// 文書の読み手（キーを持つ外部の人・エージェント）に見せるものではない。
/// 読み手向けの説明は `info.description` と各操作の `summary`・`description`
/// （ハンドラの `#[utoipa::path]`）に書く。プロパティ名が `description` の項目は、
/// 値がスキーマ（オブジェクト）なので文字列だけを消す条件で巻き込まない。
fn strip_schema_descriptions(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            if matches!(map.get("description"), Some(serde_json::Value::String(_))) {
                map.remove("description");
            }
            for child in map.values_mut() {
                strip_schema_descriptions(child);
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(strip_schema_descriptions),
        _ => {}
    }
}

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(components) = openapi.components.as_mut() {
            let mut schemas = serde_json::to_value(&components.schemas).expect("schemas serialize");
            strip_schema_descriptions(&mut schemas);
            components.schemas = serde_json::from_value(schemas).expect("schemas deserialize");
        }
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "api_key",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("cg_ で始まる API キー")
                    .description(Some(
                        "`Authorization: Bearer cg_…`。キーは ChronoGazer の API キー画面で発行する\
                         （平文は発行の 1 回だけ表示される）。",
                    ))
                    .build(),
            ),
        );
    }
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "ChronoGazer REST API（API キーで呼べる範囲）",
        version = "v1",
        description = "ChronoGazer の REST のうち、API キー（`Authorization: Bearer cg_…`）で呼べる操作だけを載せる（docs/chronogazer-agent-api-design.md §6）。\
管理系（ユーザー・サーバー設定・HUB 連携・API キーの管理・認証・バックアップ・監査ログ・UI 設定）はキーでは呼べないので載せない。\
\n\n**認証**: API キーで通った要求には CSRF のヘッダー（`X-Banto-Client`）は要らない。ログインのセッション（Cookie）で呼ぶときは、今までどおり `X-Banto-Client` が要る。\
\n\n**スコープ**: キーのスコープは `read`（読み取り）と `config`（`read` に加えて、作成・更新・削除と、収集の開始・停止・再起動）。書き込みに `read` のキーで来ると 403。失効・期限切れ・存在しないキーは 401（セッションには落ちない）。\
\n\n**エラー**: 失敗の本文は `kind` で種類を表す JSON（例: `{ \"kind\": \"validation\", \"field_errors\": [{ \"field\": ..., \"message\": ... }] }`、`{ \"kind\": \"not_found\", \"resource\": ..., \"id\": ... }`、`{ \"kind\": \"bad_request\", \"message\": ... }`）。入力の検証エラーは 422、対象が無ければ 404、楽観ロックの `expectedRevision` が古ければ 409。\
\n\n**更新**: PUT は全項目を送る（省略した項目は既定値になる）。直前に GET で読んだ `revision` を `expectedRevision` に入れると、他で先に更新されたときに 409 で気づける。\
\n\n**監査**: 成功した書き込みは監査ログに、キーの名前と接頭辞付きで残る。\
\n\n**収集への反映**: 接続・グループ・タグ・しきい値の変更は、`POST /api/collect/restart` までは収集に反映されない。"
    ),
    paths(
        collect_status_handler,
        collect_values_handler,
        collect_connections_handler,
        collect_events_handler,
        collect_history_handler,
        collect_start_handler,
        collect_stop_handler,
        collect_restart_handler,
        display_groups::list,
        display_groups::get_one,
        display_groups::create,
        display_groups::update,
        display_groups::delete,
        display_groups::reorder,
        tag_thresholds::list,
        tag_thresholds::get_one,
        tag_thresholds::update,
        plc_connections_list,
        plc_connections_get,
        plc_connections_create,
        plc_connections_update,
        plc_connections_delete,
        collection_groups_list,
        collection_groups_get,
        collection_groups_create,
        collection_groups_update,
        collection_groups_delete,
        tags_list,
        tags_get,
        tags_create,
        tags_update,
        tags_delete,
        config_exclusions_list,
        drivers_list,
    ),
    components(schemas(
        CollectorState,
        CollectorStateView,
        ExclusionUnitView,
        ExclusionView,
        CollectOutcome,
        QualityView,
        CurrentSampleView,
        ConnectionStatusView,
        ConnectionView,
        CollectEventRow,
        CollectEventList,
        ValuesResponse,
        Readout<CollectEventList>,
        Readout<CollectHistory>,
        HistoryPoint,
        HistorySeries,
        CollectHistory,
        PlcConnectionPayload,
        PlcConnectionResponse,
        CollectionGroupPayload,
        TagPayload,
        DisplayKind,
        DisplayAttributes,
        Pen,
        PenPayload,
        DisplayGroup,
        DisplayGroupPayload,
        ReorderPayload,
        TagThresholds,
        TagThresholdsPayload,
        banto_tags::Tag,
        banto_tags::CollectionGroup,
        banto_tags::driver_catalog::DriverListItem,
    )),
    modifiers(&SecurityAddon),
)]
pub(super) struct ApiDoc;

/// `GET /api/openapi.json`。**認証不要**（モジュール doc「層の外に置く理由」）。
async fn openapi_json() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

/// `GET /api/openapi.json` だけのルーター。`api_router` の**層の外**に merge する
/// （`/mcp` と同じ。`api_router` の末尾）。
pub(super) fn openapi_router() -> Router {
    Router::new().route("/api/openapi.json", get(openapi_json))
}
