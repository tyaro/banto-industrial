//! SLMP request planning (I2a): the mirror image of [`crate::planning`] for
//! MELSEC device addressing. Same job, same reason it exists - one wire round
//! trip per contiguous *group* of tags rather than one per tag, which is what
//! makes 256-tag/100ms collection cycles (docs/recorder-requirements.md §3.1)
//! feasible - and the same purity: no I/O, no `slmp`-crate types.
//!
//! ## Why this is a separate function rather than a generic one
//!
//! [`crate::planning::plan_requests`] groups by [`crate::address::AddressArea`]
//! with `u16` offsets and Modbus function-code quantity caps; this groups by
//! [`SlmpDevice`] with `u32` numbers and SLMP's own caps. Every one of those
//! four things differs, so a shared generic implementation would be a trait
//! with four associated items and no second caller - the duplication here is
//! the cheaper of the two, and it keeps each protocol's grouping rule readable
//! next to that protocol's limits.
//!
//! ## What the wrapped crate does and does not do for us
//!
//! The `slmp` crate owns *framing* (subheader, per-CPU 3-vs-4-byte device
//! serialization, bit-unit nibble packing) and this module deliberately does
//! not duplicate any of it. What the crate does **not** do is bound a request:
//! `SLMPClient::bulk_read` will happily encode any point count into its
//! 16-bit size field and then fail to parse the oversized reply. So the caps
//! below have to live here. See [`MAX_RESPONSE_DATA_BYTES`] for where the
//! binding limit actually comes from - it is the crate's receive path, not the
//! SLMP specification.

use std::collections::BTreeMap;

use super::address::{SlmpAccess, SlmpDevice};
use crate::error::PlcError;
use crate::types::{BatchReadRequest, DataType, ReadRequest};

/// Gap tolerance, in the device's own element unit (words for word devices,
/// bits for bit devices). Same rule and same reasoning as
/// [`crate::planning`]'s `MAX_GAP`: two requests within this many elements of
/// each other are merged into one read, trading a handful of
/// read-but-discarded elements for one fewer round trip, which is the right
/// trade whenever an RTT costs more than a few bytes. Kept as its own constant
/// rather than shared with the Modbus planner because the two are free to
/// diverge once there are real MELSEC tag layouts to measure against - MELSEC
/// tag lists tend to be *denser* than Modbus ones (consecutive `D` registers
/// are the norm), so this may well want to grow.
const MAX_GAP: u32 = 5;

/// The response-payload budget every cap below is derived from.
///
/// This is **not** the SLMP specification's limit (bulk read allows 960 words
/// / 3584 points per request). It is the wrapped `slmp` crate's receive path:
/// it reads a response with a single `TcpStream::read` into a fixed 2048-byte
/// buffer, with no loop to gather a reply split across TCP segments, and then
/// rejects the frame if its declared data length does not match what arrived
/// in that one read. So a reply larger than one path-MTU segment is not merely
/// inefficient here, it is a *framing error* - and, per `super`'s module doc, a
/// framing error is connection-fatal. 960 bytes of payload plus the 15-byte
/// response prefix stays comfortably inside a standard 1460-byte Ethernet MSS
/// with room for tunnelling overhead.
///
/// Raising this is exactly the kind of change that needs real-hardware
/// evidence (docs/plan.md W5's 実機検証), not a spec table.
const MAX_RESPONSE_DATA_BYTES: u32 = 960;

/// Word-unit bulk read cap: two response bytes per point.
const MAX_WORDS_PER_READ: u32 = MAX_RESPONSE_DATA_BYTES / 2;

/// Bit-unit bulk read cap: SLMP packs two points per response byte (one per
/// nibble), so the same payload budget buys four times as many points.
const MAX_BITS_PER_READ: u32 = MAX_RESPONSE_DATA_BYTES * 2;

fn max_count_for(access: SlmpAccess) -> u32 {
    match access {
        SlmpAccess::Bit => MAX_BITS_PER_READ,
        SlmpAccess::Word => MAX_WORDS_PER_READ,
    }
}

/// v1 restriction, deliberately identical in shape to the Modbus planner's
/// (docs/plan.md I2 §4): a `bit` tag addresses a bit device, every other data
/// type addresses a word device. "One bit out of a `D` register" (`D100.5`) is
/// a real MELSEC form and a real eventual need, but it needs a read strategy
/// here and a bit-offset field on [`crate::address::Address`] before the
/// parser should start accepting it - see [`super::address::parse`]'s doc
/// comment.
fn is_compatible(access: SlmpAccess, data_type: DataType) -> bool {
    match access {
        SlmpAccess::Bit => data_type == DataType::Bit,
        SlmpAccess::Word => data_type != DataType::Bit,
    }
}

/// How to interpret one mapped span of a group's response window: a numeric
/// value of the given [`DataType`], or a Shift-JIS string occupying `words`
/// consecutive words (S1 文字列タグ). This is what lets one bulk read serve a
/// mix of numeric and string tags - the span logic is shared, only the
/// decode-scatter step branches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadKind {
    Numeric(DataType),
    Str { words: u16 },
}

/// Where one original request lands within a [`SlmpPlannedRead`]'s response
/// window. The SLMP twin of [`crate::planning::MappedRequest`], differing in
/// `offset_in_read`'s width (`u32`, to match MELSEC's address space) and in
/// carrying a [`ReadKind`] rather than a bare [`DataType`] so string spans can
/// be expressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlmpMappedRequest {
    pub request_index: usize,
    /// Offset from the group's `start`, in the same element unit as `count`
    /// (words for word devices, bits for bit devices). Indexes directly into
    /// the decoded word/bit window returned for this group.
    pub offset_in_read: u32,
    pub kind: ReadKind,
}

/// One physical SLMP bulk read: `slmp/mod.rs` issues exactly one
/// `SLMPClient::bulk_read` per `SlmpPlannedRead` (bit-unit or word-unit,
/// implied by `device`'s [`SlmpDevice::access`]), then scatters the response
/// back out to every request in `mapping`.
#[derive(Debug, Clone, PartialEq)]
pub struct SlmpPlannedRead {
    pub device: SlmpDevice,
    pub start: u32,
    pub count: u32,
    pub mapping: Vec<SlmpMappedRequest>,
}

/// [`plan_slmp_requests`]'s full result. Same shape and same contract as
/// [`crate::planning::PlanOutcome`]: every input index appears exactly once,
/// either inside some group's `mapping` or in `immediate_bad`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SlmpPlanOutcome {
    pub reads: Vec<SlmpPlannedRead>,
    pub immediate_bad: Vec<(usize, PlcError)>,
}

/// A group under construction. `u64` bounds so a 32-bit tag at the very top of
/// the device space (`number = MAX_DEVICE_NUMBER`, `end = MAX_DEVICE_NUMBER +
/// 2`) cannot overflow mid-computation; the finished [`SlmpPlannedRead`]
/// narrows back to `u32`, which is always in range because `end - start` never
/// exceeds `max_count` by construction.
struct Building {
    device: SlmpDevice,
    start: u64,
    end: u64,
    mapping: Vec<SlmpMappedRequest>,
}

impl Building {
    fn finish(self) -> SlmpPlannedRead {
        SlmpPlannedRead {
            device: self.device,
            start: self.start as u32,
            count: (self.end - self.start) as u32,
            mapping: self.mapping,
        }
    }
}

/// Plan wire-level bulk reads for a numeric-only batch - the original I2a
/// entry point, kept with its exact signature (the W3 broker and
/// `SlmpClient::read_batch` call it). Since S1 it is a thin wrapper over
/// [`plan_slmp_batch`]: a [`ReadRequest`] is just the `Numeric` case of a
/// [`BatchReadRequest`], so the two planners cannot drift.
pub fn plan_slmp_requests(requests: &[ReadRequest]) -> SlmpPlanOutcome {
    let batch: Vec<BatchReadRequest> = requests
        .iter()
        .map(|&r| BatchReadRequest::Numeric(r))
        .collect();
    plan_slmp_batch(&batch)
}

/// Plan wire-level bulk reads for a mixed numeric + string batch (S1
/// 文字列タグ) - one read can serve both, which is what lets the S2 broker
/// batch a rule's numeric sources and string sources in a single round trip.
///
/// Requests that cannot reach the wire at all are resolved into
/// [`SlmpPlanOutcome::immediate_bad`] rather than attempted and failed:
///
/// - a non-[`crate::address::Address::Slmp`] address
///   ([`PlcError::AddressProtocolMismatch`]) - the tag's notation and its
///   connection's `protocol` disagree
/// - a `bit` tag at a word device or vice versa, or a *string* tag at a bit
///   device ([`PlcError::UnsupportedCombination`]) - see [`is_compatible`]
/// - a string span of zero words or more than one bulk read can carry
///   ([`PlcError::StringSpanUnsupported`]) - a per-request `Bad`, never a
///   panic, even though registry-validated tags (1..=128 words) can never
///   trigger it
///
/// A tag whose *device number* does not exist on the CPU cannot be caught
/// here (only the CPU knows its own catalogue); that one comes back as a
/// [`PlcError::SlmpEndCode`] `Bad` for its group at read time.
pub fn plan_slmp_batch(requests: &[BatchReadRequest]) -> SlmpPlanOutcome {
    let mut immediate_bad = Vec::new();

    // BTreeMap (not HashMap) so devices - and therefore the resulting `reads`
    // order - come out deterministic, keeping tests and any future
    // request-count metrics stable across runs. Same choice, same reason, as
    // the Modbus planner.
    let mut by_device: BTreeMap<SlmpDevice, Vec<(usize, u32, ReadKind)>> = BTreeMap::new();
    for (index, req) in requests.iter().enumerate() {
        let (address, kind) = match req {
            BatchReadRequest::Numeric(r) => (r.address, ReadKind::Numeric(r.data_type)),
            BatchReadRequest::String(s) => (s.address, ReadKind::Str { words: s.words }),
        };
        let Some((device, number)) = address.as_slmp() else {
            immediate_bad.push((
                index,
                PlcError::AddressProtocolMismatch {
                    expected: "slmp".to_string(),
                    actual: address.notation().to_string(),
                },
            ));
            continue;
        };

        match kind {
            ReadKind::Numeric(data_type) => {
                if !is_compatible(device.access(), data_type) {
                    immediate_bad.push((
                        index,
                        PlcError::UnsupportedCombination {
                            area: format!("{device} ({})", device.access()),
                            data_type: data_type.to_string(),
                        },
                    ));
                    continue;
                }
            }
            ReadKind::Str { words } => {
                // Strings live in word devices only - same v1 rule as every
                // non-bit numeric type.
                if device.access() != SlmpAccess::Word {
                    immediate_bad.push((
                        index,
                        PlcError::UnsupportedCombination {
                            area: format!("{device} ({})", device.access()),
                            data_type: "string".to_string(),
                        },
                    ));
                    continue;
                }
                // A span the wire cannot serve in one bulk read is a
                // per-request Bad - the group-building loop below must never
                // see an item wider than max_count, or the cap arithmetic
                // would produce an oversized read the crate rejects fatally.
                if words == 0 || words as u32 > MAX_WORDS_PER_READ {
                    immediate_bad.push((
                        index,
                        PlcError::StringSpanUnsupported {
                            words,
                            max: MAX_WORDS_PER_READ as u16,
                        },
                    ));
                    continue;
                }
            }
        }

        by_device
            .entry(device)
            .or_default()
            .push((index, number, kind));
    }

    let mut reads = Vec::new();
    for (device, mut items) in by_device {
        items.sort_by_key(|(_, number, _)| *number);
        let max_count = max_count_for(device.access()) as u64;
        let mut current: Option<Building> = None;

        for (index, number, kind) in items {
            // Bit devices are read one point at a time regardless of the tag's
            // width (a `bit` tag is the only thing that can live there), so the
            // span is 1; word devices span 1-2 words per
            // `DataType::register_span` (reused verbatim from the Modbus side
            // because "how many 16-bit words does an i32 occupy" has no
            // protocol in it), or the string's own word count.
            let span = match device.access() {
                SlmpAccess::Bit => 1u64,
                SlmpAccess::Word => match kind {
                    ReadKind::Numeric(data_type) => data_type.register_span() as u64,
                    ReadKind::Str { words } => words as u64,
                },
            };
            let start = number as u64;
            let end = start + span;

            let fits_current = current
                .as_ref()
                .map(|g| start <= g.end + MAX_GAP as u64 && end.max(g.end) - g.start <= max_count)
                .unwrap_or(false);

            if !fits_current {
                if let Some(g) = current.take() {
                    reads.push(g.finish());
                }
                current = Some(Building {
                    device,
                    start,
                    end,
                    mapping: Vec::new(),
                });
            }

            let group = current.as_mut().expect("just ensured Some above");
            group.end = group.end.max(end);
            group.mapping.push(SlmpMappedRequest {
                request_index: index,
                offset_in_read: (start - group.start) as u32,
                kind,
            });
        }

        if let Some(g) = current.take() {
            reads.push(g.finish());
        }
    }

    SlmpPlanOutcome {
        reads,
        immediate_bad,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::address::Address;

    fn req(raw: &str, data_type: DataType) -> ReadRequest {
        ReadRequest {
            address: Address::parse_slmp(raw).unwrap_or_else(|e| panic!("{raw} should parse: {e}")),
            data_type,
        }
    }

    #[test]
    fn single_request_becomes_a_single_group() {
        let outcome = plan_slmp_requests(&[req("D100", DataType::I16)]);
        assert!(outcome.immediate_bad.is_empty());
        assert_eq!(outcome.reads.len(), 1);
        let g = &outcome.reads[0];
        assert_eq!(g.device, SlmpDevice::D);
        assert_eq!(g.start, 100);
        assert_eq!(g.count, 1);
        assert_eq!(g.mapping.len(), 1);
        assert_eq!(g.mapping[0].request_index, 0);
        assert_eq!(g.mapping[0].offset_in_read, 0);
    }

    #[test]
    fn adjacent_requests_merge_into_one_group() {
        let outcome = plan_slmp_requests(&[
            req("D0", DataType::U16),
            req("D1", DataType::U16),
            req("D2", DataType::U16),
        ]);
        assert_eq!(outcome.reads.len(), 1);
        assert_eq!(outcome.reads[0].start, 0);
        assert_eq!(outcome.reads[0].count, 3);
        assert_eq!(outcome.reads[0].mapping.len(), 3);
    }

    #[test]
    fn small_gap_within_tolerance_still_merges() {
        let outcome = plan_slmp_requests(&[
            req("D0", DataType::U16),
            req(&format!("D{}", 1 + MAX_GAP), DataType::U16),
        ]);
        assert_eq!(
            outcome.reads.len(),
            1,
            "gap of exactly MAX_GAP should merge"
        );
        assert_eq!(outcome.reads[0].count, 2 + MAX_GAP);
    }

    #[test]
    fn gap_one_past_tolerance_splits_into_two_groups() {
        let outcome = plan_slmp_requests(&[
            req("D0", DataType::U16),
            req(&format!("D{}", 2 + MAX_GAP), DataType::U16),
        ]);
        assert_eq!(outcome.reads.len(), 2);
    }

    /// Two devices are two different address spaces even when the numbers
    /// overlap - `D0` and `R0` must never share a read.
    #[test]
    fn different_devices_never_share_a_group() {
        let outcome = plan_slmp_requests(&[
            req("D0", DataType::U16),
            req("R0", DataType::U16),
            req("W0", DataType::U16),
            req("M0", DataType::Bit),
            req("X0", DataType::Bit),
        ]);
        assert_eq!(outcome.reads.len(), 5);
        assert!(outcome.immediate_bad.is_empty());
    }

    #[test]
    fn thirty_two_bit_type_occupies_two_words_in_the_mapping() {
        let outcome = plan_slmp_requests(&[req("D0", DataType::F32), req("D2", DataType::I16)]);
        assert_eq!(outcome.reads.len(), 1);
        let g = &outcome.reads[0];
        assert_eq!(g.count, 3); // D0,D1 (f32) + D2 (i16)
        assert_eq!(g.mapping[0].offset_in_read, 0);
        assert_eq!(g.mapping[1].offset_in_read, 2);
    }

    #[test]
    fn out_of_order_input_is_sorted_before_grouping() {
        let outcome = plan_slmp_requests(&[
            req("D2", DataType::U16),
            req("D0", DataType::U16),
            req("D1", DataType::U16),
        ]);
        assert_eq!(outcome.reads.len(), 1);
        let g = &outcome.reads[0];
        assert_eq!(g.start, 0);
        assert_eq!(g.count, 3);
        let by_index: std::collections::HashMap<usize, u32> = g
            .mapping
            .iter()
            .map(|m| (m.request_index, m.offset_in_read))
            .collect();
        assert_eq!(by_index[&0], 2);
        assert_eq!(by_index[&1], 0);
        assert_eq!(by_index[&2], 1);
    }

    #[test]
    fn duplicate_addresses_both_map_into_the_same_group() {
        let outcome = plan_slmp_requests(&[req("D5", DataType::U16), req("D5", DataType::U16)]);
        assert_eq!(outcome.reads.len(), 1);
        assert_eq!(outcome.reads[0].mapping.len(), 2);
        assert_eq!(outcome.reads[0].mapping[0].offset_in_read, 0);
        assert_eq!(outcome.reads[0].mapping[1].offset_in_read, 0);
    }

    /// Hex-notation devices group on their *numeric* value, so a run of `X`
    /// points is contiguous on the wire rather than scattered.
    ///
    /// `X10` is the load-bearing case: read as hex it is 16, read as decimal it
    /// would be 10, and the two give different group extents (`count` 0x11 vs
    /// 0x0B). Asserting the extent is therefore what proves the radix survived
    /// all the way from the notation into the planned read.
    #[test]
    fn hex_notation_devices_group_on_their_numeric_value() {
        let outcome = plan_slmp_requests(&[
            req("X0", DataType::Bit),
            req("X5", DataType::Bit),
            req("XA", DataType::Bit),
            req("X10", DataType::Bit),
        ]);
        // Hops of 5, 5 and 6 - each within MAX_GAP + 1, so all four merge.
        assert_eq!(outcome.reads.len(), 1);
        assert_eq!(outcome.reads[0].start, 0);
        assert_eq!(
            outcome.reads[0].count, 0x11,
            "X10 must extend the group to 17 points (hex), not 11 (decimal)"
        );
    }

    #[test]
    fn splits_when_many_gap_sized_hops_would_exceed_the_word_limit() {
        let step = MAX_GAP + 1;
        let count = (MAX_WORDS_PER_READ / step) + 3;
        let requests: Vec<ReadRequest> = (0..count)
            .map(|i| req(&format!("D{}", i * step), DataType::U16))
            .collect();
        let outcome = plan_slmp_requests(&requests);
        assert!(
            outcome.reads.len() >= 2,
            "expected the word quantity limit to force a split, got {} group(s)",
            outcome.reads.len()
        );
        for g in &outcome.reads {
            assert!(g.count <= MAX_WORDS_PER_READ);
        }
    }

    #[test]
    fn splits_when_many_gap_sized_hops_would_exceed_the_bit_limit() {
        let step = MAX_GAP + 1;
        let count = (MAX_BITS_PER_READ / step) + 3;
        let requests: Vec<ReadRequest> = (0..count)
            .map(|i| req(&format!("M{}", i * step), DataType::Bit))
            .collect();
        let outcome = plan_slmp_requests(&requests);
        assert!(outcome.reads.len() >= 2);
        for g in &outcome.reads {
            assert!(g.count <= MAX_BITS_PER_READ);
        }
    }

    /// Both caps must come out of the same response-payload budget - that is
    /// the invariant that keeps a full-size reply inside one TCP segment, which
    /// is what the wrapped crate's single-`read` receive path requires (see
    /// [`MAX_RESPONSE_DATA_BYTES`]).
    #[test]
    fn both_caps_fit_the_same_response_payload_budget() {
        assert_eq!(MAX_WORDS_PER_READ * 2, MAX_RESPONSE_DATA_BYTES);
        assert_eq!(MAX_BITS_PER_READ.div_ceil(2), MAX_RESPONSE_DATA_BYTES);
    }

    #[test]
    fn bit_type_at_a_word_device_is_immediately_bad() {
        let outcome = plan_slmp_requests(&[req("D0", DataType::Bit)]);
        assert!(outcome.reads.is_empty());
        assert_eq!(outcome.immediate_bad.len(), 1);
        assert_eq!(outcome.immediate_bad[0].0, 0);
        match &outcome.immediate_bad[0].1 {
            PlcError::UnsupportedCombination { area, data_type } => {
                assert!(area.contains('D'), "message should name the device: {area}");
                assert_eq!(data_type, "bit");
            }
            other => panic!("expected UnsupportedCombination, got {other:?}"),
        }
    }

    #[test]
    fn numeric_type_at_a_bit_device_is_immediately_bad() {
        for raw in ["M0", "X0", "Y0", "TS0"] {
            let outcome = plan_slmp_requests(&[req(raw, DataType::I16)]);
            assert!(outcome.reads.is_empty(), "{raw} should not reach the wire");
            assert_eq!(outcome.immediate_bad.len(), 1);
        }
    }

    /// The timer/counter trio is the easiest place to get the bit/word split
    /// wrong, so it gets its own case: `TN`/`CN` take numeric tags, `TS`/`CS`
    /// take bit tags, and each rejects the other's.
    #[test]
    fn timer_and_counter_devices_accept_only_their_own_data_types() {
        for raw in ["TN0", "CN0", "SN0"] {
            assert!(plan_slmp_requests(&[req(raw, DataType::U16)])
                .immediate_bad
                .is_empty());
            assert_eq!(
                plan_slmp_requests(&[req(raw, DataType::Bit)])
                    .immediate_bad
                    .len(),
                1
            );
        }
        for raw in ["TS0", "TC0", "CS0", "CC0"] {
            assert!(plan_slmp_requests(&[req(raw, DataType::Bit)])
                .immediate_bad
                .is_empty());
            assert_eq!(
                plan_slmp_requests(&[req(raw, DataType::U16)])
                    .immediate_bad
                    .len(),
                1
            );
        }
    }

    #[test]
    fn a_modbus_address_is_immediately_bad_for_the_slmp_planner() {
        let requests = [
            ReadRequest {
                address: Address::parse("40001").unwrap(),
                data_type: DataType::U16,
            },
            req("D0", DataType::U16),
        ];
        let outcome = plan_slmp_requests(&requests);

        assert_eq!(outcome.immediate_bad.len(), 1);
        assert_eq!(outcome.immediate_bad[0].0, 0);
        match &outcome.immediate_bad[0].1 {
            PlcError::AddressProtocolMismatch { expected, actual } => {
                assert_eq!(expected, "slmp");
                assert_eq!(actual, "modbus-ref");
            }
            other => panic!("expected AddressProtocolMismatch, got {other:?}"),
        }

        assert_eq!(outcome.reads.len(), 1);
        assert_eq!(outcome.reads[0].mapping[0].request_index, 1);
    }

    #[test]
    fn immediate_bad_requests_do_not_block_valid_ones_from_being_planned() {
        let outcome = plan_slmp_requests(&[
            req("D0", DataType::Bit), // bad: bit tag at a word device
            req("D1", DataType::U16), // good
        ]);
        assert_eq!(outcome.immediate_bad.len(), 1);
        assert_eq!(outcome.immediate_bad[0].0, 0);
        assert_eq!(outcome.reads.len(), 1);
        assert_eq!(outcome.reads[0].mapping[0].request_index, 1);
    }

    #[test]
    fn empty_input_produces_no_reads_and_no_bad_entries() {
        let outcome = plan_slmp_requests(&[]);
        assert!(outcome.reads.is_empty());
        assert!(outcome.immediate_bad.is_empty());
    }

    /// The contract `slmp/mod.rs`'s result-assembly loop relies on: every
    /// input index is accounted for exactly once, so no `ReadResult` slot is
    /// ever left unfilled.
    #[test]
    fn every_input_index_is_accounted_for_exactly_once() {
        let requests = [
            req("D0", DataType::U16),
            req("D0", DataType::Bit), // immediate bad
            req("M0", DataType::Bit),
            req("D100", DataType::I32),
            ReadRequest {
                address: Address::parse("40001").unwrap(), // immediate bad
                data_type: DataType::U16,
            },
            req("X1F", DataType::Bit),
        ];
        let outcome = plan_slmp_requests(&requests);

        let mut seen: Vec<usize> = outcome
            .reads
            .iter()
            .flat_map(|g| g.mapping.iter().map(|m| m.request_index))
            .chain(outcome.immediate_bad.iter().map(|(i, _)| *i))
            .collect();
        seen.sort();
        assert_eq!(seen, (0..requests.len()).collect::<Vec<_>>());
    }

    // --- string spans (S1 文字列タグ) --------------------------------------

    use crate::types::{BatchReadRequest, StringReadRequest};

    fn sreq(raw: &str, words: u16) -> BatchReadRequest {
        BatchReadRequest::String(StringReadRequest {
            address: Address::parse_slmp(raw).unwrap_or_else(|e| panic!("{raw} should parse: {e}")),
            words,
        })
    }

    fn nreq(raw: &str, data_type: DataType) -> BatchReadRequest {
        BatchReadRequest::Numeric(req(raw, data_type))
    }

    /// A string occupies its full `words` span in the mapping, and an exactly
    /// adjacent numeric tag merges into the same read.
    #[test]
    fn a_string_spans_its_word_count_and_merges_with_an_adjacent_numeric() {
        let outcome = plan_slmp_batch(&[sreq("D0", 4), nreq("D4", DataType::U16)]);
        assert!(outcome.immediate_bad.is_empty());
        assert_eq!(outcome.reads.len(), 1);
        let g = &outcome.reads[0];
        assert_eq!(g.start, 0);
        assert_eq!(g.count, 5); // D0..D3 (string) + D4 (u16)
        assert_eq!(g.mapping[0].kind, ReadKind::Str { words: 4 });
        assert_eq!(g.mapping[0].offset_in_read, 0);
        assert_eq!(g.mapping[1].kind, ReadKind::Numeric(DataType::U16));
        assert_eq!(g.mapping[1].offset_in_read, 4);
    }

    /// A string wider than one bulk read can carry is a per-request Bad, not a
    /// panic, and its batch-mates still get planned.
    #[test]
    fn an_over_cap_string_is_immediately_bad_without_blocking_batch_mates() {
        let too_long = (MAX_WORDS_PER_READ + 1) as u16;
        let outcome = plan_slmp_batch(&[sreq("D0", too_long), nreq("D0", DataType::U16)]);
        assert_eq!(outcome.immediate_bad.len(), 1);
        assert_eq!(outcome.immediate_bad[0].0, 0);
        match &outcome.immediate_bad[0].1 {
            PlcError::StringSpanUnsupported { words, max } => {
                assert_eq!(*words, too_long);
                assert_eq!(*max as u32, MAX_WORDS_PER_READ);
            }
            other => panic!("expected StringSpanUnsupported, got {other:?}"),
        }
        assert_eq!(outcome.reads.len(), 1);
        assert_eq!(outcome.reads[0].mapping[0].request_index, 1);
    }

    /// A string of exactly the cap still fits one read.
    #[test]
    fn a_string_of_exactly_the_word_cap_is_planned_in_one_read() {
        let outcome = plan_slmp_batch(&[sreq("D0", MAX_WORDS_PER_READ as u16)]);
        assert!(outcome.immediate_bad.is_empty());
        assert_eq!(outcome.reads.len(), 1);
        assert_eq!(outcome.reads[0].count, MAX_WORDS_PER_READ);
    }

    #[test]
    fn a_zero_word_string_is_immediately_bad() {
        let outcome = plan_slmp_batch(&[sreq("D0", 0)]);
        assert!(outcome.reads.is_empty());
        assert_eq!(outcome.immediate_bad.len(), 1);
        assert!(matches!(
            outcome.immediate_bad[0].1,
            PlcError::StringSpanUnsupported { words: 0, .. }
        ));
    }

    #[test]
    fn a_string_at_a_bit_device_is_immediately_bad() {
        let outcome = plan_slmp_batch(&[sreq("M0", 4)]);
        assert!(outcome.reads.is_empty());
        assert_eq!(outcome.immediate_bad.len(), 1);
        match &outcome.immediate_bad[0].1 {
            PlcError::UnsupportedCombination { area, data_type } => {
                assert!(area.contains('M'), "message should name the device: {area}");
                assert_eq!(data_type, "string");
            }
            other => panic!("expected UnsupportedCombination, got {other:?}"),
        }
    }

    #[test]
    fn a_modbus_address_on_a_string_request_is_immediately_bad() {
        let outcome = plan_slmp_batch(&[BatchReadRequest::String(StringReadRequest {
            address: Address::parse("40001").unwrap(),
            words: 4,
        })]);
        assert!(outcome.reads.is_empty());
        assert!(matches!(
            outcome.immediate_bad[0].1,
            PlcError::AddressProtocolMismatch { .. }
        ));
    }

    /// Two strings whose combined span exceeds the cap split into two reads -
    /// the string case of "a read spanning the batching boundary".
    #[test]
    fn strings_split_when_their_combined_span_exceeds_the_word_cap() {
        let half = (MAX_WORDS_PER_READ / 2 + 10) as u16;
        let outcome = plan_slmp_batch(&[sreq("D0", half), sreq(&format!("D{half}"), half)]);
        assert!(outcome.immediate_bad.is_empty());
        assert_eq!(outcome.reads.len(), 2);
        for g in &outcome.reads {
            assert!(g.count <= MAX_WORDS_PER_READ);
            assert_eq!(g.count, half as u32);
        }
    }

    /// The numeric-only wrapper and the batch planner agree exactly - the
    /// tripwire that keeps `plan_slmp_requests` from drifting now that it
    /// delegates.
    #[test]
    fn plan_slmp_requests_matches_the_batch_planner_on_numeric_input() {
        let numeric = [
            req("D0", DataType::U16),
            req("D2", DataType::F32),
            req("M0", DataType::Bit),
            req("D0", DataType::Bit), // immediate bad
        ];
        let batch: Vec<BatchReadRequest> =
            numeric.iter().map(|&r| BatchReadRequest::Numeric(r)).collect();
        assert_eq!(plan_slmp_requests(&numeric), plan_slmp_batch(&batch));
    }

    /// A group at the very top of the device space must not overflow while
    /// its `end` is computed (a 32-bit tag there spans past
    /// `MAX_DEVICE_NUMBER`), which is what the `u64` bounds in [`Building`]
    /// are for.
    #[test]
    fn a_thirty_two_bit_tag_at_the_top_of_the_device_space_does_not_overflow() {
        let raw = format!("D{}", super::super::address::MAX_DEVICE_NUMBER);
        let outcome = plan_slmp_requests(&[req(&raw, DataType::U32)]);
        assert_eq!(outcome.reads.len(), 1);
        assert_eq!(
            outcome.reads[0].start,
            super::super::address::MAX_DEVICE_NUMBER
        );
        assert_eq!(outcome.reads[0].count, 2);
    }
}
