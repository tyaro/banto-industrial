//! Stable error categories for the pure RTSP foundation types.

use thiserror::Error;

/// The broad category of an error exposed by this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtspErrorCategory {
    /// The caller supplied an invalid endpoint, policy, or decoder limit.
    Config,
    /// The byte stream contained a frame that violated the decoder contract.
    Frame,
    /// FFmpeg launch preparation failed before a process was started.
    Launch,
    /// The latest-frame store could not complete an operation.
    FrameStore,
    /// Sanitized FFmpeg diagnostics could not be retained safely.
    Diagnostics,
    /// A generic FFmpeg stdout or stderr reader failed.
    Pump,
    /// One FFmpeg child session could not complete safely.
    Session,
    /// The restart-supervisor owner thread could not complete safely.
    Supervisor,
}

/// Stable, non-secret error codes suitable for status APIs and telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtspErrorCode {
    InvalidScheme,
    UserInfoNotAllowed,
    EmptyHost,
    ControlCharacter,
    InvalidAuthority,
    InvalidPort,
    InvalidReconnectPolicy,
    InvalidIoTimeout,
    InvalidFrameLimit,
    InvalidRuntimeDirectory,
    RuntimeDirectoryProbeFailed,
    FrameTooLarge,
    EmptyExecutablePath,
    InputFileCreateFailed,
    InputFileWriteFailed,
    InputFileRemoveFailed,
    SpawnFailed,
    TryWaitFailed,
    WaitFailed,
    TerminateKillFailed,
    StdioAlreadyTaken,
    FrameStoreClosed,
    FrameSequenceExhausted,
    FrameStorePoisoned,
    InvalidDiagnosticsConfig,
    DiagnosticsClosed,
    DiagnosticsPoisoned,
    StdoutReadFailed,
    StderrReadFailed,
    StdoutThreadSpawnFailed,
    StderrThreadSpawnFailed,
    StdoutThreadPanicked,
    StderrThreadPanicked,
    FfmpegExitedUnsuccessfully,
    SupervisorThreadSpawnFailed,
    SupervisorThreadPanicked,
}

/// Public, structured error information without endpoint or credential text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtspErrorInfo {
    pub category: RtspErrorCategory,
    pub code: RtspErrorCode,
}

/// The operation that failed while preparing an FFmpeg input file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfmpegFileOperation {
    Create,
    Write,
    Remove,
}

/// The FFmpeg stream whose ownership was requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfmpegStream {
    Stdout,
    Stderr,
}

/// The FFmpeg stream handled by a generic reader pump.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PumpStream {
    Stdout,
    Stderr,
}

/// The named reader worker owned by one FFmpeg session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionWorker {
    Stdout,
    Stderr,
}

/// Non-secret errors from the FFmpeg launch preparation layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum FfmpegError {
    #[error("FFmpeg executable path must not be empty")]
    EmptyExecutablePath,
    #[error("FFmpeg input file {operation:?} failed ({kind:?})")]
    InputFileIo {
        operation: FfmpegFileOperation,
        kind: std::io::ErrorKind,
    },
    #[error("FFmpeg process spawn failed ({kind:?})")]
    Spawn { kind: std::io::ErrorKind },
    #[error("FFmpeg process status check failed ({kind:?})")]
    TryWait { kind: std::io::ErrorKind },
    #[error("FFmpeg process wait failed ({kind:?})")]
    Wait { kind: std::io::ErrorKind },
    #[error("FFmpeg process termination failed ({kind:?})")]
    TerminateKill { kind: std::io::ErrorKind },
    #[error("FFmpeg {stream:?} has already been taken")]
    StdioAlreadyTaken { stream: FfmpegStream },
}

/// Errors produced by the bounded latest-frame store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum FrameStoreError {
    #[error("latest-frame store is closed")]
    Closed,
    #[error("latest-frame sequence is exhausted")]
    SequenceExhausted,
    #[error("latest-frame store synchronization state is poisoned")]
    Poisoned,
}

/// Errors from the bounded, sanitized stderr diagnostics store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DiagnosticsError {
    #[error("diagnostics configuration is invalid")]
    InvalidConfig,
    #[error("diagnostics store is closed")]
    Closed,
    #[error("diagnostics store synchronization state is poisoned")]
    Poisoned,
}

/// Non-secret errors from generic FFmpeg reader pumps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PumpError {
    #[error("{stream:?} reader failed ({kind:?})")]
    Read {
        stream: PumpStream,
        kind: std::io::ErrorKind,
    },
}

/// Non-secret lifecycle errors from one FFmpeg session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SessionError {
    #[error("{worker:?} reader thread could not be spawned ({kind:?})")]
    ThreadSpawn {
        worker: SessionWorker,
        kind: std::io::ErrorKind,
    },
    #[error("{worker:?} reader thread panicked")]
    ThreadPanicked { worker: SessionWorker },
    #[error("FFmpeg exited unsuccessfully (code: {code:?})")]
    UnsuccessfulExit { code: Option<i32> },
}

/// Non-secret lifecycle errors from the restart-supervisor owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SupervisorError {
    #[error("restart supervisor thread could not be spawned ({kind:?})")]
    ThreadSpawn { kind: std::io::ErrorKind },
    #[error("restart supervisor thread panicked")]
    ThreadPanicked,
}

/// Configuration failures. Variants intentionally do not carry the rejected
/// input, because an RTSP URL or credential can contain sensitive material.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum RtspConfigError {
    #[error("only rtsp:// and rtsps:// schemes are supported")]
    InvalidScheme,
    #[error("RTSP endpoint authority must not contain userinfo")]
    UserInfoNotAllowed,
    #[error("RTSP endpoint host must not be empty")]
    EmptyHost,
    #[error("RTSP endpoint must not contain ASCII control characters")]
    ControlCharacter,
    #[error("RTSP endpoint authority is invalid")]
    InvalidAuthority,
    #[error("RTSP endpoint port is invalid")]
    InvalidPort,
    #[error("reconnect policy parameters are invalid")]
    InvalidReconnectPolicy,
    #[error("RTSP I/O timeout is invalid")]
    InvalidIoTimeout,
    #[error("JPEG frame size limit is too small")]
    InvalidFrameLimit,
    #[error("FFmpeg runtime directory is invalid")]
    InvalidRuntimeDirectory,
    #[error("FFmpeg runtime directory access probe failed ({kind:?})")]
    RuntimeDirectoryProbeFailed { kind: std::io::ErrorKind },
}

/// Errors returned by the pure RTSP foundation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum RtspError {
    #[error("invalid RTSP configuration: {0}")]
    Config(#[from] RtspConfigError),
    #[error("JPEG frame exceeded the configured size limit of {max_frame_bytes} bytes")]
    FrameTooLarge { max_frame_bytes: usize },
    #[error("FFmpeg launch preparation failed: {0}")]
    Ffmpeg(#[from] FfmpegError),
    #[error("latest-frame store failed: {0}")]
    FrameStore(#[from] FrameStoreError),
    #[error("FFmpeg diagnostics failed: {0}")]
    Diagnostics(#[from] DiagnosticsError),
    #[error("FFmpeg stream pump failed: {0}")]
    Pump(#[from] PumpError),
    #[error("FFmpeg session failed: {0}")]
    Session(#[from] SessionError),
    #[error("FFmpeg restart supervisor failed: {0}")]
    Supervisor(#[from] SupervisorError),
}

impl RtspError {
    /// Returns the stable broad category for this error.
    pub const fn category(self) -> RtspErrorCategory {
        match self {
            Self::Config(_) => RtspErrorCategory::Config,
            Self::FrameTooLarge { .. } => RtspErrorCategory::Frame,
            Self::Ffmpeg(_) => RtspErrorCategory::Launch,
            Self::FrameStore(_) => RtspErrorCategory::FrameStore,
            Self::Diagnostics(_) => RtspErrorCategory::Diagnostics,
            Self::Pump(_) => RtspErrorCategory::Pump,
            Self::Session(_) => RtspErrorCategory::Session,
            Self::Supervisor(_) => RtspErrorCategory::Supervisor,
        }
    }

    /// Converts the error to safe information for a public status surface.
    pub const fn public_info(self) -> RtspErrorInfo {
        let (category, code) = match self {
            Self::Config(config) => (
                RtspErrorCategory::Config,
                match config {
                    RtspConfigError::InvalidScheme => RtspErrorCode::InvalidScheme,
                    RtspConfigError::UserInfoNotAllowed => RtspErrorCode::UserInfoNotAllowed,
                    RtspConfigError::EmptyHost => RtspErrorCode::EmptyHost,
                    RtspConfigError::ControlCharacter => RtspErrorCode::ControlCharacter,
                    RtspConfigError::InvalidAuthority => RtspErrorCode::InvalidAuthority,
                    RtspConfigError::InvalidPort => RtspErrorCode::InvalidPort,
                    RtspConfigError::InvalidReconnectPolicy => {
                        RtspErrorCode::InvalidReconnectPolicy
                    }
                    RtspConfigError::InvalidIoTimeout => RtspErrorCode::InvalidIoTimeout,
                    RtspConfigError::InvalidFrameLimit => RtspErrorCode::InvalidFrameLimit,
                    RtspConfigError::InvalidRuntimeDirectory => {
                        RtspErrorCode::InvalidRuntimeDirectory
                    }
                    RtspConfigError::RuntimeDirectoryProbeFailed { .. } => {
                        RtspErrorCode::RuntimeDirectoryProbeFailed
                    }
                },
            ),
            Self::FrameTooLarge { .. } => (RtspErrorCategory::Frame, RtspErrorCode::FrameTooLarge),
            Self::Ffmpeg(error) => (
                RtspErrorCategory::Launch,
                match error {
                    FfmpegError::EmptyExecutablePath => RtspErrorCode::EmptyExecutablePath,
                    FfmpegError::InputFileIo { operation, .. } => match operation {
                        FfmpegFileOperation::Create => RtspErrorCode::InputFileCreateFailed,
                        FfmpegFileOperation::Write => RtspErrorCode::InputFileWriteFailed,
                        FfmpegFileOperation::Remove => RtspErrorCode::InputFileRemoveFailed,
                    },
                    FfmpegError::Spawn { .. } => RtspErrorCode::SpawnFailed,
                    FfmpegError::TryWait { .. } => RtspErrorCode::TryWaitFailed,
                    FfmpegError::Wait { .. } => RtspErrorCode::WaitFailed,
                    FfmpegError::TerminateKill { .. } => RtspErrorCode::TerminateKillFailed,
                    FfmpegError::StdioAlreadyTaken { .. } => RtspErrorCode::StdioAlreadyTaken,
                },
            ),
            Self::FrameStore(error) => (
                RtspErrorCategory::FrameStore,
                match error {
                    FrameStoreError::Closed => RtspErrorCode::FrameStoreClosed,
                    FrameStoreError::SequenceExhausted => RtspErrorCode::FrameSequenceExhausted,
                    FrameStoreError::Poisoned => RtspErrorCode::FrameStorePoisoned,
                },
            ),
            Self::Diagnostics(error) => (
                RtspErrorCategory::Diagnostics,
                match error {
                    DiagnosticsError::InvalidConfig => RtspErrorCode::InvalidDiagnosticsConfig,
                    DiagnosticsError::Closed => RtspErrorCode::DiagnosticsClosed,
                    DiagnosticsError::Poisoned => RtspErrorCode::DiagnosticsPoisoned,
                },
            ),
            Self::Pump(error) => (
                RtspErrorCategory::Pump,
                match error {
                    PumpError::Read {
                        stream: PumpStream::Stdout,
                        ..
                    } => RtspErrorCode::StdoutReadFailed,
                    PumpError::Read {
                        stream: PumpStream::Stderr,
                        ..
                    } => RtspErrorCode::StderrReadFailed,
                },
            ),
            Self::Session(error) => (
                RtspErrorCategory::Session,
                match error {
                    SessionError::ThreadSpawn {
                        worker: SessionWorker::Stdout,
                        ..
                    } => RtspErrorCode::StdoutThreadSpawnFailed,
                    SessionError::ThreadSpawn {
                        worker: SessionWorker::Stderr,
                        ..
                    } => RtspErrorCode::StderrThreadSpawnFailed,
                    SessionError::ThreadPanicked {
                        worker: SessionWorker::Stdout,
                    } => RtspErrorCode::StdoutThreadPanicked,
                    SessionError::ThreadPanicked {
                        worker: SessionWorker::Stderr,
                    } => RtspErrorCode::StderrThreadPanicked,
                    SessionError::UnsuccessfulExit { .. } => {
                        RtspErrorCode::FfmpegExitedUnsuccessfully
                    }
                },
            ),
            Self::Supervisor(error) => (
                RtspErrorCategory::Supervisor,
                match error {
                    SupervisorError::ThreadSpawn { .. } => {
                        RtspErrorCode::SupervisorThreadSpawnFailed
                    }
                    SupervisorError::ThreadPanicked => RtspErrorCode::SupervisorThreadPanicked,
                },
            ),
        };

        RtspErrorInfo { category, code }
    }
}
