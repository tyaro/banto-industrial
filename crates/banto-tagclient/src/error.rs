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
    /// `collection_not_running`, tag-server-design.md §6 gate 5;
    /// `simulation_write_rejected` was retired by #363, 2026-09-15 - a write
    /// to a simulated device is applied to the simulator rather than
    /// refused). A transient server-side state; the
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
    /// #446: banto-hub closed the stream with close code 1008 and the reason
    /// `api_key_revoked` - an administrator revoked this key. Irreversible:
    /// only a different key helps, so the worker stops instead of
    /// reconnecting (see [`crate::close`]).
    KeyRevoked,
    /// #446: close 1008 `api_key_expired` - the key passed its
    /// `expires_at`. Irreversible for this key; the worker stops.
    KeyExpired,
    /// #446: close 1008 `api_key_tripped` - the key was tripped (for
    /// example by the write rate limit). **Reversible**: an administrator
    /// can clear the trip and the same key works again. The worker still
    /// stops (reconnecting immediately would be rejected); whether and how
    /// slowly to try again is the application's decision.
    KeyTripped,
    /// #446: close 1008 `api_key_not_found` - the Hub no longer knows this
    /// key (deleted, or the Hub's key table was replaced). The worker stops.
    KeyNotFound,
    /// #446: close 1008 with a reason this crate does not know (a newer Hub,
    /// or a session reason such as `session_revoked` that an API-key client
    /// should never receive). The Hub still said "this credential is no
    /// longer accepted", so the worker stops rather than reconnecting.
    CredentialRejected,
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
            Self::KeyRevoked => "key_revoked",
            Self::KeyExpired => "key_expired",
            Self::KeyTripped => "key_tripped",
            Self::KeyNotFound => "key_not_found",
            Self::CredentialRejected => "credential_rejected",
        }
    }

    /// #446: the Hub closed an open stream saying this credential is no
    /// longer accepted (close code 1008, classified by
    /// [`crate::close::classify_close`]). The worker publishes these as
    /// [`TagClientConnectionState::Unauthorized`](crate::TagClientConnectionState::Unauthorized)
    /// with the specific kind in `last_error` and does not reconnect.
    ///
    /// Plain [`Self::Unauthorized`] (a 401/403 on the catalog request or the
    /// handshake) is deliberately **not** included: it carries no reason.
    pub const fn is_credential_rejection(self) -> bool {
        matches!(
            self,
            Self::KeyRevoked
                | Self::KeyExpired
                | Self::KeyTripped
                | Self::KeyNotFound
                | Self::CredentialRejected
        )
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
