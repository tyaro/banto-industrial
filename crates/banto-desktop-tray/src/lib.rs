//! `banto-desktop-tray`（#392 A2）: トレイ常駐するデスクトップアプリの、窓と
//! 終了まわりの共通部分。
//!
//! もとは banto-hub のデスクトップシェル（`apps/banto-hub/src-tauri`、T16-0/
//! T16-1）の中にあった処理で、ChronoGazer も「ウィンドウを閉じても収集を
//! 止めず、トレイに常駐する」（docs/plan.md §5 #392 の H6、A2）ことになった
//! ため、重複させずに共有部分をここへ移した（#392 A1 の `banto-instance-lock`
//! と同じ扱い）。
//!
//! # この crate が持つもの・持たないもの
//!
//! 持つもの（どちらのアプリでも同じ振る舞い）:
//!
//! - [`show_window`] — 窓を最小化から戻し、表示して前面に出す（トレイの
//!   「開く」・2 つ目の起動で既存の窓を前に出す、の両方で使う）。
//! - [`hide_to_tray_on_close`]: 窓の × を「終了」ではなく「隠す」にし、初回
//!   だけ「トレイで動作を続けます」と OS 通知で知らせる
//!   （[`notify_first_hide_once`]。既読の印はアプリのデータディレクトリの
//!   ファイル）。
//! - [`restore_window_on_left_click`] — トレイの**左クリック = 窓を戻す、
//!   右クリック = メニュー**（Windows の慣習。両アプリで揃える、
//!   オーナー決定 2026-10-12）。
//! - [`ask_confirm`] — 終了の確認ダイアログ（ネイティブ、警告アイコン、
//!   ボタン名を指定できる 2 択）。
//! - [`QuitGate`] — 「終了」の確認中・終了処理中に、もう一度「終了」が押された
//!   ときの扱いを決める小さな状態機械（判断は純関数 [`decide_request`]・
//!   [`decide_answer`] で、表でテストする）。
//!
//! 持たないもの（アプリごとに中身が違う）:
//!
//! - **トレイそのもの**（`TrayIconBuilder`・メニュー項目・状態の文言）。
//!   banto-hub はホストの種類（デスクトップ／サービス接続／fallback）で
//!   メニューが変わり、ChronoGazer は収集の状態を 1 行出すだけ、と形が違う。
//! - **終了処理の中身**（何をどの順で止めるか）。banto-hub は
//!   `RunningHub::shutdown`、ChronoGazer は `shutdown_app_state`。
//! - 単一インスタンス（2 つ目の起動）は `tauri-plugin-single-instance` を
//!   各アプリが登録し、そのコールバックから [`show_window`] を呼ぶ
//!   （プラグインは「他のプラグインより先に登録する」必要があり、登録順は
//!   アプリの `run()` が決めるため、ここでは包まない）。
//!
//! # なぜ Tauri の型に依存してよいか
//!
//! この crate はデスクトップシェル（`src-tauri`）からしか使わない。窓・
//! ダイアログ・通知の呼び出しそのものが共有したい中身なので、Tauri から
//! 切り離すと何も残らない。判断の部分（[`QuitGate`]・[`claim_once_flag`]）
//! だけは Tauri を起動せずにテストできる形に出してある。

use std::path::Path;
use std::sync::Mutex;

use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Runtime, WebviewWindow, WindowEvent};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_notification::NotificationExt;

/// `labels` の順に窓を探し、最初に見つかった 1 つを最小化から戻し、表示して
/// 前面に出す。見つかれば `true`。
///
/// 複数の label を受けるのは、ChronoGazer が起動を止めたとき（旧形式の DB・
/// 同じ DB を別のプロセスが使用中、#392 A1）にメインの窓の代わりにエラーの
/// 窓を出しているため - 2 つ目の起動でも、そのとき見えているべき窓を前に
/// 出す。banto-hub はメインの窓 1 つだけを渡す。
///
/// 個々の操作（unminimize/show/set_focus）の失敗は無視する: どれか 1 つが
/// 効かなくても残りは試す価値があり、呼び出し元（トレイのクリック・
/// 2 つ目の起動）に返して意味のある回復手段も無い。
pub fn show_window<R: Runtime>(app: &AppHandle<R>, labels: &[&str]) -> bool {
    for label in labels {
        if let Some(window) = app.get_webview_window(label) {
            let _ = window.unminimize();
            let _ = window.show();
            let _ = window.set_focus();
            return true;
        }
    }
    false
}

/// トレイのイベントが「左ボタンを離した」クリックか（純関数）。
///
/// 押した（Down）と離した（Up）の両方が来るので、片方だけを拾う。
/// ダブルクリックや右・中ボタン、マウスの出入り・移動は対象外。
pub fn is_left_click_release(event: &TrayIconEvent) -> bool {
    matches!(
        event,
        TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        }
    )
}

/// トレイの左クリックで `restore` を呼ぶようにする（メニューは右クリック）。
///
/// `show_menu_on_left_click(false)` と、左クリック（[`is_left_click_release`]）
/// の拾い方を 1 か所にまとめる。`restore` は通常 [`show_window`] を呼ぶ
/// 関数（アプリごとに「終了処理中は何もしない」等が付く）。戻す窓が無い
/// ときは `restore` が何もしないだけで、メニューは右クリックで出る。
pub fn restore_window_on_left_click<R, F>(
    builder: TrayIconBuilder<R>,
    restore: F,
) -> TrayIconBuilder<R>
where
    R: Runtime,
    F: Fn(&AppHandle<R>) + Send + Sync + 'static,
{
    builder
        .show_menu_on_left_click(false)
        .on_tray_icon_event(move |tray, event| {
            if is_left_click_release(&event) {
                restore(tray.app_handle());
            }
        })
}

/// × でトレイへ格納したときに一度だけ出す OS 通知の中身と、既読の印の
/// ファイル名。
#[derive(Debug, Clone, Copy)]
pub struct TrayHint {
    /// アプリのデータディレクトリ（`app.path().app_data_dir()`）の下に作る
    /// 既読の印のファイル名。アプリの本体のデータ（DB 等）とは独立した、
    /// シェル固有の状態。
    pub flag_file_name: &'static str,
    pub title: &'static str,
    pub body: &'static str,
}

/// 窓の × を「終了」ではなく「隠す」にする（トレイへ格納）。初回だけ
/// [`notify_first_hide_once`] で知らせる。
///
/// `prevent_close` するので、この窓が閉じられて「最後の窓が閉じた」ことに
/// よるアプリの終了は起きない。アプリを終わらせるのはトレイの「終了」
/// （`AppHandle::exit`）と OS のシャットダウン・ログオフだけになる。
pub fn hide_to_tray_on_close<R: Runtime>(window: &WebviewWindow<R>, hint: TrayHint) {
    let window_to_hide = window.clone();
    let app = window.app_handle().clone();
    window.on_window_event(move |event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            let _ = window_to_hide.hide();
            notify_first_hide_once(&app, &hint);
        }
    });
}

/// × でトレイへ格納した直後に呼ぶ。既読の印が無ければ印を作ってから OS
/// 通知を出す。2 回目以降は何もしない。
///
/// 印の場所を解決できない・書き込めないときは**通知をあきらめる**（毎回
/// 通知され続けるより、出ない方が「初回だけ」という意図を壊さない安全側）。
/// OS 通知そのものの失敗（通知が許可されていない等）も無視する - 案内が
/// 1 回出せなくても、アプリの動作は妨げない。
pub fn notify_first_hide_once<R: Runtime>(app: &AppHandle<R>, hint: &TrayHint) {
    let Ok(dir) = app.path().app_data_dir() else {
        return;
    };
    if !claim_once_flag(&dir.join(hint.flag_file_name)) {
        return;
    }
    let _ = app
        .notification()
        .builder()
        .title(hint.title)
        .body(hint.body)
        .show();
}

/// 「一度だけ」の印を取る。印のファイルがまだ無く、作れたときだけ `true`
/// （= 今回が初回で、通知してよい）。既にある・親ディレクトリを作れない・
/// 書き込めないときは `false`。
pub fn claim_once_flag(path: &Path) -> bool {
    if path.exists() {
        return false;
    }
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return false;
        }
    }
    std::fs::write(path, b"1").is_ok()
}

/// 終了の確認ダイアログの文言。
#[derive(Debug, Clone, Copy)]
pub struct ConfirmPrompt<'a> {
    pub title: &'a str,
    pub message: &'a str,
    pub ok_label: &'a str,
    pub cancel_label: &'a str,
}

/// ネイティブの確認ダイアログを出し、答え（OK なら `true`）を `on_answer` に
/// 渡す。**待たない**（ダイアログは別スレッドで開き、答えはそのスレッドから
/// コールバックで返る）- メニューのイベントハンドラがどのスレッドで
/// 呼ばれても安全にするため（`blocking_show` はメインスレッドでの使用を
/// 禁じている）。キャンセル（× で閉じた場合を含む）は `false`。
pub fn ask_confirm<R: Runtime, F>(app: &AppHandle<R>, prompt: ConfirmPrompt<'_>, on_answer: F)
where
    F: FnOnce(bool) + Send + 'static,
{
    app.dialog()
        .message(prompt.message)
        .title(prompt.title)
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            prompt.ok_label.to_string(),
            prompt.cancel_label.to_string(),
        ))
        .show(on_answer);
}

/// 「終了」の進み具合。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitPhase {
    /// 何もしていない。
    Idle,
    /// 確認ダイアログを出して答えを待っている。
    Confirming,
    /// 確認を通り、終了処理に入った（もう戻らない）。
    Quitting,
}

/// 「終了」が押されたときにすること（[`decide_request`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitRequestDecision {
    /// 確認ダイアログを出す。
    ShowPrompt,
    /// もう確認ダイアログが出ている - 2 つ目は出さない。
    AlreadyConfirming,
    /// もう終了処理に入っている - 何もしない（終了処理を 2 回走らせない）。
    AlreadyQuitting,
}

/// 確認ダイアログの答えを受けてすること（[`decide_answer`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitAnswerDecision {
    /// 終了処理へ進む。
    Proceed,
    /// キャンセルされた - 元に戻る。
    Cancelled,
    /// 確認の文言を選んだときは「忙しくない」（例: 収集が止まっていた）
    /// のに、答えが返った時点では忙しくなっていた。**黙って止めず、
    /// 忙しいときの文言でもう一度確かめる**。
    AskAgain,
    /// 確認中ではないのに答えが届いた（起こらないはずの組み合わせ）- 無視する。
    Ignored,
}

/// 「終了」が押されたときの判断（純関数）。
pub fn decide_request(phase: QuitPhase) -> (QuitPhase, QuitRequestDecision) {
    match phase {
        QuitPhase::Idle => (QuitPhase::Confirming, QuitRequestDecision::ShowPrompt),
        QuitPhase::Confirming => (
            QuitPhase::Confirming,
            QuitRequestDecision::AlreadyConfirming,
        ),
        QuitPhase::Quitting => (QuitPhase::Quitting, QuitRequestDecision::AlreadyQuitting),
    }
}

/// 確認ダイアログの答えを受けたときの判断（純関数）。
///
/// - `prompted_busy`: ダイアログの文言を選んだときに忙しかったか
///   （「収集を停止してから終了します」の文言を見せたか）。
/// - `now_busy`: 答えが返った今、忙しいか。
///
/// 忙しくないときの文言で同意を取ったのに、待っている間に忙しくなった
/// （例: 起動直後の収集の自動開始、LAN から収集を開始）場合は
/// [`QuitAnswerDecision::AskAgain`]。逆（忙しい文言で同意 → 今は暇）は
/// そのまま進む（止めるものが減っただけで、同意の範囲に収まる）。
pub fn decide_answer(
    phase: QuitPhase,
    confirmed: bool,
    prompted_busy: bool,
    now_busy: bool,
) -> (QuitPhase, QuitAnswerDecision) {
    match phase {
        QuitPhase::Confirming if !confirmed => (QuitPhase::Idle, QuitAnswerDecision::Cancelled),
        QuitPhase::Confirming if !prompted_busy && now_busy => {
            (QuitPhase::Confirming, QuitAnswerDecision::AskAgain)
        }
        QuitPhase::Confirming => (QuitPhase::Quitting, QuitAnswerDecision::Proceed),
        other => (other, QuitAnswerDecision::Ignored),
    }
}

/// [`decide_request`]・[`decide_answer`] を、アプリの管理状態として 1 つ
/// 持つための入れ物。判断と状態の更新を同じロックの中で行うので、
/// トレイのメニューとダイアログの答えが別スレッドから同時に来ても、
/// 確認ダイアログが 2 つ出る・終了処理が 2 回走ることはない。
#[derive(Debug)]
pub struct QuitGate(Mutex<QuitPhase>);

impl Default for QuitGate {
    fn default() -> Self {
        Self::new()
    }
}

impl QuitGate {
    pub fn new() -> Self {
        Self(Mutex::new(QuitPhase::Idle))
    }

    /// 「終了」が押された。
    pub fn request(&self) -> QuitRequestDecision {
        self.transition(decide_request)
    }

    /// 確認ダイアログの答えが返った。
    pub fn answer(
        &self,
        confirmed: bool,
        prompted_busy: bool,
        now_busy: bool,
    ) -> QuitAnswerDecision {
        self.transition(|phase| decide_answer(phase, confirmed, prompted_busy, now_busy))
    }

    /// 今の段階（表示・テスト用）。
    pub fn phase(&self) -> QuitPhase {
        *self.lock()
    }

    fn transition<D>(&self, decide: impl FnOnce(QuitPhase) -> (QuitPhase, D)) -> D {
        let mut phase = self.lock();
        let (next, decision) = decide(*phase);
        *phase = next;
        decision
    }

    /// 中身は `Copy` の列挙 1 つで、ロック中に panic しても壊れた値は
    /// 残らないので、毒化は無視して中身を使う。
    fn lock(&self) -> std::sync::MutexGuard<'_, QuitPhase> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_table() {
        assert_eq!(
            decide_request(QuitPhase::Idle),
            (QuitPhase::Confirming, QuitRequestDecision::ShowPrompt)
        );
        assert_eq!(
            decide_request(QuitPhase::Confirming),
            (
                QuitPhase::Confirming,
                QuitRequestDecision::AlreadyConfirming
            )
        );
        assert_eq!(
            decide_request(QuitPhase::Quitting),
            (QuitPhase::Quitting, QuitRequestDecision::AlreadyQuitting)
        );
    }

    /// 確認中の答えの全組み合わせ（confirmed × prompted_busy × now_busy）と、
    /// 確認中でない段階に答えが届いた場合。
    #[test]
    fn answer_table() {
        use QuitAnswerDecision::*;
        use QuitPhase::*;
        let cases = [
            // (confirmed, prompted_busy, now_busy) -> (next, decision)
            ((false, false, false), (Idle, Cancelled)),
            ((false, false, true), (Idle, Cancelled)),
            ((false, true, false), (Idle, Cancelled)),
            ((false, true, true), (Idle, Cancelled)),
            ((true, false, false), (Quitting, Proceed)),
            // 暇な文言で同意 → 待つ間に忙しくなった: もう一度確かめる。
            ((true, false, true), (Confirming, AskAgain)),
            // 忙しい文言で同意 → 今は暇: 同意の範囲内なので進む。
            ((true, true, false), (Quitting, Proceed)),
            ((true, true, true), (Quitting, Proceed)),
        ];
        for ((confirmed, prompted, now), expected) in cases {
            assert_eq!(
                decide_answer(Confirming, confirmed, prompted, now),
                expected,
                "confirmed={confirmed} prompted_busy={prompted} now_busy={now}"
            );
        }
        for phase in [Idle, Quitting] {
            for confirmed in [false, true] {
                assert_eq!(
                    decide_answer(phase, confirmed, false, true),
                    (phase, Ignored),
                    "{phase:?} confirmed={confirmed}"
                );
            }
        }
    }

    /// 一連の流れ: 確認中の 2 回目の「終了」は無視、キャンセルで戻る、
    /// 同意後の「終了」は終了処理を 2 回走らせない。
    #[test]
    fn gate_sequence() {
        let gate = QuitGate::new();
        assert_eq!(gate.request(), QuitRequestDecision::ShowPrompt);
        assert_eq!(gate.request(), QuitRequestDecision::AlreadyConfirming);
        assert_eq!(
            gate.answer(false, true, true),
            QuitAnswerDecision::Cancelled
        );
        assert_eq!(gate.phase(), QuitPhase::Idle);

        assert_eq!(gate.request(), QuitRequestDecision::ShowPrompt);
        assert_eq!(gate.answer(true, false, true), QuitAnswerDecision::AskAgain);
        assert_eq!(gate.phase(), QuitPhase::Confirming);
        assert_eq!(gate.answer(true, true, true), QuitAnswerDecision::Proceed);
        assert_eq!(gate.phase(), QuitPhase::Quitting);
        assert_eq!(gate.request(), QuitRequestDecision::AlreadyQuitting);
        // 終了処理に入った後に遅れて届いた答えは無視（元へ戻さない）。
        assert_eq!(
            gate.answer(false, false, false),
            QuitAnswerDecision::Ignored
        );
        assert_eq!(gate.phase(), QuitPhase::Quitting);
    }

    /// 複数スレッドから同時に「終了」が押されても、確認ダイアログを出すのは
    /// 1 回だけ。
    #[test]
    fn concurrent_requests_show_one_prompt() {
        let gate = std::sync::Arc::new(QuitGate::new());
        let handles: Vec<_> = (0..16)
            .map(|_| {
                let gate = gate.clone();
                std::thread::spawn(move || gate.request())
            })
            .collect();
        let shown = handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .filter(|d| *d == QuitRequestDecision::ShowPrompt)
            .count();
        assert_eq!(shown, 1);
    }

    fn click(button: MouseButton, button_state: MouseButtonState) -> TrayIconEvent {
        TrayIconEvent::Click {
            id: tauri::tray::TrayIconId::new("t"),
            position: tauri::PhysicalPosition::new(0.0, 0.0),
            rect: tauri::Rect {
                position: tauri::PhysicalPosition::new(0, 0).into(),
                size: tauri::PhysicalSize::new(16, 16).into(),
            },
            button,
            button_state,
        }
    }

    /// 窓を戻すのは「左ボタンを離した」ときだけ（押した時・右・中は戻さない）。
    #[test]
    fn only_left_button_release_restores() {
        use MouseButton::*;
        use MouseButtonState::*;
        assert!(is_left_click_release(&click(Left, Up)));
        assert!(!is_left_click_release(&click(Left, Down)));
        assert!(!is_left_click_release(&click(Right, Up)));
        assert!(!is_left_click_release(&click(Middle, Up)));
        assert!(!is_left_click_release(&TrayIconEvent::Enter {
            id: tauri::tray::TrayIconId::new("t"),
            position: tauri::PhysicalPosition::new(0.0, 0.0),
            rect: tauri::Rect {
                position: tauri::PhysicalPosition::new(0, 0).into(),
                size: tauri::PhysicalSize::new(16, 16).into(),
            },
        }));
    }

    #[test]
    fn once_flag_is_claimed_once_and_creates_the_parent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("tray-hint-shown");
        assert!(claim_once_flag(&path), "初回は取れる");
        assert!(path.exists());
        assert!(!claim_once_flag(&path), "2 回目は取れない");
    }

    /// 親を作れない（親の位置にファイルがある）ときは取れない
    /// （= 通知しない側に倒れる）。
    #[test]
    fn once_flag_is_not_claimed_when_it_cannot_be_written() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, b"x").unwrap();
        let path = blocker.join("tray-hint-shown");
        assert!(!claim_once_flag(&path));
    }
}
