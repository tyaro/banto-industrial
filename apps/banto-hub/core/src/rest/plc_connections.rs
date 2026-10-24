//! PLC 接続の CRUD（`/api/plc-connections`）と接続テスト（保存前の疎通確認・保存済み
//! 接続のテスト）。
//!
//! `rest.rs` から中身を変えずに移したもの（docs/banto-hub-driver-refactor-plan.md §8.1、
//! P0-1 の手順 3）。I1 CRUD の共通層は `rest/crud.rs` にあり、認証・ガード・監査の補助は
//! 親の `rest.rs` にある。どちらも `use super::{...}` で使う。外から使われるパス
//! （`crate::rest::X`）は親の re-export で保つ。

use super::{
    commit_catalog_and_notify, default_payload_enabled, json, preflight_transaction,
    queue_pending_registry_change, record_write, registry_before, registry_change_should_queue,
    reject_delete_if_referenced_by_sink_group, require_editor, storage_api_error, ApiError,
    BantoError, BrokerConnectionStatus, BrokerError, CollectorManager, Deserialize, Duration,
    FieldError, HeaderMap, IntoResponse, Json, Path, PlcConnection, RegistryMutationError,
    RegistryMutationResult, Response, Serialize, State, StatusCode, TagRegistryState,
};
use banto_driver_api::{keys, ConnectContext, FatalError, ItemError, Settings, TagSpec, Value};

/// R2b-6b（docs/banto-hub-driver-ui-design.md §5、Q2）: `/api/plc-connections` の作成・更新の
/// 形 `{ name, protocol, enabled, simulation, settings }`。平らな欄（`host`・`port`・`unitId`・
/// `password` …）は受けない（`deny_unknown_fields`。使用者無しの前提で古い形は断る、
/// 2026-10-22 オーナー）。MCP の `create_connection` / `update_connection` も同じ形を直接
/// 受ける（R2b-7a）。
///
/// - 機器のドライバ（`modbus-tcp`・`slmp`）の `settings` はドライバのスキーマ
///   （`GET /api/drivers` の `settingsSchema`）に従い、`banto_tags::settings_from_schema` →
///   `settings_to_api` → `validate_config` を通る（`banto_tags::ConnectionSettingsInput`）。
///   項目エラーの名前は JSON のキー（`unit_id`）。
/// - `postgres`・`push` の `settings` は今の中身（`postgres` は `host`・`port`・`database`・
///   `username`・`password`、`push` は R2a の `settings`）で、専用の検証は今のまま。
/// - **秘密のキー**（ドライバの `secret_keys()`。今は `postgres` の `password`）は
///   [`Self::into_input`] がここで外して封印する。tri-state（オーナー決定 2026-10-24）:
///   省略 = 保つ、`null` = 消す、**空文字 = 消す**、それ以外の文字列 = 置き換え。応答には
///   値を返さない（[`PlcConnectionResponse`]）。
/// - `settings` は丸ごと置き換え（更新で省いた欄はスキーマの既定値。秘密のキーだけ省略が
///   「保つ」）。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlcConnectionPayload {
    pub name: String,
    pub protocol: String,
    #[serde(default = "default_payload_enabled")]
    pub enabled: bool,
    /// T9-2: 省略は `false`（省略で誤ってシミュレーションにしない）。
    #[serde(default)]
    pub simulation: bool,
    #[serde(default)]
    pub settings: serde_json::Map<String, serde_json::Value>,
}

/// 秘密のキーの値の項目エラー（値は載せない）。
fn secret_field_error(protocol: &str, key: &str, message: impl Into<String>) -> BantoError {
    BantoError::Validation {
        field_errors: vec![FieldError {
            field: if protocol == banto_tags::POSTGRES_PROTOCOL {
                key.to_string()
            } else {
                format!("settings.{key}")
            },
            message: message.into(),
        }],
    }
}

impl PlcConnectionPayload {
    /// banto-tags の入力に写す。`secret_keys`（`PlcConnectionService::secret_keys(protocol)`）の
    /// 値を `settings` から外し、tri-state（型の doc）で `SecretChange` にする。値は
    /// [`crate::secrets::ConnectionSecrets::platform_default`] で**ここで封印**する
    /// （banto-tags は平文を受けない）。文字列でも `null` でもない値・封印の失敗はそのキーの
    /// 項目エラー。
    pub(crate) fn into_input(
        self,
        secret_keys: &[String],
    ) -> Result<banto_tags::ConnectionSettingsInput, BantoError> {
        self.into_input_with_sealed(secret_keys, std::collections::BTreeMap::new())
    }

    /// 未適用キューの適用（R2b-1e・R2b-6b）: 積む時点で封印した値 `sealed`（キーごと）を
    /// **平文に戻さず** `SecretChange::Set` として渡す。封印した値がこの PC で開けない・
    /// 秘密のキーでない・同じキーに `settings` の値もある、はそのキーの項目エラー。
    pub(crate) fn into_input_with_sealed(
        mut self,
        secret_keys: &[String],
        mut sealed: std::collections::BTreeMap<String, crate::secrets::SealedSecret>,
    ) -> Result<banto_tags::ConnectionSettingsInput, BantoError> {
        let secrets = crate::secrets::ConnectionSecrets::platform_default();
        let mut changes = banto_tags::SecretChanges::new();
        for key in secret_keys {
            let value = self.settings.remove(key);
            if let Some(sealed) = sealed.remove(key) {
                if value.is_some() {
                    return Err(secret_field_error(
                        &self.protocol,
                        key,
                        "未適用キューの提案に平文と封印した値の両方があります",
                    ));
                }
                if secrets.open(key, &sealed).is_err() {
                    return Err(secret_field_error(
                        &self.protocol,
                        key,
                        "未適用キューの提案の秘密値をこの PC で読めません（提案を破棄して、\
                         値を入れ直して作り直してください）",
                    ));
                }
                changes.insert(key.clone(), banto_tags::SecretChange::Set(sealed.into()));
                continue;
            }
            match value {
                None => {}
                Some(serde_json::Value::Null) => {
                    changes.insert(key.clone(), banto_tags::SecretChange::Clear);
                }
                Some(serde_json::Value::String(plain)) if plain.is_empty() => {
                    changes.insert(key.clone(), banto_tags::SecretChange::Clear);
                }
                Some(serde_json::Value::String(plain)) => {
                    let sealed = secrets.seal(key, &plain).map_err(|error| {
                        secret_field_error(&self.protocol, key, error.to_string())
                    })?;
                    changes.insert(key.clone(), banto_tags::SecretChange::Set(sealed.into()));
                }
                Some(_) => {
                    return Err(secret_field_error(
                        &self.protocol,
                        key,
                        "文字列か null で指定してください",
                    ));
                }
            }
        }
        if let Some(key) = sealed.keys().next() {
            return Err(secret_field_error(
                &self.protocol,
                key,
                format!("{} 接続では {key} を指定できません", self.protocol),
            ));
        }
        Ok(banto_tags::ConnectionSettingsInput {
            name: self.name,
            protocol: self.protocol,
            enabled: self.enabled,
            simulation: self.simulation,
            settings: self.settings,
            secrets: changes,
        })
    }

    /// 未適用キューに積む前の検査: 適用時と同じ規則（`protocol` の登録・`settings` のキー）で
    /// 断る。これが無いと、未登録の `protocol` や余計な `password` の平文が封印されずにキューに
    /// 残る（#827 のレビュー）。積む前（`queue_form` の前）に呼ぶ。
    pub(crate) fn check_before_queue(
        &self,
        service: &banto_tags::PlcConnectionService,
        secret_keys: &[String],
    ) -> Result<(), BantoError> {
        let input = self.clone().into_input(secret_keys)?;
        service.check_settings_input(&input)
    }

    /// 未適用キューに積む形（R2b-6b、`crate::pending_changes` の doc）: 秘密のキーの文字列の
    /// 値は**積む前に封印して** `sealedSecrets`（キー → `crate::secrets::sealed_password_json`
    /// の形）へ移し、`settings` から外す。空文字（消す）は `null` にする。省略（保つ）・`null`
    /// （消す）はそのまま。平文はキューに置かない。
    pub(crate) fn queue_form(
        mut self,
        secret_keys: &[String],
    ) -> Result<serde_json::Value, BantoError> {
        let secrets = crate::secrets::ConnectionSecrets::platform_default();
        let mut sealed = serde_json::Map::new();
        for key in secret_keys {
            match self.settings.get(key) {
                None | Some(serde_json::Value::Null) => {}
                Some(serde_json::Value::String(plain)) if plain.is_empty() => {
                    self.settings.insert(key.clone(), serde_json::Value::Null);
                }
                Some(serde_json::Value::String(plain)) => {
                    let value = secrets.seal(key, plain).map_err(|error| {
                        secret_field_error(&self.protocol, key, error.to_string())
                    })?;
                    self.settings.remove(key);
                    sealed.insert(key.clone(), crate::secrets::sealed_password_json(&value));
                }
                Some(_) => {
                    return Err(secret_field_error(
                        &self.protocol,
                        key,
                        "文字列か null で指定してください",
                    ));
                }
            }
        }
        let mut form = serde_json::to_value(&self)
            .map_err(|err| BantoError::Other(format!("入力のシリアライズに失敗しました: {err}")))?;
        if !sealed.is_empty() {
            if let Some(object) = form.as_object_mut() {
                object.insert(
                    crate::pending_changes::SEALED_SECRETS_FIELD.to_string(),
                    serde_json::Value::Object(sealed),
                );
            }
        }
        Ok(form)
    }
}

/// #383 段階3 P2（2026-10-10 オーナー決定 H1）: banto-tags のレジストリは
/// `protocol: "hub"`（banto-hub をデータ源とする接続、ChronoGazer が Hub 経由の
/// タグを記録するための種別）を受け付けるが、**banto-hub 自身はそれを作らない**。
/// Hub が自分自身を購読することは無く、Hub の収集（`banto_collect::build_config_from`
/// の厳格版）も `hub` 接続を「未対応」として全体を止める。接続を作る・変える
/// すべての入口 - REST の作成/更新（[`plc_connections_create`]/
/// [`plc_connections_update`]）、MCP の `create_connection`/`update_connection`、
/// 未適用キューの適用（`execute_pending_apply` の `plc_connections.*`）- が
/// キューへ積む**前に**これを呼び、`protocol` の項目エラーとして拒否する
/// （キューに積んでから適用時に落ちるのでは、どの変更が悪いかが遅れて分かる）。
pub(crate) fn reject_hub_connection_protocol(protocol: &str) -> Result<(), BantoError> {
    if protocol != banto_tags::HUB_PROTOCOL {
        return Ok(());
    }
    Err(BantoError::Validation {
        field_errors: vec![FieldError {
            field: "protocol".to_string(),
            message: "banto-hub では Hub 経由の接続（hub）は作成できません\
                      （Hub 経由の接続は記録計などの Hub を使う側の設定です）"
                .to_string(),
        }],
    })
}

/// R2b-6b（設計 §5）: `/api/plc-connections` の読み取りの形
/// `{ id, name, protocol, enabled, simulation, settings }`。`settings` は保存した JSON の
/// まま（`push` の既定は `{}`）で、**秘密のキーの値は `{"$secret": "set" | "unreadable"}`**
/// に替える（値・参照は返さない。今までの `passwordSet`・`passwordState` の代わり）。
/// 値が無い秘密のキーは載らない。
///
/// ## 2 つの組み立て方
///
/// - 応答（REST）は [`PlcConnectionResponse::with_secret_rows`]: 秘密の行を開いて
///   `set` / `unreadable` を決める（設計 §5.5）。
/// - `From<PlcConnection>` は未適用キューの指紋（`crate::rest::compute_pending_base_fingerprint`
///   と適用時の照合）: 秘密の参照があれば `set`（読めるかどうかで指紋を変えない）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlcConnectionResponse {
    pub id: i64,
    pub name: String,
    pub protocol: String,
    pub enabled: bool,
    pub simulation: bool,
    pub settings: serde_json::Map<String, serde_json::Value>,
}

/// `{"$secret": state}`（応答の秘密のキーの値）。
fn secret_state_value(state: &str) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    object.insert(
        banto_tags::settings_json::SECRET_REF_KEY.to_string(),
        serde_json::Value::String(state.to_string()),
    );
    serde_json::Value::Object(object)
}

impl From<PlcConnection> for PlcConnectionResponse {
    /// 指紋の形: 秘密の参照（`{"$secret": ref}`）は `{"$secret": "set"}`。
    fn from(conn: PlcConnection) -> Self {
        let mut settings = conn.settings;
        for value in settings.values_mut() {
            if banto_tags::settings_json::is_secret_ref(value) {
                *value = secret_state_value("set");
            }
        }
        Self {
            id: conn.id,
            name: conn.name,
            protocol: conn.protocol,
            enabled: conn.enabled,
            simulation: conn.simulation,
            settings,
        }
    }
}

impl PlcConnectionResponse {
    /// 応答の形: 秘密のキー（`secret_keys` と、秘密の参照の形の値）を `rows`（その接続の秘密の
    /// 行）で開いて `{"$secret": "set" | "unreadable"}` にする（設計 §5.5）。
    pub fn with_secret_rows(
        conn: PlcConnection,
        rows: &[banto_tags::StoredSecretRow],
        secret_keys: &[String],
    ) -> Self {
        let secrets = crate::secrets::ConnectionSecrets::platform_default();
        let mut settings = serde_json::Map::new();
        for (key, value) in &conn.settings {
            let is_secret = secret_keys.iter().any(|k| k == key)
                || banto_tags::settings_json::is_secret_ref(value);
            if !is_secret {
                settings.insert(key.clone(), value.clone());
                continue;
            }
            match crate::secrets::secret_state(&secrets, &conn, key, rows) {
                crate::secrets::SecretState::None => {}
                crate::secrets::SecretState::Set => {
                    settings.insert(key.clone(), secret_state_value("set"));
                }
                crate::secrets::SecretState::Unreadable { .. } => {
                    settings.insert(key.clone(), secret_state_value("unreadable"));
                }
            }
        }
        Self {
            id: conn.id,
            name: conn.name,
            protocol: conn.protocol,
            enabled: conn.enabled,
            simulation: conn.simulation,
            settings,
        }
    }
}

/// [`PlcConnectionResponse::with_secret_rows`] for one connection, reading its sealed
/// rows (R2b-1d).
pub(crate) async fn connection_response(
    service: &banto_tags::PlcConnectionService,
    conn: PlcConnection,
) -> Result<PlcConnectionResponse, BantoError> {
    let rows = service.secret_rows(conn.id).await?;
    let secret_keys = service.secret_keys(&conn.protocol);
    Ok(PlcConnectionResponse::with_secret_rows(
        conn,
        &rows,
        &secret_keys,
    ))
}

/// [`PlcConnectionResponse::with_secret_rows`] for every connection. The connections
/// and the sealed rows are read in one read transaction (PR #777 review P2: a password
/// update committing between two separate reads would show a healthy connection as
/// `unreadable`).
pub(crate) async fn connection_responses(
    service: &banto_tags::PlcConnectionService,
) -> Result<Vec<PlcConnectionResponse>, BantoError> {
    let (connections, rows) = service.list_with_secret_rows().await?;
    Ok(connections
        .into_iter()
        .map(|conn| {
            let own: Vec<banto_tags::StoredSecretRow> = rows
                .iter()
                .filter(|row| row.connection_id == conn.id)
                .cloned()
                .collect();
            let secret_keys = service.secret_keys(&conn.protocol);
            PlcConnectionResponse::with_secret_rows(conn, &own, &secret_keys)
        })
        .collect())
}

pub(super) async fn plc_connections_list(
    State(state): State<TagRegistryState>,
) -> Result<Json<Vec<PlcConnectionResponse>>, ApiError> {
    Ok(Json(connection_responses(&state.plc_connections).await?))
}

pub(super) async fn plc_connections_get(
    State(state): State<TagRegistryState>,
    Path(id): Path<i64>,
) -> Result<Json<PlcConnectionResponse>, ApiError> {
    // One read transaction for the row and its sealed rows (PR #777 review P2).
    let (conn, rows) = state.plc_connections.get_with_secret_rows(id).await?;
    let secret_keys = state.plc_connections.secret_keys(&conn.protocol);
    Ok(Json(PlcConnectionResponse::with_secret_rows(
        conn,
        &rows,
        &secret_keys,
    )))
}

pub(super) async fn plc_connections_create(
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
    // #383 段階3 P2: キューへ積む前に拒否する（`reject_hub_connection_protocol`）。
    if let Err(err) = reject_hub_connection_protocol(&input.protocol) {
        return Err(ApiError(err).into());
    }
    let secret_keys = state.plc_connections.secret_keys(&input.protocol);
    if let Some(status) = registry_change_should_queue(&state.controller, &state.commissioning) {
        // R2b-6b: secret values are sealed before they reach the queue.
        input
            .check_before_queue(&state.plc_connections, &secret_keys)
            .map_err(ApiError)?;
        let form = input.queue_form(&secret_keys).map_err(ApiError)?;
        return queue_pending_registry_change(
            &state,
            &headers,
            "plc_connections.create",
            json!({ "input": form }),
            status,
        )
        .await;
    }
    // R2b-1d / R2b-6b: secret values are sealed here (`PlcConnectionPayload::into_input`).
    let input = input.into_input(&secret_keys).map_err(ApiError)?;
    // 読み取りを挟む書き込みなので BEGIN IMMEDIATE（checklist §5 / sink/service.rs の doc）。
    let mut tx = state
        .manager
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(storage_api_error)?;
    // #635: 書き込みの前の registry。書き込みで新しく生まれた外部名の重なりだけを断る。
    let registry_before = match registry_before(&mut tx, state.manager.registry()).await {
        Ok(before) => before,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(err.into());
        }
    };
    let created = match state
        .plc_connections
        .create_settings_tx(&mut tx, input)
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
    let _preflighted = match preflight_transaction(&mut tx, &registry_before).await {
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
        Some(&created.id.to_string()),
        Some(json!({ "name": created.name, "enabled": created.enabled })),
    )
    .await;
    commit_catalog_and_notify(&state.controller, &state.events, "plc_connections")
        .await
        .map_err(RegistryMutationError::LiveApplyFailed)?;
    let response = connection_response(&state.plc_connections, created)
        .await
        .map_err(ApiError)?;
    Ok(Json(response).into_response())
}

pub(super) async fn plc_connections_update(
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
    // #383 段階3 P2: キューへ積む前に拒否する（`reject_hub_connection_protocol`）。
    if let Err(err) = reject_hub_connection_protocol(&input.protocol) {
        return Err(ApiError(err).into());
    }
    let secret_keys = state.plc_connections.secret_keys(&input.protocol);
    if let Some(status) = registry_change_should_queue(&state.controller, &state.commissioning) {
        // R2b-6b: secret values are sealed before they reach the queue.
        input
            .check_before_queue(&state.plc_connections, &secret_keys)
            .map_err(ApiError)?;
        let form = input.queue_form(&secret_keys).map_err(ApiError)?;
        return queue_pending_registry_change(
            &state,
            &headers,
            "plc_connections.update",
            json!({ "id": id, "input": form }),
            status,
        )
        .await;
    }
    // R2b-1d / R2b-6b: secret values are sealed here (`PlcConnectionPayload::into_input`).
    let input = input.into_input(&secret_keys).map_err(ApiError)?;
    // 読み取りを挟む書き込みなので BEGIN IMMEDIATE（checklist §5 / sink/service.rs の doc）。
    let mut tx = state
        .manager
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(storage_api_error)?;
    // #635: 書き込みの前の registry。書き込みで新しく生まれた外部名の重なりだけを断る。
    let registry_before = match registry_before(&mut tx, state.manager.registry()).await {
        Ok(before) => before,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(err.into());
        }
    };
    let updated = match state
        .plc_connections
        .update_settings_tx(&mut tx, id, input)
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
    let _preflighted = match preflight_transaction(&mut tx, &registry_before).await {
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
        Some(&id.to_string()),
        Some(json!({ "name": updated.name, "enabled": updated.enabled })),
    )
    .await;
    commit_catalog_and_notify(&state.controller, &state.events, "plc_connections")
        .await
        .map_err(RegistryMutationError::LiveApplyFailed)?;
    let response = connection_response(&state.plc_connections, updated)
        .await
        .map_err(ApiError)?;
    Ok(Json(response).into_response())
}

pub(super) async fn plc_connections_delete(
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
    // 読み取りを挟む書き込みなので BEGIN IMMEDIATE（checklist §5 / sink/service.rs の doc）。
    let mut tx = state
        .manager
        .pool()
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(storage_api_error)?;
    // #635: 書き込みの前の registry。書き込みで新しく生まれた外部名の重なりだけを断る。
    let registry_before = match registry_before(&mut tx, state.manager.registry()).await {
        Ok(before) => before,
        Err(err) => {
            let _ = tx.rollback().await;
            return Err(err.into());
        }
    };
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
    let _preflighted = match preflight_transaction(&mut tx, &registry_before).await {
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
        Some(&id.to_string()),
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
pub(super) async fn plc_connections_test_saved(
    State(state): State<TagRegistryState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Response, ApiError> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "plc_connections",
        "POST",
        "/api/plc-connections/{id}/test",
    )
    .await?;

    // One read transaction for the row and its sealed rows (PR #777 review P2).
    let (conn, secret_rows) = state.plc_connections.get_with_secret_rows(id).await?;
    if !conn.is_db_source() {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "message": "S1ではpostgres接続のみ接続テストに対応しています。",
            })),
        )
            .into_response());
    }

    let outcome = crate::db_source::test_connection(&conn, &secret_rows).await;
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
// する([`test_connection`]参照)。
//
// R2b-3a-2(docs/banto-hub-driver-io-design.md §4.2・Q11): プロトコル別の
// `test_modbus_connection`/`test_slmp_connection`(`ModbusTcpClient`/
// `SlmpClient` を直接ダイヤルし、固定のリテラル `40001`/`D0` を読む)を
// ドライバに依らない [`test_connection`] にまとめた。読む 1 点はドライバの
// `DriverBase::probe()`、ダイヤルはbroker が解決した Session / Frame ドライバの
// `connect`、収集中のセッションを使ってよいかは設定の指紋で見る
// (#605 の CPU 種別の照合を、どの設定にも広げたもの)。

/// 接続テストの疎通確認に使うタイムアウト(接続・応答とも共通)。数秒固定
/// (ux-plan.md §4「タイムアウトは短め（数秒）に固定」)。
const PLC_TEST_TIMEOUT: Duration = Duration::from_secs(3);

/// 直接ダイヤル失敗時(`tcp`・`timeout`)に付けるセッション上限ヒント。
/// R2b-3a-2 でプロトコル別の 2 つ(Modbus・SLMP)を 1 つにまとめた: 実機
/// R08ENCPU は対象ポートが既に使用中だと 2 本目を受け付けず、KM-D1-ETN のような
/// 1 対 1 の機器も同じ(このファイルの上の「重要な制約」)。
const SESSION_HINT: &str = " 対象機器のポートが既に別の接続(この hub の収集や他アプリ)で使用中の可能性があります。機器によっては同じポートへの2本目の接続を受け付けません(SLMP の実機 R08ENCPU・Modbus の KM-D1-ETN で確認済み)。";

/// `POST /api/plc-connections/test` のリクエストボディ（R2b-6b、設計 §5）- 保存前の
/// フォーム値を `{ protocol, simulation, settings, connectionId? }` で受け取る
/// （[`PlcConnectionPayload`] と同じ `settings` の形。保存済み接続の編集中のみ
/// `connectionId` を添える）。平らな欄は受けない（`deny_unknown_fields`）。MCP の
/// `test_connection` も同じ形を直接受ける（R2b-7a）。
///
/// - `settings` はドライバのスキーマ（`banto_tags::settings_from_schema`）を通してから
///   ダイヤルする。省いた欄はスキーマの既定値。
/// - 秘密のキー: 省略は保存済み（`connectionId` の同じプロトコルの接続）の値を補う、
///   `null`・空文字は値無し、文字列はその値で試す（保存しない・封印しない）。
/// - `simulation: true` はダイヤルの前に断る（未保存のフォームの今の値。新規・編集中とも）。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlcConnectionTestPayload {
    pub protocol: String,
    #[serde(default)]
    pub simulation: bool,
    #[serde(default)]
    pub settings: serde_json::Map<String, serde_json::Value>,
    /// 編集中の保存済み接続の id。broker のセッションの再利用（指紋が合うときだけ）と、
    /// 省いた秘密値の補いに使う。
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

fn test_error(kind: &str, message: impl Into<String>) -> PlcConnectionTestError {
    PlcConnectionTestError {
        kind: kind.to_string(),
        message: message.into(),
    }
}

/// 接続の段で起きた致命のエラー([`FatalError`])を [`PlcConnectionTestError`] に
/// 分類する(R2b-3a-2: プロトコル別の `PlcError` の分類から置き換えた)。
/// `connecting` は接続の段か(時間切れの文言を分ける)。`hint` が `Some` の
/// とき、`kind` が `"tcp"`・`"timeout"` のときだけ末尾に付ける - 呼び出し側は
/// 「セッション上限ヒントを付けたい経路」(直接ダイヤル)でだけ `Some` を渡す。
/// 既存セッションの再利用の経路は「2 本目をダイヤルしない」こと自体が対策
/// なので常に `None`。
fn classify_fatal(
    err: &FatalError,
    connecting: bool,
    hint: Option<&str>,
) -> PlcConnectionTestError {
    let (kind, mut message) = match err {
        FatalError::Timeout if connecting => (
            "timeout",
            "接続タイムアウトです。ホスト/ポート、ネットワーク到達性とハンドシェイクを確認してください。"
                .to_string(),
        ),
        FatalError::Timeout => (
            "timeout",
            "応答タイムアウトです(3秒)。接続はできましたが応答がありませんでした。".to_string(),
        ),
        FatalError::Io(msg) => (
            "tcp",
            format!("TCP接続に失敗しました(ポートが閉じている、または到達できません): {msg}"),
        ),
        FatalError::Framing(msg) | FatalError::Protocol(msg) => (
            "protocol",
            format!(
                "プロトコルエラー: 応答が不正です({msg})。プロトコル選択やポート番号が実機と一致しているか確認してください。"
            ),
        ),
        FatalError::InvalidConfig(msg) => ("protocol", format!("接続設定が不正です: {msg}")),
        other => ("protocol", other.to_string()),
    };
    if matches!(kind, "tcp" | "timeout") {
        if let Some(hint) = hint {
            message.push_str(hint);
        }
    }
    test_error(kind, message)
}

/// 読む 1 点のタグごとのエラー([`ItemError`])の分類。接続はできている。
fn classify_item(err: &ItemError) -> PlcConnectionTestError {
    match err {
        ItemError::Device(device) => test_error(
            "device",
            format!(
                "デバイス読み出しエラー: 接続はできましたが、指定したデバイス/レジスタを読み出せませんでした({device})。アドレス設定や機種依存の可能性があります(致命的ではありません)。"
            ),
        ),
        other => test_error("device", format!("読み出しに失敗しました: {other}")),
    }
}

/// 試す設定（R2b-6b）: フォームの `settings` をドライバのスキーマに通し
/// （`banto_tags::settings_from_schema` → `settings_to_api`、保存と同じ組み立て）、秘密のキーを
/// 足したもの。秘密のキーは、フォームの文字列があればそれ、省略なら保存済み（`connectionId`
/// があり、同じプロトコル）の値を開いて補う。`null`・空文字は値無し。スキーマで必須の秘密の
/// 欄が埋まらなければ断る。
pub(super) struct RequestedSettings {
    pub(super) settings: Settings,
    /// 保存済みの接続（指紋に入る秘密値の参照の出どころ）。
    pub(super) saved: Option<PlcConnection>,
    /// フォームが秘密のキーを替えた・消したか（保存済みのセッションの読みは答えにならない）。
    pub(super) secrets_changed: bool,
}

pub(super) async fn requested_settings(
    manager: &CollectorManager,
    driver: &dyn banto_driver_api::DriverBase,
    payload: &PlcConnectionTestPayload,
) -> Result<RequestedSettings, PlcConnectionTestError> {
    let saved = match payload.connection_id {
        Some(id) => manager
            .plc_connections()
            .get(id)
            .await
            .ok()
            .filter(|conn| conn.protocol == payload.protocol),
        None => None,
    };
    let secret_keys = driver.secret_keys();
    let mut input = payload.settings.clone();
    let mut given: Vec<(String, String)> = Vec::new();
    let mut omitted: Vec<String> = Vec::new();
    let mut secrets_changed = false;
    for key in &secret_keys {
        match input.remove(key) {
            None => omitted.push(key.clone()),
            Some(serde_json::Value::Null) => secrets_changed = true,
            Some(serde_json::Value::String(plain)) => {
                secrets_changed = true;
                if !plain.is_empty() {
                    given.push((key.clone(), plain));
                }
            }
            Some(_) => {
                return Err(refused_settings(&[banto_driver_api::FieldError::new(
                    key.as_str(),
                    "文字列か null で指定してください",
                )]))
            }
        }
    }
    let map = banto_tags::settings_from_schema(&driver.settings_schema(), &input)
        .map_err(|errors| refused_settings(&errors))?;
    let mut settings = banto_tags::settings_to_api(&map);
    for (key, plain) in given {
        settings.insert(key.as_str(), plain);
    }
    if let (Some(conn), false) = (&saved, omitted.is_empty()) {
        let rows = manager
            .plc_connections()
            .secret_rows(conn.id)
            .await
            .map_err(|err| {
                test_error("protocol", format!("保存済みの秘密値を読めません: {err}"))
            })?;
        let secrets = crate::secrets::ConnectionSecrets::platform_default();
        for key in &omitted {
            match crate::secrets::open_connection_secret(&secrets, conn, key, &rows) {
                Ok(Some(plain)) => settings.insert(key.as_str(), plain.as_str()),
                Ok(None) => {}
                Err(reason) => {
                    return Err(test_error(
                        "protocol",
                        format!(
                        "保存済みの {key} を読めません（{}）。入れ直してからテストしてください。",
                        reason.as_str()
                    ),
                    ))
                }
            }
        }
    }
    let missing: Vec<banto_driver_api::FieldError> = driver
        .settings_schema()
        .fields
        .iter()
        .filter(|field| {
            field.required
                && matches!(field.kind, banto_driver_api::FieldKind::Secret)
                && settings.get(field.key.as_str()).is_none()
        })
        .map(|field| {
            banto_driver_api::FieldError::new(format!("settings.{}", field.key), "必須です")
        })
        .collect();
    if !missing.is_empty() {
        return Err(refused_settings(&missing));
    }
    Ok(RequestedSettings {
        settings,
        saved,
        secrets_changed,
    })
}

/// ドライバの `validate_config` が断った設定を、ダイヤルせずに返す。接続先の
/// 欄(`host`・`port`・`unit_id`)は従来の直接ダイヤルの範囲検査と同じ
/// `"tcp"`、それ以外(CPU 種別など)は `"protocol"`。
fn refused_settings(errors: &[banto_driver_api::FieldError]) -> PlcConnectionTestError {
    let kind = if errors.iter().all(|e| {
        matches!(
            e.key.strip_prefix("settings.").unwrap_or(&e.key),
            keys::HOST | keys::PORT | keys::UNIT_ID
        )
    }) {
        "tcp"
    } else {
        "protocol"
    };
    let detail = errors
        .iter()
        .map(|e| {
            let field = if e.key.starts_with("settings.") {
                e.key.clone()
            } else {
                format!("settings.{}", e.key)
            };
            format!("{field}: {}", e.message)
        })
        .collect::<Vec<_>>()
        .join("、");
    test_error(kind, format!("接続設定が不正です({detail})。"))
}

/// ドライバが名乗る読む 1 点([`banto_driver_api::DriverBase::probe`])を、
/// そのドライバの文法で [`TagSpec`] にする。`None` は「読まずに接続だけ確かめる」。
fn probe_spec(
    base: &dyn banto_driver_api::DriverBase,
) -> Result<Option<TagSpec>, PlcConnectionTestError> {
    let Some((text, kind)) = base.probe() else {
        return Ok(None);
    };
    let address = base.parse_address(&text, &kind).map_err(|err| {
        test_error(
            "protocol",
            format!("ドライバの接続テストの読み取り点 {text:?} を解釈できません: {err}"),
        )
    })?;
    Ok(Some(TagSpec::new(address, kind)))
}

/// 読む 1 点の結果(1 件)を接続テストの結果にする。
fn probe_outcome(results: Vec<Result<Value, ItemError>>) -> (bool, Option<PlcConnectionTestError>) {
    match results.into_iter().next() {
        Some(Ok(_)) => (true, None),
        Some(Err(err)) => (false, Some(classify_item(&err))),
        None => (
            false,
            Some(test_error("device", "読み出し結果が空でした。")),
        ),
    }
}

/// 接続テストの本体(R2b-3a-2、docs/banto-hub-driver-io-design.md Q11)。
/// プロトコルに依らない:
///
/// 1. 試す設定を作る([`requested_settings`])。
/// 2. `payload.connection_id` があり、その接続の broker セッションが既に
///    生きていれば、それを再利用して読む(新規ダイヤルしない - 実機 R08ENCPU・
///    KM-D1-ETN は対象ポートが既に使用中だと 2 本目を受け付けないため、これを
///    誤診しないための対策。このファイルの上のコメント参照)。ただし、
///    セッションの設定の指紋([`banto_broker::BrokerHandle::settings_fingerprint`]、
///    いま読む handle 自身が作成時に持っていたもの)が試す設定の指紋と違えば、
///    セッションの読みはフォームの値を試したことにならないので、試さずに
///    「保存してからテストしてください」と返す(#605 の CPU 種別の照合を
///    全部の設定に広げたもの)。
/// 3. 無ければ直接ダイヤルする: broker と共通の Session / Frame ドライバで
///    `validate_config` → `connect`(接続・各応答の時間切れは
///    [`PLC_TEST_TIMEOUT`]、外側は broker と同じ種類別の上限)→ ドライバの読む 1 点を
///    `read_batch`(外側の上限は broker と同じ式)→ 必ず `disconnect`。
async fn test_connection(
    manager: &CollectorManager,
    payload: &PlcConnectionTestPayload,
) -> (bool, Option<PlcConnectionTestError>) {
    let registry = manager.registry();
    let Some(resolved) = banto_broker::resolve_session_driver(registry, &payload.protocol) else {
        return (
            false,
            Some(test_error(
                "unsupported",
                format!("不明なプロトコルです: {}", payload.protocol),
            )),
        );
    };
    let driver = resolved.driver();
    // 保存済みの接続（同じプロトコル）は、指紋に入る秘密値の参照の出どころ
    // （フォームが秘密のキーを省いたら、参照は保存済みのまま、と読む）。
    let RequestedSettings {
        settings,
        saved,
        secrets_changed,
    } = match requested_settings(manager, driver.as_ref(), payload).await {
        Ok(requested) => requested,
        Err(err) => return (false, Some(err)),
    };
    let probe = match probe_spec(driver.as_ref()) {
        Ok(probe) => probe,
        Err(err) => return (false, Some(err)),
    };

    if let Some(connection_id) = payload.connection_id {
        if let Some(handle) = manager.sessions().handle_for(connection_id) {
            let wanted =
                manager
                    .sessions()
                    .dial_fingerprint(&payload.protocol, saved.as_ref(), &settings);
            // R2b-6b: フォームが秘密値を替えた・消したなら、指紋（参照だけを持つ）は
            // 合っても、セッションの読みはフォームの値を試したことにならない。
            if secrets_changed || wanted.is_none() || handle.settings_fingerprint() != wanted {
                return (
                    false,
                    Some(test_error(
                        "protocol",
                        "この接続は収集中で、保存済みの設定のセッションを使っています。フォームの設定（接続先・CPU 種別・秘密値など）は、保存してからテストしてください。",
                    )),
                );
            }
            let Some(spec) = probe else {
                // 読む点を持たないドライバ: セッションが張れていれば成功。
                return match *handle.status_watch().borrow() {
                    BrokerConnectionStatus::Connected => (true, None),
                    _ => (false, Some(test_error("tcp", SHARED_SESSION_DOWN))),
                };
            };
            // `ReadOnlyHandle::read` は broker がジョブに上限を掛ける
            // (design §5.5)が、テストは従来どおり数秒で切る。
            return match tokio::time::timeout(PLC_TEST_TIMEOUT, handle.read(vec![spec])).await {
                Err(_elapsed) => (
                    false,
                    Some(test_error(
                        "timeout",
                        "応答タイムアウトです(3秒)。共有セッションが応答しませんでした。",
                    )),
                ),
                Ok(Err(BrokerError::Disconnected { .. })) => {
                    (false, Some(test_error("tcp", SHARED_SESSION_DOWN)))
                }
                Ok(Err(BrokerError::ConnectionFailed { reason, .. })) => (
                    false,
                    Some(test_error(
                        "tcp",
                        format!("共有セッションが接続断で失敗しました: {reason}"),
                    )),
                ),
                Ok(Err(BrokerError::TaskGone { .. })) => (
                    false,
                    Some(test_error(
                        "tcp",
                        "内部エラー: セッションタスクが終了しています。",
                    )),
                ),
                // 防御的フォールバック: この経路では通常発生しない
                // (UnsupportedProtocol/InvalidSettings は ensure_connection
                // 時点で弾かれているはず)。
                Ok(Err(err)) => (false, Some(test_error("protocol", err.to_string()))),
                // 既存セッション再利用経路: セッション上限ヒントは付けない
                // (design の意図: 「2本目をダイヤルしない」こと自体が対策)。
                Ok(Ok(results)) => probe_outcome(results),
            };
        }
    }

    // 直接ダイヤル(connectionId が無い、またはセッションが見つからない場合)。
    let refused = driver.validate_config(&settings);
    if !refused.is_empty() {
        return (false, Some(refused_settings(&refused)));
    }
    let cx = ConnectContext::new("connection-test", PLC_TEST_TIMEOUT, PLC_TEST_TIMEOUT);
    let connected = tokio::time::timeout(
        resolved.connect_limit(banto_broker::SessionTimeouts {
            connect: PLC_TEST_TIMEOUT,
            response: PLC_TEST_TIMEOUT,
        }),
        driver.connect(&settings, cx),
    )
    .await
    .unwrap_or(Err(FatalError::Timeout));
    let mut session = match connected {
        Ok(session) => session,
        Err(err) => return (false, Some(classify_fatal(&err, true, Some(SESSION_HINT)))),
    };
    let outcome = match probe {
        None => (true, None),
        Some(spec) => {
            let specs = [spec];
            let read = tokio::time::timeout(
                banto_broker::read_job_limit(specs.len(), PLC_TEST_TIMEOUT),
                session.read_batch(&specs),
            )
            .await
            .unwrap_or(Err(FatalError::Timeout));
            match read {
                Ok(results) => probe_outcome(results),
                Err(err) => (false, Some(classify_fatal(&err, false, Some(SESSION_HINT)))),
            }
        }
    };
    let _ = tokio::time::timeout(banto_broker::DISCONNECT_LIMIT, session.disconnect()).await;
    outcome
}

/// 共有セッションが切断中のときの文言。
const SHARED_SESSION_DOWN: &str = "この接続の共有セッションは現在切断中です(再接続待機中)。PLCの電源やネットワーク、または他アプリとのセッション競合を確認してください。";

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
/// 疎通確認そのもの(virtual/simulation の即時拒否・ドライバによるダイヤル・
/// 所要時間計測)は[`run_plc_connection_test`]へ切り出してある - T21
/// S1-c（docs/banto-hub-t21-design.md）の MCP `test_connection`ツール
/// ([`crate::mcp`]の`tool_test_connection`)がこの REST ハンドラと**全く同じ
/// 疎通確認**を行うために共有する(二重実装しない、このファイル冒頭
/// 「§3.7」節と同じ規律)。
pub(super) async fn plc_connections_test(
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
    // "unsupported"(実プロトコルの疎通確認より前に判定。未知プロトコルは
    // `test_connection` の最初)。
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
        // R2b-3a-2: プロトコル名で分けない。broker が管理しないプロトコル
        // (レジストリに Session / Frame ドライバが無い)は `test_connection` が
        // "unsupported" で返す。
        test_connection(manager, payload).await
    };

    PlcConnectionTestResponse {
        ok,
        elapsed_ms: started.elapsed().as_millis() as u64,
        error,
    }
}

#[cfg(test)]
mod frame_connection_tests {
    use super::*;
    use crate::{
        broker_glue::{BrokerSimRegistry, HubDrivers, HubSessions},
        computed::{ComputedEngine, ServerTagStore},
    };
    use banto_driver_api::{
        AddressError, DataType, DriverAddress, DriverBase, DriverDescriptor, DriverEntry, DriverId,
        FieldError, FrameConnection, FrameDriver, TagKind,
    };
    use banto_driver_conformance::fake::echo::{EchoFrameDriver, EchoResponder};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    // The connection-test payload supplies host/port for modbus-tcp.
    // Override only the fixture id; production registrations remain unchanged.
    struct ProbeFrameDriver {
        descriptor: DriverDescriptor,
        echo: EchoFrameDriver,
        creates: Arc<AtomicUsize>,
        required_token: bool,
    }
    impl DriverBase for ProbeFrameDriver {
        fn descriptor(&self) -> &DriverDescriptor {
            &self.descriptor
        }
        fn validate_config(&self, settings: &Settings) -> Vec<FieldError> {
            self.echo.validate_config(settings)
        }
        /// R2b-6b: the test payload's `settings` go through this schema (host / port /
        /// unit_id, like `banto-driver-modbus`).
        fn settings_schema(&self) -> banto_driver_api::SettingsSchema {
            use banto_driver_api::{FieldKind, SettingField, SettingsSchema};
            let mut fields = vec![
                SettingField::new("host", "ホスト", FieldKind::host()).required(),
                SettingField::new("port", "ポート", FieldKind::integer(1, 65_535)).required(),
                SettingField::new("unit_id", "ユニットID", FieldKind::integer(0, 255)).default(1),
            ];
            if self.required_token {
                fields.push(SettingField::new("token", "トークン", FieldKind::secret()).required());
            }
            SettingsSchema::new(fields)
        }
        fn secret_keys(&self) -> Vec<String> {
            if self.required_token {
                vec!["token".into()]
            } else {
                Vec::new()
            }
        }
        fn parse_address(&self, text: &str, kind: &TagKind) -> Result<DriverAddress, AddressError> {
            let mut address = self.echo.parse_address(text, kind)?;
            address.driver = self.descriptor.id.clone();
            Ok(address)
        }
        fn offset_address(
            &self,
            text: &str,
            kind: &TagKind,
            n: i64,
        ) -> Result<String, AddressError> {
            self.echo.offset_address(text, kind, n)
        }
        fn address_help(&self) -> String {
            self.echo.address_help()
        }
        fn probe(&self) -> Option<(String, TagKind)> {
            Some(("W10".into(), TagKind::numeric(DataType::U16)))
        }
    }
    impl FrameDriver for ProbeFrameDriver {
        fn create_connection(
            &self,
            settings: &Settings,
        ) -> Result<Box<dyn FrameConnection>, FatalError> {
            self.creates.fetch_add(1, Ordering::SeqCst);
            self.echo.create_connection(settings)
        }
    }
    async fn fixture() -> (
        CollectorManager,
        Arc<HubSessions>,
        sqlx::SqlitePool,
        Arc<AtomicUsize>,
    ) {
        fixture_with_secret(false).await
    }
    async fn fixture_with_secret(
        required_token: bool,
    ) -> (
        CollectorManager,
        Arc<HubSessions>,
        sqlx::SqlitePool,
        Arc<AtomicUsize>,
    ) {
        let pool = crate::db::init_db_memory().await.unwrap();
        let creates = Arc::new(AtomicUsize::new(0));
        let echo = EchoFrameDriver::new();
        let mut descriptor = echo.descriptor().clone();
        descriptor.id = DriverId::new("modbus-tcp").unwrap();
        let mut drivers = HubDrivers::none();
        Arc::get_mut(&mut drivers.registry)
            .unwrap()
            .register(DriverEntry::frame(Arc::new(ProbeFrameDriver {
                descriptor,
                echo,
                creates: creates.clone(),
                required_token,
            })))
            .unwrap();
        let sessions = Arc::new(HubSessions::new(
            banto_broker::BackoffConfig::default(),
            drivers,
        ));
        let manager = CollectorManager::new(
            pool.clone(),
            std::env::temp_dir(),
            Arc::new(banto_tstore::SystemClock),
            banto_collect::CollectorOptions::default(),
            sessions.clone(),
            Arc::new(BrokerSimRegistry::new()),
            Arc::new(ComputedEngine::new(Arc::new(ServerTagStore::new()))),
        );
        (manager, sessions, pool, creates)
    }
    fn payload(port: i64) -> PlcConnectionTestPayload {
        PlcConnectionTestPayload {
            protocol: "modbus-tcp".into(),
            simulation: false,
            settings: serde_json::json!({"host": "127.0.0.1", "port": port, "unit_id": 1})
                .as_object()
                .cloned()
                .unwrap(),
            connection_id: None,
        }
    }

    /// 接続テストも保存と同じく `Host` の前後の空白を落としてから張る（`" 127.0.0.1 "`
    /// のままだと名前解決に失敗する）。
    #[tokio::test]
    async fn requested_settings_trims_the_host_like_the_save_path() {
        let (manager, sessions, pool, _creates) = fixture().await;
        let resolved = banto_broker::resolve_session_driver(manager.registry(), "modbus-tcp")
            .expect("modbus-tcp is registered");
        let mut test = payload(502);
        test.settings
            .insert("host".into(), serde_json::json!(" 127.0.0.1 "));
        let requested = requested_settings(&manager, resolved.driver().as_ref(), &test)
            .await
            .unwrap_or_else(|err| panic!("{err:?}"));
        assert_eq!(requested.settings.get("host"), Some("127.0.0.1"));
        manager.shutdown().await;
        sessions.shutdown().await;
        pool.close().await;
    }

    #[tokio::test]
    async fn frame_connection_test_refuses_invalid_settings_without_io() {
        let (manager, sessions, pool, creates) = fixture().await;
        assert!(sessions.is_broker_managed("modbus-tcp"));
        let response = run_plc_connection_test(&manager, &payload(0)).await;
        assert!(!response.ok);
        let error = response.error.unwrap();
        assert_eq!(
            error.kind, "tcp",
            "Frame must reach validation, not unknown protocol"
        );
        assert!(error.message.contains("port"), "{error:?}");
        assert_eq!(creates.load(Ordering::SeqCst), 0);
        manager.shutdown().await;
        sessions.shutdown().await;
        pool.close().await;
    }

    #[tokio::test]
    async fn connection_test_names_a_missing_required_secret_under_settings() {
        let (manager, sessions, pool, creates) = fixture_with_secret(true).await;
        let response = run_plc_connection_test(&manager, &payload(502)).await;
        assert!(!response.ok);
        let error = response.error.unwrap();
        assert!(
            error.message.contains("settings.token: 必須です"),
            "{error:?}"
        );
        assert_eq!(creates.load(Ordering::SeqCst), 0);
        manager.shutdown().await;
        sessions.shutdown().await;
        pool.close().await;
    }

    #[tokio::test]
    async fn frame_connection_test_completes_handshake_probe_and_disconnect_over_tcp() {
        use banto_driver_conformance::frame::{Reply, Responder};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut echo = EchoResponder::new();
            echo.set_word('W', 10, 7);
            echo.on_connect();
            let mut codes = vec![];
            loop {
                let mut header = [0; 6];
                match stream.read_exact(&mut header).await {
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
                    Err(error) => panic!("read request: {error}"),
                }
                let mut request = header.to_vec();
                request.resize(
                    6 + usize::from(u16::from_be_bytes([header[4], header[5]])),
                    0,
                );
                stream.read_exact(&mut request[6..]).await.unwrap();
                codes.push(header[1]);
                let Reply::Bytes(reply) = echo.reply(&request) else {
                    panic!("echo reply");
                };
                stream.write_all(&reply).await.unwrap();
            }
            codes
        });
        let (manager, sessions, pool, creates) = fixture().await;
        let response = tokio::time::timeout(
            Duration::from_secs(5),
            run_plc_connection_test(&manager, &payload(i64::from(port))),
        )
        .await
        .unwrap();
        assert!(response.ok, "{response:?}");
        assert_eq!(creates.load(Ordering::SeqCst), 1);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), peer)
                .await
                .unwrap()
                .unwrap(),
            b"HR"
        );
        assert!(
            sessions.handle_for(1).is_none(),
            "direct test must not create a broker task"
        );
        manager.shutdown().await;
        sessions.shutdown().await;
        pool.close().await;
    }
}
