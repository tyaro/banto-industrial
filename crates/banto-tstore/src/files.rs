//! Data-directory-level operations that do not need an open connection to
//! any one file: enumerating what is on disk ([`list_data_files`]) and
//! deleting what has aged out ([`prune_files`]). Both are plain synchronous
//! `std::fs` - deliberately not `async`: they run once at startup / on a
//! daily timer in the consuming app (I3b), not on the hot append path, so
//! there is no reason to make them tokio-async and no `Clock` dependency
//! either (`today` is passed in directly - see `prune_files`'s doc comment).

use std::fs;
use std::path::{Path, PathBuf};

use crate::date::LocalDate;
use crate::error::TstoreError;
use crate::schema::parse_data_file_name;

/// One data file found in a data directory, already parsed. Files whose
/// *name* does not match this crate's `YYYYMMDD-NNN.sqlite3` pattern are
/// silently skipped by both functions in this module (not an error - a
/// stray unrelated file, e.g. a `.gitkeep` or a WAL/SHM sidecar file SQLite
/// itself creates alongside an open `.sqlite3`, must not abort a scan).
#[derive(Debug, Clone, PartialEq)]
pub struct DataFileInfo {
    pub path: PathBuf,
    pub date: LocalDate,
    pub seq: u32,
}

/// List every recognized data file in `data_dir`, sorted ascending by
/// `(date, seq)` - the order [`crate::reader::TsReader`] consumers walk when
/// resolving a date range to files ("日付範囲→該当ファイル解決の補助").
///
/// Returns an empty `Vec` (not an error) if `data_dir` does not exist yet -
/// "no data written yet" is a normal state, not a failure, mirroring
/// `prune_files`'s same tolerance.
pub fn list_data_files(data_dir: &Path) -> Result<Vec<DataFileInfo>, TstoreError> {
    if !data_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut files = Vec::new();
    for entry in fs::read_dir(data_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue; // non-UTF-8 name: cannot be one of ours, skip
        };
        if let Some((date, seq)) = parse_data_file_name(file_name) {
            files.push(DataFileInfo {
                path: entry.path(),
                date,
                seq,
            });
        }
    }

    files.sort_by_key(|f| (f.date, f.seq));
    Ok(files)
}

/// Highest-`seq` recognized file for exactly `date`, if any -
/// [`crate::writer::TsWriter`]'s file-resolution helper (used both at
/// `open()` and at day-rollover) so it and [`list_data_files`] never
/// disagree about what counts as a valid file name.
pub(crate) fn latest_file_for_date(
    data_dir: &Path,
    date: LocalDate,
) -> Result<Option<DataFileInfo>, TstoreError> {
    let mut candidates: Vec<DataFileInfo> = list_data_files(data_dir)?
        .into_iter()
        .filter(|f| f.date == date)
        .collect();
    candidates.sort_by_key(|f| f.seq);
    Ok(candidates.pop())
}

/// The outcome of one [`prune_files`] call.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PruneReport {
    pub deleted: Vec<PathBuf>,
    pub kept: Vec<PathBuf>,
}

/// Classify every recognized data file into "would be deleted" / "would be
/// kept" for `retention_days` relative to `today`, **without touching the
/// filesystem** (no `fs::remove_file` call) - the dry-run counterpart of
/// [`prune_files`], which shares this exact classification (see its doc
/// comment for the age rule) and only adds the actual deletion step on top.
/// Kept as a single function so the two never drift apart (banto-hub T19
/// S2-d, UX-39: the REST layer needs a "how many files would this delete"
/// preview before the destructive prune-now call).
pub fn plan_prune(
    data_dir: &Path,
    retention_days: u32,
    today: LocalDate,
) -> Result<PruneReport, TstoreError> {
    let mut report = PruneReport::default();
    let today_days = today.to_days_since_epoch();

    for file in list_data_files(data_dir)? {
        let age_days = today_days - file.date.to_days_since_epoch();
        if age_days > retention_days as i64 {
            report.deleted.push(file.path);
        } else {
            report.kept.push(file.path);
        }
    }

    Ok(report)
}

/// Delete every recognized data file older than `retention_days` relative to
/// `today`, keeping `today`'s own file(s) unconditionally regardless of
/// `retention_days` (design: "当日は対象外"). A file is deleted when
/// `today.to_days_since_epoch() - file.date.to_days_since_epoch() >
/// retention_days` - so `retention_days = 0` keeps only today, `= 1` keeps
/// today and yesterday, etc.
///
/// `today` is a plain [`LocalDate`], not a [`crate::clock::Clock`]: this
/// function has no rotation-timing concern of its own (unlike
/// [`crate::writer::TsWriter`]) - the caller (I3b, typically once at startup
/// and once on a daily timer) already has to compute "today" from a clock
/// for its own scheduling anyway, so threading a `Clock` trait object
/// through here would just be an indirection for a value the caller already
/// has in hand.
///
/// Never touches files that do not parse as `YYYYMMDD-NNN.sqlite3` (same
/// "skip, don't fail" tolerance as [`list_data_files`]) - this function
/// only ever deletes files it can positively identify as its own.
///
/// The classification itself (which files are "deleted" vs "kept") is
/// delegated to [`plan_prune`] so the dry-run preview and the real prune can
/// never disagree; this function's only addition on top of the plan is
/// actually removing the files the plan marked as `deleted`.
pub fn prune_files(
    data_dir: &Path,
    retention_days: u32,
    today: LocalDate,
) -> Result<PruneReport, TstoreError> {
    let plan = plan_prune(data_dir, retention_days, today)?;
    for path in &plan.deleted {
        fs::remove_file(path)?;
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Fresh, empty temp directory for one test - plain `std::fs` (no
    /// `tempfile` dependency, matching `banto-storage`'s own
    /// `connect_creates_a_file_that_does_not_exist_yet` test pattern).
    /// Best-effort cleanup on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let id = COUNTER.fetch_add(1, Ordering::SeqCst);
            let dir = std::env::temp_dir().join(format!(
                "banto-tstore-test-{}-{label}-{id}",
                std::process::id()
            ));
            fs::create_dir_all(&dir).expect("create temp dir");
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn touch(&self, name: &str) {
            fs::write(self.0.join(name), b"").expect("touch file");
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn list_data_files_returns_empty_for_missing_dir() {
        let dir = std::env::temp_dir().join("banto-tstore-test-does-not-exist-xyz");
        let files = list_data_files(&dir).expect("should not error");
        assert!(files.is_empty());
    }

    #[test]
    fn list_data_files_ignores_unrelated_files() {
        let dir = TempDir::new("ignore-unrelated");
        dir.touch("readme.txt");
        dir.touch("20260712-001.sqlite3-wal");
        dir.touch(".gitkeep");
        dir.touch("20260712-001.sqlite3");

        let files = list_data_files(dir.path()).expect("list should succeed");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].date, LocalDate::new(2026, 7, 12));
        assert_eq!(files[0].seq, 1);
    }

    #[test]
    fn list_data_files_is_sorted_by_date_then_seq() {
        let dir = TempDir::new("sorted");
        for name in [
            "20260713-001.sqlite3",
            "20260712-002.sqlite3",
            "20260712-001.sqlite3",
            "20260711-001.sqlite3",
        ] {
            dir.touch(name);
        }

        let files = list_data_files(dir.path()).expect("list should succeed");
        let ordered: Vec<(LocalDate, u32)> = files.iter().map(|f| (f.date, f.seq)).collect();
        assert_eq!(
            ordered,
            vec![
                (LocalDate::new(2026, 7, 11), 1),
                (LocalDate::new(2026, 7, 12), 1),
                (LocalDate::new(2026, 7, 12), 2),
                (LocalDate::new(2026, 7, 13), 1),
            ]
        );
    }

    #[test]
    fn latest_file_for_date_picks_highest_seq() {
        let dir = TempDir::new("latest-seq");
        for name in [
            "20260712-001.sqlite3",
            "20260712-003.sqlite3",
            "20260712-002.sqlite3",
        ] {
            dir.touch(name);
        }
        let latest = latest_file_for_date(dir.path(), LocalDate::new(2026, 7, 12))
            .expect("should succeed")
            .expect("should find a file");
        assert_eq!(latest.seq, 3);
    }

    #[test]
    fn latest_file_for_date_ignores_other_dates() {
        let dir = TempDir::new("latest-other-dates");
        dir.touch("20260711-005.sqlite3");
        let latest =
            latest_file_for_date(dir.path(), LocalDate::new(2026, 7, 12)).expect("should succeed");
        assert_eq!(latest, None);
    }

    #[test]
    fn prune_files_never_deletes_today_regardless_of_retention() {
        let dir = TempDir::new("prune-keep-today");
        let today = LocalDate::new(2026, 7, 12);
        dir.touch(&data_file_name_for_test(today, 1));

        let report = prune_files(dir.path(), 0, today).expect("prune should succeed");
        assert_eq!(report.deleted.len(), 0);
        assert_eq!(report.kept.len(), 1);
    }

    #[test]
    fn prune_files_deletes_files_older_than_retention() {
        let dir = TempDir::new("prune-old");
        let today = LocalDate::new(2026, 7, 12);
        let old_date = LocalDate::new(2026, 4, 1); // well over 90 days before today
        dir.touch(&data_file_name_for_test(old_date, 1));
        dir.touch(&data_file_name_for_test(today, 1));

        let report = prune_files(dir.path(), 90, today).expect("prune should succeed");
        assert_eq!(report.deleted.len(), 1);
        assert!(!report.deleted[0].exists());
        assert_eq!(report.kept.len(), 1);
    }

    #[test]
    fn prune_files_keeps_files_exactly_at_the_retention_boundary() {
        let dir = TempDir::new("prune-boundary");
        let today = LocalDate::new(2026, 7, 12);
        // days_from_civil(2026,7,12) - days_from_civil(2026,4,13) == 90.
        let boundary_date = LocalDate::new(2026, 4, 13);
        dir.touch(&data_file_name_for_test(boundary_date, 1));

        let report = prune_files(dir.path(), 90, today).expect("prune should succeed");
        assert_eq!(
            report.deleted.len(),
            0,
            "age == retention_days must survive"
        );
        assert_eq!(report.kept.len(), 1);
    }

    #[test]
    fn prune_files_deletes_one_day_past_the_retention_boundary() {
        let dir = TempDir::new("prune-past-boundary");
        let today = LocalDate::new(2026, 7, 12);
        let past_boundary = LocalDate::new(2026, 4, 12); // age 91 > retention 90
        dir.touch(&data_file_name_for_test(past_boundary, 1));

        let report = prune_files(dir.path(), 90, today).expect("prune should succeed");
        assert_eq!(report.deleted.len(), 1);
    }

    #[test]
    fn prune_files_ignores_unrelated_files() {
        let dir = TempDir::new("prune-ignore-unrelated");
        let today = LocalDate::new(2026, 7, 12);
        dir.touch("not-a-data-file.txt");

        let report = prune_files(dir.path(), 0, today).expect("prune should succeed");
        assert_eq!(report.deleted.len(), 0);
        assert_eq!(report.kept.len(), 0);
        assert!(dir.path().join("not-a-data-file.txt").exists());
    }

    #[test]
    fn prune_files_on_missing_dir_is_a_harmless_no_op() {
        let dir = std::env::temp_dir().join("banto-tstore-test-prune-missing-xyz");
        let report = prune_files(&dir, 90, LocalDate::new(2026, 7, 12)).expect("should not error");
        assert_eq!(report, PruneReport::default());
    }

    fn data_file_name_for_test(date: LocalDate, seq: u32) -> String {
        crate::schema::data_file_name(date, seq)
    }

    // --- plan_prune (dry-run) ---

    #[test]
    fn plan_prune_does_not_touch_the_filesystem() {
        let dir = TempDir::new("plan-prune-no-delete");
        let today = LocalDate::new(2026, 7, 12);
        let old_date = LocalDate::new(2026, 4, 1); // well over 90 days before today
        dir.touch(&data_file_name_for_test(old_date, 1));
        dir.touch(&data_file_name_for_test(today, 1));

        let plan = plan_prune(dir.path(), 90, today).expect("plan should succeed");
        assert_eq!(plan.deleted.len(), 1);
        assert_eq!(plan.kept.len(), 1);
        // Unlike prune_files, nothing was actually removed.
        assert!(plan.deleted[0].exists());
    }

    #[test]
    fn plan_prune_classification_matches_prune_files() {
        let plan_dir = TempDir::new("plan-prune-matches-a");
        let prune_dir = TempDir::new("plan-prune-matches-b");
        let today = LocalDate::new(2026, 7, 12);
        let dates = [
            LocalDate::new(2026, 7, 12), // today: always kept
            LocalDate::new(2026, 4, 13), // exactly at the 90-day boundary: kept
            LocalDate::new(2026, 4, 12), // one day past boundary: deleted
            LocalDate::new(2026, 1, 1),  // well past boundary: deleted
        ];
        for date in dates {
            plan_dir.touch(&data_file_name_for_test(date, 1));
            prune_dir.touch(&data_file_name_for_test(date, 1));
        }

        let plan = plan_prune(plan_dir.path(), 90, today).expect("plan should succeed");
        let pruned = prune_files(prune_dir.path(), 90, today).expect("prune should succeed");

        let plan_deleted: Vec<&str> = plan
            .deleted
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap())
            .collect();
        let pruned_deleted: Vec<&str> = pruned
            .deleted
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(plan_deleted, pruned_deleted);
        assert_eq!(plan.deleted.len(), 2);

        let plan_kept: Vec<&str> = plan
            .kept
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap())
            .collect();
        let pruned_kept: Vec<&str> = pruned
            .kept
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(plan_kept, pruned_kept);
        assert_eq!(plan.kept.len(), 2);
    }

    #[test]
    fn plan_prune_on_missing_dir_is_a_harmless_no_op() {
        let dir = std::env::temp_dir().join("banto-tstore-test-plan-prune-missing-xyz");
        let plan = plan_prune(&dir, 90, LocalDate::new(2026, 7, 12)).expect("should not error");
        assert_eq!(plan, PruneReport::default());
    }
}
