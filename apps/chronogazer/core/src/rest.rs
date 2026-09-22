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
//! | GET    | `/api/plc-connections`      | -              | `PlcConnectionResponse[]` (viewer+, #383 段階2a/R1-B) |
//! | POST   | `/api/plc-connections`      | `PlcConnectionPayload` | `PlcConnectionResponse` (editor+) |
//! | GET    | `/api/plc-connections/{id}` | -              | `PlcConnectionResponse` (viewer+) |
//! | PUT    | `/api/plc-connections/{id}` | `PlcConnectionPayload` | `PlcConnectionResponse` (editor+) |
//! | DELETE | `/api/plc-connections/{id}` | -              | 204 (editor+)           |
//! | GET    | `/api/collection-groups`      | -            | `CollectionGroup[]` (viewer+) |
//! | POST   | `/api/collection-groups`      | `CollectionGroupPayload` | `CollectionGroup` (editor+) |
//! | GET    | `/api/collection-groups/{id}` | -            | `CollectionGroup` (viewer+) |
//! | PUT    | `/api/collection-groups/{id}` | `CollectionGroupPayload` | `CollectionGroup` (editor+) |
//! | DELETE | `/api/collection-groups/{id}` | -            | 204 (editor+)           |
//! | GET    | `/api/tags`      | -                        | `Tag[]` (viewer+)       |
//! | POST   | `/api/tags`      | `TagPayload`             | `Tag` (editor+)         |
//! | GET    | `/api/tags/{id}` | -                        | `Tag` (viewer+)         |
//! | PUT    | `/api/tags/{id}` | `TagPayload`             | `Tag` (editor+)         |
//! | DELETE | `/api/tags/{id}` | -                        | 204 (editor+)           |
//! | GET    | `/api/collect`   | -                        | `CollectorStateView` (viewer+, #383 段階2b/R1-C) |
//! | POST   | `/api/collect/start\|stop\|restart` | -     | `CollectOutcome` (editor+) |
//! | GET    | `/api/collect/values` | -                   | `Readout<{[tagKey]: CurrentSampleView}>` (viewer+, C-3a) |
//! | GET    | `/api/collect/connections` | -              | `Readout<{[connKey]: ConnectionStatusView}>` (viewer+, C-3a) |
//! | GET    | `/api/collect/events?offset=&limit=&asOfId=` | -    | `Readout<CollectEventList>` (viewer+, C-3a / `asOfId` = #409) |
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
//! Tauri command instead, spec §10), while `banto-serve` enables it via
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
//! the route's minimum. Only `admin` can manage other accounts.
//!
//! `/api/plc-connections/*`, `/api/collection-groups/*` and `/api/tags/*`
//! (#383 段階2a / R1-B, `docs/recorder-requirements.md` §3.6) instead follow
//! `viewer`-read / `editor`+-write: unlike the `admin`-only routers above,
//! their GET routes need only `require_auth` (any role), so read and write
//! share a path but need different floors - [`tag_registry_router`] uses
//! [`require_editor`] inline on each write handler rather than a
//! single-floor `RoleGuard` layer. A future 表示グループ resource would
//! follow the same pattern.
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
//! `/api/plc-connections/*`/`/api/collection-groups/*`/`/api/tags/*` (#383
//! 段階2a / R1-B): [`require_editor`] records `action: "denied"` the same
//! way [`require_role_at_least`] does above; a successful create/update/
//! delete records via [`record_write`] with `resource` one of
//! `"plc_connections"`/`"collection_groups"`/`"tags"` and `entity_id` the
//! row id - same helper, same shape as every other mutating handler in this
//! module. Reads are never audited (same convention).
//!
//! `/api/collect*` (#383 段階2b / R1-C): **読み取り（`GET`）は `viewer`
//! 以上・変更（`POST`）は `editor` 以上**（`admin` ではない - [`collect_router`]
//! の doc に両方の根拠）。**床は 2 段でも `resource` は 1 つ** - 拒否も成功も
//! `resource: `[`COLLECT_AUDIT_RESOURCE`]（`"collect"`）で、これは
//! `src-tauri` の `collect_*` コマンドが参照するのと**同じ定数** - 同じ操作が
//! 経路によって別の `resource` にならないようにするため（#397 P2-D）。
//! 成功の `action` は `"start"`/`"stop"`/`"restart"`、`detail` は結果の状態と
//! `pending` だけ。`GET /api/collect`（読み取り）は監査しない。
//!
//! `GET /api/collect` が返すのは**公開用の**
//! [`crate::collect::CollectorStateView`]（`startFailed` は返すが理由は
//! 返さない）で、内部の `CollectorState` ではない - 床を `viewer` まで
//! 下げた根拠を型で担保するため（#407 レビュー P2-2）。**キューに入れられ
//! なかった変更操作は `result: "ok"` で記録されない**（`?` でエラーとして
//! 抜けるので `record_collect_operation` に届かない）- 「受け付けた」と
//! 監査にも嘘を書かないための形（#407 レビュー P2-1）。
//!
//! C-3a で足した読み出し 3 本（`/api/collect/values` / `.../connections` /
//! `.../events`）も**同じ床・同じ `resource`**で、**どれも監査しない**
//! （読み取りは監査しない、という全体の規約）。3 本とも
//! [`crate::collect::Readout`] を返す - 「**走っていない**」「**読めなかった**」
//! 「**読めて 0 件**」を**別の値**にするためで、空のコレクションを返して
//! 3 つを 1 つに潰さない（docs/implementation-checklist.md §5）。載せる情報も
//! 3 本とも公開用の型に通してあり、とくに `collect_events.detail`（自由文。
//! 接続先やファイルパスを含みうる）は**型でも SQL でも落としてある** -
//! `crate::collect` のモジュール doc「公開用の型 - 何を載せ、何を落としたか」。
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
//! comment and its callers in `src-tauri`'s `run()`/`bin/banto-serve.rs`'s
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
use axum::routing::{get, post, put};
use axum::{Json, Router};
use banto_core::{BantoError, ErrorBody, FieldError, ListParams, ListResult};
use banto_server::{
    auth_routes, require_auth, require_banto_client_header, sse_route, ApiError, AuthState,
    Identity, ServerEvent,
};
use banto_tags::{
    CollectionGroup, CollectionGroupInput, CollectionGroupService, PlcConnection,
    PlcConnectionInput, PlcConnectionService, Tag, TagInput, TagService, MODBUS_PROTOCOL,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::str::FromStr;
use tokio::sync::broadcast;

use crate::audit::{AuditEntry, AuditLogService};
use crate::backup::{BackupInfo, BackupService, PendingRestoreInfo};
use crate::collect::{
    CollectEventList, CollectOutcome, CollectorService, CollectorStateView, ConnectionStatusView,
    CurrentSampleView, EventPage, Readout, COLLECT_AUDIT_RESOURCE, COLLECT_OPERATION_ROLE,
    COLLECT_READ_ROLE,
};
use crate::hub::{HubService, HubSubscriptionView, HubView};
use crate::settings::{AuditSettings, SettingsService};
use crate::users::{Role, UserIdentity, UserSummary, UsersService};

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

/// Resolve the caller's identity and require role >= `editor` (R0 §3.6:
/// resources are viewer-read / editor-write). Records a `denied` audit entry
/// (mirroring [`require_role_at_least`]) when an AUTHENTICATED caller's role
/// is too low, and returns `BantoError::Forbidden`; no valid session at all
/// returns `BantoError::Unauthorized` (deliberately NOT audited, same
/// reasoning as the RBAC middleware: nothing resembling a real user to
/// attribute a denial to). Used inline by [`tag_registry_router`]'s write
/// handlers, which - unlike the admin-only routers above - cannot use a
/// single-floor `RoleGuard` middleware because their GET routes are only
/// `require_auth` (viewer+), so read and write share a path but need
/// different floors. This is the REST twin of `src-tauri`'s `require_role`,
/// and the same helper relay-wright's `rest.rs` uses for its own R1-B
/// registry router.
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
/// M14). Shared by `banto-serve` (the standalone REST dev server) and
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
/// (`bin/banto-serve.rs`'s `main`/`src-tauri`'s `run()`) is judged
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

// --- #332: Hub 接続 ----------------------------------------------------------

/// `/api/hub/*` のハンドラ用 state（`BackupsState` と同じ構成）: 操作本体の
/// [`HubService`]、監査記録用の [`AuditLogService`]、actor 解決用の
/// [`AuthState`]。
#[derive(Clone)]
struct HubState {
    hub: HubService,
    audit: AuditLogService,
    auth: AuthState,
}

/// `POST /api/hub/connect` の body。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HubConnectBody {
    endpoint: String,
}

/// `POST /api/hub/adopt-key` の body。`key` は**ログにも監査 detail にも
/// 出さない**（`src-tauri` の `hub_adopt_manual_key` と同じ扱い）。
/// `Debug` を意図的に derive していない - うっかり `{:?}` で平文キーを
/// 出せないようにするため。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HubAdoptBody {
    endpoint: String,
    key: String,
}

/// `PUT /api/hub/selected-tags` の body。空配列は正当な入力（受入条件）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HubSelectedTagsBody {
    tags: Vec<String>,
}

/// `settings_change`/`settings` の監査エントリを 1 本記録する（Hub 操作
/// 共通）。`detail` には接続先とキー名までしか入れない - 平文キーは
/// どの経路にも出さない。
async fn record_hub_change(state: &HubState, headers: &HeaderMap, detail: serde_json::Value) {
    let identity = actor_identity(headers, &state.auth);
    state
        .audit
        .record(AuditEntry {
            actor_username: identity.as_ref().map(|i| i.id.as_str()),
            actor_role: identity.as_ref().map(|i| i.role.as_str()),
            action: "settings_change",
            resource: "settings",
            entity_id: None,
            detail: Some(detail),
            origin: "rest",
            result: "ok",
        })
        .await;
}

/// `GET /api/hub`（`admin` 限定）: 保存済み設定での現在状態。読み取りなので
/// 監査しない（他の read ルートと同じ規約）。**発行は行わない。**
async fn hub_status_handler(State(state): State<HubState>) -> Result<Json<HubView>, ApiError> {
    Ok(Json(state.hub.status().await?))
}

/// `POST /api/hub/connect`（`admin` 限定）。
async fn hub_connect_handler(
    State(state): State<HubState>,
    headers: HeaderMap,
    Json(body): Json<HubConnectBody>,
) -> Result<Json<HubView>, ApiError> {
    let view = state.hub.connect(&body.endpoint).await?;
    record_hub_change(
        &state,
        &headers,
        json!({ "hubEndpoint": body.endpoint, "hubStatus": view.status.as_str() }),
    )
    .await;
    Ok(Json(view))
}

/// `POST /api/hub/adopt-key`（`admin` 限定）: ロックダウン済み Hub 向けの
/// 手動連携。
async fn hub_adopt_key_handler(
    State(state): State<HubState>,
    headers: HeaderMap,
    Json(body): Json<HubAdoptBody>,
) -> Result<Json<HubView>, ApiError> {
    let endpoint = body.endpoint;
    let view = state.hub.adopt_manual_key(&endpoint, body.key).await?;
    record_hub_change(
        &state,
        &headers,
        json!({ "hubEndpoint": endpoint, "hubKeyAdopted": true, "hubStatus": view.status.as_str() }),
    )
    .await;
    Ok(Json(view))
}

/// `POST /api/hub/refresh`（`admin` 限定）: タグ一覧の再取得。
async fn hub_refresh_handler(State(state): State<HubState>) -> Result<Json<HubView>, ApiError> {
    Ok(Json(state.hub.refresh_catalog().await?))
}

/// `GET /api/hub/subscription`（`admin` 限定、#383 段階1）: 購読の状態だけ。
///
/// **ネットワークを叩かない**（メモリ上の `watch` を読むだけ）ので、設定
/// 画面はこれをポーリングしてよい。`GET /api/hub` は catalog を毎回
/// 取り直すので、ポーリングには使わない。読み取りなので監査しない。
async fn hub_subscription_handler(State(state): State<HubState>) -> Json<HubSubscriptionView> {
    Json(state.hub.subscription().await)
}

/// `PUT /api/hub/selected-tags`（`admin` 限定）。
async fn hub_selected_tags_handler(
    State(state): State<HubState>,
    headers: HeaderMap,
    Json(body): Json<HubSelectedTagsBody>,
) -> Result<StatusCode, ApiError> {
    let count = body.tags.len();
    state.hub.set_selected_tags(body.tags).await?;
    record_hub_change(&state, &headers, json!({ "hubSelectedTagCount": count })).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /api/hub`（`admin` 限定）: ローカルの設定とキーリングだけを消す。
/// Hub 側のキーは失効させない（`banto_hub_bootstrap::Bootstrapper::disconnect`
/// の doc comment参照）。
async fn hub_disconnect_handler(
    State(state): State<HubState>,
    headers: HeaderMap,
) -> Result<Json<HubView>, ApiError> {
    let view = state.hub.disconnect().await?;
    record_hub_change(&state, &headers, json!({ "hubDisconnected": true })).await;
    Ok(Json(view))
}

/// `/api/hub/*`（#332）: `admin` 限定、`users_router`/`backups_router` と
/// 同じ掛け方（`require_auth` → `require_role_at_least`）。
fn hub_router(hub: HubService, audit: AuditLogService, auth: AuthState) -> Router {
    let state = HubState {
        hub,
        audit: audit.clone(),
        auth: auth.clone(),
    };
    Router::new()
        .route(
            "/api/hub",
            get(hub_status_handler).delete(hub_disconnect_handler),
        )
        .route("/api/hub/connect", post(hub_connect_handler))
        .route("/api/hub/adopt-key", post(hub_adopt_key_handler))
        .route("/api/hub/refresh", post(hub_refresh_handler))
        .route("/api/hub/subscription", get(hub_subscription_handler))
        .route("/api/hub/selected-tags", put(hub_selected_tags_handler))
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            RoleGuard {
                auth: auth.clone(),
                min: Role::Admin,
                resource: "hub",
                audit,
            },
            require_role_at_least,
        ))
        .layer(middleware::from_fn_with_state(auth, require_auth))
}

// --- #383 段階2b / R1-C（C-2）: 収集の操作 -----------------------------------

/// `/api/collect*` のハンドラ用 state（[`HubState`] と同じ構成）: 操作本体の
/// [`CollectorService`]、監査記録用の [`AuditLogService`]、actor 解決用の
/// [`AuthState`]。
#[derive(Clone)]
struct CollectState {
    collect: CollectorService,
    audit: AuditLogService,
    auth: AuthState,
}

/// 収集の操作が成功したことを 1 本記録する（`start`/`stop`/`restart` 共通）。
///
/// `resource` は [`COLLECT_AUDIT_RESOURCE`]、つまり**Tauri 側の `collect_*`
/// コマンドと同じ定数**（両経路で `resource` が割れないように - その定数の
/// doc 参照）。`detail` に入れるのは**結果の状態**だけで、接続先・
/// ファイルパス・[`crate::collect::CollectorState::StartFailed`] の理由と
/// いった**内部の詳細は入れない**（`CollectorState::as_str` の doc）。
/// `pending` を載せるのは、
/// 「操作は受け付けたが、応答を待つのをやめた」を後から読み分けられるように
/// するため - **打ち切りは失敗ではない**ので `result` は `"ok"` のまま。
async fn record_collect_operation(
    state: &CollectState,
    headers: &HeaderMap,
    action: &'static str,
    outcome: &CollectOutcome,
) {
    let identity = actor_identity(headers, &state.auth);
    state
        .audit
        .record(AuditEntry {
            actor_username: identity.as_ref().map(|i| i.id.as_str()),
            actor_role: identity.as_ref().map(|i| i.role.as_str()),
            action,
            resource: COLLECT_AUDIT_RESOURCE,
            entity_id: None,
            detail: Some(
                json!({ "collectState": outcome.status.as_str(), "pending": outcome.pending }),
            ),
            origin: "rest",
            result: "ok",
        })
        .await;
}

/// `GET /api/collect`（**`viewer` 以上** - [`COLLECT_READ_ROLE`]）: 収集の
/// 現在の状態。
///
/// **ネットワークもディスクも DB も触らない**（メモリ上の状態を読むだけ）
/// ので、画面はこれをポーリングしてよい。読み取りなので監査しない
/// （他の read ルートと同じ規約）。
///
/// 返すのは**公開用の [`CollectorStateView`]** であって、内部の
/// `CollectorState` ではない（#407 レビュー P2-2）。内部の型は
/// `StartFailed { reason }` を持っていて、その `reason` には接続先のホストや
/// `data.dir` の絶対パスが載りうる - 床を `viewer` まで下げた根拠が
/// 「状態に機微な情報が無いこと」なので、そこは型で担保する。**変換は
/// [`CollectorService::state_view`] の 1 箇所**で、`src-tauri` の
/// `collect_status` も同じものを通る（経路によって公開する情報が割れない）。
/// 起動失敗の理由が利用者へどこで伝わるかは `crate::collect` のモジュール
/// doc「理由はどこで利用者に伝わるか」。
async fn collect_status_handler(State(state): State<CollectState>) -> Json<CollectorStateView> {
    Json(state.collect.state_view())
}

/// `GET /api/collect/values`（**`viewer` 以上** - [`COLLECT_READ_ROLE`]、
/// #383 段階2b / R1-C の C-3a）: タグごとの現在値。
///
/// **キューもディスクも DB も触らない**（葉に公開してある現在値ハンドルから
/// `snapshot()` を取るだけ）ので、画面はこれをポーリングしてよい - 遅い
/// `start()` の後ろで待たされない（`crate::collect` のモジュール doc
/// 「現在値はキューから外して葉に公開する」）。読み取りなので監査しない。
///
/// 返すのは [`Readout`]: **「走っていない」と「読めて 0 件」を別の値**にする
/// （空の `{}` を返して 2 つを潰さない）。値の型は公開用の
/// [`CurrentSampleView`] で、**品質と時刻を必ず載せる**（画面が Stale / Bad を
/// 出し分けられること自体が R0 §3.2 の要求）。
async fn collect_values_handler(
    State(state): State<CollectState>,
) -> Json<Readout<HashMap<String, CurrentSampleView>>> {
    Json(state.collect.values())
}

/// `GET /api/collect/connections`（**`viewer` 以上**、C-3a）: 接続ごとの状態。
///
/// **3 つの結末を返しうる唯一の口**（走っていない / 読めなかった / 読めて
/// 0 件）。「読めなかった」は、ライフサイクルタスクが遅い `start`/`stop` を
/// 処理している最中に `crate::collect::COLLECT_READ_TIMEOUT` が過ぎた場合 -
/// **「失敗しました」とも「接続 0 件」とも言わない**（#400 の言い分け）。
async fn collect_connections_handler(
    State(state): State<CollectState>,
) -> Json<Readout<HashMap<String, ConnectionStatusView>>> {
    Json(state.collect.connections().await)
}

/// `GET /api/collect/events?offset=&limit=&asOfId=` のクエリ（C-3a。
/// `asOfId` は #409 レビュー P2-2）。
///
/// どれも省略可（既定は先頭から `crate::collect::COLLECT_EVENTS_DEFAULT_LIMIT`
/// 件、境界はその時点の最大 `id`）。範囲外の `limit` は [`EventPage::new`] が
/// 丸める - 拒否しないのは、画面のページャが素直に使えるようにするため。
/// `asOfId` はスナップショット境界（[`EventPage`] の doc）で、画面は世代の
/// 最初の応答で返ってきた値を後続ブロックに渡す。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CollectEventsQuery {
    #[serde(default)]
    offset: Option<u64>,
    #[serde(default)]
    limit: Option<u64>,
    #[serde(default)]
    as_of_id: Option<i64>,
}

/// `GET /api/collect/events`（**`viewer` 以上**、C-3a）: `collect_events` の
/// 1 ページ（**新しい順**）。
///
/// **監査ログ一覧（`POST /api/audit-log/list`）の流儀に倣う** - 総件数は
/// `ListResult::total_count`（`totalCount`）で返し、既定の取得件数も同じ 50。
/// 違うのは `GET` + クエリ文字列にしたこと（並べ替え・絞り込みは持たない）と、
/// **取得件数に上限を掛けた**こと（この口は `viewer` にも開いているため -
/// `crate::collect::COLLECT_EVENTS_MAX_LIMIT`）。
///
/// **`detail` 列は返さない**（`crate::collect::CollectEventRow` の doc）:
/// 自由文で、切断理由（接続先を含みうる）や書き込みエラー（ファイルパスを
/// 含みうる）がそのまま入っている列なので、SQL と型の両方で落としてある。
///
/// **収集が止まっていても読める**（`Readout::NotRunning` を返さない）- 過去の
/// 記録であって走っているエンジンの覗き窓ではないため。走っているかどうかは
/// `GET /api/collect` を併せて読むこと。
async fn collect_events_handler(
    State(state): State<CollectState>,
    Query(query): Query<CollectEventsQuery>,
) -> Json<Readout<CollectEventList>> {
    Json(
        state
            .collect
            .events(EventPage::new(query.offset, query.limit).as_of(query.as_of_id))
            .await,
    )
}

/// `POST /api/collect/start`（**`editor` 以上** - [`COLLECT_OPERATION_ROLE`]）。
async fn collect_start_handler(
    State(state): State<CollectState>,
    headers: HeaderMap,
) -> Result<Json<CollectOutcome>, ApiError> {
    let outcome = state.collect.start().await?;
    record_collect_operation(&state, &headers, "start", &outcome).await;
    Ok(Json(outcome))
}

/// `POST /api/collect/stop`（**`editor` 以上** - [`COLLECT_OPERATION_ROLE`]）。
async fn collect_stop_handler(
    State(state): State<CollectState>,
    headers: HeaderMap,
) -> Result<Json<CollectOutcome>, ApiError> {
    let outcome = state.collect.stop().await?;
    record_collect_operation(&state, &headers, "stop", &outcome).await;
    Ok(Json(outcome))
}

/// `POST /api/collect/restart`（**`editor` 以上** - [`COLLECT_OPERATION_ROLE`]）: **レジストリの変更を収集へ
/// 反映する唯一の口**。接続・グループ・タグの CRUD が自動でこれを呼ぶことは
/// しない（理由は `crate::collect` のモジュール doc「起動時の自動開始と、
/// 「収集を再起動」だけが反映の口であること」）。
async fn collect_restart_handler(
    State(state): State<CollectState>,
    headers: HeaderMap,
) -> Result<Json<CollectOutcome>, ApiError> {
    let outcome = state.collect.restart().await?;
    record_collect_operation(&state, &headers, "restart", &outcome).await;
    Ok(Json(outcome))
}

/// `/api/collect*`（#383 段階2b / R1-C）: **読み取りは `viewer` 以上・変更は
/// `editor` 以上**の 2 段（`require_auth` → ルート群ごとの
/// `require_role_at_least`）。床の値は両方とも `crate::collect` の定数
/// （[`COLLECT_READ_ROLE`] / [`COLLECT_OPERATION_ROLE`]）から来ていて、
/// `src-tauri` の `collect_*` コマンドが**同じ定数**を参照する - 経路によって
/// 床が割れようがないようにするため（[`COLLECT_AUDIT_RESOURCE`] と同じ作法）。
///
/// **なぜ変更が `editor` であって `admin` ではないか**:
/// docs/recorder-requirements.md §3.6「収集の開始/停止は editor 以上」と、
/// そこに追記された **2026-09-18 のオーナー決定** - 「banto-hub は同種の制御
/// （自動書き込みエンジンの arm/disarm 等）を admin 限定にしているが、
/// chronogazer は別プロダクトであり R0 本文のこの決定を継承する。**banto-hub の
/// 先例に引きずられて admin 限定へ変更しない**」。
///
/// **なぜ読み取りが `viewer` なのか**（2026-09-20 オーナー決定、#407 レビュー。
/// 当初は router 全体を `editor` にしていた）: R0 §3.6 の viewer は
/// 「**閲覧のみ**」であって「何も見えない」ではない。収集が動いているかどうかは
/// 監視画面（C-3 / R1-D）の基本情報で、公開する [`CollectorStateView`] には
/// 機微な情報が何も入っていない（接続先もキーもファイルパスも無い。**内部の
/// `CollectorState` をそのまま載せないのはこのため** - #407 レビュー P2-2）
/// ので、viewer に見せて困るものが無い。すぐ上の [`hub_router`] が
/// 読み取りまで `admin` なのは
/// **Hub の接続先とキーの情報**を扱うからで、**収集の稼働状態はそれとは性質が
/// 違う** - 先例に引きずられない、という上と同じ考え方。
///
/// **床は分けても `resource` は分けない**: 読み取りの拒否も変更の拒否も
/// [`COLLECT_AUDIT_RESOURCE`]（`"collect"`）で記録する。変更側は
/// **3 ルートまとめて 1 つの `RoleGuard`** に掛けてあるので、「変更操作は
/// editor 以上」は 1 か所で決まる（次に操作を足しても床が散らない）。
fn collect_router(collect: CollectorService, audit: AuditLogService, auth: AuthState) -> Router {
    let state = CollectState {
        collect,
        audit: audit.clone(),
        auth: auth.clone(),
    };
    // 読み取りの床（`viewer` 以上）。**4 ルートまとめて 1 つの `RoleGuard`**
    // で、ここが「読み取りは誰まで」の唯一の決まり場所（変更側と同じ作法 -
    // 次に読み出しを足しても床が散らない）。
    let reads = Router::new()
        .route("/api/collect", get(collect_status_handler))
        // C-3a の 3 つの読み出し口。状態と同じ床・同じ `resource` で、
        // どれも監査しない（読み取りは監査しない、という全体の規約）。
        .route("/api/collect/values", get(collect_values_handler))
        .route("/api/collect/connections", get(collect_connections_handler))
        .route("/api/collect/events", get(collect_events_handler))
        .with_state(state.clone())
        .layer(middleware::from_fn_with_state(
            RoleGuard {
                auth: auth.clone(),
                min: COLLECT_READ_ROLE,
                resource: COLLECT_AUDIT_RESOURCE,
                audit: audit.clone(),
            },
            require_role_at_least,
        ));
    // 変更操作の床（`editor` 以上）。**3 ルートまとめて 1 つの
    // `RoleGuard`** に掛ける - 床がルートごとに散ると、次に足した操作が
    // 黙って別の床になる。
    let writes = Router::new()
        .route("/api/collect/start", post(collect_start_handler))
        .route("/api/collect/stop", post(collect_stop_handler))
        .route("/api/collect/restart", post(collect_restart_handler))
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            RoleGuard {
                auth: auth.clone(),
                min: COLLECT_OPERATION_ROLE,
                resource: COLLECT_AUDIT_RESOURCE,
                audit,
            },
            require_role_at_least,
        ));
    reads
        .merge(writes)
        .layer(middleware::from_fn_with_state(auth, require_auth))
}

// --- #383 段階2a / R1-B: レジストリ CRUD（PLC接続・収集グループ・タグ） ----
//
// banto-tags の3サービス（`PlcConnectionService`/`CollectionGroupService`/
// `TagService`）を REST に配線する。手本は relay-wright の同名セクション
// （`apps/relay-wright/core/src/rest.rs`）- viewer-read/editor-write
// （R0 §3.6）を [`require_editor`] で掛け、成功した書き込みだけ
// [`record_write`] で監査する（`origin: "rest"`）。
//
// relay-wright と違い、このアプリはカスケード削除・`/describe`・`/test`・
// 式チェック・バッチ操作を持たない（#383 段階2a のスコープ外 - 指示書
// 「やらないこと」）。バニラな CRUD だけ。banto-tags 自身の削除ガード
// （「まだ N 件のグループ/タグから参照されています」）がそのまま
// `BantoError::Validation` として返る。

fn default_payload_enabled() -> bool {
    true
}

fn default_plc_protocol() -> String {
    MODBUS_PROTOCOL.to_string()
}

fn default_plc_unit_id() -> i64 {
    1
}

fn default_tag_decimals() -> i64 {
    0
}

/// chronogazer は PLC 直結アプリ（recorder-requirements.md §1「対象環境」）
/// で、`banto_tags::ALLOWED_PROTOCOLS` の4つのうち `"virtual"`（演算/内部
/// タグの置き場、banto-hub が calc/mem を自動発行する仕組み）と
/// `"postgres"`（DB Source/Sink、banto-hub 固有）はどちらも banto-hub の
/// 機能で、chronogazer には対応する画面もタグ種別も無い。指示書の指定
/// どおり `"modbus-tcp"`/`"slmp"` だけを受け付ける - relay-wright の
/// `reject_postgres_connection_protocol`（postgres だけを拒否）より一段
/// 狭い許可リスト方式にしているのは、chronogazer が「PLC 直結」以外の
/// 用途を一切持たないため（将来 `ALLOWED_PROTOCOLS` が5つ目を増やしても、
/// ここで拒否されるのが安全側のデフォルト）。REST の create/update ハンドラ
/// と、双方向対称の Tauri コマンド（`apps/chronogazer/src-tauri/src/lib.rs`
/// の `plc_connections_create`/`plc_connections_update`）の両方から呼ぶ -
/// 片方だけに書くともう片方の経路から通ってしまう。
fn reject_disallowed_connection_protocol(protocol: &str) -> Result<(), BantoError> {
    const ALLOWED: [&str; 2] = [MODBUS_PROTOCOL, "slmp"];
    if ALLOWED.contains(&protocol) {
        return Ok(());
    }
    Err(BantoError::Validation {
        field_errors: vec![FieldError {
            field: "protocol".to_string(),
            message: format!(
                "chronogazer が対応するプロトコルは {} のいずれかです",
                ALLOWED.join(", ")
            ),
        }],
    })
}

/// Wire-shaped (camelCase) create/update payload for `plc_connections`.
///
/// R1-B 指示書（#383 段階2a）どおり、banto-tags/relay-wright/banto-hub の
/// `PlcConnectionInput`/`PlcConnectionPayload` から chronogazer に不要な
/// フィールドを落とした最小形: `simulation`（接続単位シミュレーションは
/// banto-hub 固有機能）と `database`/`username`/`password`（`"postgres"`
/// 専用列 - [`reject_disallowed_connection_protocol`] が postgres 接続の
/// 作成自体を拒否するので、この3列を持たせても常に空にしかならない）は
/// ワイヤに出さない。`word_order` は残す（指示書の残す一覧に明記）。
#[derive(Debug, Clone, Deserialize)]
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
    /// `""` = 未指定（プロトコルの既定に従う） - `PlcConnectionInput::
    /// word_order`のドキュメント参照。素通しするだけで chronogazer 側の
    /// 追加ロジックは無い。
    #[serde(default)]
    pub word_order: String,
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
            // R1-B: `simulation` は banto-hub 固有機能（接続単位シミュ
            // レーション切り替え）。chronogazer は一切設定しない - 常に
            // 列の既定値（`false`）のまま。
            simulation: false,
            word_order: payload.word_order,
            // postgres 専用列。[`reject_disallowed_connection_protocol`]
            // が postgres 接続の作成を拒否するので常に `None` でよい。
            database: None,
            username: None,
            password: None,
        }
    }
}

/// `plc_connections` の GET/list/create/update が返す読み取り DTO。
/// `banto_tags::PlcConnection` をそのまま `Serialize` すると `password`
/// 列（`banto_hub`/relay-wright と共有する平文カラム、
/// `PlcConnection::password`の doc comment「外部呼び出し元へ決して
/// シリアライズしない」）がそのまま応答に出てしまう。chronogazer は
/// postgres 接続を作成できない（[`reject_disallowed_connection_protocol`]）
/// ので通常運用でこの列が非空になることは無いはずだが、防御的に - 何らか
/// の経路（データベースファイルの直接操作等）で非空の `password` を持つ行
/// が紛れ込んでいても外へ出さない。`simulation`/`database`/`username` も
/// 同じ理由（chronogazer は一切書かない列）で応答から省く - relay-wright
/// の同名 DTO と異なり、これらの列を「常に既定値のまま」であることを
/// 応答形から見ても分かるようにしている。
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
    pub word_order: String,
}

impl From<PlcConnection> for PlcConnectionResponse {
    fn from(conn: PlcConnection) -> Self {
        Self {
            id: conn.id,
            name: conn.name,
            protocol: conn.protocol,
            host: conn.host,
            port: conn.port,
            unit_id: conn.unit_id,
            enabled: conn.enabled,
            word_order: conn.word_order,
        }
    }
}

/// Wire-shaped (camelCase) create/update payload for `collection_groups`.
/// `default_writable`/`query_sql`（S2 DB Source、banto-hub 固有）は
/// [`From`] impl 側で列の既定値に固定する - chronogazer には対応する UI が
/// 無い。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionGroupPayload {
    pub name: String,
    pub plc_connection_id: i64,
    pub period_ms: i64,
    #[serde(default = "default_payload_enabled")]
    pub enabled: bool,
}

impl From<CollectionGroupPayload> for CollectionGroupInput {
    fn from(payload: CollectionGroupPayload) -> Self {
        Self {
            name: payload.name,
            plc_connection_id: payload.plc_connection_id,
            period_ms: payload.period_ms,
            enabled: payload.enabled,
            // banto-hub 固有（新規タグ登録フォームの writable チェック
            // ボックス初期値）。chronogazer に対応 UI は無い - 列の既定値
            // （`true`）のまま。
            default_writable: true,
            // S2 DB Source（banto-hub 固有）。chronogazer は postgres 接続
            // を作成できないので常に `None`。
            query_sql: None,
        }
    }
}

/// Wire-shaped (camelCase) create/update payload for `tags`。
///
/// R1-B 指示書の「残すもの」（名前/接続/デバイスアドレス/データ型/
/// スケーリング/単位/小数桁）に厳密に合わせた最小形。しきい値
/// （`thresholdH`/`Hh`/`L`/`Ll`）と文字列タグ（`stringLength`/
/// `stringEncoding`）は指示書の残す一覧に無い - recorder-requirements.md
/// §6 は「タグ設定」画面（本 PR）と「グループ設定（ペン割当・表示種別・
/// しきい値）」画面を別画面として挙げており、しきい値の編集は後者
/// （表示グループ、R1-B の範囲外・#383 の後続段階）の役目と読める。
/// `writable`/`tagKind`/`expression`/`retain` は指示書が明示的に落とす
/// もの（演算タグ・書き込みは banto-hub 固有 / R0 §7 非スコープ）。
/// [`From`] impl 側でこれらを `banto_tags::TagInput` の既定値に固定する。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TagPayload {
    pub name: String,
    pub collection_group_id: i64,
    pub address: String,
    pub data_type: String,
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
    #[serde(default = "default_payload_enabled")]
    pub enabled: bool,
}

impl From<TagPayload> for TagInput {
    fn from(payload: TagPayload) -> Self {
        Self {
            name: payload.name,
            collection_group_id: payload.collection_group_id,
            address: payload.address,
            data_type: payload.data_type,
            // 文字列タグ（"string"）は本 PR のスコープ外（この payload の
            // doc comment参照） - chronogazer は常に非文字列タグとして
            // 列の既定値を渡す。`data_type == "string"` を誰かが直接送って
            // きた場合は banto-tags 側の検証（string_length 必須）が人間
            // 可読なエラーで弾く。
            string_length: None,
            string_encoding: "utf8".to_string(),
            raw_lo: payload.raw_lo,
            raw_hi: payload.raw_hi,
            eng_lo: payload.eng_lo,
            eng_hi: payload.eng_hi,
            unit: payload.unit,
            decimals: payload.decimals,
            // しきい値は本 PR のスコープ外（この payload の doc comment
            // 参照）。
            threshold_h: None,
            threshold_hh: None,
            threshold_l: None,
            threshold_ll: None,
            enabled: payload.enabled,
            writable: false,
            tag_kind: "plc".to_string(),
            expression: None,
            retain: false,
            expected_revision: None,
        }
    }
}

/// State for the `/api/plc-connections/*`, `/api/collection-groups/*` and
/// `/api/tags/*` handlers: banto-tags の3サービスと、書き込みを editor
/// ゲート（[`require_editor`]）・監査するための `AuthState`/
/// `AuditLogService`。
#[derive(Clone)]
struct TagRegistryState {
    plc_connections: PlcConnectionService,
    collection_groups: CollectionGroupService,
    tags: TagService,
    auth: AuthState,
    audit: AuditLogService,
}

async fn plc_connections_list(
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

async fn plc_connections_get(
    State(state): State<TagRegistryState>,
    Path(id): Path<i64>,
) -> Result<Json<PlcConnectionResponse>, ApiError> {
    Ok(Json(PlcConnectionResponse::from(
        state.plc_connections.get(id).await?,
    )))
}

async fn plc_connections_create(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Json(input): Json<PlcConnectionPayload>,
) -> Result<Json<PlcConnectionResponse>, ApiError> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "plc_connections",
        "POST",
        "/api/plc-connections",
    )
    .await?;
    reject_disallowed_connection_protocol(&input.protocol)?;
    let created = state.plc_connections.create(input.into()).await?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "create",
        "plc_connections",
        &created.id.to_string(),
        Some(json!({ "name": created.name, "enabled": created.enabled })),
    )
    .await;
    Ok(Json(PlcConnectionResponse::from(created)))
}

async fn plc_connections_update(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(input): Json<PlcConnectionPayload>,
) -> Result<Json<PlcConnectionResponse>, ApiError> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "plc_connections",
        "PUT",
        "/api/plc-connections/{id}",
    )
    .await?;
    reject_disallowed_connection_protocol(&input.protocol)?;
    let updated = state.plc_connections.update(id, input.into()).await?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "update",
        "plc_connections",
        &id.to_string(),
        Some(json!({ "name": updated.name, "enabled": updated.enabled })),
    )
    .await;
    Ok(Json(PlcConnectionResponse::from(updated)))
}

async fn plc_connections_delete(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "plc_connections",
        "DELETE",
        "/api/plc-connections/{id}",
    )
    .await?;
    state.plc_connections.delete(id).await?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "delete",
        "plc_connections",
        &id.to_string(),
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn collection_groups_list(
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

async fn collection_groups_get(
    State(state): State<TagRegistryState>,
    Path(id): Path<i64>,
) -> Result<Json<CollectionGroup>, ApiError> {
    Ok(Json(state.collection_groups.get(id).await?))
}

async fn collection_groups_create(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Json(input): Json<CollectionGroupPayload>,
) -> Result<Json<CollectionGroup>, ApiError> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "collection_groups",
        "POST",
        "/api/collection-groups",
    )
    .await?;
    let created = state.collection_groups.create(input.into()).await?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "create",
        "collection_groups",
        &created.id.to_string(),
        Some(json!({ "name": created.name, "enabled": created.enabled })),
    )
    .await;
    Ok(Json(created))
}

async fn collection_groups_update(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(input): Json<CollectionGroupPayload>,
) -> Result<Json<CollectionGroup>, ApiError> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "collection_groups",
        "PUT",
        "/api/collection-groups/{id}",
    )
    .await?;
    let updated = state.collection_groups.update(id, input.into()).await?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "update",
        "collection_groups",
        &id.to_string(),
        Some(json!({ "name": updated.name, "enabled": updated.enabled })),
    )
    .await;
    Ok(Json(updated))
}

async fn collection_groups_delete(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "collection_groups",
        "DELETE",
        "/api/collection-groups/{id}",
    )
    .await?;
    state.collection_groups.delete(id).await?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "delete",
        "collection_groups",
        &id.to_string(),
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn tags_list(State(state): State<TagRegistryState>) -> Result<Json<Vec<Tag>>, ApiError> {
    Ok(Json(state.tags.list(ListParams::default()).await?.rows))
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
) -> Result<Json<Tag>, ApiError> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "tags",
        "POST",
        "/api/tags",
    )
    .await?;
    let created = state.tags.create(input.into()).await?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "create",
        "tags",
        &created.id.to_string(),
        Some(json!({ "name": created.name, "enabled": created.enabled })),
    )
    .await;
    Ok(Json(created))
}

async fn tags_update(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(input): Json<TagPayload>,
) -> Result<Json<Tag>, ApiError> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "tags",
        "PUT",
        "/api/tags/{id}",
    )
    .await?;
    let updated = state.tags.update(id, input.into()).await?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "update",
        "tags",
        &id.to_string(),
        Some(json!({ "name": updated.name, "enabled": updated.enabled })),
    )
    .await;
    Ok(Json(updated))
}

async fn tags_delete(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "tags",
        "DELETE",
        "/api/tags/{id}",
    )
    .await?;
    state.tags.delete(id).await?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "delete",
        "tags",
        &id.to_string(),
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// `/api/plc-connections/*` + `/api/collection-groups/*` + `/api/tags/*`
/// (R0 §3.6): viewer-read / editor-write, exactly the same floor split
/// (router-wide `require_auth` for reads, inline [`require_editor`] per
/// write handler) as `users_router`/`hub_router` use `RoleGuard` for. The
/// services are banto-tags' own - all input validation and delete guards
/// ("まだ N 件の...から参照されています") live there, shared verbatim with
/// the Tauri commands (`plc_connections_*`/`collection_groups_*`/`tags_*`
/// in `src-tauri`) - invariant: both transport paths behave identically.
fn tag_registry_router(
    plc_connections: PlcConnectionService,
    collection_groups: CollectionGroupService,
    tags: TagService,
    audit: AuditLogService,
    auth: AuthState,
) -> Router {
    let state = TagRegistryState {
        plc_connections,
        collection_groups,
        tags,
        auth: auth.clone(),
        audit,
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
        .route("/api/tags", get(tags_list).post(tags_create))
        .route(
            "/api/tags/{id}",
            get(tags_get).put(tags_update).delete(tags_delete),
        )
        .with_state(state)
        .layer(middleware::from_fn_with_state(auth, require_auth))
}

/// Compose the full `/api/*` router (spec §11.1): auth routes (login/
/// logout/check/identity from `banto_server` - wrapped with an audit-log
/// hook for `logout`, spec M14 - plus status/setup/change-password here
/// since those need `UsersService`), SSE events, the `admin`-only `users`
/// management routes (spec M10), the `admin`-only `audit-log` viewer (spec
/// M14), the `admin`-only `backups` routes (spec M17), and the per-user
/// `ui-settings` routes (spec M12), all behind the CSRF header check. Mount
/// the result *before* `banto_server::static_files::static_router` so
/// `/api/*` takes priority over the SPA fallback. [`tag_registry_router`]
/// (#383 段階2a / R1-B: PLC connections/collection groups/tags) is the first
/// resource wired up this way; a future 表示グループ (display group)
/// resource would get its own RBAC-split read/write router merged in here
/// the same way. [`collect_router`] (#383 段階2b / R1-C: 収集の開始・停止・
/// 再起動) is `editor`-floored router-wide, like `hub_router` is
/// `admin`-floored.
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
    // #332: Hub 接続。`KeyStore` の実装ごと呼び出し元が構築して渡す
    // （デスクトップ = OS キーリング、`banto-serve` =
    // `crate::hub::UnavailableKeyStore`）。`HubService::new` が非同期
    // （`installation_id` の読み書き）なのでここでは構築できない。
    hub: HubService,
    // #383 段階2a / R1-B: banto-tags の3サービス。呼び出し元
    // （`bin/banto-serve.rs`/`src-tauri`）が `*Service::new(pool.clone())`
    // で構築して渡す - このモジュールは `DbPool` を直接持たない。
    plc_connections: PlcConnectionService,
    collection_groups: CollectionGroupService,
    tags: TagService,
    // #383 段階2b / R1-C（C-2）: 収集ランタイム。呼び出し元
    // （`bin/banto-serve.rs`/`src-tauri`）が `CollectorService::new` で構築
    // して渡す（`data.dir` の解決基準が呼び出し元ごとに違うため -
    // `crate::collect::resolve_data_dir` 参照）。デスクトップとこの LAN
    // サーバーが**同じ実体**を共有するのは `hub` と同じ理由。
    collect: CollectorService,
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
        .merge(backups_router(backup, audit.clone(), auth.clone()))
        .merge(hub_router(hub, audit.clone(), auth.clone()))
        .merge(collect_router(collect, audit.clone(), auth.clone()))
        .merge(tag_registry_router(
            plc_connections,
            collection_groups,
            tags,
            audit,
            auth.clone(),
        ))
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
            PathBuf::from("unused-in-tests").join("chronogazer.sqlite3"),
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
        let (plc_connections, collection_groups, tags) = tag_registry_services(pool.clone());
        let collect = test_collector_service(pool.clone());
        let audit = AuditLogService::new(pool);

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
        let hub = test_hub_service(settings.clone()).await;
        (
            api_router(
                users,
                settings,
                audit,
                backup,
                hub,
                plc_connections,
                collection_groups,
                tags,
                collect,
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
        let (plc_connections, collection_groups, tags) = tag_registry_services(pool.clone());
        let collect = test_collector_service(pool.clone());
        let audit = AuditLogService::new(pool);
        let auth = demo_auth();
        let token = auth
            .login("admin", "admin")
            .await
            .expect("login should succeed");
        let hub = test_hub_service(settings.clone()).await;
        (
            api_router(
                users,
                settings,
                audit,
                backup,
                hub,
                plc_connections,
                collection_groups,
                tags,
                collect,
                auth,
                tx,
                false,
            ),
            token,
        )
    }

    /// #332: a `HubService` for router tests. Backed by
    /// [`crate::hub::UnavailableKeyStore`], so it can never write a
    /// plaintext key anywhere during a test - these tests only exercise the
    /// `/api/hub/*` routes' auth/role gating and the "not configured"
    /// answer, never a real Hub handshake (that is the `banto-hub-bootstrap`
    /// crate's own mock-server suite).
    async fn test_hub_service(settings: SettingsService) -> crate::hub::HubService {
        crate::hub::HubService::new(
            settings,
            std::sync::Arc::new(crate::hub::UnavailableKeyStore),
        )
        .await
        .expect("HubService::new")
    }

    /// #383 段階2a / R1-B: the three banto-tags registry services, for
    /// `api_router`'s new parameters - every router-building test helper
    /// below needs these, so factored out rather than repeated at each call
    /// site (mirrors `test_hub_service` just above).
    fn tag_registry_services(
        pool: sqlx::SqlitePool,
    ) -> (PlcConnectionService, CollectionGroupService, TagService) {
        (
            PlcConnectionService::new(pool.clone()),
            CollectionGroupService::new(pool.clone()),
            TagService::new(pool),
        )
    }

    /// #383 段階2b / R1-C（C-2）: [`api_router`] の新しい引数用の収集サービス。
    ///
    /// `data_dir` が [`unused_backup_service`] と同じプレースホルダなのは、
    /// **どのルーターテストも実際にファイルを作らない**から: これらのテストが
    /// 見るのは `/api/collect*` の認可・監査・ワイヤ形だけで、収集を本当に
    /// 走らせる（= `TsWriter` を開く）のは収集対象が 1 件以上ある構成での
    /// `start` だけ。レジストリが空のテスト DB では
    /// `crate::collect::CollectorState::NoTargets` で止まるので、この
    /// ディレクトリは一度も触られない。
    fn test_collector_service(pool: sqlx::SqlitePool) -> CollectorService {
        CollectorService::new(pool, PathBuf::from("unused-in-tests"))
    }

    async fn body_json(response: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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
        let (plc_connections, collection_groups, tags) = tag_registry_services(pool.clone());
        let collect = test_collector_service(pool.clone());
        let audit = AuditLogService::new(pool);
        let auth = demo_auth();
        let hub = test_hub_service(settings.clone()).await;
        api_router(
            users,
            settings,
            audit,
            backup,
            hub,
            plc_connections,
            collection_groups,
            tags,
            collect,
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn auth_status_reports_uninitialized_before_any_setup() {
        let router = router_with_setup(true).await;
        let response = router.oneshot(get("/api/auth/status")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response).await;
        assert_eq!(json["initialized"], false);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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
    /// `/api/auth/change-password` - mirrors how `banto-serve`/`src-tauri`
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
        let (plc_connections, collection_groups, tags) = tag_registry_services(pool.clone());
        let collect = test_collector_service(pool.clone());
        let audit = AuditLogService::new(pool);
        let auth = AuthState::new(audited_credential_verifier(users.clone(), audit.clone()));
        let hub = test_hub_service(settings.clone()).await;
        (
            api_router(
                users,
                settings,
                audit.clone(),
                backup,
                hub,
                plc_connections,
                collection_groups,
                tags,
                collect,
                auth,
                tx,
                allow_setup,
            ),
            audit,
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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

    /// #332: `/api/hub/*` は `admin` 限定（`users_router` と同じ掛け方）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn hub_routes_are_admin_only() {
        let (router, admin, editor, viewer) = router_with_role_tokens().await;

        for token in [&editor, &viewer] {
            let response = router
                .clone()
                .oneshot(get_auth("/api/hub", token))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
            let response = router
                .clone()
                .oneshot(post_json_auth(
                    "/api/hub/connect",
                    token,
                    json!({ "endpoint": "http://127.0.0.1:1" }),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }

        // #383 段階1: 購読状態のポーリング口も同じ admin 限定。
        for token in [&editor, &viewer] {
            let response = router
                .clone()
                .oneshot(get_auth("/api/hub/subscription", token))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }

        let response = router
            .clone()
            .oneshot(get_auth("/api/hub", &admin))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    /// #383 段階1: 購読状態は未設定でも 200 で「停止（理由付き）」を返す。
    /// 接続の 6 状態とは別軸なので、Hub が無くても例外にならない。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn hub_subscription_is_stopped_with_a_reason_before_any_connect() {
        let (router, admin, _editor, _viewer) = router_with_role_tokens().await;

        let response = router
            .oneshot(get_auth("/api/hub/subscription", &admin))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["state"], json!("stopped"));
        assert_eq!(body["subscribedCount"], json!(0));
        assert_eq!(body["unresolved"], json!([]));
        assert_eq!(body["unsupported"], json!([]));
        assert_eq!(body["values"], json!([]));
        assert!(
            body["reason"].as_str().is_some_and(|s| !s.is_empty()),
            "停止している理由を必ず出す: {body}"
        );
    }

    /// #332: 何も設定していないうちは `notConfigured`。`tags` は空配列では
    /// なく `null`（「読めていない」と「タグ 0 件で接続済み」を取り違え
    /// させないため - 受入条件）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn hub_status_is_not_configured_before_any_connect() {
        let (router, admin, _editor, _viewer) = router_with_role_tokens().await;

        let response = router
            .clone()
            .oneshot(get_auth("/api/hub", &admin))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["status"]["state"], json!("notConfigured"));
        assert_eq!(body["tags"], json!(null));
        assert_eq!(body["selectedTags"], json!([]));
    }

    /// #332: 接続先の形式エラーはフィールド検証として返す（画面が
    /// `endpoint` 欄にマッピングできるように）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn hub_connect_rejects_a_non_http_endpoint_as_a_field_error() {
        let (router, admin, _editor, _viewer) = router_with_role_tokens().await;

        let response = router
            .oneshot(post_json_auth(
                "/api/hub/connect",
                &admin,
                json!({ "endpoint": "https://example.test" }),
            ))
            .await
            .unwrap();
        assert!(response.status().is_client_error());
        let body = body_json(response).await;
        assert_eq!(body["kind"], json!("validation"));
        assert_eq!(body["field_errors"][0]["field"], json!("endpoint"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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
        let (router, audit, _pool, admin, editor, viewer) =
            router_with_role_tokens_audit_and_pool().await;
        (router, audit, admin, editor, viewer)
    }

    /// [`router_with_role_tokens_and_audit`] と同じものを組むが、**router と
    /// 同じプールも返す**（#383 段階2b / R1-C の C-3a）: `collect_events` の
    /// ような「REST からは書けないが読める」表に、テストが直接 1 行入れる
    /// ため（あの表へ書くのは収集エンジンだけで、書き込み API は無い）。
    async fn router_with_role_tokens_audit_and_pool() -> (
        Router,
        AuditLogService,
        sqlx::SqlitePool,
        String,
        String,
        String,
    ) {
        let pool = migrate_memory().await.expect("migrate_memory");
        let pool_for_tests = pool.clone();
        let (tx, _rx) = broadcast::channel(16);
        let users = UsersService::new(pool.clone());
        let settings = SettingsService::new(pool.clone());
        let backup = unused_backup_service(pool.clone());
        let (plc_connections, collection_groups, tags) = tag_registry_services(pool.clone());
        let collect = test_collector_service(pool.clone());
        let audit = AuditLogService::new(pool);

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

        let hub = test_hub_service(settings.clone()).await;
        let router = api_router(
            users,
            settings,
            audit.clone(),
            backup,
            hub,
            plc_connections,
            collection_groups,
            tags,
            collect,
            auth,
            tx,
            false,
        );
        (
            router,
            audit,
            pool_for_tests,
            admin_token,
            editor_token,
            viewer_token,
        )
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
    /// - The returned `crate::test_support::TempDir` guard must be kept
    ///   alive by the caller for as long as the router is in use - dropping
    ///   it deletes the directory `backups/`/`restore-pending.sqlite3` live
    ///   in.
    ///
    /// Returns `(dir, router, admin, editor, viewer)` - **dir first, router
    /// second** - not the more natural-looking `(router, dir, ...)`. See
    /// `crate::test_support`'s module doc for why: every call site
    /// destructures this directly, and a tuple's bindings from one `let`
    /// pattern drop in *reverse* of how they're listed - `router` (which
    /// holds the pool via its cloned service state) must be listed AFTER
    /// `dir` so it drops before `dir`'s cleanup runs. Getting this order
    /// backwards silently leaks the temp dir on every run (measured before
    /// this fix).
    async fn router_with_role_tokens_and_backup(
    ) -> (crate::test_support::TempDir, Router, String, String, String) {
        let dir = crate::test_support::TempDir::new();
        let db_path = dir.path().join("chronogazer.sqlite3");
        // `crate::db::init_db` (not a raw `sqlx::migrate!` call here) - see
        // that module's doc comment for why this app's own schema is NOT
        // applied via `sqlx::migrate!` (it would collide with
        // `banto_tags::migrate`'s own bookkeeping on this same pool).
        let pool = crate::db::init_db(&db_path).await.expect("init_db");

        let (tx, _rx) = broadcast::channel(16);
        let users = UsersService::new(pool.clone());
        let settings = SettingsService::new(pool.clone());
        let backup = BackupService::new(db_path, pool.clone());
        let (plc_connections, collection_groups, tags) = tag_registry_services(pool.clone());
        let collect = test_collector_service(pool.clone());
        let audit = AuditLogService::new(pool);

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

        let hub = test_hub_service(settings.clone()).await;
        let router = api_router(
            users,
            settings,
            audit,
            backup,
            hub,
            plc_connections,
            collection_groups,
            tags,
            collect,
            auth,
            tx,
            false,
        );
        (dir, router, admin_token, editor_token, viewer_token)
    }

    /// (a) `/api/audit-log/list` is admin-only: 200 for admin, 403 for
    /// editor/viewer.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn admin_can_create_list_and_download_backups() {
        let (_dir, router, admin, _editor, _viewer) = router_with_role_tokens_and_backup().await;

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
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn editor_and_viewer_cannot_access_backups_routes() {
        let (_dir, router, _admin, editor, viewer) = router_with_role_tokens_and_backup().await;

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
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn restore_upload_of_garbage_bytes_is_rejected_as_validation() {
        let (_dir, router, admin, _editor, _viewer) = router_with_role_tokens_and_backup().await;

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
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stage_restore_from_existing_backup_then_cancel_is_recorded_in_the_audit_log() {
        let (_dir, router, admin, _editor, _viewer) = router_with_role_tokens_and_backup().await;

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

    // --- #383 段階2a / R1-B: レジストリ CRUD ---------------------------------

    fn plc_connection_payload(name: &str) -> serde_json::Value {
        json!({
            "name": name,
            "protocol": "modbus-tcp",
            "host": "192.168.11.200",
            "port": 502,
            "unitId": 1,
            "enabled": true
        })
    }

    /// R0 §3.6: 読み取りは viewer 以上、書き込みは editor 以上 - 3エンティティ
    /// すべて（`plc_connections`/`collection_groups`/`tags` は FK で連なる
    /// ので、この1テストで PLC接続→収集グループ→タグの順に作成する）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tag_registry_write_routes_require_editor_viewer_can_only_read() {
        let (router, _admin, editor, viewer) = router_with_role_tokens().await;

        // viewer: GET は通る、POST は 403 / forbidden kind。
        let viewer_list = router
            .clone()
            .oneshot(get_auth("/api/plc-connections", &viewer))
            .await
            .unwrap();
        assert_eq!(viewer_list.status(), StatusCode::OK);

        let viewer_create = router
            .clone()
            .oneshot(post_json_auth(
                "/api/plc-connections",
                &viewer,
                plc_connection_payload("viewer-should-not-create"),
            ))
            .await
            .unwrap();
        assert_eq!(viewer_create.status(), StatusCode::FORBIDDEN);
        assert_eq!(body_json(viewer_create).await["kind"], "forbidden");

        // editor: PLC接続を作成できる。
        let create_conn = router
            .clone()
            .oneshot(post_json_auth(
                "/api/plc-connections",
                &editor,
                plc_connection_payload("plc1"),
            ))
            .await
            .unwrap();
        assert_eq!(create_conn.status(), StatusCode::OK);
        let conn = body_json(create_conn).await;
        assert_eq!(conn["name"], "plc1");
        assert_eq!(conn["wordOrder"], "high_low"); // #325: modbus-tcp の既定
        let conn_id = conn["id"].as_i64().unwrap();

        // editor: そのPLC接続配下に収集グループを作成できる。
        let create_group = router
            .clone()
            .oneshot(post_json_auth(
                "/api/collection-groups",
                &editor,
                json!({
                    "name": "group1",
                    "plcConnectionId": conn_id,
                    "periodMs": 1000,
                    "enabled": true
                }),
            ))
            .await
            .unwrap();
        assert_eq!(create_group.status(), StatusCode::OK);
        let group = body_json(create_group).await;
        let group_id = group["id"].as_i64().unwrap();

        // viewer: グループ作成は 403。
        let viewer_group = router
            .clone()
            .oneshot(post_json_auth(
                "/api/collection-groups",
                &viewer,
                json!({
                    "name": "viewer-group",
                    "plcConnectionId": conn_id,
                    "periodMs": 1000,
                    "enabled": true
                }),
            ))
            .await
            .unwrap();
        assert_eq!(viewer_group.status(), StatusCode::FORBIDDEN);

        // editor: そのグループ配下にタグを作成できる。
        let create_tag = router
            .clone()
            .oneshot(post_json_auth(
                "/api/tags",
                &editor,
                json!({
                    "name": "tag1",
                    "collectionGroupId": group_id,
                    "address": "D3000",
                    "dataType": "i16",
                    "decimals": 0,
                    "enabled": true
                }),
            ))
            .await
            .unwrap();
        assert_eq!(create_tag.status(), StatusCode::OK);
        let tag = body_json(create_tag).await;
        assert_eq!(tag["name"], "tag1");
        let tag_id = tag["id"].as_i64().unwrap();

        // viewer: タグ作成・削除は 403、一覧は見える。
        let viewer_tag_list = router
            .clone()
            .oneshot(get_auth("/api/tags", &viewer))
            .await
            .unwrap();
        assert_eq!(viewer_tag_list.status(), StatusCode::OK);
        let viewer_tag_delete = router
            .clone()
            .oneshot(delete_auth(&format!("/api/tags/{tag_id}"), &viewer))
            .await
            .unwrap();
        assert_eq!(viewer_tag_delete.status(), StatusCode::FORBIDDEN);

        // editor: 作った順の逆（タグ→グループ→接続）で削除できる
        // （banto-tags 自身の参照ガード - 子が残っていると親は消せない）。
        let delete_tag = router
            .clone()
            .oneshot(delete_auth(&format!("/api/tags/{tag_id}"), &editor))
            .await
            .unwrap();
        assert_eq!(delete_tag.status(), StatusCode::NO_CONTENT);
        let delete_group = router
            .clone()
            .oneshot(delete_auth(
                &format!("/api/collection-groups/{group_id}"),
                &editor,
            ))
            .await
            .unwrap();
        assert_eq!(delete_group.status(), StatusCode::NO_CONTENT);
        let delete_conn = router
            .oneshot(delete_auth(
                &format!("/api/plc-connections/{conn_id}"),
                &editor,
            ))
            .await
            .unwrap();
        assert_eq!(delete_conn.status(), StatusCode::NO_CONTENT);
    }

    /// [`reject_disallowed_connection_protocol`]: chronogazer は
    /// `"virtual"`/`"postgres"` 接続の作成を拒否する（banto-hub 固有の
    /// プロトコル）。人間可読な `field_errors` として返ることを固定する。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn plc_connection_create_rejects_virtual_and_postgres_protocol_readably() {
        let (router, _admin, editor, _viewer) = router_with_role_tokens().await;

        for protocol in ["virtual", "postgres"] {
            let mut payload = plc_connection_payload("not-allowed");
            payload["protocol"] = json!(protocol);
            let response = router
                .clone()
                .oneshot(post_json_auth("/api/plc-connections", &editor, payload))
                .await
                .unwrap();
            assert!(response.status().is_client_error(), "protocol={protocol}");
            let body = body_json(response).await;
            assert_eq!(body["kind"], "validation", "protocol={protocol}");
            assert_eq!(body["field_errors"][0]["field"], "protocol");
            let message = body["field_errors"][0]["message"].as_str().unwrap();
            assert!(!message.is_empty(), "protocol={protocol}: {message}");
        }
    }

    /// banto-tags 自身の検証（`period_ms` は `ALLOWED_PERIOD_MS` のいずれか）
    /// が人間可読な `field_errors` としてそのまま返ることを固定する - 画面の
    /// 選択肢は固定値しか出さないが、REST を直接叩く経路（他クライアント・
    /// 手動テスト）はそれをバイパスできるので、バックエンド側の検証がこの
    /// 経路でも効くことを確認する。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn collection_group_create_rejects_a_period_outside_the_allowed_list_readably() {
        let (router, _admin, editor, _viewer) = router_with_role_tokens().await;

        let conn = body_json(
            router
                .clone()
                .oneshot(post_json_auth(
                    "/api/plc-connections",
                    &editor,
                    plc_connection_payload("plc-for-period-test"),
                ))
                .await
                .unwrap(),
        )
        .await;
        let conn_id = conn["id"].as_i64().unwrap();

        let response = router
            .oneshot(post_json_auth(
                "/api/collection-groups",
                &editor,
                json!({
                    "name": "bad-period",
                    "plcConnectionId": conn_id,
                    "periodMs": 999,
                    "enabled": true
                }),
            ))
            .await
            .unwrap();
        assert!(response.status().is_client_error());
        let body = body_json(response).await;
        assert_eq!(body["kind"], "validation");
        assert_eq!(body["field_errors"][0]["field"], "periodMs");
        assert!(!body["field_errors"][0]["message"]
            .as_str()
            .unwrap()
            .is_empty());
    }

    /// R0 §3.6 の監査記録: PLC接続の作成・更新・削除がそれぞれ
    /// `create`/`update`/`delete` × `resource: "plc_connections"` として
    /// 残る（`origin: "rest"`、actor は実行した editor）。監査ログの閲覧
    /// 自体は admin 限定（`audit_log_router`）なので、書き込みは editor、
    /// 確認は admin のトークンで行う。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn plc_connection_mutations_are_audited() {
        let (router, _audit, admin, editor, _viewer) = router_with_role_tokens_and_audit().await;

        let created = body_json(
            router
                .clone()
                .oneshot(post_json_auth(
                    "/api/plc-connections",
                    &editor,
                    plc_connection_payload("audited-plc"),
                ))
                .await
                .unwrap(),
        )
        .await;
        let conn_id = created["id"].as_i64().unwrap();

        let mut updated_payload = plc_connection_payload("audited-plc-renamed");
        updated_payload["port"] = json!(503);
        router
            .clone()
            .oneshot(put_json(
                &format!("/api/plc-connections/{conn_id}"),
                &editor,
                updated_payload,
            ))
            .await
            .unwrap();

        router
            .clone()
            .oneshot(delete_auth(
                &format!("/api/plc-connections/{conn_id}"),
                &editor,
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
        let conn_id_str = conn_id.to_string();
        for action in ["create", "update", "delete"] {
            let entry = rows
                .iter()
                .find(|r| {
                    r["action"] == action
                        && r["resource"] == "plc_connections"
                        && r["entityId"] == conn_id_str
                })
                .unwrap_or_else(|| {
                    panic!(
                        "expected a {action}/plc_connections entry for id {conn_id}, got {rows:?}"
                    )
                });
            assert_eq!(entry["actorUsername"], "editor");
            assert_eq!(entry["actorRole"], "editor");
            assert_eq!(entry["origin"], "rest");
            assert_eq!(entry["result"], "ok");
        }
    }

    // --- #383 段階2b / R1-C（C-2）: 収集の操作 -------------------------------

    /// `/api/collect*` の全ルートを admin / editor / viewer の 3 役で叩いて、
    /// **床が 2 段**（読み取りは `viewer` 以上・変更は `editor` 以上）である
    /// ことを固定する（2026-09-20 オーナー決定、#407 レビュー。
    /// [`collect_router`] の doc に根拠）。あわせて、**床を分けても拒否は
    /// `resource: "collect"` のまま**であることも見る（#397 の
    /// `denied_hub_command_is_recorded_under_the_hub_resource` が手本。
    /// `src-tauri` 側に**同じ床・同じ綴り**を固定する双子のテストがある）。
    ///
    /// 反証（回帰の検出）: 読み取りの `RoleGuard` の `min` を
    /// `COLLECT_OPERATION_ROLE` に戻すと viewer の `GET` が 403 になって
    /// 落ちる。変更側の `min` を `COLLECT_READ_ROLE` に下げると viewer の
    /// `POST` が 200 になって落ちる。`resource` を `"settings"` 等に変えると
    /// 最後の `assert_eq!` が落ちる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn collect_reads_are_viewer_and_operations_are_editor_all_under_the_collect_resource() {
        let (router, _audit, admin, editor, viewer) = router_with_role_tokens_and_audit().await;

        // **viewer は状態を読める**（R0 §3.6 の「閲覧のみ」は「何も見えない」
        // ではない）。
        let response = router
            .clone()
            .oneshot(get_auth("/api/collect", &viewer))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "viewer は収集の状態を読めること"
        );
        assert_eq!(body_json(response).await["state"], "stopped");

        // viewer は変更できない（床は 3 ルートまとめて 1 つ）。
        for path in [
            "/api/collect/start",
            "/api/collect/stop",
            "/api/collect/restart",
        ] {
            let response = router
                .clone()
                .oneshot(post_json_auth(path, &viewer, json!({})))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
        }

        // editor は変更も通る（admin へ引き上げていない）。
        for token in [&editor, &admin] {
            let response = router
                .clone()
                .oneshot(get_auth("/api/collect", token))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let response = router
                .clone()
                .oneshot(post_json_auth("/api/collect/stop", token, json!({})))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }

        let rows = body_json(
            router
                .oneshot(post_json_auth(
                    "/api/audit-log/list",
                    &admin,
                    json!(ListParams::default()),
                ))
                .await
                .unwrap(),
        )
        .await["rows"]
            .clone();
        let rows = rows.as_array().unwrap().clone();
        let denials: Vec<_> = rows.iter().filter(|r| r["action"] == "denied").collect();
        assert_eq!(
            denials.len(),
            3,
            "変更 3 件だけが拒否される（読み取りは通る）: {rows:?}"
        );
        for entry in denials {
            assert_eq!(
                entry["resource"], "collect",
                "床を分けても resource は分けない（Tauri 側と同じ綴り）: {entry:?}"
            );
            assert_eq!(entry["actorUsername"], "viewer");
            assert_eq!(entry["actorRole"], "viewer");
            assert_eq!(entry["origin"], "rest");
            assert_eq!(entry["result"], "denied");
        }
    }

    /// **収集対象 0 件で開始してもエラーにならない**（`crate::collect` の
    /// 「空とエラーを別の状態にする」規律が、ワイヤまで届いていること）。
    /// このテストのレジストリは空なので `noTargets` が返る。
    ///
    /// 反証（回帰の検出）: `Lifecycle::start` の `tag_count() == 0` 分岐を
    /// 外すと `Collector::start` の `CollectError::Config` が `Err` として
    /// 返り、200 ではなくエラー応答になって落ちる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn collect_start_with_no_targets_is_not_an_error() {
        let (router, _audit, _admin, editor, _viewer) = router_with_role_tokens_and_audit().await;

        let response = router
            .oneshot(post_json_auth("/api/collect/start", &editor, json!({})))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["status"]["state"], "noTargets");
        assert_eq!(
            body["pending"], false,
            "上限に掛かっていないのに「まだ終わっていない」になっている: {body:?}"
        );
    }

    /// **#407 レビュー P2-2 の受入条件（REST 側）**: 起動に失敗した後、
    /// **viewer が状態を読んでも失敗理由が見えない**。
    ///
    /// 検証用の目印をタグのアドレスに埋めて `build_config` を失敗させる
    /// （`banto-tags` はアドレスの書式を検証しないので、REST 経由でも
    /// そのまま登録できる）。`CollectError::Config` の文言はアドレスを
    /// そのまま含むので、**内部の `CollectorState` をワイヤに載せていれば
    /// 目印が必ず出てくる**。
    ///
    /// あわせて、**理由がどこで利用者に伝わるか**も同じテストで押さえる -
    /// 起動を実行した editor には `POST /api/collect/start` の**エラー応答**
    /// として理由が返る（状態の読み取りから理由を落とした分の埋め合わせが
    /// 本当に存在すること。`crate::collect` のモジュール doc
    /// 「理由はどこで利用者に伝わるか」）。
    ///
    /// 反証（回帰の検出）: `collect_status_handler` を
    /// `Json(state.collect.state())`（内部の型）に戻すと、`reason` キーと
    /// 目印の両方が応答に出て 2 つの `assert!` が落ちる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_viewer_reading_the_state_never_sees_why_the_start_failed() {
        const MARKER: &str = "CHRONOGAZER-LEAK-CANARY";
        let (router, _audit, _admin, editor, viewer) = router_with_role_tokens_and_audit().await;

        // 収集対象を 1 件だけ作る。アドレスが解釈できないので
        // `build_config` が失敗し、状態に理由（= 目印を含む）が焼き付く。
        let conn = body_json(
            router
                .clone()
                .oneshot(post_json_auth(
                    "/api/plc-connections",
                    &editor,
                    plc_connection_payload("plc1"),
                ))
                .await
                .unwrap(),
        )
        .await;
        let group = body_json(
            router
                .clone()
                .oneshot(post_json_auth(
                    "/api/collection-groups",
                    &editor,
                    json!({
                        "name": "group1",
                        "plcConnectionId": conn["id"].as_i64().unwrap(),
                        "periodMs": 1000,
                        "enabled": true
                    }),
                ))
                .await
                .unwrap(),
        )
        .await;
        let create_tag = router
            .clone()
            .oneshot(post_json_auth(
                "/api/tags",
                &editor,
                json!({
                    "name": "tag1",
                    "collectionGroupId": group["id"].as_i64().unwrap(),
                    "address": MARKER,
                    "dataType": "i16",
                    "decimals": 0,
                    "enabled": true
                }),
            ))
            .await
            .unwrap();
        assert_eq!(create_tag.status(), StatusCode::OK);

        // 起動した本人（editor）には、エラー応答として理由が返る。
        let start = router
            .clone()
            .oneshot(post_json_auth("/api/collect/start", &editor, json!({})))
            .await
            .unwrap();
        assert_ne!(
            start.status(),
            StatusCode::OK,
            "前提が崩れている: 起動が失敗していない"
        );
        let start_body = body_json(start).await.to_string();
        assert!(
            start_body.contains(MARKER),
            "起動を実行した本人にも理由が伝わっていない（埋め合わせが無い）: {start_body}"
        );

        // viewer が状態を読む: 状態そのものは返るが、理由は返らない。
        let status = router
            .oneshot(get_auth("/api/collect", &viewer))
            .await
            .unwrap();
        assert_eq!(status.status(), StatusCode::OK);
        let body = body_json(status).await;
        assert_eq!(
            body["state"], "startFailed",
            "状態そのものは返すこと（理由だけを落とす）: {body}"
        );
        assert!(
            body.get("reason").is_none(),
            "`reason` フィールドがワイヤに出ている: {body}"
        );
        assert!(
            !body.to_string().contains(MARKER),
            "起動失敗の理由が viewer に漏れている: {body}"
        );
    }

    /// 成功した操作が監査に残る（操作者と結果の状態）。`resource` は
    /// **拒否と同じ `"collect"`** で、`action` は `start`/`stop`/`restart`。
    /// `detail` に入るのは状態の識別子と `pending` だけ - 接続先や
    /// ファイルパスは入らない。
    ///
    /// 反証（回帰の検出）: `record_collect_operation` の呼び出しを消すと
    /// 3 つの `expect` が落ちる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn collect_operations_are_audited_under_the_collect_resource() {
        let (router, _audit, admin, editor, _viewer) = router_with_role_tokens_and_audit().await;

        for path in [
            "/api/collect/start",
            "/api/collect/restart",
            "/api/collect/stop",
        ] {
            let response = router
                .clone()
                .oneshot(post_json_auth(path, &editor, json!({})))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path}");
        }

        let rows = body_json(
            router
                .oneshot(post_json_auth(
                    "/api/audit-log/list",
                    &admin,
                    json!(ListParams::default()),
                ))
                .await
                .unwrap(),
        )
        .await["rows"]
            .clone();
        let rows = rows.as_array().unwrap().clone();
        for action in ["start", "restart", "stop"] {
            let entry = rows
                .iter()
                .find(|r| r["action"] == action && r["resource"] == "collect")
                .unwrap_or_else(|| panic!("expected a {action}/collect entry, got {rows:?}"));
            assert_eq!(entry["actorUsername"], "editor");
            assert_eq!(entry["actorRole"], "editor");
            assert_eq!(entry["origin"], "rest");
            assert_eq!(entry["result"], "ok");
            // `detail` は JSON を**文字列として**持つ列（`AuditLogEntry` の
            // doc）なので、ここで解いてから中身を見る。
            let detail: serde_json::Value = serde_json::from_str(
                entry["detail"]
                    .as_str()
                    .unwrap_or_else(|| panic!("detail が空: {entry:?}")),
            )
            .expect("detail は JSON");
            assert_eq!(detail["pending"], false);
            assert!(
                detail["collectState"].is_string(),
                "結果の状態が残っていない: {detail:?}"
            );
            assert!(
                detail.get("reason").is_none(),
                "内部の詳細（理由）が監査に漏れている: {detail:?}"
            );
        }
    }

    // --- #383 段階2b / R1-C（C-3a）: 収集の読み出し ---------------------------

    /// C-3a の 3 本（現在値・接続状態・イベント一覧）も**状態の読み取りと
    /// 同じ床**（`viewer` 以上 - [`COLLECT_READ_ROLE`]）で、**トークンが
    /// 無ければ 401**。`src-tauri` 側に**同じことを主張する双子のテスト**が
    /// ある（両経路の床が一致していることの固定）。
    ///
    /// あわせて、**収集が走っていないときの 3 本の答え**も見る:
    /// 現在値と接続状態は `notRunning`（**空の `{}` ではない**）、
    /// イベント一覧は**それでも読める** `ready` で 0 件（過去の記録は
    /// 走っているかどうかと無関係 - `crate::collect` のモジュール doc）。
    ///
    /// 反証（回帰の検出）: 読み取りの `RoleGuard` の `min` を
    /// `COLLECT_OPERATION_ROLE` に戻すと viewer の 3 本が 403 になって落ちる。
    /// `values`/`connections` が走っていないときに空の `Ready` を返す実装に
    /// すると `state` の `assert_eq!` が落ちる。イベント一覧が収集の状態を
    /// 見て `notRunning` を返す実装にすると最後の `assert_eq!` が落ちる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn collect_readouts_are_viewer_readable_and_never_collapse_not_running_into_zero() {
        let (router, _audit, _admin, _editor, viewer) = router_with_role_tokens_and_audit().await;

        for path in [
            "/api/collect/values",
            "/api/collect/connections",
            "/api/collect/events",
        ] {
            // トークン無しは拒否（`require_auth` が最初に効く）。
            let anonymous = router.clone().oneshot(get(path)).await.unwrap();
            assert_eq!(
                anonymous.status(),
                StatusCode::UNAUTHORIZED,
                "{path} が未認証で読めている"
            );

            // viewer は読める。
            let response = router
                .clone()
                .oneshot(get_auth(path, &viewer))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "viewer が {path} を読めない"
            );
        }

        // 走っていないときの答え（このテストのレジストリは空なので、収集は
        // 一度も起動していない）。
        let values = body_json(
            router
                .clone()
                .oneshot(get_auth("/api/collect/values", &viewer))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(
            values["state"], "notRunning",
            "「走っていない」を「0 件」に潰している: {values}"
        );
        let connections = body_json(
            router
                .clone()
                .oneshot(get_auth("/api/collect/connections", &viewer))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(connections["state"], "notRunning", "{connections}");

        // イベント一覧は走っていなくても読める（0 件という事実が返る）。
        let events = body_json(
            router
                .oneshot(get_auth("/api/collect/events", &viewer))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(
            events["state"], "ready",
            "過去の記録の一覧を「走っていない」で隠している: {events}"
        );
        assert_eq!(events["data"]["rows"].as_array().unwrap().len(), 0);
        assert_eq!(events["data"]["totalCount"], 0);
    }

    /// イベント一覧の**ワイヤ形**（#383 段階2b / R1-C の C-3a）:
    ///
    /// * **`detail` 列を返さない**（自由文で、接続先やファイルパスを含みうる。
    ///   C-2 の P2 と同じ類の漏れを繰り返さない）、
    /// * 総件数は `totalCount`（監査ログ一覧と同じ綴り）、
    /// * `?limit=`/`?offset=` が効き、**新しい順**。
    ///
    /// `collect_events` に書くのは収集エンジンだけ（REST には書き込み口が
    /// 無い）ので、router と同じプールへ直接 1 行入れて読む。
    ///
    /// 反証（回帰の検出）: `crate::collect::CollectEventRow` に `detail` を
    /// 足して `read_events` の `SELECT` にも足すと、目印の `assert!` が落ちる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn collect_events_are_paged_newest_first_and_never_carry_the_free_text_detail() {
        const MARKER: &str = "CHRONOGAZER-LEAK-CANARY";
        let (router, _audit, pool, _admin, _editor, viewer) =
            router_with_role_tokens_audit_and_pool().await;

        for (ts, kind, detail) in [
            (100_i64, "collection_started", None),
            (
                200,
                "plc_disconnected",
                Some(format!("接続エラー: {MARKER} に繋がりません")),
            ),
            (300, "plc_reconnected", None),
        ] {
            sqlx::query(
                "INSERT INTO collect_events (ts, kind, connection_key, detail) \
                 VALUES (?, ?, 'conn:1', ?)",
            )
            .bind(ts)
            .bind(kind)
            .bind(detail)
            .execute(&pool)
            .await
            .expect("insert collect_events");
        }

        let body = body_json(
            router
                .clone()
                .oneshot(get_auth("/api/collect/events?limit=2", &viewer))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(body["state"], "ready");
        assert!(
            !body.to_string().contains(MARKER),
            "自由文の detail がワイヤに漏れている: {body}"
        );
        let rows = body["data"]["rows"].as_array().unwrap().clone();
        assert_eq!(rows.len(), 2, "?limit= が効いていない: {body}");
        assert_eq!(
            rows[0]["kind"], "plc_reconnected",
            "新しい順になっていない: {body}"
        );
        assert!(
            rows[0].get("detail").is_none(),
            "`detail` フィールドがワイヤに出ている: {body}"
        );
        assert_eq!(
            body["data"]["totalCount"], 3,
            "総件数はページングの前の件数（監査ログ一覧と同じ）: {body}"
        );

        // 2 ページ目。
        let body = body_json(
            router
                .clone()
                .oneshot(get_auth("/api/collect/events?limit=2&offset=2", &viewer))
                .await
                .unwrap(),
        )
        .await;
        let rows = body["data"]["rows"].as_array().unwrap().clone();
        assert_eq!(rows.len(), 1, "?offset= が効いていない: {body}");
        assert_eq!(rows[0]["kind"], "collection_started");
        assert_eq!(
            body["data"]["asOfId"], 3,
            "境界未指定ならその時点の最大 id を使い、応答に載せること: {body}"
        );

        // スナップショット境界（#409 レビュー P2-2）: `?asOfId=` が効き、
        // 境界の後の行は件数にも行にも入らない。
        let body = body_json(
            router
                .oneshot(get_auth("/api/collect/events?asOfId=2", &viewer))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(body["data"]["asOfId"], 2, "?asOfId= が効いていない: {body}");
        assert_eq!(body["data"]["totalCount"], 2, "{body}");
        let ids: Vec<i64> = body["data"]["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["id"].as_i64().unwrap())
            .collect();
        assert_eq!(ids, vec![2, 1], "境界の後の行が混ざった: {body}");
    }
}
