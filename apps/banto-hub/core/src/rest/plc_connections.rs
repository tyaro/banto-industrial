//! PLC 接続の CRUD（`/api/plc-connections`）と接続テスト（保存前の疎通確認・保存済み
//! 接続のテスト）。
//!
//! `rest.rs` から中身を変えずに移したもの（docs/banto-hub-driver-refactor-plan.md §8.1、
//! P0-1 の手順 3）。I1 CRUD の共通層は `rest/crud.rs` にあり、認証・ガード・監査の補助は
//! 親の `rest.rs` にある。どちらも `use super::{...}` で使う。外から使われるパス
//! （`crate::rest::X`）は親の re-export で保つ。

use super::{
    commit_catalog_and_notify, default_payload_enabled, json, preflight_transaction,
    queue_pending_registry_change, record_write, registry_before, registry_change_should_queue,
    reject_delete_if_referenced_by_sink_group, require_editor, storage_api_error, Address,
    ApiError, BantoError, BatchReadRequest, BatchReadResult, BrokerError, CollectorManager,
    DataType, Deserialize, Duration, FieldError, HeaderMap, IntoResponse, Json, ListParams,
    ModbusTcpClient, ModbusTcpConfig, Path, PlcClient, PlcConnection, PlcConnectionInput, PlcError,
    ReadRequest, ReadResult, RegistryMutationError, RegistryMutationResult, Response, Serialize,
    SlmpClient, SlmpConfig, State, StatusCode, TagRegistryState,
};

fn default_plc_protocol() -> String {
    "modbus-tcp".to_string()
}

fn default_plc_unit_id() -> i64 {
    1
}

/// T9-2 (docs/ux-plan.md §1): a `PlcConnectionPayload` missing `simulation`
/// (an old client, or a create/update that never mentions it) keeps the
/// existing safe default - a connection is never accidentally simulated by
/// omission.
fn default_plc_simulation() -> bool {
    false
}

/// P3-b（監査指摘 2026-08-12）/ 2026-09-08 オーナー決定: a
/// `PlcConnectionPayload` missing `wordOrder` (an old client, or a
/// create/update that never mentions it) defers to
/// `banto_tags::plc_connection`'s **protocol-aware** default - `"high_low"`
/// for `modbus-tcp`, `"low_high"` for everything else - by handing it the
/// empty-string "unspecified" sentinel rather than a concrete order.
///
/// Returning `"low_high"` here (as this fn did until 2026-09-08) would mask
/// that default entirely: the payload would arrive at
/// `PlcConnectionInput` already filled in, so an omitted `wordOrder` on a
/// Modbus connection could never reach the `"high_low"` default the owner
/// decision requires. `""` is a safe sentinel because it is not in
/// `ALLOWED_WORD_ORDERS` and migration `0010`'s SQL `CHECK` rejects it, so
/// it was never a storable value.
fn default_plc_word_order() -> String {
    String::new()
}

/// Wire-shaped (camelCase) create/update payload for `plc_connections` -
/// copied from relay-wright's `PlcConnectionPayload` (invariant across every
/// app that exposes I1 over REST: one payload shape).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlcConnectionPayload {
    pub name: String,
    #[serde(default = "default_plc_protocol")]
    pub protocol: String,
    pub host: String,
    pub port: i64,
    #[serde(default = "default_plc_unit_id")]
    pub unit_id: i64,
    #[serde(default = "default_payload_enabled")]
    pub enabled: bool,
    /// T9-2 (docs/ux-plan.md §1, 「接続単位のシミュレーションモード」): opts
    /// this connection into an in-process simulator instead of a real PLC -
    /// see `banto_tags::PlcConnection::simulation`'s doc comment for what
    /// this actually does at collection time
    /// (`crates/banto-collect/src/simulation.rs`) and, for broker-routed
    /// connections specifically, `crate::broker_glue::BrokerSimRegistry`.
    #[serde(default = "default_plc_simulation")]
    pub simulation: bool,
    /// P3-b（監査指摘 2026-08-12）: ワード順（32bit値 u32/f32 等、および
    /// issue #325 で追加された 64bit 値 i64/u64/f64（4レジスタ）の上位/下位
    /// ワードの並び）。`"low_high"` / `"high_low"` のいずれか - 検証は
    /// `banto_tags::plc_connection::validate_plc_connection_input` 側
    /// （`ALLOWED_WORD_ORDERS`）に委ねる。
    ///
    /// 2026-09-08 オーナー決定: **modbus-tcp でも有効な設定**（既定は
    /// `"high_low"` = Modbus/IEEE 慣習、SLMP は `"low_high"` = MELSEC 標準）。
    /// それ以前は収集経路が Modbus のこの列を無視して `HighLow` 固定で動く
    /// 一方 read-on-demand/書き込み経路は列を読んでおり、同じタグの値が経路
    /// ごとにワード反転して食い違っていた。フォームも "slmp" 選択時しか
    /// 表示していなかったが、現在は modbus-tcp でも表示する。virtual/postgres
    /// 接続では引き続き無意味（`unit_id` と同じ扱い）。
    ///
    /// 省略時は [`default_plc_word_order`] の "unspecified" 番兵を経由して
    /// プロトコル依存の既定が適用される。
    #[serde(default = "default_plc_word_order")]
    pub word_order: String,
    /// #605: SLMP の CPU 種別（`"Q"` / `"R"`）。省略と空文字は
    /// `banto_tags::PlcConnectionInput::cpu_type` と同じ意味
    /// （作成は `"R"`、更新は既存の値を保つ）。未知の値は検証エラー。
    #[serde(default)]
    pub cpu_type: Option<String>,
    /// S1（docs/banto-hub-external-db-design.md §4.1）: `protocol: "postgres"`
    /// 接続の DB 名。他プロトコルでは省略（`None`）のままにする -
    /// `banto_tags::plc_connection::validate_plc_connection_input`が
    /// postgres 以外での指定を拒否する。
    #[serde(default)]
    pub database: Option<String>,
    /// S1: `protocol: "postgres"` 接続の DB ユーザー名。[`Self::database`]と
    /// 同じ省略時ルール。
    #[serde(default)]
    pub username: Option<String>,
    /// S1: `protocol: "postgres"` 接続のパスワード（平文保存、§2.2）。
    /// **create と update で意味が異なる** -
    /// `banto_tags::PlcConnectionInput::password`の doc comment 参照:
    /// - create: 省略/`null`/空文字列はいずれも「パスワード無し」。
    /// - update: 省略/`null`は「現在のパスワードを変更しない」、空文字列は
    ///   「消去」、それ以外の文字列は「置き換え」。
    ///
    /// GET/list のレスポンスにはこのフィールドは無く、代わりに
    /// [`PlcConnectionResponse::password_set`]（bool）を返す - 平文
    /// パスワードを一度保存した接続を読み返すだけで露出させないため。
    #[serde(default)]
    pub password: Option<String>,
}

impl From<PlcConnectionPayload> for PlcConnectionInput {
    fn from(payload: PlcConnectionPayload) -> Self {
        Self {
            name: payload.name,
            protocol: payload.protocol,
            host: payload.host,
            port: payload.port,
            unit_id: payload.unit_id,
            enabled: payload.enabled,
            // T9-2: wired through - see `PlcConnectionPayload::simulation`'s
            // doc comment.
            simulation: payload.simulation,
            // P3-b: wired through - see `PlcConnectionPayload::word_order`'s
            // doc comment.
            word_order: payload.word_order,
            // #605: 省略は None のままサービスへ渡す（作成は R、更新は既存を保つ）。
            cpu_type: payload.cpu_type,
            // S1: wired through verbatim - see `PlcConnectionPayload::database`/
            // `::username`/`::password`'s doc comments for the (create vs.
            // update) semantics of `password` specifically.
            database: payload.database,
            username: payload.username,
            password: payload.password,
        }
    }
}

/// #383 段階3 P2（2026-10-10 オーナー決定 H1）: banto-tags のレジストリは
/// `protocol: "hub"`（banto-hub をデータ源とする接続、ChronoGazer が Hub 経由の
/// タグを記録するための種別）を受け付けるが、**banto-hub 自身はそれを作らない**。
/// Hub が自分自身を購読することは無く、Hub の収集（`banto_collect::build_config_from`
/// の厳格版）も `hub` 接続を「未対応」として全体を止める。接続を作る・変える
/// すべての入口 - REST の作成/更新（[`plc_connections_create`]/
/// [`plc_connections_update`]）、MCP の `create_connection`/`update_connection`、
/// 未適用キューの適用（[`execute_pending_apply`] の `plc_connections.*`）- が
/// キューへ積む**前に**これを呼び、`protocol` の項目エラーとして拒否する
/// （キューに積んでから適用時に落ちるのでは、どの変更が悪いかが遅れて分かる）。
/// 設定パッケージの取り込み（`configPackage.ts`）は REST の作成/更新を呼ぶので、
/// ここで同じく拒否される。
pub(crate) fn reject_hub_connection_protocol(
    payload: &PlcConnectionPayload,
) -> Result<(), BantoError> {
    if payload.protocol != banto_tags::HUB_PROTOCOL {
        return Ok(());
    }
    Err(BantoError::Validation {
        field_errors: vec![FieldError {
            field: "protocol".to_string(),
            message: "banto-hub では Hub 経由の接続（hub）は作成できません\
                      （Hub 経由の接続は記録計などの Hub を使う側の設定です）"
                .to_string(),
        }],
    })
}

/// S1（docs/banto-hub-external-db-design.md §4.1・§2.2）: GET/list が返す
/// `plc_connections` の読み取り DTO。`banto_tags::PlcConnection`をそのまま
/// `Serialize`すると`password`列（平文）がそのまま応答に出てしまうため、
/// 代わりにこの型へ変換してから返す - `password`は決して外部へ出さず、
/// [`Self::password_set`]（非空パスワードが保存されているかの bool）だけを
/// 返す。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlcConnectionResponse {
    pub id: i64,
    pub name: String,
    pub protocol: String,
    pub host: String,
    pub port: i64,
    pub unit_id: i64,
    pub enabled: bool,
    pub simulation: bool,
    pub word_order: String,
    /// #605: `"Q"` または `"R"`。SLMP 以外の行も列は持つ（既定 `"R"`）。
    pub cpu_type: String,
    pub database: Option<String>,
    pub username: Option<String>,
    /// S1: true のとき、この接続には非空のパスワードが保存されている
    /// （値そのものは返さない）。
    pub password_set: bool,
}

impl From<PlcConnection> for PlcConnectionResponse {
    fn from(conn: PlcConnection) -> Self {
        let password_set = conn.password.as_deref().is_some_and(|s| !s.is_empty());
        Self {
            id: conn.id,
            name: conn.name,
            protocol: conn.protocol,
            host: conn.host,
            port: conn.port,
            unit_id: conn.unit_id,
            enabled: conn.enabled,
            simulation: conn.simulation,
            word_order: conn.word_order,
            cpu_type: conn.cpu_type.clone(),
            database: conn.database,
            username: conn.username,
            password_set,
        }
    }
}

pub(super) async fn plc_connections_list(
    State(state): State<TagRegistryState>,
) -> Result<Json<Vec<PlcConnectionResponse>>, ApiError> {
    Ok(Json(
        state
            .plc_connections
            .list(ListParams::default())
            .await?
            .rows
            .into_iter()
            .map(PlcConnectionResponse::from)
            .collect(),
    ))
}

pub(super) async fn plc_connections_get(
    State(state): State<TagRegistryState>,
    Path(id): Path<i64>,
) -> Result<Json<PlcConnectionResponse>, ApiError> {
    Ok(Json(state.plc_connections.get(id).await?.into()))
}

pub(super) async fn plc_connections_create(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Json(input): Json<PlcConnectionPayload>,
) -> RegistryMutationResult<Response> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "plc_connections",
        "POST",
        "/api/plc-connections",
    )
    .await?;
    // #383 段階3 P2: キューへ積む前に拒否する（`reject_hub_connection_protocol`）。
    if let Err(err) = reject_hub_connection_protocol(&input) {
        return Err(ApiError(err).into());
    }
    if let Some(status) = registry_change_should_queue(&state.controller, &state.commissioning) {
        return queue_pending_registry_change(
            &state,
            &headers,
            "plc_connections.create",
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
    let registry_before = match registry_before(&mut tx).await {
        Ok(before) => before,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(err.into());
        }
    };
    let created = match state.plc_connections.create_tx(&mut tx, input.into()).await {
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
        "plc_connections",
        Some(&created.id.to_string()),
        Some(json!({ "name": created.name, "enabled": created.enabled })),
    )
    .await;
    commit_catalog_and_notify(&state.controller, &state.events, "plc_connections")
        .await
        .map_err(RegistryMutationError::LiveApplyFailed)?;
    Ok(Json(PlcConnectionResponse::from(created)).into_response())
}

pub(super) async fn plc_connections_update(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(input): Json<PlcConnectionPayload>,
) -> RegistryMutationResult<Response> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "plc_connections",
        "PUT",
        "/api/plc-connections/{id}",
    )
    .await?;
    // #383 段階3 P2: キューへ積む前に拒否する（`reject_hub_connection_protocol`）。
    if let Err(err) = reject_hub_connection_protocol(&input) {
        return Err(ApiError(err).into());
    }
    if let Some(status) = registry_change_should_queue(&state.controller, &state.commissioning) {
        return queue_pending_registry_change(
            &state,
            &headers,
            "plc_connections.update",
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
    let registry_before = match registry_before(&mut tx).await {
        Ok(before) => before,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(err.into());
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
        "update",
        "plc_connections",
        Some(&id.to_string()),
        Some(json!({ "name": updated.name, "enabled": updated.enabled })),
    )
    .await;
    commit_catalog_and_notify(&state.controller, &state.events, "plc_connections")
        .await
        .map_err(RegistryMutationError::LiveApplyFailed)?;
    Ok(Json(PlcConnectionResponse::from(updated)).into_response())
}

pub(super) async fn plc_connections_delete(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> RegistryMutationResult<Response> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "plc_connections",
        "DELETE",
        "/api/plc-connections/{id}",
    )
    .await?;
    if let Some(status) = registry_change_should_queue(&state.controller, &state.commissioning) {
        return queue_pending_registry_change(
            &state,
            &headers,
            "plc_connections.delete",
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
    let registry_before = match registry_before(&mut tx).await {
        Ok(before) => before,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(err.into());
        }
    };
    // 外部 DB 連携 S4（docs/banto-hub-external-db-design.md §5.2、
    // `crate::sink`のモジュール doc comment「FK を張らない理由」参照）:
    // この接続を参照する sink group が1件でもあれば削除を拒否する
    // （RESTRICT 相当）。`banto_tags`はこの接続の下にある Hub 専用
    // テーブルを知らないため、`cascade_delete_tx`を呼ぶ**前**にここで
    // チェックする。
    if let Err(err) = reject_delete_if_referenced_by_sink_group(&mut tx, id).await {
        let _ = tx.rollback().await;
        return Err(ApiError(err).into());
    }
    // T19 S2-b（UX-38、docs/banto-hub-t19-design.md §3.4・§7.5、2026-09-02
    // オーナー決定）: 「タグがあってもグループ・接続を削除可。定義のみ削除
    // し、履歴は残す」- `delete_tx`（子が居れば拒否）ではなく
    // `cascade_delete_tx`（配下のグループ・タグごと1トランザクションで削除、
    // `banto_tags::plc_connection::PlcConnectionService::cascade_delete_tx`
    // の doc comment参照）を使う。`banto-tstore` の履歴データはこのクレート
    // が触らない別ストレージなので無傷のまま残る。予約接続（calc/mem）は
    // このカスケード経路でも従来どおり削除できない。
    let cascade = match state.plc_connections.cascade_delete_tx(&mut tx, id).await {
        Ok(cascade) => cascade,
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
        "delete",
        "plc_connections",
        Some(&id.to_string()),
        Some(json!({
            "cascade": {
                "deletedGroups": cascade.deleted_groups,
                "deletedTags": cascade.deleted_tags,
            },
        })),
    )
    .await;
    commit_catalog_and_notify(&state.controller, &state.events, "plc_connections")
        .await
        .map_err(RegistryMutationError::LiveApplyFailed)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `POST /api/plc-connections/{id}/test` - S1a
/// （docs/banto-hub-external-db-design.md §4.6・§7 スライス表「S1」:
/// 「接続テストが `SELECT 1`〈実際は `SELECT version()`〉と列一覧を
/// 返す」の PostgreSQL 側実装。**保存済み**の接続 `id` を対象にする点で
/// T12 の `POST /api/plc-connections/test`（保存前のフォーム値を直接
/// 受け取る、PLC 専用）とは別物 - 名前を分けて共存させている
/// (`plc_connections_test`が既存、こちらは`plc_connections_test_saved`)。
///
/// 認可は他の `plc_connections` 書き込みハンドラ（create/update/delete）と
/// 同じ`require_editor`に揃える - このリソースの REST 面は既に role
/// ベースの editor ゲートで統一されており（T21 の MCP 側だけが admin
/// スコープを使う別体系）、ここだけ新しいゲートを持ち込まない。
///
/// S1a では `protocol == "postgres"` の接続のみ対応する
/// （[`banto_tags::PlcConnection::is_db_source`]）。それ以外は`400`で
/// 「S1では postgres のみ対応」と明示する - `ok:false`の200ではなく
/// 明確なリクエストエラーにする（T12の`test_connection`が`virtual`/
/// `simulation`を`ok:false`の200で表現するのとは意図的に区別: あちらは
/// 「テスト対象として意味を成さないが正常な入力」、こちらは「この
/// エンドポイントがそもそも対応しないプロトコル」）。
///
/// レジストリへの書き込みは一切発生しない読み取り専用の疎通確認なので、
/// `record_write`/`commit_catalog_and_notify`は呼ばない -
/// `plc_connections_test`（T12）と同じ判断。
pub(super) async fn plc_connections_test_saved(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Response, ApiError> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "plc_connections",
        "POST",
        "/api/plc-connections/{id}/test",
    )
    .await?;

    let conn = state.plc_connections.get(id).await?;
    if !conn.is_db_source() {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "message": "S1ではpostgres接続のみ接続テストに対応しています。",
            })),
        )
            .into_response());
    }

    let outcome = crate::db_source::test_connection(&conn).await;
    Ok((StatusCode::OK, Json(outcome)).into_response())
}

// --- T12 (docs/ux-plan.md §4): 保存前の接続テスト ---------------------------
//
// `POST /api/plc-connections/test` は保存不要でホスト/ポート/プロトコルの
// 疎通を確認する - TCP 接続だけでなく実プロトコルで軽い読み出し1回
// (先頭デバイス1点)まで行うことで、ポートは開いているがプロトコル不一致、
// という誤設定も検出する(§4 の設計方針)。レジストリへの書き込みが一切
// 発生しない読み取り専用の疎通確認なので、`record_write`/`rebuild_and_notify`
// は呼ばない。
//
// 重要な制約(実機 R08ENCPU・オムロン KM-D1-ETN、`crates/banto-broker/src/lib.rs`
// のモジュール doc、`crate::broker_glue`のモジュール doc「Session sync
// policy」節参照): 三菱 SLMP は対象ポートが既に別の接続で使用中だと同じ
// ポートへの2本目を受け付けない(2026-08-07 実機確認: ポート毎に1接続、CPU
// 側で複数ポートを開けていれば複数同時セッションは可能)。Modbus TCP も
// #337 で収集読み取りが broker セッションに相乗りするようになった結果、
// KM-D1-ETN(KANC-718B 12-1)のような1対1接続しか受け付けない機器では同じ
// 制約が生じる(2026-09-08 オーナー決定)。そのため、保存済み接続
// (`connectionId`あり)のテストはプロトコルを問わず、既存の broker
// セッションが生きていればそれを再利用して読み、無い場合のみ直接ダイヤル
// する([`test_slmp_connection`]/[`test_modbus_connection`]参照)。

/// 接続テストの疎通確認に使うタイムアウト(接続・応答とも共通)。数秒固定
/// (ux-plan.md §4「タイムアウトは短め（数秒）に固定」)。
const PLC_TEST_TIMEOUT: Duration = Duration::from_secs(3);

/// Modbus 直接ダイヤル失敗時に付けるセッション上限ヒント - 「軽く付けてよい」
/// 扱い(実機依存が明確でないため断定しない)。
const MODBUS_SESSION_HINT: &str =
    " 対象PLCが既に別セッションと接続中の場合、機種によっては同時接続数の上限により失敗することがあります。";

/// SLMP 直接ダイヤル失敗時に付けるセッション上限ヒント - 実機 R08ENCPU で
/// 「対象ポートが既に別の接続で使用中」だと2本目を受け付けない実測がある
/// ため必須ヒントとする。
const SLMP_SESSION_HINT: &str = " 対象ポートが既に別の接続(この hub の収集や他アプリ)で使用中の可能性があります。SLMPは同一ポートへの2本目の接続を受け付けないことがあります(実機R08ENCPUで確認済み)。";

/// `POST /api/plc-connections/test` のリクエストボディ - 保存前のフォーム値を
/// そのまま受け取る(`PlcConnectionPayload`とは別型: 接続 id を持たないのが
/// 通常で、保存済み接続の編集中のみ`connectionId`を添える)。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlcConnectionTestPayload {
    pub protocol: String,
    pub host: String,
    pub port: i64,
    #[serde(default = "default_plc_unit_id")]
    pub unit_id: i64,
    /// フォームの「シミュレーションモード」チェックボックスの現在値。
    /// シミュレーション接続のテストは常に内蔵シミュレータへ繋がり無意味なので
    /// 拒否する(ux-plan.md §4「protocol: virtual と simulation: true 相当の
    /// テストは明示エラー」)。保存済み行の simulation フラグではなく、
    /// フォームの現在値をそのまま送らせる設計(未保存の編集中でも即座に弾ける
    /// ようにするため)。
    #[serde(default)]
    pub simulation: bool,
    /// 保存済み接続を編集中にテストする場合のみ送る。broker 経由の既存
    /// セッション判定にのみ使う(接続情報自体は上記 host/port/protocol/unitId
    /// を毎回使う - フォームの現在入力値をテストするのがこの API の目的の
    /// ため)。
    #[serde(default)]
    pub connection_id: Option<i64>,
    /// #605: 直接ダイヤルの SLMP フレームに使う CPU 種別。省略・`"R"` は
    /// iQ-R、`"Q"` は Q シリーズ。未知の値はダイヤルせず失敗にする。
    /// 既存セッションを再利用する経路で、保存済みセッションの CPU と食い違う
    /// ときは試さずに「保存してからテストしてください」と返す（2 本目の TCP を
    /// 張らない既存の制約があるため、直接ダイヤルには切り替えない）。
    #[serde(default)]
    pub cpu_type: Option<String>,
}

/// `POST /api/plc-connections/test` の応答。`ToSchema` は付けない - 下記
/// `plc_connections_test`のdoc comment参照。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlcConnectionTestResponse {
    pub ok: bool,
    pub elapsed_ms: u64,
    pub error: Option<PlcConnectionTestError>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlcConnectionTestError {
    /// "tcp" | "timeout" | "protocol" | "device" | "unsupported"
    pub kind: String,
    /// 日本語、対処ヒント込み。
    pub message: String,
}

/// #605: 接続テストの直接ダイヤルが使う CPU。省略・空・`"R"` は iQ-R、
/// `"Q"` は Q シリーズ。それ以外はダイヤルせずエラーにする
/// （収集側の「手編集行は R に倒す」とは違い、画面から来た未知の値は拒否する）。
fn slmp_cpu_from_test_payload(
    cpu_type: &Option<String>,
) -> Result<banto_plc::SlmpCpu, PlcConnectionTestError> {
    match cpu_type.as_deref() {
        None | Some("") | Some("R") => Ok(banto_plc::SlmpCpu::R),
        Some("Q") => Ok(banto_plc::SlmpCpu::Q),
        Some(other) => Err(PlcConnectionTestError {
            kind: "protocol".to_string(),
            message: format!("CPU種別が不正です（Q または R を指定してください）: {other}"),
        }),
    }
}

/// [`PlcError`]を[`PlcConnectionTestError`]に分類する(Modbus・SLMP直接
/// ダイヤル共用、`crates/banto-plc/src/error.rs`の各バリアント参照)。
/// `hint`が`Some`のとき、分類結果の`kind`が`"tcp"`または`"timeout"`のときだけ
/// 末尾に付加する(それ以外の kind には無関係な文言なので付けない) -
/// 呼び出し側は「セッション上限ヒントを付けたい経路」でだけ`Some`を渡す。
/// broker 経由の既存セッション再利用経路(`test_slmp_connection`の前半)は
/// このヒントが無関係( 「2本目をダイヤルしない」ことそのものが対策なので)
/// なので常に`None`を渡す。
fn classify_plc_error(err: &PlcError, hint: Option<&str>) -> PlcConnectionTestError {
    let (kind, message) = match err {
        PlcError::ConnectTimeout(_) => (
            "timeout",
            "接続タイムアウトです(3秒)。ホスト/ポート、ネットワーク到達性を確認してください。"
                .to_string(),
        ),
        PlcError::ResponseTimeout => (
            "timeout",
            "応答タイムアウトです(3秒)。接続はできましたが応答がありませんでした。".to_string(),
        ),
        PlcError::Connection(msg) => (
            "tcp",
            format!("TCP接続に失敗しました(ポートが閉じている、または到達できません): {msg}"),
        ),
        PlcError::Protocol(msg) => (
            "protocol",
            format!(
                "プロトコルエラー: 応答が不正です({msg})。プロトコル選択やポート番号が実機と一致しているか確認してください。"
            ),
        ),
        PlcError::ModbusException { message, .. } | PlcError::SlmpEndCode { message, .. } => (
            "device",
            format!(
                "デバイス読み出しエラー: 接続はできましたが、指定したデバイス/レジスタを読み出せませんでした({message})。アドレス設定や機種依存の可能性があります(致命的ではありません)。"
            ),
        ),
        // NotConnected / InvalidAddress / AddressProtocolMismatch /
        // UnsupportedCombination / StringSpanUnsupported - このAPIは固定
        // アドレス("40001"/"D0")しか使わないので通常発生しない防御的ケース。
        other => ("device", format!("読み出しに失敗しました: {other}")),
    };
    let mut message = message;
    if matches!(kind, "tcp" | "timeout") {
        if let Some(hint) = hint {
            message.push_str(hint);
        }
    }
    PlcConnectionTestError {
        kind: kind.to_string(),
        message,
    }
}

/// Modbus TCP の接続テスト。`payload.connection_id`があり、その接続の broker
/// セッションが既に生きていれば、それを再利用して読む(新規ダイヤルしない -
/// [`test_slmp_connection`]と同じ対策)。無ければ直接ダイヤルにフォールバック
/// する(ポート/ユニットIDの範囲検証 → `ModbusTcpClient::connect` → 保持
/// レジスタ先頭1点の`read_batch` → 必ず`disconnect`、の順)。
///
/// 2026-09-08 オーナー決定: #337 で Modbus TCP の収集読み取りも broker
/// セッションへ相乗りするようになり、Modbus も1接続=1ソケットになった。
/// SLMP の R08ENCPU と同様、1対1接続しか受け付けない機器(オムロン
/// KM-D1-ETN、KANC-718B 12-1、#337 参照)では、収集稼働中に接続テストを
/// 押すと2本目のダイヤルが拒否され、収集は正常なのに「失敗」と誤診断
/// されていた。SLMP と同じセッション再利用へ揃えてこれを解消する。
async fn test_modbus_connection(
    manager: &CollectorManager,
    payload: &PlcConnectionTestPayload,
) -> (bool, Option<PlcConnectionTestError>) {
    if let Some(connection_id) = payload.connection_id {
        if let Some(handle) = manager.sessions().handle_for(connection_id) {
            let requests = vec![BatchReadRequest::Numeric(ReadRequest {
                address: Address::parse("40001").expect("valid literal"),
                data_type: DataType::U16,
            })];
            // `ReadOnlyHandle::read`自体には外側タイムアウトが無いため、
            // ここで明示的に包む(`test_slmp_connection`と同じ)。
            return match tokio::time::timeout(PLC_TEST_TIMEOUT, handle.read(requests)).await {
                Err(_elapsed) => (
                    false,
                    Some(PlcConnectionTestError {
                        kind: "timeout".to_string(),
                        message: "応答タイムアウトです(3秒)。共有セッションが応答しませんでした。"
                            .to_string(),
                    }),
                ),
                Ok(Err(BrokerError::Disconnected { .. })) => (
                    false,
                    Some(PlcConnectionTestError {
                        kind: "tcp".to_string(),
                        message: "この接続の共有セッションは現在切断中です(再接続待機中)。PLCの電源やネットワーク、または他アプリとのセッション競合を確認してください。"
                            .to_string(),
                    }),
                ),
                Ok(Err(BrokerError::ConnectionFailed { reason, .. })) => (
                    false,
                    Some(PlcConnectionTestError {
                        kind: "tcp".to_string(),
                        message: format!("共有セッションが接続断で失敗しました: {reason}"),
                    }),
                ),
                Ok(Err(BrokerError::TaskGone { .. })) => (
                    false,
                    Some(PlcConnectionTestError {
                        kind: "tcp".to_string(),
                        message: "内部エラー: セッションタスクが終了しています。".to_string(),
                    }),
                ),
                // 防御的フォールバック: この経路では通常発生しない
                // (UnsupportedProtocol/InvalidPortはensure_connection時点で
                // 弾かれているはず)。
                Ok(Err(err @ BrokerError::UnsupportedProtocol { .. }))
                | Ok(Err(err @ BrokerError::InvalidPort { .. })) => (
                    false,
                    Some(PlcConnectionTestError {
                        kind: "protocol".to_string(),
                        message: err.to_string(),
                    }),
                ),
                // 既存セッション再利用経路: セッション上限ヒントは付けない
                // (design の意図: 「2本目をダイヤルしない」こと自体が対策)。
                Ok(Ok(mut results)) => match results.pop() {
                    Some(BatchReadResult::Value(_)) => (true, None),
                    Some(BatchReadResult::Bad(err)) => {
                        (false, Some(classify_plc_error(&err, None)))
                    }
                    None => (
                        false,
                        Some(PlcConnectionTestError {
                            kind: "device".to_string(),
                            message: "読み出し結果が空でした。".to_string(),
                        }),
                    ),
                },
            };
        }
    }

    // 直接ダイヤル(connectionId が無い、またはセッションが見つからない場合)。
    let port = match u16::try_from(payload.port) {
        Ok(port) => port,
        Err(_) => {
            return (
                false,
                Some(PlcConnectionTestError {
                    kind: "tcp".to_string(),
                    message: "ポート番号が不正です(1〜65535の範囲で指定してください)。".to_string(),
                }),
            );
        }
    };
    let unit_id = match u8::try_from(payload.unit_id) {
        Ok(unit_id) => unit_id,
        Err(_) => {
            return (
                false,
                Some(PlcConnectionTestError {
                    kind: "tcp".to_string(),
                    message: "ユニットID(スレーブID)が不正です(0〜255の範囲で指定してください)。"
                        .to_string(),
                }),
            );
        }
    };

    let config = ModbusTcpConfig {
        host: payload.host.clone(),
        port,
        unit_id,
        connect_timeout: PLC_TEST_TIMEOUT,
        response_timeout: PLC_TEST_TIMEOUT,
        ..Default::default()
    };
    let mut client = ModbusTcpClient::new(config);

    if let Err(err) = client.connect().await {
        return (
            false,
            Some(classify_plc_error(&err, Some(MODBUS_SESSION_HINT))),
        );
    }

    // 保持レジスタ先頭を1点読む - テストで固定した文字列リテラルなので parse
    // は失敗し得ない。
    let requests = [ReadRequest {
        address: Address::parse("40001").expect("valid literal"),
        data_type: DataType::U16,
    }];
    let read_result = client.read_batch(&requests).await;
    client.disconnect().await;

    match read_result {
        Ok(mut results) => match results.pop() {
            Some(ReadResult::Value(_)) => (true, None),
            Some(ReadResult::Bad(err)) => (
                false,
                Some(classify_plc_error(&err, Some(MODBUS_SESSION_HINT))),
            ),
            None => (
                false,
                Some(PlcConnectionTestError {
                    kind: "device".to_string(),
                    message: "読み出し結果が空でした。".to_string(),
                }),
            ),
        },
        Err(err) => (
            false,
            Some(classify_plc_error(&err, Some(MODBUS_SESSION_HINT))),
        ),
    }
}

/// #605: 既存セッションを再利用する前の CPU 照合。`session_cpu` は読む handle が
/// 作成時に持っていた CPU。フォームの CPU（`requested`、省略・空は「保存済みのまま」
/// で比べない）と違えば、保存してからテストするよう案内するエラーを返す。
/// 未知の値は直接ダイヤルと同じく不正として返す。
pub(super) fn slmp_session_cpu_refusal(
    session_cpu: Option<banto_plc::SlmpCpu>,
    requested: &Option<String>,
) -> Option<PlcConnectionTestError> {
    if requested.as_deref().is_none_or(str::is_empty) {
        return None;
    }
    let requested_cpu = match slmp_cpu_from_test_payload(requested) {
        Ok(cpu) => cpu,
        Err(err) => return Some(err),
    };
    let session_cpu = session_cpu?;
    if session_cpu == requested_cpu {
        return None;
    }
    Some(PlcConnectionTestError {
        kind: "protocol".to_string(),
        message: format!(
            "この接続は収集中で、保存済みの CPU 種別（{session_cpu:?}）のセッションを使っています。フォームの CPU 種別（{requested_cpu:?}）は、保存してからテストしてください。"
        ),
    })
}

/// SLMP の接続テスト。`payload.connection_id`があり、その接続の broker
/// セッションが既に生きていれば、それを再利用して読む(新規ダイヤルしない -
/// 実機 R08ENCPU は対象ポートが既に使用中だと2本目を受け付けないため、これを
/// 誤診しないための対策。このモジュール冒頭のコメント参照)。無ければ直接
/// ダイヤルにフォールバックする。
async fn test_slmp_connection(
    manager: &CollectorManager,
    payload: &PlcConnectionTestPayload,
) -> (bool, Option<PlcConnectionTestError>) {
    if let Some(connection_id) = payload.connection_id {
        if let Some(handle) = manager.sessions().handle_for(connection_id) {
            // #605: 保存済みセッションの CPU とフォームの CPU が違うとき、
            // セッションの読みはフォームの値を試したことにならない。かといって
            // 2 本目をダイヤルすると、1 本しか受けない機器（R08ENCPU など）では
            // 誤診になる。なのでテストせず、保存してから試すよう案内する。
            // 比べる CPU は、いま読む handle 自身が持つ（別のレジストリを引き直さない。
            // 引き直すと、設定の反映と交差して古いセッションを新しい CPU で通してしまう）。
            if let Some(refusal) = slmp_session_cpu_refusal(handle.slmp_cpu(), &payload.cpu_type) {
                return (false, Some(refusal));
            }
            let requests = vec![BatchReadRequest::Numeric(ReadRequest {
                address: Address::parse_slmp("D0").expect("valid literal"),
                data_type: DataType::U16,
            })];
            // `ReadOnlyHandle::read`自体には外側タイムアウトが無いため、
            // ここで明示的に包む(実装指示どおり)。
            return match tokio::time::timeout(PLC_TEST_TIMEOUT, handle.read(requests)).await {
                Err(_elapsed) => (
                    false,
                    Some(PlcConnectionTestError {
                        kind: "timeout".to_string(),
                        message: "応答タイムアウトです(3秒)。共有セッションが応答しませんでした。"
                            .to_string(),
                    }),
                ),
                Ok(Err(BrokerError::Disconnected { .. })) => (
                    false,
                    Some(PlcConnectionTestError {
                        kind: "tcp".to_string(),
                        message: "この接続の共有セッションは現在切断中です(再接続待機中)。PLCの電源やネットワーク、または他アプリとのセッション競合を確認してください。"
                            .to_string(),
                    }),
                ),
                Ok(Err(BrokerError::ConnectionFailed { reason, .. })) => (
                    false,
                    Some(PlcConnectionTestError {
                        kind: "tcp".to_string(),
                        message: format!("共有セッションが接続断で失敗しました: {reason}"),
                    }),
                ),
                Ok(Err(BrokerError::TaskGone { .. })) => (
                    false,
                    Some(PlcConnectionTestError {
                        kind: "tcp".to_string(),
                        message: "内部エラー: セッションタスクが終了しています。".to_string(),
                    }),
                ),
                // 防御的フォールバック: この経路では通常発生しない
                // (UnsupportedProtocol/InvalidPortはensure_connection時点で
                // 弾かれているはず)。
                Ok(Err(err @ BrokerError::UnsupportedProtocol { .. }))
                | Ok(Err(err @ BrokerError::InvalidPort { .. })) => (
                    false,
                    Some(PlcConnectionTestError {
                        kind: "protocol".to_string(),
                        message: err.to_string(),
                    }),
                ),
                // 既存セッション再利用経路: セッション上限ヒントは付けない
                // (design の意図: 「2本目をダイヤルしない」こと自体が対策)。
                Ok(Ok(mut results)) => match results.pop() {
                    Some(BatchReadResult::Value(_)) => (true, None),
                    Some(BatchReadResult::Bad(err)) => {
                        (false, Some(classify_plc_error(&err, None)))
                    }
                    None => (
                        false,
                        Some(PlcConnectionTestError {
                            kind: "device".to_string(),
                            message: "読み出し結果が空でした。".to_string(),
                        }),
                    ),
                },
            };
        }
    }

    // 直接ダイヤル(connectionId が無い、またはセッションが見つからない場合)。
    // SLMP は unit_id を使わないので port のみ検証する。
    // #605: フォームの CPU 種別をフレームに載せる。未知の値はダイヤルしない。
    let cpu = match slmp_cpu_from_test_payload(&payload.cpu_type) {
        Ok(cpu) => cpu,
        Err(err) => return (false, Some(err)),
    };
    let port = match u16::try_from(payload.port) {
        Ok(port) => port,
        Err(_) => {
            return (
                false,
                Some(PlcConnectionTestError {
                    kind: "tcp".to_string(),
                    message: "ポート番号が不正です(1〜65535の範囲で指定してください)。".to_string(),
                }),
            );
        }
    };

    let config = SlmpConfig {
        host: payload.host.clone(),
        port,
        cpu,
        connect_timeout: PLC_TEST_TIMEOUT,
        response_timeout: PLC_TEST_TIMEOUT,
        ..Default::default()
    };
    let mut client = SlmpClient::new(config);

    if let Err(err) = client.connect().await {
        return (
            false,
            Some(classify_plc_error(&err, Some(SLMP_SESSION_HINT))),
        );
    }

    let requests = [ReadRequest {
        address: Address::parse_slmp("D0").expect("valid literal"),
        data_type: DataType::U16,
    }];
    let read_result = client.read_batch(&requests).await;
    client.disconnect().await;

    match read_result {
        Ok(mut results) => match results.pop() {
            Some(ReadResult::Value(_)) => (true, None),
            Some(ReadResult::Bad(err)) => (
                false,
                Some(classify_plc_error(&err, Some(SLMP_SESSION_HINT))),
            ),
            None => (
                false,
                Some(PlcConnectionTestError {
                    kind: "device".to_string(),
                    message: "読み出し結果が空でした。".to_string(),
                }),
            ),
        },
        Err(err) => (
            false,
            Some(classify_plc_error(&err, Some(SLMP_SESSION_HINT))),
        ),
    }
}

/// `POST /api/plc-connections/test` - T12(docs/ux-plan.md §4)。保存前に
/// host/port/protocol の疎通確認(実プロトコルでの軽い読み出し1回まで)を
/// 行う。**意図的に** `#[utoipa::path]`を付けず`ApiDoc::paths(...)`にも
/// 加えない - utoipa がドキュメント対象にしているのは`/api/v1/*`の機械
/// クライアント向け API のみで(このファイル冒頭「二系統に分かれた
/// ルーター」節参照)、他の`/api/plc-connections/*`管理ハンドラ
/// (create/update/delete)も同様に対象外にしている。ここだけドキュメント
/// 対象を広げると既存方針と矛盾するため、意図的に対象外のままにする。
///
/// 権限は他の I1 書き込みハンドラと同じ`require_editor`。CSRF は
/// `tag_registry_router`が属する管理系ルーター全体に既に`require_banto_client_header`
/// がかかっているので、ここでの追加対応は不要。
///
/// 失敗(`ok: false`)は通常の 200 応答として返す(`tags_batch`と同じ
/// 「ok:false は通常の応答」という判断) - レジストリへの書き込みが一切
/// 発生しない読み取り専用の疎通確認なので、監査ログ記録(`record_write`)や
/// `rebuild_and_notify`は呼ばない。
///
/// 疎通確認そのもの(virtual/simulation の即時拒否・プロトコル別ダイヤル・
/// 所要時間計測)は[`run_plc_connection_test`]へ切り出してある - T21
/// S1-c（docs/banto-hub-t21-design.md）の MCP `test_connection`ツール
/// ([`crate::mcp`]の`tool_test_connection`)がこの REST ハンドラと**全く同じ
/// 疎通確認**を行うために共有する(二重実装しない、このファイル冒頭
/// 「§3.7」節と同じ規律)。
pub(super) async fn plc_connections_test(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Json(payload): Json<PlcConnectionTestPayload>,
) -> Result<Json<PlcConnectionTestResponse>, ApiError> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "plc_connections",
        "POST",
        "/api/plc-connections/test",
    )
    .await?;

    Ok(Json(
        run_plc_connection_test(&state.manager, &payload).await,
    ))
}

/// [`plc_connections_test`]のdoc comment参照 - REST ハンドラと MCP の
/// `test_connection`ツールが共有する疎通確認本体。認可判定
/// (`require_editor`/MCP 側の`require_admin_scope`)は呼び出し元の責務で、
/// ここでは行わない。
pub(crate) async fn run_plc_connection_test(
    manager: &CollectorManager,
    payload: &PlcConnectionTestPayload,
) -> PlcConnectionTestResponse {
    let started = std::time::Instant::now();

    // ガード: virtual / simulation / 未知プロトコルは即座に ok:false, kind:
    // "unsupported"(実プロトコルの疎通確認より前に判定)。
    let (ok, error) = if payload.protocol == "virtual" {
        (
            false,
            Some(PlcConnectionTestError {
                kind: "unsupported".to_string(),
                message: "virtual接続はテスト対象外です(calc/mem予約接続)。".to_string(),
            }),
        )
    } else if payload.simulation {
        (
            false,
            Some(PlcConnectionTestError {
                kind: "unsupported".to_string(),
                message: "シミュレーション接続はテスト不要です(常に内蔵シミュレータに接続され、常に成功します)。"
                    .to_string(),
            }),
        )
    } else {
        match payload.protocol.as_str() {
            "modbus-tcp" => test_modbus_connection(manager, payload).await,
            "slmp" => test_slmp_connection(manager, payload).await,
            other => (
                false,
                Some(PlcConnectionTestError {
                    kind: "unsupported".to_string(),
                    message: format!("不明なプロトコルです: {other}"),
                }),
            ),
        }
    };

    PlcConnectionTestResponse {
        ok,
        elapsed_ms: started.elapsed().as_millis() as u64,
        error,
    }
}
