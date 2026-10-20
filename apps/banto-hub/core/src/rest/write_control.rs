//! 書き込み受付トグル API（`/api/write-control/*`）。
//!
//! `rest.rs` から中身を変えずに移したもの（docs/banto-hub-driver-refactor-plan.md §8.1、
//! P0-1 の手順 2）。共通部品（認証・ガード・監査の補助）は親の `rest.rs` にあり、
//! `use super::{...}` で使う。外から使われるパス（`crate::rest::X`）は親の re-export で
//! 保つ。

use super::{
    actor_identity, broadcast, json, middleware, post, record_write, require_session,
    require_session_role, Arc, AuditEntry, AuditLogService, AuthGate, AuthState, CollectorManager,
    HeaderMap, IntoResponse, Json, OperationKind, Response, Role, Router, Serialize, ServerEvent,
    SessionRoleGuard, State, StatusCode, ToSchema, UnverifiedStopException, WriteControl,
    WriteControlChange,
};

// --- 書き込み受付トグル (T2-4、設計 §6-6): admin 限定、CSRF + bearer -------
//
// `POST /api/write-control/enable`/`disable` は
// `crate::write_control::WriteControl`（ライブフラグ、既定 enabled・再起動
// で永続値を復元）を切り替え、`crate::write_control::persist_enabled` で
// 永続値も更新する（`WriteControl` のモジュール doc comment 参照 - 永続値は
// 次回起動時のライブフラグの初期値としてそのまま復元される。2026-09-09
// オーナー決定 #340）。

#[derive(Clone)]
struct WriteControlAdminState {
    write_control: Arc<WriteControl>,
    manager: Arc<CollectorManager>,
    auth: AuthState,
    audit: AuditLogService,
    events: broadcast::Sender<ServerEvent>,
}

/// `GET /api/v1/status` の `write_enabled`/`write_was_enabled_before_restart`
/// と同じ形の応答（`POST /api/write-control/enable|disable` の応答）。
/// `write_was_enabled_before_restart` は「起動時に復元した値」を指す
/// （フィールド名は外部クライアント Thermal Monitor との互換のため変更
/// しない。2026-09-09 オーナー決定 #340）。
/// `persistence_warning`（#433）: 停止が片方にしか保存できなかったとき等の
/// 説明（[`crate::write_control::WriteControlChange::warning`]）。無ければ `null`。
#[derive(Debug, Serialize, ToSchema)]
struct WriteControlStatusResponse {
    write_enabled: bool,
    write_was_enabled_before_restart: bool,
    persistence_warning: Option<String>,
}

/// 停止・再開の本体は [`WriteControl::set_enabled_detached`]（#433。DB と状態
/// ファイルの 2 か所に保存する - `crate::write_control` のモジュール doc 参照）。
/// このハンドラは結果を応答と監査に変換するだけ:
/// - **disable（非常停止）**: ライブフラグは必ず即座に落ちる。どちらか一方に
///   保存できれば 200（再起動しても停止のまま）。片方が失敗したときは
///   `persistence_warning` にその旨を載せる。両方失敗したときだけ 500
///   `write_control_persist_failed`（書き込みは止まったまま）。
/// - **enable**: 両方に保存できたときだけライブフラグを立てて 200。片方でも
///   失敗したら 500 `write_control_persist_failed`（ライブフラグには触れない）。
///   保存中、または保存を待っている間に停止が割り込んだら 409
///   `write_control_stop_interrupted`。
///
/// 監査ログは成功/失敗の両方を記録する（[`write_control_audit_detail`]）。
/// `ServerEvent::ResourceChanged` は停止では常に（ライブフラグは必ず落ちる）、
/// 再開では成功したときだけ送る。監査と通知は停止・再開と**同じ別タスクの
/// 中で**行う（[`record_write_control_outcome`]）ので、クライアントが切断して
/// このハンドラの future が捨てられても残る（#437 の「非常停止の監査を
/// 失わない」を切断にも広げる。`crate::write_control` のモジュール doc
/// 「呼び出し側が future を捨てても」）。応答はその完了の後に返す（従来どおり）。
async fn write_control_set(
    state: &WriteControlAdminState,
    headers: &HeaderMap,
    enabled: bool,
    action: &str,
    unverified_stop: bool,
) -> Response {
    let identity = actor_identity(headers, &state.auth);
    let actor_id = identity.as_ref().map(|i| i.id.as_str());

    let follow_up = {
        let audit = state.audit.clone();
        let auth = state.auth.clone();
        let headers = headers.clone();
        let events = state.events.clone();
        let action = action.to_string();
        move |change: WriteControlChange| async move {
            record_write_control_outcome(
                &audit,
                &auth,
                &headers,
                &events,
                &action,
                unverified_stop,
                &change,
            )
            .await;
        }
    };
    // 別タスクで最後まで走らせる（クライアントが切断してこのハンドラの
    // future が捨てられても、停止・再開と監査を途中で止めない。
    // `crate::write_control` のモジュール doc「呼び出し側が future を捨てても」）。
    let change = state
        .write_control
        .set_enabled_detached(&state.manager.pool(), enabled, actor_id, follow_up)
        .await;

    if !change.succeeded() {
        if change.interrupted_by_stop {
            return (
                StatusCode::CONFLICT,
                Json(json!({
                    "error": "write_control_stop_interrupted",
                    "message": change.warning().unwrap_or_default()
                })),
            )
                .into_response();
        }
        return write_control_persist_failed_response(&change.warning().unwrap_or_default());
    }

    Json(WriteControlStatusResponse {
        write_enabled: state.write_control.is_enabled(),
        write_was_enabled_before_restart: state.write_control.was_enabled_before_restart(),
        persistence_warning: change.warning(),
    })
    .into_response()
}

/// [`write_control_set`] の停止・再開の後始末（通知と監査）。
/// [`WriteControl::set_enabled_detached`] の `follow_up` として、停止・再開と
/// 同じ別タスクの中で呼ぶ（クライアントの切断で捨てられない）。監査は
/// `AuditLogService::record`（3 秒で打ち切って保留、#437）を通る。
async fn record_write_control_outcome(
    audit: &AuditLogService,
    auth: &AuthState,
    headers: &HeaderMap,
    events: &broadcast::Sender<ServerEvent>,
    action: &str,
    unverified_stop: bool,
    change: &WriteControlChange,
) {
    let detail = with_session_exception_mark(write_control_audit_detail(change), unverified_stop);

    if !change.requested_enabled || change.succeeded() {
        let _ = events.send(ServerEvent::ResourceChanged {
            resource: "write_control".to_string(),
        });
    }

    if !change.succeeded() {
        record_write_control_failure(audit, auth, headers, action, detail).await;
        return;
    }

    record_write(
        audit,
        auth,
        headers,
        action,
        "write_control",
        Some("1"),
        Some(detail),
    )
    .await;
}

/// 停止・再開の監査の `detail`（#433）。`persisted` は「再起動後も要求どおり
/// に残るか」（停止はどちらか一方、再開は両方）。`persistedDb` /
/// `persistedFile`（状態ファイルを持たない構成では `null`）と、失敗した側の
/// `dbError` / `fileError`、停止に割り込まれた再開の `interruptedByStop`、
/// 停止の DB 保存が制限時間を超えた `dbTimedOut`（失敗ではなくタイムアウト。
/// 書き込みは続いていて遅れて完了し得る - `crate::write_control` のモジュール
/// doc「停止の DB 保存は 5 秒で打ち切る」）。
/// MCP（`crate::mcp::tool_set_write_control`）も同じ形を使う。
pub(crate) fn write_control_audit_detail(
    change: &crate::write_control::WriteControlChange,
) -> serde_json::Value {
    let mut detail = json!({
        "enabled": change.requested_enabled,
        "persisted": change.succeeded(),
        "persistedDb": change.db_saved(),
        "persistedFile": change.file_saved(),
    });
    if let Some(object) = detail.as_object_mut() {
        if let Some(err) = change.db_error() {
            object.insert("dbError".to_string(), json!(err));
        }
        if let Some(err) = change.file_error() {
            object.insert("fileError".to_string(), json!(err));
        }
        if change.interrupted_by_stop {
            object.insert("interruptedByStop".to_string(), json!(true));
        }
        if change.db_timed_out {
            object.insert("dbTimedOut".to_string(), json!(true));
        }
    }
    detail
}

/// [`write_control_set`] の失敗時の監査行 - `record_write`（成功専用、
/// `result: "ok"` 固定）とは別に、`result: "failed"` を記録する。
async fn record_write_control_failure(
    audit: &AuditLogService,
    auth: &AuthState,
    headers: &HeaderMap,
    action: &str,
    detail: serde_json::Value,
) {
    let identity = actor_identity(headers, auth);
    audit
        .record(AuditEntry {
            actor_username: identity.as_ref().map(|i| i.id.as_str()),
            actor_role: identity.as_ref().map(|i| i.role.as_str()),
            action,
            resource: "write_control",
            entity_id: Some("1"),
            detail: Some(detail),
            origin: "rest",
            result: "failed",
        })
        .await;
}

/// #431: 照合できなかったため停止のみ例外で通した要求の監査行に付ける印。
/// `detail.sessionCheck = "unverified_stop_exception"` と、人が読む説明。
fn with_session_exception_mark(
    mut detail: serde_json::Value,
    unverified_stop: bool,
) -> serde_json::Value {
    if unverified_stop {
        if let Some(object) = detail.as_object_mut() {
            object.insert(
                "sessionCheck".to_string(),
                json!("unverified_stop_exception"),
            );
            object.insert(
                "sessionCheckNote".to_string(),
                json!("照合できなかったため、停止のみ例外で許可"),
            );
        }
    }
    detail
}

/// #340: `write_control_set` の永続化失敗を伝える 500。`writes_disabled`
/// 等（このファイル上部の `*_response` 群）と同じ `{"error": ..., "message": ...}`
/// 形の ad hoc JSON - `WriteControlStatusResponse` の `kind` タグ付き
/// `BantoError`/`ApiError` 系とは別の、この機能専用の安定した機械可読コード。
fn write_control_persist_failed_response(message: &str) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({
            "error": "write_control_persist_failed",
            "message": message
        })),
    )
        .into_response()
}

/// #431: 再開は「止める」操作ではない - 照合できなければ 500（例外なし）。
pub(super) const WRITE_CONTROL_ENABLE_OPERATION: OperationKind = OperationKind::Normal;

async fn write_control_enable(
    State(state): State<WriteControlAdminState>,
    headers: HeaderMap,
) -> Response {
    write_control_set(&state, &headers, true, "enable", false).await
}

/// #431: 緊急停止は「書き込みを止める」操作だと宣言する。DB が応答しない
/// ときも、それまで有効だったセッションなら受け付ける
/// （[`session_gate_decision`]）。この宣言が例外の唯一の入口。
pub(super) const WRITE_CONTROL_DISABLE_OPERATION: OperationKind = OperationKind::StopWrites;

async fn write_control_disable(
    State(state): State<WriteControlAdminState>,
    headers: HeaderMap,
    exception: Option<axum::Extension<UnverifiedStopException>>,
) -> Response {
    write_control_set(&state, &headers, false, "disable", exception.is_some()).await
}

pub(super) fn write_control_router(
    write_control: Arc<WriteControl>,
    manager: Arc<CollectorManager>,
    audit: AuditLogService,
    auth: AuthState,
    events: broadcast::Sender<ServerEvent>,
) -> Router {
    let state = WriteControlAdminState {
        write_control,
        manager,
        auth: auth.clone(),
        audit: audit.clone(),
        events,
    };
    // #431: ルートごとに、ハンドラの隣で宣言した操作の種類でゲートを組む
    // （同じゲートを 2 本のルートで共有しない - 例外が再開に漏れないように）。
    let gated = |router: Router<WriteControlAdminState>, operation: OperationKind| {
        router
            .with_state(state.clone())
            .layer(middleware::from_fn_with_state(
                SessionRoleGuard {
                    auth: auth.clone(),
                    min: Role::Admin,
                    resource: "write_control",
                    audit: audit.clone(),
                },
                require_session_role,
            ))
            .layer(middleware::from_fn_with_state(
                AuthGate {
                    auth: auth.clone(),
                    operation,
                },
                require_session,
            ))
    };
    gated(
        Router::new().route("/api/write-control/enable", post(write_control_enable)),
        WRITE_CONTROL_ENABLE_OPERATION,
    )
    .merge(gated(
        Router::new().route("/api/write-control/disable", post(write_control_disable)),
        WRITE_CONTROL_DISABLE_OPERATION,
    ))
}
