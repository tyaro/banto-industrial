//! OpenAPI 自動生成（`ApiDoc`・`GET /api/v1/openapi.json`・Swagger UI の `openapi_router`）。
//!
//! `rest.rs` から中身を変えずに移したもの（docs/banto-hub-driver-refactor-plan.md §8.1、
//! P0-1 の手順 8）。`ApiDoc` の `paths(...)`・`schemas(...)` が参照するハンドラ・応答型と、
//! utoipa が生成する `__path_<fn>` は親の `rest.rs` が `rest/{tag_space,admin_status,write}.rs`
//! から取り込んでおり、ここでは `use super::{...}` で名前解決する（derive の本文は無変更）。

use super::{
    __path_v1_events, __path_v1_status, __path_v1_tags, __path_v1_value_read_now,
    __path_v1_value_single, __path_v1_values, __path_v1_write_value, __path_v1_write_values_batch,
    get, BatchWriteEntryRequest, BatchWriteEntryResponse, BatchWriteValueRequest,
    BatchWriteValuesResponse, CatalogResponse, CatalogTagEntry, Config, ConnectionStatusEntry,
    EventEntry, EventsResponse, GrpcStatusEntry, Json, MqttStatusEntry, OpenApi, ReadNowResponse,
    Router, SingleValueResponse, State, StatusResponse, SwaggerUi, TagEntry, ValueEntry,
    ValuesResponse, WriteValueRequest, WriteValueResponse,
};
// R2a-2c（docs/banto-hub-ingest-design.md §6.2）: 取り込み口の REST の送信口。
use crate::ingest::rest::{__path_v1_ingest_values, IngestRejectedItem, IngestValuesResponse};
use crate::ingest::validate::IngestValuesMessage;
use crate::ingest::IngestItem;

// --- OpenAPI 自動生成（設計 §5.1・§10-6、2026-08-04 決定） ------------------
//
// catalog（`/api/v1/tags`）はクライアントとの互換性契約（設計 §4.1）なので、
// コードとスキーマを単一ソース化する目的で utoipa を採用した。
// `ApiDoc::openapi()` は上の各ハンドラの `#[utoipa::path]` とこのファイルの
// 各応答型の `ToSchema` から機械的に構築される - ハンドラのシグネチャや
// 応答型を変えれば `/api/v1/openapi.json` も自動で追従する。

#[derive(OpenApi)]
#[openapi(
    info(
        title = "banto-hub タグ空間 API",
        version = "v1",
        description = "banto-hub の /api/v1/* API（docs/tag-server-design.md §5.1）。catalog（/api/v1/tags）はクライアントとの互換性契約である（§4.1）: 外部名・安定 ID・revision を用いたバインディングを前提に、フィールド名や JSON 形はこのスキーマとコードが常に一致するよう utoipa で自動生成している。書き込み（POST /api/v1/values/{tag}、T2-4・§6）は writable タグのみ・write:{tag} スコープの API キー限定。取り込み（POST /api/v1/ingest/{id}/values、docs/banto-hub-ingest-design.md §6.2）は push 接続へ値を送る口で、ingest:#{id} スコープの API キー限定。"
    ),
    paths(
        v1_tags,
        v1_values,
        v1_value_single,
        v1_value_read_now,
        v1_write_value,
        v1_write_values_batch,
        v1_status,
        v1_events,
        v1_ingest_values,
    ),
    components(schemas(
        TagEntry,
        CatalogResponse,
        CatalogTagEntry,
        ValueEntry,
        SingleValueResponse,
        ReadNowResponse,
        ValuesResponse,
        ConnectionStatusEntry,
        MqttStatusEntry,
        GrpcStatusEntry,
        StatusResponse,
        EventEntry,
        EventsResponse,
        WriteValueRequest,
        WriteValueResponse,
        BatchWriteEntryRequest,
        BatchWriteValueRequest,
        BatchWriteEntryResponse,
        BatchWriteValuesResponse,
        IngestValuesMessage,
        IngestItem,
        IngestValuesResponse,
        IngestRejectedItem,
    ))
)]
pub(super) struct ApiDoc;

/// `GET /api/v1/openapi.json` - **認証不要**（T0-2 実装指示: 「このエンド
/// ポイント自体は認証不要でよい」）。判断理由: OpenAPI スキーマはフィールド
/// 名・型・パスの一覧であって値そのものではなく、秘匿すべき情報を含まない。
/// 逆に、スキーマを見るために API キーを要求すると「まずキーを発行して
/// もらわないとどんな API か分からない」という鶏卵になり、外部連携の
/// 導入コストを不必要に上げる。
///
/// T16-2 第三スライス（docs/banto-hub-t16-design.md §5 既知の gap）: 応答
/// している Hub インスタンス自身の profile-id を`info.x-banto-hub-profile-id`
/// 拡張フィールドとして埋め込む。`crate::http_hub_health::HttpHubHealthProbe`
/// が「lock ファイルの置き場所」だけでなく「ワイヤ応答そのもの」から
/// profile-id を確認できるようにするための唯一の情報源 - utoipa の
/// `#[openapi(info(...))]` は任意拡張フィールドを直接生成できないため、
/// `ApiDoc::openapi()`が生成した`serde_json::Value`へ後から差し込む
/// （実装指示どおり、utoipa の`Extensions` API とは戦わない最小実装）。
async fn openapi_json(State(state): State<OpenApiState>) -> Json<serde_json::Value> {
    let mut value = serde_json::to_value(ApiDoc::openapi()).expect("openapi serialize");
    value["info"]["x-banto-hub-profile-id"] = serde_json::json!(state.profile_id);
    Json(value)
}

/// [`openapi_json`]専用の状態 - この Hub インスタンスが実際に使っている
/// profile-id（`crate::profile_paths`参照）だけを持つ。`HubRuntime::start`
/// （`crate::runtime`）が`HubConfig::profile_id`をそのまま渡す。
#[derive(Clone)]
struct OpenApiState {
    profile_id: String,
}

/// `GET /api/v1/openapi.json` と `GET /api/v1/swagger-ui/*` 専用ルーター -
/// 認証層を一切通さない（`openapi_json`関数の doc comment 参照）。
/// `profile_id`はこの Hub インスタンスが実際に使っている profile-id
/// （呼び出し元 `api_router_with_controller_mode`が
/// `HubConfig::profile_id`をそのまま渡す）。
///
/// ux-plan.md §5 バックログ「OpenAPI の Swagger UI 同梱」（2026-08-12
/// オーナー決定）: `/api/v1/swagger-ui` に Swagger UI 本体（HTML/JS/CSS）を
/// 同梱・マウントする。`openapi.json` 同様**認証不要** - Swagger UI 自体は
/// 静的アセット＋上の `openapi_json` が返すスキーマの閲覧・試打 UI に
/// すぎず、秘匿情報を含まないため（`openapi_json` の doc comment と同じ
/// 判断）。`SwaggerUi::url(...)`は使わない - それだと utoipa-swagger-ui が
/// **別の** `ApiDoc::openapi()`静的スナップショットを新規ルートとして
/// 生成してしまい、上の`openapi_json`（`x-banto-hub-profile-id`埋め込み
/// 済み・状態を持つ）と二重管理になる。代わりに`Config::new([...])`で
/// Swagger UI のフロントエンド JS に「スキーマは`/api/v1/openapi.json`を
/// 見に行け」と教えるだけにし、ルートは1本のまま
/// （`utoipa_swagger_ui::Config::new`は fetch 先 URL の設定のみで、それ自体は
/// ルートを追加しない）。アセットは`vendored` feature
/// （`utoipa-swagger-ui-vendored`、workspace Cargo.toml 参照）でバイナリに
/// 同梱済み - ビルド時・実行時ともに外部ネットワーク（CDN 含む）へ
/// 一切アクセスしない。
pub(super) fn openapi_router(profile_id: String) -> Router {
    Router::new()
        .route("/api/v1/openapi.json", get(openapi_json))
        .merge(SwaggerUi::new("/api/v1/swagger-ui").config(Config::new(["/api/v1/openapi.json"])))
        .with_state(OpenApiState { profile_id })
}
