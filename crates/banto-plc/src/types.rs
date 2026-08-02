//! The shapes that cross [`crate::client::PlcClient`]'s boundary: what to
//! read ([`ReadRequest`]) and what came back ([`ReadResult`]/[`TagValue`]).

use crate::address::Address;

/// A tag's wire data type. Mirrors `banto-tags::ALLOWED_DATA_TYPES`
/// (`"bit" | "i16" | "u16" | "i32" | "u32" | "f32"`) one-for-one by design -
/// this crate does not depend on `banto-tags` (I2 does not depend on I1 in
/// docs/plan.md's dependency graph; I3 is the one that bridges them), so the
/// correspondence is kept in sync by convention and by [`DataType::parse`]'s
/// tests rather than a shared type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataType {
    Bit,
    I16,
    U16,
    I32,
    U32,
    F32,
}

impl DataType {
    /// Parse `banto-tags::Tag::data_type`'s string form. A convenience for
    /// I3's glue code (not used by this crate itself); returns `None` rather
    /// than a `PlcError` because an unrecognized string here is a
    /// `banto-tags` schema/`CHECK`-constraint bug, not a runtime PLC
    /// condition - callers should treat it as a programmer error, not
    /// something to fold into a per-tag `ReadResult::Bad`.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "bit" => Some(DataType::Bit),
            "i16" => Some(DataType::I16),
            "u16" => Some(DataType::U16),
            "i32" => Some(DataType::I32),
            "u32" => Some(DataType::U32),
            "f32" => Some(DataType::F32),
            _ => None,
        }
    }

    /// How many consecutive registers this type occupies (v1: bit types
    /// live in the coil/discrete-input area instead and this method is
    /// never called for them by [`crate::planning::plan_requests`] - see
    /// `element_span`'s doc comment there for why `Bit` still needs *a*
    /// answer rather than panicking).
    pub(crate) fn register_span(self) -> u16 {
        match self {
            DataType::Bit => 1,
            DataType::I16 | DataType::U16 => 1,
            DataType::I32 | DataType::U32 | DataType::F32 => 2,
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
        };
        f.write_str(s)
    }
}

/// One tag to read: where, and how to interpret it. Cheap to construct in
/// bulk (`Copy`) - a collection group's poll cycle builds one of these per
/// enabled tag every period.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReadRequest {
    pub address: Address,
    pub data_type: DataType,
}

/// A decoded reading, always widened to `f64` for numeric types (docs/plan.md
/// I2 §1: "数値は最終的にf64へ") - the scaling step (`banto-tags::scale_raw`)
/// and every downstream consumer (trend charts, thresholds) works in `f64`
/// regardless of the tag's wire width, so there is no reason to carry
/// `i16`/`u32`/`f32` past this crate's boundary. `Bit` stays a `bool`
/// because "scaled bit" is not a meaningful concept (recorder-requirements.md
/// has no scaling story for bit tags).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TagValue {
    Bit(bool),
    F64(f64),
}

/// The outcome for one [`ReadRequest`] within a [`crate::client::PlcClient::read_batch`]
/// call. `Bad` carries the [`crate::error::PlcError`] that explains *why*
/// this one request failed while its batch-mates may well have succeeded -
/// see `error.rs`'s module doc for the connection-level-vs-per-request split
/// that decides whether a failure becomes a `Bad` entry here or an `Err`
/// from `read_batch` itself.
#[derive(Debug, Clone, PartialEq)]
pub enum ReadResult {
    Value(TagValue),
    Bad(crate::error::PlcError),
}

// --- S1 string tags: the mixed (numeric + string) batch vocabulary ---------
//
// Why these are *parallel* types rather than a `TagValue::Str` variant and a
// new `ReadRequest` field: `TagValue`/`ReadRequest`/`WriteRequest` are `Copy`
// and are matched exhaustively / constructed as full struct literals by
// existing consumers (relay-wright's engine, banto-collect) whose behavior S1
// must leave untouched. A `String`-carrying variant would remove `Copy` and
// break every exhaustive `match` downstream, forcing app-side changes that
// belong to S2. So strings enter through a superset layer - [`BatchReadRequest`]
// / [`PlcValue`] / [`BatchReadResult`] - that wraps the existing numeric types
// unchanged; the numeric-only API keeps its exact shape and the S2 broker can
// move to the batch API to mix numeric and string reads in one call.

/// One MELSEC string tag to read: where, and how many consecutive 16-bit word
/// devices it occupies (`banto-tags::Tag::string_length`; SJIS capacity =
/// 2 bytes per word). Decoding is fixed - Shift-JIS, low byte of each word
/// first, trimmed at the first NUL - see `decode.rs::decode_string_value`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StringReadRequest {
    pub address: Address,
    /// Consecutive word devices to read (1..=480 servable in one SLMP bulk
    /// read; the registry caps it at 128). Out-of-range values become a
    /// per-request `Bad`, never a panic.
    pub words: u16,
}

/// One entry of a mixed batch: either an ordinary numeric/bit read (the
/// existing [`ReadRequest`], unchanged) or a string read. This is the
/// request type the S2 broker passes to `plan_slmp_batch` /
/// `SlmpClient::read_batch_mixed` so one wire round trip can serve both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BatchReadRequest {
    Numeric(ReadRequest),
    String(StringReadRequest),
}

impl From<ReadRequest> for BatchReadRequest {
    fn from(request: ReadRequest) -> Self {
        BatchReadRequest::Numeric(request)
    }
}

/// A decoded reading that can also carry a string - the string-capable
/// superset of [`TagValue`], used by the mixed-batch API only. Not `Copy`
/// (a `String` cannot be), which is exactly why it is a separate type; see
/// the module-level note above.
#[derive(Debug, Clone, PartialEq)]
pub enum PlcValue {
    Bit(bool),
    F64(f64),
    /// Shift-JIS text decoded from the tag's word span, trimmed at the first
    /// NUL terminator (so trailing 0x00 padding never survives into the
    /// value).
    Str(String),
}

impl From<TagValue> for PlcValue {
    fn from(value: TagValue) -> Self {
        match value {
            TagValue::Bit(b) => PlcValue::Bit(b),
            TagValue::F64(v) => PlcValue::F64(v),
        }
    }
}

impl PlcValue {
    /// The numeric/bit projection, `None` for a string - the bridge back to
    /// the legacy [`TagValue`]-shaped API where a string can never appear.
    pub fn as_tag_value(&self) -> Option<TagValue> {
        match self {
            PlcValue::Bit(b) => Some(TagValue::Bit(*b)),
            PlcValue::F64(v) => Some(TagValue::F64(*v)),
            PlcValue::Str(_) => None,
        }
    }
}

/// The outcome for one [`BatchReadRequest`] within a mixed batch - the
/// string-capable twin of [`ReadResult`], with the identical `Bad` semantics.
#[derive(Debug, Clone, PartialEq)]
pub enum BatchReadResult {
    Value(PlcValue),
    Bad(crate::error::PlcError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_type_parse_round_trips_every_banto_tags_string() {
        // Kept in sync with banto-tags::ALLOWED_DATA_TYPES by hand (see this
        // module's doc comment) - this test is the tripwire: if someone adds
        // a data type to banto-tags without adding it here, this list (not a
        // shared constant) is what has to be remembered to update, and this
        // test at least proves the reverse direction (Display) matches too.
        for s in ["bit", "i16", "u16", "i32", "u32", "f32"] {
            let dt = DataType::parse(s).unwrap_or_else(|| panic!("{s} should parse"));
            assert_eq!(dt.to_string(), s);
        }
    }

    #[test]
    fn data_type_parse_rejects_unknown_string() {
        assert_eq!(DataType::parse("f64"), None);
        assert_eq!(DataType::parse(""), None);
    }

    #[test]
    fn register_span_is_one_for_16_bit_and_two_for_32_bit() {
        assert_eq!(DataType::I16.register_span(), 1);
        assert_eq!(DataType::U16.register_span(), 1);
        assert_eq!(DataType::I32.register_span(), 2);
        assert_eq!(DataType::U32.register_span(), 2);
        assert_eq!(DataType::F32.register_span(), 2);
    }
}
