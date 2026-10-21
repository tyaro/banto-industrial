//! I1 CRUD の共通層（収集構成の反映・pending queue への振り分け・変更系エラーの
//! 写像）。接続・収集グループ・タグ・pending-changes・sink の各リソースが共有する。
//!
//! `rest.rs` から中身を変えずに移したもの（docs/banto-hub-driver-refactor-plan.md §8.1、
//! P0-1 の手順 3）。接続専用の部品は `rest/plc_connections.rs` に分けた。共通部品
//! （認証・ガード・監査の補助）は親の `rest.rs` にあり、`use super::{...}` で使う。
//! 外から使われるパス（`crate::rest::X`）は親の re-export で保つ。

use super::{
    actor_identity, broadcast, json, ApiError, Arc, AuditLogService, AuthState, BantoError,
    CollectionController, CollectionGroupService, CollectionState, CollectionStatus,
    CollectionStatusResponse, CollectorManager, CommissioningState, HeaderMap, IntoResponse, Json,
    PendingChange, PendingChangesService, PlcConnectionResponse, PlcConnectionService, Response,
    Serialize, ServerEvent, StatusCode, Tag, TagService, TagUpdateError,
};

// --- I1 CRUD (viewer-read / editor-write) + collector rebuild -------------

pub(super) fn default_payload_enabled() -> bool {
    true
}

/// Commit the catalog already preflighted in the write transaction, apply it
/// to the running collection if one is in flight, and notify admin-UI SSE
/// subscribers.
///
/// **#341（オーナー決定 2026-09-09 / 2026-09-14、docs/tag-server-design.md
/// §4.3）**: the single function every registry-mutation handler in this file,
/// every `crate::mcp` config tool AND `execute_pending_apply` (the
/// `/api/pending-changes/{id}/apply` handler's worker) funnel through - see
/// this fn's call sites - so this is the one place that turns a committed
/// registry change into a live, **non-stopping** reconfiguration of the
/// running collection. The whole of that work lives in
/// [`CollectionController::commit_catalog_and_apply_live`] (transition-lock
/// discipline, run-mode safety, `apply_config` partial reconfiguration,
/// broker-session sync, `configured_revision` + `config_changed`) - read that
/// method's doc comment before changing anything here.
///
/// Before #341 this fn only advanced the configured revision (plus the T19
/// S2-a 案B broker-session resync), and the pre-T14-3 live apply was hidden
/// behind a `legacy_live_reconfigure` flag that production always left off;
/// both are gone - the new path covers every caller, legacy router included
/// (it simply does nothing extra unless the controller reports `Running`).
///
/// `Err` means "the registry row is committed but the live/catalog apply
/// failed"; callers turn it into an API error rather than a silent 200 so an
/// operator never believes a change took effect when it did not. The message
/// is also recorded in `CollectorManager::last_error`
/// (`/api/v1/status` の `last_config_error`).
// T21 S1-b（docs/banto-hub-t21-design.md §5）: `pub(crate)` にして
// `crate::mcp` の構成補助ツール（`create_connection`/`delete_connection`）
// からも同じ経路を呼べるようにする。
/// #341 レビュー対応（2026-09-14）: 実行構成への反映に失敗したときの
/// **回復手段**をエラー文言に必ず添える。DB には既に入っているので、
/// 反映は「もう一度何か構成を変える」「収集を止めて開始し直す」
/// 「`POST /api/collection/reapply`（admin）を叩く」のいずれでも再試行
/// できる - どれも同じ `CollectionController::commit_catalog_and_apply_live`
/// を通るため。
pub(crate) const LIVE_APPLY_RECOVERY_HINT: &str =
    "（変更は保存済みです。次の構成変更、収集の停止→開始、または POST /api/collection/reapply で再反映できます）";

pub(crate) async fn commit_catalog_and_notify(
    controller: &CollectionController,
    events: &broadcast::Sender<ServerEvent>,
    resource: &str,
) -> Result<(), String> {
    let result = controller.commit_catalog_and_apply_live().await;
    // 成否によらず status watch を更新する - 失敗時も `configured_revision`
    // だけは進んでいる（catalog commit は成功したが実行構成への適用で
    // 失敗した）場合があり、購読者にはその事実をそのまま見せる。
    controller.refresh_status();
    let _ = events.send(ServerEvent::ResourceChanged {
        resource: resource.to_string(),
    });
    if let Err(err) = &result {
        eprintln!("banto-hub: {resource} 変更の実行構成への反映に失敗しました: {err}");
    }
    result
}

#[derive(Clone)]
pub(super) struct TagRegistryState {
    pub(super) plc_connections: PlcConnectionService,
    pub(super) collection_groups: CollectionGroupService,
    pub(super) tags: TagService,
    pub(super) auth: AuthState,
    pub(super) commissioning: CommissioningState,
    pub(super) audit: AuditLogService,
    pub(super) manager: Arc<CollectorManager>,
    pub(super) controller: Arc<CollectionController>,
    pub(super) events: broadcast::Sender<ServerEvent>,
    pub(super) pending_changes: PendingChangesService,
}

/// #341（オーナー決定 2026-09-09、docs/tag-server-design.md §4.3）:
/// 接続・グループ・タグの CRUD を pending queue へ回すかどうかの**唯一の
/// 判定**。`Some(status)` なら [`queue_pending_registry_change`]（REST）
/// あるいは `crate::mcp` の同等処理へ、`None` ならその場で DB へ書いて
/// [`commit_catalog_and_notify`] で反映する。
///
/// 条件は「**収集中 かつ ロックダウン済み**」の1つだけ:
///
/// - 試運転中（`locked_down == false`）は配線の誤りを直しながらタグを足す
///   工程なので、収集中でも queue を挟まず即時・無停止で反映する。護りは
///   トランザクション内 preflight・`revision` 楽観ロック・監査ログで足りる
///   （queue のフィンガープリントガードは queue を経由しない経路では不要）。
/// - ロックダウン済みは「構成凍結」の意図どおり、queue + 明示適用の
///   ワンクッションを必ず挟む（ヒューマンエラー防止）。
/// - 収集停止中はどちらの状態でもそのまま即時反映（従来どおり）。
/// - 判定は `state != Stopped`、つまり `Starting`/`Stopping`/`Faulted` も
///   ロックダウン済みでは queue 扱いにする（#341 レビュー対応2 で明示。
///   挙動は #341 以前から変えていない）。遷移の途中や失敗直後に直接
///   書き込みを始めるより、キューに載せて人の明示適用を待つ方が保守的で
///   あり、`Running` だけを特別扱いすると「開始中に滑り込んだ CRUD だけ
///   即座に反映される」という説明しづらい穴ができるため。
///
/// REST（[`TagRegistryState`] 経由）と MCP（`crate::mcp::McpState` 経由）の
/// 両方から呼べるよう、全体の状態ではなく実際に読む2つだけを受け取る
/// （[`compute_pending_base_fingerprint`] と同じ理由・同じ規律）。二重実装を
/// 避けるのは**判定条件**であり、queue への保存処理自体は REST/MCP で
/// それぞれの応答形式に合わせて別実装のままである。
pub(crate) fn registry_change_should_queue(
    controller: &CollectionController,
    commissioning: &CommissioningState,
) -> Option<CollectionStatus> {
    let status = controller.status();
    (status.state != CollectionState::Stopped && commissioning.is_locked_down()).then_some(status)
}

#[derive(Debug, Serialize)]
pub(super) struct QueuedPendingChangeResponse {
    queued: bool,
    pending: PendingChange,
    status: CollectionStatusResponse,
    message: String,
}

/// S1a レビュー対応（`docs/banto-hub-external-db-design.md`§2.2）: `pending_changes.payload`
/// 列の `plc_connections.create`/`.update` は接続のパスワードを持ちうる。R2b-1e
/// （docs/banto-hub-connection-settings-design.md §5.3）から、積む時点で封印した値
/// （`input.sealedPassword`）に替え、履歴（`applied`/`canceled`）へ移すときに外す
/// （`crate::pending_changes` のモジュール doc）。この関数は外部へ返す直前の写しにだけ掛け、
/// **DB の行は変えない**。
///
/// `payload.input.password` と `payload.input.sealedPassword` を取り除き、代わりに
/// `passwordSet` を入れる（`crate::pending_changes::redact_queued_password`）:
/// - `"plc_connections.create"`: 封印した値があるか `password` が非空なら `true`、
///   省略/`null`/空文字列なら `false`。
/// - `"plc_connections.update"`: **tri-state** - 封印した値（置き換え）なら `true`、
///   空文字列（消去）なら `false`、省略/`null`（現在のパスワードを維持）なら `null`
///   （この pending change 自体は「今の保存状態」を持たないので、`true`/`false` の
///   どちらでもなく `null` とする。現在値は `GET /api/plc-connections/{id}` の
///   `passwordSet` で別途確認できる）。
/// - 履歴へ移すときに替えた `passwordSet` はそのまま。
///
/// それ以外の`source`（`"plc_connections.delete"`、`collection_groups.*`、
/// `tags.*`）は無変更でそのまま返す - password を持ちうるのは
/// `plc_connections.create`/`.update`の2つだけ。
fn redact_pending_payload(source: &str, payload: &serde_json::Value) -> serde_json::Value {
    crate::pending_changes::redact_queued_password(source, payload)
}

/// [`redact_pending_payload`]を`PendingChange`全体に適用したコピーを返す -
/// list/get/cancel/requeue/apply/enqueue のあらゆる応答が、DB の生の行では
/// なく必ずこの関数を通した後の値を返すための唯一の入口。
pub(super) fn redact_pending_change_for_response(pending: PendingChange) -> PendingChange {
    let payload = redact_pending_payload(&pending.source, &pending.payload);
    PendingChange { payload, ..pending }
}

/// Per-resource staleness guard for the pending-change queue (TAG-P0-3
/// follow-up, 2026-08-12): captures the current DB state of the resource a
/// `plc_connections.update`/`.delete` or `collection_groups.update`/`.delete`
/// pending change targets, at the moment it is queued. `execute_pending_apply`
/// re-fetches and re-serializes the same way right before applying and
/// rejects the apply as a conflict if the strings differ (or the row is
/// gone) — this is a per-resource check, not a global `configured_revision`
/// comparison, because `commit_catalog_and_notify` bumps the global revision
/// on every successful apply and a global check would break applying
/// multiple unrelated queued changes back-to-back.
///
/// Deliberately NOT a hash: rows are small and `serde_json::to_string` on a
/// `#[derive(Serialize)]` struct with fixed field order is exactly as
/// comparable as a hash here, without adding a crypto dependency.
///
/// `*.create` sources get `None` (there is no prior row to go stale) and
/// `tags.*` sources get `None` (already guarded by `Tag`'s own
/// `expectedRevision` optimistic-lock mechanism — see
/// `TagUpdateError::RevisionConflict` — adding a second guard here would be
/// redundant).
///
/// T21 S1-b（docs/banto-hub-t21-design.md §5）: made `pub(crate)` and takes
/// the two services it actually reads directly (`&PlcConnectionService`/
/// `&CollectionGroupService`) instead of the full `TagRegistryState` - this
/// crate's private, admin-REST-only god-struct that also carries `AuthState`
/// (which needs a live login-verification closure and cannot be cheaply
/// fabricated from `crate::mcp`, whose API-key auth never touches it). The
/// body and the two behaviors it implements (`plc_connections.*` /
/// `collection_groups.*`) are otherwise unchanged - this is the minimal
/// decoupling needed so [`crate::mcp`]'s config tools can call the exact same
/// fingerprint logic REST uses, rather than re-implementing it.
pub(crate) async fn compute_pending_base_fingerprint(
    plc_connections: &PlcConnectionService,
    collection_groups: &CollectionGroupService,
    source: &str,
    payload: &serde_json::Value,
) -> Option<String> {
    let id = payload.get("id")?.as_i64()?;
    match source {
        // S1（§2.2、`PlcConnection::password`の doc comment）: フィンガー
        // プリントは「enqueue 時点と比べて行が変わったか」だけを見る比較用
        // 文字列であり、`pending_changes.base_fingerprint`列（DB に平文で
        // 残る・将来 GET で読めても構わない設計ではない）に平文パスワードを
        // 書き込まないよう、生の`PlcConnection`ではなく`PlcConnectionResponse`
        // （`passwordSet`のみ）をシリアライズする。パスワードだけを変える
        // 編集は fingerprint 上「変化なし」に見えうるが、既存の
        // `check_fingerprint_unchanged`はそもそも「同一エディタでの連続編集を
        // 弾く」ための粗い比較であり、この程度の取りこぼしは平文永続化を
        // 増やす代償に見合わない。
        "plc_connections.update" | "plc_connections.delete" => plc_connections
            .get(id)
            .await
            .ok()
            .map(PlcConnectionResponse::from)
            .and_then(|row| serde_json::to_string(&row).ok()),
        "collection_groups.update" | "collection_groups.delete" => collection_groups
            .get(id)
            .await
            .ok()
            .and_then(|row| serde_json::to_string(&row).ok()),
        _ => None,
    }
}

pub(super) async fn queue_pending_registry_change(
    state: &TagRegistryState,
    headers: &HeaderMap,
    source: &str,
    payload: serde_json::Value,
    status: CollectionStatus,
) -> RegistryMutationResult<Response> {
    let identity = actor_identity(headers, &state.auth);
    let base_fingerprint = compute_pending_base_fingerprint(
        &state.plc_connections,
        &state.collection_groups,
        source,
        &payload,
    )
    .await;
    let pending = state
        .pending_changes
        .create_pending(
            source,
            &payload,
            state.manager.configured_revision() as i64,
            base_fingerprint.as_deref(),
            identity.as_ref().map(|v| v.id.as_str()),
            identity.as_ref().map(|v| v.role.as_str()),
        )
        .await?;

    Ok((
        StatusCode::ACCEPTED,
        Json(QueuedPendingChangeResponse {
            queued: true,
            pending: redact_pending_change_for_response(pending),
            status: status.into(),
            message: "ロックダウン済みで収集中のため、変更を未適用キューに保存しました。"
                .to_string(),
        }),
    )
        .into_response())
}

pub(super) enum RegistryMutationError {
    Api(ApiError),
    /// #341（2026-09-14）: DB へのコミットは成功したが、その内容を catalog /
    /// 走行中の収集へ反映できなかった（[`commit_catalog_and_notify`] の
    /// `Err`）。レジストリ行自体は既に存在するので「作成/更新に失敗した」
    /// ではなく「反映に失敗した」ことを伝える専用のエラー - 走行中の収集は
    /// `apply_config` の all-or-nothing により元の構成のまま無傷で、
    /// 詳細は `/api/v1/status` の `last_config_error` にも残る。200 +
    /// 警告にしないのは、オペレータが「反映された」と誤解したまま次の
    /// 作業へ進むのを防ぐため。
    LiveApplyFailed(String),
    /// T18-1（docs/banto-hub-desktop-plan.md §9.4 TAG-UX-C 4点目）:
    /// [`TagService::update_tx`] が `TagUpdateError::RevisionConflict` を
    /// 返した場合の REST 表現 - `CollectionEditLocked` と同じ `409`
    /// パターンで、現在の（他セッションが先に更新した）`Tag` を丸ごと
    /// 返す。管理 UI はこれを使ってフォームをサーバー最新値へ更新する
    /// （差分表示 UI 自体は本 PR のスコープ外）。`banto_tags::TagUpdateError`
    /// と同じ理由で `Box` にしている（`clippy::large_enum_variant`）。
    TagRevisionConflict(Box<Tag>),
}

impl From<ApiError> for RegistryMutationError {
    fn from(error: ApiError) -> Self {
        Self::Api(error)
    }
}

impl From<BantoError> for RegistryMutationError {
    fn from(error: BantoError) -> Self {
        Self::Api(ApiError(error))
    }
}

impl From<TagUpdateError> for RegistryMutationError {
    fn from(error: TagUpdateError) -> Self {
        match error {
            TagUpdateError::Banto(error) => Self::Api(ApiError(error)),
            TagUpdateError::RevisionConflict(tag) => Self::TagRevisionConflict(tag),
        }
    }
}

impl IntoResponse for RegistryMutationError {
    fn into_response(self) -> Response {
        match self {
            Self::Api(error) => error.into_response(),
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
            Self::TagRevisionConflict(tag) => (
                StatusCode::CONFLICT,
                Json(json!({
                    "error": "tag_revision_conflict",
                    "message": "他のクライアントがこのタグを更新済みです。再読込してから保存してください。",
                    "tag": tag
                })),
            )
                .into_response(),
        }
    }
}

pub(super) type RegistryMutationResult<T> = Result<T, RegistryMutationError>;

// #341（2026-09-14）: `require_collection_stopped`（収集中なら一律 409
// `collection_edit_locked`）はここにあったが撤去した。従来は queue 分岐の
// 直後にあって到達不能だったところ、[`registry_change_should_queue`] の
// 新しい条件では「試運転中 + 収集中」がここへ到達してしまい、即時反映の
// 決定と真っ向から矛盾するため残せない。
