//! MQTT・store・gRPC・OPC UA の設定 API（`/api/mqtt-settings`・`/api/store-settings`・
//! `/api/grpc-settings`・`/api/opcua-settings`）。
//!
//! `rest.rs` から中身を変えずに移したもの（docs/banto-hub-driver-refactor-plan.md §8.1、
//! P0-1 の手順 1）。共通部品（認証・ガード・監査の補助）は親の `rest.rs` にあり、
//! `use super::{...}` で使う。外から使われるパス（`crate::rest::X`）は親の re-export で
//! 保つ。

use super::{
    broadcast, get, json, middleware, post, record_write, require_banto_client_header,
    require_session, require_session_role, ApiError, Arc, AsyncMutex, AuditLogService, AuthGate,
    AuthState, BantoError, CollectorManager, Db, Deserialize, FieldError, HeaderMap,
    HubSettingsExt, IpAddr, Json, LocalDate, MqttPublisher, MqttSettings, OperationKind, Role,
    Router, Serialize, ServerEvent, SessionRoleGuard, SettingsService, State, StoreSettings,
    ToSchema,
};

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
    let config = SettingsService::new(Db::Sqlite(state.manager.pool()))
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

    let settings_service = SettingsService::new(Db::Sqlite(state.manager.pool()));
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

    // 実装指示「保存で即時適用」- CollectorManager::apply_run と同じ「古い
    // タスクを止めて新しいタスク」パターン（`crate::mqtt`のモジュール doc
    // comment参照）。
    state.mqtt.apply(&config).await;

    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "update",
        "mqtt_settings",
        Some("1"),
        Some(json!({ "enabled": config.enabled })),
    )
    .await;
    let _ = state.events.send(ServerEvent::ResourceChanged {
        resource: "mqtt_settings".to_string(),
    });

    Ok(Json(config.into()))
}

pub(super) fn mqtt_settings_router(
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
            SessionRoleGuard {
                auth: auth.clone(),
                min: Role::Admin,
                resource: "mqtt_settings",
                audit,
            },
            require_session_role,
        ))
        .layer(middleware::from_fn_with_state(
            AuthGate {
                auth,
                operation: OperationKind::Normal,
            },
            require_session,
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
    let config = SettingsService::new(Db::Sqlite(state.manager.pool()))
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

    let settings_service = SettingsService::new(Db::Sqlite(state.manager.pool()));
    let existing = settings_service.store_config().await?;
    let config = StoreSettings {
        data_dir: existing.data_dir,
        retention_days: body.retention_days,
    };
    settings_service.set_store_config(&config).await?;

    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "update",
        "store_settings",
        Some("1"),
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
    let config = SettingsService::new(Db::Sqlite(state.manager.pool()))
        .store_config()
        .await?;
    let Some(retention_days) = resolve_prune_retention_days(config.retention_days) else {
        return Ok(Json(PrunePreviewResponse {
            would_delete_count: 0,
        }));
    };
    let clock = state.manager.clock();
    let today = LocalDate::from_epoch_ms(clock.now_ms(), clock.utc_offset_ms());
    // 収集中は書き込み側の判定（開いている日付は消さない）も含めて数える。
    // prune-now と同じ口を通すので、プレビューの件数と実際の件数がずれない（#541）。
    let plan = state
        .manager
        .plan_prune_data_files(retention_days, today)
        .await
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
    let config = SettingsService::new(Db::Sqlite(state.manager.pool()))
        .store_config()
        .await?;
    let deleted_count = match resolve_prune_retention_days(config.retention_days) {
        Some(retention_days) => {
            let clock = state.manager.clock();
            let today = LocalDate::from_epoch_ms(clock.now_ms(), clock.utc_offset_ms());
            // 収集中は書き込み側（`TsWriter`）を通して消す。書き手が今開いている
            // 日付のファイルは、保持日数を過ぎていても消さない（#541）。
            let report = state
                .manager
                .prune_data_files(retention_days, today)
                .await
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
        &headers,
        "delete",
        "store_settings_prune",
        Some("1"),
        Some(json!({ "deletedCount": deleted_count })),
    )
    .await;

    Ok(Json(PruneNowResponse { deleted_count }))
}

pub(super) fn store_settings_router(
    manager: Arc<CollectorManager>,
    audit: AuditLogService,
    auth: AuthState,
) -> Router {
    let state = StoreSettingsAdminState {
        manager,
        auth: auth.clone(),
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
            SessionRoleGuard {
                auth: auth.clone(),
                min: Role::Admin,
                resource: "store_settings",
                audit,
            },
            require_session_role,
        ))
        .layer(middleware::from_fn_with_state(
            AuthGate {
                auth,
                operation: OperationKind::Normal,
            },
            require_session,
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
    let config = SettingsService::new(Db::Sqlite(state.manager.pool()))
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

    let settings_service = SettingsService::new(Db::Sqlite(state.manager.pool()));
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
        Some("1"),
        Some(json!({ "enabled": config.enabled, "bind": config.bind, "port": config.port })),
    )
    .await;
    let _ = state.events.send(ServerEvent::ResourceChanged {
        resource: "grpc_settings".to_string(),
    });

    Ok(Json(config.into()))
}

pub(super) fn grpc_settings_router(
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
            SessionRoleGuard {
                auth: auth.clone(),
                min: Role::Admin,
                resource: "grpc_settings",
                audit,
            },
            require_session_role,
        ))
        .layer(middleware::from_fn_with_state(
            AuthGate {
                auth,
                operation: OperationKind::Normal,
            },
            require_session,
        ))
}

#[derive(Clone)]
struct OpcUaSettingsAdminState {
    manager: Arc<CollectorManager>,
    publisher: Arc<crate::opcua::OpcUaPublisher>,
    auth: AuthState,
    audit: AuditLogService,
    events: broadcast::Sender<ServerEvent>,
    write_lock: Arc<AsyncMutex<()>>,
}

async fn opcua_settings_get(
    State(state): State<OpcUaSettingsAdminState>,
) -> Result<Json<crate::settings::OpcUaSettings>, ApiError> {
    let settings = SettingsService::new(Db::Sqlite(state.manager.pool()));
    Ok(Json(settings.opcua_config().await?))
}

/// listener の状態（#634）。`PUT /api/opcua-settings` は保存して起動を始めたら
/// すぐ返るので、起動中・起動済み・失敗（理由つき）・停止はここで見る。
async fn opcua_status_get(
    State(state): State<OpcUaSettingsAdminState>,
) -> Json<crate::opcua::OpcUaStatus> {
    Json(state.publisher.status())
}

fn valid_opcua_host(host: &str) -> bool {
    if host.is_empty() || host != host.trim() {
        return false;
    }
    if host.parse::<IpAddr>().is_ok() {
        return true;
    }
    if let Some(ip) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        return ip.parse::<std::net::Ipv6Addr>().is_ok();
    }
    let dns = host.strip_suffix('.').unwrap_or(host);
    dns.len() <= 253
        && !dns.is_empty()
        && dns.is_ascii()
        && dns.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        })
}

fn unspecified_opcua_host(host: &str) -> bool {
    let bare = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host);
    bare.strip_suffix('.')
        .unwrap_or(bare)
        .parse::<IpAddr>()
        .is_ok_and(|ip| ip.is_unspecified())
}

pub(crate) fn validate_opcua_settings(config: &crate::settings::OpcUaSettings) -> Vec<FieldError> {
    let mut errors = Vec::new();
    let mut invalid = |field: &str, message: &str| {
        errors.push(FieldError {
            field: field.to_owned(),
            message: message.to_owned(),
        })
    };
    let bind = config.bind.parse::<IpAddr>();
    if bind.is_err() {
        invalid("bind", "IP アドレスを指定してください");
    }
    if config.port == 0 {
        invalid("port", "1〜65535 のポートを指定してください");
    }
    if config.endpoint_path.is_empty()
        || !config.endpoint_path.starts_with('/')
        || config.endpoint_path.chars().any(char::is_whitespace)
        || config.endpoint_path.contains(['?', '#'])
    {
        invalid("endpointPath", "/ で始まるパスを指定してください");
    }
    if config.namespace_uri.is_empty()
        || config.namespace_uri != config.namespace_uri.trim()
        || config.namespace_uri == "http://opcfoundation.org/UA/"
        || config.namespace_uri == "urn:banto:opcua:server"
    {
        invalid(
            "namespaceUri",
            "標準名前空間と異なる URI を指定してください",
        );
    }
    let host = config.advertised_host.as_deref().filter(|h| !h.is_empty());
    if let Some(host) = host {
        if !valid_opcua_host(host) || unspecified_opcua_host(host) {
            invalid(
                "advertisedHost",
                "ホスト名または IP アドレスのみ指定してください",
            );
        }
    }
    if bind.is_ok_and(|ip| ip.is_unspecified()) && host.is_none() {
        invalid(
            "advertisedHost",
            "全インターフェース待受には公開ホストが必要です",
        );
    }
    errors
}

async fn opcua_settings_put(
    State(state): State<OpcUaSettingsAdminState>,
    headers: HeaderMap,
    Json(mut config): Json<crate::settings::OpcUaSettings>,
) -> Result<Json<crate::settings::OpcUaSettings>, ApiError> {
    if config.advertised_host.as_deref() == Some("") {
        config.advertised_host = None;
    }
    let errors = validate_opcua_settings(&config);
    if !errors.is_empty() {
        return Err(ApiError(BantoError::Validation {
            field_errors: errors,
        }));
    }
    // クライアント切断で設定保存と listener 切り替えが途中停止しないよう切り離す。
    // #634: `apply` は古い listener を止めて起動を背景タスクへ渡したら返る
    // （起動の完了は待たない）。200 は「保存した・起動を始めた」までを意味し、
    // 起動の結果は `GET /api/opcua-status` に出る。
    let applied = tokio::spawn(async move {
        // 保存と適用を1つの順序にし、並行 PUT の古い設定が後から適用されることを
        // 防ぐ（起動どうしの順序と世代は `OpcUaPublisher` が守る）。
        let _write_guard = state.write_lock.lock().await;
        SettingsService::new(Db::Sqlite(state.manager.pool()))
            .set_opcua_config(&config)
            .await?;
        state.publisher.apply(&config).await;
        record_write(
            &state.audit,
            &state.auth,
            &headers,
            "update",
            "opcua_settings",
            Some("1"),
            Some(
                json!({"enabled": config.enabled, "bind": config.bind, "port": config.port,
                "endpointPath": config.endpoint_path, "namespaceUri": config.namespace_uri,
                "advertisedHost": config.advertised_host}),
            ),
        )
        .await;
        let _ = state.events.send(ServerEvent::ResourceChanged {
            resource: "opcua_settings".to_owned(),
        });
        Ok::<_, BantoError>(config)
    })
    .await
    .map_err(|err| ApiError(BantoError::Other(format!("OPC UA 設定タスク: {err}"))))??;
    Ok(Json(applied))
}

/// HubRuntime から共通 publisher を渡す管理系の追加ルーター。
pub(crate) fn opcua_settings_router(
    manager: Arc<CollectorManager>,
    publisher: Arc<crate::opcua::OpcUaPublisher>,
    audit: AuditLogService,
    auth: AuthState,
    events: broadcast::Sender<ServerEvent>,
) -> Router {
    let state = OpcUaSettingsAdminState {
        manager,
        publisher,
        auth: auth.clone(),
        audit: audit.clone(),
        events,
        write_lock: Arc::new(AsyncMutex::new(())),
    };
    Router::new()
        .route(
            "/api/opcua-settings",
            get(opcua_settings_get).put(opcua_settings_put),
        )
        .route("/api/opcua-status", get(opcua_status_get))
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            SessionRoleGuard {
                auth: auth.clone(),
                min: Role::Admin,
                resource: "opcua_settings",
                audit,
            },
            require_session_role,
        ))
        .layer(middleware::from_fn_with_state(
            AuthGate {
                auth,
                operation: OperationKind::Normal,
            },
            require_session,
        ))
        .layer(middleware::from_fn(require_banto_client_header))
}

#[cfg(test)]
mod opcua_settings_tests {
    use super::*;

    #[test]
    fn defaults_are_loopback_and_disabled() {
        let config = crate::settings::OpcUaSettings::default();
        assert!(!config.enabled);
        assert_eq!(config.bind, "127.0.0.1");
        assert_eq!(config.port, 4840);
        assert_eq!(config.endpoint_path, "/");
        assert!(validate_opcua_settings(&config).is_empty());
    }

    #[test]
    fn unspecified_bind_requires_a_real_advertised_host() {
        let mut config = crate::settings::OpcUaSettings {
            enabled: true,
            bind: "0.0.0.0".to_owned(),
            ..Default::default()
        };
        assert!(validate_opcua_settings(&config)
            .iter()
            .any(|e| e.field == "advertisedHost"));
        config.advertised_host = Some("opcua.example.local".to_owned());
        assert!(validate_opcua_settings(&config).is_empty());
        config.advertised_host = Some("opc.tcp://evil:4840/".to_owned());
        assert!(validate_opcua_settings(&config)
            .iter()
            .any(|e| e.field == "advertisedHost"));
        config.advertised_host = Some("0:0:0:0:0:0:0:0".to_owned());
        assert!(validate_opcua_settings(&config)
            .iter()
            .any(|e| e.field == "advertisedHost"));
    }

    #[test]
    fn rejects_invalid_port_path_and_namespace() {
        let config = crate::settings::OpcUaSettings {
            port: 0,
            endpoint_path: "relative".to_owned(),
            namespace_uri: " ".to_owned(),
            ..Default::default()
        };
        let fields: Vec<_> = validate_opcua_settings(&config)
            .into_iter()
            .map(|e| e.field)
            .collect();
        assert!(fields.contains(&"port".to_owned()));
        assert!(fields.contains(&"endpointPath".to_owned()));
        assert!(fields.contains(&"namespaceUri".to_owned()));
    }
}
