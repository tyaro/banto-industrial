//! Modbus TCP *write* request planning (#131 前半スライス): the mirror image
//! of `crate::slmp::planning` for Modbus, and (going one layer further back)
//! of `banto-plc/src/planning.rs`'s *read* planner for the same protocol.
//! Same job - turn a flat list of targets into the minimal set of wire
//! operations - and the same purity (no I/O, no socket type at all).
//!
//! ## Read this first: `crate::slmp::planning`'s "one rule that differs from
//! the read planner" section applies here unchanged
//!
//! A write must never merge two targets that are not *exactly* adjacent -
//! writing a device the caller did not ask to write is a real, possibly
//! destructive side effect on a live PLC, unlike a harmless over-*read*. The
//! SLMP write planner enforces this with a gap-tolerance-zero rule instead of
//! the read planner's `MAX_GAP`, and this module enforces the identical rule
//! for Modbus: [`plan_modbus_writes`] merges two targets into one FC15/FC16
//! group only when the second begins exactly where the first ends. See
//! [`writes_never_merge_across_a_gap`] below - the same tripwire name the
//! SLMP module uses, guarding the same property.
//!
//! ## Which Modbus areas are writable at all (2026-09-01 オーナー決定A)
//!
//! Only [`AddressArea::Coil`] (`0xxxx`) and [`AddressArea::HoldingRegister`]
//! (`4xxxx`) are writable by Modbus wire protocol - [`AddressArea::DiscreteInput`]
//! (`1xxxx`) and [`AddressArea::InputRegister`] (`3xxxx`) are read-only,
//! permanently, for every implementation (this is the *device*'s wire
//! behavior, not something this driver or any other could work around). A
//! request targeting either read-only area is resolved into
//! [`ModbusWritePlanOutcome::immediate_bad`] as
//! [`PlcWriteError::ModbusReadOnlyArea`], before any wire traffic - the exact
//! same defense-in-depth relationship the read side has with
//! `banto-tags::tag::validate_tag_input`'s registration-time check (that
//! check stops a `writable` tag from ever being *defined* on a read-only
//! area; this one stops a write from reaching the wire even if a caller
//! constructs a [`crate::types::WriteRequest`] directly, bypassing the
//! registry).
//!
//! ## Quantity limits differ from the read side - do not reuse its constants
//!
//! [`MAX_WRITE_REGISTERS`]/[`MAX_WRITE_COILS`] are the Modbus **write** caps
//! (FC16: 123 registers: FC15: 1968 coils), not
//! `banto_plc::planning::MAX_REGISTERS_PER_READ`/`MAX_COILS_PER_READ` (125/
//! 2000), which are the **read** caps (FC3/4: 125 registers, FC1/2: 2000
//! coils) bound by a different rule (the response payload, which carries
//! data, versus the request payload here, which does not need to leave room
//! for a reply). See each constant's own doc comment for the PDU-size
//! derivation.
//!
//! ## Single vs. multiple function codes: decided here, executed in `mod.rs`
//!
//! This module's plans do not name a function code - a [`ModbusPlannedWrite`]
//! is just "this area, this contiguous range, this payload, these request
//! indices". `super::execute_modbus_writes` picks FC5/FC6 when a plan's
//! payload has exactly one element and FC15/FC16 otherwise (the module doc
//! at `crates/banto-plc-write/src/modbus/mod.rs` explains why single-element
//! groups deliberately prefer the single-write function codes over a
//! one-element FC15/FC16, which the wire protocol also allows). Keeping that
//! choice in the executor rather than here mirrors the SLMP split, where
//! `slmp::planning` never touches the wrapped crate's `bulk_write` call
//! either.
//!
//! ## What is deliberately out of scope for this slice (#131 前半)
//!
//! - **Bit-in-word writes** (`BatchWriteRequest::BitInWord`,
//!   docs/tag-server-design.md §6.1): SLMP needs a read/modify/write RMW
//!   because it has no bit-in-word write command, but Modbus has FC22 (Mask
//!   Write Register), which can do the same job *atomically* - no RMW race,
//!   no confirmation read. Implementing FC22 is left to whenever a caller
//!   actually needs Modbus bit-in-word writes (§6.1 already records this:
//!   "Modbus 書き込み（I9）実装時は FC22...が I9 の設計材料"). A `WriteRequest`
//!   whose address carries a bit-in-word qualifier (`"40001.3"`) is rejected
//!   as [`PlcWriteError::UnsupportedCombination`], the same way the SLMP
//!   planner rejects a bit-qualified address on its own plain (non-`BitInWord`)
//!   write request - see `a_bit_qualified_address_on_a_plain_numeric_write_is_immediately_bad`
//!   there.
//! - **String writes**: MELSEC string devices are an SLMP-specific concept
//!   (`banto-tags`'s `string` data type exists for MELSEC string devices,
//!   docs/tag-server-design.md's S1) with no Modbus equivalent in this
//!   system, so there is no Modbus counterpart to
//!   `crate::slmp::planning::plan_slmp_write_batch`'s string branch. Only the
//!   numeric+bit [`crate::types::WriteRequest`] shape is planned here.

use std::collections::BTreeMap;

use banto_plc::{AddressArea, DataType, WordOrder};

use crate::encode::{encode_bit_value, encode_word_value};
use crate::error::PlcWriteError;
use crate::types::WriteRequest;

/// Modbus **write** register cap (FC16, Write Multiple Registers): the
/// Modbus Application Protocol Specification V1.1b3 §6.12 states 123
/// registers, which also falls straight out of the 253-byte PDU limit: a
/// FC16 request PDU totals `function(1) + start(2) + quantity(2) +
/// byte_count(1) + data(2*N)`, i.e. `6 + 2N` bytes, and `6 + 2*123 = 252 <=
/// 253 < 6 + 2*124`. **Not** `banto_plc::planning::MAX_REGISTERS_PER_READ`
/// (125, FC3/4's read cap) - see this module's doc comment.
pub const MAX_WRITE_REGISTERS: u16 = 123;

/// Modbus **write** coil cap (FC15, Write Multiple Coils): the spec (§6.11)
/// states 1968 coils. The same `6 + data_len <= 253` PDU budget as
/// [`MAX_WRITE_REGISTERS`] would allow slightly more (`data_len =
/// ceil(N/8) <= 247` => `N <= 1976`); the spec's 1968 = `246 * 8` is the
/// number actually specified, and this driver follows the spec's number
/// rather than the looser byte-budget bound. **Not**
/// `banto_plc::planning::MAX_COILS_PER_READ` (2000, FC1/2's read cap) - see
/// this module's doc comment.
pub const MAX_WRITE_COILS: u16 = 1968;

/// v1 device/data-type restriction, the write-side twin of
/// `banto_plc::planning`'s read-side compatibility check: [`DataType::Bit`]
/// writes [`AddressArea::Coil`], every other type writes
/// [`AddressArea::HoldingRegister`]. [`AddressArea::DiscreteInput`]/
/// [`AddressArea::InputRegister`] are filtered out earlier (read-only, never
/// reach this check) - see [`plan_modbus_writes`].
fn is_compatible(area: AddressArea, data_type: DataType) -> bool {
    match area {
        AddressArea::Coil => data_type == DataType::Bit,
        AddressArea::HoldingRegister => data_type != DataType::Bit,
        AddressArea::DiscreteInput | AddressArea::InputRegister => false,
    }
}

/// One encoded target awaiting grouping - the Modbus twin of
/// `crate::slmp::planning::ItemPayload`.
enum ItemPayload {
    Words(Vec<u16>),
    Bit(bool),
}

/// The ready-to-serialize payload of one planned write: a contiguous
/// register window or a contiguous run of coils. Which one is implied by
/// `area` ([`AddressArea::Coil`] => `Bits`, [`AddressArea::HoldingRegister`]
/// => `Words`), exactly as `crate::slmp::planning::WritePayload` mirrors its
/// device's access kind.
#[derive(Debug, Clone, PartialEq)]
pub enum WritePayload {
    Words(Vec<u16>),
    Bits(Vec<bool>),
}

impl WritePayload {
    /// Element count: registers for a word write, coils for a bit write -
    /// what decides FC5/FC6 (single, `len() == 1`) vs. FC15/FC16 (multiple)
    /// in `super::execute_modbus_writes`.
    pub fn len(&self) -> usize {
        match self {
            WritePayload::Words(w) => w.len(),
            WritePayload::Bits(b) => b.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One physical Modbus write: `super::execute_modbus_writes` issues exactly
/// one wire request per [`ModbusPlannedWrite`] (FC5/FC6 if `payload.len() ==
/// 1`, FC15/FC16 otherwise). `request_indices` lists every input request
/// this group fulfils, in ascending offset order, mirroring
/// `crate::slmp::planning::SlmpPlannedWrite`.
#[derive(Debug, Clone, PartialEq)]
pub struct ModbusPlannedWrite {
    pub area: AddressArea,
    pub start_offset: u16,
    pub payload: WritePayload,
    pub request_indices: Vec<usize>,
}

/// [`plan_modbus_writes`]'s full result. Same contract as every other
/// planner in this codebase: every input index appears exactly once, either
/// inside some [`ModbusPlannedWrite`]'s `request_indices` or in
/// `immediate_bad`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ModbusWritePlanOutcome {
    pub writes: Vec<ModbusPlannedWrite>,
    pub immediate_bad: Vec<(usize, PlcWriteError)>,
}

/// A group under construction, generic over its element type (`u16` word or
/// `bool` coil) - the Modbus twin of `crate::slmp::planning`'s
/// `BuildingWords`/`BuildingBits`, merged into one generic type since Modbus
/// grouping never needs to tell the two apart mid-build (unlike SLMP's
/// per-device dispatch, a Modbus group's element type is fixed the moment its
/// `area` is fixed).
struct Building<T> {
    start: u64,
    buffer: Vec<T>,
    request_indices: Vec<usize>,
}

/// Plan wire-level Modbus writes for `requests`, encoding each value per
/// `word_order` (must match the read side's configured order - see
/// `crate::encode` - or a write/read round trip disagrees on 32-bit values).
///
/// Requests that cannot reach the wire are resolved into
/// [`ModbusWritePlanOutcome::immediate_bad`] rather than attempted and
/// failed:
///
/// - a non-Modbus address ([`PlcWriteError::AddressProtocolMismatch`])
/// - a bit-in-word-qualified address (`"40001.3"`) - not supported for an
///   ordinary write this slice (see this module's doc comment)
///   ([`PlcWriteError::UnsupportedCombination`])
/// - [`AddressArea::DiscreteInput`]/[`AddressArea::InputRegister`] - always
///   read-only ([`PlcWriteError::ModbusReadOnlyArea`])
/// - a `bit` type at [`AddressArea::HoldingRegister`] or vice versa
///   ([`PlcWriteError::UnsupportedCombination`])
/// - a value that will not encode into its register
///   ([`PlcWriteError::ValueOutOfRange`] / [`PlcWriteError::ValueTypeMismatch`])
///
/// A target whose address does not exist on the device cannot be caught
/// here; that surfaces as a per-request [`PlcWriteError::ModbusException`] at
/// write time.
pub fn plan_modbus_writes(
    requests: &[WriteRequest],
    word_order: WordOrder,
) -> ModbusWritePlanOutcome {
    let mut immediate_bad = Vec::new();

    // BTreeMap keyed by area for deterministic ordering (Coil groups before
    // HoldingRegister groups), same reasoning as the SLMP planner's
    // `BTreeMap<SlmpDevice, _>`.
    let mut by_area: BTreeMap<AddressArea, Vec<(usize, u16, ItemPayload)>> = BTreeMap::new();

    for (index, req) in requests.iter().enumerate() {
        let Some((area, offset, bit)) = req.address.as_modbus_ref() else {
            immediate_bad.push((
                index,
                PlcWriteError::AddressProtocolMismatch {
                    expected: "modbus-ref".to_string(),
                    actual: req.address.notation().to_string(),
                },
            ));
            continue;
        };

        // T8-style bit-in-word notation (`"40001.3"`) is only meaningful
        // through a dedicated RMW path (FC22, not yet implemented - see this
        // module's doc comment), never as a whole-register write.
        if bit.is_some() {
            immediate_bad.push((
                index,
                PlcWriteError::UnsupportedCombination {
                    area: format!("{area} ({offset} 番地のビット位置指定)"),
                    data_type: req.data_type.to_string(),
                },
            ));
            continue;
        }

        if matches!(
            area,
            AddressArea::DiscreteInput | AddressArea::InputRegister
        ) {
            immediate_bad.push((
                index,
                PlcWriteError::ModbusReadOnlyArea {
                    area: area.to_string(),
                },
            ));
            continue;
        }

        if !is_compatible(area, req.data_type) {
            immediate_bad.push((
                index,
                PlcWriteError::UnsupportedCombination {
                    area: area.to_string(),
                    data_type: req.data_type.to_string(),
                },
            ));
            continue;
        }

        let item = match area {
            AddressArea::Coil => match encode_bit_value(req.value) {
                Ok(b) => ItemPayload::Bit(b),
                Err(e) => {
                    immediate_bad.push((index, e));
                    continue;
                }
            },
            AddressArea::HoldingRegister => {
                match encode_word_value(req.value, req.data_type, word_order) {
                    Ok(words) => ItemPayload::Words(words),
                    Err(e) => {
                        immediate_bad.push((index, e));
                        continue;
                    }
                }
            }
            // Filtered out above.
            AddressArea::DiscreteInput | AddressArea::InputRegister => unreachable!(
                "DiscreteInput/InputRegister already routed to ModbusReadOnlyArea above"
            ),
        };

        by_area.entry(area).or_default().push((index, offset, item));
    }

    let mut writes = Vec::new();
    for (area, mut items) in by_area {
        items.sort_by_key(|(_, offset, _)| *offset);
        match area {
            AddressArea::Coil => plan_group(
                area,
                items,
                MAX_WRITE_COILS as u64,
                &mut writes,
                |p| match p {
                    ItemPayload::Bit(b) => vec![b],
                    ItemPayload::Words(_) => unreachable!("word payload at Coil"),
                },
                WritePayload::Bits,
            ),
            AddressArea::HoldingRegister => plan_group(
                area,
                items,
                MAX_WRITE_REGISTERS as u64,
                &mut writes,
                |p| match p {
                    ItemPayload::Words(w) => w,
                    ItemPayload::Bit(_) => unreachable!("bit payload at HoldingRegister"),
                },
                WritePayload::Words,
            ),
            AddressArea::DiscreteInput | AddressArea::InputRegister => {
                unreachable!("read-only areas never enter by_area")
            }
        }
    }

    ModbusWritePlanOutcome {
        writes,
        immediate_bad,
    }
}

/// Group one area's (already offset-sorted) items into contiguous plans.
/// Merges only when the next target begins at or before the current group's
/// end (**no gap** - see this module's doc comment) and the resulting span
/// stays within `max_count`. Generic over the element type via `to_elems`
/// (unwraps an [`ItemPayload`] into its `Vec<T>`, one element per offset) and
/// `finish` (wraps the finished `Vec<T>` buffer into a [`WritePayload`]).
fn plan_group<T: Clone>(
    area: AddressArea,
    items: Vec<(usize, u16, ItemPayload)>,
    max_count: u64,
    out: &mut Vec<ModbusPlannedWrite>,
    to_elems: impl Fn(ItemPayload) -> Vec<T>,
    finish: impl Fn(Vec<T>) -> WritePayload,
) {
    let mut current: Option<Building<T>> = None;

    for (index, offset, payload) in items {
        let elems = to_elems(payload);
        let span = elems.len() as u64;
        let start = offset as u64;
        let end = start + span;

        // No gap: the target must begin at or before the current group's
        // end. Its span may extend the group, but only up to the cap.
        let fits_current = current.as_ref().is_some_and(|g| {
            let group_end = g.start + g.buffer.len() as u64;
            start <= group_end && end.max(group_end) - g.start <= max_count
        });

        if !fits_current {
            if let Some(g) = current.take() {
                out.push(ModbusPlannedWrite {
                    area,
                    start_offset: g.start as u16,
                    payload: finish(g.buffer),
                    request_indices: g.request_indices,
                });
            }
            current = Some(Building {
                start,
                buffer: Vec::new(),
                request_indices: Vec::new(),
            });
        }

        let group = current.as_mut().expect("just ensured Some above");
        let pos_offset = (start - group.start) as usize;
        // pos_offset <= buffer.len() by the no-gap rule, so this either
        // overwrites an overlapping element or extends contiguously - never
        // leaves a hole.
        for (i, e) in elems.into_iter().enumerate() {
            let pos = pos_offset + i;
            if pos < group.buffer.len() {
                group.buffer[pos] = e;
            } else {
                group.buffer.push(e);
            }
        }
        group.request_indices.push(index);
    }

    if let Some(g) = current.take() {
        out.push(ModbusPlannedWrite {
            area,
            start_offset: g.start as u16,
            payload: finish(g.buffer),
            request_indices: g.request_indices,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use banto_plc::{Address, TagValue};

    fn wreq(raw: &str, data_type: DataType, value: TagValue) -> WriteRequest {
        WriteRequest {
            address: Address::parse(raw).unwrap_or_else(|e| panic!("{raw} should parse: {e}")),
            data_type,
            value,
        }
    }

    fn word(raw: &str, data_type: DataType, v: f64) -> WriteRequest {
        wreq(raw, data_type, TagValue::F64(v))
    }

    fn coil(raw: &str, v: bool) -> WriteRequest {
        wreq(raw, DataType::Bit, TagValue::Bit(v))
    }

    const LH: WordOrder = WordOrder::LowHigh;

    #[test]
    fn single_register_request_becomes_a_single_group() {
        let outcome = plan_modbus_writes(&[word("40001", DataType::U16, 7.0)], LH);
        assert!(outcome.immediate_bad.is_empty());
        assert_eq!(outcome.writes.len(), 1);
        let g = &outcome.writes[0];
        assert_eq!(g.area, AddressArea::HoldingRegister);
        assert_eq!(g.start_offset, 0);
        assert_eq!(g.payload, WritePayload::Words(vec![7]));
        assert_eq!(g.request_indices, vec![0]);
    }

    #[test]
    fn adjacent_register_requests_merge_into_one_group() {
        let outcome = plan_modbus_writes(
            &[
                word("40001", DataType::U16, 1.0),
                word("40002", DataType::U16, 2.0),
                word("40003", DataType::U16, 3.0),
            ],
            LH,
        );
        assert_eq!(outcome.writes.len(), 1);
        assert_eq!(outcome.writes[0].start_offset, 0);
        assert_eq!(
            outcome.writes[0].payload,
            WritePayload::Words(vec![1, 2, 3])
        );
        assert_eq!(outcome.writes[0].request_indices, vec![0, 1, 2]);
    }

    /// The load-bearing safety property (this module's doc comment, and #131's
    /// implementation instructions' explicit requirement): unlike the read
    /// planner (which merges within a gap tolerance), a write must never
    /// coalesce across a gap, because that would write a register the caller
    /// never named.
    #[test]
    fn writes_never_merge_across_a_gap() {
        // 40001 and 40003 leave 40002 untouched - must be two separate
        // writes, not one three-register write that clobbers 40002.
        let outcome = plan_modbus_writes(
            &[
                word("40001", DataType::U16, 1.0),
                word("40003", DataType::U16, 3.0),
            ],
            LH,
        );
        assert_eq!(
            outcome.writes.len(),
            2,
            "a one-register gap must split the write so 40002 is never touched"
        );
        assert_eq!(outcome.writes[0].payload, WritePayload::Words(vec![1]));
        assert_eq!(outcome.writes[1].payload, WritePayload::Words(vec![3]));
    }

    #[test]
    fn a_32bit_value_spans_two_registers_and_an_adjacent_16bit_merges() {
        // 40001 = f32 (40001,40002), 40003 = i16 - exactly contiguous.
        let outcome = plan_modbus_writes(
            &[
                word("40001", DataType::F32, 1.5),
                word("40003", DataType::I16, 9.0),
            ],
            LH,
        );
        assert_eq!(outcome.writes.len(), 1);
        assert_eq!(
            outcome.writes[0].payload,
            WritePayload::Words(vec![0x0000, 0x3FC0, 0x0009])
        );
        assert_eq!(outcome.writes[0].request_indices, vec![0, 1]);
    }

    #[test]
    fn coil_and_holding_register_never_share_a_group() {
        let outcome = plan_modbus_writes(
            &[word("40001", DataType::U16, 1.0), coil("00001", true)],
            LH,
        );
        assert_eq!(outcome.writes.len(), 2);
        assert!(outcome.immediate_bad.is_empty());
    }

    #[test]
    fn out_of_order_input_is_sorted_before_grouping() {
        let outcome = plan_modbus_writes(
            &[
                word("40003", DataType::U16, 30.0),
                word("40001", DataType::U16, 10.0),
                word("40002", DataType::U16, 20.0),
            ],
            LH,
        );
        assert_eq!(outcome.writes.len(), 1);
        let g = &outcome.writes[0];
        assert_eq!(g.start_offset, 0);
        assert_eq!(g.payload, WritePayload::Words(vec![10, 20, 30]));
        assert_eq!(g.request_indices, vec![1, 2, 0]);
    }

    #[test]
    fn adjacent_coils_merge_and_gapped_coils_split() {
        let merged = plan_modbus_writes(
            &[
                coil("00001", true),
                coil("00002", false),
                coil("00003", true),
            ],
            LH,
        );
        assert_eq!(merged.writes.len(), 1);
        assert_eq!(
            merged.writes[0].payload,
            WritePayload::Bits(vec![true, false, true])
        );

        let split = plan_modbus_writes(&[coil("00001", true), coil("00003", true)], LH);
        assert_eq!(split.writes.len(), 2, "a coil gap must split too");
    }

    #[test]
    fn register_group_splits_at_the_write_cap() {
        let count = MAX_WRITE_REGISTERS as u32 + 10;
        let requests: Vec<WriteRequest> = (0..count)
            .map(|i| word(&format!("4{:04}", i + 1), DataType::U16, (i % 100) as f64))
            .collect();
        let outcome = plan_modbus_writes(&requests, LH);
        assert!(outcome.writes.len() >= 2);
        for g in &outcome.writes {
            assert!(g.payload.len() as u16 <= MAX_WRITE_REGISTERS);
        }
    }

    #[test]
    fn coil_group_splits_at_the_write_cap() {
        let count = MAX_WRITE_COILS as u32 + 10;
        let requests: Vec<WriteRequest> = (0..count)
            .map(|i| coil(&format!("0{:04}", i + 1), i % 2 == 0))
            .collect();
        let outcome = plan_modbus_writes(&requests, LH);
        assert!(outcome.writes.len() >= 2);
        for g in &outcome.writes {
            assert!(g.payload.len() as u16 <= MAX_WRITE_COILS);
        }
    }

    #[test]
    fn a_slmp_address_is_immediately_bad() {
        let requests = [
            WriteRequest {
                address: Address::parse_slmp("D0").unwrap(),
                data_type: DataType::U16,
                value: TagValue::F64(1.0),
            },
            word("40001", DataType::U16, 2.0),
        ];
        let outcome = plan_modbus_writes(&requests, LH);
        assert_eq!(outcome.immediate_bad.len(), 1);
        assert_eq!(outcome.immediate_bad[0].0, 0);
        match &outcome.immediate_bad[0].1 {
            PlcWriteError::AddressProtocolMismatch { expected, actual } => {
                assert_eq!(expected, "modbus-ref");
                assert_eq!(actual, "slmp");
            }
            other => panic!("expected AddressProtocolMismatch, got {other:?}"),
        }
        assert_eq!(outcome.writes.len(), 1);
        assert_eq!(outcome.writes[0].request_indices, vec![1]);
    }

    /// 2026-09-01 オーナー決定A: DiscreteInput/InputRegister are always
    /// rejected, regardless of data_type - the defense-in-depth twin of
    /// `banto-tags::tag`'s registration-time check.
    #[test]
    fn discrete_input_is_always_immediately_bad() {
        let outcome = plan_modbus_writes(&[wreq("10001", DataType::Bit, TagValue::Bit(true))], LH);
        assert!(outcome.writes.is_empty());
        assert_eq!(outcome.immediate_bad.len(), 1);
        match &outcome.immediate_bad[0].1 {
            PlcWriteError::ModbusReadOnlyArea { area } => assert_eq!(area, "discrete_input"),
            other => panic!("expected ModbusReadOnlyArea, got {other:?}"),
        }
    }

    #[test]
    fn input_register_is_always_immediately_bad() {
        let outcome = plan_modbus_writes(&[word("30001", DataType::U16, 1.0)], LH);
        assert!(outcome.writes.is_empty());
        assert_eq!(outcome.immediate_bad.len(), 1);
        match &outcome.immediate_bad[0].1 {
            PlcWriteError::ModbusReadOnlyArea { area } => assert_eq!(area, "input_register"),
            other => panic!("expected ModbusReadOnlyArea, got {other:?}"),
        }
    }

    #[test]
    fn a_bit_type_at_a_holding_register_is_immediately_bad() {
        let outcome = plan_modbus_writes(&[wreq("40001", DataType::Bit, TagValue::Bit(true))], LH);
        assert!(outcome.writes.is_empty());
        assert_eq!(outcome.immediate_bad.len(), 1);
        assert!(matches!(
            outcome.immediate_bad[0].1,
            PlcWriteError::UnsupportedCombination { .. }
        ));
    }

    #[test]
    fn a_numeric_type_at_a_coil_is_immediately_bad() {
        let outcome = plan_modbus_writes(&[word("00001", DataType::I16, 1.0)], LH);
        assert!(outcome.writes.is_empty());
        assert_eq!(outcome.immediate_bad.len(), 1);
        assert!(matches!(
            outcome.immediate_bad[0].1,
            PlcWriteError::UnsupportedCombination { .. }
        ));
    }

    #[test]
    fn an_unencodable_value_is_immediately_bad_and_never_grouped() {
        let outcome = plan_modbus_writes(
            &[
                word("40001", DataType::U16, 70000.0), // out of range
                word("40002", DataType::U16, 5.0),     // fine
            ],
            LH,
        );
        assert_eq!(outcome.immediate_bad.len(), 1);
        assert_eq!(outcome.immediate_bad[0].0, 0);
        assert!(matches!(
            outcome.immediate_bad[0].1,
            PlcWriteError::ValueOutOfRange { .. }
        ));
        assert_eq!(outcome.writes.len(), 1);
        assert_eq!(outcome.writes[0].start_offset, 1);
        assert_eq!(outcome.writes[0].request_indices, vec![1]);
    }

    /// A bit-in-word-qualified address is not supported for a plain write
    /// this slice (see this module's doc comment on FC22/RMW being out of
    /// scope) - rejected rather than silently treated as a whole-register
    /// write.
    #[test]
    fn a_bit_qualified_address_is_immediately_bad() {
        let outcome = plan_modbus_writes(&[word("40001.5", DataType::U16, 1.0)], LH);
        assert!(outcome.writes.is_empty());
        assert_eq!(outcome.immediate_bad.len(), 1);
        assert!(matches!(
            outcome.immediate_bad[0].1,
            PlcWriteError::UnsupportedCombination { .. }
        ));
    }

    #[test]
    fn empty_input_produces_nothing() {
        let outcome = plan_modbus_writes(&[], LH);
        assert!(outcome.writes.is_empty());
        assert!(outcome.immediate_bad.is_empty());
    }

    /// The contract the result-assembly loop relies on: every input index is
    /// accounted for exactly once - in some group or in `immediate_bad`.
    #[test]
    fn every_input_index_is_accounted_for_exactly_once() {
        let requests = [
            word("40001", DataType::U16, 1.0),
            wreq("40001", DataType::Bit, TagValue::Bit(true)), // bad: bit at holding register
            coil("00001", true),
            word("40101", DataType::I32, -3.0),
            WriteRequest {
                address: Address::parse_slmp("D0").unwrap(), // bad: slmp
                data_type: DataType::U16,
                value: TagValue::F64(1.0),
            },
            word("40201", DataType::U16, 70000.0), // bad: out of range
            coil("00050", false),
            wreq("10001", DataType::Bit, TagValue::Bit(true)), // bad: read-only area
        ];
        let outcome = plan_modbus_writes(&requests, LH);

        let mut seen: Vec<usize> = outcome
            .writes
            .iter()
            .flat_map(|g| g.request_indices.iter().copied())
            .chain(outcome.immediate_bad.iter().map(|(i, _)| *i))
            .collect();
        seen.sort();
        assert_eq!(seen, (0..requests.len()).collect::<Vec<_>>());
    }
}
