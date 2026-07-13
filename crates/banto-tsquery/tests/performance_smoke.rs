//! Performance smoke test (println only - not a CI failure condition, per
//! this crate's design instructions): one group, 8 tags, 86,400 rows (a full
//! day of 1s-period collection - recorder-requirements.md §3.1's v1 tag
//! count target scaled to one group), then `read_decimated` at
//! `target_bins=1200`.

mod common;

use std::time::Instant;

use banto_tsquery::TsQuery;
use common::*;

#[tokio::test]
async fn read_decimated_one_day_of_8_tags_at_1s_period() {
    let dir = TempDir::new("perf-smoke");
    let clock = clock_at(DAY1_START_MS);

    const TAG_COUNT: usize = 8;
    const TICKS: i64 = 86_400;

    let tags = (0..TAG_COUNT)
        .map(|t| tag(&format!("t{t}"), Some("unit"), 2))
        .collect();
    let config = store_config(vec![group("g1", 1_000, tags)]);
    let writer = open_writer_with_large_buffer(dir.path(), config, clock).await;

    let write_started = Instant::now();
    for tick in 0..TICKS {
        let values: Vec<Option<f64>> = (0..TAG_COUNT)
            .map(|t| Some((t as f64) + tick as f64))
            .collect();
        writer
            .append("g1", DAY1_START_MS + tick * 1_000, &values)
            .await
            .expect("append should succeed");
    }
    writer.flush().await.expect("flush should succeed");
    let write_elapsed = write_started.elapsed();
    writer.close().await.expect("close should succeed");

    let query = TsQuery::new(dir.path());
    let all_tags = (0..TAG_COUNT).map(|t| format!("t{t}")).collect::<Vec<_>>();

    let query_started = Instant::now();
    let result = query
        .read_decimated(
            "g1",
            &all_tags,
            DAY1_START_MS,
            DAY1_START_MS + (TICKS - 1) * 1_000,
            1_200,
        )
        .await
        .expect("read_decimated should succeed");
    let query_elapsed = query_started.elapsed();

    println!(
        "banto-tsquery perf smoke: wrote {TICKS} rows x {TAG_COUNT} tags in {write_elapsed:?}; \
         read_decimated(target_bins=1200) over the full day -> {} bins in {query_elapsed:?}",
        result.bins.len()
    );

    // Sanity, not a timing assertion: the query must actually have covered
    // the whole day and produced roughly the requested bin count.
    assert!(result.bins.len() <= 1_200);
    assert!(result.bins.len() > 1_000);
    assert!(result.bins.iter().all(|b| b.tags.len() == TAG_COUNT));
}
