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

use banto_plc::{Address, PlcValue};
use banto_plc_write::{BatchWriteRequest, StringWriteRequest, WriteRequest};
use sqlx::SqlitePool;

use super::arming::ArmingState;
use super::broker::BrokerHandle;
use super::rate_limiter::RateLimiter;
use super::rule_engine::{plc_value_as_f64, PendingWrite, WireShape};
use super::write_audit::{
    insert_pending_fire, insert_row, set_result, AuditAction, AuditResult, AuditRow,
};

/// How many characters of a string source/written value are kept in the audit
/// detail JSON (S2 文字列タグ) - enough to identify a recipe/status string
/// without unbounded audit rows. Counted in `char`s, so multibyte SJIS text is
/// never cut mid-character.
const MAX_AUDIT_TEXT_CHARS: usize = 100;

/// A write target resolved to everything the writer needs to issue a write:
/// which connection's broker handle, the wire address, and its shape (numeric
/// or, since S2, a string device's word span).
#[derive(Debug, Clone)]
pub struct ResolvedTarget {
    pub connection_id: i64,
    pub address: Address,
    pub shape: WireShape,
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
        // Snapshot fields shared by every audit row for this intent. A string
        // write leaves the numeric `target_value_written` column NULL and
        // carries its text in the detail JSON instead (S2 文字列タグ); a
        // numeric write is the reverse.
        let written_f64: Option<f64> = match &pending.value {
            PlcValue::Str(_) => None,
            other => Some(plc_value_as_f64(other)),
        };
        let detail = string_write_detail(&pending);
        let base = |result: AuditResult| {
            let mut row = AuditRow::new(AuditAction::RuleFire, result, pending.rule_name.clone())
                .with_rule(pending.rule_id)
                .with_source(pending.source_tag_id, pending.source_value)
                .with_target(pending.write_target_id, written_f64);
            if let Some(detail) = &detail {
                row = row.with_detail(detail.clone());
            }
            row
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
            .with_target(pending.write_target_id, written_f64)
            .with_detail("rate limit exceeded; breaker tripped and engine auto-disarmed");
            insert_row(&self.pool, &tripped).await?;
            return Ok(());
        }

        // (4) Dry-run: audit the would-be write, never touch the broker.
        if self.arming.is_dry_run() {
            insert_row(&self.pool, &base(AuditResult::SuppressedDryRun)).await?;
            return Ok(());
        }

        // Build the wire request first: a value whose kind cannot match the
        // target's shape (save-time-prevented, but defended here) is audited
        // `failed` with no physical write and no rate-limiter charge.
        let Some(request) = build_request(&target, &pending.value) else {
            insert_row(
                &self.pool,
                &base(AuditResult::Failed)
                    .with_detail("write value type does not match the target"),
            )
            .await?;
            return Ok(());
        };

        // (5) Log-before-write.
        let audit_id = insert_pending_fire(&self.pool, &base(AuditResult::Ok)).await?;
        // Count this real write against the rate windows (only reached on the
        // true write path, so a dry-run never consumes budget). A string write
        // is one request, so it counts as one exactly like a numeric write.
        self.rate_limiter.record(target.connection_id, now);

        let result = handle.write(vec![request]).await;

        let final_result = match &result {
            Ok(results) if results.first().is_some_and(|r| r.is_ok()) => AuditResult::Ok,
            _ => AuditResult::Failed,
        };
        set_result(&self.pool, audit_id, final_result).await?;
        Ok(())
    }
}

/// Turn a resolved target and the value to write into the (numeric or string)
/// batch write request. `None` when the value's kind cannot match the target's
/// shape - a numeric target handed a string, or a string target handed a
/// non-string (both save-time-prevented; defended here rather than panicking).
fn build_request(target: &ResolvedTarget, value: &PlcValue) -> Option<BatchWriteRequest> {
    match target.shape {
        WireShape::Numeric(data_type) => value.as_tag_value().map(|value| {
            BatchWriteRequest::Numeric(WriteRequest {
                address: target.address,
                data_type,
                value,
            })
        }),
        WireShape::Str { words } => match value {
            PlcValue::Str(text) => Some(BatchWriteRequest::String(StringWriteRequest {
                address: target.address,
                words,
                value: text.clone(),
            })),
            _ => None,
        },
    }
}

/// The audit detail JSON for a write that touches string text (S2 文字列タグ):
/// `{"sourceText":..,"writtenText":..}` with each side present only when it is
/// a string, and each truncated to [`MAX_AUDIT_TEXT_CHARS`]. `None` for a
/// purely numeric write, so numeric audit rows keep their `detail` NULL exactly
/// as before.
fn string_write_detail(pending: &PendingWrite) -> Option<String> {
    let written_text = match &pending.value {
        PlcValue::Str(s) => Some(s.as_str()),
        _ => None,
    };
    let source_text = pending.source_text.as_deref();
    if written_text.is_none() && source_text.is_none() {
        return None;
    }
    let mut obj = serde_json::Map::new();
    if let Some(s) = source_text {
        obj.insert("sourceText".to_string(), truncate_text(s).into());
    }
    if let Some(s) = written_text {
        obj.insert("writtenText".to_string(), truncate_text(s).into());
    }
    Some(serde_json::Value::Object(obj).to_string())
}

/// Truncate to [`MAX_AUDIT_TEXT_CHARS`] on a `char` boundary (never mid-byte).
fn truncate_text(s: &str) -> String {
    if s.chars().count() <= MAX_AUDIT_TEXT_CHARS {
        s.to_string()
    } else {
        s.chars().take(MAX_AUDIT_TEXT_CHARS).collect()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use banto_plc::DataType;
    use sqlx::SqlitePool;

    use super::*;
    use crate::db::init_db_memory;
    use crate::engine::broker::spawn_test_handle_answering_ok;
    use crate::engine::rate_limiter::RateLimitConfig;

    fn pending(rule_id: i64, value: f64) -> PendingWrite {
        PendingWrite {
            rule_id,
            rule_name: format!("R{rule_id}"),
            write_target_id: 1,
            source_tag_id: Some(10),
            source_value: Some(1.0),
            source_text: None,
            value: PlcValue::F64(value),
        }
    }

    async fn count(pool: &SqlitePool, action: &str, result: &str) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM write_audit_log WHERE action = ? AND result = ?")
            .bind(action)
            .bind(result)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// The plan's W5 deterministic window-timing test (the writer-level twin
    /// of `rate_limiter.rs`'s unit tests and the real-time storm test in
    /// `tests/engine_integration.rs`): a storm through [`Writer::process`]
    /// proving trip → auto-disarm → manual-re-arm-is-not-enough → window
    /// slide re-opens the budget, with every audit row checked.
    ///
    /// Determinism comes from the same seam the rate limiter itself uses: the
    /// writer takes an explicit `now: Instant`, so the test builds an
    /// `Instant` ladder with plain `Duration` arithmetic - no wall clock in
    /// the assertions at all. (`tokio::time::pause` would NOT help here:
    /// pausing virtualizes `tokio::time::Instant`, but the rate window runs
    /// on injected `std::time::Instant`s, which is strictly more
    /// deterministic - the same reasoning as banto-collect's
    /// `backoff_ladder_advances_virtual_time_deterministically`, adapted to
    /// an injected-clock design.) The broker is a no-network test fake, so
    /// the only I/O is the in-memory audit DB.
    #[tokio::test]
    async fn storm_trips_breaker_and_only_rearm_plus_window_slide_recovers() {
        let pool = init_db_memory().await.expect("in-memory db");
        let arming = Arc::new(ArmingState::new(false));
        arming.arm();
        let (handle, _broker_task) = spawn_test_handle_answering_ok(1);
        let target = ResolvedTarget {
            connection_id: 1,
            address: Address::parse_slmp("D200").expect("valid address"),
            shape: WireShape::Numeric(DataType::U16),
        };
        let mut writer = Writer::new(
            pool.clone(),
            arming.clone(),
            RateLimiter::new(RateLimitConfig {
                window: Duration::from_secs(60),
                global_max: 1000,
                per_connection_max: 2,
            }),
            HashMap::from([(1, handle)]),
            HashMap::from([(1, target)]),
        );

        // Two writes fit the per-connection budget of 2.
        let t0 = Instant::now();
        writer.process(pending(1, 100.0), t0).await.unwrap();
        writer
            .process(pending(2, 101.0), t0 + Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(count(&pool, "rule_fire", "ok").await, 2);
        assert!(arming.is_armed(), "clean writes must not disarm");

        // The third within the window trips the breaker and auto-disarms.
        writer
            .process(pending(3, 102.0), t0 + Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(
            count(&pool, "rate_limit_tripped", "suppressed_rate_limited").await,
            1
        );
        assert!(!arming.is_armed(), "tripping must auto-disarm");

        // Still disarmed: the next intent is suppressed as disarmed (gate #2
        // fires before the rate gate), not tripped again.
        writer
            .process(pending(4, 103.0), t0 + Duration::from_secs(3))
            .await
            .unwrap();
        assert_eq!(count(&pool, "rule_fire", "suppressed_disarmed").await, 1);

        // Manual re-arm alone is NOT enough while the window is still full:
        // the very next intent inside the window trips the breaker again.
        arming.arm();
        writer
            .process(pending(5, 104.0), t0 + Duration::from_secs(30))
            .await
            .unwrap();
        assert_eq!(
            count(&pool, "rate_limit_tripped", "suppressed_rate_limited").await,
            2
        );
        assert!(!arming.is_armed(), "re-tripping must auto-disarm again");

        // Re-arm AND slide the window past both recorded writes (at t0 and
        // t0+1s; both are >= 60s old at t0+62s): the write goes through.
        arming.arm();
        writer
            .process(pending(6, 105.0), t0 + Duration::from_secs(62))
            .await
            .unwrap();
        assert_eq!(count(&pool, "rule_fire", "ok").await, 3);
        assert!(
            arming.is_armed(),
            "a clean write after recovery stays armed"
        );
    }

    /// A STRING write (S2 文字列タグ): the numeric `target_value_written` column
    /// stays NULL and the source/written text lands in the detail JSON.
    #[tokio::test]
    async fn string_write_audits_text_in_detail_and_leaves_numeric_columns_null() {
        let pool = init_db_memory().await.expect("in-memory db");
        let arming = Arc::new(ArmingState::new(false));
        arming.arm();
        let (handle, _broker_task) = spawn_test_handle_answering_ok(1);
        let target = ResolvedTarget {
            connection_id: 1,
            address: Address::parse_slmp("D300").expect("valid address"),
            shape: WireShape::Str { words: 4 },
        };
        let mut writer = Writer::new(
            pool.clone(),
            arming.clone(),
            RateLimiter::new(RateLimitConfig::default()),
            HashMap::from([(1, handle)]),
            HashMap::from([(1, target)]),
        );

        let pending = PendingWrite {
            rule_id: 1,
            rule_name: "SR".to_string(),
            write_target_id: 1,
            source_tag_id: Some(10),
            source_value: None,
            source_text: Some("OK".to_string()),
            value: PlcValue::Str("NG".to_string()),
        };
        writer.process(pending, Instant::now()).await.unwrap();

        assert_eq!(count(&pool, "rule_fire", "ok").await, 1);
        let (written, detail): (Option<f64>, Option<String>) = sqlx::query_as(
            "SELECT target_value_written, detail FROM write_audit_log \
             WHERE action = 'rule_fire' AND result = 'ok'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(written, None, "string write leaves the numeric column NULL");
        let detail = detail.expect("string write records a detail JSON");
        let parsed: serde_json::Value = serde_json::from_str(&detail).unwrap();
        assert_eq!(parsed["sourceText"], "OK");
        assert_eq!(parsed["writtenText"], "NG");
    }
}
