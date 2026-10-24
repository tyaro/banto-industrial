//! Decode a Modbus register window (`&[u16]`, already big-endian-per-register
//! per the wire format - see `modbus/frame.rs`) into typed values. Pure
//! functions, no I/O - `modbus/mod.rs` is the only caller in production
//! code, but they are exercised directly here with hand-built byte/word
//! arrays so the decoding math is proven independent of any socket.

use crate::error::PlcError;
use crate::types::{DataType, TagValue};

/// Which register holds the high 16 bits of a 32-bit value (or the highest
/// 16 bits of a 4-register 64-bit value) - `banto_driver_api::WordOrder`,
/// re-exported at its pre-R1-1 path (`banto_plc::decode::WordOrder`). Only the
/// enum moved (docs/banto-driver-api-design.md §3.1); combining registers per
/// order (`combine_u32` / `combine_u64`) stays here.
pub use banto_driver_api::WordOrder;

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
pub fn decode_register_value(
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
pub fn decode_register_bit(regs: &[u16], start: usize, bit: u8) -> Result<TagValue, PlcError> {
    let word = *regs.get(start).ok_or_else(|| {
        PlcError::Protocol(format!(
            "register window out of bounds: start={start} span=1 len={}",
            regs.len()
        ))
    })?;
    Ok(TagValue::Bit((word >> bit) & 1 != 0))
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
}
