//! The shapes that cross [`crate::client::PlcWriteClient`]'s boundary: what to
//! write ([`WriteRequest`]) and what came back ([`WriteResult`]).
//!
//! ## Why the value is a reused `banto_plc::TagValue`, not a new `WriteValue`
//!
//! I5's brief left this open: reuse [`banto_plc::TagValue`] if it fits, or
//! define a minimal `WriteValue` here if it does not. It fits, and reusing it
//! is the more symmetric choice. `TagValue` is `Bit(bool) | F64(f64)` - a plain
//! carrier, *not* quality-tagged (read quality lives in `ReadResult::Bad`, a
//! separate thing), so nothing read-shaped leaks into a write. And the two
//! directions become exact inverses: a read *decodes* a wire register window
//! into `TagValue::F64` (widening every numeric width to `f64`, see
//! `banto-plc/src/decode.rs`), and a write *encodes* a `TagValue::F64` back down
//! to the target register width (see [`crate::encode`]). Carrying the value as
//! `f64` means the narrowing - and the range/precision checks it needs - all
//! live in one place ([`crate::encode`]) and surface as a per-request
//! [`crate::error::PlcWriteError::ValueOutOfRange`], never a silent truncation.
//! A bespoke `WriteValue { U16(u16), I32(i32), ... }` would push that
//! validation onto every caller instead.

use banto_plc::{Address, DataType, TagValue};

/// One target to write: where, how to interpret it on the wire, and the value.
/// `Copy` like [`banto_plc::ReadRequest`], so a rule engine can build these in
/// bulk cheaply.
///
/// `value`'s kind must match `data_type`: a [`TagValue::Bit`] for
/// `DataType::Bit`, a [`TagValue::F64`] for every numeric type. A mismatch is a
/// per-request [`crate::error::PlcWriteError::ValueTypeMismatch`], resolved
/// before any wire traffic.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WriteRequest {
    pub address: Address,
    pub data_type: DataType,
    pub value: TagValue,
}

/// The outcome for one [`WriteRequest`] within a
/// [`crate::client::PlcWriteClient::write_batch`] call. The write twin of
/// `banto_plc::ReadResult`: a bare [`WriteResult::Ok`] (there is no value to
/// hand back on a successful write) or a [`WriteResult::Bad`] carrying the
/// per-request reason. See `banto-plc/src/error.rs` and
/// [`crate::error::PlcWriteError::is_connection_fatal`] for the split that
/// decides whether a failure becomes a `Bad` here or an `Err` from
/// `write_batch` itself.
#[derive(Debug, Clone, PartialEq)]
pub enum WriteResult {
    Ok,
    Bad(crate::error::PlcWriteError),
}

impl WriteResult {
    /// Convenience for `matches!(self, WriteResult::Ok)`, so callers surfacing a
    /// batch outcome do not each re-spell it.
    pub fn is_ok(&self) -> bool {
        matches!(self, WriteResult::Ok)
    }
}

// --- S1 string tags: the mixed (numeric + string) write batch ---------------
//
// Same reasoning as `banto_plc::BatchReadRequest` (see banto-plc/src/types.rs):
// `WriteRequest`/`TagValue` are `Copy` and consumed as-is by relay-wright's
// engine, so a `String`-carrying value cannot be folded into them without
// app-side changes that belong to S2. Strings enter through a parallel layer
// that wraps the numeric type unchanged.

/// One MELSEC string target to write: where, the fixed span in consecutive
/// 16-bit word devices (`banto-tags::Tag::string_length`), and the text.
///
/// The encoded Shift-JIS bytes must fit `2 * words` bytes; anything longer is
/// a per-request [`crate::error::PlcWriteError::ValueOutOfRange`] and nothing
/// is written - **never** a silent truncation, because a recipe string cut
/// mid-way is a real hazard on a live PLC. Shorter strings are padded with
/// 0x00 to the full span, so a longer previous value can never bleed through
/// the tail of the window.
#[derive(Debug, Clone, PartialEq)]
pub struct StringWriteRequest {
    pub address: Address,
    /// Consecutive word devices the string occupies (the registry caps this
    /// at 128; the wire itself at 960 per bulk write). The whole span is
    /// always written: encoded bytes first, 0x00 padding to the end.
    pub words: u16,
    pub value: String,
}

/// One entry of a mixed write batch: an ordinary numeric/bit write (the
/// existing [`WriteRequest`], unchanged), a string write, or a T8 bit-in-word
/// RMW write (docs/tag-server-design.md §6.1). The request type for
/// `plan_slmp_write_batch` / `SlmpWriteClient::write_batch_mixed` - and, via
/// `banto-broker`'s `Job::Write`, the type every broker-mediated write
/// speaks, unchanged since T2 (adding this variant required no broker code
/// change - see `slmp::planning`'s and `slmp::mod`'s module docs for why).
#[derive(Debug, Clone, PartialEq)]
pub enum BatchWriteRequest {
    Numeric(WriteRequest),
    String(StringWriteRequest),
    /// Set or clear a single bit of a *word* device without disturbing its
    /// other 15 bits (§6.1: SLMP has no dedicated bit-in-word write command,
    /// unlike Modbus's FC22 Mask Write Register - see `slmp::planning`'s
    /// module doc for why this is therefore a read/modify/write/verify
    /// sequence rather than one wire operation).
    ///
    /// `address` must carry a bit position ([`banto_plc::Address::as_slmp`]'s
    /// third element - i.e. parsed from `"D100.5"` notation, not a plain
    /// `"D100"`) naming a **word** device; anything else is a per-request
    /// [`crate::error::PlcWriteError::UnsupportedCombination`], resolved
    /// before any wire traffic exactly like every other address/data-type
    /// mismatch this crate rejects.
    BitInWord {
        address: Address,
        value: bool,
    },
}

impl From<WriteRequest> for BatchWriteRequest {
    fn from(request: WriteRequest) -> Self {
        BatchWriteRequest::Numeric(request)
    }
}
