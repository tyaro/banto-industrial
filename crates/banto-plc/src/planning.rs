//! Request planning (docs/plan.md I2 §4): turn a flat list of
//! [`ReadRequest`]s into the minimal set of wire-level Modbus reads that
//! covers them, respecting Modbus's per-function quantity limits. Pure
//! function, no I/O - this is what makes 256-tag/100ms collection cycles
//! (docs/recorder-requirements.md §3.1) feasible at all: one round trip per
//! contiguous *group* of tags, not one per tag.
//!
//! ## Grouping rule
//!
//! Requests in the same [`AddressArea`] are sorted by offset and merged
//! greedily: a request joins the current group if its start is within
//! [`MAX_GAP`] elements of the group's current end *and* the group would
//! still fit within that area's per-request quantity limit
//! ([`MAX_REGISTERS_PER_READ`] / [`MAX_COILS_PER_READ`]); otherwise it
//! starts a new group. This trades a few wastefully-read-but-discarded
//! registers (up to `MAX_GAP` of them, per gap) for fewer round trips - the
//! right trade for the 100ms-target periodic case, where round trips (each
//! costing an RTT) dominate wall-clock time far more than a handful of
//! extra bytes on the wire.
//!
//! Requests whose `data_type` cannot exist in their `address`'s area (v1
//! restriction: bit types only in coil/discrete-input areas, everything
//! else only in register areas - docs/plan.md I2 §4) never reach the wire at
//! all: they are resolved immediately into [`PlanOutcome::immediate_bad`].
//! Since I2a the same is true of a request carrying a non-Modbus
//! [`crate::address::Address`] variant: this planner only understands
//! reference-number addressing, so an SLMP address here means the tag and its
//! `PlcConnection`'s protocol disagree, and it becomes an immediate
//! [`PlcError::AddressProtocolMismatch`] rather than being coerced into some
//! register offset. [`crate::slmp::planning::plan_slmp_requests`] is the
//! mirror-image function for the other variant.

use std::collections::BTreeMap;

use crate::address::AddressArea;
use crate::error::PlcError;
use crate::types::{BatchReadRequest, DataType, ReadRequest};

/// Gap tolerance, in the area's own element unit (registers for
/// input/holding-register areas, bits for coil/discrete-input areas): two
/// requests whose offsets are within this many elements of each other are
/// combined into one wire read rather than issued separately. Chosen as a
/// small, cheap-to-justify default - typical tag layouts (a device's related
/// process values) cluster within a handful of registers of each other, and
/// wasting at most 5 extra register reads per merge is negligible next to
/// the cost of an additional TCP round trip. Not yet exposed as
/// configuration; revisit once I3 has real device layouts to measure
/// against (docs/plan.md I2 §4 leaves the exact number to implementation).
const MAX_GAP: u16 = 5;

/// Modbus read-holding/input-registers (FC3/FC4) maximum quantity per
/// request (Modbus Application Protocol spec: max 0x7D = 125 registers,
/// bounded by the 253-byte PDU payload limit).
const MAX_REGISTERS_PER_READ: u16 = 125;

/// Modbus read-coils/discrete-inputs (FC1/FC2) maximum quantity per request
/// (Modbus Application Protocol spec: max 0x7D0 = 2000 bits, bounded by the
/// same PDU payload limit packed 8 bits/byte).
const MAX_COILS_PER_READ: u16 = 2000;

fn max_count_for(area: AddressArea) -> u16 {
    match area {
        AddressArea::Coil | AddressArea::DiscreteInput => MAX_COILS_PER_READ,
        AddressArea::InputRegister | AddressArea::HoldingRegister => MAX_REGISTERS_PER_READ,
    }
}

/// v1 restriction (docs/plan.md I2 §4): bit tags only address coil/
/// discrete-input areas; every other data type only addresses register
/// areas. "Read one bit out of a holding register" is now real (T8,
/// docs/tag-server-design.md §6.1) but is handled as a *separate* case in
/// [`plan_requests`]'s per-request match, not folded in here - see that
/// function's comment for why this function's contract stays exactly what
/// it was pre-T8: `is_compatible(area, data_type)` alone still means "does
/// this data type live in this area with **no** bit qualifier".
fn is_compatible(area: AddressArea, data_type: DataType) -> bool {
    match area {
        AddressArea::Coil | AddressArea::DiscreteInput => data_type == DataType::Bit,
        AddressArea::InputRegister | AddressArea::HoldingRegister => data_type != DataType::Bit,
    }
}

/// Where one original [`ReadRequest`] (by its index in the slice passed to
/// [`plan_requests`]) lands within a [`PlannedRead`]'s response window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MappedRequest {
    pub request_index: usize,
    /// Offset from the group's `start_offset`, in the same element unit as
    /// `count` (registers or bits). Index directly into the decoded
    /// register/bit window returned for this group.
    pub offset_in_read: u16,
    pub data_type: DataType,
    /// `Some(0..=15)` for a T8 bit-in-word request (§6.1: `"40001.3"`) - the
    /// bit position `modbus/mod.rs` must extract from the register window at
    /// `offset_in_read` instead of decoding the whole register as
    /// `data_type` (which is always [`DataType::Bit`] whenever this is
    /// `Some`, but the register itself holds 16 bits' worth of unrelated
    /// tags, not a bool). `None` for every pre-T8 request, register or coil
    /// alike.
    pub bit: Option<u8>,
}

/// One physical Modbus request: `modbus/mod.rs` issues exactly one FC1/2/3/4
/// call per `PlannedRead` (function code implied by `area`), then scatters
/// the response back out to every request in `mapping`.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedRead {
    pub area: AddressArea,
    pub start_offset: u16,
    pub count: u16,
    pub mapping: Vec<MappedRequest>,
}

/// [`plan_requests`]'s full result: requests that need a wire round trip
/// ([`PlannedRead`]s) plus requests that were already known-bad without one
/// (area/data-type mismatches, `(request_index, reason)` pairs).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlanOutcome {
    pub reads: Vec<PlannedRead>,
    pub immediate_bad: Vec<(usize, PlcError)>,
}

/// A group under construction, tracked with `u32` bounds so a 32-bit value
/// at the very top of the address space (`offset = 65_535`, `end =
/// 65_537`) cannot overflow `u16` mid-computation; only the finished
/// [`PlannedRead`] narrows back to `u16` (`start_offset`/`count`), which is
/// always in range because `end - start` never exceeds `max_count` (<=
/// 2000) by construction.
struct Building {
    area: AddressArea,
    start: u32,
    end: u32,
    mapping: Vec<MappedRequest>,
}

impl Building {
    fn finish(self) -> PlannedRead {
        PlannedRead {
            area: self.area,
            start_offset: self.start as u16,
            count: (self.end - self.start) as u16,
            mapping: self.mapping,
        }
    }
}

/// Plan wire-level reads for `requests`. See this module's doc comment for
/// the grouping rule and `error.rs` for why area/data-type mismatches are
/// resolved here rather than surfacing as a Modbus exception later.
///
/// ## T8 bit-in-word requests (docs/tag-server-design.md §6.1)
///
/// A request whose address carries a bit position (`"40001.3"`,
/// [`crate::address::Address::as_modbus_ref`]'s third element) is *not* a new
/// wire operation: it still reads the whole register at `offset` via the
/// area's ordinary FC3/FC4 group (the same [`PlannedRead`] a plain numeric
/// tag at that offset would use, and it happily shares a group with one -
/// the planner's existing adjacency/gap merging does not need to know a bit
/// is being extracted). Only the *decode* step differs, which is why the
/// bit position rides along on [`MappedRequest::bit`] rather than
/// influencing anything above (`span`, grouping, `max_count`): `DataType::Bit`
/// occupies exactly one register just like an `i16`/`u16`
/// ([`DataType::register_span`]), so no arithmetic in this function needed
/// to change. This is also what gives "duplicate bit tags on the same
/// register never cost an extra read" for free: two bit-in-word requests at
/// the same offset land in the same group exactly as two plain numeric
/// duplicates already did (`duplicate_addresses_both_map_into_the_same_group`).
///
/// A bit-in-word request is only valid at a register area
/// ([`AddressArea::InputRegister`]/[`AddressArea::HoldingRegister`]) with
/// `data_type == DataType::Bit` - the combination [`crate::address::Address::parse`] already
/// restricts the *address* text to, but `data_type` is a separate column
/// (`banto-tags::Tag::data_type`) that a misconfigured tag could still
/// disagree with (e.g. address `"40001.3"` with `data_type = "u16"`), so it
/// is re-checked here and rejected as [`PlcError::UnsupportedCombination`]
/// exactly like every other area/data-type mismatch this function resolves.
pub fn plan_requests(requests: &[ReadRequest]) -> PlanOutcome {
    let mut immediate_bad = Vec::new();

    // BTreeMap (not HashMap) so areas - and therefore the resulting
    // `reads` order - come out deterministic, which keeps tests and any
    // future request-count metrics stable across runs.
    // `(index, offset, data_type, bit)` per area: the offset is resolved up
    // front so the sort below (and the grouping loop) never has to re-match
    // on the `Address` variant it has already proven is `ModbusRef`. `bit` is
    // the T8 bit-in-word position (§6.1), `None` for a plain request.
    type AreaItem = (usize, u16, DataType, Option<u8>);
    let mut by_area: BTreeMap<AddressArea, Vec<AreaItem>> = BTreeMap::new();
    for (index, req) in requests.iter().enumerate() {
        let Some((area, offset, bit)) = req.address.as_modbus_ref() else {
            immediate_bad.push((
                index,
                PlcError::AddressProtocolMismatch {
                    expected: "modbus-tcp".to_string(),
                    actual: req.address.notation().to_string(),
                },
            ));
            continue;
        };

        let accepted = match bit {
            // T8 bit-in-word: only a register area, only a `bit` tag.
            Some(_) => {
                matches!(
                    area,
                    AddressArea::InputRegister | AddressArea::HoldingRegister
                ) && req.data_type == DataType::Bit
            }
            // Pre-T8 shape, unchanged.
            None => is_compatible(area, req.data_type),
        };

        if accepted {
            by_area
                .entry(area)
                .or_default()
                .push((index, offset, req.data_type, bit));
        } else {
            immediate_bad.push((
                index,
                PlcError::UnsupportedCombination {
                    area: area.to_string(),
                    data_type: req.data_type.to_string(),
                },
            ));
        }
    }

    let mut reads = Vec::new();
    for (area, mut items) in by_area {
        items.sort_by_key(|(_, offset, _, _)| *offset);
        let max_count = max_count_for(area) as u32;
        let mut current: Option<Building> = None;

        for (index, offset, data_type, bit) in items {
            let span = data_type.register_span() as u32;
            let start = offset as u32;
            let end = start + span;

            let fits_current = current
                .as_ref()
                .map(|g| start <= g.end + MAX_GAP as u32 && end.max(g.end) - g.start <= max_count)
                .unwrap_or(false);

            if !fits_current {
                if let Some(g) = current.take() {
                    reads.push(g.finish());
                }
                current = Some(Building {
                    area,
                    start,
                    end,
                    mapping: Vec::new(),
                });
            }

            let group = current.as_mut().expect("just ensured Some above");
            group.end = group.end.max(end);
            group.mapping.push(MappedRequest {
                request_index: index,
                offset_in_read: (start - group.start) as u16,
                data_type,
                bit,
            });
        }

        if let Some(g) = current.take() {
            reads.push(g.finish());
        }
    }

    PlanOutcome {
        reads,
        immediate_bad,
    }
}

/// The mixed-batch front door for the Modbus planner (S1 文字列タグ). String
/// support on Modbus is **out of scope for S1** - MELSEC string devices are an
/// SLMP concept, and no Modbus device profile has asked for one - so every
/// [`BatchReadRequest::String`] resolves to a per-request
/// [`PlcError::UnsupportedCombination`] `Bad` (mirroring how a bit tag at a
/// register address is handled: before any wire traffic, without taking its
/// numeric batch-mates down). Numeric entries are planned by
/// [`plan_requests`] unchanged, with their outcome indices mapped back to the
/// mixed batch's positions.
pub fn plan_batch_requests(requests: &[BatchReadRequest]) -> PlanOutcome {
    let mut immediate_bad = Vec::new();
    let mut numeric = Vec::with_capacity(requests.len());
    let mut numeric_to_original = Vec::with_capacity(requests.len());

    for (index, req) in requests.iter().enumerate() {
        match req {
            BatchReadRequest::Numeric(r) => {
                numeric.push(*r);
                numeric_to_original.push(index);
            }
            BatchReadRequest::String(_) => immediate_bad.push((
                index,
                PlcError::UnsupportedCombination {
                    area: "modbus-tcp".to_string(),
                    data_type: "string".to_string(),
                },
            )),
        }
    }

    let mut outcome = plan_requests(&numeric);
    for read in &mut outcome.reads {
        for m in &mut read.mapping {
            m.request_index = numeric_to_original[m.request_index];
        }
    }
    for (index, _) in &mut outcome.immediate_bad {
        *index = numeric_to_original[*index];
    }
    outcome.immediate_bad.extend(immediate_bad);

    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::address::Address;

    fn req(area: AddressArea, offset: u16, data_type: DataType) -> ReadRequest {
        ReadRequest {
            address: Address::ModbusRef {
                area,
                offset,
                bit: None,
            },
            data_type,
        }
    }

    /// T8 (docs/tag-server-design.md §6.1): a bit-in-word request at a
    /// register area.
    fn bit_in_word_req(area: AddressArea, offset: u16, bit: u8) -> ReadRequest {
        ReadRequest {
            address: Address::ModbusRef {
                area,
                offset,
                bit: Some(bit),
            },
            data_type: DataType::Bit,
        }
    }

    #[test]
    fn single_request_becomes_a_single_group() {
        let requests = [req(AddressArea::HoldingRegister, 10, DataType::I16)];
        let outcome = plan_requests(&requests);
        assert!(outcome.immediate_bad.is_empty());
        assert_eq!(outcome.reads.len(), 1);
        let g = &outcome.reads[0];
        assert_eq!(g.area, AddressArea::HoldingRegister);
        assert_eq!(g.start_offset, 10);
        assert_eq!(g.count, 1);
        assert_eq!(g.mapping.len(), 1);
        assert_eq!(g.mapping[0].request_index, 0);
        assert_eq!(g.mapping[0].offset_in_read, 0);
    }

    #[test]
    fn adjacent_requests_merge_into_one_group() {
        let requests = [
            req(AddressArea::HoldingRegister, 0, DataType::I16),
            req(AddressArea::HoldingRegister, 1, DataType::I16),
            req(AddressArea::HoldingRegister, 2, DataType::I16),
        ];
        let outcome = plan_requests(&requests);
        assert_eq!(outcome.reads.len(), 1);
        let g = &outcome.reads[0];
        assert_eq!(g.start_offset, 0);
        assert_eq!(g.count, 3);
        assert_eq!(g.mapping.len(), 3);
    }

    #[test]
    fn small_gap_within_tolerance_still_merges() {
        // offsets 0 and 0+1+MAX_GAP are exactly at the tolerance boundary.
        let requests = [
            req(AddressArea::HoldingRegister, 0, DataType::I16),
            req(AddressArea::HoldingRegister, 1 + MAX_GAP, DataType::I16),
        ];
        let outcome = plan_requests(&requests);
        assert_eq!(
            outcome.reads.len(),
            1,
            "gap of exactly MAX_GAP should merge"
        );
        assert_eq!(outcome.reads[0].count, 2 + MAX_GAP);
    }

    #[test]
    fn gap_one_past_tolerance_splits_into_two_groups() {
        let requests = [
            req(AddressArea::HoldingRegister, 0, DataType::I16),
            req(AddressArea::HoldingRegister, 2 + MAX_GAP, DataType::I16),
        ];
        let outcome = plan_requests(&requests);
        assert_eq!(outcome.reads.len(), 2);
    }

    #[test]
    fn different_areas_never_share_a_group() {
        let requests = [
            req(AddressArea::HoldingRegister, 0, DataType::I16),
            req(AddressArea::InputRegister, 0, DataType::I16),
            req(AddressArea::Coil, 0, DataType::Bit),
            req(AddressArea::DiscreteInput, 0, DataType::Bit),
        ];
        let outcome = plan_requests(&requests);
        assert_eq!(outcome.reads.len(), 4);
    }

    #[test]
    fn thirty_two_bit_type_occupies_two_registers_in_the_mapping() {
        let requests = [
            req(AddressArea::HoldingRegister, 0, DataType::F32),
            req(AddressArea::HoldingRegister, 2, DataType::I16),
        ];
        let outcome = plan_requests(&requests);
        assert_eq!(outcome.reads.len(), 1);
        let g = &outcome.reads[0];
        assert_eq!(g.count, 3); // registers 0,1 (f32) + register 2 (i16)
        assert_eq!(g.mapping[0].offset_in_read, 0);
        assert_eq!(g.mapping[1].offset_in_read, 2);
    }

    #[test]
    fn splits_on_a_gap_far_larger_than_tolerance() {
        let requests = [
            req(AddressArea::HoldingRegister, 0, DataType::I16),
            req(
                AddressArea::HoldingRegister,
                MAX_REGISTERS_PER_READ + 1,
                DataType::I16,
            ),
        ];
        let outcome = plan_requests(&requests);
        assert_eq!(outcome.reads.len(), 2);
    }

    #[test]
    fn splits_when_many_gap_sized_hops_would_exceed_register_limit() {
        // Chain requests MAX_GAP+1 apart (always within merge tolerance
        // pairwise) far enough that the cumulative span blows past
        // MAX_REGISTERS_PER_READ - the quantity cap must still kick in and
        // start a new group, independent of the gap check.
        let step = MAX_GAP + 1;
        let count = (MAX_REGISTERS_PER_READ / step) + 3;
        let requests: Vec<ReadRequest> = (0..count)
            .map(|i| req(AddressArea::HoldingRegister, i * step, DataType::I16))
            .collect();
        let outcome = plan_requests(&requests);
        assert!(
            outcome.reads.len() >= 2,
            "expected the register quantity limit to force a split, got {} group(s)",
            outcome.reads.len()
        );
        for g in &outcome.reads {
            assert!(g.count <= MAX_REGISTERS_PER_READ);
        }
    }

    #[test]
    fn splits_when_many_gap_sized_hops_would_exceed_coil_limit() {
        let step = MAX_GAP + 1;
        let count = (MAX_COILS_PER_READ / step) + 3;
        let requests: Vec<ReadRequest> = (0..count)
            .map(|i| req(AddressArea::Coil, i * step, DataType::Bit))
            .collect();
        let outcome = plan_requests(&requests);
        assert!(outcome.reads.len() >= 2);
        for g in &outcome.reads {
            assert!(g.count <= MAX_COILS_PER_READ);
        }
    }

    #[test]
    fn out_of_order_input_is_sorted_before_grouping() {
        let requests = [
            req(AddressArea::HoldingRegister, 2, DataType::I16),
            req(AddressArea::HoldingRegister, 0, DataType::I16),
            req(AddressArea::HoldingRegister, 1, DataType::I16),
        ];
        let outcome = plan_requests(&requests);
        assert_eq!(outcome.reads.len(), 1);
        let g = &outcome.reads[0];
        assert_eq!(g.start_offset, 0);
        assert_eq!(g.count, 3);
        // request_index 1 (offset 0) must map to offset_in_read 0, etc.
        let by_index: std::collections::HashMap<usize, u16> = g
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
        let requests = [
            req(AddressArea::HoldingRegister, 5, DataType::I16),
            req(AddressArea::HoldingRegister, 5, DataType::I16),
        ];
        let outcome = plan_requests(&requests);
        assert_eq!(outcome.reads.len(), 1);
        assert_eq!(outcome.reads[0].mapping.len(), 2);
        assert_eq!(outcome.reads[0].mapping[0].offset_in_read, 0);
        assert_eq!(outcome.reads[0].mapping[1].offset_in_read, 0);
    }

    #[test]
    fn bit_type_at_a_register_address_is_immediately_bad() {
        let requests = [req(AddressArea::HoldingRegister, 0, DataType::Bit)];
        let outcome = plan_requests(&requests);
        assert!(outcome.reads.is_empty());
        assert_eq!(outcome.immediate_bad.len(), 1);
        assert_eq!(outcome.immediate_bad[0].0, 0);
        assert!(matches!(
            outcome.immediate_bad[0].1,
            PlcError::UnsupportedCombination { .. }
        ));
    }

    #[test]
    fn register_type_at_a_coil_address_is_immediately_bad() {
        let requests = [req(AddressArea::Coil, 0, DataType::I16)];
        let outcome = plan_requests(&requests);
        assert!(outcome.reads.is_empty());
        assert_eq!(outcome.immediate_bad.len(), 1);
    }

    #[test]
    fn immediate_bad_requests_do_not_block_valid_ones_from_being_planned() {
        let requests = [
            req(AddressArea::HoldingRegister, 0, DataType::Bit), // bad
            req(AddressArea::HoldingRegister, 1, DataType::I16), // good
        ];
        let outcome = plan_requests(&requests);
        assert_eq!(outcome.immediate_bad.len(), 1);
        assert_eq!(outcome.immediate_bad[0].0, 0);
        assert_eq!(outcome.reads.len(), 1);
        assert_eq!(outcome.reads[0].mapping[0].request_index, 1);
    }

    #[test]
    fn empty_input_produces_no_reads_and_no_bad_entries() {
        let outcome = plan_requests(&[]);
        assert!(outcome.reads.is_empty());
        assert!(outcome.immediate_bad.is_empty());
    }

    /// I2a: an SLMP address can now reach this Modbus-only planner if a
    /// connection's `protocol` and a tag's `address` disagree. It must be
    /// resolved here, without wire traffic, and without taking its
    /// batch-mates down with it.
    #[test]
    fn an_slmp_address_is_immediately_bad_for_the_modbus_planner() {
        let requests = [
            ReadRequest {
                address: Address::parse_slmp("D100").unwrap(),
                data_type: DataType::U16,
            },
            req(AddressArea::HoldingRegister, 0, DataType::I16),
        ];
        let outcome = plan_requests(&requests);

        assert_eq!(outcome.immediate_bad.len(), 1);
        assert_eq!(outcome.immediate_bad[0].0, 0);
        match &outcome.immediate_bad[0].1 {
            PlcError::AddressProtocolMismatch { expected, actual } => {
                assert_eq!(expected, "modbus-tcp");
                assert_eq!(actual, "slmp");
            }
            other => panic!("expected AddressProtocolMismatch, got {other:?}"),
        }

        assert_eq!(outcome.reads.len(), 1);
        assert_eq!(outcome.reads[0].mapping[0].request_index, 1);
    }

    /// S1: string is not supported on Modbus - a string entry in a mixed
    /// batch is a per-request `Bad` with the same unsupported-combination
    /// shape as a bit-at-register mismatch, and its numeric batch-mates are
    /// still planned with their original indices.
    #[test]
    fn a_string_request_is_immediately_bad_on_modbus_without_blocking_batch_mates() {
        use crate::types::{BatchReadRequest, StringReadRequest};

        let requests = [
            BatchReadRequest::String(StringReadRequest {
                address: Address::ModbusRef {
                    area: AddressArea::HoldingRegister,
                    offset: 0,
                    bit: None,
                },
                words: 4,
            }),
            BatchReadRequest::Numeric(req(AddressArea::HoldingRegister, 10, DataType::I16)),
            BatchReadRequest::Numeric(req(AddressArea::HoldingRegister, 0, DataType::Bit)), // bad
        ];
        let outcome = plan_batch_requests(&requests);

        let mut bad_indices: Vec<usize> = outcome.immediate_bad.iter().map(|(i, _)| *i).collect();
        bad_indices.sort();
        assert_eq!(bad_indices, vec![0, 2]);
        let string_bad = outcome
            .immediate_bad
            .iter()
            .find(|(i, _)| *i == 0)
            .map(|(_, e)| e)
            .unwrap();
        match string_bad {
            PlcError::UnsupportedCombination { area, data_type } => {
                assert_eq!(area, "modbus-tcp");
                assert_eq!(data_type, "string");
            }
            other => panic!("expected UnsupportedCombination, got {other:?}"),
        }

        assert_eq!(outcome.reads.len(), 1);
        assert_eq!(outcome.reads[0].mapping[0].request_index, 1);
    }

    #[test]
    fn bit_requests_group_like_register_requests() {
        let requests = [
            req(AddressArea::Coil, 0, DataType::Bit),
            req(AddressArea::Coil, 1, DataType::Bit),
            req(AddressArea::Coil, 2, DataType::Bit),
        ];
        let outcome = plan_requests(&requests);
        assert_eq!(outcome.reads.len(), 1);
        assert_eq!(outcome.reads[0].count, 3);
    }

    // --- T8, docs/tag-server-design.md §6.1: bit-in-word requests ----------

    /// The core §6.1 property: a bit-in-word request folds into the
    /// register's existing word read rather than spending a wire operation
    /// of its own - `count` stays 1, not 2, for two bit tags on the same
    /// register.
    #[test]
    fn bit_in_word_request_folds_into_the_ordinary_register_read() {
        let outcome = plan_requests(&[bit_in_word_req(AddressArea::HoldingRegister, 0, 5)]);
        assert!(outcome.immediate_bad.is_empty());
        assert_eq!(outcome.reads.len(), 1);
        let g = &outcome.reads[0];
        assert_eq!(g.area, AddressArea::HoldingRegister);
        assert_eq!(g.start_offset, 0);
        assert_eq!(
            g.count, 1,
            "one bit-in-word tag must cost exactly one register"
        );
        assert_eq!(g.mapping[0].bit, Some(5));
        assert_eq!(g.mapping[0].data_type, DataType::Bit);
    }

    /// Two bit tags on the *same* register must not duplicate the read - the
    /// planner's existing "duplicate addresses share a group" dedup already
    /// covers this because both requests present the same `offset`; T8 adds
    /// no new dedup mechanism.
    #[test]
    fn multiple_bits_of_the_same_register_share_one_read() {
        let outcome = plan_requests(&[
            bit_in_word_req(AddressArea::HoldingRegister, 100, 0),
            bit_in_word_req(AddressArea::HoldingRegister, 100, 5),
            bit_in_word_req(AddressArea::HoldingRegister, 100, 15),
        ]);
        assert!(outcome.immediate_bad.is_empty());
        assert_eq!(
            outcome.reads.len(),
            1,
            "one register, one read, however many bits"
        );
        let g = &outcome.reads[0];
        assert_eq!(g.count, 1);
        assert_eq!(g.mapping.len(), 3);
        let bits: Vec<u8> = g.mapping.iter().map(|m| m.bit.unwrap()).collect();
        assert_eq!(bits, vec![0, 5, 15]);
    }

    /// A bit-in-word tag alongside a plain numeric tag at an adjacent
    /// register still merges into one read (the ordinary adjacency rule),
    /// unaffected by the bit qualifier.
    #[test]
    fn bit_in_word_request_merges_with_an_adjacent_plain_register_request() {
        let outcome = plan_requests(&[
            bit_in_word_req(AddressArea::HoldingRegister, 0, 3),
            req(AddressArea::HoldingRegister, 1, DataType::U16),
        ]);
        assert_eq!(outcome.reads.len(), 1);
        assert_eq!(outcome.reads[0].count, 2);
    }

    /// §6.1: a coil/discrete-input area is already bit-granular, so a
    /// bit-in-word request there is rejected rather than silently accepted -
    /// this exercises the planner's own defense in depth even though
    /// `Address::parse` already refuses to produce such an address.
    #[test]
    fn bit_in_word_request_at_a_bit_area_is_immediately_bad() {
        let outcome = plan_requests(&[bit_in_word_req(AddressArea::Coil, 0, 0)]);
        assert!(outcome.reads.is_empty());
        assert_eq!(outcome.immediate_bad.len(), 1);
        assert!(matches!(
            outcome.immediate_bad[0].1,
            PlcError::UnsupportedCombination { .. }
        ));
    }

    /// A tag whose `address` names a bit position but whose `data_type`
    /// column disagrees (not `bit`) is a per-request configuration error,
    /// not a silent whole-register read.
    #[test]
    fn bit_in_word_address_with_a_non_bit_data_type_is_immediately_bad() {
        let requests = [ReadRequest {
            address: Address::ModbusRef {
                area: AddressArea::HoldingRegister,
                offset: 0,
                bit: Some(3),
            },
            data_type: DataType::U16,
        }];
        let outcome = plan_requests(&requests);
        assert!(outcome.reads.is_empty());
        assert_eq!(outcome.immediate_bad.len(), 1);
        assert!(matches!(
            outcome.immediate_bad[0].1,
            PlcError::UnsupportedCombination { .. }
        ));
    }

    /// Existing (non-bit-qualified) behavior must be completely unchanged:
    /// this is the exact pre-T8 grouping test, re-run to pin down that
    /// `bit: None` requests take the same path they always did.
    #[test]
    fn pre_t8_requests_are_unaffected_by_the_bit_in_word_addition() {
        let requests = [
            req(AddressArea::HoldingRegister, 0, DataType::I16),
            req(AddressArea::HoldingRegister, 1, DataType::I16),
        ];
        let outcome = plan_requests(&requests);
        assert_eq!(outcome.reads.len(), 1);
        assert_eq!(outcome.reads[0].count, 2);
        for m in &outcome.reads[0].mapping {
            assert_eq!(m.bit, None);
        }
    }
}
