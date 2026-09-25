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
use axum::routing::{get, post, put};
use axum::{Json, Router};
use banto_broker::{is_supported_protocol, BrokerConnectionStatus, BrokerError};
use banto_collect::{
    build_config_from, connections_with_collected_groups, ApplyReport, ConnectionStatus, Quality,
    RegistrySnapshot,
};
use banto_core::{BantoError, ErrorBody, FieldError, ListParams, ListResult};
use banto_tstore::LocalDate;
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
    auth_routes, require_banto_client_header, sse_route, ApiError, AuthState, Identity,
    ServerEvent, SessionAccount, SessionStamp, SessionValidation,
};
use banto_tags::{
    BatchTagDeleteOutcome, BatchTagOutcome, BatchTagUpdateOutcome, CollectionGroup,
    CollectionGroupInput, CollectionGroupService, GroupTagCount, PlcConnection, PlcConnectionInput,
    PlcConnectionService, Tag, TagInput, TagService, TagUpdateError, COMPUTED_TAG_KIND,
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
use crate::computed::ComputedEngine;
use crate::controller::{CollectionController, CollectionState, CollectionStatus, RunMode};
use crate::hub::{CollectorManager, SimulationCoverageReport, TagEntry, TagMap};
use crate::mqtt::MqttPublisher;
use crate::pending_changes::{PendingChange, PendingChangesService};
use crate::settings::{AuditSettings, MqttSettings, SettingsService, StoreSettings};
use crate::sink::{
    reject_delete_if_referenced_by_sink_group, remove_tag_from_all_sink_groups, SinkGroup,
    SinkGroupInput, SinkGroupService, SinkGroupStatusPush, SinkStatusSnapshot, SinkStatusStore,
};
use crate::system_info::{SystemInfoSampler, SystemInfoSnapshot};
use crate::users::{Role, UserIdentity, UserSummary, UsersService};
use crate::value_source::{effective_simulation_for_tag, value_source_for_tag};
use crate::write_audit::{WriteAuditEntry, WriteAuditService};
use crate::write_control::WriteControl;
use crate::write_rate::WriteRateLimiter;

// --- shared helpers (users/audit/RBAC - copied from chronogazer/relay-wright's rest.rs) ---

pub(crate) fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
}

/// `banto_server::require_auth`'s own 401 body, reproduced here for
/// `require_tag_space_auth` (T0-2's `/api/v1/*` auth middleware below) since
/// that middleware replaces `require_auth` entirely rather than wrapping it.
/// T19 S5: `crate::mcp`'s `POST /mcp` auth middleware reuses this same body
/// for all of its (simplified, single-status) auth failure branches.
pub(crate) fn unauthorized_response() -> Response {
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
    /// #431: このゲートの後ろにあるルートの操作の種類。照合できなかった
    /// ときの扱いだけがこれで変わる。既定は [`OperationKind::Normal`]
    /// （例外なし）で、[`OperationKind::StopWrites`] はハンドラの隣で
    /// 明示的に宣言した定数からだけ渡す（`WRITE_CONTROL_DISABLE_OPERATION`）。
    operation: OperationKind,
}

/// #431: ルートの操作の種類（ルート名ではなく、ハンドラの隣の定数で宣言する）。
/// banto v1.7.0 #204 で要求ごとの照合を入れたため、DB が応答しないと緊急停止
/// （`POST /api/write-control/disable`）まで 500 で通らなくなった。書き込みを
/// **止める**操作だけは、照合できないときも例外で通す（オーナー決定
/// 2026-09-24、issue #431）。ルートを足しても、宣言しない限り例外は広がらない。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OperationKind {
    /// 既定。照合できなければ（DB エラー）その要求は 500。
    Normal,
    /// 書き込みを止める操作。照合できない（DB エラー・タイムアウト）ときも、
    /// それまで有効だった（メモリ上で有効な）セッションなら通す。失効が
    /// **確認できた**セッションは拒否する。
    StopWrites,
}

/// #431: `AuthState::authenticate` の結果の分類（タイムアウトを含む）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionCheck {
    /// 照合できて、有効。
    Valid,
    /// トークンが無い・期限切れ、または照合でアカウントの削除・世代の変更が
    /// **確認できた**。
    Revoked,
    /// DB が照合に答えられなかった（エラー・タイムアウト）。
    Unverified,
}

/// #431: ゲートの判断。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GateDecision {
    /// 照合できて有効。通す。
    Allow,
    /// 照合できなかったが、止める操作で、それまで有効だったセッション。
    /// 例外で通し、監査に印を付ける（[`UnverifiedStopException`]）。
    AllowUnverifiedStop,
    /// 401。
    Unauthorized,
    /// 照合できなかった。その要求だけ 500（セッションは残す）。
    Unavailable,
}

/// #431: ゲートの判断（純関数、表でテストする）。`previously_valid` は
/// 照合できなかったとき、そのトークンがメモリ上でまだ有効（既知・期限内）
/// だったか - 「それまで有効だったセッション」の意味。
pub(crate) fn session_gate_decision(
    check: SessionCheck,
    operation: OperationKind,
    previously_valid: bool,
) -> GateDecision {
    match (check, operation) {
        (SessionCheck::Valid, _) => GateDecision::Allow,
        (SessionCheck::Revoked, _) => GateDecision::Unauthorized,
        (SessionCheck::Unverified, OperationKind::Normal) => GateDecision::Unavailable,
        (SessionCheck::Unverified, OperationKind::StopWrites) if previously_valid => {
            GateDecision::AllowUnverifiedStop
        }
        (SessionCheck::Unverified, OperationKind::StopWrites) => GateDecision::Unauthorized,
    }
}

/// #431: 止める操作のルートで、照合を待つ上限。DB が応答しない（ハングする）
/// ときも緊急停止を待たせない。banto の SSE の再照合と同じ 5 秒
/// （`banto_server::events::REVALIDATE_TIMEOUT`）。通常のルートには掛けない
/// （従来どおり）。
const STOP_SESSION_CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// #431: 照合できなかったが止める操作として例外で通した要求に、ゲートが
/// 載せる印。ハンドラはこれを見て監査に残す。
#[derive(Clone, Copy, Debug)]
pub(crate) struct UnverifiedStopException;

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
///
/// banto v1.7.0 #204: ロックダウン済みの判定は `banto_server::require_auth`
/// と同じく `AuthState::authenticate` でアカウントと照合する（メモリ上の
/// トークンだけを見る `verify` ではない）。削除・世代の変わった
/// アカウントのセッションは 401、DB が照合に答えられないときはトークンを
/// 残したままこの要求だけ失敗させる（`ApiError` の 500）。通過した要求には
/// `require_auth` と同じく `AuthenticatedSession` を載せる。
///
/// 試運転モード（未ロックダウン）はトークンを発行しない（合成 identity を
/// 要求ごとに返すだけ）ので、ロックダウンした時点で次の要求からこの照合に
/// 切り替わる - 持ち越すセッションが無い。
async fn require_auth_or_commissioning(
    State(gate): State<AuthGate>,
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if !gate.commissioning.is_locked_down() {
        return next.run(req).await;
    }
    let token = bearer_token(req.headers())
        .map(str::to_string)
        .or_else(|| extract_ws_protocol_token(req.uri().path(), req.headers()));
    let Some(token) = token else {
        return unauthorized_response();
    };
    // #431: 止める操作だけ、照合に上限時間を掛ける（DB がハングしても停止を
    // 待たせない）。通常の操作は従来どおり。
    let outcome = match gate.operation {
        OperationKind::Normal => Some(gate.auth.authenticate(&token).await),
        OperationKind::StopWrites => {
            tokio::time::timeout(STOP_SESSION_CHECK_TIMEOUT, gate.auth.authenticate(&token))
                .await
                .ok()
        }
    };
    let (check, session, error) = match outcome {
        Some(Ok(Some(session))) => (SessionCheck::Valid, Some(session), None),
        Some(Ok(None)) => (SessionCheck::Revoked, None, None),
        Some(Err(err)) => (SessionCheck::Unverified, None, Some(err)),
        None => (SessionCheck::Unverified, None, None),
    };
    // 照合できなかったとき、トークンはメモリ上に残っている（`authenticate` は
    // DB エラーでトークンを消さない）。まだ既知・期限内なら「それまで有効」。
    let previously_valid =
        check == SessionCheck::Unverified && gate.auth.identity_for(&token).is_some();
    match session_gate_decision(check, gate.operation, previously_valid) {
        GateDecision::Allow => {
            if let Some(session) = session {
                req.extensions_mut().insert(session);
            }
            next.run(req).await
        }
        GateDecision::AllowUnverifiedStop => {
            eprintln!(
                "banto-hub: セッションを照合できなかったため、書き込みの停止のみ例外で受け付けます"
            );
            req.extensions_mut().insert(UnverifiedStopException);
            next.run(req).await
        }
        GateDecision::Unauthorized => unauthorized_response(),
        GateDecision::Unavailable => {
            ApiError(error.unwrap_or_else(|| {
                BantoError::Other("セッションを照合できませんでした".to_string())
            }))
            .into_response()
        }
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
            // 合成の identity（実在の行ではない）。照合には使わない。
            auth_epoch: 0,
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
                operation: OperationKind::Normal,
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
        Ok(user) => {
            // banto v1.7.0 #204: 新しいアカウントの `(id, auth_epoch)` に結び
            // 付けて発行する。`issue_token`（世代なし）は `Lookup` 付きの
            // `AuthState` では最初の要求で拒否される。
            let account = session_account(&user);
            let identity = account.identity.clone();
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
            let token = state.auth.issue_account_token(account, false);
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
    // banto v1.7.0 #204: `require_auth` の外なので、`identity_for`（メモリ上の
    // 値だけ）ではなく `authenticate` でアカウントと照合する。削除・世代の
    // 変わったアカウントのセッションは 401（パスワード変更に進ませない）。
    let Some(token) = bearer_token(&headers) else {
        return Err(ApiError(BantoError::Unauthorized));
    };
    let Some(session) = state.auth.authenticate(token).await? else {
        return Err(ApiError(BantoError::Unauthorized));
    };
    let identity = session.identity;

    let new_epoch = state
        .users
        .change_password(&identity.id, &body.current_password, &body.new_password)
        .await?;
    // 変更で世代が進み、このアカウントのセッション（他の端末・Tauri・
    // Remember me）はすべて終わる。現在のパスワードを示したこのトークンだけ
    // 新しい世代へ付け替える - ただし照合してからの変更がこの 1 回だけの
    // とき（あいだにロール変更などが挟まったら、他と同じく終わらせる）。
    // 付け替えに失敗（その間に失効）しても、変更自体は成功している。
    if let Some(stamp) = session.stamp {
        if new_epoch == stamp.auth_epoch + 1 {
            state.auth.rotate_session_epoch(token, stamp, new_epoch);
        }
    }
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

/// banto v1.7.0 #204: `UserIdentity` -> セッションが結び付く `Identity` +
/// [`SessionStamp`]（`Identity.id` はユーザー名 - `banto_server::Identity` の
/// doc comment の規約）。セットアップ直後のログイン（`issue_account_token`）と
/// [`user_session_lookup`] の両方が使う。
pub(crate) fn session_account(user: &UserIdentity) -> SessionAccount {
    SessionAccount {
        identity: Identity {
            id: user.username.clone(),
            name: user.display_name.clone(),
            role: user.role.to_string(),
        },
        stamp: SessionStamp {
            account_id: user.id,
            auth_epoch: user.auth_epoch,
        },
    }
}

/// banto v1.7.0 #204: 要求のたびにアカウントを読み直す `SessionLookup`。
/// ユーザー名で `users` を引き、無ければ `Ok(None)`（セッションは失効）、DB が
/// 答えられなければ `Err`（その要求は失敗、セッションは残す）。
/// `UsersService::verify` はユーザー名を正規化しない（入力のまま照合する）
/// ので、ここも入力のまま引く - ログイン時は入力どおりのユーザー名で呼ばれ、
/// `verify` と同じアカウントを返すことが条件になる（`SessionValidation::Lookup`
/// の doc comment）。
pub fn user_session_lookup(
    users: UsersService,
) -> impl Fn(
    String,
) -> futures_util::future::BoxFuture<'static, Result<Option<SessionAccount>, BantoError>>
       + Send
       + Sync
       + 'static {
    move |username: String| {
        let users = users.clone();
        Box::pin(async move {
            Ok(users
                .get_by_username(&username)
                .await?
                .map(|user| session_account(&user)))
        })
    }
}

/// banto v1.7.0 #204: 実アカウント用の REST `AuthState`。ログインは
/// [`audited_credential_verifier`]、要求ごとの照合は [`user_session_lookup`]
/// （`SessionValidation::Lookup`）。削除・降格・パスワード変更/リセットで、
/// そのアカウントのセッション（他の端末・Remember me を含む）は次の要求で
/// 401 になる。本番の `AuthState` はすべてここで作り、照合の付け忘れを防ぐ
/// （`SessionValidation::DisabledNoRevocation` は固定の検証関数を使うテスト
/// 専用）。
pub fn user_auth_state(users: UsersService, audit: AuditLogService) -> AuthState {
    AuthState::new(
        audited_credential_verifier(users.clone(), audit),
        SessionValidation::lookup(user_session_lookup(users)),
    )
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
                operation: OperationKind::Normal,
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

// T21 S3（docs/banto-hub-t21-design.md、構成補助 MCP の
// create_api_key）: `crate::mcp`の`create_api_key`ツールが同じ
// request 型・「現在時刻より未来」検証を再利用する（`GrpcSettingsBody`等と
// 同じ`pub(crate)`化の理由 - 二重実装しない）。フィールドも
// `pub(crate)`にする必要がある点が`GrpcSettingsBody`と同じ（応答専用の
// `IssuedApiKeyResponse`と違い、呼び出し元が`name`/`scopes`/`expires_at`を
// 個別に読む）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateApiKeyRequest {
    pub(crate) name: String,
    pub(crate) scopes: Vec<String>,
    /// H10 ①（docs/improvement-plan.md、2026-08-08 オーナー決定）: 任意の
    /// 有効期限（絶対 epoch ミリ秒、wire は `expiresAt` -
    /// `crate::api_keys::ApiKeySummary` の `expires_at`/`FieldError::field`
    /// 命名規約と同じ camelCase に揃えるため、この構造体自体にも
    /// `rename_all = "camelCase"` を追加した - `name`/`scopes` は
    /// 1語なので実質無変化）。省略/`null` = 無期限（既定・動作不変、
    /// `#[serde(default)]` は既存クライアントの後方互換のため -
    /// `GrpcSettingsBody::bind` 等と同じ規約）。`Some` の場合は
    /// [`api_keys_create`]（および`crate::mcp`の`create_api_key`）が
    /// 「現在時刻より未来」を検証してから[`ApiKeysService::issue`]に渡す
    /// （`issue` 自体は再検証しない）。
    #[serde(default)]
    pub(crate) expires_at: Option<i64>,
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
// T21 S3: `crate::mcp`の`create_api_key`ツールが同じ応答型を再利用する
// （`MqttSettingsResponse`等と同じ`pub(crate)`化の理由）。応答専用型
// なので（`CreateApiKeyRequest`と違い）フィールド自体は`pub(crate)`に
// しない - `From<IssuedApiKey>`経由で構築し`Serialize`するだけで、
// 呼び出し元が個々のフィールドを読むことはない。
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct IssuedApiKeyResponse {
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
                operation: OperationKind::Normal,
            },
            require_auth_or_commissioning,
        ))
}

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
    commissioning: CommissioningState,
    audit: AuditLogService,
    events: broadcast::Sender<ServerEvent>,
}

/// `GET /api/v1/status` の `write_enabled`/`write_was_enabled_before_restart`
/// と同じ形の応答（`POST /api/write-control/enable|disable` の応答）。
/// `write_was_enabled_before_restart` は「起動時に永続テーブルから復元した
/// 値」を指す（フィールド名は外部クライアント Thermal Monitor との互換の
/// ため変更しない。2026-09-09 オーナー決定 #340）。
#[derive(Debug, Serialize, ToSchema)]
struct WriteControlStatusResponse {
    write_enabled: bool,
    write_was_enabled_before_restart: bool,
}

/// #340 レビュー対応（2026-09-14）: `enabled_persisted` は次回起動時の
/// ライブ値そのものとして復元されるようになったため（`WriteControl` の
/// モジュール doc comment参照）、永続化の失敗を握りつぶして 200 を返すと
/// 「今は効いているが再起動すると黙って元に戻る」状態を作ってしまう。
/// disable/enable で非対称に扱う:
/// - **disable（非常停止）**: 先にライブフラグを `disable()` する(DB 障害
///   があっても書き込みは必ず即座に止まる)。永続化が失敗しても無効化
///   そのものは成功しているので、その旨を含めた 500 を返す。
/// - **enable**: 先に `persist_enabled(true)` を試す。失敗したらライブ
///   フラグには触れない(disabled のまま)。永続化が確認できてから
///   `enable()` する。
///
/// どちらの分岐でも監査ログは成功/失敗の両方を記録する(失敗時は
/// `detail.persisted = false` + `detail.error`)。`ServerEvent::ResourceChanged`
/// はライブ状態が実際に変わった場合にのみ送る。
async fn write_control_set(
    state: &WriteControlAdminState,
    headers: &HeaderMap,
    enabled: bool,
    action: &str,
    unverified_stop: bool,
) -> Response {
    let identity = actor_identity(headers, &state.auth, &state.commissioning);
    let actor_id = identity.as_ref().map(|i| i.id.as_str());

    if enabled {
        if let Err(err) =
            crate::write_control::persist_enabled(&state.manager.pool(), true, actor_id).await
        {
            record_write_control_failure(
                &state.audit,
                &state.auth,
                &state.commissioning,
                headers,
                action,
                true,
                err.to_string(),
                unverified_stop,
            )
            .await;
            return write_control_persist_failed_response(
                "永続化に失敗したため有効化しませんでした。",
            );
        }
        state.write_control.enable();
    } else {
        state.write_control.disable();
        if let Err(err) =
            crate::write_control::persist_enabled(&state.manager.pool(), false, actor_id).await
        {
            record_write_control_failure(
                &state.audit,
                &state.auth,
                &state.commissioning,
                headers,
                action,
                false,
                err.to_string(),
                unverified_stop,
            )
            .await;
            let _ = state.events.send(ServerEvent::ResourceChanged {
                resource: "write_control".to_string(),
            });
            return write_control_persist_failed_response(
                "書き込み受付は無効化しましたが永続化に失敗したため再起動後は前回の永続値に戻ります。",
            );
        }
    }

    record_write(
        &state.audit,
        &state.auth,
        &state.commissioning,
        headers,
        action,
        "write_control",
        "1",
        Some(with_session_exception_mark(
            json!({ "enabled": enabled, "persisted": true }),
            unverified_stop,
        )),
    )
    .await;
    let _ = state.events.send(ServerEvent::ResourceChanged {
        resource: "write_control".to_string(),
    });

    Json(WriteControlStatusResponse {
        write_enabled: state.write_control.is_enabled(),
        write_was_enabled_before_restart: state.write_control.was_enabled_before_restart(),
    })
    .into_response()
}

/// [`write_control_set`] の永続化失敗時の監査行 - `record_write`（成功専用、
/// `result: "ok"` 固定）とは別に、`result: "failed"` と
/// `detail.persisted = false` / `detail.error` を記録する。
#[allow(clippy::too_many_arguments)]
async fn record_write_control_failure(
    audit: &AuditLogService,
    auth: &AuthState,
    commissioning: &CommissioningState,
    headers: &HeaderMap,
    action: &str,
    enabled: bool,
    error: String,
    unverified_stop: bool,
) {
    let identity = actor_identity(headers, auth, commissioning);
    audit
        .record(AuditEntry {
            actor_username: identity.as_ref().map(|i| i.id.as_str()),
            actor_role: identity.as_ref().map(|i| i.role.as_str()),
            action,
            resource: "write_control",
            entity_id: Some("1"),
            detail: Some(with_session_exception_mark(
                json!({ "enabled": enabled, "persisted": false, "error": error }),
                unverified_stop,
            )),
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
const WRITE_CONTROL_ENABLE_OPERATION: OperationKind = OperationKind::Normal;

async fn write_control_enable(
    State(state): State<WriteControlAdminState>,
    headers: HeaderMap,
) -> Response {
    write_control_set(&state, &headers, true, "enable", false).await
}

/// #431: 緊急停止は「書き込みを止める」操作だと宣言する。DB が応答しない
/// ときも、それまで有効だったセッションなら受け付ける
/// （[`session_gate_decision`]）。この宣言が例外の唯一の入口。
const WRITE_CONTROL_DISABLE_OPERATION: OperationKind = OperationKind::StopWrites;

async fn write_control_disable(
    State(state): State<WriteControlAdminState>,
    headers: HeaderMap,
    exception: Option<axum::Extension<UnverifiedStopException>>,
) -> Response {
    write_control_set(&state, &headers, false, "disable", exception.is_some()).await
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
    // #431: ルートごとに、ハンドラの隣で宣言した操作の種類でゲートを組む
    // （同じゲートを 2 本のルートで共有しない - 例外が再開に漏れないように）。
    let gated = |router: Router<WriteControlAdminState>, operation: OperationKind| {
        router
            .with_state(state.clone())
            .layer(middleware::from_fn_with_state(
                RoleGuard {
                    auth: auth.clone(),
                    commissioning: commissioning.clone(),
                    min: Role::Admin,
                    resource: "write_control",
                    audit: audit.clone(),
                },
                require_role_at_least,
            ))
            .layer(middleware::from_fn_with_state(
                AuthGate {
                    auth: auth.clone(),
                    commissioning: commissioning.clone(),
                    operation,
                },
                require_auth_or_commissioning,
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
    let identity = actor_identity(headers, &state.auth, &state.commissioning);
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
                operation: OperationKind::Normal,
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
// T21 S2-b（docs/banto-hub-t21-design.md、構成補助 MCP の設定 get/set）:
// `crate::mcp`が同じ入力検証・apply 挙動を再利用するため、この型と
// `validate_mqtt_settings_request`を`pub(crate)`にしてある - REST/MCP で
// 設定の validation/response 形を二重実装しないための共有（このモジュールの
// 他の`pub(crate)`化と同じ理由）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MqttSettingsRequest {
    pub(crate) enabled: bool,
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) client_id: String,
    #[serde(default)]
    pub(crate) username: Option<String>,
    /// 空文字は「変更なし」（既存のパスワードを維持）- 実装指示どおり
    /// （`crate::settings::MqttSettings`のフィールド doc comment参照）。
    /// `GET`はパスワードを一切返さないので、UI が「今の値」を知らずに
    /// フォームを保存しても上書きされない。
    #[serde(default)]
    pub(crate) password: Option<String>,
    pub(crate) prefix: String,
    pub(crate) qos: u8,
    pub(crate) min_interval_ms: i64,
}

/// `password`フィールドが**存在しない**- 実装指示「password は GET で
/// 返さない」をレスポンス型そのもので保証する（`serde`のシリアライズ対象
/// フィールドに含めていないので、実装ミスで漏らしようがない）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MqttSettingsResponse {
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
pub(crate) fn validate_mqtt_settings_request(body: &MqttSettingsRequest) -> Vec<FieldError> {
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
                operation: OperationKind::Normal,
            },
            require_auth_or_commissioning,
        ))
}

// --- 履歴保持 (T19 S2-d、docs/banto-hub-t19-design.md §5.1、UX-39): admin
// 限定、CSRF + bearer -----------------------------------------------------
//
// `GET/PUT /api/store-settings` は `mqtt_settings_router`/`audit_log_router`
// と同型（admin ロール限定 + CSRF は `api_router` 側で管理系ルーター全体に
// 一括で被せる）だが、**`PUT` は保存するだけで即時適用（剪定）はしない**
// （2026-09-03 オーナー決定1: 「保存は方針のみ。剪定は別ボタンで即時実行」）
// - `mqtt_settings_put`/`grpc_settings_put`の「保存 → apply」パターンとは
// 意図的に非対称。剪定は `POST /api/store-settings/prune-now` だけが行う
// 別の破壊的操作。
//
// `prune-preview`/`prune-now` は `data_dir`/`clock` を
// `CollectorManager::data_dir`/`CollectorManager::clock`（このモジュールの
// 他の`now_ms`利用箇所と同じ供給元 - `state.manager.clock()`）から得る。
// `SettingsService::store_config().data_dir` を直接読まないのは、
// `data_dir_override`/`BANTO_HUB_DATA`環境変数で実効値が永続設定と
// ズレている環境があり得るため - `CollectorManager::data_dir`のdoc comment
// 参照。`today`の算出は`crate::runtime::prune_once`と全く同じ式
// （`LocalDate::from_epoch_ms(clock.now_ms(), clock.utc_offset_ms())`）を
// 使い、剪定タイミングの判断がずれないようにする。

#[derive(Clone)]
struct StoreSettingsAdminState {
    manager: Arc<CollectorManager>,
    auth: AuthState,
    commissioning: CommissioningState,
    audit: AuditLogService,
}

/// `GET/PUT /api/store-settings`・`POST /api/store-settings/prune-*`の
/// request/response body。`data_dir`はこの T19 S2-d のスコープ外なので
/// 含めない（実装指示「data_dir は今回のスコープ外」）。`retentionDays`が
/// `null`＝無制限（[`crate::settings::StoreSettings::retention_days`]の
/// doc comment参照）。
// T21 S2-b: `crate::mcp`の`get_retention`/`set_retention`が同じ
// request/response 型・validation を再利用する（`MqttSettingsRequest`等と
// 同じ`pub(crate)`化の理由）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StoreSettingsResponse {
    retention_days: Option<i64>,
}

impl From<StoreSettings> for StoreSettingsResponse {
    fn from(config: StoreSettings) -> Self {
        Self {
            retention_days: config.retention_days,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StoreSettingsRequest {
    #[serde(default)]
    pub(crate) retention_days: Option<i64>,
}

/// tstore 保持期間の上限（10年）。実装指示「上限は3650（10年）とする」。
const MAX_STORE_RETENTION_DAYS: i64 = 3650;

/// 入力検証（実装指示どおり）: `null`（無制限）は常に可。数値は 1 以上
/// [`MAX_STORE_RETENTION_DAYS`] 以下の整数のみ（0 以下＝「当日のみ」相当は
/// 今回 UI から提供しない）。
pub(crate) fn validate_store_settings_request(body: &StoreSettingsRequest) -> Vec<FieldError> {
    let mut errors = Vec::new();
    if let Some(days) = body.retention_days {
        if !(1..=MAX_STORE_RETENTION_DAYS).contains(&days) {
            errors.push(FieldError {
                field: "retentionDays".to_string(),
                message: format!(
                    "retentionDays は 1〜{MAX_STORE_RETENTION_DAYS} の整数、または無制限（null）にしてください"
                ),
            });
        }
    }
    errors
}

async fn store_settings_get(
    State(state): State<StoreSettingsAdminState>,
) -> Result<Json<StoreSettingsResponse>, ApiError> {
    let config = SettingsService::new(state.manager.pool())
        .store_config()
        .await?;
    Ok(Json(config.into()))
}

/// `PUT /api/store-settings`（admin 限定）: 保持方針を**保存するだけ**
/// （2026-09-03 オーナー決定1）。ここで `banto_tstore::prune_files`/
/// `plan_prune`を呼ぶことは絶対にしない - 次回の24h周期タスク/起動時
/// （`crate::runtime::prune_once`）で自然に効く。`dataDir`はこの
/// リクエストで変更しない（既存値を読んで維持する）。
async fn store_settings_put(
    State(state): State<StoreSettingsAdminState>,
    headers: HeaderMap,
    Json(body): Json<StoreSettingsRequest>,
) -> Result<Json<StoreSettingsResponse>, ApiError> {
    let field_errors = validate_store_settings_request(&body);
    if !field_errors.is_empty() {
        return Err(ApiError(BantoError::Validation { field_errors }));
    }

    let settings_service = SettingsService::new(state.manager.pool());
    let existing = settings_service.store_config().await?;
    let config = StoreSettings {
        data_dir: existing.data_dir,
        retention_days: body.retention_days,
    };
    settings_service.set_store_config(&config).await?;

    record_write(
        &state.audit,
        &state.auth,
        &state.commissioning,
        &headers,
        "update",
        "store_settings",
        "1",
        Some(json!({ "retentionDays": config.retention_days })),
    )
    .await;

    Ok(Json(config.into()))
}

/// `POST /api/store-settings/prune-preview`の応答: 現在
/// **保存済みの**保持方針で削除される見込みのファイル数
/// （`banto_tstore::plan_prune`、実際には削除しない）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PrunePreviewResponse {
    would_delete_count: usize,
}

/// `retention_days`（設定の生値）を`banto_tstore`が要求する`u32`日数へ
/// 変換する。剪定は不可逆な削除なので、`None`（無制限）はもちろん、
/// `u32`に収まらない想定外の値（`parse_retention`は上限クランプを
/// しないため、DBを直接編集された等で`u32::MAX`超の値が入り得る）の
/// ときも「削除しない」側（`None`）に倒す - 変換失敗を「削除数0」では
/// なく「大量削除」に丸めるのは破壊的操作の失敗方向として逆。
///
/// prune-preview / prune-now の両方が必ずこの関数の結果**だけ**で
/// 「prune するか否か」を決めること - 判定がここから外れて両者でズレると
/// 「プレビューは0件だったのに実行したら消えた」が起き得る。
fn resolve_prune_retention_days(retention_days: Option<i64>) -> Option<u32> {
    retention_days.and_then(|days| u32::try_from(days).ok())
}

/// `POST /api/store-settings/prune-preview`（admin 限定・読み取り専用）:
/// フロントの確認ダイアログ（実装指示「削除予定数を返す手段を必ず用意」）が
/// 使う。破壊的操作ではないため監査エントリは記録しない（read routes are
/// never audited - `crate::audit`のモジュール doc comment参照）。
async fn store_settings_prune_preview(
    State(state): State<StoreSettingsAdminState>,
) -> Result<Json<PrunePreviewResponse>, ApiError> {
    let config = SettingsService::new(state.manager.pool())
        .store_config()
        .await?;
    let Some(retention_days) = resolve_prune_retention_days(config.retention_days) else {
        return Ok(Json(PrunePreviewResponse {
            would_delete_count: 0,
        }));
    };
    let clock = state.manager.clock();
    let today = LocalDate::from_epoch_ms(clock.now_ms(), clock.utc_offset_ms());
    let plan = banto_tstore::plan_prune(state.manager.data_dir(), retention_days, today)
        .map_err(|err| ApiError(BantoError::Storage(err.to_string())))?;
    Ok(Json(PrunePreviewResponse {
        would_delete_count: plan.deleted.len(),
    }))
}

/// `POST /api/store-settings/prune-now`の応答: 実際に削除したファイル数。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PruneNowResponse {
    deleted_count: usize,
}

/// `POST /api/store-settings/prune-now`（admin 限定）: **保存済みの**
/// 保持方針（`store_config().retention_days`）で今すぐ剪定する
/// （2026-09-03 オーナー決定1: 「今すぐ古い履歴を削除」の実体）。無制限
/// （`None`）のときは何も削除せず`deletedCount: 0`を返す。不可逆な削除
/// なので必ず監査エントリを記録する。
async fn store_settings_prune_now(
    State(state): State<StoreSettingsAdminState>,
    headers: HeaderMap,
) -> Result<Json<PruneNowResponse>, ApiError> {
    let config = SettingsService::new(state.manager.pool())
        .store_config()
        .await?;
    let deleted_count = match resolve_prune_retention_days(config.retention_days) {
        Some(retention_days) => {
            let clock = state.manager.clock();
            let today = LocalDate::from_epoch_ms(clock.now_ms(), clock.utc_offset_ms());
            let report = banto_tstore::prune_files(state.manager.data_dir(), retention_days, today)
                .map_err(|err| ApiError(BantoError::Storage(err.to_string())))?;
            report.deleted.len()
        }
        None => {
            // `resolve_prune_retention_days`が`None`を返すのは無制限設定
            // （既知の正常系、ログ不要）か、`u32`に収まらない想定外の値
            // （破壊的操作の入口なので記録する）のどちらか。
            if let Some(retention_days) = config.retention_days {
                crate::hub_log::log_err_line(&format!(
                    "banto-hub: 保持日数の値が想定外のため剪定をスキップしました: {retention_days}"
                ));
            }
            0
        }
    };

    record_write(
        &state.audit,
        &state.auth,
        &state.commissioning,
        &headers,
        "delete",
        "store_settings_prune",
        "1",
        Some(json!({ "deletedCount": deleted_count })),
    )
    .await;

    Ok(Json(PruneNowResponse { deleted_count }))
}

fn store_settings_router(
    manager: Arc<CollectorManager>,
    audit: AuditLogService,
    auth: AuthState,
    commissioning: CommissioningState,
) -> Router {
    let state = StoreSettingsAdminState {
        manager,
        auth: auth.clone(),
        commissioning: commissioning.clone(),
        audit: audit.clone(),
    };
    Router::new()
        .route(
            "/api/store-settings",
            get(store_settings_get).put(store_settings_put),
        )
        .route(
            "/api/store-settings/prune-preview",
            post(store_settings_prune_preview),
        )
        .route(
            "/api/store-settings/prune-now",
            post(store_settings_prune_now),
        )
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            RoleGuard {
                auth: auth.clone(),
                commissioning: commissioning.clone(),
                min: Role::Admin,
                resource: "store_settings",
                audit,
            },
            require_role_at_least,
        ))
        .layer(middleware::from_fn_with_state(
            AuthGate {
                auth,
                commissioning,
                operation: OperationKind::Normal,
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
// T21 S2-b: `crate::mcp`の`get_grpc_settings`/`set_grpc_settings`が同じ
// request/response 型・validation を再利用する（`MqttSettingsRequest`等と
// 同じ`pub(crate)`化の理由）。
#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GrpcSettingsBody {
    pub(crate) enabled: bool,
    /// 省略時(`None`)は現在値を維持。指定する場合は `IpAddr` として
    /// 解釈できる文字列(例: `"127.0.0.1"`/`"0.0.0.0"`)である必要がある
    /// - 既定は `crate::settings::DEFAULT_GRPC_BIND`(`"127.0.0.1"`)。
    #[serde(default)]
    pub(crate) bind: Option<String>,
    pub(crate) port: u16,
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
pub(crate) fn validate_grpc_settings_body(body: &GrpcSettingsBody) -> Vec<FieldError> {
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
                operation: OperationKind::Normal,
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
                operation: OperationKind::Normal,
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

/// P3-b（監査指摘 2026-08-12）/ 2026-09-08 オーナー決定: a
/// `PlcConnectionPayload` missing `wordOrder` (an old client, or a
/// create/update that never mentions it) defers to
/// `banto_tags::plc_connection`'s **protocol-aware** default - `"high_low"`
/// for `modbus-tcp`, `"low_high"` for everything else - by handing it the
/// empty-string "unspecified" sentinel rather than a concrete order.
///
/// Returning `"low_high"` here (as this fn did until 2026-09-08) would mask
/// that default entirely: the payload would arrive at
/// `PlcConnectionInput` already filled in, so an omitted `wordOrder` on a
/// Modbus connection could never reach the `"high_low"` default the owner
/// decision requires. `""` is a safe sentinel because it is not in
/// `ALLOWED_WORD_ORDERS` and migration `0010`'s SQL `CHECK` rejects it, so
/// it was never a storable value.
fn default_plc_word_order() -> String {
    String::new()
}

/// T19 S1-b（UX-34、docs/banto-hub-t19-design.md §2）: a
/// `CollectionGroupPayload` missing `defaultWritable` (an old client, or a
/// create/update that never mentions it) keeps the same default as the
/// column itself (`migrations/0012_collection_groups_add_default_writable
/// .sql`'s `DEFAULT 1`, and `banto_tags::collection_group::
/// default_writable_true`) - UX-34's overall "既定 ON" policy, mirrored the
/// same way `default_plc_word_order` mirrors migration `0010`'s column
/// default.
fn default_group_writable() -> bool {
    true
}

fn default_tag_decimals() -> i64 {
    0
}

/// T20 ①a: [`TagPayload::string_encoding`]'s serde default - mirrors
/// `banto_tags::tag`'s own `default_string_encoding()` (private to that
/// crate), so an existing client's payload (written before this field
/// existed) still deserializes and creates a UTF-8-encoded tag, the same
/// "old payload still works" treatment as `writable`/`tag_kind`.
fn default_tag_string_encoding() -> String {
    "utf8".to_string()
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
    /// (`crates/banto-collect/src/simulation.rs`) and, for broker-routed
    /// connections specifically, `crate::broker_glue::BrokerSimRegistry`.
    #[serde(default = "default_plc_simulation")]
    pub simulation: bool,
    /// P3-b（監査指摘 2026-08-12）: ワード順（32bit値 u32/f32 等、および
    /// issue #325 で追加された 64bit 値 i64/u64/f64（4レジスタ）の上位/下位
    /// ワードの並び）。`"low_high"` / `"high_low"` のいずれか - 検証は
    /// `banto_tags::plc_connection::validate_plc_connection_input` 側
    /// （`ALLOWED_WORD_ORDERS`）に委ねる。
    ///
    /// 2026-09-08 オーナー決定: **modbus-tcp でも有効な設定**（既定は
    /// `"high_low"` = Modbus/IEEE 慣習、SLMP は `"low_high"` = MELSEC 標準）。
    /// それ以前は収集経路が Modbus のこの列を無視して `HighLow` 固定で動く
    /// 一方 read-on-demand/書き込み経路は列を読んでおり、同じタグの値が経路
    /// ごとにワード反転して食い違っていた。フォームも "slmp" 選択時しか
    /// 表示していなかったが、現在は modbus-tcp でも表示する。virtual/postgres
    /// 接続では引き続き無意味（`unit_id` と同じ扱い）。
    ///
    /// 省略時は [`default_plc_word_order`] の "unspecified" 番兵を経由して
    /// プロトコル依存の既定が適用される。
    #[serde(default = "default_plc_word_order")]
    pub word_order: String,
    /// S1（docs/banto-hub-external-db-design.md §4.1）: `protocol: "postgres"`
    /// 接続の DB 名。他プロトコルでは省略（`None`）のままにする -
    /// `banto_tags::plc_connection::validate_plc_connection_input`が
    /// postgres 以外での指定を拒否する。
    #[serde(default)]
    pub database: Option<String>,
    /// S1: `protocol: "postgres"` 接続の DB ユーザー名。[`Self::database`]と
    /// 同じ省略時ルール。
    #[serde(default)]
    pub username: Option<String>,
    /// S1: `protocol: "postgres"` 接続のパスワード（平文保存、§2.2）。
    /// **create と update で意味が異なる** -
    /// `banto_tags::PlcConnectionInput::password`の doc comment 参照:
    /// - create: 省略/`null`/空文字列はいずれも「パスワード無し」。
    /// - update: 省略/`null`は「現在のパスワードを変更しない」、空文字列は
    ///   「消去」、それ以外の文字列は「置き換え」。
    ///
    /// GET/list のレスポンスにはこのフィールドは無く、代わりに
    /// [`PlcConnectionResponse::password_set`]（bool）を返す - 平文
    /// パスワードを一度保存した接続を読み返すだけで露出させないため。
    #[serde(default)]
    pub password: Option<String>,
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
            // S1: wired through verbatim - see `PlcConnectionPayload::database`/
            // `::username`/`::password`'s doc comments for the (create vs.
            // update) semantics of `password` specifically.
            database: payload.database,
            username: payload.username,
            password: payload.password,
        }
    }
}

/// S1（docs/banto-hub-external-db-design.md §4.1・§2.2）: GET/list が返す
/// `plc_connections` の読み取り DTO。`banto_tags::PlcConnection`をそのまま
/// `Serialize`すると`password`列（平文）がそのまま応答に出てしまうため、
/// 代わりにこの型へ変換してから返す - `password`は決して外部へ出さず、
/// [`Self::password_set`]（非空パスワードが保存されているかの bool）だけを
/// 返す。
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
    pub simulation: bool,
    pub word_order: String,
    pub database: Option<String>,
    pub username: Option<String>,
    /// S1: true のとき、この接続には非空のパスワードが保存されている
    /// （値そのものは返さない）。
    pub password_set: bool,
}

impl From<PlcConnection> for PlcConnectionResponse {
    fn from(conn: PlcConnection) -> Self {
        let password_set = conn.password.as_deref().is_some_and(|s| !s.is_empty());
        Self {
            id: conn.id,
            name: conn.name,
            protocol: conn.protocol,
            host: conn.host,
            port: conn.port,
            unit_id: conn.unit_id,
            enabled: conn.enabled,
            simulation: conn.simulation,
            word_order: conn.word_order,
            database: conn.database,
            username: conn.username,
            password_set,
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
    /// T19 S1-b（UX-34）: このグループへ新規タグを登録するときの
    /// `writable` チェックボックスの既定値。省略時は `default_group_writable`
    /// （既定 ON、`banto_tags::collection_group::default_writable_true`と
    /// 同じ既定値）。
    #[serde(default = "default_group_writable")]
    pub default_writable: bool,
    /// 外部 DB 連携 S2（docs/banto-hub-external-db-design.md §4.1・§4.2）:
    /// `protocol = "postgres"` の接続配下のグループが1周期ごとに実行する
    /// SELECT 文。`#[serde(default)]`（= `None`）なので、この列を知らない
    /// 既存クライアントの PLC グループ payload は無変更で通る
    /// （`defaultWritable`/`writable`/`tagKind` と同じ後方互換の扱い）。
    /// 必須/禁止の2方向の検証は `banto_tags` 側の `validate_query_sql`。
    #[serde(default)]
    pub query_sql: Option<String>,
}

impl From<CollectionGroupPayload> for CollectionGroupInput {
    fn from(payload: CollectionGroupPayload) -> Self {
        Self {
            name: payload.name,
            plc_connection_id: payload.plc_connection_id,
            period_ms: payload.period_ms,
            enabled: payload.enabled,
            // T19 S1-b: wired through - see `CollectionGroupPayload::
            // default_writable`'s doc comment.
            default_writable: payload.default_writable,
            // 外部 DB 連携 S2: 同上 - `postgres` 接続配下のグループでは必須、
            // それ以外では指定不可（`banto_tags` 側の `validate_query_sql`
            // が両方向を強制する）。
            query_sql: payload.query_sql,
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
    /// T20 ①a（docs/banto-hub-t20-design.md §3.1、2026-09-04 オーナー決定
    /// 「文字コードは既定 UTF-8、タグ単位で Shift-JIS も選択可」）。
    /// `#[serde(default = "default_tag_string_encoding")]`（= `"utf8"`）
    /// なので既存クライアントのペイロードは無変更で通る（`writable`等と
    /// 同じ「旧ペイロード互換」の作法）。管理 UI にエンコーディング選択
    /// フォームを足すのは①aでは任意 - このフィールドは型として通すことを
    /// 優先し、値は API 経由で直接指定できる。
    #[serde(default = "default_tag_string_encoding")]
    pub string_encoding: String,
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
            string_encoding: payload.string_encoding,
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
struct QueuedPendingChangeResponse {
    queued: bool,
    pending: PendingChange,
    status: CollectionStatusResponse,
    message: String,
}

/// S1a レビュー対応（`docs/banto-hub-external-db-design.md`§2.2、
/// `banto_tags::PlcConnection::password`の doc comment「外部呼び出し元へ
/// 決してシリアライズしない」）: `pending_changes.payload`列は
/// `plc_connections.create`/`.update`の`PlcConnectionPayload`をそのまま
/// JSON 化して保存する（`queue_pending_registry_change`/`crate::mcp`の
/// `tool_create_connection`/`tool_update_connection`参照）ため、`password`
/// を含みうる。**DB に保存する値・`decode_pending_payload`/
/// `execute_pending_apply`が適用時に読む値は一切変更しない**（適用には
/// 平文パスワードそのものが必要）- この関数は外部へ返す直前のコピーにだけ
/// 適用する。
///
/// `payload.input.password`を取り除き、代わりに`passwordSet`を挿入する:
/// - `"plc_connections.create"`: `password`が非空文字列なら`true`、
///   省略/`null`/空文字列なら`false`
///   （`banto_tags::plc_connection`の`stored_password_for_create`と同じ
///   「`None`も空文字列も『まだ無し』」の扱い）。
/// - `"plc_connections.update"`: **tri-state** - 非空文字列なら`true`
///   （置き換え）、空文字列なら`false`（消去）、省略/`null`なら`null`
///   （`stored_password_for_update`の「`None`は現在のパスワードを維持」の
///   意味をそのまま反映 - この pending change 自体は「今の保存状態」を
///   持たないので、`true`/`false`のどちらでもなく`null`とする。現在値は
///   `GET /api/plc-connections/{id}`の`passwordSet`で別途確認できる）。
///
/// それ以外の`source`（`"plc_connections.delete"`、`collection_groups.*`、
/// `tags.*`）は無変更でそのまま返す - password を持ちうるのは
/// `plc_connections.create`/`.update`の2つだけ。
fn redact_pending_payload(source: &str, payload: &serde_json::Value) -> serde_json::Value {
    if source != "plc_connections.create" && source != "plc_connections.update" {
        return payload.clone();
    }
    let mut payload = payload.clone();
    let Some(input) = payload.get_mut("input").and_then(|v| v.as_object_mut()) else {
        return payload;
    };
    let password_set = match input.remove("password") {
        None | Some(serde_json::Value::Null) => {
            if source == "plc_connections.create" {
                serde_json::Value::Bool(false)
            } else {
                serde_json::Value::Null
            }
        }
        Some(serde_json::Value::String(s)) => serde_json::Value::Bool(!s.is_empty()),
        // 文字列でない不正な値が来た場合(本来は起こらない)も、少なくとも
        // 平文パスワードそのものを外に漏らさないことだけは保つ。
        Some(_) => serde_json::Value::Bool(false),
    };
    input.insert("passwordSet".to_string(), password_set);
    payload
}

/// [`redact_pending_payload`]を`PendingChange`全体に適用したコピーを返す -
/// list/get/cancel/requeue/apply/enqueue のあらゆる応答が、DB の生の行では
/// なく必ずこの関数を通した後の値を返すための唯一の入口。
fn redact_pending_change_for_response(pending: PendingChange) -> PendingChange {
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

async fn queue_pending_registry_change(
    state: &TagRegistryState,
    headers: &HeaderMap,
    source: &str,
    payload: serde_json::Value,
    status: CollectionStatus,
) -> RegistryMutationResult<Response> {
    let identity = actor_identity(headers, &state.auth, &state.commissioning);
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

enum RegistryMutationError {
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

type RegistryMutationResult<T> = Result<T, RegistryMutationError>;

// #341（2026-09-14）: `require_collection_stopped`（収集中なら一律 409
// `collection_edit_locked`）はここにあったが撤去した。従来は queue 分岐の
// 直後にあって到達不能だったところ、[`registry_change_should_queue`] の
// 新しい条件では「試運転中 + 収集中」がここへ到達してしまい、即時反映の
// 決定と真っ向から矛盾するため残せない。
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
    Ok(Json(state.plc_connections.get(id).await?.into()))
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
    if let Some(status) = registry_change_should_queue(&state.controller, &state.commissioning) {
        return queue_pending_registry_change(
            &state,
            &headers,
            "plc_connections.create",
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
    let created = match state.plc_connections.create_tx(&mut tx, input.into()).await {
        Ok(created) => created,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(ApiError(err).into());
        }
    };
    // #341 レビュー対応: この snapshot は**保存前検証（preflight）専用**。
    // catalog へ反映するのは `CollectionController::commit_catalog_and_apply_live`
    // が transition ロックの下で読み直す最新の snapshot（同 fn の doc
    // comment「古い snapshot の窓」節）。
    let _preflighted = match preflight_transaction(&mut tx).await {
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
    commit_catalog_and_notify(&state.controller, &state.events, "plc_connections")
        .await
        .map_err(RegistryMutationError::LiveApplyFailed)?;
    Ok(Json(PlcConnectionResponse::from(created)).into_response())
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
    if let Some(status) = registry_change_should_queue(&state.controller, &state.commissioning) {
        return queue_pending_registry_change(
            &state,
            &headers,
            "plc_connections.update",
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
    // #341 レビュー対応: この snapshot は**保存前検証（preflight）専用**。
    // catalog へ反映するのは `CollectionController::commit_catalog_and_apply_live`
    // が transition ロックの下で読み直す最新の snapshot（同 fn の doc
    // comment「古い snapshot の窓」節）。
    let _preflighted = match preflight_transaction(&mut tx).await {
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
    commit_catalog_and_notify(&state.controller, &state.events, "plc_connections")
        .await
        .map_err(RegistryMutationError::LiveApplyFailed)?;
    Ok(Json(PlcConnectionResponse::from(updated)).into_response())
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
    if let Some(status) = registry_change_should_queue(&state.controller, &state.commissioning) {
        return queue_pending_registry_change(
            &state,
            &headers,
            "plc_connections.delete",
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
    // 外部 DB 連携 S4（docs/banto-hub-external-db-design.md §5.2、
    // `crate::sink`のモジュール doc comment「FK を張らない理由」参照）:
    // この接続を参照する sink group が1件でもあれば削除を拒否する
    // （RESTRICT 相当）。`banto_tags`はこの接続の下にある Hub 専用
    // テーブルを知らないため、`cascade_delete_tx`を呼ぶ**前**にここで
    // チェックする。
    if let Err(err) = reject_delete_if_referenced_by_sink_group(&mut tx, id).await {
        let _ = tx.rollback().await;
        return Err(ApiError(err).into());
    }
    // T19 S2-b（UX-38、docs/banto-hub-t19-design.md §3.4・§7.5、2026-09-02
    // オーナー決定）: 「タグがあってもグループ・接続を削除可。定義のみ削除
    // し、履歴は残す」- `delete_tx`（子が居れば拒否）ではなく
    // `cascade_delete_tx`（配下のグループ・タグごと1トランザクションで削除、
    // `banto_tags::plc_connection::PlcConnectionService::cascade_delete_tx`
    // の doc comment参照）を使う。`banto-tstore` の履歴データはこのクレート
    // が触らない別ストレージなので無傷のまま残る。予約接続（calc/mem）は
    // このカスケード経路でも従来どおり削除できない。
    let cascade = match state.plc_connections.cascade_delete_tx(&mut tx, id).await {
        Ok(cascade) => cascade,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(ApiError(err).into());
        }
    };
    // #341 レビュー対応: この snapshot は**保存前検証（preflight）専用**。
    // catalog へ反映するのは `CollectionController::commit_catalog_and_apply_live`
    // が transition ロックの下で読み直す最新の snapshot（同 fn の doc
    // comment「古い snapshot の窓」節）。
    let _preflighted = match preflight_transaction(&mut tx).await {
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
        Some(json!({
            "cascade": {
                "deletedGroups": cascade.deleted_groups,
                "deletedTags": cascade.deleted_tags,
            },
        })),
    )
    .await;
    commit_catalog_and_notify(&state.controller, &state.events, "plc_connections")
        .await
        .map_err(RegistryMutationError::LiveApplyFailed)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `POST /api/plc-connections/{id}/test` - S1a
/// （docs/banto-hub-external-db-design.md §4.6・§7 スライス表「S1」:
/// 「接続テストが `SELECT 1`〈実際は `SELECT version()`〉と列一覧を
/// 返す」の PostgreSQL 側実装。**保存済み**の接続 `id` を対象にする点で
/// T12 の `POST /api/plc-connections/test`（保存前のフォーム値を直接
/// 受け取る、PLC 専用）とは別物 - 名前を分けて共存させている
/// (`plc_connections_test`が既存、こちらは`plc_connections_test_saved`)。
///
/// 認可は他の `plc_connections` 書き込みハンドラ（create/update/delete）と
/// 同じ`require_editor`に揃える - このリソースの REST 面は既に role
/// ベースの editor ゲートで統一されており（T21 の MCP 側だけが admin
/// スコープを使う別体系）、ここだけ新しいゲートを持ち込まない。
///
/// S1a では `protocol == "postgres"` の接続のみ対応する
/// （[`banto_tags::PlcConnection::is_db_source`]）。それ以外は`400`で
/// 「S1では postgres のみ対応」と明示する - `ok:false`の200ではなく
/// 明確なリクエストエラーにする（T12の`test_connection`が`virtual`/
/// `simulation`を`ok:false`の200で表現するのとは意図的に区別: あちらは
/// 「テスト対象として意味を成さないが正常な入力」、こちらは「この
/// エンドポイントがそもそも対応しないプロトコル」）。
///
/// レジストリへの書き込みは一切発生しない読み取り専用の疎通確認なので、
/// `record_write`/`commit_catalog_and_notify`は呼ばない -
/// `plc_connections_test`（T12）と同じ判断。
async fn plc_connections_test_saved(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Response, ApiError> {
    require_editor(
        &state.auth,
        &state.commissioning,
        &state.audit,
        &headers,
        "plc_connections",
        "POST",
        "/api/plc-connections/{id}/test",
    )
    .await?;

    let conn = state.plc_connections.get(id).await?;
    if !conn.is_db_source() {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "message": "S1ではpostgres接続のみ接続テストに対応しています。",
            })),
        )
            .into_response());
    }

    let outcome = crate::db_source::test_connection(&conn).await;
    Ok((StatusCode::OK, Json(outcome)).into_response())
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
// 重要な制約(実機 R08ENCPU・オムロン KM-D1-ETN、`crates/banto-broker/src/lib.rs`
// のモジュール doc、`crate::broker_glue`のモジュール doc「Session sync
// policy」節参照): 三菱 SLMP は対象ポートが既に別の接続で使用中だと同じ
// ポートへの2本目を受け付けない(2026-08-07 実機確認: ポート毎に1接続、CPU
// 側で複数ポートを開けていれば複数同時セッションは可能)。Modbus TCP も
// #337 で収集読み取りが broker セッションに相乗りするようになった結果、
// KM-D1-ETN(KANC-718B 12-1)のような1対1接続しか受け付けない機器では同じ
// 制約が生じる(2026-09-08 オーナー決定)。そのため、保存済み接続
// (`connectionId`あり)のテストはプロトコルを問わず、既存の broker
// セッションが生きていればそれを再利用して読み、無い場合のみ直接ダイヤル
// する([`test_slmp_connection`]/[`test_modbus_connection`]参照)。

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

/// Modbus TCP の接続テスト。`payload.connection_id`があり、その接続の broker
/// セッションが既に生きていれば、それを再利用して読む(新規ダイヤルしない -
/// [`test_slmp_connection`]と同じ対策)。無ければ直接ダイヤルにフォールバック
/// する(ポート/ユニットIDの範囲検証 → `ModbusTcpClient::connect` → 保持
/// レジスタ先頭1点の`read_batch` → 必ず`disconnect`、の順)。
///
/// 2026-09-08 オーナー決定: #337 で Modbus TCP の収集読み取りも broker
/// セッションへ相乗りするようになり、Modbus も1接続=1ソケットになった。
/// SLMP の R08ENCPU と同様、1対1接続しか受け付けない機器(オムロン
/// KM-D1-ETN、KANC-718B 12-1、#337 参照)では、収集稼働中に接続テストを
/// 押すと2本目のダイヤルが拒否され、収集は正常なのに「失敗」と誤診断
/// されていた。SLMP と同じセッション再利用へ揃えてこれを解消する。
async fn test_modbus_connection(
    manager: &CollectorManager,
    payload: &PlcConnectionTestPayload,
) -> (bool, Option<PlcConnectionTestError>) {
    if let Some(connection_id) = payload.connection_id {
        if let Some(handle) = manager.sessions().handle_for(connection_id) {
            let requests = vec![BatchReadRequest::Numeric(ReadRequest {
                address: Address::parse("40001").expect("valid literal"),
                data_type: DataType::U16,
            })];
            // `ReadOnlyHandle::read`自体には外側タイムアウトが無いため、
            // ここで明示的に包む(`test_slmp_connection`と同じ)。
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
    manager: &CollectorManager,
    payload: &PlcConnectionTestPayload,
) -> (bool, Option<PlcConnectionTestError>) {
    if let Some(connection_id) = payload.connection_id {
        if let Some(handle) = manager.sessions().handle_for(connection_id) {
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
///
/// 疎通確認そのもの(virtual/simulation の即時拒否・プロトコル別ダイヤル・
/// 所要時間計測)は[`run_plc_connection_test`]へ切り出してある - T21
/// S1-c（docs/banto-hub-t21-design.md）の MCP `test_connection`ツール
/// ([`crate::mcp`]の`tool_test_connection`)がこの REST ハンドラと**全く同じ
/// 疎通確認**を行うために共有する(二重実装しない、このファイル冒頭
/// 「§3.7」節と同じ規律)。
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

    Ok(Json(
        run_plc_connection_test(&state.manager, &payload).await,
    ))
}

/// [`plc_connections_test`]のdoc comment参照 - REST ハンドラと MCP の
/// `test_connection`ツールが共有する疎通確認本体。認可判定
/// (`require_editor`/MCP 側の`require_admin_scope`)は呼び出し元の責務で、
/// ここでは行わない。
pub(crate) async fn run_plc_connection_test(
    manager: &CollectorManager,
    payload: &PlcConnectionTestPayload,
) -> PlcConnectionTestResponse {
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
            "modbus-tcp" => test_modbus_connection(manager, payload).await,
            "slmp" => test_slmp_connection(manager, payload).await,
            other => (
                false,
                Some(PlcConnectionTestError {
                    kind: "unsupported".to_string(),
                    message: format!("不明なプロトコルです: {other}"),
                }),
            ),
        }
    };

    PlcConnectionTestResponse {
        ok,
        elapsed_ms: started.elapsed().as_millis() as u64,
        error,
    }
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

/// `POST /api/collection-groups/{id}/describe` - S3
/// (docs/banto-hub-external-db-design.md §4.6・§7 スライス表「S3: 列名候補の
/// 提示」): 保存済みグループの `query_sql` を対象に、`db_source::describe_group`
/// （5秒タイムアウトの `describe` 1回、実行はしない）を呼び、列名・
/// PostgreSQL型・v1 対応可否を返す。タグ登録 UI の「列を取得」ボタンが
/// 「結果列名」候補（`db` タグの `address`）を提示するために呼ぶ。
///
/// 認可は保存済み接続の接続テスト（[`plc_connections_test_saved`]）と
/// 同じ `require_editor` に揃える - こちらも DB へ接続してクエリ（読み取り
/// 専用の `describe`）を実行する操作である点は接続テストと同じ性質のため。
///
/// 対象グループが `protocol == "postgres"` の接続配下でない場合、または
/// （通常到達しない防御として）`query_sql` が `None` の場合は `400` で
/// 明示する - `plc_connections_test_saved` が非 postgres を `400` にする
/// のと同じ判断（`ok: false` の `200` ではなく、このエンドポイントが
/// そもそも対応しない入力であることを明確にする）。
///
/// レジストリへの書き込みは一切発生しない読み取り専用の疎通確認なので、
/// `record_write`/`commit_catalog_and_notify` は呼ばない -
/// `plc_connections_test_saved` と同じ判断。
async fn collection_groups_describe(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Response, ApiError> {
    require_editor(
        &state.auth,
        &state.commissioning,
        &state.audit,
        &headers,
        "collection_groups",
        "POST",
        "/api/collection-groups/{id}/describe",
    )
    .await?;

    let group = state.collection_groups.get(id).await?;
    let conn = state.plc_connections.get(group.plc_connection_id).await?;
    if !conn.is_db_source() {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "message": "postgres接続配下のグループのみdescribeに対応しています。",
            })),
        )
            .into_response());
    }
    let Some(query_sql) = group.query_sql.as_deref() else {
        // 通常到達しない: postgres 配下のグループは `validate_query_sql`
        // （crates/banto-tags/src/collection_group.rs）が `query_sql` を
        // 必須にしている（§4.1）。念のための防御。
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "message": "このグループにはquerySqlが設定されていません。",
            })),
        )
            .into_response());
    };

    let outcome = crate::db_source::describe_group(&conn, query_sql).await;
    Ok((StatusCode::OK, Json(outcome)).into_response())
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
    if let Some(status) = registry_change_should_queue(&state.controller, &state.commissioning) {
        return queue_pending_registry_change(
            &state,
            &headers,
            "collection_groups.create",
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
    // #341 レビュー対応: この snapshot は**保存前検証（preflight）専用**。
    // catalog へ反映するのは `CollectionController::commit_catalog_and_apply_live`
    // が transition ロックの下で読み直す最新の snapshot（同 fn の doc
    // comment「古い snapshot の窓」節）。
    let _preflighted = match preflight_transaction(&mut tx).await {
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
    commit_catalog_and_notify(&state.controller, &state.events, "collection_groups")
        .await
        .map_err(RegistryMutationError::LiveApplyFailed)?;
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
    if let Some(status) = registry_change_should_queue(&state.controller, &state.commissioning) {
        return queue_pending_registry_change(
            &state,
            &headers,
            "collection_groups.update",
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
    // #341 レビュー対応: この snapshot は**保存前検証（preflight）専用**。
    // catalog へ反映するのは `CollectionController::commit_catalog_and_apply_live`
    // が transition ロックの下で読み直す最新の snapshot（同 fn の doc
    // comment「古い snapshot の窓」節）。
    let _preflighted = match preflight_transaction(&mut tx).await {
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
    commit_catalog_and_notify(&state.controller, &state.events, "collection_groups")
        .await
        .map_err(RegistryMutationError::LiveApplyFailed)?;
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
    if let Some(status) = registry_change_should_queue(&state.controller, &state.commissioning) {
        return queue_pending_registry_change(
            &state,
            &headers,
            "collection_groups.delete",
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
    // T19 S2-b（UX-38）: `plc_connections_delete` と同じ理由・同じ形で
    // `cascade_delete_tx` を使う（属するタグごと1トランザクションで削除、
    // `banto_tags::collection_group::CollectionGroupService::cascade_delete_tx`
    // の doc comment参照）。
    let cascade = match state.collection_groups.cascade_delete_tx(&mut tx, id).await {
        Ok(cascade) => cascade,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(ApiError(err).into());
        }
    };
    // #341 レビュー対応: この snapshot は**保存前検証（preflight）専用**。
    // catalog へ反映するのは `CollectionController::commit_catalog_and_apply_live`
    // が transition ロックの下で読み直す最新の snapshot（同 fn の doc
    // comment「古い snapshot の窓」節）。
    let _preflighted = match preflight_transaction(&mut tx).await {
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
        Some(json!({
            "cascade": { "deletedTags": cascade.deleted_tags },
        })),
    )
    .await;
    commit_catalog_and_notify(&state.controller, &state.events, "collection_groups")
        .await
        .map_err(RegistryMutationError::LiveApplyFailed)?;
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

// --- #342 段階A: 演算タグの式チェック API（保存前検証・エラー位置・プレビュー）
// -----------------------------------------------------------------------
//
// issue #342 は「`banto_expr::typecheck::check` をそのまま呼び、参照タグの
// 型は現行 catalog から解決する」前提で書かれていたが、これはコード事実と
// 食い違う: `typecheck` は `banto-expr` の非公開モジュール（`lib.rs` の
// `mod typecheck;`）で、公開 API は `compile(source) -> Result<CompiledExpr,
// CompileError>` だけ。しかも `banto-expr` はレジストリを持たない設計で、
// タグ参照の型は常に `Type::Num` 固定（`crates/banto-expr/src/lib.rs` 冒頭
// doc「タグ参照は常に Num 型」参照）なので「参照タグの型解決を渡す」引数
// 自体が存在しない。そのためここでは `compile()` を呼び、
// `CompiledExpr::referenced_tags()` を `TagMap` と突き合わせる**2段構成**に
// する - `crate::computed::build_plan` が既にこの形（`resolve_referenced_tag`
// を両者で共有 - 判定条件を新しく発明しない）。`build_plan` 自体は呼ばない:
// あれは登録済み computed タグ全件の rebuild 検証用で、単発式のプレビューには
// 過剰（未登録の全タグを毎回コンパイルし直す）なうえ `String` エラーしか
// 返さない。

/// `POST /api/tags/expression/check` の body。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExpressionCheckRequest {
    expression: String,
    /// この式を保存する予定のタグの完全名（省略可）。循環参照の判定にだけ
    /// 使う（[`cycle_check_nodes`] 参照）。
    #[serde(default)]
    external_name: Option<String>,
}

/// 参照タグ1件の要約（コンパイル成功後、存在確認・文字列タグ拒否を通った
/// ものだけが入る）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExpressionRefEntry {
    name: String,
    data_type: String,
    unit: Option<String>,
    tag_kind: String,
}

/// 現在値による試算結果 - `evaluated: false` のときは `value: null` で
/// `reason` に日本語の理由（参照が Bad/Stale・`eval` 失敗のいずれか）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExpressionPreview {
    value: Option<f64>,
    evaluated: bool,
    reason: Option<String>,
}

/// `banto_expr::CompileError` の全 variant 名を写した `kind` 文字列 - `pos`
/// は `CompileError::SourceTooLong` だけ持たない（バイトオフセット =
/// 文字オフセット、`crates/banto-expr/src/error.rs` 冒頭のコメント参照）。
/// `message` は `CompileError`/`CycleError` の `Display`（既に日本語）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExpressionCheckError {
    kind: String,
    pos: Option<usize>,
    message: String,
}

/// `POST /api/tags/expression/check` の応答。**常に 200** - 式が不正でも
/// `ok: false` で返す（保存 API ではなくプレビュー用のため、HTTP エラーに
/// しない。#342 実装指示「レスポンス」節）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ExpressionCheckResponse {
    ok: bool,
    result_type: Option<String>,
    refs: Vec<ExpressionRefEntry>,
    preview: ExpressionPreview,
    error: Option<ExpressionCheckError>,
}

impl ExpressionCheckResponse {
    /// 参照解決・循環検査で失敗したときの共通ショートカット - コンパイル
    /// 自体は成功しているので `result_type`/`refs`（そこまでに解決できた分）
    /// は埋めたまま返す（「ok: false のときは resultType/refs/preview を
    /// 可能な範囲で埋める」の実装）。
    fn resolution_error(
        result_type: Option<String>,
        refs: Vec<ExpressionRefEntry>,
        kind: &'static str,
        message: String,
    ) -> Self {
        Self {
            ok: false,
            result_type,
            refs,
            preview: ExpressionPreview {
                value: None,
                evaluated: false,
                reason: None,
            },
            error: Some(ExpressionCheckError {
                kind: kind.to_string(),
                pos: None,
                message,
            }),
        }
    }
}

fn expression_result_type_wire(ty: banto_expr::Type) -> String {
    match ty {
        banto_expr::Type::Num => "num".to_string(),
        banto_expr::Type::Bool => "bool".to_string(),
    }
}

/// `banto_expr::CompileError` → [`ExpressionCheckError`]。全 variant を
/// 網羅（`match` に `_` を置かない - 新しい variant が増えたらコンパイル
/// エラーで気づけるようにする）。
fn expression_compile_error(err: &banto_expr::CompileError) -> ExpressionCheckError {
    use banto_expr::CompileError;
    let message = err.to_string();
    let (kind, pos) = match err {
        CompileError::Syntax { pos, .. } => ("syntax", Some(*pos)),
        CompileError::TypeMismatch { pos, .. } => ("type_mismatch", Some(*pos)),
        CompileError::UnknownFunction { pos, .. } => ("unknown_function", Some(*pos)),
        CompileError::ArityMismatch { pos, .. } => ("arity_mismatch", Some(*pos)),
        CompileError::BadBitIndex { pos, .. } => ("bad_bit_index", Some(*pos)),
        CompileError::BadBitTarget { pos, .. } => ("bad_bit_target", Some(*pos)),
        // 唯一 `pos` を持たない variant - 式全体の長さについてのエラーで、
        // 特定の文字位置を指す意味がないため（`error.rs` の doc comment）。
        CompileError::SourceTooLong { .. } => ("source_too_long", None),
        CompileError::TooDeep { pos, .. } => ("too_deep", Some(*pos)),
    };
    ExpressionCheckError {
        kind: kind.to_string(),
        pos,
        message,
    }
}

/// 循環参照の判定に使うノード集合を組み立てる（#342 実装指示「循環参照の
/// 判定」節）。現在の `TagMap` の computed タグそれぞれについて、既に
/// commit 済みの [`ComputedEngine`] の plan から参照先一覧を引く
/// （`build_plan` を呼び直して全タグを再コンパイルしない - 既存タグは
/// 登録時に検証済みで、この plan がその正）。`external_name` のノードだけ
/// 今回の式の参照先（`new_refs`）で置き換える - 存在しなければ追加する。
fn cycle_check_nodes(
    map: &TagMap,
    computed: &ComputedEngine,
    external_name: &str,
    new_refs: &[String],
) -> Vec<(String, Vec<String>)> {
    let mut nodes: Vec<(String, Vec<String>)> = Vec::new();
    let mut replaced = false;
    for entry in map.iter() {
        if entry.tag_kind != COMPUTED_TAG_KIND {
            continue;
        }
        if entry.external_name == external_name {
            nodes.push((external_name.to_string(), new_refs.to_vec()));
            replaced = true;
        } else {
            let deps = computed
                .referenced_tags(&entry.external_name)
                .unwrap_or_default();
            nodes.push((entry.external_name.clone(), deps));
        }
    }
    if !replaced {
        nodes.push((external_name.to_string(), new_refs.to_vec()));
    }
    nodes
}

/// REST ハンドラ（[`tags_expression_check`]）と MCP ツール（`crate::mcp` の
/// `check_expression`）が共有する検証本体。認可・pending queue 判定は行わ
/// ない（呼び出し側の責務 - REST は `require_editor`、MCP は
/// `require_admin_scope`）。同期処理のみ（DB I/O も await もない）なので
/// 非同期にしていない - 呼び出し元が async 関数であっても素通しで呼べる。
pub(crate) fn evaluate_expression_check(
    manager: &CollectorManager,
    expression: &str,
    external_name: Option<&str>,
) -> ExpressionCheckResponse {
    let compiled = match banto_expr::compile(expression) {
        Ok(compiled) => compiled,
        Err(err) => {
            return ExpressionCheckResponse {
                ok: false,
                result_type: None,
                refs: Vec::new(),
                preview: ExpressionPreview {
                    value: None,
                    evaluated: false,
                    reason: None,
                },
                error: Some(expression_compile_error(&err)),
            };
        }
    };

    let result_type = Some(expression_result_type_wire(compiled.result_type()));
    let map = manager.tag_map();

    // 参照タグの解決（存在確認・文字列タグ拒否) - `build_plan` と同じ判定を
    // `crate::computed::resolve_referenced_tag` で共有する。
    let mut refs = Vec::with_capacity(compiled.referenced_tags().len());
    for name in compiled.referenced_tags() {
        match crate::computed::resolve_referenced_tag(&map, name) {
            Ok(entry) => refs.push(ExpressionRefEntry {
                name: entry.external_name.clone(),
                data_type: entry.data_type.clone(),
                unit: entry.unit.clone(),
                tag_kind: entry.tag_kind.clone(),
            }),
            Err(crate::computed::ReferencedTagError::Missing) => {
                return ExpressionCheckResponse::resolution_error(
                    result_type,
                    refs,
                    "unknown_tag",
                    format!("参照先タグが存在しません: {name}"),
                );
            }
            Err(crate::computed::ReferencedTagError::StringType) => {
                return ExpressionCheckResponse::resolution_error(
                    result_type,
                    refs,
                    "string_ref",
                    format!("文字列タグは参照できません: {name}"),
                );
            }
        }
    }

    // 循環参照の判定（externalName が与えられたときだけ）。
    if let Some(ext_name) = external_name {
        let computed = manager.computed_engine();
        let nodes = cycle_check_nodes(&map, &computed, ext_name, compiled.referenced_tags());
        if let Err(cycle_err) = banto_expr::validate_dag(&nodes) {
            return ExpressionCheckResponse::resolution_error(
                result_type,
                refs,
                "cycle",
                cycle_err.to_string(),
            );
        }
    }

    // プレビュー試算: 参照が全件 Good で値を持つときだけ評価する。
    let now_ms = manager.clock().now_ms();
    let current = manager.current_values();
    let server_store = manager.server_store();
    let mut inputs: Vec<(String, banto_expr::Value)> =
        Vec::with_capacity(compiled.referenced_tags().len());
    let mut blocked_reason: Option<String> = None;
    for name in compiled.referenced_tags() {
        // 上のループで解決済み（存在確認・文字列タグ拒否を通過済み）。
        let entry = map
            .get(name)
            .expect("#342: referenced tag resolved above via resolve_referenced_tag");
        let (value, quality, _t) =
            crate::hub::read_current(entry, current.as_ref(), &server_store, now_ms);
        if blocked_reason.is_some() {
            continue;
        }
        match (value, quality) {
            (Some(v), Quality::Good) => inputs.push((name.clone(), banto_expr::Value::Num(v))),
            (Some(_), Quality::Stale) => {
                blocked_reason = Some(format!("参照タグ {name} の値が Stale です"));
            }
            _ => {
                blocked_reason = Some(format!("参照タグ {name} の値が Bad です"));
            }
        }
    }

    let preview = match blocked_reason {
        Some(reason) => ExpressionPreview {
            value: None,
            evaluated: false,
            reason: Some(reason),
        },
        None => {
            let closure = |tag: &str| inputs.iter().find(|(n, _)| n == tag).map(|(_, v)| *v);
            match compiled.eval(&closure) {
                Ok(banto_expr::Value::Num(x)) => ExpressionPreview {
                    value: Some(x),
                    evaluated: true,
                    reason: None,
                },
                // 結果型 Bool は `computed.rs::evaluate_tick` と同じ規約で
                // 1.0/0.0 に変換する（I1 に bool データ型はない）。
                Ok(banto_expr::Value::Bool(b)) => ExpressionPreview {
                    value: Some(if b { 1.0 } else { 0.0 }),
                    evaluated: true,
                    reason: None,
                },
                Err(err) => ExpressionPreview {
                    value: None,
                    evaluated: false,
                    reason: Some(err.to_string()),
                },
            }
        }
    };

    ExpressionCheckResponse {
        ok: true,
        result_type,
        refs,
        preview,
        error: None,
    }
}

/// `plc_connections_test`と同じ理由で意図的に `#[utoipa::path]` を付けず
/// `ApiDoc` にも加えない（`tags_address_writable` の doc comment参照 -
/// `/api/v1/*` のみを文書化する既存方針。このエンドポイントも同じ
/// `tag_registry_router`＝管理系ルーター配下）。
async fn tags_expression_check(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Json(input): Json<ExpressionCheckRequest>,
) -> Result<Json<ExpressionCheckResponse>, ApiError> {
    require_editor(
        &state.auth,
        &state.commissioning,
        &state.audit,
        &headers,
        "tags",
        "POST",
        "/api/tags/expression/check",
    )
    .await
    .map_err(ApiError)?;
    Ok(Json(evaluate_expression_check(
        &state.manager,
        &input.expression,
        input.external_name.as_deref(),
    )))
}

// --- #342 段階B: 組み込み関数表の配布（式欄のセグメント補完用） ------------

/// `GET /api/tags/expression/functions` の応答要素。`banto_expr::
/// BuiltinFunction` をそのまま wire 形（camelCase）へ写す。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExpressionFunctionEntry {
    name: String,
    arity: usize,
    /// 補完候補に出す呼び出し形の見本（例: `if(条件, 真のとき, 偽のとき)`）。
    signature: String,
    /// 補完候補に出す1行の日本語説明。
    description: String,
}

/// `GET /api/tags/expression/functions` の応答。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExpressionFunctionsResponse {
    functions: Vec<ExpressionFunctionEntry>,
}

/// `GET /api/tags/expression/functions`（#342 段階B）: 式欄のセグメント補完に
/// 出す**組み込み関数の一覧**を返す。
///
/// **なぜ API で配るのか**: 関数名と引数個数の正は
/// `banto_expr::BUILTIN_FUNCTIONS`（型検査の `check_call` が読むのと同じ表）
/// 1箇所だけにしたい。TS からは Rust の const を import できないので、
/// 「フロントに関数表を手書きする」（＝2つ目の表を作ってドリフトさせる）
/// 代わりにサーバーが配る。**内容は静的**（DB も設定も読まない）なので
/// クライアントは1回取得してキャッシュしてよい。
///
/// 認可は `POST /api/tags/expression/check` と同じ `require_editor` -
/// 式欄を触れるのは編集権限のあるユーザーだけで、補完もその式欄の付属機能
/// だから（読み取り専用の内容ではあるが、ゲートを分けて2種類にしない）。
/// MCP へは足さない（補完は UI 専用、#342 段階B 実装指示）。
///
/// `plc_connections_test`/`tags_expression_check` と同じ理由で意図的に
/// `#[utoipa::path]` を付けず `ApiDoc` にも加えない（`/api/v1/*` のみを
/// 文書化する既存方針）。
async fn tags_expression_functions(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
) -> Result<Json<ExpressionFunctionsResponse>, ApiError> {
    require_editor(
        &state.auth,
        &state.commissioning,
        &state.audit,
        &headers,
        "tags",
        "GET",
        "/api/tags/expression/functions",
    )
    .await
    .map_err(ApiError)?;
    Ok(Json(ExpressionFunctionsResponse {
        functions: banto_expr::BUILTIN_FUNCTIONS
            .iter()
            .map(|f| ExpressionFunctionEntry {
                name: f.name.to_string(),
                arity: f.arity,
                signature: f.signature.to_string(),
                description: f.description.to_string(),
            })
            .collect(),
    }))
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
    if let Some(status) = registry_change_should_queue(&state.controller, &state.commissioning) {
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
    // #341 レビュー対応: この snapshot は**保存前検証（preflight）専用**。
    // catalog へ反映するのは `CollectionController::commit_catalog_and_apply_live`
    // が transition ロックの下で読み直す最新の snapshot（同 fn の doc
    // comment「古い snapshot の窓」節）。
    let _preflighted = match preflight_transaction(&mut tx).await {
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
    commit_catalog_and_notify(&state.controller, &state.events, "tags")
        .await
        .map_err(RegistryMutationError::LiveApplyFailed)?;
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
    if let Some(status) = registry_change_should_queue(&state.controller, &state.commissioning) {
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
    // #341 レビュー対応: この snapshot は**保存前検証（preflight）専用**。
    // catalog へ反映するのは `CollectionController::commit_catalog_and_apply_live`
    // が transition ロックの下で読み直す最新の snapshot（同 fn の doc
    // comment「古い snapshot の窓」節）。
    let _preflighted = match preflight_transaction(&mut tx).await {
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
    commit_catalog_and_notify(&state.controller, &state.events, "tags")
        .await
        .map_err(RegistryMutationError::LiveApplyFailed)?;
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
    if let Some(status) = registry_change_should_queue(&state.controller, &state.commissioning) {
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
    // 外部 DB 連携 S4（`crate::sink`のモジュール doc comment「FK を張らない
    // 理由」参照）: `hub_sink_group_tags`は`tags(id)`へ FK を張っていない
    // ため、単体タグ削除の経路では能動的にメンバーシップ行を消す。
    if let Err(err) = remove_tag_from_all_sink_groups(&mut tx, id).await {
        let _ = tx.rollback().await;
        return Err(ApiError(err).into());
    }
    // #341 レビュー対応: この snapshot は**保存前検証（preflight）専用**。
    // catalog へ反映するのは `CollectionController::commit_catalog_and_apply_live`
    // が transition ロックの下で読み直す最新の snapshot（同 fn の doc
    // comment「古い snapshot の窓」節）。
    let _preflighted = match preflight_transaction(&mut tx).await {
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
    commit_catalog_and_notify(&state.controller, &state.events, "tags")
        .await
        .map_err(RegistryMutationError::LiveApplyFailed)?;
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

/// T19 S2-c1: [`PendingBatchTagsPayload`]の一括削除版 - `tags.batch_delete`
/// 経由でキューされた pending change を [`execute_pending_apply`] が
/// デコードする際の形。一括削除に dry run は無いので、更新/作成版と違い
/// `dry_run` フィールドを持たない([`BatchTagsDeleteRequest`]と同型)。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PendingBatchTagsDeletePayload {
    ids: Vec<i64>,
}

enum PendingApplyError {
    Api(ApiError),
    /// #341（2026-09-14 オーナー回答）: 適用（DB への書き込み）は成功した
    /// が、その内容を catalog / 走行中の収集へ反映できなかった
    /// （[`commit_catalog_and_notify`] の `Err`）。`Api`/`Conflict` と違い
    /// **pending change は `applied` へ遷移させる**（レジストリ行は既に
    /// コミット済みで、`failed` にして再適用させると二重適用になる） -
    /// [`pending_changes_apply`] がこの1件だけ特別扱いしている。
    LiveApplyFailed(String),
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
            Self::LiveApplyFailed(message) => {
                format!(
                    "変更は保存しましたが、実行構成への反映に失敗しました: {message}{LIVE_APPLY_RECOVERY_HINT}"
                )
            }
            Self::Conflict { resource } => pending_apply_conflict_message(resource),
        }
    }
}

impl IntoResponse for PendingApplyError {
    fn into_response(self) -> Response {
        match self {
            Self::Api(err) => err.into_response(),
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

/// #341（2026-09-14 オーナー回答「明示適用も無停止」）: かつてここには
/// `CollectionState::Stopped` 要求（収集中は 409 `collection_edit_locked`）が
/// あったが撤去した。ロックダウン済みで求められるワンクッションは
/// 「pending queue + 人による明示適用」そのものであり、適用のために収集を
/// 止める技術的な理由は T7（`Collector::apply_config` による接続単位の
/// 部分再構成、二重接続窓の解消）以降もう無い - 反映は他の全 mutation と
/// 同じく [`commit_catalog_and_notify`] に任せる（走行中なら
/// [`CollectionController::commit_catalog_and_apply_live`] が無停止で
/// 実行構成まで更新する）。per-resource フィンガープリントガード・
/// キャンセル・requeue の導線はそのまま維持している。
async fn execute_pending_apply(
    state: &PendingChangesAdminState,
    pending: &PendingChange,
) -> Result<(), PendingApplyError> {
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
                // S1: compare against the same redacted DTO
                // `compute_pending_base_fingerprint` used at enqueue time
                // (this crate's own comment there) - never the raw
                // `PlcConnection` (which would serialize the plaintext
                // password).
                check_fingerprint_unchanged(
                    state
                        .plc_connections
                        .get(body.id)
                        .await
                        .map(PlcConnectionResponse::from),
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
                // S1: same redacted-DTO comparison as the `.update` arm above.
                check_fingerprint_unchanged(
                    state
                        .plc_connections
                        .get(body.id)
                        .await
                        .map(PlcConnectionResponse::from),
                    expected,
                    "plc_connections",
                )
                .await?;
            }
            // 外部 DB 連携 S4: `plc_connections_delete`（即時削除の REST
            // ハンドラ）と同じ理由 - `cascade_delete_tx`の前に sink group
            // からの参照を拒否する。
            reject_delete_if_referenced_by_sink_group(&mut tx, body.id)
                .await
                .map_err(ApiError)
                .map_err(PendingApplyError::Api)?;
            // T19 S2-b（UX-38）: `plc_connections_delete`（即時削除の REST
            // ハンドラ）と同じく `cascade_delete_tx` を使う - 収集稼働中に
            // キューされ、後で（停止後に）適用される削除も同じ「子ごと削除」
            // 仕様に揃える。
            state
                .plc_connections
                .cascade_delete_tx(&mut tx, body.id)
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
            // T19 S2-b（UX-38）: `plc_connections.delete`分岐と同じ理由で
            // `cascade_delete_tx` を使う。
            state
                .collection_groups
                .cascade_delete_tx(&mut tx, body.id)
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
            // 外部 DB 連携 S4: `tags_delete`（即時削除の REST ハンドラ）と
            // 同じ理由でメンバーシップ行を能動的に消す。
            remove_tag_from_all_sink_groups(&mut tx, body.id)
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
        "tags.batch_delete" => {
            // T19 S2-c1 (UX-37): tags.batch_update 分岐と同じ形 - 稼働中に
            // キューされ、後で（停止後に）適用される一括削除も、即時削除
            // （`tags_batch_delete`）と全く同じ `delete_batch_tx` を使う。
            // ここを他の delete 系分岐（`tags.delete`/`plc_connections.delete`/
            // `collection_groups.delete`）と揃えないと、「即時削除はできる
            // のに、稼働中に積んだ保留削除だけ適用時に失敗する」という
            // 非対称バグになる（S2-b で実際に踏みかけた罠 - 実装指示参照）。
            let body: PendingBatchTagsDeletePayload = decode_pending_payload(pending)?;
            let outcome = state
                .tags
                .delete_batch_tx(&mut tx, &body.ids)
                .await
                .map_err(ApiError)
                .map_err(PendingApplyError::Api)?;
            if let BatchTagDeleteOutcome::Invalid(errors) = outcome {
                // `tags.batch_update` 分岐（直上）と違い `.{field.field}` を
                // 付けない: あちらの pending payload は `{ "tags": [ {
                // "id":…, "name":… } ] }` というオブジェクトの配列なので
                // `tags[i].<field>` が実在する JSON パスになるが、こちらの
                // payload は `{ "ids": [1, 2, 3] }` というスカラの配列なの
                // で、`ids[i]` 自体が対象を指し、その下に `.id` は存在しな
                // い。実際 `delete_batch_tx` が返す行エラーの `field` は
                // 構造上つねに `"id"`（重複・不存在の2種のみ）なので、
                // `.{field.field}` を付けても情報は増えず実在しないパスに
                // なるだけ。
                let field_errors = errors
                    .into_iter()
                    .flat_map(|row| {
                        row.field_errors.into_iter().map(move |field| FieldError {
                            field: format!("ids[{}]", row.index),
                            message: field.message,
                        })
                    })
                    .collect();
                return Err(PendingApplyError::Api(ApiError(BantoError::Validation {
                    field_errors,
                })));
            }
            // 外部 DB 連携 S4: 即時削除（`tags_batch_delete`）と同じ理由。
            for id in &body.ids {
                remove_tag_from_all_sink_groups(&mut tx, *id)
                    .await
                    .map_err(ApiError)
                    .map_err(PendingApplyError::Api)?;
            }
            "tags"
        }
        other => {
            return Err(PendingApplyError::Api(preflight_api_error(format!(
                "未対応の pending source です: {other}"
            ))));
        }
    };

    // #341 レビュー対応: この snapshot は**保存前検証（preflight）専用**。
    // catalog へ反映するのは `CollectionController::commit_catalog_and_apply_live`
    // が transition ロックの下で読み直す最新の snapshot（同 fn の doc
    // comment「古い snapshot の窓」節）。
    let _preflighted = match preflight_transaction(&mut tx).await {
        Ok(snapshot) => snapshot,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(PendingApplyError::Api(err));
        }
    };

    if let Err(err) = tx.commit().await {
        return Err(PendingApplyError::Api(storage_api_error(err)));
    }

    commit_catalog_and_notify(&state.controller, &state.events, resource)
        .await
        .map_err(PendingApplyError::LiveApplyFailed)?;

    Ok(())
}

async fn pending_changes_list(
    State(state): State<PendingChangesAdminState>,
    Query(query): Query<PendingChangesQuery>,
) -> Result<Json<Vec<PendingChange>>, ApiError> {
    let limit = query.limit.unwrap_or(100).clamp(1, 1000);
    Ok(Json(
        state
            .pending_changes
            .list(limit)
            .await?
            .into_iter()
            .map(redact_pending_change_for_response)
            .collect(),
    ))
}

async fn pending_changes_get(
    State(state): State<PendingChangesAdminState>,
    Path(id): Path<i64>,
) -> Result<Json<PendingChange>, ApiError> {
    Ok(Json(redact_pending_change_for_response(
        state.pending_changes.get(id).await?,
    )))
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
    Ok(Json(redact_pending_change_for_response(pending)))
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
    Ok(Json(redact_pending_change_for_response(pending)))
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
        // #341: 実行構成への反映だけが失敗した場合、レジストリ行は既に
        // コミット済みなので `failed`（= requeue して再適用しうる）へは
        // 落とさず `applied` にする - 二重適用を作らないため。反映失敗
        // 自体は 500 で伝え、詳細は `/api/v1/status` の
        // `last_config_error` にも残る。
        if let PendingApplyError::LiveApplyFailed(message) = &err {
            let applied = match state.pending_changes.mark_applied(id).await {
                Ok(applied) => Some(redact_pending_change_for_response(applied)),
                Err(mark_err) => {
                    eprintln!(
                        "banto-hub: pending_change={id} の applied 遷移に失敗しました: {mark_err}"
                    );
                    None
                }
            };
            // #341 レビュー対応（2026-09-14）: 成功時の `record_write`
            // （`result: "ok"`）と対になる失敗の監査行。DB へは適用済み
            // （行は `applied`）で実行構成への反映だけが失敗した、という
            // 事実がここだけからしか追えないため、必ず残す
            // （#340 の `record_write_control_failure` と同じ型）。
            let identity = actor_identity(&headers, &state.auth, &state.commissioning);
            state
                .audit
                .record(AuditEntry {
                    actor_username: identity.as_ref().map(|i| i.id.as_str()),
                    actor_role: identity.as_ref().map(|i| i.role.as_str()),
                    action: "apply",
                    resource: "pending_changes",
                    entity_id: Some(&id.to_string()),
                    detail: Some(json!({
                        "source": applying.source,
                        "failureReason": failure_reason,
                        "note": "DB への適用は成功したが、実行構成への反映に失敗した",
                    })),
                    origin: "rest",
                    result: "failed",
                })
                .await;
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": "live_reconfigure_failed",
                    "message": format!(
                        "変更は保存しましたが、実行構成への反映に失敗しました: {message}{LIVE_APPLY_RECOVERY_HINT}"
                    ),
                    "failureReason": failure_reason,
                    "pending": applied,
                })),
            )
                .into_response();
        }
        if let Err(mark_err) = state.pending_changes.mark_failed(id, &failure_reason).await {
            eprintln!("banto-hub: pending_change={id} の failed 遷移に失敗しました: {mark_err}");
        }

        return match err {
            PendingApplyError::Api(err) => err.into_response(),
            // TAG-P0-3 follow-up（2026-08-12）: `IntoResponse` 実装がそのまま
            // 409 + conflict body を返す。`failure_reason` は既に
            // `mark_failed` で DB へ記録済みなので、`GET
            // /api/pending-changes/{id}` から後追いで確認できる。
            err @ PendingApplyError::Conflict { .. } => err.into_response(),
            // #341: 上の分岐で早期 return 済み。
            PendingApplyError::LiveApplyFailed(_) => unreachable!(),
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
    Json(redact_pending_change_for_response(applied)).into_response()
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
                operation: OperationKind::Normal,
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
        if let Some(status) = registry_change_should_queue(&state.controller, &state.commissioning)
        {
            return queue_pending_registry_change(
                &state,
                &headers,
                "tags.batch_create",
                json!({ "dryRun": false, "tags": body.tags }),
                status,
            )
            .await;
        }
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
            // #341 レビュー対応: 上記と同じ - preflight 専用。
            let _preflighted = match preflight_transaction(&mut tx).await {
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
                commit_catalog_and_notify(&state.controller, &state.events, "tags")
                    .await
                    .map_err(RegistryMutationError::LiveApplyFailed)?;
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
        if let Some(status) = registry_change_should_queue(&state.controller, &state.commissioning)
        {
            return queue_pending_registry_change(
                &state,
                &headers,
                "tags.batch_update",
                json!({ "dryRun": false, "tags": body.tags }),
                status,
            )
            .await;
        }
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
            // #341 レビュー対応: 上記と同じ - preflight 専用。
            let _preflighted = match preflight_transaction(&mut tx).await {
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
                commit_catalog_and_notify(&state.controller, &state.events, "tags")
                    .await
                    .map_err(RegistryMutationError::LiveApplyFailed)?;
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

// --- T19 S2-c1 一括削除 API (bulk tag delete, UX-37): tags_batch_update
// (T18-3b) の削除版として骨格をそのまま写す - 稼働中は pending 1件で 202、
// 停止中は単一トランザクションで all-or-nothing 削除 → catalog commit
// 1回。実際の削除ロジックは `banto_tags::TagService::delete_batch_tx`
// （単票 `delete_tx` と同じ判定を id ごとに適用するだけ - 一括経路だけの
// 新しい削除セマンティクスは持たない、そのメソッドの doc comment 参照）。

/// `POST /api/tags/batch-delete` のリクエスト。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchTagsDeleteRequest {
    pub ids: Vec<i64>,
}

/// 行番号(0起点)付きのフィールドエラー一覧 - [`BatchTagUpdateRowErrorResponse`]
/// と全く同じ形（`banto_tags::BatchTagDeleteError`が
/// `banto_tags::BatchTagUpdateError`と同じ形を持つことの写し）。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchTagDeleteRowErrorResponse {
    index: usize,
    id: i64,
    field_errors: Vec<BatchTagFieldErrorResponse>,
}

impl From<banto_tags::BatchTagDeleteError> for BatchTagDeleteRowErrorResponse {
    fn from(err: banto_tags::BatchTagDeleteError) -> Self {
        Self {
            index: err.index,
            id: err.id,
            field_errors: err.field_errors.into_iter().map(Into::into).collect(),
        }
    }
}

/// `POST /api/tags/batch-delete` の応答。[`BatchTagsUpdateResponse`]と同じ
/// 「常に 200、`ok: false` で行ごとエラー」契約だが、`dryRun`/`tags` は
/// 持たない - `tags_batch_delete`の doc comment 参照（削除に事前検証
/// プレビューの意味が薄いため dry run 自体を設けていない）。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchTagsDeleteResponse {
    ok: bool,
    /// 削除された(または `ok: false` なら常に0の)件数。
    count: usize,
    errors: Vec<BatchTagDeleteRowErrorResponse>,
}

/// `POST /api/tags/batch-delete` - T19 S2-c1（UX-37 一括削除）。editor
/// 以上、`tags_batch_update`の骨格をそのまま写した削除版:
/// トランザクション内検証 → all-or-nothing 適用 → catalog commit 1回。
///
/// `dryRun` は設けない（実装指示、doc 冒頭参照） - 一括更新の dry run は
/// 「差分を確認してから確定する」ためのものだが、削除は差分ではなく
/// リクエストの `ids` そのものが対象そのものであり、対象件数・内訳は
/// クライアント側（選択済みタグの一覧）で既に分かっている。サーバー
/// 往復で検証結果を見せる段階を挟む理由がないため、常に即時適用
/// （収集稼働中は他の書き込みと同じく pending キュー）の一段構成にした。
async fn tags_batch_delete(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Json(body): Json<BatchTagsDeleteRequest>,
) -> RegistryMutationResult<Response> {
    require_editor(
        &state.auth,
        &state.commissioning,
        &state.audit,
        &headers,
        "tags",
        "POST",
        "/api/tags/batch-delete",
    )
    .await?;

    if let Some(status) = registry_change_should_queue(&state.controller, &state.commissioning) {
        return queue_pending_registry_change(
            &state,
            &headers,
            "tags.batch_delete",
            json!({ "ids": body.ids }),
            status,
        )
        .await;
    }

    let ids = body.ids;
    if ids.is_empty() {
        return Ok(Json(BatchTagsDeleteResponse {
            ok: true,
            count: 0,
            errors: Vec::new(),
        })
        .into_response());
    }

    let mut tx = state
        .manager
        .pool()
        .begin()
        .await
        .map_err(storage_api_error)?;
    let outcome = match state.tags.delete_batch_tx(&mut tx, &ids).await {
        Ok(outcome) => outcome,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(ApiError(err).into());
        }
    };
    match outcome {
        BatchTagDeleteOutcome::Invalid(errors) => {
            let _ = tx.rollback().await;
            Ok(Json(BatchTagsDeleteResponse {
                ok: false,
                count: 0,
                errors: errors.into_iter().map(Into::into).collect(),
            })
            .into_response())
        }
        BatchTagDeleteOutcome::Valid { count } => {
            // 外部 DB 連携 S4: `tags_delete`と同じ理由 - all-or-nothing で
            // 消えた`ids`全件について、対応するメンバーシップ行も消す。
            for id in &ids {
                if let Err(err) = remove_tag_from_all_sink_groups(&mut tx, *id).await {
                    let _ = tx.rollback().await;
                    return Err(ApiError(err).into());
                }
            }
            // #341 レビュー対応: 上記と同じ - preflight 専用。
            let _preflighted = match preflight_transaction(&mut tx).await {
                Ok(snapshot) => snapshot,
                Err(err) => {
                    let _ = tx.rollback().await;
                    return Err(err.into());
                }
            };
            tx.commit().await.map_err(storage_api_error)?;
            // 削除件数を detail に含める（T19 S2-b の cascade_delete と同じ
            // 考え方 - 監査ログから「何件消えたか」が読めるようにする）。
            record_write(
                &state.audit,
                &state.auth,
                &state.commissioning,
                &headers,
                "batch_delete",
                "tags",
                "-",
                Some(json!({ "count": count })),
            )
            .await;
            // tags_batch/tags_batch_update と同じ核心: n 件でも catalog
            // commit はここで1回だけ。
            commit_catalog_and_notify(&state.controller, &state.events, "tags")
                .await
                .map_err(RegistryMutationError::LiveApplyFailed)?;
            Ok(Json(BatchTagsDeleteResponse {
                ok: true,
                count,
                errors: Vec::new(),
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
        // S1a (docs/banto-hub-external-db-design.md §4.6): 保存済み接続の
        // 接続テスト - `{id}` の下の固定サブパスなので
        // `/api/plc-connections/{id}` の GET/PUT/DELETE とは axum のルート
        // マッチングで衝突しない。
        .route(
            "/api/plc-connections/{id}/test",
            post(plc_connections_test_saved),
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
        // S3（docs/banto-hub-external-db-design.md §7 row S3）: 保存済み
        // グループの `query_sql` describe - `{id}` の下の固定サブパスなので
        // `/api/collection-groups/{id}` の GET/PUT/DELETE とは axum の
        // ルートマッチングで衝突しない（`/api/plc-connections/{id}/test`
        // と同じ形）。
        .route(
            "/api/collection-groups/{id}/describe",
            post(collection_groups_describe),
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
        // T19 S2-c1 (UX-37): 一括削除 - 同じ理由で `/api/tags` 直下の固定
        // パス（`/api/tags/batch`・`/api/tags/batch-update` と衝突しない
        // 別セグメント）。
        .route("/api/tags/batch-delete", post(tags_batch_delete))
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
        // #342 段階A: 保存前の式検証（エラー位置・参照タグ解決・循環判定・
        // 現在値によるプレビュー試算）。同じく `/api/tags` 直下の固定セグ
        // メントで `/api/tags/{id}` (i64) とは衝突しない。
        .route("/api/tags/expression/check", post(tags_expression_check))
        // #342 段階B: 式欄のセグメント補完に出す組み込み関数表の配布
        // （`tags_expression_functions` の doc comment 参照）。`/api/tags`
        // 直下の固定セグメントなので `/api/tags/{id}` (i64) と衝突しない。
        .route(
            "/api/tags/expression/functions",
            get(tags_expression_functions),
        )
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            AuthGate {
                auth,
                commissioning,
                operation: OperationKind::Normal,
            },
            require_auth_or_commissioning,
        ))
}

// --- 外部 DB 連携 S4（docs/banto-hub-external-db-design.md §5.2・§6-13）:
// `hub_sink_groups` の CRUD ---------------------------------------------------
//
// `plc_connections`/`collection_groups`/`tags`（[`tag_registry_router`]）と
// 同じ認可（`require_editor`書き込み・viewer 読み取り）だが、**pending queue
// には載らない**（§6-13「収集に影響しないため即時適用」・`crate::sink`の
// モジュール doc comment参照）: sink group は PLC 収集パイプラインに一切
// 関わらないので、`queue_pending_registry_change`分岐を持たない。カタログの
// 再構築（`commit_catalog_and_notify`）も呼ばない - 変更の伝搬は SSE
// `ResourceChanged { resource: "sink_groups" }` の送出のみで足りる
// （サイドカーはこれを検知して`GET /api/sink/config`を取り直す）。

#[derive(Clone)]
struct SinkGroupsState {
    sink_groups: SinkGroupService,
    auth: AuthState,
    commissioning: CommissioningState,
    audit: AuditLogService,
    events: broadcast::Sender<ServerEvent>,
}

async fn sink_groups_list(
    State(state): State<SinkGroupsState>,
) -> Result<Json<Vec<SinkGroup>>, ApiError> {
    Ok(Json(state.sink_groups.list().await?))
}

async fn sink_groups_get(
    State(state): State<SinkGroupsState>,
    Path(id): Path<i64>,
) -> Result<Json<SinkGroup>, ApiError> {
    Ok(Json(state.sink_groups.get(id).await?))
}

async fn sink_groups_create(
    State(state): State<SinkGroupsState>,
    headers: HeaderMap,
    Json(input): Json<SinkGroupInput>,
) -> Result<Response, ApiError> {
    require_editor(
        &state.auth,
        &state.commissioning,
        &state.audit,
        &headers,
        "sink_groups",
        "POST",
        "/api/sink/groups",
    )
    .await?;
    let created = state.sink_groups.create(input).await?;
    record_write(
        &state.audit,
        &state.auth,
        &state.commissioning,
        &headers,
        "create",
        "sink_groups",
        &created.id.to_string(),
        Some(json!({ "name": created.name, "enabled": created.enabled })),
    )
    .await;
    let _ = state.events.send(ServerEvent::ResourceChanged {
        resource: "sink_groups".to_string(),
    });
    Ok(Json(created).into_response())
}

async fn sink_groups_update(
    State(state): State<SinkGroupsState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(input): Json<SinkGroupInput>,
) -> Result<Response, ApiError> {
    require_editor(
        &state.auth,
        &state.commissioning,
        &state.audit,
        &headers,
        "sink_groups",
        "PUT",
        "/api/sink/groups/{id}",
    )
    .await?;
    let updated = state.sink_groups.update(id, input).await?;
    record_write(
        &state.audit,
        &state.auth,
        &state.commissioning,
        &headers,
        "update",
        "sink_groups",
        &id.to_string(),
        Some(json!({ "name": updated.name, "enabled": updated.enabled })),
    )
    .await;
    let _ = state.events.send(ServerEvent::ResourceChanged {
        resource: "sink_groups".to_string(),
    });
    Ok(Json(updated).into_response())
}

async fn sink_groups_delete(
    State(state): State<SinkGroupsState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Response, ApiError> {
    require_editor(
        &state.auth,
        &state.commissioning,
        &state.audit,
        &headers,
        "sink_groups",
        "DELETE",
        "/api/sink/groups/{id}",
    )
    .await?;
    state.sink_groups.delete(id).await?;
    record_write(
        &state.audit,
        &state.auth,
        &state.commissioning,
        &headers,
        "delete",
        "sink_groups",
        &id.to_string(),
        None,
    )
    .await;
    let _ = state.events.send(ServerEvent::ResourceChanged {
        resource: "sink_groups".to_string(),
    });
    Ok(StatusCode::NO_CONTENT.into_response())
}

fn sink_groups_router(
    sink_groups: SinkGroupService,
    audit: AuditLogService,
    auth: AuthState,
    commissioning: CommissioningState,
    events: broadcast::Sender<ServerEvent>,
) -> Router {
    let state = SinkGroupsState {
        sink_groups,
        auth: auth.clone(),
        commissioning: commissioning.clone(),
        audit,
        events,
    };
    Router::new()
        .route(
            "/api/sink/groups",
            get(sink_groups_list).post(sink_groups_create),
        )
        .route(
            "/api/sink/groups/{id}",
            get(sink_groups_get)
                .put(sink_groups_update)
                .delete(sink_groups_delete),
        )
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            AuthGate {
                auth,
                commissioning,
                operation: OperationKind::Normal,
            },
            require_auth_or_commissioning,
        ))
}

// --- 外部 DB 連携 S4（design §5.2・§6-15）: サイドカー専用の admin 面
// `GET /api/sink/config`・`PUT /api/sink/status` -----------------------------
//
// admin スコープを持つ API キー（`bh_`、サイドカー専用に発行する想定）か、
// admin ロールのセッション（動作確認用）のいずれかを要求する -
// `crate::mcp::require_admin_scope`（MCP の構成ツールは admin スコープの
// API キーしか経路がない）と同じ「admin のみ」判定を、REST では
// API キー/セッションの両方に対して行う点だけが違う。`/api/v1/*`の
// `require_tag_space_auth`（read/write スコープ）とも、管理系ルーターの
// `RoleGuard`（セッションのみ）とも異なる、この2エンドポイント専用の
// ゲート。

/// [`require_sink_admin`]の判定結果。`Err`はそのまま返せる`Response`
/// （401/403）を運ぶ - 呼び出し元のハンドラは`if let Err(resp) = ... { return
/// resp; }`の1行で済む。`Response`は大きい（axum の`http::Response<Body>`）
/// ため`clippy::result_large_err`が付くが、この関数は2エンドポイントだけが
/// 呼ぶ低頻度のゲートで、ボックス化するほどのホットパスではない。
#[allow(clippy::result_large_err)]
async fn require_sink_admin(
    api_keys: &ApiKeysService,
    auth: &AuthState,
    commissioning: &CommissioningState,
    headers: &HeaderMap,
    now_ms: i64,
) -> Result<(), Response> {
    let Some(token) = bearer_token(headers) else {
        return Err(unauthorized_response());
    };
    if token.starts_with("bh_") {
        match api_keys.lookup(token, now_ms).await {
            Ok(ApiKeyLookup::Valid(ctx)) => {
                if let Err(err) = api_keys
                    .touch_last_used(ctx.id, now_ms, ctx.last_used_at_ms)
                    .await
                {
                    eprintln!(
                        "banto-hub: sink admin API キーの last_used_at 更新に失敗しました: {err}"
                    );
                }
                if ctx.has_admin_scope() {
                    Ok(())
                } else {
                    Err(forbidden_response())
                }
            }
            // Revoked/Tripped/Expired/NotFound はいずれも一律401
            // （`crate::mcp::require_mcp_auth`と同じ判断 - この2
            // エンドポイントもサイドカー専用の機械アクセスで、拒否理由の
            // 細分は必須ではない）。
            _ => Err(unauthorized_response()),
        }
    } else {
        match actor_identity(headers, auth, commissioning) {
            Some(identity)
                if Role::from_str(&identity.role)
                    .map(|role| role.at_least(Role::Admin))
                    .unwrap_or(false) =>
            {
                Ok(())
            }
            Some(_) => Err(forbidden_response()),
            None => Err(unauthorized_response()),
        }
    }
}

#[derive(Clone)]
struct SinkAdminState {
    sink_groups: SinkGroupService,
    plc_connections: PlcConnectionService,
    manager: Arc<CollectorManager>,
    sink_status: Arc<SinkStatusStore>,
    api_keys: ApiKeysService,
    auth: AuthState,
    commissioning: CommissioningState,
}

/// `GET /api/sink/config`の応答（設計 §5.2）。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SinkConfigResponse {
    generated_at: i64,
    groups: Vec<SinkConfigGroupEntry>,
    connections: Vec<SinkConfigConnectionEntry>,
}

/// 有効な sink group 1件分。`db_connection_id`は`connections`から対応する
/// 行を引くためのキー（設計本文の例には無いが、サイドカーがどの接続へ
/// INSERT するかを決める唯一の手掛かりなのでここに含める - S4 実装指示の
/// 逸脱点、PR 本文に記載）。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SinkConfigGroupEntry {
    id: i64,
    name: String,
    db_connection_id: i64,
    mode: String,
    interval_ms: i64,
    table_name: String,
    store_bad: bool,
    tags: Vec<SinkConfigTagEntry>,
}

/// 対象タグ1件分。`connectionId`/`groupId`/`tagId`は
/// `crates/banto-tagclient/src/types.rs`の`StableTagId::new(connection_id,
/// group_id, tag_id)`と完全に同じ3つ組 - サイドカーは banto-tagclient SDK
/// でこの3つ組から購読する（設計 §5.1「Hub からの値の取得は
/// banto-tagclient SDK」）。`externalName`はログ・推奨DDL表示など人が読む
///用途のためのおまけ。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SinkConfigTagEntry {
    connection_id: i64,
    group_id: i64,
    tag_id: i64,
    external_name: String,
}

/// 接続1件分。**パスワードを平文で含む**（設計 §6-15、2026-09-06 オーナー
/// 決定「admin スコープの専用 API キー + ループバック」）- このエンドポイント
/// 自体が admin スコープ限定であることと、Hub・サイドカーが同一マシンで
/// ループバック接続する運用前提（docs/banto-hub-external-db-design.md
/// §2.2・§5.2）がその埋め合わせ。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SinkConfigConnectionEntry {
    id: i64,
    name: String,
    host: String,
    port: i64,
    database: Option<String>,
    username: Option<String>,
    password: Option<String>,
}

async fn build_sink_config_response(
    state: &SinkAdminState,
) -> Result<SinkConfigResponse, BantoError> {
    let groups = state.sink_groups.list_enabled().await?;

    let map = state.manager.tag_map();
    let by_tag_id: std::collections::HashMap<i64, &TagEntry> =
        map.iter().map(|entry| (entry.ids.2, entry)).collect();

    let mut connection_ids: Vec<i64> = groups.iter().map(|group| group.db_connection_id).collect();
    connection_ids.sort_unstable();
    connection_ids.dedup();
    let mut connections = Vec::with_capacity(connection_ids.len());
    for connection_id in connection_ids {
        let conn = state.plc_connections.get(connection_id).await?;
        connections.push(SinkConfigConnectionEntry {
            id: conn.id,
            name: conn.name,
            host: conn.host,
            port: conn.port,
            database: conn.database,
            username: conn.username,
            password: conn.password,
        });
    }

    let group_entries = groups
        .into_iter()
        .map(|group| SinkConfigGroupEntry {
            id: group.id,
            name: group.name,
            db_connection_id: group.db_connection_id,
            mode: group.mode,
            interval_ms: group.interval_ms,
            table_name: group.table_name,
            store_bad: group.store_bad,
            // §5.2「行の削除で行も消える」の孤児許容分 - `crate::sink`の
            // モジュール doc comment参照: 現在の catalog に無い tag_id は
            // ここで単に読み飛ばす（サイドカー側は存在するタグだけを
            // 購読することになる）。
            tags: group
                .tag_ids
                .iter()
                .filter_map(|tag_id| {
                    by_tag_id.get(tag_id).map(|entry| SinkConfigTagEntry {
                        connection_id: entry.ids.0,
                        group_id: entry.ids.1,
                        tag_id: entry.ids.2,
                        external_name: entry.external_name.clone(),
                    })
                })
                .collect(),
        })
        .collect();

    Ok(SinkConfigResponse {
        generated_at: state.manager.clock().now_ms(),
        groups: group_entries,
        connections,
    })
}

/// `GET /api/sink/config`のパスワード平文返却はループバック専用という
/// 前提（設計 §6-15）をプロセス起動後の初回呼び出し1回だけ`warn`する -
/// このコードベースにログレベル機構は無い（`eprintln!`のみ）ため、
/// 「初回だけ」は`std::sync::Once`で表現する。
static SINK_CONFIG_LOOPBACK_WARNING: std::sync::Once = std::sync::Once::new();

async fn sink_config_get(State(state): State<SinkAdminState>, headers: HeaderMap) -> Response {
    let now_ms = state.manager.clock().now_ms();
    if let Err(resp) = require_sink_admin(
        &state.api_keys,
        &state.auth,
        &state.commissioning,
        &headers,
        now_ms,
    )
    .await
    {
        return resp;
    }
    SINK_CONFIG_LOOPBACK_WARNING.call_once(|| {
        eprintln!(
            "banto-hub: warn: GET /api/sink/config はDB接続のパスワードを平文で返します。\
             サイドカーは Hub と同一マシンからのループバック接続のみを前提とします\
             （docs/banto-hub-external-db-design.md §2.2・§5.2・§6-15）。"
        );
    });
    match build_sink_config_response(&state).await {
        Ok(body) => Json(body).into_response(),
        Err(err) => ApiError(err).into_response(),
    }
}

/// `PUT /api/sink/status`のボディ（設計 §5.2）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SinkStatusPushRequest {
    groups: Vec<SinkGroupStatusPush>,
}

async fn sink_status_put(
    State(state): State<SinkAdminState>,
    headers: HeaderMap,
    Json(body): Json<SinkStatusPushRequest>,
) -> Response {
    let now_ms = state.manager.clock().now_ms();
    if let Err(resp) = require_sink_admin(
        &state.api_keys,
        &state.auth,
        &state.commissioning,
        &headers,
        now_ms,
    )
    .await
    {
        return resp;
    }

    let mut field_errors = Vec::new();
    for group in &body.groups {
        if !crate::sink::is_valid_sink_group_state(&group.state) {
            field_errors.push(FieldError {
                field: "groups".to_string(),
                message: format!(
                    "group {} の state が不正です（{}）: {}",
                    group.id,
                    crate::sink::ALLOWED_SINK_GROUP_STATES.join(", "),
                    group.state
                ),
            });
        }
    }
    if !field_errors.is_empty() {
        return ApiError(BantoError::Validation { field_errors }).into_response();
    }

    // 未知の sink group id は列挙して422で拒否する（実装指示）。
    let known_ids: std::collections::HashSet<i64> = match state.sink_groups.list().await {
        Ok(groups) => groups.into_iter().map(|group| group.id).collect(),
        Err(err) => return ApiError(err).into_response(),
    };
    let mut unknown_ids: Vec<i64> = body
        .groups
        .iter()
        .map(|group| group.id)
        .filter(|id| !known_ids.contains(id))
        .collect();
    unknown_ids.sort_unstable();
    unknown_ids.dedup();
    if !unknown_ids.is_empty() {
        return ApiError(BantoError::Validation {
            field_errors: vec![FieldError {
                field: "groups".to_string(),
                message: format!(
                    "未知の sink group id が含まれています: {}",
                    unknown_ids
                        .iter()
                        .map(|id| id.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }],
        })
        .into_response();
    }

    // T2-4/§5.5「行単位の INSERT は監査しない」・実装指示「本文は info
    // レベルでもログしない」と同じ抑制 - push 本文（`lastError`に接続文字列
    // の断片が混ざりうる）を一切ログに出さない。
    state.sink_status.record(body.groups, now_ms);
    StatusCode::NO_CONTENT.into_response()
}

fn sink_admin_router(
    sink_groups: SinkGroupService,
    plc_connections: PlcConnectionService,
    manager: Arc<CollectorManager>,
    sink_status: Arc<SinkStatusStore>,
    api_keys: ApiKeysService,
    auth: AuthState,
    commissioning: CommissioningState,
) -> Router {
    let state = SinkAdminState {
        sink_groups,
        plc_connections,
        manager,
        sink_status,
        api_keys,
        auth,
        commissioning,
    };
    Router::new()
        .route("/api/sink/config", get(sink_config_get))
        .route("/api/sink/status", put(sink_status_put))
        .with_state(state)
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
    /// T3（設計 §5.3）: `GET /api/v1/status` の `mqtt.connected` のため。
    pub(crate) mqtt: Arc<MqttPublisher>,
    /// T19 S3-b（docs/banto-hub-t19-design.md §3.9、UX-46）: `GET
    /// /api/v1/status`・`GET /api/status` の `system`（CPU%・メモリ）の
    /// ため。`admin_status_router`・`admin_tag_stream_router`・
    /// `tag_space_router`のいずれも[`api_router_with_controller_mode`]が
    /// 構築した**同じ** `Arc` を受け取る（`mqtt`/`write_control`等と同じ
    /// 共有規律）- `SystemInfoSampler`のモジュール doc comment が説明する
    /// 「共有`System`を使い回す」ことの前提。
    pub(crate) system_info: Arc<SystemInfoSampler>,
    /// 外部 DB 連携 S4（design §5.2・§5.5）: `GET /api/v1/status`・
    /// `GET /api/status` の `sink` 節のため。上記の各 `Arc` と同じ共有規律 -
    /// `sink_admin_router`（`PUT /api/sink/status`）が書き込むものと**同じ**
    /// `Arc`を受け取る。
    pub(crate) sink_status: Arc<SinkStatusStore>,
    /// #335（2026-09-14 オーナー決定）: computed タグの `value_source`/
    /// `effective_simulation` 判定に、式が参照する入力タグの現在の
    /// simulation 状態が要るため（[`ComputedEngine::referenced_tags`]）。
    /// `manager.computed_engine()` と**同じ** `Arc`（他の共有フィールドと
    /// 同じ規律）。
    pub(crate) computed: Arc<ComputedEngine>,
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
    fn from_runtime(
        entry: &TagEntry,
        runtime: &CollectionStatus,
        map: &TagMap,
        computed: &ComputedEngine,
    ) -> Self {
        Self {
            entry: entry.clone(),
            configured_simulation: entry.simulation,
            effective_simulation: effective_simulation_for_tag(entry, runtime, map, computed),
            value_source: value_source_for_tag(entry, runtime, map, computed).to_string(),
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

/// `GET /api/v1/tags`・管理系 `GET /api/tag-catalog`（[`admin_tag_catalog`]、
/// 試運転モード対応・設計 §5.6・2026-08-31 オーナー決定「案A」の続き）が
/// 共有する本体。**API キー・セッション token のいずれでも、収集状態を
/// 問わず常に全タグを返す**（2026-09-14/15 オーナー決定 #335 - 接続単位の
/// simulation 設定済み PLC タグも computed タグも、catalog からは一切
/// 隠さない。「WS/gRPC では購読できるのに catalog からは発見できない」と
/// いう不整合を解消するための決定であり、以前ここにあった
/// `api_key_external_output_allowed`（API キー要求だけシミュレーション系
/// タグを除外していた関数、および呼び出し元だったこの関数の
/// `api_key_request` 引数）は撤去済み。2026-09-15 オーナー決定（「外部出力を
/// PLC への出力と勘違いしていた」）でさらに、[`v1_values`]・
/// [`v1_value_single`]・[`v1_value_read_now`]の run 単位 AllSimulation
/// ゲートも撤去した - 外部への読み取り出力はシミュレーションで一切
/// ゲートしない（catalog は元々ゲート対象外だった）。
fn build_catalog_response(state: &TagSpaceState, query: &TagsQuery) -> CatalogResponse {
    let map = state.manager.tag_map();
    let revision = state.manager.revision();
    let runtime = state.controller.status();
    let tags: Vec<CatalogTagEntry> = map
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
        .map(|entry| CatalogTagEntry::from_runtime(entry, &runtime, &map, &state.computed))
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
/// requests see exactly the same tags as session/admin requests（2026-09-14
/// オーナー決定 #335、[`build_catalog_response`]のdoc comment参照）。
/// ロジックは[`build_catalog_response`]側にあり、ここでは`ctx`を受け取る
/// だけ（catalog は API キー・セッションの別を判定しない） - 管理系の
/// [`admin_tag_catalog`]も同じ関数を呼ぶ（二重管理を避けるための分離、
/// `compute_status`/[`v1_status`]と同じ構成）。
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
    _ctx: Option<Extension<ApiKeyContext>>,
) -> Json<CatalogResponse> {
    Json(build_catalog_response(&state, &query))
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
#[allow(clippy::too_many_arguments)]
fn value_entry(
    external_name: &str,
    entry: &TagEntry,
    current: Option<&banto_collect::CurrentValuesHandle>,
    server_store: &crate::computed::ServerTagStore,
    now_ms: i64,
    runtime: &CollectionStatus,
    map: &TagMap,
    computed: &ComputedEngine,
) -> ValueEntry {
    let (v, q, t) = crate::hub::read_current(entry, current, server_store, now_ms);
    ValueEntry {
        tag: external_name.to_string(),
        v,
        q: crate::hub::quality_str(q).to_string(),
        t,
        value_source: value_source_for_tag(entry, runtime, map, computed).to_string(),
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
/// 不変)。2026-09-15 オーナー決定（#335 追補、「外部出力を PLC への出力と
/// 勘違いしていた」）: simulation / derived_simulation / computed をタグ
/// 単位で除外することはもう無く、run が AllSimulation 中でも無条件で
/// 値を返す - 外部への**読み取り**出力（REST/WS/gRPC/MQTT）はシミュレー
/// ションでゲートしない。呼び出し側は`value_source`/`collectionMode`で
/// 判別する。旧 T15-3 の`test_output`opt-in 機構は #362 で撤去済み
/// （`docs/tag-server-design.md` §6.3参照）。
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
                map,
                &state.computed,
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
/// 従来どおり全アクセス(管理 UI 不変)。2026-09-15 オーナー決定（#335 追補）:
/// simulation / derived_simulation / computed をタグ単位で隠すことはもう
/// 無く、run が AllSimulation 中でも無条件で値を返す（外部への読み取り
/// 出力はシミュレーションでゲートしない - `value_source`で判別させる）。
#[utoipa::path(
    get,
    path = "/api/v1/values/{tag}",
    params(("tag" = String, Path, description = "外部名 {connection}.{group}.{tag}")),
    responses(
        (status = 200, description = "単一タグの現在値", body = SingleValueResponse),
        (status = 403, description = "per-tag read スコープ外(API キー、H10 ③)"),
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
            &map,
            &state.computed,
        ),
        &runtime,
    ))
    .into_response()
}

/// `GET /api/v1/values/{tag}/read-now` の応答(T20 ①b、
/// docs/banto-hub-t20-design.md §3.1)。[`SingleValueResponse`](cache 読み)
/// と違い `v` は `serde_json::Value` - 数値・真偽値・文字列のいずれも
/// 表せる(文字列タグはこの経路でしか読めない - このモジュールの
/// [`v1_value_read_now`] doc comment参照)。`SingleValueResponse` 自体は
/// 変更しない(既存の cache 読みの応答型を壊さない、実装指示の制約)。
#[derive(Debug, Serialize, ToSchema)]
struct ReadNowResponse {
    tag: String,
    /// 数値・真偽値・文字列のいずれか(数値タグは scale 済みの工学値 -
    /// `crate::read_path`のモジュール doc comment「スケーリング」節参照)。
    v: serde_json::Value,
    /// その場読みが成功した時点の品質は常に `"good"` - キャッシュ読みと
    /// 違い、この経路は失敗を `q: "bad"` の200応答ではなく HTTP エラーで
    /// 表す(`read_now_rejection_response`参照)ので、200応答の`q`は常に
    /// `"good"`。フィールド自体は cache 読みの `ValueEntry`/`SingleValueResponse`
    /// と形を揃えるために残す。
    q: String,
    /// この読み取りが完了した時刻(ミリ秒epoch) - cache 読みの `t`
    /// (サンプル採取時刻)と違い、その場読みでは「サンプル時刻」という
    /// 概念自体が無い(収集サイクルを経由しない)ので、応答組み立て時の
    /// 時刻をそのまま使う。
    t: i64,
}

impl ReadNowResponse {
    fn from_value(tag: String, value: banto_plc::PlcValue, now_ms: i64) -> Self {
        Self {
            tag,
            v: crate::read_path::plc_value_to_json(&value),
            q: "good".to_string(),
            t: now_ms,
        }
    }
}

/// `GET /api/v1/values/{tag}/read-now` - T20 ①b(案A、
/// docs/banto-hub-t20-design.md §3.1「その場読み」)。`GET
/// /api/v1/values/{tag}`(cache 読み、[`v1_value_single`]、
/// current_values/tstore 経由)とは別経路 - **この経路は毎回 PLC から直接
/// 読み、収集キャッシュ(current_values/tstore)には一切触れない**
/// (`crate::read_path`のモジュール doc comment参照)。収集パイプラインから
/// 意図的に除外されている文字列タグを読める唯一の GET エンドポイント。
///
/// 認証は [`v1_value_single`]と同じ read ルート規律(`ctx.has_any_read()`
/// はミドルウェア`require_tag_space_auth`、per-tag `ctx.can_read_value(tag)`
/// はこのハンドラ自身)。エラーマッピングの詳細は
/// [`crate::read_path::execute_read_now`]/[`read_now_rejection_response`]
/// 参照。2026-09-15 オーナー決定（#335 追補）: タグ単位の simulation /
/// derived_simulation / computed 除外も、run 単位の AllSimulation ゲートも
/// 撤去済み - 外部への読み取り出力はシミュレーションで一切ゲートしない。
#[utoipa::path(
    get,
    path = "/api/v1/values/{tag}/read-now",
    params(("tag" = String, Path, description = "外部名 {connection}.{group}.{tag}")),
    responses(
        (status = 200, description = "その場読みの結果(v は数値・真偽値・文字列のいずれか)", body = ReadNowResponse),
        (status = 403, description = "per-tag read スコープ外(API キー、H10 ③と同じ規律)"),
        (status = 404, description = "catalog に存在しない外部名"),
        (status = 422, description = "internal/computed タグ(PLC 接続を持たない)、またはアドレス不正"),
        (status = 501, description = "接続のプロトコルに broker ドライバが未対応"),
        (status = 502, description = "PLC からの読み取りに失敗"),
        (status = 503, description = "収集セッションが無い(新規にはダイヤルしない)"),
    ),
    tag = "tag-space",
)]
async fn v1_value_read_now(
    State(state): State<TagSpaceState>,
    Path(tag): Path<String>,
    ctx: Option<Extension<ApiKeyContext>>,
) -> Response {
    let map = state.manager.tag_map();
    if map.get(&tag).is_none() {
        return ApiError(BantoError::NotFound {
            resource: "tags".to_string(),
            id: tag,
        })
        .into_response();
    }
    if let Some(Extension(ctx)) = &ctx {
        if !ctx.can_read_value(&tag) {
            return forbidden_response();
        }
    }
    match crate::read_path::execute_read_now(&state.manager, &tag).await {
        Ok(value) => {
            let now_ms = state.manager.clock().now_ms();
            Json(ReadNowResponse::from_value(tag, value.value, now_ms)).into_response()
        }
        Err(rejection) => read_now_rejection_response(tag, rejection),
    }
}

/// [`v1_value_read_now`]の拒否理由 → HTTP 応答マッピング
/// (`write_rejection_response`の読み取り版・同じ形)。`NotFound` は
/// [`v1_value_read_now`]がその場読みを試みる前に自前で 404 返しているので
/// 通常はここへ来ないが、`crate::read_path::execute_read_now`は
/// 他の呼び出し元(MCP の `read_tag_now`)からも呼ばれる独立した関数であり、
/// かつ2回の catalog 参照の間に rebuild が挟まる理論上の競合もあり得るため、
/// `write_rejection_response`と違って`unreachable!`にはしない(素直に404
/// を返す)。
fn read_now_rejection_response(
    tag: String,
    rejection: crate::read_path::ReadNowRejection,
) -> Response {
    use crate::read_path::ReadNowRejection;

    if matches!(rejection, ReadNowRejection::NotFound) {
        return ApiError(BantoError::NotFound {
            resource: "tags".to_string(),
            id: tag,
        })
        .into_response();
    }

    let status = match &rejection {
        ReadNowRejection::NotFound => StatusCode::NOT_FOUND, // 上で早期returnされるが、network越しの二重呼び出し等に備え防御的に用意
        ReadNowRejection::NotPlcBacked | ReadNowRejection::InvalidAddress(_) => {
            StatusCode::UNPROCESSABLE_ENTITY
        }
        ReadNowRejection::UnsupportedProtocol => StatusCode::NOT_IMPLEMENTED,
        ReadNowRejection::NoSession => StatusCode::SERVICE_UNAVAILABLE,
        ReadNowRejection::ReadFailed(_) => StatusCode::BAD_GATEWAY,
        ReadNowRejection::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(rejection.to_json())).into_response()
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

/// `GET /api/v1/status` の `db_source`（外部 DB 連携 S2、
/// docs/banto-hub-external-db-design.md §4.2・§4.3）: DB Source の接続ごとの
/// 運転状態。`crate::db_source::status` のプロセス内メモリをそのまま写した
/// もので、**秘密は含まない**（`last_error` は
/// `crate::db_source::sanitize_postgres_error` を通した文字列だけ）ため、
/// `mqtt`/`grpc` 節と同じく admin 限定にはしていない。
#[derive(Debug, Serialize, ToSchema)]
struct DbSourceStatusEntry {
    connection_id: i64,
    connection_name: String,
    /// `connected` / `backoff` / `disabled` / `error` / `stopped`
    /// （`crate::db_source::DbConnectionState`）。`stopped` は S2b
    /// （設計 §4.8・§6-16）: 収集が Running でないのでタスクを起動して
    /// いない（DB へ繋いでいない・配下のタグは Bad）。
    state: String,
    last_poll_at: Option<i64>,
    last_error: Option<String>,
    consecutive_failures: u32,
    /// S2b（§4.8・§6-17）: この接続のタスクが異常終了して supervisor に
    /// 作り直された回数と、その最後の理由
    /// （`crate::db_source::DbConnectionStatus::restarts`）。
    restarts: u32,
    last_restart_reason: Option<String>,
    groups: Vec<DbSourceGroupStatusEntry>,
}

/// [`DbSourceStatusEntry`] のグループ1件分。
#[derive(Debug, Serialize, ToSchema)]
struct DbSourceGroupStatusEntry {
    group_id: i64,
    group_name: String,
    last_ok_at: Option<i64>,
    last_error: Option<String>,
    /// 直近の実行で取れた行数。生成 SQL が `LIMIT 2` を付けるので `2` は
    /// 「2 行以上」を意味する（`crate::db_source::convert::build_wrapper_sql`）。
    row_count_last: Option<i64>,
}

impl From<crate::db_source::DbConnectionStatus> for DbSourceStatusEntry {
    fn from(status: crate::db_source::DbConnectionStatus) -> Self {
        Self {
            connection_id: status.connection_id,
            connection_name: status.connection_name,
            state: status.state.as_str().to_string(),
            last_poll_at: status.last_poll_at,
            last_error: status.last_error,
            consecutive_failures: status.consecutive_failures,
            restarts: status.restarts,
            last_restart_reason: status.last_restart_reason,
            groups: status
                .groups
                .into_iter()
                .map(|group| DbSourceGroupStatusEntry {
                    group_id: group.group_id,
                    group_name: group.group_name,
                    last_ok_at: group.last_ok_at,
                    last_error: group.last_error,
                    row_count_last: group.row_count_last,
                })
                .collect(),
        }
    }
}

/// `GET /api/v1/status`の`sink.sidecar`（外部 DB 連携 S4、design §5.2・
/// §5.5）。`crate::sink::status::SinkStatusSnapshot`のwire形。
#[derive(Debug, Serialize, ToSchema)]
struct SinkSidecarStatusEntry {
    /// `"online"`（[`crate::sink::SINK_SIDECAR_STALE_AFTER_MS`]以内に
    /// `PUT /api/sink/status`の push があった）か`"unknown"`（無い、または
    /// 一度も push が無い）。
    state: String,
    last_seen_at: Option<i64>,
}

/// `GET /api/v1/status`の`sink.groups`の1件分 - サイドカーが最後に push
/// した内容をそのまま映す（`crate::sink::SinkGroupStatusPush`のwire形）。
#[derive(Debug, Serialize, ToSchema)]
struct SinkGroupStatusEntry {
    id: i64,
    state: String,
    queued: u64,
    dropped: u64,
    last_flush_at: Option<i64>,
    last_error: Option<String>,
}

/// `GET /api/v1/status`の`sink`節（外部 DB 連携 S4）。
#[derive(Debug, Serialize, ToSchema)]
struct SinkStatusEntry {
    sidecar: SinkSidecarStatusEntry,
    groups: Vec<SinkGroupStatusEntry>,
}

impl From<SinkStatusSnapshot> for SinkStatusEntry {
    fn from(snapshot: SinkStatusSnapshot) -> Self {
        Self {
            sidecar: SinkSidecarStatusEntry {
                state: snapshot.sidecar_state.to_string(),
                last_seen_at: snapshot.last_seen_at,
            },
            groups: snapshot
                .groups
                .into_iter()
                .map(|push| SinkGroupStatusEntry {
                    id: push.id,
                    state: push.state,
                    queued: push.queued,
                    dropped: push.dropped,
                    last_flush_at: push.last_flush_at,
                    last_error: push.last_error,
                })
                .collect(),
        }
    }
}

/// `GET /api/v1/status` の応答。T19 S5: `crate::mcp`の`get_server_status`
/// ツールが[`compute_status`]をそのまま呼んで`serde_json::to_value`で
/// 包み直すため`pub(crate)`（フィールド自体へは触れない - トレイト経由の
/// シリアライズのみ）。
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct StatusResponse {
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
    /// T2-4（設計 §6-6）: 起動時に永続テーブルから復元した値
    /// (`crate::write_control::WriteControl` のモジュール doc comment
    /// 参照。以後の enable/disable ではこの値自体は変わらない。
    /// 2026-09-09 オーナー決定 #340)。
    write_was_enabled_before_restart: bool,
    /// T3（設計 §5.3）: MQTT publish の設定/接続状態。
    mqtt: MqttStatusEntry,
    /// T4（設計 §5.4）: gRPC サーバーの設定。
    grpc: GrpcStatusEntry,
    /// T7-2（設計 §4.3）: 直近の `apply_config` 呼び出しの結果。最後に成功
    /// した rebuild が `apply_config` を経由しなかった場合（起動直後の初回
    /// 成功、または空構成への遷移）は `null`
    /// (`crate::hub::CollectorManager::last_apply` のドキュメント参照)。
    last_apply: Option<LastApplyEntry>,
    /// 外部 DB 連携 S2（§4.2・§4.3）: DB Source の接続ごとの運転状態。
    /// `postgres` 接続が1つも無い通常構成では空配列。
    db_source: Vec<DbSourceStatusEntry>,
    /// T19 S3-b（docs/banto-hub-t19-design.md §3.9、UX-46「サーバー状態の
    /// 拡充」）: サーバー自身の CPU 使用率・メモリ使用量。
    /// `crate::system_info::SystemInfoSampler`のモジュール doc comment
    /// 参照 - プロセス起動直後最初の呼び出しは `cpu_percent` が `0.0`に
    /// なりうる。
    system: SystemInfoSnapshot,
    /// 外部 DB 連携 S4（design §5.2・§5.5）: DB Sink サイドカーの運転状態。
    /// サイドカーが1度も`PUT /api/sink/status`を push していない、または
    /// 15秒以上 push が無ければ`sidecar.state == "unknown"`
    /// （`crate::sink::status`のモジュール doc comment参照）。
    sink: SinkStatusEntry,
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
///
/// **T19 S2-a (UX-48, 2026-09-03)**: an enabled, broker-managed connection
/// with zero enabled collection groups reports `"unused"` here instead of
/// falling through to `broker_status` (which would read `None`/`Stopped`
/// indistinguishably from a disabled connection) - see
/// `crate::hub::CollectorManager::sync_broker_sessions_from`'s doc comment for
/// why such a connection has no broker session to report on in the first
/// place.
pub(crate) async fn compute_status(state: &TagSpaceState) -> Result<StatusResponse, ApiError> {
    let runtime = state.controller.status();
    let revision = state.manager.configured_revision();
    let last_config_error = state.manager.last_error();
    let statuses = state.manager.connection_status().await;

    let connections = PlcConnectionService::new(state.manager.pool())
        .list(ListParams::default())
        .await?
        .rows;
    // T19 S2-a (UX-48): same predicate the session sync
    // (`CollectorManager::sync_broker_sessions_from`) and the collector itself
    // (`banto_collect::build_config_from`) use to decide "does this
    // connection have anything to collect" - reused here so the status
    // screen can tell a genuinely-unused connection ("unused" below) apart
    // from one that is enabled, has tags, and simply isn't connecting
    // ("stopped"/"reconnecting").
    let collectible_connection_ids = connections_with_collected_groups(
        &CollectionGroupService::new(state.manager.pool())
            .list(ListParams::default())
            .await?
            .rows,
    );
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
                // T19 S2-a (UX-48): a connection with no enabled collection
                // group gets no broker session pre-synced at all (see
                // `CollectorManager::sync_broker_sessions_from`'s doc comment),
                // so `broker_status` would otherwise round it down to
                // "stopped" indistinguishably from a disabled or genuinely
                // failing connection. Surface it as its own "unused" status
                // instead - a disabled connection still reports "stopped"
                // (checked first: `conn.enabled` gates this branch), so the
                // two stay visually distinct.
                if conn.enabled && !collectible_connection_ids.contains(&conn.id) {
                    ("unused", None)
                } else {
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
        mqtt: MqttStatusEntry {
            enabled: mqtt_settings.enabled,
            connected: state.mqtt.connected(),
        },
        grpc: GrpcStatusEntry {
            enabled: grpc_settings.enabled,
            port: grpc_settings.port,
        },
        last_apply: state.manager.last_apply().map(LastApplyEntry::from),
        db_source: state
            .manager
            .db_source_status()
            .into_iter()
            .map(DbSourceStatusEntry::from)
            .collect(),
        system: state.system_info.sample(),
        sink: state
            .sink_status
            .snapshot(state.manager.clock().now_ms())
            .into(),
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

/// [`SystemInfoSnapshot`]のcamelCase版（`cpuPercent`・`processMemoryBytes`・
/// `hostMemoryUsedBytes`・`hostMemoryTotalBytes`）。
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct AdminSystemInfoEntry {
    cpu_percent: f32,
    process_memory_bytes: u64,
    host_memory_used_bytes: u64,
    host_memory_total_bytes: u64,
}

impl From<SystemInfoSnapshot> for AdminSystemInfoEntry {
    fn from(snapshot: SystemInfoSnapshot) -> Self {
        Self {
            cpu_percent: snapshot.cpu_percent,
            process_memory_bytes: snapshot.process_memory_bytes,
            host_memory_used_bytes: snapshot.host_memory_used_bytes,
            host_memory_total_bytes: snapshot.host_memory_total_bytes,
        }
    }
}

/// [`DbSourceStatusEntry`]のcamelCase版（外部 DB 連携 S2）。
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct AdminDbSourceStatusEntry {
    connection_id: i64,
    connection_name: String,
    state: String,
    last_poll_at: Option<i64>,
    last_error: Option<String>,
    consecutive_failures: u32,
    /// S2b（§4.8・§6-17）: JSON では `restarts` / `lastRestartReason`。
    restarts: u32,
    last_restart_reason: Option<String>,
    groups: Vec<AdminDbSourceGroupStatusEntry>,
}

/// [`DbSourceGroupStatusEntry`]のcamelCase版。
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct AdminDbSourceGroupStatusEntry {
    group_id: i64,
    group_name: String,
    last_ok_at: Option<i64>,
    last_error: Option<String>,
    row_count_last: Option<i64>,
}

impl From<DbSourceStatusEntry> for AdminDbSourceStatusEntry {
    fn from(entry: DbSourceStatusEntry) -> Self {
        Self {
            connection_id: entry.connection_id,
            connection_name: entry.connection_name,
            state: entry.state,
            last_poll_at: entry.last_poll_at,
            last_error: entry.last_error,
            consecutive_failures: entry.consecutive_failures,
            restarts: entry.restarts,
            last_restart_reason: entry.last_restart_reason,
            groups: entry
                .groups
                .into_iter()
                .map(|group| AdminDbSourceGroupStatusEntry {
                    group_id: group.group_id,
                    group_name: group.group_name,
                    last_ok_at: group.last_ok_at,
                    last_error: group.last_error,
                    row_count_last: group.row_count_last,
                })
                .collect(),
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
    mqtt: MqttStatusEntry,
    grpc: GrpcStatusEntry,
    last_apply: Option<AdminLastApplyEntry>,
    /// 外部 DB 連携 S2: [`DbSourceStatusEntry`]のcamelCase版。
    db_source: Vec<AdminDbSourceStatusEntry>,
    /// T19 S3-b（UX-46）: [`SystemInfoSnapshot`]のcamelCase版。
    system: AdminSystemInfoEntry,
    /// 外部 DB 連携 S4: [`SinkStatusEntry`]のcamelCase版。
    sink: AdminSinkStatusEntry,
}

/// [`SinkSidecarStatusEntry`]のcamelCase版。
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct AdminSinkSidecarStatusEntry {
    state: String,
    last_seen_at: Option<i64>,
}

/// [`SinkGroupStatusEntry`]のcamelCase版。
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct AdminSinkGroupStatusEntry {
    id: i64,
    state: String,
    queued: u64,
    dropped: u64,
    last_flush_at: Option<i64>,
    last_error: Option<String>,
}

/// [`SinkStatusEntry`]のcamelCase版。
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct AdminSinkStatusEntry {
    sidecar: AdminSinkSidecarStatusEntry,
    groups: Vec<AdminSinkGroupStatusEntry>,
}

impl From<SinkStatusEntry> for AdminSinkStatusEntry {
    fn from(entry: SinkStatusEntry) -> Self {
        Self {
            sidecar: AdminSinkSidecarStatusEntry {
                state: entry.sidecar.state,
                last_seen_at: entry.sidecar.last_seen_at,
            },
            groups: entry
                .groups
                .into_iter()
                .map(|group| AdminSinkGroupStatusEntry {
                    id: group.id,
                    state: group.state,
                    queued: group.queued,
                    dropped: group.dropped,
                    last_flush_at: group.last_flush_at,
                    last_error: group.last_error,
                })
                .collect(),
        }
    }
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
            mqtt: status.mqtt,
            grpc: status.grpc,
            last_apply: status.last_apply.map(Into::into),
            db_source: status.db_source.into_iter().map(Into::into).collect(),
            system: status.system.into(),
            sink: status.sink.into(),
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
/// （[`v1_tags`]）と共有する - 2026-09-14 オーナー決定（#335）で
/// `build_catalog_response`は API キー・セッションの別を判定しなくなった
/// ため、この管理 UI 経路と`/api/v1/tags`は常に同じ結果を返す
/// （シミュレーション設定・computed を含め全件、[`admin_values`]と同じ
/// 判断）。
async fn admin_tag_catalog(
    State(state): State<TagSpaceState>,
    Query(query): Query<TagsQuery>,
) -> Json<AdminCatalogResponse> {
    Json(build_catalog_response(&state, &query).into())
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
#[allow(clippy::too_many_arguments)]
fn admin_status_router(
    manager: Arc<CollectorManager>,
    controller: Arc<CollectionController>,
    write_control: Arc<WriteControl>,
    mqtt: Arc<MqttPublisher>,
    system_info: Arc<SystemInfoSampler>,
    sink_status: Arc<SinkStatusStore>,
    auth: AuthState,
    commissioning: CommissioningState,
) -> Router {
    let computed = manager.computed_engine();
    let state = TagSpaceState {
        manager,
        controller,
        write_control,
        mqtt,
        system_info,
        sink_status,
        computed,
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
                operation: OperationKind::Normal,
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
#[allow(clippy::too_many_arguments)]
fn admin_tag_stream_router(
    manager: Arc<CollectorManager>,
    controller: Arc<CollectionController>,
    write_control: Arc<WriteControl>,
    mqtt: Arc<MqttPublisher>,
    system_info: Arc<SystemInfoSampler>,
    sink_status: Arc<SinkStatusStore>,
    auth: AuthState,
    commissioning: CommissioningState,
) -> Router {
    let computed = manager.computed_engine();
    let state = TagSpaceState {
        manager,
        controller,
        write_control,
        mqtt,
        system_info,
        sink_status,
        computed,
    };
    Router::new()
        .route(ADMIN_TAG_STREAM_PATH, get(crate::stream::ws_upgrade))
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            AuthGate {
                auth,
                commissioning,
                operation: OperationKind::Normal,
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
/// `{ "v": <number|bool> }`」、T20 ①a で文字列を追加 - docs/banto-hub-t20-design.md
/// §3.1）。`v` は `serde_json::Value` のまま保持し、[`parse_requested_value`]
/// で数値/真偽値/文字列のみを受理する（配列・オブジェクトは 422）—
/// `bool`/`number`/`string` を1つの Rust 型に素直にマップする serde 表現が
/// なく、utoipa の untagged enum サポートも弱いため、検証はハンドラ内で行う。
#[derive(Debug, Deserialize, ToSchema)]
struct WriteValueRequest {
    /// 書き込む工学値。数値タグには数値、bit タグには真偽値、string タグ
    /// には文字列（T20 ①a）。（2026-08-06〜: 型が data_type と一致しない
    /// 場合は 422 `unsupported_value_type` - 暗黙の型変換はしない）。
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
/// する。`bool` は [`RequestedValue::Bool`]、数値は [`RequestedValue::Num`]、
/// 文字列は [`RequestedValue::Str`](T20 ①a、docs/banto-hub-t20-design.md
/// §3.1 で追加 - string タグへの書き込みに使う)。どちらの型が来たかは
/// gate 7（`crate::write_path::execute_write`）が data_type との対称性検査に
/// 使うので、ここで `f64` へ潰さない（2026-08-06 変更: 従来は `bool` を
/// `1.0`/`0.0` に潰して数値と区別せずに渡していた）。配列・オブジェクト・
/// `null` は `None`（呼び出し元が 422 `unsupported_value_type` を返す）。
/// REST 固有の wire 形式（JSON の `v`）からの変換であり、gRPC 側は
/// `oneof num|bool` を直接分解するだけで済む（文字列は本スライスの対象外 -
/// gRPC の `WriteValueRequest` に文字列 oneof が無い）ため、この関数は
/// `crate::write_path` へは移していない。
pub(crate) fn parse_requested_value(
    v: &serde_json::Value,
) -> Option<crate::write_path::RequestedValue> {
    use crate::write_path::RequestedValue;
    if let Some(b) = v.as_bool() {
        Some(RequestedValue::Bool(b))
    } else if let Some(s) = v.as_str() {
        Some(RequestedValue::Str(s.to_string()))
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
        WriteRejection::CollectionNotRunning(_) => StatusCode::SERVICE_UNAVAILABLE,
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
        // T20-3a: `execute_write`(単票、この関数が変換する唯一の呼び出し
        // 元)は `BatchAborted`/`DuplicateTagInBatch` を絶対に返さない -
        // `crate::write_path`の各バリアントのdoc comment参照。バッチ専用の
        // `POST /api/v1/values/batch`(`v1_write_values_batch`)は
        // `WriteRejection::to_json()`をそのまま封筒に載せる別の応答経路
        // (下記)を使い、この関数は通らない。
        WriteRejection::BatchAborted => {
            unreachable!(
                "execute_write は BatchAborted を返さない - execute_write_batch 専用の結果"
            )
        }
        WriteRejection::DuplicateTagInBatch(_) => {
            unreachable!(
                "execute_write は DuplicateTagInBatch を返さない - execute_write_batch 専用の結果"
            )
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
        (status = 503, description = "writes_disabled / collection_not_running"),
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

// --- レシピ一括書き込み（T20-3a、docs/banto-hub-t20-design.md §3.3） -------
//
// `POST /api/v1/values/batch`。ゲート1〜8の本体は
// `crate::write_path::execute_write_batch` へ丸ごと委譲する（単票
// `v1_write_value` と同じ「二重実装は絶対に不可」規律、§3.7「ゲートを
// 迂回しない」）。このハンドラ自身が担うのは REST 固有の前段(セッション
// token 拒否・**各エントリの** `write:{tag}` スコープ検査・JSON body の
// 正規化)と、per-entry 結果を常に 200 の封筒へ変換するだけ
// （`crate::rest`の `POST /api/tags/batch`(`BatchTagsResponse`)と同じ
// 「1件でも不正なら全体拒否は例外ではなく通常の応答」という思想 -
// このモジュールの `BatchTagsResponse` doc comment参照）。

/// `POST /api/v1/values/batch` の1エントリぶんの request body。`v` は単票
/// [`WriteValueRequest::v`]・[`parse_requested_value`] と同じ正規化。
#[derive(Debug, Deserialize, ToSchema)]
struct BatchWriteEntryRequest {
    /// 外部名 `{connection}.{group}.{tag}`。
    tag: String,
    /// 書き込む工学値(単票と同じ - 数値タグには数値、bit タグには真偽値)。
    /// 2026-09-05 レビュー対応: `#[schema(value_type = f64)]` を外した -
    /// 数値タグには数値、bit タグには真偽値を送れる(`parse_requested_value`
    /// 参照)にもかかわらず、以前は OpenAPI スキーマ上 `f64` 固定になって
    /// おり、bit タグへの `true`/`false` が生成ドキュメント上表現できて
    /// いなかった。注釈を外すと `serde_json::Value` の既定の
    /// `ToSchema`(utoipa 5 組み込み、`schema_type = AnyValue`)にフォール
    /// バックし、number・boolean のどちらも受理できることが正しく表れる。
    /// 単票 [`WriteValueRequest::v`] は同じ `f64` 固定の問題を抱えている
    /// が、今回の指摘対象はバッチのみなのでそちらは変更しない(別の既存
    /// 課題として残す)。
    v: serde_json::Value,
}

/// `POST /api/v1/values/batch` の request body（設計 §3.3
/// 「`[{tag, value}, ...]` を受ける」）。
#[derive(Debug, Deserialize, ToSchema)]
struct BatchWriteValueRequest {
    writes: Vec<BatchWriteEntryRequest>,
}

/// per-entry 結果の1件分。成功時は `ok: true` のみ、失敗時は `ok: false` +
/// [`crate::write_path::WriteRejection::rest_error_code`]/`detail`
/// （単票のエラー本文と同じ語彙 - `error`/`detail` フィールド名も揃える）。
#[derive(Debug, Serialize, ToSchema)]
struct BatchWriteEntryResponse {
    tag: String,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

impl From<crate::write_path::BatchEntryOutcome> for BatchWriteEntryResponse {
    fn from(outcome: crate::write_path::BatchEntryOutcome) -> Self {
        match outcome.result {
            Ok(()) => Self {
                tag: outcome.tag,
                ok: true,
                error: None,
                detail: None,
            },
            Err(rejection) => Self {
                tag: outcome.tag,
                ok: false,
                error: Some(rejection.rest_error_code().to_string()),
                detail: rejection.detail(),
            },
        }
    }
}

/// `POST /api/v1/values/batch` の応答。**常に 200**（設計 §3.3・このモジュール
/// の `BatchTagsResponse` doc comment と同じ判断 - 事前ゲート
/// all-or-nothing による全体中止は例外ではなく通常の応答で、クライアントは
/// `writes[].ok`/`error`/`detail` を行ごとに見る）。認証エラー(403)や JSON
/// パース失敗(400)は既存の `ApiError`/`Json` extractor 経路のまま区別する。
#[derive(Debug, Serialize, ToSchema)]
struct BatchWriteValuesResponse {
    writes: Vec<BatchWriteEntryResponse>,
}

/// `POST /api/v1/values/batch` - レシピ一括書き込み（T20-3a）。
///
/// 事前段(a): セッション token では書けない([`v1_write_value`]と同じ)。
/// 事前段(b): **各エントリの** `write:{tag}` スコープを検査する - 1件でも
/// スコープ不足なら(事前ゲート all-or-nothing の思想を認証にも適用)
/// **全体を拒否**して 403 `missing_write_scope` を返し、1件も書かない
/// (`crate::write_path::execute_write_batch` へは進まない)。
///
/// スコープを通過した後は、`v` の正規化(単票と同じ
/// [`parse_requested_value`])を済ませ、ゲート1〜8の本体をまるごと
/// `crate::write_path::execute_write_batch` へ委譲する。
#[utoipa::path(
    post,
    path = "/api/v1/values/batch",
    request_body = BatchWriteValueRequest,
    responses(
        (status = 200, description = "常に200 - per-entry の ok/error は writes[] を参照", body = BatchWriteValuesResponse),
        (status = 403, description = "missing_write_scope（1件でもスコープ不足）/ session_token_cannot_write"),
    ),
    tag = "tag-space",
)]
async fn v1_write_values_batch(
    State(state): State<WriteState>,
    ctx: Option<Extension<ApiKeyContext>>,
    Json(body): Json<BatchWriteValueRequest>,
) -> Response {
    let Some(Extension(ctx)) = ctx else {
        return session_token_cannot_write_response();
    };
    // 事前ゲート all-or-nothing をスコープ検査にも適用する(設計 §3.3
    // REST 節): 1件でも write スコープが無ければ、どのエントリも
    // execute_write_batch へ進めず全体を 403 で拒否する。
    if body.writes.iter().any(|w| !ctx.has_write_scope(&w.tag)) {
        return missing_write_scope_response();
    }

    let entries: Vec<(String, Option<crate::write_path::RequestedValue>)> = body
        .writes
        .into_iter()
        .map(|w| (w.tag, parse_requested_value(&w.v)))
        .collect();

    let deps = crate::write_path::WriteDeps {
        manager: state.manager.as_ref(),
        collection_controller: state.collection_controller.as_deref(),
        api_keys: &state.api_keys,
        write_audit: &state.write_audit,
        write_control: state.write_control.as_ref(),
        rate_limiter: state.rate_limiter.as_ref(),
        events: &state.events,
    };

    let outcomes = crate::write_path::execute_write_batch(&deps, &ctx, entries).await;
    Json(BatchWriteValuesResponse {
        writes: outcomes.into_iter().map(Into::into).collect(),
    })
    .into_response()
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
        v1_value_read_now,
        v1_write_value,
        v1_write_values_batch,
        v1_status,
        v1_events,
    ),
    components(schemas(
        TagEntry,
        CatalogResponse,
        CatalogTagEntry,
        ValueEntry,
        SingleValueResponse,
        ReadNowResponse,
        ValuesResponse,
        ConnectionStatusEntry,
        MqttStatusEntry,
        GrpcStatusEntry,
        StatusResponse,
        EventEntry,
        EventsResponse,
        WriteValueRequest,
        WriteValueResponse,
        BatchWriteEntryRequest,
        BatchWriteValueRequest,
        BatchWriteEntryResponse,
        BatchWriteValuesResponse,
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
    } else {
        // banto v1.7.0 #204: セッション token もアカウントと照合する
        // （`require_auth_or_commissioning` と同じ判断: 失効は 401、DB が
        // 答えられなければトークンを残して 500）。
        match state.auth.authenticate(&token).await {
            Ok(Some(_)) if is_write_route => session_token_cannot_write_response(),
            Ok(Some(session)) => {
                req.extensions_mut().insert(session);
                next.run(req).await
            }
            Ok(None) => unauthorized_response(),
            Err(err) => ApiError(err).into_response(),
        }
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
    // T19 S3-b（docs/banto-hub-t19-design.md §3.9、UX-46）: `GET
    // /api/v1/status` の `system` のため - `admin_status_router`/
    // `admin_tag_stream_router`へ渡すものと**同じ** `Arc`（上記
    // `mqtt`/`write_control`と同じ共有規律）。
    system_info: Arc<SystemInfoSampler>,
    // 外部 DB 連携 S4: `GET /api/v1/status` の `sink` 節のため -
    // `admin_status_router`/`admin_tag_stream_router`/`sink_admin_router`
    // へ渡すものと**同じ** `Arc`。
    sink_status: Arc<SinkStatusStore>,
) -> Router {
    let state = TagSpaceState {
        manager: manager.clone(),
        controller: controller.clone(),
        write_control: write_control.clone(),
        mqtt,
        system_info,
        sink_status,
        computed: manager.computed_engine(),
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
        // T20 ①b(docs/banto-hub-t20-design.md §3.1): read-on-demand -
        // 静的セグメント `read-now` は axum(matchit)のルーティングで
        // `{tag}`より深い階層なので `/api/v1/values/{tag}` とは衝突しない
        // (T20-3a の `/api/v1/values/batch` と同じ「静的セグメント優先」の
        // 理屈)。
        .route("/api/v1/values/{tag}/read-now", get(v1_value_read_now))
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
        // T20-3a: 静的セグメント `batch` は axum(matchit)のルーティングで
        // `{tag}` パラメータより優先して一致する - `"batch"`という外部名の
        // タグは作れない(`banto_tags`のタグ名バリデーションに依存しない、
        // ルーティング層の静的優先で保証される)ので衝突しない。
        .route("/api/v1/values/batch", post(v1_write_values_batch))
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
                operation: OperationKind::Normal,
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
    // T2-4（設計 §6）: 書き込み受付フラグ（既定 enabled、起動時に永続値を
    // 復元。2026-09-09 オーナー決定 #340）と書き込み監査サービス - どちらも
    // `bin/banto-hub.rs`（本番）または各テストのセットアップで一度だけ
    // 構築し、ここに注入する（`ApiKeysService` 等の他サービスと同じ規約）。
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
    // 互換ルーター（`api_router`、自前の `CollectionController` を内部で
    // 作る既存の埋め込み先/テスト向け）として組み立てるか。
    //
    // #341（2026-09-14）までは `legacy_live_reconfigure` という名前で、
    // `commit_catalog_and_notify` の「registry write でそのまま `apply_run`
    // する」pre-T14-3 挙動の opt-in も兼ねていた。その役割は
    // `crate::controller::CollectionController::commit_catalog_and_apply_live`
    // （全ルーター共通の live re-apply）が引き取ったので削除し、**残る唯一の
    // 意味**である T14-4 の `enforce_collection_state`（書き込みが収集状態を
    // 見るか ＝ `tag_space_router`/`crate::mcp::mcp_router` へ
    // `!legacy_compat_router` として渡す）に名前を合わせた。
    legacy_compat_router: bool,
    // T16-2 第三スライス（docs/banto-hub-t16-design.md §5）:
    // `GET /api/v1/openapi.json`の`info.x-banto-hub-profile-id`に埋め込む
    // この Hub インスタンス自身の profile-id - `HubRuntime::start`
    // （`crate::runtime`）が`HubConfig::profile_id`をそのまま渡す。
    profile_id: String,
) -> Router {
    let commissioning_state = commissioning.state();
    // T19 S3-b（docs/banto-hub-t19-design.md §3.9、UX-46）: 1プロセスにつき
    // 1個だけ構築し、`admin_status_router`・`admin_tag_stream_router`・
    // `tag_space_router`の3者へ同じ `Arc` を配る - `mqtt`/`write_control`
    // 等、このファイルの他の共有状態と同じ規律（`SystemInfoSampler`の
    // モジュール doc comment「共有 System を使い回す」の前提そのもの）。
    let system_info = Arc::new(SystemInfoSampler::new());
    // 外部 DB 連携 S4（`crate::sink`のモジュール doc comment参照）:
    // `system_info`と同じ「ここで1個だけ構築し、複数ルーターへ同じ`Arc`/
    // サービスを配る」規律。`sink_groups`（`SqlitePool`ベース、`Clone`可能）
    // は `sink_groups_router`（CRUD）と `sink_admin_router`
    // （`GET /api/sink/config`）の両方が使う。`sink_status`は
    // `sink_admin_router`（`PUT /api/sink/status`が書く）・
    // `admin_status_router`/`admin_tag_stream_router`/`tag_space_router`
    // （`GET /api/status`・`GET /api/v1/status`が読む）で共有する。
    let sink_groups = SinkGroupService::new(manager.pool());
    let sink_status = SinkStatusStore::new();

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
            // T21 S3: `crate::mcp`の`lock_down`ツール用にフル
            // `CommissioningService`を後段の`mcp_router`呼び出しへも渡す
            // 必要があるため、ここでは`.clone()`する（このファイルの
            // `Arc`/サービス共有規律と同じ - `commissioning`自体は`Clone`）。
            commissioning.clone(),
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
        ))
        // 外部 DB 連携 S4（design §5.2・§6-13）: `hub_sink_groups`の CRUD -
        // `tag_registry_router`と同じ認可（require_editor 書き込み・viewer
        // 読み取り）だが pending queue には載らない
        // （`sink_groups_router`のdoc comment参照）。`sink_groups`は下の
        // `mcp_router`・末尾の`sink_admin_router`でも使うため`.clone()`。
        .merge(sink_groups_router(
            sink_groups.clone(),
            audit.clone(),
            auth.clone(),
            commissioning_state.clone(),
            events.clone(),
        ))
        .merge(pending_changes_router(
            pending_changes,
            // `sink_admin_router`（末尾）が`GET /api/sink/config`で
            // 接続情報（パスワード含む）を読むために`plc_connections`を
            // 再利用する - ここで`.clone()`して1つ先に残す。
            plc_connections.clone(),
            collection_groups,
            tags,
            audit.clone(),
            auth.clone(),
            commissioning_state.clone(),
            manager.clone(),
            controller.clone(),
            events.clone(),
        ))
        .merge(write_control_router(
            write_control.clone(),
            manager.clone(),
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
        .merge(store_settings_router(
            manager.clone(),
            audit.clone(),
            auth.clone(),
            commissioning_state.clone(),
        ))
        .merge(grpc_settings_router(
            manager.clone(),
            // T21 S2-b: `crate::mcp::mcp_router`(下の`.merge`)へも同じ
            // `Arc<GrpcServer>`を渡す必要があるため、ここでは`.clone()`する
            // （このファイルの他の共有`Arc`規律と同じ - `grpc_server`自体は
            // 元のまま下の`mcp_router`呼び出しへ渡す）。
            grpc_server.clone(),
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
        // `manager`/`controller`/`write_control`/`mqtt`/
        // `system_info`の各`Arc`は、下の`tag_space_router`/
        // `admin_tag_stream_router`へ渡すものと**同じ**インスタンスの
        // `clone()` - 別インスタンスを作ると状態が分裂する（このファイルの
        // 他の`Arc`共有規律と同じ）。
        .merge(admin_status_router(
            manager.clone(),
            controller.clone(),
            write_control.clone(),
            mqtt.clone(),
            system_info.clone(),
            sink_status.clone(),
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
            mqtt.clone(),
            system_info.clone(),
            sink_status.clone(),
            auth.clone(),
            commissioning_state.clone(),
        ))
        // T19 S5（docs/banto-hub-t19-design.md §3.7、UX-41）: `POST /mcp` -
        // `tag_space_router`と同じ形で、CSRF レイヤー（`admin`）の外側に
        // `.merge()`する。API キー認証（`bh_`のみ、`crate::mcp`のモジュール
        // doc comment「認証」節参照）を自前で持つため、CSRF ヘッダは不要
        // （`/api/v1/*`と同じ扱い）。以降の各引数は他ルーターと同じ
        // `Arc`/サービスを`.clone()`で配る（`crate::mcp::mcp_router`の
        // doc comment「呼び出し元は...同じインスタンスを渡すこと」参照）-
        // 直後の`tag_space_router`呼び出しがこれらの元の値を消費(move)する
        // ので、ここでは必ず`.clone()`する。
        .merge(crate::mcp::mcp_router(
            manager.clone(),
            controller.clone(),
            api_keys.clone(),
            write_audit.clone(),
            write_control.clone(),
            rate_limiter.clone(),
            events.clone(),
            // 外部 DB 連携 S4: `sink_admin_router`（下）でも使うため
            // `.clone()`にする（従来は最後の使用箇所だったので bare move
            // だった）。
            commissioning_state.clone(),
            mqtt.clone(),
            system_info.clone(),
            !legacy_compat_router,
            // T21 S1-b（docs/banto-hub-t21-design.md §5）: 構成補助ツール用
            // - REST の他ルーターと同じ `audit` をそのまま共有する
            // （このファイルの `.merge` 呼び出し規律と同じ）。
            audit.clone(),
            // T21 S2-b: 設定 get/set ツール用 - 上の `grpc_settings_router`と
            // 同じ `Arc<GrpcServer>`（`grpc_server.clone()`）を共有する
            // （このファイルの `Arc` 共有規律と同じ）。
            grpc_server,
            // T21 S3: `lock_down`ツール用 - `commissioning_router`に渡した
            // ものと同じフル `CommissioningService`（上の`.clone()`参照）。
            // ここが最後の使用箇所なので`.clone()`しない。
            commissioning,
            // 外部 DB 連携 S4: `list_sink_groups`等 T21 系 MCP ツール用。
            // `sink_admin_router`（下）でも使うため`.clone()`する。
            sink_groups.clone(),
            sink_status.clone(),
        ))
        // 外部 DB 連携 S4（design §5.2・§6-15）: サイドカー専用の admin 面。
        // `tag_space_router`/`mcp_router`と同じ形で、CSRF レイヤー（`admin`）
        // の外側に`.merge()`する - 認証は`require_sink_admin`が自前で
        // 行うため CSRF ヘッダは不要（`/api/v1/*`・`POST /mcp`と同じ扱い）。
        .merge(sink_admin_router(
            sink_groups,
            plc_connections,
            manager.clone(),
            sink_status.clone(),
            api_keys.clone(),
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
            !legacy_compat_router,
            system_info,
            sink_status,
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
    // #341 レビュー対応（2026-09-14）: DB Source の計画もここで検証する。
    // `CollectorManager::commit_catalog` は catalog・演算 plan・DB Source
    // plan の3つを検証してから入れ替えるが、preflight が前2つしか見て
    // いなかったため「保存は通ったのに commit_catalog が検証で落ちる」
    // （= `commit_catalog_and_notify` が 500 を返す）余地が残っていた。
    // 3つ揃えたことで、保存後の commit で残る失敗要因はストレージ障害と
    // 走行中の collector 側の適用失敗だけになる（保存前検証が「保存成功 ＝
    // 実行可能」を保証する T14-3 の設計どおり）。
    crate::db_source::build_plan(snapshot)
        .map_err(|err| preflight_api_error(format!("DB Source の検証に失敗しました: {err}")))?;
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

// T21 S1-b（docs/banto-hub-t21-design.md §5）: `pub(crate)` so
// `crate::mcp`'s config tools (`create_connection`/`delete_connection`) can
// run the exact same in-transaction preflight REST does - signature and body
// unchanged.
pub(crate) async fn preflight_transaction(
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
    let controller = Arc::new(crate::controller::CollectionController::new(
        manager.clone(),
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
        let sim_registry = Arc::new(crate::broker_glue::BrokerSimRegistry::new());
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
        /// #341 レビュー対応: router が使っているものと**同じ**
        /// `CollectorManager`。`fail_next_apply_for_test`（テスト専用の
        /// 失敗注入、`crate::hub` 参照）を endpoint 越しのテストから
        /// 呼ぶために持つ。
        manager: Arc<CollectorManager>,
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
        // banto v1.7.0 #204: 本番（`user_auth_state`）と同じ照合を入れる。
        // 検証関数は監査を記録しない版のまま（既存テストの監査件数を変えない）。
        let auth = AuthState::new(
            move |u: String, p: String| {
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
            },
            SessionValidation::lookup(user_session_lookup(users.clone())),
        );
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
            manager.clone(),
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
            manager,
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
        // T19 S2-a (UX-48): a connection with zero collection groups (this
        // one - it was created with no group at all) reports "unused", not
        // "stopped"/"reconnecting" - it never gets a broker session synced
        // in the first place (`CollectorManager::sync_broker_sessions_from`'s
        // doc comment), so lumping it in with "stopped" would read as
        // broken rather than simply not set up yet.
        assert_eq!(json["connections"][0]["status"], "unused");
    }

    /// T19 S2-a (UX-48): the flip side of the assertion added to
    /// `tags_create_via_admin_router_rebuilds_the_catalog` above - once a
    /// connection has an enabled group and tag, its status is no longer
    /// "unused" even before any explicit rebuild/run has synced a broker
    /// session for it (it falls back to the pre-existing "stopped" rounding
    /// documented on `compute_status`, not the new "unused" bucket).
    #[tokio::test]
    async fn a_connection_with_a_tag_is_never_reported_unused() {
        let (router, token, _dir) = router_with_token().await;

        let (status, conn) = admin_post(
            &router,
            "/api/plc-connections",
            &token,
            json!({ "name": "line1", "host": "127.0.0.1", "port": 15023 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{conn:?}");
        let (status, group) = admin_post(
            &router,
            "/api/collection-groups",
            &token,
            json!({ "name": "g1", "plcConnectionId": conn["id"], "periodMs": 1000 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{group:?}");
        let (status, tag) = admin_post(
            &router,
            "/api/tags",
            &token,
            json!({
                "name": "t1",
                "collectionGroupId": group["id"],
                "address": "40001",
                "dataType": "i16",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{tag:?}");

        let (status, status_body) = v1_get(&router, &token, "/api/v1/status").await;
        assert_eq!(status, StatusCode::OK, "{status_body:?}");
        assert_eq!(status_body["connections"].as_array().unwrap().len(), 1);
        assert_ne!(status_body["connections"][0]["status"], "unused");
    }

    /// T19 S2-a 案B (UX-48, docs/banto-hub-t19-design.md §3.8, 2026-09-03)
    /// で追加、#341 (2026-09-14) で live re-apply 経路の回帰テストとして
    /// 引き継いだ: a manager/controller pair without the router/RBAC/session
    /// layer around it, so the tests below can call
    /// `commit_catalog_and_notify` (this file's own commit entry point - the
    /// ONE function every registry-mutation handler and
    /// `execute_pending_apply` funnel through, this fn's own doc comment)
    /// directly while the controller is `Running`, without going through a
    /// router whose `registry_change_should_queue` policy (試運転か
    /// ロックダウン済みか) would decide for them. What these tests pin down
    /// is the layer underneath that policy:
    /// `CollectionController::commit_catalog_and_apply_live` must sync
    /// broker sessions for a change committed while `Running` (案B の要件)
    /// and must NOT dial anything while `Stopped` (T15-4 の要件) - both
    /// regardless of which REST-level policy sent the change here.
    async fn catalog_resync_test_env() -> (
        Arc<CollectorManager>,
        Arc<CollectionController>,
        broadcast::Sender<ServerEvent>,
        sqlx::SqlitePool,
        tempfile::TempDir,
    ) {
        let pool = migrate_memory().await.expect("migrate_memory");
        let (manager, dir) = test_manager_with_clock(pool.clone(), Arc::new(SystemClock));
        let controller = Arc::new(CollectionController::new(manager.clone()));
        let (events, _rx) = tokio_broadcast::channel(16);
        (manager, controller, events, pool, dir)
    }

    /// T19 S2-a 案B, "most important" per the implementation instructions:
    /// a catalog change committed while collection is `Stopped` must NOT
    /// dial a broker session - `CollectionController::
    /// commit_catalog_and_apply_live` only touches the collector/broker
    /// sessions while `Running` (its own doc comment). This is the direct evidence that 案B does not
    /// reopen the T15-4 hazard (`crate::write_path`'s module doc comment,
    /// "gate 8 は broker セッションを新規に張らない") of dialing a PLC the
    /// operator meant to leave stopped.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn commit_catalog_and_notify_does_not_dial_a_session_while_collection_is_stopped() {
        let (manager, controller, events, pool, _dir) = catalog_resync_test_env().await;
        assert_eq!(controller.status().state, CollectionState::Stopped);

        let conn = PlcConnectionService::new(pool.clone())
            .create(
                serde_json::from_value::<PlcConnectionPayload>(json!({
                    "name": "line-resync-stopped",
                    "host": "127.0.0.1",
                    "port": 15041,
                }))
                .unwrap()
                .into(),
            )
            .await
            .unwrap();
        let group = CollectionGroupService::new(pool.clone())
            .create(
                serde_json::from_value::<CollectionGroupPayload>(json!({
                    "name": "g1",
                    "plcConnectionId": conn.id,
                    "periodMs": 1000,
                }))
                .unwrap()
                .into(),
            )
            .await
            .unwrap();
        TagService::new(pool.clone())
            .create(
                serde_json::from_value::<TagPayload>(json!({
                    "name": "t1",
                    "collectionGroupId": group.id,
                    "address": "40001",
                    "dataType": "i16",
                }))
                .unwrap()
                .into(),
            )
            .await
            .unwrap();

        commit_catalog_and_notify(&controller, &events, "tags")
            .await
            .expect("live apply");

        assert!(
            !manager.sessions().connection_ids().contains(&conn.id),
            "a catalog change committed while stopped must not dial a broker session"
        );
        assert!(
            manager.write_broker_handle_peek(conn.id).is_none(),
            "no session exists, so the write path must still fail closed"
        );
    }

    /// T19 S2-a 案B: the "add" case - a tag (and its group) registered under
    /// a previously-tagless connection while collection is already
    /// `Running` gets a broker session synced immediately on that catalog
    /// commit, not only at the next `start`/`stop` cycle.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn commit_catalog_and_notify_syncs_a_session_for_a_newly_tagged_connection_while_running()
    {
        let (manager, controller, events, pool, _dir) = catalog_resync_test_env().await;

        let conn = PlcConnectionService::new(pool.clone())
            .create(
                serde_json::from_value::<PlcConnectionPayload>(json!({
                    "name": "line-resync-add",
                    "host": "127.0.0.1",
                    "port": 15042,
                }))
                .unwrap()
                .into(),
            )
            .await
            .unwrap();

        let started = controller.start(RunMode::Configured).await;
        assert_eq!(started.state, CollectionState::Running);
        // Zero collection groups yet (T19 S2-a) - genuinely tagless, so
        // `apply_run` (inside `start`) did not sync a broker session for it.
        assert!(!manager.sessions().connection_ids().contains(&conn.id));

        let group = CollectionGroupService::new(pool.clone())
            .create(
                serde_json::from_value::<CollectionGroupPayload>(json!({
                    "name": "g1",
                    "plcConnectionId": conn.id,
                    "periodMs": 1000,
                }))
                .unwrap()
                .into(),
            )
            .await
            .unwrap();
        TagService::new(pool.clone())
            .create(
                serde_json::from_value::<TagPayload>(json!({
                    "name": "t1",
                    "collectionGroupId": group.id,
                    "address": "40001",
                    "dataType": "i16",
                }))
                .unwrap()
                .into(),
            )
            .await
            .unwrap();

        commit_catalog_and_notify(&controller, &events, "tags")
            .await
            .expect("live apply");

        assert!(
            manager.sessions().connection_ids().contains(&conn.id),
            "adding a tag to a tagless connection while running must sync a broker session \
             immediately (T19 S2-a 案B), not only at the next start/stop cycle"
        );
        assert!(
            manager.write_broker_handle_peek(conn.id).is_some(),
            "the write path must find the session peek-able right after the catalog commit"
        );

        controller.stop().await;
    }

    /// T19 S2-a 案B: the "remove" flip side - once a connection's last
    /// enabled collection group is disabled while collection is `Running`,
    /// its broker session is torn down on that same catalog commit, mirroring
    /// `CollectorManager::rebuild`'s own add+remove sync (T7-2).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn commit_catalog_and_notify_removes_a_session_once_the_last_group_is_disabled_while_running(
    ) {
        let (manager, controller, events, pool, _dir) = catalog_resync_test_env().await;

        let conn = PlcConnectionService::new(pool.clone())
            .create(
                serde_json::from_value::<PlcConnectionPayload>(json!({
                    "name": "line-resync-remove",
                    "host": "127.0.0.1",
                    "port": 15043,
                }))
                .unwrap()
                .into(),
            )
            .await
            .unwrap();
        let groups = CollectionGroupService::new(pool.clone());
        let group = groups
            .create(
                serde_json::from_value::<CollectionGroupPayload>(json!({
                    "name": "g1",
                    "plcConnectionId": conn.id,
                    "periodMs": 1000,
                }))
                .unwrap()
                .into(),
            )
            .await
            .unwrap();
        TagService::new(pool.clone())
            .create(
                serde_json::from_value::<TagPayload>(json!({
                    "name": "t1",
                    "collectionGroupId": group.id,
                    "address": "40001",
                    "dataType": "i16",
                }))
                .unwrap()
                .into(),
            )
            .await
            .unwrap();

        let started = controller.start(RunMode::Configured).await;
        assert_eq!(started.state, CollectionState::Running);
        assert!(
            manager.sessions().connection_ids().contains(&conn.id),
            "a connection with a tag must get a broker session synced on start, as before T19 S2-a"
        );

        groups
            .update(
                group.id,
                CollectionGroupInput {
                    name: group.name.clone(),
                    plc_connection_id: group.plc_connection_id,
                    period_ms: group.period_ms,
                    enabled: false,
                    default_writable: group.default_writable,
                    // 外部 DB 連携 S2: 既存行の SQL 文をそのまま持ち回る
                    // （この経路はグループを無効化するだけで、SQL 文を
                    // 書き換える意図は無い）。
                    query_sql: group.query_sql.clone(),
                },
            )
            .await
            .unwrap();

        commit_catalog_and_notify(&controller, &events, "collection_groups")
            .await
            .expect("live apply");

        assert!(
            !manager.sessions().connection_ids().contains(&conn.id),
            "disabling a connection's last enabled group while running must remove its broker \
             session on that same catalog commit (T19 S2-a 案B)"
        );
        assert!(
            manager.write_broker_handle_peek(conn.id).is_none(),
            "no session remains, so the write path must fail closed"
        );

        controller.stop().await;
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

    /// `admin_post` の `PUT` 版 - S1（外部 DB 連携）のパスワード意味論
    /// テスト（`password: None`が既存値維持、`Some("")`が消去、
    /// `Some(s)`が置換）がまとめて必要とするため追加。
    async fn admin_put(
        router: &Router,
        path: &str,
        token: &str,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let response = router
            .clone()
            .oneshot(
                HttpRequest::put(path)
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

    /// `admin_post` の `DELETE` 版 - T19 S2-b（UX-38）のカスケード削除
    /// テストがまとめて必要とするため追加。
    async fn admin_delete(
        router: &Router,
        path: &str,
        token: &str,
    ) -> (StatusCode, serde_json::Value) {
        let response = router
            .clone()
            .oneshot(
                HttpRequest::delete(path)
                    .header("Authorization", format!("Bearer {token}"))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
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

    /// `admin_post`/`admin_put`/`admin_delete`の`GET`版 - 外部 DB 連携 S4の
    /// sink group テストがまとめて必要とするため追加。
    async fn admin_get(
        router: &Router,
        path: &str,
        token: &str,
    ) -> (StatusCode, serde_json::Value) {
        let response = router
            .clone()
            .oneshot(
                HttpRequest::get(path)
                    .header("Authorization", format!("Bearer {token}"))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
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

    /// `v1_get`の`PUT`版 - 外部 DB 連携 S4の`PUT /api/sink/status`テスト用。
    /// `/api/sink/*`も`require_sink_admin`が自前で認証するため CSRF 対象外
    /// （`/api/v1/*`・`POST /mcp`と同じ扱い）。
    async fn v1_put(
        router: &Router,
        key: &str,
        path: &str,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let response = router
            .clone()
            .oneshot(
                HttpRequest::put(path)
                    .header("Authorization", format!("Bearer {key}"))
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

    /// #335 のテスト用 - `computed`/`map` を要求するようになった
    /// `value_source_for_tag`/`effective_simulation_for_tag`/
    /// `CatalogTagEntry::from_runtime` に渡す「何もコミットされていない」
    /// `ComputedEngine`。PLC/internal/db タグの分類は `map`/`computed` を
    /// 見ないので影響を受けない - computed タグは「入力なし」(`None`)扱いに
    /// なり、真にシミュレーション中の入力から`derived_simulation`に
    /// 昇格する経路は通らない(その経路は`tests/computed.rs`の統合テストで
    /// 検証する)。
    fn empty_computed_engine() -> ComputedEngine {
        ComputedEngine::new(Arc::new(crate::computed::ServerTagStore::new()))
    }

    #[test]
    fn rest_catalog_dto_separates_configured_and_effective_simulation() {
        let tag = metadata_test_tag(banto_tags::PLC_TAG_KIND, false, true);
        let all_simulation = metadata_test_status(CollectionState::Running, RunMode::AllSimulation);
        let configured = metadata_test_status(CollectionState::Running, RunMode::Configured);
        let stopped = metadata_test_status(CollectionState::Stopped, RunMode::AllSimulation);
        let map = TagMap::default();
        let computed = empty_computed_engine();

        let all_simulation_json = serde_json::to_value(CatalogTagEntry::from_runtime(
            &tag,
            &all_simulation,
            &map,
            &computed,
        ))
        .expect("catalog DTO serializes");
        assert_eq!(all_simulation_json["simulation"], false);
        assert_eq!(all_simulation_json["configured_simulation"], false);
        assert_eq!(all_simulation_json["effective_simulation"], true);
        assert_eq!(all_simulation_json["value_source"], "simulation");

        let all_simulation_catalog = serde_json::to_value(CatalogResponse {
            revision: 4,
            run_id: all_simulation.run_id,
            collection_mode: all_simulation.mode.as_str().to_string(),
            tags: vec![CatalogTagEntry::from_runtime(
                &tag,
                &all_simulation,
                &map,
                &computed,
            )],
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
            tags: vec![CatalogTagEntry::from_runtime(
                &tag,
                &configured,
                &map,
                &computed,
            )],
        })
        .expect("configured catalog response serializes");
        assert_eq!(configured_catalog["run_id"], 9);
        assert_eq!(configured_catalog["collection_mode"], "configured");
        assert_eq!(configured_catalog["tags"][0]["value_source"], "real");

        assert!(
            !CatalogTagEntry::from_runtime(&tag, &configured, &map, &computed).effective_simulation
        );
        assert!(
            !CatalogTagEntry::from_runtime(&tag, &stopped, &map, &computed).effective_simulation
        );
        assert_eq!(
            CatalogTagEntry::from_runtime(&tag, &configured, &map, &computed).value_source,
            "real"
        );
        assert_eq!(
            CatalogTagEntry::from_runtime(&tag, &stopped, &map, &computed).value_source,
            "real"
        );
        let stopped_catalog = serde_json::to_value(CatalogResponse {
            revision: 4,
            run_id: stopped.run_id,
            collection_mode: stopped.mode.as_str().to_string(),
            tags: vec![CatalogTagEntry::from_runtime(
                &tag, &stopped, &map, &computed,
            )],
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
        assert!(
            !CatalogTagEntry::from_runtime(&saved_simulation, &stopped, &map, &computed)
                .effective_simulation
        );
    }

    /// #335（2026-09-14 オーナー決定）: computed の既定ラベルは
    /// `"computed"` - `derived_simulation`は AllSimulation 運転中（または
    /// 入力が真にシミュレーション中、`tests/computed.rs`の統合テスト参照）
    /// だけの情報ラベルに変わった。
    #[test]
    fn rest_value_source_uses_safe_tag_kind_classification() {
        let all_simulation = metadata_test_status(CollectionState::Running, RunMode::AllSimulation);
        let configured = metadata_test_status(CollectionState::Running, RunMode::Configured);
        let plc = metadata_test_tag(banto_tags::PLC_TAG_KIND, false, true);
        let computed_tag = metadata_test_tag(banto_tags::COMPUTED_TAG_KIND, false, true);
        let internal = metadata_test_tag(banto_tags::INTERNAL_TAG_KIND, false, true);
        let map = TagMap::default();
        let computed = empty_computed_engine();

        assert_eq!(
            value_source_for_tag(&plc, &all_simulation, &map, &computed),
            "simulation"
        );
        assert_eq!(
            value_source_for_tag(&plc, &configured, &map, &computed),
            "real"
        );
        assert_eq!(
            value_source_for_tag(&computed_tag, &all_simulation, &map, &computed),
            "derived_simulation",
            "AllSimulation 運転中は入力の有無によらず derived_simulation"
        );
        assert_eq!(
            value_source_for_tag(&computed_tag, &configured, &map, &computed),
            "computed",
            "Configured 運転中・入力がコミットされていない computed の既定は computed"
        );
        assert_eq!(
            value_source_for_tag(&internal, &all_simulation, &map, &computed),
            "internal"
        );
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

    /// #341（2026-09-14）: 旧 `collection_edit_lock_is_http_409_with_current_status`
    /// の置き換え。「収集中は一律 409」は撤回されたので、代わりに検証するのは
    /// 「DB へは入ったが実行構成へ反映できなかった」ときの応答形
    /// （200 + 警告ではなく 500 + `live_reconfigure_failed`）。
    #[tokio::test]
    async fn live_apply_failure_is_http_500_with_live_reconfigure_failed() {
        let response =
            RegistryMutationError::LiveApplyFailed("catalog の検証に失敗しました".to_string())
                .into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"], "live_reconfigure_failed");
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .contains("catalog の検証に失敗しました"),
            "{body:?}"
        );
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
            "ロックダウン済みで収集中のため、変更を未適用キューに保存しました。"
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

    /// #341（オーナー決定 2026-09-09、docs/tag-server-design.md §4.3）: 上の
    /// `*_while_running_is_accepted_and_queued` 3本の**試運転中の兄弟** -
    /// 同じ「収集中」でも、ロックダウン前（`test_env_unlocked`）なら pending
    /// queue を経由せずその場で反映される。202 ではなく 200、
    /// `pending_changes` 行は増えず、`configured_revision` が進む。
    #[tokio::test]
    async fn tags_create_while_running_and_commissioning_is_applied_immediately() {
        let env = test_env_unlocked().await;

        let (status, conn) = admin_post(
            &env.router,
            "/api/plc-connections",
            &env.admin_token,
            json!({ "name": "line1", "host": "127.0.0.1", "port": 15122 }),
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

        let (_, before) = admin_get(&env.router, "/api/status", &env.admin_token).await;
        let revision_before = before["configuredRevision"].as_u64().unwrap();

        let (status, created) = admin_post(
            &env.router,
            "/api/tags",
            &env.admin_token,
            json!({
                "name": "temp01",
                "collectionGroupId": group["id"],
                "address": "40001",
                "dataType": "i16",
                "enabled": true
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created:?}");
        assert_eq!(created["name"], "temp01", "{created:?}");
        assert!(created["queued"].is_null(), "{created:?}");

        let queued_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pending_changes")
            .fetch_one(&env.pool)
            .await
            .unwrap();
        assert_eq!(queued_count, 0, "試運転中は pending queue を経由しない");

        let (_, after) = admin_get(&env.router, "/api/status", &env.admin_token).await;
        assert!(
            after["configuredRevision"].as_u64().unwrap() > revision_before,
            "{after:?}"
        );
        // 収集は止まっていない（無停止反映）。
        assert_eq!(after["collectionState"], "running", "{after:?}");
    }

    /// #341: 接続の作成版（[`tags_create_while_running_and_commissioning_is_applied_immediately`]
    /// と同じ契約）。
    #[tokio::test]
    async fn plc_connections_create_while_running_and_commissioning_is_applied_immediately() {
        let env = test_env_unlocked().await;

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

        let (status, created) = admin_post(
            &env.router,
            "/api/plc-connections",
            &env.admin_token,
            json!({ "name": "line-commissioning", "host": "127.0.0.1", "port": 15123 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created:?}");
        assert_eq!(created["name"], "line-commissioning", "{created:?}");

        let queued_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pending_changes")
            .fetch_one(&env.pool)
            .await
            .unwrap();
        assert_eq!(queued_count, 0);
    }

    /// #341: 一括作成版（`tags.batch_create`）。
    #[tokio::test]
    async fn tags_batch_non_dry_run_while_running_and_commissioning_is_applied_immediately() {
        let env = test_env_unlocked().await;

        let (status, conn) = admin_post(
            &env.router,
            "/api/plc-connections",
            &env.admin_token,
            json!({ "name": "line1", "host": "127.0.0.1", "port": 15124 }),
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

        let (status, body) = admin_post(
            &env.router,
            "/api/tags/batch",
            &env.admin_token,
            json!({
                "dryRun": false,
                "tags": [{
                    "name": "temp01",
                    "collectionGroupId": group["id"],
                    "address": "40001",
                    "dataType": "i16",
                    "enabled": true
                }]
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(body["ok"], true, "{body:?}");
        assert_eq!(body["count"], 1, "{body:?}");

        let queued_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pending_changes")
            .fetch_one(&env.pool)
            .await
            .unwrap();
        assert_eq!(queued_count, 0);
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

    /// S1a レビュー対応（`banto_tags::PlcConnection::password`の doc
    /// comment「外部呼び出し元へ決してシリアライズしない」）: 収集稼働中に
    /// パスワード付きの`plc_connections.create`をキューへ入れると、
    /// (a) `202 Accepted`の応答、(b) `GET /api/pending-changes`、
    /// (c) `GET /api/pending-changes/{id}` のいずれの JSON にも平文
    /// パスワードが一切現れず（`passwordSet: true`だけが出る）、それでいて
    /// (d) 適用（apply）後は`PlcConnectionService::get`で見える実際の
    /// 保存済み行にはパスワードがちゃんと入っている（＝DB へ保存された
    /// `pending_changes.payload`自体は redact していない）ことを確認する。
    #[tokio::test]
    async fn pending_changes_endpoints_never_leak_a_queued_plc_connection_password() {
        const SECRET: &str = "s3cret-pw-do-not-leak";

        let env = test_env().await;

        // Collection needs to be running for a write to be queued instead of
        // applied immediately - seed an unrelated (non-postgres) connection +
        // group for that, same as the other pending-changes tests above.
        let (status, conn) = admin_post(
            &env.router,
            "/api/plc-connections",
            &env.admin_token,
            json!({ "name": "line-pw-redact-seed", "host": "127.0.0.1", "port": 15040 }),
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

        // Queue a postgres connection create WITH a password while running.
        let (status, queued_body) = admin_post(
            &env.router,
            "/api/plc-connections",
            &env.admin_token,
            json!({
                "name": "pgdb-pw-redact",
                "protocol": "postgres",
                "host": "10.0.0.9",
                "port": 5432,
                "database": "appdb",
                "username": "appuser",
                "password": SECRET
            }),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{queued_body:?}");
        assert!(
            !queued_body.to_string().contains(SECRET),
            "202 body must never contain the plaintext password: {queued_body:?}"
        );
        assert_eq!(
            queued_body["pending"]["payload"]["input"]["passwordSet"], true,
            "{queued_body:?}"
        );
        assert!(
            queued_body["pending"]["payload"]["input"]
                .get("password")
                .is_none(),
            "{queued_body:?}"
        );
        let pending_id = queued_body["pending"]["id"]
            .as_i64()
            .expect("pending id should exist");

        // GET /api/pending-changes (list): the secret must not appear
        // anywhere in the serialized body.
        let list_response = env
            .router
            .clone()
            .oneshot(
                HttpRequest::get("/api/pending-changes")
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(list_response.status(), StatusCode::OK);
        let list_bytes = axum::body::to_bytes(list_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let list_text = String::from_utf8(list_bytes.to_vec()).unwrap();
        assert!(
            !list_text.contains(SECRET),
            "GET /api/pending-changes must never contain the plaintext password: {list_text}"
        );
        let list_body: serde_json::Value = serde_json::from_str(&list_text).unwrap();
        let listed = list_body
            .as_array()
            .expect("array body")
            .iter()
            .find(|row| row["id"] == pending_id)
            .expect("queued pending change should be listed");
        assert_eq!(listed["payload"]["input"]["passwordSet"], true);
        assert!(listed["payload"]["input"].get("password").is_none());

        // GET /api/pending-changes/{id}: same guarantee.
        let get_response = env
            .router
            .clone()
            .oneshot(
                HttpRequest::get(format!("/api/pending-changes/{pending_id}"))
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get_response.status(), StatusCode::OK);
        let get_bytes = axum::body::to_bytes(get_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let get_text = String::from_utf8(get_bytes.to_vec()).unwrap();
        assert!(
            !get_text.contains(SECRET),
            "GET /api/pending-changes/{{id}} must never contain the plaintext password: {get_text}"
        );
        let get_body: serde_json::Value = serde_json::from_str(&get_text).unwrap();
        assert_eq!(get_body["payload"]["input"]["passwordSet"], true);
        assert!(get_body["payload"]["input"].get("password").is_none());

        // Stop and apply: the queued create must actually go through with
        // the real password - only the OUTBOUND representation is redacted,
        // never the stored `pending_changes.payload` the apply path reads.
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

        let apply_response = env
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
        assert_eq!(apply_response.status(), StatusCode::OK);
        let apply_bytes = axum::body::to_bytes(apply_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let apply_text = String::from_utf8(apply_bytes.to_vec()).unwrap();
        assert!(
            !apply_text.contains(SECRET),
            "apply response must never contain the plaintext password: {apply_text}"
        );
        let apply_body: serde_json::Value = serde_json::from_str(&apply_text).unwrap();
        assert_eq!(apply_body["state"], "applied");

        // But the actually-stored row DOES have the password - proving the
        // stored payload itself was never touched by the redaction above.
        let created = PlcConnectionService::new(env.pool.clone())
            .list(ListParams::default())
            .await
            .unwrap()
            .rows
            .into_iter()
            .find(|c| c.name == "pgdb-pw-redact")
            .expect("the queued postgres connection should have been created");
        assert_eq!(created.password.as_deref(), Some(SECRET));
    }

    /// #341（2026-09-14 オーナー回答「明示適用も無停止」）: 旧
    /// `pending_changes_apply_while_running_returns_409_and_keeps_queue_row`
    /// の置き換え。ロックダウン済み + 収集中に積まれた pending change は、
    /// **収集を止めずに**明示適用でき、queue 行は `applied` へ遷移し、
    /// 収集は `Running` のまま続く。
    #[tokio::test]
    async fn pending_changes_apply_while_running_applies_without_stopping() {
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
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["state"], "applied", "{body:?}");

        let pending = PendingChangesService::new(env.pool.clone())
            .get(pending_id)
            .await
            .unwrap();
        assert_eq!(pending.state, PendingChangeState::Applied);

        // 適用されたタグが実際にレジストリへ入っている。
        let (status, tags) = admin_get(&env.router, "/api/tags", &env.admin_token).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            tags.as_array()
                .unwrap()
                .iter()
                .any(|t| t["name"] == "temp-apply-running-01"),
            "{tags:?}"
        );

        // 収集は止まっていない。
        let (status, collection) = admin_get(&env.router, "/api/status", &env.admin_token).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(collection["collectionState"], "running", "{collection:?}");
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

        // #341（2026-09-14）: failed 行の作り方を差し替えた。以前は
        // 「収集稼働中の apply は 409」を使っていたが、その 409 自体が
        // 撤廃された（適用は無停止で通る）ので、代わりに**同じ名前のタグを
        // 先に直接作っておく**ことで、適用時に一意制約で失敗させる
        // （requeue が対象とする「一過性の失敗」の実例そのもの）。
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
        let (status, blocker) = admin_post(
            &env.router,
            "/api/tags",
            &env.admin_token,
            json!({
                "name": "temp-requeue-basic-01",
                "collectionGroupId": group["id"],
                "address": "40002",
                "dataType": "i16"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{blocker:?}");

        let apply_conflicting = env
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
        assert!(
            !apply_conflicting.status().is_success(),
            "{:?}",
            apply_conflicting.status()
        );

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

    /// #341 レビュー対応（2026-09-14）: エラー変換だけを見る
    /// `live_apply_failure_is_http_500_with_live_reconfigure_failed` とは別に、
    /// **endpoint 越し**に「DB への適用は成功したが実行構成への反映だけが
    /// 失敗した」状態を作り、契約4点を一度に固定する:
    ///
    /// 1. `POST /api/pending-changes/{id}/apply` は 500
    ///    `live_reconfigure_failed`（回復手段の案内つき）
    /// 2. pending change の行は **`applied`**（`failed` にしない - DB へは
    ///    入っているので requeue で二重適用させない）
    /// 3. 監査ログに `result = "failed"` の行が残る
    /// 4. 理由が `/api/status` の `lastConfigError` からも読める
    ///
    /// 反映失敗は `CollectorManager` のテスト専用フック
    /// （`fail_next_apply_for_test`、同モジュール参照）で決定的に起こす -
    /// 本物の失敗要因（tstore の writer を開けない）を OS 越しに再現するのは
    /// プラットフォーム依存が強すぎるため。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pending_apply_live_reconfigure_failure_is_500_and_marks_applied() {
        let env = test_env().await;

        let (status, conn) = admin_post(
            &env.router,
            "/api/plc-connections",
            &env.admin_token,
            json!({ "name": "line-live-fail", "host": "127.0.0.1", "port": 15026 }),
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

        // ロックダウン済み + 収集中なので queue へ。
        let (status, queued) = admin_post(
            &env.router,
            "/api/tags",
            &env.admin_token,
            json!({
                "name": "temp-live-fail-01",
                "collectionGroupId": group["id"],
                "address": "40001",
                "dataType": "i16",
                "enabled": true
            }),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{queued:?}");
        let pending_id = queued["pending"]["id"].as_i64().expect("pending id");

        // 次の `apply_run`（= 明示適用の live re-apply）だけを失敗させる。
        env.manager.fail_next_apply_for_test();

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
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"], "live_reconfigure_failed", "{body:?}");
        let message = body["message"].as_str().unwrap();
        assert!(message.contains("テスト用に注入された"), "{body:?}");
        assert!(
            message.contains("POST /api/collection/reapply"),
            "回復手段が案内されること: {body:?}"
        );

        // 2. DB へは適用済みなので行は applied（failed にしない）。
        assert_eq!(body["pending"]["state"], "applied", "{body:?}");
        let pending = PendingChangesService::new(env.pool.clone())
            .get(pending_id)
            .await
            .unwrap();
        assert_eq!(pending.state, PendingChangeState::Applied);
        let (status, tags) = admin_get(&env.router, "/api/tags", &env.admin_token).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            tags.as_array()
                .unwrap()
                .iter()
                .any(|t| t["name"] == "temp-live-fail-01"),
            "レジストリ行自体は入っていること: {tags:?}"
        );

        // #341 レビュー対応2: catalog も直前の成功構成へ戻っている
        // （読み側だけ新構成、という食い違いを残さない -
        // `CollectorManager::rollback_catalog`）。DB には在るが catalog
        // （`/api/v1/tags`）には**まだ現れない**のが正。
        let (status, catalog) = admin_get(&env.router, "/api/v1/tags", &env.admin_token).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            !catalog["tags"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["name"] == "temp-live-fail-01"),
            "反映に失敗したので catalog へは載らない: {catalog:?}"
        );

        // 3. 監査ログに失敗行が残る。
        let audited: Vec<(String, String)> = sqlx::query_as(
            "SELECT action, result FROM audit_log WHERE resource = 'pending_changes' ORDER BY id",
        )
        .fetch_all(&env.pool)
        .await
        .unwrap();
        assert!(
            audited
                .iter()
                .any(|(action, result)| action == "apply" && result == "failed"),
            "{audited:?}"
        );

        // 4. 理由が `/api/status` からも読める。
        let (status, runtime) = admin_get(&env.router, "/api/status", &env.admin_token).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            runtime["lastConfigError"]
                .as_str()
                .unwrap_or_default()
                .contains("テスト用に注入された"),
            "{runtime:?}"
        );
        // 収集自体は止まっていない。
        assert_eq!(runtime["collectionState"], "running", "{runtime:?}");

        // 回復手段（`LIVE_APPLY_RECOVERY_HINT`）どおり、再適用すれば載る。
        let reapply = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post("/api/collection/reapply")
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(reapply.status(), StatusCode::OK);
        let (status, catalog) = admin_get(&env.router, "/api/v1/tags", &env.admin_token).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            catalog["tags"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["name"] == "temp-live-fail-01"),
            "reapply で catalog へ載ること: {catalog:?}"
        );
    }

    /// #341 レビュー対応（2026-09-14）: 反映失敗からの回復手段
    /// `POST /api/collection/reapply` が、収集を止めずに最新の DB 内容を
    /// 適用し直せること（`collection_reapply` の doc comment）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn collection_reapply_reapplies_the_latest_registry_without_stopping() {
        let env = test_env().await;

        let (status, conn) = admin_post(
            &env.router,
            "/api/plc-connections",
            &env.admin_token,
            json!({ "name": "line-reapply", "host": "127.0.0.1", "port": 15027 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{conn:?}");

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
        let (_, before) = admin_get(&env.router, "/api/status", &env.admin_token).await;
        let run_id_before = before["runId"].as_u64();

        let reapply = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post("/api/collection/reapply")
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(reapply.status(), StatusCode::OK);

        let (_, after) = admin_get(&env.router, "/api/status", &env.admin_token).await;
        assert_eq!(after["collectionState"], "running", "{after:?}");
        assert_eq!(after["runId"].as_u64(), run_id_before, "{after:?}");
        assert!(after["lastConfigError"].is_null(), "{after:?}");

        // viewer は admin 専用ルーターに弾かれる。
        let forbidden = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post("/api/collection/reapply")
                    .header("Authorization", format!("Bearer {}", env.viewer_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    }

    /// TAG-P0-3 follow-up（2026-08-14）: 一過性の失敗は requeue → 失敗要因の
    /// 解消 → 再 apply で回復できることを確認する（#341 で「収集稼働中の
    /// 409」が無くなったため、失敗要因は同名タグの一意制約違反に差し替えた）。
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

        // #341（2026-09-14）: 一過性の失敗の作り方を差し替えた（上の
        // `pending_changes_requeue_endpoint_returns_pending_state` と同じ
        // 理由）- 同名タグを先に直接作って一意制約違反で失敗させ、その
        // 邪魔者を消してから requeue → 再 apply が通ることを確認する。
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
        let (status, blocker) = admin_post(
            &env.router,
            "/api/tags",
            &env.admin_token,
            json!({
                "name": "temp-requeue-transient-01",
                "collectionGroupId": group["id"],
                "address": "40002",
                "dataType": "i16"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{blocker:?}");
        let blocker_id = blocker["id"].as_i64().expect("blocker tag id");

        let apply_conflicting = env
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
        assert!(
            !apply_conflicting.status().is_success(),
            "{:?}",
            apply_conflicting.status()
        );

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

        // 失敗要因（同名タグ）を取り除く = 一過性の失敗が解消した状態。
        let delete_blocker = env
            .router
            .clone()
            .oneshot(
                HttpRequest::delete(format!("/api/tags/{blocker_id}"))
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(delete_blocker.status().is_success());

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

    // --- T19 S2-b (UX-38, docs/banto-hub-t19-design.md §3.4/§7.5,
    // 2026-09-02 オーナー決定「タグがあってもグループ・接続を削除可。定義の
    // み削除し、履歴は残す」) --------------------------------------------

    /// REST 経由の主張: `DELETE /api/plc-connections/{id}` は、配下に収集
    /// グループ・タグがあっても拒否せず、まとめて削除する
    /// （収集停止中は即時 - `plc_connections_delete` が
    /// `cascade_delete_tx` を呼ぶ経路）。
    #[tokio::test]
    async fn plc_connections_delete_cascades_when_it_has_groups_and_tags() {
        let env = test_env().await;

        let (status, conn) = admin_post(
            &env.router,
            "/api/plc-connections",
            &env.admin_token,
            json!({ "name": "line-cascade", "host": "127.0.0.1", "port": 15060 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{conn:?}");
        let conn_id = conn["id"].as_i64().unwrap();

        let (status, group) = admin_post(
            &env.router,
            "/api/collection-groups",
            &env.admin_token,
            json!({ "name": "g-cascade", "plcConnectionId": conn_id, "periodMs": 1000 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{group:?}");
        let group_id = group["id"].as_i64().unwrap();

        let (status, tag) = admin_post(
            &env.router,
            "/api/tags",
            &env.admin_token,
            json!({
                "name": "t-cascade",
                "collectionGroupId": group_id,
                "address": "40001",
                "dataType": "i16"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{tag:?}");
        let tag_id = tag["id"].as_i64().unwrap();

        // 削除前: 拒否ではなく成功する（旧仕様なら 422 Validation だった）。
        let (status, _body) = admin_delete(
            &env.router,
            &format!("/api/plc-connections/{conn_id}"),
            &env.admin_token,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        assert!(matches!(
            PlcConnectionService::new(env.pool.clone())
                .get(conn_id)
                .await
                .unwrap_err(),
            BantoError::NotFound { .. }
        ));
        assert!(matches!(
            CollectionGroupService::new(env.pool.clone())
                .get(group_id)
                .await
                .unwrap_err(),
            BantoError::NotFound { .. }
        ));
        assert!(matches!(
            TagService::new(env.pool.clone())
                .get(tag_id)
                .await
                .unwrap_err(),
            BantoError::NotFound { .. }
        ));
    }

    /// `DELETE /api/collection-groups/{id}` の同じ主張: タグがあっても
    /// 拒否せず、まとめて削除する。
    #[tokio::test]
    async fn collection_groups_delete_cascades_when_it_has_tags() {
        let env = test_env().await;

        let (status, conn) = admin_post(
            &env.router,
            "/api/plc-connections",
            &env.admin_token,
            json!({ "name": "line-cascade-2", "host": "127.0.0.1", "port": 15061 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{conn:?}");
        let conn_id = conn["id"].as_i64().unwrap();

        let (status, group) = admin_post(
            &env.router,
            "/api/collection-groups",
            &env.admin_token,
            json!({ "name": "g-cascade-2", "plcConnectionId": conn_id, "periodMs": 1000 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{group:?}");
        let group_id = group["id"].as_i64().unwrap();

        let (status, tag) = admin_post(
            &env.router,
            "/api/tags",
            &env.admin_token,
            json!({
                "name": "t-cascade-2",
                "collectionGroupId": group_id,
                "address": "40001",
                "dataType": "i16"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{tag:?}");
        let tag_id = tag["id"].as_i64().unwrap();

        let (status, _body) = admin_delete(
            &env.router,
            &format!("/api/collection-groups/{group_id}"),
            &env.admin_token,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        assert!(matches!(
            CollectionGroupService::new(env.pool.clone())
                .get(group_id)
                .await
                .unwrap_err(),
            BantoError::NotFound { .. }
        ));
        assert!(matches!(
            TagService::new(env.pool.clone())
                .get(tag_id)
                .await
                .unwrap_err(),
            BantoError::NotFound { .. }
        ));
        // The connection itself is untouched by a group-level delete.
        PlcConnectionService::new(env.pool.clone())
            .get(conn_id)
            .await
            .expect("the connection must survive a group delete");
    }

    /// UX-38 の対象外: 予約接続（calc/mem）はカスケード経路でも削除できない
    /// （§7.5「virtual 接続（calc/mem）の削除・編集禁止はそのまま維持
    /// する」）。
    #[tokio::test]
    async fn plc_connections_delete_still_refuses_a_virtual_connection() {
        let env = test_env().await;
        let calc = PlcConnectionService::new(env.pool.clone())
            .create(banto_tags::PlcConnectionInput {
                name: banto_tags::CALC_CONNECTION_NAME.to_string(),
                protocol: banto_tags::VIRTUAL_PROTOCOL.to_string(),
                host: String::new(),
                port: 0,
                unit_id: 1,
                enabled: true,
                simulation: false,
                word_order: "low_high".to_string(),
                database: None,
                username: None,
                password: None,
            })
            .await
            .unwrap();

        let (status, body) = admin_delete(
            &env.router,
            &format!("/api/plc-connections/{}", calc.id),
            &env.admin_token,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");

        PlcConnectionService::new(env.pool.clone())
            .get(calc.id)
            .await
            .expect("calc should survive");
    }

    // --- S1 (docs/banto-hub-external-db-design.md §4.1・§4.6・§6・§7):
    // PostgreSQL 接続エンティティ・接続テスト API ------------------------

    /// `POST /api/plc-connections` で `protocol: "postgres"` を作成すると、
    /// 応答に `passwordSet: true` が入り、`password` フィールド自体は
    /// 出ない（`PlcConnectionResponse`が生の`password`を決して外部へ
    /// 出さないことの回帰テスト）。
    #[tokio::test]
    async fn create_postgres_connection_returns_password_set_without_password_field() {
        let env = test_env().await;
        let (status, body) = admin_post(
            &env.router,
            "/api/plc-connections",
            &env.admin_token,
            json!({
                "name": "erp-db",
                "protocol": "postgres",
                "host": "10.0.0.50",
                "port": 5432,
                "database": "erp",
                "username": "reader",
                "password": "s3cret",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(body["passwordSet"], true);
        assert!(
            body.get("password").is_none(),
            "the response must never contain the raw password: {body:?}"
        );
        assert_eq!(body["database"], "erp");
        assert_eq!(body["username"], "reader");
    }

    /// `PUT`で`password`を省略（`null`/未指定）すると、既存のパスワードは
    /// 変更されない（`banto_tags::PlcConnectionInput::password`の doc
    /// comment、update の tri-state PATCH 意味論の REST 経由での確認）。
    #[tokio::test]
    async fn update_omitting_password_keeps_the_existing_password() {
        let env = test_env().await;
        let (status, created) = admin_post(
            &env.router,
            "/api/plc-connections",
            &env.admin_token,
            json!({
                "name": "erp-db-keep",
                "protocol": "postgres",
                "host": "10.0.0.50",
                "port": 5432,
                "database": "erp",
                "username": "reader",
                "password": "original-secret",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created:?}");
        let conn_id = created["id"].as_i64().unwrap();
        assert_eq!(created["passwordSet"], true);

        // password フィールドを省略した PUT（他は全項目必須）。
        let (status, updated) = admin_put(
            &env.router,
            &format!("/api/plc-connections/{conn_id}"),
            &env.admin_token,
            json!({
                "name": "erp-db-keep-renamed",
                "protocol": "postgres",
                "host": "10.0.0.50",
                "port": 5432,
                "unitId": 1,
                "enabled": true,
                "simulation": false,
                "wordOrder": "low_high",
                "database": "erp",
                "username": "reader",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{updated:?}");
        assert_eq!(
            updated["passwordSet"], true,
            "omitting password must keep it set"
        );

        let stored = PlcConnectionService::new(env.pool.clone())
            .get(conn_id)
            .await
            .unwrap();
        assert_eq!(stored.password.as_deref(), Some("original-secret"));
    }

    /// `PUT`で`password: ""`を送ると、パスワードが消去される。
    #[tokio::test]
    async fn update_with_empty_password_clears_it() {
        let env = test_env().await;
        let (status, created) = admin_post(
            &env.router,
            "/api/plc-connections",
            &env.admin_token,
            json!({
                "name": "erp-db-clear",
                "protocol": "postgres",
                "host": "10.0.0.50",
                "port": 5432,
                "database": "erp",
                "username": "reader",
                "password": "original-secret",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created:?}");
        let conn_id = created["id"].as_i64().unwrap();

        let (status, updated) = admin_put(
            &env.router,
            &format!("/api/plc-connections/{conn_id}"),
            &env.admin_token,
            json!({
                "name": "erp-db-clear",
                "protocol": "postgres",
                "host": "10.0.0.50",
                "port": 5432,
                "unitId": 1,
                "enabled": true,
                "simulation": false,
                "wordOrder": "low_high",
                "database": "erp",
                "username": "reader",
                "password": "",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{updated:?}");
        assert_eq!(updated["passwordSet"], false);

        let stored = PlcConnectionService::new(env.pool.clone())
            .get(conn_id)
            .await
            .unwrap();
        assert_eq!(stored.password, None);
    }

    /// 非 postgres 接続に`database`を付けると`422 Validation`で拒否される
    /// (`banto_tags::plc_connection::validate_plc_connection_input`の
    /// postgres-only ルール、REST 経由の確認)。
    #[tokio::test]
    async fn create_non_postgres_with_database_is_rejected() {
        let env = test_env().await;
        let (status, body) = admin_post(
            &env.router,
            "/api/plc-connections",
            &env.admin_token,
            json!({
                "name": "not-pg",
                "protocol": "modbus-tcp",
                "host": "127.0.0.1",
                "port": 15070,
                "database": "somedb",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
        assert_eq!(body["kind"], "validation");
    }

    /// S1（この段階では postgres 接続配下にグループを作れない）: REST 経由の
    /// 確認 - `banto_tags::collection_group`側の単体テストの REST 版。
    #[tokio::test]
    async fn create_group_under_a_postgres_connection_is_rejected() {
        let env = test_env().await;
        let (status, conn) = admin_post(
            &env.router,
            "/api/plc-connections",
            &env.admin_token,
            json!({
                "name": "erp-db-group-guard",
                "protocol": "postgres",
                "host": "10.0.0.50",
                "port": 5432,
                "database": "erp",
                "username": "reader",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{conn:?}");
        let conn_id = conn["id"].as_i64().unwrap();

        let (status, body) = admin_post(
            &env.router,
            "/api/collection-groups",
            &env.admin_token,
            json!({ "name": "g-on-pg", "plcConnectionId": conn_id, "periodMs": 1000 }),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
        assert_eq!(body["kind"], "validation");
    }

    /// `POST /api/plc-connections/{id}/test` - S1a の接続テスト API。
    /// 非 postgres 接続に対しては 400 で拒否する（design §4.6・実装指示
    /// 「for non-postgres protocols respond 400」）。
    #[tokio::test]
    async fn test_saved_connection_rejects_non_postgres_with_400() {
        let env = test_env().await;
        let (status, conn) = admin_post(
            &env.router,
            "/api/plc-connections",
            &env.admin_token,
            json!({ "name": "not-pg-test", "host": "127.0.0.1", "port": 15071 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{conn:?}");
        let conn_id = conn["id"].as_i64().unwrap();

        let (status, body) = admin_post(
            &env.router,
            &format!("/api/plc-connections/{conn_id}/test"),
            &env.admin_token,
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body:?}");
    }

    /// `POST /api/plc-connections/{id}/test` の実 PostgreSQL 経路。
    /// `BANTO_TEST_PG_URL`が未設定なら`eprintln!`してスキップする
    /// （`crate::db_source`の同種テストと同じ慣習）。
    #[tokio::test]
    async fn test_saved_connection_against_a_real_postgresql_returns_the_server_version() {
        let Ok(url) = std::env::var("BANTO_TEST_PG_URL") else {
            eprintln!("skipped: BANTO_TEST_PG_URL unset");
            return;
        };
        let options: sqlx::postgres::PgConnectOptions = url
            .parse()
            .expect("BANTO_TEST_PG_URL should be a valid postgres:// URL");
        let password = url
            .split_once("://")
            .and_then(|(_, rest)| rest.split_once('@'))
            .and_then(|(userinfo, _)| userinfo.split_once(':'))
            .map(|(_, password)| password.to_string());

        let env = test_env().await;
        let (status, conn) = admin_post(
            &env.router,
            "/api/plc-connections",
            &env.admin_token,
            json!({
                "name": "real-pg-test",
                "protocol": "postgres",
                "host": options.get_host(),
                "port": options.get_port(),
                "database": options.get_database(),
                "username": options.get_username(),
                "password": password,
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{conn:?}");
        let conn_id = conn["id"].as_i64().unwrap();

        let (status, body) = admin_post(
            &env.router,
            &format!("/api/plc-connections/{conn_id}/test"),
            &env.admin_token,
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(body["ok"], true, "{body:?}");
        let server_version = body["serverVersion"].as_str().expect("serverVersion");
        eprintln!("POST /api/plc-connections/{{id}}/test -> {server_version}");
        assert!(body.get("error").is_none());
    }

    /// `POST /api/plc-connections/{id}/test`の失敗経路: 閉じたポートに
    /// 対して`ok:false`かつパスワードを含まないエラー文言を返す。
    #[tokio::test]
    async fn test_saved_connection_against_a_closed_port_reports_failure_without_the_password() {
        let env = test_env().await;
        let (status, conn) = admin_post(
            &env.router,
            "/api/plc-connections",
            &env.admin_token,
            json!({
                "name": "closed-port-pg",
                "protocol": "postgres",
                "host": "127.0.0.1",
                "port": 1,
                "database": "erp",
                "username": "reader",
                "password": "s3cret-password",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{conn:?}");
        let conn_id = conn["id"].as_i64().unwrap();

        let (status, body) = admin_post(
            &env.router,
            &format!("/api/plc-connections/{conn_id}/test"),
            &env.admin_token,
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(body["ok"], false, "{body:?}");
        let error = body["error"].as_str().expect("error message");
        assert!(
            !error.contains("s3cret-password"),
            "error text must never contain the plaintext password: {error}"
        );
    }

    /// キューされた削除（収集稼働中に 202 で保留し、停止後に適用）も同じ
    /// カスケード仕様に揃っている - `execute_pending_apply` の
    /// `"plc_connections.delete"` 分岐が `cascade_delete_tx` を呼ぶことの
    /// 回帰テスト（この分岐を旧 `delete_tx` のまま直し忘れると、即時削除
    /// (`plc_connections_delete`) は成功するのにキュー適用だけこの
    /// バリデーションで失敗する、という非対称なバグになる）。
    #[tokio::test]
    async fn pending_apply_plc_connection_delete_cascades_when_it_has_groups_and_tags() {
        let env = test_env().await;

        let (status, conn) = admin_post(
            &env.router,
            "/api/plc-connections",
            &env.admin_token,
            json!({ "name": "line-cascade-pending", "host": "127.0.0.1", "port": 15062 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{conn:?}");
        let conn_id = conn["id"].as_i64().unwrap();

        let (status, group) = admin_post(
            &env.router,
            "/api/collection-groups",
            &env.admin_token,
            json!({ "name": "g-cascade-pending", "plcConnectionId": conn_id, "periodMs": 1000 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{group:?}");
        let group_id = group["id"].as_i64().unwrap();

        let (status, tag) = admin_post(
            &env.router,
            "/api/tags",
            &env.admin_token,
            json!({
                "name": "t-cascade-pending",
                "collectionGroupId": group_id,
                "address": "40001",
                "dataType": "i16"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{tag:?}");
        let tag_id = tag["id"].as_i64().unwrap();

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

        // While running, the delete is only queued (202) - not yet applied.
        let queued = env
            .router
            .clone()
            .oneshot(
                HttpRequest::delete(format!("/api/plc-connections/{conn_id}"))
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
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

        let apply = env
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
        assert_eq!(apply.status(), StatusCode::OK);
        let apply_bytes = axum::body::to_bytes(apply.into_body(), usize::MAX)
            .await
            .unwrap();
        let apply_body: serde_json::Value = serde_json::from_slice(&apply_bytes).unwrap();
        assert_eq!(apply_body["state"], "applied");

        assert!(matches!(
            PlcConnectionService::new(env.pool.clone())
                .get(conn_id)
                .await
                .unwrap_err(),
            BantoError::NotFound { .. }
        ));
        assert!(matches!(
            CollectionGroupService::new(env.pool.clone())
                .get(group_id)
                .await
                .unwrap_err(),
            BantoError::NotFound { .. }
        ));
        assert!(matches!(
            TagService::new(env.pool.clone())
                .get(tag_id)
                .await
                .unwrap_err(),
            BantoError::NotFound { .. }
        ));
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
                    database: None,
                    username: None,
                    password: None,
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
                    database: None,
                    username: None,
                    password: None,
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
                    default_writable: true,
                    query_sql: None,
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

    // --- T19 S2-d (docs/banto-hub-t19-design.md §5.1、UX-39): 履歴の保持
    // 期間 - GET/PUT /api/store-settings と POST /api/store-settings/
    // prune-preview・prune-now -------------------------------------------

    /// `env`が使う実際のtstoreデータディレクトリ（`test_manager_with_clock`
    /// が`dir.path().join("data")`を`CollectorManager`へ渡している - この
    /// テストではファイルを直接置く/確認するために同じパスを再現する）。
    fn store_settings_test_data_dir(env: &TestEnv) -> std::path::PathBuf {
        env._dir.path().join("data")
    }

    fn touch_data_file(
        data_dir: &std::path::Path,
        date: LocalDate,
        seq: u32,
    ) -> std::path::PathBuf {
        std::fs::create_dir_all(data_dir).expect("create data dir");
        let path = data_dir.join(format!("{}-{:03}.sqlite3", date.to_yyyymmdd(), seq));
        std::fs::write(&path, b"").expect("touch data file");
        path
    }

    /// `GET/PUT /api/store-settings`: admin 限定（viewer は403）、既定値
    /// （`Some(7)`）、`null`（無制限）を含めた往復、バリデーション範囲
    /// （1〜3650、実装指示どおり）を確認する。
    #[tokio::test]
    async fn store_settings_round_trips_requires_admin_and_validates_range() {
        let env = test_env().await;

        let get = |token: String| {
            let router = env.router.clone();
            async move {
                let request = HttpRequest::builder()
                    .method("GET")
                    .uri("/api/store-settings")
                    .header("Authorization", format!("Bearer {token}"))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap();
                router.oneshot(request).await.unwrap()
            }
        };

        let response = get(env.viewer_token.clone()).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let response = get(env.admin_token.clone()).await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["retentionDays"], 7);

        let put = |token: String, value: serde_json::Value| {
            let router = env.router.clone();
            async move {
                let request = HttpRequest::builder()
                    .method("PUT")
                    .uri("/api/store-settings")
                    .header("Authorization", format!("Bearer {token}"))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({ "retentionDays": value })).unwrap(),
                    ))
                    .unwrap();
                router.oneshot(request).await.unwrap()
            }
        };

        // viewer は書けない。
        let response = put(env.viewer_token.clone(), serde_json::json!(30)).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        // バリデーション: 0以下・上限超過は422。
        for bad in [
            serde_json::json!(0),
            serde_json::json!(-5),
            serde_json::json!(3651),
        ] {
            let response = put(env.admin_token.clone(), bad.clone()).await;
            assert_eq!(
                response.status(),
                StatusCode::UNPROCESSABLE_ENTITY,
                "retentionDays={bad:?} should be rejected"
            );
        }

        // 正常な保存 → 再取得で往復する。
        let response = put(env.admin_token.clone(), serde_json::json!(30)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let applied: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(applied["retentionDays"], 30);

        let response = get(env.admin_token.clone()).await;
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let refetched: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(refetched["retentionDays"], 30);

        // UX-39: 無制限（null）も保存・読み戻しできる。
        let response = put(env.admin_token.clone(), serde_json::Value::Null).await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let applied: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(applied["retentionDays"], serde_json::Value::Null);

        let response = get(env.admin_token.clone()).await;
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let refetched: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(refetched["retentionDays"], serde_json::Value::Null);

        // `resource = 'store_settings'`は成功した更新（action='update'）に
        // 加えて、viewer の403（`require_role_at_least`が同じ`resource`名で
        // 記録する`action='denied'`エントリ）も含む - ここでは「保存が
        // 何回成功したか」だけを見るため`action='update'`に絞る。
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT action, resource, result FROM audit_log WHERE resource = 'store_settings' AND action = 'update' ORDER BY id",
        )
        .fetch_all(&env.pool)
        .await
        .unwrap();
        // 正常な PUT（30 → null）の2件だけが記録される（バリデーション
        // エラーになった3件・viewer の403は記録されない）。
        assert_eq!(rows.len(), 2);
        for row in rows {
            assert_eq!(
                row,
                (
                    "update".to_string(),
                    "store_settings".to_string(),
                    "ok".to_string()
                )
            );
        }
    }

    /// オーナー決定1の核心: **`PUT /api/store-settings` は保存するだけで、
    /// 履歴ファイルを一切削除しない**。保持日数を1日に絞っても、既存の
    /// 古いデータファイルはディスク上に残ることを確認する。
    #[tokio::test]
    async fn store_settings_put_never_deletes_history_files() {
        let today = LocalDate::new(2026, 7, 12);
        let now_ms = today.to_days_since_epoch() * 86_400_000 + 12 * 3_600_000;
        let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(now_ms, 0));
        let env = test_env_with_clock(clock).await;

        let data_dir = store_settings_test_data_dir(&env);
        let old_file = touch_data_file(&data_dir, LocalDate::new(2026, 6, 1), 1);
        let today_file = touch_data_file(&data_dir, today, 1);

        // 保持日数を1日という厳しい値に変更しても、PUT 自体は削除しない。
        let put = HttpRequest::builder()
            .method("PUT")
            .uri("/api/store-settings")
            .header("Authorization", format!("Bearer {}", env.admin_token))
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({ "retentionDays": 1 })).unwrap(),
            ))
            .unwrap();
        let response = env.router.clone().oneshot(put).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        assert!(old_file.exists(), "PUT must not prune the old file");
        assert!(today_file.exists());
    }

    /// `POST /api/store-settings/prune-preview`・`prune-now`が**保存済みの**
    /// 保持方針で動くこと、preview は削除しないこと、prune-now は実際に
    /// 削除して監査ログに残すことを確認する（admin 限定も併せて確認）。
    #[tokio::test]
    async fn store_settings_prune_preview_and_prune_now_use_saved_policy() {
        let today = LocalDate::new(2026, 7, 12);
        let now_ms = today.to_days_since_epoch() * 86_400_000 + 12 * 3_600_000;
        let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(now_ms, 0));
        let env = test_env_with_clock(clock).await;

        let data_dir = store_settings_test_data_dir(&env);
        // 7日保持のとき、30日以上前のファイルは削除対象、当日ファイルは
        // 対象外 - `banto-tstore`の`prune_files`単体テストと同じ境界判定。
        let old_file = touch_data_file(&data_dir, LocalDate::new(2026, 6, 1), 1);
        let today_file = touch_data_file(&data_dir, today, 1);

        let put = HttpRequest::builder()
            .method("PUT")
            .uri("/api/store-settings")
            .header("Authorization", format!("Bearer {}", env.admin_token))
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({ "retentionDays": 7 })).unwrap(),
            ))
            .unwrap();
        let response = env.router.clone().oneshot(put).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // viewer は preview/prune-now どちらも403。
        for path in [
            "/api/store-settings/prune-preview",
            "/api/store-settings/prune-now",
        ] {
            let request = HttpRequest::builder()
                .method("POST")
                .uri(path)
                .header("Authorization", format!("Bearer {}", env.viewer_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap();
            let response = env.router.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "path={path}");
        }

        // preview は件数を返すだけで削除しない。
        let preview = HttpRequest::builder()
            .method("POST")
            .uri("/api/store-settings/prune-preview")
            .header("Authorization", format!("Bearer {}", env.admin_token))
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .body(Body::empty())
            .unwrap();
        let response = env.router.clone().oneshot(preview).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["wouldDeleteCount"], 1);
        assert!(old_file.exists(), "preview must not delete anything");

        // prune-now は実際に削除し、削除数を返し、監査ログに残す。
        let prune_now = HttpRequest::builder()
            .method("POST")
            .uri("/api/store-settings/prune-now")
            .header("Authorization", format!("Bearer {}", env.admin_token))
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .body(Body::empty())
            .unwrap();
        let response = env.router.clone().oneshot(prune_now).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["deletedCount"], 1);
        assert!(
            !old_file.exists(),
            "prune-now must delete the aged-out file"
        );
        assert!(today_file.exists(), "today's file must survive");

        let rows: Vec<(String, String, String, Option<String>)> = sqlx::query_as(
            "SELECT action, resource, result, detail FROM audit_log WHERE resource = 'store_settings_prune' ORDER BY id",
        )
        .fetch_all(&env.pool)
        .await
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "delete");
        assert_eq!(rows[0].1, "store_settings_prune");
        assert_eq!(rows[0].2, "ok");
        let detail: serde_json::Value =
            serde_json::from_str(rows[0].3.as_deref().unwrap()).unwrap();
        assert_eq!(detail["deletedCount"], 1);
    }

    /// UX-39（無制限の選択肢）: `retentionDays: null`のときは
    /// preview/prune-now とも何も削除しない。
    #[tokio::test]
    async fn store_settings_prune_with_unlimited_policy_deletes_nothing() {
        let today = LocalDate::new(2026, 7, 12);
        let now_ms = today.to_days_since_epoch() * 86_400_000 + 12 * 3_600_000;
        let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(now_ms, 0));
        let env = test_env_with_clock(clock).await;

        let data_dir = store_settings_test_data_dir(&env);
        let old_file = touch_data_file(&data_dir, LocalDate::new(2020, 1, 1), 1);

        let put = HttpRequest::builder()
            .method("PUT")
            .uri("/api/store-settings")
            .header("Authorization", format!("Bearer {}", env.admin_token))
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({ "retentionDays": null })).unwrap(),
            ))
            .unwrap();
        let response = env.router.clone().oneshot(put).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let preview = HttpRequest::builder()
            .method("POST")
            .uri("/api/store-settings/prune-preview")
            .header("Authorization", format!("Bearer {}", env.admin_token))
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .body(Body::empty())
            .unwrap();
        let response = env.router.clone().oneshot(preview).await.unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["wouldDeleteCount"], 0);

        let prune_now = HttpRequest::builder()
            .method("POST")
            .uri("/api/store-settings/prune-now")
            .header("Authorization", format!("Bearer {}", env.admin_token))
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .body(Body::empty())
            .unwrap();
        let response = env.router.clone().oneshot(prune_now).await.unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["deletedCount"], 0);
        assert!(old_file.exists(), "unlimited retention must delete nothing");
    }

    /// レビュー指摘の安全化: 保持日数が`u32::MAX`を超える値で保存されて
    /// いる状態（`parse_retention`は上限クランプをしないため、保存経路を
    /// 通らず直接設定された等でこの状態になり得る）で prune-preview /
    /// prune-now を叩くと、どちらも削除0件になり（既存ファイルが消えず）、
    /// 両者の判定が一致すること（`resolve_prune_retention_days`を両方の
    /// ハンドラが共有していることの固定）を確認する。
    #[tokio::test]
    async fn store_settings_prune_with_out_of_range_retention_days_deletes_nothing() {
        let today = LocalDate::new(2026, 7, 12);
        let now_ms = today.to_days_since_epoch() * 86_400_000 + 12 * 3_600_000;
        let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(now_ms, 0));
        let env = test_env_with_clock(clock).await;

        let data_dir = store_settings_test_data_dir(&env);
        let old_file = touch_data_file(&data_dir, LocalDate::new(2020, 1, 1), 1);

        // `PUT /api/store-settings`のバリデーション（1〜3650）を経由せず、
        // 保存経路そのものへ想定外の巨大値を直接書き込む - レビュー指摘が
        // 想定する「設定が壊れている・DBを直接編集された」状況の再現。
        SettingsService::new(env.pool.clone())
            .set_store_config(&StoreSettings {
                data_dir: "./data".to_string(),
                retention_days: Some(u32::MAX as i64 + 1),
            })
            .await
            .expect("set_store_config");

        let preview = HttpRequest::builder()
            .method("POST")
            .uri("/api/store-settings/prune-preview")
            .header("Authorization", format!("Bearer {}", env.admin_token))
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .body(Body::empty())
            .unwrap();
        let response = env.router.clone().oneshot(preview).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["wouldDeleteCount"], 0);
        assert!(old_file.exists(), "preview must not delete anything");

        let prune_now = HttpRequest::builder()
            .method("POST")
            .uri("/api/store-settings/prune-now")
            .header("Authorization", format!("Bearer {}", env.admin_token))
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .body(Body::empty())
            .unwrap();
        let response = env.router.clone().oneshot(prune_now).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["deletedCount"], 0);
        assert!(
            old_file.exists(),
            "out-of-range retention_days must delete nothing, matching the preview"
        );
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

    // --- T19 S1-b0: GET /api/tags/address-writable (PR #232 レビュー指摘) --
    //
    // `tags_address_writable`/`AddressWritableResponse`の doc comment が
    // 定義する3値の意味論をここで固定する:
    //   - `writable: Some(true)`, `area: None`  … 領域制限が「存在しない」
    //     （`slmp`/`virtual`）
    //   - `writable: Some(bool)`, `area: Some(_)` … `modbus-tcp`でパース成功
    //   - `writable: None`, `area: None`         … 判定不能
    // 当初案はこれを1つの`null`に統合していたため、"SLMP の既定が永久に
    // 効かない"（1と5の混同）と"入力途中の Modbus アドレスで誤って既定
    // ON"（3/4と5の混同）の両方の実害を招くところだった - 以下のテストは
    // その3値それぞれを区別する。

    /// `GET /api/tags/address-writable`をクエリ文字列で叩くヘルパー。
    async fn address_writable(
        router: &Router,
        token: &str,
        protocol: &str,
        address: &str,
    ) -> (StatusCode, serde_json::Value) {
        // テストで使う値はすべて英数字・ハイフン・ドットのみ（`modbus-tcp`・
        // `40001.5`・空文字・`abc`等）で、クエリ文字列としてパーセント
        // エンコード無しでも安全にそのまま埋め込める - 依存を増やしてまで
        // エンコーダを導入する必要はない。
        let uri = format!("/api/tags/address-writable?protocol={protocol}&address={address}");
        let response = router
            .clone()
            .oneshot(
                HttpRequest::get(uri)
                    .header("Authorization", format!("Bearer {token}"))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
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

    /// (1) `slmp`は任意のアドレスで `writable: true, area: null`
    /// （領域による制限が存在しない）。
    #[tokio::test]
    async fn slmp_protocol_has_no_area_restriction_regardless_of_address() {
        let env = test_env().await;
        let (status, body) = address_writable(&env.router, &env.admin_token, "slmp", "D100").await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(body["writable"], serde_json::json!(true), "{body:?}");
        assert!(body["area"].is_null(), "{body:?}");
    }

    /// (2) `virtual`も同様に `writable: true, area: null`。
    #[tokio::test]
    async fn virtual_protocol_has_no_area_restriction_regardless_of_address() {
        let env = test_env().await;
        let (status, body) = address_writable(&env.router, &env.admin_token, "virtual", "").await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(body["writable"], serde_json::json!(true), "{body:?}");
        assert!(body["area"].is_null(), "{body:?}");
    }

    /// (3) `modbus-tcp` + 書き込み可能領域（holding register `40001`・coil
    /// `00001`）は `writable: true` かつ `area`が領域名 - **(5)の
    /// `writable: null`（判定不能）と区別できること**が本質。
    #[tokio::test]
    async fn modbus_writable_areas_report_writable_true_with_area_name() {
        let env = test_env().await;

        let (status, body) =
            address_writable(&env.router, &env.admin_token, "modbus-tcp", "40001").await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(body["writable"], serde_json::json!(true), "{body:?}");
        assert_eq!(
            body["area"],
            serde_json::json!("holding_register"),
            "{body:?}"
        );

        let (status, body) =
            address_writable(&env.router, &env.admin_token, "modbus-tcp", "00001").await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(body["writable"], serde_json::json!(true), "{body:?}");
        assert_eq!(body["area"], serde_json::json!("coil"), "{body:?}");
    }

    /// (4) `modbus-tcp` + 読み取り専用領域（discrete input `10001`・input
    /// register `30001`）は `writable: false` かつ `area`が領域名 - **(5)の
    /// `writable: null`（判定不能）と絶対に混同してはならない**（`false`と
    /// `null`の区別がこのエンドポイントの存在意義そのもの）。
    #[tokio::test]
    async fn modbus_read_only_areas_report_writable_false_not_null_with_area_name() {
        let env = test_env().await;

        let (status, body) =
            address_writable(&env.router, &env.admin_token, "modbus-tcp", "10001").await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(body["writable"], serde_json::json!(false), "{body:?}");
        assert_eq!(
            body["area"],
            serde_json::json!("discrete_input"),
            "{body:?}"
        );

        let (status, body) =
            address_writable(&env.router, &env.admin_token, "modbus-tcp", "30001").await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(body["writable"], serde_json::json!(false), "{body:?}");
        assert_eq!(
            body["area"],
            serde_json::json!("input_register"),
            "{body:?}"
        );
    }

    /// (5) `modbus-tcp` + パース不能（空文字・`abc`・桁数不正）は
    /// `writable: null, area: null`（判定不能）- **(1)の`writable: true`
    /// とも(4)の`writable: false`とも異なる第三の値**であることを固定する。
    /// 入力途中のアドレス文字列でこれを`false`扱いすると誤って書き込み
    /// 拒否UIになり、`true`扱いすると読み取り専用領域へ誤って既定ONに
    /// なるところだった（レビュー指摘の核心）。
    #[tokio::test]
    async fn modbus_unparseable_address_reports_null_writable_and_null_area() {
        let env = test_env().await;

        for address in ["", "abc", "4001", "400001234567"] {
            let (status, body) =
                address_writable(&env.router, &env.admin_token, "modbus-tcp", address).await;
            assert_eq!(status, StatusCode::OK, "address={address:?} body={body:?}");
            assert!(
                body["writable"].is_null(),
                "address={address:?} body={body:?}"
            );
            assert!(body["area"].is_null(), "address={address:?} body={body:?}");
        }
    }

    /// (6) 未知の protocol は判定不能 - `writable: null, area: null`。
    /// SLMP/virtual/modbus-tcp のいずれでもない値は将来プロトコルの
    /// 追加候補であり、安全側（既定を適用しない）へ倒れる。
    #[tokio::test]
    async fn unknown_protocol_reports_null_writable_and_null_area() {
        let env = test_env().await;
        let (status, body) = address_writable(
            &env.router,
            &env.admin_token,
            "some-future-protocol",
            "40001",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert!(body["writable"].is_null(), "{body:?}");
        assert!(body["area"].is_null(), "{body:?}");
    }

    /// 認可: `require_editor`ではなく`require_auth`のみを要求する意図的な
    /// 設計（読み取り専用・副作用なし）なので、viewer でも 200 で呼べる
    /// ことを固定する。
    #[tokio::test]
    async fn viewer_role_can_call_address_writable() {
        let env = test_env().await;
        let (status, body) =
            address_writable(&env.router, &env.viewer_token, "modbus-tcp", "40001").await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(body["writable"], serde_json::json!(true), "{body:?}");
        assert_eq!(
            body["area"],
            serde_json::json!("holding_register"),
            "{body:?}"
        );
    }

    /// bit接尾辞（T8 §6.1、`banto_plc::address::Address::parse`）: レジスタ
    /// 領域（holding/input register）では`.N`が受理され、`area`は bit を
    /// 無視してレジスタ自体の領域を返す - `AddressWritableResponse`は bit
    /// 位置を運ばない（doc comment参照）ので、`.5`の有無で判定結果が
    /// 変わらないことを固定する。
    #[tokio::test]
    async fn modbus_bit_suffix_on_register_area_is_accepted_and_reports_register_area() {
        let env = test_env().await;

        let (status, body) =
            address_writable(&env.router, &env.admin_token, "modbus-tcp", "30001.5").await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(body["writable"], serde_json::json!(false), "{body:?}");
        assert_eq!(
            body["area"],
            serde_json::json!("input_register"),
            "{body:?}"
        );

        let (status, body) =
            address_writable(&env.router, &env.admin_token, "modbus-tcp", "40001.3").await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(body["writable"], serde_json::json!(true), "{body:?}");
        assert_eq!(
            body["area"],
            serde_json::json!("holding_register"),
            "{body:?}"
        );
    }

    /// bit接尾辞をコイル/discrete input（既にビット粒度）へ付けるのは
    /// `Address::parse`が拒否する（"すでにビット粒度の領域への冗長な`.N`は
    /// 無音で受理せず、未知エリアと同様に拒否する"という`parse`自身の doc
    /// comment）ので、このエンドポイントでは判定不能（`null`/`null`）に
    /// フォールバックすることを固定する。
    #[tokio::test]
    async fn modbus_bit_suffix_on_coil_area_is_rejected_as_unparseable() {
        let env = test_env().await;
        let (status, body) =
            address_writable(&env.router, &env.admin_token, "modbus-tcp", "00001.0").await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert!(body["writable"].is_null(), "{body:?}");
        assert!(body["area"].is_null(), "{body:?}");
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

    // --- 外部 DB 連携 S4（docs/banto-hub-external-db-design.md §5.2・§6-13・
    // §6-15）: DB Sink の Hub 側（`hub_sink_groups` CRUD、`GET
    // /api/sink/config`、`PUT /api/sink/status`、`GET /api/status`の
    // `sink`節） ------------------------------------------------------------

    /// PLC 接続1つ+収集グループ+タグ1つ、および postgres 接続1つを作る -
    /// sink group のテストが共通に必要とするフィクスチャ
    /// （`seed_scope_fixture`と同じ「admin REST 経由で組み立てる」方針）。
    /// 戻り値は `(postgres 接続の id, タグの id)`。
    async fn seed_sink_fixture(router: &Router, admin_token: &str) -> (i64, i64) {
        let (status, conn) = admin_post(
            router,
            "/api/plc-connections",
            admin_token,
            json!({ "name": "line1", "host": "127.0.0.1", "port": 15201 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{conn:?}");
        let (status, group) = admin_post(
            router,
            "/api/collection-groups",
            admin_token,
            json!({ "name": "fast", "plcConnectionId": conn["id"], "periodMs": 100 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{group:?}");
        let (status, tag) = admin_post(
            router,
            "/api/tags",
            admin_token,
            json!({
                "name": "temp01",
                "collectionGroupId": group["id"],
                "address": "40001",
                "dataType": "i16",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{tag:?}");

        let (status, pg) = admin_post(
            router,
            "/api/plc-connections",
            admin_token,
            json!({
                "name": "erp-db",
                "protocol": "postgres",
                "host": "10.0.0.50",
                "port": 5432,
                "database": "erp",
                "username": "reader",
                "password": "s3cret",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{pg:?}");

        (pg["id"].as_i64().unwrap(), tag["id"].as_i64().unwrap())
    }

    fn sink_group_body(db_connection_id: i64, tag_id: i64) -> serde_json::Value {
        json!({
            "name": "line1-log",
            "dbConnectionId": db_connection_id,
            "mode": "interval",
            "intervalMs": 1000,
            "tableName": "public.tag_history",
            "storeBad": false,
            "enabled": true,
            "tagIds": [tag_id],
        })
    }

    #[tokio::test]
    async fn sink_groups_crud_happy_path() {
        let env = test_env().await;
        let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;

        let (status, created) = admin_post(
            &env.router,
            "/api/sink/groups",
            &env.admin_token,
            sink_group_body(db_connection_id, tag_id),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created:?}");
        assert_eq!(created["name"], "line1-log");
        assert_eq!(created["dbConnectionId"], db_connection_id);
        assert_eq!(created["tagIds"], json!([tag_id]));
        let id = created["id"].as_i64().unwrap();

        let (status, listed) = admin_get(&env.router, "/api/sink/groups", &env.admin_token).await;
        assert_eq!(status, StatusCode::OK, "{listed:?}");
        assert_eq!(listed.as_array().unwrap().len(), 1);

        let (status, fetched) = admin_get(
            &env.router,
            &format!("/api/sink/groups/{id}"),
            &env.admin_token,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{fetched:?}");
        assert_eq!(fetched, created);

        let mut updated_body = sink_group_body(db_connection_id, tag_id);
        updated_body["intervalMs"] = json!(2000);
        updated_body["enabled"] = json!(false);
        let (status, updated) = admin_put(
            &env.router,
            &format!("/api/sink/groups/{id}"),
            &env.admin_token,
            updated_body,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{updated:?}");
        assert_eq!(updated["intervalMs"], 2000);
        assert_eq!(updated["enabled"], false);

        let (status, _) = admin_delete(
            &env.router,
            &format!("/api/sink/groups/{id}"),
            &env.admin_token,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, _) = admin_get(
            &env.router,
            &format!("/api/sink/groups/{id}"),
            &env.admin_token,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn sink_groups_create_requires_editor() {
        let env = test_env().await;
        let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
        let (status, body) = admin_post(
            &env.router,
            "/api/sink/groups",
            &env.viewer_token,
            sink_group_body(db_connection_id, tag_id),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body:?}");
    }

    #[tokio::test]
    async fn sink_groups_create_rejects_empty_name() {
        let env = test_env().await;
        let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
        let mut body = sink_group_body(db_connection_id, tag_id);
        body["name"] = json!("   ");
        let (status, body) =
            admin_post(&env.router, "/api/sink/groups", &env.admin_token, body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    }

    #[tokio::test]
    async fn sink_groups_create_rejects_too_long_name() {
        let env = test_env().await;
        let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
        let mut body = sink_group_body(db_connection_id, tag_id);
        body["name"] = json!("x".repeat(65));
        let (status, body) =
            admin_post(&env.router, "/api/sink/groups", &env.admin_token, body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    }

    #[tokio::test]
    async fn sink_groups_create_rejects_duplicate_name() {
        let env = test_env().await;
        let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
        let (status, _) = admin_post(
            &env.router,
            "/api/sink/groups",
            &env.admin_token,
            sink_group_body(db_connection_id, tag_id),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, body) = admin_post(
            &env.router,
            "/api/sink/groups",
            &env.admin_token,
            sink_group_body(db_connection_id, tag_id),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    }

    #[tokio::test]
    async fn sink_groups_create_rejects_bad_mode() {
        let env = test_env().await;
        let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
        let mut body = sink_group_body(db_connection_id, tag_id);
        body["mode"] = json!("hourly");
        let (status, body) =
            admin_post(&env.router, "/api/sink/groups", &env.admin_token, body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    }

    #[tokio::test]
    async fn sink_groups_create_rejects_out_of_range_interval_ms() {
        let env = test_env().await;
        let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
        let mut body = sink_group_body(db_connection_id, tag_id);
        body["intervalMs"] = json!(10);
        let (status, body) =
            admin_post(&env.router, "/api/sink/groups", &env.admin_token, body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    }

    #[tokio::test]
    async fn sink_groups_create_rejects_invalid_table_name() {
        let env = test_env().await;
        let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
        let mut body = sink_group_body(db_connection_id, tag_id);
        body["tableName"] = json!("\"quoted\"");
        let (status, body) =
            admin_post(&env.router, "/api/sink/groups", &env.admin_token, body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    }

    #[tokio::test]
    async fn sink_groups_create_rejects_empty_tag_ids() {
        let env = test_env().await;
        let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
        let _ = tag_id;
        let mut body = sink_group_body(db_connection_id, tag_id);
        body["tagIds"] = json!([]);
        let (status, body) =
            admin_post(&env.router, "/api/sink/groups", &env.admin_token, body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    }

    #[tokio::test]
    async fn sink_groups_create_rejects_duplicate_tag_ids() {
        let env = test_env().await;
        let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
        let mut body = sink_group_body(db_connection_id, tag_id);
        body["tagIds"] = json!([tag_id, tag_id]);
        let (status, body) =
            admin_post(&env.router, "/api/sink/groups", &env.admin_token, body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    }

    #[tokio::test]
    async fn sink_groups_create_rejects_unknown_tag_id() {
        let env = test_env().await;
        let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
        let mut body = sink_group_body(db_connection_id, tag_id);
        body["tagIds"] = json!([999_999]);
        let (status, body) =
            admin_post(&env.router, "/api/sink/groups", &env.admin_token, body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    }

    /// 接続が `protocol: postgres` でなければ422（実装指示: 「connection
    /// that is not postgres → 422」）。
    #[tokio::test]
    async fn sink_groups_create_rejects_non_postgres_connection() {
        let env = test_env().await;
        let (_db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
        let (status, plc) = admin_get(&env.router, "/api/plc-connections", &env.admin_token).await;
        assert_eq!(status, StatusCode::OK, "{plc:?}");
        let modbus_connection_id = plc
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["protocol"] == "modbus-tcp")
            .unwrap()["id"]
            .as_i64()
            .unwrap();

        let mut body = sink_group_body(modbus_connection_id, tag_id);
        body["dbConnectionId"] = json!(modbus_connection_id);
        let (status, body) =
            admin_post(&env.router, "/api/sink/groups", &env.admin_token, body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    }

    #[tokio::test]
    async fn sink_groups_create_rejects_unknown_connection() {
        let env = test_env().await;
        let (_db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
        let body = sink_group_body(999_999, tag_id);
        let (status, body) =
            admin_post(&env.router, "/api/sink/groups", &env.admin_token, body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    }

    /// この接続を参照する sink group がある間は `plc_connections` の削除を
    /// 拒否する（RESTRICT 相当、実装指示: 「connection that a sink group
    /// references is rejected with a clear message」）。
    #[tokio::test]
    async fn deleting_a_connection_referenced_by_a_sink_group_is_rejected() {
        let env = test_env().await;
        let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
        let (status, _) = admin_post(
            &env.router,
            "/api/sink/groups",
            &env.admin_token,
            sink_group_body(db_connection_id, tag_id),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, body) = admin_delete(
            &env.router,
            &format!("/api/plc-connections/{db_connection_id}"),
            &env.admin_token,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
        let message = body["field_errors"][0]["message"].as_str().unwrap_or("");
        assert!(
            message.contains("sink group"),
            "expected the message to mention sink group: {body:?}"
        );

        // 実際にまだ存在すること。
        PlcConnectionService::new(env.pool.clone())
            .get(db_connection_id)
            .await
            .expect("connection should survive");
    }

    /// 対象タグを削除すると、`hub_sink_group_tags`の対応する行も消える
    /// （`crate::sink`のモジュール doc comment「タグ削除の経路では能動的に
    /// 消す」）。
    #[tokio::test]
    async fn deleting_a_tag_removes_it_from_sink_group_membership() {
        let env = test_env().await;
        let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
        let (status, created) = admin_post(
            &env.router,
            "/api/sink/groups",
            &env.admin_token,
            sink_group_body(db_connection_id, tag_id),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created:?}");
        let group_id = created["id"].as_i64().unwrap();

        let (status, _) = admin_delete(
            &env.router,
            &format!("/api/tags/{tag_id}"),
            &env.admin_token,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, fetched) = admin_get(
            &env.router,
            &format!("/api/sink/groups/{group_id}"),
            &env.admin_token,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{fetched:?}");
        assert_eq!(fetched["tagIds"], json!([]));
    }

    /// `GET /api/sink/config` は admin 以外 403、admin なら200でパスワード
    /// を含み、タグは stable id（connectionId/groupId/tagId）を運ぶ
    /// （実装指示）。
    #[tokio::test]
    async fn sink_config_get_requires_admin_and_carries_password_and_stable_ids() {
        let env = test_env().await;
        let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
        let (status, _) = admin_post(
            &env.router,
            "/api/sink/groups",
            &env.admin_token,
            sink_group_body(db_connection_id, tag_id),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, body) = v1_get(&env.router, &env.viewer_token, "/api/sink/config").await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body:?}");

        let (status, body) = v1_get(&env.router, &env.admin_token, "/api/sink/config").await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert!(body["generatedAt"].as_i64().is_some());
        let groups = body["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        let tags = groups[0]["tags"].as_array().unwrap();
        assert_eq!(tags.len(), 1);
        assert!(tags[0]["connectionId"].as_i64().is_some());
        assert!(tags[0]["groupId"].as_i64().is_some());
        assert_eq!(tags[0]["tagId"], tag_id);
        assert!(tags[0]["externalName"].as_str().unwrap().contains("temp01"));

        let connections = body["connections"].as_array().unwrap();
        assert_eq!(connections.len(), 1);
        assert_eq!(connections[0]["id"], db_connection_id);
        assert_eq!(connections[0]["password"], "s3cret");

        // admin スコープを持つ API キーでも通ること(サイドカーの本来の
        // 経路)。admin スコープ無しのキーは403。
        let (status, issued) =
            issue_api_key(&env.router, &env.admin_token, "sidecar", &["admin"]).await;
        assert_eq!(status, StatusCode::CREATED, "{issued:?}");
        let admin_key = issued["key"].as_str().unwrap();
        let (status, body) = v1_get(&env.router, admin_key, "/api/sink/config").await;
        assert_eq!(status, StatusCode::OK, "{body:?}");

        let (status, issued) =
            issue_api_key(&env.router, &env.admin_token, "reader-only", &["read"]).await;
        assert_eq!(status, StatusCode::CREATED, "{issued:?}");
        let read_key = issued["key"].as_str().unwrap();
        let (status, body) = v1_get(&env.router, read_key, "/api/sink/config").await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body:?}");
    }

    /// 無効な sink group のみの構成では`groups`/`connections`とも空配列。
    #[tokio::test]
    async fn sink_config_get_excludes_disabled_groups() {
        let env = test_env().await;
        let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
        let mut body = sink_group_body(db_connection_id, tag_id);
        body["enabled"] = json!(false);
        let (status, _) = admin_post(&env.router, "/api/sink/groups", &env.admin_token, body).await;
        assert_eq!(status, StatusCode::OK);

        let (status, body) = v1_get(&env.router, &env.admin_token, "/api/sink/config").await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(body["groups"].as_array().unwrap().len(), 0);
        assert_eq!(body["connections"].as_array().unwrap().len(), 0);
    }

    fn sink_status_push_body(group_id: i64) -> serde_json::Value {
        json!({
            "groups": [{
                "id": group_id,
                "state": "running",
                "queued": 3,
                "dropped": 0,
                "lastFlushAt": 1_000,
                "lastError": null,
            }],
        })
    }

    #[tokio::test]
    async fn sink_status_put_requires_admin() {
        let env = test_env().await;
        let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
        let (status, created) = admin_post(
            &env.router,
            "/api/sink/groups",
            &env.admin_token,
            sink_group_body(db_connection_id, tag_id),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let group_id = created["id"].as_i64().unwrap();

        let (status, body) = v1_put(
            &env.router,
            &env.viewer_token,
            "/api/sink/status",
            sink_status_push_body(group_id),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body:?}");
    }

    #[tokio::test]
    async fn sink_status_put_rejects_unknown_group_id() {
        let env = test_env().await;
        let (status, body) = v1_put(
            &env.router,
            &env.admin_token,
            "/api/sink/status",
            sink_status_push_body(999_999),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
        let message = body["field_errors"][0]["message"].as_str().unwrap_or("");
        assert!(message.contains("999999"), "{body:?}");
    }

    /// push 後は `GET /api/status` の `sink.sidecar.state == "online"` と
    /// 対応するグループ行が見え、15秒以上経過すると`"unknown"`に戻る
    /// （実装指示 - `ManualClock`で決定的に検証する）。
    #[tokio::test]
    async fn sink_status_push_then_status_shows_online_then_unknown_after_stale_window() {
        let now_ms = 1_700_000_000_000i64;
        let clock = Arc::new(ManualClock::new(now_ms, 0));
        let env = test_env_with_clock(clock.clone()).await;
        let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
        let (status, created) = admin_post(
            &env.router,
            "/api/sink/groups",
            &env.admin_token,
            sink_group_body(db_connection_id, tag_id),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let group_id = created["id"].as_i64().unwrap();

        let (status, _) = v1_put(
            &env.router,
            &env.admin_token,
            "/api/sink/status",
            sink_status_push_body(group_id),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, admin_status_body) =
            admin_get(&env.router, "/api/status", &env.admin_token).await;
        assert_eq!(status, StatusCode::OK, "{admin_status_body:?}");
        assert_eq!(admin_status_body["sink"]["sidecar"]["state"], "online");
        let groups = admin_status_body["sink"]["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0]["id"], group_id);
        assert_eq!(groups[0]["queued"], 3);

        clock.advance_ms(crate::sink::SINK_SIDECAR_STALE_AFTER_MS);
        let (status, admin_status_body) =
            admin_get(&env.router, "/api/status", &env.admin_token).await;
        assert_eq!(status, StatusCode::OK, "{admin_status_body:?}");
        assert_eq!(admin_status_body["sink"]["sidecar"]["state"], "unknown");
    }

    // --- banto v1.7.0 #204: session revocation -------------------------------

    fn session_request(
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<serde_json::Value>,
    ) -> HttpRequest<Body> {
        let mut builder = HttpRequest::builder()
            .method(method)
            .uri(path)
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1);
        if let Some(token) = token {
            builder = builder.header("Authorization", format!("Bearer {token}"));
        }
        match body {
            Some(body) => builder
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        }
    }

    async fn session_call(
        router: &Router,
        request: HttpRequest<Body>,
    ) -> (StatusCode, serde_json::Value) {
        let response = router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    async fn session_login(router: &Router, username: &str, remember: bool) -> String {
        let (status, body) = session_call(
            router,
            session_request(
                "POST",
                "/api/auth/login",
                None,
                Some(json!({ "username": username, "password": "password123", "remember": remember })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["token"]
            .as_str()
            .unwrap_or_else(|| panic!("login for {username} returned no token: {body}"))
            .to_string()
    }

    /// One session per surface: the admin UI (`require_auth_or_commissioning`)
    /// and the tag space (`require_tag_space_auth`'s session branch). Separate
    /// tokens, because the first surface to notice a revocation drops the
    /// token from memory - checking one token on both would let the second
    /// surface pass without re-checking anything.
    struct SurfaceTokens {
        ui: String,
        v1: String,
    }

    async fn surface_login(router: &Router, username: &str, remember: bool) -> SurfaceTokens {
        SurfaceTokens {
            ui: session_login(router, username, remember).await,
            v1: session_login(router, username, remember).await,
        }
    }

    /// `(admin UI, tag space)` statuses - both must refuse a revoked session.
    async fn session_statuses(router: &Router, tokens: &SurfaceTokens) -> (StatusCode, StatusCode) {
        let admin_ui = session_call(
            router,
            session_request("GET", "/api/tags", Some(&tokens.ui), None),
        )
        .await
        .0;
        let tag_space = session_call(
            router,
            session_request("GET", "/api/v1/tags", Some(&tokens.v1), None),
        )
        .await
        .0;
        (admin_ui, tag_space)
    }

    #[derive(Debug, Clone, Copy)]
    enum AccountChange {
        Delete,
        Demote,
        PasswordChange,
        PasswordReset,
    }

    /// banto v1.7.0 #204 completion table (banto-hub has no Tauri command
    /// surface for accounts - REST is its only transport): deleting,
    /// demoting, resetting or changing the password of an account ends its
    /// sessions held elsewhere - normal and "Remember me" - on both the admin
    /// UI routes and the tag space, while other accounts' sessions are
    /// untouched and the session that changed its OWN password keeps
    /// working.
    #[tokio::test]
    async fn account_changes_end_the_accounts_other_sessions() {
        let ok = (StatusCode::OK, StatusCode::OK);
        let revoked = (StatusCode::UNAUTHORIZED, StatusCode::UNAUTHORIZED);
        for change in [
            AccountChange::Delete,
            AccountChange::Demote,
            AccountChange::PasswordChange,
            AccountChange::PasswordReset,
        ] {
            let env = test_env().await;
            let router = &env.router;
            let admin = &env.admin_token;
            let (status, alice) = session_call(
                router,
                session_request(
                    "POST",
                    "/api/users",
                    Some(admin),
                    Some(json!({
                        "username": "alice",
                        "password": "password123",
                        "displayName": "Alice",
                        "role": "editor",
                    })),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{alice}");
            let alice_id = alice["id"].as_i64().unwrap();
            let alice_normal = surface_login(router, "alice", false).await;
            let alice_remember = surface_login(router, "alice", true).await;
            // The session that makes its own password change (on the admin
            // UI surface), plus one on the tag space.
            let alice_self = surface_login(router, "alice", false).await;
            let viewer = surface_login(router, "viewer1", false).await;
            for tokens in [&alice_normal, &alice_remember, &alice_self, &viewer] {
                assert_eq!(
                    session_statuses(router, tokens).await,
                    ok,
                    "{change:?}: before"
                );
            }

            let (status, body) = match change {
                AccountChange::Delete => {
                    session_call(
                        router,
                        session_request(
                            "DELETE",
                            &format!("/api/users/{alice_id}"),
                            Some(admin),
                            None,
                        ),
                    )
                    .await
                }
                AccountChange::Demote => {
                    session_call(
                        router,
                        session_request(
                            "PUT",
                            &format!("/api/users/{alice_id}"),
                            Some(admin),
                            Some(json!({ "displayName": "Alice", "role": "viewer" })),
                        ),
                    )
                    .await
                }
                AccountChange::PasswordReset => {
                    session_call(
                        router,
                        session_request(
                            "POST",
                            &format!("/api/users/{alice_id}/reset-password"),
                            Some(admin),
                            Some(json!({ "newPassword": "reset-password1" })),
                        ),
                    )
                    .await
                }
                AccountChange::PasswordChange => {
                    session_call(
                        router,
                        session_request(
                            "POST",
                            "/api/auth/change-password",
                            Some(&alice_self.ui),
                            Some(json!({
                                "currentPassword": "password123",
                                "newPassword": "changed-password1",
                            })),
                        ),
                    )
                    .await
                }
            };
            assert!(status.is_success(), "{change:?}: {status} {body}");

            assert_eq!(
                session_statuses(router, &alice_normal).await,
                revoked,
                "{change:?}: normal session"
            );
            assert_eq!(
                session_statuses(router, &alice_remember).await,
                revoked,
                "{change:?}: Remember me session"
            );
            // The changer keeps its session; its tag-space sibling (another
            // session of the same account) ends like the rest.
            let expected_self = match change {
                AccountChange::PasswordChange => (StatusCode::OK, StatusCode::UNAUTHORIZED),
                _ => revoked,
            };
            assert_eq!(
                session_statuses(router, &alice_self).await,
                expected_self,
                "{change:?}: the session that made the change"
            );
            assert_eq!(
                session_statuses(router, &viewer).await,
                ok,
                "{change:?}: bystander"
            );
            assert_eq!(
                session_call(
                    router,
                    session_request("GET", "/api/users", Some(admin), None)
                )
                .await
                .0,
                StatusCode::OK,
                "{change:?}: acting admin"
            );
        }
    }

    /// #204: a demoted account is authorized with its CURRENT role once it
    /// logs in again - and a stale session cannot keep its old role even for
    /// one request (it is refused outright, see the table test above).
    #[tokio::test]
    async fn a_fresh_session_after_a_demotion_uses_the_new_role() {
        let env = test_env().await;
        let router = &env.router;
        let admin = &env.admin_token;
        let (_, admin2) = session_call(
            router,
            session_request(
                "POST",
                "/api/users",
                Some(admin),
                Some(json!({
                    "username": "admin2",
                    "password": "password123",
                    "displayName": "Admin 2",
                    "role": "admin",
                })),
            ),
        )
        .await;
        let admin2_id = admin2["id"].as_i64().unwrap();
        let (status, _) = session_call(
            router,
            session_request(
                "PUT",
                &format!("/api/users/{admin2_id}"),
                Some(admin),
                Some(json!({ "displayName": "Admin 2", "role": "viewer" })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let fresh = surface_login(router, "admin2", false).await;
        assert_eq!(
            session_call(
                router,
                session_request("GET", "/api/users", Some(&fresh.ui), None)
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            session_statuses(router, &fresh).await,
            (StatusCode::OK, StatusCode::OK)
        );
    }

    /// #204: when the DB cannot answer the re-check, the request fails (500)
    /// but the session is KEPT - it works again as soon as the DB answers.
    #[tokio::test]
    async fn a_failed_recheck_fails_the_request_but_keeps_the_session() {
        let env = test_env().await;
        let router = &env.router;
        let viewer = surface_login(router, "viewer1", true).await;
        sqlx::query("ALTER TABLE users RENAME TO users_away")
            .execute(&env.pool)
            .await
            .unwrap();
        assert_eq!(
            session_statuses(router, &viewer).await,
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                StatusCode::INTERNAL_SERVER_ERROR
            )
        );
        sqlx::query("ALTER TABLE users_away RENAME TO users")
            .execute(&env.pool)
            .await
            .unwrap();
        assert_eq!(
            session_statuses(router, &viewer).await,
            (StatusCode::OK, StatusCode::OK)
        );
    }

    /// #204: the first account's session from `POST /api/auth/setup` is
    /// bound to the new account (`issue_account_token`), so a lookup-enabled
    /// `AuthState` accepts it - an unstamped token would be refused on its
    /// first use.
    #[tokio::test]
    async fn the_setup_session_is_bound_to_the_new_account() {
        let pool = migrate_memory().await.expect("migrate_memory");
        let users = UsersService::new(pool.clone());
        let audit = AuditLogService::new(pool.clone());
        let auth = user_auth_state(users.clone(), audit.clone());
        let router = extra_auth_router(users, auth.clone(), audit, true);
        let (status, body) = session_call(
            &router,
            session_request(
                "POST",
                "/api/auth/setup",
                None,
                Some(json!({
                    "username": "owner",
                    "password": "password123",
                    "displayName": "Owner",
                })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let token = body["token"].as_str().expect("setup token");
        let session = auth
            .authenticate(token)
            .await
            .unwrap()
            .expect("the setup session is accepted");
        assert_eq!(session.identity.id, "owner");
        assert_eq!(session.identity.role, "admin");
    }

    /// #431: ゲートの判断の総当たり表。例外（照合できないのに通す）は
    /// 「止める操作」かつ「それまで有効だったセッション」の 1 行だけ。
    #[test]
    fn session_gate_decision_table() {
        use GateDecision::*;
        use OperationKind::*;
        use SessionCheck::*;
        let table = [
            (Valid, Normal, false, Allow),
            (Valid, Normal, true, Allow),
            (Valid, StopWrites, false, Allow),
            (Valid, StopWrites, true, Allow),
            // 失効が確認できたら、止める操作でも拒否
            (Revoked, Normal, false, Unauthorized),
            (Revoked, Normal, true, Unauthorized),
            (Revoked, StopWrites, false, Unauthorized),
            (Revoked, StopWrites, true, Unauthorized),
            // 照合できない: 通常は 500、止める操作だけ例外
            (Unverified, Normal, false, Unavailable),
            (Unverified, Normal, true, Unavailable),
            (Unverified, StopWrites, false, Unauthorized),
            (Unverified, StopWrites, true, AllowUnverifiedStop),
        ];
        for (check, operation, previously_valid, expected) in table {
            assert_eq!(
                session_gate_decision(check, operation, previously_valid),
                expected,
                "{check:?} / {operation:?} / previously_valid={previously_valid}"
            );
        }
    }

    /// #431: 例外の入口は宣言だけ。緊急停止は止める操作、再開は通常の操作。
    #[test]
    fn only_the_write_stop_route_is_declared_as_a_stop_operation() {
        assert_eq!(WRITE_CONTROL_DISABLE_OPERATION, OperationKind::StopWrites);
        assert_eq!(WRITE_CONTROL_ENABLE_OPERATION, OperationKind::Normal);
    }
}
