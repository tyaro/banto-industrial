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
//! 対象に**（名前で他のキーを探さない）失効させる。失効に失敗しても
//! 致命扱いにせず、同じ案内を出して終わる。
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

/// 1段階の検証結果の状態。「実際に試して失敗した」（`Fail`）と「前段の失敗で
/// 試せなかった」（`Skipped`）は読み手にとって全く違う情報なので区別する
/// （`real_hub_smoke.rs` と同じ設計）。どちらも「検証できていない」という点
/// では非成功として終了コードに反映する。
#[derive(Clone, Copy, PartialEq, Eq)]
enum StepStatus {
    Pass,
    Fail,
    Skipped,
}

impl StepStatus {
    fn mark(self) -> &'static str {
        match self {
            StepStatus::Pass => "OK",
            StepStatus::Fail => "NG",
            StepStatus::Skipped => "SKIP",
        }
    }

    fn is_pass(self) -> bool {
        matches!(self, StepStatus::Pass)
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

    fn skipped(name: &'static str, detail: impl Into<String>) -> Self {
        Self::new(name, StepStatus::Skipped, detail)
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
async fn read_hub_record(settings: &SettingsService) -> Option<HubRecord> {
    let raw = settings.get(HUB_RECORD_SETTINGS_KEY).await.ok()??;
    if raw.trim().is_empty() {
        return None;
    }
    serde_json::from_str(&raw).ok()
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
/// 正規化そのものは `banto_hub_bootstrap::admin::base_url` が行っている
/// が `pub(crate)` でこの crate の外から呼べないため、**同じ検証・正規化
/// ルールで実装されている公開 API** `banto_tagclient::Endpoint::new` で
/// 代用する（`admin.rs` のdoc「Mirrors `banto_tagclient::Endpoint`'s
/// contract」のとおり、スキーム・ホスト・ポート・末尾スラッシュの扱いは
/// 完全に同じ）。`Endpoint` は正規化後の文字列を直接公開していないため、
/// 決定的に導ける `tags_url()` を比較する - `chronogazer_core::hub` の
/// `fingerprint_endpoint`（購読世代の同一性判定）が同じ理由で同じ手法を
/// 使っているのと同じ考え方。どちらかが不正な形式なら「同じではない」
/// として扱う（安全側 - 判断に迷ったら失効させない）。
fn same_hub(a: &str, b: &str) -> bool {
    match (Endpoint::new(a), Endpoint::new(b)) {
        (Ok(a), Ok(b)) => a.tags_url() == b.tags_url(),
        _ => false,
    }
}

/// `curl` で手動失効するときの1行を組み立てる（実行はしない）。手順7の
/// 案内表示・失敗時のフォールバックの両方で使う。
fn revoke_curl_hint(hub_url: &str, key_id: i64) -> String {
    format!(
        "curl -X POST -H \"X-Banto-Client: banto\" {}/api/api-keys/{key_id}/revoke",
        hub_url.trim_end_matches('/')
    )
}

/// `POST /api/api-keys/{id}/revoke` を叩く（ベストエフォート、
/// `CG_SMOKE_REVOKE=1` のときだけ手順7から呼ぶ）。
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
async fn revoke_api_key(hub_url: &str, key_id: i64) -> Result<(), String> {
    let http = reqwest::ClientBuilder::new()
        .no_proxy()
        .timeout(REVOKE_TIMEOUT)
        // 製品の管理クライアント（banto-hub-bootstrap の AdminClient、
        // banto-tagclient の RestClient::new）と同じ規律: リダイレクトは
        // 追従しない。失効は資格情報を伴う管理操作なので、Hubが3xxを
        // 返しても想定外のホストへ飛ばしてはいけない
        // （2026-09-18 Copilotレビュー指摘）。
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| format!("reqwestクライアント構築失敗: {error}"))?;
    let base = hub_url.trim_end_matches('/');
    let url = format!("{base}/api/api-keys/{key_id}/revoke");
    let response = http
        .post(url)
        .header("X-Banto-Client", "banto")
        .send()
        .await
        .map_err(|error| format!("送信失敗: {error}"))?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!("HTTPステータス={}", response.status()))
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    println!("=== chronogazer Hub 実機確認ハーネス（#383 段階1）===");
    let hub_url = env_var("HUB_URL", DEFAULT_HUB_URL);
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

    println!("Hub URL: {hub_url}");
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
    results.push(StepResult::pass(
        "準備",
        "DB・KeyStore(インメモリ)・HubServiceを初期化した",
    ));
    println!();

    // == 2. 接続 ==============================================================
    println!("== 2. 接続 ==");
    // connect() の**前**に、同じ CG_SMOKE_DIR に残っていたかもしれない
    // 古いレコードの key_id を控えておく（2026-09-17 Copilotレビュー
    // 指摘）: connect() は NeedsPairing / Unreachable でも `Ok(HubView)`
    // を返すため、接続が実際には成功していなくても、設定DBに残っていた
    // 古い hub.record（このプロセスではなく過去の実行が発行したキー）を
    // 「今回発行したキー」と誤認しうる。connect後のkey_idがこの値と違う
    // （かつ状態がconnected）ときだけ、実際に今回発行されたと判断する。
    let key_id_before_connect = read_hub_record(&settings1).await.and_then(|r| r.key_id);
    let connect_view = hub1.connect(&hub_url).await;
    let (
        step2_ok,
        catalog_tags,
        key_name_1,
        keyring_account_1,
        key_id_1,
        record_endpoint_1,
        issued_this_run_1,
    ) = match connect_view {
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
            let record = read_hub_record(&settings1).await;
            let keyring_account = record.as_ref().map(|r| r.keyring_account.clone());
            // 手順7の後始末（キー失効の案内・CG_SMOKE_REVOKE）に使う
            // key_id。自己発行なら必ず Some、手動キー採用ならこの
            // ハーネスは使わないので気にしなくてよい（HubRecordのdoc）。
            let key_id = record.as_ref().and_then(|r| r.key_id);
            // レコードが「今回のHUB_URL」のものかどうかを手順7で確かめる
            // ために、endpointも一緒に持ち出す（#382と同じ間違いをしない
            // - same_hub のdoc comment参照）。connect()が失敗した場合、
            // read_hub_record は同じ CG_SMOKE_DIR に残っていた**別の
            // 実行・別のHub**のレコードを返しうる（失敗したconnectは
            // レコードを書き換えない）ため、ここで捕まえておかないと
            // 手順7がそれを「今回のキー」と誤認する。
            let record_endpoint = record.as_ref().map(|r| r.endpoint.clone());
            println!(
                "  keyring account: {}",
                keyring_account.as_deref().unwrap_or("-")
            );
            let ok = view.status.is_connected();
            // 「今回発行した」= 接続がconnectedになり、かつkey_idが
            // connect前と変わった。接続に失敗していれば当然違うし、
            // 仮にconnectedでも（このハーネスの MemoryKeyStore は毎回
            // 空なので起こらないはずだが）既存キーをそのまま再利用して
            // key_idが変わらないケースも「今回発行した」扱いにしない
            // - 発行元を問わず「変わったときだけ」で判定する方が、
            // Bootstrapperの内部実装に依存せず正しい。
            let issued_this_run = ok && key_id != key_id_before_connect;
            (
                ok,
                view.tags,
                view.key_name,
                keyring_account,
                key_id,
                record_endpoint,
                issued_this_run,
            )
        }
        Err(err) => {
            println!("  接続に失敗しました: {err}");
            (false, None, None, None, None, None, false)
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
                                let record_after = read_hub_record(&settings2).await;
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
        results.push(StepResult::skipped(
            "保持観測(HOLD)",
            "CG_SMOKE_HOLD_SECS=0のためスキップ",
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

    // 発行したAPIキーの後始末（Copilotレビュー指摘、2026-09-17）:
    // MemoryKeyStoreは実行のたびに空から始まるので、手順2で毎回新しい
    // readキーがHub側に発行される。disconnectはHub側を失効させない契約
    // なので、案内無しでは孤児キーが溜まり続ける。
    //
    // 失効の対象にするのは、次の**2条件がそろったとき**だけ:
    // 1. レコードが今回のHUB_URLのものか（same_hub） - #382と同じ間違い
    //    をしない。同じCG_SMOKE_DIRを別のHubに向けて使い、今回のconnect()
    //    が失敗した場合、read_hub_recordは古い（別のHubの）レコードを
    //    返しうる。key_idはHubごとの連番なので、確かめずに使うと無関係な
    //    第三者のキーを失効させかねない。
    // 2. そのキーを**今回のプロセスが発行したか**（issued_this_run_1）
    //    - connect()はNeedsPairing/Unreachableでも`Ok(HubView)`を返す
    //    ため、接続が実際には成功していなくても設定DBに残っていた古い
    //    レコード（過去の実行が発行したキー）を読んでしまう。ロック
    //    ダウン済みHubに同じCG_SMOKE_DIRを再利用すると、このプロセスが
    //    発行していない永続キーを失効させかねない
    //    （2026-09-17 Copilotレビュー指摘）。
    let (cleanup_detail, cleanup_ok) = match key_id_1 {
        Some(key_id) => {
            let key_name_display = key_name_1.as_deref().unwrap_or("-");
            let record_endpoint_display = record_endpoint_1.as_deref().unwrap_or("-");
            let matches_current_hub = record_endpoint_1
                .as_deref()
                .is_some_and(|endpoint| same_hub(endpoint, &hub_url));
            if !matches_current_hub {
                let hint = revoke_curl_hint(record_endpoint_display, key_id);
                println!(
                    "  設定に残っている記録は別のHub({record_endpoint_display})のものです(今回のHUB_URL={hub_url}とは異なる接続先)。"
                );
                println!(
                    "  別Hubのレコードなので、このハーネスはCG_SMOKE_REVOKE=1が指定されていても失効させません。"
                );
                println!("  片付けたい場合は、そのHub宛てに手動で失効させてください:");
                println!("    {hint}");
                (
                    format!(
                        "key_id={key_id}は別Hub({record_endpoint_display})のもののため失効せず。手動失効: {hint}"
                    ),
                    true,
                )
            } else if !issued_this_run_1 {
                let hint = revoke_curl_hint(&hub_url, key_id);
                println!(
                    "  設定に残っているキー(id={key_id}, name={key_name_display})は今回のプロセスが発行したものではありません(過去の実行の記録が残っているか、接続に失敗しています)。"
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
                // プロセスが発行した」ときだけ。契約は「毎回、残存キーと
                // 失効コマンドを出す」ので、自動失効を試みる**前**に案内を
                // 出してから失効し、結果を続けて表示する
                // （2026-09-17 Copilotレビュー指摘: 以前は自動失効に
                // 成功したときだけこの案内が出ず、契約が守れていなかった）。
                let hint = revoke_curl_hint(&hub_url, key_id);
                println!("  発行したAPIキー: id={key_id}, name={key_name_display}");
                println!(
                    "  このキーはHubに残ります(disconnectはHub側を失効させない契約)。不要なら失効させてください:"
                );
                println!("    {hint}");
                if auto_revoke {
                    println!("  CG_SMOKE_REVOKE=1: 上記のキーを失効させます...");
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
                    // 「CG_SMOKE_REVOKE=1で再実行」とだけ書くと、
                    // CG_SMOKE_DIR を指定せずに再実行して別のキーを新規
                    // 発行しただけになり、今回のキーは孤児のままになる
                    // （2026-09-17 Copilotレビュー指摘）。同じ
                    // CG_SMOKE_DIR を使うよう明示する。
                    println!(
                        "  (自動で失効させたい場合は、同じ CG_SMOKE_DIR を指定して CG_SMOKE_REVOKE=1 で再実行してください: CG_SMOKE_DIR={} CG_SMOKE_REVOKE=1。またはこの curl コマンドで直接失効させてください)",
                        smoke_dir.display()
                    );
                    (format!("key_id={key_id}はHubに残存。失効: {hint}"), true)
                }
            }
        }
        None => {
            println!("  発行済みAPIキーはありません(手順2で接続できなかった、またはロックダウン済みHub)。");
            ("発行済みキー無し".to_owned(), true)
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
    let all_pass = results.iter().all(|r| r.status.is_pass());
    println!();
    println!(
        "総合結果: {}",
        if all_pass {
            "全項目成功"
        } else {
            "一部項目が失敗・想定外、またはスキップ"
        }
    );
}
