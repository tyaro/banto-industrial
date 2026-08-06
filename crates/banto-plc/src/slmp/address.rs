//! MELSEC device-code address notation (`"D100"`, `"M50"`, `"X1A"`, `"ZR0"`):
//! the SLMP counterpart to `address.rs`'s Modbus reference-number parsing, and
//! pure in exactly the same way - no socket, no dependency on the `slmp` crate.
//! [`crate::address::Address::parse_slmp`] is the public entry point;
//! everything here exists to serve it.
//!
//! ## Why this vocabulary is this crate's own, not the `slmp` crate's
//!
//! The `slmp` crate has its own `DeviceType` enum with the same 28 devices,
//! and [`SlmpDevice::to_wire`] maps onto it one-for-one. Redefining it here
//! anyway buys two things the wrapped crate cannot give:
//!
//! 1. `slmp::DeviceType` knows a device's *wire code* but not whether it is a
//!    bit or a word device, nor whether its number is written in decimal or
//!    hexadecimal. Both are needed *before* any wire traffic - the first by
//!    [`super::planning`] (to pick bit-unit vs word-unit bulk reads, and to
//!    reject `bit`-typed tags at word devices), the second by the parser right
//!    here. Rust cannot add inherent methods to a foreign type, so these
//!    would have to live in free functions matching on a foreign enum, which
//!    is the same table with worse ergonomics.
//! 2. It keeps [`crate::address::Address`] - the type `banto-collect` and
//!    every future consumer stores per tag - free of any `slmp`-crate type in
//!    its public signature, so swapping or bumping the wrapped crate cannot
//!    ripple out into callers' code.
//!
//! `slmp_device_wire_codes_match_the_wrapped_crate` is the tripwire that
//! keeps the two tables honest.

use crate::error::PlcError;

/// Whether a device is addressed one bit at a time or one 16-bit word at a
/// time. Derived from the device type rather than stored alongside it
//  - a `D` device is never a bit device, so there is no state here that
/// could ever disagree with itself (contrast [`crate::address::AddressArea`],
/// where Modbus genuinely has separate bit and word *spaces* a caller picks
/// between).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SlmpAccess {
    /// X/Y/M/L/... - read with a bit-unit bulk read (SLMP subcommand
    /// `0x0001`/`0x0003`), two points per response byte.
    Bit,
    /// D/W/R/... - read with a word-unit bulk read (SLMP subcommand
    /// `0x0000`/`0x0002`), two bytes per point.
    Word,
}

impl std::fmt::Display for SlmpAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SlmpAccess::Bit => "bit_device",
            SlmpAccess::Word => "word_device",
        })
    }
}

/// A MELSEC device type. All 28 devices the wrapped `slmp` crate can encode,
/// no more: a device this crate accepted but could not put on the wire would
/// only fail later and further from the mistake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SlmpDevice {
    /// Input relay (bit, hex).
    X,
    /// Output relay (bit, hex).
    Y,
    /// Internal relay (bit, decimal).
    M,
    /// Latch relay (bit, decimal).
    L,
    /// Annunciator (bit, decimal).
    F,
    /// Edge relay (bit, decimal).
    V,
    /// Link relay (bit, hex).
    B,
    /// Data register (word, decimal) - the workhorse of most tag lists.
    D,
    /// Link register (word, hex).
    W,
    /// Step relay (bit, decimal).
    S,
    /// Index register (word, decimal).
    Z,
    /// File register (word, decimal).
    R,
    /// Serial-number-access file register (word, decimal).
    ZR,
    /// Timer contact (bit, decimal).
    TS,
    /// Timer coil (bit, decimal).
    TC,
    /// Timer current value (word, decimal).
    TN,
    /// Retentive timer contact (bit, decimal).
    SS,
    /// Retentive timer coil (bit, decimal).
    SC,
    /// Retentive timer current value (word, decimal).
    SN,
    /// Counter contact (bit, decimal).
    CS,
    /// Counter coil (bit, decimal).
    CC,
    /// Counter current value (word, decimal).
    CN,
    /// Link special relay (bit, hex).
    SB,
    /// Special register (word, decimal).
    SD,
    /// Special relay (bit, decimal).
    SM,
    /// Link special register (word, hex).
    SW,
    /// Direct-access input (bit, hex).
    DX,
    /// Direct-access output (bit, hex).
    DY,
}

/// Every [`SlmpDevice`], two-character mnemonics before one-character ones.
/// [`parse`] walks this in order and takes the first prefix match, which is
/// the whole reason the order matters: scanned the other way, `"SD100"` would
/// match `S` and leave `"D100"` as a nonsense device number, and `"DX10"`
/// would match `D`. Longest-first prefix matching is what makes the notation
/// unambiguous without a grammar.
///
/// `device_table_is_ordered_longest_mnemonic_first` and
/// `device_table_lists_every_device_exactly_once` are the tripwires that keep
/// a newly added device from being appended in the wrong half.
const DEVICE_TABLE: &[SlmpDevice] = &[
    // Two-character mnemonics.
    SlmpDevice::ZR,
    SlmpDevice::TS,
    SlmpDevice::TC,
    SlmpDevice::TN,
    SlmpDevice::SS,
    SlmpDevice::SC,
    SlmpDevice::SN,
    SlmpDevice::CS,
    SlmpDevice::CC,
    SlmpDevice::CN,
    SlmpDevice::SB,
    SlmpDevice::SD,
    SlmpDevice::SM,
    SlmpDevice::SW,
    SlmpDevice::DX,
    SlmpDevice::DY,
    // One-character mnemonics.
    SlmpDevice::X,
    SlmpDevice::Y,
    SlmpDevice::M,
    SlmpDevice::L,
    SlmpDevice::F,
    SlmpDevice::V,
    SlmpDevice::B,
    SlmpDevice::D,
    SlmpDevice::W,
    SlmpDevice::S,
    SlmpDevice::Z,
    SlmpDevice::R,
];

/// Largest device number this crate accepts. Not a per-device catalogue limit
/// (those are CPU-model specific and belong in the device profile an operator
/// configures, not in a parser) but the hard *wire* ceiling: SLMP's device
/// specification field carries the number in 3 bytes, whichever CPU series is
/// in use (the R series' 4th byte is a fixed `0x00` pad, not a 4th address
/// byte - see `slmp::Device::serialize`). A number that cannot be encoded is
/// better rejected here, where the error can name the offending tag, than
/// silently truncated into a read of some unrelated address.
pub const MAX_DEVICE_NUMBER: u32 = 0x00FF_FFFF;

impl SlmpDevice {
    /// Every device this crate can address, for callers that need to enumerate
    /// rather than match: the simulator's wire-code-to-device reverse lookup,
    /// the tests that assert table-wide properties, and (eventually) any UI
    /// offering an operator a device to pick from. Returns [`DEVICE_TABLE`]
    /// itself, so "every device" cannot drift from "every device the parser
    /// knows".
    ///
    /// The order is [`DEVICE_TABLE`]'s - grouped by mnemonic length, because
    /// that is what [`parse`] needs - and is deliberately *not* part of this
    /// method's contract. A caller that wants a stable presentation order
    /// should sort by [`SlmpDevice::mnemonic`].
    pub const fn all() -> &'static [SlmpDevice] {
        DEVICE_TABLE
    }

    /// The mnemonic as written in MELSEC documentation and in tag lists -
    /// also exactly what [`parse`] accepts, which is what keeps
    /// parse/[`Display`](std::fmt::Display) round-tripping.
    pub const fn mnemonic(self) -> &'static str {
        match self {
            SlmpDevice::X => "X",
            SlmpDevice::Y => "Y",
            SlmpDevice::M => "M",
            SlmpDevice::L => "L",
            SlmpDevice::F => "F",
            SlmpDevice::V => "V",
            SlmpDevice::B => "B",
            SlmpDevice::D => "D",
            SlmpDevice::W => "W",
            SlmpDevice::S => "S",
            SlmpDevice::Z => "Z",
            SlmpDevice::R => "R",
            SlmpDevice::ZR => "ZR",
            SlmpDevice::TS => "TS",
            SlmpDevice::TC => "TC",
            SlmpDevice::TN => "TN",
            SlmpDevice::SS => "SS",
            SlmpDevice::SC => "SC",
            SlmpDevice::SN => "SN",
            SlmpDevice::CS => "CS",
            SlmpDevice::CC => "CC",
            SlmpDevice::CN => "CN",
            SlmpDevice::SB => "SB",
            SlmpDevice::SD => "SD",
            SlmpDevice::SM => "SM",
            SlmpDevice::SW => "SW",
            SlmpDevice::DX => "DX",
            SlmpDevice::DY => "DY",
        }
    }

    /// Bit device or word device. Drives both the v1 data-type restriction
    /// (`bit` tags only at bit devices, numeric tags only at word devices -
    /// see [`super::planning`]) and which bulk-read subcommand a group uses.
    pub const fn access(self) -> SlmpAccess {
        match self {
            SlmpDevice::X
            | SlmpDevice::Y
            | SlmpDevice::M
            | SlmpDevice::L
            | SlmpDevice::F
            | SlmpDevice::V
            | SlmpDevice::B
            | SlmpDevice::S
            | SlmpDevice::TS
            | SlmpDevice::TC
            | SlmpDevice::SS
            | SlmpDevice::SC
            | SlmpDevice::CS
            | SlmpDevice::CC
            | SlmpDevice::SB
            | SlmpDevice::SM
            | SlmpDevice::DX
            | SlmpDevice::DY => SlmpAccess::Bit,
            SlmpDevice::D
            | SlmpDevice::W
            | SlmpDevice::Z
            | SlmpDevice::R
            | SlmpDevice::ZR
            | SlmpDevice::TN
            | SlmpDevice::SN
            | SlmpDevice::CN
            | SlmpDevice::SD
            | SlmpDevice::SW => SlmpAccess::Word,
        }
    }

    /// The radix a device's number is *written* in. This is a notation rule,
    /// not a wire rule (the wire always carries a plain binary integer):
    /// MELSEC engineering tools render the link/IO-adjacent devices
    /// (X/Y/B/W/SB/SW/DX/DY) in hexadecimal and everything else in decimal,
    /// so `"X1A"` and `"M26"` are *different* numbers written from the same
    /// tag list. Getting this wrong would silently read the wrong address, so
    /// it is a property of the device rather than a per-tag flag an operator
    /// could forget to set.
    pub const fn radix(self) -> u32 {
        match self {
            SlmpDevice::X
            | SlmpDevice::Y
            | SlmpDevice::B
            | SlmpDevice::W
            | SlmpDevice::SB
            | SlmpDevice::SW
            | SlmpDevice::DX
            | SlmpDevice::DY => 16,
            _ => 10,
        }
    }
}

impl std::fmt::Display for SlmpDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.mnemonic())
    }
}

/// Largest bit position accepted by the `.N` bit-in-word suffix (§6.1) -
/// MELSEC words are 16 bits, positions `0..=15`.
pub const MAX_BIT_POSITION: u8 = 15;

/// Parse MELSEC device notation into `(device, number, bit)`.
///
/// Accepts leading/trailing whitespace (trimmed, same as
/// [`crate::address::Address::parse`] and for the same reason - `banto-tags`
/// already trims `Tag::address`, and trimming twice is harmless) and
/// lowercase mnemonics (`"d100"`), because a tag list typed by hand will
/// contain both. Everything else must be exactly right:
///
/// - a known device mnemonic, longest match first (see [`DEVICE_TABLE`])
/// - followed by at least one digit, all of them valid in that device's
///   [`SlmpDevice::radix`] - so `"X1A"` is `X` number 26 while `"M1A"` is
///   rejected outright rather than quietly read as `M1`
/// - a number no greater than [`MAX_DEVICE_NUMBER`]
/// - optionally, a bit-in-word suffix: `.` followed by 1-2 **decimal**
///   digits naming a bit position `0..=15` (T8, docs/tag-server-design.md
///   §6.1) - `"D100.5"` is word device `D100`, bit 5
///
/// ## Why the bit suffix is decimal-only, even at a hex-numbered device
///
/// [`SlmpDevice::radix`] governs how the *device number* is written (`X1A` is
/// hex), but the bit-in-word suffix is a separate axis: MELSEC engineering
/// tools always write the bit-within-word position in decimal, and `docs/
/// tag-server-design.md` §6.1 records the reasoning for rejecting a hex
/// spelling here even though it would be unambiguous: `"D100.A"` reads as
/// "device D, number 100, hex digit A" to nobody's actual MELSEC tooling, and
/// allowing it would invite exactly that misreading next to Modbus's
/// register+bit notation (`"40001.3"`), which is decimal for the same
/// human-convention reason. So the suffix is validated with
/// [`char::is_ascii_digit`], never against `device.radix()`.
///
/// ## Why only word devices accept the suffix
///
/// A bit device (`M`/`X`/`Y`/...) already *is* one bit - `"M50.0"` would be a
/// bit position on something that has no further bits to select, so it is
/// rejected exactly like an unknown mnemonic rather than silently accepted as
/// a redundant `.0`. This is what routes a bit-typed tag to the right read
/// strategy one layer up ([`super::planning`]): a `bit` tag with no suffix
/// addresses a bit device via the existing bit-unit bulk read, unchanged; a
/// `bit` tag *with* a suffix addresses a word device and folds into that
/// word's ordinary word-unit bulk read (decoded down to one bit) - see this
/// module's sibling `super::planning` module doc for the "why fold into the
/// existing word read" reasoning.
///
/// Deliberately *not* accepted in v1, for the same reason as before T8: digit
/// designation (`"K4M0"`) and module-qualified notation (`"U3E0\\G100"`).
/// Each is a real MELSEC form, and each needs a matching read strategy in
/// [`super::planning`] before the parser should start promising it works.
pub(crate) fn parse(raw: &str) -> Result<(SlmpDevice, u32, Option<u8>), PlcError> {
    let trimmed = raw.trim();
    let invalid = || PlcError::InvalidAddress(raw.to_string());

    // ASCII-only by construction: every mnemonic and digit below is ASCII, so
    // uppercasing cannot change the string's length or byte boundaries. A
    // non-ASCII address fails the digit check further down.
    let upper = trimmed.to_ascii_uppercase();

    // Split off the optional bit-in-word suffix first: `split_once` finds the
    // *first* '.', so a malformed multi-dot string (`"D100.3.4"`) leaves a
    // non-digit remainder that the digit check below rejects, rather than
    // being silently reinterpreted.
    let (base, bit) = match upper.split_once('.') {
        Some((base, bit_text)) => {
            if bit_text.is_empty()
                || bit_text.len() > 2
                || !bit_text.chars().all(|c| c.is_ascii_digit())
            {
                return Err(invalid());
            }
            // Safe: 1-2 ASCII digits parse as a `u8` with room to spare
            // (max 99), so this can only fail to construct a value that the
            // MAX_BIT_POSITION check immediately below rejects anyway.
            let bit: u8 = bit_text.parse().map_err(|_| invalid())?;
            if bit > MAX_BIT_POSITION {
                return Err(invalid());
            }
            (base, Some(bit))
        }
        None => (upper.as_str(), None),
    };

    let device = DEVICE_TABLE
        .iter()
        .copied()
        .find(|d| base.starts_with(d.mnemonic()))
        .ok_or_else(invalid)?;

    let digits = &base[device.mnemonic().len()..];
    if digits.is_empty() {
        return Err(invalid());
    }
    let radix = device.radix();
    if !digits.chars().all(|c| c.is_digit(radix)) {
        return Err(invalid());
    }

    // `from_str_radix` is the only thing that can still fail here, and only
    // by overflowing u32 - a 9-digit decimal number, say. Folded into the
    // same rejection as the explicit ceiling check below.
    let number = u32::from_str_radix(digits, radix).map_err(|_| invalid())?;
    if number > MAX_DEVICE_NUMBER {
        return Err(invalid());
    }

    // A bit suffix only makes sense on a word device (see this function's
    // doc comment) - a bit device is rejected here rather than by a separate
    // caller-side check, so "M50.0 is nonsense" is a parse error at the same
    // layer as every other notation mistake.
    if bit.is_some() && device.access() != SlmpAccess::Word {
        return Err(invalid());
    }

    Ok((device, number, bit))
}

/// Render `(device, number, bit)` back into the notation [`parse`] accepts.
/// Used by error messages (and by [`crate::address::Address`]'s
/// [`Display`](std::fmt::Display)) so a rejected tag is reported in the same
/// spelling the operator configured, not as a debug dump.
pub(crate) fn format(device: SlmpDevice, number: u32, bit: Option<u8>) -> String {
    let base = match device.radix() {
        16 => format!("{device}{number:X}"),
        _ => format!("{device}{number}"),
    };
    match bit {
        Some(b) => format!("{base}.{b}"),
        None => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse and assert no bit suffix was present, returning the pre-T8
    /// `(device, number)` shape - keeps every pre-existing assertion below
    /// exactly as readable as it was before `parse` grew a third return
    /// value, while still proving that a plain (unqualified) address is
    /// unaffected by T8's bit notation.
    fn p(raw: &str) -> (SlmpDevice, u32) {
        let (device, number, bit) =
            parse(raw).unwrap_or_else(|e| panic!("{raw} should parse: {e}"));
        assert_eq!(bit, None, "{raw} should not carry a bit suffix");
        (device, number)
    }

    /// The whole parser rests on [`DEVICE_TABLE`]'s ordering (see its doc
    /// comment); this proves the ordering rather than trusting the literal.
    #[test]
    fn device_table_is_ordered_longest_mnemonic_first() {
        let mut previous = usize::MAX;
        for device in DEVICE_TABLE {
            let len = device.mnemonic().len();
            assert!(
                len <= previous,
                "{device} (len {len}) appears after a shorter mnemonic (len {previous}) - \
                 longest-first ordering is what makes prefix matching unambiguous"
            );
            previous = len;
        }
    }

    /// A device missing from the table parses as nothing at all; a duplicated
    /// one is dead weight. Counting against the parse of every mnemonic keeps
    /// this honest without needing `strum`-style enum iteration.
    #[test]
    fn device_table_lists_every_device_exactly_once() {
        let mut seen: Vec<SlmpDevice> = DEVICE_TABLE.to_vec();
        seen.sort();
        seen.dedup();
        assert_eq!(
            seen.len(),
            DEVICE_TABLE.len(),
            "DEVICE_TABLE contains a duplicate"
        );
        assert_eq!(
            DEVICE_TABLE.len(),
            28,
            "DEVICE_TABLE should list all 28 devices the slmp crate can encode"
        );
    }

    #[test]
    fn every_device_mnemonic_round_trips_through_parse() {
        for device in DEVICE_TABLE {
            let text = format!("{}0", device.mnemonic());
            let (parsed, number) = p(&text);
            assert_eq!(parsed, *device, "{text} parsed as the wrong device");
            assert_eq!(number, 0);
        }
    }

    #[test]
    fn parses_the_common_word_devices() {
        assert_eq!(p("D100"), (SlmpDevice::D, 100));
        assert_eq!(p("R1000"), (SlmpDevice::R, 1000));
        assert_eq!(p("ZR32768"), (SlmpDevice::ZR, 32768));
        assert_eq!(p("SD0"), (SlmpDevice::SD, 0));
    }

    #[test]
    fn parses_the_common_bit_devices() {
        assert_eq!(p("M50"), (SlmpDevice::M, 50));
        assert_eq!(p("SM400"), (SlmpDevice::SM, 400));
        assert_eq!(p("L0"), (SlmpDevice::L, 0));
    }

    /// The single most consequential rule in this module: the same digits
    /// mean different numbers at a hex device and a decimal device.
    #[test]
    fn hexadecimal_devices_parse_their_number_as_hex() {
        assert_eq!(p("X1A"), (SlmpDevice::X, 0x1A));
        assert_eq!(p("Y20"), (SlmpDevice::Y, 0x20));
        assert_eq!(p("B3F"), (SlmpDevice::B, 0x3F));
        assert_eq!(p("W1FF"), (SlmpDevice::W, 0x1FF));
        assert_eq!(p("SW100"), (SlmpDevice::SW, 0x100));
        assert_eq!(p("DX10"), (SlmpDevice::DX, 0x10));
    }

    #[test]
    fn decimal_devices_parse_their_number_as_decimal() {
        assert_eq!(p("M20"), (SlmpDevice::M, 20));
        assert_eq!(p("D20"), (SlmpDevice::D, 20));
        // Same digits as Y20 above, deliberately - Y20 is 32, M20 is 20.
        assert_ne!(p("M20").1, p("Y20").1);
    }

    /// Longest-first matching, stated as the cases that would break under
    /// shortest-first matching.
    #[test]
    fn two_character_mnemonics_win_over_their_one_character_prefixes() {
        assert_eq!(p("SD100").0, SlmpDevice::SD);
        assert_eq!(p("SM100").0, SlmpDevice::SM);
        assert_eq!(p("SS100").0, SlmpDevice::SS);
        assert_eq!(p("DX100").0, SlmpDevice::DX);
        assert_eq!(p("DY100").0, SlmpDevice::DY);
        assert_eq!(p("ZR100").0, SlmpDevice::ZR);
        assert_eq!(p("CN100").0, SlmpDevice::CN);
        assert_eq!(p("TN100").0, SlmpDevice::TN);
    }

    /// ...and the reverse: a one-character device followed by digits that
    /// merely *look* like the tail of a two-character mnemonic.
    #[test]
    fn one_character_mnemonics_still_parse_when_no_longer_match_exists() {
        assert_eq!(p("S100"), (SlmpDevice::S, 100));
        assert_eq!(p("D100"), (SlmpDevice::D, 100));
        assert_eq!(p("Z9"), (SlmpDevice::Z, 9));
    }

    #[test]
    fn accepts_lowercase_and_mixed_case() {
        assert_eq!(p("d100"), (SlmpDevice::D, 100));
        assert_eq!(p("zr5"), (SlmpDevice::ZR, 5));
        assert_eq!(p("x1a"), (SlmpDevice::X, 0x1A));
        assert_eq!(p("Sd7"), (SlmpDevice::SD, 7));
    }

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(p("  D100  "), (SlmpDevice::D, 100));
    }

    #[test]
    fn accepts_leading_zeros() {
        assert_eq!(p("X0010"), (SlmpDevice::X, 0x10));
        assert_eq!(p("D0000"), (SlmpDevice::D, 0));
    }

    /// A hex digit at a decimal device is a mistake worth surfacing, not a
    /// truncation: `"M1A"` must not become `M1`.
    #[test]
    fn rejects_hex_digits_at_a_decimal_device() {
        assert!(matches!(parse("M1A"), Err(PlcError::InvalidAddress(_))));
        assert!(matches!(parse("D1F"), Err(PlcError::InvalidAddress(_))));
    }

    #[test]
    fn rejects_non_hex_letters_at_a_hex_device() {
        assert!(matches!(parse("X1G"), Err(PlcError::InvalidAddress(_))));
        assert!(matches!(parse("WZZ"), Err(PlcError::InvalidAddress(_))));
    }

    #[test]
    fn rejects_a_mnemonic_with_no_number() {
        assert!(matches!(parse("D"), Err(PlcError::InvalidAddress(_))));
        assert!(matches!(parse("ZR"), Err(PlcError::InvalidAddress(_))));
    }

    #[test]
    fn rejects_unknown_device_mnemonics() {
        // No plain T or C device exists (only TS/TC/TN, CS/CC/CN), and there
        // is no P/K/H/E/U device in this crate's set.
        for text in ["T100", "C100", "P0", "K4M0", "H10", "E1", "U3E0"] {
            assert!(
                matches!(parse(text), Err(PlcError::InvalidAddress(_))),
                "{text} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_digit_designation_notation() {
        // Still not accepted in v1, see `parse`'s doc comment.
        assert!(matches!(parse("K4M0"), Err(PlcError::InvalidAddress(_))));
    }

    #[test]
    fn rejects_a_number_past_the_three_byte_wire_ceiling() {
        let ok = format!("D{MAX_DEVICE_NUMBER}");
        assert_eq!(p(&ok), (SlmpDevice::D, MAX_DEVICE_NUMBER));

        let too_big = format!("D{}", MAX_DEVICE_NUMBER + 1);
        assert!(matches!(parse(&too_big), Err(PlcError::InvalidAddress(_))));
    }

    // --- T8, docs/tag-server-design.md §6.1: bit-in-word notation ----------

    /// The load-bearing new case: a word device with a decimal bit suffix
    /// parses to `(device, number, Some(bit))`.
    #[test]
    fn parses_bit_in_word_notation_at_a_word_device() {
        assert_eq!(parse("D100.5").unwrap(), (SlmpDevice::D, 100, Some(5)));
        assert_eq!(parse("W10.0").unwrap(), (SlmpDevice::W, 0x10, Some(0)));
        assert_eq!(parse("ZR5.15").unwrap(), (SlmpDevice::ZR, 5, Some(15)));
    }

    /// §6.1's decision, stated as a test: the bit position is always decimal,
    /// even at a hex-numbered device like `W`/`X`/`Y` - `.A` is never a valid
    /// bit position, only `.10`.
    #[test]
    fn bit_suffix_is_decimal_only_even_at_a_hex_numbered_device() {
        assert!(matches!(parse("W10.A"), Err(PlcError::InvalidAddress(_))));
        assert!(matches!(parse("X0.F"), Err(PlcError::InvalidAddress(_))));
    }

    /// §6.1: bit devices already address one bit, so a `.N` suffix on one is
    /// a parse error rather than a redundant no-op.
    #[test]
    fn rejects_a_bit_suffix_on_a_bit_device() {
        for text in ["M50.0", "X1A.3", "Y0.15", "TS0.0"] {
            assert!(
                matches!(parse(text), Err(PlcError::InvalidAddress(_))),
                "{text} should be rejected: bit devices are already bit-granular"
            );
        }
    }

    /// Bit position `0..=15` is the whole legal range (a MELSEC word is 16
    /// bits); `16` and above must be rejected, not wrapped or truncated.
    #[test]
    fn rejects_a_bit_position_past_fifteen() {
        assert_eq!(parse("D0.15").unwrap().2, Some(15));
        assert!(matches!(parse("D0.16"), Err(PlcError::InvalidAddress(_))));
        assert!(matches!(parse("D0.99"), Err(PlcError::InvalidAddress(_))));
    }

    #[test]
    fn rejects_a_malformed_bit_suffix() {
        for text in ["D0.", "D0..5", "D0.5.6", "D0.-1"] {
            assert!(
                matches!(parse(text), Err(PlcError::InvalidAddress(_))),
                "{text} should be rejected"
            );
        }
    }

    #[test]
    fn bit_in_word_notation_round_trips_through_format() {
        for (device, number, bit) in [
            (SlmpDevice::D, 100u32, 5u8),
            (SlmpDevice::W, 0x10, 0),
            (SlmpDevice::ZR, 5, 15),
        ] {
            let text = format(device, number, Some(bit));
            assert_eq!(parse(&text).unwrap(), (device, number, Some(bit)));
        }
    }

    /// A number wide enough to overflow `u32` must be rejected the same way
    /// as one that merely exceeds the wire ceiling, not panic or wrap.
    #[test]
    fn rejects_a_number_too_wide_for_u32() {
        assert!(matches!(
            parse("D99999999999999999999"),
            Err(PlcError::InvalidAddress(_))
        ));
    }

    #[test]
    fn rejects_empty_and_whitespace_only() {
        assert!(matches!(parse(""), Err(PlcError::InvalidAddress(_))));
        assert!(matches!(parse("   "), Err(PlcError::InvalidAddress(_))));
    }

    #[test]
    fn rejects_modbus_reference_notation() {
        // The two notations must not overlap: a Modbus address handed to the
        // SLMP parser is a configuration mistake (wrong protocol on the
        // connection), and silently reading it as some device would be worse
        // than refusing it.
        for text in ["40001", "00001", "465536"] {
            assert!(
                matches!(parse(text), Err(PlcError::InvalidAddress(_))),
                "{text} should be rejected by the SLMP parser"
            );
        }
    }

    #[test]
    fn invalid_address_error_echoes_the_original_untrimmed_text() {
        // Same contract as address.rs's Modbus parser: easier to spot a bad
        // tag definition when the error repeats exactly what was configured.
        let err = parse("  bogus  ").unwrap_err();
        match err {
            PlcError::InvalidAddress(text) => assert_eq!(text, "  bogus  "),
            other => panic!("expected InvalidAddress, got {other:?}"),
        }
    }

    #[test]
    fn bit_and_word_devices_are_classified_as_expected() {
        for d in [
            SlmpDevice::X,
            SlmpDevice::Y,
            SlmpDevice::M,
            SlmpDevice::L,
            SlmpDevice::F,
            SlmpDevice::V,
            SlmpDevice::B,
            SlmpDevice::S,
            SlmpDevice::TS,
            SlmpDevice::TC,
            SlmpDevice::SS,
            SlmpDevice::SC,
            SlmpDevice::CS,
            SlmpDevice::CC,
            SlmpDevice::SB,
            SlmpDevice::SM,
            SlmpDevice::DX,
            SlmpDevice::DY,
        ] {
            assert_eq!(d.access(), SlmpAccess::Bit, "{d} should be a bit device");
        }
        for d in [
            SlmpDevice::D,
            SlmpDevice::W,
            SlmpDevice::Z,
            SlmpDevice::R,
            SlmpDevice::ZR,
            SlmpDevice::TN,
            SlmpDevice::SN,
            SlmpDevice::CN,
            SlmpDevice::SD,
            SlmpDevice::SW,
        ] {
            assert_eq!(d.access(), SlmpAccess::Word, "{d} should be a word device");
        }
    }

    /// The timer/counter trio splits across access types (contact and coil
    /// are bits, "N" is the current value word) - the single easiest thing to
    /// get wrong when adding devices.
    #[test]
    fn timer_and_counter_current_values_are_word_devices_but_their_contacts_are_bits() {
        assert_eq!(SlmpDevice::TN.access(), SlmpAccess::Word);
        assert_eq!(SlmpDevice::SN.access(), SlmpAccess::Word);
        assert_eq!(SlmpDevice::CN.access(), SlmpAccess::Word);
        assert_eq!(SlmpDevice::TS.access(), SlmpAccess::Bit);
        assert_eq!(SlmpDevice::TC.access(), SlmpAccess::Bit);
        assert_eq!(SlmpDevice::CS.access(), SlmpAccess::Bit);
        assert_eq!(SlmpDevice::CC.access(), SlmpAccess::Bit);
    }

    #[test]
    fn format_round_trips_through_parse_in_both_radixes() {
        for text in ["D100", "M50", "X1A", "W1FF", "ZR32768", "SM400", "DY3F"] {
            let (device, number, bit) = parse(text).unwrap();
            assert_eq!(
                format(device, number, bit),
                text,
                "{text} should survive a parse/format round trip"
            );
        }
    }
}
