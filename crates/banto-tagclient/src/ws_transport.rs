//! Direct, authenticated WebSocket handshake for S2b-1.
//!
//! This module deliberately stops after the connection handshake. Subscription,
//! worker/watch delivery, rebinding, reconnect, and shutdown belong to S2b-2/S3.

use std::time::Duration;

use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{client::IntoClientRequest, protocol::WebSocketConfig, Error as WsError},
    WebSocketStream,
};

use crate::{
    endpoint::Endpoint,
    error::{Error, ErrorKind, Result},
    secret::SecretApiKey,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_WEBSOCKET_SIZE: usize = 1024 * 1024;

pub(crate) type WebSocketConnection =
    WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

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
    let result = tokio::time::timeout(
        timeout,
        connect_async_with_config(request, Some(config), false),
    )
    .await
    .map_err(|_| Error::new(ErrorKind::Transport))?;
    let (stream, _) = result.map_err(classify_handshake_error)?;
    Ok(stream)
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

    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    use tokio_tungstenite::{
        accept_hdr_async,
        tungstenite::handshake::server::{Request, Response},
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
            connection.get_config().max_message_size,
            Some(MAX_WEBSOCKET_SIZE)
        );
        assert_eq!(
            connection.get_config().max_frame_size,
            Some(MAX_WEBSOCKET_SIZE)
        );
        assert_eq!(
            connection.get_config().max_write_buffer_size,
            MAX_WEBSOCKET_SIZE
        );
        assert!(!connection.get_config().accept_unmasked_frames);
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
                .unwrap_err();
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
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Transport);
        assert!(!format!("{error:?}").contains(TEST_SECRET));
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
