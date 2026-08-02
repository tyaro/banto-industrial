//! Encode a [`banto_plc::TagValue`] into the raw register/bit payload a write
//! puts on the wire - the exact inverse of `banto-plc/src/decode.rs`, which
//! *decodes* a raw register window into a `TagValue`. Pure functions, no I/O.
//!
//! Keeping this symmetric with the read side matters for one specific reason:
//! the 32-bit word-order switch. The read side fetches a raw `u16` window and
//! applies [`WordOrder`] in `decode_register_value`; this side produces a raw
//! `u16` window and applies the *same* [`WordOrder`] here, so a value written
//! by this crate and read back by `banto-plc` round-trips byte-for-byte. That
//! is why the write client hands the wrapped `slmp` crate a sequence of
//! `TypedData::U16` words it computed here, rather than a `TypedData::U32`: the
//! crate's own 32-bit serialization is fixed to one word order (native-endian,
//! i.e. low word first) and would silently disagree with a `HighLow`-configured
//! read.
//!
//! Every numeric conversion here can *fail* in a way no decode can: a `f64`
//! that will not fit its target register width. Decoding only ever widens (any
//! wire width fits in `f64`); encoding narrows, so out-of-range and
//! non-integral values are rejected as [`PlcWriteError::ValueOutOfRange`]
//! rather than silently truncated onto a PLC output.

use banto_plc::{DataType, TagValue, WordOrder};

use crate::error::PlcWriteError;

/// Split a `u32` into its two register words per `order` - the exact inverse of
/// `banto-plc/src/decode.rs`'s `combine_u32`, which reads
/// `HighLow => (hi=regs[0], lo=regs[1])` and `LowHigh => (hi=regs[1],
/// lo=regs[0])`.
fn split_u32(val: u32, order: WordOrder) -> [u16; 2] {
    let hi = (val >> 16) as u16;
    let lo = val as u16;
    match order {
        WordOrder::HighLow => [hi, lo],
        WordOrder::LowHigh => [lo, hi],
    }
}

/// Pull the `f64` out of a numeric [`TagValue`], or report the caller handed a
/// `Bit` value for a numeric `data_type`.
fn require_f64(value: TagValue, data_type: DataType) -> Result<f64, PlcWriteError> {
    match value {
        TagValue::F64(x) => Ok(x),
        TagValue::Bit(_) => Err(PlcWriteError::ValueTypeMismatch {
            data_type: data_type.to_string(),
            value_kind: "bit".to_string(),
        }),
    }
}

/// Reject a value that cannot land exactly in an integer register: non-finite,
/// non-integral, or outside `[lo, hi]`. `lo`/`hi` are passed as `f64` (every
/// 16-/32-bit integer bound is exactly representable) so one helper serves
/// every integer width.
fn require_integral_in_range(
    x: f64,
    lo: f64,
    hi: f64,
    data_type: DataType,
) -> Result<(), PlcWriteError> {
    let bad = |detail: &str| {
        Err(PlcWriteError::ValueOutOfRange {
            data_type: data_type.to_string(),
            value: format!("{x}"),
            detail: detail.to_string(),
        })
    };
    if !x.is_finite() {
        return bad("値が有限ではありません");
    }
    if x.fract() != 0.0 {
        return bad("整数ではありません");
    }
    if x < lo || x > hi {
        return bad(&format!("範囲 [{lo}, {hi}] の外です"));
    }
    Ok(())
}

/// Encode a value bound for a **word** device into its register window (1 word
/// for 16-bit types, 2 for 32-bit, ordered per `order`). `data_type` is
/// guaranteed non-`Bit` by the planner's compatibility check before this is
/// called; a `Bit` here would be a planner bug and is reported as a value error
/// rather than panicking.
pub(crate) fn encode_word_value(
    value: TagValue,
    data_type: DataType,
    order: WordOrder,
) -> Result<Vec<u16>, PlcWriteError> {
    let x = require_f64(value, data_type)?;
    let words = match data_type {
        DataType::U16 => {
            require_integral_in_range(x, 0.0, u16::MAX as f64, data_type)?;
            vec![x as u16]
        }
        DataType::I16 => {
            require_integral_in_range(x, i16::MIN as f64, i16::MAX as f64, data_type)?;
            vec![(x as i16) as u16]
        }
        DataType::U32 => {
            require_integral_in_range(x, 0.0, u32::MAX as f64, data_type)?;
            split_u32(x as u32, order).to_vec()
        }
        DataType::I32 => {
            require_integral_in_range(x, i32::MIN as f64, i32::MAX as f64, data_type)?;
            split_u32((x as i32) as u32, order).to_vec()
        }
        DataType::F32 => {
            // f64 -> f32 is inherently lossy (the read side widened f32 -> f64),
            // so precision loss is expected here rather than an error. Only a
            // value too large in magnitude for f32 would become infinite, which
            // is a real out-of-range condition worth rejecting.
            let f = x as f32;
            if !f.is_finite() && x.is_finite() {
                return Err(PlcWriteError::ValueOutOfRange {
                    data_type: data_type.to_string(),
                    value: format!("{x}"),
                    detail: "f32 で表現するには大きすぎます".to_string(),
                });
            }
            split_u32(f.to_bits(), order).to_vec()
        }
        DataType::Bit => {
            return Err(PlcWriteError::ValueOutOfRange {
                data_type: data_type.to_string(),
                value: format!("{x}"),
                detail: "bit 型はワードデバイスに書けません".to_string(),
            });
        }
    };
    Ok(words)
}

/// Encode a value bound for a **bit** device. `data_type` is guaranteed `Bit`
/// by the planner; the value must be a [`TagValue::Bit`], else it is a
/// per-request [`PlcWriteError::ValueTypeMismatch`].
pub(crate) fn encode_bit_value(value: TagValue) -> Result<bool, PlcWriteError> {
    match value {
        TagValue::Bit(b) => Ok(b),
        TagValue::F64(_) => Err(PlcWriteError::ValueTypeMismatch {
            data_type: DataType::Bit.to_string(),
            value_kind: "numeric".to_string(),
        }),
    }
}

/// Encode a string bound for a `words`-word span of a word device (S1
/// 文字列タグ): Shift-JIS bytes, 0x00-padded to exactly `2 * words` bytes,
/// packed **low byte first** into each word - the exact inverse of
/// `banto-plc/src/decode.rs::decode_string_value` (see there for the wire
/// evidence from the wrapped `slmp` crate), so a write→read round trip is
/// byte-for-byte. Note [`banto_plc::WordOrder`] plays no part here: it orders
/// the two words of a 32-bit *numeric* value, whereas a string's byte order
/// within each word is fixed by MELSEC's storage convention.
///
/// Two rejections, both per-request `Bad`s and both about never mangling text
/// onto a live PLC:
/// - a character with no Shift-JIS representation
///   ([`PlcWriteError::ValueOutOfRange`]) rather than encoding_rs's HTML
///   escape substitution
/// - encoded bytes longer than the span's `2 * words` capacity
///   ([`PlcWriteError::ValueOutOfRange`]) rather than silent truncation - a
///   cut-off recipe string is a real hazard
pub(crate) fn encode_string_value(value: &str, words: u16) -> Result<Vec<u16>, PlcWriteError> {
    let (bytes, _, had_errors) = encoding_rs::SHIFT_JIS.encode(value);
    if had_errors {
        return Err(PlcWriteError::ValueOutOfRange {
            data_type: "string".to_string(),
            value: value.to_string(),
            detail: "Shift-JIS で表現できない文字を含みます".to_string(),
        });
    }
    let capacity = words as usize * 2;
    if bytes.len() > capacity {
        return Err(PlcWriteError::ValueOutOfRange {
            data_type: "string".to_string(),
            value: value.to_string(),
            detail: format!(
                "Shift-JIS で {} バイトになり、{words} 語（{capacity} バイト）に収まりません",
                bytes.len()
            ),
        });
    }

    let mut padded = bytes.into_owned();
    padded.resize(capacity, 0x00);
    Ok(padded
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_u16() {
        assert_eq!(
            encode_word_value(
                TagValue::F64(0x1234 as f64),
                DataType::U16,
                WordOrder::LowHigh
            )
            .unwrap(),
            vec![0x1234]
        );
    }

    #[test]
    fn encodes_i16_negative() {
        // -1 as i16 == 0xFFFF.
        assert_eq!(
            encode_word_value(TagValue::F64(-1.0), DataType::I16, WordOrder::LowHigh).unwrap(),
            vec![0xFFFF]
        );
    }

    /// u32 0x0001_0002 in both word orders, exact words spelled out - the write
    /// twin of `decode.rs`'s two word-order cases.
    #[test]
    fn encodes_u32_in_both_word_orders() {
        // LowHigh: low word (0x0002) first, high word (0x0001) second - MELSEC's
        // native storage and this crate's SLMP default.
        assert_eq!(
            encode_word_value(
                TagValue::F64(0x0001_0002u32 as f64),
                DataType::U32,
                WordOrder::LowHigh
            )
            .unwrap(),
            vec![0x0002, 0x0001]
        );
        // HighLow: high word first - must be the mirror image.
        assert_eq!(
            encode_word_value(
                TagValue::F64(0x0001_0002u32 as f64),
                DataType::U32,
                WordOrder::HighLow
            )
            .unwrap(),
            vec![0x0001, 0x0002]
        );
    }

    #[test]
    fn encodes_i32_negative_same_bytes_in_both_orders() {
        // -1 as i32 = 0xFFFF_FFFF, symmetric, so both words are 0xFFFF either way.
        for order in [WordOrder::LowHigh, WordOrder::HighLow] {
            assert_eq!(
                encode_word_value(TagValue::F64(-1.0), DataType::I32, order).unwrap(),
                vec![0xFFFF, 0xFFFF]
            );
        }
    }

    /// f32 1.5 = 0x3FC00000. Low word 0x0000, high word 0x3FC0.
    #[test]
    fn encodes_f32_in_both_word_orders() {
        assert_eq!(
            encode_word_value(TagValue::F64(1.5), DataType::F32, WordOrder::LowHigh).unwrap(),
            vec![0x0000, 0x3FC0]
        );
        assert_eq!(
            encode_word_value(TagValue::F64(1.5), DataType::F32, WordOrder::HighLow).unwrap(),
            vec![0x3FC0, 0x0000]
        );
    }

    #[test]
    fn rejects_out_of_range_u16() {
        let err = encode_word_value(TagValue::F64(70000.0), DataType::U16, WordOrder::LowHigh)
            .unwrap_err();
        assert!(matches!(err, PlcWriteError::ValueOutOfRange { .. }));
    }

    #[test]
    fn rejects_non_integral_into_an_integer_type() {
        let err =
            encode_word_value(TagValue::F64(1.5), DataType::I16, WordOrder::LowHigh).unwrap_err();
        assert!(matches!(err, PlcWriteError::ValueOutOfRange { .. }));
    }

    #[test]
    fn rejects_negative_into_unsigned() {
        let err =
            encode_word_value(TagValue::F64(-1.0), DataType::U16, WordOrder::LowHigh).unwrap_err();
        assert!(matches!(err, PlcWriteError::ValueOutOfRange { .. }));
    }

    #[test]
    fn rejects_nan_and_infinity_into_an_integer_type() {
        for x in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(matches!(
                encode_word_value(TagValue::F64(x), DataType::I32, WordOrder::LowHigh),
                Err(PlcWriteError::ValueOutOfRange { .. })
            ));
        }
    }

    #[test]
    fn rejects_a_bit_value_at_a_numeric_type() {
        let err =
            encode_word_value(TagValue::Bit(true), DataType::U16, WordOrder::LowHigh).unwrap_err();
        assert!(matches!(err, PlcWriteError::ValueTypeMismatch { .. }));
    }

    #[test]
    fn encodes_bit_values() {
        assert!(encode_bit_value(TagValue::Bit(true)).unwrap());
        assert!(!encode_bit_value(TagValue::Bit(false)).unwrap());
    }

    #[test]
    fn rejects_a_numeric_value_at_a_bit_device() {
        let err = encode_bit_value(TagValue::F64(1.0)).unwrap_err();
        assert!(matches!(err, PlcWriteError::ValueTypeMismatch { .. }));
    }

    // --- encode_string_value (S1 文字列タグ) -------------------------------

    /// The load-bearing byte-order case: "AB" = [0x41, 0x42], low byte first
    /// within the word -> 0x4241 (NOT 0x4142) - the mirror of decode.rs's
    /// test of the same name.
    #[test]
    fn encodes_ascii_low_byte_first_within_each_word() {
        assert_eq!(
            encode_string_value("ABCD", 2).unwrap(),
            vec![0x4241, 0x4443]
        );
    }

    #[test]
    fn pads_the_remainder_of_the_span_with_nul() {
        // "ABC" = 3 bytes into a 4-word (8-byte) span: [0x41,0x42,0x43,0,0,0,0,0].
        assert_eq!(
            encode_string_value("ABC", 4).unwrap(),
            vec![0x4241, 0x0043, 0x0000, 0x0000]
        );
    }

    /// Multi-byte SJIS: "テスト" = [0x83, 0x65, 0x83, 0x58, 0x83, 0x67].
    #[test]
    fn encodes_multibyte_sjis() {
        assert_eq!(
            encode_string_value("テスト", 4).unwrap(),
            vec![0x6583, 0x5883, 0x6783, 0x0000]
        );
    }

    #[test]
    fn a_string_of_exactly_the_span_capacity_is_accepted_unpadded() {
        assert_eq!(encode_string_value("AB", 1).unwrap(), vec![0x4241]);
    }

    /// One byte over capacity is rejected outright - never truncated.
    #[test]
    fn rejects_a_string_longer_than_the_span_without_truncating() {
        let err = encode_string_value("ABC", 1).unwrap_err();
        match err {
            PlcWriteError::ValueOutOfRange {
                data_type, value, ..
            } => {
                assert_eq!(data_type, "string");
                assert_eq!(value, "ABC");
            }
            other => panic!("expected ValueOutOfRange, got {other:?}"),
        }
    }

    /// Multi-byte overflow: "テスト" is 6 SJIS bytes, over a 2-word span.
    #[test]
    fn rejects_multibyte_overflow() {
        assert!(matches!(
            encode_string_value("テスト", 2),
            Err(PlcWriteError::ValueOutOfRange { .. })
        ));
    }

    /// A character outside Shift-JIS is rejected rather than substituted
    /// (encoding_rs would otherwise emit an HTML numeric escape).
    #[test]
    fn rejects_characters_not_representable_in_shift_jis() {
        let err = encode_string_value("🚀", 8).unwrap_err();
        assert!(matches!(err, PlcWriteError::ValueOutOfRange { .. }));
    }

    #[test]
    fn empty_string_becomes_an_all_nul_span() {
        assert_eq!(encode_string_value("", 2).unwrap(), vec![0x0000, 0x0000]);
    }

    #[test]
    fn accepts_the_integer_boundary_values() {
        assert_eq!(
            encode_word_value(
                TagValue::F64(u16::MAX as f64),
                DataType::U16,
                WordOrder::LowHigh
            )
            .unwrap(),
            vec![0xFFFF]
        );
        assert_eq!(
            encode_word_value(
                TagValue::F64(i16::MIN as f64),
                DataType::I16,
                WordOrder::LowHigh
            )
            .unwrap(),
            vec![0x8000]
        );
        assert_eq!(
            encode_word_value(
                TagValue::F64(u32::MAX as f64),
                DataType::U32,
                WordOrder::HighLow
            )
            .unwrap(),
            vec![0xFFFF, 0xFFFF]
        );
    }
}
