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
//! decision, docs/scada/scada-design.md §9.6). [`RestClient::write_tag`] therefore
//! fetches no catalog: it sends one POST and nothing else. Right after a
//! rename the old name no longer exists, so the Hub answers 404
//! ([`ErrorKind::WriteRejected`]) and nothing is written to the wrong tag.
//! The one case a name cannot distinguish is "a different tag was later
//! registered under the old name" - a write would then land on that tag. This
//! is **accepted by policy, not closed by the SDK** (2026-09-30 owner
//! decision, docs/scada/scada-design.md §9.6): under a name contract it is the same
//! thing as a deliberate delete-and-recreate, and the Hub's per-tag `writable`
//! opt-in (a newly created tag cannot be written), the caller's type check
//! against the catalog, the Hub's rename warning and the write audit are the
//! safeguards. No "expected catalog revision" parameter is planned. This
//! crate does not retry either (point 2 above).

use reqwest::{Client, StatusCode};
use serde_json::json;

use crate::endpoint::Endpoint;
use crate::error::{Error, ErrorKind, Result, WriteErrorDetail};
use crate::secret::SecretApiKey;

/// Upper bound of the response body this module reads. Only the `error` code
/// (or the receipt) is wanted; anything larger is cut off and treated as
/// unreadable.
const MAX_ERROR_BODY_BYTES: usize = 16 * 1024;
/// Same limits as banto-hub's `WriteMeta::parse` (`write_path.rs`).
const MAX_CORRELATION_ID_CHARS: usize = 64;
const MAX_ON_BEHALF_OF_CHARS: usize = 128;

/// The engineering-value payload for a write, mirroring banto-hub's wire `v`
/// field (`apps/banto-hub/core/src/rest.rs` `WriteValueRequest`/
/// `RequestedValue`). There is deliberately no implicit bool<->number
/// coercion: banto-hub itself rejects a data_type mismatch with 422
/// `unsupported_value_type`, and this type preserves that distinction all the
/// way from the caller instead of collapsing it before the request is sent.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum RequestedValue {
    /// For numeric tags.
    Num(f64),
    /// For bit tags (design §6.1 bit-in-word tags included).
    Bool(bool),
    /// For string tags.
    Text(String),
}

impl RequestedValue {
    fn to_json(&self) -> serde_json::Value {
        match self {
            RequestedValue::Num(value) => json!(value),
            RequestedValue::Bool(value) => json!(value),
            RequestedValue::Text(value) => json!(value),
        }
    }
}

/// Optional per-write metadata (tag-server-design.md §6.6). Both fields are
/// validated here with the same rules as banto-hub before anything is sent;
/// the values are never logged.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct WriteOptions {
    correlation_id: Option<String>,
    on_behalf_of: Option<String>,
}

impl WriteOptions {
    /// Caller-chosen id that ties this write to the caller's own record
    /// (1-64 chars of `[A-Za-z0-9._:-]`).
    #[must_use]
    pub fn with_correlation_id(mut self, correlation_id: impl Into<String>) -> Self {
        self.correlation_id = Some(correlation_id.into());
        self
    }

    /// The operator the caller says it acts for (1-128 chars, no control
    /// characters). The Hub records it as a claim, not as a verified identity.
    #[must_use]
    pub fn with_on_behalf_of(mut self, on_behalf_of: impl Into<String>) -> Self {
        self.on_behalf_of = Some(on_behalf_of.into());
        self
    }

    pub fn correlation_id(&self) -> Option<&str> {
        self.correlation_id.as_deref()
    }

    pub fn on_behalf_of(&self) -> Option<&str> {
        self.on_behalf_of.as_deref()
    }

    /// Same rules as banto-hub's `WriteMeta::parse`.
    pub(crate) fn validate(&self) -> Result<()> {
        if let Some(id) = &self.correlation_id {
            let len = id.chars().count();
            let allowed = id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'));
            if len == 0 || len > MAX_CORRELATION_ID_CHARS || !allowed {
                return Err(Error::new(ErrorKind::InvalidWriteOptions));
            }
        }
        if let Some(who) = &self.on_behalf_of {
            let len = who.chars().count();
            if len == 0 || len > MAX_ON_BEHALF_OF_CHARS || who.chars().any(char::is_control) {
                return Err(Error::new(ErrorKind::InvalidWriteOptions));
            }
        }
        Ok(())
    }
}

/// What a successful (HTTP 200) write returns. A 200 means the write
/// succeeded even when the body could not be read: then `audit_id` is `None`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct WriteReceipt {
    audit_id: Option<i64>,
    correlation_id: Option<String>,
}

impl WriteReceipt {
    /// The Hub's audit row id for this write.
    pub fn audit_id(&self) -> Option<i64> {
        self.audit_id
    }

    /// The `correlation_id` the Hub echoed back.
    pub fn correlation_id(&self) -> Option<&str> {
        self.correlation_id.as_deref()
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
    options: &WriteOptions,
) -> Result<WriteReceipt> {
    options.validate()?;
    let url = endpoint.value_url(external_name);
    let mut payload = json!({ "v": value.to_json() });
    if let Some(map) = payload.as_object_mut() {
        if let Some(id) = &options.correlation_id {
            map.insert("correlation_id".into(), json!(id));
        }
        if let Some(who) = &options.on_behalf_of {
            map.insert("on_behalf_of".into(), json!(who));
        }
    }
    let body = serde_json::to_vec(&payload).map_err(|_| {
        tracing::warn!("banto-hub write request body failed to serialize");
        Error::new(ErrorKind::Transport)
    })?;
    let request = http
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body);
    let mut response = secret
        .apply_authorization(request)
        .send()
        .await
        .map_err(|error| log_write_send_failure(endpoint, &error))?;
    let status = response.status();
    let body = read_limited_body(&mut response).await;
    if status.is_success() {
        return Ok(parse_receipt(endpoint, body.as_deref()));
    }
    let code = body.as_deref().and_then(error_code);
    let detail = body
        .as_deref()
        .and_then(|body| error_detail(body, options.correlation_id.as_deref()));
    classify_write_failure(endpoint, status, code.as_deref())
        .map_err(|error| error.with_detail(detail))
}

/// Read at most [`MAX_ERROR_BODY_BYTES`] of the body. `None` when it could not
/// be read or was larger than the limit.
async fn read_limited_body(response: &mut reqwest::Response) -> Option<Vec<u8>> {
    let mut buffer = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if buffer.len() + chunk.len() > MAX_ERROR_BODY_BYTES {
                    return None;
                }
                buffer.extend_from_slice(&chunk);
            }
            Ok(None) => return Some(buffer),
            Err(_) => return None,
        }
    }
}

/// The `error` code of a failure body; `detail` is deliberately never read.
fn error_code(body: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    value.get("error")?.as_str().map(str::to_owned)
}

/// Build the public [`WriteErrorDetail`] from a failure body. `code` only when
/// it is `[a-z0-9_]{1,64}`; `correlation_id` only when it equals what was sent;
/// `None` when nothing usable was found.
fn error_detail(body: &[u8], sent_correlation_id: Option<&str>) -> Option<WriteErrorDetail> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let code = value
        .get("error")
        .and_then(serde_json::Value::as_str)
        .filter(|code| {
            (1..=64).contains(&code.len())
                && code
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        })
        .map(str::to_owned);
    let audit_id = value.get("audit_id").and_then(serde_json::Value::as_i64);
    let correlation_id = value
        .get("correlation_id")
        .and_then(serde_json::Value::as_str)
        .filter(|echoed| Some(*echoed) == sent_correlation_id)
        .map(str::to_owned);
    if code.is_none() && audit_id.is_none() && correlation_id.is_none() {
        return None;
    }
    Some(WriteErrorDetail::new(code, audit_id, correlation_id))
}

fn parse_receipt(endpoint: &Endpoint, body: Option<&[u8]>) -> WriteReceipt {
    let parsed = body.and_then(|body| serde_json::from_slice::<serde_json::Value>(body).ok());
    let Some(value) = parsed else {
        tracing::warn!(
            host = ?endpoint.host(),
            port = ?endpoint.port(),
            "banto-hub write succeeded but the response body was not readable"
        );
        return WriteReceipt::default();
    };
    WriteReceipt {
        audit_id: value.get("audit_id").and_then(serde_json::Value::as_i64),
        correlation_id: value
            .get("correlation_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
    }
}

/// Log a safe, secret-free diagnostic for a failed write send and classify it
/// (see `rest.rs`'s `log_send_failure` for the same redaction contract: never
/// the `reqwest::Error`'s own `Display`/`Debug`, which echoes the request
/// URL/path back).
///
/// A connect or request-builder failure means the request was never sent:
/// [`ErrorKind::Transport`].
/// Any other failure (timeout after sending, connection cut mid-response, ...)
/// happens after the request may have reached the Hub, so the outcome is
/// unknown: [`ErrorKind::WriteUncertain`].
fn log_write_send_failure(endpoint: &Endpoint, error: &reqwest::Error) -> Error {
    tracing::warn!(
        host = ?endpoint.host(),
        port = ?endpoint.port(),
        timeout = error.is_timeout(),
        connect = error.is_connect(),
        "banto-hub write request failed"
    );
    if error.is_connect() || error.is_builder() {
        Error::new(ErrorKind::Transport)
    } else {
        Error::new(ErrorKind::WriteUncertain)
    }
}

/// Map a failed write response to a stable [`ErrorKind`]
/// (tag-server-design.md §6 gates 1-8 and §6.6; codes cross-checked against
/// `apps/banto-hub/core/src/rest/write.rs` `write_rejection_response`).
/// Only the HTTP status and the body's `error` code are used, never `detail`:
/// it can carry text built from the request, and this crate does not put
/// arbitrary server text into its public error surface. An unreadable or
/// unknown code falls to the status's default below.
///
/// The criterion is "can it be guaranteed that nothing was sent": a server
/// error that cannot guarantee it is [`ErrorKind::WriteUncertain`].
///
/// - 403: `key_tripped` -> [`ErrorKind::KeyTripped`], otherwise
///   [`ErrorKind::WriteForbidden`] (`not_writable` / `missing_write_scope` /
///   `session_token_cannot_write`): a configuration problem that will not go
///   away on retry.
/// - 503 with the Hub's own code (`writes_disabled` / `collection_not_running`)
///   is [`ErrorKind::WriteUnavailable`], a transient state where nothing was
///   sent. A 503 without a readable or known code (a proxy's) is
///   [`ErrorKind::WriteUncertain`].
/// - 504 and any other 5xx not listed here -> [`ErrorKind::WriteUncertain`]
///   (a proxy may have forwarded the POST before timing out).
/// - 502: `write_failed` (the send failed for certain) ->
///   [`ErrorKind::WriteFailed`]; `write_uncertain` and anything else
///   (for example a reverse proxy's own 502) -> [`ErrorKind::WriteUncertain`],
///   because it is not known whether the write reached the device.
/// - 500: `audit_write_failed` (nothing was sent) -> [`ErrorKind::Transport`];
///   `internal` and anything else -> [`ErrorKind::WriteUncertain`].
/// - 401 reuses [`ErrorKind::Unauthorized`].
/// - 404/409/422/429/501 are [`ErrorKind::WriteRejected`].
fn classify_write_failure(
    endpoint: &Endpoint,
    status: StatusCode,
    code: Option<&str>,
) -> Result<WriteReceipt> {
    let kind = match status {
        StatusCode::UNAUTHORIZED => ErrorKind::Unauthorized,
        StatusCode::FORBIDDEN => match code {
            Some("key_tripped") => ErrorKind::KeyTripped,
            _ => ErrorKind::WriteForbidden,
        },
        StatusCode::SERVICE_UNAVAILABLE => match code {
            Some("writes_disabled" | "collection_not_running") => ErrorKind::WriteUnavailable,
            _ => ErrorKind::WriteUncertain,
        },
        StatusCode::BAD_GATEWAY => match code {
            Some("write_failed") => ErrorKind::WriteFailed,
            _ => ErrorKind::WriteUncertain,
        },
        StatusCode::INTERNAL_SERVER_ERROR => match code {
            Some("audit_write_failed") => ErrorKind::Transport,
            _ => ErrorKind::WriteUncertain,
        },
        StatusCode::NOT_FOUND
        | StatusCode::CONFLICT
        | StatusCode::UNPROCESSABLE_ENTITY
        | StatusCode::TOO_MANY_REQUESTS
        | StatusCode::NOT_IMPLEMENTED => ErrorKind::WriteRejected,
        status if status.is_redirection() => ErrorKind::InvalidEndpoint,
        // 504 (a proxy that already forwarded the POST) and any other 5xx:
        // nothing guarantees the Hub did not send the write.
        status if status.is_server_error() => ErrorKind::WriteUncertain,
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
    fn builder_failure_is_transport_because_nothing_was_sent() {
        let endpoint = Endpoint::new("http://example.test").unwrap();
        let error = reqwest::Client::new().get("not a url").build().unwrap_err();
        assert!(error.is_builder());
        assert_eq!(
            log_write_send_failure(&endpoint, &error).kind(),
            ErrorKind::Transport
        );
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
            classify_write_failure(&endpoint, StatusCode::FORBIDDEN, None)
                .unwrap_err()
                .kind(),
            ErrorKind::WriteForbidden
        );
        assert_eq!(
            classify_write_failure(
                &endpoint,
                StatusCode::SERVICE_UNAVAILABLE,
                Some("writes_disabled")
            )
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
        ] {
            assert_eq!(
                classify_write_failure(&endpoint, status, None)
                    .unwrap_err()
                    .kind(),
                ErrorKind::WriteRejected
            );
        }
    }

    #[test]
    fn error_detail_restricts_code_and_checks_correlation_id() {
        let d = error_detail(
            br#"{"error":"rate_limited","audit_id":42,"correlation_id":"c1","detail":"x"}"#,
            Some("c1"),
        )
        .unwrap();
        assert_eq!(d.code(), Some("rate_limited"));
        assert_eq!(d.audit_id(), Some(42));
        assert_eq!(d.correlation_id(), Some("c1"));
        // a mismatching or unsent correlation id is dropped
        let d = error_detail(br#"{"error":"a","correlation_id":"zz"}"#, Some("c1")).unwrap();
        assert_eq!(d.correlation_id(), None);
        let d = error_detail(br#"{"error":"a","correlation_id":"c1"}"#, None).unwrap();
        assert_eq!(d.correlation_id(), None);
        // bad code characters / length
        assert!(error_detail(br#"{"error":"Bad Code"}"#, None).is_none());
        assert!(error_detail(br#"{"error":"<html>"}"#, None).is_none());
        let long = format!(r#"{{"error":"{}"}}"#, "a".repeat(65));
        assert!(error_detail(long.as_bytes(), None).is_none());
        let ok = format!(r#"{{"error":"{}"}}"#, "a".repeat(64));
        assert!(error_detail(ok.as_bytes(), None).is_some());
        // audit_id alone is kept even when the code is unusable
        let d = error_detail(br#"{"error":"NO","audit_id":7}"#, None).unwrap();
        assert_eq!((d.code(), d.audit_id()), (None, Some(7)));
        // not JSON
        assert!(error_detail(b"<html>bad gateway</html>", None).is_none());
        assert!(error_detail(b"", None).is_none());
    }

    #[test]
    fn unauthorized_write_status_matches_read_path_classification() {
        let endpoint = Endpoint::new("http://example.test").unwrap();
        assert_eq!(
            classify_write_failure(&endpoint, StatusCode::UNAUTHORIZED, None)
                .unwrap_err()
                .kind(),
            ErrorKind::Unauthorized
        );
    }

    #[test]
    fn failure_codes_refine_the_status_classification() {
        let endpoint = Endpoint::new("http://example.test").unwrap();
        let cases = [
            (
                StatusCode::FORBIDDEN,
                Some("key_tripped"),
                ErrorKind::KeyTripped,
            ),
            (
                StatusCode::FORBIDDEN,
                Some("not_writable"),
                ErrorKind::WriteForbidden,
            ),
            (StatusCode::FORBIDDEN, None, ErrorKind::WriteForbidden),
            (
                StatusCode::BAD_GATEWAY,
                Some("write_failed"),
                ErrorKind::WriteFailed,
            ),
            (
                StatusCode::BAD_GATEWAY,
                Some("write_uncertain"),
                ErrorKind::WriteUncertain,
            ),
            (
                StatusCode::BAD_GATEWAY,
                Some("???"),
                ErrorKind::WriteUncertain,
            ),
            (StatusCode::BAD_GATEWAY, None, ErrorKind::WriteUncertain),
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Some("audit_write_failed"),
                ErrorKind::Transport,
            ),
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Some("internal"),
                ErrorKind::WriteUncertain,
            ),
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                None,
                ErrorKind::WriteUncertain,
            ),
            (
                StatusCode::NOT_FOUND,
                Some("key_tripped"),
                ErrorKind::WriteRejected,
            ),
        ];
        for (status, code, expected) in cases {
            assert_eq!(
                classify_write_failure(&endpoint, status, code)
                    .unwrap_err()
                    .kind(),
                expected,
                "{status} {code:?}"
            );
        }
    }

    #[test]
    fn requested_text_serializes_as_a_json_string() {
        assert_eq!(
            RequestedValue::Text("ab\"c".into()).to_json(),
            json!("ab\"c")
        );
    }

    #[test]
    fn write_options_validation_follows_the_hub_rules() {
        let long_ok = "x".repeat(64);
        let long_bad = "x".repeat(65);
        for id in ["a", "A.b_c:d-1", long_ok.as_str()] {
            assert!(
                WriteOptions::default()
                    .with_correlation_id(id)
                    .validate()
                    .is_ok(),
                "{id}"
            );
        }
        for id in ["", "has space", "日本", "a/b", long_bad.as_str()] {
            assert_eq!(
                WriteOptions::default()
                    .with_correlation_id(id)
                    .validate()
                    .unwrap_err()
                    .kind(),
                ErrorKind::InvalidWriteOptions
            );
        }
        let who_ok = "é".repeat(128);
        let who_bad = "é".repeat(129);
        for who in ["u", "山田 太郎", who_ok.as_str()] {
            assert!(WriteOptions::default()
                .with_on_behalf_of(who)
                .validate()
                .is_ok());
        }
        for who in ["", "a\nb", "a\u{7}", who_bad.as_str()] {
            assert_eq!(
                WriteOptions::default()
                    .with_on_behalf_of(who)
                    .validate()
                    .unwrap_err()
                    .kind(),
                ErrorKind::InvalidWriteOptions
            );
        }
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
            .write_tag(
                "line1.fast.temp01",
                RequestedValue::Num(25.4),
                WriteOptions::default(),
            )
            .await
            .unwrap();

        let (write_request, extra_connection) = server.await.unwrap();
        // The first (and only) request is the POST: no `GET /api/v1/tags`
        // precedes it (2026-09-30 owner decision, docs/scada/scada-design.md §9.6).
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
                .write_tag(name, RequestedValue::Num(1.0), WriteOptions::default())
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
            .write_tag(
                "line1.fast.old_name",
                RequestedValue::Bool(true),
                WriteOptions::default(),
            )
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
            // A bodyless 503 is a proxy's, not the Hub's: outcome unknown.
            ("503 Service Unavailable", ErrorKind::WriteUncertain),
            ("401 Unauthorized", ErrorKind::Unauthorized),
        ] {
            let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let address = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(serve_one_write(listener, status));

            let client = write_client(address, "test-token");
            let error = client
                .write_tag(
                    "line1.fast.temp01",
                    RequestedValue::Bool(true),
                    WriteOptions::default(),
                )
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
        let server = tokio::spawn(serve_one_write(listener, "504 Gateway Timeout"));

        let client = write_client(address, "test-token");
        let started = Instant::now();
        let error = client
            .write_tag(
                "line1.fast.temp01",
                RequestedValue::Num(1.0),
                WriteOptions::default(),
            )
            .await
            .unwrap_err();
        // No backoff/sleep of any kind belongs to this path (worker.rs's
        // smallest configured backoff step is measured in whole seconds) -
        // an immediate return is itself evidence no retry loop ran.
        assert!(started.elapsed() < Duration::from_millis(500));
        assert_eq!(error.kind(), ErrorKind::WriteUncertain);
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
            .write_tag(
                "line1.fast.temp01",
                RequestedValue::Num(1.0),
                WriteOptions::default(),
            )
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
            client.write_tag(
                "line1.fast.temp01",
                RequestedValue::Num(1.0),
                WriteOptions::default(),
            ),
        )
        .await
        .expect("write_tag must not hang on a truncated response")
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::WriteUncertain);
        assert!(!log.contains(secret));
        assert!(!log.contains("secret-write-path"));
        server.await.unwrap();
    }

    async fn respond_json(stream: &mut TcpStream, status: &str, body: &str) {
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    }

    async fn serve_one_json(listener: TcpListener, status: &'static str, body: String) -> String {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_full_request(&mut stream).await;
        respond_json(&mut stream, status, &body).await;
        request
    }

    async fn write_with_response(
        status: &'static str,
        body: impl Into<String>,
        value: RequestedValue,
        options: WriteOptions,
    ) -> (Result<WriteReceipt>, String) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(serve_one_json(listener, status, body.into()));
        let client = write_client(address, "test-token");
        let result = client.write_tag("line1.fast.temp01", value, options).await;
        (result, server.await.unwrap())
    }

    #[tokio::test]
    async fn write_tag_success_returns_audit_id_and_correlation_id() {
        let (result, request) = write_with_response(
            "200 OK",
            r#"{"tag":"line1.fast.temp01","result":"ok","audit_id":42,"correlation_id":"c-1"}"#,
            RequestedValue::Text("abc".into()),
            WriteOptions::default()
                .with_correlation_id("c-1")
                .with_on_behalf_of("山田"),
        )
        .await;
        let receipt = result.unwrap();
        assert_eq!(receipt.audit_id(), Some(42));
        assert_eq!(receipt.correlation_id(), Some("c-1"));
        let body = request.split("\r\n\r\n").nth(1).unwrap();
        let sent: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(
            sent,
            json!({"v": "abc", "correlation_id": "c-1", "on_behalf_of": "山田"})
        );
    }

    #[tokio::test]
    async fn write_tag_without_options_sends_only_v() {
        let (result, request) = write_with_response(
            "200 OK",
            r#"{"tag":"t","result":"ok","audit_id":1}"#,
            RequestedValue::Bool(true),
            WriteOptions::default(),
        )
        .await;
        let receipt = result.unwrap();
        assert_eq!(receipt.audit_id(), Some(1));
        assert_eq!(receipt.correlation_id(), None);
        assert!(request.ends_with(r#"{"v":true}"#), "{request}");
    }

    #[tokio::test]
    async fn write_tag_200_with_broken_body_is_still_ok_without_audit_id() {
        for body in ["not json", "{}", r#"{"audit_id":"x"}"#, ""] {
            let (result, _) = write_with_response(
                "200 OK",
                body,
                RequestedValue::Num(1.0),
                WriteOptions::default(),
            )
            .await;
            let receipt = result.unwrap();
            assert_eq!(receipt.audit_id(), None, "{body:?}");
            assert_eq!(receipt.correlation_id(), None, "{body:?}");
        }
    }

    #[tokio::test]
    async fn write_tag_invalid_options_send_nothing() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let client = write_client(address, "test-token");
        for options in [
            WriteOptions::default().with_correlation_id("bad id"),
            WriteOptions::default().with_on_behalf_of(""),
        ] {
            let error = client
                .write_tag("line1.fast.temp01", RequestedValue::Num(1.0), options)
                .await
                .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::InvalidWriteOptions);
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(200), listener.accept())
                .await
                .is_err(),
            "invalid options must never reach the network"
        );
    }

    #[tokio::test]
    async fn write_tag_classifies_failure_bodies_by_error_code() {
        let cases = [
            (
                "403 Forbidden",
                r#"{"error":"key_tripped"}"#,
                ErrorKind::KeyTripped,
            ),
            (
                "403 Forbidden",
                r#"{"error":"not_writable"}"#,
                ErrorKind::WriteForbidden,
            ),
            ("403 Forbidden", "garbage", ErrorKind::WriteForbidden),
            (
                "502 Bad Gateway",
                r#"{"error":"write_failed","audit_id":3}"#,
                ErrorKind::WriteFailed,
            ),
            (
                "502 Bad Gateway",
                r#"{"error":"write_uncertain","audit_id":3}"#,
                ErrorKind::WriteUncertain,
            ),
            (
                "502 Bad Gateway",
                "<html>proxy</html>",
                ErrorKind::WriteUncertain,
            ),
            (
                "500 Internal Server Error",
                r#"{"error":"audit_write_failed"}"#,
                ErrorKind::Transport,
            ),
            (
                "500 Internal Server Error",
                r#"{"error":"internal"}"#,
                ErrorKind::WriteUncertain,
            ),
            ("500 Internal Server Error", "", ErrorKind::WriteUncertain),
            (
                "503 Service Unavailable",
                r#"{"error":"writes_disabled"}"#,
                ErrorKind::WriteUnavailable,
            ),
            (
                "503 Service Unavailable",
                r#"{"error":"collection_not_running"}"#,
                ErrorKind::WriteUnavailable,
            ),
            (
                "503 Service Unavailable",
                "<html>proxy</html>",
                ErrorKind::WriteUncertain,
            ),
            ("503 Service Unavailable", "", ErrorKind::WriteUncertain),
            ("504 Gateway Timeout", "", ErrorKind::WriteUncertain),
            (
                "504 Gateway Timeout",
                "<html>gateway timeout</html>",
                ErrorKind::WriteUncertain,
            ),
            ("507 Insufficient Storage", "", ErrorKind::WriteUncertain),
            ("599 Custom", "", ErrorKind::WriteUncertain),
        ];
        for (status, body, expected) in cases {
            let (result, _) = write_with_response(
                status,
                body,
                RequestedValue::Num(1.0),
                WriteOptions::default(),
            )
            .await;
            assert_eq!(result.unwrap_err().kind(), expected, "{status} {body}");
        }
    }

    #[tokio::test]
    async fn write_tag_504_after_post_is_uncertain_and_not_retried() {
        for body in ["", "<html>gateway timeout</html>"] {
            let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let address = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                let mut received = 0_u32;
                while let Ok(Ok((mut stream, _))) =
                    tokio::time::timeout(Duration::from_millis(500), listener.accept()).await
                {
                    read_full_request(&mut stream).await;
                    received += 1;
                    respond_json(&mut stream, "504 Gateway Timeout", body).await;
                }
                received
            });
            let client = write_client(address, "test-token");
            let error = client
                .write_tag(
                    "line1.fast.temp01",
                    RequestedValue::Num(1.0),
                    WriteOptions::default(),
                )
                .await
                .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::WriteUncertain, "{body:?}");
            assert_eq!(server.await.unwrap(), 1, "{body:?}: must not be resent");
        }
    }

    #[tokio::test]
    async fn write_tag_oversized_error_body_falls_back_to_the_status_default() {
        let body = format!(
            r#"{{"error":"key_tripped","detail":"{}"}}"#,
            "x".repeat(20_000)
        );
        let (result, _) = write_with_response(
            "403 Forbidden",
            body,
            RequestedValue::Num(1.0),
            WriteOptions::default(),
        )
        .await;
        assert_eq!(result.unwrap_err().kind(), ErrorKind::WriteForbidden);
    }

    #[tokio::test]
    async fn write_tag_connect_failure_is_transport() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let client = write_client(address, "test-token");
        let error = client
            .write_tag(
                "line1.fast.temp01",
                RequestedValue::Num(1.0),
                WriteOptions::default(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Transport);
    }

    #[tokio::test]
    async fn write_tag_timeout_after_sending_is_write_uncertain() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_full_request(&mut stream).await;
            // Never answer: hold the connection until the client gives up.
            tokio::time::sleep(Duration::from_secs(2)).await;
        });
        let client = RestClient::with_request_timeout(
            Endpoint::new(address).unwrap(),
            SecretApiKey::new("test-token".into()).unwrap(),
            Duration::from_millis(200),
        )
        .unwrap();
        let error = client
            .write_tag(
                "line1.fast.temp01",
                RequestedValue::Num(1.0),
                WriteOptions::default(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::WriteUncertain);
        server.abort();
    }
}
