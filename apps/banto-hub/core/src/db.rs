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

    // T2-4 (docs/tag-server-design.md §6-4「レート制限ブレーカ」・2026-08-05
    // 決定): `tripped_at` はキーの「トリップ」状態 - `revoked_at`(不可逆・
    // T0-2 の失効設計)とは別の**解除可能な**状態。`api_keys` は T0-2 で
    // 既に `CREATE TABLE IF NOT EXISTS` 済みなので、新しい列は
    // `ALTER TABLE ... ADD COLUMN` で後追いする必要がある - SQLite の
    // `ADD COLUMN` には `IF NOT EXISTS` 相当の構文がないため、
    // [`add_column_if_missing`] で `PRAGMA table_info` を見て「無ければ
    // 足す」形の冪等処理にしている(2回目以降の起動で列が既にあれば
    // 何もしない)。
    add_column_if_missing(pool, "api_keys", "tripped_at", "TEXT").await?;

    // T2-4 (docs/tag-server-design.md §6-6「再起動での安全側復帰」):
    // 書き込み受付フラグの永続値(表示専用 - `crate::write_control` の
    // モジュール doc 参照。**ライブフラグは常に起動時 disabled** で、この
    // テーブルは `was_enabled_before_restart` の履歴表示にしか使わない)。
    // `armed_state`(relay-wright)と同じ id=1 単一行パターン。
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS write_control_state (
          id INTEGER PRIMARY KEY CHECK (id = 1),
          enabled_persisted INTEGER NOT NULL DEFAULT 0,
          last_changed_at TEXT,
          last_changed_by TEXT
        )",
    )
    .execute(pool)
    .await
    .map_err(banto_storage::storage_error)?;
    sqlx::query("INSERT OR IGNORE INTO write_control_state (id, enabled_persisted) VALUES (1, 0)")
        .execute(pool)
        .await
        .map_err(banto_storage::storage_error)?;

    // T2-4 (docs/tag-server-design.md §6-3「log-before-write」): 書き込み
    // 監査ログ。列の意味・log-before-write の2段挿入パターンは
    // `crate::write_audit` のモジュール doc 参照。`action`/`result` の
    // 許容値は同モジュールの `WriteAuditAction`/`WriteAuditResult` の
    // `CHECK` 制約と一致させる。
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS hub_write_audit (
          id INTEGER PRIMARY KEY AUTOINCREMENT,
          ts TEXT NOT NULL DEFAULT (datetime('now')),
          api_key_id INTEGER NOT NULL,
          api_key_name_snapshot TEXT NOT NULL,
          tag_id INTEGER NOT NULL,
          external_name_snapshot TEXT NOT NULL,
          value_requested REAL,
          action TEXT NOT NULL CHECK (action IN ('write', 'rate_limit_tripped')),
          result TEXT NOT NULL CHECK (
            result IN ('ok', 'failed', 'suppressed_disabled', 'suppressed_rate_limited')
          ),
          detail TEXT
        )",
    )
    .execute(pool)
    .await
    .map_err(banto_storage::storage_error)?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_hub_write_audit_ts ON hub_write_audit(ts)")
        .execute(pool)
        .await
        .map_err(banto_storage::storage_error)?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_hub_write_audit_tag ON hub_write_audit(tag_id)")
        .execute(pool)
        .await
        .map_err(banto_storage::storage_error)?;

    // T6-2 (docs/tag-server-design.md §4.2「retain フラグで再起動時の最終値
    // 復元」): `retain = true` の内部タグの最終値。`tag_id` を主キーにする
    // （`crate::computed::ServerTagStore`のキー`"tag:{id}"`と同じidを使う -
    // `crate::computed::upsert_retained_value`/`load_retained_values`
    // 参照）。**`tags(id)` への FOREIGN KEY は張らない** - このテーブルは
    // `apply_app_schema`（このモジュール）の一部として
    // `banto_tags::migrate`（`tags` テーブルを作る側）より**先に**走る
    // (`run_migrations`のこのモジュール冒頭の doc comment参照) ため、その
    // 時点では参照先テーブルがまだ存在しない。タグ削除時に古い行が残っても
    // 実害はない(ロード時に該当 `tag_id` が catalog に無ければ
    // `ServerTagStore` へは書かれず、単に無視される)。
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS hub_retained_values (
          tag_id INTEGER PRIMARY KEY,
          value REAL NOT NULL,
          ptime_ms INTEGER NOT NULL
        )",
    )
    .execute(pool)
    .await
    .map_err(banto_storage::storage_error)?;

    Ok(())
}

/// `table` に `column` 列が無ければ `ddl_type` で `ALTER TABLE ... ADD
/// COLUMN` する冪等ヘルパー(この関数の呼び出し元 doc comment 参照: SQLite
/// の `ADD COLUMN` には `IF NOT EXISTS` が無いため、`PRAGMA table_info` で
/// 列の存在を見てから判断する)。`table`/`column`/`ddl_type` は呼び出し元が
/// 埋め込む固定のスキーマ定数のみを渡す想定(ユーザー入力を通さない)。
async fn add_column_if_missing(
    pool: &SqlitePool,
    table: &str,
    column: &str,
    ddl_type: &str,
) -> Result<(), BantoError> {
    use sqlx::Row;

    let rows = sqlx::query(&format!("PRAGMA table_info({table})"))
        .fetch_all(pool)
        .await
        .map_err(banto_storage::storage_error)?;
    let exists = rows.iter().any(|row| {
        row.try_get::<String, _>("name")
            .map(|name| name == column)
            .unwrap_or(false)
    });
    if !exists {
        sqlx::query(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {ddl_type}"
        ))
        .execute(pool)
        .await
        .map_err(banto_storage::storage_error)?;
    }
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

    /// T2-4: `api_keys.tripped_at` は `ALTER TABLE ADD COLUMN` の冪等ヘルパー
    /// ([`add_column_if_missing`]) 経由で足される。列が存在すること、かつ
    /// 二回目の `run_migrations` で `ALTER TABLE` エラー(重複列)を起こさない
    /// ことの両方を確認する。
    #[tokio::test]
    async fn api_keys_tripped_at_column_is_added_idempotently() {
        let pool = banto_storage::connect_sqlite_memory().await.unwrap();
        run_migrations(&pool).await.unwrap();

        use sqlx::Row;
        let rows = sqlx::query("PRAGMA table_info(api_keys)")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert!(
            rows.iter()
                .any(|row| row.get::<String, _>("name") == "tripped_at"),
            "api_keys should gain a tripped_at column"
        );

        // Second run must not error (ALTER TABLE ADD COLUMN on an existing
        // column would fail if add_column_if_missing's check were skipped).
        run_migrations(&pool).await.unwrap();
    }

    /// T2-4 (§6-6): `write_control_state` は起動時に id=1 の1行を必ず seed し、
    /// `enabled_persisted = 0` から始まる - `crate::write_control::WriteControl`
    /// が「再起動は常に disabled」を守るための前提。
    #[tokio::test]
    async fn write_control_state_seeds_a_single_disabled_row() {
        let pool = init_db_memory().await.unwrap();
        let enabled: i64 =
            sqlx::query_scalar("SELECT enabled_persisted FROM write_control_state WHERE id = 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(enabled, 0);
    }
}
