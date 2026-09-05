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
//!
//! ## T8 bit-in-word writes (docs/tag-server-design.md §6.1)
//!
//! [`BatchWriteRequest::BitInWord`] cannot be planned as an ordinary word
//! write: SLMP has no bit-in-word write command, so the *value* to put on
//! the wire (a whole word) is not knowable at plan time - it depends on
//! whatever the other 15 bits of that word hold at execute time, which this
//! pure, no-I/O module cannot see. So a `BitInWord` request produces a
//! **plan**, not a payload: [`SlmpPlannedBitWrite`] carries a `set_mask`/
//! `clear_mask` pair (which bits to force to 1, which to force to 0 - always
//! disjoint by construction) and a `mapping` of which request wants which
//! bit at which value, for `slmp/mod.rs`'s [`crate::execute_slmp_writes`] to
//! turn into an actual read/modify/write/verify sequence against a live
//! session. This is exactly the same plan/execute split the rest of this
//! crate already has (pure planner vs I/O executor) - T8 only adds a case
//! where the planner's output is a *recipe* rather than a ready-to-serialize
//! payload.
//!
//! **Grouping is exact-word, not gap-based, and never merges across words**:
//! every `BitInWord` request is grouped by `(device, number)` - the *word*
//! address - with no adjacency/gap logic at all (there is nothing to merge
//! across; a bit position is not a wire offset). Two requests naming the
//! same word compose into one [`SlmpPlannedBitWrite`] (mask OR-composition,
//! §6.1: "同一バッチ内の同一ワード宛てビット書き込みはマスク合成して1回の
//! RMW"); two requests naming *different* words always produce two separate
//! plans, full stop - there is no cap or threshold at which they would
//! merge, preserving the gap-tolerance-zero write-safety discipline this
//! module's other section documents (writing a word nobody asked about is
//! never acceptable, and merging two RMWs into one wire operation would mean
//! writing back a *different* word than either caller named). See
//! `different_words_never_merge_into_one_bit_write` for the tripwire.
//!
//! **Conflicting requests for the same bit are rejected, not resolved by
//! last-write-wins**: if two requests in the batch ask for the same
//! `(device, number, bit)` with different values (one `true`, one `false`),
//! silently picking one would make the outcome depend on iteration/insertion
//! order rather than anything the caller controls - a nondeterminism this
//! crate's whole safety posture (encode failures and address mismatches are
//! *rejected*, never guessed at) argues against. Both conflicting requests
//! become [`PlcWriteError::ConflictingBitWrite`] `Bad`s instead, and every
//! *other*, non-conflicting bit of the same word still proceeds normally
//! (duplicate requests for the same bit with the *same* value are fine and
//! simply both ride along in one `mapping`, mirroring the read planner's
//! `duplicate_addresses_both_map_into_the_same_group`).
//!
//! **Ordinary writes and bit-in-word writes never share a group, by
//! construction**: a `BitInWord` request is filtered into its own
//! `bit_writes` bucket before the ordinary `by_device`/`plan_word_device`
//! grouping ever sees it, so a plain word write to `D100` and a bit write to
//! `D100.5` in the same batch are always two independent operations, never
//! coalesced into one. Execution order between the two kinds when they
//! target the very same word (docs/tag-server-design.md §6.1: "通常のワード
//! 書き込みとビット書き込みが同一バッチに混在した場合の順序・独立性") is
//! the plainest possible answer given that independence: `execute_slmp_writes`
//! runs every ordinary [`SlmpPlannedWrite`] first, then every
//! [`SlmpPlannedBitWrite`] - see that function's doc comment for why this
//! (rather than interleaving by original request index) is the judgment call
//! T8 recorded, and for the resulting caveat (an ordinary write and an RMW to
//! the very same word in one batch always resolves ordinary-first).

use std::collections::{BTreeMap, BTreeSet};

use banto_plc::{DataType, SlmpAccess, SlmpDevice, WordOrder};

use crate::encode::{encode_bit_value, encode_string_value, encode_word_value};
use crate::error::PlcWriteError;
use crate::types::{BatchWriteRequest, WriteRequest};

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

/// One request's contribution to a [`SlmpPlannedBitWrite`]: which bit of the
/// group's word it wants, and at what value - the RMW twin of the read
/// side's `SlmpMappedRequest`, and what lets `execute_slmp_writes`'s
/// confirmation read verify (and report) each request's own bit
/// independently of its batch-mates (T8, docs/tag-server-design.md §6.1:
/// "確認値の該当ビットが期待値なら Ok、不一致なら該当要求 WriteResult::Bad").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BitWriteMapping {
    pub request_index: usize,
    pub bit: u8,
    pub value: bool,
}

/// One planned RMW operation (T8, docs/tag-server-design.md §6.1): read the
/// word at `device`/`number`, apply `set_mask`/`clear_mask`, write it back,
/// then confirm. `set_mask`/`clear_mask` are always disjoint (a bit is never
/// in both - see [`plan_slmp_write_batch`]'s conflict handling, which routes
/// a bit with contradictory requested values to `immediate_bad` instead of
/// ever reaching here), so the new word is simply
/// `(current & !(set_mask | clear_mask)) | set_mask`.
///
/// Unlike [`SlmpPlannedWrite`] (whose `payload` is already the exact bytes to
/// put on the wire), this plan carries no ready-made payload at all - the
/// value to write depends on a runtime read `slmp/mod.rs` has not done yet.
/// That is the fundamental reason bit-in-word writes are a separate type
/// rather than another [`WritePayload`] variant.
#[derive(Debug, Clone, PartialEq)]
pub struct SlmpPlannedBitWrite {
    pub device: SlmpDevice,
    pub number: u32,
    pub set_mask: u16,
    pub clear_mask: u16,
    pub mapping: Vec<BitWriteMapping>,
}

/// [`plan_slmp_writes`]'s full result. Same contract as the read planner's
/// outcome: every input index appears exactly once, either inside some
/// [`SlmpPlannedWrite`]'s `request_indices`, some [`SlmpPlannedBitWrite`]'s
/// `mapping`, or in `immediate_bad`. `bit_writes` is always populated
/// independently of `writes` - see this module's doc comment ("Ordinary
/// writes and bit-in-word writes never share a group") for why the two never
/// interact during planning.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SlmpWritePlanOutcome {
    pub writes: Vec<SlmpPlannedWrite>,
    pub bit_writes: Vec<SlmpPlannedBitWrite>,
    pub immediate_bad: Vec<(usize, PlcWriteError)>,
}

/// One encoded target awaiting grouping.
enum ItemPayload {
    Words(Vec<u16>),
    Bit(bool),
}

/// One `BitInWord` request awaiting composition into a word's
/// [`SlmpPlannedBitWrite`]: `(request_index, bit, value)`.
type BitItem = (usize, u8, bool);

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
    // A thin wrapper over the mixed planner (a `WriteRequest` is just the
    // `Numeric` case), so the two cannot drift - same shape as the read side's
    // `plan_slmp_requests`.
    let batch: Vec<BatchWriteRequest> = requests
        .iter()
        .map(|r| BatchWriteRequest::Numeric(*r))
        .collect();
    plan_slmp_write_batch(&batch, word_order)
}

/// Plan wire-level bulk writes for a mixed numeric + string batch (S1
/// 文字列タグ). Same contract and same rejections as [`plan_slmp_writes`],
/// plus the string-specific ones:
///
/// - a string at a bit device ([`PlcWriteError::UnsupportedCombination`])
/// - a span of zero words or wider than one bulk write
///   ([`PlcWriteError::StringSpanUnsupported`])
/// - text that will not fit `2 * words` Shift-JIS bytes, or that Shift-JIS
///   cannot represent ([`PlcWriteError::ValueOutOfRange`]) - rejected here,
///   before grouping, so **nothing** of a too-long string is ever written
///   (no truncation, no partial span)
///
/// An encodable string becomes an ordinary `words`-long word payload
/// (0x00-padded to its full span), so the gap-0 grouping below extends to L-word
/// spans with no special casing: an exactly-adjacent numeric target still
/// merges into the same `bulk_write`, and a gap still splits.
///
/// ## T8 bit-in-word requests (docs/tag-server-design.md §6.1)
///
/// A [`BatchWriteRequest::BitInWord`] is validated here exactly like every
/// other request (address must be SLMP, device must be a **word** device,
/// the address must actually carry a bit position - plain `"D100"` in a
/// `BitInWord` request is rejected as [`PlcWriteError::UnsupportedCombination`]
/// just as surely as `"D100.5"` would be for an ordinary [`WriteRequest`]),
/// but it is never encoded or placed in `by_device`/`writes` - see this
/// module's doc comment for why it becomes a [`SlmpPlannedBitWrite`] in
/// `outcome.bit_writes` instead, mask-composed per `(device, number)` with
/// same-bit conflicts rejected into `immediate_bad`.
pub fn plan_slmp_write_batch(
    requests: &[BatchWriteRequest],
    word_order: WordOrder,
) -> SlmpWritePlanOutcome {
    let mut immediate_bad = Vec::new();

    // BTreeMap for deterministic device (and therefore `writes`) ordering, same
    // reason as the read planner.
    let mut by_device: BTreeMap<SlmpDevice, Vec<(usize, u32, ItemPayload)>> = BTreeMap::new();

    // T8: bit-in-word requests never enter `by_device` - they are collected
    // here, per exact `(device, number)` word address, and turned into
    // `SlmpPlannedBitWrite`s in a separate pass below (see this module's doc
    // comment for why the two families can never share a group).
    let mut bit_items: BTreeMap<(SlmpDevice, u32), Vec<BitItem>> = BTreeMap::new();

    for (index, req) in requests.iter().enumerate() {
        let address = match req {
            BatchWriteRequest::Numeric(r) => r.address,
            BatchWriteRequest::String(s) => s.address,
            BatchWriteRequest::BitInWord { address, .. } => *address,
        };
        let Some((device, number, bit_pos)) = address.as_slmp() else {
            immediate_bad.push((
                index,
                PlcWriteError::AddressProtocolMismatch {
                    expected: "slmp".to_string(),
                    actual: address.notation().to_string(),
                },
            ));
            continue;
        };

        let item = match req {
            BatchWriteRequest::Numeric(r) => {
                // A bit-qualified address (`"D100.5"`) on a plain numeric
                // write is a configuration mistake, not a whole-register
                // write of some sort - T8 introduced the qualifier
                // specifically to route through `BitInWord`'s RMW path
                // instead.
                if bit_pos.is_some() || !is_compatible(device.access(), r.data_type) {
                    immediate_bad.push((
                        index,
                        PlcWriteError::UnsupportedCombination {
                            area: format!("{device} ({})", device.access()),
                            data_type: r.data_type.to_string(),
                        },
                    ));
                    continue;
                }
                match device.access() {
                    SlmpAccess::Bit => match encode_bit_value(r.value) {
                        Ok(b) => ItemPayload::Bit(b),
                        Err(e) => {
                            immediate_bad.push((index, e));
                            continue;
                        }
                    },
                    SlmpAccess::Word => match encode_word_value(r.value, r.data_type, word_order) {
                        Ok(words) => ItemPayload::Words(words),
                        Err(e) => {
                            immediate_bad.push((index, e));
                            continue;
                        }
                    },
                }
            }
            BatchWriteRequest::String(s) => {
                // Strings live in word devices only, same v1 rule as reads,
                // and never carry a bit qualifier (a string occupies a whole
                // span of words, not one bit of one).
                if device.access() != SlmpAccess::Word || bit_pos.is_some() {
                    immediate_bad.push((
                        index,
                        PlcWriteError::UnsupportedCombination {
                            area: format!("{device} ({})", device.access()),
                            data_type: "string".to_string(),
                        },
                    ));
                    continue;
                }
                // A span one bulk write cannot carry is a per-request Bad -
                // never a panic, and never split across two writes (a string
                // torn across a group boundary could be observed half-written).
                if s.words == 0 || s.words as u32 > MAX_WRITE_WORDS {
                    immediate_bad.push((
                        index,
                        PlcWriteError::StringSpanUnsupported {
                            words: s.words,
                            max: MAX_WRITE_WORDS as u16,
                        },
                    ));
                    continue;
                }
                match encode_string_value(&s.value, s.words, s.encoding) {
                    Ok(words) => ItemPayload::Words(words),
                    Err(e) => {
                        immediate_bad.push((index, e));
                        continue;
                    }
                }
            }
            BatchWriteRequest::BitInWord { value, .. } => {
                let Some(bit) = bit_pos else {
                    immediate_bad.push((
                        index,
                        PlcWriteError::UnsupportedCombination {
                            area: format!("{device}{number}"),
                            data_type: "bit_in_word（アドレスに .N ビット位置がありません）"
                                .to_string(),
                        },
                    ));
                    continue;
                };
                if device.access() != SlmpAccess::Word {
                    immediate_bad.push((
                        index,
                        PlcWriteError::UnsupportedCombination {
                            area: format!("{device} ({})", device.access()),
                            data_type: "bit_in_word".to_string(),
                        },
                    ));
                    continue;
                }
                bit_items
                    .entry((device, number))
                    .or_default()
                    .push((index, bit, *value));
                continue;
            }
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

    // T8: one SlmpPlannedBitWrite per distinct (device, number) - never
    // merged across words (see this module's doc comment), mask-composed
    // within a word, with same-bit conflicts rejected rather than guessed at.
    let mut bit_writes = Vec::new();
    for ((device, number), items) in bit_items {
        plan_bit_in_word_group(device, number, items, &mut immediate_bad, &mut bit_writes);
    }

    SlmpWritePlanOutcome {
        writes,
        bit_writes,
        immediate_bad,
    }
}

/// Compose one word's worth of `BitInWord` requests into (at most) one
/// [`SlmpPlannedBitWrite`], per this module's doc comment's "T8 bit-in-word
/// writes" section.
///
/// A bit named by two requests with the *same* value is fine - both simply
/// ride along in `mapping` (mirroring the read planner's duplicate-address
/// handling); a bit named with *conflicting* values rejects every request
/// naming that particular bit as [`PlcWriteError::ConflictingBitWrite`] and
/// excludes it from the mask entirely, while every other, non-conflicting
/// bit of the same word still proceeds normally in the returned group (if
/// any bits remain - a word whose every request conflicts produces no
/// [`SlmpPlannedBitWrite`] at all, only `immediate_bad` entries).
fn plan_bit_in_word_group(
    device: SlmpDevice,
    number: u32,
    items: Vec<BitItem>,
    immediate_bad: &mut Vec<(usize, PlcWriteError)>,
    out: &mut Vec<SlmpPlannedBitWrite>,
) {
    // First pass: which bit positions have more than one distinct requested
    // value. `BTreeMap`/`BTreeSet` (not `HashMap`/`HashSet`) so a conflict
    // report and the final mapping order are deterministic across runs.
    let mut seen_value: BTreeMap<u8, bool> = BTreeMap::new();
    let mut conflicted_bits: BTreeSet<u8> = BTreeSet::new();
    for &(_, bit, value) in &items {
        match seen_value.get(&bit) {
            Some(&existing) if existing != value => {
                conflicted_bits.insert(bit);
            }
            Some(_) => {} // consistent duplicate, not a conflict
            None => {
                seen_value.insert(bit, value);
            }
        }
    }

    let mut set_mask = 0u16;
    let mut clear_mask = 0u16;
    let mut mapping = Vec::new();
    for (index, bit, value) in items {
        if conflicted_bits.contains(&bit) {
            immediate_bad.push((
                index,
                PlcWriteError::ConflictingBitWrite {
                    area: format!("{device}{number}"),
                    bit,
                },
            ));
            continue;
        }
        if value {
            set_mask |= 1 << bit;
        } else {
            clear_mask |= 1 << bit;
        }
        mapping.push(BitWriteMapping {
            request_index: index,
            bit,
            value,
        });
    }

    if !mapping.is_empty() {
        out.push(SlmpPlannedBitWrite {
            device,
            number,
            set_mask,
            clear_mask,
            mapping,
        });
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

    // --- T8, docs/tag-server-design.md §6.1: bit-in-word RMW planning ------

    fn bwreq(raw: &str, value: bool) -> BatchWriteRequest {
        BatchWriteRequest::BitInWord {
            address: Address::parse_slmp(raw).unwrap_or_else(|e| panic!("{raw}: {e}")),
            value,
        }
    }

    #[test]
    fn single_bit_in_word_request_becomes_one_planned_bit_write() {
        let outcome = plan_slmp_write_batch(&[bwreq("D100.5", true)], LH);
        assert!(outcome.immediate_bad.is_empty());
        assert!(
            outcome.writes.is_empty(),
            "must not become an ordinary write"
        );
        assert_eq!(outcome.bit_writes.len(), 1);
        let g = &outcome.bit_writes[0];
        assert_eq!(g.device, SlmpDevice::D);
        assert_eq!(g.number, 100);
        assert_eq!(g.set_mask, 1 << 5);
        assert_eq!(g.clear_mask, 0);
        assert_eq!(
            g.mapping,
            vec![BitWriteMapping {
                request_index: 0,
                bit: 5,
                value: true
            }]
        );
    }

    #[test]
    fn a_clear_request_sets_the_clear_mask_not_the_set_mask() {
        let outcome = plan_slmp_write_batch(&[bwreq("D100.5", false)], LH);
        let g = &outcome.bit_writes[0];
        assert_eq!(g.set_mask, 0);
        assert_eq!(g.clear_mask, 1 << 5);
    }

    /// §6.1's core mask-composition property: several bits of the *same*
    /// word in one batch compose into a single [`SlmpPlannedBitWrite`], not
    /// one per bit.
    #[test]
    fn multiple_bits_of_the_same_word_compose_into_one_planned_bit_write() {
        let outcome = plan_slmp_write_batch(
            &[
                bwreq("D100.0", true),
                bwreq("D100.5", false),
                bwreq("D100.F", true), // hex bit 15 (T20-④)
            ],
            LH,
        );
        assert!(outcome.immediate_bad.is_empty());
        assert_eq!(
            outcome.bit_writes.len(),
            1,
            "one word, one RMW, however many bits"
        );
        let g = &outcome.bit_writes[0];
        assert_eq!(g.set_mask, (1 << 0) | (1 << 15));
        assert_eq!(g.clear_mask, 1 << 5);
        assert_eq!(g.mapping.len(), 3);
    }

    /// The write-safety discipline this whole crate exists for, restated for
    /// RMW: two *different* words must never be combined into one
    /// [`SlmpPlannedBitWrite`], however adjacent their numbers are.
    #[test]
    fn different_words_never_merge_into_one_bit_write() {
        let outcome = plan_slmp_write_batch(&[bwreq("D100.0", true), bwreq("D101.0", true)], LH);
        assert!(outcome.immediate_bad.is_empty());
        assert_eq!(
            outcome.bit_writes.len(),
            2,
            "adjacent words must still be two independent RMWs"
        );
        let numbers: Vec<u32> = outcome.bit_writes.iter().map(|g| g.number).collect();
        assert_eq!(numbers, vec![100, 101]);
    }

    /// Two requests naming the *same* bit with the *same* value are not a
    /// conflict - both simply ride along in `mapping` (harmless duplication,
    /// same precedent as the read planner's duplicate-address handling).
    #[test]
    fn duplicate_same_value_bit_requests_are_not_a_conflict() {
        let outcome = plan_slmp_write_batch(&[bwreq("D100.5", true), bwreq("D100.5", true)], LH);
        assert!(outcome.immediate_bad.is_empty());
        assert_eq!(outcome.bit_writes.len(), 1);
        assert_eq!(outcome.bit_writes[0].mapping.len(), 2);
        assert_eq!(outcome.bit_writes[0].set_mask, 1 << 5);
    }

    /// Two requests naming the same bit with *conflicting* values are both
    /// rejected - RMW cannot satisfy both, and guessing a winner would be a
    /// silent, order-dependent behavior.
    #[test]
    fn conflicting_bit_requests_are_both_rejected() {
        let outcome = plan_slmp_write_batch(
            &[
                bwreq("D100.5", true),
                bwreq("D100.5", false),
                bwreq("D100.6", true), // unaffected, non-conflicting bit
            ],
            LH,
        );
        assert_eq!(outcome.immediate_bad.len(), 2);
        for (index, err) in &outcome.immediate_bad {
            assert!(*index == 0 || *index == 1, "unexpected bad index {index}");
            assert!(matches!(err, PlcWriteError::ConflictingBitWrite { bit, .. } if *bit == 5));
        }
        // The non-conflicting bit still gets planned.
        assert_eq!(outcome.bit_writes.len(), 1);
        assert_eq!(outcome.bit_writes[0].set_mask, 1 << 6);
        assert_eq!(
            outcome.bit_writes[0].mapping,
            vec![BitWriteMapping {
                request_index: 2,
                bit: 6,
                value: true,
            }]
        );
    }

    /// A word whose *every* bit request conflicts produces no
    /// `SlmpPlannedBitWrite` at all - nothing left to compose.
    #[test]
    fn a_word_with_only_conflicting_requests_produces_no_planned_write() {
        let outcome = plan_slmp_write_batch(&[bwreq("D100.5", true), bwreq("D100.5", false)], LH);
        assert_eq!(outcome.immediate_bad.len(), 2);
        assert!(outcome.bit_writes.is_empty());
    }

    #[test]
    fn a_modbus_address_on_a_bit_in_word_write_is_immediately_bad() {
        let outcome = plan_slmp_write_batch(
            &[BatchWriteRequest::BitInWord {
                address: Address::parse("40001.3").unwrap(),
                value: true,
            }],
            LH,
        );
        assert!(outcome.bit_writes.is_empty());
        assert!(matches!(
            outcome.immediate_bad[0].1,
            PlcWriteError::AddressProtocolMismatch { .. }
        ));
    }

    /// A `BitInWord` request whose address carries no bit position at all
    /// (`"M50"`, not `"M50.N"` - and MELSEC notation has no `.N` suffix for
    /// bit devices in the first place, so this is also the only way a bit
    /// device ever reaches this match arm) is rejected rather than treated
    /// as targeting bit 0 or some other guessed position.
    #[test]
    fn bit_in_word_request_with_no_bit_position_is_immediately_bad() {
        let outcome = plan_slmp_write_batch(&[bwreq("M50", true)], LH);
        assert!(outcome.bit_writes.is_empty());
        assert_eq!(outcome.immediate_bad.len(), 1);
        assert!(matches!(
            outcome.immediate_bad[0].1,
            PlcWriteError::UnsupportedCombination { .. }
        ));
    }

    /// A plain numeric write whose address happens to carry a bit position
    /// (`"D100.5"`) is rejected rather than silently treated as a
    /// whole-register write - the qualifier means "use `BitInWord`".
    #[test]
    fn a_bit_qualified_address_on_a_plain_numeric_write_is_immediately_bad() {
        let outcome = plan_slmp_writes(&[word("D100.5", DataType::U16, 1.0)], LH);
        assert!(outcome.writes.is_empty());
        assert_eq!(outcome.immediate_bad.len(), 1);
        assert!(matches!(
            outcome.immediate_bad[0].1,
            PlcWriteError::UnsupportedCombination { .. }
        ));
    }

    /// Ordinary writes and bit-in-word writes to unrelated words in the same
    /// batch are fully independent: an ordinary `D200` write is unaffected
    /// by a `D100.5` bit write sharing the batch.
    #[test]
    fn ordinary_and_bit_in_word_writes_coexist_in_one_batch() {
        let outcome = plan_slmp_write_batch(
            &[
                nreq(word("D200", DataType::U16, 42.0)),
                bwreq("D100.5", true),
            ],
            LH,
        );
        assert!(outcome.immediate_bad.is_empty());
        assert_eq!(outcome.writes.len(), 1);
        assert_eq!(outcome.writes[0].request_indices, vec![0]);
        assert_eq!(outcome.bit_writes.len(), 1);
        assert_eq!(outcome.bit_writes[0].mapping[0].request_index, 1);
    }

    /// Every input index is accounted for exactly once even with `BitInWord`
    /// requests mixed in - the same contract `every_input_index_is_accounted_for_exactly_once`
    /// pins down for the pre-T8 request kinds.
    #[test]
    fn every_input_index_is_accounted_for_exactly_once_with_bit_in_word_requests() {
        let requests = [
            nreq(word("D0", DataType::U16, 1.0)),
            bwreq("D100.5", true),
            bwreq("D100.5", false), // conflicts with the previous one
            BatchWriteRequest::BitInWord {
                address: Address::parse("40001.3").unwrap(), // bad: modbus
                value: true,
            },
            bwreq("D200.0", true),
        ];
        let outcome = plan_slmp_write_batch(&requests, LH);

        let mut seen: Vec<usize> = outcome
            .writes
            .iter()
            .flat_map(|g| g.request_indices.iter().copied())
            .chain(
                outcome
                    .bit_writes
                    .iter()
                    .flat_map(|g| g.mapping.iter().map(|m| m.request_index)),
            )
            .chain(outcome.immediate_bad.iter().map(|(i, _)| *i))
            .collect();
        seen.sort();
        assert_eq!(seen, (0..requests.len()).collect::<Vec<_>>());
    }

    // --- string writes (S1 文字列タグ) -------------------------------------

    use crate::types::{BatchWriteRequest, StringWriteRequest};

    fn sreq(raw: &str, words: u16, value: &str) -> BatchWriteRequest {
        sreq_enc(raw, words, value, crate::types::StringEncoding::ShiftJis)
    }

    /// Same as [`sreq`] but with an explicit [`crate::types::StringEncoding`]
    /// - used by the UTF-8 planner tests below (T20 ①a).
    fn sreq_enc(
        raw: &str,
        words: u16,
        value: &str,
        encoding: crate::types::StringEncoding,
    ) -> BatchWriteRequest {
        BatchWriteRequest::String(StringWriteRequest {
            address: Address::parse_slmp(raw).unwrap_or_else(|e| panic!("{raw} should parse: {e}")),
            words,
            value: value.to_string(),
            encoding,
        })
    }

    fn nreq(req: WriteRequest) -> BatchWriteRequest {
        BatchWriteRequest::Numeric(req)
    }

    /// A string becomes its full padded span in the group payload, and an
    /// exactly-adjacent numeric target merges into the same write - the L-word
    /// generalization of the 32-bit span case above.
    #[test]
    fn a_string_spans_its_padded_word_count_and_merges_with_an_adjacent_numeric() {
        let outcome = plan_slmp_write_batch(
            &[sreq("D0", 4, "ABC"), nreq(word("D4", DataType::U16, 9.0))],
            LH,
        );
        assert!(outcome.immediate_bad.is_empty());
        assert_eq!(outcome.writes.len(), 1);
        let g = &outcome.writes[0];
        assert_eq!(g.start, 0);
        // "ABC" -> [0x4241, 0x0043, 0, 0] (low byte first, NUL-padded), then 9.
        assert_eq!(
            g.payload,
            WritePayload::Words(vec![0x4241, 0x0043, 0x0000, 0x0000, 0x0009])
        );
        assert_eq!(g.request_indices, vec![0, 1]);
    }

    /// The write-safety rule holds for strings too: a gap after the string's
    /// span splits the write, so the in-between device is never touched.
    #[test]
    fn a_string_never_merges_across_a_gap() {
        let outcome = plan_slmp_write_batch(
            &[sreq("D0", 2, "AB"), nreq(word("D3", DataType::U16, 1.0))],
            LH,
        );
        assert_eq!(outcome.writes.len(), 2, "D2 must never be written");
    }

    /// T20 ①a: the planner forwards each request's own `encoding` to
    /// `encode_string_value` rather than assuming Shift-JIS - a UTF-8 request
    /// for the same Japanese text as `a_string_spans_its_padded_word_count...`
    /// above produces different bytes (9 UTF-8 bytes vs. 6 Shift-JIS bytes for
    /// "テスト"), proving the encoding actually reaches the encoder.
    #[test]
    fn a_string_request_is_encoded_with_its_own_encoding() {
        let outcome = plan_slmp_write_batch(
            &[sreq_enc(
                "D0",
                5,
                "テスト",
                crate::types::StringEncoding::Utf8,
            )],
            LH,
        );
        assert!(outcome.immediate_bad.is_empty());
        assert_eq!(outcome.writes.len(), 1);
        let WritePayload::Words(words) = &outcome.writes[0].payload else {
            panic!("expected a Words payload");
        };
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let mut expected = "テスト".as_bytes().to_vec();
        expected.resize(10, 0x00);
        assert_eq!(bytes, expected);
    }

    /// Text longer than the span is rejected outright - per-item Bad, nothing
    /// grouped, batch-mates unaffected. The "no silent truncation" guarantee
    /// at the planner level.
    #[test]
    fn an_overlong_string_is_immediately_bad_and_nothing_of_it_is_planned() {
        let outcome = plan_slmp_write_batch(
            &[
                sreq("D0", 2, "ABCDE"), // 5 bytes > 4-byte capacity
                nreq(word("D10", DataType::U16, 5.0)),
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
        assert_eq!(outcome.writes[0].start, 10);
        assert_eq!(outcome.writes[0].request_indices, vec![1]);
    }

    #[test]
    fn a_string_at_a_bit_device_is_immediately_bad() {
        let outcome = plan_slmp_write_batch(&[sreq("M0", 4, "AB")], LH);
        assert!(outcome.writes.is_empty());
        assert_eq!(outcome.immediate_bad.len(), 1);
        match &outcome.immediate_bad[0].1 {
            PlcWriteError::UnsupportedCombination { area, data_type } => {
                assert!(area.contains('M'));
                assert_eq!(data_type, "string");
            }
            other => panic!("expected UnsupportedCombination, got {other:?}"),
        }
    }

    #[test]
    fn zero_and_over_cap_string_spans_are_immediately_bad_not_a_panic() {
        for words in [0u16, (MAX_WRITE_WORDS + 1) as u16] {
            let outcome = plan_slmp_write_batch(&[sreq("D0", words, "")], LH);
            assert!(outcome.writes.is_empty(), "words={words}");
            assert!(matches!(
                outcome.immediate_bad[0].1,
                PlcWriteError::StringSpanUnsupported { .. }
            ));
        }
    }

    #[test]
    fn a_modbus_address_on_a_string_write_is_immediately_bad() {
        let outcome = plan_slmp_write_batch(
            &[BatchWriteRequest::String(StringWriteRequest {
                address: Address::parse("40001").unwrap(),
                words: 4,
                value: "AB".to_string(),
                encoding: crate::types::StringEncoding::ShiftJis,
            })],
            LH,
        );
        assert!(matches!(
            outcome.immediate_bad[0].1,
            PlcWriteError::AddressProtocolMismatch { .. }
        ));
    }

    /// The numeric-only wrapper and the batch planner agree exactly - the
    /// tripwire that keeps `plan_slmp_writes` from drifting now that it
    /// delegates.
    #[test]
    fn plan_slmp_writes_matches_the_batch_planner_on_numeric_input() {
        let numeric = [
            word("D0", DataType::U16, 1.0),
            word("D1", DataType::F32, 1.5),
            bit("M0", true),
            word("D100", DataType::U16, 70000.0), // immediate bad
        ];
        let batch: Vec<BatchWriteRequest> = numeric
            .iter()
            .map(|r| BatchWriteRequest::Numeric(*r))
            .collect();
        assert_eq!(
            plan_slmp_writes(&numeric, LH),
            plan_slmp_write_batch(&batch, LH)
        );
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
