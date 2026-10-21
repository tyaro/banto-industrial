//! `push` の静的な部分（R2a-2a、docs/banto-hub-ingest-design.md §2・§3.2・§3.3）:
//! [`PushDriver`]（`banto_driver_api::DriverBase`）。
//!
//! `push` は R2a では Hub 側の `ExternalSource` で、`DriverKind::Session` でも
//! `HostBound` でもない（§2 の表、Q1）。それでも識別・設定の検証・キーの検証は
//! ドライバと同じ形（`DriverBase`）で持つ。**R2b-1a から host が `DriverKind::HostProvided`
//! でレジストリへ登録する**（[`crate::host_provided`]、設計
//! docs/banto-hub-connection-settings-design.md §4.1。I/O は Hub の収集の外部ソースのまま）。
//! `banto_drivers::builtin()` には入れない（トリップワイヤ 2「broker の表 ==
//! `HostBound`」を崩さない、§2 の案 C）。core はこのために `banto-drivers` に
//! 依存しない（R2b-0 の決まりのまま）。本番の経路はまだレジストリを読まない。
//!
//! 中身は `banto_tags::push` の純関数を包むだけ:
//!
//! - [`PushDriver::validate_config`] は `banto_driver_api::keys::*` を読まない。
//!   `Settings` は文字列の対しか持てない（§1.11）ので、`"stale_timeout_ms"` は
//!   10 進の文字列、`"bridge"` は JSON の文字列で受け、JSON のオブジェクトに組み
//!   直して `banto_tags::validate_push_settings` に掛ける（§3.2。入れ子の設定を API で
//!   どう表すかは R2b-1 の課題、§13 の 5）。ほかのキーは断らない（`DriverBase` の
//!   約束・適合テスト C2。`settings` の JSON そのものは未知のキーを断るが、それは
//!   `PlcConnectionService` の入口の話）。
//! - [`PushDriver::parse_address`] は `banto_tags::validate_push_key` を包む。正規形は前後の
//!   空白を除いた文字列で、`area = "key"`・`writable_area = false`・`bit = None`・
//!   `word_count = kind.word_count()`（キーに語数の意味は無い。C1 の冪等だけを満たす）。
//! - [`PushDriver::offset_address`] は `n == 0` なら正規形をそのまま返す（C1）。それ以外は
//!   断る（キーは連番にできない）。
//!
//! 秘密値は持たない（`secret_keys` は既定の空。ブリッジの DB のパスワードは参照先の
//! `postgres` 接続の列のまま、§3.2・§10.2）。

use banto_driver_api::{
    AddressError, Capabilities, DriverAddress, DriverBase, DriverDescriptor, DriverId, FieldError,
    Settings, TagKind,
};
use banto_tags::{validate_push_key, validate_push_settings};

/// `plc_connections.protocol` の `"push"`（`banto_tags::PUSH_PROTOCOL` と同じ綴り）。
pub const PUSH_PROTOCOL: &str = banto_tags::PUSH_PROTOCOL;

/// [`PushDriver::validate_config`] が読む `Settings` のキー: 黙ってから Down と
/// みなすまで（ms、10 進の文字列）。
pub const STALE_TIMEOUT_MS_KEY: &str = "stale_timeout_ms";

/// [`PushDriver::validate_config`] が読む `Settings` のキー: ブリッジの区画
/// （`settings.bridge` の JSON の文字列）。
pub const BRIDGE_KEY: &str = "bridge";

/// キーの区画名（`DriverAddress::area`）。
const KEY_AREA: &str = "key";

/// `push` の `DriverBase`（モジュールの doc 参照）。状態は持たない。
#[derive(Debug, Clone)]
pub struct PushDriver {
    descriptor: DriverDescriptor,
}

impl PushDriver {
    /// `id = "push"`、読み取りだけ（書き込みの方向は持たない、§9.5-8）、
    /// `transports = []`（Hub から繋ぎに行く先が無い。送り手が Hub へ繋ぐ）。
    pub fn new() -> Self {
        let id = DriverId::new(PUSH_PROTOCOL).expect("\"push\" fits the DriverId grammar");
        PushDriver {
            descriptor: DriverDescriptor::new(
                id,
                env!("CARGO_PKG_VERSION"),
                "取り込み（push）",
                Capabilities::READ_ONLY,
            )
            .with_transports(Vec::new()),
        }
    }
}

impl Default for PushDriver {
    fn default() -> Self {
        Self::new()
    }
}

/// host がレジストリへ登録するときの入口（§12 Q5、[`crate::host_provided`]）。
pub fn push_driver() -> PushDriver {
    PushDriver::new()
}

impl DriverBase for PushDriver {
    fn descriptor(&self) -> &DriverDescriptor {
        &self.descriptor
    }

    fn validate_config(&self, settings: &Settings) -> Vec<FieldError> {
        let mut errors = Vec::new();
        let mut object = serde_json::Map::new();
        if let Some(text) = settings.get(STALE_TIMEOUT_MS_KEY) {
            match text.trim().parse::<u64>() {
                Ok(ms) => {
                    object.insert(STALE_TIMEOUT_MS_KEY.to_string(), ms.into());
                }
                Err(_) => errors.push(FieldError::new(
                    STALE_TIMEOUT_MS_KEY,
                    "stale_timeout_ms は整数（ミリ秒）で指定してください",
                )),
            }
        }
        if let Some(text) = settings.get(BRIDGE_KEY) {
            match serde_json::from_str::<serde_json::Value>(text) {
                Ok(bridge) => {
                    object.insert(BRIDGE_KEY.to_string(), bridge);
                }
                Err(err) => errors.push(FieldError::new(
                    BRIDGE_KEY,
                    format!("bridge を JSON として読めません: {err}"),
                )),
            }
        }
        if let Err(settings_errors) = validate_push_settings(&serde_json::Value::Object(object)) {
            errors.extend(settings_errors.into_iter().map(to_driver_field_error));
        }
        errors
    }

    fn parse_address(&self, text: &str, kind: &TagKind) -> Result<DriverAddress, AddressError> {
        let canonical = validate_push_key(text).map_err(AddressError::new)?;
        Ok(DriverAddress::new(
            self.descriptor.id.clone(),
            canonical,
            KEY_AREA,
            false,
            kind.word_count(),
        ))
    }

    fn offset_address(
        &self,
        canonical: &str,
        _kind: &TagKind,
        n: i64,
    ) -> Result<String, AddressError> {
        if n == 0 {
            return Ok(canonical.to_string());
        }
        Err(AddressError::new("push のキーは連番にできません"))
    }

    fn address_help(&self) -> String {
        "送り手が決めるキー（1〜256 文字、大文字小文字を区別）".to_string()
    }
}

/// `validate_push_settings` の項目エラー（`settings.stale_timeout_ms`・
/// `settings.bridge.queries[0].sql` の形）を `Settings` のキーへ写す。入れ子の位置は
/// 文言の先頭に残す（`bridge.queries[0].sql: ...`）。
fn to_driver_field_error(error: banto_core::FieldError) -> FieldError {
    let path = error.field.strip_prefix("settings.").unwrap_or("");
    let key = if path.starts_with(STALE_TIMEOUT_MS_KEY) {
        STALE_TIMEOUT_MS_KEY
    } else {
        // `settings` そのもの（大きさの上限）と `settings.bridge*` はブリッジの区画の話。
        BRIDGE_KEY
    };
    if path.is_empty() || path == key {
        FieldError::new(key, error.message)
    } else {
        FieldError::new(key, format!("{path}: {}", error.message))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use banto_driver_api::{DataType, StringEncoding, Transport};
    use banto_driver_conformance::{static_checks, StaticFixtures};

    fn numeric(data_type: DataType) -> TagKind {
        TagKind::numeric(data_type)
    }

    fn keys_of(errors: &[FieldError]) -> Vec<&str> {
        errors.iter().map(|e| e.key.as_str()).collect()
    }

    /// §11 の R2a-2a の完了条件: `PushDriver` が適合テストの静的な部分（C1・C2）を通る。
    #[test]
    fn push_driver_passes_the_static_conformance_checks() {
        let f32 = numeric(DataType::F32);
        let longest = "k".repeat(banto_tags::MAX_PUSH_KEY_LEN);
        let fixtures = StaticFixtures::new(Settings::new())
            .valid("line1.temp", f32.clone(), "line1.temp")
            .valid("  Line1/Temp  ", f32.clone(), "Line1/Temp")
            .valid("温度 1", numeric(DataType::Bit), "温度 1")
            .valid("D100", numeric(DataType::I64), "D100")
            .valid(&longest, f32.clone(), &longest)
            .invalid("", f32.clone())
            .invalid("   ", f32.clone())
            .invalid("a\tb", f32.clone())
            .invalid("a\u{0}", f32.clone())
            .invalid(&format!("{longest}k"), f32.clone())
            .offset("line1.temp", f32.clone(), 0, Some("line1.temp"))
            .offset("line1.temp", f32.clone(), 1, None)
            .offset("line1.temp", f32, -1, None);
        let failures = static_checks(&push_driver(), &fixtures);
        assert!(failures.is_empty(), "{failures:#?}");
    }

    #[test]
    fn the_descriptor_is_push_read_only_without_transports() {
        let driver = PushDriver::new();
        let d = driver.descriptor();
        assert_eq!(d.id.as_str(), "push");
        assert_eq!(d.id.as_str(), banto_tags::PUSH_PROTOCOL);
        assert_eq!(d.capabilities, Capabilities::READ_ONLY);
        assert_eq!(d.transports, Vec::<Transport>::new());
        assert!(driver.secret_keys().is_empty());
        assert!(driver.address_help().contains("256"));
    }

    #[test]
    fn parse_address_reports_the_key_shape() {
        let driver = PushDriver::new();
        let parsed = driver
            .parse_address(" a.b ", &numeric(DataType::U32))
            .unwrap();
        assert_eq!(parsed.canonical, "a.b");
        assert_eq!(parsed.area, "key");
        assert!(!parsed.writable_area);
        assert_eq!(parsed.bit, None);
        assert_eq!(parsed.word_count, 2);
        // 語数に意味は無いが、kind のとおりにする（C1）。文字列の kind も断らない
        // （string のタグを断るのはタグの配置検証、banto_tags の push_tag_errors）。
        let s = driver
            .parse_address("k", &TagKind::string(4, StringEncoding::Utf8))
            .unwrap();
        assert_eq!(s.word_count, 4);
    }

    #[test]
    fn validate_config_reads_only_its_two_keys() {
        let driver = PushDriver::new();
        assert!(driver.validate_config(&Settings::new()).is_empty());
        let ok = Settings::from_pairs([
            ("stale_timeout_ms", "5000"),
            (
                "bridge",
                r#"{"kind":"db","db_connection_id":3,"queries":[{"name":"q","period_ms":1000,"sql":"SELECT 1 AS v","columns":[{"column":"v","key":"k"}]}]}"#,
            ),
            // keys::* などの他の層のキーは読まない・断らない。
            ("host", "ignored"),
            ("port", "abc"),
        ])
        .unwrap();
        assert_eq!(driver.validate_config(&ok), vec![]);
    }

    #[test]
    fn validate_config_maps_errors_onto_the_settings_keys() {
        let driver = PushDriver::new();
        let check = |pairs: &[(&str, &str)]| {
            driver.validate_config(&Settings::from_pairs(pairs.iter().copied()).unwrap())
        };
        assert_eq!(
            keys_of(&check(&[("stale_timeout_ms", "abc")])),
            ["stale_timeout_ms"]
        );
        assert_eq!(
            keys_of(&check(&[("stale_timeout_ms", "999")])),
            ["stale_timeout_ms"]
        );
        assert_eq!(
            keys_of(&check(&[("stale_timeout_ms", "3600001")])),
            ["stale_timeout_ms"]
        );
        assert_eq!(keys_of(&check(&[("bridge", "{not json")])), ["bridge"]);
        let errors = check(&[(
            "bridge",
            r#"{"kind":"db","db_connection_id":3,"queries":[{"name":"q","period_ms":300,"sql":"SELECT 1","columns":[]}]}"#,
        )]);
        assert_eq!(keys_of(&errors), ["bridge"]);
        assert!(
            errors[0]
                .message
                .starts_with("bridge.queries[0].period_ms: "),
            "{}",
            errors[0].message
        );
        // 送る量の見積もり（§7.1）も同じ経路で断る: 100 ms × 3 本 = 30 メッセージ / 秒。
        let q = |name: &str| {
            format!(
                r#"{{"name":"{name}","period_ms":100,"sql":"SELECT 1 AS v","columns":[{{"column":"v","key":"{name}"}}]}}"#
            )
        };
        let fast = format!(
            r#"{{"kind":"db","db_connection_id":3,"queries":[{},{},{}]}}"#,
            q("a"),
            q("b"),
            q("c")
        );
        let errors = check(&[("bridge", fast.as_str())]);
        assert_eq!(keys_of(&errors), ["bridge"]);
        assert!(
            errors[0].message.contains("約 30 メッセージ"),
            "{}",
            errors[0].message
        );
    }
}
