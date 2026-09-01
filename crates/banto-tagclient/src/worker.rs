//! One connection-generation worker and bounded latest-state publication.
//!
//! The attempt and supervisor are crate-private. S3a provides the public owner
//! for task lifetime and explicit shutdown; S3b-1 adds sequential reconnect and
//! backoff, bounded rebinding, and metadata re-resolution.

use tokio::sync::{oneshot, watch};
use tokio::time::Duration;

use crate::{
    binding::{resolve_bindings, BindingRequest},
    error::{Error, ErrorKind, Result},
    rest::RestClient,
    stream_core::{AcceptedWire, PublishGate},
    types::{TagClientConnectionState, TagClientState},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BackoffConfig {
    base: Duration,
    max: Duration,
}

impl Default for BackoffConfig {
    fn default() -> Self {
        Self {
            base: Duration::from_secs(1),
            max: Duration::from_secs(30),
        }
    }
}

impl BackoffConfig {
    #[cfg(test)]
    pub(crate) const fn new(base: Duration, max: Duration) -> Self {
        Self { base, max }
    }

    fn delay_for(self, retry_number: u32) -> Duration {
        if self.max.is_zero() || self.base.is_zero() {
            return Duration::ZERO;
        }
        let mut delay = if self.base < self.max {
            self.base
        } else {
            self.max
        };
        let mut doublings = retry_number.saturating_sub(1);
        while doublings > 0 && delay < self.max {
            let Some(next) = delay.checked_mul(2) else {
                return self.max;
            };
            if next >= self.max {
                return self.max;
            }
            delay = next;
            doublings -= 1;
        }
        delay
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RetryTracker {
    next_retry_number: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RebindTracker {
    additional_attempts: u8,
}

impl RebindTracker {
    const MAX_ADDITIONAL_ATTEMPTS: u8 = 3;

    const fn new() -> Self {
        Self {
            additional_attempts: 0,
        }
    }

    fn next_attempt(&mut self) -> bool {
        if self.additional_attempts >= Self::MAX_ADDITIONAL_ATTEMPTS {
            return false;
        }
        self.additional_attempts += 1;
        true
    }
}

fn next_rebind_mode(
    tracker: &mut RebindTracker,
    mode: AttemptMode,
    was_live: bool,
    reason: ErrorKind,
) -> Option<AttemptMode> {
    if was_live || !matches!(mode, AttemptMode::Rebinding { .. }) {
        *tracker = RebindTracker::new();
    }
    tracker
        .next_attempt()
        .then_some(AttemptMode::Rebinding { reason })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AttemptMode {
    Connecting,
    Rebinding { reason: ErrorKind },
}

impl RetryTracker {
    const fn new() -> Self {
        Self {
            next_retry_number: 1,
        }
    }

    fn delay_after_failure(&mut self, backoff: BackoffConfig, was_live: bool) -> Duration {
        if was_live {
            self.next_retry_number = 1;
        }
        let delay = backoff.delay_for(self.next_retry_number);
        self.next_retry_number = self.next_retry_number.saturating_add(1);
        delay
    }

    fn reset(&mut self) {
        self.next_retry_number = 1;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AttemptFailure {
    error: Error,
    was_live: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FailurePlan {
    Rebind(AttemptMode),
    Backoff,
    Terminal,
}

fn plan_failure(
    retry_tracker: &mut RetryTracker,
    rebind_tracker: &mut RebindTracker,
    mode: AttemptMode,
    failure: AttemptFailure,
) -> FailurePlan {
    let kind = failure.error.kind();
    if is_rebindable(kind) {
        if failure.was_live {
            retry_tracker.reset();
        }
        if let Some(next_mode) = next_rebind_mode(rebind_tracker, mode, failure.was_live, kind) {
            return FailurePlan::Rebind(next_mode);
        }
        *rebind_tracker = RebindTracker::new();
        return FailurePlan::Backoff;
    }
    if is_retryable(kind) {
        *rebind_tracker = RebindTracker::new();
        FailurePlan::Backoff
    } else {
        FailurePlan::Terminal
    }
}

pub(crate) async fn run_supervisor(
    rest: &RestClient,
    requests: &[BindingRequest],
    subscription_id: i64,
    state_tx: &watch::Sender<TagClientState>,
    stop: oneshot::Receiver<()>,
) -> Result<()> {
    run_supervisor_with_config(
        rest,
        requests,
        subscription_id,
        state_tx,
        stop,
        BackoffConfig::default(),
    )
    .await
}

pub(crate) async fn run_supervisor_with_config(
    rest: &RestClient,
    requests: &[BindingRequest],
    subscription_id: i64,
    state_tx: &watch::Sender<TagClientState>,
    mut stop: oneshot::Receiver<()>,
    backoff: BackoffConfig,
) -> Result<()> {
    let mut retry_tracker = RetryTracker::new();
    let mut rebind_tracker = RebindTracker::new();
    let mut mode = AttemptMode::Connecting;
    loop {
        let attempt = run_attempt(rest, requests, subscription_id, state_tx, &mut stop, mode).await;
        let failure = match attempt {
            Ok(()) => {
                state_tx.send_replace(TagClientState::new(TagClientConnectionState::Stopped));
                return Ok(());
            }
            Err(failure) => failure,
        };
        let error = failure.error;
        if error.kind() == ErrorKind::Stopped {
            tracing::debug!("banto-tagclient worker stopped by caller request");
            state_tx.send_replace(TagClientState::new(TagClientConnectionState::Stopped));
            return Ok(());
        }
        if error.kind() == ErrorKind::Unauthorized {
            tracing::warn!("banto-tagclient worker stopping: banto-hub rejected credentials");
            state_tx.send_replace(TagClientState::unauthorized());
            return Err(error);
        }
        if is_rebindable(error.kind()) {
            state_tx.send_replace(TagClientState::rebinding(error.kind()));
        }
        match plan_failure(&mut retry_tracker, &mut rebind_tracker, mode, failure) {
            FailurePlan::Rebind(next_mode) => {
                tracing::debug!(
                    error_kind = error.kind().as_str(),
                    "banto-tagclient rebinding after a configuration change or metadata mismatch"
                );
                mode = next_mode;
                continue;
            }
            FailurePlan::Terminal => {
                tracing::warn!(
                    error_kind = error.kind().as_str(),
                    "banto-tagclient worker stopping: terminal, non-retryable error"
                );
                let mut state = TagClientState::new(TagClientConnectionState::Stopped);
                state.fail(error.kind());
                state_tx.send_replace(state);
                return Err(error);
            }
            FailurePlan::Backoff => {
                state_tx.send_replace(TagClientState::reconnecting(error.kind()));
                let delay = retry_tracker.delay_after_failure(backoff, failure.was_live);
                tracing::debug!(
                    error_kind = error.kind().as_str(),
                    delay_ms = delay.as_millis() as u64,
                    "banto-tagclient scheduling a reconnect attempt from the catalog"
                );
                mode = AttemptMode::Connecting;
                tokio::select! {
                    biased;
                    _ = &mut stop => {
                        state_tx.send_replace(TagClientState::new(TagClientConnectionState::Stopped));
                        return Ok(());
                    }
                    _ = tokio::time::sleep(delay) => {}
                }
            }
        }
    }
}

fn is_rebindable(kind: ErrorKind) -> bool {
    matches!(
        kind,
        ErrorKind::RevisionMismatch
            | ErrorKind::RuntimeMetadataMismatch
            | ErrorKind::BindingUnresolved
    )
}

fn is_retryable(kind: ErrorKind) -> bool {
    matches!(
        kind,
        ErrorKind::Transport | ErrorKind::ProtocolError | ErrorKind::CatalogUnavailable
    )
}

async fn run_attempt(
    rest: &RestClient,
    requests: &[BindingRequest],
    subscription_id: i64,
    state_tx: &watch::Sender<TagClientState>,
    stop: &mut oneshot::Receiver<()>,
    mode: AttemptMode,
) -> std::result::Result<(), AttemptFailure> {
    match mode {
        AttemptMode::Connecting => {
            state_tx.send_replace(TagClientState::new(TagClientConnectionState::Connecting));
        }
        AttemptMode::Rebinding { reason } => {
            state_tx.send_replace(TagClientState::rebinding(reason));
        }
    }
    let mut was_live = false;
    run_generation_inner(
        rest,
        requests,
        subscription_id,
        state_tx,
        stop,
        mode,
        &mut was_live,
    )
    .await
    .map_err(|error| AttemptFailure { error, was_live })
}

/// Run exactly one connection generation. No task is spawned and no retry is
/// attempted; any failure clears current and returns its stable error kind.
#[allow(
    dead_code,
    reason = "S3a TagClientHandle owns this crate-private worker"
)]
pub(crate) async fn run_generation(
    rest: &RestClient,
    requests: &[BindingRequest],
    subscription_id: i64,
    state_tx: &watch::Sender<TagClientState>,
    mut stop: oneshot::Receiver<()>,
) -> Result<()> {
    match run_attempt(
        rest,
        requests,
        subscription_id,
        state_tx,
        &mut stop,
        AttemptMode::Connecting,
    )
    .await
    {
        Ok(()) => {
            state_tx.send_replace(TagClientState::new(TagClientConnectionState::Stopped));
            Ok(())
        }
        Err(failure) if failure.error.kind() == ErrorKind::Stopped => {
            state_tx.send_replace(TagClientState::new(TagClientConnectionState::Stopped));
            Ok(())
        }
        Err(failure) => {
            let mut state = TagClientState::new(TagClientConnectionState::Stopped);
            state.fail(failure.error.kind());
            state_tx.send_replace(state);
            Err(failure.error)
        }
    }
}

async fn run_generation_inner(
    rest: &RestClient,
    requests: &[BindingRequest],
    subscription_id: i64,
    state_tx: &watch::Sender<TagClientState>,
    stop: &mut oneshot::Receiver<()>,
    mode: AttemptMode,
    was_live: &mut bool,
) -> Result<()> {
    if requests.is_empty() {
        return Err(Error::new(ErrorKind::InvalidTagSelection));
    }
    let catalog = tokio::select! {
        biased;
        _ = &mut *stop => return Err(stopped()),
        result = rest.fetch_catalog() => result?,
    };
    let resolution = resolve_bindings(requests, &catalog.tags)?;
    if !resolution.unresolved.is_empty() {
        return Err(Error::new(ErrorKind::BindingUnresolved));
    }
    let tags = resolution
        .resolved
        .iter()
        .map(|binding| binding.external_name.clone())
        .collect::<Vec<_>>();
    let mut gate = PublishGate::new(subscription_id, tags.clone())?;

    if matches!(mode, AttemptMode::Connecting) {
        state_tx.send_replace(TagClientState::new(TagClientConnectionState::Handshaking));
    }
    let mut connection = tokio::select! {
        biased;
        _ = &mut *stop => return Err(stopped()),
        result = rest.connect_stream() => result?,
    };
    tokio::select! {
        biased;
        _ = &mut *stop => {
            connection.close_best_effort().await;
            return Err(stopped());
        }
        result = connection.subscribe_on_change(subscription_id, &tags) => result?,
    }

    loop {
        let text = tokio::select! {
            biased;
            _ = &mut *stop => {
                connection.close_best_effort().await;
                return Err(stopped());
            }
            result = connection.receive_text() => result?,
        };
        if gate.accept_wire(&text)? == AcceptedWire::Data {
            break;
        }
    }

    let tag_refs = tags.iter().map(String::as_str).collect::<Vec<_>>();
    let mut values_future = Box::pin(rest.fetch_values(&tag_refs));
    let rest_snapshot = loop {
        tokio::select! {
            biased;
            _ = &mut *stop => {
                connection.close_best_effort().await;
                return Err(stopped());
            }
            text = connection.receive_text() => {
                gate.accept_wire(&text?)?;
            }
            snapshot = &mut values_future => break snapshot?,
        }
    };
    gate.record_rest_snapshot(rest_snapshot)?;
    publish_snapshot(&gate, &catalog, state_tx, was_live)?;

    loop {
        let text = tokio::select! {
            biased;
            _ = &mut *stop => {
                connection.close_best_effort().await;
                return Err(stopped());
            }
            result = connection.receive_text() => result?,
        };
        if gate.accept_wire(&text)? == AcceptedWire::Data {
            publish_snapshot(&gate, &catalog, state_tx, was_live)?;
        }
    }
}

fn publish_snapshot(
    gate: &PublishGate,
    catalog: &crate::types::CatalogSnapshot,
    state_tx: &watch::Sender<TagClientState>,
    was_live: &mut bool,
) -> Result<()> {
    let snapshot = gate.finalize(catalog)?;
    let mut state = TagClientState::new(TagClientConnectionState::Stopped);
    state.publish(snapshot);
    state_tx.send_replace(state);
    *was_live = true;
    Ok(())
}

fn stopped() -> Error {
    Error::new(ErrorKind::Stopped)
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc, Mutex,
        },
        time::Duration,
    };

    use futures_util::{SinkExt, StreamExt};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::{oneshot, watch},
    };
    use tokio_tungstenite::{accept_async, tungstenite::Message, WebSocketStream};

    use super::*;
    use crate::{
        endpoint::Endpoint,
        secret::SecretApiKey,
        types::{
            CatalogSnapshot, CatalogTag, CollectionMode, StableTagId, ValueEntry, ValueQuality,
            ValueSource,
        },
    };

    fn tag(id: StableTagId, name: &str) -> CatalogTag {
        CatalogTag {
            external_name: name.into(),
            tag_key: format!("key:{name}"),
            ids: id,
            connection: "connection".into(),
            group: "group".into(),
            name: name.into(),
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
        }
    }

    fn catalog(tags: Vec<CatalogTag>) -> CatalogSnapshot {
        CatalogSnapshot {
            revision: 1,
            run_id: Some(7),
            collection_mode: CollectionMode::Configured,
            tags,
        }
    }

    fn values(values: Vec<ValueEntry>) -> crate::types::ValuesSnapshot {
        crate::types::ValuesSnapshot {
            revision: 1,
            t: 10,
            run_id: Some(7),
            collection_mode: CollectionMode::Configured,
            values,
        }
    }

    fn value(tag: &str, number: f64, timestamp: i64) -> ValueEntry {
        ValueEntry {
            tag: tag.into(),
            v: Some(number),
            q: ValueQuality::Good,
            t: timestamp,
            value_source: ValueSource::Real,
        }
    }

    async fn read_http_request(stream: &mut tokio::net::TcpStream) -> String {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let count = stream.read(&mut buffer).await.unwrap();
            request.extend_from_slice(&buffer[..count]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                return String::from_utf8(request).unwrap();
            }
        }
    }

    async fn write_json(stream: &mut tokio::net::TcpStream, body: String) {
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    }

    fn named_values(
        name: &str,
        number: f64,
        revision: u64,
        run_id: Option<u64>,
        mode: CollectionMode,
    ) -> crate::types::ValuesSnapshot {
        crate::types::ValuesSnapshot {
            revision,
            t: 10,
            run_id,
            collection_mode: mode,
            values: vec![value(name, number, 10)],
        }
    }

    async fn serve_named_generation(
        listener: &TcpListener,
        name: &str,
        catalog_snapshot: CatalogSnapshot,
        values_snapshot: crate::types::ValuesSnapshot,
        catalog_count: &AtomicUsize,
        catalog_release: Option<oneshot::Receiver<()>>,
    ) -> WebSocketStream<tokio::net::TcpStream> {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_http_request(&mut stream).await;
        assert!(request.starts_with("GET /api/v1/tags HTTP/1.1"));
        catalog_count.fetch_add(1, Ordering::SeqCst);
        if let Some(catalog_release) = catalog_release {
            catalog_release.await.unwrap();
        }
        write_json(
            &mut stream,
            serde_json::to_string(&catalog_snapshot).unwrap(),
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
        let body = format!(
            r#"{{"op":"data","id":1,"t":10,"values":[{{"tag":"{name}","v":10,"q":"good","t":10}}]}}"#
        );
        socket.send(Message::Text(body.into())).await.unwrap();

        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_http_request(&mut stream).await;
        assert!(request.starts_with("GET /api/v1/values?tags="));
        write_json(
            &mut stream,
            serde_json::to_string(&values_snapshot).unwrap(),
        )
        .await;
        socket
    }

    async fn serve_unresolved_attempts_and_check_backoff(
        listener: TcpListener,
        attempts: usize,
        fifth_connection: oneshot::Sender<bool>,
    ) {
        for _ in 0..attempts {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_http_request(&mut stream).await;
            write_json(
                &mut stream,
                serde_json::to_string(&catalog(Vec::new())).unwrap(),
            )
            .await;
        }
        let connected = tokio::time::timeout(Duration::from_millis(40), listener.accept())
            .await
            .is_ok();
        let _ = fifth_connection.send(connected);
    }

    async fn serve_rebind_catalog_wait(listener: TcpListener, second_started: oneshot::Sender<()>) {
        let (mut first, _) = listener.accept().await.unwrap();
        read_http_request(&mut first).await;
        write_json(
            &mut first,
            serde_json::to_string(&catalog(Vec::new())).unwrap(),
        )
        .await;
        let (mut second, _) = listener.accept().await.unwrap();
        read_http_request(&mut second).await;
        second_started.send(()).unwrap();
        let mut buffer = [0_u8; 64];
        let _ = tokio::time::timeout(Duration::from_secs(1), second.read(&mut buffer)).await;
    }

    async fn wait_rebinding(receiver: &mut watch::Receiver<TagClientState>, reason: ErrorKind) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let state = receiver.borrow();
                if state.connection_state() == TagClientConnectionState::Rebinding
                    && state.last_error() == Some(reason)
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

    async fn wait_state(
        receiver: &mut watch::Receiver<TagClientState>,
        expected: TagClientConnectionState,
        error: Option<ErrorKind>,
    ) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let state = receiver.borrow();
                if state.connection_state() == expected && state.last_error() == error {
                    return;
                }
                drop(state);
                receiver.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
    }

    fn named_catalog(
        name: &str,
        revision: u64,
        run_id: Option<u64>,
        mode: CollectionMode,
    ) -> CatalogSnapshot {
        let mut snapshot = catalog(vec![tag(StableTagId::new(1, 1, 1), name)]);
        snapshot.revision = revision;
        snapshot.run_id = run_id;
        snapshot.collection_mode = mode;
        snapshot
    }

    fn client(address: String) -> RestClient {
        RestClient::new(
            Endpoint::new(address).unwrap(),
            SecretApiKey::new("test-token".into()).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn backoff_is_exponential_capped_and_overflow_safe() {
        let backoff = BackoffConfig::default();
        assert_eq!(backoff.delay_for(1), Duration::from_secs(1));
        assert_eq!(backoff.delay_for(2), Duration::from_secs(2));
        assert_eq!(backoff.delay_for(3), Duration::from_secs(4));
        assert_eq!(backoff.delay_for(4), Duration::from_secs(8));
        assert_eq!(backoff.delay_for(5), Duration::from_secs(16));
        assert_eq!(backoff.delay_for(6), Duration::from_secs(30));
        assert_eq!(backoff.delay_for(100), Duration::from_secs(30));
        assert_eq!(backoff.delay_for(u32::MAX), Duration::from_secs(30));
    }

    #[test]
    fn rebind_tracker_allows_three_additional_attempts_then_falls_back() {
        let mut tracker = RebindTracker::new();
        assert!(tracker.next_attempt());
        assert!(tracker.next_attempt());
        assert!(tracker.next_attempt());
        assert!(!tracker.next_attempt());
    }

    #[test]
    fn live_rebind_success_starts_a_new_rebind_budget() {
        let mut tracker = RebindTracker::new();
        let first = next_rebind_mode(
            &mut tracker,
            AttemptMode::Connecting,
            false,
            ErrorKind::RevisionMismatch,
        )
        .unwrap();
        let second = next_rebind_mode(
            &mut tracker,
            first,
            false,
            ErrorKind::RuntimeMetadataMismatch,
        )
        .unwrap();

        // The generation became Live after the second attempt. A later
        // config_changed therefore starts a fresh three-attempt cycle.
        let mut mode =
            next_rebind_mode(&mut tracker, second, true, ErrorKind::RevisionMismatch).unwrap();
        mode = next_rebind_mode(&mut tracker, mode, false, ErrorKind::RevisionMismatch).unwrap();
        mode = next_rebind_mode(&mut tracker, mode, false, ErrorKind::RevisionMismatch).unwrap();
        assert!(
            next_rebind_mode(&mut tracker, mode, false, ErrorKind::RevisionMismatch,).is_none()
        );
    }

    #[test]
    fn live_rebind_also_resets_the_normal_retry_budget() {
        let backoff = BackoffConfig::new(Duration::from_secs(1), Duration::from_secs(30));
        let mut retry_tracker = RetryTracker::new();
        let mut rebind_tracker = RebindTracker::new();
        let mut mode = AttemptMode::Connecting;
        let transport_failure = AttemptFailure {
            error: Error::new(ErrorKind::Transport),
            was_live: false,
        };
        assert_eq!(
            plan_failure(
                &mut retry_tracker,
                &mut rebind_tracker,
                mode,
                transport_failure
            ),
            FailurePlan::Backoff
        );
        assert_eq!(
            retry_tracker.delay_after_failure(backoff, false),
            Duration::from_secs(1)
        );
        assert_eq!(
            plan_failure(
                &mut retry_tracker,
                &mut rebind_tracker,
                mode,
                transport_failure
            ),
            FailurePlan::Backoff
        );
        assert_eq!(
            retry_tracker.delay_after_failure(backoff, false),
            Duration::from_secs(2)
        );

        // A config_changed after a Live generation resets both budgets before
        // the bounded rebind cycle begins.
        let live_rebind_failure = AttemptFailure {
            error: Error::new(ErrorKind::RevisionMismatch),
            was_live: true,
        };
        mode = match plan_failure(
            &mut retry_tracker,
            &mut rebind_tracker,
            mode,
            live_rebind_failure,
        ) {
            FailurePlan::Rebind(next_mode) => next_mode,
            _ => panic!("live rebind must start a rebind cycle"),
        };
        for _ in 0..2 {
            mode = match plan_failure(
                &mut retry_tracker,
                &mut rebind_tracker,
                mode,
                AttemptFailure {
                    error: Error::new(ErrorKind::RevisionMismatch),
                    was_live: false,
                },
            ) {
                FailurePlan::Rebind(next_mode) => next_mode,
                _ => panic!("rebind budget should still allow another attempt"),
            };
        }
        assert_eq!(
            plan_failure(
                &mut retry_tracker,
                &mut rebind_tracker,
                mode,
                AttemptFailure {
                    error: Error::new(ErrorKind::RevisionMismatch),
                    was_live: false,
                }
            ),
            FailurePlan::Backoff
        );
        assert_eq!(
            retry_tracker.delay_after_failure(backoff, false),
            Duration::from_secs(1)
        );
    }

    #[test]
    fn live_failure_resets_retry_delay_to_the_base() {
        let backoff = BackoffConfig::new(Duration::from_secs(1), Duration::from_secs(30));
        let mut tracker = RetryTracker::new();
        assert_eq!(
            tracker.delay_after_failure(backoff, false),
            Duration::from_secs(1)
        );
        assert_eq!(
            tracker.delay_after_failure(backoff, false),
            Duration::from_secs(2)
        );
        assert_eq!(
            tracker.delay_after_failure(backoff, true),
            Duration::from_secs(1)
        );
    }

    #[tokio::test]
    async fn watch_keeps_only_latest_complete_state_and_non_live_clears_current() {
        let (sender, receiver) =
            watch::channel(TagClientState::new(TagClientConnectionState::Stopped));
        let mut latest = None;
        for number in 0..256 {
            let mut state = TagClientState::new(TagClientConnectionState::Stopped);
            state.publish(values(vec![value("tag", number as f64, number)]));
            sender.send_replace(state);
            latest = Some(number as f64);
        }
        assert_eq!(receiver.borrow().current().unwrap().values[0].v, latest);
        sender.send_replace(TagClientState::new(TagClientConnectionState::Stopped));
        assert_eq!(receiver.borrow().current(), None);
    }

    #[tokio::test]
    async fn config_changed_burst_rebinds_once_and_uses_new_external_name() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let catalog_count = Arc::new(AtomicUsize::new(0));
        let server_count = Arc::clone(&catalog_count);
        let (first_live_tx, first_live_rx) = oneshot::channel();
        let (release_old_tx, release_old_rx) = oneshot::channel();
        let (release_rebind_tx, release_rebind_rx) = oneshot::channel();
        let (second_live_tx, second_live_rx) = oneshot::channel();
        let (release_new_tx, release_new_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let mut old_socket = serve_named_generation(
                &listener,
                "old-name",
                named_catalog("old-name", 1, Some(7), CollectionMode::Configured),
                named_values("old-name", 10.0, 1, Some(7), CollectionMode::Configured),
                &server_count,
                None,
            )
            .await;
            first_live_tx.send(()).unwrap();
            release_old_rx.await.unwrap();
            old_socket
                .send(Message::Text(
                    r#"{"op":"config_changed","revision":2}"#.into(),
                ))
                .await
                .unwrap();
            old_socket
                .send(Message::Text(
                    r#"{"op":"config_changed","revision":3}"#.into(),
                ))
                .await
                .unwrap();
            drop(old_socket);
            let new_socket = serve_named_generation(
                &listener,
                "new-name",
                named_catalog("new-name", 1, Some(7), CollectionMode::Configured),
                named_values("new-name", 20.0, 1, Some(7), CollectionMode::Configured),
                &server_count,
                Some(release_rebind_rx),
            )
            .await;
            second_live_tx.send(()).unwrap();
            release_new_rx.await.unwrap();
            drop(new_socket);
        });

        let (sender, mut receiver) =
            watch::channel(TagClientState::new(TagClientConnectionState::Stopped));
        let mut history_receiver = sender.subscribe();
        let (history_stop_tx, mut history_stop_rx) = oneshot::channel();
        let (second_live_seen_tx, second_live_seen_rx) = oneshot::channel();
        let history_task = tokio::spawn(async move {
            let mut states = Vec::new();
            let mut live_count = 0;
            let mut second_live_seen_tx = Some(second_live_seen_tx);
            loop {
                tokio::select! {
                    _ = &mut history_stop_rx => return states,
                    changed = history_receiver.changed() => {
                        if changed.is_err() {
                            return states;
                        }
                        let state = history_receiver.borrow().connection_state();
                        states.push(state);
                        if state == TagClientConnectionState::Live {
                            live_count += 1;
                            if live_count == 2 {
                                if let Some(sender) = second_live_seen_tx.take() {
                                    let _ = sender.send(());
                                }
                            }
                        }
                    }
                }
            }
        });
        let (stop_tx, stop_rx) = oneshot::channel();
        let rest_client = client(address);
        let requests = vec![BindingRequest {
            binding_key: "stable".into(),
            stable_id: StableTagId::new(1, 1, 1),
        }];
        let worker_sender = sender.clone();
        let task = tokio::spawn(async move {
            run_supervisor_with_config(
                &rest_client,
                &requests,
                1,
                &worker_sender,
                stop_rx,
                BackoffConfig::new(Duration::from_millis(1), Duration::from_millis(5)),
            )
            .await
        });
        first_live_rx.await.unwrap();
        wait_state(&mut receiver, TagClientConnectionState::Live, None).await;
        assert_eq!(
            receiver.borrow().current().unwrap().values[0].tag,
            "old-name"
        );
        release_old_tx.send(()).unwrap();
        wait_rebinding(&mut receiver, ErrorKind::RevisionMismatch).await;
        assert_eq!(receiver.borrow().current(), None);
        release_rebind_tx.send(()).unwrap();
        second_live_rx.await.unwrap();
        wait_state(&mut receiver, TagClientConnectionState::Live, None).await;
        let current = receiver.borrow().current().unwrap().clone();
        assert_eq!(current.values[0].tag, "new-name");
        assert_eq!(current.values[0].v, Some(20.0));
        assert_eq!(catalog_count.load(Ordering::SeqCst), 2);
        second_live_seen_rx.await.unwrap();
        history_stop_tx.send(()).unwrap();
        let history = history_task.await.unwrap();
        let first_live = history
            .iter()
            .position(|state| *state == TagClientConnectionState::Live)
            .unwrap();
        let second_live = history
            .iter()
            .skip(first_live + 1)
            .position(|state| *state == TagClientConnectionState::Live)
            .map(|offset| first_live + 1 + offset)
            .unwrap();
        assert!(history[first_live + 1..second_live]
            .iter()
            .all(|state| *state == TagClientConnectionState::Rebinding));
        release_new_tx.send(()).unwrap();
        stop_tx.send(()).unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .is_ok());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn config_changed_before_initial_rest_completion_never_publishes_partial_live() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let catalog_count = Arc::new(AtomicUsize::new(0));
        let server_count = Arc::clone(&catalog_count);
        let (config_sent_tx, config_sent_rx) = oneshot::channel();
        let (release_values_tx, release_values_rx) = oneshot::channel();
        let (live_tx, live_rx) = oneshot::channel();
        let (release_new_tx, release_new_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut catalog_stream, _) = listener.accept().await.unwrap();
            read_http_request(&mut catalog_stream).await;
            server_count.fetch_add(1, Ordering::SeqCst);
            write_json(
                &mut catalog_stream,
                serde_json::to_string(&named_catalog(
                    "old-name",
                    1,
                    Some(7),
                    CollectionMode::Configured,
                ))
                .unwrap(),
            )
            .await;
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let _ = tokio::time::timeout(Duration::from_secs(1), socket.next())
                .await
                .unwrap();
            socket
                .send(Message::Text(
                    r#"{"op":"data","id":1,"t":10,"values":[{"tag":"old-name","v":10,"q":"good","t":10}]}"#.into(),
                ))
                .await
                .unwrap();
            let (mut values_stream, _) = listener.accept().await.unwrap();
            read_http_request(&mut values_stream).await;
            socket
                .send(Message::Text(
                    r#"{"op":"config_changed","revision":2}"#.into(),
                ))
                .await
                .unwrap();
            config_sent_tx.send(()).unwrap();
            release_values_rx.await.unwrap();
            write_json(
                &mut values_stream,
                serde_json::to_string(&named_values(
                    "old-name",
                    10.0,
                    1,
                    Some(7),
                    CollectionMode::Configured,
                ))
                .unwrap(),
            )
            .await;
            drop(socket);
            let new_socket = serve_named_generation(
                &listener,
                "new-name",
                named_catalog("new-name", 1, Some(7), CollectionMode::Configured),
                named_values("new-name", 20.0, 1, Some(7), CollectionMode::Configured),
                &server_count,
                None,
            )
            .await;
            live_tx.send(()).unwrap();
            release_new_rx.await.unwrap();
            drop(new_socket);
        });

        let (sender, mut receiver) =
            watch::channel(TagClientState::new(TagClientConnectionState::Stopped));
        let (stop_tx, stop_rx) = oneshot::channel();
        let rest_client = client(address);
        let requests = vec![BindingRequest {
            binding_key: "stable".into(),
            stable_id: StableTagId::new(1, 1, 1),
        }];
        let worker_sender = sender.clone();
        let task = tokio::spawn(async move {
            run_supervisor_with_config(
                &rest_client,
                &requests,
                1,
                &worker_sender,
                stop_rx,
                BackoffConfig::new(Duration::from_millis(1), Duration::from_millis(5)),
            )
            .await
        });
        config_sent_rx.await.unwrap();
        wait_rebinding(&mut receiver, ErrorKind::RevisionMismatch).await;
        assert_eq!(receiver.borrow().current(), None);
        release_values_tx.send(()).unwrap();
        live_rx.await.unwrap();
        wait_state(&mut receiver, TagClientConnectionState::Live, None).await;
        assert_eq!(
            receiver.borrow().current().unwrap().values[0].tag,
            "new-name"
        );
        release_new_tx.send(()).unwrap();
        stop_tx.send(()).unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .is_ok());
        server.await.unwrap();
    }

    async fn assert_metadata_rebind(
        first_catalog: CatalogSnapshot,
        first_values: crate::types::ValuesSnapshot,
        reason: ErrorKind,
    ) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let catalog_count = Arc::new(AtomicUsize::new(0));
        let server_count = Arc::clone(&catalog_count);
        let (rebind_gate_tx, rebind_gate_rx) = oneshot::channel();
        let (live_tx, live_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let first_socket = serve_named_generation(
                &listener,
                "alpha",
                first_catalog,
                first_values,
                &server_count,
                None,
            )
            .await;
            let second_socket = serve_named_generation(
                &listener,
                "alpha",
                named_catalog("alpha", 1, Some(7), CollectionMode::Configured),
                named_values("alpha", 20.0, 1, Some(7), CollectionMode::Configured),
                &server_count,
                Some(rebind_gate_rx),
            )
            .await;
            live_tx.send(()).unwrap();
            release_rx.await.unwrap();
            drop(second_socket);
            drop(first_socket);
        });
        let (sender, mut receiver) =
            watch::channel(TagClientState::new(TagClientConnectionState::Stopped));
        let (stop_tx, stop_rx) = oneshot::channel();
        let rest_client = client(address);
        let requests = vec![BindingRequest {
            binding_key: "stable".into(),
            stable_id: StableTagId::new(1, 1, 1),
        }];
        let worker_sender = sender.clone();
        let task = tokio::spawn(async move {
            run_supervisor_with_config(
                &rest_client,
                &requests,
                1,
                &worker_sender,
                stop_rx,
                BackoffConfig::new(Duration::from_millis(1), Duration::from_millis(5)),
            )
            .await
        });
        wait_rebinding(&mut receiver, reason).await;
        assert_eq!(receiver.borrow().current(), None);
        rebind_gate_tx.send(()).unwrap();
        live_rx.await.unwrap();
        wait_state(&mut receiver, TagClientConnectionState::Live, None).await;
        assert_eq!(catalog_count.load(Ordering::SeqCst), 2);
        release_tx.send(()).unwrap();
        stop_tx.send(()).unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .is_ok());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn revision_mismatch_rebinds_fail_closed() {
        assert_metadata_rebind(
            named_catalog("alpha", 1, Some(7), CollectionMode::Configured),
            named_values("alpha", 10.0, 2, Some(7), CollectionMode::Configured),
            ErrorKind::RevisionMismatch,
        )
        .await;
    }

    #[tokio::test]
    async fn unresolved_catalog_rebinds_and_recovers_with_same_stable_id() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let catalog_count = Arc::new(AtomicUsize::new(0));
        let server_count = Arc::clone(&catalog_count);
        let (rebind_gate_tx, rebind_gate_rx) = oneshot::channel();
        let (live_tx, live_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_http_request(&mut stream).await;
            server_count.fetch_add(1, Ordering::SeqCst);
            write_json(
                &mut stream,
                serde_json::to_string(&catalog(Vec::new())).unwrap(),
            )
            .await;
            let socket = serve_named_generation(
                &listener,
                "resolved-name",
                named_catalog("resolved-name", 1, Some(7), CollectionMode::Configured),
                named_values(
                    "resolved-name",
                    20.0,
                    1,
                    Some(7),
                    CollectionMode::Configured,
                ),
                &server_count,
                Some(rebind_gate_rx),
            )
            .await;
            live_tx.send(()).unwrap();
            release_rx.await.unwrap();
            drop(socket);
        });
        let (sender, mut receiver) =
            watch::channel(TagClientState::new(TagClientConnectionState::Stopped));
        let (stop_tx, stop_rx) = oneshot::channel();
        let rest_client = client(address);
        let requests = vec![BindingRequest {
            binding_key: "stable".into(),
            stable_id: StableTagId::new(1, 1, 1),
        }];
        let worker_sender = sender.clone();
        let task = tokio::spawn(async move {
            run_supervisor_with_config(
                &rest_client,
                &requests,
                1,
                &worker_sender,
                stop_rx,
                BackoffConfig::new(Duration::from_millis(1), Duration::from_millis(5)),
            )
            .await
        });
        wait_rebinding(&mut receiver, ErrorKind::BindingUnresolved).await;
        assert_eq!(receiver.borrow().current(), None);
        rebind_gate_tx.send(()).unwrap();
        live_rx.await.unwrap();
        wait_state(&mut receiver, TagClientConnectionState::Live, None).await;
        assert_eq!(catalog_count.load(Ordering::SeqCst), 2);
        release_tx.send(()).unwrap();
        stop_tx.send(()).unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .is_ok());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn four_rebind_failures_enter_normal_backoff_without_fast_fifth_attempt() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let (fifth_tx, fifth_rx) = oneshot::channel();
        let server = tokio::spawn(serve_unresolved_attempts_and_check_backoff(
            listener, 4, fifth_tx,
        ));
        let (sender, mut receiver) =
            watch::channel(TagClientState::new(TagClientConnectionState::Stopped));
        let (stop_tx, stop_rx) = oneshot::channel();
        let rest_client = client(address);
        let requests = vec![BindingRequest {
            binding_key: "stable".into(),
            stable_id: StableTagId::new(1, 1, 1),
        }];
        let worker_sender = sender.clone();
        let task = tokio::spawn(async move {
            run_supervisor_with_config(
                &rest_client,
                &requests,
                1,
                &worker_sender,
                stop_rx,
                BackoffConfig::new(Duration::from_millis(100), Duration::from_millis(200)),
            )
            .await
        });
        wait_state(
            &mut receiver,
            TagClientConnectionState::Reconnecting,
            Some(ErrorKind::BindingUnresolved),
        )
        .await;
        assert_eq!(receiver.borrow().current(), None);
        assert!(!fifth_rx.await.unwrap());
        stop_tx.send(()).unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .is_ok());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn stop_during_rebinding_catalog_waits_for_no_extra_attempt() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let (second_started_tx, second_started_rx) = oneshot::channel();
        let server = tokio::spawn(serve_rebind_catalog_wait(listener, second_started_tx));
        let (sender, receiver) =
            watch::channel(TagClientState::new(TagClientConnectionState::Stopped));
        let (stop_tx, stop_rx) = oneshot::channel();
        let rest_client = client(address);
        let requests = vec![BindingRequest {
            binding_key: "stable".into(),
            stable_id: StableTagId::new(1, 1, 1),
        }];
        let worker_sender = sender.clone();
        let task = tokio::spawn(async move {
            run_supervisor_with_config(
                &rest_client,
                &requests,
                1,
                &worker_sender,
                stop_rx,
                BackoffConfig::new(Duration::from_millis(1), Duration::from_millis(5)),
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(1), second_started_rx)
            .await
            .unwrap()
            .unwrap();
        stop_tx.send(()).unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .is_ok());
        assert_eq!(
            receiver.borrow().connection_state(),
            TagClientConnectionState::Stopped
        );
        assert_eq!(receiver.borrow().current(), None);
        assert_eq!(receiver.borrow().last_error(), None);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn transport_during_rebind_enters_backoff_and_recovers_from_catalog() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let catalog_count = Arc::new(AtomicUsize::new(0));
        let server_count = Arc::clone(&catalog_count);
        let (first_live_tx, first_live_rx) = oneshot::channel();
        let (release_old_tx, release_old_rx) = oneshot::channel();
        let (release_old_socket_tx, release_old_socket_rx) = oneshot::channel();
        let (reconnect_tx, reconnect_rx) = oneshot::channel();
        let (second_live_tx, second_live_rx) = oneshot::channel();
        let (release_new_tx, release_new_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let mut first_socket = serve_named_generation(
                &listener,
                "old-name",
                named_catalog("old-name", 1, Some(7), CollectionMode::Configured),
                named_values("old-name", 10.0, 1, Some(7), CollectionMode::Configured),
                &server_count,
                None,
            )
            .await;
            first_live_tx.send(()).unwrap();
            release_old_rx.await.unwrap();
            first_socket
                .send(Message::Text(
                    r#"{"op":"config_changed","revision":2}"#.into(),
                ))
                .await
                .unwrap();
            release_old_socket_rx.await.unwrap();
            drop(first_socket);
            let (mut failed, _) = listener.accept().await.unwrap();
            read_http_request(&mut failed).await;
            drop(failed);
            reconnect_tx.send(()).unwrap();
            let second_socket = serve_named_generation(
                &listener,
                "new-name",
                named_catalog("new-name", 1, Some(7), CollectionMode::Configured),
                named_values("new-name", 20.0, 1, Some(7), CollectionMode::Configured),
                &server_count,
                None,
            )
            .await;
            second_live_tx.send(()).unwrap();
            release_new_rx.await.unwrap();
            drop(second_socket);
        });
        let (sender, mut receiver) =
            watch::channel(TagClientState::new(TagClientConnectionState::Stopped));
        let (stop_tx, stop_rx) = oneshot::channel();
        let rest_client = client(address);
        let requests = vec![BindingRequest {
            binding_key: "stable".into(),
            stable_id: StableTagId::new(1, 1, 1),
        }];
        let worker_sender = sender.clone();
        let task = tokio::spawn(async move {
            run_supervisor_with_config(
                &rest_client,
                &requests,
                1,
                &worker_sender,
                stop_rx,
                BackoffConfig::new(Duration::from_millis(1), Duration::from_millis(5)),
            )
            .await
        });
        first_live_rx.await.unwrap();
        wait_state(&mut receiver, TagClientConnectionState::Live, None).await;
        release_old_tx.send(()).unwrap();
        wait_rebinding(&mut receiver, ErrorKind::RevisionMismatch).await;
        release_old_socket_tx.send(()).unwrap();
        reconnect_rx.await.unwrap();
        wait_state(
            &mut receiver,
            TagClientConnectionState::Reconnecting,
            Some(ErrorKind::Transport),
        )
        .await;
        second_live_rx.await.unwrap();
        wait_state(&mut receiver, TagClientConnectionState::Live, None).await;
        assert_eq!(catalog_count.load(Ordering::SeqCst), 2);
        release_new_tx.send(()).unwrap();
        stop_tx.send(()).unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .is_ok());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn run_id_mismatch_rebinds_fail_closed() {
        assert_metadata_rebind(
            named_catalog("alpha", 1, Some(7), CollectionMode::Configured),
            named_values("alpha", 10.0, 1, Some(8), CollectionMode::Configured),
            ErrorKind::RuntimeMetadataMismatch,
        )
        .await;
    }

    #[tokio::test]
    async fn collection_mode_mismatch_rebinds_fail_closed() {
        assert_metadata_rebind(
            named_catalog("alpha", 1, Some(7), CollectionMode::Configured),
            named_values("alpha", 10.0, 1, Some(7), CollectionMode::AllSimulation),
            ErrorKind::RuntimeMetadataMismatch,
        )
        .await;
    }

    #[tokio::test]
    async fn unknown_collection_mode_rebinds_fail_closed() {
        assert_metadata_rebind(
            named_catalog(
                "alpha",
                1,
                Some(7),
                CollectionMode::Unknown("future".into()),
            ),
            named_values(
                "alpha",
                10.0,
                1,
                Some(7),
                CollectionMode::Unknown("future".into()),
            ),
            ErrorKind::RuntimeMetadataMismatch,
        )
        .await;
    }

    #[tokio::test]
    async fn generation_orders_catalog_ws_subscribe_data_then_rest_and_publishes_atomically() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let order = Arc::new(Mutex::new(Vec::new()));
        let order_server = Arc::clone(&order);
        let (start_burst_tx, start_burst_rx) = oneshot::channel();
        let (burst_done_tx, burst_done_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let id_a = StableTagId::new(1, 1, 1);
            let id_b = StableTagId::new(1, 1, 2);
            let catalog_body =
                serde_json::to_string(&catalog(vec![tag(id_a, "alpha"), tag(id_b, "beta")]))
                    .unwrap();
            let values_body =
                serde_json::to_string(&values(vec![value("alpha", 1.0, 5), value("beta", 2.0, 5)]))
                    .unwrap();

            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_http_request(&mut stream).await;
            assert!(request.starts_with("GET /api/v1/tags HTTP/1.1"));
            order_server.lock().unwrap().push("catalog");
            write_json(&mut stream, catalog_body).await;

            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let subscription = tokio::time::timeout(Duration::from_secs(1), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(
                subscription,
                Message::Text(
                    r#"{"op":"subscribe","id":9,"tags":["alpha","beta"],"mode":"on_change"}"#
                        .into()
                )
            );
            order_server.lock().unwrap().push("subscribe");
            socket.send(Message::Text(r#"{"op":"data","id":9,"t":5,"values":[{"tag":"alpha","v":1,"q":"good","t":5}]}"#.into())).await.unwrap();

            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_http_request(&mut stream).await;
            assert!(request.starts_with("GET /api/v1/values?tags=alpha%2Cbeta HTTP/1.1"));
            order_server.lock().unwrap().push("values");
            socket.send(Message::Text(r#"{"op":"data","id":9,"t":20,"values":[{"tag":"alpha","v":20,"q":"good","t":20}]}"#.into())).await.unwrap();
            write_json(&mut stream, values_body).await;
            let _ = start_burst_rx.await;
            for timestamp in 30..=125 {
                let body = format!(
                    r#"{{"op":"data","id":9,"t":{0},"values":[{{"tag":"alpha","v":{0},"q":"good","t":{0}}}]}}"#,
                    timestamp
                );
                socket.send(Message::Text(body.into())).await.unwrap();
            }
            socket.flush().await.unwrap();
            burst_done_tx.send(()).unwrap();
            let _ = release_rx.await;
        });

        let (sender, mut receiver) =
            watch::channel(TagClientState::new(TagClientConnectionState::Stopped));
        let mut progress_receiver = sender.subscribe();
        let requests = vec![
            BindingRequest {
                binding_key: "a".into(),
                stable_id: StableTagId::new(1, 1, 1),
            },
            BindingRequest {
                binding_key: "b".into(),
                stable_id: StableTagId::new(1, 1, 2),
            },
        ];
        let rest_client = client(address);
        let (_stop_tx, stop_rx) = oneshot::channel();
        let worker = tokio::spawn(async move {
            run_generation(&rest_client, &requests, 9, &sender, stop_rx).await
        });
        for expected in [
            TagClientConnectionState::Connecting,
            TagClientConnectionState::Handshaking,
        ] {
            tokio::time::timeout(Duration::from_secs(1), receiver.changed())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(receiver.borrow().connection_state(), expected);
            assert_eq!(receiver.borrow().current(), None);
        }
        tokio::time::timeout(Duration::from_secs(1), receiver.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            receiver.borrow().connection_state(),
            TagClientConnectionState::Live
        );
        let initial = receiver.borrow().clone();
        assert_eq!(initial.current().unwrap().values[0].v, Some(20.0));
        assert_eq!(initial.current().unwrap().values[1].v, Some(2.0));
        assert_eq!(initial.current().unwrap().t, 20);
        start_burst_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), burst_done_rx)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                progress_receiver.changed().await.unwrap();
                if progress_receiver
                    .borrow_and_update()
                    .current()
                    .is_some_and(|snapshot| {
                        snapshot
                            .values
                            .iter()
                            .any(|entry| entry.tag == "alpha" && entry.v == Some(125.0))
                    })
                {
                    break;
                }
            }
        })
        .await
        .unwrap();
        assert!(receiver.has_changed().unwrap());
        tokio::time::timeout(Duration::from_secs(1), receiver.changed())
            .await
            .unwrap()
            .unwrap();
        let latest = receiver.borrow().clone();
        assert_eq!(latest.connection_state(), TagClientConnectionState::Live);
        assert_eq!(latest.current().unwrap().values[0].v, Some(125.0));
        assert_eq!(latest.current().unwrap().values[1].v, Some(2.0));
        assert_eq!(latest.current().unwrap().t, 125);
        release_tx.send(()).unwrap();
        let result = tokio::time::timeout(Duration::from_secs(2), worker)
            .await
            .unwrap();
        assert_eq!(result.unwrap().unwrap_err().kind(), ErrorKind::Transport);
        assert_eq!(
            receiver.borrow().connection_state(),
            TagClientConnectionState::Stopped
        );
        assert_eq!(receiver.borrow().current(), None);
        assert_eq!(
            order.lock().unwrap().as_slice(),
            ["catalog", "subscribe", "values"]
        );
        let state = receiver.borrow().clone();
        assert_eq!(state.connection_state(), TagClientConnectionState::Stopped);
        assert_eq!(state.current(), None);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn empty_binding_requests_fail_before_websocket_and_keep_current_empty() {
        let (sender, receiver) =
            watch::channel(TagClientState::new(TagClientConnectionState::Stopped));
        let (_stop_tx, stop_rx) = oneshot::channel();
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            run_generation(
                &client("http://127.0.0.1:1".into()),
                &[],
                1,
                &sender,
                stop_rx,
            ),
        )
        .await
        .unwrap();
        assert_eq!(result.unwrap_err().kind(), ErrorKind::InvalidTagSelection);
        assert_eq!(
            receiver.borrow().connection_state(),
            TagClientConnectionState::Stopped
        );
        assert_eq!(receiver.borrow().current(), None);
    }

    #[tokio::test]
    async fn unresolved_binding_fails_before_websocket_and_keeps_current_empty() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let _ = read_http_request(&mut stream).await;
            write_json(
                &mut stream,
                serde_json::to_string(&catalog(Vec::new())).unwrap(),
            )
            .await;
        });
        let (sender, receiver) =
            watch::channel(TagClientState::new(TagClientConnectionState::Stopped));
        let request = BindingRequest {
            binding_key: "missing".into(),
            stable_id: StableTagId::new(9, 9, 9),
        };
        let (_stop_tx, stop_rx) = oneshot::channel();
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            run_generation(&client(address), &[request], 1, &sender, stop_rx),
        )
        .await
        .unwrap();
        assert_eq!(result.unwrap_err().kind(), ErrorKind::BindingUnresolved);
        assert_eq!(
            receiver.borrow().connection_state(),
            TagClientConnectionState::Stopped
        );
        assert_eq!(receiver.borrow().current(), None);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn config_changed_during_generation_fails_closed_and_clears_current() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let id = StableTagId::new(1, 1, 1);
            let (mut stream, _) = listener.accept().await.unwrap();
            let _ = read_http_request(&mut stream).await;
            write_json(
                &mut stream,
                serde_json::to_string(&catalog(vec![tag(id, "alpha")])).unwrap(),
            )
            .await;
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            let _ = tokio::time::timeout(Duration::from_secs(1), socket.next())
                .await
                .unwrap();
            socket
                .send(Message::Text(
                    r#"{"op":"data","id":1,"t":1,"values":[{"tag":"alpha","v":1,"q":"good","t":1}]}"#.into(),
                ))
                .await
                .unwrap();
            socket
                .send(Message::Text(
                    r#"{"op":"config_changed","revision":2}"#.into(),
                ))
                .await
                .unwrap();
        });
        let (sender, receiver) =
            watch::channel(TagClientState::new(TagClientConnectionState::Stopped));
        let request = BindingRequest {
            binding_key: "alpha".into(),
            stable_id: StableTagId::new(1, 1, 1),
        };
        let (_stop_tx, stop_rx) = oneshot::channel();
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            run_generation(&client(address), &[request], 1, &sender, stop_rx),
        )
        .await
        .unwrap();
        assert_eq!(result.unwrap_err().kind(), ErrorKind::RevisionMismatch);
        assert_eq!(
            receiver.borrow().connection_state(),
            TagClientConnectionState::Stopped
        );
        assert_eq!(receiver.borrow().current(), None);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn retry_backoff_diagnostic_omits_secret() {
        let (log, _guard) = crate::test_support::capture();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        // Accept and immediately drop each connection (without ever writing a
        // response) so every catalog request fails fast and deterministically
        // with a transport error, unlike relying on OS-specific timing for a
        // connection to a closed port.
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let mut buffer = [0_u8; 1024];
                let _ = stream.read(&mut buffer).await;
                drop(stream);
            }
        });
        let (sender, mut receiver) =
            watch::channel(TagClientState::new(TagClientConnectionState::Stopped));
        let (stop_tx, stop_rx) = oneshot::channel();
        let rest_client = client(address);
        let requests = vec![BindingRequest {
            binding_key: "stable".into(),
            stable_id: StableTagId::new(1, 1, 1),
        }];
        let worker_sender = sender.clone();
        let task = tokio::spawn(async move {
            run_supervisor_with_config(
                &rest_client,
                &requests,
                1,
                &worker_sender,
                stop_rx,
                BackoffConfig::new(Duration::from_millis(20), Duration::from_millis(50)),
            )
            .await
        });
        wait_state(
            &mut receiver,
            TagClientConnectionState::Reconnecting,
            Some(ErrorKind::Transport),
        )
        .await;
        stop_tx.send(()).unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .is_ok());
        server.abort();
        assert!(!log.contains("test-token"));
        assert!(log.contains("scheduling a reconnect"));
    }
}
