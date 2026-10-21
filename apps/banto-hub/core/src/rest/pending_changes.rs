//! 保留中の変更（pending changes）の閲覧・取り消し・再キュー・適用 API
//! （`/api/pending-changes*`）と、その適用実行（`execute_pending_apply`）。
//!
//! `rest.rs` から中身を変えずに移したもの（docs/banto-hub-driver-refactor-plan.md §8.1、
//! P0-1 の手順 5）。I1 CRUD の共通層（キューへの振り分け・秘匿処理）は `rest/crud.rs`
//! にあり、認証・ガード・監査の補助は親の `rest.rs` にある。どちらも `use super::{...}`
//! で使う。外から使われるパス（`crate::rest::X`）は親の re-export で保つ。

use super::{
    actor_identity, broadcast, commit_catalog_and_notify, get, json, middleware, post,
    preflight_api_error, preflight_transaction, record_write, redact_pending_change_for_response,
    registry_before, reject_delete_if_referenced_by_sink_group, reject_hub_connection_protocol,
    remove_tag_from_all_sink_groups, require_session, require_session_role, storage_api_error,
    ApiError, Arc, AsyncMutex, AuditEntry, AuditLogService, AuthGate, AuthState, BantoError,
    BatchTagDeleteOutcome, BatchTagOutcome, BatchTagUpdateOutcome, CollectionController,
    CollectionGroupPayload, CollectionGroupService, CollectorManager, Deserialize, FieldError,
    HeaderMap, IntoResponse, Json, OperationKind, Path, PendingChange, PendingChangesService,
    PlcConnectionPayload, PlcConnectionResponse, PlcConnectionService, Query, Response, Role,
    Router, Serialize, ServerEvent, SessionRoleGuard, State, StatusCode, TagBatchUpdatePayload,
    TagInput, TagPayload, TagService, TagUpdateError, LIVE_APPLY_RECOVERY_HINT,
};

#[derive(Clone)]
struct PendingChangesAdminState {
    pending_changes: PendingChangesService,
    plc_connections: PlcConnectionService,
    collection_groups: CollectionGroupService,
    tags: TagService,
    manager: Arc<CollectorManager>,
    controller: Arc<CollectionController>,
    events: broadcast::Sender<ServerEvent>,
    apply_lock: Arc<AsyncMutex<()>>,
    auth: AuthState,
    audit: AuditLogService,
}

#[derive(Debug, Deserialize)]
struct PendingChangesQuery {
    limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct PendingChangeWithInput<T> {
    input: T,
}

#[derive(Debug, Deserialize)]
struct PendingChangeWithId {
    id: i64,
}

#[derive(Debug, Deserialize)]
struct PendingChangeWithIdAndInput<T> {
    id: i64,
    input: T,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PendingBatchTagsPayload {
    #[serde(default)]
    dry_run: bool,
    tags: Vec<TagPayload>,
}

/// T18-3b: [`PendingBatchTagsPayload`] の一括更新版 - `tags.batch_update`
/// 経由でキューされた pending change を [`execute_pending_apply`] が
/// デコードする際の形。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PendingBatchTagsUpdatePayload {
    #[serde(default)]
    dry_run: bool,
    tags: Vec<TagBatchUpdatePayload>,
}

/// T19 S2-c1: [`PendingBatchTagsPayload`]の一括削除版 - `tags.batch_delete`
/// 経由でキューされた pending change を [`execute_pending_apply`] が
/// デコードする際の形。一括削除に dry run は無いので、更新/作成版と違い
/// `dry_run` フィールドを持たない([`BatchTagsDeleteRequest`]と同型)。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PendingBatchTagsDeletePayload {
    ids: Vec<i64>,
}

enum PendingApplyError {
    Api(ApiError),
    /// #341（2026-09-14 オーナー回答）: 適用（DB への書き込み）は成功した
    /// が、その内容を catalog / 走行中の収集へ反映できなかった
    /// （[`commit_catalog_and_notify`] の `Err`）。`Api`/`Conflict` と違い
    /// **pending change は `applied` へ遷移させる**（レジストリ行は既に
    /// コミット済みで、`failed` にして再適用させると二重適用になる） -
    /// [`pending_changes_apply`] がこの1件だけ特別扱いしている。
    LiveApplyFailed(String),
    /// TAG-P0-3 follow-up（2026-08-12）: この pending change の
    /// `base_fingerprint` が、適用直前に再取得した対象リソースの現在値と
    /// 一致しない（＝ enqueue 後に別経路で変更または削除された）場合の
    /// エラー。`compute_pending_base_fingerprint` の doc comment 参照。
    /// グローバルな `configured_revision` 比較ではなく per-resource 比較に
    /// しているのは、複数件の pending change を連続適用すると
    /// `commit_catalog_and_notify` が毎回グローバル revision を進めるため。
    Conflict {
        resource: &'static str,
    },
}

impl From<ApiError> for PendingApplyError {
    fn from(value: ApiError) -> Self {
        Self::Api(value)
    }
}

impl From<BantoError> for PendingApplyError {
    fn from(value: BantoError) -> Self {
        Self::Api(ApiError(value))
    }
}

/// `resource`（テーブル名）を日本語の表示名に寄せてから conflict
/// メッセージを組み立てる。`reason()`（`failure_reason` 用）と
/// `IntoResponse`（HTTP レスポンス body 用）の両方から呼ぶことで、
/// メッセージ文言の重複定義を避ける。
fn pending_apply_conflict_message(resource: &str) -> String {
    let display_name = match resource {
        "plc_connections" => "PLC接続",
        "collection_groups" => "収集グループ",
        other => other,
    };
    format!(
        "適用対象の{display_name}が未適用キュー登録後に変更されています。この提案を破棄して再作成してください。"
    )
}

/// `BantoError` の `Display`（`thiserror` の `#[error(...)]`）は種別ごとの
/// 定型文だけで、`Validation` は常に `"validation failed"` としか出ない。
/// フィールド単位の理由（`field_errors`）が丸ごと落ちる。pending change の
/// 失敗理由としてはこれでは「何が」失敗したのか分からない（実機で再現した
/// 不具合の修正2、2026-08-31 オーナー報告: 収集稼働中に同じ名前で収集
/// グループを3回作成し、3回とも適用時に `pending change の適用に失敗
/// しました: validation failed` としか出ず、名前が重複していることに
/// 気づけなかった）。`Validation` のときだけ `field_errors` を
/// `"{field}: {message}"` の形へ展開する（該当する UI 入力欄の項目名と揃う。
/// `ConnectionDrawer.svelte`/`CollectionGroupDrawer.svelte` の
/// `applyFieldErrors` が読む `field`/`message` と同じペア）。それ以外の
/// 種別は従来どおり `Display` に委ねる。
fn banto_error_detail(err: &BantoError) -> String {
    match err {
        BantoError::Validation { field_errors } if !field_errors.is_empty() => field_errors
            .iter()
            .map(|fe| format!("{}: {}", fe.field, fe.message))
            .collect::<Vec<_>>()
            .join(", "),
        other => other.to_string(),
    }
}

impl PendingApplyError {
    fn reason(&self) -> String {
        match self {
            Self::Api(err) => format!(
                "pending change の適用に失敗しました: {}",
                banto_error_detail(&err.0)
            ),
            Self::LiveApplyFailed(message) => {
                format!(
                    "変更は保存しましたが、実行構成への反映に失敗しました: {message}{LIVE_APPLY_RECOVERY_HINT}"
                )
            }
            Self::Conflict { resource } => pending_apply_conflict_message(resource),
        }
    }
}

impl IntoResponse for PendingApplyError {
    fn into_response(self) -> Response {
        match self {
            Self::Api(err) => err.into_response(),
            Self::LiveApplyFailed(message) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": "live_reconfigure_failed",
                    "message": format!(
                        "変更は保存しましたが、実行構成への反映に失敗しました: {message}{LIVE_APPLY_RECOVERY_HINT}"
                    ),
                })),
            )
                .into_response(),
            Self::Conflict { resource } => (
                StatusCode::CONFLICT,
                Json(json!({
                    "error": "pending_apply_conflict",
                    "resource": resource,
                    "message": pending_apply_conflict_message(resource),
                })),
            )
                .into_response(),
        }
    }
}

fn decode_pending_payload<T: serde::de::DeserializeOwned>(
    pending: &PendingChange,
) -> Result<T, PendingApplyError> {
    serde_json::from_value(pending.payload.clone()).map_err(|err| {
        PendingApplyError::Api(preflight_api_error(format!(
            "pending payload の形式が不正です(source={}): {err}",
            pending.source
        )))
    })
}

/// Re-fetch `id` via `getter`'s result and compare its re-serialized form
/// against `expected` (the fingerprint captured when the pending change was
/// queued — see `compute_pending_base_fingerprint`). `Ok(())` if unchanged,
/// `Err(PendingApplyError::Conflict)` if the row changed OR is now gone
/// (`Err` from the getter — e.g. `NotFound` — also counts as staleness: the
/// row this pending change targets no longer exists in the state it was
/// queued against).
async fn check_fingerprint_unchanged<T: Serialize>(
    current: Result<T, BantoError>,
    expected: &str,
    resource: &'static str,
) -> Result<(), PendingApplyError> {
    let unchanged = match current {
        Ok(row) => serde_json::to_string(&row)
            .map(|s| s == expected)
            .unwrap_or(false),
        Err(_) => false,
    };
    if unchanged {
        Ok(())
    } else {
        Err(PendingApplyError::Conflict { resource })
    }
}

/// #341（2026-09-14 オーナー回答「明示適用も無停止」）: かつてここには
/// `CollectionState::Stopped` 要求（収集中は 409 `collection_edit_locked`）が
/// あったが撤去した。ロックダウン済みで求められるワンクッションは
/// 「pending queue + 人による明示適用」そのものであり、適用のために収集を
/// 止める技術的な理由は T7（`Collector::apply_config` による接続単位の
/// 部分再構成、二重接続窓の解消）以降もう無い - 反映は他の全 mutation と
/// 同じく [`commit_catalog_and_notify`] に任せる（走行中なら
/// [`CollectionController::commit_catalog_and_apply_live`] が無停止で
/// 実行構成まで更新する）。per-resource フィンガープリントガード・
/// キャンセル・requeue の導線はそのまま維持している。
async fn execute_pending_apply(
    state: &PendingChangesAdminState,
    pending: &PendingChange,
) -> Result<(), PendingApplyError> {
    // 読み取りを挟む書き込みなので BEGIN IMMEDIATE（checklist §5 / sink/service.rs の doc）。
    let mut tx = state
        .manager
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(storage_api_error)
        .map_err(PendingApplyError::Api)?;
    // #635: 書き込みの前の registry。書き込みで新しく生まれた外部名の重なりだけを断る。
    let registry_before = match registry_before(&mut tx).await {
        Ok(before) => before,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(PendingApplyError::Api(err));
        }
    };

    let resource = match pending.source.as_str() {
        "plc_connections.create" => {
            let body: PendingChangeWithInput<PlcConnectionPayload> =
                decode_pending_payload(pending)?;
            reject_hub_connection_protocol(&body.input)
                .map_err(ApiError)
                .map_err(PendingApplyError::Api)?;
            state
                .plc_connections
                .create_tx(
                    &mut tx,
                    body.input
                        .into_input()
                        .map_err(ApiError)
                        .map_err(PendingApplyError::Api)?,
                )
                .await
                .map_err(ApiError)
                .map_err(PendingApplyError::Api)?;
            "plc_connections"
        }
        "plc_connections.update" => {
            let body: PendingChangeWithIdAndInput<PlcConnectionPayload> =
                decode_pending_payload(pending)?;
            reject_hub_connection_protocol(&body.input)
                .map_err(ApiError)
                .map_err(PendingApplyError::Api)?;
            if let Some(expected) = &pending.base_fingerprint {
                // Plain pool read (not part of `tx` above) — same source as
                // `compute_pending_base_fingerprint` used at enqueue time.
                // The `apply_lock` mutex in `PendingChangesAdminState`
                // already serializes concurrent apply calls against each
                // other, which is the concurrency case this guard closes
                // (edits made *before* Apply was clicked, not sub-request
                // races during Apply itself).
                // S1: compare against the same redacted DTO
                // `compute_pending_base_fingerprint` used at enqueue time
                // (this crate's own comment there) - never the raw
                // `PlcConnection` (which would serialize the plaintext
                // password).
                check_fingerprint_unchanged(
                    state
                        .plc_connections
                        .get(body.id)
                        .await
                        .map(PlcConnectionResponse::from),
                    expected,
                    "plc_connections",
                )
                .await?;
            }
            state
                .plc_connections
                .update_tx(
                    &mut tx,
                    body.id,
                    body.input
                        .into_input()
                        .map_err(ApiError)
                        .map_err(PendingApplyError::Api)?,
                )
                .await
                .map_err(ApiError)
                .map_err(PendingApplyError::Api)?;
            "plc_connections"
        }
        "plc_connections.delete" => {
            let body: PendingChangeWithId = decode_pending_payload(pending)?;
            if let Some(expected) = &pending.base_fingerprint {
                // S1: same redacted-DTO comparison as the `.update` arm above.
                check_fingerprint_unchanged(
                    state
                        .plc_connections
                        .get(body.id)
                        .await
                        .map(PlcConnectionResponse::from),
                    expected,
                    "plc_connections",
                )
                .await?;
            }
            // 外部 DB 連携 S4: `plc_connections_delete`（即時削除の REST
            // ハンドラ）と同じ理由 - `cascade_delete_tx`の前に sink group
            // からの参照を拒否する。
            reject_delete_if_referenced_by_sink_group(&mut tx, body.id)
                .await
                .map_err(ApiError)
                .map_err(PendingApplyError::Api)?;
            // T19 S2-b（UX-38）: `plc_connections_delete`（即時削除の REST
            // ハンドラ）と同じく `cascade_delete_tx` を使う - 収集稼働中に
            // キューされ、後で（停止後に）適用される削除も同じ「子ごと削除」
            // 仕様に揃える。
            state
                .plc_connections
                .cascade_delete_tx(&mut tx, body.id)
                .await
                .map_err(ApiError)
                .map_err(PendingApplyError::Api)?;
            "plc_connections"
        }
        "collection_groups.create" => {
            let body: PendingChangeWithInput<CollectionGroupPayload> =
                decode_pending_payload(pending)?;
            state
                .collection_groups
                .create_tx(&mut tx, body.input.into())
                .await
                .map_err(ApiError)
                .map_err(PendingApplyError::Api)?;
            "collection_groups"
        }
        "collection_groups.update" => {
            let body: PendingChangeWithIdAndInput<CollectionGroupPayload> =
                decode_pending_payload(pending)?;
            if let Some(expected) = &pending.base_fingerprint {
                check_fingerprint_unchanged(
                    state.collection_groups.get(body.id).await,
                    expected,
                    "collection_groups",
                )
                .await?;
            }
            state
                .collection_groups
                .update_tx(&mut tx, body.id, body.input.into())
                .await
                .map_err(ApiError)
                .map_err(PendingApplyError::Api)?;
            "collection_groups"
        }
        "collection_groups.delete" => {
            let body: PendingChangeWithId = decode_pending_payload(pending)?;
            if let Some(expected) = &pending.base_fingerprint {
                check_fingerprint_unchanged(
                    state.collection_groups.get(body.id).await,
                    expected,
                    "collection_groups",
                )
                .await?;
            }
            // T19 S2-b（UX-38）: `plc_connections.delete`分岐と同じ理由で
            // `cascade_delete_tx` を使う。
            state
                .collection_groups
                .cascade_delete_tx(&mut tx, body.id)
                .await
                .map_err(ApiError)
                .map_err(PendingApplyError::Api)?;
            "collection_groups"
        }
        "tags.create" => {
            let body: PendingChangeWithInput<TagPayload> = decode_pending_payload(pending)?;
            state
                .tags
                .create_tx(&mut tx, body.input.into())
                .await
                .map_err(ApiError)
                .map_err(PendingApplyError::Api)?;
            "tags"
        }
        "tags.update" => {
            let body: PendingChangeWithIdAndInput<TagPayload> = decode_pending_payload(pending)?;
            state
                .tags
                .update_tx(&mut tx, body.id, body.input.into())
                .await
                .map_err(|err| match err {
                    TagUpdateError::Banto(error) => PendingApplyError::Api(ApiError(error)),
                    TagUpdateError::RevisionConflict(_) => PendingApplyError::Api(ApiError(
                        BantoError::Validation {
                            field_errors: vec![FieldError {
                                field: "expectedRevision".to_string(),
                                message: "他のクライアントがこのタグを更新済みです。再読込してから再試行してください。"
                                    .to_string(),
                            }],
                        },
                    )),
                })?;
            "tags"
        }
        "tags.delete" => {
            let body: PendingChangeWithId = decode_pending_payload(pending)?;
            state
                .tags
                .delete_tx(&mut tx, body.id)
                .await
                .map_err(ApiError)
                .map_err(PendingApplyError::Api)?;
            // 外部 DB 連携 S4: `tags_delete`（即時削除の REST ハンドラ）と
            // 同じ理由でメンバーシップ行を能動的に消す。
            remove_tag_from_all_sink_groups(&mut tx, body.id)
                .await
                .map_err(ApiError)
                .map_err(PendingApplyError::Api)?;
            "tags"
        }
        "tags.batch_create" => {
            let body: PendingBatchTagsPayload = decode_pending_payload(pending)?;
            if body.dry_run {
                return Err(PendingApplyError::Api(preflight_api_error(
                    "pending の tags.batch_create は dryRun=false のみ対応です".to_string(),
                )));
            }
            let inputs: Vec<TagInput> = body.tags.into_iter().map(Into::into).collect();
            let outcome = state
                .tags
                .create_batch_tx(&mut tx, &inputs)
                .await
                .map_err(ApiError)
                .map_err(PendingApplyError::Api)?;
            if let BatchTagOutcome::Invalid(errors) = outcome {
                let field_errors = errors
                    .into_iter()
                    .flat_map(|row| {
                        row.field_errors.into_iter().map(move |field| FieldError {
                            field: format!("tags[{}].{}", row.index, field.field),
                            message: field.message,
                        })
                    })
                    .collect();
                return Err(PendingApplyError::Api(ApiError(BantoError::Validation {
                    field_errors,
                })));
            }
            "tags"
        }
        "tags.batch_update" => {
            let body: PendingBatchTagsUpdatePayload = decode_pending_payload(pending)?;
            if body.dry_run {
                return Err(PendingApplyError::Api(preflight_api_error(
                    "pending の tags.batch_update は dryRun=false のみ対応です".to_string(),
                )));
            }
            let updates: Vec<(i64, TagInput)> = body.tags.into_iter().map(Into::into).collect();
            let outcome = state
                .tags
                .update_batch_tx(&mut tx, &updates)
                .await
                .map_err(ApiError)
                .map_err(PendingApplyError::Api)?;
            if let BatchTagUpdateOutcome::Invalid(errors) = outcome {
                let field_errors = errors
                    .into_iter()
                    .flat_map(|row| {
                        row.field_errors.into_iter().map(move |field| FieldError {
                            field: format!("tags[{}].{}", row.index, field.field),
                            message: field.message,
                        })
                    })
                    .collect();
                return Err(PendingApplyError::Api(ApiError(BantoError::Validation {
                    field_errors,
                })));
            }
            "tags"
        }
        "tags.batch_delete" => {
            // T19 S2-c1 (UX-37): tags.batch_update 分岐と同じ形 - 稼働中に
            // キューされ、後で（停止後に）適用される一括削除も、即時削除
            // （`tags_batch_delete`）と全く同じ `delete_batch_tx` を使う。
            // ここを他の delete 系分岐（`tags.delete`/`plc_connections.delete`/
            // `collection_groups.delete`）と揃えないと、「即時削除はできる
            // のに、稼働中に積んだ保留削除だけ適用時に失敗する」という
            // 非対称バグになる（S2-b で実際に踏みかけた罠 - 実装指示参照）。
            let body: PendingBatchTagsDeletePayload = decode_pending_payload(pending)?;
            let outcome = state
                .tags
                .delete_batch_tx(&mut tx, &body.ids)
                .await
                .map_err(ApiError)
                .map_err(PendingApplyError::Api)?;
            if let BatchTagDeleteOutcome::Invalid(errors) = outcome {
                // `tags.batch_update` 分岐（直上）と違い `.{field.field}` を
                // 付けない: あちらの pending payload は `{ "tags": [ {
                // "id":…, "name":… } ] }` というオブジェクトの配列なので
                // `tags[i].<field>` が実在する JSON パスになるが、こちらの
                // payload は `{ "ids": [1, 2, 3] }` というスカラの配列なの
                // で、`ids[i]` 自体が対象を指し、その下に `.id` は存在しな
                // い。実際 `delete_batch_tx` が返す行エラーの `field` は
                // 構造上つねに `"id"`（重複・不存在の2種のみ）なので、
                // `.{field.field}` を付けても情報は増えず実在しないパスに
                // なるだけ。
                let field_errors = errors
                    .into_iter()
                    .flat_map(|row| {
                        row.field_errors.into_iter().map(move |field| FieldError {
                            field: format!("ids[{}]", row.index),
                            message: field.message,
                        })
                    })
                    .collect();
                return Err(PendingApplyError::Api(ApiError(BantoError::Validation {
                    field_errors,
                })));
            }
            // 外部 DB 連携 S4: 即時削除（`tags_batch_delete`）と同じ理由。
            for id in &body.ids {
                remove_tag_from_all_sink_groups(&mut tx, *id)
                    .await
                    .map_err(ApiError)
                    .map_err(PendingApplyError::Api)?;
            }
            "tags"
        }
        other => {
            return Err(PendingApplyError::Api(preflight_api_error(format!(
                "未対応の pending source です: {other}"
            ))));
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
            return Err(PendingApplyError::Api(err));
        }
    };

    if let Err(err) = tx.commit().await {
        return Err(PendingApplyError::Api(storage_api_error(err)));
    }

    commit_catalog_and_notify(&state.controller, &state.events, resource)
        .await
        .map_err(PendingApplyError::LiveApplyFailed)?;

    Ok(())
}

async fn pending_changes_list(
    State(state): State<PendingChangesAdminState>,
    Query(query): Query<PendingChangesQuery>,
) -> Result<Json<Vec<PendingChange>>, ApiError> {
    let limit = query.limit.unwrap_or(100).clamp(1, 1000);
    Ok(Json(
        state
            .pending_changes
            .list(limit)
            .await?
            .into_iter()
            .map(redact_pending_change_for_response)
            .collect(),
    ))
}

async fn pending_changes_get(
    State(state): State<PendingChangesAdminState>,
    Path(id): Path<i64>,
) -> Result<Json<PendingChange>, ApiError> {
    Ok(Json(redact_pending_change_for_response(
        state.pending_changes.get(id).await?,
    )))
}

async fn pending_changes_cancel(
    State(state): State<PendingChangesAdminState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<PendingChange>, ApiError> {
    let pending = state.pending_changes.cancel_pending(id).await?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "cancel",
        "pending_changes",
        Some(&id.to_string()),
        None,
    )
    .await;
    Ok(Json(redact_pending_change_for_response(pending)))
}

async fn pending_changes_requeue(
    State(state): State<PendingChangesAdminState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<PendingChange>, ApiError> {
    let pending = state.pending_changes.requeue_pending(id).await?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "requeue",
        "pending_changes",
        Some(&id.to_string()),
        None,
    )
    .await;
    Ok(Json(redact_pending_change_for_response(pending)))
}

async fn pending_changes_apply(
    State(state): State<PendingChangesAdminState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    let _guard = state.apply_lock.lock().await;
    let applying = match state.pending_changes.start_applying(id).await {
        Ok(applying) => applying,
        Err(err) => return ApiError(err).into_response(),
    };

    if let Err(err) = execute_pending_apply(&state, &applying).await {
        let failure_reason = err.reason();
        // #341: 実行構成への反映だけが失敗した場合、レジストリ行は既に
        // コミット済みなので `failed`（= requeue して再適用しうる）へは
        // 落とさず `applied` にする - 二重適用を作らないため。反映失敗
        // 自体は 500 で伝え、詳細は `/api/v1/status` の
        // `last_config_error` にも残る。
        if let PendingApplyError::LiveApplyFailed(message) = &err {
            let applied = match state.pending_changes.mark_applied(id).await {
                Ok(applied) => Some(redact_pending_change_for_response(applied)),
                Err(mark_err) => {
                    eprintln!(
                        "banto-hub: pending_change={id} の applied 遷移に失敗しました: {mark_err}"
                    );
                    None
                }
            };
            // #341 レビュー対応（2026-09-14）: 成功時の `record_write`
            // （`result: "ok"`）と対になる失敗の監査行。DB へは適用済み
            // （行は `applied`）で実行構成への反映だけが失敗した、という
            // 事実がここだけからしか追えないため、必ず残す
            // （#340 の `record_write_control_failure` と同じ型）。
            let identity = actor_identity(&headers, &state.auth);
            state
                .audit
                .record(AuditEntry {
                    actor_username: identity.as_ref().map(|i| i.id.as_str()),
                    actor_role: identity.as_ref().map(|i| i.role.as_str()),
                    action: "apply",
                    resource: "pending_changes",
                    entity_id: Some(&id.to_string()),
                    detail: Some(json!({
                        "source": applying.source,
                        "failureReason": failure_reason,
                        "note": "DB への適用は成功したが、実行構成への反映に失敗した",
                    })),
                    origin: "rest",
                    result: "failed",
                })
                .await;
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": "live_reconfigure_failed",
                    "message": format!(
                        "変更は保存しましたが、実行構成への反映に失敗しました: {message}{LIVE_APPLY_RECOVERY_HINT}"
                    ),
                    "failureReason": failure_reason,
                    "pending": applied,
                })),
            )
                .into_response();
        }
        if let Err(mark_err) = state.pending_changes.mark_failed(id, &failure_reason).await {
            eprintln!("banto-hub: pending_change={id} の failed 遷移に失敗しました: {mark_err}");
        }

        return match err {
            PendingApplyError::Api(err) => err.into_response(),
            // TAG-P0-3 follow-up（2026-08-12）: `IntoResponse` 実装がそのまま
            // 409 + conflict body を返す。`failure_reason` は既に
            // `mark_failed` で DB へ記録済みなので、`GET
            // /api/pending-changes/{id}` から後追いで確認できる。
            err @ PendingApplyError::Conflict { .. } => err.into_response(),
            // #341: 上の分岐で早期 return 済み。
            PendingApplyError::LiveApplyFailed(_) => unreachable!(),
        };
    }

    let applied = match state.pending_changes.mark_applied(id).await {
        Ok(applied) => applied,
        Err(err) => return ApiError(err).into_response(),
    };
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "apply",
        "pending_changes",
        Some(&id.to_string()),
        Some(json!({ "source": applying.source })),
    )
    .await;
    Json(redact_pending_change_for_response(applied)).into_response()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn pending_changes_router(
    pending_changes: PendingChangesService,
    plc_connections: PlcConnectionService,
    collection_groups: CollectionGroupService,
    tags: TagService,
    audit: AuditLogService,
    auth: AuthState,
    manager: Arc<CollectorManager>,
    controller: Arc<CollectionController>,
    events: broadcast::Sender<ServerEvent>,
) -> Router {
    let state = PendingChangesAdminState {
        pending_changes,
        plc_connections,
        collection_groups,
        tags,
        manager,
        controller,
        events,
        apply_lock: Arc::new(AsyncMutex::new(())),
        auth: auth.clone(),
        audit: audit.clone(),
    };
    Router::new()
        .route("/api/pending-changes", get(pending_changes_list))
        .route("/api/pending-changes/{id}", get(pending_changes_get))
        .route(
            "/api/pending-changes/{id}/apply",
            post(pending_changes_apply),
        )
        .route(
            "/api/pending-changes/{id}/cancel",
            post(pending_changes_cancel),
        )
        .route(
            "/api/pending-changes/{id}/requeue",
            post(pending_changes_requeue),
        )
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            SessionRoleGuard {
                auth: auth.clone(),
                min: Role::Admin,
                resource: "pending_changes",
                audit,
            },
            require_session_role,
        ))
        .layer(middleware::from_fn_with_state(
            AuthGate {
                auth,
                operation: OperationKind::Normal,
            },
            require_session,
        ))
}
