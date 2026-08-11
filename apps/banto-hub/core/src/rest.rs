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
//!   利用互換のため）。`GET /api/v1/openapi.json` と `GET
//!   /api/v1/swagger-ui/*`（同梱 Swagger UI、ux-plan.md §5・2026-08-12
//!   オーナー決定）だけは認証不要 - スキーマ自体は秘密ではないため
//!   （`openapi_json` 関数の doc comment・`openapi_router` の doc comment
//!   参照）。
//!
//! ## I1 CRUD 書き込み後の catalog commit（T14-3）
//!
//! `tag_registry_router` の書き込みハンドラ（create/update/delete、3
//! リソース共通）は、同一SQLiteトランザクション内で提案mutation後の
//! registry snapshot/catalog/computed plan/configを検証し、成功した場合だけ
//! [`crate::hub::CollectorManager::commit_catalog`] を呼ぶ。これは
//! configured revisionだけを前進させ、Collector/Broker/Simulatorを起動・再適用
//! しない。実行中構成への適用はCollectionControllerのstart経路に限る。
//! 併せて admin-UI 向けの `ServerEvent::ResourceChanged` を SSE (`/api/events`)
//! に流す。
//!
//! ## タグ空間の値の意味論（設計 §4）
//!
//! `/api/v1/values*` は [`crate::hub::CollectorManager::current_values`] を
//! 読むだけで完結し、PLC への追加要求を発生させない。無効化されている
//! タグ（`TagEntry::enabled == false` — 接続・グループ・タグいずれかが無効）
//! は、たとえ過去のサンプルが current-value キャッシュに残っていても
//! 強制的に `q: "bad", v: null` を返す（欠測を隠さない）。404 になるのは
//! catalog に定義そのものが存在しない外部名だけ。

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Extension, Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use banto_broker::{BrokerConnectionStatus, BrokerError};
use banto_collect::{build_config_from, ApplyReport, ConnectionStatus, RegistrySnapshot};
use banto_core::{BantoError, ErrorBody, FieldError, ListParams, ListResult};
// T12 (docs/ux-plan.md §4): 保存前の接続テスト API 用。Modbus/SLMP 両方の
// 直接ダイヤル経路が同じ型を使うので、ここで一括 import する
// (`BatchReadRequest`/`BatchReadResult`は`banto_broker`ではなく`banto_plc`が
// 定義元 - `banto_broker::BrokerHandle::read`/`ReadOnlyHandle::read`が
// 引数・戻り値としてそのまま使っている)。
use banto_plc::{
    Address, BatchReadRequest, BatchReadResult, DataType, ModbusTcpClient, ModbusTcpConfig,
    PlcClient, PlcError, ReadRequest, ReadResult, SlmpClient, SlmpConfig,
};
use banto_server::{
    auth_routes, require_auth, require_banto_client_header, sse_route, ApiError, AuthState,
    Identity, ServerEvent,
};
use banto_tags::{
    BatchTagOutcome, CollectionGroup, CollectionGroupInput, CollectionGroupService, PlcConnection,
    PlcConnectionInput, PlcConnectionService, Tag, TagInput, TagService, TagUpdateError,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::str::FromStr;
use tokio::sync::broadcast;
use tokio::sync::Mutex as AsyncMutex;
use utoipa::{OpenApi, ToSchema};
use utoipa_swagger_ui::{Config, SwaggerUi};

use crate::api_keys::{ApiKeyContext, ApiKeyLookup, ApiKeysService, IssuedApiKey};
use crate::audit::{AuditEntry, AuditLogService};
use crate::controller::{CollectionController, CollectionState, CollectionStatus, RunMode};
use crate::hub::{CollectorManager, SimulationCoverageReport, TagEntry};
use crate::mqtt::MqttPublisher;
use crate::pending_changes::{PendingChange, PendingChangesService};
use crate::settings::{AuditSettings, MqttSettings, SettingsService};
use crate::test_output::TestOutputControl;
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

/// `Sec-WebSocket-Protocol`-as-bearer-carrier fallback for `GET
/// /api/v1/stream` only (judgment call, 2026-08-07 - browser WS auth gap
/// discovered while building banto-hub's live tag monitor, T10).
///
/// The browser's native `WebSocket` constructor cannot set custom request
/// headers - there is no way for page JS to attach `Authorization` to a WS
/// handshake, so a plain `new WebSocket('/api/v1/stream')` from the admin
/// UI can never authenticate against [`require_tag_space_auth`]'s normal
/// [`bearer_token`] check. The standard workaround (used by e.g. AWS IoT's
/// browser MQTT-over-WS SDK) is to smuggle the token through
/// `Sec-WebSocket-Protocol`, which the browser *does* let JS set via the
/// `WebSocket(url, protocols)` constructor overload: the client connects
/// with `new WebSocket(url, ['bearer', token])`, which the browser sends as
/// the header `Sec-WebSocket-Protocol: bearer, <token>`. A `?token=` query
/// parameter was deliberately rejected as an alternative - it would leak the
/// token into server access logs and browser history, whereas the
/// subprotocol header is not part of the URL and is not logged that way.
///
/// Scoped to the exact path `/api/v1/stream` so no other `/api/v1/*` route's
/// auth behavior changes - every other machine client (Rust tests, API-key
/// clients) can and does set `Authorization` directly.
///
/// Note: [`crate::stream::ws_upgrade`] calls
/// `WebSocketUpgrade::protocols(["bearer"])`, which only selects/echoes
/// `"bearer"` back in the response if the client actually offered it in its
/// own `Sec-WebSocket-Protocol` request header - so machine clients that
/// authenticate via `Authorization` and never offer a subprotocol are
/// unaffected. See that function's doc comment for the full rationale
/// (`tokio-tungstenite`'s client-side handshake validation requires the echo
/// when the client does offer a subprotocol).
fn extract_ws_protocol_token(path: &str, headers: &HeaderMap) -> Option<String> {
    if path != "/api/v1/stream" {
        return None;
    }
    let raw = headers
        .get(axum::http::header::SEC_WEBSOCKET_PROTOCOL)?
        .to_str()
        .ok()?;
    let parts: Vec<&str> = raw.split(',').map(str::trim).collect();
    if parts.len() != 2 || parts[0] != "bearer" {
        return None;
    }
    Some(parts[1].to_string())
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

fn simulation_output_disabled_response() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({ "error": "simulation_output_disabled" })),
    )
        .into_response()
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

// --- audit log (docs/banto-hub-remaining-plan.md P3-a: retention-config
// endpoints added - chronogazer/relay-wright と同型の `AuditSettings`
// 配線) -----------------------------------------------------------------

#[derive(Clone)]
struct AuditLogState {
    audit: AuditLogService,
    // `mqtt_settings_router`/`grpc_settings_router`と同じ規約:
    // `SettingsService`自体は持たず、ハンドラ内で
    // `SettingsService::new(state.manager.pool())`を都度構築する。
    manager: Arc<CollectorManager>,
    auth: AuthState,
}

/// `POST /api/audit-log/list`（admin 限定）: フィルタ/ソート/ページング
/// 済みの監査ログ一覧。読む前に retention 設定に従って opportunistic に
/// 剪定する（chronogazer/relay-wright の`audit_log_list`と同じ「list実行
/// 時に軽く」規約 - `crate::audit::AuditLogService::prune`のdoc comment
/// 参照）。剪定に失敗しても一覧の取得自体は続行する（best-effort）。
/// P3-a 追補（2026-08-12）: `crate::runtime::HubRuntime::start`の24h周期
/// タスクが同じ剪定を回すようになったため、このopportunistic剪定は
/// もはや無制限成長を防ぐための唯一の保証ではないが、設定変更直後に
/// 画面を開いた管理者へ即座に反映する効果があるため残している。
async fn audit_log_list(
    State(state): State<AuditLogState>,
    Json(params): Json<ListParams>,
) -> Result<Json<ListResult<crate::audit::AuditLogEntry>>, ApiError> {
    if let Ok(config) = SettingsService::new(state.manager.pool())
        .audit_config()
        .await
    {
        let _ = state
            .audit
            .prune(config.retention_days, config.retention_rows)
            .await;
    }
    Ok(Json(state.audit.list(params).await?))
}

/// `GET /api/audit-log/config`（admin 限定）: 現在の retention 設定。
/// 読み取り専用のため監査エントリは記録しない（read routes are never
/// audited - `crate::audit`のモジュール doc comment参照）。
async fn audit_log_config_get(
    State(state): State<AuditLogState>,
) -> Result<Json<AuditSettings>, ApiError> {
    Ok(Json(
        SettingsService::new(state.manager.pool())
            .audit_config()
            .await?,
    ))
}

/// `PUT /api/audit-log/config`（admin 限定）: retention 設定を保存する。
/// `retentionDays`/`retentionRows`いずれも省略・`null`可（そのフィールド
/// を無制限にする - `crate::settings::AuditSettings`のdoc comment参照）。
/// `mqtt_settings_put`/`grpc_settings_put`と同じ「保存 → 監査エントリ
/// 記録」の形だが、こちらは即時適用するランタイム状態を持たないため
/// （`prune`は次回の24h周期タスク/起動時/list実行時のいずれかで読まれる
/// だけ - `crate::runtime::audit_prune_once`のdoc comment参照）、`apply`
/// 相当の呼び出しは無い。
async fn audit_log_config_put(
    State(state): State<AuditLogState>,
    headers: HeaderMap,
    Json(config): Json<AuditSettings>,
) -> Result<Json<AuditSettings>, ApiError> {
    let settings_service = SettingsService::new(state.manager.pool());
    settings_service.set_audit_config(&config).await?;

    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "update",
        "audit_log_config",
        "1",
        Some(json!({
            "retentionDays": config.retention_days,
            "retentionRows": config.retention_rows,
        })),
    )
    .await;

    Ok(Json(settings_service.audit_config().await?))
}

fn audit_log_router(
    audit: AuditLogService,
    auth: AuthState,
    manager: Arc<CollectorManager>,
) -> Router {
    let state = AuditLogState {
        audit: audit.clone(),
        manager,
        auth: auth.clone(),
    };
    Router::new()
        .route("/api/audit-log/list", post(audit_log_list))
        .route(
            "/api/audit-log/config",
            get(audit_log_config_get).put(audit_log_config_put),
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
    /// H10 ①: `api_keys_create` の「有効期限は未来限定」検証で使う時計
    /// （`manager.clock()`）。他の `*AdminState`（`WriteControlAdminState`
    /// 等）が `manager` を持つのと同じ規約 - テストでは
    /// `ManualClock` に差し替えられる。
    manager: Arc<CollectorManager>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateApiKeyRequest {
    name: String,
    scopes: Vec<String>,
    /// H10 ①（docs/improvement-plan.md、2026-08-08 オーナー決定）: 任意の
    /// 有効期限（絶対 epoch ミリ秒、wire は `expiresAt` -
    /// `crate::api_keys::ApiKeySummary` の `expires_at`/`FieldError::field`
    /// 命名規約と同じ camelCase に揃えるため、この構造体自体にも
    /// `rename_all = "camelCase"` を追加した - `name`/`scopes` は
    /// 1語なので実質無変化）。省略/`null` = 無期限（既定・動作不変、
    /// `#[serde(default)]` は既存クライアントの後方互換のため -
    /// `GrpcSettingsBody::bind` 等と同じ規約）。`Some` の場合は
    /// [`api_keys_create`] が「現在時刻より未来」を検証してから
    /// [`ApiKeysService::issue`] に渡す（`issue` 自体は再検証しない）。
    #[serde(default)]
    expires_at: Option<i64>,
}

/// `POST /api/api-keys` の応答 - `IssuedApiKey` をそのまま返すと `key`
/// フィールド名がスネークケースのままになる（`crate::api_keys` は機械
/// クライアント向け `/api/v1/*` と同じ snake_case 規約）ので、それに
/// 合わせてここでも変換なしでそのまま公開する（T0-2 実装指示の応答例
/// `{ "id", "name", "prefix", "scopes", "key": "bh_..." }` と一致）。
///
/// H10 ①で `expiresAt` を追加していない: 発行応答は元々
/// `created_at`/`revoked_at`/`tripped_at` も含まない最小限の形（「平文
/// key を一度だけ返す」ことが主目的）で、入力どおりの値をそのまま返すだけの
/// `expiresAt` もこの最小性に合わせた - 必要なら直後の `GET /api/api-keys`
/// 一覧（`crate::api_keys::ApiKeySummary`）で確認できる。
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
/// キー平文・ハッシュは監査 detail に入れない」）。`expiresAt`（H10 ①）は
/// 秘密ではないので監査 detail に含めてよい。
///
/// `expiresAt` の「未来限定」検証はここで行う（`crate::api_keys` の
/// サービス層は `now_ms` を持たないため） - `state.manager.clock()` は
/// `require_tag_space_auth` 等と同じ、テストで差し替え可能な時計。
/// 不正なら `validate_scopes`/重複名と同じ `BantoError::Validation` 経由の
/// 4xx で弾く（新しい HTTP パスは作らない）。
async fn api_keys_create(
    State(state): State<ApiKeysAdminState>,
    headers: HeaderMap,
    Json(body): Json<CreateApiKeyRequest>,
) -> Result<(StatusCode, Json<IssuedApiKeyResponse>), ApiError> {
    if let Some(expires_at) = body.expires_at {
        let now_ms = state.manager.clock().now_ms();
        if expires_at <= now_ms {
            return Err(ApiError(BantoError::Validation {
                field_errors: vec![FieldError {
                    field: "expiresAt".to_string(),
                    message: "有効期限は現在時刻より後の日時を指定してください".to_string(),
                }],
            }));
        }
    }

    let issued = state
        .api_keys
        .issue(&body.name, body.scopes, body.expires_at)
        .await?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "create",
        "api_keys",
        &issued.id.to_string(),
        Some(json!({
            "name": issued.name,
            "scopes": issued.scopes,
            "expiresAt": body.expires_at,
        })),
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
fn api_keys_router(
    api_keys: ApiKeysService,
    audit: AuditLogService,
    auth: AuthState,
    manager: Arc<CollectorManager>,
) -> Router {
    let state = ApiKeysAdminState {
        api_keys,
        auth: auth.clone(),
        audit: audit.clone(),
        manager,
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

// --- テスト出力トグル (T15-3、設計 §6.3): admin 限定、CSRF + bearer -------
//
// `POST /api/test-output/enable`/`disable` は
// `crate::test_output::TestOutputControl`（ライブフラグのみ、非永続 -
// `write_control_router`と同型だが`persist_enabled`に相当するものはない）
// を切り替える。`enable`は`write-control`と違い無条件では成功しない -
// 収集が`Running`かつ mode が`AllSimulation`であることを要求する
// （主用途「全体シミュレーション中は通常出力が空になる」の代替出力先を
// 用意すること、実装指示参照）。`disable`は常に成功する
// （設計「停止／終了／切替／サービス再起動後に必ず無効へ戻る」の一部を
// 明示操作でも行えるようにする）。

#[derive(Clone)]
struct TestOutputAdminState {
    test_output: Arc<TestOutputControl>,
    controller: Arc<CollectionController>,
    auth: AuthState,
    audit: AuditLogService,
    events: broadcast::Sender<ServerEvent>,
}

/// `GET /api/v1/status`の`test_output`と同じ形（`v1_status`参照）。
/// `crate::test_output::TestOutputStatus`をそのまま JSON へ写す。
#[derive(Debug, Serialize, ToSchema)]
struct TestOutputStatusEntry {
    enabled: bool,
    run_id: Option<u64>,
}

impl From<crate::test_output::TestOutputStatus> for TestOutputStatusEntry {
    fn from(status: crate::test_output::TestOutputStatus) -> Self {
        Self {
            enabled: status.enabled,
            run_id: status.run_id,
        }
    }
}

/// [`test_output_enable`]が有効化の前提を満たさないときの応答 - 実装指示
/// 「Reject with 409...if collection is not Running or mode is not
/// AllSimulation」。`RegistryMutationError::CollectionEditLocked`と同じ
/// 「409 + 現在の`CollectionStatusResponse`を返す」形にする(呼び出し側が
/// 状態を見て次にどう操作すべきか判断できるようにする)。
struct TestOutputNotEligible(CollectionStatusResponse);

impl IntoResponse for TestOutputNotEligible {
    fn into_response(self) -> Response {
        (
            StatusCode::CONFLICT,
            Json(json!({
                "error": "test_output_not_available",
                "status": self.0,
                "message": "テスト出力は収集が稼働中かつ全 PLC シミュレーション中のみ有効化できます。",
            })),
        )
            .into_response()
    }
}

async fn test_output_enable(
    State(state): State<TestOutputAdminState>,
    headers: HeaderMap,
) -> Result<Json<TestOutputStatusEntry>, TestOutputNotEligible> {
    let status = state.controller.status();
    let run_id = match (status.state, status.mode, status.run_id) {
        (CollectionState::Running, RunMode::AllSimulation, Some(run_id)) => run_id,
        _ => return Err(TestOutputNotEligible(status.into())),
    };
    state.test_output.enable(run_id);

    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "enable",
        "test_output",
        "1",
        Some(json!({ "runId": run_id })),
    )
    .await;
    let _ = state.events.send(ServerEvent::ResourceChanged {
        resource: "test_output".to_string(),
    });

    Ok(Json(state.test_output.status().into()))
}

async fn test_output_disable(
    State(state): State<TestOutputAdminState>,
    headers: HeaderMap,
) -> Json<TestOutputStatusEntry> {
    state.test_output.disable();

    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "disable",
        "test_output",
        "1",
        None,
    )
    .await;
    let _ = state.events.send(ServerEvent::ResourceChanged {
        resource: "test_output".to_string(),
    });

    Json(state.test_output.status().into())
}

fn test_output_router(
    test_output: Arc<TestOutputControl>,
    controller: Arc<CollectionController>,
    audit: AuditLogService,
    auth: AuthState,
    events: broadcast::Sender<ServerEvent>,
) -> Router {
    let state = TestOutputAdminState {
        test_output,
        controller,
        auth: auth.clone(),
        audit: audit.clone(),
        events,
    };
    Router::new()
        .route("/api/test-output/enable", post(test_output_enable))
        .route("/api/test-output/disable", post(test_output_disable))
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            RoleGuard {
                auth: auth.clone(),
                min: Role::Admin,
                resource: "test_output",
                audit,
            },
            require_role_at_least,
        ))
        .layer(middleware::from_fn_with_state(auth, require_auth))
}

// --- collection lifecycle control (T14-4): admin + CSRF --------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CollectionStatusResponse {
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
        "1",
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
) -> Json<CollectionStatusResponse> {
    let status = state.controller.start(RunMode::Configured).await;
    collection_control_result(&state, &headers, "start", status).await
}

async fn collection_start_all_simulation(
    State(state): State<CollectionAdminState>,
    headers: HeaderMap,
) -> Json<CollectionStatusResponse> {
    let status = state.controller.start(RunMode::AllSimulation).await;
    collection_control_result(&state, &headers, "start_all_simulation", status).await
}

async fn collection_stop(
    State(state): State<CollectionAdminState>,
    headers: HeaderMap,
) -> Json<CollectionStatusResponse> {
    let status = state.controller.stop().await;
    collection_control_result(&state, &headers, "stop", status).await
}

async fn collection_set_mode(
    State(state): State<CollectionAdminState>,
    headers: HeaderMap,
    Json(body): Json<CollectionModeRequest>,
) -> Result<Json<CollectionStatusResponse>, ApiError> {
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
    let status = state.controller.set_mode(mode).await;
    Ok(collection_control_result(&state, &headers, "set_mode", status).await)
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

fn collection_control_router(
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
            RoleGuard {
                auth: auth.clone(),
                min: Role::Admin,
                resource: "collection",
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
// 持たないため、`enabled`/`port` は常に現在値をそのまま読み書きする。
//
// 2026-08-08 オーナー決定(docs/improvement-plan.md H3)で `bind`
// (`crate::settings::GrpcSettings::bind`、既定 `127.0.0.1`)を追加した -
// `PUT` の `bind` は `Option<String>` で、`mqtt.password` と同じ「省略
// (`None`)= 現在値を維持」規約に合わせる(`GrpcSettingsBody`のdoc comment
// 参照)。ただし `bind` は秘匿情報ではないので、`password`と違い明示的な
// 空文字を「変更なし」とは扱わない - 指定した以上は有効な IP アドレスで
// あることを要求する(`validate_grpc_settings_body`参照)。gRPC は認証
// 必須だが TLS が無いため、既定を loopback にして LAN 公開は管理者の
// 明示 opt-in とする(`crate::grpc::GrpcServer::apply`のdoc comment参照)。

#[derive(Clone)]
struct GrpcSettingsAdminState {
    manager: Arc<CollectorManager>,
    grpc_server: Arc<crate::grpc::GrpcServer>,
    auth: AuthState,
    audit: AuditLogService,
    events: broadcast::Sender<ServerEvent>,
}

/// `GET/PUT /api/grpc-settings`の request/response body。
///
/// `bind`: `PUT` で省略(未送信、または `null`)すると現在値を維持する
/// (このモジュールの「gRPC 設定」セクション doc comment参照)。`GET` の
/// 応答には常に現在の設定値が入る(`From<GrpcSettings>`参照 - 省略が
/// 起きるのは request 側だけ)。
#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct GrpcSettingsBody {
    enabled: bool,
    /// 省略時(`None`)は現在値を維持。指定する場合は `IpAddr` として
    /// 解釈できる文字列(例: `"127.0.0.1"`/`"0.0.0.0"`)である必要がある
    /// - 既定は `crate::settings::DEFAULT_GRPC_BIND`(`"127.0.0.1"`)。
    #[serde(default)]
    bind: Option<String>,
    port: u16,
}

impl From<crate::settings::GrpcSettings> for GrpcSettingsBody {
    fn from(config: crate::settings::GrpcSettings) -> Self {
        Self {
            enabled: config.enabled,
            bind: Some(config.bind),
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

/// 入力検証:
/// - `port` は 0 不可(`u16` なので上限 65535 は型で保証済み)
/// - `bind` を指定した場合(`Some`)は `IpAddr` として解釈できる必要がある
///   - 省略(`None`)は「現在値を維持」であって検証対象ではない
///     (`GrpcSettingsBody::bind`のdoc comment参照)
///   - `mqtt.password` と違い、空文字は「変更なし」の特別扱いをしない
///     (空文字は単に不正な IP として弾かれる) - bind は秘匿情報ではない
///     ので `None`/フィールド省略のほうで「維持」の意図を表せば足りる
fn validate_grpc_settings_body(body: &GrpcSettingsBody) -> Vec<FieldError> {
    let mut errors = Vec::new();
    if body.port == 0 {
        errors.push(FieldError {
            field: "port".to_string(),
            message: "port は 1〜65535 で指定してください".to_string(),
        });
    }
    if let Some(bind) = &body.bind {
        if bind.parse::<IpAddr>().is_err() {
            errors.push(FieldError {
                field: "bind".to_string(),
                message: "bind は IP アドレス(例: 127.0.0.1、0.0.0.0)で指定してください"
                    .to_string(),
            });
        }
    }
    errors
}

async fn grpc_settings_put(
    State(state): State<GrpcSettingsAdminState>,
    headers: HeaderMap,
    Json(body): Json<GrpcSettingsBody>,
) -> Result<Json<GrpcSettingsBody>, ApiError> {
    let field_errors = validate_grpc_settings_body(&body);
    if !field_errors.is_empty() {
        return Err(ApiError(BantoError::Validation { field_errors }));
    }

    let settings_service = SettingsService::new(state.manager.pool());
    // `bind` 省略時は現在値を維持する(`GrpcSettingsBody::bind`のdoc
    // comment参照 - `mqtt_settings_put`が`password`の「変更なし」を
    // 解決するために既存値を読むのと同じ形)。
    let existing = settings_service.grpc_config().await?;
    let bind = body.bind.unwrap_or(existing.bind);

    let config = crate::settings::GrpcSettings {
        enabled: body.enabled,
        bind,
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
        Some(json!({ "enabled": config.enabled, "bind": config.bind, "port": config.port })),
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

/// T9-2 (docs/ux-plan.md §1): a `PlcConnectionPayload` missing `simulation`
/// (an old client, or a create/update that never mentions it) keeps the
/// existing safe default - a connection is never accidentally simulated by
/// omission.
fn default_plc_simulation() -> bool {
    false
}

/// P3-b（監査指摘 2026-08-12）: a `PlcConnectionPayload` missing `wordOrder`
/// (an old client, or a create/update that never mentions it) keeps MELSEC's
/// own low-word-first order - the same default
/// `banto_tags::plc_connection::default_word_order` and migration `0010`'s
/// column default already use, so an old client's connections behave exactly
/// as they did before this field existed.
fn default_plc_word_order() -> String {
    "low_high".to_string()
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
#[derive(Debug, Clone, Deserialize, Serialize)]
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
    /// T9-2 (docs/ux-plan.md §1, 「接続単位のシミュレーションモード」): opts
    /// this connection into an in-process simulator instead of a real PLC -
    /// see `banto_tags::PlcConnection::simulation`'s doc comment for what
    /// this actually does at collection time
    /// (`crates/banto-collect/src/simulation.rs`) and, for broker-routed SLMP
    /// connections specifically, `crate::broker_glue::SlmpSimRegistry`.
    #[serde(default = "default_plc_simulation")]
    pub simulation: bool,
    /// P3-b（監査指摘 2026-08-12）: SLMP のワード順（32bit値の上位/下位ワードの
    /// 並び）。`"low_high"`（既定・MELSEC標準）/ `"high_low"`（Modbus/IEEE慣習）
    /// のいずれか - 検証は `banto_tags::plc_connection::validate_plc_connection_input`
    /// 側（`ALLOWED_WORD_ORDERS`）に委ねる。modbus-tcp/virtual 接続では無意味
    /// （`unit_id` と同じ扱い）だが、フォームは "slmp" 選択時のみ表示する
    /// （`plc-connections/+page.svelte`）。
    #[serde(default = "default_plc_word_order")]
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
            // T9-2: wired through - see `PlcConnectionPayload::simulation`'s
            // doc comment.
            simulation: payload.simulation,
            // P3-b: wired through - see `PlcConnectionPayload::word_order`'s
            // doc comment.
            word_order: payload.word_order,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
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

#[derive(Debug, Clone, Deserialize, Serialize)]
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
    /// T18-1（docs/banto-hub-desktop-plan.md §9.4 TAG-UX-C 4点目「revision /
    /// ETag で後勝ち上書きを防ぐ」）: 編集画面が最後に取得した
    /// `Tag::revision`。`#[serde(default)]`（= `None`）なので既存クライアント
    /// のペイロードは無変更で通る（revision チェック無しの互換動作 -
    /// `banto_tags::TagInput::expected_revision`のドキュメント参照）。管理 UI
    /// （`tags/+page.svelte`）は編集フォームを開いた時点の `selected.revision`
    /// を常に送る。
    #[serde(default)]
    pub expected_revision: Option<i64>,
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
            expected_revision: payload.expected_revision,
        }
    }
}

/// Commit the catalog already preflighted in the write transaction and notify
/// admin-UI SSE subscribers. Production callers leave
/// `legacy_live_reconfigure` disabled: registry writes advance the configured
/// revision only. The compatibility router can opt into the pre-T14-3 live
/// apply for existing embedders/tests.
async fn commit_catalog_and_notify(
    manager: &CollectorManager,
    controller: &CollectionController,
    events: &broadcast::Sender<ServerEvent>,
    resource: &str,
    snapshot: RegistrySnapshot,
    legacy_live_reconfigure: bool,
) {
    if let Err(err) = manager.commit_catalog(&snapshot).await {
        eprintln!("banto-hub: {resource} 変更後の catalog commit に失敗しました: {err}");
    } else {
        controller.refresh_status();
        if legacy_live_reconfigure && manager.current_values().is_some() {
            if let Err(err) = manager
                .apply_run(crate::controller::RunMode::Configured)
                .await
            {
                eprintln!("banto-hub: {resource} 変更後の live reconfigure に失敗しました: {err}");
            }
        }
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
    controller: Arc<CollectionController>,
    events: broadcast::Sender<ServerEvent>,
    pending_changes: PendingChangesService,
    legacy_live_reconfigure: bool,
}

#[derive(Debug, Serialize)]
struct QueuedPendingChangeResponse {
    queued: bool,
    pending: PendingChange,
    status: CollectionStatusResponse,
    message: String,
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
async fn compute_pending_base_fingerprint(
    state: &TagRegistryState,
    source: &str,
    payload: &serde_json::Value,
) -> Option<String> {
    let id = payload.get("id")?.as_i64()?;
    match source {
        "plc_connections.update" | "plc_connections.delete" => state
            .plc_connections
            .get(id)
            .await
            .ok()
            .and_then(|row| serde_json::to_string(&row).ok()),
        "collection_groups.update" | "collection_groups.delete" => state
            .collection_groups
            .get(id)
            .await
            .ok()
            .and_then(|row| serde_json::to_string(&row).ok()),
        _ => None,
    }
}

async fn queue_pending_registry_change(
    state: &TagRegistryState,
    headers: &HeaderMap,
    source: &str,
    payload: serde_json::Value,
    status: CollectionStatus,
) -> RegistryMutationResult<Response> {
    let identity = actor_identity(headers, &state.auth);
    let base_fingerprint = compute_pending_base_fingerprint(state, source, &payload).await;
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
            pending,
            status: status.into(),
            message: "収集中のため変更を未適用キューに保存しました。".to_string(),
        }),
    )
        .into_response())
}

enum RegistryMutationError {
    Api(ApiError),
    CollectionEditLocked(CollectionStatusResponse),
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
            Self::CollectionEditLocked(status) => (
                StatusCode::CONFLICT,
                Json(json!({
                    "error": "collection_edit_locked",
                    "state": status.state,
                    "status": status,
                    "message": "収集中は構成を編集できません。停止してから再試行してください。"
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

type RegistryMutationResult<T> = Result<T, RegistryMutationError>;

fn require_collection_stopped(state: &TagRegistryState) -> RegistryMutationResult<()> {
    let status = state.controller.status();
    if status.state != CollectionState::Stopped {
        return Err(RegistryMutationError::CollectionEditLocked(status.into()));
    }
    Ok(())
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
) -> RegistryMutationResult<Response> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "plc_connections",
        "POST",
        "/api/plc-connections",
    )
    .await?;
    let status = state.controller.status();
    if status.state != CollectionState::Stopped {
        return queue_pending_registry_change(
            &state,
            &headers,
            "plc_connections.create",
            json!({ "input": input }),
            status,
        )
        .await;
    }
    require_collection_stopped(&state)?;
    let mut tx = state
        .manager
        .pool()
        .begin()
        .await
        .map_err(storage_api_error)?;
    let created = match state.plc_connections.create_tx(&mut tx, input.into()).await {
        Ok(created) => created,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(ApiError(err).into());
        }
    };
    let snapshot = match preflight_transaction(&mut tx).await {
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
        "plc_connections",
        &created.id.to_string(),
        Some(json!({ "name": created.name, "enabled": created.enabled })),
    )
    .await;
    commit_catalog_and_notify(
        &state.manager,
        &state.controller,
        &state.events,
        "plc_connections",
        snapshot,
        state.legacy_live_reconfigure,
    )
    .await;
    Ok(Json(created).into_response())
}

async fn plc_connections_update(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(input): Json<PlcConnectionPayload>,
) -> RegistryMutationResult<Response> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "plc_connections",
        "PUT",
        "/api/plc-connections/{id}",
    )
    .await?;
    let status = state.controller.status();
    if status.state != CollectionState::Stopped {
        return queue_pending_registry_change(
            &state,
            &headers,
            "plc_connections.update",
            json!({ "id": id, "input": input }),
            status,
        )
        .await;
    }
    require_collection_stopped(&state)?;
    let mut tx = state
        .manager
        .pool()
        .begin()
        .await
        .map_err(storage_api_error)?;
    let updated = match state
        .plc_connections
        .update_tx(&mut tx, id, input.into())
        .await
    {
        Ok(updated) => updated,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(ApiError(err).into());
        }
    };
    let snapshot = match preflight_transaction(&mut tx).await {
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
        "plc_connections",
        &id.to_string(),
        Some(json!({ "name": updated.name, "enabled": updated.enabled })),
    )
    .await;
    commit_catalog_and_notify(
        &state.manager,
        &state.controller,
        &state.events,
        "plc_connections",
        snapshot,
        state.legacy_live_reconfigure,
    )
    .await;
    Ok(Json(updated).into_response())
}

async fn plc_connections_delete(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> RegistryMutationResult<Response> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "plc_connections",
        "DELETE",
        "/api/plc-connections/{id}",
    )
    .await?;
    let status = state.controller.status();
    if status.state != CollectionState::Stopped {
        return queue_pending_registry_change(
            &state,
            &headers,
            "plc_connections.delete",
            json!({ "id": id }),
            status,
        )
        .await;
    }
    require_collection_stopped(&state)?;
    let mut tx = state
        .manager
        .pool()
        .begin()
        .await
        .map_err(storage_api_error)?;
    if let Err(err) = state.plc_connections.delete_tx(&mut tx, id).await {
        let _ = tx.rollback().await;
        return Err(ApiError(err).into());
    }
    let snapshot = match preflight_transaction(&mut tx).await {
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
        "plc_connections",
        &id.to_string(),
        None,
    )
    .await;
    commit_catalog_and_notify(
        &state.manager,
        &state.controller,
        &state.events,
        "plc_connections",
        snapshot,
        state.legacy_live_reconfigure,
    )
    .await;
    Ok(StatusCode::NO_CONTENT.into_response())
}

// --- T12 (docs/ux-plan.md §4): 保存前の接続テスト ---------------------------
//
// `POST /api/plc-connections/test` は保存不要でホスト/ポート/プロトコルの
// 疎通を確認する - TCP 接続だけでなく実プロトコルで軽い読み出し1回
// (先頭デバイス1点)まで行うことで、ポートは開いているがプロトコル不一致、
// という誤設定も検出する(§4 の設計方針)。レジストリへの書き込みが一切
// 発生しない読み取り専用の疎通確認なので、`record_write`/`rebuild_and_notify`
// は呼ばない。
//
// 重要な制約(実機 R08ENCPU、`crates/banto-broker/src/lib.rs`のモジュール
// doc、`crate::broker_glue`のモジュール doc「Session sync policy」節参照):
// 三菱 SLMP は対象ポートが既に別の接続で使用中だと同じポートへの2本目を
// 受け付けない(2026-08-07 実機確認: ポート毎に1接続、CPU側で複数ポートを
// 開けていれば複数同時セッションは可能)ため、保存済み接続(`connectionId`
// あり)のテストは、既存の broker セッションが生きていればそれを再利用して
// 読み、無い場合のみ直接ダイヤルする([`test_slmp_connection`]参照)。

/// 接続テストの疎通確認に使うタイムアウト(接続・応答とも共通)。数秒固定
/// (ux-plan.md §4「タイムアウトは短め（数秒）に固定」)。
const PLC_TEST_TIMEOUT: Duration = Duration::from_secs(3);

/// Modbus 直接ダイヤル失敗時に付けるセッション上限ヒント - 「軽く付けてよい」
/// 扱い(実機依存が明確でないため断定しない)。
const MODBUS_SESSION_HINT: &str =
    " 対象PLCが既に別セッションと接続中の場合、機種によっては同時接続数の上限により失敗することがあります。";

/// SLMP 直接ダイヤル失敗時に付けるセッション上限ヒント - 実機 R08ENCPU で
/// 「対象ポートが既に別の接続で使用中」だと2本目を受け付けない実測がある
/// ため必須ヒントとする。
const SLMP_SESSION_HINT: &str = " 対象ポートが既に別の接続(この hub の収集や他アプリ)で使用中の可能性があります。SLMPは同一ポートへの2本目の接続を受け付けないことがあります(実機R08ENCPUで確認済み)。";

/// `POST /api/plc-connections/test` のリクエストボディ - 保存前のフォーム値を
/// そのまま受け取る(`PlcConnectionPayload`とは別型: 接続 id を持たないのが
/// 通常で、保存済み接続の編集中のみ`connectionId`を添える)。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlcConnectionTestPayload {
    pub protocol: String,
    pub host: String,
    pub port: i64,
    #[serde(default = "default_plc_unit_id")]
    pub unit_id: i64,
    /// フォームの「シミュレーションモード」チェックボックスの現在値。
    /// シミュレーション接続のテストは常に内蔵シミュレータへ繋がり無意味なので
    /// 拒否する(ux-plan.md §4「protocol: virtual と simulation: true 相当の
    /// テストは明示エラー」)。保存済み行の simulation フラグではなく、
    /// フォームの現在値をそのまま送らせる設計(未保存の編集中でも即座に弾ける
    /// ようにするため)。
    #[serde(default)]
    pub simulation: bool,
    /// 保存済み接続を編集中にテストする場合のみ送る。broker 経由の既存
    /// セッション判定にのみ使う(接続情報自体は上記 host/port/protocol/unitId
    /// を毎回使う - フォームの現在入力値をテストするのがこの API の目的の
    /// ため)。
    #[serde(default)]
    pub connection_id: Option<i64>,
}

/// `POST /api/plc-connections/test` の応答。`ToSchema` は付けない - 下記
/// `plc_connections_test`のdoc comment参照。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlcConnectionTestResponse {
    pub ok: bool,
    pub elapsed_ms: u64,
    pub error: Option<PlcConnectionTestError>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlcConnectionTestError {
    /// "tcp" | "timeout" | "protocol" | "device" | "unsupported"
    pub kind: String,
    /// 日本語、対処ヒント込み。
    pub message: String,
}

/// [`PlcError`]を[`PlcConnectionTestError`]に分類する(Modbus・SLMP直接
/// ダイヤル共用、`crates/banto-plc/src/error.rs`の各バリアント参照)。
/// `hint`が`Some`のとき、分類結果の`kind`が`"tcp"`または`"timeout"`のときだけ
/// 末尾に付加する(それ以外の kind には無関係な文言なので付けない) -
/// 呼び出し側は「セッション上限ヒントを付けたい経路」でだけ`Some`を渡す。
/// broker 経由の既存セッション再利用経路(`test_slmp_connection`の前半)は
/// このヒントが無関係( 「2本目をダイヤルしない」ことそのものが対策なので)
/// なので常に`None`を渡す。
fn classify_plc_error(err: &PlcError, hint: Option<&str>) -> PlcConnectionTestError {
    let (kind, message) = match err {
        PlcError::ConnectTimeout(_) => (
            "timeout",
            "接続タイムアウトです(3秒)。ホスト/ポート、ネットワーク到達性を確認してください。"
                .to_string(),
        ),
        PlcError::ResponseTimeout => (
            "timeout",
            "応答タイムアウトです(3秒)。接続はできましたが応答がありませんでした。".to_string(),
        ),
        PlcError::Connection(msg) => (
            "tcp",
            format!("TCP接続に失敗しました(ポートが閉じている、または到達できません): {msg}"),
        ),
        PlcError::Protocol(msg) => (
            "protocol",
            format!(
                "プロトコルエラー: 応答が不正です({msg})。プロトコル選択やポート番号が実機と一致しているか確認してください。"
            ),
        ),
        PlcError::ModbusException { message, .. } | PlcError::SlmpEndCode { message, .. } => (
            "device",
            format!(
                "デバイス読み出しエラー: 接続はできましたが、指定したデバイス/レジスタを読み出せませんでした({message})。アドレス設定や機種依存の可能性があります(致命的ではありません)。"
            ),
        ),
        // NotConnected / InvalidAddress / AddressProtocolMismatch /
        // UnsupportedCombination / StringSpanUnsupported - このAPIは固定
        // アドレス("40001"/"D0")しか使わないので通常発生しない防御的ケース。
        other => ("device", format!("読み出しに失敗しました: {other}")),
    };
    let mut message = message;
    if matches!(kind, "tcp" | "timeout") {
        if let Some(hint) = hint {
            message.push_str(hint);
        }
    }
    PlcConnectionTestError {
        kind: kind.to_string(),
        message,
    }
}

/// Modbus TCP の接続テスト - 直接ダイヤルのみ(Modbusには broker/共有
/// セッションの概念がない)。ポート/ユニットIDの範囲検証 →
/// `ModbusTcpClient::connect` → 保持レジスタ先頭1点の`read_batch` → 必ず
/// `disconnect`、の順で行う。
async fn test_modbus_connection(
    payload: &PlcConnectionTestPayload,
) -> (bool, Option<PlcConnectionTestError>) {
    let port = match u16::try_from(payload.port) {
        Ok(port) => port,
        Err(_) => {
            return (
                false,
                Some(PlcConnectionTestError {
                    kind: "tcp".to_string(),
                    message: "ポート番号が不正です(1〜65535の範囲で指定してください)。".to_string(),
                }),
            );
        }
    };
    let unit_id = match u8::try_from(payload.unit_id) {
        Ok(unit_id) => unit_id,
        Err(_) => {
            return (
                false,
                Some(PlcConnectionTestError {
                    kind: "tcp".to_string(),
                    message: "ユニットID(スレーブID)が不正です(0〜255の範囲で指定してください)。"
                        .to_string(),
                }),
            );
        }
    };

    let config = ModbusTcpConfig {
        host: payload.host.clone(),
        port,
        unit_id,
        connect_timeout: PLC_TEST_TIMEOUT,
        response_timeout: PLC_TEST_TIMEOUT,
        ..Default::default()
    };
    let mut client = ModbusTcpClient::new(config);

    if let Err(err) = client.connect().await {
        return (
            false,
            Some(classify_plc_error(&err, Some(MODBUS_SESSION_HINT))),
        );
    }

    // 保持レジスタ先頭を1点読む - テストで固定した文字列リテラルなので parse
    // は失敗し得ない。
    let requests = [ReadRequest {
        address: Address::parse("40001").expect("valid literal"),
        data_type: DataType::U16,
    }];
    let read_result = client.read_batch(&requests).await;
    client.disconnect().await;

    match read_result {
        Ok(mut results) => match results.pop() {
            Some(ReadResult::Value(_)) => (true, None),
            Some(ReadResult::Bad(err)) => (
                false,
                Some(classify_plc_error(&err, Some(MODBUS_SESSION_HINT))),
            ),
            None => (
                false,
                Some(PlcConnectionTestError {
                    kind: "device".to_string(),
                    message: "読み出し結果が空でした。".to_string(),
                }),
            ),
        },
        Err(err) => (
            false,
            Some(classify_plc_error(&err, Some(MODBUS_SESSION_HINT))),
        ),
    }
}

/// SLMP の接続テスト。`payload.connection_id`があり、その接続の broker
/// セッションが既に生きていれば、それを再利用して読む(新規ダイヤルしない -
/// 実機 R08ENCPU は対象ポートが既に使用中だと2本目を受け付けないため、これを
/// 誤診しないための対策。このモジュール冒頭のコメント参照)。無ければ直接
/// ダイヤルにフォールバックする。
async fn test_slmp_connection(
    state: &TagRegistryState,
    payload: &PlcConnectionTestPayload,
) -> (bool, Option<PlcConnectionTestError>) {
    if let Some(connection_id) = payload.connection_id {
        if let Some(handle) = state.manager.sessions().handle_for(connection_id) {
            let requests = vec![BatchReadRequest::Numeric(ReadRequest {
                address: Address::parse_slmp("D0").expect("valid literal"),
                data_type: DataType::U16,
            })];
            // `ReadOnlyHandle::read`自体には外側タイムアウトが無いため、
            // ここで明示的に包む(実装指示どおり)。
            return match tokio::time::timeout(PLC_TEST_TIMEOUT, handle.read(requests)).await {
                Err(_elapsed) => (
                    false,
                    Some(PlcConnectionTestError {
                        kind: "timeout".to_string(),
                        message: "応答タイムアウトです(3秒)。共有セッションが応答しませんでした。"
                            .to_string(),
                    }),
                ),
                Ok(Err(BrokerError::Disconnected { .. })) => (
                    false,
                    Some(PlcConnectionTestError {
                        kind: "tcp".to_string(),
                        message: "この接続の共有セッションは現在切断中です(再接続待機中)。PLCの電源やネットワーク、または他アプリとのセッション競合を確認してください。"
                            .to_string(),
                    }),
                ),
                Ok(Err(BrokerError::ConnectionFailed { reason, .. })) => (
                    false,
                    Some(PlcConnectionTestError {
                        kind: "tcp".to_string(),
                        message: format!("共有セッションが接続断で失敗しました: {reason}"),
                    }),
                ),
                Ok(Err(BrokerError::TaskGone { .. })) => (
                    false,
                    Some(PlcConnectionTestError {
                        kind: "tcp".to_string(),
                        message: "内部エラー: セッションタスクが終了しています。".to_string(),
                    }),
                ),
                // 防御的フォールバック: この経路では通常発生しない
                // (UnsupportedProtocol/InvalidPortはensure_connection時点で
                // 弾かれているはず)。
                Ok(Err(err @ BrokerError::UnsupportedProtocol { .. }))
                | Ok(Err(err @ BrokerError::InvalidPort { .. })) => (
                    false,
                    Some(PlcConnectionTestError {
                        kind: "protocol".to_string(),
                        message: err.to_string(),
                    }),
                ),
                // 既存セッション再利用経路: セッション上限ヒントは付けない
                // (design の意図: 「2本目をダイヤルしない」こと自体が対策)。
                Ok(Ok(mut results)) => match results.pop() {
                    Some(BatchReadResult::Value(_)) => (true, None),
                    Some(BatchReadResult::Bad(err)) => {
                        (false, Some(classify_plc_error(&err, None)))
                    }
                    None => (
                        false,
                        Some(PlcConnectionTestError {
                            kind: "device".to_string(),
                            message: "読み出し結果が空でした。".to_string(),
                        }),
                    ),
                },
            };
        }
    }

    // 直接ダイヤル(connectionId が無い、またはセッションが見つからない場合)。
    // SLMP は unit_id を使わないので port のみ検証する。
    let port = match u16::try_from(payload.port) {
        Ok(port) => port,
        Err(_) => {
            return (
                false,
                Some(PlcConnectionTestError {
                    kind: "tcp".to_string(),
                    message: "ポート番号が不正です(1〜65535の範囲で指定してください)。".to_string(),
                }),
            );
        }
    };

    let config = SlmpConfig {
        host: payload.host.clone(),
        port,
        connect_timeout: PLC_TEST_TIMEOUT,
        response_timeout: PLC_TEST_TIMEOUT,
        ..Default::default()
    };
    let mut client = SlmpClient::new(config);

    if let Err(err) = client.connect().await {
        return (
            false,
            Some(classify_plc_error(&err, Some(SLMP_SESSION_HINT))),
        );
    }

    let requests = [ReadRequest {
        address: Address::parse_slmp("D0").expect("valid literal"),
        data_type: DataType::U16,
    }];
    let read_result = client.read_batch(&requests).await;
    client.disconnect().await;

    match read_result {
        Ok(mut results) => match results.pop() {
            Some(ReadResult::Value(_)) => (true, None),
            Some(ReadResult::Bad(err)) => (
                false,
                Some(classify_plc_error(&err, Some(SLMP_SESSION_HINT))),
            ),
            None => (
                false,
                Some(PlcConnectionTestError {
                    kind: "device".to_string(),
                    message: "読み出し結果が空でした。".to_string(),
                }),
            ),
        },
        Err(err) => (
            false,
            Some(classify_plc_error(&err, Some(SLMP_SESSION_HINT))),
        ),
    }
}

/// `POST /api/plc-connections/test` - T12(docs/ux-plan.md §4)。保存前に
/// host/port/protocol の疎通確認(実プロトコルでの軽い読み出し1回まで)を
/// 行う。**意図的に** `#[utoipa::path]`を付けず`ApiDoc::paths(...)`にも
/// 加えない - utoipa がドキュメント対象にしているのは`/api/v1/*`の機械
/// クライアント向け API のみで(このファイル冒頭「二系統に分かれた
/// ルーター」節参照)、他の`/api/plc-connections/*`管理ハンドラ
/// (create/update/delete)も同様に対象外にしている。ここだけドキュメント
/// 対象を広げると既存方針と矛盾するため、意図的に対象外のままにする。
///
/// 権限は他の I1 書き込みハンドラと同じ`require_editor`。CSRF は
/// `tag_registry_router`が属する管理系ルーター全体に既に`require_banto_client_header`
/// がかかっているので、ここでの追加対応は不要。
///
/// 失敗(`ok: false`)は通常の 200 応答として返す(`tags_batch`と同じ
/// 「ok:false は通常の応答」という判断) - レジストリへの書き込みが一切
/// 発生しない読み取り専用の疎通確認なので、監査ログ記録(`record_write`)や
/// `rebuild_and_notify`は呼ばない。
async fn plc_connections_test(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Json(payload): Json<PlcConnectionTestPayload>,
) -> Result<Json<PlcConnectionTestResponse>, ApiError> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "plc_connections",
        "POST",
        "/api/plc-connections/test",
    )
    .await?;

    let started = std::time::Instant::now();

    // ガード: virtual / simulation / 未知プロトコルは即座に ok:false, kind:
    // "unsupported"(実プロトコルの疎通確認より前に判定)。
    let (ok, error) = if payload.protocol == "virtual" {
        (
            false,
            Some(PlcConnectionTestError {
                kind: "unsupported".to_string(),
                message: "virtual接続はテスト対象外です(calc/mem予約接続)。".to_string(),
            }),
        )
    } else if payload.simulation {
        (
            false,
            Some(PlcConnectionTestError {
                kind: "unsupported".to_string(),
                message: "シミュレーション接続はテスト不要です(常に内蔵シミュレータに接続され、常に成功します)。"
                    .to_string(),
            }),
        )
    } else {
        match payload.protocol.as_str() {
            "modbus-tcp" => test_modbus_connection(&payload).await,
            "slmp" => test_slmp_connection(&state, &payload).await,
            other => (
                false,
                Some(PlcConnectionTestError {
                    kind: "unsupported".to_string(),
                    message: format!("不明なプロトコルです: {other}"),
                }),
            ),
        }
    };

    Ok(Json(PlcConnectionTestResponse {
        ok,
        elapsed_ms: started.elapsed().as_millis() as u64,
        error,
    }))
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
    let status = state.controller.status();
    if status.state != CollectionState::Stopped {
        return queue_pending_registry_change(
            &state,
            &headers,
            "collection_groups.create",
            json!({ "input": input }),
            status,
        )
        .await;
    }
    require_collection_stopped(&state)?;
    let mut tx = state
        .manager
        .pool()
        .begin()
        .await
        .map_err(storage_api_error)?;
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
    let snapshot = match preflight_transaction(&mut tx).await {
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
        &created.id.to_string(),
        Some(json!({ "name": created.name, "enabled": created.enabled })),
    )
    .await;
    commit_catalog_and_notify(
        &state.manager,
        &state.controller,
        &state.events,
        "collection_groups",
        snapshot,
        state.legacy_live_reconfigure,
    )
    .await;
    Ok(Json(created).into_response())
}

async fn collection_groups_update(
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
    let status = state.controller.status();
    if status.state != CollectionState::Stopped {
        return queue_pending_registry_change(
            &state,
            &headers,
            "collection_groups.update",
            json!({ "id": id, "input": input }),
            status,
        )
        .await;
    }
    require_collection_stopped(&state)?;
    let mut tx = state
        .manager
        .pool()
        .begin()
        .await
        .map_err(storage_api_error)?;
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
    let snapshot = match preflight_transaction(&mut tx).await {
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
        &id.to_string(),
        Some(json!({ "name": updated.name, "enabled": updated.enabled })),
    )
    .await;
    commit_catalog_and_notify(
        &state.manager,
        &state.controller,
        &state.events,
        "collection_groups",
        snapshot,
        state.legacy_live_reconfigure,
    )
    .await;
    Ok(Json(updated).into_response())
}

async fn collection_groups_delete(
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
    let status = state.controller.status();
    if status.state != CollectionState::Stopped {
        return queue_pending_registry_change(
            &state,
            &headers,
            "collection_groups.delete",
            json!({ "id": id }),
            status,
        )
        .await;
    }
    require_collection_stopped(&state)?;
    let mut tx = state
        .manager
        .pool()
        .begin()
        .await
        .map_err(storage_api_error)?;
    if let Err(err) = state.collection_groups.delete_tx(&mut tx, id).await {
        let _ = tx.rollback().await;
        return Err(ApiError(err).into());
    }
    let snapshot = match preflight_transaction(&mut tx).await {
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
        &id.to_string(),
        None,
    )
    .await;
    commit_catalog_and_notify(
        &state.manager,
        &state.controller,
        &state.events,
        "collection_groups",
        snapshot,
        state.legacy_live_reconfigure,
    )
    .await;
    Ok(StatusCode::NO_CONTENT.into_response())
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
) -> RegistryMutationResult<Response> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "tags",
        "POST",
        "/api/tags",
    )
    .await?;
    let status = state.controller.status();
    if status.state != CollectionState::Stopped {
        return queue_pending_registry_change(
            &state,
            &headers,
            "tags.create",
            json!({ "input": input }),
            status,
        )
        .await;
    }
    let mut tx = state
        .manager
        .pool()
        .begin()
        .await
        .map_err(storage_api_error)?;
    let created = match state.tags.create_tx(&mut tx, input.into()).await {
        Ok(created) => created,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(ApiError(err).into());
        }
    };
    let snapshot = match preflight_transaction(&mut tx).await {
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
        "tags",
        &created.id.to_string(),
        Some(json!({ "name": created.name, "enabled": created.enabled })),
    )
    .await;
    commit_catalog_and_notify(
        &state.manager,
        &state.controller,
        &state.events,
        "tags",
        snapshot,
        state.legacy_live_reconfigure,
    )
    .await;
    Ok(Json(created).into_response())
}

async fn tags_update(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(input): Json<TagPayload>,
) -> RegistryMutationResult<Response> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "tags",
        "PUT",
        "/api/tags/{id}",
    )
    .await?;
    let status = state.controller.status();
    if status.state != CollectionState::Stopped {
        return queue_pending_registry_change(
            &state,
            &headers,
            "tags.update",
            json!({ "id": id, "input": input }),
            status,
        )
        .await;
    }
    let mut tx = state
        .manager
        .pool()
        .begin()
        .await
        .map_err(storage_api_error)?;
    let updated = match state.tags.update_tx(&mut tx, id, input.into()).await {
        Ok(updated) => updated,
        // T18-1: `TagUpdateError::RevisionConflict` の場合もロールバックは
        // 必須 - preflight/catalog を一切見ないまま抜けるので、ここで
        // 既にトランザクションは何も commit していない状態だが、明示的に
        // rollback して SQLite の接続をトランザクション外に戻す
        // （`CollectionEditLocked` 等、既存の早期 return と同じ作法）。
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(err.into());
        }
    };
    let snapshot = match preflight_transaction(&mut tx).await {
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
        "tags",
        &id.to_string(),
        Some(json!({ "name": updated.name, "enabled": updated.enabled })),
    )
    .await;
    commit_catalog_and_notify(
        &state.manager,
        &state.controller,
        &state.events,
        "tags",
        snapshot,
        state.legacy_live_reconfigure,
    )
    .await;
    Ok(Json(updated).into_response())
}

async fn tags_delete(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> RegistryMutationResult<Response> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "tags",
        "DELETE",
        "/api/tags/{id}",
    )
    .await?;
    let status = state.controller.status();
    if status.state != CollectionState::Stopped {
        return queue_pending_registry_change(
            &state,
            &headers,
            "tags.delete",
            json!({ "id": id }),
            status,
        )
        .await;
    }
    let mut tx = state
        .manager
        .pool()
        .begin()
        .await
        .map_err(storage_api_error)?;
    if let Err(err) = state.tags.delete_tx(&mut tx, id).await {
        let _ = tx.rollback().await;
        return Err(ApiError(err).into());
    }
    let snapshot = match preflight_transaction(&mut tx).await {
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
        "tags",
        &id.to_string(),
        None,
    )
    .await;
    commit_catalog_and_notify(
        &state.manager,
        &state.controller,
        &state.events,
        "tags",
        snapshot,
        state.legacy_live_reconfigure,
    )
    .await;
    Ok(StatusCode::NO_CONTENT.into_response())
}

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
    legacy_live_reconfigure: bool,
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

enum PendingApplyError {
    Api(ApiError),
    CollectionEditLocked(CollectionStatusResponse),
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

impl PendingApplyError {
    fn reason(&self) -> String {
        match self {
            Self::Api(err) => format!("pending change の適用に失敗しました: {}", err.0),
            Self::CollectionEditLocked(status) => {
                format!("収集中は構成を編集できません(state={})", status.state)
            }
            Self::Conflict { resource } => pending_apply_conflict_message(resource),
        }
    }
}

impl IntoResponse for PendingApplyError {
    fn into_response(self) -> Response {
        match self {
            Self::Api(err) => err.into_response(),
            Self::CollectionEditLocked(status) => (
                StatusCode::CONFLICT,
                Json(json!({
                    "error": "collection_edit_locked",
                    "state": status.state,
                    "status": status,
                    "message": "収集中は構成を編集できません。停止してから再試行してください。"
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

async fn execute_pending_apply(
    state: &PendingChangesAdminState,
    pending: &PendingChange,
) -> Result<(), PendingApplyError> {
    let status = state.controller.status();
    if status.state != CollectionState::Stopped {
        return Err(PendingApplyError::CollectionEditLocked(status.into()));
    }

    let mut tx = state
        .manager
        .pool()
        .begin()
        .await
        .map_err(storage_api_error)
        .map_err(PendingApplyError::Api)?;

    let resource = match pending.source.as_str() {
        "plc_connections.create" => {
            let body: PendingChangeWithInput<PlcConnectionPayload> =
                decode_pending_payload(pending)?;
            state
                .plc_connections
                .create_tx(&mut tx, body.input.into())
                .await
                .map_err(ApiError)
                .map_err(PendingApplyError::Api)?;
            "plc_connections"
        }
        "plc_connections.update" => {
            let body: PendingChangeWithIdAndInput<PlcConnectionPayload> =
                decode_pending_payload(pending)?;
            if let Some(expected) = &pending.base_fingerprint {
                // Plain pool read (not part of `tx` above) — same source as
                // `compute_pending_base_fingerprint` used at enqueue time.
                // The `apply_lock` mutex in `PendingChangesAdminState`
                // already serializes concurrent apply calls against each
                // other, which is the concurrency case this guard closes
                // (edits made *before* Apply was clicked, not sub-request
                // races during Apply itself).
                check_fingerprint_unchanged(
                    state.plc_connections.get(body.id).await,
                    expected,
                    "plc_connections",
                )
                .await?;
            }
            state
                .plc_connections
                .update_tx(&mut tx, body.id, body.input.into())
                .await
                .map_err(ApiError)
                .map_err(PendingApplyError::Api)?;
            "plc_connections"
        }
        "plc_connections.delete" => {
            let body: PendingChangeWithId = decode_pending_payload(pending)?;
            if let Some(expected) = &pending.base_fingerprint {
                check_fingerprint_unchanged(
                    state.plc_connections.get(body.id).await,
                    expected,
                    "plc_connections",
                )
                .await?;
            }
            state
                .plc_connections
                .delete_tx(&mut tx, body.id)
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
            state
                .collection_groups
                .delete_tx(&mut tx, body.id)
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
        other => {
            return Err(PendingApplyError::Api(preflight_api_error(format!(
                "未対応の pending source です: {other}"
            ))));
        }
    };

    let snapshot = match preflight_transaction(&mut tx).await {
        Ok(snapshot) => snapshot,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(PendingApplyError::Api(err));
        }
    };

    if let Err(err) = tx.commit().await {
        return Err(PendingApplyError::Api(storage_api_error(err)));
    }

    commit_catalog_and_notify(
        &state.manager,
        &state.controller,
        &state.events,
        resource,
        snapshot,
        state.legacy_live_reconfigure,
    )
    .await;

    Ok(())
}

async fn pending_changes_list(
    State(state): State<PendingChangesAdminState>,
    Query(query): Query<PendingChangesQuery>,
) -> Result<Json<Vec<PendingChange>>, ApiError> {
    let limit = query.limit.unwrap_or(100).clamp(1, 1000);
    Ok(Json(state.pending_changes.list(limit).await?))
}

async fn pending_changes_get(
    State(state): State<PendingChangesAdminState>,
    Path(id): Path<i64>,
) -> Result<Json<PendingChange>, ApiError> {
    Ok(Json(state.pending_changes.get(id).await?))
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
        &id.to_string(),
        None,
    )
    .await;
    Ok(Json(pending))
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
        let failed = match state.pending_changes.mark_failed(id, &failure_reason).await {
            Ok(failed) => Some(failed),
            Err(mark_err) => {
                eprintln!(
                    "banto-hub: pending_change={id} の failed 遷移に失敗しました: {mark_err}"
                );
                None
            }
        };

        return match err {
            PendingApplyError::CollectionEditLocked(status) => (
                StatusCode::CONFLICT,
                Json(json!({
                    "error": "collection_edit_locked",
                    "state": status.state,
                    "status": status,
                    "message": "収集中は構成を編集できません。停止してから再試行してください。",
                    "failureReason": failure_reason,
                    "pending": failed,
                })),
            )
                .into_response(),
            PendingApplyError::Api(err) => err.into_response(),
            // TAG-P0-3 follow-up（2026-08-12）: `IntoResponse` 実装がそのまま
            // 409 + conflict body を返す。`failure_reason` は既に
            // `mark_failed` で DB へ記録済みなので、`GET
            // /api/pending-changes/{id}` から後追いで確認できる。
            err @ PendingApplyError::Conflict { .. } => err.into_response(),
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
        &id.to_string(),
        Some(json!({ "source": applying.source })),
    )
    .await;
    Json(applied).into_response()
}

#[allow(clippy::too_many_arguments)]
fn pending_changes_router(
    pending_changes: PendingChangesService,
    plc_connections: PlcConnectionService,
    collection_groups: CollectionGroupService,
    tags: TagService,
    audit: AuditLogService,
    auth: AuthState,
    manager: Arc<CollectorManager>,
    controller: Arc<CollectionController>,
    events: broadcast::Sender<ServerEvent>,
    legacy_live_reconfigure: bool,
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
        legacy_live_reconfigure,
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
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            RoleGuard {
                auth: auth.clone(),
                min: Role::Admin,
                resource: "pending_changes",
                audit,
            },
            require_role_at_least,
        ))
        .layer(middleware::from_fn_with_state(auth, require_auth))
}

// --- T11-1 一括登録 API (docs/ux-plan.md §3): 連続登録 UI と T11-2 の CSV
// インポートが共有する基盤 - transaction内検証 → all-or-nothing 適用 →
// catalog commit 1回。
// パターン展開（名前パターン/連続アドレス生成）はクライアント側（TS、
// `apps/banto-hub/src/lib/banto/continuousRegistration.ts`）が担い、この
// エンドポイントは展開済みの `TagInput` 配列を受け取るだけの汎用一括 API
// のまま保つ（設計: 「展開結果を一括 API に渡す方式」）。

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchTagsRequest {
    pub tags: Vec<TagPayload>,
    /// `true`: 検証結果だけを返す（DB 無変更）。`false`（既定）: 検証 →
    /// 単一トランザクションで全件 INSERT → catalog commit を1回。
    #[serde(default)]
    pub dry_run: bool,
}

// Note: none of the three response types below derive `utoipa::ToSchema` -
// `banto_tags::Tag` (embedded in `BatchTagsResponse::tags`) does not
// implement it, and (like `IssuedApiKeyResponse`/`GrpcSettingsBody` above)
// this admin-surface endpoint is not part of `ApiDoc`'s documented
// `/api/v1/*` schema anyway (see this module's doc comment: only the
// machine-facing tag-space API is utoipa-documented).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchTagFieldErrorResponse {
    field: String,
    message: String,
}

impl From<FieldError> for BatchTagFieldErrorResponse {
    fn from(err: FieldError) -> Self {
        Self {
            field: err.field,
            message: err.message,
        }
    }
}

/// 行番号(0起点)付きのフィールドエラー一覧 - 設計「行番号/インデックス
/// 付きのエラー一覧」。CSV インポート(T11-2)ではこの `index` がそのまま
/// CSV の行番号(ヘッダ行を除く0起点データ行)に対応する想定。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchTagRowErrorResponse {
    index: usize,
    field_errors: Vec<BatchTagFieldErrorResponse>,
}

impl From<banto_tags::BatchTagError> for BatchTagRowErrorResponse {
    fn from(err: banto_tags::BatchTagError) -> Self {
        Self {
            index: err.index,
            field_errors: err.field_errors.into_iter().map(Into::into).collect(),
        }
    }
}

/// `POST /api/tags/batch` の応答。**常に 200** で返す(判断: 2026-08-07) -
/// 「1件でも不正なら全体拒否」という結果は例外ではなく、dry run の検証
/// レポートと地続きの通常の応答だから。認証/権限エラー(401/403)や DB
/// レベルの想定外エラー(500)は既存の `ApiError` 経路(非2xx + `ErrorBody`)
/// のまま区別する。`ok: false` のときクライアントは `errors` を行ごとに
/// 表示する(`apps/banto-hub/src/lib/banto/tagRegistryAdmin.ts` 参照)。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchTagsResponse {
    ok: bool,
    dry_run: bool,
    /// 適用された(または dry run で適用されたはずの)件数。`ok: false` の
    /// ときは常に 0。
    count: usize,
    errors: Vec<BatchTagRowErrorResponse>,
    /// `ok: true && dry_run: false` のときだけ `Some`(実際に作成された
    /// タグ)。dry run では何も書き込まないので `None`。
    #[serde(skip_serializing_if = "Option::is_none")]
    tags: Option<Vec<Tag>>,
}

/// `POST /api/tags/batch` - T11-1。editor 以上、CSRF は `/api/tags/*` と
/// 同じ管理系ルーターの層で一括適用される(`tag_registry_router` 参照)。
async fn tags_batch(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Json(body): Json<BatchTagsRequest>,
) -> RegistryMutationResult<Response> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "tags",
        "POST",
        "/api/tags/batch",
    )
    .await?;

    if !body.dry_run {
        let status = state.controller.status();
        if status.state != CollectionState::Stopped {
            return queue_pending_registry_change(
                &state,
                &headers,
                "tags.batch_create",
                json!({ "dryRun": false, "tags": body.tags }),
                status,
            )
            .await;
        }
        require_collection_stopped(&state)?;
    }

    let dry_run = body.dry_run;
    let inputs: Vec<TagInput> = body.tags.into_iter().map(Into::into).collect();

    if inputs.is_empty() {
        return Ok(Json(BatchTagsResponse {
            ok: true,
            dry_run,
            count: 0,
            errors: Vec::new(),
            tags: (!dry_run).then(Vec::new),
        })
        .into_response());
    }

    let mut tx = state
        .manager
        .pool()
        .begin()
        .await
        .map_err(storage_api_error)?;
    let outcome = match state.tags.create_batch_tx(&mut tx, &inputs).await {
        Ok(outcome) => outcome,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(ApiError(err).into());
        }
    };
    match outcome {
        BatchTagOutcome::Invalid(errors) => {
            let _ = tx.rollback().await;
            Ok(Json(BatchTagsResponse {
                ok: false,
                dry_run,
                count: 0,
                errors: errors.into_iter().map(Into::into).collect(),
                tags: None,
            })
            .into_response())
        }
        BatchTagOutcome::Valid { count, tags } => {
            let snapshot = match preflight_transaction(&mut tx).await {
                Ok(snapshot) => snapshot,
                Err(err) => {
                    let _ = tx.rollback().await;
                    return Err(err.into());
                }
            };
            if dry_run {
                tx.rollback().await.map_err(storage_api_error)?;
            } else {
                tx.commit().await.map_err(storage_api_error)?;
                record_write(
                    &state.audit,
                    &state.auth,
                    &headers,
                    "batch_create",
                    "tags",
                    "-",
                    Some(json!({ "count": count })),
                )
                .await;
                // T11-1 の核心: n 件でも catalog commit はここで1回だけ。
                commit_catalog_and_notify(
                    &state.manager,
                    &state.controller,
                    &state.events,
                    "tags",
                    snapshot,
                    state.legacy_live_reconfigure,
                )
                .await;
            }
            Ok(Json(BatchTagsResponse {
                ok: true,
                dry_run,
                count,
                errors: Vec::new(),
                tags: if dry_run { None } else { tags },
            })
            .into_response())
        }
    }
}

/// `/api/plc-connections/*` + `/api/collection-groups/*` + `/api/tags/*`
/// (viewer-read / editor-write) - `relay-wright-core::rest::tag_registry_router`
/// を雛形に、書き込み成功後に catalog commit と SSE通知を行う。
#[allow(clippy::too_many_arguments)]
fn tag_registry_router(
    plc_connections: PlcConnectionService,
    collection_groups: CollectionGroupService,
    tags: TagService,
    pending_changes: PendingChangesService,
    audit: AuditLogService,
    auth: AuthState,
    manager: Arc<CollectorManager>,
    controller: Arc<CollectionController>,
    events: broadcast::Sender<ServerEvent>,
    legacy_live_reconfigure: bool,
) -> Router {
    let state = TagRegistryState {
        plc_connections,
        collection_groups,
        tags,
        auth: auth.clone(),
        audit,
        manager,
        controller: controller.clone(),
        events,
        pending_changes,
        legacy_live_reconfigure,
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
        // T12 (docs/ux-plan.md §4): 保存前の接続テスト - `{id}` の下ではなく
        // 固定パスなので `/api/plc-connections/{id}` と衝突しない
        // (下の `/api/tags/batch` と同型のパス設計)。
        .route("/api/plc-connections/test", post(plc_connections_test))
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
        // T11-1 (docs/ux-plan.md §3): 一括登録 - `/api/tags/{id}` の下では
        // なく `/api/tags` の下の固定パスなので、`{id}` (i64) パラメータと
        // 衝突しない。
        .route("/api/tags/batch", post(tags_batch))
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
    pub(crate) controller: Arc<CollectionController>,
    /// T2-4（設計 §6-6）: `GET /api/v1/status` の `write_enabled`/
    /// `write_was_enabled_before_restart` のため。
    pub(crate) write_control: Arc<WriteControl>,
    /// T15-3（設計 §6.3）: `GET /api/v1/status` の `test_output` のため。
    pub(crate) test_output: Arc<TestOutputControl>,
    /// T3（設計 §5.3）: `GET /api/v1/status` の `mqtt.connected` のため。
    pub(crate) mqtt: Arc<MqttPublisher>,
}

#[derive(Debug, Deserialize)]
struct TagsQuery {
    connection: Option<String>,
    group: Option<String>,
}

/// `GET /api/v1/tags` の応答: `{ "revision", "run_id",
/// "collection_mode", "tags": [CatalogTagEntry...] }`。
#[derive(Debug, Serialize, ToSchema)]
struct CatalogResponse {
    revision: u64,
    run_id: Option<u64>,
    collection_mode: String,
    tags: Vec<CatalogTagEntry>,
}

/// REST wire DTO for one catalog entry.
///
/// `TagEntry` remains the saved/configured catalog owned by `CollectorManager`.
/// The runtime fields are layered here so an all-simulation run can be
/// observed without mutating the DB-backed catalog or the shared TagMap.
#[derive(Debug, Serialize, ToSchema)]
struct CatalogTagEntry {
    #[serde(flatten)]
    entry: TagEntry,
    configured_simulation: bool,
    effective_simulation: bool,
    value_source: String,
}

impl CatalogTagEntry {
    fn from_runtime(entry: &TagEntry, runtime: &CollectionStatus) -> Self {
        Self {
            entry: entry.clone(),
            configured_simulation: entry.simulation,
            effective_simulation: effective_simulation_for_tag(entry, runtime),
            value_source: value_source_for_tag(entry, runtime).to_string(),
        }
    }
}

fn effective_simulation_for_connection(
    protocol: &str,
    enabled: bool,
    configured_simulation: bool,
    runtime: &CollectionStatus,
) -> bool {
    enabled
        && runtime.state == CollectionState::Running
        && matches!(protocol, "modbus-tcp" | "slmp")
        && (configured_simulation || runtime.mode == RunMode::AllSimulation)
}

fn effective_simulation_for_tag(entry: &TagEntry, runtime: &CollectionStatus) -> bool {
    entry.tag_kind == banto_tags::PLC_TAG_KIND
        && entry.enabled
        && runtime.state == CollectionState::Running
        && (entry.simulation || runtime.mode == RunMode::AllSimulation)
}

fn value_source_for_tag(entry: &TagEntry, runtime: &CollectionStatus) -> &'static str {
    match entry.tag_kind.as_str() {
        banto_tags::PLC_TAG_KIND if effective_simulation_for_tag(entry, runtime) => "simulation",
        banto_tags::PLC_TAG_KIND => "real",
        banto_tags::COMPUTED_TAG_KIND => "derived_simulation",
        banto_tags::INTERNAL_TAG_KIND => "internal",
        // Tag registration validates tag_kind, but keep the wire contract
        // fail-safe if a future kind is introduced without this DTO update.
        _ => "internal",
    }
}

/// API-key reads expose only the normal external value space.  Saved PLC
/// simulation configuration is hidden even while the controller is stopped,
/// while computed tags remain hidden because their source is always
/// `derived_simulation`.  Internal tags are intentionally retained for
/// backwards compatibility with existing API-key clients.
fn api_key_external_output_allowed(entry: &TagEntry, runtime: &CollectionStatus) -> bool {
    if entry.simulation {
        return false;
    }
    !matches!(
        value_source_for_tag(entry, runtime),
        "simulation" | "derived_simulation"
    )
}

/// `GET /api/v1/tags` - catalog: `{ "revision", "run_id",
/// "collection_mode", "tags": [CatalogTagEntry...] }`,
/// optionally filtered by `?connection=`/`?group=` (matched against the
/// entry's connection/group *name*, design §5.1's route table). API-key
/// requests additionally omit simulation and derived-simulation entries.
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
    ctx: Option<Extension<ApiKeyContext>>,
) -> Json<CatalogResponse> {
    let map = state.manager.tag_map();
    let revision = state.manager.revision();
    let runtime = state.controller.status();
    let api_key_request = ctx.is_some();
    let tags: Vec<CatalogTagEntry> = map
        .iter()
        .filter(|entry| !api_key_request || api_key_external_output_allowed(entry, &runtime))
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
        .map(|entry| CatalogTagEntry::from_runtime(entry, &runtime))
        .collect();
    Json(CatalogResponse {
        revision,
        run_id: runtime.run_id,
        collection_mode: runtime.mode.as_str().to_string(),
        tags,
    })
}

/// One `/api/v1/values*` entry's wire shape (design §5.1's route table:
/// `{ "tag", "v", "q", "t" }`).
#[derive(Debug, Clone, Serialize, ToSchema)]
struct ValueEntry {
    tag: String,
    v: Option<f64>,
    q: String,
    t: i64,
    value_source: String,
}

/// Thin wire-formatting wrapper over [`crate::hub::read_current`] (see its
/// doc comment - this is the "same v/q/t semantics as `crate::stream`'s
/// `data` messages" helper the T1 実装指示 asked to share rather than
/// duplicate; T6-2 widened it to also cover computed/internal tags via
/// `read_current` instead of calling `effective_sample` directly).
fn value_entry(
    external_name: &str,
    entry: &TagEntry,
    current: Option<&banto_collect::CurrentValuesHandle>,
    server_store: &crate::computed::ServerTagStore,
    now_ms: i64,
    runtime: &CollectionStatus,
) -> ValueEntry {
    let (v, q, t) = crate::hub::read_current(entry, current, server_store, now_ms);
    ValueEntry {
        tag: external_name.to_string(),
        v,
        q: crate::hub::quality_str(q).to_string(),
        t,
        value_source: value_source_for_tag(entry, runtime).to_string(),
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
    run_id: Option<u64>,
    collection_mode: String,
    values: Vec<ValueEntry>,
}

/// Single-value response. The bulk response carries the same run metadata at
/// the response level, while each value carries its own source classification.
#[derive(Debug, Serialize, ToSchema)]
struct SingleValueResponse {
    tag: String,
    v: Option<f64>,
    q: String,
    t: i64,
    run_id: Option<u64>,
    collection_mode: String,
    value_source: String,
}

impl SingleValueResponse {
    fn from_value(value: ValueEntry, runtime: &CollectionStatus) -> Self {
        Self {
            tag: value.tag,
            v: value.v,
            q: value.q,
            t: value.t,
            run_id: runtime.run_id,
            collection_mode: runtime.mode.as_str().to_string(),
            value_source: value.value_source,
        }
    }
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
///
/// H10 ③(Option B、docs/h10-3-read-scope-proposal.md §5 S4): API キー起因
/// の読み取り(`ctx` あり)だけがタグ単位スコープで絞られる。`?tags=`
/// 省略(暗黙の全件)はスコープ外を**黙って除いた**集合を返す(「聞いても
/// いないのに403」を避ける)。`?tags=` で明示的にスコープ外タグを挙げたら
/// [`v1_value_single`] と同じ**403**(存在は catalog 経由で既知なので
/// 404 ではない)。セッション token(`ctx` 無し)は従来どおり全件(管理 UI
/// 不変)。API キー時はこのスコープ判定後に simulation / derived_simulation
/// を値一覧から除外する。
#[utoipa::path(
    get,
    path = "/api/v1/values",
    params(
        ("tags" = Option<String>, Query, description = "カンマ区切りの外部名。省略時は全タグ"),
    ),
    responses(
        (status = 200, description = "現在値スナップショット", body = ValuesResponse),
        (status = 400, description = "?tags= に未知の外部名が含まれる"),
        (status = 403, description = "?tags= に per-tag read スコープ外の外部名が含まれる(API キー、H10 ③)"),
    ),
    tag = "tag-space",
)]
async fn v1_values(
    State(state): State<TagSpaceState>,
    Query(query): Query<ValuesQuery>,
    ctx: Option<Extension<ApiKeyContext>>,
) -> Response {
    let map = state.manager.tag_map();
    let revision = state.manager.revision();
    let runtime = state.controller.status();
    let now_ms = state.manager.clock().now_ms();
    let current = state.manager.current_values();
    let server_store = state.manager.server_store();

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

    let names: Vec<String> = if let Some(Extension(ctx)) = &ctx {
        if query.tags.is_some() {
            // 明示指定でスコープ外を1つでも挙げたら 403(単一と同じ規律)。
            if names.iter().any(|name| !ctx.can_read_value(name)) {
                return forbidden_response();
            }
            names
        } else {
            // 暗黙の全件はスコープ外を黙って除く。
            names
                .into_iter()
                .filter(|name| ctx.can_read_value(name))
                .collect()
        }
    } else {
        names
    };

    let names: Vec<String> = if ctx.is_some() {
        names
            .into_iter()
            .filter(|name| {
                map.get(name)
                    .map(|entry| api_key_external_output_allowed(entry, &runtime))
                    .unwrap_or(false)
            })
            .collect()
    } else {
        names
    };

    let values: Vec<ValueEntry> = names
        .iter()
        .filter_map(|name| map.get(name).map(|entry| (name, entry)))
        .map(|(name, entry)| {
            value_entry(
                name,
                entry,
                current.as_ref(),
                &server_store,
                now_ms,
                &runtime,
            )
        })
        .collect();

    Json(ValuesResponse {
        revision,
        t: now_ms,
        run_id: runtime.run_id,
        collection_mode: runtime.mode.as_str().to_string(),
        values,
    })
    .into_response()
}

/// `GET /api/v1/values/{tag}` - single tag. `404` only when the external
/// name is not in the catalog at all (design: 「404 になるのは定義が存在
/// しない外部名のみ」) - an undefined-but-uncollected tag is `200` with
/// `q: "bad"`.
///
/// H10 ③(Option B、docs/h10-3-read-scope-proposal.md §5 S3): タグは
/// catalog に見えている(=存在は既知)ので、per-tag read スコープ外は 404
/// ではなく**403**(`forbidden_response`)。API キー起因の読み取り
/// (`ctx` あり)だけがこの判定を受ける - セッション token(`ctx` 無し)は
/// 従来どおり全アクセス(管理 UI 不変)。API キー時は simulation /
/// derived_simulation の値を返さず、単一値では `503
/// simulation_output_disabled` とし、catalog からも除外する。
#[utoipa::path(
    get,
    path = "/api/v1/values/{tag}",
    params(("tag" = String, Path, description = "外部名 {connection}.{group}.{tag}")),
    responses(
        (status = 200, description = "単一タグの現在値", body = SingleValueResponse),
        (status = 403, description = "per-tag read スコープ外(API キー、H10 ③)"),
        (status = 503, description = "simulation / derived_simulation は API キーの外部出力対象外"),
        (status = 404, description = "catalog に存在しない外部名"),
    ),
    tag = "tag-space",
)]
async fn v1_value_single(
    State(state): State<TagSpaceState>,
    Path(tag): Path<String>,
    ctx: Option<Extension<ApiKeyContext>>,
) -> Response {
    let map = state.manager.tag_map();
    let Some(entry) = map.get(&tag) else {
        return ApiError(BantoError::NotFound {
            resource: "tags".to_string(),
            id: tag,
        })
        .into_response();
    };
    if let Some(Extension(ctx)) = &ctx {
        if !ctx.can_read_value(&tag) {
            return forbidden_response();
        }
    }
    let now_ms = state.manager.clock().now_ms();
    let runtime = state.controller.status();
    if ctx.is_some() && !api_key_external_output_allowed(entry, &runtime) {
        return simulation_output_disabled_response();
    }
    let current = state.manager.current_values();
    let server_store = state.manager.server_store();
    Json(SingleValueResponse::from_value(
        value_entry(
            &tag,
            entry,
            current.as_ref(),
            &server_store,
            now_ms,
            &runtime,
        ),
        &runtime,
    ))
    .into_response()
}

/// `GET /api/v1/status` の `connections` 配列1件分。
#[derive(Debug, Serialize, ToSchema)]
struct ConnectionStatusEntry {
    name: String,
    id: i64,
    status: String,
    attempt: Option<u32>,
    /// T9-2 (docs/ux-plan.md §1): mirrors `banto_tags::PlcConnection::simulation` -
    /// lets a monitoring client (or the admin UI) flag a connection whose
    /// live values are synthetic, not from a real PLC.
    simulation: bool,
    configured_simulation: bool,
    effective_simulation: bool,
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

/// `GET /api/v1/status` の `last_apply`（T7-2、設計 §4.3実装指示「ApplyReport
/// の内容を...`/api/v1/status` に出す」）: 直近の `apply_config` 呼び出しの
/// 結果 - どの接続が追加/削除/入れ替え/無変更だったか、tstore writer が
/// ローテートしたか。[`banto_collect::ApplyReport`] のワイヤ表現（同クレート
/// 側は `ToSchema`/`Serialize` を持たない内部型のため、ここで DTO 化する -
/// このファイルの他の DTO（[`ConnectionStatusEntry`] 等）と同じ流儀）。
#[derive(Debug, Serialize, ToSchema)]
struct LastApplyEntry {
    added: Vec<String>,
    removed: Vec<String>,
    replaced: Vec<String>,
    unchanged: Vec<String>,
    writer_rotated: bool,
}

impl From<ApplyReport> for LastApplyEntry {
    fn from(report: ApplyReport) -> Self {
        Self {
            added: report.added,
            removed: report.removed,
            replaced: report.replaced,
            unchanged: report.unchanged,
            writer_rotated: report.writer_rotated,
        }
    }
}

/// `GET /api/v1/status` の応答。
#[derive(Debug, Serialize, ToSchema)]
struct StatusResponse {
    version: String,
    revision: u64,
    configured_revision: u64,
    running_revision: u64,
    run_id: Option<u64>,
    collection_state: String,
    collection_mode: String,
    last_runtime_error: Option<String>,
    last_config_error: Option<String>,
    connections: Vec<ConnectionStatusEntry>,
    /// T2-4（設計 §6-6）: 書き込み受付が今いま有効かどうか(ライブフラグ)。
    write_enabled: bool,
    /// T2-4（設計 §6-6）: プロセス再起動前は有効だったか(表示専用の履歴 -
    /// `crate::write_control::WriteControl` のモジュール doc comment
    /// 参照。ライブの `write_enabled` には一切影響しない)。
    write_was_enabled_before_restart: bool,
    /// T15-3（設計 §6.3）: テスト出力（現在の run コンテキスト限定・
    /// 非永続）が今いま有効かどうかと、有効な場合はどの run に紐付いて
    /// いるか。`crate::test_output::TestOutputControl`のモジュール doc
    /// comment参照。
    test_output: TestOutputStatusEntry,
    /// T3（設計 §5.3）: MQTT publish の設定/接続状態。
    mqtt: MqttStatusEntry,
    /// T4（設計 §5.4）: gRPC サーバーの設定。
    grpc: GrpcStatusEntry,
    /// T7-2（設計 §4.3）: 直近の `apply_config` 呼び出しの結果。最後に成功
    /// した rebuild が `apply_config` を経由しなかった場合（起動直後の初回
    /// 成功、または空構成への遷移）は `null`
    /// (`crate::hub::CollectorManager::last_apply` のドキュメント参照)。
    last_apply: Option<LastApplyEntry>,
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
    let runtime = state.controller.status();
    let revision = state.manager.configured_revision();
    let last_config_error = state.manager.last_error();
    let statuses = state.manager.connection_status().await;

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
                simulation: conn.simulation,
                configured_simulation: conn.simulation,
                effective_simulation: effective_simulation_for_connection(
                    &conn.protocol,
                    conn.enabled,
                    conn.simulation,
                    &runtime,
                ),
            }
        })
        .collect();

    Ok(Json(StatusResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
        revision,
        configured_revision: runtime.configured_revision,
        running_revision: runtime.running_revision,
        run_id: runtime.run_id,
        collection_state: runtime.state.as_str().to_string(),
        collection_mode: runtime.mode.as_str().to_string(),
        last_runtime_error: runtime.last_error,
        last_config_error,
        connections: entries,
        write_enabled: state.write_control.is_enabled(),
        write_was_enabled_before_restart: state.write_control.was_enabled_before_restart(),
        test_output: state.test_output.status().into(),
        mqtt: MqttStatusEntry {
            enabled: mqtt_settings.enabled,
            connected: state.mqtt.connected(),
        },
        grpc: GrpcStatusEntry {
            enabled: grpc_settings.enabled,
            port: grpc_settings.port,
        },
        last_apply: state.manager.last_apply().map(LastApplyEntry::from),
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
    /// 書き込む工学値。数値タグには数値、bit タグには真偽値
    /// （2026-08-06〜: 型が data_type と一致しない場合は 422
    /// `unsupported_value_type` - 暗黙の型変換はしない）。
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

/// リクエスト body の `v` を [`crate::write_path::RequestedValue`] に正規化
/// する。`bool` は [`RequestedValue::Bool`]、数値は [`RequestedValue::Num`]。
/// どちらの型が来たかは gate 7（`crate::write_path::execute_write`）が
/// data_type との対称性検査に使うので、ここで `f64` へ潰さない（2026-08-06
/// 変更: 従来は `bool` を `1.0`/`0.0` に潰して数値と区別せずに渡していた）。
/// 文字列・配列・オブジェクト・`null` は `None`（呼び出し元が 422
/// `unsupported_value_type` を返す）。REST 固有の wire 形式（JSON の `v`）
/// からの変換であり、gRPC 側は `oneof num|bool` を直接分解するだけで
/// 済むため、この関数は `crate::write_path` へは移していない。
fn parse_requested_value(v: &serde_json::Value) -> Option<crate::write_path::RequestedValue> {
    use crate::write_path::RequestedValue;
    if let Some(b) = v.as_bool() {
        Some(RequestedValue::Bool(b))
    } else {
        v.as_f64().map(RequestedValue::Num)
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
        WriteRejection::CollectionNotRunning(_) | WriteRejection::SimulationWriteRejected => {
            StatusCode::SERVICE_UNAVAILABLE
        }
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
    collection_controller: Option<Arc<CollectionController>>,
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
        (status = 503, description = "writes_disabled / simulation_write_rejected"),
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
        collection_controller: state.collection_controller.as_deref(),
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
        CatalogTagEntry,
        ValueEntry,
        SingleValueResponse,
        ValuesResponse,
        ConnectionStatusEntry,
        MqttStatusEntry,
        GrpcStatusEntry,
        StatusResponse,
        TestOutputStatusEntry,
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
///
/// T16-2 第三スライス（docs/banto-hub-t16-design.md §5 既知の gap）: 応答
/// している Hub インスタンス自身の profile-id を`info.x-banto-hub-profile-id`
/// 拡張フィールドとして埋め込む。`crate::http_hub_health::HttpHubHealthProbe`
/// が「lock ファイルの置き場所」だけでなく「ワイヤ応答そのもの」から
/// profile-id を確認できるようにするための唯一の情報源 - utoipa の
/// `#[openapi(info(...))]` は任意拡張フィールドを直接生成できないため、
/// `ApiDoc::openapi()`が生成した`serde_json::Value`へ後から差し込む
/// （実装指示どおり、utoipa の`Extensions` API とは戦わない最小実装）。
async fn openapi_json(State(state): State<OpenApiState>) -> Json<serde_json::Value> {
    let mut value = serde_json::to_value(ApiDoc::openapi()).expect("openapi serialize");
    value["info"]["x-banto-hub-profile-id"] = serde_json::json!(state.profile_id);
    Json(value)
}

/// [`openapi_json`]専用の状態 - この Hub インスタンスが実際に使っている
/// profile-id（`crate::profile_paths`参照）だけを持つ。`HubRuntime::start`
/// （`crate::runtime`）が`HubConfig::profile_id`をそのまま渡す。
#[derive(Clone)]
struct OpenApiState {
    profile_id: String,
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
/// - `Authorization` ヘッダがない、または `Bearer ` で始まらない → `GET
///   /api/v1/stream` だけは [`extract_ws_protocol_token`] で
///   `Sec-WebSocket-Protocol` からのフォールバックを試す（ブラウザの
///   WebSocket が `Authorization` を送れないための救済 - 同関数の doc
///   comment 参照）。それも失敗すれば 401
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
///   - [`ApiKeyLookup::Expired`]（H10 ①、docs/improvement-plan.md・
///     2026-08-08 オーナー決定）→ 401 + audit_log に同様の `denied` 記録
///     （`reason: "expired"` - `Revoked` の腕をそのまま踏襲。期限切れは
///     「失効」ではないが、未認証というレスポンス上の扱いは revoked と同じ
///     401 が適切 - `crate::api_keys` のモジュール doc comment「有効期限」
///     参照）
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
    let Some(token) = bearer_token(req.headers())
        .map(str::to_string)
        .or_else(|| extract_ws_protocol_token(req.uri().path(), req.headers()))
    else {
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
        // H10 ①: 期限切れ判定([`crate::api_keys::ApiKeysService::lookup`])
        // にも last_used_at 更新にも同じ「今」を使う - 呼び出しごとに
        // ずれないよう一度だけ取得する。
        let now_ms = state.manager.clock().now_ms();
        match state.api_keys.lookup(&token, now_ms).await {
            Ok(ApiKeyLookup::Valid(ctx)) => {
                // H10 ③(Option B): この認証層のゲートは「read 系ルートに
                // 入れるか」だけを見る(has_any_read = 素の read か任意の
                // read:... を1つでも持つか)。個々のタグの値を読めるかどうか
                // (can_read_value)は catalog を絞らず、値ハンドラ側
                // (v1_value_single/v1_values/crate::stream)が個別に判定する
                // - `crate::api_keys::ApiKeyContext` の doc comment「read の
                // タグ単位化」参照。
                if !is_write_route && !ctx.has_any_read() {
                    return forbidden_response();
                }
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
            Ok(ApiKeyLookup::Expired { id, name }) => {
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
                        detail: Some(json!({ "reason": "expired", "name": name, "method": method, "path": path })),
                        origin: "rest",
                        result: "denied",
                    })
                    .await;
                unauthorized_response()
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
    controller: Arc<CollectionController>,
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
    enforce_collection_state: bool,
    // T15-3（設計 §6.3）: `GET /api/v1/status` の `test_output` のため -
    // `controller`が保持するものと**同じ** `Arc`（呼び出し元の責務、
    // `test_output_router`のそれと同じ規律）。
    test_output: Arc<TestOutputControl>,
) -> Router {
    let state = TagSpaceState {
        manager: manager.clone(),
        controller: controller.clone(),
        write_control: write_control.clone(),
        test_output,
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
        collection_controller: enforce_collection_state.then_some(controller.clone()),
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

/// `GET /api/v1/openapi.json` と `GET /api/v1/swagger-ui/*` 専用ルーター -
/// 認証層を一切通さない（`openapi_json`関数の doc comment 参照）。
/// `profile_id`はこの Hub インスタンスが実際に使っている profile-id
/// （呼び出し元 `api_router_with_controller_mode`が
/// `HubConfig::profile_id`をそのまま渡す）。
///
/// ux-plan.md §5 バックログ「OpenAPI の Swagger UI 同梱」（2026-08-12
/// オーナー決定）: `/api/v1/swagger-ui` に Swagger UI 本体（HTML/JS/CSS）を
/// 同梱・マウントする。`openapi.json` 同様**認証不要** - Swagger UI 自体は
/// 静的アセット＋上の `openapi_json` が返すスキーマの閲覧・試打 UI に
/// すぎず、秘匿情報を含まないため（`openapi_json` の doc comment と同じ
/// 判断）。`SwaggerUi::url(...)`は使わない - それだと utoipa-swagger-ui が
/// **別の** `ApiDoc::openapi()`静的スナップショットを新規ルートとして
/// 生成してしまい、上の`openapi_json`（`x-banto-hub-profile-id`埋め込み
/// 済み・状態を持つ）と二重管理になる。代わりに`Config::new([...])`で
/// Swagger UI のフロントエンド JS に「スキーマは`/api/v1/openapi.json`を
/// 見に行け」と教えるだけにし、ルートは1本のまま
/// （`utoipa_swagger_ui::Config::new`は fetch 先 URL の設定のみで、それ自体は
/// ルートを追加しない）。アセットは`vendored` feature
/// （`utoipa-swagger-ui-vendored`、workspace Cargo.toml 参照）でバイナリに
/// 同梱済み - ビルド時・実行時ともに外部ネットワーク（CDN 含む）へ
/// 一切アクセスしない。
fn openapi_router(profile_id: String) -> Router {
    Router::new()
        .route("/api/v1/openapi.json", get(openapi_json))
        .merge(SwaggerUi::new("/api/v1/swagger-ui").config(Config::new(["/api/v1/openapi.json"])))
        .with_state(OpenApiState { profile_id })
}

// --- composition ------------------------------------------------------------

/// Compose the full router: the admin surface (auth/users/audit-log/I1 CRUD/
/// api-keys/SSE, all behind CSRF + bearer auth) merged with the tag-space API
/// (API キー + セッション bearer 併用、CSRF なし - see this module's doc
/// comment) and the unauthenticated `/api/v1/openapi.json`.
#[allow(clippy::too_many_arguments)]
fn api_router_with_controller_mode(
    users: UsersService,
    audit: AuditLogService,
    plc_connections: PlcConnectionService,
    collection_groups: CollectionGroupService,
    tags: TagService,
    pending_changes: PendingChangesService,
    api_keys: ApiKeysService,
    manager: Arc<CollectorManager>,
    controller: Arc<CollectionController>,
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
    legacy_live_reconfigure: bool,
    // T15-3（設計 §6.3）: テスト出力の非永続フラグ - `controller`が保持
    // するものと**同じ** `Arc` を渡すこと（呼び出し元の責務、
    // `write_control`/`mqtt`/`grpc_server`と同じ規約）。ここで新規に
    // 構築すると `CollectionController`・`MqttPublisher`・
    // `GrpcService`・この REST admin エンドポイントの4者が別々の
    // フラグを見てしまう。
    test_output: Arc<TestOutputControl>,
    // T16-2 第三スライス（docs/banto-hub-t16-design.md §5）:
    // `GET /api/v1/openapi.json`の`info.x-banto-hub-profile-id`に埋め込む
    // この Hub インスタンス自身の profile-id - `HubRuntime::start`
    // （`crate::runtime`）が`HubConfig::profile_id`をそのまま渡す。
    profile_id: String,
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
        .merge(audit_log_router(
            audit.clone(),
            auth.clone(),
            manager.clone(),
        ))
        .merge(api_keys_router(
            api_keys.clone(),
            audit.clone(),
            auth.clone(),
            manager.clone(),
        ))
        .merge(tag_registry_router(
            plc_connections.clone(),
            collection_groups.clone(),
            tags.clone(),
            pending_changes.clone(),
            audit.clone(),
            auth.clone(),
            manager.clone(),
            controller.clone(),
            events.clone(),
            legacy_live_reconfigure,
        ))
        .merge(pending_changes_router(
            pending_changes,
            plc_connections,
            collection_groups,
            tags,
            audit.clone(),
            auth.clone(),
            manager.clone(),
            controller.clone(),
            events.clone(),
            legacy_live_reconfigure,
        ))
        .merge(write_control_router(
            write_control.clone(),
            manager.clone(),
            audit.clone(),
            auth.clone(),
            events.clone(),
        ))
        .merge(test_output_router(
            test_output.clone(),
            controller.clone(),
            audit.clone(),
            auth.clone(),
            events.clone(),
        ))
        .merge(collection_control_router(
            controller.clone(),
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
            controller,
            auth,
            api_keys,
            audit,
            write_control,
            write_audit,
            events,
            mqtt,
            rate_limiter,
            !legacy_live_reconfigure,
            test_output,
        ))
        .merge(openapi_router(profile_id))
}

/// Run the collector/computed preflight against a registry snapshot read from
/// the same SQLite transaction that contains the proposed mutation. The tags
/// services remain the authority for SQL FK/UNIQUE/shape validation; this
/// helper covers the cross-table collector rules (address/data type and
/// computed dependency validation).
fn preflight_snapshot(snapshot: &RegistrySnapshot) -> Result<(), ApiError> {
    let map = crate::hub::build_catalog_from(snapshot)
        .map_err(|err| preflight_api_error(format!("catalog の検証に失敗しました: {err}")))?;
    crate::computed::build_plan(&map)
        .map_err(|err| preflight_api_error(format!("演算タグの検証に失敗しました: {err}")))?;
    build_config_from(snapshot)
        .map(|_| ())
        .map_err(|err| preflight_api_error(err.to_string()))
}

fn preflight_api_error(message: String) -> ApiError {
    ApiError(BantoError::Validation {
        field_errors: vec![FieldError {
            field: "configuration".to_string(),
            message,
        }],
    })
}

fn storage_api_error(error: sqlx::Error) -> ApiError {
    ApiError(BantoError::Storage(error.to_string()))
}

async fn preflight_transaction(
    connection: &mut sqlx::SqliteConnection,
) -> Result<RegistrySnapshot, ApiError> {
    let snapshot = RegistrySnapshot::load_connection(connection)
        .await
        .map_err(|err| preflight_api_error(format!("レジストリの読み取りに失敗しました: {err}")))?;
    preflight_snapshot(&snapshot)?;
    Ok(snapshot)
}

/// Production composition entry point. Registry writes update the configured
/// catalog but do not apply it to the running collection.
#[allow(clippy::too_many_arguments)]
pub fn api_router_with_controller(
    users: UsersService,
    audit: AuditLogService,
    plc_connections: PlcConnectionService,
    collection_groups: CollectionGroupService,
    tags: TagService,
    api_keys: ApiKeysService,
    manager: Arc<CollectorManager>,
    controller: Arc<CollectionController>,
    auth: AuthState,
    events: broadcast::Sender<ServerEvent>,
    allow_setup: bool,
    write_control: Arc<WriteControl>,
    write_audit: WriteAuditService,
    mqtt: Arc<MqttPublisher>,
    grpc_server: Arc<crate::grpc::GrpcServer>,
    rate_limiter: Arc<AsyncMutex<WriteRateLimiter>>,
    test_output: Arc<TestOutputControl>,
    // T16-2 第三スライス: `api_router_with_controller_mode`のフィールド doc
    // comment 参照。
    profile_id: String,
) -> Router {
    let pending_changes = PendingChangesService::new(manager.pool());
    api_router_with_controller_mode(
        users,
        audit,
        plc_connections,
        collection_groups,
        tags,
        pending_changes,
        api_keys,
        manager,
        controller,
        auth,
        events,
        allow_setup,
        write_control,
        write_audit,
        mqtt,
        grpc_server,
        rate_limiter,
        false,
        test_output,
        profile_id,
    )
}

/// Compatibility composition entry point for existing embedders/tests. New
/// hosts should use [`api_router_with_controller`] so MQTT and status observe
/// the process-wide lifecycle controller.
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
    write_control: Arc<WriteControl>,
    write_audit: WriteAuditService,
    mqtt: Arc<MqttPublisher>,
    grpc_server: Arc<crate::grpc::GrpcServer>,
    rate_limiter: Arc<AsyncMutex<WriteRateLimiter>>,
    // T16-2 第三スライス: `api_router_with_controller_mode`のフィールド doc
    // comment 参照。
    profile_id: String,
) -> Router {
    let pending_changes = PendingChangesService::new(manager.pool());
    let test_output = Arc::new(TestOutputControl::new());
    let controller = Arc::new(crate::controller::CollectionController::new(
        manager.clone(),
        write_control.clone(),
        test_output.clone(),
    ));
    api_router_with_controller_mode(
        users,
        audit,
        plc_connections,
        collection_groups,
        tags,
        pending_changes,
        api_keys,
        manager,
        controller,
        auth,
        events,
        allow_setup,
        write_control,
        write_audit,
        mqtt,
        grpc_server,
        rate_limiter,
        true,
        test_output,
        profile_id,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_keys::ApiKeysService;
    use crate::db::migrate_memory;
    use crate::hub::CollectorManager;
    use crate::pending_changes::PendingChangeState;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use banto_collect::CollectorOptions;
    use banto_tstore::{Clock, ManualClock, SystemClock};
    use tokio::sync::broadcast as tokio_broadcast;
    use tower::ServiceExt;

    const CLIENT_HEADER: (&str, &str) = ("X-Banto-Client", "banto");

    /// A `CollectorManager` for router-composition tests that never actually
    /// rebuild (I1 CRUD tests below only cover auth/RBAC/plumbing, not the
    /// collector lifecycle itself - that is `hub.rs`'s and the integration
    /// test's job). Points at a real temp dir since `CollectorManager` always
    /// needs a `data_dir`, even if `rebuild` is never called in a given test.
    ///
    /// `clock` is injectable (H10 ①): every pre-existing caller goes through
    /// [`test_env`], which passes `Arc::new(SystemClock)` - same behavior as
    /// before this parameter existed. The expiry E2E tests below go through
    /// [`test_env_with_clock`] instead, passing a [`ManualClock`] so they can
    /// advance past a key's `expiresAt` deterministically instead of
    /// depending on real wall-clock time.
    fn test_manager_with_clock(
        pool: sqlx::SqlitePool,
        clock: Arc<dyn Clock>,
    ) -> (Arc<CollectorManager>, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let sessions = Arc::new(crate::broker_glue::HubSessions::new(
            banto_broker::BackoffConfig::default(),
        ));
        let sim_registry = Arc::new(crate::broker_glue::SlmpSimRegistry::new());
        let computed = Arc::new(crate::computed::ComputedEngine::new(Arc::new(
            crate::computed::ServerTagStore::new(),
        )));
        let manager = CollectorManager::new(
            pool,
            dir.path().join("data"),
            clock,
            CollectorOptions::default(),
            sessions,
            sim_registry,
            computed,
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
        test_env_with_clock(Arc::new(SystemClock)).await
    }

    /// [`test_env`] but with an injectable clock (H10 ①) - lets a test
    /// create a key with `expiresAt = clock.now_ms() + small`, assert it
    /// authenticates, then `advance_ms` past the deadline and assert 401 -
    /// deterministically, without depending on real wall-clock time. See
    /// [`test_manager_with_clock`]'s doc comment for the same reasoning.
    async fn test_env_with_clock(clock: Arc<dyn Clock>) -> TestEnv {
        let pool = migrate_memory().await.expect("migrate_memory");
        let (tx, _rx) = tokio_broadcast::channel(16);
        let users = UsersService::new(pool.clone());
        let audit = AuditLogService::new(pool.clone());
        let plc_connections = PlcConnectionService::new(pool.clone());
        let collection_groups = CollectionGroupService::new(pool.clone());
        let tags = TagService::new(pool.clone());
        let api_keys = ApiKeysService::new(pool.clone());
        let (manager, dir) = test_manager_with_clock(pool.clone(), clock);

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
            crate::profile_paths::DEFAULT_PROFILE_ID.to_string(),
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

    /// [`issue_api_key`] but also sets `expiresAt` (H10 ①) - kept as a
    /// separate helper rather than adding a parameter to [`issue_api_key`]
    /// so the ~10 existing call sites above (all unlimited keys) stay
    /// untouched.
    async fn issue_api_key_with_expiry(
        router: &Router,
        token: &str,
        name: &str,
        scopes: &[&str],
        expires_at: Option<i64>,
    ) -> (StatusCode, serde_json::Value) {
        let response = router
            .clone()
            .oneshot(
                HttpRequest::post("/api/api-keys")
                    .header("Authorization", format!("Bearer {token}"))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "name": name,
                            "scopes": scopes,
                            "expiresAt": expires_at,
                        })
                        .to_string(),
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

    // --- H10 ①: 任意の有効期限 ----------------------------------------------

    /// 無期限キー（`expiresAt` 省略）は今までどおり認証できる - 期限切れ
    /// 判定の追加が既定動作を変えていないことの回帰防止（実装指示の受け入れ
    /// 条件: 「無期限キーの従来動作不変」）。`ManualClock` を大きく進めても
    /// 無期限キーは影響を受けないことも合わせて確認する。
    #[tokio::test]
    async fn unlimited_api_key_still_authenticates_regardless_of_the_clock() {
        let clock = Arc::new(ManualClock::new(1_000_000, 0));
        let env = test_env_with_clock(clock.clone()).await;
        let (status, issued) =
            issue_api_key(&env.router, &env.admin_token, "unlimited", &["read"]).await;
        assert_eq!(status, StatusCode::CREATED, "{issued:?}");
        let key = issued["key"].as_str().unwrap().to_string();

        clock.advance_ms(999_999_999_999);

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

    /// 期限付きキー: 期限内は 200、`advance_ms` で期限を過ぎさせると 401 に
    /// なり audit_log に `denied`/`api_keys`/`{"reason":"expired"}` が記録
    /// される（実装指示の受け入れ条件: 「期限切れキーの 401 と UI 警告の
    /// テスト」の REST 側、`revoked_api_key_is_401_and_audited` と同型）。
    #[tokio::test]
    async fn expired_api_key_is_401_and_audited() {
        let clock = Arc::new(ManualClock::new(1_000_000, 0));
        let env = test_env_with_clock(clock.clone()).await;

        let expires_at = clock.now_ms() + 60_000;
        let (status, issued) = issue_api_key_with_expiry(
            &env.router,
            &env.admin_token,
            "expiring",
            &["read"],
            Some(expires_at),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{issued:?}");
        // `IssuedApiKeyResponse`（発行応答）は `id`/`name`/`prefix`/`scopes`/
        // `key` のみで `expiresAt` は含まない（発行時点で入力どおりの値を
        // そのまま返すだけの情報であり、既存の `created_at` 等と同じく
        // 「一覧を見ればわかる」ため - このモジュールの
        // `IssuedApiKeyResponse` doc comment参照）。代わりに一覧
        // （`GET /api/api-keys`）に camelCase の `expiresAt` として正しく
        // 反映されることをここで確認する。
        let key = issued["key"].as_str().unwrap().to_string();
        let id = issued["id"].as_i64().unwrap();

        let list_response = env
            .router
            .clone()
            .oneshot(
                HttpRequest::get("/api/api-keys")
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(list_response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(list_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let listed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let entry = listed
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"].as_i64() == Some(id))
            .expect("issued key should appear in the list");
        assert_eq!(entry["expiresAt"].as_i64(), Some(expires_at));

        // 期限前: 通常どおり読める。
        let response = env
            .router
            .clone()
            .oneshot(
                HttpRequest::get("/api/v1/tags")
                    .header("Authorization", format!("Bearer {key}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // 期限を過ぎさせる。
        clock.advance_ms(60_001);

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
            .find(|row| {
                row.action == "denied"
                    && row.resource == "api_keys"
                    && row
                        .detail
                        .as_deref()
                        .is_some_and(|detail| detail.contains("\"expired\""))
            })
            .expect("a denied/api_keys audit row with reason=expired should exist");
        assert_eq!(denied.entity_id.as_deref(), Some(id.to_string().as_str()));
    }

    /// 発行時点で既に過去/現在時刻以下の `expiresAt` は 422（`Validation`）で
    /// 拒否される（実装指示: 「Some(e) and e <= now_ms、reject...」）。
    #[tokio::test]
    async fn creating_an_api_key_with_a_past_expiry_is_rejected() {
        let clock = Arc::new(ManualClock::new(1_000_000, 0));
        let env = test_env_with_clock(clock.clone()).await;

        let (status, body) = issue_api_key_with_expiry(
            &env.router,
            &env.admin_token,
            "already-expired",
            &["read"],
            Some(clock.now_ms()), // e <= now_ms(ちょうど今) は拒否
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
        assert_eq!(body["kind"], "validation");
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
        // T16-2 第三スライス（docs/banto-hub-t16-design.md §5）:
        // `crate::http_hub_health::HttpHubHealthProbe`がワイヤ上で profile-id
        // を確認できるようにするための拡張フィールド。
        assert_eq!(
            json["info"]["x-banto-hub-profile-id"],
            serde_json::json!(crate::profile_paths::DEFAULT_PROFILE_ID)
        );
    }

    /// `GET /api/v1/swagger-ui/`（末尾スラッシュあり）は認証不要で 200 の
    /// Swagger UI HTML を返し、それが読み込む `swagger-initializer.js`
    /// （同じく認証不要）が `/api/v1/openapi.json` を指すよう設定されている
    /// （`openapi_router` の doc comment、ux-plan.md §5「OpenAPI の
    /// Swagger UI 同梱」）。`Config`の urls はテンプレート
    /// `swagger-initializer.js`側に埋め込まれる実装（`index.html`自体には
    /// 現れない - utoipa-swagger-ui の`format_config`）ため、2ファイルに
    /// 分けて確認する。末尾スラッシュ無しは 301/308 でスラッシュ有りへ
    /// redirect される（utoipa-swagger-ui の axum 統合の既定挙動）。
    #[tokio::test]
    async fn swagger_ui_is_public_and_references_openapi_json() {
        let env = test_env().await;

        let redirect = env
            .router
            .clone()
            .oneshot(
                HttpRequest::get("/api/v1/swagger-ui")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            redirect.status().is_redirection(),
            "expected redirect, got {}",
            redirect.status()
        );

        let index_response = env
            .router
            .clone()
            .oneshot(
                HttpRequest::get("/api/v1/swagger-ui/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(index_response.status(), StatusCode::OK);
        let index_bytes = axum::body::to_bytes(index_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let index_html = String::from_utf8(index_bytes.to_vec()).unwrap();
        assert!(
            index_html.contains("swagger-initializer.js"),
            "swagger-ui index.html should load swagger-initializer.js"
        );

        let init_response = env
            .router
            .oneshot(
                HttpRequest::get("/api/v1/swagger-ui/swagger-initializer.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(init_response.status(), StatusCode::OK);
        let init_bytes = axum::body::to_bytes(init_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let init_js = String::from_utf8(init_bytes.to_vec()).unwrap();
        assert!(
            init_js.contains("/api/v1/openapi.json"),
            "swagger-initializer.js should be configured to fetch /api/v1/openapi.json"
        );
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

    // --- H10 ③: per-tag read スコープ(Option B、
    // docs/h10-3-read-scope-proposal.md §5・§6) ------------------------------

    /// admin 系ルーター(CSRF ヘッダ必須)への `POST` - `issue_api_key` と
    /// 同型の汎用ヘルパ(H10 ③ のタグ seed フィクスチャで複数リソースを
    /// 作るため、`tests/grpc.rs`/`tests/computed.rs` の `admin_post` と
    /// 同じ形をここにも用意する)。
    async fn admin_post(
        router: &Router,
        path: &str,
        token: &str,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let response = router
            .clone()
            .oneshot(
                HttpRequest::post(path)
                    .header("Authorization", format!("Bearer {token}"))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
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

    /// 認証ヘッダ付きで `/api/v1/*` に `GET` する小さなヘルパ。`/api/v1/*`
    /// は CSRF 対象外なので `X-Banto-Client` は付けない
    /// （`v1_tags_requires_auth_but_not_the_csrf_header` 参照）。
    async fn v1_get(router: &Router, key: &str, path: &str) -> (StatusCode, serde_json::Value) {
        let response = router
            .clone()
            .oneshot(
                HttpRequest::get(path)
                    .header("Authorization", format!("Bearer {key}"))
                    .body(Body::empty())
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

    /// `line1.fast.temp01`/`line1.fast.temp02`/`line2.slow.press01` の3タグを
    /// 別接続・別グループで作る(admin REST 経由 - I1 の書き込みハンドラは
    /// 成功のたびに `CollectorManager::rebuild` を自動で呼ぶので、明示的な
    /// rebuild 呼び出しは不要、このモジュールの doc comment「I1 CRUD 書き
    /// 込み後の再構築」参照)。catalog は registry の読み取りだけで完結する
    /// (`crate::hub::build_catalog`)ため、実際に PLC へ繋がらないポートでも
    /// rebuild は成功しカタログへ反映される -
    /// `tags_create_via_admin_router_rebuilds_the_catalog` と同じ判断。
    async fn seed_scope_fixture(router: &Router, admin_token: &str) {
        let (status, conn1) = admin_post(
            router,
            "/api/plc-connections",
            admin_token,
            json!({ "name": "line1", "host": "127.0.0.1", "port": 15101 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{conn1:?}");
        let (status, group1) = admin_post(
            router,
            "/api/collection-groups",
            admin_token,
            json!({ "name": "fast", "plcConnectionId": conn1["id"], "periodMs": 100 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{group1:?}");
        for (name, address) in [("temp01", "40001"), ("temp02", "40003")] {
            let (status, tag) = admin_post(
                router,
                "/api/tags",
                admin_token,
                json!({
                    "name": name,
                    "collectionGroupId": group1["id"],
                    "address": address,
                    "dataType": "i16",
                }),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{tag:?}");
        }

        let (status, conn2) = admin_post(
            router,
            "/api/plc-connections",
            admin_token,
            json!({ "name": "line2", "host": "127.0.0.1", "port": 15102 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{conn2:?}");
        let (status, group2) = admin_post(
            router,
            "/api/collection-groups",
            admin_token,
            json!({ "name": "slow", "plcConnectionId": conn2["id"], "periodMs": 1000 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{group2:?}");
        let (status, tag) = admin_post(
            router,
            "/api/tags",
            admin_token,
            json!({
                "name": "press01",
                "collectionGroupId": group2["id"],
                "address": "40001",
                "dataType": "i16",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{tag:?}");
    }

    /// S1/S3/S4(案 B): `read:line1.fast.temp01` キーは catalog は全タグ
    /// (line2 のタグ含む)を見られるが、値は自分のタグしか読めない - 単一は
    /// 403、バルクは黙って除外、`?tags=` 明示指定は 403。
    #[tokio::test]
    async fn exact_read_scope_key_sees_full_catalog_but_only_its_own_tag_value() {
        let env = test_env().await;
        seed_scope_fixture(&env.router, &env.admin_token).await;
        let (status, issued) = issue_api_key(
            &env.router,
            &env.admin_token,
            "line1-temp01-reader",
            &["read:line1.fast.temp01"],
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{issued:?}");
        let key = issued["key"].as_str().unwrap();

        // catalog は絞らない - line2 のタグも見える(案 B の核)。
        let (status, catalog) = v1_get(&env.router, key, "/api/v1/tags").await;
        assert_eq!(status, StatusCode::OK, "{catalog:?}");
        let names: Vec<&str> = catalog["tags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["external_name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"line1.fast.temp01"));
        assert!(names.contains(&"line1.fast.temp02"));
        assert!(
            names.contains(&"line2.slow.press01"),
            "catalog must stay unfiltered (Option B): {names:?}"
        );

        // 単一: 自分のタグは 200、他人のタグは 403(404 ではない - catalog
        // に見えている=存在は既知のため)。
        let (status, _) = v1_get(&env.router, key, "/api/v1/values/line1.fast.temp01").await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = v1_get(&env.router, key, "/api/v1/values/line2.slow.press01").await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        // バルク(?tags= 省略): スコープ外は黙って除かれる。
        let (status, bulk) = v1_get(&env.router, key, "/api/v1/values").await;
        assert_eq!(status, StatusCode::OK, "{bulk:?}");
        let bulk_tags: Vec<&str> = bulk["values"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["tag"].as_str().unwrap())
            .collect();
        assert_eq!(bulk_tags, vec!["line1.fast.temp01"]);

        // バルク(?tags= 明示、スコープ外を含む): 403(単一と同じ規律)。
        let (status, _) = v1_get(&env.router, key, "/api/v1/values?tags=line2.slow.press01").await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    /// S1(グループ・ワイルドカード): `read:line1.fast.*` キーはそのグループ
    /// の全タグ(temp01・temp02)を読めるが、別グループ(line2.slow)は読め
    /// ない。
    #[tokio::test]
    async fn group_wildcard_read_scope_key_reads_every_tag_in_its_group_but_not_others() {
        let env = test_env().await;
        seed_scope_fixture(&env.router, &env.admin_token).await;
        let (status, issued) = issue_api_key(
            &env.router,
            &env.admin_token,
            "line1-fast-reader",
            &["read:line1.fast.*"],
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{issued:?}");
        let key = issued["key"].as_str().unwrap();

        for tag in ["line1.fast.temp01", "line1.fast.temp02"] {
            let (status, body) = v1_get(&env.router, key, &format!("/api/v1/values/{tag}")).await;
            assert_eq!(status, StatusCode::OK, "{tag}: {body:?}");
        }
        let (status, _) = v1_get(&env.router, key, "/api/v1/values/line2.slow.press01").await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        let (status, bulk) = v1_get(&env.router, key, "/api/v1/values").await;
        assert_eq!(status, StatusCode::OK, "{bulk:?}");
        let mut bulk_tags: Vec<&str> = bulk["values"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["tag"].as_str().unwrap())
            .collect();
        bulk_tags.sort_unstable();
        assert_eq!(bulk_tags, vec!["line1.fast.temp01", "line1.fast.temp02"]);
    }

    /// S2(後方互換): 素の `read` キーは従来どおり catalog も値(単一・
    /// バルク・明示 `?tags=`)も全件読める - per-tag スコープ導入で既定動作
    /// が変わっていないことの回帰防止。
    #[tokio::test]
    async fn bare_read_scope_key_still_reads_every_tag_value() {
        let env = test_env().await;
        seed_scope_fixture(&env.router, &env.admin_token).await;
        let (status, issued) =
            issue_api_key(&env.router, &env.admin_token, "bare-reader", &["read"]).await;
        assert_eq!(status, StatusCode::CREATED, "{issued:?}");
        let key = issued["key"].as_str().unwrap();

        let (status, _) = v1_get(&env.router, key, "/api/v1/values/line2.slow.press01").await;
        assert_eq!(status, StatusCode::OK);

        let (status, bulk) = v1_get(&env.router, key, "/api/v1/values").await;
        assert_eq!(status, StatusCode::OK, "{bulk:?}");
        let mut bulk_tags: Vec<&str> = bulk["values"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["tag"].as_str().unwrap())
            .collect();
        bulk_tags.sort_unstable();
        assert_eq!(
            bulk_tags,
            vec![
                "line1.fast.temp01",
                "line1.fast.temp02",
                "line2.slow.press01"
            ]
        );

        // 明示 ?tags= でも全件通る(スコープ外という概念が無い)。
        let (status, _) = v1_get(&env.router, key, "/api/v1/values?tags=line2.slow.press01").await;
        assert_eq!(status, StatusCode::OK);
    }

    fn metadata_test_tag(tag_kind: &str, simulation: bool, enabled: bool) -> TagEntry {
        TagEntry {
            external_name: "line1.fast.temp".to_string(),
            tag_key: "tag:3".to_string(),
            ids: (1, 2, 3),
            connection: "line1".to_string(),
            group: "fast".to_string(),
            name: "temp".to_string(),
            address: "40001".to_string(),
            data_type: "f32".to_string(),
            unit: Some("C".to_string()),
            decimals: 1,
            period_ms: 100,
            enabled,
            writable: false,
            tag_kind: tag_kind.to_string(),
            expression: (tag_kind == banto_tags::COMPUTED_TAG_KIND)
                .then(|| "line1.fast.temp + 1".to_string()),
            retain: false,
            simulation,
        }
    }

    fn metadata_test_status(state: CollectionState, mode: RunMode) -> CollectionStatus {
        CollectionStatus {
            state,
            mode,
            run_id: (state == CollectionState::Running).then_some(9),
            last_error: None,
            configured_revision: 1,
            running_revision: 1,
        }
    }

    #[test]
    fn rest_catalog_dto_separates_configured_and_effective_simulation() {
        let tag = metadata_test_tag(banto_tags::PLC_TAG_KIND, false, true);
        let all_simulation = metadata_test_status(CollectionState::Running, RunMode::AllSimulation);
        let configured = metadata_test_status(CollectionState::Running, RunMode::Configured);
        let stopped = metadata_test_status(CollectionState::Stopped, RunMode::AllSimulation);

        let all_simulation_json =
            serde_json::to_value(CatalogTagEntry::from_runtime(&tag, &all_simulation))
                .expect("catalog DTO serializes");
        assert_eq!(all_simulation_json["simulation"], false);
        assert_eq!(all_simulation_json["configured_simulation"], false);
        assert_eq!(all_simulation_json["effective_simulation"], true);
        assert_eq!(all_simulation_json["value_source"], "simulation");

        let all_simulation_catalog = serde_json::to_value(CatalogResponse {
            revision: 4,
            run_id: all_simulation.run_id,
            collection_mode: all_simulation.mode.as_str().to_string(),
            tags: vec![CatalogTagEntry::from_runtime(&tag, &all_simulation)],
        })
        .expect("catalog response serializes");
        assert_eq!(all_simulation_catalog["run_id"], 9);
        assert_eq!(all_simulation_catalog["collection_mode"], "all_simulation");
        assert_eq!(
            all_simulation_catalog["tags"][0]["value_source"],
            "simulation"
        );

        let configured_catalog = serde_json::to_value(CatalogResponse {
            revision: 4,
            run_id: configured.run_id,
            collection_mode: configured.mode.as_str().to_string(),
            tags: vec![CatalogTagEntry::from_runtime(&tag, &configured)],
        })
        .expect("configured catalog response serializes");
        assert_eq!(configured_catalog["run_id"], 9);
        assert_eq!(configured_catalog["collection_mode"], "configured");
        assert_eq!(configured_catalog["tags"][0]["value_source"], "real");

        assert!(!CatalogTagEntry::from_runtime(&tag, &configured).effective_simulation);
        assert!(!CatalogTagEntry::from_runtime(&tag, &stopped).effective_simulation);
        assert_eq!(
            CatalogTagEntry::from_runtime(&tag, &configured).value_source,
            "real"
        );
        assert_eq!(
            CatalogTagEntry::from_runtime(&tag, &stopped).value_source,
            "real"
        );
        let stopped_catalog = serde_json::to_value(CatalogResponse {
            revision: 4,
            run_id: stopped.run_id,
            collection_mode: stopped.mode.as_str().to_string(),
            tags: vec![CatalogTagEntry::from_runtime(&tag, &stopped)],
        })
        .expect("stopped catalog response serializes");
        assert!(stopped_catalog["run_id"].is_null());
        assert_eq!(stopped_catalog["collection_mode"], "all_simulation");
        assert!(
            !tag.simulation,
            "the shared saved catalog must stay unchanged"
        );

        let saved_simulation = metadata_test_tag(banto_tags::PLC_TAG_KIND, true, true);
        assert!(effective_simulation_for_connection(
            "modbus-tcp",
            true,
            true,
            &configured,
        ));
        assert!(!effective_simulation_for_connection(
            "modbus-tcp",
            true,
            true,
            &stopped,
        ));
        assert!(!CatalogTagEntry::from_runtime(&saved_simulation, &stopped).effective_simulation);
    }

    #[test]
    fn rest_value_source_uses_safe_tag_kind_classification() {
        let all_simulation = metadata_test_status(CollectionState::Running, RunMode::AllSimulation);
        let configured = metadata_test_status(CollectionState::Running, RunMode::Configured);
        let plc = metadata_test_tag(banto_tags::PLC_TAG_KIND, false, true);
        let computed = metadata_test_tag(banto_tags::COMPUTED_TAG_KIND, false, true);
        let internal = metadata_test_tag(banto_tags::INTERNAL_TAG_KIND, false, true);

        assert_eq!(value_source_for_tag(&plc, &all_simulation), "simulation");
        assert_eq!(value_source_for_tag(&plc, &configured), "real");
        assert_eq!(
            value_source_for_tag(&computed, &all_simulation),
            "derived_simulation"
        );
        assert_eq!(value_source_for_tag(&internal, &all_simulation), "internal");
    }

    #[test]
    fn api_key_external_output_hides_simulation_and_derived_values() {
        let all_simulation = metadata_test_status(CollectionState::Running, RunMode::AllSimulation);
        let configured = metadata_test_status(CollectionState::Running, RunMode::Configured);
        let stopped = metadata_test_status(CollectionState::Stopped, RunMode::AllSimulation);
        let physical = metadata_test_tag(banto_tags::PLC_TAG_KIND, false, true);
        let saved_simulation = metadata_test_tag(banto_tags::PLC_TAG_KIND, true, true);
        let computed = metadata_test_tag(banto_tags::COMPUTED_TAG_KIND, false, true);
        let internal = metadata_test_tag(banto_tags::INTERNAL_TAG_KIND, false, true);

        assert!(!api_key_external_output_allowed(&physical, &all_simulation));
        assert!(api_key_external_output_allowed(&physical, &configured));
        assert!(!api_key_external_output_allowed(
            &saved_simulation,
            &configured
        ));
        assert!(!api_key_external_output_allowed(
            &saved_simulation,
            &stopped
        ));
        assert!(!api_key_external_output_allowed(&computed, &configured));
        assert!(!api_key_external_output_allowed(&computed, &all_simulation));
        assert!(api_key_external_output_allowed(&internal, &all_simulation));
    }

    #[test]
    fn rest_connection_effective_simulation_returns_to_configured_value() {
        let all_simulation = metadata_test_status(CollectionState::Running, RunMode::AllSimulation);
        let configured = metadata_test_status(CollectionState::Running, RunMode::Configured);
        let stopped = metadata_test_status(CollectionState::Stopped, RunMode::AllSimulation);

        assert!(effective_simulation_for_connection(
            "modbus-tcp",
            true,
            false,
            &all_simulation,
        ));
        assert!(!effective_simulation_for_connection(
            "modbus-tcp",
            true,
            false,
            &configured,
        ));
        assert!(!effective_simulation_for_connection(
            "modbus-tcp",
            true,
            false,
            &stopped,
        ));
        assert!(!effective_simulation_for_connection(
            "virtual",
            true,
            false,
            &all_simulation,
        ));
    }

    #[test]
    fn collection_control_status_is_camel_case_and_all_simulation_is_explicit() {
        let status = CollectionStatusResponse::from(CollectionStatus {
            state: CollectionState::Faulted,
            mode: RunMode::AllSimulation,
            run_id: Some(7),
            last_error: Some("T15未実装".to_string()),
            configured_revision: 3,
            running_revision: 2,
        });
        let value = serde_json::to_value(status).expect("status serializes");
        assert_eq!(value["state"], "faulted");
        assert_eq!(value["mode"], "all_simulation");
        assert_eq!(value["runId"], 7);
        assert_eq!(value["configuredRevision"], 3);
        assert_eq!(value["runningRevision"], 2);
        assert_eq!(value["lastError"], "T15未実装");
    }

    #[tokio::test]
    async fn simulation_write_rejection_is_http_503_with_machine_code() {
        let response = write_rejection_response(
            "line1.fast.temp01".to_string(),
            crate::write_path::WriteRejection::SimulationWriteRejected,
        );

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            body,
            serde_json::json!({"error": "simulation_write_rejected"})
        );
    }

    #[tokio::test]
    async fn simulation_output_rejection_is_http_503_with_machine_code() {
        let response = simulation_output_disabled_response();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            body,
            serde_json::json!({"error": "simulation_output_disabled"})
        );
    }

    #[test]
    fn collection_edit_lock_is_http_409_with_current_status() {
        let status = CollectionStatusResponse::from(CollectionStatus {
            state: CollectionState::Running,
            mode: RunMode::Configured,
            run_id: Some(1),
            last_error: None,
            configured_revision: 4,
            running_revision: 4,
        });
        let response = RegistryMutationError::CollectionEditLocked(status).into_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn collection_control_requires_admin_and_csrf_and_is_idempotent() {
        let env = test_env().await;

        let missing_csrf = HttpRequest::builder()
            .method("POST")
            .uri("/api/collection/start")
            .header("Authorization", format!("Bearer {}", env.admin_token))
            .body(Body::empty())
            .unwrap();
        let response = env.router.clone().oneshot(missing_csrf).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let viewer = HttpRequest::builder()
            .method("POST")
            .uri("/api/collection/start")
            .header("Authorization", format!("Bearer {}", env.viewer_token))
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .body(Body::empty())
            .unwrap();
        let response = env.router.clone().oneshot(viewer).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let start = || {
            HttpRequest::builder()
                .method("POST")
                .uri("/api/collection/start")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap()
        };
        let first = env.router.clone().oneshot(start()).await.unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        let first_body = axum::body::to_bytes(first.into_body(), usize::MAX)
            .await
            .unwrap();
        let first_json: serde_json::Value = serde_json::from_slice(&first_body).unwrap();
        assert_eq!(first_json["state"], "running");

        let repeated = env.router.clone().oneshot(start()).await.unwrap();
        let repeated_body = axum::body::to_bytes(repeated.into_body(), usize::MAX)
            .await
            .unwrap();
        let repeated_json: serde_json::Value = serde_json::from_slice(&repeated_body).unwrap();
        assert_eq!(repeated_json["runId"], first_json["runId"]);

        let stop = HttpRequest::builder()
            .method("POST")
            .uri("/api/collection/stop")
            .header("Authorization", format!("Bearer {}", env.admin_token))
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .body(Body::empty())
            .unwrap();
        let response = env.router.clone().oneshot(stop).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT action, resource, result FROM audit_log WHERE resource = 'collection' ORDER BY id",
        )
        .fetch_all(&env.pool)
        .await
        .unwrap();
        assert_eq!(
            rows,
            vec![
                (
                    "denied".to_string(),
                    "collection".to_string(),
                    "denied".to_string()
                ),
                (
                    "start".to_string(),
                    "collection".to_string(),
                    "ok".to_string()
                ),
                (
                    "start".to_string(),
                    "collection".to_string(),
                    "ok".to_string()
                ),
                (
                    "stop".to_string(),
                    "collection".to_string(),
                    "ok".to_string()
                ),
            ]
        );
    }

    #[tokio::test]
    async fn tags_create_while_running_is_accepted_and_queued() {
        let env = test_env().await;

        let (status, conn) = admin_post(
            &env.router,
            "/api/plc-connections",
            &env.admin_token,
            json!({ "name": "line1", "host": "127.0.0.1", "port": 15022 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{conn:?}");

        let (status, group) = admin_post(
            &env.router,
            "/api/collection-groups",
            &env.admin_token,
            json!({ "name": "fast", "plcConnectionId": conn["id"], "periodMs": 100 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{group:?}");

        let start = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post("/api/collection/start")
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(start.status(), StatusCode::OK);

        let response = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post("/api/tags")
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "name": "temp01",
                            "collectionGroupId": group["id"],
                            "address": "40001",
                            "dataType": "i16",
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);

        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["queued"], true);
        assert_eq!(
            body["message"],
            "収集中のため変更を未適用キューに保存しました。"
        );

        let queued_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pending_changes")
            .fetch_one(&env.pool)
            .await
            .unwrap();
        assert_eq!(queued_count, 1);
    }

    #[tokio::test]
    async fn plc_connections_create_while_running_is_accepted_and_queued() {
        let env = test_env().await;

        let start = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post("/api/collection/start")
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(start.status(), StatusCode::OK);

        let response = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post("/api/plc-connections")
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({ "name": "line-running", "host": "127.0.0.1", "port": 15022 })
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);

        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["queued"], true);

        let queued_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pending_changes")
            .fetch_one(&env.pool)
            .await
            .unwrap();
        assert_eq!(queued_count, 1);
    }

    #[tokio::test]
    async fn tags_batch_non_dry_run_while_running_is_accepted_and_queued() {
        let env = test_env().await;

        let (status, conn) = admin_post(
            &env.router,
            "/api/plc-connections",
            &env.admin_token,
            json!({ "name": "line1", "host": "127.0.0.1", "port": 15022 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{conn:?}");

        let (status, group) = admin_post(
            &env.router,
            "/api/collection-groups",
            &env.admin_token,
            json!({ "name": "fast", "plcConnectionId": conn["id"], "periodMs": 100 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{group:?}");

        let start = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post("/api/collection/start")
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(start.status(), StatusCode::OK);

        let response = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post("/api/tags/batch")
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "dryRun": false,
                            "tags": [{
                                "name": "temp01",
                                "collectionGroupId": group["id"],
                                "address": "40001",
                                "dataType": "i16"
                            }]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);

        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["queued"], true);

        let queued_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pending_changes")
            .fetch_one(&env.pool)
            .await
            .unwrap();
        assert_eq!(queued_count, 1);
    }

    #[tokio::test]
    async fn pending_changes_cancel_endpoint_returns_canceled_state() {
        let env = test_env().await;
        let pending_changes = PendingChangesService::new(env.pool.clone());
        let pending = pending_changes
            .create_pending(
                "tags.delete",
                &json!({ "id": 1 }),
                1,
                None,
                Some("admin"),
                Some("admin"),
            )
            .await
            .unwrap();

        let response = env
            .router
            .oneshot(
                HttpRequest::post(format!("/api/pending-changes/{}/cancel", pending.id))
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["state"], "canceled");

        let current = pending_changes.get(pending.id).await.unwrap();
        assert_eq!(current.state, PendingChangeState::Canceled);
    }

    #[tokio::test]
    async fn pending_changes_apply_endpoint_applies_queued_tags_create() {
        let env = test_env().await;

        let (status, conn) = admin_post(
            &env.router,
            "/api/plc-connections",
            &env.admin_token,
            json!({ "name": "line-apply", "host": "127.0.0.1", "port": 15023 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{conn:?}");

        let (status, group) = admin_post(
            &env.router,
            "/api/collection-groups",
            &env.admin_token,
            json!({ "name": "fast", "plcConnectionId": conn["id"], "periodMs": 100 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{group:?}");

        let start = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post("/api/collection/start")
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(start.status(), StatusCode::OK);

        let queued = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post("/api/tags")
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "name": "temp-apply-01",
                            "collectionGroupId": group["id"],
                            "address": "40001",
                            "dataType": "i16"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(queued.status(), StatusCode::ACCEPTED);
        let queued_bytes = axum::body::to_bytes(queued.into_body(), usize::MAX)
            .await
            .unwrap();
        let queued_body: serde_json::Value = serde_json::from_slice(&queued_bytes).unwrap();
        let pending_id = queued_body["pending"]["id"]
            .as_i64()
            .expect("pending id should exist");

        let stop = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post("/api/collection/stop")
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(stop.status(), StatusCode::OK);

        let response = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post(format!("/api/pending-changes/{pending_id}/apply"))
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["state"], "applied");

        let pending = PendingChangesService::new(env.pool.clone())
            .get(pending_id)
            .await
            .unwrap();
        assert_eq!(pending.state, PendingChangeState::Applied);
    }

    #[tokio::test]
    async fn pending_changes_apply_while_running_returns_409_and_keeps_queue_row() {
        let env = test_env().await;

        let (status, conn) = admin_post(
            &env.router,
            "/api/plc-connections",
            &env.admin_token,
            json!({ "name": "line-apply-running", "host": "127.0.0.1", "port": 15024 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{conn:?}");

        let (status, group) = admin_post(
            &env.router,
            "/api/collection-groups",
            &env.admin_token,
            json!({ "name": "fast", "plcConnectionId": conn["id"], "periodMs": 100 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{group:?}");

        let start = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post("/api/collection/start")
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(start.status(), StatusCode::OK);

        let queued = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post("/api/tags")
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "name": "temp-apply-running-01",
                            "collectionGroupId": group["id"],
                            "address": "40001",
                            "dataType": "i16"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(queued.status(), StatusCode::ACCEPTED);
        let queued_bytes = axum::body::to_bytes(queued.into_body(), usize::MAX)
            .await
            .unwrap();
        let queued_body: serde_json::Value = serde_json::from_slice(&queued_bytes).unwrap();
        let pending_id = queued_body["pending"]["id"]
            .as_i64()
            .expect("pending id should exist");

        let response = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post(format!("/api/pending-changes/{pending_id}/apply"))
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"], "collection_edit_locked");
        assert!(body["failureReason"].is_string());
        assert_eq!(body["pending"]["state"], "failed");

        let pending = PendingChangesService::new(env.pool.clone())
            .get(pending_id)
            .await
            .unwrap();
        assert_eq!(pending.state, PendingChangeState::Failed);

        let queued_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pending_changes")
            .fetch_one(&env.pool)
            .await
            .unwrap();
        assert_eq!(queued_count, 1);
    }

    /// TAG-P0-3 follow-up（2026-08-12）: 適用対象の `plc_connections` 行が
    /// enqueue 時点から変わっていなければ、`base_fingerprint` ガードは何も
    /// 妨げない（ハッピーパス）。
    #[tokio::test]
    async fn pending_apply_plc_connection_update_succeeds_when_row_unchanged() {
        let env = test_env().await;

        let (status, conn) = admin_post(
            &env.router,
            "/api/plc-connections",
            &env.admin_token,
            json!({ "name": "line-fp-ok", "host": "127.0.0.1", "port": 15030 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{conn:?}");
        let conn_id = conn["id"].as_i64().unwrap();

        let start = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post("/api/collection/start")
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(start.status(), StatusCode::OK);

        let queued = env
            .router
            .clone()
            .oneshot(
                HttpRequest::put(format!("/api/plc-connections/{conn_id}"))
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({ "name": "line-fp-ok-renamed", "host": "127.0.0.1", "port": 15030 })
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(queued.status(), StatusCode::ACCEPTED);
        let queued_bytes = axum::body::to_bytes(queued.into_body(), usize::MAX)
            .await
            .unwrap();
        let queued_body: serde_json::Value = serde_json::from_slice(&queued_bytes).unwrap();
        let pending_id = queued_body["pending"]["id"].as_i64().unwrap();

        let stop = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post("/api/collection/stop")
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(stop.status(), StatusCode::OK);

        let response = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post(format!("/api/pending-changes/{pending_id}/apply"))
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["state"], "applied");

        let updated = PlcConnectionService::new(env.pool.clone())
            .get(conn_id)
            .await
            .unwrap();
        assert_eq!(updated.name, "line-fp-ok-renamed");
    }

    /// TAG-P0-3 follow-up（2026-08-12）: pending change を enqueue した後、
    /// キューを経由しない別経路（別セッションなどを模した直接の service
    /// 呼び出し）で同じ `plc_connections` 行が書き換えられていた場合、
    /// apply は conflict として拒否され、pending 行は `failed` へ遷移して
    /// 具体的な `failure_reason` を残す。その後 `failed` からキャンセル
    /// できることも確認する（従来は `pending` のみキャンセル可能だった）。
    #[tokio::test]
    async fn pending_apply_plc_connection_update_conflicts_when_row_changed_out_of_band() {
        let env = test_env().await;

        let (status, conn) = admin_post(
            &env.router,
            "/api/plc-connections",
            &env.admin_token,
            json!({ "name": "line-fp-conflict", "host": "127.0.0.1", "port": 15031 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{conn:?}");
        let conn_id = conn["id"].as_i64().unwrap();

        let start = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post("/api/collection/start")
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(start.status(), StatusCode::OK);

        let queued = env
            .router
            .clone()
            .oneshot(
                HttpRequest::put(format!("/api/plc-connections/{conn_id}"))
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({ "name": "line-fp-conflict-queued", "host": "127.0.0.1", "port": 15031 })
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(queued.status(), StatusCode::ACCEPTED);
        let queued_bytes = axum::body::to_bytes(queued.into_body(), usize::MAX)
            .await
            .unwrap();
        let queued_body: serde_json::Value = serde_json::from_slice(&queued_bytes).unwrap();
        let pending_id = queued_body["pending"]["id"].as_i64().unwrap();

        // 「別経路での編集」を、pending queue を経由しない直接の service 層
        // 呼び出し（`update_tx` ではなく非トランザクション `update`）で
        // 模す - 別セッションが同じ行を先に書き換えたケースに相当する。
        PlcConnectionService::new(env.pool.clone())
            .update(
                conn_id,
                PlcConnectionInput {
                    name: "line-fp-conflict-hijacked".to_string(),
                    protocol: "modbus-tcp".to_string(),
                    host: "127.0.0.1".to_string(),
                    port: 15031,
                    unit_id: 1,
                    enabled: true,
                    simulation: false,

                    word_order: "low_high".to_string(),
                },
            )
            .await
            .unwrap();

        let stop = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post("/api/collection/stop")
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(stop.status(), StatusCode::OK);

        let response = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post(format!("/api/pending-changes/{pending_id}/apply"))
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"], "pending_apply_conflict");
        assert_eq!(body["resource"], "plc_connections");

        let pending = PendingChangesService::new(env.pool.clone())
            .get(pending_id)
            .await
            .unwrap();
        assert_eq!(pending.state, PendingChangeState::Failed);
        let failure_reason = pending
            .failure_reason
            .expect("failure_reason should be set");
        assert!(
            !failure_reason.is_empty() && failure_reason != "pending change の適用に失敗しました",
            "failure_reason should be specific, not generic: {failure_reason}"
        );
        assert!(
            failure_reason.contains("変更されています"),
            "failure_reason should explain the staleness conflict: {failure_reason}"
        );

        // 元の接続は「別経路」の編集内容のまま(適用は拒否されたので上書き
        // されていない)。
        let untouched = PlcConnectionService::new(env.pool.clone())
            .get(conn_id)
            .await
            .unwrap();
        assert_eq!(untouched.name, "line-fp-conflict-hijacked");

        // failed からのキャンセルは新たに許可された復旧経路。
        let cancel_response = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post(format!("/api/pending-changes/{pending_id}/cancel"))
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(cancel_response.status(), StatusCode::OK);
        let cancel_bytes = axum::body::to_bytes(cancel_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let cancel_body: serde_json::Value = serde_json::from_slice(&cancel_bytes).unwrap();
        assert_eq!(cancel_body["state"], "canceled");
    }

    /// TAG-P0-3 follow-up（2026-08-12）: `collection_groups.update` でも
    /// 同じ conflict 検出が働く。
    #[tokio::test]
    async fn pending_apply_collection_group_update_conflicts_when_row_changed_out_of_band() {
        let env = test_env().await;

        let (status, conn) = admin_post(
            &env.router,
            "/api/plc-connections",
            &env.admin_token,
            json!({ "name": "line-group-fp", "host": "127.0.0.1", "port": 15032 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{conn:?}");
        let conn_id = conn["id"].as_i64().unwrap();

        let (status, group) = admin_post(
            &env.router,
            "/api/collection-groups",
            &env.admin_token,
            json!({ "name": "fast", "plcConnectionId": conn_id, "periodMs": 100 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{group:?}");
        let group_id = group["id"].as_i64().unwrap();

        let start = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post("/api/collection/start")
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(start.status(), StatusCode::OK);

        let queued = env
            .router
            .clone()
            .oneshot(
                HttpRequest::put(format!("/api/collection-groups/{group_id}"))
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "name": "fast-queued",
                            "plcConnectionId": conn_id,
                            "periodMs": 100
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(queued.status(), StatusCode::ACCEPTED);
        let queued_bytes = axum::body::to_bytes(queued.into_body(), usize::MAX)
            .await
            .unwrap();
        let queued_body: serde_json::Value = serde_json::from_slice(&queued_bytes).unwrap();
        let pending_id = queued_body["pending"]["id"].as_i64().unwrap();

        // 別経路での編集（pending queue を経由しない直接の service 呼び出し）。
        CollectionGroupService::new(env.pool.clone())
            .update(
                group_id,
                CollectionGroupInput {
                    name: "fast-hijacked".to_string(),
                    plc_connection_id: conn_id,
                    period_ms: 100,
                    enabled: true,
                },
            )
            .await
            .unwrap();

        let stop = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post("/api/collection/stop")
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(stop.status(), StatusCode::OK);

        let response = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post(format!("/api/pending-changes/{pending_id}/apply"))
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"], "pending_apply_conflict");
        assert_eq!(body["resource"], "collection_groups");

        let pending = PendingChangesService::new(env.pool.clone())
            .get(pending_id)
            .await
            .unwrap();
        assert_eq!(pending.state, PendingChangeState::Failed);
    }

    /// TAG-P0-3 follow-up（2026-08-12）、最重要の回帰ガード:
    /// フィンガープリントは per-resource でなければならない
    /// （グローバル `configured_revision` 比較に「こっそり」置き換えられて
    /// いないことを保証する）。無関係な2件の pending change（別々の
    /// `plc_connections` 行）を running 中に queue し、片方を適用して
    /// `commit_catalog_and_notify` にグローバル revision を進めさせた後、
    /// もう片方の適用が「global revision が動いた」ことに巻き込まれず
    /// 成功することを確認する。
    #[tokio::test]
    async fn pending_apply_fingerprint_is_per_resource_not_global_revision() {
        let env = test_env().await;

        let (status, conn_a) = admin_post(
            &env.router,
            "/api/plc-connections",
            &env.admin_token,
            json!({ "name": "line-a", "host": "127.0.0.1", "port": 15040 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{conn_a:?}");
        let conn_a_id = conn_a["id"].as_i64().unwrap();

        let (status, conn_b) = admin_post(
            &env.router,
            "/api/plc-connections",
            &env.admin_token,
            json!({ "name": "line-b", "host": "127.0.0.1", "port": 15041 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{conn_b:?}");
        let conn_b_id = conn_b["id"].as_i64().unwrap();

        let start = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post("/api/collection/start")
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(start.status(), StatusCode::OK);

        // A・B それぞれ独立に pending update を queue する。
        let queue_update = |id: i64, name: &'static str, port: i64| {
            let router = env.router.clone();
            let token = env.admin_token.clone();
            async move {
                let response = router
                    .oneshot(
                        HttpRequest::put(format!("/api/plc-connections/{id}"))
                            .header("Authorization", format!("Bearer {token}"))
                            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                            .header("content-type", "application/json")
                            .body(Body::from(
                                json!({ "name": name, "host": "127.0.0.1", "port": port })
                                    .to_string(),
                            ))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::ACCEPTED);
                let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                    .await
                    .unwrap();
                let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                body["pending"]["id"].as_i64().unwrap()
            }
        };
        let pending_a_id = queue_update(conn_a_id, "line-a-renamed", 15040).await;
        let pending_b_id = queue_update(conn_b_id, "line-b-renamed", 15041).await;

        let stop = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post("/api/collection/stop")
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(stop.status(), StatusCode::OK);

        // A を先に適用する - 成功すれば `commit_catalog_and_notify` が
        // グローバル `configured_revision` を進める。
        let apply_a = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post(format!("/api/pending-changes/{pending_a_id}/apply"))
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(apply_a.status(), StatusCode::OK);
        let apply_a_bytes = axum::body::to_bytes(apply_a.into_body(), usize::MAX)
            .await
            .unwrap();
        let apply_a_body: serde_json::Value = serde_json::from_slice(&apply_a_bytes).unwrap();
        assert_eq!(apply_a_body["state"], "applied");

        // B の適用は、A の適用でグローバル revision が進んだことに巻き込ま
        // れず成功しなければならない - B のフィンガープリントは B 自身の
        // 行に対して取られており、A の変更とは無関係だから(per-resource
        // ガードの核心)。
        let apply_b = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post(format!("/api/pending-changes/{pending_b_id}/apply"))
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let apply_b_bytes_status = apply_b.status();
        let apply_b_bytes = axum::body::to_bytes(apply_b.into_body(), usize::MAX)
            .await
            .unwrap();
        let apply_b_body: serde_json::Value = serde_json::from_slice(&apply_b_bytes).unwrap();
        assert_eq!(
            apply_b_bytes_status,
            StatusCode::OK,
            "B's apply must not be rejected by A's global revision bump: {apply_b_body:?}"
        );
        assert_eq!(apply_b_body["state"], "applied");

        let updated_a = PlcConnectionService::new(env.pool.clone())
            .get(conn_a_id)
            .await
            .unwrap();
        assert_eq!(updated_a.name, "line-a-renamed");
        let updated_b = PlcConnectionService::new(env.pool.clone())
            .get(conn_b_id)
            .await
            .unwrap();
        assert_eq!(updated_b.name, "line-b-renamed");
    }

    // --- 監査ログ retention 設定 (docs/banto-hub-remaining-plan.md P3-a) ----

    /// `GET/PUT /api/audit-log/config`: admin 限定（viewer は403）、既定値
    /// （90日/100,000件）、保存した値の round-trip、PUT が
    /// `audit_log_config`リソースへの`update`監査エントリを1件だけ記録する
    /// ことを1本のテストで固定する（`collection_control_requires_admin_and_csrf_and_is_idempotent`
    /// と同じ「1テストで RBAC + 挙動 + 監査を通して確認する」形）。
    #[tokio::test]
    async fn audit_log_config_round_trips_and_requires_admin() {
        let env = test_env().await;

        let viewer_get = HttpRequest::builder()
            .method("GET")
            .uri("/api/audit-log/config")
            .header("Authorization", format!("Bearer {}", env.viewer_token))
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .body(Body::empty())
            .unwrap();
        let response = env.router.clone().oneshot(viewer_get).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let admin_get = HttpRequest::builder()
            .method("GET")
            .uri("/api/audit-log/config")
            .header("Authorization", format!("Bearer {}", env.admin_token))
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .body(Body::empty())
            .unwrap();
        let response = env.router.clone().oneshot(admin_get).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["retentionDays"], 90);
        assert_eq!(body["retentionRows"], 100_000);

        let put_body = || {
            Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "retentionDays": 30,
                    "retentionRows": 5000
                }))
                .unwrap(),
            )
        };

        let viewer_put = HttpRequest::builder()
            .method("PUT")
            .uri("/api/audit-log/config")
            .header("Authorization", format!("Bearer {}", env.viewer_token))
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .header("content-type", "application/json")
            .body(put_body())
            .unwrap();
        let response = env.router.clone().oneshot(viewer_put).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let admin_put = HttpRequest::builder()
            .method("PUT")
            .uri("/api/audit-log/config")
            .header("Authorization", format!("Bearer {}", env.admin_token))
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .header("content-type", "application/json")
            .body(put_body())
            .unwrap();
        let response = env.router.clone().oneshot(admin_put).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let applied: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(applied["retentionDays"], 30);
        assert_eq!(applied["retentionRows"], 5000);

        let refetch = HttpRequest::builder()
            .method("GET")
            .uri("/api/audit-log/config")
            .header("Authorization", format!("Bearer {}", env.admin_token))
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .body(Body::empty())
            .unwrap();
        let response = env.router.clone().oneshot(refetch).await.unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let refetched: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(refetched["retentionDays"], 30);
        assert_eq!(refetched["retentionRows"], 5000);

        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT action, resource, result FROM audit_log WHERE resource = 'audit_log_config' ORDER BY id",
        )
        .fetch_all(&env.pool)
        .await
        .unwrap();
        assert_eq!(
            rows,
            vec![(
                "update".to_string(),
                "audit_log_config".to_string(),
                "ok".to_string()
            )]
        );
    }

    /// `POST /api/audit-log/list`が読む前に retention 設定に従って
    /// opportunistic に剪定すること（`crate::audit::AuditLogService::prune`
    /// の配線そのもの、docs/banto-hub-remaining-plan.md P3-a）を、REST
    /// エンドポイント越しに確認する - `crate::audit`の単体テストは
    /// `prune`自体の正しさを担保済みなので、ここでは「呼ばれること」だけを
    /// 見る。
    #[tokio::test]
    async fn audit_log_list_opportunistically_prunes_by_configured_retention() {
        let env = test_env().await;

        for i in 0..5 {
            sqlx::query(
                "INSERT INTO audit_log (actor_username, actor_role, action, resource, entity_id, detail, origin, result) \
                 VALUES ('admin', 'admin', 'create', 'items', ?, NULL, 'rest', 'ok')",
            )
            .bind(i.to_string())
            .execute(&env.pool)
            .await
            .unwrap();
        }

        let put = HttpRequest::builder()
            .method("PUT")
            .uri("/api/audit-log/config")
            .header("Authorization", format!("Bearer {}", env.admin_token))
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "retentionDays": null,
                    "retentionRows": 2
                }))
                .unwrap(),
            ))
            .unwrap();
        let response = env.router.clone().oneshot(put).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // PUT 自体も1件 audit_log 行を作るため、この時点で 5(シード) + 1(PUT)
        // = 6件。retentionRows=2 で list を叩くと、剪定後は最新2件のみ残る。
        let list = HttpRequest::builder()
            .method("POST")
            .uri("/api/audit-log/list")
            .header("Authorization", format!("Bearer {}", env.admin_token))
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({})).unwrap(),
            ))
            .unwrap();
        let response = env.router.clone().oneshot(list).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["totalCount"], 2);

        let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
            .fetch_one(&env.pool)
            .await
            .unwrap();
        assert_eq!(remaining, 2);
    }
}
