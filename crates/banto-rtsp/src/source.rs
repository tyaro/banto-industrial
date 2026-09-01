//! Production composition for the synchronous FFmpeg restart supervisor.
//!
//! Static options are validated before an owner thread is started. Per-attempt
//! input files remain inside a caller-owned protected runtime directory and
//! contain the authenticated URL only for as long as FFmpeg needs it.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use crate::session::{FfmpegSessionCompletion, SessionStopToken};
use crate::supervisor::{start_supervisor, AttemptReporter, SessionAttemptFactory};
use crate::{
    FfmpegDiagnostics, FfmpegDiagnosticsHandle, FfmpegError, FfmpegFileOperation, FfmpegInputFile,
    FfmpegLogSanitizer, FfmpegSession, JpegFrameDecoder, LatestFrameHandle, LatestFrameStore,
    RtspConfig, RtspConfigError, RtspError, VideoStatus, VideoSupervisor, VideoSupervisorHandle,
};

const UNIQUE_FILE_ATTEMPTS: usize = 32;
const INPUT_FILE_PREFIX: &str = "banto-rtsp-input";
const PROBE_FILE_PREFIX: &str = "banto-rtsp-probe";

static FILE_NONCE: AtomicU64 = AtomicU64::new(0);

/// Validated static configuration for starting an [`RtspVideoSource`].
///
/// The runtime directory must already exist and be owned by the caller. The
/// constructor probes create/remove access but does not create the directory
/// or modify permissions. On Windows, the caller must apply a protected ACL
/// that grants access only to the application identity and administrators.
pub struct FfmpegSupervisorOptions {
    config: RtspConfig,
    executable: PathBuf,
    runtime_directory: PathBuf,
    max_frame_bytes: usize,
    max_diagnostic_entries: usize,
    max_diagnostic_entry_bytes: usize,
}

impl FfmpegSupervisorOptions {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: RtspConfig,
        executable: impl Into<PathBuf>,
        runtime_directory: impl Into<PathBuf>,
        max_frame_bytes: usize,
        max_diagnostic_entries: usize,
        max_diagnostic_entry_bytes: usize,
    ) -> Result<Self, RtspError> {
        let options = Self {
            config,
            executable: executable.into(),
            runtime_directory: runtime_directory.into(),
            max_frame_bytes,
            max_diagnostic_entries,
            max_diagnostic_entry_bytes,
        };
        options.validate()?;
        Ok(options)
    }

    /// Revalidates static resources, then starts the production owner thread.
    pub fn start(self) -> Result<RtspVideoSource, RtspError> {
        RtspVideoSource::start(self)
    }

    fn validate(&self) -> Result<(), RtspError> {
        if self.executable.as_os_str().is_empty() {
            return Err(FfmpegError::EmptyExecutablePath.into());
        }
        JpegFrameDecoder::new(self.max_frame_bytes)?;
        let _ =
            FfmpegDiagnostics::new(self.max_diagnostic_entries, self.max_diagnostic_entry_bytes)?;
        validate_runtime_directory(&self.runtime_directory)
    }
}

impl fmt::Debug for FfmpegSupervisorOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FfmpegSupervisorOptions")
            .field("config_present", &true)
            .field(
                "executable_present",
                &!self.executable.as_os_str().is_empty(),
            )
            .field("runtime_directory_present", &true)
            .field("max_frame_bytes", &self.max_frame_bytes)
            .field("max_diagnostic_entries", &self.max_diagnostic_entries)
            .field(
                "max_diagnostic_entry_bytes",
                &self.max_diagnostic_entry_bytes,
            )
            .finish()
    }
}

/// Owner for one production FFmpeg RTSP source and its stable consumer handles.
pub struct RtspVideoSource {
    supervisor: Option<VideoSupervisor>,
    supervisor_handle: VideoSupervisorHandle,
    frames: LatestFrameHandle,
    diagnostics: FfmpegDiagnosticsHandle,
}

impl RtspVideoSource {
    /// Validates options again immediately before creating shared stores and
    /// starting the supervisor thread.
    pub fn start(options: FfmpegSupervisorOptions) -> Result<Self, RtspError> {
        start_source_with_factory(options, |options, frames, diagnostics| {
            ProductionSessionFactory::new(options, frames, diagnostics)
        })
    }

    pub fn frames(&self) -> LatestFrameHandle {
        self.frames.clone()
    }

    pub fn diagnostics(&self) -> FfmpegDiagnosticsHandle {
        self.diagnostics.clone()
    }

    pub fn supervisor_handle(&self) -> VideoSupervisorHandle {
        self.supervisor_handle.clone()
    }

    pub fn status(&self) -> VideoStatus {
        self.supervisor_handle.status()
    }

    /// Requests shutdown. Repeated requests are harmless.
    pub fn request_stop(&self) {
        self.supervisor_handle.request_stop();
    }

    /// Requests shutdown and joins the supervisor and current session.
    pub fn stop_and_join(mut self) -> Result<(), RtspError> {
        match self.supervisor.take() {
            Some(supervisor) => supervisor.stop_and_join(),
            None => Ok(()),
        }
    }
}

impl fmt::Debug for RtspVideoSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RtspVideoSource")
            .field("status", &self.status())
            .field("supervisor_owned", &self.supervisor.is_some())
            .finish()
    }
}

impl Drop for RtspVideoSource {
    fn drop(&mut self) {
        if let Some(supervisor) = self.supervisor.take() {
            drop(supervisor);
        }
    }
}

fn start_source_with_factory<B, F>(
    options: FfmpegSupervisorOptions,
    build_factory: B,
) -> Result<RtspVideoSource, RtspError>
where
    B: FnOnce(&FfmpegSupervisorOptions, LatestFrameStore, FfmpegDiagnostics) -> F,
    F: SessionAttemptFactory,
{
    options.validate()?;
    let reconnect = *options.config.reconnect_policy();
    let frames = LatestFrameStore::new();
    let frame_handle = frames.handle();
    let (diagnostics, diagnostics_handle) = FfmpegDiagnostics::new(
        options.max_diagnostic_entries,
        options.max_diagnostic_entry_bytes,
    )?;
    let factory = build_factory(&options, frames.clone(), diagnostics.clone());
    let supervisor = start_supervisor(factory, reconnect, frames, diagnostics)?;
    let supervisor_handle = supervisor.handle();

    Ok(RtspVideoSource {
        supervisor: Some(supervisor),
        supervisor_handle,
        frames: frame_handle,
        diagnostics: diagnostics_handle,
    })
}

struct ProductionSessionFactory {
    config: RtspConfig,
    executable: PathBuf,
    runtime_directory: PathBuf,
    max_frame_bytes: usize,
    frames: LatestFrameStore,
    diagnostics: FfmpegDiagnostics,
}

impl ProductionSessionFactory {
    fn new(
        options: &FfmpegSupervisorOptions,
        frames: LatestFrameStore,
        diagnostics: FfmpegDiagnostics,
    ) -> Self {
        Self {
            config: options.config.clone(),
            executable: options.executable.clone(),
            runtime_directory: options.runtime_directory.clone(),
            max_frame_bytes: options.max_frame_bytes,
            frames,
            diagnostics,
        }
    }

    fn build_session(&self) -> Result<FfmpegSession, RtspError> {
        let input = create_unique_input_file(
            &self.runtime_directory,
            self.config.endpoint(),
            self.config.credentials(),
            self.config.transport(),
            self.config.validated_io_timeout(),
        )?;
        let spec = crate::FfmpegCommandSpec::new(&self.executable, &input)?;
        let decoder = JpegFrameDecoder::new(self.max_frame_bytes)?;
        let sanitizer = self.sanitizer(&input).stream();
        FfmpegSession::spawn(
            &spec,
            input,
            decoder,
            self.frames.clone(),
            self.diagnostics.clone(),
            sanitizer,
        )
    }

    fn sanitizer(&self, input: &FfmpegInputFile) -> FfmpegLogSanitizer {
        let mut sanitizer =
            FfmpegLogSanitizer::new(self.config.endpoint(), self.config.credentials());
        add_path_patterns(&mut sanitizer, &self.executable);
        add_path_patterns(&mut sanitizer, &self.runtime_directory);
        add_path_patterns(&mut sanitizer, input.path());
        sanitizer
    }
}

impl SessionAttemptFactory for ProductionSessionFactory {
    fn run_attempt(
        &mut self,
        stop: SessionStopToken,
        reporter: AttemptReporter,
    ) -> Result<FfmpegSessionCompletion, RtspError> {
        self.build_session()?
            .run_preserving_stores_until_with_first_frame(stop, move |received_at| {
                reporter.frame(received_at);
            })
    }
}

fn create_unique_input_file(
    directory: &Path,
    endpoint: &crate::RtspEndpoint,
    credentials: Option<&crate::RtspCredentials>,
    transport: crate::RtspTransport,
    io_timeout: crate::config::ValidatedIoTimeout,
) -> Result<FfmpegInputFile, RtspError> {
    create_unique_input_file_with(
        directory,
        endpoint,
        credentials,
        transport,
        io_timeout,
        next_file_nonce,
    )
}

fn create_unique_input_file_with<F>(
    directory: &Path,
    endpoint: &crate::RtspEndpoint,
    credentials: Option<&crate::RtspCredentials>,
    transport: crate::RtspTransport,
    io_timeout: crate::config::ValidatedIoTimeout,
    mut next_nonce: F,
) -> Result<FfmpegInputFile, RtspError>
where
    F: FnMut() -> u64,
{
    for _ in 0..UNIQUE_FILE_ATTEMPTS {
        let path = unique_path(directory, INPUT_FILE_PREFIX, next_nonce());
        match FfmpegInputFile::create_new_validated(
            path,
            endpoint,
            credentials,
            transport,
            io_timeout,
        ) {
            Ok(input) => return Ok(input),
            Err(RtspError::Ffmpeg(FfmpegError::InputFileIo {
                operation: FfmpegFileOperation::Create,
                kind: io::ErrorKind::AlreadyExists,
            })) => {}
            Err(error) => return Err(error),
        }
    }
    Err(FfmpegError::InputFileIo {
        operation: FfmpegFileOperation::Create,
        kind: io::ErrorKind::AlreadyExists,
    }
    .into())
}

fn validate_runtime_directory(directory: &Path) -> Result<(), RtspError> {
    if directory.as_os_str().is_empty() {
        return Err(RtspConfigError::InvalidRuntimeDirectory.into());
    }
    let metadata = fs::metadata(directory)
        .map_err(|_| RtspError::from(RtspConfigError::InvalidRuntimeDirectory))?;
    if !metadata.is_dir() {
        return Err(RtspConfigError::InvalidRuntimeDirectory.into());
    }

    for _ in 0..UNIQUE_FILE_ATTEMPTS {
        let path = unique_path(directory, PROBE_FILE_PREFIX, next_file_nonce());
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        match options.open(&path) {
            Ok(mut file) => {
                let guard = ProbeFile::new(path);
                file.write_all(b"probe")
                    .map_err(|error| runtime_probe_error(error.kind()))?;
                drop(file);
                return guard.cleanup();
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(runtime_probe_error(error.kind())),
        }
    }
    Err(runtime_probe_error(io::ErrorKind::AlreadyExists))
}

struct ProbeFile {
    path: Option<PathBuf>,
}

impl ProbeFile {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn cleanup(mut self) -> Result<(), RtspError> {
        let path = self.path.as_ref().expect("probe path is present");
        fs::remove_file(path).map_err(|error| runtime_probe_error(error.kind()))?;
        self.path = None;
        Ok(())
    }
}

impl Drop for ProbeFile {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = fs::remove_file(path);
        }
    }
}

fn runtime_probe_error(kind: io::ErrorKind) -> RtspError {
    RtspConfigError::RuntimeDirectoryProbeFailed { kind }.into()
}

fn unique_path(directory: &Path, prefix: &str, nonce: u64) -> PathBuf {
    directory.join(format!(
        "{prefix}-{}-{nonce:016x}.ffconcat",
        std::process::id()
    ))
}

fn next_file_nonce() -> u64 {
    FILE_NONCE.fetch_add(1, Ordering::Relaxed)
}

fn add_path_patterns(sanitizer: &mut FfmpegLogSanitizer, path: &Path) {
    let path = path.to_string_lossy().into_owned();
    for variant in [
        path.clone(),
        path.replace('\\', "/"),
        path.to_lowercase(),
        path.to_uppercase(),
    ] {
        sanitizer.add_sensitive_pattern(&variant);
        sanitizer.add_sensitive_pattern(variant.replace('\'', "'\\''"));
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::fs;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier, Mutex};
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::{
        FfmpegSessionOutcome, PumpSummary, ReconnectPolicy, RtspCredentials, RtspEndpoint,
        RtspErrorCode, RtspTransport,
    };

    static TEST_DIRECTORY_NONCE: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let nonce = TEST_DIRECTORY_NONCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "banto-rtsp-source-{label}-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn config() -> RtspConfig {
        RtspConfig::new(
            RtspEndpoint::new("rtsp://camera.example/private-stream").unwrap(),
            Some(RtspCredentials::new("private-user", "private-pass")),
            RtspTransport::Tcp,
            Duration::from_secs(5),
            ReconnectPolicy::new(Duration::from_nanos(1), Duration::from_nanos(1), 1).unwrap(),
        )
        .unwrap()
    }

    fn options(directory: &Path, executable: &str) -> FfmpegSupervisorOptions {
        FfmpegSupervisorOptions::new(config(), executable, directory, 1024, 4, 64).unwrap()
    }

    fn empty_summary(first_frame_seen: bool) -> PumpSummary {
        PumpSummary {
            bytes_read: 0,
            frames_published: u64::from(first_frame_seen),
            first_frame_seen,
        }
    }

    fn exited() -> FfmpegSessionCompletion {
        FfmpegSessionCompletion::Exited(FfmpegSessionOutcome {
            exit_code: Some(0),
            stdout: empty_summary(false),
            stderr: empty_summary(false),
        })
    }

    fn stopped() -> FfmpegSessionCompletion {
        FfmpegSessionCompletion::Stopped {
            stdout: empty_summary(false),
            stderr: empty_summary(false),
        }
    }

    #[test]
    fn static_validation_finishes_before_factory_builder() {
        let directory = TestDirectory::new("static-validation");
        let options = options(&directory.0, "ffmpeg-private-path");
        fs::remove_dir(&directory.0).unwrap();
        let builds = Arc::new(AtomicUsize::new(0));
        let build_probe = Arc::clone(&builds);

        let result = start_source_with_factory(options, move |_, _, _| {
            build_probe.fetch_add(1, Ordering::Relaxed);
            StableFactory::default()
        });

        assert_eq!(
            result.unwrap_err().public_info().code,
            RtspErrorCode::InvalidRuntimeDirectory
        );
        assert_eq!(builds.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn options_reject_all_static_bounds_without_leaving_probe_files() {
        let directory = TestDirectory::new("invalid-options");

        assert_eq!(
            FfmpegSupervisorOptions::new(config(), "", &directory.0, 1024, 4, 64)
                .unwrap_err()
                .public_info()
                .code,
            RtspErrorCode::EmptyExecutablePath
        );
        assert_eq!(
            FfmpegSupervisorOptions::new(config(), "ffmpeg", &directory.0, 3, 4, 64)
                .unwrap_err()
                .public_info()
                .code,
            RtspErrorCode::InvalidFrameLimit
        );
        assert_eq!(
            FfmpegSupervisorOptions::new(config(), "ffmpeg", &directory.0, 1024, 0, 64)
                .unwrap_err()
                .public_info()
                .code,
            RtspErrorCode::InvalidDiagnosticsConfig
        );
        let regular_file = directory.0.join("not-a-directory");
        fs::write(&regular_file, "safe").unwrap();
        assert_eq!(
            FfmpegSupervisorOptions::new(config(), "ffmpeg", &regular_file, 1024, 4, 64)
                .unwrap_err()
                .public_info()
                .code,
            RtspErrorCode::InvalidRuntimeDirectory
        );
        assert_eq!(
            fs::read_dir(&directory.0)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .contains(PROBE_FILE_PREFIX))
                .count(),
            0
        );
    }

    #[test]
    fn unique_input_creation_skips_create_new_collision() {
        let directory = TestDirectory::new("collision");
        let first = unique_path(&directory.0, INPUT_FILE_PREFIX, 7);
        let second = unique_path(&directory.0, INPUT_FILE_PREFIX, 8);
        fs::write(&first, "sentinel").unwrap();
        let mut nonces = VecDeque::from([7, 8]);

        let input = create_unique_input_file_with(
            &directory.0,
            config().endpoint(),
            None,
            RtspTransport::Tcp,
            config().validated_io_timeout(),
            || nonces.pop_front().unwrap(),
        )
        .unwrap();

        assert_eq!(input.path(), second);
        assert!(fs::read_to_string(input.path())
            .unwrap()
            .contains("option timeout 5000000\n"));
        assert_eq!(fs::read_to_string(first).unwrap(), "sentinel");
        drop(input);
        assert!(!second.exists());
    }

    #[derive(Default)]
    struct StableFactory {
        attempts: usize,
    }

    impl SessionAttemptFactory for StableFactory {
        fn run_attempt(
            &mut self,
            _stop: SessionStopToken,
            _reporter: AttemptReporter,
        ) -> Result<FfmpegSessionCompletion, RtspError> {
            self.attempts += 1;
            Ok(if self.attempts == 1 {
                exited()
            } else {
                stopped()
            })
        }
    }

    struct PublishingFactory {
        attempts: usize,
        frames: LatestFrameStore,
        diagnostics: FfmpegDiagnostics,
        completed: Arc<Barrier>,
    }

    impl SessionAttemptFactory for PublishingFactory {
        fn run_attempt(
            &mut self,
            _stop: SessionStopToken,
            _reporter: AttemptReporter,
        ) -> Result<FfmpegSessionCompletion, RtspError> {
            self.attempts += 1;
            self.frames
                .publish(vec![self.attempts as u8], SystemTime::UNIX_EPOCH)
                .unwrap();
            self.diagnostics
                .push_sanitized(&format!("attempt {}", self.attempts))
                .unwrap();
            if self.attempts == 1 {
                Ok(exited())
            } else {
                self.completed.wait();
                Ok(stopped())
            }
        }
    }

    #[test]
    fn public_consumer_handles_remain_stable_across_retries() {
        let directory = TestDirectory::new("stable-handles");
        let completed = Arc::new(Barrier::new(2));
        let factory_completed = Arc::clone(&completed);
        let source = start_source_with_factory(
            options(&directory.0, "ffmpeg"),
            move |_, frames, diagnostics| PublishingFactory {
                attempts: 0,
                frames,
                diagnostics,
                completed: factory_completed,
            },
        )
        .unwrap();
        let frames = source.frames();
        let diagnostics = source.diagnostics();

        completed.wait();
        source.stop_and_join().unwrap();

        let latest = frames.snapshot().unwrap().unwrap();
        assert_eq!(latest.sequence, 2);
        assert_eq!(latest.jpeg, [2]);
        assert_eq!(
            diagnostics.snapshot().unwrap(),
            ["attempt 1".to_owned(), "attempt 2".to_owned()]
        );
    }

    #[test]
    fn stop_join_closes_stores_owned_by_source() {
        let directory = TestDirectory::new("source-close");
        let frame_probe = Arc::new(Mutex::new(None));
        let diagnostics_probe = Arc::new(Mutex::new(None));
        let frame_probe_writer = Arc::clone(&frame_probe);
        let diagnostics_probe_writer = Arc::clone(&diagnostics_probe);
        let source = start_source_with_factory(
            options(&directory.0, "ffmpeg"),
            move |_, frames, diagnostics| {
                *frame_probe_writer.lock().unwrap() = Some(frames.clone());
                *diagnostics_probe_writer.lock().unwrap() = Some(diagnostics.clone());
                StableFactory::default()
            },
        )
        .unwrap();

        source.request_stop();
        source.request_stop();
        source.stop_and_join().unwrap();

        let frame_error = frame_probe
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .publish(Vec::new(), SystemTime::now())
            .unwrap_err();
        let diagnostics_error = diagnostics_probe
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .push_sanitized("safe")
            .unwrap_err();
        assert_eq!(
            frame_error.public_info().code,
            RtspErrorCode::FrameStoreClosed
        );
        assert_eq!(
            diagnostics_error.public_info().code,
            RtspErrorCode::DiagnosticsClosed
        );
    }

    #[test]
    fn input_guard_is_removed_when_process_spawn_fails() {
        let directory = TestDirectory::new("spawn-cleanup");
        let options = FfmpegSupervisorOptions::new(
            config(),
            directory.0.join("missing-test-executable"),
            &directory.0,
            1024,
            4,
            64,
        )
        .unwrap();
        let frames = LatestFrameStore::new();
        let (diagnostics, _) = FfmpegDiagnostics::new(4, 64).unwrap();
        let factory = ProductionSessionFactory::new(&options, frames, diagnostics);

        let error = factory.build_session().unwrap_err();

        assert_eq!(error.public_info().code, RtspErrorCode::SpawnFailed);
        assert!(fs::read_dir(&directory.0).unwrap().next().is_none());
    }

    #[test]
    fn production_sanitizer_hides_endpoint_and_all_runtime_paths() {
        let directory = TestDirectory::new("diagnostic-private'runtime");
        let executable = directory.0.join("private-ffmpeg-executable");
        let options =
            FfmpegSupervisorOptions::new(config(), &executable, &directory.0, 1024, 8, 512)
                .unwrap();
        let frames = LatestFrameStore::new();
        let (diagnostics, handle) = FfmpegDiagnostics::new(8, 512).unwrap();
        let factory = ProductionSessionFactory::new(&options, frames, diagnostics.clone());
        let input = create_unique_input_file(
            &directory.0,
            options.config.endpoint(),
            options.config.credentials(),
            options.config.transport(),
            options.config.validated_io_timeout(),
        )
        .unwrap();
        let escaped_input = input.path().to_string_lossy().replace('\'', "'\\''");
        let raw = format!(
            "host=camera.example resource=/private-stream executable={} runtime={} input={} escaped={escaped_input}",
            executable.display(),
            directory.0.display(),
            input.path().display()
        );

        crate::pump_stderr(
            std::io::Cursor::new(raw.as_bytes()),
            factory.sanitizer(&input).stream(),
            &diagnostics,
        )
        .unwrap();

        let retained = handle.snapshot().unwrap().join("");
        let input_text = input.path().to_string_lossy().into_owned();
        for secret in [
            "camera.example",
            "/private-stream",
            "private-ffmpeg-executable",
            "diagnostic-private'runtime",
            "diagnostic-private'\\''runtime",
            input_text.as_str(),
        ] {
            assert!(!retained.contains(secret));
        }
        assert!(retained.contains("[REDACTED]"));
    }

    #[test]
    fn public_debug_and_errors_hide_all_sensitive_values() {
        let directory = TestDirectory::new("private-runtime-name");
        let start_options = options(&directory.0, "private-ffmpeg-path");
        let options_debug = format!("{start_options:?}");
        let config_debug = format!("{:?}", config());
        let error_debug = format!(
            "{:?}",
            FfmpegSupervisorOptions::new(config(), "", &directory.0, 1024, 4, 64).unwrap_err()
        );
        let source =
            start_source_with_factory(options(&directory.0, "private-ffmpeg-path"), |_, _, _| {
                StableFactory::default()
            })
            .unwrap();
        let source_debug = format!("{source:?}");
        source.stop_and_join().unwrap();

        for secret in [
            "camera.example",
            "private-stream",
            "private-user",
            "private-pass",
            "private-runtime-name",
            "private-ffmpeg-path",
        ] {
            assert!(!options_debug.contains(secret));
            assert!(!config_debug.contains(secret));
            assert!(!error_debug.contains(secret));
            assert!(!source_debug.contains(secret));
        }
    }
}
