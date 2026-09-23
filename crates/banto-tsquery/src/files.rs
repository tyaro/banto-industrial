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

/// Any real `ptime_ms` a genuine `banto-tstore` file could ever hold is
/// vastly inside this bound - a data collector's timestamps are always a
/// real calendar date, nowhere near the edges of what `i64` can represent.
/// [`clamp_query_bounds`] clamps caller-supplied `from_ms`/`to_ms` to it
/// before either reaches any query arithmetic, most importantly
/// `decimate.rs`'s `(ptime - ?) / ?` bin-index SQL expression: without this,
/// an extreme `from_ms` (e.g. `i64::MIN`) makes SQLite's own `ptime -
/// from_ms` overflow `i64` and silently fall back to a `REAL`
/// (floating-point) result, which then fails to decode as the `i64` this
/// crate expects (`tests/range_overflow.rs` reproduces this - not a Rust
/// panic, but a `TsQueryError::Storage` decode failure either way). Chosen
/// as `i64::MAX / 4` so two values independently clamped to
/// `[-SAFE_QUERY_BOUND_MS, SAFE_QUERY_BOUND_MS]` can never overflow `i64`
/// when subtracted (worst case magnitude `i64::MAX / 2`).
///
/// Clamping changes nothing for a genuine file: no real `ptime_ms` is ever
/// outside this range, so a request that already covers it (however extreme
/// its literal `from_ms`/`to_ms`) reads exactly the same data either way -
/// this is "round to the whole representable timeline", not an error, per
/// the same judgement [`candidate_files`]'s own padding already applies.
pub(crate) const SAFE_QUERY_BOUND_MS: i64 = i64::MAX / 4;

/// Clamp caller-supplied `from_ms`/`to_ms` to [`SAFE_QUERY_BOUND_MS`] -
/// applied identically by every query method that touches `from_ms`/`to_ms`
/// (`raw`/`decimate`/`aggregate`) so all of them agree on what "the whole
/// representable timeline" means, and none of them hands an unclamped
/// extreme value to SQL arithmetic or this module's own date-padding.
pub(crate) fn clamp_query_bounds(from_ms: i64, to_ms: i64) -> (i64, i64) {
    (
        from_ms.clamp(-SAFE_QUERY_BOUND_MS, SAFE_QUERY_BOUND_MS),
        to_ms.clamp(-SAFE_QUERY_BOUND_MS, SAFE_QUERY_BOUND_MS),
    )
}

/// Every recognized data file in `data_dir`, ascending by `(date, seq)`
/// (`list_data_files`'s own order), filtered to those whose local date could
/// possibly overlap `[from_ms, to_ms]` under any real-world UTC offset.
pub(crate) fn candidate_files(
    data_dir: &Path,
    from_ms: i64,
    to_ms: i64,
) -> Result<Vec<DataFileInfo>, TsQueryError> {
    // `from_ms`/`to_ms` can be arbitrary caller-supplied `i64` (R2 will take
    // these from an API, not just this crate's own tests) - an unchecked `-`/
    // `+` here panics on overflow at the extremes (`from_ms == i64::MIN`,
    // `to_ms == i64::MAX`). Saturating is the right choice, not an error: the
    // padding only ever widens the file-name pre-filter so real files near a
    // genuine boundary are not missed, and saturating at `i64::MIN`/`i64::MAX`
    // just means "the range already covers everything representable" - no
    // real data file's date will ever be outside that, so the result is
    // identical to what unpadded arithmetic would have found.
    let from_date = LocalDate::from_epoch_ms(from_ms.saturating_sub(MAX_OFFSET_PAD_MS), 0);
    let to_date = LocalDate::from_epoch_ms(to_ms.saturating_add(MAX_OFFSET_PAD_MS), 0);
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
