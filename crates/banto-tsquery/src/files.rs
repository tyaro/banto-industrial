//! Candidate-file selection: which of a data directory's `YYYYMMDD-NNN`
//! files could possibly hold rows for a given `[from_ms, to_ms]` range,
//! *before* any file is opened.
//!
//! `banto_tstore::DataFileInfo::date` is a *local* calendar date, but by how
//! much (which UTC offset) is not itself recorded anywhere retrievable
//! without opening the file (`tstore_meta` has no `utc_offset_ms` key - see
//! `banto-tstore/src/clock.rs`'s doc on why the writer re-queries the OS
//! offset on every rotation check rather than persisting it). Rather than
//! guess or open every file to find out, this module pads the requested
//! range by [`MAX_OFFSET_PAD_MS`] (24h - safely wider than any real-world UTC
//! offset, which never exceeds +-14h) on each side before comparing against
//! `date`, so a file is never wrongly excluded regardless of what offset
//! produced its name. The cost of this conservative padding is at most one
//! extra file opened at each end of a range, which the SQL `WHERE ptime
//! BETWEEN ? AND ?` in every downstream query then trims to nothing anyway.
use std::path::Path;

use banto_tstore::{list_data_files, DataFileInfo, LocalDate};

use crate::error::TsQueryError;

const MAX_OFFSET_PAD_MS: i64 = 24 * 3_600_000;

/// Every recognized data file in `data_dir`, ascending by `(date, seq)`
/// (`list_data_files`'s own order), filtered to those whose local date could
/// possibly overlap `[from_ms, to_ms]` under any real-world UTC offset.
pub(crate) fn candidate_files(
    data_dir: &Path,
    from_ms: i64,
    to_ms: i64,
) -> Result<Vec<DataFileInfo>, TsQueryError> {
    let from_date = LocalDate::from_epoch_ms(from_ms - MAX_OFFSET_PAD_MS, 0);
    let to_date = LocalDate::from_epoch_ms(to_ms + MAX_OFFSET_PAD_MS, 0);
    Ok(list_data_files(data_dir)?
        .into_iter()
        .filter(|f| f.date >= from_date && f.date <= to_date)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn empty_dir_yields_no_candidates() {
        let dir = TempDir::new("empty");
        let files = candidate_files(dir.path(), 0, 1_000).unwrap();
        assert!(files.is_empty());
    }
}
