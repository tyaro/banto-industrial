//! Host-owned, per-connection allowlist for an explicitly selected signed
//! write component. Parsing is independent of the guest's own SD checks.
//!
//! Word intervals (`D100-D105`) authorize word and bit-device writes. Word-bit
//! intervals (`D100.3-D100.5`, `W1A.0-W1B.F`) are a separate list that alone
//! authorizes the guest's read-modify-write of single bits inside one D/W word;
//! a word interval never authorizes a bit, and a bit interval never a word.
//! The grammar mirrors the SLMP guest's `write_policy.rs` (banto-driver-wasm
//! #61) exactly, so the Host accepts the same saved values and nothing looser.
//!
//! G3 GX Works3 global labels (banto-driver-wasm #74) have their own list,
//! `label_write_allowlist`, under the same `d_u16_write_mode`: `;`-separated
//! canonical labels (`@Tank1.Level`) or ranges of the last subscript
//! (`@Arr[0-9]`, `@M[1,0-15]`), no whitespace, control characters, `*`, empty
//! or duplicate entries, at most 32 entries and 2048 characters, matched
//! case-sensitively. It mirrors the guest's `label.rs` (`LabelAllowlist`,
//! `parse_entry`, `parse_name`) line by line. An enabled mode needs at least
//! one of the two lists to be nonempty (before G3 the D/W list was required);
//! every value saved before G3 keeps its meaning, and the Q test write still
//! conflicts with an enabled mode whichever list enables it.

use banto_driver_api::Settings;

pub(super) const MODE_KEY: &str = "d_u16_write_mode";
pub(super) const ALLOWLIST_KEY: &str = "d_u16_write_allowlist";
pub(super) const LABEL_ALLOWLIST_KEY: &str = "label_write_allowlist";
/// The `area` the SLMP guest reports for every global label (`@Name`).
pub(super) const LABEL_AREA: &str = "@";
const Q_TEST_KEY: &str = "q25prh_d3010_3013_test_write";
pub(super) const MAX_WORDS: usize = 960;
pub(super) const MAX_BITS: usize = 7168;
const MAX_INTERVALS: usize = 16;
const MAX_TEXT_LEN: usize = 256;
const MAX_DEVICE: u32 = 0x00ff_ffff;
/// `label_write_allowlist` limits, counted in characters as the guest does.
pub(super) const MAX_LABEL_ALLOWLIST_LEN: usize = 2048;
const MAX_LABEL_ALLOWLIST_ENTRIES: usize = 32;
const MAX_LABEL_DIMENSIONS: usize = 3;

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
    /// G3 `label_write_allowlist` entries.
    labels: Vec<LabelEntry>,
}

/// One `label_write_allowlist` entry (the guest's `AllowEntry`).
#[derive(Debug, Clone, PartialEq, Eq)]
enum LabelEntry {
    Exact(String),
    /// Same canonical head and tail, last subscript within `start..=end`.
    Range {
        head: String,
        start: u32,
        end: u32,
        tail: String,
    },
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
        let labels = parse_label_allowlist(settings.get(LABEL_ALLOWLIST_KEY))?;
        if enabled && ranges.is_empty() && bit_ranges.is_empty() && labels.is_empty() {
            return Err((
                ALLOWLIST_KEY,
                "enabled write requires an explicit device or label allowlist",
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
            labels,
        })
    }

    /// G3: a label write needs the common enabled mode and a
    /// `label_write_allowlist` entry: the exact canonical label, or the same
    /// head and tail with the last subscript inside a range (case-sensitive).
    /// `canonical` is the guest's canonical text (`@Arr[3]`); anything that is
    /// not a canonical label is refused.
    pub(super) fn allows_label(&self, canonical: &str) -> bool {
        if !self.enabled {
            return false;
        }
        let Some((head, last)) = canonical_label(canonical) else {
            return false;
        };
        self.labels.iter().any(|entry| match entry {
            LabelEntry::Exact(text) => text == canonical,
            LabelEntry::Range {
                head: allowed_head,
                start,
                end,
                tail,
            } => last.as_ref().is_some_and(|(number, label_tail)| {
                head == *allowed_head && label_tail == tail && (start..=end).contains(&number)
            }),
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

/// A label split into its canonical head (with `@`) and, when it has a
/// subscript, the last subscript and the canonical text after it.
type SplitLabel = (String, Option<(u32, String)>);

/// The guest's `parse_name`: split a label name (without `@`) into canonical
/// text before the last subscript (with `@`) and, when there is one, that
/// subscript and the canonical text after it. The name is kept as written
/// (case and non-ASCII characters); whitespace, control characters, a `.`
/// followed by a digit (bit specification), empty members and malformed or
/// more than three-dimensional decimal subscripts are errors. Leading zeros of
/// subscripts are dropped in the canonical text.
fn parse_label_name(name: &str) -> Result<SplitLabel, &'static str> {
    if name.is_empty() {
        return Err("label name is empty");
    }
    if name.chars().any(char::is_whitespace) {
        return Err("label name must not contain whitespace");
    }
    if name.chars().any(char::is_control) {
        return Err("label name must not contain control characters");
    }
    let mut out = String::with_capacity(name.len());
    // Byte range of the most recent subscript number in `out`.
    let mut last: Option<(usize, usize, u32)> = None;
    for (index, member) in name.split('.').enumerate() {
        if index > 0 {
            if member.starts_with(|c: char| c.is_ascii_digit()) {
                return Err("bit specification (.n) of a label is not supported");
            }
            out.push('.');
        }
        let (base, subscripts) = match member.split_once('[') {
            Some((base, rest)) => {
                let inner = rest
                    .strip_suffix(']')
                    .ok_or("label subscript must close with ] at the end of a member")?;
                (base, Some(inner))
            }
            None => (member, None),
        };
        if base.is_empty() {
            return Err("label name has an empty member");
        }
        if base.contains([']', ',']) {
            return Err("malformed label subscript");
        }
        out.push_str(base);
        let Some(inner) = subscripts else {
            continue;
        };
        let parts = inner.split(',').collect::<Vec<_>>();
        if parts.len() > MAX_LABEL_DIMENSIONS {
            return Err("label arrays have at most three dimensions");
        }
        out.push('[');
        for (position, part) in parts.iter().enumerate() {
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                return Err("label subscript must be a decimal number");
            }
            let number = part
                .parse::<u32>()
                .map_err(|_| "label subscript exceeds u32")?;
            if position > 0 {
                out.push(',');
            }
            let start = out.len();
            out.push_str(&number.to_string());
            last = Some((start, out.len(), number));
        }
        out.push(']');
    }
    Ok(match last {
        Some((start, end, number)) => (
            format!("@{}", &out[..start]),
            Some((number, out[end..].to_owned())),
        ),
        None => (format!("@{out}"), None),
    })
}

/// A label written by the guest's canonical text (`@` and the name, decimal
/// subscripts without leading zeros), split as [`parse_label_name`] does.
/// `None` for anything else, so a non-canonical address never matches.
fn canonical_label(text: &str) -> Option<SplitLabel> {
    let (head, last) = parse_label_name(text.strip_prefix('@')?).ok()?;
    let canonical = match &last {
        Some((number, tail)) => format!("{head}{number}{tail}"),
        None => head.clone(),
    };
    (canonical == text).then_some((head, last))
}

/// The guest's `canonical_decimal`: decimal digits without a leading zero.
fn canonical_decimal(text: &str) -> Option<u32> {
    if text.is_empty()
        || !text.bytes().all(|b| b.is_ascii_digit())
        || (text.len() > 1 && text.starts_with('0'))
    {
        return None;
    }
    text.parse().ok()
}

/// The guest's `parse_entry`: a canonical label, or a canonical label whose
/// last subscript is `start-end` (both canonical decimals, start <= end).
fn parse_label_entry(entry: &str) -> Result<LabelEntry, &'static str> {
    const NOT_CANONICAL: &str = "label allowlist entries must be canonical @labels";
    let name = entry.strip_prefix('@').ok_or(NOT_CANONICAL)?;
    // A range sits in the last element of the last subscript group.
    let range = name.rfind('[').and_then(|open| {
        let close = open + name[open..].find(']')?;
        let element_start = name[open + 1..close]
            .rfind(',')
            .map_or(open + 1, |comma| open + 2 + comma);
        let (start, end) = name[element_start..close].split_once('-')?;
        Some((element_start, close, start, end))
    });
    if let Some((element_start, close, start, end)) = range {
        let (Some(start), Some(end)) = (canonical_decimal(start), canonical_decimal(end)) else {
            return Err("label allowlist range must be decimal start-end without leading zeros");
        };
        if start > end {
            return Err("label allowlist range is reversed");
        }
        let probe = format!("{}{start}{}", &name[..element_start], &name[close..]);
        let (head, last) = parse_label_name(&probe).map_err(|_| NOT_CANONICAL)?;
        let Some((number, tail)) = last else {
            return Err(NOT_CANONICAL);
        };
        if format!("{head}{number}{tail}") != format!("@{probe}") {
            return Err(NOT_CANONICAL);
        }
        return Ok(LabelEntry::Range {
            head,
            start,
            end,
            tail,
        });
    }
    let (head, last) = parse_label_name(name).map_err(|_| NOT_CANONICAL)?;
    let canonical = match last {
        Some((number, tail)) => format!("{head}{number}{tail}"),
        None => head,
    };
    if canonical != entry {
        return Err(NOT_CANONICAL);
    }
    Ok(LabelEntry::Exact(canonical))
}

/// The guest's `LabelAllowlist::parse`, with the same checks in the same order.
fn parse_label_allowlist(
    raw: Option<&str>,
) -> Result<Vec<LabelEntry>, (&'static str, &'static str)> {
    let fail = |message| (LABEL_ALLOWLIST_KEY, message);
    let raw = raw.unwrap_or("");
    if raw.chars().count() > MAX_LABEL_ALLOWLIST_LEN {
        return Err(fail("label write allowlist is too long"));
    }
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    if raw.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(fail("label write allowlist must not contain whitespace"));
    }
    if raw.contains('*') {
        return Err(fail("label write allowlist has no wildcards"));
    }
    let mut entries = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for entry in raw.split(';') {
        if entry.is_empty() {
            return Err(fail("label write allowlist has an empty entry"));
        }
        if entries.len() == MAX_LABEL_ALLOWLIST_ENTRIES {
            return Err(fail("too many label write allowlist entries"));
        }
        if seen.contains(&entry) {
            return Err(fail("duplicate label write allowlist entry"));
        }
        seen.push(entry);
        entries.push(parse_label_entry(entry).map_err(fail)?);
    }
    Ok(entries)
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
                    labels: Vec::new(),
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
                labels: Vec::new(),
            }
        );
    }

    fn label_settings(mode: &str, devices: &str, labels: &str) -> Settings {
        Settings::from_pairs([
            (MODE_KEY, mode),
            (ALLOWLIST_KEY, devices),
            (LABEL_ALLOWLIST_KEY, labels),
        ])
        .unwrap()
    }

    fn label_policy(labels: &str) -> Result<WritePolicy, (&'static str, &'static str)> {
        WritePolicy::parse(&label_settings("enabled", "", labels))
    }

    #[test]
    fn label_allowlist_matches_exact_labels_and_last_subscript_ranges() {
        // The guest's `allowlist_matches_exact_labels_and_last_subscript_ranges`.
        let policy =
            label_policy("@Tank1.Level;@Arr[0-9];@M[1,0-15];@St.A[2-4].x;@Single[7];@タンク.水位")
                .unwrap();
        for text in [
            "@Tank1.Level",
            "@Arr[0]",
            "@Arr[9]",
            "@M[1,0]",
            "@M[1,15]",
            "@St.A[3].x",
            "@Single[7]",
            "@タンク.水位",
        ] {
            assert!(policy.allows_label(text), "{text}");
        }
        for text in [
            "@tank1.level",
            "@Tank1",
            "@Tank1.Level2",
            "@Arr[10]",
            "@Arr",
            "@Arr[1,1]",
            "@M[2,0]",
            "@M[1,16]",
            "@M[0,1,5]",
            "@St.A[3].y",
            "@St.A[3]",
            "@Single[6]",
            "@XArr[1]",
            // Only the guest's canonical text matches (the guest canonicalizes
            // `@Arr[009]` to `@Arr[9]` before the Hub sees it).
            "@Arr[009]",
            "@Arr[ 1]",
            "Tank1.Level",
            "",
        ] {
            assert!(!policy.allows_label(text), "{text}");
        }
        // Labels never authorize devices, and devices never authorize labels.
        assert!(!policy.allows_device_span(WriteArea::D, 100, 1));
        let devices = WritePolicy::parse(&settings("enabled", "D100-D105")).unwrap();
        assert!(!devices.allows_label("@Tank1.Level"));
        let disabled = WritePolicy::parse(&label_settings("disabled", "", "@Tank1.Level")).unwrap();
        assert!(!disabled.allows_label("@Tank1.Level"));
    }

    #[test]
    fn enabled_needs_either_list_and_still_conflicts_with_the_q_test_write() {
        assert!(label_policy("@A").is_ok());
        assert!(WritePolicy::parse(&label_settings("enabled", "D100-D105", "")).is_ok());
        assert!(WritePolicy::parse(&label_settings("enabled", "D100-D105", "@A")).is_ok());
        assert_eq!(
            WritePolicy::parse(&label_settings("enabled", "", "")).err(),
            Some((
                ALLOWLIST_KEY,
                "enabled write requires an explicit device or label allowlist"
            ))
        );
        // A label list alone is saved but writes nothing while disabled.
        assert!(WritePolicy::parse(&label_settings("disabled", "", "@A")).is_ok());
        let combined = Settings::from_pairs([
            (MODE_KEY, "enabled"),
            (LABEL_ALLOWLIST_KEY, "@A"),
            (Q_TEST_KEY, "enabled"),
        ])
        .unwrap();
        assert_eq!(
            WritePolicy::parse(&combined).err(),
            Some((
                MODE_KEY,
                "normal write and Q test write cannot both be enabled"
            ))
        );
    }

    #[test]
    fn malformed_label_allowlists_fail_closed() {
        // The guest's `malformed_allowlists_fail_closed`, plus a few more.
        for raw in [
            "@A;",
            ";@A",
            "@A;;@B",
            "@A;@A",
            "@A @B",
            "@A; @B",
            "@A,@B",
            "A",
            "@",
            "@Arr[03]",
            "@Arr[3-1]",
            "@Arr[01-3]",
            "@Arr[1-03]",
            "@Arr[1-]",
            "@Arr[-1]",
            "@Arr[*]",
            "@Arr*",
            "@X.3",
            "@Arr[1-2,3]",
            "@Arr[0-4294967296]",
            "@A\u{7}",
            "@A\u{3000}",
            "@Arr[1][2]",
            "@Arr[1,2,3,4]",
            "@A..B",
        ] {
            let error = label_policy(raw).err();
            assert_eq!(
                error.map(|(key, _)| key),
                Some(LABEL_ALLOWLIST_KEY),
                "{raw}"
            );
            // The device list beside it does not rescue a bad label list.
            assert!(
                WritePolicy::parse(&label_settings("enabled", "D100-D105", raw)).is_err(),
                "{raw}"
            );
            assert!(
                WritePolicy::parse(&label_settings("disabled", "", raw)).is_err(),
                "{raw}"
            );
        }
        let entries = |n: usize| {
            (0..n)
                .map(|i| format!("@L{i}"))
                .collect::<Vec<_>>()
                .join(";")
        };
        assert_eq!(label_policy(&entries(32)).map(|p| p.labels.len()), Ok(32));
        assert!(label_policy(&entries(33)).is_err());
        // 2048 characters, counted as characters (a non-ASCII name is not
        // counted in bytes).
        assert!(label_policy(&format!("@{}", "A".repeat(2047))).is_ok());
        assert!(label_policy(&format!("@{}", "A".repeat(2048))).is_err());
        assert!(label_policy(&format!("@{}", "水".repeat(2047))).is_ok());
        assert!(label_policy(&format!("@{}", "水".repeat(2048))).is_err());
    }

    #[test]
    fn label_entries_parse_to_the_guest_shape() {
        assert_eq!(
            label_policy("@Tank1.Level;@Arr[0-9];@M[1,0-15];@St.A[2-4].x;@Single[7]")
                .unwrap()
                .labels,
            vec![
                LabelEntry::Exact("@Tank1.Level".into()),
                LabelEntry::Range {
                    head: "@Arr[".into(),
                    start: 0,
                    end: 9,
                    tail: "]".into(),
                },
                LabelEntry::Range {
                    head: "@M[1,".into(),
                    start: 0,
                    end: 15,
                    tail: "]".into(),
                },
                LabelEntry::Range {
                    head: "@St.A[".into(),
                    start: 2,
                    end: 4,
                    tail: "].x".into(),
                },
                LabelEntry::Exact("@Single[7]".into()),
            ]
        );
    }

    #[test]
    fn previously_valid_values_parse_identically_beside_an_empty_label_list() {
        for raw in [
            "D3010-D3013",
            "D3010-D3013,W1A-W1F",
            "D3010-D3013,M10-M17,Y1A-Y1F,L10-L10,B20-B21",
            "D100-D105,D100.3-D100.5,W1A.0-W1B.F",
        ] {
            let before = WritePolicy::parse(&settings("enabled", raw)).unwrap();
            assert!(before.labels.is_empty());
            assert_eq!(
                WritePolicy::parse(&label_settings("enabled", raw, "")).unwrap(),
                before,
                "{raw}"
            );
        }
    }
}
