//! [`read_range`]: [`crate::TsQuery::read_range`]'s implementation - the one
//! query method that *does* pull materialized rows into Rust (by design,
//! this is the small-range/CSV-export path; large ranges belong on
//! `read_decimated` instead, which is why this enforces `max_rows`).
//!
//! Delegates the per-file row fetch to
//! [`banto_tstore::reader::TsReader::read_range_columns`] (see `plan.rs`'s
//! module doc for why the other three query methods cannot do the same) and
//! only adds: multi-file concatenation in file order (files never overlap in
//! time - see `writer.rs`'s rotation design - so no merge-sort is needed,
//! only concatenation), `tag_keys` projection/reordering, and the row-count
//! limit. Memory is bounded in the query itself: each file is read with
//! `LIMIT remaining_budget + 1` and only the requested tags' columns, so a
//! live writer appending mid-read cannot grow the fetch, and one extra row
//! is enough to know the budget was exceeded. Consequently
//! `TsQueryError::TooManyRows::count` means "at least this many" (rows
//! counted up to the first overshoot), not the true total.
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

        // Only the columns this file actually has are selected (an empty
        // selection still reads `ptime`, so a group present without any
        // requested tag keeps emitting all-None rows). `slots[i]` maps
        // tag_keys[i] to its position in the selected list.
        let mut selected: Vec<usize> = Vec::new();
        let slots: Vec<Option<usize>> = column_indices
            .iter()
            .map(|idx| {
                idx.map(|i| {
                    selected.iter().position(|&s| s == i).unwrap_or_else(|| {
                        selected.push(i);
                        selected.len() - 1
                    })
                })
            })
            .collect();

        // Bounded fetch: at most one row past the remaining budget, so an
        // oversized range is detected without materializing it.
        let remaining = max_rows.saturating_sub(rows.len());
        let samples = reader
            .read_range_columns(
                group_key,
                &selected,
                from_ms,
                to_ms,
                remaining.saturating_add(1),
            )
            .await?;
        if samples.len() > remaining {
            return Err(TsQueryError::TooManyRows {
                count: rows.len() + samples.len(),
                max: max_rows,
            });
        }

        for (ptime_ms, fetched) in samples {
            let values = slots
                .iter()
                .map(|slot| slot.and_then(|s| fetched[s]))
                .collect();
            rows.push(RawRow { ptime_ms, values });
        }
    }
    Ok(())
}
