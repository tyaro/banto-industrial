//! [`read_range`]: [`crate::TsQuery::read_range`]'s implementation - the one
//! query method that *does* pull materialized rows into Rust (by design,
//! this is the small-range/CSV-export path; large ranges belong on
//! `read_decimated` instead, which is why this enforces `max_rows`).
//!
//! Delegates the actual per-file row fetch to
//! [`banto_tstore::reader::TsReader::read_range`] (see `plan.rs`'s module
//! doc for why the other three query methods cannot do the same) and only
//! adds: multi-file concatenation in file order (files never overlap in
//! time - see `writer.rs`'s rotation design - so no merge-sort is needed,
//! only concatenation), `tag_keys` projection/reordering, and the row-count
//! limit.

use std::path::Path;

use banto_tstore::TsReader;

use crate::error::TsQueryError;
use crate::files::candidate_files;
use crate::types::{RawRange, RawRow};

/// Default cap on [`crate::TsQuery::read_range`]'s total row count when the
/// caller passes `None` for `max_rows`.
pub const DEFAULT_MAX_RAW_ROWS: usize = 100_000;

pub(crate) async fn read_range(
    data_dir: &Path,
    group_key: &str,
    tag_keys: &[String],
    from_ms: i64,
    to_ms: i64,
    max_rows: Option<usize>,
) -> Result<RawRange, TsQueryError> {
    if from_ms > to_ms {
        return Err(TsQueryError::InvalidInput(format!(
            "from_ms ({from_ms}) は to_ms ({to_ms}) 以下である必要があります"
        )));
    }
    let max_rows = max_rows.unwrap_or(DEFAULT_MAX_RAW_ROWS);

    let files = candidate_files(data_dir, from_ms, to_ms)?;
    let mut rows: Vec<RawRow> = Vec::new();

    for file in &files {
        let reader = match TsReader::open(&file.path).await {
            Ok(reader) => reader,
            Err(err) => {
                return Err(TsQueryError::IncompatibleFile {
                    path: file.path.clone(),
                    message: err.to_string(),
                })
            }
        };
        let Some(group) = reader.group(group_key) else {
            continue; // this file's schema never included this group - not an error.
        };

        // tag_keys[i] -> index into this file's GroupMeta::columns, or None
        // if this file's frozen schema never had that tag (config-change
        // gap - "ファイル跨ぎは tag_key マッチ").
        let column_indices: Vec<Option<usize>> = tag_keys
            .iter()
            .map(|tag_key| group.columns.iter().position(|c| &c.tag_key == tag_key))
            .collect();

        let samples = reader.read_range(group_key, from_ms, to_ms).await?;
        if rows.len() + samples.len() > max_rows {
            return Err(TsQueryError::TooManyRows {
                count: rows.len() + samples.len(),
                max: max_rows,
            });
        }

        for sample in samples {
            let values = column_indices
                .iter()
                .map(|idx| idx.and_then(|i| sample.values[i]))
                .collect();
            rows.push(RawRow {
                ptime_ms: sample.ptime_ms,
                values,
            });
        }
    }

    Ok(RawRange {
        tag_keys: tag_keys.to_vec(),
        rows,
    })
}
