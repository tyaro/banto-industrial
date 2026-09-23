//! Regression tests for extreme `from_ms`/`to_ms`/`target_bins` inputs to
//! `TsQuery::read_range`/`TsQuery::read_decimated`/`TsQuery::aggregate`.
//! Found while writing #422 ("段階2: R2 のヒストリカル API が範囲を直接
//! 受け取る前に、極端な値で panic しないことを確かめる"): `i64::MIN`/
//! `i64::MAX` (and combinations that make `to_ms - from_ms` itself overflow
//! `i64`, such as `i64::MIN..i64::MAX`) made
//! `crates/banto-tsquery/src/files.rs::candidate_files` and
//! `crates/banto-tsquery/src/decimate.rs`'s bin-width/bin-count arithmetic
//! panic on unchecked `+`/`-` in a debug build (and silently wrap in
//! release), and made decimation's `(ptime - ?) / ?` SQL overflow inside
//! SQLite (a `REAL` result that fails to decode as `i64`). No screen or API
//! reaches these methods with such values today, but R2
//! (recorder-requirements.md §3.3) will take a range from the wire, so this
//! crate's public API must not trust its callers not to send one.
//!
//! The contract these tests pin (#423 review): the returned range is the
//! requested one, aligned bins start at `from_ms` and cover `[from_ms,
//! to_ms]`, and a stored row whose `ptime_ms` is in range is always
//! returned - no clamping of the range itself.
mod common;

use banto_tsquery::{BinValue, DecimatedRange, TsQuery, TsQueryError};
use common::*;

/// The first value past the `i64::MAX / 4` clamp the first version of #423
/// applied. Neither the file pre-filter's padding nor `to_ms - from_ms`
/// overflows here, so any change in results at this value is a regression
/// of the clamp itself.
const PAST_OLD_CLAMP_MS: i64 = i64::MAX / 4 + 1;

/// The aligned-grid contract from `types.rs`: bins start at `from_ms`, are
/// `bin_ms` apart, every bin starts inside the returned range, and the last
/// bin contains `to_ms`.
fn assert_bins_cover_range(decimated: &DecimatedRange) {
    let (from_ms, to_ms, bin_ms) = (decimated.from_ms, decimated.to_ms, decimated.bin_ms);
    assert!(bin_ms >= 1);
    assert!(!decimated.bins.is_empty());
    assert_eq!(decimated.bins[0].ptime_ms, from_ms);
    for pair in decimated.bins.windows(2) {
        assert_eq!(
            i128::from(pair[1].ptime_ms) - i128::from(pair[0].ptime_ms),
            i128::from(bin_ms)
        );
    }
    for bin in &decimated.bins {
        assert!(
            from_ms <= bin.ptime_ms && bin.ptime_ms <= to_ms,
            "bin {} outside [{from_ms}, {to_ms}]",
            bin.ptime_ms
        );
    }
    let last = decimated.bins.last().unwrap().ptime_ms;
    assert!(
        i128::from(last) + i128::from(bin_ms) > i128::from(to_ms),
        "last bin {last} + {bin_ms} does not reach {to_ms}"
    );
}

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

    let err = query
        .aggregate("g1", &tags, i64::MAX, i64::MIN)
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

/// `start == end` at each extreme must not panic and must report exactly
/// one bin, at that very point - not a special case, just the ordinary
/// single-point range at the edge of what `i64` can represent.
/// `PAST_OLD_CLAMP_MS` is the #423 review's case: the clamp put the bin at
/// `edge - 1`, outside the returned range.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_equals_end_at_each_extreme_does_not_panic() {
    let dir = TempDir::new("overflow-start-eq-end");
    std::fs::create_dir_all(dir.path()).unwrap();
    let query = TsQuery::new(dir.path());
    let tags = tag_keys(&["t1"]);

    for edge in [
        i64::MIN,
        i64::MIN + 1,
        -PAST_OLD_CLAMP_MS,
        -1,
        0,
        PAST_OLD_CLAMP_MS,
        i64::MAX - 1,
        i64::MAX,
    ] {
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
        assert_eq!(decimated.bin_ms, 1);
        assert_eq!(decimated.bins.len(), 1);
        assert_eq!(decimated.bins[0].ptime_ms, edge);
        assert_bins_cover_range(&decimated);
    }
}

/// The width-exceeds-`i64` case (`i64::MIN..i64::MAX`, whose span does not
/// fit an `i64`). The bins must cover the whole requested range, not a
/// clamped part of it. With `target_bins <= 2` a single bin cannot (its
/// width would not fit `bin_ms: i64`), so `bin_ms` is `i64::MAX` and there
/// are 3 bins - the documented exception to "at most `target_bins`".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn min_to_max_range_does_not_panic_and_covers_everything() {
    let dir = TempDir::new("overflow-min-max");
    std::fs::create_dir_all(dir.path()).unwrap();
    let query = TsQuery::new(dir.path());
    let tags = tag_keys(&["t1"]);

    let range = query
        .read_range("g1", &tags, i64::MIN, i64::MAX, None)
        .await
        .unwrap();
    assert!(range.rows.is_empty());

    for target_bins in [1usize, 2, 3, 1_000, banto_tsquery::MAX_TARGET_BINS] {
        let decimated = query
            .read_decimated("g1", &tags, i64::MIN, i64::MAX, target_bins)
            .await
            .unwrap();
        assert_eq!(decimated.from_ms, i64::MIN);
        assert_eq!(decimated.to_ms, i64::MAX);
        assert_bins_cover_range(&decimated);
        if target_bins <= 2 {
            assert_eq!(decimated.bin_ms, i64::MAX);
            assert_eq!(decimated.bins.len(), 3);
        } else {
            assert!(decimated.bins.len() <= target_bins);
        }
    }
}

/// Same as the empty-directory case above, but against a file that actually
/// has data - the fix must not just avoid panicking, it must not silently
/// lose the real samples either.
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

    for target_bins in [1usize, 2, 1_000] {
        let decimated = query
            .read_decimated("g1", &tags, i64::MIN, i64::MAX, target_bins)
            .await
            .unwrap();
        assert_bins_cover_range(&decimated);
        // Every sample is 2026 (positive) and `from_ms` is i64::MIN, so this
        // is the "from negative, ptime positive" case of the bin-index SQL.
        let expected_idx = ((i128::from(DAY1_START_MS) - i128::from(i64::MIN))
            / i128::from(decimated.bin_ms)) as usize;
        for (i, bin) in decimated.bins.iter().enumerate() {
            if i == expected_idx {
                assert_eq!(bin.tags[0], BinValue::Range { min: 0.0, max: 4.0 });
            } else {
                assert_eq!(bin.tags[0], BinValue::Gap, "bin {i}");
            }
        }
    }
}

/// #423 review, finding 2: `TsWriter::append` does not restrict `ptime_ms`,
/// and the file's name comes from the clock, not the sample - so an
/// ordinary 2026 file can hold a row at `PAST_OLD_CLAMP_MS`. Every query
/// whose range includes that time must return it (the clamp dropped it
/// without an error).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stored_row_past_the_old_clamp_is_still_found() {
    let dir = TempDir::new("overflow-row-past-clamp");
    let clock = clock_at(DAY1_START_MS);
    let config = store_config(vec![group("g1", 1_000, vec![tag("t1", None, 0)])]);
    let writer = open_writer(dir.path(), config, clock).await;
    let t = PAST_OLD_CLAMP_MS;
    writer.append("g1", t, &[Some(42.0)]).await.unwrap();
    writer.close().await.unwrap();

    let query = TsQuery::new(dir.path());
    let tags = tag_keys(&["t1"]);

    let range = query.read_range("g1", &tags, 0, t, None).await.unwrap();
    assert_eq!(range.rows.len(), 1);
    assert_eq!(range.rows[0].ptime_ms, t);
    assert_eq!(range.rows[0].values, vec![Some(42.0)]);

    let aggregates = query.aggregate("g1", &tags, 0, t).await.unwrap();
    assert_eq!(aggregates.len(), 1);
    assert_eq!(aggregates[0].count, 1);
    assert_eq!(aggregates[0].max, Some(42.0));

    // Every range below also covers the file's own (2026) date: file
    // selection goes by the date in the file name, so a range that covers
    // only `t` itself does not open this file at all - unchanged behavior,
    // not part of this fix (see the PR).
    for (from_ms, to_ms, target_bins) in [
        (0, t, 10usize),
        (i64::MIN, i64::MAX, 1),
        (i64::MIN, t, 1_000),
        (-t, t, 7),
    ] {
        let decimated = query
            .read_decimated("g1", &tags, from_ms, to_ms, target_bins)
            .await
            .unwrap();
        assert_eq!((decimated.from_ms, decimated.to_ms), (from_ms, to_ms));
        assert_bins_cover_range(&decimated);
        let expected_idx =
            ((i128::from(t) - i128::from(from_ms)) / i128::from(decimated.bin_ms)) as usize;
        let hits: Vec<usize> = decimated
            .bins
            .iter()
            .enumerate()
            .filter(|(_, bin)| bin.tags[0] != BinValue::Gap)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(hits, vec![expected_idx], "{from_ms}..{to_ms}");
        assert_eq!(
            decimated.bins[hits[0]].tags[0],
            BinValue::Range {
                min: 42.0,
                max: 42.0
            }
        );
    }
}

/// `target_bins == MAX_TARGET_BINS` combined with an extreme range must
/// resolve to a bounded bin count, not explode the allocation.
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
    assert_bins_cover_range(&decimated);
}
