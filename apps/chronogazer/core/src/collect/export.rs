//! 期間指定 CSV 出力のための読み出し（#1207 R3 の PR2）: ヒストリカル画面で選んだ
//! 表示グループ・期間のタグの値を、**横持ちの 1 枚の表**（1 行 = 1 つの時刻、
//! 列 = タグ）としてデータファイル（`banto-tsquery`）から返す口。CSV の組み立てと
//! 保存は画面の側（PR3）で、ここは値を返すだけ。履歴（[`super::history`]）の双子で、
//! 検証・経路・絞り・「理由を返さない」作法を揃えている。
//!
//! # 形と経路
//!
//! * REST `GET /api/collect/export?tagIds=1,2&fromMs=&toMs=&mode=raw|interval&intervalMs=`
//!   と Tauri `collect_export` の双子。どちらも [`validate_export_request`] →
//!   [`CollectorService::export`] の**同じ 2 つの関数**を通る。
//! * 床は読み取りと同じ `viewer` 以上。読み取りなので監査しない。
//! * 結末は [`Readout`]。`notRunning` は返さない（収集が止まっていても読める）。
//!   データファイル・レジストリを読めなかった、またはタイムアウトは
//!   [`Readout::Unavailable`]（理由はログだけ。パスを含みうる）。
//! * 収集の操作キューは通らない（履歴と同じ。`banto-tsquery` は自前の読み取り専用
//!   接続で読む）。
//!
//! # 2 つの出力（オーナー決定 2026-11-19）
//!
//! * `raw`（生の値）: 記録されているサンプルをそのまま。**収集グループが違うと
//!   時刻が揃わない**ので、グループごとに `read_range` した結果を**時刻でマージ**し、
//!   時刻ごとに 1 行（昇順）にする。その時刻にサンプルが無いタグのセルは `null`。
//! * `interval`（間隔で揃える）: `intervalMs`（1 秒以上）の固定幅の区間
//!   （`fromMs + i * intervalMs`）ごとに最小・最大・平均を返す。どのグループも
//!   同じ起点・同じ幅なので行は自然に揃う。**サンプルの無い区間も行は出す**
//!   （全タグ `null`。CSV で時刻の欠けを作らない）。
//!
//! # 上限（viewer にも開いているため）
//!
//! * タグは 1〜[`HISTORY_MAX_TAGS`] 本（重複は 1 本）、期間は `fromMs <= toMs` で
//!   幅 [`HISTORY_MAX_WINDOW_MS`]（31 日）以内（履歴と同じ。`fromMs == toMs` は
//!   両端を含むので 1 時刻だけ読める）、
//! * 行数は [`EXPORT_MAX_ROWS`]（10 万。`banto_tsquery::DEFAULT_MAX_RAW_ROWS`・
//!   `MAX_STAT_BINS` と同じ）まで。`interval` は **区間の数
//!   `(toMs - fromMs) / intervalMs + 1` が上限以内**かを検証で見る
//!   （ファイルに触る前に 422）。`raw` は読んでみないと分からないので、
//!   **グループごとに読んだ行の合計**が上限を超えたら打ち切る。
//!
//! **行数超過の扱い（割り切り）**: 超えたときは [`Readout::Unavailable`] でも
//! 部分結果でもなく、`BantoError::Validation`（REST は 422）で `toMs` に
//! 「行数が上限（10 万行）を超えます。期間を狭めるか、間隔で揃えて出力してください」
//! を付ける。利用者が自分で直せる誤りなので、画面がそのまま見せられる。
//! 合計は**グループごとの行数の和**で、時刻をマージした後の行数（和以下）より
//! 保守的に数える（マージ後の数を知るには全部読む必要がある）。
//!
//! # 長い読み出しの絞り
//!
//! 読む量は期間と行数に比例して重いので、幅に関わらず**常に**履歴の
//! 「長い読み出し」と同じ絞り（同時 [`super::history::HISTORY_LONG_READ_PERMITS`] 本、待ちを含めて
//! [`HISTORY_LONG_READ_TIMEOUT`]）を通す（[`run_long_read`]）。履歴の長い読み出しと
//! 札を共有するので、出力が走っている間は他の長い読み出しも順番待ちになる。
//!
//! # 返すもの
//!
//! [`CollectExport::tags`] は**要求順**でレジストリに居るタグだけ（列の定義）。
//! 居ないタグは [`CollectExport::unknown_tag_ids`]（1 本の誤りで全体を止めない）。
//! 各 [`ExportRow::values`] は `tags` と同じ長さ・順のセルで、**セルは数値 1 つ**
//! （値が無いセルは `null`）。`raw` はサンプルの値、`interval` は要求の
//! `stat`（[`ExportStat`]: `avg`/`min`/`max`）で選んだ区間の統計量。3 つ全部を
//! 載せると JSON が 3 倍になる（最大 10 万行 × 8 タグ）ので、1 回の出力では 1 つだけ
//! 返す。選んだ統計量は [`CollectExport::stat`] に返す（`raw` は `null`）。
//! **非有限（NaN・±∞）は欠測と同じく `null`**（履歴と同じ。`interval` は選んだ
//! 統計量が非有限ならその区間は `null`）。
//!
//! シミュレーション接続のタグは値が記録されないので全部 `null`。画面が注記できるよう
//! `tags[].simulation` を載せる。タグを別グループへ移す前の値は読めない
//! （履歴と同じ割り切り。モジュール doc [`super::history`]）。

use std::collections::BTreeMap;

use banto_core::{BantoError, FieldError};
use banto_tsquery::{TsQuery, TsQueryError};
use serde::Serialize;
use sqlx::SqlitePool;

use super::history::{
    field_error, run_long_read, tag_placements, LongReadStop, TagPlacement,
    HISTORY_LONG_READ_TIMEOUT, HISTORY_MAX_TAGS, HISTORY_MAX_WINDOW_MS,
};
use super::{CollectorService, Readout};

/// 1 回の出力の行数の上限（10 万行）。`banto_tsquery` の
/// `DEFAULT_MAX_RAW_ROWS` / `MAX_STAT_BINS` と同じ値。
pub const EXPORT_MAX_ROWS: usize = 100_000;

/// `interval` の区間の幅の下限（ミリ秒）。
pub const EXPORT_MIN_INTERVAL_MS: i64 = 1000;

/// 行数超過の誤りの文言（`toMs` に付ける）。
const TOO_MANY_ROWS_MESSAGE: &str =
    "行数が上限（10 万行）を超えます。期間を狭めるか、間隔で揃えて出力してください";

/// 出力の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
#[derive(utoipa::ToSchema)]
pub enum ExportMode {
    /// 記録されたサンプルをそのまま（時刻でマージ）。
    Raw,
    /// 固定幅の区間ごとの最小・最大・平均。
    Interval,
}

/// `interval` で出す統計量（1 回の出力では 1 つ）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
#[derive(utoipa::ToSchema)]
pub enum ExportStat {
    Avg,
    Min,
    Max,
}

/// 検証を通った出力の要求。**[`validate_export_request`] でしか作れない**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportRequest {
    tag_ids: Vec<i64>,
    from_ms: i64,
    to_ms: i64,
    mode: ExportMode,
    /// `Interval` のときだけ `Some`。
    interval_ms: Option<i64>,
    /// `Interval` のときだけ `Some`。
    stat: Option<ExportStat>,
}

impl ExportRequest {
    pub fn tag_ids(&self) -> &[i64] {
        &self.tag_ids
    }
}

/// 横持ちの 1 行（1 つの時刻）。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
#[derive(utoipa::ToSchema)]
pub struct ExportRow {
    /// `raw` はサンプルの時刻、`interval` は区間の始まり（UTC epoch ミリ秒）。
    pub t_ms: i64,
    /// [`CollectExport::tags`] と同じ長さ・順。値が無いセルは `null`。
    /// `raw` はサンプルの値、`interval` は選んだ統計量。
    pub values: Vec<Option<f64>>,
}

/// 列（タグ）の定義。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
#[derive(utoipa::ToSchema)]
pub struct ExportTag {
    pub tag_id: i64,
    /// レジストリ上シミュレーション接続か（値は記録されない）。
    pub simulation: bool,
}

/// 出力の読み出し結果（[`Readout::Ready`] の中身）。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
#[derive(utoipa::ToSchema)]
pub struct CollectExport {
    pub from_ms: i64,
    pub to_ms: i64,
    pub mode: ExportMode,
    /// `interval` のときだけ値、`raw` は `null`。
    pub interval_ms: Option<i64>,
    /// `interval` のときだけ値（選んだ統計量）、`raw` は `null`。
    pub stat: Option<ExportStat>,
    /// 列。要求順（重複は除く）。レジストリに居ないタグは含まない。
    pub tags: Vec<ExportTag>,
    pub unknown_tag_ids: Vec<i64>,
    /// 時刻の昇順。
    pub rows: Vec<ExportRow>,
}

/// 出力の要求を検証する。REST と Tauri の**両方がここを通る**。誤りは
/// すべて `field_errors` に並べて返す（フィールド名はワイヤの綴り）。
pub fn validate_export_request(
    tag_ids: &[i64],
    from_ms: i64,
    to_ms: i64,
    mode: &str,
    interval_ms: Option<i64>,
    stat: Option<&str>,
) -> Result<ExportRequest, BantoError> {
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
    let mut window_ok = true;
    if width < 0 {
        window_ok = false;
        errors.push(field_error(
            "toMs",
            "終わりの時刻は始まりの時刻以降にしてください",
        ));
    } else if width > i128::from(HISTORY_MAX_WINDOW_MS) {
        window_ok = false;
        errors.push(field_error(
            "toMs",
            format!(
                "期間は {} 秒以内にしてください",
                HISTORY_MAX_WINDOW_MS / 1000
            ),
        ));
    }

    let parsed_mode = match mode {
        "raw" => Some(ExportMode::Raw),
        "interval" => Some(ExportMode::Interval),
        _ => {
            errors.push(field_error(
                "mode",
                "mode は raw か interval にしてください",
            ));
            None
        }
    };

    let mut kept_interval = None;
    match (parsed_mode, interval_ms) {
        (Some(ExportMode::Raw), Some(_)) => {
            errors.push(field_error(
                "intervalMs",
                "raw では intervalMs を指定できません",
            ));
        }
        (Some(ExportMode::Interval), None) => {
            errors.push(field_error(
                "intervalMs",
                "interval では intervalMs が必須です",
            ));
        }
        (Some(ExportMode::Interval), Some(ms)) => {
            if ms < EXPORT_MIN_INTERVAL_MS {
                errors.push(field_error(
                    "intervalMs",
                    format!("intervalMs は {EXPORT_MIN_INTERVAL_MS} 以上にしてください"),
                ));
            } else if window_ok && width / i128::from(ms) + 1 > EXPORT_MAX_ROWS as i128 {
                errors.push(field_error(
                    "intervalMs",
                    "区間の数が上限（10 万）を超えます。間隔を広げるか、期間を狭めてください",
                ));
            } else {
                kept_interval = Some(ms);
            }
        }
        _ => {}
    }

    let mut kept_stat = None;
    match (parsed_mode, stat) {
        (Some(ExportMode::Raw), Some(_)) => {
            errors.push(field_error("stat", "raw では stat を指定できません"));
        }
        (Some(ExportMode::Interval), None) => {
            errors.push(field_error("stat", "interval では stat が必須です"));
        }
        (Some(ExportMode::Interval), Some(name)) => match name {
            "avg" => kept_stat = Some(ExportStat::Avg),
            "min" => kept_stat = Some(ExportStat::Min),
            "max" => kept_stat = Some(ExportStat::Max),
            _ => errors.push(field_error(
                "stat",
                "stat は avg・min・max のどれかにしてください",
            )),
        },
        _ => {}
    }

    match (errors.is_empty(), parsed_mode) {
        (true, Some(mode)) => Ok(ExportRequest {
            tag_ids: unique,
            from_ms,
            to_ms,
            mode,
            interval_ms: kept_interval,
            stat: kept_stat,
        }),
        _ => Err(BantoError::Validation {
            field_errors: errors,
        }),
    }
}

/// 読み出しが失敗した理由（ログ用。公開する形には載せない）。
#[derive(Debug)]
enum ExportReadError {
    Registry(sqlx::Error),
    Store(TsQueryError),
    /// 行数が上限を超えた（利用者が直せる誤りなので 422 にする）。
    TooManyRows,
}

impl std::fmt::Display for ExportReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Registry(err) => write!(f, "レジストリ: {err}"),
            Self::Store(err) => write!(f, "データファイル: {err}"),
            Self::TooManyRows => write!(f, "行数が上限を超えました"),
        }
    }
}

/// 非有限（NaN・±∞）は欠測と同じく `None`。
fn finite(value: Option<f64>) -> Option<f64> {
    value.filter(|v| v.is_finite())
}

/// `export` の行数超過を利用者向けの検証エラー（`toMs`）にする。
fn too_many_rows_error() -> BantoError {
    BantoError::Validation {
        field_errors: vec![field_error("toMs", TOO_MANY_ROWS_MESSAGE)],
    }
}

/// 読み出しの失敗を `export` の結果に写す。`TooManyRows` だけが利用者の誤り
/// （`Err`）で、あとはログに残して `Unavailable`。
fn map_read_error(err: ExportReadError) -> Result<Readout<CollectExport>, BantoError> {
    match err {
        ExportReadError::TooManyRows => Err(too_many_rows_error()),
        err => {
            eprintln!("banto: 出力の読み出しに失敗しました: {err}");
            Ok(Readout::Unavailable)
        }
    }
}

/// 出力の本体（上限・打ち切りの外側は [`CollectorService::export`]）。
async fn read_export(
    pool: &SqlitePool,
    query: &TsQuery,
    request: &ExportRequest,
    max_rows: usize,
) -> Result<CollectExport, ExportReadError> {
    let placements = tag_placements(pool, &request.tag_ids)
        .await
        .map_err(ExportReadError::Registry)?;

    // 列: 要求順で、レジストリに居るタグだけ。
    let mut tags: Vec<ExportTag> = Vec::new();
    let mut unknown_tag_ids: Vec<i64> = Vec::new();
    // タグ ID -> 列の番号。
    let mut column_of: BTreeMap<i64, usize> = BTreeMap::new();
    for id in &request.tag_ids {
        match placements.get(id) {
            Some(TagPlacement { simulation, .. }) => {
                column_of.insert(*id, tags.len());
                tags.push(ExportTag {
                    tag_id: *id,
                    simulation: *simulation,
                });
            }
            None => unknown_tag_ids.push(*id),
        }
    }

    // 今の所属グループで束ねる（最初に現れた順）。
    let mut groups: Vec<(i64, Vec<i64>)> = Vec::new();
    for tag in &tags {
        let group_id = placements[&tag.tag_id].group_id;
        match groups.iter_mut().find(|(g, _)| *g == group_id) {
            Some((_, ids)) => ids.push(tag.tag_id),
            None => groups.push((group_id, vec![tag.tag_id])),
        }
    }

    let width = tags.len();
    // 時刻 -> 行のセル。BTreeMap なので昇順に取り出せる。
    let mut table: BTreeMap<i64, Vec<Option<f64>>> = BTreeMap::new();
    let mut rows_read: usize = 0;

    for (group_id, ids) in &groups {
        let group_key = format!("grp:{group_id}");
        let keys: Vec<String> = ids.iter().map(|id| format!("tag:{id}")).collect();
        match request.mode {
            ExportMode::Raw => {
                let budget = max_rows.saturating_sub(rows_read);
                let range = match query
                    .read_range(
                        &group_key,
                        &keys,
                        request.from_ms,
                        request.to_ms,
                        Some(budget),
                    )
                    .await
                {
                    Ok(range) => range,
                    Err(TsQueryError::TooManyRows { .. }) => {
                        return Err(ExportReadError::TooManyRows)
                    }
                    Err(err) => return Err(ExportReadError::Store(err)),
                };
                rows_read += range.rows.len();
                if rows_read > max_rows {
                    return Err(ExportReadError::TooManyRows);
                }
                for row in range.rows {
                    let cells = table
                        .entry(row.ptime_ms)
                        .or_insert_with(|| vec![None; width]);
                    for (offset, id) in ids.iter().enumerate() {
                        let value = row.values.get(offset).copied().flatten();
                        cells[column_of[id]] = finite(value);
                    }
                }
            }
            ExportMode::Interval => {
                let interval_ms = request
                    .interval_ms
                    .expect("interval では検証が intervalMs を保証する");
                let stat = request.stat.expect("interval では検証が stat を保証する");
                let stats = query
                    .read_binned_stats(
                        &group_key,
                        &keys,
                        request.from_ms,
                        request.to_ms,
                        interval_ms,
                    )
                    .await
                    .map_err(ExportReadError::Store)?;
                for bin in stats.bins {
                    let cells = table
                        .entry(bin.ptime_ms)
                        .or_insert_with(|| vec![None; width]);
                    for (offset, id) in ids.iter().enumerate() {
                        let cell = bin.tags.get(offset).copied().flatten().and_then(|s| {
                            finite(Some(match stat {
                                ExportStat::Avg => s.avg,
                                ExportStat::Min => s.min,
                                ExportStat::Max => s.max,
                            }))
                        });
                        cells[column_of[id]] = cell;
                    }
                }
            }
        }
    }

    Ok(CollectExport {
        from_ms: request.from_ms,
        to_ms: request.to_ms,
        mode: request.mode,
        interval_ms: request.interval_ms,
        stat: request.stat,
        tags,
        unknown_tag_ids,
        rows: table
            .into_iter()
            .map(|(t_ms, values)| ExportRow { t_ms, values })
            .collect(),
    })
}

impl CollectorService {
    /// **期間指定の出力の読み出し**（`GET /api/collect/export` / `collect_export`）。
    /// 形・上限・割り切りはこのモジュールの doc。
    ///
    /// * 読めた → `Ok(Readout::Ready)`、
    /// * 行数が上限を超えた → `Err(BantoError::Validation)`（`toMs`。`interval` の
    ///   区間の数は検証の段階で `intervalMs` に付く）、
    /// * レジストリかデータファイルを読めなかった・時間切れ → `Ok(Readout::Unavailable)`
    ///   （理由はログだけ）。
    ///
    /// **常に**長い読み出しの絞り（[`run_long_read`]）を通す。
    pub async fn export(
        &self,
        request: &ExportRequest,
    ) -> Result<Readout<CollectExport>, BantoError> {
        let ctx = &self.inner.ctx;
        let query = TsQuery::with_place(ctx.data_place());
        let pool = ctx.pool.clone();
        let request = request.clone();
        let outcome = run_long_read(
            &self.inner.history_long_reads,
            HISTORY_LONG_READ_TIMEOUT,
            async move { read_export(&pool, &query, &request, EXPORT_MAX_ROWS).await },
        )
        .await;
        match outcome {
            Ok(Ok(export)) => Ok(Readout::Ready { data: export }),
            Ok(Err(err)) => map_read_error(err),
            Err(LongReadStop::TimedOut) => {
                eprintln!(
                    "banto: 出力の読み出しが{}秒以内に終わりませんでした",
                    HISTORY_LONG_READ_TIMEOUT.as_secs()
                );
                Ok(Readout::Unavailable)
            }
            Err(LongReadStop::Aborted) => {
                eprintln!("banto: 出力の読み出しのタスクが途中で終わりました");
                Ok(Readout::Unavailable)
            }
        }
    }
}

#[cfg(test)]
mod tests;
