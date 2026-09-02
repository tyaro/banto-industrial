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
use banto_broker::{is_supported_protocol, BrokerConnectionStatus, BrokerError};
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
    auth_routes, require_banto_client_header, sse_route, ApiError, AuthState, Identity, ServerEvent,
};
use banto_tags::{
    BatchTagOutcome, BatchTagUpdateOutcome, CollectionGroup, CollectionGroupInput,
    CollectionGroupService, GroupTagCount, PlcConnection, PlcConnectionInput, PlcConnectionService,
    Tag, TagInput, TagService, TagUpdateError,
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
use crate::commissioning::{CommissioningService, CommissioningState};
use crate::controller::{CollectionController, CollectionState, CollectionStatus, RunMode};
use crate::hub::{CollectorManager, SimulationCoverageReport, TagEntry, TagMap};
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

/// 試運転モード（docs/tag-server-design.md §5.6「試運転モードとロックダウン」・
/// 2026-08-30 オーナー決定）: ロックダウン済みなら従来どおり bearer token
/// から identity を引く。**未ロックダウン（試運転モード）なら、渡された
/// `headers`の中身に関わらず無条件で合成の管理者 identity
/// (`crate::commissioning::synthetic_identity`) を返す** - 設計 §5.6
/// 「actor_identity() が合成の管理者 identity を返す」「これにより
/// require_editor などの下流が現行のまま動く」のとおり。この関数の呼び出し
/// 元（`require_editor`・`record_write`・監査ログ記録の各所）は一切
/// 分岐を増やさずに「admin 相当の identity が常に手に入る」前提のまま
/// 動く。監査ログ (`audit_log.actor_username`) にはこの合成 id
/// (`commissioning`) がそのまま記録される - 「試運転モード中に行われた
/// 操作」だと後から判別できる、意図した挙動（設計 §5.6）。
fn actor_identity(
    headers: &HeaderMap,
    auth: &AuthState,
    commissioning: &CommissioningState,
) -> Option<Identity> {
    if !commissioning.is_locked_down() {
        return Some(crate::commissioning::synthetic_identity());
    }
    bearer_token(headers).and_then(|token| auth.identity_for(token))
}

/// `AuthState` + `CommissioningState`をまとめた、
/// [`require_auth_or_commissioning`]の`middleware::from_fn_with_state`用
/// state。従来の`banto_server::require_auth`（`State<AuthState>`のみ）を
/// 直接差し替えず、この型を挟む1段ラッパーにしてある理由は
/// [`require_auth_or_commissioning`]のdoc comment参照。
#[derive(Clone)]
struct AuthGate {
    auth: AuthState,
    commissioning: CommissioningState,
}

/// 試運転モード（設計 §5.6・2026-08-30 オーナー決定）: `banto_server::require_auth`
/// をそのまま`.layer(middleware::from_fn_with_state(auth, require_auth))`
/// で貼ると、常にセッション bearer を要求してしまい試運転モードの
/// 「管理 UI / 管理 REST は認証なしで操作できる」を実現できない。この
/// ラッパーが手前に立ち、ロックダウン済みなら`require_auth`と全く同じ
/// 判定（bearer token 検証、失敗時 401 - `unauthorized_response`は
/// `banto_server::require_auth`の401ボディをそのまま再現したもの、この
/// ファイル冒頭の`unauthorized_response`のdoc comment参照）を行い、
/// **未ロックダウン中は無条件で次のレイヤーへ素通しする**（設計 §5.6
/// 「require_auth を通さない（またはバイパスする）」）。素通しした後段の
/// `require_role_at_least`/`require_editor`は[`actor_identity`]経由で
/// 合成 admin identity を受け取るので、トークン無しでも「admin 相当」
/// として動く（このファイル管理系ルーターの`require_auth`レイヤー全箇所
/// （`users_router`等）でこれに差し替える - `/api/v1/*`のタグ空間 API
/// （`require_tag_space_auth`、API キー認証）はこの対象外 - 設計 §5.6は
/// 「管理 UI / 管理 REST」のみを試運転モードの対象にしている）。
///
/// `admin_tag_stream_router`（`/api/tag-stream`、試運転モード対応・
/// 2026-08-31 オーナー決定）用に、ロックダウン済み時の bearer 取得へ
/// [`extract_ws_protocol_token`]によるフォールバックを追加した -
/// ブラウザの`WebSocket`は`Authorization`ヘッダを送れないため、
/// `require_tag_space_auth`が`/api/v1/stream`向けに使っているのと同じ
/// `Sec-WebSocket-Protocol: bearer, <token>`の運び方をここでも認める
/// 必要がある。`extract_ws_protocol_token`はパスを厳密一致で許可リスト化
/// している（`/api/v1/stream`と[`ADMIN_TAG_STREAM_PATH`]の2つのみ）ので、
/// この関数を`.layer`として使う他の管理系ルーター（`/api/status`・
/// `/api/users`等）には一切影響しない - それらのパスに対しては
/// `extract_ws_protocol_token`が常に`None`を返す。
async fn require_auth_or_commissioning(
    State(gate): State<AuthGate>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if !gate.commissioning.is_locked_down() {
        return next.run(req).await;
    }
    let token = bearer_token(req.headers())
        .map(str::to_string)
        .or_else(|| extract_ws_protocol_token(req.uri().path(), req.headers()));
    match token {
        Some(token) if gate.auth.verify(&token) => next.run(req).await,
        _ => unauthorized_response(),
    }
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
/// 2026-08-31 オーナー決定（試運転モード対応の続き）: 管理系 WS
/// `admin_tag_stream_router`（`/api/tag-stream`）も同じブラウザ制約
/// （`Authorization`を送れない）を抱えるため、許可パスに
/// [`ADMIN_TAG_STREAM_PATH`]を追加した。`/api/v1/stream`自身の挙動は
/// 一切変えていない - この関数を呼ぶのは`require_tag_space_auth`
/// （`/api/v1/stream`専用）と`require_auth_or_commissioning`
/// （`admin_tag_stream_router`はこれ経由、他の管理系ルーターは
/// このパス自体が来ないので影響なし）の2箇所のみで、どちらも
/// パスの厳密一致で絞っているため他ルートへの越境は起きない。
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
    if path != "/api/v1/stream" && path != ADMIN_TAG_STREAM_PATH {
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
/// 試運転モード対応（設計 §5.6）で `commissioning` 引数が増えて8引数に
/// なった - このファイルの他の合成関数（`api_router_with_controller_mode`
/// 等）と同じく `#[allow]` で許容する。
#[allow(clippy::too_many_arguments)]
async fn record_write(
    audit: &AuditLogService,
    auth: &AuthState,
    commissioning: &CommissioningState,
    headers: &HeaderMap,
    action: &str,
    resource: &str,
    entity_id: &str,
    detail: Option<serde_json::Value>,
) {
    let identity = actor_identity(headers, auth, commissioning);
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
    commissioning: CommissioningState,
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
    // 試運転モード（設計 §5.6・2026-08-30 オーナー決定）: 未ロックダウン中は
    // トークンの有無に関わらず合成 admin identity を使う -
    // `require_auth_or_commissioning`が手前で既に素通ししている前提なので、
    // ここで従来どおりトークン必須にしてしまうと「管理 REST は認証なしで
    // 操作できる」が実現できない（[`actor_identity`]と同じ判断）。
    let identity = if !guard.commissioning.is_locked_down() {
        Some(crate::commissioning::synthetic_identity())
    } else {
        bearer_token(req.headers()).and_then(|token| guard.auth.identity_for(token))
    };
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
    commissioning: &CommissioningState,
    audit: &AuditLogService,
    headers: &HeaderMap,
    resource: &'static str,
    method: &str,
    path: &str,
) -> Result<(), BantoError> {
    match actor_identity(headers, auth, commissioning) {
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
    commissioning: CommissioningState,
    audit: AuditLogService,
}

/// `users_delete`専用: 呼び出し元自身の numeric row id を解決する
/// （自己削除ガード`UsersService::delete_user`のdoc comment参照）。
///
/// 試運転モード（設計 §5.6・2026-08-30 オーナー決定）中は、bearer token を
/// 一切要求せず、`users`テーブルに絶対に存在しない sentinel の
/// `id: 0`（`AUTOINCREMENT`は1始まり）を持つ合成`UserIdentity`を返す -
/// `delete_user`の`id == acting_user_id`という自己削除ガードは、実在しない
/// idとは決して一致しないため無害に素通りする（合成 identity は
/// 「削除されうる実在アカウント」ではないので、このガードの対象外で
/// 正しい）。
async fn acting_user(
    headers: &HeaderMap,
    auth: &AuthState,
    commissioning: &CommissioningState,
    users: &UsersService,
) -> Result<UserIdentity, BantoError> {
    if !commissioning.is_locked_down() {
        return Ok(UserIdentity {
            id: 0,
            username: crate::commissioning::SYNTHETIC_ACTOR_ID.to_string(),
            display_name: "試運転モード".to_string(),
            role: Role::Admin,
        });
    }
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
        &state.commissioning,
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
        &state.commissioning,
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
        &state.commissioning,
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
    let acting = acting_user(&headers, &state.auth, &state.commissioning, &state.users).await?;
    state.users.delete_user(id, acting.id).await?;
    record_write(
        &state.audit,
        &state.auth,
        &state.commissioning,
        &headers,
        "delete",
        "users",
        &id.to_string(),
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

fn users_router(
    users: UsersService,
    audit: AuditLogService,
    auth: AuthState,
    commissioning: CommissioningState,
) -> Router {
    let state = UsersAdminState {
        users,
        auth: auth.clone(),
        commissioning: commissioning.clone(),
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
                commissioning: commissioning.clone(),
                min: Role::Admin,
                resource: "users",
                audit,
            },
            require_role_at_least,
        ))
        .layer(middleware::from_fn_with_state(
            AuthGate {
                auth,
                commissioning,
            },
            require_auth_or_commissioning,
        ))
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
    commissioning: CommissioningState,
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
        actor_identity(req.headers(), &state.auth, &state.commissioning)
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
    commissioning: CommissioningState,
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
        &state.commissioning,
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
    commissioning: CommissioningState,
    manager: Arc<CollectorManager>,
) -> Router {
    let state = AuditLogState {
        audit: audit.clone(),
        manager,
        auth: auth.clone(),
        commissioning: commissioning.clone(),
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
                commissioning: commissioning.clone(),
                min: Role::Admin,
                resource: "audit_log",
                audit,
            },
            require_role_at_least,
        ))
        .layer(middleware::from_fn_with_state(
            AuthGate {
                auth,
                commissioning,
            },
            require_auth_or_commissioning,
        ))
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
    commissioning: CommissioningState,
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
        &state.commissioning,
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
        &state.commissioning,
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
        &state.commissioning,
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
    commissioning: CommissioningState,
    manager: Arc<CollectorManager>,
) -> Router {
    let state = ApiKeysAdminState {
        api_keys,
        auth: auth.clone(),
        commissioning: commissioning.clone(),
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
                commissioning: commissioning.clone(),
                min: Role::Admin,
                resource: "api_keys",
                audit,
            },
            require_role_at_least,
        ))
        .layer(middleware::from_fn_with_state(
            AuthGate {
                auth,
                commissioning,
            },
            require_auth_or_commissioning,
        ))
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
    commissioning: CommissioningState,
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

    let identity = actor_identity(headers, &state.auth, &state.commissioning);
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
        &state.commissioning,
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
    commissioning: CommissioningState,
    events: broadcast::Sender<ServerEvent>,
) -> Router {
    let state = WriteControlAdminState {
        write_control,
        manager,
        auth: auth.clone(),
        commissioning: commissioning.clone(),
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
                commissioning: commissioning.clone(),
                min: Role::Admin,
                resource: "write_control",
                audit,
            },
            require_role_at_least,
        ))
        .layer(middleware::from_fn_with_state(
            AuthGate {
                auth,
                commissioning,
            },
            require_auth_or_commissioning,
        ))
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
    commissioning: CommissioningState,
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
        &state.commissioning,
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
        &state.commissioning,
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
    commissioning: CommissioningState,
    events: broadcast::Sender<ServerEvent>,
) -> Router {
    let state = TestOutputAdminState {
        test_output,
        controller,
        auth: auth.clone(),
        commissioning: commissioning.clone(),
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
                commissioning: commissioning.clone(),
                min: Role::Admin,
                resource: "test_output",
                audit,
            },
            require_role_at_least,
        ))
        .layer(middleware::from_fn_with_state(
            AuthGate {
                auth,
                commissioning,
            },
            require_auth_or_commissioning,
        ))
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
    commissioning: CommissioningState,
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
        &state.commissioning,
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
    commissioning: CommissioningState,
    events: broadcast::Sender<ServerEvent>,
) -> Router {
    let state = CollectionAdminState {
        controller,
        manager,
        auth: auth.clone(),
        commissioning: commissioning.clone(),
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
                commissioning: commissioning.clone(),
                min: Role::Admin,
                resource: "collection",
                audit,
            },
            require_role_at_least,
        ))
        .layer(middleware::from_fn_with_state(
            AuthGate {
                auth,
                commissioning,
            },
            require_auth_or_commissioning,
        ))
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
    commissioning: CommissioningState,
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
        &state.commissioning,
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
    commissioning: CommissioningState,
    events: broadcast::Sender<ServerEvent>,
) -> Router {
    let state = MqttSettingsAdminState {
        manager,
        mqtt,
        auth: auth.clone(),
        commissioning: commissioning.clone(),
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
                commissioning: commissioning.clone(),
                min: Role::Admin,
                resource: "mqtt_settings",
                audit,
            },
            require_role_at_least,
        ))
        .layer(middleware::from_fn_with_state(
            AuthGate {
                auth,
                commissioning,
            },
            require_auth_or_commissioning,
        ))
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
    commissioning: CommissioningState,
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
        &state.commissioning,
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
    commissioning: CommissioningState,
    events: broadcast::Sender<ServerEvent>,
) -> Router {
    let state = GrpcSettingsAdminState {
        manager,
        grpc_server,
        auth: auth.clone(),
        commissioning: commissioning.clone(),
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
                commissioning: commissioning.clone(),
                min: Role::Admin,
                resource: "grpc_settings",
                audit,
            },
            require_role_at_least,
        ))
        .layer(middleware::from_fn_with_state(
            AuthGate {
                auth,
                commissioning,
            },
            require_auth_or_commissioning,
        ))
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
    commissioning: CommissioningState,
) -> Router {
    let state = WriteAuditAdminState { write_audit };
    Router::new()
        .route("/api/write-audit/list", post(write_audit_list))
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            RoleGuard {
                auth: auth.clone(),
                commissioning: commissioning.clone(),
                min: Role::Admin,
                resource: "write_audit",
                audit,
            },
            require_role_at_least,
        ))
        .layer(middleware::from_fn_with_state(
            AuthGate {
                auth,
                commissioning,
            },
            require_auth_or_commissioning,
        ))
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
    commissioning: CommissioningState,
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
    let identity = actor_identity(headers, &state.auth, &state.commissioning);
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
        &state.commissioning,
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
        &state.commissioning,
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
        &state.commissioning,
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
        &state.commissioning,
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
        &state.commissioning,
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
        &state.commissioning,
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
        &state.commissioning,
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
        &state.commissioning,
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
        &state.commissioning,
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
        &state.commissioning,
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
        &state.commissioning,
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
        &state.commissioning,
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
        &state.commissioning,
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

/// `POST /api/tags/list`（T18-5a 第2段、docs/banto-hub-t18-design.md §4
/// 決定6「薄い部品の先行配線」）: `write_audit_list`/`audit_log_list` と同型の
/// 素通しハンドラ - `ListParams` をそのままサービスへ渡し `ListResult<Tag>`
/// を返すだけ。認可は `GET /api/tags` と同じくルーター全体の
/// `require_auth`（viewer 以上で読み取り可）のみで、`require_editor` は
/// 呼ばない（読み取り専用エンドポイントのため）。
async fn tags_list_query(
    State(state): State<TagRegistryState>,
    Json(params): Json<ListParams>,
) -> Result<Json<ListResult<Tag>>, ApiError> {
    Ok(Json(state.tags.list(params).await?))
}

/// `GET /api/tags/group-counts`（T18-5a 第2段、同 §4 決定6）: グループ別の
/// タグ件数集計。`GET /api/tags` と同じ読み取り専用エンドポイントなので
/// `require_auth` のみ。
async fn tags_group_counts(
    State(state): State<TagRegistryState>,
) -> Result<Json<Vec<GroupTagCount>>, ApiError> {
    Ok(Json(state.tags.count_by_group().await?))
}

#[derive(Debug, Deserialize)]
struct AddressWritableQuery {
    /// `banto_tags::plc_connection::ALLOWED_PROTOCOLS`（`"modbus-tcp"` /
    /// `"slmp"` / `"virtual"`）のいずれかを想定するが、未知の値もエラーには
    /// せず単に「判定材料が無い」＝`writable: null` として扱う（下記応答の
    /// フィールド doc 参照）- 将来プロトコルが増えてもこのエンドポイント自体
    /// の追随を待たずに安全側（既定を適用しない）へ倒れる。
    protocol: String,
    /// 生のアドレス文字列（例: `"30001"`）。空文字・不正な形式・入力途中の
    /// 文字列も許容し、単に `writable: null` を返す（下記参照）。
    address: String,
}

/// [`tags_address_writable`]の応答。**T19 S1-b0（2026-09-02 オーナー決定）:
/// `null`(`None`)は「判定不能」だけを意味する** - 領域制限が無いことの
/// 表明ではない。3値の意味は次のとおり:
///
/// - `writable: Some(true)`, `area: None` -
///   領域による書き込み制限が存在しないプロトコル（`slmp`/`virtual`。SLMP
///   デバイスに Modbus の 1xxxx/3xxxx に相当する「恒久的に読み取り専用の
///   エリア」という概念は無い）。書き込み可能領域の観点では既定を適用して
///   よい。
/// - `writable: Some(bool)`, `area: Some(_)` - `protocol == "modbus-tcp"`
///   で `address` が Modbus 参照番号としてパースできた。`area` は
///   [`AddressArea`]の`Display`文字列（`"coil"`/`"discrete_input"`/
///   `"input_register"`/`"holding_register"`）、`writable` はその領域の
///   [`AddressArea::is_writable`]。
/// - `writable: None`, `area: None` - **判定不能**（`address` が空・入力
///   途中・不正な形式、または `protocol` がこの3つのいずれでもない）。
///   `apps/banto-hub/src/lib/banto/writableDefault.ts`の
///   `writableArea: boolean | undefined`契約と対応させるため、UI は
///   これを`undefined`として扱い、**チェックボックスの既定 ON を適用しない**
///   （安全側 - 誤って読み取り専用領域に既定 ON してしまうより、既定が
///   一時的に効かない方を選ぶ）。
///
/// **呼び出し失敗時のフェイルセーフ契約:** このエンドポイント自体が
/// 4xx/5xx を返した場合、あるいはネットワーク到達不能などで応答が
/// 得られなかった場合も、呼び出し側は上記の`writable: None`と同じ扱い
/// （＝既定を適用しない）にすること。判定不能を「安全に倒す」という
/// この応答の設計方針を、通信失敗時にも一貫させるため。
#[derive(Debug, Serialize)]
struct AddressWritableResponse {
    writable: Option<bool>,
    area: Option<String>,
}

/// `GET /api/tags/address-writable`（T19 S1-b0、2026-09-02 オーナー判断）:
/// 「Modbus の 1xxxx（discrete input）/3xxxx（input register）は書き込め
/// ない」という規則の正は `banto_plc_address::AddressArea::is_writable`
/// （`banto-plc`が re-export し、`banto-tags`の登録時検証・
/// `banto-plc-write`の書き込みプランナーも同じ定義を読む）1箇所のみ - この
/// ハンドラはそれをアドレス文字列に適用して返すだけで、**UI 側に「先頭桁で
/// エリアを判定する」ロジックを持たせないための唯一の入口**にする
/// （`crates/banto-tags/src/tag.rs::modbus_read_only_area`のような「桁数＋
/// 先頭桁」の狭い複製すら UI には作らせない、という 2026-09-02 の設計判断 -
/// アドレスのパース自体をサーバーに一本化する）。
///
/// `/api/tags` と同じ読み取り専用・副作用なしのエンドポイントなので
/// `require_auth`（viewer 以上）のみで`require_editor`は呼ばない。DB
/// アクセスも無い純粋な計算なので`State<TagRegistryState>`も取らない。
/// `plc_connections_test`と同じ理由で意図的に`#[utoipa::path]`を付けず
/// `ApiDoc`にも加えない(`/api/v1/*`のみを文書化する既存方針、このファイル
/// 冒頭「二系統に分かれたルーター」節参照)。
///
/// `null`の意味論・呼び出し失敗時の契約は[`AddressWritableResponse`]の
/// doc comment を参照。
async fn tags_address_writable(
    Query(query): Query<AddressWritableQuery>,
) -> Json<AddressWritableResponse> {
    match query.protocol.as_str() {
        "modbus-tcp" => match Address::parse(&query.address) {
            Ok(addr) => match addr.as_modbus_ref() {
                Some((area, _offset, _bit)) => Json(AddressWritableResponse {
                    writable: Some(area.is_writable()),
                    area: Some(area.to_string()),
                }),
                // `Address::parse` only ever produces `ModbusRef` (see
                // `banto_plc::address`'s own
                // `parse_still_means_modbus_reference_notation` test), so
                // this arm is unreachable in practice - kept instead of
                // `unreachable!()` so a future change to `parse` degrades to
                // "cannot determine" rather than a panic on this
                // side-effect-free endpoint.
                None => Json(AddressWritableResponse {
                    writable: None,
                    area: None,
                }),
            },
            // Malformed or still-being-typed address text - cannot determine
            // yet, not "no restriction" (see this response type's doc
            // comment on why these two must not collapse into one value).
            Err(_) => Json(AddressWritableResponse {
                writable: None,
                area: None,
            }),
        },
        // SLMP devices have no Modbus-style permanently-read-only area, and
        // `virtual` (calc/mem) tags have no PLC address at all - neither
        // protocol has an area-based write restriction to report, so the
        // default may be applied.
        "slmp" | "virtual" => Json(AddressWritableResponse {
            writable: Some(true),
            area: None,
        }),
        // Unknown protocol (e.g. a future addition this handler has not
        // been taught about yet) - cannot determine, fail safe rather than
        // guess either way.
        _ => Json(AddressWritableResponse {
            writable: None,
            area: None,
        }),
    }
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
        &state.commissioning,
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
        &state.commissioning,
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
        &state.commissioning,
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
        &state.commissioning,
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
        &state.commissioning,
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
        &state.commissioning,
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
    commissioning: CommissioningState,
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
        &state.commissioning,
        &headers,
        "cancel",
        "pending_changes",
        &id.to_string(),
        None,
    )
    .await;
    Ok(Json(pending))
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
        &state.commissioning,
        &headers,
        "requeue",
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
        &state.commissioning,
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
    commissioning: CommissioningState,
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
        commissioning: commissioning.clone(),
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
            RoleGuard {
                auth: auth.clone(),
                commissioning: commissioning.clone(),
                min: Role::Admin,
                resource: "pending_changes",
                audit,
            },
            require_role_at_least,
        ))
        .layer(middleware::from_fn_with_state(
            AuthGate {
                auth,
                commissioning,
            },
            require_auth_or_commissioning,
        ))
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
        &state.commissioning,
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
                    &state.commissioning,
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

// --- T18-3b 一括更新 API (bulk tag operations): tags_batch (T11-1) の
// update-side 対（apps/banto-hub/core/src/rest.rs doc 冒頭ではなくこの
// セクション直下に配置 - tags_batch のリクエスト/レスポンス封筒・
// all-or-nothing・catalog commit 1回という骨格をそのまま流用する）。
// 用途: (1) 一括 enabled 切り替え、(2) 一括グループ移動
// （collectionGroupId 付け替え）。一括削除は対象外。

/// [`TagPayload`] に `id`（更新対象）を足しただけの一括更新1行分のペイロード。
/// `#[serde(flatten)]` で `TagPayload` の全フィールド（`expectedRevision`
/// 込み）をそのまま JSON 直下に展開する - 単票 PUT の body と全く同じ形
/// （`{ name, address, ..., expectedRevision }`）に `id` だけが乗る。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TagBatchUpdatePayload {
    pub id: i64,
    #[serde(flatten)]
    pub input: TagPayload,
}

impl From<TagBatchUpdatePayload> for (i64, TagInput) {
    fn from(payload: TagBatchUpdatePayload) -> Self {
        (payload.id, payload.input.into())
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchTagsUpdateRequest {
    pub tags: Vec<TagBatchUpdatePayload>,
    /// `true`: 検証結果だけを返す（DB 無変更）。`false`（既定）: 検証 →
    /// 単一トランザクションで全件 UPDATE → catalog commit を1回
    /// （`BatchTagsRequest::dry_run` と同じ意味）。
    #[serde(default)]
    pub dry_run: bool,
}

/// 行番号(0起点)付きのフィールドエラー一覧 - [`BatchTagRowErrorResponse`]
/// と同じ形だが、更新対象の行は既に `id` を持っているので併記する
/// （クライアントが `index` だけでなく `id` でも行を突き合わせられる）。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchTagUpdateRowErrorResponse {
    index: usize,
    id: i64,
    field_errors: Vec<BatchTagFieldErrorResponse>,
}

impl From<banto_tags::BatchTagUpdateError> for BatchTagUpdateRowErrorResponse {
    fn from(err: banto_tags::BatchTagUpdateError) -> Self {
        Self {
            index: err.index,
            id: err.id,
            field_errors: err.field_errors.into_iter().map(Into::into).collect(),
        }
    }
}

/// `POST /api/tags/batch-update` の応答。[`BatchTagsResponse`]（T11-1）と
/// 同じ「常に 200、`ok: false` で行ごとエラー」契約 - あちらの doc comment
/// 参照。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchTagsUpdateResponse {
    ok: bool,
    dry_run: bool,
    /// 適用された(または dry run で適用されたはずの)件数。`ok: false` の
    /// ときは常に 0。
    count: usize,
    errors: Vec<BatchTagUpdateRowErrorResponse>,
    /// `ok: true && dry_run: false` のときだけ `Some`(実際に更新されたタグ)。
    #[serde(skip_serializing_if = "Option::is_none")]
    tags: Option<Vec<Tag>>,
}

/// `POST /api/tags/batch-update` - T18-3b（bulk tag operations）。editor
/// 以上、`tags_batch`（T11-1）の骨格をそのまま写した update 版:
/// トランザクション内検証 → all-or-nothing 適用 → catalog commit 1回。
/// revision 競合（[`TagUpdateError::RevisionConflict`] が単票更新で返す
/// `409`）もここでは行単位エラーとして集約する - 一括操作の「どれか stale
/// なら全体 ok:false・無書込」という all-or-nothing 契約を、単票の
/// 「対象1行だけ 409 で弾く」より優先するため（design: T18-3b 一括操作）。
async fn tags_batch_update(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Json(body): Json<BatchTagsUpdateRequest>,
) -> RegistryMutationResult<Response> {
    require_editor(
        &state.auth,
        &state.commissioning,
        &state.audit,
        &headers,
        "tags",
        "POST",
        "/api/tags/batch-update",
    )
    .await?;

    if !body.dry_run {
        let status = state.controller.status();
        if status.state != CollectionState::Stopped {
            return queue_pending_registry_change(
                &state,
                &headers,
                "tags.batch_update",
                json!({ "dryRun": false, "tags": body.tags }),
                status,
            )
            .await;
        }
        require_collection_stopped(&state)?;
    }

    let dry_run = body.dry_run;
    let updates: Vec<(i64, TagInput)> = body.tags.into_iter().map(Into::into).collect();

    if updates.is_empty() {
        return Ok(Json(BatchTagsUpdateResponse {
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
    let outcome = match state.tags.update_batch_tx(&mut tx, &updates).await {
        Ok(outcome) => outcome,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(ApiError(err).into());
        }
    };
    match outcome {
        BatchTagUpdateOutcome::Invalid(errors) => {
            let _ = tx.rollback().await;
            Ok(Json(BatchTagsUpdateResponse {
                ok: false,
                dry_run,
                count: 0,
                errors: errors.into_iter().map(Into::into).collect(),
                tags: None,
            })
            .into_response())
        }
        BatchTagUpdateOutcome::Valid { count, tags } => {
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
                    &state.commissioning,
                    &headers,
                    "batch_update",
                    "tags",
                    "-",
                    Some(json!({ "count": count })),
                )
                .await;
                // T18-3b の核心 (tags_batch/T11-1 と同じ): n 件でも catalog
                // commit はここで1回だけ。
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
            Ok(Json(BatchTagsUpdateResponse {
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
    commissioning: CommissioningState,
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
        commissioning: commissioning.clone(),
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
        // T18-3b (bulk tag operations): 一括更新 - 同じ理由で `/api/tags`
        // 直下の固定パス（`/api/tags/batch` と衝突しない別セグメント）。
        .route("/api/tags/batch-update", post(tags_batch_update))
        // T18-5a 第2段 (docs/banto-hub-t18-design.md §4 決定6): `ListParams`
        // 素通しのページング付き一覧 - `/api/tags/batch`・`/api/tags/batch-update`
        // と同じ理由で `/api/tags/{id}` (i64 パラメータ) と衝突しない固定
        // セグメント (axum の matchit は静的セグメント優先 - 統合テストで
        // `/api/tags/{id}` 側の解決も併せて確認する)。
        .route("/api/tags/list", post(tags_list_query))
        // 同じく固定セグメント。グループ別タグ件数集計。
        .route("/api/tags/group-counts", get(tags_group_counts))
        // T19 S1-b0（2026-09-02）: 同じく固定セグメント。指定アドレスが
        // 書き込み可能な領域かどうかの判定（`tags_address_writable`の doc
        // comment 参照）。
        .route("/api/tags/address-writable", get(tags_address_writable))
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            AuthGate {
                auth,
                commissioning,
            },
            require_auth_or_commissioning,
        ))
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

/// `GET /api/v1/tags`・管理系 `GET /api/tag-catalog`（[`admin_tag_catalog`]、
/// 試運転モード対応・設計 §5.6・2026-08-31 オーナー決定「案A」の続き）が
/// 共有する本体。`api_key_request` が true の場合のみ
/// [`api_key_external_output_allowed`] でシミュレーション系タグを隠す
/// （機械クライアント向けの絞り込み、design §5.1）。管理 UI 側の呼び出し
/// （`admin_tag_catalog`、`api_key_request = false` 固定）は他の管理系
/// エンドポイントと同様、シミュレーション設定も含め全件を返す。
fn build_catalog_response(
    state: &TagSpaceState,
    query: &TagsQuery,
    api_key_request: bool,
) -> CatalogResponse {
    let map = state.manager.tag_map();
    let revision = state.manager.revision();
    let runtime = state.controller.status();
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
    CatalogResponse {
        revision,
        run_id: runtime.run_id,
        collection_mode: runtime.mode.as_str().to_string(),
        tags,
    }
}

/// `GET /api/v1/tags` ハンドラ本体 - catalog: `{ "revision", "run_id",
/// "collection_mode", "tags": [CatalogTagEntry...] }`,
/// optionally filtered by `?connection=`/`?group=` (matched against the
/// entry's connection/group *name*, design §5.1's route table). API-key
/// requests additionally omit simulation and derived-simulation entries.
/// ロジックは[`build_catalog_response`]側にあり、ここでは`ctx`から
/// `api_key_request`を判定して渡すだけ - 管理系の[`admin_tag_catalog`]は
/// 同じ関数を`api_key_request = false`固定で呼ぶ（二重管理を避けるための
/// 分離、`compute_status`/[`v1_status`]と同じ構成）。
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
    Json(build_catalog_response(&state, &query, ctx.is_some()))
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
    let runtime = state.controller.status();

    let names = match resolve_value_names(&map, &query) {
        Ok(names) => names,
        Err(unknown) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "unknown_tag", "tags": unknown })),
            )
                .into_response();
        }
    };

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

    Json(build_values_response(&state, &map, &runtime, names)).into_response()
}

/// `GET /api/v1/values`・管理系`GET /api/values`([`admin_values`])が共有
/// する`?tags=`解決: 省略時は全タグ、指定時はカンマ区切りをパースする。
/// 未知の外部名を1つでも含む場合は`Err(unknown)`（外部名のリスト、呼び出し
/// 元が400を組み立てる）を返す。API キーのper-tagスコープ判定（`ctx`）は
/// ここに含めない - 管理系にはその概念が無く、`v1_values`側だけが追加で
/// 適用する。
fn resolve_value_names(map: &TagMap, query: &ValuesQuery) -> Result<Vec<String>, Vec<String>> {
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
        let unknown: Vec<String> = names
            .iter()
            .filter(|name| map.get(name).is_none())
            .cloned()
            .collect();
        if !unknown.is_empty() {
            return Err(unknown);
        }
    }

    Ok(names)
}

/// 指定された外部名の集合から[`ValuesResponse`]を組み立てる -
/// `/api/v1/values`・管理系`/api/values`([`admin_values`])が共有する本体。
/// `map`/`runtime`は呼び出し元がAPIキーのper-tagスコープ絞り込み等を終えた
/// 後の値を渡す想定（1リクエスト内で複数回`tag_map()`/`controller.status()`
/// を呼んで状態がずれるのを避けるため、呼び出し元で1回だけ取得したものを
/// 共有する）。
fn build_values_response(
    state: &TagSpaceState,
    map: &TagMap,
    runtime: &CollectionStatus,
    names: Vec<String>,
) -> ValuesResponse {
    let now_ms = state.manager.clock().now_ms();
    let current = state.manager.current_values();
    let server_store = state.manager.server_store();

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
                runtime,
            )
        })
        .collect();

    ValuesResponse {
        revision: state.manager.revision(),
        t: now_ms,
        run_id: runtime.run_id,
        collection_mode: runtime.mode.as_str().to_string(),
        values,
    }
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

/// `GET /api/v1/status`・管理系 `GET /api/status`（2026-08-31 オーナー決定
/// 「案A」、`admin_status`参照）が共有する本体。`{ "version", "revision",
/// "last_config_error", "connections": [...] }` (design §5.1's route table)
/// を組み立てる。Connection names come from the registry directly (not the
/// catalog) so a connection with zero tags still appears.
///
/// T2-2/#131 (docs/tag-server-design.md §6-5, 2026-08-05 決定): a
/// broker-managed connection's status (per `banto_broker::is_supported_protocol`;
/// `"slmp"` and, as of #131, 2026-09-01, `"modbus-tcp"` too) comes from
/// [`crate::hub::CollectorManager::broker_status`] (the broker's own
/// `ConnState`) instead of `banto_collect`'s own status map - see
/// `crate::broker_glue`'s module doc ("The two-backoff double bookkeeping")
/// for why the broker's answer is the one that reflects whether the physical
/// session is actually up for a broker-managed connection. A connection
/// whose protocol the broker does NOT manage (any protocol string outside
/// that registered set) still falls back to reading from
/// `banto_collect::Collector::status` - unaffected by #131, since it was
/// never broker-routed to begin with.
async fn compute_status(state: &TagSpaceState) -> Result<StatusResponse, ApiError> {
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
            let (status_str, attempt) = if is_supported_protocol(&conn.protocol) {
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

    Ok(StatusResponse {
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
    })
}

/// `GET /api/v1/status` ハンドラ本体 - [`compute_status`]をそのまま JSON へ
/// 包むだけ。ロジックは`compute_status`側にあり、ここでは wire 形式
/// （snake_case、機械クライアント向け）を選ぶだけ - 管理系の
/// [`admin_status`]はカラムは同じ`compute_status`を呼び、camelCase の
/// [`AdminStatusResponse`]へ包み直す（二重管理を避けるための分離）。
#[utoipa::path(
    get,
    path = "/api/v1/status",
    responses((status = 200, description = "サーバー状態", body = StatusResponse)),
    tag = "tag-space",
)]
async fn v1_status(State(state): State<TagSpaceState>) -> Result<Json<StatusResponse>, ApiError> {
    Ok(Json(compute_status(&state).await?))
}

// --- 管理系 `GET /api/status`・`GET /api/values`・`GET /api/tag-catalog`・
// `GET /api/tag-stream`（設計 §5.6・2026-08-31 オーナー決定「案A」） --------
//
// 実機の試運転モードで判明した問題: 管理 UI（`hubStatus.ts`）が
// `GET /api/v1/status`・`GET /api/v1/values` を使っていたため、試運転モード
// （未ロックダウン・未ログイン・API キー未発行）中はどちらも401になり、
// 状態ページの「サーバー状態」「タグ現在値」が空になっていた
// （`hostSwitchGate.isPreflightOk`が`status.revision`を要求するため、
// Desktop↔Service 切替ウィザードまで連鎖的に塞がれる）。当初これを
// `/api/status`・`/api/values`の新設だけで解消したつもりだったが、
// **ライブタグモニタ（`tagMonitorAdmin.ts`）が別に`/api/v1/tags`（catalog）と
// `/api/v1/stream`（WS）を直接叩いている経路を見落としていた** -
// 同じ理由（`require_tag_space_auth`固定・試運転モードのバイパス対象外）で
// モニタの行が1つも出ない不具合が残っていた。`/api/tag-catalog`・
// `/api/tag-stream`はこの見落としの是正として同日に追加した。
//
// なぜ `/api/v1/*` 側を試運転モード対応にしないか: `/api/v1/*` は機械
// クライアント向けタグ空間 API で、認証は`require_tag_space_auth`
// （API キー or セッション bearer）固定 - 設計 §5.6 の決定で試運転モードの
// バイパス対象**外**（PLC 書き込み経路 `/api/v1/values/{tag}` と同じ境界を
// 守るため、機械クライアントの認証要件を試運転モードで緩めない）。
//
// 採用した方針: `/api/v1/*` のルート・認証・レスポンス形状は一切変えず、
// 管理系ルーター（試運転モードのバイパスが効き、ロックダウン後はセッション
// bearer が要る側）に別口のエンドポイントを追加する。ロジックは
// `compute_status`/`resolve_value_names`/`build_values_response`/
// `build_catalog_response`（上の`/api/v1/status`・`/api/v1/values`・
// `/api/v1/tags`ハンドラと**完全に同じ関数**）を呼ぶだけで、二重管理には
// していない。WS（`/api/tag-stream`）は関数どころかハンドラ自体
// （[`crate::stream::ws_upgrade`]）を`/api/v1/stream`とそのまま共有する -
// `admin_tag_stream_router`のdoc comment参照。
//
// 認可レベル: `RoleGuard`（admin 限定）は掛けない。理由:
// 1. `/api/v1/status`・`/api/v1/values`自体がそもそもロール制約の無い
//    読み取り専用エンドポイント（`tag_space_router`参照、`RoleGuard`は
//    一切登場しない）。管理系に持ち込むだけでロールを新設するのは
//    「同じ情報を管理 UI からも読めるようにする」というこの変更の趣旨から
//    外れる。
// 2. 管理系ルーターの既存の慣行でも、書き込み系（`write-control`・
//    `collection`等）は`RoleGuard{min: Role::Admin}`を掛ける一方、
//    読み取り専用の一覧系（`/api/tags`・`/api/plc-connections`・
//    `/api/pending-changes`の`GET`等、`tag_registry_router`/
//    `pending_changes_router`参照）は`require_auth_or_commissioning`のみ
//    （ロール不問 = viewer でも読める）。状態ページ・タグ現在値は
//    まさにこの「読み取り専用の一覧系」に分類され、viewer ロールの
//    利用者にも見えるべき情報（実際、状態ページは`canManageWriteControl`
//    等で書き込み操作のボタンだけを admin 限定にし、閲覧自体は誰でも
//    行える設計 - `(app)/status/+page.svelte`参照）。
//
// レスポンスの命名規則: 管理系 DTO（`CollectionStatusResponse`・
// `WriteControlStatusResponse`等）はすべて`#[serde(rename_all =
// "camelCase")]`。`/api/v1/*`側は意図して snake_case のまま
// （`hubStatus.ts`冒頭の doc comment参照）。このエンドポイントは管理系
// ルーターに属するので camelCase 側の規約に倣う - フィールド名に
// アンダースコアを含まない型（`MqttStatusEntry`・`GrpcStatusEntry`・
// `ValueEntry`）はそのまま再利用し（camelCase と snake_case で JSON が
// 一致するため二重定義しない）、アンダースコアを含む型だけ
// `Admin*`のcamelCase版を新設して`From`で変換する。

/// [`ConnectionStatusEntry`]のcamelCase版（`configuredSimulation`・
/// `effectiveSimulation`）。
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct AdminConnectionStatusEntry {
    name: String,
    id: i64,
    status: String,
    attempt: Option<u32>,
    simulation: bool,
    configured_simulation: bool,
    effective_simulation: bool,
}

impl From<ConnectionStatusEntry> for AdminConnectionStatusEntry {
    fn from(entry: ConnectionStatusEntry) -> Self {
        Self {
            name: entry.name,
            id: entry.id,
            status: entry.status,
            attempt: entry.attempt,
            simulation: entry.simulation,
            configured_simulation: entry.configured_simulation,
            effective_simulation: entry.effective_simulation,
        }
    }
}

/// [`TestOutputStatusEntry`]のcamelCase版（`runId`）。
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct AdminTestOutputStatusEntry {
    enabled: bool,
    run_id: Option<u64>,
}

impl From<TestOutputStatusEntry> for AdminTestOutputStatusEntry {
    fn from(entry: TestOutputStatusEntry) -> Self {
        Self {
            enabled: entry.enabled,
            run_id: entry.run_id,
        }
    }
}

/// [`LastApplyEntry`]のcamelCase版（`writerRotated`）。
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct AdminLastApplyEntry {
    added: Vec<String>,
    removed: Vec<String>,
    replaced: Vec<String>,
    unchanged: Vec<String>,
    writer_rotated: bool,
}

impl From<LastApplyEntry> for AdminLastApplyEntry {
    fn from(entry: LastApplyEntry) -> Self {
        Self {
            added: entry.added,
            removed: entry.removed,
            replaced: entry.replaced,
            unchanged: entry.unchanged,
            writer_rotated: entry.writer_rotated,
        }
    }
}

/// `GET /api/status`の応答 - [`StatusResponse`]（`/api/v1/status`）と
/// 完全に同じ情報をcamelCaseで運ぶ。`mqtt`/`grpc`は元の型
/// （`MqttStatusEntry`/`GrpcStatusEntry`）をそのまま再利用する -
/// フィールド名にアンダースコアが無くcamelCase化で形が変わらないため。
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct AdminStatusResponse {
    version: String,
    revision: u64,
    configured_revision: u64,
    running_revision: u64,
    run_id: Option<u64>,
    collection_state: String,
    collection_mode: String,
    last_runtime_error: Option<String>,
    last_config_error: Option<String>,
    connections: Vec<AdminConnectionStatusEntry>,
    write_enabled: bool,
    write_was_enabled_before_restart: bool,
    test_output: AdminTestOutputStatusEntry,
    mqtt: MqttStatusEntry,
    grpc: GrpcStatusEntry,
    last_apply: Option<AdminLastApplyEntry>,
}

impl From<StatusResponse> for AdminStatusResponse {
    fn from(status: StatusResponse) -> Self {
        Self {
            version: status.version,
            revision: status.revision,
            configured_revision: status.configured_revision,
            running_revision: status.running_revision,
            run_id: status.run_id,
            collection_state: status.collection_state,
            collection_mode: status.collection_mode,
            last_runtime_error: status.last_runtime_error,
            last_config_error: status.last_config_error,
            connections: status.connections.into_iter().map(Into::into).collect(),
            write_enabled: status.write_enabled,
            write_was_enabled_before_restart: status.write_was_enabled_before_restart,
            test_output: status.test_output.into(),
            mqtt: status.mqtt,
            grpc: status.grpc,
            last_apply: status.last_apply.map(Into::into),
        }
    }
}

/// `GET /api/status` - 管理 UI 向け（試運転モードのバイパス対象、
/// このセクション冒頭のdoc comment参照）。[`compute_status`]を
/// `/api/v1/status`と共有し、camelCaseへ包み直すだけ。
async fn admin_status(
    State(state): State<TagSpaceState>,
) -> Result<Json<AdminStatusResponse>, ApiError> {
    Ok(Json(compute_status(&state).await?.into()))
}

/// `GET /api/values`の応答 - [`ValuesResponse`]（`/api/v1/values`）と
/// 完全に同じ情報をcamelCaseで運ぶ。`values`は[`ValueEntry`]をそのまま
/// 再利用する（フィールド名にアンダースコアが無いため）。
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct AdminValuesResponse {
    revision: u64,
    t: i64,
    run_id: Option<u64>,
    collection_mode: String,
    values: Vec<ValueEntry>,
}

impl From<ValuesResponse> for AdminValuesResponse {
    fn from(resp: ValuesResponse) -> Self {
        Self {
            revision: resp.revision,
            t: resp.t,
            run_id: resp.run_id,
            collection_mode: resp.collection_mode,
            values: resp.values,
        }
    }
}

/// `GET /api/values` - 管理 UI 向け（試運転モードのバイパス対象、
/// このセクション冒頭のdoc comment参照）。`/api/v1/values`と違い API キー
/// の概念が無い（管理系ルーターにはセッション bearer/試運転の合成
/// identity しか来ない）ので、`ctx`によるper-tagスコープ絞り込みは行わず
/// 常に全件（`?tags=`指定時はその集合）を返す - `resolve_value_names`/
/// `build_values_response`は`v1_values`と共有する。
async fn admin_values(
    State(state): State<TagSpaceState>,
    Query(query): Query<ValuesQuery>,
) -> Response {
    let map = state.manager.tag_map();
    let runtime = state.controller.status();

    let names = match resolve_value_names(&map, &query) {
        Ok(names) => names,
        Err(unknown) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "unknown_tag", "tags": unknown })),
            )
                .into_response();
        }
    };

    Json(AdminValuesResponse::from(build_values_response(
        &state, &map, &runtime, names,
    )))
    .into_response()
}

/// [`CatalogTagEntry`]のcamelCase版（`GET /api/tag-catalog`用、
/// [`admin_tag_catalog`]参照）。`banto_tags`/`crate::hub::TagEntry`は
/// `/api/v1/*`向けに意図して snake_case のまま（同型の doc comment
/// 「Field names are plain snake_case on the wire」参照）なので、
/// `#[serde(flatten)]`に頼らずフィールドを手動で列挙してcamelCaseへ
/// 変換する（`AdminConnectionStatusEntry`等、このファイルの他の
/// `Admin*`camelCase版と同じ手法）。
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct AdminCatalogTagEntry {
    external_name: String,
    tag_key: String,
    #[schema(value_type = Vec<i64>)]
    ids: (i64, i64, i64),
    connection: String,
    group: String,
    name: String,
    address: String,
    data_type: String,
    unit: Option<String>,
    decimals: i64,
    period_ms: i64,
    enabled: bool,
    writable: bool,
    tag_kind: String,
    expression: Option<String>,
    retain: bool,
    simulation: bool,
    configured_simulation: bool,
    effective_simulation: bool,
    value_source: String,
}

impl From<CatalogTagEntry> for AdminCatalogTagEntry {
    fn from(entry: CatalogTagEntry) -> Self {
        Self {
            external_name: entry.entry.external_name,
            tag_key: entry.entry.tag_key,
            ids: entry.entry.ids,
            connection: entry.entry.connection,
            group: entry.entry.group,
            name: entry.entry.name,
            address: entry.entry.address,
            data_type: entry.entry.data_type,
            unit: entry.entry.unit,
            decimals: entry.entry.decimals,
            period_ms: entry.entry.period_ms,
            enabled: entry.entry.enabled,
            writable: entry.entry.writable,
            tag_kind: entry.entry.tag_kind,
            expression: entry.entry.expression,
            retain: entry.entry.retain,
            simulation: entry.entry.simulation,
            configured_simulation: entry.configured_simulation,
            effective_simulation: entry.effective_simulation,
            value_source: entry.value_source,
        }
    }
}

/// [`CatalogResponse`]のcamelCase版。
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct AdminCatalogResponse {
    revision: u64,
    run_id: Option<u64>,
    collection_mode: String,
    tags: Vec<AdminCatalogTagEntry>,
}

impl From<CatalogResponse> for AdminCatalogResponse {
    fn from(resp: CatalogResponse) -> Self {
        Self {
            revision: resp.revision,
            run_id: resp.run_id,
            collection_mode: resp.collection_mode,
            tags: resp.tags.into_iter().map(Into::into).collect(),
        }
    }
}

/// `GET /api/tag-catalog` - 管理 UI 向け（試運転モードのバイパス対象、
/// このセクション冒頭のdoc comment参照）。ライブタグモニタ
/// （`tagMonitorAdmin.ts`）が本来必要としていたのはこれ - 元々は
/// `/api/v1/tags`を直接叩いていたため、試運転モード中は行が1つも表示され
/// ない不具合の原因だった。[`build_catalog_response`]を`/api/v1/tags`
/// （[`v1_tags`]）と共有し、`api_key_request = false`固定（管理系ルーター
/// にAPIキーの概念は無い）で呼ぶ - シミュレーション設定を含め全件を返す
/// （[`admin_values`]と同じ判断）。
async fn admin_tag_catalog(
    State(state): State<TagSpaceState>,
    Query(query): Query<TagsQuery>,
) -> Json<AdminCatalogResponse> {
    Json(build_catalog_response(&state, &query, false).into())
}

/// [`admin_status`]・[`admin_values`]・[`admin_tag_catalog`]用ルーター -
/// 管理系（試運転モードのバイパスが効く側）に配置する。`RoleGuard`は
/// 掛けない理由はこのセクション冒頭のdoc comment参照（読み取り専用・
/// ロール不問、`tag_registry_router`の`GET`系と同じ扱い）。状態は
/// [`TagSpaceState`]を[`tag_space_router`]とは別に組み立てる - こちらは
/// `require_auth_or_commissioning`層を被せるため、`require_tag_space_auth`
/// 層を被せる`tag_space_router`側の`Router`とは共有できない（axum の
/// `Router`は1つにつき1枚の認証`.layer`しか意味を持たないため、同じ
/// `Router`を2種類の認証で使い回すことはできない）。
///
/// **WS（`/api/tag-stream`）はここに同居させない** -
/// [`admin_tag_stream_router`]のdoc comment参照（この Router 全体は最終的に
/// `require_banto_client_header`(CSRF) レイヤーの内側に組み込まれるが、
/// ブラウザの`WebSocket`はCSRF用カスタムヘッダを送れないため）。
fn admin_status_router(
    manager: Arc<CollectorManager>,
    controller: Arc<CollectionController>,
    write_control: Arc<WriteControl>,
    test_output: Arc<TestOutputControl>,
    mqtt: Arc<MqttPublisher>,
    auth: AuthState,
    commissioning: CommissioningState,
) -> Router {
    let state = TagSpaceState {
        manager,
        controller,
        write_control,
        test_output,
        mqtt,
    };
    Router::new()
        .route("/api/status", get(admin_status))
        .route("/api/values", get(admin_values))
        .route("/api/tag-catalog", get(admin_tag_catalog))
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            AuthGate {
                auth,
                commissioning,
            },
            require_auth_or_commissioning,
        ))
}

/// `GET /api/tag-stream`（管理系 WS、[`admin_tag_stream_router`]）のパス -
/// [`extract_ws_protocol_token`]の許可リストと[`admin_tag_stream_router`]の
/// 両方から参照する定数にして、パス文字列のタイプミスで両者がずれるのを
/// 防ぐ。
const ADMIN_TAG_STREAM_PATH: &str = "/api/tag-stream";

/// [`crate::stream::ws_upgrade`]（`/api/v1/stream`と全く同じハンドラ関数）
/// 用の管理系ルーター - 試運転モード対応（設計 §5.6・2026-08-31 オーナー
/// 決定）で新設。ライブタグモニタ（`tagMonitorAdmin.ts`）の購読先を
/// 試運転モードでも繋がるようにするための追加（このセクション冒頭の
/// doc comment参照 - `/api/tag-catalog`と対になる存在）。
///
/// ## なぜ`admin_status_router`に同居させないか（CSRF とブラウザ WS の制約）
///
/// [`api_router_with_controller_mode`]が組み立てる`admin`ルーター一式は
/// 最後に`require_banto_client_header`（CSRF、`X-Banto-Client`ヘッダ必須）
/// を1枚被せる。ところがブラウザの`WebSocket`コンストラクタは（この
/// ファイル冒頭のモジュール doc comment、および
/// `extract_ws_protocol_token`のdoc comment が`Authorization`について
/// 説明しているのと全く同じ理由で）カスタムヘッダを一切送れない -
/// `X-Banto-Client`もその例外ではない。もし`/api/tag-stream`を
/// `admin_status_router`に同居させて`admin`側のCSRFレイヤーの内側に
/// 置いてしまうと、実ブラウザからは**認証の成否に関わらず**常に403に
/// なり、管理 UI から絶対に接続できなくなる。
///
/// そのため、このルーターは単独で構築し、`tag_space_router`
/// （`/api/v1/*`、同じ理由でCSRF対象外 - このファイル冒頭のモジュール
/// doc comment参照）と同様に`admin`へCSRFレイヤーを被せた**後**に
/// `.merge()`する（[`api_router_with_controller_mode`]参照）。CSRFを
/// 要求しない代わりに、認証自体は[`require_auth_or_commissioning`]で
/// 別途担保する（ロックダウン済みなら有効なセッション bearer が必須 -
/// CSRFの有無に関わらず未認証アクセスは401になる）ので、保護水準は
/// 落ちていない。
///
/// ## 認証: ロックダウン済みでの Sec-WebSocket-Protocol フォールバック
///
/// ブラウザは`Authorization`ヘッダも送れないので、ロックダウン済み状態
/// （通常ログイン後）でこのWSに繋ぐには何らかの代替経路が要る。
/// `/api/v1/stream`が使っているのと**全く同じ仕組み**
/// （`Sec-WebSocket-Protocol: bearer, <token>`、`extract_ws_protocol_token`
/// 参照）を[`require_auth_or_commissioning`]側にも追加した - パスの許可
/// リストに[`ADMIN_TAG_STREAM_PATH`]を足しただけで、`/api/v1/stream`の
/// 認証・ルート・レスポンス形状は一切変えていない（`require_tag_space_auth`
/// は無変更）。試運転モード中（未ロックダウン）は
/// [`require_auth_or_commissioning`]がヘッダの中身を見る前に無条件で
/// 素通しするので、トークン（Sec-WebSocket-Protocolオファーそのもの）が
/// 無くても接続できる。
///
/// ## `scope`（per-tag read スコープ）は常に`None`
///
/// [`crate::stream::ws_upgrade`]は`ApiKeyContext`拡張が無ければ`scope`を
/// `None`として扱う（=購読の絞り込み無し、全アクセス）。管理系ルーターは
/// `require_auth_or_commissioning`しか通らず`ApiKeyContext`を挿入する
/// ことは無いので、この管理系WSは常に`scope = None`側の経路を通る -
/// これは「セッション bearer で`/api/v1/stream`に繋いだ場合」と全く同じ
/// 挙動（`crate::stream::handle_socket`のフィールド doc comment
/// 「`external = scope.is_some()`」参照）で、管理 UI が今まで手にしていた
/// アクセス範囲を狭めても広げてもいない。
fn admin_tag_stream_router(
    manager: Arc<CollectorManager>,
    controller: Arc<CollectionController>,
    write_control: Arc<WriteControl>,
    test_output: Arc<TestOutputControl>,
    mqtt: Arc<MqttPublisher>,
    auth: AuthState,
    commissioning: CommissioningState,
) -> Router {
    let state = TagSpaceState {
        manager,
        controller,
        write_control,
        test_output,
        mqtt,
    };
    Router::new()
        .route(ADMIN_TAG_STREAM_PATH, get(crate::stream::ws_upgrade))
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            AuthGate {
                auth,
                commissioning,
            },
            require_auth_or_commissioning,
        ))
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

// --- 試運転モード/ロックダウン (設計 §5.6・2026-08-30 オーナー決定) --------
//
// `GET /api/commissioning/status`: 現在ロックダウン済みかどうか。試運転
// モード中は管理 UI がまだログインできない（ログインという概念自体が
// バイパスされている）ため、**この読み取りだけは`require_auth_or_commissioning`
// の対象外にして常に未認証で叩けるようにする** - 実装指示「未認証でも
// 取得できる必要がある」のとおり。読み取り専用のため監査エントリは
// 記録しない（`crate::audit`のモジュール doc「read routes are never
// audited」と同じ規約）。
//
// `POST /api/commissioning/lock-down`: 試運転モード → ロックダウン済みへの
// 唯一の正方向遷移（`CommissioningService::lock_down`）。他の admin
// エンドポイントと同じ`RoleGuard`（admin ちょうど）+
// `require_auth_or_commissioning`を掛ける - 試運転モード中はその
// ガード自体が素通しになるので実質誰でも叩けるが、ロックダウン済みに
// なった後は admin セッションが無いと叩けなくなる（＝ロックダウン後に
// 再度ロックダウンし直すことはできるが admin 権限が要る、という自然な
// 挙動）。
//
// 試運転モードへ戻す REST エンドポイントは意図的に存在しない
// （`crate::commissioning`のモジュール doc「REST では絶対に公開しない
// こと」参照 - 設計 §5.6「UI・REST からは解除できない」・
// `banto-hub-elev.exe`経由限定）。

#[derive(Clone)]
struct CommissioningAdminState {
    commissioning: CommissioningService,
    auth: AuthState,
    audit: AuditLogService,
}

async fn commissioning_status(
    State(state): State<CommissioningAdminState>,
) -> Json<crate::commissioning::CommissioningStatus> {
    Json(crate::commissioning::CommissioningStatus {
        locked_down: state.commissioning.is_locked_down(),
    })
}

async fn commissioning_lock_down(
    State(state): State<CommissioningAdminState>,
    headers: HeaderMap,
) -> Result<Json<crate::commissioning::CommissioningStatus>, ApiError> {
    state.commissioning.lock_down().await?;
    record_write(
        &state.audit,
        &state.auth,
        &state.commissioning.state(),
        &headers,
        "lock_down",
        "commissioning",
        "1",
        None,
    )
    .await;
    Ok(Json(crate::commissioning::CommissioningStatus {
        locked_down: true,
    }))
}

fn commissioning_router(
    commissioning: CommissioningService,
    audit: AuditLogService,
    auth: AuthState,
) -> Router {
    let state = CommissioningAdminState {
        commissioning: commissioning.clone(),
        auth: auth.clone(),
        audit: audit.clone(),
    };
    let commissioning_state = commissioning.state();

    // status は未認証で読める必要がある（設計 §5.6）ので、他の admin
    // ルーターと違い `require_auth_or_commissioning`/`RoleGuard` を一切
    // 掛けない - `require_banto_client_header`（CSRF、`admin`ルーター全体に
    // 掛かる）だけは他の admin エンドポイントと同様に適用される
    // （`X-Banto-Client`ヘッダはログイン資格情報ではなく「自前のフロント
    // エンドから来た」ことを示すだけなので、未ログインの `GET
    // /api/auth/status`等と同じく問題ない - このファイル冒頭の
    // 「二系統に分かれたルーター」節参照）。
    let status_route = Router::new()
        .route("/api/commissioning/status", get(commissioning_status))
        .with_state(state.clone());

    let lock_down_route = Router::new()
        .route(
            "/api/commissioning/lock-down",
            post(commissioning_lock_down),
        )
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            RoleGuard {
                auth: auth.clone(),
                commissioning: commissioning_state.clone(),
                min: Role::Admin,
                resource: "commissioning",
                audit,
            },
            require_role_at_least,
        ))
        .layer(middleware::from_fn_with_state(
            AuthGate {
                auth,
                commissioning: commissioning_state,
            },
            require_auth_or_commissioning,
        ));

    status_route.merge(lock_down_route)
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
    // 試運転モードとロックダウン（設計 §5.6・2026-08-30 オーナー決定）:
    // `crate::runtime::HubRuntime::start`が起動時に一度だけ`CommissioningService::load`
    // した結果をここへ注入する（`ApiKeysService`等、他サービスと同じ規約）。
    // ここではフル `CommissioningService`（`commissioning_router`の
    // `lock_down`が使う）を受け取り、他の全 admin ルーターへは軽量な
    // `CommissioningState`ハンドル（`.state()`）だけを配る。
    commissioning: CommissioningService,
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
    let commissioning_state = commissioning.state();

    let audited_auth_routes = auth_routes(auth.clone()).layer(middleware::from_fn_with_state(
        LogoutAuditState {
            auth: auth.clone(),
            commissioning: commissioning_state.clone(),
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
        .merge(commissioning_router(
            commissioning,
            audit.clone(),
            auth.clone(),
        ))
        .merge(users_router(
            users,
            audit.clone(),
            auth.clone(),
            commissioning_state.clone(),
        ))
        .merge(audit_log_router(
            audit.clone(),
            auth.clone(),
            commissioning_state.clone(),
            manager.clone(),
        ))
        .merge(api_keys_router(
            api_keys.clone(),
            audit.clone(),
            auth.clone(),
            commissioning_state.clone(),
            manager.clone(),
        ))
        .merge(tag_registry_router(
            plc_connections.clone(),
            collection_groups.clone(),
            tags.clone(),
            pending_changes.clone(),
            audit.clone(),
            auth.clone(),
            commissioning_state.clone(),
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
            commissioning_state.clone(),
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
            commissioning_state.clone(),
            events.clone(),
        ))
        .merge(test_output_router(
            test_output.clone(),
            controller.clone(),
            audit.clone(),
            auth.clone(),
            commissioning_state.clone(),
            events.clone(),
        ))
        .merge(collection_control_router(
            controller.clone(),
            manager.clone(),
            audit.clone(),
            auth.clone(),
            commissioning_state.clone(),
            events.clone(),
        ))
        .merge(write_audit_router(
            write_audit.clone(),
            audit.clone(),
            auth.clone(),
            commissioning_state.clone(),
        ))
        .merge(mqtt_settings_router(
            manager.clone(),
            mqtt.clone(),
            audit.clone(),
            auth.clone(),
            commissioning_state.clone(),
            events.clone(),
        ))
        .merge(grpc_settings_router(
            manager.clone(),
            grpc_server,
            audit.clone(),
            auth.clone(),
            commissioning_state.clone(),
            events.clone(),
        ))
        // 試運転モード対応（設計 §5.6・2026-08-31 オーナー決定「案A」）:
        // `GET /api/status`・`GET /api/values`・`GET /api/tag-catalog` -
        // `/api/v1/status`・`/api/v1/values`・`/api/v1/tags`と同じ情報を
        // 管理系（試運転モードのバイパスが効く側）から読めるようにする。
        // `admin_status_router`のdoc comment参照。ここで渡す
        // `manager`/`controller`/`write_control`/`test_output`/`mqtt`の各
        // `Arc`は、下の`tag_space_router`/`admin_tag_stream_router`へ渡す
        // ものと**同じ**インスタンスの`clone()` - 別インスタンスを作ると
        // 状態が分裂する（このファイルの他の`Arc`共有規律と同じ）。
        .merge(admin_status_router(
            manager.clone(),
            controller.clone(),
            write_control.clone(),
            test_output.clone(),
            mqtt.clone(),
            auth.clone(),
            commissioning_state.clone(),
        ))
        .layer(middleware::from_fn(require_banto_client_header));

    admin
        // `admin_tag_stream_router`（`/api/tag-stream`）は意図的に上の
        // `admin`（CSRF レイヤー適用済み）へ`tag_space_router`と同じ形で
        // `.merge()`する - CSRF レイヤーの**外側**に置く必要がある理由は
        // `admin_tag_stream_router`のdoc comment参照（ブラウザの
        // `WebSocket`はCSRF用カスタムヘッダを送れない）。
        .merge(admin_tag_stream_router(
            manager.clone(),
            controller.clone(),
            write_control.clone(),
            test_output.clone(),
            mqtt.clone(),
            auth.clone(),
            commissioning_state,
        ))
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
    // 試運転モードとロックダウン（設計 §5.6）: `api_router_with_controller_mode`
    // のフィールド doc comment参照。
    commissioning: CommissioningService,
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
        commissioning,
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
    // 試運転モードとロックダウン（設計 §5.6）: `api_router_with_controller_mode`
    // のフィールド doc comment参照。
    commissioning: CommissioningService,
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
        commissioning,
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

    /// 試運転モード（設計 §5.6・2026-08-30 オーナー決定）のテスト用: [`test_env`]
    /// と同じ環境を、明示的に**ロックダウンしないまま**（＝試運転モードの
    /// まま）返す - `commissioning_mode_tests`（このモジュール下部）専用。
    async fn test_env_unlocked() -> TestEnv {
        test_env_with_clock_and_lock(Arc::new(SystemClock), false).await
    }

    /// [`test_env`] but with an injectable clock (H10 ①) - lets a test
    /// create a key with `expiresAt = clock.now_ms() + small`, assert it
    /// authenticates, then `advance_ms` past the deadline and assert 401 -
    /// deterministically, without depending on real wall-clock time. See
    /// [`test_manager_with_clock`]'s doc comment for the same reasoning.
    /// Always locked down - see [`test_env_with_clock_and_lock`].
    async fn test_env_with_clock(clock: Arc<dyn Clock>) -> TestEnv {
        test_env_with_clock_and_lock(clock, true).await
    }

    /// [`test_env_with_clock`]で共有している実体。`locked_down: false`は
    /// [`test_env_unlocked`]専用（試運転モードのテスト） - このファイルの
    /// 大半の既存テストは「ロックダウン済み」の挙動そのものを検証している
    /// ため、`test_env_with_clock`自体は常に`true`を渡して既存の意味論を
    /// 一切変えない（実装指示「既存の認証・監査の挙動を、ロックダウン済み
    /// 状態では一切変えないこと」）。
    async fn test_env_with_clock_and_lock(clock: Arc<dyn Clock>, locked_down: bool) -> TestEnv {
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

        // 試運転モードとロックダウン（設計 §5.6・2026-08-30 オーナー決定）:
        // このファイルの大半の既存テストは「ロックダウン済み（従来どおり
        // bearer セッションのログイン必須）」の挙動そのものを検証している
        // ため、共有テスト環境は明示的にロックダウンしてから router を
        // 組み立てる - こうしないと未ロックダウン（試運転モード）の
        // バイパスが効いてしまい、`admin_token`/`viewer_token`を使わない
        // リクエストまで通ってしまう（実装指示「既存の認証・監査の挙動を、
        // ロックダウン済み状態では一切変えないこと」）。試運転モード側の
        // 挙動は`commissioning_mode_tests`（このモジュール下部）で別途
        // 専用のテスト環境を組んで検証する。
        let settings = SettingsService::new(pool.clone());
        let commissioning = CommissioningService::load(settings, users.clone())
            .await
            .expect("CommissioningService::load");
        if locked_down {
            commissioning
                .lock_down()
                .await
                .expect("lock_down the shared test environment");
        }

        let router = api_router(
            users,
            audit,
            plc_connections,
            collection_groups,
            tags,
            api_keys.clone(),
            manager,
            auth,
            commissioning,
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

    /// TAG-P0-3 follow-up（2026-08-14）: failed 状態の提案を requeue すると
    /// pending へ戻り、failure_reason はクリアされる。
    #[tokio::test]
    async fn pending_changes_requeue_endpoint_returns_pending_state() {
        let env = test_env().await;

        let (status, conn) = admin_post(
            &env.router,
            "/api/plc-connections",
            &env.admin_token,
            json!({ "name": "line-requeue-basic", "host": "127.0.0.1", "port": 15034 }),
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
                            "name": "temp-requeue-basic-01",
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

        // 収集稼働中に apply → 409 collection_edit_locked で failed へ遷移
        // （pending_changes_apply_while_running_returns_409_and_keeps_queue_row
        // と同じ経路で failed 行を用意する）。
        let apply_while_running = env
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
        assert_eq!(apply_while_running.status(), StatusCode::CONFLICT);

        let pending_changes = PendingChangesService::new(env.pool.clone());
        let failed = pending_changes.get(pending_id).await.unwrap();
        assert_eq!(failed.state, PendingChangeState::Failed);

        let response = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post(format!("/api/pending-changes/{pending_id}/requeue"))
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
        assert_eq!(body["state"], "pending");
        assert!(body["failureReason"].is_null());

        let requeued = pending_changes.get(pending_id).await.unwrap();
        assert_eq!(requeued.state, PendingChangeState::Pending);
    }

    /// TAG-P0-3 follow-up（2026-08-14）: 一過性の失敗（収集稼働中の 409）は
    /// requeue → 収集停止 → 再 apply で回復できることを確認する。
    #[tokio::test]
    async fn pending_changes_requeue_then_apply_succeeds_after_transient_failure_clears() {
        let env = test_env().await;

        let (status, conn) = admin_post(
            &env.router,
            "/api/plc-connections",
            &env.admin_token,
            json!({ "name": "line-requeue-transient", "host": "127.0.0.1", "port": 15035 }),
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
                            "name": "temp-requeue-transient-01",
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

        let apply_while_running = env
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
        assert_eq!(apply_while_running.status(), StatusCode::CONFLICT);

        let pending_changes = PendingChangesService::new(env.pool.clone());
        let failed = pending_changes.get(pending_id).await.unwrap();
        assert_eq!(failed.state, PendingChangeState::Failed);

        let requeue_response = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post(format!("/api/pending-changes/{pending_id}/requeue"))
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(requeue_response.status(), StatusCode::OK);
        let requeue_bytes = axum::body::to_bytes(requeue_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let requeue_body: serde_json::Value = serde_json::from_slice(&requeue_bytes).unwrap();
        assert_eq!(requeue_body["state"], "pending");

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

        let reapply = env
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
        assert_eq!(reapply.status(), StatusCode::OK);
        let reapply_bytes = axum::body::to_bytes(reapply.into_body(), usize::MAX)
            .await
            .unwrap();
        let reapply_body: serde_json::Value = serde_json::from_slice(&reapply_bytes).unwrap();
        assert_eq!(reapply_body["state"], "applied");

        let applied = pending_changes.get(pending_id).await.unwrap();
        assert_eq!(applied.state, PendingChangeState::Applied);
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

    /// 実機で再現した不具合の修正2（2026-08-31 オーナー報告）: pending change
    /// の適用が validation エラー（名前の重複）で失敗したとき、
    /// `failure_reason` にフィールド単位の詳細（`name: 既に使用されています`）
    /// が含まれることを確認する。修正前は `BantoError` の `Display`
    /// （`thiserror`）が種別ごとの定型文だけ（`Validation` は常に
    /// `"validation failed"`）だったため、`pending change の適用に失敗
    /// しました: validation failed` としか出ず、何が失敗したのか分からな
    /// かった - オーナーが収集稼働中に同じ名前（`group1`）で収集グループを
    /// 3回作成し、3回とも適用が全滅した際にこれで気づけなかった。
    #[tokio::test]
    async fn pending_apply_collection_group_create_failure_reason_includes_field_detail() {
        let env = test_env().await;

        let (status, conn) = admin_post(
            &env.router,
            "/api/plc-connections",
            &env.admin_token,
            json!({ "name": "line-dup-name-detail", "host": "127.0.0.1", "port": 15042 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{conn:?}");
        let conn_id = conn["id"].as_i64().unwrap();

        // 先に "group1" を普通に（収集停止中に）作成しておく - 既存レコード
        // として DB に存在する状態を作る。
        let (status, existing) = admin_post(
            &env.router,
            "/api/collection-groups",
            &env.admin_token,
            json!({ "name": "group1", "plcConnectionId": conn_id, "periodMs": 100 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{existing:?}");

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

        // 収集稼働中に同じ名前 "group1" で再度作成 → オーナーが実機で再現した
        // 状況そのもの: 202 でキューに入るだけで、DB には現れない。
        let queued = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post("/api/collection-groups")
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({ "name": "group1", "plcConnectionId": conn_id, "periodMs": 100 })
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

        // 適用すると "name" が重複しているため validation エラーで拒否される。
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
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let pending = PendingChangesService::new(env.pool.clone())
            .get(pending_id)
            .await
            .unwrap();
        assert_eq!(pending.state, PendingChangeState::Failed);
        let failure_reason = pending
            .failure_reason
            .expect("failure_reason should be set");
        assert_ne!(
            failure_reason, "pending change の適用に失敗しました: validation failed",
            "failure_reason should include field-level detail, not just the generic BantoError Display: {failure_reason}"
        );
        assert!(
            failure_reason.contains("name") && failure_reason.contains("既に使用されています"),
            "failure_reason should say which field failed and why: {failure_reason}"
        );
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

    /// TAG-P0-3 follow-up（2026-08-14）: requeue は fingerprint/payload を
    /// 一切変更しないため、真のコンフリクト（enqueue 後に対象行が別経路で
    /// 変わっている場合）は requeue → 再 apply でも安全に再度 fail し、
    /// 対象行を上書きしないことを確認する。
    #[tokio::test]
    async fn pending_changes_requeue_conflict_still_fails_on_reapply_with_fingerprint_intact() {
        let env = test_env().await;

        let (status, conn) = admin_post(
            &env.router,
            "/api/plc-connections",
            &env.admin_token,
            json!({ "name": "line-fp-conflict-requeue", "host": "127.0.0.1", "port": 15033 }),
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
                        json!({ "name": "line-fp-conflict-requeue-queued", "host": "127.0.0.1", "port": 15033 })
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

        // 別経路（pending queue を経由しない直接の service 呼び出し）で
        // 同じ行を書き換え、真のコンフリクトを作る。
        PlcConnectionService::new(env.pool.clone())
            .update(
                conn_id,
                PlcConnectionInput {
                    name: "line-fp-conflict-requeue-hijacked".to_string(),
                    protocol: "modbus-tcp".to_string(),
                    host: "127.0.0.1".to_string(),
                    port: 15033,
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

        let first_apply = env
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
        assert_eq!(first_apply.status(), StatusCode::CONFLICT);
        let first_apply_bytes = axum::body::to_bytes(first_apply.into_body(), usize::MAX)
            .await
            .unwrap();
        let first_apply_body: serde_json::Value =
            serde_json::from_slice(&first_apply_bytes).unwrap();
        assert_eq!(first_apply_body["error"], "pending_apply_conflict");
        assert_eq!(first_apply_body["resource"], "plc_connections");

        let pending_changes = PendingChangesService::new(env.pool.clone());
        let failed = pending_changes.get(pending_id).await.unwrap();
        assert_eq!(failed.state, PendingChangeState::Failed);

        // requeue は成功する（failed -> pending への差し戻しは常に許可）。
        let requeue_response = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post(format!("/api/pending-changes/{pending_id}/requeue"))
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(requeue_response.status(), StatusCode::OK);
        let requeue_bytes = axum::body::to_bytes(requeue_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let requeue_body: serde_json::Value = serde_json::from_slice(&requeue_bytes).unwrap();
        assert_eq!(requeue_body["state"], "pending");

        // しかし fingerprint/payload は据え置きのままなので、コンフリクトが
        // 解消されていない限り再 apply も同じ理由で再び fail する。
        let second_apply = env
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
        assert_eq!(second_apply.status(), StatusCode::CONFLICT);
        let second_apply_bytes = axum::body::to_bytes(second_apply.into_body(), usize::MAX)
            .await
            .unwrap();
        let second_apply_body: serde_json::Value =
            serde_json::from_slice(&second_apply_bytes).unwrap();
        assert_eq!(second_apply_body["error"], "pending_apply_conflict");
        assert_eq!(second_apply_body["resource"], "plc_connections");

        let failed_again = pending_changes.get(pending_id).await.unwrap();
        assert_eq!(failed_again.state, PendingChangeState::Failed);

        // 対象行は「別経路」の編集内容のまま(再 apply も拒否されたので
        // 上書きされていない)。
        let untouched = PlcConnectionService::new(env.pool.clone())
            .get(conn_id)
            .await
            .unwrap();
        assert_eq!(untouched.name, "line-fp-conflict-requeue-hijacked");
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

    // --- T18-5a 第2段 (docs/banto-hub-t18-design.md §4 決定6): 薄い部品の
    // 先行配線 - POST /api/tags/list と GET /api/tags/group-counts --------

    /// `admin_post` 経由で PLC接続 → 収集グループ → タグ を1本作る。
    /// `group_id` が `None` なら新しい接続+グループも作る、`Some` ならその
    /// グループへタグを追加するだけ - 返り値は `(tag_id, group_id)`。
    async fn create_tag_via_admin(
        router: &Router,
        token: &str,
        suffix: &str,
        group_id: Option<i64>,
    ) -> (i64, i64) {
        let group_id = match group_id {
            Some(id) => id,
            None => {
                let (status, conn) = admin_post(
                    router,
                    "/api/plc-connections",
                    token,
                    json!({ "name": format!("conn-{suffix}"), "host": "127.0.0.1", "port": 15300 }),
                )
                .await;
                assert_eq!(status, StatusCode::OK, "{conn:?}");
                let conn_id = conn["id"].as_i64().unwrap();
                let (status, group) = admin_post(
                    router,
                    "/api/collection-groups",
                    token,
                    json!({
                        "name": format!("group-{suffix}"),
                        "plcConnectionId": conn_id,
                        "periodMs": 1000
                    }),
                )
                .await;
                assert_eq!(status, StatusCode::OK, "{group:?}");
                group["id"].as_i64().unwrap()
            }
        };
        let (status, tag) = admin_post(
            router,
            "/api/tags",
            token,
            json!({
                "name": format!("tag-{suffix}"),
                "collectionGroupId": group_id,
                "address": "D100",
                "dataType": "i16"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{tag:?}");
        (tag["id"].as_i64().unwrap(), group_id)
    }

    /// (a) 認証なし → 401（CSRF ヘッダは付けている - `admin_routes_require_the_csrf_header`
    /// が CSRF 側は別途カバー済みなので、ここでは認証だけを外す）。
    #[tokio::test]
    async fn tags_list_query_requires_auth() {
        let env = test_env().await;
        let response = env
            .router
            .oneshot(
                HttpRequest::post("/api/tags/list")
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&json!({})).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    /// (b) filter + sort + pagination + totalCount が `TagService::list` に
    /// そのまま素通しされること、viewer でも読めること（`GET /api/tags` と
    /// 同じ require_auth のみ）、(c) `/api/tags/{id}` が `/api/tags/list` と
    /// 衝突せず両方とも正しいハンドラへ解決されることを1本で確認する。
    #[tokio::test]
    async fn tags_list_query_filters_paginates_and_resolves_beside_tags_id() {
        let env = test_env().await;
        let (tag_a, group_id) =
            create_tag_via_admin(&env.router, &env.admin_token, "a", None).await;
        let (tag_b, _) =
            create_tag_via_admin(&env.router, &env.admin_token, "b", Some(group_id)).await;
        let (_tag_c, _) =
            create_tag_via_admin(&env.router, &env.admin_token, "c", Some(group_id)).await;

        let (status, body) = admin_post(
            &env.router,
            "/api/tags/list",
            &env.admin_token,
            json!({
                "filters": [{ "field": "collectionGroupId", "op": "eq", "value": group_id }],
                "sort": [{ "field": "name", "direction": "asc" }],
                "pagination": { "offset": 0, "limit": 2 }
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(body["totalCount"], 3);
        let rows = body["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["name"], "tag-a");
        assert_eq!(rows[1]["name"], "tag-b");

        // viewer（読み取り専用）でも読める - `require_editor` は呼ばない。
        let (status, viewer_body) =
            admin_post(&env.router, "/api/tags/list", &env.viewer_token, json!({})).await;
        assert_eq!(status, StatusCode::OK, "{viewer_body:?}");
        assert_eq!(viewer_body["totalCount"], 3);

        // (c) `/api/tags/{id}` 側も同じルーターで正しく解決される
        // (axum の matchit は静的セグメント `/api/tags/list` を優先するため
        // `{id}: i64` に飲み込まれない - 逆にここでは数値 id が
        // `/api/tags/{id}` へ正しく届くことを確認する)。
        let response = env
            .router
            .clone()
            .oneshot(
                HttpRequest::get(format!("/api/tags/{tag_b}"))
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
        let single: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(single["id"], tag_b);
        assert_eq!(single["name"], "tag-b");

        let _ = tag_a;
    }

    /// (d) `GET /api/tags/group-counts` がグループ別のタグ件数を返すこと。
    /// 200 が返ること自体が (c) の「`/api/tags/{id}` (`{id}: i64`) に
    /// 飲み込まれていない」証拠でもある - 飲み込まれていれば `group-counts`
    /// は不正な i64 として 400 になる。
    #[tokio::test]
    async fn tags_group_counts_returns_per_group_totals() {
        let env = test_env().await;
        let (_tag_a1, group1) =
            create_tag_via_admin(&env.router, &env.admin_token, "ga1", None).await;
        create_tag_via_admin(&env.router, &env.admin_token, "ga2", Some(group1)).await;
        let (_tag_b1, group2) =
            create_tag_via_admin(&env.router, &env.admin_token, "gb1", None).await;

        let response = env
            .router
            .clone()
            .oneshot(
                HttpRequest::get("/api/tags/group-counts")
                    .header("Authorization", format!("Bearer {}", env.viewer_token))
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
        let counts = body.as_array().unwrap();
        let find = |gid: i64| {
            counts
                .iter()
                .find(|c| c["collectionGroupId"] == gid)
                .map(|c| c["tagCount"].as_i64().unwrap())
        };
        assert_eq!(find(group1), Some(2));
        assert_eq!(find(group2), Some(1));
    }

    // --- 試運転モードとロックダウン (設計 §5.6・2026-08-30 オーナー決定) ----

    /// 未ロックダウン（試運転モード）中は、管理 REST が `Authorization`
    /// ヘッダを一切付けなくても通ることを確認する - 実装指示「未ロックダウン
    /// 時に認証なしで管理 API が通る」。`GET /api/users`（`RoleGuard`で
    /// admin 限定のエンドポイント）を選んだのは、`require_auth_or_commissioning`
    /// だけでなく`require_role_at_least`（合成 identity の role が admin
    /// 相当であること）も両方バイパスされていることまで一度に確認できる
    /// ため。
    #[tokio::test]
    async fn unlocked_commissioning_mode_allows_admin_api_without_any_token() {
        let env = test_env_unlocked().await;
        let response = env
            .router
            .oneshot(
                HttpRequest::get("/api/users")
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "commissioning mode should let an unauthenticated request through"
        );
    }

    /// 対照実験: ロックダウン済み（既存の共有テスト環境 `test_env()`）では
    /// 従来どおり `Authorization` ヘッダ無しの管理 API アクセスが 401 になる
    /// ことを明示的に固定する - 実装指示「ロックダウン済みで認証なしなら
    /// 401」。このファイルの他の多数のテストも同じ前提の上に成り立って
    /// いるが、この試運転モード機能に直接紐づく回帰テストとして単独でも
    /// 固定しておく。
    #[tokio::test]
    async fn locked_down_admin_api_requires_a_token() {
        let env = test_env().await;
        let response = env
            .router
            .oneshot(
                HttpRequest::get("/api/users")
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    /// `GET /api/commissioning/status` は試運転モード中でも
    /// `Authorization` ヘッダ無しで読める必要がある（実装指示「未認証でも
    /// 取得できる必要がある」- 試運転モードでは認証そのものが無いため）。
    #[tokio::test]
    async fn unlocked_commissioning_status_is_readable_without_any_token() {
        let env = test_env_unlocked().await;
        let response = env
            .router
            .oneshot(
                HttpRequest::get("/api/commissioning/status")
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
        assert_eq!(body["lockedDown"], false);
    }

    /// 同じ読み取り専用ステータスは、ロックダウン済みでも同様に
    /// `Authorization` ヘッダ無しで読める（意図的 - UI が警告バナーの
    /// 表示可否を判断するのに使う想定で、どちらの状態でも認証を要求
    /// しない）。
    #[tokio::test]
    async fn locked_down_commissioning_status_is_still_readable_without_a_token() {
        let env = test_env().await;
        let response = env
            .router
            .oneshot(
                HttpRequest::get("/api/commissioning/status")
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
        assert_eq!(body["lockedDown"], true);
    }

    /// `POST /api/commissioning/lock-down`（設計 §5.6「遷移」の唯一の正方向
    /// 経路）: 試運転モード中は（`require_auth_or_commissioning`/
    /// `require_role_at_least`がバイパスされているため）トークン無しで
    /// 叩け、成功すると状態がロックダウン済みへ切り替わる。切り替わった
    /// 直後は、同じ router に対する以降のリクエストが（もはや試運転モード
    /// ではないので）再び 401 を要求するようになることまで確認する -
    /// これは `CommissioningState`（`Arc<AtomicBool>`）がプロセス内で
    /// 共有されていることの証拠でもある。
    #[tokio::test]
    async fn commissioning_lock_down_flips_state_and_then_requires_auth() {
        let env = test_env_unlocked().await;

        let response = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post("/api/commissioning/lock-down")
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
        assert_eq!(body["lockedDown"], true);

        // Now that lock-down has been applied, a plain unauthenticated
        // request to an admin route must be rejected again.
        let response = env
            .router
            .oneshot(
                HttpRequest::get("/api/users")
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    /// 実装指示「管理者0件でロックダウンしようとするとエラー」の REST
    /// 経由での確認。既存の唯一の admin アカウントを editor へ降格させた
    /// 上で（生の SQL 経由 - `UsersService::update_user`自身の「最後の
    /// admin を降格できない」ガードを迂回する必要がある。
    /// `crate::commissioning`のテストと同じ手筋）、ロックダウンを試みると
    /// 明確なエラー（4xx、`success` フラグを持たない `ApiError` 形）になり、
    /// 状態も試運転モードのまま変わらないことを確認する。
    #[tokio::test]
    async fn commissioning_lock_down_fails_without_any_admin_account() {
        let env = test_env_unlocked().await;
        sqlx::query("UPDATE users SET role = 'editor' WHERE username = 'admin'")
            .execute(&env.pool)
            .await
            .expect("downgrade the only admin via raw SQL");

        let response = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post("/api/commissioning/lock-down")
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            response.status().is_client_error(),
            "expected a 4xx error, got {}",
            response.status()
        );

        // Still commissioning mode - the failed lock-down must not have
        // flipped the state.
        let response = env
            .router
            .oneshot(
                HttpRequest::get("/api/commissioning/status")
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["lockedDown"], false);
    }

    // --- 試運転モード対応: 管理系 `/api/status`・`/api/values`
    // (設計 §5.6・2026-08-31 オーナー決定「案A」) -----------------------------
    //
    // 実機で判明した問題: `/api/v1/status`・`/api/v1/values`は
    // `require_tag_space_auth`（API キー or セッション bearer）固定で、
    // 設計 §5.6 の判断により試運転モードのバイパス対象**外**（PLC 書き込み
    // 経路と同じ境界を守るため）。試運転モード中（未ロックダウン・未
    // ログイン・API キー未発行）の管理 UI からこれを直接叩くと 401 になり、
    // 状態ページの「サーバー状態」「タグ現在値」が空になっていた
    // （`hostSwitchGate.isPreflightOk`が`status.revision`を要求するため、
    // 切替ウィザードまで連鎖的に塞がれる）。以下は管理系ルーターに新設した
    // `/api/status`・`/api/values`（`admin_status_router`）がこれを解消して
    // いることの確認。

    /// 試運転モード中は `/api/status`・`/api/values` が `Authorization`
    /// ヘッダ無しで読める - `unlocked_commissioning_mode_allows_admin_api_without_any_token`
    /// と同型（管理系ルーターに属するので`require_auth_or_commissioning`の
    /// バイパスが効く）。
    #[tokio::test]
    async fn unlocked_commissioning_mode_allows_admin_status_and_values_without_any_token() {
        let env = test_env_unlocked().await;

        let response = env
            .router
            .clone()
            .oneshot(
                HttpRequest::get("/api/status")
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = env
            .router
            .oneshot(
                HttpRequest::get("/api/values")
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    /// 対照実験: ロックダウン済みでは、他の管理系ルーターと同様
    /// `Authorization` ヘッダ無しの `/api/status`・`/api/values` は 401
    /// （`locked_down_admin_api_requires_a_token`と同型）。
    #[tokio::test]
    async fn locked_down_admin_status_and_values_require_a_token() {
        let env = test_env().await;

        let response = env
            .router
            .clone()
            .oneshot(
                HttpRequest::get("/api/status")
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = env
            .router
            .oneshot(
                HttpRequest::get("/api/values")
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    /// 回帰確認: `v1_status`本体を`compute_status`へ切り出した後も、
    /// `/api/v1/status`は従来どおり API キー認証で動く - `require_tag_space_auth`
    /// を一切変えていないことの確認（`issued_api_key_can_read_api_v1_tags`
    /// と同型、対象だけ`/api/v1/status`）。
    #[tokio::test]
    async fn v1_status_still_works_with_an_api_key_after_the_admin_status_split() {
        let env = test_env().await;
        let (status, issued) =
            issue_api_key(&env.router, &env.admin_token, "status-reader", &["read"]).await;
        assert_eq!(status, StatusCode::CREATED, "{issued:?}");
        let key = issued["key"].as_str().expect("key should be present");

        let (status, body) = v1_get(&env.router, key, "/api/v1/status").await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert!(body["revision"].is_number());
        assert!(body["last_config_error"].is_null());
    }

    /// 共有ロジックの担保: 管理系 `/api/status`・`/api/values`（camelCase）
    /// と `/api/v1/status`・`/api/v1/values`（snake_case）が同じ情報を
    /// 返すこと - 両者が[`compute_status`]/[`build_values_response`]を
    /// 共有していることの直接的な証拠（実装を二重管理していれば、この
    /// テストは値がずれた時点で落ちる）。セッション bearer はどちらの
    /// ルーターにも通る（`/api/v1/*`はAPIキーとセッション bearer の両対応、
    /// 管理系はセッション bearer 対応）ので、同じ`admin_token`で両方を
    /// 叩いて比較する。
    #[tokio::test]
    async fn admin_status_and_values_carry_the_same_information_as_v1() {
        let env = test_env().await;
        seed_scope_fixture(&env.router, &env.admin_token).await;

        let (status, v1_status_body) =
            v1_get(&env.router, &env.admin_token, "/api/v1/status").await;
        assert_eq!(status, StatusCode::OK, "{v1_status_body:?}");

        let response = env
            .router
            .clone()
            .oneshot(
                HttpRequest::get("/api/status")
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
        let admin_status_body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(admin_status_body["revision"], v1_status_body["revision"]);
        assert_eq!(admin_status_body["version"], v1_status_body["version"]);
        assert_eq!(
            admin_status_body["collectionState"],
            v1_status_body["collection_state"]
        );
        assert_eq!(
            admin_status_body["writeEnabled"],
            v1_status_body["write_enabled"]
        );
        assert_eq!(
            admin_status_body["lastConfigError"],
            v1_status_body["last_config_error"]
        );
        assert_eq!(
            admin_status_body["connections"].as_array().unwrap().len(),
            v1_status_body["connections"].as_array().unwrap().len()
        );
        assert_eq!(
            admin_status_body["connections"][0]["name"],
            v1_status_body["connections"][0]["name"]
        );
        assert_eq!(
            admin_status_body["connections"][0]["effectiveSimulation"],
            v1_status_body["connections"][0]["effective_simulation"]
        );

        let (status, v1_values_body) =
            v1_get(&env.router, &env.admin_token, "/api/v1/values").await;
        assert_eq!(status, StatusCode::OK, "{v1_values_body:?}");

        let response = env
            .router
            .oneshot(
                HttpRequest::get("/api/values")
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
        let admin_values_body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        let mut v1_tags: Vec<&str> = v1_values_body["values"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["tag"].as_str().unwrap())
            .collect();
        let mut admin_tags: Vec<&str> = admin_values_body["values"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["tag"].as_str().unwrap())
            .collect();
        v1_tags.sort_unstable();
        admin_tags.sort_unstable();
        assert!(!v1_tags.is_empty(), "fixture should seed at least one tag");
        assert_eq!(v1_tags, admin_tags);
        assert_eq!(admin_values_body["revision"], v1_values_body["revision"]);
    }

    // --- 試運転モード対応: 管理系 `/api/tag-catalog`・`/api/tag-stream`
    // (設計 §5.6・2026-08-31 オーナー決定「案A」の続き) ------------------------
    //
    // 状態ページ（`/api/status`・`/api/values`）は直した当日に直したが、
    // ライブタグモニタ（`tagMonitorAdmin.ts`）が別に`/api/v1/tags`
    // （catalog）・`/api/v1/stream`（WS）を直接叩いていることを見落として
    // いた - 同じ理由（`require_tag_space_auth`固定）で試運転モード中は
    // モニタの行が1つも表示されない不具合が残っていた。以下はその是正の
    // 確認（`unlocked_commissioning_mode_allows_admin_status_and_values_without_any_token`
    // 等と同型）。WS（`/api/tag-stream`）は実TCP接続が要るため
    // `tests/stream.rs`側で確認する。

    /// 試運転モード中は `/api/tag-catalog` が `Authorization` ヘッダ無しで
    /// 読める。
    #[tokio::test]
    async fn unlocked_commissioning_mode_allows_admin_tag_catalog_without_any_token() {
        let env = test_env_unlocked().await;

        let response = env
            .router
            .oneshot(
                HttpRequest::get("/api/tag-catalog")
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    /// 対照実験: ロックダウン済みでは、他の管理系ルーターと同様
    /// `Authorization` ヘッダ無しの `/api/tag-catalog` は 401。
    #[tokio::test]
    async fn locked_down_admin_tag_catalog_requires_a_token() {
        let env = test_env().await;

        let response = env
            .router
            .oneshot(
                HttpRequest::get("/api/tag-catalog")
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    /// 回帰確認: catalog本体を`build_catalog_response`へ切り出した後も、
    /// `/api/v1/tags`は従来どおり API キー認証で動く（`v1_status_still_works_with_an_api_key_after_the_admin_status_split`
    /// と同型、対象だけ`/api/v1/tags`）。
    #[tokio::test]
    async fn v1_tags_still_works_with_an_api_key_after_the_admin_catalog_split() {
        let env = test_env().await;
        seed_scope_fixture(&env.router, &env.admin_token).await;
        let (status, issued) =
            issue_api_key(&env.router, &env.admin_token, "catalog-reader", &["read"]).await;
        assert_eq!(status, StatusCode::CREATED, "{issued:?}");
        let key = issued["key"].as_str().expect("key should be present");

        let (status, body) = v1_get(&env.router, key, "/api/v1/tags").await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert!(body["tags"].as_array().is_some_and(|tags| !tags.is_empty()));
    }

    /// 共有ロジックの担保: 管理系 `/api/tag-catalog`（camelCase）と
    /// `/api/v1/tags`（snake_case）が同じ情報を返すこと -
    /// 両者が[`build_catalog_response`]を共有していることの直接的な証拠
    /// （`admin_status_and_values_carry_the_same_information_as_v1`と同型）。
    #[tokio::test]
    async fn admin_tag_catalog_carries_the_same_information_as_v1_tags() {
        let env = test_env().await;
        seed_scope_fixture(&env.router, &env.admin_token).await;

        let (status, v1_body) = v1_get(&env.router, &env.admin_token, "/api/v1/tags").await;
        assert_eq!(status, StatusCode::OK, "{v1_body:?}");

        let response = env
            .router
            .oneshot(
                HttpRequest::get("/api/tag-catalog")
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
        let admin_body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(admin_body["revision"], v1_body["revision"]);
        assert_eq!(admin_body["collectionMode"], v1_body["collection_mode"]);

        let mut v1_tag_names: Vec<&str> = v1_body["tags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["external_name"].as_str().unwrap())
            .collect();
        let mut admin_tag_names: Vec<&str> = admin_body["tags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["externalName"].as_str().unwrap())
            .collect();
        v1_tag_names.sort_unstable();
        admin_tag_names.sort_unstable();
        assert!(
            !v1_tag_names.is_empty(),
            "fixture should seed at least one tag"
        );
        assert_eq!(v1_tag_names, admin_tag_names);

        // camelCase 変換が正しく効いていること（フィールド名だけでなく値も
        // 一致すること）の代表サンプル1件。
        let v1_first = v1_body["tags"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["external_name"] == v1_tag_names[0])
            .unwrap();
        let admin_first = admin_body["tags"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["externalName"] == v1_tag_names[0])
            .unwrap();
        assert_eq!(admin_first["tagKey"], v1_first["tag_key"]);
        assert_eq!(admin_first["dataType"], v1_first["data_type"]);
        assert_eq!(admin_first["periodMs"], v1_first["period_ms"]);
        assert_eq!(
            admin_first["effectiveSimulation"],
            v1_first["effective_simulation"]
        );
        assert_eq!(admin_first["valueSource"], v1_first["value_source"]);
    }
}
