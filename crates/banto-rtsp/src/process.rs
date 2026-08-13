//! Owned FFmpeg child-process operations for a future RTSP supervisor.
//!
//! This module owns one directly spawned child and its piped standard streams.
//! It does not create a shell, start reader threads, parse JPEG, or implement a
//! restart loop. The owner must take the streams and explicitly terminate or
//! wait for the child; `Drop` remains a final best-effort reap guard.

use std::fmt;
use std::io;
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Stdio};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use crate::{FfmpegCommandSpec, FfmpegError, FfmpegStream, RtspError};

#[cfg(windows)]
/// Prevents a console window from flashing for a GUI-launched sidecar.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A directly spawned FFmpeg process with owned lifecycle and stdio handles.
pub struct FfmpegChild {
    child: Child,
    exit_status: Option<ExitStatus>,
}

impl FfmpegChild {
    /// Spawns exactly the executable and argv from `spec` without a shell.
    pub fn spawn(spec: &FfmpegCommandSpec) -> Result<Self, RtspError> {
        let mut command = Command::new(spec.executable());
        command
            .args(spec.argv())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        #[cfg(windows)]
        command.creation_flags(CREATE_NO_WINDOW);

        let child = command
            .spawn()
            .map_err(|error| FfmpegError::Spawn { kind: error.kind() })?;

        Ok(Self {
            child,
            exit_status: None,
        })
    }

    /// Returns the operating-system process identifier.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Takes FFmpeg stdout exactly once.
    pub fn take_stdout(&mut self) -> Result<ChildStdout, RtspError> {
        self.child.stdout.take().ok_or_else(|| {
            FfmpegError::StdioAlreadyTaken {
                stream: FfmpegStream::Stdout,
            }
            .into()
        })
    }

    /// Takes FFmpeg stderr exactly once.
    pub fn take_stderr(&mut self) -> Result<ChildStderr, RtspError> {
        self.child.stderr.take().ok_or_else(|| {
            FfmpegError::StdioAlreadyTaken {
                stream: FfmpegStream::Stderr,
            }
            .into()
        })
    }

    /// Checks whether the child has exited, reaping it when the OS reports an
    /// exit. A previously observed status is returned without touching the OS.
    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>, RtspError> {
        if let Some(status) = self.exit_status {
            return Ok(Some(status));
        }

        let status = self
            .child
            .try_wait()
            .map_err(|error| FfmpegError::TryWait { kind: error.kind() })?;
        if status.is_some() {
            self.exit_status = status;
        }
        Ok(status)
    }

    /// Waits for and reaps the child. Repeated calls are safe and return the
    /// cached exit status.
    pub fn wait(&mut self) -> Result<ExitStatus, RtspError> {
        if let Some(status) = self.exit_status {
            return Ok(status);
        }

        let status = self
            .child
            .wait()
            .map_err(|error| FfmpegError::Wait { kind: error.kind() })?;
        self.exit_status = Some(status);
        Ok(status)
    }

    /// Terminates the child if it is still running and always attempts to
    /// wait/reap it after a kill request. Already-exited children are handled
    /// idempotently.
    pub fn terminate(&mut self) -> Result<ExitStatus, RtspError> {
        if let Some(status) = self.exit_status {
            return Ok(status);
        }

        if let Some(status) = self
            .child
            .try_wait()
            .map_err(|error| FfmpegError::TryWait { kind: error.kind() })?
        {
            self.exit_status = Some(status);
            return Ok(status);
        }

        let kill_result = self.child.kill();
        let wait_result = self.child.wait();

        match (kill_result, wait_result) {
            (Ok(()), Ok(status)) => {
                self.exit_status = Some(status);
                Ok(status)
            }
            // A process can exit between try_wait and kill. The wait below
            // proves ownership was reaped, so this race is successful.
            (Err(error), Ok(status)) if error.kind() == io::ErrorKind::NotFound => {
                self.exit_status = Some(status);
                Ok(status)
            }
            (Err(error), Ok(status)) => {
                self.exit_status = Some(status);
                if error.kind() == io::ErrorKind::NotFound {
                    Ok(status)
                } else {
                    Err(FfmpegError::TerminateKill { kind: error.kind() }.into())
                }
            }
            (Ok(()), Err(error)) => Err(FfmpegError::Wait { kind: error.kind() }.into()),
            (Err(kill_error), Err(_wait_error)) => Err(FfmpegError::TerminateKill {
                kind: kill_error.kind(),
            }
            .into()),
        }
    }
}

impl fmt::Debug for FfmpegChild {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FfmpegChild")
            .field("pid", &self.child.id())
            .field("stdout_taken", &self.child.stdout.is_none())
            .field("stderr_taken", &self.child.stderr.is_none())
            .field("exit_status_known", &self.exit_status.is_some())
            .finish()
    }
}

impl Drop for FfmpegChild {
    fn drop(&mut self) {
        if self.exit_status.is_some() {
            return;
        }

        match self.child.try_wait() {
            Ok(Some(status)) => {
                self.exit_status = Some(status);
            }
            Ok(None) | Err(_) => {
                // Even when kill fails, wait is attempted so this owner does
                // not intentionally leave an unreaped child behind.
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::{
        FfmpegCommandSpec, FfmpegInputFile, RtspEndpoint, RtspErrorCategory, RtspErrorCode,
        RtspTransport,
    };

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn test_path(label: &str) -> PathBuf {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "banto-rtsp-process-{label}-{}-{id}",
            std::process::id()
        ))
    }

    fn spec(executable: PathBuf) -> (FfmpegCommandSpec, crate::FfmpegInputFile) {
        let input_path = test_path("input");
        let input = FfmpegInputFile::create_new(
            &input_path,
            &RtspEndpoint::new("rtsp://camera.example/live").unwrap(),
            None,
            RtspTransport::Tcp,
            std::time::Duration::from_secs(5),
        )
        .unwrap();
        let spec = FfmpegCommandSpec::new(executable, &input).unwrap();
        (spec, input)
    }

    #[test]
    fn missing_executable_is_launch_error_without_path_or_secret() {
        let missing = test_path("missing-camera-secret-password");
        let (spec, _input) = spec(missing.clone());
        let error = FfmpegChild::spawn(&spec).unwrap_err();

        assert_eq!(error.category(), RtspErrorCategory::Launch);
        assert_eq!(error.public_info().code, RtspErrorCode::SpawnFailed);
        let text = format!("{error:?} {error}");
        assert!(!text.contains(&missing.to_string_lossy().to_string()));
        assert!(!text.contains("secret-password"));
    }

    #[test]
    fn stdout_and_stderr_are_take_once_and_termination_is_idempotent() {
        let (spec, _input) = spec(std::env::current_exe().unwrap());
        let mut child = FfmpegChild::spawn(&spec).unwrap();
        let debug_before = format!("{child:?}");
        assert!(debug_before.contains("pid"));
        assert!(debug_before.contains("stdout_taken: false"));

        let _stdout = child.take_stdout().unwrap();
        assert_eq!(
            child.take_stdout().unwrap_err().public_info().code,
            RtspErrorCode::StdioAlreadyTaken
        );
        let _stderr = child.take_stderr().unwrap();
        assert_eq!(
            child.take_stderr().unwrap_err().public_info().code,
            RtspErrorCode::StdioAlreadyTaken
        );

        let first = child.terminate().unwrap();
        let second = child.terminate().unwrap();
        assert_eq!(first, second);
        assert_eq!(child.wait().unwrap(), first);
    }

    #[test]
    fn drop_reaps_a_running_direct_child_without_shell() {
        let (spec, _input) = spec(std::env::current_exe().unwrap());
        let child = FfmpegChild::spawn(&spec).unwrap();
        let _pid = child.pid();
        drop(child);
    }

    #[test]
    fn test_input_path_is_cleaned_after_process_test() {
        let (spec, input) = spec(std::env::current_exe().unwrap());
        let mut child = FfmpegChild::spawn(&spec).unwrap();
        let _ = child.terminate();
        let path = input.path().to_owned();
        drop(input);
        assert!(!fs::exists(path).unwrap());
    }
}
