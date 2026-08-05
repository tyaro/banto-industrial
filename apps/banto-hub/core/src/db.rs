//! Database bootstrap for banto-hub (docs/tag-server-design.md §3.2 table
//! "タグ定義・CRUD | banto-tags | サーバー自身の SQLite に同居"): connect,
//! apply this app's own schema (settings/users/audit_log), then
//! `banto_tags::migrate` (I1's PLC connection/collection group/tag registry
//! tables), then `banto_collect::migrate` (I3b's `collect_events` table) -
//! all three against the SAME pool. banto-hub shares one SQLite database
//! across its own tables and every I-series crate's tables, exactly like
//! ChronoGazer (`apps/chronogazer/core/src/db.rs`, which this module is
//! copied from almost verbatim) - this is the one place that bootstraps the
//! whole schema.
//!
//! This app's own tables are applied as plain **idempotent DDL** (`CREATE
//! TABLE IF NOT EXISTS` etc.), not through `sqlx::migrate!`: **only
//! `banto_tags::migrate` may use `sqlx::migrate!` against this pool.**
//! `sqlx`'s migration bookkeeping table (`_sqlx_migrations`) is a single,
//! database-wide table with no per-crate namespacing (`sqlx` 0.8 has no
//! `Migrator::set_table_name`), so a second independent `sqlx::migrate!`
//! source sharing this pool would collide on overlapping version numbers -
//! this is exactly why `banto_collect::migrate` itself applies its one table
//! (`collect_events`) via `CREATE TABLE IF NOT EXISTS` rather than its own
//! migrator (see `banto-collect`'s `lib.rs` doc comment), and why this
//! module's own schema does the same. See `apps/chronogazer/core/src/db.rs`
//! for the fuller writeup of the empirically-confirmed
//! `MigrateError::VersionMismatch` this avoids.

use banto_core::BantoError;
use sqlx::SqlitePool;

/// Connect to the SQLite database at `path` and apply the full schema (this
/// app's own, then `banto_tags`, then `banto_collect`). Used by
/// `bin/banto-hub.rs` with a path under the app's data directory.
pub async fn init_db(path: impl AsRef<std::path::Path>) -> Result<SqlitePool, BantoError> {
    let pool = banto_storage::connect_sqlite(path).await?;
    run_migrations(&pool).await?;
    Ok(pool)
}

/// Same as [`init_db`] but against a private in-memory database. Used by
/// unit tests so each test gets an isolated, fully-migrated database.
///
/// NOTE: `banto-collect`'s own registry/config-build tests require a
/// *file-backed* database (its pool hands out multiple connections and each
/// `:memory:` connection is a separate empty database - see
/// `crates/banto-collect/tests/integration.rs`'s module doc). This function
/// is fine for this crate's own unit tests (services layer only, single
/// connection at a time via `sqlx::SqlitePool`'s default pooling behavior
/// against `:memory:` - `banto_storage::connect_sqlite_memory` pins this to
/// one connection internally), but the T0-1 E2E integration test
/// (`tests/integration.rs`) uses [`init_db`] against a real temp file
/// instead, exactly like `banto-collect`'s own integration tests.
pub async fn init_db_memory() -> Result<SqlitePool, BantoError> {
    let pool = banto_storage::connect_sqlite_memory().await?;
    run_migrations(&pool).await?;
    Ok(pool)
}

/// Same as [`init_db_memory`], `pub(crate)` for `rest.rs`'s test module -
/// mirrors chronogazer's `migrate_memory` naming.
#[cfg(test)]
pub(crate) async fn migrate_memory() -> Result<SqlitePool, BantoError> {
    let pool = banto_storage::connect_sqlite_memory().await?;
    run_migrations(&pool).await?;
    Ok(pool)
}

async fn run_migrations(pool: &SqlitePool) -> Result<(), BantoError> {
    apply_app_schema(pool).await?;
    // I1: banto-tags owns its own migrations/ directory and is applied here,
    // right after this app's own schema - see banto_tags::migrate's doc
    // comment for why it is designed to be called this way, and this
    // module's own doc comment for why THIS app's half is deliberately NOT
    // also a `sqlx::migrate!` source.
    banto_tags::migrate(pool).await?;
    // I3b: banto-collect's one table (`collect_events`), applied via its own
    // idempotent `CREATE TABLE IF NOT EXISTS` for the same shared-migrator
    // reason as above (see banto-collect's `lib.rs` doc comment).
    banto_collect::migrate(pool)
        .await
        .map_err(|err| BantoError::Storage(err.to_string()))?;
    Ok(())
}

/// This app's own tables, applied as idempotent DDL - see this module's doc
/// comment for why. Mirrors ChronoGazer's `settings`/`users`/`audit_log`
/// shape exactly (same schema, same reasoning): banto-hub has no product
/// requirement that differs here, so there is no value in inventing a
/// different shape.
async fn apply_app_schema(pool: &SqlitePool) -> Result<(), BantoError> {
    sqlx::query("CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
        .execute(pool)
        .await
        .map_err(banto_storage::storage_error)?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS users (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            username TEXT NOT NULL UNIQUE,
            password_hash TEXT NOT NULL,
            display_name TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            role TEXT NOT NULL DEFAULT 'admin' CHECK (role IN ('admin','editor','viewer'))
        )",
    )
    .execute(pool)
    .await
    .map_err(banto_storage::storage_error)?;

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

    // T0-2 (docs/tag-server-design.md §5.6): /api/v1/* の機械クライアント
    // 認証用 API キー。列の意味は `crate::api_keys` のモジュール doc 参照
    // (特に `last_used_at` の保存形式は同モジュールの判断で epoch ミリ秒の
    // 10進文字列 - `created_at`/`revoked_at` の ISO 日時文字列とは異なる)。
    // 失効は物理削除でなく `revoked_at` を立てるだけ(履歴を残す方針、同
    // モジュール参照)。
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS api_keys (
          id INTEGER PRIMARY KEY AUTOINCREMENT,
          name TEXT NOT NULL UNIQUE,
          prefix TEXT NOT NULL UNIQUE,
          key_hash TEXT NOT NULL,
          scopes TEXT NOT NULL,
          created_at TEXT NOT NULL DEFAULT (datetime('now')),
          last_used_at TEXT,
          revoked_at TEXT
        )",
    )
    .execute(pool)
    .await
    .map_err(banto_storage::storage_error)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end proof that `init_db_memory` applies this app's own schema
    /// AND `banto_tags`'s AND `banto_collect`'s against the same pool - the
    /// T0-1 integration point this module exists to wire up.
    #[tokio::test]
    async fn init_db_memory_applies_all_three_schemas() {
        let pool = init_db_memory()
            .await
            .expect("init_db_memory should succeed");
        for table in [
            "settings",
            "users",
            "audit_log",
            "plc_connections",
            "collection_groups",
            "tags",
            "collect_events",
            "api_keys",
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
    }

    /// Running schema application twice against the same pool must not
    /// error - both this app's idempotent DDL, `banto_tags::migrate`'s
    /// bookkeeping table, and `banto_collect::migrate`'s idempotent DDL all
    /// tolerate being called again.
    #[tokio::test]
    async fn schema_application_is_idempotent_across_two_runs_on_the_same_db() {
        let pool = banto_storage::connect_sqlite_memory().await.unwrap();
        run_migrations(&pool).await.unwrap();
        run_migrations(&pool).await.unwrap();
    }
}
