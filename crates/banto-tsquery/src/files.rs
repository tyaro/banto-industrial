//! Candidate-file selection: which of a data directory's `YYYYMMDD-NNN`
//! files could possibly hold rows for a given `[from_ms, to_ms]` range,
//! *before* any file is opened.
//!
//! `banto_tstore::DataFileInfo::date` is a *local* calendar date, but by how
//! much (which UTC offset) is not itself recorded anywhere retrievable
//! without opening the file (`tstore_meta` has no `utc_offset_ms` key - see
//! `banto-tstore/src/clock.rs`'s doc on why the writer re-queries the OS
//! offset on every rotation check rather than persisting it). Rather than
//! guess or open every file to find out, the requested range is padded by
//! [`banto_tstore::MAX_OFFSET_PAD_MS`] (24h - safely wider than any
//! real-world UTC offset, which never exceeds +-14h) on each side before
//! comparing against `date`, so a file is never wrongly excluded regardless
//! of what offset produced its name. The cost of this conservative padding is
//! at most one extra file opened at each end of a range, which the SQL
//! `WHERE ptime BETWEEN ? AND ?` in every downstream query then trims to
//! nothing anyway.
//!
//! ## Depends on the ptime/file-date contract (#424, owner decision 2026-09-24)
//!
//! Selecting by file-name date is only correct because every row's `ptime`
//! is near its file's date. That is a contract `banto-tstore` owns and
//! enforces on write: [`banto_tstore::TsWriter::append`] rejects a row for
//! which [`banto_tstore::ptime_is_findable_in`] is false. The date range
//! used here is [`banto_tstore::candidate_date_range`] - the *same*
//! definition that predicate is built on - so what the writer accepts and
//! what this module selects cannot drift apart (pinned by
//! `tests::candidate_files_agree_with_the_write_side_predicate`). Do not
//! compute the range here in any other way.
use std::path::Path;

use banto_tstore::{candidate_date_range, list_data_files, DataFileInfo};

use crate::error::TsQueryError;

/// Every recognized data file in `data_dir`, ascending by `(date, seq)`
/// (`list_data_files`'s own order), filtered to those whose local date could
/// possibly overlap `[from_ms, to_ms]` under any real-world UTC offset.
pub(crate) fn candidate_files(
    data_dir: &Path,
    from_ms: i64,
    to_ms: i64,
) -> Result<Vec<DataFileInfo>, TsQueryError> {
    // `from_ms`/`to_ms` can be arbitrary caller-supplied `i64` (R2 will take
    // these from an API, not just this crate's own tests) -
    // `candidate_date_range` saturates rather than overflowing at the
    // extremes (#423; see its doc).
    let (from_date, to_date) = candidate_date_range(from_ms, to_ms);
    Ok(list_data_files(data_dir)?
        .into_iter()
        .filter(|f| f.date >= from_date && f.date <= to_date)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use banto_tstore::LocalDate;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempDir(std::path::PathBuf);
    impl TempDir {
        fn new(label: &str) -> Self {
            let id = COUNTER.fetch_add(1, Ordering::SeqCst);
            let dir = std::env::temp_dir().join(format!(
                "banto-tsquery-test-files-{}-{label}-{id}",
                std::process::id()
            ));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
        fn touch(&self, name: &str) {
            fs::write(self.0.join(name), b"").unwrap();
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn excludes_files_well_outside_the_padded_range() {
        let dir = TempDir::new("outside");
        dir.touch("20260101-001.sqlite3");
        dir.touch("20260712-001.sqlite3");

        // 2026-07-12T00:00:00Z .. 2026-07-12T01:00:00Z
        let from_ms = 20_646 * 86_400_000;
        let to_ms = from_ms + 3_600_000;
        let files = candidate_files(dir.path(), from_ms, to_ms).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].date, LocalDate::new(2026, 7, 12));
    }

    #[test]
    fn includes_neighboring_day_files_within_the_offset_pad() {
        let dir = TempDir::new("neighbors");
        dir.touch("20260711-001.sqlite3");
        dir.touch("20260712-001.sqlite3");
        dir.touch("20260713-001.sqlite3");

        // A query near local midnight could, under some UTC offset, fall
        // into the neighboring calendar-day file - both neighbors must
        // survive the pre-filter so the SQL WHERE clause gets a chance to
        // decide precisely.
        let from_ms = 20_646 * 86_400_000; // 2026-07-12T00:00:00Z
        let to_ms = from_ms;
        let files = candidate_files(dir.path(), from_ms, to_ms).unwrap();
        assert_eq!(files.len(), 3);
    }

    /// #424: the write side's acceptance test
    /// (`banto_tstore::ptime_is_findable_in`) and this module's selection must
    /// be the same condition. For every file date and every `ptime` in the
    /// boundary table, "the point query `[ptime, ptime]` selects the file"
    /// must equal "the writer would accept that row into that file" - and
    /// both must match the closed form documented in `banto_tstore::findable`
    /// (computed here independently, in `i128`), so shifting the shared
    /// constant or the shared range is caught too.
    #[test]
    fn candidate_files_agree_with_the_write_side_predicate() {
        use banto_tstore::ptime_is_findable_in;

        const HOUR: i64 = 3_600_000;
        const DAY: i64 = 86_400_000;

        let dates = [
            LocalDate::new(1969, 12, 30),
            LocalDate::new(1969, 12, 31),
            LocalDate::new(1970, 1, 1),
            LocalDate::new(1970, 1, 2),
            LocalDate::new(2026, 7, 10),
            LocalDate::new(2026, 7, 11),
            LocalDate::new(2026, 7, 12),
            LocalDate::new(2026, 7, 13),
            LocalDate::new(2026, 7, 14),
        ];
        let dir = TempDir::new("agree");
        for date in dates {
            dir.touch(&format!("{}-001.sqlite3", date.to_yyyymmdd()));
        }
        let start = |d: LocalDate| d.to_days_since_epoch() * DAY;

        let mut ptimes = vec![i64::MIN, i64::MIN + 1, -1, 0, 1, i64::MAX - 1, i64::MAX];
        for date in dates {
            let s = start(date);
            // Edges of the documented window (s - 24h, s + 48h), the file's
            // own UTC day, and local midnights under UTC offsets of +-14h
            // (local midnight of `date` is `s - offset` in UTC) - each
            // +-1ms.
            for base in [
                s - 24 * HOUR,
                s - 14 * HOUR,
                s,
                s + 14 * HOUR,
                s + DAY,
                s + DAY + 14 * HOUR,
                s + 48 * HOUR,
            ] {
                ptimes.extend([base - 1, base, base + 1]);
            }
        }

        let mut accepted = 0;
        let mut rejected = 0;
        for p in ptimes {
            let selected: Vec<LocalDate> = candidate_files(dir.path(), p, p)
                .unwrap()
                .into_iter()
                .map(|f| f.date)
                .collect();
            for date in dates {
                let findable = ptime_is_findable_in(date, p);
                assert_eq!(
                    selected.contains(&date),
                    findable,
                    "date={date:?} ptime={p}: read-side selection and write-side predicate disagree"
                );
                let s = i128::from(start(date));
                let closed = s - i128::from(24 * HOUR) <= i128::from(p)
                    && i128::from(p) < s + i128::from(48 * HOUR);
                assert_eq!(findable, closed, "date={date:?} ptime={p}: closed form");
                if findable {
                    accepted += 1;
                } else {
                    rejected += 1;
                }
            }
        }
        // The table really exercises both sides of the boundary.
        assert!(accepted > 100 && rejected > 100, "{accepted}/{rejected}");
    }

    #[test]
    fn empty_dir_yields_no_candidates() {
        let dir = TempDir::new("empty");
        let files = candidate_files(dir.path(), 0, 1_000).unwrap();
        assert!(files.is_empty());
    }
}
