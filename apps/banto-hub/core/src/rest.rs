//! REST surface for banto-hub (docs/tag-server-design.md §5.1「REST（T0）」・
//! §5.6「認証（全プロトコル共通）」)。
//!
//! ## 二系統に分かれたルーター
//!
//! - **管理系**（`/api/auth/*`・`/api/users/*`・`/api/audit-log/*`・
//!   `/api/plc-connections|collection-groups|tags/*`・`/api/api-keys/*`・
//!   `/api/events`）: `apps/chronogazer/core` / `apps/relay-wright/core` と
//!   同型 — `require_banto_client_header`（CSRF）をルーター全体に適用し、
//!   ブラウザ管理 UI 用の bearer セッション + RBAC（viewer 読み取り /
//!   editor 書き込み / admin 限定）で保護する。`/api/api-keys/*`（API キー
//!   の発行・一覧・失効、設計 §5.6・T0-2）は admin ロール限定。
//! - **タグ空間 API**（`/api/v1/*`）: 機械クライアント向け別ルーター
//!   （設計 §5.1/§5.6。`GET /api/v1/stream` の WebSocket 購読は T1、
//!   `crate::stream` 参照）。CSRF ヘッダは要求しない — ブラウザ CSRF 対策は
//!   「JS からしか付けられない独自ヘッダ」が前提だが、機械クライアントは
//!   そもそも任意ヘッダを付けられるので CSRF の脅威モデルに乗らない。
//!   **認証は API キー + セッション bearer の併用**（T0-2、設計 §5.6）:
//!   `Authorization: Bearer <value>` の `<value>` が `bh_` で始まれば
//!   `crate::api_keys::ApiKeysService` で照合（`read` スコープ必須、
//!   失効済みなら 401 + audit_log 記録）、それ以外は従来どおり
//!   `AuthState` のセッション token として照合する（管理 UI からの直接
//!   利用互換のため）。`GET /api/v1/openapi.json` だけは認証不要 -
//!   スキーマ自体は秘密ではないため（`openapi_json` 関数の doc comment
//!   参照）。
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

use axum::extract::{Extension, Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use banto_broker::BrokerConnectionStatus;
use banto_collect::ConnectionStatus;
use banto_core::{BantoError, ErrorBody, FieldError, ListParams, ListResult};
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
use tokio::sync::Mutex as AsyncMutex;
use utoipa::{OpenApi, ToSchema};

use crate::api_keys::{ApiKeyContext, ApiKeyLookup, ApiKeysService, IssuedApiKey};
use crate::audit::{AuditEntry, AuditLogService};
use crate::hub::{CollectorManager, TagEntry};
use crate::mqtt::MqttPublisher;
use crate::settings::{MqttSettings, SettingsService};
use crate::users::{Role, UserIdentity, UserSummary, UsersService};
use crate::write_audit::{WriteAuditEntry, WriteAuditService};
use crate::write_control::WriteControl;
use crate::write_rate::WriteRateLimiter;

// --- shared helpers (users/audit/RBAC - copied from chronogazer/relay-wright's rest.rs) ---

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
}

/// `banto_server::require_auth`'s own 401 body, reproduced here for
/// `require_tag_space_auth` (T0-2's `/api/v1/*` auth middleware below) since
/// that middleware replaces `require_auth` entirely rather than wrapping it.
fn unauthorized_response() -> Response {
    (StatusCode::UNAUTHORIZED, Json(ErrorBody::Unauthorized)).into_response()
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

/// T2-4（設計 §6-4「トリップ」）: トリップ中の API キーでの
/// `/api/v1/*` アクセス - read/write いずれも 403。
/// `crate::rest::require_tag_space_auth` から呼ぶ。
fn key_tripped_response() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({ "error": "key_tripped" })),
    )
        .into_response()
}

/// T2-4（設計 §6-8、実装指示 §5「認証」）: `POST /api/v1/values/{tag}` を
/// セッション token で叩いた場合の明示的な 403（§6-8: 「セッション token
/// では書けない」）。
fn session_token_cannot_write_response() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "error": "session_token_cannot_write",
            "message": "書き込みは write:{tag} スコープを持つ API キーでのみ可能です。管理 UI のセッション token では書き込めません。"
        })),
    )
        .into_response()
}

/// T2-4（実装指示 §5「認証」）: `write:{tag}` スコープの完全一致を持たない
/// API キー（`read` のみ、または別タグの `write:` スコープ）での書き込み
/// 試行。
fn missing_write_scope_response() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({ "error": "missing_write_scope" })),
    )
        .into_response()
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

// --- API キー管理 (docs/tag-server-design.md §5.6・T0-2 実装指示 §1「管理
// REST」): admin ロール限定、CSRF + bearer セッション（このルーター自体は
// `crate::api_keys::ApiKeysService`（発行される bh_ キー）を消費する側では
// なく、管理 UI セッションから叩く前提 - `/api/v1/*` の API キー認証とは
// 別物）。---------------------------------------------------------------------

#[derive(Clone)]
struct ApiKeysAdminState {
    api_keys: ApiKeysService,
    auth: AuthState,
    audit: AuditLogService,
}

#[derive(Debug, Deserialize)]
struct CreateApiKeyRequest {
    name: String,
    scopes: Vec<String>,
}

/// `POST /api/api-keys` の応答 - `IssuedApiKey` をそのまま返すと `key`
/// フィールド名がスネークケースのままになる（`crate::api_keys` は機械
/// クライアント向け `/api/v1/*` と同じ snake_case 規約）ので、それに
/// 合わせてここでも変換なしでそのまま公開する（T0-2 実装指示の応答例
/// `{ "id", "name", "prefix", "scopes", "key": "bh_..." }` と一致）。
#[derive(Debug, Serialize, ToSchema)]
struct IssuedApiKeyResponse {
    id: i64,
    name: String,
    prefix: String,
    scopes: Vec<String>,
    /// 平文キー全体。この応答限りでしか手に入らない（設計: 「key はこの
    /// 応答限り」）。
    key: String,
}

impl From<IssuedApiKey> for IssuedApiKeyResponse {
    fn from(issued: IssuedApiKey) -> Self {
        Self {
            id: issued.id,
            name: issued.name,
            prefix: issued.prefix,
            scopes: issued.scopes,
            key: issued.key,
        }
    }
}

/// `POST /api/api-keys` - 発行。監査ログには **キー平文・ハッシュを
/// 含めない**（設計 T0-2 実装指示: 「監査ログに record_write — ただし
/// キー平文・ハッシュは監査 detail に入れない」）。
async fn api_keys_create(
    State(state): State<ApiKeysAdminState>,
    headers: HeaderMap,
    Json(body): Json<CreateApiKeyRequest>,
) -> Result<(StatusCode, Json<IssuedApiKeyResponse>), ApiError> {
    let issued = state.api_keys.issue(&body.name, body.scopes).await?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "create",
        "api_keys",
        &issued.id.to_string(),
        Some(json!({ "name": issued.name, "scopes": issued.scopes })),
    )
    .await;
    Ok((StatusCode::CREATED, Json(issued.into())))
}

/// `GET /api/api-keys` - 一覧（`key_hash` は含まない、設計: 「key_hash は
/// 返さない」）。
async fn api_keys_list(
    State(state): State<ApiKeysAdminState>,
) -> Result<Json<Vec<crate::api_keys::ApiKeySummary>>, ApiError> {
    Ok(Json(state.api_keys.list().await?))
}

/// `POST /api/api-keys/{id}/revoke` - 失効（冪等、設計: 「DELETE は設けない
/// （失効履歴を残す方針）」）。
async fn api_keys_revoke(
    State(state): State<ApiKeysAdminState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<crate::api_keys::ApiKeySummary>, ApiError> {
    let summary = state.api_keys.revoke(id).await?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "revoke",
        "api_keys",
        &id.to_string(),
        None,
    )
    .await;
    Ok(Json(summary))
}

/// `POST /api/api-keys/{id}/clear-trip` - トリップ解除（冪等、T2-4・
/// 設計 §6-4「復帰は管理 UI から手動」）。`revoke` と違い `tripped_at` は
/// `NULL` に戻せる - `crate::api_keys` のモジュール doc comment「トリップ」
/// 参照。
async fn api_keys_clear_trip(
    State(state): State<ApiKeysAdminState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<crate::api_keys::ApiKeySummary>, ApiError> {
    let summary = state.api_keys.clear_trip(id).await?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "clear_trip",
        "api_keys",
        &id.to_string(),
        None,
    )
    .await;
    Ok(Json(summary))
}

/// `/api/api-keys/*`（設計 §5.6・T0-2 実装指示: 「管理系ルーターに追加 —
/// CSRF + bearer + RBAC admin 限定」）。T0-2 実装指示は発行/一覧/失効
/// いずれも admin 限定と明記しているため、[`require_editor`]（editor 以上）
/// ではなく [`RoleGuard`]（admin ちょうど）をルーター全体に掛ける -
/// `users_router`/`audit_log_router` と同型（ハンドラ内で個別に role
/// チェックし直さない: 到達した時点で呼び出し元は admin であることが
/// ルーター層で保証済み）。
fn api_keys_router(api_keys: ApiKeysService, audit: AuditLogService, auth: AuthState) -> Router {
    let state = ApiKeysAdminState {
        api_keys,
        auth: auth.clone(),
        audit: audit.clone(),
    };
    Router::new()
        .route("/api/api-keys", get(api_keys_list).post(api_keys_create))
        .route("/api/api-keys/{id}/revoke", post(api_keys_revoke))
        .route("/api/api-keys/{id}/clear-trip", post(api_keys_clear_trip))
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            RoleGuard {
                auth: auth.clone(),
                min: Role::Admin,
                resource: "api_keys",
                audit,
            },
            require_role_at_least,
        ))
        .layer(middleware::from_fn_with_state(auth, require_auth))
}

// --- 書き込み受付トグル (T2-4、設計 §6-6): admin 限定、CSRF + bearer -------
//
// `POST /api/write-control/enable`/`disable` は
// `crate::write_control::WriteControl`（ライブフラグ、起動時 disabled）を
// 切り替え、`crate::write_control::persist_enabled` で表示専用の永続値も
// 更新する（`WriteControl` のモジュール doc comment 参照 - 永続値は次回
// 起動時のライブフラグには一切影響しない）。

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
#[derive(Debug, Serialize, ToSchema)]
struct WriteControlStatusResponse {
    write_enabled: bool,
    write_was_enabled_before_restart: bool,
}

async fn write_control_set(
    state: &WriteControlAdminState,
    headers: &HeaderMap,
    enabled: bool,
    action: &str,
) -> Json<WriteControlStatusResponse> {
    if enabled {
        state.write_control.enable();
    } else {
        state.write_control.disable();
    }

    let identity = actor_identity(headers, &state.auth);
    if let Err(err) = crate::write_control::persist_enabled(
        &state.manager.pool(),
        enabled,
        identity.as_ref().map(|i| i.id.as_str()),
    )
    .await
    {
        eprintln!("banto-hub: 書き込み受付状態の永続化に失敗しました: {err}");
    }

    record_write(
        &state.audit,
        &state.auth,
        headers,
        action,
        "write_control",
        "1",
        Some(json!({ "enabled": enabled })),
    )
    .await;
    let _ = state.events.send(ServerEvent::ResourceChanged {
        resource: "write_control".to_string(),
    });

    Json(WriteControlStatusResponse {
        write_enabled: state.write_control.is_enabled(),
        write_was_enabled_before_restart: state.write_control.was_enabled_before_restart(),
    })
}

async fn write_control_enable(
    State(state): State<WriteControlAdminState>,
    headers: HeaderMap,
) -> Json<WriteControlStatusResponse> {
    write_control_set(&state, &headers, true, "enable").await
}

async fn write_control_disable(
    State(state): State<WriteControlAdminState>,
    headers: HeaderMap,
) -> Json<WriteControlStatusResponse> {
    write_control_set(&state, &headers, false, "disable").await
}

fn write_control_router(
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
    Router::new()
        .route("/api/write-control/enable", post(write_control_enable))
        .route("/api/write-control/disable", post(write_control_disable))
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            RoleGuard {
                auth: auth.clone(),
                min: Role::Admin,
                resource: "write_control",
                audit,
            },
            require_role_at_least,
        ))
        .layer(middleware::from_fn_with_state(auth, require_auth))
}

// --- MQTT 設定 (T3、設計 §5.3): admin 限定、CSRF + bearer -------------------
//
// `GET/PUT /api/mqtt-settings`（実装指示どおり）。`write_control_router`と
// 同型: admin ロール限定 + CSRF(`require_banto_client_header`は`api_router`
// 側で管理系ルーター全体に一括で被せる) + `PUT`成功で
// `crate::mqtt::MqttPublisher::apply`を呼んで即時適用する（実装指示「保存で
// 即時適用」）。

#[derive(Clone)]
struct MqttSettingsAdminState {
    manager: Arc<CollectorManager>,
    mqtt: Arc<MqttPublisher>,
    auth: AuthState,
    audit: AuditLogService,
    events: broadcast::Sender<ServerEvent>,
}

/// `GET/PUT /api/mqtt-settings`の request/response body。admin-UI 向け
/// リソースの流儀（`ApiKeySummary`等）に合わせて camelCase。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MqttSettingsRequest {
    enabled: bool,
    host: String,
    port: u16,
    client_id: String,
    #[serde(default)]
    username: Option<String>,
    /// 空文字は「変更なし」（既存のパスワードを維持）- 実装指示どおり
    /// （`crate::settings::MqttSettings`のフィールド doc comment参照）。
    /// `GET`はパスワードを一切返さないので、UI が「今の値」を知らずに
    /// フォームを保存しても上書きされない。
    #[serde(default)]
    password: Option<String>,
    prefix: String,
    qos: u8,
    min_interval_ms: i64,
}

/// `password`フィールドが**存在しない**- 実装指示「password は GET で
/// 返さない」をレスポンス型そのもので保証する（`serde`のシリアライズ対象
/// フィールドに含めていないので、実装ミスで漏らしようがない）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct MqttSettingsResponse {
    enabled: bool,
    host: String,
    port: u16,
    client_id: String,
    username: Option<String>,
    prefix: String,
    qos: u8,
    min_interval_ms: i64,
}

impl From<MqttSettings> for MqttSettingsResponse {
    fn from(config: MqttSettings) -> Self {
        Self {
            enabled: config.enabled,
            host: config.host,
            port: config.port,
            client_id: config.client_id,
            username: config.username,
            prefix: config.prefix,
            qos: config.qos,
            min_interval_ms: config.min_interval_ms,
        }
    }
}

async fn mqtt_settings_get(
    State(state): State<MqttSettingsAdminState>,
) -> Result<Json<MqttSettingsResponse>, ApiError> {
    let config = SettingsService::new(state.manager.pool())
        .mqtt_config()
        .await?;
    Ok(Json(config.into()))
}

/// 入力検証（設計 §5.3 の制約をそのまま反映）:
/// - `qos`は0/1のみ（「2は使わない」）
/// - `min_interval_ms`は0以上
/// - `enabled=true`のときは`host`必須（無効化するだけなら未入力のままでよい）
fn validate_mqtt_settings_request(body: &MqttSettingsRequest) -> Vec<FieldError> {
    let mut errors = Vec::new();
    if body.qos > 1 {
        errors.push(FieldError {
            field: "qos".to_string(),
            message: "qos は 0 または 1 のみ対応しています(2 は使いません)".to_string(),
        });
    }
    if body.min_interval_ms < 0 {
        errors.push(FieldError {
            field: "minIntervalMs".to_string(),
            message: "minIntervalMs は 0 以上である必要があります".to_string(),
        });
    }
    if body.enabled && body.host.trim().is_empty() {
        errors.push(FieldError {
            field: "host".to_string(),
            message: "MQTT を有効にする場合は host が必須です".to_string(),
        });
    }
    if body.client_id.trim().is_empty() {
        errors.push(FieldError {
            field: "clientId".to_string(),
            message: "clientId は必須です".to_string(),
        });
    }
    if body.prefix.trim().is_empty() {
        errors.push(FieldError {
            field: "prefix".to_string(),
            message: "prefix は必須です".to_string(),
        });
    }
    errors
}

async fn mqtt_settings_put(
    State(state): State<MqttSettingsAdminState>,
    headers: HeaderMap,
    Json(body): Json<MqttSettingsRequest>,
) -> Result<Json<MqttSettingsResponse>, ApiError> {
    let field_errors = validate_mqtt_settings_request(&body);
    if !field_errors.is_empty() {
        return Err(ApiError(BantoError::Validation { field_errors }));
    }

    let settings_service = SettingsService::new(state.manager.pool());
    // 空文字パスワードは「変更なし」- 現在の永続値を読んでフォールバック
    // する（`MqttSettingsRequest::password`のdoc comment参照）。
    let existing = settings_service.mqtt_config().await?;
    let password = match body.password.as_deref() {
        Some(value) if !value.is_empty() => Some(value.to_string()),
        _ => existing.password,
    };

    let config = MqttSettings {
        enabled: body.enabled,
        host: body.host,
        port: body.port,
        client_id: body.client_id,
        username: body.username.filter(|value| !value.is_empty()),
        password,
        prefix: body.prefix,
        qos: body.qos,
        min_interval_ms: body.min_interval_ms,
    };
    settings_service.set_mqtt_config(&config).await?;

    // 実装指示「保存で即時適用」- CollectorManager::rebuild と同じ「古い
    // タスクを止めて新しいタスク」パターン（`crate::mqtt`のモジュール doc
    // comment参照）。
    state.mqtt.apply(&config).await;

    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "update",
        "mqtt_settings",
        "1",
        Some(json!({ "enabled": config.enabled })),
    )
    .await;
    let _ = state.events.send(ServerEvent::ResourceChanged {
        resource: "mqtt_settings".to_string(),
    });

    Ok(Json(config.into()))
}

fn mqtt_settings_router(
    manager: Arc<CollectorManager>,
    mqtt: Arc<MqttPublisher>,
    audit: AuditLogService,
    auth: AuthState,
    events: broadcast::Sender<ServerEvent>,
) -> Router {
    let state = MqttSettingsAdminState {
        manager,
        mqtt,
        auth: auth.clone(),
        audit: audit.clone(),
        events,
    };
    Router::new()
        .route(
            "/api/mqtt-settings",
            get(mqtt_settings_get).put(mqtt_settings_put),
        )
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            RoleGuard {
                auth: auth.clone(),
                min: Role::Admin,
                resource: "mqtt_settings",
                audit,
            },
            require_role_at_least,
        ))
        .layer(middleware::from_fn_with_state(auth, require_auth))
}

// --- gRPC 設定 (T4、設計 §5.4): admin 限定、CSRF + bearer -------------------
//
// `GET/PUT /api/grpc-settings`（実装指示どおり）。`mqtt_settings_router` と
// 同型: admin ロール限定 + CSRF + `PUT` 成功で `crate::grpc::GrpcServer::apply`
// を呼んで即時適用する（実装指示「保存で即時適用 - MqttPublisher と同じ
// 再起動可能マネージャパターン」）。MQTT と違い認証情報(パスワード等)を
// 持たないため、`MqttSettingsRequest`ほど複雑な「変更なし」フィールドの
// 扱いは不要 - `enabled`/`port` の2値をそのまま読み書きする。

#[derive(Clone)]
struct GrpcSettingsAdminState {
    manager: Arc<CollectorManager>,
    grpc_server: Arc<crate::grpc::GrpcServer>,
    auth: AuthState,
    audit: AuditLogService,
    events: broadcast::Sender<ServerEvent>,
}

/// `GET/PUT /api/grpc-settings`の request/response body。
#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct GrpcSettingsBody {
    enabled: bool,
    port: u16,
}

impl From<crate::settings::GrpcSettings> for GrpcSettingsBody {
    fn from(config: crate::settings::GrpcSettings) -> Self {
        Self {
            enabled: config.enabled,
            port: config.port,
        }
    }
}

async fn grpc_settings_get(
    State(state): State<GrpcSettingsAdminState>,
) -> Result<Json<GrpcSettingsBody>, ApiError> {
    let config = SettingsService::new(state.manager.pool())
        .grpc_config()
        .await?;
    Ok(Json(config.into()))
}

async fn grpc_settings_put(
    State(state): State<GrpcSettingsAdminState>,
    headers: HeaderMap,
    Json(body): Json<GrpcSettingsBody>,
) -> Result<Json<GrpcSettingsBody>, ApiError> {
    if body.port == 0 {
        return Err(ApiError(BantoError::Validation {
            field_errors: vec![FieldError {
                field: "port".to_string(),
                message: "port は 1〜65535 で指定してください".to_string(),
            }],
        }));
    }

    let settings_service = SettingsService::new(state.manager.pool());
    let config = crate::settings::GrpcSettings {
        enabled: body.enabled,
        port: body.port,
    };
    settings_service.set_grpc_config(&config).await?;

    // 実装指示「保存で即時適用」- `crate::mqtt::MqttPublisher::apply` と
    // 同じ「古いタスクを止めて新しいタスク」パターン（`crate::grpc`の
    // モジュール doc comment参照）。
    state.grpc_server.apply(&config).await;

    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "update",
        "grpc_settings",
        "1",
        Some(json!({ "enabled": config.enabled, "port": config.port })),
    )
    .await;
    let _ = state.events.send(ServerEvent::ResourceChanged {
        resource: "grpc_settings".to_string(),
    });

    Ok(Json(config.into()))
}

fn grpc_settings_router(
    manager: Arc<CollectorManager>,
    grpc_server: Arc<crate::grpc::GrpcServer>,
    audit: AuditLogService,
    auth: AuthState,
    events: broadcast::Sender<ServerEvent>,
) -> Router {
    let state = GrpcSettingsAdminState {
        manager,
        grpc_server,
        auth: auth.clone(),
        audit: audit.clone(),
        events,
    };
    Router::new()
        .route(
            "/api/grpc-settings",
            get(grpc_settings_get).put(grpc_settings_put),
        )
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            RoleGuard {
                auth: auth.clone(),
                min: Role::Admin,
                resource: "grpc_settings",
                audit,
            },
            require_role_at_least,
        ))
        .layer(middleware::from_fn_with_state(auth, require_auth))
}

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
fn write_audit_router(
    write_audit: WriteAuditService,
    audit: AuditLogService,
    auth: AuthState,
) -> Router {
    let state = WriteAuditAdminState { write_audit };
    Router::new()
        .route("/api/write-audit/list", post(write_audit_list))
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            RoleGuard {
                auth: auth.clone(),
                min: Role::Admin,
                resource: "write_audit",
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

/// T2-3: mirrors `banto_tags::tag`'s own `default_tag_kind` (not reused
/// directly - that one is private to `banto-tags`) so a `TagPayload` missing
/// `tagKind` builds the same `"plc"` `TagInput` an old client always got.
fn default_tag_kind() -> String {
    "plc".to_string()
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
    /// T2-3 (docs/tag-server-design.md §10-2/§6 item 1): `#[serde(default)]`
    /// (= `false`) so an existing API client's payload (written before this
    /// field existed) still deserializes and creates a non-writable tag,
    /// exactly the pre-T2 behaviour (design §10-2: "既存の API クライアント
    /// のペイロードは無変更で通る").
    #[serde(default)]
    pub writable: bool,
    #[serde(default = "default_tag_kind")]
    pub tag_kind: String,
    #[serde(default)]
    pub expression: Option<String>,
    #[serde(default)]
    pub retain: bool,
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
            writable: payload.writable,
            tag_kind: payload.tag_kind,
            expression: payload.expression,
            retain: payload.retain,
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
//
// T0-2（設計 §10-6、utoipa 採用 2026-08-04 決定）: 以下の応答型はすべて
// `Serialize` + `utoipa::ToSchema` を derive し、`#[utoipa::path]` で
// `ApiDoc`（このセクション末尾）にまとめて `GET /api/v1/openapi.json` で
// 配信する。**wire 形式（フィールド名・JSON 形）は T0-1 から一切変えて
// いない** - 型を導入しただけで、実際に流れる JSON は
// `apps/banto-hub/core/tests/integration.rs` の T0-1 時点のアサーションと
// 完全に同じ（このセクションの各関数 doc comment に旧来の json! リテラルの
// 形を残してあるのはそのため）。

/// `pub(crate)` (not private): `crate::stream`'s `GET /api/v1/stream` handler
/// is mounted directly onto [`tag_space_router`]'s `Router` (same
/// `require_tag_space_auth` layer, no separate router/state - T1 実装指示
/// §9「既存の require_tag_space_auth を /api/v1/stream にも適用」の最も
/// 単純な実現), so it needs this state type too.
#[derive(Clone)]
pub(crate) struct TagSpaceState {
    pub(crate) manager: Arc<CollectorManager>,
    /// T2-4（設計 §6-6）: `GET /api/v1/status` の `write_enabled`/
    /// `write_was_enabled_before_restart` のため。
    pub(crate) write_control: Arc<WriteControl>,
    /// T3（設計 §5.3）: `GET /api/v1/status` の `mqtt.connected` のため。
    pub(crate) mqtt: Arc<MqttPublisher>,
}

#[derive(Debug, Deserialize)]
struct TagsQuery {
    connection: Option<String>,
    group: Option<String>,
}

/// `GET /api/v1/tags` の応答: `{ "revision", "tags": [TagEntry...] }`。
#[derive(Debug, Serialize, ToSchema)]
struct CatalogResponse {
    revision: u64,
    tags: Vec<TagEntry>,
}

/// `GET /api/v1/tags` - catalog: `{ "revision", "tags": [TagEntry...] }`,
/// optionally filtered by `?connection=`/`?group=` (matched against the
/// entry's connection/group *name*, design §5.1's route table).
#[utoipa::path(
    get,
    path = "/api/v1/tags",
    params(
        ("connection" = Option<String>, Query, description = "接続名で絞り込む"),
        ("group" = Option<String>, Query, description = "収集グループ名で絞り込む"),
    ),
    responses((status = 200, description = "catalog スナップショット", body = CatalogResponse)),
    tag = "tag-space",
)]
async fn v1_tags(
    State(state): State<TagSpaceState>,
    Query(query): Query<TagsQuery>,
) -> Json<CatalogResponse> {
    let map = state.manager.tag_map();
    let revision = state.manager.revision();
    let tags: Vec<TagEntry> = map
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
        .cloned()
        .collect();
    Json(CatalogResponse { revision, tags })
}

/// One `/api/v1/values*` entry's wire shape (design §5.1's route table:
/// `{ "tag", "v", "q", "t" }`).
#[derive(Debug, Clone, Serialize, ToSchema)]
struct ValueEntry {
    tag: String,
    v: Option<f64>,
    q: String,
    t: i64,
}

/// Thin wire-formatting wrapper over [`crate::hub::effective_sample`] (see
/// its doc comment - this is the "same v/q/t semantics as `crate::stream`'s
/// `data` messages" helper the T1 実装指示 asked to share rather than
/// duplicate).
fn value_entry(
    external_name: &str,
    entry: &TagEntry,
    sample: Option<banto_collect::CurrentSample>,
    now_ms: i64,
) -> ValueEntry {
    let (v, q, t) = crate::hub::effective_sample(entry, sample, now_ms);
    ValueEntry {
        tag: external_name.to_string(),
        v,
        q: crate::hub::quality_str(q).to_string(),
        t,
    }
}

#[derive(Debug, Deserialize)]
struct ValuesQuery {
    tags: Option<String>,
}

/// `GET /api/v1/values` の応答: `{ "revision", "t", "values": [ValueEntry...] }`。
#[derive(Debug, Serialize, ToSchema)]
struct ValuesResponse {
    revision: u64,
    t: i64,
    values: Vec<ValueEntry>,
}

/// `GET /api/v1/values` - full or partial (`?tags=a,b,c`) snapshot.
///
/// An unknown name in `?tags=` is a `400` enumerating every unresolved name
/// (design instructions: 「未知の名前が混ざったら...部分成功で誤解させない」),
/// never a per-row `bad`/`unknown_tag` - the request as a whole is rejected
/// so the caller cannot mistake "misspelled tag" for "tag exists but is
/// currently bad". (This error body stays a raw `serde_json::Value` - only
/// the *successful* `/api/v1/*` bodies were in scope for the T0-2 typed-struct
/// conversion.)
#[utoipa::path(
    get,
    path = "/api/v1/values",
    params(
        ("tags" = Option<String>, Query, description = "カンマ区切りの外部名。省略時は全タグ"),
    ),
    responses(
        (status = 200, description = "現在値スナップショット", body = ValuesResponse),
        (status = 400, description = "?tags= に未知の外部名が含まれる"),
    ),
    tag = "tag-space",
)]
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

    let values: Vec<ValueEntry> = names
        .iter()
        .filter_map(|name| map.get(name).map(|entry| (name, entry)))
        .map(|(name, entry)| {
            let sample = current.as_ref().and_then(|c| c.get(&entry.tag_key));
            value_entry(name, entry, sample, now_ms)
        })
        .collect();

    Json(ValuesResponse {
        revision,
        t: now_ms,
        values,
    })
    .into_response()
}

/// `GET /api/v1/values/{tag}` - single tag. `404` only when the external
/// name is not in the catalog at all (design: 「404 になるのは定義が存在
/// しない外部名のみ」) - an undefined-but-uncollected tag is `200` with
/// `q: "bad"`.
#[utoipa::path(
    get,
    path = "/api/v1/values/{tag}",
    params(("tag" = String, Path, description = "外部名 {connection}.{group}.{tag}")),
    responses(
        (status = 200, description = "単一タグの現在値", body = ValueEntry),
        (status = 404, description = "catalog に存在しない外部名"),
    ),
    tag = "tag-space",
)]
async fn v1_value_single(
    State(state): State<TagSpaceState>,
    Path(tag): Path<String>,
) -> Result<Json<ValueEntry>, ApiError> {
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
    Ok(Json(value_entry(&tag, entry, sample, now_ms)))
}

/// `GET /api/v1/status` の `connections` 配列1件分。
#[derive(Debug, Serialize, ToSchema)]
struct ConnectionStatusEntry {
    name: String,
    id: i64,
    status: String,
    attempt: Option<u32>,
}

/// `GET /api/v1/status` の `mqtt`（T3、設計実装指示「`/api/v1/status` に
/// `mqtt: { "enabled": bool, "connected": bool }` を追加」）。`enabled` は
/// 設定値（settings テーブルの `mqtt.enabled`）、`connected` は
/// [`crate::mqtt::MqttPublisher::connected`]（実際に MQTT ブローカーへ接続
/// できているかのライブ状態）- 両者は独立: `enabled=true` でもブローカー
/// 不通なら `connected=false` になる。
#[derive(Debug, Serialize, ToSchema)]
struct MqttStatusEntry {
    enabled: bool,
    connected: bool,
}

/// `GET /api/v1/status` の `grpc`（T4、設計実装指示「`/api/v1/status` に
/// `grpc: { enabled, port }` を追加」）。MQTT と違い「実際に接続できて
/// いるか」のライブ状態は持たない - gRPC サーバーは外部へ接続しに行く
/// クライアントではなく listen するだけなので、設定値がそのまま意図した
/// 状態を表す(`crate::grpc::GrpcServer`のモジュール doc comment参照)。
#[derive(Debug, Serialize, ToSchema)]
struct GrpcStatusEntry {
    enabled: bool,
    port: u16,
}

/// `GET /api/v1/status` の応答。
#[derive(Debug, Serialize, ToSchema)]
struct StatusResponse {
    version: String,
    revision: u64,
    last_config_error: Option<String>,
    connections: Vec<ConnectionStatusEntry>,
    /// T2-4（設計 §6-6）: 書き込み受付が今いま有効かどうか(ライブフラグ)。
    write_enabled: bool,
    /// T2-4（設計 §6-6）: プロセス再起動前は有効だったか(表示専用の履歴 -
    /// `crate::write_control::WriteControl` のモジュール doc comment
    /// 参照。ライブの `write_enabled` には一切影響しない)。
    write_was_enabled_before_restart: bool,
    /// T3（設計 §5.3）: MQTT publish の設定/接続状態。
    mqtt: MqttStatusEntry,
    /// T4（設計 §5.4）: gRPC サーバーの設定。
    grpc: GrpcStatusEntry,
}

/// `GET /api/v1/status` - `{ "version", "revision", "last_config_error",
/// "connections": [...] }` (design §5.1's route table). Connection names
/// come from the registry directly (not the catalog) so a connection with
/// zero tags still appears.
///
/// T2-2 (docs/tag-server-design.md §6-5, 2026-08-05 決定): an SLMP
/// connection's status comes from
/// [`crate::hub::CollectorManager::broker_status`] (the broker's own
/// `ConnState`) instead of `banto_collect`'s own status map - see
/// `crate::broker_glue`'s module doc ("The two-backoff double bookkeeping")
/// for why the broker's answer is the one that reflects whether the physical
/// session is actually up for a broker-managed connection. Modbus connections
/// are unaffected and keep reading from `banto_collect::Collector::status`.
#[utoipa::path(
    get,
    path = "/api/v1/status",
    responses((status = 200, description = "サーバー状態", body = StatusResponse)),
    tag = "tag-space",
)]
async fn v1_status(State(state): State<TagSpaceState>) -> Result<Json<StatusResponse>, ApiError> {
    let revision = state.manager.revision();
    let last_config_error = state.manager.last_error();
    let statuses = state.manager.connection_status();

    let connections = PlcConnectionService::new(state.manager.pool())
        .list(ListParams::default())
        .await?
        .rows;
    let mqtt_settings = SettingsService::new(state.manager.pool())
        .mqtt_config()
        .await?;
    let grpc_settings = SettingsService::new(state.manager.pool())
        .grpc_config()
        .await?;

    let entries: Vec<ConnectionStatusEntry> = connections
        .into_iter()
        .map(|conn| {
            let (status_str, attempt) = if conn.protocol == "slmp" {
                match state.manager.broker_status(conn.id) {
                    Some(BrokerConnectionStatus::Connected) => ("connected", None),
                    Some(BrokerConnectionStatus::Reconnecting { attempt }) => {
                        ("reconnecting", Some(attempt))
                    }
                    // "stopped" also covers "no broker session yet" (no
                    // rebuild has run, or this connection is currently
                    // disabled) - same rounding banto_collect's own
                    // ConnectionStatus branch below uses.
                    Some(BrokerConnectionStatus::Stopped) | None => ("stopped", None),
                }
            } else {
                let key = format!("conn:{}", conn.id);
                match statuses.get(&key) {
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
                }
            };
            ConnectionStatusEntry {
                name: conn.name,
                id: conn.id,
                status: status_str.to_string(),
                attempt,
            }
        })
        .collect();

    Ok(Json(StatusResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
        revision,
        last_config_error,
        connections: entries,
        write_enabled: state.write_control.is_enabled(),
        write_was_enabled_before_restart: state.write_control.was_enabled_before_restart(),
        mqtt: MqttStatusEntry {
            enabled: mqtt_settings.enabled,
            connected: state.mqtt.connected(),
        },
        grpc: GrpcStatusEntry {
            enabled: grpc_settings.enabled,
            port: grpc_settings.port,
        },
    }))
}

#[derive(Debug, Deserialize)]
struct EventsQuery {
    from_ms: Option<i64>,
    to_ms: Option<i64>,
    limit: Option<i64>,
}

/// `GET /api/v1/events` の `events` 配列1件分
/// (`crates/banto-collect/migrations/0001_collect_events.sql`'s columns)。
#[derive(Debug, Serialize, ToSchema)]
struct EventEntry {
    id: i64,
    ts: i64,
    kind: String,
    connection_key: Option<String>,
    tag_key: Option<String>,
    level: Option<String>,
    value: Option<f64>,
    detail: Option<String>,
}

/// `GET /api/v1/events` の応答。
#[derive(Debug, Serialize, ToSchema)]
struct EventsResponse {
    events: Vec<EventEntry>,
}

/// `GET /api/v1/events` - range query over `collect_events`, newest first,
/// default `limit` 100 (clamped to a sane range so a misbehaving client
/// cannot force an unbounded scan).
#[utoipa::path(
    get,
    path = "/api/v1/events",
    params(
        ("from_ms" = Option<i64>, Query, description = "範囲の下限（epoch ms、既定 0）"),
        ("to_ms" = Option<i64>, Query, description = "範囲の上限（epoch ms、既定は無制限）"),
        ("limit" = Option<i64>, Query, description = "最大件数（既定 100、1〜1000 にクランプ）"),
    ),
    responses((status = 200, description = "collect_events の範囲クエリ結果", body = EventsResponse)),
    tag = "tag-space",
)]
async fn v1_events(
    State(state): State<TagSpaceState>,
    Query(query): Query<EventsQuery>,
) -> Result<Json<EventsResponse>, ApiError> {
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
        "SELECT id, ts, kind, connection_key, tag_key, level, value, detail          FROM collect_events WHERE ts >= ? AND ts <= ? ORDER BY ts DESC, id DESC LIMIT ?",
    )
    .bind(from_ms)
    .bind(to_ms)
    .bind(limit)
    .fetch_all(&state.manager.pool())
    .await
    .map_err(banto_storage::storage_error)?;

    let events: Vec<EventEntry> = rows
        .into_iter()
        .map(
            |(id, ts, kind, connection_key, tag_key, level, value, detail)| EventEntry {
                id,
                ts,
                kind,
                connection_key,
                tag_key,
                level,
                value,
                detail,
            },
        )
        .collect();

    Ok(Json(EventsResponse { events }))
}

// --- 書き込みエンドポイント (T2-4、設計 §5.1「POST /api/v1/values/{tag}」・
// §6 全体) -------------------------------------------------------------
//
// ゲート順・監査・レート制限・受付トグルの規律は relay-wright の
// `engine/writer.rs` を直接の下敷きにしている（§6 実装指示: 「参考実装は
// relay-wright の engine/{writer,rate_limiter,arming,write_audit}.rs（設計は
// 流用、コードは hub 語彙で書き直し）」）。hub 固有の差分は主に3点:
// - relay-wright の `Writer` はルールエンジンが生成する `PendingWrite` を
//   1タスクが順に処理する専有構造だが、hub は複数の REST/gRPC リクエストが
//   並行に飛んでくるので、[`WriteRateLimiter`] を `tokio::sync::Mutex` で
//   包んで共有する（peek→exceed 判定→(ゲート通過なら)record を1つの
//   ロック区間に収めず、ロックは短時間だけ握って離す - 詳細は
//   `crate::write_path::execute_write` 本体のコメント参照）。
// - relay-wright の disarm は「ルールエンジン全体」を止めるが、hub の
//   `WriteControl` は書き込みエンドポイントの受付可否のみを制御する
//   （収集自体は止めない）。
// - 認証主体が「ルール」ではなく「API キー」なので、監査行の主語も
//   `api_key_id`/`api_key_name_snapshot`（`crate::write_audit` 参照）。
//
// **T4（設計 §5.4）でゲート1〜8の本体を `crate::write_path::execute_write`
// へ抽出した** - gRPC の `WriteValue`（`crate::grpc`）が REST と同一の
// ゲート・監査・レート制限を通る必要があり（実装指示「二重実装は絶対に
// 不可」）、以降このモジュールに残るのは REST 固有の前段（セッション token
// 拒否・JSON body のパース）と、`WriteRejection` を HTTP ステータス + JSON
// へ変換する [`write_rejection_response`] のみ。

/// `POST /api/v1/values/{tag}` の request body（設計 §5.1「body
/// `{ "v": <number|bool> }`」）。`v` は `serde_json::Value` のまま保持し、
/// [`parse_requested_value`] で数値/真偽値のみを受理する（文字列・配列・
/// オブジェクトは 422）— `bool` と `number` を1つの Rust 型に素直に
/// マップする serde 表現がなく、utoipa の untagged enum サポートも弱いため、
/// 検証はハンドラ内で行う。
#[derive(Debug, Deserialize, ToSchema)]
struct WriteValueRequest {
    /// 書き込む工学値。数値、または bit タグ向けの真偽値
    /// （真偽値は 1.0/0.0 として扱う）。
    #[schema(value_type = f64, example = 1)]
    v: serde_json::Value,
}

/// `POST /api/v1/values/{tag}` の成功応答（設計 §6 実装指示 §5「応答
/// `{ "tag", "result": "ok" }`」）。gRPC の `WriteValueResponse`
/// （`crate::grpc`）も同じ2フィールドの型付き版。
#[derive(Debug, Serialize, ToSchema)]
struct WriteValueResponse {
    tag: String,
    result: String,
}

/// リクエスト body の `v` を工学値の `f64` に正規化する。`bool` は
/// `1.0`/`0.0`、数値はそのまま。文字列・配列・オブジェクト・`null` は
/// `None`（呼び出し元が 422 `unsupported_value_type` を返す）。REST 固有の
/// wire 形式（JSON の `v`）からの変換であり、gRPC 側は `oneof num|bool` を
/// 直接分解するだけで済むため、この関数は `crate::write_path` へは移して
/// いない。
fn parse_requested_value(v: &serde_json::Value) -> Option<f64> {
    if let Some(b) = v.as_bool() {
        Some(if b { 1.0 } else { 0.0 })
    } else {
        v.as_f64()
    }
}

/// [`crate::write_path::WriteRejection`] を REST の HTTP ステータス + JSON
/// 本文へ変換する（gRPC 側の対応物は `crate::grpc::write_rejection_status`）。
/// `NotFound`（404）だけは特別扱いする - `crate::write_path` は transport に
/// 依存しない設計上、`banto_core::BantoError`/`ApiError` を一切知らないため、
/// ここで `ApiError(BantoError::NotFound { .. })` へ組み立て直す。それ以外は
/// `WriteRejection::rest_error_code`/`detail`（gate の意味論のみを持つ純粋な
/// 情報）から機械的に HTTP ステータスを割り当てる - 対応表は
/// `crate::write_path::WriteRejection` の doc comment に列挙したものと一致
/// させる。
fn write_rejection_response(tag: String, rejection: crate::write_path::WriteRejection) -> Response {
    use crate::write_path::WriteRejection;

    if matches!(rejection, WriteRejection::NotFound) {
        return ApiError(BantoError::NotFound {
            resource: "tags".to_string(),
            id: tag,
        })
        .into_response();
    }

    let status = match &rejection {
        WriteRejection::NotFound => unreachable!("上で特別扱い済み"),
        WriteRejection::NotWritable => StatusCode::FORBIDDEN,
        WriteRejection::TagDisabled => StatusCode::CONFLICT,
        WriteRejection::UnsupportedProtocol => StatusCode::NOT_IMPLEMENTED,
        WriteRejection::WritesDisabled => StatusCode::SERVICE_UNAVAILABLE,
        WriteRejection::RateLimited => StatusCode::TOO_MANY_REQUESTS,
        WriteRejection::UnsupportedValueType(_)
        | WriteRejection::ValueOutOfRange(_)
        | WriteRejection::InvalidAddress(_) => StatusCode::UNPROCESSABLE_ENTITY,
        WriteRejection::WriteFailed(_) => StatusCode::BAD_GATEWAY,
        WriteRejection::AuditWriteFailed | WriteRejection::Internal(_) => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    (status, Json(rejection.to_json())).into_response()
}

/// [`v1_write_value`] とその周辺(レート制限・受付・監査)が必要とする状態
/// 一式。`TagSpaceState`とは別の型にしているのは、axum のルーターは
/// 1つの `State<T>` にしか `.with_state` できないため -
/// [`tag_space_router`] は GET 系ハンドラ用の `TagSpaceState` の
/// ルーターとこの `WriteState` のルーターを別々に組み立てて `.merge`
/// する(同じパス `/api/v1/values/{tag}` に GET/POST 両方が乗るのは axum
/// の `MethodRouter::merge_for_path` がそのまま面倒を見る)。
///
/// T4: 各フィールドは `crate::write_path::WriteDeps` へそのまま borrow
/// できる形にしてある - `crate::grpc::GrpcService` も同じ形のフィールドを
/// 持ち、両者がここから `WriteDeps` を組み立てて
/// `crate::write_path::execute_write` を呼ぶ(このファイル冒頭「書き込み
/// エンドポイント」節の doc comment参照)。
#[derive(Clone)]
struct WriteState {
    manager: Arc<CollectorManager>,
    api_keys: ApiKeysService,
    write_audit: WriteAuditService,
    write_control: Arc<WriteControl>,
    /// タグ毎+全体の2段レート制限(設計 §6-4)。複数リクエストが並行して
    /// 飛んでくるため`tokio::sync::Mutex`で包んで共有する - relay-wright
    /// の `Writer` が単一タスク専有だったのと違い、hub はこの1個の
    /// リミッタを複数リクエストが取り合う。ロックは「peek」「record」の
    /// 各操作の間だけ短時間握る(`crate::write_path::execute_write` 本体の
    /// コメント参照)。
    rate_limiter: Arc<AsyncMutex<WriteRateLimiter>>,
    events: broadcast::Sender<ServerEvent>,
}

/// `POST /api/v1/values/{tag}` - 書き込み(設計 §5.1・§6 全体)。
///
/// このハンドラ自身が担うのは REST 固有の前段だけ:
///
/// 事前段(番号なし、§6-8「認証」): `crate::rest::require_tag_space_auth`
/// を通過した時点で「有効な `bh_` API キー」であることは確定しているが、
/// このハンドラ自身がさらに (a) セッション token では到達できない
/// (ミドルウェア側で弾く - `ctx` extension が無ければ 403)、(b)
/// `write:{tag}` スコープの完全一致、の2つを検査する。
///
/// body の `v` を [`parse_requested_value`] で工学値へ正規化した後は、
/// ゲート1〜8の本体(catalog 解決・writable・実効 enabled・プロトコル対応・
/// 受付トグル・レート制限・値変換・log-before-write)を
/// `crate::write_path::execute_write` へそのまま委譲する(T4、実装指示
/// 「二重実装は絶対に不可」) - 結果を [`write_rejection_response`] で
/// HTTP ステータス + JSON へ変換するだけ。ゲートの詳細な番号・意味論は
/// `crate::write_path::execute_write` の doc comment を参照。
#[utoipa::path(
    post,
    path = "/api/v1/values/{tag}",
    params(("tag" = String, Path, description = "外部名 {connection}.{group}.{tag}")),
    request_body = WriteValueRequest,
    responses(
        (status = 200, description = "書き込み成功", body = WriteValueResponse),
        (status = 403, description = "not_writable / missing_write_scope / session_token_cannot_write / key_tripped"),
        (status = 404, description = "catalog に存在しない外部名"),
        (status = 409, description = "tag_disabled"),
        (status = 422, description = "unsupported_value_type / value_out_of_range"),
        (status = 429, description = "rate_limited"),
        (status = 501, description = "write_unsupported_protocol"),
        (status = 502, description = "write_failed"),
        (status = 503, description = "writes_disabled"),
    ),
    tag = "tag-space",
)]
async fn v1_write_value(
    State(state): State<WriteState>,
    Path(tag): Path<String>,
    ctx: Option<Extension<ApiKeyContext>>,
    Json(body): Json<WriteValueRequest>,
) -> Response {
    // 事前段(a): セッション token では書けない(§6-8) - require_tag_space_auth
    // は API キー・セッション token どちらでもここまで到達させるので、
    // ここで ApiKeyContext extension の有無により再確認する。
    let Some(Extension(ctx)) = ctx else {
        return session_token_cannot_write_response();
    };
    // 事前段(b): write:{tag} スコープの完全一致(read スコープでは書けない)。
    if !ctx.has_write_scope(&tag) {
        return missing_write_scope_response();
    }

    let requested = parse_requested_value(&body.v);
    let deps = crate::write_path::WriteDeps {
        manager: state.manager.as_ref(),
        api_keys: &state.api_keys,
        write_audit: &state.write_audit,
        write_control: state.write_control.as_ref(),
        rate_limiter: state.rate_limiter.as_ref(),
        events: &state.events,
    };

    match crate::write_path::execute_write(&deps, &ctx, &tag, requested).await {
        Ok(ok) => Json(WriteValueResponse {
            tag: ok.tag,
            result: "ok".to_string(),
        })
        .into_response(),
        Err(rejection) => write_rejection_response(tag, rejection),
    }
}

// --- OpenAPI 自動生成（設計 §5.1・§10-6、2026-08-04 決定） ------------------
//
// catalog（`/api/v1/tags`）はクライアントとの互換性契約（設計 §4.1）なので、
// コードとスキーマを単一ソース化する目的で utoipa を採用した。
// `ApiDoc::openapi()` は上の各ハンドラの `#[utoipa::path]` とこのファイルの
// 各応答型の `ToSchema` から機械的に構築される - ハンドラのシグネチャや
// 応答型を変えれば `/api/v1/openapi.json` も自動で追従する。

#[derive(OpenApi)]
#[openapi(
    info(
        title = "banto-hub タグ空間 API",
        version = "v1",
        description = "banto-hub の /api/v1/* API（docs/tag-server-design.md §5.1）。catalog（/api/v1/tags）はクライアントとの互換性契約である（§4.1）: 外部名・安定 ID・revision を用いたバインディングを前提に、フィールド名や JSON 形はこのスキーマとコードが常に一致するよう utoipa で自動生成している。書き込み（POST /api/v1/values/{tag}、T2-4・§6）は writable タグのみ・write:{tag} スコープの API キー限定。"
    ),
    paths(
        v1_tags,
        v1_values,
        v1_value_single,
        v1_write_value,
        v1_status,
        v1_events,
    ),
    components(schemas(
        TagEntry,
        CatalogResponse,
        ValueEntry,
        ValuesResponse,
        ConnectionStatusEntry,
        MqttStatusEntry,
        GrpcStatusEntry,
        StatusResponse,
        EventEntry,
        EventsResponse,
        WriteValueRequest,
        WriteValueResponse,
    ))
)]
struct ApiDoc;

/// `GET /api/v1/openapi.json` - **認証不要**（T0-2 実装指示: 「このエンド
/// ポイント自体は認証不要でよい」）。判断理由: OpenAPI スキーマはフィールド
/// 名・型・パスの一覧であって値そのものではなく、秘匿すべき情報を含まない。
/// 逆に、スキーマを見るために API キーを要求すると「まずキーを発行して
/// もらわないとどんな API か分からない」という鶏卵になり、外部連携の
/// 導入コストを不必要に上げる。
async fn openapi_json() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

// --- /api/v1/* 認証: API キー + セッション bearer 併用（設計 §5.6・T0-2） ---

#[derive(Clone)]
struct TagSpaceAuthState {
    auth: AuthState,
    api_keys: ApiKeysService,
    audit: AuditLogService,
    manager: Arc<CollectorManager>,
}

/// `/api/v1/*`（`GET /api/v1/openapi.json` を除く）の認証ミドルウェア -
/// T0-1 の `require_auth`（セッション bearer のみ）を置き換える（このモジュール
/// の doc comment 参照）。
///
/// - `Authorization` ヘッダがない、または `Bearer ` で始まらない → 401
/// - 値が `bh_` で始まる → API キーとして
///   [`crate::api_keys::ApiKeysService::lookup`] で照合:
///   - [`ApiKeyLookup::Valid`] → `POST /api/v1/values/{tag}`（書き込み、
///     T2-4）以外は `read` スコープを要求する（`write:` のみのキー等は
///     403 - 認証はできたが権限がない、という区別のため 401 ではなく
///     403）。書き込みルートは `read` を要求せず、代わりに
///     [`v1_write_value`] 自身が `write:{tag}` の完全一致を検査する
///     （リクエスト path の `{tag}` はここではまだ分からないため）。
///     いずれの場合も通過時は [`ApiKeyContext`] をリクエスト
///     extensions に載せて次段へ渡す（[`v1_write_value`] がスコープ
///     検査と監査行の `api_key_id`/`api_key_name_snapshot` に使う）。
///     `last_used_at` を60秒スロットルで更新（[`crate::api_keys::should_touch_last_used`]）
///   - [`ApiKeyLookup::Revoked`] → 401 + audit_log に
///     `action: "denied", resource: "api_keys"` を記録（設計 T0-2 実装
///     指示: 「失効済みキーでのアクセス試行は audit_log に記録する」）
///   - [`ApiKeyLookup::Tripped`]（T2-4、設計 §6-4）→ 403
///     `{ "error": "key_tripped" }` + audit_log に同様の `denied` 記録
///     （read/write いずれのリクエストも拒否 - `crate::api_keys` の
///     モジュール doc comment「トリップ」参照）
///   - [`ApiKeyLookup::NotFound`] → 401（監査記録しない - 存在しない/
///     偽造されたキーは「誰が」を特定できないただのノイズであり、
///     revoked の場合と違って「元は正規に発行されたキーが使われた」という
///     実害のシグナルがない）
/// - それ以外（`bh_` で始まらない）→ 従来どおり `AuthState` のセッション
///   token として照合（管理 UI からの利用互換のため - このモジュールの
///   doc comment参照）。ただし書き込みルートに限っては 403
///   `session_token_cannot_write` を返す（設計 §6-8: 「セッション token
///   では書けない」）
async fn require_tag_space_auth(
    State(state): State<TagSpaceAuthState>,
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let Some(token) = bearer_token(req.headers()).map(str::to_string) else {
        return unauthorized_response();
    };

    // T2-4（設計 §6-8）: `POST /api/v1/values/{tag}` だけ認証規律が違う
    // （read スコープ不要・write:{tag} は `v1_write_value` 自身が検査・
    // セッション token 不可）- パスは `/api/v1/values/{tag}` の
    // ちょうど1階層下なので、`/api/v1/values`（一括スナップショット）や
    // `/api/v1/values/{tag}` への GET とは POST であることで区別する。
    let is_write_route =
        req.method() == Method::POST && req.uri().path().starts_with("/api/v1/values/");

    if token.starts_with("bh_") {
        match state.api_keys.lookup(&token).await {
            Ok(ApiKeyLookup::Valid(ctx)) => {
                if !is_write_route && !ctx.has_read_scope() {
                    return forbidden_response();
                }
                let now_ms = state.manager.clock().now_ms();
                if let Err(err) = state
                    .api_keys
                    .touch_last_used(ctx.id, now_ms, ctx.last_used_at_ms)
                    .await
                {
                    eprintln!("banto-hub: API キーの last_used_at 更新に失敗しました: {err}");
                }
                req.extensions_mut().insert(ctx);
                next.run(req).await
            }
            Ok(ApiKeyLookup::Revoked { id, name }) => {
                let method = req.method().as_str().to_string();
                let path = req.uri().path().to_string();
                state
                    .audit
                    .record(AuditEntry {
                        actor_username: None,
                        actor_role: None,
                        action: "denied",
                        resource: "api_keys",
                        entity_id: Some(&id.to_string()),
                        detail: Some(json!({ "reason": "revoked", "name": name, "method": method, "path": path })),
                        origin: "rest",
                        result: "denied",
                    })
                    .await;
                unauthorized_response()
            }
            Ok(ApiKeyLookup::Tripped { id, name }) => {
                let method = req.method().as_str().to_string();
                let path = req.uri().path().to_string();
                state
                    .audit
                    .record(AuditEntry {
                        actor_username: None,
                        actor_role: None,
                        action: "denied",
                        resource: "api_keys",
                        entity_id: Some(&id.to_string()),
                        detail: Some(json!({ "reason": "tripped", "name": name, "method": method, "path": path })),
                        origin: "rest",
                        result: "denied",
                    })
                    .await;
                key_tripped_response()
            }
            Ok(ApiKeyLookup::NotFound) => unauthorized_response(),
            Err(err) => {
                eprintln!("banto-hub: API キー照合に失敗しました: {err}");
                unauthorized_response()
            }
        }
    } else if state.auth.verify(&token) {
        if is_write_route {
            return session_token_cannot_write_response();
        }
        next.run(req).await
    } else {
        unauthorized_response()
    }
}

/// `/api/v1/*`（design §5.1/§5.6）: `require_tag_space_auth`（API キー +
/// セッション bearer 併用）を全ルートに適用する。`GET /api/v1/openapi.json`
/// は別ルーター（`openapi_router`、`crate::rest::api_router` 参照）に分けて
/// あり、この認証層を通らない。
///
/// T2-4: GET 系ハンドラ（[`TagSpaceState`]）と書き込みハンドラ
/// （[`WriteState`]）は状態型が違うため axum の `Router` を分けて組み立て、
/// `.merge()` してから認証レイヤーを1枚だけ被せる（同じ
/// `/api/v1/values/{tag}` パスに GET/POST が同居するのは axum の
/// `MethodRouter` マージがそのまま面倒を見る - `WriteState`のモジュール
/// doc comment参照）。
#[allow(clippy::too_many_arguments)]
fn tag_space_router(
    manager: Arc<CollectorManager>,
    auth: AuthState,
    api_keys: ApiKeysService,
    audit: AuditLogService,
    write_control: Arc<WriteControl>,
    write_audit: WriteAuditService,
    events: broadcast::Sender<ServerEvent>,
    mqtt: Arc<MqttPublisher>,
    // T4（設計 §5.4 実装指示「二重実装は絶対に不可」）: `crate::grpc::GrpcService`
    // と**同じ** `Arc` を受け取る - ここで新規に構築しない。書き込みゲート
    // (`crate::write_path::execute_write`)を REST/gRPC で共有していても、
    // レート制限のバジェット(`crate::write_rate::WriteRateLimiter`)が別
    // インスタンスなら「タグ毎+全体の書き込み上限」が実質2倍緩む抜け道に
    // なる - 呼び出し元（[`api_router`]）が1個だけ構築し、REST・gRPC 両方の
    // `WriteState`/`GrpcService` へ同じ `Arc` を配る。
    rate_limiter: Arc<AsyncMutex<WriteRateLimiter>>,
) -> Router {
    let state = TagSpaceState {
        manager: manager.clone(),
        write_control: write_control.clone(),
        mqtt,
    };
    let auth_state = TagSpaceAuthState {
        auth: auth.clone(),
        api_keys: api_keys.clone(),
        audit,
        manager: manager.clone(),
    };
    let write_state = WriteState {
        manager: manager.clone(),
        api_keys,
        write_audit,
        write_control,
        rate_limiter,
        events,
    };

    let read_router = Router::new()
        .route("/api/v1/tags", get(v1_tags))
        .route("/api/v1/values", get(v1_values))
        .route("/api/v1/values/{tag}", get(v1_value_single))
        .route("/api/v1/status", get(v1_status))
        .route("/api/v1/events", get(v1_events))
        // T1（設計 §5.2・§5.6の9番）: 認証は他の /api/v1/* と同一の
        // require_tag_space_auth（read スコープ必須）- アップグレード
        // リクエスト自体が普通の HTTP GET なので、この `.layer` がそのまま
        // 効く（`crate::stream` 側の doc comment 参照）。
        .route("/api/v1/stream", get(crate::stream::ws_upgrade))
        .with_state(state);

    let write_router = Router::new()
        .route("/api/v1/values/{tag}", post(v1_write_value))
        .with_state(write_state);

    read_router
        .merge(write_router)
        .layer(middleware::from_fn_with_state(
            auth_state,
            require_tag_space_auth,
        ))
}

/// `GET /api/v1/openapi.json` 専用ルーター - 認証層を一切通さない
/// （`openapi_json`関数の doc comment 参照）。
fn openapi_router() -> Router {
    Router::new().route("/api/v1/openapi.json", get(openapi_json))
}

// --- composition ------------------------------------------------------------

/// Compose the full router: the admin surface (auth/users/audit-log/I1 CRUD/
/// api-keys/SSE, all behind CSRF + bearer auth) merged with the tag-space API
/// (API キー + セッション bearer 併用、CSRF なし - see this module's doc
/// comment) and the unauthenticated `/api/v1/openapi.json`.
#[allow(clippy::too_many_arguments)]
pub fn api_router(
    users: UsersService,
    audit: AuditLogService,
    plc_connections: PlcConnectionService,
    collection_groups: CollectionGroupService,
    tags: TagService,
    api_keys: ApiKeysService,
    manager: Arc<CollectorManager>,
    auth: AuthState,
    events: broadcast::Sender<ServerEvent>,
    allow_setup: bool,
    // T2-4（設計 §6）: 書き込み受付の起動時 disabled フラグと書き込み監査
    // サービス - どちらも `bin/banto-hub.rs`（本番）または各テストの
    // セットアップで一度だけ構築し、ここに注入する（`ApiKeysService` 等の
    // 他サービスと同じ規約）。
    write_control: Arc<WriteControl>,
    write_audit: WriteAuditService,
    // T3（設計 §5.3）: MQTT publish - `bin/banto-hub.rs`（本番）または各
    // テストのセットアップで一度だけ構築し、ここに注入する（上記2引数と
    // 同じ規約）。
    mqtt: Arc<MqttPublisher>,
    // T4（設計 §5.4）: gRPC サーバー - 上記と同じ規約（`bin/banto-hub.rs`
    // または各テストのセットアップで一度だけ構築する）。`/api/grpc-settings`
    // の `PUT` がこの `GrpcServer::apply` を呼んで即時適用する。
    grpc_server: Arc<crate::grpc::GrpcServer>,
    // T4（実装指示「二重実装は絶対に不可」）: `crate::grpc::GrpcService` の
    // 構築にも**同じ** `Arc` を渡すこと(呼び出し元の責務) -
    // `tag_space_router` のフィールド doc comment参照。ここで新規に
    // 構築すると REST/gRPC でレート制限のバジェットが分裂してしまう。
    rate_limiter: Arc<AsyncMutex<WriteRateLimiter>>,
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
        .merge(api_keys_router(
            api_keys.clone(),
            audit.clone(),
            auth.clone(),
        ))
        .merge(tag_registry_router(
            plc_connections,
            collection_groups,
            tags,
            audit.clone(),
            auth.clone(),
            manager.clone(),
            events.clone(),
        ))
        .merge(write_control_router(
            write_control.clone(),
            manager.clone(),
            audit.clone(),
            auth.clone(),
            events.clone(),
        ))
        .merge(write_audit_router(
            write_audit.clone(),
            audit.clone(),
            auth.clone(),
        ))
        .merge(mqtt_settings_router(
            manager.clone(),
            mqtt.clone(),
            audit.clone(),
            auth.clone(),
            events.clone(),
        ))
        .merge(grpc_settings_router(
            manager.clone(),
            grpc_server,
            audit.clone(),
            auth.clone(),
            events.clone(),
        ))
        .layer(middleware::from_fn(require_banto_client_header));

    admin
        .merge(tag_space_router(
            manager,
            auth,
            api_keys,
            audit,
            write_control,
            write_audit,
            events,
            mqtt,
            rate_limiter,
        ))
        .merge(openapi_router())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_keys::ApiKeysService;
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
        let sessions = Arc::new(crate::broker_glue::HubSessions::new(
            banto_broker::BackoffConfig::default(),
        ));
        let manager = CollectorManager::new(
            pool,
            dir.path().join("data"),
            Arc::new(SystemClock),
            CollectorOptions::default(),
            sessions,
        );
        (Arc::new(manager), dir)
    }

    /// Everything a T0-2 test needs: the assembled router, an admin session
    /// token, a viewer session token (RBAC-negative tests, e.g. "viewer may
    /// not issue API keys"), the `ApiKeysService` (so a test can seed/inspect
    /// keys directly instead of only through REST), and the owning temp dir.
    struct TestEnv {
        router: Router,
        admin_token: String,
        viewer_token: String,
        api_keys: ApiKeysService,
        pool: sqlx::SqlitePool,
        _dir: tempfile::TempDir,
    }

    async fn test_env() -> TestEnv {
        let pool = migrate_memory().await.expect("migrate_memory");
        let (tx, _rx) = tokio_broadcast::channel(16);
        let users = UsersService::new(pool.clone());
        let audit = AuditLogService::new(pool.clone());
        let plc_connections = PlcConnectionService::new(pool.clone());
        let collection_groups = CollectionGroupService::new(pool.clone());
        let tags = TagService::new(pool.clone());
        let api_keys = ApiKeysService::new(pool.clone());
        let (manager, dir) = test_manager(pool.clone());

        users
            .setup_first_user("admin", "password123", "管理者")
            .await
            .expect("setup_first_user");
        users
            .create_user("viewer1", "password123", "閲覧者", Role::Viewer)
            .await
            .expect("create_user viewer");
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
        let viewer_token = auth
            .login("viewer1", "password123")
            .await
            .expect("viewer login");

        let write_control = Arc::new(crate::write_control::WriteControl::new(false));
        let write_audit = crate::write_audit::WriteAuditService::new(pool.clone());
        let mqtt = Arc::new(crate::mqtt::MqttPublisher::new(manager.clone()));
        // T4: REST/gRPC で共有する1個の rate_limiter（`tag_space_router`の
        // フィールド doc comment参照）と `GrpcServer`（`crate::grpc`のテストで
        // 直接叩かない限り listen はしない - `apply`を呼ばないため）。
        let rate_limiter = Arc::new(AsyncMutex::new(WriteRateLimiter::new(
            crate::write_rate::WriteRateLimitConfig::default(),
        )));
        let grpc_service = crate::grpc::GrpcService::new(
            manager.clone(),
            api_keys.clone(),
            audit.clone(),
            write_audit.clone(),
            write_control.clone(),
            rate_limiter.clone(),
            tx.clone(),
        );
        let grpc_server = Arc::new(crate::grpc::GrpcServer::new(grpc_service));
        let router = api_router(
            users,
            audit,
            plc_connections,
            collection_groups,
            tags,
            api_keys.clone(),
            manager,
            auth,
            tx,
            false,
            write_control,
            write_audit,
            mqtt,
            grpc_server,
            rate_limiter,
        );
        TestEnv {
            router,
            admin_token,
            viewer_token,
            api_keys,
            pool,
            _dir: dir,
        }
    }

    /// Backward-compatible shim for the pre-T0-2 tests below that only need
    /// an admin session token.
    async fn router_with_token() -> (Router, String, tempfile::TempDir) {
        let env = test_env().await;
        (env.router, env.admin_token, env._dir)
    }

    /// `POST /api/api-keys` through the admin surface (bearer + CSRF +
    /// admin RBAC). Returns the parsed JSON body (`{ id, name, prefix,
    /// scopes, key }` on success).
    async fn issue_api_key(
        router: &Router,
        token: &str,
        name: &str,
        scopes: &[&str],
    ) -> (StatusCode, serde_json::Value) {
        let response = router
            .clone()
            .oneshot(
                HttpRequest::post("/api/api-keys")
                    .header("Authorization", format!("Bearer {token}"))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "name": name, "scopes": scopes }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value =
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
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

    // --- T0-2: API キー基盤 ------------------------------------------------

    /// 発行 → そのキーで `/api/v1/tags` が読める（E2E、実装指示 §3の1件目）。
    #[tokio::test]
    async fn issued_api_key_can_read_api_v1_tags() {
        let env = test_env().await;
        let (status, issued) =
            issue_api_key(&env.router, &env.admin_token, "mes-gateway", &["read"]).await;
        assert_eq!(status, StatusCode::CREATED, "{issued:?}");
        let key = issued["key"].as_str().expect("key should be present");
        assert!(key.starts_with("bh_"));
        assert_eq!(issued["scopes"], serde_json::json!(["read"]));

        let response = env
            .router
            .oneshot(
                HttpRequest::get("/api/v1/tags")
                    .header("Authorization", format!("Bearer {key}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    /// 失効済みキーでのアクセスは 401 + audit_log に "denied"/"api_keys" が
    /// 記録される（実装指示 §3の2件目 + §1「失効済みキーでのアクセス試行は
    /// audit_log に記録する」）。
    #[tokio::test]
    async fn revoked_api_key_is_401_and_audited() {
        let env = test_env().await;
        let (_status, issued) =
            issue_api_key(&env.router, &env.admin_token, "revoke-me", &["read"]).await;
        let key = issued["key"].as_str().unwrap().to_string();
        let id = issued["id"].as_i64().unwrap();

        let revoke = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post(format!("/api/api-keys/{id}/revoke"))
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(revoke.status(), StatusCode::OK);

        let response = env
            .router
            .oneshot(
                HttpRequest::get("/api/v1/tags")
                    .header("Authorization", format!("Bearer {key}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let audit = AuditLogService::new(env.pool.clone());
        let entries = audit.list(ListParams::default()).await.unwrap();
        let denied = entries
            .rows
            .iter()
            .find(|row| row.action == "denied" && row.resource == "api_keys")
            .expect("a denied/api_keys audit row should exist");
        assert_eq!(denied.entity_id.as_deref(), Some(id.to_string().as_str()));
    }

    /// `write:` のみのスコープを持つキーで `/api/v1/*`（read 専用エンド
    /// ポイント）にアクセスすると 403（実装指示 §3の3件目）。
    #[tokio::test]
    async fn write_only_scope_key_reading_api_v1_is_403() {
        let env = test_env().await;
        let (status, issued) = issue_api_key(
            &env.router,
            &env.admin_token,
            "writer-only",
            &["write:line1.fast.temp01"],
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{issued:?}");
        let key = issued["key"].as_str().unwrap();

        let response = env
            .router
            .oneshot(
                HttpRequest::get("/api/v1/tags")
                    .header("Authorization", format!("Bearer {key}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    /// 不正なスコープ構文（ワイルドカード）での発行は 400 相当
    /// （`BantoError::Validation` → `422`。実装指示 §3の5件目 - `ApiError`
    /// は `Validation` を `422 UNPROCESSABLE_ENTITY` にマップする、
    /// `banto_server::response::status_for` 参照。本テストは "400 系" の
    /// 実体であるこのステータスを確認する）。
    #[tokio::test]
    async fn issuing_with_invalid_scope_syntax_is_rejected() {
        let env = test_env().await;
        let (status, body) = issue_api_key(
            &env.router,
            &env.admin_token,
            "bad-scope",
            &["write:line1.fast.*"],
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
        assert_eq!(body["kind"], "validation");
    }

    /// viewer ロールで `POST /api/api-keys` すると 403（admin 限定、
    /// 実装指示 §3の6件目）。
    #[tokio::test]
    async fn viewer_role_cannot_issue_api_keys() {
        let env = test_env().await;
        let (status, _body) =
            issue_api_key(&env.router, &env.viewer_token, "should-fail", &["read"]).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    /// `GET /api/v1/openapi.json` は認証不要で 200、`/api/v1/values` 等の
    /// パスを含む（実装指示 §3の7件目）。
    #[tokio::test]
    async fn openapi_json_is_public_and_lists_tag_space_paths() {
        let env = test_env().await;
        let response = env
            .router
            .oneshot(
                HttpRequest::get("/api/v1/openapi.json")
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
        let paths = json["paths"].as_object().expect("paths object");
        for path in [
            "/api/v1/tags",
            "/api/v1/values",
            "/api/v1/values/{tag}",
            "/api/v1/status",
            "/api/v1/events",
        ] {
            assert!(paths.contains_key(path), "missing path: {path}");
        }
    }

    /// セッション token でも引き続き `/api/v1/*` が読める（設計 T0-1 からの
    /// 互換維持、実装指示 §3の4件目）- API キーが未発行でも管理 UI の
    /// bearer セッションだけで動くことを、`test_env` が発行済みキーを
    /// 一切作らないまま確認する。
    #[tokio::test]
    async fn session_token_still_reads_api_v1() {
        let env = test_env().await;
        let response = env
            .router
            .oneshot(
                HttpRequest::get("/api/v1/status")
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    /// `last_used_at` が実際の HTTP リクエスト経由で更新されることの
    /// REST レベルの確認（60秒スロットルの純粋関数単体テストは
    /// `crate::api_keys` 側にある - 実装指示 §3の8件目）。
    #[tokio::test]
    async fn last_used_at_is_populated_after_a_request() {
        let env = test_env().await;
        let (_status, issued) =
            issue_api_key(&env.router, &env.admin_token, "touch-me", &["read"]).await;
        let key = issued["key"].as_str().unwrap();
        let id = issued["id"].as_i64().unwrap();

        let before = env.api_keys.list().await.unwrap();
        let before_entry = before.iter().find(|k| k.id == id).unwrap();
        assert_eq!(before_entry.last_used_at, None);

        let response = env
            .router
            .oneshot(
                HttpRequest::get("/api/v1/tags")
                    .header("Authorization", format!("Bearer {key}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let after = env.api_keys.list().await.unwrap();
        let after_entry = after.iter().find(|k| k.id == id).unwrap();
        assert!(after_entry.last_used_at.is_some());
    }
}
