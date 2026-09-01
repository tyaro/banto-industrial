//! Stable, non-sensitive error classification for the public boundary.

use std::fmt;

/// Machine-stable error category. Callers must match this enum rather than
/// parsing a human-readable error string.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ErrorKind {
    InvalidSecret,
    InvalidEndpoint,
    InvalidTagSelection,
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
    /// Write rejected with HTTP 403 (`not_writable` / `missing_write_scope` /
    /// `session_token_cannot_write` / `key_tripped`, tag-server-design.md §6
    /// gates 2/8). A configuration or credential problem, not a transient
    /// server state - retrying the same request will not help.
    WriteForbidden,
    /// Write rejected with HTTP 503 (`writes_disabled` /
    /// `collection_not_running` / `simulation_write_rejected`,
    /// tag-server-design.md §6 gate 5). A transient server-side state; the
    /// caller may choose to retry later, but this crate never does so
    /// automatically (2026-09-01 owner decision - see the `write` module doc).
    WriteUnavailable,
    /// Any other write-time rejection banto-hub returned as a stable error
    /// code (404 `not_found`, 409 `tag_disabled`, 422
    /// `unsupported_value_type`/`value_out_of_range`, 429 `rate_limited`,
    /// 501 `write_unsupported_protocol`, 502 `write_failed`). The request as
    /// constructed cannot succeed; the caller must change the tag, value, or
    /// timing before trying again.
    WriteRejected,
}

impl ErrorKind {
    /// Stable snake_case identifier suitable for telemetry or UI mapping.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidSecret => "invalid_secret",
            Self::InvalidEndpoint => "invalid_endpoint",
            Self::InvalidTagSelection => "invalid_tag_selection",
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
            Self::WriteForbidden => "write_forbidden",
            Self::WriteUnavailable => "write_unavailable",
            Self::WriteRejected => "write_rejected",
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
