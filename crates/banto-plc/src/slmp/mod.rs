//! [`SlmpClient`]: the [`crate::client::PlcClient`] implementation for MELSEC
//! MC protocol / SLMP (I2a), the sibling implementation `modbus/mod.rs`'s
//! module doc and README.md:42 have been reserving space for since I2. MELSEC
//! is the eventual primary target of this product line
//! (docs/recorder-requirements.md §1); Modbus TCP only went first because it
//! is easier to debug against free simulators.
//!
//! ## Why this one wraps a crate when `modbus/` is hand-written
//!
//! Modbus TCP's wire format is a 7-byte header and a 5-byte PDU, and
//! `modbus/frame.rs` implements it in under 200 lines. SLMP is not that: the
//! 4E binary frame carries a 15-byte subheader with a network/PC/IO/area
//! access route, device addressing that is 3 bytes on Q/L and 4 (one of them a
//! fixed pad) on the R series, and bit-unit responses packed two points to a
//! byte, one per nibble. Re-deriving that from the specification would be
//! effort spent reproducing something already written and MIT-licensed, so
//! this module wraps the `slmp` crate (chosen and approved before I2a - see the
//! plan's Context section, where `slmp_client` was rejected for its BSD-3
//! licence and unclear async story) and confines itself to the two things the
//! crate does not do: deciding *what* to read ([`planning`]) and translating
//! *how it failed* into this crate's vocabulary.
//!
//! ## Where the connection-level/per-request line falls for this protocol
//!
//! [`SlmpClient::read_batch`] issues one bulk read per
//! [`planning::SlmpPlannedRead`] group and classifies every failure via
//! [`classify_slmp_error`], which is the whole reason this module needs a
//! translation layer at all: the wrapped crate's `slmp::SlmpError` and this
//! crate's [`PlcError`] are deliberately separate vocabularies (I2a's own,
//! versus I2's - see this crate's `error.rs`), so a MELSEC CPU politely
//! refusing one read and a truncated frame need to land in different
//! [`PlcError`] variants even though both start life as an `Err` from the
//! same `bulk_read` call.
//!
//! - **Non-zero SLMP end code** (`0xC059` wrong command, `0xCEE1` request too
//!   long, ...), `slmp::SlmpError::Device { end_code }` - not fatal, and the
//!   direct analogue of a Modbus exception response. The crate validates the
//!   response frame's declared data length against what actually arrived
//!   *before* it inspects the end code, so reaching `Device` proves a
//!   complete, length-consistent frame was received and nothing is left
//!   unread: the byte stream is still aligned to a request boundary. This
//!   becomes `ReadResult::Bad` for only the requests mapped to that one
//!   group, and the loop moves on.
//! - **Anything else** (`Framing`, `Timeout`, `NotConnected`, `Io`) - fatal.
//!   `read_batch` stops issuing groups, drops the session (`self.inner =
//!   None`), and returns `Err`; the caller must `connect()` again. Per
//!   docs/plan.md I2 §2 this crate does not retry or reconnect on its own -
//!   that loop is I3's.
//!
//! Telling the first case from the second used to mean reading the wrapped
//! crate's error *message text* (crates.io `slmp` 0.1.x reported everything as
//! `std::io::Error`, exposing the end code no other way). H9
//! (docs/h9-slmp-structured-error-spec.md, 2026-08-12) replaced that with the
//! git dependency on the owner's fork (`slmp` 0.2.0, see the workspace
//! `Cargo.toml`), which exposes `SlmpError::Device { end_code }` as its own
//! enum variant - [`classify_slmp_error`] is now a plain structural `match`,
//! no string parsing anywhere in this module. The coupling to the wrapped
//! crate's behavior is still real (it still has to actually validate length
//! before end code, and actually distinguish `Device` from `Framing`), so it
//! is still guarded by a test that drives a real end-code response through
//! the real crate rather than by a unit test over hand-built `SlmpError`
//! values - see `slmp_end_code_is_bad_not_fatal` in `integration_tests.rs`.
//!
//! ## Two deliberate differences from `modbus/mod.rs`
//!
//! 1. **No `tokio::time::timeout` around each group.** The Modbus client wraps
//!    every request in one and treats a fire as connection-fatal precisely
//!    because cancelling `read_exact` mid-frame desynchronizes the stream. Here
//!    the wrapped crate applies its own send/receive deadlines internally
//!    (`set_send_timeout`/`set_recv_timeout`, both wired to
//!    [`SlmpConfig::response_timeout`]), so the timeout fires *inside* the
//!    crate and surfaces as an ordinary error return. Adding a second,
//!    outer timeout would reintroduce exactly the mid-read cancellation the
//!    inner one exists to avoid.
//! 2. **No `TCP_NODELAY`.** The wrapped crate owns its `TcpStream` and neither
//!    exposes it nor disables Nagle's algorithm. Since this client also waits
//!    for each reply before sending the next request, Nagle can only add
//!    latency here, never save a packet - the same argument that makes
//!    `modbus/mod.rs` set it. It is simply not reachable through this crate's
//!    API, and it is a real (if small) headwind against the 100ms-cycle target
//!    in recorder-requirements.md §3.1. Worth measuring on real hardware
//!    (docs/plan.md W5's 実機検証) before deciding whether it justifies a
//!    patch upstream.

pub mod address;
#[cfg(test)]
mod integration_tests;
pub mod planning;
#[cfg(any(test, feature = "simulator"))]
pub mod simulator;

use std::time::Duration;

use crate::client::{BoxFuture, PlcClient};
use crate::decode::{decode_register_bit, decode_register_value, decode_string_value, WordOrder};
use crate::error::PlcError;
use crate::types::{BatchReadRequest, BatchReadResult, PlcValue, ReadRequest, ReadResult};

use address::{SlmpAccess, SlmpDevice};
use planning::{plan_slmp_batch, plan_slmp_requests, ReadKind, SlmpPlanOutcome, SlmpPlannedRead};

/// MELSEC CPU series, which SLMP frames differ by: Q and L serialize a device
/// address in 4 bytes and use subcommand `0x0000`/`0x0001`, R uses 6 bytes and
/// `0x0002`/`0x0003`. Mirrors the wrapped crate's `CPU` enum, and re-declared
/// here for the same reason [`SlmpDevice`] is (see `address.rs`'s module doc):
/// it appears in [`SlmpConfig`], which is public API this crate should be able
/// to keep stable across a dependency bump.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SlmpCpu {
    /// Q series (and QnA-compatible), 4-byte device addressing.
    Q,
    /// R series (iQ-R), 6-byte device addressing. The current-generation
    /// default, hence [`Default`].
    #[default]
    R,
    /// L series, frame-compatible with Q.
    L,
}

impl SlmpCpu {
    /// Map onto the wrapped crate's `CPU`. `pub` (was `pub(crate)`) so the W3
    /// broker in relay-wright can build a bare `slmp::SLMPClient` from an
    /// [`SlmpConfig`] via [`SlmpConfig::to_wire_props`] without re-deriving the
    /// mapping (which is what `banto-plc-write` had to do while this was
    /// private - a duplication a later cleanup can now remove).
    pub fn to_wire(self) -> slmp::CPU {
        match self {
            SlmpCpu::Q => slmp::CPU::Q,
            SlmpCpu::R => slmp::CPU::R,
            SlmpCpu::L => slmp::CPU::L,
        }
    }
}

impl SlmpDevice {
    /// Map onto the wrapped crate's device enum.
    ///
    /// Lives here rather than next to the rest of [`SlmpDevice`] in
    /// `address.rs` on purpose: that module is kept free of any `slmp`-crate
    /// reference so the address vocabulary stays pure (Rust is happy to have
    /// one type's inherent impls split across modules of the same crate). The
    /// mapping is total and one-for-one; `slmp_device_wire_codes_match_the_wrapped_crate`
    /// proves it agrees with the crate on every device's actual wire code, so a
    /// mis-typed arm here cannot silently read the wrong device.
    ///
    /// `pub` (was `pub(crate)`) alongside [`SlmpCpu::to_wire`] and
    /// [`SlmpConfig::to_wire_props`] so a later cleanup can drop
    /// `banto-plc-write`'s duplicated copy of this table. Not used by the W3
    /// broker itself (device mapping happens inside [`execute_slmp_reads`] /
    /// `banto_plc_write::execute_slmp_writes`), only exposed for that cleanup.
    pub fn to_wire(self) -> slmp::DeviceType {
        use slmp::DeviceType as W;
        match self {
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
}

/// Everything needed to reach and speak to one MELSEC CPU over SLMP.
///
/// The `network_id`/`pc_id`/`io_id`/`area_id` quartet is SLMP's "access route",
/// which identifies the target station within a MELSEC network. The defaults
/// below are the values that address *the CPU on the far end of this very
/// socket* - the only case v1 needs - and are the same ones the wrapped crate's
/// own examples use. Routing to a station behind the connected one is a real
/// MELSEC capability, deliberately left as configuration rather than modelled,
/// because nothing in recorder-requirements.md asks for it and a wrong value
/// here fails loudly (the CPU rejects the frame) rather than silently.
/// `PartialEq` (T7-1, docs/tag-server-design.md §4.3): same reasoning as
/// [`crate::modbus::ModbusTcpConfig`]'s derive - lets `banto-collect` diff a
/// connection's config across a config reload. No `f64` field here either,
/// so the derived structural comparison is exact.
#[derive(Debug, Clone, PartialEq)]
pub struct SlmpConfig {
    pub host: String,
    /// No universal default exists: SLMP's port is whatever the Ethernet
    /// module's parameters assign it. 5007 is the wrapped crate's example value
    /// and a common choice for the binary 4E frame, but this is expected to
    /// come from `banto-tags::PlcConnection::port` in practice, not from here.
    pub port: u16,
    pub cpu: SlmpCpu,
    /// Default 3s, matching [`crate::modbus::ModbusTcpConfig`] (docs/plan.md
    /// I2 §2).
    ///
    /// Caveat: the wrapped crate hardcodes its own 1s ceiling on the
    /// underlying `TcpStream::connect`, so a value above 1s is effectively
    /// capped at 1s and this setting can only ever *shorten* the deadline. The
    /// timeout is still applied here so a deliberately sub-second value works,
    /// and so the resulting error is this crate's
    /// [`PlcError::ConnectTimeout`] with the address in it either way.
    pub connect_timeout: Duration,
    /// Default 1s (docs/plan.md I2 §2). Wired into the wrapped crate's own
    /// send *and* receive deadlines - see this module's doc comment for why
    /// there is no second, outer timeout.
    pub response_timeout: Duration,
    /// Default [`WordOrder::LowHigh`] - note this differs from
    /// [`crate::modbus::ModbusTcpConfig`]'s [`WordOrder::HighLow`], and the
    /// difference is not an oversight. MELSEC stores a 32-bit value with its
    /// *low* word in the lower-numbered device (`D0` low, `D1` high), which is
    /// the opposite of the Modbus/IEEE convention. Getting this backwards does
    /// not fail, it silently returns byte-swapped numbers, so the default is
    /// set per protocol rather than shared.
    pub word_order: WordOrder,
    /// SLMP access route: network number of the target station. `0` = the
    /// station on the other end of this connection.
    pub network_id: u8,
    /// SLMP access route: requesting station number. `0xFF` = "this PC".
    pub pc_id: u8,
    /// SLMP access route: target module I/O number. `0x03FF` = the CPU itself.
    pub io_id: u16,
    /// SLMP access route: target multi-drop station. `0` when there is none.
    pub area_id: u8,
    /// Echoed back by the CPU in every response and checked by the wrapped
    /// crate, so it doubles as a cheap "is this reply mine" guard. Constant per
    /// connection (unlike Modbus's transaction id, which this client
    /// increments) because this client only ever has one request in flight.
    pub serial_id: u16,
    /// The CPU's own monitoring timer, in units of 250ms - how long the CPU
    /// waits for the operation it was asked to perform. `0x0010` = 16 units =
    /// 4s, comfortably longer than [`Self::response_timeout`] so that the
    /// deadline that fires first is *ours*, keeping the failure mode a clean
    /// client-side timeout rather than a CPU-side one whose partial state we
    /// would have to reason about.
    pub cpu_timer: u16,
}

impl Default for SlmpConfig {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 5007,
            cpu: SlmpCpu::default(),
            connect_timeout: Duration::from_secs(3),
            response_timeout: Duration::from_secs(1),
            word_order: WordOrder::LowHigh,
            network_id: 0x00,
            pc_id: 0xFF,
            io_id: 0x03FF,
            area_id: 0x00,
            serial_id: 0x0001,
            cpu_timer: 0x0010,
        }
    }
}

impl SlmpConfig {
    /// Build the wrapped crate's connection props. `pub` (was private) because
    /// the W3 broker (relay-wright) owns a bare `slmp::SLMPClient` per CPU and
    /// needs to construct it from an [`SlmpConfig`] to drive both
    /// [`execute_slmp_reads`] and `banto_plc_write::execute_slmp_writes` over the
    /// one shared session - the whole point of the broker. Exposing the assembly
    /// here (rather than each caller re-deriving it, as `banto-plc-write` still
    /// does internally) keeps the single source of truth in this crate.
    pub fn to_wire_props(&self) -> slmp::SLMP4EConnectionProps {
        slmp::SLMP4EConnectionProps {
            ip: self.host.clone(),
            port: self.port,
            cpu: self.cpu.to_wire(),
            serial_id: self.serial_id,
            network_id: self.network_id,
            pc_id: self.pc_id,
            io_id: self.io_id,
            area_id: self.area_id,
            cpu_timer: self.cpu_timer,
        }
    }
}

/// Translate the wrapped crate's structured [`slmp::SlmpError`] into this
/// crate's [`PlcError`], which is what decides connection-fatal vs per-request
/// `Bad` (see [`PlcError::is_connection_fatal`] and this module's doc
/// comment). A plain structural `match`, unlike the H9-era text-parsing
/// `classify_io_error`/`parse_end_code` this replaced: `slmp` 0.2.0 exposes
/// `Device { end_code }` as its own variant, so there is nothing left to
/// parse.
fn classify_slmp_error(err: slmp::SlmpError) -> PlcError {
    match err {
        // A complete, length-consistent frame carrying a non-zero end code -
        // the device refused this one request but answered in full, so the
        // byte stream is still aligned to a request boundary.
        slmp::SlmpError::Device { end_code } => PlcError::SlmpEndCode {
            code: end_code,
            message: slmp::end_code_name(end_code).to_string(),
        },
        // The response structure itself is corrupt (bad length, bad fixed
        // field, echo mismatch) - the byte stream may be desynchronized.
        slmp::SlmpError::Framing(e) => PlcError::Protocol(e.to_string()),
        // The crate raises this for both its send and its receive deadline;
        // either way the reply we needed did not arrive.
        slmp::SlmpError::Timeout => PlcError::ResponseTimeout,
        // The crate's own "no stream" guard. Should be unreachable from
        // `read_batch` (which checks `self.inner` first), but if the two ever
        // disagree, agreeing with the crate is the honest answer.
        slmp::SlmpError::NotConnected => PlcError::NotConnected,
        // Refused, reset, broken pipe, unexpected EOF, DNS failure, and
        // anything else transport/IO-shaped.
        slmp::SlmpError::Io(e) => PlcError::Connection(e.to_string()),
    }
}

/// Dial a fresh SLMP session against `config`: build the wrapped crate's
/// client, wire [`SlmpConfig::response_timeout`] into its send/receive
/// deadlines, then race `connect()` against [`SlmpConfig::connect_timeout`]
/// and map a failure onto [`PlcError`].
///
/// The single shared implementation of the SLMP connect sequence (H9
/// transport 共通化, docs/improvement-plan.md §H9): this crate's
/// [`SlmpClient::connect`], `banto_plc_write::slmp::SlmpWriteClient::connect`,
/// and `banto_broker`'s `connect_attempt` used to hand-roll the identical
/// four steps independently (build the wire client, wire both timeouts, race
/// `connect()` against the outer deadline, map `slmp::SlmpError` to this
/// crate's error type) - this is now the one place that sequence is written,
/// and the three callers each fold its result into their own shape (owned
/// `Option<inner>` for the two client wrappers, a bare returned client for
/// the broker). `classify_slmp_error` above is deliberately not reused here:
/// it maps *request*-time failures where a non-zero end code is a per-request
/// `Bad`, but a `connect()` failure is always connection-fatal, so this
/// function keeps its own two-arm mapping (`Timeout` → [`PlcError::ConnectTimeout`],
/// everything else → [`PlcError::Connection`]) exactly as all three call
/// sites already agreed on.
pub async fn dial_slmp(config: &SlmpConfig) -> Result<slmp::SLMPClient, PlcError> {
    let addr = format!("{}:{}", config.host, config.port);
    let mut client = slmp::SLMPClient::new(config.to_wire_props());
    client.set_send_timeout(config.response_timeout);
    client.set_recv_timeout(config.response_timeout);

    tokio::time::timeout(config.connect_timeout, client.connect())
        .await
        .map_err(|_| PlcError::ConnectTimeout(addr.clone()))?
        .map_err(|e| match e {
            // The crate's own hardcoded 1s connect ceiling (see
            // `SlmpConfig::connect_timeout`) surfaces here rather than as our
            // outer timeout elapsing, so it has to be mapped to the same
            // error - otherwise the same failure would be reported two
            // different ways depending on which deadline happened to be
            // shorter.
            slmp::SlmpError::Timeout => PlcError::ConnectTimeout(addr.clone()),
            other => PlcError::Connection(other.to_string()),
        })?;

    Ok(client)
}

/// The [`crate::client::PlcClient`] implementation for MELSEC MC/SLMP. One
/// instance per PLC connection - not `Clone`, not internally reconnecting (see
/// this module's doc comment and docs/plan.md I2 §2).
pub struct SlmpClient {
    config: SlmpConfig,
    /// `Some` exactly while connected. The wrapped crate keeps its own
    /// `Option<TcpStream>` but does *not* clear it when a read fails, so this
    /// is what actually enforces "after a connection-level failure, every call
    /// returns `NotConnected` until `connect()`" - the same contract
    /// `modbus/mod.rs` gets from `Option<TcpStream>`.
    inner: Option<slmp::SLMPClient>,
}

impl SlmpClient {
    pub fn new(config: SlmpConfig) -> Self {
        Self {
            config,
            inner: None,
        }
    }

    /// Read a mixed numeric + string batch in one call (S1 文字列タグ) - the
    /// owned-socket form of [`plan_slmp_batch`] + [`execute_slmp_batch_reads`],
    /// with exactly [`PlcClient::read_batch`]'s connection semantics: `Err`
    /// only for connection-fatal failures (which drop the session, so the next
    /// call is `NotConnected`), per-request `Bad` for everything else. An
    /// inherent method rather than part of the [`PlcClient`] trait: the trait's
    /// consumers (banto-collect) are numeric-only by design and must stay
    /// unable to request a string read.
    pub async fn read_batch_mixed(
        &mut self,
        requests: &[BatchReadRequest],
    ) -> Result<Vec<BatchReadResult>, PlcError> {
        if self.inner.is_none() {
            return Err(PlcError::NotConnected);
        }

        let outcome = plan_slmp_batch(requests);
        let word_order = self.config.word_order;
        let client = self
            .inner
            .as_mut()
            .expect("checked Some above, only cleared on the fatal branch below");

        match execute_slmp_batch_reads(client, &outcome, requests.len(), word_order).await {
            Ok(results) => Ok(results),
            Err(err) => {
                self.inner = None;
                Err(err)
            }
        }
    }
}

/// Issue one bulk read for `group` and return its decoded window.
///
/// `Err(PlcError::SlmpEndCode { .. })` is the only non-fatal outcome (see
/// this module's doc comment); every other `Err` means the session is no
/// longer trustworthy. A free function (rather than a `SlmpClient` method) so
/// both the owned-socket [`SlmpClient::read_batch`] and the borrowed-socket
/// [`execute_slmp_reads`] share the one per-group read implementation and
/// cannot drift.
async fn execute_one(
    client: &mut slmp::SLMPClient,
    group: &SlmpPlannedRead,
) -> Result<GroupValues, PlcError> {
    let start = slmp::Device {
        device_type: group.device.to_wire(),
        address: group.start as usize,
    };
    let expected = group.count as usize;

    // Bit devices take a bit-unit bulk read (SLMP subcommand
    // `0x0001`/`0x0003`, two points per response byte), word devices a
    // word-unit one. `slmp::DataType::U16` is used for *every* word group
    // regardless of the tags' own types on purpose: it makes the crate hand
    // back the raw register window, which `decode.rs::decode_register_value`
    // then interprets - so the 32-bit word-order handling is shared with
    // Modbus rather than reimplemented, and one group can serve a mix of
    // i16/u32/f32 tags in a single round trip. Letting the crate do the
    // typing would need one request per data type.
    let data = match group.device.access() {
        SlmpAccess::Bit => {
            client
                .bulk_read(start, expected, slmp::DataType::Bool)
                .await
        }
        SlmpAccess::Word => client.bulk_read(start, expected, slmp::DataType::U16).await,
    }
    .map_err(classify_slmp_error)?;

    // A response carrying fewer (or more) points than were asked for means
    // the crate's frame accounting and ours disagree - the stream is not
    // where we think it is, so this is fatal, not a per-tag problem.
    if data.len() != expected {
        return Err(PlcError::Protocol(format!(
            "SLMP bulk read of {}{} returned {} point(s), expected {expected}",
            group.device,
            group.start,
            data.len()
        )));
    }

    match group.device.access() {
        SlmpAccess::Bit => {
            let mut bits = Vec::with_capacity(expected);
            for d in &data {
                match d.data {
                    slmp::TypedData::Bool(b) => bits.push(b),
                    other => {
                        return Err(PlcError::Protocol(format!(
                            "SLMP bit read returned a non-bool point: {other:?}"
                        )))
                    }
                }
            }
            Ok(GroupValues::Bits(bits))
        }
        SlmpAccess::Word => {
            let mut words = Vec::with_capacity(expected);
            for d in &data {
                match d.data {
                    slmp::TypedData::U16(w) => words.push(w),
                    other => {
                        return Err(PlcError::Protocol(format!(
                            "SLMP word read returned a non-u16 point: {other:?}"
                        )))
                    }
                }
            }
            Ok(GroupValues::Words(words))
        }
    }
}

/// Execute a planned batch of reads on a **borrowed** `slmp::SLMPClient`, the
/// reusable core the W3 broker calls directly on its shared per-CPU session -
/// the read twin of `banto_plc_write::execute_slmp_writes`. Fills
/// `outcome.immediate_bad` in by index, issues one bulk read per group in
/// `outcome.reads`, and returns a `Vec<ReadResult>` of length `total_requests`
/// in original request order.
///
/// `word_order` is the one shape difference from the write twin: a write bakes
/// the 32-bit word order into its payload at plan time
/// (`plan_slmp_writes(requests, word_order)`), so `execute_slmp_writes` needs no
/// such argument; a read *decodes* the fetched register window here, after the
/// wire round trip, so the order has to arrive with the call. `plan_slmp_requests`
/// (unlike its write sibling) is therefore pure of it.
///
/// `Err` is reserved for a connection-fatal failure (the caller must drop the
/// session and reconnect); a device-side end code becomes a per-request `Bad`
/// for that group's requests and the loop continues. Does not own or reconnect
/// the socket - lifecycle is the caller's ([`SlmpClient`] for the standalone
/// form, the broker for the shared one).
pub async fn execute_slmp_reads(
    client: &mut slmp::SLMPClient,
    outcome: &SlmpPlanOutcome,
    total_requests: usize,
    word_order: WordOrder,
) -> Result<Vec<ReadResult>, PlcError> {
    // Delegate to the string-capable executor and narrow the results back to
    // the numeric-only shape. A `Str` value cannot occur for an outcome built
    // by `plan_slmp_requests` (its input type cannot express a string); if a
    // caller hands this legacy entry point a *batch*-planned outcome anyway,
    // the string becomes a per-request `Bad` rather than a panic - the value
    // simply does not fit this function's return type.
    let results = execute_slmp_batch_reads(client, outcome, total_requests, word_order).await?;
    Ok(results
        .into_iter()
        .map(|r| match r {
            BatchReadResult::Value(value) => match value.as_tag_value() {
                Some(v) => ReadResult::Value(v),
                None => ReadResult::Bad(PlcError::Protocol(
                    "文字列読み出しは execute_slmp_batch_reads を使ってください".to_string(),
                )),
            },
            BatchReadResult::Bad(e) => ReadResult::Bad(e),
        })
        .collect())
}

/// The string-capable twin of [`execute_slmp_reads`], executing an outcome of
/// [`plan_slmp_batch`] (numeric and string spans mixed in the same groups) on
/// a **borrowed** `slmp::SLMPClient` - the entry point the S2 broker uses for
/// mixed batches. Identical contract: `Err` is connection-fatal only, a
/// device-side end code becomes a per-request `Bad` for its group, and every
/// input index is answered exactly once, in original request order.
///
/// String spans are fetched exactly like numeric ones - the group is one raw
/// `u16` window on the wire - and only the scatter step differs: a
/// [`ReadKind::Str`] span goes through `decode.rs::decode_string_value`
/// (low-byte-first per word, Shift-JIS, NUL-trimmed) into
/// [`PlcValue::Str`]. `word_order` applies to 32-bit *numeric* decoding only;
/// a string's byte order is fixed by MELSEC's storage convention, not
/// configurable per device family.
pub async fn execute_slmp_batch_reads(
    client: &mut slmp::SLMPClient,
    outcome: &SlmpPlanOutcome,
    total_requests: usize,
    word_order: WordOrder,
) -> Result<Vec<BatchReadResult>, PlcError> {
    let mut results: Vec<Option<BatchReadResult>> = vec![None; total_requests];
    for (index, reason) in &outcome.immediate_bad {
        results[*index] = Some(BatchReadResult::Bad(reason.clone()));
    }

    for group in &outcome.reads {
        match execute_one(client, group).await {
            Ok(values) => {
                for m in &group.mapping {
                    let value = match &values {
                        GroupValues::Bits(bits) => PlcValue::Bit(bits[m.offset_in_read as usize]),
                        GroupValues::Words(words) => {
                            let decoded = match m.kind {
                                ReadKind::Numeric(data_type) => decode_register_value(
                                    words,
                                    m.offset_in_read as usize,
                                    data_type,
                                    word_order,
                                )
                                .map(PlcValue::from),
                                ReadKind::Str { words: span } => decode_string_value(
                                    words,
                                    m.offset_in_read as usize,
                                    span as usize,
                                )
                                .map(PlcValue::Str),
                                // T8 (docs/tag-server-design.md §6.1): one
                                // bit out of the fetched word, not the whole
                                // word as a number.
                                ReadKind::BitInWord { bit } => {
                                    decode_register_bit(words, m.offset_in_read as usize, bit)
                                        .map(PlcValue::from)
                                }
                            };
                            match decoded {
                                Ok(v) => v,
                                Err(e) => {
                                    results[m.request_index] = Some(BatchReadResult::Bad(e));
                                    continue;
                                }
                            }
                        }
                    };
                    results[m.request_index] = Some(BatchReadResult::Value(value));
                }
            }
            Err(err) if !err.is_connection_fatal() => {
                // SLMP end code: the CPU refused this one group but answered in
                // full, so only these requests are bad.
                for m in &group.mapping {
                    results[m.request_index] = Some(BatchReadResult::Bad(err.clone()));
                }
            }
            Err(err) => {
                // Connection-fatal: hand the error up. Dropping/reconnecting the
                // session is the caller's job.
                return Err(err);
            }
        }
    }

    Ok(results
        .into_iter()
        .enumerate()
        .map(|(i, r)| {
            r.unwrap_or_else(|| {
                panic!("plan_slmp_batch must account for every input index, missing {i}")
            })
        })
        .collect())
}

enum GroupValues {
    Bits(Vec<bool>),
    Words(Vec<u16>),
}

impl PlcClient for SlmpClient {
    fn connect(&mut self) -> BoxFuture<'_, Result<(), PlcError>> {
        Box::pin(async move {
            // Drop any previous session first, so a redundant `connect()`
            // replaces it rather than leaking a socket - same
            // implementation-defined-but-reconnects behaviour as
            // `ModbusTcpClient` (see `client.rs`'s note on this).
            if let Some(previous) = self.inner.take() {
                previous.close().await;
            }

            self.inner = Some(dial_slmp(&self.config).await?);
            Ok(())
        })
    }

    fn read_batch<'a>(
        &'a mut self,
        requests: &'a [ReadRequest],
    ) -> BoxFuture<'a, Result<Vec<ReadResult>, PlcError>> {
        Box::pin(async move {
            if self.inner.is_none() {
                return Err(PlcError::NotConnected);
            }

            let outcome = plan_slmp_requests(requests);
            let word_order = self.config.word_order;

            // Thin wrapper over the shared execute function (the read twin of
            // `SlmpWriteClient::write_batch`): run it on our own socket, and on
            // a fatal error drop the session so the next call is `NotConnected`.
            // The broker does the equivalent for its own borrowed socket, so the
            // owned and borrowed paths cannot drift.
            let client = self
                .inner
                .as_mut()
                .expect("checked Some above, only cleared on the fatal branch below");

            match execute_slmp_reads(client, &outcome, requests.len(), word_order).await {
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
                // Unlike `modbus/mod.rs`, which just drops its `TcpStream`,
                // the wrapped crate offers a real shutdown - worth using, since
                // a CPU has a finite number of SLMP sessions and a
                // half-open one ties up a slot until it times out. That session
                // ceiling is also an open question for I5 (read and write
                // clients were planned as separate sessions), which is one more
                // reason not to leak them here.
                client.close().await;
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::address::Address;

    /// The tripwire for [`SlmpDevice::to_wire`]: this crate's device table and
    /// the wrapped crate's must agree on every device's *actual wire code*, not
    /// merely map to a same-named variant. A transposed pair here would read a
    /// plausible-looking wrong device (say `SD` where `SM` was configured) with
    /// no error anywhere, which is the worst failure mode this module has - so
    /// it is checked against the byte the crate would actually put on the wire.
    #[test]
    fn slmp_device_wire_codes_match_the_wrapped_crate() {
        // (mnemonic, wire code) per the MELSEC MC protocol device code table.
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
            let (device, _, _) = address::parse(&format!("{mnemonic}0"))
                .unwrap_or_else(|e| panic!("{mnemonic}0 should parse: {e}"));
            assert_eq!(device.mnemonic(), *mnemonic);
            assert_eq!(
                device.to_wire().to_code(),
                *code,
                "{mnemonic} should serialize as wire code 0x{code:02X}"
            );
        }
    }

    /// MELSEC's low-word-first storage is the reason
    /// [`SlmpConfig::default`] does not simply use [`WordOrder::default`];
    /// stated as a test so a future "tidy up the duplicated default" change
    /// has to argue with it.
    #[test]
    fn default_word_order_is_low_high_unlike_modbus() {
        assert_eq!(SlmpConfig::default().word_order, WordOrder::LowHigh);
        assert_eq!(
            crate::modbus::ModbusTcpConfig::default().word_order,
            WordOrder::HighLow
        );
    }

    #[test]
    fn default_cpu_timer_outlasts_the_default_response_timeout() {
        // cpu_timer is in 250ms units; ours must be the deadline that fires
        // first (see SlmpConfig::cpu_timer's doc comment).
        let config = SlmpConfig::default();
        let cpu_budget = Duration::from_millis(250) * config.cpu_timer as u32;
        assert!(
            cpu_budget > config.response_timeout,
            "cpu_timer ({cpu_budget:?}) should outlast response_timeout ({:?})",
            config.response_timeout
        );
    }

    /// The classification table this module's whole error contract rests on -
    /// now a plain structural `match` over `slmp::SlmpError` (H9,
    /// docs/h9-slmp-structured-error-spec.md), no message text anywhere. The
    /// end-to-end proof that the *real* wrapped crate actually produces
    /// `Device` vs `Framing` correctly still lives in `integration_tests.rs`'s
    /// `slmp_end_code_is_bad_not_fatal` /
    /// `a_malformed_frame_is_fatal_even_though_it_shares_a_kind_with_an_end_code`;
    /// this test only pins down [`classify_slmp_error`]'s own mapping table.
    #[test]
    fn classify_slmp_error_splits_fatal_from_per_request() {
        let classified = classify_slmp_error(slmp::SlmpError::Device { end_code: 0xC059 });
        assert_eq!(
            classified,
            PlcError::SlmpEndCode {
                code: 0xC059,
                message: slmp::end_code_name(0xC059).to_string()
            }
        );
        assert!(
            !classified.is_connection_fatal(),
            "an end code must stay a per-request Bad"
        );

        // Framing must come out fatal even though a text-parsing classifier
        // could once confuse it with a `Device` response sharing the same
        // `io::ErrorKind` - this is the pair H9's structured enum exists to
        // separate unambiguously.
        let framing = classify_slmp_error(slmp::SlmpError::Framing(
            slmp::FramingError::LengthMismatch {
                declared: 4,
                actual: 2,
            },
        ));
        assert!(matches!(framing, PlcError::Protocol(_)));
        assert!(framing.is_connection_fatal());

        assert_eq!(
            classify_slmp_error(slmp::SlmpError::Timeout),
            PlcError::ResponseTimeout
        );
        assert!(classify_slmp_error(slmp::SlmpError::Timeout).is_connection_fatal());

        assert_eq!(
            classify_slmp_error(slmp::SlmpError::NotConnected),
            PlcError::NotConnected
        );
        assert!(classify_slmp_error(slmp::SlmpError::NotConnected).is_connection_fatal());

        let io = classify_slmp_error(slmp::SlmpError::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "boom",
        )));
        assert!(matches!(io, PlcError::Connection(_)));
        assert!(io.is_connection_fatal());
    }

    #[tokio::test]
    async fn read_batch_before_connect_is_not_connected() {
        let mut client = SlmpClient::new(SlmpConfig {
            host: "127.0.0.1".to_string(),
            ..Default::default()
        });
        let requests = [ReadRequest {
            address: Address::parse_slmp("D0").unwrap(),
            data_type: crate::types::DataType::U16,
        }];
        assert!(matches!(
            client.read_batch(&requests).await,
            Err(PlcError::NotConnected)
        ));
    }

    /// Even with nothing connected, a batch of *only* unservable requests must
    /// still fail with `NotConnected` rather than quietly returning all-`Bad` -
    /// `read_batch`'s connection check comes first, matching Modbus.
    #[tokio::test]
    async fn disconnect_on_a_never_connected_client_is_a_no_op() {
        let mut client = SlmpClient::new(SlmpConfig::default());
        client.disconnect().await; // must not panic
        assert!(client.inner.is_none());
    }
}
