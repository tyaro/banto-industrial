//! The ptime/file-date contract (#424, owner decision 2026-09-24, 案 A): one
//! definition of "which data files could hold a row at time `t`", shared by
//! the write side ([`crate::writer::TsWriter::append`]) and the read side
//! (`banto-tsquery`'s `candidate_files`), so the two can never drift apart.
//!
//! ## Why this exists
//!
//! A data file's name carries a *local* calendar date (`YYYYMMDD`), but the
//! UTC offset that produced it is not recorded (see `clock.rs`: the writer
//! re-queries the OS offset on every rotation check). The reader therefore
//! pre-filters files by name alone: it pads the requested range by
//! [`MAX_OFFSET_PAD_MS`] on each side, converts both ends to a date **at UTC
//! offset 0**, and only opens files whose date falls in between
//! ([`candidate_date_range`]). A row whose `ptime` is far from its file's date
//! is invisible to every query - it is on disk, but no range that contains
//! its `ptime` ever opens that file.
//!
//! ## The contract
//!
//! A row at `ptime_ms` may only be stored in a file dated `date` if
//! [`ptime_is_findable_in`]`(date, ptime_ms)` holds, i.e. if a query for the
//! single-point range `[ptime_ms, ptime_ms]` selects that file. Because
//! [`candidate_date_range`] only widens as the range widens, the same file is
//! then selected by **every** query range that contains `ptime_ms`.
//!
//! In closed form (away from the `i64` extremes, where the padding
//! saturates), with `start(date)` = UTC midnight at the start of `date` as an
//! epoch-ms value (`date.to_days_since_epoch() * 86_400_000`):
//!
//! ```text
//! start(date) - 24h  <=  ptime_ms  <  start(date) + 48h
//! ```
//!
//! i.e. "the row's time is within 24h of the file's date" (the file's day,
//! widened by 24h on each side). A real local day under a UTC offset `o`
//! (`|o| <= 14h` for every real time zone) spans UTC
//! `[start(date) - o, start(date) - o + 24h)`, so every row whose time falls
//! inside its file's own local day is findable with at least 10h to spare.
//!
//! [`crate::writer::TsWriter::append`] rejects rows that break the contract
//! ([`crate::error::TstoreError::PtimeOutsideFileDate`]). The collector
//! (`banto-collect`) takes `ptime` from the same clock that dates the file,
//! so it only ever hits this when the clock jumps by more than
//! `24h - |o|` between reading `ptime` and the `append` call (the sample is
//! dropped and recorded as an append failure - owner decision 2026-09-24).
//!
//! Also dependent on this contract: `banto-tstore`'s retention
//! ([`crate::files::prune_files`]/[`crate::files::plan_prune`]) decides by the
//! file name's date alone, so it is only as accurate as "a row's time is near
//! its file's date".

use crate::date::LocalDate;

/// Padding the read side applies to each end of a query range before
/// converting it to a file-name date (24h - safely wider than any real-world
/// UTC offset, which never exceeds +-14h). Part of the ptime/file-date
/// contract: changing it changes both what the writer accepts and what the
/// reader opens, together.
pub const MAX_OFFSET_PAD_MS: i64 = 24 * 3_600_000;

/// The inclusive range of file-name dates the read side opens for a query
/// over `[from_ms, to_ms]`. The single definition `banto-tsquery`'s
/// `candidate_files` filters by, and that [`ptime_is_findable_in`] is built
/// on.
///
/// Saturating arithmetic, not an error: the padding only ever widens the
/// pre-filter so files near a genuine boundary are not missed, and saturating
/// at `i64::MIN`/`i64::MAX` means "the range already covers everything
/// representable" (`from_ms`/`to_ms` can be arbitrary caller-supplied values -
/// #423).
pub fn candidate_date_range(from_ms: i64, to_ms: i64) -> (LocalDate, LocalDate) {
    (
        LocalDate::from_epoch_ms(from_ms.saturating_sub(MAX_OFFSET_PAD_MS), 0),
        LocalDate::from_epoch_ms(to_ms.saturating_add(MAX_OFFSET_PAD_MS), 0),
    )
}

/// `true` iff a row at `ptime_ms` stored in a file dated `date` is found by
/// the read side, i.e. the query `[ptime_ms, ptime_ms]` (and therefore every
/// query range containing `ptime_ms`) selects that file. See this module's
/// doc for the closed form.
pub fn ptime_is_findable_in(date: LocalDate, ptime_ms: i64) -> bool {
    let (from_date, to_date) = candidate_date_range(ptime_ms, ptime_ms);
    from_date <= date && date <= to_date
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR_MS: i64 = 3_600_000;
    const DAY_MS: i64 = 86_400_000;

    fn start_ms(date: LocalDate) -> i64 {
        date.to_days_since_epoch() * DAY_MS
    }

    /// The closed form in the module doc, computed independently of
    /// `candidate_date_range` (in `i128`, no saturation).
    fn closed_form(date: LocalDate, ptime_ms: i64) -> bool {
        let start = i128::from(start_ms(date));
        let p = i128::from(ptime_ms);
        start - i128::from(24 * HOUR_MS) <= p && p < start + i128::from(48 * HOUR_MS)
    }

    fn dates() -> Vec<LocalDate> {
        vec![
            LocalDate::new(2026, 7, 12),
            LocalDate::new(2026, 1, 1),
            LocalDate::new(2025, 12, 31),
            LocalDate::new(2024, 2, 29),
            LocalDate::new(1970, 1, 1),
            LocalDate::new(1969, 12, 31), // negative epoch ms
            LocalDate::new(1900, 3, 1),
            LocalDate::new(9999, 12, 31),
        ]
    }

    #[test]
    fn predicate_matches_the_closed_form_at_every_boundary() {
        for date in dates() {
            let start = start_ms(date);
            for base in [
                start - 24 * HOUR_MS,
                start,
                start + DAY_MS,
                start + 48 * HOUR_MS,
            ] {
                for delta in [-DAY_MS, -HOUR_MS, -1, 0, 1, HOUR_MS] {
                    let p = base + delta;
                    assert_eq!(
                        ptime_is_findable_in(date, p),
                        closed_form(date, p),
                        "date={date:?} ptime={p}"
                    );
                }
            }
        }
    }

    #[test]
    fn exact_edges_of_the_window() {
        let date = LocalDate::new(2026, 7, 12);
        let start = start_ms(date);
        assert!(!ptime_is_findable_in(date, start - 24 * HOUR_MS - 1));
        assert!(ptime_is_findable_in(date, start - 24 * HOUR_MS));
        assert!(ptime_is_findable_in(date, start + 48 * HOUR_MS - 1));
        assert!(!ptime_is_findable_in(date, start + 48 * HOUR_MS));

        // Negative epoch ms (1969-12-31 starts at -86_400_000).
        let neg = LocalDate::new(1969, 12, 31);
        let start = start_ms(neg);
        assert_eq!(start, -DAY_MS);
        assert!(!ptime_is_findable_in(neg, start - 24 * HOUR_MS - 1));
        assert!(ptime_is_findable_in(neg, start - 24 * HOUR_MS));
        assert!(ptime_is_findable_in(neg, start + 48 * HOUR_MS - 1));
        assert!(!ptime_is_findable_in(neg, start + 48 * HOUR_MS));
    }

    /// Every instant of a local day, under any real-world UTC offset
    /// (`-14h..=+14h`), is findable in that local day's file - including 1ms
    /// either side of local midnight (the previous/next local day's file is
    /// then also findable, which is fine: it is the file the writer would
    /// have used a moment earlier/later).
    #[test]
    fn every_instant_of_a_local_day_is_findable_under_any_real_offset() {
        for offset_h in -14..=14i64 {
            let offset = offset_h * HOUR_MS;
            for date in dates() {
                let local_midnight_utc = start_ms(date) - offset;
                for p in [
                    local_midnight_utc,
                    local_midnight_utc + 1,
                    local_midnight_utc + DAY_MS / 2,
                    local_midnight_utc + DAY_MS - 1,
                ] {
                    assert_eq!(LocalDate::from_epoch_ms(p, offset), date);
                    assert!(
                        ptime_is_findable_in(date, p),
                        "offset={offset_h}h date={date:?} ptime={p}"
                    );
                }
                // 1ms before / at the next local midnight: still findable in
                // this file (a row sampled just before midnight, appended
                // just after, lands in the next file - and vice versa).
                assert!(ptime_is_findable_in(date, local_midnight_utc - 1));
                assert!(ptime_is_findable_in(date, local_midnight_utc + DAY_MS));
            }
        }
    }

    #[test]
    fn extremes_do_not_panic() {
        let date = LocalDate::new(2026, 7, 12);
        for p in [i64::MIN, i64::MIN + 1, -1, 0, 1, i64::MAX - 1, i64::MAX] {
            let _ = ptime_is_findable_in(date, p);
            let _ = candidate_date_range(p, p);
        }
        assert!(!ptime_is_findable_in(date, i64::MIN));
        assert!(!ptime_is_findable_in(date, i64::MAX));
        let (lo, hi) = candidate_date_range(i64::MIN, i64::MAX);
        assert!(lo <= date && date <= hi);
    }
}
