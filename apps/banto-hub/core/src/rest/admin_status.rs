//! 管理系の `GET /api/status`・`GET /api/values`・`GET /api/tag-catalog`・
//! `GET /api/tag-stream`（設計 §5.6・2026-08-31 オーナー決定「案A」）と、その並びにある `GET /api/v1/events`。
//!
//! `rest.rs` から中身を変えずに移したもの（docs/banto-hub-driver-refactor-plan.md §8.1、
//! P0-1 の手順 7）。タグ空間 API の状態型・ヘルパは `rest/tag_space.rs` にあり、
//! 認証・ガードの補助は親の `rest.rs` にある。どちらも `use super::{...}` で使う。
//! 外から使われるパス（`crate::rest::X`）は親の re-export で保つ。

use super::{
    build_catalog_response, build_values_response, compute_status, get, json, middleware,
    require_session, resolve_value_names, ApiError, Arc, AuditLogService, AuthGate, AuthState,
    CatalogResponse, CatalogTagEntry, CollectionController, CollectorManager,
    ConnectionStatusEntry, DbSourceStatusEntry, Deserialize, GrpcStatusEntry, IngestStatusEntry,
    IntoResponse, Json, LastApplyEntry, MqttPublisher, MqttStatusEntry, OperationKind, Query,
    Response, Router, Serialize, SinkStatusEntry, SinkStatusStore, State, StatusCode,
    StatusResponse, SystemInfoSampler, SystemInfoSnapshot, TagSpaceState, TagsQuery, ToSchema,
    ValueEntry, ValuesQuery, ValuesResponse, WriteControl,
};

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
// 認可レベル: `SessionRoleGuard`（admin 限定）は掛けない。理由:
// 1. `/api/v1/status`・`/api/v1/values`自体がそもそもロール制約の無い
//    読み取り専用エンドポイント（`tag_space_router`参照、`SessionRoleGuard`は
//    一切登場しない）。管理系に持ち込むだけでロールを新設するのは
//    「同じ情報を管理 UI からも読めるようにする」というこの変更の趣旨から
//    外れる。
// 2. 管理系ルーターの既存の慣行でも、書き込み系（`write-control`・
//    `collection`等）は`SessionRoleGuard{min: Role::Admin}`を掛ける一方、
//    読み取り専用の一覧系（`/api/tags`・`/api/plc-connections`・
//    `/api/pending-changes`の`GET`等、`tag_registry_router`/
//    `pending_changes_router`参照）は`require_session`のみ
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
    /// R2b-1e（docs/banto-hub-connection-settings-design.md §5.5）: パスワードを読めないので
    /// 起こしていない接続だけ `"unreadable"`、それ以外は `null`（`lastError` の文言は
    /// 「パスワード: 読めません（この PC で再入力してください）（理由: …）」）。
    password_state: Option<&'static str>,
    /// R2b-1e: 読めない理由 `"missing"`（秘密の行が無い）/ `"undecryptable"`（この PC で
    /// 開けない）/ `"invalid"`（参照の形が違う）。読めるなら `null`。
    password_unreadable_reason: Option<&'static str>,
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
            password_state: entry.password_unreadable.map(|_| "unreadable"),
            password_unreadable_reason: entry.password_unreadable.map(|reason| reason.as_str()),
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

/// [`IngestStatusEntry`]のcamelCase版（取り込み口 `push`、R2a-2e、
/// docs/banto-hub-ingest-design.md §9）。資格情報・値を含まないのは元の型と同じ。
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct AdminIngestStatusEntry {
    connection_id: i64,
    name: String,
    enabled: bool,
    bridge: bool,
    link: String,
    link_detail: Option<String>,
    last_seen_at: Option<i64>,
    stale_timeout_ms: u64,
    collecting: bool,
    epoch: u64,
    held_values: u64,
    senders: Vec<AdminIngestSenderStatusEntry>,
    accepted: u64,
    rejected: u64,
    rate_limited: u64,
    too_many_items: u64,
    unknown_keys: Vec<String>,
    last_source_time: Option<i64>,
    sender_status: Option<String>,
}

/// [`AdminIngestStatusEntry`]の送り手 1 つ。
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct AdminIngestSenderStatusEntry {
    transport: String,
    api_key_name: String,
    peer: Option<String>,
    connected_at: Option<i64>,
    last_seen_at: Option<i64>,
}

impl From<IngestStatusEntry> for AdminIngestStatusEntry {
    fn from(entry: IngestStatusEntry) -> Self {
        Self {
            connection_id: entry.connection_id,
            name: entry.name,
            enabled: entry.enabled,
            bridge: entry.bridge,
            link: entry.link,
            link_detail: entry.link_detail,
            last_seen_at: entry.last_seen_at,
            stale_timeout_ms: entry.stale_timeout_ms,
            collecting: entry.collecting,
            epoch: entry.epoch,
            held_values: entry.held_values,
            senders: entry
                .senders
                .into_iter()
                .map(|sender| AdminIngestSenderStatusEntry {
                    transport: sender.transport,
                    api_key_name: sender.api_key_name,
                    peer: sender.peer,
                    connected_at: sender.connected_at,
                    last_seen_at: sender.last_seen_at,
                })
                .collect(),
            accepted: entry.accepted,
            rejected: entry.rejected,
            rate_limited: entry.rate_limited,
            too_many_items: entry.too_many_items,
            unknown_keys: entry.unknown_keys,
            last_source_time: entry.last_source_time,
            sender_status: entry.sender_status,
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
    write_persistence_warning: Option<String>,
    audit_pending_count: u64,
    audit_pending_oldest_ts: Option<String>,
    audit_dropped_count: u64,
    audit_failed_count: u64,
    mqtt: MqttStatusEntry,
    grpc: GrpcStatusEntry,
    last_apply: Option<AdminLastApplyEntry>,
    /// 外部 DB 連携 S2: [`DbSourceStatusEntry`]のcamelCase版。
    db_source: Vec<AdminDbSourceStatusEntry>,
    /// 取り込み口 `push`（R2a-2e）: [`IngestStatusEntry`]のcamelCase版。
    ingest: Vec<AdminIngestStatusEntry>,
    /// T19 S3-b（UX-46）: [`SystemInfoSnapshot`]のcamelCase版。
    system: AdminSystemInfoEntry,
    /// 外部 DB 連携 S4: [`SinkStatusEntry`]のcamelCase版。
    sink: AdminSinkStatusEntry,
    /// R2b-1e（docs/banto-hub-connection-settings-design.md §5.8・§6.5）: 接続の秘密値の
    /// 保存の方式と警告。管理系の `GET /api/status` だけに出す（`/api/v1/status` には無い）。
    secret_storage: AdminSecretStorageEntry,
}

/// R2b-1e: [`AdminStatusResponse::secret_storage`]。秘密値・参照・blob は含まない。
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
struct AdminSecretStorageEntry {
    /// 新しく封印するときの方式: `"dpapi-machine-v1"`（Windows）/ `"plain-v1"`（それ以外）。
    scheme: &'static str,
    /// Windows 以外（`plain-v1`）のときだけ「秘密値を平文で保存しています（Windows 以外）」、
    /// Windows では `null`（設計 §5.8）。
    plaintext_warning: Option<&'static str>,
    /// Windows で封印し直せずに平文で残っている件数（`plain-v1` の秘密の行と、未適用キューの
    /// 平文・`plain-v1` のパスワード。`crate::upgrade::plaintext_remaining`）。Windows 以外は
    /// 常に 0（平文が既定なので `plaintextWarning` で出す）。
    plaintext_remaining: u64,
    /// `plaintextRemaining > 0` のときだけ「移行待ちの秘密値が平文で残っています（n 件）」、
    /// それ以外は `null`（設計 §6.5）。
    plaintext_remaining_warning: Option<String>,
}

impl AdminSecretStorageEntry {
    async fn load(state: &TagSpaceState) -> Result<Self, ApiError> {
        use crate::secrets::{
            ConnectionSecrets, SecretScheme, PLAINTEXT_REMAINING_WARNING, PLAINTEXT_STORAGE_WARNING,
        };
        let secrets = ConnectionSecrets::platform_default();
        let scheme = secrets.seal_scheme();
        let plaintext_remaining =
            crate::upgrade::plaintext_remaining(&state.manager.pool(), &secrets).await?;
        Ok(Self {
            scheme: scheme.as_str(),
            plaintext_warning: (scheme == SecretScheme::PlainV1)
                .then_some(PLAINTEXT_STORAGE_WARNING),
            plaintext_remaining,
            plaintext_remaining_warning: (plaintext_remaining > 0)
                .then(|| format!("{PLAINTEXT_REMAINING_WARNING}（{plaintext_remaining} 件）")),
        })
    }
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

impl AdminStatusResponse {
    fn new(status: StatusResponse, secret_storage: AdminSecretStorageEntry) -> Self {
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
            write_persistence_warning: status.write_persistence_warning,
            audit_pending_count: status.audit_pending_count,
            audit_pending_oldest_ts: status.audit_pending_oldest_ts,
            audit_dropped_count: status.audit_dropped_count,
            audit_failed_count: status.audit_failed_count,
            mqtt: status.mqtt,
            grpc: status.grpc,
            last_apply: status.last_apply.map(Into::into),
            db_source: status.db_source.into_iter().map(Into::into).collect(),
            ingest: status.ingest.into_iter().map(Into::into).collect(),
            system: status.system.into(),
            sink: status.sink.into(),
            secret_storage,
        }
    }
}

/// `GET /api/status` - 管理 UI 向け（試運転モードのバイパス対象、
/// このセクション冒頭のdoc comment参照）。[`compute_status`]を
/// `/api/v1/status`と共有し、camelCaseへ包み直す。R2b-1e: 管理系だけの
/// `secretStorage`（[`AdminSecretStorageEntry`]）を足す。
async fn admin_status(
    State(state): State<TagSpaceState>,
) -> Result<Json<AdminStatusResponse>, ApiError> {
    let status = compute_status(&state).await?;
    let secret_storage = AdminSecretStorageEntry::load(&state).await?;
    Ok(Json(AdminStatusResponse::new(status, secret_storage)))
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
    // #682 PR-H: 応答の `revision` は `map` と同じ lock の読みから取る。
    let (map, revision) = state.manager.catalog_with_revision();
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
        &state, &map, revision, &runtime, names,
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
/// 管理系（試運転モードのバイパスが効く側）に配置する。`SessionRoleGuard`は
/// 掛けない理由はこのセクション冒頭のdoc comment参照（読み取り専用・
/// ロール不問、`tag_registry_router`の`GET`系と同じ扱い）。状態は
/// [`TagSpaceState`]を[`tag_space_router`]とは別に組み立てる - こちらは
/// `require_session`層を被せるため、`require_tag_space_auth`
/// 層を被せる`tag_space_router`側の`Router`とは共有できない（axum の
/// `Router`は1つにつき1枚の認証`.layer`しか意味を持たないため、同じ
/// `Router`を2種類の認証で使い回すことはできない）。
///
/// **WS（`/api/tag-stream`）はここに同居させない** -
/// [`admin_tag_stream_router`]のdoc comment参照（この Router 全体は最終的に
/// `require_banto_client_header`(CSRF) レイヤーの内側に組み込まれるが、
/// ブラウザの`WebSocket`はCSRF用カスタムヘッダを送れないため）。
#[allow(clippy::too_many_arguments)]
pub(super) fn admin_status_router(
    manager: Arc<CollectorManager>,
    controller: Arc<CollectionController>,
    write_control: Arc<WriteControl>,
    mqtt: Arc<MqttPublisher>,
    system_info: Arc<SystemInfoSampler>,
    sink_status: Arc<SinkStatusStore>,
    auth: AuthState,
    audit: AuditLogService,
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
        audit,
    };
    Router::new()
        .route("/api/status", get(admin_status))
        .route("/api/values", get(admin_values))
        .route("/api/tag-catalog", get(admin_tag_catalog))
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            AuthGate {
                auth,
                operation: OperationKind::Normal,
            },
            require_session,
        ))
}

/// `GET /api/tag-stream`（管理系 WS、[`admin_tag_stream_router`]）のパス -
/// [`extract_ws_protocol_token`]の許可リストと[`admin_tag_stream_router`]の
/// 両方から参照する定数にして、パス文字列のタイプミスで両者がずれるのを
/// 防ぐ。
pub(super) const ADMIN_TAG_STREAM_PATH: &str = "/api/tag-stream";

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
/// 要求しない代わりに、認証自体は[`require_session`]で
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
/// 参照）を[`require_session`]側にも追加した - パスの許可
/// リストに[`ADMIN_TAG_STREAM_PATH`]を足しただけで、`/api/v1/stream`の
/// 認証・ルート・レスポンス形状は一切変えていない（`require_tag_space_auth`
/// は無変更）。試運転モード中（未ロックダウン）は
/// [`require_session`]がヘッダの中身を見る前に無条件で
/// 素通しするので、トークン（Sec-WebSocket-Protocolオファーそのもの）が
/// 無くても接続できる。
///
/// ## `scope`（per-tag read スコープ）は常に`None`
///
/// [`crate::stream::ws_upgrade`]は`ApiKeyContext`拡張が無ければ`scope`を
/// `None`として扱う（=購読の絞り込み無し、全アクセス）。管理系ルーターは
/// `require_session`しか通らず`ApiKeyContext`を挿入する
/// ことは無いので、この管理系WSは常に`scope = None`側の経路を通る -
/// これは「セッション bearer で`/api/v1/stream`に繋いだ場合」と全く同じ
/// 挙動（`crate::stream::handle_socket`のフィールド doc comment
/// 「`external = scope.is_some()`」参照）で、管理 UI が今まで手にしていた
/// アクセス範囲を狭めても広げてもいない。
#[allow(clippy::too_many_arguments)]
pub(super) fn admin_tag_stream_router(
    manager: Arc<CollectorManager>,
    controller: Arc<CollectionController>,
    write_control: Arc<WriteControl>,
    mqtt: Arc<MqttPublisher>,
    system_info: Arc<SystemInfoSampler>,
    sink_status: Arc<SinkStatusStore>,
    auth: AuthState,
    audit: AuditLogService,
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
        audit,
    };
    Router::new()
        .route(ADMIN_TAG_STREAM_PATH, get(crate::stream::ws_upgrade))
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            AuthGate {
                auth,
                operation: OperationKind::Normal,
            },
            require_session,
        ))
}

#[derive(Debug, Deserialize)]
pub(super) struct EventsQuery {
    from_ms: Option<i64>,
    to_ms: Option<i64>,
    limit: Option<i64>,
}

/// `GET /api/v1/events` の `events` 配列1件分
/// (`crates/banto-collect/migrations/0001_collect_events.sql`'s columns)。
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct EventEntry {
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
pub(super) struct EventsResponse {
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
pub(super) async fn v1_events(
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
