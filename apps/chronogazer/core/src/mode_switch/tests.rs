//! `mode_switch` のテスト。SCM は台本どおりに答える偽物（[`ScriptedScm`]）、時計は眠ると
//! 進む偽物（[`FakeClock`]）。

use super::*;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;

use banto_service_control::TransitionHandle;

fn summary(state: ScmState, checkpoint: u32) -> ServiceStatusSummary {
    ServiceStatusSummary {
        state,
        auto_start: false,
        pid: None,
        checkpoint,
        wait_hint: Duration::from_secs(1),
        win32_exit_code: 0,
        service_specific_exit_code: 0,
    }
}

fn stopped_with(specific: u32) -> ServiceStatusSummary {
    ServiceStatusSummary {
        win32_exit_code: if specific == 0 { 0 } else { 1066 },
        service_specific_exit_code: specific,
        ..summary(ScmState::Stopped, 0)
    }
}

/// 呼ばれた操作の記録。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    Query,
    Start(Vec<String>),
    Stop,
}

/// 台本どおりに答える SCM。照会は `queries` を前から返し、尽きたら最後の答えを繰り返す。
/// 開始・停止は `starts`・`stops` を前から返す（尽きたら `Ok`）。
struct ScriptedScm {
    queries: RefCell<VecDeque<Result<ServiceStatusSummary, ServiceManagerError>>>,
    last: RefCell<Option<ServiceStatusSummary>>,
    starts: RefCell<VecDeque<Result<(), ServiceManagerError>>>,
    stops: RefCell<VecDeque<Result<(), ServiceManagerError>>>,
    /// 開始・停止の後に照会の台本を差し替える（開始の前の照会と後の照会を分けるため）。
    after_start: RefCell<Option<Vec<ServiceStatusSummary>>>,
    calls: RefCell<Vec<Call>>,
}

impl ScriptedScm {
    fn new(queries: Vec<ServiceStatusSummary>) -> Self {
        Self {
            queries: RefCell::new(queries.into_iter().map(Ok).collect()),
            last: RefCell::new(None),
            starts: RefCell::new(VecDeque::new()),
            stops: RefCell::new(VecDeque::new()),
            after_start: RefCell::new(None),
            calls: RefCell::new(Vec::new()),
        }
    }

    fn with_starts(self, starts: Vec<Result<(), ServiceManagerError>>) -> Self {
        *self.starts.borrow_mut() = starts.into();
        self
    }

    fn with_stops(self, stops: Vec<Result<(), ServiceManagerError>>) -> Self {
        *self.stops.borrow_mut() = stops.into();
        self
    }

    fn with_after_start(self, queries: Vec<ServiceStatusSummary>) -> Self {
        *self.after_start.borrow_mut() = Some(queries);
        self
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.borrow().clone()
    }
}

impl ServiceManager for ScriptedScm {
    fn query_status(&self) -> Result<ServiceStatusSummary, ServiceManagerError> {
        self.calls.borrow_mut().push(Call::Query);
        match self.queries.borrow_mut().pop_front() {
            Some(Ok(summary)) => {
                *self.last.borrow_mut() = Some(summary.clone());
                Ok(summary)
            }
            Some(Err(err)) => Err(err),
            None => Ok(self.last.borrow().clone().expect("台本が空")),
        }
    }

    fn start(&self) -> Result<TransitionHandle, ServiceManagerError> {
        self.start_with_args(&[])
    }

    fn start_with_args(
        &self,
        args: &[std::ffi::OsString],
    ) -> Result<TransitionHandle, ServiceManagerError> {
        self.calls.borrow_mut().push(Call::Start(
            args.iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect(),
        ));
        let result = self.starts.borrow_mut().pop_front().unwrap_or(Ok(()));
        if result.is_ok() {
            if let Some(queries) = self.after_start.borrow_mut().take() {
                *self.queries.borrow_mut() = queries.into_iter().map(Ok).collect();
            }
        }
        result.map(|()| TransitionHandle::new(ScmState::Running))
    }

    fn stop(&self) -> Result<TransitionHandle, ServiceManagerError> {
        self.calls.borrow_mut().push(Call::Stop);
        self.stops
            .borrow_mut()
            .pop_front()
            .unwrap_or(Ok(()))
            .map(|()| TransitionHandle::new(ScmState::Stopped))
    }

    fn restart(&self) -> Result<TransitionHandle, ServiceManagerError> {
        unreachable!("切り替えは restart を使わない")
    }

    fn set_auto_start(&self, _enabled: bool) -> Result<(), ServiceManagerError> {
        unreachable!("自動開始は elev が設定する")
    }
}

/// 眠ると進む時計。
struct FakeClock {
    base: Instant,
    elapsed: Cell<Duration>,
}

impl FakeClock {
    fn new() -> Self {
        Self {
            base: Instant::now(),
            elapsed: Cell::new(Duration::ZERO),
        }
    }
}

impl SwitchClock for FakeClock {
    fn now(&self) -> Instant {
        self.base + self.elapsed.get()
    }

    fn sleep(&self, duration: Duration) {
        self.elapsed.set(self.elapsed.get() + duration);
    }
}

const T: SwitchTimings = SwitchTimings {
    poll: Duration::from_secs(1),
    phase: Duration::from_secs(5),
    overall: Duration::from_secs(60),
};

fn stages() -> (RefCell<Vec<SwitchStage>>,) {
    (RefCell::new(Vec::new()),)
}

// ---- 終了コード ----

#[test]
fn service_exit_reads_the_specific_code_only_behind_1066() {
    assert_eq!(service_exit(&stopped_with(0)), ServiceExit::Clean);
    assert_eq!(service_exit(&stopped_with(4)), ServiceExit::CannotOperate);
    assert_eq!(service_exit(&stopped_with(5)), ServiceExit::LeftOperation);
    assert_eq!(
        service_exit(&stopped_with(2)),
        ServiceExit::Other {
            win32: 1066,
            specific: 2
        }
    );
    // 1066 でなければ固有の値は読まない（OS のエラーで止まった）。
    let os_error = ServiceStatusSummary {
        win32_exit_code: 1067,
        service_specific_exit_code: 5,
        ..summary(ScmState::Stopped, 0)
    };
    assert_eq!(
        service_exit(&os_error),
        ServiceExit::Other {
            win32: 1067,
            specific: 0
        }
    );
    // 1066 で固有の値が 0 は正常。
    let zero = ServiceStatusSummary {
        win32_exit_code: 1066,
        ..summary(ScmState::Stopped, 0)
    };
    assert_eq!(service_exit(&zero), ServiceExit::Clean);
}

// ---- 待ち方 ----

#[test]
fn watch_extends_the_deadline_while_the_checkpoint_advances_and_fails_when_it_stalls() {
    let start = Instant::now();
    let long = SwitchTimings {
        overall: Duration::from_secs(600),
        ..T
    };
    let mut watch = PendingWatch::new(ScmState::Running, start, long);
    // 段の待ち時間（5 秒）を何度超えても、checkpoint が進む間は待つ。
    for i in 1..=20u32 {
        let now = start + Duration::from_secs(4 * u64::from(i));
        assert_eq!(
            watch.observe(now, summary(ScmState::StartPending, i)),
            WatchStep::Wait,
            "{i}"
        );
    }
    // 止まった: 最後に進んだ時刻（80 秒）+ 5 秒までは待ち、過ぎたら失敗。
    let last = start + Duration::from_secs(80);
    assert_eq!(
        watch.observe(
            last + Duration::from_secs(5),
            summary(ScmState::StartPending, 20)
        ),
        WatchStep::Wait
    );
    assert_eq!(
        watch.observe(
            last + Duration::from_secs(6),
            summary(ScmState::StartPending, 20)
        ),
        WatchStep::Stalled
    );
}

#[test]
fn watch_uses_the_longer_wait_hint_and_caps_the_whole_wait() {
    let start = Instant::now();
    let long_hint = ServiceStatusSummary {
        wait_hint: Duration::from_secs(20),
        ..summary(ScmState::StopPending, 1)
    };
    let mut watch = PendingWatch::new(ScmState::Stopped, start, T);
    assert_eq!(watch.observe(start, long_hint.clone()), WatchStep::Wait);
    assert_eq!(
        watch.observe(start + Duration::from_secs(19), long_hint.clone()),
        WatchStep::Wait
    );
    assert_eq!(
        watch.observe(start + Duration::from_secs(21), long_hint),
        WatchStep::Stalled
    );

    // 全体の上限は checkpoint が進み続けても超えない。
    let mut watch = PendingWatch::new(ScmState::Running, start, T);
    let at = start + T.overall;
    assert_eq!(
        watch.observe(at, summary(ScmState::StartPending, 99)),
        WatchStep::OverallTimeout
    );
}

#[test]
fn watch_reports_reached_and_unexpected_and_counts_query_failures_as_no_progress() {
    let start = Instant::now();
    let mut watch = PendingWatch::new(ScmState::Running, start, T);
    assert!(matches!(
        watch.observe(start, summary(ScmState::Running, 0)),
        WatchStep::Reached(_)
    ));
    assert!(matches!(
        watch.observe(start, stopped_with(4)),
        WatchStep::Unexpected(_)
    ));
    assert_eq!(watch.observe_failure(start + T.phase), WatchStep::Wait);
    assert_eq!(
        watch.observe_failure(start + T.phase + Duration::from_secs(1)),
        WatchStep::Stalled
    );
}

// ---- 運用モードへ ----

#[test]
fn start_waits_through_tightening_and_reports_running() {
    let scm = ScriptedScm::new(vec![
        summary(ScmState::StartPending, 1),
        summary(ScmState::StartPending, 2),
        summary(ScmState::Running, 0),
    ]);
    let (seen,) = stages();
    let outcome = start_service_and_wait(&scm, &FakeClock::new(), T, &mut |stage| {
        seen.borrow_mut().push(stage)
    });
    assert_eq!(outcome, StartOutcome::Running);
    assert_eq!(scm.calls()[0], Call::Start(vec![]));
    assert_eq!(
        seen.into_inner(),
        vec![
            SwitchStage::StartingService,
            SwitchStage::TighteningPermissions
        ]
    );
}

#[test]
fn start_reports_why_the_service_stopped_without_running() {
    for (specific, exit) in [
        (4, ServiceExit::CannotOperate),
        (5, ServiceExit::LeftOperation),
    ] {
        let scm = ScriptedScm::new(vec![
            summary(ScmState::StartPending, 1),
            stopped_with(specific),
        ]);
        let outcome = start_service_and_wait(&scm, &FakeClock::new(), T, &mut |_| {});
        assert_eq!(outcome, StartOutcome::Ended(exit), "{specific}");
    }
}

#[test]
fn start_fails_when_the_checkpoint_stops_and_maps_start_errors() {
    let scm = ScriptedScm::new(vec![summary(ScmState::StartPending, 3)]);
    assert_eq!(
        start_service_and_wait(&scm, &FakeClock::new(), T, &mut |_| {}),
        StartOutcome::Stalled
    );
    let scm = ScriptedScm::new(vec![summary(ScmState::Stopped, 0)]).with_starts(vec![Err(
        ServiceManagerError::NotFound("ChronoGazer".into()),
    )]);
    assert_eq!(
        start_service_and_wait(&scm, &FakeClock::new(), T, &mut |_| {}),
        StartOutcome::NotInstalled
    );
    let scm = ScriptedScm::new(vec![summary(ScmState::Stopped, 0)])
        .with_starts(vec![Err(ServiceManagerError::AccessDenied)]);
    assert!(matches!(
        start_service_and_wait(&scm, &FakeClock::new(), T, &mut |_| {}),
        StartOutcome::StartFailed(_)
    ));
    // 1056（照会と開始の間に別の操作で開始した）は失敗にせず Running を待つ。
    let scm = ScriptedScm::new(vec![summary(ScmState::Running, 0)])
        .with_starts(vec![Err(ServiceManagerError::AlreadyRunning)]);
    assert_eq!(
        start_service_and_wait(&scm, &FakeClock::new(), T, &mut |_| {}),
        StartOutcome::Running
    );
}

// ---- 設定モードへ ----

#[test]
fn stop_waits_for_relaxing_and_reads_the_exit_code() {
    for (specific, exit) in [(0, ServiceExit::Clean), (5, ServiceExit::LeftOperation)] {
        let scm = ScriptedScm::new(vec![
            summary(ScmState::StopPending, 1),
            summary(ScmState::StopPending, 2),
            stopped_with(specific),
        ]);
        let (seen,) = stages();
        let outcome = stop_service_and_wait(&scm, &FakeClock::new(), T, &mut |stage| {
            seen.borrow_mut().push(stage)
        });
        assert_eq!(outcome, StopOutcome::Stopped(exit));
        assert_eq!(
            seen.into_inner(),
            vec![
                SwitchStage::StoppingService,
                SwitchStage::RelaxingPermissions
            ]
        );
    }
}

/// 締めている間の STOP は 1061（か 1052）で断られる。失敗にせず Running を待って出し直す。
#[test]
fn stop_refused_with_1061_waits_for_running_then_stops_again() {
    let scm = ScriptedScm::new(vec![
        summary(ScmState::StartPending, 4),
        summary(ScmState::StartPending, 5),
        summary(ScmState::Running, 0),
        summary(ScmState::StopPending, 1),
        stopped_with(0),
    ])
    .with_stops(vec![Err(ServiceManagerError::CannotAcceptControl), Ok(())]);
    let (seen,) = stages();
    let outcome = stop_service_and_wait(&scm, &FakeClock::new(), T, &mut |stage| {
        seen.borrow_mut().push(stage)
    });
    assert_eq!(outcome, StopOutcome::Stopped(ServiceExit::Clean));
    let stops = scm.calls().iter().filter(|c| **c == Call::Stop).count();
    assert_eq!(stops, 2);
    assert!(seen
        .into_inner()
        .contains(&SwitchStage::WaitingForTightening));
}

#[test]
fn stop_refused_with_1061_takes_the_stop_on_its_own_as_the_result() {
    // 締める途中で自分から止まった（4）: それが停止の結果（出し直さない）。
    let scm = ScriptedScm::new(vec![summary(ScmState::StartPending, 1), stopped_with(4)])
        .with_stops(vec![Err(ServiceManagerError::CannotAcceptControl)]);
    assert_eq!(
        stop_service_and_wait(&scm, &FakeClock::new(), T, &mut |_| {}),
        StopOutcome::Stopped(ServiceExit::CannotOperate)
    );
    assert_eq!(scm.calls().iter().filter(|c| **c == Call::Stop).count(), 1);
}

/// START_PENDING の STOP は 1052 でも断られる（実機）。1061 と同じに待って出し直す。
#[test]
fn stop_refused_with_1052_while_start_pending_waits_for_running_then_stops_again() {
    let scm = ScriptedScm::new(vec![
        summary(ScmState::StartPending, 4),
        summary(ScmState::StartPending, 5),
        summary(ScmState::Running, 0),
        summary(ScmState::StopPending, 1),
        stopped_with(0),
    ])
    .with_stops(vec![Err(ServiceManagerError::InvalidControl), Ok(())]);
    let outcome = stop_service_and_wait(&scm, &FakeClock::new(), T, &mut |_| {});
    assert_eq!(outcome, StopOutcome::Stopped(ServiceExit::Clean));
    assert_eq!(scm.calls().iter().filter(|c| **c == Call::Stop).count(), 2);
}

#[test]
fn stop_refused_with_1052_while_start_pending_takes_the_stop_on_its_own_as_the_result() {
    let scm = ScriptedScm::new(vec![summary(ScmState::StartPending, 1), stopped_with(4)])
        .with_stops(vec![Err(ServiceManagerError::InvalidControl)]);
    assert_eq!(
        stop_service_and_wait(&scm, &FakeClock::new(), T, &mut |_| {}),
        StopOutcome::Stopped(ServiceExit::CannotOperate)
    );
    assert_eq!(scm.calls().iter().filter(|c| **c == Call::Stop).count(), 1);
}

/// 1052 の直後に起動が終わっていた（Running）: STOP を 1 回だけ出し直す。
#[test]
fn stop_refused_with_1052_then_running_resends_the_stop_once() {
    let scm = ScriptedScm::new(vec![
        summary(ScmState::Running, 0),
        summary(ScmState::StopPending, 1),
        stopped_with(0),
    ])
    .with_stops(vec![Err(ServiceManagerError::InvalidControl), Ok(())]);
    assert_eq!(
        stop_service_and_wait(&scm, &FakeClock::new(), T, &mut |_| {}),
        StopOutcome::Stopped(ServiceExit::Clean)
    );
    assert_eq!(scm.calls().iter().filter(|c| **c == Call::Stop).count(), 2);
}

/// 再送でも 1052 なら、そのとき初めて失敗にする（繰り返さない）。
#[test]
fn stop_refused_with_1052_while_running_twice_is_a_failure() {
    let scm = ScriptedScm::new(vec![summary(ScmState::Running, 0)]).with_stops(vec![
        Err(ServiceManagerError::InvalidControl),
        Err(ServiceManagerError::InvalidControl),
    ]);
    assert!(matches!(
        stop_service_and_wait(&scm, &FakeClock::new(), T, &mut |_| {}),
        StopOutcome::StopFailed(_)
    ));
    assert_eq!(scm.calls().iter().filter(|c| **c == Call::Stop).count(), 2);
}

/// 1052 の直後の照会が Stopped（pending を挟まない）: その結果を返す。
#[test]
fn stop_refused_with_1052_then_stopped_returns_that_result() {
    let scm = ScriptedScm::new(vec![stopped_with(4)])
        .with_stops(vec![Err(ServiceManagerError::InvalidControl)]);
    assert_eq!(
        stop_service_and_wait(&scm, &FakeClock::new(), T, &mut |_| {}),
        StopOutcome::Stopped(ServiceExit::CannotOperate)
    );
    assert_eq!(scm.calls().iter().filter(|c| **c == Call::Stop).count(), 1);
}

#[test]
fn stop_gives_up_after_the_retry_limit_and_on_a_stalled_relax() {
    let scm = ScriptedScm::new(vec![summary(ScmState::Running, 0)]).with_stops(vec![
        Err(
            ServiceManagerError::CannotAcceptControl
        );
        (MAX_STOP_RETRIES + 1)
            as usize
    ]);
    assert!(matches!(
        stop_service_and_wait(&scm, &FakeClock::new(), T, &mut |_| {}),
        StopOutcome::StopFailed(_)
    ));
    assert_eq!(
        scm.calls().iter().filter(|c| **c == Call::Stop).count(),
        (MAX_STOP_RETRIES + 1) as usize
    );

    let scm = ScriptedScm::new(vec![summary(ScmState::StopPending, 7)]);
    assert_eq!(
        stop_service_and_wait(&scm, &FakeClock::new(), T, &mut |_| {}),
        StopOutcome::Stalled
    );
}

// ---- 緩めるだけの開始 ----

/// 開始の直前の Stopped（前の終了コード 5 を載せたまま）を完了と取り違えない: 照会は
/// `Ok` の後に始める。
#[test]
fn relax_only_queries_only_after_the_start_returned() {
    let scm = ScriptedScm::new(vec![stopped_with(5)])
        .with_after_start(vec![summary(ScmState::StartPending, 1), stopped_with(0)]);
    let outcome = relax_only_and_wait(&scm, &FakeClock::new(), T, &mut |_| {});
    assert_eq!(outcome, RelaxOnlyOutcome::Done(ServiceExit::Clean));
    let calls = scm.calls();
    assert_eq!(calls[0], Call::Start(vec!["relax-only".to_string()]));
    assert!(calls[1..].iter().all(|c| *c == Call::Query), "{calls:?}");
}

#[test]
fn relax_only_reports_left_operation_and_maps_refusals() {
    let scm = ScriptedScm::new(vec![stopped_with(0)]).with_after_start(vec![stopped_with(5)]);
    assert_eq!(
        relax_only_and_wait(&scm, &FakeClock::new(), T, &mut |_| {}),
        RelaxOnlyOutcome::Done(ServiceExit::LeftOperation)
    );
    let scm = ScriptedScm::new(vec![summary(ScmState::Running, 0)]).with_starts(vec![Err(
        ServiceManagerError::InvalidState(ScmState::Running),
    )]);
    assert_eq!(
        relax_only_and_wait(&scm, &FakeClock::new(), T, &mut |_| {}),
        RelaxOnlyOutcome::NotStopped(ScmState::Running)
    );
    let scm = ScriptedScm::new(vec![summary(ScmState::Running, 0)])
        .with_starts(vec![Err(ServiceManagerError::AlreadyRunning)]);
    assert_eq!(
        relax_only_and_wait(&scm, &FakeClock::new(), T, &mut |_| {}),
        RelaxOnlyOutcome::NotStopped(ScmState::Running)
    );
    let scm = ScriptedScm::new(vec![summary(ScmState::Stopped, 0)]).with_starts(vec![Err(
        ServiceManagerError::NotFound("ChronoGazer".into()),
    )]);
    assert_eq!(
        relax_only_and_wait(&scm, &FakeClock::new(), T, &mut |_| {}),
        RelaxOnlyOutcome::NotInstalled
    );
    let scm = ScriptedScm::new(vec![summary(ScmState::Stopped, 0)])
        .with_starts(vec![Err(ServiceManagerError::AccessDenied)]);
    assert!(matches!(
        relax_only_and_wait(&scm, &FakeClock::new(), T, &mut |_| {}),
        RelaxOnlyOutcome::Refused(_)
    ));
    // Running になった（緩めるだけのはずが通常の開始になった）は想定外として返す。
    let scm = ScriptedScm::new(vec![stopped_with(0)])
        .with_after_start(vec![summary(ScmState::Running, 0)]);
    assert_eq!(
        relax_only_and_wait(&scm, &FakeClock::new(), T, &mut |_| {}),
        RelaxOnlyOutcome::Unexpected(ScmState::Running)
    );
}

// ---- デスクトップのログ ----

#[test]
fn desktop_log_appends_lines_with_the_time_each_happened() {
    let dir = crate::test_support::TempDir::new();
    let logs = dir.path().join("logs");
    // 2026-10-19 09:00:00 JST。
    let clock = banto_tstore::ManualClock::new(1_792_368_000_000, 9 * 3_600_000);
    let started = DesktopLogEntry {
        at_ms: 1_792_368_000_000 - 65_000,
        text: LOG_TO_SETTING_STARTED.to_string(),
    };
    let done = DesktopLogEntry::now(&clock, LOG_TO_SETTING_COMPLETED);
    let path = append_desktop_log(&logs, &clock, &[started, done]).expect("書ける");
    assert_eq!(
        path.file_name().unwrap().to_string_lossy(),
        "chronogazer-desktop-2026-10-19.log"
    );
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        text,
        format!(
            "[2026-10-19 08:58:55] {LOG_TO_SETTING_STARTED}\n[2026-10-19 09:00:00] {LOG_TO_SETTING_COMPLETED}\n"
        )
    );
    // 追記（上書きしない）。
    append_desktop_log(&logs, &clock, &[DesktopLogEntry::now(&clock, "3 行目")]).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 3);
}

// ---- 引き継ぎ ----

#[test]
fn handoff_args_round_trip_and_only_exact_shapes_count() {
    let args = handoff_args(4242);
    assert_eq!(
        parse_handoff(args.iter().map(String::as_str)),
        Handoff {
            to_operation: true,
            wait_pid: Some(4242)
        }
    );
    assert_eq!(parse_handoff([]), Handoff::default());
    for other in [
        "--Switch-To-Operation",
        "--switch-to-operation=1",
        "switch-to-operation",
    ] {
        assert_eq!(parse_handoff([other]), Handoff::default(), "{other}");
    }
    for bad in [
        "--wait-pid=",
        "--wait-pid=0",
        "--wait-pid=-1",
        "--wait-pid=x",
    ] {
        assert_eq!(parse_handoff([bad]).wait_pid, None, "{bad}");
    }
}

#[test]
fn waiting_for_a_finished_process_returns_at_once() {
    let mut child = std::process::Command::new(if cfg!(windows) { "cmd" } else { "true" })
        .args(if cfg!(windows) {
            vec!["/C", "exit 0"]
        } else {
            vec![]
        })
        .spawn()
        .expect("子プロセスを起動できる");
    let pid = child.id();
    child.wait().unwrap();
    assert!(wait_for_process_exit(pid, Duration::from_secs(5)));
    // 自分自身は終わらない: 上限で偽。
    assert!(!wait_for_process_exit(
        std::process::id(),
        Duration::from_millis(200)
    ));
}
