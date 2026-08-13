//! One synchronous FFmpeg session with two owned reader workers.
//!
//! A session owns one direct [`FfmpegChild`] and exactly two named threads:
//! `banto-rtsp-stdout` and `banto-rtsp-stderr`. It waits/reaps the child and
//! joins both workers before returning. This module deliberately implements no
//! restart loop, backoff, async runtime, or application adapter.
//!
//! Runtime error precedence is deterministic: child wait/reap error, stdout
//! panic, stderr panic, stdout pump error, stderr pump error, frame-store close
//! error, diagnostics close error, then unsuccessful exit. Setup/thread-spawn
//! errors take precedence during setup, while cleanup and any started join are
//! still attempted.

use std::fmt;
use std::io::{self, Read};
use std::process::ExitStatus;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, TryRecvError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};

use crate::{
    pump::pump_jpeg_stream_with_first_frame, pump_stderr, FfmpegChild, FfmpegCommandSpec,
    FfmpegDiagnostics, FfmpegInputFile, FfmpegLogStreamSanitizer, JpegFrameDecoder,
    LatestFrameStore, PumpSummary, RtspError, SessionError, SessionWorker,
};

const STDOUT_WORKER_NAME: &str = "banto-rtsp-stdout";
const STDERR_WORKER_NAME: &str = "banto-rtsp-stderr";

type FirstFrameCallback = Box<dyn FnOnce(SystemTime) + Send + 'static>;

/// Crate-internal controller capability for one interruptible session.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) struct SessionStopSignal {
    requested: Arc<AtomicBool>,
}

/// Crate-internal observation capability moved into a session run.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) struct SessionStopToken {
    requested: Arc<AtomicBool>,
}

impl SessionStopSignal {
    #[allow(dead_code)]
    pub(crate) fn pair() -> (Self, SessionStopToken) {
        let requested = Arc::new(AtomicBool::new(false));
        (
            Self {
                requested: Arc::clone(&requested),
            },
            SessionStopToken { requested },
        )
    }

    #[allow(dead_code)]
    pub(crate) fn request_stop(&self) {
        self.requested.store(true, Ordering::Release);
    }
}

impl SessionStopToken {
    pub(crate) fn is_stop_requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }
}

/// Successful completion details without command, path, stderr, or frame data.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct FfmpegSessionOutcome {
    pub exit_code: Option<i32>,
    pub stdout: PumpSummary,
    pub stderr: PumpSummary,
}

/// Crate-internal completion used by the future restart supervisor.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FfmpegSessionCompletion {
    Exited(FfmpegSessionOutcome),
    Stopped {
        stdout: PumpSummary,
        stderr: PumpSummary,
    },
}

impl fmt::Debug for FfmpegSessionOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FfmpegSessionOutcome")
            .field("exit_code", &self.exit_code)
            .field("stdout", &self.stdout)
            .field("stderr", &self.stderr)
            .finish()
    }
}

/// One directly spawned FFmpeg child and its complete synchronous session.
pub struct FfmpegSession {
    child: FfmpegChild,
    input_guard: Option<FfmpegInputFile>,
    decoder: JpegFrameDecoder,
    frames: LatestFrameStore,
    diagnostics: FfmpegDiagnostics,
    sanitizer: FfmpegLogStreamSanitizer,
}

impl FfmpegSession {
    /// Spawns one direct child. The caller should obtain consumer handles from
    /// the stores before moving their producers into this session.
    pub fn spawn(
        spec: &FfmpegCommandSpec,
        input_guard: FfmpegInputFile,
        decoder: JpegFrameDecoder,
        frames: LatestFrameStore,
        diagnostics: FfmpegDiagnostics,
        sanitizer: FfmpegLogStreamSanitizer,
    ) -> Result<Self, RtspError> {
        Ok(Self {
            child: FfmpegChild::spawn(spec)?,
            input_guard: Some(input_guard),
            decoder,
            frames,
            diagnostics,
            sanitizer,
        })
    }

    /// Runs until the child exits, then joins both reader workers.
    pub fn run(self) -> Result<FfmpegSessionOutcome, RtspError> {
        self.run_with_store_policy(StoreClosePolicy::Close)
    }

    /// Crate-internal path for the restart supervisor that owns stable
    /// stores and consumer handles across multiple one-shot child sessions.
    #[allow(dead_code)]
    pub(crate) fn run_preserving_stores(self) -> Result<FfmpegSessionOutcome, RtspError> {
        self.run_with_store_policy(StoreClosePolicy::Preserve)
    }

    /// Crate-internal interruptible path for the restart supervisor.
    #[allow(dead_code)]
    pub(crate) fn run_preserving_stores_until(
        self,
        stop: SessionStopToken,
    ) -> Result<FfmpegSessionCompletion, RtspError> {
        self.run_with_control(StoreClosePolicy::Preserve, Some(stop))
    }

    pub(crate) fn run_preserving_stores_until_with_first_frame<F>(
        self,
        stop: SessionStopToken,
        first_frame: F,
    ) -> Result<FfmpegSessionCompletion, RtspError>
    where
        F: FnOnce(SystemTime) + Send + 'static,
    {
        self.run_with_control_and_first_frame(
            StoreClosePolicy::Preserve,
            Some(stop),
            Some(Box::new(first_frame)),
        )
    }

    fn run_with_store_policy(
        self,
        store_policy: StoreClosePolicy,
    ) -> Result<FfmpegSessionOutcome, RtspError> {
        match self.run_with_control(store_policy, None)? {
            FfmpegSessionCompletion::Exited(outcome) => Ok(outcome),
            FfmpegSessionCompletion::Stopped { .. } => {
                unreachable!("a session without a stop token cannot report stopped")
            }
        }
    }

    fn run_with_control(
        self,
        store_policy: StoreClosePolicy,
        stop: Option<SessionStopToken>,
    ) -> Result<FfmpegSessionCompletion, RtspError> {
        self.run_with_control_and_first_frame(store_policy, stop, None)
    }

    fn run_with_control_and_first_frame(
        self,
        store_policy: StoreClosePolicy,
        stop: Option<SessionStopToken>,
        first_frame: Option<FirstFrameCallback>,
    ) -> Result<FfmpegSessionCompletion, RtspError> {
        let Self {
            mut child,
            input_guard,
            decoder,
            frames,
            diagnostics,
            sanitizer,
        } = self;

        let stdout = match child.take_stdout() {
            Ok(stdout) => stdout,
            Err(error) => {
                let _ = child.terminate();
                let _ = apply_store_policy(store_policy, &frames, &diagnostics);
                return Err(error);
            }
        };
        let stderr = match child.take_stderr() {
            Ok(stderr) => stderr,
            Err(error) => {
                let _ = child.terminate();
                let _ = apply_store_policy(store_policy, &frames, &diagnostics);
                return Err(error);
            }
        };

        run_session_parts_with_control_and_first_frame(
            &mut child,
            stdout,
            stderr,
            decoder,
            frames,
            diagnostics,
            sanitizer,
            input_guard,
            &SystemThreadSpawner,
            store_policy,
            stop.as_ref(),
            first_frame,
        )
    }
}

impl fmt::Debug for FfmpegSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FfmpegSession")
            .field("child_owned", &true)
            .field("input_guard_present", &self.input_guard.is_some())
            .field("stdout_worker_name", &STDOUT_WORKER_NAME)
            .field("stderr_worker_name", &STDERR_WORKER_NAME)
            .finish()
    }
}

#[derive(Clone, Copy)]
struct SessionExit {
    success: bool,
    code: Option<i32>,
}

impl From<ExitStatus> for SessionExit {
    fn from(status: ExitStatus) -> Self {
        Self {
            success: status.success(),
            code: status.code(),
        }
    }
}

trait ChildLifecycle {
    fn try_wait_and_reap(&mut self) -> Result<Option<SessionExit>, RtspError>;
    fn terminate_and_reap(&mut self) -> Result<SessionExit, RtspError>;
}

impl ChildLifecycle for FfmpegChild {
    fn try_wait_and_reap(&mut self) -> Result<Option<SessionExit>, RtspError> {
        self.try_wait().map(|status| status.map(SessionExit::from))
    }

    fn terminate_and_reap(&mut self) -> Result<SessionExit, RtspError> {
        self.terminate().map(SessionExit::from)
    }
}

trait WorkerSpawner {
    fn spawn<T, F>(&self, name: &'static str, work: F) -> io::Result<JoinHandle<T>>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static;
}

struct SystemThreadSpawner;

#[derive(Clone, Copy, PartialEq, Eq)]
enum StoreClosePolicy {
    Close,
    Preserve,
}

impl WorkerSpawner for SystemThreadSpawner {
    fn spawn<T, F>(&self, name: &'static str, work: F) -> io::Result<JoinHandle<T>>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        thread::Builder::new().name(name.to_owned()).spawn(work)
    }
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn run_session_parts<C, O, E, S>(
    child: &mut C,
    stdout: O,
    stderr: E,
    decoder: JpegFrameDecoder,
    frames: LatestFrameStore,
    diagnostics: FfmpegDiagnostics,
    sanitizer: FfmpegLogStreamSanitizer,
    input_guard: Option<FfmpegInputFile>,
    spawner: &S,
) -> Result<FfmpegSessionOutcome, RtspError>
where
    C: ChildLifecycle,
    O: Read + Send + 'static,
    E: Read + Send + 'static,
    S: WorkerSpawner,
{
    run_session_parts_with_policy(
        child,
        stdout,
        stderr,
        decoder,
        frames,
        diagnostics,
        sanitizer,
        input_guard,
        spawner,
        StoreClosePolicy::Close,
    )
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn run_session_parts_with_policy<C, O, E, S>(
    child: &mut C,
    stdout: O,
    stderr: E,
    decoder: JpegFrameDecoder,
    frames: LatestFrameStore,
    diagnostics: FfmpegDiagnostics,
    sanitizer: FfmpegLogStreamSanitizer,
    input_guard: Option<FfmpegInputFile>,
    spawner: &S,
    store_policy: StoreClosePolicy,
) -> Result<FfmpegSessionOutcome, RtspError>
where
    C: ChildLifecycle,
    O: Read + Send + 'static,
    E: Read + Send + 'static,
    S: WorkerSpawner,
{
    match run_session_parts_with_control(
        child,
        stdout,
        stderr,
        decoder,
        frames,
        diagnostics,
        sanitizer,
        input_guard,
        spawner,
        store_policy,
        None,
    )? {
        FfmpegSessionCompletion::Exited(outcome) => Ok(outcome),
        FfmpegSessionCompletion::Stopped { .. } => {
            unreachable!("session parts without a stop token cannot report stopped")
        }
    }
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn run_session_parts_with_control<C, O, E, S>(
    child: &mut C,
    stdout: O,
    stderr: E,
    decoder: JpegFrameDecoder,
    frames: LatestFrameStore,
    diagnostics: FfmpegDiagnostics,
    sanitizer: FfmpegLogStreamSanitizer,
    input_guard: Option<FfmpegInputFile>,
    spawner: &S,
    store_policy: StoreClosePolicy,
    stop: Option<&SessionStopToken>,
) -> Result<FfmpegSessionCompletion, RtspError>
where
    C: ChildLifecycle,
    O: Read + Send + 'static,
    E: Read + Send + 'static,
    S: WorkerSpawner,
{
    run_session_parts_with_control_and_first_frame(
        child,
        stdout,
        stderr,
        decoder,
        frames,
        diagnostics,
        sanitizer,
        input_guard,
        spawner,
        store_policy,
        stop,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_session_parts_with_control_and_first_frame<C, O, E, S>(
    child: &mut C,
    stdout: O,
    stderr: E,
    decoder: JpegFrameDecoder,
    frames: LatestFrameStore,
    diagnostics: FfmpegDiagnostics,
    sanitizer: FfmpegLogStreamSanitizer,
    input_guard: Option<FfmpegInputFile>,
    spawner: &S,
    store_policy: StoreClosePolicy,
    stop: Option<&SessionStopToken>,
    first_frame: Option<FirstFrameCallback>,
) -> Result<FfmpegSessionCompletion, RtspError>
where
    C: ChildLifecycle,
    O: Read + Send + 'static,
    E: Read + Send + 'static,
    S: WorkerSpawner,
{
    let (completion_sender, completion_receiver) = mpsc::channel();
    let stdout_frames = frames.clone();
    let stdout_sender = completion_sender.clone();
    let stdout_worker = match spawner.spawn(STDOUT_WORKER_NAME, move || {
        let mut decoder = decoder;
        run_worker(stdout_sender, move || {
            pump_jpeg_stream_with_first_frame(
                stdout,
                &mut decoder,
                &stdout_frames,
                input_guard,
                first_frame,
            )
        })
    }) {
        Ok(worker) => worker,
        Err(error) => {
            let _ = child.terminate_and_reap();
            let _ = apply_store_policy(store_policy, &frames, &diagnostics);
            return Err(SessionError::ThreadSpawn {
                worker: SessionWorker::Stdout,
                kind: error.kind(),
            }
            .into());
        }
    };

    let stderr_diagnostics = diagnostics.clone();
    let stderr_sender = completion_sender.clone();
    let stderr_worker = match spawner.spawn(STDERR_WORKER_NAME, move || {
        run_worker(stderr_sender, move || {
            pump_stderr(stderr, sanitizer, &stderr_diagnostics)
        })
    }) {
        Ok(worker) => worker,
        Err(error) => {
            let _ = child.terminate_and_reap();
            let _ = stdout_worker.join();
            let _ = apply_store_policy(store_policy, &frames, &diagnostics);
            return Err(SessionError::ThreadSpawn {
                worker: SessionWorker::Stderr,
                kind: error.kind(),
            }
            .into());
        }
    };
    drop(completion_sender);

    let wait_result = monitor_child_and_workers(child, &completion_receiver, stop);
    if wait_result.is_err() {
        let _ = child.terminate_and_reap();
    }

    let stdout_result = stdout_worker.join();
    let stderr_result = stderr_worker.join();
    let (frames_close, diagnostics_close) = apply_store_policy(store_policy, &frames, &diagnostics);

    let monitor_completion = wait_result?;
    let stdout_summary = join_worker(stdout_result, SessionWorker::Stdout)?;
    let stderr_summary = join_worker(stderr_result, SessionWorker::Stderr)?;
    frames_close?;
    diagnostics_close?;
    match monitor_completion {
        MonitorCompletion::Exited(exit) => {
            if !exit.success {
                return Err(SessionError::UnsuccessfulExit { code: exit.code }.into());
            }
            Ok(FfmpegSessionCompletion::Exited(FfmpegSessionOutcome {
                exit_code: exit.code,
                stdout: stdout_summary,
                stderr: stderr_summary,
            }))
        }
        MonitorCompletion::Stopped => Ok(FfmpegSessionCompletion::Stopped {
            stdout: stdout_summary,
            stderr: stderr_summary,
        }),
    }
}

fn apply_store_policy(
    policy: StoreClosePolicy,
    frames: &LatestFrameStore,
    diagnostics: &FfmpegDiagnostics,
) -> (Result<(), RtspError>, Result<(), RtspError>) {
    match policy {
        StoreClosePolicy::Close => (frames.close(), diagnostics.close()),
        StoreClosePolicy::Preserve => (Ok(()), Ok(())),
    }
}

fn join_worker(
    result: thread::Result<WorkerCompletion>,
    worker: SessionWorker,
) -> Result<PumpSummary, RtspError> {
    match result {
        Ok(WorkerCompletion::Finished(result)) => result,
        Ok(WorkerCompletion::Panicked) | Err(_) => {
            Err(SessionError::ThreadPanicked { worker }.into())
        }
    }
}

enum WorkerCompletion {
    Finished(Result<PumpSummary, RtspError>),
    Panicked,
}

#[derive(Clone, Copy)]
struct WorkerSignal {
    failed: bool,
}

fn run_worker<F>(sender: mpsc::Sender<WorkerSignal>, work: F) -> WorkerCompletion
where
    F: FnOnce() -> Result<PumpSummary, RtspError>,
{
    let completion = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)) {
        Ok(result) => WorkerCompletion::Finished(result),
        Err(_) => WorkerCompletion::Panicked,
    };
    let failed = !matches!(completion, WorkerCompletion::Finished(Ok(_)));
    let _ = sender.send(WorkerSignal { failed });
    completion
}

enum MonitorCompletion {
    Exited(SessionExit),
    Stopped,
}

fn monitor_child_and_workers<C: ChildLifecycle>(
    child: &mut C,
    receiver: &mpsc::Receiver<WorkerSignal>,
    stop: Option<&SessionStopToken>,
) -> Result<MonitorCompletion, RtspError> {
    let mut completed_workers = 0;
    let pre_requested = stop.is_some_and(SessionStopToken::is_stop_requested);
    loop {
        if let Some(exit) = child.try_wait_and_reap()? {
            return Ok(MonitorCompletion::Exited(exit));
        }

        if pre_requested {
            child.terminate_and_reap()?;
            return Ok(MonitorCompletion::Stopped);
        }

        loop {
            match receiver.try_recv() {
                Ok(signal) => {
                    completed_workers += 1;
                    if signal.failed {
                        return child.terminate_and_reap().map(MonitorCompletion::Exited);
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) if completed_workers == 2 => break,
                Err(TryRecvError::Disconnected) => {
                    return child.terminate_and_reap().map(MonitorCompletion::Exited);
                }
            }
        }

        if stop.is_some_and(SessionStopToken::is_stop_requested) {
            child.terminate_and_reap()?;
            return Ok(MonitorCompletion::Stopped);
        }

        if completed_workers == 2 {
            thread::sleep(Duration::from_millis(25));
            continue;
        }

        match receiver.recv_timeout(Duration::from_millis(25)) {
            Ok(signal) => {
                completed_workers += 1;
                if signal.failed {
                    return child.terminate_and_reap().map(MonitorCompletion::Exited);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) if completed_workers == 2 => {}
            Err(RecvTimeoutError::Disconnected) => {
                return child.terminate_and_reap().map(MonitorCompletion::Exited);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier, Condvar, Mutex};

    use super::*;
    use crate::{
        FfmpegError, FfmpegLogSanitizer, RtspCredentials, RtspEndpoint, RtspErrorCode,
        RtspTransport,
    };

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn endpoint() -> RtspEndpoint {
        RtspEndpoint::new("rtsp://camera.example/live").unwrap()
    }

    fn test_path(label: &str) -> PathBuf {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "banto-rtsp-session-{label}-{}-{id}",
            std::process::id()
        ))
    }

    fn input_guard(label: &str) -> (PathBuf, FfmpegInputFile) {
        let path = test_path(label);
        let guard = FfmpegInputFile::create_new(
            &path,
            &endpoint(),
            None,
            RtspTransport::Tcp,
            std::time::Duration::from_secs(5),
        )
        .unwrap();
        (path, guard)
    }

    fn assert_stores_closed(frames: &LatestFrameStore, diagnostics: &FfmpegDiagnostics) {
        let frame_error = frames
            .publish(Vec::new(), std::time::SystemTime::now())
            .unwrap_err();
        let diagnostics_error = diagnostics.push_sanitized("safe text").unwrap_err();
        assert_eq!(
            frame_error.public_info().code,
            RtspErrorCode::FrameStoreClosed
        );
        assert_eq!(
            diagnostics_error.public_info().code,
            RtspErrorCode::DiagnosticsClosed
        );
    }

    fn assert_stores_usable(frames: &LatestFrameStore, diagnostics: &FfmpegDiagnostics) {
        frames
            .publish(Vec::new(), std::time::SystemTime::now())
            .unwrap();
        diagnostics.push_sanitized("safe text").unwrap();
    }

    struct FakeChild {
        try_wait_result: Result<Option<SessionExit>, RtspError>,
        terminate_result: Result<SessionExit, RtspError>,
        waited: Arc<AtomicBool>,
        terminated: Arc<AtomicBool>,
    }

    impl FakeChild {
        fn success() -> (Self, Arc<AtomicBool>, Arc<AtomicBool>) {
            Self::with_wait(Ok(SessionExit {
                success: true,
                code: Some(0),
            }))
        }

        fn running() -> (Self, Arc<AtomicBool>, Arc<AtomicBool>) {
            let waited = Arc::new(AtomicBool::new(false));
            let terminated = Arc::new(AtomicBool::new(false));
            (
                Self {
                    try_wait_result: Ok(None),
                    terminate_result: Ok(SessionExit {
                        success: false,
                        code: None,
                    }),
                    waited: Arc::clone(&waited),
                    terminated: Arc::clone(&terminated),
                },
                waited,
                terminated,
            )
        }

        fn with_wait(
            wait_result: Result<SessionExit, RtspError>,
        ) -> (Self, Arc<AtomicBool>, Arc<AtomicBool>) {
            let waited = Arc::new(AtomicBool::new(false));
            let terminated = Arc::new(AtomicBool::new(false));
            (
                Self {
                    try_wait_result: wait_result.map(Some),
                    terminate_result: Ok(SessionExit {
                        success: false,
                        code: None,
                    }),
                    waited: Arc::clone(&waited),
                    terminated: Arc::clone(&terminated),
                },
                waited,
                terminated,
            )
        }

        fn with_terminate_error(error: RtspError) -> (Self, Arc<AtomicBool>, Arc<AtomicBool>) {
            let (mut child, waited, terminated) = Self::running();
            child.terminate_result = Err(error);
            (child, waited, terminated)
        }
    }

    impl ChildLifecycle for FakeChild {
        fn try_wait_and_reap(&mut self) -> Result<Option<SessionExit>, RtspError> {
            if !matches!(self.try_wait_result, Ok(None)) {
                self.waited.store(true, Ordering::SeqCst);
            }
            self.try_wait_result
        }

        fn terminate_and_reap(&mut self) -> Result<SessionExit, RtspError> {
            self.waited.store(true, Ordering::SeqCst);
            self.terminated.store(true, Ordering::SeqCst);
            self.terminate_result
        }
    }

    struct NamedReader<R> {
        inner: R,
        expected_name: &'static str,
        observed: Arc<AtomicBool>,
    }

    impl<R: Read> Read for NamedReader<R> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            assert_eq!(thread::current().name(), Some(self.expected_name));
            self.observed.store(true, Ordering::SeqCst);
            self.inner.read(buffer)
        }
    }

    struct FailingReader;

    impl Read for FailingReader {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "test failure"))
        }
    }

    struct PanicReader;

    impl Read for PanicReader {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            panic!("injected reader panic")
        }
    }

    struct DropTrackedReader<R> {
        inner: R,
        dropped: Arc<AtomicBool>,
    }

    impl<R: Read> Read for DropTrackedReader<R> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.inner.read(buffer)
        }
    }

    impl<R> Drop for DropTrackedReader<R> {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }

    struct GatedReader {
        started: Arc<Barrier>,
        gate: Arc<(Mutex<bool>, Condvar)>,
        dropped: Arc<AtomicBool>,
    }

    impl Read for GatedReader {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            self.started.wait();
            let (open, changed) = &*self.gate;
            let guard = open.lock().unwrap();
            let _guard = changed.wait_while(guard, |open| !*open).unwrap();
            Ok(0)
        }
    }

    impl Drop for GatedReader {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }

    struct StopAndFailReader {
        signal: SessionStopSignal,
    }

    impl Read for StopAndFailReader {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            self.signal.request_stop();
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "test failure"))
        }
    }

    #[test]
    fn pre_requested_stop_is_normal_joins_workers_and_preserves_stores() {
        let (signal, token) = SessionStopSignal::pair();
        signal.request_stop();
        let (path, guard) = input_guard("pre-stop");
        let (mut child, waited, terminated) = FakeChild::running();
        let frames = LatestFrameStore::new();
        let frames_probe = frames.clone();
        let (diagnostics, _) = FfmpegDiagnostics::new(4, 64).unwrap();
        let diagnostics_probe = diagnostics.clone();
        let stdout_dropped = Arc::new(AtomicBool::new(false));
        let stderr_dropped = Arc::new(AtomicBool::new(false));

        let completion = run_session_parts_with_control(
            &mut child,
            DropTrackedReader {
                inner: Cursor::new(Vec::<u8>::new()),
                dropped: Arc::clone(&stdout_dropped),
            },
            DropTrackedReader {
                inner: Cursor::new(Vec::<u8>::new()),
                dropped: Arc::clone(&stderr_dropped),
            },
            JpegFrameDecoder::new(64).unwrap(),
            frames,
            diagnostics,
            FfmpegLogSanitizer::new(&endpoint(), None).stream(),
            Some(guard),
            &SystemThreadSpawner,
            StoreClosePolicy::Preserve,
            Some(&token),
        )
        .unwrap();

        assert!(matches!(
            completion,
            FfmpegSessionCompletion::Stopped { .. }
        ));
        assert!(waited.load(Ordering::SeqCst));
        assert!(terminated.load(Ordering::SeqCst));
        assert!(stdout_dropped.load(Ordering::SeqCst));
        assert!(stderr_dropped.load(Ordering::SeqCst));
        assert!(!path.exists());
        assert_stores_usable(&frames_probe, &diagnostics_probe);
    }

    #[test]
    fn in_flight_stop_is_observed_without_sleep_based_coordination() {
        let (signal, token) = SessionStopSignal::pair();
        let (mut child, _, terminated) = FakeChild::running();
        let frames = LatestFrameStore::new();
        let frames_probe = frames.clone();
        let (diagnostics, _) = FfmpegDiagnostics::new(4, 64).unwrap();
        let diagnostics_probe = diagnostics.clone();
        let started = Arc::new(Barrier::new(3));
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let stdout_dropped = Arc::new(AtomicBool::new(false));
        let stderr_dropped = Arc::new(AtomicBool::new(false));
        let requester_started = Arc::clone(&started);
        let requester_gate = Arc::clone(&gate);
        let requester = thread::spawn(move || {
            requester_started.wait();
            signal.request_stop();
            let (open, changed) = &*requester_gate;
            *open.lock().unwrap() = true;
            changed.notify_all();
        });

        let completion = run_session_parts_with_control(
            &mut child,
            GatedReader {
                started: Arc::clone(&started),
                gate: Arc::clone(&gate),
                dropped: Arc::clone(&stdout_dropped),
            },
            GatedReader {
                started,
                gate,
                dropped: Arc::clone(&stderr_dropped),
            },
            JpegFrameDecoder::new(64).unwrap(),
            frames,
            diagnostics,
            FfmpegLogSanitizer::new(&endpoint(), None).stream(),
            None,
            &SystemThreadSpawner,
            StoreClosePolicy::Preserve,
            Some(&token),
        )
        .unwrap();
        requester.join().unwrap();

        assert!(matches!(
            completion,
            FfmpegSessionCompletion::Stopped { .. }
        ));
        assert!(terminated.load(Ordering::SeqCst));
        assert!(stdout_dropped.load(Ordering::SeqCst));
        assert!(stderr_dropped.load(Ordering::SeqCst));
        assert_stores_usable(&frames_probe, &diagnostics_probe);
    }

    #[test]
    fn termination_failure_wins_over_requested_stop() {
        let (signal, token) = SessionStopSignal::pair();
        signal.request_stop();
        let terminate_error: RtspError = FfmpegError::TerminateKill {
            kind: io::ErrorKind::PermissionDenied,
        }
        .into();
        let (mut child, _, terminated) = FakeChild::with_terminate_error(terminate_error);
        let frames = LatestFrameStore::new();
        let frames_probe = frames.clone();
        let (diagnostics, _) = FfmpegDiagnostics::new(4, 64).unwrap();
        let diagnostics_probe = diagnostics.clone();

        let error = run_session_parts_with_control(
            &mut child,
            Cursor::new(Vec::<u8>::new()),
            Cursor::new(Vec::<u8>::new()),
            JpegFrameDecoder::new(64).unwrap(),
            frames,
            diagnostics,
            FfmpegLogSanitizer::new(&endpoint(), None).stream(),
            None,
            &SystemThreadSpawner,
            StoreClosePolicy::Preserve,
            Some(&token),
        )
        .unwrap_err();

        assert!(terminated.load(Ordering::SeqCst));
        assert_eq!(error.public_info().code, RtspErrorCode::TerminateKillFailed);
        assert_stores_usable(&frames_probe, &diagnostics_probe);
    }

    #[test]
    fn try_wait_failure_wins_over_pre_requested_stop() {
        let (signal, token) = SessionStopSignal::pair();
        signal.request_stop();
        let wait_error: RtspError = FfmpegError::TryWait {
            kind: io::ErrorKind::Other,
        }
        .into();
        let (mut child, waited, terminated) = FakeChild::with_wait(Err(wait_error));
        let frames = LatestFrameStore::new();
        let frames_probe = frames.clone();
        let (diagnostics, _) = FfmpegDiagnostics::new(4, 64).unwrap();
        let diagnostics_probe = diagnostics.clone();

        let error = run_session_parts_with_control(
            &mut child,
            Cursor::new(Vec::<u8>::new()),
            Cursor::new(Vec::<u8>::new()),
            JpegFrameDecoder::new(64).unwrap(),
            frames,
            diagnostics,
            FfmpegLogSanitizer::new(&endpoint(), None).stream(),
            None,
            &SystemThreadSpawner,
            StoreClosePolicy::Preserve,
            Some(&token),
        )
        .unwrap_err();

        assert!(waited.load(Ordering::SeqCst));
        assert!(terminated.load(Ordering::SeqCst));
        assert_eq!(error.public_info().code, RtspErrorCode::TryWaitFailed);
        assert_stores_usable(&frames_probe, &diagnostics_probe);
    }

    #[test]
    fn pump_failure_is_not_relabelled_when_stop_is_requested_concurrently() {
        let (signal, token) = SessionStopSignal::pair();
        let (mut child, _, terminated) = FakeChild::running();
        let frames = LatestFrameStore::new();
        let frames_probe = frames.clone();
        let (diagnostics, _) = FfmpegDiagnostics::new(4, 64).unwrap();
        let diagnostics_probe = diagnostics.clone();

        let error = run_session_parts_with_control(
            &mut child,
            StopAndFailReader { signal },
            Cursor::new(Vec::<u8>::new()),
            JpegFrameDecoder::new(64).unwrap(),
            frames,
            diagnostics,
            FfmpegLogSanitizer::new(&endpoint(), None).stream(),
            None,
            &SystemThreadSpawner,
            StoreClosePolicy::Preserve,
            Some(&token),
        )
        .unwrap_err();

        assert!(terminated.load(Ordering::SeqCst));
        assert_eq!(error.public_info().code, RtspErrorCode::StdoutReadFailed);
        assert_stores_usable(&frames_probe, &diagnostics_probe);
    }

    #[test]
    fn successful_session_runs_named_dual_pumps_and_cleans_first_frame_input() {
        let (path, guard) = input_guard("success");
        let (mut child, waited, _) = FakeChild::success();
        let frames = LatestFrameStore::new();
        let frames_probe = frames.clone();
        let frame_handle = frames.handle();
        let (diagnostics, diagnostics_handle) = FfmpegDiagnostics::new(8, 128).unwrap();
        let diagnostics_probe = diagnostics.clone();
        let credentials = RtspCredentials::new("viewer", "example-pass");
        let sanitizer = FfmpegLogSanitizer::new(&endpoint(), Some(&credentials)).stream();
        let stdout_named = Arc::new(AtomicBool::new(false));
        let stderr_named = Arc::new(AtomicBool::new(false));
        let stdout = NamedReader {
            inner: Cursor::new([0xff, 0xd8, 7, 0xff, 0xd9]),
            expected_name: STDOUT_WORKER_NAME,
            observed: Arc::clone(&stdout_named),
        };
        let stderr = NamedReader {
            inner: Cursor::new(b"rtsp://viewer:example-pass@camera.example/live"),
            expected_name: STDERR_WORKER_NAME,
            observed: Arc::clone(&stderr_named),
        };

        let outcome = run_session_parts(
            &mut child,
            stdout,
            stderr,
            JpegFrameDecoder::new(64).unwrap(),
            frames,
            diagnostics,
            sanitizer,
            Some(guard),
            &SystemThreadSpawner,
        )
        .unwrap();

        assert_eq!(outcome.stdout.frames_published, 1);
        assert!(waited.load(Ordering::SeqCst));
        assert!(stdout_named.load(Ordering::SeqCst));
        assert!(stderr_named.load(Ordering::SeqCst));
        assert!(!path.exists());
        assert_eq!(
            frame_handle.snapshot().unwrap().unwrap().jpeg,
            [0xff, 0xd8, 7, 0xff, 0xd9]
        );
        let diagnostics_text = diagnostics_handle.snapshot().unwrap().join("");
        assert!(diagnostics_text.contains("[REDACTED]"));
        assert!(!diagnostics_text.contains("camera.example"));
        assert!(!diagnostics_text.contains("example-pass"));
        assert_stores_closed(&frames_probe, &diagnostics_probe);
    }

    #[test]
    fn session_first_frame_callback_follows_cleanup_with_published_timestamp() {
        let (path, guard) = input_guard("first-frame-callback");
        let callback_path = path.clone();
        let (mut child, _, _) = FakeChild::success();
        let frames = LatestFrameStore::new();
        let frame_handle = frames.handle();
        let (diagnostics, _) = FfmpegDiagnostics::new(4, 64).unwrap();
        let reported_at = Arc::new(Mutex::new(None));
        let callback_at = Arc::clone(&reported_at);

        run_session_parts_with_control_and_first_frame(
            &mut child,
            Cursor::new([0xff, 0xd8, 9, 0xff, 0xd9]),
            Cursor::new(Vec::<u8>::new()),
            JpegFrameDecoder::new(64).unwrap(),
            frames,
            diagnostics,
            FfmpegLogSanitizer::new(&endpoint(), None).stream(),
            Some(guard),
            &SystemThreadSpawner,
            StoreClosePolicy::Preserve,
            None,
            Some(Box::new(move |received_at| {
                assert!(!callback_path.exists());
                *callback_at.lock().unwrap() = Some(received_at);
            })),
        )
        .unwrap();

        let frame = frame_handle.snapshot().unwrap().unwrap();
        assert_eq!(*reported_at.lock().unwrap(), Some(frame.received_at));
        assert!(!path.exists());
    }

    #[test]
    fn preserve_mode_keeps_stores_usable_after_success() {
        let (mut child, _, _) = FakeChild::success();
        let frames = LatestFrameStore::new();
        let frames_probe = frames.clone();
        let (diagnostics, _) = FfmpegDiagnostics::new(4, 64).unwrap();
        let diagnostics_probe = diagnostics.clone();

        run_session_parts_with_policy(
            &mut child,
            Cursor::new(Vec::<u8>::new()),
            Cursor::new(Vec::<u8>::new()),
            JpegFrameDecoder::new(64).unwrap(),
            frames,
            diagnostics,
            FfmpegLogSanitizer::new(&endpoint(), None).stream(),
            None,
            &SystemThreadSpawner,
            StoreClosePolicy::Preserve,
        )
        .unwrap();

        assert_stores_usable(&frames_probe, &diagnostics_probe);
    }

    #[test]
    fn preserve_mode_keeps_stores_usable_after_pump_error() {
        let (mut child, _, terminated) = FakeChild::running();
        let frames = LatestFrameStore::new();
        let frames_probe = frames.clone();
        let (diagnostics, _) = FfmpegDiagnostics::new(4, 64).unwrap();
        let diagnostics_probe = diagnostics.clone();

        let error = run_session_parts_with_policy(
            &mut child,
            FailingReader,
            Cursor::new(Vec::<u8>::new()),
            JpegFrameDecoder::new(64).unwrap(),
            frames,
            diagnostics,
            FfmpegLogSanitizer::new(&endpoint(), None).stream(),
            None,
            &SystemThreadSpawner,
            StoreClosePolicy::Preserve,
        )
        .unwrap_err();

        assert!(terminated.load(Ordering::SeqCst));
        assert_eq!(error.public_info().code, RtspErrorCode::StdoutReadFailed);
        assert_stores_usable(&frames_probe, &diagnostics_probe);
    }

    #[test]
    fn preserve_mode_keeps_stores_usable_after_worker_panic() {
        let (mut child, _, terminated) = FakeChild::running();
        let frames = LatestFrameStore::new();
        let frames_probe = frames.clone();
        let (diagnostics, _) = FfmpegDiagnostics::new(4, 64).unwrap();
        let diagnostics_probe = diagnostics.clone();

        let error = run_session_parts_with_policy(
            &mut child,
            Cursor::new(Vec::<u8>::new()),
            PanicReader,
            JpegFrameDecoder::new(64).unwrap(),
            frames,
            diagnostics,
            FfmpegLogSanitizer::new(&endpoint(), None).stream(),
            None,
            &SystemThreadSpawner,
            StoreClosePolicy::Preserve,
        )
        .unwrap_err();

        assert!(terminated.load(Ordering::SeqCst));
        assert_eq!(
            error.public_info().code,
            RtspErrorCode::StderrThreadPanicked
        );
        assert_stores_usable(&frames_probe, &diagnostics_probe);
    }

    #[test]
    fn unsuccessful_exit_is_structured_after_workers_join() {
        let (mut child, waited, _) = FakeChild::with_wait(Ok(SessionExit {
            success: false,
            code: Some(7),
        }));
        let frames = LatestFrameStore::new();
        let (diagnostics, _) = FfmpegDiagnostics::new(2, 64).unwrap();

        let error = run_session_parts(
            &mut child,
            Cursor::new(Vec::<u8>::new()),
            Cursor::new(Vec::<u8>::new()),
            JpegFrameDecoder::new(64).unwrap(),
            frames,
            diagnostics,
            FfmpegLogSanitizer::new(&endpoint(), None).stream(),
            None,
            &SystemThreadSpawner,
        )
        .unwrap_err();

        assert!(waited.load(Ordering::SeqCst));
        assert_eq!(
            error.public_info().code,
            RtspErrorCode::FfmpegExitedUnsuccessfully
        );
        assert_eq!(
            error,
            RtspError::Session(SessionError::UnsuccessfulExit { code: Some(7) })
        );
    }

    #[test]
    fn stdout_read_failure_is_returned_after_reap_and_both_joins() {
        let (mut child, waited, terminated) = FakeChild::running();
        let frames = LatestFrameStore::new();
        let (diagnostics, _) = FfmpegDiagnostics::new(2, 64).unwrap();

        let error = run_session_parts(
            &mut child,
            FailingReader,
            Cursor::new(Vec::<u8>::new()),
            JpegFrameDecoder::new(64).unwrap(),
            frames,
            diagnostics,
            FfmpegLogSanitizer::new(&endpoint(), None).stream(),
            None,
            &SystemThreadSpawner,
        )
        .unwrap_err();

        assert!(waited.load(Ordering::SeqCst));
        assert!(terminated.load(Ordering::SeqCst));
        assert_eq!(error.public_info().code, RtspErrorCode::StdoutReadFailed);
    }

    #[test]
    fn worker_panic_is_structured_without_payload() {
        let (mut child, _, _) = FakeChild::success();
        let frames = LatestFrameStore::new();
        let (diagnostics, _) = FfmpegDiagnostics::new(2, 64).unwrap();

        let error = run_session_parts(
            &mut child,
            Cursor::new(Vec::<u8>::new()),
            PanicReader,
            JpegFrameDecoder::new(64).unwrap(),
            frames,
            diagnostics,
            FfmpegLogSanitizer::new(&endpoint(), None).stream(),
            None,
            &SystemThreadSpawner,
        )
        .unwrap_err();

        assert_eq!(
            error.public_info().code,
            RtspErrorCode::StderrThreadPanicked
        );
        let debug = format!("{error:?} {error}");
        assert!(!debug.contains("camera.example"));
        assert!(!debug.contains("injected reader panic"));
    }

    #[test]
    fn child_wait_error_precedes_worker_failures_and_triggers_terminate() {
        let wait_error: RtspError = FfmpegError::Wait {
            kind: io::ErrorKind::Other,
        }
        .into();
        let (mut child, waited, terminated) = FakeChild::with_wait(Err(wait_error));
        let frames = LatestFrameStore::new();
        let (diagnostics, _) = FfmpegDiagnostics::new(2, 64).unwrap();

        let error = run_session_parts(
            &mut child,
            PanicReader,
            FailingReader,
            JpegFrameDecoder::new(64).unwrap(),
            frames,
            diagnostics,
            FfmpegLogSanitizer::new(&endpoint(), None).stream(),
            None,
            &SystemThreadSpawner,
        )
        .unwrap_err();

        assert!(waited.load(Ordering::SeqCst));
        assert!(terminated.load(Ordering::SeqCst));
        assert_eq!(error.public_info().code, RtspErrorCode::WaitFailed);
    }

    struct FailSecondSpawner {
        calls: AtomicUsize,
    }

    impl WorkerSpawner for FailSecondSpawner {
        fn spawn<T, F>(&self, name: &'static str, work: F) -> io::Result<JoinHandle<T>>
        where
            T: Send + 'static,
            F: FnOnce() -> T + Send + 'static,
        {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 1 {
                Err(io::Error::other("injected spawn failure"))
            } else {
                thread::Builder::new().name(name.to_owned()).spawn(work)
            }
        }
    }

    #[test]
    fn partial_thread_spawn_failure_terminates_and_joins_started_worker() {
        let (path, guard) = input_guard("spawn-failure");
        let (mut child, _, terminated) = FakeChild::success();
        let frames = LatestFrameStore::new();
        let (diagnostics, _) = FfmpegDiagnostics::new(2, 64).unwrap();
        let spawner = FailSecondSpawner {
            calls: AtomicUsize::new(0),
        };

        let error = run_session_parts(
            &mut child,
            Cursor::new(Vec::<u8>::new()),
            Cursor::new(Vec::<u8>::new()),
            JpegFrameDecoder::new(64).unwrap(),
            frames,
            diagnostics,
            FfmpegLogSanitizer::new(&endpoint(), None).stream(),
            Some(guard),
            &spawner,
        )
        .unwrap_err();

        assert!(terminated.load(Ordering::SeqCst));
        assert!(!path.exists());
        assert_eq!(
            error.public_info().code,
            RtspErrorCode::StderrThreadSpawnFailed
        );
    }

    #[test]
    fn public_session_uses_harmless_local_child_and_cleans_input() {
        let (path, guard) = input_guard("local-child");
        let spec = FfmpegCommandSpec::new(std::env::current_exe().unwrap(), &guard).unwrap();
        let frames = LatestFrameStore::new();
        let frames_probe = frames.clone();
        let (diagnostics, _) = FfmpegDiagnostics::new(8, 256).unwrap();
        let diagnostics_probe = diagnostics.clone();
        let session = FfmpegSession::spawn(
            &spec,
            guard,
            JpegFrameDecoder::new(1024).unwrap(),
            frames,
            diagnostics,
            FfmpegLogSanitizer::new(&endpoint(), None).stream(),
        )
        .unwrap();

        let result = session.run();

        assert!(!path.exists());
        assert_stores_closed(&frames_probe, &diagnostics_probe);
        if let Err(error) = result {
            assert_eq!(
                error.public_info().code,
                RtspErrorCode::FfmpegExitedUnsuccessfully
            );
        }
    }

    #[test]
    fn crate_internal_session_path_preserves_stores_with_local_child() {
        let (_path, guard) = input_guard("local-child-preserve");
        let spec = FfmpegCommandSpec::new(std::env::current_exe().unwrap(), &guard).unwrap();
        let frames = LatestFrameStore::new();
        let frames_probe = frames.clone();
        let (diagnostics, _) = FfmpegDiagnostics::new(8, 256).unwrap();
        let diagnostics_probe = diagnostics.clone();
        let session = FfmpegSession::spawn(
            &spec,
            guard,
            JpegFrameDecoder::new(1024).unwrap(),
            frames,
            diagnostics,
            FfmpegLogSanitizer::new(&endpoint(), None).stream(),
        )
        .unwrap();

        let _ = session.run_preserving_stores();

        assert_stores_usable(&frames_probe, &diagnostics_probe);
    }

    #[test]
    fn session_and_outcome_debug_are_secret_safe() {
        let outcome = FfmpegSessionOutcome {
            exit_code: Some(0),
            stdout: PumpSummary {
                bytes_read: 5,
                frames_published: 1,
                first_frame_seen: true,
            },
            stderr: PumpSummary {
                bytes_read: 4,
                frames_published: 0,
                first_frame_seen: false,
            },
        };
        let debug = format!("{outcome:?}");

        assert!(!debug.contains("camera.example"));
        assert!(!debug.contains("example-pass"));
        assert!(!debug.contains("JPEG"));
    }
}
