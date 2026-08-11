//! Audit trail service, copied near-verbatim from
//! `apps/chronogazer/core/src/audit.rs`: who did what, when, and whether it
//! was allowed. Backed by the `audit_log` table (`db.rs::apply_app_schema`),
//! same service-layer pattern as [`crate::users::UsersService`] - testable in
//! a plain `cargo test`, no `axum` dependency.
//!
//! **This service does not know about actors, RBAC, or HTTP** - it only
//! knows how to store/list/prune rows. Every REST handler that mutates state
//! (or gets rejected by an RBAC guard) is responsible for building an
//! [`AuditEntry`] itself and calling [`AuditLogService::record`] - see
//! `crate::rest`'s `require_role_at_least`/`require_editor` call sites for
//! where the actor (`Identity`) is known. `origin` is always `"rest"` here
//! (banto-hub has no Tauri command surface, unlike chronogazer/relay-wright).
//!
//! SECURITY: [`AuditEntry::detail`] is a JSON **summary** (spec: "値の全量は
//! 入れない") - changed field NAMES, a new role, a method+path, etc. Nothing
//! that calls into this module may ever put a password, password hash, or
//! bearer token into `detail`. There is no runtime guard against this (the
//! type is a free-form `serde_json::Value`) - it is enforced by review at
//! every call site instead.

use banto_core::{BantoError, ListParams, ListResult};
use banto_storage::ColumnMap;
use serde::Serialize;
use sqlx::{QueryBuilder, Sqlite, SqlitePool};

/// One row of the `audit_log` table, wire-shaped for the audit-log viewer
/// (spec M14's admin-only grid). `detail` is the raw JSON-encoded summary
/// string as stored (not re-parsed into a `Value`) - the frontend grid can
/// `JSON.parse` it on demand for display, mirroring how `detail` is written
/// (see [`AuditLogService::try_record`]).
#[derive(Debug, Clone, PartialEq, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct AuditLogEntry {
    pub id: i64,
    pub ts: String,
    pub actor_username: Option<String>,
    pub actor_role: Option<String>,
    pub action: String,
    pub resource: String,
    pub entity_id: Option<String>,
    pub detail: Option<String>,
    pub origin: String,
    pub result: String,
}

/// One record to write to the `audit_log` table (spec M14). Borrowed string
/// fields keep call sites cheap - this is built fresh at each call site and
/// consumed immediately by [`AuditLogService::record`]/[`AuditLogService::try_record`],
/// never stored.
///
/// See this module's doc comment for the hard rule on what `detail` may
/// carry.
#[derive(Debug, Clone)]
pub struct AuditEntry<'a> {
    /// Username snapshot of the actor, or `None` for an unauthenticated
    /// event (e.g. a login failure before any session exists).
    pub actor_username: Option<&'a str>,
    /// The actor's role AT THE TIME of the action (spec: not looked up
    /// later - a role change afterward must not rewrite history).
    pub actor_role: Option<&'a str>,
    /// e.g. `"create"`, `"update"`, `"delete"`, `"login"`, `"login_failed"`,
    /// `"logout"`, `"setup"`, `"password_reset"`, `"settings_change"`,
    /// `"denied"`.
    pub action: &'a str,
    /// e.g. `"items"`, `"users"`, `"settings"`, `"auth"`.
    pub resource: &'a str,
    pub entity_id: Option<&'a str>,
    pub detail: Option<serde_json::Value>,
    /// Always `"rest"` for banto-hub (no Tauri command surface, unlike
    /// chronogazer/relay-wright, which also record `"tauri"` here) - kept as
    /// a plain `&str` rather than a fixed enum so this module stays
    /// transport-agnostic, matching the copied-from crate's shape.
    pub origin: &'a str,
    /// `"ok"`, `"denied"`, or `"failed"`.
    pub result: &'a str,
}

/// Column whitelist for [`AuditLogService::list`] (spec M14, mirrors
/// `crate::items::column_map`): wire field name (camelCase, as sent by the
/// audit-log viewer) -> actual `audit_log` SQL column.
fn column_map() -> ColumnMap {
    ColumnMap::new()
        .column("id", "id")
        .column("ts", "ts")
        .column("actorUsername", "actor_username")
        .column("actorRole", "actor_role")
        .column("action", "action")
        .column("resource", "resource")
        .column("entityId", "entity_id")
        .column("detail", "detail")
        .column("origin", "origin")
        .column("result", "result")
}

/// Audit trail service (spec M14): append-only writes, a filtered/sorted/
/// paginated read (admin-only viewer), and retention-based pruning.
///
/// `Clone` is cheap (`SqlitePool` is an `Arc`-backed handle), matching
/// `UsersService`/`SettingsService`.
#[derive(Clone)]
pub struct AuditLogService {
    pool: SqlitePool,
}

impl AuditLogService {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Write one audit entry. `Result`-returning (unlike
    /// [`AuditLogService::record`]) so unit tests can assert on failure
    /// modes; every `crate::rest` call site should use `record` instead,
    /// which never fails the caller's real operation over an audit-write
    /// hiccup.
    pub async fn try_record(&self, entry: AuditEntry<'_>) -> Result<(), BantoError> {
        let detail = entry
            .detail
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|err| {
                BantoError::Other(format!("監査ログのdetailシリアライズに失敗しました: {err}"))
            })?;

        sqlx::query(
            "INSERT INTO audit_log (actor_username, actor_role, action, resource, entity_id, detail, origin, result) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(entry.actor_username)
        .bind(entry.actor_role)
        .bind(entry.action)
        .bind(entry.resource)
        .bind(entry.entity_id)
        .bind(detail)
        .bind(entry.origin)
        .bind(entry.result)
        .execute(&self.pool)
        .await
        .map_err(banto_storage::storage_error)?;
        Ok(())
    }

    /// Fire-and-forget wrapper around [`AuditLogService::try_record`]: a
    /// failure to WRITE an audit entry must never fail the operation being
    /// audited (e.g. a `tags.create` that otherwise succeeded) - it is only
    /// logged as a warning (`eprintln`; this workspace has no `tracing`
    /// dependency, see the root `Cargo.toml`). Every `crate::rest` handler
    /// calls this, not `try_record`, directly.
    pub async fn record(&self, entry: AuditEntry<'_>) {
        let action = entry.action.to_string();
        let resource = entry.resource.to_string();
        if let Err(err) = self.try_record(entry).await {
            eprintln!(
                "banto: 監査ログの記録に失敗しました（action={action}, resource={resource}）: {err}"
            );
        }
    }

    /// Filtered/sorted/paginated read (spec M14's admin-only viewer), using
    /// the same `banto_storage::list_query` pattern every other listable
    /// service in this crate does. Deliberately called only from the
    /// admin-gated `/api/audit-log/list` route / `audit_log_list` command -
    /// this service itself has no RBAC awareness (see this module's doc
    /// comment).
    pub async fn list(&self, params: ListParams) -> Result<ListResult<AuditLogEntry>, BantoError> {
        let columns = column_map();

        let mut rows_builder: QueryBuilder<'_, Sqlite> = QueryBuilder::new(
            "SELECT id, ts, actor_username, actor_role, action, resource, entity_id, detail, origin, result \
             FROM audit_log",
        );
        banto_storage::list_query::sqlite::apply_list_params(&mut rows_builder, &columns, &params)?;
        let rows: Vec<AuditLogEntry> = rows_builder
            .build_query_as::<AuditLogEntry>()
            .fetch_all(&self.pool)
            .await
            .map_err(banto_storage::storage_error)?;

        let mut count_builder: QueryBuilder<'_, Sqlite> =
            QueryBuilder::new("SELECT COUNT(*) FROM audit_log");
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

    /// Retention-based pruning: delete rows older than `retention_days` (if
    /// `Some`), then - separately - delete the oldest rows beyond
    /// `retention_rows` (if `Some`), oldest-first by `id` (`AUTOINCREMENT`
    /// guarantees `id` order matches insertion order, which is a more
    /// reliable "oldest" tiebreak than `ts` alone since several rows can
    /// share the same second). `None` means unlimited for that dimension;
    /// a non-positive value is also treated defensively as "skip this
    /// dimension" (mirrors chronogazer's `AuditSettings` convention this
    /// method was copied alongside).
    ///
    /// docs/banto-hub-remaining-plan.md P3-a: wired up the same way
    /// chronogazer/relay-wright already do - the settings come from
    /// [`crate::settings::SettingsService::audit_config`], and this method
    /// is called from two places, matching the reference crates' "no
    /// separate background pruning task" reasoning (see their
    /// `crate::rest::audit_log_list` doc comment):
    /// - once at startup (`crate::runtime::HubRuntime::start`), and
    /// - opportunistically before every `POST /api/audit-log/list`
    ///   (`crate::rest::audit_log_list`) - best-effort, so a prune failure
    ///   never blocks an admin from viewing existing entries.
    ///
    /// Returns the total number of rows deleted.
    pub async fn prune(
        &self,
        retention_days: Option<i64>,
        retention_rows: Option<i64>,
    ) -> Result<u64, BantoError> {
        let mut deleted: u64 = 0;

        if let Some(days) = retention_days.filter(|d| *d > 0) {
            let result = sqlx::query(
                "DELETE FROM audit_log WHERE ts < datetime('now', '-' || ? || ' days')",
            )
            .bind(days)
            .execute(&self.pool)
            .await
            .map_err(banto_storage::storage_error)?;
            deleted += result.rows_affected();
        }

        if let Some(max_rows) = retention_rows.filter(|r| *r > 0) {
            let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
                .fetch_one(&self.pool)
                .await
                .map_err(banto_storage::storage_error)?;
            let excess = total - max_rows;
            if excess > 0 {
                let result = sqlx::query(
                    "DELETE FROM audit_log WHERE id IN \
                     (SELECT id FROM audit_log ORDER BY id ASC LIMIT ?)",
                )
                .bind(excess)
                .execute(&self.pool)
                .await
                .map_err(banto_storage::storage_error)?;
                deleted += result.rows_affected();
            }
        }

        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrate_memory;
    use banto_core::{FilterOp, FilterState, Pagination, SortDirection, SortState};
    use serde_json::json;

    async fn service() -> AuditLogService {
        let pool = migrate_memory().await.expect("migrate_memory");
        AuditLogService::new(pool)
    }

    fn sample_entry<'a>(action: &'a str, resource: &'a str, actor: &'a str) -> AuditEntry<'a> {
        AuditEntry {
            actor_username: Some(actor),
            actor_role: Some("admin"),
            action,
            resource,
            entity_id: Some("1"),
            detail: Some(json!({ "name": "Widget" })),
            origin: "rest",
            result: "ok",
        }
    }

    #[tokio::test]
    async fn record_then_list_round_trips() {
        let svc = service().await;
        svc.try_record(sample_entry("create", "items", "admin"))
            .await
            .expect("try_record should succeed");

        let result = svc
            .list(ListParams::default())
            .await
            .expect("list should succeed");
        assert_eq!(result.total_count, 1);
        assert_eq!(result.rows.len(), 1);
        let row = &result.rows[0];
        assert_eq!(row.actor_username.as_deref(), Some("admin"));
        assert_eq!(row.actor_role.as_deref(), Some("admin"));
        assert_eq!(row.action, "create");
        assert_eq!(row.resource, "items");
        assert_eq!(row.entity_id.as_deref(), Some("1"));
        assert_eq!(row.origin, "rest");
        assert_eq!(row.result, "ok");
        let detail: serde_json::Value =
            serde_json::from_str(row.detail.as_deref().expect("detail should be set")).unwrap();
        assert_eq!(detail, json!({ "name": "Widget" }));
        assert!(!row.ts.is_empty());
    }

    #[tokio::test]
    async fn record_with_no_actor_stores_null_actor_columns() {
        let svc = service().await;
        svc.try_record(AuditEntry {
            actor_username: None,
            actor_role: None,
            action: "login_failed",
            resource: "auth",
            entity_id: None,
            detail: None,
            origin: "rest",
            result: "failed",
        })
        .await
        .expect("try_record should succeed");

        let result = svc.list(ListParams::default()).await.unwrap();
        assert_eq!(result.rows[0].actor_username, None);
        assert_eq!(result.rows[0].actor_role, None);
        assert_eq!(result.rows[0].detail, None);
    }

    /// `record` (the fire-and-forget entry point every `crate::rest` call
    /// site actually uses) must not panic and must still persist a valid entry -
    /// this is the "happy path" proof that it delegates to `try_record`
    /// correctly, since its failure-swallowing behavior itself can't be
    /// observed without a broken pool (not exercised here).
    #[tokio::test]
    async fn record_persists_like_try_record() {
        let svc = service().await;
        svc.record(sample_entry("delete", "users", "owner")).await;
        let result = svc.list(ListParams::default()).await.unwrap();
        assert_eq!(result.total_count, 1);
        assert_eq!(result.rows[0].action, "delete");
    }

    #[tokio::test]
    async fn list_filters_by_resource_and_action() {
        let svc = service().await;
        svc.record(sample_entry("create", "items", "admin")).await;
        svc.record(sample_entry("create", "users", "admin")).await;
        svc.record(sample_entry("delete", "items", "admin")).await;

        let result = svc
            .list(ListParams {
                filters: vec![
                    FilterState {
                        field: "resource".to_string(),
                        op: FilterOp::Eq,
                        value: json!("items"),
                    },
                    FilterState {
                        field: "action".to_string(),
                        op: FilterOp::Eq,
                        value: json!("create"),
                    },
                ],
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(result.total_count, 1);
        assert_eq!(result.rows[0].resource, "items");
        assert_eq!(result.rows[0].action, "create");
    }

    #[tokio::test]
    async fn list_filters_by_actor_username() {
        let svc = service().await;
        svc.record(sample_entry("create", "items", "alice")).await;
        svc.record(sample_entry("create", "items", "bob")).await;

        let result = svc
            .list(ListParams {
                filters: vec![FilterState {
                    field: "actorUsername".to_string(),
                    op: FilterOp::Eq,
                    value: json!("bob"),
                }],
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(result.total_count, 1);
        assert_eq!(result.rows[0].actor_username.as_deref(), Some("bob"));
    }

    #[tokio::test]
    async fn list_sorts_and_paginates() {
        let svc = service().await;
        for action in ["a", "b", "c"] {
            svc.record(sample_entry(action, "items", "admin")).await;
        }

        let result = svc
            .list(ListParams {
                sort: vec![SortState {
                    field: "id".to_string(),
                    direction: SortDirection::Desc,
                }],
                pagination: Some(Pagination {
                    offset: 0,
                    limit: 1,
                }),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(result.total_count, 3);
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0].action, "c"); // most recently inserted first
    }

    // --- prune (spec M14) ---------------------------------------------------

    async fn seed_n(svc: &AuditLogService, n: usize) {
        for i in 0..n {
            svc.record(sample_entry("create", "items", "admin")).await;
            let _ = i;
        }
    }

    #[tokio::test]
    async fn prune_with_both_none_is_a_no_op() {
        let svc = service().await;
        seed_n(&svc, 5).await;
        let deleted = svc.prune(None, None).await.unwrap();
        assert_eq!(deleted, 0);
        assert_eq!(
            svc.list(ListParams::default()).await.unwrap().total_count,
            5
        );
    }

    #[tokio::test]
    async fn prune_by_row_count_keeps_the_newest_rows() {
        let svc = service().await;
        seed_n(&svc, 5).await;

        let deleted = svc.prune(None, Some(2)).await.unwrap();
        assert_eq!(deleted, 3);

        let remaining = svc.list(ListParams::default()).await.unwrap();
        assert_eq!(remaining.total_count, 2);
        // The two highest ids (most recently inserted) must survive.
        let mut ids: Vec<i64> = remaining.rows.iter().map(|r| r.id).collect();
        ids.sort();
        assert_eq!(ids, vec![4, 5]);
    }

    #[tokio::test]
    async fn prune_by_row_count_is_a_no_op_when_under_the_limit() {
        let svc = service().await;
        seed_n(&svc, 3).await;
        let deleted = svc.prune(None, Some(100)).await.unwrap();
        assert_eq!(deleted, 0);
        assert_eq!(
            svc.list(ListParams::default()).await.unwrap().total_count,
            3
        );
    }

    #[tokio::test]
    async fn prune_by_days_deletes_rows_older_than_the_cutoff() {
        let svc = service().await;
        seed_n(&svc, 2).await;
        // Directly backdate one row's `ts` past a 1-day retention window -
        // there is no clock injection in this service (unlike
        // `banto_server::auth`'s `AuthState`), so this is the simplest way
        // to exercise the days branch deterministically.
        sqlx::query("UPDATE audit_log SET ts = datetime('now', '-10 days') WHERE id = 1")
            .execute(&svc.pool)
            .await
            .unwrap();

        let deleted = svc.prune(Some(1), None).await.unwrap();
        assert_eq!(deleted, 1);
        let remaining = svc.list(ListParams::default()).await.unwrap();
        assert_eq!(remaining.total_count, 1);
        assert_eq!(remaining.rows[0].id, 2);
    }

    #[tokio::test]
    async fn prune_treats_non_positive_values_as_unlimited() {
        let svc = service().await;
        seed_n(&svc, 5).await;
        let deleted = svc.prune(Some(0), Some(-1)).await.unwrap();
        assert_eq!(deleted, 0);
        assert_eq!(
            svc.list(ListParams::default()).await.unwrap().total_count,
            5
        );
    }
}
