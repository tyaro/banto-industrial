//! 書き込み監査の閲覧 API（`/api/write-audit/list`）。
//!
//! `rest.rs` から中身を変えずに移したもの（docs/banto-hub-driver-refactor-plan.md §8.1、
//! P0-1 の手順 2）。共通部品（認証・ガード・監査の補助）は親の `rest.rs` にあり、
//! `use super::{...}` で使う。外から使われるパス（`crate::rest::X`）は親の re-export で
//! 保つ。

use super::{
    middleware, post, require_session, require_session_role, ApiError, AuditLogService, AuthGate,
    AuthState, Json, ListParams, ListResult, OperationKind, Role, Router, SessionRoleGuard, State,
    WriteAuditEntry, WriteAuditService,
};

// --- 書き込み監査の閲覧 (T2-4、設計 §6-3): admin 限定、CSRF + bearer -------

#[derive(Clone)]
struct WriteAuditAdminState {
    write_audit: WriteAuditService,
}

async fn write_audit_list(
    State(state): State<WriteAuditAdminState>,
    Json(params): Json<ListParams>,
) -> Result<Json<ListResult<WriteAuditEntry>>, ApiError> {
    Ok(Json(state.write_audit.list(params).await?))
}

/// `/api/write-audit/list`（`crate::audit`の `audit_log_router` と同型:
/// admin 限定、ページング付き POST 1本）。
pub(super) fn write_audit_router(
    write_audit: WriteAuditService,
    audit: AuditLogService,
    auth: AuthState,
) -> Router {
    let state = WriteAuditAdminState { write_audit };
    Router::new()
        .route("/api/write-audit/list", post(write_audit_list))
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            SessionRoleGuard {
                auth: auth.clone(),
                min: Role::Admin,
                resource: "write_audit",
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
