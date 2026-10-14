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
//!   文言）、同意されたら**既存の終了処理**（[`crate::exit_cleanup`] →
//!   エンジンの終了 `RunningEngine::shutdown`: LAN サーバー → 収集（最終 flush と
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
//! - **同意と収集の開始の競合**（#392 A2 レビュー、2026-10-12）: 同意の後に
//!   状態を読んでから終了を確定するまでの間に、REST・Tauri の開始・自動開始が
//!   割り込むと、古い「止まっている」で進んで新しい収集を黙って止めてしまう。
//!   これは収集側の「終了の受付」（`CollectorService::begin_quit_confirm`、
//!   `chronogazer_core::collect` の doc「終了の受付」）で直列化する: 答えの
//!   コールバックは状態を自分で読まず、[`CollectorQuitAdmission`] を通して
//!   **開始の受付と同じロックの中で印を立てて**忙しいかを読む。印が立った
//!   後の開始は「終了処理中のため、収集を開始できません」のエラーで断られ、
//!   確かめ直すとき（`AskAgain`）は印を下ろす（上げ下げは
//!   [`QuitGate::answer`] が行う）。ここで扱うのは**確認のための一時的な
//!   印**だけで、終了処理（[`crate::exit_cleanup`] →
//!   エンジンの終了、OS のシャットダウンを含む）は取り消せない印
//!   （`CollectorService::begin_shutdown`）を立てる。遅れて届いた `AskAgain`
//!   は後者を下ろせない（#392 A2 再レビュー P2-2）。
//! - **終了処理中にもう一度「終了」**: [`banto_desktop_tray::QuitGate`] が
//!   確認ダイアログを 2 つ出さず、終了処理を 2 回走らせない。終了処理の本体も
//!   [`crate::exit_cleanup`] が 1 回に限る（OS のシャットダウンで
//!   `RunEvent::Exit` が同時に来ても、走っている後始末の完了を待つだけ）。
//! - **停止の失敗**: 失敗（最終 flush の失敗・予算切れ）を握りつぶさず、
//!   何が起きたかをダイアログで知らせてから終了する。**プロセスは残さない**
//!   （banto-hub の「アプリを終了」も停止の結果にかかわらず必ず終了する）。
//!   残しても回復の手段が無い - 失敗した `stop()` でもエンジンは止まって
//!   おり（`CollectorService::stop` の doc）、DB プールは既に閉じている。
//! - **終了処理中は窓を開かない**: 閉じた DB の上で画面を操作させないため、
//!   終了処理に入ったら窓を隠し、「開く」・左クリック・2 つ目の起動を無視する。
//!
//! # 3 つの態（#392 B2-4c、[`TrayMode`]・[`menu_spec`]）
//!
//! シェルの画面（[`crate::shell`]）に合わせてメニューを変える。トレイは 1 つだけで、
//! 画面が変わったら作り直さずにメニューと tooltip を差し替える（[`refresh`]。
//! [`install`] も 2 回目以降は差し替えるだけ）。
//!
//! | 態       | 1 行目（押せない）                     | 項目                                 |
//! | -------- | -------------------------------------- | ------------------------------------ |
//! | Desktop  | 収集の状態（A2 のまま）                | 開く・終了                           |
//! | Service  | 「サービス: 実行中」                   | 開く・終了（サービスは止まりません） |
//! | Fallback | 「サービス: <状態>（接続できません）」 | 開く・再試行・終了                   |
//!
//! サービスの開始・停止はトレイに出さない（O4、B2-5）。Service の「終了」は確認の文で
//! 「サービスは止まらず収集を続ける」と知らせる。Fallback の「終了」は止めるものが
//! 無いので確認しない（**判断**。ただし確認の答えと同じ受付（[`QuitGate`]）を通すので、
//! 「再試行」で Desktop が起動し終えて収集中になっていれば、収集中の文言で確かめ直す）。
//! どの態でも終了処理は [`crate::exit_cleanup`] を通る（シェルの移り変わりの終わりを
//! 待ち、エンジンがあれば止める）。待つ上限は、トレイ・Fallback の「終了」では移り変わりを
//! 60 秒（専用のスレッドで待つので画面は塞がない）、OS の終了では後始末と合わせて 1 つの
//! 5 秒の期限（`crate::exit_budgets`、PR #608 の全体監査 F2・F3）。
//!
//! # メインスレッドとロック（PR #608 の Copilot レビュー）
//!
//! トレイのメニューの作成・差し替え・tooltip は、Windows ではメインスレッド（イベント
//! ループ）で行われ、他のスレッドから呼ぶとメインスレッドの処理を**待つ**。一方、
//! メニューの選択・窓の `on_navigation`・確認ダイアログの答えはメインスレッドから
//! 来る。そこで次を約束にする:
//!
//! - **ロックを握ったままトレイの API を呼ばない**（メインスレッドを待たない）。以前は
//!   見張りと `quit` の書き換えを 1 つのロックで直列にしていたが、見張りがロックを
//!   握ったままメインスレッドを待ち、メインスレッドの `quit` が同じロックを待つと
//!   止まる。書き換えは [`post_spec`] で**メインスレッドへ渡すだけ**（待たない）にし、
//!   ロックをやめた。書き換えはすべてメインスレッドの上で順に走るので、「終了処理中」の
//!   後に古い表示が来ても、その時点で終了の段階を見て何もしない（[`should_apply`]）。
//! - **メインスレッドから来た終了は、重い処理を別のスレッドへ渡す**: 確認を省く態
//!   （Fallback）の終了は確定の手順ごと別のスレッドで行い、後始末（移り変わり・エンジンの
//!   停止を待つ）は今までどおり専用のスレッドで行う。メインスレッドの上で取るロックは
//!   `QuitGate`・`ShellGate`・収集の受付の短いものだけで、どれも中でメインスレッドを
//!   待たない（`ShellGate` の `admit` の中の `manage` もメインスレッドを使わない）。
//!
//! この並行はモックのランタイムでは再現できないので単体テストは無い（構造で防ぐ:
//! トレイの書き換えに使うロック自体が無い）。書き換えの判断だけを純関数でテストする。

use std::time::Duration;

use banto_desktop_tray::{
    ConfirmPrompt, QuitAdmission, QuitAnswerDecision, QuitGate, QuitPhase, QuitRequestDecision,
    TrayHint,
};
use chronogazer_core::collect::{CollectorService, CollectorState};
use tauri::menu::{Menu, MenuBuilder, MenuItemBuilder};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{AppHandle, Manager, Wry};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

use crate::shell::{FALLBACK_WINDOW_LABEL, SERVICE_WINDOW_LABEL};
use crate::startup::RECOVERY_WINDOW_LABEL;
use crate::{AppState, ExitCleanupReport, ExitTrigger, LEGACY_DB_WINDOW_LABEL, MAIN_WINDOW_LABEL};

/// × で隠した初回だけ出す通知（既読の印はアプリのデータディレクトリの
/// `tray-hint-shown`。banto-hub と同じ名前だが、データディレクトリが
/// アプリごとに別なので混ざらない）。
///
/// #392 B2-4b でエンジンのデータはデータのルート（`%ProgramData%\ChronoGazer`）へ
/// 移ったが、この印は利用者ごとの画面の状態なので、共有 crate
/// （`banto_desktop_tray::notify_first_hide_once`）が置く `app_data_dir()`
/// （`%APPDATA%\dev.tyaro.chronogazer`）のまま。旧 DB が無ければ、フォルダが
/// あっても旧データの案内（`crate::legacy_notice`）は出ない。
pub(crate) const TRAY_HINT: TrayHint = TrayHint {
    flag_file_name: "tray-hint-shown",
    title: "ChronoGazer",
    body: "ChronoGazer はタスクトレイで動作を続けます（収集も続きます）。終了するにはトレイのアイコンを右クリックして「終了」を選んでください。",
};

/// サービスの画面の × で隠した初回の通知（印は [`TRAY_HINT`] と共有 = 利用者ごとに
/// 1 回だけ）。収集を続けるのはサービスなので、そう書く。
pub(crate) const SERVICE_TRAY_HINT: TrayHint = TrayHint {
    flag_file_name: TRAY_HINT.flag_file_name,
    title: "ChronoGazer",
    body: "ChronoGazer の画面はタスクトレイに格納されました（記録エンジンのサービスは収集を続けます）。終了するにはトレイのアイコンを右クリックして「終了」を選んでください。",
};

/// 接続できない状態の窓の × で隠した初回の通知（印は [`TRAY_HINT`] と共有）。
pub(crate) const FALLBACK_TRAY_HINT: TrayHint = TrayHint {
    flag_file_name: TRAY_HINT.flag_file_name,
    title: "ChronoGazer",
    body: "ChronoGazer はタスクトレイで動作を続けます（サービスに接続できるようになると自動でつなぎます）。終了するにはトレイのアイコンを右クリックして「終了」を選んでください。",
};

/// トレイの態（モジュール doc「3 つの態」）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TrayMode {
    /// プロセスの中でエンジンを動かしている（A2 のまま）。
    Desktop,
    /// サービスの画面へつないでいる。
    Service,
    /// 接続できない状態を見せている（`scm` は SCM の状態の短い文言）。
    Fallback { scm: String },
}

/// メニューの 1 項目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MenuEntry {
    /// 押せない状態の行（tooltip にも使う）。
    Status(String),
    Separator,
    Item {
        id: &'static str,
        text: &'static str,
    },
}

/// メニューの組み立て（純関数。モジュール doc の表）。`label` は Desktop の収集の
/// 状態の文言（ほかの態では使わない - このプロセスは収集していない）。
pub(crate) fn menu_spec(mode: &TrayMode, label: &str) -> Vec<MenuEntry> {
    let open = MenuEntry::Item {
        id: "open",
        text: "開く",
    };
    match mode {
        TrayMode::Desktop => vec![
            MenuEntry::Status(label.to_string()),
            MenuEntry::Separator,
            open,
            MenuEntry::Item {
                id: "quit",
                text: "終了",
            },
        ],
        TrayMode::Service => vec![
            MenuEntry::Status("サービス: 実行中".to_string()),
            MenuEntry::Separator,
            open,
            MenuEntry::Item {
                id: "quit",
                text: "終了（サービスは止まりません）",
            },
        ],
        TrayMode::Fallback { scm } => vec![
            MenuEntry::Status(format!("サービス: {scm}（接続できません）")),
            MenuEntry::Separator,
            open,
            MenuEntry::Item {
                id: "retry",
                text: "再試行",
            },
            MenuEntry::Item {
                id: "quit",
                text: "終了",
            },
        ],
    }
}

/// メニューの 1 行目（tooltip に出す）。
fn status_of(spec: &[MenuEntry]) -> &str {
    spec.iter()
        .find_map(|entry| match entry {
            MenuEntry::Status(text) => Some(text.as_str()),
            _ => None,
        })
        .unwrap_or("")
}

/// 終了の前に確認するか（Fallback は止めるものが無いので確認しない）。
pub(crate) fn needs_quit_confirm(mode: &TrayMode) -> bool {
    !matches!(mode, TrayMode::Fallback { .. })
}

/// 態に合わせた終了の確認の本文。Desktop と、収集中で確かめ直すときは
/// [`quit_prompt_message`]。
pub(crate) fn quit_prompt_for(mode: &TrayMode, collecting: bool) -> &'static str {
    match mode {
        TrayMode::Service if !collecting => {
            "ChronoGazer の画面を終了します。記録エンジン（サービス）は止まらず、収集を続けます。よろしいですか?"
        }
        _ => quit_prompt_message(collecting),
    }
}

/// トレイの状態表示を読み直す間隔。読むのはメモリ上の状態だけ
/// （`CollectorService::state` はネットワークもディスクも DB も触らない）で、
/// トレイを書き換えるのは文言が変わったときだけ。
const STATUS_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// 終了の確認に「収集中」の文言を使うか（= 止めるべき収集があるか）。
/// 判定は収集側（[`CollectorState::is_collecting`]）と同じもの - 終了の
/// 受付（`CollectorService::begin_quit_confirm`）の「忙しい」と食い違わないように。
pub(crate) fn is_collecting(state: &CollectorState) -> bool {
    state.is_collecting()
}

/// 確認の答えで [`QuitGate::answer`] に渡す、終了の受付の口（このモジュールの
/// doc「同意と収集の開始の競合」）。`AppState` が無い（起動を止めてエラーの
/// 窓を出している）ときは止める収集が無いので、常に暇で印も無い。
struct CollectorQuitAdmission(Option<CollectorService>);

impl QuitAdmission for CollectorQuitAdmission {
    fn begin_quit_confirm(&self) -> bool {
        self.0
            .as_ref()
            .map(CollectorService::begin_quit_confirm)
            .unwrap_or(false)
    }

    fn cancel_quit_confirm(&self) {
        if let Some(collect) = &self.0 {
            collect.cancel_quit_confirm();
        }
    }
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

/// トレイを作って管理状態に入れ、状態表示の見張りを起こす。起動
/// （[`crate::launch_desktop`]）の最後（`AppState` を manage した後）に 1 回だけ
/// 呼ぶ。失敗は起動の失敗としてエラーの窓に出る（#392、panic しない）。
///
/// #392 B2-4c: シェルの態（[`TrayMode`]）に合わせて作る。Service・Fallback の起動では
/// 窓より先に作る。2 回目以降（Fallback の「再試行」で Desktop が起動した）は
/// 作り直さず、今の態に差し替えるだけ。
pub(crate) fn install(app: &AppHandle) -> tauri::Result<()> {
    if installed(app) {
        refresh(app);
        return Ok(());
    }
    let handle = app;
    let spec = current_spec(handle);
    let menu = build_menu(handle, &spec)?;
    // 左クリックは窓を戻す（メニューは右クリック）。
    let builder = TrayIconBuilder::new()
        .icon(app.default_window_icon().cloned().ok_or_else(|| {
            // tauri.conf.json の bundle.icon で既定アイコンを設定済みなので
            // 起きないはずだが、起きても panic せずに起動の失敗として知らせる。
            tauri::Error::AssetNotFound("既定のウィンドウアイコン".to_string())
        })?)
        .tooltip(tooltip_text(status_of(&spec)))
        .menu(&menu)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => open_main_window(app),
            "retry" => crate::shell::request_retry(app),
            "quit" => request_quit(app),
            _ => {}
        });
    let tray = banto_desktop_tray::restore_window_on_left_click(builder, open_main_window)
        .build(handle)?;
    // `TrayIcon` は最後の参照が drop されると消えるので、プロセスの終わりまで
    // 管理状態で持つ（banto-hub と同じ）。
    app.manage(tray);

    tauri::async_runtime::spawn(watch_status(handle.clone(), spec));
    Ok(())
}

/// トレイを作ってあるか。
pub(crate) fn installed(app: &AppHandle) -> bool {
    app.try_state::<TrayIcon<Wry>>().is_some()
}

/// 今の態でメニューと tooltip を差し替える（シェルの画面が変わったとき）。終了処理に
/// 入っていたら「終了処理中」を上書きしない。
pub(crate) fn refresh(app: &AppHandle) {
    post_spec(app, current_spec(app), false);
}

/// トレイの書き換えをメインスレッドへ渡す（**待たない**。モジュール doc「メイン
/// スレッドとロック」）。`even_when_quitting` が偽なら、メインスレッドで実際に書き換える
/// 時点で終了処理に入っていれば何もしない（「終了処理中」を古い状態で上書きしない）。
fn post_spec(app: &AppHandle, spec: Vec<MenuEntry>, even_when_quitting: bool) {
    let handle = app.clone();
    let posted = app.run_on_main_thread(move || {
        if should_apply(even_when_quitting, quitting(&handle)) {
            apply_spec(&handle, &spec);
        }
    });
    if let Err(err) = posted {
        eprintln!("banto: トレイの表示を更新できませんでした: {err}");
    }
}

/// メインスレッドで書き換える時点の判断（純関数）: 「終了処理中」の書き換えは常に、
/// それ以外は終了処理に入っていなければ。
pub(crate) fn should_apply(even_when_quitting: bool, quitting_now: bool) -> bool {
    even_when_quitting || !quitting_now
}

/// 窓を戻す（トレイの「開く」・左クリック・2 つ目の起動）。終了処理に
/// 入っていたら何もしない。メインの窓が無い（起動を止めてエラーの窓を
/// 出している）ときはエラーの窓を前に出す。
pub(crate) fn open_main_window(app: &AppHandle) {
    if quitting(app) {
        return;
    }
    // 起動を止めたときのエラーの窓（旧形式の DB・既に起動中、#392 の対処の窓）も
    // 2 つ目の起動で前に出す。
    // #392 B2-4c: サービスの画面・接続できない状態の窓も（同時にあるのは 1 種類）。
    banto_desktop_tray::show_window(
        app,
        &[
            MAIN_WINDOW_LABEL,
            SERVICE_WINDOW_LABEL,
            FALLBACK_WINDOW_LABEL,
            LEGACY_DB_WINDOW_LABEL,
            RECOVERY_WINDOW_LABEL,
        ],
    );
}

fn quitting(app: &AppHandle) -> bool {
    app.try_state::<QuitGate>()
        .map(|gate| gate.phase() == QuitPhase::Quitting)
        .unwrap_or(false)
}

fn current_state(app: &AppHandle) -> Option<CollectorState> {
    app.try_state::<AppState>()
        .map(|state| state.services.collect.state())
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

/// 今の態と収集の状態からメニューを組む。
fn current_spec(app: &AppHandle) -> Vec<MenuEntry> {
    menu_spec(&crate::shell::current_tray_mode(app), &current_label(app))
}

fn build_menu(app: &AppHandle, spec: &[MenuEntry]) -> tauri::Result<Menu<Wry>> {
    let mut builder = MenuBuilder::new(app);
    for entry in spec {
        builder = match entry {
            MenuEntry::Status(text) => {
                let status = MenuItemBuilder::with_id("status", text)
                    .enabled(false)
                    .build(app)?;
                builder.item(&status)
            }
            MenuEntry::Separator => builder.separator(),
            MenuEntry::Item { id, text } => builder.text(*id, *text),
        };
    }
    builder.build()
}

fn apply_spec(app: &AppHandle, spec: &[MenuEntry]) {
    let Some(tray) = app.try_state::<TrayIcon<Wry>>() else {
        return;
    };
    let _ = tray.set_tooltip(Some(tooltip_text(status_of(spec))));
    if let Ok(menu) = build_menu(app, spec) {
        let _ = tray.set_menu(Some(menu));
    }
}

/// 収集の状態をトレイへ映す見張り。`install` から 1 回だけ起こす（二重起動
/// しない）。文言が変わったときだけトレイを書き換える。終了処理に入ったら
/// 抜ける（「終了処理中」の表示を上書きしないため）。プロセスが終われば
/// ランタイムごと消える。
async fn watch_status(app: AppHandle, mut shown: Vec<MenuEntry>) {
    loop {
        tokio::time::sleep(STATUS_POLL_INTERVAL).await;
        let spec = current_spec(&app);
        if quitting(&app) {
            break;
        }
        // 書き換えはメインスレッドへ渡すだけ（待たない）。確認の直後に終了へ入られても、
        // メインスレッドで書き換える時点でもう一度確かめるので、古い状態で
        // 「終了処理中」を上書きしない（[`post_spec`]）。
        if spec != shown {
            post_spec(&app, spec.clone(), false);
            shown = spec;
        }
    }
}

/// トレイの「終了」（#392 B2-4c からは接続できない状態の窓の「終了」も）。
pub(crate) fn request_quit(app: &AppHandle) {
    let Some(gate) = app.try_state::<QuitGate>() else {
        return;
    };
    match gate.request() {
        QuitRequestDecision::ShowPrompt => {
            // DB の復旧の途中なら確認も出さずに断る（確定の時点でも確かめ直す -
            // [`confirm_quit`]）。
            if crate::shell::recovering(app) {
                let admission = CollectorQuitAdmission(None);
                let _ = gate.answer(false, false, &admission);
                tell_quit_refused(app);
                return;
            }
            if needs_quit_confirm(&crate::shell::current_tray_mode(app)) {
                prompt_quit(app, currently_collecting(app));
            } else {
                // 確認しない態（Fallback）: 同意したものとして同じ受付を通す。メニュー・
                // 窓のボタン（`on_navigation`）はメインスレッドから来るので、終了の処理は
                // 別のスレッドへ渡してメインスレッドを塞がない（PR #608 の Copilot レビュー）。
                let app = app.clone();
                std::thread::spawn(move || on_quit_answer(app, true, false));
            }
        }
        // 確認ダイアログは出ている／終了処理は走っている - どちらも 2 つ目は
        // 作らない。
        QuitRequestDecision::AlreadyConfirming | QuitRequestDecision::AlreadyQuitting => {}
    }
}

fn prompt_quit(app: &AppHandle, prompted_collecting: bool) {
    let app_for_answer = app.clone();
    let mode = crate::shell::current_tray_mode(app);
    banto_desktop_tray::ask_confirm(
        app,
        ConfirmPrompt {
            title: "ChronoGazer を終了",
            message: quit_prompt_for(&mode, prompted_collecting),
            ok_label: "終了",
            cancel_label: "キャンセル",
        },
        move |confirmed| on_quit_answer(app_for_answer, confirmed, prompted_collecting),
    );
}

/// 確認の答え（確認しない態では「同意」として直接呼ぶ）。
fn on_quit_answer(app: AppHandle, confirmed: bool, prompted_collecting: bool) {
    let Some(gate) = app.try_state::<QuitGate>() else {
        return;
    };
    let admission = CollectorQuitAdmission(
        app.try_state::<AppState>()
            .map(|state| state.services.collect.clone()),
    );
    let step = confirm_quit(
        confirmed,
        || crate::shell::reserve_quit(&app),
        || crate::shell::release_quit(&app),
        |confirmed| gate.answer(confirmed, prompted_collecting, &admission),
    );
    match step {
        QuitStep::Proceed => quit(app.clone()),
        QuitStep::AskAgain => prompt_quit(&app, true),
        QuitStep::Refused => tell_quit_refused(&app),
        QuitStep::Nothing => {}
    }
}

/// 確認の答えの後にすること（[`confirm_quit`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QuitStep {
    /// 終了処理へ進む（終了の予約は持ったまま。扉は後始末が閉じる）。
    Proceed,
    /// 収集中の文言で確かめ直す（予約は下ろした）。
    AskAgain,
    /// DB の復旧の途中なので断った（確認の段階は戻した）。
    Refused,
    /// 取り消し・遅れて届いた答え（予約は下ろした）。
    Nothing,
}

/// 確認の答えを、シェルの終了の予約と合わせて確定する（PR #608 のオーナーレビュー
/// P2）。同意なら**先に**終了を予約し（`reserve`。DB の復旧の途中なら断られる）、
/// 断られたら答えを「取り消し」として渡して確認の段階を戻す（`app.exit` へ進まない）。
/// 予約できたら答えを渡し、終了へ進まなかったら予約を下ろす（`release`）。`answer` は
/// `QuitGate::answer`（引数は渡す「同意」）。
pub(crate) fn confirm_quit(
    confirmed: bool,
    reserve: impl FnOnce() -> Result<(), crate::shell::QuitRefused>,
    release: impl FnOnce(),
    answer: impl FnOnce(bool) -> QuitAnswerDecision,
) -> QuitStep {
    if !confirmed {
        let _ = answer(false);
        return QuitStep::Nothing;
    }
    if reserve().is_err() {
        let _ = answer(false);
        return QuitStep::Refused;
    }
    match answer(true) {
        QuitAnswerDecision::Proceed => QuitStep::Proceed,
        QuitAnswerDecision::AskAgain => {
            release();
            QuitStep::AskAgain
        }
        QuitAnswerDecision::Cancelled | QuitAnswerDecision::Ignored => {
            release();
            QuitStep::Nothing
        }
    }
}

/// 終了を断ったことを知らせる。
fn tell_quit_refused(app: &AppHandle) {
    eprintln!("banto: データベースの復旧の途中のため、トレイの終了を断りました");
    app.dialog()
        .message(crate::shell::RECOVERY_IN_PROGRESS_MESSAGE)
        .title("ChronoGazer を終了")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCustom("OK".to_string()))
        .show(|_| {});
}

/// 確認を通った後の終了。窓を隠し、トレイを「終了処理中」にしてから、
/// 既存の終了処理を 1 回だけ通す。済まなかったことがあれば知らせてから
/// 終了する。
fn quit(app: AppHandle) {
    for label in [
        MAIN_WINDOW_LABEL,
        SERVICE_WINDOW_LABEL,
        FALLBACK_WINDOW_LABEL,
    ] {
        if let Some(window) = app.get_webview_window(label) {
            let _ = window.hide();
        }
    }
    // 終了の段階（`QuitPhase::Quitting`）は確認の答えで既に立っているので、この後に
    // メインスレッドで書き換わる他の表示は何もしない（[`should_apply`]）。
    post_spec(
        &app,
        vec![MenuEntry::Status("終了処理中…".to_string())],
        true,
    );
    // 後始末（[`crate::exit_cleanup`]）は、シェルの移り変わり（「再試行」の Desktop の
    // 起動）の終わりと後始末の完了を条件変数で**ブロックして**待つ
    // （本体は共有の `ExitCleanupOnce` の専用スレッドで走る）ので、tokio の
    // ワーカーを塞がないよう専用のスレッドから呼ぶ（#392 B1-2）。
    std::thread::spawn(move || {
        let report = crate::exit_cleanup(&app, ExitTrigger::TrayQuit);
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

    fn items(spec: &[MenuEntry]) -> Vec<(&'static str, &'static str)> {
        spec.iter()
            .filter_map(|entry| match entry {
                MenuEntry::Item { id, text } => Some((*id, *text)),
                _ => None,
            })
            .collect()
    }

    /// #392 B2-4c: 3 つの態のメニュー（モジュール doc の表）。Desktop は A2 のまま。
    #[test]
    fn menu_spec_per_mode() {
        let desktop = menu_spec(&TrayMode::Desktop, "収集停止中");
        assert_eq!(desktop[0], MenuEntry::Status("収集停止中".to_string()));
        assert_eq!(desktop[1], MenuEntry::Separator);
        assert_eq!(items(&desktop), vec![("open", "開く"), ("quit", "終了")]);
        assert_eq!(
            tooltip_text(status_of(&desktop)),
            "ChronoGazer — 収集停止中"
        );

        // Service は収集の状態を出さない（このプロセスは収集していない）。
        let service = menu_spec(&TrayMode::Service, "収集停止中");
        assert_eq!(
            service[0],
            MenuEntry::Status("サービス: 実行中".to_string())
        );
        assert_eq!(
            items(&service),
            vec![("open", "開く"), ("quit", "終了（サービスは止まりません）")]
        );
        // サービスの開始・停止は出さない（O4）。
        assert_eq!(items(&service).len(), 2);

        let fallback = menu_spec(
            &TrayMode::Fallback {
                scm: "開始中".to_string(),
            },
            "x",
        );
        assert_eq!(
            fallback[0],
            MenuEntry::Status("サービス: 開始中（接続できません）".to_string())
        );
        assert_eq!(
            items(&fallback),
            vec![("open", "開く"), ("retry", "再試行"), ("quit", "終了")]
        );
    }

    struct NotBusy;
    impl QuitAdmission for NotBusy {
        fn begin_quit_confirm(&self) -> bool {
            false
        }
        fn cancel_quit_confirm(&self) {}
    }

    /// PR #608 のオーナーレビュー P2: DB の復旧の途中に確定した「終了」は断られ、
    /// 終了処理（`app.exit`）へ進まず、確認の段階も戻る。復旧が終われば通る。
    #[test]
    fn quit_confirmed_during_recovery_is_refused_then_allowed_after() {
        use crate::shell::{ShellGate, ShellView};
        let shell = ShellGate::new(ShellView::Desktop);
        let quit_gate = QuitGate::new();
        let recovery = shell.begin_recovery().expect("recovery");

        assert_eq!(quit_gate.request(), QuitRequestDecision::ShowPrompt);
        let step = confirm_quit(
            true,
            || shell.reserve_quit(),
            || shell.release_quit(),
            |confirmed| quit_gate.answer(confirmed, false, &NotBusy),
        );
        assert_eq!(step, QuitStep::Refused);
        assert_eq!(quit_gate.phase(), QuitPhase::Idle);
        assert!(!shell.is_exiting());

        drop(recovery);
        assert_eq!(quit_gate.request(), QuitRequestDecision::ShowPrompt);
        let step = confirm_quit(
            true,
            || shell.reserve_quit(),
            || shell.release_quit(),
            |confirmed| quit_gate.answer(confirmed, false, &NotBusy),
        );
        assert_eq!(step, QuitStep::Proceed);
        assert_eq!(quit_gate.phase(), QuitPhase::Quitting);
        // 予約は持ったまま: この後の新しい復旧は始まらない。
        assert!(shell.begin_recovery().is_err());
    }

    /// 取り消し・確かめ直しでは予約を下ろす（復旧を塞ぎ続けない）。
    #[test]
    fn quit_reservation_is_released_unless_proceeding() {
        use crate::shell::{ShellGate, ShellView};
        let shell = ShellGate::new(ShellView::Desktop);
        let quit_gate = QuitGate::new();
        quit_gate.request();
        let step = confirm_quit(
            false,
            || shell.reserve_quit(),
            || shell.release_quit(),
            |confirmed| quit_gate.answer(confirmed, false, &NotBusy),
        );
        assert_eq!(step, QuitStep::Nothing);
        drop(shell.begin_recovery().expect("not reserved after cancel"));

        quit_gate.request();
        let step = confirm_quit(
            true,
            || shell.reserve_quit(),
            || shell.release_quit(),
            |_| QuitAnswerDecision::AskAgain,
        );
        assert_eq!(step, QuitStep::AskAgain);
        drop(
            shell
                .begin_recovery()
                .expect("not reserved after ask-again"),
        );
    }

    /// 終了処理に入った後は「終了処理中」以外の書き換えをしない。
    #[test]
    fn only_the_quitting_label_is_applied_after_quit() {
        assert!(should_apply(false, false));
        assert!(!should_apply(false, true));
        assert!(should_apply(true, true));
        assert!(should_apply(true, false));
    }

    /// 終了の確認: Desktop は A2 のまま、Service はサービスが止まらないこと、Fallback は
    /// 確認しない。
    #[test]
    fn quit_prompt_per_mode() {
        assert_eq!(
            quit_prompt_for(&TrayMode::Desktop, true),
            quit_prompt_message(true)
        );
        assert_eq!(
            quit_prompt_for(&TrayMode::Desktop, false),
            quit_prompt_message(false)
        );
        let service = quit_prompt_for(&TrayMode::Service, false);
        assert!(service.contains("止まらず"), "{service}");
        // 収集中で確かめ直すとき（「再試行」で Desktop が動き出した）は収集中の文言。
        assert_eq!(
            quit_prompt_for(&TrayMode::Service, true),
            quit_prompt_message(true)
        );
        assert!(needs_quit_confirm(&TrayMode::Desktop));
        assert!(needs_quit_confirm(&TrayMode::Service));
        assert!(!needs_quit_confirm(&TrayMode::Fallback {
            scm: "停止".into()
        }));
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
