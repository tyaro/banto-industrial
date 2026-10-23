//! The shape of a driver's connection settings ([`SettingsSchema`]) and the
//! names of the tag kinds it can read ([`KindName`]) - what the Hub's generic
//! connection form and tag screens are built from instead of per-protocol
//! code (docs/banto-hub-driver-ui-design.md §3, R2b-6a; WIT `config.schema`
//! and `address.supported-kinds`).
//!
//! Types and plain data only, like the rest of the crate: no validation
//! logic lives here. A schema *describes* the fields (key, label, kind,
//! default, basic / advanced); whether a settings value is acceptable stays
//! [`DriverBase::validate_config`](crate::DriverBase::validate_config)'s
//! decision, which the host still calls before saving. The host fills
//! defaults from the schema; the conformance checks C17-C19 keep the schema
//! and the driver honest with each other.
//!
//! Everything here is `#[non_exhaustive]` with constructors and `with_*` /
//! builder methods (see the crate docs), so a field or a variant can be added
//! later without breaking a driver. With the `serde` feature the types
//! serialize to the JSON the UI receives as is (enums in `snake_case`;
//! [`KindName`] as the plain string `"bit"` ... `"f64"` / `"string"`).
//!
//! Conditional fields (one field shown by the value of another) are
//! deliberately not part of v1; a driver that needs them ships its own UI
//! (design Q4).

use std::fmt;

use crate::types::DataType;

/// Which fields of a connection's settings a person edits, in display order.
/// Keys are unique (conformance C17).
///
/// The default is empty: a driver that does not describe its settings gets a
/// generic form with no fields.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SettingsSchema {
    /// The fields, in the order they are shown.
    pub fields: Vec<SettingField>,
}

impl SettingsSchema {
    /// A schema of `fields`.
    pub fn new(fields: Vec<SettingField>) -> Self {
        SettingsSchema { fields }
    }

    /// The same schema with `field` appended.
    pub fn with_field(mut self, field: SettingField) -> Self {
        self.fields.push(field);
        self
    }

    /// The field whose key is `key`.
    pub fn field(&self, key: &str) -> Option<&SettingField> {
        self.fields.iter().find(|f| f.key == key)
    }

    /// Whether there are no fields.
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }
}

/// One editable setting.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SettingField {
    /// The key in the connection's settings (`"host"`, `"port"`,
    /// `"cpu_type"`; see [`keys`](crate::settings::keys)).
    pub key: String,
    /// The name shown to people (Japanese, plugin-design §6.2).
    pub label: String,
    /// An explanation shown under the field.
    pub help: Option<String>,
    /// What kind of value it holds.
    pub kind: FieldKind,
    /// Whether the settings are invalid without it.
    pub required: bool,
    /// The value a new connection starts with.
    pub default: Option<SettingValue>,
    /// Whether it is always shown or folded under "advanced".
    pub group: FieldGroup,
}

impl SettingField {
    /// A basic, optional field with no help text and no default; refine it
    /// with [`required`](Self::required), [`default`](Self::default),
    /// [`help`](Self::help) and [`advanced`](Self::advanced).
    pub fn new(key: impl Into<String>, label: impl Into<String>, kind: FieldKind) -> Self {
        SettingField {
            key: key.into(),
            label: label.into(),
            help: None,
            kind,
            required: false,
            default: None,
            group: FieldGroup::Basic,
        }
    }

    /// The same field, marked required.
    pub fn required(self) -> Self {
        SettingField {
            required: true,
            ..self
        }
    }

    /// The same field with `default` as its starting value.
    pub fn default(self, default: impl Into<SettingValue>) -> Self {
        SettingField {
            default: Some(default.into()),
            ..self
        }
    }

    /// The same field with the text shown under it.
    pub fn help(self, help: impl Into<String>) -> Self {
        SettingField {
            help: Some(help.into()),
            ..self
        }
    }

    /// The same field, folded under "advanced".
    pub fn advanced(self) -> Self {
        SettingField {
            group: FieldGroup::Advanced,
            ..self
        }
    }
}

/// What a setting's value is, which decides the control the UI draws.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(tag = "type", rename_all = "snake_case")
)]
pub enum FieldKind {
    /// Free text.
    Text {
        /// The longest accepted text, in characters.
        max_len: Option<u32>,
        /// Grey text shown while the field is empty.
        placeholder: Option<String>,
    },
    /// An IPv4 address or a host name (IPv6 is out of scope, 2026-09-29).
    Host,
    /// A whole number. The port is `Integer { min: 1, max: 65535 }`.
    Integer {
        /// The smallest accepted value.
        min: Option<i64>,
        /// The largest accepted value.
        max: Option<i64>,
    },
    /// One of a fixed set of text values.
    Choice {
        /// The choices, in display order. Values are unique (C17).
        options: Vec<ChoiceOption>,
    },
    /// On / off.
    Bool,
    /// A secret (a password, a token). Exactly the keys
    /// [`DriverBase::secret_keys`](crate::DriverBase::secret_keys) lists (C18).
    Secret,
}

impl FieldKind {
    /// Free text with no length limit and no placeholder.
    pub fn text() -> Self {
        FieldKind::Text {
            max_len: None,
            placeholder: None,
        }
    }

    /// An IPv4 address or host name.
    pub fn host() -> Self {
        FieldKind::Host
    }

    /// A whole number in `min..=max`.
    pub fn integer(min: i64, max: i64) -> Self {
        FieldKind::Integer {
            min: Some(min),
            max: Some(max),
        }
    }

    /// A whole number with no stated range.
    pub fn integer_unbounded() -> Self {
        FieldKind::Integer {
            min: None,
            max: None,
        }
    }

    /// One of `options`.
    pub fn choice(options: Vec<ChoiceOption>) -> Self {
        FieldKind::Choice { options }
    }

    /// On / off.
    pub fn bool() -> Self {
        FieldKind::Bool
    }

    /// A secret value.
    pub fn secret() -> Self {
        FieldKind::Secret
    }
}

/// One choice of a [`FieldKind::Choice`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ChoiceOption {
    /// The value stored in the settings.
    pub value: String,
    /// The name shown to people.
    pub label: String,
}

impl ChoiceOption {
    /// A choice storing `value`, shown as `label`.
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        ChoiceOption {
            value: value.into(),
            label: label.into(),
        }
    }
}

/// A typed setting value, for [`SettingField::default`]. (A connection's
/// settings themselves are text, [`Settings`](crate::Settings); the host
/// writes a default into them with its text form.)
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(untagged)
)]
pub enum SettingValue {
    /// Text (also a [`FieldKind::Choice`]'s value).
    Text(String),
    /// A whole number.
    Integer(i64),
    /// On / off.
    Bool(bool),
}

impl SettingValue {
    /// The value as the text a [`Settings`](crate::Settings) entry holds
    /// (`"502"`, `"true"`).
    pub fn to_text(&self) -> String {
        match self {
            SettingValue::Text(s) => s.clone(),
            SettingValue::Integer(n) => n.to_string(),
            SettingValue::Bool(b) => b.to_string(),
        }
    }
}

impl From<&str> for SettingValue {
    fn from(s: &str) -> Self {
        SettingValue::Text(s.to_owned())
    }
}

impl From<String> for SettingValue {
    fn from(s: String) -> Self {
        SettingValue::Text(s)
    }
}

impl From<i64> for SettingValue {
    fn from(n: i64) -> Self {
        SettingValue::Integer(n)
    }
}

impl From<bool> for SettingValue {
    fn from(b: bool) -> Self {
        SettingValue::Bool(b)
    }
}

/// Whether a field is always shown or folded away.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
pub enum FieldGroup {
    /// Always shown.
    Basic,
    /// Folded under "advanced".
    Advanced,
}

/// The name of a tag kind in the UI's data-type vocabulary: a [`DataType`]
/// (`"bit"`, `"i16"` ... `"f64"`) or `"string"`.
///
/// [`DataType`] is bits and numbers only; a string is a separate
/// [`TagKind::Str`](crate::TagKind), so a type that can name both is needed
/// for [`DriverBase::supported_kinds`](crate::DriverBase::supported_kinds).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(try_from = "String", into = "String")
)]
pub enum KindName {
    /// A bit or a number.
    Numeric(DataType),
    /// A string.
    String,
}

impl KindName {
    /// The name `"string"` as text.
    const STRING: &'static str = "string";

    /// Parse `"bit"` ... `"f64"` / `"string"` (the spelling
    /// [`Display`](fmt::Display) writes).
    pub fn parse(s: &str) -> Option<Self> {
        if s == Self::STRING {
            Some(KindName::String)
        } else {
            DataType::parse(s).map(KindName::Numeric)
        }
    }

    /// The name of `kind`; `None` for a [`TagKind`](crate::TagKind) variant
    /// added after this crate's version.
    pub fn of(kind: &crate::TagKind) -> Option<Self> {
        match kind {
            crate::TagKind::Numeric(data_type) => Some(KindName::Numeric(*data_type)),
            crate::TagKind::Str { .. } => Some(KindName::String),
            // `TagKind` is `#[non_exhaustive]` for other crates; a variant
            // added later has no name until this function learns it.
            #[allow(unreachable_patterns)]
            _ => None,
        }
    }
}

impl fmt::Display for KindName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KindName::Numeric(data_type) => write!(f, "{data_type}"),
            KindName::String => f.write_str(Self::STRING),
        }
    }
}

impl From<DataType> for KindName {
    fn from(data_type: DataType) -> Self {
        KindName::Numeric(data_type)
    }
}

impl From<KindName> for String {
    fn from(name: KindName) -> Self {
        name.to_string()
    }
}

/// A string that is not a [`KindName`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidKindName {
    input: String,
}

impl InvalidKindName {
    /// The rejected input, as given.
    pub fn input(&self) -> &str {
        &self.input
    }
}

impl fmt::Display for InvalidKindName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "データ型の名前 {:?} は使えません", self.input)
    }
}

impl std::error::Error for InvalidKindName {}

impl TryFrom<String> for KindName {
    type Error = InvalidKindName;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        KindName::parse(&s).ok_or(InvalidKindName { input: s })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_builder_defaults_and_chaining() {
        let f = SettingField::new("host", "ホスト", FieldKind::host());
        assert_eq!(f.group, FieldGroup::Basic);
        assert!(!f.required && f.help.is_none() && f.default.is_none());

        let f = SettingField::new("port", "ポート", FieldKind::integer(1, 65535))
            .required()
            .default(502)
            .help("TCP のポート")
            .advanced();
        assert!(f.required);
        assert_eq!(f.default, Some(SettingValue::Integer(502)));
        assert_eq!(f.help.as_deref(), Some("TCP のポート"));
        assert_eq!(f.group, FieldGroup::Advanced);
    }

    #[test]
    fn schema_lookup_and_default_is_empty() {
        assert!(SettingsSchema::default().is_empty());
        let s = SettingsSchema::default()
            .with_field(SettingField::new("a", "A", FieldKind::text()))
            .with_field(SettingField::new("b", "B", FieldKind::bool()));
        assert_eq!(s.fields.len(), 2);
        assert_eq!(s.field("b").map(|f| f.label.as_str()), Some("B"));
        assert!(s.field("c").is_none());
    }

    #[test]
    fn setting_value_text_form() {
        assert_eq!(SettingValue::from("R").to_text(), "R");
        assert_eq!(SettingValue::from(502).to_text(), "502");
        assert_eq!(SettingValue::from(true).to_text(), "true");
    }

    #[test]
    fn kind_name_spelling_matches_data_type() {
        for data_type in [
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
            let name = KindName::Numeric(data_type);
            assert_eq!(name.to_string(), data_type.to_string());
            assert_eq!(KindName::parse(&name.to_string()), Some(name));
        }
        assert_eq!(KindName::String.to_string(), "string");
        assert_eq!(KindName::parse("string"), Some(KindName::String));
        assert_eq!(KindName::parse("text"), None);
        assert_eq!(
            KindName::try_from("nope".to_owned()).unwrap_err().input(),
            "nope"
        );
    }

    #[test]
    fn kind_name_of_a_tag_kind() {
        use crate::{StringEncoding, TagKind};
        assert_eq!(
            KindName::of(&TagKind::numeric(DataType::F32)),
            Some(KindName::Numeric(DataType::F32))
        );
        assert_eq!(
            KindName::of(&TagKind::string(4, StringEncoding::Utf8)),
            Some(KindName::String)
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn schema_json_shape_round_trips() {
        let schema = SettingsSchema::new(vec![
            SettingField::new("host", "ホスト", FieldKind::host()).required(),
            SettingField::new("port", "ポート", FieldKind::integer(1, 65535)).default(502),
            SettingField::new(
                "cpu_type",
                "CPU",
                FieldKind::choice(vec![ChoiceOption::new("R", "iQ-R")]),
            )
            .default("R")
            .advanced(),
            SettingField::new("on", "有効", FieldKind::bool()).default(true),
            SettingField::new("pw", "パスワード", FieldKind::secret()),
        ]);
        let json = serde_json::to_value(&schema).unwrap();
        assert_eq!(
            json["fields"][0]["kind"],
            serde_json::json!({"type": "host"})
        );
        assert_eq!(
            json["fields"][1]["kind"],
            serde_json::json!({"type": "integer", "min": 1, "max": 65535})
        );
        assert_eq!(json["fields"][1]["default"], serde_json::json!(502));
        assert_eq!(json["fields"][2]["group"], serde_json::json!("advanced"));
        assert_eq!(json["fields"][2]["default"], serde_json::json!("R"));
        assert_eq!(json["fields"][3]["default"], serde_json::json!(true));
        assert_eq!(
            serde_json::from_value::<SettingsSchema>(json).unwrap(),
            schema
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn kind_name_serializes_as_a_plain_string() {
        let names = vec![KindName::Numeric(DataType::U16), KindName::String];
        let json = serde_json::to_string(&names).unwrap();
        assert_eq!(json, r#"["u16","string"]"#);
        assert_eq!(serde_json::from_str::<Vec<KindName>>(&json).unwrap(), names);
        assert!(serde_json::from_str::<KindName>(r#""text""#).is_err());
    }
}
