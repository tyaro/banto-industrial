//! Shared test scaffolding for `banto-hub-core`'s integration tests
//! (`tests/*.rs` - each compiles as an independent binary, so this module is
//! pulled in via `mod common;` in each one, the standard `tests/common/mod.rs`
//! pattern: naming it `common.rs` instead would make cargo treat it as its
//! own test binary).
//!
//! Until 2026-08-08 every `tests/*.rs` file in this crate carried its own,
//! byte-for-byte copy of `TempEnv` (12 copies - see PR #54's investigation
//! notes). That PR discovered the leak; this module fixes it and becomes the
//! single copy every test file now shares.
//!
//! ## The leak (root cause, confirmed by measurement 2026-08-08)
//!
//! Every test here opens a *file-backed* `SqlitePool` in WAL mode
//! (`banto_hub_core::db::init_db` -> `banto_storage::connect_sqlite`) -
//! required because `CollectorManager`/services hand out several pool
//! clones concurrently, and each `:memory:` connection would otherwise be
//! its own empty database. On Windows, closing a `SqlitePool` clone does
//! not synchronously release the underlying file handles: the actual
//! `CloseHandle` happens on sqlx-sqlite's per-connection worker machinery
//! and, even after that returns, the OS can hold the file "in use" for a
//! short additional window. A `remove_dir_all` issued immediately after the
//! last pool clone is dropped observes `ERROR_SHARING_VIOLATION` almost
//! every time - measured at ~1/15 (7%) immediate success across repeated
//! trials, whether or not an explicit `pool.close().await` preceded the
//! drop (an explicit close only raised that to ~7/15, 47% - still not
//! reliable on its own).
//!
//! ## The fix: retry, and why it must be a `Drop`-time retry
//!
//! [`TempEnv::drop`] retries `remove_dir_all` on a short delay
//! ([`RETRY_DELAY`], up to [`MAX_ATTEMPTS`] times - about 2s worst case).
//! Measured: with this retry in place, cleanup succeeded 15/15 trials,
//! converging within 1-2 attempts (i.e. usually inside the first 50-100ms)
//! in every observed run.
//!
//! `Drop::drop` is synchronous, so the retry delay can only be a blocking
//! `std::thread::sleep` - not `tokio::time::sleep(..).await`. This has a
//! critical consequence, also measured directly: **on a single-threaded
//! (`#[tokio::test]` default / `flavor = "current_thread"`) runtime, a
//! blocking sleep inside `Drop::drop` starves the only worker thread**, so
//! the background task that actually finishes closing the SQLite
//! connection never gets polled - cleanup then fails 100% of the time
//! (0/15 trials succeeded even within a full second of retrying). Under a
//! multi-thread runtime with >= 2 workers, the other worker(s) keep
//! progressing that background task while one thread blocks in `drop`, so
//! the retry converges quickly and reliably (the 15/15 result above).
//!
//! **Consequence: every test that owns a [`TempEnv`] must run under
//! `#[tokio::test(flavor = "multi_thread", worker_threads = 2)]` (or more
//! workers) - never the bare `#[tokio::test]` default.** This is not
//! optional polish; it is what makes the `Drop` retry actually work at all.
//!
//! ## Why `TestApp` also needs [`shutdown_test_app`] - the retry alone is
//! ## NOT enough once a `CollectorManager` is involved
//!
//! The measurements above used a bare `SqlitePool` with a handful of short
//! lived clones - close to worst case for the OS-level release lag, but
//! every clone was actually returned to the pool before `drop` ran. A real
//! `TestApp` is different: `CollectorManager::rebuild()` spawns a
//! **long-lived background task per PLC connection**
//! (`banto_collect::Collector`), and each of those tasks holds its own
//! `SqlitePool` connection checked out **for as long as the task keeps
//! running** - `tokio::spawn` detaches it from the test function entirely,
//! so it keeps running (and keeps that connection open) past the point
//! where the test returns and `TestApp`/`TempEnv` go out of scope, unless
//! something explicitly stops it first.
//!
//! Measured directly (2026-08-08, `cargo test -p banto-hub-core` x2 without
//! this fix): the *majority* of directories across most `tests/*.rs` files
//! were left behind - not the rare residual-lag failure the retry alone
//! fixes, but a connection that was never closed at all within the
//! `TempEnv::drop` retry's ~2s budget, because the collector task backing
//! it was still running. No amount of retrying helps here: the file handle
//! genuinely stays open until that task is stopped (or the whole test
//! *process* exits, which is exactly the accumulation PR #54 found).
//!
//! The fix: every `TestApp` in this crate's `tests/*.rs` files implements
//! `Drop` and calls [`shutdown_test_app`] first thing, which drives
//! `CollectorManager::shutdown()` (stops every collector task, per-crate
//! contract) followed by `pool.close()` to completion *before* `TestApp`'s
//! own fields (including its `TempEnv`) are dropped - Rust always runs a
//! type's explicit `Drop::drop` before auto-dropping its fields, so this
//! ordering is guaranteed regardless of field declaration order. With that
//! in place, every registry connection is genuinely closed by the time
//! `TempEnv::drop`'s retry runs, restoring the 100%-success case measured
//! above.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use banto_hub_core::hub::CollectorManager;
use sqlx::SqlitePool;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Delay between `remove_dir_all` retries in [`TempEnv::drop`].
const RETRY_DELAY: Duration = Duration::from_millis(50);

/// Retry ceiling in [`TempEnv::drop`] - `RETRY_DELAY * MAX_ATTEMPTS` (~2s)
/// is the worst-case time a single test's teardown can block on this, kept
/// generous because the measured common case converges within 1-2 attempts.
const MAX_ATTEMPTS: u32 = 40;

/// A temp directory holding the registry DB and the tstore data dir - the
/// registry must be *file-backed* (not `:memory:`): `CollectorManager`
/// hands out several pool connections concurrently (registry reads, event
/// persistence, per-connection tasks), and each `:memory:` connection is a
/// separate empty database.
///
/// See this module's doc comment for why [`TempEnv::drop`] retries, and why
/// every test using this type must run on a multi-thread tokio runtime.
pub struct TempEnv {
    root: PathBuf,
}

impl TempEnv {
    /// `prefix` identifies the calling test file (e.g. `"banto-hub-it"`,
    /// `"banto-hub-t7-2-it"`) so a directory left behind by a genuinely
    /// panicking test (the one case the retry in `drop` can't fully save -
    /// see its doc comment) is still traceable to its origin. `label`
    /// identifies the individual test/fixture within that file.
    pub fn new(prefix: &str, label: &str) -> Self {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        // The nanosecond timestamp (in addition to the PID + counter)
        // guards against PID reuse colliding with an old, already
        // initialized directory from a previous run (see PR #54).
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "{prefix}-{}-{label}-{id}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("create temp env");
        Self { root }
    }

    pub fn registry_path(&self) -> PathBuf {
        self.root.join("registry.sqlite3")
    }

    pub fn data_dir(&self) -> PathBuf {
        self.root.join("data")
    }
}

impl Drop for TempEnv {
    fn drop(&mut self) {
        for attempt in 1..=MAX_ATTEMPTS {
            match std::fs::remove_dir_all(&self.root) {
                Ok(()) => return,
                Err(_) if attempt < MAX_ATTEMPTS => {
                    std::thread::sleep(RETRY_DELAY);
                }
                Err(err) => {
                    // Last resort: leave a breadcrumb instead of silently
                    // leaking - this module's doc comment explains the
                    // remaining cases this can happen (a `TestApp` that
                    // doesn't call `shutdown_test_app` first, or a panic
                    // unwinding through a single-threaded context).
                    eprintln!(
                        "TempEnv: giving up removing {:?} after {attempt} attempts: {err}",
                        self.root
                    );
                }
            }
        }
    }
}

/// Synchronously drives `manager.shutdown()` (stops every background
/// per-connection collector task, releasing the registry `SqlitePool`
/// connections they hold checked out) followed by `pool.close()`, from
/// within a `Drop` impl. See this module's doc comment ("Why `TestApp` also
/// needs `shutdown_test_app`") for why this is required, not optional.
///
/// Every `TestApp` in this crate's `tests/*.rs` files must call this as the
/// first thing in its own `Drop::drop`:
///
/// ```ignore
/// impl Drop for TestApp {
///     fn drop(&mut self) {
///         common::shutdown_test_app(&self.manager, &self.pool);
///     }
/// }
/// ```
///
/// Uses `block_in_place` + `Handle::block_on` to run async teardown from a
/// sync `Drop::drop` - requires the ambient tokio runtime to be
/// multi-thread (already a hard requirement for [`TempEnv`], see above);
/// `block_in_place` panics outright on a single-threaded runtime.
///
/// Wrapped in `catch_unwind`: if this is running while a test is already
/// panicking (e.g. an assertion failure happened to fire while `manager`'s
/// internal lock was held, poisoning it), letting a *second* panic escape
/// from inside a `Drop::drop` that is itself running during unwind would
/// abort the whole test process instead of just failing the one test. On
/// that path `TempEnv::drop`'s retry may still be unable to fully clean up
/// (the collector task never actually stopped) - an accepted, rare
/// exception to the "always cleans up" guarantee, not a silent process
/// abort.
pub fn shutdown_test_app(manager: &CollectorManager, pool: &SqlitePool) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                manager.shutdown().await;
                pool.close().await;
            });
        });
    }));
}
