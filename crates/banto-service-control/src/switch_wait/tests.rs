//! `switch_wait` のテスト。SCM は台本どおりに答える偽物、時計は眠ると進む偽物。

use super::*;
use crate::TransitionHandle;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;

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

const T: SwitchTimings = SwitchTimings {
    poll: Duration::from_secs(1),
    phase: Duration::from_secs(5),
    overall: Duration::from_secs(60),
};

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

/// 照会を前から返し、尽きたら最後の答えを繰り返す SCM。開始の呼び出しと引数を記録する。
struct ScriptedScm {
    queries: RefCell<VecDeque<ServiceStatusSummary>>,
    last: RefCell<Option<ServiceStatusSummary>>,
    start: RefCell<Option<Result<(), ServiceManagerError>>>,
    started_with: RefCell<Vec<Vec<String>>>,
}

impl ScriptedScm {
    fn new(queries: Vec<ServiceStatusSummary>) -> Self {
        Self {
            queries: RefCell::new(queries.into()),
            last: RefCell::new(None),
            start: RefCell::new(None),
            started_with: RefCell::new(Vec::new()),
        }
    }

    fn refusing(self, err: ServiceManagerError) -> Self {
        *self.start.borrow_mut() = Some(Err(err));
        self
    }
}

impl ServiceManager for ScriptedScm {
    fn query_status(&self) -> Result<ServiceStatusSummary, ServiceManagerError> {
        let next = self.queries.borrow_mut().pop_front();
        let summary = match next {
            Some(summary) => summary,
            None => self.last.borrow().clone().expect("台本が空"),
        };
        *self.last.borrow_mut() = Some(summary.clone());
        Ok(summary)
    }

    fn start(&self) -> Result<TransitionHandle, ServiceManagerError> {
        self.start_with_args(&[])
    }

    fn start_with_args(
        &self,
        args: &[std::ffi::OsString],
    ) -> Result<TransitionHandle, ServiceManagerError> {
        self.started_with.borrow_mut().push(
            args.iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect(),
        );
        match self.start.borrow_mut().take() {
            Some(Err(err)) => Err(err),
            _ => Ok(TransitionHandle::new(ScmState::Running)),
        }
    }

    fn stop(&self) -> Result<TransitionHandle, ServiceManagerError> {
        unreachable!("この台本は stop を使わない")
    }

    fn restart(&self) -> Result<TransitionHandle, ServiceManagerError> {
        unreachable!()
    }

    fn set_auto_start(&self, _enabled: bool) -> Result<(), ServiceManagerError> {
        unreachable!()
    }
}

// ---- 終了コード ----

#[test]
fn service_exit_reads_the_specific_code_only_behind_1066() {
    let read = |s: &ServiceStatusSummary| service_exit_with(s, 4, 5);
    assert_eq!(read(&stopped_with(0)), ServiceEnd::Clean);
    assert_eq!(read(&stopped_with(4)), ServiceEnd::CannotOperate);
    assert_eq!(read(&stopped_with(5)), ServiceEnd::LeftOperation);
    assert_eq!(
        read(&stopped_with(2)),
        ServiceEnd::Other {
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
        read(&os_error),
        ServiceEnd::Other {
            win32: 1067,
            specific: 0
        }
    );
    // 1066 で固有の値が 0 は正常。
    let zero = ServiceStatusSummary {
        win32_exit_code: 1066,
        ..summary(ScmState::Stopped, 0)
    };
    assert_eq!(read(&zero), ServiceEnd::Clean);
    // 製品が渡した値で読む（4・5 固定ではない）。
    assert_eq!(
        service_exit_with(&stopped_with(7), 7, 8),
        ServiceEnd::CannotOperate
    );
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

// ---- relax-only ----

fn relax(scm: &ScriptedScm) -> RelaxOnlyOutcome {
    relax_only_and_wait(scm, &FakeClock::new(), T, "relax-only", 4, 5)
}

#[test]
fn relax_only_passes_the_argument_and_reports_clean_or_left_tight() {
    // 緩め終わった（0）。pending の checkpoint が進む間は 5 秒の段を超えても待つ。
    let mut script: Vec<_> = (1..=10)
        .map(|i| summary(ScmState::StartPending, i))
        .collect();
    script.push(stopped_with(0));
    let scm = ScriptedScm::new(script);
    assert_eq!(relax(&scm), RelaxOnlyOutcome::Done(ServiceEnd::Clean));
    assert_eq!(*scm.started_with.borrow(), vec![vec!["relax-only"]]);

    // 緩められなかった（5）。
    let scm = ScriptedScm::new(vec![summary(ScmState::StartPending, 1), stopped_with(5)]);
    assert_eq!(
        relax(&scm),
        RelaxOnlyOutcome::Done(ServiceEnd::LeftOperation)
    );
}

#[test]
fn relax_only_gives_up_when_the_checkpoint_stops() {
    // checkpoint が進まない pending は、段の待ち（5 秒）で時間切れ（反証: 延長を外すと
    // 上の「進む間は待つ」が落ち、止まりを外すとこれが終わらない）。
    let scm = ScriptedScm::new(vec![summary(ScmState::StartPending, 1)]);
    assert_eq!(relax(&scm), RelaxOnlyOutcome::Stalled);
}

#[test]
fn relax_only_refuses_without_waiting_when_the_request_is_not_accepted() {
    let scm = ScriptedScm::new(vec![summary(ScmState::Running, 0)])
        .refusing(ServiceManagerError::InvalidState(ScmState::Running));
    assert_eq!(relax(&scm), RelaxOnlyOutcome::NotStopped(ScmState::Running));
    let scm = ScriptedScm::new(vec![]).refusing(ServiceManagerError::NotFound("x".into()));
    assert_eq!(relax(&scm), RelaxOnlyOutcome::NotInstalled);
    let scm = ScriptedScm::new(vec![]).refusing(ServiceManagerError::AccessDenied);
    assert!(matches!(relax(&scm), RelaxOnlyOutcome::Refused(_)));
}

#[test]
fn relax_only_stops_at_the_overall_limit_even_while_the_checkpoint_advances() {
    let script: Vec<_> = (1..=200)
        .map(|i| summary(ScmState::StartPending, i))
        .collect();
    let scm = ScriptedScm::new(script);
    assert_eq!(relax(&scm), RelaxOnlyOutcome::TimedOut);
}
