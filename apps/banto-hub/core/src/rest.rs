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
//! ## banto のルーターをそのまま使う（I3'、2026-10-04）
//!
//! 管理系のうち `/api/auth/{status,setup,change-password}`（+ 試運転の
//! `POST /api/auth/grant/commissioning`）・`/api/users/*`・`/api/audit-log/*`
//! と、ログイン/ログアウトの監査・成功の監査（`record_write`）・監査の actor の
//! 解決（`actor_identity`）は `banto_server::routes` のものを使う（以前は
//! chronogazer/relay-wright からのコピーを持っていた。独自実装は banto に
//! 寄せる、2026-10-01 オーナー方針。ChronoGazer の I2a #498 と同じ）。
//! サービス（users/audit/settings）も banto の `banto-admin-services` のもの
//! （`crate::users`・`crate::audit`・`crate::settings` の doc comment）。
//!
//! 自前のまま残しているもの（banto-hub 固有）: 管理系の認証ゲート
//! [`require_session`]（#431 の `StopWrites`・[`SessionCheck`] の区別・5 秒の
//! 上限・[`UnverifiedStopException`]）とその後ろの RBAC の床
//! [`SessionRoleGuard`]（banto の `RoleGuard` に替えない理由はその doc
//! comment）、[`extract_ws_protocol_token`] と `crate::stream` の
//! `SessionStreamCredential`、[`require_editor`]、API キー・MCP・gRPC・
//! 書き込み監査・pending changes・sink・タグ空間などの各ルーター。
//!
//! banto のものに替えて変わった振る舞い（wire・監査の語彙を含む）:
//!
//! - 初回セットアップが 1 文で原子的になった（banto #277）。同時に来ても
//!   アカウントは 1 つだけで、残りは `200 { success: false, error }`。
//! - 失敗ログインの監査の `actorUsername` は 32 文字に切り詰められ
//!   （末尾が `…`）、存在しない長い名前でもダミーの検証が走る。トークン
//!   無し・無効なトークンのログアウトは記録されない（banto #278）。
//! - 監査ログ一覧の応答に `deletionEpoch` が乗る（#248 の受け皿。画面は
//!   まだ使わない）。`asOfId` が読めないときは axum 既定の平文の 400 では
//!   なく JSON の `400 { kind: "bad_request" }`。
//! - `PUT /api/audit-log/config` の成功の監査は `action: "settings_change"`・
//!   `resource: "settings"`・`entityId: null`（以前は `update` /
//!   `audit_log_config` / `"1"`）。`/api/audit-log/config` の権限拒否の
//!   `resource` も `settings`（一覧の拒否は従来どおり `audit_log`）。
//! - `POST /api/auth/change-password` を grant のセッション（試運転）で
//!   呼ぶと `403` と `password_change` の `denied` の監査（ADR-0017: grant は
//!   資格情報の持ち主ではない）。
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
use banto_server::routes::{
    actor_identity, audit_log_router, audit_logout_middleware, extra_auth_router, record_write,
    users_router, LogoutAuditState,
};
use banto_server::{
    auth_routes, require_banto_client_header, sse_route, ApiError, AuthState, AuthenticatedSession,
    GrantRegistry, ServerEvent,
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

use crate::api_keys::{
    api_key_verdict, ApiKeyContext, ApiKeyRejection, ApiKeysService, IssuedApiKey,
};
use crate::audit::{AuditEntry, AuditLogService};
use crate::commissioning::{CommissioningService, CommissioningState};
use crate::computed::ComputedEngine;
use crate::controller::{
    CollectionController, CollectionState, CollectionStatus, QuitInProgress, RunMode,
};
use crate::db::Db;
use crate::hub::{CollectorManager, SimulationCoverageReport, TagEntry, TagMap};
use crate::mqtt::MqttPublisher;
use crate::pending_changes::{PendingChange, PendingChangesService};
use crate::settings::{HubSettingsExt, MqttSettings, SettingsService, StoreSettings};
use crate::sink::{
    reject_delete_if_referenced_by_sink_group, remove_tag_from_all_sink_groups, SinkGroup,
    SinkGroupInput, SinkGroupService, SinkGroupStatusPush, SinkStatusSnapshot, SinkStatusStore,
};
use crate::system_info::{SystemInfoSampler, SystemInfoSnapshot};
use crate::users::{Role, UsersService};
use crate::value_source::{effective_simulation_for_tag, value_source_for_tag};
use crate::write_audit::{WriteAuditEntry, WriteAuditService};
use crate::write_control::{WriteControl, WriteControlChange};
use crate::write_rate::WriteRateLimiter;

// --- shared helpers -------------------------------------------------------
//
// I3'（2026-10-04）: bearer の取り出し（[`bearer_token`]）・監査の actor の解決
// （`actor_identity`）・成功の監査（`record_write`）は `banto_server::routes` の
// ものを使う（以前は chronogazer/relay-wright からのコピーを持っていた）。
// `crate::mcp`・`crate::stream` などが `crate::rest::bearer_token` のパスで
// 使うので re-export する。
//
// 試運転モード（未ロックダウン）でも **合成 identity を要求ごとに返す分岐は
// 無い**（banto v3.0.0 追従、2026-10-04、ADR-0017）。試運転中の操作は
// `POST /api/auth/grant/commissioning` で発行された grant トークン
// （`crate::commissioning::commissioning_grant_spec`、identity は
// `synthetic_identity()` 固定）で行われ、`AuthState` のトークン表に載っている
// ので、`actor_identity` はロックダウンの前後を問わず同じ 1 本の経路でよい。
// 監査ログ (`audit_log.actor_username`) に残る値は従来と同じ `commissioning`
// （設計 §5.6「試運転モード中に行われた操作」だと後から判別できる）。

pub(crate) use banto_server::routes::bearer_token;

// I3': `/api/auth/{status,setup,change-password}`・`/api/users/*`・
// `/api/audit-log/*` とログイン/ログアウトの監査は banto のルーターを使う。
// 各ホスト（`crate::runtime`・テスト）が使う 3 つは、これまでの
// `banto_hub_core::rest::*` のパスのまま呼べるよう re-export する。
pub use banto_server::routes::{audited_credential_verifier, user_auth_state, user_session_lookup};

// MQTT・store・gRPC・OPC UA の設定 API は `rest/settings.rs` へ移した
// （docs/banto-hub-driver-refactor-plan.md §8.1）。`crate::rest::X` のパスは
// ここで re-export して保つ（`crate::mcp`・`crate::runtime`・`crate::opcua` が使う）。
mod settings;
use settings::{grpc_settings_router, mqtt_settings_router, store_settings_router};
pub(crate) use settings::{
    opcua_settings_router, validate_grpc_settings_body, validate_mqtt_settings_request,
    validate_opcua_settings, validate_store_settings_request, GrpcSettingsBody,
    MqttSettingsRequest, MqttSettingsResponse, StoreSettingsRequest, StoreSettingsResponse,
};

// API キー管理・書き込み受付トグル・収集ライフサイクル制御・書き込み監査の閲覧 API は
// `rest/{api_keys,write_control,collection_control,write_audit}.rs` へ移した
// （docs/banto-hub-driver-refactor-plan.md §8.1）。`crate::rest::X` のパスはここで
// re-export して保つ（`crate::mcp` が使う）。
mod api_keys;
mod collection_control;
mod write_audit;
mod write_control;
use api_keys::api_keys_router;
pub(crate) use api_keys::{CreateApiKeyRequest, IssuedApiKeyResponse};
#[cfg(test)]
use collection_control::quit_in_progress_response;
use collection_control::{collection_control_router, CollectionStatusResponse};
use write_audit::write_audit_router;
pub(crate) use write_control::write_control_audit_detail;
use write_control::write_control_router;
#[cfg(test)]
use write_control::{WRITE_CONTROL_DISABLE_OPERATION, WRITE_CONTROL_ENABLE_OPERATION};

// I1 CRUD の共通層（収集構成の反映・pending queue への振り分け・変更系エラーの
// 写像）と PLC 接続の CRUD・接続テストは `rest/{crud,plc_connections}.rs` へ移した
// （docs/banto-hub-driver-refactor-plan.md §8.1）。`crate::rest::X` のパスはここで
// re-export して保つ（`crate::mcp` が使う）。
mod crud;
mod plc_connections;
pub(crate) use crud::{
    commit_catalog_and_notify, compute_pending_base_fingerprint, registry_change_should_queue,
    LIVE_APPLY_RECOVERY_HINT,
};
use crud::{
    default_payload_enabled, queue_pending_registry_change, redact_pending_change_for_response,
    RegistryMutationError, RegistryMutationResult, TagRegistryState,
};
#[cfg(test)]
use plc_connections::slmp_session_cpu_refusal;
pub(crate) use plc_connections::{reject_hub_connection_protocol, run_plc_connection_test};
pub use plc_connections::{
    PlcConnectionPayload, PlcConnectionResponse, PlcConnectionTestError, PlcConnectionTestPayload,
    PlcConnectionTestResponse,
};

// 収集グループとタグの CRUD・一括操作・`tag_registry_router`（と、それぞれの
// 作成・更新ペイロード）は `rest/{collection_groups,tags}.rs` へ移した
// （docs/banto-hub-driver-refactor-plan.md §8.1）。`crate::rest::X` のパスは
// ここで re-export して保つ（`crate::mcp` が使う）。
mod collection_groups;
mod tags;
pub use collection_groups::CollectionGroupPayload;
use tags::tag_registry_router;
pub use tags::{
    BatchTagsDeleteRequest, BatchTagsRequest, BatchTagsUpdateRequest, TagBatchUpdatePayload,
    TagPayload, TAG_THRESHOLDS_NOT_HELD_MESSAGE,
};
#[cfg(test)]
use tags::{TxTestHook, TX_TEST_HOOK};

// 保留中の変更（pending changes）API と外部 DB 連携（Sink）API は
// `rest/{pending_changes,sink}.rs` へ移した（docs/banto-hub-driver-refactor-plan.md §8.1）。
// ルーターは composition から使うのでここで取り込む。
mod pending_changes;
mod sink;
use pending_changes::pending_changes_router;
use sink::{sink_admin_router, sink_groups_router};

// `/api/v1/*` のタグ空間 API・認証、管理系の状態と値、書き込み API は
// `rest/{tag_space,admin_status,write}.rs` へ移した（docs/banto-hub-driver-refactor-plan.md §8.1）。
// `ApiDoc` の `paths(...)`・`schemas(...)` と composition が使う名前はここで取り込む。
mod admin_status;
mod tag_space;
mod write;
use admin_status::{
    __path_v1_events, admin_status_router, admin_tag_stream_router, v1_events, EventEntry,
    EventsResponse, ADMIN_TAG_STREAM_PATH,
};
use tag_space::{
    __path_v1_status, __path_v1_tags, __path_v1_value_read_now, __path_v1_value_single,
    __path_v1_values, build_catalog_response, build_values_response, resolve_value_names,
    tag_space_router, CatalogResponse, CatalogTagEntry, ConnectionStatusEntry, DbSourceStatusEntry,
    GrpcStatusEntry, LastApplyEntry, MqttStatusEntry, ReadNowResponse, SingleValueResponse,
    SinkStatusEntry, StatusResponse, TagsQuery, ValueEntry, ValuesQuery, ValuesResponse,
};
pub(crate) use tag_space::{compute_status, TagSpaceState};
#[cfg(test)]
use tag_space::{effective_simulation_for_connection, value_entry};
pub(crate) use write::parse_requested_value;
use write::{
    __path_v1_write_value, __path_v1_write_values_batch, v1_write_value, v1_write_values_batch,
    BatchWriteEntryRequest, BatchWriteEntryResponse, BatchWriteValueRequest,
    BatchWriteValuesResponse, WriteState, WriteValueRequest, WriteValueResponse,
};

// 式チェック・関数表（#342）、OpenAPI（`ApiDoc`）、試運転モード/ロックダウンは
// `rest/{expression,openapi,commissioning}.rs` へ移した（docs/banto-hub-driver-refactor-plan.md §8.1）。
// composition・`tag_registry_router`・`crate::mcp` が使う名前はここで取り込む。
mod commissioning;
mod expression;
mod openapi;
use commissioning::commissioning_router;
pub(crate) use expression::evaluate_expression_check;
use expression::{tags_expression_check, tags_expression_functions};
use openapi::openapi_router;
#[cfg(test)]
use openapi::ApiDoc;

/// `banto_server::require_auth`'s own 401 body, reproduced here for
/// `require_tag_space_auth` (T0-2's `/api/v1/*` auth middleware below) since
/// that middleware replaces `require_auth` entirely rather than wrapping it.
/// T19 S5: `crate::mcp`'s `POST /mcp` auth middleware reuses this same body
/// for all of its (simplified, single-status) auth failure branches.
pub(crate) fn unauthorized_response() -> Response {
    (StatusCode::UNAUTHORIZED, Json(ErrorBody::Unauthorized)).into_response()
}

/// [`require_session`]の`middleware::from_fn_with_state`用 state。従来の
/// `banto_server::require_auth`（`State<AuthState>`のみ）を直接差し替えず、
/// この型を挟む1段ラッパーにしてある理由は[`require_session`]のdoc comment参照。
#[derive(Clone)]
struct AuthGate {
    auth: AuthState,
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

/// 管理系ルーターの認証ゲート（`banto_server::require_auth` 相当の 1 段
/// ラッパー。`unauthorized_response`は`banto_server::require_auth`の401ボディを
/// そのまま再現したもの、このファイル冒頭のdoc comment参照）。
///
/// **試運転モードの分岐は無い**（banto v3.0.0 追従、2026-10-04、ADR-0017）。
/// 2026-08-30〜10-04 の `require_session` は未ロックダウン中の要求を
/// 無条件で素通しし、後段が合成 admin identity を使っていた（34 か所の迂回）。
/// 今は試運転中も bearer が要る: 管理 UI・bootstrap は
/// `POST /api/auth/grant/commissioning`（`extra_auth_router` が merge する banto の
/// `grant_router`、条件は未ロックダウン・loopback の peer）で合成 admin の grant
/// トークンを受け取り、それをここに通す。ロックダウンは `CommissioningService::
/// lock_down` が保存の直後に `revoke_grant_tokens` するので、次の要求から 401。
/// `require_auth` をそのまま使わない理由は下の #431 と、`/api/tag-stream` の
/// `Sec-WebSocket-Protocol` からの bearer の取り出し（[`extract_ws_protocol_token`]）。
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
/// 試運転の grant トークンも `authenticate` に通す（grant セッションは
/// アカウント照合を飛ばすので、DB 障害時でも `Valid`。#431 の例外は
/// アカウントのセッションにだけ意味がある）。
async fn require_session(
    State(gate): State<AuthGate>,
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
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
                // #430: `/api/tag-stream` をセッションで開いたときも、接続中に
                // 同じトークンを照合し直せるよう材料を載せる
                // （`crate::stream::SessionStreamCredential`）。試運転の grant
                // トークンも同じ（ロックダウンの `revoke_grant_tokens` の後の
                // 次の照合で `session_revoked` として閉じる、ADR-0017）。
                req.extensions_mut()
                    .insert(crate::stream::SessionStreamCredential::new(
                        gate.auth.clone(),
                        token,
                    ));
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
/// （`/api/v1/stream`専用）と`require_session`
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

/// [`require_session_role`] の state: [`require_session`] の後ろに積む
/// RBAC の床（`banto_server::routes::RoleGuard` と同じ 4 フィールド）。
///
/// **banto の `RoleGuard`/`require_role_at_least` に置き換えない**（I3'、
/// 2026-10-04 の判断）。banto のものは、手前のゲートが
/// `AuthenticatedSession` を載せていないとき `AuthState::authenticate`
/// （DB でのアカウント照合）でやり直し、DB が答えなければ 500 にする。
/// [`require_session`] は #431 の例外（[`GateDecision::AllowUnverifiedStop`]、
/// DB が照合に答えられないときの緊急停止 `POST /api/write-control/disable`）で
/// まさにそのセッションを載せずに通すので、banto の床に替えると緊急停止が
/// 床で 500 になり、#431 が無効になる（tests/write.rs の
/// 「照合できないときも止められる」テストが落ちる）。こちらはメモリ上の
/// トークン（`identity_for`）に戻る。`/api/tag-stream` の
/// `Sec-WebSocket-Protocol` からの bearer も見る（[`extract_ws_protocol_token`]）。
///
/// banto のルーター（`users_router`・`audit_log_router`）は banto の
/// `require_auth` + `RoleGuard` を自分で積んでいるので、この型は使わない
/// （どちらも #431 の例外の無い通常の操作で、`require_auth` は
/// [`require_session`] の [`OperationKind::Normal`] と同じ判断をする）。
#[derive(Clone)]
struct SessionRoleGuard {
    auth: AuthState,
    min: Role,
    resource: &'static str,
    audit: AuditLogService,
}

fn forbidden_response() -> Response {
    (StatusCode::FORBIDDEN, Json(ErrorBody::Forbidden)).into_response()
}

/// T2-4（設計 §6-4「トリップ」）: トリップ中の API キーでの
/// `/api/v1/*` アクセス - read/write いずれも 403。#435 で `/api/sink/*` と
/// MCP も同じ応答にそろえた（[`api_key_rejection_response`] 経由）。
fn key_tripped_response() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({ "error": "key_tripped" })),
    )
        .into_response()
}

/// #435: 使えない API キーへの HTTP 応答。REST のタグ空間 `/api/v1/*`・
/// サイドカー用の `/api/sink/*`・MCP（`crate::mcp::require_mcp_auth`）が共通で
/// 使う（gRPC は `crate::grpc::api_key_rejection_status`）。どう拒否するかは
/// [`crate::api_keys::api_key_verdict`] が決める:
/// - 失効・期限切れ・存在しない → 401（`ErrorBody::Unauthorized`）
/// - トリップ → 403 `{"error": "key_tripped"}`（認証はできたが今は使えない。
///   admin が解除すれば同じキーで戻るので、キーを捨てさせない）
/// - 照合できない（#434）→ 500（`ApiError`、セッションの照合失敗と同じ形）
pub(crate) fn api_key_rejection_response(rejection: ApiKeyRejection) -> Response {
    match rejection {
        ApiKeyRejection::Unauthenticated(_) => unauthorized_response(),
        ApiKeyRejection::Tripped => key_tripped_response(),
        ApiKeyRejection::Unavailable(err) => {
            eprintln!("banto-hub: API キー照合に失敗しました: {err}");
            ApiError(err).into_response()
        }
    }
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

async fn require_session_role(
    State(guard): State<SessionRoleGuard>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    // 手前の [`require_session`] が照合した `AuthenticatedSession`（試運転の
    // grant セッションを含む）を使う。無いのは #431 の例外（照合できなかった
    // 止める操作）だけで、そのときはメモリ上のトークンから引く（`/api/tag-stream`
    // の `Sec-WebSocket-Protocol` 経路も extension から取れる）。
    let identity = req
        .extensions()
        .get::<AuthenticatedSession>()
        .map(|session| session.identity.clone())
        .or_else(|| {
            bearer_token(req.headers())
                .map(str::to_string)
                .or_else(|| extract_ws_protocol_token(req.uri().path(), req.headers()))
                .and_then(|token| guard.auth.identity_for(&token))
        });
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
            audit: audit.clone(),
        },
        audit_logout_middleware,
    ));

    // ADR-0017: この Hub が資格情報なしで発行する grant は試運転の
    // `commissioning` だけ（`CommissioningService::grant_spec`: 合成 admin 固定、
    // 条件 = 未ロックダウン、loopback の peer のみ、上限
    // `COMMISSIONING_GRANT_MAX_SESSIONS`）。閲覧公開（`publicViewer`）は無い。
    let mut grants = GrantRegistry::new();
    grants
        .register(commissioning.grant_spec())
        .expect("the commissioning grant registers on an empty registry");
    let grants = Arc::new(grants);

    let admin = Router::new()
        .merge(audited_auth_routes)
        // I3': banto の `extra_auth_router`（status/setup/change-password +
        // `grant_router`）。`status_extras` は無し（`GET /api/auth/status` は
        // `{ initialized, grants }` のまま）。
        .merge(extra_auth_router(
            users.clone(),
            auth.clone(),
            audit.clone(),
            allow_setup,
            grants,
            None,
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
        .merge(users_router(users, audit.clone(), auth.clone()))
        .merge(audit_log_router(
            audit.clone(),
            SettingsService::new(Db::Sqlite(manager.pool())),
            auth.clone(),
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
            manager.clone(),
            controller.clone(),
            events.clone(),
        ))
        .merge(write_control_router(
            write_control.clone(),
            manager.clone(),
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
        .merge(store_settings_router(
            manager.clone(),
            audit.clone(),
            auth.clone(),
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
            audit.clone(),
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
            audit.clone(),
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

/// The registry as it was at the start of a write transaction, before the
/// write (#635). [`preflight_transaction`] compares the registry after the
/// write against it so that only the external-name collisions the write
/// **creates** are rejected - collisions already in the DB (rows written
/// before 2026-10-17) do not block unrelated edits or the edits that
/// resolve them. Read it with [`registry_before`] right after `BEGIN
/// IMMEDIATE` and before any write, on the same transaction.
pub(crate) struct RegistryBefore(RegistrySnapshot);

pub(crate) async fn registry_before(
    connection: &mut sqlx::SqliteConnection,
) -> Result<RegistryBefore, ApiError> {
    RegistrySnapshot::load_connection(connection)
        .await
        .map(RegistryBefore)
        .map_err(|err| preflight_api_error(format!("レジストリの読み取りに失敗しました: {err}")))
}

/// #635（2026-10-17 オーナー決定 案 A）: 接続・グループ・タグの作成・改名
/// （グループの付け替えを含む）で、外部名（`conn.group.tag`）が既存のタグと
/// 重なるなら検証エラーにする。判定は `crate::hub::new_external_name_collisions`
/// （catalog と同じ組み立て方の純関数）。REST・MCP・未適用キューの適用・
/// 一括作成／更新、設定パッケージの取り込み（REST の作成／更新を呼ぶ）は
/// すべて [`preflight_transaction`] を通るので、ここ 1 か所で断る。
fn reject_new_external_name_collisions(
    before: &RegistryBefore,
    after: &RegistrySnapshot,
) -> Result<(), ApiError> {
    let collisions = crate::hub::new_external_name_collisions(&before.0, after);
    if collisions.is_empty() {
        return Ok(());
    }
    Err(ApiError(BantoError::Validation {
        field_errors: collisions
            .iter()
            .map(|collision| FieldError {
                field: "name".to_string(),
                message: collision.message(),
            })
            .collect(),
    }))
}

// T21 S1-b（docs/banto-hub-t21-design.md §5）: `pub(crate)` so
// `crate::mcp`'s config tools (`create_connection`/`delete_connection`) can
// run the exact same in-transaction preflight REST does. #635 added
// `before` (read with [`registry_before`] before the write): every write
// passes it, deletes included, so a new write path cannot forget it.
pub(crate) async fn preflight_transaction(
    connection: &mut sqlx::SqliteConnection,
    before: &RegistryBefore,
) -> Result<RegistrySnapshot, ApiError> {
    let snapshot = RegistrySnapshot::load_connection(connection)
        .await
        .map_err(|err| preflight_api_error(format!("レジストリの読み取りに失敗しました: {err}")))?;
    reject_new_external_name_collisions(before, &snapshot)?;
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
    use banto_server::{Identity, SessionValidation};
    use banto_tstore::{Clock, ManualClock, SystemClock};
    use tokio::sync::broadcast as tokio_broadcast;
    use tower::ServiceExt;

    const CLIENT_HEADER: (&str, &str) = ("X-Banto-Client", "banto");

    fn slmp_conn_with_cpu(id: i64, cpu: &str) -> banto_tags::PlcConnection {
        banto_tags::PlcConnection {
            id,
            name: format!("conn-{id}"),
            protocol: "slmp".to_string(),
            host: "127.0.0.1".to_string(),
            port: 5007,
            unit_id: 1,
            enabled: true,
            simulation: false,
            word_order: "low_high".to_string(),
            cpu_type: cpu.to_string(),
            database: None,
            username: None,
            password: None,
        }
    }

    /// #605 P2: 接続テストが比べる CPU は、いま読む handle 自身が作成時に持っていた
    /// CPU でなければならない。古い R の handle を取ったあと、読む前に設定の反映
    /// （`BrokerSimRegistry::resolve`）が新しい Q を公開する、という交差を
    /// 同期点で再現する。別のレジストリ（resolve が公開した最新の CPU）を引くと
    /// 要求の Q と一致して通ってしまうが、handle の CPU（R）で比べるので断られる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn session_cpu_check_uses_the_handle_generation_not_the_latest_published_cpu() {
        let sessions = crate::broker_glue::HubSessions::new(banto_broker::BackoffConfig::default());
        let registry = crate::broker_glue::BrokerSimRegistry::new();
        let old = slmp_conn_with_cpu(1, "R");
        registry.resolve(&old).await;
        sessions.ensure_connection(&old).unwrap();

        // 1. 古い R の handle を取る。
        let handle = sessions.handle_for(1).expect("live session");
        assert_eq!(handle.slmp_cpu(), Some(banto_plc::SlmpCpu::R));

        // 2. 読む前に、設定の反映が新しい Q を公開する（古いセッションはまだ生きている）。
        let (_, _, changed) = registry.resolve(&slmp_conn_with_cpu(1, "Q")).await;
        assert!(changed, "Q の公開は resolve で changed になる");

        // 3. 要求の Q との比較は、古い handle の CPU（R）に対して行われ、断られる。
        let refusal = slmp_session_cpu_refusal(handle.slmp_cpu(), &Some("Q".to_string()))
            .expect("R のセッションを Q の要求で通さない");
        assert_eq!(refusal.kind, "protocol");
        assert!(
            refusal.message.contains("保存してからテスト"),
            "{}",
            refusal.message
        );

        // 世代が同じなら通る。作り直したあとの handle は Q を持つ。
        assert!(slmp_session_cpu_refusal(handle.slmp_cpu(), &Some("R".to_string())).is_none());
        assert!(slmp_session_cpu_refusal(handle.slmp_cpu(), &None).is_none());
        assert!(sessions.stop_and_join(1).await);
        sessions
            .ensure_connection(&slmp_conn_with_cpu(1, "Q"))
            .unwrap();
        let fresh = sessions.handle_for(1).expect("rebuilt session");
        assert_eq!(fresh.slmp_cpu(), Some(banto_plc::SlmpCpu::Q));
        assert!(slmp_session_cpu_refusal(fresh.slmp_cpu(), &Some("Q".to_string())).is_none());
        sessions.shutdown().await;
    }

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
        test_env_on_pool(pool, clock, locked_down).await
    }

    /// 別接続からの書き込みと競合させるテスト用（#528）: メモリ DB ではなく
    /// **ファイルの DB**（接続が複数開く）の上に [`test_env`] と同じ環境を組む。
    async fn test_env_on_file_db(path: &std::path::Path) -> TestEnv {
        let pool = crate::db::init_db(path).await.expect("init_db");
        test_env_on_pool(pool, Arc::new(SystemClock), true).await
    }

    /// [`test_env_with_clock_and_lock`] の本体（渡された `pool` の上に環境を組む）。
    async fn test_env_on_pool(
        pool: sqlx::SqlitePool,
        clock: Arc<dyn Clock>,
        locked_down: bool,
    ) -> TestEnv {
        let (tx, _rx) = tokio_broadcast::channel(16);
        let users = UsersService::new(Db::Sqlite(pool.clone()));
        let audit = AuditLogService::new(Db::Sqlite(pool.clone()));
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
        let settings = SettingsService::new(Db::Sqlite(pool.clone()));
        let commissioning = CommissioningService::load(settings, users.clone(), auth.clone())
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

    /// `index.html` だけを持つ UI アセット（SPA のフォールバックの確認用）。
    struct SpaAssets;

    impl banto_server::UiAssets for SpaAssets {
        fn get(path: &str) -> Option<(String, std::borrow::Cow<'static, [u8]>)> {
            (path == "index.html").then(|| {
                (
                    "text/html; charset=utf-8".to_string(),
                    std::borrow::Cow::Borrowed(b"<html>spa-index</html>".as_slice()),
                )
            })
        }
    }

    /// 本番と同じ合成（`api_router(..).merge(static_router(..))`）で、存在しない
    /// `/api/*` が SPA の `index.html`（200）ではなく JSON の 404
    /// （`kind: not_found`）になること（banto v6.3.1 の `static_router`、
    /// banto-industrial #547）。既知の API ルートと SPA のルートは従来どおり。
    /// 反証: 修正は上流（banto）にあり、banto v6.3.0 以前に戻さないと落とせない。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unknown_api_path_is_json_404_through_the_composed_router() {
        let (api, token, _dir) = router_with_token().await;
        let app = api.merge(banto_server::static_router::<SpaAssets>());

        async fn send(app: &Router, req: HttpRequest<Body>) -> (StatusCode, String, Vec<u8>) {
            let response = app.clone().oneshot(req).await.unwrap();
            let status = response.status();
            let content_type = response
                .headers()
                .get("content-type")
                .map(|v| v.to_str().unwrap().to_string())
                .unwrap_or_default();
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            (status, content_type, bytes.to_vec())
        }

        for (method, uri) in [
            ("GET", "/api/does-not-exist"),
            ("POST", "/api/does-not-exist"),
            ("GET", "/api"),
        ] {
            let req = HttpRequest::builder()
                .method(method)
                .uri(uri)
                .header("Authorization", format!("Bearer {token}"))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap();
            let (status, content_type, bytes) = send(&app, req).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}");
            assert!(content_type.starts_with("application/json"), "{uri}");
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["kind"], json!("not_found"), "{uri}");
        }

        // 既知のルートは従来どおり。
        let req = HttpRequest::get("/api/auth/check")
            .header("Authorization", format!("Bearer {token}"))
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .body(Body::empty())
            .unwrap();
        let (status, _, bytes) = send(&app, req).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(bytes, b"true");

        // SPA のルートは index.html。
        let req = HttpRequest::get("/monitor").body(Body::empty()).unwrap();
        let (status, content_type, bytes) = send(&app, req).await;
        assert_eq!(status, StatusCode::OK);
        assert!(content_type.starts_with("text/html"));
        assert_eq!(bytes, b"<html>spa-index</html>");
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

    /// 2026-10-19 オーナー決定（migration 0022）: グループ名は接続内で一意。
    /// REST で別の接続の下に同じ名前のグループを作れ、同じ接続の下では
    /// 作れない。別の接続へ移す更新が移り先の同名と重なれば `name` の項目
    /// エラー（422）になり、行は動かない。
    #[tokio::test]
    async fn collection_group_names_are_unique_per_connection_over_rest() {
        let env = test_env().await;
        let (router, token) = (&env.router, env.admin_token.as_str());
        let mut conn_ids = Vec::new();
        for (name, port) in [("line-per-conn-a", 15101), ("line-per-conn-b", 15102)] {
            let (status, conn) = admin_post(
                router,
                "/api/plc-connections",
                token,
                json!({ "name": name, "host": "127.0.0.1", "port": port }),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{conn:?}");
            conn_ids.push(conn["id"].as_i64().unwrap());
        }
        let (a, b) = (conn_ids[0], conn_ids[1]);

        let (status, ga) = admin_post(
            router,
            "/api/collection-groups",
            token,
            json!({ "name": "fast", "plcConnectionId": a, "periodMs": 1000 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{ga:?}");
        let (status, gb) = admin_post(
            router,
            "/api/collection-groups",
            token,
            json!({ "name": "fast", "plcConnectionId": b, "periodMs": 1000 }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "same name under another connection: {gb:?}"
        );

        let (status, body) = admin_post(
            router,
            "/api/collection-groups",
            token,
            json!({ "name": "fast", "plcConnectionId": a, "periodMs": 1000 }),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
        assert_eq!(body["field_errors"][0]["field"], "name", "{body:?}");
        assert_eq!(
            body["field_errors"][0]["message"], "この接続内では既に使用されています",
            "{body:?}"
        );

        let gb_id = gb["id"].as_i64().unwrap();
        let (status, body) = admin_put(
            router,
            &format!("/api/collection-groups/{gb_id}"),
            token,
            json!({
                "name": "fast",
                "plcConnectionId": a,
                "periodMs": 1000,
                "enabled": true,
                "defaultWritable": true,
                "querySql": null,
            }),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
        assert_eq!(body["field_errors"][0]["field"], "name", "{body:?}");
        let still = CollectionGroupService::new(env.pool.clone())
            .get(gb_id)
            .await
            .unwrap();
        assert_eq!(
            still.plc_connection_id, b,
            "a refused move leaves the row in place"
        );
    }

    /// #635（2026-10-17 オーナー決定 案 A）: 接続・グループ・タグの作成と改名で、
    /// ドットを含む名前をつないだ外部名が既存のタグと重なれば 422 で断り、
    /// どの既存のタグと重なるかを文言で示す。断った書き込みは残らない。
    #[tokio::test]
    async fn registry_writes_reject_a_new_external_name_collision() {
        let env = test_env().await;
        let (router, token) = (&env.router, env.admin_token.as_str());

        async fn post_ok(
            router: &Router,
            token: &str,
            path: &str,
            body: serde_json::Value,
        ) -> serde_json::Value {
            let (status, created) = admin_post(router, path, token, body).await;
            assert_eq!(status, StatusCode::OK, "{path}: {created:?}");
            created
        }
        fn assert_collision(status: StatusCode, body: &serde_json::Value, existing: &str) {
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
            assert_eq!(body["field_errors"][0]["field"], "name", "{body:?}");
            let message = body["field_errors"][0]["message"].as_str().unwrap_or("");
            assert!(
                message.contains("既存のタグ") && message.contains(existing),
                "{message}"
            );
        }

        // 既存: 接続「a.b」/ グループ「c」/ タグ「d」= 外部名 a.b.c.d
        let ab = post_ok(
            router,
            token,
            "/api/plc-connections",
            json!({ "name": "a.b", "host": "127.0.0.1", "port": 15081 }),
        )
        .await;
        let c = post_ok(
            router,
            token,
            "/api/collection-groups",
            json!({ "name": "c", "plcConnectionId": ab["id"], "periodMs": 1000 }),
        )
        .await;
        let tag = |name: &str, group: &serde_json::Value| {
            json!({
                "name": name,
                "collectionGroupId": group["id"],
                "address": "40001",
                "dataType": "i16",
            })
        };
        post_ok(router, token, "/api/tags", tag("d", &c)).await;

        // 作成: 接続「a」/ グループ「b.c」/ タグ「d」も a.b.c.d になる。
        let a = post_ok(
            router,
            token,
            "/api/plc-connections",
            json!({ "name": "a", "host": "127.0.0.1", "port": 15082 }),
        )
        .await;
        let bc = post_ok(
            router,
            token,
            "/api/collection-groups",
            json!({ "name": "b.c", "plcConnectionId": a["id"], "periodMs": 1000 }),
        )
        .await;
        let (status, body) = admin_post(router, "/api/tags", token, tag("d", &bc)).await;
        assert_collision(status, &body, "接続「a.b」/ グループ「c」/ タグ「d」");
        assert!(body["field_errors"][0]["message"]
            .as_str()
            .is_some_and(|message| message.contains("a.b.c.d")));

        // タグの改名: 「e」で作ってから「d」へ変える。
        let e = post_ok(router, token, "/api/tags", tag("e", &bc)).await;
        let (status, body) = admin_put(
            router,
            &format!("/api/tags/{}", e["id"]),
            token,
            tag("d", &bc),
        )
        .await;
        assert_collision(status, &body, "接続「a.b」/ グループ「c」/ タグ「d」");

        // グループの改名: a.b / zz / y を作り、グループを「c2.x」へ変えると、
        // 既存の a.b / c2 / x.y（= a.b.c2.x.y）と重なる。
        let c2 = post_ok(
            router,
            token,
            "/api/collection-groups",
            json!({ "name": "c2", "plcConnectionId": ab["id"], "periodMs": 1000 }),
        )
        .await;
        post_ok(router, token, "/api/tags", tag("x.y", &c2)).await;
        let zz = post_ok(
            router,
            token,
            "/api/collection-groups",
            json!({ "name": "zz", "plcConnectionId": ab["id"], "periodMs": 1000 }),
        )
        .await;
        post_ok(router, token, "/api/tags", tag("y", &zz)).await;
        let (status, body) = admin_put(
            router,
            &format!("/api/collection-groups/{}", zz["id"]),
            token,
            json!({ "name": "c2.x", "plcConnectionId": ab["id"], "periodMs": 1000 }),
        )
        .await;
        assert_collision(status, &body, "接続「a.b」/ グループ「c2」/ タグ「x.y」");

        // 接続の改名: 接続「a」は使用中の名前なので先に消す。m / b.c2 / d を作り、
        // m を「a」へ変えると a.b.c2.d になり、既存の a.b / c2 / d と重なる。
        post_ok(router, token, "/api/tags", tag("d", &c2)).await;
        let (status, body) =
            admin_delete(router, &format!("/api/plc-connections/{}", a["id"]), token).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body:?}");
        let m = post_ok(
            router,
            token,
            "/api/plc-connections",
            json!({ "name": "m", "host": "127.0.0.1", "port": 15083 }),
        )
        .await;
        let bc2 = post_ok(
            router,
            token,
            "/api/collection-groups",
            json!({ "name": "b.c2", "plcConnectionId": m["id"], "periodMs": 1000 }),
        )
        .await;
        post_ok(router, token, "/api/tags", tag("d", &bc2)).await;
        let (status, body) = admin_put(
            router,
            &format!("/api/plc-connections/{}", m["id"]),
            token,
            json!({ "name": "a", "host": "127.0.0.1", "port": 15083 }),
        )
        .await;
        assert_collision(status, &body, "接続「a.b」/ グループ「c2」/ タグ「d」");

        // 断った書き込みは残らず、catalog に重なりは無い。
        let names: Vec<String> = sqlx::query_scalar("SELECT name FROM plc_connections")
            .fetch_all(&env.pool)
            .await
            .unwrap();
        assert!(names.contains(&"m".to_owned()) && !names.contains(&"a".to_owned()));
        assert!(env.manager.tag_map().collisions().is_empty());
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

        let started = controller
            .start(RunMode::Configured)
            .await
            .expect("収集を開始できる");
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

        let started = controller
            .start(RunMode::Configured)
            .await
            .expect("収集を開始できる");
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

        let audit = AuditLogService::new(Db::Sqlite(env.pool.clone()));
        let entries = audit.list(ListParams::default()).await.unwrap();
        let denied = entries
            .rows
            .iter()
            .find(|row| row.action == "denied" && row.resource == "api_keys")
            .expect("a denied/api_keys audit row should exist");
        assert_eq!(denied.entity_id.as_deref(), Some(id.to_string().as_str()));
    }

    /// #435: 使えない API キーへの HTTP 応答の表（REST・サイドカー用・MCP で共通の
    /// 変換）。失効・期限切れ・存在しないは 401、トリップは 403 `key_tripped`、
    /// 照合できないは 500。
    #[tokio::test]
    async fn api_key_rejection_response_table() {
        use crate::api_keys::{ApiKeyRejection as R, UnauthenticatedReason as U};
        let table: Vec<(&str, R, StatusCode, serde_json::Value)> = vec![
            (
                "revoked",
                R::Unauthenticated(U::Revoked),
                StatusCode::UNAUTHORIZED,
                serde_json::json!("unauthorized"),
            ),
            (
                "expired",
                R::Unauthenticated(U::Expired),
                StatusCode::UNAUTHORIZED,
                serde_json::json!("unauthorized"),
            ),
            (
                "not found",
                R::Unauthenticated(U::NotFound),
                StatusCode::UNAUTHORIZED,
                serde_json::json!("unauthorized"),
            ),
            (
                "tripped",
                R::Tripped,
                StatusCode::FORBIDDEN,
                serde_json::json!("key_tripped"),
            ),
            (
                "unavailable",
                R::Unavailable(BantoError::Storage("db down".to_string())),
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!("storage"),
            ),
        ];
        for (label, rejection, status, marker) in table {
            let response = api_key_rejection_response(rejection);
            assert_eq!(response.status(), status, "{label}");
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            let got = if body.get("error").is_some() {
                &body["error"]
            } else {
                &body["kind"]
            };
            assert_eq!(*got, marker, "{label}: {body}");
        }
    }

    /// #434: `GET` を API キーで叩いてステータスと本文を返す。
    async fn get_with_key(
        router: &Router,
        path: &str,
        key: &str,
    ) -> (StatusCode, serde_json::Value) {
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
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    /// #434: タグ空間 `/api/v1/*` とサイドカー用の `/api/sink/config`。無効な
    /// キーは DB が答えていれば従来どおり（失効・期限切れ・存在しないは 401、
    /// タグ空間のトリップは 403 `key_tripped`）。照合そのものができない（DB
    /// エラー）ときは、どのキーでも 401 ではなく 500（`ApiError`、セッションの
    /// 照合失敗と同じ形）。
    #[tokio::test]
    async fn api_key_check_failure_is_500_while_invalid_keys_stay_401() {
        let env = test_env().await;
        let scopes = || vec!["read".to_string(), "admin".to_string()];
        let valid = env
            .api_keys
            .issue("state-valid", scopes(), None)
            .await
            .unwrap();
        let revoked = env
            .api_keys
            .issue("state-revoked", scopes(), None)
            .await
            .unwrap();
        env.api_keys.revoke(revoked.id).await.unwrap();
        let tripped = env
            .api_keys
            .issue("state-tripped", scopes(), None)
            .await
            .unwrap();
        env.api_keys.trip(tripped.id).await.unwrap();
        let expired = env
            .api_keys
            .issue("state-expired", scopes(), Some(1))
            .await
            .unwrap();
        let unknown = "bh_ZZZZZZZZ_well-formed-but-unregistered".to_string();
        // (キー, `/api/v1/tags` の期待, `/api/sink/config` の期待)
        let keys: Vec<(&str, String, StatusCode, StatusCode)> = vec![
            ("valid", valid.key, StatusCode::OK, StatusCode::OK),
            (
                "revoked",
                revoked.key,
                StatusCode::UNAUTHORIZED,
                StatusCode::UNAUTHORIZED,
            ),
            (
                "tripped",
                tripped.key,
                StatusCode::FORBIDDEN,
                StatusCode::FORBIDDEN,
            ),
            (
                "expired",
                expired.key,
                StatusCode::UNAUTHORIZED,
                StatusCode::UNAUTHORIZED,
            ),
            (
                "unknown",
                unknown,
                StatusCode::UNAUTHORIZED,
                StatusCode::UNAUTHORIZED,
            ),
        ];
        for (label, key, tags, sink) in &keys {
            let (status, body) = get_with_key(&env.router, "/api/v1/tags", key).await;
            assert_eq!(status, *tags, "tags {label}: {body:?}");
            let (status, sink_body) = get_with_key(&env.router, "/api/sink/config", key).await;
            assert_eq!(status, *sink, "sink {label}: {sink_body:?}");
            // #435: トリップはどちらも 403 `key_tripped`（同じ本文）。
            if *label == "tripped" {
                assert_eq!(body["error"], "key_tripped", "{body:?}");
                assert_eq!(sink_body["error"], "key_tripped", "{sink_body:?}");
            }
        }

        sqlx::query("ALTER TABLE api_keys RENAME TO api_keys_away")
            .execute(&env.pool)
            .await
            .expect("move api_keys away");
        for (label, key, _, _) in &keys {
            for path in ["/api/v1/tags", "/api/sink/config"] {
                let (status, body) = get_with_key(&env.router, path, key).await;
                assert_eq!(
                    status,
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "{path} {label}: the key could not be checked, so it is not a 401: {body:?}"
                );
                assert_eq!(body["kind"], "storage", "{path} {label}: {body:?}");
            }
        }
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

        let audit = AuditLogService::new(Db::Sqlite(env.pool.clone()));
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

    /// `ApiDoc::openapi()` の出力を、コミット済みスナップショット
    /// （`tests/snapshots/openapi.json`）と完全一致で比較する。utoipa の
    /// メジャー更新などで API 契約（パス・パラメータ・必須性・型）が意図せず
    /// 変わっていないことを検知するためのテスト。再生成は
    /// `UPDATE_OPENAPI_SNAPSHOT=1 cargo test -p banto-hub-core openapi_snapshot`。
    /// 比較は `serde_json::Value`（キー順非依存）、ファイルは pretty 出力。
    #[test]
    fn openapi_snapshot_matches() {
        let actual = serde_json::to_value(ApiDoc::openapi()).expect("openapi serialize");
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("snapshots")
            .join("openapi.json");
        if matches!(std::env::var("UPDATE_OPENAPI_SNAPSHOT").as_deref(), Ok("1")) {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let mut text = serde_json::to_string_pretty(&actual).unwrap();
            text.push('\n');
            std::fs::write(&path, text).unwrap();
            return;
        }
        let expected: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&path)
                .expect("snapshot missing; run with UPDATE_OPENAPI_SNAPSHOT=1"),
        )
        .expect("snapshot parse");
        assert!(
            actual == expected,
            "OpenAPI output differs from tests/snapshots/openapi.json; review the diff and regenerate with UPDATE_OPENAPI_SNAPSHOT=1"
        );
    }

    /// axum 0.8 の `Router` はルート一覧を返す公開 API を持たないので、`Debug`
    /// 出力（`path_router.routes` の `RouteId(n): MethodRouter { get: Route, .. }`
    /// と `node` の `RouteId(n): "/path"`）から「METHOD path」を復元する。
    /// `fallback_router` 以降は読まない。形式が変わって 1 本も取れなければ
    /// 呼び出し側の assert が落ちる（axum を上げたときはここを見直す）。
    fn route_list_from_debug(router: &Router) -> Vec<String> {
        const METHODS: [&str; 9] = [
            "get", "head", "delete", "options", "patch", "post", "put", "trace", "connect",
        ];
        let dump = format!("{router:#?}");
        let dump = dump.split("fallback_router:").next().unwrap();
        let (routes_part, node_part) = dump
            .split_once("        node: Node {")
            .expect("Router Debug に node が無い");
        let id_of = |lines: &mut std::iter::Peekable<std::str::Lines>| -> Option<u32> {
            // `RouteId(` の次の行が `N,`
            let n = lines.next()?.trim().trim_end_matches(',').to_string();
            n.parse().ok()
        };
        // id -> メソッド一覧
        let mut methods: std::collections::BTreeMap<u32, Vec<&str>> = Default::default();
        let mut lines = routes_part.lines().peekable();
        let mut current: Option<u32> = None;
        while let Some(line) = lines.next() {
            let t = line.trim();
            if t == "RouteId(" {
                current = id_of(&mut lines);
                if let Some(id) = current {
                    methods.entry(id).or_default();
                }
                continue;
            }
            if let Some(id) = current {
                for m in METHODS {
                    if let Some(rest) = t.strip_prefix(&format!("{m}: ")) {
                        if rest != "None," {
                            methods.get_mut(&id).unwrap().push(m);
                        }
                    }
                }
            }
        }
        // id -> パス
        let mut paths: std::collections::BTreeMap<u32, String> = Default::default();
        let mut lines = node_part.lines().peekable();
        let mut pending: Option<u32> = None;
        while let Some(line) = lines.next() {
            let t = line.trim();
            if t == "RouteId(" {
                pending = id_of(&mut lines);
                continue;
            }
            if let (Some(id), Some(rest)) = (pending, t.strip_prefix("): \"")) {
                if let Some(path) = rest.strip_suffix("\",") {
                    paths.insert(id, path.to_string());
                    pending = None;
                }
            }
        }
        let mut out = Vec::new();
        for (id, ms) in &methods {
            let path = paths
                .get(id)
                .unwrap_or_else(|| panic!("RouteId({id}) のパスが node に無い"));
            for m in ms {
                out.push(format!("{} {path}", m.to_uppercase()));
            }
        }
        out.sort();
        out
    }

    /// `api_router`（本番の `api_router_with_controller` と同じ
    /// `api_router_with_controller_mode` の合成）と `opcua_settings_router` が登録する「METHOD path」の
    /// 全一覧を、コミット済みスナップショット（`tests/snapshots/routes.txt`、
    /// ソート済み・1 行 1 ルート）と完全一致で比較する。rest.rs を子モジュールへ
    /// 分割する間に、ルートが増減・変更していないことを検知するためのテスト
    /// （docs/banto-hub-driver-refactor-plan.md §8.1）。`HEAD` は axum が `GET`
    /// から暗黙に扱うので一覧には出ない。再生成は
    /// `UPDATE_ROUTES_SNAPSHOT=1 cargo test -p banto-hub-core routes_snapshot`。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn routes_snapshot_matches() {
        let env = test_env().await;
        let mut routes = route_list_from_debug(&env.router);
        // `/api/opcua-*` は `HubRuntime`（runtime.rs）が `api_router_with_controller`
        // とは別に組んで merge するので、同じ手順で別に取って足す。
        let users = UsersService::new(Db::Sqlite(env.pool.clone()));
        let audit = AuditLogService::new(Db::Sqlite(env.pool.clone()));
        let (tx, _rx) = tokio_broadcast::channel(16);
        let opcua_router = opcua_settings_router(
            env.manager.clone(),
            crate::opcua::OpcUaPublisher::new(env.manager.clone()),
            audit.clone(),
            user_auth_state(users, audit),
            tx,
        );
        routes.extend(route_list_from_debug(&opcua_router));
        routes.sort();
        assert!(
            routes.len() > 50 && routes.contains(&"GET /api/status".to_string()),
            "ルート一覧の復元に失敗（axum の Debug 形式が変わった可能性）: {} 本",
            routes.len()
        );
        let mut actual = routes.join("\n");
        actual.push('\n');
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("snapshots")
            .join("routes.txt");
        if matches!(std::env::var("UPDATE_ROUTES_SNAPSHOT").as_deref(), Ok("1")) {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, actual).unwrap();
            return;
        }
        let expected = std::fs::read_to_string(&path)
            .expect("snapshot missing; run with UPDATE_ROUTES_SNAPSHOT=1")
            .replace("\r\n", "\n");
        assert!(
            actual == expected,
            "route list differs from tests/snapshots/routes.txt; review the diff and regenerate with UPDATE_ROUTES_SNAPSHOT=1\n--- actual ---\n{actual}"
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
    fn rest_current_invalid_values_serialize_as_null_bad() {
        let mut tag = metadata_test_tag("internal", false, true);
        let store = crate::computed::ServerTagStore::new();
        let status = metadata_test_status(CollectionState::Stopped, RunMode::Configured);
        for data_type in ["f32", "bit"] {
            tag.data_type = data_type.to_string();
            for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 2.0] {
                if data_type != "bit" && value.is_finite() {
                    continue;
                }
                store.set(&tag.tag_key, Some(value), banto_collect::Quality::Good, 123);
                let json = serde_json::to_value(value_entry(
                    &tag.external_name,
                    &tag,
                    None,
                    &store,
                    999,
                    &status,
                    &TagMap::default(),
                    &empty_computed_engine(),
                ))
                .unwrap();
                assert_eq!(json["v"], serde_json::Value::Null);
                assert_eq!(json["q"], "bad");
                assert_eq!(json["t"], 123);
            }
        }
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

    /// #392 A2 レビュー（2026-10-12）: 終了の受付中に断った開始は 409 で、
    /// 本文は管理 UI（`collectionControlAdmin.ts` の `isErrorBody`）が
    /// `message` を表示できる共通の [`ErrorBody`] の形。
    #[tokio::test]
    async fn quit_in_progress_is_http_409_with_a_displayable_error_body() {
        let response = quit_in_progress_response(QuitInProgress);
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["kind"], "other");
        assert_eq!(body["message"], crate::controller::QUIT_IN_PROGRESS_MESSAGE);
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
                cpu_type: None,
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
    /// `failure_reason` にフィールド単位の詳細（`name: この接続内では既に使用されています`、
    /// グループ名は 2026-10-19 から接続内で一意 - migration 0022）
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
            failure_reason.contains("name")
                && failure_reason.contains("この接続内では既に使用されています"),
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
                    cpu_type: None,
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
                    cpu_type: None,
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
    /// `settings`リソースへの`settings_change`監査エントリを1件だけ記録する
    /// （I3' で banto の `audit_log_router` に替えてからの語彙）ことを1本の
    /// テストで固定する（`collection_control_requires_admin_and_csrf_and_is_idempotent`
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

        // I3'（banto の `audit_log_router`）: 設定の読み書きは `settings`
        // リソース。viewer の拒否 2 件（GET・PUT）と、保存の
        // `settings_change` 1 件（`entityId` なし）。以前の写しは
        // `update`/`audit_log_config`/`"1"` で、拒否は `audit_log` だった。
        let rows: Vec<(String, String, String, Option<String>)> = sqlx::query_as(
            "SELECT action, resource, result, entity_id FROM audit_log              WHERE resource IN ('settings', 'audit_log', 'audit_log_config') ORDER BY id",
        )
        .fetch_all(&env.pool)
        .await
        .unwrap();
        let denied = (
            "denied".to_string(),
            "settings".to_string(),
            "denied".to_string(),
            None,
        );
        assert_eq!(
            rows,
            vec![
                denied.clone(),
                denied,
                (
                    "settings_change".to_string(),
                    "settings".to_string(),
                    "ok".to_string(),
                    None
                ),
            ]
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

    /// #428: `asOfId` 付きの取得（2 ブロック目以降）では剪定しない - 上限に
    /// 張り付いた状態でも、読み込みの途中で行が消えて失効しない。`asOfId`
    /// なし（世代の最初・再読み込み）では従来どおり剪定する。
    #[tokio::test]
    async fn audit_log_list_with_as_of_id_does_not_prune() {
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
        // 保持件数の上限を 2 にする（`PUT` も 1 行記録する = 6 行、上限超過）。
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

        let count = || async {
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM audit_log")
                .fetch_one(&env.pool)
                .await
                .unwrap()
        };
        let before = count().await;
        assert_eq!(before, 6);
        let max_id: i64 = sqlx::query_scalar("SELECT MAX(id) FROM audit_log")
            .fetch_one(&env.pool)
            .await
            .unwrap();

        let list = |uri: String| {
            HttpRequest::builder()
                .method("POST")
                .uri(uri)
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({})).unwrap(),
                ))
                .unwrap()
        };

        // `asOfId` 付き: 上限を超えていても消さない（件数も境界の集合のまま）。
        let response = env
            .router
            .clone()
            .oneshot(list(format!("/api/audit-log/list?asOfId={max_id}")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["totalCount"], 6, "{body}");
        assert_eq!(count().await, before, "asOfId 付きの取得で剪定された");

        // `asOfId` なし: 従来どおり剪定する。
        let response = env
            .router
            .clone()
            .oneshot(list("/api/audit-log/list".to_string()))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(count().await, 2);
    }

    /// `?asOfId=`（#428）: 省略すると従来どおり全行 + 応答の `asOfId` に最大
    /// `id`、指定するとその境界より後の行は件数にも行にも入らない。床は
    /// `admin` のまま（クエリを付けても editor/viewer は 403）。
    #[tokio::test]
    async fn audit_log_list_as_of_id_pins_the_snapshot() {
        let env = test_env().await;
        let audit = AuditLogService::new(Db::Sqlite(env.pool.clone()));
        let entry = |action: &'static str| AuditEntry {
            actor_username: Some("admin"),
            actor_role: Some("admin"),
            action,
            resource: "items",
            entity_id: None,
            detail: None,
            origin: "rest",
            result: "ok",
        };
        for action in ["a", "b", "c"] {
            audit.try_record(entry(action)).await.unwrap();
        }

        let list = |uri: String, token: String| {
            HttpRequest::builder()
                .method("POST")
                .uri(uri)
                .header("Authorization", format!("Bearer {token}"))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({})).unwrap(),
                ))
                .unwrap()
        };
        let call = |request: HttpRequest<Body>| {
            let router = env.router.clone();
            async move {
                let response = router.oneshot(request).await.unwrap();
                let status = response.status();
                let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                    .await
                    .unwrap();
                (
                    status,
                    serde_json::from_slice::<serde_json::Value>(&bytes)
                        .unwrap_or(serde_json::Value::Null),
                )
            }
        };

        let (status, first) = call(list(
            "/api/audit-log/list".to_string(),
            env.admin_token.clone(),
        ))
        .await;
        assert_eq!(status, StatusCode::OK, "{first}");
        let as_of_id = first["asOfId"].as_i64().expect("asOfId が返ること");
        let first_total = first["totalCount"].as_u64().unwrap();
        let max_id = first["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_i64().unwrap())
            .max()
            .unwrap();
        assert_eq!(as_of_id, max_id, "省略時の境界は最大 id: {first}");
        assert_eq!(first["rows"].as_array().unwrap().len() as u64, first_total);

        audit.try_record(entry("after")).await.unwrap();

        let (status, pinned) = call(list(
            format!("/api/audit-log/list?asOfId={as_of_id}"),
            env.admin_token.clone(),
        ))
        .await;
        assert_eq!(status, StatusCode::OK, "{pinned}");
        assert_eq!(pinned["asOfId"], as_of_id);
        assert_eq!(pinned["totalCount"].as_u64().unwrap(), first_total);
        assert!(
            pinned["rows"]
                .as_array()
                .unwrap()
                .iter()
                .all(|r| r["action"] != "after"),
            "境界より後の行が混ざった: {pinned}"
        );

        let (_, fresh) = call(list(
            "/api/audit-log/list".to_string(),
            env.admin_token.clone(),
        ))
        .await;
        assert_eq!(fresh["totalCount"].as_u64().unwrap(), first_total + 1);
        assert!(fresh["asOfId"].as_i64().unwrap() > as_of_id);

        // 床は admin のまま: editor（ここで作ってログイン）と viewer は 403。
        UsersService::new(Db::Sqlite(env.pool.clone()))
            .create_user("editor1", "password123", "編集者", Role::Editor)
            .await
            .expect("create_user editor");
        let (status, login) = call(
            HttpRequest::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(
                        &json!({ "username": "editor1", "password": "password123" }),
                    )
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{login}");
        let editor_token = login["token"].as_str().unwrap().to_string();
        for token in [editor_token, env.viewer_token.clone()] {
            let (status, body) = call(list(
                format!("/api/audit-log/list?asOfId={as_of_id}"),
                token,
            ))
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        }
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
        // 加えて、viewer の403（`require_session_role`が同じ`resource`名で
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
        SettingsService::new(Db::Sqlite(env.pool.clone()))
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

    /// ファイル DB の一時パス（テストごとに一意）。
    fn temp_db_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "banto-hub-{label}-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn remove_db_files(path: &std::path::Path) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    /// #528 の回帰テスト共通: `PUT /api/tags/{tag}` を [`TxTestHook`] の下で
    /// 始め、ハンドラがトランザクションを始めて**止まっている間**に、別接続
    /// （`busy_timeout = 0`）から書き込みを試みる。
    ///
    /// - `BEGIN IMMEDIATE` で始めていれば、ハンドラが書き込みロックを握っているので
    ///   別接続の `BEGIN IMMEDIATE` は**必ず** `SQLITE_BUSY` で失敗する。
    /// - 素の `BEGIN`（DEFERRED）だと、フックの読み取りはスナップショットを張るだけで
    ///   ロックを握らないので、別接続の書き込みは成功してしまう（テストはここで落ちる。
    ///   通った場合の後続は、スナップショットが古くなって `UPDATE` が
    ///   `SQLITE_BUSY_SNAPSHOT`（500）になる実際の失敗の再現でもある）。
    ///
    /// 「待った時間」ではなく「ロックを握っている」という積極的な証拠で判定するので、
    /// ハンドラの開始が遅れても結果は変わらない。止めたハンドラを解放して応答を返す。
    async fn put_tag_while_probing_the_write_lock(
        env: &TestEnv,
        path: &std::path::Path,
        tag: i64,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let hook = Arc::new(TxTestHook {
            reached: tokio::sync::Notify::new(),
            proceed: tokio::sync::Notify::new(),
        });
        let router = env.router.clone();
        let token = env.admin_token.clone();
        let request = tokio::spawn(TX_TEST_HOOK.scope(hook.clone(), async move {
            admin_put(&router, &format!("/api/tags/{tag}"), &token, body).await
        }));

        // 到達しなければテストの失敗（時間切れは成功の根拠にしない）。
        tokio::time::timeout(std::time::Duration::from_secs(60), hook.reached.notified())
            .await
            .expect("ハンドラがトランザクションを始めてフックへ着くはず");

        // ハンドラは止まっている。別接続（待たない設定）から書き込みを試みる。
        use sqlx::ConnectOptions;
        let mut probe = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(path)
            .busy_timeout(std::time::Duration::ZERO)
            .connect()
            .await
            .expect("connect probe");
        let attempt = sqlx::query("BEGIN IMMEDIATE").execute(&mut probe).await;
        match attempt {
            Err(sqlx::Error::Database(err)) => {
                assert!(
                    matches!(err.code().as_deref(), Some("5") | Some("261")),
                    "SQLITE_BUSY のはず: {err}"
                );
            }
            other => panic!(
                "ハンドラが書き込みロックを握っているはず（BEGIN IMMEDIATE で始めていれば \
                 別接続の BEGIN IMMEDIATE は BUSY になる）: {other:?}"
            ),
        }
        drop(probe);

        hook.proceed.notify_one();
        request.await.expect("join")
    }

    /// #528: 書き込みロックを握ったまま検証する（`BEGIN IMMEDIATE`）ので
    /// `PUT /api/tags/{id}` は 200 になる。素の `BEGIN` だと別接続が割り込めて
    /// `database is locked`（REST 500）になりうる。
    #[tokio::test]
    async fn tags_update_holds_the_write_lock_from_the_start() {
        let path = temp_db_path("tags-update-busy");
        let env = test_env_on_file_db(&path).await;
        let (tag1, group_id) =
            create_tag_via_admin(&env.router, &env.admin_token, "imm1", None).await;

        let (status, body) = put_tag_while_probing_the_write_lock(
            &env,
            &path,
            tag1,
            json!({
                "name": "tag-imm1-renamed",
                "collectionGroupId": group_id,
                "address": "D100",
                "dataType": "i16"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(body["name"], "tag-imm1-renamed");

        env.pool.close().await;
        remove_db_files(&path);
    }

    /// #528: 同じ状況でも、本物の版の食い違いは 500 ではなく 409
    /// （`tag_revision_conflict`）で返る。
    #[tokio::test]
    async fn tags_update_true_revision_conflict_is_409_while_holding_the_write_lock() {
        let path = temp_db_path("tags-update-conflict");
        let env = test_env_on_file_db(&path).await;
        let (tag1, group_id) =
            create_tag_via_admin(&env.router, &env.admin_token, "cf1", None).await;
        let (_, current) =
            admin_get(&env.router, &format!("/api/tags/{tag1}"), &env.admin_token).await;
        let stale = current["revision"].as_i64().unwrap() + 100;

        let (status, body) = put_tag_while_probing_the_write_lock(
            &env,
            &path,
            tag1,
            json!({
                "name": "tag-cf1-renamed",
                "collectionGroupId": group_id,
                "address": "D100",
                "dataType": "i16",
                "expectedRevision": stale
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body:?}");
        assert_eq!(body["error"], "tag_revision_conflict", "{body:?}");

        env.pool.close().await;
        remove_db_files(&path);
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

    // --- 試運転モードとロックダウン (設計 §5.6・2026-08-30 オーナー決定、
    // banto v3.0.0 ADR-0017 で grant 方式に、2026-10-04) --------------------

    /// loopback の peer。`tower::oneshot` には `ConnectInfo` が無いので、本番の
    /// `banto_server::start` の経路と同じものを extensions に明示する（banto の
    /// `rest/tests.rs` と同じ手筋）。
    const LOOPBACK_PEER: std::net::SocketAddr =
        std::net::SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 40000);
    const LAN_PEER: std::net::SocketAddr = std::net::SocketAddr::new(
        std::net::IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 20)),
        40000,
    );

    /// `POST /api/auth/grant/commissioning` を `peer` から叩く。
    async fn grant_request(
        router: &Router,
        peer: Option<std::net::SocketAddr>,
    ) -> (StatusCode, serde_json::Value) {
        let mut request = HttpRequest::post("/api/auth/grant/commissioning")
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .body(Body::empty())
            .unwrap();
        if let Some(peer) = peer {
            request
                .extensions_mut()
                .insert(axum::extract::ConnectInfo(peer));
        }
        let response = router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value =
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    /// 試運転モードの loopback の呼び出しが受け取る grant トークン。
    async fn commissioning_grant(router: &Router) -> String {
        let (status, body) = grant_request(router, Some(LOOPBACK_PEER)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["success"], true, "{body}");
        body["token"].as_str().expect("grant token").to_string()
    }

    /// `GET /api/auth/status` を `peer` から叩いたときの `grants.commissioning`。
    async fn commissioning_availability(
        router: &Router,
        peer: Option<std::net::SocketAddr>,
    ) -> serde_json::Value {
        let mut request = HttpRequest::get("/api/auth/status")
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .body(Body::empty())
            .unwrap();
        if let Some(peer) = peer {
            request
                .extensions_mut()
                .insert(axum::extract::ConnectInfo(peer));
        }
        let response = router.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        body["grants"]["commissioning"].clone()
    }

    /// `Authorization` 無しで管理 REST を叩いたときの status。
    async fn unauthenticated_status(router: &Router, path: &str) -> StatusCode {
        router
            .clone()
            .oneshot(
                HttpRequest::get(path)
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    /// 試運転モード（未ロックダウン）でも、管理 REST は `Authorization` 無しでは
    /// 401（banto v3.0.0 追従で「無認証で通す」分岐は無くなった）。代わりに
    /// loopback の peer は `POST /api/auth/grant/commissioning` で合成 admin の
    /// grant を受け取り、それで `GET /api/users`（`RoleGuard` で admin 限定）が
    /// 通る。`GET /api/auth/identity` はその identity を `kind: "commissioning"`
    /// 付きで返す（画面の `SessionKind` の由来）。
    #[tokio::test]
    async fn commissioning_mode_requires_the_grant_instead_of_passing_everyone() {
        let env = test_env_unlocked().await;
        assert_eq!(
            unauthenticated_status(&env.router, "/api/users").await,
            StatusCode::UNAUTHORIZED,
            "no unauthenticated admin request passes, even while commissioning"
        );

        let token = commissioning_grant(&env.router).await;
        let (status, users) = admin_get(&env.router, "/api/users", &token).await;
        assert_eq!(status, StatusCode::OK, "{users:?}");

        let (status, identity) = admin_get(&env.router, "/api/auth/identity", &token).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(identity["kind"], "commissioning", "{identity}");
        assert_eq!(identity["id"], crate::commissioning::SYNTHETIC_ACTOR_ID);
        assert_eq!(identity["role"], "admin");
    }

    /// 発行の条件（ADR-0017 §2、`commissioning_grant_spec`）: loopback の peer
    /// だけ。LAN の peer・peer 不明は 403（fail closed）。`GET /api/auth/status`
    /// の `grants.commissioning` も同じ判定。ロックダウン済みなら loopback でも
    /// 403 で、status も false。
    #[tokio::test]
    async fn the_commissioning_grant_is_loopback_only_and_closed_once_locked_down() {
        let env = test_env_unlocked().await;
        assert_eq!(
            commissioning_availability(&env.router, Some(LOOPBACK_PEER)).await,
            true
        );
        assert_eq!(
            commissioning_availability(&env.router, Some(LAN_PEER)).await,
            false
        );
        assert_eq!(commissioning_availability(&env.router, None).await, false);
        let (status, _) = grant_request(&env.router, Some(LAN_PEER)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = grant_request(&env.router, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        // 登録されていない kind は 404（閲覧公開はこの Hub に無い）。
        let response = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post("/api/auth/grant/publicViewer")
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let locked = test_env().await;
        assert_eq!(
            commissioning_availability(&locked.router, Some(LOOPBACK_PEER)).await,
            false
        );
        let (status, _) = grant_request(&locked.router, Some(LOOPBACK_PEER)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    /// 対照実験: ロックダウン済み（既存の共有テスト環境 `test_env()`）では
    /// 従来どおり `Authorization` ヘッダ無しの管理 API アクセスが 401 になる
    /// ことを明示的に固定する。
    #[tokio::test]
    async fn locked_down_admin_api_requires_a_token() {
        let env = test_env().await;
        assert_eq!(
            unauthenticated_status(&env.router, "/api/users").await,
            StatusCode::UNAUTHORIZED
        );
    }

    /// `GET /api/commissioning/status` は試運転モード中でも
    /// `Authorization` ヘッダ無しで読める必要がある（bootstrap と画面が、
    /// grant を取る前に状態を知るため）。
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
    /// `Authorization` ヘッダ無しで読める。
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
    /// 経路）を試運転の grant 自身で叩く。成功すると状態がロックダウン済みへ
    /// 切り替わり、`CommissioningService::lock_down` が保存の直後に試運転 grant
    /// のトークンを失効させる（ADR-0017 §5）ので、**同じ grant での次の要求は
    /// 401**、`GET /api/auth/identity` は `200 null`。監査には actor
    /// `commissioning`（ロックダウンの前に確定した）と `revokedGrants` が残る。
    #[tokio::test]
    async fn commissioning_lock_down_flips_state_and_revokes_the_grant() {
        let env = test_env_unlocked().await;
        let token = commissioning_grant(&env.router).await;
        let other = commissioning_grant(&env.router).await;

        let (status, body) = admin_post(
            &env.router,
            "/api/commissioning/lock-down",
            &token,
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["lockedDown"], true);

        for revoked in [&token, &other] {
            let (status, _) = admin_get(&env.router, "/api/users", revoked).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
            let (status, identity) = admin_get(&env.router, "/api/auth/identity", revoked).await;
            assert_eq!(status, StatusCode::OK);
            assert!(identity.is_null(), "{identity}");
        }
        // アカウントのセッションは巻き込まない。
        let (status, _) = admin_get(&env.router, "/api/users", &env.admin_token).await;
        assert_eq!(status, StatusCode::OK);
        // 以後 grant は出ない。
        let (status, _) = grant_request(&env.router, Some(LOOPBACK_PEER)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        let (actor, detail): (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT actor_username, detail FROM audit_log WHERE action = 'lock_down' ORDER BY id DESC LIMIT 1",
        )
        .fetch_one(&env.pool)
        .await
        .unwrap();
        assert_eq!(
            actor.as_deref(),
            Some(crate::commissioning::SYNTHETIC_ACTOR_ID)
        );
        let detail: serde_json::Value = serde_json::from_str(&detail.expect("detail")).unwrap();
        assert_eq!(detail["revokedGrants"], 2, "{detail}");
    }

    /// 実装指示「管理者0件でロックダウンしようとするとエラー」の REST
    /// 経由での確認。既存の唯一の admin アカウントを editor へ降格させた
    /// 上で（生の SQL 経由 - `UsersService::update_user`自身の「最後の
    /// admin を降格できない」ガードを迂回する必要がある）、ロックダウンを
    /// 試みると明確なエラー（4xx、`ApiError` 形）になり、状態も試運転モードの
    /// まま変わらず、**grant も失効しない**（失敗の経路でフラグも失効も
    /// 動かさない）ことを確認する。
    #[tokio::test]
    async fn commissioning_lock_down_fails_without_any_admin_account() {
        let env = test_env_unlocked().await;
        sqlx::query("UPDATE users SET role = 'editor' WHERE username = 'admin'")
            .execute(&env.pool)
            .await
            .expect("downgrade the only admin via raw SQL");
        let token = commissioning_grant(&env.router).await;

        let (status, _) = admin_post(
            &env.router,
            "/api/commissioning/lock-down",
            &token,
            json!({}),
        )
        .await;
        assert!(
            status.is_client_error(),
            "expected a 4xx error, got {status}"
        );

        let response = env
            .router
            .clone()
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
        let (status, _) = admin_get(&env.router, "/api/users", &token).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "the grant survives a failed lock-down"
        );
    }

    /// ADR-0017 §3: 試運転の grant セッション（行を持たない）でもユーザーを削除
    /// できる（自己削除ガードの行 id 照合だけが外れる）。admin の床と「最後の
    /// admin は消せない」はそのまま。
    #[tokio::test]
    async fn the_commissioning_grant_can_delete_users_but_not_the_last_admin() {
        let env = test_env_unlocked().await;
        let token = commissioning_grant(&env.router).await;
        let (status, users) = admin_get(&env.router, "/api/users", &token).await;
        assert_eq!(status, StatusCode::OK);
        let id_of = |name: &str| {
            users
                .as_array()
                .unwrap()
                .iter()
                .find(|u| u["username"] == name)
                .map(|u| u["id"].as_i64().unwrap())
                .unwrap()
        };
        let viewer = id_of("viewer1");
        let admin = id_of("admin");

        // 最後の admin のガードは `BantoError::Other`（従来どおり 500 の形）。
        let (status, body) =
            admin_delete(&env.router, &format!("/api/users/{admin}"), &token).await;
        assert_ne!(
            status,
            StatusCode::NO_CONTENT,
            "the last admin stays: {body}"
        );
        assert!(!status.is_success(), "{status} {body}");
        let (_, after) = admin_get(&env.router, "/api/users", &token).await;
        assert!(
            after
                .as_array()
                .unwrap()
                .iter()
                .any(|u| u["username"] == "admin"),
            "{after}"
        );
        let (status, _) = admin_delete(&env.router, &format!("/api/users/{viewer}"), &token).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    /// 競合の統合テスト（ADR-0017 §2「発行と失効の直列化」、移行 PR の必須項目）:
    /// `enabled()` が true を返した発行要求を**挿入の直前で保留** → ロックダウン
    /// （保存 → `revoke_grant_tokens`）が完了 → 発行要求を再開 → **403**。直後の
    /// `GET /api/auth/identity`（先に発行されていたトークン）は `200 null`、
    /// 認証が必要な `GET /api/users` は 401、status の `grants.commissioning` は
    /// false。保留は `GrantSpec.enabled` を包んで「判定の結果を返す前に待つ」
    /// ことで作る（判定と挿入のあいだに失効が割り込む形）。開いていたストリームが
    /// 再検証で閉じることは `tests/stream.rs` の
    /// `a_commissioning_grant_stream_closes_within_an_interval_after_lock_down`
    /// （実 TCP が要る）が同じ失効で固定する。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_issuance_parked_before_the_insert_loses_to_a_lock_down_that_completed_meanwhile() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let pool = migrate_memory().await.expect("migrate_memory");
        let users = UsersService::new(Db::Sqlite(pool.clone()));
        let audit = AuditLogService::new(Db::Sqlite(pool.clone()));
        users
            .setup_first_user("admin", "password123", "管理者")
            .await
            .expect("setup_first_user");
        let auth = user_auth_state(users.clone(), audit.clone());
        let settings = SettingsService::new(Db::Sqlite(pool.clone()));
        let commissioning = CommissioningService::load(settings, users.clone(), auth.clone())
            .await
            .expect("CommissioningService::load");

        // 本物の試運転 grant の仕様を包む: `park` が立っている間、判定（true）を
        // 返す前に `judged` を鳴らして `release` を待つ。
        let park = Arc::new(AtomicBool::new(false));
        let judged = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let mut spec = commissioning.grant_spec();
        let inner = spec.enabled.clone();
        spec.enabled = {
            let park = park.clone();
            let judged = judged.clone();
            let release = release.clone();
            Arc::new(move || {
                let judgment = inner();
                let park = park.clone();
                let judged = judged.clone();
                let release = release.clone();
                Box::pin(async move {
                    let verdict = judgment.await;
                    if park.load(Ordering::SeqCst) {
                        judged.notify_one();
                        release.notified().await;
                    }
                    verdict
                })
            })
        };
        let mut registry = GrantRegistry::new();
        registry.register(spec).expect("register");
        let router = extra_auth_router(
            users.clone(),
            auth.clone(),
            audit.clone(),
            false,
            Arc::new(registry),
            None,
        )
        .merge(auth_routes(auth.clone()))
        .merge(users_router(users, audit, auth.clone()));

        // 先に 1 つ発行しておく（ロックダウンで失効する対象）。
        let earlier = commissioning_grant(&router).await;
        assert!((spec_enabled_probe(&router).await), "still commissioning");

        // 保留する発行要求 B。
        park.store(true, Ordering::SeqCst);
        let parked_router = router.clone();
        let parked =
            tokio::spawn(async move { grant_request(&parked_router, Some(LOOPBACK_PEER)).await });
        judged.notified().await; // B は true を取得し、挿入の直前で止まっている。

        // ロックダウン完了（保存 → 失効）。
        let revoked = commissioning.lock_down().await.expect("lock_down");
        assert_eq!(revoked, 1);
        park.store(false, Ordering::SeqCst);
        release.notify_one();

        let (status, body) = parked.await.unwrap();
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "the parked issuance must not insert after the revocation: {body}"
        );
        assert!(body["token"].is_null(), "{body}");
        assert!(auth.identity_for(&earlier).is_none());

        let (status, identity) = admin_get(&router, "/api/auth/identity", &earlier).await;
        assert_eq!(status, StatusCode::OK);
        assert!(identity.is_null(), "{identity}");
        let (status, _) = admin_get(&router, "/api/users", &earlier).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(
            commissioning_availability(&router, Some(LOOPBACK_PEER)).await,
            false
        );
    }

    /// `GET /api/auth/status` の `grants.commissioning`（loopback）を bool で。
    async fn spec_enabled_probe(router: &Router) -> bool {
        commissioning_availability(router, Some(LOOPBACK_PEER)).await == true
    }

    // --- 試運転モード対応: 管理系 `/api/status`・`/api/values`・`/api/tag-catalog`
    // (設計 §5.6・2026-08-31 オーナー決定「案A」) -----------------------------
    //
    // 管理 UI（ブラウザ）が読む `/api/status`・`/api/values`・`/api/tag-catalog`
    // は管理系ルーター（`admin_status_router`）にあり、`/api/v1/*`
    // （`require_tag_space_auth`、API キー or セッション）とは別。banto v3.0.0
    // 追従で、試運転中もこれらは grant トークンで読む（トークン無しは 401）。

    /// 試運転モード中: grant トークンで `/api/status`・`/api/values`・
    /// `/api/tag-catalog` が読め、トークン無しは 401。
    #[tokio::test]
    async fn commissioning_grant_reads_admin_status_values_and_catalog() {
        let env = test_env_unlocked().await;
        let token = commissioning_grant(&env.router).await;
        for path in ["/api/status", "/api/values", "/api/tag-catalog"] {
            assert_eq!(
                unauthenticated_status(&env.router, path).await,
                StatusCode::UNAUTHORIZED,
                "{path}"
            );
            let (status, body) = admin_get(&env.router, path, &token).await;
            assert_eq!(status, StatusCode::OK, "{path}: {body}");
        }
    }

    /// 対照実験: ロックダウン済みでは、他の管理系ルーターと同様
    /// `Authorization` ヘッダ無しの `/api/status`・`/api/values`・
    /// `/api/tag-catalog` は 401（`locked_down_admin_api_requires_a_token`と同型）。
    #[tokio::test]
    async fn locked_down_admin_status_values_and_catalog_require_a_token() {
        let env = test_env().await;
        for path in ["/api/status", "/api/values", "/api/tag-catalog"] {
            assert_eq!(
                unauthenticated_status(&env.router, path).await,
                StatusCode::UNAUTHORIZED,
                "{path}"
            );
            let (status, _) = admin_get(&env.router, path, &env.viewer_token).await;
            assert_eq!(status, StatusCode::OK, "{path}: read-only, any role");
        }
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

    /// One session per surface: the admin UI (`require_session`)
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
        let users = UsersService::new(Db::Sqlite(pool.clone()));
        let audit = AuditLogService::new(Db::Sqlite(pool.clone()));
        let auth = user_auth_state(users.clone(), audit.clone());
        let router = extra_auth_router(
            users,
            auth.clone(),
            audit,
            true,
            Arc::new(GrantRegistry::new()),
            None,
        );
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

    // --- I3': banto のルーターに替えて入ったもの（banto #277/#278/#248） ---
    //
    // 本番と同じ合成（`api_router`）・本番と同じ `user_auth_state`（監査
    // つきの検証関数とアカウント照合）の上で、banto v2.1.0 の修正が
    // banto-hub の REST で効いていることを固定する。以前の写し
    // （`crate::users`・`crate::rest` の自前のルーター）ではどれも落ちる。

    /// 本番と同じ `user_auth_state` で組んだ router。アカウントは作らない
    /// （初回セットアップの前の状態）。試運転のまま（ロックダウンしない）。
    async fn banto_wiring_env() -> (Router, sqlx::SqlitePool, UsersService, tempfile::TempDir) {
        let pool = migrate_memory().await.expect("migrate_memory");
        let (tx, _rx) = tokio_broadcast::channel(16);
        let users = UsersService::new(Db::Sqlite(pool.clone()));
        let audit = AuditLogService::new(Db::Sqlite(pool.clone()));
        let api_keys = ApiKeysService::new(pool.clone());
        let (manager, dir) = test_manager_with_clock(pool.clone(), Arc::new(SystemClock));
        let auth = user_auth_state(users.clone(), audit.clone());
        let write_control = Arc::new(crate::write_control::WriteControl::new(false));
        let write_audit = crate::write_audit::WriteAuditService::new(pool.clone());
        let mqtt = Arc::new(crate::mqtt::MqttPublisher::new(manager.clone()));
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
        let commissioning = CommissioningService::load(
            SettingsService::new(Db::Sqlite(pool.clone())),
            users.clone(),
            auth.clone(),
        )
        .await
        .expect("CommissioningService::load");
        let router = api_router(
            users.clone(),
            audit,
            PlcConnectionService::new(pool.clone()),
            CollectionGroupService::new(pool.clone()),
            TagService::new(pool.clone()),
            api_keys,
            manager,
            auth,
            commissioning,
            tx,
            true,
            write_control,
            write_audit,
            mqtt,
            grpc_server,
            rate_limiter,
            crate::profile_paths::DEFAULT_PROFILE_ID.to_string(),
        );
        (router, pool, users, dir)
    }

    async fn audit_rows(pool: &sqlx::SqlitePool, action: &str) -> Vec<Option<String>> {
        sqlx::query_scalar("SELECT actor_username FROM audit_log WHERE action = ? ORDER BY id")
            .bind(action)
            .fetch_all(pool)
            .await
            .unwrap()
    }

    /// banto #277: 初回セットアップの「表が空か」と INSERT が 1 文。同時に
    /// 来たセットアップは 1 つだけ成功し、アカウントも `setup` の監査も 1 件。
    /// 以前の写しは「空か確かめる → ハッシュ → INSERT」の 3 段で、全員が空を
    /// 見て admin が複数できた。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_first_setups_create_exactly_one_account() {
        let (router, pool, _users, _dir) = banto_wiring_env().await;
        const ATTEMPTS: usize = 8;
        let barrier = Arc::new(tokio::sync::Barrier::new(ATTEMPTS));
        let mut handles = Vec::new();
        for i in 0..ATTEMPTS {
            let router = router.clone();
            let barrier = barrier.clone();
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                session_call(
                    &router,
                    session_request(
                        "POST",
                        "/api/auth/setup",
                        None,
                        Some(json!({
                            "username": format!("owner{i}"),
                            "password": "password123",
                            "displayName": format!("Owner {i}"),
                        })),
                    ),
                )
                .await
            }));
        }
        let mut succeeded = 0;
        for handle in handles {
            let (status, body) = handle.await.unwrap();
            assert_eq!(status, StatusCode::OK, "{body}");
            if body["success"] == true {
                assert!(body["token"].is_string(), "{body}");
                succeeded += 1;
            } else {
                assert_eq!(body["success"], false, "{body}");
                assert!(body["token"].is_null(), "{body}");
            }
        }
        assert_eq!(succeeded, 1, "exactly one setup wins");
        let accounts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(accounts, 1);
        assert_eq!(audit_rows(&pool, "setup").await.len(), 1);
    }

    /// banto #278: 失敗ログインの監査に残す名前は 32 文字（末尾 `…`）に
    /// 切り詰める。名前は攻撃者が決められ、実在のアカウントである必要も
    /// ないので、そのまま残すと監査ログを膨らませる口になる。以前の写しは
    /// 入力をそのまま記録していた。
    #[tokio::test]
    async fn a_failed_login_records_a_bounded_username() {
        let (router, pool, users, _dir) = banto_wiring_env().await;
        users
            .setup_first_user("admin", "password123", "管理者")
            .await
            .unwrap();
        let long_name = "x".repeat(4096);
        let (status, body) = session_call(
            &router,
            session_request(
                "POST",
                "/api/auth/login",
                None,
                Some(json!({ "username": long_name, "password": "wrong-password" })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["success"], false, "{body}");

        let recorded = audit_rows(&pool, "login_failed").await;
        assert_eq!(recorded.len(), 1);
        let recorded = recorded[0].clone().expect("an actor username");
        assert_eq!(recorded.chars().count(), 32, "{recorded}");
        assert_eq!(recorded, format!("{}…", "x".repeat(31)));

        // 32 文字以内の名前は（実在しなくても）そのまま残る。
        let (_, body) = session_call(
            &router,
            session_request(
                "POST",
                "/api/auth/login",
                None,
                Some(json!({ "username": "nobody", "password": "wrong-password" })),
            ),
        )
        .await;
        assert_eq!(body["success"], false, "{body}");
        assert_eq!(
            audit_rows(&pool, "login_failed")
                .await
                .last()
                .cloned()
                .flatten(),
            Some("nobody".to_string())
        );
    }

    /// banto #278: ログアウトの監査は、実際にセッションを終えたときだけ。
    /// トークン無し・知らないトークン・終えた後のトークンのログアウトは資格
    /// 情報なしで何度でも叩けるので、記録すると監査ログを膨らませる口になる。
    /// 以前の写しは毎回（actor なしで）1 行残していた。
    #[tokio::test]
    async fn only_a_logout_that_ends_a_session_is_audited() {
        let (router, pool, users, _dir) = banto_wiring_env().await;
        users
            .setup_first_user("admin", "password123", "管理者")
            .await
            .unwrap();
        let token = session_login(&router, "admin", false).await;

        let logout = |token: Option<String>| {
            let router = router.clone();
            async move {
                session_call(
                    &router,
                    session_request("POST", "/api/auth/logout", token.as_deref(), None),
                )
                .await
                .0
            }
        };
        assert!(logout(None).await.is_success());
        assert!(logout(Some("not-a-known-token".to_string()))
            .await
            .is_success());
        assert!(audit_rows(&pool, "logout").await.is_empty());

        assert!(logout(Some(token.clone())).await.is_success());
        assert_eq!(
            audit_rows(&pool, "logout").await,
            vec![Some("admin".to_string())]
        );

        // 終えた後の同じトークンは、もう何も終えない。
        assert!(logout(Some(token)).await.is_success());
        assert_eq!(audit_rows(&pool, "logout").await.len(), 1);
    }

    /// #248 の受け皿: 監査ログ一覧の応答に `deletionEpoch`（保持の剪定が
    /// 行を消すたびに進む数）が乗る。画面（PR2）はこれと件数を世代の最初の
    /// 応答と比べて、途中で削除が入ったことを知る。以前の写しの応答には
    /// 無かった。
    #[tokio::test]
    async fn the_audit_log_list_carries_the_deletion_epoch() {
        let env = test_env().await;
        let list = |as_of: Option<i64>| {
            let path = match as_of {
                Some(id) => format!("/api/audit-log/list?asOfId={id}"),
                None => "/api/audit-log/list".to_string(),
            };
            session_request("POST", &path, Some(&env.admin_token), Some(json!({})))
        };
        let (status, body) = session_call(&env.router, list(None)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["deletionEpoch"], 0, "{body}");
        let as_of = body["asOfId"].as_i64().expect("asOfId");

        // 監査を 2 行にしてから（viewer の拒否 + 設定の保存）保持件数を 1 に
        // し、境界なしの一覧（剪定する）で古い 1 行を消す。
        let (status, _) = session_call(
            &env.router,
            session_request(
                "GET",
                "/api/audit-log/config",
                Some(&env.viewer_token),
                None,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = session_call(
            &env.router,
            session_request(
                "PUT",
                "/api/audit-log/config",
                Some(&env.admin_token),
                Some(json!({ "retentionDays": null, "retentionRows": 1 })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, body) = session_call(&env.router, list(None)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["deletionEpoch"], 1, "{body}");

        // 境界つきの一覧（世代の 2 ブロック目以降）も同じ数を返す。
        let (status, body) = session_call(&env.router, list(Some(as_of))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["deletionEpoch"], 1, "{body}");

        // 読めない `asOfId` は JSON の 400（axum 既定の平文ではない）。
        let (status, body) = session_call(
            &env.router,
            session_request(
                "POST",
                "/api/audit-log/list?asOfId=abc",
                Some(&env.admin_token),
                Some(json!({})),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["kind"], "bad_request", "{body}");
    }

    // --- #383 段階3 P2: banto-hub は `hub` 接続を作らない ------------------

    /// `reject_hub_connection_protocol`: REST の作成も、既存の接続を `hub` へ
    /// 変える更新も、`protocol` の項目エラーで拒否し、何も書かない。
    #[tokio::test]
    async fn plc_connections_create_and_update_reject_the_hub_protocol() {
        let env = test_env().await;

        let (status, body) = admin_post(
            &env.router,
            "/api/plc-connections",
            &env.admin_token,
            json!({ "name": "hub", "protocol": "hub", "host": "", "port": 0 }),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
        assert_eq!(body["field_errors"][0]["field"], "protocol", "{body:?}");
        assert!(
            body["field_errors"][0]["message"]
                .as_str()
                .unwrap()
                .contains("Hub 経由の接続"),
            "{body:?}"
        );
        let rows: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM plc_connections WHERE protocol = 'hub'")
                .fetch_one(&env.pool)
                .await
                .unwrap();
        assert_eq!(rows, 0);

        let (status, conn) = admin_post(
            &env.router,
            "/api/plc-connections",
            &env.admin_token,
            json!({ "name": "line-hub-switch", "host": "127.0.0.1", "port": 15071 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{conn:?}");
        let id = conn["id"].as_i64().unwrap();
        let (status, body) = admin_put(
            &env.router,
            &format!("/api/plc-connections/{id}"),
            &env.admin_token,
            json!({ "name": "line-hub-switch", "protocol": "hub", "host": "", "port": 0 }),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
        assert_eq!(body["field_errors"][0]["field"], "protocol", "{body:?}");
        let protocol: String =
            sqlx::query_scalar("SELECT protocol FROM plc_connections WHERE id = ?")
                .bind(id)
                .fetch_one(&env.pool)
                .await
                .unwrap();
        assert_eq!(protocol, "modbus-tcp");
    }

    /// 未適用キューに `hub` の作成が残っていても（REST/MCP はキューへ積む前に
    /// 拒否するので、今の版では積まれない）、適用時にもう一度拒否して
    /// `failed` にし、理由に `protocol` を残す。
    #[tokio::test]
    async fn pending_apply_rejects_a_queued_hub_connection() {
        let env = test_env().await;
        let payload = json!({
            "input": { "name": "hub", "protocol": "hub", "host": "", "port": 0 }
        });
        let pending = PendingChangesService::new(env.pool.clone())
            .create_pending(
                "plc_connections.create",
                &payload,
                env.manager.configured_revision() as i64,
                None,
                Some("admin"),
                Some("ui"),
            )
            .await
            .unwrap();

        let response = env
            .router
            .clone()
            .oneshot(
                HttpRequest::post(format!("/api/pending-changes/{}/apply", pending.id))
                    .header("Authorization", format!("Bearer {}", env.admin_token))
                    .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let pending = PendingChangesService::new(env.pool.clone())
            .get(pending.id)
            .await
            .unwrap();
        assert_eq!(pending.state, PendingChangeState::Failed);
        let reason = pending.failure_reason.expect("failure_reason");
        assert!(reason.contains("protocol"), "{reason}");
        let rows: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM plc_connections WHERE protocol = 'hub'")
                .fetch_one(&env.pool)
                .await
                .unwrap();
        assert_eq!(rows, 0);
    }
}
