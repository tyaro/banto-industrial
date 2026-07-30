//! REST surface for the embedded server (spec §11.1): a LAN browser's
//! `HttpDataProvider` (Phase B, `packages/admin-core/src/providers/tauri.ts`
//! is the wire contract it must match) hits the same service layer and DB
//! `src-tauri`'s Tauri commands use.
//!
//! ## Route table
//!
//! | Method | Path               | Body           | Response              |
//! |--------|--------------------|----------------|------------------------|
//! | GET    | `/api/auth/status`   | -              | `{initialized}` (NO auth required) |
//! | POST   | `/api/auth/setup`     | `{username,password,displayName}` | `{success,error?,token?}` (needs `allow_setup`) |
//! | POST   | `/api/auth/login`    | `{username,password}` | `{success,error?,token?}` |
//! | POST   | `/api/auth/logout`   | -              | 200                    |
//! | GET    | `/api/auth/check`    | -              | `bool`                 |
//! | GET    | `/api/auth/identity` | -              | `Identity \| null`     |
//! | POST   | `/api/auth/change-password` | `{currentPassword,newPassword}` | `{success}` (auth required) |
//! | GET    | `/api/events`        | -              | SSE stream of `ServerEvent` |
//! | GET    | `/api/users`         | -              | `UserSummary[]` (admin) |
//! | POST   | `/api/users`         | `{username,password,displayName,role}` | `UserIdentityResponse` (admin) |
//! | PUT    | `/api/users/{id}`    | `{displayName,role}` | `UserSummary` (admin) |
//! | POST   | `/api/users/{id}/reset-password` | `{newPassword}` | `{success}` (admin) |
//! | DELETE | `/api/users/{id}`    | -              | 204 (admin)             |
//! | GET    | `/api/ui-settings/{key}` | -          | `{value: string \| null}` (any role) |
//! | PUT    | `/api/ui-settings/{key}` | `{value}`  | 204 (any role)          |
//! | POST   | `/api/audit-log/list` | `ListParams`   | `ListResult<AuditLogEntry>` (admin) |
//! | GET    | `/api/audit-log/config` | -            | `AuditSettings` (admin) |
//! | PUT    | `/api/audit-log/config` | `AuditSettings` | `AuditSettings` (admin) |
//! | POST   | `/api/backups`        | -              | `BackupInfo` (admin, spec M17) |
//! | GET    | `/api/backups`        | -              | `BackupInfo[]` (admin)  |
//! | GET    | `/api/backups/{fileName}` | -          | raw bytes, `Content-Disposition: attachment` (admin) |
//! | POST   | `/api/backups/restore?fileName=` | raw bytes (`application/octet-stream`) | 204 (admin) |
//! | POST   | `/api/backups/{fileName}/restore` | -   | 204 (admin)             |
//! | GET    | `/api/backups/pending-restore` | -      | `PendingRestoreInfo \| null` (admin) |
//! | DELETE | `/api/backups/pending-restore` | -      | 204 (admin)             |
//!
//! `/api/ui-settings/*` (spec M12 SettingsProvider migration): per-user UI
//! settings (theme/preset/dock layout), namespaced by the caller's own
//! `username` (`SettingsService::ui_get`/`ui_set` - see that module for the
//! `ui.{username}.{key}` storage key scheme). Guarded by `require_auth`
//! alone - unlike `/api/users`, there is no role floor: a `viewer` may
//! freely read/write their OWN UI preferences, they just cannot touch
//! anyone else's (there is no way to name another user's key over this
//! wire - `username` always comes from the caller's own bearer token, never
//! a request parameter).
//!
//! `/api/auth/status` and `/api/auth/setup` are deliberately NOT behind
//! `require_auth` - the login page needs `status` before any session exists,
//! and `setup` is how the very first session gets created. `setup` is
//! additionally gated by an `allow_setup` flag (spec §8.2): the Tauri app
//! always passes `false` (desktop first-run goes through the `auth_setup`
//! Tauri command instead, spec §10), while `relay-wright-serve` enables it via
//! `BANTO_ALLOW_SETUP=1` so this REST path is exercisable standalone.
//!
//! Every `/api/*` route requires the `X-Banto-Client: banto` header
//! (`banto_server::csrf`) and, except for the auth routes themselves, a
//! valid bearer token (`banto_server::auth::require_auth`).
//!
//! ## RBAC (spec M10, `docs/roadmap.md`)
//!
//! On top of `require_auth` (valid session, any role), all `/api/users`
//! routes are additionally gated by [`require_role_at_least`]: it
//! re-resolves the bearer token to an [`Identity`], parses `Identity.role`
//! into [`Role`], and rejects with `403 { "kind": "forbidden" }`
//! (`banto_core::ErrorBody::Forbidden`) if the caller's role is not at least
//! the route's minimum. Only `admin` can manage other accounts. Future
//! resources (R1-B: PLC connections/collection groups/tags/display groups)
//! follow the same `viewer` read / `editor`+ write / `admin`-only pattern
//! items used to demonstrate in the banto template.
//!
//! ## Audit log (spec M14, `docs/roadmap.md`)
//!
//! Every mutating handler above (`users` create/update/delete, password
//! reset, self-service password change) records a `crate::audit::AuditEntry`
//! to [`crate::audit::AuditLogService`] once its underlying service call has
//! already succeeded (`origin: "rest"`); [`require_role_at_least`] records
//! `action: "denied"` when an authenticated caller's role is too low;
//! [`audited_credential_verifier`] records `login`/`login_failed`;
//! [`audit_logout_middleware`] records `logout`; and `auth_setup_handler`
//! records `setup`. Read routes (`list`/`get`) are never audited. The trail
//! itself is only readable via `POST /api/audit-log/list`, `admin`-only.
//!
//! `/api/backups/*` (spec M17): `admin`-only, guarded the same way
//! `/api/users/*`/`/api/audit-log/*` are. `POST /api/backups` records
//! `action: "backup"`; either restore-staging route records
//! `action: "restore_staged"`; `DELETE /api/backups/pending-restore` records
//! `action: "restore_cancelled"` - all `resource: "backups"`. Reads (`GET
//! /api/backups`, the per-file download, `GET .../pending-restore`) are
//! never audited, same "read routes are never audited" convention as
//! everywhere else in this module. `action: "restore_applied"` is
//! deliberately NEVER recorded from here - a staged restore is only ever
//! APPLIED at the next process start, before any REST router (or pool) even
//! exists yet (spec M17: "稼働中のプールの差し替えはしない") - see
//! `crate::backup::BackupService::apply_pending_restore_at_startup`'s doc
//! comment and its callers in `src-tauri`'s `run()`/`bin/relay-wright-serve.rs`'s
//! `main`, which record that entry themselves once a fresh `AuditLogService`
//! exists. `POST /api/backups/restore`'s request body is raw bytes
//! (`Content-Type: application/octet-stream`), not JSON or multipart - this
//! workspace has no multipart dependency (spec M17 design decision:
//! "依存追加はしない") - with the uploaded file's original name passed as a
//! `?fileName=` query parameter purely for the audit `detail`/error
//! messages, never as a filesystem path (the actual bytes are always staged
//! under the service's own fixed `restore-pending.sqlite3` name - see
//! `crate::backup::BackupService::stage_restore_from_bytes`).

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use banto_core::{BantoError, ErrorBody, ListParams, ListResult};
use banto_server::{
    auth_routes, require_auth, require_banto_client_header, sse_route, ApiError, AuthState,
    Identity, ServerEvent,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::str::FromStr;
use tokio::sync::broadcast;

use crate::audit::{AuditEntry, AuditLogService};
use crate::backup::{BackupInfo, BackupService, PendingRestoreInfo};
use crate::settings::{AuditSettings, SettingsService};
use crate::users::{Role, UserIdentity, UserSummary, UsersService};
use crate::write_rules::{WriteRuleDetail, WriteRuleInput, WriteRuleService};
use crate::write_targets::{WriteTarget, WriteTargetInput, WriteTargetService};

/// Request-body size cap for `POST /api/backups/restore` (spec M17: "サイズ
/// 上限（例256MB）を設ける"). Applied via `DefaultBodyLimit` on
/// [`backups_router`] - axum's own built-in default is 2MB
/// (`axum::extract::DefaultBodyLimit`), far too small for an uploaded DB
/// backup.
const MAX_RESTORE_UPLOAD_BYTES: usize = 256 * 1024 * 1024;

/// Resolve the caller's [`Identity`] from its bearer token, best-effort
/// (spec M14): every audit-recording call site needs "who did this", and
/// every one of them runs AFTER `require_auth`/`require_role_at_least` has
/// already proven the token valid, so this should always resolve - `None`
/// here is a defensive fallback (e.g. the token expired in the instant
/// between the guard and the handler running), not an expected path. Shared
/// by the users write handlers below; auth-flow events (login/setup/
/// logout) resolve their own actor differently since they run before or
/// without a caller session.
fn actor_identity(headers: &HeaderMap, auth: &AuthState) -> Option<Identity> {
    bearer_token(headers).and_then(|token| auth.identity_for(token))
}

/// Record a successful write (spec M14: create/update/delete/password_reset
/// etc.) once the service call it follows has already succeeded. Resolves
/// the actor from the same bearer token `require_auth`/`require_role_at_least`
/// validated - see [`actor_identity`]. `origin` is always `"rest"` at every
/// call site in this module (the REST layer); kept as a parameter rather
/// than hardcoded only so this helper reads the same as the audit
/// entry it builds.
async fn record_write(
    audit: &AuditLogService,
    auth: &AuthState,
    headers: &HeaderMap,
    action: &str,
    resource: &str,
    entity_id: &str,
    detail: Option<serde_json::Value>,
) {
    let identity = actor_identity(headers, auth);
    audit
        .record(AuditEntry {
            actor_username: identity.as_ref().map(|i| i.id.as_str()),
            actor_role: identity.as_ref().map(|i| i.role.as_str()),
            action,
            resource,
            entity_id: Some(entity_id),
            detail,
            origin: "rest",
            result: "ok",
        })
        .await;
}

/// `State` for [`require_role_at_least`]: the `AuthState` needed to resolve
/// a bearer token back to an [`Identity`], the minimum [`Role`] the guarded
/// routes require, the `resource` name to tag a denial with (spec M14), and
/// the `AuditLogService` to record that denial to.
#[derive(Clone)]
struct RoleGuard {
    auth: AuthState,
    min: Role,
    resource: &'static str,
    audit: AuditLogService,
}

fn forbidden_response() -> Response {
    (StatusCode::FORBIDDEN, Json(ErrorBody::Forbidden)).into_response()
}

/// Axum middleware (spec M10 RBAC): stacked *after* `require_auth` on a
/// router, so a request has already been proven to carry a valid bearer
/// token by the time this runs. Re-resolves that token to an [`Identity`],
/// parses `Identity.role`, and rejects with `403
/// { "kind": "forbidden" }` unless the caller's role is at least
/// `guard.min`. Attach via
/// `middleware::from_fn_with_state(RoleGuard { auth, min, resource, audit }, require_role_at_least)`.
///
/// A missing/invalid token at this point (the identity lookup failing) means
/// `require_auth` did not actually run first - treated as `Forbidden` rather
/// than panicking, so a misconfigured router fails closed instead of open.
/// Spec M14: a denial is only recorded to the audit log when there IS a
/// resolved identity whose role is simply too low - the defensive
/// missing-token case above is not a meaningful RBAC decision to audit (it
/// means the router itself is misconfigured, not that a real user got
/// rejected).
async fn require_role_at_least(
    State(guard): State<RoleGuard>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let identity = bearer_token(req.headers()).and_then(|token| guard.auth.identity_for(token));
    let role = identity
        .as_ref()
        .and_then(|identity| Role::from_str(&identity.role).ok());

    match role {
        Some(role) if role.at_least(guard.min) => next.run(req).await,
        _ => {
            if let Some(identity) = &identity {
                let method = req.method().as_str().to_string();
                let path = req.uri().path().to_string();
                guard
                    .audit
                    .record(AuditEntry {
                        actor_username: Some(&identity.id),
                        actor_role: Some(&identity.role),
                        action: "denied",
                        resource: guard.resource,
                        entity_id: None,
                        detail: Some(json!({ "method": method, "path": path })),
                        origin: "rest",
                        result: "denied",
                    })
                    .await;
            }
            forbidden_response()
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UserIdentityResponse {
    id: i64,
    username: String,
    display_name: String,
    role: Role,
}

impl From<UserIdentity> for UserIdentityResponse {
    fn from(identity: UserIdentity) -> Self {
        Self {
            id: identity.id,
            username: identity.username,
            display_name: identity.display_name,
            role: identity.role,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateUserRequest {
    username: String,
    password: String,
    display_name: String,
    role: Role,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateUserRequest {
    display_name: String,
    role: Role,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResetPasswordRequest {
    new_password: String,
}

#[derive(Debug, Serialize)]
struct ResetPasswordResponse {
    success: bool,
}

/// State for the `/api/users/*` handlers: `UsersService` for the CRUD
/// itself, `AuthState` so `users_delete` can resolve the acting caller's
/// numeric row id from its bearer token (spec M10's self-deletion guard,
/// see `UsersService::delete_user`'s doc comment), and `AuditLogService`
/// (spec M14) so every mutation here records a `create`/`update`/
/// `password_reset`/`delete` entry once it has already succeeded.
#[derive(Clone)]
struct UsersAdminState {
    users: UsersService,
    auth: AuthState,
    audit: AuditLogService,
}

/// Resolve the [`UserIdentity`] of the caller making this request, from its
/// bearer token. `require_auth`/`require_role_at_least` have already proven
/// the token is valid and `admin`-roled by the time a `/api/users/*` handler
/// runs, so this should always succeed - `Unauthorized` here is a defensive
/// fallback (e.g. the account was deleted by another admin between the
/// token being issued and this request), not an expected path.
async fn acting_user(
    headers: &HeaderMap,
    auth: &AuthState,
    users: &UsersService,
) -> Result<UserIdentity, BantoError> {
    let username = bearer_token(headers)
        .and_then(|token| auth.identity_for(token))
        .map(|identity| identity.id);
    let Some(username) = username else {
        return Err(BantoError::Unauthorized);
    };
    users
        .get_by_username(&username)
        .await?
        .ok_or(BantoError::Unauthorized)
}

async fn users_list(
    State(state): State<UsersAdminState>,
) -> Result<Json<Vec<UserSummary>>, ApiError> {
    Ok(Json(state.users.list_users().await?))
}

async fn users_create(
    State(state): State<UsersAdminState>,
    headers: HeaderMap,
    Json(body): Json<CreateUserRequest>,
) -> Result<Json<UserIdentityResponse>, ApiError> {
    let identity = state
        .users
        .create_user(
            &body.username,
            &body.password,
            &body.display_name,
            body.role,
        )
        .await?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "create",
        "users",
        &identity.id.to_string(),
        Some(json!({ "username": identity.username, "role": identity.role })),
    )
    .await;
    Ok(Json(identity.into()))
}

async fn users_update(
    State(state): State<UsersAdminState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(body): Json<UpdateUserRequest>,
) -> Result<Json<UserSummary>, ApiError> {
    let updated = state
        .users
        .update_user(id, &body.display_name, body.role)
        .await?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "update",
        "users",
        &id.to_string(),
        Some(json!({ "role": updated.role })),
    )
    .await;
    Ok(Json(updated))
}

async fn users_reset_password(
    State(state): State<UsersAdminState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(body): Json<ResetPasswordRequest>,
) -> Result<Json<ResetPasswordResponse>, ApiError> {
    state.users.reset_password(id, &body.new_password).await?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "password_reset",
        "users",
        &id.to_string(),
        None,
    )
    .await;
    Ok(Json(ResetPasswordResponse { success: true }))
}

async fn users_delete(
    State(state): State<UsersAdminState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    let acting = acting_user(&headers, &state.auth, &state.users).await?;
    state.users.delete_user(id, acting.id).await?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "delete",
        "users",
        &id.to_string(),
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// `/api/users/*` (spec M10): `admin`-only account management. Guarded with
/// `require_auth` then `require_role_at_least`, with `Role::Admin` as the
/// floor.
fn users_router(users: UsersService, audit: AuditLogService, auth: AuthState) -> Router {
    let state = UsersAdminState {
        users,
        auth: auth.clone(),
        audit: audit.clone(),
    };
    Router::new()
        .route("/api/users", get(users_list).post(users_create))
        .route(
            "/api/users/{id}",
            axum::routing::put(users_update).delete(users_delete),
        )
        .route("/api/users/{id}/reset-password", post(users_reset_password))
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            RoleGuard {
                auth: auth.clone(),
                min: Role::Admin,
                resource: "users",
                audit,
            },
            require_role_at_least,
        ))
        .layer(middleware::from_fn_with_state(auth, require_auth))
}

/// State shared by `/api/auth/status`, `/api/auth/setup` and
/// `/api/auth/change-password` (see [`extra_auth_router`]): these need both
/// `UsersService` (the credential store, spec §8.2) and `AuthState` (to
/// issue a token on `setup`'s implicit login, and to resolve the calling
/// account on `change-password`), neither of which `banto_server::auth`
/// knows about on its own.
#[derive(Clone)]
struct UsersAuthState {
    users: UsersService,
    auth: AuthState,
    audit: AuditLogService,
    allow_setup: bool,
}

#[derive(Debug, Serialize)]
struct AuthStatusResponse {
    initialized: bool,
}

async fn auth_status_handler(
    State(state): State<UsersAuthState>,
) -> Result<Json<AuthStatusResponse>, ApiError> {
    let initialized = state.users.is_initialized().await?;
    Ok(Json(AuthStatusResponse { initialized }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetupRequest {
    username: String,
    password: String,
    display_name: String,
}

#[derive(Debug, Serialize)]
struct SetupResponse {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    token: Option<String>,
}

/// `POST /api/auth/setup`: creates the first account, then behaves like a
/// successful login (spec §8.2/§3.3). Three distinct outcomes:
/// - `allow_setup` is `false` -> `403` with a plain `{kind,message}` body
///   (not the `{success,error?}` shape below - this is a server
///   configuration rejection, not a "try again" outcome).
/// - `UsersService::setup_first_user` returns `BantoError::Validation` (bad
///   username/password) -> `422` with `field_errors` (spec: form fields
///   should be able to map these), same convention every other mutating
///   handler in this module uses.
/// - Anything else (already initialized, storage error) -> `200` with
///   `{success:false,error}`, mirroring `login_handler`'s "expected,
///   retryable failure" convention.
async fn auth_setup_handler(
    State(state): State<UsersAuthState>,
    Json(body): Json<SetupRequest>,
) -> Result<Response, ApiError> {
    if !state.allow_setup {
        let message = "このサーバーでは初期セットアップが許可されていません".to_string();
        return Ok((StatusCode::FORBIDDEN, Json(ErrorBody::Other { message })).into_response());
    }

    match state
        .users
        .setup_first_user(&body.username, &body.password, &body.display_name)
        .await
    {
        Ok(identity) => {
            let identity = Identity {
                id: identity.username,
                name: identity.display_name,
                role: identity.role.to_string(),
            };
            state
                .audit
                .record(AuditEntry {
                    actor_username: Some(&identity.id),
                    actor_role: Some(&identity.role),
                    action: "setup",
                    resource: "auth",
                    entity_id: None,
                    detail: None,
                    origin: "rest",
                    result: "ok",
                })
                .await;
            let token = state.auth.issue_token(identity);
            Ok(Json(SetupResponse {
                success: true,
                error: None,
                token: Some(token),
            })
            .into_response())
        }
        Err(err @ BantoError::Validation { .. }) => Err(ApiError(err)),
        Err(other) => Ok(Json(SetupResponse {
            success: false,
            error: Some(other.to_string()),
            token: None,
        })
        .into_response()),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChangePasswordRequest {
    current_password: String,
    new_password: String,
}

#[derive(Debug, Serialize)]
struct ChangePasswordResponse {
    success: bool,
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
}

/// `POST /api/auth/change-password`: authenticated via the same bearer
/// token as every other guarded route, but implemented as a plain handler
/// (not `require_auth` middleware) since it also needs the token's bound
/// `Identity` to know *which* account to update - `require_auth` only
/// proves the token is valid, it does not thread the identity through.
async fn auth_change_password_handler(
    State(state): State<UsersAuthState>,
    headers: HeaderMap,
    Json(body): Json<ChangePasswordRequest>,
) -> Result<Json<ChangePasswordResponse>, ApiError> {
    let identity = bearer_token(&headers).and_then(|token| state.auth.identity_for(token));
    let Some(identity) = identity else {
        return Err(ApiError(BantoError::Unauthorized));
    };

    state
        .users
        .change_password(&identity.id, &body.current_password, &body.new_password)
        .await?;
    // Spec M14: a self-service password change is a security event (it is
    // also what naturally invalidates an M11 autologin credential), so it IS
    // audited - `entity_id` is the caller's own numeric row id (matching the
    // other `users` entries), recovered from the username since the bearer
    // token only carries the latter. `detail` stays `None`: neither the old
    // nor the new password (nor any hash) may ever be recorded.
    let entity_id = state
        .users
        .get_by_username(&identity.id)
        .await
        .ok()
        .flatten()
        .map(|user| user.id.to_string());
    state
        .audit
        .record(AuditEntry {
            actor_username: Some(&identity.id),
            actor_role: Some(&identity.role),
            action: "password_change",
            resource: "users",
            entity_id: entity_id.as_deref(),
            detail: None,
            origin: "rest",
            result: "ok",
        })
        .await;
    Ok(Json(ChangePasswordResponse { success: true }))
}

fn extra_auth_router(
    users: UsersService,
    auth: AuthState,
    audit: AuditLogService,
    allow_setup: bool,
) -> Router {
    let state = UsersAuthState {
        users,
        auth,
        audit,
        allow_setup,
    };
    Router::new()
        .route("/api/auth/status", get(auth_status_handler))
        .route("/api/auth/setup", post(auth_setup_handler))
        .route(
            "/api/auth/change-password",
            post(auth_change_password_handler),
        )
        .with_state(state)
}

/// State for the `/api/ui-settings/*` handlers (spec M12): `SettingsService`
/// for the per-user key/value store itself, plus `AuthState` to resolve the
/// caller's own `username` from the bearer token `require_auth` already
/// validated (same pattern as [`UsersAuthState`]/[`acting_user`] above).
#[derive(Clone)]
struct UiSettingsState {
    settings: SettingsService,
    auth: AuthState,
}

#[derive(Debug, Serialize)]
struct UiSettingValueResponse {
    value: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UiSettingSetRequest {
    value: String,
}

/// Resolve the calling session's `username` (spec convention: bearer-token
/// `Identity.id` IS the username, see `banto_server::auth::Identity`'s doc
/// comment) from its bearer token. `require_auth` has already proven the
/// token valid by the time a `/api/ui-settings/*` handler runs, so this
/// should always succeed; `Unauthorized` here is a defensive fallback (e.g.
/// the token expired between `require_auth` and this handler running), not
/// an expected path - mirrors [`acting_user`] above.
fn acting_username(headers: &HeaderMap, auth: &AuthState) -> Result<String, BantoError> {
    bearer_token(headers)
        .and_then(|token| auth.identity_for(token))
        .map(|identity| identity.id)
        .ok_or(BantoError::Unauthorized)
}

async fn ui_settings_get(
    State(state): State<UiSettingsState>,
    headers: HeaderMap,
    Path(key): Path<String>,
) -> Result<Json<UiSettingValueResponse>, ApiError> {
    let username = acting_username(&headers, &state.auth)?;
    let value = state.settings.ui_get(&username, &key).await?;
    Ok(Json(UiSettingValueResponse { value }))
}

async fn ui_settings_set(
    State(state): State<UiSettingsState>,
    headers: HeaderMap,
    Path(key): Path<String>,
    Json(body): Json<UiSettingSetRequest>,
) -> Result<StatusCode, ApiError> {
    let username = acting_username(&headers, &state.auth)?;
    state.settings.ui_set(&username, &key, &body.value).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `/api/ui-settings/*` (spec M12): `require_auth` only, no
/// [`require_role_at_least`] floor - see this module's doc comment for why
/// (every route here only ever touches the caller's OWN namespaced keys).
fn ui_settings_router(settings: SettingsService, auth: AuthState) -> Router {
    let state = UiSettingsState {
        settings,
        auth: auth.clone(),
    };
    Router::new()
        .route(
            "/api/ui-settings/{key}",
            get(ui_settings_get).put(ui_settings_set),
        )
        .with_state(state)
        .layer(middleware::from_fn_with_state(auth, require_auth))
}

// --- M14: audit log ---------------------------------------------------------

/// Wraps `UsersService::verify` as the async credential verifier
/// `banto_server::AuthState::new` expects (spec §8.2), additionally
/// recording a `login`/`login_failed` audit entry for every attempt (spec
/// M14). Shared by `relay-wright-serve` (the standalone REST dev server) and
/// `src-tauri`'s embedded LAN server auth state - both are `origin: "rest"`
/// sessions (the Tauri webview's OWN session goes through the `auth_login`
/// command instead, which records its own login/login_failed entries with
/// `origin: "tauri"`).
pub fn audited_credential_verifier(
    users: UsersService,
    audit: AuditLogService,
) -> impl Fn(String, String) -> futures_util::future::BoxFuture<'static, Option<Identity>>
       + Send
       + Sync
       + 'static {
    move |username: String, password: String| {
        let users = users.clone();
        let audit = audit.clone();
        Box::pin(async move {
            match users.verify(&username, &password).await {
                Ok(Some(identity)) => {
                    audit
                        .record(AuditEntry {
                            actor_username: Some(&identity.username),
                            actor_role: Some(identity.role.as_str()),
                            action: "login",
                            resource: "auth",
                            entity_id: None,
                            detail: None,
                            origin: "rest",
                            result: "ok",
                        })
                        .await;
                    Some(Identity {
                        id: identity.username,
                        name: identity.display_name,
                        role: identity.role.to_string(),
                    })
                }
                _ => {
                    audit
                        .record(AuditEntry {
                            actor_username: Some(&username),
                            actor_role: None,
                            action: "login_failed",
                            resource: "auth",
                            entity_id: None,
                            detail: None,
                            origin: "rest",
                            result: "failed",
                        })
                        .await;
                    None
                }
            }
        })
    }
}

/// State for [`audit_logout_middleware`]: needs `AuthState` to resolve the
/// logging-out session's identity BEFORE the token is invalidated, plus
/// `AuditLogService` to record it (spec M14).
#[derive(Clone)]
struct LogoutAuditState {
    auth: AuthState,
    audit: AuditLogService,
}

/// Wraps the WHOLE `banto_server::auth_routes` sub-router (login/logout/
/// check/identity) rather than adding a competing `/api/auth/logout` route
/// of its own (spec M14): `axum::Router::merge` panics if two routers both
/// register the same path+method, and `banto_server::auth_routes` bundles
/// all four routes into one `Router` with no way to omit just `logout` - so
/// this instead inspects each request's path/method, resolving the caller's
/// identity (before the real handler invalidates the token) only when the
/// request IS the logout route, letting `next` run the real handler
/// completely unmodified either way, then recording the `logout` entry
/// after.
///
/// `POST /api/auth/login`'s own login/login_failed events are NOT recorded
/// here - see [`audited_credential_verifier`], which records those from
/// inside the credential-verifier closure instead (simpler: no need to peek
/// at the response body to learn success/failure).
async fn audit_logout_middleware(
    State(state): State<LogoutAuditState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let is_logout =
        req.method() == axum::http::Method::POST && req.uri().path() == "/api/auth/logout";
    let identity = if is_logout {
        actor_identity(req.headers(), &state.auth)
    } else {
        None
    };

    let response = next.run(req).await;

    if is_logout {
        state
            .audit
            .record(AuditEntry {
                actor_username: identity.as_ref().map(|i| i.id.as_str()),
                actor_role: identity.as_ref().map(|i| i.role.as_str()),
                action: "logout",
                resource: "auth",
                entity_id: None,
                detail: None,
                origin: "rest",
                result: "ok",
            })
            .await;
    }

    response
}

/// State for the `/api/audit-log/*` handlers (spec M14): `AuditLogService`
/// for the read/write itself, `SettingsService` for the retention-policy
/// config endpoints (and the list route's opportunistic prune), plus
/// `AuthState` so `audit_config_apply` can resolve the calling actor (via
/// [`actor_identity`]) for its own `settings_change` audit entry, same as
/// the users write handlers' `record_write` helper.
#[derive(Clone)]
struct AuditLogState {
    audit: AuditLogService,
    settings: SettingsService,
    auth: AuthState,
}

/// `POST /api/audit-log/list` (spec M14, `admin`-only): filtered/sorted/
/// paginated read of the audit trail (spec: read routes themselves are
/// never audited, only mutations/denials/auth events are). Also
/// opportunistically prunes (spec: "list実行時に軽く") before answering -
/// best-effort, a prune failure must never block an admin from viewing
/// existing entries, so its result is discarded. There is deliberately no
/// separate background pruning task: this plus a once-at-startup prune
/// (`bin/relay-wright-serve.rs`'s `main`/`src-tauri`'s `run()`) is judged
/// sufficient - the audit-log viewer is an admin-only, infrequently-visited
/// page, and each prune is a couple of indexed `DELETE`s, not an expensive
/// scan.
async fn audit_log_list(
    State(state): State<AuditLogState>,
    Json(params): Json<ListParams>,
) -> Result<Json<ListResult<crate::audit::AuditLogEntry>>, ApiError> {
    if let Ok(config) = state.settings.audit_config().await {
        let _ = state
            .audit
            .prune(config.retention_days, config.retention_rows)
            .await;
    }
    Ok(Json(state.audit.list(params).await?))
}

/// `GET /api/audit-log/config` (spec M14, `admin`-only): current retention
/// policy - read-only, so unlike `audit_config_apply` this records nothing
/// (spec: read routes are never audited).
async fn audit_config_get(
    State(state): State<AuditLogState>,
) -> Result<Json<AuditSettings>, ApiError> {
    Ok(Json(state.settings.audit_config().await?))
}

/// `PUT /api/audit-log/config` (spec M14, `admin`-only): persist a new
/// retention policy (days and/or row-count cap; either may be `null` for
/// "unlimited" on that dimension, see [`crate::settings::AuditSettings`]),
/// mirroring `src-tauri`'s `audit_config_apply` command - same
/// `settings_change`/`settings` audit entry shape, just `origin: "rest"` and
/// the actor resolved from the bearer token (`actor_identity`) instead of
/// from Tauri's session mutex.
async fn audit_config_apply(
    State(state): State<AuditLogState>,
    headers: HeaderMap,
    Json(config): Json<AuditSettings>,
) -> Result<Json<AuditSettings>, ApiError> {
    state.settings.set_audit_config(&config).await?;
    let identity = actor_identity(&headers, &state.auth);
    state
        .audit
        .record(AuditEntry {
            actor_username: identity.as_ref().map(|i| i.id.as_str()),
            actor_role: identity.as_ref().map(|i| i.role.as_str()),
            action: "settings_change",
            resource: "settings",
            entity_id: None,
            detail: Some(serde_json::json!({
                "retentionDays": config.retention_days,
                "retentionRows": config.retention_rows,
            })),
            origin: "rest",
            result: "ok",
        })
        .await;
    Ok(Json(state.settings.audit_config().await?))
}

/// `/api/audit-log/*` (spec M14): `admin`-only, guarded the same way
/// `users_router` is (`require_auth` then `require_role_at_least`).
fn audit_log_router(audit: AuditLogService, settings: SettingsService, auth: AuthState) -> Router {
    let state = AuditLogState {
        audit: audit.clone(),
        settings,
        auth: auth.clone(),
    };
    Router::new()
        .route("/api/audit-log/list", post(audit_log_list))
        .route(
            "/api/audit-log/config",
            get(audit_config_get).put(audit_config_apply),
        )
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            RoleGuard {
                auth: auth.clone(),
                min: Role::Admin,
                resource: "audit_log",
                audit,
            },
            require_role_at_least,
        ))
        .layer(middleware::from_fn_with_state(auth, require_auth))
}

// --- M17: SQLite backup/restore ---------------------------------------------

/// State for the `/api/backups/*` handlers (spec M17): `BackupService` for
/// the operation itself, plus `AuditLogService`/`AuthState` so
/// `backups_create_handler`/`backups_restore_from_upload`/
/// `backups_restore_from_existing`/`backups_cancel_pending` can each record
/// their own audit entry once the underlying service call has already
/// succeeded (same pattern as `UsersAdminState`). Read
/// handlers (`backups_list`/`backups_download`/`backups_pending_status`)
/// also take this state (rather than a narrower read-only one) purely to
/// avoid a second near-identical struct - they simply never touch `audit`.
#[derive(Clone)]
struct BackupsState {
    backup: BackupService,
    audit: AuditLogService,
    auth: AuthState,
}

async fn backups_create_handler(
    State(state): State<BackupsState>,
    headers: HeaderMap,
) -> Result<Json<BackupInfo>, ApiError> {
    let info = state.backup.create().await?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "backup",
        "backups",
        &info.file_name,
        Some(json!({ "sizeBytes": info.size_bytes })),
    )
    .await;
    Ok(Json(info))
}

async fn backups_list_handler(
    State(state): State<BackupsState>,
) -> Result<Json<Vec<BackupInfo>>, ApiError> {
    Ok(Json(state.backup.list().await?))
}

/// `GET /api/backups/{fileName}` (spec M17): LAN download. Not audited -
/// same "read routes are never audited" convention as everywhere else (see
/// this module's doc comment).
async fn backups_download_handler(
    State(state): State<BackupsState>,
    Path(file_name): Path<String>,
) -> Result<Response, ApiError> {
    let bytes = state.backup.read(&file_name).await?;
    let response = Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, "application/octet-stream")
        .header(
            axum::http::header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{file_name}\""),
        )
        .body(axum::body::Body::from(bytes))
        .map_err(|err| ApiError(BantoError::Other(err.to_string())))?;
    Ok(response)
}

#[derive(Debug, Deserialize)]
struct RestoreUploadQuery {
    #[serde(rename = "fileName")]
    file_name: Option<String>,
}

/// `POST /api/backups/restore?fileName=` (spec M17): stage a restore from a
/// raw uploaded file. `fileName` (if present) is ONLY ever used for the
/// audit `detail` - the uploaded bytes are always staged under
/// `BackupService`'s own fixed `restore-pending.sqlite3` name, never under
/// the client-supplied name (see this module's doc comment).
async fn backups_restore_from_upload(
    State(state): State<BackupsState>,
    headers: HeaderMap,
    Query(query): Query<RestoreUploadQuery>,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    state.backup.stage_restore_from_bytes(&body).await?;
    let identity = actor_identity(&headers, &state.auth);
    state
        .audit
        .record(AuditEntry {
            actor_username: identity.as_ref().map(|i| i.id.as_str()),
            actor_role: identity.as_ref().map(|i| i.role.as_str()),
            action: "restore_staged",
            resource: "backups",
            entity_id: None,
            detail: Some(json!({ "source": "upload", "fileName": query.file_name })),
            origin: "rest",
            result: "ok",
        })
        .await;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/backups/{fileName}/restore` (spec M17): stage a restore from
/// an existing backup already in `backups/`.
async fn backups_restore_from_existing(
    State(state): State<BackupsState>,
    headers: HeaderMap,
    Path(file_name): Path<String>,
) -> Result<StatusCode, ApiError> {
    state.backup.stage_restore_from_file(&file_name).await?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "restore_staged",
        "backups",
        &file_name,
        Some(json!({ "source": "existing", "fileName": file_name })),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn backups_pending_status(
    State(state): State<BackupsState>,
) -> Json<Option<PendingRestoreInfo>> {
    Json(state.backup.pending_restore().await)
}

async fn backups_cancel_pending(
    State(state): State<BackupsState>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    state.backup.cancel_pending_restore().await?;
    let identity = actor_identity(&headers, &state.auth);
    state
        .audit
        .record(AuditEntry {
            actor_username: identity.as_ref().map(|i| i.id.as_str()),
            actor_role: identity.as_ref().map(|i| i.role.as_str()),
            action: "restore_cancelled",
            resource: "backups",
            entity_id: None,
            detail: None,
            origin: "rest",
            result: "ok",
        })
        .await;
    Ok(StatusCode::NO_CONTENT)
}

/// `/api/backups/*` (spec M17): `admin`-only, guarded the same way
/// `users_router`/`audit_log_router` are. `DefaultBodyLimit::max` raises the
/// upload route's body cap from axum's 2MB default to
/// [`MAX_RESTORE_UPLOAD_BYTES`] - applied to the whole router (the other
/// routes here have no meaningful request body, so this is harmless for
/// them).
fn backups_router(backup: BackupService, audit: AuditLogService, auth: AuthState) -> Router {
    let state = BackupsState {
        backup,
        audit: audit.clone(),
        auth: auth.clone(),
    };
    Router::new()
        .route(
            "/api/backups",
            post(backups_create_handler).get(backups_list_handler),
        )
        .route("/api/backups/restore", post(backups_restore_from_upload))
        .route(
            "/api/backups/pending-restore",
            get(backups_pending_status).delete(backups_cancel_pending),
        )
        .route("/api/backups/{fileName}", get(backups_download_handler))
        .route(
            "/api/backups/{fileName}/restore",
            post(backups_restore_from_existing),
        )
        .with_state(state)
        .layer(axum::extract::DefaultBodyLimit::max(
            MAX_RESTORE_UPLOAD_BYTES,
        ))
        .layer(middleware::from_fn_with_state(
            RoleGuard {
                auth: auth.clone(),
                min: Role::Admin,
                resource: "backups",
                audit,
            },
            require_role_at_least,
        ))
        .layer(middleware::from_fn_with_state(auth, require_auth))
}

// --- W2: write registry/rule CRUD -------------------------------------------

/// Resolve the caller's identity and require role >= `editor` (spec M10:
/// resources are viewer-read / editor-write). Records a `denied` audit entry
/// (mirroring [`require_role_at_least`]) when an AUTHENTICATED caller's role
/// is too low, and returns `BantoError::Forbidden`; no valid session at all
/// returns `BantoError::Unauthorized` (deliberately NOT audited, same
/// reasoning as the RBAC middleware: nothing resembling a real user to
/// attribute a denial to). Used inline by the write handlers of
/// [`write_registry_router`], which - unlike the admin-only routers - cannot
/// use a single-floor `RoleGuard` middleware because their GET routes are
/// only `require_auth` (viewer+), so read and write share a path but need
/// different floors. This is the REST twin of `src-tauri`'s `require_role`.
async fn require_editor(
    auth: &AuthState,
    audit: &AuditLogService,
    headers: &HeaderMap,
    resource: &'static str,
    method: &str,
    path: &str,
) -> Result<(), BantoError> {
    match actor_identity(headers, auth) {
        Some(identity)
            if Role::from_str(&identity.role)
                .map(|role| role.at_least(Role::Editor))
                .unwrap_or(false) =>
        {
            Ok(())
        }
        Some(identity) => {
            audit
                .record(AuditEntry {
                    actor_username: Some(&identity.id),
                    actor_role: Some(&identity.role),
                    action: "denied",
                    resource,
                    entity_id: None,
                    detail: Some(json!({ "method": method, "path": path })),
                    origin: "rest",
                    result: "denied",
                })
                .await;
            Err(BantoError::Forbidden)
        }
        None => Err(BantoError::Unauthorized),
    }
}

/// State for the `/api/write-targets/*` and `/api/write-rules/*` handlers
/// (spec §1 両経路対称): both services plus `AuthState`/`AuditLogService` so
/// each mutation can editor-gate ([`require_editor`]) and audit exactly as
/// the Tauri commands do.
#[derive(Clone)]
struct WriteRegistryState {
    targets: WriteTargetService,
    rules: WriteRuleService,
    auth: AuthState,
    audit: AuditLogService,
}

async fn write_targets_list(
    State(state): State<WriteRegistryState>,
) -> Result<Json<Vec<WriteTarget>>, ApiError> {
    Ok(Json(state.targets.list(ListParams::default()).await?.rows))
}

async fn write_targets_get(
    State(state): State<WriteRegistryState>,
    Path(id): Path<i64>,
) -> Result<Json<WriteTarget>, ApiError> {
    Ok(Json(state.targets.get(id).await?))
}

async fn write_targets_create(
    State(state): State<WriteRegistryState>,
    headers: HeaderMap,
    Json(input): Json<WriteTargetInput>,
) -> Result<Json<WriteTarget>, ApiError> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "write_targets",
        "POST",
        "/api/write-targets",
    )
    .await?;
    let created = state.targets.create(input).await?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "create",
        "write_targets",
        &created.id.to_string(),
        Some(json!({ "name": created.name })),
    )
    .await;
    Ok(Json(created))
}

async fn write_targets_update(
    State(state): State<WriteRegistryState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(input): Json<WriteTargetInput>,
) -> Result<Json<WriteTarget>, ApiError> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "write_targets",
        "PUT",
        "/api/write-targets/{id}",
    )
    .await?;
    let updated = state.targets.update(id, input).await?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "update",
        "write_targets",
        &id.to_string(),
        Some(json!({ "name": updated.name })),
    )
    .await;
    Ok(Json(updated))
}

async fn write_targets_delete(
    State(state): State<WriteRegistryState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "write_targets",
        "DELETE",
        "/api/write-targets/{id}",
    )
    .await?;
    state.targets.delete(id).await?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "delete",
        "write_targets",
        &id.to_string(),
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn write_rules_list(
    State(state): State<WriteRegistryState>,
) -> Result<Json<Vec<WriteRuleDetail>>, ApiError> {
    Ok(Json(state.rules.list(ListParams::default()).await?.rows))
}

async fn write_rules_get(
    State(state): State<WriteRegistryState>,
    Path(id): Path<i64>,
) -> Result<Json<WriteRuleDetail>, ApiError> {
    Ok(Json(state.rules.get(id).await?))
}

async fn write_rules_create(
    State(state): State<WriteRegistryState>,
    headers: HeaderMap,
    Json(input): Json<WriteRuleInput>,
) -> Result<Json<WriteRuleDetail>, ApiError> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "write_rules",
        "POST",
        "/api/write-rules",
    )
    .await?;
    let created = state.rules.create(input).await?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "create",
        "write_rules",
        &created.rule.id.to_string(),
        Some(json!({ "name": created.rule.name, "enabled": created.rule.enabled })),
    )
    .await;
    Ok(Json(created))
}

async fn write_rules_update(
    State(state): State<WriteRegistryState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(input): Json<WriteRuleInput>,
) -> Result<Json<WriteRuleDetail>, ApiError> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "write_rules",
        "PUT",
        "/api/write-rules/{id}",
    )
    .await?;
    let updated = state.rules.update(id, input).await?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "update",
        "write_rules",
        &id.to_string(),
        Some(json!({ "name": updated.rule.name, "enabled": updated.rule.enabled })),
    )
    .await;
    Ok(Json(updated))
}

async fn write_rules_delete(
    State(state): State<WriteRegistryState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "write_rules",
        "DELETE",
        "/api/write-rules/{id}",
    )
    .await?;
    state.rules.delete(id).await?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "delete",
        "write_rules",
        &id.to_string(),
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// `/api/write-targets/*` + `/api/write-rules/*` (spec §1 両経路対称, plan
/// W2): viewer-read / editor-write. Guarded by `require_auth` for the whole
/// router (any authenticated role may read); each write handler additionally
/// calls [`require_editor`]. This split - rather than the admin routers'
/// single-floor `RoleGuard` middleware - is what lets GET and POST/PUT/DELETE
/// share a path with different role floors.
fn write_registry_router(
    targets: WriteTargetService,
    rules: WriteRuleService,
    audit: AuditLogService,
    auth: AuthState,
) -> Router {
    let state = WriteRegistryState {
        targets,
        rules,
        auth: auth.clone(),
        audit,
    };
    Router::new()
        .route(
            "/api/write-targets",
            get(write_targets_list).post(write_targets_create),
        )
        .route(
            "/api/write-targets/{id}",
            get(write_targets_get)
                .put(write_targets_update)
                .delete(write_targets_delete),
        )
        .route(
            "/api/write-rules",
            get(write_rules_list).post(write_rules_create),
        )
        .route(
            "/api/write-rules/{id}",
            get(write_rules_get)
                .put(write_rules_update)
                .delete(write_rules_delete),
        )
        .with_state(state)
        .layer(middleware::from_fn_with_state(auth, require_auth))
}

/// Compose the full `/api/*` router (spec §11.1): auth routes (login/
/// logout/check/identity from `banto_server` - wrapped with an audit-log
/// hook for `logout`, spec M14 - plus status/setup/change-password here
/// since those need `UsersService`), SSE events, the `admin`-only `users`
/// management routes (spec M10), the `admin`-only `audit-log` viewer (spec
/// M14), the `admin`-only `backups` routes (spec M17), the viewer-read/
/// editor-write `write-targets`/`write-rules` registry (plan W2), and the
/// per-user `ui-settings` routes (spec M12), all behind the CSRF header
/// check. Mount
/// the result *before* `banto_server::static_files::static_router` so
/// `/api/*` takes priority over the SPA fallback. Future resources (R1-B:
/// PLC connections/collection groups/tags/display groups) get their own
/// RBAC-split read/write routers merged in here the same way the banto
/// template's `items_router` used to.
// Each parameter is a distinct, already-cloneable service handle threaded
// through from `main()`/tests (no natural subset to bundle into a struct
// without adding an indirection layer with a single call site); simpler to
// allow this than to invent a "Services" struct for one function.
#[allow(clippy::too_many_arguments)]
pub fn api_router(
    users: UsersService,
    settings: SettingsService,
    audit: AuditLogService,
    backup: BackupService,
    write_targets: WriteTargetService,
    write_rules: WriteRuleService,
    auth: AuthState,
    events: broadcast::Sender<ServerEvent>,
    allow_setup: bool,
) -> Router {
    let audited_auth_routes = auth_routes(auth.clone()).layer(middleware::from_fn_with_state(
        LogoutAuditState {
            auth: auth.clone(),
            audit: audit.clone(),
        },
        audit_logout_middleware,
    ));

    Router::new()
        .merge(audited_auth_routes)
        .merge(extra_auth_router(
            users.clone(),
            auth.clone(),
            audit.clone(),
            allow_setup,
        ))
        .merge(sse_route(auth.clone(), events))
        .merge(users_router(users, audit.clone(), auth.clone()))
        .merge(audit_log_router(
            audit.clone(),
            settings.clone(),
            auth.clone(),
        ))
        .merge(write_registry_router(
            write_targets,
            write_rules,
            audit.clone(),
            auth.clone(),
        ))
        .merge(backups_router(backup, audit, auth.clone()))
        .merge(ui_settings_router(settings, auth))
        .layer(middleware::from_fn(require_banto_client_header))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrate_memory;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use banto_core::BantoError;
    use serde_json::json;
    use std::path::PathBuf;
    use tempfile::tempdir;
    use tower::ServiceExt;

    const CLIENT_HEADER: (&str, &str) = ("X-Banto-Client", "banto");

    /// A `BackupService` for router helpers that do not exercise
    /// `/api/backups/*` at all (the overwhelming majority of this module's
    /// tests) - `BackupService::new` only stores its arguments, so an
    /// on-disk path that is never actually written to is harmless. Tests
    /// that DO exercise backups use [`router_with_role_tokens_and_backup`]
    /// instead, which points at a real, writable temp directory AND (unlike
    /// every other helper here) a real on-disk pool - see that function's
    /// doc comment for why the pool matters too.
    fn unused_backup_service(pool: sqlx::SqlitePool) -> BackupService {
        BackupService::new(
            PathBuf::from("unused-in-tests").join("relay-wright.sqlite3"),
            pool,
        )
    }

    fn demo_auth() -> AuthState {
        AuthState::new(|u: String, p: String| {
            Box::pin(async move {
                if u == "admin" && p == "admin" {
                    Some(Identity {
                        id: "admin".to_string(),
                        name: "管理者".to_string(),
                        role: "admin".to_string(),
                    })
                } else {
                    None
                }
            })
        })
    }

    /// Router + one bearer token per role (admin/editor/viewer), for the
    /// RBAC tests below (spec M10). Unlike [`demo_auth_with_roles`] (whose
    /// login verifier is independent of any `UsersService`), the three
    /// accounts here are REAL rows in the same `UsersService`/pool the
    /// router's `/api/users/*` routes operate on - required so
    /// `users_delete`'s `acting_user` lookup (by the token's username) can
    /// actually resolve the admin account performing the delete in
    /// `admin_can_create_list_update_reset_password_and_delete_users`
    /// below.
    async fn router_with_role_tokens() -> (Router, String, String, String) {
        let pool = migrate_memory().await.expect("migrate_memory");
        let (tx, _rx) = broadcast::channel(16);
        let users = UsersService::new(pool.clone());
        let settings = SettingsService::new(pool.clone());
        let backup = unused_backup_service(pool.clone());
        let audit = AuditLogService::new(pool.clone());
        let write_targets = WriteTargetService::new(pool.clone());
        let write_rules = WriteRuleService::new(pool);

        users
            .setup_first_user("admin", "password123", "管理者")
            .await
            .expect("setup_first_user");
        users
            .create_user("editor", "password123", "編集者", Role::Editor)
            .await
            .expect("create editor");
        users
            .create_user("viewer", "password123", "閲覧者", Role::Viewer)
            .await
            .expect("create viewer");

        let verify_users = users.clone();
        let auth = AuthState::new(move |u: String, p: String| {
            let users = verify_users.clone();
            Box::pin(async move {
                match users.verify(&u, &p).await {
                    Ok(Some(identity)) => Some(Identity {
                        id: identity.username,
                        name: identity.display_name,
                        role: identity.role.to_string(),
                    }),
                    _ => None,
                }
            })
        });

        let admin_token = auth
            .login("admin", "password123")
            .await
            .expect("admin login");
        let editor_token = auth
            .login("editor", "password123")
            .await
            .expect("editor login");
        let viewer_token = auth
            .login("viewer", "password123")
            .await
            .expect("viewer login");
        (
            api_router(
                users,
                settings,
                audit,
                backup,
                write_targets,
                write_rules,
                auth,
                tx,
                false,
            ),
            admin_token,
            editor_token,
            viewer_token,
        )
    }

    async fn router_with_token() -> (Router, String) {
        let pool = migrate_memory().await.expect("migrate_memory");
        let (tx, _rx) = broadcast::channel(16);
        let users = UsersService::new(pool.clone());
        let settings = SettingsService::new(pool.clone());
        let backup = unused_backup_service(pool.clone());
        let audit = AuditLogService::new(pool.clone());
        let write_targets = WriteTargetService::new(pool.clone());
        let write_rules = WriteRuleService::new(pool);
        let auth = demo_auth();
        let token = auth
            .login("admin", "admin")
            .await
            .expect("login should succeed");
        (
            api_router(
                users,
                settings,
                audit,
                backup,
                write_targets,
                write_rules,
                auth,
                tx,
                false,
            ),
            token,
        )
    }

    async fn body_json(response: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn missing_csrf_header_is_forbidden_even_with_a_token() {
        let (router, token) = router_with_token().await;
        let response = router
            .oneshot(
                HttpRequest::get("/api/auth/check")
                    .header("Authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    /// Sanity check that `BantoError` variants used elsewhere still map the
    /// way this module's tests assume (guards against silent drift if
    /// `banto_core::error` changes).
    #[test]
    fn error_kind_used_in_tests_matches_banto_core() {
        let err = BantoError::NotFound {
            resource: "items".to_string(),
            id: "1".to_string(),
        };
        assert_eq!(
            serde_json::to_value(&err).unwrap()["kind"],
            json!("not_found")
        );
    }

    async fn router_with_setup(allow_setup: bool) -> Router {
        let pool = migrate_memory().await.expect("migrate_memory");
        let (tx, _rx) = broadcast::channel(16);
        let users = UsersService::new(pool.clone());
        let settings = SettingsService::new(pool.clone());
        let backup = unused_backup_service(pool.clone());
        let audit = AuditLogService::new(pool.clone());
        let write_targets = WriteTargetService::new(pool.clone());
        let write_rules = WriteRuleService::new(pool);
        let auth = demo_auth();
        api_router(
            users,
            settings,
            audit,
            backup,
            write_targets,
            write_rules,
            auth,
            tx,
            allow_setup,
        )
    }

    fn get(path: &str) -> HttpRequest<Body> {
        HttpRequest::get(path)
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .body(Body::empty())
            .unwrap()
    }

    fn post_json(path: &str, body: serde_json::Value) -> HttpRequest<Body> {
        HttpRequest::post(path)
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    #[tokio::test]
    async fn auth_status_reports_uninitialized_before_any_setup() {
        let router = router_with_setup(true).await;
        let response = router.oneshot(get("/api/auth/status")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response).await;
        assert_eq!(json["initialized"], false);
    }

    #[tokio::test]
    async fn auth_status_needs_no_bearer_token() {
        // Same assertion as above, phrased to make explicit that omitting
        // Authorization entirely (not just an invalid token) still gets a
        // 200, not a 401 - the login page calls this before any session
        // exists.
        let router = router_with_setup(true).await;
        let request = HttpRequest::get("/api/auth/status")
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .body(Body::empty())
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_setup_is_forbidden_when_allow_setup_is_false() {
        let router = router_with_setup(false).await;
        let response = router
            .oneshot(post_json(
                "/api/auth/setup",
                json!({ "username": "owner", "password": "password123", "displayName": "オーナー" }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn auth_setup_creates_account_and_the_token_works_for_guarded_routes() {
        let router = router_with_setup(true).await;

        let setup_response = router
            .clone()
            .oneshot(post_json(
                "/api/auth/setup",
                json!({ "username": "owner", "password": "password123", "displayName": "オーナー" }),
            ))
            .await
            .unwrap();
        assert_eq!(setup_response.status(), StatusCode::OK);
        let setup_json = body_json(setup_response).await;
        assert_eq!(setup_json["success"], true);
        let token = setup_json["token"].as_str().expect("token").to_string();

        // `initialized` should now be true.
        let status_response = router
            .clone()
            .oneshot(get("/api/auth/status"))
            .await
            .unwrap();
        assert_eq!(body_json(status_response).await["initialized"], true);

        // And the freshly-issued token should work on a guarded route.
        let identity_request = HttpRequest::get("/api/auth/identity")
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .header("Authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let identity_response = router.oneshot(identity_request).await.unwrap();
        assert_eq!(identity_response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_setup_rejects_short_password_with_422_validation() {
        let router = router_with_setup(true).await;
        let response = router
            .oneshot(post_json(
                "/api/auth/setup",
                json!({ "username": "owner", "password": "short", "displayName": "オーナー" }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let json = body_json(response).await;
        assert_eq!(json["kind"], "validation");
        assert_eq!(json["field_errors"][0]["field"], "password");
    }

    #[tokio::test]
    async fn auth_setup_second_call_returns_success_false_already_initialized() {
        let router = router_with_setup(true).await;
        let first = post_json(
            "/api/auth/setup",
            json!({ "username": "owner", "password": "password123", "displayName": "オーナー" }),
        );
        router.clone().oneshot(first).await.unwrap();

        let second = post_json(
            "/api/auth/setup",
            json!({ "username": "someone-else", "password": "password123", "displayName": "誰か" }),
        );
        let response = router.oneshot(second).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response).await;
        assert_eq!(json["success"], false);
        assert!(json["error"].as_str().unwrap().contains("初期化"));
    }

    async fn setup_and_get_token(router: &Router) -> String {
        let response = router
            .clone()
            .oneshot(post_json(
                "/api/auth/setup",
                json!({ "username": "owner", "password": "password123", "displayName": "オーナー" }),
            ))
            .await
            .unwrap();
        body_json(response).await["token"]
            .as_str()
            .expect("token")
            .to_string()
    }

    #[tokio::test]
    async fn auth_change_password_requires_a_bearer_token() {
        let router = router_with_setup(true).await;
        setup_and_get_token(&router).await;

        let response = router
            .oneshot(post_json(
                "/api/auth/change-password",
                json!({ "currentPassword": "password123", "newPassword": "newpassword1" }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn auth_change_password_rejects_wrong_current_password() {
        let router = router_with_setup(true).await;
        let token = setup_and_get_token(&router).await;

        let request = HttpRequest::post("/api/auth/change-password")
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .header("Authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(
                json!({ "currentPassword": "not-the-password", "newPassword": "newpassword1" })
                    .to_string(),
            ))
            .unwrap();
        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let json = body_json(response).await;
        assert_eq!(json["field_errors"][0]["field"], "currentPassword");
    }

    /// Builds a router whose `/api/auth/login` verifier is backed by the
    /// SAME `UsersService`/pool as `/api/auth/setup` and
    /// `/api/auth/change-password` - mirrors how `relay-wright-serve`/`src-tauri`
    /// wire things in production (unlike `router_with_setup` above, whose
    /// `demo_auth()` login verifier is intentionally independent). Also
    /// returns the `AuditLogService` sharing the router's pool, so M14
    /// tests can assert on what got recorded.
    async fn router_with_real_login(allow_setup: bool) -> (Router, AuditLogService) {
        let pool = migrate_memory().await.expect("migrate_memory");
        let (tx, _rx) = broadcast::channel(16);
        let users = UsersService::new(pool.clone());
        let settings = SettingsService::new(pool.clone());
        let backup = unused_backup_service(pool.clone());
        let audit = AuditLogService::new(pool.clone());
        let write_targets = WriteTargetService::new(pool.clone());
        let write_rules = WriteRuleService::new(pool);
        let auth = AuthState::new(audited_credential_verifier(users.clone(), audit.clone()));
        (
            api_router(
                users,
                settings,
                audit.clone(),
                backup,
                write_targets,
                write_rules,
                auth,
                tx,
                allow_setup,
            ),
            audit,
        )
    }

    #[tokio::test]
    async fn auth_change_password_success_then_relogin_with_new_password() {
        let (router, _audit) = router_with_real_login(true).await;
        let token = setup_and_get_token(&router).await;

        let change_request = HttpRequest::post("/api/auth/change-password")
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .header("Authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(
                json!({ "currentPassword": "password123", "newPassword": "newpassword1" })
                    .to_string(),
            ))
            .unwrap();
        let change_response = router.clone().oneshot(change_request).await.unwrap();
        assert_eq!(change_response.status(), StatusCode::OK);
        assert_eq!(body_json(change_response).await["success"], true);

        // The old password must no longer work.
        let old_login = router
            .clone()
            .oneshot(post_json(
                "/api/auth/login",
                json!({ "username": "owner", "password": "password123" }),
            ))
            .await
            .unwrap();
        assert_eq!(body_json(old_login).await["success"], false);

        // The new password must work.
        let new_login = router
            .oneshot(post_json(
                "/api/auth/login",
                json!({ "username": "owner", "password": "newpassword1" }),
            ))
            .await
            .unwrap();
        let json = body_json(new_login).await;
        assert_eq!(json["success"], true);
        assert!(json["token"].as_str().is_some());
    }

    // --- M10 RBAC ----------------------------------------------------------

    fn put_json(path: &str, token: &str, body: serde_json::Value) -> HttpRequest<Body> {
        HttpRequest::put(path)
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .header("Authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    fn post_json_auth(path: &str, token: &str, body: serde_json::Value) -> HttpRequest<Body> {
        HttpRequest::post(path)
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .header("Authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    fn get_auth(path: &str, token: &str) -> HttpRequest<Body> {
        HttpRequest::get(path)
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .header("Authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap()
    }

    fn delete_auth(path: &str, token: &str) -> HttpRequest<Body> {
        HttpRequest::delete(path)
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .header("Authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap()
    }

    #[tokio::test]
    async fn only_admin_can_list_users() {
        let (router, admin, editor, viewer) = router_with_role_tokens().await;

        for (token, expected) in [
            (&admin, StatusCode::OK),
            (&editor, StatusCode::FORBIDDEN),
            (&viewer, StatusCode::FORBIDDEN),
        ] {
            let response = router
                .clone()
                .oneshot(get_auth("/api/users", token))
                .await
                .unwrap();
            assert_eq!(response.status(), expected, "token role mismatch");
        }
    }

    #[tokio::test]
    async fn non_admin_users_write_routes_are_forbidden_with_forbidden_kind() {
        let (router, _admin, editor, _viewer) = router_with_role_tokens().await;

        let response = router
            .oneshot(post_json_auth(
                "/api/users",
                &editor,
                json!({
                    "username": "newperson",
                    "password": "password123",
                    "displayName": "New Person",
                    "role": "viewer"
                }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let json = body_json(response).await;
        assert_eq!(json["kind"], "forbidden");
    }

    #[tokio::test]
    async fn admin_can_create_list_update_reset_password_and_delete_users() {
        let (router, admin, _editor, _viewer) = router_with_role_tokens().await;

        let create_response = router
            .clone()
            .oneshot(post_json_auth(
                "/api/users",
                &admin,
                json!({
                    "username": "newperson",
                    "password": "password123",
                    "displayName": "New Person",
                    "role": "editor"
                }),
            ))
            .await
            .unwrap();
        assert_eq!(create_response.status(), StatusCode::OK);
        let created = body_json(create_response).await;
        assert_eq!(created["role"], "editor");
        let id = created["id"].as_i64().unwrap();

        let list_response = router
            .clone()
            .oneshot(get_auth("/api/users", &admin))
            .await
            .unwrap();
        let list = body_json(list_response).await;
        assert!(list.as_array().unwrap().iter().any(|u| u["id"] == id));

        let update_response = router
            .clone()
            .oneshot(put_json(
                &format!("/api/users/{id}"),
                &admin,
                json!({ "displayName": "Updated Person", "role": "viewer" }),
            ))
            .await
            .unwrap();
        assert_eq!(update_response.status(), StatusCode::OK);
        assert_eq!(body_json(update_response).await["role"], "viewer");

        let reset_response = router
            .clone()
            .oneshot(post_json_auth(
                &format!("/api/users/{id}/reset-password"),
                &admin,
                json!({ "newPassword": "resetpassword1" }),
            ))
            .await
            .unwrap();
        assert_eq!(reset_response.status(), StatusCode::OK);
        assert_eq!(body_json(reset_response).await["success"], true);

        let delete_response = router
            .oneshot(delete_auth(&format!("/api/users/{id}"), &admin))
            .await
            .unwrap();
        assert_eq!(delete_response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn users_routes_are_unauthorized_without_a_token() {
        let (router, _admin, _editor, _viewer) = router_with_role_tokens().await;
        let response = router.oneshot(get("/api/users")).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    // --- M12 per-user UI settings ------------------------------------------

    fn put_ui_setting(key: &str, token: &str, value: &str) -> HttpRequest<Body> {
        put_json(
            &format!("/api/ui-settings/{key}"),
            token,
            json!({ "value": value }),
        )
    }

    #[tokio::test]
    async fn ui_settings_round_trip_via_rest() {
        let (router, _admin, _editor, viewer) = router_with_role_tokens().await;

        // Unset key reads back as {"value": null}.
        let response = router
            .clone()
            .oneshot(get_auth("/api/ui-settings/theme", &viewer))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(body_json(response).await["value"].is_null());

        // PUT then GET round-trips - and note this is the VIEWER role:
        // writing your own UI settings needs no role floor (unlike
        // `settings_set`/`/api/users`).
        let put_response = router
            .clone()
            .oneshot(put_ui_setting("theme", &viewer, "glass"))
            .await
            .unwrap();
        assert_eq!(put_response.status(), StatusCode::NO_CONTENT);

        let response = router
            .oneshot(get_auth("/api/ui-settings/theme", &viewer))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_json(response).await["value"], "glass");
    }

    #[tokio::test]
    async fn ui_settings_are_isolated_per_user() {
        let (router, admin, editor, _viewer) = router_with_role_tokens().await;

        let put_response = router
            .clone()
            .oneshot(put_ui_setting("theme", &admin, "glass"))
            .await
            .unwrap();
        assert_eq!(put_response.status(), StatusCode::NO_CONTENT);

        // The admin's value must NOT be visible to the editor's session -
        // each account reads its own `ui.{username}.*` namespace.
        let response = router
            .clone()
            .oneshot(get_auth("/api/ui-settings/theme", &editor))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(body_json(response).await["value"].is_null());

        // And the admin still sees their own value.
        let response = router
            .oneshot(get_auth("/api/ui-settings/theme", &admin))
            .await
            .unwrap();
        assert_eq!(body_json(response).await["value"], "glass");
    }

    #[tokio::test]
    async fn ui_settings_reject_an_invalid_key_with_422_validation() {
        let (router, _admin, _editor, viewer) = router_with_role_tokens().await;

        // `%20` decodes to a space in the path param - an invalid key char.
        let response = router
            .clone()
            .oneshot(put_ui_setting("bad%20key!", &viewer, "x"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let json = body_json(response).await;
        assert_eq!(json["kind"], "validation");
        assert_eq!(json["field_errors"][0]["field"], "key");

        let response = router
            .oneshot(get_auth("/api/ui-settings/bad%20key!", &viewer))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn ui_settings_routes_are_unauthorized_without_a_token() {
        let (router, _admin, _editor, _viewer) = router_with_role_tokens().await;

        let response = router
            .clone()
            .oneshot(get("/api/ui-settings/theme"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = router
            .oneshot(post_json(
                "/api/ui-settings/theme",
                json!({ "value": "glass" }),
            ))
            .await
            .unwrap();
        // POST is not a registered method on this route, but the request
        // must still die at `require_auth` (401), not reach any handler.
        assert!(
            response.status() == StatusCode::UNAUTHORIZED
                || response.status() == StatusCode::METHOD_NOT_ALLOWED
        );
    }

    // --- M14 Audit -----------------------------------------------------------

    /// Like `router_with_role_tokens`, but also returns the `AuditLogService`
    /// sharing the router's pool (so these tests can query
    /// `/api/audit-log/list` as the admin token and assert on what got
    /// recorded), and wires the login verifier through
    /// [`audited_credential_verifier`] so login events are actually recorded
    /// - `router_with_role_tokens`'s own verifier predates M14 and stays a
    ///   plain credential check since none of ITS callers care about audit
    ///   events.
    async fn router_with_role_tokens_and_audit() -> (Router, AuditLogService, String, String, String)
    {
        let pool = migrate_memory().await.expect("migrate_memory");
        let (tx, _rx) = broadcast::channel(16);
        let users = UsersService::new(pool.clone());
        let settings = SettingsService::new(pool.clone());
        let backup = unused_backup_service(pool.clone());
        let audit = AuditLogService::new(pool.clone());
        let write_targets = WriteTargetService::new(pool.clone());
        let write_rules = WriteRuleService::new(pool);

        users
            .setup_first_user("admin", "password123", "管理者")
            .await
            .expect("setup_first_user");
        users
            .create_user("editor", "password123", "編集者", Role::Editor)
            .await
            .expect("create editor");
        users
            .create_user("viewer", "password123", "閲覧者", Role::Viewer)
            .await
            .expect("create viewer");

        let auth = AuthState::new(audited_credential_verifier(users.clone(), audit.clone()));
        let admin_token = auth
            .login("admin", "password123")
            .await
            .expect("admin login");
        let editor_token = auth
            .login("editor", "password123")
            .await
            .expect("editor login");
        let viewer_token = auth
            .login("viewer", "password123")
            .await
            .expect("viewer login");

        let router = api_router(
            users,
            settings,
            audit.clone(),
            backup,
            write_targets,
            write_rules,
            auth,
            tx,
            false,
        );
        (router, audit, admin_token, editor_token, viewer_token)
    }

    /// Like `router_with_role_tokens_and_audit`, but for the M17
    /// `/api/backups/*` tests, which need a `BackupService` that ACTUALLY
    /// WORKS end to end (create/list/read/stage a real file), not
    /// [`unused_backup_service`]'s placeholder. Two things every other
    /// helper in this module gets to skip:
    /// - The router's own pool must be a real ON-DISK sqlite file, not
    ///   `:memory:` (`migrate_memory()`) - `VACUUM INTO` (which
    ///   `BackupService::create` uses) silently writes nothing when its
    ///   SOURCE connection is `:memory:` (see `crate::backup`'s test module
    ///   doc comment for the empirically-verified reason).
    /// - The returned `tempfile::TempDir` guard must be kept alive by the
    ///   caller for as long as the router is in use - dropping it deletes
    ///   the directory `backups/`/`restore-pending.sqlite3` live in.
    async fn router_with_role_tokens_and_backup(
    ) -> (Router, tempfile::TempDir, String, String, String) {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("relay-wright.sqlite3");
        // `crate::db::init_db` (not a raw `sqlx::migrate!` call here) - see
        // that module's doc comment for why this app's own schema is NOT
        // applied via `sqlx::migrate!` (it would collide with
        // `banto_tags::migrate`'s own bookkeeping on this same pool).
        let pool = crate::db::init_db(&db_path).await.expect("init_db");

        let (tx, _rx) = broadcast::channel(16);
        let users = UsersService::new(pool.clone());
        let settings = SettingsService::new(pool.clone());
        let backup = BackupService::new(db_path, pool.clone());
        let audit = AuditLogService::new(pool.clone());
        let write_targets = WriteTargetService::new(pool.clone());
        let write_rules = WriteRuleService::new(pool);

        users
            .setup_first_user("admin", "password123", "管理者")
            .await
            .expect("setup_first_user");
        users
            .create_user("editor", "password123", "編集者", Role::Editor)
            .await
            .expect("create editor");
        users
            .create_user("viewer", "password123", "閲覧者", Role::Viewer)
            .await
            .expect("create viewer");

        let auth = AuthState::new(audited_credential_verifier(users.clone(), audit.clone()));
        let admin_token = auth
            .login("admin", "password123")
            .await
            .expect("admin login");
        let editor_token = auth
            .login("editor", "password123")
            .await
            .expect("editor login");
        let viewer_token = auth
            .login("viewer", "password123")
            .await
            .expect("viewer login");

        let router = api_router(
            users,
            settings,
            audit,
            backup,
            write_targets,
            write_rules,
            auth,
            tx,
            false,
        );
        (router, dir, admin_token, editor_token, viewer_token)
    }

    /// (a) `/api/audit-log/list` is admin-only: 200 for admin, 403 for
    /// editor/viewer.
    #[tokio::test]
    async fn audit_log_list_is_admin_only() {
        let (router, _audit, admin, editor, viewer) = router_with_role_tokens_and_audit().await;

        let admin_response = router
            .clone()
            .oneshot(post_json_auth(
                "/api/audit-log/list",
                &admin,
                json!(ListParams::default()),
            ))
            .await
            .unwrap();
        assert_eq!(admin_response.status(), StatusCode::OK);

        for token in [&editor, &viewer] {
            let response = router
                .clone()
                .oneshot(post_json_auth(
                    "/api/audit-log/list",
                    token,
                    json!(ListParams::default()),
                ))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::FORBIDDEN,
                "token role mismatch"
            );
        }
    }

    #[tokio::test]
    async fn audit_log_list_requires_a_token() {
        let (router, _audit, _admin, _editor, _viewer) = router_with_role_tokens_and_audit().await;
        let response = router
            .oneshot(post_json(
                "/api/audit-log/list",
                json!(ListParams::default()),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    /// `GET /api/audit-log/config` is admin-only: 200 (with the default
    /// retention policy) for admin, 403 for editor/viewer.
    #[tokio::test]
    async fn audit_config_get_is_admin_only() {
        let (router, _audit, admin, editor, viewer) = router_with_role_tokens_and_audit().await;

        let admin_response = router
            .clone()
            .oneshot(get_auth("/api/audit-log/config", &admin))
            .await
            .unwrap();
        assert_eq!(admin_response.status(), StatusCode::OK);
        let body = body_json(admin_response).await;
        assert_eq!(body["retentionDays"], 90);
        assert_eq!(body["retentionRows"], 100_000);

        for token in [&editor, &viewer] {
            let response = router
                .clone()
                .oneshot(get_auth("/api/audit-log/config", token))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::FORBIDDEN,
                "token role mismatch"
            );
        }
    }

    /// `PUT /api/audit-log/config` (admin) persists the new policy - a
    /// following `GET` reflects it - and records a `settings_change` audit
    /// entry (spec M14: settings mutations are audited, unlike the read-only
    /// `GET`). `editor`/`viewer` are rejected with 403 and the policy is left
    /// untouched.
    #[tokio::test]
    async fn audit_config_apply_persists_and_is_admin_only() {
        let (router, _audit, admin, editor, viewer) = router_with_role_tokens_and_audit().await;

        for token in [&editor, &viewer] {
            let response = router
                .clone()
                .oneshot(put_json(
                    "/api/audit-log/config",
                    token,
                    json!({ "retentionDays": 30, "retentionRows": 5000 }),
                ))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::FORBIDDEN,
                "token role mismatch"
            );
        }

        let apply_response = router
            .clone()
            .oneshot(put_json(
                "/api/audit-log/config",
                &admin,
                json!({ "retentionDays": 30, "retentionRows": 5000 }),
            ))
            .await
            .unwrap();
        assert_eq!(apply_response.status(), StatusCode::OK);
        let applied = body_json(apply_response).await;
        assert_eq!(applied["retentionDays"], 30);
        assert_eq!(applied["retentionRows"], 5000);

        let get_response = router
            .clone()
            .oneshot(get_auth("/api/audit-log/config", &admin))
            .await
            .unwrap();
        let refetched = body_json(get_response).await;
        assert_eq!(refetched["retentionDays"], 30);
        assert_eq!(refetched["retentionRows"], 5000);

        let list_response = router
            .oneshot(post_json_auth(
                "/api/audit-log/list",
                &admin,
                json!(ListParams::default()),
            ))
            .await
            .unwrap();
        let rows = body_json(list_response).await["rows"].clone();
        let rows = rows.as_array().unwrap();
        let entry = rows
            .iter()
            .find(|r| r["action"] == "settings_change" && r["resource"] == "settings")
            .unwrap_or_else(|| panic!("expected a settings_change/settings entry, got {rows:?}"));
        assert_eq!(entry["actorUsername"], "admin");
        assert_eq!(entry["origin"], "rest");
        assert_eq!(entry["result"], "ok");
    }

    /// (c) A viewer's rejected write is recorded as `denied`.
    #[tokio::test]
    async fn viewer_write_denial_is_recorded_as_denied() {
        let (router, _audit, admin, _editor, viewer) = router_with_role_tokens_and_audit().await;

        // `/api/users` is `admin`-only (spec M10), so a `viewer` token is
        // denied by `RoleGuard` here the same way it would be on any other
        // guarded mutating route - this only needs ONE such route to prove
        // `require_role_at_least` records the denial (spec M14), not
        // resource-specific coverage.
        let response = router
            .clone()
            .oneshot(post_json_auth(
                "/api/users",
                &viewer,
                json!({
                    "username": "nope",
                    "password": "password123",
                    "displayName": "Nope",
                    "role": "viewer"
                }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let list_response = router
            .oneshot(post_json_auth(
                "/api/audit-log/list",
                &admin,
                json!(ListParams::default()),
            ))
            .await
            .unwrap();
        let rows = body_json(list_response).await["rows"].clone();
        let rows = rows.as_array().unwrap();
        let entry = rows
            .iter()
            .find(|r| r["action"] == "denied" && r["resource"] == "users")
            .unwrap_or_else(|| panic!("expected a denied/users entry, got {rows:?}"));
        assert_eq!(entry["actorUsername"], "viewer");
        assert_eq!(entry["actorRole"], "viewer");
        assert_eq!(entry["result"], "denied");
    }

    /// `users` create/reset-password entries must never leak the plaintext
    /// password into `detail` (spec M14's hard rule - see
    /// `crate::audit`'s module doc comment).
    #[tokio::test]
    async fn users_create_audit_entry_never_contains_the_password() {
        let (router, _audit, admin, _editor, _viewer) = router_with_role_tokens_and_audit().await;

        router
            .clone()
            .oneshot(post_json_auth(
                "/api/users",
                &admin,
                json!({
                    "username": "newperson",
                    "password": "supersecret1",
                    "displayName": "New Person",
                    "role": "viewer"
                }),
            ))
            .await
            .unwrap();

        let list_response = router
            .oneshot(post_json_auth(
                "/api/audit-log/list",
                &admin,
                json!(ListParams::default()),
            ))
            .await
            .unwrap();
        let rows = body_json(list_response).await["rows"].clone();
        let rows = rows.as_array().unwrap();
        let entry = rows
            .iter()
            .find(|r| r["action"] == "create" && r["resource"] == "users")
            .expect("expected a create/users entry");
        assert_eq!(entry["actorUsername"], "admin");
        let detail = entry["detail"].as_str().expect("detail should be set");
        assert!(
            !detail.contains("supersecret1"),
            "audit detail must never contain the password: {detail}"
        );
        assert!(detail.contains("newperson"));
    }

    /// (d) A failed login attempt is recorded as `login_failed`. Uses
    /// `router_with_real_login` (not `router_with_role_tokens_and_audit`)
    /// since it wires `/api/auth/login` through the same
    /// `audited_credential_verifier` production code path.
    #[tokio::test]
    async fn login_failure_is_recorded_as_login_failed() {
        let (router, audit) = router_with_real_login(true).await;
        setup_and_get_token(&router).await; // creates the "owner" admin account

        let response = router
            .oneshot(post_json(
                "/api/auth/login",
                json!({ "username": "owner", "password": "wrong-password" }),
            ))
            .await
            .unwrap();
        assert_eq!(body_json(response).await["success"], false);

        let result = audit.list(ListParams::default()).await.unwrap();
        let entry = result
            .rows
            .iter()
            .find(|r| r.action == "login_failed")
            .unwrap_or_else(|| panic!("expected a login_failed entry, got {:?}", result.rows));
        assert_eq!(entry.actor_username.as_deref(), Some("owner"));
        assert_eq!(entry.actor_role, None);
        assert_eq!(entry.result, "failed");
    }

    #[tokio::test]
    async fn login_success_is_recorded_as_login() {
        let (router, audit) = router_with_real_login(true).await;
        setup_and_get_token(&router).await;

        router
            .clone()
            .oneshot(post_json(
                "/api/auth/login",
                json!({ "username": "owner", "password": "password123" }),
            ))
            .await
            .unwrap();

        let result = audit.list(ListParams::default()).await.unwrap();
        assert!(
            result
                .rows
                .iter()
                .any(|r| r.action == "login" && r.actor_username.as_deref() == Some("owner")),
            "expected a login entry, got {:?}",
            result.rows
        );
    }

    #[tokio::test]
    async fn logout_is_recorded() {
        let (router, audit) = router_with_real_login(true).await;
        let token = setup_and_get_token(&router).await;

        router
            .oneshot(
                HttpRequest::post("/api/auth/logout")
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .header("Authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let result = audit.list(ListParams::default()).await.unwrap();
        assert!(
            result
                .rows
                .iter()
                .any(|r| r.action == "logout" && r.actor_username.as_deref() == Some("owner")),
            "expected a logout entry, got {:?}",
            result.rows
        );
    }

    #[tokio::test]
    async fn setup_is_recorded() {
        let (router, audit) = router_with_real_login(true).await;
        setup_and_get_token(&router).await;

        let result = audit.list(ListParams::default()).await.unwrap();
        assert!(
            result
                .rows
                .iter()
                .any(|r| r.action == "setup" && r.actor_username.as_deref() == Some("owner")),
            "expected a setup entry, got {:?}",
            result.rows
        );
    }

    /// Spec M14 (coordinator review): a self-service password change is a
    /// security event and must be recorded as `password_change` (actor =
    /// entity = the caller) - and the entry must never carry the password.
    #[tokio::test]
    async fn change_password_is_recorded_as_password_change() {
        let (router, audit) = router_with_real_login(true).await;
        let token = setup_and_get_token(&router).await;

        let change_request = HttpRequest::post("/api/auth/change-password")
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .header("Authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(
                json!({ "currentPassword": "password123", "newPassword": "newpassword1" })
                    .to_string(),
            ))
            .unwrap();
        let response = router.oneshot(change_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let result = audit.list(ListParams::default()).await.unwrap();
        let entry = result
            .rows
            .iter()
            .find(|r| r.action == "password_change")
            .unwrap_or_else(|| panic!("expected a password_change entry, got {:?}", result.rows));
        assert_eq!(entry.actor_username.as_deref(), Some("owner"));
        assert_eq!(entry.actor_role.as_deref(), Some("admin"));
        assert_eq!(entry.resource, "users");
        // `setup_first_user` creates the very first row -> id 1.
        assert_eq!(entry.entity_id.as_deref(), Some("1"));
        assert_eq!(entry.origin, "rest");
        assert_eq!(entry.result, "ok");
        assert_eq!(entry.detail, None, "detail must never carry the password");
    }

    // --- M17: SQLite backup/restore -------------------------------------------

    fn post_bytes_auth(path: &str, token: &str, bytes: Vec<u8>) -> HttpRequest<Body> {
        HttpRequest::post(path)
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .header("Authorization", format!("Bearer {token}"))
            .header("content-type", "application/octet-stream")
            .body(Body::from(bytes))
            .unwrap()
    }

    async fn body_bytes(response: axum::response::Response) -> Vec<u8> {
        axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec()
    }

    /// admin can create a backup, see it in the list, and download the exact
    /// same bytes back (spec M17: "バックアップファイルが作成・ダウンロード
    /// でき"). `POST /api/backups` is recorded as `action: "backup"`.
    #[tokio::test]
    async fn admin_can_create_list_and_download_backups() {
        let (router, _dir, admin, _editor, _viewer) = router_with_role_tokens_and_backup().await;

        let create_response = router
            .clone()
            .oneshot(post_bytes_auth("/api/backups", &admin, Vec::new()))
            .await
            .unwrap();
        assert_eq!(create_response.status(), StatusCode::OK);
        let created = body_json(create_response).await;
        let file_name = created["fileName"].as_str().expect("fileName").to_string();
        assert!(created["sizeBytes"].as_u64().unwrap() > 0);

        let list_response = router
            .clone()
            .oneshot(get_auth("/api/backups", &admin))
            .await
            .unwrap();
        assert_eq!(list_response.status(), StatusCode::OK);
        let listed = body_json(list_response).await;
        assert_eq!(listed.as_array().unwrap().len(), 1);
        assert_eq!(listed[0]["fileName"], file_name);

        let download_response = router
            .oneshot(get_auth(&format!("/api/backups/{file_name}"), &admin))
            .await
            .unwrap();
        assert_eq!(download_response.status(), StatusCode::OK);
        let disposition = download_response
            .headers()
            .get(axum::http::header::CONTENT_DISPOSITION)
            .expect("Content-Disposition header")
            .to_str()
            .unwrap()
            .to_string();
        assert!(disposition.contains("attachment"));
        assert!(disposition.contains(&file_name));
        let bytes = body_bytes(download_response).await;
        assert_eq!(&bytes[0..16], b"SQLite format 3\0");
    }

    /// `editor`/`viewer` cannot reach ANY `/api/backups/*` route (spec M17:
    /// "admin以外は全API 403") - checked against both a read route (`GET
    /// /api/backups`) and a write route (`POST /api/backups`).
    #[tokio::test]
    async fn editor_and_viewer_cannot_access_backups_routes() {
        let (router, _dir, _admin, editor, viewer) = router_with_role_tokens_and_backup().await;

        for token in [&editor, &viewer] {
            let list_response = router
                .clone()
                .oneshot(get_auth("/api/backups", token))
                .await
                .unwrap();
            assert_eq!(list_response.status(), StatusCode::FORBIDDEN);
            let json = body_json(list_response).await;
            assert_eq!(json["kind"], "forbidden");

            let create_response = router
                .clone()
                .oneshot(post_bytes_auth("/api/backups", token, Vec::new()))
                .await
                .unwrap();
            assert_eq!(create_response.status(), StatusCode::FORBIDDEN);
        }
    }

    /// Uploading garbage bytes to `/api/backups/restore` must be rejected
    /// (spec M17: "壊れたファイルのリストアが検証で拒否される") - `Validation`
    /// maps to `422` (`banto_server::response::status_for`), and no pending
    /// restore is left staged.
    #[tokio::test]
    async fn restore_upload_of_garbage_bytes_is_rejected_as_validation() {
        let (router, _dir, admin, _editor, _viewer) = router_with_role_tokens_and_backup().await;

        let response = router
            .clone()
            .oneshot(post_bytes_auth(
                "/api/backups/restore",
                &admin,
                b"not a sqlite file".to_vec(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let json = body_json(response).await;
        assert_eq!(json["kind"], "validation");

        let pending_response = router
            .oneshot(get_auth("/api/backups/pending-restore", &admin))
            .await
            .unwrap();
        assert_eq!(body_json(pending_response).await, serde_json::Value::Null);
    }

    /// Full stage-from-existing-backup -> cancel round trip (spec M17),
    /// asserting both the `pending-restore` status endpoint AND the
    /// `restore_staged`/`restore_cancelled` audit entries it records.
    #[tokio::test]
    async fn stage_restore_from_existing_backup_then_cancel_is_recorded_in_the_audit_log() {
        let (router, _dir, admin, _editor, _viewer) = router_with_role_tokens_and_backup().await;

        let create_response = router
            .clone()
            .oneshot(post_bytes_auth("/api/backups", &admin, Vec::new()))
            .await
            .unwrap();
        let file_name = body_json(create_response).await["fileName"]
            .as_str()
            .unwrap()
            .to_string();

        let stage_response = router
            .clone()
            .oneshot(post_bytes_auth(
                &format!("/api/backups/{file_name}/restore"),
                &admin,
                Vec::new(),
            ))
            .await
            .unwrap();
        assert_eq!(stage_response.status(), StatusCode::NO_CONTENT);

        let pending_response = router
            .clone()
            .oneshot(get_auth("/api/backups/pending-restore", &admin))
            .await
            .unwrap();
        let pending = body_json(pending_response).await;
        assert!(pending["sizeBytes"].as_u64().unwrap() > 0);

        let cancel_response = router
            .clone()
            .oneshot(delete_auth("/api/backups/pending-restore", &admin))
            .await
            .unwrap();
        assert_eq!(cancel_response.status(), StatusCode::NO_CONTENT);

        let pending_after_cancel = router
            .clone()
            .oneshot(get_auth("/api/backups/pending-restore", &admin))
            .await
            .unwrap();
        assert_eq!(
            body_json(pending_after_cancel).await,
            serde_json::Value::Null
        );

        let audit_response = router
            .oneshot(post_json_auth(
                "/api/audit-log/list",
                &admin,
                json!(ListParams::default()),
            ))
            .await
            .unwrap();
        let rows = body_json(audit_response).await["rows"].clone();
        let rows = rows.as_array().unwrap();
        assert!(
            rows.iter()
                .any(|r| r["action"] == "backup" && r["resource"] == "backups"),
            "expected a backup entry, got {rows:?}"
        );
        assert!(
            rows.iter()
                .any(|r| r["action"] == "restore_staged" && r["resource"] == "backups"),
            "expected a restore_staged entry, got {rows:?}"
        );
        assert!(
            rows.iter()
                .any(|r| r["action"] == "restore_cancelled" && r["resource"] == "backups"),
            "expected a restore_cancelled entry, got {rows:?}"
        );
    }

    // --- W2: write registry dual-path symmetry ------------------------------

    /// Router with a seeded PLC connection (so a write target can be created),
    /// real admin/editor/viewer accounts + tokens, and the shared audit
    /// service/pool exposed so the W2 dual-path tests can assert both the
    /// write result AND the audit trail (spec §1 両経路対称: the REST path
    /// must produce the same authz + audit as the Tauri path).
    async fn write_registry_router_test() -> (Router, AuditLogService, i64, String, String) {
        use banto_tags::{PlcConnectionInput, PlcConnectionService};

        let pool = migrate_memory().await.expect("migrate_memory");
        let (tx, _rx) = broadcast::channel(16);
        let users = UsersService::new(pool.clone());
        let settings = SettingsService::new(pool.clone());
        let backup = unused_backup_service(pool.clone());
        let audit = AuditLogService::new(pool.clone());
        let write_targets = WriteTargetService::new(pool.clone());
        let write_rules = WriteRuleService::new(pool.clone());

        let conn = PlcConnectionService::new(pool.clone())
            .create(PlcConnectionInput {
                name: "PLC1".to_string(),
                protocol: "modbus-tcp".to_string(),
                host: "10.0.0.1".to_string(),
                port: 502,
                unit_id: 1,
                enabled: true,
            })
            .await
            .expect("seed plc connection");

        users
            .setup_first_user("admin", "password123", "管理者")
            .await
            .expect("setup_first_user");
        users
            .create_user("editor", "password123", "編集者", Role::Editor)
            .await
            .expect("create editor");
        users
            .create_user("viewer", "password123", "閲覧者", Role::Viewer)
            .await
            .expect("create viewer");

        let auth = AuthState::new(audited_credential_verifier(users.clone(), audit.clone()));
        let editor_token = auth
            .login("editor", "password123")
            .await
            .expect("editor login");
        let viewer_token = auth
            .login("viewer", "password123")
            .await
            .expect("viewer login");

        let router = api_router(
            users,
            settings,
            audit.clone(),
            backup,
            write_targets,
            write_rules,
            auth,
            tx,
            false,
        );
        (router, audit, conn.id, editor_token, viewer_token)
    }

    /// An `editor` can create a write target over REST, and the mutation is
    /// recorded to the audit log with `origin: "rest"` - the REST half of the
    /// dual-path create+audit symmetry (the Tauri half is asserted in
    /// `src-tauri`'s own tests).
    #[tokio::test]
    async fn rest_editor_can_create_write_target_and_it_is_audited() {
        let (router, audit, plc_id, editor, _viewer) = write_registry_router_test().await;
        let response = router
            .oneshot(post_json_auth(
                "/api/write-targets",
                &editor,
                json!({
                    "name": "WT1",
                    "plcConnectionId": plc_id,
                    "address": "D100",
                    "dataType": "i16"
                }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let created = body_json(response).await;
        assert_eq!(created["name"], "WT1");

        let entries = audit.list(ListParams::default()).await.unwrap();
        let entry = entries
            .rows
            .iter()
            .find(|r| r.action == "create" && r.resource == "write_targets")
            .expect("expected a write_targets create audit entry");
        assert_eq!(entry.origin, "rest");
        assert_eq!(entry.result, "ok");
        assert_eq!(entry.actor_username.as_deref(), Some("editor"));
    }

    /// A `viewer` is denied (403) when trying to create a write target, and
    /// the denial is recorded (`action: "denied"`, `origin: "rest"`) - proving
    /// `require_editor` gates writes and audits the denial exactly as the
    /// admin `RoleGuard`/Tauri `require_role` do.
    #[tokio::test]
    async fn rest_viewer_cannot_create_write_target_and_denial_is_audited() {
        let (router, audit, plc_id, _editor, viewer) = write_registry_router_test().await;
        let response = router
            .oneshot(post_json_auth(
                "/api/write-targets",
                &viewer,
                json!({
                    "name": "WT1",
                    "plcConnectionId": plc_id,
                    "address": "D100",
                    "dataType": "i16"
                }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let entries = audit.list(ListParams::default()).await.unwrap();
        assert!(
            entries
                .rows
                .iter()
                .any(|r| r.action == "denied" && r.resource == "write_targets"),
            "expected a denied entry for write_targets, got {:?}",
            entries.rows
        );
        // And nothing was actually created.
        assert!(!entries
            .rows
            .iter()
            .any(|r| r.action == "create" && r.resource == "write_targets"));
    }
}
