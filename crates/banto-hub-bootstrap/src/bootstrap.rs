//! [`Bootstrapper`]: the whole "zero-config" flow, app-independent.
//!
//! Reuse an already-issued key if there is one; otherwise ask the Hub
//! whether it is still in commissioning mode and, only then, mint exactly
//! one `read` key for this installation, hand the plaintext straight to the
//! [`KeyStore`], and prove the result by reading the catalog through
//! `banto-tagclient`.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use banto_tagclient::{Endpoint, ErrorKind as TagErrorKind, RestClient, SecretApiKey};
use zeroize::Zeroizing;

use crate::admin::{base_url, keyring_account, AdminClient, IssueOutcome};
use crate::error::{Error, ErrorKind, Result};
use crate::scopes::{validate_issue_scopes, DEFAULT_SCOPES};
use crate::state::{BootstrapState, HubRecord, KeyStore};
use crate::status::{HubConnection, HubStatus, UnreachableCause};

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
        }
    }

    /// Connect to `endpoint`, issuing a `read` key if this installation does
    /// not already have a working one.
    ///
    /// Reusing an existing key is the first thing tried, so restarting the
    /// app does not mint a key per launch. A key is only ever issued when
    /// the Hub reports it is still in commissioning mode; a locked-down Hub
    /// yields [`HubStatus::NeedsPairing`] and nothing else is attempted.
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
        let account = keyring_account(admin.base(), &self.installation_id);
        let previous = self.state.load()?;

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

        // 3. Revoke the key this installation issued last time, if any.
        //    Best effort, and only ever by id (see `AdminClient::revoke`).
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
        let account = keyring_account(admin.base(), &self.installation_id);
        let key = Zeroizing::new(key);
        let connection = self.verify(&admin, endpoint, &key).await?;
        if !connection.status.is_connected() {
            return Ok(connection);
        }
        self.keys.set(&account, &key)?;
        let previous = self.state.load()?;
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
        let tag_endpoint =
            Endpoint::new(endpoint).map_err(|_| Error::new(ErrorKind::InvalidEndpoint))?;
        let secret =
            SecretApiKey::new(key.to_string()).map_err(|_| Error::new(ErrorKind::InvalidKey))?;
        let client = RestClient::new(tag_endpoint, secret)
            .map_err(|_| Error::new(ErrorKind::InvalidEndpoint))?;

        match client.fetch_catalog().await {
            Ok(catalog) => Ok(HubConnection::connected(catalog)),
            Err(error) => Ok(HubConnection::failed(match error.kind() {
                // `banto-tagclient` collapses 401 and 403 into one kind;
                // the split the UI needs is recovered with a status-only
                // probe (see `AdminClient::probe_tags_status`).
                TagErrorKind::Unauthorized => match admin.probe_tags_status(key).await {
                    Some(403) => HubStatus::Forbidden,
                    _ => HubStatus::AuthFailed,
                },
                TagErrorKind::CatalogUnavailable => {
                    HubStatus::unreachable(UnreachableCause::ServerError)
                }
                TagErrorKind::ProtocolError => HubStatus::unreachable(UnreachableCause::Protocol),
                TagErrorKind::InvalidEndpoint => {
                    HubStatus::unreachable(UnreachableCause::InvalidEndpoint)
                }
                _ => HubStatus::unreachable(UnreachableCause::Transport),
            })),
        }
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
        // across a re-issue against the same Hub, drop it when the endpoint
        // changes (the names would refer to a different catalog).
        let selected_tags = previous
            .filter(|record| record.endpoint == endpoint)
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
