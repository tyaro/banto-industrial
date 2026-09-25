//! Database bootstrap for the chronogazer app (spec §12): connect, apply
//! this app's own schema, then `banto_tags::migrate` (I1's PLC connection/
//! collection group/tag registry tables), then `banto_collect::migrate`
//! (I3b's `collect_events` table, #383 段階2b) against the SAME pool -
//! ChronoGazer shares one SQLite database across the app's own tables
//! (settings/users/audit_log) and every I-series crate's tables (plan.md
//! §5: "single app-data file"), so this is the one place that bootstraps
//! the whole schema.
//!
//! This app's own tables are applied as plain **idempotent DDL**
//! (`CREATE TABLE IF NOT EXISTS` etc.) rather than through
//! `sqlx::migrate!` - unlike the banto template
//! (`apps/admin-template/core/src/db.rs`, which this module started
//! from), this app is NOT the only thing
//! running a `sqlx::migrate!`-based migrator against its pool:
//! `banto_tags::migrate` below runs its OWN embedded `sqlx::migrate!` on
//! the identical database. `sqlx`'s migration bookkeeping table
//! (`_sqlx_migrations`) is a single, database-wide table with no way to
//! namespace it per crate (`sqlx` 0.8 has no `Migrator::set_table_name`),
//! so two independent `sqlx::migrate!` sources sharing one pool collide on
//! overlapping version numbers - empirically confirmed here as
//! `MigrateError::VersionMismatch`/`VersionMissing` on every single
//! `init_db`/`init_db_memory` call once `banto_tags::migrate` was wired
//! in. `crates/banto-collect/Cargo.toml` documents the identical
//! constraint (its `collect_events` table for the same reason); this
//! module is the app-level version of that same deviation - see
//! `docs/r1a-readme-gaps.md` for the full writeup. The `migrations/*.sql`
//! files in this crate remain as schema documentation/history; they are
//! no longer executed by `sqlx::migrate!` - [`apply_app_schema`] below is
//! the actual source of truth and must be kept in sync with them by hand.

use banto_core::BantoError;
use sqlx::SqlitePool;

/// The one SQLite pool type every service in this crate is built over,
/// re-exported so downstream crates (notably `src-tauri`, whose invariant is
/// to add NO new dependencies of its own) can name it - e.g. to hold the
/// pool in their own app state so it can be `close()`d on exit - without
/// taking a direct `sqlx` dependency. Same precedent (and same wording) as
/// `relay_wright_core::db::DbPool`.
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
    // I3b / #383 段階2b（R1-C）: `banto-collect` の `collect_events` テーブル。
    // `crate::collect::CollectorService` が持つ `EventSink` の**永続側**の
    // 出力先で、live broadcast と対になっている。`EventSink::emit` は
    // insert の失敗を握り潰す（収集を止めないため）ので、ここで作っておかないと
    // イベントは黙って消える - `banto_collect::migrate` の doc が
    // 「consuming app が `banto_tags::migrate` の後に 1 回呼ぶ」と定めている
    // のがまさにこの位置で、`apps/banto-hub/core/src/db.rs` も同じ順序で
    // 呼んでいる。`sqlx::migrate!` ではなく冪等 DDL なので、このモジュールの
    // doc が言う migrator の衝突も起こさない。
    banto_collect::migrate(pool)
        .await
        .map_err(|err| BantoError::Other(err.to_string()))?;
    Ok(())
}

/// This app's own tables, applied as idempotent DDL - see this module's
/// doc comment for why. Mirrors `migrations/0001_settings.sql` through
/// `migrations/0004_audit_log.sql` exactly; update both together.
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

    // 0005_user_auth_epoch.sql（banto v1.7.0 #204 のセッション失効）: 同じ
    // `pragma_table_info` の冪等チェック。既存の行は 0 から始まる（セッションは
    // プロセスのメモリにしか無いので、既存セッションの移行は要らない）。
    let has_auth_epoch_column: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('users') WHERE name = 'auth_epoch'",
    )
    .fetch_one(pool)
    .await
    .map_err(banto_storage::storage_error)?;
    if has_auth_epoch_column == 0 {
        sqlx::query("ALTER TABLE users ADD COLUMN auth_epoch INTEGER NOT NULL DEFAULT 0")
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
    /// schema (`settings`/`users`/`audit_log`, including the `role` column)
    /// AND `banto_tags`'s (`plc_connections`/`collection_groups`/`tags`)
    /// against the same pool - the R1-A integration point this module
    /// exists to wire up.
    #[tokio::test]
    async fn init_db_memory_applies_both_this_apps_and_banto_tags_schema() {
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
            // #383 段階2b / R1-C: `banto_collect::migrate` の分。
            "collect_events",
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

    /// banto v1.7.0 #204: `auth_epoch` の無い既存の DB（v1.6.0 追従時点の
    /// スキーマ = 0002 + 0003 だけの `users`）から起動しても、既存の行が
    /// 世代 0 で読め、ログインと世代の更新が動くこと。
    #[tokio::test]
    async fn auth_epoch_is_added_to_an_existing_users_table() {
        let pool = banto_storage::connect_sqlite_memory().await.unwrap();
        sqlx::query(
            "CREATE TABLE users (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                username TEXT NOT NULL UNIQUE,
                password_hash TEXT NOT NULL,
                display_name TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "ALTER TABLE users ADD COLUMN role TEXT NOT NULL DEFAULT 'admin' \
             CHECK (role IN ('admin','editor','viewer'))",
        )
        .execute(&pool)
        .await
        .unwrap();
        // 旧スキーマのまま作られていたアカウント（旧版のハッシュ形式と同じ
        // argon2id の PHC 文字列）。
        let users = crate::users::UsersService::new(pool.clone());
        let hash = {
            use argon2::password_hash::{rand_core::OsRng, PasswordHasher, SaltString};
            argon2::Argon2::default()
                .hash_password(b"old-password", &SaltString::generate(&mut OsRng))
                .unwrap()
                .to_string()
        };
        sqlx::query(
            "INSERT INTO users (username, password_hash, display_name, role) \
             VALUES ('legacy', ?, 'Legacy', 'editor')",
        )
        .bind(&hash)
        .execute(&pool)
        .await
        .unwrap();

        run_migrations(&pool).await.unwrap();
        run_migrations(&pool).await.unwrap(); // 2 回目も列を重複させない

        let legacy = users
            .verify("legacy", "old-password")
            .await
            .unwrap()
            .expect("既存のアカウントでログインできる");
        assert_eq!(legacy.auth_epoch, 0);
        let new_epoch = users
            .change_password("legacy", "old-password", "new-password")
            .await
            .unwrap();
        assert_eq!(new_epoch, 1);
    }
}
