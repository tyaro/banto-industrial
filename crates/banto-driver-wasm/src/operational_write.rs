//! Host-owned, per-connection allowlist for an explicitly selected signed
//! write component. Parsing is independent of the guest's own SD checks.
//!
//! Word intervals (`D100-D105`) authorize word and bit-device writes. Word-bit
//! intervals (`D100.3-D100.5`, `W1A.0-W1B.F`) are a separate list that alone
//! authorizes the guest's read-modify-write of single bits inside one D/W word;
//! a word interval never authorizes a bit, and a bit interval never a word.
//! The grammar mirrors the SLMP guest's `write_policy.rs` (banto-driver-wasm
//! #61) exactly, so the Host accepts the same saved values and nothing looser.

use banto_driver_api::Settings;

pub(super) const MODE_KEY: &str = "d_u16_write_mode";
pub(super) const ALLOWLIST_KEY: &str = "d_u16_write_allowlist";
const Q_TEST_KEY: &str = "q25prh_d3010_3013_test_write";
pub(super) const MAX_WORDS: usize = 960;
pub(super) const MAX_BITS: usize = 7168;
const MAX_INTERVALS: usize = 16;
const MAX_TEXT_LEN: usize = 256;
const MAX_DEVICE: u32 = 0x00ff_ffff;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WriteArea {
    D,
    W,
    M,
    Y,
    L,
    B,
}

impl WriteArea {
    pub(super) fn from_name(name: &str) -> Option<Self> {
        match name {
            "D" => Some(Self::D),
            "W" => Some(Self::W),
            "M" => Some(Self::M),
            "Y" => Some(Self::Y),
            "L" => Some(Self::L),
            "B" => Some(Self::B),
            _ => None,
        }
    }

    pub(super) fn canonical(self, index: u32) -> String {
        match self {
            Self::D => format!("D{index}"),
            Self::W => format!("W{index:X}"),
            Self::M => format!("M{index}"),
            Self::Y => format!("Y{index:X}"),
            Self::L => format!("L{index}"),
            Self::B => format!("B{index:X}"),
        }
    }

    pub(super) fn is_bit(self) -> bool {
        matches!(self, Self::M | Self::Y | Self::L | Self::B)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WritePolicy {
    enabled: bool,
    ranges: Vec<(WriteArea, u32, u32)>,
    /// D/W word-bit intervals as inclusive `word * 16 + bit` positions.
    bit_ranges: Vec<(WriteArea, u64, u64)>,
}

impl WritePolicy {
    pub(super) fn parse(settings: &Settings) -> Result<Self, (&'static str, &'static str)> {
        let enabled = match settings.get(MODE_KEY).unwrap_or("disabled") {
            "disabled" => false,
            "enabled" => true,
            _ => return Err((MODE_KEY, "write mode must be disabled or enabled")),
        };
        let raw = settings.get(ALLOWLIST_KEY).unwrap_or("");
        if raw.len() > MAX_TEXT_LEN {
            return Err((ALLOWLIST_KEY, "write allowlist is too long"));
        }
        let mut ranges = Vec::new();
        let mut bit_ranges: Vec<(WriteArea, u64, u64)> = Vec::new();
        if !raw.is_empty() {
            for interval in raw.split(',') {
                if ranges.len() + bit_ranges.len() == MAX_INTERVALS {
                    return Err((ALLOWLIST_KEY, "too many write ranges"));
                }
                let Some((start, end)) = interval.split_once('-') else {
                    return Err((ALLOWLIST_KEY, "expected same-area start-end ranges"));
                };
                if start.contains('.') || end.contains('.') {
                    let (area, start) = parse_word_bit(start)?;
                    let (end_area, end) = parse_word_bit(end)?;
                    if area != end_area {
                        return Err((ALLOWLIST_KEY, "write range must remain in one device area"));
                    }
                    if end < start || end - start >= MAX_BITS as u64 {
                        return Err((
                            ALLOWLIST_KEY,
                            "write range is reversed or exceeds device request limit",
                        ));
                    }
                    if area == WriteArea::D && start / 16 <= 3001 && end / 16 >= 3000 {
                        return Err((ALLOWLIST_KEY, "D3000 and D3001 are protected"));
                    }
                    if bit_ranges
                        .iter()
                        .rev()
                        .find(|&&(other, _, _)| other == area)
                        .is_some_and(|&(_, _, last)| start <= last)
                    {
                        return Err((ALLOWLIST_KEY, "write ranges must be ordered and disjoint"));
                    }
                    bit_ranges.push((area, start, end));
                    continue;
                }
                let (area, start) = parse_device(start)?;
                let (end_area, end) = parse_device(end)?;
                if area != end_area {
                    return Err((ALLOWLIST_KEY, "write range must remain in one device area"));
                }
                let limit = if area.is_bit() { MAX_BITS } else { MAX_WORDS };
                if end < start || end - start >= limit as u32 {
                    return Err((
                        ALLOWLIST_KEY,
                        "write range is reversed or exceeds device request limit",
                    ));
                }
                if area == WriteArea::D && start <= 3001 && end >= 3000 {
                    return Err((ALLOWLIST_KEY, "D3000 and D3001 are protected"));
                }
                if ranges
                    .iter()
                    .rev()
                    .find(|&&(other, _, _)| other == area)
                    .is_some_and(|&(_, _, last)| start <= last)
                {
                    return Err((ALLOWLIST_KEY, "write ranges must be ordered and disjoint"));
                }
                ranges.push((area, start, end));
            }
        }
        if enabled && ranges.is_empty() && bit_ranges.is_empty() {
            return Err((
                ALLOWLIST_KEY,
                "enabled write requires an explicit allowlist",
            ));
        }
        if enabled && settings.get(Q_TEST_KEY) == Some("enabled") {
            return Err((
                MODE_KEY,
                "normal write and Q test write cannot both be enabled",
            ));
        }
        Ok(Self {
            enabled,
            ranges,
            bit_ranges,
        })
    }

    /// Every written bit of one D/W word must lie inside a word-bit interval.
    /// Word intervals do not count. D3000/D3001 stay protected.
    pub(super) fn allows_word_bits(&self, area: WriteArea, word: u32, bits: &[u8]) -> bool {
        self.enabled
            && matches!(area, WriteArea::D | WriteArea::W)
            && !bits.is_empty()
            && word <= MAX_DEVICE
            && (area != WriteArea::D || !(3000..=3001).contains(&word))
            && bits.iter().all(|&bit| {
                let position = u64::from(word) * 16 + u64::from(bit);
                bit < 16
                    && self.bit_ranges.iter().any(|&(allowed_area, start, end)| {
                        allowed_area == area && (start..=end).contains(&position)
                    })
            })
    }

    pub(super) fn allows_device_span(&self, area: WriteArea, start: u32, len: usize) -> bool {
        if !self.enabled || len == 0 || len > if area.is_bit() { MAX_BITS } else { MAX_WORDS } {
            return false;
        }
        let Ok(offset) = u32::try_from(len - 1) else {
            return false;
        };
        let Some(end) = start.checked_add(offset) else {
            return false;
        };
        end <= MAX_DEVICE
            && (area != WriteArea::D || start > 3001 || end < 3000)
            && self
                .ranges
                .iter()
                .any(|&(allowed_area, allowed_start, allowed_end)| {
                    area == allowed_area && start >= allowed_start && end <= allowed_end
                })
    }
}

fn parse_device(value: &str) -> Result<(WriteArea, u32), (&'static str, &'static str)> {
    let (area, digits, radix) = match value.as_bytes().first() {
        Some(b'D') => (WriteArea::D, &value[1..], 10),
        Some(b'W') => (WriteArea::W, &value[1..], 16),
        Some(b'M') => (WriteArea::M, &value[1..], 10),
        Some(b'Y') => (WriteArea::Y, &value[1..], 16),
        Some(b'L') => (WriteArea::L, &value[1..], 10),
        Some(b'B') => (WriteArea::B, &value[1..], 16),
        _ => return Err((ALLOWLIST_KEY, "expected D/W/M/Y/L/B address")),
    };
    if digits.is_empty()
        || !digits.bytes().all(|ch| {
            if radix == 10 {
                ch.is_ascii_digit()
            } else {
                ch.is_ascii_hexdigit() && !ch.is_ascii_lowercase()
            }
        })
    {
        return Err((ALLOWLIST_KEY, "invalid write address"));
    }
    let number = u32::from_str_radix(digits, radix)
        .map_err(|_| (ALLOWLIST_KEY, "write address is out of range"))?;
    if number > MAX_DEVICE {
        return Err((ALLOWLIST_KEY, "write address is out of range"));
    }
    Ok((area, number))
}

/// `D<decimal>.<hex>` or `W<HEX>.<hex>`; uppercase hexadecimal only. Returns
/// the inclusive bit position `word * 16 + bit`.
fn parse_word_bit(value: &str) -> Result<(WriteArea, u64), (&'static str, &'static str)> {
    let (word, bit) = value
        .split_once('.')
        .ok_or((ALLOWLIST_KEY, "bit range ends must both be word bits"))?;
    if !matches!(word.as_bytes().first(), Some(b'D' | b'W')) {
        return Err((ALLOWLIST_KEY, "word-bit ranges require D or W"));
    }
    let (area, number) = parse_device(word)?;
    if bit.len() != 1
        || !bit
            .bytes()
            .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_lowercase())
    {
        return Err((
            ALLOWLIST_KEY,
            "word bit must be one uppercase hexadecimal digit",
        ));
    }
    let bit = u64::from_str_radix(bit, 16).map_err(|_| (ALLOWLIST_KEY, "invalid word bit"))?;
    Ok((area, u64::from(number) * 16 + bit))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(mode: &str, allowlist: &str) -> Settings {
        Settings::from_pairs([(MODE_KEY, mode), (ALLOWLIST_KEY, allowlist)]).unwrap()
    }

    #[test]
    fn default_disabled_and_explicit_range_only() {
        let disabled = WritePolicy::parse(&Settings::new()).unwrap();
        assert!(!disabled.allows_device_span(WriteArea::D, 3010, 1));
        let enabled = WritePolicy::parse(&settings("enabled", "D3010-D3013,D4000-D4009")).unwrap();
        assert!(enabled.allows_device_span(WriteArea::D, 3010, 4));
        assert!(enabled.allows_device_span(WriteArea::D, 4003, 2));
        assert!(!enabled.allows_device_span(WriteArea::W, 0x1a, 1));
        for (start, len) in [
            (3000, 1),
            (3001, 1),
            (3002, 10),
            (3010, 5),
            (3013, 2),
            (3999, 2),
            (4000, 11),
            (3010, 0),
            (3010, 961),
        ] {
            assert!(
                !enabled.allows_device_span(WriteArea::D, start, len),
                "{start} / {len}"
            );
        }
    }

    #[test]
    fn one_common_policy_requires_an_explicit_w_range() {
        let policy = WritePolicy::parse(&settings("enabled", "D3010-D3013,W1A-W1F")).unwrap();
        assert!(policy.allows_device_span(WriteArea::D, 3010, 4));
        assert!(policy.allows_device_span(WriteArea::W, 0x1a, 6));
        assert!(!policy.allows_device_span(WriteArea::W, 0x1f, 2));
        assert!(!policy.allows_device_span(WriteArea::D, 0x1a, 1));
        for bad in ["W1A-D27", "W1A-W20,W1F-W21", "W1G-W1F", "w1a-w1f"] {
            assert!(
                WritePolicy::parse(&settings("enabled", bad)).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn bit_areas_use_the_same_explicit_policy_and_point_limit() {
        let old = WritePolicy::parse(&settings("enabled", "D3010-D3013,W1A-W1F")).unwrap();
        assert!(!old.allows_device_span(WriteArea::M, 10, 1));
        let policy = WritePolicy::parse(&settings(
            "enabled",
            "D3010-D3013,M10-M17,Y1A-Y1F,L10-L10,B20-B21",
        ))
        .unwrap();
        assert!(policy.allows_device_span(WriteArea::M, 10, 8));
        assert!(policy.allows_device_span(WriteArea::Y, 0x1a, 6));
        assert!(policy.allows_device_span(WriteArea::L, 10, 1));
        assert!(policy.allows_device_span(WriteArea::B, 0x20, 2));
        assert!(!policy.allows_device_span(WriteArea::M, 17, 2));
        assert!(!policy.allows_device_span(WriteArea::D, 3010, 5));
        assert!(WritePolicy::parse(&settings("enabled", "M0-M7167")).is_ok());
        assert!(WritePolicy::parse(&settings("enabled", "M0-M7168")).is_err());
        assert!(WritePolicy::parse(&settings("enabled", "D3000-D3002")).is_err());
        assert!(WritePolicy::parse(&settings("enabled", "Y1a-Y1F")).is_err());
    }

    #[test]
    fn malformed_or_broad_policy_fails_closed() {
        for (mode, ranges) in [
            ("enabled", ""),
            ("enabled", "D2999-D3002"),
            ("enabled", "D3001-D3001"),
            ("enabled", "D0-D960"),
            ("enabled", "D3013-D3010"),
            ("enabled", "D3010-D3013,D3013-D3014"),
            ("enabled", "D4000-D4001,D3010-D3013"),
            ("enabled", "D3010-D3013,"),
            ("enabled", "D3010 - D3013"),
            ("enabled", "d3010-d3013"),
            ("enabled", "D16777215-D16777216"),
            ("yes", "D3010-D3013"),
        ] {
            assert!(
                WritePolicy::parse(&settings(mode, ranges)).is_err(),
                "{mode} / {ranges}"
            );
        }
        let combined = Settings::from_pairs([
            (MODE_KEY, "enabled"),
            (ALLOWLIST_KEY, "D3010-D3013"),
            (Q_TEST_KEY, "enabled"),
        ])
        .unwrap();
        assert!(WritePolicy::parse(&combined).is_err());
    }

    #[test]
    fn overlap_is_rejected_even_when_another_area_sits_between() {
        for bad in [
            "D3010-D3013,W1A-W1F,D3012-D3014",
            "D3010-D3013,W1A-W1F,D3013-D3014",
            "W1A-W1F,D3010-D3013,W1C-W1D",
            "W1A-W1F,D3010-D3013,W1F-W20",
        ] {
            assert_eq!(
                WritePolicy::parse(&settings("enabled", bad)).err(),
                Some((ALLOWLIST_KEY, "write ranges must be ordered and disjoint")),
                "{bad}"
            );
        }
        let ok =
            WritePolicy::parse(&settings("enabled", "D3010-D3011,W1A-W1F,D3020-D3021")).unwrap();
        assert!(ok.allows_device_span(WriteArea::D, 3010, 2));
        assert!(ok.allows_device_span(WriteArea::W, 0x1a, 6));
        assert!(ok.allows_device_span(WriteArea::D, 3020, 2));
        assert!(!ok.allows_device_span(WriteArea::D, 3012, 1));
    }

    #[test]
    fn word_bit_intervals_are_separate_from_word_intervals() {
        let policy = WritePolicy::parse(&settings(
            "enabled",
            "D100-D105,D100.3-D100.5,W1A.0-W1B.F,D200.F-D201.1",
        ))
        .unwrap();
        assert!(policy.allows_word_bits(WriteArea::D, 100, &[3, 4, 5]));
        assert!(!policy.allows_word_bits(WriteArea::D, 100, &[2]));
        assert!(!policy.allows_word_bits(WriteArea::D, 100, &[5, 6]));
        assert!(
            !policy.allows_word_bits(WriteArea::D, 101, &[0]),
            "a word interval does not authorize bits"
        );
        assert!(policy.allows_word_bits(WriteArea::W, 0x1a, &[0, 15]));
        assert!(policy.allows_word_bits(WriteArea::W, 0x1b, &[15]));
        assert!(!policy.allows_word_bits(WriteArea::W, 0x1c, &[0]));
        assert!(!policy.allows_word_bits(WriteArea::D, 0x1a, &[0]));
        assert!(policy.allows_word_bits(WriteArea::D, 200, &[15]));
        assert!(policy.allows_word_bits(WriteArea::D, 201, &[0, 1]));
        assert!(!policy.allows_word_bits(WriteArea::D, 200, &[14]));
        assert!(!policy.allows_word_bits(WriteArea::D, 201, &[2]));
        assert!(!policy.allows_word_bits(WriteArea::D, 100, &[]));
        assert!(!policy.allows_word_bits(WriteArea::D, 100, &[16]));
        assert!(!policy.allows_word_bits(WriteArea::M, 100, &[3]));
        // Bit intervals never authorize word writes.
        let bits_only = WritePolicy::parse(&settings("enabled", "D100.0-D100.F")).unwrap();
        assert!(!bits_only.allows_device_span(WriteArea::D, 100, 1));
        assert!(policy.allows_device_span(WriteArea::D, 100, 6));
        let disabled = WritePolicy::parse(&settings("disabled", "D100.0-D100.F")).unwrap();
        assert!(!disabled.allows_word_bits(WriteArea::D, 100, &[0]));
        // Word and bit intervals are ordered independently per device.
        for ok in [
            "D100.0-D100.F,D50-D60",
            "D200-D205,D100.0-D100.1",
            "W1A.0-W1A.1,D5.0-D5.1",
            "D2999.0-D2999.F,D3002.0-D3002.F",
        ] {
            assert!(WritePolicy::parse(&settings("enabled", ok)).is_ok(), "{ok}");
        }
        let max = WritePolicy::parse(&settings("enabled", "D0.0-D447.F")).unwrap();
        assert!(max.allows_word_bits(WriteArea::D, 447, &[15]));
    }

    #[test]
    fn malformed_word_bit_intervals_fail_closed() {
        for ranges in [
            "D100.5-D100.3",
            "D100.3-D100.3,D100.3-D100.4",
            "D100.4-D100.5,D100.0-D100.1",
            "D100.3-D101",
            "D100-D101.3",
            "D100.3-W64.3",
            "D100.a-D100.b",
            "W1a.0-W1a.1",
            "W1A.f-W1A.f",
            "D100.10-D100.11",
            "D100.-D100.1",
            "D.1-D.2",
            "M10.0-M10.1",
            "Y1.0-Y1.1",
            "SD10.0-SD10.1",
            "D2999.0-D3000.0",
            "D3001.F-D3001.F",
            "D3000.0-D3000.0",
            "D0.0-D448.0",
            "D16777216.0-D16777216.1",
            "D100.3 - D100.5",
            "D100.3-D100.5,",
        ] {
            assert!(
                WritePolicy::parse(&settings("enabled", ranges)).is_err(),
                "{ranges}"
            );
        }
        // The 16-interval cap is shared between word and bit intervals.
        let sixteen = (0..8)
            .map(|n| {
                format!(
                    "D{}-D{},D{}.0-D{}.0",
                    4000 + n,
                    4000 + n,
                    5000 + n,
                    5000 + n
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        assert!(WritePolicy::parse(&settings("enabled", &sixteen)).is_ok());
        let seventeen = format!("{sixteen},D6000.0-D6000.0");
        assert!(WritePolicy::parse(&settings("enabled", &seventeen)).is_err());
        let long = "D4000.0-D4000.0,".repeat(17);
        assert!(WritePolicy::parse(&settings("enabled", &long)).is_err());
        // A bit-only allowlist still conflicts with the Q test write.
        let combined = Settings::from_pairs([
            (MODE_KEY, "enabled"),
            (ALLOWLIST_KEY, "D100.0-D100.1"),
            (Q_TEST_KEY, "enabled"),
        ])
        .unwrap();
        assert!(WritePolicy::parse(&combined).is_err());
    }

    #[test]
    fn previously_valid_word_and_bit_device_values_parse_identically() {
        for (raw, ranges) in [
            ("D3010-D3013", vec![(WriteArea::D, 3010, 3013)]),
            (
                "D3010-D3013,W1A-W1F",
                vec![(WriteArea::D, 3010, 3013), (WriteArea::W, 0x1a, 0x1f)],
            ),
            (
                "D3010-D3013,M10-M17,Y1A-Y1F,L10-L10,B20-B21",
                vec![
                    (WriteArea::D, 3010, 3013),
                    (WriteArea::M, 10, 17),
                    (WriteArea::Y, 0x1a, 0x1f),
                    (WriteArea::L, 10, 10),
                    (WriteArea::B, 0x20, 0x21),
                ],
            ),
            ("M0-M7167", vec![(WriteArea::M, 0, 7167)]),
        ] {
            assert_eq!(
                WritePolicy::parse(&settings("enabled", raw)).unwrap(),
                WritePolicy {
                    enabled: true,
                    ranges,
                    bit_ranges: Vec::new(),
                },
                "{raw}"
            );
        }
        assert_eq!(
            WritePolicy::parse(&Settings::new()).unwrap(),
            WritePolicy {
                enabled: false,
                ranges: Vec::new(),
                bit_ranges: Vec::new(),
            }
        );
    }
}
