//! Decode a Modbus register window (`&[u16]`, already big-endian-per-register
//! per the wire format - see `modbus/frame.rs`) into typed values. Pure
//! functions, no I/O - `modbus/mod.rs` is the only caller in production
//! code, but they are exercised directly here with hand-built byte/word
//! arrays so the decoding math is proven independent of any socket.

use crate::error::PlcError;
use crate::types::{DataType, StringEncoding, TagValue};

/// Which register holds the high 16 bits of a 32-bit value, or (since the
/// 64-bit `I64`/`U64`/`F64` types, owner decision 2026-09-08) the highest 16
/// bits of a 4-register 64-bit value (docs/plan.md I2 §5). Byte order
/// *within* a register is fixed by Modbus itself (big-endian) and is not a
/// parameter - only the order of the register group varies by device, which
/// is what this controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WordOrder {
    /// First register (lowest offset) holds the highest word - the
    /// Modbus/IEEE convention and this crate's default. For a 4-register
    /// value this means `regs[0]` is the most significant word and
    /// `regs[3]` the least significant.
    #[default]
    HighLow,
    /// First register holds the lowest word - common on drives/instruments
    /// that treat the register group as a little-endian machine word. For a
    /// 4-register value this reverses the whole group: `regs[3]` is the
    /// most significant word and `regs[0]` the least significant. The
    /// Omron KM-D1-ETN power meter (KANC-718B §12.5) uses this ordering for
    /// its `f64` measurement registers.
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

/// Combine four registers into a `u64` per `order`, the same convention as
/// [`combine_u32`] extended to a 4-word group (64-bit types, owner decision
/// 2026-09-08).
fn combine_u64(regs: [u16; 4], order: WordOrder) -> u64 {
    let [a, b, c, d] = match order {
        WordOrder::HighLow => regs,
        WordOrder::LowHigh => [regs[3], regs[2], regs[1], regs[0]],
    };
    ((a as u64) << 48) | ((b as u64) << 32) | ((c as u64) << 16) | (d as u64)
}

/// Decode the value at `regs[start..]` (1, 2 or 4 registers, per `data_type`)
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
        DataType::I64 => {
            combine_u64([window[0], window[1], window[2], window[3]], order) as i64 as f64
        }
        DataType::U64 => combine_u64([window[0], window[1], window[2], window[3]], order) as f64,
        DataType::F64 => f64::from_bits(combine_u64(
            [window[0], window[1], window[2], window[3]],
            order,
        )),
        DataType::Bit => {
            return Err(PlcError::Protocol(
                "decode_register_value called with DataType::Bit".to_string(),
            ))
        }
    };
    Ok(TagValue::F64(value))
}

/// Decode a single bit out of the word at `regs[start]` (T8, docs/tag-server-design.md
/// §6.1: "D100.5" - a bit-in-word address, folded into the word's ordinary
/// bulk read rather than requiring a dedicated wire operation). `bit` is
/// `0..=15`, already range-checked by [`crate::address::Address::parse`]/
/// [`crate::slmp::address::parse`] at tag-definition time, so this never
/// shifts out of a `u16`'s range; it is not re-validated here, matching
/// [`decode_register_value`]'s own trust of `start`/`span` having already
/// been proven safe by the planner.
///
/// Shares this module's bounds-checking style with [`decode_register_value`]:
/// an out-of-bounds `start` is a planning bug (never seen in practice, since
/// the planner always sizes the response window to cover every mapped
/// request), reported as [`PlcError::Protocol`] rather than a panic.
pub(crate) fn decode_register_bit(
    regs: &[u16],
    start: usize,
    bit: u8,
) -> Result<TagValue, PlcError> {
    let word = *regs.get(start).ok_or_else(|| {
        PlcError::Protocol(format!(
            "register window out of bounds: start={start} span=1 len={}",
            regs.len()
        ))
    })?;
    Ok(TagValue::Bit((word >> bit) & 1 != 0))
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
/// remainder is decoded via `encoding_rs` using the table `encoding` selects
/// (T20 ①b, docs/banto-hub-t20-design.md §3.1 - pre-①b this was
/// unconditionally Shift-JIS, mirroring `banto-plc-write/src/encode.rs`'s own
/// ①a widening on the write side); bytes that are not valid in that encoding
/// are a per-request decode error (delivered as `Bad` by the executor, like
/// any other decode failure) rather than silently replaced text - a mangled
/// recipe string that still "reads fine" is worse than a Bad quality.
pub(crate) fn decode_string_value(
    regs: &[u16],
    start: usize,
    words: usize,
    encoding: StringEncoding,
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

    let (table, label) = match encoding {
        StringEncoding::Utf8 => (encoding_rs::UTF_8, "UTF-8"),
        StringEncoding::ShiftJis => (encoding_rs::SHIFT_JIS, "Shift-JIS"),
    };
    let (text, _, had_errors) = table.decode(&bytes[..end]);
    if had_errors {
        return Err(PlcError::Protocol(format!(
            "文字列デバイスの内容が {label} として不正です ({end} バイト)"
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

    // --- 64-bit types (I64/U64/F64, owner decision 2026-09-08) ------------

    /// u64 value 0x0001_0002_0003_0004 as four registers, both word orders,
    /// exact bytes spelled out (same style as the 32-bit word-order tests
    /// above).
    #[test]
    fn decodes_u64_high_low_word_order() {
        let regs = [0x0001u16, 0x0002u16, 0x0003u16, 0x0004u16]; // highest word first
        let v = decode_register_value(&regs, 0, DataType::U64, WordOrder::HighLow).unwrap();
        assert_eq!(v, TagValue::F64(0x0001_0002_0003_0004_u64 as f64));
    }

    #[test]
    fn decodes_u64_low_high_word_order() {
        let regs = [0x0004u16, 0x0003u16, 0x0002u16, 0x0001u16]; // lowest word first
        let v = decode_register_value(&regs, 0, DataType::U64, WordOrder::LowHigh).unwrap();
        assert_eq!(v, TagValue::F64(0x0001_0002_0003_0004_u64 as f64));
    }

    #[test]
    fn decodes_i64_negative_across_both_word_orders() {
        // -1 as i64 = 0xFFFF_FFFF_FFFF_FFFF, same bytes regardless of word order.
        let hl = [0xFFFFu16, 0xFFFFu16, 0xFFFFu16, 0xFFFFu16];
        assert_eq!(
            decode_register_value(&hl, 0, DataType::I64, WordOrder::HighLow).unwrap(),
            TagValue::F64(-1.0)
        );
        let lh = [0xFFFFu16, 0xFFFFu16, 0xFFFFu16, 0xFFFFu16];
        assert_eq!(
            decode_register_value(&lh, 0, DataType::I64, WordOrder::LowHigh).unwrap(),
            TagValue::F64(-1.0)
        );
    }

    /// f64 1.5 = 0x3FF8_0000_0000_0000 (IEEE 754), both word orders.
    #[test]
    fn decodes_f64_high_low_word_order() {
        let regs = [0x3FF8u16, 0x0000u16, 0x0000u16, 0x0000u16];
        let v = decode_register_value(&regs, 0, DataType::F64, WordOrder::HighLow).unwrap();
        assert_eq!(v, TagValue::F64(1.5));
    }

    #[test]
    fn decodes_f64_low_high_word_order() {
        let regs = [0x0000u16, 0x0000u16, 0x0000u16, 0x3FF8u16];
        let v = decode_register_value(&regs, 0, DataType::F64, WordOrder::LowHigh).unwrap();
        assert_eq!(v, TagValue::F64(1.5));
    }

    /// Omron KM-D1-ETN power meter (KANC-718B §12.5) real-world case: its
    /// `f64` measurement registers use `WordOrder::LowHigh` - the received
    /// word order A,B,C,D (highest to lowest) must be re-ordered to D,C,B,A
    /// before the bits are reinterpreted as a double. Uses a couple of
    /// realistic measurement values (not just round bit patterns) to prove
    /// the round trip end to end.
    #[test]
    fn decodes_km_d1_etn_style_f64_measurement_with_low_high_word_order() {
        for value in [100.5_f64, 1234.5678_f64] {
            let bits = value.to_bits();
            let a = (bits >> 48) as u16; // most significant word
            let b = (bits >> 32) as u16;
            let c = (bits >> 16) as u16;
            let d = bits as u16; // least significant word
                                 // KM-D1-ETN transmits the word order reversed relative to the
                                 // straightforward high-to-low layout, i.e. LowHigh: the register
                                 // array below is [d, c, b, a] so that WordOrder::LowHigh
                                 // (which reverses regs) reconstructs [a, b, c, d].
            let regs = [d, c, b, a];
            let decoded =
                decode_register_value(&regs, 0, DataType::F64, WordOrder::LowHigh).unwrap();
            assert_eq!(decoded, TagValue::F64(value));
        }
    }

    /// Known/accepted limitation (owner decision 2026-09-08, see
    /// `DataType`'s doc comment): `U64`/`I64` values beyond `2^53` lose
    /// precision once widened to `f64`. `2^53 + 1` is the smallest integer
    /// that cannot be represented exactly as `f64`, and rounds down to
    /// `2^53` here.
    #[test]
    fn u64_beyond_2_pow_53_loses_precision_as_a_known_limitation() {
        let value: u64 = 9_007_199_254_740_993; // 2^53 + 1
        let regs = [
            (value >> 48) as u16,
            (value >> 32) as u16,
            (value >> 16) as u16,
            value as u16,
        ];
        let v = decode_register_value(&regs, 0, DataType::U64, WordOrder::HighLow).unwrap();
        assert_eq!(v, TagValue::F64(9_007_199_254_740_992.0));
    }

    #[test]
    fn decode_respects_a_nonzero_start_offset_for_64_bit_types() {
        let regs = [0xDEADu16, 0x0000u16, 0x0000u16, 0x0000u16, 0x0042u16];
        let v = decode_register_value(&regs, 1, DataType::U64, WordOrder::HighLow).unwrap();
        assert_eq!(v, TagValue::F64(0x0042 as f64));
    }

    #[test]
    fn out_of_bounds_64_bit_window_is_a_protocol_error_not_a_panic() {
        // Only 3 registers available, but U64 needs 4.
        let regs = [0x0000u16, 0x0000u16, 0x0000u16];
        let err = decode_register_value(&regs, 0, DataType::U64, WordOrder::HighLow).unwrap_err();
        assert!(matches!(err, PlcError::Protocol(_)));
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

    // --- decode_register_bit (T8, docs/tag-server-design.md §6.1) ---------

    #[test]
    fn decodes_every_bit_position_of_a_word() {
        // 0x1234 = 0b0001_0010_0011_0100 - bit 2, bit 4, bit 5, bit 9,
        // bit 12 are set; every other position in 0..=15 is clear.
        let regs = [0x1234u16];
        let set_bits = [2u8, 4, 5, 9, 12];
        for bit in 0..=15u8 {
            let expected = set_bits.contains(&bit);
            assert_eq!(
                decode_register_bit(&regs, 0, bit).unwrap(),
                TagValue::Bit(expected),
                "bit {bit} of 0x1234 should be {expected}"
            );
        }
    }

    #[test]
    fn decode_register_bit_respects_a_nonzero_start_offset() {
        let regs = [0x0000u16, 0xFFFFu16];
        assert_eq!(
            decode_register_bit(&regs, 1, 0).unwrap(),
            TagValue::Bit(true)
        );
        assert_eq!(
            decode_register_bit(&regs, 0, 0).unwrap(),
            TagValue::Bit(false)
        );
    }

    #[test]
    fn decode_register_bit_out_of_bounds_window_is_a_protocol_error_not_a_panic() {
        let regs: [u16; 0] = [];
        let err = decode_register_bit(&regs, 0, 0).unwrap_err();
        assert!(matches!(err, PlcError::Protocol(_)));
    }

    // --- decode_string_value (S1 文字列タグ) -------------------------------
    //
    // Every test below passes `StringEncoding::ShiftJis` explicitly (T20
    // ①b, docs/banto-hub-t20-design.md §3.1) - this crate's pre-①b behavior
    // was unconditionally Shift-JIS, and these tests fix that behavior in
    // place rather than silently switching to a default. The UTF-8 tests
    // further down are the new ①b coverage.

    /// The load-bearing byte-order case, exact words spelled out: "AB" =
    /// SJIS/ASCII [0x41, 0x42], low byte first within the word -> 0x4241
    /// (NOT 0x4142).
    #[test]
    fn decodes_ascii_string_low_byte_first_within_each_word() {
        let regs = [0x4241u16, 0x4443u16]; // "ABCD"
        assert_eq!(
            decode_string_value(&regs, 0, 2, StringEncoding::ShiftJis).unwrap(),
            "ABCD"
        );
    }

    /// Multi-byte SJIS: "テスト" = [0x83, 0x65, 0x83, 0x58, 0x83, 0x67],
    /// packed low-first into three words.
    #[test]
    fn decodes_multibyte_sjis_string() {
        let regs = [0x6583u16, 0x5883u16, 0x6783u16];
        assert_eq!(
            decode_string_value(&regs, 0, 3, StringEncoding::ShiftJis).unwrap(),
            "テスト"
        );
    }

    /// The stream is cut at the *first* NUL: an embedded terminator hides
    /// everything after it, including non-NUL bytes.
    #[test]
    fn trims_at_the_first_nul_terminator() {
        // "AB" + NUL + "C" -> bytes [0x41, 0x42, 0x00, 0x43].
        let regs = [0x4241u16, 0x4300u16];
        assert_eq!(
            decode_string_value(&regs, 0, 2, StringEncoding::ShiftJis).unwrap(),
            "AB"
        );
    }

    /// Trailing NUL padding of the fixed span never reaches the value.
    #[test]
    fn trims_trailing_nul_padding() {
        let regs = [0x4241u16, 0x0000u16, 0x0000u16]; // "AB" in a 3-word span
        assert_eq!(
            decode_string_value(&regs, 0, 3, StringEncoding::ShiftJis).unwrap(),
            "AB"
        );
    }

    /// A span filled to the brim (no terminator anywhere) is legal - the
    /// whole 2×words bytes are the string.
    #[test]
    fn decodes_a_full_span_with_no_terminator() {
        let regs = [0x4241u16, 0x4443u16]; // "ABCD", exactly 2L bytes
        assert_eq!(
            decode_string_value(&regs, 0, 2, StringEncoding::ShiftJis).unwrap(),
            "ABCD"
        );
    }

    #[test]
    fn respects_a_nonzero_start_offset() {
        let regs = [0xDEADu16, 0x4241u16]; // window starts at 1
        assert_eq!(
            decode_string_value(&regs, 1, 1, StringEncoding::ShiftJis).unwrap(),
            "AB"
        );
    }

    #[test]
    fn empty_string_decodes_as_empty() {
        let regs = [0x0000u16];
        assert_eq!(
            decode_string_value(&regs, 0, 1, StringEncoding::ShiftJis).unwrap(),
            ""
        );
    }

    /// Invalid SJIS bytes are an error, not silently-substituted text
    /// (0xFF is not a legal Shift-JIS lead byte).
    #[test]
    fn invalid_sjis_bytes_are_a_decode_error_not_replacement_text() {
        let regs = [0x00FFu16]; // bytes [0xFF, 0x00] -> trimmed to [0xFF]
        let err = decode_string_value(&regs, 0, 1, StringEncoding::ShiftJis).unwrap_err();
        assert!(matches!(err, PlcError::Protocol(_)));
    }

    #[test]
    fn out_of_bounds_string_window_is_a_protocol_error_not_a_panic() {
        let regs = [0x4241u16];
        let err = decode_string_value(&regs, 0, 2, StringEncoding::ShiftJis).unwrap_err();
        assert!(matches!(err, PlcError::Protocol(_)));
    }

    // --- decode_string_value: StringEncoding::Utf8 (T20 ①b, new) ----------

    /// The UTF-8 twin of `decodes_ascii_string_low_byte_first_within_each_word`.
    /// ASCII bytes are identical under UTF-8 and Shift-JIS, so this alone
    /// does not prove the encoding switch actually happened; the multibyte
    /// test below does.
    #[test]
    fn decodes_ascii_string_as_utf8() {
        let regs = [0x4241u16, 0x4443u16]; // "ABCD"
        assert_eq!(
            decode_string_value(&regs, 0, 2, StringEncoding::Utf8).unwrap(),
            "ABCD"
        );
    }

    /// The load-bearing UTF-8 case: "テスト" is 9 bytes in UTF-8 (vs 6 in
    /// SJIS - `decodes_multibyte_sjis_string`), packed low-byte-first into
    /// 5 words (padded with one trailing 0x00 byte, trimmed as a terminator
    /// like any other). Decoding these exact wire bytes as Shift-JIS would
    /// produce mojibake, not this text - proving the encoding switch is
    /// real, not a no-op.
    #[test]
    fn decodes_multibyte_utf8_string() {
        let bytes = "テスト".as_bytes(); // 9 bytes: E3 83 86 E3 82 B9 E3 83 88
        assert_eq!(bytes.len(), 9);
        let mut padded = bytes.to_vec();
        padded.push(0x00); // pad to 10 bytes = 5 words
        let regs: Vec<u16> = padded
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        assert_eq!(
            decode_string_value(&regs, 0, regs.len(), StringEncoding::Utf8).unwrap(),
            "テスト"
        );
    }

    /// Same wire bytes as `decodes_multibyte_utf8_string`, decoded as
    /// Shift-JIS instead - proves the two encodings are not interchangeable
    /// and `encoding` genuinely selects between them (rather than, say, both
    /// silently falling back to one table). Either outcome proves the point:
    /// a different (mojibake) string, or an outright decode error because
    /// the UTF-8 bytes are not valid Shift-JIS at all - what must never
    /// happen is decoding back to the original "テスト".
    #[test]
    fn utf8_wire_bytes_are_not_the_same_text_under_shift_jis() {
        let bytes = "テスト".as_bytes();
        let mut padded = bytes.to_vec();
        padded.push(0x00);
        let regs: Vec<u16> = padded
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        // `Err` also proves the point: the bytes are not valid Shift-JIS at all.
        if let Ok(sjis) = decode_string_value(&regs, 0, regs.len(), StringEncoding::ShiftJis) {
            assert_ne!(sjis, "テスト");
        }
    }

    /// Invalid UTF-8 bytes are a decode error under `StringEncoding::Utf8`,
    /// same contract as `invalid_sjis_bytes_are_a_decode_error_not_replacement_text`
    /// for Shift-JIS. 0xFF is never valid as a UTF-8 lead byte.
    #[test]
    fn invalid_utf8_bytes_are_a_decode_error_not_replacement_text() {
        let regs = [0x00FFu16]; // bytes [0xFF, 0x00] -> trimmed to [0xFF]
        let err = decode_string_value(&regs, 0, 1, StringEncoding::Utf8).unwrap_err();
        assert!(matches!(err, PlcError::Protocol(_)));
    }
}
