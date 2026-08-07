//! [`TsWriter`]: the append/flush/rotate side of one data-file series.
//!
//! ## Buffering, without a background task
//!
//! `append`/`flush`/`close` are the only places buffered rows ever move -
//! there is no `tokio::spawn`ed timer flushing on a schedule. Instead,
//! [`WriterOptions::flush_interval_ms`]/`max_buffered_rows` are checked
//! inline at the end of every `append` call (`Inner::maybe_flush`): if
//! either threshold is crossed, that same `append` call performs the flush
//! before returning. This keeps the crate's dependency footprint down (no
//! `tokio` "rt"/"time" features - see `Cargo.toml`'s comment) and keeps
//! flush timing fully deterministic under an injected [`crate::clock::Clock`]
//! for tests; the trade-off, spelled out here rather than left implicit, is
//! that if a group's collection period is *slower* than
//! `flush_interval_ms`, the last buffered row(s) for that group sit
//! unflushed until the next `append` (any group) or an explicit `flush()`/
//! `close()` - acceptable because every real collection group in this
//! product polls continuously (recorder-requirements.md §3.1: 100ms-60s
//! periods, never "occasionally"), so there is always another `append`
//! along shortly, and `close()` always flushes on the way out regardless.
//!
//! ## Rotation
//!
//! Two distinct triggers, both funnelled through the same
//! [`resolve_file`] helper so "reuse if the on-disk config hash matches,
//! otherwise create the next `-NNN`" is one code path:
//!
//! - **Local-midnight rollover** (`Inner::rotate_if_needed`, checked at the
//!   top of every `append`): driven by [`crate::clock::Clock::now_ms`] - the
//!   *wall-clock* "now" at the moment `append` is called, not the sample's
//!   own `ptime_ms` argument. This is what makes rotation deterministically
//!   testable via [`crate::clock::ManualClock`] independent of whatever
//!   `ptime_ms` values a test happens to pass.
//! - **Config change** (`TsWriter::open`/`open_with_options` only): a
//!   *new* `TsWriter` instance for the same day with a different
//!   [`crate::config::StoreConfig`] gets the next `-NNN` for today rather
//!   than reusing/overwriting the previous one - mid-session config changes
//!   are not auto-detected by an already-open `TsWriter` (design: "構成変更
//!   時は再 open で連番ローテーション" - the caller is expected to open a
//!   fresh writer after a config change, not mutate one in place).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sqlx::{QueryBuilder, Sqlite, SqlitePool};
use tokio::sync::Mutex;

use crate::clock::Clock;
use crate::config::{compute_config_hash, StoreConfig};
use crate::date::LocalDate;
use crate::error::TstoreError;
use crate::files::latest_file_for_date;
use crate::schema::{self, column_name_for_index, table_name_for_index};

/// Buffering thresholds - "既定: 1秒 or 500行" (design). Not part of
/// [`TsWriter::open`]'s signature (kept matching the design's exact
/// skeleton) - use [`TsWriter::open_with_options`] to override.
#[derive(Debug, Clone, Copy)]
pub struct WriterOptions {
    pub max_buffered_rows: usize,
    pub flush_interval_ms: i64,
}

impl Default for WriterOptions {
    fn default() -> Self {
        Self {
            max_buffered_rows: 500,
            flush_interval_ms: 1_000,
        }
    }
}

/// Compact per-group info `Inner` needs on every `append`/`flush` - derived
/// once from `StoreConfig` at `open()` time (positionally, via
/// `table_name_for_index`/`column_name_for_index`), not re-read from the
/// database: a freshly-created file's schema and this are guaranteed to
/// agree because both come from the exact same naming functions over the
/// exact same `StoreConfig`, and a *reused* file only ever gets reused when
/// its `config_hash` already matched this `StoreConfig` (`resolve_file`).
struct GroupRuntime {
    table_name: String,
    column_count: usize,
}

struct BufferedRow {
    ptime_ms: i64,
    values: Vec<Option<f64>>,
}

struct Inner {
    data_dir: PathBuf,
    config: StoreConfig,
    config_hash: String,
    clock: Arc<dyn Clock>,
    options: WriterOptions,
    pool: SqlitePool,
    current_date: LocalDate,
    #[allow(dead_code)] // kept for diagnostics/future use; not read today
    current_seq: u32,
    groups: HashMap<String, GroupRuntime>,
    buffer: HashMap<String, Vec<BufferedRow>>,
    buffered_row_count: usize,
    last_flush_ms: i64,
}

/// One data-file series' writer. Cheap to hold across a long-running
/// process (I3b's collection engine keeps exactly one of these per
/// `StoreConfig` snapshot) - all mutable state lives behind one internal
/// `tokio::sync::Mutex`, so every public method takes `&self` and can be
/// called concurrently from multiple collection-group poll loops without
/// external synchronization; the mutex simply serializes them, which is
/// also what makes "batch everything pending into one transaction" work.
pub struct TsWriter {
    inner: Mutex<Inner>,
}

// Manual `Debug` (not `#[derive]`): `Inner` holds `Arc<dyn Clock>`, and
// `Clock` does not require `Debug` (it is a minimal, `dyn`-compatible
// trait - see `clock.rs`'s doc comment - adding a `Debug` supertrait bound
// there purely to satisfy a derive here would leak an unrelated requirement
// onto every future `Clock` implementor). Tests only need this for
// `Result::unwrap_err`'s `T: Debug` bound, not real inspection, so a
// constant placeholder is enough.
impl std::fmt::Debug for TsWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TsWriter").finish_non_exhaustive()
    }
}

impl TsWriter {
    /// Open (or create) today's data file for `config` under `data_dir`,
    /// with default [`WriterOptions`]. See [`Self::open_with_options`] for
    /// custom buffering thresholds.
    pub async fn open(
        data_dir: &Path,
        config: StoreConfig,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, TstoreError> {
        Self::open_with_options(data_dir, config, clock, WriterOptions::default()).await
    }

    pub async fn open_with_options(
        data_dir: &Path,
        config: StoreConfig,
        clock: Arc<dyn Clock>,
        options: WriterOptions,
    ) -> Result<Self, TstoreError> {
        config.validate()?;
        // Tiny, one-shot blocking call (directory creation, not the hot
        // append path) - not worth pulling in tokio's `fs` feature for
        // (same reasoning as `files.rs`'s module doc for why that module
        // stays fully synchronous).
        std::fs::create_dir_all(data_dir)?;

        let config_hash = compute_config_hash(&config);
        let now_ms = clock.now_ms();
        let today = LocalDate::from_epoch_ms(now_ms, clock.utc_offset_ms());
        let (pool, seq) = resolve_file(data_dir, &config, &config_hash, today, now_ms).await?;
        let groups = build_group_runtime(&config);

        Ok(Self {
            inner: Mutex::new(Inner {
                data_dir: data_dir.to_path_buf(),
                config,
                config_hash,
                clock,
                options,
                pool,
                current_date: today,
                current_seq: seq,
                groups,
                buffer: HashMap::new(),
                buffered_row_count: 0,
                last_flush_ms: now_ms,
            }),
        })
    }

    /// Buffer one group's one-period sample. `values` must be exactly
    /// `group_key`'s configured tag count, in `GroupConfig.tags` order;
    /// `None` = missing sample (NULL). May trigger an in-line rotation
    /// (local-midnight crossed) and/or flush (thresholds crossed) - see this
    /// module's doc comment.
    pub async fn append(
        &self,
        group_key: &str,
        ptime_ms: i64,
        values: &[Option<f64>],
    ) -> Result<(), TstoreError> {
        let mut inner = self.inner.lock().await;
        inner.rotate_if_needed().await?;

        let expected = inner
            .groups
            .get(group_key)
            .ok_or_else(|| TstoreError::UnknownGroup(group_key.to_string()))?
            .column_count;
        if values.len() != expected {
            return Err(TstoreError::ValueCountMismatch {
                group_key: group_key.to_string(),
                expected,
                actual: values.len(),
            });
        }

        inner
            .buffer
            .entry(group_key.to_string())
            .or_default()
            .push(BufferedRow {
                ptime_ms,
                values: values.to_vec(),
            });
        inner.buffered_row_count += 1;

        inner.maybe_flush().await
    }

    /// Force a flush of whatever is currently buffered, regardless of
    /// thresholds. A no-op (not an error) if nothing is buffered.
    pub async fn flush(&self) -> Result<(), TstoreError> {
        let mut inner = self.inner.lock().await;
        inner.flush_locked().await
    }

    /// Flush any remaining buffered rows and close the underlying
    /// connection pool. Consumes `self` - there is no further use for a
    /// `TsWriter` after `close()` (mirrors `sqlx::SqlitePool::close`'s
    /// "graceful shutdown" contract).
    pub async fn close(self) -> Result<(), TstoreError> {
        let mut inner = self.inner.into_inner();
        inner.flush_locked().await?;
        inner.pool.close().await;
        Ok(())
    }
}

impl Inner {
    async fn maybe_flush(&mut self) -> Result<(), TstoreError> {
        let now_ms = self.clock.now_ms();
        let should_flush = self.buffered_row_count >= self.options.max_buffered_rows
            || now_ms - self.last_flush_ms >= self.options.flush_interval_ms;
        if should_flush {
            self.flush_locked().await
        } else {
            Ok(())
        }
    }

    /// Check for a local-midnight crossing and, if one occurred since the
    /// currently-open file was resolved, flush the outgoing day's buffer
    /// into its (still-open) file, then resolve/open today's file.
    async fn rotate_if_needed(&mut self) -> Result<(), TstoreError> {
        let today = LocalDate::from_epoch_ms(self.clock.now_ms(), self.clock.utc_offset_ms());
        if today == self.current_date {
            return Ok(());
        }

        self.flush_locked().await?;
        let now_ms = self.clock.now_ms();
        let (pool, seq) = resolve_file(
            &self.data_dir,
            &self.config,
            &self.config_hash,
            today,
            now_ms,
        )
        .await?;
        let old_pool = std::mem::replace(&mut self.pool, pool);
        old_pool.close().await;
        self.current_date = today;
        self.current_seq = seq;
        Ok(())
    }

    async fn flush_locked(&mut self) -> Result<(), TstoreError> {
        if self.buffer.is_empty() {
            return Ok(());
        }

        let mut tx = self.pool.begin().await?;
        for (group_key, rows) in self.buffer.drain() {
            if rows.is_empty() {
                continue;
            }
            let table_name = &self
                .groups
                .get(&group_key)
                .expect("buffered rows only ever exist for a group validated at append() time")
                .table_name;

            let column_count = rows[0].values.len();
            let mut column_list = String::from("ptime");
            for i in 0..column_count {
                column_list.push_str(", ");
                column_list.push_str(&column_name_for_index(i));
            }

            let mut query_builder: QueryBuilder<Sqlite> =
                QueryBuilder::new(format!("INSERT INTO {table_name} ({column_list}) "));
            query_builder.push_values(rows.iter(), |mut binder, row| {
                binder.push_bind(row.ptime_ms);
                for value in &row.values {
                    binder.push_bind(*value);
                }
            });
            query_builder.build().execute(&mut *tx).await?;
        }
        tx.commit().await?;

        self.buffered_row_count = 0;
        self.last_flush_ms = self.clock.now_ms();
        Ok(())
    }
}

fn build_group_runtime(config: &StoreConfig) -> HashMap<String, GroupRuntime> {
    config
        .groups
        .iter()
        .enumerate()
        .map(|(index, group)| {
            (
                group.key.clone(),
                GroupRuntime {
                    table_name: table_name_for_index(index),
                    column_count: group.tags.len(),
                },
            )
        })
        .collect()
}

/// Resolve the writable file for `date`: reuse the highest-`-NNN` existing
/// file for `date` if its recorded `config_hash` matches `config_hash`,
/// otherwise create the next `-NNN` (or `-001` if none exists yet) with a
/// freshly-generated schema. Shared by [`TsWriter::open_with_options`] (initial
/// open) and [`Inner::rotate_if_needed`] (midnight rollover) - see this
/// module's doc comment on why both go through one function.
async fn resolve_file(
    data_dir: &Path,
    config: &StoreConfig,
    config_hash: &str,
    date: LocalDate,
    created_at_ms: i64,
) -> Result<(SqlitePool, u32), TstoreError> {
    if let Some(existing) = latest_file_for_date(data_dir, date)? {
        let pool = schema::connect_writable(&existing.path).await?;
        let file_meta = schema::read_file_meta(&pool).await?;
        if file_meta.config_hash == config_hash {
            return Ok((pool, existing.seq));
        }
        pool.close().await;

        let next_seq = existing.seq + 1;
        let path = data_dir.join(schema::data_file_name(date, next_seq));
        let pool = schema::connect_writable(&path).await?;
        schema::create_schema(&pool, config, config_hash, date, created_at_ms).await?;
        return Ok((pool, next_seq));
    }

    let path = data_dir.join(schema::data_file_name(date, 1));
    let pool = schema::connect_writable(&path).await?;
    schema::create_schema(&pool, config, config_hash, date, created_at_ms).await?;
    Ok((pool, 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ManualClock;
    use crate::config::{GroupConfig, TagColumn};
    use crate::files::list_data_files;
    use crate::reader::TsReader;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let id = COUNTER.fetch_add(1, Ordering::SeqCst);
            let dir = std::env::temp_dir().join(format!(
                "banto-tstore-test-writer-{}-{label}-{id}",
                std::process::id()
            ));
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        /// Retries on a short delay - see `crate::test_support`'s module doc
        /// for the full Windows WAL-close-timing rationale and why every
        /// test using `TempDir` must run on a multi-thread tokio runtime
        /// with >= 2 workers (hence every test below is annotated
        /// `#[tokio::test(flavor = "multi_thread", worker_threads = 2)]`
        /// rather than the bare `#[tokio::test]`).
        fn drop(&mut self) {
            crate::test_support::retry_remove(&self.0, |p| std::fs::remove_dir_all(p));
        }
    }

    fn tag(key: &str, unit: Option<&str>, decimals: u8) -> TagColumn {
        TagColumn {
            key: key.to_string(),
            name: format!("Tag {key}"),
            data_type: "f32".to_string(),
            unit: unit.map(str::to_string),
            decimals,
        }
    }

    fn two_group_config() -> StoreConfig {
        StoreConfig {
            groups: vec![
                GroupConfig {
                    key: "g1".to_string(),
                    name: "Group 1".to_string(),
                    period_ms: 1_000,
                    tags: vec![tag("t1", Some("degC"), 1), tag("t2", None, 0)],
                },
                GroupConfig {
                    key: "g2".to_string(),
                    name: "Group 2".to_string(),
                    period_ms: 100,
                    tags: vec![tag("t3", Some("kPa"), 2)],
                },
            ],
        }
    }

    // JST-like fixed offset used throughout - exercises offset handling
    // without depending on the host's actual local timezone.
    const OFFSET_MS: i64 = 9 * 3_600_000;

    // 2026-07-12T00:00:00Z == 2026-07-12T09:00 local (offset applied).
    const DAY1_START_MS: i64 = 20_646 * 86_400_000;

    fn clock_at(now_ms: i64) -> Arc<ManualClock> {
        Arc::new(ManualClock::new(now_ms, OFFSET_MS))
    }

    // --- round trip ------------------------------------------------------

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn append_flush_and_read_back_round_trips_multiple_groups_with_nulls() {
        let dir = TempDir::new("roundtrip");
        let clock = clock_at(DAY1_START_MS);
        let writer = TsWriter::open(dir.path(), two_group_config(), clock.clone())
            .await
            .expect("open should succeed");

        writer
            .append("g1", DAY1_START_MS, &[Some(21.5), None])
            .await
            .expect("append g1 row 1");
        writer
            .append("g1", DAY1_START_MS + 1_000, &[Some(22.0), Some(1.0)])
            .await
            .expect("append g1 row 2");
        writer
            .append("g2", DAY1_START_MS, &[Some(101.25)])
            .await
            .expect("append g2 row 1");

        writer.flush().await.expect("flush should succeed");

        let files = list_data_files(dir.path()).expect("list should succeed");
        assert_eq!(files.len(), 1);

        let reader = TsReader::open(&files[0].path).await.expect("reader open");
        assert_eq!(reader.groups().len(), 2);

        let g1_samples = reader
            .read_range("g1", DAY1_START_MS, DAY1_START_MS + 1_000)
            .await
            .expect("read_range g1");
        assert_eq!(g1_samples.len(), 2);
        assert_eq!(g1_samples[0].ptime_ms, DAY1_START_MS);
        assert_eq!(g1_samples[0].values, vec![Some(21.5), None]);
        assert_eq!(g1_samples[1].values, vec![Some(22.0), Some(1.0)]);

        let g2_samples = reader
            .read_range("g2", DAY1_START_MS, DAY1_START_MS)
            .await
            .expect("read_range g2");
        assert_eq!(g2_samples.len(), 1);
        assert_eq!(g2_samples[0].values, vec![Some(101.25)]);

        writer.close().await.expect("close should succeed");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reader_reports_column_metadata_matching_the_config() {
        let dir = TempDir::new("column-meta");
        let clock = clock_at(DAY1_START_MS);
        let writer = TsWriter::open(dir.path(), two_group_config(), clock)
            .await
            .unwrap();
        writer.close().await.unwrap();

        let files = list_data_files(dir.path()).unwrap();
        let reader = TsReader::open(&files[0].path).await.unwrap();
        let g1 = reader.group("g1").expect("g1 present");
        assert_eq!(g1.table_name, "samples_1");
        assert_eq!(g1.columns.len(), 2);
        assert_eq!(g1.columns[0].column_name, "c1");
        assert_eq!(g1.columns[0].tag_key, "t1");
        assert_eq!(g1.columns[0].unit.as_deref(), Some("degC"));
        assert_eq!(g1.columns[0].decimals, 1);
        assert_eq!(g1.columns[1].column_name, "c2");
        assert_eq!(g1.columns[1].unit, None);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn read_range_bounds_are_inclusive_and_exclude_outside_values() {
        let dir = TempDir::new("range-bounds");
        let clock = clock_at(DAY1_START_MS);
        let writer = TsWriter::open(dir.path(), two_group_config(), clock)
            .await
            .unwrap();
        for i in 0..5i64 {
            writer
                .append("g1", DAY1_START_MS + i * 1_000, &[Some(i as f64), None])
                .await
                .unwrap();
        }
        writer.close().await.unwrap();

        let files = list_data_files(dir.path()).unwrap();
        let reader = TsReader::open(&files[0].path).await.unwrap();
        let samples = reader
            .read_range("g1", DAY1_START_MS + 1_000, DAY1_START_MS + 3_000)
            .await
            .unwrap();
        assert_eq!(samples.len(), 3);
        assert_eq!(samples[0].values[0], Some(1.0));
        assert_eq!(samples[2].values[0], Some(3.0));
    }

    // --- open()/reopen rotation -------------------------------------------

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reopen_with_same_config_appends_to_the_same_file() {
        let dir = TempDir::new("reopen-same-config");
        let clock = clock_at(DAY1_START_MS);

        let writer1 = TsWriter::open(dir.path(), two_group_config(), clock.clone())
            .await
            .unwrap();
        writer1
            .append("g1", DAY1_START_MS, &[Some(1.0), None])
            .await
            .unwrap();
        writer1.close().await.unwrap();

        let writer2 = TsWriter::open(dir.path(), two_group_config(), clock)
            .await
            .unwrap();
        writer2
            .append("g1", DAY1_START_MS + 1_000, &[Some(2.0), None])
            .await
            .unwrap();
        writer2.close().await.unwrap();

        let files = list_data_files(dir.path()).unwrap();
        assert_eq!(
            files.len(),
            1,
            "same config on the same day must not rotate"
        );

        let reader = TsReader::open(&files[0].path).await.unwrap();
        let samples = reader
            .read_range("g1", DAY1_START_MS, DAY1_START_MS + 1_000)
            .await
            .unwrap();
        assert_eq!(samples.len(), 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reopen_with_a_different_config_rotates_to_the_next_sequence() {
        let dir = TempDir::new("reopen-diff-config");
        let clock = clock_at(DAY1_START_MS);

        let writer1 = TsWriter::open(dir.path(), two_group_config(), clock.clone())
            .await
            .unwrap();
        writer1.close().await.unwrap();

        let mut changed_config = two_group_config();
        changed_config.groups[0].tags.push(tag("t_new", None, 0));
        let writer2 = TsWriter::open(dir.path(), changed_config, clock)
            .await
            .unwrap();
        writer2.close().await.unwrap();

        let files = list_data_files(dir.path()).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].seq, 1);
        assert_eq!(files[1].seq, 2);
        assert_eq!(files[0].date, files[1].date);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn opening_a_third_time_with_the_original_config_rotates_again_rather_than_reusing_seq_1()
    {
        // The *latest* file for the day is what config-sameness is compared
        // against, not "any" same-day file - so returning to an earlier
        // config still moves forward rather than silently reusing -001
        // (which would risk two live schemas both claiming the same file).
        let dir = TempDir::new("reopen-thrice");
        let clock = clock_at(DAY1_START_MS);

        TsWriter::open(dir.path(), two_group_config(), clock.clone())
            .await
            .unwrap()
            .close()
            .await
            .unwrap();

        let mut changed = two_group_config();
        changed.groups[0].tags.push(tag("t_new", None, 0));
        TsWriter::open(dir.path(), changed, clock.clone())
            .await
            .unwrap()
            .close()
            .await
            .unwrap();

        TsWriter::open(dir.path(), two_group_config(), clock)
            .await
            .unwrap()
            .close()
            .await
            .unwrap();

        let files = list_data_files(dir.path()).unwrap();
        assert_eq!(files.len(), 3);
        assert_eq!(
            files.iter().map(|f| f.seq).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn open_rejects_an_invalid_config_without_touching_disk() {
        let dir = TempDir::new("open-invalid-config");
        let clock = clock_at(DAY1_START_MS);
        let empty_config = StoreConfig { groups: vec![] };
        let err = TsWriter::open(dir.path(), empty_config, clock)
            .await
            .unwrap_err();
        assert!(matches!(err, TstoreError::Config(_)));
    }

    // --- day-crossing rotation --------------------------------------------

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn append_across_local_midnight_rotates_to_a_new_dated_file() {
        let dir = TempDir::new("midnight-rotation");
        let clock = clock_at(DAY1_START_MS);
        let writer = TsWriter::open(dir.path(), two_group_config(), clock.clone())
            .await
            .unwrap();

        writer
            .append("g1", DAY1_START_MS, &[Some(1.0), None])
            .await
            .unwrap();
        writer.flush().await.unwrap();

        // Advance the clock's local date by one full day.
        clock.advance_ms(86_400_000);
        writer
            .append("g1", DAY1_START_MS + 86_400_000, &[Some(2.0), None])
            .await
            .unwrap();
        writer.close().await.unwrap();

        let files = list_data_files(dir.path()).unwrap();
        assert_eq!(files.len(), 2);
        assert_ne!(files[0].date, files[1].date);
        assert_eq!(files[0].seq, 1);
        assert_eq!(files[1].seq, 1, "a new day starts back at seq 1");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn midnight_rotation_flushes_the_outgoing_days_buffer_before_swapping_files() {
        let dir = TempDir::new("midnight-flush");
        let clock = clock_at(DAY1_START_MS);
        let writer = TsWriter::open(dir.path(), two_group_config(), clock.clone())
            .await
            .unwrap();

        // Buffered, NOT explicitly flushed - relies on rotation flushing it.
        writer
            .append("g1", DAY1_START_MS, &[Some(1.0), None])
            .await
            .unwrap();

        clock.advance_ms(86_400_000);
        writer
            .append("g1", DAY1_START_MS + 86_400_000, &[Some(2.0), None])
            .await
            .unwrap();
        writer.close().await.unwrap();

        let files = list_data_files(dir.path()).unwrap();
        assert_eq!(files.len(), 2);

        let day1_reader = TsReader::open(&files[0].path).await.unwrap();
        let day1_samples = day1_reader
            .read_range("g1", 0, DAY1_START_MS + 1_000)
            .await
            .unwrap();
        assert_eq!(
            day1_samples.len(),
            1,
            "day 1's row must have been flushed into day 1's file, not lost or misfiled"
        );
    }

    // --- buffering ----------------------------------------------------

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn row_count_threshold_triggers_an_automatic_flush() {
        let dir = TempDir::new("row-threshold");
        let clock = clock_at(DAY1_START_MS);
        let options = WriterOptions {
            max_buffered_rows: 3,
            flush_interval_ms: 3_600_000, // effectively disabled for this test
        };
        let writer = TsWriter::open_with_options(dir.path(), two_group_config(), clock, options)
            .await
            .unwrap();

        for i in 0..3i64 {
            writer
                .append("g1", DAY1_START_MS + i * 1_000, &[Some(i as f64), None])
                .await
                .unwrap();
        }

        // Read via a *second*, independent reader without calling flush()
        // explicitly - proves the 3rd append auto-flushed.
        let files = list_data_files(dir.path()).unwrap();
        let reader = TsReader::open(&files[0].path).await.unwrap();
        let samples = reader.read_range("g1", 0, i64::MAX).await.unwrap();
        assert_eq!(samples.len(), 3);

        writer.close().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn flush_interval_threshold_triggers_an_automatic_flush() {
        let dir = TempDir::new("interval-threshold");
        let clock = clock_at(DAY1_START_MS);
        let options = WriterOptions {
            max_buffered_rows: 1_000_000, // effectively disabled for this test
            flush_interval_ms: 500,
        };
        let writer =
            TsWriter::open_with_options(dir.path(), two_group_config(), clock.clone(), options)
                .await
                .unwrap();

        writer
            .append("g1", DAY1_START_MS, &[Some(1.0), None])
            .await
            .unwrap();

        // Still under the interval - nothing flushed yet.
        clock.advance_ms(100);
        writer
            .append("g1", DAY1_START_MS + 1, &[Some(2.0), None])
            .await
            .unwrap();
        let files = list_data_files(dir.path()).unwrap();
        let reader = TsReader::open(&files[0].path).await.unwrap();
        assert_eq!(reader.read_range("g1", 0, i64::MAX).await.unwrap().len(), 0);
        drop(reader);

        // Crosses the interval - this append should flush everything
        // buffered so far (both rows).
        clock.advance_ms(500);
        writer
            .append("g1", DAY1_START_MS + 2, &[Some(3.0), None])
            .await
            .unwrap();
        let reader = TsReader::open(&files[0].path).await.unwrap();
        assert_eq!(reader.read_range("g1", 0, i64::MAX).await.unwrap().len(), 3);

        writer.close().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn close_flushes_remaining_buffered_rows() {
        let dir = TempDir::new("close-flushes");
        let clock = clock_at(DAY1_START_MS);
        let options = WriterOptions {
            max_buffered_rows: 1_000_000,
            flush_interval_ms: 3_600_000,
        };
        let writer = TsWriter::open_with_options(dir.path(), two_group_config(), clock, options)
            .await
            .unwrap();
        writer
            .append("g1", DAY1_START_MS, &[Some(1.0), None])
            .await
            .unwrap();
        writer.close().await.unwrap();

        let files = list_data_files(dir.path()).unwrap();
        let reader = TsReader::open(&files[0].path).await.unwrap();
        assert_eq!(reader.read_range("g1", 0, i64::MAX).await.unwrap().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn flush_with_nothing_buffered_is_a_harmless_no_op() {
        let dir = TempDir::new("flush-empty");
        let clock = clock_at(DAY1_START_MS);
        let writer = TsWriter::open(dir.path(), two_group_config(), clock)
            .await
            .unwrap();
        writer
            .flush()
            .await
            .expect("flushing nothing should be fine");
        writer
            .flush()
            .await
            .expect("flushing nothing twice should be fine");
        writer.close().await.unwrap();
    }

    // --- error cases -------------------------------------------------

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn append_to_unknown_group_is_an_error() {
        let dir = TempDir::new("unknown-group");
        let clock = clock_at(DAY1_START_MS);
        let writer = TsWriter::open(dir.path(), two_group_config(), clock)
            .await
            .unwrap();
        let err = writer
            .append("no-such-group", DAY1_START_MS, &[])
            .await
            .unwrap_err();
        assert!(matches!(err, TstoreError::UnknownGroup(g) if g == "no-such-group"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn append_with_wrong_value_count_is_an_error() {
        let dir = TempDir::new("wrong-value-count");
        let clock = clock_at(DAY1_START_MS);
        let writer = TsWriter::open(dir.path(), two_group_config(), clock)
            .await
            .unwrap();
        let err = writer
            .append("g1", DAY1_START_MS, &[Some(1.0)]) // g1 expects 2 values
            .await
            .unwrap_err();
        match err {
            TstoreError::ValueCountMismatch {
                group_key,
                expected,
                actual,
            } => {
                assert_eq!(group_key, "g1");
                assert_eq!(expected, 2);
                assert_eq!(actual, 1);
            }
            other => panic!("expected ValueCountMismatch, got {other:?}"),
        }
    }

    // --- performance smoke (println only - not a CI failure condition) ---

    /// 32 groups x 8 tags, ~1 "second" of simulated 1s-period ticks per
    /// group batched together (32 x 8 = 256 tags total, matching
    /// recorder-requirements.md §3.1's v1 tag-count target) - prints elapsed
    /// wall time for the whole batch. Deliberately has no timing assertion:
    /// this is a smoke test for "does bulk buffered writing complete in a
    /// sane amount of time", not a CI performance gate (design instruction:
    /// "CI失敗条件にしない").
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn write_performance_smoke_32_groups_x_8_tags_x_1000_ticks() {
        let dir = TempDir::new("perf-smoke");
        let clock = clock_at(DAY1_START_MS);

        const GROUP_COUNT: usize = 32;
        const TAG_COUNT: usize = 8;
        const TICKS: i64 = 1_000;

        let groups = (0..GROUP_COUNT)
            .map(|g| GroupConfig {
                key: format!("g{g}"),
                name: format!("Group {g}"),
                period_ms: 1_000,
                tags: (0..TAG_COUNT)
                    .map(|t| tag(&format!("t{g}_{t}"), Some("unit"), 2))
                    .collect(),
            })
            .collect();
        let config = StoreConfig { groups };

        let writer = TsWriter::open(dir.path(), config, clock)
            .await
            .expect("open should succeed");

        let started = Instant::now();
        for tick in 0..TICKS {
            for g in 0..GROUP_COUNT {
                let values: Vec<Option<f64>> = (0..TAG_COUNT)
                    .map(|t| Some((g * TAG_COUNT + t) as f64 + tick as f64))
                    .collect();
                writer
                    .append(&format!("g{g}"), DAY1_START_MS + tick * 1_000, &values)
                    .await
                    .expect("append should succeed");
            }
        }
        writer.flush().await.expect("final flush should succeed");
        let elapsed = started.elapsed();

        let total_rows = GROUP_COUNT as i64 * TICKS;
        println!(
            "banto-tstore perf smoke: {GROUP_COUNT} groups x {TAG_COUNT} tags, {total_rows} rows \
             ({} appends) in {elapsed:?} ({:.1} appends/ms)",
            total_rows,
            total_rows as f64 / elapsed.as_millis().max(1) as f64
        );

        writer.close().await.expect("close should succeed");
    }
}
