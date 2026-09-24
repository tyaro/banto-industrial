//! [`TstoreError`]: every failure mode this crate's public API can report.
//!
//! Unlike `banto-tags` (which shares `banto_core::BantoError` across a
//! Tauri/REST boundary for CRUD-entity resources), this crate defines its
//! own error type - same choice `banto-plc` made for `PlcError`, for the
//! same reason: `banto-tstore` is a low-level engine crate consumed by I3b's
//! collection engine, not itself a Tauri command handler, and it
//! deliberately does not depend on the tag-registry stack at all (this
//! crate's own module doc, and the design's "tstoreはレジストリDBを読まない"
//! principle). I3b's own error type is the one that should decide how (or
//! whether) a `TstoreError` gets folded into `BantoError` for the UI.

use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TstoreError {
    /// A `sqlx`/filesystem failure - the message is the underlying error's
    /// `Display` text, captured at the point of failure (mirrors
    /// `banto_plc::PlcError::Connection`'s "own `String`, don't wrap the
    /// non-`Clone` source type" choice, though here it's about keeping this
    /// enum simple rather than `Clone`-ability).
    #[error("ストレージエラー: {0}")]
    Storage(String),

    /// A [`crate::config::StoreConfig`] failed validation before any file
    /// was touched - empty group list, empty/duplicate `group_key`/`tag_key`,
    /// or `period_ms == 0` (see `config.rs::validate`).
    #[error("設定エラー: {0}")]
    Config(String),

    /// [`crate::writer::TsWriter::append`]/[`crate::reader::TsReader::read_range`]
    /// referenced a `group_key` that is not part of this store's (frozen at
    /// file-creation time) configuration.
    #[error("未知のグループです: {0}")]
    UnknownGroup(String),

    /// `append`'s `values` slice length did not match the group's configured
    /// column (tag) count.
    #[error("値の個数が一致しません: グループ {group_key} は {expected} 列ですが {actual} 個の値が渡されました")]
    ValueCountMismatch {
        group_key: String,
        expected: usize,
        actual: usize,
    },

    /// [`crate::writer::TsWriter::append`] was given a `ptime_ms` that the
    /// read side could never find in the currently open file (dated
    /// `file_date`): the ptime/file-date contract in [`crate::findable`]
    /// (#424, owner decision 2026-09-24). The row was not buffered or
    /// written; the writer's state (open file, other buffered rows) is
    /// unchanged. In collection this only happens when the wall clock jumps
    /// by roughly a day between taking `ptime` and the `append` call - the
    /// sample is dropped and the next one is written normally.
    #[error(
        "記録時刻 {ptime_ms}ms はデータファイルの日付 {}-{:02}-{:02} から離れすぎているため書き込めません（読み出しで見つけられなくなるため。時計が大きく飛んだ可能性があります）",
        .file_date.year,
        .file_date.month,
        .file_date.day
    )]
    PtimeOutsideFileDate {
        ptime_ms: i64,
        file_date: crate::date::LocalDate,
    },

    /// A data file's name did not match the `YYYYMMDD-NNN.sqlite3` pattern
    /// this crate itself always writes - either filesystem corruption or a
    /// foreign file placed in the data directory by something else.
    #[error("データファイル名の形式が不正です: {0}")]
    InvalidFileName(PathBuf),

    /// A file opened via [`crate::reader::TsReader::open`] (or reused via
    /// [`crate::writer::TsWriter::open`]) has a `tstore_meta` table but is
    /// not a `banto-tstore` file this version understands: `format_version`
    /// missing/unsupported, or another required `tstore_meta`/
    /// `tstore_groups`/`tstore_columns` key/row missing - a genuine format
    /// mismatch or corruption. Distinct from [`Self::Uninitialized`], which
    /// means there is no `banto-tstore` schema at all yet.
    #[error("互換性のないファイルです: {0}")]
    IncompatibleFile(String),

    /// The file exists (SQLite already created it on disk) but has **no**
    /// `banto-tstore` schema at all yet - no `tstore_meta` table, so nothing
    /// downstream could be "wrong" about it either. The one confirmed cause:
    /// [`crate::writer::TsWriter::open`]'s underlying `SqlitePoolOptions::
    /// connect_with(.. .create_if_missing(true) ..)` (`schema::connect_writable`)
    /// brings the physical `.sqlite3` file into existence *before*
    /// `schema::create_schema`'s single DDL transaction commits - a reader
    /// that opens the file inside that (short, but real - see
    /// `crates/banto-tsquery/tests/concurrency.rs`) window observes a valid,
    /// connectable, zero-table SQLite database. Unlike [`Self::IncompatibleFile`]
    /// (a real format problem worth surfacing as an error), this is "no data
    /// here yet" - callers that walk a data directory (`banto-tsquery`) treat
    /// it exactly like a file that does not exist at all, not as a failure.
    #[error("banto-tstore のスキーマがまだ存在しません（書き込み中の可能性があります）: {0}")]
    Uninitialized(String),
}

impl From<sqlx::Error> for TstoreError {
    fn from(err: sqlx::Error) -> Self {
        TstoreError::Storage(err.to_string())
    }
}

impl From<std::io::Error> for TstoreError {
    fn from(err: std::io::Error) -> Self {
        TstoreError::Storage(err.to_string())
    }
}
