//! RTSP endpoint and credential values, kept separate by construction.

use std::fmt;

use crate::error::{RtspConfigError, RtspError};

/// The two schemes supported by the first RTSP foundation slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RtspScheme {
    Rtsp,
    Rtsps,
}

/// A validated RTSP endpoint that never contains userinfo.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct RtspEndpoint {
    raw: String,
    scheme_separator: usize,
    scheme: RtspScheme,
    host: String,
    port: Option<u16>,
}

impl RtspEndpoint {
    /// Parses an endpoint and rejects credentials embedded in its authority.
    pub fn new(input: impl AsRef<str>) -> Result<Self, RtspError> {
        let input = input.as_ref();
        if input.bytes().any(|byte| byte.is_ascii_control()) {
            return Err(RtspConfigError::ControlCharacter.into());
        }

        let Some(scheme_separator) = input.find("://") else {
            return Err(RtspConfigError::InvalidScheme.into());
        };
        let scheme_text = &input[..scheme_separator];
        let scheme = if scheme_text.eq_ignore_ascii_case("rtsp") {
            RtspScheme::Rtsp
        } else if scheme_text.eq_ignore_ascii_case("rtsps") {
            RtspScheme::Rtsps
        } else {
            return Err(RtspConfigError::InvalidScheme.into());
        };

        let authority_start = scheme_separator + 3;
        let remainder = &input[authority_start..];
        let authority_end = remainder.find(['/', '?', '#']).unwrap_or(remainder.len());
        let authority = &remainder[..authority_end];
        if authority.is_empty() {
            return Err(RtspConfigError::EmptyHost.into());
        }
        if authority.contains('@') {
            return Err(RtspConfigError::UserInfoNotAllowed.into());
        }

        let (host, port) = parse_authority(authority)?;
        Ok(Self {
            raw: input.to_owned(),
            scheme_separator,
            scheme,
            host,
            port,
        })
    }

    pub const fn scheme(&self) -> RtspScheme {
        self.scheme
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub const fn port(&self) -> Option<u16> {
        self.port
    }

    /// Returns the validated endpoint without credentials.
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    pub(crate) const fn scheme_separator(&self) -> usize {
        self.scheme_separator
    }
}

impl TryFrom<&str> for RtspEndpoint {
    type Error = RtspError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl fmt::Debug for RtspEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RtspEndpoint")
            .field("scheme", &self.scheme)
            .field("host_present", &!self.host.is_empty())
            .field("port_present", &self.port.is_some())
            .finish()
    }
}

impl fmt::Display for RtspEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.raw)
    }
}

/// Credentials are separate from [`RtspEndpoint`] so they cannot be embedded
/// in endpoint logs or accidentally serialized as part of a URL.
#[derive(Clone, PartialEq, Eq)]
pub struct RtspCredentials {
    username: String,
    password: String,
}

impl RtspCredentials {
    pub fn new(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            username: username.into(),
            password: password.into(),
        }
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    pub fn password(&self) -> &str {
        &self.password
    }
}

impl fmt::Debug for RtspCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RtspCredentials(<redacted>)")
    }
}

fn parse_authority(authority: &str) -> Result<(String, Option<u16>), RtspError> {
    if authority.starts_with('[') {
        let Some(close) = authority.find(']') else {
            return Err(RtspConfigError::InvalidAuthority.into());
        };
        let host = &authority[1..close];
        if host.is_empty() {
            return Err(RtspConfigError::EmptyHost.into());
        }
        let suffix = &authority[close + 1..];
        let port = parse_port_suffix(suffix)?;
        return Ok((host.to_owned(), port));
    }

    if authority.contains(['[', ']']) {
        return Err(RtspConfigError::InvalidAuthority.into());
    }
    let mut parts = authority.split(':');
    let host = parts.next().unwrap_or_default();
    let port_text = parts.next();
    if parts.next().is_some() {
        return Err(RtspConfigError::InvalidAuthority.into());
    }
    if host.is_empty() {
        return Err(RtspConfigError::EmptyHost.into());
    }
    if host
        .chars()
        .any(|character| character.is_ascii_whitespace())
    {
        return Err(RtspConfigError::InvalidAuthority.into());
    }
    let port = port_text.map(parse_port).transpose()?;
    Ok((host.to_owned(), port))
}

fn parse_port_suffix(suffix: &str) -> Result<Option<u16>, RtspError> {
    if suffix.is_empty() {
        Ok(None)
    } else if let Some(port_text) = suffix.strip_prefix(':') {
        Ok(Some(parse_port(port_text)?))
    } else {
        Err(RtspConfigError::InvalidAuthority.into())
    }
}

fn parse_port(port_text: &str) -> Result<u16, RtspError> {
    if port_text.is_empty() {
        return Err(RtspConfigError::InvalidPort.into());
    }
    port_text
        .parse()
        .map_err(|_| RtspConfigError::InvalidPort.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_rtsp_and_rtsps() {
        let rtsp = RtspEndpoint::new("rtsp://camera.example/live").unwrap();
        let rtsps = RtspEndpoint::new("rtsps://camera.example:8554/live").unwrap();

        assert_eq!(rtsp.scheme(), RtspScheme::Rtsp);
        assert_eq!(rtsps.scheme(), RtspScheme::Rtsps);
        assert_eq!(rtsps.host(), "camera.example");
        assert_eq!(rtsps.port(), Some(8554));
    }

    #[test]
    fn rejects_invalid_scheme_userinfo_empty_host_and_controls() {
        assert!(matches!(
            RtspEndpoint::new("http://camera/live"),
            Err(RtspError::Config(RtspConfigError::InvalidScheme))
        ));
        assert!(matches!(
            RtspEndpoint::new("rtsp://user:password@camera/live"),
            Err(RtspError::Config(RtspConfigError::UserInfoNotAllowed))
        ));
        assert!(matches!(
            RtspEndpoint::new("rtsp:///live"),
            Err(RtspError::Config(RtspConfigError::EmptyHost))
        ));
        assert!(matches!(
            RtspEndpoint::new("rtsp://camera/\nstream"),
            Err(RtspError::Config(RtspConfigError::ControlCharacter))
        ));
    }

    #[test]
    fn credentials_and_config_debug_are_redacted() {
        let credentials = RtspCredentials::new("camera-operator", "super-secret-password");
        let debug = format!("{credentials:?}");

        assert!(!debug.contains("camera-operator"));
        assert!(!debug.contains("super-secret-password"));
    }
}
