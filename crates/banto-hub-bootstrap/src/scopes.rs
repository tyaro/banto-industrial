//! The issue whitelist: what this crate is allowed to ask banto-hub to mint.
//!
//! Issue #332 (2026-09-08 owner comment) turned "`admin` は原則として自動発行
//! しない" into a prohibition, because a self-issued `admin` key survives
//! lockdown through the MCP surface and would be a permanent bypass of it.
//! `write:*` is refused for the same reason in the other direction: nothing
//! that can move a PLC output may be handed out without a human deciding it.
//!
//! The rule is a **whitelist**, not a blacklist, so a future banto-hub scope
//! verb is refused by default until someone deliberately adds it here. It is
//! checked before any network call, so a refused request never reaches the
//! Hub at all.

use crate::error::{Error, ErrorKind, Result};

/// The only scopes this crate will ever ask banto-hub to issue.
///
/// v1 is exactly `read` (whole-catalog read). Narrowing to
/// `read:{connection}.{group}.*` later means adding those forms here
/// deliberately; `admin` and any `write:` form must never be added.
pub const ISSUABLE_SCOPES: &[&str] = &["read"];

/// The scopes [`Bootstrapper::connect`](crate::Bootstrapper::connect) asks
/// for.
pub const DEFAULT_SCOPES: &[&str] = &["read"];

/// Reject any scope outside [`ISSUABLE_SCOPES`] before a key is issued.
///
/// An empty request is refused too: banto-hub would mint a key that
/// `GET /api/v1/tags` then answers `403` for, which is a confusing state to
/// create on purpose.
pub fn validate_issue_scopes(scopes: &[&str]) -> Result<()> {
    if scopes.is_empty() {
        return Err(Error::with_detail(
            ErrorKind::ForbiddenScope,
            "スコープが空です",
        ));
    }
    for scope in scopes {
        if !ISSUABLE_SCOPES.contains(scope) {
            // The scope string is caller-supplied configuration, never a
            // secret, so echoing it back is safe and makes the refusal
            // actionable.
            return Err(Error::with_detail(ErrorKind::ForbiddenScope, *scope));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_is_never_issuable() {
        for scope in ["admin", "ADMIN", "admin:all", " admin"] {
            let error = validate_issue_scopes(&[scope]).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::ForbiddenScope);
        }
        // Also when smuggled in alongside an allowed scope.
        assert_eq!(
            validate_issue_scopes(&["read", "admin"])
                .unwrap_err()
                .kind(),
            ErrorKind::ForbiddenScope
        );
    }

    #[test]
    fn write_scopes_are_never_issuable() {
        for scope in [
            "write:line1.fast.temp01",
            "write:line1.fast.*",
            "write",
            "Write:line1.fast.temp01",
        ] {
            assert_eq!(
                validate_issue_scopes(&[scope]).unwrap_err().kind(),
                ErrorKind::ForbiddenScope,
                "{scope} must be refused"
            );
        }
    }

    #[test]
    fn only_the_whitelisted_read_scope_passes() {
        validate_issue_scopes(DEFAULT_SCOPES).unwrap();
        validate_issue_scopes(&["read"]).unwrap();
        // Narrower read forms are valid banto-hub scopes but are not on the
        // whitelist yet - fail closed rather than guess.
        assert_eq!(
            validate_issue_scopes(&["read:line1.fast.*"])
                .unwrap_err()
                .kind(),
            ErrorKind::ForbiddenScope
        );
        assert_eq!(
            validate_issue_scopes(&[]).unwrap_err().kind(),
            ErrorKind::ForbiddenScope
        );
    }
}
