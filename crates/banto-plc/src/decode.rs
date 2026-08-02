//! Decode a Modbus register window (`&[u16]`, already big-endian-per-register
//! per the wire format - see `modbus/frame.rs`) into typed values. Pure
//! functions, no I/O - `modbus/mod.rs` is the only caller in production
//! code, but they are exercised directly here with hand-built byte/word
//! arrays so the decoding math is proven independent of any socket.

use crate::error::PlcError;
use crate::types::{DataType, TagValue};

/// Which register holds the high 16 bits of a 32-bit value (docs/plan.md I2
/// §5). Byte order *within* a register is fixed by Modbus itself
/// (big-endian) and is not a parameter - only the order of the *two
/// registers* varies by device, which is what this controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WordOrder {
    /// First register (lower offset) holds the high word - the Modbus/IEEE
    /// convention and this crate's default.
    #[default]
    HighLow,
    /// First register holds the low word - common on drives/instruments
    /// that treat the register pair as a little-endian machine word.
    LowHigh,
}

/// Combine two registers into a `u32` per `order`.
fn combine_u32(regs: [u16; 2], order: WordOrder) -> u32 {
    let (hi, lo) = match order {
        WordOrder::HighLow => (regs[0], regs[1]),
        WordOrder::LowHigh => (regs[1], regs[0]),
    };
    ((hi as u32) << 16) | (lo as u32)
}

/// Decode the value at `regs[start..]` (1 or 2 registers, per `data_type`)
/// into a [`TagValue::F64`]. `start` is a [`crate::planning::MappedRequest::offset_in_read`],
/// an offset into the *response* window, not a PLC address, so bounds
/// checking here only guards against a planning bug, not a malformed PLC
/// response (the response's register count is already validated against the
/// request's `count` in `modbus/frame.rs::parse_response_pdu` before this is
/// ever called).
///
/// Never called with `DataType::Bit` in practice ([`crate::planning::plan_requests`]
/// only ever routes `Bit` requests into the coil/discrete-input decode path
/// in `modbus/mod.rs`, never here) - included in the match for exhaustiveness
/// and returns [`PlcError::Protocol`] rather than panicking if that
/// invariant is ever violated by a future change.
pub(crate) fn decode_register_value(
    regs: &[u16],
    start: usize,
    data_type: DataType,
    order: WordOrder,
) -> Result<TagValue, PlcError> {
    let span = data_type.register_span() as usize;
    let window = regs.get(start..start + span).ok_or_else(|| {
        PlcError::Protocol(format!(
            "register window out of bounds: start={start} span={span} len={}",
            regs.len()
        ))
    })?;

    let value = match data_type {
        DataType::I16 => window[0] as i16 as f64,
        DataType::U16 => window[0] as f64,
        DataType::I32 => combine_u32([window[0], window[1]], order) as i32 as f64,
        DataType::U32 => combine_u32([window[0], window[1]], order) as f64,
        DataType::F32 => f32::from_bits(combine_u32([window[0], window[1]], order)) as f64,
        DataType::Bit => {
            return Err(PlcError::Protocol(
                "decode_register_value called with DataType::Bit".to_string(),
            ))
        }
    };
    Ok(TagValue::F64(value))
}

/// Decode the MELSEC string at `regs[start..start + words]` into a Rust
/// `String` (S1 文字列タグ).
///
/// Byte order within each word is **low byte first**: a MELSEC string is the
/// SJIS byte stream laid into consecutive word devices two bytes at a time,
/// and on the wire each word travels little-endian - the wrapped `slmp` crate
/// itself builds its string type by taking the wire bytes verbatim
/// (`TypedData::from` in slmp-0.1.23's `data/mod.rs`:
/// `DataType::String(n) => PLCString::from_shift_jis_bytes(bytes, n)`, with
/// `DataType::U16` decoding the *same* stream via `u16::from_le_bytes`). So
/// word `w` contributes `w.to_le_bytes()` = `[low, high]`, and `"AB"` stored
/// at `D0` is the single word `0x4241`.
///
/// The byte stream is cut at the first NUL (0x00) - MELSEC's terminator
/// convention, and the same rule the wrapped crate's `PLCString` applies -
/// which also removes any trailing 0x00 padding of the fixed span. The
/// remainder is Shift-JIS decoded via `encoding_rs`; bytes that are not valid
/// SJIS are a per-request decode error (delivered as `Bad` by the executor,
/// like any other decode failure) rather than silently replaced text - a
/// mangled recipe string that still "reads fine" is worse than a Bad quality.
pub(crate) fn decode_string_value(
    regs: &[u16],
    start: usize,
    words: usize,
) -> Result<String, PlcError> {
    let window = regs.get(start..start + words).ok_or_else(|| {
        PlcError::Protocol(format!(
            "string window out of bounds: start={start} words={words} len={}",
            regs.len()
        ))
    })?;

    let mut bytes = Vec::with_capacity(words * 2);
    for w in window {
        bytes.extend_from_slice(&w.to_le_bytes()); // low byte first
    }
    let end = bytes.iter().position(|&b| b == 0x00).unwrap_or(bytes.len());

    let (text, _, had_errors) = encoding_rs::SHIFT_JIS.decode(&bytes[..end]);
    if had_errors {
        return Err(PlcError::Protocol(format!(
            "文字列デバイスの内容が Shift-JIS として不正です ({end} バイト)"
        )));
    }
    Ok(text.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_u16() {
        let regs = [0x1234u16];
        let v = decode_register_value(&regs, 0, DataType::U16, WordOrder::HighLow).unwrap();
        assert_eq!(v, TagValue::F64(0x1234 as f64));
    }

    #[test]
    fn decodes_i16_negative() {
        // 0xFFFF as i16 == -1.
        let regs = [0xFFFFu16];
        let v = decode_register_value(&regs, 0, DataType::I16, WordOrder::HighLow).unwrap();
        assert_eq!(v, TagValue::F64(-1.0));
    }

    #[test]
    fn decodes_i16_positive_boundary() {
        let regs = [0x7FFFu16]; // i16::MAX
        let v = decode_register_value(&regs, 0, DataType::I16, WordOrder::HighLow).unwrap();
        assert_eq!(v, TagValue::F64(32_767.0));
    }

    /// u32 value 0x0001_0002 as two registers, both word orders, exact bytes
    /// spelled out (docs/plan.md I2 §5: "32bit値のワード順テストは両方向の
    /// 実バイト列で").
    #[test]
    fn decodes_u32_high_low_word_order() {
        let regs = [0x0001u16, 0x0002u16]; // high word first
        let v = decode_register_value(&regs, 0, DataType::U32, WordOrder::HighLow).unwrap();
        assert_eq!(v, TagValue::F64(0x0001_0002_u32 as f64));
    }

    #[test]
    fn decodes_u32_low_high_word_order() {
        let regs = [0x0002u16, 0x0001u16]; // low word first
        let v = decode_register_value(&regs, 0, DataType::U32, WordOrder::LowHigh).unwrap();
        assert_eq!(v, TagValue::F64(0x0001_0002_u32 as f64));
    }

    #[test]
    fn decodes_i32_negative_across_both_word_orders() {
        // -1 as i32 = 0xFFFF_FFFF, same bytes regardless of word order.
        let hl = [0xFFFFu16, 0xFFFFu16];
        assert_eq!(
            decode_register_value(&hl, 0, DataType::I32, WordOrder::HighLow).unwrap(),
            TagValue::F64(-1.0)
        );
        let lh = [0xFFFFu16, 0xFFFFu16];
        assert_eq!(
            decode_register_value(&lh, 0, DataType::I32, WordOrder::LowHigh).unwrap(),
            TagValue::F64(-1.0)
        );
    }

    /// f32 1.5 = 0x3FC00000 (IEEE 754). High word 0x3FC0, low word 0x0000.
    #[test]
    fn decodes_f32_high_low_word_order() {
        let regs = [0x3FC0u16, 0x0000u16];
        let v = decode_register_value(&regs, 0, DataType::F32, WordOrder::HighLow).unwrap();
        assert_eq!(v, TagValue::F64(1.5));
    }

    #[test]
    fn decodes_f32_low_high_word_order() {
        let regs = [0x0000u16, 0x3FC0u16];
        let v = decode_register_value(&regs, 0, DataType::F32, WordOrder::LowHigh).unwrap();
        assert_eq!(v, TagValue::F64(1.5));
    }

    #[test]
    fn decode_respects_a_nonzero_start_offset() {
        let regs = [0xDEADu16, 0x0042u16, 0x0000u16];
        let v = decode_register_value(&regs, 1, DataType::U16, WordOrder::HighLow).unwrap();
        assert_eq!(v, TagValue::F64(0x0042 as f64));
    }

    #[test]
    fn out_of_bounds_window_is_a_protocol_error_not_a_panic() {
        let regs = [0x0001u16];
        let err = decode_register_value(&regs, 0, DataType::U32, WordOrder::HighLow).unwrap_err();
        assert!(matches!(err, PlcError::Protocol(_)));
    }

    #[test]
    fn decoding_bit_type_here_is_a_protocol_error_not_a_panic() {
        let regs = [0x0001u16];
        let err = decode_register_value(&regs, 0, DataType::Bit, WordOrder::HighLow).unwrap_err();
        assert!(matches!(err, PlcError::Protocol(_)));
    }

    // --- decode_string_value (S1 文字列タグ) -------------------------------

    /// The load-bearing byte-order case, exact words spelled out: "AB" =
    /// SJIS/ASCII [0x41, 0x42], low byte first within the word -> 0x4241
    /// (NOT 0x4142).
    #[test]
    fn decodes_ascii_string_low_byte_first_within_each_word() {
        let regs = [0x4241u16, 0x4443u16]; // "ABCD"
        assert_eq!(decode_string_value(&regs, 0, 2).unwrap(), "ABCD");
    }

    /// Multi-byte SJIS: "テスト" = [0x83, 0x65, 0x83, 0x58, 0x83, 0x67],
    /// packed low-first into three words.
    #[test]
    fn decodes_multibyte_sjis_string() {
        let regs = [0x6583u16, 0x5883u16, 0x6783u16];
        assert_eq!(decode_string_value(&regs, 0, 3).unwrap(), "テスト");
    }

    /// The stream is cut at the *first* NUL: an embedded terminator hides
    /// everything after it, including non-NUL bytes.
    #[test]
    fn trims_at_the_first_nul_terminator() {
        // "AB" + NUL + "C" -> bytes [0x41, 0x42, 0x00, 0x43].
        let regs = [0x4241u16, 0x4300u16];
        assert_eq!(decode_string_value(&regs, 0, 2).unwrap(), "AB");
    }

    /// Trailing NUL padding of the fixed span never reaches the value.
    #[test]
    fn trims_trailing_nul_padding() {
        let regs = [0x4241u16, 0x0000u16, 0x0000u16]; // "AB" in a 3-word span
        assert_eq!(decode_string_value(&regs, 0, 3).unwrap(), "AB");
    }

    /// A span filled to the brim (no terminator anywhere) is legal - the
    /// whole 2×words bytes are the string.
    #[test]
    fn decodes_a_full_span_with_no_terminator() {
        let regs = [0x4241u16, 0x4443u16]; // "ABCD", exactly 2L bytes
        assert_eq!(decode_string_value(&regs, 0, 2).unwrap(), "ABCD");
    }

    #[test]
    fn respects_a_nonzero_start_offset() {
        let regs = [0xDEADu16, 0x4241u16]; // window starts at 1
        assert_eq!(decode_string_value(&regs, 1, 1).unwrap(), "AB");
    }

    #[test]
    fn empty_string_decodes_as_empty() {
        let regs = [0x0000u16];
        assert_eq!(decode_string_value(&regs, 0, 1).unwrap(), "");
    }

    /// Invalid SJIS bytes are an error, not silently-substituted text
    /// (0xFF is not a legal Shift-JIS lead byte).
    #[test]
    fn invalid_sjis_bytes_are_a_decode_error_not_replacement_text() {
        let regs = [0x00FFu16]; // bytes [0xFF, 0x00] -> trimmed to [0xFF]
        let err = decode_string_value(&regs, 0, 1).unwrap_err();
        assert!(matches!(err, PlcError::Protocol(_)));
    }

    #[test]
    fn out_of_bounds_string_window_is_a_protocol_error_not_a_panic() {
        let regs = [0x4241u16];
        let err = decode_string_value(&regs, 0, 2).unwrap_err();
        assert!(matches!(err, PlcError::Protocol(_)));
    }
}
