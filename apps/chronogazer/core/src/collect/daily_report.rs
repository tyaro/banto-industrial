//! 日報帳票のための読み出し（#1207 R3 の日報）: 「日報」ページで選んだ表示グループ・
//! **1 日**のタグについて、**1 時間ごと**の最小・最大・平均・件数と、**その日全体**の
//! 最小・最大・平均・件数をデータファイル（`banto-tsquery`）から返す口。帳票の
//! 組み立て・印刷は画面の側で、ここは値を返すだけ。期間 CSV（[`super::export`]）の
//! 双子で、検証・経路・絞り・「理由を返さない」作法を揃えている。
//!
//! # 形と経路
//!
//! * REST `GET /api/collect/daily-report?tagIds=1,2&fromMs=&toMs=` と Tauri
//!   `collect_daily_report` の双子。どちらも [`validate_daily_report_request`] →
//!   [`CollectorService::daily_report`] の**同じ 2 つの関数**を通る。
//! * 床は読み取りと同じ `viewer` 以上。読み取りなので監査しない。収集の操作キューは
//!   通らない。結末は [`Readout`]（`notRunning` は返さない。読めなかった・時間切れは
//!   [`Readout::Unavailable`]、理由はログだけ）。
//! * 常に履歴の「長い読み出し」と同じ絞り（[`run_long_read`]）を通す。
//!
//! # 「1 日」の範囲（半開区間）
//!
//! 画面が**ローカルの 0 時**を `fromMs`、**翌日のローカル 0 時**を `toMs` として渡す
//! （タイムゾーンの扱いは画面の側。夏時間の日は 23 / 25 時間になる）。日は
//! **`[fromMs, toMs)`（`toMs` を含まない）**。一方 `banto-tsquery` の
//! `read_binned_stats` / `aggregate` は**両端を含む**ので、tsquery には
//! **`toMs - 1`** を渡す。こうしないと、翌日 0 時ちょうどのサンプルが日の合計に入り、
//! 幅が 1 時間の倍数のときは 25 本目の（ほぼ空の）区間も増える。
//!
//! **日の合計は時間ごとの値と同じ 1 回の読み出し**（`read_binned_stats`）から、
//! 区間の最小の最小・最大の最大・件数の合計・合計値の合計 ÷ 件数（件数で重みを付けた
//! 平均）として組む（[`merge_day`]）。`aggregate` を別に呼ぶと、収集中に 2 回の読み出しの
//! 間でサンプルが増え、日の件数・最小・最大が時間の行と食い違うことがあるため。
//!
//! # 上限
//!
//! * タグは 1〜[`HISTORY_MAX_TAGS`] 本（重複は 1 本。期間 CSV と同じ文言）、
//! * 期間 `toMs - fromMs` は [`DAILY_REPORT_MIN_WINDOW_MS`]（1 時間）以上
//!   [`DAILY_REPORT_MAX_WINDOW_MS`]（25 時間）以下。区間の幅は 1 時間固定
//!   （[`DAILY_REPORT_BIN_MS`]）で、区間は `fromMs + i * 1 時間` から
//!   `ceil(幅 / 1 時間)` 本（最大 25）。
//!
//! # 返すもの
//!
//! [`CollectDailyReport::tags`] は**要求順**でレジストリに居るタグだけ（列の定義）。
//! 居ないタグは [`CollectDailyReport::unknown_tag_ids`]。[`DailyReportHour::values`]・
//! [`CollectDailyReport::day`] は `tags` と同じ長さ・順で、サンプルが無い（または
//! 最小・最大・平均が非有限の）タグは `null`（期間 CSV と同じ。非有限は欠測として扱う）。
//! 日の合計は、サンプルのある区間のどれかに非有限があれば（時間の行と同じく）`null`、
//! なければ時間の行から組んだ値で、**時間の行と必ず一致する**（件数は時間の件数の和）。
//! 時間の行は**サンプルが無くても必ず出す**（全タグ `null`）。
//!
//! シミュレーション接続の値は記録されない（`null`）。タグを別グループへ移す前の値は
//! 読めない（履歴と同じ割り切り）。

use banto_core::{BantoError, FieldError};
use banto_tsquery::{TagStat, TsQuery, TsQueryError};
use serde::Serialize;
use sqlx::SqlitePool;

use super::history::{
    field_error, run_long_read, tag_placements, LongReadStop, TagPlacement,
    HISTORY_LONG_READ_TIMEOUT, HISTORY_MAX_TAGS,
};
use super::{CollectorService, Readout};

/// 区間の幅（1 時間、固定）。
pub const DAILY_REPORT_BIN_MS: i64 = 3_600_000;

/// 期間の下限（1 時間）。
pub const DAILY_REPORT_MIN_WINDOW_MS: i64 = DAILY_REPORT_BIN_MS;

/// 期間の上限（25 時間 = 夏時間の日）。
pub const DAILY_REPORT_MAX_WINDOW_MS: i64 = 25 * DAILY_REPORT_BIN_MS;

/// 検証を通った日報の要求。**[`validate_daily_report_request`] でしか作れない**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyReportRequest {
    tag_ids: Vec<i64>,
    from_ms: i64,
    to_ms: i64,
}

impl DailyReportRequest {
    pub fn tag_ids(&self) -> &[i64] {
        &self.tag_ids
    }
}

/// 最小・最大・平均・件数（1 タグの 1 区間、または 1 日全体）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
#[derive(utoipa::ToSchema)]
pub struct DailyReportStat {
    pub min: f64,
    pub max: f64,
    pub avg: f64,
    pub count: i64,
}

/// 1 時間ぶんの行。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
#[derive(utoipa::ToSchema)]
pub struct DailyReportHour {
    /// 区間の始まり（UTC epoch ミリ秒）。
    pub t_ms: i64,
    /// [`CollectDailyReport::tags`] と同じ長さ・順。サンプルが無いタグは `null`。
    pub values: Vec<Option<DailyReportStat>>,
}

/// 列（タグ）の定義。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
#[derive(utoipa::ToSchema)]
pub struct DailyReportTag {
    pub tag_id: i64,
    /// レジストリ上シミュレーション接続か（値は記録されない）。
    pub simulation: bool,
}

/// 日報の読み出し結果（[`Readout::Ready`] の中身）。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
#[derive(utoipa::ToSchema)]
pub struct CollectDailyReport {
    pub from_ms: i64,
    pub to_ms: i64,
    /// 常に 3_600_000。
    pub interval_ms: i64,
    /// 列。要求順（重複は除く）。レジストリに居ないタグは含まない。
    pub tags: Vec<DailyReportTag>,
    pub unknown_tag_ids: Vec<i64>,
    /// 時刻の昇順。サンプルが無い時間も行は出す。
    pub hours: Vec<DailyReportHour>,
    /// `tags` と同じ長さ・順の、その日全体の統計量。サンプルが無いタグは `null`。
    pub day: Vec<Option<DailyReportStat>>,
}

/// 日報の要求を検証する。REST と Tauri の**両方がここを通る**。誤りはすべて
/// `field_errors` に並べて返す（フィールド名はワイヤの綴り）。
pub fn validate_daily_report_request(
    tag_ids: &[i64],
    from_ms: i64,
    to_ms: i64,
) -> Result<DailyReportRequest, BantoError> {
    let mut errors: Vec<FieldError> = Vec::new();

    let mut unique: Vec<i64> = Vec::with_capacity(tag_ids.len());
    for &id in tag_ids {
        if !unique.contains(&id) {
            unique.push(id);
        }
    }
    if unique.is_empty() {
        errors.push(field_error("tagIds", "タグを 1 つ以上指定してください"));
    } else if unique.len() > HISTORY_MAX_TAGS {
        errors.push(field_error(
            "tagIds",
            format!(
                "タグは {HISTORY_MAX_TAGS} 個までです（{} 個）",
                unique.len()
            ),
        ));
    }

    let width = i128::from(to_ms) - i128::from(from_ms);
    if width < i128::from(DAILY_REPORT_MIN_WINDOW_MS) {
        errors.push(field_error(
            "toMs",
            format!(
                "期間は {} 時間以上にしてください",
                DAILY_REPORT_MIN_WINDOW_MS / DAILY_REPORT_BIN_MS
            ),
        ));
    } else if width > i128::from(DAILY_REPORT_MAX_WINDOW_MS) {
        errors.push(field_error(
            "toMs",
            format!(
                "期間は {} 時間以内にしてください",
                DAILY_REPORT_MAX_WINDOW_MS / DAILY_REPORT_BIN_MS
            ),
        ));
    }

    if errors.is_empty() {
        Ok(DailyReportRequest {
            tag_ids: unique,
            from_ms,
            to_ms,
        })
    } else {
        Err(BantoError::Validation {
            field_errors: errors,
        })
    }
}

/// 読み出しが失敗した理由（ログ用。公開する形には載せない）。
#[derive(Debug)]
enum DailyReportReadError {
    Registry(sqlx::Error),
    Store(TsQueryError),
}

impl std::fmt::Display for DailyReportReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Registry(err) => write!(f, "レジストリ: {err}"),
            Self::Store(err) => write!(f, "データファイル: {err}"),
        }
    }
}

/// 3 つとも有限で件数が 1 以上のときだけ統計量にする（非有限は欠測と同じ `None`）。
fn stat_of(min: f64, max: f64, avg: f64, count: i64) -> Option<DailyReportStat> {
    (count > 0 && min.is_finite() && max.is_finite() && avg.is_finite()).then_some(
        DailyReportStat {
            min,
            max,
            avg,
            count,
        },
    )
}

/// 日の合計を、時間ごとの区間（同じ 1 回の読み出し）から組む。サンプルのある区間
/// （`Some`）だけを見て、どれかの最小・最大・合計・平均が非有限なら `None`（時間の行と
/// 同じく、非有限のサンプルは日も欠測にする）。そうでなければ最小の最小・最大の最大・
/// 件数の和・合計値の和 ÷ 件数の和。件数の和が 0（サンプルなし）も `None`。
fn merge_day<'a>(bins: impl IntoIterator<Item = Option<&'a TagStat>>) -> Option<DailyReportStat> {
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    let mut sum = 0.0_f64;
    let mut count = 0_i64;
    for s in bins.into_iter().flatten() {
        if !(s.min.is_finite() && s.max.is_finite() && s.sum.is_finite() && s.avg.is_finite()) {
            return None;
        }
        min = min.min(s.min);
        max = max.max(s.max);
        sum += s.sum;
        count = count.saturating_add(s.count);
    }
    if count <= 0 {
        return None;
    }
    stat_of(min, max, sum / count as f64, count)
}

/// 日報の本体（上限・打ち切りの外側は [`CollectorService::daily_report`]）。
async fn read_daily_report(
    pool: &SqlitePool,
    query: &TsQuery,
    request: &DailyReportRequest,
) -> Result<CollectDailyReport, DailyReportReadError> {
    let placements = tag_placements(pool, &request.tag_ids)
        .await
        .map_err(DailyReportReadError::Registry)?;

    // 列: 要求順で、レジストリに居るタグだけ。
    let mut tags: Vec<DailyReportTag> = Vec::new();
    let mut unknown_tag_ids: Vec<i64> = Vec::new();
    for id in &request.tag_ids {
        match placements.get(id) {
            Some(TagPlacement { simulation, .. }) => tags.push(DailyReportTag {
                tag_id: *id,
                simulation: *simulation,
            }),
            None => unknown_tag_ids.push(*id),
        }
    }

    // 今の所属グループで束ねる（最初に現れた順）。(グループ, [(列の番号, タグ ID)])
    let mut groups: Vec<(i64, Vec<(usize, i64)>)> = Vec::new();
    for (column, tag) in tags.iter().enumerate() {
        let group_id = placements[&tag.tag_id].group_id;
        match groups.iter_mut().find(|(g, _)| *g == group_id) {
            Some((_, ids)) => ids.push((column, tag.tag_id)),
            None => groups.push((group_id, vec![(column, tag.tag_id)])),
        }
    }

    // 日は `[fromMs, toMs)`、tsquery は両端を含むので `toMs - 1` を渡す
    // （モジュール doc「「1 日」の範囲」）。検証が幅 1 時間以上を保証する。
    let last_ms = request.to_ms - 1;
    let bin_count =
        (request.to_ms - request.from_ms + DAILY_REPORT_BIN_MS - 1) / DAILY_REPORT_BIN_MS;
    let width = tags.len();
    let mut hours: Vec<DailyReportHour> = (0..bin_count)
        .map(|i| DailyReportHour {
            t_ms: request.from_ms + i * DAILY_REPORT_BIN_MS,
            values: vec![None; width],
        })
        .collect();
    let mut day: Vec<Option<DailyReportStat>> = vec![None; width];

    for (group_id, members) in &groups {
        let group_key = format!("grp:{group_id}");
        let keys: Vec<String> = members.iter().map(|(_, id)| format!("tag:{id}")).collect();

        let stats = query
            .read_binned_stats(
                &group_key,
                &keys,
                request.from_ms,
                last_ms,
                DAILY_REPORT_BIN_MS,
            )
            .await
            .map_err(DailyReportReadError::Store)?;
        for (index, bin) in stats.bins.iter().enumerate() {
            let Some(hour) = hours.get_mut(index) else {
                continue;
            };
            for (offset, (column, _)) in members.iter().enumerate() {
                hour.values[*column] = bin
                    .tags
                    .get(offset)
                    .copied()
                    .flatten()
                    .and_then(|s| stat_of(s.min, s.max, s.avg, s.count));
            }
        }

        // 日の合計は同じ `stats` から（2 回目の読み出しはしない）。
        for (offset, (column, _)) in members.iter().enumerate() {
            day[*column] = merge_day(
                stats
                    .bins
                    .iter()
                    .take(hours.len())
                    .map(|bin| bin.tags.get(offset).and_then(Option::as_ref)),
            );
        }
    }

    Ok(CollectDailyReport {
        from_ms: request.from_ms,
        to_ms: request.to_ms,
        interval_ms: DAILY_REPORT_BIN_MS,
        tags,
        unknown_tag_ids,
        hours,
        day,
    })
}

impl CollectorService {
    /// **日報の読み出し**（`GET /api/collect/daily-report` / `collect_daily_report`）。
    /// 形・上限・割り切りはこのモジュールの doc。
    ///
    /// * 読めた → `Ok(Readout::Ready)`、
    /// * レジストリかデータファイルを読めなかった・時間切れ → `Ok(Readout::Unavailable)`
    ///   （理由はログだけ）。
    ///
    /// **常に**長い読み出しの絞り（[`run_long_read`]）を通す。
    pub async fn daily_report(
        &self,
        request: &DailyReportRequest,
    ) -> Result<Readout<CollectDailyReport>, BantoError> {
        let ctx = &self.inner.ctx;
        let query = TsQuery::with_place(ctx.data_place());
        let pool = ctx.pool.clone();
        let request = request.clone();
        let outcome = run_long_read(
            &self.inner.history_long_reads,
            HISTORY_LONG_READ_TIMEOUT,
            async move { read_daily_report(&pool, &query, &request).await },
        )
        .await;
        match outcome {
            Ok(Ok(report)) => Ok(Readout::Ready { data: report }),
            Ok(Err(err)) => {
                eprintln!("banto: 日報の読み出しに失敗しました: {err}");
                Ok(Readout::Unavailable)
            }
            Err(LongReadStop::TimedOut) => {
                eprintln!(
                    "banto: 日報の読み出しが{}秒以内に終わりませんでした",
                    HISTORY_LONG_READ_TIMEOUT.as_secs()
                );
                Ok(Readout::Unavailable)
            }
            Err(LongReadStop::Aborted) => {
                eprintln!("banto: 日報の読み出しのタスクが途中で終わりました");
                Ok(Readout::Unavailable)
            }
        }
    }
}

#[cfg(test)]
mod merge_tests {
    use super::*;

    fn ts(min: f64, max: f64, sum: f64, count: i64) -> TagStat {
        TagStat {
            min,
            max,
            avg: sum / count as f64,
            sum,
            count,
        }
    }

    #[test]
    fn merges_weighted_by_count() {
        let a = ts(1.0, 5.0, 12.0, 4); // avg 3
        let b = ts(-2.0, 3.0, 4.0, 1);
        let got = merge_day([Some(&a), None, Some(&b)]).unwrap();
        assert_eq!((got.min, got.max, got.count), (-2.0, 5.0, 5));
        assert!((got.avg - 3.2).abs() < 1e-12, "avg {}", got.avg);
    }

    #[test]
    fn empty_or_all_gaps_is_none() {
        assert_eq!(merge_day([]), None);
        assert_eq!(merge_day([None, None]), None);
    }

    #[test]
    fn any_non_finite_bin_poisons_the_day() {
        let ok = ts(1.0, 2.0, 3.0, 2);
        for bad in [
            ts(1.0, f64::INFINITY, f64::INFINITY, 1),
            ts(f64::NEG_INFINITY, 1.0, f64::NEG_INFINITY, 1),
            TagStat {
                min: 1.0,
                max: 1.0,
                avg: f64::NAN,
                sum: f64::NAN,
                count: 1,
            },
        ] {
            assert_eq!(merge_day([Some(&ok), Some(&bad)]), None);
            assert_eq!(merge_day([Some(&bad), Some(&ok)]), None);
        }
    }
}

#[cfg(test)]
mod tests;
