//! 設定モード ⇔ 運用モードの切り替えの配線（#623 PR-4。設計は
//! `docs/chronogazer-service-design.md` §5 の「設定モード ⇔ 運用モード」・§4 の「#623 PR-4 の
//! 実装メモ」と、PR-1 設計書の §3.6・§3.9・§8 の PR-4）。
//!
//! SCM の扱いと待ち方（checkpoint が進む間は待つ・1061 は待って出し直す・`relax-only` は `Ok` の
//! 後に照会を始める）は `chronogazer_core::mode_switch` の純粋な部分と偽の SCM のテストが持つ。
//! ここは「どの入り口から・どの順で呼び、結果でどの画面へ移るか」と文言だけ。
//!
//! # 入り口と流れ
//!
//! | 入り口                                        | 流れ                                                                                               |
//! | --------------------------------------------- | -------------------------------------------------------------------------------------------------- |
//! | 設定画面の「運用モードに切り替える」（Desktop） | 前の確かめ（サービスが停止中）→（自動開始なら）elev `autostart-enable`（UAC 1 回）→ 表の「運用モードへ」→ デスクトップのログ →エンジンを止める（終了の後始末）→ 新しいシェルへ引き継いで終わる |
//! | 引き継いだ新しいシェル・Fallback の [運用を再開] | 札 → サービスを開始 → 締める間は段階を出す → Running → 接続を確かめて Service の画面へ               |
//! | トレイの「設定モードに切り替える」（Service）・Fallback の [設定モードに切り替える]（サービスが動いている） | 札 → STOP（1061 なら待って出し直す）→ 緩める間は段階を出す → Stopped → `decide` の表 → Desktop |
//! | Fallback の [設定モードに切り替える]・[設定モードに戻す]（サービスが停止） | 札 → `relax-only`（UAC なし）→ 戻らなければ elev `grant-data-acl`（UAC）→ `decide` の表 → Desktop |
//!
//! Desktop へ進むかは、どの道でも「再試行」と同じ `shell::on_retry`（= `decide` の表）で決める
//! （モードが運用モードのままなら決して Desktop にしない。ライフサイクルの表の注 3・注 4）。
//!
//! # なぜ「運用モードへ」はプロセスを終えるか
//!
//! この PC の中のエンジンは A1 の排他（DB の二重オープンを防ぐ名前付き mutex）を、プロセスの
//! 終わりまで持つ（`DesktopEngine`・`AppState` は Tauri の managed state で外せない）。排他を
//! 持ったままサービスを開始すると、サービスは A1 で終了コード 2 になる。そこでエンジンを終了の
//! 後始末（最終 flush を含む）で止め、新しいシェルに `--switch-to-operation --wait-pid=<pid>` を
//! 渡してプロセスを終える。新しいシェルは古いプロセスが終わるのを待ってから（`run()` の最初、
//! single-instance の判定より前）続きを行う。「Desktop から他の画面へは移らない」の約束は
//! そのまま（移るのは新しいプロセス）。
//!
//! # デスクトップのログ
//!
//! 設定モードの間だけ `logs\chronogazer-desktop-YYYY-MM-DD.log` に書く（オーナー決定 (3)）。
//! 運用モードの間は `logs\` が読み取りだけなので、設定モードへの切り替えの「開始」などは起きた
//! 時刻で覚えておき（`ShellState::pending_log`）、Desktop のエンジンが動き始めてから「完了」と
//! 対で書く。
//!
//! # 既知の制限
//!
//! - 切り替えの最中（札を持っている間）にトレイの「終了」を押すと、終了は札の終わりを 60 秒まで
//!   待ってから進む（ライフサイクルの表）。締める・緩めるのが 60 秒より長いと、切り替えの途中で
//!   シェルだけが終わる（サービスは自分で締め終わる・緩め終わるので、次の起動で今の状態を
//!   判定し直す）。
//! - 引き継いだ新しいシェルが起動できなかった（実行ファイルを起動できない）ときは、知らせて
//!   終わる。次に起動すると、サービスが停止中なので設定モードで始まる（切り替えは続かない）。

use std::path::PathBuf;

use banto_service_control::{ScmState, ServiceManager, ServiceManagerError, ServiceStatusSummary};
use chronogazer_core::mode_switch::{
    self, relax_only_and_wait, start_service_and_wait, stop_service_and_wait, DesktopLogEntry,
    Handoff, RealClock, RelaxOnlyOutcome, ServiceExit, StartOutcome, StopOutcome, SwitchStage,
    DEFAULT_TIMINGS,
};
use chronogazer_core::shell_launch::{ScmObservation, ServiceHealth};
use tauri::{AppHandle, Manager};

use crate::lifecycle::{Refusal, TransitionGuard};
use crate::shell::{
    self, FallbackCause, FallbackState, ShellState, ShellView, Step, SwitchDirection,
    FALLBACK_WINDOW_LABEL, SERVICE_WINDOW_LABEL,
};

/// 運用モードへの切り替えの後、サービスの接続を確かめる上限（Running の後。締めるのは
/// Running の前に終わっているので、ここはエンジンが listener を開くまでの短い待ち）。
const HEALTH_WAIT_ATTEMPTS: u32 = 30;
const HEALTH_WAIT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// 昇格ヘルパーの実行ファイル名（シェルの隣に置かれる。#392 B3-3b）。
const ELEV_EXE_FILE_NAME: &str = "chronogazer-elev.exe";
/// elev のアクション: 自動開始（遅延）にする。
const ELEV_AUTOSTART_ENABLE: &str = "autostart-enable";
/// elev のアクション: データのフォルダの ACL を設定モードの形に直す（修復）。
const ELEV_GRANT_DATA_ACL: &str = "grant-data-acl";
/// `ShellExecuteExW` の「利用者が断った」（`ERROR_CANCELLED`）。
const ERROR_CANCELLED: u32 = 1223;

// ---------------------------------------------------------------------------
// 判断と文言（純関数）
// ---------------------------------------------------------------------------

/// 引き継いだ新しいシェルが「運用モードへ」を続けてよいか（SCM が停止のときだけ。純関数）。
pub(crate) fn handoff_applies(handoff: Handoff, scm: &ScmObservation) -> bool {
    handoff.to_operation && *scm == ScmObservation::State(ScmState::Stopped)
}

/// 「運用モードへ」の前の確かめの結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OperationPlan {
    /// 自動開始を elev で設定する（チェックが入っていて、今は手動開始）。
    pub(crate) enable_auto_start: bool,
}

/// 「運用モードへ」の前の確かめ（純関数）。サービスが停止中のときだけ進める。`query` は照会の
/// 結果（Windows でないときは `None`）。
pub(crate) fn operation_precheck(
    query: Option<&Result<ServiceStatusSummary, ServiceManagerError>>,
    auto_start: bool,
) -> Result<OperationPlan, String> {
    let summary = match query {
        None => return Err("運用モードは Windows でだけ使えます。".to_string()),
        Some(Err(err)) => {
            return Err(format!(
                "常時記録の仕組みの状態を確かめられませんでした（{err}）。"
            ))
        }
        Some(Ok(summary)) => summary,
    };
    match summary.state {
        ScmState::Stopped => Ok(OperationPlan {
            enable_auto_start: auto_start && !summary.auto_start,
        }),
        ScmState::NotInstalled => Err(
            "このPCには常時記録の仕組み（運用モードの部品）が入っていません。ChronoGazer のインストーラで入れてから、もう一度切り替えてください。"
                .to_string(),
        ),
        ref other => Err(format!(
            "常時記録の仕組みが既に動いています（{}）。少し待ってから、もう一度試してください。",
            shell::describe_scm(&ScmObservation::State(other.clone()))
        )),
    }
}

/// elev の結果を文言にする（純関数）。`what` は何のための許可か。
pub(crate) fn elev_result_message(
    result: Result<u32, banto_service_elev::launch::LaunchError>,
    what: &str,
) -> Result<(), String> {
    use banto_service_elev::launch::LaunchError;
    match result {
        Ok(0) => Ok(()),
        Ok(code) => Err(format!(
            "{what}に失敗しました（管理者用の補助プログラムの終了コード {code}。詳しくは管理者に確かめてもらってください）。"
        )),
        Err(LaunchError::ShellExecute(ERROR_CANCELLED)) => {
            Err(format!("{what}のための管理者の許可が得られませんでした。"))
        }
        Err(err) => Err(format!("{what}ができませんでした（{err}）。")),
    }
}

/// 自動開始の設定に失敗したときの文（設定画面に出す。純関数）。
pub(crate) fn auto_start_failure_message(reason: &str) -> String {
    format!(
        "{reason}切り替えはしていません。「Windows の起動時に自動で開始する」のチェックを外すと、自動開始なしで切り替えられます（再起動の後は「運用を再開」を押します）。"
    )
}

/// 「運用モードへ」を断った理由の文（純関数）。
pub(crate) fn refusal_message(refusal: Refusal) -> &'static str {
    match refusal {
        Refusal::Exiting | Refusal::AlreadyQuitting => "終了処理中のため、切り替えられません。",
        Refusal::AlreadyConfirming => {
            "終了の確認の途中です。確認を閉じてから、もう一度試してください。"
        }
        Refusal::RecoveryInProgress | Refusal::RecoveryBusy => {
            "データベースの復旧の途中です。終わってから、もう一度試してください。"
        }
        Refusal::Busy
        | Refusal::Stale
        | Refusal::NotFallback
        | Refusal::Quitting
        | Refusal::EngineRunning
        | Refusal::NotDesktop
        | Refusal::NotServiceSide => {
            "別の処理の途中です。しばらくしてから、もう一度試してください。"
        }
    }
}

/// 運用モードへの開始が Running にならなかったときの、次の画面の理由と知らせ（純関数）。
pub(crate) fn after_start_failure(outcome: &StartOutcome) -> (FallbackCause, Option<String>) {
    match outcome {
        StartOutcome::Running => (FallbackCause::AtStartup, None),
        StartOutcome::Ended(ServiceExit::CannotOperate) => (FallbackCause::CannotOperate, None),
        StartOutcome::Ended(ServiceExit::LeftOperation) => {
            (FallbackCause::OperationStopped { left_tight: true }, None)
        }
        StartOutcome::Ended(ServiceExit::Clean) => (
            FallbackCause::AtStartup,
            Some("常時記録の仕組みが、記録を始める前に止まりました。原因はログで確かめられます。".to_string()),
        ),
        StartOutcome::Ended(ServiceExit::Other { win32, specific }) => (
            FallbackCause::AtStartup,
            Some(format!(
                "常時記録の仕組みが、記録を始める前に止まりました（終了コード {win32}・{specific}）。原因はログで確かめられます。"
            )),
        ),
        StartOutcome::NotInstalled => (
            FallbackCause::AtStartup,
            Some("このPCには常時記録の仕組み（運用モードの部品）が入っていません。".to_string()),
        ),
        StartOutcome::StartFailed(reason) => (
            FallbackCause::AtStartup,
            Some(format!(
                "常時記録の仕組みを開始できませんでした（{reason}）。ChronoGazer Operators のメンバーか管理者で操作してください。"
            )),
        ),
        StartOutcome::Unexpected(state) => (
            FallbackCause::AtStartup,
            Some(format!(
                "常時記録の仕組みが思わぬ状態になりました（{state}）。"
            )),
        ),
        StartOutcome::Stalled => (
            FallbackCause::AtStartup,
            Some("ファイルの権限の設定が進まなくなりました。常時記録の仕組みのログを確かめてください。".to_string()),
        ),
        StartOutcome::TimedOut => (
            FallbackCause::AtStartup,
            Some("切り替えが 30 分以内に終わりませんでした。常時記録の仕組みのログを確かめてください。".to_string()),
        ),
    }
}

/// 設定モードへの停止の結果の扱い（純関数）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AfterStop {
    /// 止まった: `decide` の表で次を決める（追加のログの行つき）。
    Decide {
        exit: ServiceExit,
        log: Option<String>,
    },
    /// 止められなかった: Fallback に知らせる（ログの行つき）。
    Failed { notice: String, log: String },
}

/// 停止の結果から次を決める（純関数）。
pub(crate) fn after_stop(outcome: &StopOutcome) -> AfterStop {
    let failed = |notice: String| AfterStop::Failed {
        log: format!("設定モードへの切り替えを中断（{notice}）"),
        notice,
    };
    match outcome {
        StopOutcome::Stopped(exit @ ServiceExit::LeftOperation) => AfterStop::Decide {
            exit: *exit,
            log: Some(mode_switch::LOG_RELAX_FAILED.to_string()),
        },
        StopOutcome::Stopped(exit @ ServiceExit::CannotOperate) => AfterStop::Decide {
            exit: *exit,
            log: Some(
                "サービスは運用モードにできないまま止まりました（終了コード 4。設定モードのままです）"
                    .to_string(),
            ),
        },
        StopOutcome::Stopped(exit) => AfterStop::Decide {
            exit: *exit,
            log: None,
        },
        StopOutcome::NotInstalled => {
            failed("常時記録の仕組みが登録されていません。".to_string())
        }
        StopOutcome::StopFailed(reason) => failed(format!(
            "常時記録の仕組みを止められませんでした（{reason}）。ChronoGazer Operators のメンバーか管理者で操作してください。"
        )),
        StopOutcome::Unexpected(state) => failed(format!(
            "常時記録の仕組みが停止の途中で思わぬ状態になりました（{state}）。"
        )),
        StopOutcome::Stalled => failed(
            "ファイルの権限を元に戻すのが進まなくなりました。常時記録の仕組みのログを確かめてください。"
                .to_string(),
        ),
        StopOutcome::TimedOut => failed(
            "切り替えが 30 分以内に終わりませんでした。常時記録の仕組みのログを確かめてください。"
                .to_string(),
        ),
    }
}

/// 緩めるだけの開始の結果の扱い（純関数）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AfterRelax {
    /// 緩め終わった: `decide` の表で次を決める。
    Decide,
    /// 戻らなかった: elev の修復（UAC）へ。
    Elevate,
    /// サービスが動いている（停止中ではなかった）: 知らせるだけ。
    Running(String),
}

/// 緩めるだけの開始の結果から次を決める（純関数）。戻らなかった理由が何であれ（5・未登録・
/// 権限が無い・固まった）、最後の手段は elev の修復（設計 §3.6: `relax-only` で戻らなければ
/// elev）。サービスが動いているときだけは修復しない（elev も稼働中は断る）。
pub(crate) fn after_relax(outcome: &RelaxOnlyOutcome) -> AfterRelax {
    match outcome {
        RelaxOnlyOutcome::Done(ServiceExit::Clean) => AfterRelax::Decide,
        RelaxOnlyOutcome::NotStopped(state) => AfterRelax::Running(format!(
            "常時記録の仕組みが動いています（{state}）。止まってから、もう一度押してください。"
        )),
        RelaxOnlyOutcome::Done(_)
        | RelaxOnlyOutcome::NotInstalled
        | RelaxOnlyOutcome::Refused(_)
        | RelaxOnlyOutcome::Unexpected(_)
        | RelaxOnlyOutcome::Stalled
        | RelaxOnlyOutcome::TimedOut => AfterRelax::Elevate,
    }
}

/// [設定モードに切り替える] の道（純関数）: サービスが動いているなら止める、止まっているなら
/// 緩めるだけの開始（と elev の修復）、分からなければ知らせる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToSettingRoute {
    Stop,
    Restore,
    Unknown,
}

pub(crate) fn to_setting_route(scm: &ScmObservation) -> ToSettingRoute {
    match scm {
        ScmObservation::State(
            ScmState::Running | ScmState::StartPending | ScmState::StopPending,
        ) => ToSettingRoute::Stop,
        ScmObservation::State(ScmState::Stopped | ScmState::NotInstalled) => {
            ToSettingRoute::Restore
        }
        ScmObservation::State(ScmState::Other(_))
        | ScmObservation::QueryFailed(_)
        | ScmObservation::NotWindows => ToSettingRoute::Unknown,
    }
}

/// トレイの「設定モードに切り替える」の確認の文。
pub(crate) const TO_SETTING_PROMPT: &str = "設定モード（この画面で記録し、設定を変えられる形）に切り替えます。切り替えの間は収集が止まります。よろしいですか?";

// ---------------------------------------------------------------------------
// 設定画面の「運用モードへ」（Desktop）
// ---------------------------------------------------------------------------

/// 設定画面の「運用モードに切り替える」（Tauri のコマンド `switch_to_operation_mode` の本体）。
/// 受け付けたら `Ok`（この後プロセスは終わり、新しいシェルが続ける）。断ったら理由の文。
pub(crate) async fn request_switch_to_operation(
    app: &AppHandle,
    auto_start: bool,
) -> Result<(), String> {
    let (layout, hooks) = {
        let shell = app
            .try_state::<ShellState>()
            .ok_or_else(|| "まだ起動の途中です。".to_string())?;
        (shell.layout.clone(), shell.hooks.clone())
    };
    let query = tauri::async_runtime::spawn_blocking(move || {
        cfg!(windows).then(|| shell::service_manager(&hooks).query_status())
    })
    .await
    .map_err(|err| format!("常時記録の仕組みの状態を確かめられませんでした（{err}）。"))?;
    let plan = operation_precheck(query.as_ref(), auto_start)?;
    if plan.enable_auto_start {
        tauri::async_runtime::spawn_blocking(|| run_elev(ELEV_AUTOSTART_ENABLE, "自動開始の設定"))
            .await
            .map_err(|err| {
                auto_start_failure_message(&format!("自動開始を設定できませんでした（{err}）。"))
            })?
            .map_err(|reason| auto_start_failure_message(&reason))?;
    }
    let lifecycle = shell::lifecycle(app).ok_or_else(|| "まだ起動の途中です。".to_string())?;
    lifecycle
        .begin_switch_to_operation()
        .map_err(|refusal| refusal_message(refusal).to_string())?;
    // ここから先は終了と同じ（確定は戻らない）。
    write_desktop_log(
        &layout.logs_dir(),
        &[DesktopLogEntry::now_system(
            mode_switch::log_to_operation_started(auto_start),
        )],
    );
    eprintln!("banto: 運用モードへ切り替えます（記録エンジンを止め、新しい画面に続きを任せます）");
    let app = app.clone();
    std::thread::spawn(move || hand_off(app));
    Ok(())
}

/// エンジンを止め、新しいシェルへ引き継いでプロセスを終える（専用のスレッド。終了の後始末は
/// 条件変数で待つので tokio のワーカーから呼ばない）。
fn hand_off(app: AppHandle) {
    crate::tray::show_quitting_status(&app, "運用モードへ切り替えています…");
    let report = crate::exit_cleanup(&app, crate::lifecycle::ExitTrigger::TrayQuit);
    if let Some(message) = report
        .as_ref()
        .and_then(crate::tray::cleanup_failure_message)
    {
        eprintln!("banto: {message}");
    }
    match spawn_successor() {
        Ok(()) => app.exit(0),
        Err(err) => {
            eprintln!("banto: 運用モードへの切り替えを続ける画面を起動できませんでした: {err}");
            let app_for_exit = app.clone();
            use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
            app.dialog()
                .message(format!(
                    "記録は止めましたが、切り替えを続ける画面を起動できませんでした（{err}）。ChronoGazer をもう一度起動して、設定画面の「運用モード」から切り替え直してください。"
                ))
                .title("運用モードへの切り替え")
                .kind(MessageDialogKind::Error)
                .buttons(MessageDialogButtons::OkCustom("OK".to_string()))
                .show(move |_| app_for_exit.exit(0));
        }
    }
}

/// 新しいシェルを、引き継ぎの引数つきで起動する。
fn spawn_successor() -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    std::process::Command::new(exe)
        .args(mode_switch::handoff_args(std::process::id()))
        .spawn()
        .map(drop)
}

// ---------------------------------------------------------------------------
// 運用モードへ（引き継いだ新しいシェル・[運用を再開]）
// ---------------------------------------------------------------------------

/// [運用を再開]（Fallback の窓）・引き継いだ新しいシェルの続き。
pub(crate) fn request_resume_operation(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let Some(guard) = take_ticket(&app) else {
            return;
        };
        run_to_operation(app, guard).await;
    });
}

async fn run_to_operation(app: AppHandle, guard: TransitionGuard) {
    show_switching(
        &app,
        SwitchDirection::ToOperation,
        SwitchStage::StartingService,
    );
    let Some(hooks) = hooks(&app) else {
        return;
    };
    let progress_app = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let manager = shell::service_manager(&hooks);
        start_service_and_wait(&manager, &RealClock, DEFAULT_TIMINGS, &mut |stage| {
            render_stage(&progress_app, SwitchDirection::ToOperation, stage)
        })
    })
    .await
    .unwrap_or_else(|err| StartOutcome::StartFailed(err.to_string()));
    eprintln!("banto: 運用モードへの切り替え: {outcome:?}");
    if outcome == StartOutcome::Running {
        render_stage(
            &app,
            SwitchDirection::ToOperation,
            SwitchStage::WaitingForService,
        );
        match wait_until_healthy(&app).await {
            Some(port) => shell::switch_to_service(&app, guard, port),
            None => {
                finish_fallback(
                    &app,
                    guard,
                    FallbackCause::AtStartup,
                    Some("常時記録は始まりましたが、まだ画面をつなげられません。つながり次第、自動でつなぎます。".to_string()),
                )
                .await
            }
        }
        return;
    }
    let (cause, notice) = after_start_failure(&outcome);
    finish_fallback(&app, guard, cause, notice).await;
}

/// サービスの接続を確かめられるまで待つ（上限つき。Healthy ならその port）。
async fn wait_until_healthy(app: &AppHandle) -> Option<u16> {
    for _ in 0..HEALTH_WAIT_ATTEMPTS {
        let (scm, health, _, _) = shell::observe_async(app).await?;
        match (&scm, &health) {
            (
                ScmObservation::State(ScmState::Running),
                Some(ServiceHealth::Healthy { port, .. }),
            ) => return Some(*port),
            (ScmObservation::State(ScmState::Running), _) => {}
            // 止まった・途中になった: 待たない（Fallback で今の状態を出す）。
            _ => return None,
        }
        tokio::time::sleep(HEALTH_WAIT_INTERVAL).await;
    }
    None
}

// ---------------------------------------------------------------------------
// 設定モードへ（トレイ・Fallback の窓）
// ---------------------------------------------------------------------------

/// トレイの「設定モードに切り替える」（Service の画面）。確かめてから始める。
pub(crate) fn request_to_setting_from_tray(app: &AppHandle) {
    let app_for_answer = app.clone();
    banto_desktop_tray::ask_confirm(
        app,
        banto_desktop_tray::ConfirmPrompt {
            title: "設定モードに切り替える",
            message: TO_SETTING_PROMPT,
            ok_label: "切り替える",
            cancel_label: "キャンセル",
        },
        move |confirmed| {
            if confirmed {
                request_to_setting(&app_for_answer);
            }
        },
    );
}

/// [設定モードに切り替える]・[設定モードに戻す]（Fallback の窓）とトレイの確認の後。
pub(crate) fn request_to_setting(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let Some(guard) = take_ticket(&app) else {
            return;
        };
        let from_service = matches!(guard.view(), Some(ShellView::Service { .. }));
        let Some((scm, _, _, _)) = shell::observe_async(&app).await else {
            drop(guard);
            return;
        };
        match to_setting_route(&scm) {
            ToSettingRoute::Stop => run_to_setting(app, guard, from_service).await,
            ToSettingRoute::Restore => run_restore_setting(app, guard).await,
            ToSettingRoute::Unknown => {
                drop(guard);
                shell::rerender_fallback(
                    &app,
                    Some(format!(
                        "常時記録の仕組みの状態が分からないため、切り替えられません（{}）。",
                        shell::describe_scm(&scm)
                    )),
                    false,
                );
            }
        }
    });
}

async fn run_to_setting(app: AppHandle, guard: TransitionGuard, from_service: bool) {
    if from_service && !replace_service_window(&app) {
        drop(guard);
        return;
    }
    show_switching(
        &app,
        SwitchDirection::ToSetting,
        SwitchStage::StoppingService,
    );
    push_log(&app, mode_switch::LOG_TO_SETTING_STARTED.to_string());
    let Some(hooks) = hooks(&app) else {
        return;
    };
    let progress_app = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let manager = shell::service_manager(&hooks);
        stop_service_and_wait(&manager, &RealClock, DEFAULT_TIMINGS, &mut |stage| {
            render_stage(&progress_app, SwitchDirection::ToSetting, stage)
        })
    })
    .await
    .unwrap_or_else(|err| StopOutcome::StopFailed(err.to_string()));
    eprintln!("banto: 設定モードへの切り替え: {outcome:?}");
    match after_stop(&outcome) {
        AfterStop::Decide { exit, log } => {
            if let Some(line) = log {
                push_log(&app, line);
            }
            enter_setting(app, guard, Some(exit)).await;
        }
        AfterStop::Failed { notice, log } => {
            push_log(&app, log);
            finish_fallback(&app, guard, FallbackCause::AtStartup, Some(notice)).await;
        }
    }
}

async fn run_restore_setting(app: AppHandle, guard: TransitionGuard) {
    show_switching(
        &app,
        SwitchDirection::ToSetting,
        SwitchStage::RelaxingPermissions,
    );
    push_log(&app, mode_switch::LOG_TO_SETTING_STARTED.to_string());
    let Some(hooks) = hooks(&app) else {
        return;
    };
    let progress_app = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let manager = shell::service_manager(&hooks);
        relax_only_and_wait(&manager, &RealClock, DEFAULT_TIMINGS, &mut |stage| {
            render_stage(&progress_app, SwitchDirection::ToSetting, stage)
        })
    })
    .await
    .unwrap_or_else(|err| RelaxOnlyOutcome::Refused(err.to_string()));
    eprintln!("banto: 設定モードに戻す（relax-only）: {outcome:?}");
    match after_relax(&outcome) {
        AfterRelax::Decide => {
            push_log(&app, mode_switch::log_restored_by("relax-only"));
            enter_setting(app, guard, Some(ServiceExit::Clean)).await;
        }
        AfterRelax::Running(notice) => {
            finish_fallback(&app, guard, FallbackCause::AtStartup, Some(notice)).await;
        }
        AfterRelax::Elevate => {
            render_stage(
                &app,
                SwitchDirection::ToSetting,
                SwitchStage::RepairingPermissions,
            );
            let repaired = tauri::async_runtime::spawn_blocking(|| {
                run_elev(ELEV_GRANT_DATA_ACL, "ファイルの権限の修復")
            })
            .await
            .unwrap_or_else(|err| Err(err.to_string()));
            match repaired {
                Ok(()) => {
                    push_log(&app, mode_switch::log_restored_by("管理者の許可で修復"));
                    enter_setting(app, guard, None).await;
                }
                Err(reason) => {
                    eprintln!("banto: 設定モードに戻せませんでした: {reason}");
                    finish_fallback(
                        &app,
                        guard,
                        FallbackCause::OperationStopped { left_tight: true },
                        Some(format!("設定モードに戻せませんでした。{reason}")),
                    )
                    .await;
                }
            }
        }
    }
}

/// サービスが止まった後: 「再試行」と同じ判定（`decide` の表）で次の画面へ。Desktop が動き
/// 始めたら、デスクトップのログに「完了」を足して書く。
async fn enter_setting(app: AppHandle, guard: TransitionGuard, exit: Option<ServiceExit>) {
    let Some((scm, health, mode, last_exit)) = shell::observe_async(&app).await else {
        drop(guard);
        return;
    };
    let last_exit = last_exit.or(exit);
    match shell::on_retry(
        FallbackCause::AtStartup,
        &scm,
        health.as_ref(),
        &mode,
        last_exit,
    ) {
        Step::ToDesktop => {
            render_stage(
                &app,
                SwitchDirection::ToSetting,
                SwitchStage::StartingDesktop,
            );
            let started = shell::switch_to_desktop(&app, guard).await;
            push_log(
                &app,
                if started {
                    mode_switch::LOG_TO_SETTING_COMPLETED.to_string()
                } else {
                    "設定モードに戻しましたが、この画面の記録エンジンを起動できませんでした（画面の案内を参照）".to_string()
                },
            );
            flush_log(&app);
        }
        Step::ToService { port } => shell::switch_to_service(&app, guard, port),
        Step::Refresh(state) => finish_with_state(&app, guard, state, None),
        Step::Stay | Step::ToFallback(_) => drop(guard),
    }
}

// ---------------------------------------------------------------------------
// 窓・札・ログの小さな部品
// ---------------------------------------------------------------------------

fn hooks(app: &AppHandle) -> Option<shell::ShellTestHooks> {
    app.try_state::<ShellState>()
        .map(|shell| shell.hooks.clone())
}

fn logs_dir(app: &AppHandle) -> Option<PathBuf> {
    app.try_state::<ShellState>()
        .map(|shell| shell.layout.logs_dir())
}

/// モードの切り替えの札（ライフサイクルの表の注 4）。断られたら窓に知らせる。
fn take_ticket(app: &AppHandle) -> Option<TransitionGuard> {
    let lifecycle = shell::lifecycle(app)?;
    match lifecycle.begin_mode_switch() {
        Ok((guard, _)) => Some(guard),
        Err(refusal) => {
            eprintln!("banto: モードの切り替えを断りました: {refusal:?}");
            shell::rerender_fallback(app, Some(refusal_message(refusal).to_string()), false);
            None
        }
    }
}

/// Service の画面の窓を、切り替えの窓（Fallback の窓）に置き換える（新しい窓を先に作る）。
fn replace_service_window(app: &AppHandle) -> bool {
    let Some(logs) = logs_dir(app) else {
        return false;
    };
    let page = shell::switching_page_view(
        SwitchDirection::ToSetting,
        SwitchStage::StoppingService,
        &logs,
    );
    // 利用者が選んだ切り替えなので、サービスの画面が隠れていても窓を出す。
    let hide_on_close = crate::tray::installed(app);
    if let Err(err) = shell::open_fallback_window(app, &page, true, hide_on_close) {
        eprintln!("banto: 切り替えの窓を作れませんでした: {err}");
        return false;
    }
    shell::destroy_window(app, SERVICE_WINDOW_LABEL);
    true
}

/// 切り替えの窓を出す（Fallback の窓があれば描き直し、無ければ作る）。
fn show_switching(app: &AppHandle, direction: SwitchDirection, stage: SwitchStage) {
    let Some(logs) = logs_dir(app) else {
        return;
    };
    let page = shell::switching_page_view(direction, stage, &logs);
    let hide_on_close = crate::tray::installed(app);
    if let Err(err) = shell::open_fallback_window(app, &page, true, hide_on_close) {
        eprintln!("banto: 切り替えの窓を出せませんでした: {err}");
    }
}

/// 段階を描き直す（SCM を待つスレッドから呼ぶ。窓が無ければ何もしない）。
fn render_stage(app: &AppHandle, direction: SwitchDirection, stage: SwitchStage) {
    let Some(logs) = logs_dir(app) else {
        return;
    };
    if let Some(window) = app.get_webview_window(FALLBACK_WINDOW_LABEL) {
        let page = shell::switching_page_view(direction, stage, &logs);
        if let Err(err) = window.eval(page.render_script()) {
            eprintln!("banto: 切り替えの窓を更新できませんでした: {err}");
        }
    }
}

/// 札を返して Fallback を置き、今の観測で描き直す。
async fn finish_fallback(
    app: &AppHandle,
    guard: TransitionGuard,
    cause: FallbackCause,
    notice: Option<String>,
) {
    let state = match shell::observe_async(app).await {
        Some((scm, health, _, _)) => FallbackState {
            cause: shell::carry_cause(cause, &scm),
            scm,
            health,
        },
        None => FallbackState {
            scm: ScmObservation::QueryFailed("照会できませんでした".to_string()),
            health: None,
            cause,
        },
    };
    finish_with_state(app, guard, state, notice);
}

fn finish_with_state(
    app: &AppHandle,
    guard: TransitionGuard,
    state: FallbackState,
    notice: Option<String>,
) {
    guard.finish(ShellView::Fallback(state));
    shell::rerender_fallback(app, notice, false);
    crate::tray::refresh(app);
}

/// デスクトップのログに書く行を覚えておく（起きた時刻で）。
fn push_log(app: &AppHandle, text: String) {
    if let Some(shell) = app.try_state::<ShellState>() {
        if let Ok(mut pending) = shell.pending_log.lock() {
            pending.push(DesktopLogEntry::now_system(text));
        }
    }
}

/// 覚えておいた行をデスクトップのログに書く（設定モードになってから）。
fn flush_log(app: &AppHandle) {
    let Some(shell) = app.try_state::<ShellState>() else {
        return;
    };
    let entries = match shell.pending_log.lock() {
        Ok(mut pending) => std::mem::take(&mut *pending),
        Err(_) => return,
    };
    if !entries.is_empty() {
        write_desktop_log(&shell.layout.logs_dir(), &entries);
    }
}

/// デスクトップのログに書く（失敗は標準エラー出力へ、行も一緒に出す）。
fn write_desktop_log(logs_dir: &std::path::Path, entries: &[DesktopLogEntry]) {
    if let Err(err) = mode_switch::append_desktop_log_now(logs_dir, entries) {
        eprintln!("banto: デスクトップのログに書けませんでした（{err}）");
        for entry in entries {
            eprintln!("banto: {}", entry.text);
        }
    }
}

/// 昇格ヘルパーを UAC で起動して待つ（呼ぶスレッドは終わるまで止まる）。
fn run_elev(action: &str, what: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        let exe = std::env::current_exe()
            .map_err(|err| format!("{what}ができませんでした（{err}）。"))?
            .with_file_name(ELEV_EXE_FILE_NAME);
        if !exe.is_file() {
            return Err(format!(
                "{what}ができませんでした（管理者用の補助プログラム {ELEV_EXE_FILE_NAME} が見つかりません）。"
            ));
        }
        elev_result_message(
            banto_service_elev::launch::run_elevated(&exe, &[action]),
            what,
        )
    }
    #[cfg(not(windows))]
    {
        let _ = (action, ELEV_EXE_FILE_NAME);
        Err(format!("{what}は Windows でだけ使えます。"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use banto_service_elev::launch::LaunchError;
    use std::time::Duration;

    fn summary(state: ScmState, auto_start: bool) -> ServiceStatusSummary {
        ServiceStatusSummary {
            state,
            auto_start,
            pid: None,
            checkpoint: 0,
            wait_hint: Duration::ZERO,
            win32_exit_code: 0,
            service_specific_exit_code: 0,
        }
    }

    #[test]
    fn handoff_continues_only_when_the_service_is_stopped() {
        let on = Handoff {
            to_operation: true,
            wait_pid: Some(1),
        };
        assert!(handoff_applies(
            on,
            &ScmObservation::State(ScmState::Stopped)
        ));
        for scm in [
            ScmObservation::State(ScmState::Running),
            ScmObservation::State(ScmState::StartPending),
            ScmObservation::State(ScmState::NotInstalled),
            ScmObservation::QueryFailed("x".into()),
            ScmObservation::NotWindows,
        ] {
            assert!(!handoff_applies(on, &scm), "{scm:?}");
        }
        assert!(!handoff_applies(
            Handoff::default(),
            &ScmObservation::State(ScmState::Stopped)
        ));
    }

    /// 前の確かめ: 停止中だけ進む。自動開始の elev は、チェックが入っていて今は手動のときだけ。
    #[test]
    fn precheck_allows_only_a_stopped_service_and_plans_auto_start() {
        let stopped_manual = Ok(summary(ScmState::Stopped, false));
        let stopped_auto = Ok(summary(ScmState::Stopped, true));
        assert_eq!(
            operation_precheck(Some(&stopped_manual), true),
            Ok(OperationPlan {
                enable_auto_start: true
            })
        );
        assert_eq!(
            operation_precheck(Some(&stopped_manual), false),
            Ok(OperationPlan {
                enable_auto_start: false
            })
        );
        assert_eq!(
            operation_precheck(Some(&stopped_auto), true),
            Ok(OperationPlan {
                enable_auto_start: false
            })
        );
        let not_installed = Ok(summary(ScmState::NotInstalled, false));
        assert!(operation_precheck(Some(&not_installed), true)
            .unwrap_err()
            .contains("インストーラ"));
        for state in [
            ScmState::Running,
            ScmState::StartPending,
            ScmState::StopPending,
        ] {
            let running = Ok(summary(state.clone(), false));
            assert!(
                operation_precheck(Some(&running), true)
                    .unwrap_err()
                    .contains("既に動いています"),
                "{state:?}"
            );
        }
        let failed = Err(ServiceManagerError::AccessDenied);
        assert!(operation_precheck(Some(&failed), true).is_err());
        assert!(operation_precheck(None, true)
            .unwrap_err()
            .contains("Windows"));
    }

    #[test]
    fn elev_results_become_plain_sentences() {
        assert_eq!(elev_result_message(Ok(0), "自動開始の設定"), Ok(()));
        assert!(elev_result_message(Ok(1), "自動開始の設定")
            .unwrap_err()
            .contains("終了コード 1"));
        assert!(
            elev_result_message(Err(LaunchError::ShellExecute(1223)), "自動開始の設定")
                .unwrap_err()
                .contains("管理者の許可が得られませんでした")
        );
        assert!(
            elev_result_message(Err(LaunchError::ShellExecute(2)), "自動開始の設定")
                .unwrap_err()
                .contains("os error 2")
        );
        assert!(auto_start_failure_message("x。").contains("チェックを外す"));
    }

    /// 運用モードへの開始の結果ごとの次の画面: 4 は「運用モードにできませんでした」、5 は
    /// 「運用モードのまま残っています」（[設定モードに戻す]）、ほかは知らせつきの Fallback。
    #[test]
    fn start_failures_choose_the_page() {
        assert_eq!(
            after_start_failure(&StartOutcome::Ended(ServiceExit::CannotOperate)),
            (FallbackCause::CannotOperate, None)
        );
        assert_eq!(
            after_start_failure(&StartOutcome::Ended(ServiceExit::LeftOperation)),
            (FallbackCause::OperationStopped { left_tight: true }, None)
        );
        for outcome in [
            StartOutcome::Ended(ServiceExit::Clean),
            StartOutcome::Ended(ServiceExit::Other {
                win32: 1066,
                specific: 2,
            }),
            StartOutcome::NotInstalled,
            StartOutcome::StartFailed("権限".into()),
            StartOutcome::Unexpected(ScmState::NotInstalled),
            StartOutcome::Stalled,
            StartOutcome::TimedOut,
        ] {
            let (cause, notice) = after_start_failure(&outcome);
            assert_eq!(cause, FallbackCause::AtStartup, "{outcome:?}");
            assert!(notice.is_some(), "{outcome:?}");
        }
    }

    /// 設定モードへの停止の結果: 止まったら（終了コードに関わらず）`decide` の表へ。5 は
    /// 「緩められませんでした」をログに足す。止められなければ中断をログに足して知らせる。
    #[test]
    fn stop_outcomes_decide_or_fail() {
        assert_eq!(
            after_stop(&StopOutcome::Stopped(ServiceExit::Clean)),
            AfterStop::Decide {
                exit: ServiceExit::Clean,
                log: None
            }
        );
        assert_eq!(
            after_stop(&StopOutcome::Stopped(ServiceExit::LeftOperation)),
            AfterStop::Decide {
                exit: ServiceExit::LeftOperation,
                log: Some(mode_switch::LOG_RELAX_FAILED.to_string())
            }
        );
        assert!(matches!(
            after_stop(&StopOutcome::Stopped(ServiceExit::CannotOperate)),
            AfterStop::Decide {
                exit: ServiceExit::CannotOperate,
                log: Some(_)
            }
        ));
        for outcome in [
            StopOutcome::NotInstalled,
            StopOutcome::StopFailed("権限".into()),
            StopOutcome::Unexpected(ScmState::Running),
            StopOutcome::Stalled,
            StopOutcome::TimedOut,
        ] {
            match after_stop(&outcome) {
                AfterStop::Failed { notice, log } => {
                    assert!(log.contains("中断"), "{outcome:?}");
                    assert!(log.contains(&notice), "{outcome:?}");
                }
                other => panic!("{outcome:?} -> {other:?}"),
            }
        }
    }

    /// 緩めるだけの開始で戻らなければ elev の修復（UAC）。サービスが動いているときは修復しない。
    #[test]
    fn relax_only_falls_back_to_elev_except_while_running() {
        assert_eq!(
            after_relax(&RelaxOnlyOutcome::Done(ServiceExit::Clean)),
            AfterRelax::Decide
        );
        for outcome in [
            RelaxOnlyOutcome::Done(ServiceExit::LeftOperation),
            RelaxOnlyOutcome::Done(ServiceExit::Other {
                win32: 1067,
                specific: 0,
            }),
            RelaxOnlyOutcome::NotInstalled,
            RelaxOnlyOutcome::Refused("権限".into()),
            RelaxOnlyOutcome::Unexpected(ScmState::Running),
            RelaxOnlyOutcome::Stalled,
            RelaxOnlyOutcome::TimedOut,
        ] {
            assert_eq!(after_relax(&outcome), AfterRelax::Elevate, "{outcome:?}");
        }
        assert!(matches!(
            after_relax(&RelaxOnlyOutcome::NotStopped(ScmState::Running)),
            AfterRelax::Running(_)
        ));
    }

    #[test]
    fn to_setting_stops_a_running_service_and_restores_a_stopped_one() {
        for state in [
            ScmState::Running,
            ScmState::StartPending,
            ScmState::StopPending,
        ] {
            assert_eq!(
                to_setting_route(&ScmObservation::State(state)),
                ToSettingRoute::Stop
            );
        }
        for state in [ScmState::Stopped, ScmState::NotInstalled] {
            assert_eq!(
                to_setting_route(&ScmObservation::State(state)),
                ToSettingRoute::Restore
            );
        }
        for scm in [
            ScmObservation::State(ScmState::Other("Paused".into())),
            ScmObservation::QueryFailed("x".into()),
            ScmObservation::NotWindows,
        ] {
            assert_eq!(to_setting_route(&scm), ToSettingRoute::Unknown);
        }
    }

    #[test]
    fn user_facing_texts_mention_the_pause_and_avoid_jargon() {
        assert!(TO_SETTING_PROMPT.contains(shell::PAUSE_NOTE));
        for refusal in [
            Refusal::Busy,
            Refusal::Exiting,
            Refusal::AlreadyConfirming,
            Refusal::RecoveryInProgress,
            Refusal::NotDesktop,
        ] {
            assert!(!refusal_message(refusal).is_empty());
        }
    }
}
