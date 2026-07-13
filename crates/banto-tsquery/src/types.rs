//! Public result shapes for [`crate::TsQuery`]'s four query methods.

/// One row from [`crate::TsQuery::read_range`]: a `ptime_ms` plus one
/// `Option<f64>` per requested tag, in `RawRange::tag_keys` order. `None`
/// means either a stored NULL (missing sample, matching
/// [`banto_tstore::reader::Sample`]'s convention) *or* the tag not existing
/// at all in whichever file this row came from (a config-change gap) - both
/// collapse to the same "no value" signal a raw CSV export needs.
#[derive(Debug, Clone, PartialEq)]
pub struct RawRow {
    pub ptime_ms: i64,
    pub values: Vec<Option<f64>>,
}

/// [`crate::TsQuery::read_range`]'s result: every sample in
/// `[from_ms, to_ms]` (both bounds inclusive, matching
/// [`banto_tstore::reader::TsReader::read_range`]'s convention) across
/// however many files the range spans, concatenated in ascending `ptime_ms`
/// order.
#[derive(Debug, Clone, PartialEq)]
pub struct RawRange {
    /// Echoes the caller's requested tag order - `rows[i].values` is always
    /// this same length/order.
    pub tag_keys: Vec<String>,
    pub rows: Vec<RawRow>,
}

/// One tag's value within one [`Bin`]: either the envelope of every sample
/// that landed in the bin, or [`BinValue::Gap`] if none did (design
/// principle: server-side decimation must never hide a spike, and must never
/// hide a genuine absence of data either - see `decimate.rs`'s module doc).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BinValue {
    /// `min`/`max` across every non-NULL sample of this tag within the bin
    /// (`min == max` when exactly one sample landed in the bin, e.g. the
    /// near-native-resolution zoom-in path - see `decimate.rs`).
    Range { min: f64, max: f64 },
    /// Either the bin contained zero rows at all (collection stopped, or no
    /// file covers this time window), or every row it did contain had a
    /// NULL/absent value for this specific tag (this tag was not part of
    /// the group's configuration for whichever file(s) covered this bin, or
    /// the sample itself was a recorded NULL).
    Gap,
}

/// One time bin of [`crate::TsQuery::read_decimated`]'s result.
#[derive(Debug, Clone, PartialEq)]
pub struct Bin {
    /// The bin's start time in the aligned-grid case
    /// (`from_ms + bin_index * bin_ms`), or the exact sample `ptime_ms` in
    /// the near-native-resolution passthrough case (see `decimate.rs`'s
    /// module doc for when each applies) - never bin-center or bin-end.
    pub ptime_ms: i64,
    /// Aligned to `DecimatedRange::tag_keys` order.
    pub tags: Vec<BinValue>,
}

/// [`crate::TsQuery::read_decimated`]'s result.
#[derive(Debug, Clone, PartialEq)]
pub struct DecimatedRange {
    pub tag_keys: Vec<String>,
    /// Ascending by `ptime_ms`, one entry per bin covering `[from_ms,
    /// to_ms]` - including bins with no data at all (all-`Gap` `tags`), so
    /// the bin sequence itself is a complete, evenly-spaced (in the aligned
    /// case) timeline a chart can walk without needing to infer missing
    /// bins from gaps in this `Vec`.
    pub bins: Vec<Bin>,
    /// The bin width actually used, after clamping to the group's
    /// (file-max) `period_ms` - see `decimate.rs`'s module doc. May differ
    /// from what a naive `(to_ms - from_ms) / target_bins` would have
    /// produced.
    pub bin_ms: i64,
    pub from_ms: i64,
    pub to_ms: i64,
}

/// One tag's summary within [`crate::TsQuery::aggregate`]'s result.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TagAggregate {
    /// `None` only when `count == 0` (no non-NULL sample of this tag in the
    /// range at all - distinct from a tag that has genuine zero-valued
    /// samples, which reports `Some(0.0)`).
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub avg: Option<f64>,
    /// Number of non-NULL samples the aggregate was computed over (NULLs
    /// excluded, matching SQL `MIN`/`MAX`/`AVG`/`COUNT(column)` semantics -
    /// not `COUNT(*)`).
    pub count: i64,
}

/// One tag as reported by [`crate::TsQuery::catalog`].
#[derive(Debug, Clone, PartialEq)]
pub struct TagCatalogEntry {
    pub tag_key: String,
    /// From the most recently rotated file that still describes this tag
    /// (name/unit/decimals can drift across a config-change rotation; the
    /// latest file's metadata is treated as current - see `catalog.rs`).
    pub tag_name: String,
    pub unit: Option<String>,
    pub decimals: u8,
}

/// One group as reported by [`crate::TsQuery::catalog`].
#[derive(Debug, Clone, PartialEq)]
pub struct GroupCatalogEntry {
    pub group_key: String,
    pub group_name: String,
    /// Union of every tag ever seen for this group across every file in the
    /// data directory (not just the latest file's current columns) - a UI
    /// building a "select tags for this historical range" control needs to
    /// offer tags even if they were retired from the group's live config.
    pub tags: Vec<TagCatalogEntry>,
    /// Earliest/latest `ptime_ms` this group actually has a row for, across
    /// every file - `None` if every file that mentions this `group_key` has
    /// zero rows in its `samples_<n>` table (an edge case: a group defined
    /// but never actually collected into, e.g. immediately disabled).
    pub earliest_ms: Option<i64>,
    pub latest_ms: Option<i64>,
}

/// [`crate::TsQuery::catalog`]'s result - what a UI's period picker/group
/// selector initializes from without needing the tag registry (I1) at all,
/// mirroring `banto-tstore`'s own registry-independence principle.
#[derive(Debug, Clone, PartialEq)]
pub struct Catalog {
    pub groups: Vec<GroupCatalogEntry>,
}
