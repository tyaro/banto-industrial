//! The engine's arming state (W3-B safety invariant #1,
//! `luminous-discovering-goblet.md`). A tiny lock-free holder for the two live
//! flags that gate every physical write - `armed` and `dry_run` - plus one
//! piece of read-only history, plus (H10 ②, 2026-08-08 オーナー決定
//! `docs/improvement-plan.md` H10) the timed arm auto-expiry policy/state.
//!
//! ## The one rule that makes this safe: armed ALWAYS constructs to `false`
//!
//! [`ArmingState::new`] hard-codes the live `armed` flag to `false`, no matter
//! what was persisted. The persisted `armed_state.armed_persisted` row is
//! loaded *only* into [`ArmingState::was_armed_before_restart`], purely so the
//! UI can say "this engine was armed when it last shut down". It is NEVER fed
//! into the live flag. This is the anti-footgun the `db.rs` / migration doc
//! comments call out: a process restart (crash, power blip, redeploy) must
//! never silently resume live actuation - a human has to re-arm.
//!
//! ## H10 ②: timed arm auto-expiry
//!
//! [`ArmingState::arm`] takes an injected `now: Instant` (the same
//! deterministic-clock seam [`crate::engine::rate_limiter::RateLimiter`] and
//! [`crate::engine::writer::Writer`] already use - relay-wright has no
//! `SystemClock`/`ManualClock` abstraction; a threaded-in monotonic
//! `std::time::Instant` IS the test seam here) and records it in `armed_at`,
//! cleared again by [`ArmingState::disarm`]. `auto_disarm` is the configured
//! window (`None` disables the feature entirely); [`ArmingState::is_expired`]
//! and [`ArmingState::remaining`] are pure functions of `armed_at`/
//! `auto_disarm` against a caller-supplied `now` - this type never reads the
//! wall clock itself. Enforcing the expiry (flipping `armed` back to `false`,
//! persisting it, and auditing it) is the wiring layer's job
//! ([`crate::engine::writer::Writer::enforce_arm_expiry`], driven once per
//! tick from [`crate::engine::run_engine_loop`]) - this module only tracks
//! the state.
//!
//! ## Pure, sync, tauri/DB-free
//!
//! This type is `AtomicBool`s plus a small `Mutex<Option<Instant>>` for the
//! arm timestamp: no pool, no async, no audit. Persisting a flip to
//! `armed_state` and writing the `arm`/`disarm`/`dry_run_toggle`/auto-disarm
//! audit row is the wiring layer's job ([`crate::engine::EngineControl`] /
//! [`crate::engine::writer::Writer`] / [`crate::engine::write_audit`]),
//! keeping the state machine itself trivially testable.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The live arm/dry-run flags plus the informational was-armed-before-restart
/// bit and the H10 ② auto-expiry policy/state. Cheap to share behind an `Arc`
/// (the writer and the control handle both hold one).
#[derive(Debug)]
pub struct ArmingState {
    /// The live "may this engine issue physical writes?" flag. ALWAYS `false`
    /// on construction (see the module doc).
    armed: AtomicBool,
    /// When `true`, the engine evaluates and audits would-be writes but never
    /// calls `broker.write` (safety invariant #6).
    dry_run: AtomicBool,
    /// The persisted armed value observed at startup - informational ONLY.
    was_armed_before_restart: bool,
    /// When the engine was last armed (the `now` passed to [`Self::arm`]),
    /// `None` while disarmed. Set on [`Self::arm`], cleared on
    /// [`Self::disarm`] - a plain `Mutex` rather than an atomic since
    /// `Instant` has no lock-free representation; the critical section is a
    /// single read/write, never held across an await.
    armed_at: Mutex<Option<Instant>>,
    /// The configured auto-disarm window (H10 ②, `arm.auto_disarm_secs`
    /// settings key, default 8h = 1 shift). `None` disables the feature
    /// entirely - an engine built with `None` never auto-expires, regardless
    /// of how long it stays armed. Fixed for the lifetime of this
    /// `ArmingState` (a settings change takes effect on the next
    /// `engine_reload`, which rebuilds the engine - and this state - from
    /// scratch, same as every other `EngineConfig` tunable).
    auto_disarm: Option<Duration>,
}

impl ArmingState {
    /// Construct with the live `armed` flag forced to `false`. `was_armed_persisted`
    /// is the value read from the `armed_state` table and is exposed *only* via
    /// [`Self::was_armed_before_restart`]; it never influences the live flag.
    /// `auto_disarm` is the H10 ② window (`None` disables auto-expiry).
    pub fn new(was_armed_persisted: bool, auto_disarm: Option<Duration>) -> Self {
        Self {
            armed: AtomicBool::new(false),
            dry_run: AtomicBool::new(false),
            was_armed_before_restart: was_armed_persisted,
            armed_at: Mutex::new(None),
            auto_disarm,
        }
    }

    /// Arm at `now` (H10 ②: records the arm timestamp the auto-expiry window
    /// counts from). `now` is the injected monotonic clock - see the module
    /// doc's "H10 ②" section.
    pub fn arm(&self, now: Instant) {
        self.armed.store(true, Ordering::SeqCst);
        *self.armed_at.lock().expect("armed_at mutex poisoned") = Some(now);
    }

    /// Disarm - also clears `armed_at` (H10 ②), so a later re-arm starts a
    /// fresh auto-expiry window rather than inheriting a stale one.
    pub fn disarm(&self) {
        self.armed.store(false, Ordering::SeqCst);
        *self.armed_at.lock().expect("armed_at mutex poisoned") = None;
    }

    pub fn is_armed(&self) -> bool {
        self.armed.load(Ordering::SeqCst)
    }

    pub fn set_dry_run(&self, on: bool) {
        self.dry_run.store(on, Ordering::SeqCst);
    }

    pub fn is_dry_run(&self) -> bool {
        self.dry_run.load(Ordering::SeqCst)
    }

    /// Whether the persisted state at startup was "armed" - for UI display
    /// only. Has no bearing on whether the engine is live now.
    pub fn was_armed_before_restart(&self) -> bool {
        self.was_armed_before_restart
    }

    /// The configured H10 ② auto-disarm window, or `None` if the feature is
    /// disabled (`arm.auto_disarm_secs = 0`).
    pub fn auto_disarm(&self) -> Option<Duration> {
        self.auto_disarm
    }

    /// H10 ②: has the arm window elapsed as of `now`? Pure function of
    /// `is_armed()`/`armed_at`/`auto_disarm` against the supplied `now` - a
    /// disarmed engine, or one with `auto_disarm = None`, is never expired.
    /// The wiring layer ([`crate::engine::writer::Writer::enforce_arm_expiry`])
    /// is what actually acts on this.
    pub fn is_expired(&self, now: Instant) -> bool {
        let Some(window) = self.auto_disarm else {
            return false;
        };
        if !self.is_armed() {
            return false;
        }
        let armed_at = *self.armed_at.lock().expect("armed_at mutex poisoned");
        match armed_at {
            Some(at) => at + window <= now,
            None => false,
        }
    }

    /// H10 ②: time remaining until auto-disarm as of `now` - `Some(Duration::ZERO)`
    /// at/after the deadline (saturating, never negative), `None` while
    /// disarmed or when `auto_disarm` is disabled. Used to populate
    /// `EngineStatus.arm_remaining_secs` for the UI countdown.
    pub fn remaining(&self, now: Instant) -> Option<Duration> {
        if !self.is_armed() {
            return None;
        }
        let window = self.auto_disarm?;
        let armed_at = (*self.armed_at.lock().expect("armed_at mutex poisoned"))?;
        let deadline = armed_at + window;
        Some(deadline.saturating_duration_since(now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructs_disarmed_even_when_persisted_armed() {
        let state = ArmingState::new(true, None);
        assert!(
            !state.is_armed(),
            "live armed flag must be false on construction regardless of persisted value"
        );
        assert!(
            state.was_armed_before_restart(),
            "persisted armed=true must survive as informational history"
        );
    }

    #[test]
    fn arm_disarm_and_dry_run_toggle() {
        let state = ArmingState::new(false, None);
        assert!(!state.is_armed());
        state.arm(Instant::now());
        assert!(state.is_armed());
        state.disarm();
        assert!(!state.is_armed());

        assert!(!state.is_dry_run());
        state.set_dry_run(true);
        assert!(state.is_dry_run());
        state.set_dry_run(false);
        assert!(!state.is_dry_run());
    }

    // --- H10 ②: timed arm auto-expiry ---------------------------------------

    /// The deterministic `Instant`-ladder proof (mirrors `writer.rs`'s
    /// `storm_trips_breaker_and_only_rearm_plus_window_slide_recovers`): a
    /// configured window makes `is_expired`/`remaining` behave exactly at,
    /// just before, and after the deadline - no wall clock in the assertions.
    #[test]
    fn auto_disarm_expiry_ladder_and_remaining() {
        let window = Duration::from_secs(100);
        let state = ArmingState::new(false, Some(window));
        let t0 = Instant::now();
        state.arm(t0);

        assert!(!state.is_expired(t0), "not expired at the moment of arming");
        assert!(
            !state.is_expired(t0 + window - Duration::from_millis(1)),
            "not expired just before the deadline"
        );
        assert!(
            state.is_expired(t0 + window),
            "expired exactly at the deadline (>=, not >)"
        );
        assert!(
            state.is_expired(t0 + window + Duration::from_secs(1)),
            "stays expired after the deadline"
        );

        assert_eq!(
            state.remaining(t0),
            Some(window),
            "full window remaining at arm time"
        );
        assert_eq!(
            state.remaining(t0 + Duration::from_secs(40)),
            Some(Duration::from_secs(60)),
            "remaining ticks down linearly"
        );
        assert_eq!(
            state.remaining(t0 + window),
            Some(Duration::ZERO),
            "remaining saturates to zero at the deadline"
        );
        assert_eq!(
            state.remaining(t0 + window + Duration::from_secs(1)),
            Some(Duration::ZERO),
            "remaining saturates to zero after the deadline, never negative"
        );
    }

    #[test]
    fn auto_disarm_none_never_expires() {
        let state = ArmingState::new(false, None);
        let t0 = Instant::now();
        state.arm(t0);

        assert!(
            !state.is_expired(t0 + Duration::from_secs(365 * 24 * 3600)),
            "auto_disarm = None must never expire, no matter how long armed"
        );
        assert_eq!(
            state.remaining(t0),
            None,
            "remaining is None when the feature is disabled"
        );
        assert_eq!(state.auto_disarm(), None);
    }

    #[test]
    fn not_armed_is_never_expired_even_with_a_window_configured() {
        let state = ArmingState::new(false, Some(Duration::from_millis(1)));
        let t0 = Instant::now();
        // Never armed - `is_expired`/`remaining` must both read as "not
        // applicable", not "expired" (gate #2 in `Writer::process` - disarmed
        // - already covers this case; expiry must not double-report it).
        assert!(!state.is_expired(t0 + Duration::from_secs(1)));
        assert_eq!(state.remaining(t0), None);
    }

    #[test]
    fn disarm_clears_armed_at_so_expiry_state_resets() {
        let window = Duration::from_secs(10);
        let state = ArmingState::new(false, Some(window));
        let t0 = Instant::now();
        state.arm(t0);
        assert!(state.is_expired(t0 + Duration::from_secs(20)));

        state.disarm();
        assert!(!state.is_armed());
        assert!(
            !state.is_expired(t0 + Duration::from_secs(20)),
            "disarmed must never read as expired"
        );
        assert_eq!(state.remaining(t0 + Duration::from_secs(20)), None);

        // Re-arming starts a FRESH window rather than inheriting the old
        // (already past-deadline) `armed_at`.
        let t1 = t0 + Duration::from_secs(1000);
        state.arm(t1);
        assert!(
            !state.is_expired(t1),
            "a fresh arm must not be immediately expired"
        );
        assert_eq!(state.remaining(t1), Some(window));
    }
}
