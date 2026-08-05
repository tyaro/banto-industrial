//! REST surface for banto-hub (docs/tag-server-design.md §5.1「REST（T0）」・
//! §5.6「認証（全プロトコル共通）」)。
//!
//! ## 二系統に分かれたルーター
//!
//! - **管理系**（`/api/auth/*`・`/api/users/*`・`/api/audit-log/*`・
//!   `/api/plc-connections|collection-groups|tags/*`・`/api/events`）:
//!   `apps/chronogazer/core` / `apps/relay-wright/core` と同型 —
//!   `require_banto_client_header`（CSRF）をルーター全体に適用し、
//!   ブラウザ管理 UI 用の bearer セッション + RBAC（viewer 読み取り /
//!   editor 書き込み / admin 限定）で保護する。
//! - **タグ空間 API**（`/api/v1/*`）: 機械クライアント向け別ルーター
//!   （設計 §5.1/§5.6）。CSRF ヘッダは要求しない — ブラウザ CSRF 対策は
//!   「JS からしか付けられない独自ヘッダ」が前提だが、機械クライアントは
//!   そもそも任意ヘッダを付けられるので CSRF の脅威モデルに乗らない。
//!   認証は `require_auth`（bearer セッション）のみを適用する。
//!   **T0-2 で API キー認証（設計 §5.6 のスコープ）に置き換える予定** —
//!   T0-1 時点では管理 UI と同じ bearer トークンを共用する暫定実装。
//!
//! ## I1 CRUD 書き込み後の再構築（設計 §4.3）
//!
//! `tag_registry_router` の書き込みハンドラ（create/update/delete、3
//! リソース共通）は、レジストリへの書き込みが成功した後に必ず
//! [`crate::hub::CollectorManager::rebuild`] を呼ぶ。rebuild が失敗しても
//! CRUD 自体は成功のまま返す（設計指示: 「rebuild 失敗は CRUD 自体の失敗に
//! しない」）— 定義は保存済みで、Collector が旧構成のまま
//! `last_error`（`/api/v1/status`）に出る、という状態を許容する。
//! 併せて admin-UI 向けの `ServerEvent::ResourceChanged` を SSE (`/api/events`)
//! に流す（レジストリが実際に変わったことは rebuild の成否と独立な事実なので、
//! rebuild が失敗していても送る）。
//!
//! ## タグ空間の値の意味論（設計 §4）
//!
//! `/api/v1/values*` は [`crate::hub::CollectorManager::current_values`] を
//! 読むだけで完結し、PLC への追加要求を発生させない。無効化されている
//! タグ（`TagEntry::enabled == false` — 接続・グループ・タグいずれかが無効）
//! は、たとえ過去のサンプルが current-value キャッシュに残っていても
//! 強制的に `q: "bad", v: null` を返す（欠測を隠さない）。404 になるのは
//! catalog に定義そのものが存在しない外部名だけ。

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use banto_collect::ConnectionStatus;
use banto_core::{BantoError, ErrorBody, ListParams, ListResult};
use banto_server::{
    auth_routes, require_auth, require_banto_client_header, sse_route, ApiError, AuthState,
    Identity, ServerEvent,
};
use banto_tags::{
    CollectionGroup, CollectionGroupInput, CollectionGroupService, PlcConnection,
    PlcConnectionInput, PlcConnectionService, Tag, TagInput, TagService,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::str::FromStr;
use tokio::sync::broadcast;

use crate::audit::{AuditEntry, AuditLogService};
use crate::hub::{CollectorManager, TagEntry};
use crate::users::{Role, UserIdentity, UserSummary, UsersService};

// --- shared helpers (users/audit/RBAC - copied from chronogazer/relay-wright's rest.rs) ---

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
}

fn actor_identity(headers: &HeaderMap, auth: &AuthState) -> Option<Identity> {
    bearer_token(headers).and_then(|token| auth.identity_for(token))
}

/// Record a successful write once the service call it follows has already
/// succeeded - same convention as chronogazer/relay-wright's `record_write`.
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

/// Resolve the caller and require role >= `editor` (spec M10 の慣行踏襲:
/// viewer 読み取り / editor 書き込み)。 I1 の3リソースの書き込みハンドラで
/// 共通に使う - `relay-wright-core::rest::require_editor` と同型。
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

// --- users admin (spec-equivalent of chronogazer's M10 users_router) ------

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

#[derive(Clone)]
struct UsersAdminState {
    users: UsersService,
    auth: AuthState,
    audit: AuditLogService,
}

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

// --- extra auth routes (status/setup/change-password) ---------------------

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

/// Wraps `UsersService::verify` as the async credential verifier
/// `banto_server::AuthState::new` expects, additionally recording a
/// `login`/`login_failed` audit entry - copied from chronogazer's
/// `audited_credential_verifier`.
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

#[derive(Clone)]
struct LogoutAuditState {
    auth: AuthState,
    audit: AuditLogService,
}

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

// --- audit log (read-only in T0: no retention-config endpoints - not part
// of the T0-1 scope) ---------------------------------------------------------

#[derive(Clone)]
struct AuditLogState {
    audit: AuditLogService,
}

async fn audit_log_list(
    State(state): State<AuditLogState>,
    Json(params): Json<ListParams>,
) -> Result<Json<ListResult<crate::audit::AuditLogEntry>>, ApiError> {
    Ok(Json(state.audit.list(params).await?))
}

fn audit_log_router(audit: AuditLogService, auth: AuthState) -> Router {
    let state = AuditLogState {
        audit: audit.clone(),
    };
    Router::new()
        .route("/api/audit-log/list", post(audit_log_list))
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

// --- I1 CRUD (viewer-read / editor-write) + collector rebuild -------------

fn default_payload_enabled() -> bool {
    true
}

fn default_plc_protocol() -> String {
    "modbus-tcp".to_string()
}

fn default_plc_unit_id() -> i64 {
    1
}

fn default_tag_decimals() -> i64 {
    0
}

/// Wire-shaped (camelCase) create/update payload for `plc_connections` -
/// copied from relay-wright's `PlcConnectionPayload` (invariant across every
/// app that exposes I1 over REST: one payload shape).
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
        }
    }
}

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
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TagPayload {
    pub name: String,
    pub collection_group_id: i64,
    pub address: String,
    pub data_type: String,
    #[serde(default)]
    pub string_length: Option<i64>,
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
    #[serde(default)]
    pub threshold_h: Option<f64>,
    #[serde(default)]
    pub threshold_hh: Option<f64>,
    #[serde(default)]
    pub threshold_l: Option<f64>,
    #[serde(default)]
    pub threshold_ll: Option<f64>,
    #[serde(default)]
    pub enabled: bool,
}

impl From<TagPayload> for TagInput {
    fn from(payload: TagPayload) -> Self {
        Self {
            name: payload.name,
            collection_group_id: payload.collection_group_id,
            address: payload.address,
            data_type: payload.data_type,
            string_length: payload.string_length,
            raw_lo: payload.raw_lo,
            raw_hi: payload.raw_hi,
            eng_lo: payload.eng_lo,
            eng_hi: payload.eng_hi,
            unit: payload.unit,
            decimals: payload.decimals,
            threshold_h: payload.threshold_h,
            threshold_hh: payload.threshold_hh,
            threshold_l: payload.threshold_l,
            threshold_ll: payload.threshold_ll,
            enabled: payload.enabled,
        }
    }
}

/// Rebuild the collector and notify admin-UI SSE subscribers after an I1
/// write. Never fails the caller (design instructions: 「rebuild 失敗は CRUD
/// 自体の失敗にしない」) - a rebuild failure is only logged; its message
/// remains visible via `/api/v1/status`'s `last_config_error`.
async fn rebuild_and_notify(
    manager: &CollectorManager,
    events: &broadcast::Sender<ServerEvent>,
    resource: &str,
) {
    if let Err(err) = manager.rebuild().await {
        eprintln!("banto-hub: {resource} 変更後の collector 再構築に失敗しました: {err}");
    }
    let _ = events.send(ServerEvent::ResourceChanged {
        resource: resource.to_string(),
    });
}

#[derive(Clone)]
struct TagRegistryState {
    plc_connections: PlcConnectionService,
    collection_groups: CollectionGroupService,
    tags: TagService,
    auth: AuthState,
    audit: AuditLogService,
    manager: Arc<CollectorManager>,
    events: broadcast::Sender<ServerEvent>,
}

async fn plc_connections_list(
    State(state): State<TagRegistryState>,
) -> Result<Json<Vec<PlcConnection>>, ApiError> {
    Ok(Json(
        state
            .plc_connections
            .list(ListParams::default())
            .await?
            .rows,
    ))
}

async fn plc_connections_get(
    State(state): State<TagRegistryState>,
    Path(id): Path<i64>,
) -> Result<Json<PlcConnection>, ApiError> {
    Ok(Json(state.plc_connections.get(id).await?))
}

async fn plc_connections_create(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Json(input): Json<PlcConnectionPayload>,
) -> Result<Json<PlcConnection>, ApiError> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "plc_connections",
        "POST",
        "/api/plc-connections",
    )
    .await?;
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
    rebuild_and_notify(&state.manager, &state.events, "plc_connections").await;
    Ok(Json(created))
}

async fn plc_connections_update(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(input): Json<PlcConnectionPayload>,
) -> Result<Json<PlcConnection>, ApiError> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "plc_connections",
        "PUT",
        "/api/plc-connections/{id}",
    )
    .await?;
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
    rebuild_and_notify(&state.manager, &state.events, "plc_connections").await;
    Ok(Json(updated))
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
    rebuild_and_notify(&state.manager, &state.events, "plc_connections").await;
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
    rebuild_and_notify(&state.manager, &state.events, "collection_groups").await;
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
    rebuild_and_notify(&state.manager, &state.events, "collection_groups").await;
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
    rebuild_and_notify(&state.manager, &state.events, "collection_groups").await;
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
    rebuild_and_notify(&state.manager, &state.events, "tags").await;
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
    rebuild_and_notify(&state.manager, &state.events, "tags").await;
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
    rebuild_and_notify(&state.manager, &state.events, "tags").await;
    Ok(StatusCode::NO_CONTENT)
}

/// `/api/plc-connections/*` + `/api/collection-groups/*` + `/api/tags/*`
/// (viewer-read / editor-write) - `relay-wright-core::rest::tag_registry_router`
/// を雛形に、書き込み成功後に必ず [`rebuild_and_notify`] を挟む点だけが違う。
#[allow(clippy::too_many_arguments)]
fn tag_registry_router(
    plc_connections: PlcConnectionService,
    collection_groups: CollectionGroupService,
    tags: TagService,
    audit: AuditLogService,
    auth: AuthState,
    manager: Arc<CollectorManager>,
    events: broadcast::Sender<ServerEvent>,
) -> Router {
    let state = TagRegistryState {
        plc_connections,
        collection_groups,
        tags,
        auth: auth.clone(),
        audit,
        manager,
        events,
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

// --- /api/v1/* タグ空間 API（設計 §5.1） ------------------------------------

#[derive(Clone)]
struct TagSpaceState {
    manager: Arc<CollectorManager>,
}

fn quality_str(quality: banto_collect::Quality) -> &'static str {
    match quality {
        banto_collect::Quality::Good => "good",
        banto_collect::Quality::Bad => "bad",
        banto_collect::Quality::Stale => "stale",
    }
}

#[derive(Debug, Deserialize)]
struct TagsQuery {
    connection: Option<String>,
    group: Option<String>,
}

/// `GET /api/v1/tags` - catalog: `{ "revision", "tags": [TagEntry...] }`,
/// optionally filtered by `?connection=`/`?group=` (matched against the
/// entry's connection/group *name*, design §5.1's route table).
async fn v1_tags(
    State(state): State<TagSpaceState>,
    Query(query): Query<TagsQuery>,
) -> Json<serde_json::Value> {
    let map = state.manager.tag_map();
    let revision = state.manager.revision();
    let tags: Vec<&TagEntry> = map
        .iter()
        .filter(|entry| {
            query
                .connection
                .as_deref()
                .map(|c| c == entry.connection)
                .unwrap_or(true)
        })
        .filter(|entry| {
            query
                .group
                .as_deref()
                .map(|g| g == entry.group)
                .unwrap_or(true)
        })
        .collect();
    Json(json!({ "revision": revision, "tags": tags }))
}

/// One `/api/v1/values*` entry's wire shape (design §5.1's route table:
/// `{ "tag", "v", "q", "t" }`).
fn value_json(
    external_name: &str,
    entry: &TagEntry,
    sample: Option<banto_collect::CurrentSample>,
    now_ms: i64,
) -> serde_json::Value {
    // A disabled tag (its own flag, or its group's/connection's) always
    // reads bad/null regardless of what a stale cached sample says (design
    // §4: 欠測を隠さない - a client must not be able to mistake "this was
    // last collected while still enabled" for "this is currently good").
    let (v, q, t) = if !entry.enabled {
        (None, "bad", sample.map(|s| s.ptime_ms).unwrap_or(now_ms))
    } else {
        match sample {
            Some(s) => (s.value, quality_str(s.quality), s.ptime_ms),
            None => (None, "bad", now_ms),
        }
    };
    json!({ "tag": external_name, "v": v, "q": q, "t": t })
}

#[derive(Debug, Deserialize)]
struct ValuesQuery {
    tags: Option<String>,
}

/// `GET /api/v1/values` - full or partial (`?tags=a,b,c`) snapshot.
///
/// An unknown name in `?tags=` is a `400` enumerating every unresolved name
/// (design instructions: 「未知の名前が混ざったら...部分成功で誤解させない」),
/// never a per-row `bad`/`unknown_tag` - the request as a whole is rejected
/// so the caller cannot mistake "misspelled tag" for "tag exists but is
/// currently bad".
async fn v1_values(
    State(state): State<TagSpaceState>,
    Query(query): Query<ValuesQuery>,
) -> Response {
    let map = state.manager.tag_map();
    let revision = state.manager.revision();
    let now_ms = state.manager.clock().now_ms();
    let current = state.manager.current_values();

    let names: Vec<String> = match &query.tags {
        Some(raw) => raw
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        None => map
            .iter()
            .map(|entry| entry.external_name.clone())
            .collect(),
    };

    if query.tags.is_some() {
        let unknown: Vec<&str> = names
            .iter()
            .map(String::as_str)
            .filter(|name| map.get(name).is_none())
            .collect();
        if !unknown.is_empty() {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "unknown_tag", "tags": unknown })),
            )
                .into_response();
        }
    }

    let values: Vec<serde_json::Value> = names
        .iter()
        .filter_map(|name| map.get(name).map(|entry| (name, entry)))
        .map(|(name, entry)| {
            let sample = current.as_ref().and_then(|c| c.get(&entry.tag_key));
            value_json(name, entry, sample, now_ms)
        })
        .collect();

    Json(json!({ "revision": revision, "t": now_ms, "values": values })).into_response()
}

/// `GET /api/v1/values/{tag}` - single tag. `404` only when the external
/// name is not in the catalog at all (design: 「404 になるのは定義が存在
/// しない外部名のみ」) - an undefined-but-uncollected tag is `200` with
/// `q: "bad"`.
async fn v1_value_single(
    State(state): State<TagSpaceState>,
    Path(tag): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let map = state.manager.tag_map();
    let Some(entry) = map.get(&tag) else {
        return Err(ApiError(BantoError::NotFound {
            resource: "tags".to_string(),
            id: tag,
        }));
    };
    let now_ms = state.manager.clock().now_ms();
    let current = state.manager.current_values();
    let sample = current.as_ref().and_then(|c| c.get(&entry.tag_key));
    Ok(Json(value_json(&tag, entry, sample, now_ms)))
}

/// `GET /api/v1/status` - `{ "version", "revision", "last_config_error",
/// "connections": [...] }` (design §5.1's route table). Connection names
/// come from the registry directly (not the catalog) so a connection with
/// zero tags still appears.
async fn v1_status(
    State(state): State<TagSpaceState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let revision = state.manager.revision();
    let last_config_error = state.manager.last_error();
    let statuses = state.manager.connection_status();

    let connections = PlcConnectionService::new(state.manager.pool())
        .list(ListParams::default())
        .await?
        .rows;

    let entries: Vec<serde_json::Value> = connections
        .into_iter()
        .map(|conn| {
            let key = format!("conn:{}", conn.id);
            let (status_str, attempt) = match statuses.get(&key) {
                Some(ConnectionStatus::Connected) => ("connected", None),
                Some(ConnectionStatus::Reconnecting { attempt }) => {
                    ("reconnecting", Some(*attempt))
                }
                // "stopped" also covers "never started" (e.g. this
                // connection - or everything - is currently disabled, or no
                // rebuild has run yet) - the design's ConnectionStatus enum
                // has no fourth "never seen" variant, and "stopped" reads
                // correctly for all of those (design: 決定事項なし → 妥当な
                // 解釈として Stopped に丸める).
                Some(ConnectionStatus::Stopped) | None => ("stopped", None),
            };
            json!({
                "name": conn.name,
                "id": conn.id,
                "status": status_str,
                "attempt": attempt,
            })
        })
        .collect();

    Ok(Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "revision": revision,
        "last_config_error": last_config_error,
        "connections": entries,
    })))
}

#[derive(Debug, Deserialize)]
struct EventsQuery {
    from_ms: Option<i64>,
    to_ms: Option<i64>,
    limit: Option<i64>,
}

/// `GET /api/v1/events` - range query over `collect_events`
/// (`crates/banto-collect/migrations/0001_collect_events.sql`'s columns),
/// newest first, default `limit` 100 (clamped to a sane range so a
/// misbehaving client cannot force an unbounded scan).
async fn v1_events(
    State(state): State<TagSpaceState>,
    Query(query): Query<EventsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let from_ms = query.from_ms.unwrap_or(0);
    let to_ms = query.to_ms.unwrap_or(i64::MAX);
    let limit = query.limit.unwrap_or(100).clamp(1, 1000);

    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        i64,
        i64,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<f64>,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT id, ts, kind, connection_key, tag_key, level, value, detail \
         FROM collect_events WHERE ts >= ? AND ts <= ? ORDER BY ts DESC, id DESC LIMIT ?",
    )
    .bind(from_ms)
    .bind(to_ms)
    .bind(limit)
    .fetch_all(&state.manager.pool())
    .await
    .map_err(banto_storage::storage_error)?;

    let events: Vec<serde_json::Value> = rows
        .into_iter()
        .map(
            |(id, ts, kind, connection_key, tag_key, level, value, detail)| {
                json!({
                    "id": id,
                    "ts": ts,
                    "kind": kind,
                    "connection_key": connection_key,
                    "tag_key": tag_key,
                    "level": level,
                    "value": value,
                    "detail": detail,
                })
            },
        )
        .collect();

    Ok(Json(json!({ "events": events })))
}

/// `/api/v1/*` (design §5.1/§5.6): `require_auth` only, no CSRF header - see
/// this module's doc comment.
fn tag_space_router(manager: Arc<CollectorManager>, auth: AuthState) -> Router {
    let state = TagSpaceState { manager };
    Router::new()
        .route("/api/v1/tags", get(v1_tags))
        .route("/api/v1/values", get(v1_values))
        .route("/api/v1/values/{tag}", get(v1_value_single))
        .route("/api/v1/status", get(v1_status))
        .route("/api/v1/events", get(v1_events))
        .with_state(state)
        .layer(middleware::from_fn_with_state(auth, require_auth))
}

// --- composition ------------------------------------------------------------

/// Compose the full router: the admin surface (auth/users/audit-log/I1 CRUD/
/// SSE, all behind CSRF + bearer auth) merged with the tag-space API
/// (bearer auth only, no CSRF - see this module's doc comment).
#[allow(clippy::too_many_arguments)]
pub fn api_router(
    users: UsersService,
    audit: AuditLogService,
    plc_connections: PlcConnectionService,
    collection_groups: CollectionGroupService,
    tags: TagService,
    manager: Arc<CollectorManager>,
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

    let admin = Router::new()
        .merge(audited_auth_routes)
        .merge(extra_auth_router(
            users.clone(),
            auth.clone(),
            audit.clone(),
            allow_setup,
        ))
        .merge(sse_route(auth.clone(), events.clone()))
        .merge(users_router(users, audit.clone(), auth.clone()))
        .merge(audit_log_router(audit.clone(), auth.clone()))
        .merge(tag_registry_router(
            plc_connections,
            collection_groups,
            tags,
            audit,
            auth.clone(),
            manager.clone(),
            events,
        ))
        .layer(middleware::from_fn(require_banto_client_header));

    admin.merge(tag_space_router(manager, auth))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrate_memory;
    use crate::hub::CollectorManager;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use banto_collect::CollectorOptions;
    use banto_tstore::SystemClock;
    use tokio::sync::broadcast as tokio_broadcast;
    use tower::ServiceExt;

    const CLIENT_HEADER: (&str, &str) = ("X-Banto-Client", "banto");

    /// A `CollectorManager` for router-composition tests that never actually
    /// rebuild (I1 CRUD tests below only cover auth/RBAC/plumbing, not the
    /// collector lifecycle itself - that is `hub.rs`'s and the integration
    /// test's job). Points at a real temp dir since `CollectorManager` always
    /// needs a `data_dir`, even if `rebuild` is never called in a given test.
    fn test_manager(pool: sqlx::SqlitePool) -> (Arc<CollectorManager>, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let manager = CollectorManager::new(
            pool,
            dir.path().join("data"),
            Arc::new(SystemClock),
            CollectorOptions::default(),
        );
        (Arc::new(manager), dir)
    }

    async fn router_with_token() -> (Router, String, tempfile::TempDir) {
        let pool = migrate_memory().await.expect("migrate_memory");
        let (tx, _rx) = tokio_broadcast::channel(16);
        let users = UsersService::new(pool.clone());
        let audit = AuditLogService::new(pool.clone());
        let plc_connections = PlcConnectionService::new(pool.clone());
        let collection_groups = CollectionGroupService::new(pool.clone());
        let tags = TagService::new(pool.clone());
        let (manager, dir) = test_manager(pool);

        users
            .setup_first_user("admin", "password123", "管理者")
            .await
            .expect("setup_first_user");
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
        let token = auth
            .login("admin", "password123")
            .await
            .expect("admin login");

        let router = api_router(
            users,
            audit,
            plc_connections,
            collection_groups,
            tags,
            manager,
            auth,
            tx,
            false,
        );
        (router, token, dir)
    }

    #[tokio::test]
    async fn v1_tags_requires_auth_but_not_the_csrf_header() {
        let (router, token, _dir) = router_with_token().await;

        // No X-Banto-Client header at all - would 403 on the admin surface,
        // must succeed on /api/v1/*.
        let response = router
            .clone()
            .oneshot(
                HttpRequest::get("/api/v1/tags")
                    .header("Authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // No bearer token at all - must 401.
        let response = router
            .oneshot(
                HttpRequest::get("/api/v1/tags")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn admin_routes_require_the_csrf_header() {
        let (router, token, _dir) = router_with_token().await;
        let response = router
            .oneshot(
                HttpRequest::get("/api/tags")
                    .header("Authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn v1_values_unknown_tag_is_a_400_listing_every_unresolved_name() {
        let (router, token, _dir) = router_with_token().await;
        let response = router
            .oneshot(
                HttpRequest::get("/api/v1/values?tags=nope.nope.nope,also.missing.one")
                    .header("Authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["error"], "unknown_tag");
        assert_eq!(json["tags"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn v1_value_single_unknown_tag_is_404() {
        let (router, token, _dir) = router_with_token().await;
        let response = router
            .oneshot(
                HttpRequest::get("/api/v1/values/nope.nope.nope")
                    .header("Authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn tags_create_via_admin_router_rebuilds_the_catalog() {
        let (router, token, _dir) = router_with_token().await;

        let create_conn = HttpRequest::post("/api/plc-connections")
            .header("Authorization", format!("Bearer {token}"))
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"name":"line1","host":"127.0.0.1","port":15022}"#,
            ))
            .unwrap();
        let response = router.clone().oneshot(create_conn).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // The catalog is empty until a tag exists, but the write itself must
        // have gone through the CSRF+auth+RBAC gate cleanly and triggered
        // (an ultimately empty, since nothing is collectible yet) rebuild
        // without panicking or erroring the request.
        let response = router
            .oneshot(
                HttpRequest::get("/api/v1/status")
                    .header("Authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["revision"], 1);
        assert!(json["last_config_error"].is_null());
        assert_eq!(json["connections"].as_array().unwrap().len(), 1);
    }
}
