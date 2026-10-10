//! Database bootstrap for banto-hub (docs/tag-server-design.md §3.2 table
//! "タグ定義・CRUD | banto-tags | サーバー自身の SQLite に同居"): connect,
//! refuse a database in the pre-2026-10-02 format, then apply this app's own
//! migrations (`migrations-sqlite/`), then `banto_tags::migrate` (I1's PLC
//! connection/collection group/tag registry tables), then
//! `banto_collect::migrate` (I3b's `collect_events` table) - all three against
//! the SAME pool. banto-hub shares one SQLite database across its own tables
//! and every I-series crate's tables, exactly like ChronoGazer
//! (`apps/chronogazer/core/src/db.rs`) - this is the one place that
//! bootstraps the whole schema.
//!
//! ## スキーマの出所（DB スキーマの整理、2026-10-02 オーナー決定）
//!
//! `migrations-sqlite/` は 2 種類:
//!
//! - `0002`〜`0008`（settings/users/user_roles/audit_log/user_auth_epoch/
//!   audit_log_pending_id の 6 本）は、当面
//!   `banto apps/admin-template/core/migrations-sqlite/<同じファイル名>`
//!   を **byte 等価**でコピーしたもの（`0002`〜`0007` は v2.1.1、`0008` は
//!   v6.5.0 = 監査の保留の `audit_log.pending_id`、banto ADR-0019・#437。
//!   banto 側には手を入れない。番号の飛び - `0001_items`・`0006_attachments`
//!   が無い - は上流の番号をそのまま残しているため）。中身を書き換えないこと:
//!   上流と食い違うと「banto に寄せる」土台にならず、`sqlx` の checksum も
//!   変わる。上流を上げるときは同じ名前でコピーし直す。ChronoGazer と同じ 6 本。
//!   `0008` は `0101`〜`0106` を適用済みの既存 DB には「若い番号が後から来る」
//!   形になるが、`sqlx` 0.9 の `Migrator::run` は適用済みに無い版を流すだけで
//!   順序の逆転を拒まない（テスト
//!   `an_existing_db_without_0008_gets_it_on_the_next_startup`）。
//! - `0101_*` 以降は banto-hub 固有のテーブル（api_keys・write_control_state
//!   と seed・hub_write_audit・hub_retained_values・pending_changes・
//!   hub_sink_groups/hub_sink_group_tags）。スキーマの整理（2026-10-02）で、それまでの冪等 DDL と後追いの
//!   `ADD COLUMN`（`api_keys.tripped_at`/`expires_at`・
//!   `hub_write_audit.value_requested_text`・`pending_changes.base_fingerprint`）
//!   を反映した**最終形**で書き直した。列の意味は各ファイルの先頭コメントと、
//!   それぞれを扱うモジュールの doc 参照。上流の番号と重ならないよう 0101 から
//!   始める。今後の列追加は新しい番号のファイルで足す（既存のファイルは
//!   書き換えない）。
//!
//! ## なぜ `sqlx::migrate!` に戻せたか
//!
//! 以前はこの app の分を手書きの冪等 DDL で作っていた。`sqlx` 0.8 の
//! migration 記録テーブル（`_sqlx_migrations`）は DB に 1 つで名前を変えられず、
//! 同じ pool で `banto_tags::migrate` の `sqlx::migrate!` と番号が衝突する
//! （`MigrateError::VersionMismatch`、docs/r1a-readme-gaps.md）ため。
//! `sqlx` 0.9 には `Migrator::dangerous_set_table_name` があるので、記録
//! テーブルを migrator ごとに分けて衝突を無くした:
//!
//! - この app: [`MIGRATIONS_TABLE`]（`_sqlx_migrations_banto_hub`）
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
//! 分からないため。新規の空ファイルと新しい形式の DB は通る。
//! `crate::runtime::HubRuntime::start` はこれを `HubStartError::LegacyDatabase`
//! にして既存の起動失敗のログ経路（コンソールの stderr・サービスのログ）に
//! 乗せる。作り直しの操作は作らず、手順を docs/banto-hub-operations.md に
//! 書くだけにした（2026-10-02 オーナー決定）。

use banto_core::BantoError;
use sqlx::SqlitePool;
use std::fmt;
use std::path::{Path, PathBuf};

/// banto のサービス（`banto_admin_services` の users/settings/audit。I3' で
/// 自前のコピーから置き換えた）が受け取る、バックエンドを問わない接続
/// ハンドル。banto-hub は SQLite 固定なので、いつも `Db::Sqlite(pool.clone())`
/// で組む（`sqlx` の pool は `Arc` なので、同じ pool を共有する）。
/// `banto_hub_core::db::Db` として引けるよう re-export する（ChronoGazer の
/// `db.rs` と同じ）。
pub use banto_storage::Db;

/// この app の migration 記録テーブルの名前（モジュール doc「なぜ
/// `sqlx::migrate!` に戻せたか」）。旧形式の判定にも使う。
pub const MIGRATIONS_TABLE: &str = "_sqlx_migrations_banto_hub";

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
             （アルファ版のため自動移行はありません。手順は docs/banto-hub-operations.md）",
            self.path.display()
        )
    }
}

impl std::error::Error for LegacyDatabase {}

/// [`init_db`] の失敗。旧形式の DB（[`InitDbError::Legacy`]）と、それ以外の
/// 接続・migration の失敗（[`InitDbError::Storage`]）を呼び出し側で分けられる
/// ようにする。
#[derive(Debug, thiserror::Error)]
pub enum InitDbError {
    /// 旧形式の DB。起動を拒否する。
    #[error(transparent)]
    Legacy(#[from] LegacyDatabase),
    /// 接続または migration の失敗。
    #[error(transparent)]
    Storage(#[from] BantoError),
}

/// `BantoError` で受ける呼び出し側（`crate::service_elevated` 等）のため。
/// 旧形式は説明文ごと `BantoError::Other` に入れる。
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
/// `banto_tags`, then `banto_collect`). Used by `bin/banto-hub.rs` with a
/// path under the app's data directory.
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
/// unit tests so each test gets an isolated, fully-migrated database (always
/// empty, so the old-format check is not needed).
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
    // この app 自身の分（admin-template のコピー + 0101 以降の Hub 固有）。
    // 記録テーブルを分ける理由はモジュール doc 参照。
    let mut migrator = sqlx::migrate!("./migrations-sqlite");
    migrator.dangerous_set_table_name(MIGRATIONS_TABLE);
    migrator
        .run(pool)
        .await
        .map_err(|err| BantoError::Storage(err.to_string()))?;
    // I1: banto-tags owns its own migrations/ directory and is applied here,
    // right after this app's own schema. It records into its own
    // `_sqlx_migrations_banto_tags`, so it no longer collides with this
    // app's migrator.
    banto_tags::migrate(pool).await?;
    // I3b: banto-collect's one table (`collect_events`), applied via its own
    // idempotent `CREATE TABLE IF NOT EXISTS` (no bookkeeping table - see
    // banto-collect's `lib.rs` doc comment).
    banto_collect::migrate(pool)
        .await
        .map_err(|err| BantoError::Storage(err.to_string()))?;
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
            "write_control_state",
            "hub_write_audit",
            "hub_retained_values",
            "pending_changes",
            "hub_sink_groups",
            "hub_sink_group_tags",
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

    /// 2 回流しても失敗しない（どの migrator も自分の記録テーブルで適用済みを
    /// 覚えている。`banto_collect` は冪等 DDL）。
    #[tokio::test]
    async fn schema_application_is_idempotent_across_two_runs_on_the_same_db() {
        let pool = banto_storage::connect_sqlite_memory().await.unwrap();
        run_migrations(&pool).await.unwrap();
        run_migrations(&pool).await.unwrap();
    }

    /// スキーマ整理（2026-10-02）: 記録テーブルは migrator ごとに分かれ、共有の `_sqlx_migrations`
    /// は作られない。この app の分は上流 admin-template の 6 本 + Hub 固有。
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
                MIGRATIONS_TABLE.to_string(),
                "_sqlx_migrations_banto_tags".to_string(),
            ]
        );
        let versions: Vec<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT version FROM {MIGRATIONS_TABLE} ORDER BY version"
        )))
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            versions,
            vec![2, 3, 4, 5, 7, 8, 101, 102, 103, 104, 105, 106]
        );
    }

    /// スキーマ整理（2026-10-02）: 以前は後追いの `ADD COLUMN` で足していた列が、新しい DB では
    /// 最初から（`CREATE TABLE` の最終形として）ある。
    #[tokio::test]
    async fn formerly_added_columns_are_part_of_the_final_shape() {
        let pool = init_db_memory().await.unwrap();
        for (table, column) in [
            ("users", "role"),
            ("users", "auth_epoch"),
            ("api_keys", "tripped_at"),
            ("api_keys", "expires_at"),
            ("hub_write_audit", "value_requested_text"),
            ("pending_changes", "base_fingerprint"),
        ] {
            let count: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM pragma_table_info(?) WHERE name = ?")
                    .bind(table)
                    .bind(column)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(count, 1, "expected {table}.{column} to exist");
        }
    }

    /// T2-4 (§6-6)。2026-09-09 オーナー決定 (#340): `write_control_state` は
    /// id=1 の1行を seed し、`enabled_persisted = 1` (既定「書き込み可」) から
    /// 始まる - `crate::write_control::WriteControl` がこの値をそのまま
    /// ライブフラグへ復元する前提。
    #[tokio::test]
    async fn write_control_state_seeds_a_single_enabled_row() {
        let pool = init_db_memory().await.unwrap();
        let enabled: i64 =
            sqlx::query_scalar("SELECT enabled_persisted FROM write_control_state WHERE id = 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(enabled, 1);
    }

    /// 新規の空ファイルは通り、2 回目の起動（同じファイル）も migrate が
    /// 冪等で、1 回目に書いた行（運用者が止めた書き込み受付を含む）が残る -
    /// seed が二度流れて上書きしないこと。
    #[tokio::test]
    async fn a_new_file_passes_and_the_second_startup_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("registry.sqlite3");

        let pool = init_db(&db_path).await.expect("a new file must pass");
        sqlx::query(
            "UPDATE write_control_state SET enabled_persisted = 0, \
             last_changed_at = datetime('now'), last_changed_by = 'admin' WHERE id = 1",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;

        let pool = init_db(&db_path)
            .await
            .expect("the new-format DB must pass on the second startup");
        let enabled: i64 =
            sqlx::query_scalar("SELECT enabled_persisted FROM write_control_state WHERE id = 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(enabled, 0, "the operator's choice must survive a restart");
        pool.close().await;
    }

    /// #437: banto v6.5.0 の `0008_audit_log_pending_id.sql`（監査の保留、
    /// banto ADR-0019）は、`0101`〜`0106` を適用済みの既存 DB にとって
    /// 「番号が若い後から来た migration」になる。`sqlx` 0.9 の `Migrator::run`
    /// は適用済みの集合に無い版を順に流すだけで、順序の逆転を拒まない
    /// （`ignore_missing` が効くのは「DB にあってソースに無い」側だけ）。
    /// 0008 を除いた集合で作った DB を、今の集合で起動し直して確かめる。
    #[tokio::test]
    async fn an_existing_db_without_0008_gets_it_on_the_next_startup() {
        let dir = tempfile::tempdir().unwrap();
        let old_set = dir.path().join("migrations-before-0008");
        std::fs::create_dir(&old_set).unwrap();
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations-sqlite");
        for entry in std::fs::read_dir(&source).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name();
            if name.to_string_lossy().starts_with("0008_") {
                continue;
            }
            std::fs::copy(entry.path(), old_set.join(&name)).unwrap();
        }

        let db_path = dir.path().join("registry.sqlite3");
        let pool = banto_storage::connect_sqlite(&db_path).await.unwrap();
        let mut old = sqlx::migrate::Migrator::new(old_set.as_path())
            .await
            .unwrap();
        old.dangerous_set_table_name(MIGRATIONS_TABLE);
        old.run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO audit_log (action, resource, origin, result) \
             VALUES ('probe', 'test', 'rest', 'ok')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let before: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('audit_log') WHERE name = 'pending_id'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(before, 0, "the old set must not have 0008");
        pool.close().await;

        let pool = init_db(&db_path)
            .await
            .expect("an existing DB must take 0008 after 0101-0106");
        let versions: Vec<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT version FROM {MIGRATIONS_TABLE} ORDER BY version"
        )))
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            versions,
            vec![2, 3, 4, 5, 7, 8, 101, 102, 103, 104, 105, 106]
        );
        let pending_id: Option<String> =
            sqlx::query_scalar("SELECT pending_id FROM audit_log WHERE action = 'probe'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(pending_id, None, "the earlier row is kept, with NULL");
        let index: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master \
             WHERE type = 'index' AND name = 'idx_audit_log_pending_id'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(index, 1);
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
            let dir = tempfile::tempdir().unwrap();
            let db_path = dir.path().join("registry.sqlite3");
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
