//! Windows サービスの本体を動かす共有ランナー（#392 B3-1）。
//!
//! banto-hub（`BantoHub`）と banto-hub-sink（`BantoHubSink`）は、SCM との
//! やりとり（`define_windows_service!` と `service_dispatcher::start`、
//! 制御ハンドラの登録、STOP / PRESHUTDOWN の受け付け、Running → StopPending
//! （heartbeat）→ Stopped の報告、本体の panic を FFI 境界の手前で止める
//! こと）を同じ形で 2 か所に持っていた。ChronoGazer のサービス化（B3-2）で
//! 3 つ目の写しを作らないよう、ここへ 1 つにまとめた。製品が持つのは
//! [`ServiceProgram`] の実装（ログの開き方と本体）と終了コードの意味だけ。
//!
//! ## 流れ（[`run_program_with`]）
//!
//! 1. [`ServiceProgram::open_log`]（制御ハンドラの登録より前。登録の失敗も
//!    ログに残すため - 移す前の Hub と sink の順序のまま）。
//! 2. 制御ハンドラを登録する。ハンドラは [`handler_action`] で制御を判断し、
//!    最初の STOP / PRESHUTDOWN だけを記録して StopPending へ入り、待って
//!    いる本体を起こす（すぐ戻る。最終 flush は本体が行う）。SHUTDOWN は
//!    PRESHUTDOWN と同時に受け付けられないので `NotImplemented`。
//! 3. [`ServiceStatusReporter`] を作る（登録で得たハンドルへ報告する）。
//!    登録から作成までの間に停止が届いていたら、ここで StopPending へ入る。
//! 4. [`ServiceProgram::run`] を [`std::panic::catch_unwind`] で包んで呼ぶ。
//!    本体は起動できたら [`ServiceContext::report_running`] を呼び、停止の
//!    合図（[`ServiceContext::wait_for_stop`] / [`ServiceContext::stopped`]）
//!    を待ち、後始末をして [`ServiceExit`] を返す。
//! 5. 返ってきた [`ServiceExit`] で Stopped を**ちょうど 1 回**報告する。
//!    本体が panic したときも `ServiceSpecific(1)`（[`PANIC_EXIT_CODE`]）で
//!    報告する（2026-10-15 オーナー決定 D2。以前は報告せずに終わり、SCM には
//!    最後の状態が残っていた）。
//!
//! 本体は Stopped を報告できない（報告の口を持たない）。移す前の Hub は
//! async ブロックの中の `return` が関数を抜けないせいで Stopped を 2 回
//! 報告しかけ、`StoppedGate`（#330）で止めていたが、本体が終了コードを
//! **返す**形になったのでその型の誤りは起こらない（Stopped の後に何も報告
//! しないことは [`ServiceStatusReporter`] も保証している）。
//!
//! ## 製品の終了コード
//!
//! 意味は製品ごとに違うので、この crate は型（[`ServiceExit`]）だけを持つ。
//! - banto-hub: 1 = ランタイムの構築・Configured 収集の開始の失敗、
//!   2 = profile のロックを別プロセスが保持、3 = その他の起動失敗。
//! - banto-hub-sink: 1 = ランタイムの構築・実行の開始の失敗、2 = 設定ファイル。
//! - ChronoGazer（B3-2 の予定）: 2 = A1 の排他、3 = その他の起動失敗、
//!   1 = 後始末が終わらなかった。
//!
//! どの製品でも、本体の panic は 1（[`PANIC_EXIT_CODE`]）。**この値は
//! Hub の「ランタイム・Configured 収集の開始の失敗」、sink の「ランタイム・
//! 実行の開始の失敗」、ChronoGazer の「後始末が終わらなかった」と同じなので、
//! `sc query` の `SERVICE_EXIT_CODE` だけでは panic と見分けられない**
//! （オーナー決定 D2 は値を分けない）。panic はログの行で見分ける:
//! ランナーが [`ServiceProgram::log_error`] と標準エラー出力に
//!
//! ```text
//! {label}: サービス本体が予期せず panic しました: {panic の中身}
//! ```
//!
//! を 1 行書く（`{label}` は [`ServiceProgram::label`]、中身は `panic!` の
//! 文字列。文字列でなければ「（文字列ではない panic）」）。
//!
//! ## 本体が持つランタイムと panic
//!
//! tokio のランタイムを本体の変数のまま `block_on` の中で panic すると、
//! アンワインドの途中でランタイムが drop され（タスクの終わりを待ちうる）、
//! Stopped の報告がその後になる。本体は [`ServiceContext::hold_until_stopped`]
//! でランタイムを先に預けてから使う: 預けた側の参照はランナーが持ち続ける
//! ので、panic でも正常終了でも、ランタイムが落ちるのは Stopped の報告の後。
//!
//! ## 試験用の panic（デバッグビルドだけ）
//!
//! デバッグビルド（`debug_assertions`）に限り、環境変数
//! `BANTO_SERVICE_TEST_PANIC_AFTER_RUNNING=1` があると
//! [`ServiceContext::report_running`] が Running を報告した直後に panic する
//! （実機で「panic → STOPPED / `SERVICE_EXIT_CODE=1` とログの行」を確かめる
//! ため。Hub ではこれが `block_on` の中で起きる）。release のバイナリ
//! （インストーラが入れるもの）にはこの分岐自体が入らない。
//!
//! ## 1 プロセスに 1 つ
//!
//! `define_windows_service!` が作る FFI の入口は関数ポインタなので、本体を
//! 引数で渡せない。この crate が入口を 1 つだけ定義し、[`run_service`] は
//! 型を消した本体を static に置いてからディスパッチャを起動する（入口が
//! 取り出して動かす）。2 回目の [`run_service`] は
//! [`DispatchError::AlreadyStarted`]（ディスパッチャはプロセスに 1 回しか
//! 起動できない）。SCM の外で起動されたとき（人が `run-service` を直接
//! 実行したとき）は [`DispatchError::NotUnderScm`] で、置いた本体は捨てる。
//! 案内の文言は [`dispatch_failure_lines`] が製品共通で作る（オーナー決定
//! D3。終了コード 1 は製品が決める）。
//!
//! ## 判断と Windows の糊を分ける
//!
//! [`run_program_with`] は Windows に触れない（登録と報告は引数の関数）。
//! テストはこれに偽の登録を渡し、制御を別スレッドから送って報告の順序を
//! 確かめる（全 OS）。Windows の糊（`windows_glue`）は登録と
//! `SetServiceStatus` を差し込むだけ。

use std::any::Any;
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use crate::shutdown::{
    record_first_stop, stop_signal, ScmControlKind, ServiceStatusPlan, ServiceStatusReporter,
    StopNotice, StopSignal,
};

/// 本体が panic したときに Stopped へ載せるサービス固有の終了コード
/// （オーナー決定 D2）。
pub const PANIC_EXIT_CODE: u32 = 1;

/// Win32 の `ERROR_SERVICE_SPECIFIC_ERROR`。サービス固有の終了コードを
/// 報告するとき `dwWin32ExitCode` に入る値。
pub const ERROR_SERVICE_SPECIFIC_ERROR: u32 = 1066;

/// サービス本体の終わり方。Stopped に載せる終了コードになる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceExit {
    /// 正常に停止した（`Win32(0)`）。
    Success,
    /// サービス固有の終了コードで止まった（`ServiceSpecific(n)`）。
    /// 意味は製品ごと（モジュール doc「製品の終了コード」）。
    Specific(u32),
}

impl ServiceExit {
    /// `SERVICE_STATUS` の `(dwWin32ExitCode, dwServiceSpecificExitCode)`。
    /// `sc query` の `WIN32_EXIT_CODE`・`SERVICE_EXIT_CODE` に出る値。
    pub fn scm_exit_codes(self) -> (u32, u32) {
        match self {
            ServiceExit::Success => (0, 0),
            ServiceExit::Specific(code) => (ERROR_SERVICE_SPECIFIC_ERROR, code),
        }
    }

    /// `windows-service` の終了コードへ写す。
    #[cfg(windows)]
    pub fn to_service_exit_code(self) -> windows_service::service::ServiceExitCode {
        use windows_service::service::ServiceExitCode;
        match self {
            ServiceExit::Success => ServiceExitCode::Win32(0),
            ServiceExit::Specific(code) => ServiceExitCode::ServiceSpecific(code),
        }
    }
}

/// 製品がランナーに渡すサービス本体。
///
/// `run` は `&self` で呼ぶ（本体が動いている間も、状態の報告に失敗した
/// ときのログを別スレッドから [`Self::log_error`] で書くため）。
pub trait ServiceProgram: Send + Sync + 'static {
    /// SCM 上のサービス名（制御ハンドラの登録とディスパッチャに渡す）。
    fn service_name(&self) -> &str;

    /// ログ行の先頭に付ける製品名（`banto-hub` など）。ランナーが書く行
    /// （ハンドラの登録の失敗・状態の報告の失敗・panic）は
    /// `"{label}: ..."` の形。
    fn label(&self) -> &str;

    /// サービスのログを開く。制御ハンドラの登録より前に 1 回だけ呼ぶ。
    /// 開けないときも本体は動かす（ログに書けないだけ）。
    fn open_log(&mut self) {}

    /// エラーの行を 1 行書く。どのスレッドからも呼ばれうる。
    fn log_error(&self, line: &str);

    /// 本体。起動できたら [`ServiceContext::report_running`] を呼び、停止の
    /// 合図を待ち、後始末をして終わり方を返す。Stopped はランナーが報告する。
    fn run(&self, ctx: &ServiceContext) -> ServiceExit;
}

/// 制御ハンドラが 1 つの制御に対してすること（[`handler_action`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandlerAction {
    /// `NO_ERROR` を返すだけ（INTERROGATE、または 2 回目以降の停止）。
    Acknowledge,
    /// 最初の停止合図。記録済みで、StopPending へ入り本体を起こす。
    /// `NO_ERROR` を返す。
    BeginStop(StopSignal),
    /// `ERROR_CALL_NOT_IMPLEMENTED` を返す（SHUTDOWN と、扱わない制御）。
    NotImplemented,
}

/// 制御を判断し、最初の停止合図だけを `slot` に記録する。
///
/// - INTERROGATE → [`HandlerAction::Acknowledge`]
/// - STOP / PRESHUTDOWN → 最初の 1 回だけ [`HandlerAction::BeginStop`]、
///   2 回目以降は [`HandlerAction::Acknowledge`]（先に始めた停止を
///   やり直さない）
/// - SHUTDOWN / その他 → [`HandlerAction::NotImplemented`]（SHUTDOWN は
///   PRESHUTDOWN と相互排他なので受け付けていない）
pub fn handler_action(control: ScmControlKind, slot: &Mutex<Option<StopNotice>>) -> HandlerAction {
    match control {
        ScmControlKind::Interrogate => HandlerAction::Acknowledge,
        kind => match stop_signal(kind) {
            Some(signal) => {
                if record_first_stop(slot, signal) {
                    HandlerAction::BeginStop(signal)
                } else {
                    HandlerAction::Acknowledge
                }
            }
            None => HandlerAction::NotImplemented,
        },
    }
}

/// 制御ハンドラが SCM へ返す結果（`windows-service` の
/// `ServiceControlHandlerResult` の写し）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlReply {
    /// `NO_ERROR`。
    NoError,
    /// `ERROR_CALL_NOT_IMPLEMENTED`。
    NotImplemented,
}

/// 制御ハンドラ。SCM のスレッドから呼ばれる。
pub type ControlHandler = Box<dyn Fn(ScmControlKind) -> ControlReply + Send + Sync>;

/// エラーの行を書く関数（[`ServiceProgram::log_error`] を包んだもの）。
pub type ErrorLog = Arc<dyn Fn(&str) + Send + Sync>;

/// 停止の合図を、ハンドラと本体で共有する。
struct StopShared {
    notice: Mutex<Option<StopNotice>>,
    /// 同期の本体（[`ServiceContext::wait_for_stop`]）を起こす。
    cvar: Condvar,
    /// 非同期の本体（[`ServiceContext::stopped`]）を起こす。
    notify: tokio::sync::Notify,
    /// 登録の後にセットする（ハンドラが先に呼ばれたら、まだ無い）。
    reporter: OnceLock<ServiceStatusReporter<ServiceExit>>,
}

impl StopShared {
    fn new() -> Self {
        Self {
            notice: Mutex::new(None),
            cvar: Condvar::new(),
            notify: tokio::sync::Notify::new(),
            reporter: OnceLock::new(),
        }
    }

    fn notice(&self) -> Option<StopNotice> {
        *lock(&self.notice)
    }

    fn handle(&self, control: ScmControlKind) -> ControlReply {
        match handler_action(control, &self.notice) {
            HandlerAction::BeginStop(_) => {
                if let Some(reporter) = self.reporter.get() {
                    reporter.begin_stop();
                }
                self.cvar.notify_all();
                self.notify.notify_waiters();
                ControlReply::NoError
            }
            HandlerAction::Acknowledge => ControlReply::NoError,
            HandlerAction::NotImplemented => ControlReply::NotImplemented,
        }
    }
}

/// 本体に渡す、停止の合図と状態の報告の口。複製しても同じものを指す
/// （非同期のタスクへ持ち込むため）。
#[derive(Clone)]
pub struct ServiceContext {
    shared: Arc<StopShared>,
    reporter: ServiceStatusReporter<ServiceExit>,
    deferred: Arc<Mutex<Vec<Box<dyn Send>>>>,
}

impl ServiceContext {
    /// Running を報告する。起動中に停止が届いていた（または停止が始まって
    /// いた）ら報告せず `false`（受け付けを立てた瞬間に停止へ入るため。
    /// 本体はそのまま停止の処理へ進む）。
    pub fn report_running(&self) -> bool {
        if self.shared.notice().is_some() {
            return false;
        }
        let reported = self.reporter.report_running();
        #[cfg(debug_assertions)]
        if reported {
            debug_test_panic_after_running();
        }
        reported
    }

    /// 停止にかかる見込みを伝える（StopPending の heartbeat が checkpoint を
    /// 進める上限。[`ServiceStatusReporter::set_expected_stop`]）。
    pub fn set_expected_stop(&self, expected_stop: std::time::Duration) {
        self.reporter.set_expected_stop(expected_stop);
    }

    /// 届いた最初の停止合図（まだなら `None`）。
    pub fn stop_notice(&self) -> Option<StopNotice> {
        self.shared.notice()
    }

    /// 停止の合図が届くまでこのスレッドで待つ（同期の本体向け）。
    pub fn wait_for_stop(&self) -> StopNotice {
        let mut guard = lock(&self.shared.notice);
        loop {
            if let Some(notice) = *guard {
                return notice;
            }
            guard = self
                .shared
                .cvar
                .wait(guard)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    /// 停止の合図が届くまで待つ（非同期の本体向け。どのランタイムでもよい）。
    pub async fn stopped(&self) -> StopNotice {
        loop {
            let notified = self.shared.notify.notified();
            let mut notified = std::pin::pin!(notified);
            // 先に待ち手として登録してから確かめる（確かめた直後の
            // `notify_waiters` を取りこぼさない）。
            notified.as_mut().enable();
            if let Some(notice) = self.shared.notice() {
                return notice;
            }
            notified.await;
        }
    }

    /// `value` を Stopped の報告の**後**で落とす（移す前の Hub と sink は、
    /// Stopped を報告してから関数を抜けてランタイムを落としていた。その順序を
    /// 保つ）。本体がこの後も使うもの（tokio のランタイムなど）は
    /// [`Self::hold_until_stopped`] で預ける - 使い終わってから渡す形だと、
    /// その前に panic したときアンワインドで先に落ちる。
    pub fn drop_after_stopped<T: Send + 'static>(&self, value: T) {
        lock(&self.deferred).push(Box::new(value));
    }

    /// `value` を預け、本体が使う参照を返す。ランナーが持つ参照は Stopped の
    /// 報告の後で落ちるので、本体が panic して手元の参照がアンワインドで
    /// 落ちても、`value` 自体はまだ落ちない（モジュール doc「本体が持つ
    /// ランタイムと panic」。tokio のランタイムは `block_on` の前にこれで
    /// 預ける）。
    pub fn hold_until_stopped<T: Send + Sync + 'static>(&self, value: T) -> Arc<T> {
        let value = Arc::new(value);
        self.drop_after_stopped(value.clone());
        value
    }
}

/// 試験用の panic の環境変数（モジュール doc「試験用の panic」）。
#[cfg(debug_assertions)]
const TEST_PANIC_AFTER_RUNNING_ENV: &str = "BANTO_SERVICE_TEST_PANIC_AFTER_RUNNING";

/// デバッグビルドだけ: 環境変数が `1` なら panic する。
#[cfg(debug_assertions)]
fn debug_test_panic_after_running() {
    if std::env::var_os(TEST_PANIC_AFTER_RUNNING_ENV).is_some_and(|value| value == "1") {
        panic!("{TEST_PANIC_AFTER_RUNNING_ENV}=1 による試験用の panic（デバッグビルドだけ）");
    }
}

/// サービス本体を動かす（Windows に触れない芯。モジュール doc「流れ」）。
///
/// `register` は制御ハンドラを登録し、状態の報告の関数を返す（Windows では
/// `service_control_handler::register` と `SetServiceStatus`、テストでは偽物）。
/// 登録に失敗したら、その理由をログに書いて `None`（報告の口が無いので
/// Stopped も報告できない - 移す前と同じ）。それ以外は Stopped に載せた
/// 終わり方。
pub fn run_program_with<P, R, F>(mut program: P, register: R) -> Option<ServiceExit>
where
    P: ServiceProgram,
    R: FnOnce(ControlHandler, ErrorLog) -> Result<F, String>,
    F: Fn(&ServiceStatusPlan, ServiceExit) + Send + Sync + 'static,
{
    program.open_log();
    let program = Arc::new(program);
    let log: ErrorLog = {
        let program = program.clone();
        Arc::new(move |line: &str| program.log_error(line))
    };

    let shared = Arc::new(StopShared::new());
    let handler: ControlHandler = {
        let shared = shared.clone();
        Box::new(move |control| shared.handle(control))
    };
    let report = match register(handler, log) {
        Ok(report) => report,
        Err(err) => {
            program.log_error(&format!(
                "{}: サービスコントロールハンドラの登録に失敗しました: {err}",
                program.label()
            ));
            return None;
        }
    };

    let reporter = ServiceStatusReporter::new(report, ServiceExit::Success);
    let _ = shared.reporter.set(reporter.clone());
    // 登録からここまでの間に届いた停止（ハンドラはまだ報告できなかった）。
    // 後からハンドラが呼んでも `begin_stop` は 2 回目を報告しない。
    if shared.notice().is_some() {
        reporter.begin_stop();
    }

    let ctx = ServiceContext {
        shared,
        reporter: reporter.clone(),
        deferred: Arc::new(Mutex::new(Vec::new())),
    };
    let exit = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| program.run(&ctx))) {
        Ok(exit) => exit,
        Err(payload) => {
            let line = format!(
                "{}: サービス本体が予期せず panic しました: {}",
                program.label(),
                panic_message(payload.as_ref())
            );
            // ログへの書き込み自体が panic の原因だった場合の保険に、先に
            // stderr へ書く（サービスとしては誰にも見えない）。
            eprintln!("{line}");
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                program.log_error(&line);
            }));
            ServiceExit::Specific(PANIC_EXIT_CODE)
        }
    };
    reporter.report_stopped(exit);
    let deferred = std::mem::take(&mut *lock(&ctx.deferred));
    drop(deferred);
    Some(exit)
}

/// panic の中身を 1 行の文字列にする（`&str` と `String` だけ読める）。
fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "（文字列ではない panic）".to_string()
    }
}

/// [`run_service`] の失敗。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DispatchError {
    /// このプロセスでは既にディスパッチャを起動した（1 プロセスに 1 回）。
    #[error("このプロセスでは既にサービスのディスパッチャを起動しています")]
    AlreadyStarted,
    /// SCM の外で起動された（`service_dispatcher::start` の失敗。
    /// 中身はそのエラーの文言）。
    #[error("{0}")]
    NotUnderScm(String),
}

/// [`run_service`] が失敗したときに標準エラー出力へ出す 2 行（製品共通。
/// オーナー決定 D3。終了コードは製品が決める - Hub も sink も 1）。
pub fn dispatch_failure_lines(label: &str, err: &DispatchError) -> [String; 2] {
    [
        format!("{label}: service_dispatcher の起動に失敗しました: {err}"),
        format!(
            "{label}: 'run-service' は SCM がサービス開始時に呼ぶ内部エントリポイントです。手動では実行できません（`install` → `Start-Service` で起動してください）"
        ),
    ]
}

mod handoff;
mod overrun;
#[cfg(windows)]
mod windows_glue;

pub use overrun::OverrunWatch;

/// サービスとして本体を動かす（`run-service` の中身）。SCM が本体を
/// 止め終わるまで戻らない。SCM の外で呼ばれたら
/// [`DispatchError::NotUnderScm`]（すぐ戻る）。
#[cfg(windows)]
pub fn run_service<P: ServiceProgram>(program: P) -> Result<(), DispatchError> {
    windows_glue::run_service(program)
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests;
