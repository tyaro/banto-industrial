//! Shared `#[cfg(test)]`-only helpers for this crate's unit test modules
//! (`backup::tests`/`rest::tests`).
//!
//! ## Why this exists (2026-08-08 audit finding)
//!
//! `backup::tests`/`rest::tests` used bare `tempfile::tempdir()` for every
//! fixture that needs a real on-disk SQLite file (`VACUUM INTO` - what
//! `BackupService::create` uses - silently writes nothing when its source
//! connection is `:memory:`, so these tests cannot use the usual
//! `crate::db::migrate_memory` shortcut). On Windows, closing a WAL-mode
//! `SqlitePool` does not synchronously release its file handles - even
//! immediately after `pool.close().await` returns, the OS can hold the file
//! "in use" for a short additional window - so `tempfile::TempDir`'s single,
//! non-retrying `remove_dir_all` (which also silently swallows its error)
//! left a directory behind on nearly every test. Measured: one
//! `cargo test -p chronogazer-core --lib` run left 16 directories behind -
//! matching this module's every fixture that ever created one (identical
//! finding to `relay-wright-core`, which this app's `backup.rs`/`rest.rs`
//! were copied from almost verbatim).
//!
//! Some fixtures (`backup::tests::service`) additionally returned
//! `(BackupService, tempfile::TempDir)` and callers destructured it as
//! `let (svc, _dir) = service().await;` - Rust drops a tuple pattern's
//! bindings in *reverse* of how they're listed, so `_dir` (position 2)
//! dropped **before** `svc` (position 1, which owns the still-open pool).
//! That is a second, independent bug on top of the missing retry - see
//! `apps/banto-hub/core/tests/common/mod.rs`'s module doc for the fuller
//! writeup (this app's `manager_env`-shaped tests had the exact same two
//! bugs) and `service`'s own doc comment here for how the fix reorders it.
//!
//! [`TempDir::new`] keeps using `tempfile::tempdir()` for its collision-safe
//! unique naming (`.keep()` hands the directory over without deleting it),
//! but takes over cleanup itself with a retry. This requires every test
//! using it to run on a multi-thread tokio runtime with >= 2 workers
//! (`Drop::drop` is synchronous, so the retry can only block via
//! `std::thread::sleep` - on a single-threaded runtime that starves the
//! only worker thread and prevents the background close from ever being
//! polled - see the `TempEnv` doc comment linked above for the measured
//! 0%-vs-100% success rates).
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::Duration;

const RETRY_DELAY: Duration = Duration::from_millis(50);
const MAX_ATTEMPTS: u32 = 40;

pub(crate) struct TempDir(PathBuf);

impl TempDir {
    pub(crate) fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        Self(dir)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        for attempt in 1..=MAX_ATTEMPTS {
            match std::fs::remove_dir_all(&self.0) {
                Ok(()) => return,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
                Err(_) if attempt < MAX_ATTEMPTS => {
                    std::thread::sleep(RETRY_DELAY);
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
