//! トレイ常駐（#392 A2、docs/plan.md §5「#392 収集エンジンの UI 独立」H6 の
//! A）。**ウィンドウを閉じても収集を止めない**。
//!
//! # 振る舞い
//!
//! - メインの窓の × は終了ではなく**隠す**（トレイへ格納）。初回だけ OS 通知で
//!   「トレイで動作を続けます」と知らせる。収集・Hub 購読・LAN サーバーは
//!   そのまま動き続ける（どれも窓とは無関係にプロセスの中で動いている）。
//! - トレイのアイコンの**左クリック**か、メニューの「開く」で窓を戻す。
//!   右クリックでメニュー（収集の状態・開く・終了）。
//! - 「終了」は確認ダイアログを出し（収集中なら「収集を停止してから終了」の
//!   文言）、同意されたら**既存の終了処理**（[`crate::exit_cleanup_once`] →
//!   `shutdown_app_state`: LAN サーバー → 収集（最終 flush と
//!   `collection_stopped`）→ Hub 購読 → DB の順）を通してから
//!   `AppHandle::exit` する。
//! - 2 つ目の起動は `tauri-plugin-single-instance` が受け、既存の窓を前に出して
//!   2 つ目のプロセスは終わる（`run()` の登録箇所の注記）。
//!
//! 窓の前面化・× で隠す・初回通知・確認ダイアログ・終了の二重実行防止は
//! banto-hub と共有する `banto-desktop-tray`（`crates/`）の部品を使う。
//! ここにあるのは ChronoGazer 固有の部分（収集の状態の文言・メニュー・
//! 終了処理の呼び出しと失敗の知らせ方）だけ。
//!
//! # 安全・並行の扱い
//!
//! - **確認中に収集の状態が変わる**: 確認の文言は開いた時点の状態で選ぶが、
//!   **終了処理は状態に関係なく必ず収集の停止を通す**（`stop()` は走って
//!   いなければ何もしない）。さらに「止まっている」文言で同意を取った後に
//!   収集が始まっていたら、黙って止めずに「収集中」の文言でもう一度確かめる
//!   （[`banto_desktop_tray::decide_answer`] の `AskAgain`）。
//! - **終了処理中にもう一度「終了」**: [`banto_desktop_tray::QuitGate`] が
//!   確認ダイアログを 2 つ出さず、終了処理を 2 回走らせない。終了処理の本体も
//!   [`crate::exit_cleanup_once`] が 1 回に限る（OS のシャットダウンで
//!   `RunEvent::Exit` が同時に来ても、走っている後始末の完了を待つだけ）。
//! - **停止の失敗**: 失敗（最終 flush の失敗・予算切れ）を握りつぶさず、
//!   何が起きたかをダイアログで知らせてから終了する。**プロセスは残さない**
//!   （banto-hub の「アプリを終了」も停止の結果にかかわらず必ず終了する）。
//!   残しても回復の手段が無い - 失敗した `stop()` でもエンジンは止まって
//!   おり（`CollectorService::stop` の doc）、DB プールは既に閉じている。
//! - **終了処理中は窓を開かない**: 閉じた DB の上で画面を操作させないため、
//!   終了処理に入ったら窓を隠し、「開く」・左クリック・2 つ目の起動を無視する。

use std::time::Duration;

use banto_desktop_tray::{
    ConfirmPrompt, QuitAnswerDecision, QuitGate, QuitPhase, QuitRequestDecision, TrayHint,
};
use chronogazer_core::collect::CollectorState;
use tauri::menu::{Menu, MenuBuilder, MenuItemBuilder};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{AppHandle, Manager, Wry};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

use crate::{AppState, ExitCleanupReport, LEGACY_DB_WINDOW_LABEL, MAIN_WINDOW_LABEL};

/// × で隠した初回だけ出す通知（既読の印はアプリのデータディレクトリの
/// `tray-hint-shown`。banto-hub と同じ名前だが、データディレクトリが
/// アプリごとに別なので混ざらない）。
pub(crate) const TRAY_HINT: TrayHint = TrayHint {
    flag_file_name: "tray-hint-shown",
    title: "ChronoGazer",
    body: "ChronoGazer はタスクトレイで動作を続けます（収集も続きます）。終了するにはトレイのアイコンを右クリックして「終了」を選んでください。",
};

/// トレイの状態表示を読み直す間隔。読むのはメモリ上の状態だけ
/// （`CollectorService::state` はネットワークもディスクも DB も触らない）で、
/// トレイを書き換えるのは文言が変わったときだけ。
const STATUS_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// 終了の確認に「収集中」の文言を使うか（= 止めるべき収集があるか）。
/// 開始中も含める（開始処理は止めないと走り切ってしまう）。対象 0 件・
/// 開始失敗・停止中はエンジンが無いので含めない。
pub(crate) fn is_collecting(state: &CollectorState) -> bool {
    matches!(
        state,
        CollectorState::Running { .. } | CollectorState::Starting
    )
}

/// トレイのメニューの 1 行目と tooltip に出す、収集の状態の文言。
/// 起動失敗の理由は出さない（パスや接続先を含みうる。理由は画面の収集の
/// 状態表示で見る）。
pub(crate) fn status_label(state: &CollectorState) -> String {
    match state {
        CollectorState::Stopped => "収集停止中".to_string(),
        CollectorState::Starting => "収集を開始しています".to_string(),
        CollectorState::Running {
            groups,
            tags,
            exclusions,
        } => {
            if exclusions.is_empty() {
                format!("収集中（{groups} グループ・{tags} タグ）")
            } else {
                format!(
                    "収集中（{groups} グループ・{tags} タグ、除外 {} 件）",
                    exclusions.len()
                )
            }
        }
        CollectorState::NoTargets { .. } => "収集対象なし".to_string(),
        CollectorState::StartFailed { .. } => "収集の開始に失敗（画面で理由を確認）".to_string(),
    }
}

pub(crate) fn tooltip_text(label: &str) -> String {
    format!("ChronoGazer — {label}")
}

/// 終了の確認ダイアログの本文。
pub(crate) fn quit_prompt_message(collecting: bool) -> &'static str {
    if collecting {
        "収集を停止し、記録中のデータを書き出してから ChronoGazer を終了します。終了すると、次に起動するまで収集は行われません。よろしいですか?"
    } else {
        "ChronoGazer を終了します。よろしいですか?"
    }
}

/// 終了処理が完全には済まなかったときに利用者へ出す文。問題が無ければ
/// `None`（黙って終了する）。
pub(crate) fn cleanup_failure_message(report: &ExitCleanupReport) -> Option<String> {
    const LOSS: &str =
        "最後に記録した値の一部がファイルに書き出されていない可能性があります。ChronoGazer を終了します。";
    if report.timed_out {
        return Some(format!(
            "終了処理が {} 秒以内に終わらなかったため、途中で切り上げました。{LOSS}",
            report.budget_secs
        ));
    }
    report
        .collect_stop_error
        .as_ref()
        .map(|err| format!("収集の停止中にエラーが発生しました: {err}\n\n{LOSS}"))
}

/// トレイを作って管理状態に入れ、状態表示の見張りを起こす。`setup()` の
/// 最後（`AppState` を manage した後）に 1 回だけ呼ぶ。
pub(crate) fn install(app: &tauri::App) -> tauri::Result<()> {
    let handle = app.handle();
    let label = current_label(handle);
    let menu = build_menu(handle, &label)?;
    // 左クリックは窓を戻す（メニューは右クリック）。
    let builder = TrayIconBuilder::new()
        .icon(
            app.default_window_icon()
                .cloned()
                .expect("tauri.conf.json の bundle.icon で既定アイコンを設定済み"),
        )
        .tooltip(tooltip_text(&label))
        .menu(&menu)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => open_main_window(app),
            "quit" => request_quit(app),
            _ => {}
        });
    let tray = banto_desktop_tray::restore_window_on_left_click(builder, open_main_window)
        .build(handle)?;
    // `TrayIcon` は最後の参照が drop されると消えるので、プロセスの終わりまで
    // 管理状態で持つ（banto-hub と同じ）。
    app.manage(tray);

    tauri::async_runtime::spawn(watch_status(handle.clone(), label));
    Ok(())
}

/// 窓を戻す（トレイの「開く」・左クリック・2 つ目の起動）。終了処理に
/// 入っていたら何もしない。メインの窓が無い（起動を止めてエラーの窓を
/// 出している）ときはエラーの窓を前に出す。
pub(crate) fn open_main_window(app: &AppHandle) {
    if quitting(app) {
        return;
    }
    banto_desktop_tray::show_window(app, &[MAIN_WINDOW_LABEL, LEGACY_DB_WINDOW_LABEL]);
}

fn quitting(app: &AppHandle) -> bool {
    app.try_state::<QuitGate>()
        .map(|gate| gate.phase() == QuitPhase::Quitting)
        .unwrap_or(false)
}

fn current_state(app: &AppHandle) -> Option<CollectorState> {
    app.try_state::<AppState>()
        .map(|state| state.collect.state())
}

fn current_label(app: &AppHandle) -> String {
    current_state(app)
        .map(|state| status_label(&state))
        .unwrap_or_else(|| "起動中".to_string())
}

fn currently_collecting(app: &AppHandle) -> bool {
    current_state(app)
        .map(|state| is_collecting(&state))
        .unwrap_or(false)
}

fn build_menu(app: &AppHandle, label: &str) -> tauri::Result<Menu<Wry>> {
    let status = MenuItemBuilder::with_id("status", label)
        .enabled(false)
        .build(app)?;
    MenuBuilder::new(app)
        .item(&status)
        .separator()
        .text("open", "開く")
        .text("quit", "終了")
        .build()
}

fn apply_label(app: &AppHandle, label: &str) {
    let Some(tray) = app.try_state::<TrayIcon<Wry>>() else {
        return;
    };
    let _ = tray.set_tooltip(Some(tooltip_text(label)));
    if let Ok(menu) = build_menu(app, label) {
        let _ = tray.set_menu(Some(menu));
    }
}

/// 収集の状態をトレイへ映す見張り。`install` から 1 回だけ起こす（二重起動
/// しない）。文言が変わったときだけトレイを書き換える。終了処理に入ったら
/// 抜ける（「終了処理中」の表示を上書きしないため）。プロセスが終われば
/// ランタイムごと消える。
async fn watch_status(app: AppHandle, mut shown: String) {
    loop {
        tokio::time::sleep(STATUS_POLL_INTERVAL).await;
        let label = current_label(&app);
        // 「終了中か」の確認と書き換えを、`quit` の「終了処理中」の書き換えと
        // 同じロックの中で行う - 確認の直後に終了へ入られても、古い状態で
        // 「終了処理中」を上書きしない。
        let _guard = TRAY_LABEL_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        if quitting(&app) {
            break;
        }
        if label != shown {
            apply_label(&app, &label);
            shown = label;
        }
    }
}

/// トレイの文言の書き換えを、見張りと `quit` の間で直列にするロック
/// （[`watch_status`] の注記）。
static TRAY_LABEL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// トレイの「終了」。
fn request_quit(app: &AppHandle) {
    let Some(gate) = app.try_state::<QuitGate>() else {
        return;
    };
    match gate.request() {
        QuitRequestDecision::ShowPrompt => prompt_quit(app, currently_collecting(app)),
        // 確認ダイアログは出ている／終了処理は走っている - どちらも 2 つ目は
        // 作らない。
        QuitRequestDecision::AlreadyConfirming | QuitRequestDecision::AlreadyQuitting => {}
    }
}

fn prompt_quit(app: &AppHandle, prompted_collecting: bool) {
    let app_for_answer = app.clone();
    banto_desktop_tray::ask_confirm(
        app,
        ConfirmPrompt {
            title: "ChronoGazer を終了",
            message: quit_prompt_message(prompted_collecting),
            ok_label: "終了",
            cancel_label: "キャンセル",
        },
        move |confirmed| {
            let app = app_for_answer;
            let now_collecting = currently_collecting(&app);
            let Some(gate) = app.try_state::<QuitGate>() else {
                return;
            };
            match gate.answer(confirmed, prompted_collecting, now_collecting) {
                QuitAnswerDecision::Proceed => quit(app.clone()),
                QuitAnswerDecision::AskAgain => prompt_quit(&app, true),
                QuitAnswerDecision::Cancelled | QuitAnswerDecision::Ignored => {}
            }
        },
    );
}

/// 確認を通った後の終了。窓を隠し、トレイを「終了処理中」にしてから、
/// 既存の終了処理を 1 回だけ通す。済まなかったことがあれば知らせてから
/// 終了する。
fn quit(app: AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        let _ = window.hide();
    }
    {
        let _guard = TRAY_LABEL_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        apply_label(&app, "終了処理中…");
    }
    tauri::async_runtime::spawn(async move {
        let report = crate::exit_cleanup_once(&app).await;
        let failure = report.as_ref().and_then(cleanup_failure_message);
        match failure {
            None => app.exit(0),
            Some(message) => {
                eprintln!("banto: {message}");
                let app_for_exit = app.clone();
                app.dialog()
                    .message(message)
                    .title("ChronoGazer を終了")
                    .kind(MessageDialogKind::Error)
                    .buttons(MessageDialogButtons::OkCustom("OK".to_string()))
                    .show(move |_| app_for_exit.exit(0));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running(groups: usize, tags: usize, exclusions: usize) -> CollectorState {
        CollectorState::Running {
            groups,
            tags,
            exclusions: (0..exclusions).map(|_| dummy_exclusion()).collect(),
        }
    }

    fn dummy_exclusion() -> chronogazer_core::collect::ExclusionView {
        chronogazer_core::collect::ExclusionView {
            unit: chronogazer_core::collect::ExclusionUnitView::Tag,
            id: 1,
            key: "t".into(),
            name: "t".into(),
            reason: "invalid",
            message: "x".into(),
        }
    }

    /// 終了の確認に「収集中」の文言を使うのは、走っている／開始中のときだけ。
    #[test]
    fn collecting_states() {
        assert!(is_collecting(&running(1, 2, 0)));
        assert!(is_collecting(&CollectorState::Starting));
        assert!(!is_collecting(&CollectorState::Stopped));
        assert!(!is_collecting(&CollectorState::NoTargets {
            exclusions: vec![]
        }));
        assert!(!is_collecting(&CollectorState::StartFailed {
            reason: "x".into()
        }));
    }

    /// 状態の文言。起動失敗の理由（パスを含みうる）は出さない。
    #[test]
    fn labels() {
        assert_eq!(status_label(&CollectorState::Stopped), "収集停止中");
        assert_eq!(
            status_label(&running(2, 15, 0)),
            "収集中（2 グループ・15 タグ）"
        );
        assert_eq!(
            status_label(&running(2, 15, 3)),
            "収集中（2 グループ・15 タグ、除外 3 件）"
        );
        assert_eq!(
            status_label(&CollectorState::NoTargets { exclusions: vec![] }),
            "収集対象なし"
        );
        let failed = status_label(&CollectorState::StartFailed {
            reason: r"C:\secret\data を開けません".into(),
        });
        assert!(!failed.contains("secret"), "{failed}");
        assert_eq!(tooltip_text("収集停止中"), "ChronoGazer — 収集停止中");
    }

    #[test]
    fn quit_prompt_mentions_stopping_only_when_collecting() {
        assert!(quit_prompt_message(true).contains("収集を停止"));
        assert!(!quit_prompt_message(false).contains("収集"));
    }

    /// 終了処理の結果: 問題が無ければ黙って終わり、最終 flush の失敗・予算切れは
    /// 「止めた」と言わずにそのまま知らせる。
    #[test]
    fn cleanup_failure_messages() {
        let ok = ExitCleanupReport::default();
        assert_eq!(cleanup_failure_message(&ok), None);

        let flush_failed = ExitCleanupReport {
            collect_stop_error: Some("disk full".into()),
            ..ExitCleanupReport::default()
        };
        let message = cleanup_failure_message(&flush_failed).expect("失敗は知らせる");
        assert!(message.contains("disk full"), "{message}");
        assert!(message.contains("書き出されていない可能性"), "{message}");

        let timed_out = ExitCleanupReport {
            timed_out: true,
            budget_secs: 5,
            ..ExitCleanupReport::default()
        };
        let message = cleanup_failure_message(&timed_out).expect("予算切れは知らせる");
        assert!(message.contains("5 秒"), "{message}");
    }
}
