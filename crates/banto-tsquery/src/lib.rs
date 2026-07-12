//! banto-tsquery: I4 時系列クエリ層 (docs/plan.md I4,
//! docs/recorder-requirements.md §3.3 "トレンド3モード" のヒストリカル/
//! ハイブリッド、§3.5 "日報帳票").
//!
//! [`TsQuery`] answers period queries over a `banto-tstore` (I3a) data
//! directory: raw ranges for small windows/CSV export
//! ([`TsQuery::read_range`]), server-side min/max-decimated bins for trend
//! viewports ([`TsQuery::read_decimated`]), per-tag min/max/avg summaries for
//! daily reports ([`TsQuery::aggregate`]), and what data/groups/tags exist at
//! all for a UI's period picker ([`TsQuery::catalog`]).
//!
//! Like `banto-tstore` itself, this crate never reads the tag registry (I1):
//! every method works from a data directory's self-described
//! `tstore_meta`/`tstore_groups`/`tstore_columns` files alone.
//!
//! ## Why a `min`/`max` envelope, not averaged decimation
//!
//! A recorder's entire value proposition over a fixed-interval chart is
//! catching the sample a naive average would smooth away - see
//! [`mod@decimate`]'s module doc for the full design rationale. Every
//! [`DecimatedRange`] bin therefore reports [`BinValue::Range { min, max }`]
//! (both ends of the envelope, never just one representative value), and a
//! bin with no valid samples reports [`BinValue::Gap`] rather than being
//! silently omitted or interpolated.
//!
//! ## Module map
//!
//! - [`files`][]: candidate-file pre-selection for a `[from_ms, to_ms]`
//!   range, tolerant of not knowing a file's write-time UTC offset.
//! - [`plan`][]: per-file, per-`group_key` planning (own read-only
//!   connection + `tag_key` -> physical column resolution + SQL-identifier
//!   safety) that `decimate`/`aggregate` build their `GROUP BY`/aggregate
//!   SQL on top of.
//! - [`raw`][]: [`TsQuery::read_range`], delegating the per-file fetch to
//!   [`banto_tstore::TsReader::read_range`].
//! - [`decimate`][]: [`TsQuery::read_decimated`].
//! - [`aggregate`][]: [`TsQuery::aggregate`].
//! - [`catalog`][]: [`TsQuery::catalog`].
//! - [`types`][]: every public result shape.
//! - [`error`][]: [`TsQueryError`].

mod aggregate;
mod catalog;
mod decimate;
mod files;
mod plan;
mod raw;
mod types;

pub mod error;

pub use decimate::MAX_TARGET_BINS;
pub use error::TsQueryError;
pub use raw::DEFAULT_MAX_RAW_ROWS;
pub use types::{
    Bin, BinValue, Catalog, DecimatedRange, GroupCatalogEntry, RawRange, RawRow, TagAggregate,
    TagCatalogEntry,
};

use std::path::{Path, PathBuf};

/// A read-only handle on one `banto-tstore` data directory. Cheap to
/// construct/hold (just a `PathBuf` - no connections are opened until a
/// query method is called, and none are kept open between calls, unlike
/// `banto-tstore::TsWriter`'s long-lived pool).
#[derive(Debug, Clone)]
pub struct TsQuery {
    data_dir: PathBuf,
}

impl TsQuery {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
        }
    }

    /// Every sample for `group_key`/`tag_keys` with `from_ms <= ptime_ms <=
    /// to_ms` (both bounds inclusive - matching
    /// [`banto_tstore::TsReader::read_range`]'s convention), concatenated
    /// across however many files the range spans, ascending by `ptime_ms`.
    /// Intended for small ranges (CSV export - recorder-requirements.md
    /// §3.5): errors with [`TsQueryError::TooManyRows`] rather than
    /// returning a partial result if the total row count would exceed
    /// `max_rows` (`None` = [`DEFAULT_MAX_RAW_ROWS`]). For anything wider,
    /// use [`Self::read_decimated`].
    pub async fn read_range(
        &self,
        group_key: &str,
        tag_keys: &[String],
        from_ms: i64,
        to_ms: i64,
        max_rows: Option<usize>,
    ) -> Result<RawRange, TsQueryError> {
        raw::read_range(
            &self.data_dir,
            group_key,
            tag_keys,
            from_ms,
            to_ms,
            max_rows,
        )
        .await
    }

    /// Server-side min/max-decimated bins across `[from_ms, to_ms]`, sized
    /// so the result has roughly `target_bins` entries (≒ the caller's
    /// viewport pixel width) regardless of how many raw samples the range
    /// actually contains - see [`mod@decimate`]'s module doc for the full
    /// bin-width/gap/near-native-zoom design.
    pub async fn read_decimated(
        &self,
        group_key: &str,
        tag_keys: &[String],
        from_ms: i64,
        to_ms: i64,
        target_bins: usize,
    ) -> Result<DecimatedRange, TsQueryError> {
        decimate::read_decimated(
            &self.data_dir,
            group_key,
            tag_keys,
            from_ms,
            to_ms,
            target_bins,
        )
        .await
    }

    /// Per-tag `min`/`max`/`avg`/`count` over `[from_ms, to_ms]` (both
    /// bounds inclusive), `NULL` samples excluded from all four (daily
    /// report use case - recorder-requirements.md §3.5). Returned in the
    /// same order as `tag_keys`.
    pub async fn aggregate(
        &self,
        group_key: &str,
        tag_keys: &[String],
        from_ms: i64,
        to_ms: i64,
    ) -> Result<Vec<TagAggregate>, TsQueryError> {
        aggregate::aggregate(&self.data_dir, group_key, tag_keys, from_ms, to_ms).await
    }

    /// Every group/tag this data directory's files describe, with the
    /// earliest/latest `ptime_ms` each group actually has data for -
    /// intended for a UI's period-selection/group-selection initialization,
    /// not a hot path (opens every recognized file in the directory; see
    /// [`mod@catalog`]'s module doc).
    pub async fn catalog(&self) -> Result<Catalog, TsQueryError> {
        catalog::catalog(&self.data_dir).await
    }

    /// The data directory this handle reads from.
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }
}
