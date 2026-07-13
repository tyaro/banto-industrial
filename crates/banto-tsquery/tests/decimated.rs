//! `TsQuery::read_decimated` - min/max envelope correctness (spike
//! preservation), gap semantics (zero-row bins, all-NULL-for-one-tag bins),
//! bin-width clamping to `period_ms`, the near-native-resolution passthrough
//! mode, and input validation.

mod common;

use banto_tsquery::{BinValue, TsQuery, TsQueryError, MAX_TARGET_BINS};
use common::*;

#[tokio::test]
async fn spike_is_preserved_by_the_min_max_envelope() {
    let dir = TempDir::new("dec-spike");
    let clock = clock_at(DAY1_START_MS);
    let config = store_config(vec![group("g1", 100, vec![tag("t1", None, 1)])]);
    let writer = open_writer(dir.path(), config, clock).await;
    for i in 0..30i64 {
        let offset = i * 100;
        let value = if i == 15 { 999.0 } else { 1.0 }; // one spike at offset 1500ms
        writer
            .append("g1", DAY1_START_MS + offset, &[Some(value)])
            .await
            .unwrap();
    }
    writer.close().await.unwrap();

    let query = TsQuery::new(dir.path());
    let result = query
        .read_decimated(
            "g1",
            &tag_keys(&["t1"]),
            DAY1_START_MS,
            DAY1_START_MS + 2_900,
            3,
        )
        .await
        .unwrap();

    assert_eq!(result.bin_ms, 967);
    assert_eq!(result.bins.len(), 3);
    // Bin 1 ([967, 1934)) contains offsets 1000..1900, including the spike
    // at 1500 - min/max must span the whole envelope, not average it away.
    assert_eq!(
        result.bins[1].tags[0],
        BinValue::Range {
            min: 1.0,
            max: 999.0
        }
    );
    // The spike must not leak into neighboring bins.
    assert_eq!(
        result.bins[0].tags[0],
        BinValue::Range { min: 1.0, max: 1.0 }
    );
    assert_eq!(
        result.bins[2].tags[0],
        BinValue::Range { min: 1.0, max: 1.0 }
    );
}

#[tokio::test]
async fn zero_row_bins_are_gap_and_do_not_break_neighboring_bins() {
    let dir = TempDir::new("dec-zero-row-gap");
    let clock = clock_at(DAY1_START_MS);
    // period_ms is declared metadata only (used for the bin_ms clamp) - it
    // does not have to match the actual write cadence, so 250ms here stays
    // safely below the 1000ms bin_ms this test computes, guaranteeing the
    // binned (not near-native-passthrough) path runs.
    let config = store_config(vec![group("g1", 250, vec![tag("t1", None, 1)])]);
    let writer = open_writer(dir.path(), config, clock).await;
    for offset in [0i64, 1_000, 2_000, 6_500] {
        writer
            .append(
                "g1",
                DAY1_START_MS + offset,
                &[Some(offset as f64 / 1_000.0)],
            )
            .await
            .unwrap();
    }
    writer.close().await.unwrap();

    let query = TsQuery::new(dir.path());
    let result = query
        .read_decimated(
            "g1",
            &tag_keys(&["t1"]),
            DAY1_START_MS,
            DAY1_START_MS + 6_999,
            7,
        )
        .await
        .unwrap();

    assert_eq!(result.bin_ms, 1_000);
    assert_eq!(result.bins.len(), 7);
    assert_eq!(
        result.bins[0].tags[0],
        BinValue::Range { min: 0.0, max: 0.0 }
    );
    assert_eq!(
        result.bins[1].tags[0],
        BinValue::Range { min: 1.0, max: 1.0 }
    );
    assert_eq!(
        result.bins[2].tags[0],
        BinValue::Range { min: 2.0, max: 2.0 }
    );
    // Bins 3, 4, 5 (offsets [3000,6000)) have zero rows at all - a real
    // collection-stop scenario - and must be reported as gaps, not silently
    // dropped from `bins` or interpolated from neighbors.
    assert_eq!(result.bins[3].tags[0], BinValue::Gap);
    assert_eq!(result.bins[4].tags[0], BinValue::Gap);
    assert_eq!(result.bins[5].tags[0], BinValue::Gap);
    assert_eq!(
        result.bins[6].tags[0],
        BinValue::Range { min: 6.5, max: 6.5 }
    );
}

#[tokio::test]
async fn a_null_mixed_with_valid_samples_in_one_bin_still_reports_a_range() {
    let dir = TempDir::new("dec-null-mixed");
    let clock = clock_at(DAY1_START_MS);
    let config = store_config(vec![group(
        "g1",
        1_000,
        vec![tag("t1", None, 1), tag("t2", None, 1)],
    )]);
    let writer = open_writer(dir.path(), config, clock).await;
    let rows: [(i64, f64, Option<f64>); 5] = [
        (0, 0.0, None),
        (1_000, 1.0, Some(50.0)),
        (2_000, 2.0, None),
        (3_000, 3.0, Some(30.0)),
        (4_000, 4.0, Some(40.0)),
    ];
    for (offset, t1, t2) in rows {
        writer
            .append("g1", DAY1_START_MS + offset, &[Some(t1), t2])
            .await
            .unwrap();
    }
    writer.close().await.unwrap();

    let query = TsQuery::new(dir.path());
    let result = query
        .read_decimated(
            "g1",
            &tag_keys(&["t1", "t2"]),
            DAY1_START_MS,
            DAY1_START_MS + 4_000,
            3,
        )
        .await
        .unwrap();

    assert_eq!(result.bin_ms, 1_334);
    assert_eq!(result.bins.len(), 3);

    // Bin 0 ([0, 1334)): offsets 0 (t2=NULL) and 1000 (t2=Some(50)) - the
    // NULL must not suppress the valid sample; only NULL/NULL would.
    assert_eq!(
        result.bins[0].tags[0],
        BinValue::Range { min: 0.0, max: 1.0 }
    );
    assert_eq!(
        result.bins[0].tags[1],
        BinValue::Range {
            min: 50.0,
            max: 50.0
        }
    );

    // Bin 1 ([1334, 2668)): only offset 2000, t2 is NULL there and nowhere
    // else in the bin - this is the pure "every row NULL" gap case.
    assert_eq!(
        result.bins[1].tags[0],
        BinValue::Range { min: 2.0, max: 2.0 }
    );
    assert_eq!(result.bins[1].tags[1], BinValue::Gap);

    // Bin 2: offsets 3000 and 4000, both tags present throughout.
    assert_eq!(
        result.bins[2].tags[0],
        BinValue::Range { min: 3.0, max: 4.0 }
    );
    assert_eq!(
        result.bins[2].tags[1],
        BinValue::Range {
            min: 30.0,
            max: 40.0
        }
    );
}

#[tokio::test]
async fn bin_ms_is_clamped_up_to_the_group_period_ms() {
    let dir = TempDir::new("dec-clamp");
    let clock = clock_at(DAY1_START_MS);
    let config = store_config(vec![group("g1", 5_000, vec![tag("t1", None, 0)])]);
    let writer = open_writer(dir.path(), config, clock).await;
    writer
        .append("g1", DAY1_START_MS, &[Some(1.0)])
        .await
        .unwrap();
    writer
        .append("g1", DAY1_START_MS + 9_000, &[Some(2.0)])
        .await
        .unwrap();
    writer.close().await.unwrap();

    let query = TsQuery::new(dir.path());
    // target_bins=1000 over a 10s span would naively want ~10ms bins, far
    // finer than the group's declared 5000ms collection period.
    let result = query
        .read_decimated(
            "g1",
            &tag_keys(&["t1"]),
            DAY1_START_MS,
            DAY1_START_MS + 10_000,
            1_000,
        )
        .await
        .unwrap();
    assert_eq!(result.bin_ms, 5_000);
}

#[tokio::test]
async fn near_native_resolution_zoom_reports_exact_sample_time_not_bin_aligned() {
    let dir = TempDir::new("dec-near-native");
    let clock = clock_at(DAY1_START_MS);
    let config = store_config(vec![group("g1", 1_000, vec![tag("t1", None, 1)])]);
    let writer = open_writer(dir.path(), config, clock).await;
    for offset in [0i64, 1_000, 2_000, 3_000] {
        writer
            .append(
                "g1",
                DAY1_START_MS + offset,
                &[Some(offset as f64 / 1_000.0)],
            )
            .await
            .unwrap();
    }
    writer.close().await.unwrap();

    let query = TsQuery::new(dir.path());
    // from_ms is deliberately NOT aligned to the 1000ms period grid - a
    // bin-aligned result would report ptime_ms = from_ms + i*bin_ms
    // (…+500, …+1500), which does not match any real sample time.
    let result = query
        .read_decimated(
            "g1",
            &tag_keys(&["t1"]),
            DAY1_START_MS + 500,
            DAY1_START_MS + 2_500,
            5,
        )
        .await
        .unwrap();

    assert_eq!(result.bin_ms, 1_000); // clamped to period_ms as usual.
    assert_eq!(result.bins.len(), 2);
    assert_eq!(result.bins[0].ptime_ms, DAY1_START_MS + 1_000);
    assert_eq!(
        result.bins[0].tags[0],
        BinValue::Range { min: 1.0, max: 1.0 }
    );
    assert_eq!(result.bins[1].ptime_ms, DAY1_START_MS + 2_000);
    assert_eq!(
        result.bins[1].tags[0],
        BinValue::Range { min: 2.0, max: 2.0 }
    );
}

#[tokio::test]
async fn a_group_with_no_data_at_all_returns_gap_bins_covering_the_range() {
    let dir = TempDir::new("dec-no-data");
    let query = TsQuery::new(dir.path());
    let result = query
        .read_decimated("g1", &tag_keys(&["t1"]), 0, 999, 1)
        .await
        .unwrap();
    assert_eq!(result.bins.len(), 1);
    assert_eq!(result.bins[0].tags[0], BinValue::Gap);
}

#[tokio::test]
async fn target_bins_zero_is_invalid_input() {
    let dir = TempDir::new("dec-zero-bins");
    let query = TsQuery::new(dir.path());
    let err = query
        .read_decimated("g1", &tag_keys(&["t1"]), 0, 1_000, 0)
        .await
        .unwrap_err();
    assert!(matches!(err, TsQueryError::InvalidInput(_)));
}

#[tokio::test]
async fn target_bins_over_the_limit_is_invalid_input() {
    let dir = TempDir::new("dec-over-limit");
    let query = TsQuery::new(dir.path());
    let err = query
        .read_decimated("g1", &tag_keys(&["t1"]), 0, 1_000, MAX_TARGET_BINS + 1)
        .await
        .unwrap_err();
    assert!(matches!(err, TsQueryError::InvalidInput(_)));
}

#[tokio::test]
async fn binned_path_covers_the_full_range_contiguously() {
    let dir = TempDir::new("dec-contiguous");
    let clock = clock_at(DAY1_START_MS);
    let config = store_config(vec![group("g1", 100, vec![tag("t1", None, 1)])]);
    let writer = open_writer(dir.path(), config, clock).await;
    for i in 0..100i64 {
        let offset = i * 100;
        writer
            .append("g1", DAY1_START_MS + offset, &[Some(offset as f64)])
            .await
            .unwrap();
    }
    writer.close().await.unwrap();

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
        .unwrap();

    assert_eq!(result.bin_ms, 1_000);
    assert_eq!(result.bins.len(), 10);
    for (i, bin) in result.bins.iter().enumerate() {
        assert_eq!(bin.ptime_ms, DAY1_START_MS + (i as i64) * 1_000);
        assert!(
            matches!(bin.tags[0], BinValue::Range { .. }),
            "bin {i} should have data, got {:?}",
            bin.tags[0]
        );
    }
}
