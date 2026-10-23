//! Modbus TCP write framing: [`build_write_frame`] (one
//! [`ModbusPlannedWrite`] to its MBAP frame) and [`parse_write_response_pdu`]
//! (the echo check), plus [`planning`] (what is written, and what is refused)
//! and the write-capable [`simulator`].
//!
//! Until R2b-3c this module also held `ModbusWriteClient` (the
//! `PlcWriteClient` for Modbus TCP) and
//! `execute_modbus_writes` (a planned batch on a borrowed `TcpStream`). The
//! broker now reaches Modbus through the frame-type driver
//! (`banto-driver-modbus` over `banto-driver-engine`, docs/banto-hub-driver-io-design.md
//! §6.1), which sends one step per planned write with these two functions;
//! the socket, the response timeout and the "a timeout is fatal" rule are the
//! engine's. This module reuses the read side's `pub` framing primitives
//! ([`banto_plc::modbus::frame`]) rather than re-deriving MBAP framing.
//!
//! ## Where the connection-fatal/per-request line falls
//!
//! A Modbus **exception response** (the device says "no" to one write, e.g.
//! illegal data address) is not fatal - the byte stream is still in sync, so
//! only that write is bad ([`crate::error::PlcWriteError::ModbusException`],
//! [`crate::error::PlcWriteError::is_connection_fatal`] is `false`).
//! Everything else (an echo that does not match, an unexpected function
//! code) is connection-fatal.
//!
//! ## Single vs. multiple write function codes
//!
//! A [`crate::modbus::planning::ModbusPlannedWrite`] whose payload has
//! exactly one element uses FC5 (write single coil) / FC6 (write single
//! register); a multi-element group uses FC15 (write multiple coils) / FC16
//! (write multiple registers). The single-write codes are used whenever
//! possible rather than a one-element FC15/FC16, which the wire protocol also
//! permits: a single write's intent is unambiguous straight from its function
//! code, and some older/simpler slave implementations do not support
//! FC15/FC16 at all, while FC5/FC6 support is close to universal.
//!
//! ## `build_request_frame` reuse for FC5/FC6 (see also `banto-plc`'s doc
//! comment on that function)
//!
//! `banto_plc::modbus::frame::build_request_frame(tid, unit_id, function,
//! start_offset, count)` builds a PDU shaped `function + u16 + u16`. A read
//! request (`function + start_offset + count`) and a single-write
//! request/response echo (`function + address + value`) are *exactly* the
//! same byte shape - only the field names differ. [`build_write_frame`]
//! therefore calls it unmodified for FC5/FC6, passing the coil/register
//! value in the position the function signature calls `count`.
//!
//! The word order a 32-bit value is encoded in is the connection's, the same
//! as the read side's (`ModbusTcpConfig::word_order`, default
//! `HighLow`; the driver's column default `low_high` when stored).

pub mod planning;

#[cfg(any(test, feature = "simulator"))]
pub mod simulator;

use banto_plc::modbus::frame::{
    build_request_frame, encode_bits_payload, encode_registers_payload, exception_message,
    wrap_mbap, EXCEPTION_FLAG,
};

use crate::error::PlcWriteError;

use planning::{ModbusPlannedWrite, WritePayload};

const FC_WRITE_SINGLE_COIL: u8 = 0x05;
const FC_WRITE_SINGLE_REGISTER: u8 = 0x06;
const FC_WRITE_MULTIPLE_COILS: u8 = 0x0F;
const FC_WRITE_MULTIPLE_REGISTERS: u8 = 0x10;

/// Build the wire frame for one [`ModbusPlannedWrite`], choosing FC5/FC6
/// (single) or FC15/FC16 (multiple) by its payload length (see this module's
/// doc comment). Returns the function code used (for the response parser)
/// alongside the frame.
pub fn build_write_frame(
    transaction_id: u16,
    unit_id: u8,
    plan: &ModbusPlannedWrite,
) -> (u8, Vec<u8>) {
    match &plan.payload {
        WritePayload::Bits(bits) if bits.len() == 1 => {
            // Modbus coil value convention: 0xFF00 = ON, 0x0000 = OFF (spec
            // §6.5). See this module's doc comment for why calling
            // `build_request_frame` (a "read" helper by name) is exactly
            // right here.
            let value: u16 = if bits[0] { 0xFF00 } else { 0x0000 };
            let frame = build_request_frame(
                transaction_id,
                unit_id,
                FC_WRITE_SINGLE_COIL,
                plan.start_offset,
                value,
            );
            (FC_WRITE_SINGLE_COIL, frame)
        }
        WritePayload::Words(words) if words.len() == 1 => {
            let frame = build_request_frame(
                transaction_id,
                unit_id,
                FC_WRITE_SINGLE_REGISTER,
                plan.start_offset,
                words[0],
            );
            (FC_WRITE_SINGLE_REGISTER, frame)
        }
        WritePayload::Bits(bits) => {
            let packed = encode_bits_payload(bits);
            let mut pdu = Vec::with_capacity(6 + packed.len());
            pdu.push(FC_WRITE_MULTIPLE_COILS);
            pdu.extend_from_slice(&plan.start_offset.to_be_bytes());
            pdu.extend_from_slice(&(bits.len() as u16).to_be_bytes());
            pdu.push(packed.len() as u8);
            pdu.extend_from_slice(&packed);
            (
                FC_WRITE_MULTIPLE_COILS,
                wrap_mbap(transaction_id, unit_id, &pdu),
            )
        }
        WritePayload::Words(words) => {
            let packed = encode_registers_payload(words);
            let mut pdu = Vec::with_capacity(6 + packed.len());
            pdu.push(FC_WRITE_MULTIPLE_REGISTERS);
            pdu.extend_from_slice(&plan.start_offset.to_be_bytes());
            pdu.extend_from_slice(&(words.len() as u16).to_be_bytes());
            pdu.push(packed.len() as u8);
            pdu.extend_from_slice(&packed);
            (
                FC_WRITE_MULTIPLE_REGISTERS,
                wrap_mbap(transaction_id, unit_id, &pdu),
            )
        }
    }
}

/// Parse one write response PDU. Unlike a read response, every successful
/// Modbus write response (FC5/6/15/16 alike) is a plain 5-byte echo -
/// `function + 2 words` - never a `byte_count`-prefixed shape, which is
/// exactly why this crate does not reuse `banto-plc`'s (read-shaped)
/// `parse_response_pdu` (see `banto-plc/src/modbus/frame.rs`'s module doc).
///
/// `expected_echo` is the request PDU's own first 5 bytes (`function` + the
/// two 16-bit fields the caller sent) - a successful response must echo them
/// back verbatim (spec §6.5/6.6/6.11/6.12), so comparing byte-for-byte
/// catches a desynchronized/misbehaving device the same way a transaction id
/// mismatch does, rather than trusting "function code matched" alone.
pub fn parse_write_response_pdu(pdu: &[u8], expected_echo: &[u8; 5]) -> Result<(), PlcWriteError> {
    let expected_function = expected_echo[0];
    let function = *pdu
        .first()
        .ok_or_else(|| PlcWriteError::Protocol("empty response PDU".to_string()))?;

    if function == expected_function | EXCEPTION_FLAG {
        let code = *pdu
            .get(1)
            .ok_or_else(|| PlcWriteError::Protocol("truncated exception response".to_string()))?;
        return Err(PlcWriteError::ModbusException {
            function: expected_function,
            code,
            message: exception_message(code).to_string(),
        });
    }

    if pdu.len() != 5 {
        return Err(PlcWriteError::Protocol(format!(
            "write response PDU has unexpected length: {} (expected 5)",
            pdu.len()
        )));
    }
    if pdu != expected_echo {
        return Err(PlcWriteError::Protocol(format!(
            "write response did not echo the request: sent {expected_echo:02x?}, got {pdu:02x?}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one classification #131's brief calls out explicitly: a device
    /// exception must NOT be connection-fatal, or a broker would reconnect on
    /// every ordinary device refusal. (Kept from the removed
    /// `ModbusWriteClient` tests.)
    #[test]
    fn modbus_exception_is_not_connection_fatal() {
        let err = PlcWriteError::ModbusException {
            function: 0x06,
            code: 0x02,
            message: "illegal data address".to_string(),
        };
        assert!(!err.is_connection_fatal());
    }
}
