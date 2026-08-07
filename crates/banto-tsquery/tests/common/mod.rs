//! Shared test fixtures for `banto-tsquery`'s integration tests. Data files
//! are always produced through `banto_tstore::TsWriter`/`ManualClock` (never
//! hand-written SQL), so fixtures stay faithful to what I3b's real collector
//! writes - matching the module doc's stated design intent.
//!
//! `#[allow(dead_code)]` throughout: each of the several `tests/*.rs` binaries
//! only uses a subset of these helpers, and each binary compiles this module
//! independently (the standard `tests/common/mod.rs` pattern), so an unused
//! helper in any *one* binary is expected, not a real dead-code smell.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use banto_tstore::{GroupConfig, ManualClock, StoreConfig, TagColumn, TsWriter, WriterOptions};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Fresh temp directory for one test, cleaned up on drop - mirrors
/// `banto-tstore`'s own test `TempDir` helper exactly (plain `std::fs`, no
/// `tempfile` dependency).
///
/// ## Cleanup: why `drop` retries
///
/// On Windows, closing a WAL-mode `SqlitePool` (every `TsWriter` opens one
/// per data file) does not synchronously release the underlying file
/// handles, so a `remove_dir_all` issued immediately after the writer is
/// closed/dropped can observe `ERROR_SHARING_VIOLATION` (measured directly
/// in `banto-hub-core`'s identically-shaped `TempEnv` - see
/// `apps/banto-hub/core/tests/common/mod.rs`'s module doc for the full
/// writeup and measurements, and `banto-tstore/src/writer.rs`'s own
/// `TempDir` for the same fix applied there). [`TempDir::drop`] retries on a
/// short delay to reliably close this window.
///
/// This requires every test using `TempDir` to run on a multi-thread tokio
/// runtime with >= 2 workers (`Drop::drop` is synchronous, so the retry can
/// only block via `std::thread::sleep` - on a single-threaded runtime that
/// would starve the only worker thread and prevent the background close
/// from ever being polled) - hence every test in this crate's `tests/*.rs`
/// is annotated `#[tokio::test(flavor = "multi_thread", worker_threads = 2)]`
/// rather than the bare `#[tokio::test]`.
pub struct TempDir(PathBuf);

/// Delay between `remove_dir_all` retries in [`TempDir::drop`].
const TEMP_DIR_RETRY_DELAY: Duration = Duration::from_millis(50);

/// Retry ceiling in [`TempDir::drop`] - `TEMP_DIR_RETRY_DELAY *
/// TEMP_DIR_MAX_ATTEMPTS` (~2s) is the worst-case teardown block.
const TEMP_DIR_MAX_ATTEMPTS: u32 = 40;

impl TempDir {
    pub fn new(label: &str) -> Self {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "banto-tsquery-test-{}-{label}-{id}",
            std::process::id()
        ));
        Self(dir)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // `NotFound` is not an error here: a test that never actually wrote
        // anything (fixture setup failed before touching disk) never
        // creates this directory in the first place.
        for attempt in 1..=TEMP_DIR_MAX_ATTEMPTS {
            match std::fs::remove_dir_all(&self.0) {
                Ok(()) => return,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
                Err(_) if attempt < TEMP_DIR_MAX_ATTEMPTS => {
                    std::thread::sleep(TEMP_DIR_RETRY_DELAY);
                }
                Err(err) => {
                    eprintln!(
                        "TempDir: giving up removing {:?} after {attempt} attempts: {err}",
                        self.0
                    );
                }
            }
        }
    }
}

/// JST-like fixed offset, matching `banto-tstore/src/writer.rs`'s own test
/// convention - exercises non-zero-offset handling without depending on the
/// host's actual local timezone.
pub const OFFSET_MS: i64 = 9 * 3_600_000;

/// 2026-07-12T00:00:00Z (== 2026-07-12T09:00 local under `OFFSET_MS`) - an
/// arbitrary but fixed anchor, matching `banto-tstore/src/writer.rs`'s own
/// tests' `DAY1_START_MS`.
pub const DAY1_START_MS: i64 = 20_646 * 86_400_000;

pub fn clock_at(now_ms: i64) -> Arc<ManualClock> {
    Arc::new(ManualClock::new(now_ms, OFFSET_MS))
}

pub fn tag(key: &str, unit: Option<&str>, decimals: u8) -> TagColumn {
    TagColumn {
        key: key.to_string(),
        name: format!("Tag {key}"),
        data_type: "f32".to_string(),
        unit: unit.map(str::to_string),
        decimals,
    }
}

pub fn tag_named(key: &str, name: &str, unit: Option<&str>, decimals: u8) -> TagColumn {
    TagColumn {
        key: key.to_string(),
        name: name.to_string(),
        data_type: "f32".to_string(),
        unit: unit.map(str::to_string),
        decimals,
    }
}

pub fn group(key: &str, period_ms: u32, tags: Vec<TagColumn>) -> GroupConfig {
    GroupConfig {
        key: key.to_string(),
        name: format!("Group {key}"),
        period_ms,
        tags,
    }
}

pub fn store_config(groups: Vec<GroupConfig>) -> StoreConfig {
    StoreConfig { groups }
}

pub async fn open_writer(dir: &Path, config: StoreConfig, clock: Arc<ManualClock>) -> TsWriter {
    TsWriter::open(dir, config, clock)
        .await
        .expect("writer open should succeed")
}

/// Same as [`open_writer`] but with a larger buffer so a long append loop
/// (e.g. the performance smoke test) does not pay for as many small flush
/// transactions. Capped well below SQLite's bound on bound parameters per
/// statement (`max_buffered_rows * (1 + column_count)` must stay under
/// SQLite's `SQLITE_MAX_VARIABLE_NUMBER`, ~32766 on the bundled build) - a
/// single group holds *all* buffered rows (banto-tstore's buffer is
/// per-group, not global), so this only stays safe for a handful of tags.
pub async fn open_writer_with_large_buffer(
    dir: &Path,
    config: StoreConfig,
    clock: Arc<ManualClock>,
) -> TsWriter {
    let options = WriterOptions {
        max_buffered_rows: 2_000,
        flush_interval_ms: 3_600_000,
    };
    TsWriter::open_with_options(dir, config, clock, options)
        .await
        .expect("writer open should succeed")
}

pub fn tag_keys(keys: &[&str]) -> Vec<String> {
    keys.iter().map(|k| k.to_string()).collect()
}
