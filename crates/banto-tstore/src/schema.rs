//! Data file naming, connection helpers, and the DDL/DML that turns a
//! [`crate::config::StoreConfig`] into a fresh, self-described SQLite file
//! (or reads one back). Shared by [`crate::writer::TsWriter`] (writes) and
//! [`crate::reader::TsReader`] (reads) so both agree on the exact same table
//! (`samples_<n>`) and column (`c<i>`) naming scheme.
//!
//! ## Why `group_key`/`tag_key` never appear inside generated SQL
//!
//! Every table/column *identifier* this module generates is a
//! programmatically-derived, always-safe string: `samples_<n>` (`n` = the
//! group's 1-based position in `StoreConfig.groups`) and `c<i>` (`i` = the
//! tag's 1-based position within its group). The caller-supplied
//! `group_key`/`tag_key`/names are only ever bound as `TEXT` *values* into
//! `tstore_groups`/`tstore_columns` - never formatted into SQL text - so
//! there is no SQL-identifier-injection surface here regardless of what
//! characters a key contains.
//!
//! ## Table layout (format_version 1)
//!
//! - `tstore_meta(key TEXT PRIMARY KEY, value TEXT)`: `format_version`,
//!   `created_at_ms`, `local_date` (`YYYYMMDD`), `config_hash`
//! - `tstore_groups(group_key TEXT PRIMARY KEY, group_name, period_ms, table_name)`
//! - `tstore_columns(group_key, column_name, column_index, tag_key, tag_name,
//!   data_type, unit, decimals)`, `PRIMARY KEY (group_key, column_name)`
//! - per group: `samples_<n>(ptime INTEGER PRIMARY KEY, c1 REAL, c2 REAL, ...)`
//!
//! `samples_<n>` deliberately stays a plain `ROWID` table (no `WITHOUT
//! ROWID`): `ptime INTEGER PRIMARY KEY` already makes `ptime` an alias for
//! the table's `rowid`, so rows are physically clustered/ordered by `ptime`
//! on disk *for free*, which is exactly what `WITHOUT ROWID` would also
//! trade for - except `WITHOUT ROWID` additionally duplicates the full key
//! into every interior b-tree page and drops the small fixed-size rowid
//! optimization SQLite gives integer-PK tables, which would cost more than
//! it buys for a table that is (a) already narrow/PK-clustered and (b)
//! appended to in monotonically-increasing-`ptime` order (the best case for
//! plain rowid b-tree insertion, no page-split thrashing). `WITHOUT ROWID`
//! earns its keep on tables with a *non-integer* or *composite* primary key,
//! which this is not.

use std::path::Path;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use crate::config::StoreConfig;
use crate::date::LocalDate;
use crate::error::TstoreError;
use crate::meta::{ColumnMeta, FileMeta, GroupMeta};

pub(crate) const FORMAT_VERSION: i64 = 1;
const FILE_EXTENSION: &str = "sqlite3";

pub(crate) fn table_name_for_index(group_index: usize) -> String {
    format!("samples_{}", group_index + 1)
}

pub(crate) fn column_name_for_index(tag_index: usize) -> String {
    format!("c{}", tag_index + 1)
}

/// `YYYYMMDD-NNN.sqlite3` (`NNN` zero-padded to 3 digits, per this crate's
/// design "同日内の連番3桁").
pub(crate) fn data_file_name(date: LocalDate, seq: u32) -> String {
    format!("{}-{:03}.{}", date.to_yyyymmdd(), seq, FILE_EXTENSION)
}

/// Parse a file *name* (not a full path - callers pass `Path::file_name()`)
/// back into `(date, seq)`. Rejects anything that is not exactly
/// `YYYYMMDD-NNN.sqlite3` - see [`crate::error::TstoreError::InvalidFileName`]'s
/// doc comment for why "foreign file in the data directory" is a recoverable
/// (skip it) rather than fatal condition at the call sites that matter
/// ([`crate::files::list_data_files`]/[`crate::files::prune_files`]).
pub(crate) fn parse_data_file_name(file_name: &str) -> Option<(LocalDate, u32)> {
    let stem = file_name.strip_suffix(&format!(".{FILE_EXTENSION}"))?;
    let (date_part, seq_part) = stem.split_once('-')?;
    if seq_part.len() != 3 || !seq_part.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let date = LocalDate::parse_yyyymmdd(date_part)?;
    let seq: u32 = seq_part.parse().ok()?;
    Some((date, seq))
}

/// Open (creating if missing) a writable connection: WAL journaling +
/// foreign keys on, same baseline `banto_storage::sqlite::connect` uses -
/// not reused directly from there because pulling in `banto-storage` (and
/// transitively `banto-core`) for four lines of `sqlx` options would be a
/// real dependency purely to avoid a few lines of duplication, which cuts
/// against this crate's registry-independence principle (this module's doc,
/// and `lib.rs`'s doc comment).
pub(crate) async fn connect_writable(path: &Path) -> Result<SqlitePool, TstoreError> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true);
    Ok(SqlitePoolOptions::new().connect_with(options).await?)
}

/// Open an existing file read-only - [`crate::reader::TsReader::open`]'s
/// connection. Never creates the file (a missing path is a genuine error for
/// a reader, unlike a writer which is allowed to create today's first file).
pub(crate) async fn connect_readonly(path: &Path) -> Result<SqlitePool, TstoreError> {
    if !path.is_file() {
        return Err(TstoreError::Storage(format!(
            "ファイルが見つかりません: {}",
            path.display()
        )));
    }
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .read_only(true);
    Ok(SqlitePoolOptions::new().connect_with(options).await?)
}

/// Create every table (`tstore_meta`/`tstore_groups`/`tstore_columns` +
/// one `samples_<n>` per group) and populate the two metadata tables, all in
/// one transaction - `config` must already have passed [`StoreConfig::validate`]
/// (this crate's two callers, both in `writer.rs`, always validate first).
pub(crate) async fn create_schema(
    pool: &SqlitePool,
    config: &StoreConfig,
    config_hash: &str,
    date: LocalDate,
    created_at_ms: i64,
) -> Result<(), TstoreError> {
    let mut tx = pool.begin().await?;

    sqlx::query("CREATE TABLE tstore_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "CREATE TABLE tstore_groups (\
            group_key TEXT PRIMARY KEY, \
            group_name TEXT NOT NULL, \
            period_ms INTEGER NOT NULL, \
            table_name TEXT NOT NULL UNIQUE\
        )",
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "CREATE TABLE tstore_columns (\
            group_key TEXT NOT NULL REFERENCES tstore_groups(group_key), \
            column_name TEXT NOT NULL, \
            column_index INTEGER NOT NULL, \
            tag_key TEXT NOT NULL, \
            tag_name TEXT NOT NULL, \
            data_type TEXT NOT NULL, \
            unit TEXT, \
            decimals INTEGER NOT NULL, \
            PRIMARY KEY (group_key, column_name)\
        )",
    )
    .execute(&mut *tx)
    .await?;

    for meta_row in [
        ("format_version", FORMAT_VERSION.to_string()),
        ("created_at_ms", created_at_ms.to_string()),
        ("local_date", date.to_yyyymmdd()),
        ("config_hash", config_hash.to_string()),
    ] {
        sqlx::query("INSERT INTO tstore_meta (key, value) VALUES (?, ?)")
            .bind(meta_row.0)
            .bind(meta_row.1)
            .execute(&mut *tx)
            .await?;
    }

    for (group_index, group) in config.groups.iter().enumerate() {
        let table_name = table_name_for_index(group_index);

        sqlx::query(
            "INSERT INTO tstore_groups (group_key, group_name, period_ms, table_name) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(&group.key)
        .bind(&group.name)
        .bind(group.period_ms)
        .bind(&table_name)
        .execute(&mut *tx)
        .await?;

        let mut create_table = format!("CREATE TABLE {table_name} (ptime INTEGER PRIMARY KEY");
        for tag_index in 0..group.tags.len() {
            create_table.push_str(&format!(", {} REAL", column_name_for_index(tag_index)));
        }
        create_table.push(')');
        // AssertSqlSafe: create_table はモジュール冒頭のコメントの通り
        // `table_name_for_index`/`column_name_for_index` が生成する
        // `samples_<n>`/`c<i>` のみで構成される識別子であり、呼び出し元の
        // `group_key`/`tag_key` などユーザー入力は一切含まれない。
        sqlx::query(sqlx::AssertSqlSafe(create_table))
            .execute(&mut *tx)
            .await?;

        for (tag_index, tag) in group.tags.iter().enumerate() {
            let column_name = column_name_for_index(tag_index);
            sqlx::query(
                "INSERT INTO tstore_columns \
                    (group_key, column_name, column_index, tag_key, tag_name, data_type, unit, decimals) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&group.key)
            .bind(&column_name)
            .bind(tag_index as i64)
            .bind(&tag.key)
            .bind(&tag.name)
            .bind(&tag.data_type)
            .bind(&tag.unit)
            .bind(tag.decimals as i64)
            .execute(&mut *tx)
            .await?;
        }
    }

    tx.commit().await?;
    Ok(())
}

/// Read a file's `tstore_meta`/`tstore_groups`/`tstore_columns` back into a
/// [`FileMeta`]. Returns [`TstoreError::Uninitialized`] if `tstore_meta`
/// itself does not exist (no `banto-tstore` schema at all yet - see that
/// variant's doc comment for the writer-race window this covers), or
/// [`TstoreError::IncompatibleFile`] if `tstore_meta` exists but a required
/// key is missing/invalid, or `format_version` is not one this build
/// understands.
pub(crate) async fn read_file_meta(pool: &SqlitePool) -> Result<FileMeta, TstoreError> {
    // Detect "no schema at all" robustly via `sqlite_master`, rather than
    // inferring it from whatever error the `SELECT ... FROM tstore_meta`
    // below happens to fail with - a missing-table error is the only case
    // this should catch (a *locked*/corrupt-but-present `tstore_meta` must
    // still fall through to that query and surface as `IncompatibleFile`,
    // not be silently reclassified as "uninitialized").
    let table_exists =
        sqlx::query("SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'tstore_meta'")
            .fetch_optional(pool)
            .await?
            .is_some();
    if !table_exists {
        return Err(TstoreError::Uninitialized(
            "tstore_meta テーブルが存在しません（書き込み中、またはbanto-tstoreのファイルではない可能性があります）"
                .to_string(),
        ));
    }

    let meta_rows = sqlx::query("SELECT key, value FROM tstore_meta")
        .fetch_all(pool)
        .await
        .map_err(|_| {
            TstoreError::IncompatibleFile(
                "tstore_meta テーブルの読み取りに失敗しました".to_string(),
            )
        })?;

    let mut format_version: Option<i64> = None;
    let mut created_at_ms: Option<i64> = None;
    let mut local_date: Option<LocalDate> = None;
    let mut config_hash: Option<String> = None;
    for row in meta_rows {
        let key: String = row.get("key");
        let value: String = row.get("value");
        match key.as_str() {
            "format_version" => format_version = value.parse().ok(),
            "created_at_ms" => created_at_ms = value.parse().ok(),
            "local_date" => local_date = LocalDate::parse_yyyymmdd(&value),
            "config_hash" => config_hash = Some(value),
            _ => {}
        }
    }

    let format_version = format_version.ok_or_else(|| {
        TstoreError::IncompatibleFile("tstore_meta.format_version がありません".to_string())
    })?;
    if format_version != FORMAT_VERSION {
        return Err(TstoreError::IncompatibleFile(format!(
            "未対応の format_version です: {format_version}（対応: {FORMAT_VERSION}）"
        )));
    }
    let created_at_ms = created_at_ms.ok_or_else(|| {
        TstoreError::IncompatibleFile("tstore_meta.created_at_ms が不正です".to_string())
    })?;
    let local_date = local_date.ok_or_else(|| {
        TstoreError::IncompatibleFile("tstore_meta.local_date が不正です".to_string())
    })?;
    let config_hash = config_hash.ok_or_else(|| {
        TstoreError::IncompatibleFile("tstore_meta.config_hash がありません".to_string())
    })?;

    let group_rows =
        sqlx::query("SELECT group_key, group_name, period_ms, table_name FROM tstore_groups")
            .fetch_all(pool)
            .await?;

    let mut groups = Vec::with_capacity(group_rows.len());
    for group_row in group_rows {
        let group_key: String = group_row.get("group_key");
        let group_name: String = group_row.get("group_name");
        let period_ms: i64 = group_row.get("period_ms");
        let table_name: String = group_row.get("table_name");

        let column_rows = sqlx::query(
            "SELECT column_name, tag_key, tag_name, data_type, unit, decimals \
             FROM tstore_columns WHERE group_key = ? ORDER BY column_index ASC",
        )
        .bind(&group_key)
        .fetch_all(pool)
        .await?;

        let columns = column_rows
            .into_iter()
            .map(|column_row| {
                let decimals: i64 = column_row.get("decimals");
                ColumnMeta {
                    column_name: column_row.get("column_name"),
                    tag_key: column_row.get("tag_key"),
                    tag_name: column_row.get("tag_name"),
                    data_type: column_row.get("data_type"),
                    unit: column_row.get("unit"),
                    decimals: decimals as u8,
                }
            })
            .collect();

        groups.push(GroupMeta {
            key: group_key,
            name: group_name,
            period_ms: period_ms as u32,
            table_name,
            columns,
        });
    }

    Ok(FileMeta {
        format_version,
        created_at_ms,
        local_date,
        config_hash,
        groups,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_name_for_index_is_one_based() {
        assert_eq!(table_name_for_index(0), "samples_1");
        assert_eq!(table_name_for_index(1), "samples_2");
    }

    #[test]
    fn column_name_for_index_is_one_based() {
        assert_eq!(column_name_for_index(0), "c1");
        assert_eq!(column_name_for_index(7), "c8");
    }

    #[test]
    fn data_file_name_zero_pads_seq_to_three_digits() {
        let date = LocalDate::new(2026, 7, 12);
        assert_eq!(data_file_name(date, 1), "20260712-001.sqlite3");
        assert_eq!(data_file_name(date, 42), "20260712-042.sqlite3");
    }

    #[test]
    fn parse_data_file_name_round_trips() {
        let date = LocalDate::new(2026, 7, 12);
        let name = data_file_name(date, 7);
        assert_eq!(parse_data_file_name(&name), Some((date, 7)));
    }

    #[test]
    fn parse_data_file_name_rejects_wrong_extension() {
        assert_eq!(parse_data_file_name("20260712-001.db"), None);
    }

    #[test]
    fn parse_data_file_name_rejects_missing_dash() {
        assert_eq!(parse_data_file_name("20260712001.sqlite3"), None);
    }

    #[test]
    fn parse_data_file_name_rejects_wrong_seq_width() {
        assert_eq!(parse_data_file_name("20260712-01.sqlite3"), None);
        assert_eq!(parse_data_file_name("20260712-0001.sqlite3"), None);
    }

    #[test]
    fn parse_data_file_name_rejects_non_digit_seq() {
        assert_eq!(parse_data_file_name("20260712-abc.sqlite3"), None);
    }

    #[test]
    fn parse_data_file_name_rejects_bogus_date() {
        assert_eq!(parse_data_file_name("2026071-001.sqlite3"), None);
    }
}
