//! The engine's arming state (W3-B safety invariant #1,
//! `luminous-discovering-goblet.md`). A tiny lock-free holder for the two live
//! flags that gate every physical write - `armed` and `dry_run` - plus one
//! piece of read-only history.
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
//! ## Pure, sync, tauri/DB-free
//!
//! This type is just `AtomicBool`s: no pool, no async, no audit. Persisting a
//! flip to `armed_state` and writing the `arm`/`disarm`/`dry_run_toggle` audit
//! row is the wiring layer's job ([`crate::engine::EngineControl`] /
//! [`crate::engine::write_audit`]), keeping the state machine itself trivially
//! testable.

use std::sync::atomic::{AtomicBool, Ordering};

/// The live arm/dry-run flags plus the informational was-armed-before-restart
/// bit. Cheap to share behind an `Arc` (the writer and the control handle both
/// hold one).
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
}

impl ArmingState {
    /// Construct with the live `armed` flag forced to `false`. `was_armed_persisted`
    /// is the value read from the `armed_state` table and is exposed *only* via
    /// [`Self::was_armed_before_restart`]; it never influences the live flag.
    pub fn new(was_armed_persisted: bool) -> Self {
        Self {
            armed: AtomicBool::new(false),
            dry_run: AtomicBool::new(false),
            was_armed_before_restart: was_armed_persisted,
        }
    }

    pub fn arm(&self) {
        self.armed.store(true, Ordering::SeqCst);
    }

    pub fn disarm(&self) {
        self.armed.store(false, Ordering::SeqCst);
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructs_disarmed_even_when_persisted_armed() {
        let state = ArmingState::new(true);
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
        let state = ArmingState::new(false);
        assert!(!state.is_armed());
        state.arm();
        assert!(state.is_armed());
        state.disarm();
        assert!(!state.is_armed());

        assert!(!state.is_dry_run());
        state.set_dry_run(true);
        assert!(state.is_dry_run());
        state.set_dry_run(false);
        assert!(!state.is_dry_run());
    }
}
