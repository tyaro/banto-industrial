//! 接続の `settings`（JSON）⇔ ドライバ API の `Settings`、平らな形 ⇔ `settings`、
//! レジストリによる検証（R2b-1c、docs/banto-hub-connection-settings-design.md
//! §3.2・§3.3・§3.5・§4.2・§4.3）。
//!
//! The legacy host input adapter writes validated settings to the JSON column.
//! Registry-backed readers use the JSON settings directly.
//!
//! ## [`settings_to_api`]（設計 §3.5）
//!
//! `banto_driver_api::Settings` は文字列の対しか持てない（入れ子を持てない）ので、
//! JSON の値を次の規則で文字列にする。**API は変えず、ホストの純関数 1 つで変換する**。
//!
//! | JSON の値          | `Settings` の値                                      |
//! | ------------------ | ---------------------------------------------------- |
//! | 文字列             | そのまま                                             |
//! | 整数・小数         | 10 進の文字列（`serde_json` の表記）                 |
//! | `true` / `false`   | `"true"` / `"false"`                                 |
//! | オブジェクト・配列 | 詰めた JSON の文字列（R2a の `push` の `bridge`）    |
//! | 秘密の参照         | **渡さない**（検証には渡らない。接続にだけ開封して渡す） |
//! | `null`             | キーを渡さない                                       |
//!
//! **秘密の参照の見分け方**: キーを**ちょうど 1 つ** `"$secret"` だけ持ち、その値が
//! **文字列**のオブジェクト（`{"$secret": "<ref>"}`、設計 §3.3）。キーが増えている・値が
//! 文字列でない・`"$secret"` 以外のキーのオブジェクトは秘密の参照ではなく、ふつうの
//! オブジェクトとして詰めた JSON の文字列になる（秘密のキーに平文を置く取りこぼしを
//! 断るのは保存の層: `crate::connection_secret::check_settings_shape`、R2b-1d）。`serde_json` の `Map` は既定では順序を持たない（キーの
//! 辞書順）ので、`Settings` の並びも辞書順になる。並びに意味を持たせる呼び出し側は
//! 無い（検証は `get` で引く）。
//!
//! ## [`legacy`]
//!
//! The flat host input adapter remains for host connections and tests.
//!
//! ## [`validate_settings_with_registry`]（設計 §4.2 の (3)(4)）
//!
//! `registry.get(protocol)` が無ければ `protocol` の項目エラー、あれば
//! [`settings_to_api`] → `validate_config` → `FieldError.key` を
//! `settings.<key>` に写して返す。旧い平らな形の検証（(1)）は呼ばない
//! （`plc_connection.rs` の `validate_plc_connection_input`）。

use banto_core::FieldError;
use banto_driver_api::{
    DriverRegistry, FieldError as DriverFieldError, FieldKind, SettingField, SettingValue,
    Settings, SettingsSchema,
};
use serde_json::{Map, Value};

/// 秘密の参照のキー（設計 §3.3 `{"$secret": "<ref>"}`）。
pub const SECRET_REF_KEY: &str = "$secret";

/// `value` が秘密の参照か（モジュールの doc: キーがちょうど `"$secret"` 1 つで、値が文字列の
/// オブジェクト）。
pub fn is_secret_ref(value: &Value) -> bool {
    match value {
        Value::Object(object) => {
            object.len() == 1 && matches!(object.get(SECRET_REF_KEY), Some(Value::String(_)))
        }
        _ => false,
    }
}

/// 接続の `settings`（JSON のオブジェクト）をドライバ API の [`Settings`] に直す
/// （設計 §3.5、モジュールの doc の表）。純関数。
pub fn settings_to_api(map: &Map<String, Value>) -> Settings {
    let mut settings = Settings::new();
    for (key, value) in map {
        match value {
            Value::Null => {}
            Value::String(text) => settings.insert(key.as_str(), text.as_str()),
            Value::Number(number) => settings.insert(key.as_str(), number.to_string()),
            Value::Bool(flag) => settings.insert(key.as_str(), flag.to_string()),
            Value::Object(_) | Value::Array(_) => {
                if !is_secret_ref(value) {
                    settings.insert(key.as_str(), value.to_string());
                }
            }
        }
    }
    settings
}

/// `FieldError.key`（`Settings` のキー）を接続の `settings.<key>` に写す。
///
/// `push` の入れ子（`bridge.queries[0].sql: ...` のように文言の先頭へ残された位置、
/// `ingest/driver.rs` の `to_driver_field_error`）は、`settings.bridge.queries[0].sql` の
/// 項目名と元の文言に戻す。
fn rest_field_error(protocol: &str, key: &str, message: String) -> FieldError {
    if protocol == crate::PUSH_PROTOCOL {
        if let Some(rest) = message.strip_prefix(key) {
            if rest.starts_with(['.', '[']) {
                if let Some((path, inner)) = message.split_once(": ") {
                    return FieldError {
                        field: format!("settings.{path}"),
                        message: inner.to_string(),
                    };
                }
            }
        }
        return FieldError {
            field: format!("settings.{key}"),
            message,
        };
    }
    FieldError {
        field: format!("settings.{key}"),
        message,
    }
}

/// 設計 §4.2 の (3)(4): `protocol` をレジストリで引き、`settings` をそのドライバの
/// `validate_config` に掛けて、項目エラーを REST の綴りで返す。空なら通った。
///
/// `registry.get(protocol)` が無ければ `protocol` の項目エラー 1 つだけ
/// （`validate_plc_connection_input` の文言と同じ形で、レジストリの id を並べる）。
/// 平らな形の旧い検証は含まない。
pub fn validate_settings_with_registry(
    registry: &DriverRegistry,
    protocol: &str,
    settings: &Map<String, Value>,
) -> Vec<FieldError> {
    let Some(entry) = registry.get(protocol) else {
        let ids: Vec<&str> = registry.ids().map(|id| id.as_str()).collect();
        return vec![FieldError {
            field: "protocol".to_string(),
            message: format!("対応プロトコルは {} のいずれかです", ids.join(", ")),
        }];
    };
    entry
        .base()
        .validate_config(&settings_to_api(settings))
        .into_iter()
        .map(|error| rest_field_error(protocol, &error.key, error.message))
        .collect()
}

/// スキーマに従って、汎用フォームの入力（JSON のオブジェクト）から接続の `settings` を作る
/// （R2b-6b、docs/banto-hub-driver-ui-design.md §4.2）。純関数。この後に今までどおり
/// [`settings_to_api`] → `validate_config` を通す（欄どうしの整合はドライバが見る）。
///
/// - **種類**: キーごとに [`FieldKind`] と JSON の型を合わせる。`Integer` は JSON の整数だけ
///   （`"502"`・`502.5` は断る。文字列から数への読み替えはしない）、`Text`・`Host`・`Choice`
///   は文字列、`Bool` は真偽値。
/// - **制約**: `Integer` の `min`・`max`、`Text` の `max_len`（文字数）、`Choice` の選択肢。
/// - **必須**: 無い（または `null`）欄は既定値を埋める。既定値が無い必須の欄は欠けとして
///   報告する。`Text`・`Host` の必須の欄は空白だけの文字列も欠けとみなす。
/// - **スキーマに無いキーは断る**（ドライバの `validate_config` は未知のキーを断らない約束なので、
///   入口で止める。Q9）。
/// - **秘密のキー**（`Secret`）は tri-state をそのまま出力に残す: **省略 = 保つ**（出力にも
///   キーが無い）、**`null` = 消す**（出力は `null`）、**文字列 = 置き換え**（出力はその文字列。
///   平文なので、呼び手が保存の前に封印して `SecretChange` に写し、`settings` からは外す）。
///   既定値は埋めない。必須の秘密の欄に `null` は欠けとして断る（省略は今の値を保つので、
///   作成で値が要るかは呼び手が今の行と合わせて見る）。
///
/// エラーは `banto_driver_api::FieldError`（キーは `settings.<key>`）で、スキーマの欄の順、
/// その後に未知のキーを並べる。`schema` が空なら、空でない入力は全部未知のキー。
pub fn settings_from_schema(
    schema: &SettingsSchema,
    input: &Map<String, Value>,
) -> Result<Map<String, Value>, Vec<DriverFieldError>> {
    let mut settings = Map::new();
    let mut errors = Vec::new();
    for field in &schema.fields {
        let key = field.key.as_str();
        let value = input.get(key);
        if matches!(field.kind, FieldKind::Secret) {
            match value {
                None => {}
                Some(Value::Null) if field.required => {
                    errors.push(DriverFieldError::new(
                        format!("settings.{key}"),
                        REQUIRED_MESSAGE,
                    ));
                }
                Some(Value::Null) => {
                    settings.insert(key.to_string(), Value::Null);
                }
                Some(Value::String(text)) => {
                    settings.insert(key.to_string(), Value::String(text.clone()));
                }
                Some(_) => errors.push(DriverFieldError::new(
                    format!("settings.{key}"),
                    "文字列か null で指定してください",
                )),
            }
            continue;
        }
        let value = match value {
            None | Some(Value::Null) => {
                match &field.default {
                    Some(default) => {
                        settings.insert(key.to_string(), setting_value_json(default));
                    }
                    None if field.required => {
                        errors.push(DriverFieldError::new(
                            format!("settings.{key}"),
                            REQUIRED_MESSAGE,
                        ));
                    }
                    None => {}
                }
                continue;
            }
            Some(value) => value,
        };
        match check_field_value(field, value) {
            Ok(()) => {
                // `Host` は前後の空白を落として持つ（エンジンの TCP 接続は名前解決に
                // そのまま渡すので、`" 127.0.0.1 "` は繋がらない。旧い平らな形も trim
                // していた）。`Text`・`Secret` は値そのものなので触らない。
                let value = match (&field.kind, value) {
                    (FieldKind::Host, Value::String(text)) => Value::String(text.trim().into()),
                    _ => value.clone(),
                };
                settings.insert(key.to_string(), value);
            }
            Err(message) => errors.push(DriverFieldError::new(format!("settings.{key}"), message)),
        }
    }
    for key in input.keys() {
        if schema.field(key).is_none() {
            errors.push(DriverFieldError::new(
                format!("settings.{key}"),
                "このドライバの設定に無い欄です",
            ));
        }
    }
    if errors.is_empty() {
        Ok(settings)
    } else {
        Err(errors)
    }
}

/// [`settings_from_schema`] の必須の欠けの文言。
const REQUIRED_MESSAGE: &str = "必須です";

/// スキーマの既定値を `settings` の JSON の値にする（`Integer` は数、`Bool` は真偽値）。
fn setting_value_json(value: &SettingValue) -> Value {
    match value {
        SettingValue::Text(text) => Value::String(text.clone()),
        SettingValue::Integer(n) => Value::from(*n),
        SettingValue::Bool(flag) => Value::Bool(*flag),
        // `SettingValue` は `#[non_exhaustive]`。後の版で増えた種類はその文字の形で持つ。
        #[allow(unreachable_patterns)]
        other => Value::String(other.to_text()),
    }
}

/// `value`（`null` でない）が `field` の種類と制約に合うか。合わなければその文言。
fn check_field_value(field: &SettingField, value: &Value) -> Result<(), String> {
    let text = || match value {
        Value::String(text) => Ok(text.as_str()),
        _ => Err("文字列で指定してください".to_string()),
    };
    match &field.kind {
        FieldKind::Text { max_len, .. } => {
            let text = text()?;
            if field.required && text.trim().is_empty() {
                return Err(REQUIRED_MESSAGE.to_string());
            }
            match max_len {
                Some(max) if text.chars().count() > *max as usize => {
                    Err(format!("{max} 文字以下にしてください"))
                }
                _ => Ok(()),
            }
        }
        FieldKind::Host => {
            let text = text()?;
            if field.required && text.trim().is_empty() {
                return Err(REQUIRED_MESSAGE.to_string());
            }
            Ok(())
        }
        FieldKind::Integer { min, max } => {
            let Some(n) = value.as_i64() else {
                return Err("整数で指定してください".to_string());
            };
            match (min, max) {
                (Some(min), Some(max)) if n < *min || n > *max => {
                    Err(format!("{min}〜{max} で指定してください"))
                }
                (Some(min), None) if n < *min => Err(format!("{min} 以上で指定してください")),
                (None, Some(max)) if n > *max => Err(format!("{max} 以下で指定してください")),
                _ => Ok(()),
            }
        }
        FieldKind::Choice { options } => {
            let text = text()?;
            if options.iter().any(|option| option.value == text) {
                Ok(())
            } else {
                let values: Vec<&str> = options.iter().map(|o| o.value.as_str()).collect();
                Err(format!(
                    "{} のいずれかを指定してください",
                    values.join(", ")
                ))
            }
        }
        FieldKind::Bool => match value {
            Value::Bool(_) => Ok(()),
            _ => Err("true か false で指定してください".to_string()),
        },
        // 呼び手（settings_from_schema）が先に扱う。
        FieldKind::Secret => Ok(()),
        // `FieldKind` は `#[non_exhaustive]`。このホストが知らない種類の欄は確かめられないので
        // 断る（素通しにすると、スキーマが約束した制約を見ないまま保存する）。
        #[allow(unreachable_patterns)]
        _ => Err("このホストが扱えない種類の欄です".to_string()),
    }
}

/// ホスト接続とテスト用の平らな入力を `settings` に写すアダプタ（設計 §5、Q3）。
pub mod legacy {
    use super::{validate_settings_with_registry, DriverRegistry, FieldError, Map, Value};
    use crate::plc_connection::{
        normalize_plc_connection_input, PlcConnectionInput, MODBUS_PROTOCOL, POSTGRES_PROTOCOL,
        PUSH_PROTOCOL,
    };

    /// `slmp` の綴り（`ALLOWED_PROTOCOLS` と同じ。定数は無い）。
    pub(crate) const SLMP_PROTOCOL: &str = "slmp";

    /// 正規化（`normalize_plc_connection_input`）と旧い検証を通った入力を、設計 §3.2 の
    /// 表のとおりに `settings` へ写す。**プロトコルが読まない欄は写さない**（捨てる）。
    ///
    /// | `protocol`   | `settings` のキー                                     |
    /// | ------------ | ----------------------------------------------------- |
    /// | `modbus-tcp` | `host`・`port`・`unit_id`・`word_order`               |
    /// | `slmp`       | `host`・`port`・`word_order`・`cpu_type`              |
    /// | `postgres`   | `host`・`port`・`database`・`username`（`password` は下）|
    /// | `virtual`    | `{}`                                                  |
    /// | `hub`        | `{}`                                                  |
    /// | `push`       | `input.settings` をそのまま（`None`・`null` は `{}`） |
    /// | その他       | `{}`（レジストリが断る）                              |
    ///
    /// - `host`・`database`・`username` は保存と同じく前後の空白を除く。
    /// - `slmp` の `cpu_type` は、入力が `None`・空のとき**キーを置かない**。
    ///   書き込みの入口が作成時の既定値 `R`、更新時の既存値を合わせる。
    /// - **秘密値（`postgres` の `password`）は写さない**。`settings` には
    ///   `{"$secret": "<ref>"}` しか置かない（設計 §3.3）。参照は書き込みの入口が
    ///   `PlcConnectionInput::secrets`（封印済みの値）から挿す（R2b-1d）。
    /// - `push` で `settings` がオブジェクトでないとき（検証が先に断る入力）は `{}`。
    ///   この関数は検証の後に呼ぶ前提で、断る責任は旧い検証が持つ。
    pub fn settings_from_flat(input: &PlcConnectionInput) -> Map<String, Value> {
        let mut map = Map::new();
        let protocol = input.protocol.as_str();
        if protocol == MODBUS_PROTOCOL {
            map.insert("host".into(), input.host.trim().into());
            map.insert("port".into(), input.port.into());
            map.insert("unit_id".into(), input.unit_id.into());
            map.insert("word_order".into(), input.word_order.clone().into());
        } else if protocol == SLMP_PROTOCOL {
            map.insert("host".into(), input.host.trim().into());
            map.insert("port".into(), input.port.into());
            map.insert("word_order".into(), input.word_order.clone().into());
            if let Some(cpu_type) = input.cpu_type.as_deref().filter(|s| !s.is_empty()) {
                map.insert("cpu_type".into(), cpu_type.into());
            }
        } else if protocol == POSTGRES_PROTOCOL {
            map.insert("host".into(), input.host.trim().into());
            map.insert("port".into(), input.port.into());
            if let Some(database) = input.database.as_deref() {
                map.insert("database".into(), database.trim().into());
            }
            if let Some(username) = input.username.as_deref() {
                map.insert("username".into(), username.trim().into());
            }
        } else if protocol == PUSH_PROTOCOL {
            if let Some(Value::Object(settings)) = &input.settings {
                map = settings.clone();
            }
        }
        // `virtual`・`hub`・知らないプロトコルは `{}`。
        map
    }

    /// 設計 §4.2 の (2)〜(4) を平らな入力から続けて行う: 正規化 → [`settings_from_flat`] →
    /// [`super::validate_settings_with_registry`]。旧い平らな形の検証 (1) は含まない。
    /// 保存の入口（`PlcConnectionService::with_registry`）は同じ (2)〜(4) を、「省略は保つ」を
    /// 合わせた**最終の** `settings` に掛ける。これは旧い検証とレジストリの検証が同じ入力を
    /// 断ることの表のテスト（`apps/banto-hub/host/tests/connection_validation_consistency.rs`）
    /// が呼ぶ。
    pub fn validate_flat_with_registry(
        registry: &DriverRegistry,
        input: PlcConnectionInput,
    ) -> Vec<FieldError> {
        let input = normalize_plc_connection_input(input);
        let settings = settings_from_flat(&input);
        validate_settings_with_registry(registry, &input.protocol, &settings)
    }
}

#[cfg(test)]
mod tests {
    use super::legacy::settings_from_flat;
    use super::*;
    use crate::plc_connection::PlcConnectionInput;
    use serde_json::json;

    fn object(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            other => panic!("not an object: {other}"),
        }
    }

    fn pairs(settings: &Settings) -> Vec<(String, String)> {
        let mut pairs: Vec<(String, String)> = settings
            .iter()
            .map(|s| (s.key.clone(), s.value.clone()))
            .collect();
        pairs.sort();
        pairs
    }

    fn p(key: &str, value: &str) -> (String, String) {
        (key.to_string(), value.to_string())
    }

    // ---- JSON ⇔ Settings（設計 §3.5 の表 1 行 1 ケース）----

    #[test]
    fn json_to_settings_follows_the_design_table_row_by_row() {
        // (行の名前, JSON の値, `Settings` に出るキーの値。None はキーが出ない)
        let rows: Vec<(&str, Value, Option<&str>)> = vec![
            ("string", json!("high_low"), Some("high_low")),
            ("empty string", json!(""), Some("")),
            ("integer", json!(502), Some("502")),
            ("negative integer", json!(-1), Some("-1")),
            ("float", json!(1.5), Some("1.5")),
            ("true", json!(true), Some("true")),
            ("false", json!(false), Some("false")),
            (
                "object",
                json!({"b": 1, "a": [1, 2]}),
                Some(r#"{"a":[1,2],"b":1}"#),
            ),
            ("array", json!([1, "x", null]), Some(r#"[1,"x",null]"#)),
            (
                "secret reference",
                json!({"$secret": "0123456789abcdef0123456789abcdef"}),
                None,
            ),
            ("null", Value::Null, None),
        ];
        for (name, value, want) in rows {
            let mut map = Map::new();
            map.insert("k".to_string(), value);
            let settings = settings_to_api(&map);
            assert_eq!(settings.get("k"), want, "{name}");
            assert_eq!(
                settings.iter().count(),
                usize::from(want.is_some()),
                "{name}"
            );
        }
    }

    #[test]
    fn only_an_object_with_exactly_one_string_secret_key_is_a_secret_reference() {
        let secret = json!({"$secret": "ref"});
        assert!(is_secret_ref(&secret));
        for not_secret in [
            json!({"$secret": 5}),
            json!({"$secret": null}),
            json!({"$secret": "ref", "extra": 1}),
            json!({"$other": "ref"}),
            json!({}),
            json!("$secret"),
            json!(["$secret"]),
        ] {
            assert!(!is_secret_ref(&not_secret), "{not_secret}");
        }
        // 秘密の参照ではない `$secret` つきのオブジェクトは、ふつうの JSON の文字列になる。
        let map = object(json!({"k": {"$secret": "ref", "extra": 1}}));
        assert_eq!(
            settings_to_api(&map).get("k"),
            Some(r#"{"$secret":"ref","extra":1}"#)
        );
    }

    #[test]
    fn settings_to_api_keeps_every_key_once_and_skips_nulls_and_secrets_together() {
        let map = object(json!({
            "host": "plc.local",
            "port": 502,
            "password": {"$secret": "ref"},
            "gone": null,
            "bridge": {"kind": "db"},
        }));
        assert_eq!(
            pairs(&settings_to_api(&map)),
            [
                p("bridge", r#"{"kind":"db"}"#),
                p("host", "plc.local"),
                p("port", "502"),
            ]
        );
        assert_eq!(settings_to_api(&Map::new()), Settings::new());
    }

    // ---- §3.2 の写し方（プロトコルごと、捨てる欄を含む）----

    fn input(protocol: &str) -> PlcConnectionInput {
        PlcConnectionInput {
            name: "c".to_string(),
            protocol: protocol.to_string(),
            host: " 10.0.0.5 ".to_string(),
            port: 5000,
            unit_id: 7,
            enabled: true,
            simulation: false,
            word_order: "high_low".to_string(),
            cpu_type: Some("Q".to_string()),
            database: Some(" appdb ".to_string()),
            username: Some(" alice ".to_string()),
            // A sealed `plain-v1` secret (the blob is the plaintext): `settings_from_flat`
            // must not carry it - the write path inserts only the reference.
            secrets: [(
                "password".to_string(),
                crate::SecretChange::Set(crate::SealedSecretRow {
                    r#ref: "0123456789abcdef0123456789abcdef".to_string(),
                    scheme: crate::SCHEME_PLAIN_V1.to_string(),
                    blob: zeroize::Zeroizing::new(b"secret-plaintext".to_vec()),
                }),
            )]
            .into_iter()
            .collect(),
            settings: Some(json!({"stale_timeout_ms": 5000})),
        }
    }

    #[test]
    fn settings_from_flat_follows_design_3_2_per_protocol() {
        // modbus-tcp: host・port・unit_id・word_order。cpu_type・database・username・
        // password・settings は捨てる。
        assert_eq!(
            settings_from_flat(&input("modbus-tcp")),
            object(
                json!({"host": "10.0.0.5", "port": 5000, "unit_id": 7, "word_order": "high_low"})
            )
        );
        // slmp: host・port・word_order・cpu_type。unit_id は捨てる。
        assert_eq!(
            settings_from_flat(&input("slmp")),
            object(
                json!({"host": "10.0.0.5", "port": 5000, "word_order": "high_low", "cpu_type": "Q"})
            )
        );
        // postgres: host・port・database・username。password は写さない（秘密の参照は
        // R2b-1d）。unit_id・word_order・cpu_type は捨てる。
        assert_eq!(
            settings_from_flat(&input("postgres")),
            object(
                json!({"host": "10.0.0.5", "port": 5000, "database": "appdb", "username": "alice"})
            )
        );
        // virtual・hub: {}。
        assert_eq!(settings_from_flat(&input("virtual")), Map::new());
        assert_eq!(settings_from_flat(&input("hub")), Map::new());
        // push: 今の settings をそのまま。
        assert_eq!(
            settings_from_flat(&input("push")),
            object(json!({"stale_timeout_ms": 5000}))
        );
        let mut push = input("push");
        push.settings = None;
        assert_eq!(settings_from_flat(&push), Map::new());
        push.settings = Some(Value::Null);
        assert_eq!(settings_from_flat(&push), Map::new());
        // 知らないプロトコル: {}（レジストリが protocol で断る）。
        assert_eq!(settings_from_flat(&input("bogus")), Map::new());
    }

    #[test]
    fn settings_from_flat_never_carries_the_plaintext_password() {
        for protocol in ["modbus-tcp", "slmp", "postgres", "virtual", "hub", "push"] {
            let text = Value::Object(settings_from_flat(&input(protocol))).to_string();
            assert!(!text.contains("secret-plaintext"), "{protocol}: {text}");
            assert!(!text.contains("password"), "{protocol}: {text}");
        }
    }

    #[test]
    fn slmp_cpu_type_is_left_out_when_unspecified() {
        for unspecified in [None, Some(String::new())] {
            let mut slmp = input("slmp");
            slmp.cpu_type = unspecified;
            let map = settings_from_flat(&slmp);
            assert!(!map.contains_key("cpu_type"));
        }
    }

    // ---- rest_field_error ----

    #[test]
    fn field_keys_are_written_in_the_rest_spelling() {
        let cases = [
            ("modbus-tcp", "unit_id", "settings.unit_id"),
            ("modbus-tcp", "word_order", "settings.word_order"),
            ("slmp", "cpu_type", "settings.cpu_type"),
            ("slmp", "host", "settings.host"),
            ("postgres", "port", "settings.port"),
            ("postgres", "database", "settings.database"),
            ("postgres", "username", "settings.username"),
            ("push", "stale_timeout_ms", "settings.stale_timeout_ms"),
            ("push", "bridge", "settings.bridge"),
        ];
        for (protocol, key, field) in cases {
            assert_eq!(
                rest_field_error(protocol, key, "m".to_string()).field,
                field
            );
        }
        // push の入れ子の位置は項目名へ戻し、文言は元に戻す。
        let nested = rest_field_error(
            "push",
            "bridge",
            "bridge.queries[0].sql: 空です".to_string(),
        );
        assert_eq!(nested.field, "settings.bridge.queries[0].sql");
        assert_eq!(nested.message, "空です");
        // 位置でない先頭一致（"bridge を JSON として…"）は戻さない。
        let not_nested = rest_field_error(
            "push",
            "bridge",
            "bridge を JSON として読めません: x".to_string(),
        );
        assert_eq!(not_nested.field, "settings.bridge");
        assert_eq!(not_nested.message, "bridge を JSON として読めません: x");
    }

    #[test]
    fn an_unregistered_protocol_is_a_protocol_field_error() {
        let registry = DriverRegistry::new();
        let errors = validate_settings_with_registry(&registry, "modbus-tcp", &Map::new());
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "protocol");
        assert!(errors[0].message.starts_with("対応プロトコルは "));
    }

    // ---- settings_from_schema（R2b-6b、設計 §4.2・§10）----

    mod from_schema {
        use super::super::settings_from_schema;
        use super::object;
        use banto_driver_api::{ChoiceOption, FieldKind, SettingField, SettingsSchema};
        use serde_json::{json, Value};

        /// Modbus / SLMP の形（必須の host、既定つきの必須の port、既定つきの choice）に、
        /// 表のための text・bool・secret と既定の無い任意の integer を足したもの。
        fn schema() -> SettingsSchema {
            SettingsSchema::new(vec![
                SettingField::new("host", "ホスト", FieldKind::host()).required(),
                SettingField::new("port", "ポート", FieldKind::integer(1, 65535))
                    .required()
                    .default(502),
                SettingField::new(
                    "word_order",
                    "ワード順",
                    FieldKind::choice(vec![
                        ChoiceOption::new("high_low", "上位が先"),
                        ChoiceOption::new("low_high", "下位が先"),
                    ]),
                )
                .default("high_low")
                .advanced(),
                SettingField::new(
                    "note",
                    "メモ",
                    FieldKind::Text {
                        max_len: Some(4),
                        placeholder: None,
                    },
                ),
                SettingField::new("backup_host", "予備ホスト", FieldKind::host()),
                SettingField::new("tls", "TLS", FieldKind::bool()).default(false),
                SettingField::new("retries", "再試行", FieldKind::integer_unbounded()),
                SettingField::new("password", "パスワード", FieldKind::secret()),
                SettingField::new("token", "トークン", FieldKind::secret()).required(),
            ])
        }

        /// 1 行 = (名前, 入力, 期待)。期待は `Ok(出力)` か `Err([(キー, 文言の一部)])`。
        type Expect = Result<Value, Vec<(&'static str, &'static str)>>;

        fn cases() -> Vec<(&'static str, Value, Expect)> {
            let defaults = json!({ "port": 502, "word_order": "high_low", "tls": false });
            let with = |extra: Value| {
                let mut out = object(defaults.clone());
                out.extend(object(extra));
                Ok(Value::Object(out))
            };
            vec![
                // 既定値の補完（省略と null のどちらも）
                (
                    "最小の入力に既定値を埋める",
                    json!({ "host": "10.0.0.1" }),
                    with(json!({ "host": "10.0.0.1" })),
                ),
                (
                    "Host は前後の空白を落とし、Text は落とさない",
                    json!({ "host": " 127.0.0.1 ", "backup_host": "\tb \n", "note": " a " }),
                    with(json!({ "host": "127.0.0.1", "backup_host": "b", "note": " a " })),
                ),
                (
                    "Secret は前後の空白を落とさない",
                    json!({ "host": "h", "password": " p ", "token": " t " }),
                    with(json!({ "host": "h", "password": " p ", "token": " t " })),
                ),
                (
                    "null も既定値で埋める",
                    json!({ "host": "h", "port": null, "word_order": null, "tls": null }),
                    with(json!({ "host": "h" })),
                ),
                (
                    "与えた値は既定値より優先する",
                    json!({ "host": "h", "port": 5007, "word_order": "low_high", "tls": true,
                            "note": "abcd", "retries": -3 }),
                    Ok(
                        json!({ "host": "h", "port": 5007, "word_order": "low_high", "tls": true,
                               "note": "abcd", "retries": -3 }),
                    ),
                ),
                // 種類の違い（読み替えをしない）
                (
                    "Integer に文字列の数は断る",
                    json!({ "host": "h", "port": "502" }),
                    Err(vec![("port", "整数で")]),
                ),
                (
                    "Integer に小数は断る",
                    json!({ "host": "h", "port": 502.5 }),
                    Err(vec![("port", "整数で")]),
                ),
                (
                    "Host に数は断る",
                    json!({ "host": 10 }),
                    Err(vec![("host", "文字列で")]),
                ),
                (
                    "必須でない Host に数は断る",
                    json!({ "host": "h", "backup_host": 5 }),
                    Err(vec![("backup_host", "文字列で")]),
                ),
                (
                    "必須でない Host に真偽値は断る",
                    json!({ "host": "h", "backup_host": true }),
                    Err(vec![("backup_host", "文字列で")]),
                ),
                (
                    "Choice に数は断る",
                    json!({ "host": "h", "word_order": 1 }),
                    Err(vec![("word_order", "文字列で")]),
                ),
                (
                    "Bool に文字列は断る",
                    json!({ "host": "h", "tls": "true" }),
                    Err(vec![("tls", "true か false")]),
                ),
                (
                    "Text にオブジェクトは断る",
                    json!({ "host": "h", "note": {} }),
                    Err(vec![("note", "文字列で")]),
                ),
                // 必須の欠け
                (
                    "必須の host が無い",
                    json!({}),
                    Err(vec![("host", "必須です")]),
                ),
                (
                    "必須の host が null",
                    json!({ "host": null }),
                    Err(vec![("host", "必須です")]),
                ),
                (
                    "必須の host が空白だけ",
                    json!({ "host": "  " }),
                    Err(vec![("host", "必須です")]),
                ),
                // 範囲外・選択肢の外・長さ
                (
                    "port が 0",
                    json!({ "host": "h", "port": 0 }),
                    Err(vec![("port", "1〜65535")]),
                ),
                (
                    "port が 65536",
                    json!({ "host": "h", "port": 65536 }),
                    Err(vec![("port", "1〜65535")]),
                ),
                (
                    "port の端は通る",
                    json!({ "host": "h", "port": 65535 }),
                    with(json!({ "host": "h", "port": 65535 })),
                ),
                (
                    "選択肢に無い値",
                    json!({ "host": "h", "word_order": "big" }),
                    Err(vec![("word_order", "high_low, low_high")]),
                ),
                (
                    "max_len を超える（文字数で数える）",
                    json!({ "host": "h", "note": "あいうえお" }),
                    Err(vec![("note", "4 文字以下")]),
                ),
                (
                    "max_len ちょうどは通る（文字数で数える）",
                    json!({ "host": "h", "note": "あいうえ" }),
                    with(json!({ "host": "h", "note": "あいうえ" })),
                ),
                // 未知のキー
                (
                    "スキーマに無いキーは断る",
                    json!({ "host": "h", "unit_id": 1 }),
                    Err(vec![("unit_id", "無い欄")]),
                ),
                (
                    "$secret の参照もスキーマに無いキーなら断る",
                    json!({ "host": "h", "extra": { "$secret": "r" } }),
                    Err(vec![("extra", "無い欄")]),
                ),
                // 秘密のキーの tri-state
                (
                    "秘密の省略は保つ（出力にキーが無い）",
                    json!({ "host": "h" }),
                    with(json!({ "host": "h" })),
                ),
                (
                    "秘密の null は消す",
                    json!({ "host": "h", "password": null }),
                    with(json!({ "host": "h", "password": null })),
                ),
                (
                    "秘密の文字列は置き換え",
                    json!({ "host": "h", "password": "pw", "token": "t" }),
                    with(json!({ "host": "h", "password": "pw", "token": "t" })),
                ),
                (
                    "秘密の空文字も置き換え（文字列）",
                    json!({ "host": "h", "password": "" }),
                    with(json!({ "host": "h", "password": "" })),
                ),
                (
                    "秘密に数は断る",
                    json!({ "host": "h", "password": 1 }),
                    Err(vec![("password", "文字列か null")]),
                ),
                (
                    "秘密に参照のオブジェクトは断る",
                    json!({ "host": "h", "password": { "$secret": "r" } }),
                    Err(vec![("password", "文字列か null")]),
                ),
                (
                    "必須の秘密の null は欠け",
                    json!({ "host": "h", "token": null }),
                    Err(vec![("token", "必須です")]),
                ),
                // 複数のエラーはスキーマの順、その後に未知のキー
                (
                    "エラーはスキーマの順に全部",
                    json!({ "zzz": 1, "tls": 1, "port": "x" }),
                    Err(vec![
                        ("host", "必須です"),
                        ("port", "整数で"),
                        ("tls", "true か false"),
                        ("zzz", "無い欄"),
                    ]),
                ),
            ]
        }

        #[test]
        fn the_table() {
            for (name, input, expect) in cases() {
                let got = settings_from_schema(&schema(), &object(input));
                match (got, expect) {
                    (Ok(map), Ok(want)) => assert_eq!(Value::Object(map), want, "{name}"),
                    (Err(errors), Err(want)) => {
                        let keys: Vec<&str> = errors.iter().map(|e| e.key.as_str()).collect();
                        let want_keys: Vec<String> =
                            want.iter().map(|(k, _)| format!("settings.{k}")).collect();
                        assert_eq!(keys, want_keys, "{name}: {errors:?}");
                        for (error, (_, part)) in errors.iter().zip(&want) {
                            assert!(error.message.contains(part), "{name}: {errors:?}");
                        }
                    }
                    (got, want) => panic!("{name}: got {got:?}, want {want:?}"),
                }
            }
        }

        #[test]
        fn an_empty_schema_refuses_every_key_and_accepts_nothing() {
            let empty = SettingsSchema::default();
            assert_eq!(
                settings_from_schema(&empty, &object(json!({}))).unwrap(),
                object(json!({}))
            );
            let errors =
                settings_from_schema(&empty, &object(json!({ "a": 1, "b": 2 }))).unwrap_err();
            let keys: Vec<&str> = errors.iter().map(|e| e.key.as_str()).collect();
            assert_eq!(keys, ["settings.a", "settings.b"]);
        }

        #[test]
        fn bounds_on_one_side_only() {
            let schema = SettingsSchema::new(vec![
                SettingField::new(
                    "lo",
                    "lo",
                    FieldKind::Integer {
                        min: Some(0),
                        max: None,
                    },
                ),
                SettingField::new(
                    "hi",
                    "hi",
                    FieldKind::Integer {
                        min: None,
                        max: Some(9),
                    },
                ),
            ]);
            let ok = settings_from_schema(&schema, &object(json!({ "lo": 1_000_000, "hi": -5 })));
            assert!(ok.is_ok(), "{ok:?}");
            let errors =
                settings_from_schema(&schema, &object(json!({ "lo": -1, "hi": 10 }))).unwrap_err();
            assert!(errors[0].message.contains("0 以上"), "{errors:?}");
            assert!(errors[1].message.contains("9 以下"), "{errors:?}");
        }
    }
}
