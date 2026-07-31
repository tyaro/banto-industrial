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
