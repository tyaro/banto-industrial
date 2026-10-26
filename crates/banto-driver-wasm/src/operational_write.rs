//! Host-owned, per-connection allowlist for an explicitly selected signed
//! write component. Parsing is independent of the guest's own SD checks.

use banto_driver_api::Settings;

pub(super) const MODE_KEY: &str = "d_u16_write_mode";
pub(super) const ALLOWLIST_KEY: &str = "d_u16_write_allowlist";
const Q_TEST_KEY: &str = "q25prh_d3010_3013_test_write";
pub(super) const MAX_WORDS: usize = 960;
const MAX_INTERVALS: usize = 16;
const MAX_TEXT_LEN: usize = 256;
const MAX_DEVICE: u32 = 0x00ff_ffff;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WriteArea {
    D,
    W,
}

impl WriteArea {
    pub(super) fn from_name(name: &str) -> Option<Self> {
        match name {
            "D" => Some(Self::D),
            "W" => Some(Self::W),
            _ => None,
        }
    }

    pub(super) fn canonical(self, index: u32) -> String {
        match self {
            Self::D => format!("D{index}"),
            Self::W => format!("W{index:X}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WritePolicy {
    enabled: bool,
    ranges: Vec<(WriteArea, u32, u32)>,
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
        if !raw.is_empty() {
            for interval in raw.split(',') {
                if ranges.len() == MAX_INTERVALS {
                    return Err((ALLOWLIST_KEY, "too many write ranges"));
                }
                let Some((start, end)) = interval.split_once('-') else {
                    return Err((ALLOWLIST_KEY, "expected same-area start-end ranges"));
                };
                let (area, start) = parse_device(start)?;
                let (end_area, end) = parse_device(end)?;
                if area != end_area {
                    return Err((ALLOWLIST_KEY, "write range must remain in one device area"));
                }
                if end < start || end - start >= MAX_WORDS as u32 {
                    return Err((
                        ALLOWLIST_KEY,
                        "write range is reversed or exceeds 960 words",
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
        if enabled && ranges.is_empty() {
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
        Ok(Self { enabled, ranges })
    }

    pub(super) fn allows_device_span(&self, area: WriteArea, start: u32, len: usize) -> bool {
        if !self.enabled || len == 0 || len > MAX_WORDS {
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
    let (area, digits, radix) = if let Some(digits) = value.strip_prefix('D') {
        (WriteArea::D, digits, 10)
    } else if let Some(digits) = value.strip_prefix('W') {
        (WriteArea::W, digits, 16)
    } else {
        return Err((ALLOWLIST_KEY, "expected D or W address"));
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
}
