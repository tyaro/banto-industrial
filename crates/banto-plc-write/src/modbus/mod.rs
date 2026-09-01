//! [`ModbusWriteClient`]: the [`crate::client::PlcWriteClient`] implementation
//! for Modbus TCP writes (#131 前半スライス). The write mirror of
//! `banto_plc::modbus::ModbusTcpClient`, hand-rolling the same wire format
//! rather than wrapping an external crate - Modbus has none to wrap, unlike
//! `crate::slmp` (see `banto-plc/src/modbus/frame.rs`'s module doc for why
//! the read side hand-rolls it too). This module reuses that read side's
//! now-`pub` framing primitives ([`banto_plc::modbus::frame`], exposed for
//! exactly this purpose in #131) rather than re-deriving MBAP framing a
//! second time - see that module's doc comment for exactly which items are
//! exposed and why.
//!
//! ## The broker-sharing seam (get this shape right for #130's follow-up)
//!
//! Like `crate::slmp`'s [`crate::execute_slmp_writes`], the actual wire
//! execution is factored out of [`ModbusWriteClient`] into a free function,
//! [`execute_modbus_writes`], that operates on a borrowed `&mut TcpStream`
//! plus the small amount of session state (`next_transaction_id`) a Modbus
//! TCP connection needs to track between calls. This is deliberately the
//! same shape as the SLMP side even though nothing wires it into
//! `banto-broker` yet (#130, PR #214, still in review per this slice's
//! brief) - so that whenever Modbus connections join broker management, the
//! broker can call [`plan_modbus_writes`]/[`execute_modbus_writes`] directly
//! against its own shared socket, exactly as it will for SLMP, without this
//! module needing to change shape.
//!
//! ## Where the connection-fatal/per-request line falls
//!
//! Identical framing to the read side
//! (`banto_plc::modbus::ModbusTcpClient`'s module doc): a Modbus **exception
//! response** (device says "no" to one write, e.g. illegal data address) is
//! not fatal - the byte stream is still in sync, so it becomes a per-request
//! [`crate::types::WriteResult::Bad`] via [`crate::error::PlcWriteError::ModbusException`]
//! and the loop moves on to the next group. Everything else (timeout, I/O
//! error, malformed frame, transaction id/unit id mismatch, unexpected
//! function code) is connection-fatal: [`execute_modbus_writes`] returns
//! `Err`, and the caller must reconnect before writing again. See
//! `crate::error::PlcWriteError::is_connection_fatal`'s doc comment for why
//! getting `ModbusException` classified correctly here matters for a future
//! broker caller.
//!
//! ## Single vs. multiple write function codes
//!
//! A [`crate::modbus::planning::ModbusPlannedWrite`] whose payload has
//! exactly one element uses FC5 (write single coil) / FC6 (write single
//! register); a multi-element group uses FC15 (write multiple coils) / FC16
//! (write multiple registers). The single-write codes are used whenever
//! possible (recommended by this slice's implementation brief) rather than a
//! one-element FC15/FC16, which the wire protocol also permits: a single
//! write's intent is unambiguous straight from its function code, and some
//! older/simpler slave implementations do not support FC15/FC16 at all,
//! while FC5/FC6 support is close to universal.
//!
//! ## `build_request_frame` reuse for FC5/FC6 (see also `banto-plc`'s doc
//! comment on that function)
//!
//! `banto_plc::modbus::frame::build_request_frame(tid, unit_id, function,
//! start_offset, count)` builds a PDU shaped `function + u16 + u16`. A read
//! request (`function + start_offset + count`) and a single-write
//! request/response echo (`function + address + value`) are *exactly* the
//! same byte shape - only the field names differ. [`build_write_frame`]
//! below therefore calls it unmodified for FC5/FC6, passing the coil/register
//! value in the position the function signature calls `count`.
//!
//! ## word_order default must match the read side (docs/plan.md I2 §5)
//!
//! [`banto_plc::ModbusTcpConfig::word_order`] defaults to
//! [`banto_plc::WordOrder::HighLow`] - the *opposite* of SLMP's `LowHigh`
//! default. [`ModbusWriteClient`] takes the same [`banto_plc::ModbusTcpConfig`]
//! the read client uses (rather than defining its own config type), which is
//! what makes "use the connection's configured word order" automatic instead
//! of a second place someone could set it inconsistently - see
//! `modbus_write_client_defaults_to_high_low_word_order` in this module's
//! tests for the pinned assertion.

pub mod planning;

#[cfg(any(test, feature = "simulator"))]
pub mod simulator;

#[cfg(test)]
mod integration_tests;

use std::time::Duration;

use banto_plc::modbus::frame::{
    build_request_frame, encode_bits_payload, encode_registers_payload, exception_message,
    parse_mbap_header, wrap_mbap, EXCEPTION_FLAG, MBAP_HEADER_LEN,
};
use banto_plc::{BoxFuture, ModbusTcpConfig, PlcError};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::client::PlcWriteClient;
use crate::error::PlcWriteError;
use crate::types::{WriteRequest, WriteResult};

use planning::{plan_modbus_writes, ModbusPlannedWrite, ModbusWritePlanOutcome, WritePayload};

const FC_WRITE_SINGLE_COIL: u8 = 0x05;
const FC_WRITE_SINGLE_REGISTER: u8 = 0x06;
const FC_WRITE_MULTIPLE_COILS: u8 = 0x0F;
const FC_WRITE_MULTIPLE_REGISTERS: u8 = 0x10;

/// `banto_plc::modbus::frame::parse_mbap_header` only ever fails with
/// `PlcError::Protocol` (a nonzero protocol id or a zero length field) - see
/// that function's own body. This unwraps and rewraps into this crate's
/// error type rather than a blanket `From<PlcError> for PlcWriteError`, which
/// would wrongly imply every `PlcError` variant has a sensible write-side
/// meaning (H9's read/write vocabularies are deliberately separate - see
/// `crate::error`'s module doc).
fn mbap_error_to_write(err: PlcError) -> PlcWriteError {
    match err {
        PlcError::Protocol(msg) => PlcWriteError::Protocol(msg),
        other => PlcWriteError::Protocol(other.to_string()),
    }
}

/// Build the wire frame for one [`ModbusPlannedWrite`], choosing FC5/FC6
/// (single) or FC15/FC16 (multiple) by its payload length (see this module's
/// doc comment). Returns the function code used (for the response parser)
/// alongside the frame.
fn build_write_frame(transaction_id: u16, unit_id: u8, plan: &ModbusPlannedWrite) -> (u8, Vec<u8>) {
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
fn parse_write_response_pdu(pdu: &[u8], expected_echo: &[u8; 5]) -> Result<(), PlcWriteError> {
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

/// Send one [`ModbusPlannedWrite`] and wait for its response.
/// `Err(PlcWriteError::ModbusException { .. })` is the *only* non-fatal
/// outcome (see this module's doc comment) - every other `Err` means the
/// stream itself is no longer trustworthy. Mirrors
/// `banto_plc::modbus::ModbusTcpClient::execute_one`'s cancellation-safety
/// note: the caller wraps this in `tokio::time::timeout`, and a cancelled
/// `read_exact` loses already-buffered bytes, which is exactly why a timeout
/// is connection-fatal rather than "just this group failed".
async fn execute_one(
    stream: &mut TcpStream,
    transaction_id: u16,
    unit_id: u8,
    plan: &ModbusPlannedWrite,
) -> Result<(), PlcWriteError> {
    // The function code is recovered from `expected_echo[0]` below rather
    // than threaded separately - `build_write_frame`'s first return value
    // exists for documentation/test callers.
    let (_function, frame) = build_write_frame(transaction_id, unit_id, plan);
    let mut expected_echo = [0u8; 5];
    expected_echo.copy_from_slice(&frame[MBAP_HEADER_LEN..MBAP_HEADER_LEN + 5]);

    stream
        .write_all(&frame)
        .await
        .map_err(|e| PlcWriteError::Connection(e.to_string()))?;

    let mut header_buf = [0u8; MBAP_HEADER_LEN];
    stream
        .read_exact(&mut header_buf)
        .await
        .map_err(|e| PlcWriteError::Connection(e.to_string()))?;
    let header = parse_mbap_header(&header_buf).map_err(mbap_error_to_write)?;
    if header.transaction_id != transaction_id {
        return Err(PlcWriteError::Protocol(format!(
            "transaction id mismatch: sent {transaction_id}, received {}",
            header.transaction_id
        )));
    }
    if header.unit_id != unit_id {
        return Err(PlcWriteError::Protocol(format!(
            "unit id mismatch: sent {unit_id}, received {}",
            header.unit_id
        )));
    }

    let pdu_len = header.length as usize - 1;
    let mut pdu_buf = vec![0u8; pdu_len];
    stream
        .read_exact(&mut pdu_buf)
        .await
        .map_err(|e| PlcWriteError::Connection(e.to_string()))?;

    parse_write_response_pdu(&pdu_buf, &expected_echo)
}

/// Execute a planned batch of writes on a **borrowed** `TcpStream` plus the
/// small amount of per-connection state (`next_transaction_id`) a Modbus TCP
/// session needs - the reusable core a future broker caller (#130's
/// follow-up) can call directly on its shared socket, mirroring
/// [`crate::execute_slmp_writes`]. See this module's doc comment.
///
/// `Err` is reserved for a connection-fatal failure (the caller must drop the
/// session and reconnect); a device-side exception becomes a per-request
/// `Bad` for that group's requests and the loop continues. On a fatal `Err`,
/// any groups already written have landed on the PLC - a partial batch -
/// exactly as the read side discards partial results on `Err`.
///
/// Does not own or reconnect the socket: connection lifecycle is the
/// caller's ([`ModbusWriteClient`] for the standalone form).
pub async fn execute_modbus_writes(
    stream: &mut TcpStream,
    unit_id: u8,
    response_timeout: Duration,
    next_transaction_id: &mut u16,
    outcome: &ModbusWritePlanOutcome,
    total_requests: usize,
) -> Result<Vec<WriteResult>, PlcWriteError> {
    let mut results: Vec<Option<WriteResult>> = vec![None; total_requests];
    for (index, reason) in &outcome.immediate_bad {
        results[*index] = Some(WriteResult::Bad(reason.clone()));
    }

    for plan in &outcome.writes {
        let tid = *next_transaction_id;
        *next_transaction_id = next_transaction_id.wrapping_add(1);

        let attempt =
            tokio::time::timeout(response_timeout, execute_one(stream, tid, unit_id, plan))
                .await
                .unwrap_or(Err(PlcWriteError::ResponseTimeout));

        match attempt {
            Ok(()) => {
                for &index in &plan.request_indices {
                    results[index] = Some(WriteResult::Ok);
                }
            }
            Err(err) if !err.is_connection_fatal() => {
                // Modbus exception: only this group is bad, keep going.
                for &index in &plan.request_indices {
                    results[index] = Some(WriteResult::Bad(err.clone()));
                }
            }
            Err(err) => {
                // Connection-fatal: stream may be desynchronized or dead.
                return Err(err);
            }
        }
    }

    Ok(results
        .into_iter()
        .enumerate()
        .map(|(i, r)| {
            r.unwrap_or_else(|| {
                panic!("plan_modbus_writes must account for every input index, missing {i}")
            })
        })
        .collect())
}

/// The standalone [`PlcWriteClient`] for Modbus TCP. Owns its own socket -
/// this is the form unit tests and the simulator drive. A future
/// broker-shared form (#130's follow-up) would call [`execute_modbus_writes`]
/// directly instead, mirroring `crate::slmp::SlmpWriteClient` vs.
/// `crate::execute_slmp_writes`.
pub struct ModbusWriteClient {
    config: ModbusTcpConfig,
    stream: Option<TcpStream>,
    /// Wraps on overflow, matching
    /// `banto_plc::modbus::ModbusTcpClient::next_transaction_id` - safe for
    /// the identical reason: this client only ever has one request in flight
    /// at a time.
    next_transaction_id: u16,
}

impl ModbusWriteClient {
    pub fn new(config: ModbusTcpConfig) -> Self {
        Self {
            config,
            stream: None,
            next_transaction_id: 0,
        }
    }
}

impl PlcWriteClient for ModbusWriteClient {
    fn connect(&mut self) -> BoxFuture<'_, Result<(), PlcWriteError>> {
        Box::pin(async move {
            let addr = format!("{}:{}", self.config.host, self.config.port);
            let stream =
                tokio::time::timeout(self.config.connect_timeout, TcpStream::connect(&addr))
                    .await
                    .map_err(|_| PlcWriteError::ConnectTimeout(addr.clone()))?
                    .map_err(|e| PlcWriteError::Connection(e.to_string()))?;
            // Same reasoning as `banto_plc::modbus::ModbusTcpClient::connect`:
            // small request/response pairs, always one in flight - Nagle's
            // algorithm would only add latency here.
            let _ = stream.set_nodelay(true);
            self.stream = Some(stream);
            self.next_transaction_id = 0;
            Ok(())
        })
    }

    fn write_batch<'a>(
        &'a mut self,
        requests: &'a [WriteRequest],
    ) -> BoxFuture<'a, Result<Vec<WriteResult>, PlcWriteError>> {
        Box::pin(async move {
            if self.stream.is_none() {
                return Err(PlcWriteError::NotConnected);
            }

            let outcome = plan_modbus_writes(requests, self.config.word_order);
            let unit_id = self.config.unit_id;
            let response_timeout = self.config.response_timeout;

            let stream = self
                .stream
                .as_mut()
                .expect("checked Some above, only cleared on the fatal branch below");

            match execute_modbus_writes(
                stream,
                unit_id,
                response_timeout,
                &mut self.next_transaction_id,
                &outcome,
                requests.len(),
            )
            .await
            {
                Ok(results) => Ok(results),
                Err(err) => {
                    self.stream = None;
                    Err(err)
                }
            }
        })
    }

    fn disconnect(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            self.stream = None; // dropping the TcpStream closes the socket
        })
    }
}
