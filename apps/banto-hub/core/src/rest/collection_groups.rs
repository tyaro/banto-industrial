//! 収集グループの CRUD（`/api/collection-groups`）と `describe`（外部 DB 連携 S3）、および
//! 収集グループの作成・更新ペイロード（`CollectionGroupPayload`）。
//!
//! `rest.rs` から中身を変えずに移したもの（docs/banto-hub-driver-refactor-plan.md §8.1、
//! P0-1 の手順 4）。I1 CRUD の共通層は `rest/crud.rs` にあり、認証・ガード・監査の補助は
//! 親の `rest.rs` にある。どちらも `use super::{...}` で使う。外から使われるパス
//! （`crate::rest::X`）は親の re-export で保つ。

use super::{
    commit_catalog_and_notify, default_payload_enabled, json, preflight_transaction,
    queue_pending_registry_change, record_write, registry_before, registry_change_should_queue,
    require_editor, storage_api_error, ApiError, CollectionGroup, CollectionGroupInput,
    Deserialize, HeaderMap, IntoResponse, Json, ListParams, Path, RegistryMutationError,
    RegistryMutationResult, Response, Serialize, State, StatusCode, TagRegistryState,
};

/// T19 S1-b（UX-34、docs/banto-hub-t19-design.md §2）: a
/// `CollectionGroupPayload` missing `defaultWritable` (an old client, or a
/// create/update that never mentions it) keeps the same default as the
/// column itself (`migrations/0012_collection_groups_add_default_writable
/// .sql`'s `DEFAULT 1`, and `banto_tags::collection_group::
/// default_writable_true`) - UX-34's overall "既定 ON" policy, mirrored the
/// same way `default_plc_word_order` mirrors migration `0010`'s column
/// default.
fn default_group_writable() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionGroupPayload {
    pub name: String,
    pub plc_connection_id: i64,
    pub period_ms: i64,
    #[serde(default = "default_payload_enabled")]
    pub enabled: bool,
    /// T19 S1-b（UX-34）: このグループへ新規タグを登録するときの
    /// `writable` チェックボックスの既定値。省略時は `default_group_writable`
    /// （既定 ON、`banto_tags::collection_group::default_writable_true`と
    /// 同じ既定値）。
    #[serde(default = "default_group_writable")]
    pub default_writable: bool,
    /// 外部 DB 連携 S2（docs/banto-hub-external-db-design.md §4.1・§4.2）:
    /// `protocol = "postgres"` の接続配下のグループが1周期ごとに実行する
    /// SELECT 文。`#[serde(default)]`（= `None`）なので、この列を知らない
    /// 既存クライアントの PLC グループ payload は無変更で通る
    /// （`defaultWritable`/`writable`/`tagKind` と同じ後方互換の扱い）。
    /// 必須/禁止の2方向の検証は `banto_tags` 側の `validate_query_sql`。
    #[serde(default)]
    pub query_sql: Option<String>,
}

impl From<CollectionGroupPayload> for CollectionGroupInput {
    fn from(payload: CollectionGroupPayload) -> Self {
        Self {
            name: payload.name,
            plc_connection_id: payload.plc_connection_id,
            period_ms: payload.period_ms,
            enabled: payload.enabled,
            // T19 S1-b: wired through - see `CollectionGroupPayload::
            // default_writable`'s doc comment.
            default_writable: payload.default_writable,
            // 外部 DB 連携 S2: 同上 - `postgres` 接続配下のグループでは必須、
            // それ以外では指定不可（`banto_tags` 側の `validate_query_sql`
            // が両方向を強制する）。
            query_sql: payload.query_sql,
        }
    }
}

pub(super) async fn collection_groups_list(
    State(state): State<TagRegistryState>,
) -> Result<Json<Vec<CollectionGroup>>, ApiError> {
    Ok(Json(
        state
            .collection_groups
            .list(ListParams::default())
            .await?
            .rows,
    ))
}

pub(super) async fn collection_groups_get(
    State(state): State<TagRegistryState>,
    Path(id): Path<i64>,
) -> Result<Json<CollectionGroup>, ApiError> {
    Ok(Json(state.collection_groups.get(id).await?))
}

/// `POST /api/collection-groups/{id}/describe` - S3
/// (docs/banto-hub-external-db-design.md §4.6・§7 スライス表「S3: 列名候補の
/// 提示」): 保存済みグループの `query_sql` を対象に、`db_source::describe_group`
/// （5秒タイムアウトの `describe` 1回、実行はしない）を呼び、列名・
/// PostgreSQL型・v1 対応可否を返す。タグ登録 UI の「列を取得」ボタンが
/// 「結果列名」候補（`db` タグの `address`）を提示するために呼ぶ。
///
/// 認可は保存済み接続の接続テスト（[`plc_connections_test_saved`]）と
/// 同じ `require_editor` に揃える - こちらも DB へ接続してクエリ（読み取り
/// 専用の `describe`）を実行する操作である点は接続テストと同じ性質のため。
///
/// 対象グループが `protocol == "postgres"` の接続配下でない場合、または
/// （通常到達しない防御として）`query_sql` が `None` の場合は `400` で
/// 明示する - `plc_connections_test_saved` が非 postgres を `400` にする
/// のと同じ判断（`ok: false` の `200` ではなく、このエンドポイントが
/// そもそも対応しない入力であることを明確にする）。
///
/// レジストリへの書き込みは一切発生しない読み取り専用の疎通確認なので、
/// `record_write`/`commit_catalog_and_notify` は呼ばない -
/// `plc_connections_test_saved` と同じ判断。
pub(super) async fn collection_groups_describe(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Response, ApiError> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "collection_groups",
        "POST",
        "/api/collection-groups/{id}/describe",
    )
    .await?;

    let group = state.collection_groups.get(id).await?;
    let conn = state.plc_connections.get(group.plc_connection_id).await?;
    if !conn.is_db_source() {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "message": "postgres接続配下のグループのみdescribeに対応しています。",
            })),
        )
            .into_response());
    }
    let Some(query_sql) = group.query_sql.as_deref() else {
        // 通常到達しない: postgres 配下のグループは `validate_query_sql`
        // （crates/banto-tags/src/collection_group.rs）が `query_sql` を
        // 必須にしている（§4.1）。念のための防御。
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "message": "このグループにはquerySqlが設定されていません。",
            })),
        )
            .into_response());
    };

    let secret_rows = state.plc_connections.secret_rows(conn.id).await?;
    let outcome = crate::db_source::describe_group(&conn, &secret_rows, query_sql).await;
    Ok((StatusCode::OK, Json(outcome)).into_response())
}

pub(super) async fn collection_groups_create(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Json(input): Json<CollectionGroupPayload>,
) -> RegistryMutationResult<Response> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "collection_groups",
        "POST",
        "/api/collection-groups",
    )
    .await?;
    if let Some(status) = registry_change_should_queue(&state.controller, &state.commissioning) {
        return queue_pending_registry_change(
            &state,
            &headers,
            "collection_groups.create",
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
    let created = match state
        .collection_groups
        .create_tx(&mut tx, input.into())
        .await
    {
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
        "collection_groups",
        Some(&created.id.to_string()),
        Some(json!({ "name": created.name, "enabled": created.enabled })),
    )
    .await;
    commit_catalog_and_notify(&state.controller, &state.events, "collection_groups")
        .await
        .map_err(RegistryMutationError::LiveApplyFailed)?;
    Ok(Json(created).into_response())
}

pub(super) async fn collection_groups_update(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(input): Json<CollectionGroupPayload>,
) -> RegistryMutationResult<Response> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "collection_groups",
        "PUT",
        "/api/collection-groups/{id}",
    )
    .await?;
    if let Some(status) = registry_change_should_queue(&state.controller, &state.commissioning) {
        return queue_pending_registry_change(
            &state,
            &headers,
            "collection_groups.update",
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
        .collection_groups
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
        "collection_groups",
        Some(&id.to_string()),
        Some(json!({ "name": updated.name, "enabled": updated.enabled })),
    )
    .await;
    commit_catalog_and_notify(&state.controller, &state.events, "collection_groups")
        .await
        .map_err(RegistryMutationError::LiveApplyFailed)?;
    Ok(Json(updated).into_response())
}

pub(super) async fn collection_groups_delete(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> RegistryMutationResult<Response> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "collection_groups",
        "DELETE",
        "/api/collection-groups/{id}",
    )
    .await?;
    if let Some(status) = registry_change_should_queue(&state.controller, &state.commissioning) {
        return queue_pending_registry_change(
            &state,
            &headers,
            "collection_groups.delete",
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
    // T19 S2-b（UX-38）: `plc_connections_delete` と同じ理由・同じ形で
    // `cascade_delete_tx` を使う（属するタグごと1トランザクションで削除、
    // `banto_tags::collection_group::CollectionGroupService::cascade_delete_tx`
    // の doc comment参照）。
    let cascade = match state.collection_groups.cascade_delete_tx(&mut tx, id).await {
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
        "collection_groups",
        Some(&id.to_string()),
        Some(json!({
            "cascade": { "deletedTags": cascade.deleted_tags },
        })),
    )
    .await;
    commit_catalog_and_notify(&state.controller, &state.events, "collection_groups")
        .await
        .map_err(RegistryMutationError::LiveApplyFailed)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
