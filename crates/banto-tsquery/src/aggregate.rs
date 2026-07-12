//! [`aggregate`]: [`crate::TsQuery::aggregate`]'s implementation - the daily
//! report query (recorder-requirements.md §3.5 "日報帳票": per-tag
//! min/max/avg over one day).
//!
//! Per tag: `MIN`/`MAX`/`SUM`/`COUNT` are computed by SQL over each file's
//! matching rows (`COUNT`/`SUM` both exclude NULLs, matching
//! `COUNT(column)`'s standard SQL semantics - not `COUNT(*)`), then merged
//! across files by summing `SUM`/`COUNT` and taking min-of-mins/max-of-maxes.
//! `SUM` is carried (not a partial `AVG`) so the final average is computed
//! from one division (`total_sum / total_count`) rather than a weighted
//! average of already-rounded per-file averages, avoiding a second layer of
//! floating-point error.

use std::path::{Path, PathBuf};

use sqlx::Row;

use crate::error::TsQueryError;
use crate::files::candidate_files;
use crate::plan::plan_files;
use crate::types::TagAggregate;

/// One tag's accumulated `(min, max, sum, valid_sample_count)` across every
/// file's contribution.
type Accumulator = Option<(f64, f64, f64, i64)>;

pub(crate) async fn aggregate(
    data_dir: &Path,
    group_key: &str,
    tag_keys: &[String],
    from_ms: i64,
    to_ms: i64,
) -> Result<Vec<TagAggregate>, TsQueryError> {
    if from_ms > to_ms {
        return Err(TsQueryError::InvalidInput(format!(
            "from_ms ({from_ms}) は to_ms ({to_ms}) 以下である必要があります"
        )));
    }
    if tag_keys.is_empty() {
        return Ok(Vec::new());
    }

    let files = candidate_files(data_dir, from_ms, to_ms)?;
    let paths: Vec<PathBuf> = files.into_iter().map(|f| f.path).collect();
    let plans = plan_files(&paths, group_key, tag_keys).await?;

    let mut acc: Vec<Accumulator> = vec![None; tag_keys.len()];

    for plan in &plans {
        let present: Vec<(usize, &str)> = plan
            .columns
            .iter()
            .enumerate()
            .filter_map(|(tag_index, column)| column.as_deref().map(|c| (tag_index, c)))
            .collect();
        if present.is_empty() {
            continue;
        }

        let select_list = present
            .iter()
            .map(|(_, column)| {
                format!("MIN({column}), MAX({column}), SUM({column}), COUNT({column})")
            })
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT {select_list} FROM {} WHERE ptime >= ? AND ptime <= ?",
            plan.table_name
        );

        let row = sqlx::query(&sql)
            .bind(from_ms)
            .bind(to_ms)
            .fetch_one(&plan.pool)
            .await?;

        for (result_col, (tag_index, _)) in present.iter().enumerate() {
            let base = result_col * 4;
            let count: i64 = row.try_get(base + 3)?;
            if count == 0 {
                continue; // no non-NULL sample of this tag in this file's range.
            }
            let min: f64 = row.try_get::<Option<f64>, _>(base)?.unwrap();
            let max: f64 = row.try_get::<Option<f64>, _>(base + 1)?.unwrap();
            let sum: f64 = row.try_get::<Option<f64>, _>(base + 2)?.unwrap();

            let slot = &mut acc[*tag_index];
            *slot = Some(match slot {
                Some((emin, emax, esum, ecount)) => {
                    (emin.min(min), emax.max(max), *esum + sum, *ecount + count)
                }
                None => (min, max, sum, count),
            });
        }
    }

    Ok(acc
        .into_iter()
        .map(|slot| match slot {
            Some((min, max, sum, count)) => TagAggregate {
                min: Some(min),
                max: Some(max),
                avg: Some(sum / count as f64),
                count,
            },
            None => TagAggregate {
                min: None,
                max: None,
                avg: None,
                count: 0,
            },
        })
        .collect())
}
