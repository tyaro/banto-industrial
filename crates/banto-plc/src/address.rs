//! PLC address parsing (docs/plan.md I2 §3): one [`Address`] type covering
//! every notation this crate's protocol implementations understand.
//!
//! - **Modbus reference numbers** ([`Address::parse`]) - the
//!   instrumentation-standard notation: `0xxxx` = coil, `1xxxx` = discrete
//!   input, `3xxxx` = input register, `4xxxx` = holding register, 1-based,
//!   5 digits (`40001` -> holding register offset 0) with a 6-digit extension
//!   (`400001` onward) for offsets past `9999`.
//! - **MELSEC device codes** ([`Address::parse_slmp`], I2a) - `D100`, `M50`,
//!   `X1A`, `ZR0`. Parsed by [`crate::slmp::address`], which also owns the
//!   bit-vs-word and decimal-vs-hex rules that notation needs.
//! - **Bit-in-word notation** (T8, docs/tag-server-design.md §6.1) - a single
//!   bit of a *word* device/area, named as a `.N` suffix on either notation
//!   above: `D100.5` (SLMP) or `40001.3` (Modbus holding register), `N` in
//!   `0..=15`, always decimal (see [`crate::slmp::address::parse`]'s doc
//!   comment for why, even at a hex-numbered SLMP device). Rejected outright
//!   on a device/area that is already bit-granular (`M50.0`, `00001.0`) -
//!   see [`Address::ModbusRef`]/[`Address::Slmp`]'s field docs. This is
//!   deliberately an *address*-level notation, not a tag-name suffix: a tag
//!   still gets one ordinary `data_type = "bit"` row in `banto-tags`, so it
//!   appears in catalog listings exactly like any other tag (§6.1's decision
//!   against a `"hoge.0"`-style tag-name suffix, which would create a
//!   derived name catalog never lists - see §4.1's "catalog is a binding
//!   contract" and its rejection of FA-Server's アクティブタグ for the same
//!   reason).
//!
//! Deliberately pure functions with no PLC/IO dependency: `banto-tags`'s
//! `Tag::address` column is free-text precisely because this crate, not I1,
//! owns the format (see `crates/banto-tags/src/tag.rs`'s doc comment).
//! Callers building a [`crate::types::ReadRequest`] from a `Tag` row call
//! whichever parser matches the `PlcConnection`'s protocol once (e.g. when I3
//! loads a collection group's tags) and turn a parse failure into their own
//! `ReadResult::Bad` for that one tag without ever handing it to
//! [`crate::client::PlcClient::read_batch`]. This is what docs/plan.md I2 §3's
//! "パース失敗は個別 ReadResult::Bad（バッチは続行）" means in practice:
//! `read_batch`'s hot path only ever sees already-valid addresses, so parsing
//! never has to happen per poll cycle, only once when tag definitions are
//! (re)loaded.
//!
//! ## Why one sum type instead of one address type per protocol
//!
//! [`Address`] became an enum in I2a rather than staying Modbus-only, and the
//! alternative - a generic parameter or an associated `Addr` type on
//! [`crate::client::PlcClient`] - was rejected for the same reason the trait
//! is hand-boxed to be `dyn`-compatible (see `client.rs`): I3 holds a
//! `Vec<Box<dyn PlcClient>>` of mixed protocols, and a per-protocol address
//! type would make that impossible without re-introducing an enum somewhere
//! less convenient. Keeping the enum here means [`crate::types::ReadRequest`]
//! stays one `Copy` struct, and each protocol implementation resolves the
//! variant it cannot serve into a per-request
//! [`PlcError::AddressProtocolMismatch`] the same way it already resolves an
//! area/data-type mismatch - a configuration error surfaced per tag, never a
//! whole dead batch.

use crate::error::PlcError;
use crate::slmp::address::{self as slmp_address, SlmpDevice};

/// Which register/coil space an [`Address`] falls in, and (via
/// [`crate::planning`]) which Modbus function code reads it.
///
/// T19 S1-b0 (2026-09-02): defined in the dependency-free `banto-plc-address`
/// crate, not here - `banto-tags` (I1) needs this crate's "is this area
/// writable" fact (`is_writable()`) without taking on this crate's tokio/
/// slmp/encoding_rs dependency stack just to get it, and `banto-plc-write`
/// (I5) shares the same definition rather than re-deriving it a third time.
/// Re-exported here so every existing `banto_plc::AddressArea`/
/// `banto_plc::address::AddressArea` path is unchanged - see
/// `banto-plc-address`'s module doc for the full reasoning.
pub use banto_plc_address::AddressArea;

/// A parsed, protocol-ready PLC address. Which variant a tag gets is decided
/// once, by which parser the caller ran, which in turn follows the
/// `PlcConnection`'s `protocol` column (`"modbus-tcp"` / `"slmp"`, see
/// `banto_tags::plc_connection::ALLOWED_PROTOCOLS`) - it is never inferred
/// from the address text, so a Modbus-looking address on an SLMP connection
/// is reported as the configuration mistake it is instead of being guessed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Address {
    /// Modbus reference-number notation: an area plus a 0-based offset within
    /// it. `offset` is always a valid `u16` (0..=65535) because
    /// [`Address::parse`] is the only constructor and it rejects anything that
    /// would not fit (reference numbers above `xxxxx6` = offset 65536 do not
    /// exist in Modbus's 16-bit address space).
    ///
    /// `bit` (T8, docs/tag-server-design.md §6.1) is `Some(0..=15)` for the
    /// `"40001.3"` bit-in-word notation - a single bit within the register at
    /// `offset`, valid only for [`AddressArea::InputRegister`]/
    /// [`AddressArea::HoldingRegister`] (a coil/discrete-input address is
    /// already bit-granular, so [`Address::parse`] never produces `Some`
    /// there). `None` is the pre-T8 shape and every pre-T8 caller's behavior.
    ModbusRef {
        area: AddressArea,
        offset: u16,
        bit: Option<u8>,
    },
    /// MELSEC device-code notation: a device type plus its number. `number`
    /// is `u32` rather than `u16` because MELSEC address spaces genuinely
    /// exceed 65,535 (`ZR`/`D` on the R series run into the millions), and is
    /// capped at [`crate::slmp::address::MAX_DEVICE_NUMBER`] - SLMP's 3-byte
    /// wire field - by [`Address::parse_slmp`].
    ///
    /// Bit-vs-word is *not* a field here for the *device*: it is a property
    /// of `device` ([`SlmpDevice::access`]), so the two can never disagree.
    /// `bit` (T8, docs/tag-server-design.md §6.1) is the separate, optional
    /// bit-*in-word* notation (`"D100.5"`) - `Some(0..=15)` naming one bit of
    /// the word at `number`, valid only when `device` is a word device
    /// ([`crate::slmp::address::parse`] rejects a `.N` suffix on a bit device
    /// outright, since it is already one bit). `None` is the pre-T8 shape.
    Slmp {
        device: SlmpDevice,
        number: u32,
        bit: Option<u8>,
    },
}

impl Address {
    /// Parse instrumentation reference-number notation (Modbus). Accepts
    /// leading/trailing whitespace (trimmed, matching how
    /// `banto-tags::TagInput` itself trims `address` before storing it -
    /// trimming twice is harmless). Everything else about the input must be
    /// exactly right:
    ///
    /// - exactly 5 or 6 ASCII digits, nothing else
    /// - first digit selects the area: `0`/`1`/`3`/`4` (`2` and `5-9` are not
    ///   reference-number prefixes and are rejected)
    /// - the remaining 4 (5-digit form) or 5 (6-digit form) digits are the
    ///   1-based number *within* that area; `0` is rejected (there is no
    ///   "number 0" in 1-based notation) and so is any number above `65536`
    ///   (the resulting 0-based offset would not fit in `u16`)
    ///
    /// Kept named `parse` (rather than renamed to `parse_modbus_ref` for
    /// symmetry with [`Address::parse_slmp`]) because I2a is additive by
    /// contract: `banto-collect`'s `config::build_request` calls this, and a
    /// rename would be a breaking change to a crate I2a does not otherwise
    /// touch, bought with nothing but tidiness.
    ///
    /// T8 (docs/tag-server-design.md §6.1) additionally accepts an optional
    /// bit-in-word suffix: `.` followed by 1-2 **decimal** digits naming a
    /// bit position `0..=15` (`"40001.3"`). The suffix is only meaningful at
    /// [`AddressArea::InputRegister`]/[`AddressArea::HoldingRegister`] - a
    /// coil/discrete-input reference number is already one bit, so a `.N`
    /// there is rejected the same way an unknown area prefix is, rather than
    /// silently accepted as a redundant no-op. See
    /// [`crate::slmp::address::parse`]'s doc comment for why the suffix is
    /// decimal-only even though nothing else about Modbus reference numbers
    /// is - the same §6.1 reasoning applies to both notations.
    pub fn parse(raw: &str) -> Result<Self, PlcError> {
        let trimmed = raw.trim();
        let invalid = || PlcError::InvalidAddress(raw.to_string());

        // Split off the optional bit-in-word suffix first, exactly like
        // `crate::slmp::address::parse` - `split_once` takes the *first*
        // '.', so a malformed multi-dot string leaves a non-digit remainder
        // that the digit check just below rejects.
        let (base, bit) = match trimmed.split_once('.') {
            Some((base, bit_text)) => {
                if bit_text.is_empty()
                    || bit_text.len() > 2
                    || !bit_text.chars().all(|c| c.is_ascii_digit())
                {
                    return Err(invalid());
                }
                let bit: u8 = bit_text.parse().map_err(|_| invalid())?;
                if bit > crate::slmp::address::MAX_BIT_POSITION {
                    return Err(invalid());
                }
                (base, Some(bit))
            }
            None => (trimmed, None),
        };

        let len = base.chars().count();
        if (len != 5 && len != 6) || !base.chars().all(|c| c.is_ascii_digit()) {
            return Err(invalid());
        }

        let mut chars = base.chars();
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

        // A bit suffix only makes sense on a register area - a coil/
        // discrete-input reference is already bit-granular (see this
        // method's doc comment).
        if bit.is_some() && matches!(area, AddressArea::Coil | AddressArea::DiscreteInput) {
            return Err(invalid());
        }

        Ok(Address::ModbusRef { area, offset, bit })
    }

    /// Parse MELSEC device-code notation (SLMP, I2a). See
    /// [`crate::slmp::address::parse`] for the exact grammar - the device
    /// mnemonic table, the per-device decimal-vs-hexadecimal rule, and what
    /// v1 deliberately does not accept.
    pub fn parse_slmp(raw: &str) -> Result<Self, PlcError> {
        let (device, number, bit) = slmp_address::parse(raw)?;
        Ok(Address::Slmp {
            device,
            number,
            bit,
        })
    }

    /// The notation family this address is written in (`"modbus-ref"` /
    /// `"slmp"`). Only used to build [`PlcError::AddressProtocolMismatch`]'s
    /// message, so an operator who put an SLMP address on a Modbus connection
    /// is told which of the two is out of place rather than just "invalid".
    pub fn notation(&self) -> &'static str {
        match self {
            Address::ModbusRef { .. } => "modbus-ref",
            Address::Slmp { .. } => "slmp",
        }
    }

    /// The Modbus half of the sum type, for callers that only speak Modbus
    /// ([`crate::planning::plan_requests`]). `None` means "this address is
    /// some other protocol's", which is a per-request configuration error, not
    /// a reason to fail a batch - see this module's doc comment.
    ///
    /// The third element is the T8 bit-in-word position (§6.1), `None` for a
    /// plain reference number - see [`Address::ModbusRef`]'s doc comment.
    pub fn as_modbus_ref(&self) -> Option<(AddressArea, u16, Option<u8>)> {
        match *self {
            Address::ModbusRef { area, offset, bit } => Some((area, offset, bit)),
            _ => None,
        }
    }

    /// The SLMP half of the sum type, mirroring [`Address::as_modbus_ref`]
    /// (including its third, T8 bit-in-word element - see [`Address::Slmp`]'s
    /// doc comment).
    pub fn as_slmp(&self) -> Option<(SlmpDevice, u32, Option<u8>)> {
        match *self {
            Address::Slmp {
                device,
                number,
                bit,
            } => Some((device, number, bit)),
            _ => None,
        }
    }
}

/// Renders back into the notation the matching parser accepts, so log lines
/// and validation errors quote an address the operator can find in their tag
/// list. Modbus offsets are shown in the 6-digit extended form throughout
/// (`400001`, not `40001`) - one unambiguous spelling beats switching forms at
/// offset 9999.
impl std::fmt::Display for Address {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Address::ModbusRef { area, offset, bit } => {
                let prefix = match area {
                    AddressArea::Coil => '0',
                    AddressArea::DiscreteInput => '1',
                    AddressArea::InputRegister => '3',
                    AddressArea::HoldingRegister => '4',
                };
                match bit {
                    Some(b) => write!(f, "{prefix}{:05}.{b}", offset as u32 + 1),
                    None => write!(f, "{prefix}{:05}", offset as u32 + 1),
                }
            }
            Address::Slmp {
                device,
                number,
                bit,
            } => f.write_str(&slmp_address::format(device, number, bit)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Offset of a Modbus address, or a panic - a shorthand that keeps the
    /// pre-I2a assertions below readable now that `Address` is a sum type.
    fn offset_of(raw: &str) -> u16 {
        Address::parse(raw)
            .unwrap_or_else(|e| panic!("{raw} should parse: {e}"))
            .as_modbus_ref()
            .expect("Address::parse must produce a ModbusRef")
            .1
    }

    #[test]
    fn parses_every_area_prefix() {
        assert_eq!(
            Address::parse("00001").unwrap(),
            Address::ModbusRef {
                area: AddressArea::Coil,
                offset: 0,
                bit: None
            }
        );
        assert_eq!(
            Address::parse("10001").unwrap(),
            Address::ModbusRef {
                area: AddressArea::DiscreteInput,
                offset: 0,
                bit: None
            }
        );
        assert_eq!(
            Address::parse("30001").unwrap(),
            Address::ModbusRef {
                area: AddressArea::InputRegister,
                offset: 0,
                bit: None
            }
        );
        assert_eq!(
            Address::parse("40001").unwrap(),
            Address::ModbusRef {
                area: AddressArea::HoldingRegister,
                offset: 0,
                bit: None
            }
        );
    }

    /// I2a turned `Address` into a sum type; this pins down that
    /// `Address::parse` still means "Modbus reference number" and never
    /// produces the SLMP variant, which is what makes the change additive for
    /// `banto-collect` (whose `build_request` calls exactly this).
    #[test]
    fn parse_still_means_modbus_reference_notation() {
        assert!(matches!(
            Address::parse("40001").unwrap(),
            Address::ModbusRef { .. }
        ));
        assert_eq!(Address::parse("40001").unwrap().notation(), "modbus-ref");
    }

    #[test]
    fn five_digit_offset_is_one_based() {
        // 40001 -> offset 0 (design doc's worked example).
        assert_eq!(offset_of("40001"), 0);
        assert_eq!(offset_of("40010"), 9);
        assert_eq!(offset_of("49999"), 9998);
    }

    #[test]
    fn six_digit_extended_form_reaches_beyond_9999() {
        assert_eq!(offset_of("400001"), 0);
        assert_eq!(offset_of("410000"), 9999);
        assert_eq!(offset_of("465536"), 65_535);
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
            Address::ModbusRef {
                area: AddressArea::HoldingRegister,
                offset: 0,
                bit: None
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

    // --- I2a: the SLMP half of the sum type ---
    //
    // The notation's own edge cases (radix per device, longest-mnemonic-first
    // matching, the wire ceiling) are tested where they are implemented, in
    // `slmp/address.rs`. These only cover the wiring at this level.

    #[test]
    fn parse_slmp_produces_the_slmp_variant() {
        assert_eq!(
            Address::parse_slmp("D100").unwrap(),
            Address::Slmp {
                device: SlmpDevice::D,
                number: 100,
                bit: None
            }
        );
        assert_eq!(Address::parse_slmp("D100").unwrap().notation(), "slmp");
    }

    #[test]
    fn parse_slmp_propagates_the_notations_own_rejections() {
        assert!(matches!(
            Address::parse_slmp("T100"),
            Err(PlcError::InvalidAddress(_))
        ));
    }

    /// The two parsers must not accept each other's notation: that is what
    /// turns "wrong protocol configured on this connection" into a loud error
    /// instead of a read of some unrelated address.
    #[test]
    fn the_two_notations_do_not_overlap() {
        assert!(Address::parse("D100").is_err());
        assert!(Address::parse_slmp("40001").is_err());
    }

    #[test]
    fn accessors_return_only_their_own_variant() {
        let modbus = Address::parse("40010").unwrap();
        let slmp = Address::parse_slmp("D100").unwrap();

        assert_eq!(
            modbus.as_modbus_ref(),
            Some((AddressArea::HoldingRegister, 9, None))
        );
        assert_eq!(modbus.as_slmp(), None);

        assert_eq!(slmp.as_slmp(), Some((SlmpDevice::D, 100, None)));
        assert_eq!(slmp.as_modbus_ref(), None);
    }

    #[test]
    fn display_renders_each_variant_in_its_own_notation() {
        // Modbus always renders in the 6-digit extended form (see Display's
        // doc comment), so it round-trips through `parse` but is not always
        // byte-identical to the 5-digit input.
        assert_eq!(Address::parse("40001").unwrap().to_string(), "400001");
        assert_eq!(Address::parse("00001").unwrap().to_string(), "000001");
        assert_eq!(Address::parse("465536").unwrap().to_string(), "465536");
        assert_eq!(Address::parse_slmp("D100").unwrap().to_string(), "D100");
        assert_eq!(Address::parse_slmp("x1a").unwrap().to_string(), "X1A");
    }

    #[test]
    fn display_output_reparses_to_the_same_address() {
        for raw in ["00001", "10001", "30001", "40001", "410000", "465536"] {
            let addr = Address::parse(raw).unwrap();
            assert_eq!(
                Address::parse(&addr.to_string()).unwrap(),
                addr,
                "{raw} should survive a Display/parse round trip"
            );
        }
        for raw in ["D100", "M50", "X1A", "ZR32768"] {
            let addr = Address::parse_slmp(raw).unwrap();
            assert_eq!(Address::parse_slmp(&addr.to_string()).unwrap(), addr);
        }
    }

    // --- T8, docs/tag-server-design.md §6.1: Modbus bit-in-word notation --

    #[test]
    fn parses_bit_in_word_notation_at_a_register_area() {
        assert_eq!(
            Address::parse("40001.3").unwrap(),
            Address::ModbusRef {
                area: AddressArea::HoldingRegister,
                offset: 0,
                bit: Some(3)
            }
        );
        assert_eq!(
            Address::parse("30001.15").unwrap(),
            Address::ModbusRef {
                area: AddressArea::InputRegister,
                offset: 0,
                bit: Some(15)
            }
        );
    }

    /// §6.1: a coil/discrete-input reference is already bit-granular, so a
    /// `.N` suffix there is rejected rather than accepted as a no-op.
    #[test]
    fn rejects_a_bit_suffix_on_a_coil_or_discrete_input() {
        assert!(matches!(
            Address::parse("00001.0"),
            Err(PlcError::InvalidAddress(_))
        ));
        assert!(matches!(
            Address::parse("10001.0"),
            Err(PlcError::InvalidAddress(_))
        ));
    }

    #[test]
    fn rejects_a_modbus_bit_position_past_fifteen() {
        assert!(matches!(
            Address::parse("40001.16"),
            Err(PlcError::InvalidAddress(_))
        ));
    }

    #[test]
    fn as_modbus_ref_carries_the_bit_position() {
        let addr = Address::parse("40001.3").unwrap();
        assert_eq!(
            addr.as_modbus_ref(),
            Some((AddressArea::HoldingRegister, 0, Some(3)))
        );
    }

    #[test]
    fn display_and_parse_round_trip_bit_in_word_notation() {
        for raw in ["400001.3", "300001.15", "D100.5", "W10.0"] {
            let is_slmp = raw.starts_with(['D', 'W']);
            let addr = if is_slmp {
                Address::parse_slmp(raw).unwrap()
            } else {
                Address::parse(raw).unwrap()
            };
            let rendered = addr.to_string();
            let reparsed = if is_slmp {
                Address::parse_slmp(&rendered).unwrap()
            } else {
                Address::parse(&rendered).unwrap()
            };
            assert_eq!(reparsed, addr, "{raw} should survive a round trip");
        }
    }
}
