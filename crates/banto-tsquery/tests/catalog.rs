//! `TsQuery::catalog` - group/tag enumeration across multiple groups and
//! files, earliest/latest data range, latest-file-wins metadata, and the
//! empty-directory case.

mod common;

use banto_tsquery::TsQuery;
use common::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lists_every_group_and_tag_sorted_by_key() {
    let dir = TempDir::new("cat-groups-tags");
    let clock = clock_at(DAY1_START_MS);
    let config = store_config(vec![
        group("g1", 1_000, vec![tag("t2", None, 0), tag("t1", None, 0)]),
        group("g2", 500, vec![tag("t3", None, 0)]),
    ]);
    let writer = open_writer(dir.path(), config, clock).await;
    writer
        .append("g1", DAY1_START_MS, &[Some(1.0), Some(2.0)])
        .await
        .unwrap();
    writer
        .append("g2", DAY1_START_MS, &[Some(3.0)])
        .await
        .unwrap();
    writer.close().await.unwrap();

    let query = TsQuery::new(dir.path());
    let catalog = query.catalog().await.unwrap();

    assert_eq!(catalog.groups.len(), 2);
    assert_eq!(catalog.groups[0].group_key, "g1");
    assert_eq!(catalog.groups[1].group_key, "g2");

    // Tags come back sorted by tag_key regardless of physical column order.
    let g1_tag_keys: Vec<&str> = catalog.groups[0]
        .tags
        .iter()
        .map(|t| t.tag_key.as_str())
        .collect();
    assert_eq!(g1_tag_keys, vec!["t1", "t2"]);
    assert_eq!(catalog.groups[1].tags[0].tag_key, "t3");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_earliest_and_latest_ptime_per_group() {
    let dir = TempDir::new("cat-range");
    let clock = clock_at(DAY1_START_MS);
    let config = store_config(vec![group("g1", 1_000, vec![tag("t1", None, 0)])]);
    let writer = open_writer(dir.path(), config, clock).await;
    writer
        .append("g1", DAY1_START_MS + 500, &[Some(1.0)])
        .await
        .unwrap();
    writer
        .append("g1", DAY1_START_MS + 7_500, &[Some(2.0)])
        .await
        .unwrap();
    writer
        .append("g1", DAY1_START_MS + 3_000, &[Some(3.0)])
        .await
        .unwrap();
    writer.close().await.unwrap();

    let query = TsQuery::new(dir.path());
    let catalog = query.catalog().await.unwrap();

    assert_eq!(catalog.groups[0].earliest_ms, Some(DAY1_START_MS + 500));
    assert_eq!(catalog.groups[0].latest_ms, Some(DAY1_START_MS + 7_500));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_most_recently_rotated_files_metadata_wins() {
    let dir = TempDir::new("cat-latest-wins");
    let clock = clock_at(DAY1_START_MS);

    let writer1 = open_writer(
        dir.path(),
        store_config(vec![group(
            "g1",
            1_000,
            vec![tag_named("t1", "Old Name", Some("degC"), 1)],
        )]),
        clock.clone(),
    )
    .await;
    writer1
        .append("g1", DAY1_START_MS, &[Some(1.0)])
        .await
        .unwrap();
    writer1.close().await.unwrap();

    // Same day, renamed tag -> different config hash -> rotates to seq 2.
    let writer2 = open_writer(
        dir.path(),
        store_config(vec![group(
            "g1",
            1_000,
            vec![tag_named("t1", "New Name", Some("kPa"), 2)],
        )]),
        clock,
    )
    .await;
    writer2
        .append("g1", DAY1_START_MS + 1_000, &[Some(2.0)])
        .await
        .unwrap();
    writer2.close().await.unwrap();

    let query = TsQuery::new(dir.path());
    let catalog = query.catalog().await.unwrap();

    assert_eq!(catalog.groups.len(), 1);
    let t1 = &catalog.groups[0].tags[0];
    assert_eq!(t1.tag_name, "New Name");
    assert_eq!(t1.unit.as_deref(), Some("kPa"));
    assert_eq!(t1.decimals, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_empty_or_missing_data_directory_yields_an_empty_catalog() {
    let dir = TempDir::new("cat-empty");
    let query = TsQuery::new(dir.path());
    let catalog = query.catalog().await.unwrap();
    assert!(catalog.groups.is_empty());
}
