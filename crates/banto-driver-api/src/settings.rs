//! Connection settings (WIT `record setting { key, value }`,
//! docs/banto-driver-api-design.md §3.5).
//!
//! [`Settings`] is an ordered list (the order is the UI's and the audit
//! log's display order) with no duplicate keys; the field is private so the
//! no-duplicates invariant cannot be broken from outside, and deserializing
//! (`serde` feature) goes through the same check as [`Settings::from_pairs`].
//!
//! In R1 the host builds `Settings` from `PlcConnection`'s fixed columns
//! (`host`, `port`, `unit_id`, `word_order`, `cpu_type`) with the
//! [`keys`] below; R2b-1 moves the columns to JSON under the same keys.

use std::fmt;

/// Key names the host fills from its fixed connection columns. They keep
/// their meaning when R2b-1 moves settings to JSON. Adding a key is not a
/// breaking change.
pub mod keys {
    /// Host name or IP address.
    pub const HOST: &str = "host";
    /// TCP port.
    pub const PORT: &str = "port";
    /// Modbus unit identifier (station). Ignored by SLMP. For R1's Modbus
    /// this is the multi-drop station ([`STATION`]).
    pub const UNIT_ID: &str = "unit_id";
    /// Multi-word numeric order: `"high_low"` | `"low_high"`
    /// ([`WordOrder`](crate::WordOrder)). A connection setting, never per
    /// read unit.
    pub const WORD_ORDER: &str = "word_order";
    /// Connect timeout in milliseconds.
    pub const CONNECT_TIMEOUT_MS: &str = "connect_timeout_ms";
    /// Response timeout in milliseconds (per connection; covers serial
    /// converters too, design §8).
    pub const RESPONSE_TIMEOUT_MS: &str = "response_timeout_ms";
    /// SLMP CPU type (#605): `"R"` | `"Q"`.
    pub const CPU_TYPE: &str = "cpu_type";
    /// Transport (design §8). Always `"tcp"` in R1.
    pub const TRANSPORT: &str = "transport";
    /// Serial port name (reserved for R2b-3, design §8).
    pub const SERIAL_PORT: &str = "serial_port";
    /// RS-485 multi-drop station (reserved, design §8). R1's Modbus uses
    /// [`UNIT_ID`] for this.
    pub const STATION: &str = "station";
}

/// Ordered connection settings with unique keys.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(try_from = "Vec<Setting>", into = "Vec<Setting>")
)]
pub struct Settings {
    items: Vec<Setting>,
}

impl Settings {
    /// No settings.
    pub fn new() -> Self {
        Settings::default()
    }

    /// Settings from `(key, value)` pairs, in the given order. A key that
    /// appears twice is an error (the first duplicate is reported), not a
    /// silent overwrite.
    pub fn from_pairs<K: Into<String>, V: Into<String>>(
        pairs: impl IntoIterator<Item = (K, V)>,
    ) -> Result<Self, DuplicateKey> {
        let mut settings = Settings::new();
        for (key, value) in pairs {
            settings.push_unique(Setting::new(key, value))?;
        }
        Ok(settings)
    }

    /// The value for `key`, if set.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.items
            .iter()
            .find(|s| s.key == key)
            .map(|s| s.value.as_str())
    }

    /// Set `key` to `value`. An existing key is overwritten in place (it
    /// keeps its position); a new key is appended.
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<String>) {
        let key = key.into();
        let value = value.into();
        match self.items.iter_mut().find(|s| s.key == key) {
            Some(existing) => existing.value = value,
            None => self.items.push(Setting { key, value }),
        }
    }

    /// The settings in order.
    pub fn iter(&self) -> impl Iterator<Item = &Setting> {
        self.items.iter()
    }

    fn push_unique(&mut self, setting: Setting) -> Result<(), DuplicateKey> {
        if self.items.iter().any(|s| s.key == setting.key) {
            return Err(DuplicateKey { key: setting.key });
        }
        self.items.push(setting);
        Ok(())
    }
}

impl TryFrom<Vec<Setting>> for Settings {
    type Error = DuplicateKey;

    fn try_from(items: Vec<Setting>) -> Result<Self, Self::Error> {
        let mut settings = Settings::new();
        for setting in items {
            settings.push_unique(setting)?;
        }
        Ok(settings)
    }
}

impl From<Settings> for Vec<Setting> {
    fn from(settings: Settings) -> Self {
        settings.items
    }
}

/// One key/value pair.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Setting {
    /// The key (see [`keys`] for the shared ones).
    pub key: String,
    /// The value, always text (WIT has no typed settings).
    pub value: String,
}

impl Setting {
    /// A pair.
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Setting {
            key: key.into(),
            value: value.into(),
        }
    }
}

/// A key given twice to [`Settings::from_pairs`] (or in deserialized
/// settings).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateKey {
    key: String,
}

impl DuplicateKey {
    /// The repeated key.
    pub fn key(&self) -> &str {
        &self.key
    }
}

impl fmt::Display for DuplicateKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "設定のキー {:?} が重複しています", self.key)
    }
}

impl std::error::Error for DuplicateKey {}

/// One problem a driver's settings validation found (WIT
/// `record field-error { key, message }`): which key, and why.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FieldError {
    /// The offending key.
    pub key: String,
    /// What is wrong with it (shown to the user).
    pub message: String,
}

impl FieldError {
    /// A problem with `key`.
    pub fn new(key: impl Into<String>, message: impl Into<String>) -> Self {
        FieldError {
            key: key.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for FieldError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.key, self.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(s: &Settings) -> Vec<(&str, &str)> {
        s.iter()
            .map(|s| (s.key.as_str(), s.value.as_str()))
            .collect()
    }

    #[test]
    fn from_pairs_keeps_order_and_get_finds_values() {
        let s = Settings::from_pairs([
            (keys::HOST, "192.168.0.10"),
            (keys::PORT, "502"),
            (keys::WORD_ORDER, "low_high"),
        ])
        .unwrap();
        assert_eq!(
            pairs(&s),
            [
                ("host", "192.168.0.10"),
                ("port", "502"),
                ("word_order", "low_high")
            ]
        );
        assert_eq!(s.get(keys::PORT), Some("502"));
        assert_eq!(s.get(keys::UNIT_ID), None);
        assert_eq!(Settings::new(), Settings::default());
        assert_eq!(Settings::new().iter().count(), 0);
    }

    #[test]
    fn from_pairs_rejects_a_duplicate_key() {
        let err = Settings::from_pairs([("host", "a"), ("port", "1"), ("host", "b")]).unwrap_err();
        assert_eq!(err.key(), "host");
        assert_eq!(err.to_string(), "設定のキー \"host\" が重複しています");
        let err =
            Settings::try_from(vec![Setting::new("x", "1"), Setting::new("x", "1")]).unwrap_err();
        assert_eq!(err.key(), "x");
    }

    #[test]
    fn insert_overwrites_in_place_and_appends_new_keys() {
        let mut s = Settings::from_pairs([("host", "a"), ("port", "1")]).unwrap();
        s.insert("host", "b");
        s.insert(String::from("unit_id"), String::from("3"));
        assert_eq!(pairs(&s), [("host", "b"), ("port", "1"), ("unit_id", "3")]);
        let back: Vec<Setting> = s.into();
        assert_eq!(back[0], Setting::new("host", "b"));
    }

    #[test]
    fn field_error_new_and_display() {
        let e = FieldError::new(keys::PORT, "1〜65535 で指定してください");
        assert_eq!(e.key, "port");
        assert_eq!(e.message, "1〜65535 で指定してください");
        assert_eq!(e.to_string(), "port: 1〜65535 で指定してください");
    }

    #[test]
    fn shared_keys_are_spelled_as_designed() {
        assert_eq!(
            [
                keys::HOST,
                keys::PORT,
                keys::UNIT_ID,
                keys::WORD_ORDER,
                keys::CONNECT_TIMEOUT_MS,
                keys::RESPONSE_TIMEOUT_MS,
                keys::CPU_TYPE,
                keys::TRANSPORT,
                keys::SERIAL_PORT,
                keys::STATION,
            ],
            [
                "host",
                "port",
                "unit_id",
                "word_order",
                "connect_timeout_ms",
                "response_timeout_ms",
                "cpu_type",
                "transport",
                "serial_port",
                "station",
            ]
        );
    }
}

/// Round trips under the `serde` feature (`cargo test -p banto-driver-api
/// --all-features`; CI's `driver-api` job). They pin the wire spellings the
/// `serde` feature's doc promises, and that deserializing re-checks the
/// invariants.
#[cfg(all(test, feature = "serde"))]
mod serde_tests {
    use super::*;
    use crate::{
        DataType, DeviceError, DriverAddress, DriverId, FatalError, ItemError, StringEncoding,
        TagKind, TagSpec, TagValue, Value, WordOrder, WriteItem,
    };
    use serde_json::json;

    #[test]
    fn enum_spellings_match_parse_display_and_the_setting_values() {
        for dt in [
            DataType::Bit,
            DataType::I16,
            DataType::U16,
            DataType::I32,
            DataType::U32,
            DataType::F32,
            DataType::I64,
            DataType::U64,
            DataType::F64,
        ] {
            assert_eq!(serde_json::to_value(dt).unwrap(), json!(dt.to_string()));
        }
        assert_eq!(
            serde_json::to_value(WordOrder::HighLow).unwrap(),
            json!("high_low")
        );
        assert_eq!(
            serde_json::to_value(WordOrder::LowHigh).unwrap(),
            json!("low_high")
        );
        assert_eq!(
            serde_json::to_value(StringEncoding::Utf8).unwrap(),
            json!("utf8")
        );
        assert_eq!(
            serde_json::to_value(StringEncoding::ShiftJis).unwrap(),
            json!("shift_jis")
        );
        assert_eq!(
            serde_json::to_value(TagValue::Bit(true)).unwrap(),
            json!({"bit": true})
        );
        assert_eq!(
            serde_json::to_value(FatalError::Timeout).unwrap(),
            json!("timeout")
        );
    }

    #[test]
    fn values_round_trip() {
        let address =
            DriverAddress::new(DriverId::new("slmp").unwrap(), "D100.A", "D", true, 1).with_bit(10);
        let item = WriteItem::new(
            TagSpec::new(address, TagKind::string(4, StringEncoding::Utf8)),
            Value::Str("abc".into()),
        );
        let text = serde_json::to_string(&item).unwrap();
        assert_eq!(serde_json::from_str::<WriteItem>(&text).unwrap(), item);

        let error = ItemError::Device(DeviceError::new(0xC059, "x"));
        let text = serde_json::to_string(&error).unwrap();
        assert_eq!(serde_json::from_str::<ItemError>(&text).unwrap(), error);
    }

    #[test]
    fn deserializing_rechecks_driver_id_and_duplicate_keys() {
        assert_eq!(
            serde_json::to_value(DriverId::new("modbus-tcp").unwrap()).unwrap(),
            json!("modbus-tcp")
        );
        assert!(serde_json::from_value::<DriverId>(json!("Modbus")).is_err());

        let settings = Settings::from_pairs([("host", "a"), ("port", "1")]).unwrap();
        let value = serde_json::to_value(&settings).unwrap();
        assert_eq!(
            value,
            json!([{"key": "host", "value": "a"}, {"key": "port", "value": "1"}])
        );
        assert_eq!(serde_json::from_value::<Settings>(value).unwrap(), settings);
        let dup = json!([{"key": "host", "value": "a"}, {"key": "host", "value": "b"}]);
        let err = serde_json::from_value::<Settings>(dup).unwrap_err();
        assert!(err.to_string().contains("重複"), "{err}");
    }
}
