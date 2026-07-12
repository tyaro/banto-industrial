//! [`Clock`]: the one seam that lets [`crate::writer::TsWriter`]'s
//! local-midnight rotation (recorder-requirements.md §3.4) be driven by
//! wall-clock time in production and by an injected, deterministic time in
//! tests (design decision: "クロックは注入可能に（ローテーションのテストの
//! ため）").
//!
//! Split into two cheap, independent accessors rather than one "give me
//! today's local date" method so [`crate::date::LocalDate::from_epoch_ms`]'s
//! pure integer arithmetic (`date.rs`) stays the single place that turns
//! (epoch ms, offset ms) into a calendar date - a test clock only ever needs
//! to fake two `i64`s, never a calendar computation of its own.

use std::sync::atomic::{AtomicI64, Ordering};

/// Source of "now" for rotation decisions. `Send + Sync` (and object-safe -
/// no generics/associated types) so a writer can hold it as `Arc<dyn Clock>`
/// and share it across concurrent `append` callers, mirroring
/// `banto_plc::PlcClient`'s `dyn`-compatibility choice.
pub trait Clock: Send + Sync {
    /// Current time as UTC epoch milliseconds.
    fn now_ms(&self) -> i64;

    /// The UTC offset (milliseconds - e.g. JST = `+9 * 3_600_000`) to apply
    /// to an epoch-ms timestamp to get the local calendar date. Queried
    /// alongside `now_ms()` on every rotation check (not cached by the
    /// writer) so a long-running (24/365, recorder-requirements.md §4)
    /// process picks up a real DST transition if one ever occurs, rather
    /// than being pinned to whatever offset was in effect at `open()` time.
    fn utc_offset_ms(&self) -> i64;
}

/// Production clock: real wall-clock time and the OS's current local UTC
/// offset.
///
/// `time::UtcOffset::current_local_offset()` is documented as unsound to
/// call once other threads may have started *on Unix* (it reads process
/// environment/libc state that is not thread-safe there). This product is
/// Windows-only (recorder-requirements.md §1: "OS Windows 10/11" - the
/// Tauri desktop app and its LAN-server counterpart both ship on Windows),
/// where that restriction does not apply, so calling it here from any thread
/// at any time is fine. See `Cargo.toml`'s dependency comment for why `time`
/// (not `chrono`) and why this is the *only* place this crate touches it.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        let now = std::time::SystemTime::now();
        match now.duration_since(std::time::UNIX_EPOCH) {
            Ok(dur) => dur.as_millis() as i64,
            // System clock set before 1970 - not a real-world case for a
            // 2026-onward product, but fail to "epoch" rather than panic.
            Err(_) => 0,
        }
    }

    fn utc_offset_ms(&self) -> i64 {
        match time::UtcOffset::current_local_offset() {
            Ok(offset) => offset.whole_seconds() as i64 * 1000,
            // OS lookup failed (should not happen on Windows) - fall back to
            // UTC rather than propagating an error from every append().
            Err(_) => 0,
        }
    }
}

/// Deterministic, settable clock for tests - both this crate's own (rotation
/// tests in `writer.rs`) and, since it is public, downstream consumers'
/// (I3b's collection-engine tests want the same rotation determinism this
/// crate's tests need), same reuse-beyond-this-crate's-own-tests reasoning
/// as `banto_plc::modbus::simulator`.
///
/// `now_ms`/`utc_offset_ms` are independently settable `AtomicI64`s (not a
/// `Mutex`) so `set_now_ms`/`advance_ms` can be called from a `&ManualClock`
/// shared via `Arc` without an async lock, matching [`Clock`]'s sync,
/// non-blocking contract.
#[derive(Debug)]
pub struct ManualClock {
    now_ms: AtomicI64,
    utc_offset_ms: AtomicI64,
}

impl ManualClock {
    pub fn new(now_ms: i64, utc_offset_ms: i64) -> Self {
        Self {
            now_ms: AtomicI64::new(now_ms),
            utc_offset_ms: AtomicI64::new(utc_offset_ms),
        }
    }

    pub fn set_now_ms(&self, now_ms: i64) {
        self.now_ms.store(now_ms, Ordering::SeqCst);
    }

    pub fn advance_ms(&self, delta_ms: i64) {
        self.now_ms.fetch_add(delta_ms, Ordering::SeqCst);
    }

    pub fn set_utc_offset_ms(&self, utc_offset_ms: i64) {
        self.utc_offset_ms.store(utc_offset_ms, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> i64 {
        self.now_ms.load(Ordering::SeqCst)
    }

    fn utc_offset_ms(&self) -> i64 {
        self.utc_offset_ms.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_clock_reports_the_values_it_was_built_with() {
        let clock = ManualClock::new(1_000, 9 * 3_600_000);
        assert_eq!(clock.now_ms(), 1_000);
        assert_eq!(clock.utc_offset_ms(), 9 * 3_600_000);
    }

    #[test]
    fn manual_clock_set_now_ms_overwrites() {
        let clock = ManualClock::new(0, 0);
        clock.set_now_ms(42);
        assert_eq!(clock.now_ms(), 42);
    }

    #[test]
    fn manual_clock_advance_ms_is_additive() {
        let clock = ManualClock::new(100, 0);
        clock.advance_ms(50);
        assert_eq!(clock.now_ms(), 150);
        clock.advance_ms(-25);
        assert_eq!(clock.now_ms(), 125);
    }

    #[test]
    fn manual_clock_set_utc_offset_ms_overwrites() {
        let clock = ManualClock::new(0, 0);
        clock.set_utc_offset_ms(-8 * 3_600_000);
        assert_eq!(clock.utc_offset_ms(), -8 * 3_600_000);
    }

    #[test]
    fn system_clock_now_ms_is_plausibly_current() {
        // Sanity bound only (not a real assertion of correctness): well
        // after this crate was written and well before some absurd future.
        let ms = SystemClock.now_ms();
        assert!(ms > 1_700_000_000_000); // 2023-11-14
        assert!(ms < 4_000_000_000_000); // 2096-10-02
    }
}
