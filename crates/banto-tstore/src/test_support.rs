//! Shared `#[cfg(test)]`-only helpers for this crate's unit test modules
//! (`writer::tests`/`reader::tests`). Not a `tests/*.rs` integration test -
//! those get their own `tests/common/mod.rs` (see
//! `apps/banto-hub/core/tests/common/mod.rs` for that pattern) - this is the
//! equivalent for `#[cfg(test)] mod tests` blocks living inside `src/`,
//! which can only share code via a `pub(crate)` module declared here at the
//! crate root.
//!
//! ## Why every temp-file/dir cleanup in this crate retries (2026-08-08)
//!
//! Every `TsWriter`/`TsReader` in this crate's tests opens a WAL-mode
//! `SqlitePool`. On Windows, closing such a pool does not synchronously
//! release the underlying file handles - even after `pool.close().await`
//! returns, the OS can hold the file "in use" for a short additional
//! window, so a `remove_dir_all`/`remove_file` issued immediately after can
//! observe `ERROR_SHARING_VIOLATION`. [`retry_remove`] retries on a short
//! delay to reliably close this window (measured directly in
//! `banto-hub-core`'s identically-caused leak - see
//! `apps/banto-hub/core/tests/common/mod.rs`'s module doc for the full
//! writeup and measurements).
//!
//! This requires every test using these helpers to run on a multi-thread
//! tokio runtime with >= 2 workers (`Drop::drop` is synchronous, so the
//! retry can only block via `std::thread::sleep` - on a single-threaded
//! runtime that would starve the only worker thread and prevent the
//! background close from ever being polled) - hence every `#[tokio::test]`
//! in `writer.rs`/`reader.rs` is annotated
//! `#[tokio::test(flavor = "multi_thread", worker_threads = 2)]` rather
//! than the bare `#[tokio::test]`.

use std::path::Path;
use std::time::Duration;

/// Delay between removal retries in [`retry_remove`].
const RETRY_DELAY: Duration = Duration::from_millis(50);

/// Retry ceiling in [`retry_remove`] - `RETRY_DELAY * MAX_ATTEMPTS` (~2s) is
/// the worst-case teardown block, kept generous because the measured common
/// case converges within 1-2 attempts.
const MAX_ATTEMPTS: u32 = 40;

/// Retry `remove` (typically `std::fs::remove_dir_all` or
/// `std::fs::remove_file`, partially applied to a path via a closure) until
/// it succeeds, is a `NotFound` (nothing to remove - not an error; several
/// tests never actually create the path in the first place), or
/// [`MAX_ATTEMPTS`] is exhausted (logs a breadcrumb and gives up rather than
/// looping forever or panicking inside a `Drop`).
pub(crate) fn retry_remove(path: &Path, remove: impl Fn(&Path) -> std::io::Result<()>) {
    for attempt in 1..=MAX_ATTEMPTS {
        match remove(path) {
            Ok(()) => return,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
            Err(_) if attempt < MAX_ATTEMPTS => {
                std::thread::sleep(RETRY_DELAY);
            }
            Err(err) => {
                eprintln!(
                    "test cleanup: giving up removing {path:?} after {attempt} attempts: {err}"
                );
            }
        }
    }
}
