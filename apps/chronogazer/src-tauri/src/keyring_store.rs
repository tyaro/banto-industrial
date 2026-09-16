//! OS keyring storage for the desktop auto-login credential (spec M11) and,
//! since #332, for the banto-hub API key this installation issues itself.
//!
//! Deliberately its own tiny module rather than inline in `lib.rs`: this is
//! the one piece of `src-tauri` that talks to `keyring` directly, and
//! keeping it a thin, uniformly-erroring wrapper makes the "keyring backend
//! unavailable" degrade path (spec M11: "keyring 不在環境（一部Linux）で機能
//! が安全に degrade する") a single place to reason about instead of
//! scattered `keyring::Error` matches through the command handlers below.
//!
//! The credential is looked up by `(service, account)` where `service` is
//! fixed for this app. There are two kinds of account in the one service:
//!
//! * an account's `username` - the auto-login credential (spec M11), the
//!   same convention `keyring`'s own examples use. One `Entry` per username
//!   means a future "switch which account autologs in" never collides with a
//!   previously-configured one still sitting in the OS store.
//! * `hub:{host}:{port}:{installation_id}` - the banto-hub API key
//!   (#332, built by `banto_hub_bootstrap`). The `hub:` prefix cannot
//!   collide with a username (`UsersService` never accepts a `:`), and
//!   keying by host/port/installation means two Hubs, or two installations
//!   pointed at one Hub, never share a credential.

use banto_core::BantoError;
use banto_hub_bootstrap::error::{
    Error as BootstrapError, ErrorKind as BootstrapErrorKind, Result as BootstrapResult,
};
use banto_hub_bootstrap::KeyStore;

/// Fixed keyring service name for every credential this app stores. Not
/// derived from `CARGO_PKG_NAME` on purpose: renaming the crate must not
/// silently orphan credentials users already saved in their OS keyring.
const SERVICE_NAME: &str = "dev.tyaro.chronogazer";

/// Turns any `keyring::Error` (backend missing, permission denied, no entry
/// found, ...) into a `BantoError::Other` with a Japanese message, so callers
/// never need to know `keyring`'s error type - this is the "safe degrade"
/// spec M11 asks for when a platform has no usable keyring backend (e.g. some
/// headless Linux setups without a secret-service provider).
fn degrade(context: &str, err: keyring::Error) -> BantoError {
    BantoError::Other(format!("{context}: {err}"))
}

fn entry(account: &str) -> Result<keyring::Entry, BantoError> {
    keyring::Entry::new(SERVICE_NAME, account)
        .map_err(|err| degrade("OSキーリードへのアクセスに失敗しました", err))
}

/// Store `secret` under `account`, overwriting any existing entry.
pub fn set_secret(account: &str, secret: &str) -> Result<(), BantoError> {
    entry(account)?
        .set_password(secret)
        .map_err(|err| degrade("OSキーリングへの資格情報の保存に失敗しました", err))
}

/// Retrieve the secret stored for `account`.
///
/// "There is no entry" is `Ok(None)`, not an error - the distinction matters
/// to #332's bootstrap, which treats a missing key as "issue one" and a
/// broken backend as "fail closed". (The auto-login wrapper
/// [`get_password`] keeps its original error-on-missing behaviour so its
/// call sites in `lib.rs` are unchanged.)
pub fn get_secret(account: &str) -> Result<Option<String>, BantoError> {
    match entry(account)?.get_password() {
        Ok(secret) => Ok(Some(secret)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(err) => Err(degrade(
            "OSキーリングからの資格情報の取得に失敗しました",
            err,
        )),
    }
}

/// Remove the stored secret for `account`. A missing entry is `Ok(())`.
pub fn delete_secret(account: &str) -> Result<(), BantoError> {
    match entry(account)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(err) => Err(degrade(
            "OSキーリングからの資格情報の削除に失敗しました",
            err,
        )),
    }
}

/// Store `password` in the OS keyring under `username`, overwriting any
/// existing entry for that username.
pub fn set_password(username: &str, password: &str) -> Result<(), BantoError> {
    set_secret(username, password)
}

/// Retrieve the password previously stored for `username`, or an error if
/// there is none (or the backend is unavailable).
///
/// Unlike [`get_secret`], a missing entry is an `Err` here: `lib.rs`'s
/// auto-login path has always treated "nothing stored" as "fall back to the
/// login screen" via the error arm, and #332 did not change that.
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

/// #332: the OS keyring as `banto-hub-bootstrap`'s [`KeyStore`].
///
/// This is the only place the plaintext banto-hub API key is written. The
/// settings DB stores the `HubRecord` (endpoint, key id/name, keyring
/// account, tag selection) and never the key itself - see
/// `chronogazer_core::hub`.
#[derive(Debug, Default)]
pub struct KeyringKeyStore;

impl KeyringKeyStore {
    /// A keyring failure becomes `ErrorKind::KeyStore` carrying the Japanese
    /// reason. `BantoError::Other`'s `Display` is the message text only - it
    /// never contains the secret (that value is never passed to `degrade`).
    fn wrap(error: BantoError) -> BootstrapError {
        BootstrapError::with_detail(BootstrapErrorKind::KeyStore, error.to_string())
    }
}

impl KeyStore for KeyringKeyStore {
    fn get(&self, account: &str) -> BootstrapResult<Option<String>> {
        get_secret(account).map_err(Self::wrap)
    }

    fn set(&self, account: &str, secret: &str) -> BootstrapResult<()> {
        set_secret(account, secret).map_err(Self::wrap)
    }

    fn delete(&self, account: &str) -> BootstrapResult<()> {
        delete_secret(account).map_err(Self::wrap)
    }
}
