//! SLMP *write* request planning (I5): the mirror image of
//! `banto-plc/src/slmp/planning.rs` for writes. Same job - turn a flat list of
//! targets into the minimal set of wire operations - and the same purity (no
//! I/O, and, unlike the read planner, no `slmp`-crate types either).
//!
//! ## The one rule that differs from the read planner, and why it matters
//!
//! The read planner merges two requests within `MAX_GAP` elements of each other
//! into one bulk read, deliberately reading (and discarding) the few registers
//! in the gap to save a round trip. **A write must not do that.** Writing a
//! device the caller did not ask to write is a real, possibly destructive side
//! effect on a live PLC - the exact opposite of a harmless over-read. So this
//! planner merges only *exactly adjacent* runs (gap tolerance zero): two
//! targets share one `bulk_write` only if the second begins where the first
//! ends, so every register the group writes corresponds to a request the caller
//! actually made. `writes_never_merge_across_a_gap` is the tripwire for this.
//!
//! ## Values are encoded here, before grouping
//!
//! A target whose value will not fit its register ([`crate::encode`]) is
//! resolved into [`SlmpWritePlanOutcome::immediate_bad`] and never grouped -
//! exactly like an address-protocol mismatch. That keeps the group payloads
//! ready-to-serialize and means the wire path only ever handles values already
//! proven encodable, so a bad value costs one target its write, never its
//! batch-mates theirs (and never reaches the CPU as a truncated number).

use std::collections::BTreeMap;

use banto_plc::{DataType, SlmpAccess, SlmpDevice, WordOrder};

use crate::encode::{encode_bit_value, encode_word_value};
use crate::error::PlcWriteError;
use crate::types::WriteRequest;

/// SLMP bulk-write word-unit cap, from the MELSEC MC protocol reference (bulk
/// write allows up to 960 words per request). Unlike the read planner's cap -
/// which is bound by the wrapped crate's single-`read` receive buffer, since a
/// read reply carries data - a write's response carries only an end code, so
/// the binding limit here is the request/spec side, not the receive path.
/// Tuning against real hardware is W5's 実機検証, same as the read cap.
const MAX_WRITE_WORDS: u32 = 960;

/// SLMP bulk-write bit-unit cap, from the same reference (3584 points).
const MAX_WRITE_BITS: u32 = 3584;

/// v1 device/data-type restriction, identical to the read planner's: a `bit`
/// type writes a bit device, every other type writes a word device.
fn is_compatible(access: SlmpAccess, data_type: DataType) -> bool {
    match access {
        SlmpAccess::Bit => data_type == DataType::Bit,
        SlmpAccess::Word => data_type != DataType::Bit,
    }
}

/// The ready-to-serialize payload of one planned write: a contiguous word
/// window or a contiguous run of bits. Which one is implied by `device`'s
/// [`SlmpDevice::access`], exactly as on the read side.
#[derive(Debug, Clone, PartialEq)]
pub enum WritePayload {
    /// Word-unit write: the register window, already ordered per the configured
    /// [`WordOrder`] by [`crate::encode`].
    Words(Vec<u16>),
    /// Bit-unit write: one entry per consecutive bit device.
    Bits(Vec<bool>),
}

impl WritePayload {
    /// Element count (`count` field the wrapped crate needs): words for a word
    /// write, points for a bit write.
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

/// One physical SLMP bulk write: `slmp/mod.rs` issues exactly one
/// `SLMPClient::bulk_write` per `SlmpPlannedWrite`. `request_indices` lists
/// every input request this group fulfils (in ascending device order), so the
/// result-assembly loop can mark each one [`crate::types::WriteResult::Ok`] on
/// success or `Bad` on a per-request end code. No per-request offset is needed
/// (unlike the read side): a group write succeeds or fails as a whole, there is
/// nothing to scatter back.
#[derive(Debug, Clone, PartialEq)]
pub struct SlmpPlannedWrite {
    pub device: SlmpDevice,
    pub start: u32,
    pub payload: WritePayload,
    pub request_indices: Vec<usize>,
}

/// [`plan_slmp_writes`]'s full result. Same contract as the read planner's
/// outcome: every input index appears exactly once, either inside some group's
/// `request_indices` or in `immediate_bad`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SlmpWritePlanOutcome {
    pub writes: Vec<SlmpPlannedWrite>,
    pub immediate_bad: Vec<(usize, PlcWriteError)>,
}

/// One encoded target awaiting grouping.
enum ItemPayload {
    Words(Vec<u16>),
    Bit(bool),
}

/// A word group under construction. `u64` bounds so a 32-bit target at the very
/// top of the device space cannot overflow while `end` is computed, matching
/// the read planner's `Building`.
struct BuildingWords {
    device: SlmpDevice,
    start: u64,
    buffer: Vec<u16>,
    request_indices: Vec<usize>,
}

/// A bit group under construction.
struct BuildingBits {
    device: SlmpDevice,
    start: u64,
    buffer: Vec<bool>,
    request_indices: Vec<usize>,
}

/// Plan wire-level bulk writes for `requests`, encoding each value per
/// `word_order` (which must match the read side's configured order for a
/// write/read round trip to agree - see [`crate::encode`]).
///
/// Requests that cannot reach the wire are resolved into
/// [`SlmpWritePlanOutcome::immediate_bad`] rather than attempted and failed:
///
/// - a non-SLMP address ([`PlcWriteError::AddressProtocolMismatch`])
/// - a `bit` type at a word device or vice versa
///   ([`PlcWriteError::UnsupportedCombination`])
/// - a value that will not encode into its register
///   ([`PlcWriteError::ValueOutOfRange`] / [`PlcWriteError::ValueTypeMismatch`])
///
/// A target whose *device number* does not exist on the CPU cannot be caught
/// here; that returns a [`PlcWriteError::SlmpEndCode`] `Bad` at write time.
pub fn plan_slmp_writes(requests: &[WriteRequest], word_order: WordOrder) -> SlmpWritePlanOutcome {
    let mut immediate_bad = Vec::new();

    // BTreeMap for deterministic device (and therefore `writes`) ordering, same
    // reason as the read planner.
    let mut by_device: BTreeMap<SlmpDevice, Vec<(usize, u32, ItemPayload)>> = BTreeMap::new();

    for (index, req) in requests.iter().enumerate() {
        let Some((device, number)) = req.address.as_slmp() else {
            immediate_bad.push((
                index,
                PlcWriteError::AddressProtocolMismatch {
                    expected: "slmp".to_string(),
                    actual: req.address.notation().to_string(),
                },
            ));
            continue;
        };

        if !is_compatible(device.access(), req.data_type) {
            immediate_bad.push((
                index,
                PlcWriteError::UnsupportedCombination {
                    area: format!("{device} ({})", device.access()),
                    data_type: req.data_type.to_string(),
                },
            ));
            continue;
        }

        let item = match device.access() {
            SlmpAccess::Bit => match encode_bit_value(req.value) {
                Ok(b) => ItemPayload::Bit(b),
                Err(e) => {
                    immediate_bad.push((index, e));
                    continue;
                }
            },
            SlmpAccess::Word => match encode_word_value(req.value, req.data_type, word_order) {
                Ok(words) => ItemPayload::Words(words),
                Err(e) => {
                    immediate_bad.push((index, e));
                    continue;
                }
            },
        };

        by_device
            .entry(device)
            .or_default()
            .push((index, number, item));
    }

    let mut writes = Vec::new();
    for (device, mut items) in by_device {
        items.sort_by_key(|(_, number, _)| *number);
        match device.access() {
            SlmpAccess::Word => plan_word_device(device, items, &mut writes),
            SlmpAccess::Bit => plan_bit_device(device, items, &mut writes),
        }
    }

    SlmpWritePlanOutcome {
        writes,
        immediate_bad,
    }
}

/// Group one word device's (already number-sorted) items into contiguous
/// `bulk_write`s. Merges only when the next target begins at or before the
/// current group's end (no gap) and the resulting span stays within
/// [`MAX_WRITE_WORDS`].
fn plan_word_device(
    device: SlmpDevice,
    items: Vec<(usize, u32, ItemPayload)>,
    out: &mut Vec<SlmpPlannedWrite>,
) {
    let max_count = MAX_WRITE_WORDS as u64;
    let mut current: Option<BuildingWords> = None;

    for (index, number, payload) in items {
        let words = match payload {
            ItemPayload::Words(w) => w,
            ItemPayload::Bit(_) => unreachable!("bit payload at a word device"),
        };
        let span = words.len() as u64;
        let start = number as u64;
        let end = start + span;

        // No gap: the target must begin at or before the current group's end.
        // Its span may extend the group, but only up to the cap.
        let fits_current = current.as_ref().is_some_and(|g| {
            let group_end = g.start + g.buffer.len() as u64;
            start <= group_end && end.max(group_end) - g.start <= max_count
        });

        if !fits_current {
            if let Some(g) = current.take() {
                out.push(g.finish());
            }
            current = Some(BuildingWords {
                device,
                start,
                buffer: Vec::new(),
                request_indices: Vec::new(),
            });
        }

        let group = current.as_mut().expect("just ensured Some above");
        let offset = (start - group.start) as usize;
        // offset <= buffer.len() by the no-gap rule, so this either overwrites
        // an overlapping word or extends contiguously - never leaves a hole.
        for (i, w) in words.into_iter().enumerate() {
            let pos = offset + i;
            if pos < group.buffer.len() {
                group.buffer[pos] = w;
            } else {
                group.buffer.push(w);
            }
        }
        group.request_indices.push(index);
    }

    if let Some(g) = current.take() {
        out.push(g.finish());
    }
}

/// Group one bit device's (already number-sorted) items. Every bit spans one
/// element, so grouping is the same no-gap rule against [`MAX_WRITE_BITS`].
fn plan_bit_device(
    device: SlmpDevice,
    items: Vec<(usize, u32, ItemPayload)>,
    out: &mut Vec<SlmpPlannedWrite>,
) {
    let max_count = MAX_WRITE_BITS as u64;
    let mut current: Option<BuildingBits> = None;

    for (index, number, payload) in items {
        let bit = match payload {
            ItemPayload::Bit(b) => b,
            ItemPayload::Words(_) => unreachable!("word payload at a bit device"),
        };
        let start = number as u64;
        let end = start + 1;

        let fits_current = current.as_ref().is_some_and(|g| {
            let group_end = g.start + g.buffer.len() as u64;
            start <= group_end && end.max(group_end) - g.start <= max_count
        });

        if !fits_current {
            if let Some(g) = current.take() {
                out.push(g.finish());
            }
            current = Some(BuildingBits {
                device,
                start,
                buffer: Vec::new(),
                request_indices: Vec::new(),
            });
        }

        let group = current.as_mut().expect("just ensured Some above");
        let offset = (start - group.start) as usize;
        if offset < group.buffer.len() {
            group.buffer[offset] = bit;
        } else {
            group.buffer.push(bit);
        }
        group.request_indices.push(index);
    }

    if let Some(g) = current.take() {
        out.push(g.finish());
    }
}

impl BuildingWords {
    fn finish(self) -> SlmpPlannedWrite {
        SlmpPlannedWrite {
            device: self.device,
            start: self.start as u32,
            payload: WritePayload::Words(self.buffer),
            request_indices: self.request_indices,
        }
    }
}

impl BuildingBits {
    fn finish(self) -> SlmpPlannedWrite {
        SlmpPlannedWrite {
            device: self.device,
            start: self.start as u32,
            payload: WritePayload::Bits(self.buffer),
            request_indices: self.request_indices,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use banto_plc::{Address, TagValue};

    fn wreq(raw: &str, data_type: DataType, value: TagValue) -> WriteRequest {
        WriteRequest {
            address: Address::parse_slmp(raw).unwrap_or_else(|e| panic!("{raw} should parse: {e}")),
            data_type,
            value,
        }
    }

    fn word(raw: &str, data_type: DataType, v: f64) -> WriteRequest {
        wreq(raw, data_type, TagValue::F64(v))
    }

    fn bit(raw: &str, v: bool) -> WriteRequest {
        wreq(raw, DataType::Bit, TagValue::Bit(v))
    }

    const LH: WordOrder = WordOrder::LowHigh;

    #[test]
    fn single_word_request_becomes_a_single_group() {
        let outcome = plan_slmp_writes(&[word("D100", DataType::U16, 7.0)], LH);
        assert!(outcome.immediate_bad.is_empty());
        assert_eq!(outcome.writes.len(), 1);
        let g = &outcome.writes[0];
        assert_eq!(g.device, SlmpDevice::D);
        assert_eq!(g.start, 100);
        assert_eq!(g.payload, WritePayload::Words(vec![7]));
        assert_eq!(g.request_indices, vec![0]);
    }

    #[test]
    fn adjacent_word_requests_merge_into_one_group() {
        let outcome = plan_slmp_writes(
            &[
                word("D0", DataType::U16, 1.0),
                word("D1", DataType::U16, 2.0),
                word("D2", DataType::U16, 3.0),
            ],
            LH,
        );
        assert_eq!(outcome.writes.len(), 1);
        assert_eq!(outcome.writes[0].start, 0);
        assert_eq!(
            outcome.writes[0].payload,
            WritePayload::Words(vec![1, 2, 3])
        );
        assert_eq!(outcome.writes[0].request_indices, vec![0, 1, 2]);
    }

    /// The load-bearing safety property: unlike the read planner (which merges
    /// within MAX_GAP), a write must never coalesce across a gap, because that
    /// would write a device the caller never named.
    #[test]
    fn writes_never_merge_across_a_gap() {
        // D0 and D2 leave D1 untouched - they must be two separate writes, not
        // one three-word write that clobbers D1.
        let outcome = plan_slmp_writes(
            &[
                word("D0", DataType::U16, 1.0),
                word("D2", DataType::U16, 3.0),
            ],
            LH,
        );
        assert_eq!(
            outcome.writes.len(),
            2,
            "a one-device gap must split the write so D1 is never touched"
        );
        assert_eq!(outcome.writes[0].payload, WritePayload::Words(vec![1]));
        assert_eq!(outcome.writes[1].payload, WritePayload::Words(vec![3]));
    }

    #[test]
    fn a_32bit_value_spans_two_words_and_an_adjacent_16bit_merges() {
        // D0 = f32 (D0,D1), D2 = i16 - exactly contiguous, so one group of 3.
        let outcome = plan_slmp_writes(
            &[
                word("D0", DataType::F32, 1.5),
                word("D2", DataType::I16, 9.0),
            ],
            LH,
        );
        assert_eq!(outcome.writes.len(), 1);
        // f32 1.5 LowHigh = [0x0000, 0x3FC0], then i16 9 = [0x0009].
        assert_eq!(
            outcome.writes[0].payload,
            WritePayload::Words(vec![0x0000, 0x3FC0, 0x0009])
        );
        assert_eq!(outcome.writes[0].request_indices, vec![0, 1]);
    }

    #[test]
    fn different_devices_never_share_a_group() {
        let outcome = plan_slmp_writes(
            &[
                word("D0", DataType::U16, 1.0),
                word("R0", DataType::U16, 2.0),
                word("W0", DataType::U16, 3.0),
                bit("M0", true),
                bit("X0", false),
            ],
            LH,
        );
        assert_eq!(outcome.writes.len(), 5);
        assert!(outcome.immediate_bad.is_empty());
    }

    #[test]
    fn out_of_order_input_is_sorted_before_grouping() {
        let outcome = plan_slmp_writes(
            &[
                word("D2", DataType::U16, 30.0),
                word("D0", DataType::U16, 10.0),
                word("D1", DataType::U16, 20.0),
            ],
            LH,
        );
        assert_eq!(outcome.writes.len(), 1);
        let g = &outcome.writes[0];
        assert_eq!(g.start, 0);
        assert_eq!(g.payload, WritePayload::Words(vec![10, 20, 30]));
        // Indices come out in ascending-device order.
        assert_eq!(g.request_indices, vec![1, 2, 0]);
    }

    #[test]
    fn adjacent_bits_merge_and_gapped_bits_split() {
        let merged = plan_slmp_writes(&[bit("M0", true), bit("M1", false), bit("M2", true)], LH);
        assert_eq!(merged.writes.len(), 1);
        assert_eq!(
            merged.writes[0].payload,
            WritePayload::Bits(vec![true, false, true])
        );

        let split = plan_slmp_writes(&[bit("M0", true), bit("M2", true)], LH);
        assert_eq!(split.writes.len(), 2, "a bit gap must split too");
    }

    #[test]
    fn hex_notation_bits_group_on_their_numeric_value() {
        // X0, X1, ... X4 are exactly contiguous (hex numbering); a run of 5.
        let outcome = plan_slmp_writes(
            &[
                bit("X0", true),
                bit("X1", true),
                bit("X2", true),
                bit("X3", true),
                bit("X4", true),
            ],
            LH,
        );
        assert_eq!(outcome.writes.len(), 1);
        assert_eq!(outcome.writes[0].payload.len(), 5);
    }

    #[test]
    fn word_group_splits_at_the_word_cap() {
        let count = MAX_WRITE_WORDS + 10;
        let requests: Vec<WriteRequest> = (0..count)
            .map(|i| word(&format!("D{i}"), DataType::U16, i as f64 % 100.0))
            .collect();
        let outcome = plan_slmp_writes(&requests, LH);
        assert!(outcome.writes.len() >= 2);
        for g in &outcome.writes {
            assert!(g.payload.len() as u32 <= MAX_WRITE_WORDS);
        }
    }

    #[test]
    fn bit_group_splits_at_the_bit_cap() {
        let count = MAX_WRITE_BITS + 10;
        let requests: Vec<WriteRequest> = (0..count)
            .map(|i| bit(&format!("M{i}"), i % 2 == 0))
            .collect();
        let outcome = plan_slmp_writes(&requests, LH);
        assert!(outcome.writes.len() >= 2);
        for g in &outcome.writes {
            assert!(g.payload.len() as u32 <= MAX_WRITE_BITS);
        }
    }

    #[test]
    fn a_modbus_address_is_immediately_bad() {
        let requests = [
            WriteRequest {
                address: Address::parse("40001").unwrap(),
                data_type: DataType::U16,
                value: TagValue::F64(1.0),
            },
            word("D0", DataType::U16, 2.0),
        ];
        let outcome = plan_slmp_writes(&requests, LH);
        assert_eq!(outcome.immediate_bad.len(), 1);
        assert_eq!(outcome.immediate_bad[0].0, 0);
        match &outcome.immediate_bad[0].1 {
            PlcWriteError::AddressProtocolMismatch { expected, actual } => {
                assert_eq!(expected, "slmp");
                assert_eq!(actual, "modbus-ref");
            }
            other => panic!("expected AddressProtocolMismatch, got {other:?}"),
        }
        assert_eq!(outcome.writes.len(), 1);
        assert_eq!(outcome.writes[0].request_indices, vec![1]);
    }

    #[test]
    fn a_bit_type_at_a_word_device_is_immediately_bad() {
        let outcome = plan_slmp_writes(&[wreq("D0", DataType::Bit, TagValue::Bit(true))], LH);
        assert!(outcome.writes.is_empty());
        assert_eq!(outcome.immediate_bad.len(), 1);
        assert!(matches!(
            outcome.immediate_bad[0].1,
            PlcWriteError::UnsupportedCombination { .. }
        ));
    }

    #[test]
    fn a_numeric_type_at_a_bit_device_is_immediately_bad() {
        for raw in ["M0", "X0", "TS0"] {
            let outcome = plan_slmp_writes(&[word(raw, DataType::I16, 1.0)], LH);
            assert!(outcome.writes.is_empty(), "{raw} should not reach the wire");
            assert_eq!(outcome.immediate_bad.len(), 1);
        }
    }

    #[test]
    fn an_unencodable_value_is_immediately_bad_and_never_grouped() {
        let outcome = plan_slmp_writes(
            &[
                word("D0", DataType::U16, 70000.0), // out of range
                word("D1", DataType::U16, 5.0),     // fine
            ],
            LH,
        );
        assert_eq!(outcome.immediate_bad.len(), 1);
        assert_eq!(outcome.immediate_bad[0].0, 0);
        assert!(matches!(
            outcome.immediate_bad[0].1,
            PlcWriteError::ValueOutOfRange { .. }
        ));
        // The good one still gets planned, and on its own (the bad D0 did not
        // even reserve a slot to merge with).
        assert_eq!(outcome.writes.len(), 1);
        assert_eq!(outcome.writes[0].start, 1);
        assert_eq!(outcome.writes[0].request_indices, vec![1]);
    }

    #[test]
    fn empty_input_produces_nothing() {
        let outcome = plan_slmp_writes(&[], LH);
        assert!(outcome.writes.is_empty());
        assert!(outcome.immediate_bad.is_empty());
    }

    /// The contract the result-assembly loop relies on: every input index is
    /// accounted for exactly once - in some group or in `immediate_bad`.
    #[test]
    fn every_input_index_is_accounted_for_exactly_once() {
        let requests = [
            word("D0", DataType::U16, 1.0),
            wreq("D0", DataType::Bit, TagValue::Bit(true)), // bad: bit at word device
            bit("M0", true),
            word("D100", DataType::I32, -3.0),
            WriteRequest {
                address: Address::parse("40001").unwrap(), // bad: modbus
                data_type: DataType::U16,
                value: TagValue::F64(1.0),
            },
            word("D200", DataType::U16, 70000.0), // bad: out of range
            bit("X1F", false),
        ];
        let outcome = plan_slmp_writes(&requests, LH);

        let mut seen: Vec<usize> = outcome
            .writes
            .iter()
            .flat_map(|g| g.request_indices.iter().copied())
            .chain(outcome.immediate_bad.iter().map(|(i, _)| *i))
            .collect();
        seen.sort();
        assert_eq!(seen, (0..requests.len()).collect::<Vec<_>>());
    }

    /// A 32-bit target at the very top of the device space must not overflow
    /// while its end is computed (the `u64` bounds in `BuildingWords`).
    #[test]
    fn a_32bit_target_at_the_top_of_the_device_space_does_not_overflow() {
        let raw = format!("D{}", banto_plc::slmp::address::MAX_DEVICE_NUMBER);
        let outcome = plan_slmp_writes(&[word(&raw, DataType::U32, 1.0)], LH);
        assert_eq!(outcome.writes.len(), 1);
        assert_eq!(
            outcome.writes[0].start,
            banto_plc::slmp::address::MAX_DEVICE_NUMBER
        );
        assert_eq!(outcome.writes[0].payload.len(), 2);
    }
}
