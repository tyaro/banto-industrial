//! The self-described-metadata shapes every data file carries alongside its
//! samples (design principle: "各ファイルは自己記述的: タグ↔列の対応
//! メタデータを同梱し、後続クレート（I4クエリ層）がタグレジストリ無しでも
//! ファイル単体を解釈できる"). [`GroupMeta`]/[`ColumnMeta`] are what
//! [`crate::reader::TsReader::groups`] hands back; [`FileMeta`] additionally
//! carries the file-level bookkeeping (`schema.rs`/`writer.rs` need
//! `config_hash` and `format_version`, which a reader has no use for beyond
//! this crate, so `FileMeta` itself stays `pub(crate)`).

use crate::date::LocalDate;

/// One tag's column as recorded in `tstore_columns` - the read-back mirror
/// of [`crate::config::TagColumn`], plus the physical `column_name` (`"c1"`,
/// `"c2"`, ...) it was assigned.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnMeta {
    pub column_name: String,
    pub tag_key: String,
    pub tag_name: String,
    pub data_type: String,
    pub unit: Option<String>,
    pub decimals: u8,
}

/// One collection group's metadata as recorded in `tstore_groups` +
/// `tstore_columns` - the read-back mirror of [`crate::config::GroupConfig`],
/// plus the physical `table_name` (`"samples_1"`, ...) it was assigned.
/// `columns` is always in physical column order (`c1`, `c2`, ...), which is
/// also the order [`crate::writer::TsWriter::append`]'s `values` slice must
/// follow.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupMeta {
    pub key: String,
    pub name: String,
    pub period_ms: u32,
    pub table_name: String,
    pub columns: Vec<ColumnMeta>,
}

/// Everything read back from one data file's `tstore_meta`/`tstore_groups`/
/// `tstore_columns` tables - the full self-described state
/// [`crate::reader::TsReader::open`] and [`crate::writer::TsWriter`]'s
/// same-day-file-reuse check both need (the latter only actually reads
/// `config_hash`/`format_version` off of this - see `schema.rs::read_file_meta`
/// call sites).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FileMeta {
    pub format_version: i64,
    pub created_at_ms: i64,
    pub local_date: LocalDate,
    pub config_hash: String,
    pub groups: Vec<GroupMeta>,
}
