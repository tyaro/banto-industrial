//! OS keyring storage for the desktop auto-login credential (spec M11).
//!
//! Deliberately its own tiny module rather than inline in `lib.rs`: this is
//! the one piece of `src-tauri` that talks to `keyring` directly, and
//! keeping it a thin, uniformly-erroring wrapper makes the "keyring backend
//! unavailable" degrade path (spec M11: "keyring 不在環境（一部Linux）で機能
//! が安全に degrade する") a single place to reason about instead of
//! scattered `keyring::Error` matches through the command handlers below.
//!
//! The credential is looked up by `(service, account)` where `service` is
//! fixed for this app and `account` is the account's `username` - the same
//! convention `keyring`'s own examples use, and one `Entry` per username
//! means a future "switch which account autologs in" never collides with a
//! previously-configured one still sitting in the OS store under its own
//! username.

use banto_core::BantoError;

/// Fixed keyring service name for every credential this app stores. Not
/// derived from `CARGO_PKG_NAME` on purpose: renaming the crate must not
/// silently orphan credentials users already saved in their OS keyring.
const SERVICE_NAME: &str = "dev.tyaro.relay-wright";

/// Turns any `keyring::Error` (backend missing, permission denied, no entry
/// found, ...) into a `BantoError::Other` with a Japanese message, so callers
/// never need to know `keyring`'s error type - this is the "safe degrade"
/// spec M11 asks for when a platform has no usable keyring backend (e.g. some
/// headless Linux setups without a secret-service provider).
fn degrade(context: &str, err: keyring::Error) -> BantoError {
    BantoError::Other(format!("{context}: {err}"))
}

fn entry(username: &str) -> Result<keyring::Entry, BantoError> {
    keyring::Entry::new(SERVICE_NAME, username)
        .map_err(|err| degrade("OSキーリードへのアクセスに失敗しました", err))
}

/// Store `password` in the OS keyring under `username`, overwriting any
/// existing entry for that username.
pub fn set_password(username: &str, password: &str) -> Result<(), BantoError> {
    entry(username)?
        .set_password(password)
        .map_err(|err| degrade("OSキーリングへの資格情報の保存に失敗しました", err))
}

/// Retrieve the password previously stored for `username`, or an error if
/// there is none (or the backend is unavailable).
pub fn get_password(username: &str) -> Result<String, BantoError> {
    entry(username)?
        .get_password()
        .map_err(|err| degrade("OSキーリングからの資格情報の取得に失敗しました", err))
}

/// Remove the stored credential for `username`. Idempotent-ish in intent
/// (callers here treat "already gone"/backend errors as best-effort - see
/// `autologin_disable` in `lib.rs`, which logs and proceeds rather than
/// failing the whole command on a keyring delete error).
pub fn delete_password(username: &str) -> Result<(), BantoError> {
    entry(username)?
        .delete_credential()
        .map_err(|err| degrade("OSキーリングからの資格情報の削除に失敗しました", err))
}
