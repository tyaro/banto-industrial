//! Public owner for one banto-hub connection generation.
//!
//! This slice owns the supervisor task and exposes only a cloned state snapshot
//! or watch receiver. Rebinding and restart remain an S3b-2 concern.

use std::collections::HashSet;

use tokio::{
    runtime::Handle,
    sync::{oneshot, watch},
    task::{JoinError, JoinHandle},
};

use crate::{
    binding::BindingRequest,
    error::{Error, ErrorKind, Result},
    rest::RestClient,
    types::{TagClientConnectionState, TagClientState},
    worker,
};

const SUBSCRIPTION_ID: i64 = 1;

/// Owns one connection-generation worker and its bounded latest state.
///
/// The handle is intentionally not cloneable. Call [`shutdown`](Self::shutdown)
/// for graceful close and join; dropping the handle is a non-blocking
/// best-effort abort.
pub struct TagClientHandle {
    stop_tx: Option<oneshot::Sender<()>>,
    state_tx: watch::Sender<TagClientState>,
    state_rx: watch::Receiver<TagClientState>,
    task: Option<JoinHandle<Result<()>>>,
    explicit_shutdown: bool,
}

impl TagClientHandle {
    pub(crate) fn spawn(rest: RestClient, requests: Vec<BindingRequest>, runtime: Handle) -> Self {
        Self::spawn_inner(rest, requests, runtime, None)
    }

    #[cfg(test)]
    pub(crate) fn spawn_with_backoff(
        rest: RestClient,
        requests: Vec<BindingRequest>,
        runtime: Handle,
        backoff: worker::BackoffConfig,
    ) -> Self {
        Self::spawn_inner(rest, requests, runtime, Some(backoff))
    }

    fn spawn_inner(
        rest: RestClient,
        requests: Vec<BindingRequest>,
        runtime: Handle,
        backoff: Option<worker::BackoffConfig>,
    ) -> Self {
        let (stop_tx, stop_rx) = oneshot::channel();
        let initial = TagClientState::new(TagClientConnectionState::Stopped);
        let (state_tx, state_rx) = watch::channel(initial);
        let worker_state_tx = state_tx.clone();
        let task = runtime.spawn(async move {
            match backoff {
                Some(backoff) => {
                    worker::run_supervisor_with_config(
                        &rest,
                        &requests,
                        SUBSCRIPTION_ID,
                        &worker_state_tx,
                        stop_rx,
                        backoff,
                    )
                    .await
                }
                None => {
                    worker::run_supervisor(
                        &rest,
                        &requests,
                        SUBSCRIPTION_ID,
                        &worker_state_tx,
                        stop_rx,
                    )
                    .await
                }
            }
        });
        Self {
            stop_tx: Some(stop_tx),
            state_tx,
            state_rx,
            task: Some(task),
            explicit_shutdown: false,
        }
    }

    /// Return the latest state snapshot without exposing the state sender.
    pub fn state(&self) -> TagClientState {
        self.state_rx.borrow().clone()
    }

    /// Subscribe to the bounded latest state slot.
    pub fn state_watch(&self) -> watch::Receiver<TagClientState> {
        self.state_rx.clone()
    }

    /// Request stop, close the socket when connected, and join the worker.
    pub async fn shutdown(mut self) -> Result<()> {
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.send(());
        }
        let joined = match self.task.as_mut() {
            Some(task) => task.await,
            None => Ok(Ok(())),
        };
        if joined.is_err() {
            let mut state = TagClientState::new(TagClientConnectionState::Stopped);
            state.fail(ErrorKind::Transport);
            self.state_tx.send_replace(state);
        }
        self.explicit_shutdown = true;
        classify_join_result(joined)
    }
}

impl Drop for TagClientHandle {
    fn drop(&mut self) {
        if self.explicit_shutdown {
            return;
        }
        self.state_tx
            .send_replace(TagClientState::new(TagClientConnectionState::Stopped));
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

pub(crate) fn validate_start_requests(requests: &[BindingRequest]) -> Result<()> {
    if requests.is_empty() {
        return Err(Error::new(ErrorKind::InvalidTagSelection));
    }
    let mut binding_keys = HashSet::with_capacity(requests.len());
    let mut stable_ids = HashSet::with_capacity(requests.len());
    for request in requests {
        if !binding_keys.insert(request.binding_key.as_str()) {
            return Err(Error::new(ErrorKind::DuplicateBindingKey));
        }
        if !stable_ids.insert(request.stable_id) {
            return Err(Error::new(ErrorKind::DuplicateRequestedStableId));
        }
    }
    Ok(())
}

fn classify_join_result(joined: std::result::Result<Result<()>, JoinError>) -> Result<()> {
    match joined {
        Ok(result) => result,
        Err(_) => Err(Error::new(ErrorKind::Transport)),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futures_util::{SinkExt, StreamExt};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        sync::{oneshot, watch},
    };
    use tokio_tungstenite::{accept_async, tungstenite::Message, WebSocketStream};

    use super::*;
    use crate::{
        endpoint::Endpoint,
        secret::SecretApiKey,
        types::{
            CatalogSnapshot, CatalogTag, CollectionMode, StableTagId, ValueEntry, ValueQuality,
            ValueSource, ValuesSnapshot,
        },
    };

    fn request(key: &str, id: StableTagId) -> BindingRequest {
        BindingRequest {
            binding_key: key.into(),
            stable_id: id,
        }
    }

    fn catalog() -> CatalogSnapshot {
        CatalogSnapshot {
            revision: 1,
            run_id: Some(7),
            collection_mode: CollectionMode::Configured,
            tags: vec![CatalogTag {
                external_name: "alpha".into(),
                tag_key: "key:alpha".into(),
                ids: StableTagId::new(1, 1, 1),
                connection: "connection".into(),
                group: "group".into(),
                name: "alpha".into(),
                address: "address".into(),
                data_type: "f64".into(),
                unit: None,
                decimals: 0,
                period_ms: 100,
                enabled: true,
                writable: false,
                tag_kind: "tag".into(),
                expression: None,
                retain: false,
                simulation: false,
                configured_simulation: false,
                effective_simulation: false,
                value_source: ValueSource::Real,
            }],
        }
    }

    fn values() -> ValuesSnapshot {
        ValuesSnapshot {
            revision: 1,
            t: 1,
            run_id: Some(7),
            collection_mode: CollectionMode::Configured,
            values: vec![ValueEntry {
                tag: "alpha".into(),
                v: Some(1.0),
                q: ValueQuality::Good,
                t: 1,
                value_source: ValueSource::Real,
            }],
        }
    }

    fn client(address: String) -> RestClient {
        RestClient::new(
            Endpoint::new(address).unwrap(),
            SecretApiKey::new("test-token".into()).unwrap(),
        )
        .unwrap()
    }

    async fn read_http_request(stream: &mut TcpStream) {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let count = stream.read(&mut buffer).await.unwrap();
            request.extend_from_slice(&buffer[..count]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                return;
            }
        }
    }

    async fn write_response(stream: &mut TcpStream, status: &str, body: String) {
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    }

    #[derive(Clone, Copy)]
    enum RetryCause {
        Transport,
        Protocol,
        CatalogUnavailable,
    }

    async fn serve_retry_to_live(
        listener: TcpListener,
        first_failure: RetryCause,
        live: oneshot::Sender<()>,
        release: oneshot::Receiver<()>,
    ) {
        match first_failure {
            RetryCause::Transport => {
                let (mut stream, _) = listener.accept().await.unwrap();
                read_http_request(&mut stream).await;
            }
            RetryCause::CatalogUnavailable => {
                let (mut stream, _) = listener.accept().await.unwrap();
                read_http_request(&mut stream).await;
                write_response(&mut stream, "503 Service Unavailable", String::new()).await;
            }
            RetryCause::Protocol => {
                let (mut stream, _) = listener.accept().await.unwrap();
                read_http_request(&mut stream).await;
                write_response(
                    &mut stream,
                    "200 OK",
                    serde_json::to_string(&catalog()).unwrap(),
                )
                .await;
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = accept_async(stream).await.unwrap();
                let _ = tokio::time::timeout(Duration::from_secs(1), socket.next())
                    .await
                    .unwrap();
                socket.send(Message::Text("not-json".into())).await.unwrap();
            }
        }

        let (mut stream, _) = listener.accept().await.unwrap();
        read_http_request(&mut stream).await;
        write_response(
            &mut stream,
            "200 OK",
            serde_json::to_string(&catalog()).unwrap(),
        )
        .await;

        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(stream).await.unwrap();
        let subscription = tokio::time::timeout(Duration::from_secs(1), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(subscription, Message::Text(_)));
        socket
            .send(Message::Text(
                r#"{"op":"data","id":1,"t":1,"values":[{"tag":"alpha","v":1,"q":"good","t":1}]}"#
                    .into(),
            ))
            .await
            .unwrap();

        let (mut stream, _) = listener.accept().await.unwrap();
        read_http_request(&mut stream).await;
        write_response(
            &mut stream,
            "200 OK",
            serde_json::to_string(&values()).unwrap(),
        )
        .await;
        live.send(()).unwrap();
        let _ = release.await;
    }

    async fn serve_first_transport_and_check_no_retry(
        listener: TcpListener,
        second_connection: oneshot::Sender<bool>,
    ) {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_http_request(&mut stream).await;
        drop(stream);
        let connected = tokio::time::timeout(Duration::from_millis(150), listener.accept())
            .await
            .is_ok();
        let _ = second_connection.send(connected);
    }

    async fn serve_unauthorized_and_check_no_retry(
        listener: TcpListener,
        status: &'static str,
        second_connection: oneshot::Sender<bool>,
    ) {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_http_request(&mut stream).await;
        write_response(&mut stream, status, String::new()).await;
        let connected = tokio::time::timeout(Duration::from_millis(150), listener.accept())
            .await
            .is_ok();
        let _ = second_connection.send(connected);
    }

    async fn serve_success(listener: &TcpListener) -> WebSocketStream<TcpStream> {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_http_request(&mut stream).await;
        write_response(
            &mut stream,
            "200 OK",
            serde_json::to_string(&catalog()).unwrap(),
        )
        .await;
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(stream).await.unwrap();
        let subscription = tokio::time::timeout(Duration::from_secs(1), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(subscription, Message::Text(_)));
        socket
            .send(Message::Text(
                r#"{"op":"data","id":1,"t":1,"values":[{"tag":"alpha","v":1,"q":"good","t":1}]}"#
                    .into(),
            ))
            .await
            .unwrap();
        let (mut stream, _) = listener.accept().await.unwrap();
        read_http_request(&mut stream).await;
        write_response(
            &mut stream,
            "200 OK",
            serde_json::to_string(&values()).unwrap(),
        )
        .await;
        socket
    }

    async fn observe_graceful_close(
        mut socket: WebSocketStream<TcpStream>,
        close_seen: oneshot::Sender<bool>,
    ) {
        let message = tokio::time::timeout(Duration::from_secs(2), socket.next()).await;
        let close_frame_seen = match message {
            Ok(Some(Ok(Message::Close(frame)))) => {
                let _ = socket.send(Message::Close(frame)).await;
                true
            }
            _ => false,
        };
        let _ = close_seen.send(close_frame_seen);
    }

    async fn observe_peer_disconnect(
        mut socket: WebSocketStream<TcpStream>,
        peer_closed: oneshot::Sender<bool>,
    ) {
        let message = tokio::time::timeout(Duration::from_secs(2), socket.next()).await;
        let disconnected = matches!(
            message,
            Ok(None) | Ok(Some(Err(_))) | Ok(Some(Ok(Message::Close(_))))
        );
        let _ = peer_closed.send(disconnected);
    }

    async fn serve_generation(
        listener: TcpListener,
        send_initial: bool,
        respond_values: bool,
        require_close_frame: bool,
        ws_ready: oneshot::Sender<()>,
        values_ready: Option<oneshot::Sender<()>>,
        close_seen: oneshot::Sender<bool>,
    ) {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_http_request(&mut stream).await;
        write_response(
            &mut stream,
            "200 OK",
            serde_json::to_string(&catalog()).unwrap(),
        )
        .await;

        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(stream).await.unwrap();
        let subscription = tokio::time::timeout(Duration::from_secs(1), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(subscription, Message::Text(_)));
        let _ = ws_ready.send(());
        if !send_initial {
            if require_close_frame {
                observe_graceful_close(socket, close_seen).await;
            } else {
                observe_peer_disconnect(socket, close_seen).await;
            }
            return;
        }
        socket
            .send(Message::Text(
                r#"{"op":"data","id":1,"t":1,"values":[{"tag":"alpha","v":1,"q":"good","t":1}]}"#
                    .into(),
            ))
            .await
            .unwrap();

        let (mut values_stream, _) = listener.accept().await.unwrap();
        read_http_request(&mut values_stream).await;
        if let Some(values_ready) = values_ready {
            let _ = values_ready.send(());
        }
        if !respond_values {
            if require_close_frame {
                observe_graceful_close(socket, close_seen).await;
            } else {
                observe_peer_disconnect(socket, close_seen).await;
            }
            return;
        }
        write_response(
            &mut values_stream,
            "200 OK",
            serde_json::to_string(&values()).unwrap(),
        )
        .await;
        if require_close_frame {
            observe_graceful_close(socket, close_seen).await;
        } else {
            observe_peer_disconnect(socket, close_seen).await;
        }
    }

    async fn serve_values_wait_for_cancel(
        listener: TcpListener,
        ws_ready: oneshot::Sender<()>,
        values_ready: oneshot::Sender<()>,
        close_frame_seen: oneshot::Sender<bool>,
        peer_closed: oneshot::Sender<bool>,
    ) {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_http_request(&mut stream).await;
        write_response(
            &mut stream,
            "200 OK",
            serde_json::to_string(&catalog()).unwrap(),
        )
        .await;

        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(stream).await.unwrap();
        let subscription = tokio::time::timeout(Duration::from_secs(1), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(subscription, Message::Text(_)));
        let _ = ws_ready.send(());
        socket
            .send(Message::Text(
                r#"{"op":"data","id":1,"t":1,"values":[{"tag":"alpha","v":1,"q":"good","t":1}]}"#
                    .into(),
            ))
            .await
            .unwrap();

        let (mut values_stream, _) = listener.accept().await.unwrap();
        read_http_request(&mut values_stream).await;
        let _ = values_ready.send(());

        let first = tokio::time::timeout(Duration::from_secs(2), socket.next()).await;
        let got_close = matches!(first, Ok(Some(Ok(Message::Close(_)))));
        let _ = close_frame_seen.send(got_close);
        let second = tokio::time::timeout(Duration::from_secs(2), socket.next()).await;
        let closed = matches!(second, Ok(None) | Ok(Some(Err(_))));
        let _ = peer_closed.send(closed);
    }

    async fn wait_live(receiver: &mut watch::Receiver<TagClientState>) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if receiver.borrow().connection_state() == TagClientConnectionState::Live {
                    return;
                }
                receiver.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
    }

    async fn wait_failed(receiver: &mut watch::Receiver<TagClientState>, kind: ErrorKind) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if receiver.borrow().last_error() == Some(kind) {
                    return;
                }
                receiver.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
    }

    async fn wait_reconnecting(receiver: &mut watch::Receiver<TagClientState>, kind: ErrorKind) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let state = receiver.borrow();
                if state.connection_state() == TagClientConnectionState::Reconnecting
                    && state.last_error() == Some(kind)
                    && state.current().is_none()
                {
                    return;
                }
                drop(state);
                receiver.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn join_panic_is_classified_without_source_details() {
        let task = tokio::spawn(async { panic!("test panic") });
        let result = classify_join_result(task.await);
        let error = result.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Transport);
        assert_eq!(error.to_string(), "transport");
    }

    #[tokio::test]
    async fn start_rejects_empty_and_duplicate_requests_before_spawn() {
        let empty = client("http://127.0.0.1:1".into()).start(Vec::new());
        assert_eq!(empty.err().unwrap().kind(), ErrorKind::InvalidTagSelection);

        let duplicate_key = client("http://127.0.0.1:1".into()).start(vec![
            request("same", StableTagId::new(1, 1, 1)),
            request("same", StableTagId::new(1, 1, 2)),
        ]);
        assert_eq!(
            duplicate_key.err().unwrap().kind(),
            ErrorKind::DuplicateBindingKey
        );

        let duplicate_id = client("http://127.0.0.1:1".into()).start(vec![
            request("first", StableTagId::new(1, 1, 1)),
            request("second", StableTagId::new(1, 1, 1)),
        ]);
        assert_eq!(
            duplicate_id.err().unwrap().kind(),
            ErrorKind::DuplicateRequestedStableId
        );
    }

    #[test]
    fn start_without_runtime_returns_transport_without_panicking() {
        let result = client("http://127.0.0.1:1".into())
            .start(vec![request("alpha", StableTagId::new(1, 1, 1))]);
        assert_eq!(result.err().unwrap().kind(), ErrorKind::Transport);
    }

    #[tokio::test]
    async fn state_and_state_watch_start_stopped_without_sensitive_fields() {
        let handle = client("http://127.0.0.1:1".into())
            .start(vec![request("alpha", StableTagId::new(1, 1, 1))])
            .unwrap();
        let state = handle.state();
        assert_eq!(state.connection_state(), TagClientConnectionState::Stopped);
        assert_eq!(state.current(), None);
        assert_eq!(state.last_error(), None);
        assert!(!format!("{state:?}").contains("test-token"));
        assert!(!state.to_string().contains("127.0.0.1"));
        let watched = handle.state_watch();
        assert_eq!(watched.borrow().last_error(), None);
        drop(handle);
    }

    async fn assert_retry_recovers(first_failure: RetryCause, error: ErrorKind) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let (live_tx, live_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let server = tokio::spawn(serve_retry_to_live(
            listener,
            first_failure,
            live_tx,
            release_rx,
        ));
        let handle = TagClientHandle::spawn_with_backoff(
            client(address),
            vec![request("alpha", StableTagId::new(1, 1, 1))],
            Handle::current(),
            worker::BackoffConfig::new(Duration::from_millis(5), Duration::from_millis(20)),
        );
        let mut receiver = handle.state_watch();
        wait_reconnecting(&mut receiver, error).await;
        live_rx.await.unwrap();
        wait_live(&mut receiver).await;
        assert!(receiver.borrow().current().is_some());
        release_tx.send(()).unwrap();
        wait_reconnecting(&mut receiver, ErrorKind::Transport).await;
        assert!(
            tokio::time::timeout(Duration::from_secs(1), handle.shutdown())
                .await
                .unwrap()
                .is_ok()
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn transport_failure_reconnects_from_catalog() {
        assert_retry_recovers(RetryCause::Transport, ErrorKind::Transport).await;
    }

    #[tokio::test]
    async fn protocol_failure_reconnects_from_catalog() {
        assert_retry_recovers(RetryCause::Protocol, ErrorKind::ProtocolError).await;
    }

    #[tokio::test]
    async fn catalog_failure_reconnects_from_catalog() {
        assert_retry_recovers(
            RetryCause::CatalogUnavailable,
            ErrorKind::CatalogUnavailable,
        )
        .await;
    }

    #[tokio::test]
    async fn live_disconnect_restarts_catalog_and_returns_live() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let (first_live_tx, first_live_rx) = oneshot::channel();
        let (drop_first_tx, drop_first_rx) = oneshot::channel();
        let (second_live_tx, second_live_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let first_socket = serve_success(&listener).await;
            first_live_tx.send(()).unwrap();
            drop_first_rx.await.unwrap();
            drop(first_socket);
            let second_socket = serve_success(&listener).await;
            second_live_tx.send(()).unwrap();
            let _ = release_rx.await;
            drop(second_socket);
        });
        let handle = TagClientHandle::spawn_with_backoff(
            client(address),
            vec![request("alpha", StableTagId::new(1, 1, 1))],
            Handle::current(),
            worker::BackoffConfig::new(Duration::from_millis(5), Duration::from_millis(20)),
        );
        let mut receiver = handle.state_watch();
        first_live_rx.await.unwrap();
        wait_live(&mut receiver).await;
        drop_first_tx.send(()).unwrap();
        wait_reconnecting(&mut receiver, ErrorKind::Transport).await;
        second_live_rx.await.unwrap();
        wait_live(&mut receiver).await;
        assert!(receiver.borrow().current().is_some());
        release_tx.send(()).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), handle.shutdown())
                .await
                .unwrap()
                .is_ok()
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_during_backoff_stops_without_an_extra_attempt() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let (second_tx, second_rx) = oneshot::channel();
        let server = tokio::spawn(serve_first_transport_and_check_no_retry(
            listener, second_tx,
        ));
        let handle = TagClientHandle::spawn_with_backoff(
            client(address),
            vec![request("alpha", StableTagId::new(1, 1, 1))],
            Handle::current(),
            worker::BackoffConfig::new(Duration::from_secs(1), Duration::from_secs(30)),
        );
        let mut receiver = handle.state_watch();
        wait_reconnecting(&mut receiver, ErrorKind::Transport).await;
        let result = tokio::time::timeout(Duration::from_millis(200), handle.shutdown())
            .await
            .unwrap();
        assert!(result.is_ok());
        assert_eq!(receiver.borrow().current(), None);
        assert_eq!(receiver.borrow().last_error(), None);
        assert!(!second_rx.await.unwrap());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn drop_during_backoff_clears_state_without_an_extra_attempt() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let (second_tx, second_rx) = oneshot::channel();
        let server = tokio::spawn(serve_first_transport_and_check_no_retry(
            listener, second_tx,
        ));
        let handle = TagClientHandle::spawn_with_backoff(
            client(address),
            vec![request("alpha", StableTagId::new(1, 1, 1))],
            Handle::current(),
            worker::BackoffConfig::new(Duration::from_secs(1), Duration::from_secs(30)),
        );
        let mut receiver = handle.state_watch();
        wait_reconnecting(&mut receiver, ErrorKind::Transport).await;
        drop(handle);
        tokio::time::timeout(Duration::from_millis(200), receiver.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            receiver.borrow().connection_state(),
            TagClientConnectionState::Stopped
        );
        assert_eq!(receiver.borrow().current(), None);
        assert_eq!(receiver.borrow().last_error(), None);
        assert!(!second_rx.await.unwrap());
        server.await.unwrap();
    }

    async fn assert_unauthorized_does_not_retry(status: &'static str) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let (second_tx, second_rx) = oneshot::channel();
        let server = tokio::spawn(serve_unauthorized_and_check_no_retry(
            listener, status, second_tx,
        ));
        let handle = client(address)
            .start(vec![request("alpha", StableTagId::new(1, 1, 1))])
            .unwrap();
        let mut receiver = handle.state_watch();
        wait_failed(&mut receiver, ErrorKind::Unauthorized).await;
        assert_eq!(
            receiver.borrow().connection_state(),
            TagClientConnectionState::Unauthorized
        );
        assert_eq!(receiver.borrow().current(), None);
        assert_eq!(
            receiver.borrow().last_error(),
            Some(ErrorKind::Unauthorized)
        );
        let result = tokio::time::timeout(Duration::from_secs(1), handle.shutdown())
            .await
            .unwrap();
        assert_eq!(result.unwrap_err().kind(), ErrorKind::Unauthorized);
        assert!(!second_rx.await.unwrap());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn unauthorized_401_and_403_are_single_attempt_terminal_failures() {
        assert_unauthorized_does_not_retry("401 Unauthorized").await;
        assert_unauthorized_does_not_retry("403 Forbidden").await;
    }

    #[tokio::test]
    async fn shutdown_is_graceful_and_external_receiver_sees_clean_stopped() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let (ws_ready_tx, ws_ready_rx) = oneshot::channel();
        let (close_seen_tx, close_seen_rx) = oneshot::channel();
        let server = tokio::spawn(serve_generation(
            listener,
            true,
            true,
            true,
            ws_ready_tx,
            None,
            close_seen_tx,
        ));
        let handle = client(address)
            .start(vec![request("alpha", StableTagId::new(1, 1, 1))])
            .unwrap();
        let mut receiver = handle.state_watch();
        wait_live(&mut receiver).await;
        ws_ready_rx.await.unwrap();
        let result = tokio::time::timeout(Duration::from_secs(2), handle.shutdown())
            .await
            .unwrap();
        assert!(result.is_ok());
        assert!(tokio::time::timeout(Duration::from_secs(2), close_seen_rx)
            .await
            .unwrap()
            .unwrap());
        assert_eq!(
            receiver.borrow().connection_state(),
            TagClientConnectionState::Stopped
        );
        assert_eq!(receiver.borrow().current(), None);
        assert_eq!(receiver.borrow().last_error(), None);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_during_initial_data_wait_is_bounded_and_closes_socket() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let (ws_ready_tx, ws_ready_rx) = oneshot::channel();
        let (close_seen_tx, close_seen_rx) = oneshot::channel();
        let server = tokio::spawn(serve_generation(
            listener,
            false,
            false,
            true,
            ws_ready_tx,
            None,
            close_seen_tx,
        ));
        let handle = client(address)
            .start(vec![request("alpha", StableTagId::new(1, 1, 1))])
            .unwrap();
        let receiver = handle.state_watch();
        ws_ready_rx.await.unwrap();
        let result = tokio::time::timeout(Duration::from_secs(2), handle.shutdown())
            .await
            .unwrap();
        assert!(result.is_ok());
        assert!(tokio::time::timeout(Duration::from_secs(2), close_seen_rx)
            .await
            .unwrap()
            .unwrap());
        assert_eq!(receiver.borrow().current(), None);
        assert_eq!(receiver.borrow().last_error(), None);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_during_rest_values_wait_is_bounded_and_closes_socket() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let (ws_ready_tx, ws_ready_rx) = oneshot::channel();
        let (values_ready_tx, values_ready_rx) = oneshot::channel();
        let (close_seen_tx, close_seen_rx) = oneshot::channel();
        let server = tokio::spawn(serve_generation(
            listener,
            true,
            false,
            true,
            ws_ready_tx,
            Some(values_ready_tx),
            close_seen_tx,
        ));
        let handle = client(address)
            .start(vec![request("alpha", StableTagId::new(1, 1, 1))])
            .unwrap();
        let receiver = handle.state_watch();
        ws_ready_rx.await.unwrap();
        values_ready_rx.await.unwrap();
        let result = tokio::time::timeout(Duration::from_secs(2), handle.shutdown())
            .await
            .unwrap();
        assert!(result.is_ok());
        assert!(tokio::time::timeout(Duration::from_secs(2), close_seen_rx)
            .await
            .unwrap()
            .unwrap());
        assert_eq!(receiver.borrow().current(), None);
        assert_eq!(receiver.borrow().last_error(), None);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn cancelling_shutdown_future_drops_handle_and_aborts_worker() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let (ws_ready_tx, ws_ready_rx) = oneshot::channel();
        let (values_ready_tx, values_ready_rx) = oneshot::channel();
        let (close_frame_tx, close_frame_rx) = oneshot::channel();
        let (peer_closed_tx, peer_closed_rx) = oneshot::channel();
        let server = tokio::spawn(serve_values_wait_for_cancel(
            listener,
            ws_ready_tx,
            values_ready_tx,
            close_frame_tx,
            peer_closed_tx,
        ));
        let handle = client(address)
            .start(vec![request("alpha", StableTagId::new(1, 1, 1))])
            .unwrap();
        let mut receiver = handle.state_watch();
        ws_ready_rx.await.unwrap();
        values_ready_rx.await.unwrap();

        let shutdown_task = tokio::spawn(handle.shutdown());
        assert!(tokio::time::timeout(Duration::from_secs(2), close_frame_rx)
            .await
            .unwrap()
            .unwrap());
        shutdown_task.abort();
        assert!(shutdown_task.await.unwrap_err().is_cancelled());

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let state = receiver.borrow();
                if state.connection_state() == TagClientConnectionState::Stopped
                    && state.current().is_none()
                    && state.last_error().is_none()
                {
                    return;
                }
                drop(state);
                receiver.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(2), peer_closed_rx)
            .await
            .unwrap()
            .unwrap());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn failed_generation_preserves_error_for_state_and_shutdown() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_http_request(&mut stream).await;
            write_response(&mut stream, "401 Unauthorized", String::new()).await;
        });
        let handle = client(address)
            .start(vec![request("alpha", StableTagId::new(1, 1, 1))])
            .unwrap();
        let mut receiver = handle.state_watch();
        wait_failed(&mut receiver, ErrorKind::Unauthorized).await;
        assert_eq!(receiver.borrow().current(), None);
        let result = tokio::time::timeout(Duration::from_secs(2), handle.shutdown())
            .await
            .unwrap();
        assert_eq!(result.unwrap_err().kind(), ErrorKind::Unauthorized);
        assert_eq!(
            receiver.borrow().last_error(),
            Some(ErrorKind::Unauthorized)
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn dropping_live_handle_clears_receiver_and_closes_peer() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let (ws_ready_tx, ws_ready_rx) = oneshot::channel();
        let (close_seen_tx, close_seen_rx) = oneshot::channel();
        let server = tokio::spawn(serve_generation(
            listener,
            true,
            true,
            false,
            ws_ready_tx,
            None,
            close_seen_tx,
        ));
        let handle = client(address)
            .start(vec![request("alpha", StableTagId::new(1, 1, 1))])
            .unwrap();
        let mut receiver = handle.state_watch();
        wait_live(&mut receiver).await;
        ws_ready_rx.await.unwrap();
        drop(handle);
        tokio::time::timeout(Duration::from_secs(1), receiver.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            receiver.borrow().connection_state(),
            TagClientConnectionState::Stopped
        );
        assert_eq!(receiver.borrow().current(), None);
        assert_eq!(receiver.borrow().last_error(), None);
        assert!(tokio::time::timeout(Duration::from_secs(2), close_seen_rx)
            .await
            .unwrap()
            .unwrap());
        server.await.unwrap();
    }
}
