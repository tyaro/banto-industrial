//! Shared `#[cfg(test)]`-only helpers for this crate's **unit** test modules
//! (`#[cfg(test)] mod tests` blocks living inside `src/*.rs`, e.g.
//! `hub.rs::tests`) - not the `tests/*.rs` integration tests, which have
//! their own `tests/common/mod.rs` (see
//! `apps/banto-hub/core/tests/common/mod.rs`'s module doc for the full
//! writeup of the leak this also fixes: closing a WAL-mode `SqlitePool` on
//! Windows does not synchronously release its file handles, so
//! `remove_dir_all` issued right after can transiently fail).
//!
//! ## Why this module exists (2026-08-08 audit finding)
//!
//! `hub.rs::tests::manager_env` used to build its temp dir with
//! `tempfile::tempdir()` directly and return `(SqlitePool, tempfile::TempDir,
//! CollectorManager)`. Every call site destructured it as
//! `let (_pool, _dir, manager) = manager_env().await;` - and Rust drops a
//! `let (a, b, c) = ...` pattern's bindings in *reverse* textual order
//! (`c`, then `b`, then `a`), so `_dir` (position 2) dropped **before**
//! `_pool` (position 1). `tempfile::TempDir::drop` does one, non-retrying
//! `remove_dir_all` and swallows the error - with `_pool`'s registry file
//! still open at that point, it silently failed every time. Measured: one
//! `cargo test -p banto-hub-core --lib` run left 5 directories behind
//! (`.tmpXXXXXXXX`, each holding a live `registry.sqlite3`).
//!
//! Two independent things had to change, not just adding a retry:
//! 1. **Drop order**: unlike a `struct`'s fields (which always drop in
//!    forward declaration order regardless of how a caller uses the value),
//!    a *tuple*'s bindings drop in reverse of how the caller's `let`
//!    pattern lists them - so the fix has to live at each call site, not
//!    just in this module. `manager_env` now returns
//!    `(TempDir, CollectorManager, SqlitePool)` (dir *first*, pool *last*),
//!    and every call site destructures naturally in that order, so the
//!    pool - now bound last - drops *first*, before `dir`'s cleanup runs.
//! 2. **Retry**: even with correct ordering, the residual OS-level close
//!    lag `apps/banto-hub/core/tests/common/mod.rs` documents still
//!    applies, so [`TempDir`] retries the same way `TempEnv` there does -
//!    which is also why every test using it must run on a multi-thread
//!    tokio runtime with >= 2 workers (a blocking retry inside `Drop::drop`
//!    on a single-threaded runtime starves the only worker thread and
//!    prevents the background close from ever being polled - see that same
//!    module doc for the measured 0%-vs-100% success rates).
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static COUNTER: AtomicU64 = AtomicU64::new(0);

const RETRY_DELAY: Duration = Duration::from_millis(50);
// 40 (~2s) was the original budget; measured flaky under a full `cargo
// test -p banto-hub-core --lib` run (136 tests, high thread contention) -
// 2 of `hub.rs`'s `manager_env`-based tests occasionally still leaked even
// with correct drop ordering, consistent with the OS-level residual close
// lag this retry exists for simply taking longer under load. Doubled to
// give more headroom without changing the common-case cost (convergence is
// still typically within the first 1-2 attempts).
const MAX_ATTEMPTS: u32 = 80;

/// A fresh temp directory for one unit test, cleaned up on drop. See this
/// module's doc comment for why `drop` retries and why callers must control
/// the drop order relative to any `SqlitePool` pointed at a file inside it.
pub(crate) struct TempDir(PathBuf);

impl TempDir {
    pub(crate) fn new(label: &str) -> Self {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "banto-hub-core-unit-test-{}-{label}-{id}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
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
