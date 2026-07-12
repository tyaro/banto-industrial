//! PLC address parsing (docs/plan.md I2 §3): the instrumentation-standard
//! "reference number" notation - `0xxxx` = coil, `1xxxx` = discrete input,
//! `3xxxx` = input register, `4xxxx` = holding register, 1-based, 5 digits
//! (`40001` -> holding register offset 0) with a 6-digit extension
//! (`400001` onward) for offsets past `9999`.
//!
//! Deliberately a pure function with no PLC/IO dependency: `banto-tags`'s
//! `Tag::address` column is free-text precisely because this crate, not I1,
//! owns the format (see `crates/banto-tags/src/tag.rs`'s doc comment).
//! Callers building a [`crate::types::ReadRequest`] from a `Tag` row call
//! [`Address::parse`] once (e.g. when I3 loads a collection group's tags)
//! and turn a parse failure into their own `ReadResult::Bad` for that one
//! tag without ever handing it to [`crate::client::PlcClient::read_batch`].
//! This is what docs/plan.md I2 §3's "パース失敗は個別 ReadResult::Bad
//! （バッチは続行）" means in practice: `read_batch`'s hot path only ever
//! sees already-valid addresses, so parsing never has to happen per poll
//! cycle, only once when tag definitions are (re)loaded.

use crate::error::PlcError;

/// Which register/coil space an [`Address`] falls in, and (via
/// [`crate::planning`]) which Modbus function code reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AddressArea {
    /// `0xxxx` - read with FC1 (Read Coils).
    Coil,
    /// `1xxxx` - read with FC2 (Read Discrete Inputs).
    DiscreteInput,
    /// `3xxxx` - read with FC4 (Read Input Registers).
    InputRegister,
    /// `4xxxx` - read with FC3 (Read Holding Registers).
    HoldingRegister,
}

impl std::fmt::Display for AddressArea {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            AddressArea::Coil => "coil",
            AddressArea::DiscreteInput => "discrete_input",
            AddressArea::InputRegister => "input_register",
            AddressArea::HoldingRegister => "holding_register",
        };
        f.write_str(s)
    }
}

/// A parsed, protocol-ready PLC address: an area plus a 0-based offset
/// within it. `offset` is always a valid `u16` (0..=65535) because
/// [`Address::parse`] is the only constructor and it rejects anything that
/// would not fit (reference numbers above `xxxxx6` = offset 65536 do not
/// exist in Modbus's 16-bit address space).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Address {
    pub area: AddressArea,
    pub offset: u16,
}

impl Address {
    /// Parse instrumentation reference-number notation. Accepts leading/
    /// trailing whitespace (trimmed, matching how `banto-tags::TagInput`
    /// itself trims `address` before storing it - trimming twice is
    /// harmless). Everything else about the input must be exactly right:
    ///
    /// - exactly 5 or 6 ASCII digits, nothing else
    /// - first digit selects the area: `0`/`1`/`3`/`4` (`2` and `5-9` are not
    ///   reference-number prefixes and are rejected)
    /// - the remaining 4 (5-digit form) or 5 (6-digit form) digits are the
    ///   1-based number *within* that area; `0` is rejected (there is no
    ///   "number 0" in 1-based notation) and so is any number above `65536`
    ///   (the resulting 0-based offset would not fit in `u16`)
    pub fn parse(raw: &str) -> Result<Self, PlcError> {
        let trimmed = raw.trim();
        let invalid = || PlcError::InvalidAddress(raw.to_string());

        let len = trimmed.chars().count();
        if (len != 5 && len != 6) || !trimmed.chars().all(|c| c.is_ascii_digit()) {
            return Err(invalid());
        }

        let mut chars = trimmed.chars();
        let area = match chars.next().expect("len checked above, at least 5 chars") {
            '0' => AddressArea::Coil,
            '1' => AddressArea::DiscreteInput,
            '3' => AddressArea::InputRegister,
            '4' => AddressArea::HoldingRegister,
            _ => return Err(invalid()),
        };

        // Remaining 4 or 5 digits, as text so a leading zero (e.g. "40001"'s
        // "0001") parses as plain 1 rather than being rejected as octal or
        // similar - `str::parse::<u32>` handles this fine either way.
        let number: u32 = chars.as_str().parse().map_err(|_| invalid())?;
        if number == 0 || number > 65_536 {
            return Err(invalid());
        }

        // Safe: 1 <= number <= 65_536, so 0 <= number - 1 <= 65_535 = u16::MAX.
        let offset = (number - 1) as u16;
        Ok(Address { area, offset })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_area_prefix() {
        assert_eq!(
            Address::parse("00001").unwrap(),
            Address {
                area: AddressArea::Coil,
                offset: 0
            }
        );
        assert_eq!(
            Address::parse("10001").unwrap(),
            Address {
                area: AddressArea::DiscreteInput,
                offset: 0
            }
        );
        assert_eq!(
            Address::parse("30001").unwrap(),
            Address {
                area: AddressArea::InputRegister,
                offset: 0
            }
        );
        assert_eq!(
            Address::parse("40001").unwrap(),
            Address {
                area: AddressArea::HoldingRegister,
                offset: 0
            }
        );
    }

    #[test]
    fn five_digit_offset_is_one_based() {
        // 40001 -> offset 0 (design doc's worked example).
        assert_eq!(Address::parse("40001").unwrap().offset, 0);
        assert_eq!(Address::parse("40010").unwrap().offset, 9);
        assert_eq!(Address::parse("49999").unwrap().offset, 9998);
    }

    #[test]
    fn six_digit_extended_form_reaches_beyond_9999() {
        assert_eq!(Address::parse("400001").unwrap().offset, 0);
        assert_eq!(Address::parse("410000").unwrap().offset, 9999);
        assert_eq!(Address::parse("465536").unwrap().offset, 65_535);
    }

    #[test]
    fn rejects_number_zero() {
        assert!(matches!(
            Address::parse("40000"),
            Err(PlcError::InvalidAddress(_))
        ));
        assert!(matches!(
            Address::parse("400000"),
            Err(PlcError::InvalidAddress(_))
        ));
    }

    #[test]
    fn rejects_six_digit_overflow_past_65536() {
        // 465537 -> offset 65536, does not fit in u16.
        assert!(matches!(
            Address::parse("465537"),
            Err(PlcError::InvalidAddress(_))
        ));
    }

    #[test]
    fn rejects_unknown_area_prefix() {
        for prefix in ['2', '5', '6', '7', '8', '9'] {
            let s = format!("{prefix}0001");
            assert!(
                matches!(Address::parse(&s), Err(PlcError::InvalidAddress(_))),
                "prefix {prefix} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_wrong_length() {
        assert!(matches!(
            Address::parse("4001"),
            Err(PlcError::InvalidAddress(_))
        ));
        assert!(matches!(
            Address::parse("4000001"),
            Err(PlcError::InvalidAddress(_))
        ));
    }

    #[test]
    fn rejects_non_digit_characters() {
        assert!(matches!(
            Address::parse("4000A"),
            Err(PlcError::InvalidAddress(_))
        ));
        assert!(matches!(
            Address::parse("D40001"),
            Err(PlcError::InvalidAddress(_))
        ));
    }

    #[test]
    fn rejects_empty_and_whitespace_only() {
        assert!(matches!(
            Address::parse(""),
            Err(PlcError::InvalidAddress(_))
        ));
        assert!(matches!(
            Address::parse("   "),
            Err(PlcError::InvalidAddress(_))
        ));
    }

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(
            Address::parse("  40001  ").unwrap(),
            Address {
                area: AddressArea::HoldingRegister,
                offset: 0
            }
        );
    }

    #[test]
    fn invalid_address_error_echoes_the_original_untrimmed_text() {
        // Easier to debug a bad tag definition when the error repeats
        // exactly what was configured, not a silently-trimmed variant.
        let err = Address::parse("  bogus  ").unwrap_err();
        match err {
            PlcError::InvalidAddress(text) => assert_eq!(text, "  bogus  "),
            other => panic!("expected InvalidAddress, got {other:?}"),
        }
    }
}
