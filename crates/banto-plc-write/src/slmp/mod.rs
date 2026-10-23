//! The MELSEC MC / SLMP write side of this crate (I5): the write planner
//! ([`planning`]) and the in-process read+write [`simulator`].
//!
//! R2b-3d-join (docs/banto-hub-driver-io-design.md §6.2): the I/O that used to
//! live here - `SlmpWriteClient`, `execute_slmp_writes` (and with it the
//! bit-in-word read-modify-write-confirm sequence over a borrowed
//! `slmp::SLMPClient`) - is gone. `banto-driver-slmp`'s `FrameDriver` carries
//! out the same plan as a state machine (`write_next`: every ordinary write
//! first, then each read-modify-write as read / write-back / confirm steps),
//! and `banto-driver-engine` owns the socket. This module keeps the planning
//! that driver calls, and this crate's half of the classification of the
//! `slmp` crate's errors.
//!
//! ## Where the connection-fatal / per-request line falls
//!
//! Identical to the read side: only a *non-zero SLMP end code*
//! (`SlmpError::Device { end_code }`) is safe to continue past (the CPU refused
//! this one write but answered with a complete, length-consistent frame, so the
//! byte stream is still aligned). Everything else - `Framing`, `Timeout`,
//! `NotConnected`, `Io` - is fatal. The decision is `banto_plc::classify_slmp`
//! (H9); `impl From<SlmpErrorClass> for PlcWriteError` folds it into this
//! crate's vocabulary.
//!
//! ## Planning never coalesces across a gap
//!
//! A write has a side effect, so [`planning`] merges only exactly-adjacent
//! targets (the read planner tolerates a small gap to save round trips). See
//! its module doc. For the bit-in-word RMW (docs/tag-server-design.md §6.1) the
//! plan is a mask-composed recipe ([`planning::SlmpPlannedBitWrite`]), and every
//! [`planning::BitWriteMapping`] entry's own bit is checked independently
//! against its own requested value by the confirming read - a mismatch is
//! [`crate::error::PlcWriteError::BitWriteVerificationFailed`] for that one
//! request. Ordinary writes run to completion before the first RMW starts.

pub mod planning;

#[cfg(any(test, feature = "simulator"))]
pub mod simulator;

#[cfg(any(test, feature = "simulator"))]
use banto_plc::SlmpDevice;
use banto_plc::SlmpErrorClass;

use crate::error::PlcWriteError;

/// Map [`banto_plc::SlmpDevice`] onto the wrapped crate's `DeviceType`.
///
/// This duplicates `banto-plc`'s own `SlmpDevice::to_wire`, which is
/// `pub(crate)` there and so not reachable from this crate. The duplication is
/// total and one-for-one, and kept honest exactly as the original is: by
/// `device_wire_codes_match_the_wrapped_crate`, which checks every device
/// against the actual byte the crate puts on the wire. Used by the simulator
/// (and that test) only since R2b-3d-join.
#[cfg(any(test, feature = "simulator"))]
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

/// This crate's half of H9's single-sourced classification: `banto_plc`'s
/// [`SlmpErrorClass`] carries the read/write-independent decision
/// (`classify_slmp`), and this `From` impl just folds it into
/// [`PlcWriteError`]'s own vocabulary - the write-side twin of
/// `banto_plc`'s `impl From<SlmpErrorClass> for PlcError`.
impl From<SlmpErrorClass> for PlcWriteError {
    fn from(class: SlmpErrorClass) -> Self {
        match class {
            SlmpErrorClass::EndCode { code, message } => {
                PlcWriteError::SlmpEndCode { code, message }
            }
            SlmpErrorClass::Protocol(msg) => PlcWriteError::Protocol(msg),
            SlmpErrorClass::ResponseTimeout => PlcWriteError::ResponseTimeout,
            SlmpErrorClass::NotConnected => PlcWriteError::NotConnected,
            SlmpErrorClass::Connection(msg) => PlcWriteError::Connection(msg),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let classify_for_test = |err| PlcWriteError::from(banto_plc::classify_slmp(err));
        let end_code = classify_for_test(slmp::SlmpError::Device { end_code: 0xC061 });
        assert_eq!(
            end_code,
            PlcWriteError::SlmpEndCode {
                code: 0xC061,
                message: slmp::end_code_name(0xC061).to_string()
            }
        );
        assert!(!end_code.is_connection_fatal());

        let framing = classify_for_test(slmp::SlmpError::Framing(
            slmp::FramingError::LengthMismatch {
                declared: 4,
                actual: 2,
            },
        ));
        assert!(matches!(framing, PlcWriteError::Protocol(_)));
        assert!(framing.is_connection_fatal());

        assert!(classify_for_test(slmp::SlmpError::NotConnected).is_connection_fatal());
        assert!(classify_for_test(slmp::SlmpError::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "boom"
        )))
        .is_connection_fatal());

        assert_eq!(
            classify_for_test(slmp::SlmpError::Timeout),
            PlcWriteError::ResponseTimeout
        );
    }
}
