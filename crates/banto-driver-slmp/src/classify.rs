//! `PlcError` / `PlcWriteError` -> `banto-driver-api`'s `ItemError` /
//! `FatalError` (docs/banto-driver-api-design.md §3.4, R1-4b).
//!
//! The flat `banto-plc` / `banto-plc-write` enums decide fatality with
//! `is_connection_fatal()` (fatal by default). The API makes the same
//! decision a type: `Ok(ItemError)` costs one tag its reading, `Err(FatalError)`
//! drops the connection. These two functions are the table of §3.4, one
//! `match` arm group per row and **no wildcard arm**: a variant added to either
//! enum stops this crate compiling until someone decides which side it is on
//! (the default stays fatal, so the safe direction is `Err`).
//!
//! Not used on the production path in R1 (the broker still branches on
//! `is_connection_fatal()`); the tests pin the table, including
//! `is_connection_fatal() == false` <=> `Ok(ItemError)`. `banto-driver-modbus`
//! carries an identical copy (design §3.4 puts one in each driver crate).

use banto_driver_api::{DeviceError, FatalError, ItemError};
use banto_plc::PlcError;
use banto_plc_write::PlcWriteError;

/// Classify a read-side error (§3.4).
pub fn classify_read(err: PlcError) -> Result<ItemError, FatalError> {
    match err {
        PlcError::ModbusException {
            function,
            code,
            message,
        } => Ok(ItemError::Device(DeviceError::new(
            u32::from(code),
            format!("FC=0x{function:02x} {message}"),
        ))),
        PlcError::SlmpEndCode { code, message } => Ok(ItemError::Device(DeviceError::new(
            u32::from(code),
            message,
        ))),
        PlcError::UnsupportedCombination { .. }
        | PlcError::AddressProtocolMismatch { .. }
        | PlcError::StringSpanUnsupported { .. }
        // One tag's address cannot be understood: that tag's problem, not the
        // socket's (R2b-2a, address design §6 Q5; `is_connection_fatal` agrees).
        | PlcError::InvalidAddress(_)
        | PlcError::UnsupportedDriver(_) => Ok(ItemError::Unsupported(err.to_string())),
        PlcError::ConnectTimeout(message) | PlcError::Connection(message) => {
            Err(FatalError::Io(message))
        }
        PlcError::NotConnected => Err(FatalError::Io(err.to_string())),
        PlcError::ResponseTimeout => Err(FatalError::Timeout),
        PlcError::Protocol(message) => Err(FatalError::Framing(message)),
    }
}

/// Classify a write-side error (§3.4).
pub fn classify_write(err: PlcWriteError) -> Result<ItemError, FatalError> {
    match err {
        PlcWriteError::ModbusException {
            function,
            code,
            message,
        } => Ok(ItemError::Device(DeviceError::new(
            u32::from(code),
            format!("FC=0x{function:02x} {message}"),
        ))),
        PlcWriteError::SlmpEndCode { code, message } => Ok(ItemError::Device(DeviceError::new(
            u32::from(code),
            message,
        ))),
        PlcWriteError::UnsupportedCombination { .. }
        | PlcWriteError::AddressProtocolMismatch { .. }
        | PlcWriteError::StringSpanUnsupported { .. }
        | PlcWriteError::ValueTypeMismatch { .. }
        | PlcWriteError::ValueOutOfRange { .. }
        | PlcWriteError::ModbusReadOnlyArea { .. }
        | PlcWriteError::UnsupportedRequestKind { .. } => {
            Ok(ItemError::Unsupported(err.to_string()))
        }
        PlcWriteError::ConflictingBitWrite { .. }
        | PlcWriteError::BitWriteVerificationFailed { .. } => {
            Ok(ItemError::Rejected(err.to_string()))
        }
        PlcWriteError::ConnectTimeout(message) | PlcWriteError::Connection(message) => {
            Err(FatalError::Io(message))
        }
        PlcWriteError::NotConnected => Err(FatalError::Io(err.to_string())),
        PlcWriteError::ResponseTimeout => Err(FatalError::Timeout),
        PlcWriteError::Protocol(message) => Err(FatalError::Framing(message)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Where a variant must land, written out per variant independently of the
    /// implementation (and exhaustively, so a new variant fails to compile
    /// here too).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Want {
        Device,
        Unsupported,
        Rejected,
        Io,
        Timeout,
        Framing,
        InvalidConfig,
    }

    fn want_read(err: &PlcError) -> Want {
        match err {
            PlcError::ModbusException { .. } | PlcError::SlmpEndCode { .. } => Want::Device,
            PlcError::UnsupportedCombination { .. }
            | PlcError::AddressProtocolMismatch { .. }
            | PlcError::StringSpanUnsupported { .. }
            | PlcError::InvalidAddress(_)
            | PlcError::UnsupportedDriver(_) => Want::Unsupported,
            PlcError::ConnectTimeout(_) | PlcError::Connection(_) | PlcError::NotConnected => {
                Want::Io
            }
            PlcError::ResponseTimeout => Want::Timeout,
            PlcError::Protocol(_) => Want::Framing,
        }
    }

    fn want_write(err: &PlcWriteError) -> Want {
        match err {
            PlcWriteError::ModbusException { .. } | PlcWriteError::SlmpEndCode { .. } => {
                Want::Device
            }
            PlcWriteError::UnsupportedCombination { .. }
            | PlcWriteError::AddressProtocolMismatch { .. }
            | PlcWriteError::StringSpanUnsupported { .. }
            | PlcWriteError::ValueTypeMismatch { .. }
            | PlcWriteError::ValueOutOfRange { .. }
            | PlcWriteError::ModbusReadOnlyArea { .. }
            | PlcWriteError::UnsupportedRequestKind { .. } => Want::Unsupported,
            PlcWriteError::ConflictingBitWrite { .. }
            | PlcWriteError::BitWriteVerificationFailed { .. } => Want::Rejected,
            PlcWriteError::ConnectTimeout(_)
            | PlcWriteError::Connection(_)
            | PlcWriteError::NotConnected => Want::Io,
            PlcWriteError::ResponseTimeout => Want::Timeout,
            PlcWriteError::Protocol(_) => Want::Framing,
        }
    }

    fn got(result: &Result<ItemError, FatalError>) -> Want {
        match result {
            Ok(ItemError::Device(_)) => Want::Device,
            Ok(ItemError::Unsupported(_)) => Want::Unsupported,
            Ok(ItemError::Rejected(_)) => Want::Rejected,
            Err(FatalError::Io(_)) => Want::Io,
            Err(FatalError::Timeout) => Want::Timeout,
            Err(FatalError::Framing(_)) => Want::Framing,
            Err(FatalError::InvalidConfig(_)) => Want::InvalidConfig,
            // Not produced by any row of the table.
            Ok(_) | Err(_) => panic!("{result:?} is not a row of the §3.4 table"),
        }
    }

    /// One value of every `PlcError` variant (12 - the match in `want_read`
    /// is what keeps this list complete).
    fn every_read_error() -> Vec<PlcError> {
        vec![
            PlcError::ConnectTimeout("3s".into()),
            PlcError::Connection("reset".into()),
            PlcError::NotConnected,
            PlcError::ResponseTimeout,
            PlcError::InvalidAddress("D100".into()),
            PlcError::UnsupportedDriver("opc-ua".into()),
            PlcError::AddressProtocolMismatch {
                expected: "modbus-ref".into(),
                actual: "slmp".into(),
            },
            PlcError::UnsupportedCombination {
                area: "coil".into(),
                data_type: "f32".into(),
            },
            PlcError::StringSpanUnsupported { words: 0, max: 480 },
            PlcError::Protocol("bad mbap".into()),
            PlcError::ModbusException {
                function: 0x03,
                code: 0x02,
                message: "illegal data address".into(),
            },
            PlcError::SlmpEndCode {
                code: 0xC059,
                message: "WrongCommand".into(),
            },
        ]
    }

    fn every_write_error() -> Vec<PlcWriteError> {
        vec![
            PlcWriteError::ConnectTimeout("3s".into()),
            PlcWriteError::Connection("reset".into()),
            PlcWriteError::NotConnected,
            PlcWriteError::ResponseTimeout,
            PlcWriteError::Protocol("bad mbap".into()),
            PlcWriteError::AddressProtocolMismatch {
                expected: "modbus-ref".into(),
                actual: "slmp".into(),
            },
            PlcWriteError::UnsupportedCombination {
                area: "coil".into(),
                data_type: "f32".into(),
            },
            PlcWriteError::ValueTypeMismatch {
                data_type: "u16".into(),
                value_kind: "bit".into(),
            },
            PlcWriteError::ValueOutOfRange {
                data_type: "u16".into(),
                value: "70000".into(),
                detail: "out of range".into(),
            },
            PlcWriteError::StringSpanUnsupported {
                words: 961,
                max: 960,
            },
            PlcWriteError::SlmpEndCode {
                code: 0xC061,
                message: "WrongLength".into(),
            },
            PlcWriteError::ModbusReadOnlyArea {
                area: "input_register".into(),
            },
            PlcWriteError::ModbusException {
                function: 0x10,
                code: 0x04,
                message: "server failure".into(),
            },
            PlcWriteError::ConflictingBitWrite {
                area: "D100".into(),
                bit: 5,
            },
            PlcWriteError::BitWriteVerificationFailed {
                area: "D100".into(),
                bit: 5,
            },
            PlcWriteError::UnsupportedRequestKind {
                kind: "String".into(),
            },
        ]
    }

    /// The lists above name every variant exactly once (a duplicate or a
    /// missing one changes the count of distinct discriminants).
    #[test]
    fn the_lists_cover_each_variant_once() {
        use std::collections::HashSet;
        let reads: HashSet<_> = every_read_error()
            .iter()
            .map(std::mem::discriminant)
            .collect();
        assert_eq!(reads.len(), every_read_error().len());
        assert_eq!(reads.len(), 12);
        let writes: HashSet<_> = every_write_error()
            .iter()
            .map(std::mem::discriminant)
            .collect();
        assert_eq!(writes.len(), every_write_error().len());
        assert_eq!(writes.len(), 16);
    }

    #[test]
    fn read_errors_follow_the_table() {
        for err in every_read_error() {
            let fatal = err.is_connection_fatal();
            let want = want_read(&err);
            let result = classify_read(err.clone());
            assert_eq!(got(&result), want, "{err:?}");
            // `is_connection_fatal() == false` <=> `Ok(ItemError)`.
            assert_eq!(result.is_ok(), !fatal, "{err:?}");
        }
    }

    #[test]
    fn write_errors_follow_the_table() {
        for err in every_write_error() {
            let fatal = err.is_connection_fatal();
            let want = want_write(&err);
            let result = classify_write(err.clone());
            assert_eq!(got(&result), want, "{err:?}");
            assert_eq!(result.is_ok(), !fatal, "{err:?}");
        }
    }

    #[test]
    fn payloads_are_carried_over() {
        // Modbus: code as u32, function code prefixed into the message.
        assert_eq!(
            classify_read(PlcError::ModbusException {
                function: 0x03,
                code: 0x02,
                message: "illegal data address".into(),
            }),
            Ok(ItemError::Device(DeviceError::new(
                2,
                "FC=0x03 illegal data address"
            )))
        );
        assert_eq!(
            classify_write(PlcWriteError::ModbusException {
                function: 0x10,
                code: 0x0B,
                message: "gateway target failed".into(),
            }),
            Ok(ItemError::Device(DeviceError::new(
                11,
                "FC=0x10 gateway target failed"
            )))
        );
        // SLMP: end code as is, message untouched.
        assert_eq!(
            classify_read(PlcError::SlmpEndCode {
                code: 0xC059,
                message: "WrongCommand".into(),
            }),
            Ok(ItemError::Device(DeviceError::new(0xC059, "WrongCommand")))
        );
        assert_eq!(
            classify_write(PlcWriteError::SlmpEndCode {
                code: 0xC061,
                message: "WrongLength".into(),
            }),
            Ok(ItemError::Device(DeviceError::new(0xC061, "WrongLength")))
        );
        // `Unsupported` / `Rejected` carry the original `Display`.
        let e = PlcError::UnsupportedCombination {
            area: "coil".into(),
            data_type: "f32".into(),
        };
        assert_eq!(
            classify_read(e.clone()),
            Ok(ItemError::Unsupported(e.to_string()))
        );
        let e = PlcWriteError::ConflictingBitWrite {
            area: "D100".into(),
            bit: 5,
        };
        assert_eq!(
            classify_write(e.clone()),
            Ok(ItemError::Rejected(e.to_string()))
        );
        // Fatal rows keep the underlying text.
        assert_eq!(
            classify_read(PlcError::Connection("reset".into())),
            Err(FatalError::Io("reset".into()))
        );
        assert_eq!(
            classify_read(PlcError::Protocol("bad mbap".into())),
            Err(FatalError::Framing("bad mbap".into()))
        );
        // R2b-2a (Q5): an unparsable address is one tag's problem, not fatal.
        let e = PlcError::InvalidAddress("zzz".into());
        assert_eq!(
            classify_read(e.clone()),
            Ok(ItemError::Unsupported(e.to_string()))
        );
        assert_eq!(
            classify_write(PlcWriteError::ResponseTimeout),
            Err(FatalError::Timeout)
        );
    }
}
