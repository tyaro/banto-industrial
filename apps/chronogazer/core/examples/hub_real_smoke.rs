//! 実稼働中の banto-hub に対する chronogazer の Hub 購読
//! （#383 段階1、PR #385）の実機検証ツール（オーナー指示 2026-09-17）。
//!
//! chronogazer のデスクトップ（Tauri）ウィンドウは自動操作できないため、
//! **GUI を経由せず、Tauri コマンドと `crate::rest` が共有しているのと
//! 同じサービス層 [`chronogazer_core::hub::HubService`] を直接叩く**。
//! **製品コード（`src/`）は 1 行も変更しない** - このファイルと、必要が
//! あれば `Cargo.toml` の `[dev-dependencies]` だけが変更対象。
//!
//! 前例は `crates/banto-tagclient/examples/real_hub_smoke.rs`（実 Hub 相手の
//! 手動確認用 example）で、構成・出力の作法をそれに合わせている:
//! 番号付きの段階見出しを印字し、**途中で panic せず**、最後に PASS/FAIL の
//! 要約表を出す。前段の失敗で後続を試せなかった場合は「失敗」ではなく
//! 「スキップ」として区別する（前例と同じ理由 - 読み手にとって全く違う
//! 情報だから）。
//!
//! ## 実行方法
//!
//! ```text
//! cargo run --example hub_real_smoke -p chronogazer-core
//! ```
//!
//! 環境変数（すべて既定値あり）:
//!
//! | 変数 | 既定 | 意味 |
//! | --- | --- | --- |
//! | `HUB_URL` | `http://127.0.0.1:8722` | 接続先 Hub |
//! | `CG_SMOKE_DIR` | OS の一時ディレクトリ配下に自動生成 | 設定 DB を置く場所（APIキーはメモリ上だけに置く - 下記参照） |
//! | `CG_SMOKE_TAGS` | 空＝catalog の全タグ | 選択するタグの external name をカンマ区切りで指定 |
//! | `CG_SMOKE_WATCH_SECS` | `20` | 値を観測し続ける秒数 |
//! | `CG_SMOKE_HOLD_SECS` | `0` | 手順5(再起動の模擬)のあと、さらに観測を続ける秒数（オーナーが実機側でタグ削除・Hub停止・Hub再開を試す時間） |
//! | `CG_SMOKE_REVOKE` | `0` | `1` を指定すると、手順7でこの実行が発行したAPIキーをHub側で失効させる（下記「発行したAPIキーの後始末」参照） |
//!
//! ## 購読状態の観測（オーナー指示 2026-09-17 追加）
//!
//! 変化検出の対象は `state` / `reason` / `subscribedCount` / `unresolved` /
//! `unsupported` / `lastError` の6つ。**そのいずれかが前回の観測から
//! 変わったときだけ1行**印字する（毎ティック全部出すとうるさいため）。
//! 行頭には実行開始からの経過秒を付け、`live` を離れる/戻る・未解決/購読
//! 不可のタグに新しい名前が増える、といった節目の行には `*` を付けて
//! 目立たせる（節目は「前回に無かった名前が増えたか」の集合差分で見る -
//! 単なる「空→非空」だけでは、既に未解決が1件ある状態でさらに別のタグが
//! 消えたときに見落とす）。
//!
//! `lastValueAt` は変化検出には**使わない**（表示にだけ載る）。実機で値が
//! 流れている間はほぼ毎ティック動くため、比較対象に入れると「変わった
//! ときだけ1行」が崩れて実質ライブ中は全ティック出力になり、状態遷移を
//! 追うというこの機能の目的が潰れる。値が実際に流れていることは、手順4・5
//! の値の表・品質内訳・更新回数の方で確認する。
//!
//! 値の表（`tag`/`v`/`q`/`t`/`value_source`）は従来どおり5秒ごと。
//! `CG_SMOKE_HOLD_SECS` に正の値を入れると、手順5の直後にこの状態観測
//! だけを指定秒数続け（オーナーがHub側を操作する時間）、記録した遷移履歴を
//! 最後のPASS/FAIL表の直前に時系列でまとめて出す。
//!
//! ## KeyStore はプロセス内メモリだけ（2026-09-17 Copilotレビュー対応）
//!
//! `banto-hub-bootstrap` が提供する
//! [`banto_hub_bootstrap::state::memory::MemoryKeyStore`] をそのまま使う。
//! 以前はここに素朴な JSON ファイル実装を書いていたが、`KeyStore` トレイトの
//! doc（`crates/banto-hub-bootstrap/src/state.rs`）に
//! 「Implementations must never write the key anywhere else (log, settings
//! row, **temp file**)」と明記されている契約に正面から違反していたため
//! 撤回した。手順5「再起動の模擬」は同一プロセス内で `HubService` を
//! 作り直すだけ（プロセスをまたがない）ので、**同じ `MemoryKeyStore`
//! インスタンスを手順2・手順5の両方に渡す**ことで「キーが再発行されず
//! 再利用される」ことは従来どおり証明できる。**別プロセスをまたいだ
//! 再利用を確かめたいなら、正しい経路は OS キーリング（＝デスクトップ
//! アプリ）であって、平文ファイルではない。**
//!
//! ## 発行したAPIキーの後始末（2026-09-17 Copilotレビュー対応）
//!
//! `MemoryKeyStore` は実行のたびに空から始まるので、試運転中の Hub に
//! 対して実行するたびに**新しい `read` キーが Hub 側に発行される**。
//! [`Bootstrapper::disconnect`](banto_hub_bootstrap::Bootstrapper::disconnect)
//! はローカルの設定を消すだけで Hub 側のキーは失効させない契約
//! （他のインストールを巻き込まないため）なので、このハーネスを繰り返し
//! 実行すると Hub 側に孤児キーが溜まる（実機で4本溜まったことを確認済み）。
//!
//! 手順7（後片付け）は毎回、発行した `key_id`/`key_name` と失効用の
//! `curl` コマンド例を**必ず**印字する。加えて `CG_SMOKE_REVOKE=1` を
//! 指定したときだけ、**このインストールが手順2で発行した `key_id` だけを
//! 対象に**（名前で他のキーを探さない）失効させる。**サマリが嘘を
//! つかないよう、失効を頼まれたのに失敗した場合は手順7を FAIL にし
//! （終了コード1）、同じ手動失効の案内も出す**（2026-09-17 Copilotレビュー
//! 指摘。以前は「致命扱いにせず案内を出して終わる」だったが、それでは
//! サマリが実際には失敗した失効を成功に見せてしまうため撤回した）。
//!
//! 判定は `KeyOutcome`（`Undetermined` / `ConfirmedAbsent` /
//! `ConfirmedDifferentHub` / `ConfirmedPresent`）の4分岐に整理して
//! いる。**「発行されていない」と PASS で言い切ってよいのは、それを
//! 積極的に確認できたときだけ**という既定を反転した原則を通すため
//! （2026-09-18 Copilotレビュー指摘: 接続先の取り違え・判定材料の欠測・
//! 接続前の読み取り失敗・`connect()` 自体のエラーという同型の穴が4回
//! 見つかった）、`classify_key_outcome` は接続前後の `hub.record` の
//! **実際の差分**だけで判定し、`connect()` が `Ok`/`Err` のどちらを
//! 返したかには依存しない - `Bootstrapper` は設定の保存
//! （`save_record()`）の**後**に最終確認を行うため、その確認が失敗して
//! `connect()` 全体が `Err` になっても、実際にはキーが発行・保存されて
//! いることがある。読み取りに失敗した経路はすべて `Undetermined` に
//! 合流し、手順7を FAIL にして自動失効しない。
//!
//! ## やらないこと
//!
//! * 製品コード（`src/`）の変更。
//! * OS キーリングへの書き込み（上記参照。ディスクにも一切書かない）。
//! * banto-hub 側の設定変更（接続・タグの作成は別途行う）。
//! * CI への追加 - **example なので `cargo test` では走らない**。実 Hub が
//!   要るためこの example を CI で自動実行することもしない。

use std::collections::BTreeMap;
use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use banto_hub_bootstrap::state::memory::MemoryKeyStore;
use banto_hub_bootstrap::{HubRecord, KeyStore};
use banto_tagclient::Endpoint;
use chronogazer_core::db::init_db;
use chronogazer_core::hub::{HubService, HubSubscriptionView, HubTagView, HubValueView};
use chronogazer_core::settings::SettingsService;
use tokio::time::sleep;

const DEFAULT_HUB_URL: &str = "http://127.0.0.1:8722";
const DEFAULT_WATCH_SECS: u64 = 20;
const POLL_INTERVAL: Duration = Duration::from_secs(1);
/// live に到達するまで待つ上限。`banto-tagclient` 側の初回ハンドシェイクや
/// キー自己発行を含むので、購読だけの `real_hub_smoke.rs` の 5 秒より長めに
/// 取っている。
const LIVE_WAIT_TIMEOUT: Duration = Duration::from_secs(30);
const VALUE_TABLE_INTERVAL: Duration = Duration::from_secs(5);
/// `revoke_api_key` の1リクエストに掛ける上限。HOLDの手順でHubを止めた
/// まま `CG_SMOKE_REVOKE=1` を指定すると、上限が無いと `send()` で
/// 止まったまま最後の要約に到達できない（2026-09-17 Copilotレビュー
/// 指摘）。失効はbest effortなので、超えたら失敗として手動失効の案内を
/// 出して先へ進む。
const REVOKE_TIMEOUT: Duration = Duration::from_secs(5);

/// `chronogazer_core::hub` の設定 KV キー（`hub.record`）をここでも読む理由:
/// [`chronogazer_core::hub::HubView`] は画面が必要としない
/// `keyring_account` を持っていないが、本ハーネスの手順2・手順5は「同じ
/// keyring account / 同じ key_name が再利用されたか」を確かめる必要がある。
/// 定数はそちらの crate 内では private なので、ここでは同じ文字列リテラルを
/// **読み取り専用**でミラーする（書き込みは一切しない。値の形
/// （[`HubRecord`] の JSON）は `banto-hub-bootstrap` の公開型そのもの）。
const HUB_RECORD_SETTINGS_KEY: &str = "hub.record";

/// `chronogazer_core::hub` の設定 KV キー（`hub.installation_id`）を
/// ここでも読む理由は [`HUB_RECORD_SETTINGS_KEY`] と同じ（private な
/// 定数を読み取り専用でミラーする）。手順7が「判定不能」になったとき、
/// 手動確認の案内に「このインストールが発行したキーの名前の接頭辞
/// （`chronogazer-{installation_id}-`）」を添えるために使う。
/// `HubService::new` がこの値を手順1の時点で既に発行・永続化して
/// いるはず（`hub.record` とは別の設定行なので、`hub.record` 側が
/// 読めなくてもこちらは読めることがある）。
const HUB_INSTALLATION_ID_SETTINGS_KEY: &str = "hub.installation_id";

/// 1段階の検証結果の状態。「実際に試して失敗した」（`Fail`）と「試せな
/// かった」（`Skipped*`）は読み手にとって全く違う情報なので区別する
/// （`real_hub_smoke.rs` と同じ設計）。
///
/// **スキップはさらに2種類ある**（2026-09-18 Copilotレビュー指摘、
/// オーナーが実機で再現: 既定実行 - `CG_SMOKE_HOLD_SECS=0` で手順6の
/// HOLD がスキップされるだけ - が必ず終了コード1になっていた）:
///
/// * [`Self::SkippedBlocked`][]: **前段の失敗が原因で**試せなかった
///   （接続できなかったので catalog・購読・再起動をスキップ、など）。
///   これは「検証できていない」ので非成功のまま - PASS にすると、この
///   ハーネスで一番大事な「サマリが嘘をつかない」が逆向きに壊れる。
/// * [`Self::SkippedOptional`][]: **操作者が明示的に選んで**この任意
///   フェーズを実行しなかった（`CG_SMOKE_HOLD_SECS=0` など）。これは
///   何も壊れていない正常な既定動作なので、成功として扱ってよい。
///
/// **どちらに入るかを書き手が選び忘れたら失敗側に倒れる**ように、
/// [`StepResult::skipped`]（汎用名）は `SkippedBlocked` を返す既定とし、
/// `SkippedOptional` は専用の [`StepResult::skipped_optional`] を明示的に
/// 呼んだときだけ得られるようにしてある。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepStatus {
    Pass,
    Fail,
    /// 前段の失敗が原因で試せなかった。非成功（終了コードに反映）。
    SkippedBlocked,
    /// 操作者が明示的に選んでこの任意フェーズを実行しなかった。成功
    /// 扱い。
    SkippedOptional,
}

impl StepStatus {
    fn mark(self) -> &'static str {
        match self {
            StepStatus::Pass => "OK",
            StepStatus::Fail => "NG",
            StepStatus::SkippedBlocked | StepStatus::SkippedOptional => "SKIP",
        }
    }

    fn is_pass(self) -> bool {
        matches!(self, StepStatus::Pass | StepStatus::SkippedOptional)
    }
}

struct StepResult {
    name: &'static str,
    status: StepStatus,
    detail: String,
}

impl StepResult {
    fn new(name: &'static str, status: StepStatus, detail: impl Into<String>) -> Self {
        Self {
            name,
            status,
            detail: detail.into(),
        }
    }

    fn pass(name: &'static str, detail: impl Into<String>) -> Self {
        Self::new(name, StepStatus::Pass, detail)
    }

    fn fail(name: &'static str, detail: impl Into<String>) -> Self {
        Self::new(name, StepStatus::Fail, detail)
    }

    /// 前段の失敗が原因で試せなかった場合（既定 - [`StepStatus`] のdoc
    /// comment参照。「操作者が明示的に選んで実行しなかった」場合は
    /// [`Self::skipped_optional`] を使う）。
    fn skipped(name: &'static str, detail: impl Into<String>) -> Self {
        Self::new(name, StepStatus::SkippedBlocked, detail)
    }

    /// 操作者が明示的に選んでこの任意フェーズを実行しなかった場合
    /// （例: `CG_SMOKE_HOLD_SECS=0`）。成功として扱う。
    fn skipped_optional(name: &'static str, detail: impl Into<String>) -> Self {
        Self::new(name, StepStatus::SkippedOptional, detail)
    }
}

fn env_var(name: &str, default: &str) -> String {
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| default.to_owned())
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default()
}

/// 保存済みの `hub.record`（[`HubRecord`] の JSON）を読む。手順2・手順5の
/// 「keyring account」「key_name が同じか」の表示・比較専用（設定 KV は
/// [`HubService`] が書き、ここでは読むだけ）。
///
/// **「レコードが無い」（`Ok(None)`）と「読み取り/パースに失敗した」
/// （`Err`）を区別する**（2026-09-18 Copilotレビュー指摘）。以前は両方を
/// `None` に潰していたため、キーを発行した直後にこの読み取りだけが
/// 失敗すると、手順7が「発行済みAPIキーはありません」と表示して
/// **PASSになってしまっていた**（実際にはキーがHubに残ったまま）。
/// 呼び出し側（手順2・手順7）はこの区別を使い、読み取り失敗を
/// 「レコード無し」と取り違えない。
async fn read_hub_record(settings: &SettingsService) -> Result<Option<HubRecord>, String> {
    let raw = match settings.get(HUB_RECORD_SETTINGS_KEY).await {
        Ok(value) => value,
        Err(err) => return Err(format!("設定の読み取りに失敗しました: {err}")),
    };
    let Some(raw) = raw else {
        return Ok(None);
    };
    if raw.trim().is_empty() {
        return Ok(None);
    }
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|err| format!("hub.recordのJSON解析に失敗しました: {err}"))
}

fn print_catalog_table(tags: &[HubTagView]) {
    println!(
        "  {:<32} {:<32} {:<10} {:<10} {:<8}",
        "external_name", "name", "data_type", "unit", "tag_kind"
    );
    for tag in tags.iter().take(20) {
        println!(
            "  {:<32} {:<32} {:<10} {:<10} {:<8}",
            tag.external_name,
            tag.name,
            tag.data_type,
            tag.unit.clone().unwrap_or_else(|| "-".to_owned()),
            tag.tag_kind
        );
    }
    if tags.len() > 20 {
        println!("  ...(先頭20件のみ表示。総数 {} 件)", tags.len());
    } else {
        println!("  (総数 {} 件)", tags.len());
    }
}

/// `t` 列は [`HubValueView::t`]（epoch ミリ秒、`format_last_value_at` の
/// doc comment参照）をそのまま生の整数で出す - 日時に整形していないので
/// `lastValueAt` にあった秒/ミリ秒の取り違えは起こらない
/// （2026-09-17 オーナー実機確認: この列は問題なしと確認済み）。
fn print_value_table(values: &[HubValueView]) {
    if values.is_empty() {
        println!("  (値なし)");
        return;
    }
    println!(
        "  {:<32} {:<14} {:<8} {:<14} {:<12}",
        "tag", "v", "q", "t", "value_source"
    );
    for value in values {
        let v = value
            .v
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".to_owned());
        println!(
            "  {:<32} {:<14} {:<8} {:<14} {:<12}",
            value.tag, v, value.q, value.t, value.value_source
        );
    }
}

/// 品質ラベル別の観測回数（[`HubValueView::q`] は `good|stale|bad`、または
/// Hub が送ってきた未知のラベルそのまま - `banto-tagclient` が丸めない設計を
/// そのまま引き継ぐので、ここでも既知3種と「未知」を分けて数える）。
#[derive(Default)]
struct QualityTally {
    good: u64,
    stale: u64,
    bad: u64,
    unknown: BTreeMap<String, u64>,
}

impl QualityTally {
    fn record(&mut self, q: &str) {
        match q {
            "good" => self.good += 1,
            "stale" => self.stale += 1,
            "bad" => self.bad += 1,
            other => *self.unknown.entry(other.to_owned()).or_insert(0) += 1,
        }
    }

    fn summary(&self) -> String {
        let mut parts = vec![
            format!("good={}", self.good),
            format!("stale={}", self.stale),
            format!("bad={}", self.bad),
        ];
        for (label, count) in &self.unknown {
            parts.push(format!("未知({label})={count}"));
        }
        parts.join(", ")
    }
}

/// [`unix_seconds_to_utc_string`] が使う、日数since-epoch -> `YYYY-MM-DD`。
/// Howard Hinnant の `civil_from_days` アルゴリズム
/// (http://howardhinnant.github.io/date_algorithms.html)。日付/時刻クレート
/// を1箇所の変換のためだけに増やさない、という方針は
/// `chronogazer_core::db::iso_date_from_days_since_epoch` と同じ（あちらは
/// `pub(crate)` で他クレートの example からは呼べないため、ここに同じ
/// アルゴリズムを複製する）。
fn iso_date_from_days_since_epoch(days: i64) -> String {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// UNIX 秒（UTC）を人が読める形にする。`lastValueAt` の表示専用。
fn unix_seconds_to_utc_string(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let time_of_day = secs.rem_euclid(86_400);
    let (h, m, s) = (
        time_of_day / 3600,
        (time_of_day / 60) % 60,
        time_of_day % 60,
    );
    format!(
        "{} {h:02}:{m:02}:{s:02} UTC",
        iso_date_from_days_since_epoch(days)
    )
}

/// `lastValueAt`（[`HubSubscriptionView::last_value_at`]）を表示用に整形する。
///
/// **単位は epoch ミリ秒**（`docs/tag-server-design.md`「タイムスタンプ:
/// サンプル取得時刻（サーバー時計、UTC、ミリ秒）。PLC 時計は使わない」―
/// `banto_tagclient::ValuesSnapshot::t` 由来で、値の表の `t` 列と同じ単位）。
/// 秒として解釈すると実機で `58681-09-22 ...` のようなあり得ない年になる
/// バグが2026-09-17の実機確認で見つかったため、ここで秒に変換してから
/// 整形する。`None` は「まだ受信していません」であって時刻の欠落ではない
/// ので、そのまま伝える（`0` などに丸めない）。
fn format_last_value_at(value: Option<i64>) -> String {
    match value {
        Some(millis) => unix_seconds_to_utc_string(millis.div_euclid(1000)),
        None => "まだ受信していません".to_owned(),
    }
}

/// [`StatusTracker`] が「変化」を判定する対象フィールドだけの写し。
///
/// 変化検出の対象は `state` / `reason` / `subscribed_count` / `unresolved` /
/// `unsupported` / `last_error` の6つだけ。**`lastValueAt` は意図的に含め
/// ない**（2026-09-17 Copilotレビューで「HOLD履歴が値の到着を示せない」と
/// 指摘されたが、採らないと判断した - 理由: 実機で値が流れている間は
/// `lastValueAt` がほぼ毎ティック動く（`banto-tagclient` が値を受けるたびに
/// 進む）ため、比較対象に入れると「前回と変わったときだけ1行」という設計が
/// 壊れて**実質ライブ中は全ティック出力**になり、このトラッカーの目的
/// そのもの（状態遷移だけを読めるようにする）が潰れる。値が流れている
/// ことそのものは手順4・5の値の表・品質内訳・更新回数で別途確認できる。
/// `lastValueAt` は表示（[`StatusTracker::observe`] の出力行）にだけ載せる。
///
/// `last_error`（[`HubSubscriptionView::last_error`]）は比較対象に含める
/// （2026-09-17 オーナー実機確認: `reconnecting` のまま固まる障害の原因が
/// `BindingUnresolved` か `Transport` かの切り分けに、状態行のこの値が
/// 決定的だった。無いとコードを読むまで分からない）。
#[derive(Clone, PartialEq, Eq)]
struct StatusKey {
    state: &'static str,
    reason: Option<String>,
    subscribed_count: usize,
    unresolved: Vec<String>,
    unsupported: Vec<String>,
    last_error: Option<String>,
}

impl StatusKey {
    fn from_view(view: &HubSubscriptionView) -> Self {
        let mut unresolved = view.unresolved.clone();
        unresolved.sort();
        let mut unsupported = view.unsupported.clone();
        unsupported.sort();
        Self {
            state: view.state,
            reason: view.reason.clone(),
            subscribed_count: view.subscribed_count,
            unresolved,
            unsupported,
            last_error: view.last_error.clone(),
        }
    }
}

/// `next` に `prev` には無かった要素が1件でも増えたか（集合差分）。
/// [`StatusTracker::observe`] の「節目」判定専用 - 空→非空という特殊ケース
/// だけでなく、「1件ある状態でさらに別の名前が増えた」も同じ形で拾える。
/// 要素数は選択タグ数程度（実運用でせいぜい数十件）で、ここは表示頻度も
/// 低いので、集合を作らず単純な `contains` 探索で十分。
fn has_new_entries(prev: &[String], next: &[String]) -> bool {
    next.iter().any(|name| !prev.contains(name))
}

/// 手順5のあと（[`CG_SMOKE_HOLD_SECS` 相当] の保持観測フェーズ）で記録する
/// 1件の状態遷移。実機確認の記録としてそのまま貼れる形にするため、表示に
/// 必要な最小限（経過秒・状態・理由・未解決の有無・lastError）だけを持つ。
struct TransitionRecord {
    elapsed_secs: f64,
    state: &'static str,
    reason: Option<String>,
    has_unresolved: bool,
    last_error: Option<String>,
    notable: bool,
}

/// 購読状態の観測ループ全体（手順4のlive待ち～手順5の再起動後～保持観測）を
/// 通しで使う「前回と変わったときだけ1行出す」ための状態。
///
/// 経過秒は `start`（このハーネスの実行開始時刻）からの通し番号にしている -
/// 手順をまたいだ1本のタイムラインとして読めた方が、オーナーが実機側で
/// Hubを操作したタイミングと突き合わせやすいため。
struct StatusTracker {
    start: Instant,
    last: Option<StatusKey>,
    /// 保持観測フェーズの間だけ `true`。このフラグが立っているあいだの
    /// 変化だけを [`Self::log`] に積む（要約に出すのは保持観測の履歴だけで
    /// よいため）。
    logging: bool,
    log: Vec<TransitionRecord>,
}

impl StatusTracker {
    fn new(start: Instant) -> Self {
        Self {
            start,
            last: None,
            logging: false,
            log: Vec::new(),
        }
    }

    /// 保持観測フェーズを開始する。`last` をリセットして、次の観測を
    /// 「変化あり」として必ず1行印字・記録させる - 保持観測の開始時点の
    /// 状態を履歴の起点として残すため（オーナーがHubを操作し始める前の
    /// 基準点が要約に残っていないと、そこから何が変わったか読み取れない）。
    fn begin_hold(&mut self) {
        self.last = None;
        self.logging = true;
    }

    fn end_hold(&mut self) {
        self.logging = false;
    }

    /// 現在の購読状態を読み、前回の観測から変わっていれば1行印字する
    /// （保持観測フェーズ中なら [`Self::log`] にも積む）。
    fn observe(&mut self, view: &HubSubscriptionView) {
        let key = StatusKey::from_view(view);
        if self.last.as_ref() == Some(&key) {
            return;
        }
        let elapsed = self.start.elapsed().as_secs_f64();
        // 「節目」= live を離れた/live に戻った、または未解決・購読不可の
        // タグに新しい名前が増えた。初回の観測（`last` が無い）は基準点で
        // あって節目ではないので対象外。
        //
        // 「新しい名前が増えたか」は**集合差分**で見る（2026-09-17
        // Copilotレビュー指摘: 以前は「空→非空」だけを見ていたため、既に
        // 未解決が1件ある状態でさらに別のタグが消えても `*` が付かず、
        // HOLD観測の節目を見落としていた）。
        let notable = self.last.as_ref().is_some_and(|prev| {
            (prev.state == "live") != (key.state == "live")
                || has_new_entries(&prev.unresolved, &key.unresolved)
                || has_new_entries(&prev.unsupported, &key.unsupported)
        });
        let marker = if notable { "*" } else { " " };
        println!(
            "{marker}[{elapsed:7.1}s] state={} reason={} subscribedCount={} unresolved=[{}] unsupported=[{}] lastError={} lastValueAt={}",
            key.state,
            key.reason.as_deref().unwrap_or("-"),
            key.subscribed_count,
            key.unresolved.join(", "),
            key.unsupported.join(", "),
            key.last_error.as_deref().unwrap_or("-"),
            format_last_value_at(view.last_value_at),
        );
        if self.logging {
            self.log.push(TransitionRecord {
                elapsed_secs: elapsed,
                state: key.state,
                reason: key.reason.clone(),
                has_unresolved: !key.unresolved.is_empty(),
                last_error: key.last_error.clone(),
                notable,
            });
        }
        self.last = Some(key);
    }
}

fn print_transition_log(log: &[TransitionRecord]) {
    println!("=== 保持観測(HOLD)中の状態遷移履歴 ===");
    if log.is_empty() {
        println!("(状態変化は記録されませんでした。CG_SMOKE_HOLD_SECS=0、または変化が無かったかのどちらかです)");
        return;
    }
    for record in log {
        let marker = if record.notable { "*" } else { " " };
        println!(
            "{marker}[{:7.1}s] state={} reason={} 未解決={} lastError={}",
            record.elapsed_secs,
            record.state,
            record.reason.as_deref().unwrap_or("-"),
            if record.has_unresolved {
                "あり"
            } else {
                "なし"
            },
            record.last_error.as_deref().unwrap_or("-")
        );
    }
}

/// live 到達まで購読状態を1秒ごとに読み、[`StatusTracker`] に渡す。到達
/// したら `Some(経過時間)`、`timeout` 以内に到達しなかったら `None`。
async fn wait_for_live(
    hub: &HubService,
    timeout: Duration,
    tracker: &mut StatusTracker,
) -> Option<Duration> {
    let start = Instant::now();
    loop {
        let view = hub.subscription().await;
        tracker.observe(&view);
        if view.state == "live" {
            return Some(start.elapsed());
        }
        if start.elapsed() >= timeout {
            return None;
        }
        sleep(POLL_INTERVAL).await;
    }
}

/// live 到達後、`watch_secs` 秒のあいだ観測を続け、5秒ごとに値の表を印字
/// しつつ品質内訳と更新回数を集計する。購読状態の変化は [`StatusTracker`]
/// が独立した頻度（変化したときだけ）で印字する。
///
/// 戻り値の `bool`（`received_any_value`）は
/// [`HubValueView::v`] が `Some` の行を1件でも見たか。**`v` が `None` の
/// 行（Hubの doc: 「値がまだ無い」であって0ではない）は品質・更新回数の
/// 集計はしても、この「値を受信した」判定には数えない**
/// （2026-09-17 Copilotレビュー指摘: 以前は `q`（品質）が付いた行なら
/// `v` の有無を見ずに「受信した」扱いにしていたため、`v=None` の行だけが
/// 並ぶ live スナップショットでも PASS になってしまっていた）。更新回数
/// （`update_count`）も同じ理由で `v` が `Some` の行だけを数える - `t` の
/// 変化それ自体は `v=None` のままでも起こりうるため。
async fn observe_live(
    hub: &HubService,
    watch_secs: u64,
    tracker: &mut StatusTracker,
) -> (QualityTally, u64, bool) {
    let start = Instant::now();
    let deadline = start + Duration::from_secs(watch_secs);
    let mut tally = QualityTally::default();
    let mut last_t: BTreeMap<String, i64> = BTreeMap::new();
    let mut update_count: u64 = 0;
    let mut received_any_value = false;
    let mut last_table_print: Option<Instant> = None;
    while Instant::now() < deadline {
        let view: HubSubscriptionView = hub.subscription().await;
        tracker.observe(&view);
        for value in &view.values {
            tally.record(&value.q);
            if value.v.is_some() {
                received_any_value = true;
                if let Some(prev) = last_t.get(value.tag.as_str()) {
                    if *prev != value.t {
                        update_count += 1;
                    }
                }
            }
            last_t.insert(value.tag.clone(), value.t);
        }
        let due = last_table_print.is_none_or(|last| last.elapsed() >= VALUE_TABLE_INTERVAL);
        if due {
            println!(
                "  -- 観測経過 {:.0}秒/{}秒, 購読状態={} --",
                start.elapsed().as_secs_f64(),
                watch_secs,
                view.state
            );
            print_value_table(&view.values);
            last_table_print = Some(Instant::now());
        }
        sleep(POLL_INTERVAL).await;
    }
    (tally, update_count, received_any_value)
}

/// 保持観測(HOLD)フェーズ本体。値の表・品質集計はしない
/// （オーナーの目的は状態遷移の観測であり、値そのものは手順4/5で既に
/// 確認済みのため - ノイズを増やさない）。
async fn hold_observe(hub: &HubService, hold_secs: u64, tracker: &mut StatusTracker) {
    let deadline = Instant::now() + Duration::from_secs(hold_secs);
    while Instant::now() < deadline {
        let view = hub.subscription().await;
        tracker.observe(&view);
        sleep(POLL_INTERVAL).await;
    }
}

/// `a` と `b` が正規化後に同じ Hub を指しているか。
///
/// **これは #382（製品側、`banto_hub_bootstrap`）のレビュー第1巡で一度
/// 指摘されたのとまったく同じ間違いを、この確認ハーネスでも踏んでいた
/// ことへの修正**（2026-09-17 Copilotレビュー指摘）。`api_keys.id` は Hub
/// ごとの連番なので、接続先を確かめずに `key_id` を使うと、別の Hub の
/// 無関係な（第三者の）キーを失効させうる。`Bootstrapper::previous_for`
/// （`crates/banto-hub-bootstrap/src/bootstrap.rs`）はこれを「正規化した
/// 接続先が一致するときだけレコードを参照する」ことで防いでおり、この
/// 関数も同じ規律に揃える。
///
/// 正規化は [`hub_identity`] に委譲する（判定は1箇所に集約し、
/// [`classify_key_outcome`] もこの関数越しに同じ規則を使う）。どちらかが
/// 不正な形式なら「同じではない」として扱う（安全側 - 判断に迷ったら
/// 失効させない）。
fn same_hub(a: &str, b: &str) -> bool {
    match (hub_identity(a), hub_identity(b)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// `raw` が指す Hub の識別子: `(host, port, path prefix)` の3つ組。
///
/// `banto_hub_bootstrap::admin::keyring_account`
/// （`crates/banto-hub-bootstrap/src/admin.rs:154-159`）が
/// `KeyStore` のアカウント名に使っているのと**同じ3つ組**で正規化する。
/// 同ファイルの `keyring_account` のdoc comment（135-141行）が明記する
/// 規律 ---「the port is always spelled out (`port_or_known_default`),
/// so `http://host` and `http://host:80` are one Hub, not two」---
/// にここでも揃える（2026-09-18 Copilotレビュー指摘: 既定ポートの
/// 表記ゆれを吸収しないと `classify_key_outcome` が「別Hub」と誤判定し、
/// `CG_SMOKE_REVOKE=1` が今回発行していない既存キーを失効させる -
/// 前々回直した「他人のキーを消してしまう」側の、また別の形の再発）。
///
/// `admin::base_url` / `keyring_account` はどちらも `pub(crate)` で
/// この crate の外から呼べないため、**同じ検証・正規化ルールで実装
/// されている公開 API** `banto_tagclient::Endpoint::new` で正規化した
/// うえで、host / `port_or_known_default()` / path を個別に取り出す
/// （`admin.rs` のdoc「Mirrors `banto_tagclient::Endpoint`'s contract」
/// のとおり、スキーム・ホスト・ポート・末尾スラッシュの扱いは完全に
/// 同じ）。`Endpoint` は正規化後の base URL 自体を公開していないため、
/// 公開されている `tags_url()`（`base` に `tags` セグメントを1つ足した
/// もの）から `port_or_known_default()` とパスを逆算する - 末尾の
/// `tags` セグメントを取り除けば `keyring_account` が使う `base.path()`
/// と同じ値になる（`Endpoint::new` が末尾スラッシュを正規化してから
/// セグメントを足すため、剥がし方が一意に定まる）。
fn hub_identity(raw: &str) -> Option<(String, u16, String)> {
    let endpoint = Endpoint::new(raw).ok()?;
    let tags_url = endpoint.tags_url();
    let host = endpoint.host()?.to_owned();
    let port = tags_url.port_or_known_default()?;
    let path = tags_url
        .path()
        .strip_suffix("tags")
        .unwrap_or_else(|| tags_url.path())
        .to_owned();
    Some((host, port, path))
}

/// 手順2の結果、発行済みAPIキーについて何が分かったかの分類
/// （2026-09-18 Copilotレビュー指摘）。
///
/// **同型の「判定できないのに黙ってPASSへ倒れる」穴が4回見つかった**
/// （接続先の取り違え・判定材料の欠測・接続前の読み取り失敗・`connect()`
/// 自体のエラー）。1つずつ塞ぐのではなく、この種類ごと構造的に閉じる:
/// **「発行されていない」とPASSで言い切ってよいのは、それを積極的に
/// 確認できたときだけ**にする。[`classify_key_outcome`] はこの原則を
/// 「早期returnで`Undetermined`を返し、確認できた場合だけ最後まで
/// たどり着いて他のバリアントを組み立てる」という書き方で保証する -
/// `before`/`after` の読み取りが失敗する新しい理由が将来増えても、
/// `Result::Err` の分岐は既にすべて `Undetermined` に合流しているため、
/// 書き手が個別対応を追加し忘れても自動的に安全側に倒れる
/// （`unwrap_or_default` 的な握り潰しをしない、という意味でもある）。
#[derive(Debug, Clone, PartialEq, Eq)]
enum KeyOutcome {
    /// 判定不能。手順7は自動失効せずFAILにする。
    Undetermined(String),
    /// 確認できた: 今回のHUB_URL向けの発行済みキーは無い。
    ConfirmedAbsent,
    /// 確認できた: レコードはあるが別のHub（今回のHUB_URLとは異なる
    /// 接続先）のもの。このハーネスは触らない。
    ConfirmedDifferentHub {
        key_id: Option<i64>,
        endpoint: String,
    },
    /// 確認できた: 今回のHUB_URL向けのキーがある。
    ConfirmedPresent {
        key_id: i64,
        key_name: Option<String>,
        /// 接続前後で`key_id`が変わった（＝今回このプロセスが発行した）
        /// か、以前からのキーをそのまま維持しているだけか。
        issued_this_run: bool,
    },
}

/// [`KeyOutcome`] を判定する。
///
/// 判定は**接続前後の `hub.record` の実際の差分**だけで行い、
/// `connect()` 自体が `Ok`/`Err` のどちらを返したかには依存しない -
/// `Bootstrapper::connect_with_scopes`
/// （`crates/banto-hub-bootstrap/src/bootstrap.rs`）は `save_record()`
/// （キーの永続化）の**後**に最終確認の `verify()` を呼ぶため、
/// `verify()` が失敗して呼び出し全体が `Err`（または非 `Connected`）に
/// なっても、実際にはキーは発行・保存されていることがある
/// （今回のCopilotレビュー指摘の直接の原因）。「`connect()` が成功
/// したか」ではなく「設定の記録が実際に変わったか」を見ることで、
/// `connect()` の内部実装がどんな終わり方をしても取りこぼさない -
/// 呼び出し側（`main`）は `connect()` の戻り値が `Ok`/`Err` どちらでも
/// 必ずもう一度 `after` を読む。
///
/// `api_keys.id` は Hub ごとの連番なので、`key_id` だけの比較は接続先
/// とセットでなければ意味がない（`same_hub` のdoc comment、
/// `Bootstrapper::previous_for` と同じ理屈）。
///
/// `connect_returned_ok`（`HubService::connect()` が `Ok` を返したか）が
/// 要る理由（2026-09-18 Copilotレビュー指摘、最重要）: **「レコードが
/// 無い」は「キーを発行していない」の証明にならない**。
/// `HubService::connect()` は、`Bootstrapper` がキーを発行し設定の
/// インメモリ写しに保存した**後**、それを実ストレージへ書き戻す
/// `flush()` が失敗すると `Err` を返す
/// （`apps/chronogazer/core/src/hub.rs` の `HubService::connect`:
/// `bootstrapper.connect(...).await.map_err(...)?; self.flush().await?;`
/// `flush()` の `?` は `bootstrapper.connect()` の**後**にあるので、
/// ここで失敗すると `Ok` へは決して到達しない）。
///
/// **判定規則はこの3行に集約される**（2026-09-18 Copilotレビュー
/// 指摘2回目: 前回は「レコードが無い」経路だけ塞いだが、「レコードが
/// 変わっていない」経路が残っていた - 置き換えのキーを発行した直後に
/// `flush()` が失敗すると、`after` は書き戻し前の**古いレコードのまま**
/// なので `key_id` が変わらず、取りこぼしていた）:
///
/// - `connect()` が **`Ok`** … 従来どおり、`before`/`after` の実際の
///   差分だけで分類する。
/// - `connect()` が **`Err`** かつ レコードが**変わった**
///   （`key_id` または接続先が `before` から `after` で変わった） …
///   `Err` になった呼び出しの中で何かが変化したと積極的に確認できる
///   ので `ConfirmedPresent`（この実行が発行したと分かる）。
/// - `connect()` が **`Err`** かつ レコードが**変わっていない／無い**
///   … `Undetermined`（自動失効せず FAIL ＋ 手動案内）。
///
/// 言い換えると: **`connect()` が `Err` のときは、レコードが積極的に
/// 変わっていない限り判定不能**。「`after` の読み取りが真に最新の状態を
/// 反映していると無条件に信頼してよいのは `connect_returned_ok` の
/// ときだけ」であり、`Err` のときは「変化が観測できた」という積極的な
/// 証拠がある場合に限って例外的に確定させる。
fn classify_key_outcome(
    hub_url: &str,
    connect_returned_ok: bool,
    before: &Result<Option<HubRecord>, String>,
    after: &Result<Option<HubRecord>, String>,
) -> KeyOutcome {
    let before_record = match before {
        Ok(record) => record.as_ref(),
        Err(err) => {
            return KeyOutcome::Undetermined(format!("接続前の設定読み取りに失敗しました: {err}"))
        }
    };
    let after_record = match after {
        Ok(record) => record.as_ref(),
        Err(err) => {
            return KeyOutcome::Undetermined(format!("接続後の設定読み取りに失敗しました: {err}"))
        }
    };

    if !connect_returned_ok {
        // connect()がErrで終わったときは、bootstrapperがキーを発行・
        // 保存した後でflush()（実ストレージへの書き戻し）だけが失敗した
        // 可能性がある。その場合beforeとafterは同じ(古い)レコードを指す
        // ので、「レコードが無い/変わっていない」ことは「発行していない」
        // ことの証明にならない。beforeとafterを比べて積極的に変化が
        // 確認できたときだけConfirmedPresentとし、それ以外はすべて
        // Undeterminedに倒す。
        let changed = match (before_record, after_record) {
            (None, None) => false,
            (None, Some(_)) | (Some(_), None) => true,
            (Some(b), Some(a)) => b.key_id != a.key_id || !same_hub(&b.endpoint, &a.endpoint),
        };
        return match (changed, after_record) {
            (true, Some(after_record)) => match after_record.key_id {
                Some(key_id) => KeyOutcome::ConfirmedPresent {
                    key_id,
                    key_name: after_record.key_name.clone(),
                    issued_this_run: true,
                },
                None => KeyOutcome::Undetermined(
                    "レコードにkey_idがありません(手動キー採用は想定していません)".to_owned(),
                ),
            },
            _ => KeyOutcome::Undetermined(
                "connect()がエラーで終わり、かつ設定の記録が変わっていない(または無い)ため、発行の有無を確認できません(発行後にflush等が失敗した可能性があります)".to_owned(),
            ),
        };
    }

    // ここから先は connect_returned_ok == true。flush()まで含めて成功
    // したことが構造的に保証されているので、before/afterの実際の差分
    // だけで判定してよい（従来どおり）。
    let Some(after_record) = after_record else {
        return KeyOutcome::ConfirmedAbsent;
    };
    if !same_hub(&after_record.endpoint, hub_url) {
        return KeyOutcome::ConfirmedDifferentHub {
            key_id: after_record.key_id,
            endpoint: after_record.endpoint.clone(),
        };
    }
    let Some(key_id) = after_record.key_id else {
        // 自己発行なら必ずkey_idが付く（HubRecordのdoc）。手動キー採用
        // （key_id: None）はこのハーネスが使わない経路なので、想定外
        // として安全側（判定不能）に倒す - 決め打ちで「発行していない」
        // とは言い切らない。
        return KeyOutcome::Undetermined(
            "レコードにkey_idがありません(手動キー採用は想定していません)".to_owned(),
        );
    };
    let issued_this_run = match before_record {
        None => true,
        Some(before_record) if !same_hub(&before_record.endpoint, hub_url) => true,
        Some(before_record) => before_record.key_id != Some(key_id),
    };
    KeyOutcome::ConfirmedPresent {
        key_id,
        key_name: after_record.key_name.clone(),
        issued_this_run,
    }
}

/// 表示用に Hub の URL をサニタイズする（2026-09-18 Copilotレビュー
/// 指摘）。
///
/// `Endpoint::new` は userinfo（`http://user:pass@host`）を検証で弾くが、
/// **弾く前の生の値をログや curl 案内にそのまま出すと、誤入力に含まれた
/// パスワードが残ってしまう**。ここでは `Endpoint::new` の検証を通す前に
/// パースし、ホスト・ポート・パスだけに落とす - userinfo とクエリ文字列
/// は常に捨てる。パースそのものに失敗する形式なら、原文を一切出さず
/// プレースホルダのみ返す。
///
/// IPv6リテラル（`http://[::1]:8722`）について（2026-09-18 Copilot
/// レビュー指摘2回目）: `Url::host_str()` は角括弧なしで返すのでは
/// ないかという指摘があったが、本クレートが実際にピン留めしている
/// `url` 2.5.8（`Cargo.lock`）で検証した結果、`host_str()` は
/// IPv6アドレスを**角括弧つき**（`"[::1]"`）で返すことを確認済み
/// （手元の再現コードで `host_str()=Some("[::1]")` を確認。角括弧を
/// 追加で付け足すと `[[::1]]` になって二重に壊れるため、ここでは
/// 追加加工をしない）。回帰テストで角括弧つきのまま往復することを
/// 固定している（`sanitize_hub_url_for_display_keeps_ipv6_brackets`）。
fn sanitize_hub_url_for_display(raw: &str) -> String {
    match reqwest::Url::parse(raw) {
        Ok(url) => {
            let scheme = url.scheme();
            let host = url.host_str().unwrap_or("(ホスト不明)");
            let port = url
                .port()
                .map(|port| format!(":{port}"))
                .unwrap_or_default();
            format!("{scheme}://{host}{port}{}", url.path())
        }
        Err(_) => "(不正な形式のHUB_URL)".to_owned(),
    }
}

/// `curl` で手動失効するときの1行を組み立てる（実行はしない）。手順7の
/// 案内表示・失敗時のフォールバックの両方で使う。**呼び出し側は
/// サニタイズ済み（[`sanitize_hub_url_for_display`] を通した）URLを渡す
/// こと** - このハーネスが表示するテキストにuserinfoを含む生の値を
/// 混ぜないため。
fn revoke_curl_hint(hub_url_display: &str, key_id: i64) -> String {
    format!(
        "curl -X POST -H \"X-Banto-Client: banto\" {}",
        revoke_url(hub_url_display, key_id)
    )
}

/// 失効リクエストの絶対URLを組み立てる（純関数・テスト可能）。
/// [`revoke_curl_hint`]（表示用）と [`revoke_api_key_with_timeout`]
/// （実際に叩く）の両方がここを通ることで、表示と実際のリクエストが
/// 食い違わないようにする。
fn revoke_url(hub_url: &str, key_id: i64) -> String {
    format!(
        "{}/api/api-keys/{key_id}/revoke",
        hub_url.trim_end_matches('/')
    )
}

/// 失効リクエストの応答（HTTPステータス）をどう扱うかを決める（純関数・
/// テスト可能）。2026-09-18 Copilotレビュー指摘: 実際にPOSTを出す経路
/// （URL組み立て・ヘッダ・ステータス処理・タイムアウト）がこれまで
/// 未検証だった - ここが壊れると「失効したつもりで実は残っている」
/// になる。ステータス判定だけをこの純関数に切り出し、
/// [`revoke_api_key_with_timeout`] のモックサーバーテストから、
/// またこの関数単体からも固定できるようにする。
fn classify_revoke_response(status: reqwest::StatusCode) -> Result<(), String> {
    if status.is_success() {
        Ok(())
    } else if status == reqwest::StatusCode::UNAUTHORIZED
        || status == reqwest::StatusCode::FORBIDDEN
    {
        // `POST /api/api-keys/{id}/revoke` はロックダウン後は管理者の
        // bearer認証が要り、`X-Banto-Client` だけでは401/403になる
        // （2026-09-18 Copilotレビュー指摘）。この example は資格情報を
        // 扱わない方針（管理者トークンを受け取る環境変数・引数は作らない）
        // なので、代替経路は提供せず理由を名指しして手動対処を案内する
        // だけにする。自己発行キーの発行そのものが試運転モード限定
        // （banto-hub-bootstrapのdoc参照）なのと同じ制約。
        Err(format!(
            "HTTPステータス={status}(認証/権限エラー)。Hubがロックダウン済みのため自動失効できません - この失効APIは試運転モード中のみ動作します。ロックダウン済みのHubで失効させるにはHubの管理画面か管理者のBearerトークンが必要です(このハーネスは資格情報を扱いません)。"
        ))
    } else {
        Err(format!("HTTPステータス={status}"))
    }
}

/// `POST /api/api-keys/{id}/revoke` を叩く（ベストエフォート、
/// `CG_SMOKE_REVOKE=1` のときだけ手順7から呼ぶ）。[`REVOKE_TIMEOUT`]
/// 固定の薄いラッパー - タイムアウトを差し替えられる本体は
/// [`revoke_api_key_with_timeout`]（テストで短いタイムアウトを注入する
/// ため）。
async fn revoke_api_key(hub_url: &str, key_id: i64) -> Result<(), String> {
    revoke_api_key_with_timeout(hub_url, key_id, REVOKE_TIMEOUT).await
}

/// `POST /api/api-keys/{id}/revoke` を叩く本体。
///
/// `banto-hub-bootstrap` の `AdminClient::revoke`
/// （`crates/banto-hub-bootstrap/src/admin.rs`）と同じ経路・同じ
/// リクエスト形だが、そちらは `pub(crate)` でこの crate の外から呼べない
/// ため、`crates/banto-tagclient/examples/real_hub_smoke.rs` の
/// `set_write_control` と同じ作法（直接 reqwest で管理 REST を叩く）で
/// ここに複製する。試運転中の Hub は認証不要で、CSRFマーカー
/// `X-Banto-Client: banto`（資格情報ではない）だけで通る
/// （`admin.rs` のモジュール doc 参照）。
///
/// **必ずこのインストールが自分で発行した `key_id`（保存済みレコードの
/// もの）にだけ使う** - 名前で他のキーを探して失効させない。これは
/// `AdminClient::revoke` のdoc comment（「there is deliberately no "list
/// keys and revoke the ones whose name looks like mine" path」）と同じ
/// 規律で、こちらも他のインストール・他のツールが発行したキーを巻き込ま
/// ないための不変条件。
async fn revoke_api_key_with_timeout(
    hub_url: &str,
    key_id: i64,
    timeout: Duration,
) -> Result<(), String> {
    let http = reqwest::ClientBuilder::new()
        .no_proxy()
        .timeout(timeout)
        // 製品の管理クライアント（banto-hub-bootstrap の AdminClient、
        // banto-tagclient の RestClient::new）と同じ規律: リダイレクトは
        // 追従しない。失効は資格情報を伴う管理操作なので、Hubが3xxを
        // 返しても想定外のホストへ飛ばしてはいけない
        // （2026-09-18 Copilotレビュー指摘）。
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| format!("reqwestクライアント構築失敗: {error}"))?;
    let response = http
        .post(revoke_url(hub_url, key_id))
        .header("X-Banto-Client", "banto")
        .send()
        .await
        .map_err(|error| format!("送信失敗: {error}"))?;
    classify_revoke_response(response.status())
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    println!("=== chronogazer Hub 実機確認ハーネス（#383 段階1）===");
    // **入口で1回だけ正規化する**（trim込み）（2026-09-18 Copilotレビュー
    // 指摘）: HubService::connect() は渡されたURLをtrimしてから使う
    // （crate::hub の実装）ため、前後に空白があると「接続は成功して
    // trim済みの値で保存される」のに「このハーネスの同一性判定
    // （same_hub）には未trimの値を渡している」というズレが起き、
    // 自動失効がスキップされ案内のURLも未trimのまま出てしまう。ここで
    // trimした後の1つの値だけを、以降の同一性判定・失効リクエスト・
    // 案内表示のすべてで使う。
    let hub_url = env_var("HUB_URL", DEFAULT_HUB_URL).trim().to_owned();
    // 表示用にサニタイズした値も1回だけ作る（2026-09-18 Copilotレビュー
    // 指摘）: `Endpoint` はuserinfo（`http://user:pass@host`）を検証で
    // 弾くが、**弾く前の生の値をログやcurl案内に出すと、誤入力に含まれた
    // パスワードがそのまま残ってしまう**。ホスト・ポート・パスだけに
    // 落とし、userinfo・クエリ文字列は常に捨てる。curl案内にもこの
    // 値だけを使う（`revoke_curl_hint`/手動確認コマンドの組み立て先を
    // 参照）。
    let hub_url_display = sanitize_hub_url_for_display(&hub_url);
    let watch_secs: u64 = env::var("CG_SMOKE_WATCH_SECS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_WATCH_SECS);
    let requested_tags: Vec<String> = env::var("CG_SMOKE_TAGS")
        .ok()
        .map(|value| {
            value
                .split(',')
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let hold_secs: u64 = env::var("CG_SMOKE_HOLD_SECS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    // 既定は失効させない（Copilotレビュー指摘対応、2026-09-17: 実行のたび
    // 新しい read キーが Hub 側に増え、disconnect は Hub 側を失効させない
    // 契約なので孤児キーが溜まる。既定を「勝手に失効」にはせず、明示的に
    // 選んだときだけ失効するようにする）。
    let auto_revoke = env::var("CG_SMOKE_REVOKE").ok().as_deref() == Some("1");

    let smoke_dir = match env::var("CG_SMOKE_DIR") {
        Ok(value) if !value.trim().is_empty() => PathBuf::from(value),
        _ => std::env::temp_dir().join(format!("chronogazer-hub-smoke-{}", unix_millis())),
    };
    if let Err(err) = std::fs::create_dir_all(&smoke_dir) {
        eprintln!("一時ディレクトリを作れませんでした: {smoke_dir:?}: {err}");
        std::process::exit(1);
    }
    let db_path = smoke_dir.join("chronogazer.sqlite3");
    // #383段階1 Copilotレビュー対応（2026-09-17）: APIキーはディスクに一切
    // 書かない。`MemoryKeyStore` のdoc comment（上の「KeyStoreはプロセス内
    // メモリだけ」参照）のとおり、この1インスタンスを手順2・手順5の両方に
    // 渡すことで「再発行されず再利用される」ことを証明する。
    let keys = Arc::new(MemoryKeyStore::new());

    println!("Hub URL: {hub_url_display}");
    println!("作業ディレクトリ: {}", smoke_dir.display());
    println!("観測秒数: {watch_secs}秒");
    println!(
        "保持観測(HOLD)秒数: {hold_secs}秒{}",
        if hold_secs == 0 {
            "(0=手順6をスキップ)"
        } else {
            ""
        }
    );
    println!();

    let mut results: Vec<StepResult> = Vec::new();
    // 手順4のlive待ちから保持観測まで通しで使う、購読状態の「変化したときだけ
    // 印字する」トラッカー。経過秒はこのハーネスの実行開始からの通し番号。
    let mut tracker = StatusTracker::new(Instant::now());

    // == 1. 準備 =============================================================
    println!("== 1. 準備 ==");
    let pool1 = match init_db(&db_path).await {
        Ok(pool) => pool,
        Err(err) => {
            println!("  DB初期化に失敗しました: {err}");
            results.push(StepResult::fail("準備", format!("init_db失敗: {err}")));
            print_summary(&results);
            let all_pass = results.iter().all(|r| r.status.is_pass());
            std::process::exit(if all_pass { 0 } else { 1 });
        }
    };
    let settings1 = SettingsService::new(pool1.clone());
    let hub1 = match HubService::new(settings1.clone(), keys.clone()).await {
        Ok(hub) => hub,
        Err(err) => {
            println!("  HubService初期化に失敗しました: {err}");
            results.push(StepResult::fail(
                "準備",
                format!("HubService::new失敗: {err}"),
            ));
            print_summary(&results);
            std::process::exit(1);
        }
    };
    println!("  DB: {}", db_path.display());
    println!(
        "  キー保管: プロセス内メモリ(MemoryKeyStore) - ディスク・OSキーリングには一切書き込まない"
    );
    // 手順7が「判定不能」になったときの手動確認案内に使う
    // installation_id。HubService::new（すぐ上）がここまでに発行・
    // 永続化しているはず（HUB_INSTALLATION_ID_SETTINGS_KEYのdoc参照）。
    // 読めなくても致命的ではない（手動案内から接頭辞が抜けるだけ）ので、
    // 手順1自体の成否には影響させない。
    let installation_id = settings1
        .get(HUB_INSTALLATION_ID_SETTINGS_KEY)
        .await
        .ok()
        .flatten()
        .filter(|value| !value.trim().is_empty());
    results.push(StepResult::pass(
        "準備",
        "DB・KeyStore(インメモリ)・HubServiceを初期化した",
    ));
    println!();

    // == 2. 接続 ==============================================================
    println!("== 2. 接続 ==");
    // connect() の**前後**でhub.recordを読み、その差分だけで「今回発行
    // したか」を判定する（`KeyOutcome`/`classify_key_outcome` のdoc
    // comment参照）。**connect() 自体が Ok/Err のどちらを返しても、
    // 必ずもう一度 after を読む** - Bootstrapper は save_record()（キー
    // の永続化）の後に最終確認の verify() を呼ぶため、verify() が失敗
    // して connect() 全体が Err になっても、実際にはキーが発行・保存
    // されていることがある（2026-09-18 Copilotレビュー指摘: これで
    // 4回目の同型の穴だったため、個別にこの経路だけ直すのではなく
    // before/afterの差分判定に一本化して構造的に閉じた）。
    let before_record = read_hub_record(&settings1).await;
    if let Err(err) = &before_record {
        println!("  警告: 接続前の設定読み取りに失敗しました({err})。");
    }
    let connect_view = hub1.connect(&hub_url).await;
    // connect_view が Ok かどうかを、後で connect_view を消費する前に
    // 控えておく（classify_key_outcome のdoc comment参照 - flush()の
    // 成否が保証されるのはOkのときだけ）。
    let connect_returned_ok = connect_view.is_ok();
    let after_record = read_hub_record(&settings1).await;
    if let Err(err) = &after_record {
        println!("  警告: 接続後の設定読み取りに失敗しました({err})。");
    }
    let key_outcome =
        classify_key_outcome(&hub_url, connect_returned_ok, &before_record, &after_record);

    let (step2_ok, catalog_tags, key_name_1, keyring_account_1) = match connect_view {
        Ok(view) => {
            let tag_count = view.tags.as_ref().map(|t| t.len());
            println!("  状態: {}", view.status.as_str());
            println!(
                "  タグ件数: {}",
                tag_count
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "不明(catalog未取得)".to_owned())
            );
            println!("  キー名: {}", view.key_name.as_deref().unwrap_or("-"));
            // 手順5の同一性確認（secret_before）に使う keyring_account は
            // afterの読み取りが拾えていればそこから（読めていなければ
            // Noneのままでよい - 手順5はそれ自体が「一致しない」として
            // 正しくFAILする）。
            let keyring_account = after_record
                .as_ref()
                .ok()
                .and_then(|record| record.as_ref())
                .map(|r| r.keyring_account.clone());
            println!(
                "  keyring account: {}",
                keyring_account.as_deref().unwrap_or("-")
            );
            let ok = view.status.is_connected();
            (ok, view.tags, view.key_name, keyring_account)
        }
        Err(err) => {
            println!("  接続に失敗しました: {err}");
            (false, None, None, None)
        }
    };
    results.push(if step2_ok {
        StepResult::pass("接続", format!("状態=connected キー名={:?}", key_name_1))
    } else {
        StepResult::fail("接続", "Connected状態に到達しませんでした")
    });
    println!();

    // == 3. catalog一覧 =======================================================
    println!("== 3. catalog一覧 ==");
    let catalog_tags = match (step2_ok, catalog_tags) {
        (true, Some(tags)) => {
            print_catalog_table(&tags);
            results.push(StepResult::pass("catalog一覧", format!("{}件", tags.len())));
            Some(tags)
        }
        (true, None) => {
            println!("  スキップ: 接続はできましたがcatalogを取得できませんでした。");
            results.push(StepResult::fail("catalog一覧", "catalog取得失敗"));
            None
        }
        _ => {
            println!("  スキップ: 手順2(接続)が失敗したため試せません。");
            results.push(StepResult::skipped("catalog一覧", "接続失敗のためスキップ"));
            None
        }
    };
    println!();

    // == 4. 選択と購読 ========================================================
    println!("== 4. 選択と購読 ==");
    let selected_tags: Vec<String> = match &catalog_tags {
        Some(_) if !requested_tags.is_empty() => requested_tags.clone(),
        Some(tags) => tags.iter().map(|t| t.external_name.clone()).collect(),
        None => Vec::new(),
    };
    let step4_result;
    let mut reached_live = false;
    if catalog_tags.is_none() {
        println!("  スキップ: catalogを取得できていないため選択できません。");
        step4_result = StepResult::skipped("選択と購読", "catalog未取得のためスキップ");
    } else if selected_tags.is_empty() {
        // このブランチに来るのは requested_tags が空 かつ catalog のタグが
        // 0件のときだけ（Copilotレビュー指摘、2026-09-17: 以前の文言は
        // CG_SMOKE_TAGS が catalog に一致しない場合もここに来ると書いて
        // いたが誤り）。requested_tags が空でなければ上の
        // `Some(_) if !requested_tags.is_empty()` 分岐でそのまま
        // selected_tags になる - catalog に無い名前を指定しても
        // set_selected_tags は成功し、購読は試みられて「未解決」として
        // 報告される（plan_bindings、下の観測ログの unresolved 列を参照）。
        println!("  スキップ: catalogにタグが0件のため、選択できるタグがありません。");
        step4_result = StepResult::skipped("選択と購読", "catalogのタグが0件");
    } else {
        println!(
            "  選択タグ({}件): {}",
            selected_tags.len(),
            selected_tags.join(", ")
        );
        match hub1.set_selected_tags(selected_tags.clone()).await {
            Ok(()) => {
                println!(
                    "  選択を保存しました。live到達を待ちます(最大{}秒)。",
                    LIVE_WAIT_TIMEOUT.as_secs()
                );
                match wait_for_live(&hub1, LIVE_WAIT_TIMEOUT, &mut tracker).await {
                    Some(elapsed) => {
                        reached_live = true;
                        println!("  live到達: {:.1}秒", elapsed.as_secs_f64());
                        let (tally, update_count, received_any_value) =
                            observe_live(&hub1, watch_secs, &mut tracker).await;
                        println!("  品質内訳: {}", tally.summary());
                        println!("  値が更新された回数: {update_count}");
                        // live到達（ハンドシェイク成立）だけでは成功にしない
                        // （2026-09-17 Copilotレビュー指摘: ハンドシェイクは
                        // 通ったが値が一度も来ない、という実害を見逃した）。
                        // v が Some の値を1件でも受信したか、更新
                        // （t の変化、こちらも v=Some の行のみ）を1回でも
                        // 観測したことまで求める（observe_liveのdoc参照）。
                        let received_values = received_any_value || update_count >= 1;
                        if received_values {
                            step4_result = StepResult::pass(
                                "選択と購読",
                                format!(
                                    "live到達={:.1}s, 品質内訳=[{}], 更新回数={update_count}",
                                    elapsed.as_secs_f64(),
                                    tally.summary()
                                ),
                            );
                        } else {
                            println!(
                                "  失敗: liveには到達しましたが、観測中に値を一度も受信できませんでした(ハンドシェイクのみ)。"
                            );
                            step4_result = StepResult::fail(
                                "選択と購読",
                                format!(
                                    "live到達={:.1}s だが値未受信(品質内訳=[{}], 更新回数={update_count})",
                                    elapsed.as_secs_f64(),
                                    tally.summary()
                                ),
                            );
                        }
                    }
                    None => {
                        let view = hub1.subscription().await;
                        println!(
                            "  失敗: {}秒以内にliveへ到達しませんでした(現在の状態={}, 理由={})",
                            LIVE_WAIT_TIMEOUT.as_secs(),
                            view.state,
                            view.reason.as_deref().unwrap_or("-")
                        );
                        step4_result = StepResult::fail(
                            "選択と購読",
                            format!("live到達タイムアウト(状態={})", view.state),
                        );
                    }
                }
            }
            Err(err) => {
                println!("  タグ選択の保存に失敗しました: {err}");
                step4_result =
                    StepResult::fail("選択と購読", format!("set_selected_tags失敗: {err}"));
            }
        }
    }
    results.push(step4_result);
    println!();

    // == 5. 再起動の模擬 ======================================================
    println!("== 5. 再起動の模擬 ==");
    // 手順6(保持観測)で使うため、再構築した HubService と DB プールは
    // ここでは drop せずに持ち越す（live に届かなかった／再構築自体に
    // 失敗した場合は `None` のままで、手順6は素直にスキップする）。
    let mut hub2_for_hold: Option<HubService> = None;
    let mut pool2_for_hold = None;
    if !reached_live {
        println!("  スキップ: 手順4でliveに到達していないため、再起動後の復帰を確認できません。");
        results.push(StepResult::skipped(
            "再起動の模擬",
            "手順4がliveに到達しなかったためスキップ",
        ));
    } else {
        let secret_before = keyring_account_1
            .as_deref()
            .and_then(|account| keys.get(account).ok().flatten());

        // 「再起動」を模擬: 旧 HubService・旧 SettingsService・旧プールを
        // すべて手放し、同じ DB ファイルで一から作り直す。`keys`
        // （`MemoryKeyStore`）は**同じインスタンスをそのまま**渡す -
        // ファイル越しの永続化ではなく、この1インスタンスを使い回すことが
        // 「同一プロセス内の再起動なら再発行されない」ことの証明そのもの
        // （上のモジュールdoc「KeyStoreはプロセス内メモリだけ」参照）。
        // TagClientHandle は明示 shutdown せずに drop する - `Drop for
        // TagClientHandle` がワーカータスクを止める設計
        // （banto-tagclient handle.rs）なので、ここでも実際のプロセス終了と
        // 同じ経路を通る。
        drop(hub1);
        drop(settings1);
        pool1.close().await;

        match init_db(&db_path).await {
            Err(err) => {
                println!("  再起動後のDB初期化に失敗しました: {err}");
                results.push(StepResult::fail(
                    "再起動の模擬",
                    format!("init_db失敗: {err}"),
                ));
                // 手順2で既にAPIキーを発行している可能性があるため、ここ
                // で exit(1) しない（2026-09-17 Copilotレビュー指摘）:
                // 早期終了すると手順6・7（保持観測・後片付けと残存キーの
                // 案内）を丸ごと飛ばし、発行済みキーの案内が一切出ない
                // まま終わってしまう。失敗を記録して最後まで進む。
            }
            Ok(pool2) => {
                let settings2 = SettingsService::new(pool2.clone());
                match HubService::new(settings2.clone(), keys.clone()).await {
                    Ok(hub2) => {
                        // 指示どおり connect() は呼ばない。resume() だけで復帰する
                        // ことを確認する。
                        hub2.resume().await;
                        match wait_for_live(&hub2, LIVE_WAIT_TIMEOUT, &mut tracker).await {
                            Some(elapsed) => {
                                println!("  live再到達: {:.1}秒", elapsed.as_secs_f64());
                                // status() は「発行は絶対に行わない」(hub.rs のdoc)
                                // ので、ここで呼んでも新しいキーは発行されない。
                                let view = hub2.status().await;
                                let record_after = match read_hub_record(&settings2).await {
                                    Ok(record) => record,
                                    Err(err) => {
                                        // 読み取り失敗を「レコード無し」と
                                        // 混同しない（read_hub_recordのdoc
                                        // comment参照）。ここではNoneに
                                        // 倒しても安全側 - key_name_same /
                                        // account_same が意図どおりfalseに
                                        // なり、手順5は「一致しない」として
                                        // 正しくFAILする（黙って握り潰さない
                                        // よう、理由だけ印字する）。
                                        println!(
                                            "  警告: 再起動後の設定読み取りに失敗しました: {err}"
                                        );
                                        None
                                    }
                                };
                                let key_name_after =
                                    record_after.as_ref().and_then(|r| r.key_name.clone());
                                let account_after =
                                    record_after.as_ref().map(|r| r.keyring_account.clone());
                                let key_name_same = key_name_after == key_name_1;
                                let account_same = account_after == keyring_account_1;
                                let secret_after = account_after
                                    .as_deref()
                                    .and_then(|account| keys.get(account).ok().flatten());
                                let secret_same =
                                    secret_before.is_some() && secret_before == secret_after;
                                println!(
                                    "  key_name 再利用: {} (手順2={:?}, 再起動後={:?})",
                                    key_name_same, key_name_1, key_name_after
                                );
                                println!("  keyring account 再利用: {account_same}");
                                println!("  平文キーの内容も同一(値は印字しない): {}", secret_same);
                                let (tally, update_count, received_any_value) =
                                    observe_live(&hub2, watch_secs.min(10), &mut tracker).await;
                                println!("  再起動後の品質内訳: {}", tally.summary());
                                println!("  再起動後に値が更新された回数: {update_count}");
                                let received_values = received_any_value || update_count >= 1;
                                // status() は Hub への REST 接続状態（6状態）であって
                                // 購読の状態ではない - 一度 live になったあとで購読が
                                // 落ちていても status() は connected のままになりうる
                                // （2026-09-17 Copilotレビュー指摘、実際にこれで
                                // 壊れた購読を見逃した）。観測後に改めて
                                // subscription() を読み、購読自体が live のままで
                                // あることも PASS の条件にする。
                                let post_subscription = hub2.subscription().await;
                                let subscription_still_live = post_subscription.state == "live";
                                println!(
                                    "  観測後の購読状態: {} (live維持={subscription_still_live})",
                                    post_subscription.state
                                );
                                let status_ok =
                                    matches!(view, Ok(ref v) if v.status.is_connected());
                                let ok = key_name_same
                                    && account_same
                                    && secret_same
                                    && status_ok
                                    && subscription_still_live
                                    && received_values;
                                results.push(if ok {
                            StepResult::pass(
                                "再起動の模擬",
                                format!(
                                    "live再到達={:.1}s, key再利用=true, 観測後も購読live, 値受信=true",
                                    elapsed.as_secs_f64()
                                ),
                            )
                        } else {
                            StepResult::fail(
                                "再起動の模擬",
                                format!(
                                    "key_name一致={key_name_same}, account一致={account_same}, 平文一致={secret_same}, status={:?}, 観測後の購読状態={}(live維持={subscription_still_live}), 値受信={received_values}",
                                    view.as_ref().map(|v| v.status.as_str()),
                                    post_subscription.state
                                ),
                            )
                        });
                            }
                            None => {
                                let view = hub2.subscription().await;
                                println!(
                            "  失敗: {}秒以内にliveへ再到達しませんでした(現在の状態={}, 理由={})",
                            LIVE_WAIT_TIMEOUT.as_secs(),
                            view.state,
                            view.reason.as_deref().unwrap_or("-")
                        );
                                results.push(StepResult::fail(
                                    "再起動の模擬",
                                    format!("live再到達タイムアウト(状態={})", view.state),
                                ));
                            }
                        }
                        // live に届いたかどうかに関わらず、手順6(保持観測)で使える
                        // よう HubService と DB プールは生かしたまま持ち越す。
                        hub2_for_hold = Some(hub2);
                        pool2_for_hold = Some(pool2);
                    }
                    Err(err) => {
                        println!("  再起動後のHubService初期化に失敗しました: {err}");
                        results.push(StepResult::fail(
                            "再起動の模擬",
                            format!("HubService::new失敗: {err}"),
                        ));
                        // HubServiceを構築できなかったので手順6には持ち越さない。
                        // このプールはもう使わないので、他の分岐と同じくここで
                        // 明示的に閉じる。
                        pool2.close().await;
                    }
                }
            }
        }
    }
    println!();

    // == 6. 保持観測(HOLD) ===================================================
    // オーナー指示（2026-09-17追加）: 再起動直後だけでなく、Hub側の手動操作
    // （タグ削除・Hub停止・Hub再開）に対する追従を実機で観測したい。
    // CG_SMOKE_HOLD_SECS 秒のあいだ購読状態だけを見張り、変化を
    // StatusTracker に記録する（値の表・品質集計はしない - ノイズ削減）。
    println!("== 6. 保持観測(HOLD) ==");
    if hold_secs == 0 {
        println!("  スキップ: CG_SMOKE_HOLD_SECS=0 のため保持観測を行いません。");
        // 操作者が明示的に選んでこの任意フェーズを実行しなかった -
        // 既定実行（CG_SMOKE_HOLD_SECS未指定）がこれだけで終了コード1に
        // なっていた（2026-09-18 Copilotレビュー指摘、オーナーが実機で
        // 再現）。前段の失敗によるスキップとは区別し、成功として扱う。
        results.push(StepResult::skipped_optional(
            "保持観測(HOLD)",
            "CG_SMOKE_HOLD_SECS=0のためスキップ(任意フェーズ)",
        ));
    } else if let Some(hub2) = hub2_for_hold.as_ref() {
        println!(
            "  {hold_secs}秒間、購読状態の変化を観測します。この間にHub側を操作してください\
             （タグ削除・Hub停止・Hub再開など）。状態が変わった行にだけ * が付きます。"
        );
        tracker.begin_hold();
        hold_observe(hub2, hold_secs, &mut tracker).await;
        tracker.end_hold();
        println!(
            "  保持観測を終了しました（{}件の状態変化を記録）。",
            tracker.log.len()
        );
        results.push(StepResult::pass(
            "保持観測(HOLD)",
            format!("{hold_secs}秒観測、状態変化{}件", tracker.log.len()),
        ));
    } else {
        println!("  スキップ: 手順5でHubServiceを再構築できなかったため保持観測できません。");
        results.push(StepResult::skipped(
            "保持観測(HOLD)",
            "手順5失敗のためスキップ",
        ));
    }
    // 手順5・6で持ち越した HubService・DB プールをここで手放す。
    drop(hub2_for_hold);
    if let Some(pool2) = pool2_for_hold {
        pool2.close().await;
    }
    println!();

    // == 7. 後片付け ==========================================================
    println!("== 7. 後片付け ==");
    println!(
        "  一時ディレクトリは削除しません(失敗時の調査用。既定の挙動)。パス: {}",
        smoke_dir.display()
    );
    println!("  手動で削除する場合は上記パスを rm -rf (PowerShellならRemove-Item -Recurse -Force) してください。");

    // 発行したAPIキーの後始末（Copilotレビュー指摘、2026-09-17/18）:
    // MemoryKeyStoreは実行のたびに空から始まるので、手順2で毎回新しい
    // readキーがHub側に発行される。disconnectはHub側を失効させない契約
    // なので、案内無しでは孤児キーが溜まり続ける。
    //
    // 判定は手順2で計算済みの `key_outcome`（`KeyOutcome`）に完全に
    // 委ねる。**「発行されていない」とPASSで言い切ってよいのは、それを
    // 積極的に確認できたときだけ** - `KeyOutcome::Undetermined` は
    // 常に自動失効せずFAILにする、という不変条件を、ここでは
    // `match` の各腕で個別に「安全側かどうか」を判断するのではなく、
    // `KeyOutcome` という型そのものが保証している（構築できる場所は
    // `classify_key_outcome` の1箇所だけで、そこが常に安全側に倒れる
    // ように書いてある）。
    let (cleanup_detail, cleanup_ok) = match key_outcome {
        KeyOutcome::Undetermined(reason) => {
            println!("  発行済みキーの有無を判定できません: {reason}");
            println!("  自動失効は行いません(CG_SMOKE_REVOKE=1が指定されていても)。");
            let list_hint = format!(
                "curl -H \"X-Banto-Client: banto\" {}/api/api-keys",
                hub_url_display.trim_end_matches('/')
            );
            let revoke_hint = format!(
                "curl -X POST -H \"X-Banto-Client: banto\" {}/api/api-keys/<id>/revoke",
                hub_url_display.trim_end_matches('/')
            );
            match installation_id.as_deref() {
                Some(id) => {
                    println!(
                        "  手動で確認してください: 次のコマンドでキー一覧を取得し、name が \"chronogazer-{id}-\" で始まるキーが残っていれば、その id を使って個別に失効させてください:"
                    );
                }
                None => {
                    println!(
                        "  手動で確認してください（このインストールのinstallation_idも読めなかったため、キー名の接頭辞を絞り込めません。一覧全体を確認してください）:"
                    );
                }
            }
            println!("    {list_hint}");
            println!("    {revoke_hint}");
            (format!("判定不能のため自動失効せず: {reason}"), false)
        }
        KeyOutcome::ConfirmedAbsent => {
            println!("  発行済みAPIキーはありません(確認済み: 接続前後の設定を読めたうえで記録が無いことを確認しました)。");
            ("発行済みキー無し(確認済み)".to_owned(), true)
        }
        KeyOutcome::ConfirmedDifferentHub { key_id, endpoint } => match key_id {
            Some(key_id) => {
                let hint = revoke_curl_hint(&endpoint, key_id);
                println!(
                    "  設定に残っている記録は別のHub({endpoint})のものです(今回のHUB_URL={hub_url_display}とは異なる接続先)。"
                );
                println!(
                    "  別Hubのレコードなので、このハーネスはCG_SMOKE_REVOKE=1が指定されていても失効させません。"
                );
                println!("  片付けたい場合は、そのHub宛てに手動で失効させてください:");
                println!("    {hint}");
                (
                    format!(
                        "key_id={key_id}は別Hub({endpoint})のもののため失効せず。手動失効: {hint}"
                    ),
                    true,
                )
            }
            None => {
                println!(
                    "  設定に残っている記録は別のHub({endpoint})のものですが、key_idがありません(手動キー採用のため失効対象を特定できません)。"
                );
                (
                    format!("別Hub({endpoint})の記録だがkey_id不明のため失効対象を特定できず"),
                    true,
                )
            }
        },
        KeyOutcome::ConfirmedPresent {
            key_id,
            key_name,
            issued_this_run,
        } => {
            let key_name_display = key_name.as_deref().unwrap_or("-");
            if !issued_this_run {
                let hint = revoke_curl_hint(&hub_url_display, key_id);
                println!(
                    "  設定に残っているキー(id={key_id}, name={key_name_display})は今回のプロセスが発行したものではありません(以前からのキーをそのまま維持しています)。"
                );
                println!("  このハーネスはCG_SMOKE_REVOKE=1が指定されていても失効させません。");
                println!("  片付けたい場合は手動で失効させてください:");
                println!("    {hint}");
                (
                    format!(
                        "key_id={key_id}は今回発行したキーではないため失効せず。手動失効: {hint}"
                    ),
                    true,
                )
            } else {
                // ここに来るのは「今回のHUB_URLのもの」かつ「今回の
                // プロセスが発行した」ことを確認できたときだけ。契約は
                // 「毎回、残存キーと失効コマンドを出す」ので、自動失効を
                // 試みる**前**に案内を出してから失効し、結果を続けて表示
                // する（2026-09-17 Copilotレビュー指摘: 以前は自動失効に
                // 成功したときだけこの案内が出ず、契約が守れていなかった）。
                let hint = revoke_curl_hint(&hub_url_display, key_id);
                println!("  発行したAPIキー: id={key_id}, name={key_name_display}");
                println!(
                    "  このキーはHubに残ります(disconnectはHub側を失効させない契約)。不要なら失効させてください:"
                );
                println!("    {hint}");
                // この形（X-Banto-Clientのみ）で失効できるのは試運転
                // モード中だけ - ロックダウン済みのHubではHubの管理画面か
                // 管理者のBearerトークンが必要（このハーネスは資格情報を
                // 扱わないため、その入力は作らない）（2026-09-18
                // Copilotレビュー指摘）。
                println!(
                    "  (上記のcurlで失効できるのは試運転モード中のみです。ロックダウン済みのHubではHubの管理画面か管理者のBearerトークンで失効させてください)"
                );
                if auto_revoke {
                    println!("  CG_SMOKE_REVOKE=1: 上記のキーを失効させます...");
                    // 実際のリクエストは表示用にサニタイズした
                    // hub_url_displayではなく、生の(トリム済み)hub_url を
                    // 使う - サニタイズはuserinfo等を落とすためのもので、
                    // 落とした値をそのまま実リクエストに使うと接続先が
                    // 変わってしまう(2026-09-18 Copilotレビュー指摘)。
                    match revoke_api_key(&hub_url, key_id).await {
                        Ok(()) => {
                            println!(
                                "  失効しました。上記のキーはもう失効済みです(Hub側には残っていません)。"
                            );
                            (format!("key_id={key_id}を失効させた"), true)
                        }
                        Err(message) => {
                            println!("  失効に失敗しました: {message}");
                            println!("  引き続き上記のcurlコマンドで手動失効してください。");
                            // サマリが嘘をつかないよう、失効を頼まれたのに
                            // 失敗した場合はこの段階をFAILにする
                            // （2026-09-17 Copilotレビュー指摘: 以前はこの
                            // ケースでもPASSのままで、総合結果が成功に見えた）。
                            (
                                format!("key_id={key_id}の失効に失敗({message})。手動失効: {hint}"),
                                false,
                            )
                        }
                    }
                } else {
                    // 「同じCG_SMOKE_DIRでCG_SMOKE_REVOKE=1指定して再実行
                    // すれば自動失効できる」という以前の案内は実現しない
                    // （2026-09-18 Copilotレビュー指摘、オーナーが実機で
                    // 確認）: 再実行するとconnect()は今保存されている
                    // キーをそのまま使う（同じHub・同じ接続先なら
                    // key_idが変わらない）ため、KeyOutcomeは
                    // issued_this_run=falseの ConfirmedPresent になり、
                    // 「今回発行したものではない」として自動失効が
                    // スキップされる - 安全側の判定が働いた結果として
                    // 正しい挙動だが、案内が嘘になるので削った。
                    // 自動失効は「そのキーを発行したその実行の中」でしか
                    // 行われない（安全のため）ので、素直に印字済みの
                    // curlコマンドで手動失効するよう案内する。
                    println!(
                        "  (自動失効は、そのキーを発行したその実行の中でのみ行います(安全のため) - 再実行しても自動では失効しません。上記の curl コマンドで手動失効してください)"
                    );
                    (format!("key_id={key_id}はHubに残存。失効: {hint}"), true)
                }
            }
        }
    };
    results.push(if cleanup_ok {
        StepResult::pass(
            "後片付け",
            format!(
                "作業ディレクトリを保持: {}, {cleanup_detail}",
                smoke_dir.display()
            ),
        )
    } else {
        StepResult::fail(
            "後片付け",
            format!(
                "作業ディレクトリを保持: {}, {cleanup_detail}",
                smoke_dir.display()
            ),
        )
    });
    println!();

    print_transition_log(&tracker.log);
    println!();
    print_summary(&results);
    let all_pass = results.iter().all(|r| r.status.is_pass());
    if !all_pass {
        std::process::exit(1);
    }
}

/// 総合結果の文言を決める（`print_summary` から分離してテスト可能に
/// した、2026-09-18 Copilotレビュー指摘）。**「スキップがあるだけ」
/// （失敗は無い）と「失敗がある」を区別する** - 任意フェーズの
/// スキップ（[`StepStatus::SkippedOptional`]）しか無いのに「失敗・
/// 想定外」と同じ文言を出すと、既定実行が何も壊れていないのに壊れて
/// いるように読めてしまう（これが今回の実害そのもの）。
fn summary_verdict(statuses: &[StepStatus]) -> &'static str {
    let has_failure = statuses
        .iter()
        .any(|status| matches!(status, StepStatus::Fail | StepStatus::SkippedBlocked));
    let has_optional_skip = statuses.contains(&StepStatus::SkippedOptional);
    match (has_failure, has_optional_skip) {
        (true, _) => "一部項目が失敗、または前段の失敗により試せませんでした",
        (false, true) => "全項目成功（一部は任意フェーズのため未実施）",
        (false, false) => "全項目成功",
    }
}

fn print_summary(results: &[StepResult]) {
    println!("=== 検証結果サマリ ===");
    for result in results {
        println!(
            "[{}] {} - {}",
            result.status.mark(),
            result.name,
            result.detail
        );
    }
    let statuses: Vec<StepStatus> = results.iter().map(|result| result.status).collect();
    println!();
    println!("総合結果: {}", summary_verdict(&statuses));
}

#[cfg(test)]
mod tests {
    use super::*;

    // StepStatus/summary_verdict の回帰テスト（2026-09-18 Copilotレビュー
    // 指摘、オーナーが実機で再現: 既定実行 - CG_SMOKE_HOLD_SECS=0 で
    // 手順6がスキップされるだけ - が必ず終了コード1になっていた）。

    #[test]
    fn skipped_optional_counts_as_pass_but_skipped_blocked_does_not() {
        assert!(StepStatus::SkippedOptional.is_pass());
        assert!(!StepStatus::SkippedBlocked.is_pass());
        assert!(StepStatus::Pass.is_pass());
        assert!(!StepStatus::Fail.is_pass());
    }

    #[test]
    fn all_pass_when_only_an_optional_skip_is_present() {
        // 今回の本題そのもの: 既定実行（HOLDだけが任意スキップ）は
        // 全項目成功として終了コード0になること。
        let statuses = [
            StepStatus::Pass,
            StepStatus::Pass,
            StepStatus::Pass,
            StepStatus::Pass,
            StepStatus::Pass,
            StepStatus::SkippedOptional,
            StepStatus::Pass,
        ];
        assert!(statuses.iter().all(|status| status.is_pass()));
        assert_eq!(
            summary_verdict(&statuses),
            "全項目成功（一部は任意フェーズのため未実施）"
        );
    }

    #[test]
    fn not_all_pass_when_a_blocked_skip_is_present() {
        // 前段の失敗によるスキップは、従来どおり非成功のまま
        // （サマリが嘘をつかないことを壊さない）。
        let statuses = [
            StepStatus::Pass,
            StepStatus::Fail,
            StepStatus::SkippedBlocked,
            StepStatus::SkippedBlocked,
        ];
        assert!(!statuses.iter().all(|status| status.is_pass()));
        assert_eq!(
            summary_verdict(&statuses),
            "一部項目が失敗、または前段の失敗により試せませんでした"
        );
    }

    #[test]
    fn summary_verdict_is_plain_success_with_no_skips_at_all() {
        let statuses = [StepStatus::Pass, StepStatus::Pass];
        assert_eq!(summary_verdict(&statuses), "全項目成功");
    }

    #[test]
    fn step_result_skipped_defaults_to_blocked_not_optional() {
        // 「どちらに入るかを選び忘れたら失敗側に倒れる」ことの固定:
        // 汎用の StepResult::skipped は SkippedBlocked を返す。
        // SkippedOptional を得るには専用の skipped_optional を明示的に
        // 呼ぶ必要がある。
        assert_eq!(
            StepResult::skipped("x", "y").status,
            StepStatus::SkippedBlocked
        );
        assert_eq!(
            StepResult::skipped_optional("x", "y").status,
            StepStatus::SkippedOptional
        );
    }

    // same_hub() の正規化を固定する回帰テスト（2026-09-18 Copilotレビュー
    // 指摘: `cargo test -p chronogazer-core --example hub_real_smoke` で
    // 走る）。実機を必要としない - 正規化はローカルの文字列処理のみ。

    #[test]
    fn same_hub_absorbs_a_trailing_slash() {
        assert!(same_hub("http://127.0.0.1:8722", "http://127.0.0.1:8722/"));
    }

    #[test]
    fn same_hub_absorbs_an_explicit_default_port() {
        // crates/banto-hub-bootstrap/src/admin.rs:135-141 の
        // keyring_account のdoc comment「the port is always spelled out
        // (port_or_known_default), so http://host and http://host:80
        // are one Hub, not two」と同じ規律であることの固定。
        assert!(same_hub("http://host", "http://host:80"));
    }

    #[test]
    fn same_hub_rejects_a_different_port() {
        assert!(!same_hub("http://host:8722", "http://host:8723"));
    }

    #[test]
    fn same_hub_rejects_a_different_host() {
        assert!(!same_hub("http://host-a:8722", "http://host-b:8722"));
    }

    #[test]
    fn same_hub_rejects_a_different_path_prefix() {
        assert!(!same_hub("http://host/hub-a", "http://host/hub-b"));
    }

    #[test]
    fn same_hub_treats_malformed_input_as_different() {
        assert!(!same_hub("not a url", "http://host"));
        assert!(!same_hub("http://host", "not a url"));
    }

    // classify_key_outcome() の回帰テスト（2026-09-18 Copilotレビュー
    // 指摘: 「判定できないのに黙ってPASSへ倒れる」同型の穴が4回見つかった
    // ため、before/afterの実際の差分だけで判定する形に一本化した。この
    // 一本化そのものが、connect()自体がErrを返す経路（save_record()の
    // あとverify()相当が失敗するケース）でも取りこぼさないことの証明に
    // なる - classify_key_outcomeはconnect_viewを一切受け取らず、
    // beforeとafterのRecordだけを見るため。

    fn record(endpoint: &str, key_id: Option<i64>) -> HubRecord {
        HubRecord {
            endpoint: endpoint.to_owned(),
            key_id,
            ..HubRecord::default()
        }
    }

    #[test]
    fn classify_key_outcome_is_undetermined_when_the_pre_connect_read_failed() {
        // 判定材料が欠けたら安全側（自動失効しない・PASSにならない）。
        let before: Result<Option<HubRecord>, String> = Err("db unavailable".to_owned());
        let after: Result<Option<HubRecord>, String> =
            Ok(Some(record("http://host:8722", Some(99))));
        assert!(matches!(
            classify_key_outcome("http://host:8722", true, &before, &after),
            KeyOutcome::Undetermined(_)
        ));
    }

    #[test]
    fn classify_key_outcome_is_undetermined_when_the_post_connect_read_failed() {
        let before: Result<Option<HubRecord>, String> = Ok(None);
        let after: Result<Option<HubRecord>, String> = Err("db unavailable".to_owned());
        assert!(matches!(
            classify_key_outcome("http://host:8722", true, &before, &after),
            KeyOutcome::Undetermined(_)
        ));
    }

    #[test]
    fn classify_key_outcome_confirms_issued_even_when_connect_itself_returned_err() {
        // connect()はbootstrapperがキーを発行・保存した**後**にErrを
        // 返し得る。この関数はbefore/afterの実際の差分を見るので、
        // afterに変化が観測できていれば（＝レコードは書けている）
        // connect_returned_ok=falseでも正しくConfirmedPresent
        // （issued_this_run=true）になる - Undeterminedへ取り違えて
        // 「発行済みキーはありません」とPASSで言い切ってしまうことはない。
        let before: Result<Option<HubRecord>, String> = Ok(None);
        let after: Result<Option<HubRecord>, String> =
            Ok(Some(record("http://host:8722", Some(42))));
        let outcome = classify_key_outcome("http://host:8722", false, &before, &after);
        assert_eq!(
            outcome,
            KeyOutcome::ConfirmedPresent {
                key_id: 42,
                key_name: None,
                issued_this_run: true,
            }
        );
    }

    #[test]
    fn classify_key_outcome_is_undetermined_when_connect_erred_and_no_record_exists() {
        // 今回の本題（A、`:810`相当、最重要）: 「レコードが無い」は
        // 「キーを発行していない」の証明にならない。
        // HubService::connect()は、bootstrapperがキーを発行し設定の
        // インメモリ写しに保存した後、それを実ストレージへ書き戻す
        // flush()が失敗するとErrを返す（そのErrはbootstrapper.connect()
        // 自体の後にあるので、flush()が失敗する限りOkには決して到達
        // しない）。connect_returned_ok=falseで、かつafterにも変化が
        // 観測できない（レコードが無いまま）場合、「発行が起きたのか
        // どうかこの観測経路からは分からない」ので、ConfirmedAbsentと
        // 言い切らずUndeterminedに倒す。
        let before: Result<Option<HubRecord>, String> = Ok(None);
        let after: Result<Option<HubRecord>, String> = Ok(None);
        assert!(matches!(
            classify_key_outcome("http://host:8722", false, &before, &after),
            KeyOutcome::Undetermined(_)
        ));
    }

    #[test]
    fn classify_key_outcome_is_undetermined_when_connect_erred_and_the_record_is_unchanged() {
        // 今回の本題（A、2026-09-18 Copilotレビュー指摘2回目、`:854`
        // 相当）: 前回は「レコードが無い」経路だけ塞いだが、「レコードが
        // 変わっていない」経路が残っていた。置き換えのキーを発行した
        // あとflush()前に失敗すると、afterは(書き戻し前の)古いレコードの
        // ままなので、key_id・接続先とも変わらない。この場合も
        // 「発行していないことの証明」にはならないので、
        // ConfirmedPresent{issued_this_run: false}のようにPASSで言い切らず
        // Undeterminedに倒す。
        let before: Result<Option<HubRecord>, String> =
            Ok(Some(record("http://host:8722", Some(5))));
        let after: Result<Option<HubRecord>, String> =
            Ok(Some(record("http://host:8722", Some(5))));
        assert!(matches!(
            classify_key_outcome("http://host:8722", false, &before, &after),
            KeyOutcome::Undetermined(_)
        ));
    }

    #[test]
    fn classify_key_outcome_confirms_present_when_connect_erred_but_the_endpoint_changed() {
        // 「変わった」はkey_idだけでなく接続先(endpoint)の変化も含む
        // （3行規則の2番目の枝）。key_idの数値自体は同じでも接続先が
        // 変われば「積極的に変化した」と言えるので、Errでも
        // ConfirmedPresentにしてよい。
        let before: Result<Option<HubRecord>, String> =
            Ok(Some(record("http://host-a:8722", Some(5))));
        let after: Result<Option<HubRecord>, String> =
            Ok(Some(record("http://host-b:8722", Some(5))));
        let outcome = classify_key_outcome("http://host-b:8722", false, &before, &after);
        assert_eq!(
            outcome,
            KeyOutcome::ConfirmedPresent {
                key_id: 5,
                key_name: None,
                issued_this_run: true,
            }
        );
    }

    #[test]
    fn classify_key_outcome_confirms_absent_when_no_record_exists() {
        let before: Result<Option<HubRecord>, String> = Ok(None);
        let after: Result<Option<HubRecord>, String> = Ok(None);
        assert_eq!(
            classify_key_outcome("http://host:8722", true, &before, &after),
            KeyOutcome::ConfirmedAbsent
        );
    }

    #[test]
    fn classify_key_outcome_confirms_a_different_hub() {
        let before: Result<Option<HubRecord>, String> = Ok(None);
        let after: Result<Option<HubRecord>, String> =
            Ok(Some(record("http://host-a:8722", Some(5))));
        assert_eq!(
            classify_key_outcome("http://host-b:8722", true, &before, &after),
            KeyOutcome::ConfirmedDifferentHub {
                key_id: Some(5),
                endpoint: "http://host-a:8722".to_owned(),
            }
        );
    }

    #[test]
    fn classify_key_outcome_confirms_issued_this_run_when_the_key_id_changed_on_the_same_hub() {
        let before: Result<Option<HubRecord>, String> =
            Ok(Some(record("http://host:8722", Some(5))));
        let after: Result<Option<HubRecord>, String> =
            Ok(Some(record("http://host:8722", Some(6))));
        assert_eq!(
            classify_key_outcome("http://host:8722", true, &before, &after),
            KeyOutcome::ConfirmedPresent {
                key_id: 6,
                key_name: None,
                issued_this_run: true,
            }
        );
    }

    #[test]
    fn classify_key_outcome_confirms_not_issued_this_run_when_the_key_id_is_unchanged() {
        let before: Result<Option<HubRecord>, String> =
            Ok(Some(record("http://host:8722", Some(5))));
        let after: Result<Option<HubRecord>, String> =
            Ok(Some(record("http://host:8722", Some(5))));
        assert_eq!(
            classify_key_outcome("http://host:8722", true, &before, &after),
            KeyOutcome::ConfirmedPresent {
                key_id: 5,
                key_name: None,
                issued_this_run: false,
            }
        );
    }

    #[test]
    fn classify_key_outcome_treats_a_same_numbered_key_id_on_a_different_hub_as_issued() {
        // api_keys.id はHubごとの連番なので、別Hubに切り替えたとき旧
        // レコードのkey_idと新しいHubが今回発行したkey_idがたまたま
        // 同じ数値でも「今回発行した」と正しく判定できること。
        let before: Result<Option<HubRecord>, String> =
            Ok(Some(record("http://host-a:8722", Some(5))));
        let after: Result<Option<HubRecord>, String> =
            Ok(Some(record("http://host-b:8722", Some(5))));
        assert_eq!(
            classify_key_outcome("http://host-b:8722", true, &before, &after),
            KeyOutcome::ConfirmedPresent {
                key_id: 5,
                key_name: None,
                issued_this_run: true,
            }
        );
    }

    #[test]
    fn classify_key_outcome_treats_an_unchanged_key_id_with_default_port_spelling_as_not_issued() {
        // 既定ポートの表記ゆれ（http://host と http://host:80）を同じ
        // Hubとみなせないと、実際には同じ接続先なのに「別Hub」と誤判定
        // してissued_this_runがtrueになる。
        let before: Result<Option<HubRecord>, String> = Ok(Some(record("http://host", Some(5))));
        let after: Result<Option<HubRecord>, String> = Ok(Some(record("http://host:80", Some(5))));
        assert_eq!(
            classify_key_outcome("http://host:80", true, &before, &after),
            KeyOutcome::ConfirmedPresent {
                key_id: 5,
                key_name: None,
                issued_this_run: false,
            }
        );
    }

    // sanitize_hub_url_for_display() の回帰テスト（2026-09-18
    // Copilotレビュー指摘: userinfo・クエリは常に落とす）。

    #[test]
    fn sanitize_hub_url_for_display_drops_userinfo_and_query() {
        assert_eq!(
            sanitize_hub_url_for_display("http://user:hunter2@host:8722/hub?x=1"),
            "http://host:8722/hub"
        );
    }

    #[test]
    fn sanitize_hub_url_for_display_keeps_a_plain_url_as_is() {
        assert_eq!(
            sanitize_hub_url_for_display("http://127.0.0.1:8722"),
            "http://127.0.0.1:8722/"
        );
    }

    #[test]
    fn sanitize_hub_url_for_display_never_echoes_unparsable_input() {
        assert_eq!(
            sanitize_hub_url_for_display("not a url"),
            "(不正な形式のHUB_URL)"
        );
    }

    #[test]
    fn sanitize_hub_url_for_display_keeps_ipv6_brackets() {
        // 2026-09-18 Copilotレビュー指摘(2回目、`:881`相当): 「host_str()
        // はIPv6リテラルを角括弧なしで返すのではないか」という指摘だったが、
        // 本クレートが実際にピン留めしているurl 2.5.8では host_str() は
        // 角括弧つき("[::1]")で返すことを確認済み(角括弧を追加で付け足すと
        // "[[::1]]"になって二重に壊れる)。角括弧つきのまま往復すること、
        // IPv4・ホスト名は従来どおり(角括弧を付けない)であることを固定する。
        assert_eq!(
            sanitize_hub_url_for_display("http://[::1]:8722"),
            "http://[::1]:8722/"
        );
        assert_eq!(
            sanitize_hub_url_for_display("http://[::1]:8722/hub"),
            "http://[::1]:8722/hub"
        );
        assert_eq!(
            sanitize_hub_url_for_display("http://user:hunter2@[fe80::1]:8722/hub?x=1"),
            "http://[fe80::1]:8722/hub"
        );
        // IPv4・ホスト名は角括弧を付けない(従来どおり)。
        assert_eq!(
            sanitize_hub_url_for_display("http://127.0.0.1:8722"),
            "http://127.0.0.1:8722/"
        );
        assert_eq!(
            sanitize_hub_url_for_display("http://example.com:8722"),
            "http://example.com:8722/"
        );
    }

    // revoke_url() / classify_revoke_response() の回帰テスト（純粋な
    // ロジックだけ、ネットワーク不要）。

    #[test]
    fn revoke_url_builds_the_expected_path() {
        assert_eq!(
            revoke_url("http://127.0.0.1:8722", 42),
            "http://127.0.0.1:8722/api/api-keys/42/revoke"
        );
        assert_eq!(
            revoke_url("http://127.0.0.1:8722/", 42),
            "http://127.0.0.1:8722/api/api-keys/42/revoke"
        );
    }

    #[test]
    fn revoke_url_keeps_ipv6_brackets_intact() {
        // revoke_url()自体は文字列を再パースせず連結するだけなので、
        // 角括弧つきのIPv6ホストをそのまま渡せば壊れない。実際の失効
        // リクエストは(サニタイズ済みのhub_url_displayではなく)生の
        // hub_urlをrevoke_urlに渡すので、この経路が壊れないことを固定
        // する(2026-09-18 Copilotレビュー指摘)。
        assert_eq!(
            revoke_url("http://[::1]:8722", 42),
            "http://[::1]:8722/api/api-keys/42/revoke"
        );
    }

    #[test]
    fn classify_revoke_response_succeeds_on_2xx() {
        assert!(classify_revoke_response(reqwest::StatusCode::OK).is_ok());
        assert!(classify_revoke_response(reqwest::StatusCode::NO_CONTENT).is_ok());
    }

    #[test]
    fn classify_revoke_response_reports_lockdown_on_401_and_403() {
        for status in [
            reqwest::StatusCode::UNAUTHORIZED,
            reqwest::StatusCode::FORBIDDEN,
        ] {
            let message = classify_revoke_response(status).unwrap_err();
            assert!(message.contains("ロックダウン"), "got: {message}");
        }
    }

    #[test]
    fn classify_revoke_response_fails_without_claiming_lockdown_on_other_statuses() {
        let message =
            classify_revoke_response(reqwest::StatusCode::INTERNAL_SERVER_ERROR).unwrap_err();
        assert!(!message.contains("ロックダウン"));
    }

    // revoke_api_key_with_timeout() の実HTTP経路のテスト（2026-09-18
    // Copilotレビュー指摘: パス・ヘッダ・ステータス処理がこれまで
    // 未検証だった - ここが壊れると「失効したつもりで実は残っている」
    // になる）。ローカルのTCPモック
    // （`crates/banto-tagclient/src/rest.rs` のテストと同じ作法:
    // `std::net::TcpListener` で1リクエスト受けて応答を返す）を使う。

    fn revoke_mock_server(status: u16) -> (String, std::thread::JoinHandle<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 1024];
            loop {
                let count = stream.read(&mut buffer).unwrap();
                request.extend_from_slice(&buffer[..count]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let response =
                format!("HTTP/1.1 {status} Test\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            stream.write_all(response.as_bytes()).unwrap();
            String::from_utf8_lossy(&request).into_owned()
        });
        (address, handle)
    }

    #[tokio::test]
    async fn revoke_api_key_posts_the_expected_path_and_header_and_succeeds_on_2xx() {
        let (address, handle) = revoke_mock_server(200);
        let result = revoke_api_key_with_timeout(&address, 42, Duration::from_secs(2)).await;
        let request = handle.join().unwrap();
        assert!(result.is_ok(), "expected Ok, got: {result:?}");
        assert!(
            request.starts_with("POST /api/api-keys/42/revoke HTTP/1.1"),
            "unexpected request line in: {request}"
        );
        assert!(
            request
                .to_ascii_lowercase()
                .contains("x-banto-client: banto"),
            "missing X-Banto-Client header in: {request}"
        );
    }

    #[tokio::test]
    async fn revoke_api_key_reports_lockdown_on_401_and_403() {
        for status in [401, 403] {
            let (address, handle) = revoke_mock_server(status);
            let result = revoke_api_key_with_timeout(&address, 1, Duration::from_secs(2)).await;
            handle.join().unwrap();
            let message = result.unwrap_err();
            assert!(
                message.contains("ロックダウン"),
                "status={status}, got: {message}"
            );
        }
    }

    #[tokio::test]
    async fn revoke_api_key_fails_on_other_non_2xx_without_claiming_lockdown() {
        let (address, handle) = revoke_mock_server(500);
        let result = revoke_api_key_with_timeout(&address, 1, Duration::from_secs(2)).await;
        handle.join().unwrap();
        let message = result.unwrap_err();
        assert!(!message.contains("ロックダウン"), "got: {message}");
    }

    #[tokio::test]
    async fn revoke_api_key_times_out_when_the_server_never_responds() {
        // 固定sleepは使わない - 待つのはrevoke_api_key_with_timeoutに
        // 注入した短いタイムアウト（reqwest自身の機構）で、テスト側で
        // 「十分待ったはず」という固定時間を仮定しない。モックサーバー
        // は接続だけ受けて何も返さず、スレッドをparkして握ったままに
        // する（joinしない - テスト関数が戻ればプロセス終了時に片付く）。
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            if let Ok((_stream, _)) = listener.accept() {
                loop {
                    std::thread::park();
                }
            }
        });
        let start = std::time::Instant::now();
        let result = revoke_api_key_with_timeout(&address, 1, Duration::from_millis(200)).await;
        assert!(result.is_err(), "expected a timeout error, got: {result:?}");
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "revoke_api_key_with_timeout should be cut off by its own timeout, not hang; elapsed={:?}",
            start.elapsed()
        );
    }
}
