//! [`Bootstrapper`]: the whole "zero-config" flow, app-independent.
//!
//! Reuse an already-issued key if there is one; otherwise ask the Hub
//! whether it is still in commissioning mode and, only then, mint exactly
//! one `read` key for this installation, hand the plaintext straight to the
//! [`KeyStore`], and prove the result by reading the catalog through
//! `banto-tagclient`.

use std::hash::{BuildHasher, RandomState};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use banto_tagclient::{Endpoint, ErrorKind as TagErrorKind, RestClient, SecretApiKey};
use reqwest::Url;
use zeroize::Zeroizing;

use crate::admin::{base_url, keyring_account, AdminClient, IssueOutcome, ProbedStatus};
use crate::error::{Error, ErrorKind, Result};
use crate::scopes::{validate_issue_scopes, DEFAULT_SCOPES};
use crate::state::{BootstrapState, HubRecord, KeyStore};
use crate::status::{CredentialIdentity, HubConnection, HubStatus, UnreachableCause};

/// One app's Hub bootstrap.
///
/// Cheap to build and safe to share (`Arc` inside); it holds no connection
/// and no credential of its own - the key always comes from, and goes back
/// to, the [`KeyStore`].
pub struct Bootstrapper {
    app_id: String,
    installation_id: String,
    keys: Arc<dyn KeyStore>,
    state: Arc<dyn BootstrapState>,
    /// Seed for [`CredentialIdentity`] (#446). Random per bootstrapper, so an
    /// identity means nothing outside this process.
    identity_seed: RandomState,
}

impl Bootstrapper {
    /// `app_id` is the short, stable app name that prefixes every key this
    /// installation issues (`chronogazer`, `relay-wright`, ...).
    ///
    /// `installation_id` is generated and persisted **by the app** (a UUID
    /// v4 or equivalent 128-bit random value): this crate has no storage of
    /// its own to keep it in, and the app already owns the settings store
    /// where it belongs. It appears in both the issued key's name (so the
    /// Hub's audit trail can tell two installations apart - the actor is
    /// always the synthetic `試運転モード` identity) and the keyring
    /// account.
    pub fn new(
        app_id: impl Into<String>,
        installation_id: impl Into<String>,
        keys: Arc<dyn KeyStore>,
        state: Arc<dyn BootstrapState>,
    ) -> Self {
        Self {
            app_id: app_id.into(),
            installation_id: installation_id.into(),
            keys,
            state,
            identity_seed: RandomState::new(),
        }
    }

    /// #446: the identity of the credential stored **now** (saved endpoint +
    /// the key in the [`KeyStore`]), or `None` when nothing usable is stored.
    /// Reads the record and the keyring only - no network. See
    /// [`CredentialIdentity`] for what it is (and is not).
    pub fn stored_credential(&self) -> Result<Option<CredentialIdentity>> {
        let Some(record) = self.state.load()? else {
            return Ok(None);
        };
        let Some(stored) = self.stored_key(&record.keyring_account)? else {
            return Ok(None);
        };
        Ok(Some(self.identity(&record.endpoint, &stored)))
    }

    fn identity(&self, endpoint: &str, key: &str) -> CredentialIdentity {
        CredentialIdentity(self.identity_seed.hash_one((endpoint, key)))
    }

    /// Connect to `endpoint`, issuing a `read` key if this installation does
    /// not already have a working one.
    ///
    /// Reusing an existing key is the first thing tried, so restarting the
    /// app does not mint a key per launch. A key is only ever issued when
    /// the Hub reports it is still in commissioning mode; a locked-down Hub
    /// yields [`HubStatus::NeedsPairing`] and nothing else is attempted.
    ///
    /// Pointing this at a different `endpoint` than the saved one starts from
    /// scratch for that Hub: nothing from the old record (`key_id`,
    /// `key_name`, tag selection) is carried over, and nothing is revoked on
    /// either side - see [`previous_for`](Self::previous_for).
    pub async fn connect(&self, endpoint: &str) -> Result<HubConnection> {
        self.connect_with_scopes(endpoint, DEFAULT_SCOPES).await
    }

    /// [`connect`](Self::connect) with an explicit scope request.
    ///
    /// The scopes pass through [`validate_issue_scopes`] **before** any
    /// network call, so there is no code path in this crate that can issue
    /// an `admin` or `write:` key - not even by mistake in a future caller.
    pub async fn connect_with_scopes(
        &self,
        endpoint: &str,
        scopes: &[&str],
    ) -> Result<HubConnection> {
        validate_issue_scopes(scopes)?;
        let admin = AdminClient::new(base_url(endpoint)?)?;
        // Only a record for THIS endpoint may contribute a `key_id` (see
        // `previous_for`): ids are per-Hub row ids, so carrying one across a
        // switch would revoke an unrelated key on the new Hub.
        let previous = self.previous_for(admin.base())?;
        let account = self.account_for(admin.base(), previous.as_ref());

        // 1. Reuse. A stored key that still authenticates means no issue
        //    request at all.
        if let Some(stored) = self.stored_key(&account)? {
            let connection = self.verify(&admin, endpoint, &stored).await?;
            match connection.status {
                HubStatus::Connected { .. } => {
                    self.save_record(
                        endpoint,
                        &account,
                        previous.as_ref(),
                        previous.as_ref().and_then(|record| record.key_id),
                        previous.as_ref().and_then(|record| record.key_name.clone()),
                    )?;
                    return Ok(connection);
                }
                // Nothing answered: do not escalate to issuing a second key
                // against a Hub we cannot even talk to.
                HubStatus::Unreachable { .. } => return Ok(connection),
                // #446: a tripped key is still this installation's key - an
                // administrator clears the trip and it works again. Revoking
                // it and issuing another would throw away exactly what the
                // screen tells the operator to keep.
                HubStatus::KeyTripped => return Ok(connection),
                // The stored key is invalid or unusable - fall through and
                // re-issue (that is what "再接続で再発行" means on screen).
                _ => {}
            }
        }

        // 2. Commissioning gate. An unknown answer is never treated as
        //    "not locked down".
        match admin.locked_down().await {
            Ok(true) => return Ok(HubConnection::failed(HubStatus::NeedsPairing)),
            Ok(false) => {}
            Err(cause) => return Ok(HubConnection::failed(HubStatus::unreachable(cause))),
        }

        // 3. Revoke the key this installation issued last time **against this
        //    same Hub**, if any. Best effort, and only ever by id (see
        //    `AdminClient::revoke` and `previous_for`).
        if let Some(previous_id) = previous.as_ref().and_then(|record| record.key_id) {
            if !admin.revoke(previous_id).await {
                tracing::info!(
                    "the previously issued banto-hub API key could not be revoked; continuing"
                );
            }
        }

        // 4. Issue, retrying once with a later timestamp if the name clashed.
        let issued = match self.issue_with_retry(&admin, scopes).await {
            IssueResult::Issued(issued) => issued,
            IssueResult::NeedsPairing => return Ok(HubConnection::failed(HubStatus::NeedsPairing)),
            IssueResult::Failed(cause) => {
                return Ok(HubConnection::failed(HubStatus::unreachable(cause)))
            }
        };

        // 5. The plaintext goes straight to the keyring. If it cannot be
        //    kept, the key is revoked again rather than left dangling on the
        //    Hub with nobody holding it.
        if let Err(error) = self.keys.set(&account, &issued.key) {
            admin.revoke(issued.id).await;
            return Err(error);
        }

        // 6. Persist the non-secret record.
        self.save_record(
            endpoint,
            &account,
            previous.as_ref(),
            Some(issued.id),
            Some(issued.name.clone()),
        )?;

        // 7. Prove it. Reachability alone is not "authenticated".
        self.verify(&admin, endpoint, &issued.key).await
    }

    /// Adopt a key an administrator issued by hand - the way out of
    /// [`HubStatus::NeedsPairing`] on an already locked-down Hub.
    ///
    /// The key is checked against the catalog first and only stored once it
    /// works, so a typo never replaces a working credential. `key_id` stays
    /// `None`: a pasted key does not come with the Hub-assigned id, and this
    /// crate refuses to go looking for one by name.
    pub async fn adopt_manual_key(&self, endpoint: &str, key: String) -> Result<HubConnection> {
        let admin = AdminClient::new(base_url(endpoint)?)?;
        let previous = self.previous_for(admin.base())?;
        let account = self.account_for(admin.base(), previous.as_ref());
        let key = Zeroizing::new(key);
        let connection = self.verify(&admin, endpoint, &key).await?;
        if !connection.status.is_connected() {
            return Ok(connection);
        }
        self.keys.set(&account, &key)?;
        self.save_record(endpoint, &account, previous.as_ref(), None, None)?;
        Ok(connection)
    }

    /// Check the saved connection without issuing anything.
    ///
    /// Distinct from [`connect`](Self::connect) on purpose: opening the
    /// settings screen must never mint a key as a side effect.
    pub async fn status(&self) -> Result<HubStatus> {
        let Some(record) = self.state.load()? else {
            return Ok(HubStatus::NotConfigured);
        };
        let admin = AdminClient::new(base_url(&record.endpoint)?)?;
        let Some(stored) = self.stored_key(&record.keyring_account)? else {
            // The record survived but the keyring entry did not (a new
            // Windows user, a cleared store, a reinstall). Reconnecting
            // re-issues while the Hub is still in commissioning mode, which
            // is exactly what `AuthFailed` tells the operator to do.
            return Ok(HubStatus::AuthFailed);
        };
        Ok(self.verify(&admin, &record.endpoint, &stored).await?.status)
    }

    /// Re-read the catalog with the saved connection. Never issues.
    ///
    /// Returns the full [`HubConnection`] rather than a bare
    /// [`CatalogSnapshot`] so a refresh that fails can still be shown as one
    /// of the six states instead of a generic error - the same reason
    /// `catalog` is `None` (not an empty snapshot) on failure.
    pub async fn refresh_catalog(&self) -> Result<HubConnection> {
        let Some(record) = self.state.load()? else {
            return Err(Error::new(ErrorKind::NotConfigured));
        };
        let admin = AdminClient::new(base_url(&record.endpoint)?)?;
        let Some(stored) = self.stored_key(&record.keyring_account)? else {
            return Ok(HubConnection::failed(HubStatus::AuthFailed));
        };
        self.verify(&admin, &record.endpoint, &stored).await
    }

    /// The saved record, if any (endpoint, key name, tag selection).
    pub fn record(&self) -> Result<Option<HubRecord>> {
        self.state.load()
    }

    /// An authenticated [`RestClient`] built from the saved endpoint and the
    /// key kept in the [`KeyStore`].
    ///
    /// `Ok(None)` means "no client can be built": either nothing is
    /// configured yet, or the keyring holds no entry for the saved account
    /// (a cleared store, a new Windows user, or a runtime with no keyring at
    /// all such as `banto-serve`'s `UnavailableKeyStore`). It is deliberately
    /// **not** an error and carries no verdict about the Hub: telling
    /// `AuthFailed` apart from `Forbidden` is [`status`](Self::status)'s job,
    /// and duplicating that classification here would give callers a second,
    /// divergent source of truth for the six states.
    ///
    /// The returned client is meant to be handed to
    /// [`RestClient::start`](banto_tagclient::RestClient::start) so the
    /// application can own one subscription generation. The `Bootstrapper`
    /// does **not** own that generation - starting, fingerprinting and
    /// shutting it down is the application's work (see
    /// `chronogazer_core::hub`'s `reconcile`).
    ///
    /// The plaintext key never leaves this crate: it lives in a
    /// `Zeroizing<String>` just long enough to move into the opaque
    /// [`SecretApiKey`], and there is deliberately no accessor that hands a
    /// `String` back out.
    pub fn rest_client(&self) -> Result<Option<RestClient>> {
        let Some(record) = self.state.load()? else {
            return Ok(None);
        };
        let Some(stored) = self.stored_key(&record.keyring_account)? else {
            return Ok(None);
        };
        Ok(Some(rest_client_for(&record.endpoint, &stored)?))
    }

    /// Persist the operator's tag selection. An empty selection is valid.
    pub fn set_selected_tags(&self, tags: Vec<String>) -> Result<()> {
        let mut record = self
            .state
            .load()?
            .ok_or_else(|| Error::new(ErrorKind::NotConfigured))?;
        record.selected_tags = tags;
        self.state.save(&record)
    }

    /// Forget this Hub locally: drop the stored key and the saved record.
    ///
    /// The Hub-side key is deliberately **not** revoked. Revocation is an
    /// administrator's decision about a shared server, and "I removed this
    /// app's local configuration" is not that decision; the key remains
    /// visible and revocable in the Hub's own API-key screen.
    pub fn disconnect(&self) -> Result<()> {
        if let Some(record) = self.state.load()? {
            // Best effort: a keyring that is already gone must not stop the
            // local configuration from being cleared.
            if let Err(error) = self.keys.delete(&record.keyring_account) {
                tracing::info!(
                    kind = error.kind().as_str(),
                    "the stored banto-hub key could not be deleted; clearing the record anyway"
                );
            }
        }
        self.state.clear()
    }

    fn stored_key(&self, account: &str) -> Result<Option<Zeroizing<String>>> {
        Ok(self.keys.get(account)?.map(Zeroizing::new))
    }

    /// The saved record, but **only if it belongs to `base`**.
    ///
    /// `key_id`, `key_name` and `keyring_account` are properties of one
    /// (endpoint, installation) pair, not of the installation alone: a
    /// banto-hub `api_keys.id` is a row id in *that* Hub's database, so two
    /// Hubs hand out the same small integers to completely unrelated keys.
    /// Reading a `key_id` saved against Hub A and revoking it on Hub B would
    /// take away a stranger's credential - exactly the failure mode the
    /// "revoke only ids we issued ourselves" rule exists to prevent. The
    /// same reasoning applies to reusing A's `key_name` in B's record.
    ///
    /// Endpoints are compared **normalized** (`base_url`, the same
    /// normalization `banto_tagclient::Endpoint::new` performs), so
    /// `http://host:3100`, `http://host:3100/` and a re-typed equivalent all
    /// count as the same Hub. A saved endpoint that no longer parses is
    /// treated as "not this one".
    ///
    /// `installation_id` is deliberately NOT endpoint-scoped: it identifies
    /// this installation, and stays the same wherever it connects.
    ///
    /// v1 keeps exactly one record, so switching to another Hub **forgets**
    /// the previous endpoint's `key_id`. Its key stays on that Hub and in the
    /// OS keyring under that Hub's own account; coming back later reuses the
    /// keyring entry if it still works, and otherwise issues a fresh key
    /// under a new name. Leaving the old key alone is the same principle as
    /// never touching another installation's key.
    /// Which [`KeyStore`] account this Hub's key is filed under.
    ///
    /// A record for this same endpoint is authoritative: it names the account
    /// the key was actually written to, which may have been spelled by an
    /// earlier version of [`keyring_account`] (the path prefix was added
    /// later). Honouring it means an upgrade does not orphan a working key
    /// and silently re-issue. Only a Hub with no record yet gets the current
    /// spelling.
    ///
    /// [`KeyStore`]: crate::KeyStore
    fn account_for(&self, base: &Url, previous: Option<&HubRecord>) -> String {
        previous
            .map(|record| record.keyring_account.clone())
            .unwrap_or_else(|| keyring_account(base, &self.installation_id))
    }

    fn previous_for(&self, base: &Url) -> Result<Option<HubRecord>> {
        Ok(self.state.load()?.filter(|record| {
            base_url(&record.endpoint).is_ok_and(|previous_base| &previous_base == base)
        }))
    }

    /// `{app_id}-{installation_id}-{unix seconds}`.
    ///
    /// The timestamp is what makes a re-bootstrap after keyring loss work:
    /// banto-hub's `api_keys.name` is UNIQUE and the plaintext of the old
    /// key is unrecoverable, so a fixed name would collide forever.
    fn key_name(&self, issued_at: u64) -> String {
        format!("{}-{}-{issued_at}", self.app_id, self.installation_id)
    }

    async fn issue_with_retry(&self, admin: &AdminClient, scopes: &[&str]) -> IssueResult {
        let issued_at = unix_seconds();
        match admin.issue(&self.key_name(issued_at), scopes).await {
            IssueOutcome::Issued(issued) => IssueResult::Issued(issued),
            IssueOutcome::Unauthenticated => IssueResult::NeedsPairing,
            IssueOutcome::Failed(cause) => IssueResult::Failed(cause),
            // A same-second re-bootstrap is the only realistic way to hit
            // this; one bumped-timestamp retry covers it without a loop
            // that could hammer the Hub.
            IssueOutcome::DuplicateName => {
                match admin.issue(&self.key_name(issued_at + 1), scopes).await {
                    IssueOutcome::Issued(issued) => IssueResult::Issued(issued),
                    IssueOutcome::Unauthenticated => IssueResult::NeedsPairing,
                    IssueOutcome::DuplicateName => {
                        IssueResult::Failed(UnreachableCause::ServerError)
                    }
                    IssueOutcome::Failed(cause) => IssueResult::Failed(cause),
                }
            }
        }
    }

    /// Read `GET /api/v1/tags` through `banto-tagclient` and classify the
    /// outcome into one of the six states.
    ///
    /// A `200` with zero tags is [`HubStatus::Connected`] with
    /// `tag_count: 0` - never an error, never an empty list standing in for
    /// one.
    async fn verify(
        &self,
        admin: &AdminClient,
        endpoint: &str,
        key: &Zeroizing<String>,
    ) -> Result<HubConnection> {
        let client = rest_client_for(endpoint, key)?;
        let judged = self.identity(endpoint, key);

        let connection = match client.fetch_catalog().await {
            Ok(catalog) => HubConnection::connected(catalog),
            Err(error) => HubConnection::failed(match error.kind() {
                // `banto-tagclient` collapses 401 and 403 into one kind;
                // the split the UI needs is recovered with a status-only
                // probe (see `AdminClient::probe_tags_status`).
                TagErrorKind::Unauthorized => rejection_status(admin.probe_tags_status(key).await),
                TagErrorKind::CatalogUnavailable => {
                    HubStatus::unreachable(UnreachableCause::ServerError)
                }
                TagErrorKind::ProtocolError => HubStatus::unreachable(UnreachableCause::Protocol),
                TagErrorKind::InvalidEndpoint => {
                    HubStatus::unreachable(UnreachableCause::InvalidEndpoint)
                }
                _ => HubStatus::unreachable(UnreachableCause::Transport),
            }),
        };
        // #446: the verdict is about exactly this (endpoint, key) - which may
        // or may not be the stored one (see `HubConnection::judged`).
        Ok(connection.judging(judged))
    }

    fn save_record(
        &self,
        endpoint: &str,
        account: &str,
        previous: Option<&HubRecord>,
        key_id: Option<i64>,
        key_name: Option<String>,
    ) -> Result<()> {
        // The tag selection is the operator's, not the bootstrap's: carry it
        // across a re-issue against the same Hub. `previous` has already been
        // narrowed to this endpoint by `previous_for`, so a switch to another
        // Hub starts with an empty selection (the names would refer to a
        // different catalog) and with no `key_id`/`key_name` carried over.
        let selected_tags = previous
            .map(|record| record.selected_tags.clone())
            .unwrap_or_default();
        self.state.save(&HubRecord {
            endpoint: endpoint.to_owned(),
            installation_id: self.installation_id.clone(),
            key_id,
            key_name,
            keyring_account: account.to_owned(),
            selected_tags,
        })
    }
}

enum IssueResult {
    Issued(crate::admin::IssuedKey),
    NeedsPairing,
    Failed(UnreachableCause),
}

/// The one place a [`RestClient`] is assembled: shared by
/// [`Bootstrapper::verify`] (which proves a key by reading the catalog) and
/// [`Bootstrapper::rest_client`] (which hands the same client to the
/// application for a subscription), so the two can never drift apart in how
/// the endpoint is normalized or the key is wrapped.
/// Which state a catalog rejection (`banto-tagclient`'s single
/// `Unauthorized` for 401 and 403) becomes, from the status-only probe
/// (pure, table-tested).
///
/// | probe | state |
/// | --- | --- |
/// | `403` + `{"error": "key_tripped"}` | [`HubStatus::KeyTripped`] (#446: the same key works again once an administrator clears the trip) |
/// | `403` otherwise | [`HubStatus::Forbidden`] (authenticates, no read scope) |
/// | `401`, anything else, or no answer | [`HubStatus::AuthFailed`] |
///
/// Revoked, expired, and unknown keys all answer the same `401` on REST
/// (banto-hub does not tell them apart there, by design), so they stay one
/// state; the reason only reaches a client through the stream's close frame
/// (`banto_tagclient::close`).
fn rejection_status(probe: Option<ProbedStatus>) -> HubStatus {
    match probe {
        Some(ProbedStatus {
            status: 403,
            key_tripped: true,
        }) => HubStatus::KeyTripped,
        Some(ProbedStatus { status: 403, .. }) => HubStatus::Forbidden,
        _ => HubStatus::AuthFailed,
    }
}

fn rest_client_for(endpoint: &str, key: &Zeroizing<String>) -> Result<RestClient> {
    let tag_endpoint =
        Endpoint::new(endpoint).map_err(|_| Error::new(ErrorKind::InvalidEndpoint))?;
    let secret =
        SecretApiKey::new(key.to_string()).map_err(|_| Error::new(ErrorKind::InvalidKey))?;
    RestClient::new(tag_endpoint, secret).map_err(|_| Error::new(ErrorKind::InvalidEndpoint))
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::state::memory::{MemoryKeyStore, MemoryState};
    use crate::test_support::{
        catalog_body, commissioning_body, duplicate_name_body, issued_body, MockHub, Reply,
    };

    const APP_ID: &str = "chronogazer";
    const INSTALLATION: &str = "11111111-2222-4333-8444-555555555555";
    const STATUS_ROUTE: &str = "GET /api/commissioning/status";
    const ISSUE_ROUTE: &str = "POST /api/api-keys";
    const TAGS_ROUTE: &str = "GET /api/v1/tags";

    fn key() -> String {
        ["bh", "_", "abcd1234", "_", "opaque-secret"].concat()
    }

    fn routes(pairs: Vec<(&str, Vec<Reply>)>) -> HashMap<String, Vec<Reply>> {
        pairs
            .into_iter()
            .map(|(route, replies)| (route.to_owned(), replies))
            .collect()
    }

    fn harness(keys: Arc<MemoryKeyStore>, state: Arc<MemoryState>) -> Bootstrapper {
        Bootstrapper::new(APP_ID, INSTALLATION, keys, state)
    }

    fn account_for(hub: &MockHub) -> String {
        keyring_account(&base_url(&hub.endpoint()).unwrap(), INSTALLATION)
    }

    fn seeded_record(hub: &MockHub, key_id: Option<i64>) -> HubRecord {
        HubRecord {
            endpoint: hub.endpoint(),
            installation_id: INSTALLATION.to_owned(),
            key_id,
            key_name: Some(format!("{APP_ID}-{INSTALLATION}-1000")),
            keyring_account: account_for(hub),
            selected_tags: vec!["line1.fast.tag0".to_owned()],
        }
    }

    #[tokio::test]
    async fn a_stored_key_is_reused_without_issuing_anything() {
        let hub = MockHub::start(routes(vec![(TAGS_ROUTE, vec![(200, catalog_body(2))])]));
        let keys = Arc::new(MemoryKeyStore::new());
        keys.set(&account_for(&hub), &key()).unwrap();
        let state = Arc::new(MemoryState::seeded(seeded_record(&hub, Some(7))));
        let bootstrapper = harness(Arc::clone(&keys), Arc::clone(&state));

        let connection = bootstrapper.connect(&hub.endpoint()).await.unwrap();

        assert_eq!(connection.status, HubStatus::Connected { tag_count: 2 });
        assert_eq!(connection.catalog.unwrap().tags.len(), 2);
        assert_eq!(hub.hit_count(ISSUE_ROUTE), 0, "no key may be issued");
        assert_eq!(hub.hit_count(STATUS_ROUTE), 0);
        // The operator's selection survives a reconnect.
        assert_eq!(
            state.load().unwrap().unwrap().selected_tags,
            vec!["line1.fast.tag0".to_owned()]
        );
    }

    #[tokio::test]
    async fn a_fresh_install_issues_one_read_key_and_zero_tags_is_connected() {
        let hub = MockHub::start(routes(vec![
            (STATUS_ROUTE, vec![(200, commissioning_body(false))]),
            (ISSUE_ROUTE, vec![(201, issued_body(42, "n", &key()))]),
            (TAGS_ROUTE, vec![(200, catalog_body(0))]),
        ]));
        let keys = Arc::new(MemoryKeyStore::new());
        let state = Arc::new(MemoryState::new());
        let bootstrapper = harness(Arc::clone(&keys), Arc::clone(&state));

        let connection = bootstrapper.connect(&hub.endpoint()).await.unwrap();

        assert_eq!(
            connection.status,
            HubStatus::Connected { tag_count: 0 },
            "an empty catalog is a successful connection, not a failure"
        );
        assert!(connection.catalog.unwrap().tags.is_empty());

        let issue = hub
            .seen()
            .into_iter()
            .find(|request| request.route() == ISSUE_ROUTE)
            .expect("the key must have been issued");
        assert!(
            issue.body.contains(r#""scopes":["read"]"#),
            "{}",
            issue.body
        );
        assert!(issue.body.contains(&format!("{APP_ID}-{INSTALLATION}-")));
        assert_eq!(issue.banto_client.as_deref(), Some("banto"));

        // The plaintext went to the keyring and only to the keyring.
        assert_eq!(keys.len(), 1);
        let record = state.load().unwrap().unwrap();
        assert_eq!(record.key_id, Some(42));
        assert_eq!(record.keyring_account, account_for(&hub));
        assert!(!serde_json::to_string(&record).unwrap().contains(&key()));

        // `/api/v1/*` must NOT carry the admin CSRF marker.
        let catalog = hub
            .seen()
            .into_iter()
            .find(|request| request.route() == TAGS_ROUTE)
            .expect("the catalog must have been read");
        assert_eq!(catalog.banto_client, None);
        assert_eq!(
            catalog.authorization.as_deref(),
            Some(&format!("Bearer {}", key())[..])
        );
    }

    #[tokio::test]
    async fn a_locked_down_hub_is_never_self_issued_against() {
        let hub = MockHub::start(routes(vec![(
            STATUS_ROUTE,
            vec![(200, commissioning_body(true))],
        )]));
        let keys = Arc::new(MemoryKeyStore::new());
        let state = Arc::new(MemoryState::new());
        let bootstrapper = harness(Arc::clone(&keys), Arc::clone(&state));

        let connection = bootstrapper.connect(&hub.endpoint()).await.unwrap();

        assert_eq!(connection.status, HubStatus::NeedsPairing);
        assert!(connection.catalog.is_none());
        assert_eq!(hub.hit_count(ISSUE_ROUTE), 0);
        assert!(keys.is_empty());
        assert!(state.load().unwrap().is_none());
    }

    /// #446: the probe's answer -> state, as a table.
    #[test]
    fn a_rejected_catalog_is_classified_by_the_probe() {
        let probe = |status, key_tripped| {
            Some(ProbedStatus {
                status,
                key_tripped,
            })
        };
        for (input, expected) in [
            (probe(403, true), HubStatus::KeyTripped),
            (probe(403, false), HubStatus::Forbidden),
            (probe(401, false), HubStatus::AuthFailed),
            // A `key_tripped` flag is only ever set for a 403; a 401 stays
            // "the key is not valid".
            (probe(401, true), HubStatus::AuthFailed),
            (probe(500, false), HubStatus::AuthFailed),
            (None, HubStatus::AuthFailed),
        ] {
            assert_eq!(rejection_status(input), expected, "{input:?}");
        }
    }

    #[test]
    fn only_the_hubs_key_tripped_body_counts_as_a_trip() {
        use crate::admin::is_key_tripped_body;
        assert!(is_key_tripped_body(br#"{"error":"key_tripped"}"#));
        assert!(is_key_tripped_body(
            br#"{ "error": "key_tripped", "extra": 1 }"#
        ));
        for body in [
            &br#"{"error":"forbidden"}"#[..],
            br#"{"kind":"forbidden","message":"key_tripped"}"#,
            br#"{"error":"KEY_TRIPPED"}"#,
            br#"{"error":null}"#,
            br#"{}"#,
            b"key_tripped",
            b"",
        ] {
            assert!(
                !is_key_tripped_body(body),
                "{}",
                String::from_utf8_lossy(body)
            );
        }
    }

    /// #449 review: every verdict says **which** credential it judged, and it
    /// is compared with the stored one rather than assumed.
    #[tokio::test]
    async fn a_verdict_names_the_credential_it_judged() {
        // The stored key: `refresh_catalog` judges exactly it.
        let hub = MockHub::start(routes(vec![
            (TAGS_ROUTE, vec![(401, String::from("{}"))]),
            (STATUS_ROUTE, vec![(200, commissioning_body(false))]),
            (
                ISSUE_ROUTE,
                vec![(201, issued_body(43, "n2", "bh_second0_other-secret"))],
            ),
        ]));
        let keys = Arc::new(MemoryKeyStore::new());
        keys.set(&account_for(&hub), &key()).unwrap();
        let state = Arc::new(MemoryState::seeded(seeded_record(&hub, None)));
        let bootstrapper = harness(Arc::clone(&keys), Arc::clone(&state));
        let k1 = bootstrapper.stored_credential().unwrap().unwrap();
        let refreshed = bootstrapper.refresh_catalog().await.unwrap();
        assert_eq!(refreshed.judged, Some(k1));

        // A rejected candidate: judged, but never stored.
        let candidate = bootstrapper
            .adopt_manual_key(&hub.endpoint(), "bh_cand0000_candidate".to_owned())
            .await
            .unwrap();
        assert_eq!(candidate.status, HubStatus::AuthFailed);
        assert!(candidate.judged.is_some());
        assert_ne!(candidate.judged, Some(k1));
        assert_eq!(bootstrapper.stored_credential().unwrap(), Some(k1));

        // `connect` re-issues (the stored key is rejected): the new key is
        // stored **before** its own check, and the verdict is about it even
        // though that check fails here.
        let connected = bootstrapper.connect(&hub.endpoint()).await.unwrap();
        assert_eq!(connected.status, HubStatus::AuthFailed);
        let k2 = bootstrapper.stored_credential().unwrap().unwrap();
        assert_ne!(k2, k1);
        assert_eq!(connected.judged, Some(k2));

        // No key stored: nothing to identify.
        bootstrapper.disconnect().unwrap();
        assert_eq!(bootstrapper.stored_credential().unwrap(), None);
    }

    #[test]
    fn a_credential_identity_does_not_print_its_value() {
        let keys = Arc::new(MemoryKeyStore::new());
        let hub_endpoint = "http://127.0.0.1:1";
        let state = Arc::new(MemoryState::seeded(HubRecord {
            endpoint: hub_endpoint.to_owned(),
            installation_id: INSTALLATION.to_owned(),
            key_id: None,
            key_name: None,
            keyring_account: "account".to_owned(),
            selected_tags: Vec::new(),
        }));
        keys.set("account", &key()).unwrap();
        let identity = harness(keys, state).stored_credential().unwrap().unwrap();
        assert_eq!(format!("{identity:?}"), "CredentialIdentity(..)");
    }

    /// #446: a stored key the Hub reports as tripped (`403 key_tripped`) is
    /// `KeyTripped` for `status`, and `connect` keeps it - no revoke, no
    /// issue, no commissioning check.
    #[tokio::test]
    async fn a_tripped_stored_key_is_key_tripped_and_connect_keeps_it() {
        let hub = MockHub::start(routes(vec![(
            TAGS_ROUTE,
            vec![(403, String::from(r#"{"error":"key_tripped"}"#))],
        )]));
        let keys = Arc::new(MemoryKeyStore::new());
        keys.set(&account_for(&hub), &key()).unwrap();
        let state = Arc::new(MemoryState::seeded(seeded_record(&hub, Some(7))));
        let bootstrapper = harness(Arc::clone(&keys), Arc::clone(&state));

        assert_eq!(bootstrapper.status().await.unwrap(), HubStatus::KeyTripped);
        let connection = bootstrapper.connect(&hub.endpoint()).await.unwrap();
        assert_eq!(connection.status, HubStatus::KeyTripped);
        assert!(connection.catalog.is_none());
        assert_eq!(hub.hit_count(ISSUE_ROUTE), 0, "no second key");
        assert!(
            !hub.routes_seen()
                .iter()
                .any(|route| route.contains("/revoke")),
            "the tripped key is not revoked"
        );
        assert_eq!(keys.get(&account_for(&hub)).unwrap(), Some(key()));
    }

    #[tokio::test]
    async fn an_invalid_stored_key_is_auth_failed_and_a_scopeless_one_is_forbidden() {
        for (status, expected) in [(401, HubStatus::AuthFailed), (403, HubStatus::Forbidden)] {
            let hub = MockHub::start(routes(vec![(
                TAGS_ROUTE,
                vec![(status, String::from("{}"))],
            )]));
            let keys = Arc::new(MemoryKeyStore::new());
            keys.set(&account_for(&hub), &key()).unwrap();
            let state = Arc::new(MemoryState::seeded(seeded_record(&hub, Some(7))));
            let bootstrapper = harness(keys, state);

            assert_eq!(bootstrapper.status().await.unwrap(), expected);
        }
    }

    #[tokio::test]
    async fn an_unreachable_hub_is_unreachable_not_an_empty_catalog() {
        // Bind then drop, so the port is closed but well-formed.
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);

        let keys = Arc::new(MemoryKeyStore::new());
        let state = Arc::new(MemoryState::new());
        let bootstrapper = harness(Arc::clone(&keys), Arc::clone(&state));

        let connection = bootstrapper.connect(&endpoint).await.unwrap();
        assert_eq!(
            connection.status,
            HubStatus::unreachable(UnreachableCause::Transport)
        );
        assert!(connection.catalog.is_none());
        assert!(
            state.load().unwrap().is_none(),
            "a failed connect must not leave a configured record behind"
        );
    }

    #[tokio::test]
    async fn a_server_error_on_the_catalog_is_unreachable_not_forbidden() {
        let hub = MockHub::start(routes(vec![(TAGS_ROUTE, vec![(500, String::from("{}"))])]));
        let keys = Arc::new(MemoryKeyStore::new());
        keys.set(&account_for(&hub), &key()).unwrap();
        let state = Arc::new(MemoryState::seeded(seeded_record(&hub, Some(7))));
        let bootstrapper = harness(keys, state);

        assert_eq!(
            bootstrapper.status().await.unwrap(),
            HubStatus::unreachable(UnreachableCause::ServerError)
        );
    }

    #[tokio::test]
    async fn a_lost_keyring_revokes_the_old_id_and_re_issues_under_a_new_name() {
        let hub = MockHub::start(routes(vec![
            (STATUS_ROUTE, vec![(200, commissioning_body(false))]),
            (ISSUE_ROUTE, vec![(201, issued_body(99, "n", &key()))]),
            (TAGS_ROUTE, vec![(200, catalog_body(1))]),
            (
                "POST /api/api-keys/7/revoke",
                vec![(200, String::from("{}"))],
            ),
        ]));
        let keys = Arc::new(MemoryKeyStore::new());
        let previous = seeded_record(&hub, Some(7));
        let state = Arc::new(MemoryState::seeded(previous.clone()));
        let bootstrapper = harness(Arc::clone(&keys), Arc::clone(&state));

        let connection = bootstrapper.connect(&hub.endpoint()).await.unwrap();

        assert_eq!(connection.status, HubStatus::Connected { tag_count: 1 });
        assert_eq!(
            hub.hit_count("POST /api/api-keys/7/revoke"),
            1,
            "the id this installation issued last time must be revoked"
        );
        // Never a list-and-guess-by-name revoke.
        assert_eq!(hub.hit_count("GET /api/api-keys"), 0);
        let record = state.load().unwrap().unwrap();
        assert_eq!(record.key_id, Some(99));
        assert_ne!(
            record.key_name, previous.key_name,
            "a new name, not the old one"
        );
        assert_eq!(record.selected_tags, previous.selected_tags);
    }

    /// A `key_id` belongs to one Hub. Pointing the app at a different Hub
    /// must not carry it across: ids are per-Hub row ids, so revoking "id 7"
    /// on the new Hub would take away somebody else's key.
    #[tokio::test]
    async fn switching_endpoints_never_revokes_the_other_hubs_id() {
        let hub_a = MockHub::start(routes(vec![(TAGS_ROUTE, vec![(200, catalog_body(1))])]));
        let keys = Arc::new(MemoryKeyStore::new());
        keys.set(&account_for(&hub_a), &key()).unwrap();
        // As if a previous run had issued id 7 against Hub A.
        let state = Arc::new(MemoryState::seeded(seeded_record(&hub_a, Some(7))));
        let bootstrapper = harness(Arc::clone(&keys), Arc::clone(&state));

        // Hub B: no key for it in the keyring, so it has to issue - and it
        // happens to hand out the same id 7 (row ids are per-Hub).
        let hub_b = MockHub::start(routes(vec![
            (STATUS_ROUTE, vec![(200, commissioning_body(false))]),
            (ISSUE_ROUTE, vec![(201, issued_body(7, "n", &key()))]),
            (TAGS_ROUTE, vec![(200, catalog_body(0))]),
            (
                "POST /api/api-keys/7/revoke",
                vec![(200, String::from("{}"))],
            ),
        ]));

        let connection = bootstrapper.connect(&hub_b.endpoint()).await.unwrap();

        assert_eq!(connection.status, HubStatus::Connected { tag_count: 0 });
        assert_eq!(
            hub_b.hit_count("POST /api/api-keys/7/revoke"),
            0,
            "Hub A's id must never be revoked on Hub B"
        );
        assert_eq!(hub_a.hit_count("POST /api/api-keys/7/revoke"), 0);
        let record = state.load().unwrap().unwrap();
        assert_eq!(record.endpoint, hub_b.endpoint());
        assert_eq!(record.key_id, Some(7), "the id B itself issued");
        assert_eq!(record.keyring_account, account_for(&hub_b));
        assert_ne!(record.keyring_account, account_for(&hub_a));
        assert!(
            record.selected_tags.is_empty(),
            "A's selection refers to A's catalog and must not follow"
        );
    }

    /// v1 keeps exactly one record, so coming back to the first Hub reuses
    /// its keyring entry (no issue request) but no longer knows the id it
    /// issued there - which is safe: a forgotten id is never revoked.
    #[tokio::test]
    async fn returning_to_the_first_endpoint_reuses_its_key_but_has_forgotten_its_id() {
        let hub_a = MockHub::start(routes(vec![(TAGS_ROUTE, vec![(200, catalog_body(1))])]));
        let hub_b = MockHub::start(routes(vec![
            (STATUS_ROUTE, vec![(200, commissioning_body(false))]),
            (ISSUE_ROUTE, vec![(201, issued_body(5, "n", &key()))]),
            (TAGS_ROUTE, vec![(200, catalog_body(0))]),
        ]));
        let keys = Arc::new(MemoryKeyStore::new());
        keys.set(&account_for(&hub_a), &key()).unwrap();
        let state = Arc::new(MemoryState::seeded(seeded_record(&hub_a, Some(7))));
        let bootstrapper = harness(Arc::clone(&keys), Arc::clone(&state));

        bootstrapper.connect(&hub_b.endpoint()).await.unwrap();
        let connection = bootstrapper.connect(&hub_a.endpoint()).await.unwrap();

        assert_eq!(connection.status, HubStatus::Connected { tag_count: 1 });
        assert_eq!(
            hub_a.hit_count(ISSUE_ROUTE),
            0,
            "A's keyring entry still works, so nothing is issued"
        );
        let record = state.load().unwrap().unwrap();
        assert_eq!(record.endpoint, hub_a.endpoint());
        assert_eq!(record.keyring_account, account_for(&hub_a));
        assert_eq!(
            record.key_id, None,
            "the id A issued was forgotten when the record moved to B"
        );
    }

    /// The account spelling gained the path prefix after the first release,
    /// so the record's own `keyring_account` has to win over a freshly
    /// computed one - otherwise an upgrade orphans a working key and
    /// silently issues a second one.
    #[tokio::test]
    async fn a_recorded_keyring_account_is_reused_even_when_its_spelling_is_older() {
        let hub = MockHub::start(routes(vec![
            (STATUS_ROUTE, vec![(200, commissioning_body(false))]),
            (ISSUE_ROUTE, vec![(201, issued_body(1, "n", &key()))]),
            (TAGS_ROUTE, vec![(200, catalog_body(1))]),
        ]));
        // The pre-#382 spelling: host and port only, no path prefix.
        let base = base_url(&hub.endpoint()).unwrap();
        let legacy_account = format!(
            "hub:{}:{}:{INSTALLATION}",
            base.host_str().unwrap(),
            base.port_or_known_default().unwrap()
        );
        assert_ne!(
            legacy_account,
            account_for(&hub),
            "the spelling really did change"
        );

        let keys = Arc::new(MemoryKeyStore::new());
        keys.set(&legacy_account, &key()).unwrap();
        let mut record = seeded_record(&hub, Some(7));
        record.keyring_account = legacy_account.clone();
        let state = Arc::new(MemoryState::seeded(record));
        let bootstrapper = harness(Arc::clone(&keys), Arc::clone(&state));

        let connection = bootstrapper.connect(&hub.endpoint()).await.unwrap();

        assert_eq!(connection.status, HubStatus::Connected { tag_count: 1 });
        assert_eq!(
            hub.hit_count(ISSUE_ROUTE),
            0,
            "the key stored under the old account still works"
        );
        assert_eq!(keys.len(), 1, "no duplicate entry under the new spelling");
        assert_eq!(
            state.load().unwrap().unwrap().keyring_account,
            legacy_account
        );
    }

    /// The endpoint comparison is normalized, not textual: a trailing slash
    /// is the same Hub, so a re-typed URL must not look like a switch.
    #[tokio::test]
    async fn a_differently_spelled_equal_endpoint_is_the_same_hub() {
        let hub = MockHub::start(routes(vec![
            (STATUS_ROUTE, vec![(200, commissioning_body(false))]),
            (ISSUE_ROUTE, vec![(201, issued_body(99, "n", &key()))]),
            (TAGS_ROUTE, vec![(200, catalog_body(1))]),
            (
                "POST /api/api-keys/7/revoke",
                vec![(200, String::from("{}"))],
            ),
        ]));
        // Keyring lost, so this re-issues - which is when the previous id
        // matters.
        let keys = Arc::new(MemoryKeyStore::new());
        let state = Arc::new(MemoryState::seeded(seeded_record(&hub, Some(7))));
        let bootstrapper = harness(keys, Arc::clone(&state));

        let connection = bootstrapper
            .connect(&format!("{}/", hub.endpoint()))
            .await
            .unwrap();

        assert_eq!(connection.status, HubStatus::Connected { tag_count: 1 });
        assert_eq!(
            hub.hit_count("POST /api/api-keys/7/revoke"),
            1,
            "a trailing slash must not read as a different Hub"
        );
        assert_eq!(
            state.load().unwrap().unwrap().selected_tags,
            vec!["line1.fast.tag0".to_owned()],
            "the operator's selection survives a re-issue against the same Hub"
        );
    }

    #[tokio::test]
    async fn a_duplicate_name_is_retried_once_with_a_later_timestamp() {
        let hub = MockHub::start(routes(vec![
            (STATUS_ROUTE, vec![(200, commissioning_body(false))]),
            (
                ISSUE_ROUTE,
                vec![
                    (422, duplicate_name_body()),
                    (201, issued_body(5, "n", &key())),
                ],
            ),
            (TAGS_ROUTE, vec![(200, catalog_body(0))]),
        ]));
        let keys = Arc::new(MemoryKeyStore::new());
        let state = Arc::new(MemoryState::new());
        let bootstrapper = harness(Arc::clone(&keys), Arc::clone(&state));

        let connection = bootstrapper.connect(&hub.endpoint()).await.unwrap();

        assert_eq!(connection.status, HubStatus::Connected { tag_count: 0 });
        assert_eq!(hub.hit_count(ISSUE_ROUTE), 2, "exactly one retry");
        let issues: Vec<String> = hub
            .seen()
            .into_iter()
            .filter(|request| request.route() == ISSUE_ROUTE)
            .map(|request| request.body)
            .collect();
        assert_ne!(issues[0], issues[1], "the retry must use a different name");
    }

    #[tokio::test]
    async fn issuing_is_refused_for_admin_and_write_scopes_before_any_request() {
        let hub = MockHub::start(routes(vec![
            (STATUS_ROUTE, vec![(200, commissioning_body(false))]),
            (ISSUE_ROUTE, vec![(201, issued_body(1, "n", &key()))]),
        ]));
        let bootstrapper = harness(
            Arc::new(MemoryKeyStore::new()),
            Arc::new(MemoryState::new()),
        );

        for scopes in [
            vec!["admin"],
            vec!["write:line1.fast.temp01"],
            vec!["read", "admin"],
        ] {
            let error = bootstrapper
                .connect_with_scopes(&hub.endpoint(), &scopes)
                .await
                .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::ForbiddenScope);
        }
        assert!(
            hub.seen().is_empty(),
            "the whitelist must fail closed before any network call"
        );
    }

    #[tokio::test]
    async fn disconnect_clears_local_state_without_revoking_on_the_hub() {
        let hub = MockHub::start(routes(vec![]));
        let keys = Arc::new(MemoryKeyStore::new());
        keys.set(&account_for(&hub), &key()).unwrap();
        let state = Arc::new(MemoryState::seeded(seeded_record(&hub, Some(7))));
        let bootstrapper = harness(Arc::clone(&keys), Arc::clone(&state));

        bootstrapper.disconnect().unwrap();

        assert!(keys.is_empty());
        assert!(state.load().unwrap().is_none());
        assert!(
            hub.seen().is_empty(),
            "disconnect must not talk to the Hub at all"
        );
    }

    #[tokio::test]
    async fn a_manual_key_is_verified_before_it_is_stored() {
        // Rejected first: nothing is stored.
        let rejecting = MockHub::start(routes(vec![(TAGS_ROUTE, vec![(401, String::from("{}"))])]));
        let keys = Arc::new(MemoryKeyStore::new());
        let state = Arc::new(MemoryState::new());
        let bootstrapper = harness(Arc::clone(&keys), Arc::clone(&state));

        let connection = bootstrapper
            .adopt_manual_key(&rejecting.endpoint(), key())
            .await
            .unwrap();
        assert_eq!(connection.status, HubStatus::AuthFailed);
        assert!(keys.is_empty());
        assert!(state.load().unwrap().is_none());

        // Accepted: the plaintext goes to the keyring, the record carries a
        // reference only and no issued id.
        let accepting = MockHub::start(routes(vec![(TAGS_ROUTE, vec![(200, catalog_body(3))])]));
        let connection = bootstrapper
            .adopt_manual_key(&accepting.endpoint(), key())
            .await
            .unwrap();
        assert_eq!(connection.status, HubStatus::Connected { tag_count: 3 });
        assert_eq!(keys.len(), 1);
        let record = state.load().unwrap().unwrap();
        assert_eq!(record.key_id, None);
        assert_eq!(record.keyring_account, account_for(&accepting));
        assert!(!serde_json::to_string(&record).unwrap().contains(&key()));
        assert_eq!(accepting.hit_count(ISSUE_ROUTE), 0);
    }

    #[tokio::test]
    async fn status_is_not_configured_before_anything_is_saved_and_selection_needs_a_record() {
        let bootstrapper = harness(
            Arc::new(MemoryKeyStore::new()),
            Arc::new(MemoryState::new()),
        );
        assert_eq!(
            bootstrapper.status().await.unwrap(),
            HubStatus::NotConfigured
        );
        assert_eq!(
            bootstrapper.set_selected_tags(vec![]).unwrap_err().kind(),
            ErrorKind::NotConfigured
        );
        assert_eq!(
            bootstrapper.refresh_catalog().await.unwrap_err().kind(),
            ErrorKind::NotConfigured
        );
    }

    #[tokio::test]
    async fn an_empty_selection_can_be_saved() {
        let hub = MockHub::start(routes(vec![]));
        let state = Arc::new(MemoryState::seeded(seeded_record(&hub, Some(7))));
        let bootstrapper = harness(Arc::new(MemoryKeyStore::new()), Arc::clone(&state));

        bootstrapper.set_selected_tags(vec![]).unwrap();
        assert!(state.load().unwrap().unwrap().selected_tags.is_empty());

        bootstrapper
            .set_selected_tags(vec!["line1.fast.tag0".to_owned()])
            .unwrap();
        assert_eq!(
            state.load().unwrap().unwrap().selected_tags,
            vec!["line1.fast.tag0".to_owned()]
        );
    }

    /// `rest_client()` is the application's way into a subscription, so its
    /// three answers have to stay distinct: nothing configured and no
    /// keyring entry are both "cannot subscribe" (`None`, not an error, and
    /// not a verdict about the Hub), while a record plus a stored key yields
    /// a client. No network is involved - building the client performs no
    /// I/O.
    #[tokio::test]
    async fn rest_client_is_none_until_both_a_record_and_a_stored_key_exist() {
        let hub = MockHub::start(routes(vec![]));

        // 1. Nothing configured at all.
        let keys = Arc::new(MemoryKeyStore::new());
        let state = Arc::new(MemoryState::new());
        let bootstrapper = harness(Arc::clone(&keys), Arc::clone(&state));
        assert!(bootstrapper.rest_client().unwrap().is_none());

        // 2. A record survives but the keyring entry does not.
        let state = Arc::new(MemoryState::seeded(seeded_record(&hub, Some(7))));
        let bootstrapper = harness(Arc::clone(&keys), Arc::clone(&state));
        assert!(
            bootstrapper.rest_client().unwrap().is_none(),
            "a missing keyring entry is 'cannot subscribe', not an error"
        );

        // 3. Both present.
        keys.set(&account_for(&hub), &key()).unwrap();
        let bootstrapper = harness(keys, state);
        assert!(bootstrapper.rest_client().unwrap().is_some());
        assert!(
            hub.seen().is_empty(),
            "building a client must not talk to the Hub"
        );
    }

    #[tokio::test]
    async fn an_invalid_endpoint_is_refused_before_any_request() {
        let bootstrapper = harness(
            Arc::new(MemoryKeyStore::new()),
            Arc::new(MemoryState::new()),
        );
        for endpoint in [
            "https://example.test",
            "http://user@example.test",
            "nonsense",
        ] {
            assert_eq!(
                bootstrapper.connect(endpoint).await.unwrap_err().kind(),
                ErrorKind::InvalidEndpoint
            );
        }
    }
}
