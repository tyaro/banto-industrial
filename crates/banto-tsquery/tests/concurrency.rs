//! Read/write concurrency (H7 ④, docs/improvement-plan.md): a background
//! writer appending+flushing rows while the main task repeatedly queries the
//! same data directory through every read method, including the very first
//! moments before any file exists yet.
//!
//! SQLite's WAL mode is exactly one-writer/many-readers: a reader never
//! blocks the writer and is never blocked by it, and always sees either the
//! pre-transaction or the post-commit state of the file, never a torn one
//! (`banto-tstore`'s `schema::connect_writable` sets `SqliteJournalMode::
//! Wal`; `banto_tstore::TsWriter` commits one transaction per `flush()`).
//! This test leans on exactly that guarantee: every read below must succeed
//! and be internally consistent throughout the race, and once the writer
//! finishes, a final read must see every row it wrote.
//!
//! `TsQuery`'s four read methods pre-list candidate files with
//! `banto_tstore::list_data_files` (tolerant of a not-yet-existing data
//! directory - returns `Ok(vec![])`) and then open each one read-only. The
//! very first file's lifecycle has a narrower window than "the directory
//! doesn't exist yet", though: `TsWriter::open` creates the `.sqlite3` file
//! (via `SqlitePoolOptions::connect_with`, which establishes a real
//! connection - and SQLite creates the file as soon as anything is written
//! through it) *before* its schema-creation transaction (`CREATE TABLE
//! tstore_meta`/`tstore_groups`/`tstore_columns` + the group's `samples_<n>`,
//! all in one transaction - `schema::create_schema`) commits. A reader that
//! lists that file during this window and opens it before the commit sees a
//! real, valid, *empty* (zero-table) SQLite database. Empirically (measured
//! while writing this test) this is not a rare corner case: with the reader
//! racing from the very first instant as it does below, it is hit on
//! effectively every run, multiple times.
//!
//! **FIXED（2026-08-09、H7フォローアップ, docs/improvement-plan.md ④）**:
//! this used to be a real, documented gap in `TsQuery`'s "absent/empty, not
//! an error" contract - a reader that raced this exact window got
//! `TsQueryError::IncompatibleFile` instead of an empty/absent result, and
//! this test used to tolerate that specifically for the pre-first-row case.
//! Root cause: `banto-tstore/src/schema.rs::read_file_meta` mapped "no
//! `tstore_meta` table at all" to the *same* error variant
//! (`TstoreError::IncompatibleFile`) used for genuinely wrong/corrupt files,
//! so `banto-tsquery` had no way to tell "raced the writer" apart from "real
//! format problem". `banto-tstore` now reports the former as its own
//! variant, `TstoreError::Uninitialized`, and every one of `TsQuery`'s four
//! read methods (`raw.rs`'s `TstoreError::Uninitialized` match arm;
//! `plan.rs::is_uninitialized`, shared by `decimate.rs`/`aggregate.rs`/
//! `catalog.rs`) now skips a file in that state - contributing zero
//! rows/gaps/absence, same as a file that simply is not there yet - instead
//! of erroring. This test therefore no longer tolerates `IncompatibleFile`
//! anywhere in the race, at any row count: every read below must succeed
//! (or, for `catalog`, at worst report the group as not-yet-present) from
//! the very first poll, and any error is now a genuine, unexpected failure.

mod common;

use std::time::{Duration, Instant};

use banto_tsquery::{RawRow, TsQuery};
use common::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_reads_during_writes_never_corrupt_or_error() {
    const ROW_COUNT: i64 = 50;
    const TAG_KEY: &str = "t1";
    // Comfortably beyond the last row's ptime (DAY1_START_MS + 49_000) -
    // avoids handing `read_range`/`read_decimated` an astronomically wide
    // `to_ms` (e.g. `i64::MAX`) for no reason.
    const RANGE_TO_MS: i64 = DAY1_START_MS + (ROW_COUNT + 5) * 1_000;

    let dir = TempDir::new("concurrent-read-write");
    let dir_path = dir.path().to_path_buf();
    let clock = clock_at(DAY1_START_MS);
    let config = store_config(vec![group("g1", 1_000, vec![tag(TAG_KEY, None, 1)])]);

    // Background task: writes ROW_COUNT rows, one flush (one committed
    // transaction) per row, advancing the writer's own ManualClock
    // deterministically between them (not sleeping - keeps the writer's
    // pacing itself deterministic; it is the *reader* loop below whose
    // interleaving against these writes is intentionally left to the OS
    // scheduler, bounded only by the reader loop's own wall-clock timeout).
    let writer_task = tokio::spawn({
        let dir_path = dir_path.clone();
        let config = config.clone();
        let clock = clock.clone();
        async move {
            let writer = open_writer(&dir_path, config, clock.clone()).await;
            for i in 0..ROW_COUNT {
                writer
                    .append("g1", DAY1_START_MS + i * 1_000, &[Some(i as f64)])
                    .await
                    .expect("append should succeed");
                writer.flush().await.expect("flush should succeed");
                clock.advance_ms(1_000);
            }
            writer.close().await.expect("close should succeed");
        }
    });

    // Foreground: races the writer from the very first instant (the data
    // directory may not even exist yet) through to completion, bounded by a
    // generous wall-clock deadline rather than a fixed sleep/iteration count.
    let query = TsQuery::new(&dir_path);
    let requested_tags = tag_keys(&[TAG_KEY]);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last_count = 0usize;
    let mut saw_all = false;

    while Instant::now() < deadline {
        match query
            .read_range("g1", &requested_tags, DAY1_START_MS, RANGE_TO_MS, None)
            .await
        {
            Ok(range) => {
                assert_rows_are_sorted_and_match_what_was_written(&range.rows);
                assert!(
                    range.rows.len() >= last_count,
                    "read_range's row count must never regress across successive reads \
                     (saw {}, previously saw {last_count})",
                    range.rows.len()
                );
                last_count = range.rows.len();
            }
            // FIXED (see this file's module doc): a racing reader now skips
            // an uninitialized file instead of erroring, so this must never
            // fail at any point in the race, not just past the first row.
            Err(err) => panic!("read_range must never error: {err}"),
        }

        // Same race, via the decimated + catalog paths - both must stay
        // error-free and internally consistent throughout.
        match query
            .read_decimated("g1", &requested_tags, DAY1_START_MS, RANGE_TO_MS, 10)
            .await
        {
            Ok(decimated) => assert!(
                decimated.bins.len() <= 10,
                "must never return more than the requested target_bins"
            ),
            Err(err) => panic!("read_decimated must never error: {err}"),
        }

        match query.catalog().await {
            Ok(catalog) => {
                if let Some(g1) = catalog.groups.iter().find(|g| g.group_key == "g1") {
                    if let (Some(earliest), Some(latest)) = (g1.earliest_ms, g1.latest_ms) {
                        assert!(
                            earliest <= latest,
                            "catalog's own earliest/latest must stay internally ordered"
                        );
                        assert!(earliest >= DAY1_START_MS);
                    }
                }
            }
            Err(err) => panic!("catalog must never error: {err}"),
        }

        if last_count == ROW_COUNT as usize {
            saw_all = true;
            break;
        }
    }

    assert!(
        saw_all,
        "reader never observed all {ROW_COUNT} rows within the timeout (last saw {last_count})"
    );

    writer_task.await.expect("writer task must not panic");

    // Final read, strictly after the writer has fully finished (including
    // its own trailing close()): must see every row, exactly once each,
    // holding exactly the value it was written with.
    let final_range = query
        .read_range("g1", &requested_tags, DAY1_START_MS, RANGE_TO_MS, None)
        .await
        .expect("final read_range must succeed");
    assert_eq!(final_range.rows.len(), ROW_COUNT as usize);
    for (i, row) in final_range.rows.iter().enumerate() {
        assert_eq!(row.ptime_ms, DAY1_START_MS + i as i64 * 1_000);
        assert_eq!(row.values, vec![Some(i as f64)]);
    }
}

/// Every row currently visible must be strictly time-sorted (matching
/// `TsQuery::read_range`'s documented ascending-`ptime_ms` order) and hold
/// exactly the value tick `i` was written with (`Some(i as f64)`) - a
/// torn/partial write would show up here as a missing, `None`, or
/// mismatched value.
fn assert_rows_are_sorted_and_match_what_was_written(rows: &[RawRow]) {
    for pair in rows.windows(2) {
        assert!(
            pair[0].ptime_ms < pair[1].ptime_ms,
            "rows must be strictly time-sorted: {} then {}",
            pair[0].ptime_ms,
            pair[1].ptime_ms
        );
    }
    for row in rows {
        let tick = (row.ptime_ms - DAY1_START_MS) / 1_000;
        assert_eq!(
            row.values,
            vec![Some(tick as f64)],
            "row at ptime {} must hold exactly what tick {tick} was written with",
            row.ptime_ms
        );
    }
}
