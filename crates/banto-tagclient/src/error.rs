//! Stable, non-sensitive error classification for the public boundary.
//!
//! [`Error`] carries a stable [`ErrorKind`] and, only for a write the Hub
//! answered with an error body, an optional [`WriteErrorDetail`] (the Hub's
//! machine code, `audit_id`, and the caller's own `correlation_id` echoed
//! back). It never carries a URL, header, secret, or free-form server text.

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
    DuplicateRequestedExternalName,
    DuplicateCatalogExternalName,
    /// banto-hub rejected a subscribe with `{"op":"error","code":"unknown_tag"}`:
    /// a requested tag disappeared between the catalog fetch and the
    /// subscribe. Rebindable - the next catalog moves that name to
    /// `BindingResolution::unresolved` (#682,
    /// docs/crates/banto-tagclient-partial-binding-design.md §4.3).
    ///
    /// A name missing from the catalog (never created, deleted, or the old
    /// name after a rename - tag-server-design.md §4.1) is not an error by
    /// itself: it is listed in `BindingResolution::unresolved` and the other
    /// bindings still go `Live` (or, when nothing resolved, the worker
    /// settles in `Unbound`). Its value is never treated as current.
    BindingUnresolved,
    Unauthorized,
    Transport,
    ProtocolError,
    CatalogUnavailable,
    RevisionMismatch,
    RuntimeMetadataMismatch,
    Stopped,
    /// Write rejected with HTTP 403 (`not_writable` / `missing_write_scope` /
    /// `session_token_cannot_write`; a 403 `key_tripped` is
    /// [`Self::KeyTripped`], tag-server-design.md §6 gates 2/8). A configuration or credential problem, not a transient
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
    /// Any other write-time rejection banto-hub returned *before sending*
    /// anything to the device (404 `not_found`, 409 `tag_disabled`, 422
    /// `unsupported_value_type`/`value_out_of_range`, 429 `rate_limited`,
    /// 501 `write_unsupported_protocol`). Nothing was sent. The request as
    /// constructed cannot succeed; the caller must change the tag, value, or
    /// timing before trying again.
    WriteRejected,
    /// 502 `write_failed`: the Hub sent the write and the failure is
    /// confirmed; it had no effect on the device. Unlike
    /// [`Self::WriteRejected`] something was attempted (an `audit_id` is
    /// usually available from [`Error::write_detail`]); unlike
    /// [`Self::WriteUncertain`] the outcome is known.
    WriteFailed,
    /// The write may or may not have reached the device (502
    /// `write_uncertain` or an unknown 502, 500 `internal`, 504 or any other
    /// 5xx that cannot guarantee nothing was sent, or a failure after
    /// the request was sent such as a timeout). The caller must check the
    /// physical state before writing again; this crate never retries.
    WriteUncertain,
    /// The write options (`correlation_id` / `on_behalf_of`) do not satisfy the
    /// Hub's rules. Nothing was sent.
    InvalidWriteOptions,
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
            Self::DuplicateRequestedExternalName => "duplicate_requested_external_name",
            Self::DuplicateCatalogExternalName => "duplicate_catalog_external_name",
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
            Self::WriteFailed => "write_failed",
            Self::WriteUncertain => "write_uncertain",
            Self::InvalidWriteOptions => "invalid_write_options",
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

/// Extra, non-sensitive facts from a Hub write error body. Only built inside
/// this crate. `code` is restricted to `[a-z0-9_]{1,64}` (anything else is
/// dropped so a reverse proxy's free text never gets in), `correlation_id` is
/// kept only when it equals the value the caller sent, and `detail` is never
/// read.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct WriteErrorDetail {
    code: Option<String>,
    audit_id: Option<i64>,
    correlation_id: Option<String>,
}

impl WriteErrorDetail {
    pub(crate) fn new(
        code: Option<String>,
        audit_id: Option<i64>,
        correlation_id: Option<String>,
    ) -> Self {
        Self {
            code,
            audit_id,
            correlation_id,
        }
    }

    /// The Hub's `error` code (for example `not_writable`, `rate_limited`,
    /// `write_failed`), when it is a well-formed snake_case token.
    pub fn code(&self) -> Option<&str> {
        self.code.as_deref()
    }

    /// The audit row id, present when the Hub failed after writing the audit row.
    pub fn audit_id(&self) -> Option<i64> {
        self.audit_id
    }

    /// The `correlation_id` the Hub echoed, only if it matches what was sent.
    pub fn correlation_id(&self) -> Option<&str> {
        self.correlation_id.as_deref()
    }
}

/// Error carrying a stable category and, for write errors only, an optional
/// [`WriteErrorDetail`]. It deliberately has no source message, URL, response
/// body text, header, or secret field.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Error {
    kind: ErrorKind,
    detail: Option<Box<WriteErrorDetail>>,
}

impl Error {
    pub(crate) const fn new(kind: ErrorKind) -> Self {
        Self { kind, detail: None }
    }

    pub(crate) fn with_detail(mut self, detail: Option<WriteErrorDetail>) -> Self {
        self.detail = detail.map(Box::new);
        self
    }

    pub const fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// The Hub's code / `audit_id` / `correlation_id` for a write the Hub
    /// answered with an error body; `None` otherwise (including failures
    /// before or while sending).
    pub fn write_detail(&self) -> Option<&WriteErrorDetail> {
        self.detail.as_deref()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.kind.as_str())
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
