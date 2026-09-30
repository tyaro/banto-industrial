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
//! Tags are written **by `external_name`** (`{connection}.{group}.{tag}`),
//! the same key the Hub's write contract (`POST /api/v1/values/{tag}`, the
//! write scope) and every other public contract uses (2026-09-30 owner
//! decision, docs/scada-design.md §9.6). [`RestClient::write_tag`] therefore
//! fetches no catalog: it sends one POST and nothing else. Right after a
//! rename the old name no longer exists, so the Hub answers 404
//! ([`ErrorKind::WriteRejected`]) and nothing is written to the wrong tag.
//! The one case a name cannot distinguish is "a different tag was later
//! registered under the old name" - a write would then land on that tag. This
//! is **accepted by policy, not closed by the SDK** (2026-09-30 owner
//! decision, docs/scada-design.md §9.6): under a name contract it is the same
//! thing as a deliberate delete-and-recreate, and the Hub's per-tag `writable`
//! opt-in (a newly created tag cannot be written), the caller's type check
//! against the catalog, the Hub's rename warning and the write audit are the
//! safeguards. No "expected catalog revision" parameter is planned. This
//! crate does not retry either (point 2 above).

use reqwest::{Client, StatusCode};
use serde_json::json;

use crate::endpoint::Endpoint;
use crate::error::{Error, ErrorKind, Result};
use crate::secret::SecretApiKey;

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

/// Reject a name that cannot identify a tag before any request is sent: empty
/// or whitespace-only, or containing a comma (the same comma rule as
/// `RestClient::fetch_values`, where a comma is ambiguous in the single
/// `tags=` query; here it is refused too so one name always means one tag).
pub(crate) fn validate_write_name(external_name: &str) -> Result<()> {
    if external_name.trim().is_empty() || external_name.contains(',') {
        return Err(Error::new(ErrorKind::InvalidTagSelection));
    }
    Ok(())
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
///   server state (`writes_disabled` / `collection_not_running`) that may
///   clear on its own. (`simulation_write_rejected` was a third 503 code
///   until #363, 2026-09-15, retired it - banto-hub now applies a write to a
///   simulated device to the simulator instead of refusing it.)
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

    #[test]
    fn write_name_validation_rejects_empty_blank_and_comma_names() {
        for name in ["", " ", "\t\n", "a,b", ",", "line1.fast.temp01,"] {
            assert_eq!(
                validate_write_name(name).unwrap_err().kind(),
                ErrorKind::InvalidTagSelection,
                "{name:?}"
            );
        }
        for name in ["line1.fast.temp01", "a b", "温度.1"] {
            assert!(validate_write_name(name).is_ok(), "{name:?}");
        }
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
    // These exercise the full path (a single POST, no catalog fetch) through
    // the public `RestClient` API rather than this module's internals
    // directly, so they also prove gate 2 (single-tag; Issue #123 does not
    // add a batch endpoint) and the 2026-09-01 no-retry decision at the
    // boundary a caller actually sees.

    use std::time::{Duration, Instant};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    use crate::rest::RestClient;
    use crate::secret::SecretApiKey;

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

    async fn respond_status(stream: &mut TcpStream, status: &str) {
        let response =
            format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        stream.write_all(response.as_bytes()).await.unwrap();
    }

    /// Accept one connection, answer it with `status`, then report whether any
    /// further connection arrives within a short window (a retry or an extra
    /// request such as a catalog fetch would open one). Returns the request
    /// text and the "extra connection arrived" flag.
    async fn serve_one_write(listener: TcpListener, status: &'static str) -> (String, bool) {
        let (mut write_stream, _) = listener.accept().await.unwrap();
        let write_request = read_full_request(&mut write_stream).await;
        respond_status(&mut write_stream, status).await;
        let extra = tokio::time::timeout(Duration::from_millis(200), listener.accept())
            .await
            .is_ok();
        (write_request, extra)
    }

    #[tokio::test]
    async fn write_tag_posts_once_by_name_without_fetching_the_catalog() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(serve_one_write(listener, "200 OK"));

        let client = write_client(address, "write-secret-token");
        client
            .write_tag("line1.fast.temp01", RequestedValue::Num(25.4))
            .await
            .unwrap();

        let (write_request, extra_connection) = server.await.unwrap();
        // The first (and only) request is the POST: no `GET /api/v1/tags`
        // precedes it (2026-09-30 owner decision, docs/scada-design.md §9.6).
        assert!(write_request.starts_with("POST /api/v1/values/line1.fast.temp01 HTTP/1.1"));
        assert!(
            !extra_connection,
            "write_tag must send exactly one request (no catalog fetch, no retry)"
        );
        let lower = write_request.to_ascii_lowercase();
        assert!(lower.contains("content-type: application/json"));
        assert!(lower.contains("authorization: bearer write-secret-token"));
        assert!(write_request.contains(r#"{"v":25.4}"#));
    }

    #[tokio::test]
    async fn write_tag_rejects_invalid_names_before_sending_anything() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let client = write_client(address, "test-token");
        for name in ["", "   ", "a,b"] {
            let error = client
                .write_tag(name, RequestedValue::Num(1.0))
                .await
                .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::InvalidTagSelection, "{name:?}");
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(200), listener.accept())
                .await
                .is_err(),
            "an invalid name must never reach the network"
        );
    }

    #[tokio::test]
    async fn write_tag_to_a_name_the_hub_no_longer_knows_is_write_rejected() {
        // Right after a rename the old name is gone: the Hub answers 404 and
        // nothing is written to another tag.
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(serve_one_write(listener, "404 Not Found"));

        let client = write_client(address, "test-token");
        let error = client
            .write_tag("line1.fast.old_name", RequestedValue::Bool(true))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::WriteRejected);
        let (write_request, extra_connection) = server.await.unwrap();
        assert!(write_request.starts_with("POST /api/v1/values/line1.fast.old_name HTTP/1.1"));
        assert!(!extra_connection);
    }

    #[tokio::test]
    async fn write_tag_distinguishes_403_forbidden_from_503_unavailable() {
        for (status, expected) in [
            ("403 Forbidden", ErrorKind::WriteForbidden),
            ("503 Service Unavailable", ErrorKind::WriteUnavailable),
            ("401 Unauthorized", ErrorKind::Unauthorized),
        ] {
            let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let address = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(serve_one_write(listener, status));

            let client = write_client(address, "test-token");
            let error = client
                .write_tag("line1.fast.temp01", RequestedValue::Bool(true))
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
        // Owner decision (2026-09-01): a write is never retried
        // automatically. If `write_tag` retried, `serve_one_write` would see
        // a second connection.
        let server = tokio::spawn(serve_one_write(listener, "503 Service Unavailable"));

        let client = write_client(address, "test-token");
        let started = Instant::now();
        let error = client
            .write_tag("line1.fast.temp01", RequestedValue::Num(1.0))
            .await
            .unwrap_err();
        // No backoff/sleep of any kind belongs to this path (worker.rs's
        // smallest configured backoff step is measured in whole seconds) -
        // an immediate return is itself evidence no retry loop ran.
        assert!(started.elapsed() < Duration::from_millis(500));
        assert_eq!(error.kind(), ErrorKind::WriteUnavailable);
        let (_, extra_connection) = server.await.unwrap();
        assert!(
            !extra_connection,
            "write_tag must not open a second write attempt after a failed send"
        );
    }

    #[tokio::test]
    async fn write_rejection_diagnostic_omits_secret_and_path() {
        let (log, _guard) = crate::test_support::capture();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!(
            "http://{}/private-write-prefix",
            listener.local_addr().unwrap()
        );
        let server = tokio::spawn(async move {
            let (mut write_stream, _) = listener.accept().await.unwrap();
            read_full_request(&mut write_stream).await;
            respond_status(&mut write_stream, "403 Forbidden").await;
        });

        let secret = "write-path-secret-value";
        let client = write_client(address, secret);
        let error = client
            .write_tag("line1.fast.temp01", RequestedValue::Num(1.0))
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
        let server = tokio::spawn(async move {
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
            client.write_tag("line1.fast.temp01", RequestedValue::Num(1.0)),
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
