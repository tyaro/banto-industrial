//! Modbus TCP wire format: MBAP header + PDU encode/decode for the four read
//! function codes this crate uses (FC1/2/3/4). No external `modbus` crate
//! (docs/plan.md I2 §2 design decision): a read-only client only ever needs
//! four function codes, so hand-rolling ~150 lines here keeps the dependency
//! tree at zero for this crate and makes the wire format directly
//! inspectable/testable rather than trusting an opaque library. The same
//! trade-off is made again for MELSEC MC/SLMP when I2 continues into that
//! protocol.
//!
//! `pub(crate)` for most of this module: `modbus/mod.rs` (the client, encodes
//! requests / decodes responses) and, under the `simulator` feature,
//! `modbus/simulator.rs` (the test double, does the mirror image - decodes
//! requests / encodes responses) both live in this module tree and share
//! these functions, which is also what keeps the simulator's framing honest -
//! it is built from the same primitives as the real client rather than a
//! hand-rolled duplicate that could silently drift.
//!
//! ## What is `pub` instead, and why (2026-09-01, #131 前半スライス)
//!
//! [`build_request_frame`], [`wrap_mbap`], [`parse_mbap_header`]/[`MbapHeader`],
//! [`MBAP_HEADER_LEN`], [`exception_message`] and the byte-packing helpers
//! [`encode_bits_payload`]/[`encode_registers_payload`] are `pub` so
//! `banto-plc-write` (I5, a separate crate) can build and parse Modbus TCP
//! write frames (FC5/6/15/16) without re-deriving MBAP framing from the spec
//! a second time - the exact duplication H9
//! (docs/h9-slmp-structured-error-spec.md) had to clean up after (three
//! copies of SLMP's connect/dial sequence). This crate stays read-only itself
//! (see this crate's `lib.rs` - "no write method, on purpose"); exposing
//! these eight items only lets another crate speak the same *wire format*,
//! which is protocol trivia (byte layout), not a read/write policy decision.
//!
//! This is deliberately a **narrower** set than "make the whole module
//! `pub`": [`FC_READ_*`] stay `pub(crate)` (a write client defines its own
//! FC5/6/15/16 constants - reusing this module's read-only FC list for a
//! write would be actively confusing), and [`ParsedResponse`]/
//! [`parse_response_pdu`] stay `pub(crate)` too - their PDU shape assumes a
//! **read** response (`byte_count` + data), which does not describe a write
//! response at all (FC5/6/15/16 always echo back `function + 2 words`, no
//! byte-count field - see the Modbus Application Protocol spec §6.5-6.8,
//! 6.11-6.12). `banto-plc-write` parses its own echo shape instead of forcing
//! a mismatched shape through this function.
//!
//! [`build_request_frame`] is reused as-is for FC5/FC6 (write single coil/
//! register) despite its name: its PDU shape - `function + u16 + u16` - is
//! *byte-for-byte* identical to a single-write request (`function` +
//! address + value, both 16-bit big-endian fields), so `banto-plc-write`
//! calls it with the write target's value in the `count` parameter's place.
//! See that crate's own doc comment at the call site for why this is a
//! genuine shape reuse rather than a misuse.

use crate::address::AddressArea;
use crate::error::PlcError;

pub(crate) const FC_READ_COILS: u8 = 0x01;
pub(crate) const FC_READ_DISCRETE_INPUTS: u8 = 0x02;
pub(crate) const FC_READ_HOLDING_REGISTERS: u8 = 0x03;
pub(crate) const FC_READ_INPUT_REGISTERS: u8 = 0x04;

/// The exception-response bit (Modbus Application Protocol spec §7): a
/// response function code with this bit set means "the request failed, the
/// next byte is an exception code" rather than "here is your data". `pub`
/// (2026-09-01, #131) so `banto-plc-write` can recognize an exception
/// response in its own write-echo parser without re-deriving this from the
/// spec a second time.
pub const EXCEPTION_FLAG: u8 = 0x80;

/// MBAP header size in bytes: transaction id (2) + protocol id (2) + length
/// (2) + unit id (1). `pub` (2026-09-01, #131) - see this module's doc
/// comment.
pub const MBAP_HEADER_LEN: usize = 7;

pub(crate) fn function_code_for(area: AddressArea) -> u8 {
    match area {
        AddressArea::Coil => FC_READ_COILS,
        AddressArea::DiscreteInput => FC_READ_DISCRETE_INPUTS,
        AddressArea::InputRegister => FC_READ_INPUT_REGISTERS,
        AddressArea::HoldingRegister => FC_READ_HOLDING_REGISTERS,
    }
}

/// Human-readable text for a Modbus exception code (Modbus Application
/// Protocol spec §7, standard codes only - device-specific codes outside
/// this table still surface, just with a generic message). `pub` (2026-09-01,
/// #131) so `banto-plc-write`'s `PlcWriteError::ModbusException` gets the
/// identical wording the read side's `PlcError::ModbusException` uses,
/// rather than a second, possibly-drifting copy of this table.
pub fn exception_message(code: u8) -> &'static str {
    match code {
        0x01 => "不正なファンクションコード",
        0x02 => "不正なデータアドレス",
        0x03 => "不正なデータ値",
        0x04 => "スレーブデバイス障害",
        0x05 => "確認応答（処理中）",
        0x06 => "スレーブデバイスビジー",
        0x08 => "メモリパリティエラー",
        0x0A => "ゲートウェイパス不通",
        0x0B => "ゲートウェイ応答なし",
        _ => "不明な例外コード",
    }
}

/// Build a complete request frame (MBAP header + PDU) for a "read N
/// elements starting at `start_offset`" call - the same PDU shape for all
/// four read function codes, only `function` differs. `pub` (2026-09-01,
/// #131) - see this module's doc comment for why `banto-plc-write` also
/// calls this, unmodified, for FC5/FC6 single writes.
pub fn build_request_frame(
    transaction_id: u16,
    unit_id: u8,
    function: u8,
    start_offset: u16,
    count: u16,
) -> Vec<u8> {
    let mut pdu = Vec::with_capacity(5);
    pdu.push(function);
    pdu.extend_from_slice(&start_offset.to_be_bytes());
    pdu.extend_from_slice(&count.to_be_bytes());
    wrap_mbap(transaction_id, unit_id, &pdu)
}

/// Build a normal (non-exception) response frame for FC1/2 (packed coil
/// bits) or FC3/4 (registers), used by the test simulator.
#[cfg_attr(not(any(test, feature = "simulator")), allow(dead_code))]
pub(crate) fn build_data_response_frame(
    transaction_id: u16,
    unit_id: u8,
    function: u8,
    payload: &[u8],
) -> Vec<u8> {
    let mut pdu = Vec::with_capacity(2 + payload.len());
    pdu.push(function);
    pdu.push(payload.len() as u8);
    pdu.extend_from_slice(payload);
    wrap_mbap(transaction_id, unit_id, &pdu)
}

/// Build an exception response frame, used by the test simulator to inject
/// device-side failures (docs/plan.md I2 §6).
#[cfg_attr(not(any(test, feature = "simulator")), allow(dead_code))]
pub(crate) fn build_exception_response_frame(
    transaction_id: u16,
    unit_id: u8,
    function: u8,
    exception_code: u8,
) -> Vec<u8> {
    let pdu = [function | EXCEPTION_FLAG, exception_code];
    wrap_mbap(transaction_id, unit_id, &pdu)
}

/// Pack coil/discrete-input values into Modbus's byte layout: bit 0 of the
/// first byte is the first element, LSB-first, remaining bits of the final
/// byte zero-padded (Modbus Application Protocol spec §6.1). Used for FC1/2
/// *response* payloads here (by the test simulator) and, identically, for an
/// FC15 (write multiple coils) *request* payload by `banto-plc-write` - the
/// two are the same on-wire bit packing, just read in different directions.
/// `pub` (2026-09-01, #131) - see this module's doc comment.
pub fn encode_bits_payload(bits: &[bool]) -> Vec<u8> {
    let byte_len = bits.len().div_ceil(8);
    let mut out = vec![0u8; byte_len];
    for (i, &bit) in bits.iter().enumerate() {
        if bit {
            out[i / 8] |= 1 << (i % 8);
        }
    }
    out
}

/// Pack register values into Modbus's byte layout: each register big-endian,
/// in order. Used for FC3/4 *response* payloads here (by the test simulator)
/// and, identically, for an FC16 (write multiple registers) *request*
/// payload by `banto-plc-write`. `pub` (2026-09-01, #131) - see this module's
/// doc comment.
pub fn encode_registers_payload(regs: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(regs.len() * 2);
    for r in regs {
        out.extend_from_slice(&r.to_be_bytes());
    }
    out
}

/// `pub` (2026-09-01, #131) - the one piece `banto-plc-write` needs to frame
/// its own FC15/FC16 (multiple coils/registers) request PDUs, which have a
/// byte-count-plus-data shape [`build_request_frame`] does not produce. See
/// this module's doc comment.
pub fn wrap_mbap(transaction_id: u16, unit_id: u8, pdu: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(MBAP_HEADER_LEN + pdu.len());
    frame.extend_from_slice(&transaction_id.to_be_bytes());
    frame.extend_from_slice(&0u16.to_be_bytes()); // protocol id, always 0 for Modbus
    let length = (pdu.len() + 1) as u16; // unit id byte + pdu
    frame.extend_from_slice(&length.to_be_bytes());
    frame.push(unit_id);
    frame.extend_from_slice(pdu);
    frame
}

/// A parsed MBAP header, still needing its PDU (`length - 1` more bytes) read
/// separately - see `modbus/mod.rs`'s `execute_one` for the two-phase
/// `read_exact` this enables (fixed 7-byte header, then a PDU sized from the
/// header's `length` field).
/// `pub` (2026-09-01, #131) - `banto-plc-write` needs this to do the same
/// two-phase `read_exact` (fixed header, then a PDU sized from `length`) as
/// this crate's own read client. See this module's doc comment.
#[derive(Debug)]
pub struct MbapHeader {
    pub transaction_id: u16,
    pub length: u16,
    pub unit_id: u8,
}

pub fn parse_mbap_header(buf: &[u8; MBAP_HEADER_LEN]) -> Result<MbapHeader, PlcError> {
    let transaction_id = u16::from_be_bytes([buf[0], buf[1]]);
    let protocol_id = u16::from_be_bytes([buf[2], buf[3]]);
    if protocol_id != 0 {
        return Err(PlcError::Protocol(format!(
            "unexpected MBAP protocol id: {protocol_id} (expected 0)"
        )));
    }
    let length = u16::from_be_bytes([buf[4], buf[5]]);
    if length == 0 {
        return Err(PlcError::Protocol(
            "MBAP length field is 0 (no unit id byte)".to_string(),
        ));
    }
    let unit_id = buf[6];
    Ok(MbapHeader {
        transaction_id,
        length,
        unit_id,
    })
}

/// A decoded response PDU, one variant per shape this crate needs to
/// interpret.
#[derive(Debug)]
pub(crate) enum ParsedResponse {
    Bits(Vec<bool>),
    Registers(Vec<u16>),
    Exception { code: u8 },
}

/// Parse a response PDU already known to belong to `expected_function`'s
/// request (matched by transaction id at the MBAP layer before this is
/// called). `expected_quantity` is the element count from the *request* -
/// used both to know how many bits to unpack from the trailing padding and
/// to sanity-check the register response's declared byte count.
pub(crate) fn parse_response_pdu(
    pdu: &[u8],
    expected_function: u8,
    expected_quantity: u16,
) -> Result<ParsedResponse, PlcError> {
    let function = *pdu
        .first()
        .ok_or_else(|| PlcError::Protocol("empty response PDU".to_string()))?;

    if function == expected_function | EXCEPTION_FLAG {
        let code = *pdu
            .get(1)
            .ok_or_else(|| PlcError::Protocol("truncated exception response".to_string()))?;
        return Ok(ParsedResponse::Exception { code });
    }
    if function != expected_function {
        return Err(PlcError::Protocol(format!(
            "unexpected function code in response: {function:#04x} (expected {expected_function:#04x})"
        )));
    }

    let byte_count = *pdu
        .get(1)
        .ok_or_else(|| PlcError::Protocol("truncated response: missing byte count".to_string()))?
        as usize;
    let data = pdu.get(2..2 + byte_count).ok_or_else(|| {
        PlcError::Protocol("truncated response: declared byte count exceeds PDU".to_string())
    })?;

    match expected_function {
        FC_READ_COILS | FC_READ_DISCRETE_INPUTS => {
            let expected_bytes = (expected_quantity as usize).div_ceil(8);
            if byte_count != expected_bytes {
                return Err(PlcError::Protocol(format!(
                    "coil response byte count mismatch: got {byte_count}, expected {expected_bytes}"
                )));
            }
            let mut bits = Vec::with_capacity(expected_quantity as usize);
            for i in 0..expected_quantity as usize {
                let byte = data[i / 8];
                bits.push((byte >> (i % 8)) & 1 == 1);
            }
            Ok(ParsedResponse::Bits(bits))
        }
        FC_READ_HOLDING_REGISTERS | FC_READ_INPUT_REGISTERS => {
            let expected_bytes = expected_quantity as usize * 2;
            if byte_count != expected_bytes {
                return Err(PlcError::Protocol(format!(
                    "register response byte count mismatch: got {byte_count}, expected {expected_bytes}"
                )));
            }
            let regs = data
                .chunks_exact(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect();
            Ok(ParsedResponse::Registers(regs))
        }
        other => Err(PlcError::Protocol(format!(
            "unsupported function code: {other:#04x}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_request_frame_matches_known_bytes() {
        // Transaction id 0x0001, unit id 0x01, FC3, start 0x0000, count 0x0002
        // - a textbook "read 2 holding registers from address 40001" request,
        // cross-checked against the Modbus Application Protocol spec's worked
        // example shape.
        let frame = build_request_frame(0x0001, 0x01, FC_READ_HOLDING_REGISTERS, 0x0000, 0x0002);
        assert_eq!(
            frame,
            vec![
                0x00, 0x01, // transaction id
                0x00, 0x00, // protocol id
                0x00, 0x06, // length (unit id + pdu = 1 + 5)
                0x01, // unit id
                0x03, // function code
                0x00, 0x00, // start offset
                0x00, 0x02, // count
            ]
        );
    }

    #[test]
    fn parse_mbap_header_round_trips_build_request_frame() {
        let frame = build_request_frame(0x1234, 0x07, FC_READ_COILS, 0x000A, 0x0005);
        let mut header_buf = [0u8; MBAP_HEADER_LEN];
        header_buf.copy_from_slice(&frame[..MBAP_HEADER_LEN]);
        let header = parse_mbap_header(&header_buf).unwrap();
        assert_eq!(header.transaction_id, 0x1234);
        assert_eq!(header.unit_id, 0x07);
        assert_eq!(header.length as usize, frame.len() - MBAP_HEADER_LEN + 1);
    }

    #[test]
    fn parse_mbap_header_rejects_nonzero_protocol_id() {
        let mut buf = [0u8; MBAP_HEADER_LEN];
        buf[2] = 0x00;
        buf[3] = 0x01; // protocol id = 1
        let err = parse_mbap_header(&buf).unwrap_err();
        assert!(matches!(err, PlcError::Protocol(_)));
    }

    #[test]
    fn parse_mbap_header_rejects_zero_length() {
        let buf = [0u8; MBAP_HEADER_LEN]; // length field = 0
        let err = parse_mbap_header(&buf).unwrap_err();
        assert!(matches!(err, PlcError::Protocol(_)));
    }

    #[test]
    fn encode_then_parse_registers_round_trips() {
        let regs = [0x0001u16, 0xBEEFu16, 0x0000u16];
        let payload = encode_registers_payload(&regs);
        let response_frame =
            build_data_response_frame(0x0009, 0x01, FC_READ_HOLDING_REGISTERS, &payload);
        let pdu = &response_frame[MBAP_HEADER_LEN..];
        match parse_response_pdu(pdu, FC_READ_HOLDING_REGISTERS, 3).unwrap() {
            ParsedResponse::Registers(got) => assert_eq!(got, regs),
            other => panic!(
                "expected Registers, got a different variant: {}",
                matches_variant_name(&other)
            ),
        }
    }

    #[test]
    fn encode_then_parse_bits_round_trips_with_partial_final_byte() {
        // 10 bits -> 2 bytes, second byte only half used - proves the
        // trailing-padding bits are dropped rather than leaking as data.
        let bits = [
            true, false, true, true, false, false, false, false, true, true,
        ];
        let payload = encode_bits_payload(&bits);
        assert_eq!(payload.len(), 2);
        let response_frame = build_data_response_frame(0x0001, 0x01, FC_READ_COILS, &payload);
        let pdu = &response_frame[MBAP_HEADER_LEN..];
        match parse_response_pdu(pdu, FC_READ_COILS, bits.len() as u16).unwrap() {
            ParsedResponse::Bits(got) => assert_eq!(got, bits),
            other => panic!(
                "expected Bits, got a different variant: {}",
                matches_variant_name(&other)
            ),
        }
    }

    #[test]
    fn parse_response_pdu_recognizes_exception_flag() {
        let frame = build_exception_response_frame(0x0002, 0x01, FC_READ_HOLDING_REGISTERS, 0x02);
        let pdu = &frame[MBAP_HEADER_LEN..];
        match parse_response_pdu(pdu, FC_READ_HOLDING_REGISTERS, 1).unwrap() {
            ParsedResponse::Exception { code } => assert_eq!(code, 0x02),
            other => panic!(
                "expected Exception, got a different variant: {}",
                matches_variant_name(&other)
            ),
        }
    }

    #[test]
    fn parse_response_pdu_rejects_unexpected_function_code() {
        // Respond to an FC3 request as if it were FC4 (no exception flag).
        let payload = encode_registers_payload(&[0u16]);
        let frame = build_data_response_frame(0x0001, 0x01, FC_READ_INPUT_REGISTERS, &payload);
        let pdu = &frame[MBAP_HEADER_LEN..];
        let err = parse_response_pdu(pdu, FC_READ_HOLDING_REGISTERS, 1).unwrap_err();
        assert!(matches!(err, PlcError::Protocol(_)));
    }

    #[test]
    fn parse_response_pdu_rejects_register_byte_count_mismatch() {
        let payload = encode_registers_payload(&[0u16, 1u16]); // 2 registers
        let frame = build_data_response_frame(0x0001, 0x01, FC_READ_HOLDING_REGISTERS, &payload);
        let pdu = &frame[MBAP_HEADER_LEN..];
        // Claim we expected 3 registers, but the payload only has 2.
        let err = parse_response_pdu(pdu, FC_READ_HOLDING_REGISTERS, 3).unwrap_err();
        assert!(matches!(err, PlcError::Protocol(_)));
    }

    #[test]
    fn parse_response_pdu_rejects_truncated_pdu() {
        let err = parse_response_pdu(&[FC_READ_HOLDING_REGISTERS], FC_READ_HOLDING_REGISTERS, 1)
            .unwrap_err();
        assert!(matches!(err, PlcError::Protocol(_)));
    }

    #[test]
    fn parse_response_pdu_rejects_empty_pdu() {
        let err = parse_response_pdu(&[], FC_READ_HOLDING_REGISTERS, 1).unwrap_err();
        assert!(matches!(err, PlcError::Protocol(_)));
    }

    #[test]
    fn exception_message_covers_the_documented_standard_codes() {
        for code in [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x08, 0x0A, 0x0B] {
            assert_ne!(exception_message(code), "不明な例外コード");
        }
        assert_eq!(exception_message(0xEE), "不明な例外コード");
    }

    // Small helper so assertion-failure messages name the *actual* variant
    // without needing ParsedResponse: Debug (kept out of the production
    // build - it's `pub(crate)` and only ever compared via `match` there).
    fn matches_variant_name(r: &ParsedResponse) -> &'static str {
        match r {
            ParsedResponse::Bits(_) => "Bits",
            ParsedResponse::Registers(_) => "Registers",
            ParsedResponse::Exception { .. } => "Exception",
        }
    }
}
