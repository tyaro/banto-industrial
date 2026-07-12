//! Cross-file behavior: daily-rotation concatenation (both `read_range` and
//! `read_decimated`), config-change rotations leaving a `tag_key` absent
//! from one side turning into a per-file gap ("ファイル跨ぎは tag_key
//! マッチ"), and a bin that straddles a same-day rotation boundary merging
//! both files' contributions.

mod common;

use banto_tsquery::{BinValue, TsQuery};
use common::*;

#[tokio::test]
async fn read_range_concatenates_across_a_local_midnight_rotation() {
    let dir = TempDir::new("multi-midnight-raw");
    let clock = clock_at(DAY1_START_MS);
    let config = store_config(vec![group("g1", 1_000, vec![tag("t1", None, 1)])]);
    let writer = open_writer(dir.path(), config, clock.clone()).await;

    writer
        .append("g1", DAY1_START_MS, &[Some(1.0)])
        .await
        .unwrap();
    writer.flush().await.unwrap();

    clock.advance_ms(86_400_000);
    writer
        .append("g1", DAY1_START_MS + 86_400_000, &[Some(2.0)])
        .await
        .unwrap();
    writer.close().await.unwrap();

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
        .unwrap();

    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.rows[0].values, vec![Some(1.0)]);
    assert_eq!(result.rows[1].values, vec![Some(2.0)]);
}

#[tokio::test]
async fn read_decimated_concatenates_across_a_local_midnight_rotation() {
    let dir = TempDir::new("multi-midnight-dec");
    let clock = clock_at(DAY1_START_MS);
    let config = store_config(vec![group("g1", 1_000, vec![tag("t1", None, 1)])]);
    let writer = open_writer(dir.path(), config, clock.clone()).await;

    writer
        .append("g1", DAY1_START_MS, &[Some(1.0)])
        .await
        .unwrap();
    writer.flush().await.unwrap();

    clock.advance_ms(86_400_000);
    writer
        .append("g1", DAY1_START_MS + 86_400_000, &[Some(2.0)])
        .await
        .unwrap();
    writer.close().await.unwrap();

    let query = TsQuery::new(dir.path());
    let result = query
        .read_decimated(
            "g1",
            &tag_keys(&["t1"]),
            DAY1_START_MS,
            DAY1_START_MS + 86_400_000,
            2,
        )
        .await
        .unwrap();

    assert_eq!(result.bins.len(), 2);
    assert_eq!(
        result.bins[0].tags[0],
        BinValue::Range { min: 1.0, max: 1.0 },
        "day 1's file must land in the first bin"
    );
    assert_eq!(
        result.bins[1].tags[0],
        BinValue::Range { min: 2.0, max: 2.0 },
        "day 2's file must land in the second bin"
    );
}

#[tokio::test]
async fn a_tag_added_after_a_config_change_rotation_is_a_gap_in_the_older_files_window() {
    let dir = TempDir::new("multi-config-add-tag");
    let clock = clock_at(DAY1_START_MS);

    let writer1 = open_writer(
        dir.path(),
        store_config(vec![group(
            "g1",
            1_000,
            vec![tag("t1", None, 1), tag("t2", None, 1)],
        )]),
        clock.clone(),
    )
    .await;
    for (offset, v) in [(0i64, 0.0), (1_000, 1.0), (2_000, 2.0)] {
        writer1
            .append("g1", DAY1_START_MS + offset, &[Some(v), Some(v)])
            .await
            .unwrap();
    }
    writer1.close().await.unwrap();

    // Same day, different config (t3 added) - forces rotation to the next
    // `-NNN` sequence file rather than reusing seq 1.
    let writer2 = open_writer(
        dir.path(),
        store_config(vec![group(
            "g1",
            1_000,
            vec![tag("t1", None, 1), tag("t2", None, 1), tag("t3", None, 1)],
        )]),
        clock,
    )
    .await;
    for (offset, v) in [(3_000i64, 3.0), (4_000, 4.0), (5_000, 5.0)] {
        writer2
            .append(
                "g1",
                DAY1_START_MS + offset,
                &[Some(v), Some(v), Some(v * 10.0)],
            )
            .await
            .unwrap();
    }
    writer2.close().await.unwrap();

    let query = TsQuery::new(dir.path());
    let result = query
        .read_decimated(
            "g1",
            &tag_keys(&["t1", "t2", "t3"]),
            DAY1_START_MS,
            DAY1_START_MS + 5_000,
            5,
        )
        .await
        .unwrap();

    assert_eq!(result.bin_ms, 1_001);
    assert_eq!(result.bins.len(), 5);

    // Bins 0-1 (offsets 0..3000) come only from writer1's file, which never
    // had t3 - t3 must be a gap there, not a missing/omitted bin.
    assert_eq!(result.bins[0].tags[2], BinValue::Gap);
    assert_eq!(result.bins[1].tags[2], BinValue::Gap);
    // t1/t2 existed in both files and must have data throughout.
    assert!(matches!(result.bins[0].tags[0], BinValue::Range { .. }));
    assert!(matches!(result.bins[1].tags[0], BinValue::Range { .. }));

    // Bins 2-4 (offsets 3000..5000) come only from writer2's file, where t3
    // exists.
    for i in 2..5 {
        assert!(
            matches!(result.bins[i].tags[2], BinValue::Range { .. }),
            "bin {i} should have t3 data, got {:?}",
            result.bins[i].tags[2]
        );
    }
}

#[tokio::test]
async fn a_tag_removed_after_a_config_change_rotation_is_a_gap_in_the_newer_files_window() {
    let dir = TempDir::new("multi-config-remove-tag");
    let clock = clock_at(DAY1_START_MS);

    let writer1 = open_writer(
        dir.path(),
        store_config(vec![group(
            "g1",
            1_000,
            vec![tag("t1", None, 1), tag("t2", None, 1)],
        )]),
        clock.clone(),
    )
    .await;
    for (offset, v) in [(0i64, 0.0), (1_000, 1.0), (2_000, 2.0)] {
        writer1
            .append("g1", DAY1_START_MS + offset, &[Some(v), Some(v)])
            .await
            .unwrap();
    }
    writer1.close().await.unwrap();

    // t2 removed - same day, forces rotation.
    let writer2 = open_writer(
        dir.path(),
        store_config(vec![group("g1", 1_000, vec![tag("t1", None, 1)])]),
        clock,
    )
    .await;
    for (offset, v) in [(3_000i64, 3.0), (4_000, 4.0), (5_000, 5.0)] {
        writer2
            .append("g1", DAY1_START_MS + offset, &[Some(v)])
            .await
            .unwrap();
    }
    writer2.close().await.unwrap();

    let query = TsQuery::new(dir.path());
    let result = query
        .read_decimated(
            "g1",
            &tag_keys(&["t1", "t2"]),
            DAY1_START_MS,
            DAY1_START_MS + 5_000,
            5,
        )
        .await
        .unwrap();

    assert_eq!(result.bins.len(), 5);
    // writer1's window: t2 present.
    assert!(matches!(result.bins[0].tags[1], BinValue::Range { .. }));
    assert!(matches!(result.bins[1].tags[1], BinValue::Range { .. }));
    // writer2's window: t2 gone -> gap, not an omitted bin.
    for i in 2..5 {
        assert_eq!(
            result.bins[i].tags[1],
            BinValue::Gap,
            "bin {i} should be a gap for the removed tag"
        );
        assert!(matches!(result.bins[i].tags[0], BinValue::Range { .. }));
    }
}

#[tokio::test]
async fn one_bin_straddling_a_same_day_rotation_boundary_merges_both_files() {
    let dir = TempDir::new("multi-straddle");
    let clock = clock_at(DAY1_START_MS);

    let writer1 = open_writer(
        dir.path(),
        store_config(vec![group("g1", 100, vec![tag("t1", None, 1)])]),
        clock.clone(),
    )
    .await;
    for i in 0..10i64 {
        let offset = i * 100;
        writer1
            .append("g1", DAY1_START_MS + offset, &[Some(offset as f64 / 100.0)])
            .await
            .unwrap();
    }
    writer1.close().await.unwrap();

    // Different config (extra unused-here tag) forces a same-day rotation;
    // both files share period_ms=100 so the group's clamp floor is
    // unaffected.
    let writer2 = open_writer(
        dir.path(),
        store_config(vec![group(
            "g1",
            100,
            vec![tag("t1", None, 1), tag("t_extra", None, 1)],
        )]),
        clock,
    )
    .await;
    for i in 10..20i64 {
        let offset = i * 100;
        writer2
            .append(
                "g1",
                DAY1_START_MS + offset,
                &[Some(offset as f64 / 100.0), None],
            )
            .await
            .unwrap();
    }
    writer2.close().await.unwrap();

    let query = TsQuery::new(dir.path());
    // target_bins=1 over [0, 1900] -> a single bin wide enough to contain
    // every row from both files.
    let result = query
        .read_decimated(
            "g1",
            &tag_keys(&["t1"]),
            DAY1_START_MS,
            DAY1_START_MS + 1_900,
            1,
        )
        .await
        .unwrap();

    assert_eq!(result.bins.len(), 1);
    assert_eq!(
        result.bins[0].tags[0],
        BinValue::Range {
            min: 0.0,
            max: 19.0
        },
        "the envelope must merge contributions from both the pre- and \
         post-rotation files, not just the last one queried"
    );
}
