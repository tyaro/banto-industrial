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
//! どの製品でも、本体の panic は 1（[`PANIC_EXIT_CODE`]）。
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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Instant;

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
        self.reporter.report_running()
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

    /// `value` を Stopped の報告の**後**で落とす。本体が持つ tokio の
    /// ランタイムなど、落とすのに時間がかかりうるものを渡す（移す前の Hub と
    /// sink は、Stopped を報告してから関数を抜けてランタイムを落としていた。
    /// その順序を保つ）。
    pub fn drop_after_stopped<T: Send + 'static>(&self, value: T) {
        lock(&self.deferred).push(Box::new(value));
    }
}

/// OS の PRESHUTDOWN の作業予算（[`StopNotice::os_work_deadline`]）を
/// 超えたら 1 回だけ `on_overrun` を呼ぶ見張り。停止処理そのものは止めない
/// （警告を残すだけ）。予算内に終わったら [`Self::finish`] で畳む。
pub struct OverrunWatch {
    finished: std::sync::mpsc::Sender<()>,
    thread: std::thread::JoinHandle<()>,
}

impl OverrunWatch {
    /// `deadline` を過ぎても [`Self::finish`] が呼ばれなければ `on_overrun`。
    pub fn spawn<F>(deadline: Instant, on_overrun: F) -> Self
    where
        F: FnOnce() + Send + 'static,
    {
        let (finished, rx) = std::sync::mpsc::channel::<()>();
        let thread = std::thread::spawn(move || {
            let wait = deadline.saturating_duration_since(Instant::now());
            if let Err(std::sync::mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(wait) {
                on_overrun();
            }
        });
        Self { finished, thread }
    }

    /// 停止処理が終わった。予算内なら `on_overrun` は呼ばれない。見張りの
    /// スレッドの終わりを待つ（呼ばれていたら、その書き込みの終わりも）。
    pub fn finish(self) {
        let _ = self.finished.send(());
        let _ = self.thread.join();
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

type ServiceBody = Box<dyn FnOnce(Vec<std::ffi::OsString>) + Send>;

/// FFI の入口へ本体を渡す受け渡し場所（モジュール doc「1 プロセスに 1 つ」）。
#[cfg_attr(not(windows), allow(dead_code))]
struct Handoff {
    started: AtomicBool,
    body: Mutex<Option<ServiceBody>>,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl Handoff {
    const fn new() -> Self {
        Self {
            started: AtomicBool::new(false),
            body: Mutex::new(None),
        }
    }

    /// 本体を置く。2 回目は（1 回目が失敗していても）`AlreadyStarted`。
    fn claim(&self, body: ServiceBody) -> Result<(), DispatchError> {
        if self.started.swap(true, Ordering::SeqCst) {
            return Err(DispatchError::AlreadyStarted);
        }
        *lock(&self.body) = Some(body);
        Ok(())
    }

    /// 入口が本体を取り出す（1 回だけ）。
    fn take(&self) -> Option<ServiceBody> {
        lock(&self.body).take()
    }

    /// ディスパッチャが起動できなかった。置いた本体を捨てる。
    fn abandon(&self) {
        drop(self.take());
    }
}

/// サービスとして本体を動かす（`run-service` の中身）。SCM が本体を
/// 止め終わるまで戻らない。SCM の外で呼ばれたら
/// [`DispatchError::NotUnderScm`]（すぐ戻る）。
#[cfg(windows)]
pub fn run_service<P: ServiceProgram>(program: P) -> Result<(), DispatchError> {
    windows_glue::run_service(program)
}

#[cfg(windows)]
mod windows_glue {
    use std::ffi::OsString;

    use windows_service::service::{ServiceControl, ServiceType};
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::{define_windows_service, service_dispatcher};

    use super::{
        run_program_with, ControlHandler, ControlReply, DispatchError, ErrorLog, Handoff,
        ServiceExit, ServiceProgram,
    };
    use crate::shutdown::{service_status_from_plan, ScmControlKind, ServiceStatusPlan};

    const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;

    static HANDOFF: Handoff = Handoff::new();

    define_windows_service!(ffi_service_main, runner_service_main);

    pub(super) fn run_service<P: ServiceProgram>(program: P) -> Result<(), DispatchError> {
        let name = program.service_name().to_owned();
        HANDOFF.claim(Box::new(move |_arguments| run_program(program)))?;
        service_dispatcher::start(&name, ffi_service_main).map_err(|err| {
            HANDOFF.abandon();
            DispatchError::NotUnderScm(err.to_string())
        })
    }

    /// FFI の入口から呼ばれる。ここで panic すると FFI 境界を越えて
    /// アンワインドしうるので、本体全体を `catch_unwind` で包む（本体の
    /// `run` の panic は [`run_program_with`] が先に受けて Stopped を報告する）。
    fn runner_service_main(arguments: Vec<OsString>) {
        let Some(body) = HANDOFF.take() else {
            eprintln!("サービスの本体が見つかりません（入口が 2 回呼ばれました）");
            return;
        };
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(arguments))).is_err() {
            eprintln!("サービスのランナーが予期せず panic しました");
        }
    }

    /// `ServiceControl` を共有の種別へ写す。
    pub(super) fn scm_control_kind(control: &ServiceControl) -> ScmControlKind {
        match control {
            ServiceControl::Interrogate => ScmControlKind::Interrogate,
            ServiceControl::Stop => ScmControlKind::Stop,
            ServiceControl::Preshutdown => ScmControlKind::Preshutdown,
            ServiceControl::Shutdown => ScmControlKind::Shutdown,
            _ => ScmControlKind::Other,
        }
    }

    fn run_program<P: ServiceProgram>(program: P) {
        let name = program.service_name().to_owned();
        let label = program.label().to_owned();
        let _ = run_program_with(program, move |handler: ControlHandler, log: ErrorLog| {
            let status_handle = service_control_handler::register(&name, move |control| {
                match handler(scm_control_kind(&control)) {
                    ControlReply::NoError => ServiceControlHandlerResult::NoError,
                    ControlReply::NotImplemented => ServiceControlHandlerResult::NotImplemented,
                }
            })
            .map_err(|err| err.to_string())?;
            Ok(move |plan: &ServiceStatusPlan, exit: ServiceExit| {
                let status =
                    service_status_from_plan(plan, SERVICE_TYPE, exit.to_service_exit_code());
                if let Err(err) = status_handle.set_service_status(status) {
                    // 失敗はログだけ（停止処理自体は続ける）。
                    log(&format!("{label}: サービス状態の報告に失敗しました: {err}"));
                }
            })
        });
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    use crate::shutdown::ServicePhase;

    /// 報告を記録する（計画と終わり方）。
    type Seen = Arc<Mutex<Vec<(ServiceStatusPlan, ServiceExit)>>>;

    /// 偽の登録が返す、報告を記録する関数。
    type TestReport = Box<dyn Fn(&ServiceStatusPlan, ServiceExit) + Send + Sync>;

    /// 偽の登録: ハンドラをテストへ渡し、報告を記録する関数を返す。
    fn fake_register(
        handler_tx: mpsc::Sender<ControlHandler>,
        seen: Seen,
    ) -> impl FnOnce(ControlHandler, ErrorLog) -> Result<TestReport, String> {
        move |handler, _log| {
            handler_tx.send(handler).unwrap();
            Ok(Box::new(move |plan: &ServiceStatusPlan, exit| {
                seen.lock().unwrap().push((*plan, exit))
            }))
        }
    }

    /// 本体の振る舞いをクロージャで差し込むテスト用の program。
    struct TestProgram<B: Fn(&ServiceContext) -> ServiceExit + Send + Sync + 'static> {
        body: B,
        errors: Arc<Mutex<Vec<String>>>,
        opened: Arc<AtomicBool>,
    }

    impl<B: Fn(&ServiceContext) -> ServiceExit + Send + Sync + 'static> ServiceProgram
        for TestProgram<B>
    {
        fn service_name(&self) -> &str {
            "TestSvc"
        }
        fn label(&self) -> &str {
            "test-svc"
        }
        fn open_log(&mut self) {
            self.opened.store(true, Ordering::SeqCst);
        }
        fn log_error(&self, line: &str) {
            self.errors.lock().unwrap().push(line.to_string());
        }
        fn run(&self, ctx: &ServiceContext) -> ServiceExit {
            (self.body)(ctx)
        }
    }

    fn program<B>(body: B) -> (TestProgram<B>, Arc<Mutex<Vec<String>>>)
    where
        B: Fn(&ServiceContext) -> ServiceExit + Send + Sync + 'static,
    {
        let errors = Arc::new(Mutex::new(Vec::new()));
        (
            TestProgram {
                body,
                errors: errors.clone(),
                opened: Arc::new(AtomicBool::new(false)),
            },
            errors,
        )
    }

    fn phases(seen: &Seen) -> Vec<(ServicePhase, ServiceExit)> {
        seen.lock()
            .unwrap()
            .iter()
            .map(|(plan, exit)| (plan.phase, *exit))
            .collect()
    }

    /// 本体を別スレッドで動かし、ハンドラを受け取る。
    fn spawn_runner<P: ServiceProgram>(
        program: P,
    ) -> (
        std::thread::JoinHandle<Option<ServiceExit>>,
        ControlHandler,
        Seen,
    ) {
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let (handler_tx, handler_rx) = mpsc::channel();
        let register = fake_register(handler_tx, seen.clone());
        let runner = std::thread::spawn(move || run_program_with(program, register));
        let handler = handler_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("handler registered");
        (runner, handler, seen)
    }

    #[test]
    fn handler_action_table() {
        // (制御, 1 回目, 2 回目)
        let table = [
            (
                ScmControlKind::Interrogate,
                HandlerAction::Acknowledge,
                HandlerAction::Acknowledge,
            ),
            (
                ScmControlKind::Stop,
                HandlerAction::BeginStop(StopSignal::ServiceStop),
                HandlerAction::Acknowledge,
            ),
            (
                ScmControlKind::Preshutdown,
                HandlerAction::BeginStop(StopSignal::OsPreshutdown),
                HandlerAction::Acknowledge,
            ),
            (
                ScmControlKind::Shutdown,
                HandlerAction::NotImplemented,
                HandlerAction::NotImplemented,
            ),
            (
                ScmControlKind::Other,
                HandlerAction::NotImplemented,
                HandlerAction::NotImplemented,
            ),
        ];
        for (control, first, second) in table {
            let slot = Mutex::new(None);
            assert_eq!(handler_action(control, &slot), first, "{control:?}");
            assert_eq!(handler_action(control, &slot), second, "{control:?}");
        }

        // STOP の後の PRESHUTDOWN は停止をやり直さない（最初の合図のまま）。
        let slot = Mutex::new(None);
        assert_eq!(
            handler_action(ScmControlKind::Stop, &slot),
            HandlerAction::BeginStop(StopSignal::ServiceStop)
        );
        assert_eq!(
            handler_action(ScmControlKind::Preshutdown, &slot),
            HandlerAction::Acknowledge
        );
        assert_eq!(
            slot.lock().unwrap().unwrap().signal,
            StopSignal::ServiceStop
        );
    }

    #[test]
    fn service_exit_maps_to_scm_exit_codes() {
        assert_eq!(ServiceExit::Success.scm_exit_codes(), (0, 0));
        assert_eq!(ServiceExit::Specific(2).scm_exit_codes(), (1066, 2));
        assert_eq!(ServiceExit::Specific(3).scm_exit_codes(), (1066, 3));
    }

    /// 写した `ServiceExitCode` を `SERVICE_STATUS` へ書いて読み戻すと、
    /// [`ServiceExit::scm_exit_codes`] と同じ 2 つの値になる。
    #[cfg(windows)]
    #[test]
    fn service_exit_maps_to_windows_exit_codes() {
        use windows_service::service::ServiceExitCode;
        use windows_sys::Win32::System::Services::SERVICE_STATUS;
        for exit in [
            ServiceExit::Success,
            ServiceExit::Specific(1),
            ServiceExit::Specific(2),
            ServiceExit::Specific(3),
        ] {
            let (win32, specific) = exit.scm_exit_codes();
            let raw = SERVICE_STATUS {
                dwServiceType: 0,
                dwCurrentState: 0,
                dwControlsAccepted: 0,
                dwWin32ExitCode: win32,
                dwServiceSpecificExitCode: specific,
                dwCheckPoint: 0,
                dwWaitHint: 0,
            };
            assert_eq!(
                ServiceExitCode::from(&raw),
                exit.to_service_exit_code(),
                "{exit:?}"
            );
        }
        assert_eq!(
            ServiceExit::Success.to_service_exit_code(),
            ServiceExitCode::NO_ERROR
        );
    }

    /// (a) Running → STOP → StopPending → 本体の後始末 → Stopped(0)。
    /// 同期の待ち（`wait_for_stop`）で。
    #[test]
    fn running_then_stop_reports_pending_then_stopped_success() {
        let (cleanup_tx, cleanup_rx) = mpsc::channel::<StopNotice>();
        let (prog, errors) = program(move |ctx| {
            assert!(ctx.report_running());
            let notice = ctx.wait_for_stop();
            cleanup_tx.send(notice).unwrap();
            ServiceExit::Success
        });
        let opened = prog.opened.clone();
        let (runner, handler, seen) = spawn_runner(prog);
        assert!(opened.load(Ordering::SeqCst), "登録より前にログを開く");

        // Running の報告を待ってから STOP。
        let deadline = Instant::now() + Duration::from_secs(5);
        while phases(&seen).is_empty() {
            assert!(Instant::now() < deadline, "Running が報告されない");
            std::thread::yield_now();
        }
        assert_eq!(handler(ScmControlKind::Stop), ControlReply::NoError);
        assert_eq!(handler(ScmControlKind::Stop), ControlReply::NoError);
        assert_eq!(
            handler(ScmControlKind::Shutdown),
            ControlReply::NotImplemented
        );
        let notice = cleanup_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("本体が起きる");
        assert_eq!(notice.signal, StopSignal::ServiceStop);
        assert_eq!(runner.join().unwrap(), Some(ServiceExit::Success));

        assert_eq!(
            phases(&seen),
            vec![
                (ServicePhase::Running, ServiceExit::Success),
                (ServicePhase::StopPending, ServiceExit::Success),
                (ServicePhase::Stopped, ServiceExit::Success),
            ]
        );
        assert!(errors.lock().unwrap().is_empty());
    }

    /// (a') 非同期の待ち（`stopped`）でも PRESHUTDOWN で起きる。
    #[test]
    fn async_body_wakes_on_preshutdown() {
        let (prog, _errors) = program(|ctx| {
            assert!(ctx.report_running());
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            let notice = runtime.block_on(ctx.stopped());
            assert_eq!(notice.signal, StopSignal::OsPreshutdown);
            assert!(notice.os_work_deadline().is_some());
            ctx.drop_after_stopped(runtime);
            ServiceExit::Success
        });
        let (runner, handler, seen) = spawn_runner(prog);
        let deadline = Instant::now() + Duration::from_secs(5);
        while phases(&seen).is_empty() {
            assert!(Instant::now() < deadline, "Running が報告されない");
            std::thread::yield_now();
        }
        assert_eq!(handler(ScmControlKind::Preshutdown), ControlReply::NoError);
        assert_eq!(runner.join().unwrap(), Some(ServiceExit::Success));
        let seen = phases(&seen);
        assert_eq!(seen.last().unwrap().0, ServicePhase::Stopped);
    }

    /// (b) Running の前に起動に失敗: Stopped(n) を 1 回だけ、Running は
    /// 出さない（#330 の `StoppedGate` の置き換え。終了コード 0 で上書き
    /// しない）。
    #[test]
    fn a_start_failure_reports_stopped_once_with_its_code_and_no_running() {
        let (prog, _errors) = program(|_ctx| ServiceExit::Specific(2));
        let (runner, _handler, seen) = spawn_runner(prog);
        assert_eq!(runner.join().unwrap(), Some(ServiceExit::Specific(2)));
        assert_eq!(
            phases(&seen),
            vec![(ServicePhase::Stopped, ServiceExit::Specific(2))]
        );
    }

    /// (c) 起動中に STOP が届いた: Running を報告せず（`report_running` が
    /// false）、StopPending → Stopped。
    #[test]
    fn a_stop_during_start_suppresses_running() {
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let go_rx = Mutex::new(go_rx);
        let (prog, _errors) = program(move |ctx| {
            // 起動処理の途中で STOP が届くのを待つ。
            go_rx.lock().unwrap().recv().unwrap();
            assert!(ctx.stop_notice().is_some());
            assert!(!ctx.report_running(), "停止の後に Running を出さない");
            let notice = ctx.wait_for_stop();
            assert_eq!(notice.signal, StopSignal::ServiceStop);
            ServiceExit::Success
        });
        let (runner, handler, seen) = spawn_runner(prog);
        assert_eq!(handler(ScmControlKind::Stop), ControlReply::NoError);
        go_tx.send(()).unwrap();
        assert_eq!(runner.join().unwrap(), Some(ServiceExit::Success));
        let seen = phases(&seen);
        assert!(
            !seen
                .iter()
                .any(|(phase, _)| *phase == ServicePhase::Running),
            "{seen:?}"
        );
        assert_eq!(
            seen.last().copied(),
            Some((ServicePhase::Stopped, ServiceExit::Success))
        );
        assert_eq!(
            seen.iter()
                .filter(|(phase, _)| *phase == ServicePhase::Stopped)
                .count(),
            1
        );
    }

    /// (d) 本体の panic: Stopped(ServiceSpecific(1)) を 1 回だけ、panic を
    /// ログに残す（オーナー決定 D2）。
    #[test]
    fn a_panic_in_the_body_reports_stopped_specific_one_once() {
        let (prog, errors) = program(|ctx| {
            assert!(ctx.report_running());
            panic!("テストの panic");
        });
        let (runner, _handler, seen) = spawn_runner(prog);
        assert_eq!(
            runner.join().unwrap(),
            Some(ServiceExit::Specific(PANIC_EXIT_CODE))
        );
        assert_eq!(
            phases(&seen),
            vec![
                (ServicePhase::Running, ServiceExit::Success),
                (ServicePhase::Stopped, ServiceExit::Specific(1)),
            ]
        );
        let errors = errors.lock().unwrap();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].starts_with("test-svc: "), "{}", errors[0]);
        assert!(errors[0].contains("テストの panic"), "{}", errors[0]);
    }

    /// 登録に失敗したら、その理由をログに書き、本体は動かさず、何も
    /// 報告しない（報告の口が無い）。
    #[test]
    fn a_registration_failure_is_logged_and_runs_nothing() {
        let ran = Arc::new(AtomicBool::new(false));
        let ran_in_body = ran.clone();
        let (prog, errors) = program(move |_ctx| {
            ran_in_body.store(true, Ordering::SeqCst);
            ServiceExit::Success
        });
        let result = run_program_with(prog, |_handler, _log| {
            Err::<fn(&ServiceStatusPlan, ServiceExit), _>("os error 1063".to_string())
        });
        assert_eq!(result, None);
        assert!(!ran.load(Ordering::SeqCst));
        assert_eq!(
            errors.lock().unwrap().as_slice(),
            ["test-svc: サービスコントロールハンドラの登録に失敗しました: os error 1063"]
        );
    }

    /// 登録から reporter の作成までの間に届いた停止も StopPending にする。
    #[test]
    fn a_stop_between_registration_and_reporter_enters_stop_pending() {
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let (prog, _errors) = program(|ctx| {
            assert!(!ctx.report_running());
            ctx.wait_for_stop();
            ServiceExit::Success
        });
        let result = run_program_with(prog, move |handler, _log| {
            // 登録の直後（reporter がまだ無い）に STOP が届く。
            assert_eq!(handler(ScmControlKind::Stop), ControlReply::NoError);
            Ok(move |plan: &ServiceStatusPlan, exit: ServiceExit| {
                sink.lock().unwrap().push((*plan, exit))
            })
        });
        assert_eq!(result, Some(ServiceExit::Success));
        assert_eq!(
            phases(&seen),
            vec![
                (ServicePhase::StopPending, ServiceExit::Success),
                (ServicePhase::Stopped, ServiceExit::Success),
            ]
        );
    }

    /// `drop_after_stopped` に渡したものは Stopped の報告の後で落ちる。
    #[test]
    fn deferred_values_drop_after_stopped() {
        struct Marker(Seen, Arc<Mutex<Option<usize>>>);
        impl Drop for Marker {
            fn drop(&mut self) {
                *self.1.lock().unwrap() = Some(self.0.lock().unwrap().len());
            }
        }
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let reports_at_drop = Arc::new(Mutex::new(None));
        let marker_seen = seen.clone();
        let marker_slot = reports_at_drop.clone();
        let marker = Mutex::new(Some(Marker(marker_seen, marker_slot)));
        let (prog, _errors) = program(move |ctx| {
            ctx.drop_after_stopped(marker.lock().unwrap().take().unwrap());
            ServiceExit::Specific(3)
        });
        let sink = seen.clone();
        let result = run_program_with(prog, move |_handler, _log| {
            Ok(move |plan: &ServiceStatusPlan, exit: ServiceExit| {
                sink.lock().unwrap().push((*plan, exit))
            })
        });
        assert_eq!(result, Some(ServiceExit::Specific(3)));
        assert_eq!(*reports_at_drop.lock().unwrap(), Some(1), "Stopped の後");
    }

    #[test]
    fn the_handoff_is_claimed_once_per_process() {
        let handoff = Handoff::new();
        assert!(handoff.claim(Box::new(|_| {})).is_ok());
        assert!(handoff.take().is_some());
        assert!(handoff.take().is_none(), "取り出しは 1 回だけ");
        assert_eq!(
            handoff.claim(Box::new(|_| {})),
            Err(DispatchError::AlreadyStarted)
        );

        let failed = Handoff::new();
        assert!(failed.claim(Box::new(|_| {})).is_ok());
        failed.abandon();
        assert!(failed.take().is_none(), "失敗したら本体を捨てる");
        assert_eq!(
            failed.claim(Box::new(|_| {})),
            Err(DispatchError::AlreadyStarted),
            "ディスパッチャの起動は 1 回だけ"
        );
    }

    /// SCM の外（テストのプロセス）では `NotUnderScm`、2 回目は
    /// `AlreadyStarted`。プロセスで 1 回しか呼べないので、このテストだけが
    /// `run_service` を呼ぶ。
    #[cfg(windows)]
    #[test]
    fn run_service_outside_scm_then_again_is_already_started() {
        let (first, _) = program(|_ctx| ServiceExit::Success);
        match run_service(first) {
            Err(DispatchError::NotUnderScm(detail)) => assert!(!detail.is_empty()),
            other => panic!("SCM の外では NotUnderScm のはず: {other:?}"),
        }
        let (second, _) = program(|_ctx| ServiceExit::Success);
        assert_eq!(run_service(second), Err(DispatchError::AlreadyStarted));
    }

    #[test]
    fn dispatch_failure_lines_are_shared_across_products() {
        let lines = dispatch_failure_lines(
            "banto-hub-sink",
            &DispatchError::NotUnderScm("os error 1063".to_string()),
        );
        assert_eq!(
            lines[0],
            "banto-hub-sink: service_dispatcher の起動に失敗しました: os error 1063"
        );
        assert_eq!(
            lines[1],
            "banto-hub-sink: 'run-service' は SCM がサービス開始時に呼ぶ内部エントリポイントです。手動では実行できません（`install` → `Start-Service` で起動してください）"
        );
    }

    #[test]
    fn os_work_deadline_is_only_for_preshutdown() {
        let received_at = Instant::now();
        let stop = StopNotice {
            signal: StopSignal::ServiceStop,
            received_at,
        };
        assert_eq!(stop.os_work_deadline(), None);
        let os = StopNotice {
            signal: StopSignal::OsPreshutdown,
            received_at,
        };
        assert_eq!(
            os.os_work_deadline(),
            Some(received_at + Duration::from_secs(27))
        );
    }

    #[test]
    fn overrun_watch_fires_only_past_the_deadline() {
        let fired = Arc::new(AtomicBool::new(false));
        let flag = fired.clone();
        let watch = OverrunWatch::spawn(Instant::now() + Duration::from_secs(60), move || {
            flag.store(true, Ordering::SeqCst)
        });
        watch.finish();
        assert!(!fired.load(Ordering::SeqCst), "予算内なら警告しない");

        let fired = Arc::new(AtomicBool::new(false));
        let flag = fired.clone();
        let watch = OverrunWatch::spawn(Instant::now(), move || flag.store(true, Ordering::SeqCst));
        // 期限は過ぎている: 見張りは finish を待たずに警告し、終わる。
        let deadline = Instant::now() + Duration::from_secs(5);
        while !fired.load(Ordering::SeqCst) {
            assert!(Instant::now() < deadline, "期限を過ぎても警告しない");
            std::thread::yield_now();
        }
        watch.finish();
    }
}
