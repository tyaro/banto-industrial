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

    /// A data file's name did not match the `YYYYMMDD-NNN.sqlite3` pattern
    /// this crate itself always writes - either filesystem corruption or a
    /// foreign file placed in the data directory by something else.
    #[error("データファイル名の形式が不正です: {0}")]
    InvalidFileName(PathBuf),

    /// A file opened via [`crate::reader::TsReader::open`] (or reused via
    /// [`crate::writer::TsWriter::open`]) is not a `banto-tstore` file this
    /// version understands: missing/unreadable `tstore_meta`, or a
    /// `format_version` this build does not support.
    #[error("互換性のないファイルです: {0}")]
    IncompatibleFile(String),
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
