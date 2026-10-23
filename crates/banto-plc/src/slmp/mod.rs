//! The MELSEC MC protocol / SLMP side of this crate (I2a): the device table
//! ([`address`]), the read planner ([`planning`]), the in-process
//! [`simulator`], and the small pieces shared with the SLMP driver
//! (`banto-driver-slmp`): [`SlmpCpu`], [`SlmpConfig`] (its defaults and the
//! wire properties the `slmp` crate wants), [`SlmpDevice::to_wire`] and
//! [`classify_slmp`] / [`SlmpErrorClass`].
//!
//! R2b-3d-join (docs/banto-hub-driver-io-design.md §6.2): the I/O that used to
//! live here - `SlmpClient`, `dial_slmp`, `execute_slmp_reads` and
//! `execute_slmp_batch_reads` - is gone. The SLMP driver now builds the 4E
//! frames itself (`banto-driver-slmp`'s `frame.rs`, checked byte for byte
//! against the `slmp` crate by its `slmp_parity.rs`) and `banto-driver-engine`
//! owns the socket. What remains of the `slmp` crate in this one is the wire
//! device code and the connection properties, which that parity test uses as
//! its reference.
//!
//! ## Why this one wraps a crate when `modbus/` is hand-written
//!
//! Modbus TCP's wire format is a 7-byte header and a 5-byte PDU. SLMP's 4E
//! binary frame carries a 15-byte subheader with a network/PC/IO/area access
//! route, device addressing that is 3 bytes on Q/L and 4 (one of them a fixed
//! pad) on the R series, and bit-unit responses packed two points to a byte.
//! The wrapped crate (`slmp`, chosen and approved before I2a) is the audited
//! reference for those details.
//!
//! ## Where the connection-level/per-request line falls for this protocol
//!
//! [`classify_slmp`] decides it for a structured [`slmp::SlmpError`]:
//!
//! - **Non-zero SLMP end code** (`0xC059` wrong command, `0xCEE1` request too
//!   long, ...), `slmp::SlmpError::Device { end_code }` - not fatal, and the
//!   direct analogue of a Modbus exception response: a complete,
//!   length-consistent frame was received, so the byte stream is still aligned
//!   to a request boundary.
//! - **Anything else** (`Framing`, `Timeout`, `NotConnected`, `Io`) - fatal.
//!
//! H9 (docs/h9-slmp-structured-error-spec.md, 2026-08-12) replaced the old
//! text parsing of the wrapped crate's errors with the git dependency on the
//! owner's fork (`slmp` 0.2.1), which exposes `SlmpError::Device { end_code }`
//! as its own enum variant.

pub mod address;
pub mod planning;
#[cfg(any(test, feature = "simulator"))]
pub mod simulator;

use std::time::Duration;

use crate::decode::WordOrder;
use crate::error::PlcError;

use address::{SlmpAccess, SlmpDevice};

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
    /// broker itself (device mapping happens inside `execute_slmp_reads` (removed) /
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
    /// `execute_slmp_reads` (removed) and `banto_plc_write::execute_slmp_writes` over the
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

/// Read/write-independent classification of a structured [`slmp::SlmpError`].
/// The single source of truth for the SLMP error classification policy (H9,
/// docs/h9-slmp-structured-error-spec.md): both [`PlcError`] (this crate) and
/// `banto_plc_write::PlcWriteError` map this via `From` into their own error
/// vocabulary, so the decision logic that used to be duplicated as two
/// identical 5-arm `match`es now lives in exactly one place. Each variant
/// carries the same meaning `classify_slmp_error`'s arms below used to
/// document individually.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlmpErrorClass {
    /// A complete, length-consistent frame carrying a non-zero end code - the
    /// device refused this one request but answered in full, so the byte
    /// stream is still aligned to a request boundary. Not connection-fatal.
    EndCode { code: u16, message: String },
    /// The response structure itself is corrupt (bad length, bad fixed
    /// field, echo mismatch) - the byte stream may be desynchronized.
    /// Connection-fatal.
    Protocol(String),
    /// The crate raises this for both its send and its receive deadline;
    /// either way the reply we needed did not arrive. Connection-fatal.
    ResponseTimeout,
    /// The crate's own "no stream" guard. Should be unreachable from
    /// `read_batch`/`write_batch` (which check the session first), but if
    /// the two ever disagree, agreeing with the crate is the honest answer.
    /// Connection-fatal.
    NotConnected,
    /// Refused, reset, broken pipe, unexpected EOF, DNS failure, and
    /// anything else transport/IO-shaped. Connection-fatal.
    Connection(String),
}

/// The plain structural `match` that decides [`SlmpErrorClass`] for a
/// structured [`slmp::SlmpError`], unlike the H9-era text-parsing
/// `classify_io_error`/`parse_end_code` this replaced: `slmp` 0.2.0 exposes
/// `Device { end_code }` as its own variant, so there is nothing left to
/// parse.
pub fn classify_slmp(err: slmp::SlmpError) -> SlmpErrorClass {
    match err {
        slmp::SlmpError::Device { end_code } => SlmpErrorClass::EndCode {
            code: end_code,
            message: slmp::end_code_name(end_code).to_string(),
        },
        slmp::SlmpError::Framing(e) => SlmpErrorClass::Protocol(e.to_string()),
        slmp::SlmpError::Timeout => SlmpErrorClass::ResponseTimeout,
        slmp::SlmpError::NotConnected => SlmpErrorClass::NotConnected,
        slmp::SlmpError::Io(e) => SlmpErrorClass::Connection(e.to_string()),
    }
}

impl From<SlmpErrorClass> for PlcError {
    fn from(class: SlmpErrorClass) -> Self {
        match class {
            SlmpErrorClass::EndCode { code, message } => PlcError::SlmpEndCode { code, message },
            SlmpErrorClass::Protocol(msg) => PlcError::Protocol(msg),
            SlmpErrorClass::ResponseTimeout => PlcError::ResponseTimeout,
            SlmpErrorClass::NotConnected => PlcError::NotConnected,
            SlmpErrorClass::Connection(msg) => PlcError::Connection(msg),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Pins down [`classify_slmp`]'s mapping table - the single source the
    /// two crates' `PlcError`/`PlcWriteError` `From<SlmpErrorClass>` impls
    /// both build on (H9, docs/h9-slmp-structured-error-spec.md).
    #[test]
    fn classify_slmp_maps_every_slmp_error_variant() {
        assert_eq!(
            classify_slmp(slmp::SlmpError::Device { end_code: 0xC059 }),
            SlmpErrorClass::EndCode {
                code: 0xC059,
                message: slmp::end_code_name(0xC059).to_string()
            }
        );

        let framing = classify_slmp(slmp::SlmpError::Framing(
            slmp::FramingError::LengthMismatch {
                declared: 4,
                actual: 2,
            },
        ));
        assert!(matches!(framing, SlmpErrorClass::Protocol(_)));

        assert_eq!(
            classify_slmp(slmp::SlmpError::Timeout),
            SlmpErrorClass::ResponseTimeout
        );
        assert_eq!(
            classify_slmp(slmp::SlmpError::NotConnected),
            SlmpErrorClass::NotConnected
        );

        let io = classify_slmp(slmp::SlmpError::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "boom",
        )));
        assert!(matches!(io, SlmpErrorClass::Connection(_)));
    }

    /// The classification table this module's whole error contract rests on -
    /// now a plain structural `match` over `slmp::SlmpError` (H9,
    /// docs/h9-slmp-structured-error-spec.md), no message text anywhere. The
    /// end-to-end proof that the *real* wrapped crate actually produces
    /// `Device` vs `Framing` correctly still lives in `integration_tests.rs`'s
    /// `slmp_end_code_is_bad_not_fatal` /
    /// `a_malformed_frame_is_fatal_even_though_it_shares_a_kind_with_an_end_code`;
    /// this test only pins down [`classify_slmp_error`]'s own mapping table
    /// (i.e. that it still folds [`classify_slmp`]'s result into [`PlcError`]
    /// correctly).
    #[test]
    fn classify_slmp_error_splits_fatal_from_per_request() {
        let classify_for_test = |err| PlcError::from(classify_slmp(err));
        let classified = classify_for_test(slmp::SlmpError::Device { end_code: 0xC059 });
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
        let framing = classify_for_test(slmp::SlmpError::Framing(
            slmp::FramingError::LengthMismatch {
                declared: 4,
                actual: 2,
            },
        ));
        assert!(matches!(framing, PlcError::Protocol(_)));
        assert!(framing.is_connection_fatal());

        assert_eq!(
            classify_for_test(slmp::SlmpError::Timeout),
            PlcError::ResponseTimeout
        );
        assert!(classify_for_test(slmp::SlmpError::Timeout).is_connection_fatal());

        assert_eq!(
            classify_for_test(slmp::SlmpError::NotConnected),
            PlcError::NotConnected
        );
        assert!(classify_for_test(slmp::SlmpError::NotConnected).is_connection_fatal());

        let io = classify_for_test(slmp::SlmpError::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "boom",
        )));
        assert!(matches!(io, PlcError::Connection(_)));
        assert!(io.is_connection_fatal());
    }
}
