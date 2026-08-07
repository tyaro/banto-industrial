//! `TsQuery::read_range` - single-file raw range fetch, boundary inclusion,
//! missing-tag/missing-group tolerance, and the row-count cap.

mod common;

use banto_tsquery::{TsQuery, TsQueryError};
use common::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn returns_requested_tags_in_the_requested_order() {
    let dir = TempDir::new("raw-order");
    let clock = clock_at(DAY1_START_MS);
    let config = store_config(vec![group(
        "g1",
        1_000,
        vec![tag("t1", Some("degC"), 1), tag("t2", None, 0)],
    )]);
    let writer = open_writer(dir.path(), config, clock).await;
    for i in 0..5i64 {
        writer
            .append(
                "g1",
                DAY1_START_MS + i * 1_000,
                &[Some(i as f64), Some((i * 10) as f64)],
            )
            .await
            .unwrap();
    }
    writer.close().await.unwrap();

    let query = TsQuery::new(dir.path());
    // Requested in reverse of the group's own column order - the result
    // must follow the caller's order, not the physical column order.
    let requested = tag_keys(&["t2", "t1"]);
    let result = query
        .read_range("g1", &requested, DAY1_START_MS, DAY1_START_MS + 4_000, None)
        .await
        .unwrap();

    assert_eq!(result.tag_keys, requested);
    assert_eq!(result.rows.len(), 5);
    assert_eq!(result.rows[0].values, vec![Some(0.0), Some(0.0)]);
    assert_eq!(result.rows[2].values, vec![Some(20.0), Some(2.0)]);
    assert_eq!(result.rows[4].values, vec![Some(40.0), Some(4.0)]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn both_range_bounds_are_inclusive() {
    let dir = TempDir::new("raw-bounds");
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
    let requested = tag_keys(&["t1"]);

    // Exactly the row at from_ms and the row at to_ms must both be included.
    let result = query
        .read_range(
            "g1",
            &requested,
            DAY1_START_MS + 1_000,
            DAY1_START_MS + 3_000,
            None,
        )
        .await
        .unwrap();
    assert_eq!(result.rows.len(), 3);
    assert_eq!(result.rows[0].values, vec![Some(1.0)]);
    assert_eq!(result.rows[2].values, vec![Some(3.0)]);

    // One ms outside either bound must exclude that row.
    let result = query
        .read_range(
            "g1",
            &requested,
            DAY1_START_MS + 1_001,
            DAY1_START_MS + 2_999,
            None,
        )
        .await
        .unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0].values, vec![Some(2.0)]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tag_key_never_present_in_the_group_is_none_without_error() {
    let dir = TempDir::new("raw-missing-tag");
    let clock = clock_at(DAY1_START_MS);
    let config = store_config(vec![group("g1", 1_000, vec![tag("t1", None, 0)])]);
    let writer = open_writer(dir.path(), config, clock).await;
    writer
        .append("g1", DAY1_START_MS, &[Some(1.0)])
        .await
        .unwrap();
    writer.close().await.unwrap();

    let query = TsQuery::new(dir.path());
    let requested = tag_keys(&["t1", "does-not-exist"]);
    let result = query
        .read_range("g1", &requested, DAY1_START_MS, DAY1_START_MS, None)
        .await
        .unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0].values, vec![Some(1.0), None]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_group_key_yields_an_empty_result_not_an_error() {
    let dir = TempDir::new("raw-unknown-group");
    let clock = clock_at(DAY1_START_MS);
    let config = store_config(vec![group("g1", 1_000, vec![tag("t1", None, 0)])]);
    let writer = open_writer(dir.path(), config, clock).await;
    writer
        .append("g1", DAY1_START_MS, &[Some(1.0)])
        .await
        .unwrap();
    writer.close().await.unwrap();

    let query = TsQuery::new(dir.path());
    let result = query
        .read_range(
            "no-such-group",
            &tag_keys(&["t1"]),
            DAY1_START_MS,
            DAY1_START_MS + 10_000,
            None,
        )
        .await
        .unwrap();
    assert!(result.rows.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exceeding_max_rows_is_an_error() {
    let dir = TempDir::new("raw-too-many-rows");
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
    let err = query
        .read_range(
            "g1",
            &tag_keys(&["t1"]),
            DAY1_START_MS,
            DAY1_START_MS + 4_000,
            Some(3),
        )
        .await
        .unwrap_err();
    match err {
        TsQueryError::TooManyRows { count, max } => {
            assert_eq!(count, 5);
            assert_eq!(max, 3);
        }
        other => panic!("expected TooManyRows, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn from_greater_than_to_is_invalid_input() {
    let dir = TempDir::new("raw-invalid-range");
    let query = TsQuery::new(dir.path());
    let err = query
        .read_range("g1", &tag_keys(&["t1"]), 100, 50, None)
        .await
        .unwrap_err();
    assert!(matches!(err, TsQueryError::InvalidInput(_)));
}
