//! Write-audit persistence (W3-B safety invariant #5,
//! `luminous-discovering-goblet.md`): the only code that touches the
//! `write_audit_log` and `armed_state` tables. W1 created the schema but left
//! it without any Rust access path; this module is that path.
//!
//! ## Log-before-write
//!
//! A real physical write is recorded in two steps: [`insert_pending_fire`]
//! writes the `rule_fire` row (with a not-yet-final result) BEFORE
//! `broker.write` is called, then [`set_result`] updates that row to `ok` or
//! `failed` after. So the audit trail for an attempted write exists even if the
//! process dies mid-write - a row with a non-terminal result is itself evidence
//! that a write was in flight. Every *suppressed* case (disarmed, rate-limited,
//! dry-run) is a single [`insert_row`] with the matching terminal result and no
//! physical write at all.
//!
//! ## Actor
//!
//! Automatic `rule_fire`/`rate_limit_tripped` rows have no human actor
//! (`actor_username` is `NULL`). `arm`/`disarm`/`dry_run_toggle`/`manual_write`
//! rows carry the username of whoever flipped the switch (or clicked the
//! monitor's value cell), threaded in by the wiring layer.

use banto_core::BantoError;
use sqlx::SqlitePool;

/// `write_audit_log.action` values (mirrors the SQL `CHECK` in
/// `db.rs`/`0008_write_audit_log.sql`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditAction {
    RuleFire,
    Arm,
    Disarm,
    DryRunToggle,
    RateLimitTripped,
    /// A one-shot manual write from the タグモニタ screen
    /// (feature/tag-monitor, `crate::engine::monitor`). Unlike `rule_fire`
    /// it always carries an `actor_username` (a human clicked it) and is
    /// NOT gated by arming/rate-limit/dry-run - this is a debug app and the
    /// user explicitly relaxed those for manual writes; the audit row is the
    /// safety net that remains.
    ManualWrite,
}

impl AuditAction {
    pub fn as_str(self) -> &'static str {
        match self {
            AuditAction::RuleFire => "rule_fire",
            AuditAction::Arm => "arm",
            AuditAction::Disarm => "disarm",
            AuditAction::DryRunToggle => "dry_run_toggle",
            AuditAction::RateLimitTripped => "rate_limit_tripped",
            AuditAction::ManualWrite => "manual_write",
        }
    }
}

/// `write_audit_log.result` values (mirrors the SQL `CHECK`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditResult {
    /// A physical write succeeded, or a non-write action completed.
    Ok,
    /// A physical write was attempted but the broker returned an error.
    Failed,
    /// Suppressed because the engine was disarmed.
    SuppressedDisarmed,
    /// Suppressed because the rate limiter/breaker tripped.
    SuppressedRateLimited,
    /// Suppressed because the engine was in dry-run.
    SuppressedDryRun,
}

impl AuditResult {
    pub fn as_str(self) -> &'static str {
        match self {
            AuditResult::Ok => "ok",
            AuditResult::Failed => "failed",
            AuditResult::SuppressedDisarmed => "suppressed_disarmed",
            AuditResult::SuppressedRateLimited => "suppressed_rate_limited",
            AuditResult::SuppressedDryRun => "suppressed_dry_run",
        }
    }
}

/// The fields of one `write_audit_log` row (the auto-populated `id`/`ts` aside).
/// Built with [`AuditRow::action`] and the `with_*` setters.
#[derive(Debug, Clone)]
pub struct AuditRow {
    pub action: AuditAction,
    pub result: AuditResult,
    pub write_rule_id: Option<i64>,
    pub rule_name_snapshot: String,
    pub source_tag_id: Option<i64>,
    pub source_value_snapshot: Option<f64>,
    pub write_target_id: Option<i64>,
    pub target_value_written: Option<f64>,
    pub actor_username: Option<String>,
    pub detail: Option<String>,
}

impl AuditRow {
    /// Start a row for `action`/`result`. `rule_name_snapshot` is NOT NULL in
    /// the schema; non-rule actions pass a short label (e.g. the action name).
    pub fn new(
        action: AuditAction,
        result: AuditResult,
        rule_name_snapshot: impl Into<String>,
    ) -> Self {
        Self {
            action,
            result,
            write_rule_id: None,
            rule_name_snapshot: rule_name_snapshot.into(),
            source_tag_id: None,
            source_value_snapshot: None,
            write_target_id: None,
            target_value_written: None,
            actor_username: None,
            detail: None,
        }
    }

    pub fn with_rule(mut self, rule_id: i64) -> Self {
        self.write_rule_id = Some(rule_id);
        self
    }

    pub fn with_source(mut self, tag_id: Option<i64>, value: Option<f64>) -> Self {
        self.source_tag_id = tag_id;
        self.source_value_snapshot = value;
        self
    }

    pub fn with_target(mut self, target_id: i64, value: Option<f64>) -> Self {
        self.write_target_id = Some(target_id);
        self.target_value_written = value;
        self
    }

    pub fn with_actor(mut self, actor: Option<&str>) -> Self {
        self.actor_username = actor.map(str::to_string);
        self
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// Insert one fully-formed audit row, returning its new `id`. Used directly for
/// every terminal-result row (all suppressed cases, `rate_limit_tripped`, and
/// the arm/disarm/dry-run toggles).
pub async fn insert_row(pool: &SqlitePool, row: &AuditRow) -> Result<i64, BantoError> {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO write_audit_log (\
            write_rule_id, rule_name_snapshot, source_tag_id, source_value_snapshot, \
            write_target_id, target_value_written, actor_username, action, result, detail\
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(row.write_rule_id)
    .bind(&row.rule_name_snapshot)
    .bind(row.source_tag_id)
    .bind(row.source_value_snapshot)
    .bind(row.write_target_id)
    .bind(row.target_value_written)
    .bind(&row.actor_username)
    .bind(row.action.as_str())
    .bind(row.result.as_str())
    .bind(&row.detail)
    .fetch_one(pool)
    .await
    .map_err(banto_storage::storage_error)?;
    Ok(id)
}

/// Insert the `rule_fire` row that PRECEDES a physical write (log-before-write).
/// The row starts with `result = 'failed'` as its provisional state: if the
/// process dies between this insert and [`set_result`], the row is left saying
/// "a write was attempted and we never confirmed success", which is the safe
/// interpretation. On success the caller flips it to `ok` via [`set_result`].
pub async fn insert_pending_fire(pool: &SqlitePool, row: &AuditRow) -> Result<i64, BantoError> {
    debug_assert_eq!(row.action, AuditAction::RuleFire);
    let pending = AuditRow {
        result: AuditResult::Failed,
        ..row.clone()
    };
    insert_row(pool, &pending).await
}

/// Update a previously [`insert_pending_fire`]d row's `result` (to `ok` after a
/// confirmed write, or leave/refresh as `failed`).
pub async fn set_result(
    pool: &SqlitePool,
    audit_id: i64,
    result: AuditResult,
) -> Result<(), BantoError> {
    sqlx::query("UPDATE write_audit_log SET result = ? WHERE id = ?")
        .bind(result.as_str())
        .bind(audit_id)
        .execute(pool)
        .await
        .map_err(banto_storage::storage_error)?;
    Ok(())
}

/// Read the persisted `armed_state.armed_persisted` bit (for informational
/// startup history only - see [`crate::engine::arming::ArmingState`]). The row
/// is seeded once by `db.rs`, so this always finds exactly one.
pub async fn load_persisted_armed(pool: &SqlitePool) -> Result<bool, BantoError> {
    let armed: i64 = sqlx::query_scalar("SELECT armed_persisted FROM armed_state WHERE id = 1")
        .fetch_one(pool)
        .await
        .map_err(banto_storage::storage_error)?;
    Ok(armed != 0)
}

/// Persist a new armed value (with who changed it and when) into the single
/// `armed_state` row. This is history/UI only; it does NOT resume live writing
/// on the next start (the live flag always constructs to `false`).
pub async fn persist_armed(
    pool: &SqlitePool,
    armed: bool,
    actor: Option<&str>,
) -> Result<(), BantoError> {
    sqlx::query(
        "UPDATE armed_state \
         SET armed_persisted = ?, last_changed_at = datetime('now'), last_changed_by = ? \
         WHERE id = 1",
    )
    .bind(armed as i64)
    .bind(actor)
    .execute(pool)
    .await
    .map_err(banto_storage::storage_error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_db_memory;

    #[tokio::test]
    async fn insert_and_read_back_a_suppressed_row() {
        let pool = init_db_memory().await.unwrap();
        let row = AuditRow::new(AuditAction::RuleFire, AuditResult::SuppressedDisarmed, "R1")
            .with_rule(7)
            .with_source(Some(3), Some(12.5))
            .with_target(4, Some(1.0))
            .with_detail("disarmed");
        let id = insert_row(&pool, &row).await.unwrap();
        let (action, result, rule): (String, String, String) = sqlx::query_as(
            "SELECT action, result, rule_name_snapshot FROM write_audit_log WHERE id = ?",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(action, "rule_fire");
        assert_eq!(result, "suppressed_disarmed");
        assert_eq!(rule, "R1");
    }

    #[tokio::test]
    async fn pending_fire_starts_failed_then_flips_ok() {
        let pool = init_db_memory().await.unwrap();
        let row = AuditRow::new(AuditAction::RuleFire, AuditResult::Ok, "R1").with_rule(1);
        let id = insert_pending_fire(&pool, &row).await.unwrap();
        let before: String = sqlx::query_scalar("SELECT result FROM write_audit_log WHERE id = ?")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(before, "failed", "pending row is provisionally failed");

        set_result(&pool, id, AuditResult::Ok).await.unwrap();
        let after: String = sqlx::query_scalar("SELECT result FROM write_audit_log WHERE id = ?")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(after, "ok");
    }

    #[tokio::test]
    async fn armed_state_persists_but_load_is_informational() {
        let pool = init_db_memory().await.unwrap();
        assert!(
            !load_persisted_armed(&pool).await.unwrap(),
            "seeded disarmed"
        );
        persist_armed(&pool, true, Some("alice")).await.unwrap();
        assert!(load_persisted_armed(&pool).await.unwrap());
        let by: Option<String> =
            sqlx::query_scalar("SELECT last_changed_by FROM armed_state WHERE id = 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(by.as_deref(), Some("alice"));
    }
}
