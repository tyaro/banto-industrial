//! Stable, non-sensitive error classification for the public boundary.

use std::fmt;

/// Machine-stable error category. Callers must match this enum rather than
/// parsing a human-readable error string.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ErrorKind {
    InvalidSecret,
    InvalidEndpoint,
    DuplicateBindingKey,
    DuplicateRequestedStableId,
    DuplicateCatalogStableId,
    BindingUnresolved,
    Unauthorized,
    Transport,
    ProtocolError,
    CatalogUnavailable,
    RevisionMismatch,
    RuntimeMetadataMismatch,
    Stopped,
}

impl ErrorKind {
    /// Stable snake_case identifier suitable for telemetry or UI mapping.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidSecret => "invalid_secret",
            Self::InvalidEndpoint => "invalid_endpoint",
            Self::DuplicateBindingKey => "duplicate_binding_key",
            Self::DuplicateRequestedStableId => "duplicate_requested_stable_id",
            Self::DuplicateCatalogStableId => "duplicate_catalog_stable_id",
            Self::BindingUnresolved => "binding_unresolved",
            Self::Unauthorized => "unauthorized",
            Self::Transport => "transport",
            Self::ProtocolError => "protocol_error",
            Self::CatalogUnavailable => "catalog_unavailable",
            Self::RevisionMismatch => "revision_mismatch",
            Self::RuntimeMetadataMismatch => "runtime_metadata_mismatch",
            Self::Stopped => "stopped",
        }
    }
}

/// Error carrying only a stable category. It deliberately has no source
/// message, URL, response body, header, or secret field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Error {
    kind: ErrorKind,
}

impl Error {
    pub(crate) const fn new(kind: ErrorKind) -> Self {
        Self { kind }
    }

    pub const fn kind(self) -> ErrorKind {
        self.kind
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.kind.as_str())
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
