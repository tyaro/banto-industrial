//! `/api/v1/*` のタグ空間 API（設計 §5.1）: catalog・現在値・単一値・read-now・
//! status の GET 系と、`/api/v1/*` 認証（API キー + セッション bearer 併用、
//! 設計 §5.6・T0-2）のミドルウェア・`tag_space_router`。
//!
//! `rest.rs` から中身を変えずに移したもの（docs/banto-hub-driver-refactor-plan.md §8.1、
//! P0-1 の手順 7）。OpenAPI の `ApiDoc` と `openapi_router` は親の `rest.rs` に残り、
//! ハンドラと応答型は `use` で取り込んで `#[utoipa::path]` の名前解決を保つ。
//! 外から使われるパス（`crate::rest::X`）は親の re-export で保つ。

use super::{
    api_key_rejection_response, api_key_verdict, bearer_token, broadcast,
    connections_with_collected_groups, effective_simulation_for_tag, extract_ws_protocol_token,
    forbidden_response, get, json, middleware, post, session_token_cannot_write_response,
    unauthorized_response, v1_events, v1_write_value, v1_write_values_batch, value_source_for_tag,
    ApiError, ApiKeyContext, ApiKeysService, ApplyReport, Arc, AsyncMutex, AuditEntry,
    AuditLogService, AuthState, BantoError, BrokerConnectionStatus, CollectionController,
    CollectionGroupService, CollectionState, CollectionStatus, CollectorManager, ComputedEngine,
    ConnectionStatus, Db, Deserialize, Extension, HubSettingsExt, IntoResponse, Json, ListParams,
    Method, MqttPublisher, Path, Query, Response, Router, RunMode, Serialize, ServerEvent,
    SettingsService, SinkStatusSnapshot, SinkStatusStore, State, StatusCode, SystemInfoSampler,
    SystemInfoSnapshot, TagEntry, TagMap, ToSchema, WriteAuditService, WriteControl,
    WriteRateLimiter, WriteState,
};

// --- /api/v1/* タグ空間 API（設計 §5.1） ------------------------------------
//
// T0-2（設計 §10-6、utoipa 採用 2026-08-04 決定）: 以下の応答型はすべて
// `Serialize` + `utoipa::ToSchema` を derive し、`#[utoipa::path]` で
// `ApiDoc`（このセクション末尾）にまとめて `GET /api/v1/openapi.json` で
// 配信する。**wire 形式（フィールド名・JSON 形）は T0-1 から一切変えて
// いない** - 型を導入しただけで、実際に流れる JSON は
// `apps/banto-hub/host/tests/integration.rs` の T0-1 時点のアサーションと
// 完全に同じ（このセクションの各関数 doc comment に旧来の json! リテラルの
// 形を残してあるのはそのため）。

/// `pub(crate)` (not private): `crate::stream`'s `GET /api/v1/stream` handler
/// is mounted directly onto [`tag_space_router`]'s `Router` (same
/// `require_tag_space_auth` layer, no separate router/state - T1 実装指示
/// §9「既存の require_tag_space_auth を /api/v1/stream にも適用」の最も
/// 単純な実現), so it needs this state type too.
#[derive(Clone)]
pub(crate) struct TagSpaceState {
    pub(crate) manager: Arc<CollectorManager>,
    pub(crate) controller: Arc<CollectionController>,
    /// T2-4（設計 §6-6）: `GET /api/v1/status` の `write_enabled`/
    /// `write_was_enabled_before_restart` のため。
    pub(crate) write_control: Arc<WriteControl>,
    /// T3（設計 §5.3）: `GET /api/v1/status` の `mqtt.connected` のため。
    pub(crate) mqtt: Arc<MqttPublisher>,
    /// T19 S3-b（docs/banto-hub-t19-design.md §3.9、UX-46）: `GET
    /// /api/v1/status`・`GET /api/status` の `system`（CPU%・メモリ）の
    /// ため。`admin_status_router`・`admin_tag_stream_router`・
    /// `tag_space_router`のいずれも[`api_router_with_controller_mode`]が
    /// 構築した**同じ** `Arc` を受け取る（`mqtt`/`write_control`等と同じ
    /// 共有規律）- `SystemInfoSampler`のモジュール doc comment が説明する
    /// 「共有`System`を使い回す」ことの前提。
    pub(crate) system_info: Arc<SystemInfoSampler>,
    /// 外部 DB 連携 S4（design §5.2・§5.5）: `GET /api/v1/status`・
    /// `GET /api/status` の `sink` 節のため。上記の各 `Arc` と同じ共有規律 -
    /// `sink_admin_router`（`PUT /api/sink/status`）が書き込むものと**同じ**
    /// `Arc`を受け取る。
    pub(crate) sink_status: Arc<SinkStatusStore>,
    /// #335（2026-09-14 オーナー決定）: computed タグの `value_source`/
    /// `effective_simulation` 判定に、式が参照する入力タグの現在の
    /// simulation 状態が要るため（[`ComputedEngine::referenced_tags`]）。
    /// `manager.computed_engine()` と**同じ** `Arc`（他の共有フィールドと
    /// 同じ規律）。
    pub(crate) computed: Arc<ComputedEngine>,
    /// #437: `GET /api/v1/status`・`GET /api/status` の `audit_pending_*` 等
    /// （監査の保留の状態、[`AuditLogService::spool_backlog`]）のため。他の
    /// ルーターへ渡すものと**同じ**サービスの複製（保留の件数はサービスが
    /// 持つ `Arc` の中にあるので、複製どうしで共有される）。
    pub(crate) audit: AuditLogService,
}

#[derive(Debug, Deserialize)]
pub(super) struct TagsQuery {
    connection: Option<String>,
    group: Option<String>,
}

/// `GET /api/v1/tags` の応答: `{ "revision", "run_id",
/// "collection_mode", "tags": [CatalogTagEntry...] }`。
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct CatalogResponse {
    pub(super) revision: u64,
    pub(super) run_id: Option<u64>,
    pub(super) collection_mode: String,
    pub(super) tags: Vec<CatalogTagEntry>,
}

/// REST wire DTO for one catalog entry.
///
/// `TagEntry` remains the saved/configured catalog owned by `CollectorManager`.
/// The runtime fields are layered here so an all-simulation run can be
/// observed without mutating the DB-backed catalog or the shared TagMap.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct CatalogTagEntry {
    #[serde(flatten)]
    pub(super) entry: TagEntry,
    pub(super) configured_simulation: bool,
    pub(super) effective_simulation: bool,
    pub(super) value_source: String,
}

impl CatalogTagEntry {
    pub(super) fn from_runtime(
        entry: &TagEntry,
        runtime: &CollectionStatus,
        map: &TagMap,
        computed: &ComputedEngine,
    ) -> Self {
        Self {
            entry: entry.clone(),
            configured_simulation: entry.simulation,
            effective_simulation: effective_simulation_for_tag(entry, runtime, map, computed),
            value_source: value_source_for_tag(entry, runtime, map, computed).to_string(),
        }
    }
}

pub(super) fn effective_simulation_for_connection(
    protocol: &str,
    enabled: bool,
    configured_simulation: bool,
    runtime: &CollectionStatus,
) -> bool {
    enabled
        && runtime.state == CollectionState::Running
        && matches!(protocol, "modbus-tcp" | "slmp")
        && (configured_simulation || runtime.mode == RunMode::AllSimulation)
}

/// `GET /api/v1/tags`・管理系 `GET /api/tag-catalog`（[`admin_tag_catalog`]、
/// 試運転モード対応・設計 §5.6・2026-08-31 オーナー決定「案A」の続き）が
/// 共有する本体。**API キー・セッション token のいずれでも、収集状態を
/// 問わず常に全タグを返す**（2026-09-14/15 オーナー決定 #335 - 接続単位の
/// simulation 設定済み PLC タグも computed タグも、catalog からは一切
/// 隠さない。「WS/gRPC では購読できるのに catalog からは発見できない」と
/// いう不整合を解消するための決定であり、以前ここにあった
/// `api_key_external_output_allowed`（API キー要求だけシミュレーション系
/// タグを除外していた関数、および呼び出し元だったこの関数の
/// `api_key_request` 引数）は撤去済み。2026-09-15 オーナー決定（「外部出力を
/// PLC への出力と勘違いしていた」）でさらに、[`v1_values`]・
/// [`v1_value_single`]・[`v1_value_read_now`]の run 単位 AllSimulation
/// ゲートも撤去した - 外部への読み取り出力はシミュレーションで一切
/// ゲートしない（catalog は元々ゲート対象外だった）。
pub(super) fn build_catalog_response(state: &TagSpaceState, query: &TagsQuery) -> CatalogResponse {
    let map = state.manager.tag_map();
    let revision = state.manager.revision();
    let runtime = state.controller.status();
    let tags: Vec<CatalogTagEntry> = map
        .iter()
        .filter(|entry| {
            query
                .connection
                .as_deref()
                .map(|c| c == entry.connection)
                .unwrap_or(true)
        })
        .filter(|entry| {
            query
                .group
                .as_deref()
                .map(|g| g == entry.group)
                .unwrap_or(true)
        })
        .map(|entry| CatalogTagEntry::from_runtime(entry, &runtime, &map, &state.computed))
        .collect();
    CatalogResponse {
        revision,
        run_id: runtime.run_id,
        collection_mode: runtime.mode.as_str().to_string(),
        tags,
    }
}

/// `GET /api/v1/tags` ハンドラ本体 - catalog: `{ "revision", "run_id",
/// "collection_mode", "tags": [CatalogTagEntry...] }`,
/// optionally filtered by `?connection=`/`?group=` (matched against the
/// entry's connection/group *name*, design §5.1's route table). API-key
/// requests see exactly the same tags as session/admin requests（2026-09-14
/// オーナー決定 #335、[`build_catalog_response`]のdoc comment参照）。
/// ロジックは[`build_catalog_response`]側にあり、ここでは`ctx`を受け取る
/// だけ（catalog は API キー・セッションの別を判定しない） - 管理系の
/// [`admin_tag_catalog`]も同じ関数を呼ぶ（二重管理を避けるための分離、
/// `compute_status`/[`v1_status`]と同じ構成）。
#[utoipa::path(
    get,
    path = "/api/v1/tags",
    params(
        ("connection" = Option<String>, Query, description = "接続名で絞り込む"),
        ("group" = Option<String>, Query, description = "収集グループ名で絞り込む"),
    ),
    responses((status = 200, description = "catalog スナップショット", body = CatalogResponse)),
    tag = "tag-space",
)]
async fn v1_tags(
    State(state): State<TagSpaceState>,
    Query(query): Query<TagsQuery>,
    _ctx: Option<Extension<ApiKeyContext>>,
) -> Json<CatalogResponse> {
    Json(build_catalog_response(&state, &query))
}

/// One `/api/v1/values*` entry's wire shape (design §5.1's route table:
/// `{ "tag", "v", "q", "t" }`).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub(super) struct ValueEntry {
    tag: String,
    v: Option<f64>,
    q: String,
    t: i64,
    value_source: String,
}

/// Thin wire-formatting wrapper over [`crate::hub::read_current`] (see its
/// doc comment - this is the "same v/q/t semantics as `crate::stream`'s
/// `data` messages" helper the T1 実装指示 asked to share rather than
/// duplicate; T6-2 widened it to also cover computed/internal tags via
/// `read_current` instead of calling `effective_sample` directly).
#[allow(clippy::too_many_arguments)]
pub(super) fn value_entry(
    external_name: &str,
    entry: &TagEntry,
    current: Option<&banto_collect::CurrentValuesHandle>,
    server_store: &crate::computed::ServerTagStore,
    now_ms: i64,
    runtime: &CollectionStatus,
    map: &TagMap,
    computed: &ComputedEngine,
) -> ValueEntry {
    let (v, q, t) = crate::hub::read_current(entry, current, server_store, now_ms);
    ValueEntry {
        tag: external_name.to_string(),
        v,
        q: crate::hub::quality_str(q).to_string(),
        t,
        value_source: value_source_for_tag(entry, runtime, map, computed).to_string(),
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct ValuesQuery {
    tags: Option<String>,
}

/// `GET /api/v1/values` の応答: `{ "revision", "t", "values": [ValueEntry...] }`。
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct ValuesResponse {
    pub(super) revision: u64,
    pub(super) t: i64,
    pub(super) run_id: Option<u64>,
    pub(super) collection_mode: String,
    pub(super) values: Vec<ValueEntry>,
}

/// Single-value response. The bulk response carries the same run metadata at
/// the response level, while each value carries its own source classification.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct SingleValueResponse {
    tag: String,
    v: Option<f64>,
    q: String,
    t: i64,
    run_id: Option<u64>,
    collection_mode: String,
    value_source: String,
}

impl SingleValueResponse {
    fn from_value(value: ValueEntry, runtime: &CollectionStatus) -> Self {
        Self {
            tag: value.tag,
            v: value.v,
            q: value.q,
            t: value.t,
            run_id: runtime.run_id,
            collection_mode: runtime.mode.as_str().to_string(),
            value_source: value.value_source,
        }
    }
}

/// `GET /api/v1/values` - full or partial (`?tags=a,b,c`) snapshot.
///
/// An unknown name in `?tags=` is a `400` enumerating every unresolved name
/// (design instructions: 「未知の名前が混ざったら...部分成功で誤解させない」),
/// never a per-row `bad`/`unknown_tag` - the request as a whole is rejected
/// so the caller cannot mistake "misspelled tag" for "tag exists but is
/// currently bad". (This error body stays a raw `serde_json::Value` - only
/// the *successful* `/api/v1/*` bodies were in scope for the T0-2 typed-struct
/// conversion.)
///
/// H10 ③(Option B、docs/h10-3-read-scope-proposal.md §5 S4): API キー起因
/// の読み取り(`ctx` あり)だけがタグ単位スコープで絞られる。`?tags=`
/// 省略(暗黙の全件)はスコープ外を**黙って除いた**集合を返す(「聞いても
/// いないのに403」を避ける)。`?tags=` で明示的にスコープ外タグを挙げたら
/// [`v1_value_single`] と同じ**403**(存在は catalog 経由で既知なので
/// 404 ではない)。セッション token(`ctx` 無し)は従来どおり全件(管理 UI
/// 不変)。2026-09-15 オーナー決定（#335 追補、「外部出力を PLC への出力と
/// 勘違いしていた」）: simulation / derived_simulation / computed をタグ
/// 単位で除外することはもう無く、run が AllSimulation 中でも無条件で
/// 値を返す - 外部への**読み取り**出力（REST/WS/gRPC/MQTT）はシミュレー
/// ションでゲートしない。呼び出し側は`value_source`/`collectionMode`で
/// 判別する。旧 T15-3 の`test_output`opt-in 機構は #362 で撤去済み
/// （`docs/tag-server-design.md` §6.3参照）。
#[utoipa::path(
    get,
    path = "/api/v1/values",
    params(
        ("tags" = Option<String>, Query, description = "カンマ区切りの外部名。省略時は全タグ"),
    ),
    responses(
        (status = 200, description = "現在値スナップショット", body = ValuesResponse),
        (status = 400, description = "?tags= に未知の外部名が含まれる"),
        (status = 403, description = "?tags= に per-tag read スコープ外の外部名が含まれる(API キー、H10 ③)"),
    ),
    tag = "tag-space",
)]
async fn v1_values(
    State(state): State<TagSpaceState>,
    Query(query): Query<ValuesQuery>,
    ctx: Option<Extension<ApiKeyContext>>,
) -> Response {
    let map = state.manager.tag_map();
    let runtime = state.controller.status();

    let names = match resolve_value_names(&map, &query) {
        Ok(names) => names,
        Err(unknown) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "unknown_tag", "tags": unknown })),
            )
                .into_response();
        }
    };

    let names: Vec<String> = if let Some(Extension(ctx)) = &ctx {
        if query.tags.is_some() {
            // 明示指定でスコープ外を1つでも挙げたら 403(単一と同じ規律)。
            if names.iter().any(|name| !ctx.can_read_value(name)) {
                return forbidden_response();
            }
            names
        } else {
            // 暗黙の全件はスコープ外を黙って除く。
            names
                .into_iter()
                .filter(|name| ctx.can_read_value(name))
                .collect()
        }
    } else {
        names
    };

    Json(build_values_response(&state, &map, &runtime, names)).into_response()
}

/// `GET /api/v1/values`・管理系`GET /api/values`([`admin_values`])が共有
/// する`?tags=`解決: 省略時は全タグ、指定時はカンマ区切りをパースする。
/// 未知の外部名を1つでも含む場合は`Err(unknown)`（外部名のリスト、呼び出し
/// 元が400を組み立てる）を返す。API キーのper-tagスコープ判定（`ctx`）は
/// ここに含めない - 管理系にはその概念が無く、`v1_values`側だけが追加で
/// 適用する。
pub(super) fn resolve_value_names(
    map: &TagMap,
    query: &ValuesQuery,
) -> Result<Vec<String>, Vec<String>> {
    let names: Vec<String> = match &query.tags {
        Some(raw) => raw
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        None => map
            .iter()
            .map(|entry| entry.external_name.clone())
            .collect(),
    };

    if query.tags.is_some() {
        let unknown: Vec<String> = names
            .iter()
            .filter(|name| map.get(name).is_none())
            .cloned()
            .collect();
        if !unknown.is_empty() {
            return Err(unknown);
        }
    }

    Ok(names)
}

/// 指定された外部名の集合から[`ValuesResponse`]を組み立てる -
/// `/api/v1/values`・管理系`/api/values`([`admin_values`])が共有する本体。
/// `map`/`runtime`は呼び出し元がAPIキーのper-tagスコープ絞り込み等を終えた
/// 後の値を渡す想定（1リクエスト内で複数回`tag_map()`/`controller.status()`
/// を呼んで状態がずれるのを避けるため、呼び出し元で1回だけ取得したものを
/// 共有する）。
pub(super) fn build_values_response(
    state: &TagSpaceState,
    map: &TagMap,
    runtime: &CollectionStatus,
    names: Vec<String>,
) -> ValuesResponse {
    let now_ms = state.manager.clock().now_ms();
    let current = state.manager.current_values();
    let server_store = state.manager.server_store();

    let values: Vec<ValueEntry> = names
        .iter()
        .filter_map(|name| map.get(name).map(|entry| (name, entry)))
        .map(|(name, entry)| {
            value_entry(
                name,
                entry,
                current.as_ref(),
                &server_store,
                now_ms,
                runtime,
                map,
                &state.computed,
            )
        })
        .collect();

    ValuesResponse {
        revision: state.manager.revision(),
        t: now_ms,
        run_id: runtime.run_id,
        collection_mode: runtime.mode.as_str().to_string(),
        values,
    }
}

/// `GET /api/v1/values/{tag}` - single tag. `404` only when the external
/// name is not in the catalog at all (design: 「404 になるのは定義が存在
/// しない外部名のみ」) - an undefined-but-uncollected tag is `200` with
/// `q: "bad"`.
///
/// H10 ③(Option B、docs/h10-3-read-scope-proposal.md §5 S3): タグは
/// catalog に見えている(=存在は既知)ので、per-tag read スコープ外は 404
/// ではなく**403**(`forbidden_response`)。API キー起因の読み取り
/// (`ctx` あり)だけがこの判定を受ける - セッション token(`ctx` 無し)は
/// 従来どおり全アクセス(管理 UI 不変)。2026-09-15 オーナー決定（#335 追補）:
/// simulation / derived_simulation / computed をタグ単位で隠すことはもう
/// 無く、run が AllSimulation 中でも無条件で値を返す（外部への読み取り
/// 出力はシミュレーションでゲートしない - `value_source`で判別させる）。
#[utoipa::path(
    get,
    path = "/api/v1/values/{tag}",
    params(("tag" = String, Path, description = "外部名 {connection}.{group}.{tag}")),
    responses(
        (status = 200, description = "単一タグの現在値", body = SingleValueResponse),
        (status = 403, description = "per-tag read スコープ外(API キー、H10 ③)"),
        (status = 404, description = "catalog に存在しない外部名"),
    ),
    tag = "tag-space",
)]
async fn v1_value_single(
    State(state): State<TagSpaceState>,
    Path(tag): Path<String>,
    ctx: Option<Extension<ApiKeyContext>>,
) -> Response {
    let map = state.manager.tag_map();
    let Some(entry) = map.get(&tag) else {
        return ApiError(BantoError::NotFound {
            resource: "tags".to_string(),
            id: tag,
        })
        .into_response();
    };
    if let Some(Extension(ctx)) = &ctx {
        if !ctx.can_read_value(&tag) {
            return forbidden_response();
        }
    }
    let now_ms = state.manager.clock().now_ms();
    let runtime = state.controller.status();
    let current = state.manager.current_values();
    let server_store = state.manager.server_store();
    Json(SingleValueResponse::from_value(
        value_entry(
            &tag,
            entry,
            current.as_ref(),
            &server_store,
            now_ms,
            &runtime,
            &map,
            &state.computed,
        ),
        &runtime,
    ))
    .into_response()
}

/// `GET /api/v1/values/{tag}/read-now` の応答(T20 ①b、
/// docs/banto-hub-t20-design.md §3.1)。[`SingleValueResponse`](cache 読み)
/// と違い `v` は `serde_json::Value` - 数値・真偽値・文字列のいずれも
/// 表せる(文字列タグはこの経路でしか読めない - このモジュールの
/// [`v1_value_read_now`] doc comment参照)。`SingleValueResponse` 自体は
/// 変更しない(既存の cache 読みの応答型を壊さない、実装指示の制約)。
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct ReadNowResponse {
    tag: String,
    /// 数値・真偽値・文字列のいずれか(数値タグは scale 済みの工学値 -
    /// `crate::read_path`のモジュール doc comment「スケーリング」節参照)。
    v: serde_json::Value,
    /// その場読みが成功した時点の品質は常に `"good"` - キャッシュ読みと
    /// 違い、この経路は失敗を `q: "bad"` の200応答ではなく HTTP エラーで
    /// 表す(`read_now_rejection_response`参照)ので、200応答の`q`は常に
    /// `"good"`。フィールド自体は cache 読みの `ValueEntry`/`SingleValueResponse`
    /// と形を揃えるために残す。
    q: String,
    /// この読み取りが完了した時刻(ミリ秒epoch) - cache 読みの `t`
    /// (サンプル採取時刻)と違い、その場読みでは「サンプル時刻」という
    /// 概念自体が無い(収集サイクルを経由しない)ので、応答組み立て時の
    /// 時刻をそのまま使う。
    t: i64,
}

impl ReadNowResponse {
    fn from_value(tag: String, value: banto_plc::PlcValue, now_ms: i64) -> Self {
        Self {
            tag,
            v: crate::read_path::plc_value_to_json(&value),
            q: "good".to_string(),
            t: now_ms,
        }
    }
}

/// `GET /api/v1/values/{tag}/read-now` - T20 ①b(案A、
/// docs/banto-hub-t20-design.md §3.1「その場読み」)。`GET
/// /api/v1/values/{tag}`(cache 読み、[`v1_value_single`]、
/// current_values/tstore 経由)とは別経路 - **この経路は毎回 PLC から直接
/// 読み、収集キャッシュ(current_values/tstore)には一切触れない**
/// (`crate::read_path`のモジュール doc comment参照)。収集パイプラインから
/// 意図的に除外されている文字列タグを読める唯一の GET エンドポイント。
///
/// 認証は [`v1_value_single`]と同じ read ルート規律(`ctx.has_any_read()`
/// はミドルウェア`require_tag_space_auth`、per-tag `ctx.can_read_value(tag)`
/// はこのハンドラ自身)。エラーマッピングの詳細は
/// [`crate::read_path::execute_read_now`]/[`read_now_rejection_response`]
/// 参照。2026-09-15 オーナー決定（#335 追補）: タグ単位の simulation /
/// derived_simulation / computed 除外も、run 単位の AllSimulation ゲートも
/// 撤去済み - 外部への読み取り出力はシミュレーションで一切ゲートしない。
#[utoipa::path(
    get,
    path = "/api/v1/values/{tag}/read-now",
    params(("tag" = String, Path, description = "外部名 {connection}.{group}.{tag}")),
    responses(
        (status = 200, description = "その場読みの結果(v は数値・真偽値・文字列のいずれか)", body = ReadNowResponse),
        (status = 403, description = "per-tag read スコープ外(API キー、H10 ③と同じ規律)"),
        (status = 404, description = "catalog に存在しない外部名"),
        (status = 422, description = "internal/computed タグ(PLC 接続を持たない)、またはアドレス不正"),
        (status = 501, description = "接続のプロトコルに broker ドライバが未対応"),
        (status = 502, description = "PLC からの読み取りに失敗"),
        (status = 503, description = "収集セッションが無い(新規にはダイヤルしない)"),
    ),
    tag = "tag-space",
)]
async fn v1_value_read_now(
    State(state): State<TagSpaceState>,
    Path(tag): Path<String>,
    ctx: Option<Extension<ApiKeyContext>>,
) -> Response {
    let map = state.manager.tag_map();
    if map.get(&tag).is_none() {
        return ApiError(BantoError::NotFound {
            resource: "tags".to_string(),
            id: tag,
        })
        .into_response();
    }
    if let Some(Extension(ctx)) = &ctx {
        if !ctx.can_read_value(&tag) {
            return forbidden_response();
        }
    }
    match crate::read_path::execute_read_now(&state.manager, &tag).await {
        Ok(value) => {
            let now_ms = state.manager.clock().now_ms();
            Json(ReadNowResponse::from_value(tag, value.value, now_ms)).into_response()
        }
        Err(rejection) => read_now_rejection_response(tag, rejection),
    }
}

/// [`v1_value_read_now`]の拒否理由 → HTTP 応答マッピング
/// (`write_rejection_response`の読み取り版・同じ形)。`NotFound` は
/// [`v1_value_read_now`]がその場読みを試みる前に自前で 404 返しているので
/// 通常はここへ来ないが、`crate::read_path::execute_read_now`は
/// 他の呼び出し元(MCP の `read_tag_now`)からも呼ばれる独立した関数であり、
/// かつ2回の catalog 参照の間に rebuild が挟まる理論上の競合もあり得るため、
/// `write_rejection_response`と違って`unreachable!`にはしない(素直に404
/// を返す)。
fn read_now_rejection_response(
    tag: String,
    rejection: crate::read_path::ReadNowRejection,
) -> Response {
    use crate::read_path::ReadNowRejection;

    if matches!(rejection, ReadNowRejection::NotFound) {
        return ApiError(BantoError::NotFound {
            resource: "tags".to_string(),
            id: tag,
        })
        .into_response();
    }

    let status = match &rejection {
        ReadNowRejection::NotFound => StatusCode::NOT_FOUND, // 上で早期returnされるが、network越しの二重呼び出し等に備え防御的に用意
        ReadNowRejection::NotPlcBacked | ReadNowRejection::InvalidAddress(_) => {
            StatusCode::UNPROCESSABLE_ENTITY
        }
        ReadNowRejection::UnsupportedProtocol => StatusCode::NOT_IMPLEMENTED,
        ReadNowRejection::NoSession => StatusCode::SERVICE_UNAVAILABLE,
        ReadNowRejection::ReadFailed(_) => StatusCode::BAD_GATEWAY,
        ReadNowRejection::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(rejection.to_json())).into_response()
}

/// `GET /api/v1/status` の `connections` 配列1件分。
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct ConnectionStatusEntry {
    pub(super) name: String,
    pub(super) id: i64,
    pub(super) status: String,
    pub(super) attempt: Option<u32>,
    /// T9-2 (docs/ux-plan.md §1): mirrors `banto_tags::PlcConnection::simulation` -
    /// lets a monitoring client (or the admin UI) flag a connection whose
    /// live values are synthetic, not from a real PLC.
    pub(super) simulation: bool,
    pub(super) configured_simulation: bool,
    pub(super) effective_simulation: bool,
}

/// `GET /api/v1/status` の `mqtt`（T3、設計実装指示「`/api/v1/status` に
/// `mqtt: { "enabled": bool, "connected": bool }` を追加」）。`enabled` は
/// 設定値（settings テーブルの `mqtt.enabled`）、`connected` は
/// [`crate::mqtt::MqttPublisher::connected`]（実際に MQTT ブローカーへ接続
/// できているかのライブ状態）- 両者は独立: `enabled=true` でもブローカー
/// 不通なら `connected=false` になる。
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct MqttStatusEntry {
    enabled: bool,
    connected: bool,
}

/// `GET /api/v1/status` の `grpc`（T4、設計実装指示「`/api/v1/status` に
/// `grpc: { enabled, port }` を追加」）。MQTT と違い「実際に接続できて
/// いるか」のライブ状態は持たない - gRPC サーバーは外部へ接続しに行く
/// クライアントではなく listen するだけなので、設定値がそのまま意図した
/// 状態を表す(`crate::grpc::GrpcServer`のモジュール doc comment参照)。
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct GrpcStatusEntry {
    enabled: bool,
    port: u16,
}

/// `GET /api/v1/status` の `last_apply`（T7-2、設計 §4.3実装指示「ApplyReport
/// の内容を...`/api/v1/status` に出す」）: 直近の `apply_config` 呼び出しの
/// 結果 - どの接続が追加/削除/入れ替え/無変更だったか、tstore writer が
/// ローテートしたか。[`banto_collect::ApplyReport`] のワイヤ表現（同クレート
/// 側は `ToSchema`/`Serialize` を持たない内部型のため、ここで DTO 化する -
/// このファイルの他の DTO（[`ConnectionStatusEntry`] 等）と同じ流儀）。
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct LastApplyEntry {
    pub(super) added: Vec<String>,
    pub(super) removed: Vec<String>,
    pub(super) replaced: Vec<String>,
    pub(super) unchanged: Vec<String>,
    pub(super) writer_rotated: bool,
}

impl From<ApplyReport> for LastApplyEntry {
    fn from(report: ApplyReport) -> Self {
        Self {
            added: report.added,
            removed: report.removed,
            replaced: report.replaced,
            unchanged: report.unchanged,
            writer_rotated: report.writer_rotated,
        }
    }
}

/// `GET /api/v1/status` の `db_source`（外部 DB 連携 S2、
/// docs/banto-hub-external-db-design.md §4.2・§4.3）: DB Source の接続ごとの
/// 運転状態。`crate::db_source::status` のプロセス内メモリをそのまま写した
/// もので、**秘密は含まない**（`last_error` は
/// `crate::db_source::sanitize_postgres_error` を通した文字列だけ）ため、
/// `mqtt`/`grpc` 節と同じく admin 限定にはしていない。
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct DbSourceStatusEntry {
    pub(super) connection_id: i64,
    pub(super) connection_name: String,
    /// `connected` / `backoff` / `disabled` / `error` / `stopped`
    /// （`crate::db_source::DbConnectionState`）。`stopped` は S2b
    /// （設計 §4.8・§6-16）: 収集が Running でないのでタスクを起動して
    /// いない（DB へ繋いでいない・配下のタグは Bad）。
    pub(super) state: String,
    pub(super) last_poll_at: Option<i64>,
    pub(super) last_error: Option<String>,
    pub(super) consecutive_failures: u32,
    /// S2b（§4.8・§6-17）: この接続のタスクが異常終了して supervisor に
    /// 作り直された回数と、その最後の理由
    /// （`crate::db_source::DbConnectionStatus::restarts`）。
    pub(super) restarts: u32,
    pub(super) last_restart_reason: Option<String>,
    pub(super) groups: Vec<DbSourceGroupStatusEntry>,
}

/// [`DbSourceStatusEntry`] のグループ1件分。
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct DbSourceGroupStatusEntry {
    pub(super) group_id: i64,
    pub(super) group_name: String,
    pub(super) last_ok_at: Option<i64>,
    pub(super) last_error: Option<String>,
    /// 直近の実行で取れた行数。生成 SQL が `LIMIT 2` を付けるので `2` は
    /// 「2 行以上」を意味する（`crate::db_source::convert::build_wrapper_sql`）。
    pub(super) row_count_last: Option<i64>,
}

impl From<crate::db_source::DbConnectionStatus> for DbSourceStatusEntry {
    fn from(status: crate::db_source::DbConnectionStatus) -> Self {
        Self {
            connection_id: status.connection_id,
            connection_name: status.connection_name,
            state: status.state.as_str().to_string(),
            last_poll_at: status.last_poll_at,
            last_error: status.last_error,
            consecutive_failures: status.consecutive_failures,
            restarts: status.restarts,
            last_restart_reason: status.last_restart_reason,
            groups: status
                .groups
                .into_iter()
                .map(|group| DbSourceGroupStatusEntry {
                    group_id: group.group_id,
                    group_name: group.group_name,
                    last_ok_at: group.last_ok_at,
                    last_error: group.last_error,
                    row_count_last: group.row_count_last,
                })
                .collect(),
        }
    }
}

/// `GET /api/v1/status` の `ingest`（取り込み口 `push`、R2a-2e、
/// docs/banto-hub-ingest-design.md §9）: `push` 接続ごとの受信の状態。
/// `crate::ingest::IngestHub::status` のプロセス内メモリを写したもので、
/// **資格情報と値は含まない**（送り手は API キーの名前・接続元・時刻だけ、保持値は
/// 数だけ。`sender_status` は送り手の `status` を制御文字を落として 1 KiB に
/// 切り詰めたもの、`unknown_keys` も同じく切り詰めたキー）ため、`db_source` 節と
/// 同じく admin 限定にはしていない。
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct IngestStatusEntry {
    pub(super) connection_id: i64,
    pub(super) name: String,
    pub(super) enabled: bool,
    /// `settings.bridge` がある（DB ブリッジの接続）。
    pub(super) bridge: bool,
    /// `live`（送り手の最後の通信から `stale_timeout_ms` 以内）/ `down`（§4.3）。
    pub(super) link: String,
    /// `down` の理由（「送り手からの通信が {n} 秒ありません」など）。`live` なら `null`。
    pub(super) link_detail: Option<String>,
    pub(super) last_seen_at: Option<i64>,
    pub(super) stale_timeout_ms: u64,
    pub(super) collecting: bool,
    pub(super) epoch: u64,
    /// 保持している値の数（値そのものは出さない）。
    pub(super) held_values: u64,
    pub(super) senders: Vec<IngestSenderStatusEntry>,
    /// 受け付けた件数（Hub の起動から）。
    pub(super) accepted: u64,
    /// 件ごとに断った件数（§6.3）。
    pub(super) rejected: u64,
    /// 頻度の上限で断ったメッセージの数。
    pub(super) rate_limited: u64,
    /// 件数の上限（1 メッセージ 1,000 件）で断ったメッセージの数。
    pub(super) too_many_items: u64,
    /// 直近の未知のキー（新しい順、20 件まで）。
    pub(super) unknown_keys: Vec<String>,
    /// 受け付けた件の `t` の最大（任意）。
    pub(super) last_source_time: Option<i64>,
    /// 送り手の最後の `status`（任意、1 KiB まで）。
    pub(super) sender_status: Option<String>,
}

/// [`IngestStatusEntry`] の送り手 1 つ。`transport` は `ws`（開いている WS、
/// `connected_at` は開いた時刻）か `rest`（最後に受け付けた要求、`last_seen_at`）。
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct IngestSenderStatusEntry {
    pub(super) transport: String,
    pub(super) api_key_name: String,
    pub(super) peer: Option<String>,
    pub(super) connected_at: Option<i64>,
    pub(super) last_seen_at: Option<i64>,
}

impl From<crate::ingest::IngestConnectionStatus> for IngestStatusEntry {
    fn from(status: crate::ingest::IngestConnectionStatus) -> Self {
        Self {
            connection_id: status.connection_id,
            name: status.name,
            enabled: status.enabled,
            bridge: status.bridge,
            link: if status.live { "live" } else { "down" }.to_string(),
            link_detail: status.link_detail,
            last_seen_at: status.last_seen_ms,
            stale_timeout_ms: status.stale_timeout_ms,
            collecting: status.collecting,
            epoch: status.epoch,
            held_values: status.held_values as u64,
            senders: status
                .senders
                .into_iter()
                .map(|sender| {
                    let ws = sender.transport == crate::ingest::SenderTransport::Ws;
                    IngestSenderStatusEntry {
                        transport: sender.transport.as_str().to_string(),
                        api_key_name: sender.sender.api_key_name,
                        peer: sender.sender.peer,
                        connected_at: ws.then_some(sender.sender.at_ms),
                        last_seen_at: (!ws).then_some(sender.sender.at_ms),
                    }
                })
                .collect(),
            accepted: status.accepted,
            rejected: status.rejected,
            rate_limited: status.rate_limited,
            too_many_items: status.too_many_items,
            unknown_keys: status.unknown_keys,
            last_source_time: status.last_source_time_ms,
            sender_status: status.sender_status,
        }
    }
}

/// `GET /api/v1/status`の`sink.sidecar`（外部 DB 連携 S4、design §5.2・
/// §5.5）。`crate::sink::status::SinkStatusSnapshot`のwire形。
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct SinkSidecarStatusEntry {
    /// `"online"`（[`crate::sink::SINK_SIDECAR_STALE_AFTER_MS`]以内に
    /// `PUT /api/sink/status`の push があった）か`"unknown"`（無い、または
    /// 一度も push が無い）。
    pub(super) state: String,
    pub(super) last_seen_at: Option<i64>,
}

/// `GET /api/v1/status`の`sink.groups`の1件分 - サイドカーが最後に push
/// した内容をそのまま映す（`crate::sink::SinkGroupStatusPush`のwire形）。
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct SinkGroupStatusEntry {
    pub(super) id: i64,
    pub(super) state: String,
    pub(super) queued: u64,
    pub(super) dropped: u64,
    pub(super) last_flush_at: Option<i64>,
    pub(super) last_error: Option<String>,
}

/// `GET /api/v1/status`の`sink`節（外部 DB 連携 S4）。
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct SinkStatusEntry {
    pub(super) sidecar: SinkSidecarStatusEntry,
    pub(super) groups: Vec<SinkGroupStatusEntry>,
}

impl From<SinkStatusSnapshot> for SinkStatusEntry {
    fn from(snapshot: SinkStatusSnapshot) -> Self {
        Self {
            sidecar: SinkSidecarStatusEntry {
                state: snapshot.sidecar_state.to_string(),
                last_seen_at: snapshot.last_seen_at,
            },
            groups: snapshot
                .groups
                .into_iter()
                .map(|push| SinkGroupStatusEntry {
                    id: push.id,
                    state: push.state,
                    queued: push.queued,
                    dropped: push.dropped,
                    last_flush_at: push.last_flush_at,
                    last_error: push.last_error,
                })
                .collect(),
        }
    }
}

/// `GET /api/v1/status` の応答。T19 S5: `crate::mcp`の`get_server_status`
/// ツールが[`compute_status`]をそのまま呼んで`serde_json::to_value`で
/// 包み直すため`pub(crate)`（フィールド自体へは触れない - トレイト経由の
/// シリアライズのみ）。
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct StatusResponse {
    pub(super) version: String,
    pub(super) revision: u64,
    pub(super) configured_revision: u64,
    pub(super) running_revision: u64,
    pub(super) run_id: Option<u64>,
    pub(super) collection_state: String,
    pub(super) collection_mode: String,
    pub(super) last_runtime_error: Option<String>,
    pub(super) last_config_error: Option<String>,
    pub(super) connections: Vec<ConnectionStatusEntry>,
    /// T2-4（設計 §6-6）: 書き込み受付が今いま有効かどうか(ライブフラグ)。
    pub(super) write_enabled: bool,
    /// T2-4（設計 §6-6）: 起動時に永続テーブルから復元した値
    /// (`crate::write_control::WriteControl` のモジュール doc comment
    /// 参照。以後の enable/disable ではこの値自体は変わらない。
    /// 2026-09-09 オーナー決定 #340)。
    pub(super) write_was_enabled_before_restart: bool,
    /// #433: 書き込み受付の保存状態の注意書き（起動時に DB と状態ファイルが
    /// 食い違っていた、直近の停止・再開を片方に保存できなかった等。
    /// `crate::write_control::WriteControl::persistence_warning`）。無ければ `null`。
    pub(super) write_persistence_warning: Option<String>,
    /// #437: DB に書けず保留ファイルに退避している監査の件数（このプロセスが
    /// 見ている分、`banto_admin_services::audit::SpoolBacklog::count`）。DB が
    /// 戻れば流し込まれて 0 に戻る。保留を使っていない構成では常に 0。
    pub(super) audit_pending_count: u64,
    /// #437: 保留中で最も古い監査の時刻（`audit_log.ts` と同じ書式、UTC）。
    /// 保留が無ければ `null`。
    pub(super) audit_pending_oldest_ts: Option<String>,
    /// #437: 保留が上限（10,000 件）に達して捨てた監査の件数（起動から）。
    pub(super) audit_dropped_count: u64,
    /// #437: 保留ファイルの書き込みにも失敗して失った監査の件数（起動から）。
    pub(super) audit_failed_count: u64,
    /// T3（設計 §5.3）: MQTT publish の設定/接続状態。
    pub(super) mqtt: MqttStatusEntry,
    /// T4（設計 §5.4）: gRPC サーバーの設定。
    pub(super) grpc: GrpcStatusEntry,
    /// T7-2（設計 §4.3）: 直近の `apply_config` 呼び出しの結果。最後に成功
    /// した rebuild が `apply_config` を経由しなかった場合（起動直後の初回
    /// 成功、または空構成への遷移）は `null`
    /// (`crate::hub::CollectorManager::last_apply` のドキュメント参照)。
    pub(super) last_apply: Option<LastApplyEntry>,
    /// 外部 DB 連携 S2（§4.2・§4.3）: DB Source の接続ごとの運転状態。
    /// `postgres` 接続が1つも無い通常構成では空配列。
    pub(super) db_source: Vec<DbSourceStatusEntry>,
    /// 取り込み口 `push`（R2a-2e、docs/banto-hub-ingest-design.md §9）: `push`
    /// 接続ごとの受信の状態。`push` 接続が無ければ空配列。
    pub(super) ingest: Vec<IngestStatusEntry>,
    /// T19 S3-b（docs/banto-hub-t19-design.md §3.9、UX-46「サーバー状態の
    /// 拡充」）: サーバー自身の CPU 使用率・メモリ使用量。
    /// `crate::system_info::SystemInfoSampler`のモジュール doc comment
    /// 参照 - プロセス起動直後最初の呼び出しは `cpu_percent` が `0.0`に
    /// なりうる。
    pub(super) system: SystemInfoSnapshot,
    /// 外部 DB 連携 S4（design §5.2・§5.5）: DB Sink サイドカーの運転状態。
    /// サイドカーが1度も`PUT /api/sink/status`を push していない、または
    /// 15秒以上 push が無ければ`sidecar.state == "unknown"`
    /// （`crate::sink::status`のモジュール doc comment参照）。
    pub(super) sink: SinkStatusEntry,
}

/// `GET /api/v1/status`・管理系 `GET /api/status`（2026-08-31 オーナー決定
/// 「案A」、`admin_status`参照）が共有する本体。`{ "version", "revision",
/// "last_config_error", "connections": [...] }` (design §5.1's route table)
/// を組み立てる。Connection names come from the registry directly (not the
/// catalog) so a connection with zero tags still appears.
///
/// T2-2/#131 (docs/tag-server-design.md §6-5, 2026-08-05 決定): a
/// broker-managed connection's status (per `HubSessions::is_broker_managed`;
/// `"slmp"` and, as of #131, 2026-09-01, `"modbus-tcp"` too) comes from
/// [`crate::hub::CollectorManager::broker_status`] (the broker's own
/// `ConnState`) instead of `banto_collect`'s own status map - see
/// `crate::broker_glue`'s module doc ("The two-backoff double bookkeeping")
/// for why the broker's answer is the one that reflects whether the physical
/// session is actually up for a broker-managed connection. A connection
/// whose protocol the broker does NOT manage (any protocol string outside
/// that registered set) still falls back to reading from
/// `banto_collect::Collector::status` - unaffected by #131, since it was
/// never broker-routed to begin with.
///
/// **T19 S2-a (UX-48, 2026-09-03)**: an enabled, broker-managed connection
/// with zero enabled collection groups reports `"unused"` here instead of
/// falling through to `broker_status` (which would read `None`/`Stopped`
/// indistinguishably from a disabled connection) - see
/// `crate::hub::CollectorManager::sync_broker_sessions_from`'s doc comment for
/// why such a connection has no broker session to report on in the first
/// place.
pub(crate) async fn compute_status(state: &TagSpaceState) -> Result<StatusResponse, ApiError> {
    let runtime = state.controller.status();
    let revision = state.manager.configured_revision();
    let last_config_error = state.manager.last_error();
    let statuses = state.manager.connection_status().await;

    let connections = state
        .manager
        .plc_connections()
        .list(ListParams::default())
        .await?
        .rows;
    // T19 S2-a (UX-48): same predicate the session sync
    // (`CollectorManager::sync_broker_sessions_from`) and the collector itself
    // (`banto_collect::build_config_from`) use to decide "does this
    // connection have anything to collect" - reused here so the status
    // screen can tell a genuinely-unused connection ("unused" below) apart
    // from one that is enabled, has tags, and simply isn't connecting
    // ("stopped"/"reconnecting").
    let collectible_connection_ids = connections_with_collected_groups(
        &CollectionGroupService::new(state.manager.pool())
            .list(ListParams::default())
            .await?
            .rows,
    );
    let mqtt_settings = SettingsService::new(Db::Sqlite(state.manager.pool()))
        .mqtt_config()
        .await?;
    let grpc_settings = SettingsService::new(Db::Sqlite(state.manager.pool()))
        .grpc_config()
        .await?;

    let sessions = state.manager.sessions();
    let entries: Vec<ConnectionStatusEntry> = connections
        .into_iter()
        .map(|conn| {
            let (status_str, attempt) = if sessions.is_broker_managed(&conn.protocol) {
                // T19 S2-a (UX-48): a connection with no enabled collection
                // group gets no broker session pre-synced at all (see
                // `CollectorManager::sync_broker_sessions_from`'s doc comment),
                // so `broker_status` would otherwise round it down to
                // "stopped" indistinguishably from a disabled or genuinely
                // failing connection. Surface it as its own "unused" status
                // instead - a disabled connection still reports "stopped"
                // (checked first: `conn.enabled` gates this branch), so the
                // two stay visually distinct.
                if conn.enabled && !collectible_connection_ids.contains(&conn.id) {
                    ("unused", None)
                } else {
                    match state.manager.broker_status(conn.id) {
                        Some(BrokerConnectionStatus::Connected) => ("connected", None),
                        Some(BrokerConnectionStatus::Reconnecting { attempt }) => {
                            ("reconnecting", Some(attempt))
                        }
                        // "stopped" also covers "no broker session yet" (no
                        // rebuild has run, or this connection is currently
                        // disabled) - same rounding banto_collect's own
                        // ConnectionStatus branch below uses.
                        Some(BrokerConnectionStatus::Stopped) | None => ("stopped", None),
                    }
                }
            } else {
                let key = format!("conn:{}", conn.id);
                match statuses.get(&key) {
                    Some(ConnectionStatus::Connected) => ("connected", None),
                    Some(ConnectionStatus::Reconnecting { attempt }) => {
                        ("reconnecting", Some(*attempt))
                    }
                    // "stopped" also covers "never started" (e.g. this
                    // connection - or everything - is currently disabled, or no
                    // rebuild has run yet) - the design's ConnectionStatus enum
                    // has no fourth "never seen" variant, and "stopped" reads
                    // correctly for all of those (design: 決定事項なし → 妥当な
                    // 解釈として Stopped に丸める).
                    Some(ConnectionStatus::Stopped) | None => ("stopped", None),
                }
            };
            ConnectionStatusEntry {
                name: conn.name,
                id: conn.id,
                status: status_str.to_string(),
                attempt,
                simulation: conn.simulation,
                configured_simulation: conn.simulation,
                effective_simulation: effective_simulation_for_connection(
                    &conn.protocol,
                    conn.enabled,
                    conn.simulation,
                    &runtime,
                ),
            }
        })
        .collect();
    let audit_backlog = state.audit.spool_backlog();

    Ok(StatusResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
        revision,
        configured_revision: runtime.configured_revision,
        running_revision: runtime.running_revision,
        run_id: runtime.run_id,
        collection_state: runtime.state.as_str().to_string(),
        collection_mode: runtime.mode.as_str().to_string(),
        last_runtime_error: runtime.last_error,
        last_config_error,
        connections: entries,
        write_enabled: state.write_control.is_enabled(),
        write_was_enabled_before_restart: state.write_control.was_enabled_before_restart(),
        write_persistence_warning: state.write_control.persistence_warning(),
        audit_pending_count: audit_backlog.count,
        audit_pending_oldest_ts: audit_backlog.oldest_ts,
        audit_dropped_count: audit_backlog.dropped,
        audit_failed_count: audit_backlog.failed,
        mqtt: MqttStatusEntry {
            enabled: mqtt_settings.enabled,
            connected: state.mqtt.connected(),
        },
        grpc: GrpcStatusEntry {
            enabled: grpc_settings.enabled,
            port: grpc_settings.port,
        },
        last_apply: state.manager.last_apply().map(LastApplyEntry::from),
        db_source: state
            .manager
            .db_source_status()
            .into_iter()
            .map(DbSourceStatusEntry::from)
            .collect(),
        ingest: state
            .manager
            .ingest_hub()
            .status()
            .into_iter()
            .map(IngestStatusEntry::from)
            .collect(),
        system: state.system_info.sample(),
        sink: state
            .sink_status
            .snapshot(state.manager.clock().now_ms())
            .into(),
    })
}

/// `GET /api/v1/status` ハンドラ本体 - [`compute_status`]をそのまま JSON へ
/// 包むだけ。ロジックは`compute_status`側にあり、ここでは wire 形式
/// （snake_case、機械クライアント向け）を選ぶだけ - 管理系の
/// [`admin_status`]はカラムは同じ`compute_status`を呼び、camelCase の
/// [`AdminStatusResponse`]へ包み直す（二重管理を避けるための分離）。
#[utoipa::path(
    get,
    path = "/api/v1/status",
    responses((status = 200, description = "サーバー状態", body = StatusResponse)),
    tag = "tag-space",
)]
async fn v1_status(State(state): State<TagSpaceState>) -> Result<Json<StatusResponse>, ApiError> {
    Ok(Json(compute_status(&state).await?))
}

// --- /api/v1/* 認証: API キー + セッション bearer 併用（設計 §5.6・T0-2） ---

#[derive(Clone)]
struct TagSpaceAuthState {
    auth: AuthState,
    api_keys: ApiKeysService,
    audit: AuditLogService,
    manager: Arc<CollectorManager>,
}

/// `/api/v1/*`（`GET /api/v1/openapi.json` を除く）の認証ミドルウェア -
/// T0-1 の `require_auth`（セッション bearer のみ）を置き換える（このモジュール
/// の doc comment 参照）。
///
/// - `Authorization` ヘッダがない、または `Bearer ` で始まらない → `GET
///   /api/v1/stream` だけは [`extract_ws_protocol_token`] で
///   `Sec-WebSocket-Protocol` からのフォールバックを試す（ブラウザの
///   WebSocket が `Authorization` を送れないための救済 - 同関数の doc
///   comment 参照）。それも失敗すれば 401
/// - 値が `bh_` で始まる → API キーとして
///   [`crate::api_keys::ApiKeysService::lookup`] で照合:
///   - [`ApiKeyLookup::Valid`] → `POST /api/v1/values/{tag}`（書き込み、
///     T2-4）以外は `read` スコープを要求する（`write:` のみのキー等は
///     403 - 認証はできたが権限がない、という区別のため 401 ではなく
///     403）。書き込みルートは `read` を要求せず、代わりに
///     [`v1_write_value`] 自身が `write:{tag}` の完全一致を検査する
///     （リクエスト path の `{tag}` はここではまだ分からないため）。
///     いずれの場合も通過時は [`ApiKeyContext`] をリクエスト
///     extensions に載せて次段へ渡す（[`v1_write_value`] がスコープ
///     検査と監査行の `api_key_id`/`api_key_name_snapshot` に使う）。
///     `last_used_at` を60秒スロットルで更新（[`crate::api_keys::should_touch_last_used`]）
///   - [`ApiKeyLookup::Revoked`] → 401 + audit_log に
///     `action: "denied", resource: "api_keys"` を記録（設計 T0-2 実装
///     指示: 「失効済みキーでのアクセス試行は audit_log に記録する」）
///   - [`ApiKeyLookup::Tripped`]（T2-4、設計 §6-4）→ 403
///     `{ "error": "key_tripped" }` + audit_log に同様の `denied` 記録
///     （read/write いずれのリクエストも拒否 - `crate::api_keys` の
///     モジュール doc comment「トリップ」参照）
///   - [`ApiKeyLookup::Expired`]（H10 ①、docs/improvement-plan.md・
///     2026-08-08 オーナー決定）→ 401 + audit_log に同様の `denied` 記録
///     （`reason: "expired"` - `Revoked` の腕をそのまま踏襲。期限切れは
///     「失効」ではないが、未認証というレスポンス上の扱いは revoked と同じ
///     401 が適切 - `crate::api_keys` のモジュール doc comment「有効期限」
///     参照）
///   - [`ApiKeyLookup::NotFound`] → 401（監査記録しない - 存在しない/
///     偽造されたキーは「誰が」を特定できないただのノイズであり、
///     revoked の場合と違って「元は正規に発行されたキーが使われた」という
///     実害のシグナルがない）
/// - それ以外（`bh_` で始まらない）→ 従来どおり `AuthState` のセッション
///   token として照合（管理 UI からの利用互換のため - このモジュールの
///   doc comment参照）。ただし書き込みルートに限っては 403
///   `session_token_cannot_write` を返す（設計 §6-8: 「セッション token
///   では書けない」）
async fn require_tag_space_auth(
    State(state): State<TagSpaceAuthState>,
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let Some(token) = bearer_token(req.headers())
        .map(str::to_string)
        .or_else(|| extract_ws_protocol_token(req.uri().path(), req.headers()))
    else {
        return unauthorized_response();
    };

    // T2-4（設計 §6-8）: `POST /api/v1/values/{tag}` だけ認証規律が違う
    // （read スコープ不要・write:{tag} は `v1_write_value` 自身が検査・
    // セッション token 不可）- パスは `/api/v1/values/{tag}` の
    // ちょうど1階層下なので、`/api/v1/values`（一括スナップショット）や
    // `/api/v1/values/{tag}` への GET とは POST であることで区別する。
    let is_write_route =
        req.method() == Method::POST && req.uri().path().starts_with("/api/v1/values/");

    if token.starts_with("bh_") {
        // H10 ①: 期限切れ判定([`crate::api_keys::ApiKeysService::lookup`])
        // にも last_used_at 更新にも同じ「今」を使う - 呼び出しごとに
        // ずれないよう一度だけ取得する。
        let now_ms = state.manager.clock().now_ms();
        // #435: 使えないキーの応答は `api_key_verdict` →
        // `api_key_rejection_response` の 1 か所で決める（MCP・サイドカー用と
        // 共通）。監査（denied）はこの経路だけが残す（従来どおり）。
        match api_key_verdict(state.api_keys.check(&token, now_ms).await) {
            Ok(ctx) => {
                // H10 ③(Option B): この認証層のゲートは「read 系ルートに
                // 入れるか」だけを見る(has_any_read = 素の read か任意の
                // read:... を1つでも持つか)。個々のタグの値を読めるかどうか
                // (can_read_value)は catalog を絞らず、値ハンドラ側
                // (v1_value_single/v1_values/crate::stream)が個別に判定する
                // - `crate::api_keys::ApiKeyContext` の doc comment「read の
                // タグ単位化」参照。
                if !is_write_route && !ctx.has_any_read() {
                    return forbidden_response();
                }
                if let Err(err) = state
                    .api_keys
                    .touch_last_used(ctx.id, now_ms, ctx.last_used_at_ms)
                    .await
                {
                    eprintln!("banto-hub: API キーの last_used_at 更新に失敗しました: {err}");
                }
                req.extensions_mut().insert(ctx);
                // #430: `/api/v1/stream` が接続中も同じキーを照合し直せるよう、
                // 照合に使う材料を載せる（`crate::stream::ApiKeyStreamCredential`）。
                req.extensions_mut()
                    .insert(crate::stream::ApiKeyStreamCredential::new(
                        state.api_keys.clone(),
                        token.clone(),
                        state.manager.clone(),
                    ));
                next.run(req).await
            }
            Err((rejection, denied)) => {
                if let Some(denied) = denied {
                    let method = req.method().as_str().to_string();
                    let path = req.uri().path().to_string();
                    state
                        .audit
                        .record(AuditEntry {
                            actor_username: None,
                            actor_role: None,
                            action: "denied",
                            resource: "api_keys",
                            entity_id: Some(&denied.id.to_string()),
                            detail: Some(json!({
                                "reason": denied.reason,
                                "name": denied.name,
                                "method": method,
                                "path": path
                            })),
                            origin: "rest",
                            result: "denied",
                        })
                        .await;
                }
                api_key_rejection_response(rejection)
            }
        }
    } else {
        // banto v1.7.0 #204: セッション token もアカウントと照合する
        // （`require_session` と同じ判断: 失効は 401、DB が
        // 答えられなければトークンを残して 500）。
        match state.auth.authenticate(&token).await {
            Ok(Some(_)) if is_write_route => session_token_cannot_write_response(),
            Ok(Some(session)) => {
                req.extensions_mut().insert(session);
                // #430: `/api/v1/stream` をセッションで開いたときも、接続中に
                // 同じトークンを照合し直せるよう材料を載せる
                // （`crate::stream::SessionStreamCredential`）。
                req.extensions_mut()
                    .insert(crate::stream::SessionStreamCredential::new(
                        state.auth.clone(),
                        token.clone(),
                    ));
                next.run(req).await
            }
            Ok(None) => unauthorized_response(),
            Err(err) => ApiError(err).into_response(),
        }
    }
}

/// `/api/v1/*`（design §5.1/§5.6）: `require_tag_space_auth`（API キー +
/// セッション bearer 併用）を全ルートに適用する。`GET /api/v1/openapi.json`
/// は別ルーター（`openapi_router`、`crate::rest::api_router` 参照）に分けて
/// あり、この認証層を通らない。
///
/// T2-4: GET 系ハンドラ（[`TagSpaceState`]）と書き込みハンドラ
/// （[`WriteState`]）は状態型が違うため axum の `Router` を分けて組み立て、
/// `.merge()` してから認証レイヤーを1枚だけ被せる（同じ
/// `/api/v1/values/{tag}` パスに GET/POST が同居するのは axum の
/// `MethodRouter` マージがそのまま面倒を見る - `WriteState`のモジュール
/// doc comment参照）。
#[allow(clippy::too_many_arguments)]
pub(super) fn tag_space_router(
    manager: Arc<CollectorManager>,
    controller: Arc<CollectionController>,
    auth: AuthState,
    api_keys: ApiKeysService,
    audit: AuditLogService,
    write_control: Arc<WriteControl>,
    write_audit: WriteAuditService,
    events: broadcast::Sender<ServerEvent>,
    mqtt: Arc<MqttPublisher>,
    // T4（設計 §5.4 実装指示「二重実装は絶対に不可」）: `crate::grpc::GrpcService`
    // と**同じ** `Arc` を受け取る - ここで新規に構築しない。書き込みゲート
    // (`crate::write_path::execute_write`)を REST/gRPC で共有していても、
    // レート制限のバジェット(`crate::write_rate::WriteRateLimiter`)が別
    // インスタンスなら「タグ毎+全体の書き込み上限」が実質2倍緩む抜け道に
    // なる - 呼び出し元（[`api_router`]）が1個だけ構築し、REST・gRPC 両方の
    // `WriteState`/`GrpcService` へ同じ `Arc` を配る。
    rate_limiter: Arc<AsyncMutex<WriteRateLimiter>>,
    enforce_collection_state: bool,
    // T19 S3-b（docs/banto-hub-t19-design.md §3.9、UX-46）: `GET
    // /api/v1/status` の `system` のため - `admin_status_router`/
    // `admin_tag_stream_router`へ渡すものと**同じ** `Arc`（上記
    // `mqtt`/`write_control`と同じ共有規律）。
    system_info: Arc<SystemInfoSampler>,
    // 外部 DB 連携 S4: `GET /api/v1/status` の `sink` 節のため -
    // `admin_status_router`/`admin_tag_stream_router`/`sink_admin_router`
    // へ渡すものと**同じ** `Arc`。
    sink_status: Arc<SinkStatusStore>,
) -> Router {
    let state = TagSpaceState {
        manager: manager.clone(),
        controller: controller.clone(),
        write_control: write_control.clone(),
        mqtt,
        system_info,
        sink_status,
        computed: manager.computed_engine(),
        audit: audit.clone(),
    };
    let auth_state = TagSpaceAuthState {
        auth: auth.clone(),
        api_keys: api_keys.clone(),
        audit,
        manager: manager.clone(),
    };
    let write_state = WriteState {
        manager: manager.clone(),
        collection_controller: enforce_collection_state.then_some(controller.clone()),
        api_keys,
        write_audit,
        write_control,
        rate_limiter,
        events,
    };

    let read_router = Router::new()
        .route("/api/v1/tags", get(v1_tags))
        .route("/api/v1/values", get(v1_values))
        .route("/api/v1/values/{tag}", get(v1_value_single))
        // T20 ①b(docs/banto-hub-t20-design.md §3.1): read-on-demand -
        // 静的セグメント `read-now` は axum(matchit)のルーティングで
        // `{tag}`より深い階層なので `/api/v1/values/{tag}` とは衝突しない
        // (T20-3a の `/api/v1/values/batch` と同じ「静的セグメント優先」の
        // 理屈)。
        .route("/api/v1/values/{tag}/read-now", get(v1_value_read_now))
        .route("/api/v1/status", get(v1_status))
        .route("/api/v1/events", get(v1_events))
        // T1（設計 §5.2・§5.6の9番）: 認証は他の /api/v1/* と同一の
        // require_tag_space_auth（read スコープ必須）- アップグレード
        // リクエスト自体が普通の HTTP GET なので、この `.layer` がそのまま
        // 効く（`crate::stream` 側の doc comment 参照）。
        .route("/api/v1/stream", get(crate::stream::ws_upgrade))
        .with_state(state);

    let write_router = Router::new()
        .route("/api/v1/values/{tag}", post(v1_write_value))
        // T20-3a: 静的セグメント `batch` は axum(matchit)のルーティングで
        // `{tag}` パラメータより優先して一致する - `"batch"`という外部名の
        // タグは作れない(`banto_tags`のタグ名バリデーションに依存しない、
        // ルーティング層の静的優先で保証される)ので衝突しない。
        .route("/api/v1/values/batch", post(v1_write_values_batch))
        .with_state(write_state);

    read_router
        .merge(write_router)
        .layer(middleware::from_fn_with_state(
            auth_state,
            require_tag_space_auth,
        ))
}
