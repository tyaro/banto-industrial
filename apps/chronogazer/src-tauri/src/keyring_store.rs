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
//! fixed for this app and `account` is the auto-login account's `username` -
//! the same convention `keyring`'s own examples use. One `Entry` per username
//! means a future "switch which account autologs in" never collides with a
//! previously-configured one still sitting in the OS store.
//!
//! # Hub の API キーはここには無い（#392 B2-4b、決定 4）
//!
//! #332 から B2-4a までは、自己発行した banto-hub の API キーも同じ service に
//! `hub:{host}:{port}{path}:{installation_id}` という account で入れていた
//! （`KeyringKeyStore`）。B2-4b でデスクトップ host もデータのルート
//! （`%ProgramData%\ChronoGazer`）を使うようになり、Hub のキーは
//! `chronogazer-server` と同じ `<root>\secrets\hub-keys\` の DPAPI のファイル
//! （`chronogazer_core::key_store`、`crate::desktop_key_store`）へ移った。
//! キーリングに残っている `hub:` の項目は**移さない・読まない・消さない**
//! （旧データの DB と同じく H9 の扱い。新しいルートの DB には Hub の接続の記録が
//! 無いので使われない）。要らなければ利用者が Windows の資格情報マネージャー
//! （汎用資格情報のうち、名前に `hub:` と `dev.tyaro.chronogazer` を含むもの）から消せる。自動ログインの
//! パスワードは利用者ごとの秘密なので、キーリングのまま（決定 4）。

use banto_core::BantoError;

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
        .map_err(|err| degrade("OSキーリングへのアクセスに失敗しました", err))
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
///
/// A missing entry is an `Err` here: `lib.rs`'s auto-login path has always
/// treated "nothing stored" as "fall back to the login screen" via the error
/// arm.
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
