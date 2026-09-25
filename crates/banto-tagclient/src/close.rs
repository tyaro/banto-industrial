//! #446: classification of the close frame banto-hub sends when it ends an
//! open stream.
//!
//! banto-hub re-validates the credential of every open stream (#430, 15 s)
//! and, once it has **confirmed** that the credential is no longer usable,
//! closes the WebSocket with close code [`CREDENTIAL_CLOSE_CODE`] (1008,
//! RFC 6455 "Policy Violation") and a reason string that says why
//! (`apps/banto-hub/core/src/stream.rs`, `api_key_recheck_verdict`, #439).
//!
//! Before #446 this crate discarded the close frame and treated the close as
//! an ordinary disconnect: it reconnected once, the catalog request was
//! rejected with 401/403, and the worker stopped as a bare
//! [`ErrorKind::Unauthorized`]. That cost one pointless reconnect and, more
//! importantly, lost *why* the key stopped working - an application could not
//! tell a tripped key (an administrator can clear the trip and the same key
//! works again) from a revoked or expired one (only a different key helps).
//!
//! [`classify_close`] is the one place that turns a close frame into an
//! [`ErrorKind`]:
//!
//! | close code | reason                          | kind                                  | worker            |
//! | ---------- | ------------------------------- | ------------------------------------- | ----------------- |
//! | 1008       | [`REASON_API_KEY_REVOKED`]      | [`ErrorKind::KeyRevoked`]             | stops             |
//! | 1008       | [`REASON_API_KEY_EXPIRED`]      | [`ErrorKind::KeyExpired`]             | stops             |
//! | 1008       | [`REASON_API_KEY_TRIPPED`]      | [`ErrorKind::KeyTripped`]             | stops             |
//! | 1008       | [`REASON_API_KEY_NOT_FOUND`]    | [`ErrorKind::KeyNotFound`]            | stops             |
//! | 1008       | anything else (or no reason)    | [`ErrorKind::CredentialRejected`]     | stops             |
//! | other      | any                             | [`ErrorKind::Transport`]              | reconnects        |
//! | no frame   | -                               | [`ErrorKind::Transport`]              | reconnects        |
//!
//! Every 1008 stops the worker: the Hub sends 1008 **only** after it has
//! confirmed that the credential is unusable, so reconnecting with the same
//! key can only be rejected again. The worker publishes the stop as
//! [`TagClientConnectionState::Unauthorized`](crate::TagClientConnectionState::Unauthorized)
//! with the specific kind in `last_error` - the state name stays the one
//! existing consumers already treat as "credential problem", and the kind
//! adds the reason. Whether to try the same key again later (sensible only
//! for a trip) is the application's decision, not this crate's.
//!
//! The reason strings are duplicated from banto-hub rather than shared (this
//! crate does not depend on the Hub). The unit test
//! `reason_strings_match_the_hub_source` reads the Hub's source and fails if
//! either side changes a spelling.
//!
//! The reason text is server-controlled: it is only compared, never logged or
//! stored.

use crate::error::ErrorKind;

/// Close code banto-hub uses when it has confirmed that the stream's
/// credential is no longer usable (`REVOKED_CLOSE_CODE` in
/// `apps/banto-hub/core/src/stream.rs`).
pub const CREDENTIAL_CLOSE_CODE: u16 = 1008;

/// Reason for a revoked API key.
pub const REASON_API_KEY_REVOKED: &str = "api_key_revoked";
/// Reason for an API key past its `expires_at`.
pub const REASON_API_KEY_EXPIRED: &str = "api_key_expired";
/// Reason for a tripped API key (reversible by an administrator).
pub const REASON_API_KEY_TRIPPED: &str = "api_key_tripped";
/// Reason for an API key the Hub no longer knows.
pub const REASON_API_KEY_NOT_FOUND: &str = "api_key_not_found";

/// Classify a close frame (pure function, table in the module doc).
///
/// `code` is `None` when the peer closed without a close frame payload.
/// Anything that is not a 1008 is an ordinary disconnect
/// ([`ErrorKind::Transport`]) and keeps the existing reconnect/backoff
/// behavior - including 1013 (banto-hub's backpressure close) and 1000/1001.
pub fn classify_close(code: Option<u16>, reason: &str) -> ErrorKind {
    if code != Some(CREDENTIAL_CLOSE_CODE) {
        return ErrorKind::Transport;
    }
    match reason {
        REASON_API_KEY_REVOKED => ErrorKind::KeyRevoked,
        REASON_API_KEY_EXPIRED => ErrorKind::KeyExpired,
        REASON_API_KEY_TRIPPED => ErrorKind::KeyTripped,
        REASON_API_KEY_NOT_FOUND => ErrorKind::KeyNotFound,
        _ => ErrorKind::CredentialRejected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_frames_are_classified_by_code_and_reason() {
        // (code, reason, expected kind, stops the worker)
        let table: &[(Option<u16>, &str, ErrorKind, bool)] = &[
            (Some(1008), "api_key_revoked", ErrorKind::KeyRevoked, true),
            (Some(1008), "api_key_expired", ErrorKind::KeyExpired, true),
            (Some(1008), "api_key_tripped", ErrorKind::KeyTripped, true),
            (
                Some(1008),
                "api_key_not_found",
                ErrorKind::KeyNotFound,
                true,
            ),
            // Unknown reasons still mean "this credential is not accepted".
            (Some(1008), "", ErrorKind::CredentialRejected, true),
            (
                Some(1008),
                "session_revoked",
                ErrorKind::CredentialRejected,
                true,
            ),
            (
                Some(1008),
                "API_KEY_REVOKED",
                ErrorKind::CredentialRejected,
                true,
            ),
            (
                Some(1008),
                "api_key_revoked ",
                ErrorKind::CredentialRejected,
                true,
            ),
            // Ordinary disconnects reconnect as before, whatever the reason.
            (None, "", ErrorKind::Transport, false),
            (Some(1000), "", ErrorKind::Transport, false),
            (Some(1001), "going away", ErrorKind::Transport, false),
            (Some(1011), "api_key_revoked", ErrorKind::Transport, false),
            (Some(1013), "backpressure", ErrorKind::Transport, false),
            (Some(1013), "api_key_tripped", ErrorKind::Transport, false),
        ];
        for &(code, reason, expected, stops) in table {
            let kind = classify_close(code, reason);
            assert_eq!(kind, expected, "code={code:?} reason={reason:?}");
            assert_eq!(
                kind.is_credential_rejection(),
                stops,
                "code={code:?} reason={reason:?}"
            );
        }
    }

    #[test]
    fn plain_unauthorized_is_not_a_credential_rejection() {
        // A 401/403 carries no reason; it keeps its existing meaning.
        assert!(!ErrorKind::Unauthorized.is_credential_rejection());
        assert!(!ErrorKind::Transport.is_credential_rejection());
    }

    #[test]
    fn classified_kinds_have_distinct_stable_names() {
        let names = [
            ErrorKind::KeyRevoked,
            ErrorKind::KeyExpired,
            ErrorKind::KeyTripped,
            ErrorKind::KeyNotFound,
            ErrorKind::CredentialRejected,
        ]
        .map(ErrorKind::as_str);
        assert_eq!(
            names,
            [
                "key_revoked",
                "key_expired",
                "key_tripped",
                "key_not_found",
                "credential_rejected"
            ]
        );
    }

    /// The reason strings are written twice (here and in banto-hub). Fix the
    /// spelling in both directions against the Hub's source: every reason the
    /// Hub's API-key re-validation can send must be one this crate
    /// classifies, and every reason this crate classifies must be one the Hub
    /// sends. Also fix the close code.
    #[test]
    fn reason_strings_match_the_hub_source() {
        const HUB_STREAM_RS: &str = include_str!("../../../apps/banto-hub/core/src/stream.rs");
        // Tolerate a CRLF checkout.
        let source = HUB_STREAM_RS.replace("\r\n", "\n");
        let start = source
            .find("pub fn api_key_recheck_verdict")
            .expect("banto-hub api_key_recheck_verdict moved; update this test");
        let body = &source[start..];
        let body = &body[..body.find("\n}\n").expect("end of api_key_recheck_verdict")];
        let mut hub_reasons = Vec::new();
        let mut rest = body;
        while let Some(at) = rest.find("reason: \"") {
            let after = &rest[at + "reason: \"".len()..];
            let end = after.find('"').unwrap();
            hub_reasons.push(&after[..end]);
            rest = &after[end..];
        }
        hub_reasons.sort_unstable();
        let mut ours = vec![
            REASON_API_KEY_REVOKED,
            REASON_API_KEY_EXPIRED,
            REASON_API_KEY_TRIPPED,
            REASON_API_KEY_NOT_FOUND,
        ];
        ours.sort_unstable();
        assert_eq!(hub_reasons, ours);

        assert!(
            source.contains(&format!(
                "pub const REVOKED_CLOSE_CODE: u16 = {CREDENTIAL_CLOSE_CODE};"
            )),
            "banto-hub REVOKED_CLOSE_CODE no longer matches CREDENTIAL_CLOSE_CODE"
        );
    }
}
