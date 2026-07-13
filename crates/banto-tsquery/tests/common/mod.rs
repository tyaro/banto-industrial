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

use banto_tstore::{GroupConfig, ManualClock, StoreConfig, TagColumn, TsWriter, WriterOptions};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Fresh temp directory for one test, best-effort cleaned up on drop -
/// mirrors `banto-tstore`'s own test `TempDir` helper exactly (plain
/// `std::fs`, no `tempfile` dependency).
pub struct TempDir(PathBuf);

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
        let _ = std::fs::remove_dir_all(&self.0);
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
