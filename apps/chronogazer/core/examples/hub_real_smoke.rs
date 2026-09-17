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
//! | `CG_SMOKE_DIR` | OS の一時ディレクトリ配下に自動生成 | 設定 DB とキー保管ファイルを置く場所 |
//! | `CG_SMOKE_TAGS` | 空＝catalog の全タグ | 選択するタグの external name をカンマ区切りで指定 |
//! | `CG_SMOKE_WATCH_SECS` | `20` | 値を観測し続ける秒数 |
//!
//! ## やらないこと
//!
//! * 製品コード（`src/`）の変更。
//! * OS キーリングへの書き込み（[`JsonFileKeyStore`] のドキュメント参照）。
//! * banto-hub 側の設定変更（接続・タグの作成は別途行う）。
//! * CI への追加 - **example なので `cargo test` では走らない**。実 Hub が
//!   要るためこの example を CI で自動実行することもしない。

use std::collections::BTreeMap;
use std::env;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use banto_hub_bootstrap::error::{Error as BootstrapError, ErrorKind as BootstrapErrorKind};
use banto_hub_bootstrap::{HubRecord, KeyStore};
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

/// `chronogazer_core::hub` の設定 KV キー（`hub.record`）をここでも読む理由:
/// [`chronogazer_core::hub::HubView`] は画面が必要としない
/// `keyring_account` を持っていないが、本ハーネスの手順2・手順5は「同じ
/// keyring account / 同じ key_name が再利用されたか」を確かめる必要がある。
/// 定数はそちらの crate 内では private なので、ここでは同じ文字列リテラルを
/// **読み取り専用**でミラーする（書き込みは一切しない。値の形
/// （[`HubRecord`] の JSON）は `banto-hub-bootstrap` の公開型そのもの）。
const HUB_RECORD_SETTINGS_KEY: &str = "hub.record";

/// #383 段階1 実機確認専用の [`KeyStore`]。JSON ファイル 1 枚に
/// `account -> 平文キー` を保存する素朴な実装。
///
/// **これは確認用の平文保管であり、製品コードの経路ではない**:
/// * デスクトップ版（`src-tauri`）は OS キーリングを使う。
/// * `banto-serve`（Tauri を使わない実行形態）は
///   `chronogazer_core::hub::UnavailableKeyStore` を使い、鍵を一切保存
///   できない（書き込みは常にエラーになる）。
///
/// この example であえて JSON ファイルに永続化するのは、手順5「再起動の
/// 模擬」で **プロセス内の状態をすべて作り直しても、同じ account に対して
/// 同じ平文キーが再利用される**（＝新規発行されない）ことを確かめる必要が
/// あるため。インメモリの `HashMap` では `HubService` を作り直した時点で
/// 消えてしまい、確かめたいこと自体が確かめられない。
///
/// ファイル自体は `CG_SMOKE_DIR` 配下（既定は OS の一時ディレクトリ）に置き、
/// OS キーリングには一切触らない。
struct JsonFileKeyStore {
    path: PathBuf,
    /// この example は基本的に単一の非同期フロー上で順に呼ぶが、
    /// `HubService::spawn_supervisor` の常駐タスクが背後で同時にキーを
    /// 読みに来ることがあるため、read-modify-write の競合で片方の書き込みが
    /// 消えないよう最小限の排他だけを掛ける。
    guard: StdMutex<()>,
}

impl JsonFileKeyStore {
    fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            guard: StdMutex::new(()),
        }
    }

    fn load(&self) -> BTreeMap<String, String> {
        std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_default()
    }

    fn store(&self, map: &BTreeMap<String, String>) -> banto_hub_bootstrap::error::Result<()> {
        let json = serde_json::to_string_pretty(map).map_err(|err| {
            BootstrapError::with_detail(BootstrapErrorKind::KeyStore, err.to_string())
        })?;
        std::fs::write(&self.path, json).map_err(|err| {
            BootstrapError::with_detail(BootstrapErrorKind::KeyStore, err.to_string())
        })
    }
}

impl KeyStore for JsonFileKeyStore {
    fn get(&self, account: &str) -> banto_hub_bootstrap::error::Result<Option<String>> {
        let _guard = self.guard.lock().expect("keystore mutex poisoned");
        Ok(self.load().get(account).cloned())
    }

    fn set(&self, account: &str, secret: &str) -> banto_hub_bootstrap::error::Result<()> {
        let _guard = self.guard.lock().expect("keystore mutex poisoned");
        let mut map = self.load();
        map.insert(account.to_owned(), secret.to_owned());
        self.store(&map)
    }

    fn delete(&self, account: &str) -> banto_hub_bootstrap::error::Result<()> {
        let _guard = self.guard.lock().expect("keystore mutex poisoned");
        let mut map = self.load();
        map.remove(account);
        self.store(&map)
    }
}

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

/// live 到達まで [`HubSubscriptionView::state`] を1秒ごとに読み、変わる
/// たびに印字する。到達したら `Some(経過時間)`、`timeout` 以内に到達しな
/// かったら `None`。
async fn wait_for_live(hub: &HubService, timeout: Duration) -> Option<Duration> {
    let start = Instant::now();
    let mut last_state: Option<&'static str> = None;
    loop {
        let view = hub.subscription().await;
        if last_state != Some(view.state) {
            println!(
                "  購読状態: {} (理由={})",
                view.state,
                view.reason.as_deref().unwrap_or("-")
            );
            last_state = Some(view.state);
        }
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
/// しつつ品質内訳と更新回数を集計する。
async fn observe_live(hub: &HubService, watch_secs: u64) -> (QualityTally, u64) {
    let start = Instant::now();
    let deadline = start + Duration::from_secs(watch_secs);
    let mut tally = QualityTally::default();
    let mut last_t: BTreeMap<String, i64> = BTreeMap::new();
    let mut update_count: u64 = 0;
    let mut last_table_print: Option<Instant> = None;
    while Instant::now() < deadline {
        let view: HubSubscriptionView = hub.subscription().await;
        for value in &view.values {
            tally.record(&value.q);
            match last_t.get(value.tag.as_str()) {
                Some(prev) if *prev != value.t => update_count += 1,
                None | Some(_) => {}
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
    (tally, update_count)
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

    let smoke_dir = match env::var("CG_SMOKE_DIR") {
        Ok(value) if !value.trim().is_empty() => PathBuf::from(value),
        _ => std::env::temp_dir().join(format!("chronogazer-hub-smoke-{}", unix_millis())),
    };
    if let Err(err) = std::fs::create_dir_all(&smoke_dir) {
        eprintln!("一時ディレクトリを作れませんでした: {smoke_dir:?}: {err}");
        std::process::exit(1);
    }
    let db_path = smoke_dir.join("chronogazer.sqlite3");
    let keystore_path = smoke_dir.join("keystore.json");

    println!("Hub URL: {hub_url}");
    println!("作業ディレクトリ: {}", smoke_dir.display());
    println!("観測秒数: {watch_secs}秒");
    println!();

    let mut results: Vec<StepResult> = Vec::new();

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
    let keys1 = Arc::new(JsonFileKeyStore::new(&keystore_path));
    let hub1 = match HubService::new(settings1.clone(), keys1).await {
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
    println!("  キー保管ファイル: {}", keystore_path.display());
    results.push(StepResult::pass(
        "準備",
        "DB・KeyStore・HubServiceを初期化した",
    ));
    println!();

    // == 2. 接続 ==============================================================
    println!("== 2. 接続 ==");
    let connect_view = hub1.connect(&hub_url).await;
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
            let record = read_hub_record(&settings1).await;
            let keyring_account = record.as_ref().map(|r| r.keyring_account.clone());
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
        println!("  スキップ: 選択できるタグがありません(catalogが0件、またはCG_SMOKE_TAGSが一致しません)。");
        step4_result = StepResult::skipped("選択と購読", "選択可能なタグが0件");
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
                match wait_for_live(&hub1, LIVE_WAIT_TIMEOUT).await {
                    Some(elapsed) => {
                        reached_live = true;
                        println!("  live到達: {:.1}秒", elapsed.as_secs_f64());
                        let (tally, update_count) = observe_live(&hub1, watch_secs).await;
                        println!("  品質内訳: {}", tally.summary());
                        println!("  値が更新された回数: {update_count}");
                        step4_result = StepResult::pass(
                            "選択と購読",
                            format!(
                                "live到達={:.1}s, 品質内訳=[{}], 更新回数={update_count}",
                                elapsed.as_secs_f64(),
                                tally.summary()
                            ),
                        );
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
    if !reached_live {
        println!("  スキップ: 手順4でliveに到達していないため、再起動後の復帰を確認できません。");
        results.push(StepResult::skipped(
            "再起動の模擬",
            "手順4がliveに到達しなかったためスキップ",
        ));
    } else {
        let secret_before = keyring_account_1
            .as_deref()
            .and_then(|account| keys1_peek(&keystore_path, account));

        // 「再起動」を模擬: 旧 HubService・旧 SettingsService・旧プールを
        // すべて手放し、同じ DB ファイル・同じキー保管ファイルで一から
        // 作り直す。TagClientHandle は明示 shutdown せずに drop する -
        // `Drop for TagClientHandle` がワーカータスクを止める設計
        // （banto-tagclient handle.rs）なので、ここでも実際のプロセス終了と
        // 同じ経路を通る。
        drop(hub1);
        drop(settings1);
        pool1.close().await;

        let pool2 = match init_db(&db_path).await {
            Ok(pool) => pool,
            Err(err) => {
                println!("  再起動後のDB初期化に失敗しました: {err}");
                results.push(StepResult::fail(
                    "再起動の模擬",
                    format!("init_db失敗: {err}"),
                ));
                print_summary(&results);
                std::process::exit(1);
            }
        };
        let settings2 = SettingsService::new(pool2.clone());
        let keys2 = Arc::new(JsonFileKeyStore::new(&keystore_path));
        match HubService::new(settings2.clone(), keys2).await {
            Ok(hub2) => {
                // 指示どおり connect() は呼ばない。resume() だけで復帰する
                // ことを確認する。
                hub2.resume().await;
                match wait_for_live(&hub2, LIVE_WAIT_TIMEOUT).await {
                    Some(elapsed) => {
                        println!("  live再到達: {:.1}秒", elapsed.as_secs_f64());
                        // status() は「発行は絶対に行わない」(hub.rs のdoc)
                        // ので、ここで呼んでも新しいキーは発行されない。
                        let view = hub2.status().await;
                        let record_after = read_hub_record(&settings2).await;
                        let key_name_after = record_after.as_ref().and_then(|r| r.key_name.clone());
                        let account_after =
                            record_after.as_ref().map(|r| r.keyring_account.clone());
                        let key_name_same = key_name_after == key_name_1;
                        let account_same = account_after == keyring_account_1;
                        let secret_after = account_after
                            .as_deref()
                            .and_then(|account| keys1_peek(&keystore_path, account));
                        let secret_same = secret_before.is_some() && secret_before == secret_after;
                        println!(
                            "  key_name 再利用: {} (手順2={:?}, 再起動後={:?})",
                            key_name_same, key_name_1, key_name_after
                        );
                        println!("  keyring account 再利用: {account_same}");
                        println!("  平文キーの内容も同一(値は印字しない): {}", secret_same);
                        let (tally, update_count) = observe_live(&hub2, watch_secs.min(10)).await;
                        println!("  再起動後の品質内訳: {}", tally.summary());
                        println!("  再起動後に値が更新された回数: {update_count}");
                        let status_ok = matches!(view, Ok(ref v) if v.status.is_connected());
                        let ok = key_name_same && account_same && secret_same && status_ok;
                        results.push(if ok {
                            StepResult::pass(
                                "再起動の模擬",
                                format!(
                                    "live再到達={:.1}s, key再利用=true",
                                    elapsed.as_secs_f64()
                                ),
                            )
                        } else {
                            StepResult::fail(
                                "再起動の模擬",
                                format!(
                                    "key_name一致={key_name_same}, account一致={account_same}, 平文一致={secret_same}, status={:?}",
                                    view.as_ref().map(|v| v.status.as_str())
                                ),
                            )
                        });
                        drop(hub2);
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
                        drop(hub2);
                    }
                }
            }
            Err(err) => {
                println!("  再起動後のHubService初期化に失敗しました: {err}");
                results.push(StepResult::fail(
                    "再起動の模擬",
                    format!("HubService::new失敗: {err}"),
                ));
            }
        }
        pool2.close().await;
    }
    println!();

    // == 6. 後片付け ==========================================================
    println!("== 6. 後片付け ==");
    println!(
        "  一時ディレクトリは削除しません(失敗時の調査用。既定の挙動)。パス: {}",
        smoke_dir.display()
    );
    println!("  手動で削除する場合は上記パスを rm -rf (PowerShellならRemove-Item -Recurse -Force) してください。");
    results.push(StepResult::pass(
        "後片付け",
        format!("作業ディレクトリを保持: {}", smoke_dir.display()),
    ));
    println!();

    print_summary(&results);
    let all_pass = results.iter().all(|r| r.status.is_pass());
    if !all_pass {
        std::process::exit(1);
    }
}

/// 平文キーの値そのものは決して印字しない。手順5の同一性確認だけに使う
/// 内部ヘルパー。`JsonFileKeyStore` の `get` を直接呼べないテスト外の文脈
/// (drop 済みの旧インスタンス)からも呼べるよう、ファイルを都度開き直す。
fn keys1_peek(keystore_path: &std::path::Path, account: &str) -> Option<String> {
    let store = JsonFileKeyStore::new(keystore_path.to_path_buf());
    store.get(account).ok().flatten()
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
