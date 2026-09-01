//! Pure RTSP foundation types for banto-industrial.
//!
//! This crate owns validated endpoint/credential separation, mandatory finite
//! RTSP I/O timeout, reconnect policy, JPEG frame extraction, and
//! transport-independent video status. It is kept
//! below application and service boundaries: Tauri, axum/HTTP, Banto-HUB, and
//! network I/O must be adapters or supervisors outside this crate.
//!
//! The foundation slices deliberately do not open RTSP sockets, connect to
//! Modbus or Banto-HUB, expose Tauri commands, or deliver a UI. The process
//! primitive and one-shot session own child/process cleanup and reader threads.
//! The production source owner composes validated runtime options, direct
//! FFmpeg sessions, bounded stores, and the restart supervisor. FFmpeg binary
//! Local FFmpeg capability checks cover concat input and bounded RTSP timeout;
//! real-camera behavior and application wiring remain integration concerns.
//!
//! Security invariant: an endpoint can never contain authority userinfo, and
//! credentials are held separately. Debug output and public status/error values
//! never expose a password, a complete username, or rejected URL text.

mod config;
mod diagnostics;
mod endpoint;
mod error;
mod frame;
mod frame_store;
mod launch;
mod process;
mod pump;
mod reconnect;
mod session;
mod source;
mod status;
mod supervisor;

pub use config::RtspConfig;
pub use diagnostics::{FfmpegDiagnostics, FfmpegDiagnosticsHandle};
pub use endpoint::{RtspCredentials, RtspEndpoint, RtspScheme};
pub use error::{
    DiagnosticsError, FfmpegError, FfmpegFileOperation, FfmpegStream, FrameStoreError, PumpError,
    PumpStream, RtspConfigError, RtspError, RtspErrorCategory, RtspErrorCode, RtspErrorInfo,
    SessionError, SessionWorker, SupervisorError,
};
pub use frame::{JpegFrameDecoder, VideoFrame};
pub use frame_store::{FrameWaitResult, LatestFrameHandle, LatestFrameStore};
pub use launch::{
    FfmpegCommandSpec, FfmpegInputFile, FfmpegLogSanitizer, FfmpegLogStreamSanitizer,
};
pub use process::FfmpegChild;
pub use pump::{pump_jpeg_stream, pump_stderr, PumpSummary};
pub use reconnect::{ReconnectPolicy, RtspTransport};
pub use session::{FfmpegSession, FfmpegSessionOutcome};
pub use source::{FfmpegSupervisorOptions, RtspVideoSource};
pub use status::{VideoState, VideoStatus};
pub use supervisor::{VideoSupervisor, VideoSupervisorHandle};
