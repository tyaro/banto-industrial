//! [`TsQueryError`]: every failure mode this crate's public API can report.
//!
//! Its own error type rather than reusing [`banto_tstore::TstoreError`] -
//! same reasoning `banto-tstore` itself gave for not reusing
//! `banto_core::BantoError` (see `banto-tstore/src/error.rs`'s module doc):
//! this crate is a low-level query engine, not itself a Tauri command
//! handler, and it has failure modes (`TooManyRows`, unsafe identifiers read
//! back from a file's self-described metadata) that have no equivalent in
//! `TstoreError`. `banto_tstore::TstoreError` values (from the `TsReader`
//! path `raw.rs` delegates to) are folded in via [`From`].

use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TsQueryError {
    /// A `sqlx`/filesystem failure - mirrors
    /// [`banto_tstore::TstoreError::Storage`]'s "own `String`, don't wrap the
    /// non-`Clone` source type" choice.
    #[error("ストレージエラー: {0}")]
    Storage(String),

    /// Caller-supplied arguments are structurally invalid (`from_ms >
    /// to_ms`, `target_bins == 0`) - checked before any file is touched.
    #[error("不正な入力です: {0}")]
    InvalidInput(String),

    /// [`crate::TsQuery::read_range`] would have returned more than its
    /// `max_rows` limit (default [`crate::DEFAULT_MAX_RAW_ROWS`]). Returned
    /// instead of silently truncating - a truncated CSV export would be a
    /// worse failure mode than an explicit error telling the caller to use
    /// `read_decimated` or narrow the range.
    #[error(
        "行数上限を超えました（{count} 件 > 上限 {max} 件）。\
         read_decimated による間引き取得、または期間を狭めての再実行を検討してください"
    )]
    TooManyRows { count: usize, max: usize },

    /// A data file matched by [`banto_tstore::list_data_files`] (i.e. its
    /// *name* is a well-formed `YYYYMMDD-NNN.sqlite3`) failed basic content
    /// validation when this crate queried its `tstore_groups`/
    /// `tstore_columns` tables directly (not via `TsReader::open`, which
    /// would have applied its own `format_version`/`tstore_meta` checks -
    /// see `plan.rs`). Surfaced as a hard error rather than silently
    /// skipping the file: a query result that quietly omits a real data
    /// file (as opposed to correctly reporting "no data for this group in
    /// this file") would be misleading in an industrial-recorder context.
    #[error("互換性のないデータファイルです: {path}: {message}")]
    IncompatibleFile { path: PathBuf, message: String },

    /// Defense-in-depth: a `table_name`/`column_name` read back from a
    /// file's own `tstore_groups`/`tstore_columns` metadata did not match
    /// the always-safe `samples_<n>`/`c<i>` shape `banto-tstore`'s
    /// `schema.rs` always generates (see `plan.rs`'s module doc). Never
    /// expected in practice against a genuine `banto-tstore` file; guards
    /// against a corrupted or hand-edited one being used to inject SQL
    /// through an identifier position.
    #[error("メタデータの識別子が不正です（ファイル破損の可能性）: {path}: {identifier}")]
    UnsafeIdentifier { path: PathBuf, identifier: String },
}

impl From<sqlx::Error> for TsQueryError {
    fn from(err: sqlx::Error) -> Self {
        TsQueryError::Storage(err.to_string())
    }
}

impl From<std::io::Error> for TsQueryError {
    fn from(err: std::io::Error) -> Self {
        TsQueryError::Storage(err.to_string())
    }
}

impl From<banto_tstore::TstoreError> for TsQueryError {
    fn from(err: banto_tstore::TstoreError) -> Self {
        TsQueryError::Storage(err.to_string())
    }
}
