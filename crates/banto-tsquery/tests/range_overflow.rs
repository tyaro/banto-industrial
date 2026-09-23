//! Regression tests for extreme `from_ms`/`to_ms`/`target_bins` inputs to
//! `TsQuery::read_range`/`TsQuery::read_decimated`. Found while writing #422
//! ("段階2: R2 のヒストリカル API が範囲を直接受け取る前に、極端な値で
//! panic しないことを確かめる"): `i64::MIN`/`i64::MAX` (and combinations that
//! make `to_ms - from_ms` itself overflow `i64`, such as `i64::MIN..
//! i64::MAX`) made `crates/banto-tsquery/src/files.rs::candidate_files` and
//! `crates/banto-tsquery/src/decimate.rs`'s bin-width/bin-count arithmetic
//! panic on unchecked `+`/`-` in a debug build (and silently wrap in
//! release). No screen or API reaches these methods with such values today,
//! but R2 (recorder-requirements.md §3.3) will take a range from the wire,
//! so this crate's public API must not trust its callers not to send one.
//!
//! Every case below panicked before the fix (verified by hand: reverting
//! either the `files.rs` or `decimate.rs` `saturating_sub`/`saturating_add`
//! change and re-running this file reproduces the overflow panic - see the
//! PR description for exactly which revert broke which test).
mod common;

use banto_tsquery::{TsQuery, TsQueryError};
use common::*;

/// `start > end` stays an error, not a rounding target - a caller sending a
/// backwards range is a caller mistake, not "the whole timeline" (matches
/// the crate's pre-existing `from_ms > to_ms` check; this is a regression
/// guard, not new behavior).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_greater_than_end_is_still_invalid_input_at_the_extremes() {
    let dir = TempDir::new("overflow-start-gt-end");
    std::fs::create_dir_all(dir.path()).unwrap();
    let query = TsQuery::new(dir.path());
    let tags = tag_keys(&["t1"]);

    let err = query
        .read_range("g1", &tags, i64::MAX, i64::MIN, None)
        .await
        .unwrap_err();
    assert!(matches!(err, TsQueryError::InvalidInput(_)));

    let err = query
        .read_decimated("g1", &tags, i64::MAX, i64::MIN, 100)
        .await
        .unwrap_err();
    assert!(matches!(err, TsQueryError::InvalidInput(_)));
}

/// `target_bins == 0` is still rejected even when paired with an extreme
/// range - the two validations are independent and both must survive.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn target_bins_zero_is_still_invalid_input_at_the_extremes() {
    let dir = TempDir::new("overflow-bins-zero");
    std::fs::create_dir_all(dir.path()).unwrap();
    let query = TsQuery::new(dir.path());
    let tags = tag_keys(&["t1"]);

    let err = query
        .read_decimated("g1", &tags, i64::MIN, i64::MAX, 0)
        .await
        .unwrap_err();
    assert!(matches!(err, TsQueryError::InvalidInput(_)));
}

/// `start == end` at each extreme must not panic and must report a
/// zero-width, one-bin-wide result - not a special case, just the ordinary
/// single-point range at the edge of what `i64` can represent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_equals_end_at_each_extreme_does_not_panic() {
    let dir = TempDir::new("overflow-start-eq-end");
    std::fs::create_dir_all(dir.path()).unwrap();
    let query = TsQuery::new(dir.path());
    let tags = tag_keys(&["t1"]);

    for edge in [i64::MIN, i64::MAX, 0] {
        let range = query
            .read_range("g1", &tags, edge, edge, None)
            .await
            .unwrap();
        assert!(range.rows.is_empty()); // no data directory contents at all.

        let decimated = query
            .read_decimated("g1", &tags, edge, edge, 10)
            .await
            .unwrap();
        assert_eq!(decimated.from_ms, edge);
        assert_eq!(decimated.to_ms, edge);
        assert!(!decimated.bins.is_empty());
    }
}

/// The width-exceeds-`i64` case (`i64::MIN..i64::MAX`, whose mathematical
/// span does not fit in an `i64`) is the case this bug is really about:
/// before the fix, `files.rs::candidate_files`'s `from_ms - MAX_OFFSET_PAD_MS`
/// panicked on `i64::MIN` alone, and (once that is patched) `decimate.rs`'s
/// `to_ms - from_ms` panicked next. Treated as "the whole representable
/// timeline" (rounds, does not error) per the PR's per-site judgment table -
/// an empty data directory has nothing in this range either way, so the
/// result is the same as any other range with no matching files.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn min_to_max_range_does_not_panic_and_reads_as_everything() {
    let dir = TempDir::new("overflow-min-max");
    std::fs::create_dir_all(dir.path()).unwrap();
    let query = TsQuery::new(dir.path());
    let tags = tag_keys(&["t1"]);

    let range = query
        .read_range("g1", &tags, i64::MIN, i64::MAX, None)
        .await
        .unwrap();
    assert!(range.rows.is_empty());

    for target_bins in [1usize, 2, banto_tsquery::MAX_TARGET_BINS] {
        let decimated = query
            .read_decimated("g1", &tags, i64::MIN, i64::MAX, target_bins)
            .await
            .unwrap();
        // No file at all matches an empty directory, so every bin is a gap -
        // the key assertion is simply that this returned at all (did not
        // panic) and did not try to allocate anywhere near `i64::MAX` bins.
        assert!(!decimated.bins.is_empty());
        assert!(decimated.bins.len() <= target_bins.max(2));
    }
}

/// Same as the empty-directory case above, but against a file that actually
/// has data, at an extreme `target_bins` too - the fix must not just avoid
/// panicking, it must not silently lose the real sample either (the
/// "wraps to a wrong answer in release" failure mode the task description
/// warns about).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn min_to_max_range_still_finds_real_data() {
    let dir = TempDir::new("overflow-min-max-real-data");
    let clock = clock_at(DAY1_START_MS);
    let config = store_config(vec![group("g1", 1_000, vec![tag("t1", None, 0)])]);
    let writer = open_writer(dir.path(), config, clock).await;
    for i in 0..5i64 {
        writer
            .append("g1", DAY1_START_MS + i * 1_000, &[Some(i as f64)])
            .await
            .unwrap();
    }
    writer.close().await.unwrap();

    let query = TsQuery::new(dir.path());
    let tags = tag_keys(&["t1"]);

    let range = query
        .read_range("g1", &tags, i64::MIN, i64::MAX, None)
        .await
        .unwrap();
    assert_eq!(range.rows.len(), 5);
    assert_eq!(range.rows[0].values, vec![Some(0.0)]);
    assert_eq!(range.rows[4].values, vec![Some(4.0)]);

    // A single bin spanning i64::MIN..i64::MAX must still envelope every
    // real sample this file has (min 0.0, max 4.0) - not panic, and not
    // silently drop them.
    let decimated = query
        .read_decimated("g1", &tags, i64::MIN, i64::MAX, 1)
        .await
        .unwrap();
    let has_full_envelope = decimated.bins.iter().any(|bin| {
        matches!(
            bin.tags[0],
            banto_tsquery::BinValue::Range { min, max } if min == 0.0 && max == 4.0
        )
    });
    assert!(
        has_full_envelope,
        "expected some bin to cover the full [0.0, 4.0] envelope, got {:?}",
        decimated.bins
    );
}

/// `target_bins == MAX_TARGET_BINS` combined with an extreme range must
/// resolve to a bounded `bin_ms`/bin count, not silently truncate
/// `target_bins`'s effect the way `i64::MAX.checked_add(target_bins - 1)`
/// overflowing to `None` could (see `decimate.rs`'s `raw_bin_ms` -
/// `unwrap_or(i64::MAX)` already guarded the panic; this asserts the
/// resulting bin count is still sane, not just non-panicking).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn max_target_bins_with_extreme_range_does_not_explode_allocation() {
    let dir = TempDir::new("overflow-max-bins");
    std::fs::create_dir_all(dir.path()).unwrap();
    let query = TsQuery::new(dir.path());
    let tags = tag_keys(&["t1"]);

    let decimated = query
        .read_decimated(
            "g1",
            &tags,
            i64::MIN,
            i64::MAX,
            banto_tsquery::MAX_TARGET_BINS,
        )
        .await
        .unwrap();
    assert!(decimated.bins.len() <= banto_tsquery::MAX_TARGET_BINS);
}
