//! The six connection states the settings screen must be able to tell
//! apart, and the result type that carries one.
//!
//! The set is closed on purpose (issue #332's acceptance criteria): every
//! outcome of a bootstrap attempt maps onto exactly one of these, and an
//! error is **never** collapsed into an empty tag list. "Connected with zero
//! tags" ([`HubStatus::Connected`] with `tag_count: 0`) is a real,
//! distinguishable state - banto-hub answers `GET /api/v1/tags` with
//! `200 {"tags": []}` on a freshly installed Hub, and the operator must see
//! "接続済み・利用可能なタグなし" rather than a failure.

use banto_tagclient::CatalogSnapshot;
use serde::Serialize;

/// Why the Hub could not be reached or did not answer usefully. A small,
/// stable vocabulary - never a message built from a response body, a URL, or
/// an OS error string (same redaction contract as
/// `banto_tagclient::Error`).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum UnreachableCause {
    /// DNS, connect, TLS-less socket, or timeout failure: nothing answered.
    Transport,
    /// Something answered but the body was not the expected JSON.
    Protocol,
    /// The Hub answered with a non-2xx status that is not an
    /// authentication/authorization verdict (5xx, unexpected 4xx).
    ServerError,
    /// The Hub answered with a redirect, which this client never follows
    /// (the endpoint is pointed at the wrong thing).
    InvalidEndpoint,
}

impl UnreachableCause {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Transport => "transport",
            Self::Protocol => "protocol",
            Self::ServerError => "server_error",
            Self::InvalidEndpoint => "invalid_endpoint",
        }
    }
}

/// One of the six states of a Hub connection.
///
/// The `Serialize` form is the wire contract shared with the app's UI:
/// `{"state": "connected", "tagCount": 0}`,
/// `{"state": "unreachable", "cause": "transport"}`, and so on.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(
    tag = "state",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum HubStatus {
    /// No Hub has been configured yet (no saved record).
    NotConfigured,
    /// Authenticated and the catalog was read. `tag_count` may legitimately
    /// be `0`.
    Connected { tag_count: usize },
    /// The stored key was rejected (HTTP 401). Reconnecting re-issues one
    /// while the Hub is still in commissioning mode.
    AuthFailed,
    /// The stored key authenticated but carries no usable `read` scope
    /// (HTTP 403 without `key_tripped`).
    Forbidden,
    /// #446: the stored key is **tripped** - banto-hub answered
    /// `403 {"error": "key_tripped"}` (#435). Unlike [`Self::Forbidden`] and
    /// [`Self::AuthFailed`], the key itself is still good: an administrator
    /// clears the trip and **the same key** works again. Nothing in this
    /// crate discards, revokes, or re-issues a tripped key
    /// ([`Bootstrapper::connect`](crate::Bootstrapper::connect) stops here
    /// the way it stops at [`Self::Unreachable`]).
    KeyTripped,
    /// Nothing usable came back. See [`UnreachableCause`].
    Unreachable { cause: UnreachableCause },
    /// The Hub is already locked down and this installation has no usable
    /// key, so self-issuing is (correctly) impossible: an administrator has
    /// to hand over a key. Lockdown is never undone or bypassed to get out
    /// of this state.
    NeedsPairing,
}

impl HubStatus {
    /// Stable snake_case discriminant, for logs and tests.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::NotConfigured => "not_configured",
            Self::Connected { .. } => "connected",
            Self::AuthFailed => "auth_failed",
            Self::Forbidden => "forbidden",
            Self::KeyTripped => "key_tripped",
            Self::Unreachable { .. } => "unreachable",
            Self::NeedsPairing => "needs_pairing",
        }
    }

    pub const fn is_connected(&self) -> bool {
        matches!(self, Self::Connected { .. })
    }

    pub(crate) const fn unreachable(cause: UnreachableCause) -> Self {
        Self::Unreachable { cause }
    }
}

/// The outcome of a connect/adopt/refresh attempt: always a [`HubStatus`],
/// plus the catalog when (and only when) one was actually read.
///
/// `catalog` is `Some` exactly when `status` is
/// [`HubStatus::Connected`]; a failed read leaves it `None` rather than an
/// empty snapshot, so no caller can mistake "the request failed" for "the
/// Hub has no tags".
#[derive(Debug)]
pub struct HubConnection {
    pub status: HubStatus,
    pub catalog: Option<CatalogSnapshot>,
}

impl HubConnection {
    pub(crate) const fn failed(status: HubStatus) -> Self {
        Self {
            status,
            catalog: None,
        }
    }

    pub(crate) fn connected(catalog: CatalogSnapshot) -> Self {
        Self {
            status: HubStatus::Connected {
                tag_count: catalog.tags.len(),
            },
            catalog: Some(catalog),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_form_is_a_camel_case_discriminated_union() {
        assert_eq!(
            serde_json::to_string(&HubStatus::Connected { tag_count: 0 }).unwrap(),
            r#"{"state":"connected","tagCount":0}"#
        );
        assert_eq!(
            serde_json::to_string(&HubStatus::unreachable(UnreachableCause::Transport)).unwrap(),
            r#"{"state":"unreachable","cause":"transport"}"#
        );
        assert_eq!(
            serde_json::to_string(&HubStatus::NeedsPairing).unwrap(),
            r#"{"state":"needsPairing"}"#
        );
        assert_eq!(
            serde_json::to_string(&HubStatus::NotConfigured).unwrap(),
            r#"{"state":"notConfigured"}"#
        );
        assert_eq!(
            serde_json::to_string(&HubStatus::KeyTripped).unwrap(),
            r#"{"state":"keyTripped"}"#
        );
    }

    #[test]
    fn zero_tags_is_connected_not_a_failure() {
        let status = HubStatus::Connected { tag_count: 0 };
        assert!(status.is_connected());
        assert_eq!(status.as_str(), "connected");
    }
}
