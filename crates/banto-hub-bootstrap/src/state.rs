//! The two host-supplied boundaries: where the plaintext key lives
//! ([`KeyStore`]) and where the non-secret connection record lives
//! ([`BootstrapState`]).
//!
//! Both are traits for the same reason `banto-tagclient` keeps keyring and
//! Tauri outside its own boundary: an OS keyring backend, a SQLite settings
//! table, and a Tauri command layer are the *app's* concerns, not this
//! crate's. Both are deliberately **synchronous** - a keyring call is a
//! blocking OS call anyway, and an app whose settings store is async (like
//! chronogazer's `SettingsService`) hydrates a small in-memory mirror around
//! each bootstrap call rather than forcing an async, non-dyn-compatible
//! trait on every implementor.

use serde::{Deserialize, Serialize};

use crate::error::Result;

/// Everything about one Hub connection that is safe to persist in the app's
/// ordinary settings storage.
///
/// **There is deliberately no key field.** The plaintext API key exists in
/// exactly one place, the [`KeyStore`], and this record only carries the
/// `keyring_account` needed to find it again. A reviewer can confirm "no
/// plaintext in the settings DB" by reading this struct alone.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HubRecord {
    /// The Hub base URL exactly as the user entered it (`http://host:port`
    /// plus an optional path prefix).
    pub endpoint: String,
    /// This installation's stable id, generated once by the app (see
    /// [`Bootstrapper::new`](crate::Bootstrapper::new)). Part of both the
    /// issued key's name and the keyring account, so two installations
    /// pointed at the same Hub never fight over one credential.
    pub installation_id: String,
    /// The `id` banto-hub assigned to the key this installation issued.
    /// `None` for a key adopted manually (the Hub's issue response is the
    /// only place an id is handed out, and a pasted key does not come with
    /// one). This is the **only** handle used for revocation - a key is
    /// never looked up by name.
    pub key_id: Option<i64>,
    /// The name this installation asked for when issuing. Recorded for
    /// operator-facing display and for the Hub's audit trail only; never
    /// used to find a key to revoke.
    pub key_name: Option<String>,
    /// `hub:{host}:{port}:{installation_id}` - the [`KeyStore`] account the
    /// plaintext key is filed under.
    pub keyring_account: String,
    /// Tags the operator picked in the app. Kept here (rather than in a
    /// separate key) so "forget this Hub" is one [`BootstrapState::clear`].
    /// An empty selection is a legitimate, savable state.
    #[serde(default)]
    pub selected_tags: Vec<String>,
}

/// Where the plaintext API key is stored. Implemented by the app over its
/// OS keyring.
///
/// Implementations must never write the key anywhere else (log, settings
/// row, temp file) and must return
/// [`ErrorKind::KeyStore`](crate::ErrorKind::KeyStore) when the backend is
/// unusable, so the bootstrap fails closed instead of issuing a key it
/// cannot keep.
pub trait KeyStore: Send + Sync + 'static {
    /// The stored key for `account`, or `None` when there is no entry.
    /// A missing entry is `Ok(None)`, not an error.
    fn get(&self, account: &str) -> Result<Option<String>>;
    /// Store (or overwrite) `secret` under `account`.
    fn set(&self, account: &str, secret: &str) -> Result<()>;
    /// Remove `account`'s entry. Removing a missing entry is `Ok(())`.
    fn delete(&self, account: &str) -> Result<()>;
}

/// Where the non-secret [`HubRecord`] is persisted. Implemented by the app
/// over its ordinary settings storage.
pub trait BootstrapState: Send + Sync + 'static {
    fn load(&self) -> Result<Option<HubRecord>>;
    fn save(&self, record: &HubRecord) -> Result<()>;
    fn clear(&self) -> Result<()>;
}

/// Test/dev implementations: a `Mutex`-backed [`KeyStore`] and
/// [`BootstrapState`] pair with no OS or database behind them.
///
/// Public (not `#[cfg(test)]`) on purpose: the apps that embed this crate
/// use them in their own tests, exactly like `banto-tagclient`'s public
/// `test_support`-style helpers, and `banto-serve` (chronogazer's
/// Tauri-free dev/E2E vehicle) has no OS keyring to plug in.
pub mod memory {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::{BootstrapState, HubRecord, KeyStore};
    use crate::error::Result;

    /// An in-memory [`KeyStore`]. Contents are lost with the process.
    #[derive(Debug, Default)]
    pub struct MemoryKeyStore {
        entries: Mutex<HashMap<String, String>>,
    }

    impl MemoryKeyStore {
        pub fn new() -> Self {
            Self::default()
        }

        /// How many accounts currently hold a key - lets a test assert
        /// "the key was stored" / "disconnect wiped it" without exposing a
        /// getter that would hand the secret back out.
        pub fn len(&self) -> usize {
            self.entries.lock().expect("memory keystore poisoned").len()
        }

        pub fn is_empty(&self) -> bool {
            self.len() == 0
        }
    }

    impl KeyStore for MemoryKeyStore {
        fn get(&self, account: &str) -> Result<Option<String>> {
            Ok(self
                .entries
                .lock()
                .expect("memory keystore poisoned")
                .get(account)
                .cloned())
        }

        fn set(&self, account: &str, secret: &str) -> Result<()> {
            self.entries
                .lock()
                .expect("memory keystore poisoned")
                .insert(account.to_owned(), secret.to_owned());
            Ok(())
        }

        fn delete(&self, account: &str) -> Result<()> {
            self.entries
                .lock()
                .expect("memory keystore poisoned")
                .remove(account);
            Ok(())
        }
    }

    /// An in-memory [`BootstrapState`].
    #[derive(Debug, Default)]
    pub struct MemoryState {
        record: Mutex<Option<HubRecord>>,
    }

    impl MemoryState {
        pub fn new() -> Self {
            Self::default()
        }

        /// Seed a record as if a previous run had saved one.
        pub fn seeded(record: HubRecord) -> Self {
            Self {
                record: Mutex::new(Some(record)),
            }
        }
    }

    impl BootstrapState for MemoryState {
        fn load(&self) -> Result<Option<HubRecord>> {
            Ok(self.record.lock().expect("memory state poisoned").clone())
        }

        fn save(&self, record: &HubRecord) -> Result<()> {
            *self.record.lock().expect("memory state poisoned") = Some(record.clone());
            Ok(())
        }

        fn clear(&self) -> Result<()> {
            *self.record.lock().expect("memory state poisoned") = None;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::memory::{MemoryKeyStore, MemoryState};
    use super::*;

    #[test]
    fn record_json_never_carries_a_key_field() {
        let record = HubRecord {
            endpoint: "http://127.0.0.1:3100".to_owned(),
            installation_id: "abc".to_owned(),
            key_id: Some(7),
            key_name: Some("chronogazer-abc-1".to_owned()),
            keyring_account: "hub:127.0.0.1:3100:abc".to_owned(),
            selected_tags: vec!["line1.fast.temp01".to_owned()],
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(!json.contains("\"key\""));
        assert!(json.contains("\"keyId\":7"));
        assert_eq!(
            serde_json::from_str::<HubRecord>(&json).unwrap(),
            record,
            "the persisted form must round-trip"
        );
    }

    #[test]
    fn record_json_tolerates_a_missing_selection() {
        let record: HubRecord = serde_json::from_str(
            r#"{"endpoint":"http://h:1","installationId":"i","keyId":null,"keyName":null,"keyringAccount":"a"}"#,
        )
        .unwrap();
        assert!(record.selected_tags.is_empty());
    }

    #[test]
    fn memory_implementations_round_trip_and_clear() {
        let keys = MemoryKeyStore::new();
        assert!(keys.get("a").unwrap().is_none());
        keys.set("a", "secret").unwrap();
        assert_eq!(keys.get("a").unwrap().as_deref(), Some("secret"));
        assert_eq!(keys.len(), 1);
        keys.delete("a").unwrap();
        keys.delete("a").unwrap();
        assert!(keys.is_empty());

        let state = MemoryState::new();
        assert!(state.load().unwrap().is_none());
        let record = HubRecord {
            endpoint: "http://127.0.0.1:1".to_owned(),
            ..HubRecord::default()
        };
        state.save(&record).unwrap();
        assert_eq!(state.load().unwrap(), Some(record));
        state.clear().unwrap();
        assert!(state.load().unwrap().is_none());
    }
}
