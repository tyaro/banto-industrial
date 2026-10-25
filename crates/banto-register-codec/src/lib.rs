//! Pure numeric decoding from normalized 16-bit register words.
//!
//! Wire byte order, address validation, register bounds, and presentation or
//! storage formats belong to callers. The order of 16-bit words must always be
//! supplied explicitly; `LowHigh` reverses all four words for 64-bit values.

#![no_std]

/// Order of 16-bit words within one numeric value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WordOrder {
    HighLow,
    LowHigh,
}

/// Interpret one 16-bit word as a signed integer.
pub const fn decode_i16(word: u16) -> i16 {
    word as i16
}

/// Join two 16-bit words into an exact unsigned integer.
pub const fn decode_u32(words: [u16; 2], order: WordOrder) -> u32 {
    let (high, low) = match order {
        WordOrder::HighLow => (words[0], words[1]),
        WordOrder::LowHigh => (words[1], words[0]),
    };
    ((high as u32) << 16) | (low as u32)
}

/// Interpret two 16-bit words as a signed integer.
pub const fn decode_i32(words: [u16; 2], order: WordOrder) -> i32 {
    decode_u32(words, order) as i32
}

/// Restore an IEEE 754 binary32 value from two 16-bit words.
pub const fn decode_f32(words: [u16; 2], order: WordOrder) -> f32 {
    f32::from_bits(decode_u32(words, order))
}

/// Join four 16-bit words into an exact unsigned integer.
pub const fn decode_u64(words: [u16; 4], order: WordOrder) -> u64 {
    let [a, b, c, d] = match order {
        WordOrder::HighLow => words,
        WordOrder::LowHigh => [words[3], words[2], words[1], words[0]],
    };
    ((a as u64) << 48) | ((b as u64) << 32) | ((c as u64) << 16) | (d as u64)
}

/// Interpret four 16-bit words as a signed integer.
pub const fn decode_i64(words: [u16; 4], order: WordOrder) -> i64 {
    decode_u64(words, order) as i64
}

/// Restore an IEEE 754 binary64 value from four 16-bit words.
pub const fn decode_f64(words: [u16; 4], order: WordOrder) -> f64 {
    f64::from_bits(decode_u64(words, order))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_integer_bits_survive_both_word_orders() {
        assert_eq!(decode_i16(0x8000), i16::MIN);
        for (words, order) in [
            ([0x1234, 0xabcd], WordOrder::HighLow),
            ([0xabcd, 0x1234], WordOrder::LowHigh),
        ] {
            assert_eq!(decode_u32(words, order), 0x1234_abcd);
        }
        assert_eq!(decode_i32([0x8000, 0], WordOrder::HighLow), i32::MIN);
        for (words, order) in [
            ([0x0020, 0, 0, 1], WordOrder::HighLow),
            ([1, 0, 0, 0x0020], WordOrder::LowHigh),
        ] {
            assert_eq!(decode_u64(words, order), 9_007_199_254_740_993);
        }
        assert_eq!(decode_i64([0x8000, 0, 0, 0], WordOrder::HighLow), i64::MIN);
    }

    #[test]
    fn ieee_bits_survive_both_word_orders() {
        for (words, order) in [
            ([0x3fc0, 0], WordOrder::HighLow),
            ([0, 0x3fc0], WordOrder::LowHigh),
        ] {
            assert_eq!(decode_f32(words, order).to_bits(), 0x3fc0_0000);
        }
        for (words, order) in [
            ([0x3ff8, 0, 0, 0], WordOrder::HighLow),
            ([0, 0, 0, 0x3ff8], WordOrder::LowHigh),
        ] {
            assert_eq!(decode_f64(words, order).to_bits(), 0x3ff8_0000_0000_0000);
        }
    }
}
