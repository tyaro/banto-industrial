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
/// `migrations/0014_write_audit_log_manual_write.sql` exactly; update both
/// together. The `CREATE TABLE IF NOT EXISTS` statements below carry the
/// LATEST schema (so a fresh database is right immediately); the
/// S2 文字列タグ upgrade steps at the end of this function bring an
/// existing pre-S2 database up to the same shape (see
/// [`upgrade_write_targets_for_string`] and friends).
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

    // 0005_write_targets.sql + 0011_write_targets_allow_string.sql (S2
    // 文字列タグ): the CHECK includes 'string' and the companion
    // `string_length` column (1..=128 words iff data_type='string', enforced
    // at the service layer like banto-tags' 0005 - the SQL CHECK below is
    // defense-in-depth only).
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS write_targets (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE,
            plc_connection_id INTEGER NOT NULL,
            address TEXT NOT NULL,
            data_type TEXT NOT NULL CHECK (data_type IN ('bit', 'i16', 'u16', 'i32', 'u32', 'f32', 'string')),
            string_length INTEGER CHECK (string_length IS NULL OR string_length BETWEEN 1 AND 128),
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

    // 0006_write_rules.sql + 0013_write_rules_constant_text.sql (S2
    // 文字列タグ): `write_constant_text` carries the constant for a STRING
    // write target (`write_constant_value` stays NULL then); exactly one of
    // the two is set for constant-mode rules, enforced at the service layer.
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
            write_constant_text TEXT,
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

    // 0007_write_rule_conditions.sql +
    // 0012_write_rule_conditions_threshold_text.sql (S2 文字列タグ):
    // `threshold_value` is now nullable and `threshold_text` added - a
    // condition on a STRING source tag carries its eq/neq comparand as text
    // (numeric threshold columns NULL), a numeric condition the reverse;
    // which side must be set is enforced at the service layer (it depends on
    // the source tag's data type, which lives in banto-tags' `tags` table).
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS write_rule_conditions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            write_rule_id INTEGER NOT NULL REFERENCES write_rules(id) ON DELETE CASCADE,
            source_tag_id INTEGER NOT NULL,
            operator TEXT NOT NULL CHECK (
                operator IN ('eq', 'neq', 'gt', 'gte', 'lt', 'lte', 'between', 'bit_is')
            ),
            threshold_value REAL,
            threshold_value_2 REAL,
            threshold_text TEXT
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

    // 0008_write_audit_log.sql + 0014_write_audit_log_manual_write.sql
    // (feature/tag-monitor): the action CHECK includes 'manual_write' (the
    // タグモニタ screen's one-shot debug writes are audited under it).
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
                action IN ('rule_fire', 'arm', 'disarm', 'dry_run_toggle', 'rate_limit_tripped',
                           'manual_write')
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

    // --- S2 文字列タグ schema upgrades for PRE-S2 databases ---------------
    //
    // The `CREATE TABLE IF NOT EXISTS` statements above already carry the
    // final schema, so a fresh database needs none of this. An existing
    // database is detected column-by-column via `pragma_table_info` (the
    // same idempotent trick 0003's `users.role` uses) and upgraded in place.
    // Each step is one transaction, so a crash mid-upgrade leaves the
    // database either fully before or fully after that step and this
    // function simply resumes on the next launch.

    // 0011_write_targets_allow_string.sql: widen data_type's CHECK + add
    // string_length. SQLite cannot ALTER a CHECK constraint, so the table is
    // rebuilt - and since `write_rules` REFERENCES write_targets with
    // ON DELETE RESTRICT, the rebuild needs banto-tags' 0004 park-and-restore
    // dance, not 0005's simpler leaf pattern (see the migration file's doc
    // comment for the full constraint story).
    let has_string_length: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('write_targets') WHERE name = 'string_length'",
    )
    .fetch_one(pool)
    .await
    .map_err(banto_storage::storage_error)?;
    if has_string_length == 0 {
        upgrade_write_targets_for_string(pool).await?;
    }

    // 0012_write_rule_conditions_threshold_text.sql: threshold_value loses
    // its NOT NULL (a string condition has no numeric threshold) and
    // threshold_text is added. NOT NULL cannot be dropped by ALTER either,
    // so this is also a rebuild - but write_rule_conditions is a LEAF table
    // (nothing references it), so banto-tags' 0005 leaf pattern applies.
    let has_threshold_text: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('write_rule_conditions') \
         WHERE name = 'threshold_text'",
    )
    .fetch_one(pool)
    .await
    .map_err(banto_storage::storage_error)?;
    if has_threshold_text == 0 {
        upgrade_write_rule_conditions_for_string(pool).await?;
    }

    // 0013_write_rules_constant_text.sql: plain nullable column, so the
    // simple ADD COLUMN path (0003's users.role precedent) suffices.
    let has_constant_text: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('write_rules') WHERE name = 'write_constant_text'",
    )
    .fetch_one(pool)
    .await
    .map_err(banto_storage::storage_error)?;
    if has_constant_text == 0 {
        sqlx::query("ALTER TABLE write_rules ADD COLUMN write_constant_text TEXT")
            .execute(pool)
            .await
            .map_err(banto_storage::storage_error)?;
    }

    // 0014_write_audit_log_manual_write.sql (feature/tag-monitor): widen
    // write_audit_log's action CHECK with 'manual_write'. No column changes,
    // so `pragma_table_info` cannot detect it - the idempotent detection
    // reads the table's own DDL out of sqlite_master instead (the CHECK's
    // literal is part of the stored CREATE TABLE text). SQLite cannot ALTER
    // a CHECK, so a pre-monitor database gets the leaf-rebuild treatment
    // (write_audit_log is referenced by nothing).
    let create_sql: Option<String> = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'write_audit_log'",
    )
    .fetch_optional(pool)
    .await
    .map_err(banto_storage::storage_error)?;
    let needs_manual_write = create_sql
        .map(|sql| !sql.contains("manual_write"))
        .unwrap_or(false);
    if needs_manual_write {
        upgrade_write_audit_log_for_manual_write(pool).await?;
    }

    Ok(())
}

/// Rebuild `write_audit_log` with the manual_write-capable action CHECK
/// (mirrors `migrations/0014_write_audit_log_manual_write.sql`). A LEAF
/// rebuild - nothing references `write_audit_log` - so banto-tags' 0005 leaf
/// pattern applies verbatim (same as
/// [`upgrade_write_rule_conditions_for_string`]): copy every row (audit
/// history must survive the upgrade byte-for-byte, `ts` included - no DEFAULT
/// re-evaluation because `ts` is copied explicitly), drop, rename, recreate
/// both indexes under their original names. One transaction, so a crash
/// mid-upgrade leaves the database fully before or fully after.
async fn upgrade_write_audit_log_for_manual_write(pool: &SqlitePool) -> Result<(), BantoError> {
    let mut tx = pool.begin().await.map_err(banto_storage::storage_error)?;

    for sql in [
        "CREATE TABLE write_audit_log_new (
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
                action IN ('rule_fire', 'arm', 'disarm', 'dry_run_toggle', 'rate_limit_tripped',
                           'manual_write')
            ),
            result TEXT NOT NULL CHECK (
                result IN (
                    'ok', 'failed', 'suppressed_disarmed', 'suppressed_rate_limited',
                    'suppressed_dry_run'
                )
            ),
            detail TEXT
        )",
        "INSERT INTO write_audit_log_new (
            id, ts, write_rule_id, rule_name_snapshot, source_tag_id, source_value_snapshot,
            write_target_id, target_value_written, actor_username, action, result, detail
        )
        SELECT
            id, ts, write_rule_id, rule_name_snapshot, source_tag_id, source_value_snapshot,
            write_target_id, target_value_written, actor_username, action, result, detail
        FROM write_audit_log",
        "DROP TABLE write_audit_log",
        "ALTER TABLE write_audit_log_new RENAME TO write_audit_log",
        "CREATE INDEX idx_write_audit_log_ts ON write_audit_log (ts)",
        "CREATE INDEX idx_write_audit_log_write_rule_id ON write_audit_log (write_rule_id)",
    ] {
        sqlx::query(sql)
            .execute(&mut *tx)
            .await
            .map_err(banto_storage::storage_error)?;
    }

    tx.commit().await.map_err(banto_storage::storage_error)
}

/// Rebuild `write_targets` with the S2 string-capable schema (mirrors
/// `migrations/0011_write_targets_allow_string.sql`; see that file for the
/// full rationale). Runs in ONE transaction on ONE pooled connection - both
/// matter: the transaction makes the rebuild atomic, and the temporary
/// parking tables are per-connection so every statement must share the
/// connection the transaction holds.
///
/// `write_targets` is a REFERENCED table (`write_rules.write_target_id`,
/// ON DELETE RESTRICT), so this mirrors banto-tags' 0004 park-and-restore
/// dance rather than 0005's leaf rebuild: with foreign keys enforced
/// (banto-storage connects with `foreign_keys(true)`), `DROP TABLE
/// write_targets` performs an implicit `DELETE FROM`, which trips the
/// children's RESTRICT the moment any rule exists - and renaming the OLD
/// table out of the way instead would drag `write_rules`' REFERENCES clause
/// along with the rename. So: park the descendant rows (write_rule_conditions
/// first - it cascades from write_rules - then write_rules), delete them,
/// rebuild write_targets, rename the NEW table into the vacated name (nothing
/// references `write_targets_new`, so that rename rewrites nothing), then
/// restore the parked rows shallowest-first.
///
/// A database that needs this upgrade is pre-S2 by construction, so the
/// parked `write_rules` rows still have the OLD column set (no
/// `write_constant_text` - step 0013 runs after this one) and the explicit
/// column lists below name exactly that old shape.
async fn upgrade_write_targets_for_string(pool: &SqlitePool) -> Result<(), BantoError> {
    let mut tx = pool.begin().await.map_err(banto_storage::storage_error)?;

    for sql in [
        // Park the descendants, deepest first.
        "CREATE TEMPORARY TABLE _u0011_write_rule_conditions AS SELECT * FROM write_rule_conditions",
        "CREATE TEMPORARY TABLE _u0011_write_rules AS SELECT * FROM write_rules",
        "DELETE FROM write_rule_conditions",
        "DELETE FROM write_rules",
        // Rebuild write_targets with the widened CHECK + string_length.
        "CREATE TABLE write_targets_new (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE,
            plc_connection_id INTEGER NOT NULL,
            address TEXT NOT NULL,
            data_type TEXT NOT NULL CHECK (data_type IN ('bit', 'i16', 'u16', 'i32', 'u32', 'f32', 'string')),
            string_length INTEGER CHECK (string_length IS NULL OR string_length BETWEEN 1 AND 128),
            raw_lo REAL,
            raw_hi REAL,
            eng_lo REAL,
            eng_hi REAL,
            unit TEXT,
            decimals INTEGER NOT NULL DEFAULT 0 CHECK (decimals BETWEEN 0 AND 6),
            enabled INTEGER NOT NULL DEFAULT 1
        )",
        "INSERT INTO write_targets_new (
            id, name, plc_connection_id, address, data_type,
            raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals, enabled
        )
        SELECT
            id, name, plc_connection_id, address, data_type,
            raw_lo, raw_hi, eng_lo, eng_hi, unit, decimals, enabled
        FROM write_targets",
        "DROP TABLE write_targets",
        "ALTER TABLE write_targets_new RENAME TO write_targets",
        // 0005's index does not survive the rebuild (indexes belong to the
        // dropped table), so it is recreated under its original name.
        "CREATE INDEX idx_write_targets_plc_connection_id ON write_targets (plc_connection_id)",
        // Put the descendants back, shallowest first (old column shape - see
        // the function doc comment).
        "INSERT INTO write_rules (
            id, name, enabled, edge_mode, cooldown_ms, write_target_id,
            write_value_mode, write_constant_value, write_source_tag_id
        )
        SELECT
            id, name, enabled, edge_mode, cooldown_ms, write_target_id,
            write_value_mode, write_constant_value, write_source_tag_id
        FROM _u0011_write_rules",
        "INSERT INTO write_rule_conditions (
            id, write_rule_id, source_tag_id, operator, threshold_value, threshold_value_2
        )
        SELECT
            id, write_rule_id, source_tag_id, operator, threshold_value, threshold_value_2
        FROM _u0011_write_rule_conditions",
        "DROP TABLE _u0011_write_rules",
        "DROP TABLE _u0011_write_rule_conditions",
    ] {
        sqlx::query(sql)
            .execute(&mut *tx)
            .await
            .map_err(banto_storage::storage_error)?;
    }

    tx.commit().await.map_err(banto_storage::storage_error)
}

/// Rebuild `write_rule_conditions` with the S2 string-capable schema
/// (mirrors `migrations/0012_write_rule_conditions_threshold_text.sql`):
/// `threshold_value` drops NOT NULL and `threshold_text` is added. This is a
/// LEAF table - nothing references it - so banto-tags' 0005 leaf-rebuild
/// pattern applies verbatim: the implicit `DELETE FROM` of `DROP TABLE` only
/// deletes CHILD rows of write_rules (ON DELETE CASCADE restricts nothing),
/// and renaming `write_rule_conditions_new` rewrites no other table's schema
/// while keeping its own REFERENCES write_rules(id) clause intact.
async fn upgrade_write_rule_conditions_for_string(pool: &SqlitePool) -> Result<(), BantoError> {
    let mut tx = pool.begin().await.map_err(banto_storage::storage_error)?;

    for sql in [
        "CREATE TABLE write_rule_conditions_new (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            write_rule_id INTEGER NOT NULL REFERENCES write_rules(id) ON DELETE CASCADE,
            source_tag_id INTEGER NOT NULL,
            operator TEXT NOT NULL CHECK (
                operator IN ('eq', 'neq', 'gt', 'gte', 'lt', 'lte', 'between', 'bit_is')
            ),
            threshold_value REAL,
            threshold_value_2 REAL,
            threshold_text TEXT
        )",
        "INSERT INTO write_rule_conditions_new (
            id, write_rule_id, source_tag_id, operator, threshold_value, threshold_value_2
        )
        SELECT
            id, write_rule_id, source_tag_id, operator, threshold_value, threshold_value_2
        FROM write_rule_conditions",
        "DROP TABLE write_rule_conditions",
        "ALTER TABLE write_rule_conditions_new RENAME TO write_rule_conditions",
        "CREATE INDEX idx_write_rule_conditions_write_rule_id \
         ON write_rule_conditions (write_rule_id)",
    ] {
        sqlx::query(sql)
            .execute(&mut *tx)
            .await
            .map_err(banto_storage::storage_error)?;
    }

    tx.commit().await.map_err(banto_storage::storage_error)
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

    /// Hand-build the PRE-S2 shape of the three write_* tables (byte-for-byte
    /// the DDL this module carried before the 0011-0013 upgrades) and
    /// populate them with a referencing chain
    /// (target ← rule ← condition), so the upgrade test below runs against
    /// the exact schema a production pre-S2 database has - foreign keys
    /// enforced, RESTRICT in place.
    async fn seed_pre_s2_write_tables(pool: &SqlitePool) {
        for sql in [
            "CREATE TABLE write_targets (
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
            "CREATE INDEX idx_write_targets_plc_connection_id \
             ON write_targets (plc_connection_id)",
            "CREATE TABLE write_rules (
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
            "CREATE INDEX idx_write_rules_write_target_id ON write_rules (write_target_id)",
            "CREATE TABLE write_rule_conditions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                write_rule_id INTEGER NOT NULL REFERENCES write_rules(id) ON DELETE CASCADE,
                source_tag_id INTEGER NOT NULL,
                operator TEXT NOT NULL CHECK (
                    operator IN ('eq', 'neq', 'gt', 'gte', 'lt', 'lte', 'between', 'bit_is')
                ),
                threshold_value REAL NOT NULL,
                threshold_value_2 REAL
            )",
            "CREATE INDEX idx_write_rule_conditions_write_rule_id \
             ON write_rule_conditions (write_rule_id)",
            "INSERT INTO write_targets (id, name, plc_connection_id, address, data_type, decimals, enabled) \
             VALUES (7, 'WT7', 1, 'D200', 'u16', 2, 1)",
            "INSERT INTO write_rules (id, name, enabled, edge_mode, write_target_id, \
                write_value_mode, write_constant_value) \
             VALUES (3, 'R3', 1, 'rising', 7, 'constant', 42.5)",
            "INSERT INTO write_rule_conditions (id, write_rule_id, source_tag_id, operator, threshold_value) \
             VALUES (9, 3, 11, 'gt', 100.0)",
        ] {
            sqlx::query(sql).execute(pool).await.unwrap();
        }
    }

    /// The S2 upgrade path against a POPULATED pre-S2 database: the
    /// write_targets rebuild (park-and-restore across the RESTRICT foreign
    /// key), the write_rule_conditions leaf rebuild, and the write_rules
    /// ADD COLUMN must all apply without losing a row, the widened CHECK
    /// must accept 'string' afterward, no foreign key may be left dangling,
    /// and a second run must be a no-op.
    #[tokio::test]
    async fn s2_upgrade_preserves_rows_and_foreign_keys_on_a_populated_pre_s2_db() {
        let pool = banto_storage::connect_sqlite_memory().await.unwrap();
        seed_pre_s2_write_tables(&pool).await;

        run_migrations(&pool).await.expect("upgrade should apply");

        // New columns exist.
        for (table, column) in [
            ("write_targets", "string_length"),
            ("write_rule_conditions", "threshold_text"),
            ("write_rules", "write_constant_text"),
        ] {
            let count: i64 = sqlx::query_scalar(&format!(
                "SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = '{column}'"
            ))
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(count, 1, "expected {table}.{column} to exist");
        }

        // Every row survived, ids and values intact.
        let target: (i64, String, String, Option<i64>, i64) = sqlx::query_as(
            "SELECT id, name, data_type, string_length, decimals FROM write_targets",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(target, (7, "WT7".into(), "u16".into(), None, 2));
        let rule: (i64, String, i64, Option<f64>, Option<String>) = sqlx::query_as(
            "SELECT id, name, write_target_id, write_constant_value, write_constant_text \
             FROM write_rules",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(rule, (3, "R3".into(), 7, Some(42.5), None));
        let condition: (i64, i64, String, Option<f64>, Option<String>) = sqlx::query_as(
            "SELECT id, write_rule_id, operator, threshold_value, threshold_text \
             FROM write_rule_conditions",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(condition, (9, 3, "gt".into(), Some(100.0), None));

        // The widened CHECK accepts a string write target now.
        sqlx::query(
            "INSERT INTO write_targets (name, plc_connection_id, address, data_type, string_length) \
             VALUES ('WTS', 1, 'D300', 'string', 4)",
        )
        .execute(&pool)
        .await
        .expect("'string' must pass the rebuilt CHECK");
        // ...and a string condition can carry text with NULL numeric fields.
        sqlx::query(
            "INSERT INTO write_rule_conditions (write_rule_id, source_tag_id, operator, threshold_text) \
             VALUES (3, 12, 'eq', 'OK')",
        )
        .execute(&pool)
        .await
        .expect("nullable threshold_value + threshold_text must be accepted");

        // No dangling foreign keys after the park-and-restore dance.
        let violations: Vec<(String, i64)> =
            sqlx::query_as("SELECT \"table\", rowid FROM pragma_foreign_key_check")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert!(violations.is_empty(), "foreign_key_check: {violations:?}");

        // The RESTRICT relation still works against the rebuilt table.
        let delete = sqlx::query("DELETE FROM write_targets WHERE id = 7")
            .execute(&pool)
            .await;
        assert!(
            delete.is_err(),
            "deleting a target still referenced by a rule must trip RESTRICT"
        );

        // Second run: all three upgrades detect their columns and no-op.
        run_migrations(&pool).await.expect("re-run must be a no-op");
    }

    /// Hand-build the PRE-tag-monitor `write_audit_log` (byte-for-byte the
    /// 0008 DDL - the action CHECK without 'manual_write') and populate it,
    /// so the 0014 upgrade below runs against the exact shape a production
    /// pre-monitor database has.
    async fn seed_pre_manual_write_audit_log(pool: &SqlitePool) {
        for sql in [
            "CREATE TABLE write_audit_log (
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
            "CREATE INDEX idx_write_audit_log_ts ON write_audit_log (ts)",
            "CREATE INDEX idx_write_audit_log_write_rule_id ON write_audit_log (write_rule_id)",
            "INSERT INTO write_audit_log \
                (id, ts, write_rule_id, rule_name_snapshot, source_tag_id, source_value_snapshot, \
                 write_target_id, target_value_written, actor_username, action, result, detail) \
             VALUES (5, '2026-01-02 03:04:05', 7, 'R7', 3, 12.5, 4, 1.0, NULL, 'rule_fire', 'ok', NULL)",
            "INSERT INTO write_audit_log (id, rule_name_snapshot, actor_username, action, result) \
             VALUES (9, 'arm', 'alice', 'arm', 'ok')",
        ] {
            sqlx::query(sql).execute(pool).await.unwrap();
        }
    }

    /// The 0014 upgrade path against a POPULATED pre-tag-monitor database:
    /// the leaf rebuild must widen the action CHECK to accept 'manual_write'
    /// without losing a row (`id`/`ts` byte-for-byte - audit history), must
    /// keep both indexes, and a second run must detect the widened CHECK in
    /// sqlite_master and no-op.
    #[tokio::test]
    async fn manual_write_upgrade_widens_the_action_check_on_a_populated_db() {
        let pool = banto_storage::connect_sqlite_memory().await.unwrap();
        seed_pre_manual_write_audit_log(&pool).await;

        // The old CHECK really does reject manual_write before the upgrade.
        let rejected = sqlx::query(
            "INSERT INTO write_audit_log (rule_name_snapshot, action, result) \
             VALUES ('手動書き込み', 'manual_write', 'ok')",
        )
        .execute(&pool)
        .await;
        assert!(
            rejected.is_err(),
            "pre-upgrade CHECK must reject manual_write"
        );

        run_migrations(&pool).await.expect("upgrade should apply");

        // Every row survived, ids and ts intact.
        let rows: Vec<(i64, String, String, String)> =
            sqlx::query_as("SELECT id, ts, action, result FROM write_audit_log ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0],
            (
                5,
                "2026-01-02 03:04:05".into(),
                "rule_fire".into(),
                "ok".into()
            )
        );
        assert_eq!(rows[1].0, 9);
        assert_eq!(rows[1].2, "arm");

        // The widened CHECK accepts manual_write now (and the engine enum's
        // wire string matches it).
        sqlx::query(
            "INSERT INTO write_audit_log (rule_name_snapshot, actor_username, action, result) \
             VALUES ('手動書き込み', 'debugger', ?, 'ok')",
        )
        .bind(crate::engine::write_audit::AuditAction::ManualWrite.as_str())
        .execute(&pool)
        .await
        .expect("'manual_write' must pass the rebuilt CHECK");

        // Both indexes were recreated under their original names.
        for index in [
            "idx_write_audit_log_ts",
            "idx_write_audit_log_write_rule_id",
        ] {
            let exists: Option<String> = sqlx::query_scalar(
                "SELECT name FROM sqlite_master WHERE type = 'index' AND name = ?",
            )
            .bind(index)
            .fetch_optional(&pool)
            .await
            .unwrap();
            assert!(exists.is_some(), "expected index '{index}' to exist");
        }

        // Second run: the sqlite_master detection sees 'manual_write' and
        // no-ops (row count unchanged).
        run_migrations(&pool).await.expect("re-run must be a no-op");
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM write_audit_log")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 3);
    }
}
