//! [`CollectError`]: every failure mode this crate's public API can report.
//!
//! Two very different origins fold into one type: registry read failures
//! while assembling a [`crate::config::CollectorConfig`]
//! ([`crate::build_config`], surfaced by `banto-tags`' services as
//! `banto_core::BantoError`), and storage/engine failures while starting or
//! stopping a [`crate::Collector`] (`banto-tstore`'s `TstoreError`). The
//! hot collection loop deliberately does *not* surface errors through this
//! type - a transient per-tick PLC failure becomes a `Bad` quality flag and
//! an event, never an `Err` that would tear the 24/365 loop down
//! (recorder-requirements.md §3.1/§4). Only *configuration* and
//! *lifecycle* failures are `CollectError`s.

use banto_core::BantoError;
use banto_tstore::TstoreError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CollectError {
    /// A [`crate::config::CollectorConfig`] could not be assembled because
    /// the registry snapshot is internally inconsistent in a way the tag
    /// registry's own validation does not catch - e.g. a tag whose stored
    /// `address` text does not parse under the PLC addressing rules
    /// (`banto-tags` only enforces non-empty; format is I2/I3b's concern,
    /// see `banto-tags`'s `tag.rs` doc), or a `data_type`/`protocol` string
    /// outside the vocabulary this build understands.
    #[error("収集設定エラー: {0}")]
    Config(String),

    /// A read against the tag registry failed while building the config
    /// snapshot (`banto-tags` service error).
    #[error("レジストリ読取りエラー: {0}")]
    Registry(#[from] BantoError),

    /// Opening/flushing/closing the time-series store failed
    /// (`banto-tstore`). Only ever produced by [`crate::Collector::start`]
    /// (opening the writer) and [`crate::Collector::stop`] (final
    /// flush/close) - never by the hot append path, which never turns a
    /// transient append failure into this error type (an append failure
    /// there still never stops collection - see `task.rs`'s module doc,
    /// "H4" - though since 2026-08-08 (owner decision, docs/improvement-plan.md
    /// H4) it is no longer silently discarded either: `task.rs` now logs and
    /// records it as a `collect_events` row instead).
    #[error("時系列ストレージエラー: {0}")]
    Tstore(#[from] TstoreError),

    /// A database error while creating the `collect_events` table
    /// ([`crate::migrate`]).
    #[error("イベントテーブル作成エラー: {0}")]
    Migrate(String),
}
