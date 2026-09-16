//! The thin banto-hub admin layer: exactly the three requests this crate
//! needs that `banto-tagclient` does not expose, and nothing else.
//!
//! * `GET /api/commissioning/status` - readable without authentication by
//!   design (tag-server-design.md §5.6).
//! * `POST /api/api-keys` - authentication is bypassed while the Hub is in
//!   commissioning mode; once locked down this answers `401` and this crate
//!   reports [`HubStatus::NeedsPairing`](crate::HubStatus::NeedsPairing)
//!   instead of trying anything else.
//! * `POST /api/api-keys/{id}/revoke` - idempotent, and only ever called
//!   with an id this installation issued itself.
//!
//! Both admin routes need `X-Banto-Client: banto` (a CSRF marker applied to
//! the whole admin router, not a credential); `/api/v1/*` does not.
//!
//! There is one more request here, [`AdminClient::probe_tags_status`], which
//! is *not* an admin route: see its doc comment for why the 401/403 split
//! cannot come from `fetch_catalog` alone.

use reqwest::{Client, ClientBuilder, StatusCode, Url};
use serde::Deserialize;
use zeroize::Zeroizing;

use crate::error::{Error, ErrorKind, Result};
use crate::status::UnreachableCause;

/// CSRF marker required by banto-hub's whole admin router. Not a secret and
/// not a credential - it only asserts "this request came from a Banto
/// front end", exactly as the Hub's own UI sends it.
const BANTO_CLIENT_HEADER: (&str, &str) = ("X-Banto-Client", "banto");

/// `GET /api/commissioning/status`'s body.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommissioningStatusBody {
    locked_down: bool,
}

/// `POST /api/api-keys`'s success body. `key` is the plaintext, returned
/// only here, only once.
#[derive(Debug, Deserialize)]
struct IssuedApiKeyBody {
    id: i64,
    name: String,
    key: String,
}

/// banto-hub's `ErrorBody` (banto-core), narrowed to the one shape this
/// crate has to act on: a `validation` rejection naming the `name` field,
/// which is how a duplicate key name comes back (it is *not* a 409).
#[derive(Debug, Deserialize)]
struct ErrorBodyProbe {
    kind: Option<String>,
    #[serde(default)]
    field_errors: Vec<ErrorFieldProbe>,
}

#[derive(Debug, Deserialize)]
struct ErrorFieldProbe {
    field: String,
}

/// A key banto-hub just minted. The plaintext lives in a `Zeroizing`
/// wrapper from the moment it is parsed until it reaches the
/// [`KeyStore`](crate::KeyStore).
pub(crate) struct IssuedKey {
    pub(crate) id: i64,
    pub(crate) name: String,
    pub(crate) key: Zeroizing<String>,
}

/// What `POST /api/api-keys` did.
pub(crate) enum IssueOutcome {
    Issued(IssuedKey),
    /// The Hub rejected the request with a validation error on `name` -
    /// another key already uses it. The caller retries once with a later
    /// timestamp in the name.
    DuplicateName,
    /// `401`/`403`: the Hub will not mint a key for an unauthenticated
    /// caller, i.e. commissioning is over. Reported as "needs pairing", not
    /// as an error, and never worked around.
    Unauthenticated,
    Failed(UnreachableCause),
}

/// Validate and normalize a Hub base URL.
///
/// Mirrors `banto_tagclient::Endpoint`'s contract (plain `http`, a host, no
/// userinfo/query/fragment, no port `0`) because the same string is handed
/// to `Endpoint::new` for the catalog check; keeping a private `Url` here as
/// well is what lets this module build the two admin routes, which
/// `Endpoint` deliberately knows nothing about.
pub(crate) fn base_url(endpoint: &str) -> Result<Url> {
    let raw = endpoint.trim();
    // `Url::parse("http:///x")` silently promotes the first path segment to
    // the host; reject an empty authority before parsing, exactly as
    // `banto_tagclient::Endpoint::new` does.
    if raw
        .strip_prefix("http://")
        .is_some_and(|rest| rest.split(['/', '?', '#']).next().is_none_or(str::is_empty))
    {
        return Err(Error::new(ErrorKind::InvalidEndpoint));
    }
    let mut base = Url::parse(raw).map_err(|_| Error::new(ErrorKind::InvalidEndpoint))?;
    if base.scheme() != "http"
        || base.host_str().is_none()
        || !base.username().is_empty()
        || base.password().is_some()
        || base.query().is_some()
        || base.fragment().is_some()
        || base.port() == Some(0)
    {
        return Err(Error::new(ErrorKind::InvalidEndpoint));
    }
    let trailing_slashes = base.path().chars().rev().take_while(|&c| c == '/').count();
    {
        let mut segments = base
            .path_segments_mut()
            .map_err(|_| Error::new(ErrorKind::InvalidEndpoint))?;
        for _ in 0..trailing_slashes {
            segments.pop_if_empty();
        }
        segments.push("");
    }
    Ok(base)
}

/// The [`KeyStore`](crate::KeyStore) account one Hub + one installation
/// share: `hub:{host}:{port}:{installation_id}`.
///
/// The port is always spelled out (`port_or_known_default`) so
/// `http://host` and `http://host:80` do not end up filed as two different
/// Hubs.
pub(crate) fn keyring_account(base: &Url, installation_id: &str) -> String {
    let host = base.host_str().unwrap_or_default();
    let port = base.port_or_known_default().unwrap_or(0);
    format!("hub:{host}:{port}:{installation_id}")
}

fn join(base: &Url, segments: &[&str]) -> Url {
    let mut url = base.clone();
    {
        let mut path = url
            .path_segments_mut()
            .expect("HTTP URLs always support path segment mutation");
        path.pop_if_empty();
        for segment in segments {
            path.push(segment);
        }
    }
    url
}

/// The two admin requests plus the authorization probe, all against one
/// normalized base URL.
pub(crate) struct AdminClient {
    http: Client,
    base: Url,
}

impl AdminClient {
    /// Redirects and proxies are disabled for the same reasons
    /// `banto_tagclient::RestClient` disables them: a redirect would move
    /// a credential-bearing request somewhere the caller never named.
    pub(crate) fn new(base: Url) -> Result<Self> {
        let http = ClientBuilder::new()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .map_err(|_| {
                tracing::warn!("failed to build the banto-hub admin client");
                Error::new(ErrorKind::InvalidEndpoint)
            })?;
        Ok(Self { http, base })
    }

    pub(crate) fn base(&self) -> &Url {
        &self.base
    }

    /// `GET /api/commissioning/status`. `Err(cause)` means the answer is
    /// unknown - the caller must NOT assume "not locked down" and issue a
    /// key on a guess.
    pub(crate) async fn locked_down(&self) -> std::result::Result<bool, UnreachableCause> {
        let url = join(&self.base, &["api", "commissioning", "status"]);
        let response = self
            .http
            .get(url)
            .header(BANTO_CLIENT_HEADER.0, BANTO_CLIENT_HEADER.1)
            .send()
            .await
            .map_err(|error| self.classify_send_failure("commissioning_status", &error))?;
        let status = response.status();
        if status.is_redirection() {
            self.warn_status("commissioning_status", status);
            return Err(UnreachableCause::InvalidEndpoint);
        }
        if !status.is_success() {
            self.warn_status("commissioning_status", status);
            return Err(UnreachableCause::ServerError);
        }
        let body = response
            .bytes()
            .await
            .map_err(|error| self.classify_send_failure("commissioning_status_body", &error))?;
        serde_json::from_slice::<CommissioningStatusBody>(&body)
            .map(|body| body.locked_down)
            .map_err(|_| {
                self.warn_body("commissioning_status", body.len());
                UnreachableCause::Protocol
            })
    }

    /// `POST /api/api-keys`. The caller has already run the scope
    /// whitelist ([`validate_issue_scopes`](crate::validate_issue_scopes));
    /// this function performs no policy of its own.
    pub(crate) async fn issue(&self, name: &str, scopes: &[&str]) -> IssueOutcome {
        let url = join(&self.base, &["api", "api-keys"]);
        // Serialized by hand rather than with reqwest's `json` feature:
        // `banto-tagclient` pulls reqwest in with `default-features = false`
        // and no extra features, and this crate must not widen that (the
        // request body is two fields).
        let body = serde_json::json!({ "name": name, "scopes": scopes }).to_string();
        let response = match self
            .http
            .post(url)
            .header(BANTO_CLIENT_HEADER.0, BANTO_CLIENT_HEADER.1)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                return IssueOutcome::Failed(self.classify_send_failure("issue_api_key", &error))
            }
        };
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            self.warn_status("issue_api_key", status);
            return IssueOutcome::Unauthenticated;
        }
        if status.is_redirection() {
            self.warn_status("issue_api_key", status);
            return IssueOutcome::Failed(UnreachableCause::InvalidEndpoint);
        }
        let bytes = match response.bytes().await {
            Ok(bytes) => bytes,
            Err(error) => {
                return IssueOutcome::Failed(
                    self.classify_send_failure("issue_api_key_body", &error),
                )
            }
        };
        if !status.is_success() {
            self.warn_status("issue_api_key", status);
            if is_duplicate_name(&bytes) {
                return IssueOutcome::DuplicateName;
            }
            return IssueOutcome::Failed(UnreachableCause::ServerError);
        }
        match serde_json::from_slice::<IssuedApiKeyBody>(&bytes) {
            Ok(issued) => IssueOutcome::Issued(IssuedKey {
                id: issued.id,
                name: issued.name,
                key: Zeroizing::new(issued.key),
            }),
            Err(_) => {
                self.warn_body("issue_api_key", bytes.len());
                IssueOutcome::Failed(UnreachableCause::Protocol)
            }
        }
    }

    /// `POST /api/api-keys/{id}/revoke`, best effort.
    ///
    /// Only ever called with an id read back from this installation's own
    /// [`HubRecord`](crate::HubRecord). There is deliberately no "list keys
    /// and revoke the ones whose name looks like mine" path: names are not
    /// unique to an installation the way an id is, and guessing would take
    /// another machine's credential away.
    pub(crate) async fn revoke(&self, id: i64) -> bool {
        let url = join(&self.base, &["api", "api-keys", &id.to_string(), "revoke"]);
        match self
            .http
            .post(url)
            .header(BANTO_CLIENT_HEADER.0, BANTO_CLIENT_HEADER.1)
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => true,
            Ok(response) => {
                self.warn_status("revoke_api_key", response.status());
                false
            }
            Err(error) => {
                self.classify_send_failure("revoke_api_key", &error);
                false
            }
        }
    }

    /// Read the HTTP status of `GET /api/v1/tags` and nothing else.
    ///
    /// The catalog itself is always fetched with
    /// `banto_tagclient::RestClient::fetch_catalog` - this crate never
    /// parses a catalog body. But that client maps 401 and 403 onto the
    /// single `ErrorKind::Unauthorized`, while issue #332's acceptance
    /// criteria require "認証に失敗" (a key that is invalid, and re-issuing
    /// fixes it) and "権限が不足" (a key that authenticates but has no read
    /// scope, and re-issuing does NOT fix it) to be different states on
    /// screen. `banto-tagclient` is not changed for this, so the split is
    /// recovered here with one extra status-only request, sent **only** on
    /// the failure path.
    pub(crate) async fn probe_tags_status(&self, key: &str) -> Option<u16> {
        let url = join(&self.base, &["api", "v1", "tags"]);
        match self.http.get(url).bearer_auth(key).send().await {
            Ok(response) => Some(response.status().as_u16()),
            Err(error) => {
                self.classify_send_failure("probe_tags_status", &error);
                None
            }
        }
    }

    /// Log a safe, secret-free diagnostic and classify the failure. Only
    /// reqwest's boolean classifiers and the underlying `io::ErrorKind` are
    /// recorded - the error's own `Display` echoes the request URL back,
    /// which would leak the endpoint's path prefix.
    fn classify_send_failure(
        &self,
        operation: &'static str,
        error: &reqwest::Error,
    ) -> UnreachableCause {
        tracing::warn!(
            host = ?self.base.host_str(),
            port = ?self.base.port_or_known_default(),
            operation,
            timeout = error.is_timeout(),
            connect = error.is_connect(),
            "banto-hub admin request failed"
        );
        UnreachableCause::Transport
    }

    fn warn_status(&self, operation: &'static str, status: StatusCode) {
        tracing::warn!(
            host = ?self.base.host_str(),
            port = ?self.base.port_or_known_default(),
            operation,
            status = u64::from(status.as_u16()),
            "banto-hub admin request returned an unexpected status"
        );
    }

    fn warn_body(&self, operation: &'static str, body_len: usize) {
        tracing::warn!(
            host = ?self.base.host_str(),
            port = ?self.base.port_or_known_default(),
            operation,
            body_len = body_len as u64,
            "banto-hub admin response was not valid JSON"
        );
    }
}

/// Is this error body banto-hub's "この名前は既に使用されています"?
///
/// Matched structurally (`kind == "validation"` and a field error naming
/// `name`), never by message text - the Japanese wording is not a contract.
fn is_duplicate_name(body: &[u8]) -> bool {
    serde_json::from_slice::<ErrorBodyProbe>(body).is_ok_and(|probe| {
        probe.kind.as_deref() == Some("validation")
            && probe.field_errors.iter().any(|entry| entry.field == "name")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_matches_the_tagclient_endpoint_contract() {
        for input in [
            "https://example.test",
            "ws://example.test",
            "http://name@example.test",
            "http://name:pass@example.test",
            "http://example.test?query=1",
            "http://example.test#fragment",
            "http:///missing-host",
            "http://example.test:0",
            "not a url",
        ] {
            assert_eq!(
                base_url(input).unwrap_err().kind(),
                ErrorKind::InvalidEndpoint,
                "{input} must be refused"
            );
        }
        assert_eq!(
            base_url("http://127.0.0.1:3100").unwrap().as_str(),
            "http://127.0.0.1:3100/"
        );
        assert_eq!(
            base_url("  http://example.test/hub///  ").unwrap().as_str(),
            "http://example.test/hub/"
        );
    }

    #[test]
    fn admin_and_tag_routes_are_built_under_the_prefix() {
        let base = base_url("http://example.test/hub").unwrap();
        assert_eq!(
            join(&base, &["api", "commissioning", "status"]).as_str(),
            "http://example.test/hub/api/commissioning/status"
        );
        assert_eq!(
            join(&base, &["api", "api-keys"]).as_str(),
            "http://example.test/hub/api/api-keys"
        );
        assert_eq!(
            join(&base, &["api", "api-keys", "12", "revoke"]).as_str(),
            "http://example.test/hub/api/api-keys/12/revoke"
        );
        assert_eq!(
            join(&base, &["api", "v1", "tags"]).as_str(),
            "http://example.test/hub/api/v1/tags"
        );
    }

    #[test]
    fn keyring_account_always_spells_out_the_port() {
        assert_eq!(
            keyring_account(&base_url("http://127.0.0.1:3100").unwrap(), "inst-1"),
            "hub:127.0.0.1:3100:inst-1"
        );
        assert_eq!(
            keyring_account(&base_url("http://example.test").unwrap(), "inst-1"),
            "hub:example.test:80:inst-1"
        );
    }

    #[test]
    fn duplicate_name_is_matched_structurally() {
        assert!(is_duplicate_name(
            r#"{"kind":"validation","field_errors":[{"field":"name","message":"この名前は既に使用されています"}]}"#
                .as_bytes()
        ));
        // A validation error about a different field is not a name clash.
        assert!(!is_duplicate_name(
            br#"{"kind":"validation","field_errors":[{"field":"scopes","message":"x"}]}"#
        ));
        assert!(!is_duplicate_name(br#"{"kind":"forbidden"}"#));
        assert!(!is_duplicate_name(b"not json"));
    }
}
