//! REST transport for the banto-hub catalog, value snapshot, and (Issue
//! #123) single-tag write.

use std::fmt;

use reqwest::{Client, ClientBuilder, StatusCode};

use crate::binding::BindingRequest;
use crate::endpoint::Endpoint;
use crate::error::{Error, ErrorKind, Result};
use crate::handle::{validate_start_requests, TagClientHandle};
use crate::secret::SecretApiKey;
use crate::types::{CatalogSnapshot, StableTagId, ValuesSnapshot};
use crate::write::{resolve_write_target, send_write, RequestedValue};
use crate::ws_transport::WebSocketConnection;

/// A banto-hub REST client that connects directly to its endpoint, without
/// redirects or system/environment proxy configuration. Reads
/// ([`fetch_catalog`](Self::fetch_catalog), [`fetch_values`](Self::fetch_values))
/// may be reused freely; [`write_tag`](Self::write_tag) additionally never
/// retries (see the `write` module doc for why).
pub struct RestClient {
    endpoint: Endpoint,
    secret: SecretApiKey,
    http: Client,
}

impl RestClient {
    /// Construct a client with redirects disabled.
    pub fn new(endpoint: Endpoint, secret: SecretApiKey) -> Result<Self> {
        let http = ClientBuilder::new()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .map_err(|_| {
                tracing::warn!("failed to build the banto-hub REST client");
                Error::new(ErrorKind::Transport)
            })?;
        Ok(Self {
            endpoint,
            secret,
            http,
        })
    }

    /// Start one owned connection generation on the current Tokio runtime.
    ///
    /// Requests are validated before a task or network operation is created.
    /// Reconnect and rebinding are handled by the supervisor; use
    /// [`TagClientHandle::restart`] to replace this consumed client and its
    /// credentials after the old handle has stopped and joined.
    pub fn start(self, requests: Vec<BindingRequest>) -> Result<TagClientHandle> {
        validate_start_requests(&requests)?;
        let runtime =
            tokio::runtime::Handle::try_current().map_err(|_| Error::new(ErrorKind::Transport))?;
        Ok(TagClientHandle::spawn(self, requests, runtime))
    }

    /// Fetch and deserialize `GET /api/v1/tags`.
    pub async fn fetch_catalog(&self) -> Result<CatalogSnapshot> {
        let response = self
            .secret
            .apply_authorization(self.http.get(self.endpoint.tags_url()))
            .send()
            .await
            .map_err(|error| self.log_send_failure("fetch_catalog", &error))?;
        let status = response.status();
        classify_catalog_status(&self.endpoint, status)?;
        let body = response
            .bytes()
            .await
            .map_err(|error| self.log_send_failure("fetch_catalog_body", &error))?;
        serde_json::from_slice(&body).map_err(|_| {
            tracing::warn!(
                host = ?self.endpoint.host(),
                port = ?self.endpoint.port(),
                body_len = body.len() as u64,
                "banto-hub catalog response was not valid JSON"
            );
            Error::new(ErrorKind::ProtocolError)
        })
    }

    /// Fetch and deserialize `GET /api/v1/values` for exactly the supplied
    /// external tag names. An empty slice still sends `tags=`.
    pub async fn fetch_values(&self, tags: &[&str]) -> Result<ValuesSnapshot> {
        if tags.iter().any(|tag| tag.contains(',')) {
            return Err(Error::new(ErrorKind::InvalidTagSelection));
        }
        let mut url = self.endpoint.values_url();
        let joined = tags.join(",");
        url.query_pairs_mut().append_pair("tags", &joined);
        let response = self
            .secret
            .apply_authorization(self.http.get(url))
            .send()
            .await
            .map_err(|error| self.log_send_failure("fetch_values", &error))?;
        let status = response.status();
        classify_values_status(&self.endpoint, status)?;
        let body = response
            .bytes()
            .await
            .map_err(|error| self.log_send_failure("fetch_values_body", &error))?;
        serde_json::from_slice(&body).map_err(|_| {
            tracing::warn!(
                host = ?self.endpoint.host(),
                port = ?self.endpoint.port(),
                body_len = body.len() as u64,
                "banto-hub values response was not valid JSON"
            );
            Error::new(ErrorKind::ProtocolError)
        })
    }

    /// Write one tag by its stable ID (Issue #123, tag-server-design.md §6).
    ///
    /// This is a single `POST /api/v1/values/{tag}` and nothing else - see
    /// the `write` module doc for why it is not part of `worker.rs`'s
    /// reconnect/backoff machinery and never retries automatically. The
    /// `external_name` banto-hub currently uses is resolved fresh from
    /// `GET /api/v1/tags` on every call (never cached, never accepted
    /// directly from the caller) because a rename changes it independently
    /// of the stable ID (design §4.1). A resolution failure
    /// ([`ErrorKind::BindingUnresolved`], [`ErrorKind::DuplicateCatalogStableId`])
    /// or a catalog fetch failure both return before any write request is
    /// sent.
    pub async fn write_tag(&self, stable_id: StableTagId, value: RequestedValue) -> Result<()> {
        let catalog = self.fetch_catalog().await?;
        let external_name = resolve_write_target(stable_id, &catalog.tags)?;
        send_write(
            &self.http,
            &self.endpoint,
            &self.secret,
            &external_name,
            value,
        )
        .await
    }

    /// Connect the stream using this client's endpoint and secret.
    pub(crate) async fn connect_stream(&self) -> Result<WebSocketConnection> {
        crate::ws_transport::connect(&self.endpoint, &self.secret).await
    }

    /// Log a safe, secret-free diagnostic for a failed REST send or body
    /// read and return the stable `transport` classification.
    ///
    /// Only `reqwest::Error`'s boolean classifiers (`is_timeout`,
    /// `is_connect`) and any underlying `io::ErrorKind` are recorded. The
    /// error's own `Display`/`Debug` is never logged: reqwest's error
    /// formatting echoes the request URL back, which would leak the
    /// endpoint's path prefix (see `Endpoint`'s redaction contract).
    fn log_send_failure(&self, operation: &'static str, error: &reqwest::Error) -> Error {
        tracing::warn!(
            host = ?self.endpoint.host(),
            port = ?self.endpoint.port(),
            operation,
            timeout = error.is_timeout(),
            connect = error.is_connect(),
            io_kind = ?io_error_kind(error),
            "banto-hub REST request failed"
        );
        Error::new(ErrorKind::Transport)
    }
}

/// Walk a `std::error::Error` source chain looking for the underlying
/// `std::io::Error` and return only its `ErrorKind` (a plain enum, never the
/// OS-provided message text) so DNS/refused/timeout-style failures can be
/// told apart in logs without risking secret-bearing text from any layer.
fn io_error_kind(error: &(dyn std::error::Error + 'static)) -> Option<std::io::ErrorKind> {
    let mut source = error.source();
    while let Some(current) = source {
        if let Some(io_error) = current.downcast_ref::<std::io::Error>() {
            return Some(io_error.kind());
        }
        source = current.source();
    }
    None
}

fn classify_catalog_status(endpoint: &Endpoint, status: StatusCode) -> Result<()> {
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        tracing::warn!(
            host = ?endpoint.host(),
            port = ?endpoint.port(),
            status = u64::from(status.as_u16()),
            "banto-hub rejected the catalog request as unauthorized"
        );
        Err(Error::new(ErrorKind::Unauthorized))
    } else if status.is_redirection() {
        tracing::warn!(
            host = ?endpoint.host(),
            port = ?endpoint.port(),
            status = u64::from(status.as_u16()),
            "banto-hub catalog response was a redirect; redirects are not followed"
        );
        Err(Error::new(ErrorKind::InvalidEndpoint))
    } else if !status.is_success() {
        tracing::warn!(
            host = ?endpoint.host(),
            port = ?endpoint.port(),
            status = u64::from(status.as_u16()),
            "banto-hub catalog request failed"
        );
        Err(Error::new(ErrorKind::CatalogUnavailable))
    } else {
        Ok(())
    }
}

fn classify_values_status(endpoint: &Endpoint, status: StatusCode) -> Result<()> {
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        tracing::warn!(
            host = ?endpoint.host(),
            port = ?endpoint.port(),
            status = u64::from(status.as_u16()),
            "banto-hub rejected the values request as unauthorized"
        );
        Err(Error::new(ErrorKind::Unauthorized))
    } else if status.is_redirection() {
        tracing::warn!(
            host = ?endpoint.host(),
            port = ?endpoint.port(),
            status = u64::from(status.as_u16()),
            "banto-hub values response was a redirect; redirects are not followed"
        );
        Err(Error::new(ErrorKind::InvalidEndpoint))
    } else if !status.is_success() {
        tracing::warn!(
            host = ?endpoint.host(),
            port = ?endpoint.port(),
            status = u64::from(status.as_u16()),
            "banto-hub values request failed"
        );
        Err(Error::new(ErrorKind::Transport))
    } else {
        Ok(())
    }
}

impl fmt::Debug for RestClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RestClient")
            .field("endpoint", &self.endpoint)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    use super::*;
    use crate::{Endpoint, ErrorKind, SecretApiKey};

    fn test_secret() -> String {
        ["test", "-", "secret", "-", "opaque"].concat()
    }
    const CATALOG: &str = r#"{"revision":1,"run_id":2,"collection_mode":"configured","tags":[]}"#;
    const VALUES: &str =
        r#"{"revision":1,"t":3,"run_id":2,"collection_mode":"configured","values":[]}"#;

    fn server(status: u16, body: &'static str) -> (String, thread::JoinHandle<String>) {
        server_with_headers(status, body, None, body.len())
    }

    fn server_with_headers(
        status: u16,
        body: &'static str,
        location: Option<&'static str>,
        content_length: usize,
    ) -> (String, thread::JoinHandle<String>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            loop {
                let count = stream.read(&mut buffer).unwrap();
                request.extend_from_slice(&buffer[..count]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let location_header = location
                .map(|value| format!("Location: {value}\r\n"))
                .unwrap_or_default();
            let response = format!(
                "HTTP/1.1 {status} Test\r\n{location_header}Content-Length: {content_length}\r\nConnection: close\r\n\r\n{body}",
            );
            stream.write_all(response.as_bytes()).unwrap();
            String::from_utf8(request).unwrap()
        });
        (address, handle)
    }

    fn client(address: String) -> RestClient {
        RestClient::new(
            Endpoint::new(format!("{address}/private-prefix")).unwrap(),
            SecretApiKey::new(test_secret()).unwrap(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn catalog_and_values_are_deserialized_and_query_is_encoded() {
        let (address, catalog_server) = server(200, CATALOG);
        assert_eq!(client(address).fetch_catalog().await.unwrap().revision, 1);
        let request = catalog_server.join().unwrap();
        assert!(request.contains("GET /private-prefix/api/v1/tags HTTP/1.1"));
        assert!(request.contains(&format!("authorization: Bearer {}", test_secret())));

        let (address, values_server) = server(200, VALUES);
        assert_eq!(
            client(address)
                .fetch_values(&["room A", "line/2"])
                .await
                .unwrap()
                .revision,
            1
        );
        assert!(values_server
            .join()
            .unwrap()
            .contains("GET /private-prefix/api/v1/values?tags=room+A%2Cline%2F2 HTTP/1.1"));
    }

    #[tokio::test]
    async fn empty_selection_is_explicit_and_comma_names_fail_closed_before_io() {
        let (address, handle) = server(200, VALUES);
        client(address).fetch_values(&[]).await.unwrap();
        assert!(handle.join().unwrap().contains("/values?tags= HTTP/1.1"));

        let error = client("http://127.0.0.1:1".to_owned())
            .fetch_values(&["ambiguous,name"])
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidTagSelection);
    }

    #[tokio::test]
    async fn status_and_protocol_errors_are_stably_classified() {
        for (status, kind) in [
            (401, ErrorKind::Unauthorized),
            (403, ErrorKind::Unauthorized),
            (500, ErrorKind::CatalogUnavailable),
        ] {
            let (address, handle) = server(status, CATALOG);
            assert_eq!(
                client(address).fetch_catalog().await.unwrap_err().kind(),
                kind
            );
            assert!(handle
                .join()
                .unwrap()
                .contains("GET /private-prefix/api/v1/tags HTTP/1.1"));
        }
        let (address, handle) = server_with_headers(
            302,
            "secret-body-token",
            Some("http://127.0.0.1:1/redirect-target"),
            "secret-body-token".len(),
        );
        let error = client(address).fetch_catalog().await.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidEndpoint);
        let error_debug = format!("{error:?}");
        let error_display = error.to_string();
        assert!(!error_debug.contains("secret-body-token"));
        assert!(!error_debug.contains("private-prefix"));
        assert!(!error_display.contains("secret-body-token"));
        assert!(!error_display.contains("private-prefix"));
        assert!(handle.join().is_ok());

        let (address, _) = server_with_headers(
            302,
            "redirect-body",
            Some("http://127.0.0.1:1/redirect-target"),
            "redirect-body".len(),
        );
        assert_eq!(
            client(address)
                .fetch_values(&["tag"])
                .await
                .unwrap_err()
                .kind(),
            ErrorKind::InvalidEndpoint
        );

        for status in [401, 403] {
            let (address, _) = server(status, VALUES);
            assert_eq!(
                client(address)
                    .fetch_values(&["tag"])
                    .await
                    .unwrap_err()
                    .kind(),
                ErrorKind::Unauthorized
            );
        }
        let (address, _) = server(500, VALUES);
        assert_eq!(
            client(address)
                .fetch_values(&["tag"])
                .await
                .unwrap_err()
                .kind(),
            ErrorKind::Transport
        );
        let (address, _) = server(200, "not-json");
        assert_eq!(
            client(address).fetch_catalog().await.unwrap_err().kind(),
            ErrorKind::ProtocolError
        );

        let (address, handle) = server_with_headers(200, "secret-malformed-body", None, 999);
        let error = client(address).fetch_catalog().await.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Transport);
        let error_debug = format!("{error:?}");
        let error_display = error.to_string();
        assert!(!error_debug.contains("secret-malformed-body"));
        assert!(!error_debug.contains("private-prefix"));
        assert!(!error_display.contains("secret-malformed-body"));
        assert!(!error_display.contains("private-prefix"));
        assert!(handle.join().is_ok());
    }

    #[tokio::test]
    async fn secret_path_and_body_are_redacted_from_public_surfaces() {
        let endpoint = Endpoint::new("http://example.test/private-prefix").unwrap();
        let client = RestClient::new(endpoint, SecretApiKey::new(test_secret()).unwrap()).unwrap();
        let debug = format!("{client:?}");
        assert!(!debug.contains(&test_secret()) && !debug.contains("private-prefix"));
        assert!(!Error::new(ErrorKind::ProtocolError)
            .to_string()
            .contains("not-json"));
    }

    #[tokio::test]
    async fn connection_failure_is_transport() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        assert_eq!(
            client(address).fetch_catalog().await.unwrap_err().kind(),
            ErrorKind::Transport
        );
    }

    #[tokio::test]
    async fn connection_refused_diagnostic_omits_secret_and_path() {
        let (log, _guard) = crate::test_support::capture();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let error = client(address).fetch_catalog().await.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Transport);
        assert!(!log.contains(&test_secret()));
        assert!(!log.contains("private-prefix"));
        // The diagnostic exists and carries the operation name, so this test
        // would fail if the logging call were ever silently dropped.
        assert!(log.contains("fetch_catalog"));
    }

    #[tokio::test]
    async fn unauthorized_diagnostic_omits_secret_and_path() {
        let (log, _guard) = crate::test_support::capture();
        let (address, handle) = server(401, "");
        let error = client(address).fetch_catalog().await.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Unauthorized);
        assert!(!log.contains(&test_secret()));
        assert!(!log.contains("private-prefix"));
        assert!(log.contains("401"));
        handle.join().unwrap();
    }

    #[tokio::test]
    async fn malformed_json_diagnostic_omits_body_secret_and_path() {
        let (log, _guard) = crate::test_support::capture();
        let (address, handle) = server(200, "not-json-super-secret-body");
        let error = client(address).fetch_catalog().await.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ProtocolError);
        assert!(!log.contains("not-json-super-secret-body"));
        assert!(!log.contains(&test_secret()));
        assert!(!log.contains("private-prefix"));
        handle.join().unwrap();
    }

    #[tokio::test]
    async fn redirect_diagnostic_omits_location_secret_and_path() {
        let (log, _guard) = crate::test_support::capture();
        let (address, handle) = server_with_headers(
            302,
            "secret-body-token",
            Some("http://127.0.0.1:1/redirect-target"),
            "secret-body-token".len(),
        );
        let error = client(address).fetch_catalog().await.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidEndpoint);
        assert!(!log.contains("secret-body-token"));
        assert!(!log.contains(&test_secret()));
        assert!(!log.contains("private-prefix"));
        assert!(!log.contains("redirect-target"));
        handle.join().unwrap();
    }
}
