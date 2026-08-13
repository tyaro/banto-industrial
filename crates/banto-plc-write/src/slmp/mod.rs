//! [`SlmpWriteClient`]: the [`crate::client::PlcWriteClient`] implementation for
//! MELSEC MC / SLMP writes (I5). The write mirror of `banto-plc/src/slmp/mod.rs`,
//! wrapping the same `slmp` crate - here its `bulk_write` command rather than
//! `bulk_read` - and confining itself to the same two things the crate does not
//! do: deciding *what* to write ([`planning`]) and translating *how it failed*
//! into this crate's vocabulary ([`classify_slmp_error`]).
//!
//! ## The broker-sharing seam (get this shape right for W3)
//!
//! The relay-wright plan's PLC access broker (W3) owns **one**
//! `slmp::SLMPClient` per CPU and drives *both* reads and writes over it (a
//! single session per CPU resolves the MELSEC concurrent-session limit and
//! serializes read/write access to one point). So the actual bulk-write
//! execution is factored out of [`SlmpWriteClient`] into a free function,
//! [`execute_slmp_writes`], that operates on a borrowed `&mut slmp::SLMPClient`.
//! The broker can call [`planning::plan_slmp_writes`] (pure) and
//! [`execute_slmp_writes`] directly against the very same
//! `slmp::SLMPClient` it also issues reads on - without going through
//! [`SlmpWriteClient`]'s own socket at all. [`SlmpWriteClient`] is just the
//! standalone, owns-its-own-socket wrapper over that same function, which is
//! what this crate's unit tests and simulator exercise. This is exactly how the
//! read side keeps `plan_slmp_requests` (pure) separate from the socket I/O.
//!
//! ## Where the connection-fatal / per-request line falls
//!
//! Identical to the read side, because it wraps the identical error surface:
//! the crate reports every failure as its own `slmp::SlmpError`, and only a
//! *non-zero SLMP end code* (`SlmpError::Device { end_code }`) is safe to
//! continue past (the CPU refused this one write but answered with a
//! complete, length-consistent frame, so the byte stream is still aligned).
//! Everything else - `Framing`, `Timeout`, `NotConnected`, `Io` - is fatal:
//! [`execute_slmp_writes`] returns `Err`, and the owner drops the session.
//! H9 (docs/h9-slmp-structured-error-spec.md, 2026-08-12) moved this crate
//! onto the owner's `slmp` fork (0.2.0, git dependency - see the workspace
//! `Cargo.toml`), whose `SlmpError` exposes `Device` as a real enum variant,
//! so [`classify_slmp_error`] tells the two apart with a plain structural
//! `match` - no message-text parsing anywhere in this module any more. The
//! coupling to the wrapped crate's actual behavior (does it really keep
//! `Device` and `Framing` distinct end-to-end) is still real, so it is still
//! guarded by `slmp_write_end_code_is_bad_not_fatal` in
//! `integration_tests.rs`, the write twin of the read side's tripwire.
//!
//! ## Two deliberate differences from the read client
//!
//! 1. **No outer `tokio::time::timeout` around each write** - same reasoning as
//!    the read client: the wrapped crate applies its own send/receive deadlines
//!    (wired to [`banto_plc::SlmpConfig::response_timeout`]), so a second outer
//!    timeout would reintroduce the mid-frame cancellation the inner one exists
//!    to avoid.
//! 2. **Planning never coalesces across a gap** - a write has a side effect, so
//!    [`planning`] merges only exactly-adjacent targets (the read planner
//!    tolerates a small gap to save round trips). See its module doc.
//!
//! ## T8 bit-in-word RMW (docs/tag-server-design.md §6.1)
//!
//! [`execute_slmp_writes`] additionally runs every [`planning::SlmpPlannedBitWrite`]
//! in `outcome.bit_writes` as a **read → modify → write → confirm** sequence
//! against the same borrowed `slmp::SLMPClient` its ordinary writes use -
//! four wire operations per RMW group (one word each way, twice), all
//! sequential `.await`s on the one session this function is handed. This is
//! precisely what makes the RMW race-free with respect to *this crate's own*
//! concurrent callers without any new locking: `execute_slmp_writes` is
//! always called from inside one broker job
//! (`banto_broker::run_broker_task`'s `Job::Write` arm), and a broker task
//! services jobs strictly one at a time off its mpsc queue (see
//! `banto-broker`'s own module doc, "Message shape and how serialization is
//! guaranteed") - so no other read or write against the same CPU can land on
//! the wire between this RMW's read and its write-back. **No `banto-broker`
//! code changed to get this property**: the broker already treats a write
//! job as "call `execute_slmp_writes` and await the whole thing", so an RMW
//! sequence hiding behind that one call is invisible to it.
//!
//! What the RMW *cannot* defend against - and is not asked to, per §6.1's
//! explicit decision to accept the race and only detect it - is the target
//! PLC's own scan writing the same word between our read and our write-back.
//! The **confirmation read** exists
//! to *detect* that (never to retry or resolve it): after the write-back,
//! every bit-write group's word is read back once more, and each
//! [`planning::BitWriteMapping`] entry's own bit is checked independently
//! against its own requested value - a mismatch becomes
//! [`crate::error::PlcWriteError::BitWriteVerificationFailed`] for that one
//! request while every other bit written in the same RMW (and every other
//! group entirely) is judged on its own merits. The operational mitigation
//! (§6.1: an externally-writable bit's word must not also be written by PLC
//! ladder logic) lives in the deployment runbook, not in this code.
//!
//! **Execution order between ordinary writes and bit-in-word writes**
//! (docs/tag-server-design.md §6.1's "同一バッチ内...順序・独立性"): every
//! [`planning::SlmpPlannedWrite`] in `outcome.writes` runs to completion
//! before the first [`planning::SlmpPlannedBitWrite`] in `outcome.bit_writes`
//! starts. This is the plainest implementation of "independent execution"
//! the planner's own separation (see `planning`'s module doc: the two never
//! share a group, ever) allows for - the alternative (interleaving both
//! kinds by original request index so a `BitInWord` request submitted
//! *before* an ordinary write to the same word executes first) would need a
//! third, order-preserving data structure threading both plan vectors
//! together for a benefit that only matters when a caller mixes an ordinary
//! whole-word write and a targeted bit write to the *very same word* in one
//! batch - an unusual pattern to begin with, and one where the caller can
//! always avoid the ambiguity by splitting it across two `write`/`write_batch_mixed`
//! calls if the order matters to them. Recorded here as the T8 judgment call
//! rather than left implicit.

pub mod planning;

#[cfg(any(test, feature = "simulator"))]
pub mod simulator;

#[cfg(test)]
mod integration_tests;

use banto_plc::{dial_slmp, BoxFuture, PlcError, SlmpConfig, SlmpDevice};

use crate::client::PlcWriteClient;
use crate::error::PlcWriteError;
use crate::types::{WriteRequest, WriteResult};

use planning::{
    plan_slmp_write_batch, plan_slmp_writes, SlmpPlannedBitWrite, SlmpWritePlanOutcome,
    WritePayload,
};

/// Map [`banto_plc::SlmpDevice`] onto the wrapped crate's `DeviceType`.
///
/// This duplicates `banto-plc`'s own `SlmpDevice::to_wire`, which is
/// `pub(crate)` there and so not reachable from this crate. The duplication is
/// total and one-for-one, and kept honest exactly as the original is: by
/// `device_wire_codes_match_the_wrapped_crate`, which checks every device
/// against the actual byte the crate puts on the wire, so a transposed pair
/// here cannot silently write the wrong device.
fn device_to_wire(device: SlmpDevice) -> slmp::DeviceType {
    use slmp::DeviceType as W;
    match device {
        SlmpDevice::X => W::X,
        SlmpDevice::Y => W::Y,
        SlmpDevice::M => W::M,
        SlmpDevice::L => W::L,
        SlmpDevice::F => W::F,
        SlmpDevice::V => W::V,
        SlmpDevice::B => W::B,
        SlmpDevice::D => W::D,
        SlmpDevice::W => W::W,
        SlmpDevice::S => W::S,
        SlmpDevice::Z => W::Z,
        SlmpDevice::R => W::R,
        SlmpDevice::ZR => W::ZR,
        SlmpDevice::TS => W::TS,
        SlmpDevice::TC => W::TC,
        SlmpDevice::TN => W::TN,
        SlmpDevice::SS => W::SS,
        SlmpDevice::SC => W::SC,
        SlmpDevice::SN => W::SN,
        SlmpDevice::CS => W::CS,
        SlmpDevice::CC => W::CC,
        SlmpDevice::CN => W::CN,
        SlmpDevice::SB => W::SB,
        SlmpDevice::SD => W::SD,
        SlmpDevice::SM => W::SM,
        SlmpDevice::SW => W::SW,
        SlmpDevice::DX => W::DX,
        SlmpDevice::DY => W::DY,
    }
}

/// Translate the wrapped crate's structured [`slmp::SlmpError`] into a
/// [`PlcWriteError`], deciding connection-fatal vs per-request `Bad`. Same
/// mapping table as the read side's `classify_slmp_error` (`banto-plc`'s
/// `slmp/mod.rs`), just returning this crate's own error type - a plain
/// structural `match`, no message-text parsing (H9,
/// docs/h9-slmp-structured-error-spec.md).
fn classify_slmp_error(err: slmp::SlmpError) -> PlcWriteError {
    match err {
        slmp::SlmpError::Device { end_code } => PlcWriteError::SlmpEndCode {
            code: end_code,
            message: slmp::end_code_name(end_code).to_string(),
        },
        slmp::SlmpError::Framing(e) => PlcWriteError::Protocol(e.to_string()),
        slmp::SlmpError::Timeout => PlcWriteError::ResponseTimeout,
        slmp::SlmpError::NotConnected => PlcWriteError::NotConnected,
        slmp::SlmpError::Io(e) => PlcWriteError::Connection(e.to_string()),
    }
}

/// Map a [`banto_plc::PlcError`] coming out of [`dial_slmp`] onto this
/// crate's [`PlcWriteError`], preserving [`SlmpWriteClient::connect`]'s
/// former inline mapping exactly: `dial_slmp` only ever fails with
/// [`PlcError::ConnectTimeout`] or [`PlcError::Connection`], and both carry
/// the identical address/message `String` the old inline code built, so this
/// unwraps and rewraps them rather than going through `PlcError`'s own
/// `Display` (which would double up its own "接続エラー:" prefix onto the
/// message). Not a general `PlcError` → `PlcWriteError` conversion - H9's
/// read/write error vocabularies are deliberately separate (see this
/// module's doc comment) - so no blanket `From` is added for the other
/// variants; the catch-all only guards against `dial_slmp` growing a new
/// failure mode in the future.
fn dial_error_to_write(err: PlcError) -> PlcWriteError {
    match err {
        PlcError::ConnectTimeout(addr) => PlcWriteError::ConnectTimeout(addr),
        PlcError::Connection(msg) => PlcWriteError::Connection(msg),
        other => PlcWriteError::Connection(other.to_string()),
    }
}

/// Execute a planned batch of writes on a **borrowed** `slmp::SLMPClient`, the
/// reusable core the W3 broker calls directly on its shared per-CPU session (see
/// this module's doc comment). Issues one `bulk_write` per group in
/// `outcome.writes`, then one RMW (read/modify/write/confirm) sequence per
/// group in `outcome.bit_writes` (T8, docs/tag-server-design.md §6.1 - see
/// this module's doc comment for the ordering rationale and the broker-job
/// framing that makes the RMW race-free with respect to this crate's own
/// concurrent callers), folds `outcome.immediate_bad` in by index, and
/// returns a `Vec<WriteResult>` of length `total_requests` in original
/// request order.
///
/// `Err` is reserved for a connection-fatal failure (the caller must drop the
/// session and reconnect); a device-side end code becomes a per-request `Bad`
/// for that group's requests and the loop continues (an RMW's own end code,
/// wherever in its four-operation sequence it occurs, is exactly the same
/// per-request `Bad`, not a whole-call failure). On a fatal `Err`, any
/// groups already written (and any RMWs already completed) have landed on
/// the PLC - a partial batch - exactly as the read side discards partial
/// results on `Err`; the caller decides how to recover.
///
/// Does not own or reconnect the socket: connection lifecycle is the caller's
/// ([`SlmpWriteClient`] for the standalone form, the broker for the shared one).
pub async fn execute_slmp_writes(
    client: &mut slmp::SLMPClient,
    outcome: &SlmpWritePlanOutcome,
    total_requests: usize,
) -> Result<Vec<WriteResult>, PlcWriteError> {
    let mut results: Vec<Option<WriteResult>> = vec![None; total_requests];
    for (index, reason) in &outcome.immediate_bad {
        results[*index] = Some(WriteResult::Bad(reason.clone()));
    }

    for group in &outcome.writes {
        let start_device = slmp::Device {
            device_type: device_to_wire(group.device),
            address: group.start as usize,
        };
        // Homogeneous by construction: a group is all-word or all-bit, so the
        // `TypedData` vector is uniform and the wrapped crate's bulk_write picks
        // the right access type (it treats an all-`Bool` slice as bit access,
        // anything else as word access).
        let data: Vec<slmp::TypedData> = match &group.payload {
            WritePayload::Words(words) => words.iter().map(|&w| slmp::TypedData::U16(w)).collect(),
            WritePayload::Bits(bits) => bits.iter().map(|&b| slmp::TypedData::Bool(b)).collect(),
        };

        match client.bulk_write(start_device, &data).await {
            Ok(()) => {
                for &index in &group.request_indices {
                    results[index] = Some(WriteResult::Ok);
                }
            }
            Err(e) => {
                let err = classify_slmp_error(e);
                if err.is_connection_fatal() {
                    return Err(err);
                }
                // Device-side end code: the CPU refused this one group but the
                // frame was complete, so only these requests are bad.
                for &index in &group.request_indices {
                    results[index] = Some(WriteResult::Bad(err.clone()));
                }
            }
        }
    }

    // T8: ordinary writes above always finish before any RMW starts here -
    // see this module's doc comment for why that ordering (rather than
    // interleaving by original request index) is the recorded judgment call.
    for group in &outcome.bit_writes {
        if let Some(fatal) = execute_one_bit_write(client, group, &mut results).await {
            // `execute_one_bit_write` only returns `Some` for a connection-
            // fatal failure partway through its own read/write/confirm
            // sequence - propagate exactly like an ordinary group's fatal
            // branch above.
            return Err(fatal);
        }
    }

    Ok(results
        .into_iter()
        .enumerate()
        .map(|(i, r)| {
            r.unwrap_or_else(|| {
                panic!("plan_slmp_writes must account for every input index, missing {i}")
            })
        })
        .collect())
}

/// Read the current value of a single word device via one `bulk_read`, for
/// [`execute_one_bit_write`]'s read and confirmation steps. Returns
/// `Ok(word)` on a clean single-word response; otherwise `Err` carries
/// whatever [`classify_slmp_error`] produced (fatal or per-request), and the
/// caller tells the two apart via [`PlcWriteError::is_connection_fatal`] -
/// the same contract every other fallible step in this module already uses,
/// so callers do not need a second, RMW-specific error shape.
async fn read_one_word(
    client: &mut slmp::SLMPClient,
    device: SlmpDevice,
    number: u32,
) -> Result<u16, PlcWriteError> {
    let target = slmp::Device {
        device_type: device_to_wire(device),
        address: number as usize,
    };
    let data = client
        .bulk_read(target, 1, slmp::DataType::U16)
        .await
        .map_err(classify_slmp_error)?;
    match data.first().map(|d| &d.data) {
        Some(slmp::TypedData::U16(word)) => Ok(*word),
        other => Err(PlcWriteError::Protocol(format!(
            "RMW 読み出し {device}{number} が想定外の応答を返しました: {other:?}"
        ))),
    }
}

/// Run one [`SlmpPlannedBitWrite`]'s read/modify/write/confirm sequence (T8,
/// docs/tag-server-design.md §6.1) against `client`, writing every mapped
/// request's outcome into `results`.
///
/// Returns `None` when the whole sequence completed: every mapped request's
/// slot in `results` has been filled - `Ok`, or a per-request `Bad` for a
/// device-side end code (at the read, write-back, or confirmation step) or a
/// [`PlcWriteError::BitWriteVerificationFailed`] mismatch. None of those
/// abort the sequence early, because each is per-request by construction -
/// by the time this function can report one, it has already established the
/// connection itself is fine.
///
/// Returns `Some(err)` when a connection-fatal failure occurred at some step
/// (initial read, write-back, or confirmation read) and the sequence stopped
/// immediately; the caller propagates `err` via `Err` from
/// [`execute_slmp_writes`], exactly mirroring how an ordinary group's fatal
/// branch works. `results` is left with this group's mapped requests
/// unfilled in that case - harmless, since `execute_slmp_writes` never reads
/// `results` back out once it has decided to return `Err`.
async fn execute_one_bit_write(
    client: &mut slmp::SLMPClient,
    group: &SlmpPlannedBitWrite,
    results: &mut [Option<WriteResult>],
) -> Option<PlcWriteError> {
    // Step 1: read the word's current value.
    let current = match read_one_word(client, group.device, group.number).await {
        Ok(word) => word,
        Err(err) if err.is_connection_fatal() => return Some(err),
        Err(err) => {
            for m in &group.mapping {
                results[m.request_index] = Some(WriteResult::Bad(err.clone()));
            }
            return None;
        }
    };

    // Step 2: apply the mask and write the whole word back. `set_mask` and
    // `clear_mask` are disjoint by construction (see `SlmpPlannedBitWrite`'s
    // doc comment), so this is unambiguous: force `set_mask` bits to 1,
    // `clear_mask` bits to 0, leave every other bit exactly as read.
    let new_word = (current & !(group.set_mask | group.clear_mask)) | group.set_mask;
    let start_device = slmp::Device {
        device_type: device_to_wire(group.device),
        address: group.number as usize,
    };
    if let Err(e) = client
        .bulk_write(start_device, &[slmp::TypedData::U16(new_word)])
        .await
    {
        let err = classify_slmp_error(e);
        if err.is_connection_fatal() {
            return Some(err);
        }
        for m in &group.mapping {
            results[m.request_index] = Some(WriteResult::Bad(err.clone()));
        }
        return None;
    }

    // Step 3: confirmation read (§6.1 - mandatory, same job, same socket).
    let confirmed = match read_one_word(client, group.device, group.number).await {
        Ok(word) => word,
        Err(err) if err.is_connection_fatal() => return Some(err),
        Err(err) => {
            for m in &group.mapping {
                results[m.request_index] = Some(WriteResult::Bad(err.clone()));
            }
            return None;
        }
    };

    // Step 4: verify each request's own bit independently - one request's
    // mismatch never marks a batch-mate's correctly-landed bit as Bad.
    for m in &group.mapping {
        let landed = (confirmed >> m.bit) & 1 == 1;
        results[m.request_index] = Some(if landed == m.value {
            WriteResult::Ok
        } else {
            WriteResult::Bad(PlcWriteError::BitWriteVerificationFailed {
                area: format!("{}{}", group.device, group.number),
                bit: m.bit,
            })
        });
    }
    None
}

/// The standalone [`PlcWriteClient`] for MELSEC MC / SLMP. Owns its own socket -
/// this is the form unit tests and the simulator drive. The W3 broker does *not*
/// use this; it calls [`execute_slmp_writes`] on its own shared session (see the
/// module doc). One instance per connection, not `Clone`, not internally
/// reconnecting.
pub struct SlmpWriteClient {
    config: SlmpConfig,
    /// `Some` exactly while connected. Cleared on any connection-fatal failure,
    /// which is what enforces "every call returns `NotConnected` until
    /// `connect()`", matching the read client.
    inner: Option<slmp::SLMPClient>,
}

impl SlmpWriteClient {
    pub fn new(config: SlmpConfig) -> Self {
        Self {
            config,
            inner: None,
        }
    }

    /// Write a mixed numeric + string batch in one call (S1 文字列タグ) - the
    /// owned-socket form of [`plan_slmp_write_batch`] + [`execute_slmp_writes`],
    /// with exactly [`PlcWriteClient::write_batch`]'s connection semantics.
    /// An inherent method rather than part of the trait for the same reason
    /// `SlmpClient::read_batch_mixed` is: existing trait consumers stay
    /// numeric-only.
    pub async fn write_batch_mixed(
        &mut self,
        requests: &[crate::types::BatchWriteRequest],
    ) -> Result<Vec<WriteResult>, PlcWriteError> {
        if self.inner.is_none() {
            return Err(PlcWriteError::NotConnected);
        }

        let outcome = plan_slmp_write_batch(requests, self.config.word_order);
        let client = self
            .inner
            .as_mut()
            .expect("checked Some above, only cleared on the fatal branch below");

        match execute_slmp_writes(client, &outcome, requests.len()).await {
            Ok(results) => Ok(results),
            Err(err) => {
                self.inner = None;
                Err(err)
            }
        }
    }
}

impl PlcWriteClient for SlmpWriteClient {
    fn connect(&mut self) -> BoxFuture<'_, Result<(), PlcWriteError>> {
        Box::pin(async move {
            // Replace any previous session, same as the read client.
            if let Some(previous) = self.inner.take() {
                previous.close().await;
            }

            self.inner = Some(dial_slmp(&self.config).await.map_err(dial_error_to_write)?);
            Ok(())
        })
    }

    fn write_batch<'a>(
        &'a mut self,
        requests: &'a [WriteRequest],
    ) -> BoxFuture<'a, Result<Vec<WriteResult>, PlcWriteError>> {
        Box::pin(async move {
            if self.inner.is_none() {
                return Err(PlcWriteError::NotConnected);
            }

            let outcome = plan_slmp_writes(requests, self.config.word_order);

            // Thin wrapper over the shared execute function: run it on our own
            // socket, and on a fatal error drop the session so the next call is
            // NotConnected (the broker does the equivalent for its own socket).
            let client = self
                .inner
                .as_mut()
                .expect("checked Some above, only cleared on the fatal branch below");

            match execute_slmp_writes(client, &outcome, requests.len()).await {
                Ok(results) => Ok(results),
                Err(err) => {
                    self.inner = None;
                    Err(err)
                }
            }
        })
    }

    fn disconnect(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            if let Some(client) = self.inner.take() {
                // Real shutdown, not just a drop: a CPU has a finite number of
                // SLMP sessions and a half-open one ties up a slot. The whole
                // broker design (W3) exists to economize on those sessions, so
                // leaking one here would be doubly wrong.
                client.close().await;
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use banto_plc::{Address, DataType, TagValue};

    /// The tripwire for [`device_to_wire`]: this crate's device table and the
    /// wrapped crate's must agree on every device's *actual wire code*, not
    /// merely map to a same-named variant. Same check as `banto-plc`'s
    /// `slmp_device_wire_codes_match_the_wrapped_crate`, repeated here because
    /// this crate carries its own copy of the mapping.
    #[test]
    fn device_wire_codes_match_the_wrapped_crate() {
        let expected: &[(&str, u8)] = &[
            ("X", 0x9C),
            ("Y", 0x9D),
            ("M", 0x90),
            ("L", 0x92),
            ("F", 0x93),
            ("V", 0x94),
            ("B", 0xA0),
            ("D", 0xA8),
            ("W", 0xB4),
            ("S", 0x98),
            ("Z", 0xCC),
            ("R", 0xAF),
            ("ZR", 0xB0),
            ("TS", 0xC1),
            ("TC", 0xC0),
            ("TN", 0xC2),
            ("SS", 0xC7),
            ("SC", 0xC6),
            ("SN", 0xC8),
            ("CS", 0xC4),
            ("CC", 0xC3),
            ("CN", 0xC5),
            ("SB", 0xA1),
            ("SD", 0xA9),
            ("SM", 0x91),
            ("SW", 0xB5),
            ("DX", 0xA2),
            ("DY", 0xA3),
        ];
        assert_eq!(expected.len(), 28);

        for (mnemonic, code) in expected {
            let device = *SlmpDevice::all()
                .iter()
                .find(|d| d.mnemonic() == *mnemonic)
                .unwrap_or_else(|| panic!("{mnemonic} should be a known device"));
            assert_eq!(
                device_to_wire(device).to_code(),
                *code,
                "{mnemonic} should serialize as wire code 0x{code:02X}"
            );
        }
    }

    /// Same mapping table as `banto-plc`'s `classify_slmp_error_splits_fatal_from_per_request`,
    /// returning [`PlcWriteError`] instead - a plain structural `match`, no
    /// message-text parsing (H9, docs/h9-slmp-structured-error-spec.md).
    #[test]
    fn classify_slmp_error_splits_fatal_from_per_request() {
        let end_code = classify_slmp_error(slmp::SlmpError::Device { end_code: 0xC061 });
        assert_eq!(
            end_code,
            PlcWriteError::SlmpEndCode {
                code: 0xC061,
                message: slmp::end_code_name(0xC061).to_string()
            }
        );
        assert!(!end_code.is_connection_fatal());

        let framing = classify_slmp_error(slmp::SlmpError::Framing(
            slmp::FramingError::LengthMismatch {
                declared: 4,
                actual: 2,
            },
        ));
        assert!(matches!(framing, PlcWriteError::Protocol(_)));
        assert!(framing.is_connection_fatal());

        assert!(classify_slmp_error(slmp::SlmpError::NotConnected).is_connection_fatal());
        assert!(classify_slmp_error(slmp::SlmpError::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "boom"
        )))
        .is_connection_fatal());

        assert_eq!(
            classify_slmp_error(slmp::SlmpError::Timeout),
            PlcWriteError::ResponseTimeout
        );
    }

    #[tokio::test]
    async fn write_batch_before_connect_is_not_connected() {
        let mut client = SlmpWriteClient::new(SlmpConfig {
            host: "127.0.0.1".to_string(),
            ..Default::default()
        });
        let requests = [WriteRequest {
            address: Address::parse_slmp("D0").unwrap(),
            data_type: DataType::U16,
            value: TagValue::F64(1.0),
        }];
        assert!(matches!(
            client.write_batch(&requests).await,
            Err(PlcWriteError::NotConnected)
        ));
    }

    #[tokio::test]
    async fn disconnect_on_a_never_connected_client_is_a_no_op() {
        let mut client = SlmpWriteClient::new(SlmpConfig::default());
        client.disconnect().await; // must not panic
        assert!(client.inner.is_none());
    }
}
