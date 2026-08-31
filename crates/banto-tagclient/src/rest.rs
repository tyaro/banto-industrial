//! Read-only REST transport for the banto-hub catalog and value snapshot.

use std::fmt;

use reqwest::{Client, ClientBuilder, StatusCode};

use crate::binding::BindingRequest;
use crate::endpoint::Endpoint;
use crate::error::{Error, ErrorKind, Result};
use crate::handle::{validate_start_requests, TagClientHandle};
use crate::secret::SecretApiKey;
use crate::types::{CatalogSnapshot, ValuesSnapshot};
use crate::ws_transport::WebSocketConnection;

/// A read-only banto-hub REST client that connects directly to its endpoint,
/// without redirects or system/environment proxy configuration.
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
            .map_err(|_| Error::new(ErrorKind::Transport))?;
        Ok(Self {
            endpoint,
            secret,
            http,
        })
    }

    /// Start one owned connection generation on the current Tokio runtime.
    ///
    /// Requests are validated before a task or network operation is created.
    /// Reconnect, rebinding, and restart belong to the following slice.
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
            .map_err(|_| Error::new(ErrorKind::Transport))?;
        let status = response.status();
        classify_catalog_status(status)?;
        let body = response
            .bytes()
            .await
            .map_err(|_| Error::new(ErrorKind::Transport))?;
        serde_json::from_slice(&body).map_err(|_| Error::new(ErrorKind::ProtocolError))
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
            .map_err(|_| Error::new(ErrorKind::Transport))?;
        let status = response.status();
        classify_values_status(status)?;
        let body = response
            .bytes()
            .await
            .map_err(|_| Error::new(ErrorKind::Transport))?;
        serde_json::from_slice(&body).map_err(|_| Error::new(ErrorKind::ProtocolError))
    }

    /// Connect the stream using this client's endpoint and secret.
    pub(crate) async fn connect_stream(&self) -> Result<WebSocketConnection> {
        crate::ws_transport::connect(&self.endpoint, &self.secret).await
    }
}

fn classify_catalog_status(status: StatusCode) -> Result<()> {
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        Err(Error::new(ErrorKind::Unauthorized))
    } else if status.is_redirection() {
        Err(Error::new(ErrorKind::InvalidEndpoint))
    } else if !status.is_success() {
        Err(Error::new(ErrorKind::CatalogUnavailable))
    } else {
        Ok(())
    }
}

fn classify_values_status(status: StatusCode) -> Result<()> {
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        Err(Error::new(ErrorKind::Unauthorized))
    } else if status.is_redirection() {
        Err(Error::new(ErrorKind::InvalidEndpoint))
    } else if !status.is_success() {
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
}
