//! Restart-supervisor control core with injected session attempts.
//!
//! This module owns status, stop/join lifecycle, reconnect accounting, and an
//! interruptible backoff. Production FFmpeg command/input-file construction is
//! deliberately not wired yet; crate-private attempt and waiter seams keep the
//! control rules deterministic under tests without adding an async runtime.

use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};

use crate::session::{FfmpegSessionCompletion, SessionStopSignal, SessionStopToken};
use crate::{
    FfmpegDiagnostics, LatestFrameStore, ReconnectPolicy, RtspError, SupervisorError, VideoState,
    VideoStatus,
};

const SUPERVISOR_THREAD_NAME: &str = "banto-rtsp-supervisor";

type SupervisorTask = Box<dyn FnOnce() -> Result<(), RtspError> + Send + 'static>;
type SupervisorWorker = JoinHandle<Result<(), RtspError>>;

struct ControlState {
    stop_requested: bool,
    active_session: Option<SessionStopSignal>,
}

struct SupervisorShared {
    control: Mutex<ControlState>,
    wake: Condvar,
    status: Mutex<VideoStatus>,
}

/// Clonable control and status handle for one restart supervisor.
#[derive(Clone)]
pub struct VideoSupervisorHandle {
    shared: Arc<SupervisorShared>,
}

impl VideoSupervisorHandle {
    /// Requests final shutdown. Repeated requests are harmless.
    pub fn request_stop(&self) {
        let active = {
            let mut state = lock_recover(&self.shared.control);
            state.stop_requested = true;
            state.active_session.clone()
        };
        if let Some(signal) = active {
            signal.request_stop();
        }
        self.shared.wake.notify_all();
    }

    /// Returns a secret-safe status snapshot.
    pub fn status(&self) -> VideoStatus {
        lock_recover(&self.shared.status).clone()
    }
}

impl fmt::Debug for VideoSupervisorHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VideoSupervisorHandle")
            .field("status", &self.status())
            .finish()
    }
}

/// Thread-owning restart-supervisor control object.
pub struct VideoSupervisor {
    handle: VideoSupervisorHandle,
    worker: Option<JoinHandle<Result<(), RtspError>>>,
}

impl VideoSupervisor {
    pub fn handle(&self) -> VideoSupervisorHandle {
        self.handle.clone()
    }

    pub fn status(&self) -> VideoStatus {
        self.handle.status()
    }

    pub fn request_stop(&self) {
        self.handle.request_stop();
    }

    /// Requests stop and joins the owned supervisor thread.
    pub fn stop_and_join(mut self) -> Result<(), RtspError> {
        self.request_stop();
        self.join_worker()
    }

    fn join_worker(&mut self) -> Result<(), RtspError> {
        match self.worker.take() {
            Some(worker) => worker
                .join()
                .map_err(|_| RtspError::from(SupervisorError::ThreadPanicked))?,
            None => Ok(()),
        }
    }
}

impl fmt::Debug for VideoSupervisor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VideoSupervisor")
            .field("status", &self.status())
            .field("thread_owned", &self.worker.is_some())
            .finish()
    }
}

impl Drop for VideoSupervisor {
    fn drop(&mut self) {
        self.request_stop();
        let _ = self.join_worker();
    }
}

#[derive(Clone)]
#[allow(dead_code)]
pub(crate) struct AttemptReporter {
    shared: Arc<SupervisorShared>,
    first_frame_seen: Arc<Mutex<bool>>,
}

impl AttemptReporter {
    #[allow(dead_code)]
    pub(crate) fn frame(&self, received_at: SystemTime) {
        let is_first_frame = {
            let mut first_frame_seen = lock_recover(&self.first_frame_seen);
            let is_first = !*first_frame_seen;
            *first_frame_seen = true;
            is_first
        };
        update_status(&self.shared, |status| {
            status.last_frame_at = Some(received_at);
            if is_first_frame {
                status.state = VideoState::Live;
                status.consecutive_failures = 0;
                status.error = None;
            }
        });
    }

    fn saw_first_frame(&self) -> bool {
        *lock_recover(&self.first_frame_seen)
    }
}

#[allow(dead_code)]
pub(crate) trait SessionAttemptFactory: Send + 'static {
    fn run_attempt(
        &mut self,
        stop: SessionStopToken,
        reporter: AttemptReporter,
    ) -> Result<FfmpegSessionCompletion, RtspError>;
}

trait BackoffWaiter: Send + 'static {
    /// Returns true when stop won, false when the full delay elapsed.
    fn wait(&mut self, delay: Duration, shared: &Arc<SupervisorShared>) -> bool;
}

trait SupervisorThreadSpawner {
    fn spawn(
        &mut self,
        name: &'static str,
        task: SupervisorTask,
    ) -> std::io::Result<SupervisorWorker>;
}

struct StdThreadSpawner;

impl SupervisorThreadSpawner for StdThreadSpawner {
    fn spawn(
        &mut self,
        name: &'static str,
        task: SupervisorTask,
    ) -> std::io::Result<SupervisorWorker> {
        thread::Builder::new().name(name.to_owned()).spawn(task)
    }
}

struct CondvarBackoffWaiter;

impl BackoffWaiter for CondvarBackoffWaiter {
    fn wait(&mut self, delay: Duration, shared: &Arc<SupervisorShared>) -> bool {
        wait_for_stop(shared, delay)
    }
}

#[allow(dead_code)]
pub(crate) fn start_supervisor<F>(
    factory: F,
    reconnect: ReconnectPolicy,
    frames: LatestFrameStore,
    diagnostics: FfmpegDiagnostics,
) -> Result<VideoSupervisor, RtspError>
where
    F: SessionAttemptFactory,
{
    start_supervisor_with_waiter(
        factory,
        CondvarBackoffWaiter,
        reconnect,
        frames,
        diagnostics,
    )
}

fn start_supervisor_with_waiter<F, W>(
    factory: F,
    waiter: W,
    reconnect: ReconnectPolicy,
    frames: LatestFrameStore,
    diagnostics: FfmpegDiagnostics,
) -> Result<VideoSupervisor, RtspError>
where
    F: SessionAttemptFactory,
    W: BackoffWaiter,
{
    start_supervisor_with_spawner(
        factory,
        waiter,
        reconnect,
        frames,
        diagnostics,
        StdThreadSpawner,
    )
}

fn start_supervisor_with_spawner<F, W, S>(
    factory: F,
    waiter: W,
    reconnect: ReconnectPolicy,
    frames: LatestFrameStore,
    diagnostics: FfmpegDiagnostics,
    mut spawner: S,
) -> Result<VideoSupervisor, RtspError>
where
    F: SessionAttemptFactory,
    W: BackoffWaiter,
    S: SupervisorThreadSpawner,
{
    let shared = Arc::new(SupervisorShared {
        control: Mutex::new(ControlState {
            stop_requested: false,
            active_session: None,
        }),
        wake: Condvar::new(),
        status: Mutex::new(VideoStatus::new()),
    });
    let handle = VideoSupervisorHandle {
        shared: Arc::clone(&shared),
    };
    let worker_shared = Arc::clone(&shared);
    let worker_frames = frames.clone();
    let worker_diagnostics = diagnostics.clone();
    let task = Box::new(move || {
        run_owned_core(
            factory,
            waiter,
            reconnect,
            worker_shared,
            worker_frames,
            worker_diagnostics,
        )
    });
    let worker = match spawner.spawn(SUPERVISOR_THREAD_NAME, task) {
        Ok(worker) => worker,
        Err(error) => {
            set_stopped(&shared);
            let frame_close = frames.close();
            let diagnostics_close = diagnostics.close();
            let cleanup_result = resolve_cleanup(
                Err(SupervisorError::ThreadSpawn { kind: error.kind() }.into()),
                frame_close,
                diagnostics_close,
            );
            return match cleanup_result {
                Err(error) => Err(error),
                Ok(()) => unreachable!("spawn failure must remain the primary error"),
            };
        }
    };

    Ok(VideoSupervisor {
        handle,
        worker: Some(worker),
    })
}

fn run_owned_core<F, W>(
    factory: F,
    waiter: W,
    reconnect: ReconnectPolicy,
    shared: Arc<SupervisorShared>,
    frames: LatestFrameStore,
    diagnostics: FfmpegDiagnostics,
) -> Result<(), RtspError>
where
    F: SessionAttemptFactory,
    W: BackoffWaiter,
{
    let core_result = catch_unwind(AssertUnwindSafe(|| {
        run_control_core(factory, waiter, reconnect, Arc::clone(&shared))
    }))
    .unwrap_or_else(|_| Err(SupervisorError::ThreadPanicked.into()));
    set_stopped(&shared);
    let frame_close = frames.close();
    let diagnostics_close = diagnostics.close();

    resolve_cleanup(core_result, frame_close, diagnostics_close)
}

/// Core/session lifecycle errors take precedence over frame-store cleanup,
/// which takes precedence over diagnostics cleanup. All cleanup operations are
/// evaluated by the caller before this result is resolved.
fn resolve_cleanup(
    primary: Result<(), RtspError>,
    frame_close: Result<(), RtspError>,
    diagnostics_close: Result<(), RtspError>,
) -> Result<(), RtspError> {
    primary.and(frame_close).and(diagnostics_close)
}

fn run_control_core<F, W>(
    mut factory: F,
    mut waiter: W,
    reconnect: ReconnectPolicy,
    shared: Arc<SupervisorShared>,
) -> Result<(), RtspError>
where
    F: SessionAttemptFactory,
    W: BackoffWaiter,
{
    let mut attempt_index = 0u32;
    let mut backoff_attempt = 0u32;

    loop {
        if stop_requested(&shared) {
            set_stopped(&shared);
            return Ok(());
        }

        update_status(&shared, |status| {
            status.state = if attempt_index == 0 {
                VideoState::Connecting
            } else {
                VideoState::Reconnecting
            };
        });

        let (signal, token) = SessionStopSignal::pair();
        if !install_active_session(&shared, signal) {
            set_stopped(&shared);
            return Ok(());
        }
        let reporter = AttemptReporter {
            shared: Arc::clone(&shared),
            first_frame_seen: Arc::new(Mutex::new(false)),
        };
        let result = factory.run_attempt(token, reporter.clone());
        clear_active_session(&shared);

        match result {
            Ok(FfmpegSessionCompletion::Stopped { .. }) => {
                set_stopped(&shared);
                return Ok(());
            }
            Ok(FfmpegSessionCompletion::Exited(_)) => {
                if reporter.saw_first_frame() {
                    backoff_attempt = 0;
                }
                update_status(&shared, |status| {
                    status.state = VideoState::Reconnecting;
                });
            }
            Err(error) => {
                if reporter.saw_first_frame() {
                    backoff_attempt = 0;
                }
                record_failure(&shared, error);
            }
        }

        // Attempt failure/end is recorded before stop, but stop always wins
        // scheduling: no backoff and no additional factory call.
        if stop_requested(&shared) {
            set_stopped(&shared);
            return Ok(());
        }

        let delay = reconnect.delay_for_attempt(backoff_attempt);
        backoff_attempt = backoff_attempt.saturating_add(1);
        attempt_index = attempt_index.saturating_add(1);
        if waiter.wait(delay, &shared) {
            set_stopped(&shared);
            return Ok(());
        }
    }
}

fn install_active_session(shared: &Arc<SupervisorShared>, signal: SessionStopSignal) -> bool {
    let mut state = lock_recover(&shared.control);
    if state.stop_requested {
        return false;
    }
    state.active_session = Some(signal);
    true
}

fn clear_active_session(shared: &Arc<SupervisorShared>) {
    lock_recover(&shared.control).active_session = None;
}

fn stop_requested(shared: &Arc<SupervisorShared>) -> bool {
    lock_recover(&shared.control).stop_requested
}

fn wait_for_stop(shared: &Arc<SupervisorShared>, delay: Duration) -> bool {
    let state = lock_recover(&shared.control);
    if state.stop_requested {
        return true;
    }
    let (state, _) = shared
        .wake
        .wait_timeout_while(state, delay, |state| !state.stop_requested)
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state.stop_requested
}

fn record_failure(shared: &Arc<SupervisorShared>, error: RtspError) {
    update_status(shared, |status| {
        status.state = VideoState::Reconnecting;
        status.consecutive_failures = status.consecutive_failures.saturating_add(1);
        status.error = Some(error.public_info());
    });
}

fn set_stopped(shared: &Arc<SupervisorShared>) {
    update_status(shared, |status| status.state = VideoState::Stopped);
}

fn update_status(shared: &Arc<SupervisorShared>, update: impl FnOnce(&mut VideoStatus)) {
    update(&mut lock_recover(&shared.status));
}

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io;
    use std::sync::{Barrier, Mutex};

    use super::*;
    use crate::{
        DiagnosticsError, FfmpegError, FfmpegSessionOutcome, FrameStoreError, PumpSummary,
        RtspErrorCode, SessionError, SessionWorker,
    };

    fn policy() -> ReconnectPolicy {
        ReconnectPolicy::new(Duration::from_millis(10), Duration::from_millis(80), 2).unwrap()
    }

    fn shared() -> Arc<SupervisorShared> {
        Arc::new(SupervisorShared {
            control: Mutex::new(ControlState {
                stop_requested: false,
                active_session: None,
            }),
            wake: Condvar::new(),
            status: Mutex::new(VideoStatus::new()),
        })
    }

    fn empty_summary(first_frame_seen: bool) -> PumpSummary {
        PumpSummary {
            bytes_read: 0,
            frames_published: u64::from(first_frame_seen),
            first_frame_seen,
        }
    }

    fn exited(first_frame_seen: bool) -> FfmpegSessionCompletion {
        FfmpegSessionCompletion::Exited(FfmpegSessionOutcome {
            exit_code: Some(0),
            stdout: empty_summary(first_frame_seen),
            stderr: empty_summary(false),
        })
    }

    fn stopped() -> FfmpegSessionCompletion {
        FfmpegSessionCompletion::Stopped {
            stdout: empty_summary(false),
            stderr: empty_summary(false),
        }
    }

    enum Action {
        Fail(RtspError),
        FrameThenFail(SystemTime, RtspError),
        FramesThenFail(SystemTime, SystemTime, RtspError),
        FrameThenStop(SystemTime),
        Stop,
    }

    struct ScriptFactory {
        actions: VecDeque<Action>,
        calls: Arc<Mutex<usize>>,
        live_snapshots: Arc<Mutex<Vec<VideoStatus>>>,
    }

    impl SessionAttemptFactory for ScriptFactory {
        fn run_attempt(
            &mut self,
            _stop: SessionStopToken,
            reporter: AttemptReporter,
        ) -> Result<FfmpegSessionCompletion, RtspError> {
            *lock_recover(&self.calls) += 1;
            match self.actions.pop_front().expect("script exhausted") {
                Action::Fail(error) => Err(error),
                Action::FrameThenFail(at, error) => {
                    reporter.frame(at);
                    lock_recover(&self.live_snapshots).push(reporter.shared_status());
                    Err(error)
                }
                Action::FramesThenFail(first_at, second_at, error) => {
                    reporter.frame(first_at);
                    reporter.frame(second_at);
                    lock_recover(&self.live_snapshots).push(reporter.shared_status());
                    Err(error)
                }
                Action::FrameThenStop(at) => {
                    reporter.frame(at);
                    lock_recover(&self.live_snapshots).push(reporter.shared_status());
                    Ok(stopped())
                }
                Action::Stop => Ok(stopped()),
            }
        }
    }

    impl AttemptReporter {
        fn shared_status(&self) -> VideoStatus {
            lock_recover(&self.shared.status).clone()
        }
    }

    struct RecordingWaiter {
        delays: Arc<Mutex<Vec<Duration>>>,
        stop_after_waits: Option<usize>,
        waits: usize,
    }

    impl BackoffWaiter for RecordingWaiter {
        fn wait(&mut self, delay: Duration, shared: &Arc<SupervisorShared>) -> bool {
            lock_recover(&self.delays).push(delay);
            self.waits += 1;
            if self.stop_after_waits == Some(self.waits) {
                VideoSupervisorHandle {
                    shared: Arc::clone(shared),
                }
                .request_stop();
                true
            } else {
                false
            }
        }
    }

    type SharedCallCount = Arc<Mutex<usize>>;
    type SharedStatusSnapshots = Arc<Mutex<Vec<VideoStatus>>>;

    fn scripted(actions: Vec<Action>) -> (ScriptFactory, SharedCallCount, SharedStatusSnapshots) {
        let calls = Arc::new(Mutex::new(0));
        let live = Arc::new(Mutex::new(Vec::new()));
        (
            ScriptFactory {
                actions: actions.into(),
                calls: Arc::clone(&calls),
                live_snapshots: Arc::clone(&live),
            },
            calls,
            live,
        )
    }

    fn retry_error() -> RtspError {
        FfmpegError::Spawn {
            kind: io::ErrorKind::ConnectionRefused,
        }
        .into()
    }

    #[test]
    fn fail_fail_success_uses_exact_backoff_indices() {
        let (factory, calls, _) = scripted(vec![
            Action::Fail(retry_error()),
            Action::Fail(retry_error()),
            Action::FrameThenStop(SystemTime::UNIX_EPOCH),
        ]);
        let delays = Arc::new(Mutex::new(Vec::new()));
        let waiter = RecordingWaiter {
            delays: Arc::clone(&delays),
            stop_after_waits: None,
            waits: 0,
        };
        let state = shared();

        run_control_core(factory, waiter, policy(), Arc::clone(&state)).unwrap();

        assert_eq!(*lock_recover(&calls), 3);
        assert_eq!(
            *lock_recover(&delays),
            [Duration::from_millis(10), Duration::from_millis(20)]
        );
        assert_eq!(lock_recover(&state.status).state, VideoState::Stopped);
    }

    #[test]
    fn first_frame_sets_live_resets_then_later_failure_starts_at_one() {
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(7);
        let (factory, _, live) = scripted(vec![
            Action::Fail(retry_error()),
            Action::FrameThenFail(at, retry_error()),
            Action::Stop,
        ]);
        let waiter = RecordingWaiter {
            delays: Arc::new(Mutex::new(Vec::new())),
            stop_after_waits: None,
            waits: 0,
        };
        let state = shared();

        run_control_core(factory, waiter, policy(), Arc::clone(&state)).unwrap();

        let live = lock_recover(&live);
        assert_eq!(live[0].state, VideoState::Live);
        assert_eq!(live[0].consecutive_failures, 0);
        assert_eq!(live[0].error, None);
        let final_status = lock_recover(&state.status).clone();
        assert_eq!(final_status.state, VideoState::Stopped);
        assert_eq!(final_status.last_frame_at, Some(at));
        assert_eq!(final_status.consecutive_failures, 1);
        assert_eq!(final_status.error.unwrap().code, RtspErrorCode::SpawnFailed);
    }

    #[test]
    fn later_frame_updates_timestamp_without_repeating_first_frame_reset() {
        let first_at = SystemTime::UNIX_EPOCH + Duration::from_secs(7);
        let second_at = SystemTime::UNIX_EPOCH + Duration::from_secs(8);
        let (factory, _, live) = scripted(vec![
            Action::FramesThenFail(first_at, second_at, retry_error()),
            Action::Stop,
        ]);
        let waiter = RecordingWaiter {
            delays: Arc::new(Mutex::new(Vec::new())),
            stop_after_waits: None,
            waits: 0,
        };
        let state = shared();

        run_control_core(factory, waiter, policy(), Arc::clone(&state)).unwrap();

        let live = lock_recover(&live);
        assert_eq!(live[0].state, VideoState::Live);
        assert_eq!(live[0].last_frame_at, Some(second_at));
        assert_eq!(live[0].consecutive_failures, 0);
        assert_eq!(live[0].error, None);
    }

    struct ActiveStopFactory {
        barrier: Arc<Barrier>,
    }

    impl SessionAttemptFactory for ActiveStopFactory {
        fn run_attempt(
            &mut self,
            stop: SessionStopToken,
            _reporter: AttemptReporter,
        ) -> Result<FfmpegSessionCompletion, RtspError> {
            self.barrier.wait();
            self.barrier.wait();
            assert!(stop.is_stop_requested());
            Ok(stopped())
        }
    }

    struct BlockingWaiter {
        entered: Arc<Barrier>,
    }

    impl BackoffWaiter for BlockingWaiter {
        fn wait(&mut self, delay: Duration, shared: &Arc<SupervisorShared>) -> bool {
            self.entered.wait();
            wait_for_stop(shared, delay)
        }
    }

    struct PanicAfterConnectingFactory {
        entered: Arc<Barrier>,
        release: Arc<Barrier>,
    }

    impl SessionAttemptFactory for PanicAfterConnectingFactory {
        fn run_attempt(
            &mut self,
            _stop: SessionStopToken,
            _reporter: AttemptReporter,
        ) -> Result<FfmpegSessionCompletion, RtspError> {
            self.entered.wait();
            self.release.wait();
            panic!("injected supervisor factory panic");
        }
    }

    #[test]
    fn worker_panic_after_connecting_stops_and_closes_both_stores() {
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let frames = LatestFrameStore::new();
        let frame_probe = frames.clone();
        let (diagnostics, _) = FfmpegDiagnostics::new(4, 64).unwrap();
        let diagnostics_probe = diagnostics.clone();
        let supervisor = start_supervisor_with_waiter(
            PanicAfterConnectingFactory {
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
            },
            CondvarBackoffWaiter,
            policy(),
            frames,
            diagnostics,
        )
        .unwrap();
        let handle = supervisor.handle();
        entered.wait();
        assert_eq!(handle.status().state, VideoState::Connecting);
        release.wait();

        let error = supervisor.stop_and_join().unwrap_err();

        assert_eq!(
            error.public_info().code,
            RtspErrorCode::SupervisorThreadPanicked
        );
        assert_eq!(handle.status().state, VideoState::Stopped);
        assert_eq!(
            frame_probe
                .publish(Vec::new(), SystemTime::now())
                .unwrap_err()
                .public_info()
                .code,
            RtspErrorCode::FrameStoreClosed
        );
        assert_eq!(
            diagnostics_probe
                .push_sanitized("safe")
                .unwrap_err()
                .public_info()
                .code,
            RtspErrorCode::DiagnosticsClosed
        );
    }

    struct RejectingThreadSpawner;

    impl SupervisorThreadSpawner for RejectingThreadSpawner {
        fn spawn(
            &mut self,
            _name: &'static str,
            task: SupervisorTask,
        ) -> std::io::Result<SupervisorWorker> {
            drop(task);
            Err(io::ErrorKind::Other.into())
        }
    }

    #[test]
    fn spawn_failure_closes_both_stores_and_keeps_spawn_error_primary() {
        let (factory, calls, _) = scripted(vec![Action::Stop]);
        let frames = LatestFrameStore::new();
        let frame_probe = frames.clone();
        let (diagnostics, _) = FfmpegDiagnostics::new(4, 64).unwrap();
        let diagnostics_probe = diagnostics.clone();

        let error = start_supervisor_with_spawner(
            factory,
            CondvarBackoffWaiter,
            policy(),
            frames,
            diagnostics,
            RejectingThreadSpawner,
        )
        .unwrap_err();

        assert_eq!(*lock_recover(&calls), 0);
        assert_eq!(
            error.public_info().code,
            RtspErrorCode::SupervisorThreadSpawnFailed
        );
        assert_eq!(
            frame_probe
                .publish(Vec::new(), SystemTime::now())
                .unwrap_err()
                .public_info()
                .code,
            RtspErrorCode::FrameStoreClosed
        );
        assert_eq!(
            diagnostics_probe
                .push_sanitized("safe")
                .unwrap_err()
                .public_info()
                .code,
            RtspErrorCode::DiagnosticsClosed
        );

        let spawn_error: RtspError = SupervisorError::ThreadSpawn {
            kind: io::ErrorKind::Other,
        }
        .into();
        assert_eq!(
            resolve_cleanup(
                Err(spawn_error),
                Err(FrameStoreError::Poisoned.into()),
                Err(DiagnosticsError::Poisoned.into()),
            ),
            Err(spawn_error)
        );
    }

    #[test]
    fn core_error_then_frame_then_diagnostics_is_cleanup_precedence() {
        let core_error: RtspError = SupervisorError::ThreadPanicked.into();
        let frame_error: RtspError = FrameStoreError::Poisoned.into();
        let diagnostics_error: RtspError = DiagnosticsError::Poisoned.into();

        assert_eq!(
            resolve_cleanup(Err(core_error), Err(frame_error), Err(diagnostics_error),),
            Err(core_error)
        );
        assert_eq!(
            resolve_cleanup(Ok(()), Err(frame_error), Err(diagnostics_error)),
            Err(frame_error)
        );
    }

    #[test]
    fn stop_interrupts_active_attempt_and_join_is_idempotent_by_consumption() {
        let barrier = Arc::new(Barrier::new(2));
        let frames = LatestFrameStore::new();
        let frame_probe = frames.clone();
        let (diagnostics, _) = FfmpegDiagnostics::new(4, 64).unwrap();
        let diagnostics_probe = diagnostics.clone();
        let supervisor = start_supervisor_with_waiter(
            ActiveStopFactory {
                barrier: Arc::clone(&barrier),
            },
            CondvarBackoffWaiter,
            policy(),
            frames,
            diagnostics,
        )
        .unwrap();
        let handle = supervisor.handle();
        barrier.wait();
        handle.request_stop();
        handle.request_stop();
        barrier.wait();

        supervisor.stop_and_join().unwrap();

        assert_eq!(handle.status().state, VideoState::Stopped);
        assert_eq!(
            frame_probe
                .publish(Vec::new(), SystemTime::now())
                .unwrap_err()
                .public_info()
                .code,
            RtspErrorCode::FrameStoreClosed
        );
        assert_eq!(
            diagnostics_probe
                .push_sanitized("safe")
                .unwrap_err()
                .public_info()
                .code,
            RtspErrorCode::DiagnosticsClosed
        );
    }

    #[test]
    fn stop_interrupts_condvar_backoff_without_another_attempt() {
        let entered = Arc::new(Barrier::new(2));
        let (factory, calls, _) = scripted(vec![Action::Fail(retry_error())]);
        let (frames, diagnostics) = {
            let frames = LatestFrameStore::new();
            let (diagnostics, _) = FfmpegDiagnostics::new(4, 64).unwrap();
            (frames, diagnostics)
        };
        let supervisor = start_supervisor_with_waiter(
            factory,
            BlockingWaiter {
                entered: Arc::clone(&entered),
            },
            ReconnectPolicy::new(Duration::from_secs(60), Duration::from_secs(60), 1).unwrap(),
            frames,
            diagnostics,
        )
        .unwrap();
        let handle = supervisor.handle();
        entered.wait();
        handle.request_stop();

        supervisor.stop_and_join().unwrap();

        assert_eq!(*lock_recover(&calls), 1);
        assert_eq!(handle.status().state, VideoState::Stopped);
    }

    struct StopAndFailFactory {
        handle: VideoSupervisorHandle,
        calls: Arc<Mutex<usize>>,
    }

    impl SessionAttemptFactory for StopAndFailFactory {
        fn run_attempt(
            &mut self,
            _stop: SessionStopToken,
            _reporter: AttemptReporter,
        ) -> Result<FfmpegSessionCompletion, RtspError> {
            *lock_recover(&self.calls) += 1;
            self.handle.request_stop();
            Err(SessionError::ThreadPanicked {
                worker: SessionWorker::Stdout,
            }
            .into())
        }
    }

    #[test]
    fn concurrent_failure_is_recorded_but_stop_wins_scheduling() {
        let state = shared();
        let handle = VideoSupervisorHandle {
            shared: Arc::clone(&state),
        };
        let calls = Arc::new(Mutex::new(0));
        let factory = StopAndFailFactory {
            handle,
            calls: Arc::clone(&calls),
        };
        let waiter = RecordingWaiter {
            delays: Arc::new(Mutex::new(Vec::new())),
            stop_after_waits: None,
            waits: 0,
        };

        run_control_core(factory, waiter, policy(), Arc::clone(&state)).unwrap();

        assert_eq!(*lock_recover(&calls), 1);
        let status = lock_recover(&state.status).clone();
        assert_eq!(status.state, VideoState::Stopped);
        assert_eq!(status.consecutive_failures, 1);
        assert_eq!(
            status.error.unwrap().code,
            RtspErrorCode::StdoutThreadPanicked
        );
    }

    #[test]
    fn status_debug_is_secret_safe_and_counter_saturates() {
        let state = shared();
        lock_recover(&state.status).consecutive_failures = u32::MAX;
        record_failure(&state, retry_error());
        let handle = VideoSupervisorHandle { shared: state };
        let debug = format!("{handle:?}");

        assert_eq!(handle.status().consecutive_failures, u32::MAX);
        assert!(!debug.contains("camera.example"));
        assert!(!debug.contains("example-pass"));
        assert!(!debug.contains("stderr"));
    }

    #[test]
    fn immediate_failures_always_pass_through_backoff() {
        let (factory, calls, _) = scripted(vec![
            Action::Fail(retry_error()),
            Action::Fail(retry_error()),
            Action::Stop,
        ]);
        let delays = Arc::new(Mutex::new(Vec::new()));
        let waiter = RecordingWaiter {
            delays: Arc::clone(&delays),
            stop_after_waits: None,
            waits: 0,
        };

        run_control_core(factory, waiter, policy(), shared()).unwrap();

        assert_eq!(*lock_recover(&calls), 3);
        assert_eq!(lock_recover(&delays).len(), 2);
    }

    #[test]
    fn exited_attempt_is_delayed_before_restart() {
        struct ExitThenStop {
            calls: usize,
        }
        impl SessionAttemptFactory for ExitThenStop {
            fn run_attempt(
                &mut self,
                _stop: SessionStopToken,
                _reporter: AttemptReporter,
            ) -> Result<FfmpegSessionCompletion, RtspError> {
                self.calls += 1;
                Ok(if self.calls == 1 {
                    exited(false)
                } else {
                    stopped()
                })
            }
        }
        let delays = Arc::new(Mutex::new(Vec::new()));
        run_control_core(
            ExitThenStop { calls: 0 },
            RecordingWaiter {
                delays: Arc::clone(&delays),
                stop_after_waits: None,
                waits: 0,
            },
            policy(),
            shared(),
        )
        .unwrap();
        assert_eq!(*lock_recover(&delays), [Duration::from_millis(10)]);
    }
}
