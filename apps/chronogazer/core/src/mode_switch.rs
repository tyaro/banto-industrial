//! 設定モード ⇔ 運用モードの切り替えの、SCM の扱いと待ち方（#623 PR-4。設計は
//! `docs/chronogazer-service-design.md` §5 の「設定モード ⇔ 運用モード」と、PR-1 設計書の
//! §3.6・§3.9・§8 の PR-4）。
//!
//! シェル（`src-tauri` の `shell`）が切り替えの画面から呼ぶ。ここは Tauri に触れず、SCM は
//! [`ServiceManager`] の trait、時間は [`SwitchClock`] を通すので、テストは偽の SCM と
//! 偽の時計で順序・期限・終了コードを確かめる。
//!
//! # 待ち方（[`PendingWatch`]）
//!
//! サービスは運用モードへ締める間（START_PENDING）と、設定モードへ緩める間（STOP_PENDING）、
//! checkpoint を進めながら報告する。締める時間は木の大きさに比例するので「待ち時間 >
//! 締める時間」は保証できない。代わりに、**checkpoint が進んでいる間は期限を延ばし**
//! （期限 = 最後に進んだ時刻 + max(段の待ち時間, wait hint)）、止まったまま期限を過ぎたら
//! 失敗（[`WatchStep::Stalled`]）、全体の上限（[`SwitchTimings::overall`]）を超えたら
//! 失敗（[`WatchStep::OverallTimeout`]）にする。「進んでいるか」は PR-1d の
//! [`pending_progress`]。
//!
//! # 3 つの道
//!
//! - **運用モードへ**（[`start_service_and_wait`]）: `start` → Running まで待つ。Running を
//!   経ずに止まったら、照会の終了コードで理由を分ける（[`ServiceExit`]: 4 = 運用モードに
//!   できなかった、5 = 締めたまま残った）。
//! - **設定モードへ**（[`stop_service_and_wait`]）: `stop` → Stopped まで待つ。START_PENDING の
//!   間は SCM が STOP を 1061（[`ServiceManagerError::CannotAcceptControl`]）か
//!   1052（[`ServiceManagerError::InvalidControl`]。START_PENDING／STOP_PENDING のときだけ）で断るので、
//!   失敗にせず Running になるのを待って出し直す（[`MAX_STOP_RETRIES`] 回まで）。
//! - **緩めるだけの開始**（[`relax_only_and_wait`]）: `start_with_args(["relax-only"])`
//!   （Stopped のときだけ、Operators なら UAC なし）→ **`Ok` が返った後に照会を始め**、最初の
//!   Stopped を完了と見る（開始の直前の Stopped（前の終了コードを載せたまま）を取り違えない。
//!   `StartServiceW` はディスパッチャの接続まで戻らない - PR-3 の実測）。返る
//!   `TransitionHandle` は目標が Running 固定なので使わない。
//!
//! # デスクトップのログ（[`append_desktop_log`]）
//!
//! 設定モードの間だけ `logs\chronogazer-desktop-YYYY-MM-DD.log` に書く（2026-10-18 オーナー
//! 決定 (3)）。運用モードの間は `logs\` が利用者には読み取りだけなので書かない: 設定モードへの
//! 切り替えの「開始」の行は、その時刻で覚えておき（[`DesktopLogEntry`]）、緩め終わって書ける
//! ようになってから「完了」の行と対で書く（途切れた区間を開始・完了の時刻の対で残す、PR-1
//! 設計書 §3.9）。同じファイルには、設定モードで収集している間の診断（標準出力・標準エラー
//! 出力の向け直し。`src-tauri` の `diag_log`、2026-10-24 オーナー決定「案 A」）も同じ行の形で入る。
//!
//! # 引き継ぎ（[`Handoff`]）
//!
//! デスクトップのエンジンは A1 の排他（`Global\ChronoGazer.<scope>`）をプロセスの終わりまで
//! 持つ（`DesktopEngine` は Tauri の managed state で外せない）。そこで設定画面の「運用モードへ」は、
//! エンジンを止めてプロセスを終え、新しいシェルに [`HANDOFF_TO_OPERATION_ARG`] と
//! [`HANDOFF_WAIT_PID_PREFIX`] を渡して続きを任せる。新しいシェルは古いプロセスが終わるのを
//! 待ってから（[`wait_for_process_exit`]。single-instance の判定より前）サービスを開始する。
//!
//! # インストーラからの終了の要求（[`QUIT_FOR_UPDATE_ARG`]、#392 B3-4）
//!
//! インストーラは `taskkill /F` の代わりに `chronogazer.exe --quit-for-update` を起動する。動いて
//! いるシェルが single-instance で受け取り、確認ダイアログを出さずにトレイの「終了」と同じ
//! 後始末（最終 flush・`collection_stopped`）を通して終わる。引数の解釈は
//! [`wants_quit_for_update`]（純関数）。

use std::path::Path;
use std::time::{Duration, Instant};

use banto_service_control::{
    relax_only_and_wait as shared_relax_only_and_wait, service_exit_with, wait_for_state, ScmState,
    ServiceManager, ServiceManagerError, ServiceStatusSummary, Waited,
};
// 待ち方の部品は banto-service-control に移した（#661 661-4。banto-hub と共有）。呼び元の
// 参照（`chronogazer_core::mode_switch::...`）は変えない。
pub use banto_service_control::{
    PendingWatch, RealClock, SwitchClock, SwitchTimings, WatchStep, MAX_STOP_RETRIES,
};
use banto_tstore::Clock;

use crate::server_host::{
    desktop_log_file_name, format_timestamp, EXIT_LEFT_TIGHT, EXIT_NOT_OPERATIONAL,
};
use crate::server_service::RELAX_ONLY_ARG;

// ---------------------------------------------------------------------------
// 終了コード
// ---------------------------------------------------------------------------

// 共有 crate では `ServiceEnd`（runner の `ServiceExit` と別物）。ここでは従来の名前で使う。
pub use banto_service_control::ServiceEnd as ServiceExit;

/// 照会の結果から終了の理由を読む（純関数。4 = 運用モードにできなかった、5 = 締めたまま残った）。
/// Stopped でない照会に使っても値は読むが、意味を持つのは Stopped のときだけ。
pub fn service_exit(summary: &ServiceStatusSummary) -> ServiceExit {
    service_exit_with(summary, EXIT_NOT_OPERATIONAL as u32, EXIT_LEFT_TIGHT as u32)
}

// ---------------------------------------------------------------------------
// 段階（画面に出す）
// ---------------------------------------------------------------------------

/// 切り替えの段階（画面の「いま何をしているか」の 1 行）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchStage {
    /// 記録エンジン（サービス）の開始を頼んでいる。
    StartingService,
    /// サービスがデータのフォルダの権限を運用モードの形にしている（START_PENDING）。
    TighteningPermissions,
    /// サービスの記録エンジンの立ち上がりを待っている（Running の後の接続の確認）。
    WaitingForService,
    /// サービスに停止を頼んでいる。
    StoppingService,
    /// 権限の設定の途中で停止を断られたので、終わるのを待っている（1061／1052）。
    WaitingForTightening,
    /// サービスがデータのフォルダの権限を設定モードの形に戻している（STOP_PENDING・
    /// `relax-only` の START_PENDING）。
    RelaxingPermissions,
    /// 管理者の許可（UAC）で権限を直している（elev の修復）。
    RepairingPermissions,
    /// この PC の中で記録エンジンを起動している。
    StartingDesktop,
}

impl SwitchStage {
    /// 画面の 1 行（利用者向け。「サービス」「デスクトップ」の語を前に出さない）。
    pub fn label(self) -> &'static str {
        match self {
            Self::StartingService => "常時記録の仕組みを開始しています",
            Self::TighteningPermissions => "ファイルの権限を設定しています",
            Self::WaitingForService => "記録の開始を確かめています",
            Self::StoppingService => "常時記録の仕組みを止めています",
            Self::WaitingForTightening => {
                "ファイルの権限の設定が終わるのを待っています（終わり次第、止めます）"
            }
            Self::RelaxingPermissions => "ファイルの権限を元に戻しています",
            Self::RepairingPermissions => "管理者の許可でファイルの権限を直しています",
            Self::StartingDesktop => "この画面で記録を始めています",
        }
    }
}

// ---------------------------------------------------------------------------
// 待ち方
// ---------------------------------------------------------------------------

/// 本番の待ち時間（段 15 秒は banto-hub の `HOST_SWITCH_PHASE_TIMEOUT` と同じ。全体 30 分は
/// PR-1 設計書 §3.6 の案）。
pub const DEFAULT_TIMINGS: SwitchTimings = SwitchTimings {
    poll: Duration::from_millis(500),
    phase: Duration::from_secs(15),
    overall: Duration::from_secs(30 * 60),
};

/// `target` まで照会を繰り返す。段階は pending の種類で知らせる。
fn wait_for(
    manager: &impl ServiceManager,
    clock: &impl SwitchClock,
    timings: SwitchTimings,
    target: ScmState,
    progress: &mut dyn FnMut(SwitchStage),
    pending_stage: impl Fn(&ScmState) -> Option<SwitchStage>,
) -> Waited {
    let mut shown: Option<SwitchStage> = None;
    wait_for_state(manager, clock, timings, target, &mut |state| {
        if let Some(stage) = pending_stage(state) {
            if shown != Some(stage) {
                shown = Some(stage);
                progress(stage);
            }
        }
    })
}

// ---------------------------------------------------------------------------
// 3 つの道
// ---------------------------------------------------------------------------

/// 運用モードへ（サービスの開始）の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartOutcome {
    /// Running になった（締め終わった）。この後は health を待つ（呼び出し側）。
    Running,
    /// Running を経ずに止まった（終了の理由つき）。
    Ended(ServiceExit),
    /// サービスが登録されていない。
    NotInstalled,
    /// 開始を頼めなかった（権限が無い・その他。文言は中立）。
    StartFailed(String),
    /// 途中で別の状態になった（未登録・知らない状態）。
    Unexpected(ScmState),
    /// checkpoint が止まったまま期限を過ぎた。
    Stalled,
    /// 全体の上限を超えた。
    TimedOut,
}

/// 運用モードへ: サービスを開始し、Running まで待つ（モジュール doc「3 つの道」）。
pub fn start_service_and_wait(
    manager: &impl ServiceManager,
    clock: &impl SwitchClock,
    timings: SwitchTimings,
    progress: &mut dyn FnMut(SwitchStage),
) -> StartOutcome {
    progress(SwitchStage::StartingService);
    match manager.start() {
        Ok(_) | Err(ServiceManagerError::AlreadyRunning) => {}
        Err(ServiceManagerError::NotFound(_)) => return StartOutcome::NotInstalled,
        Err(err) => return StartOutcome::StartFailed(err.to_string()),
    }
    match wait_for(
        manager,
        clock,
        timings,
        ScmState::Running,
        progress,
        |state| (*state == ScmState::StartPending).then_some(SwitchStage::TighteningPermissions),
    ) {
        Waited::Reached(_) => StartOutcome::Running,
        Waited::Unexpected(summary) if summary.state == ScmState::Stopped => {
            StartOutcome::Ended(service_exit(&summary))
        }
        Waited::Unexpected(summary) => StartOutcome::Unexpected(summary.state),
        Waited::Stalled => StartOutcome::Stalled,
        Waited::TimedOut => StartOutcome::TimedOut,
    }
}

/// 設定モードへ（サービスの停止）の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopOutcome {
    /// 止まった（終了の理由つき。5 なら締めたまま残った）。
    Stopped(ServiceExit),
    /// サービスが登録されていない。
    NotInstalled,
    /// 停止を頼めなかった（権限が無い・その他）。
    StopFailed(String),
    /// 途中で別の状態になった（停止の途中で Running に戻った・未登録など）。
    Unexpected(ScmState),
    /// checkpoint が止まったまま期限を過ぎた。
    Stalled,
    /// 全体の上限を超えた。
    TimedOut,
}

/// 設定モードへ: サービスを止め、Stopped まで待つ。1061／1052（権限の設定の途中）は Running に
/// なるのを待って出し直す（モジュール doc「3 つの道」）。
pub fn stop_service_and_wait(
    manager: &impl ServiceManager,
    clock: &impl SwitchClock,
    timings: SwitchTimings,
    progress: &mut dyn FnMut(SwitchStage),
) -> StopOutcome {
    let mut retries = 0;
    let mut resent_on_running = false;
    loop {
        progress(SwitchStage::StoppingService);
        match manager.stop() {
            Ok(_) => break,
            Err(ServiceManagerError::NotFound(_)) => return StopOutcome::NotInstalled,
            Err(
                err @ (ServiceManagerError::CannotAcceptControl
                | ServiceManagerError::InvalidControl),
            ) => {
                let can_wait = retries < MAX_STOP_RETRIES;
                let wait = match err {
                    ServiceManagerError::CannotAcceptControl => can_wait,
                    // 1052 は、照会し直した状態で分ける。締めている間（START_PENDING。
                    // `controls_accepted` が空）か止まりかけ（STOP_PENDING）は「切り替え中」。
                    // 1052 の後に起動が終わった・失敗した競合は、Stopped なら結果、Running なら
                    // STOP を 1 回だけ出し直す。
                    _ => match manager.query_status() {
                        Ok(summary) => match summary.state {
                            ScmState::StartPending | ScmState::StopPending => can_wait,
                            ScmState::Stopped => {
                                return StopOutcome::Stopped(service_exit(&summary));
                            }
                            ScmState::Running if !resent_on_running => {
                                resent_on_running = true;
                                continue;
                            }
                            _ => false,
                        },
                        Err(_) => false,
                    },
                };
                if !wait {
                    return StopOutcome::StopFailed(err.to_string());
                }
                retries += 1;
                progress(SwitchStage::WaitingForTightening);
                match wait_for(manager, clock, timings, ScmState::Running, progress, |_| {
                    None
                }) {
                    // 締め終わった: 止め直す。
                    Waited::Reached(_) => continue,
                    // 締める途中で自分から止まった（4・5）: それが停止の結果。
                    Waited::Unexpected(summary) if summary.state == ScmState::Stopped => {
                        return StopOutcome::Stopped(service_exit(&summary));
                    }
                    Waited::Unexpected(summary) => return StopOutcome::Unexpected(summary.state),
                    Waited::Stalled => return StopOutcome::Stalled,
                    Waited::TimedOut => return StopOutcome::TimedOut,
                }
            }
            Err(err) => return StopOutcome::StopFailed(err.to_string()),
        }
    }
    match wait_for(
        manager,
        clock,
        timings,
        ScmState::Stopped,
        progress,
        |state| (*state == ScmState::StopPending).then_some(SwitchStage::RelaxingPermissions),
    ) {
        Waited::Reached(summary) => StopOutcome::Stopped(service_exit(&summary)),
        Waited::Unexpected(summary) => StopOutcome::Unexpected(summary.state),
        Waited::Stalled => StopOutcome::Stalled,
        Waited::TimedOut => StopOutcome::TimedOut,
    }
}

pub use banto_service_control::RelaxOnlyOutcome;

/// 緩めるだけの開始: `start_with_args(["relax-only"])` → `Ok` の**後に**照会を始め、最初の
/// Stopped を完了と見る（モジュール doc「3 つの道」。実体は banto-service-control）。
pub fn relax_only_and_wait(
    manager: &impl ServiceManager,
    clock: &impl SwitchClock,
    timings: SwitchTimings,
    progress: &mut dyn FnMut(SwitchStage),
) -> RelaxOnlyOutcome {
    progress(SwitchStage::RelaxingPermissions);
    shared_relax_only_and_wait(
        manager,
        clock,
        timings,
        RELAX_ONLY_ARG,
        EXIT_NOT_OPERATIONAL as u32,
        EXIT_LEFT_TIGHT as u32,
    )
}

// ---------------------------------------------------------------------------
// デスクトップのログ
// ---------------------------------------------------------------------------

/// デスクトップのログの 1 行（書く時刻ではなく、起きた時刻を持つ）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopLogEntry {
    pub at_ms: i64,
    pub text: String,
}

impl DesktopLogEntry {
    /// 今の時刻の行。
    pub fn now(clock: &dyn Clock, text: impl Into<String>) -> Self {
        Self {
            at_ms: clock.now_ms(),
            text: text.into(),
        }
    }

    /// 今の時刻（OS の時計）の行。
    pub fn now_system(text: impl Into<String>) -> Self {
        Self::now(&banto_tstore::SystemClock, text)
    }
}

/// [`append_desktop_log`] を OS の時計で（シェルが使う）。
pub fn append_desktop_log_now(
    logs_dir: &Path,
    entries: &[DesktopLogEntry],
) -> std::io::Result<std::path::PathBuf> {
    append_desktop_log(logs_dir, &banto_tstore::SystemClock, entries)
}

/// デスクトップのログに行を足す（今日のファイル `logs\chronogazer-desktop-YYYY-MM-DD.log`。
/// 各行の先頭は起きた時刻の `[YYYY-MM-DD HH:MM:SS]`）。**設定モードの間だけ呼ぶ**（モジュール
/// doc「デスクトップのログ」）。`logs\` が無ければ作る。失敗は呼び出し側が標準エラー出力へ出す
/// （ログが書けないことで切り替えを止めない）。
pub fn append_desktop_log(
    logs_dir: &Path,
    clock: &dyn Clock,
    entries: &[DesktopLogEntry],
) -> std::io::Result<std::path::PathBuf> {
    use std::io::Write;
    let offset_ms = clock.utc_offset_ms();
    let today = banto_tstore::LocalDate::from_epoch_ms(clock.now_ms(), offset_ms);
    std::fs::create_dir_all(logs_dir)?;
    let path = logs_dir.join(desktop_log_file_name(today));
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    let mut text = String::new();
    for entry in entries {
        text.push_str(&desktop_log_line(entry, offset_ms));
        text.push('\n');
    }
    file.write_all(text.as_bytes())?;
    file.flush()?;
    Ok(path)
}

/// デスクトップのログの 1 行の文字列（純関数）。
pub fn desktop_log_line(entry: &DesktopLogEntry, utc_offset_ms: i64) -> String {
    format!(
        "[{}] {}",
        format_timestamp(entry.at_ms, utc_offset_ms),
        entry.text
    )
}

/// 「運用モードへの切り替えを開始」の行（デスクトップのエンジンを止める前。完了はサービスの
/// 日次ログが書く）。
pub fn log_to_operation_started(auto_start: bool) -> String {
    format!(
        "運用モードへの切り替えを開始（この PC の中の記録エンジンを止め、サービスを開始します。Windows の起動時の自動開始: {}）",
        if auto_start { "する" } else { "しない" }
    )
}

/// 「設定モードへの切り替えを開始」の行（サービスへ STOP を出す前の時刻で覚えておく）。
pub const LOG_TO_SETTING_STARTED: &str = "設定モードへの切り替えを開始（サービスを停止します）";

/// 「設定モードへの切り替えを完了」の行（デスクトップのエンジンが動き始めた後）。
pub const LOG_TO_SETTING_COMPLETED: &str =
    "設定モードへの切り替えを完了（この PC の中で収集を開始します）";

/// サービスの終了コードが 5（緩められなかった）だったときの行。
pub const LOG_RELAX_FAILED: &str =
    "サービスはデータのフォルダの権限を元に戻せませんでした（終了コード 5。運用モードのまま残っています）";

/// 緩めるだけの開始・elev の修復で設定モードに戻ったときの行。
pub fn log_restored_by(how: &str) -> String {
    format!("データのフォルダの権限を設定モードに戻しました（{how}）")
}

// ---------------------------------------------------------------------------
// 引き継ぎ
// ---------------------------------------------------------------------------

/// 新しいシェルに「運用モードへの切り替えを続ける」ことを渡す引数。
pub const HANDOFF_TO_OPERATION_ARG: &str = "--switch-to-operation";
/// 新しいシェルに「このプロセスが終わるまで待つ」ことを渡す引数の頭（`--wait-pid=<pid>`）。
pub const HANDOFF_WAIT_PID_PREFIX: &str = "--wait-pid=";
/// 古いプロセスが終わるのを待つ上限（終了の後始末の予算 5 秒＋余裕）。
pub const HANDOFF_WAIT_LIMIT: Duration = Duration::from_secs(30);

/// 起動の引数から読んだ引き継ぎ。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Handoff {
    /// 運用モードへの切り替えを続ける。
    pub to_operation: bool,
    /// 先に終わるのを待つプロセス。
    pub wait_pid: Option<u32>,
}

/// 起動の引数（プログラム名を除く）から引き継ぎを読む（純関数）。形が完全に一致するものだけ
/// 受け、ほかは無視する（シェルはほかの引数を使わない。single-instance で前のプロセスに
/// 渡った引数もここを通さない）。
pub fn parse_handoff<'a>(args: impl IntoIterator<Item = &'a str>) -> Handoff {
    let mut handoff = Handoff::default();
    for arg in args {
        if arg == HANDOFF_TO_OPERATION_ARG {
            handoff.to_operation = true;
        } else if let Some(pid) = arg.strip_prefix(HANDOFF_WAIT_PID_PREFIX) {
            handoff.wait_pid = pid.parse::<u32>().ok().filter(|pid| *pid != 0);
        }
    }
    handoff
}

/// 新しいシェルに渡す引数（[`parse_handoff`] の逆）。
pub fn handoff_args(wait_pid: u32) -> [String; 2] {
    [
        HANDOFF_TO_OPERATION_ARG.to_string(),
        format!("{HANDOFF_WAIT_PID_PREFIX}{wait_pid}"),
    ]
}

// ---------------------------------------------------------------------------
// インストーラからの終了の要求（#392 B3-4）
// ---------------------------------------------------------------------------

/// インストーラが動いているシェルに「確認を出さずに後始末を通して終わる」ことを頼む引数
/// （`docs/chronogazer-service-design.md` §4「インストーラによるシェルの停止」）。シェルが
/// 動いていれば single-instance でそのプロセスへ渡り、動いていなければ何も始めずに 0 で
/// 終わる（`src-tauri` の `run()`）。
pub const QUIT_FOR_UPDATE_ARG: &str = "--quit-for-update";

/// 起動の引数（プログラム名を除いても含んでもよい）に [`QUIT_FOR_UPDATE_ARG`] があるか
/// （純関数。形が完全に一致するものだけ）。
pub fn wants_quit_for_update<'a>(args: impl IntoIterator<Item = &'a str>) -> bool {
    args.into_iter().any(|arg| arg == QUIT_FOR_UPDATE_ARG)
}

/// `pid` のプロセスが終わるまで待つ（上限つき。終わったら `true`）。
pub fn wait_for_process_exit(pid: u32, limit: Duration) -> bool {
    let started = Instant::now();
    while banto_instance_lock::process_is_alive(pid) {
        if started.elapsed() >= limit {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    true
}

#[cfg(test)]
mod tests;
