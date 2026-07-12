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
}
