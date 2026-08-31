//! [`TsReader`]: the minimal read-back primitive over one data file - "I4の
//! 土台。範囲クエリ・間引きはI4でやるのでここは最小限" (design principle).
//! Deliberately does *not* do windowing, downsampling, or multi-file range
//! resolution (that is I4's job, using [`crate::files::list_data_files`] to
//! find candidate files and one `TsReader` per file); this type only opens
//! one file, exposes its self-described metadata, and answers one range
//! query per call.

use std::path::Path;

use sqlx::{Row, SqlitePool};

use crate::error::TstoreError;
use crate::meta::GroupMeta;
use crate::schema;

/// One row from a `samples_<n>` table: the UTC epoch-ms `ptime` key plus one
/// `Option<f64>` per column, in the same order as the owning
/// [`GroupMeta::columns`] (`None` = the missing-sample NULL the design
/// principle "NULL=欠測" describes).
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub ptime_ms: i64,
    pub values: Vec<Option<f64>>,
}

/// A read-only handle on one data file, opened via its own embedded
/// `tstore_meta`/`tstore_groups`/`tstore_columns` metadata - no
/// `StoreConfig`/tag registry needed to interpret it (this crate's central
/// self-description principle).
#[derive(Debug)]
pub struct TsReader {
    pool: SqlitePool,
    groups: Vec<GroupMeta>,
}

impl TsReader {
    /// Open `path` read-only and load its metadata. Fails with
    /// [`TstoreError::Uninitialized`] if the file exists but has no
    /// `banto-tstore` schema at all yet (e.g. a reader raced
    /// [`crate::writer::TsWriter::open`]'s file-creation transaction - see
    /// that variant's doc comment), or [`TstoreError::IncompatibleFile`] if
    /// it has a `tstore_meta` table but is not a `banto-tstore` file this
    /// build understands (unsupported `format_version`, or another required
    /// key/table missing).
    pub async fn open(path: &Path) -> Result<Self, TstoreError> {
        let pool = schema::connect_readonly(path).await?;
        let file_meta = schema::read_file_meta(&pool).await?;
        Ok(Self {
            pool,
            groups: file_meta.groups,
        })
    }

    /// Every group this file describes, in the same order its `samples_<n>`
    /// tables were created (i.e. `groups()[i].table_name == "samples_{i+1}"`).
    pub fn groups(&self) -> &[GroupMeta] {
        &self.groups
    }

    /// Convenience lookup by `group_key` - what [`Self::read_range`] uses
    /// internally, exposed for callers that want a group's column metadata
    /// (units/decimals/tag names) without doing their own linear search over
    /// [`Self::groups`].
    pub fn group(&self, group_key: &str) -> Option<&GroupMeta> {
        self.groups.iter().find(|g| g.key == group_key)
    }

    /// Every sample for `group_key` with `from_ms <= ptime_ms <= to_ms`
    /// (both bounds inclusive), ordered ascending by `ptime_ms`.
    pub async fn read_range(
        &self,
        group_key: &str,
        from_ms: i64,
        to_ms: i64,
    ) -> Result<Vec<Sample>, TstoreError> {
        let group = self
            .group(group_key)
            .ok_or_else(|| TstoreError::UnknownGroup(group_key.to_string()))?;

        let mut column_list = String::from("ptime");
        for column in &group.columns {
            column_list.push_str(", ");
            // Always "c1".."cN" (assigned by `schema.rs`, read back
            // verbatim) - never a caller-influenced string, so this is safe
            // to splice directly (see `schema.rs`'s module doc).
            column_list.push_str(&column.column_name);
        }

        let sql = format!(
            "SELECT {column_list} FROM {} WHERE ptime >= ? AND ptime <= ? ORDER BY ptime ASC",
            group.table_name
        );

        // AssertSqlSafe: `column_list`/`group.table_name` は schema.rs の
        // モジュール doc の通り `samples_<n>`/`c<i>` 形式の生成済み識別子のみで
        // 構成され、呼び出し元の `group_key` などユーザー入力は含まれない
        // （バインド値として from_ms/to_ms のみを渡す）。
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(from_ms)
            .bind(to_ms)
            .fetch_all(&self.pool)
            .await?;

        let samples = rows
            .into_iter()
            .map(|row| {
                let ptime_ms: i64 = row.get("ptime");
                let values = group
                    .columns
                    .iter()
                    .map(|column| row.get::<Option<f64>, _>(column.column_name.as_str()))
                    .collect();
                Sample { ptime_ms, values }
            })
            .collect();
        Ok(samples)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A fresh temp *file path* for one test (not a directory - unlike
    /// `writer::tests::TempDir`, these tests open bare `.sqlite3` files
    /// directly with `schema::connect_writable`/`TsReader::open`). Cleans up
    /// the `.sqlite3` file itself plus its WAL sidecars (`-wal`/`-shm`,
    /// sqlite's standard naming - appended to the full filename, not
    /// replacing the extension) on drop - see `crate::test_support`'s
    /// module doc for why this retries and requires a multi-thread runtime.
    struct TempFile(PathBuf);

    impl TempFile {
        fn new(label: &str) -> Self {
            let id = COUNTER.fetch_add(1, Ordering::SeqCst);
            // Nanosecond timestamp alongside the PID + counter guards
            // against PID reuse colliding with an old, already-initialized
            // file from a previous run (same reasoning as
            // `apps/banto-hub/core/tests/common/mod.rs`'s `TempEnv::new`).
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "banto-tstore-test-reader-{}-{label}-{id}-{nanos}.sqlite3",
                std::process::id()
            ));
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn wal_path(&self) -> PathBuf {
            let mut s = self.0.clone().into_os_string();
            s.push("-wal");
            PathBuf::from(s)
        }

        fn shm_path(&self) -> PathBuf {
            let mut s = self.0.clone().into_os_string();
            s.push("-shm");
            PathBuf::from(s)
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            crate::test_support::retry_remove(&self.0, |p| std::fs::remove_file(p));
            crate::test_support::retry_remove(&self.wal_path(), |p| std::fs::remove_file(p));
            crate::test_support::retry_remove(&self.shm_path(), |p| std::fs::remove_file(p));
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn open_missing_file_is_a_storage_error() {
        let file = TempFile::new("missing");
        let err = TsReader::open(file.path()).await.unwrap_err();
        assert!(matches!(err, TstoreError::Storage(_)));
    }

    // H7フォローアップ（TsQuery「未初期化ファイル」対応、2026-08-09）:
    // 以前はこのケース（`tstore_meta` テーブルが無い）を
    // `TstoreError::IncompatibleFile` として扱っていたが、これは
    // ライター側のスキーマ作成トランザクション未コミット（レース）と
    // 「本当に壊れた/無関係なファイル」を区別できない誤ったエラー種別
    // だった（`crates/banto-tsquery` がこれを見て範囲クエリ全体を
    // ハードエラーにしてしまっていた）。`tstore_meta` テーブル自体が
    // 存在しない、という条件だけで判定できる以上、「壊れたファイル」と
    // 「まだ書き込み中のファイル」を区別する材料はこの時点では無い -
    // よって両方とも `Uninitialized` に倒す（`error.rs::TstoreError::
    // Uninitialized` の doc comment 参照）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn open_file_with_an_unrelated_table_and_no_tstore_meta_is_uninitialized() {
        let file = TempFile::new("foreign");
        // A real, connectable SQLite file with some other table, but no
        // `tstore_meta` - indistinguishable, from this crate's point of
        // view, from the writer-race window `Uninitialized` documents.
        let pool = schema::connect_writable(file.path()).await.unwrap();
        sqlx::query("CREATE TABLE not_tstore (id INTEGER)")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;

        let err = TsReader::open(file.path()).await.unwrap_err();
        assert!(matches!(err, TstoreError::Uninitialized(_)), "{err:?}");
    }

    /// The literal writer-race window `TstoreError::Uninitialized`'s doc
    /// comment describes: `connect_writable` (`create_if_missing(true)`)
    /// brings the physical file into existence, but this test deliberately
    /// stops there - never running `schema::create_schema`'s transaction -
    /// so the file has zero tables, exactly what a reader can observe if it
    /// opens the file in the gap between `TsWriter::open`'s file creation
    /// and that transaction's commit.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn open_file_with_zero_tables_is_uninitialized() {
        let file = TempFile::new("zero-tables");
        let pool = schema::connect_writable(file.path()).await.unwrap();
        pool.close().await;

        let err = TsReader::open(file.path()).await.unwrap_err();
        assert!(matches!(err, TstoreError::Uninitialized(_)), "{err:?}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn open_file_with_unsupported_format_version_is_incompatible() {
        let file = TempFile::new("bad-version");
        let pool = schema::connect_writable(file.path()).await.unwrap();
        sqlx::query("CREATE TABLE tstore_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO tstore_meta (key, value) VALUES ('format_version', '999')")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;

        let err = TsReader::open(file.path()).await.unwrap_err();
        match err {
            TstoreError::IncompatibleFile(message) => assert!(message.contains("999")),
            other => panic!("expected IncompatibleFile, got {other:?}"),
        }
    }
}
