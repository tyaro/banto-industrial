//! 設定モード ⇔ 運用モードの切り替えで、サービスの状態を待つ共通部分（#661 661-4。
//! ChronoGazer の `mode_switch`（#623 PR-4）にあったものを、banto-hub と共有するために移した）。
//!
//! OS に依らず（SCM そのものには触れない）、[`ServiceManager`] の trait と時刻の渡し方だけに
//! 依るので、全 OS で偽の SCM と偽の時計でテストできる。
//!
//! - [`PendingWatch`]: checkpoint が進む間は期限を延ばす待ちの状態機械。サービスは運用モードへ
//!   締める間（START_PENDING）と設定モードへ緩める間（STOP_PENDING）、checkpoint を進めながら
//!   報告する。締める時間は木の大きさに比例するので「待ち時間 > 締める時間」は保証できない。
//!   代わりに、**checkpoint が進んでいる間は期限を延ばし**（期限 = 最後に進んだ時刻 +
//!   max(段の待ち時間, wait hint)）、止まったまま期限を過ぎたら [`WatchStep::Stalled`]、全体の
//!   上限（[`SwitchTimings::overall`]）を超えたら [`WatchStep::OverallTimeout`] にする。「進んで
//!   いるか」は [`pending_progress`]。
//! - [`ServiceEnd`]・[`service_exit_with`]: 止まったサービスの終了コードの読み方（4 = 運用モードに
//!   できなかった、5 = 締めたまま残った。値は製品ごとに渡す）。
//! - [`wait_for_state`]・[`relax_only_and_wait`]: 照会を繰り返して目標の状態を待つ blocking の
//!   待ちと、緩めるだけの開始（`relax-only`）。

use std::time::{Duration, Instant};

use crate::{
    pending_progress, ScmState, ServiceManager, ServiceManagerError, ServiceStatusSummary,
};

/// `dwWin32ExitCode` がサービス固有の終了コードを指す印（`ERROR_SERVICE_SPECIFIC_ERROR`）。
const ERROR_SERVICE_SPECIFIC_ERROR: u32 = 1066;

/// 1061／1052 で断られた STOP を出し直す回数の上限（その間は待つ）。
pub const MAX_STOP_RETRIES: u32 = 3;

/// 全体の上限の既定（checkpoint が進み続けても、これを超えたら失敗。30 分）。
pub const DEFAULT_OVERALL_LIMIT: Duration = Duration::from_secs(30 * 60);

/// 待ちの時間。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwitchTimings {
    /// 照会の間隔。
    pub poll: Duration,
    /// 段の待ち時間（checkpoint が止まってからこれと wait hint の大きい方を過ぎたら失敗）。
    pub phase: Duration,
    /// 全体の上限（checkpoint が進み続けても、これを超えたら失敗）。
    pub overall: Duration,
}

/// 止まったサービスの終了の理由（照会の `win32_exit_code`・`service_specific_exit_code`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceEnd {
    /// 正常に止まった（`relax-only` なら緩め終わった）。
    Clean,
    /// 運用モードにできなかった（何も変えずに断った・締めかけて緩め直した）。モードは
    /// 設定モードのまま。
    CannotOperate,
    /// 締めたまま止まった（緩められなかった・フォルダが外から変えられた）。モードは
    /// 運用モードのまま。
    LeftOperation,
    /// それ以外（OS のエラー・ほかのサービス固有の値）。
    Other { win32: u32, specific: u32 },
}

/// 照会の結果から終了の理由を読む（純関数）。`cannot_operate`・`left_tight` は製品の
/// サービス固有の終了コード（どちらも現状 4・5）。Stopped でない照会に使っても値は読むが、
/// 意味を持つのは Stopped のときだけ。
pub fn service_exit_with(
    summary: &ServiceStatusSummary,
    cannot_operate: u32,
    left_tight: u32,
) -> ServiceEnd {
    match summary.win32_exit_code {
        0 => ServiceEnd::Clean,
        ERROR_SERVICE_SPECIFIC_ERROR => match summary.service_specific_exit_code {
            0 => ServiceEnd::Clean,
            code if code == cannot_operate => ServiceEnd::CannotOperate,
            code if code == left_tight => ServiceEnd::LeftOperation,
            specific => ServiceEnd::Other {
                win32: ERROR_SERVICE_SPECIFIC_ERROR,
                specific,
            },
        },
        win32 => ServiceEnd::Other { win32, specific: 0 },
    }
}

/// 1 回の観測の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchStep {
    /// まだ待つ。
    Wait,
    /// 目標の状態になった。
    Reached(ServiceStatusSummary),
    /// 目標でも途中（pending）でもない状態になった（Running を待つ間に止まった、など）。
    Unexpected(ServiceStatusSummary),
    /// checkpoint が止まったまま段の期限を過ぎた（サービスが固まった）。
    Stalled,
    /// 全体の上限を超えた。
    OverallTimeout,
}

/// checkpoint が進む間は期限を延ばす待ち（モジュール doc）。純粋な状態機械で、時刻は呼び出し側が
/// 渡す。
#[derive(Debug, Clone)]
pub struct PendingWatch {
    target: ScmState,
    started: Instant,
    deadline: Instant,
    timings: SwitchTimings,
    previous: Option<ServiceStatusSummary>,
}

impl PendingWatch {
    /// `target`（Running か Stopped）まで待ち始める。
    pub fn new(target: ScmState, now: Instant, timings: SwitchTimings) -> Self {
        Self {
            target,
            started: now,
            deadline: now + timings.phase,
            timings,
            previous: None,
        }
    }

    /// 照会の結果を 1 つ渡す。
    pub fn observe(&mut self, now: Instant, summary: ServiceStatusSummary) -> WatchStep {
        if summary.state == self.target {
            return WatchStep::Reached(summary);
        }
        if !matches!(
            summary.state,
            ScmState::StartPending | ScmState::StopPending
        ) {
            return WatchStep::Unexpected(summary);
        }
        if pending_progress(self.previous.as_ref(), &summary) {
            self.deadline = now + self.timings.phase.max(summary.wait_hint);
        }
        self.previous = Some(summary);
        self.expired(now)
    }

    /// 照会に失敗したとき（進んでいない扱い。期限だけ確かめる）。
    pub fn observe_failure(&mut self, now: Instant) -> WatchStep {
        self.expired(now)
    }

    fn expired(&self, now: Instant) -> WatchStep {
        if now.saturating_duration_since(self.started) >= self.timings.overall {
            WatchStep::OverallTimeout
        } else if now > self.deadline {
            WatchStep::Stalled
        } else {
            WatchStep::Wait
        }
    }
}

/// 時刻と眠り（本番は [`RealClock`]、テストは眠ると進む偽の時計）。
pub trait SwitchClock {
    fn now(&self) -> Instant;
    fn sleep(&self, duration: Duration);
}

/// 本物の時計。
#[derive(Debug, Clone, Copy, Default)]
pub struct RealClock;

impl SwitchClock for RealClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// 待った結果（目標の手前で終わった理由を含む）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Waited {
    Reached(ServiceStatusSummary),
    Unexpected(ServiceStatusSummary),
    Stalled,
    TimedOut,
}

/// `target` まで照会を繰り返す。照会が通るたびに `on_state` を呼ぶ（段階を画面に出すために、
/// 呼び出し側が重複を除いて使う）。
pub fn wait_for_state(
    manager: &impl ServiceManager,
    clock: &impl SwitchClock,
    timings: SwitchTimings,
    target: ScmState,
    on_state: &mut dyn FnMut(&ScmState),
) -> Waited {
    let mut watch = PendingWatch::new(target, clock.now(), timings);
    loop {
        let step = match manager.query_status() {
            Ok(summary) => {
                on_state(&summary.state);
                watch.observe(clock.now(), summary)
            }
            Err(_) => watch.observe_failure(clock.now()),
        };
        match step {
            WatchStep::Wait => clock.sleep(timings.poll),
            WatchStep::Reached(summary) => return Waited::Reached(summary),
            WatchStep::Unexpected(summary) => return Waited::Unexpected(summary),
            WatchStep::Stalled => return Waited::Stalled,
            WatchStep::OverallTimeout => return Waited::TimedOut,
        }
    }
}

/// 緩めるだけの開始（`relax-only`）の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelaxOnlyOutcome {
    /// 止まった（終了の理由つき。`Clean` なら緩め終わった、`LeftOperation` なら緩められない）。
    Done(ServiceEnd),
    /// サービスが停止中ではなかった（要求は出していない。値は今の状態）。
    NotStopped(ScmState),
    /// サービスが登録されていない（elev の修復だけが使える）。
    NotInstalled,
    /// 開始を頼めなかった（権限が無い・その他）。
    Refused(String),
    /// 途中で別の状態になった（Running になった・未登録など）。
    Unexpected(ScmState),
    /// checkpoint が止まったまま期限を過ぎた。
    Stalled,
    /// 全体の上限を超えた。
    TimedOut,
}

/// 緩めるだけの開始: `start_with_args([relax_only_arg])`（Stopped のときだけ。Operators なら
/// UAC なし）→ **`Ok` が返った後に**照会を始め、最初の Stopped を完了と見る（開始の直前の
/// Stopped（前の終了コードを載せたまま）を取り違えない。`StartServiceW` はディスパッチャの
/// 接続まで戻らない - ChronoGazer PR-3 の実測）。返る `TransitionHandle` は目標が Running 固定
/// なので使わない。終了コードの読み方は `cannot_operate`・`left_tight`（[`service_exit_with`]）。
pub fn relax_only_and_wait(
    manager: &impl ServiceManager,
    clock: &impl SwitchClock,
    timings: SwitchTimings,
    relax_only_arg: &str,
    cannot_operate: u32,
    left_tight: u32,
) -> RelaxOnlyOutcome {
    match manager.start_with_args(&[std::ffi::OsString::from(relax_only_arg)]) {
        Ok(_) => {}
        Err(ServiceManagerError::InvalidState(state)) => {
            return RelaxOnlyOutcome::NotStopped(state);
        }
        Err(ServiceManagerError::AlreadyRunning) => {
            return RelaxOnlyOutcome::NotStopped(ScmState::Running);
        }
        Err(ServiceManagerError::NotFound(_)) => return RelaxOnlyOutcome::NotInstalled,
        Err(err) => return RelaxOnlyOutcome::Refused(err.to_string()),
    }
    match wait_for_state(manager, clock, timings, ScmState::Stopped, &mut |_| {}) {
        Waited::Reached(summary) => {
            RelaxOnlyOutcome::Done(service_exit_with(&summary, cannot_operate, left_tight))
        }
        Waited::Unexpected(summary) => RelaxOnlyOutcome::Unexpected(summary.state),
        Waited::Stalled => RelaxOnlyOutcome::Stalled,
        Waited::TimedOut => RelaxOnlyOutcome::TimedOut,
    }
}

#[cfg(test)]
mod tests;
