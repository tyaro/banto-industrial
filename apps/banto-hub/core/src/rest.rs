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
mod tests;
