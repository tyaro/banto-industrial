//! Local calendar date, and the pure UTC-epoch-ms <-> proleptic-Gregorian
//! conversion it is built on (recorder-requirements.md §3.4: rotation
//! boundary is "ローカル深夜0時", data file names embed `YYYYMMDD`).
//!
//! The day-count <-> (year, month, day) conversion is Howard Hinnant's
//! well-known constexpr civil-calendar algorithm
//! (<https://howardhinnant.github.io/date_algorithms.html>), chosen so this
//! module needs no date/calendar crate at all - only integer arithmetic over
//! `i64` "days since 1970-01-01" - keeping [`crate::clock::Clock`]'s
//! `utc_offset_ms` the *only* place this crate reaches for anything beyond
//! `std` (see `clock.rs`'s module doc).

/// A local (not UTC) calendar date - the unit data files are named/rotated
/// by. `Ord`/`PartialOrd` derive in field-declaration order (year, month,
/// day), which is already chronological order, so sorting a `Vec<LocalDate>`
/// or comparing two dates with `<`/`>` "just works".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocalDate {
    pub year: i32,
    pub month: u32,
    pub day: u32,
}

impl LocalDate {
    pub fn new(year: i32, month: u32, day: u32) -> Self {
        Self { year, month, day }
    }

    /// The local calendar date containing `epoch_ms` (UTC epoch
    /// milliseconds) once shifted by `utc_offset_ms` (see
    /// [`crate::clock::Clock::utc_offset_ms`]).
    pub fn from_epoch_ms(epoch_ms: i64, utc_offset_ms: i64) -> Self {
        let local_ms = epoch_ms + utc_offset_ms;
        let days = local_ms.div_euclid(MS_PER_DAY);
        let (year, month, day) = civil_from_days(days);
        Self { year, month, day }
    }

    /// Days since the epoch (1970-01-01 = 0), for retention/ordering
    /// arithmetic ([`crate::files::prune_files`]).
    pub fn to_days_since_epoch(self) -> i64 {
        days_from_civil(self.year, self.month, self.day)
    }

    /// `YYYYMMDD`, the prefix every data file name starts with (`schema.rs`).
    pub fn to_yyyymmdd(self) -> String {
        format!("{:04}{:02}{:02}", self.year, self.month, self.day)
    }

    /// Parse an exactly-8-digit `YYYYMMDD` string. Deliberately does not
    /// validate month/day ranges beyond what `civil_from_days`/round-tripping
    /// would - a filename that happens to contain a nonsense date like
    /// `"20260199"` still parses (as *some* `LocalDate`, arithmetically
    /// consistent or not); [`crate::files::list_data_files`]'s callers only
    /// ever compare/sort these, never treat them as a validated calendar
    /// input, so rejecting non-digit shapes is enough.
    pub fn parse_yyyymmdd(s: &str) -> Option<Self> {
        if s.len() != 8 || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let year: i32 = s[0..4].parse().ok()?;
        let month: u32 = s[4..6].parse().ok()?;
        let day: u32 = s[6..8].parse().ok()?;
        Some(Self { year, month, day })
    }
}

const MS_PER_DAY: i64 = 86_400_000;

/// Days since 1970-01-01 for a proleptic-Gregorian (year, month, day).
/// `month` is 1-12, `day` is 1-31 (not range-checked - see
/// [`LocalDate::parse_yyyymmdd`]'s doc comment on why that is fine here).
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let y: i64 = if month <= 2 {
        year as i64 - 1
    } else {
        year as i64
    };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if month > 2 { month - 3 } else { month + 9 } as i64; // [0, 11]
    let doy = (153 * mp + 2) / 5 + day as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// Inverse of [`days_from_civil`].
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let year = if month <= 2 { y + 1 } else { y };
    (year as i32, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_is_day_zero() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
    }

    #[test]
    fn day_before_epoch_is_minus_one() {
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }

    #[test]
    fn known_date_2026_07_12() {
        // Cross-checked against Python's `datetime.date` subtraction:
        // (2026-07-12 - 1970-01-01).days == 20646.
        assert_eq!(days_from_civil(2026, 7, 12), 20_646);
        assert_eq!(civil_from_days(20_646), (2026, 7, 12));
    }

    #[test]
    fn leap_year_feb_29_round_trips() {
        let days = days_from_civil(2024, 2, 29);
        assert_eq!(civil_from_days(days), (2024, 2, 29));
        // The next day is March 1st, not a nonsense date.
        assert_eq!(civil_from_days(days + 1), (2024, 3, 1));
    }

    #[test]
    fn century_non_leap_year_feb_is_28_days() {
        // 1900 and 2100 are NOT leap years (divisible by 100, not 400).
        let feb28 = days_from_civil(1900, 2, 28);
        assert_eq!(civil_from_days(feb28 + 1), (1900, 3, 1));
    }

    #[test]
    fn year_boundary_round_trips() {
        let dec31 = days_from_civil(2025, 12, 31);
        assert_eq!(civil_from_days(dec31 + 1), (2026, 1, 1));
    }

    #[test]
    fn round_trip_over_a_wide_range() {
        for days in (-40_000..40_000).step_by(37) {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days, "days={days} -> {y}-{m}-{d}");
        }
    }

    #[test]
    fn from_epoch_ms_matches_civil_at_utc() {
        // 2026-07-12T00:00:00Z, offset 0.
        let epoch_ms = 20_646 * MS_PER_DAY;
        let d = LocalDate::from_epoch_ms(epoch_ms, 0);
        assert_eq!(d, LocalDate::new(2026, 7, 12));
    }

    #[test]
    fn from_epoch_ms_applies_positive_offset_across_midnight() {
        // 2026-07-11T23:30:00Z is 2026-07-12T08:30 JST (+9h).
        let epoch_ms = 20_646 * MS_PER_DAY - 30 * 60_000;
        let jst_offset_ms = 9 * 3_600_000;
        let d = LocalDate::from_epoch_ms(epoch_ms, jst_offset_ms);
        assert_eq!(d, LocalDate::new(2026, 7, 12));
        // Same instant at UTC is still 2026-07-11.
        let d_utc = LocalDate::from_epoch_ms(epoch_ms, 0);
        assert_eq!(d_utc, LocalDate::new(2026, 7, 11));
    }

    #[test]
    fn from_epoch_ms_applies_negative_offset_across_midnight() {
        // 2026-07-12T00:30:00Z is 2026-07-11T16:30 in UTC-8.
        let epoch_ms = 20_646 * MS_PER_DAY + 30 * 60_000;
        let offset_ms = -8 * 3_600_000;
        let d = LocalDate::from_epoch_ms(epoch_ms, offset_ms);
        assert_eq!(d, LocalDate::new(2026, 7, 11));
    }

    #[test]
    fn to_yyyymmdd_zero_pads() {
        assert_eq!(LocalDate::new(2026, 7, 1).to_yyyymmdd(), "20260701");
        assert_eq!(LocalDate::new(1, 1, 1).to_yyyymmdd(), "00010101");
    }

    #[test]
    fn parse_yyyymmdd_round_trips() {
        let d = LocalDate::new(2026, 7, 12);
        assert_eq!(LocalDate::parse_yyyymmdd(&d.to_yyyymmdd()), Some(d));
    }

    #[test]
    fn parse_yyyymmdd_rejects_wrong_length_or_non_digits() {
        assert_eq!(LocalDate::parse_yyyymmdd("2026712"), None);
        assert_eq!(LocalDate::parse_yyyymmdd("2026071x"), None);
        assert_eq!(LocalDate::parse_yyyymmdd(""), None);
    }

    #[test]
    fn ordering_is_chronological() {
        let a = LocalDate::new(2026, 7, 11);
        let b = LocalDate::new(2026, 7, 12);
        let c = LocalDate::new(2026, 8, 1);
        let d = LocalDate::new(2027, 1, 1);
        assert!(a < b && b < c && c < d);
    }

    #[test]
    fn to_days_since_epoch_matches_days_from_civil() {
        let d = LocalDate::new(2026, 7, 12);
        assert_eq!(d.to_days_since_epoch(), 20_646);
    }
}
