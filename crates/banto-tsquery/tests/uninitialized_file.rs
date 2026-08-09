//! H7フォローアップ（2026-08-09、TsQuery「未初期化ファイル」対応 -
//! docs/improvement-plan.md ④）: a data directory containing one
//! *uninitialized* file - a real, connectable, correctly-named
//! `YYYYMMDD-NNN.sqlite3` with **no** `banto-tstore` schema at all yet, the
//! same state a reader can observe if it races `TsWriter::open`'s
//! schema-creation transaction (see `banto_tstore::TstoreError::
//! Uninitialized`'s doc comment) - must be transparent to every one of
//! `TsQuery`'s four read methods: each returns an empty/absent `Ok` result,
//! never an error.
//!
//! `tests/concurrency.rs` covers the identical end state as a live race
//! against a real background writer; this file covers it deterministically
//! (construct the exact on-disk state directly, no race to win) and adds the
//! "one initialized file + one uninitialized file in the same directory"
//! case that a pure timing race cannot reliably exercise.

mod common;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use banto_tsquery::{BinValue, TsQuery};
use common::*;

/// Create a real, connectable, zero-table SQLite file at `dir/file_name`.
/// `list_data_files` (`banto-tstore/src/files.rs`) recognizes a file purely
/// by *name* (`YYYYMMDD-NNN.sqlite3`), so this is indistinguishable, from
/// every `TsQuery` read method's point of view, from a file `TsWriter::open`
/// has created but not yet run its schema-creation transaction against.
async fn touch_uninitialized_file(dir: &std::path::Path, file_name: &str) {
    // Unlike `TsWriter::open` (which calls `std::fs::create_dir_all`
    // itself), `TempDir::new` does not create the directory - every other
    // test in this crate only ever touches disk via `open_writer`, which
    // does that for them.
    std::fs::create_dir_all(dir).expect("create temp dir");
    let path = dir.join(file_name);
    let options = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .connect_with(options)
        .await
        .expect("creating an empty sqlite file should succeed");
    pool.close().await;
    assert!(path.is_file(), "the file should exist on disk");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_range_skips_an_uninitialized_file_and_returns_empty() {
    let dir = TempDir::new("uninit-raw-range");
    touch_uninitialized_file(dir.path(), "20260712-001.sqlite3").await;

    let query = TsQuery::new(dir.path());
    let result = query
        .read_range(
            "g1",
            &tag_keys(&["t1"]),
            DAY1_START_MS,
            DAY1_START_MS + 86_400_000,
            None,
        )
        .await
        .expect("an uninitialized file must not error read_range");
    assert!(result.rows.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_decimated_skips_an_uninitialized_file_and_returns_all_gap_bins() {
    let dir = TempDir::new("uninit-decimated");
    touch_uninitialized_file(dir.path(), "20260712-001.sqlite3").await;

    let query = TsQuery::new(dir.path());
    let result = query
        .read_decimated(
            "g1",
            &tag_keys(&["t1"]),
            DAY1_START_MS,
            DAY1_START_MS + 9_999,
            10,
        )
        .await
        .expect("an uninitialized file must not error read_decimated");
    assert_eq!(result.bins.len(), 10);
    assert!(
        result
            .bins
            .iter()
            .all(|bin| bin.tags.iter().all(|v| *v == BinValue::Gap)),
        "every bin must be a gap, not partially/wrongly populated: {:?}",
        result.bins
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aggregate_skips_an_uninitialized_file_and_returns_an_empty_summary() {
    let dir = TempDir::new("uninit-aggregate");
    touch_uninitialized_file(dir.path(), "20260712-001.sqlite3").await;

    let query = TsQuery::new(dir.path());
    let result = query
        .aggregate(
            "g1",
            &tag_keys(&["t1"]),
            DAY1_START_MS,
            DAY1_START_MS + 86_400_000,
        )
        .await
        .expect("an uninitialized file must not error aggregate");
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].count, 0);
    assert_eq!(result[0].min, None);
    assert_eq!(result[0].max, None);
    assert_eq!(result[0].avg, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn catalog_skips_an_uninitialized_file_and_returns_an_empty_catalog() {
    let dir = TempDir::new("uninit-catalog");
    touch_uninitialized_file(dir.path(), "20260712-001.sqlite3").await;

    let query = TsQuery::new(dir.path());
    let catalog = query
        .catalog()
        .await
        .expect("an uninitialized file must not error catalog");
    assert!(catalog.groups.is_empty());
}

/// A directory holding one genuinely-initialized file (with real data) and
/// one uninitialized file (e.g. a rotation the writer started but crashed
/// before committing its schema) - every method must see exactly the
/// initialized file's data and silently skip the other, within one call.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mix_of_initialized_and_uninitialized_files_is_transparent() {
    let dir = TempDir::new("uninit-mixed");
    let clock = clock_at(DAY1_START_MS);
    let config = store_config(vec![group("g1", 1_000, vec![tag("t1", None, 1)])]);
    let writer = open_writer(dir.path(), config, clock).await;
    writer
        .append("g1", DAY1_START_MS, &[Some(42.0)])
        .await
        .unwrap();
    writer.close().await.unwrap();

    // A later-dated file that only ever got as far as file-creation.
    touch_uninitialized_file(dir.path(), "20260713-001.sqlite3").await;

    let query = TsQuery::new(dir.path());
    let requested = tag_keys(&["t1"]);
    let range_to_ms = DAY1_START_MS + 2 * 86_400_000;

    let result = query
        .read_range("g1", &requested, DAY1_START_MS, range_to_ms, None)
        .await
        .expect("the uninitialized file must not error a range spanning both files");
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0].values, vec![Some(42.0)]);

    let aggregate = query
        .aggregate("g1", &requested, DAY1_START_MS, range_to_ms)
        .await
        .expect("aggregate must not error");
    assert_eq!(aggregate[0].count, 1);
    assert_eq!(aggregate[0].avg, Some(42.0));

    let catalog = query.catalog().await.expect("catalog must not error");
    assert_eq!(catalog.groups.len(), 1);
    assert_eq!(catalog.groups[0].earliest_ms, Some(DAY1_START_MS));
    assert_eq!(catalog.groups[0].latest_ms, Some(DAY1_START_MS));
}
