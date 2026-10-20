//! Values and data types that cross the driver boundary.
//!
//! Two groups live here (docs/banto-driver-api-design.md §3.1 / §3.2):
//!
//! - **Moved from `banto-plc` (R1-1), not `#[non_exhaustive]`**: [`DataType`],
//!   [`TagValue`], [`Value`] (`banto_plc::PlcValue`), [`StringEncoding`],
//!   [`WordOrder`]. `banto-plc` re-exports them under their old names and
//!   paths. Their logic (decoding, word-order math, string encode/decode)
//!   stays in `banto-plc` / `banto-plc-write`; only the vocabulary moved.
//! - **New in the API (WIT `types`), `#[non_exhaustive]`**: [`RawValue`],
//!   [`TagKind`], [`TagSpec`], [`WriteItem`].

use crate::address::DriverAddress;

/// A tag's wire data type. Mirrors `banto-tags::ALLOWED_DATA_TYPES`
/// (`"bit" | "i16" | "u16" | "i32" | "u32" | "f32" | "i64" | "u64" | "f64"`)
/// one-for-one by design - neither crate depends on the other, so the
/// correspondence is kept in sync by convention and by [`DataType::parse`]'s
/// tests rather than a shared type. The same spelling is the WIT
/// `tag-spec.data-type` vocabulary (design §9.2).
///
/// There is deliberately no `String` variant: strings are
/// [`TagKind::Str`], and `DataType::parse("string")` must stay `None`
/// (`banto-collect`'s registry filter relies on it, design §3.1).
///
/// The type does not know which protocol a tag belongs to: whether a given
/// data type may be *registered* for a given protocol is a `banto-tags`
/// decision (it restricts `I64`/`U64`/`F64` to Modbus connections, owner
/// decision 2026-09-08).
///
/// `I64`/`U64` values are widened to [`TagValue::F64`] like every other
/// numeric type - integers beyond `2^53` lose precision once represented as
/// `f64`. This is an accepted limitation (owner decision, 2026-09-08), not a
/// bug: a KM-D1-ETN-class instrument's readings never approach that
/// magnitude in practice.
///
/// Not `#[non_exhaustive]` (design §3.1): adding a variant is a breaking
/// change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
pub enum DataType {
    /// One bit (a coil / discrete input, a bit device, or a bit inside a
    /// word when the address carries a bit index).
    Bit,
    /// Signed 16-bit integer (one register).
    I16,
    /// Unsigned 16-bit integer (one register).
    U16,
    /// Signed 32-bit integer (two registers).
    I32,
    /// Unsigned 32-bit integer (two registers).
    U32,
    /// IEEE 754 single precision (two registers).
    F32,
    /// Signed 64-bit integer (four registers).
    I64,
    /// Unsigned 64-bit integer (four registers).
    U64,
    /// IEEE 754 double precision (four registers).
    F64,
}

impl DataType {
    /// Parse `banto-tags::Tag::data_type`'s string form (the same spelling
    /// [`Display`](std::fmt::Display) writes). Returns `None` rather than an
    /// error because an unrecognized string here is a `banto-tags`
    /// schema/`CHECK`-constraint bug, not a runtime PLC condition - callers
    /// should treat it as a programmer error, not something to fold into a
    /// per-tag Bad result.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "bit" => Some(DataType::Bit),
            "i16" => Some(DataType::I16),
            "u16" => Some(DataType::U16),
            "i32" => Some(DataType::I32),
            "u32" => Some(DataType::U32),
            "f32" => Some(DataType::F32),
            "i64" => Some(DataType::I64),
            "u64" => Some(DataType::U64),
            "f64" => Some(DataType::F64),
            _ => None,
        }
    }

    /// How many consecutive 16-bit registers / word devices this type
    /// occupies: 1 for 16-bit types, 2 for 32-bit, 4 for 64-bit.
    ///
    /// `Bit` answers 1 rather than panicking: a bit lives in the
    /// coil/discrete-input area (or is one bit of a word), and `banto-plc`'s
    /// planners never ask for its register span on the coil path, but the
    /// method is total so a bit-in-word read can still count the one word it
    /// reads. This is also the unit of [`TagKind::word_count`] and
    /// [`DriverAddress::word_count`].
    ///
    /// Public since R1-1 (it was `pub(crate)` in `banto-plc`): an inherent
    /// method can only be defined in the type's own crate, and `banto-plc`'s
    /// planners and decoder still call it.
    pub fn register_span(self) -> u16 {
        match self {
            DataType::Bit => 1,
            DataType::I16 | DataType::U16 => 1,
            DataType::I32 | DataType::U32 | DataType::F32 => 2,
            DataType::I64 | DataType::U64 | DataType::F64 => 4,
        }
    }
}

impl std::fmt::Display for DataType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            DataType::Bit => "bit",
            DataType::I16 => "i16",
            DataType::U16 => "u16",
            DataType::I32 => "i32",
            DataType::U32 => "u32",
            DataType::F32 => "f32",
            DataType::I64 => "i64",
            DataType::U64 => "u64",
            DataType::F64 => "f64",
        };
        f.write_str(s)
    }
}

/// A decoded numeric/bit reading, always widened to `f64` for numeric types
/// (docs/plan.md I2 §1: "数値は最終的にf64へ") - scaling and every downstream
/// consumer (trend charts, thresholds) works in `f64` regardless of the
/// tag's wire width. `Bit` stays a `bool` because "scaled bit" is not a
/// meaningful concept.
///
/// `I64`/`U64` values are widened into this same `f64` unchanged (owner
/// decision, 2026-09-08) - see [`DataType`]'s doc comment.
///
/// `Copy`, and matched exhaustively by existing consumers; the
/// string-capable superset is [`Value`]. A candidate to fold into [`Value`]
/// in R2b (design §3.1). Not `#[non_exhaustive]`.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
pub enum TagValue {
    /// A bit.
    Bit(bool),
    /// Any numeric type, widened to `f64`.
    F64(f64),
}

/// Character encoding of a string tag's word span (T20 ①b,
/// docs/banto-hub-t20-design.md §3.1). Selects which `encoding_rs` table
/// `banto-plc` decodes with / `banto-plc-write` encodes with; the
/// byte-packing convention itself (low byte first within each word -
/// MELSEC's storage convention) is unaffected by this choice, exactly as
/// [`WordOrder`] only ever governs multi-word numeric order, never a
/// string's byte order within a word.
///
/// Required wherever a string read/write is built (never silently
/// defaulted), so pre-T20 call sites state `ShiftJis` explicitly.
/// Not `#[non_exhaustive]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
pub enum StringEncoding {
    /// The hub's default for newly registered string tags.
    Utf8,
    /// Shift-JIS: the pre-T20 read behaviour, and relay-wright's only
    /// encoding.
    ShiftJis,
}

/// A decoded reading that can also carry a string - the string-capable
/// superset of [`TagValue`]. Not `Copy` (a `String` cannot be), which is why
/// it is a separate type. `banto-plc` re-exports it as `PlcValue`
/// (`pub use banto_driver_api::Value as PlcValue`).
///
/// This is the host's value type: what [`WriteItem::value`] carries and what
/// the engine maps a driver's [`RawValue`] into. Not `#[non_exhaustive]`.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
pub enum Value {
    /// A bit.
    Bit(bool),
    /// Any numeric type, widened to `f64`.
    F64(f64),
    /// Text decoded from the tag's word span with its [`StringEncoding`],
    /// trimmed at the first NUL terminator (so trailing 0x00 padding never
    /// survives into the value).
    Str(String),
}

impl From<TagValue> for Value {
    fn from(value: TagValue) -> Self {
        match value {
            TagValue::Bit(b) => Value::Bit(b),
            TagValue::F64(v) => Value::F64(v),
        }
    }
}

impl Value {
    /// The numeric/bit projection, `None` for a string - the bridge back to
    /// the legacy [`TagValue`]-shaped API where a string can never appear.
    pub fn as_tag_value(&self) -> Option<TagValue> {
        match self {
            Value::Bit(b) => Some(TagValue::Bit(*b)),
            Value::F64(v) => Some(TagValue::F64(*v)),
            Value::Str(_) => None,
        }
    }
}

/// Which register holds the high 16 bits of a 32-bit value, or the highest
/// 16 bits of a 4-register 64-bit value (docs/plan.md I2 §5). Byte order
/// *within* a register is fixed by the protocol and is not a parameter -
/// only the order of the register group varies by device, which is what
/// this controls.
///
/// Only the vocabulary lives here; combining registers per order stays in
/// `banto-plc`'s decoder. As a connection setting it is the value of
/// [`keys::WORD_ORDER`](crate::settings::keys::WORD_ORDER) (`"high_low"` /
/// `"low_high"`), passed per connection, never per read unit (design §3.5).
///
/// The default is `HighLow` (the Modbus convention); note that
/// `banto_plc::SlmpConfig`'s default is `LowHigh` on purpose. Not
/// `#[non_exhaustive]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
pub enum WordOrder {
    /// First register (lowest offset) holds the highest word - the
    /// Modbus/IEEE convention and the default. For a 4-register value
    /// `regs[0]` is the most significant word and `regs[3]` the least.
    #[default]
    HighLow,
    /// First register holds the lowest word - common on drives/instruments
    /// that treat the register group as a little-endian machine word. For a
    /// 4-register value this reverses the whole group. The Omron KM-D1-ETN
    /// power meter (KANC-718B §12.5) uses this ordering for its `f64`
    /// measurement registers.
    LowHigh,
}

/// A raw value as a frame-type driver's decode returns it (WIT
/// `variant value { bool, int(s64), uint(u64), float(f64), bytes(list<u8>) }`).
/// The host maps it to [`Value`]: integers widen to `f64`, and bytes become
/// text with the [`TagKind::Str`] encoding.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
pub enum RawValue {
    /// WIT `bool`.
    Bool(bool),
    /// WIT `int(s64)`.
    Int(i64),
    /// WIT `uint(u64)`.
    Uint(u64),
    /// WIT `float(f64)`.
    Float(f64),
    /// WIT `bytes(list<u8>)` - a string tag's raw word span.
    Bytes(Vec<u8>),
}

/// The *kind* of one read/write unit (WIT `tag-spec.data-type` /
/// `string-words`, design §9.2).
///
/// `banto-plc`'s `slmp::planning::ReadKind` has a third `BitInWord` case; the
/// API does not: a bit inside a word is [`DriverAddress::bit`] plus
/// `Numeric(DataType::Bit)`, and "read the word, extract the bit" is the
/// driver's planning detail.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
pub enum TagKind {
    /// A number or a bit.
    Numeric(DataType),
    /// A string.
    ///
    /// The encoding is host-side information that WIT does not carry (there
    /// the plugin returns bytes and the host decodes); R1's adapters decode
    /// with `banto-plc`, so it travels here (design §13 item 5).
    Str {
        /// 16-bit words the string occupies (`banto-tags`'
        /// `string_length`, 1..=128).
        words: u16,
        /// How the word span's bytes are text.
        encoding: StringEncoding,
    },
}

impl TagKind {
    /// A numeric or bit tag.
    pub fn numeric(data_type: DataType) -> Self {
        TagKind::Numeric(data_type)
    }

    /// A string tag of `words` 16-bit words in `encoding`. The range is not
    /// checked here (the registry caps it at 128; a driver reports an
    /// out-of-range span as [`ItemError::Unsupported`](crate::ItemError)).
    pub fn string(words: u16, encoding: StringEncoding) -> Self {
        TagKind::Str { words, encoding }
    }

    /// 16-bit words this tag occupies: [`DataType::register_span`] for
    /// `Numeric`, `words` for `Str`.
    pub fn word_count(&self) -> u16 {
        match self {
            TagKind::Numeric(data_type) => data_type.register_span(),
            TagKind::Str { words, .. } => *words,
        }
    }
}

/// One tag to read (WIT `record tag-spec { address, data-type, string-words }`).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TagSpec {
    /// Where, as the driver parsed it.
    pub address: DriverAddress,
    /// How to interpret it.
    pub kind: TagKind,
}

impl TagSpec {
    /// A tag at `address` of `kind`.
    pub fn new(address: DriverAddress, kind: TagKind) -> Self {
        TagSpec { address, kind }
    }
}

/// One tag to write (WIT `record write-item { spec, value }`).
///
/// `value` is the host's [`Value`]. A WASM host maps `Bit` to WIT `bool`,
/// `F64` to `float` and `Str` to `bytes` (encoded with the spec's
/// encoding); the write side never uses WIT `int` / `uint` - rounding to an
/// integer type and the range check are the driver's job (design §9.2).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct WriteItem {
    /// Which tag.
    pub spec: TagSpec,
    /// What to write.
    pub value: Value,
}

impl WriteItem {
    /// Write `value` to `spec`.
    pub fn new(spec: TagSpec, value: Value) -> Self {
        WriteItem { spec, value }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::address::DriverId;

    // --- moved from banto-plc/src/types.rs (R1-1, names unchanged) ---------

    #[test]
    fn data_type_parse_round_trips_every_banto_tags_string() {
        // Kept in sync with banto-tags::ALLOWED_DATA_TYPES by hand (see
        // DataType's doc comment) - this test is the tripwire: if someone adds
        // a data type to banto-tags without adding it here, this list (not a
        // shared constant) is what has to be remembered to update, and this
        // test at least proves the reverse direction (Display) matches too.
        for s in [
            "bit", "i16", "u16", "i32", "u32", "f32", "i64", "u64", "f64",
        ] {
            let dt = DataType::parse(s).unwrap_or_else(|| panic!("{s} should parse"));
            assert_eq!(dt.to_string(), s);
        }
    }

    #[test]
    fn data_type_parse_rejects_unknown_string() {
        assert_eq!(DataType::parse("i128"), None);
        assert_eq!(DataType::parse("f16"), None);
        assert_eq!(DataType::parse(""), None);
    }

    #[test]
    fn register_span_is_one_for_16_bit_two_for_32_bit_and_four_for_64_bit() {
        assert_eq!(DataType::I16.register_span(), 1);
        assert_eq!(DataType::U16.register_span(), 1);
        assert_eq!(DataType::I32.register_span(), 2);
        assert_eq!(DataType::U32.register_span(), 2);
        assert_eq!(DataType::F32.register_span(), 2);
        assert_eq!(DataType::I64.register_span(), 4);
        assert_eq!(DataType::U64.register_span(), 4);
        assert_eq!(DataType::F64.register_span(), 4);
    }

    // --- new in R1-1 -------------------------------------------------------

    #[test]
    fn data_type_parse_does_not_accept_string() {
        // design §3.1: a `String` variant must not appear, or
        // banto-collect's registry filter (`DataType::parse(..).is_none()`
        // for string tags) changes meaning. Also case-sensitive and untrimmed,
        // exactly as before the move.
        assert_eq!(DataType::parse("string"), None);
        assert_eq!(DataType::parse("I16"), None);
        assert_eq!(DataType::parse(" i16"), None);
    }

    #[test]
    fn value_from_tag_value_and_back() {
        assert_eq!(Value::from(TagValue::Bit(true)), Value::Bit(true));
        assert_eq!(Value::from(TagValue::F64(1.5)), Value::F64(1.5));
        assert_eq!(Value::Bit(false).as_tag_value(), Some(TagValue::Bit(false)));
        assert_eq!(Value::F64(-2.0).as_tag_value(), Some(TagValue::F64(-2.0)));
        assert_eq!(Value::Str("abc".into()).as_tag_value(), None);
    }

    #[test]
    fn word_order_defaults_to_high_low() {
        assert_eq!(WordOrder::default(), WordOrder::HighLow);
    }

    #[test]
    fn tag_kind_word_count_is_register_span_or_words() {
        assert_eq!(TagKind::numeric(DataType::Bit).word_count(), 1);
        assert_eq!(TagKind::numeric(DataType::I16).word_count(), 1);
        assert_eq!(TagKind::numeric(DataType::F32).word_count(), 2);
        assert_eq!(TagKind::numeric(DataType::U64).word_count(), 4);
        let s = TagKind::string(10, StringEncoding::ShiftJis);
        assert_eq!(s.word_count(), 10);
        assert_eq!(
            s,
            TagKind::Str {
                words: 10,
                encoding: StringEncoding::ShiftJis
            }
        );
    }

    #[test]
    fn tag_spec_and_write_item_constructors_keep_their_parts() {
        let driver = DriverId::new("modbus-tcp").unwrap();
        let address = DriverAddress::new(driver, "400001", "holding", true, 2);
        let spec = TagSpec::new(address.clone(), TagKind::numeric(DataType::F32));
        assert_eq!(spec.address, address);
        assert_eq!(spec.kind, TagKind::Numeric(DataType::F32));
        let item = WriteItem::new(spec.clone(), Value::F64(1.25));
        assert_eq!(item.spec, spec);
        assert_eq!(item.value, Value::F64(1.25));
    }
}
