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
//!
//! A candidate file whose `TsReader::open` fails with
//! `banto_tstore::TstoreError::Uninitialized` (writer created the file but
//! has not yet committed its schema - see that variant's doc comment) is
//! skipped, not errored: it contributes zero rows, same as a file that
//! never described `group_key` at all. Any other open failure is still a
//! hard [`TsQueryError::IncompatibleFile`].

use banto_tstore::{DataPlace, TsReader, TstoreError};

use crate::error::TsQueryError;
use crate::files::candidate_files;
use crate::plan::file_name;
use crate::types::{RawRange, RawRow};

/// Default cap on [`crate::TsQuery::read_range`]'s total row count when the
/// caller passes `None` for `max_rows`.
pub const DEFAULT_MAX_RAW_ROWS: usize = 100_000;

pub(crate) async fn read_range(
    place: &DataPlace,
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

    let files = candidate_files(place, from_ms, to_ms)?;
    let mut rows: Vec<RawRow> = Vec::new();

    for file in &files {
        // Pinned on a `DataPlace::Managed` (#623 PR7 - `TsReader::open_in`).
        let reader = match TsReader::open_in(place, &file_name(file)?).await {
            Ok(reader) => reader,
            // Uninitialized: the writer created this file but has not yet
            // committed its schema-creation transaction (raced) - not a
            // real error, treat it exactly like a file this crate has not
            // even listed yet: contribute zero rows and move on (see
            // `banto_tstore::TstoreError::Uninitialized`'s doc comment).
            Err(TstoreError::Uninitialized(_)) => continue,
            Err(err) => {
                return Err(TsQueryError::IncompatibleFile {
                    path: file.path.clone(),
                    message: err.to_string(),
                })
            }
        };
        let result = read_file(
            &reader, group_key, tag_keys, from_ms, to_ms, max_rows, &mut rows,
        )
        .await;
        reader.close().await;
        result?;
    }

    Ok(RawRange {
        tag_keys: tag_keys.to_vec(),
        rows,
    })
}

/// One opened file's rows appended to `rows` (nothing if this file never
/// described `group_key`).
async fn read_file(
    reader: &TsReader,
    group_key: &str,
    tag_keys: &[String],
    from_ms: i64,
    to_ms: i64,
    max_rows: usize,
    rows: &mut Vec<RawRow>,
) -> Result<(), TsQueryError> {
    {
        let Some(group) = reader.group(group_key) else {
            return Ok(()); // this file's schema never included this group - not an error.
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
    Ok(())
}
