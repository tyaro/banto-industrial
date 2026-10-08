//! 保持期間（`retention.days`）で古い時系列データファイルを消す（#538）。
//!
//! 以前は設定値（[`crate::settings::StoreSettings::retention_days`]）を持つ
//! だけで、どこも削除を行わなかった（`collect` のモジュール doc に「別途実装
//! する」とあった）ため、データファイルは増え続けていつかディスクを埋めた。
//! ここがその実装で、`banto-hub` の `runtime::prune_once` と同じ考え方に揃える。
//!
//! # いつ消すか
//!
//! * **起動時に 1 回**、その後は**ローカル日付が変わったとき 1 回**。判定は
//!   [`needs_sweep`]（純関数）。時計は [`CHECK_INTERVAL`] ごとに見るだけで、
//!   24 時間タイマーにはしない（夏時間や時計の補正で「日付が変わった」と
//!   「24 時間たった」がずれても、ファイル名の日付基準の削除と食い違わない）。
//! * 保持日数は**掃除のたびに設定から読み直す**ので、設定を変えたら次の掃除
//!   （次の日付の変わり目、または再起動）から効く。変更の保存では消さない
//!   （保存で不可逆な削除を起こさない。banto-hub と同じ約束）。
//! * `data.dir` は**収集が使っているディレクトリ**（起動時に解決した値）を
//!   渡される。実行中に設定の `data.dir` を変えても、収集と同じく再起動まで
//!   反映されない。
//!
//! # 何を消さないか
//!
//! * 消す判定は [`banto_tstore::prune_files`]。ファイル名の日付だけで決め、
//!   **今日の日付のファイルは `retention.days` に関わらず残す**。書き込み中の
//!   ファイルは今日のファイル（日付の変わり目の直後、書き込み側がまだ
//!   切り替えていない前日のファイルも、保持日数は 1 以上なので残る）。
//! * 保持日数が無制限（`None`）・0 以下・`u32` に収まらないときは**削除側では
//!   なく保持側に倒す**（掃除そのものを飛ばす。[`plan_sweep`]）。
//! * データファイルの名前の形（`YYYYMMDD-NNN.sqlite3`）でないものには触らない。
//!
//! # 失敗しても収集を止めない
//!
//! 掃除の失敗（設定を読めない、削除に失敗した）はログに出すだけで、収集にも
//! 次の掃除にも影響しない。削除は同期の `std::fs` なので `spawn_blocking` で
//! 回し、ランタイムを塞がない。
//!
//! 結果は標準エラー出力のログに残す（banto-hub も監査やイベントには残さず
//! ログだけ。ChronoGazer の `collect_events` は収集の出来事用なので使わない）。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use banto_tstore::{Clock, LocalDate, SystemClock};
use tokio::task::JoinHandle;

use crate::settings::{store_config, SettingsService};

/// 日付が変わったかを見る間隔。掃除そのものは日に 1 回だが、見るのは
/// これより細かく（変わり目から最長でこの間隔だけ遅れる）。
pub const CHECK_INTERVAL: Duration = Duration::from_secs(60);

/// 保持日数の設定から、この掃除でやることを決める。**判断はここだけ**
/// （純関数。状態の総当たりを表でテストする）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepPlan {
    /// 無制限（利用者が「消さない」を選んだ）。
    Unlimited,
    /// 想定外の値（0 以下、または `u32` に収まらない）。DB を直接編集された
    /// 等でしか入らない。**消さずに**ログに出す。
    Invalid(i64),
    /// この日数より古いファイルを消す（1 以上）。
    Prune(u32),
}

/// 設定値（[`crate::settings::StoreSettings::retention_days`]）→ [`SweepPlan`]。
pub fn plan_sweep(retention_days: Option<i64>) -> SweepPlan {
    match retention_days {
        None => SweepPlan::Unlimited,
        Some(days) if days <= 0 => SweepPlan::Invalid(days),
        Some(days) => match u32::try_from(days) {
            Ok(days) => SweepPlan::Prune(days),
            Err(_) => SweepPlan::Invalid(days),
        },
    }
}

/// 掃除を始めるか。前回掃除した日付が**今日と違う**（まだ掃除していない、
/// または日付が変わった）とき。時計が戻って日付が前に動いたときも「違う」
/// ので掃除する（空振りで害は無い）。
pub fn needs_sweep(last_swept: Option<LocalDate>, today: LocalDate) -> bool {
    last_swept != Some(today)
}

/// 1 回の掃除の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SweepOutcome {
    /// 無制限なので何もしなかった。
    SkippedUnlimited,
    /// 保持日数が想定外の値なので何もしなかった。
    SkippedInvalid(i64),
    /// 削除した（0 件も含む）。
    Pruned { days: u32, deleted: usize },
    /// 削除に失敗した（一部は消えているかもしれない）。
    Failed(String),
}

/// 掃除を 1 回行う。保持日数は `settings` から**今**読む（読めなければ
/// banto-hub と同じく既定値で続ける）。`today` は `clock` のローカル日付。
pub async fn sweep_once(
    settings: &SettingsService,
    data_dir: &Path,
    clock: &dyn Clock,
) -> SweepOutcome {
    let retention_days = match store_config(settings).await {
        Ok(config) => config.retention_days,
        Err(err) => {
            eprintln!("banto: 保持設定の読み取りに失敗しました（既定値で続行します）: {err}");
            crate::settings::StoreSettings::default().retention_days
        }
    };
    let today = LocalDate::from_epoch_ms(clock.now_ms(), clock.utc_offset_ms());
    let outcome = match plan_sweep(retention_days) {
        SweepPlan::Unlimited => SweepOutcome::SkippedUnlimited,
        SweepPlan::Invalid(days) => SweepOutcome::SkippedInvalid(days),
        SweepPlan::Prune(days) => {
            let dir = data_dir.to_path_buf();
            let joined =
                tokio::task::spawn_blocking(move || banto_tstore::prune_files(&dir, days, today))
                    .await;
            match joined {
                Ok(Ok(report)) => SweepOutcome::Pruned {
                    days,
                    deleted: report.deleted.len(),
                },
                Ok(Err(err)) => SweepOutcome::Failed(err.to_string()),
                Err(err) => SweepOutcome::Failed(format!("削除の処理が異常終了しました: {err}")),
            }
        }
    };
    log_outcome(&outcome);
    outcome
}

fn log_outcome(outcome: &SweepOutcome) {
    match outcome {
        SweepOutcome::SkippedUnlimited => {
            eprintln!("banto: 時系列データの保持期間は無制限のため、古いファイルの削除を飛ばしました")
        }
        SweepOutcome::SkippedInvalid(days) => eprintln!(
            "banto: 時系列データの保持日数の値が想定外のため、古いファイルの削除を飛ばしました: {days}"
        ),
        SweepOutcome::Pruned { days, deleted } if *deleted > 0 => eprintln!(
            "banto: 時系列データの保持期間（{days} 日）を過ぎたファイルを {deleted} 件削除しました"
        ),
        SweepOutcome::Pruned { .. } => {}
        SweepOutcome::Failed(err) => {
            eprintln!("banto: 時系列データの古いファイルの削除に失敗しました: {err}")
        }
    }
}

/// 起動時 + 日付が変わるたびに掃除し続ける（終わらない）。呼び出し側が
/// spawn する（`src-tauri` は新規依存を持たない規約なので、`tokio::spawn`
/// ではなく future を返す形にして、ランタイムは呼ぶ側が選ぶ）。
pub async fn run(settings: SettingsService, data_dir: PathBuf) {
    run_with(settings, data_dir, Arc::new(SystemClock), CHECK_INTERVAL).await
}

/// [`run`] の時計・間隔を差し替えられる版（テスト用に公開）。
pub async fn run_with(
    settings: SettingsService,
    data_dir: PathBuf,
    clock: Arc<dyn Clock>,
    interval: Duration,
) {
    let mut last_swept: Option<LocalDate> = None;
    loop {
        let today = LocalDate::from_epoch_ms(clock.now_ms(), clock.utc_offset_ms());
        if needs_sweep(last_swept, today) {
            // 失敗しても同じ日に掃除し直さない（毎分のログ溢れを避ける）。
            // 次の日付の変わり目か再起動で、もう一度試す。
            sweep_once(&settings, &data_dir, clock.as_ref()).await;
            last_swept = Some(today);
        }
        tokio::time::sleep(interval).await;
    }
}

/// [`run`] を現在のランタイムに spawn する（`banto-serve` 用）。
pub fn spawn(settings: SettingsService, data_dir: PathBuf) -> JoinHandle<()> {
    tokio::spawn(run(settings, data_dir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{migrate_memory, Db};
    use crate::settings::{set_store_config, StoreSettings};
    use crate::test_support::TempDir;
    use banto_tstore::ManualClock;

    const OFFSET_MS: i64 = 9 * 3_600_000;
    /// 2026-07-12T00:00:00Z（= 日本時間 09:00）。
    const DAY1_MS: i64 = 20_646 * 86_400_000;
    const DAY_MS: i64 = 86_400_000;

    fn date(offset_days: i64) -> LocalDate {
        LocalDate::from_epoch_ms(DAY1_MS + offset_days * DAY_MS, OFFSET_MS)
    }

    fn touch(dir: &TempDir, d: LocalDate) -> PathBuf {
        let path = dir.path().join(format!("{}-001.sqlite3", d.to_yyyymmdd()));
        std::fs::write(&path, b"").expect("touch");
        path
    }

    async fn settings_with(retention_days: Option<i64>) -> SettingsService {
        let pool = migrate_memory().await.expect("migrate_memory");
        let svc = SettingsService::new(Db::Sqlite(pool));
        set_store_config(
            &svc,
            &StoreSettings {
                data_dir: "./data".to_string(),
                retention_days,
            },
        )
        .await
        .unwrap();
        svc
    }

    #[test]
    fn plan_sweep_table() {
        assert_eq!(plan_sweep(None), SweepPlan::Unlimited);
        assert_eq!(plan_sweep(Some(0)), SweepPlan::Invalid(0));
        assert_eq!(plan_sweep(Some(-5)), SweepPlan::Invalid(-5));
        assert_eq!(plan_sweep(Some(1)), SweepPlan::Prune(1));
        assert_eq!(plan_sweep(Some(90)), SweepPlan::Prune(90));
        assert_eq!(
            plan_sweep(Some(u32::MAX as i64)),
            SweepPlan::Prune(u32::MAX)
        );
        let too_big = u32::MAX as i64 + 1;
        assert_eq!(plan_sweep(Some(too_big)), SweepPlan::Invalid(too_big));
    }

    /// 日付が変わったときだけ掃除する（起動直後 = 前回なし も掃除する）。
    #[test]
    fn needs_sweep_fires_on_startup_and_on_date_change_only() {
        let d1 = date(0);
        let d2 = date(1);
        assert!(needs_sweep(None, d1), "起動時");
        assert!(!needs_sweep(Some(d1), d1), "同じ日は 2 度やらない");
        assert!(needs_sweep(Some(d1), d2), "日付が変わった");
        assert!(needs_sweep(Some(d2), d1), "時計が戻っても掃除し直す");
    }

    /// 保持日数より古いファイルが消え、今日のファイルと範囲内のファイルは
    /// 残る。データファイルの形でないものには触らない。
    #[tokio::test]
    async fn sweep_deletes_old_files_but_keeps_today_and_strangers() {
        let dir = TempDir::new();
        let today = date(100);
        let old = touch(&dir, date(100 - 91));
        let edge = touch(&dir, date(100 - 90));
        let yesterday = touch(&dir, date(99));
        let today_file = touch(&dir, today);
        let stranger = dir.path().join("notes.txt");
        std::fs::write(&stranger, b"x").unwrap();

        let svc = settings_with(Some(90)).await;
        let clock = ManualClock::new(DAY1_MS + 100 * DAY_MS, OFFSET_MS);
        let outcome = sweep_once(&svc, dir.path(), &clock).await;

        assert_eq!(
            outcome,
            SweepOutcome::Pruned {
                days: 90,
                deleted: 1
            }
        );
        assert!(!old.exists(), "91 日前は消える");
        assert!(edge.exists(), "ちょうど 90 日前は残る");
        assert!(yesterday.exists());
        assert!(today_file.exists(), "今日のファイルは残る");
        assert!(stranger.exists(), "データファイルでないものは触らない");
    }

    /// 保持日数 1 でも今日と昨日（日付の変わり目に書き込み側が切り替え前の
    /// ファイル）は残る。
    #[tokio::test]
    async fn sweep_with_one_day_keeps_today_and_yesterday() {
        let dir = TempDir::new();
        let two_ago = touch(&dir, date(98));
        let yesterday = touch(&dir, date(99));
        let today_file = touch(&dir, date(100));
        let svc = settings_with(Some(1)).await;
        let clock = ManualClock::new(DAY1_MS + 100 * DAY_MS, OFFSET_MS);
        sweep_once(&svc, dir.path(), &clock).await;
        assert!(!two_ago.exists());
        assert!(yesterday.exists());
        assert!(today_file.exists());
    }

    /// 無制限・想定外の値では 1 つも消さない。
    #[tokio::test]
    async fn sweep_unlimited_deletes_nothing() {
        let dir = TempDir::new();
        let ancient = touch(&dir, date(-1000));
        let svc = settings_with(None).await;
        let clock = ManualClock::new(DAY1_MS, OFFSET_MS);
        assert_eq!(
            sweep_once(&svc, dir.path(), &clock).await,
            SweepOutcome::SkippedUnlimited
        );
        assert!(ancient.exists());
    }

    /// データディレクトリがまだ無い（まだ何も書いていない）のは失敗ではない。
    #[tokio::test]
    async fn sweep_with_missing_data_dir_is_not_a_failure() {
        let dir = TempDir::new();
        let svc = settings_with(Some(90)).await;
        let clock = ManualClock::new(DAY1_MS, OFFSET_MS);
        let outcome = sweep_once(&svc, &dir.path().join("nope"), &clock).await;
        assert_eq!(
            outcome,
            SweepOutcome::Pruned {
                days: 90,
                deleted: 0
            }
        );
    }

    /// 設定を変えたら次の掃除から効く: 90 日では残った 30 日前のファイルが、
    /// 7 日に変えたあとの掃除で消える。
    #[tokio::test]
    async fn a_changed_setting_takes_effect_on_the_next_sweep() {
        let dir = TempDir::new();
        let thirty_ago = touch(&dir, date(70));
        let svc = settings_with(Some(90)).await;
        let clock = ManualClock::new(DAY1_MS + 100 * DAY_MS, OFFSET_MS);

        sweep_once(&svc, dir.path(), &clock).await;
        assert!(thirty_ago.exists(), "90 日設定では残る");

        set_store_config(
            &svc,
            &StoreSettings {
                data_dir: "./data".to_string(),
                retention_days: Some(7),
            },
        )
        .await
        .unwrap();
        assert!(thirty_ago.exists(), "設定の保存だけでは消さない");

        sweep_once(&svc, dir.path(), &clock).await;
        assert!(!thirty_ago.exists(), "次の掃除で 7 日設定が効く");
    }

    /// 常駐ループ: 起動時に掃除し、日付が変わるまで再掃除せず、変わったら
    /// 掃除する。時間は止めず、間隔を短くして実時間で回す（`start_paused`
    /// だと SQLite プールの待ち時間まで進んでしまう）。
    #[tokio::test]
    async fn run_sweeps_at_startup_and_again_only_after_the_date_changes() {
        let dir = TempDir::new();
        let day0 = touch(&dir, date(0));
        let svc = settings_with(Some(7)).await;
        let clock = Arc::new(ManualClock::new(DAY1_MS + 100 * DAY_MS, OFFSET_MS));
        let handle = tokio::spawn(run_with(
            svc.clone(),
            dir.path().to_path_buf(),
            clock.clone(),
            Duration::from_millis(20),
        ));

        async fn wait_until_gone(path: &Path) {
            for _ in 0..200 {
                if !path.exists() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }

        wait_until_gone(&day0).await;
        assert!(!day0.exists(), "起動時に古いファイルが消える");

        // 同じ日のうちに古いファイルが現れても、再掃除はしない。
        let day1 = touch(&dir, date(1));
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(day1.exists(), "同じ日は 2 回目の掃除をしない");

        // 日付が変わると掃除する。
        clock.set_now_ms(DAY1_MS + 101 * DAY_MS);
        wait_until_gone(&day1).await;
        assert!(!day1.exists(), "日付が変わったら掃除する");
        handle.abort();
    }
}
