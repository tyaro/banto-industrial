//! Direct, authenticated WebSocket handshake and bounded stream exchange.
//!
//! The connection remains crate-private; the S3a handle owns its lifetime.
//! Rebinding and reconnect remain outside this slice.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{
        client::IntoClientRequest,
        protocol::{Message, WebSocketConfig},
        Error as WsError,
    },
    WebSocketStream,
};

use crate::{
    endpoint::Endpoint,
    error::{Error, ErrorKind, Result},
    secret::SecretApiKey,
    stream_core::validate_tag_selection,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_WEBSOCKET_SIZE: usize = 1024 * 1024;

pub(crate) struct WebSocketConnection {
    stream: WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    host: Option<String>,
    port: Option<u16>,
}

pub(crate) async fn connect(
    endpoint: &Endpoint,
    secret: &SecretApiKey,
) -> Result<WebSocketConnection> {
    connect_with_timeout(endpoint, secret, CONNECT_TIMEOUT).await
}

async fn connect_with_timeout(
    endpoint: &Endpoint,
    secret: &SecretApiKey,
    timeout: Duration,
) -> Result<WebSocketConnection> {
    let url = endpoint.stream_url()?;
    let request = url
        .as_str()
        .into_client_request()
        .map_err(|_| Error::new(ErrorKind::InvalidEndpoint))?;
    let request = secret.apply_stream_authorization(request)?;
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_WEBSOCKET_SIZE))
        .max_frame_size(Some(MAX_WEBSOCKET_SIZE))
        .max_write_buffer_size(MAX_WEBSOCKET_SIZE);
    let host = endpoint.host().map(str::to_owned);
    let port = endpoint.port();
    let result = tokio::time::timeout(
        timeout,
        connect_async_with_config(request, Some(config), false),
    )
    .await
    .map_err(|_| {
        tracing::warn!(
            host = ?host,
            port = ?port,
            timeout_ms = timeout.as_millis() as u64,
            "banto-hub WebSocket connect timed out"
        );
        Error::new(ErrorKind::Transport)
    })?;
    let (stream, _) = result.map_err(|error| {
        log_handshake_error(host.as_deref(), port, &error);
        classify_handshake_error(error)
    })?;
    Ok(WebSocketConnection { stream, host, port })
}

/// Log a safe, secret-free diagnostic for a WebSocket handshake failure.
///
/// Only the error's classification (status code, protocol violation kind, or
/// underlying `io::ErrorKind`) is recorded, never `WsError`'s own
/// `Display`/`Debug`, which can echo the handshake URL (path prefix) or a
/// server-controlled response body back into the log.
fn log_handshake_error(host: Option<&str>, port: Option<u16>, error: &WsError) {
    match error {
        WsError::Http(response) => {
            tracing::warn!(
                host = ?host,
                port = ?port,
                status = response.status().as_u16(),
                "banto-hub WebSocket handshake was rejected"
            );
        }
        WsError::Io(io_error) => {
            tracing::warn!(
                host = ?host,
                port = ?port,
                io_kind = ?io_error.kind(),
                "banto-hub WebSocket connection failed at the transport layer"
            );
        }
        WsError::Tls(_) => {
            tracing::warn!(
                host = ?host,
                port = ?port,
                "banto-hub WebSocket TLS handshake failed"
            );
        }
        WsError::Url(_) => {
            tracing::warn!(
                host = ?host,
                port = ?port,
                "banto-hub WebSocket URL was rejected before connecting"
            );
        }
        WsError::ConnectionClosed | WsError::AlreadyClosed => {
            tracing::debug!(
                host = ?host,
                port = ?port,
                "banto-hub WebSocket connection was already closed during handshake"
            );
        }
        WsError::Capacity(_)
        | WsError::Protocol(_)
        | WsError::WriteBufferFull(_)
        | WsError::Utf8(_)
        | WsError::AttackAttempt
        | WsError::HttpFormat(_) => {
            tracing::warn!(
                host = ?host,
                port = ?port,
                "banto-hub WebSocket handshake violated the protocol"
            );
        }
    }
}

/// Log a safe, secret-free diagnostic for a post-handshake WebSocket
/// send/receive failure. Same redaction rule as [`log_handshake_error`]:
/// never format `WsError` itself.
fn log_stream_error(
    host: Option<&str>,
    port: Option<u16>,
    operation: &'static str,
    error: &WsError,
) {
    match error {
        WsError::Io(io_error) => {
            tracing::warn!(
                host = ?host,
                port = ?port,
                operation,
                io_kind = ?io_error.kind(),
                "banto-hub WebSocket connection failed at the transport layer"
            );
        }
        WsError::Tls(_) => {
            tracing::warn!(
                host = ?host,
                port = ?port,
                operation,
                "banto-hub WebSocket TLS error"
            );
        }
        WsError::ConnectionClosed | WsError::AlreadyClosed => {
            tracing::debug!(
                host = ?host,
                port = ?port,
                operation,
                "banto-hub WebSocket connection was already closed"
            );
        }
        WsError::Capacity(_)
        | WsError::Protocol(_)
        | WsError::WriteBufferFull(_)
        | WsError::Utf8(_)
        | WsError::AttackAttempt
        | WsError::Url(_)
        | WsError::Http(_)
        | WsError::HttpFormat(_) => {
            tracing::warn!(
                host = ?host,
                port = ?port,
                operation,
                "banto-hub WebSocket message violated the protocol"
            );
        }
    }
}

#[derive(Serialize)]
struct SubscribeRequest<'a> {
    op: &'static str,
    id: i64,
    tags: &'a [String],
    mode: &'static str,
}

impl WebSocketConnection {
    pub(crate) async fn close_best_effort(&mut self) {
        let close = async {
            let _ = self.stream.send(Message::Close(None)).await;
            let _ = self.stream.next().await;
        };
        let _ = tokio::time::timeout(CLOSE_TIMEOUT, close).await;
    }

    /// Send one exact Hub `on_change` subscription request.
    pub(crate) async fn subscribe_on_change(
        &mut self,
        subscription_id: i64,
        tags: &[String],
    ) -> Result<()> {
        validate_tag_selection(tags)?;
        let request = SubscribeRequest {
            op: "subscribe",
            id: subscription_id,
            tags,
            mode: "on_change",
        };
        let payload =
            serde_json::to_string(&request).map_err(|_| Error::new(ErrorKind::ProtocolError))?;
        self.stream
            .send(Message::Text(payload.into()))
            .await
            .map_err(|error| {
                log_stream_error(self.host.as_deref(), self.port, "subscribe", &error);
                classify_stream_error(error)
            })
    }

    /// Receive exactly one application text frame, handling native control
    /// frames without adding an unbounded queue.
    pub(crate) async fn receive_text(&mut self) -> Result<String> {
        loop {
            match self.stream.next().await {
                Some(Ok(Message::Text(text))) => return Ok(text.to_string()),
                Some(Ok(Message::Ping(_))) => {
                    self.stream.flush().await.map_err(|error| {
                        log_stream_error(self.host.as_deref(), self.port, "flush_pong", &error);
                        classify_stream_error(error)
                    })?;
                }
                Some(Ok(Message::Pong(_))) => {}
                Some(Ok(Message::Binary(_))) | Some(Ok(Message::Frame(_))) => {
                    tracing::warn!(
                        host = ?self.host,
                        port = ?self.port,
                        "banto-hub sent an unexpected binary WebSocket frame"
                    );
                    return Err(Error::new(ErrorKind::ProtocolError));
                }
                Some(Ok(Message::Close(_))) => {
                    tracing::debug!(
                        host = ?self.host,
                        port = ?self.port,
                        "banto-hub closed the WebSocket connection"
                    );
                    return Err(Error::new(ErrorKind::Transport));
                }
                None => {
                    tracing::warn!(
                        host = ?self.host,
                        port = ?self.port,
                        "banto-hub WebSocket connection ended unexpectedly"
                    );
                    return Err(Error::new(ErrorKind::Transport));
                }
                Some(Err(error)) => {
                    log_stream_error(self.host.as_deref(), self.port, "receive", &error);
                    return Err(classify_stream_error(error));
                }
            }
        }
    }
}

fn classify_stream_error(error: WsError) -> Error {
    match error {
        WsError::Io(_) | WsError::Tls(_) | WsError::ConnectionClosed | WsError::AlreadyClosed => {
            Error::new(ErrorKind::Transport)
        }
        WsError::Protocol(
            tokio_tungstenite::tungstenite::error::ProtocolError::ResetWithoutClosingHandshake,
        ) => Error::new(ErrorKind::Transport),
        WsError::Capacity(_)
        | WsError::Protocol(_)
        | WsError::WriteBufferFull(_)
        | WsError::Utf8(_)
        | WsError::AttackAttempt
        | WsError::Url(_)
        | WsError::Http(_)
        | WsError::HttpFormat(_) => Error::new(ErrorKind::ProtocolError),
    }
}

fn classify_handshake_error(error: WsError) -> Error {
    match error {
        WsError::Http(response) => {
            let status = response.status();
            if status == 401 || status == 403 {
                Error::new(ErrorKind::Unauthorized)
            } else if status.is_redirection() {
                Error::new(ErrorKind::InvalidEndpoint)
            } else if status.is_server_error() {
                Error::new(ErrorKind::Transport)
            } else {
                Error::new(ErrorKind::ProtocolError)
            }
        }
        WsError::Url(_) => Error::new(ErrorKind::InvalidEndpoint),
        WsError::Io(_) | WsError::Tls(_) | WsError::ConnectionClosed | WsError::AlreadyClosed => {
            Error::new(ErrorKind::Transport)
        }
        WsError::Capacity(_)
        | WsError::Protocol(_)
        | WsError::WriteBufferFull(_)
        | WsError::Utf8(_)
        | WsError::AttackAttempt
        | WsError::HttpFormat(_) => Error::new(ErrorKind::ProtocolError),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use futures_util::{SinkExt, StreamExt};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    use tokio_tungstenite::{
        accept_async, accept_hdr_async,
        tungstenite::handshake::server::{Request, Response},
        tungstenite::Message,
    };

    use super::*;
    use crate::{Endpoint, ErrorKind, SecretApiKey};

    const TEST_SECRET: &str = "test-secret-opaque";

    fn client(endpoint: String) -> (Endpoint, SecretApiKey) {
        (
            Endpoint::new(endpoint).unwrap(),
            SecretApiKey::new(TEST_SECRET.to_owned()).unwrap(),
        )
    }

    async fn status_server(status: u16, body: &'static str) -> String {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).await;
            let response = format!(
                "HTTP/1.1 {status} Test\r\nLocation: http://127.0.0.1:1/redirect\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        address
    }

    #[allow(
        clippy::result_large_err,
        reason = "tungstenite test callback uses the upstream ErrorResponse type"
    )]
    #[tokio::test]
    async fn authenticated_handshake_uses_header_not_protocol_or_query() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let observed = Arc::new(Mutex::new(None));
        let observed_by_server = Arc::clone(&observed);
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let callback = move |request: &Request, response: Response| {
                let authorization = request
                    .headers()
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                let protocol = request.headers().get("sec-websocket-protocol").cloned();
                *observed_by_server.lock().unwrap() =
                    Some((authorization, protocol, request.uri().to_string()));
                Ok(response)
            };
            let _ = accept_hdr_async(stream, callback).await;
        });
        let (endpoint, secret) = client(format!("{address}/private-prefix"));
        let connection = connect_with_timeout(&endpoint, &secret, Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(
            connection.stream.get_config().max_message_size,
            Some(MAX_WEBSOCKET_SIZE)
        );
        assert_eq!(
            connection.stream.get_config().max_frame_size,
            Some(MAX_WEBSOCKET_SIZE)
        );
        assert_eq!(
            connection.stream.get_config().max_write_buffer_size,
            MAX_WEBSOCKET_SIZE
        );
        assert!(!connection.stream.get_config().accept_unmasked_frames);
        let observed = observed.lock().unwrap().clone().unwrap();
        assert_eq!(observed.0.as_deref(), Some("Bearer test-secret-opaque"));
        assert!(observed.1.is_none());
        assert_eq!(observed.2, "/private-prefix/api/v1/stream");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn handshake_statuses_are_classified_without_redirect_following() {
        for (status, kind) in [
            (401, ErrorKind::Unauthorized),
            (403, ErrorKind::Unauthorized),
            (400, ErrorKind::ProtocolError),
            (500, ErrorKind::Transport),
            (302, ErrorKind::InvalidEndpoint),
        ] {
            let address = status_server(status, "private-body-token").await;
            let (endpoint, secret) = client(address);
            let error = connect_with_timeout(&endpoint, &secret, Duration::from_secs(2))
                .await
                .err()
                .unwrap();
            assert_eq!(error.kind(), kind);
            assert!(!error.to_string().contains("private-body-token"));
        }
    }

    #[tokio::test]
    async fn timeout_is_transport_and_surfaces_remain_redacted() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).await;
            tokio::time::sleep(Duration::from_millis(100)).await;
            let _ = stream.shutdown().await;
        });
        let (endpoint, secret) = client(format!("{address}/private-path"));
        let error = connect_with_timeout(&endpoint, &secret, Duration::from_millis(20))
            .await
            .err()
            .unwrap();
        assert_eq!(error.kind(), ErrorKind::Transport);
        assert!(!format!("{error:?}").contains(TEST_SECRET));
        server.await.unwrap();
    }

    async fn accept_and_wait_for_subscription(
        listener: TcpListener,
    ) -> tokio_tungstenite::WebSocketStream<tokio::net::TcpStream> {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(stream).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        socket
    }

    #[tokio::test]
    async fn subscribe_on_change_sends_exact_ordered_json() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            match tokio::time::timeout(Duration::from_secs(1), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap()
            {
                Message::Text(text) => text.to_string(),
                _ => panic!("subscription was not text"),
            }
        });
        let (endpoint, secret) = client(address);
        let mut connection = connect_with_timeout(&endpoint, &secret, Duration::from_secs(1))
            .await
            .unwrap();
        let tags = vec!["first".to_owned(), "second".to_owned()];
        connection.subscribe_on_change(1, &tags).await.unwrap();
        assert_eq!(
            server.await.unwrap(),
            r#"{"op":"subscribe","id":1,"tags":["first","second"],"mode":"on_change"}"#
        );
    }

    #[tokio::test]
    async fn invalid_tags_are_rejected_before_any_subscription_frame() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            tokio::time::timeout(Duration::from_millis(100), socket.next())
                .await
                .is_err()
        });
        let (endpoint, secret) = client(address);
        let mut connection = connect_with_timeout(&endpoint, &secret, Duration::from_secs(1))
            .await
            .unwrap();
        for tags in [
            Vec::new(),
            vec![String::new()],
            vec!["  ".to_owned()],
            vec!["bad,tag".to_owned()],
            vec!["same".to_owned(), "same".to_owned()],
        ] {
            assert_eq!(
                connection
                    .subscribe_on_change(1, &tags)
                    .await
                    .unwrap_err()
                    .kind(),
                ErrorKind::InvalidTagSelection
            );
        }
        assert!(server.await.unwrap());
    }

    #[tokio::test]
    async fn first_data_text_is_accepted_by_s2a_publish_gate() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let mut socket = accept_and_wait_for_subscription(listener).await;
            socket
                .send(Message::Text(
                    r#"{"op":"data","id":1,"t":1,"values":[{"tag":"temperature","v":21.5,"q":"good","t":1}]}"#.into(),
                ))
                .await
                .unwrap();
        });
        let (endpoint, secret) = client(address);
        let mut connection = connect_with_timeout(&endpoint, &secret, Duration::from_secs(1))
            .await
            .unwrap();
        let tags = vec!["temperature".to_owned()];
        connection.subscribe_on_change(1, &tags).await.unwrap();
        let text = tokio::time::timeout(Duration::from_secs(1), connection.receive_text())
            .await
            .unwrap()
            .unwrap();
        let mut gate = crate::stream_core::PublishGate::new(1, tags).unwrap();
        gate.accept_wire(&text).unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn native_ping_is_flushed_as_pong_before_next_text() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let mut socket = accept_and_wait_for_subscription(listener).await;
            socket
                .send(Message::Ping(vec![1, 2, 3].into()))
                .await
                .unwrap();
            let pong = tokio::time::timeout(Duration::from_secs(1), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(matches!(pong, Message::Pong(_)));
            socket
                .send(Message::Text("{\"op\":\"pong\"}".into()))
                .await
                .unwrap();
        });
        let (endpoint, secret) = client(address);
        let mut connection = connect_with_timeout(&endpoint, &secret, Duration::from_secs(1))
            .await
            .unwrap();
        connection
            .subscribe_on_change(1, &["tag".to_owned()])
            .await
            .unwrap();
        let text = tokio::time::timeout(Duration::from_secs(1), connection.receive_text())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(text, "{\"op\":\"pong\"}");
        server.await.unwrap();
    }

    async fn connection_for_single_message(
        message: Message,
    ) -> (WebSocketConnection, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let mut socket = accept_and_wait_for_subscription(listener).await;
            socket.send(message).await.unwrap();
        });
        let (endpoint, secret) = client(address);
        let mut connection = connect_with_timeout(&endpoint, &secret, Duration::from_secs(1))
            .await
            .unwrap();
        connection
            .subscribe_on_change(1, &["tag".to_owned()])
            .await
            .unwrap();
        (connection, server)
    }

    #[tokio::test]
    async fn binary_and_oversized_text_are_protocol_errors() {
        let (mut binary, binary_server) =
            connection_for_single_message(Message::Binary(vec![1].into())).await;
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), binary.receive_text())
                .await
                .unwrap()
                .unwrap_err()
                .kind(),
            ErrorKind::ProtocolError
        );
        binary_server.await.unwrap();

        let (mut oversized, oversized_server) =
            connection_for_single_message(Message::Text("x".repeat(MAX_WEBSOCKET_SIZE + 1).into()))
                .await;
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), oversized.receive_text())
                .await
                .unwrap()
                .unwrap_err()
                .kind(),
            ErrorKind::ProtocolError
        );
        oversized_server.await.unwrap();
    }

    #[tokio::test]
    async fn close_and_eof_are_transport_errors() {
        let (mut closed, closed_server) = connection_for_single_message(Message::Close(None)).await;
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), closed.receive_text())
                .await
                .unwrap()
                .unwrap_err()
                .kind(),
            ErrorKind::Transport
        );
        closed_server.await.unwrap();

        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let _ = tokio::time::timeout(Duration::from_secs(1), socket.next()).await;
            drop(socket);
        });
        let (endpoint, secret) = client(address);
        let mut eof = connect_with_timeout(&endpoint, &secret, Duration::from_secs(1))
            .await
            .unwrap();
        eof.subscribe_on_change(1, &["tag".to_owned()])
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), eof.receive_text())
                .await
                .unwrap()
                .unwrap_err()
                .kind(),
            ErrorKind::Transport
        );
        server.await.unwrap();
    }

    #[test]
    fn classification_does_not_retain_source_details() {
        let error = classify_handshake_error(WsError::Io(std::io::Error::other(
            "private-path test-secret-opaque",
        )));
        assert_eq!(error.kind(), ErrorKind::Transport);
        assert!(!error.to_string().contains("private-path"));
        assert!(!error.to_string().contains(TEST_SECRET));
    }

    #[test]
    fn connection_close_errors_are_transport() {
        assert_eq!(
            classify_handshake_error(WsError::ConnectionClosed).kind(),
            ErrorKind::Transport
        );
        assert_eq!(
            classify_handshake_error(WsError::AlreadyClosed).kind(),
            ErrorKind::Transport
        );
    }

    #[tokio::test]
    async fn handshake_rejection_diagnostic_omits_secret_and_path() {
        let (log, _guard) = crate::test_support::capture();
        let address = status_server(401, "unauthorized-body-with-token").await;
        let (endpoint, secret) = client(format!("{address}/private-secret-path"));
        let error = connect_with_timeout(&endpoint, &secret, Duration::from_secs(2))
            .await
            .err()
            .unwrap();
        assert_eq!(error.kind(), ErrorKind::Unauthorized);
        assert!(!log.contains(TEST_SECRET));
        assert!(!log.contains("private-secret-path"));
        assert!(!log.contains("unauthorized-body-with-token"));
        assert!(log.contains("401"));
    }

    #[tokio::test]
    async fn handshake_connection_refused_diagnostic_omits_secret_and_path() {
        let (log, _guard) = crate::test_support::capture();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let (endpoint, secret) = client(format!("{address}/private-refused-path"));
        let error = connect_with_timeout(&endpoint, &secret, Duration::from_secs(2))
            .await
            .err()
            .unwrap();
        assert_eq!(error.kind(), ErrorKind::Transport);
        assert!(!log.contains(TEST_SECRET));
        assert!(!log.contains("private-refused-path"));
    }

    #[tokio::test]
    async fn handshake_timeout_diagnostic_omits_secret_and_path() {
        let (log, _guard) = crate::test_support::capture();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).await;
            tokio::time::sleep(Duration::from_millis(100)).await;
            let _ = stream.shutdown().await;
        });
        let (endpoint, secret) = client(format!("{address}/private-timeout-path"));
        let error = connect_with_timeout(&endpoint, &secret, Duration::from_millis(20))
            .await
            .err()
            .unwrap();
        assert_eq!(error.kind(), ErrorKind::Transport);
        assert!(!log.contains(TEST_SECRET));
        assert!(!log.contains("private-timeout-path"));
        assert!(log.contains("timeout_ms"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn stream_error_diagnostic_omits_secret_and_path() {
        let (log, _guard) = crate::test_support::capture();
        let (mut connection, server) =
            connection_for_single_message(Message::Binary(vec![1].into())).await;
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), connection.receive_text())
                .await
                .unwrap()
                .unwrap_err()
                .kind(),
            ErrorKind::ProtocolError
        );
        assert!(!log.contains(TEST_SECRET));
        server.await.unwrap();
    }

    #[test]
    fn protocol_and_capacity_errors_are_protocol() {
        use tokio_tungstenite::tungstenite::error::{CapacityError, ProtocolError};

        assert_eq!(
            classify_handshake_error(WsError::Protocol(ProtocolError::HandshakeIncomplete)).kind(),
            ErrorKind::ProtocolError
        );
        assert_eq!(
            classify_handshake_error(WsError::Capacity(CapacityError::MessageTooLong {
                size: 2,
                max_size: 1,
            }))
            .kind(),
            ErrorKind::ProtocolError
        );
    }
}
