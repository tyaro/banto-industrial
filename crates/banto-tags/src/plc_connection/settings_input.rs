//! `settings` の形の書き込みの入口（R2b-6b、docs/banto-hub-driver-ui-design.md §4.2・§5）。
//!
//! Hub の REST（`/api/plc-connections`）は R2b-6b から接続を
//! `{ name, protocol, enabled, simulation, settings }` で受ける。この入口は
//! [`ConnectionSettingsInput`] を受けて、プロトコルで 2 つに分ける:
//!
//! - **機器のドライバ**（レジストリの `Frame` / `Session`。今は `modbus-tcp`・`slmp`）:
//!   [`crate::settings_from_schema`]（種類・制約・必須・既定値・未知のキーの断り）→ 書き込みの
//!   トランザクションの中で秘密の参照を合わせる → [`crate::settings_to_api`] →
//!   `validate_config`。平らな形の旧い検証 (1) は通らない。項目エラーの名前は
//!   `settings.<key>`（`settings.unit_id`。平らな形の `unitId` ではない）。
//! - **ホストの接続**（`postgres`・`push`・`virtual`・`hub`。スキーマは空）: `settings` を
//!   今の平らな入力（[`PlcConnectionInput`]）に写して、今の書き込み（専用の検証と
//!   「省略は保つ」）をそのまま通す。`postgres` は `host`・`port`・`database`・`username`
//!   だけを受け（型も確かめる）、`push` は `settings` のオブジェクトをそのまま、`virtual`・
//!   `hub` は空だけを受ける。
//!
//! **秘密値は `settings` に載らない**: 呼び手（Hub）が秘密のキーを `settings` から外して
//! 封印し、[`ConnectionSettingsInput::secrets`] に写す（オーナー決定 2026-10-24: 省略 = 保つ、
//! `null`・空文字 = 消す、文字列 = 置き換え）。秘密のキーが `settings` に残っていたら断る。
//! スキーマで必須の秘密の欄は、書き込みの最後の `settings` に参照が無ければ「必須です」で
//! 断る（作成で欠けた・消した・更新で保つが保存済みの値も無い、のどれも）。
//!
//! レジストリを持つサービス（[`PlcConnectionService::with_registry`]、Hub）だけが使う。

use banto_core::{BantoError, FieldError};
use banto_driver_api::{DriverKind, FieldKind};
use serde_json::{Map, Value};
use sqlx::SqliteConnection;

use super::{
    PlcConnection, PlcConnectionInput, PlcConnectionService, ALLOWED_PROTOCOLS, HUB_PROTOCOL,
    MAX_NAME_LEN, POSTGRES_PROTOCOL, PUSH_PROTOCOL, VIRTUAL_PROTOCOL,
};
use crate::connection_secret::{SecretChange, SecretChanges};
use crate::support::{max_length_message, required_message};

/// `settings` の形の作成・更新の入力（モジュールの doc）。
#[derive(Debug, Clone, Default)]
pub struct ConnectionSettingsInput {
    pub name: String,
    pub protocol: String,
    pub enabled: bool,
    pub simulation: bool,
    /// 秘密のキーを除いた `settings`（JSON のオブジェクト）。
    pub settings: Map<String, Value>,
    /// 秘密のキーごとの変更（**封印済み**。無いキーは `Keep`）。
    pub secrets: SecretChanges,
}

/// 機器のドライバの `settings`（[`crate::settings_from_schema`] を通ったもの）と、スキーマで
/// 必須の秘密のキー。`create_in` / `update_in` に渡す。
#[derive(Debug, Clone)]
pub(super) struct TypedSettings {
    pub(super) settings: Map<String, Value>,
    pub(super) required_secrets: Vec<String>,
}

enum Route {
    /// ホストの接続: 平らな入力に写したもの（今の書き込みを通す）。
    Host(PlcConnectionInput),
    /// 機器のドライバ: 共通の欄だけの平らな入力（平らな欄は使わない）とスキーマを通した `settings`。
    Driver(PlcConnectionInput, TypedSettings),
}

impl PlcConnectionService {
    /// `settings` の形で作る（モジュールの doc）。呼び出し側のトランザクションの中で。
    pub async fn create_settings_tx(
        &self,
        connection: &mut SqliteConnection,
        input: ConnectionSettingsInput,
    ) -> Result<PlcConnection, BantoError> {
        match self.route(input)? {
            Route::Host(flat) => self.create_tx(connection, flat).await,
            Route::Driver(flat, typed) => self.create_in(connection, flat, Some(typed)).await,
        }
    }

    /// [`Self::create_settings_tx`] を自前の `BEGIN IMMEDIATE` のトランザクションで。
    pub async fn create_settings(
        &self,
        input: ConnectionSettingsInput,
    ) -> Result<PlcConnection, BantoError> {
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(banto_storage::storage_error)?;
        let created = self.create_settings_tx(&mut tx, input).await?;
        tx.commit().await.map_err(banto_storage::storage_error)?;
        Ok(created)
    }

    /// `settings` の形で `id` を置き換える（モジュールの doc）。`settings` は丸ごと置き換え
    /// （機器のドライバは省いた欄が既定値になる）、秘密のキーは `secrets` に従う。
    pub async fn update_settings_tx(
        &self,
        connection: &mut SqliteConnection,
        id: i64,
        input: ConnectionSettingsInput,
    ) -> Result<PlcConnection, BantoError> {
        let simulation = input.simulation;
        self.update_settings_optional_simulation_tx(connection, id, input, Some(simulation))
            .await
    }

    /// Update while retaining the stored simulation value when it was omitted.
    /// The choice is applied in the UPDATE statement inside the caller's transaction.
    pub async fn update_settings_preserving_simulation_tx(
        &self,
        connection: &mut SqliteConnection,
        id: i64,
        input: ConnectionSettingsInput,
    ) -> Result<PlcConnection, BantoError> {
        self.update_settings_optional_simulation_tx(connection, id, input, None)
            .await
    }

    async fn update_settings_optional_simulation_tx(
        &self,
        connection: &mut SqliteConnection,
        id: i64,
        input: ConnectionSettingsInput,
        simulation: Option<bool>,
    ) -> Result<PlcConnection, BantoError> {
        match self.route(input)? {
            Route::Host(flat) => self.update_in(connection, id, flat, simulation, None).await,
            Route::Driver(flat, typed) => {
                self.update_in(connection, id, flat, simulation, Some(typed))
                    .await
            }
        }
    }

    /// `update_settings_tx` in its own immediate transaction.
    pub async fn update_settings(
        &self,
        id: i64,
        input: ConnectionSettingsInput,
    ) -> Result<PlcConnection, BantoError> {
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(banto_storage::storage_error)?;
        let updated = self.update_settings_tx(&mut tx, id, input).await?;
        tx.commit().await.map_err(banto_storage::storage_error)?;
        Ok(updated)
    }

    /// `update_settings_preserving_simulation_tx` in its own immediate transaction.
    pub async fn update_settings_preserving_simulation(
        &self,
        id: i64,
        input: ConnectionSettingsInput,
    ) -> Result<PlcConnection, BantoError> {
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(banto_storage::storage_error)?;
        let updated = self
            .update_settings_preserving_simulation_tx(&mut tx, id, input)
            .await?;
        tx.commit().await.map_err(banto_storage::storage_error)?;
        Ok(updated)
    }

    /// 未適用キューに積む前に、適用時（[`Self::create_settings_tx`] /
    /// [`Self::update_settings_tx`]）と同じ規則で `settings` の形を検査する（平文の秘密値を
    /// キューに残さない）。DB には触らない。ホストの接続（push・postgres など）は適用時の
    /// 平らな検証（`validate_plc_connection_input`。`push` の `settings` の中身も含む）も通す。
    /// 機器のドライバの必須の秘密の欠けは、更新で保存済みを保つ場合があるので見ない。
    pub fn check_settings_input(&self, input: &ConnectionSettingsInput) -> Result<(), BantoError> {
        match self.route(input.clone())? {
            Route::Host(flat) => {
                let flat = super::normalize_plc_connection_input(flat);
                let secret_keys = self.secret_keys(&flat.protocol);
                super::validate_plc_connection_input(&flat, &secret_keys)
            }
            Route::Driver(..) => Ok(()),
        }
    }

    fn route(&self, input: ConnectionSettingsInput) -> Result<Route, BantoError> {
        let Some(registry) = &self.registry else {
            return Err(validation_error(vec![FieldError {
                field: "protocol".to_string(),
                message:
                    "settings の形の書き込みはドライバのレジストリを持つサービスだけが受けます"
                        .to_string(),
            }]));
        };
        let entry = registry.get(&input.protocol);
        let is_device = entry.as_ref().is_some_and(|entry| {
            matches!(entry.kind(), DriverKind::Frame(_) | DriverKind::Session(_))
        });
        if let (true, Some(entry)) = (is_device, entry) {
            let base = entry.base();
            return driver_route(input, &base.settings_schema(), &base.secret_keys());
        }
        if entry.is_none() && !ALLOWED_PROTOCOLS.contains(&input.protocol.as_str()) {
            // 平らな形の検証に回すと、知らないプロトコルに host・port の欠けまで並ぶ。
            return Err(validation_error(crate::validate_settings_with_registry(
                registry,
                &input.protocol,
                &Map::new(),
            )));
        }
        host_route(input).map(Route::Host)
    }
}

fn driver_route(
    input: ConnectionSettingsInput,
    schema: &banto_driver_api::SettingsSchema,
    secret_keys: &[String],
) -> Result<Route, BantoError> {
    let mut errors = name_errors(&input.name);
    errors.extend(
        secret_key_errors(&input, secret_keys)
            .into_iter()
            .map(|mut error| {
                error.field = format!("settings.{}", error.field);
                error
            }),
    );
    let mut typed_settings = Map::new();
    match crate::settings_from_schema(schema, &input.settings) {
        Ok(settings) => typed_settings = settings,
        Err(field_errors) => errors.extend(field_errors.into_iter().map(|error| FieldError {
            field: error.key,
            message: error.message,
        })),
    }
    if !errors.is_empty() {
        return Err(validation_error(errors));
    }
    let required_secrets = schema
        .fields
        .iter()
        .filter(|field| field.required && matches!(field.kind, FieldKind::Secret))
        .map(|field| field.key.clone())
        .collect();
    let flat = PlcConnectionInput {
        name: input.name,
        protocol: input.protocol,
        host: String::new(),
        port: 0,
        unit_id: 1,
        enabled: input.enabled,
        simulation: input.simulation,
        word_order: String::new(),
        cpu_type: None,
        database: None,
        username: None,
        secrets: input.secrets,
        settings: None,
    };
    Ok(Route::Driver(
        flat,
        TypedSettings {
            settings: typed_settings,
            required_secrets,
        },
    ))
}

/// ホストの接続の `settings` を平らな入力に写す（モジュールの doc）。
fn host_route(input: ConnectionSettingsInput) -> Result<PlcConnectionInput, BantoError> {
    let protocol = input.protocol.as_str();
    let mut errors = Vec::new();
    let mut flat = PlcConnectionInput {
        name: input.name.clone(),
        protocol: input.protocol.clone(),
        host: String::new(),
        port: 0,
        unit_id: 1,
        enabled: input.enabled,
        simulation: input.simulation,
        word_order: String::new(),
        cpu_type: None,
        database: None,
        username: None,
        secrets: input.secrets.clone(),
        settings: None,
    };
    if protocol == POSTGRES_PROTOCOL {
        for (key, value) in &input.settings {
            match key.as_str() {
                "host" | "database" | "username" => match value {
                    Value::String(text) => match key.as_str() {
                        "host" => flat.host = text.clone(),
                        "database" => flat.database = Some(text.clone()),
                        _ => flat.username = Some(text.clone()),
                    },
                    Value::Null => {}
                    _ => errors.push(FieldError {
                        field: key.clone(),
                        message: "文字列で指定してください".to_string(),
                    }),
                },
                "port" => match value {
                    Value::Null => {}
                    value => match value.as_i64() {
                        Some(port) => flat.port = port,
                        None => errors.push(FieldError {
                            field: key.clone(),
                            message: "整数で指定してください".to_string(),
                        }),
                    },
                },
                crate::PASSWORD_KEY => errors.push(plaintext_secret_error(key)),
                _ => errors.push(unknown_key_error(key)),
            }
        }
    } else if protocol == PUSH_PROTOCOL {
        flat.settings = Some(Value::Object(input.settings.clone()));
    } else if (protocol == VIRTUAL_PROTOCOL || protocol == HUB_PROTOCOL)
        && !input.settings.is_empty()
    {
        errors.push(FieldError {
            field: "settings".to_string(),
            message: format!("{protocol} 接続は設定を持ちません"),
        });
    }
    if errors.is_empty() {
        Ok(flat)
    } else {
        Err(validation_error(errors))
    }
}

/// 名前の検査（`validate_plc_connection_input` と同じ規則・文言）。
fn name_errors(name: &str) -> Vec<FieldError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        vec![FieldError {
            field: "name".to_string(),
            message: required_message(),
        }]
    } else if trimmed.chars().count() > MAX_NAME_LEN {
        vec![FieldError {
            field: "name".to_string(),
            message: max_length_message(MAX_NAME_LEN),
        }]
    } else {
        Vec::new()
    }
}

/// 秘密のキーの取り違え: ドライバの秘密のキーでないキーへの `Set`、`settings` に残った
/// 秘密のキー（平文を保存しない。呼び手が外して封印する約束）。
fn secret_key_errors(input: &ConnectionSettingsInput, secret_keys: &[String]) -> Vec<FieldError> {
    let mut errors = Vec::new();
    for (key, change) in &input.secrets {
        if matches!(change, SecretChange::Set(_)) && !secret_keys.iter().any(|k| k == key) {
            errors.push(FieldError {
                field: key.clone(),
                message: format!("{} 接続では {key} を指定できません", input.protocol),
            });
        }
    }
    for key in secret_keys {
        if input.settings.contains_key(key) {
            errors.push(plaintext_secret_error(key));
        }
    }
    errors
}

fn plaintext_secret_error(key: &str) -> FieldError {
    FieldError {
        field: key.to_string(),
        message: "秘密値は封印した参照としてだけ保存できます（平文は保存しません）".to_string(),
    }
}

fn unknown_key_error(key: &str) -> FieldError {
    FieldError {
        field: key.to_string(),
        message: "このプロトコルの設定に無い欄です".to_string(),
    }
}

fn validation_error(field_errors: Vec<FieldError>) -> BantoError {
    BantoError::Validation { field_errors }
}

/// スキーマで必須の秘密の欄のうち、最後の `settings` に参照が無いもの（モジュールの doc）。
pub(super) fn missing_required_secrets(
    settings: &Map<String, Value>,
    required_secrets: &[String],
) -> Vec<FieldError> {
    required_secrets
        .iter()
        .filter(|key| !settings.contains_key(key.as_str()))
        .map(|key| FieldError {
            field: format!("settings.{key}"),
            // `settings_from_schema` の必須の欠けと同じ文言。
            message: "必須です".to_string(),
        })
        .collect()
}

/// 最終検証の項目名を、ドライバの接続が公開する `settings.<key>` に揃える。
pub(super) fn with_settings_keys(error: BantoError) -> BantoError {
    match error {
        BantoError::Validation { field_errors } => BantoError::Validation {
            field_errors: field_errors
                .into_iter()
                .map(|error| FieldError {
                    field: match error.field.as_str() {
                        "unitId" => "settings.unit_id".to_string(),
                        "wordOrder" => "settings.word_order".to_string(),
                        "cpuType" => "settings.cpu_type".to_string(),
                        "protocol" | "settings" => error.field,
                        field if field.starts_with("settings.") => error.field,
                        _ => format!("settings.{}", error.field),
                    },
                    message: error.message,
                })
                .collect(),
        },
        other => other,
    }
}

#[cfg(test)]
mod tests;
