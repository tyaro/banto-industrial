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
