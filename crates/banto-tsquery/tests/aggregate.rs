//! `TsQuery::aggregate` - per-tag min/max/avg/count with NULLs excluded,
//! cross-file merging, and input validation.

mod common;

use banto_tsquery::{TsQuery, TsQueryError};
use common::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn computes_min_max_avg_count_excluding_nulls() {
    let dir = TempDir::new("agg-basic");
    let clock = clock_at(DAY1_START_MS);
    let config = store_config(vec![group("g1", 1_000, vec![tag("t1", None, 1)])]);
    let writer = open_writer(dir.path(), config, clock).await;
    for (offset, value) in [
        (0i64, Some(10.0)),
        (1_000, None),
        (2_000, Some(20.0)),
        (3_000, None),
        (4_000, Some(30.0)),
    ] {
        writer
            .append("g1", DAY1_START_MS + offset, &[value])
            .await
            .unwrap();
    }
    writer.close().await.unwrap();

    let query = TsQuery::new(dir.path());
    let result = query
        .aggregate(
            "g1",
            &tag_keys(&["t1"]),
            DAY1_START_MS,
            DAY1_START_MS + 4_000,
        )
        .await
        .unwrap();

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].count, 3);
    assert_eq!(result[0].min, Some(10.0));
    assert_eq!(result[0].max, Some(30.0));
    assert_eq!(result[0].avg, Some(20.0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tag_with_zero_samples_reports_all_none() {
    let dir = TempDir::new("agg-zero-samples");
    let clock = clock_at(DAY1_START_MS);
    let config = store_config(vec![group("g1", 1_000, vec![tag("t1", None, 1)])]);
    let writer = open_writer(dir.path(), config, clock).await;
    writer
        .append("g1", DAY1_START_MS, &[Some(1.0)])
        .await
        .unwrap();
    writer.close().await.unwrap();

    let query = TsQuery::new(dir.path());
    // "does-not-exist" is never part of the group's schema at all.
    let result = query
        .aggregate(
            "g1",
            &tag_keys(&["t1", "does-not-exist"]),
            DAY1_START_MS,
            DAY1_START_MS,
        )
        .await
        .unwrap();

    assert_eq!(result[0].count, 1);
    assert_eq!(result[1].count, 0);
    assert_eq!(result[1].min, None);
    assert_eq!(result[1].max, None);
    assert_eq!(result[1].avg, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn merges_min_max_avg_across_multiple_files() {
    let dir = TempDir::new("agg-multi-file");
    let clock = clock_at(DAY1_START_MS);

    let writer1 = open_writer(
        dir.path(),
        store_config(vec![group("g1", 1_000, vec![tag("t1", None, 1)])]),
        clock.clone(),
    )
    .await;
    writer1
        .append("g1", DAY1_START_MS, &[Some(10.0)])
        .await
        .unwrap();
    writer1
        .append("g1", DAY1_START_MS + 1_000, &[Some(20.0)])
        .await
        .unwrap();
    writer1.close().await.unwrap();

    clock.advance_ms(86_400_000);
    let writer2 = open_writer(
        dir.path(),
        store_config(vec![group("g1", 1_000, vec![tag("t1", None, 1)])]),
        clock.clone(),
    )
    .await;
    writer2
        .append("g1", DAY1_START_MS + 86_400_000, &[Some(30.0)])
        .await
        .unwrap();
    writer2
        .append("g1", DAY1_START_MS + 86_400_000 + 1_000, &[Some(40.0)])
        .await
        .unwrap();
    writer2.close().await.unwrap();

    let query = TsQuery::new(dir.path());
    let result = query
        .aggregate(
            "g1",
            &tag_keys(&["t1"]),
            DAY1_START_MS,
            DAY1_START_MS + 86_400_000 + 1_000,
        )
        .await
        .unwrap();

    assert_eq!(result[0].count, 4);
    assert_eq!(result[0].min, Some(10.0));
    assert_eq!(result[0].max, Some(40.0));
    assert_eq!(result[0].avg, Some(25.0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn from_greater_than_to_is_invalid_input() {
    let dir = TempDir::new("agg-invalid-range");
    let query = TsQuery::new(dir.path());
    let err = query
        .aggregate("g1", &tag_keys(&["t1"]), 100, 50)
        .await
        .unwrap_err();
    assert!(matches!(err, TsQueryError::InvalidInput(_)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_tag_keys_returns_an_empty_vec() {
    let dir = TempDir::new("agg-empty-tags");
    let query = TsQuery::new(dir.path());
    let result = query.aggregate("g1", &[], 0, 1_000).await.unwrap();
    assert!(result.is_empty());
}
