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
    /// [`TstoreError::IncompatibleFile`] if `path` is not a `banto-tstore`
    /// file this build understands (missing `tstore_meta`/unsupported
    /// `format_version`).
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

        let rows = sqlx::query(&sql)
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn open_foreign_sqlite_file_is_incompatible() {
        let file = TempFile::new("foreign");
        // A real SQLite file, but not one this crate ever wrote.
        let pool = schema::connect_writable(file.path()).await.unwrap();
        sqlx::query("CREATE TABLE not_tstore (id INTEGER)")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;

        let err = TsReader::open(file.path()).await.unwrap_err();
        assert!(matches!(err, TstoreError::IncompatibleFile(_)));
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
