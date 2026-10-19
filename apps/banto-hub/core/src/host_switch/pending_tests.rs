//! 切り替えの待ち（checkpoint で延びる）・終了コード 4・5・STOP の 1052／1061・`relax-only`
//! のテスト（#661 661-4）。SCM は台本どおりに答える偽物、時計は engine の`clock_offset`を進める。
//! 全 OS で動く（SCM には触れない）。

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use banto_service_control::TransitionHandle;

use super::*;
use crate::hub_health::{HealthOutcome, MockHubHealthProbe};

const PHASE: Duration = Duration::from_secs(5);

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

#[derive(Default)]
struct Inner {
    /// 照会は前から返し、尽きたら最後の答えを繰り返す。
    queries: RefCell<VecDeque<ServiceStatusSummary>>,
    last: RefCell<Option<ServiceStatusSummary>>,
    /// STOP の答え（尽きたら`Ok`）。
    stops: RefCell<VecDeque<Result<(), ServiceManagerError>>>,
    /// START の答え（尽きたら`Ok`）。
    starts: RefCell<VecDeque<Result<(), ServiceManagerError>>>,
    stop_calls: RefCell<u32>,
    start_args: RefCell<Vec<Vec<String>>>,
}

#[derive(Clone, Default)]
struct Scripted(Rc<Inner>);

impl Scripted {
    fn with_state(state: ServiceStatusSummary) -> Self {
        let scm = Self::default();
        scm.set(state);
        scm
    }

    /// 以後の照会の答えを差し替える（尽きたらこれを繰り返す）。
    fn set(&self, state: ServiceStatusSummary) {
        *self.0.queries.borrow_mut() = VecDeque::new();
        *self.0.last.borrow_mut() = Some(state);
    }

    fn stops(self, stops: Vec<Result<(), ServiceManagerError>>) -> Self {
        *self.0.stops.borrow_mut() = stops.into();
        self
    }

    fn stop_calls(&self) -> u32 {
        *self.0.stop_calls.borrow()
    }
}

impl ServiceManager for Scripted {
    fn query_status(&self) -> Result<ServiceStatusSummary, ServiceManagerError> {
        let next = self.0.queries.borrow_mut().pop_front();
        if let Some(next) = next {
            *self.0.last.borrow_mut() = Some(next);
        }
        Ok(self.0.last.borrow().clone().expect("台本が空"))
    }

    fn start(&self) -> Result<TransitionHandle, ServiceManagerError> {
        self.start_with_args(&[])
    }

    fn start_with_args(
        &self,
        args: &[std::ffi::OsString],
    ) -> Result<TransitionHandle, ServiceManagerError> {
        self.0.start_args.borrow_mut().push(
            args.iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect(),
        );
        self.0
            .starts
            .borrow_mut()
            .pop_front()
            .unwrap_or(Ok(()))
            .map(|()| TransitionHandle::new(ScmState::Running))
    }

    fn stop(&self) -> Result<TransitionHandle, ServiceManagerError> {
        *self.0.stop_calls.borrow_mut() += 1;
        self.0
            .stops
            .borrow_mut()
            .pop_front()
            .unwrap_or(Ok(()))
            .map(|()| TransitionHandle::new(ScmState::Stopped))
    }

    fn restart(&self) -> Result<TransitionHandle, ServiceManagerError> {
        unreachable!()
    }

    fn set_auto_start(&self, _enabled: bool) -> Result<(), ServiceManagerError> {
        unreachable!()
    }
}

fn engine(
    scm: &Scripted,
    initial_host: HostKind,
) -> HostSwitchEngine<Scripted, MockHubHealthProbe, MockDesktopHostControl> {
    HostSwitchEngine::new(
        scm.clone(),
        MockHubHealthProbe::unreachable(),
        MockDesktopHostControl::new(),
        HostSwitchConfig {
            expected_profile: "default".to_string(),
            expected_port: 8722,
            can_operate_service: true,
            initial_host,
            phase_timeout: PHASE,
        },
    )
}

type TestEngine = HostSwitchEngine<Scripted, MockHubHealthProbe, MockDesktopHostControl>;

fn advance(engine: &mut TestEngine, seconds: u64) {
    engine.clock_offset += Duration::from_secs(seconds);
}

fn poll(engine: &mut TestEngine) -> StepOutcome {
    engine.step(SwitchCommand::Poll).unwrap()
}

fn faulted(outcome: StepOutcome) -> (HostKind, FaultStage, String) {
    match outcome {
        StepOutcome::Faulted {
            reached,
            stage,
            reason,
        } => (reached, stage, reason),
        other => panic!("expected Faulted, got {other:?}"),
    }
}

// ---- 運用モードへ: checkpoint で延びる ----

#[test]
fn service_starting_extends_while_the_checkpoint_advances_then_fails_when_it_stalls() {
    let scm = Scripted::with_state(summary(ScmState::StartPending, 1));
    let mut engine = engine(&scm, HostKind::Offline);
    assert_eq!(
        engine.step(SwitchCommand::SwitchToService).unwrap(),
        StepOutcome::Progressed
    );
    // 段の待ち時間（5 秒）を何度超えても、checkpoint が進む間は待つ（反証: 延長を外すと 2 回目で
    // 落ちる）。
    for i in 1..=10u32 {
        advance(&mut engine, 4);
        scm.set(summary(ScmState::StartPending, i));
        assert_eq!(poll(&mut engine), StepOutcome::Waiting, "{i}");
    }
    // 止まった: 最後に進んだ時刻 + 5 秒（実時間が少し進むので 4 秒で確かめる）までは待ち、過ぎたら失敗（反証: 止まりを見ないと永久に待つ）。
    advance(&mut engine, 4);
    assert_eq!(poll(&mut engine), StepOutcome::Waiting);
    advance(&mut engine, 2);
    let (reached, stage, reason) = faulted(poll(&mut engine));
    assert_eq!(
        (reached, stage),
        (HostKind::Offline, FaultStage::ServiceStarting)
    );
    assert!(reason.contains("checkpoint"), "{reason}");
}

#[test]
fn service_starting_stops_at_the_overall_limit_even_while_the_checkpoint_advances() {
    let scm = Scripted::with_state(summary(ScmState::StartPending, 1));
    let mut engine = engine(&scm, HostKind::Offline);
    engine.set_overall_limit(Duration::from_secs(30));
    engine.step(SwitchCommand::SwitchToService).unwrap();
    for i in 1..=7u32 {
        advance(&mut engine, 4);
        scm.set(summary(ScmState::StartPending, i));
        assert_eq!(poll(&mut engine), StepOutcome::Waiting, "{i}");
    }
    advance(&mut engine, 4);
    scm.set(summary(ScmState::StartPending, 8));
    let (_, stage, reason) = faulted(poll(&mut engine));
    assert_eq!(stage, FaultStage::ServiceStarting);
    assert!(reason.contains("全体の上限"), "{reason}");
}

// ---- 運用モードへ: Running を経ずに止まる ----

#[test]
fn service_that_stops_without_running_is_told_apart_by_exit_code() {
    for (code, expected) in [
        (4, Some(ServiceFault::CannotOperate)),
        (5, Some(ServiceFault::LeftTight)),
        (2, None),
        (0, None),
    ] {
        let scm = Scripted::with_state(summary(ScmState::StartPending, 1));
        let mut engine = engine(&scm, HostKind::Offline);
        engine.step(SwitchCommand::SwitchToService).unwrap();
        assert_eq!(poll(&mut engine), StepOutcome::Waiting);
        scm.set(stopped_with(code));
        let (reached, stage, reason) = faulted(poll(&mut engine));
        assert_eq!(
            (reached, stage),
            (HostKind::Offline, FaultStage::ServiceStarting),
            "{code}"
        );
        assert_eq!(engine.state().service_fault, expected, "{code}: {reason}");
        if expected.is_none() {
            // 4・5 以外は従来の失敗（予期しない状態）。
            assert!(reason.contains("予期しない"), "{code}: {reason}");
        }
    }
}

#[test]
fn left_tight_survives_a_failed_start_and_still_blocks_the_desktop() {
    let scm = Scripted::with_state(stopped_with(5));
    let mut engine = engine(&scm, HostKind::Offline);
    // 観測: Stopped/5 で LeftTight。
    engine.step(SwitchCommand::SwitchToService).unwrap();
    let (reached, _, _) = faulted(poll(&mut engine));
    assert_eq!(reached, HostKind::Offline);
    assert_eq!(engine.state().service_fault, Some(ServiceFault::LeftTight));
    // 再度 SwitchToService、start が AccessDenied で失敗しても印は消えない。
    scm.0
        .starts
        .borrow_mut()
        .push_back(Err(ServiceManagerError::AccessDenied));
    let (_, stage, _) = faulted(engine.step(SwitchCommand::SwitchToService).unwrap());
    assert_eq!(stage, FaultStage::ServiceStarting);
    assert_eq!(engine.state().service_fault, Some(ServiceFault::LeftTight));
    // SwitchToDesktop は SCM を照会し直し、Stopped/5 のままなら Desktop にしない。
    let (reached, stage, _) = faulted(engine.step(SwitchCommand::SwitchToDesktop).unwrap());
    assert_eq!(
        (reached, stage),
        (HostKind::Offline, FaultStage::AwaitingServiceRelease)
    );
    assert_eq!(engine.state().service_fault, Some(ServiceFault::LeftTight));
    // Stopped/0 になれば進む。
    scm.set(stopped_with(0));
    assert_eq!(
        engine.step(SwitchCommand::SwitchToDesktop).unwrap(),
        StepOutcome::Completed(HostKind::Desktop)
    );
    assert_eq!(engine.state().service_fault, None);
}

#[test]
fn left_tight_is_cleared_once_the_service_is_running_and_healthy() {
    let scm = Scripted::with_state(stopped_with(5));
    let mut engine = engine(&scm, HostKind::Offline);
    engine.step(SwitchCommand::SwitchToService).unwrap();
    faulted(poll(&mut engine));
    assert_eq!(engine.state().service_fault, Some(ServiceFault::LeftTight));
    scm.set(summary(ScmState::Running, 0));
    engine.probe.set_outcome(HealthOutcome::Healthy {
        version: "1.2.3".to_string(),
    });
    engine.step(SwitchCommand::SwitchToService).unwrap();
    assert_eq!(engine.state().service_fault, Some(ServiceFault::LeftTight));
    for _ in 0..5 {
        if poll(&mut engine) == StepOutcome::Completed(HostKind::Service) {
            break;
        }
    }
    assert_eq!(engine.state().current, HostKind::Service);
    assert_eq!(engine.state().service_fault, None);
}

#[test]
fn service_fault_is_cleared_when_a_new_switch_starts() {
    let scm = Scripted::with_state(summary(ScmState::StartPending, 1));
    let mut engine = engine(&scm, HostKind::Offline);
    engine.step(SwitchCommand::SwitchToService).unwrap();
    scm.set(stopped_with(4));
    poll(&mut engine);
    assert_eq!(
        engine.state().service_fault,
        Some(ServiceFault::CannotOperate)
    );
    scm.set(summary(ScmState::StartPending, 1));
    engine.step(SwitchCommand::SwitchToService).unwrap();
    assert_eq!(engine.state().service_fault, None);
}

// ---- 設定モードへ: STOP の 1061／1052 ----

fn to_desktop_engine(scm: &Scripted) -> TestEngine {
    engine(scm, HostKind::Service)
}

#[test]
fn stop_refused_with_1061_waits_for_the_switch_then_resends_the_stop() {
    let scm = Scripted::with_state(summary(ScmState::StartPending, 1))
        .stops(vec![Err(ServiceManagerError::CannotAcceptControl)]);
    let mut engine = to_desktop_engine(&scm);
    assert_eq!(
        engine.step(SwitchCommand::SwitchToDesktop).unwrap(),
        StepOutcome::Progressed
    );
    assert_eq!(engine.state().phase, SwitchPhase::ServiceStopping);
    assert_eq!(poll(&mut engine), StepOutcome::Waiting);
    // 締め終わって Running になったら、STOP を出し直す。
    scm.set(summary(ScmState::Running, 0));
    assert_eq!(poll(&mut engine), StepOutcome::Progressed);
    assert_eq!(engine.state().phase, SwitchPhase::AwaitingServiceRelease);
    assert_eq!(scm.stop_calls(), 2);
    scm.set(stopped_with(0));
    assert_eq!(poll(&mut engine), StepOutcome::Completed(HostKind::Desktop));
}

#[test]
fn stop_refused_with_1052_follows_the_state_seen_after_it() {
    // Stopped: 結果を取る（STOP は出し直さない）。
    let scm =
        Scripted::with_state(stopped_with(4)).stops(vec![Err(ServiceManagerError::InvalidControl)]);
    let mut engine = to_desktop_engine(&scm);
    assert_eq!(
        engine.step(SwitchCommand::SwitchToDesktop).unwrap(),
        StepOutcome::Progressed
    );
    assert_eq!(engine.state().phase, SwitchPhase::AwaitingServiceRelease);
    assert_eq!(poll(&mut engine), StepOutcome::Completed(HostKind::Desktop));
    assert_eq!(scm.stop_calls(), 1);

    // Running: STOP を出し直す（1 回の競合）。
    let scm = Scripted::with_state(summary(ScmState::Running, 0))
        .stops(vec![Err(ServiceManagerError::InvalidControl)]);
    let mut engine = to_desktop_engine(&scm);
    engine.step(SwitchCommand::SwitchToDesktop).unwrap();
    assert_eq!(engine.state().phase, SwitchPhase::ServiceStopping);
    assert_eq!(poll(&mut engine), StepOutcome::Progressed);
    assert_eq!(engine.state().phase, SwitchPhase::AwaitingServiceRelease);
    assert_eq!(scm.stop_calls(), 2);

    // pending: 切り替え中として待つ（失敗にしない）。
    let scm = Scripted::with_state(summary(ScmState::StartPending, 3))
        .stops(vec![Err(ServiceManagerError::InvalidControl)]);
    let mut engine = to_desktop_engine(&scm);
    engine.step(SwitchCommand::SwitchToDesktop).unwrap();
    assert_eq!(engine.state().phase, SwitchPhase::ServiceStopping);
    assert_eq!(poll(&mut engine), StepOutcome::Waiting);
    assert_eq!(scm.stop_calls(), 1);
}

#[test]
fn stop_refused_forever_gives_up_after_the_retry_limit() {
    let refuse = || Err(ServiceManagerError::CannotAcceptControl);
    let scm = Scripted::with_state(summary(ScmState::Running, 0))
        .stops((0..10).map(|_| refuse()).collect());
    let mut engine = to_desktop_engine(&scm);
    engine.step(SwitchCommand::SwitchToDesktop).unwrap();
    let mut last = StepOutcome::Waiting;
    for _ in 0..10 {
        last = poll(&mut engine);
        if matches!(last, StepOutcome::Faulted { .. }) {
            break;
        }
    }
    let (reached, stage, _) = faulted(last);
    assert_eq!(
        (reached, stage),
        (HostKind::Service, FaultStage::ServiceStopping)
    );
    // 最初の 1 回 + MAX_STOP_RETRIES 回の出し直し。
    assert_eq!(scm.stop_calls(), 1 + MAX_STOP_RETRIES);
}

#[test]
fn stop_refused_for_other_reasons_is_still_a_plain_failure() {
    let scm = Scripted::with_state(summary(ScmState::Running, 0))
        .stops(vec![Err(ServiceManagerError::AccessDenied)]);
    let mut engine = to_desktop_engine(&scm);
    let (reached, stage, _) = faulted(engine.step(SwitchCommand::SwitchToDesktop).unwrap());
    assert_eq!(
        (reached, stage),
        (HostKind::Service, FaultStage::ServiceStopping)
    );
}

// ---- 設定モードへ: STOP_PENDING の待ちと終了コード ----

#[test]
fn service_release_extends_while_stop_pending_progresses_and_fails_when_it_stalls() {
    let scm = Scripted::with_state(summary(ScmState::StopPending, 1));
    let mut engine = to_desktop_engine(&scm);
    engine.step(SwitchCommand::SwitchToDesktop).unwrap();
    for i in 1..=10u32 {
        advance(&mut engine, 4);
        scm.set(summary(ScmState::StopPending, i));
        assert_eq!(poll(&mut engine), StepOutcome::Waiting, "{i}");
    }
    advance(&mut engine, 6);
    let (reached, stage, _) = faulted(poll(&mut engine));
    assert_eq!(
        (reached, stage),
        (HostKind::Service, FaultStage::AwaitingServiceRelease)
    );
}

#[test]
fn stopped_with_left_tight_does_not_hand_over_to_the_desktop() {
    let scm = Scripted::with_state(summary(ScmState::StopPending, 1));
    let mut engine = to_desktop_engine(&scm);
    engine.step(SwitchCommand::SwitchToDesktop).unwrap();
    assert_eq!(poll(&mut engine), StepOutcome::Waiting);
    scm.set(stopped_with(5));
    let (reached, stage, _) = faulted(poll(&mut engine));
    assert_eq!(
        (reached, stage),
        (HostKind::Offline, FaultStage::AwaitingServiceRelease)
    );
    assert_eq!(engine.state().service_fault, Some(ServiceFault::LeftTight));
    assert_eq!(engine.state().current, HostKind::Offline);

    // 再要求しても、終了コード 5 のままなら Desktop には渡さない。
    let (reached, stage, _) = faulted(engine.step(SwitchCommand::SwitchToDesktop).unwrap());
    assert_eq!(
        (reached, stage),
        (HostKind::Offline, FaultStage::AwaitingServiceRelease)
    );
    assert_eq!(engine.state().service_fault, Some(ServiceFault::LeftTight));
    assert_eq!(engine.state().current, HostKind::Offline);

    // サービスが動き出していたら（終了コードが 5 でなくても）Desktop には渡さない。
    scm.set(summary(ScmState::Running, 0));
    let (reached, stage, _) = faulted(engine.step(SwitchCommand::SwitchToDesktop).unwrap());
    assert_eq!(
        (reached, stage),
        (HostKind::Offline, FaultStage::AwaitingServiceRelease)
    );
    assert_eq!(engine.state().service_fault, Some(ServiceFault::LeftTight));
    assert_eq!(engine.state().current, HostKind::Offline);

    // relax-only で緩めて、止まって終了コードが 0 になったら通る。
    scm.set(stopped_with(0));
    assert_eq!(
        engine.step(SwitchCommand::SwitchToDesktop).unwrap(),
        StepOutcome::Completed(HostKind::Desktop)
    );
    assert_eq!(engine.state().service_fault, None);
}

#[test]
fn stopping_again_with_left_tight_after_the_first_stopped_does_not_hand_over() {
    // 一度 Stopped を見た後に動き出し、終了コード 5 で止まり直した: 締めたまま残っている。
    let scm = Scripted::with_state(summary(ScmState::StopPending, 1));
    let mut engine = to_desktop_engine(&scm);
    engine.probe.set_outcome(HealthOutcome::Healthy {
        version: "1.2.3".to_string(),
    });
    engine.step(SwitchCommand::SwitchToDesktop).unwrap();
    scm.set(stopped_with(0));
    assert_eq!(poll(&mut engine), StepOutcome::Waiting);
    scm.set(summary(ScmState::Running, 0));
    assert_eq!(poll(&mut engine), StepOutcome::Waiting);
    scm.set(stopped_with(5));
    let (reached, stage, _) = faulted(poll(&mut engine));
    assert_eq!(
        (reached, stage),
        (HostKind::Offline, FaultStage::AwaitingServiceRelease)
    );
    assert_eq!(engine.state().service_fault, Some(ServiceFault::LeftTight));
}

#[test]
fn stopped_with_cannot_operate_still_completes_the_switch_to_the_desktop() {
    // 締めている途中で 4 になって止まった: 設定モードのままなのでデスクトップへ渡してよい。
    let scm = Scripted::with_state(summary(ScmState::StopPending, 1));
    let mut engine = to_desktop_engine(&scm);
    engine.step(SwitchCommand::SwitchToDesktop).unwrap();
    scm.set(stopped_with(4));
    assert_eq!(poll(&mut engine), StepOutcome::Completed(HostKind::Desktop));
    assert_eq!(engine.state().service_fault, None);
}

// ---- relax-only ----

struct FakeClock {
    base: Instant,
    elapsed: std::cell::Cell<Duration>,
}

impl SwitchClock for FakeClock {
    fn now(&self) -> Instant {
        self.base + self.elapsed.get()
    }

    fn sleep(&self, duration: Duration) {
        self.elapsed.set(self.elapsed.get() + duration);
    }
}

fn fake_clock() -> FakeClock {
    FakeClock {
        base: Instant::now(),
        elapsed: std::cell::Cell::new(Duration::ZERO),
    }
}

const TIMINGS: SwitchTimings = SwitchTimings {
    poll: Duration::from_secs(1),
    phase: Duration::from_secs(5),
    overall: Duration::from_secs(60),
};

#[test]
fn relax_only_reports_loosened_or_left_tight_by_exit_code() {
    for (code, expected) in [
        (0, ServiceEnd::Clean),
        (5, ServiceEnd::LeftOperation),
        (4, ServiceEnd::CannotOperate),
    ] {
        let scm = Scripted::with_state(stopped_with(code));
        // 開始の後の照会は、START_PENDING を通ってから Stopped（開始の直前の Stopped を取り違え
        // ない）。
        *scm.0.queries.borrow_mut() =
            vec![summary(ScmState::StartPending, 1), stopped_with(code)].into();
        *scm.0.last.borrow_mut() = Some(summary(ScmState::StartPending, 1));
        assert_eq!(
            relax_only_and_wait(&scm, &fake_clock(), TIMINGS),
            RelaxOnlyOutcome::Done(expected),
            "{code}"
        );
        assert_eq!(*scm.0.start_args.borrow(), vec![vec!["relax-only"]]);
    }
}

#[test]
fn relax_only_is_not_sent_unless_the_service_is_stopped() {
    let scm = Scripted::with_state(summary(ScmState::Running, 0));
    scm.0
        .starts
        .borrow_mut()
        .push_back(Err(ServiceManagerError::InvalidState(ScmState::Running)));
    assert_eq!(
        relax_only_and_wait(&scm, &fake_clock(), TIMINGS),
        RelaxOnlyOutcome::NotStopped(ScmState::Running)
    );
}

#[test]
fn relax_only_waits_while_the_checkpoint_advances_and_stops_when_it_stalls() {
    // 5 秒の段を超えて 20 回進み続けても待つ。
    let scm = Scripted::default();
    let mut script: VecDeque<_> = (1..=20)
        .map(|i| summary(ScmState::StartPending, i))
        .collect();
    script.push_back(stopped_with(0));
    *scm.0.queries.borrow_mut() = script;
    *scm.0.last.borrow_mut() = Some(summary(ScmState::StartPending, 1));
    assert_eq!(
        relax_only_and_wait(&scm, &fake_clock(), TIMINGS),
        RelaxOnlyOutcome::Done(ServiceEnd::Clean)
    );
    // 止まったまま。
    let scm = Scripted::with_state(summary(ScmState::StartPending, 1));
    assert_eq!(
        relax_only_and_wait(&scm, &fake_clock(), TIMINGS),
        RelaxOnlyOutcome::Stalled
    );
}
