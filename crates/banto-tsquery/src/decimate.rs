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
//!   bin_idx` (`floor((ptime - from_ms) / bin_ms)`, written so it cannot
//!   overflow `i64` - see `binned_sql`) runs inside each file's own SQLite
//!   engine; this module
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

    // `from_ms`/`to_ms` are used as-is from here on - both for choosing
    // files and for every SQL comparison - so a stored row whose `ptime_ms`
    // lies in the requested range is always a candidate, however extreme the
    // range (`TsWriter::append` does not restrict `ptime_ms`, so such rows
    // can exist). The only arithmetic that could overflow `i64` - the width
    // of the range, the bin width/count and each row's bin index - is done
    // in `i128` on the Rust side (`raw_bin_ms`/`bin_starts`) or with an
    // overflow-free decomposition inside SQLite (`binned_sql`).
    let files = candidate_files(data_dir, from_ms, to_ms)?;
    let paths: Vec<PathBuf> = files.into_iter().map(|f| f.path).collect();
    let plans = plan_files(&paths, group_key, tag_keys).await?;

    let raw_bin_ms = raw_bin_ms(from_ms, to_ms, target_bins);

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

/// `ceil(inclusive_width / target_bins)`, computed in `i128`.
///
/// The *inclusive* width of `[from_ms, to_ms]` is `to_ms - from_ms + 1`, not
/// `to_ms - from_ms`: using the exclusive span would make `target_bins == 1`
/// (say) produce *two* bins whenever `to_ms - from_ms` is an exact multiple
/// of the resulting bin width. With the inclusive width,
/// `ceil(width / target_bins)`-wide bins always put `to_ms` inside the last
/// of at most `target_bins` bins.
///
/// The width itself is up to `2^64` (`i64::MIN..=i64::MAX`), which does not
/// fit `i64` - hence `i128`. The resulting bin width fits `i64` except when
/// `ceil(width / target_bins)` itself exceeds `i64::MAX` (only possible for
/// `target_bins <= 2`, since `width <= 2^64`); it is then held at
/// `i64::MAX` (the public `DecimatedRange::bin_ms` is an `i64`), and the bin
/// count becomes `ceil(width / i64::MAX)`, 2 or 3 - the one case where the
/// result has more than `target_bins` bins (documented on
/// [`crate::TsQuery::read_decimated`]). That count is the fewest `i64`-wide
/// bins that can cover the whole requested range.
fn raw_bin_ms(from_ms: i64, to_ms: i64, target_bins: usize) -> i64 {
    let width = i128::from(to_ms) - i128::from(from_ms) + 1;
    let target_bins = target_bins as i128;
    let ceil = (width + target_bins - 1) / target_bins;
    i64::try_from(ceil).unwrap_or(i64::MAX).max(1)
}

fn build_gap_bins(
    from_ms: i64,
    to_ms: i64,
    bin_ms: i64,
    tag_count: usize,
) -> Result<Vec<Bin>, TsQueryError> {
    Ok(bin_starts(from_ms, to_ms, bin_ms)?
        .into_iter()
        .map(|ptime_ms| Bin {
            ptime_ms,
            tags: vec![BinValue::Gap; tag_count],
        })
        .collect())
}

/// Every bin's start time, `from_ms + i * bin_ms` for
/// `i in 0..=(to_ms - from_ms) / bin_ms` - the aligned grid whose origin is
/// the caller's own `from_ms` and whose last bin contains `to_ms`.
///
/// Computed in `i128` (`to_ms - from_ms` can exceed `i64::MAX`). Every start
/// fits `i64`: `i * bin_ms <= to_ms - from_ms` for every `i` in range, so
/// each start lies in `[from_ms, to_ms]`. The `try_from`s below therefore
/// never fail in practice - they return an error rather than panic only
/// because this module takes arbitrary caller input.
///
/// The count is at most `target_bins` (or 3, see [`raw_bin_ms`]): `bin_ms`
/// is never smaller than `raw_bin_ms`, so `(to_ms - from_ms) / bin_ms + 1 =
/// ceil(width / bin_ms) <= ceil(width / raw_bin_ms)`.
fn bin_starts(from_ms: i64, to_ms: i64, bin_ms: i64) -> Result<Vec<i64>, TsQueryError> {
    let span = i128::from(to_ms) - i128::from(from_ms);
    let count = span / i128::from(bin_ms) + 1;
    let count = usize::try_from(count).map_err(|_| {
        TsQueryError::InvalidInput(format!(
            "計算されたビン数が不正です: {count}（from_ms/to_ms/target_bins を確認してください）"
        ))
    })?;
    (0..count)
        .map(|i| {
            let start = i128::from(from_ms) + i as i128 * i128::from(bin_ms);
            i64::try_from(start).map_err(|_| {
                TsQueryError::InvalidInput(format!("ビンの開始時刻が i64 に収まりません: {start}"))
            })
        })
        .collect()
}

/// One tag's accumulated envelope across every file's contribution to one
/// bin: `(min, max, valid_sample_count)`. Merged additively across files
/// because a bin can straddle a file boundary (local-midnight rotation) -
/// see this module's doc comment.
type Accumulator = Option<(f64, f64, i64)>;

/// The per-file `GROUP BY` query: one row per non-empty bin, `bin_idx` first,
/// then `MIN`/`MAX`/`COUNT` for every `present` column.
///
/// ## Bin index without overflow
///
/// The bin index of a row is `floor((ptime - from_ms) / bin_ms)`. The
/// obvious SQL `(ptime - ?) / ?` is wrong at the extremes: `ptime - from_ms`
/// can exceed `i64::MAX` (e.g. `from_ms = i64::MIN`), and SQLite then
/// silently switches that subtraction to `REAL`, so the column decodes as a
/// float, not the `i64` this module reads.
///
/// Instead, with `b = bin_ms > 0`, split both times by floor division:
/// `p = qp*b + rp` and `f = qf*b + rf` with `0 <= rp, rf < b`. Then
/// `(p - f) / b = (qp - qf) + (rp - rf) / b` with `-1 < (rp - rf) / b < 1`,
/// so
///
/// ```text
/// floor((p - f) / b) = qp - qf - (rp < rf ? 1 : 0)
/// ```
///
/// Every step fits `i64`:
/// - `qf = from_ms.div_euclid(b)` and `rf = from_ms.rem_euclid(b)` are
///   computed in Rust and bound as parameters.
/// - SQLite's `/` and `%` truncate toward zero (C semantics), so
///   `bin_q = ptime / b` and `bin_r = ptime % b` (`-b < bin_r < b`) are
///   turned into the floor pair by `qp = bin_q - (bin_r < 0)` and
///   `rp = bin_r + (bin_r < 0 ? b : 0)`. This is what makes the index
///   right for negative times (before 1970). Neither step overflows:
///   `ptime / b` with `b >= 1` never does; `bin_q - 1` happens only when
///   `bin_r < 0`, which rules out `bin_q = i64::MIN` (that needs `b = 1`,
///   where `bin_r = 0`); and `bin_r + b` lies in `(0, b)`.
/// - `qp - qf` equals the final index plus 0 or 1. For every row the
///   `WHERE ptime >= from_ms AND ptime <= to_ms` keeps, that index is in
///   `[0, number of bins)`, which [`bin_starts`] has already bounded (at
///   most `MAX_TARGET_BINS`), so the subtraction's true result is small and
///   SQLite never overflows it.
///
/// `tests::sql_bin_index_matches_i128_reference` checks this against the
/// `i128` reference for negative/positive times, `from_ms < 0 <= ptime`,
/// both ends of the range, `b = 1`, huge `b` and `i64::MIN..=i64::MAX`.
fn binned_sql(table_name: &str, present: &[(usize, &str)]) -> String {
    let mut sql = String::from(
        "SELECT (bin_q - (bin_r < 0)) - ? \
         - ((bin_r + CASE WHEN bin_r < 0 THEN ? ELSE 0 END) < ?) AS bin_idx",
    );
    for (_, column) in present {
        sql.push_str(&format!(", MIN({column}), MAX({column}), COUNT({column})"));
    }
    sql.push_str(&format!(
        " FROM (SELECT ptime / ? AS bin_q, ptime % ? AS bin_r, * FROM {table_name} \
         WHERE ptime >= ? AND ptime <= ?) GROUP BY bin_idx ORDER BY bin_idx"
    ));
    sql
}

/// Run [`binned_sql`] against one file. Bind order follows the `?`s in
/// [`binned_sql`]'s text: `qf`, `b`, `rf` (outer select), then `b`, `b`,
/// `from_ms`, `to_ms` (inner select).
async fn fetch_bin_rows(
    pool: &sqlx::SqlitePool,
    table_name: &str,
    present: &[(usize, &str)],
    from_ms: i64,
    to_ms: i64,
    bin_ms: i64,
) -> Result<Vec<sqlx::sqlite::SqliteRow>, sqlx::Error> {
    let from_q = from_ms.div_euclid(bin_ms);
    let from_r = from_ms.rem_euclid(bin_ms);
    // AssertSqlSafe: `column`/`table_name` は is_safe_table_name/
    // is_safe_column_name で検証済みの samples_<n>/c<i> のみで構成される
    // （plan.rs の「SQL-identifier safety」）。バインド値のみ可変。
    sqlx::query(sqlx::AssertSqlSafe(binned_sql(table_name, present)))
        .bind(from_q)
        .bind(bin_ms)
        .bind(from_r)
        .bind(bin_ms)
        .bind(bin_ms)
        .bind(from_ms)
        .bind(to_ms)
        .fetch_all(pool)
        .await
}

async fn fetch_binned(
    plans: &[FilePlan],
    tag_count: usize,
    from_ms: i64,
    to_ms: i64,
    bin_ms: i64,
) -> Result<Vec<Bin>, TsQueryError> {
    let starts = bin_starts(from_ms, to_ms, bin_ms)?;
    let num_bins = starts.len();
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

        let rows = fetch_bin_rows(
            &plan.pool,
            &plan.table_name,
            &present,
            from_ms,
            to_ms,
            bin_ms,
        )
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

    Ok(starts
        .into_iter()
        .zip(acc)
        .map(|(ptime_ms, slots)| Bin {
            ptime_ms,
            tags: slots
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

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    /// The bin index this module must produce, computed where nothing can
    /// overflow (`i128`, floor division).
    fn reference_index(ptime_ms: i64, from_ms: i64, bin_ms: i64) -> i128 {
        (i128::from(ptime_ms) - i128::from(from_ms)).div_euclid(i128::from(bin_ms))
    }

    /// The aligned-grid contract: first bin at `from_ms`, evenly spaced by
    /// `bin_ms`, every start inside `[from_ms, to_ms]`, last bin contains
    /// `to_ms`.
    fn assert_grid_covers(starts: &[i64], from_ms: i64, to_ms: i64, bin_ms: i64) {
        assert!(!starts.is_empty());
        assert_eq!(starts[0], from_ms);
        for pair in starts.windows(2) {
            assert_eq!(
                i128::from(pair[1]) - i128::from(pair[0]),
                i128::from(bin_ms)
            );
        }
        let last = *starts.last().unwrap();
        assert!(last <= to_ms);
        assert!(i128::from(last) + i128::from(bin_ms) > i128::from(to_ms));
    }

    #[test]
    fn raw_bin_ms_and_grid_cover_the_whole_requested_range() {
        let ranges = [
            (i64::MIN, i64::MAX),
            (i64::MIN, i64::MIN),
            (i64::MAX, i64::MAX),
            (i64::MIN, -1),
            (0, i64::MAX),
            (-1, 0),
            (i64::MAX / 4 + 1, i64::MAX / 4 + 1),
            (-86_400_000 * 3 - 7, 86_400_000 * 5 + 11),
        ];
        for (from_ms, to_ms) in ranges {
            for target_bins in [1usize, 2, 3, 7, 1_000, MAX_TARGET_BINS] {
                let bin_ms = raw_bin_ms(from_ms, to_ms, target_bins);
                assert!(bin_ms >= 1);
                let starts = bin_starts(from_ms, to_ms, bin_ms).unwrap();
                assert_grid_covers(&starts, from_ms, to_ms, bin_ms);
                let width = i128::from(to_ms) - i128::from(from_ms) + 1;
                let ideal_bin_ms = (width + target_bins as i128 - 1) / target_bins as i128;
                if ideal_bin_ms <= i128::from(i64::MAX) {
                    assert_eq!(i128::from(bin_ms), ideal_bin_ms.max(1));
                    assert!(
                        starts.len() <= target_bins,
                        "{from_ms}..{to_ms} / {target_bins}"
                    );
                } else {
                    // bin_ms held at i64::MAX: the documented up-to-3 case.
                    assert_eq!(bin_ms, i64::MAX);
                    assert!(starts.len() <= 3);
                }
            }
        }
        // MIN..MAX in one requested bin: [MIN, -1, MAX - 1] with bin_ms = MAX.
        let starts = bin_starts(i64::MIN, i64::MAX, raw_bin_ms(i64::MIN, i64::MAX, 1)).unwrap();
        assert_eq!(starts, vec![i64::MIN, -1, i64::MAX - 1]);
    }

    /// `binned_sql`'s overflow-free bin index against the `i128` reference.
    /// Each case table holds rows whose value is their expected index, so a
    /// group whose `MIN`/`MAX` differ from its `bin_idx` means some row was
    /// put in the wrong bin.
    #[tokio::test]
    async fn sql_bin_index_matches_i128_reference() {
        let options: SqliteConnectOptions = "sqlite::memory:".parse().unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();

        let froms = [
            i64::MIN,
            i64::MIN + 1,
            -1_000_000_000_007,
            -86_400_001,
            -8,
            -7,
            -1,
            0,
            1,
            7,
            1_000_000_000_003,
            i64::MAX / 4 + 1,
            i64::MAX - 10,
            i64::MAX,
        ];
        let bin_widths = [
            1,
            2,
            3,
            7,
            1_000,
            86_400_000,
            i64::MAX / 3,
            i64::MAX / 2,
            i64::MAX / 2 + 1,
            i64::MAX - 1,
            i64::MAX,
        ];
        let mut checked_rows = 0usize;
        for from_ms in froms {
            for bin_ms in bin_widths {
                let b = i128::from(bin_ms);
                let f = i128::from(from_ms);
                let mut candidates: Vec<i128> = [
                    0,
                    1,
                    2,
                    b - 1,
                    b,
                    b + 1,
                    2 * b - 1,
                    2 * b,
                    3 * b,
                    1_000 * b + 17,
                ]
                .iter()
                .map(|k| f + k)
                .collect();
                candidates.extend([
                    i128::from(i64::MIN),
                    i128::from(i64::MIN) + 1,
                    -b - 1,
                    -b,
                    -b + 1,
                    -1,
                    0,
                    1,
                    b - 1,
                    b,
                    b + 1,
                    i128::from(i64::MAX) - 2,
                    i128::from(i64::MAX) - 1,
                    i128::from(i64::MAX),
                ]);
                let mut ptimes: Vec<i64> = candidates
                    .into_iter()
                    .filter_map(|p| i64::try_from(p).ok())
                    .filter(|&p| p >= from_ms)
                    // The index is the position in a grid of at most
                    // MAX_TARGET_BINS bins in real use; keep it well inside
                    // what an f64 value column holds exactly.
                    .filter(|&p| reference_index(p, from_ms, bin_ms) < (1 << 40))
                    .collect();
                ptimes.sort_unstable();
                ptimes.dedup();

                sqlx::query("DROP TABLE IF EXISTS samples_1")
                    .execute(&pool)
                    .await
                    .unwrap();
                sqlx::query("CREATE TABLE samples_1 (ptime INTEGER NOT NULL, c0 REAL)")
                    .execute(&pool)
                    .await
                    .unwrap();
                for &p in &ptimes {
                    sqlx::query("INSERT INTO samples_1 (ptime, c0) VALUES (?, ?)")
                        .bind(p)
                        .bind(reference_index(p, from_ms, bin_ms) as f64)
                        .execute(&pool)
                        .await
                        .unwrap();
                }

                let rows =
                    fetch_bin_rows(&pool, "samples_1", &[(0, "c0")], from_ms, i64::MAX, bin_ms)
                        .await
                        .unwrap_or_else(|e| panic!("from={from_ms} b={bin_ms}: {e}"));
                let mut seen = 0i64;
                for row in rows {
                    let bin_idx: i64 = row
                        .try_get(0)
                        .unwrap_or_else(|e| panic!("from={from_ms} b={bin_ms}: {e}"));
                    let min: f64 = row.try_get(1).unwrap();
                    let max: f64 = row.try_get(2).unwrap();
                    let count: i64 = row.try_get(3).unwrap();
                    assert_eq!(
                        (min, max),
                        (bin_idx as f64, bin_idx as f64),
                        "from={from_ms} b={bin_ms} bin_idx={bin_idx}"
                    );
                    seen += count;
                }
                assert_eq!(seen as usize, ptimes.len(), "from={from_ms} b={bin_ms}");
                checked_rows += ptimes.len();
            }
        }
        assert!(checked_rows > 1_000, "only {checked_rows} rows checked");
    }
}
