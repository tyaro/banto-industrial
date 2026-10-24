//! MCP の接続ツールの平らな形のアダプタ（R2b-6b、docs/banto-hub-driver-ui-design.md §5。
//! **R2b-7 で消える**）。
//!
//! REST の `/api/plc-connections` と接続テストは R2b-6b で `settings` の形
//! （`crate::rest::PlcConnectionPayload`・`PlcConnectionTestPayload`）に変わったが、MCP の
//! `list_connections`・`create_connection`・`update_connection`・`test_connection` は R2b-7 まで
//! 今の平らな形（`host`・`port`・`unitId`・`wordOrder`・`cpuType`・`database`・`username`・
//! `password`・push の `settings`）のまま据え置く（Q2）。このモジュールが両者を写す:
//!
//! - 入力: まず平らな形の旧い検証 (1)（`PlcConnectionService::validate_flat_input`。項目名と
//!   文言は今のまま、例えば `unitId`）を通し、`banto_tags::settings_json::legacy::settings_from_flat`
//!   で `settings` に写して REST と同じ入口（封印・未適用キュー・`create_settings_tx`）に渡す。
//!   今の「省略は保つ」は、更新のときに保存済みの行から補う: `slmp` の `cpuType`、`push` の
//!   `settings`。`wordOrder` の省略（空文字）はスキーマの既定（プロトコルの既定と同じ）。
//!   `password` はそのまま `settings.password` に載り、REST と同じ tri-state で封印される
//!   （`null`・省略は保つ、空文字は消す、値は置き換え）。
//! - 応答: 今の平らな応答（`passwordSet`・`passwordState`）を組み立てる。
//! - 接続テスト: 平らな値を `settings` に写す。フォームが送らない `wordOrder` と省略した
//!   `cpuType` は、今と同じく保存済み（`connectionId` の同じプロトコルの接続）の値を使う。
//!   `simulation` はそのまま引き継ぐ。

use banto_core::{BantoError, FieldError};
use banto_tags::{PlcConnection, PlcConnectionInput, PlcConnectionService};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::rest::{PlcConnectionPayload, PlcConnectionTestPayload};

fn default_protocol() -> String {
    banto_tags::MODBUS_PROTOCOL.to_string()
}

fn default_unit_id() -> i64 {
    1
}

fn default_enabled() -> bool {
    true
}

/// MCP の `create_connection`・`update_connection` の平らな入力（R2b-6b より前の REST の
/// `PlcConnectionPayload` と同じ形）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct FlatPlcConnectionPayload {
    pub(super) name: String,
    #[serde(default = "default_protocol")]
    pub(super) protocol: String,
    pub(super) host: String,
    pub(super) port: i64,
    #[serde(default = "default_unit_id")]
    unit_id: i64,
    #[serde(default = "default_enabled")]
    enabled: bool,
    #[serde(default)]
    simulation: bool,
    /// 空文字（省略）はプロトコルの既定。
    #[serde(default)]
    word_order: String,
    /// 作成の省略・空文字は `R`、更新の省略・空文字は保存済みを保つ。
    #[serde(default)]
    cpu_type: Option<String>,
    #[serde(default)]
    database: Option<String>,
    #[serde(default)]
    username: Option<String>,
    /// 省略・`null` は保つ（作成は無し）、空文字は消す、値は置き換え。
    #[serde(default)]
    password: Option<String>,
    /// push の `settings`。作成の省略は既定、更新の省略・`null` は保存済みを保つ。
    #[serde(default)]
    settings: Option<Value>,
}

impl FlatPlcConnectionPayload {
    /// 旧い検証 (1) に掛ける平らな入力（秘密値は持たない）。
    fn flat_input(&self) -> PlcConnectionInput {
        PlcConnectionInput {
            name: self.name.clone(),
            protocol: self.protocol.clone(),
            host: self.host.clone(),
            port: self.port,
            unit_id: self.unit_id,
            enabled: self.enabled,
            simulation: self.simulation,
            word_order: self.word_order.clone(),
            cpu_type: self.cpu_type.clone(),
            database: self.database.clone(),
            username: self.username.clone(),
            secrets: Default::default(),
            settings: self.settings.clone(),
        }
    }

    /// 平らな形の旧い検証 (1)。今と同じ項目名・文言で断る（`password` は postgres 以外で
    /// 値があれば今と同じ文言で断る）。
    pub(super) fn check(&self, service: &PlcConnectionService) -> Result<(), BantoError> {
        if self.protocol != banto_tags::POSTGRES_PROTOCOL
            && self.password.as_deref().is_some_and(|p| !p.is_empty())
        {
            return Err(BantoError::Validation {
                field_errors: vec![FieldError {
                    field: "password".to_string(),
                    message: "postgres接続以外では指定できません".to_string(),
                }],
            });
        }
        service.validate_flat_input(self.flat_input())
    }

    /// `settings` の形に写す（モジュールの doc）。`stored` は更新の対象の保存済みの行。
    pub(super) fn into_settings_payload(
        self,
        stored: Option<&PlcConnection>,
    ) -> PlcConnectionPayload {
        let mut settings =
            banto_tags::settings_json::legacy::settings_from_flat(&self.flat_input());
        if settings
            .get("word_order")
            .and_then(Value::as_str)
            .is_some_and(str::is_empty)
        {
            settings.remove("word_order");
        }
        let stored_same = stored.filter(|conn| conn.protocol == self.protocol);
        if self.protocol == "slmp" && !settings.contains_key("cpu_type") {
            if let Some(cpu) = stored_same.and_then(|conn| conn.settings.get("cpu_type")) {
                settings.insert("cpu_type".to_string(), cpu.clone());
            }
        }
        if self.protocol == banto_tags::PUSH_PROTOCOL
            && !matches!(self.settings, Some(Value::Object(_)))
        {
            if let Some(conn) = stored_same {
                settings = conn.settings.clone();
            }
        }
        if self.protocol == banto_tags::POSTGRES_PROTOCOL {
            if let Some(password) = self.password {
                settings.insert(
                    banto_tags::PASSWORD_KEY.to_string(),
                    Value::String(password),
                );
            }
        }
        PlcConnectionPayload {
            name: self.name,
            protocol: self.protocol,
            enabled: self.enabled,
            simulation: self.simulation,
            settings,
        }
    }
}

/// MCP の `list_connections` と、作成・更新の結果の平らな応答（R2b-6b より前の REST の
/// `PlcConnectionResponse` と同じ形）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct FlatPlcConnectionResponse {
    id: i64,
    name: String,
    protocol: String,
    host: String,
    port: i64,
    unit_id: i64,
    enabled: bool,
    simulation: bool,
    word_order: String,
    cpu_type: String,
    database: Option<String>,
    username: Option<String>,
    password_set: bool,
    /// push の `settings`（既定のままなら `null`、他のプロトコルは常に `null`）。
    settings: Option<Value>,
    /// `"none"` / `"set"` / `"unreadable"`（設計 §5.5）。
    password_state: &'static str,
}

impl FlatPlcConnectionResponse {
    fn build(conn: PlcConnection, rows: &[banto_tags::StoredSecretRow]) -> Self {
        let state = crate::secrets::secret_state(
            &crate::secrets::ConnectionSecrets::platform_default(),
            &conn,
            banto_tags::PASSWORD_KEY,
            rows,
        );
        let settings = (conn.protocol == banto_tags::PUSH_PROTOCOL && !conn.settings.is_empty())
            .then(|| Value::Object(conn.settings.clone()));
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
            cpu_type: conn.cpu_type,
            database: conn.database,
            username: conn.username,
            password_set: state == crate::secrets::SecretState::Set,
            settings,
            password_state: state.as_str(),
        }
    }
}

/// 1 件の平らな応答（その接続の秘密の行を読む）。
pub(super) async fn flat_connection_response(
    service: &PlcConnectionService,
    conn: PlcConnection,
) -> Result<FlatPlcConnectionResponse, BantoError> {
    let rows = service.secret_rows(conn.id).await?;
    Ok(FlatPlcConnectionResponse::build(conn, &rows))
}

/// 全件の平らな応答（接続と秘密の行を 1 つの読み取りのトランザクションで読む）。
pub(super) async fn flat_connection_responses(
    service: &PlcConnectionService,
) -> Result<Vec<FlatPlcConnectionResponse>, BantoError> {
    let (connections, rows) = service.list_with_secret_rows().await?;
    Ok(connections
        .into_iter()
        .map(|conn| {
            let own: Vec<banto_tags::StoredSecretRow> = rows
                .iter()
                .filter(|row| row.connection_id == conn.id)
                .cloned()
                .collect();
            FlatPlcConnectionResponse::build(conn, &own)
        })
        .collect())
}

/// MCP の `test_connection` の平らな入力（R2b-6b より前の REST の `PlcConnectionTestPayload`
/// と同じ形）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct FlatPlcConnectionTestPayload {
    protocol: String,
    host: String,
    port: i64,
    #[serde(default = "default_unit_id")]
    unit_id: i64,
    #[serde(default)]
    simulation: bool,
    #[serde(default)]
    pub(super) connection_id: Option<i64>,
    #[serde(default)]
    cpu_type: Option<String>,
}

impl FlatPlcConnectionTestPayload {
    /// `settings` の形に写す（モジュールの doc）。`saved` は `connectionId` の保存済みの行。
    pub(super) fn into_test_payload(
        self,
        saved: Option<&PlcConnection>,
    ) -> PlcConnectionTestPayload {
        let saved = saved.filter(|conn| conn.protocol == self.protocol);
        let input = PlcConnectionInput {
            name: String::new(),
            protocol: self.protocol.clone(),
            host: self.host,
            port: self.port,
            unit_id: self.unit_id,
            enabled: true,
            simulation: false,
            word_order: saved
                .map(|conn| conn.word_order.clone())
                .unwrap_or_default(),
            cpu_type: self
                .cpu_type
                .filter(|cpu| !cpu.is_empty())
                .or_else(|| saved.map(|conn| conn.cpu_type.clone())),
            database: None,
            username: None,
            secrets: Default::default(),
            settings: None,
        };
        let mut settings: Map<String, Value> =
            banto_tags::settings_json::legacy::settings_from_flat(&input);
        if settings
            .get("word_order")
            .and_then(Value::as_str)
            .is_some_and(str::is_empty)
        {
            settings.remove("word_order");
        }
        PlcConnectionTestPayload {
            protocol: self.protocol,
            simulation: self.simulation,
            settings,
            connection_id: self.connection_id,
        }
    }
}
