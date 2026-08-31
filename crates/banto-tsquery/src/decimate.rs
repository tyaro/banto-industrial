//! [`read_decimated`]: [`crate::TsQuery::read_decimated`]'s implementation -
//! the trend-viewport query (recorder-requirements.md §3.3 "ヒストリカル").
//!
//! ## Design principles (司令塔決定, restated here as the code's contract)
//!
//! - **min/max envelope, never average-only decimation**: a server-side
//!   reducer that returns one averaged point per bin can hide a genuine
//!   spike - exactly what a paperless recorder exists to *not* lose. Every
//!   bin therefore reports both the minimum and maximum of every sample
//!   that landed in it ([`crate::types::BinValue::Range`]), computed by
//!   SQL's own `MIN`/`MAX` aggregates (`fetch_binned`) - never by pulling
//!   raw rows into Rust and reducing them here.
//! - **binning happens in SQLite, per file**: `fetch_binned`'s `GROUP BY
//!   (ptime - ?) / ?` runs inside each file's own SQLite engine; this module
//!   only ever receives back at most `O(bins x tags)` rows total, never
//!   `O(samples)` - a 30-day x 1s range (~2.6M rows/group) still returns at
//!   most `target_bins` grouped rows per file. Merging multiple files'
//!   partial bin results (needed because a bin can straddle a local-midnight
//!   file boundary) happens in `acc`, a `Vec` sized to `num_bins x
//!   tag_keys.len()`, not to the row count.
//! - **gaps are never hidden**: a bin with zero rows at all, or zero
//!   non-NULL rows for one particular tag, is reported as
//!   [`crate::types::BinValue::Gap`] for that tag - never silently dropped
//!   from `DecimatedRange::bins` (every bin index in `[0, num_bins)` is
//!   always present) and never interpolated. See `fetch_binned`'s per-tag
//!   `COUNT(...)` check.
//! - **cross-file tag matching is by `tag_key`, not column position**: a
//!   tag's physical column (`c3`, say) can differ file to file after a
//!   config-change rotation. `plan.rs`'s `FilePlan::columns` already
//!   resolves each requested `tag_key` to this file's column name (or
//!   `None`); a `None` here means this file simply contributes no rows for
//!   that tag, which (combined with the zero-rows-in-this-file-for-this-tag
//!   case) becomes `Gap` exactly like any other missing sample.
//!
//! ## Bin width and the near-native-resolution passthrough
//!
//! `bin_ms` starts as `ceil((to_ms - from_ms + 1) / target_bins)` (the `+ 1`
//! makes the width inclusive of both endpoints, matching this crate's
//! inclusive-both-ends range convention; `target_bins ≒` viewport pixel
//! width) and is then clamped up to the queried group's
//! `period_ms` (the *maximum* `period_ms` seen across every file the range
//! touches, if it changed - clamping to a too-small period would just
//! recompute the same grouping SQLite already did per sample, for no
//! benefit; clamping to the true collection period is the floor below which
//! finer bins carry no additional information).
//!
//! When that clamp actually engages (`bin_ms == period_ms`) *and* the total
//! row count in range is small (`<= target_bins * 2` - i.e. the caller is
//! zoomed in close enough that there is roughly at most one real sample per
//! requested bin anyway), this module skips the `GROUP BY` entirely and
//! returns the raw rows themselves (`fetch_raw_passthrough`), each becoming
//! one [`crate::types::Bin`] at its *exact* `ptime_ms` rather than a
//! bin-boundary-aligned timestamp. Without this, every bin's reported time
//! would snap to `from_ms + i * bin_ms` regardless of exactly when its one
//! sample actually landed within the bin, which - once zoomed in far enough
//! that adjacent points are only a few bins apart - reads as a visible
//! "staircase" rather than the smooth line the true sample times would
//! draw. This mode does not synthesize extra `Gap` bins for periods with no
//! samples at all (unlike the binned path, which always fills `[0,
//! num_bins)`) - a deliberate, narrower gap contract than the binned path's,
//! acceptable here because a near-native-resolution zoom is close enough to
//! "show me the raw data" that missing points already read visually as a
//! wider gap between plotted points, without needing an explicit marker.

use std::path::{Path, PathBuf};

use sqlx::Row;

use crate::error::TsQueryError;
use crate::files::candidate_files;
use crate::plan::{plan_files, FilePlan};
use crate::types::{Bin, BinValue, DecimatedRange};

/// Upper bound on `target_bins`, guarding the `Vec` allocation in
/// `fetch_binned`/`build_gap_bins` (`num_bins x tag_keys.len()`) against a
/// caller mistake or hostile input - far above any real chart's pixel width
/// (recorder-requirements.md §4's cited upper bound is "10 系列 x 1万点").
/// Public so a UI can clamp its own `target_bins` before calling rather than
/// discovering the limit only via [`crate::TsQueryError::InvalidInput`].
pub const MAX_TARGET_BINS: usize = 200_000;

pub(crate) async fn read_decimated(
    data_dir: &Path,
    group_key: &str,
    tag_keys: &[String],
    from_ms: i64,
    to_ms: i64,
    target_bins: usize,
) -> Result<DecimatedRange, TsQueryError> {
    if from_ms > to_ms {
        return Err(TsQueryError::InvalidInput(format!(
            "from_ms ({from_ms}) は to_ms ({to_ms}) 以下である必要があります"
        )));
    }
    if target_bins == 0 {
        return Err(TsQueryError::InvalidInput(
            "target_bins は1以上である必要があります".to_string(),
        ));
    }
    if target_bins > MAX_TARGET_BINS {
        return Err(TsQueryError::InvalidInput(format!(
            "target_bins が上限を超えています: {target_bins}（上限 {MAX_TARGET_BINS}）"
        )));
    }

    let files = candidate_files(data_dir, from_ms, to_ms)?;
    let paths: Vec<PathBuf> = files.into_iter().map(|f| f.path).collect();
    let plans = plan_files(&paths, group_key, tag_keys).await?;

    // The *inclusive* width of [from_ms, to_ms] is (to_ms - from_ms + 1) ms,
    // not (to_ms - from_ms): using the exclusive span here would make
    // target_bins == 1 (say) produce *two* bins whenever to_ms - from_ms is
    // an exact multiple of the resulting bin width (bin_idx = (to_ms -
    // from_ms) / bin_ms would land exactly on the next bin rather than the
    // last valid index of the requested single bin). Using the +1-wide
    // value guarantees ceil(inclusive_width / target_bins) bins, evenly
    // spaced, always cover to_ms inside the last bin rather than spilling
    // into an extra one.
    let inclusive_width_ms = (to_ms - from_ms).max(0).saturating_add(1);
    let raw_bin_ms = inclusive_width_ms
        .checked_add(target_bins as i64 - 1)
        .map(|padded| padded / target_bins as i64)
        .unwrap_or(i64::MAX)
        .max(1);

    if plans.is_empty() {
        // No file describes this group at all in range - every bin is a
        // gap for every requested tag; nothing left to clamp bin_ms
        // against.
        let bins = build_gap_bins(from_ms, to_ms, raw_bin_ms, tag_keys.len())?;
        return Ok(DecimatedRange {
            tag_keys: tag_keys.to_vec(),
            bins,
            bin_ms: raw_bin_ms,
            from_ms,
            to_ms,
        });
    }

    let period_ms_used = plans.iter().map(|p| p.period_ms).max().unwrap();
    let bin_ms = raw_bin_ms.max(period_ms_used);

    if tag_keys.is_empty() {
        let bins = build_gap_bins(from_ms, to_ms, bin_ms, 0)?;
        return Ok(DecimatedRange {
            tag_keys: Vec::new(),
            bins,
            bin_ms,
            from_ms,
            to_ms,
        });
    }

    let mut use_raw_passthrough = false;
    if bin_ms == period_ms_used {
        let mut total_rows: i64 = 0;
        for plan in &plans {
            let sql = format!(
                "SELECT COUNT(*) FROM {} WHERE ptime >= ? AND ptime <= ?",
                plan.table_name
            );
            // AssertSqlSafe: plan.table_name は plan.rs の「SQL-identifier
            // safety」の通り is_safe_table_name で検証済みの samples_<n> のみ。
            let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
                .bind(from_ms)
                .bind(to_ms)
                .fetch_one(&plan.pool)
                .await?;
            total_rows += count;
        }
        use_raw_passthrough = total_rows <= target_bins as i64 * 2;
    }

    let bins = if use_raw_passthrough {
        fetch_raw_passthrough(&plans, tag_keys.len(), from_ms, to_ms).await?
    } else {
        fetch_binned(&plans, tag_keys.len(), from_ms, to_ms, bin_ms).await?
    };

    Ok(DecimatedRange {
        tag_keys: tag_keys.to_vec(),
        bins,
        bin_ms,
        from_ms,
        to_ms,
    })
}

fn build_gap_bins(
    from_ms: i64,
    to_ms: i64,
    bin_ms: i64,
    tag_count: usize,
) -> Result<Vec<Bin>, TsQueryError> {
    let num_bins = bin_count(from_ms, to_ms, bin_ms)?;
    Ok((0..num_bins)
        .map(|i| Bin {
            ptime_ms: from_ms + (i as i64) * bin_ms,
            tags: vec![BinValue::Gap; tag_count],
        })
        .collect())
}

fn bin_count(from_ms: i64, to_ms: i64, bin_ms: i64) -> Result<usize, TsQueryError> {
    let count = (to_ms - from_ms) / bin_ms + 1;
    usize::try_from(count).map_err(|_| {
        TsQueryError::InvalidInput(format!(
            "計算されたビン数が不正です: {count}（from_ms/to_ms/target_bins を確認してください）"
        ))
    })
}

/// One tag's accumulated envelope across every file's contribution to one
/// bin: `(min, max, valid_sample_count)`. Merged additively across files
/// because a bin can straddle a file boundary (local-midnight rotation) -
/// see this module's doc comment.
type Accumulator = Option<(f64, f64, i64)>;

async fn fetch_binned(
    plans: &[FilePlan],
    tag_count: usize,
    from_ms: i64,
    to_ms: i64,
    bin_ms: i64,
) -> Result<Vec<Bin>, TsQueryError> {
    let num_bins = bin_count(from_ms, to_ms, bin_ms)?;
    let mut acc: Vec<Vec<Accumulator>> = vec![vec![None; tag_count]; num_bins];

    for plan in plans {
        let present: Vec<(usize, &str)> = plan
            .columns
            .iter()
            .enumerate()
            .filter_map(|(tag_index, column)| column.as_deref().map(|c| (tag_index, c)))
            .collect();
        if present.is_empty() {
            continue;
        }

        let mut sql = String::from("SELECT (ptime - ?) / ? AS bin_idx");
        for (_, column) in &present {
            sql.push_str(&format!(", MIN({column}), MAX({column}), COUNT({column})"));
        }
        sql.push_str(&format!(
            " FROM {} WHERE ptime >= ? AND ptime <= ? GROUP BY bin_idx ORDER BY bin_idx",
            plan.table_name
        ));

        // AssertSqlSafe: `column`/`plan.table_name` は is_safe_table_name/
        // is_safe_column_name で検証済みの samples_<n>/c<i> のみで構成される
        // （plan.rs の「SQL-identifier safety」）。バインド値のみ可変。
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(from_ms)
            .bind(bin_ms)
            .bind(from_ms)
            .bind(to_ms)
            .fetch_all(&plan.pool)
            .await?;

        for row in rows {
            let bin_idx: i64 = row.try_get(0)?;
            if bin_idx < 0 {
                continue; // defensive: WHERE ptime >= from_ms already guarantees this cannot happen.
            }
            let bin_idx = bin_idx as usize;
            if bin_idx >= num_bins {
                continue; // defensive: WHERE ptime <= to_ms already guarantees this cannot happen.
            }

            for (result_col, (tag_index, _)) in present.iter().enumerate() {
                let base = 1 + result_col * 3;
                let min: Option<f64> = row.try_get(base)?;
                let max: Option<f64> = row.try_get(base + 1)?;
                let count: i64 = row.try_get(base + 2)?;
                if count == 0 {
                    continue; // every row in this bin was NULL for this tag - stays Gap.
                }
                let (min, max) = (min.unwrap(), max.unwrap());
                let slot = &mut acc[bin_idx][*tag_index];
                *slot = Some(match slot {
                    Some((emin, emax, ecount)) => (emin.min(min), emax.max(max), *ecount + count),
                    None => (min, max, count),
                });
            }
        }
    }

    Ok((0..num_bins)
        .map(|i| Bin {
            ptime_ms: from_ms + (i as i64) * bin_ms,
            tags: acc[i]
                .iter()
                .map(|slot| match slot {
                    Some((min, max, _)) => BinValue::Range {
                        min: *min,
                        max: *max,
                    },
                    None => BinValue::Gap,
                })
                .collect(),
        })
        .collect())
}

async fn fetch_raw_passthrough(
    plans: &[FilePlan],
    tag_count: usize,
    from_ms: i64,
    to_ms: i64,
) -> Result<Vec<Bin>, TsQueryError> {
    let mut bins = Vec::new();

    for plan in plans {
        let present: Vec<(usize, &str)> = plan
            .columns
            .iter()
            .enumerate()
            .filter_map(|(tag_index, column)| column.as_deref().map(|c| (tag_index, c)))
            .collect();
        if present.is_empty() {
            continue;
        }

        let mut column_list = String::from("ptime");
        for (_, column) in &present {
            column_list.push_str(", ");
            column_list.push_str(column);
        }
        let sql = format!(
            "SELECT {column_list} FROM {} WHERE ptime >= ? AND ptime <= ? ORDER BY ptime ASC",
            plan.table_name
        );

        // AssertSqlSafe: `column_list`/`plan.table_name` は is_safe_table_name/
        // is_safe_column_name で検証済みの samples_<n>/c<i> のみで構成される
        // （plan.rs の「SQL-identifier safety」）。
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(from_ms)
            .bind(to_ms)
            .fetch_all(&plan.pool)
            .await?;

        for row in rows {
            let ptime_ms: i64 = row.try_get(0)?;
            let mut tags = vec![BinValue::Gap; tag_count];
            for (result_col, (tag_index, _)) in present.iter().enumerate() {
                let value: Option<f64> = row.try_get(result_col + 1)?;
                if let Some(value) = value {
                    tags[*tag_index] = BinValue::Range {
                        min: value,
                        max: value,
                    };
                }
            }
            bins.push(Bin { ptime_ms, tags });
        }
    }

    // `plans` is already in ascending (date, seq) order and same-group files
    // never overlap in time (a rotation only ever starts a *new* file going
    // forward - see banto-tstore/src/writer.rs's module doc), and each
    // file's own rows are already `ORDER BY ptime ASC` - so the
    // concatenation above is already globally time-ordered without an
    // extra sort pass.
    Ok(bins)
}
