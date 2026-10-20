//! 収集の開始・停止・再適用などのライフサイクル制御 API（`/api/collection/*`）。
//!
//! `rest.rs` から中身を変えずに移したもの（docs/banto-hub-driver-refactor-plan.md §8.1、
//! P0-1 の手順 2）。共通部品（認証・ガード・監査の補助）は親の `rest.rs` にあり、
//! `use super::{...}` で使う。外から使われるパス（`crate::rest::X`）は親の re-export で
//! 保つ。

use super::{
    actor_identity, broadcast, get, json, middleware, post, record_write, require_session,
    require_session_role, ApiError, Arc, AuditEntry, AuditLogService, AuthGate, AuthState,
    BantoError, CollectionController, CollectionStatus, CollectorManager, Deserialize, ErrorBody,
    FieldError, HeaderMap, IntoResponse, Json, OperationKind, QuitInProgress, Response, Role,
    Router, RunMode, Serialize, ServerEvent, SessionRoleGuard, SimulationCoverageReport, State,
    StatusCode,
};

// --- collection lifecycle control (T14-4): admin + CSRF --------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CollectionStatusResponse {
    state: String,
    mode: String,
    run_id: Option<u64>,
    configured_revision: u64,
    running_revision: u64,
    last_error: Option<String>,
}

impl From<CollectionStatus> for CollectionStatusResponse {
    fn from(status: CollectionStatus) -> Self {
        Self {
            state: status.state.as_str().to_string(),
            mode: status.mode.as_str().to_string(),
            run_id: status.run_id,
            configured_revision: status.configured_revision,
            running_revision: status.running_revision,
            last_error: status.last_error,
        }
    }
}

#[derive(Debug, Deserialize)]
struct CollectionModeRequest {
    mode: String,
}

#[derive(Clone)]
struct CollectionAdminState {
    controller: Arc<CollectionController>,
    /// T15-2: `GET /api/collection/simulation-coverage`が
    /// `CollectorManager::simulation_coverage_report`を呼ぶために必要
    /// (`CollectionController`自身はレジストリを読まない、ライフサイクル
    /// 状態機械のみ - `crate::controller`のモジュール doc comment参照)。
    manager: Arc<CollectorManager>,
    auth: AuthState,
    audit: AuditLogService,
    events: broadcast::Sender<ServerEvent>,
}

async fn collection_control_result(
    state: &CollectionAdminState,
    headers: &HeaderMap,
    action: &str,
    status: CollectionStatus,
) -> Json<CollectionStatusResponse> {
    record_write(
        &state.audit,
        &state.auth,
        headers,
        action,
        "collection",
        Some("1"),
        Some(json!({
            "state": status.state.as_str(),
            "mode": status.mode.as_str(),
            "runId": status.run_id,
            "configuredRevision": status.configured_revision,
            "runningRevision": status.running_revision,
            "lastError": status.last_error,
        })),
    )
    .await;
    let _ = state.events.send(ServerEvent::ResourceChanged {
        resource: "collection".to_string(),
    });
    Json(status.into())
}

async fn collection_start(
    State(state): State<CollectionAdminState>,
    headers: HeaderMap,
) -> Response {
    match state.controller.start(RunMode::Configured).await {
        Ok(status) => collection_control_result(&state, &headers, "start", status)
            .await
            .into_response(),
        Err(refused) => quit_in_progress_response(refused),
    }
}

async fn collection_start_all_simulation(
    State(state): State<CollectionAdminState>,
    headers: HeaderMap,
) -> Response {
    match state.controller.start(RunMode::AllSimulation).await {
        Ok(status) => collection_control_result(&state, &headers, "start_all_simulation", status)
            .await
            .into_response(),
        Err(refused) => quit_in_progress_response(refused),
    }
}

/// #392 A2 レビュー（2026-10-12）: デスクトップのトレイの「アプリを終了」が
/// 終了の受付を始めた後の開始（`start`・`start-all-simulation`・走行中の
/// `mode` 切り替え）は、`409 Conflict` で**明示的に断る**
/// （`crate::controller` のモジュール doc「終了の受付」）。開始は実行されて
/// いないので、監査（`record_write`、成功専用）にも `ResourceChanged` にも
/// 載せない。本文は共通の [`ErrorBody`]（`{ kind: "other", message }`）-
/// 管理 UI の `collectionControlAdmin.ts` がこの形の `message` をそのまま
/// トーストに出すため（独自の `{ error, message }` にすると
/// 「409 Conflict」としか出ない）。
pub(super) fn quit_in_progress_response(refused: QuitInProgress) -> Response {
    (
        StatusCode::CONFLICT,
        Json(ErrorBody::Other {
            message: refused.to_string(),
        }),
    )
        .into_response()
}

async fn collection_stop(
    State(state): State<CollectionAdminState>,
    headers: HeaderMap,
) -> Json<CollectionStatusResponse> {
    let status = state.controller.stop().await;
    collection_control_result(&state, &headers, "stop", status).await
}

/// #341 レビュー対応（2026-09-14）: `POST /api/collection/reapply`（admin）-
/// **最新の DB 内容を catalog と実行構成へ適用し直す**。他の
/// `/api/collection/*` と違い、収集のライフサイクル（start/stop/mode）には
/// 一切触らない。
///
/// 用途は「`live_reconfigure_failed`（500）で反映だけ失敗した後の回復」。
/// レジストリ行は既にコミット済みなので、同じ
/// [`CollectionController::commit_catalog_and_apply_live`]
/// をもう一度通せばよい（収集 `Running` なら無停止で実行構成まで、
/// `Stopped` 等なら catalog だけ）- 収集を止めて開始し直す必要はない。
/// 冪等（何度呼んでも同じ DB 状態を適用し直すだけ）。
///
/// MCP には**あえて足していない**: 回復操作であって構成操作ではなく、
/// `crate::mcp` の構成ツールはどれも自分の mutation の直後に同じ経路を
/// 通るため（T21 の「REST と二重実装しない」規律）。
async fn collection_reapply(
    State(state): State<CollectionAdminState>,
    headers: HeaderMap,
) -> Response {
    match state.controller.commit_catalog_and_apply_live().await {
        Ok(()) => {
            let status = state.controller.status();
            collection_control_result(&state, &headers, "reapply", status)
                .await
                .into_response()
        }
        Err(message) => {
            record_collection_reapply_failure(&state, &headers, &message).await;
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": "live_reconfigure_failed",
                    "message": format!("実行構成への反映に失敗しました: {message}"),
                })),
            )
                .into_response()
        }
    }
}

/// [`collection_reapply`] の失敗時の監査行 - `record_write`（成功専用、
/// `result: "ok"` 固定）とは別に `result: "failed"` で記録する
/// （#340 の [`record_write_control_failure`] と同じ型）。
async fn record_collection_reapply_failure(
    state: &CollectionAdminState,
    headers: &HeaderMap,
    error: &str,
) {
    let identity = actor_identity(headers, &state.auth);
    state
        .audit
        .record(AuditEntry {
            actor_username: identity.as_ref().map(|i| i.id.as_str()),
            actor_role: identity.as_ref().map(|i| i.role.as_str()),
            action: "reapply",
            resource: "collection",
            entity_id: Some("1"),
            detail: Some(json!({ "error": error })),
            origin: "rest",
            result: "failed",
        })
        .await;
}

async fn collection_set_mode(
    State(state): State<CollectionAdminState>,
    headers: HeaderMap,
    Json(body): Json<CollectionModeRequest>,
) -> Result<Response, ApiError> {
    let mode = match body.mode.as_str() {
        "configured" => RunMode::Configured,
        "all_simulation" => RunMode::AllSimulation,
        _ => {
            return Err(ApiError(BantoError::Validation {
                field_errors: vec![FieldError {
                    field: "mode".to_string(),
                    message: "configured または all_simulation を指定してください".to_string(),
                }],
            }))
        }
    };
    match state.controller.set_mode(mode).await {
        Ok(status) => Ok(
            collection_control_result(&state, &headers, "set_mode", status)
                .await
                .into_response(),
        ),
        Err(refused) => Ok(quit_in_progress_response(refused)),
    }
}

/// T15-2 (docs/banto-hub-desktop-plan.md §9.7): all-simulation 開始前の
/// プリフライト - `CollectorManager::simulation_coverage_report`をそのまま
/// 返す。`start(AllSimulation)`をブロックしないので副作用は一切無く、
/// `record_write`/`ServerEvent::ResourceChanged`も送らない(表示専用の
/// 読み取り API - 他の`collection_*`ハンドラのような「状態が変わる操作」
/// ではない)。
async fn collection_simulation_coverage(
    State(state): State<CollectionAdminState>,
) -> Result<Json<SimulationCoverageReport>, ApiError> {
    let report = state
        .manager
        .simulation_coverage_report()
        .await
        .map_err(BantoError::Storage)
        .map_err(ApiError)?;
    Ok(Json(report))
}

pub(super) fn collection_control_router(
    controller: Arc<CollectionController>,
    manager: Arc<CollectorManager>,
    audit: AuditLogService,
    auth: AuthState,
    events: broadcast::Sender<ServerEvent>,
) -> Router {
    let state = CollectionAdminState {
        controller,
        manager,
        auth: auth.clone(),
        audit: audit.clone(),
        events,
    };
    Router::new()
        .route("/api/collection/start", post(collection_start))
        .route(
            "/api/collection/start-all-simulation",
            post(collection_start_all_simulation),
        )
        .route("/api/collection/stop", post(collection_stop))
        // #341 レビュー対応（2026-09-14）: 反映失敗からの回復手段
        // （`LIVE_APPLY_RECOVERY_HINT`）。`collection_reapply` の doc
        // comment 参照。
        .route("/api/collection/reapply", post(collection_reapply))
        .route(
            "/api/collection/mode",
            post(collection_set_mode).put(collection_set_mode),
        )
        .route(
            "/api/collection/simulation-coverage",
            get(collection_simulation_coverage),
        )
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            SessionRoleGuard {
                auth: auth.clone(),
                min: Role::Admin,
                resource: "collection",
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
