//! Pure RTSP configuration values.

use std::fmt;
use std::time::Duration;

use crate::{
    ReconnectPolicy, RtspConfigError, RtspCredentials, RtspEndpoint, RtspError, RtspTransport,
};

/// A socket I/O timeout already proven representable by FFmpeg's signed
/// microsecond option. Keeping the conversion here prevents each launch path
/// from making a different truncation or overflow decision.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct ValidatedIoTimeout {
    duration: Duration,
    microseconds: i64,
}

impl ValidatedIoTimeout {
    pub(crate) fn new(duration: Duration) -> Result<Self, RtspError> {
        let microseconds = duration.as_micros();
        if microseconds == 0 || microseconds > i64::MAX as u128 {
            return Err(RtspConfigError::InvalidIoTimeout.into());
        }

        Ok(Self {
            duration,
            microseconds: microseconds as i64,
        })
    }

    pub(crate) const fn duration(self) -> Duration {
        self.duration
    }

    pub(crate) const fn microseconds(self) -> i64 {
        self.microseconds
    }
}

/// Configuration shared by a future transport/supervisor layer.
#[derive(Clone, PartialEq, Eq)]
pub struct RtspConfig {
    endpoint: RtspEndpoint,
    credentials: Option<RtspCredentials>,
    transport: RtspTransport,
    io_timeout: ValidatedIoTimeout,
    reconnect_policy: ReconnectPolicy,
}

impl RtspConfig {
    pub fn new(
        endpoint: RtspEndpoint,
        credentials: Option<RtspCredentials>,
        transport: RtspTransport,
        io_timeout: Duration,
        reconnect_policy: ReconnectPolicy,
    ) -> Result<Self, RtspError> {
        Ok(Self {
            endpoint,
            credentials,
            transport,
            io_timeout: ValidatedIoTimeout::new(io_timeout)?,
            reconnect_policy,
        })
    }

    pub fn endpoint(&self) -> &RtspEndpoint {
        &self.endpoint
    }

    pub fn credentials(&self) -> Option<&RtspCredentials> {
        self.credentials.as_ref()
    }

    pub const fn transport(&self) -> RtspTransport {
        self.transport
    }

    /// Returns the mandatory finite socket I/O timeout used by FFmpeg's RTSP
    /// demuxer for each connection attempt.
    pub const fn io_timeout(&self) -> Duration {
        self.io_timeout.duration()
    }

    pub(crate) const fn validated_io_timeout(&self) -> ValidatedIoTimeout {
        self.io_timeout
    }

    pub fn reconnect_policy(&self) -> &ReconnectPolicy {
        &self.reconnect_policy
    }
}

impl fmt::Debug for RtspConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RtspConfig")
            .field("endpoint", &self.endpoint)
            .field("credentials", &self.credentials)
            .field("transport", &self.transport)
            .field("io_timeout_configured", &true)
            .field("reconnect_policy", &self.reconnect_policy)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn config_debug_does_not_expose_credentials() {
        let config = RtspConfig::new(
            RtspEndpoint::new("rtsp://camera.example/live").unwrap(),
            Some(RtspCredentials::new("operator", "password")),
            RtspTransport::Tcp,
            Duration::from_secs(5),
            ReconnectPolicy::new(Duration::from_millis(10), Duration::from_secs(1), 2).unwrap(),
        )
        .unwrap();
        let debug = format!("{config:?}");

        assert!(!debug.contains("operator"));
        assert!(!debug.contains("password"));
    }

    #[test]
    fn io_timeout_rejects_zero_sub_microsecond_and_signed_microsecond_overflow() {
        let reconnect =
            || ReconnectPolicy::new(Duration::from_millis(10), Duration::from_secs(1), 2).unwrap();
        let make = |timeout| {
            RtspConfig::new(
                RtspEndpoint::new("rtsp://camera.example/live").unwrap(),
                None,
                RtspTransport::Tcp,
                timeout,
                reconnect(),
            )
        };

        for timeout in [Duration::ZERO, Duration::from_nanos(999)] {
            let error = make(timeout).unwrap_err();
            assert_eq!(error, RtspConfigError::InvalidIoTimeout.into());
            assert_eq!(
                error.public_info().code,
                crate::RtspErrorCode::InvalidIoTimeout
            );
        }

        let overflow = Duration::from_micros(i64::MAX as u64)
            .checked_add(Duration::from_micros(1))
            .unwrap();
        assert_eq!(
            make(overflow).unwrap_err(),
            RtspConfigError::InvalidIoTimeout.into()
        );
    }

    #[test]
    fn io_timeout_accepts_exact_microseconds_and_hides_value_in_debug() {
        let timeout = Duration::from_micros(1_234_567);
        let config = RtspConfig::new(
            RtspEndpoint::new("rtsp://camera.example/live").unwrap(),
            None,
            RtspTransport::Udp,
            timeout,
            ReconnectPolicy::new(Duration::from_millis(10), Duration::from_secs(1), 2).unwrap(),
        )
        .unwrap();

        assert_eq!(config.io_timeout(), timeout);
        assert_eq!(config.validated_io_timeout().microseconds(), 1_234_567);
        assert!(!format!("{config:?}").contains("1234567"));
    }
}
