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

use banto_core::{BantoError, ListParams, SortDirection, SortState};
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

/// 監査ログ一覧の 1 ページ分の応答（#428。chronogazer の #410 と同じ形）。
/// `rows` / `totalCount` は `banto_core::ListResult` と**同じ綴り**で、そこに
/// **この応答が使ったスナップショット境界** `asOfId` を 1 つ足したもの
/// （`ListResult` は `banto-core` の型でフィールドを足せないので、同じ綴りの
/// 型をここに置く）。フィールドを足しただけなので、`rows`/`totalCount` しか
/// 読まない既存の呼び出し元はそのまま動く。
///
/// * `total_count` も `rows` も **`id <= as_of_id` で絞った集合**から取る。
/// * **表が空のときの `as_of_id` は `0`**（`id` は 1 から振られるので「どの行も
///   含まない境界」。`null` にすると「境界を決めさせる」と区別できない）。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditLogList {
    pub rows: Vec<AuditLogEntry>,
    pub total_count: u64,
    pub as_of_id: i64,
}

/// 並べ替えの最後に `id` を足して、並びを一意にする（#428。
/// [`AuditLogService::list_as_of`] の doc）。すでに `id` で並べているなら
/// 何もしない。向きは最後の（列として解決できる）並べ替えに揃える - 並べ替えが
/// 無ければ `Desc`（新しい順）。
fn with_id_tiebreaker(mut params: ListParams) -> ListParams {
    if params.sort.iter().any(|s| s.field == "id") {
        return params;
    }
    let columns = column_map();
    let direction = params
        .sort
        .iter()
        .rev()
        .find(|s| columns.resolve(&s.field).is_some())
        .map(|s| s.direction)
        .unwrap_or(SortDirection::Desc);
    params.sort.push(SortState {
        field: "id".to_string(),
        direction,
    });
    params
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

    /// Filtered/sorted/paginated read (spec M14's admin-only viewer) with no
    /// snapshot boundary - same as [`AuditLogService::list_as_of`] with
    /// `as_of_id: None` (the boundary it picks is the newest row, so the
    /// result covers the whole table exactly as before #428). Kept so the
    /// many existing callers (tests, other services) stay unchanged.
    pub async fn list(&self, params: ListParams) -> Result<AuditLogList, BantoError> {
        self.list_as_of(params, None).await
    }

    /// Filtered/sorted/paginated read (spec M14's admin-only viewer), using
    /// the same `banto_storage::list_query` pattern every other listable
    /// service in this crate does. Deliberately called only from the
    /// admin-gated `/api/audit-log/list` route - this service itself has no
    /// RBAC awareness (see this module's doc comment).
    ///
    /// **`as_of_id` = スナップショット境界**（#428。chronogazer の #410 と
    /// 同じ直し方）: 指定すると `id <= as_of_id` の行**だけ**を数え、並べる。
    /// 未指定なら**この読み取りの時点の最大 `id`**（表が空なら `0`）を境界に
    /// する - 全行を対象にするのと同じなので、省略した呼び出し元の結果は
    /// 変わらない。使った境界は [`AuditLogList::as_of_id`] で返す。画面は
    /// 世代の最初の応答で返ってきた境界を固定し、同じ世代の後続ブロックに
    /// 渡す - ブロック取得の合間に行が**足されても**、同じ集合から取れる。
    /// `audit_log.id` は `INTEGER PRIMARY KEY AUTOINCREMENT`
    /// （`crate::db` の `apply_app_schema`）なので単調増加かつ**削除後も
    /// 再利用されない**。
    ///
    /// **削除には効かない**: 保持期間の [`AuditLogService::prune`] が集合の
    /// 行を消すと、同じ境界でも `OFFSET` がずれうる。それは画面側が「同じ境界・
    /// 同じ条件の総件数が世代の最初と変わった」ことで検出して採らない
    /// （行は書き換えられず、境界より後の行は集合に入らないので、件数が変わる
    /// のは削除だけ）。
    ///
    /// **境界の決定・行・件数を 1 つの読み取りトランザクションで行う**:
    /// 1 回の応答の中で行と件数の基準時点がずれない（件数の比較が上の検出の
    /// 根拠なので、ここがずれると誤検出・見逃しになる）。
    ///
    /// **並びは必ず `id` で一意にする**（[`with_id_tiebreaker`]）: `ts` は
    /// 秒単位で同じ値の行が並びうるので、`ts DESC` だけでは同順位の行の順序が
    /// SQL として保証されない（#428 の再現: 絞り込みで `actor_username` の
    /// 索引が選ばれると、同じ秒の行が `ts DESC` でも古い順に並んだ）。
    pub async fn list_as_of(
        &self,
        params: ListParams,
        as_of_id: Option<i64>,
    ) -> Result<AuditLogList, BantoError> {
        let columns = column_map();
        let params = with_id_tiebreaker(params);

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(banto_storage::storage_error)?;

        let as_of_id = match as_of_id {
            Some(id) => id,
            None => sqlx::query_scalar::<_, Option<i64>>("SELECT MAX(id) FROM audit_log")
                .fetch_one(&mut *tx)
                .await
                .map_err(banto_storage::storage_error)?
                .unwrap_or(0),
        };

        // 境界で絞った集合を `audit_log` という名前の副問い合わせにして、
        // 列の許可リスト（`column_map`）と `apply_list_params` をそのまま使う
        // （`append_where` は自分で ` WHERE ` を書くので、外側に条件を足せない）。
        let mut rows_builder: QueryBuilder<Sqlite> = QueryBuilder::new(
            "SELECT id, ts, actor_username, actor_role, action, resource, entity_id, detail, origin, result \
             FROM (SELECT * FROM audit_log WHERE id <= ",
        );
        rows_builder.push_bind(as_of_id);
        rows_builder.push(") AS audit_log");
        banto_storage::list_query::sqlite::apply_list_params(&mut rows_builder, &columns, &params)?;
        let rows: Vec<AuditLogEntry> = rows_builder
            .build_query_as::<AuditLogEntry>()
            .fetch_all(&mut *tx)
            .await
            .map_err(banto_storage::storage_error)?;

        let mut count_builder: QueryBuilder<Sqlite> =
            QueryBuilder::new("SELECT COUNT(*) FROM (SELECT * FROM audit_log WHERE id <= ");
        count_builder.push_bind(as_of_id);
        count_builder.push(") AS audit_log");
        banto_storage::list_query::sqlite::append_where(
            &mut count_builder,
            &columns,
            &params.filters,
        )?;
        let total_count: i64 = count_builder
            .build_query_scalar()
            .fetch_one(&mut *tx)
            .await
            .map_err(banto_storage::storage_error)?;

        tx.commit().await.map_err(banto_storage::storage_error)?;

        Ok(AuditLogList {
            rows,
            total_count: total_count as u64,
            as_of_id,
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
    /// docs/banto-hub-remaining-plan.md P3-a: the settings come from
    /// [`crate::settings::SettingsService::audit_config`], and this method
    /// is called from three places:
    /// - once at startup (`crate::runtime::HubRuntime::start`),
    /// - every tick of `HubRuntime::start`'s existing 24h tstore-retention
    ///   loop (P3-a 追補、2026-08-12: unlike chronogazer/relay-wright,
    ///   which are desktop apps a human restarts often and so get away with
    ///   "startup + opportunistic-on-list only", banto-hub is a 24/7
    ///   headless collector - a deployment that never opens the audit-log
    ///   viewer and never restarts would otherwise never prune at all. See
    ///   `crate::runtime::audit_prune_once`'s doc comment), and
    /// - opportunistically before every `POST /api/audit-log/list`
    ///   (`crate::rest::audit_log_list`) - best-effort, so a prune failure
    ///   never blocks an admin from viewing existing entries. Kept even
    ///   though the 24h loop makes it non-load-bearing, for low-latency
    ///   effect when an admin changes the policy and reopens the viewer.
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

    // --- スナップショット境界と並びの一意性（#428） -------------------------

    fn page(offset: u64, limit: u64, sort: Vec<SortState>) -> ListParams {
        ListParams {
            sort,
            pagination: Some(Pagination { offset, limit }),
            ..Default::default()
        }
    }

    fn ts_desc() -> Vec<SortState> {
        vec![SortState {
            field: "ts".to_string(),
            direction: SortDirection::Desc,
        }]
    }

    fn ids(list: &AuditLogList) -> Vec<i64> {
        list.rows.iter().map(|r| r.id).collect()
    }

    /// 境界を固定すると、ブロックの合間に足された行は件数にも行にも入らず、
    /// 境界で重複せず末尾まで辿れる（#428 の欠陥 2 の backend 側）。
    #[tokio::test]
    async fn list_as_of_pins_the_set_across_blocks() {
        let svc = service().await;
        seed_n(&svc, 5).await;

        let first = svc.list_as_of(page(0, 2, ts_desc()), None).await.unwrap();
        assert_eq!(first.as_of_id, 5);
        assert_eq!(first.total_count, 5);
        assert_eq!(ids(&first), vec![5, 4]);

        seed_n(&svc, 2).await; // id 6, 7 - 境界より後

        let second = svc
            .list_as_of(page(2, 2, ts_desc()), Some(first.as_of_id))
            .await
            .unwrap();
        let third = svc
            .list_as_of(page(4, 2, ts_desc()), Some(first.as_of_id))
            .await
            .unwrap();
        assert_eq!(second.as_of_id, 5, "渡した境界をそのまま返すこと");
        assert_eq!(second.total_count, 5);
        assert_eq!(ids(&second), vec![3, 2]);
        assert_eq!(ids(&third), vec![1], "末尾まで辿れる（欠落なし）");

        // 省略すると従来どおり全行（新しい行もここで入る）。
        let fresh = svc.list(page(0, 10, ts_desc())).await.unwrap();
        assert_eq!(fresh.as_of_id, 7);
        assert_eq!(fresh.total_count, 7);
        assert_eq!(ids(&fresh), vec![7, 6, 5, 4, 3, 2, 1]);
    }

    /// 表が空のときの境界は `0`（どの行も含まない）。`0` を渡せば同じ空集合。
    #[tokio::test]
    async fn list_as_of_on_an_empty_table_is_zero() {
        let svc = service().await;
        let empty = svc.list(ListParams::default()).await.unwrap();
        assert_eq!(empty.as_of_id, 0);
        assert_eq!(empty.total_count, 0);
        seed_n(&svc, 2).await;
        let pinned = svc
            .list_as_of(ListParams::default(), Some(0))
            .await
            .unwrap();
        assert_eq!(pinned.total_count, 0);
        assert!(pinned.rows.is_empty());
    }

    /// 絞り込みと境界は両方効く（件数も同じ条件で数える）。
    #[tokio::test]
    async fn list_as_of_combines_with_filters() {
        let svc = service().await;
        svc.record(sample_entry("create", "items", "alice")).await; // 1
        svc.record(sample_entry("create", "items", "bob")).await; // 2
        svc.record(sample_entry("create", "items", "alice")).await; // 3
        svc.record(sample_entry("create", "items", "alice")).await; // 4 - 境界の後
        let result = svc
            .list_as_of(
                ListParams {
                    filters: vec![FilterState {
                        field: "actorUsername".to_string(),
                        op: FilterOp::Eq,
                        value: json!("alice"),
                    }],
                    ..Default::default()
                },
                Some(3),
            )
            .await
            .unwrap();
        assert_eq!(result.total_count, 2);
        assert_eq!(ids(&result), vec![3, 1]);
    }

    /// 保持期間の削除は境界に効かない - **同じ境界の件数が減る**ことで画面が
    /// 検出する（`auditBlocks.ts` のスナップショット失効）。この前提を固定する。
    #[tokio::test]
    async fn prune_between_blocks_shrinks_the_pinned_count() {
        let svc = service().await;
        seed_n(&svc, 5).await;
        let first = svc.list_as_of(page(0, 2, ts_desc()), None).await.unwrap();
        seed_n(&svc, 1).await; // id 6
        svc.prune(None, Some(5)).await.unwrap(); // id 1 が消える
        let second = svc
            .list_as_of(page(2, 2, ts_desc()), Some(first.as_of_id))
            .await
            .unwrap();
        assert_eq!(second.total_count, first.total_count - 1);
    }

    /// 同じ `ts` の行が並んでも、並びは `id` で一意に決まる（`ts` は秒単位）。
    /// 並べ替えの向きに揃えた `id` が最後に足される。
    #[tokio::test]
    async fn list_breaks_ts_ties_by_id_in_the_sort_direction() {
        let svc = service().await;
        seed_n(&svc, 4).await;
        sqlx::query("UPDATE audit_log SET ts = '2026-09-24 00:00:00'")
            .execute(&svc.pool)
            .await
            .unwrap();

        let desc = svc.list(page(0, 10, ts_desc())).await.unwrap();
        assert_eq!(ids(&desc), vec![4, 3, 2, 1]);

        let asc = svc
            .list(page(
                0,
                10,
                vec![SortState {
                    field: "ts".to_string(),
                    direction: SortDirection::Asc,
                }],
            ))
            .await
            .unwrap();
        assert_eq!(ids(&asc), vec![1, 2, 3, 4]);

        // 並べ替えが無ければ新しい順。
        let none = svc.list(page(0, 10, vec![])).await.unwrap();
        assert_eq!(ids(&none), vec![4, 3, 2, 1]);
    }

    /// banto-hub で再現した形（#428）: 絞り込みなしの `ts DESC` は `ts` の索引を
    /// 逆に辿るので偶然 `id` 降順になるが、`actorUsername` で絞ると別の索引が
    /// 選ばれ、同じ秒の行が**古い順**（修正前は `[1, 3, 4, 6]`）に並んだ。
    #[tokio::test]
    async fn list_breaks_ts_ties_by_id_under_a_filter() {
        let svc = service().await;
        for actor in ["alice", "bob", "alice", "alice", "bob", "alice"] {
            svc.record(sample_entry("create", "items", actor)).await;
        }
        sqlx::query("UPDATE audit_log SET ts = '2026-09-24 00:00:00'")
            .execute(&svc.pool)
            .await
            .unwrap();
        let result = svc
            .list(ListParams {
                sort: ts_desc(),
                filters: vec![FilterState {
                    field: "actorUsername".to_string(),
                    op: FilterOp::Eq,
                    value: json!("alice"),
                }],
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(ids(&result), vec![6, 4, 3, 1]);
    }

    #[test]
    fn id_tiebreaker_is_added_once_and_follows_the_last_known_sort() {
        let with = with_id_tiebreaker(page(0, 1, ts_desc()));
        assert_eq!(with.sort.len(), 2);
        assert_eq!(with.sort[1].field, "id");
        assert_eq!(with.sort[1].direction, SortDirection::Desc);

        // すでに `id` で並べているなら足さない。
        let already = with_id_tiebreaker(page(
            0,
            1,
            vec![SortState {
                field: "id".to_string(),
                direction: SortDirection::Asc,
            }],
        ));
        assert_eq!(already.sort.len(), 1);

        // 列として解決できない並べ替え（黙って無視される）は向きの根拠にしない。
        let unknown = with_id_tiebreaker(page(
            0,
            1,
            vec![
                SortState {
                    field: "action".to_string(),
                    direction: SortDirection::Asc,
                },
                SortState {
                    field: "nope".to_string(),
                    direction: SortDirection::Desc,
                },
            ],
        ));
        assert_eq!(unknown.sort.last().unwrap().direction, SortDirection::Asc);
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
