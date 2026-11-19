//! [`read_binned_stats`]: [`crate::TsQuery::read_binned_stats`]'s
//! implementation - per-interval min/max/avg/sum/count at a *fixed* bin width,
//! for interval CSV export (issue #1207 R3: 1 row per 1 min / 10 min / 1 h).
//!
//! Neither neighbour fits: `read_decimated` derives (and clamps) its bin
//! width from a target bin count and reports only a min/max envelope;
//! `aggregate` gives one average over the whole range. This query reuses
//! `decimate.rs`'s overflow-safe per-file `GROUP BY` and cross-file merge
//! (`fetch_accumulated`), additionally fetching `SUM` so that
//! `avg = total_sum / total_count` is formed *after* merging files (a bin can
//! straddle a local-midnight file boundary; an average of per-file averages
//! would be wrong).
//!
//! - bins start at `from_ms + i * bin_ms`; both ends of `[from_ms, to_ms]` are
//!   inclusive (a sample exactly at `to_ms` is counted in the last bin);
//! - `bin_ms` is exactly the caller's: no clamp to the collection period and
//!   no raw passthrough;
//! - every bin index is present; a tag with no non-NULL sample in a bin is
//!   `None` (never dropped, never interpolated);
//! - NULL samples are excluded from all four statistics, as in `aggregate`.
//!   There is no other non-finite handling: SQLite stores NaN as NULL (so it
//!   is excluded like any NULL); `+-inf` is kept as written, so `max`/`avg`
//!   can be `inf` (and an `inf + -inf` sum yields a NaN `avg`).
//!
//! Size limits (checked before any file is opened or allocated): the bin
//! count is capped by [`MAX_STAT_BINS`] and `bins x tags` by
//! [`MAX_STAT_CELLS`], since the dense accumulator is allocated up front even
//! when there is no history.

use banto_tstore::DataPlace;

use crate::decimate::{bin_starts, fetch_accumulated};
use crate::error::TsQueryError;
use crate::files::candidate_files;
use crate::plan::{close_plans, plan_files};
use crate::types::{BinnedStats, StatBin, TagStat};

/// Upper bound on the number of bins, guarding the `num_bins x
/// tag_keys.len()` allocation and the size of the result: each bin becomes
/// one CSV row, and 100,000 rows (about 69 days at 1 min) is already far
/// beyond what a spreadsheet user can sensibly open. Public so a UI can
/// choose a coarser interval up front instead of discovering the limit via
/// [`TsQueryError::InvalidInput`].
pub const MAX_STAT_BINS: usize = 100_000;

/// Upper bound on `bins x tags` (accumulator cells). Each cell is about 40
/// bytes, so the worst case is about 40 MB. The ChronoGazer CSV export uses
/// at most 8 tags x 100,000 bins = 800,000 cells. Checked before any
/// allocation, even when there is no history.
pub const MAX_STAT_CELLS: usize = 1_000_000;

pub(crate) async fn read_binned_stats(
    place: &DataPlace,
    group_key: &str,
    tag_keys: &[String],
    from_ms: i64,
    to_ms: i64,
    bin_ms: i64,
) -> Result<BinnedStats, TsQueryError> {
    if from_ms > to_ms {
        return Err(TsQueryError::InvalidInput(format!(
            "from_ms ({from_ms}) は to_ms ({to_ms}) 以下である必要があります"
        )));
    }
    if bin_ms <= 0 {
        return Err(TsQueryError::InvalidInput(format!(
            "bin_ms は1以上である必要があります: {bin_ms}"
        )));
    }
    let count = (i128::from(to_ms) - i128::from(from_ms)) / i128::from(bin_ms) + 1;
    if count > MAX_STAT_BINS as i128 {
        return Err(TsQueryError::InvalidInput(format!(
            "ビン数が上限を超えています: {count}（上限 {MAX_STAT_BINS}）。間隔を広げるか期間を狭めてください"
        )));
    }
    let cells = count * tag_keys.len() as i128;
    if cells > MAX_STAT_CELLS as i128 {
        return Err(TsQueryError::InvalidInput(format!(
            "ビン数 × タグ数が上限を超えています: {count} × {}（上限 {MAX_STAT_CELLS}）。期間を狭める・間隔を広げる・タグ数を絞るのいずれかをしてください",
            tag_keys.len()
        )));
    }

    let (starts, acc) = if tag_keys.is_empty() {
        let starts = bin_starts(from_ms, to_ms, bin_ms)?;
        let acc = vec![Vec::new(); starts.len()];
        (starts, acc)
    } else {
        let files = candidate_files(place, from_ms, to_ms)?;
        let plans = plan_files(place, &files, group_key, tag_keys).await?;
        let result = fetch_accumulated(&plans, tag_keys.len(), from_ms, to_ms, bin_ms, true).await;
        close_plans(plans).await;
        result?
    };

    let bins = starts
        .into_iter()
        .zip(acc)
        .map(|(ptime_ms, slots)| StatBin {
            ptime_ms,
            tags: slots
                .into_iter()
                .map(|slot| {
                    slot.map(|(min, max, count, sum)| TagStat {
                        min,
                        max,
                        avg: sum / count as f64,
                        sum,
                        count,
                    })
                })
                .collect(),
        })
        .collect();

    Ok(BinnedStats {
        tag_keys: tag_keys.to_vec(),
        bin_ms,
        bins,
    })
}
