//! `TsQuery::read_binned_stats` - fixed-width min/max/avg/count bins, gaps,
//! inclusive range ends, cross-file merging and input validation.

mod common;

use banto_tsquery::{TsQuery, TsQueryError, MAX_STAT_BINS, MAX_STAT_CELLS};
use common::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn computes_per_bin_stats_with_gaps_and_inclusive_edges() {
    let dir = TempDir::new("stats-basic");
    let clock = clock_at(DAY1_START_MS);
    let config = store_config(vec![group("g1", 1_000, vec![tag("t1", None, 1)])]);
    let writer = open_writer(dir.path(), config, clock).await;
    // bin_ms 2000 over [0, 8000]: bins at 0, 2000, 4000, 6000, 8000.
    // bin0: 0,1000 ; bin1: 2000 + NULL@3000 ; bin2: empty ; bin3: 6000 ; bin4: 8000 (== to_ms)
    for (offset, value) in [
        (0i64, Some(10.0)),
        (1_000, Some(20.0)),
        (2_000, Some(5.0)),
        (3_000, None),
        (6_000, Some(-1.0)),
        (8_000, Some(7.0)),
    ] {
        writer
            .append("g1", DAY1_START_MS + offset, &[value])
            .await
            .unwrap();
    }
    writer.close().await.unwrap();

    let r = TsQuery::new(dir.path())
        .read_binned_stats(
            "g1",
            &tag_keys(&["t1"]),
            DAY1_START_MS,
            DAY1_START_MS + 8_000,
            2_000,
        )
        .await
        .unwrap();

    assert_eq!(r.bin_ms, 2_000);
    assert_eq!(r.tag_keys, tag_keys(&["t1"]));
    assert_eq!(r.bins.len(), 5);
    for (i, b) in r.bins.iter().enumerate() {
        assert_eq!(b.ptime_ms, DAY1_START_MS + i as i64 * 2_000);
    }
    let s0 = r.bins[0].tags[0].unwrap();
    assert_eq!((s0.min, s0.max, s0.avg, s0.count), (10.0, 20.0, 15.0, 2));
    let s1 = r.bins[1].tags[0].unwrap();
    assert_eq!((s1.min, s1.max, s1.avg, s1.count), (5.0, 5.0, 5.0, 1));
    assert!(r.bins[2].tags[0].is_none());
    let s3 = r.bins[3].tags[0].unwrap();
    assert_eq!((s3.min, s3.max, s3.avg, s3.count), (-1.0, -1.0, -1.0, 1));
    let s4 = r.bins[4].tags[0].unwrap();
    assert_eq!((s4.min, s4.max, s4.avg, s4.count), (7.0, 7.0, 7.0, 1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bin_straddling_two_files_uses_count_weighted_average() {
    let dir = TempDir::new("stats-straddle");
    let clock = clock_at(DAY1_START_MS);
    let w1 = open_writer(
        dir.path(),
        store_config(vec![group("g1", 100, vec![tag("t1", None, 1)])]),
        clock.clone(),
    )
    .await;
    // file 1: three samples of 0.0 ; file 2: one sample of 100.0
    for i in 0..3i64 {
        w1.append("g1", DAY1_START_MS + i * 100, &[Some(0.0)])
            .await
            .unwrap();
    }
    w1.close().await.unwrap();
    let w2 = open_writer(
        dir.path(),
        store_config(vec![group(
            "g1",
            100,
            vec![tag("t1", None, 1), tag("t_extra", None, 1)],
        )]),
        clock,
    )
    .await;
    w2.append("g1", DAY1_START_MS + 300, &[Some(100.0), None])
        .await
        .unwrap();
    w2.close().await.unwrap();

    let r = TsQuery::new(dir.path())
        .read_binned_stats(
            "g1",
            &tag_keys(&["t1"]),
            DAY1_START_MS,
            DAY1_START_MS + 399,
            400,
        )
        .await
        .unwrap();
    assert_eq!(r.bins.len(), 1);
    let s = r.bins[0].tags[0].unwrap();
    // weighted: 100 / 4 = 25 (average of averages would be 50)
    assert_eq!((s.min, s.max, s.avg, s.count), (0.0, 100.0, 25.0, 4));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tag_missing_from_one_file_is_none_there_and_stats_elsewhere() {
    let dir = TempDir::new("stats-missing-tag");
    let clock = clock_at(DAY1_START_MS);
    let w1 = open_writer(
        dir.path(),
        store_config(vec![group("g1", 1_000, vec![tag("t1", None, 1)])]),
        clock.clone(),
    )
    .await;
    for i in 0..2i64 {
        w1.append("g1", DAY1_START_MS + i * 1_000, &[Some(1.0)])
            .await
            .unwrap();
    }
    w1.close().await.unwrap();
    let w2 = open_writer(
        dir.path(),
        store_config(vec![group(
            "g1",
            1_000,
            vec![tag("t1", None, 1), tag("t2", None, 1)],
        )]),
        clock,
    )
    .await;
    for i in 2..4i64 {
        w2.append("g1", DAY1_START_MS + i * 1_000, &[Some(3.0), Some(9.0)])
            .await
            .unwrap();
    }
    w2.close().await.unwrap();

    let r = TsQuery::new(dir.path())
        .read_binned_stats(
            "g1",
            &tag_keys(&["t1", "t2"]),
            DAY1_START_MS,
            DAY1_START_MS + 3_999,
            2_000,
        )
        .await
        .unwrap();
    assert_eq!(r.bins.len(), 2);
    assert_eq!(r.bins[0].tags[0].unwrap().avg, 1.0);
    assert!(r.bins[0].tags[1].is_none());
    assert_eq!(r.bins[1].tags[0].unwrap().avg, 3.0);
    let t2 = r.bins[1].tags[1].unwrap();
    assert_eq!((t2.avg, t2.count), (9.0, 2));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_data_yields_all_none_bins() {
    let dir = TempDir::new("stats-nodata");
    let r = TsQuery::new(dir.path())
        .read_binned_stats("g1", &tag_keys(&["t1"]), 0, 2_999, 1_000)
        .await
        .unwrap();
    assert_eq!(r.bins.len(), 3);
    assert!(r.bins.iter().all(|b| b.tags == vec![None]));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_invalid_arguments() {
    let dir = TempDir::new("stats-validation");
    let q = TsQuery::new(dir.path());
    let keys = tag_keys(&["t1"]);
    for (from, to, bin) in [(0, 10, 0), (0, 10, -5), (10, 0, 1)] {
        let err = q
            .read_binned_stats("g1", &keys, from, to, bin)
            .await
            .unwrap_err();
        assert!(matches!(err, TsQueryError::InvalidInput(_)), "{err:?}");
    }
    // exactly MAX_STAT_BINS is accepted, one more is not.
    let max = MAX_STAT_BINS as i64;
    q.read_binned_stats("g1", &keys, 0, max - 1, 1)
        .await
        .unwrap();
    let err = q
        .read_binned_stats("g1", &keys, 0, max, 1)
        .await
        .unwrap_err();
    assert!(matches!(err, TsQueryError::InvalidInput(_)), "{err:?}");
    let err = q
        .read_binned_stats("g1", &keys, i64::MIN, i64::MAX, 1)
        .await
        .unwrap_err();
    assert!(matches!(err, TsQueryError::InvalidInput(_)), "{err:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_too_many_cells_even_without_history() {
    let dir = TempDir::new("stats-cells");
    let q = TsQuery::new(dir.path());
    let max = MAX_STAT_BINS as i64;
    let names: Vec<String> = (0..11).map(|i| format!("t{i}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let err = q
        .read_binned_stats("g1", &tag_keys(&refs), 0, max - 1, 1)
        .await
        .unwrap_err();
    assert!(matches!(err, TsQueryError::InvalidInput(_)), "{err:?}");
    // exactly MAX_STAT_CELLS (10 tags x 100,000 bins) is accepted.
    assert_eq!(MAX_STAT_CELLS, 10 * MAX_STAT_BINS);
    let r = q
        .read_binned_stats("g1", &tag_keys(&refs[..10]), 0, max - 1, 1)
        .await
        .unwrap();
    assert_eq!(r.bins.len(), MAX_STAT_BINS);
}
