//! OS シャットダウンでサービスが最終 flush を通すための、ホスト非依存の判断。
//!
//! Windows の SCM は、サービスが `SERVICE_ACCEPT_PRESHUTDOWN` を報告している
//! ときだけ `SERVICE_CONTROL_PRESHUTDOWN` を送る。送らないサービスは
//! シャットダウンで停止処理を通らずにプロセスを終わらされる（#589）。
//! `SERVICE_ACCEPT_SHUTDOWN` と `SERVICE_ACCEPT_PRESHUTDOWN` は同時に
//! 受け付けられない（相互排他）。こちらは待ち時間を指定できる PRESHUTDOWN
//! だけを受ける。
//!
//! 待ち時間の既定は Windows 10 build 15063 以降 10 秒で、`SetServiceStatus`
//! の `wait_hint` では延びない。延びるのは `ChangeServiceConfig2W` の
//! `SERVICE_CONFIG_PRESHUTDOWN_INFO` だけ。既に登録済みのサービスへも
//! 同じ値を書き、読み戻して確認する（起動種別と ACL は触らない）。

use std::sync::Mutex;
use std::time::{Duration, Instant};

#[cfg(windows)]
use crate::windows_impl::map_win_err;
use crate::ServiceManagerError;

/// SCM が PRESHUTDOWN を送ってから、サービスが `Stopped` を報告するまで
/// 待つ上限。build 15063 以降の既定 10 秒では、sink の既定 flush（5 秒）と
/// flusher の停止待ち・状態 push が収まらない。Hub の収集停止（tstore の
/// 最終 flush を含む）の余裕もここに含める。
pub const PRESHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);

/// `Stopped` の報告と、完了または打ち切りのログをディスクへ書く余裕。
/// 停止処理の本体は [`stop_work_budget`] まで。ここを超えても SCM は
/// [`PRESHUTDOWN_TIMEOUT`] までは待ってからプロセスを終わらせうる。
pub const PRESHUTDOWN_REPORT_MARGIN: Duration = Duration::from_secs(3);

/// 停止処理の本体に使ってよい時間。[`PRESHUTDOWN_TIMEOUT`] から
/// [`PRESHUTDOWN_REPORT_MARGIN`] を引いた残り。引いた結果が負になる
/// ときはゼロ。
pub fn stop_work_budget(preshutdown_timeout: Duration) -> Duration {
    preshutdown_timeout.saturating_sub(PRESHUTDOWN_REPORT_MARGIN)
}

/// ハンドラが受けた SCM 制御の種別。Windows の列挙をこの crate の外へ
/// 漏らさないための写し。製品側が `ServiceControl` から写す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScmControlKind {
    /// 状態の問い合わせ。停止しない。
    Interrogate,
    /// `Stop-Service` / `sc stop`。
    Stop,
    /// OS シャットダウン（PRESHUTDOWN を受け付けているサービスだけ）。
    Preshutdown,
    /// OS シャットダウンの後段。PRESHUTDOWN と同時には受け付けられない。
    Shutdown,
    /// この製品が扱わない制御。
    Other,
}

/// 最終 flush を含む停止経路へ入る合図。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopSignal {
    /// 運用者の停止。製品の通常の停止（設定どおりの flush。全体の打ち切りはしない）。
    ServiceStop,
    /// OS の PRESHUTDOWN。[`stop_work_budget`] 以内に終える。
    OsPreshutdown,
}

/// 制御を停止合図へ写す。`Shutdown` は受け付けないので `None`
/// （PRESHUTDOWN と相互排他。両方を受理する実装にはしない）。
pub fn stop_signal(control: ScmControlKind) -> Option<StopSignal> {
    match control {
        ScmControlKind::Stop => Some(StopSignal::ServiceStop),
        ScmControlKind::Preshutdown => Some(StopSignal::OsPreshutdown),
        ScmControlKind::Shutdown | ScmControlKind::Interrogate | ScmControlKind::Other => None,
    }
}

/// 最初の停止合図。`received_at` は SCM が制御を届けた時刻で、
/// OS シャットダウンの予算はこの時刻から数える。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StopNotice {
    /// 最初に届いた停止合図。後から来た制御では上書きしない。
    pub signal: StopSignal,
    /// ハンドラが合図を記録した時刻。
    pub received_at: Instant,
}

/// 最初の停止合図だけを `slot` に記録する。2 回目以降は `false` で、
/// 中身は変えない（STOP の後に別の制御が来ても、先に始めた停止を
/// やり直さない）。
pub fn record_first_stop(slot: &Mutex<Option<StopNotice>>, signal: StopSignal) -> bool {
    let mut guard = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard.is_some() {
        return false;
    }
    *guard = Some(StopNotice {
        signal,
        received_at: Instant::now(),
    });
    true
}

/// SCM へ報告する状態。pending 以外は checkpoint と wait_hint を 0 にする
/// （`SetServiceStatus` の決まり）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServicePhase {
    /// 制御を受け付ける稼働中。
    Running,
    /// 停止処理の途中。制御は受け付けない。
    StopPending,
    /// 停止済み。これ以降の報告は製品側のゲートが落とす。
    Stopped,
}

/// 1 回の `SetServiceStatus` に載せる中身（終了コードとサービス種別は
/// 呼び出し側が足す）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceStatusPlan {
    /// 報告する状態。
    pub phase: ServicePhase,
    /// `SERVICE_ACCEPT_STOP` を載せるか。
    pub accept_stop: bool,
    /// `SERVICE_ACCEPT_PRESHUTDOWN` を載せるか。SHUTDOWN は載せない。
    pub accept_preshutdown: bool,
    /// pending の進捗。非 pending は 0。StopPending は 1 以上。
    pub checkpoint: u32,
    /// 次の状態報告までの見込み。非 pending は 0。StopPending は 0 より大きい。
    pub wait_hint: Duration,
}

/// 稼働中に受け付ける制御。STOP（通常停止）と PRESHUTDOWN（OS の
/// シャットダウン）。SHUTDOWN は付けない。
pub fn running_status() -> ServiceStatusPlan {
    ServiceStatusPlan {
        phase: ServicePhase::Running,
        accept_stop: true,
        accept_preshutdown: true,
        checkpoint: 0,
        wait_hint: Duration::ZERO,
    }
}

/// 停止済み。制御は受け付けない。
pub fn stopped_status() -> ServiceStatusPlan {
    ServiceStatusPlan {
        phase: ServicePhase::Stopped,
        accept_stop: false,
        accept_preshutdown: false,
        checkpoint: 0,
        wait_hint: Duration::ZERO,
    }
}

/// 停止処理に入った報告。`checkpoint` が 0、または `wait_hint` が 0 の
/// ときは `None`（pending で両方を 0 にすると SCM は進捗が無いと扱う）。
pub fn stop_pending_status(checkpoint: u32, wait_hint: Duration) -> Option<ServiceStatusPlan> {
    if checkpoint == 0 || wait_hint.is_zero() {
        return None;
    }
    Some(ServiceStatusPlan {
        phase: ServicePhase::StopPending,
        accept_stop: false,
        accept_preshutdown: false,
        checkpoint,
        wait_hint,
    })
}

/// 停止通知のあと SCM へ出す状態の順。先に StopPending、後始末のあと
/// Stopped を 1 回。`wait_hint` が 0 のときは計画を作れない。
pub fn stop_status_sequence(wait_hint: Duration) -> Option<(ServiceStatusPlan, ServiceStatusPlan)> {
    let pending = stop_pending_status(1, wait_hint)?;
    Some((pending, stopped_status()))
}

/// 停止処理の途中で StopPending を報告し直す間隔。`wait_hint`
/// （[`PRESHUTDOWN_TIMEOUT`] の 30 秒）より十分短くする。
pub const STOP_PENDING_HEARTBEAT: Duration = Duration::from_secs(5);

/// 停止にかかる見込み `expected_stop` から、heartbeat が進めてよい
/// checkpoint の上限を決める（最初の 1 + `interval` の刻みで割った回数、
/// 切り上げ）。上限に達したら checkpoint を進めない - 停止が固まったときに
/// SCM の「応答なし」を隠し続けないため。`interval` が 0 なら 1。
pub fn heartbeat_checkpoint_limit(expected_stop: Duration, interval: Duration) -> u32 {
    if interval.is_zero() {
        return 1;
    }
    let ticks = expected_stop.as_nanos().div_ceil(interval.as_nanos());
    u32::try_from(ticks).unwrap_or(u32::MAX).saturating_add(1)
}

/// サービスが SCM へ出す状態報告を 1 か所に集める（Running → StopPending
/// （checkpoint を進める heartbeat つき）→ Stopped）。
///
/// **約束**: 状態の移り変わりと報告は 1 つの `Mutex` の中で行う。
/// - Stopped を報告した後は、どの報告も（Running も StopPending も
///   heartbeat の報告も）しない。Stopped の後の `SetServiceStatus` は
///   クラッシュしうる（Microsoft の文書）。
/// - 停止の合図を受ける側（制御ハンドラ）が [`Self::begin_stop`] を呼ぶ
///   より先に、停止処理が [`Self::report_stopped`] まで進んでも、後から
///   来た `begin_stop` は何も報告せず heartbeat も始めない（#589 の
///   再レビュー P2: 合図を公開してから heartbeat を保存するまでの間に
///   Stopped が追い越していた）。
/// - heartbeat のスレッドも、報告の前に同じロックの中で Stopped でない
///   ことを確かめる。
/// - Running は、停止が始まった後には報告しない。
///
/// heartbeat は [`STOP_PENDING_HEARTBEAT`] ごとに checkpoint を 1 つ進めて
/// StopPending を報告し直す。SCM（`sc stop`・`Stop-Service`）は
/// `wait_hint` の間に checkpoint が進まないと応答なしとみなすので、30 秒を
/// 超える停止でも進んでいる限り待ち時間切れを出させない。ただし
/// [`Self::set_expected_stop`] の見込みから決めた上限
/// （[`heartbeat_checkpoint_limit`]）で進めるのをやめる - heartbeat は停止の
/// 処理が実際に進んでいるかを見ていないので、上限が無いと停止が固まっても
/// 応答なしが出なくなる。上限の後 `wait_hint` を過ぎても Stopped が来なければ、
/// SCM は従来どおり応答なしと報告する。OS の PRESHUTDOWN の待ち時間は
/// これでは延びない。
///
/// `E` は Stopped に載せる終了コードの型（Windows では `ServiceExitCode`）。
/// Running と StopPending には `ok_exit` を載せる。`report` はロックの中で
/// 呼ばれるので、この型を呼び返してはいけない。
pub struct ServiceStatusReporter<E> {
    shared: std::sync::Arc<ReporterShared<E>>,
}

impl<E> Clone for ServiceStatusReporter<E> {
    fn clone(&self) -> Self {
        Self {
            shared: self.shared.clone(),
        }
    }
}

type ReportFn<E> = Box<dyn Fn(&ServiceStatusPlan, E) + Send + Sync>;

struct ReporterShared<E> {
    state: Mutex<ReporterState>,
    heartbeat: Mutex<Option<HeartbeatThread>>,
    report: ReportFn<E>,
    ok_exit: E,
    wait_hint: Duration,
    interval: Duration,
}

struct ReporterState {
    phase: ReporterPhase,
    max_checkpoint: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReporterPhase {
    Starting,
    Running,
    Stopping { checkpoint: u32 },
    Stopped,
}

struct HeartbeatThread {
    stop_tx: std::sync::mpsc::Sender<()>,
    thread: std::thread::JoinHandle<()>,
}

impl<E: Copy + Send + Sync + 'static> ServiceStatusReporter<E> {
    /// `wait_hint` は [`PRESHUTDOWN_TIMEOUT`]、heartbeat の間隔は
    /// [`STOP_PENDING_HEARTBEAT`]。停止の見込みは既定で
    /// [`PRESHUTDOWN_TIMEOUT`]（[`Self::set_expected_stop`] で変える）。
    pub fn new<F>(report: F, ok_exit: E) -> Self
    where
        F: Fn(&ServiceStatusPlan, E) + Send + Sync + 'static,
    {
        Self::with_timing(
            report,
            ok_exit,
            PRESHUTDOWN_TIMEOUT,
            STOP_PENDING_HEARTBEAT,
            PRESHUTDOWN_TIMEOUT,
        )
    }

    fn with_timing<F>(
        report: F,
        ok_exit: E,
        wait_hint: Duration,
        interval: Duration,
        expected_stop: Duration,
    ) -> Self
    where
        F: Fn(&ServiceStatusPlan, E) + Send + Sync + 'static,
    {
        Self {
            shared: std::sync::Arc::new(ReporterShared {
                state: Mutex::new(ReporterState {
                    phase: ReporterPhase::Starting,
                    max_checkpoint: heartbeat_checkpoint_limit(expected_stop, interval),
                }),
                heartbeat: Mutex::new(None),
                report: Box::new(report),
                ok_exit,
                wait_hint,
                interval,
            }),
        }
    }

    /// 停止にかかる見込みを変える（heartbeat の checkpoint の上限）。
    pub fn set_expected_stop(&self, expected_stop: Duration) {
        let mut state = lock(&self.shared.state);
        state.max_checkpoint = heartbeat_checkpoint_limit(expected_stop, self.shared.interval);
    }

    /// Running を報告する。停止が始まった後・Stopped の後は報告せず `false`。
    pub fn report_running(&self) -> bool {
        let mut state = lock(&self.shared.state);
        if state.phase != ReporterPhase::Starting {
            return false;
        }
        state.phase = ReporterPhase::Running;
        (self.shared.report)(&running_status(), self.shared.ok_exit);
        true
    }

    /// 停止に入る: StopPending（checkpoint 1）を報告し、heartbeat を始める。
    /// 既に停止中、または Stopped を報告済みなら何もせず `false`。
    pub fn begin_stop(&self) -> bool {
        let mut state = lock(&self.shared.state);
        if !matches!(
            state.phase,
            ReporterPhase::Starting | ReporterPhase::Running
        ) {
            return false;
        }
        let Some(plan) = stop_pending_status(1, self.shared.wait_hint) else {
            return false;
        };
        state.phase = ReporterPhase::Stopping { checkpoint: 1 };
        (self.shared.report)(&plan, self.shared.ok_exit);
        // スレッドの登録もロックの中で行う（Stopped の側は、このロックを
        // 取って Stopped にしてから heartbeat を取り出す）。
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let shared = self.shared.clone();
        let thread = std::thread::spawn(move || heartbeat_loop(&shared, &stop_rx));
        *lock(&self.shared.heartbeat) = Some(HeartbeatThread { stop_tx, thread });
        true
    }

    /// Stopped を 1 回だけ報告する。以後は何も報告しない。2 回目は `false`。
    /// heartbeat のスレッドは、ロックを放してから止めて終わりを待つ。
    pub fn report_stopped(&self, exit: E) -> bool {
        let heartbeat = {
            let mut state = lock(&self.shared.state);
            if state.phase == ReporterPhase::Stopped {
                return false;
            }
            state.phase = ReporterPhase::Stopped;
            (self.shared.report)(&stopped_status(), exit);
            lock(&self.shared.heartbeat).take()
        };
        if let Some(heartbeat) = heartbeat {
            let _ = heartbeat.stop_tx.send(());
            let _ = heartbeat.thread.join();
        }
        true
    }
}

fn heartbeat_loop<E: Copy>(shared: &ReporterShared<E>, stop_rx: &std::sync::mpsc::Receiver<()>) {
    loop {
        match stop_rx.recv_timeout(shared.interval) {
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let mut state = lock(&shared.state);
                let ReporterPhase::Stopping { checkpoint } = state.phase else {
                    // Stopped（または想定外の状態）: 何も報告しない。
                    return;
                };
                if checkpoint >= state.max_checkpoint {
                    // 見込みを超えた。進めるのをやめ、SCM の判断に任せる。
                    return;
                }
                let next = checkpoint + 1;
                let Some(plan) = stop_pending_status(next, shared.wait_hint) else {
                    return;
                };
                state.phase = ReporterPhase::Stopping { checkpoint: next };
                (shared.report)(&plan, shared.ok_exit);
            }
            // 止める合図、または送り手が消えた。
            _ => return,
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 登録済みサービスの PRESHUTDOWN 待ち時間を、希望値と比べた結果。
/// 起動種別と ACL の変更は含まない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreshutdownConfigAction {
    /// 読み戻した値が既に希望と一致する。書き込まない。
    AlreadySet,
    /// 未読か、値が違う。希望値を書く。`previous_ms` は書けた場合の以前の値。
    Update {
        /// 書き込む前に読めたミリ秒。読めなければ `None`。
        previous_ms: Option<u32>,
    },
}

/// 今のミリ秒と希望のミリ秒から、書く必要があるかを決める純関数。
pub fn preshutdown_config_action(
    current_ms: Option<u32>,
    desired_ms: u32,
) -> PreshutdownConfigAction {
    match current_ms {
        Some(ms) if ms == desired_ms => PreshutdownConfigAction::AlreadySet,
        other => PreshutdownConfigAction::Update { previous_ms: other },
    }
}

/// [`ensure_preshutdown_timeout`] が読み戻しまで終えた結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreshutdownEnsureOutcome {
    /// 書き込んだか、既に一致していたか。
    pub action: PreshutdownConfigAction,
    /// 設定しようとしたミリ秒。
    pub desired_ms: u32,
    /// 最後に読み戻したミリ秒。希望と一致している。
    pub readback_ms: u32,
}

/// install が利用者に出す文言。起動種別と ACL を変えていないことを含む。
pub fn describe_preshutdown_outcome(outcome: &PreshutdownEnsureOutcome) -> String {
    match outcome.action {
        PreshutdownConfigAction::AlreadySet => format!(
            "OS シャットダウンの待ち時間（PRESHUTDOWN）は既に {} ミリ秒です（読み戻して確認しました。起動種別と ACL は変更していません）",
            outcome.readback_ms
        ),
        PreshutdownConfigAction::Update {
            previous_ms: Some(previous),
        } => format!(
            "OS シャットダウンの待ち時間（PRESHUTDOWN）を {previous} ミリ秒から {} ミリ秒に更新し、読み戻して確認しました（起動種別と ACL は変更していません）",
            outcome.desired_ms
        ),
        PreshutdownConfigAction::Update { previous_ms: None } => format!(
            "OS シャットダウンの待ち時間（PRESHUTDOWN）を {} ミリ秒に設定し、読み戻して確認しました（以前の値は読めませんでした。起動種別と ACL は変更していません）",
            outcome.desired_ms
        ),
    }
}

/// install が PRESHUTDOWN の待ち時間を揃えた結果を、利用者に出す 1 行に
/// する（[`preshutdown_report`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreshutdownReport {
    /// 揃えて読み戻しまで確認した。標準出力に出す。
    Applied(String),
    /// 揃えられなかった。**install は失敗にしない**: 標準エラー出力に
    /// 理由を 1 行残し、終了コードは 0 のまま。実際の待ち時間は確かめられて
    /// いない（以前の設定のままか、OS の既定のままの可能性がある）。新規登録の直後に失敗して「登録済みなのに
    /// 失敗」にしない・インストーラが毎回呼ぶ既存登録の経路でアップグレードを
    /// 止めないため（#589 のオーナーレビュー）。
    Warning(String),
}

/// [`ensure_preshutdown_timeout`] の結果を [`PreshutdownReport`] にする。
/// 失敗は常に [`PreshutdownReport::Warning`]（呼び出し側は終了しない）。
pub fn preshutdown_report(
    result: &Result<PreshutdownEnsureOutcome, ServiceManagerError>,
) -> PreshutdownReport {
    match result {
        Ok(outcome) => PreshutdownReport::Applied(describe_preshutdown_outcome(outcome)),
        Err(err) => PreshutdownReport::Warning(format!(
            "OS シャットダウンの待ち時間（PRESHUTDOWN）を設定できませんでした（サービスの登録はそのままです。実際の待ち時間は確認できていません - 以前の設定のままか、OS の既定のままの可能性があります。もう一度 install を実行すると設定し直します）: {err}"
        )),
    }
}

/// `timeout` をミリ秒の `u32` にする。収まらなければ `None`
/// （`set_preshutdown_timeout` は収まらない値で panic するため、呼ぶ前に弾く）。
pub fn preshutdown_timeout_ms(timeout: Duration) -> Option<u32> {
    u32::try_from(timeout.as_millis()).ok()
}

/// サービスの PRESHUTDOWN 待ち時間を `timeout` にし、読み戻して確認する。
///
/// 既に同じ値なら書き込まない。違うときだけ
/// `ChangeServiceConfig2W`（`SERVICE_CONFIG_PRESHUTDOWN_INFO`）を呼ぶ。
/// `ChangeServiceConfigW`（起動種別・実行ファイル・アカウント）と
/// サービス DACL は触らない。
///
/// サービスが未登録なら [`ServiceManagerError::NotFound`]。
#[cfg(windows)]
pub fn ensure_preshutdown_timeout(
    service_name: &str,
    timeout: Duration,
) -> Result<PreshutdownEnsureOutcome, ServiceManagerError> {
    use windows_service::service::ServiceAccess;
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    let desired_ms = preshutdown_timeout_ms(timeout).ok_or_else(|| {
        ServiceManagerError::Other("PRESHUTDOWN の待ち時間が u32 ミリ秒に収まりません".to_string())
    })?;

    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(|err| map_win_err(service_name, err))?;
    let service = manager
        .open_service(
            service_name,
            ServiceAccess::QUERY_CONFIG | ServiceAccess::CHANGE_CONFIG,
        )
        .map_err(|err| map_win_err(service_name, err))?;

    let current_ms = query_preshutdown_ms(&service).ok();
    let action = preshutdown_config_action(current_ms, desired_ms);
    if !matches!(action, PreshutdownConfigAction::AlreadySet) {
        service
            .set_preshutdown_timeout(timeout)
            .map_err(|err| map_win_err(service_name, err))?;
    }

    let readback_ms = query_preshutdown_ms(&service).map_err(|detail| {
        ServiceManagerError::Other(format!(
            "PRESHUTDOWN の待ち時間を読み戻せませんでした: {detail}"
        ))
    })?;
    if readback_ms != desired_ms {
        return Err(ServiceManagerError::Other(format!(
            "PRESHUTDOWN の待ち時間の読み戻しが一致しません（希望 {desired_ms} ミリ秒、実際 {readback_ms} ミリ秒）"
        )));
    }

    Ok(PreshutdownEnsureOutcome {
        action,
        desired_ms,
        readback_ms,
    })
}

/// `QueryServiceConfig2W` で PRESHUTDOWN のミリ秒を読む。
/// 失敗理由は OS のエラー文字列。
#[cfg(windows)]
fn query_preshutdown_ms(service: &windows_service::service::Service) -> Result<u32, String> {
    use std::mem::size_of;

    use windows_sys::Win32::System::Services::{
        QueryServiceConfig2W, SERVICE_CONFIG_PRESHUTDOWN_INFO, SERVICE_PRESHUTDOWN_INFO,
    };

    let mut info = SERVICE_PRESHUTDOWN_INFO {
        dwPreshutdownTimeout: 0,
    };
    let mut needed = 0u32;
    // SAFETY: `info` は `SERVICE_PRESHUTDOWN_INFO` の大きさだけ生きていて、
    // 呼び出しが返るまで借用する。ハンドルは `service` が持つ。
    let ok = unsafe {
        QueryServiceConfig2W(
            service.raw_handle(),
            SERVICE_CONFIG_PRESHUTDOWN_INFO,
            &mut info as *mut SERVICE_PRESHUTDOWN_INFO as *mut u8,
            size_of::<SERVICE_PRESHUTDOWN_INFO>() as u32,
            &mut needed,
        )
    };
    if ok == 0 {
        Err(std::io::Error::last_os_error().to_string())
    } else {
        Ok(info.dwPreshutdownTimeout)
    }
}

/// 計画の受理フラグを `windows-service` のビットへ写す。
/// SHUTDOWN は立てない（PRESHUTDOWN と相互排他）。
#[cfg(windows)]
pub fn controls_accepted(
    plan: &ServiceStatusPlan,
) -> windows_service::service::ServiceControlAccept {
    use windows_service::service::ServiceControlAccept;
    let mut flags = ServiceControlAccept::empty();
    if plan.accept_stop {
        flags |= ServiceControlAccept::STOP;
    }
    if plan.accept_preshutdown {
        flags |= ServiceControlAccept::PRESHUTDOWN;
    }
    flags
}

/// 計画を `ServiceStatus` にする。終了コードとサービス種別は呼び出し側の値。
#[cfg(windows)]
pub fn service_status_from_plan(
    plan: &ServiceStatusPlan,
    service_type: windows_service::service::ServiceType,
    exit_code: windows_service::service::ServiceExitCode,
) -> windows_service::service::ServiceStatus {
    use windows_service::service::{ServiceState, ServiceStatus};
    let current_state = match plan.phase {
        ServicePhase::Running => ServiceState::Running,
        ServicePhase::StopPending => ServiceState::StopPending,
        ServicePhase::Stopped => ServiceState::Stopped,
    };
    ServiceStatus {
        service_type,
        current_state,
        controls_accepted: controls_accepted(plan),
        exit_code,
        checkpoint: plan.checkpoint,
        wait_hint: plan.wait_hint,
        process_id: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 記録した報告（計画と終了コード）。
    type Seen = std::sync::Arc<Mutex<Vec<(ServiceStatusPlan, u32)>>>;

    /// 報告を記録する reporter（終了コードは u32）。
    fn recording_reporter(
        interval: Duration,
        expected_stop: Duration,
    ) -> (ServiceStatusReporter<u32>, Seen) {
        let seen = std::sync::Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let reporter = ServiceStatusReporter::with_timing(
            move |plan: &ServiceStatusPlan, exit: u32| sink.lock().unwrap().push((*plan, exit)),
            0,
            PRESHUTDOWN_TIMEOUT,
            interval,
            expected_stop,
        );
        (reporter, seen)
    }

    #[test]
    fn checkpoint_limit_divides_the_expected_stop_by_the_interval() {
        let five = Duration::from_secs(5);
        assert_eq!(heartbeat_checkpoint_limit(Duration::ZERO, five), 1);
        assert_eq!(heartbeat_checkpoint_limit(Duration::from_secs(5), five), 2);
        assert_eq!(heartbeat_checkpoint_limit(Duration::from_secs(6), five), 3);
        assert_eq!(heartbeat_checkpoint_limit(Duration::from_secs(35), five), 8);
        assert_eq!(heartbeat_checkpoint_limit(five, Duration::ZERO), 1);
    }

    /// heartbeat は checkpoint を 2 から 1 つずつ進め、上限で止まる。
    /// Stopped の後は何も報告しない。
    #[test]
    fn heartbeat_advances_up_to_the_limit_and_nothing_follows_stopped() {
        // 見込み 3 刻み → 上限 4（1 + 3）。
        let interval = Duration::from_millis(10);
        let (reporter, seen) = recording_reporter(interval, interval * 3);
        assert!(reporter.report_running());
        assert!(reporter.begin_stop());
        assert!(!reporter.begin_stop(), "2 回目の停止は報告しない");
        assert!(!reporter.report_running(), "停止の後に Running を出さない");
        std::thread::sleep(Duration::from_millis(200));
        let pending: Vec<u32> = seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(plan, _)| plan.phase == ServicePhase::StopPending)
            .map(|(plan, _)| plan.checkpoint)
            .collect();
        assert_eq!(pending, vec![1, 2, 3, 4], "上限 4 で進めるのをやめる");

        assert!(reporter.report_stopped(7));
        assert!(!reporter.report_stopped(0), "Stopped は 1 回だけ");
        assert!(!reporter.begin_stop());
        assert!(!reporter.report_running());
        std::thread::sleep(Duration::from_millis(50));
        let seen = seen.lock().unwrap();
        let (last, exit) = seen.last().copied().unwrap();
        assert_eq!(last.phase, ServicePhase::Stopped);
        assert_eq!(exit, 7);
        assert_eq!(
            seen.iter()
                .filter(|(plan, _)| plan.phase == ServicePhase::Stopped)
                .count(),
            1
        );
    }

    /// heartbeat が刻もうとしてロックを待っている間に Stopped になった場合、
    /// ロックを取った heartbeat は何も報告しない（報告の前に同じロックの中で
    /// 確かめる）。ロックをテストが握って、heartbeat を決定的に待たせる。
    #[test]
    fn a_heartbeat_waiting_for_the_lock_does_not_report_after_stopped() {
        let (reporter, seen) =
            recording_reporter(Duration::from_millis(1), Duration::from_secs(60));
        assert!(reporter.begin_stop());
        {
            let mut state = lock(&reporter.shared.state);
            // heartbeat の刻み（1ms）を何度も過ぎるまで握る - スレッドは
            // 報告しようとしてこのロックを待つ。
            std::thread::sleep(Duration::from_millis(50));
            // Stopped の報告がロックを取って状態を変えたのと同じ形。
            state.phase = ReporterPhase::Stopped;
        }
        std::thread::sleep(Duration::from_millis(50));
        let phases: Vec<ServicePhase> = seen.lock().unwrap().iter().map(|(p, _)| p.phase).collect();
        assert_eq!(
            phases,
            vec![ServicePhase::StopPending],
            "Stopped の後に heartbeat が報告した"
        );
    }

    /// #589 再レビュー P2: ハンドラが停止の合図を公開してから heartbeat を
    /// 始めるまでの間に、停止処理が先に Stopped まで進んだ場合。後から来た
    /// `begin_stop` は何も報告しない。
    #[test]
    fn a_late_begin_stop_after_stopped_reports_nothing() {
        let (reporter, seen) =
            recording_reporter(Duration::from_millis(1), Duration::from_secs(60));
        assert!(reporter.report_running());
        let notice = std::sync::Arc::new(Mutex::new(None::<StopNotice>));

        let published = std::sync::Arc::new(std::sync::Barrier::new(2));
        let resume = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handler = {
            let reporter = reporter.clone();
            let notice = notice.clone();
            let published = published.clone();
            let resume = resume.clone();
            std::thread::spawn(move || {
                // ハンドラ: 合図を公開する。
                assert!(record_first_stop(&notice, StopSignal::ServiceStop));
                published.wait();
                // ここで止まっている間に、停止処理が Stopped まで進む。
                resume.wait();
                reporter.begin_stop()
            })
        };
        published.wait();
        // 停止処理: 合図はもう公開済みなので待たずに終わり、Stopped を出す。
        assert!(notice.lock().unwrap().is_some());
        assert!(reporter.report_stopped(0));
        resume.wait();
        assert!(!handler.join().unwrap(), "Stopped の後に停止を始めた");
        std::thread::sleep(Duration::from_millis(20));
        let phases: Vec<ServicePhase> = seen.lock().unwrap().iter().map(|(p, _)| p.phase).collect();
        assert_eq!(phases, vec![ServicePhase::Running, ServicePhase::Stopped]);
    }

    /// PRESHUTDOWN を設定できなくても install は失敗にしない: どの失敗も
    /// 警告で、理由と「登録はそのまま・OS の既定で動く」を含む。
    #[test]
    fn a_preshutdown_failure_is_a_warning_not_an_install_failure() {
        for err in [
            ServiceManagerError::AccessDenied,
            ServiceManagerError::NotFound("Svc".to_owned()),
            ServiceManagerError::Other("readback mismatch".to_owned()),
        ] {
            let reason = err.to_string();
            match preshutdown_report(&Err(err)) {
                PreshutdownReport::Warning(line) => {
                    assert!(line.contains(&reason), "{line}");
                    assert!(
                        line.contains("実際の待ち時間は確認できていません"),
                        "{line}"
                    );
                    assert!(!line.contains('\n'), "1 行に収める: {line}");
                }
                PreshutdownReport::Applied(line) => panic!("失敗が成功扱い: {line}"),
            }
        }
    }

    #[test]
    fn an_applied_preshutdown_reports_the_readback() {
        let outcome = PreshutdownEnsureOutcome {
            action: PreshutdownConfigAction::AlreadySet,
            desired_ms: 30_000,
            readback_ms: 30_000,
        };
        assert_eq!(
            preshutdown_report(&Ok(outcome)),
            PreshutdownReport::Applied(describe_preshutdown_outcome(&outcome))
        );
    }

    #[test]
    fn stop_and_preshutdown_enter_the_graceful_path() {
        assert_eq!(
            stop_signal(ScmControlKind::Stop),
            Some(StopSignal::ServiceStop)
        );
        assert_eq!(
            stop_signal(ScmControlKind::Preshutdown),
            Some(StopSignal::OsPreshutdown)
        );
    }

    /// SHUTDOWN は PRESHUTDOWN と同時に受け付けられない。受理しない制御は
    /// 停止経路へ入れない（来ても NotImplemented のまま）。
    #[test]
    fn shutdown_interrogate_and_other_are_not_stop_signals() {
        for control in [
            ScmControlKind::Shutdown,
            ScmControlKind::Interrogate,
            ScmControlKind::Other,
        ] {
            assert_eq!(stop_signal(control), None);
        }
    }

    #[test]
    fn work_budget_keeps_a_margin_for_the_stopped_report() {
        assert_eq!(
            stop_work_budget(PRESHUTDOWN_TIMEOUT),
            Duration::from_secs(27)
        );
        assert_eq!(stop_work_budget(Duration::from_secs(2)), Duration::ZERO);
        assert_eq!(preshutdown_timeout_ms(PRESHUTDOWN_TIMEOUT), Some(30_000));
    }

    #[test]
    fn running_accepts_stop_and_preshutdown_only() {
        let plan = running_status();
        assert!(plan.accept_stop);
        assert!(plan.accept_preshutdown);
        assert_eq!(plan.checkpoint, 0);
        assert_eq!(plan.wait_hint, Duration::ZERO);
        assert!(!stopped_status().accept_stop);
        assert!(!stopped_status().accept_preshutdown);
    }

    #[test]
    fn stop_pending_requires_checkpoint_and_wait_hint() {
        assert!(stop_pending_status(0, Duration::from_secs(30)).is_none());
        assert!(stop_pending_status(1, Duration::ZERO).is_none());
        let plan = stop_pending_status(1, PRESHUTDOWN_TIMEOUT).expect("valid pending");
        assert_eq!(plan.phase, ServicePhase::StopPending);
        assert_eq!(plan.checkpoint, 1);
        assert_eq!(plan.wait_hint, PRESHUTDOWN_TIMEOUT);
        assert!(!plan.accept_stop);
        assert!(!plan.accept_preshutdown);
    }

    /// 通知のあと StopPending、後始末のあと Stopped が 1 回、の順。
    #[test]
    fn stop_sequence_is_pending_then_stopped_once() {
        let (pending, stopped) = stop_status_sequence(PRESHUTDOWN_TIMEOUT).expect("sequence");
        assert_eq!(pending.phase, ServicePhase::StopPending);
        assert_eq!(pending.checkpoint, 1);
        assert_eq!(stopped.phase, ServicePhase::Stopped);
        assert_eq!(stopped.checkpoint, 0);
        assert!(stop_status_sequence(Duration::ZERO).is_none());
    }

    #[test]
    fn first_stop_signal_wins() {
        let slot = Mutex::new(None);
        assert!(record_first_stop(&slot, StopSignal::OsPreshutdown));
        assert!(!record_first_stop(&slot, StopSignal::ServiceStop));
        let notice = slot.lock().unwrap().expect("recorded");
        assert_eq!(notice.signal, StopSignal::OsPreshutdown);
    }

    #[test]
    fn preshutdown_action_skips_write_when_already_set() {
        assert_eq!(
            preshutdown_config_action(Some(30_000), 30_000),
            PreshutdownConfigAction::AlreadySet
        );
        assert_eq!(
            preshutdown_config_action(Some(10_000), 30_000),
            PreshutdownConfigAction::Update {
                previous_ms: Some(10_000)
            }
        );
        assert_eq!(
            preshutdown_config_action(None, 30_000),
            PreshutdownConfigAction::Update { previous_ms: None }
        );
    }

    #[test]
    fn describe_outcome_mentions_that_start_type_and_acl_stay() {
        let already = describe_preshutdown_outcome(&PreshutdownEnsureOutcome {
            action: PreshutdownConfigAction::AlreadySet,
            desired_ms: 30_000,
            readback_ms: 30_000,
        });
        assert!(already.contains("30000"));
        assert!(already.contains("起動種別"));
        assert!(already.contains("ACL"));

        let updated = describe_preshutdown_outcome(&PreshutdownEnsureOutcome {
            action: PreshutdownConfigAction::Update {
                previous_ms: Some(10_000),
            },
            desired_ms: 30_000,
            readback_ms: 30_000,
        });
        assert!(updated.contains("10000"));
        assert!(updated.contains("30000"));
        assert!(updated.contains("起動種別"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_running_bits_are_stop_and_preshutdown_not_shutdown() {
        use windows_service::service::ServiceControlAccept;
        let accept = controls_accepted(&running_status());
        assert!(accept.contains(ServiceControlAccept::STOP));
        assert!(accept.contains(ServiceControlAccept::PRESHUTDOWN));
        assert!(!accept.contains(ServiceControlAccept::SHUTDOWN));
        let pending = stop_pending_status(1, PRESHUTDOWN_TIMEOUT).expect("pending");
        assert!(controls_accepted(&pending).is_empty());
    }
}
