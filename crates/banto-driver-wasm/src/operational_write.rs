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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WritePolicy {
    enabled: bool,
    ranges: Vec<(u32, u32)>,
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
                    return Err((ALLOWLIST_KEY, "expected Dstart-Dend ranges"));
                };
                let start = parse_device(start)?;
                let end = parse_device(end)?;
                if end < start || end - start >= MAX_WORDS as u32 {
                    return Err((
                        ALLOWLIST_KEY,
                        "write range is reversed or exceeds 960 words",
                    ));
                }
                if start <= 3001 && end >= 3000 {
                    return Err((ALLOWLIST_KEY, "D3000 and D3001 are protected"));
                }
                if ranges.last().is_some_and(|&(_, last)| start <= last) {
                    return Err((ALLOWLIST_KEY, "write ranges must be ordered and disjoint"));
                }
                ranges.push((start, end));
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

    pub(super) fn allows_span(&self, start: u32, len: usize) -> bool {
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
            && (start > 3001 || end < 3000)
            && self
                .ranges
                .iter()
                .any(|&(allowed_start, allowed_end)| start >= allowed_start && end <= allowed_end)
    }
}

fn parse_device(value: &str) -> Result<u32, (&'static str, &'static str)> {
    let digits = value
        .strip_prefix('D')
        .filter(|digits| !digits.is_empty() && digits.bytes().all(|ch| ch.is_ascii_digit()))
        .ok_or((ALLOWLIST_KEY, "expected decimal D addresses"))?;
    let number = digits
        .parse::<u32>()
        .map_err(|_| (ALLOWLIST_KEY, "write address is out of range"))?;
    if number > MAX_DEVICE {
        return Err((ALLOWLIST_KEY, "write address is out of range"));
    }
    Ok(number)
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
        assert!(!disabled.allows_span(3010, 1));
        let enabled = WritePolicy::parse(&settings("enabled", "D3010-D3013,D4000-D4009")).unwrap();
        assert!(enabled.allows_span(3010, 4));
        assert!(enabled.allows_span(4003, 2));
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
            assert!(!enabled.allows_span(start, len), "{start} / {len}");
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
