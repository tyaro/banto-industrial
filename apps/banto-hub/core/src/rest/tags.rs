//! タグの CRUD（`/api/tags`）・一括登録/更新/削除（`/api/tags/batch*`）・
//! `tag_registry_router`（接続・グループ・タグの I1 ルーター）、およびタグの
//! 作成・更新ペイロード（`TagPayload`）。
//!
//! `rest.rs` から中身を変えずに移したもの（docs/banto-hub-driver-refactor-plan.md §8.1、
//! P0-1 の手順 4）。I1 CRUD の共通層は `rest/crud.rs` にあり、認証・ガード・監査の補助は
//! 親の `rest.rs` にある。どちらも `use super::{...}` で使う。外から使われるパス
//! （`crate::rest::X`）は親の re-export で保つ。

use banto_tags::{offset_tag_address, parse_tag_address, AddressProbe, OffsetUnit};

use super::collection_groups::{
    collection_groups_create, collection_groups_delete, collection_groups_describe,
    collection_groups_get, collection_groups_list, collection_groups_update,
};
use super::diagnostics::{range_diagnostic_get, range_diagnostic_post};
use super::drivers::drivers_list;
use super::plc_connections::{
    plc_connections_create, plc_connections_delete, plc_connections_get, plc_connections_list,
    plc_connections_test, plc_connections_test_saved, plc_connections_update,
};
use super::{
    broadcast, commit_catalog_and_notify, get, json, middleware, post, preflight_transaction,
    queue_pending_registry_change, record_write, registry_before, registry_change_should_queue,
    remove_tag_from_all_sink_groups, require_editor, require_session, storage_api_error,
    tags_expression_check, tags_expression_functions, ApiError, Arc, AuditLogService, AuthGate,
    AuthState, BantoError, BatchTagDeleteOutcome, BatchTagOutcome, BatchTagUpdateOutcome,
    CollectionController, CollectionGroupService, CollectorManager, CommissioningState,
    Deserialize, FieldError, GroupTagCount, HeaderMap, IntoResponse, Json, ListParams, ListResult,
    OperationKind, Path, PendingChangesService, PlcConnectionService, RegistryMutationError,
    RegistryMutationResult, Response, Router, Serialize, ServerEvent, State, StatusCode, Tag,
    TagInput, TagRegistryState, TagService,
};

fn default_tag_decimals() -> i64 {
    0
}

/// T20 ①a: [`TagPayload::string_encoding`]'s serde default - mirrors
/// `banto_tags::tag`'s own `default_string_encoding()` (private to that
/// crate), so an existing client's payload (written before this field
/// existed) still deserializes and creates a UTF-8-encoded tag, the same
/// "old payload still works" treatment as `writable`/`tag_kind`.
fn default_tag_string_encoding() -> String {
    "utf8".to_string()
}

/// T2-3: mirrors `banto_tags::tag`'s own `default_tag_kind` (not reused
/// directly - that one is private to `banto-tags`) so a `TagPayload` missing
/// `tagKind` builds the same `"plc"` `TagInput` an old client always got.
fn default_tag_kind() -> String {
    "plc".to_string()
}

/// タグの作成・更新の wire 形（REST の `POST`/`PUT /api/tags`・一括 API・
/// MCP の `create_tag`/`update_tag`・未適用キューの保存形が共有する）。
///
/// **しきい値は持たない**（#533、2026-10-08 オーナー決定: しきい値 H/HH/L/LL は
/// 使う側（記録計・SCADA）が持つ設定で、Hub は持たず警報を判定しない）。
/// `thresholdH` などを**値付きで**送ってきたクライアント（#533 より前の画面・
/// 設定パッケージ・MCP クライアントなど）は、黙って捨てずに検証エラーで断る
/// （[`TagPayload::reject_thresholds`]。ChronoGazer の `TagPayload`（#532）と
/// 同じ作法）。`null` は通す（#533 より前の画面は空欄を `null` で送っていた）。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TagPayload {
    pub name: String,
    pub collection_group_id: i64,
    pub address: String,
    pub data_type: String,
    #[serde(default)]
    pub string_length: Option<i64>,
    /// T20 ①a（docs/banto-hub-t20-design.md §3.1、2026-09-04 オーナー決定
    /// 「文字コードは既定 UTF-8、タグ単位で Shift-JIS も選択可」）。
    /// `#[serde(default = "default_tag_string_encoding")]`（= `"utf8"`）
    /// なので既存クライアントのペイロードは無変更で通る（`writable`等と
    /// 同じ「旧ペイロード互換」の作法）。管理 UI にエンコーディング選択
    /// フォームを足すのは①aでは任意 - このフィールドは型として通すことを
    /// 優先し、値は API 経由で直接指定できる。
    #[serde(default = "default_tag_string_encoding")]
    pub string_encoding: String,
    #[serde(default)]
    pub raw_lo: Option<f64>,
    #[serde(default)]
    pub raw_hi: Option<f64>,
    #[serde(default)]
    pub eng_lo: Option<f64>,
    #[serde(default)]
    pub eng_hi: Option<f64>,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default = "default_tag_decimals")]
    pub decimals: i64,
    /// #533: 受け取るのは「値付きで送られたら断る」ためだけ
    /// （[`TagPayload::reject_thresholds`]）。タグには渡さず、未適用キューの
    /// 保存形にも書かない（`skip_serializing`）。
    #[serde(default, skip_serializing)]
    pub threshold_h: Option<f64>,
    #[serde(default, skip_serializing)]
    pub threshold_hh: Option<f64>,
    #[serde(default, skip_serializing)]
    pub threshold_l: Option<f64>,
    #[serde(default, skip_serializing)]
    pub threshold_ll: Option<f64>,
    #[serde(default)]
    pub enabled: bool,
    /// T2-3 (docs/tag-server-design.md §10-2/§6 item 1): `#[serde(default)]`
    /// (= `false`) so an existing API client's payload (written before this
    /// field existed) still deserializes and creates a non-writable tag,
    /// exactly the pre-T2 behaviour (design §10-2: "既存の API クライアント
    /// のペイロードは無変更で通る").
    #[serde(default)]
    pub writable: bool,
    #[serde(default = "default_tag_kind")]
    pub tag_kind: String,
    #[serde(default)]
    pub expression: Option<String>,
    #[serde(default)]
    pub retain: bool,
    /// T18-1（docs/banto-hub-desktop-plan.md §9.4 TAG-UX-C 4点目「revision /
    /// ETag で後勝ち上書きを防ぐ」）: 編集画面が最後に取得した
    /// `Tag::revision`。`#[serde(default)]`（= `None`）なので既存クライアント
    /// のペイロードは無変更で通る（revision チェック無しの互換動作 -
    /// `banto_tags::TagInput::expected_revision`のドキュメント参照）。管理 UI
    /// （`tags/+page.svelte`）は編集フォームを開いた時点の `selected.revision`
    /// を常に送る。
    #[serde(default)]
    pub expected_revision: Option<i64>,
}

/// [`TagPayload::reject_thresholds`] の案内（画面・MCP の応答にそのまま出る）。
pub const TAG_THRESHOLDS_NOT_HELD_MESSAGE: &str =
    "banto-hub はしきい値を持ちません（警報の判定は PLC・記録計・SCADA の側で行います）";

impl TagPayload {
    /// #533: しきい値を値付きで送ってきたら断る（黙って捨てると、送った側は
    /// 設定できたと思い込む）。タグの作成・更新の入口（REST の単票・一括、
    /// MCP）で、未適用キューに積む**前**に呼ぶ。
    pub fn reject_thresholds(&self) -> Result<(), BantoError> {
        let field_errors = self.threshold_field_errors();
        if field_errors.is_empty() {
            Ok(())
        } else {
            Err(BantoError::Validation { field_errors })
        }
    }

    /// [`Self::reject_thresholds`] の中身（一括 API は行ごとのエラーに並べる）。
    fn threshold_field_errors(&self) -> Vec<FieldError> {
        [
            ("thresholdLl", self.threshold_ll),
            ("thresholdL", self.threshold_l),
            ("thresholdH", self.threshold_h),
            ("thresholdHh", self.threshold_hh),
        ]
        .into_iter()
        .filter(|(_, value)| value.is_some())
        .map(|(field, _)| FieldError {
            field: field.to_string(),
            message: TAG_THRESHOLDS_NOT_HELD_MESSAGE.to_string(),
        })
        .collect()
    }
}

impl From<TagPayload> for TagInput {
    fn from(payload: TagPayload) -> Self {
        Self {
            name: payload.name,
            collection_group_id: payload.collection_group_id,
            address: payload.address,
            data_type: payload.data_type,
            string_length: payload.string_length,
            string_encoding: payload.string_encoding,
            raw_lo: payload.raw_lo,
            raw_hi: payload.raw_hi,
            eng_lo: payload.eng_lo,
            eng_hi: payload.eng_hi,
            unit: payload.unit,
            decimals: payload.decimals,
            enabled: payload.enabled,
            writable: payload.writable,
            tag_kind: payload.tag_kind,
            expression: payload.expression,
            retain: payload.retain,
            expected_revision: payload.expected_revision,
        }
    }
}

async fn tags_list(State(state): State<TagRegistryState>) -> Result<Json<Vec<Tag>>, ApiError> {
    Ok(Json(state.tags.list(ListParams::default()).await?.rows))
}

/// `POST /api/tags/list`（T18-5a 第2段、docs/banto-hub-t18-design.md §4
/// 決定6「薄い部品の先行配線」）: `write_audit_list`/banto の `audit_log_list` と同型の
/// 素通しハンドラ - `ListParams` をそのままサービスへ渡し `ListResult<Tag>`
/// を返すだけ。認可は `GET /api/tags` と同じくルーター全体の
/// `require_auth`（viewer 以上で読み取り可）のみで、`require_editor` は
/// 呼ばない（読み取り専用エンドポイントのため）。
async fn tags_list_query(
    State(state): State<TagRegistryState>,
    Json(params): Json<ListParams>,
) -> Result<Json<ListResult<Tag>>, ApiError> {
    Ok(Json(state.tags.list(params).await?))
}

/// `GET /api/tags/group-counts`（T18-5a 第2段、同 §4 決定6）: グループ別の
/// タグ件数集計。`GET /api/tags` と同じ読み取り専用エンドポイントなので
/// `require_auth` のみ。
async fn tags_group_counts(
    State(state): State<TagRegistryState>,
) -> Result<Json<Vec<GroupTagCount>>, ApiError> {
    Ok(Json(state.tags.count_by_group().await?))
}

/// `POST /api/tags/parse-addresses` の件数の上限。一括作成（`/api/tags/batch`）には
/// サーバー側の件数の上限が無く（本文の大きさだけ）、画面の CSV の上限
/// （`tagCsv.ts` の `MAX_CSV_ROWS` = 10000 行）が実質の上限なので、1 回の呼び出しで
/// CSV 1 本分の全行（同じ接続の行をまとめて 1 回、設計 §7.2）を受けられる値にする。
const MAX_PARSE_ADDRESS_ITEMS: usize = 10_000;

/// [`tags_parse_addresses`] の要求の 1 行。`tagKind` は必須（保存と同じ種類ごとの規則を
/// 当てるため、2026-10-22 オーナーのレビュー P2）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ParseAddressItem {
    tag_kind: String,
    address: String,
    data_type: String,
    #[serde(default)]
    string_length: Option<i64>,
    #[serde(default = "default_tag_string_encoding")]
    string_encoding: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ParseAddressesRequest {
    connection_id: i64,
    items: Vec<ParseAddressItem>,
}

/// 1 行の結果。`area`・`writableArea`・`index`・`bit`・`wordCount` は `plc` のときだけ値を持つ
/// （それ以外は `null`）。
#[derive(Debug, Serialize)]
#[serde(untagged)]
enum ParseAddressResult {
    Ok(ParseAddressOk),
    Err(ParseAddressErr),
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ParseAddressOk {
    ok: bool,
    canonical: String,
    area: Option<String>,
    writable_area: Option<bool>,
    index: Option<u64>,
    bit: Option<u8>,
    word_count: Option<u16>,
}

#[derive(Debug, Serialize)]
struct ParseAddressErr {
    ok: bool,
    message: String,
}

#[derive(Debug, Serialize)]
struct ParseAddressesResponse {
    items: Vec<ParseAddressResult>,
}

/// `POST /api/tags/parse-addresses`（R2b-2d、docs/banto-hub-driver-address-design.md §7.1）:
/// 保存と**同じ**関数（[`banto_tags::parse_tag_address`] = 項目の検証 →
/// `normalize_tag_address`）に、保存の入口が使うのと同じドライバのレジストリ
/// （`CollectorManager::registry()`。`CollectorManager::tags()` が `TagService::with_registry`
/// に渡すもの）を渡して、接続のプロトコルの下でのアドレスの正規形と領域の情報を返す。
/// 画面の「書き込み可」の既定（`writableDefault.ts`）と CSV の更新の分類（`tagCsvDiff.ts`）が
/// 文法を TS に写さずに使う。
///
/// `/api/tags` の読み取りと同じく副作用が無いので `require_auth`（viewer 以上）だけで
/// `require_editor` は呼ばない。`plc_connections_test` と同じ理由で意図的に
/// `#[utoipa::path]` を付けず `ApiDoc` にも加えない（`/api/v1/*` だけを文書化する既存方針）。
///
/// 行ごとの失敗は 200 のまま `ok: false` + `message`（複数の項目エラーは `；` でつなぐ）。
/// 判定不能を「安全」に読ませない: 呼び出し側は `ok: false`・通信の失敗・応答の欠落を
/// すべて「判定不能」（書き込み可の既定を当てない）として扱う。
async fn tags_parse_addresses(
    State(state): State<TagRegistryState>,
    Json(body): Json<ParseAddressesRequest>,
) -> Result<Json<ParseAddressesResponse>, ApiError> {
    if body.items.len() > MAX_PARSE_ADDRESS_ITEMS {
        return Err(ApiError(BantoError::Validation {
            field_errors: vec![FieldError {
                field: "items".to_string(),
                message: format!("items は {MAX_PARSE_ADDRESS_ITEMS} 件以下にしてください"),
            }],
        }));
    }
    let connection = state.plc_connections.get(body.connection_id).await?;
    let registry = state.manager.registry();
    let items = body
        .items
        .iter()
        .map(|item| {
            let probe = AddressProbe {
                tag_kind: &item.tag_kind,
                address: &item.address,
                data_type: &item.data_type,
                string_length: item.string_length,
                string_encoding: &item.string_encoding,
            };
            match parse_tag_address(&connection.name, &connection.protocol, &probe, registry) {
                Ok(normalized) => {
                    let parsed = normalized.parsed.as_ref();
                    ParseAddressResult::Ok(ParseAddressOk {
                        ok: true,
                        canonical: normalized.canonical.clone(),
                        area: parsed.map(|p| p.area.clone()),
                        writable_area: parsed.map(|p| p.writable_area),
                        index: parsed.and_then(|p| p.index),
                        bit: parsed.and_then(|p| p.bit),
                        word_count: parsed.map(|p| p.word_count),
                    })
                }
                Err(errors) => address_err(&errors),
            }
        })
        .collect();
    Ok(Json(ParseAddressesResponse { items }))
}

fn address_err(errors: &[FieldError]) -> ParseAddressResult {
    ParseAddressResult::Err(ParseAddressErr {
        ok: false,
        message: errors
            .iter()
            .map(|e| e.message.as_str())
            .collect::<Vec<_>>()
            .join("；"),
    })
}

fn default_tag_kind_plc() -> String {
    "plc".to_string()
}

/// [`tags_offset_addresses`] の要求の 1 行。接続は項目ごと（設計 §4.3: 絞り込み「すべて」で
/// SLMP と Modbus のタグが 1 回の要求に混ざる）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OffsetAddressItem {
    connection_id: i64,
    #[serde(default = "default_tag_kind_plc")]
    tag_kind: String,
    address: String,
    data_type: String,
    #[serde(default)]
    string_length: Option<i64>,
    #[serde(default = "default_tag_string_encoding")]
    string_encoding: String,
    n: i64,
    unit: OffsetAddressUnit,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum OffsetAddressUnit {
    Points,
    Words,
}

#[derive(Debug, Deserialize)]
struct OffsetAddressesRequest {
    items: Vec<OffsetAddressItem>,
}

/// `POST /api/tags/offset-addresses`（R2b-6d-1、docs/banto-hub-driver-ui-design.md §4.3）:
/// 各項目の接続のドライバでアドレスを `n` 点（`points`、連続登録）または `n` ワード
/// （`words`、オフセットコピー・構造体登録）動かす。検証と動かした後の再検証は保存と同じ
/// 関数（[`banto_tags::offset_tag_address`] → [`banto_tags::parse_tag_address`]）で、応答は
/// [`tags_parse_addresses`] と同じ形・同じ順（動かした後のアドレスの解析結果）。接続が無い・
/// ドライバの接続でない（postgres / push / virtual）・範囲外は、その項目だけ `ok: false`。
/// 権限と文書化の扱いも [`tags_parse_addresses`] と同じ。
async fn tags_offset_addresses(
    State(state): State<TagRegistryState>,
    Json(body): Json<OffsetAddressesRequest>,
) -> Result<Json<ParseAddressesResponse>, ApiError> {
    if body.items.len() > MAX_PARSE_ADDRESS_ITEMS {
        return Err(ApiError(BantoError::Validation {
            field_errors: vec![FieldError {
                field: "items".to_string(),
                message: format!("items は {MAX_PARSE_ADDRESS_ITEMS} 件以下にしてください"),
            }],
        }));
    }
    let registry = state.manager.registry();
    // 接続は要求の中で重複するので id ごとに 1 回だけ引く（無い接続は None）。
    let mut connections: std::collections::HashMap<i64, Option<banto_tags::PlcConnection>> =
        std::collections::HashMap::new();
    let mut items = Vec::with_capacity(body.items.len());
    for item in &body.items {
        if let std::collections::hash_map::Entry::Vacant(slot) =
            connections.entry(item.connection_id)
        {
            let found = match state.plc_connections.get(item.connection_id).await {
                Ok(connection) => Some(connection),
                Err(BantoError::NotFound { .. }) => None,
                Err(error) => return Err(ApiError(error)),
            };
            slot.insert(found);
        }
        let Some(Some(connection)) = connections.get(&item.connection_id) else {
            items.push(ParseAddressResult::Err(ParseAddressErr {
                ok: false,
                message: format!("接続 {} が見つかりません", item.connection_id),
            }));
            continue;
        };
        let probe = AddressProbe {
            tag_kind: &item.tag_kind,
            address: &item.address,
            data_type: &item.data_type,
            string_length: item.string_length,
            string_encoding: &item.string_encoding,
        };
        let unit = match item.unit {
            OffsetAddressUnit::Points => OffsetUnit::Points,
            OffsetAddressUnit::Words => OffsetUnit::Words,
        };
        items.push(
            match offset_tag_address(
                &connection.name,
                &connection.protocol,
                &probe,
                registry,
                item.n,
                unit,
            ) {
                Ok(normalized) => {
                    let parsed = normalized.parsed.as_ref();
                    ParseAddressResult::Ok(ParseAddressOk {
                        ok: true,
                        canonical: normalized.canonical.clone(),
                        area: parsed.map(|p| p.area.clone()),
                        writable_area: parsed.map(|p| p.writable_area),
                        index: parsed.and_then(|p| p.index),
                        bit: parsed.and_then(|p| p.bit),
                        word_count: parsed.map(|p| p.word_count),
                    })
                }
                Err(errors) => address_err(&errors),
            },
        );
    }
    Ok(Json(ParseAddressesResponse { items }))
}

async fn tags_get(
    State(state): State<TagRegistryState>,
    Path(id): Path<i64>,
) -> Result<Json<Tag>, ApiError> {
    Ok(Json(state.tags.get(id).await?))
}

async fn tags_create(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Json(input): Json<TagPayload>,
) -> RegistryMutationResult<Response> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "tags",
        "POST",
        "/api/tags",
    )
    .await?;
    input.reject_thresholds().map_err(ApiError)?;
    if let Some(status) = registry_change_should_queue(&state.controller, &state.commissioning) {
        return queue_pending_registry_change(
            &state,
            &headers,
            "tags.create",
            json!({ "input": input }),
            status,
        )
        .await;
    }
    // 読み取りを挟む書き込みなので BEGIN IMMEDIATE（checklist §5 / sink/service.rs の doc）。
    let mut tx = state
        .manager
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(storage_api_error)?;
    // #635: 書き込みの前の registry。書き込みで新しく生まれた外部名の重なりだけを断る。
    let registry_before = match registry_before(&mut tx, state.manager.registry()).await {
        Ok(before) => before,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(err.into());
        }
    };
    let created = match state.tags.create_tx(&mut tx, input.into()).await {
        Ok(created) => created,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(ApiError(err).into());
        }
    };
    // #341 レビュー対応: この snapshot は**保存前検証（preflight）専用**。
    // catalog へ反映するのは `CollectionController::commit_catalog_and_apply_live`
    // が transition ロックの下で読み直す最新の snapshot（同 fn の doc
    // comment「古い snapshot の窓」節）。
    let _preflighted = match preflight_transaction(&mut tx, &registry_before).await {
        Ok(snapshot) => snapshot,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(err.into());
        }
    };
    tx.commit().await.map_err(storage_api_error)?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "create",
        "tags",
        Some(&created.id.to_string()),
        Some(json!({ "name": created.name, "enabled": created.enabled })),
    )
    .await;
    commit_catalog_and_notify(&state.controller, &state.events, "tags")
        .await
        .map_err(RegistryMutationError::LiveApplyFailed)?;
    Ok(Json(created).into_response())
}

/// #528 の回帰テスト専用の割り込み点（本番では存在しない）。`tags_update` が
/// トランザクションを始めた直後に、(1) 読み取りを 1 回してスナップショットを
/// 張り、(2) テストへ知らせ、(3) テストが許すまで止まる。止まっている間、
/// `BEGIN IMMEDIATE` ならハンドラが書き込みロックを握っており、素の `BEGIN` なら
/// 握っていない（スナップショットだけ）ので、テストは別接続からそれを直接確かめる。
/// 待ち時間に頼らないための仕掛け（`display_groups` の `ReadPause` と同じ考え方）。
#[cfg(test)]
pub(crate) struct TxTestHook {
    pub(crate) reached: tokio::sync::Notify,
    pub(crate) proceed: tokio::sync::Notify,
}

#[cfg(test)]
tokio::task_local! {
    pub(crate) static TX_TEST_HOOK: std::sync::Arc<TxTestHook>;
}

#[cfg(test)]
async fn tx_test_hook(tx: &mut sqlx::SqliteConnection) {
    if let Ok(hook) = TX_TEST_HOOK.try_with(|hook| hook.clone()) {
        let _ = sqlx::query("SELECT COUNT(*) FROM tags")
            .fetch_one(&mut *tx)
            .await;
        hook.reached.notify_one();
        hook.proceed.notified().await;
    }
}

async fn tags_update(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(input): Json<TagPayload>,
) -> RegistryMutationResult<Response> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "tags",
        "PUT",
        "/api/tags/{id}",
    )
    .await?;
    input.reject_thresholds().map_err(ApiError)?;
    if let Some(status) = registry_change_should_queue(&state.controller, &state.commissioning) {
        return queue_pending_registry_change(
            &state,
            &headers,
            "tags.update",
            json!({ "id": id, "input": input }),
            status,
        )
        .await;
    }
    // 読み取りを挟む書き込みなので BEGIN IMMEDIATE（checklist §5 / sink/service.rs の doc）。
    let mut tx = state
        .manager
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(storage_api_error)?;
    // #635: 書き込みの前の registry。書き込みで新しく生まれた外部名の重なりだけを断る。
    let registry_before = match registry_before(&mut tx, state.manager.registry()).await {
        Ok(before) => before,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(err.into());
        }
    };
    #[cfg(test)]
    tx_test_hook(&mut tx).await;
    let updated = match state.tags.update_tx(&mut tx, id, input.into()).await {
        Ok(updated) => updated,
        // T18-1: `TagUpdateError::RevisionConflict` の場合もロールバックは
        // 必須 - preflight/catalog を一切見ないまま抜けるので、ここで
        // 既にトランザクションは何も commit していない状態だが、明示的に
        // rollback して SQLite の接続をトランザクション外に戻す
        // （`CollectionEditLocked` 等、既存の早期 return と同じ作法）。
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(err.into());
        }
    };
    // #341 レビュー対応: この snapshot は**保存前検証（preflight）専用**。
    // catalog へ反映するのは `CollectionController::commit_catalog_and_apply_live`
    // が transition ロックの下で読み直す最新の snapshot（同 fn の doc
    // comment「古い snapshot の窓」節）。
    let _preflighted = match preflight_transaction(&mut tx, &registry_before).await {
        Ok(snapshot) => snapshot,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(err.into());
        }
    };
    tx.commit().await.map_err(storage_api_error)?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "update",
        "tags",
        Some(&id.to_string()),
        Some(json!({ "name": updated.name, "enabled": updated.enabled })),
    )
    .await;
    commit_catalog_and_notify(&state.controller, &state.events, "tags")
        .await
        .map_err(RegistryMutationError::LiveApplyFailed)?;
    Ok(Json(updated).into_response())
}

async fn tags_delete(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> RegistryMutationResult<Response> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "tags",
        "DELETE",
        "/api/tags/{id}",
    )
    .await?;
    if let Some(status) = registry_change_should_queue(&state.controller, &state.commissioning) {
        return queue_pending_registry_change(
            &state,
            &headers,
            "tags.delete",
            json!({ "id": id }),
            status,
        )
        .await;
    }
    // 読み取りを挟む書き込みなので BEGIN IMMEDIATE（checklist §5 / sink/service.rs の doc）。
    let mut tx = state
        .manager
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(storage_api_error)?;
    // #635: 書き込みの前の registry。書き込みで新しく生まれた外部名の重なりだけを断る。
    let registry_before = match registry_before(&mut tx, state.manager.registry()).await {
        Ok(before) => before,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(err.into());
        }
    };
    if let Err(err) = state.tags.delete_tx(&mut tx, id).await {
        let _ = tx.rollback().await;
        return Err(ApiError(err).into());
    }
    // 外部 DB 連携 S4（`crate::sink`のモジュール doc comment「FK を張らない
    // 理由」参照）: `hub_sink_group_tags`は`tags(id)`へ FK を張っていない
    // ため、単体タグ削除の経路では能動的にメンバーシップ行を消す。
    if let Err(err) = remove_tag_from_all_sink_groups(&mut tx, id).await {
        let _ = tx.rollback().await;
        return Err(ApiError(err).into());
    }
    // #341 レビュー対応: この snapshot は**保存前検証（preflight）専用**。
    // catalog へ反映するのは `CollectionController::commit_catalog_and_apply_live`
    // が transition ロックの下で読み直す最新の snapshot（同 fn の doc
    // comment「古い snapshot の窓」節）。
    let _preflighted = match preflight_transaction(&mut tx, &registry_before).await {
        Ok(snapshot) => snapshot,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(err.into());
        }
    };
    tx.commit().await.map_err(storage_api_error)?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "delete",
        "tags",
        Some(&id.to_string()),
        None,
    )
    .await;
    commit_catalog_and_notify(&state.controller, &state.events, "tags")
        .await
        .map_err(RegistryMutationError::LiveApplyFailed)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

// --- T11-1 一括登録 API (docs/ux-plan.md §3): 連続登録 UI と T11-2 の CSV
// インポートが共有する基盤 - transaction内検証 → all-or-nothing 適用 →
// catalog commit 1回。
// パターン展開（名前パターン/連続アドレス生成）はクライアント側（TS、
// `apps/banto-hub/src/lib/banto/continuousRegistration.ts`）が担い、この
// エンドポイントは展開済みの `TagInput` 配列を受け取るだけの汎用一括 API
// のまま保つ（設計: 「展開結果を一括 API に渡す方式」）。

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchTagsRequest {
    pub tags: Vec<TagPayload>,
    /// `true`: 検証結果だけを返す（DB 無変更）。`false`（既定）: 検証 →
    /// 単一トランザクションで全件 INSERT → catalog commit を1回。
    #[serde(default)]
    pub dry_run: bool,
}

// Note: none of the three response types below derive `utoipa::ToSchema` -
// `banto_tags::Tag` (embedded in `BatchTagsResponse::tags`) does not
// implement it, and (like `IssuedApiKeyResponse`/`GrpcSettingsBody` above)
// this admin-surface endpoint is not part of `ApiDoc`'s documented
// `/api/v1/*` schema anyway (see this module's doc comment: only the
// machine-facing tag-space API is utoipa-documented).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchTagFieldErrorResponse {
    field: String,
    message: String,
}

impl From<FieldError> for BatchTagFieldErrorResponse {
    fn from(err: FieldError) -> Self {
        Self {
            field: err.field,
            message: err.message,
        }
    }
}

/// 行番号(0起点)付きのフィールドエラー一覧 - 設計「行番号/インデックス
/// 付きのエラー一覧」。CSV インポート(T11-2)ではこの `index` がそのまま
/// CSV の行番号(ヘッダ行を除く0起点データ行)に対応する想定。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchTagRowErrorResponse {
    index: usize,
    field_errors: Vec<BatchTagFieldErrorResponse>,
}

impl From<banto_tags::BatchTagError> for BatchTagRowErrorResponse {
    fn from(err: banto_tags::BatchTagError) -> Self {
        Self {
            index: err.index,
            field_errors: err.field_errors.into_iter().map(Into::into).collect(),
        }
    }
}

/// `POST /api/tags/batch` の応答。**常に 200** で返す(判断: 2026-08-07) -
/// 「1件でも不正なら全体拒否」という結果は例外ではなく、dry run の検証
/// レポートと地続きの通常の応答だから。認証/権限エラー(401/403)や DB
/// レベルの想定外エラー(500)は既存の `ApiError` 経路(非2xx + `ErrorBody`)
/// のまま区別する。`ok: false` のときクライアントは `errors` を行ごとに
/// 表示する(`apps/banto-hub/src/lib/banto/tagRegistryAdmin.ts` 参照)。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchTagsResponse {
    ok: bool,
    dry_run: bool,
    /// 適用された(または dry run で適用されたはずの)件数。`ok: false` の
    /// ときは常に 0。
    count: usize,
    errors: Vec<BatchTagRowErrorResponse>,
    /// `ok: true && dry_run: false` のときだけ `Some`(実際に作成された
    /// タグ)。dry run では何も書き込まないので `None`。
    #[serde(skip_serializing_if = "Option::is_none")]
    tags: Option<Vec<Tag>>,
}

/// `POST /api/tags/batch` - T11-1。editor 以上、CSRF は `/api/tags/*` と
/// 同じ管理系ルーターの層で一括適用される(`tag_registry_router` 参照)。
async fn tags_batch(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Json(body): Json<BatchTagsRequest>,
) -> RegistryMutationResult<Response> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "tags",
        "POST",
        "/api/tags/batch",
    )
    .await?;

    // #533: しきい値を値付きで送った行は、未適用キューに積む前・dry run でも
    // 行ごとのエラーとして返す（「常に 200、`ok: false` で行ごとエラー」の契約）。
    let threshold_errors: Vec<BatchTagRowErrorResponse> = body
        .tags
        .iter()
        .enumerate()
        .filter_map(|(index, tag)| {
            let field_errors = tag.threshold_field_errors();
            (!field_errors.is_empty()).then(|| BatchTagRowErrorResponse {
                index,
                field_errors: field_errors.into_iter().map(Into::into).collect(),
            })
        })
        .collect();
    if !threshold_errors.is_empty() {
        return Ok(Json(BatchTagsResponse {
            ok: false,
            dry_run: body.dry_run,
            count: 0,
            errors: threshold_errors,
            tags: None,
        })
        .into_response());
    }

    if !body.dry_run {
        if let Some(status) = registry_change_should_queue(&state.controller, &state.commissioning)
        {
            return queue_pending_registry_change(
                &state,
                &headers,
                "tags.batch_create",
                json!({ "dryRun": false, "tags": body.tags }),
                status,
            )
            .await;
        }
    }

    let dry_run = body.dry_run;
    let inputs: Vec<TagInput> = body.tags.into_iter().map(Into::into).collect();

    if inputs.is_empty() {
        return Ok(Json(BatchTagsResponse {
            ok: true,
            dry_run,
            count: 0,
            errors: Vec::new(),
            tags: (!dry_run).then(Vec::new),
        })
        .into_response());
    }

    // 読み取りを挟む書き込みなので BEGIN IMMEDIATE（checklist §5 / sink/service.rs の doc）。

    let mut tx = state
        .manager
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(storage_api_error)?;
    // #635: 書き込みの前の registry。書き込みで新しく生まれた外部名の重なりだけを断る。
    let registry_before = match registry_before(&mut tx, state.manager.registry()).await {
        Ok(before) => before,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(err.into());
        }
    };
    let outcome = match state.tags.create_batch_tx(&mut tx, &inputs).await {
        Ok(outcome) => outcome,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(ApiError(err).into());
        }
    };
    match outcome {
        BatchTagOutcome::Invalid(errors) => {
            let _ = tx.rollback().await;
            Ok(Json(BatchTagsResponse {
                ok: false,
                dry_run,
                count: 0,
                errors: errors.into_iter().map(Into::into).collect(),
                tags: None,
            })
            .into_response())
        }
        BatchTagOutcome::Valid { count, tags } => {
            // #341 レビュー対応: 上記と同じ - preflight 専用。
            let _preflighted = match preflight_transaction(&mut tx, &registry_before).await {
                Ok(snapshot) => snapshot,
                Err(err) => {
                    let _ = tx.rollback().await;
                    return Err(err.into());
                }
            };
            if dry_run {
                tx.rollback().await.map_err(storage_api_error)?;
            } else {
                tx.commit().await.map_err(storage_api_error)?;
                record_write(
                    &state.audit,
                    &state.auth,
                    &headers,
                    "batch_create",
                    "tags",
                    Some("-"),
                    Some(json!({ "count": count })),
                )
                .await;
                // T11-1 の核心: n 件でも catalog commit はここで1回だけ。
                commit_catalog_and_notify(&state.controller, &state.events, "tags")
                    .await
                    .map_err(RegistryMutationError::LiveApplyFailed)?;
            }
            Ok(Json(BatchTagsResponse {
                ok: true,
                dry_run,
                count,
                errors: Vec::new(),
                tags: if dry_run { None } else { tags },
            })
            .into_response())
        }
    }
}

// --- T18-3b 一括更新 API (bulk tag operations): tags_batch (T11-1) の
// update-side 対（apps/banto-hub/core/src/rest.rs doc 冒頭ではなくこの
// セクション直下に配置 - tags_batch のリクエスト/レスポンス封筒・
// all-or-nothing・catalog commit 1回という骨格をそのまま流用する）。
// 用途: (1) 一括 enabled 切り替え、(2) 一括グループ移動
// （collectionGroupId 付け替え）。一括削除は対象外。

/// [`TagPayload`] に `id`（更新対象）を足しただけの一括更新1行分のペイロード。
/// `#[serde(flatten)]` で `TagPayload` の全フィールド（`expectedRevision`
/// 込み）をそのまま JSON 直下に展開する - 単票 PUT の body と全く同じ形
/// （`{ name, address, ..., expectedRevision }`）に `id` だけが乗る。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TagBatchUpdatePayload {
    pub id: i64,
    #[serde(flatten)]
    pub input: TagPayload,
}

impl From<TagBatchUpdatePayload> for (i64, TagInput) {
    fn from(payload: TagBatchUpdatePayload) -> Self {
        (payload.id, payload.input.into())
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchTagsUpdateRequest {
    pub tags: Vec<TagBatchUpdatePayload>,
    /// `true`: 検証結果だけを返す（DB 無変更）。`false`（既定）: 検証 →
    /// 単一トランザクションで全件 UPDATE → catalog commit を1回
    /// （`BatchTagsRequest::dry_run` と同じ意味）。
    #[serde(default)]
    pub dry_run: bool,
}

/// 行番号(0起点)付きのフィールドエラー一覧 - [`BatchTagRowErrorResponse`]
/// と同じ形だが、更新対象の行は既に `id` を持っているので併記する
/// （クライアントが `index` だけでなく `id` でも行を突き合わせられる）。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchTagUpdateRowErrorResponse {
    index: usize,
    id: i64,
    field_errors: Vec<BatchTagFieldErrorResponse>,
}

impl From<banto_tags::BatchTagUpdateError> for BatchTagUpdateRowErrorResponse {
    fn from(err: banto_tags::BatchTagUpdateError) -> Self {
        Self {
            index: err.index,
            id: err.id,
            field_errors: err.field_errors.into_iter().map(Into::into).collect(),
        }
    }
}

/// `POST /api/tags/batch-update` の応答。[`BatchTagsResponse`]（T11-1）と
/// 同じ「常に 200、`ok: false` で行ごとエラー」契約 - あちらの doc comment
/// 参照。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchTagsUpdateResponse {
    ok: bool,
    dry_run: bool,
    /// 適用された(または dry run で適用されたはずの)件数。`ok: false` の
    /// ときは常に 0。
    count: usize,
    errors: Vec<BatchTagUpdateRowErrorResponse>,
    /// `ok: true && dry_run: false` のときだけ `Some`(実際に更新されたタグ)。
    #[serde(skip_serializing_if = "Option::is_none")]
    tags: Option<Vec<Tag>>,
}

/// `POST /api/tags/batch-update` - T18-3b（bulk tag operations）。editor
/// 以上、`tags_batch`（T11-1）の骨格をそのまま写した update 版:
/// トランザクション内検証 → all-or-nothing 適用 → catalog commit 1回。
/// revision 競合（[`TagUpdateError::RevisionConflict`] が単票更新で返す
/// `409`）もここでは行単位エラーとして集約する - 一括操作の「どれか stale
/// なら全体 ok:false・無書込」という all-or-nothing 契約を、単票の
/// 「対象1行だけ 409 で弾く」より優先するため（design: T18-3b 一括操作）。
async fn tags_batch_update(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Json(body): Json<BatchTagsUpdateRequest>,
) -> RegistryMutationResult<Response> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "tags",
        "POST",
        "/api/tags/batch-update",
    )
    .await?;

    // #533: `tags_batch` と同じ（しきい値を値付きで送った行は行ごとのエラー）。
    let threshold_errors: Vec<BatchTagUpdateRowErrorResponse> = body
        .tags
        .iter()
        .enumerate()
        .filter_map(|(index, tag)| {
            let field_errors = tag.input.threshold_field_errors();
            (!field_errors.is_empty()).then(|| BatchTagUpdateRowErrorResponse {
                index,
                id: tag.id,
                field_errors: field_errors.into_iter().map(Into::into).collect(),
            })
        })
        .collect();
    if !threshold_errors.is_empty() {
        return Ok(Json(BatchTagsUpdateResponse {
            ok: false,
            dry_run: body.dry_run,
            count: 0,
            errors: threshold_errors,
            tags: None,
        })
        .into_response());
    }

    if !body.dry_run {
        if let Some(status) = registry_change_should_queue(&state.controller, &state.commissioning)
        {
            return queue_pending_registry_change(
                &state,
                &headers,
                "tags.batch_update",
                json!({ "dryRun": false, "tags": body.tags }),
                status,
            )
            .await;
        }
    }

    let dry_run = body.dry_run;
    let updates: Vec<(i64, TagInput)> = body.tags.into_iter().map(Into::into).collect();

    if updates.is_empty() {
        return Ok(Json(BatchTagsUpdateResponse {
            ok: true,
            dry_run,
            count: 0,
            errors: Vec::new(),
            tags: (!dry_run).then(Vec::new),
        })
        .into_response());
    }

    // 読み取りを挟む書き込みなので BEGIN IMMEDIATE（checklist §5 / sink/service.rs の doc）。

    let mut tx = state
        .manager
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(storage_api_error)?;
    // #635: 書き込みの前の registry。書き込みで新しく生まれた外部名の重なりだけを断る。
    let registry_before = match registry_before(&mut tx, state.manager.registry()).await {
        Ok(before) => before,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(err.into());
        }
    };
    let outcome = match state.tags.update_batch_tx(&mut tx, &updates).await {
        Ok(outcome) => outcome,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(ApiError(err).into());
        }
    };
    match outcome {
        BatchTagUpdateOutcome::Invalid(errors) => {
            let _ = tx.rollback().await;
            Ok(Json(BatchTagsUpdateResponse {
                ok: false,
                dry_run,
                count: 0,
                errors: errors.into_iter().map(Into::into).collect(),
                tags: None,
            })
            .into_response())
        }
        BatchTagUpdateOutcome::Valid { count, tags } => {
            // #341 レビュー対応: 上記と同じ - preflight 専用。
            let _preflighted = match preflight_transaction(&mut tx, &registry_before).await {
                Ok(snapshot) => snapshot,
                Err(err) => {
                    let _ = tx.rollback().await;
                    return Err(err.into());
                }
            };
            if dry_run {
                tx.rollback().await.map_err(storage_api_error)?;
            } else {
                tx.commit().await.map_err(storage_api_error)?;
                record_write(
                    &state.audit,
                    &state.auth,
                    &headers,
                    "batch_update",
                    "tags",
                    Some("-"),
                    Some(json!({ "count": count })),
                )
                .await;
                // T18-3b の核心 (tags_batch/T11-1 と同じ): n 件でも catalog
                // commit はここで1回だけ。
                commit_catalog_and_notify(&state.controller, &state.events, "tags")
                    .await
                    .map_err(RegistryMutationError::LiveApplyFailed)?;
            }
            Ok(Json(BatchTagsUpdateResponse {
                ok: true,
                dry_run,
                count,
                errors: Vec::new(),
                tags: if dry_run { None } else { tags },
            })
            .into_response())
        }
    }
}

// --- T19 S2-c1 一括削除 API (bulk tag delete, UX-37): tags_batch_update
// (T18-3b) の削除版として骨格をそのまま写す - 稼働中は pending 1件で 202、
// 停止中は単一トランザクションで all-or-nothing 削除 → catalog commit
// 1回。実際の削除ロジックは `banto_tags::TagService::delete_batch_tx`
// （単票 `delete_tx` と同じ判定を id ごとに適用するだけ - 一括経路だけの
// 新しい削除セマンティクスは持たない、そのメソッドの doc comment 参照）。

/// `POST /api/tags/batch-delete` のリクエスト。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchTagsDeleteRequest {
    pub ids: Vec<i64>,
}

/// 行番号(0起点)付きのフィールドエラー一覧 - [`BatchTagUpdateRowErrorResponse`]
/// と全く同じ形（`banto_tags::BatchTagDeleteError`が
/// `banto_tags::BatchTagUpdateError`と同じ形を持つことの写し）。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchTagDeleteRowErrorResponse {
    index: usize,
    id: i64,
    field_errors: Vec<BatchTagFieldErrorResponse>,
}

impl From<banto_tags::BatchTagDeleteError> for BatchTagDeleteRowErrorResponse {
    fn from(err: banto_tags::BatchTagDeleteError) -> Self {
        Self {
            index: err.index,
            id: err.id,
            field_errors: err.field_errors.into_iter().map(Into::into).collect(),
        }
    }
}

/// `POST /api/tags/batch-delete` の応答。[`BatchTagsUpdateResponse`]と同じ
/// 「常に 200、`ok: false` で行ごとエラー」契約だが、`dryRun`/`tags` は
/// 持たない - `tags_batch_delete`の doc comment 参照（削除に事前検証
/// プレビューの意味が薄いため dry run 自体を設けていない）。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchTagsDeleteResponse {
    ok: bool,
    /// 削除された(または `ok: false` なら常に0の)件数。
    count: usize,
    errors: Vec<BatchTagDeleteRowErrorResponse>,
}

/// `POST /api/tags/batch-delete` - T19 S2-c1（UX-37 一括削除）。editor
/// 以上、`tags_batch_update`の骨格をそのまま写した削除版:
/// トランザクション内検証 → all-or-nothing 適用 → catalog commit 1回。
///
/// `dryRun` は設けない（実装指示、doc 冒頭参照） - 一括更新の dry run は
/// 「差分を確認してから確定する」ためのものだが、削除は差分ではなく
/// リクエストの `ids` そのものが対象そのものであり、対象件数・内訳は
/// クライアント側（選択済みタグの一覧）で既に分かっている。サーバー
/// 往復で検証結果を見せる段階を挟む理由がないため、常に即時適用
/// （収集稼働中は他の書き込みと同じく pending キュー）の一段構成にした。
async fn tags_batch_delete(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Json(body): Json<BatchTagsDeleteRequest>,
) -> RegistryMutationResult<Response> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "tags",
        "POST",
        "/api/tags/batch-delete",
    )
    .await?;

    if let Some(status) = registry_change_should_queue(&state.controller, &state.commissioning) {
        return queue_pending_registry_change(
            &state,
            &headers,
            "tags.batch_delete",
            json!({ "ids": body.ids }),
            status,
        )
        .await;
    }

    let ids = body.ids;
    if ids.is_empty() {
        return Ok(Json(BatchTagsDeleteResponse {
            ok: true,
            count: 0,
            errors: Vec::new(),
        })
        .into_response());
    }

    // 読み取りを挟む書き込みなので BEGIN IMMEDIATE（checklist §5 / sink/service.rs の doc）。

    let mut tx = state
        .manager
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(storage_api_error)?;
    // #635: 書き込みの前の registry。書き込みで新しく生まれた外部名の重なりだけを断る。
    let registry_before = match registry_before(&mut tx, state.manager.registry()).await {
        Ok(before) => before,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(err.into());
        }
    };
    let outcome = match state.tags.delete_batch_tx(&mut tx, &ids).await {
        Ok(outcome) => outcome,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(ApiError(err).into());
        }
    };
    match outcome {
        BatchTagDeleteOutcome::Invalid(errors) => {
            let _ = tx.rollback().await;
            Ok(Json(BatchTagsDeleteResponse {
                ok: false,
                count: 0,
                errors: errors.into_iter().map(Into::into).collect(),
            })
            .into_response())
        }
        BatchTagDeleteOutcome::Valid { count } => {
            // 外部 DB 連携 S4: `tags_delete`と同じ理由 - all-or-nothing で
            // 消えた`ids`全件について、対応するメンバーシップ行も消す。
            for id in &ids {
                if let Err(err) = remove_tag_from_all_sink_groups(&mut tx, *id).await {
                    let _ = tx.rollback().await;
                    return Err(ApiError(err).into());
                }
            }
            // #341 レビュー対応: 上記と同じ - preflight 専用。
            let _preflighted = match preflight_transaction(&mut tx, &registry_before).await {
                Ok(snapshot) => snapshot,
                Err(err) => {
                    let _ = tx.rollback().await;
                    return Err(err.into());
                }
            };
            tx.commit().await.map_err(storage_api_error)?;
            // 削除件数を detail に含める（T19 S2-b の cascade_delete と同じ
            // 考え方 - 監査ログから「何件消えたか」が読めるようにする）。
            record_write(
                &state.audit,
                &state.auth,
                &headers,
                "batch_delete",
                "tags",
                Some("-"),
                Some(json!({ "count": count })),
            )
            .await;
            // tags_batch/tags_batch_update と同じ核心: n 件でも catalog
            // commit はここで1回だけ。
            commit_catalog_and_notify(&state.controller, &state.events, "tags")
                .await
                .map_err(RegistryMutationError::LiveApplyFailed)?;
            Ok(Json(BatchTagsDeleteResponse {
                ok: true,
                count,
                errors: Vec::new(),
            })
            .into_response())
        }
    }
}

/// `/api/plc-connections/*` + `/api/collection-groups/*` + `/api/tags/*`
/// (viewer-read / editor-write) - `relay-wright-core::rest::tag_registry_router`
/// を雛形に、書き込み成功後に catalog commit と SSE通知を行う。
#[allow(clippy::too_many_arguments)]
pub(super) fn tag_registry_router(
    plc_connections: PlcConnectionService,
    collection_groups: CollectionGroupService,
    tags: TagService,
    pending_changes: PendingChangesService,
    audit: AuditLogService,
    auth: AuthState,
    commissioning: CommissioningState,
    manager: Arc<CollectorManager>,
    controller: Arc<CollectionController>,
    events: broadcast::Sender<ServerEvent>,
) -> Router {
    let state = TagRegistryState {
        plc_connections,
        collection_groups,
        tags,
        auth: auth.clone(),
        commissioning: commissioning.clone(),
        audit,
        manager,
        controller: controller.clone(),
        events,
        pending_changes,
    };
    Router::new()
        .route(
            "/api/plc-connections",
            get(plc_connections_list).post(plc_connections_create),
        )
        .route(
            "/api/plc-connections/{id}",
            get(plc_connections_get)
                .put(plc_connections_update)
                .delete(plc_connections_delete),
        )
        // T12 (docs/ux-plan.md §4): 保存前の接続テスト - `{id}` の下ではなく
        // 固定パスなので `/api/plc-connections/{id}` と衝突しない
        // (下の `/api/tags/batch` と同型のパス設計)。
        .route("/api/plc-connections/test", post(plc_connections_test))
        .route(
            "/api/plc-connections/{id}/diagnostics",
            get(range_diagnostic_get).post(range_diagnostic_post),
        )
        // S1a (docs/banto-hub-external-db-design.md §4.6): 保存済み接続の
        // 接続テスト - `{id}` の下の固定サブパスなので
        // `/api/plc-connections/{id}` の GET/PUT/DELETE とは axum のルート
        // マッチングで衝突しない。
        .route(
            "/api/plc-connections/{id}/test",
            post(plc_connections_test_saved),
        )
        .route(
            "/api/collection-groups",
            get(collection_groups_list).post(collection_groups_create),
        )
        .route(
            "/api/collection-groups/{id}",
            get(collection_groups_get)
                .put(collection_groups_update)
                .delete(collection_groups_delete),
        )
        // S3（docs/banto-hub-external-db-design.md §7 row S3）: 保存済み
        // グループの `query_sql` describe - `{id}` の下の固定サブパスなので
        // `/api/collection-groups/{id}` の GET/PUT/DELETE とは axum の
        // ルートマッチングで衝突しない（`/api/plc-connections/{id}/test`
        // と同じ形）。
        .route(
            "/api/collection-groups/{id}/describe",
            post(collection_groups_describe),
        )
        .route("/api/tags", get(tags_list).post(tags_create))
        .route(
            "/api/tags/{id}",
            get(tags_get).put(tags_update).delete(tags_delete),
        )
        // T11-1 (docs/ux-plan.md §3): 一括登録 - `/api/tags/{id}` の下では
        // なく `/api/tags` の下の固定パスなので、`{id}` (i64) パラメータと
        // 衝突しない。
        .route("/api/tags/batch", post(tags_batch))
        // T18-3b (bulk tag operations): 一括更新 - 同じ理由で `/api/tags`
        // 直下の固定パス（`/api/tags/batch` と衝突しない別セグメント）。
        .route("/api/tags/batch-update", post(tags_batch_update))
        // T19 S2-c1 (UX-37): 一括削除 - 同じ理由で `/api/tags` 直下の固定
        // パス（`/api/tags/batch`・`/api/tags/batch-update` と衝突しない
        // 別セグメント）。
        .route("/api/tags/batch-delete", post(tags_batch_delete))
        // T18-5a 第2段 (docs/banto-hub-t18-design.md §4 決定6): `ListParams`
        // 素通しのページング付き一覧 - `/api/tags/batch`・`/api/tags/batch-update`
        // と同じ理由で `/api/tags/{id}` (i64 パラメータ) と衝突しない固定
        // セグメント (axum の matchit は静的セグメント優先 - 統合テストで
        // `/api/tags/{id}` 側の解決も併せて確認する)。
        .route("/api/tags/list", post(tags_list_query))
        // 同じく固定セグメント。グループ別タグ件数集計。
        .route("/api/tags/group-counts", get(tags_group_counts))
        // R2b-2d: 同じく固定セグメント。アドレスの解析（保存と同じ関数、
        // `tags_parse_addresses` の doc comment 参照）。
        .route("/api/tags/parse-addresses", post(tags_parse_addresses))
        .route("/api/tags/offset-addresses", post(tags_offset_addresses))
        // R2b-6b（docs/banto-hub-driver-ui-design.md §4.1）: ドライバの一覧。読み取りなので
        // `require_session` だけ（`rest/drivers.rs` の doc）。
        .route("/api/drivers", get(drivers_list))
        // #342 段階A: 保存前の式検証（エラー位置・参照タグ解決・循環判定・
        // 現在値によるプレビュー試算）。同じく `/api/tags` 直下の固定セグ
        // メントで `/api/tags/{id}` (i64) とは衝突しない。
        .route("/api/tags/expression/check", post(tags_expression_check))
        // #342 段階B: 式欄のセグメント補完に出す組み込み関数表の配布
        // （`tags_expression_functions` の doc comment 参照）。`/api/tags`
        // 直下の固定セグメントなので `/api/tags/{id}` (i64) と衝突しない。
        .route(
            "/api/tags/expression/functions",
            get(tags_expression_functions),
        )
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            AuthGate {
                auth,
                operation: OperationKind::Normal,
            },
            require_session,
        ))
}
