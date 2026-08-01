//! Database bootstrap for the relay-wright app (plan
//! `luminous-discovering-goblet.md`, W1): connect, apply this app's own
//! schema, then `banto_tags::migrate` (PLC connection/collection group/tag
//! registry tables) against the SAME pool - relay-wright shares one SQLite
//! database across the app's own tables (settings/users/audit_log plus
//! this app's own write_targets/write_rules/write_rule_conditions/
//! write_audit_log/armed_state) and banto-tags' tables (plan.md §5: "single
//! app-data file"), so this is the one place that bootstraps the whole
//! schema.
//!
//! This app's own tables are applied as plain **idempotent DDL**
//! (`CREATE TABLE IF NOT EXISTS` etc.) rather than through
//! `sqlx::migrate!`. This crate started from `apps/chronogazer/core/src/db.rs`
//! (itself adapted from the banto template's `apps/admin-template/core/src/db.rs`),
//! and keeps that adaptation rather than the template's own `sqlx::migrate!`
//! pattern, for the same reason chronogazer needed it: `banto_tags::migrate`
//! below runs its OWN embedded `sqlx::migrate!` on the identical database,
//! and `sqlx`'s migration bookkeeping table (`_sqlx_migrations`) is a
//! single, database-wide table with no way to namespace it per crate
//! (`sqlx` 0.8 has no `Migrator::set_table_name`) - two independent
//! `sqlx::migrate!` sources sharing one pool collide on overlapping VERSION
//! NUMBERS, empirically confirmed in chronogazer as
//! `MigrateError::VersionMismatch`/`VersionMissing` on every single
//! `init_db`/`init_db_memory` call (see `docs/r1a-readme-gaps.md` for the
//! full writeup).
//!
//! Note this collision is about version-number bookkeeping, NOT about
//! whether the two migrators' tables overlap in content - the plan's own
//! W1 section assumed this app's brand-new, non-overlapping write_* tables
//! could safely use a plain `sqlx::migrate!` since "完全に別テーブルのため"
//! (banto-collect's `CREATE TABLE IF NOT EXISTS` workaround supposedly
//! being unnecessary here). That assumption does not hold: the collision is
//! in the SHARED `_sqlx_migrations` table itself, which fires regardless of
//! table content the moment two independent `sqlx::migrate!` calls run
//! against the same pool. So this module keeps the idempotent-DDL pattern
//! for ALL of this app's own tables, including the new write_* ones, not
//! just the pre-existing settings/users/audit_log ones.
//!
//! `crates/banto-collect/migrations/0001_collect_events.sql` documents the
//! identical constraint (its `collect_events` table, for the same reason).
//! The `migrations/*.sql` files in this crate remain as schema
//! documentation/history; they are no longer executed by `sqlx::migrate!` -
//! [`apply_app_schema`] below is the actual source of truth and must be
//! kept in sync with them by hand.

use banto_core::BantoError;
use sqlx::SqlitePool;

/// The one SQLite pool type every service in this crate is built over,
/// re-exported so downstream crates (notably `src-tauri`, whose invariant is
/// to add NO new dependencies) can name it - e.g. to hold the pool in their
/// own app state - without taking a direct `sqlx` dependency of their own.
pub type DbPool = SqlitePool;

/// Connect to the SQLite database at `path` and apply the full schema (this
/// app's own, then `banto_tags`'s). Used by the `src-tauri` adapter with a
/// path under the app's data directory.
pub async fn init_db(path: impl AsRef<std::path::Path>) -> Result<SqlitePool, BantoError> {
    let pool = banto_storage::connect_sqlite(path).await?;
    run_migrations(&pool).await?;
    Ok(pool)
}

/// Same as [`init_db`] but against a private in-memory database. Used by
/// tests so each test gets an isolated, fully-migrated database.
pub async fn init_db_memory() -> Result<SqlitePool, BantoError> {
    let pool = banto_storage::connect_sqlite_memory().await?;
    run_migrations(&pool).await?;
    Ok(pool)
}

/// Same as [`init_db_memory`], `pub(crate)` for `rest.rs`'s test module -
/// kept as a separate name (rather than just reusing `init_db_memory`
/// directly) since it predates this app's own migrations being the only
/// thing seeded here and several call sites already spell it this way.
#[cfg(test)]
pub(crate) async fn migrate_memory() -> Result<SqlitePool, BantoError> {
    let pool = banto_storage::connect_sqlite_memory().await?;
    run_migrations(&pool).await?;
    Ok(pool)
}

async fn run_migrations(pool: &SqlitePool) -> Result<(), BantoError> {
    apply_app_schema(pool).await?;
    // I1 (docs/plan.md): banto-tags owns its own migrations/ directory and
    // is applied here, right after this app's own schema, so every caller
    // of init_db/init_db_memory gets the full schema in one call - see
    // banto_tags::migrate's doc comment for why it is designed to be
    // called this way, and this module's own doc comment for why THIS
    // app's half is deliberately NOT also a `sqlx::migrate!` source.
    banto_tags::migrate(pool).await?;
    Ok(())
}

/// This app's own tables, applied as idempotent DDL - see this module's
/// doc comment for why. Mirrors `migrations/0001_settings.sql` through
/// `migrations/0010_qr_strings.sql` exactly; update both together.
async fn apply_app_schema(pool: &SqlitePool) -> Result<(), BantoError> {
    // 0001_settings.sql
    sqlx::query("CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
        .execute(pool)
        .await
        .map_err(banto_storage::storage_error)?;

    // 0002_users.sql
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS users (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            username TEXT NOT NULL UNIQUE,
            password_hash TEXT NOT NULL,
            display_name TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
    )
    .execute(pool)
    .await
    .map_err(banto_storage::storage_error)?;

    // 0003_user_roles.sql: SQLite has no `ADD COLUMN IF NOT EXISTS`, so
    // check `pragma_table_info` first - the idempotent equivalent.
    let has_role_column: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM pragma_table_info('users') WHERE name = 'role'")
            .fetch_one(pool)
            .await
            .map_err(banto_storage::storage_error)?;
    if has_role_column == 0 {
        sqlx::query(
            "ALTER TABLE users ADD COLUMN role TEXT NOT NULL DEFAULT 'admin' \
             CHECK (role IN ('admin','editor','viewer'))",
        )
        .execute(pool)
        .await
        .map_err(banto_storage::storage_error)?;
    }

    // 0004_audit_log.sql
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS audit_log (
          id INTEGER PRIMARY KEY AUTOINCREMENT,
          ts TEXT NOT NULL DEFAULT (datetime('now')),
          actor_username TEXT,
          actor_role TEXT,
          action TEXT NOT NULL,
          resource TEXT NOT NULL,
          entity_id TEXT,
          detail TEXT,
          origin TEXT NOT NULL,
          result TEXT NOT NULL DEFAULT 'ok'
        )",
    )
    .execute(pool)
    .await
    .map_err(banto_storage::storage_error)?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_audit_log_ts ON audit_log(ts)")
        .execute(pool)
        .await
        .map_err(banto_storage::storage_error)?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_audit_log_actor ON audit_log(actor_username)")
        .execute(pool)
        .await
        .map_err(banto_storage::storage_error)?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_audit_log_resource ON audit_log(resource, entity_id)",
    )
    .execute(pool)
    .await
    .map_err(banto_storage::storage_error)?;

    // 0005_write_targets.sql
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS write_targets (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE,
            plc_connection_id INTEGER NOT NULL,
            address TEXT NOT NULL,
            data_type TEXT NOT NULL CHECK (data_type IN ('bit', 'i16', 'u16', 'i32', 'u32', 'f32')),
            raw_lo REAL,
            raw_hi REAL,
            eng_lo REAL,
            eng_hi REAL,
            unit TEXT,
            decimals INTEGER NOT NULL DEFAULT 0 CHECK (decimals BETWEEN 0 AND 6),
            enabled INTEGER NOT NULL DEFAULT 1
        )",
    )
    .execute(pool)
    .await
    .map_err(banto_storage::storage_error)?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_write_targets_plc_connection_id \
         ON write_targets (plc_connection_id)",
    )
    .execute(pool)
    .await
    .map_err(banto_storage::storage_error)?;

    // 0006_write_rules.sql
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS write_rules (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE,
            enabled INTEGER NOT NULL DEFAULT 0,
            edge_mode TEXT NOT NULL CHECK (edge_mode IN ('rising', 'falling', 'change')),
            cooldown_ms INTEGER,
            write_target_id INTEGER NOT NULL REFERENCES write_targets(id) ON DELETE RESTRICT,
            write_value_mode TEXT NOT NULL CHECK (write_value_mode IN ('constant', 'copy_from_source')),
            write_constant_value REAL,
            write_source_tag_id INTEGER
        )",
    )
    .execute(pool)
    .await
    .map_err(banto_storage::storage_error)?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_write_rules_write_target_id ON write_rules (write_target_id)",
    )
    .execute(pool)
    .await
    .map_err(banto_storage::storage_error)?;

    // 0007_write_rule_conditions.sql
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS write_rule_conditions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            write_rule_id INTEGER NOT NULL REFERENCES write_rules(id) ON DELETE CASCADE,
            source_tag_id INTEGER NOT NULL,
            operator TEXT NOT NULL CHECK (
                operator IN ('eq', 'neq', 'gt', 'gte', 'lt', 'lte', 'between', 'bit_is')
            ),
            threshold_value REAL NOT NULL,
            threshold_value_2 REAL
        )",
    )
    .execute(pool)
    .await
    .map_err(banto_storage::storage_error)?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_write_rule_conditions_write_rule_id \
         ON write_rule_conditions (write_rule_id)",
    )
    .execute(pool)
    .await
    .map_err(banto_storage::storage_error)?;

    // 0008_write_audit_log.sql
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS write_audit_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ts TEXT NOT NULL DEFAULT (datetime('now')),
            write_rule_id INTEGER,
            rule_name_snapshot TEXT NOT NULL,
            source_tag_id INTEGER,
            source_value_snapshot REAL,
            write_target_id INTEGER,
            target_value_written REAL,
            actor_username TEXT,
            action TEXT NOT NULL CHECK (
                action IN ('rule_fire', 'arm', 'disarm', 'dry_run_toggle', 'rate_limit_tripped')
            ),
            result TEXT NOT NULL CHECK (
                result IN (
                    'ok', 'failed', 'suppressed_disarmed', 'suppressed_rate_limited',
                    'suppressed_dry_run'
                )
            ),
            detail TEXT
        )",
    )
    .execute(pool)
    .await
    .map_err(banto_storage::storage_error)?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_write_audit_log_ts ON write_audit_log (ts)")
        .execute(pool)
        .await
        .map_err(banto_storage::storage_error)?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_write_audit_log_write_rule_id \
         ON write_audit_log (write_rule_id)",
    )
    .execute(pool)
    .await
    .map_err(banto_storage::storage_error)?;

    // 0009_armed_state.sql. SAFETY (plan W1/W3): this table only persists
    // the last-known armed state for audit/UI history display - W3's
    // engine must always initialize its IN-MEMORY armed flag to `false`
    // on every process start regardless of `armed_persisted`'s stored
    // value (see the migration file's doc comment for the full rationale).
    // `INSERT OR IGNORE` seeds the single disarmed row exactly once so
    // every later reader can assume it always has exactly one row.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS armed_state (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            armed_persisted INTEGER NOT NULL DEFAULT 0,
            last_changed_at TEXT,
            last_changed_by TEXT
        )",
    )
    .execute(pool)
    .await
    .map_err(banto_storage::storage_error)?;
    sqlx::query(
        "INSERT OR IGNORE INTO armed_state (id, armed_persisted, last_changed_at, last_changed_by) \
         VALUES (1, 0, NULL, NULL)",
    )
    .execute(pool)
    .await
    .map_err(banto_storage::storage_error)?;

    // 0010_qr_strings.sql: QR文字列リスト（タッチパネル読み取りデバッグ支援、
    // /qr-codes 画面）。他テーブルへの参照を持たない独立ユーティリティ。
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS qr_strings (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            label TEXT NOT NULL DEFAULT '',
            text TEXT NOT NULL,
            sort_order INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
    )
    .execute(pool)
    .await
    .map_err(banto_storage::storage_error)?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_qr_strings_sort_order ON qr_strings (sort_order)")
        .execute(pool)
        .await
        .map_err(banto_storage::storage_error)?;

    Ok(())
}

/// Days-since-epoch (1970-01-01) -> `YYYY-MM-DD`, using Howard Hinnant's
/// `civil_from_days` algorithm (http://howardhinnant.github.io/date_algorithms.html).
/// No date/time crate dependency for one small conversion.
///
/// `pub(crate)` (not private) since `crate::backup` (spec M17) reuses this to
/// turn a backup file's filesystem mtime into an ISO date for display.
pub(crate) fn iso_date_from_days_since_epoch(days: i64) -> String {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_date_round_trips_known_epoch_days() {
        assert_eq!(iso_date_from_days_since_epoch(0), "1970-01-01");
        assert_eq!(iso_date_from_days_since_epoch(1), "1970-01-02");
        assert_eq!(iso_date_from_days_since_epoch(-1), "1969-12-31");
    }

    /// End-to-end proof that `init_db_memory` applies BOTH this app's own
    /// schema (`settings`/`users`/`audit_log`, including the `role` column,
    /// plus this app's own `write_targets`/`write_rules`/
    /// `write_rule_conditions`/`write_audit_log`/`armed_state`) AND
    /// `banto_tags`'s (`plc_connections`/`collection_groups`/`tags`)
    /// against the same pool - the W1 integration point this module exists
    /// to wire up.
    #[tokio::test]
    async fn init_db_memory_applies_both_this_apps_and_banto_tags_schema() {
        let pool = init_db_memory()
            .await
            .expect("init_db_memory should succeed");
        for table in [
            "settings",
            "users",
            "audit_log",
            "write_targets",
            "write_rules",
            "write_rule_conditions",
            "write_audit_log",
            "armed_state",
            "qr_strings",
            "plc_connections",
            "collection_groups",
            "tags",
        ] {
            let exists: Option<String> = sqlx::query_scalar(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?",
            )
            .bind(table)
            .fetch_optional(&pool)
            .await
            .unwrap();
            assert!(exists.is_some(), "expected table '{table}' to exist");
        }

        let has_role_column: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('users') WHERE name = 'role'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(has_role_column, 1, "expected users.role to exist");
    }

    /// `armed_state` must always have exactly its one seeded row
    /// (`id = 1`), and that row must start disarmed - this is the
    /// PERSISTED default only; it does not by itself prove W3's separate
    /// in-memory-always-starts-disarmed rule (there is no engine yet in
    /// W1), but a persisted default of "armed" would be a safety footgun
    /// waiting to happen the moment W3 is written, so it is pinned here.
    #[tokio::test]
    async fn armed_state_is_seeded_as_a_single_disarmed_row() {
        let pool = init_db_memory()
            .await
            .expect("init_db_memory should succeed");
        let rows: Vec<(i64, i64)> = sqlx::query_as("SELECT id, armed_persisted FROM armed_state")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "expected exactly one armed_state row");
        assert_eq!(rows[0], (1, 0), "expected the seeded row to be disarmed");
    }

    /// Running schema application twice against the same pool must not
    /// error and must not duplicate the `role` column - both this app's
    /// idempotent DDL and `banto_tags::migrate`'s own bookkeeping table
    /// tolerate being called again, so a second `init_db`-style call (e.g.
    /// a future feature that re-checks schema on every launch) is always
    /// safe.
    #[tokio::test]
    async fn schema_application_is_idempotent_across_two_runs_on_the_same_db() {
        let pool = banto_storage::connect_sqlite_memory().await.unwrap();
        run_migrations(&pool).await.unwrap();
        run_migrations(&pool).await.unwrap(); // second run: must not error
    }
}
