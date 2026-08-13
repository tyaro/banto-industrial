//! Public video state without transport- or credential-specific details.

use std::time::SystemTime;

use crate::{RtspError, RtspErrorInfo};

/// State of a future RTSP video supervisor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoState {
    Stopped,
    Connecting,
    Live,
    Reconnecting,
    Error,
}

/// Safe status snapshot for UI and service health surfaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoStatus {
    pub state: VideoState,
    pub last_frame_at: Option<SystemTime>,
    pub consecutive_failures: u32,
    pub error: Option<RtspErrorInfo>,
}

impl VideoStatus {
    pub const fn new() -> Self {
        Self {
            state: VideoState::Stopped,
            last_frame_at: None,
            consecutive_failures: 0,
            error: None,
        }
    }

    pub fn set_error(&mut self, error: RtspError) {
        self.state = VideoState::Error;
        self.error = Some(error.public_info());
    }
}

impl Default for VideoStatus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RtspConfigError, RtspErrorCode};

    #[test]
    fn status_exposes_only_structured_non_secret_error_info() {
        let mut status = VideoStatus::new();
        status.set_error(RtspError::Config(RtspConfigError::UserInfoNotAllowed));

        assert_eq!(status.state, VideoState::Error);
        assert_eq!(
            status.error,
            Some(RtspErrorInfo {
                category: crate::RtspErrorCategory::Config,
                code: RtspErrorCode::UserInfoNotAllowed,
            })
        );
        let debug = format!("{status:?}");
        assert!(!debug.contains("password"));
        assert!(!debug.contains("user"));
    }
}
