//! Single-tag REST write path (Issue #123 write scope; tag-server-design.md
//! §6 "書き込み経路の安全設計").
//!
//! Two owner decisions (2026-09-01) shape everything in this module:
//!
//! 1. **Single-tag writes only.** Batch/recipe writes are not implemented.
//!    The server's "1 write = 1 request = 1 audit row" invariant (design
//!    §6-3 log-before-write) does not obviously extend to a batch (one audit
//!    row per value, or one for the whole batch? what does a partial failure
//!    report?), and getting that wrong would mean redoing this later. It is
//!    deferred until a real batch/recipe requirement appears.
//! 2. **No automatic retry.** [`RestClient::write_tag`] is a single request:
//!    on any failure it returns immediately with a stable [`ErrorKind`] and
//!    performs no further network I/O. Unlike the read/subscribe path
//!    (`worker.rs`), a write is never idempotent from the PLC's point of
//!    view - resending a "turn on" write because the response was lost could
//!    physically double-actuate a device. Retrying (or not) is entirely the
//!    caller's decision, made with knowledge this crate does not have (was
//!    the physical action already observed?). This is why write does not
//!    reuse `worker.rs`'s reconnect/backoff machinery at all - `write_tag`
//!    does not spawn a task, does not touch `TagClientHandle`, and is not a
//!    generation the supervisor can retry into.
//!
//! Tag selection follows the same convention as read/subscribe: callers pass
//! a [`StableTagId`], never an `external_name` directly, because renames
//! change the external name but not the stable ID (design §4.1). Resolution
//! reuses [`crate::binding::resolve_bindings`] against a freshly fetched
//! catalog - fresh on every write, since a write should always target the
//! name banto-hub considers current, not a name cached from an earlier read.

use reqwest::{Client, StatusCode};
use serde_json::json;

use crate::binding::{resolve_bindings, BindingRequest};
use crate::endpoint::Endpoint;
use crate::error::{Error, ErrorKind, Result};
use crate::secret::SecretApiKey;
use crate::types::StableTagId;

/// A dummy binding key for the single-request resolution
/// [`resolve_write_target`] performs; write has no caller-chosen binding
/// keys of its own; `resolve_bindings` requires one; only its outcome
/// (resolved vs. unresolved) matters here, never the key.
const WRITE_BINDING_KEY: &str = "write";

/// The engineering-value payload for a write, mirroring banto-hub's wire `v`
/// field (`apps/banto-hub/core/src/rest.rs` `WriteValueRequest`/
/// `RequestedValue`). There is deliberately no implicit bool<->number
/// coercion: banto-hub itself rejects a data_type mismatch with 422
/// `unsupported_value_type`, and this type preserves that distinction all the
/// way from the caller instead of collapsing it before the request is sent.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum RequestedValue {
    /// For numeric tags.
    Num(f64),
    /// For bit tags (design §6.1 bit-in-word tags included).
    Bool(bool),
}

impl RequestedValue {
    fn to_json(self) -> serde_json::Value {
        match self {
            RequestedValue::Num(value) => json!(value),
            RequestedValue::Bool(value) => json!(value),
        }
    }
}

/// Resolve one [`StableTagId`] to its current `external_name` against
/// `catalog`, reusing [`resolve_bindings`] rather than a bespoke lookup so
/// duplicate-ID catalogs fail closed the same way read/subscribe binding
/// does (see `binding.rs` module doc).
pub(crate) fn resolve_write_target(
    stable_id: StableTagId,
    catalog: &[crate::types::CatalogTag],
) -> Result<String> {
    let request = BindingRequest {
        binding_key: WRITE_BINDING_KEY.to_owned(),
        stable_id,
    };
    let resolution = resolve_bindings(std::slice::from_ref(&request), catalog)?;
    match resolution.resolved.into_iter().next() {
        Some(resolved) => Ok(resolved.external_name),
        None => Err(Error::new(ErrorKind::BindingUnresolved)),
    }
}

/// Send exactly one `POST /api/v1/values/{external_name}` and classify the
/// response. No retry of any kind happens here or in any caller (module doc
/// point 2) - a single failed send is a single returned [`Error`].
pub(crate) async fn send_write(
    http: &Client,
    endpoint: &Endpoint,
    secret: &SecretApiKey,
    external_name: &str,
    value: RequestedValue,
) -> Result<()> {
    let url = endpoint.value_url(external_name);
    let body = serde_json::to_vec(&json!({ "v": value.to_json() })).map_err(|_| {
        tracing::warn!("banto-hub write request body failed to serialize");
        Error::new(ErrorKind::Transport)
    })?;
    let request = http
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body);
    let response = secret
        .apply_authorization(request)
        .send()
        .await
        .map_err(|error| log_write_send_failure(endpoint, &error))?;
    classify_write_status(endpoint, response.status())
}

/// Log a safe, secret-free diagnostic for a failed write send and return the
/// stable `transport` classification (see `rest.rs`'s `log_send_failure` for
/// the same redaction contract: never the `reqwest::Error`'s own
/// `Display`/`Debug`, which echoes the request URL/path back).
fn log_write_send_failure(endpoint: &Endpoint, error: &reqwest::Error) -> Error {
    tracing::warn!(
        host = ?endpoint.host(),
        port = ?endpoint.port(),
        timeout = error.is_timeout(),
        connect = error.is_connect(),
        "banto-hub write request failed"
    );
    Error::new(ErrorKind::Transport)
}

/// Map the write endpoint's HTTP status to a stable [`ErrorKind`]
/// (tag-server-design.md §6 gates 1-8; exact codes cross-checked against
/// `apps/banto-hub/core/src/rest.rs` `write_rejection_response` and
/// `apps/banto-hub/core/src/write_path.rs` `WriteRejection::rest_error_code`,
/// 2026-09-01). Only the status is used, never the response body - the body
/// can carry a `detail` string built from request-derived text, and this
/// crate does not put arbitrary server text into its public error surface.
///
/// - 403 and 503 are kept apart deliberately (design instruction, 2026-09-01
///   owner decision): 403 is a configuration/permission problem
///   (`not_writable` / `missing_write_scope` / `session_token_cannot_write` /
///   `key_tripped`) that will not go away on retry, while 503 is a transient
///   server state (`writes_disabled` / `collection_not_running` /
///   `simulation_write_rejected`) that may clear on its own.
/// - 401 reuses [`ErrorKind::Unauthorized`], matching the read path's
///   treatment of an outright-rejected credential.
/// - 404/409/422/429/501/502 collapse into [`ErrorKind::WriteRejected`]: each
///   means the request as constructed cannot succeed, but none of them are
///   the write-specific 403/503 split this task exists to make.
fn classify_write_status(endpoint: &Endpoint, status: StatusCode) -> Result<()> {
    if status.is_success() {
        return Ok(());
    }
    let kind = match status {
        StatusCode::UNAUTHORIZED => ErrorKind::Unauthorized,
        StatusCode::FORBIDDEN => ErrorKind::WriteForbidden,
        StatusCode::SERVICE_UNAVAILABLE => ErrorKind::WriteUnavailable,
        StatusCode::NOT_FOUND
        | StatusCode::CONFLICT
        | StatusCode::UNPROCESSABLE_ENTITY
        | StatusCode::TOO_MANY_REQUESTS
        | StatusCode::NOT_IMPLEMENTED
        | StatusCode::BAD_GATEWAY => ErrorKind::WriteRejected,
        status if status.is_redirection() => ErrorKind::InvalidEndpoint,
        _ => ErrorKind::Transport,
    };
    tracing::warn!(
        host = ?endpoint.host(),
        port = ?endpoint.port(),
        status = u64::from(status.as_u16()),
        error_kind = kind.as_str(),
        "banto-hub write request was rejected"
    );
    Err(Error::new(kind))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CatalogTag, ValueSource};

    fn catalog_entry(id: StableTagId, name: &str) -> CatalogTag {
        CatalogTag {
            external_name: name.to_owned(),
            tag_key: format!("tag:{name}"),
            ids: id,
            connection: "connection-a".into(),
            group: "group-a".into(),
            name: name.into(),
            address: "address-a".into(),
            data_type: "f64".into(),
            unit: None,
            decimals: 0,
            period_ms: 100,
            enabled: true,
            writable: true,
            tag_kind: "plc".into(),
            expression: None,
            retain: false,
            simulation: false,
            configured_simulation: false,
            effective_simulation: false,
            value_source: ValueSource::Real,
        }
    }

    #[test]
    fn resolves_a_known_stable_id_to_its_current_external_name() {
        let id = StableTagId::new(1, 2, 3);
        let catalog = [catalog_entry(id, "line1.fast.temp01")];
        assert_eq!(
            resolve_write_target(id, &catalog).unwrap(),
            "line1.fast.temp01"
        );
    }

    #[test]
    fn unknown_stable_id_is_binding_unresolved() {
        let catalog = [catalog_entry(StableTagId::new(1, 2, 3), "other")];
        let error = resolve_write_target(StableTagId::new(9, 9, 9), &catalog).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::BindingUnresolved);
    }

    #[test]
    fn duplicate_catalog_stable_id_fails_closed() {
        let id = StableTagId::new(1, 2, 3);
        let catalog = [catalog_entry(id, "a"), catalog_entry(id, "b")];
        let error = resolve_write_target(id, &catalog).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::DuplicateCatalogStableId);
    }

    #[test]
    fn requested_value_serializes_bool_and_number_distinctly() {
        assert_eq!(RequestedValue::Bool(true).to_json(), json!(true));
        assert_eq!(RequestedValue::Num(1.0).to_json(), json!(1.0));
        // A bit tag's `true` must never collapse to the number `1` before
        // banto-hub's own data_type symmetry check (gate 7) sees it.
        assert_ne!(RequestedValue::Bool(true).to_json(), json!(1));
    }

    #[test]
    fn status_403_and_503_are_classified_distinctly() {
        let endpoint = Endpoint::new("http://example.test").unwrap();
        assert_eq!(
            classify_write_status(&endpoint, StatusCode::FORBIDDEN)
                .unwrap_err()
                .kind(),
            ErrorKind::WriteForbidden
        );
        assert_eq!(
            classify_write_status(&endpoint, StatusCode::SERVICE_UNAVAILABLE)
                .unwrap_err()
                .kind(),
            ErrorKind::WriteUnavailable
        );
    }

    #[test]
    fn other_write_time_rejections_collapse_to_write_rejected() {
        let endpoint = Endpoint::new("http://example.test").unwrap();
        for status in [
            StatusCode::NOT_FOUND,
            StatusCode::CONFLICT,
            StatusCode::UNPROCESSABLE_ENTITY,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::NOT_IMPLEMENTED,
            StatusCode::BAD_GATEWAY,
        ] {
            assert_eq!(
                classify_write_status(&endpoint, status).unwrap_err().kind(),
                ErrorKind::WriteRejected
            );
        }
    }

    #[test]
    fn unauthorized_write_status_matches_read_path_classification() {
        let endpoint = Endpoint::new("http://example.test").unwrap();
        assert_eq!(
            classify_write_status(&endpoint, StatusCode::UNAUTHORIZED)
                .unwrap_err()
                .kind(),
            ErrorKind::Unauthorized
        );
    }

    #[test]
    fn success_status_is_ok() {
        let endpoint = Endpoint::new("http://example.test").unwrap();
        assert!(classify_write_status(&endpoint, StatusCode::OK).is_ok());
    }

    // --- End-to-end `RestClient::write_tag` tests -------------------------
    //
    // These exercise the full path (catalog resolve, then a single POST)
    // through the public `RestClient` API rather than this module's
    // internals directly, so they also prove gate 2 (single-tag; Issue #123
    // does not add a batch endpoint) and the 2026-09-01 no-retry decision at
    // the boundary a caller actually sees.

    use std::time::{Duration, Instant};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::oneshot;

    use crate::rest::RestClient;
    use crate::secret::SecretApiKey;
    use crate::types::{CatalogSnapshot, CollectionMode};

    fn catalog_snapshot(tags: Vec<CatalogTag>) -> CatalogSnapshot {
        CatalogSnapshot {
            revision: 1,
            run_id: Some(7),
            collection_mode: CollectionMode::Configured,
            tags,
        }
    }

    fn write_client(address: String, secret: &str) -> RestClient {
        RestClient::new(
            Endpoint::new(address).unwrap(),
            SecretApiKey::new(secret.into()).unwrap(),
        )
        .unwrap()
    }

    /// Read one full HTTP/1.1 request (headers + any `Content-Length` body)
    /// off `stream`. Unlike the GET-only helpers elsewhere in this crate's
    /// tests, a write POST has a body that is not guaranteed to arrive in
    /// the same read as the header terminator, so this keeps reading until
    /// the declared body length is satisfied.
    async fn read_full_request(stream: &mut TcpStream) -> String {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let count = stream.read(&mut buffer).await.unwrap();
            assert_ne!(count, 0, "peer closed before a full request arrived");
            request.extend_from_slice(&buffer[..count]);
            let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n")
            else {
                continue;
            };
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length: usize = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|value| value.trim().to_owned())
                })
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            if request.len() >= header_end + 4 + content_length {
                return String::from_utf8(request).unwrap();
            }
        }
    }

    async fn respond_json(stream: &mut TcpStream, body: String) {
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    }

    async fn respond_status(stream: &mut TcpStream, status: &str) {
        let response =
            format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        stream.write_all(response.as_bytes()).await.unwrap();
    }

    #[tokio::test]
    async fn write_tag_resolves_the_stable_id_and_posts_the_json_body() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let id = StableTagId::new(1, 2, 3);
        let catalog = serde_json::to_string(&catalog_snapshot(vec![catalog_entry(
            id,
            "line1.fast.temp01",
        )]))
        .unwrap();
        let server = tokio::spawn(async move {
            let (mut catalog_stream, _) = listener.accept().await.unwrap();
            let catalog_request = read_full_request(&mut catalog_stream).await;
            respond_json(&mut catalog_stream, catalog).await;

            let (mut write_stream, _) = listener.accept().await.unwrap();
            let write_request = read_full_request(&mut write_stream).await;
            respond_status(&mut write_stream, "200 OK").await;
            (catalog_request, write_request)
        });

        let client = write_client(address, "write-secret-token");
        client
            .write_tag(id, RequestedValue::Num(25.4))
            .await
            .unwrap();

        let (catalog_request, write_request) = server.await.unwrap();
        assert!(catalog_request.starts_with("GET /api/v1/tags HTTP/1.1"));
        // The write never accepts external_name directly (design §4.1): the
        // path segment below is proof the SDK itself resolved it from the
        // catalog, not something the caller passed through.
        assert!(write_request.starts_with("POST /api/v1/values/line1.fast.temp01 HTTP/1.1"));
        let lower = write_request.to_ascii_lowercase();
        assert!(lower.contains("content-type: application/json"));
        assert!(lower.contains("authorization: bearer write-secret-token"));
        assert!(write_request.contains(r#"{"v":25.4}"#));
    }

    #[tokio::test]
    async fn write_tag_distinguishes_403_forbidden_from_503_unavailable() {
        for (status, expected) in [
            ("403 Forbidden", ErrorKind::WriteForbidden),
            ("503 Service Unavailable", ErrorKind::WriteUnavailable),
        ] {
            let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let address = format!("http://{}", listener.local_addr().unwrap());
            let id = StableTagId::new(1, 2, 3);
            let catalog = serde_json::to_string(&catalog_snapshot(vec![catalog_entry(
                id,
                "line1.fast.temp01",
            )]))
            .unwrap();
            let server = tokio::spawn(async move {
                let (mut catalog_stream, _) = listener.accept().await.unwrap();
                read_full_request(&mut catalog_stream).await;
                respond_json(&mut catalog_stream, catalog).await;
                let (mut write_stream, _) = listener.accept().await.unwrap();
                read_full_request(&mut write_stream).await;
                respond_status(&mut write_stream, status).await;
            });

            let client = write_client(address, "test-token");
            let error = client
                .write_tag(id, RequestedValue::Bool(true))
                .await
                .unwrap_err();
            assert_eq!(error.kind(), expected);
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn write_tag_never_retries_and_returns_immediately_after_a_failed_send() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let id = StableTagId::new(1, 2, 3);
        let catalog = serde_json::to_string(&catalog_snapshot(vec![catalog_entry(
            id,
            "line1.fast.temp01",
        )]))
        .unwrap();
        let (third_tx, third_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut catalog_stream, _) = listener.accept().await.unwrap();
            read_full_request(&mut catalog_stream).await;
            respond_json(&mut catalog_stream, catalog).await;

            let (mut write_stream, _) = listener.accept().await.unwrap();
            read_full_request(&mut write_stream).await;
            respond_status(&mut write_stream, "503 Service Unavailable").await;

            // Owner decision (2026-09-01): a write is never retried
            // automatically. If `write_tag` retried, it would open a third
            // connection here; confirm it never does.
            let arrived = tokio::time::timeout(Duration::from_millis(200), listener.accept())
                .await
                .is_ok();
            let _ = third_tx.send(arrived);
        });

        let client = write_client(address, "test-token");
        let started = Instant::now();
        let error = client
            .write_tag(id, RequestedValue::Num(1.0))
            .await
            .unwrap_err();
        // No backoff/sleep of any kind belongs to this path (worker.rs's
        // smallest configured backoff step is measured in whole seconds) -
        // an immediate return is itself evidence no retry loop ran.
        assert!(started.elapsed() < Duration::from_millis(500));
        assert_eq!(error.kind(), ErrorKind::WriteUnavailable);
        assert!(
            !third_rx.await.unwrap(),
            "write_tag must not open a second write attempt after a failed send"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn write_tag_short_circuits_before_posting_when_catalog_fetch_fails() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let id = StableTagId::new(1, 2, 3);
        let (second_tx, second_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut catalog_stream, _) = listener.accept().await.unwrap();
            read_full_request(&mut catalog_stream).await;
            respond_status(&mut catalog_stream, "401 Unauthorized").await;
            let arrived = tokio::time::timeout(Duration::from_millis(200), listener.accept())
                .await
                .is_ok();
            let _ = second_tx.send(arrived);
        });

        let client = write_client(address, "test-token");
        let error = client
            .write_tag(id, RequestedValue::Num(1.0))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Unauthorized);
        assert!(
            !second_rx.await.unwrap(),
            "an unresolvable/failed catalog fetch must never be followed by a POST"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn write_tag_short_circuits_before_posting_when_stable_id_is_unresolved() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let missing_id = StableTagId::new(9, 9, 9);
        let catalog = serde_json::to_string(&catalog_snapshot(Vec::new())).unwrap();
        let (second_tx, second_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut catalog_stream, _) = listener.accept().await.unwrap();
            read_full_request(&mut catalog_stream).await;
            respond_json(&mut catalog_stream, catalog).await;
            let arrived = tokio::time::timeout(Duration::from_millis(200), listener.accept())
                .await
                .is_ok();
            let _ = second_tx.send(arrived);
        });

        let client = write_client(address, "test-token");
        let error = client
            .write_tag(missing_id, RequestedValue::Bool(true))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::BindingUnresolved);
        assert!(!second_rx.await.unwrap());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn write_rejection_diagnostic_omits_secret_and_path() {
        let (log, _guard) = crate::test_support::capture();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!(
            "http://{}/private-write-prefix",
            listener.local_addr().unwrap()
        );
        let id = StableTagId::new(1, 2, 3);
        let catalog = serde_json::to_string(&catalog_snapshot(vec![catalog_entry(
            id,
            "line1.fast.temp01",
        )]))
        .unwrap();
        let server = tokio::spawn(async move {
            let (mut catalog_stream, _) = listener.accept().await.unwrap();
            read_full_request(&mut catalog_stream).await;
            respond_json(&mut catalog_stream, catalog).await;
            let (mut write_stream, _) = listener.accept().await.unwrap();
            read_full_request(&mut write_stream).await;
            respond_status(&mut write_stream, "403 Forbidden").await;
        });

        let secret = "write-path-secret-value";
        let client = write_client(address, secret);
        let error = client
            .write_tag(id, RequestedValue::Num(1.0))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::WriteForbidden);
        assert!(!log.contains(secret));
        assert!(!log.contains("private-write-prefix"));
        assert!(log.contains("403"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn write_send_failure_diagnostic_omits_secret_and_path() {
        let (log, _guard) = crate::test_support::capture();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!(
            "http://{}/secret-write-path",
            listener.local_addr().unwrap()
        );
        let id = StableTagId::new(1, 2, 3);
        let catalog = serde_json::to_string(&catalog_snapshot(vec![catalog_entry(
            id,
            "line1.fast.temp01",
        )]))
        .unwrap();
        let server = tokio::spawn(async move {
            let (mut catalog_stream, _) = listener.accept().await.unwrap();
            read_full_request(&mut catalog_stream).await;
            respond_json(&mut catalog_stream, catalog).await;
            // Accept the write connection, write an incomplete response,
            // then close - hyper sees EOF mid-response instead of a
            // deterministic status, forcing the client's send to fail at
            // the transport layer without relying on OS-specific RST/FIN
            // timing.
            let (mut write_stream, _) = listener.accept().await.unwrap();
            let mut buffer = [0_u8; 1024];
            let _ = write_stream.read(&mut buffer).await;
            write_stream.write_all(b"HTTP/1.1 500").await.unwrap();
            drop(write_stream);
        });

        let secret = "write-transport-secret";
        let client = write_client(address, secret);
        let error = tokio::time::timeout(
            Duration::from_secs(2),
            client.write_tag(id, RequestedValue::Num(1.0)),
        )
        .await
        .expect("write_tag must not hang on a truncated response")
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Transport);
        assert!(!log.contains(secret));
        assert!(!log.contains("secret-write-path"));
        server.await.unwrap();
    }
}
