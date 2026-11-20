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
//! * 期間は `fromMs < toMs` かつ **幅 [`HISTORY_MAX_WINDOW_MS`]（31 日）以内**
//!   （両端を含む。`banto-tsquery` の約束どおり。2026-11-19 オーナー決定:
//!   ヒストリカル画面の 1 回の表示は 31 日まで。「とりあえず 31 日、後でどうするかは
//!   別途考える」）、
//! * `bins` は **1〜[`HISTORY_MAX_BINS`]**（2000。グラフの幅に合わせた API の
//!   上限で、`banto_tsquery::MAX_TARGET_BINS`（20 万、内部のメモリの安全弁）
//!   とは別に決める）、
//! * 応答全体の点の数の予算として **タグの本数 × `bins` ≤ [`HISTORY_MAX_POINTS`]**
//!   （32000）。
//!
//! **返す点の数は、どの経路でも系列ごとに `bins` 以下**（[`fit_to_bins`]）。
//! `banto-tsquery` は拡大しきったときの素通し（1 区間 1 サンプル程度）で最大
//! `bins × 2` 行を返しうるので、その場合はここで `bins` 個の区間に寄せ直す。
//! したがって 1 回の応答は最大 [`HISTORY_MAX_POINTS`] 点で、JSON に直す処理
//! （[`HISTORY_READ_TIMEOUT`] の外で行われる）の大きさにも上限が付く。この口は
//! 閲覧公開（ログインなしの閲覧者）にも開いているので、上限は「有効な要求で
//! 作れる最大の応答」を基準に決めている（PR #536 のオーナーレビュー P1）。
//!
//! 外れたら `BantoError::Validation`（REST は 422）で、**ファイルには触らない**。
//!
//! # 長い読み出しの絞り
//!
//! bins・点の予算は期間に依らないので応答の大きさは変わらないが、**読む量**
//! （31 日 × 8 タグ分の日次ファイル）は期間に比例して増える。そこで幅が
//! [`HISTORY_LONG_READ_THRESHOLD_MS`]（1 時間 = 旧上限。ちょうどは短い側）を
//! **超える**要求を「長い読み出し」と呼び、
//!
//! * 同時に走るのは **[`HISTORY_LONG_READ_PERMITS`] 本（2 本）まで**
//!   （`tokio::sync::Semaphore`）。1 時間以下の要求（監視画面のリアルタイム
//!   トレンドの初期窓）は絞りの対象にしない - 長い読み出しで監視画面を待たせない、
//! * 待ち時間の上限は絞りの待ちを含めて **[`HISTORY_LONG_READ_TIMEOUT`]（30 秒）**。
//!   1 時間以下は従来どおり [`HISTORY_READ_TIMEOUT`]（10 秒）。
//!
//! **打ち切っても実際の読み出しは裏で走り続けうる**: `sqlx` の SQLite は別
//! スレッドで文を実行するので、future を捨てても文が最後まで走ることがある。
//! 待つ側（呼び出し側）だけ打ち切って札を返すと、「実際に走っている重い
//! 読み出し」が 2 本を超えて積み上がり、絞りの意味がなくなる。そのため長い
//! 読み出しは `tokio::spawn` したタスクの中で**札（permit）を持ったまま**行い、
//! timeout は `JoinHandle` を待つ側にだけ掛ける。打ち切ったら呼び出し側は
//! [`Readout::Unavailable`] を返すが、タスクは abort せず、**終わるまで札を
//! 持ち続ける**（[`run_long_read`]）。札の待ちの最中に打ち切られた場合は
//! まだ何も spawn していないので、残るものは無い。
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
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use banto_core::{BantoError, FieldError};
use banto_tsquery::{BinValue, DecimatedRange, TsQuery};
use serde::Serialize;
use sqlx::SqlitePool;
use tokio::sync::Semaphore;

use super::{CollectorService, Readout};

/// 1 回の要求で読めるタグの本数の上限（重複を除いた数）。トレンドのペンの
/// 本数の想定（表示グループ 1 つ分、[`crate::display_groups::MAX_PENS_PER_GROUP`]）に
/// 合わせた 16（#1207・#1231 PR 5a、trend-design.md §6）。期間 CSV・日報は
/// この定数を使わず、それぞれ 8 本のまま（`EXPORT_MAX_TAGS`・`DAILY_REPORT_MAX_TAGS`）。
pub const HISTORY_MAX_TAGS: usize = 16;

/// 1 回の要求で読める期間の幅の上限（ミリ秒、31 日）。ヒストリカル画面の 1 回の
/// 表示の上限（2026-11-19 オーナー決定。後で見直す）。リアルタイムトレンドの
/// 初期窓（既定 10 分・選択可）はこの下の短い読み出しで賄う。
/// 応答の大きさは [`HISTORY_MAX_BINS`]・[`HISTORY_MAX_POINTS`] が決めるので、
/// 期間を広げても上限は付いたまま。
pub const HISTORY_MAX_WINDOW_MS: i64 = 31 * 24 * 3_600_000;

/// 「長い読み出し」の境界（ミリ秒、1 時間 = 旧上限）。**幅がこれを超える**
/// 要求だけが絞り（[`HISTORY_LONG_READ_PERMITS`]）と長い待ち時間
/// （[`HISTORY_LONG_READ_TIMEOUT`]）の対象で、ちょうど 1 時間は短い側。
pub const HISTORY_LONG_READ_THRESHOLD_MS: i64 = 3_600_000;

/// 長い読み出しの同時数の上限。理由はモジュール doc「長い読み出しの絞り」。
pub const HISTORY_LONG_READ_PERMITS: usize = 2;

/// 系列 1 本あたりの `bins` の上限（= 返す点の数の上限）。
///
/// **`banto_tsquery::MAX_TARGET_BINS`（20 万）を使わない**: あれはクエリ層の
/// メモリの安全弁で、HTTP の口の上限としては大きすぎる（当時の 8 本 × 20 万で
/// 160 万点の応答を、有効な要求で作れてしまった。PR #536 のオーナーレビュー P1）。
///
/// 2000 の根拠: 点は「1 区間の最小・最大」なので、**描画の 1 ピクセルに 1 区間**
/// あれば山を落とさない。D-3b のトレンドの描画幅は画面幅以下（フル HD の全幅でも
/// 2000px 未満）で、それ以上の点は LineChart の側で間引かれて見えない。
/// 時間で言えば、窓 1 時間を 2000 区間に割ると 1.8 秒、既定の 10 分なら
/// 0.3 秒で、1 秒周期の収集なら 10 分窓は全サンプルがそのまま入る。
pub const HISTORY_MAX_BINS: usize = 2000;

/// 1 回の応答全体の点の数の予算: **タグの本数（重複を除く）× `bins`** の上限。
///
/// 32000 = 16 ペン × 2000 区間 = 32 ペン × 1000 区間。1 点はおよそ 40 バイトの
/// JSON なので応答は 1.3MB 程度で収まる（旧 8000 点は 8 ペン × 1000 区間）。
/// 16 ペンのトレンドを 2000px で描けば 1 ピクセル 1 区間で足りる。ペンがさらに
/// 多いときは 1 本あたりの区間を減らしてもらう。
/// 16 ペン × 31 日の読み出しの時間は実データで未測定（trend-design.md §6）。
pub const HISTORY_MAX_POINTS: usize = 32000;

/// 1 回の読み出しの待ち時間の上限。過ぎたら [`Readout::Unavailable`]。
///
/// 打ち切ったときに残るものは無い: 読み出しは読み取り専用の接続を開いて
/// `SELECT` するだけで、future を捨てれば接続も閉じる（docs/
/// implementation-checklist.md §5「時間切れで見捨てた非同期処理の副作用は
/// 残る」- この処理には副作用が無い）。
pub const HISTORY_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// 長い読み出し（[`HISTORY_LONG_READ_THRESHOLD_MS`] 超）の待ち時間の上限。
/// 絞りの待ちも含めた全体。過ぎたら [`Readout::Unavailable`] だが、読み出し自体は
/// 終わるまで札を持って走り続ける（[`run_long_read`]）。
pub const HISTORY_LONG_READ_TIMEOUT: Duration = Duration::from_secs(30);

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

    /// 長い読み出し（幅が [`HISTORY_LONG_READ_THRESHOLD_MS`] を超える。ちょうどは
    /// 短い側）か。絞りと待ち時間の対象かどうかを決める。
    fn is_long(&self) -> bool {
        self.to_ms.saturating_sub(self.from_ms) > HISTORY_LONG_READ_THRESHOLD_MS
    }
}

/// 履歴の 1 点: 間引いた 1 区間の最小・最大。**欠測と非有限は両方 `null`**
/// （このモジュールの doc「返すもの」）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
#[derive(utoipa::ToSchema)]
pub struct HistoryPoint {
    /// 区間の始まり、または素通しのときはサンプルの時刻（UTC epoch ミリ秒）。
    pub t_ms: i64,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

/// タグ 1 本の系列。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
#[derive(utoipa::ToSchema)]
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
#[derive(utoipa::ToSchema)]
pub struct CollectHistory {
    /// 要求の期間をそのまま返す（両端を含む）。
    pub from_ms: i64,
    pub to_ms: i64,
    /// 要求順（重複は除く）。レジストリに居ないタグは含まない。
    pub series: Vec<HistorySeries>,
    /// レジストリに居なかったタグ ID（要求順）。
    pub unknown_tag_ids: Vec<i64>,
}

pub(super) fn field_error(field: &str, message: impl Into<String>) -> FieldError {
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
    } else if !unique.is_empty()
        && unique.len() <= HISTORY_MAX_TAGS
        && unique.len().saturating_mul(bins) > HISTORY_MAX_POINTS
    {
        // 本数と bins がそれぞれ上限内のときだけ見る（どちらかが外れていれば
        // そちらの誤りで足りる）。直すのはどちらでもよいが、画面の側で
        // 調整するのは bins なので、誤りは bins に付ける。
        errors.push(field_error(
            "bins",
            format!(
                "タグの本数 × bins は {HISTORY_MAX_POINTS} 以下にしてください（{} × {bins}）",
                unique.len()
            ),
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
pub(super) struct TagPlacement {
    pub(super) group_id: i64,
    pub(super) simulation: bool,
}

/// 要求したタグの今の所属グループと、接続がシミュレーションかを引く。
/// 居ないタグはマップに入らない。
pub(super) async fn tag_placements(
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

/// 系列の点を**必ず `bins` 個以下**にする（このモジュールの doc「上限」）。
///
/// `banto-tsquery` の区切った経路（`bins` 個以下）と、ファイルが無い・
/// グループが無いときの欠測だけの経路（同じく `bins` 個以下）はそのまま通る。
/// 超えるのは拡大しきったときの素通し（最大 `bins × 2` 行）だけで、そのときは
/// `[from_ms, to_ms]` を幅 `ceil((to - from + 1) / bins)` の `bins` 個以下の
/// 区間に割り直し、区間ごとに**最小の最小・最大の最大**へ畳む（値のある点が
/// 無い区間は `null`。山を落とさない、の原則は保つ）。点の時刻は区間に
/// 入った最初の点の時刻（サンプルの実際の時刻を残す）。戻り値の 2 つ目は
/// 割り直したときの区間の幅（割り直さなければ `None`）。
fn fit_to_bins(
    points: Vec<HistoryPoint>,
    from_ms: i64,
    to_ms: i64,
    bins: usize,
) -> (Vec<HistoryPoint>, Option<i64>) {
    if points.len() <= bins || bins == 0 {
        return (points, None);
    }
    let width = i128::from(to_ms) - i128::from(from_ms) + 1;
    let bins_i = bins as i128;
    let bucket = (width + bins_i - 1) / bins_i;
    let mut out: Vec<(i128, HistoryPoint)> = Vec::with_capacity(bins);
    for point in points {
        // 範囲外の点は来ない（`banto-tsquery` は両端を含む範囲だけ返す）が、
        // 来ても端の区間に寄せて個数の約束を守る。
        let offset = (i128::from(point.t_ms) - i128::from(from_ms)).clamp(0, width - 1);
        let index = offset / bucket;
        match out.last_mut() {
            Some((last, merged)) if *last == index => {
                if let (Some(min), Some(max)) = (point.min, point.max) {
                    merged.min = Some(merged.min.map_or(min, |m| m.min(min)));
                    merged.max = Some(merged.max.map_or(max, |m| m.max(max)));
                }
            }
            _ => out.push((index, point)),
        }
    }
    let bucket_ms = i64::try_from(bucket).unwrap_or(i64::MAX);
    (out.into_iter().map(|(_, p)| p).collect(), Some(bucket_ms))
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
            let (points, refit_ms) = fit_to_bins(
                points_for_column(&range, column),
                request.from_ms,
                request.to_ms,
                request.bins,
            );
            let bin_ms = refit_ms.map_or(range.bin_ms, |ms| ms.max(range.bin_ms));
            by_tag.insert(*id, (bin_ms, points));
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

/// 長い読み出しの打ち切りの結末（[`run_long_read`]）。
#[derive(Debug, PartialEq, Eq)]
pub(super) enum LongReadStop {
    /// `timeout` を過ぎた（札の待ち、または読み出し中）。
    TimedOut,
    /// 読み出しのタスクが panic した・取り消された。
    Aborted,
}

/// 長い読み出しを札つきで走らせる。
///
/// 札は**呼び出し側で先に取り**（待ちは `timeout` に含む）、読み出しは
/// `tokio::spawn` したタスクが札を持ったまま行う。`timeout` は `JoinHandle` を
/// 待つ側にだけ掛かり、過ぎてもタスクは abort しない（終わるまで札を持つ）。
/// 理由: `sqlx` の SQLite は別スレッドで文を実行するので future を捨てても文は
/// 走り続けうる。札を先に返すと実際に走る重い読み出しが 2 本を超えうる。
pub(super) async fn run_long_read<T, F>(
    gate: &Arc<Semaphore>,
    timeout: Duration,
    work: F,
) -> Result<T, LongReadStop>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let gate = Arc::clone(gate);
    let waited = tokio::time::timeout(timeout, async move {
        // 札の待ち。ここで打ち切られても、まだ何も spawn していない。
        let permit = gate
            .acquire_owned()
            .await
            .map_err(|_closed| LongReadStop::Aborted)?;
        let task = tokio::spawn(async move {
            let out = work.await;
            drop(permit); // 読み出しが本当に終わってから返す
            out
        });
        task.await.map_err(|_join| LongReadStop::Aborted)
    })
    .await;
    waited.unwrap_or(Err(LongReadStop::TimedOut))
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
        // 書き手と同じ置き場で読む（#623 PR7）: サービスは握った `data.dir` の中で、日付ファイルの
        // 本体・`-wal`・`-shm` を握ってから読み取り専用で開く（リンクにされた名前は断る）。
        // デスクトップ・`data.dir` が外のときはパス（[`super::CollectorContext::data_place`]）。
        let query = TsQuery::with_place(ctx.data_place());
        let (limit, outcome) = if request.is_long() {
            // 長い読み出し: 同時 2 本に絞り、spawn したタスクが札を持つ。
            let pool = ctx.pool.clone();
            let request = request.clone();
            let outcome = run_long_read(
                &self.inner.history_long_reads,
                HISTORY_LONG_READ_TIMEOUT,
                async move { read_history(&pool, &query, &request).await },
            )
            .await;
            (HISTORY_LONG_READ_TIMEOUT, outcome)
        } else {
            let outcome = tokio::time::timeout(
                HISTORY_READ_TIMEOUT,
                read_history(&ctx.pool, &query, request),
            )
            .await
            .map_err(|_elapsed| LongReadStop::TimedOut);
            (HISTORY_READ_TIMEOUT, outcome)
        };
        match outcome {
            Ok(Ok(history)) => Readout::Ready { data: history },
            Ok(Err(err)) => {
                eprintln!("banto: 履歴の読み出しに失敗しました: {err}");
                Readout::Unavailable
            }
            Err(LongReadStop::TimedOut) => {
                eprintln!(
                    "banto: 履歴の読み出しが{}秒以内に終わりませんでした",
                    limit.as_secs()
                );
                Readout::Unavailable
            }
            Err(LongReadStop::Aborted) => {
                eprintln!("banto: 履歴の読み出しのタスクが途中で終わりました");
                Readout::Unavailable
            }
        }
    }
}

#[cfg(test)]
pub(super) mod tests;
