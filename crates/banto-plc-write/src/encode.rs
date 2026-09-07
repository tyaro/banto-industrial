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
use crate::StringEncoding;

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

/// Split a `u64` into its four register words per `order` - the exact inverse
/// of `banto-plc/src/decode.rs`'s `combine_u64` (64-bit types, owner decision
/// 2026-09-08), which reads `HighLow => regs` verbatim as `[a, b, c, d]` (`a`
/// most significant) and `LowHigh => [regs[3], regs[2], regs[1], regs[0]]`
/// reversed to `[a, b, c, d]`. So for `HighLow` the wire order is `[a, b, c,
/// d]` directly, and for `LowHigh` it is the reverse, `[d, c, b, a]` - the
/// same `HighLow`/`LowHigh` shape as [`split_u32`], extended to four words.
fn split_u64(val: u64, order: WordOrder) -> [u16; 4] {
    let a = (val >> 48) as u16;
    let b = (val >> 32) as u16;
    let c = (val >> 16) as u16;
    let d = val as u16;
    match order {
        WordOrder::HighLow => [a, b, c, d],
        WordOrder::LowHigh => [d, c, b, a],
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
/// non-integral, or outside `[lo, hi]`. `lo`/`hi` are passed as `f64` and must
/// themselves be exactly representable as `f64` - true of every 16-/32-bit
/// integer bound (`i16`/`u16`/`i32`/`u32` `MIN`/`MAX`), which is all this
/// helper is used for. **Not used for the 64-bit integer types** (`I64`/
/// `U64`): `i64::MAX as f64` and `u64::MAX as f64` are *not* exactly
/// representable (they round up to `2^63` and `2^64` respectively, both
/// one-past the real bound), so passing them here as `hi` would silently
/// accept an out-of-range value. See [`require_u64_in_range`] and
/// [`require_i64_in_range`], which handle that case explicitly.
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

/// Reject a value that cannot land exactly in a `u64` register group:
/// non-finite, non-integral, negative, or `>= 2^64`.
///
/// Deliberately **not** built on [`require_integral_in_range`]: that helper
/// compares against an inclusive `hi` bound that must itself be exactly
/// representable as `f64`, which holds for `u16::MAX`/`u32::MAX` but not for
/// `u64::MAX`. `u64::MAX` (`2^64 - 1`) sits between two `f64` values that are
/// `2048` apart at that magnitude, and rounds *up* to the nearer one -
/// `u64::MAX as f64` is exactly `2^64`, one past every real `u64` value.
/// Using that as an inclusive upper bound would wrongly accept `x == 2^64`
/// (which does not fit in a `u64` - `2^64_f64 as u64` silently saturates to
/// `u64::MAX` under Rust's saturating float-to-int cast rather than
/// panicking or erroring).
///
/// The fix is to compare against `2^64` itself with a **strict** `<`: every
/// `f64` integer strictly below `2^64` does fit in a `u64` once truncated
/// (there is no representable `f64` between the true `u64::MAX` and `2^64`
/// to slip through), so this correctly accepts the full range and rejects
/// only what is genuinely out of bounds.
fn require_u64_in_range(x: f64, data_type: DataType) -> Result<(), PlcWriteError> {
    // 2^64, exactly representable as f64 - see doc comment above.
    const TWO_POW_64: f64 = 18_446_744_073_709_551_616.0;
    let bad = |detail: String| {
        Err(PlcWriteError::ValueOutOfRange {
            data_type: data_type.to_string(),
            value: format!("{x}"),
            detail,
        })
    };
    if !x.is_finite() {
        return bad("値が有限ではありません".to_string());
    }
    if x.fract() != 0.0 {
        return bad("整数ではありません".to_string());
    }
    if x < 0.0 || x >= TWO_POW_64 {
        return bad(format!("範囲 [0, {}] の外です", u64::MAX));
    }
    Ok(())
}

/// Reject a value that cannot land exactly in an `i64` register group:
/// non-finite, non-integral, or outside `i64`'s true range.
///
/// Same pitfall as [`require_u64_in_range`], mirrored on the signed side:
/// `i64::MIN` (`-2^63`) *is* exactly representable as `f64`, so the lower
/// bound can safely be the plain inclusive `i64::MIN as f64`. But
/// `i64::MAX` (`2^63 - 1`) is not - it rounds *up* to `2^63`, one past every
/// real `i64` value - so the upper bound is instead a strict `< 2^63`. As
/// with the `u64` case, no representable `f64` integer falls strictly
/// between the true `i64::MAX` and `2^63`, so this is exact: every `f64`
/// integer in `[-2^63, 2^63)` fits in an `i64` once truncated.
fn require_i64_in_range(x: f64, data_type: DataType) -> Result<(), PlcWriteError> {
    // 2^63, exactly representable as f64 - see doc comment above. i64::MIN as
    // f64 is exactly -2^63, so it is used directly as the lower bound.
    const TWO_POW_63: f64 = 9_223_372_036_854_775_808.0;
    let bad = |detail: String| {
        Err(PlcWriteError::ValueOutOfRange {
            data_type: data_type.to_string(),
            value: format!("{x}"),
            detail,
        })
    };
    if !x.is_finite() {
        return bad("値が有限ではありません".to_string());
    }
    if x.fract() != 0.0 {
        return bad("整数ではありません".to_string());
    }
    if x < i64::MIN as f64 || x >= TWO_POW_63 {
        return bad(format!("範囲 [{}, {}] の外です", i64::MIN, i64::MAX));
    }
    Ok(())
}

/// Encode a value bound for a **word** device into its register window (1 word
/// for 16-bit types, 2 for 32-bit, 4 for 64-bit (`I64`/`U64`/`F64`, owner
/// decision 2026-09-08), ordered per `order`). `data_type` is guaranteed
/// non-`Bit` by the planner's compatibility check before this is called; a
/// `Bit` here would be a planner bug and is reported as a value error rather
/// than panicking.
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
        DataType::U64 => {
            require_u64_in_range(x, data_type)?;
            split_u64(x as u64, order).to_vec()
        }
        DataType::I64 => {
            require_i64_in_range(x, data_type)?;
            split_u64((x as i64) as u64, order).to_vec()
        }
        DataType::F64 => {
            // f64 -> f64 is lossless (unlike the F32 arm above): the only
            // rejection is non-finite input (NaN/Inf), which `to_bits` would
            // otherwise happily encode onto the wire as a well-formed but
            // meaningless bit pattern. Unlike the integer 64-bit arms, no
            // integral check applies - F64 is the one 64-bit type meant to
            // carry fractional values.
            if !x.is_finite() {
                return Err(PlcWriteError::ValueOutOfRange {
                    data_type: data_type.to_string(),
                    value: format!("{x}"),
                    detail: "値が有限ではありません".to_string(),
                });
            }
            split_u64(x.to_bits(), order).to_vec()
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

/// Encode a string bound for a `words`-word span of a word device (S1/T20 ①a
/// 文字列タグ): `encoding`-chosen bytes (UTF-8 or Shift-JIS,
/// [`StringEncoding`]), 0x00-padded to exactly `2 * words` bytes, packed **low
/// byte first** into each word - the exact inverse of
/// `banto-plc/src/decode.rs::decode_string_value` (see there for the wire
/// evidence from the wrapped `slmp` crate), so a write→read round trip is
/// byte-for-byte **when the reader decodes with the same encoding** (T20 ①a
/// keeps the recorder's read path Shift-JIS-only by design -
/// docs/banto-hub-t20-design.md §3.1's 案A - so a UTF-8-written tag round-trips
/// only through this crate's own read helpers, not the recorder). Note
/// [`banto_plc::WordOrder`] plays no part here: it orders the two words of a
/// 32-bit *numeric* value, whereas a string's byte order within each word is
/// fixed by MELSEC's storage convention regardless of character encoding.
///
/// Two rejections, both per-request `Bad`s and both about never mangling text
/// onto a live PLC:
/// - a character with no representation in `encoding`
///   ([`PlcWriteError::ValueOutOfRange`]) rather than encoding_rs's HTML
///   escape substitution - **UTF-8 can represent every `char` a Rust `&str`
///   can hold, so this branch is unreachable for [`StringEncoding::Utf8`]**
///   and only ever fires for [`StringEncoding::ShiftJis`]; it is still
///   checked uniformly for both rather than special-cased away, so this
///   function's contract does not depend on that fact holding forever.
/// - encoded bytes longer than the span's `2 * words` capacity
///   ([`PlcWriteError::ValueOutOfRange`]) rather than silent truncation - a
///   cut-off recipe string is a real hazard
pub fn encode_string_value(
    value: &str,
    words: u16,
    encoding: StringEncoding,
) -> Result<Vec<u16>, PlcWriteError> {
    let (table, label) = match encoding {
        StringEncoding::Utf8 => (encoding_rs::UTF_8, "UTF-8"),
        StringEncoding::ShiftJis => (encoding_rs::SHIFT_JIS, "Shift-JIS"),
    };
    let (bytes, _, had_errors) = table.encode(value);
    if had_errors {
        return Err(PlcWriteError::ValueOutOfRange {
            data_type: "string".to_string(),
            value: value.to_string(),
            detail: format!("{label} で表現できない文字を含みます"),
        });
    }
    let capacity = words as usize * 2;
    if bytes.len() > capacity {
        return Err(PlcWriteError::ValueOutOfRange {
            data_type: "string".to_string(),
            value: value.to_string(),
            detail: format!(
                "{label} で {} バイトになり、{words} 語（{capacity} バイト）に収まりません",
                bytes.len()
            ),
        });
    }

    let mut padded = bytes.into_owned();
    padded.resize(capacity, 0x00);
    Ok(padded
        .as_chunks::<2>()
        .0
        .iter()
        .map(|chunk| u16::from_le_bytes(*chunk))
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

    // --- encode_string_value (S1/T20 ①a 文字列タグ) ------------------------
    //
    // Every pre-T20 test below passes `StringEncoding::ShiftJis` explicitly -
    // this is the load-bearing proof that relay-wright's existing (Shift-JIS
    // only) behaviour is exactly reproduced by naming the encoding rather than
    // relying on any default. The `_utf8` block further down is the new T20
    // ①a coverage.

    /// The load-bearing byte-order case: "AB" = [0x41, 0x42], low byte first
    /// within the word -> 0x4241 (NOT 0x4142) - the mirror of decode.rs's
    /// test of the same name.
    #[test]
    fn encodes_ascii_low_byte_first_within_each_word() {
        assert_eq!(
            encode_string_value("ABCD", 2, StringEncoding::ShiftJis).unwrap(),
            vec![0x4241, 0x4443]
        );
    }

    #[test]
    fn pads_the_remainder_of_the_span_with_nul() {
        // "ABC" = 3 bytes into a 4-word (8-byte) span: [0x41,0x42,0x43,0,0,0,0,0].
        assert_eq!(
            encode_string_value("ABC", 4, StringEncoding::ShiftJis).unwrap(),
            vec![0x4241, 0x0043, 0x0000, 0x0000]
        );
    }

    /// Multi-byte SJIS: "テスト" = [0x83, 0x65, 0x83, 0x58, 0x83, 0x67].
    #[test]
    fn encodes_multibyte_sjis() {
        assert_eq!(
            encode_string_value("テスト", 4, StringEncoding::ShiftJis).unwrap(),
            vec![0x6583, 0x5883, 0x6783, 0x0000]
        );
    }

    #[test]
    fn a_string_of_exactly_the_span_capacity_is_accepted_unpadded() {
        assert_eq!(
            encode_string_value("AB", 1, StringEncoding::ShiftJis).unwrap(),
            vec![0x4241]
        );
    }

    /// One byte over capacity is rejected outright - never truncated.
    #[test]
    fn rejects_a_string_longer_than_the_span_without_truncating() {
        let err = encode_string_value("ABC", 1, StringEncoding::ShiftJis).unwrap_err();
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
            encode_string_value("テスト", 2, StringEncoding::ShiftJis),
            Err(PlcWriteError::ValueOutOfRange { .. })
        ));
    }

    /// A character outside Shift-JIS is rejected rather than substituted
    /// (encoding_rs would otherwise emit an HTML numeric escape).
    #[test]
    fn rejects_characters_not_representable_in_shift_jis() {
        let err = encode_string_value("🚀", 8, StringEncoding::ShiftJis).unwrap_err();
        assert!(matches!(err, PlcWriteError::ValueOutOfRange { .. }));
    }

    #[test]
    fn empty_string_becomes_an_all_nul_span() {
        assert_eq!(
            encode_string_value("", 2, StringEncoding::ShiftJis).unwrap(),
            vec![0x0000, 0x0000]
        );
    }

    // --- encode_string_value: StringEncoding::Utf8 (T20 ①a, new) ----------

    /// ASCII round-trips identically in UTF-8 and Shift-JIS (both are ASCII
    /// supersets), so the byte-order convention (low byte first per word)
    /// carries over unchanged from the Shift-JIS test of the same shape.
    #[test]
    fn utf8_encodes_ascii_low_byte_first_within_each_word() {
        assert_eq!(
            encode_string_value("ABCD", 2, StringEncoding::Utf8).unwrap(),
            vec![0x4241, 0x4443]
        );
    }

    /// Multi-byte UTF-8: "テスト" is 9 bytes in UTF-8 (3 bytes/char) - unlike
    /// Shift-JIS's 6 bytes for the same text, so this is also the case that
    /// proves UTF-8 and Shift-JIS are not silently interchangeable spans.
    #[test]
    fn utf8_encodes_multibyte_japanese_text() {
        let encoded = encode_string_value("テスト", 5, StringEncoding::Utf8).unwrap();
        // 9 UTF-8 bytes padded to 10 (5 words), NUL-padded last byte.
        let bytes: Vec<u8> = encoded.iter().flat_map(|w| w.to_le_bytes()).collect();
        assert_eq!(bytes, {
            let mut expected = "テスト".as_bytes().to_vec();
            expected.push(0x00);
            expected
        });
    }

    /// UTF-8 can represent every Unicode scalar value, including ones with no
    /// Shift-JIS mapping (e.g. an emoji) - the mirror of
    /// `rejects_characters_not_representable_in_shift_jis` showing the same
    /// input now succeeds under `StringEncoding::Utf8`.
    #[test]
    fn utf8_accepts_characters_shift_jis_cannot_represent() {
        assert!(encode_string_value("🚀", 8, StringEncoding::Utf8).is_ok());
    }

    /// Same overflow/no-truncation guarantee as Shift-JIS's
    /// `rejects_a_string_longer_than_the_span_without_truncating`, exercised
    /// under UTF-8.
    #[test]
    fn utf8_rejects_a_string_longer_than_the_span_without_truncating() {
        let err = encode_string_value("ABC", 1, StringEncoding::Utf8).unwrap_err();
        assert!(matches!(err, PlcWriteError::ValueOutOfRange { .. }));
    }

    #[test]
    fn utf8_empty_string_becomes_an_all_nul_span() {
        assert_eq!(
            encode_string_value("", 2, StringEncoding::Utf8).unwrap(),
            vec![0x0000, 0x0000]
        );
    }

    /// Round-trip proof at the byte level: encode then manually decode (low
    /// byte first per word, per this function's doc comment) reproduces the
    /// original text - the write-side half of the "UTF-8 round-trips through
    /// this crate's own read helpers" claim in `encode_string_value`'s doc
    /// comment.
    #[test]
    fn utf8_round_trips_through_manual_byte_level_decode() {
        let text = "Recipe #1: テスト";
        let words = 12u16;
        let encoded = encode_string_value(text, words, StringEncoding::Utf8).unwrap();
        let mut bytes: Vec<u8> = encoded.iter().flat_map(|w| w.to_le_bytes()).collect();
        // Trim the NUL padding the same way a decoder would.
        while bytes.last() == Some(&0) {
            bytes.pop();
        }
        assert_eq!(std::str::from_utf8(&bytes).unwrap(), text);
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

    // --- 64-bit types (I64/U64/F64, owner decision 2026-09-08) ------------

    /// Mirror of `decode.rs`'s `combine_u64`, reimplemented locally rather
    /// than called across the crate boundary (it is `pub(crate)` to
    /// `banto-plc`) - used only to prove `split_u64`'s round trip below.
    fn recombine_u64(regs: [u16; 4], order: WordOrder) -> u64 {
        let [a, b, c, d] = match order {
            WordOrder::HighLow => regs,
            WordOrder::LowHigh => [regs[3], regs[2], regs[1], regs[0]],
        };
        ((a as u64) << 48) | ((b as u64) << 32) | ((c as u64) << 16) | (d as u64)
    }

    /// u64 value 0x0001_0002_0003_0004 as four registers, both word orders,
    /// exact words spelled out (same style as `encodes_u32_in_both_word_orders`).
    #[test]
    fn encodes_u64_in_both_word_orders() {
        assert_eq!(
            encode_word_value(
                TagValue::F64(0x0001_0002_0003_0004u64 as f64),
                DataType::U64,
                WordOrder::HighLow
            )
            .unwrap(),
            vec![0x0001, 0x0002, 0x0003, 0x0004]
        );
        assert_eq!(
            encode_word_value(
                TagValue::F64(0x0001_0002_0003_0004u64 as f64),
                DataType::U64,
                WordOrder::LowHigh
            )
            .unwrap(),
            vec![0x0004, 0x0003, 0x0002, 0x0001]
        );
    }

    /// -1 as i64 = 0xFFFF_FFFF_FFFF_FFFF, symmetric, so all four words are
    /// 0xFFFF either way - the 64-bit twin of `encodes_i32_negative_same_bytes_in_both_orders`.
    #[test]
    fn encodes_i64_negative_same_bytes_in_both_orders() {
        for order in [WordOrder::HighLow, WordOrder::LowHigh] {
            assert_eq!(
                encode_word_value(TagValue::F64(-1.0), DataType::I64, order).unwrap(),
                vec![0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF]
            );
        }
    }

    /// f64 1.5 = 0x3FF8_0000_0000_0000 (IEEE 754), both word orders - the
    /// 64-bit twin of `encodes_f32_in_both_word_orders`.
    #[test]
    fn encodes_f64_in_both_word_orders() {
        assert_eq!(
            encode_word_value(TagValue::F64(1.5), DataType::F64, WordOrder::HighLow).unwrap(),
            vec![0x3FF8, 0x0000, 0x0000, 0x0000]
        );
        assert_eq!(
            encode_word_value(TagValue::F64(1.5), DataType::F64, WordOrder::LowHigh).unwrap(),
            vec![0x0000, 0x0000, 0x0000, 0x3FF8]
        );
    }

    /// Round trip through `split_u64` + a locally reimplemented `combine_u64`
    /// (the real one is `pub(crate)` to `banto-plc` and not reachable from
    /// here) for a representative set of `f64` values, both word orders -
    /// proves `split_u64` is the exact inverse the module doc comment claims.
    #[test]
    fn f64_round_trips_through_split_u64_both_word_orders() {
        for value in [
            0.0_f64,
            1.5,
            -1.5,
            100.5,
            1234.5678,
            f64::MIN_POSITIVE,
            f64::MAX,
            f64::MIN,
        ] {
            for order in [WordOrder::HighLow, WordOrder::LowHigh] {
                let regs = split_u64(value.to_bits(), order);
                let bits = recombine_u64(regs, order);
                assert_eq!(
                    f64::from_bits(bits),
                    value,
                    "round trip failed for {value} under {order:?}"
                );
            }
        }
    }

    /// Same round-trip proof for `u64`/`i64` integral values via
    /// `encode_word_value` itself (rather than `split_u64` directly), so the
    /// range-checking arms are exercised too.
    #[test]
    fn u64_and_i64_round_trip_through_encode_word_value_both_word_orders() {
        // 2^64 - 2048 / 2^63 - 1024: the largest u64/i64 values that are
        // *exactly* representable as f64 at their magnitude (spacing 2048/1024
        // respectively - see `require_u64_in_range`'s doc comment). Plain
        // `u64::MAX - 2048` or `i64::MAX - 1024` would NOT be exactly
        // representable (u64::MAX/i64::MAX are themselves odd, off the 2048/
        // 1024 grid), so `value as f64` would silently round to a different
        // value before ever reaching `encode_word_value` - the wrong thing to
        // assert a round trip against.
        for order in [WordOrder::HighLow, WordOrder::LowHigh] {
            for value in [
                0u64,
                1,
                42,
                18_446_744_073_709_549_568,
                0x0001_0002_0003_0004,
            ] {
                let regs =
                    encode_word_value(TagValue::F64(value as f64), DataType::U64, order).unwrap();
                let bits = recombine_u64(regs.try_into().unwrap(), order);
                assert_eq!(bits, value);
            }
            for value in [0i64, -1, i64::MIN, 9_223_372_036_854_774_784, -1234567] {
                let regs =
                    encode_word_value(TagValue::F64(value as f64), DataType::I64, order).unwrap();
                let bits = recombine_u64(regs.try_into().unwrap(), order);
                assert_eq!(bits as i64, value);
            }
        }
    }

    #[test]
    fn rejects_negative_into_u64() {
        let err =
            encode_word_value(TagValue::F64(-1.0), DataType::U64, WordOrder::HighLow).unwrap_err();
        assert!(matches!(err, PlcWriteError::ValueOutOfRange { .. }));
    }

    #[test]
    fn rejects_non_integral_into_u64() {
        let err =
            encode_word_value(TagValue::F64(1.5), DataType::U64, WordOrder::HighLow).unwrap_err();
        assert!(matches!(err, PlcWriteError::ValueOutOfRange { .. }));
    }

    /// The load-bearing 64-bit boundary case (see `require_u64_in_range`'s doc
    /// comment): `u64::MAX as f64` is *not* an exact representation of
    /// `u64::MAX` - it rounds up to `2^64`, one past every real `u64` value -
    /// so it must be rejected, not silently accepted or saturated.
    #[test]
    fn rejects_u64_max_as_f64_because_it_rounds_up_to_two_pow_64() {
        let err = encode_word_value(
            TagValue::F64(u64::MAX as f64),
            DataType::U64,
            WordOrder::HighLow,
        )
        .unwrap_err();
        assert!(matches!(err, PlcWriteError::ValueOutOfRange { .. }));
    }

    #[test]
    fn rejects_two_pow_64_into_u64() {
        let err = encode_word_value(
            TagValue::F64(18_446_744_073_709_551_616.0), // 2^64
            DataType::U64,
            WordOrder::HighLow,
        )
        .unwrap_err();
        assert!(matches!(err, PlcWriteError::ValueOutOfRange { .. }));
    }

    /// The largest `f64` integer strictly below `2^64` (`2^64 - 2048`, the
    /// representable spacing at that magnitude) is a genuinely valid `u64`
    /// and must be accepted - the boundary just inside the range that
    /// `rejects_u64_max_as_f64_because_it_rounds_up_to_two_pow_64` sits just
    /// outside of.
    #[test]
    fn accepts_the_largest_representable_f64_below_two_pow_64() {
        // 2^64 - 2048: the largest u64 value exactly representable as f64 (the
        // f64 spacing at this magnitude is 2048) - NOT `u64::MAX - 2048`,
        // which is off the representable grid (u64::MAX is odd).
        let value: u64 = 18_446_744_073_709_549_568;
        assert_eq!(value as f64 as u64, value); // sanity: exactly representable
        assert!(encode_word_value(
            TagValue::F64(value as f64),
            DataType::U64,
            WordOrder::HighLow
        )
        .is_ok());
    }

    #[test]
    fn rejects_i64_out_of_range() {
        for x in [
            i64::MIN as f64 - 1.0e10,    // well below i64::MIN
            9_223_372_036_854_775_808.0, // 2^63, one past i64::MAX
        ] {
            let err =
                encode_word_value(TagValue::F64(x), DataType::I64, WordOrder::HighLow).unwrap_err();
            assert!(matches!(err, PlcWriteError::ValueOutOfRange { .. }));
        }
    }

    /// The signed twin of `rejects_u64_max_as_f64_because_it_rounds_up_to_two_pow_64`:
    /// `i64::MAX as f64` rounds up to `2^63`, one past every real `i64`
    /// value, so it must be rejected.
    #[test]
    fn rejects_i64_max_as_f64_because_it_rounds_up_to_two_pow_63() {
        let err = encode_word_value(
            TagValue::F64(i64::MAX as f64),
            DataType::I64,
            WordOrder::HighLow,
        )
        .unwrap_err();
        assert!(matches!(err, PlcWriteError::ValueOutOfRange { .. }));
    }

    /// `i64::MIN` (`-2^63`) *is* exactly representable as `f64`, unlike
    /// `i64::MAX` - this must be accepted at the boundary.
    #[test]
    fn accepts_i64_min_boundary_exactly() {
        assert!(encode_word_value(
            TagValue::F64(i64::MIN as f64),
            DataType::I64,
            WordOrder::HighLow
        )
        .is_ok());
    }

    #[test]
    fn rejects_nan_and_infinity_into_f64() {
        for x in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let err =
                encode_word_value(TagValue::F64(x), DataType::F64, WordOrder::HighLow).unwrap_err();
            assert!(matches!(err, PlcWriteError::ValueOutOfRange { .. }));
        }
    }

    /// Unlike the integer 64-bit types, `F64` must accept non-integral
    /// values - that is the entire point of the type.
    #[test]
    fn f64_accepts_non_integral_values() {
        assert!(encode_word_value(TagValue::F64(1.5), DataType::F64, WordOrder::HighLow).is_ok());
        assert!(
            encode_word_value(TagValue::F64(1234.5678), DataType::F64, WordOrder::LowHigh).is_ok()
        );
    }
}
