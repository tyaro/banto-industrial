//! 直近の履歴の読み出し（R1-D の D-3a）: リアルタイムトレンドの**初期窓**を
//! データファイル（`banto-tstore`）から間引いて返す口。R2 のヒストリカル
//! トレンドからも使い回す前提で、画面には依存しない（画面は D-3b）。
//!
//! # 形と経路
//!
//! * REST `GET /api/collect/history?tagIds=1,2&fromMs=&toMs=&bins=` と Tauri
//!   `collect_history` の双子。どちらも [`validate_history_request`] →
//!   [`CollectorService::history`] の**同じ 2 つの関数**を通るので、経路に
//!   よって上限も形も割れない（[`super::COLLECT_AUDIT_RESOURCE`] と同じ作法）。
//! * 床は**読み取りと同じ `viewer` 以上**（[`super::COLLECT_READ_ROLE`]）。
//!   読み取りなので監査しない。
//! * 結末は [`Readout`]。**`notRunning` は返さない** - 履歴は過去の記録で、
//!   収集が止まっていても読める（イベント一覧と同じ考え方）。データファイルか
//!   レジストリを読めなかったとき、[`HISTORY_READ_TIMEOUT`] で打ち切ったときは
//!   [`Readout::Unavailable`]。**理由は返さない** - `banto-tsquery` のエラー
//!   文言はデータファイルの絶対パスを含みうる（`IncompatibleFile` など）。
//!   理由はサーバー側のログにだけ出す（C-3a のイベント一覧と同じ判断）。
//!
//! # 収集の操作キューを通らない
//!
//! docs/implementation-checklist.md §5「読み取りと操作で同じキューを共有
//! しない」: この口はライフサイクルタスクにも、そのどちらのキューにも触らない。
//! `banto-tsquery` は呼ぶたびに自前の読み取り専用接続を開いてデータファイルを
//! 直接読む（非同期の `sqlx` なので `spawn_blocking` も要らない）。WAL の
//! 読み手なので、走っている収集の書き手（`TsWriter`）も待たせない。収集が
//! 起動処理の最中でも、この口は待たされない。
//!
//! # 上限（viewer にも開いているため）
//!
//! * タグは **1〜[`HISTORY_MAX_TAGS`] 本**（重複は 1 本に数える）、
//! * 期間は `fromMs < toMs` かつ **幅 [`HISTORY_MAX_WINDOW_MS`]（1 時間）以内**
//!   （両端を含む。`banto-tsquery` の約束どおり）、
//! * `bins` は **1〜[`HISTORY_MAX_BINS`]**（= `banto_tsquery::MAX_TARGET_BINS`）。
//!
//! 外れたら `BantoError::Validation`（REST は 422）で、**ファイルには触らない**。
//! 1 時間を超える期間は R2（ヒストリカル）で別に決める。
//!
//! # 返すもの（タグごとの系列）
//!
//! [`CollectHistory::series`] は**要求した順**に、レジストリに居るタグ
//! 1 本につき 1 系列。各点は `banto-tsquery` の間引き結果そのままの
//! **最小・最大の包絡**（[`HistoryPoint`]。平均は返さない - 間引きで山を
//! 隠さない、が `banto-tsquery` の設計原則）。
//!
//! * **欠測（`Gap`）は `min`/`max` とも `null`**。0 に潰さない
//!   （R1-D「null を 0 と区別する」）。
//! * **非有限の値（NaN・±∞）も `null`** - JSON では `null` になって
//!   「値が無い」と区別が付かなくなるので、公開する形へ変換するここで畳む
//!   （docs/implementation-checklist.md §5、#408）。包絡の片端だけが非有限でも
//!   **点ごと `null`** にする（片端だけ残すと包絡として嘘になる）。
//! * 点の時刻 `tMs` は `banto-tsquery` の `Bin::ptime_ms` - 間引いた区間の
//!   **始まり**、ただし拡大しきって 1 区間に 1 サンプル程度のときは**サンプル
//!   そのものの時刻**（`banto-tsquery` の decimate.rs の素通し）。そのため
//!   **系列ごとに点の時刻が揃うとは限らない**（収集周期の違うグループの
//!   タグを混ぜると `binMs` も別）。系列ごとに `binMs` を返す。
//! * レジストリに居ないタグ ID（消された・誤り）は系列を作らず
//!   [`CollectHistory::unknown_tag_ids`] に並べる。**1 本の誤りで全体を
//!   止めない**（docs/implementation-checklist.md §5）。
//!
//! # 収集グループをまたぐ要求
//!
//! データファイルの中のタグは収集グループ（`grp:<id>`）ごとの表にある。
//! 要求したタグを**今の**所属グループで束ね、グループごとに
//! `TsQuery::read_decimated` を 1 回ずつ呼んで、要求順に並べ直す。
//!
//! **タグを別の収集グループへ移した場合（割り切り）**: 移す前の値は**前の
//! グループ**の表にしか無く、この口は今のグループしか読まないので、移す前の
//! 区間は欠測（`null`）として返る。過去の所属を知るにはデータディレクトリの
//! 全ファイルを開く（`TsQuery::catalog`）必要があり、ポーリングされうる口に
//! 載せる重さではないため。1 時間の初期窓で困るのは移した直後の 1 時間だけ。
//! R2 のヒストリカルで要るなら、そこで catalog を引く形を設計する。
//!
//! # 外したタグ・シミュレーション
//!
//! * 開始時に**外したタグ**（#414 段階2）には tstore の列が無いので、
//!   その間は `banto-tsquery` が欠測として返し、ここでも `null` になる
//!   （エラーではない）。
//! * **シミュレーション接続のタグの値は記録されない**（`banto-collect` の
//!   `task.rs` の約束。`tests/simulation_not_recorded.rs`）ので、その間は
//!   欠測（`null`）になる。特別扱いはしない - シミュレーションにする前に
//!   実機で記録した値は本物なので、そのまま返す。画面が「シミュレーション中
//!   （値は記録されません）」と添えられるよう、系列に**レジストリの今の値**の
//!   `simulation` を載せる（走っている収集の値ではない。D-3b で使う）。
//!
//! # 直近の区間は少し遅れる
//!
//! 収集の書き手は既定で約 1 秒ごとに flush する（`banto_tstore::WriterOptions`）。
//! flush 前の値はデータファイルにまだ無いので、`toMs` を「今」にすると末尾の
//! 1 秒ほどは欠測に見える。リアルタイムトレンドは初期窓をこの口で取り、
//! 以後は現在値のポーリングで足していく（D-3b）。

use std::collections::HashMap;
use std::time::Duration;

use banto_core::{BantoError, FieldError};
use banto_tsquery::{BinValue, DecimatedRange, TsQuery};
use serde::Serialize;
use sqlx::SqlitePool;

use super::{CollectorService, Readout};

/// 1 回の要求で読めるタグの本数の上限（重複を除いた数）。トレンドのペンの
/// 本数の想定（表示グループ 1 つ分）に合わせた。
pub const HISTORY_MAX_TAGS: usize = 8;

/// 1 回の要求で読める期間の幅の上限（ミリ秒、1 時間）。リアルタイムトレンドの
/// 初期窓（既定 10 分・選択可）を賄う幅。
pub const HISTORY_MAX_WINDOW_MS: i64 = 3_600_000;

/// `bins` の上限。`banto-tsquery` の上限をそのまま使う（ここで別の値を
/// 決めると、どちらが効いているのか分からなくなる）。
pub const HISTORY_MAX_BINS: usize = banto_tsquery::MAX_TARGET_BINS;

/// 1 回の読み出しの待ち時間の上限。過ぎたら [`Readout::Unavailable`]。
///
/// 打ち切ったときに残るものは無い: 読み出しは読み取り専用の接続を開いて
/// `SELECT` するだけで、future を捨てれば接続も閉じる（docs/
/// implementation-checklist.md §5「時間切れで見捨てた非同期処理の副作用は
/// 残る」- この処理には副作用が無い）。
pub const HISTORY_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// 検証を通った履歴の要求。**[`validate_history_request`] でしか作れない**
/// （フィールドは非公開）ので、[`CollectorService::history`] に上限を外れた
/// 要求が届くことはない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryRequest {
    /// 重複を除き、要求の順を保ったタグ ID。
    tag_ids: Vec<i64>,
    from_ms: i64,
    to_ms: i64,
    bins: usize,
}

impl HistoryRequest {
    pub fn tag_ids(&self) -> &[i64] {
        &self.tag_ids
    }
}

/// 履歴の 1 点: 間引いた 1 区間の最小・最大。**欠測と非有限は両方 `null`**
/// （このモジュールの doc「返すもの」）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryPoint {
    /// 区間の始まり、または素通しのときはサンプルの時刻（UTC epoch ミリ秒）。
    pub t_ms: i64,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

/// タグ 1 本の系列。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistorySeries {
    pub tag_id: i64,
    /// このタグの接続が**レジストリ上**シミュレーションか（記録されない。
    /// このモジュールの doc「外したタグ・シミュレーション」）。
    pub simulation: bool,
    /// 実際に使われた区間の幅（`banto-tsquery` がグループの収集周期まで
    /// 広げることがある）。系列ごとに違いうる。
    pub bin_ms: i64,
    /// 時刻の昇順。
    pub points: Vec<HistoryPoint>,
}

/// 履歴の読み出し結果（[`Readout::Ready`] の中身）。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectHistory {
    /// 要求の期間をそのまま返す（両端を含む）。
    pub from_ms: i64,
    pub to_ms: i64,
    /// 要求順（重複は除く）。レジストリに居ないタグは含まない。
    pub series: Vec<HistorySeries>,
    /// レジストリに居なかったタグ ID（要求順）。
    pub unknown_tag_ids: Vec<i64>,
}

fn field_error(field: &str, message: impl Into<String>) -> FieldError {
    FieldError {
        field: field.to_string(),
        message: message.into(),
    }
}

/// 履歴の要求を検証する。REST と Tauri の**両方がここを通る**。誤りは
/// すべて `field_errors` に並べて返す（フィールド名はワイヤの綴り）。
pub fn validate_history_request(
    tag_ids: &[i64],
    from_ms: i64,
    to_ms: i64,
    bins: u64,
) -> Result<HistoryRequest, BantoError> {
    let mut errors = Vec::new();

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

    // 幅は i128 で測る（両端が極端な値でも溢れない）。
    let width = i128::from(to_ms) - i128::from(from_ms);
    if width <= 0 {
        errors.push(field_error(
            "toMs",
            "終わりの時刻は始まりの時刻より後にしてください",
        ));
    } else if width > i128::from(HISTORY_MAX_WINDOW_MS) {
        errors.push(field_error(
            "toMs",
            format!(
                "期間は {} 秒以内にしてください",
                HISTORY_MAX_WINDOW_MS / 1000
            ),
        ));
    }

    let bins = usize::try_from(bins).unwrap_or(usize::MAX);
    if bins == 0 || bins > HISTORY_MAX_BINS {
        errors.push(field_error(
            "bins",
            format!("bins は 1〜{HISTORY_MAX_BINS} にしてください"),
        ));
    }

    if errors.is_empty() {
        Ok(HistoryRequest {
            tag_ids: unique,
            from_ms,
            to_ms,
            bins,
        })
    } else {
        Err(BantoError::Validation {
            field_errors: errors,
        })
    }
}

/// REST の `tagIds=1,2,3` を読む。空の要素・数値でない要素は
/// `tagIds` の誤りにする（本数の上限は [`validate_history_request`]）。
pub fn parse_tag_ids(raw: &str) -> Result<Vec<i64>, BantoError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    trimmed
        .split(',')
        .map(|part| part.trim().parse::<i64>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| BantoError::Validation {
            field_errors: vec![field_error(
                "tagIds",
                "タグ ID はカンマ区切りの整数で指定してください",
            )],
        })
}

/// レジストリから引いた、タグ 1 本の今の置き場。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TagPlacement {
    group_id: i64,
    simulation: bool,
}

/// 要求したタグの今の所属グループと、接続がシミュレーションかを引く。
/// 居ないタグはマップに入らない。
async fn tag_placements(
    pool: &SqlitePool,
    tag_ids: &[i64],
) -> Result<HashMap<i64, TagPlacement>, sqlx::Error> {
    if tag_ids.is_empty() {
        return Ok(HashMap::new());
    }
    // ID の並びは JSON 配列 1 つで渡し、`json_each` で展開する（SQL 文は
    // 固定のまま。本数で文を組み立てない）。
    // `LEFT JOIN`: グループ・接続の行は外部キーで必ずあるはずだが、無くても
    // タグを「居ない」側へ落とさない（シミュレーションでない扱いで読む）。
    let ids_json = serde_json::to_string(tag_ids).expect("i64 の配列は必ず JSON にできる");
    let query = sqlx::query_as::<_, (i64, i64, bool)>(
        "SELECT t.id, t.collection_group_id, COALESCE(c.simulation, 0)          FROM tags t          LEFT JOIN collection_groups g ON g.id = t.collection_group_id          LEFT JOIN plc_connections c ON c.id = g.plc_connection_id          WHERE t.id IN (SELECT value FROM json_each(?))",
    )
    .bind(ids_json);
    Ok(query
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(|(id, group_id, simulation)| {
            (
                id,
                TagPlacement {
                    group_id,
                    simulation,
                },
            )
        })
        .collect())
}

/// 間引いた 1 区間の 1 タグを公開する形へ。**欠測も非有限も `null`**。
fn point_from_bin(t_ms: i64, value: BinValue) -> HistoryPoint {
    match value {
        BinValue::Range { min, max } if min.is_finite() && max.is_finite() => HistoryPoint {
            t_ms,
            min: Some(min),
            max: Some(max),
        },
        // `Gap`、または包絡のどちらかの端が NaN・±∞。
        _ => HistoryPoint {
            t_ms,
            min: None,
            max: None,
        },
    }
}

/// 1 グループ分の間引き結果から、`column` 番目のタグの点列を取り出す。
fn points_for_column(range: &DecimatedRange, column: usize) -> Vec<HistoryPoint> {
    range
        .bins
        .iter()
        .map(|bin| {
            let value = bin.tags.get(column).copied().unwrap_or(BinValue::Gap);
            point_from_bin(bin.ptime_ms, value)
        })
        .collect()
}

/// 読み出しが失敗した理由（ログ用。公開する形には載せない）。
#[derive(Debug)]
enum HistoryReadError {
    Registry(sqlx::Error),
    Store(banto_tsquery::TsQueryError),
}

impl std::fmt::Display for HistoryReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Registry(err) => write!(f, "レジストリ: {err}"),
            Self::Store(err) => write!(f, "データファイル: {err}"),
        }
    }
}

/// 履歴の本体（上限・打ち切りの外側は [`CollectorService::history`]）。
async fn read_history(
    pool: &SqlitePool,
    query: &TsQuery,
    request: &HistoryRequest,
) -> Result<CollectHistory, HistoryReadError> {
    let placements = tag_placements(pool, &request.tag_ids)
        .await
        .map_err(HistoryReadError::Registry)?;

    // 今の所属グループで束ねる（グループの順は最初に現れた順）。
    let mut groups: Vec<(i64, Vec<i64>)> = Vec::new();
    for id in &request.tag_ids {
        let Some(placement) = placements.get(id) else {
            continue;
        };
        match groups.iter_mut().find(|(g, _)| *g == placement.group_id) {
            Some((_, ids)) => ids.push(*id),
            None => groups.push((placement.group_id, vec![*id])),
        }
    }

    let mut by_tag: HashMap<i64, (i64, Vec<HistoryPoint>)> = HashMap::new();
    for (group_id, ids) in &groups {
        let keys: Vec<String> = ids.iter().map(|id| format!("tag:{id}")).collect();
        let range = query
            .read_decimated(
                &format!("grp:{group_id}"),
                &keys,
                request.from_ms,
                request.to_ms,
                request.bins,
            )
            .await
            .map_err(HistoryReadError::Store)?;
        for (column, id) in ids.iter().enumerate() {
            by_tag.insert(*id, (range.bin_ms, points_for_column(&range, column)));
        }
    }

    let mut series = Vec::new();
    let mut unknown_tag_ids = Vec::new();
    for id in &request.tag_ids {
        match (placements.get(id), by_tag.remove(id)) {
            (Some(placement), Some((bin_ms, points))) => series.push(HistorySeries {
                tag_id: *id,
                simulation: placement.simulation,
                bin_ms,
                points,
            }),
            _ => unknown_tag_ids.push(*id),
        }
    }

    Ok(CollectHistory {
        from_ms: request.from_ms,
        to_ms: request.to_ms,
        series,
        unknown_tag_ids,
    })
}

impl CollectorService {
    /// **直近の履歴**（`GET /api/collect/history` / `collect_history`）。
    /// 形・上限・割り切りはこのモジュールの doc。
    ///
    /// * 読めた → [`Readout::Ready`]（データファイルがまだ無い・その区間に
    ///   値が無いときも `Ready` で、点が全部 `null`）、
    /// * レジストリかデータファイルを読めなかった・[`HISTORY_READ_TIMEOUT`]
    ///   で打ち切った → [`Readout::Unavailable`]（理由はログだけ）。
    ///
    /// **[`Readout::NotRunning`] は返さない**。収集のライフサイクルタスクにも
    /// キューにも触らない（このモジュールの doc「収集の操作キューを通らない」）。
    pub async fn history(&self, request: &HistoryRequest) -> Readout<CollectHistory> {
        let ctx = &self.inner.ctx;
        let query = TsQuery::new(ctx.data_dir.clone());
        match tokio::time::timeout(
            HISTORY_READ_TIMEOUT,
            read_history(&ctx.pool, &query, request),
        )
        .await
        {
            Ok(Ok(history)) => Readout::Ready { data: history },
            Ok(Err(err)) => {
                eprintln!("banto: 履歴の読み出しに失敗しました: {err}");
                Readout::Unavailable
            }
            Err(_elapsed) => {
                eprintln!(
                    "banto: 履歴の読み出しが{}秒以内に終わりませんでした",
                    HISTORY_READ_TIMEOUT.as_secs()
                );
                Readout::Unavailable
            }
        }
    }
}

#[cfg(test)]
mod tests;
