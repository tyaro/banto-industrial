//! The shapes that cross the read boundary (the former `PlcClient`, removed in R2b-3d-join): what to
//! read ([`ReadRequest`]) and what came back ([`ReadResult`]/[`TagValue`]).

use crate::address::Address;

// R1-1 (docs/banto-driver-api-design.md §3.1): the value/data-type vocabulary
// moved to the published, dependency-free `banto-driver-api` crate so that
// drivers outside this repository can share it. Re-exported here under the
// old names and paths (`banto_plc::types::DataType`, `banto_plc::PlcValue`, ...)
// so no caller changed. They are deliberately not `#[non_exhaustive]` - callers
// outside that crate (`banto-plc-write`'s encoder, banto-hub's write path)
// match on them exhaustively. `DataType::register_span` moved with the type
// (an inherent method can only live in the type's crate) and is now `pub`;
// this crate's planners and decoder call it unchanged. The tests that covered
// only these items (`data_type_parse_*`, `register_span_*`) moved with them.
/// The string-capable superset of [`TagValue`] - `banto_driver_api::Value`
/// under its pre-R1-1 name. See that type's documentation.
pub use banto_driver_api::Value as PlcValue;
pub use banto_driver_api::{DataType, StringEncoding, TagValue};

/// One tag to read: where, and how to interpret it. Cheap to construct in
/// bulk (`Copy`) - a collection group's poll cycle builds one of these per
/// enabled tag every period.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReadRequest {
    pub address: Address,
    pub data_type: DataType,
}

/// The outcome for one [`ReadRequest`] within a `PlcClient::read_batch` (removed)
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

/// One MELSEC string tag to read: where, how many consecutive 16-bit word
/// devices it occupies (`banto-tags::Tag::string_length`; SJIS capacity =
/// 2 bytes per word), and the [`StringEncoding`] to decode it with (T20 ①b -
/// pre-①b, decoding was unconditionally Shift-JIS; see [`StringEncoding`]'s
/// doc comment for why every pre-①b call site now passes
/// `StringEncoding::ShiftJis` explicitly rather than getting a default).
/// Byte order is fixed regardless of encoding - low byte of each word first,
/// trimmed at the first NUL by the driver engine's string decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StringReadRequest {
    pub address: Address,
    /// Consecutive word devices to read (1..=480 servable in one SLMP bulk
    /// read; the registry caps it at 128). Out-of-range values become a
    /// per-request `Bad`, never a panic.
    pub words: u16,
    /// T20 ①b: which `encoding_rs` table decodes the word span. Required
    /// (not `Option`/defaulted) for the same reason
    /// `StringWriteRequest::encoding` is - see [`StringEncoding`]'s doc
    /// comment.
    pub encoding: StringEncoding,
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

/// The outcome for one [`BatchReadRequest`] within a mixed batch - the
/// string-capable twin of [`ReadResult`], with the identical `Bad` semantics.
#[derive(Debug, Clone, PartialEq)]
pub enum BatchReadResult {
    Value(PlcValue),
    Bad(crate::error::PlcError),
}
