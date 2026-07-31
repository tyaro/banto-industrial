//! The writer (W3-B safety invariants #4, #5, #6,
//! `luminous-discovering-goblet.md`). This is the ONLY code in the whole app
//! that holds a write-capable [`BrokerHandle`] and the only place
//! `broker.write` is ever called. Everything upstream ([`crate::engine::poller`],
//! [`crate::engine::rule_engine`]) is structurally incapable of writing.
//!
//! ## The gate order (per [`PendingWrite`])
//!
//! For each intent, in this exact order:
//! 1. **resolve** the target to a [`WriteRequest`]. An unresolvable target is
//!    audited `failed` and skipped (no write attempted).
//! 2. **disarmed?** → audit `suppressed_disarmed`, skip (invariant #1).
//! 3. **rate limit would exceed?** → audit `rate_limit_tripped` /
//!    `suppressed_rate_limited`, **auto-disarm** (trip the breaker), skip
//!    (invariant #4). A manual re-arm is then required.
//! 4. **dry-run?** → audit `suppressed_dry_run`, skip - never call the broker
//!    (invariant #6).
//! 5. otherwise **log-before-write** (invariant #5): insert the `rule_fire`
//!    audit row FIRST, record the rate-limiter slot, THEN call `broker.write`,
//!    THEN update the row's result to `ok`/`failed`.
//!
//! Because a dry-run returns at step 4 it never records a rate-limiter slot, so
//! dry-run can never trip the breaker - there is no physical write storm to
//! guard against.
//!
//! ## Single-tasked
//!
//! The engine drives one writer from one task, feeding it [`PendingWrite`]s one
//! at a time, so the writer owns its [`RateLimiter`] with no locking and every
//! arming check sees the effect of the previous intent's auto-disarm.

use std::collections::HashMap;
use std::time::Instant;

use banto_plc::{Address, DataType};
use banto_plc_write::WriteRequest;
use sqlx::SqlitePool;

use super::arming::ArmingState;
use super::broker::BrokerHandle;
use super::rate_limiter::RateLimiter;
use super::rule_engine::{tag_value_as_f64, PendingWrite};
use super::write_audit::{
    insert_pending_fire, insert_row, set_result, AuditAction, AuditResult, AuditRow,
};

/// A write target resolved to everything the writer needs to issue a write:
/// which connection's broker handle, and the wire address/type.
#[derive(Debug, Clone)]
pub struct ResolvedTarget {
    pub connection_id: i64,
    pub address: Address,
    pub data_type: DataType,
}

/// The writer: the sole holder of write-capable broker handles.
pub struct Writer {
    pool: SqlitePool,
    arming: std::sync::Arc<ArmingState>,
    rate_limiter: RateLimiter,
    /// Write-capable handle per connection id. Held ONLY here.
    handles: HashMap<i64, BrokerHandle>,
    /// `write_target_id` → resolved target.
    targets: HashMap<i64, ResolvedTarget>,
}

impl Writer {
    pub fn new(
        pool: SqlitePool,
        arming: std::sync::Arc<ArmingState>,
        rate_limiter: RateLimiter,
        handles: HashMap<i64, BrokerHandle>,
        targets: HashMap<i64, ResolvedTarget>,
    ) -> Self {
        Self {
            pool,
            arming,
            rate_limiter,
            handles,
            targets,
        }
    }

    /// Process one write intent through the full safety gate. Errors here are
    /// only *infrastructure* failures (e.g. the audit DB is unreachable);
    /// business suppression is not an error - it is an audited outcome.
    pub async fn process(
        &mut self,
        pending: PendingWrite,
        now: Instant,
    ) -> Result<(), banto_core::BantoError> {
        // Snapshot fields shared by every audit row for this intent.
        let written_f64 = tag_value_as_f64(pending.value);
        let base = |result: AuditResult| {
            AuditRow::new(AuditAction::RuleFire, result, pending.rule_name.clone())
                .with_rule(pending.rule_id)
                .with_source(pending.source_tag_id, pending.source_value)
                .with_target(pending.write_target_id, Some(written_f64))
        };

        // (1) Resolve the target.
        let Some(target) = self.targets.get(&pending.write_target_id).cloned() else {
            insert_row(
                &self.pool,
                &base(AuditResult::Failed).with_detail("write target could not be resolved"),
            )
            .await?;
            return Ok(());
        };
        let Some(handle) = self.handles.get(&target.connection_id).cloned() else {
            insert_row(
                &self.pool,
                &base(AuditResult::Failed)
                    .with_detail("no broker handle for the target's connection"),
            )
            .await?;
            return Ok(());
        };

        // (2) Disarmed.
        if !self.arming.is_armed() {
            insert_row(&self.pool, &base(AuditResult::SuppressedDisarmed)).await?;
            return Ok(());
        }

        // (3) Rate limit would exceed -> trip breaker + auto-disarm.
        if self.rate_limiter.would_exceed(target.connection_id, now) {
            self.arming.disarm();
            let tripped = AuditRow::new(
                AuditAction::RateLimitTripped,
                AuditResult::SuppressedRateLimited,
                pending.rule_name.clone(),
            )
            .with_rule(pending.rule_id)
            .with_source(pending.source_tag_id, pending.source_value)
            .with_target(pending.write_target_id, Some(written_f64))
            .with_detail("rate limit exceeded; breaker tripped and engine auto-disarmed");
            insert_row(&self.pool, &tripped).await?;
            return Ok(());
        }

        // (4) Dry-run: audit the would-be write, never touch the broker.
        if self.arming.is_dry_run() {
            insert_row(&self.pool, &base(AuditResult::SuppressedDryRun)).await?;
            return Ok(());
        }

        // (5) Log-before-write.
        let audit_id = insert_pending_fire(&self.pool, &base(AuditResult::Ok)).await?;
        // Count this real write against the rate windows (only reached on the
        // true write path, so a dry-run never consumes budget).
        self.rate_limiter.record(target.connection_id, now);

        let request = WriteRequest {
            address: target.address,
            data_type: target.data_type,
            value: pending.value,
        };
        let result = handle.write(vec![request]).await;

        let final_result = match &result {
            Ok(results) if results.first().is_some_and(|r| r.is_ok()) => AuditResult::Ok,
            _ => AuditResult::Failed,
        };
        set_result(&self.pool, audit_id, final_result).await?;
        Ok(())
    }
}
