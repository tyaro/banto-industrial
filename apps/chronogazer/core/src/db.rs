//! Database bootstrap for the chronogazer app (spec §12): connect, refuse a
//! database in the pre-2026-10-02 format, then apply this app's own migrations
//! (`migrations-sqlite/`), then `banto_tags::migrate` (the PLC connection/
//! collection group/tag registry tables), then `banto_collect::migrate`
//! (`collect_events`, #383 段階2b) against the SAME pool - ChronoGazer shares
//! one SQLite database across the app's own tables (settings/users/
//! audit_log) and every I-series crate's tables (plan.md §5: "single
//! app-data file"), so this is the one place that bootstraps the whole
//! schema.
//!
//! ## スキーマの出所（DB スキーマの整理、2026-10-02 オーナー決定）
//!
//! この app 自身のテーブルは banto の admin-template と**同じ形**にする。
//! `migrations-sqlite/` の 5 本は、当面
//! `banto v2.1.1 apps/admin-template/core/migrations-sqlite/<同じファイル名>`
//! を **byte 等価**でコピーしたもの（banto 側には手を入れない。番号の飛び -
//! `0001_items`・`0006_attachments` が無い - は上流の番号をそのまま残している
//! ため）。ファイルの中身を書き換えないこと: 上流と食い違うと「banto に寄せる」
//! 土台にならず、`sqlx` の checksum も変わる。上流を上げるときは同じ名前で
//! コピーし直す。
//!
//! ## なぜ `sqlx::migrate!` に戻せたか
//!
//! 以前はこの app の分を手書きの冪等 DDL で作っていた。`sqlx` 0.8 の
//! migration 記録テーブル（`_sqlx_migrations`）は DB に 1 つで名前を変えられず、
//! 同じ pool で `banto_tags::migrate` の `sqlx::migrate!` と番号が衝突した
//! （`MigrateError::VersionMismatch`、docs/r1a-readme-gaps.md）ため。
//! `sqlx` 0.9 には `Migrator::dangerous_set_table_name` があるので、記録
//! テーブルを migrator ごとに分けて衝突を無くした:
//!
//! - この app: [`MIGRATIONS_TABLE`]（`_sqlx_migrations_chronogazer`）
//! - `banto_tags::migrate`: `_sqlx_migrations_banto_tags`
//! - `banto_collect::migrate`: 記録テーブルを使わない（冪等 DDL のまま）
//!
//! `dangerous_` が警告しているのは「既存 DB の記録を見失い、適用済みの
//! migration を流し直そうとする」こと。**既存の DB は壊してよい**（アルファ版。
//! 2026-10-02 オーナー決定）ので受け入れた。その代わり、古い形式の DB は
//! 黙って流さずに起動を拒否する（次節）。
//!
//! ## 旧形式の DB の拒否
//!
//! [`init_db`] は接続直後・migrate の前に、「この app の記録テーブル
//! （[`MIGRATIONS_TABLE`]）が無いのに `users` または `settings` がある」DB を
//! 旧形式とみなし、[`InitDbError::Legacy`] を返す（自動移行はしない）。
//! そのまま流すと `CREATE TABLE users` が「既にある」で落ち、何が起きたか
//! 分からないため。新規の空ファイルと新しい形式の DB は通る。呼び出し側
//! （`src-tauri` の起動・`bin/banto-serve.rs`）はメッセージを stderr に出して
//! 終了する。作り直しの手順は `apps/chronogazer/README.md` 参照。

use banto_core::BantoError;
use sqlx::SqlitePool;
use std::fmt;
use std::path::{Path, PathBuf};

/// The one SQLite pool type every service in this crate is built over,
/// re-exported so downstream crates (notably `src-tauri`, whose invariant is
/// to add NO new dependencies of its own) can name it - e.g. to hold the
/// pool in their own app state so it can be `close()`d on exit - without
/// taking a direct `sqlx` dependency. Same precedent (and same wording) as
/// `relay_wright_core::db::DbPool`.
pub type DbPool = SqlitePool;

/// この app の migration 記録テーブルの名前（モジュール doc「なぜ
/// `sqlx::migrate!` に戻せたか」）。旧形式の判定にも使う。
pub const MIGRATIONS_TABLE: &str = "_sqlx_migrations_chronogazer";

/// 旧形式（2026-10-02 のスキーマ整理より前）の DB を開こうとした（モジュール doc「旧形式の DB の
/// 拒否」）。`Display` がそのまま利用者向けの説明になる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyDatabase {
    /// 拒否した DB ファイルのパス。
    pub path: PathBuf,
}

impl fmt::Display for LegacyDatabase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "この DB は旧形式です。{} を削除または退避して起動し直してください\
             （アルファ版のため自動移行はありません）",
            self.path.display()
        )
    }
}

impl std::error::Error for LegacyDatabase {}

/// [`init_db`] の失敗。旧形式の DB（[`InitDbError::Legacy`]）と、それ以外の
/// 接続・migration の失敗（[`InitDbError::Storage`]）を呼び出し側で分けられる
/// ようにする。
#[derive(Debug)]
pub enum InitDbError {
    /// 旧形式の DB。起動を拒否する。
    Legacy(LegacyDatabase),
    /// 接続または migration の失敗。
    Storage(BantoError),
}

impl fmt::Display for InitDbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Legacy(legacy) => legacy.fmt(f),
            Self::Storage(err) => err.fmt(f),
        }
    }
}

impl std::error::Error for InitDbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Legacy(legacy) => Some(legacy),
            Self::Storage(err) => Some(err),
        }
    }
}

impl From<BantoError> for InitDbError {
    fn from(err: BantoError) -> Self {
        Self::Storage(err)
    }
}

/// `BantoError` で受ける呼び出し側のため。旧形式は説明文ごと
/// `BantoError::Other` に入れる。
impl From<InitDbError> for BantoError {
    fn from(err: InitDbError) -> Self {
        match err {
            InitDbError::Legacy(legacy) => BantoError::Other(legacy.to_string()),
            InitDbError::Storage(err) => err,
        }
    }
}

/// Connect to the SQLite database at `path`, refuse it if it is in the
/// pre-2026-10-02 format, and apply the full schema (this app's own, then
/// `banto_tags`'s, then `banto_collect`'s). Used by the `src-tauri` adapter
/// with a path under the app's data directory.
pub async fn init_db(path: impl AsRef<Path>) -> Result<SqlitePool, InitDbError> {
    let path = path.as_ref();
    let pool = banto_storage::connect_sqlite(path).await?;
    if is_legacy_schema(&pool).await? {
        pool.close().await;
        return Err(InitDbError::Legacy(LegacyDatabase {
            path: path.to_path_buf(),
        }));
    }
    run_migrations(&pool).await?;
    Ok(pool)
}

/// Same as [`init_db`] but against a private in-memory database. Used by
/// tests so each test gets an isolated, fully-migrated database (always
/// empty, so the old-format check is not needed).
pub async fn init_db_memory() -> Result<SqlitePool, BantoError> {
    let pool = banto_storage::connect_sqlite_memory().await?;
    run_migrations(&pool).await?;
    Ok(pool)
}

/// Same as [`init_db_memory`], `pub(crate)` for `rest.rs`'s test module -
/// kept as a separate name (rather than just reusing `init_db_memory`
/// directly) since several call sites already spell it this way.
#[cfg(test)]
pub(crate) async fn migrate_memory() -> Result<SqlitePool, BantoError> {
    let pool = banto_storage::connect_sqlite_memory().await?;
    run_migrations(&pool).await?;
    Ok(pool)
}

/// 「この app の記録テーブルが無いのに `users` か `settings` がある」か
/// （モジュール doc「旧形式の DB の拒否」）。
async fn is_legacy_schema(pool: &SqlitePool) -> Result<bool, BantoError> {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name IN (?, 'users', 'settings')",
    )
    .bind(MIGRATIONS_TABLE)
    .fetch_all(pool)
    .await
    .map_err(banto_storage::storage_error)?;
    let has_record = tables.iter().any(|name| name == MIGRATIONS_TABLE);
    Ok(!has_record && !tables.is_empty())
}

async fn run_migrations(pool: &SqlitePool) -> Result<(), BantoError> {
    // この app 自身の分（admin-template のコピー）。記録テーブルを分ける理由は
    // モジュール doc 参照。
    let mut migrator = sqlx::migrate!("./migrations-sqlite");
    migrator.dangerous_set_table_name(MIGRATIONS_TABLE);
    migrator
        .run(pool)
        .await
        .map_err(|err| BantoError::Storage(err.to_string()))?;
    // I1 (docs/plan.md): banto-tags owns its own migrations/ directory and
    // is applied here, right after this app's own schema, so every caller
    // of init_db/init_db_memory gets the full schema in one call. It records
    // into its own `_sqlx_migrations_banto_tags`, so it no longer collides
    // with this app's migrator.
    banto_tags::migrate(pool).await?;
    // I3b / #383 段階2b（R1-C）: `banto-collect` の `collect_events` テーブル。
    // `crate::collect::CollectorService` が持つ `EventSink` の**永続側**の
    // 出力先で、live broadcast と対になっている。`EventSink::emit` は
    // insert の失敗を握り潰す（収集を止めないため）ので、ここで作っておかないと
    // イベントは黙って消える - `banto_collect::migrate` の doc が
    // 「consuming app が `banto_tags::migrate` の後に 1 回呼ぶ」と定めている
    // のがまさにこの位置で、`apps/banto-hub/core/src/db.rs` も同じ順序で
    // 呼んでいる。`sqlx::migrate!` ではなく冪等 DDL なので記録テーブルは無い。
    banto_collect::migrate(pool)
        .await
        .map_err(|err| BantoError::Other(err.to_string()))?;
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

    /// 2 回流しても失敗しない（どの migrator も自分の記録テーブルで適用済みを
    /// 覚えている。`banto_collect` は冪等 DDL）。
    #[tokio::test]
    async fn schema_application_is_idempotent_across_two_runs_on_the_same_db() {
        let pool = banto_storage::connect_sqlite_memory().await.unwrap();
        run_migrations(&pool).await.unwrap();
        run_migrations(&pool).await.unwrap(); // second run: must not error
    }

    /// スキーマ整理（2026-10-02）: 記録テーブルは migrator ごとに分かれ、共有の `_sqlx_migrations`
    /// は作られない。この app の分は上流 admin-template の 5 本。
    #[tokio::test]
    async fn migrations_are_recorded_in_per_migrator_tables() {
        let pool = init_db_memory().await.unwrap();
        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE '\\_sqlx%' \
             ESCAPE '\\' ORDER BY name",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            tables,
            vec![
                "_sqlx_migrations_banto_tags".to_string(),
                MIGRATIONS_TABLE.to_string(),
            ]
        );
        let versions: Vec<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT version FROM {MIGRATIONS_TABLE} ORDER BY version"
        )))
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(versions, vec![2, 3, 4, 5, 7]);
        let has_auth_epoch: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('users') WHERE name = 'auth_epoch'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(has_auth_epoch, 1);
    }

    /// 新規の空ファイルは通り、2 回目の起動（同じファイル）も migrate が
    /// 冪等で、1 回目に書いた行が残る。
    #[tokio::test]
    async fn a_new_file_passes_and_the_second_startup_is_idempotent() {
        let dir = crate::test_support::TempDir::new();
        let db_path = dir.path().join("chronogazer.sqlite3");

        let pool = init_db(&db_path).await.expect("a new file must pass");
        sqlx::query("INSERT INTO settings (key, value) VALUES ('probe', 'kept')")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;

        let pool = init_db(&db_path)
            .await
            .expect("the new-format DB must pass on the second startup");
        let value: String = sqlx::query_scalar("SELECT value FROM settings WHERE key = 'probe'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(value, "kept");
        pool.close().await;
    }

    /// 旧形式（記録テーブルが無いのに `users` か `settings` がある）は
    /// [`InitDbError::Legacy`] で拒否され、DB には何も足されない。
    #[tokio::test]
    async fn a_legacy_database_is_refused_without_being_touched() {
        for legacy_table in [
            "CREATE TABLE users (id INTEGER PRIMARY KEY, username TEXT NOT NULL)",
            "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        ] {
            let dir = crate::test_support::TempDir::new();
            let db_path = dir.path().join("chronogazer.sqlite3");
            let pool = banto_storage::connect_sqlite(&db_path).await.unwrap();
            sqlx::query(legacy_table).execute(&pool).await.unwrap();
            pool.close().await;

            let err = init_db(&db_path)
                .await
                .expect_err("a legacy DB must be refused");
            match &err {
                InitDbError::Legacy(legacy) => assert_eq!(legacy.path, db_path),
                other => panic!("expected InitDbError::Legacy, got {other:?}"),
            }
            let message = err.to_string();
            assert!(message.contains("旧形式"), "{message}");
            assert!(
                message.contains(&db_path.display().to_string()),
                "{message}"
            );

            let pool = banto_storage::connect_sqlite(&db_path).await.unwrap();
            let tables: Vec<String> = sqlx::query_scalar(
                "SELECT name FROM sqlite_master WHERE type = 'table' \
                 AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )
            .fetch_all(&pool)
            .await
            .unwrap();
            assert_eq!(tables.len(), 1, "nothing may be added: {tables:?}");
            pool.close().await;
        }
    }
}
