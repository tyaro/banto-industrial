//! Stable, non-sensitive error classification for the public boundary.
//!
//! Same contract as `banto_tagclient::error`: an [`Error`] carries a
//! machine-stable [`ErrorKind`] and, at most, a short human detail that the
//! caller supplied. The plaintext API key, the endpoint's path prefix, and
//! any response body are never part of an error's `Debug`/`Display`.
//!
//! Note the deliberate split between an `Err` and a
//! [`HubStatus`](crate::HubStatus): everything the *Hub* answered - including
//! "unauthorized", "forbidden", "unreachable", "locked down" - is a
//! **status**, not an error, because those are normal outcomes the settings
//! screen must render distinctly. An `Err` here means the bootstrap could
//! not even be attempted (bad endpoint, refused scope, unusable keyring or
//! settings store).

use std::fmt;

/// Machine-stable error category. Callers must match this enum rather than
/// parsing a human-readable error string.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ErrorKind {
    /// The supplied endpoint is not a plain-`http` origin (plus optional
    /// path prefix) - mirrors `banto_tagclient::ErrorKind::InvalidEndpoint`.
    InvalidEndpoint,
    /// A scope outside the issue whitelist was requested (`admin`, any
    /// `write:*`, or anything else not in [`ISSUABLE_SCOPES`]).
    ///
    /// [`ISSUABLE_SCOPES`]: crate::ISSUABLE_SCOPES
    ForbiddenScope,
    /// A key that cannot be sent as a bearer credential (empty, whitespace,
    /// non-ASCII) - only reachable through
    /// [`adopt_manual_key`](crate::Bootstrapper::adopt_manual_key) and a
    /// corrupted keyring entry.
    InvalidKey,
    /// The OS keyring (or whatever the app plugged into [`KeyStore`]) could
    /// not be read or written. The bootstrap fails closed here rather than
    /// keeping a plaintext key anywhere else.
    ///
    /// [`KeyStore`]: crate::KeyStore
    KeyStore,
    /// The app's own persisted bootstrap record could not be read or
    /// written.
    State,
    /// The operation needs a saved connection and there is none yet.
    NotConfigured,
}

impl ErrorKind {
    /// Stable snake_case identifier suitable for telemetry or UI mapping.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidEndpoint => "invalid_endpoint",
            Self::ForbiddenScope => "forbidden_scope",
            Self::InvalidKey => "invalid_key",
            Self::KeyStore => "key_store",
            Self::State => "state",
            Self::NotConfigured => "not_configured",
        }
    }
}

/// Error carrying a stable category plus, optionally, a caller-supplied
/// detail.
///
/// `detail` exists so a [`KeyStore`](crate::KeyStore) implementation can
/// surface *why* the OS keyring is unavailable (the settings screen shows
/// it). Implementations must never put a key, a bearer header, or an
/// endpoint path prefix in it - this crate itself only ever puts a rejected
/// scope string there.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Error {
    kind: ErrorKind,
    detail: Option<String>,
}

impl Error {
    pub const fn new(kind: ErrorKind) -> Self {
        Self { kind, detail: None }
    }

    pub fn with_detail(kind: ErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: Some(detail.into()),
        }
    }

    pub const fn kind(&self) -> ErrorKind {
        self.kind
    }

    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.detail {
            Some(detail) => write!(f, "{}: {detail}", self.kind.as_str()),
            None => f.write_str(self.kind.as_str()),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_is_the_stable_identifier_with_an_optional_detail() {
        assert_eq!(Error::new(ErrorKind::KeyStore).to_string(), "key_store");
        assert_eq!(
            Error::with_detail(ErrorKind::ForbiddenScope, "admin").to_string(),
            "forbidden_scope: admin"
        );
        assert_eq!(
            Error::with_detail(ErrorKind::ForbiddenScope, "admin").detail(),
            Some("admin")
        );
    }
}
