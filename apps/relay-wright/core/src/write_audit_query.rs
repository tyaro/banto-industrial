//! Read-only LIST access to the `write_audit_log` table (plan
//! `luminous-discovering-goblet.md`, W4). The engine's own
//! [`crate::engine::write_audit`] module is the ONLY writer of this table
//! (log-before-write, arm/disarm/dry-run toggles, rate-limit trips); W4 adds
//! the first READER so the monitoring UI can display the write-audit trail.
//!
//! ## Invariants (docs/conventions.md)
//! - §2 (サービス層非依存): `Clone` + `SqlitePool` + `BantoError` only - no
//!   tauri/axum/RBAC/HTTP. Authorization is added by the wiring layer
//!   (`crate::rest` / `src-tauri`); this is a viewer+ read there.
//! - SQL columns are reached ONLY through the [`column_map`] whitelist
//!   (list filter/sort), never string-interpolated from caller input - the
//!   same idiom as [`crate::write_targets`]/[`crate::write_rules`]'s `list`.
//!
//! READ-ONLY by design: there is no create/update/delete here and reading is
//! never audited (reading is not a mutation - same convention as the M14
//! audit-log viewer and every other `list`/`get` in this crate). The rows are
//! fully denormalized snapshots (`rule_name_snapshot`, `source_value_snapshot`,
//! ...) written at fire time, so no joins are needed.

use banto_core::{BantoError, ListParams, ListResult, SortDirection, SortState};
use banto_storage::ColumnMap;
use serde::{Deserialize, Serialize};
use sqlx::{QueryBuilder, Sqlite, SqlitePool};

/// One row of the `write_audit_log` table, wire-shaped (camelCase) for the W4
/// monitoring grid. Every column is a snapshot captured by the engine at the
/// moment the row was written (see [`crate::engine::write_audit`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct WriteAuditLogRow {
    pub id: i64,
    pub ts: String,
    pub write_rule_id: Option<i64>,
    pub rule_name_snapshot: String,
    pub source_tag_id: Option<i64>,
    pub source_value_snapshot: Option<f64>,
    pub write_target_id: Option<i64>,
    pub target_value_written: Option<f64>,
    pub actor_username: Option<String>,
    /// One of `rule_fire`/`arm`/`disarm`/`dry_run_toggle`/`rate_limit_tripped`.
    pub action: String,
    /// One of `ok`/`failed`/`suppressed_disarmed`/`suppressed_rate_limited`/
    /// `suppressed_dry_run`.
    pub result: String,
    pub detail: Option<String>,
}

/// The list filter/sort whitelist: wire field name (camelCase) -> SQL column.
/// Caller-supplied filter/sort fields are only ever honored if they appear
/// here, so no caller input ever reaches SQL as an identifier (invariant §2 /
/// docs/conventions.md).
fn column_map() -> ColumnMap {
    ColumnMap::new()
        .column("id", "id")
        .column("ts", "ts")
        .column("writeRuleId", "write_rule_id")
        .column("ruleNameSnapshot", "rule_name_snapshot")
        .column("sourceTagId", "source_tag_id")
        .column("sourceValueSnapshot", "source_value_snapshot")
        .column("writeTargetId", "write_target_id")
        .column("targetValueWritten", "target_value_written")
        .column("actorUsername", "actor_username")
        .column("action", "action")
        .column("result", "result")
        .column("detail", "detail")
}

const COLUMNS: &str = "id, ts, write_rule_id, rule_name_snapshot, source_tag_id, \
     source_value_snapshot, write_target_id, target_value_written, actor_username, \
     action, result, detail";

/// Read-only service for the `write_audit_log` table (plan W4). Tauri/axum-
/// independent (invariant §2): only `SqlitePool` + `BantoError`. There is no
/// mutating method here on purpose - the engine owns all writes.
#[derive(Clone)]
pub struct WriteAuditLogService {
    pool: SqlitePool,
}

impl WriteAuditLogService {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Filtered/sorted/paginated read, using the exact
    /// `banto_storage::list_query` idiom [`crate::write_targets::WriteTargetService::list`]
    /// uses. When the caller supplies no sort, defaults to newest-first
    /// (`ts` desc) so the operator sees the most recent write activity at the
    /// top without having to ask for it (plan W4).
    pub async fn list(
        &self,
        params: ListParams,
    ) -> Result<ListResult<WriteAuditLogRow>, BantoError> {
        let columns = column_map();

        // Default sort: newest-first. Applied at the service layer (not just
        // the UI) so every caller - REST, Tauri, a future headless client -
        // gets the same sensible default order.
        let params = if params.sort.is_empty() {
            ListParams {
                sort: vec![SortState {
                    field: "ts".to_string(),
                    direction: SortDirection::Desc,
                }],
                ..params
            }
        } else {
            params
        };

        let mut rows_builder: QueryBuilder<Sqlite> =
            QueryBuilder::new(format!("SELECT {COLUMNS} FROM write_audit_log"));
        banto_storage::list_query::sqlite::apply_list_params(&mut rows_builder, &columns, &params)?;
        let rows: Vec<WriteAuditLogRow> = rows_builder
            .build_query_as::<WriteAuditLogRow>()
            .fetch_all(&self.pool)
            .await
            .map_err(banto_storage::storage_error)?;

        let mut count_builder: QueryBuilder<Sqlite> =
            QueryBuilder::new("SELECT COUNT(*) FROM write_audit_log");
        banto_storage::list_query::sqlite::append_where(
            &mut count_builder,
            &columns,
            &params.filters,
        )?;
        let total_count: i64 = count_builder
            .build_query_scalar()
            .fetch_one(&self.pool)
            .await
            .map_err(banto_storage::storage_error)?;

        Ok(ListResult {
            rows,
            total_count: total_count as u64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_db_memory;
    use crate::engine::write_audit::{insert_row, AuditAction, AuditResult, AuditRow};
    use banto_core::{FilterOp, FilterState, Pagination};
    use serde_json::json;

    /// Seed one row directly through the engine's own insert path (the real
    /// writer), so these read tests exercise exactly the shape production rows
    /// have.
    async fn seed(pool: &SqlitePool, row: AuditRow) -> i64 {
        insert_row(pool, &row).await.expect("insert_row")
    }

    #[tokio::test]
    async fn list_empty_is_zero_rows_and_zero_total() {
        let pool = init_db_memory().await.unwrap();
        let svc = WriteAuditLogService::new(pool);
        let result = svc.list(ListParams::default()).await.expect("list");
        assert_eq!(result.total_count, 0);
        assert!(result.rows.is_empty());
    }

    #[tokio::test]
    async fn list_returns_a_seeded_row_with_all_fields() {
        let pool = init_db_memory().await.unwrap();
        seed(
            &pool,
            AuditRow::new(AuditAction::RuleFire, AuditResult::Ok, "R1")
                .with_rule(7)
                .with_source(Some(3), Some(12.5))
                .with_target(4, Some(1.0))
                .with_actor(Some("alice"))
                .with_detail("wrote 1.0"),
        )
        .await;
        let svc = WriteAuditLogService::new(pool);
        let result = svc.list(ListParams::default()).await.expect("list");
        assert_eq!(result.total_count, 1);
        assert_eq!(result.rows.len(), 1);
        let row = &result.rows[0];
        assert_eq!(row.action, "rule_fire");
        assert_eq!(row.result, "ok");
        assert_eq!(row.rule_name_snapshot, "R1");
        assert_eq!(row.write_rule_id, Some(7));
        assert_eq!(row.source_tag_id, Some(3));
        assert_eq!(row.source_value_snapshot, Some(12.5));
        assert_eq!(row.write_target_id, Some(4));
        assert_eq!(row.target_value_written, Some(1.0));
        assert_eq!(row.actor_username.as_deref(), Some("alice"));
        assert_eq!(row.detail.as_deref(), Some("wrote 1.0"));
        assert!(!row.ts.is_empty());
    }

    #[tokio::test]
    async fn list_defaults_to_newest_first() {
        let pool = init_db_memory().await.unwrap();
        // Rows share the same `datetime('now')` second, so a plain `ts desc`
        // could tie; give each an explicit, ordered `ts` so the default-sort
        // assertion is deterministic. `id AUTOINCREMENT` still ascends with
        // insertion order, and `ts desc` here matches that order in reverse.
        for (ts, name) in [
            ("2026-01-01 00:00:01", "oldest"),
            ("2026-01-01 00:00:02", "middle"),
            ("2026-01-01 00:00:03", "newest"),
        ] {
            sqlx::query(
                "INSERT INTO write_audit_log (ts, rule_name_snapshot, action, result) \
                 VALUES (?, ?, 'rule_fire', 'ok')",
            )
            .bind(ts)
            .bind(name)
            .execute(&pool)
            .await
            .unwrap();
        }
        let svc = WriteAuditLogService::new(pool);
        let result = svc.list(ListParams::default()).await.expect("list");
        assert_eq!(result.total_count, 3);
        assert_eq!(result.rows[0].rule_name_snapshot, "newest");
        assert_eq!(result.rows[2].rule_name_snapshot, "oldest");
    }

    #[tokio::test]
    async fn list_filters_sorts_and_paginates_with_total_count() {
        let pool = init_db_memory().await.unwrap();
        // Three ok rows and one suppressed row; filter to ok only, sort by id
        // desc, take the first page of 1.
        for name in ["A", "B", "C"] {
            seed(
                &pool,
                AuditRow::new(AuditAction::RuleFire, AuditResult::Ok, name),
            )
            .await;
        }
        seed(
            &pool,
            AuditRow::new(AuditAction::RuleFire, AuditResult::SuppressedDisarmed, "D"),
        )
        .await;

        let svc = WriteAuditLogService::new(pool);
        let result = svc
            .list(ListParams {
                sort: vec![SortState {
                    field: "id".to_string(),
                    direction: SortDirection::Desc,
                }],
                filters: vec![FilterState {
                    field: "result".to_string(),
                    op: FilterOp::Eq,
                    value: json!("ok"),
                }],
                pagination: Some(Pagination {
                    offset: 0,
                    limit: 1,
                }),
            })
            .await
            .expect("list");
        // total_count reflects the filter (3 ok rows), not the page size.
        assert_eq!(result.total_count, 3);
        assert_eq!(result.rows.len(), 1);
        // Newest ok row by id desc is "C".
        assert_eq!(result.rows[0].rule_name_snapshot, "C");
    }

    #[tokio::test]
    async fn list_filters_by_action_and_actor() {
        let pool = init_db_memory().await.unwrap();
        seed(
            &pool,
            AuditRow::new(AuditAction::Arm, AuditResult::Ok, "arm").with_actor(Some("bob")),
        )
        .await;
        seed(
            &pool,
            AuditRow::new(AuditAction::RuleFire, AuditResult::Ok, "R1"),
        )
        .await;

        let svc = WriteAuditLogService::new(pool);
        let result = svc
            .list(ListParams {
                filters: vec![FilterState {
                    field: "action".to_string(),
                    op: FilterOp::Eq,
                    value: json!("arm"),
                }],
                ..Default::default()
            })
            .await
            .expect("list");
        assert_eq!(result.total_count, 1);
        assert_eq!(result.rows[0].action, "arm");
        assert_eq!(result.rows[0].actor_username.as_deref(), Some("bob"));
    }
}
