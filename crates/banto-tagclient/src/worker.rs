//! One connection-generation worker and bounded latest-state publication.
//!
//! The attempt and supervisor are crate-private. S3a provides the public owner
//! for task lifetime and explicit shutdown; S3b-1 adds sequential reconnect and
//! backoff, bounded rebinding, and metadata re-resolution.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use tokio::sync::{oneshot, watch};

/// Process-wide generation counter (#682 R2): unique across every handle, so a
/// fresh `start` after `shutdown` never reuses a number an observer has seen.
static GENERATION: AtomicU64 = AtomicU64::new(0);
use tokio::time::Duration;

use crate::{
    binding::{resolve_bindings, BindingRequest},
    error::{Error, ErrorKind, Result},
    rest::RestClient,
    stream_core::{parse_unbound_wire, AcceptedWire, PublishGate, UnboundWire},
    types::{GenerationSnapshot, TagClientConnectionState, TagClientState},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BackoffConfig {
    base: Duration,
    max: Duration,
    /// How long `run_unbound` waits for `pong` after its `ping` (design
    /// §5.1 / §14). A Hub that keeps the WebSocket but never answers would
    /// otherwise pin the worker in `Handshaking` / `Rebinding` forever.
    pong_timeout: Duration,
}

/// Default for [`BackoffConfig::pong_timeout`].
const PONG_TIMEOUT: Duration = Duration::from_secs(10);

impl Default for BackoffConfig {
    fn default() -> Self {
        Self {
            base: Duration::from_secs(1),
            max: Duration::from_secs(30),
            pong_timeout: PONG_TIMEOUT,
        }
    }
}

impl BackoffConfig {
    #[cfg(test)]
    pub(crate) const fn new(base: Duration, max: Duration) -> Self {
        Self {
            base,
            max,
            pong_timeout: PONG_TIMEOUT,
        }
    }

    #[cfg(test)]
    pub(crate) const fn with_pong_timeout(mut self, pong_timeout: Duration) -> Self {
        self.pong_timeout = pong_timeout;
        self
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

/// `established` is `was_live || reached_unbound`: an attempt that went
/// `Live` or `Unbound` settled a generation, so the next rebind starts a new
/// budget (#682 PR-3, docs/banto-tagclient-partial-binding-design.md §7).
fn next_rebind_mode(
    tracker: &mut RebindTracker,
    mode: AttemptMode,
    established: bool,
    reason: ErrorKind,
) -> Option<AttemptMode> {
    if established || !matches!(mode, AttemptMode::Rebinding { .. }) {
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
    /// The attempt published `Live` (received data). Resets both the rebind
    /// budget and the reconnect backoff.
    was_live: bool,
    /// The attempt published `Unbound` (#682 PR-3). Resets only the rebind
    /// budget; the reconnect backoff keeps growing until data arrives, so a
    /// peer that drops subscription-less sockets cannot cause a fixed-delay
    /// reconnect loop (design §7).
    reached_unbound: bool,
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
        if let Some(next_mode) = next_rebind_mode(
            rebind_tracker,
            mode,
            failure.was_live || failure.reached_unbound,
            kind,
        ) {
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
        let attempt = run_attempt(
            rest,
            requests,
            subscription_id,
            state_tx,
            &mut stop,
            mode,
            backoff.pong_timeout,
        )
        .await;
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
        // #446: a close 1008 (`is_credential_rejection`) stops here too, with
        // its reason kept in `last_error`. The Hub sends 1008 only after it
        // confirmed the credential is unusable, so reconnecting with the same
        // key would only be rejected again (see `crate::close`).
        if error.kind() == ErrorKind::Unauthorized || error.kind().is_credential_rejection() {
            tracing::warn!(
                error_kind = error.kind().as_str(),
                "banto-tagclient worker stopping: banto-hub rejected credentials"
            );
            state_tx.send_replace(TagClientState::unauthorized_because(error.kind()));
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
    pong_timeout: Duration,
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
    let mut reached_unbound = false;
    run_generation_inner(
        rest,
        requests,
        subscription_id,
        state_tx,
        stop,
        mode,
        pong_timeout,
        &mut was_live,
        &mut reached_unbound,
    )
    .await
    .map_err(|error| AttemptFailure {
        error,
        was_live,
        reached_unbound,
    })
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
        PONG_TIMEOUT,
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

#[allow(
    clippy::too_many_arguments,
    reason = "one attempt's borrowed context; the two flags report how far it got"
)]
async fn run_generation_inner(
    rest: &RestClient,
    requests: &[BindingRequest],
    subscription_id: i64,
    state_tx: &watch::Sender<TagClientState>,
    stop: &mut oneshot::Receiver<()>,
    mode: AttemptMode,
    pong_timeout: Duration,
    was_live: &mut bool,
    reached_unbound: &mut bool,
) -> Result<()> {
    if requests.is_empty() {
        return Err(Error::new(ErrorKind::InvalidTagSelection));
    }
    let catalog = tokio::select! {
        biased;
        _ = &mut *stop => return Err(stopped()),
        result = rest.fetch_catalog() => result?,
    };
    // R2: every attempt that fetched a catalog takes a new generation number,
    // even if it later fails the publish gate.
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let catalog = Arc::new(catalog);
    let resolution = Arc::new(resolve_bindings(requests, &catalog.tags)?);
    // #682 PR-2 (partial binding, design §8 P2): only the resolved names are
    // subscribed (WS `subscribe`), fetched (REST `?tags=`), and allowed by the
    // publish gate. Unresolved requests stay in `resolution.unresolved` of the
    // published snapshot without a value.
    let tags = resolution
        .resolved
        .iter()
        .map(|binding| binding.external_name.clone())
        .collect::<Vec<_>>();
    // #682 PR-3: with nothing resolved there is nothing to subscribe; the
    // WebSocket is still opened and the attempt settles as `Unbound`.
    let gate = if tags.is_empty() {
        None
    } else {
        Some(PublishGate::new(subscription_id, tags.clone())?)
    };

    if matches!(mode, AttemptMode::Connecting) {
        state_tx.send_replace(TagClientState::new(TagClientConnectionState::Handshaking));
    }
    let mut connection = tokio::select! {
        biased;
        _ = &mut *stop => return Err(stopped()),
        result = rest.connect_stream() => result?,
    };
    let Some(mut gate) = gate else {
        return run_unbound(
            rest,
            requests,
            &mut connection,
            &catalog,
            generation,
            state_tx,
            stop,
            pong_timeout,
            reached_unbound,
        )
        .await;
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
    publish_snapshot(&gate, generation, &catalog, &resolution, state_tx, was_live)?;

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
            publish_snapshot(&gate, generation, &catalog, &resolution, state_tx, was_live)?;
        }
    }
}

/// The `Unbound` branch of one attempt (#682 PR-3,
/// docs/banto-tagclient-partial-binding-design.md §5.1 / §5.2 / §7).
///
/// The order is load-bearing:
/// 1. `ping` (stop-interruptible like `subscribe_on_change`), then wait for
///    `pong`. The Hub answers `pong` only from the request loop that runs
///    after it registered its `revision_rx`, so after `pong` every later
///    configuration change arrives as `config_changed`. A WebSocket connect
///    alone does not prove that (the Hub's socket handler starts in
///    `on_upgrade`, possibly later).
/// 2. Fetch the catalog again. A different `revision` (a change before the
///    registration) is `revision_mismatch`; a different `run_id` (a Hub
///    restart whose revision happened to match) is
///    `runtime_metadata_mismatch`.
/// 3. Resolve again against the second catalog: if a requested name appears
///    there although the revision matched (a torn Hub read), treat it as
///    `revision_mismatch`. Kept even with the Hub fixed, so the SDK alone
///    keeps `Unbound` correct.
/// 4. Only then publish `Unbound` with the second catalog, and wait for
///    `config_changed` (rebind) or a disconnect.
///
/// `receive_text()` errors are returned unchanged everywhere, so a close
/// 1008 still stops as `Unauthorized` with its reason (#446).
#[allow(
    clippy::too_many_arguments,
    reason = "one attempt's borrowed context, mirroring run_generation_inner"
)]
async fn run_unbound(
    rest: &RestClient,
    requests: &[BindingRequest],
    connection: &mut crate::ws_transport::WebSocketConnection,
    catalog: &crate::types::CatalogSnapshot,
    generation: u64,
    state_tx: &watch::Sender<TagClientState>,
    stop: &mut oneshot::Receiver<()>,
    pong_timeout: Duration,
    reached_unbound: &mut bool,
) -> Result<()> {
    tokio::select! {
        biased;
        _ = &mut *stop => {
            connection.close_best_effort().await;
            return Err(stopped());
        }
        result = connection.send_ping() => result?,
    }
    // One deadline for the whole wait (frames other than `pong` do not
    // extend it). Expiry is a retryable `Transport` failure: the connection
    // is dropped and the supervisor retries with its normal backoff.
    let pong_deadline = tokio::time::Instant::now() + pong_timeout;
    loop {
        let text = tokio::select! {
            biased;
            _ = &mut *stop => {
                connection.close_best_effort().await;
                return Err(stopped());
            }
            _ = tokio::time::sleep_until(pong_deadline) => {
                connection.close_best_effort().await;
                return Err(Error::new(ErrorKind::Transport));
            }
            result = connection.receive_text() => result?,
        };
        match parse_unbound_wire(&text)? {
            UnboundWire::Pong => break,
            UnboundWire::ConfigChanged => return Err(Error::new(ErrorKind::RevisionMismatch)),
            UnboundWire::Ignored => {}
        }
    }

    let recheck = tokio::select! {
        biased;
        _ = &mut *stop => {
            connection.close_best_effort().await;
            return Err(stopped());
        }
        result = rest.fetch_catalog() => result?,
    };
    if recheck.revision != catalog.revision {
        return Err(Error::new(ErrorKind::RevisionMismatch));
    }
    if recheck.run_id != catalog.run_id {
        return Err(Error::new(ErrorKind::RuntimeMetadataMismatch));
    }
    let resolution = resolve_bindings(requests, &recheck.tags)?;
    if !resolution.resolved.is_empty() {
        return Err(Error::new(ErrorKind::RevisionMismatch));
    }
    state_tx.send_replace(TagClientState::unbound(GenerationSnapshot {
        generation,
        catalog: Arc::new(recheck),
        resolution: Arc::new(resolution),
        values: None,
    }));
    *reached_unbound = true;

    loop {
        let text = tokio::select! {
            biased;
            _ = &mut *stop => {
                connection.close_best_effort().await;
                return Err(stopped());
            }
            result = connection.receive_text() => result?,
        };
        match parse_unbound_wire(&text)? {
            UnboundWire::ConfigChanged => return Err(Error::new(ErrorKind::RevisionMismatch)),
            UnboundWire::Pong | UnboundWire::Ignored => {}
        }
    }
}

fn publish_snapshot(
    gate: &PublishGate,
    generation: u64,
    catalog: &Arc<crate::types::CatalogSnapshot>,
    resolution: &Arc<crate::binding::BindingResolution>,
    state_tx: &watch::Sender<TagClientState>,
    was_live: &mut bool,
) -> Result<()> {
    let values = gate.finalize(catalog)?;
    check_live_complete(catalog, resolution, &values)?;
    let mut state = TagClientState::new(TagClientConnectionState::Stopped);
    state.publish(GenerationSnapshot {
        generation,
        catalog: Arc::clone(catalog),
        resolution: Arc::clone(resolution),
        values: Some(Arc::new(values)),
    });
    state_tx.send_replace(state);
    *was_live = true;
    Ok(())
}

/// Before publishing `Live`, require that every resolved binding has a
/// catalog row and a value row (R4), so `GenerationSnapshot::binding()` never
/// yields `Resolved { value: None }` while `Live`.
fn check_live_complete(
    catalog: &crate::types::CatalogSnapshot,
    resolution: &crate::binding::BindingResolution,
    values: &crate::types::ValuesSnapshot,
) -> Result<()> {
    let catalog_names: std::collections::HashSet<&str> = catalog
        .tags
        .iter()
        .map(|tag| tag.external_name.as_str())
        .collect();
    let value_names: std::collections::HashSet<&str> = values
        .values
        .iter()
        .map(|value| value.tag.as_str())
        .collect();
    for binding in &resolution.resolved {
        let name = binding.external_name.as_str();
        if !catalog_names.contains(name) || !value_names.contains(name) {
            return Err(Error::new(ErrorKind::ProtocolError));
        }
    }
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
        binding::{UnresolvedBinding, UnresolvedReason},
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

    fn resolved(name: &str) -> crate::binding::BindingResolution {
        crate::binding::BindingResolution {
            resolved: vec![crate::binding::ResolvedBinding {
                binding_key: format!("bk-{name}"),
                external_name: name.into(),
                tag_key: format!("key:{name}"),
            }],
            unresolved: vec![],
        }
    }

    #[test]
    fn check_live_complete_rejects_missing_value_row() {
        let resolution = resolved("a");
        let catalog = catalog(vec![tag(StableTagId::new(1, 1, 1), "a")]);
        let values = values(vec![value("b", 1.0, 10)]);
        let err = check_live_complete(&catalog, &resolution, &values).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::ProtocolError);
    }

    #[test]
    fn check_live_complete_rejects_missing_catalog_row() {
        let resolution = resolved("a");
        let catalog = catalog(vec![tag(StableTagId::new(1, 1, 1), "b")]);
        let values = values(vec![value("a", 1.0, 10)]);
        let err = check_live_complete(&catalog, &resolution, &values).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::ProtocolError);
    }

    #[test]
    fn check_live_complete_accepts_complete_generation() {
        let resolution = resolved("a");
        let catalog = catalog(vec![tag(StableTagId::new(1, 1, 1), "a")]);
        let values = values(vec![value("a", 1.0, 10)]);
        check_live_complete(&catalog, &resolution, &values).unwrap();
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
        serve_stream_after_catalog(listener, name, values_snapshot).await
    }

    /// The WS subscribe / first data frame / REST values part of one
    /// generation, after its catalog response has been written.
    async fn serve_stream_after_catalog(
        listener: &TcpListener,
        name: &str,
        values_snapshot: crate::types::ValuesSnapshot,
    ) -> WebSocketStream<tokio::net::TcpStream> {
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

    /// Each of `attempts` attempts finds nothing resolved, so it opens the WS
    /// and pings; the re-check catalog after `pong` has a new revision, so it
    /// fails `revision_mismatch` (design §5.1). Reports whether a further
    /// attempt connected within 40 ms.
    async fn serve_unresolved_attempts_and_check_backoff(
        listener: TcpListener,
        attempts: usize,
        fifth_connection: oneshot::Sender<bool>,
    ) {
        let log = HubLog::default();
        let mut sockets = Vec::new();
        for attempt in 0..attempts as u64 {
            sockets.push(
                serve_unbound_handshake(
                    &listener,
                    &empty_catalog(attempt * 2 + 1, Some(7)),
                    &empty_catalog(attempt * 2 + 2, Some(7)),
                    &log,
                )
                .await,
            );
        }
        let connected = tokio::time::timeout(Duration::from_millis(40), listener.accept())
            .await
            .is_ok();
        let _ = fifth_connection.send(connected);
        drop(sockets);
    }

    async fn serve_rebind_catalog_wait(listener: TcpListener, second_started: oneshot::Sender<()>) {
        // The first attempt's re-check catalog has a new revision, so the
        // worker rebinds and its next catalog request is held.
        let log = HubLog::default();
        let socket = serve_unbound_handshake(
            &listener,
            &empty_catalog(1, Some(7)),
            &empty_catalog(2, Some(7)),
            &log,
        )
        .await;
        let (mut second, _) = listener.accept().await.unwrap();
        read_http_request(&mut second).await;
        second_started.send(()).unwrap();
        let mut buffer = [0_u8; 64];
        let _ = tokio::time::timeout(Duration::from_secs(1), second.read(&mut buffer)).await;
        drop(socket);
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

    /// #682 R1 / R4 / R6: the published snapshot pairs values with the catalog of
    /// the same generation, `binding(key)` returns the live value, and cloning
    /// the state shares the `Arc`s. Returns the generation number.
    fn assert_same_generation(state: &TagClientState, binding_key: &str, expected: f64) -> u64 {
        let snapshot = state.snapshot().unwrap();
        let values = snapshot.values.as_deref().unwrap();
        assert_eq!(values.revision, snapshot.catalog.revision);
        assert_eq!(values.run_id, snapshot.catalog.run_id);
        assert_eq!(values.collection_mode, snapshot.catalog.collection_mode);
        assert_eq!(snapshot.revision(), values.revision);
        assert_eq!(state.current(), Some(values));
        match snapshot.binding(binding_key) {
            Some(crate::types::BindingView::Resolved {
                binding,
                tag,
                value,
            }) => {
                assert_eq!(binding.external_name, tag.external_name);
                assert_eq!(value.unwrap().v, Some(expected));
            }
            other => panic!("expected a resolved live binding, got {other:?}"),
        }
        let cloned = state.clone();
        assert!(Arc::ptr_eq(cloned.snapshot().unwrap(), snapshot));
        snapshot.generation
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

    /// The same external name after a delete-and-recreate: a fresh numeric ID.
    /// Binding is by name, so this must resolve exactly like the original.
    fn recreated_catalog(
        name: &str,
        revision: u64,
        run_id: Option<u64>,
        mode: CollectionMode,
    ) -> CatalogSnapshot {
        let mut snapshot = named_catalog(name, revision, run_id, mode);
        snapshot.tags[0].ids = StableTagId::new(1, 1, 9);
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
            reached_unbound: false,
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
            reached_unbound: false,
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
                    reached_unbound: false,
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
                    reached_unbound: false,
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
    fn reaching_unbound_resets_only_the_rebind_budget_not_the_backoff() {
        // design §7: `Unbound` settles a generation, so a later
        // config_changed starts a fresh rebind budget, but the reconnect
        // backoff keeps growing until a `Live` generation received data.
        let backoff = BackoffConfig::new(Duration::from_secs(1), Duration::from_secs(30));
        let mut retry_tracker = RetryTracker::new();
        let mut rebind_tracker = RebindTracker::new();
        let mut mode = AttemptMode::Connecting;
        let unbound_rebind = AttemptFailure {
            error: Error::new(ErrorKind::RevisionMismatch),
            was_live: false,
            reached_unbound: true,
        };
        // Many Unbound -> config_changed -> Unbound rounds never run out.
        for _ in 0..8 {
            mode = match plan_failure(
                &mut retry_tracker,
                &mut rebind_tracker,
                mode,
                unbound_rebind,
            ) {
                FailurePlan::Rebind(next_mode) => next_mode,
                other => panic!("an Unbound rebind must not fall back: {other:?}"),
            };
        }
        let unbound_disconnect = AttemptFailure {
            error: Error::new(ErrorKind::Transport),
            was_live: false,
            reached_unbound: true,
        };
        for expected in [1, 2, 4] {
            assert_eq!(
                plan_failure(
                    &mut retry_tracker,
                    &mut rebind_tracker,
                    mode,
                    unbound_disconnect
                ),
                FailurePlan::Backoff
            );
            assert_eq!(
                retry_tracker.delay_after_failure(backoff, unbound_disconnect.was_live),
                Duration::from_secs(expected)
            );
            mode = AttemptMode::Connecting;
        }
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
            state.publish(GenerationSnapshot::for_values(
                number as u64,
                values(vec![value("tag", number as f64, number)]),
            ));
            sender.send_replace(state);
            latest = Some(number as f64);
        }
        assert_eq!(receiver.borrow().current().unwrap().values[0].v, latest);
        sender.send_replace(TagClientState::new(TagClientConnectionState::Stopped));
        assert_eq!(receiver.borrow().current(), None);
    }

    #[tokio::test]
    async fn config_changed_burst_rebinds_once_and_resolves_the_recreated_same_name() {
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
                "alpha",
                named_catalog("alpha", 1, Some(7), CollectionMode::Configured),
                named_values("alpha", 10.0, 1, Some(7), CollectionMode::Configured),
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
                "alpha",
                recreated_catalog("alpha", 1, Some(7), CollectionMode::Configured),
                named_values("alpha", 20.0, 1, Some(7), CollectionMode::Configured),
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
            external_name: "alpha".into(),
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
        assert_eq!(receiver.borrow().current().unwrap().values[0].tag, "alpha");
        // #682: the first Live carries one consistent generation.
        let first_generation = assert_same_generation(&receiver.borrow(), "stable", 10.0);
        release_old_tx.send(()).unwrap();
        wait_rebinding(&mut receiver, ErrorKind::RevisionMismatch).await;
        assert_eq!(receiver.borrow().current(), None);
        // R3: Rebinding drops the old generation together with `current`.
        assert_eq!(receiver.borrow().snapshot(), None);
        release_rebind_tx.send(()).unwrap();
        second_live_rx.await.unwrap();
        wait_state(&mut receiver, TagClientConnectionState::Live, None).await;
        let current = receiver.borrow().current().unwrap().clone();
        assert_eq!(current.values[0].tag, "alpha");
        assert_eq!(current.values[0].v, Some(20.0));
        // R2: the rebind took a newer generation (a gate failure may skip numbers).
        let second_generation = assert_same_generation(&receiver.borrow(), "stable", 20.0);
        assert!(second_generation > first_generation);
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
                    "alpha",
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
                    r#"{"op":"data","id":1,"t":10,"values":[{"tag":"alpha","v":10,"q":"good","t":10}]}"#.into(),
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
                    "alpha",
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
                "alpha",
                recreated_catalog("alpha", 1, Some(7), CollectionMode::Configured),
                named_values("alpha", 20.0, 1, Some(7), CollectionMode::Configured),
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
            external_name: "alpha".into(),
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
        assert_eq!(receiver.borrow().current().unwrap().values[0].tag, "alpha");
        assert_eq!(receiver.borrow().current().unwrap().values[0].v, Some(20.0));
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
            external_name: "alpha".into(),
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

    fn binding_requests(pairs: &[(&str, &str)]) -> Vec<BindingRequest> {
        pairs
            .iter()
            .map(|(key, name)| BindingRequest {
                binding_key: (*key).into(),
                external_name: (*name).into(),
            })
            .collect()
    }

    fn multi_catalog(names: &[&str], revision: u64) -> CatalogSnapshot {
        let mut snapshot = catalog(
            names
                .iter()
                .enumerate()
                .map(|(index, name)| tag(StableTagId::new(1, 1, index as i64 + 1), name))
                .collect(),
        );
        snapshot.revision = revision;
        snapshot
    }

    /// What the mock Hub received from the client for one generation: the
    /// `tags` of the WS `subscribe` and of the REST `?tags=` query.
    #[derive(Debug)]
    struct ReceivedSelection {
        subscribed: Vec<String>,
        fetched: Vec<String>,
    }

    /// #682 PR-2: one generation for several names. `live` is the names (and
    /// REST values) the Hub publishes; the selection the client asked for is
    /// returned so the test can assert unresolved names were never requested.
    async fn serve_multi_generation(
        listener: &TcpListener,
        catalog_snapshot: CatalogSnapshot,
        live: &[(&str, f64)],
    ) -> (WebSocketStream<tokio::net::TcpStream>, ReceivedSelection) {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_http_request(&mut stream).await;
        assert!(request.starts_with("GET /api/v1/tags HTTP/1.1"));
        let revision = catalog_snapshot.revision;
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
        let Message::Text(text) = subscription else {
            panic!("expected a subscribe text frame, got {subscription:?}");
        };
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["op"], "subscribe");
        let subscribed = parsed["tags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tag| tag.as_str().unwrap().to_owned())
            .collect();
        let frame_values = live
            .iter()
            .map(|(name, _)| format!(r#"{{"tag":"{name}","v":1,"q":"good","t":10}}"#))
            .collect::<Vec<_>>()
            .join(",");
        socket
            .send(Message::Text(
                format!(r#"{{"op":"data","id":1,"t":10,"values":[{frame_values}]}}"#).into(),
            ))
            .await
            .unwrap();

        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_http_request(&mut stream).await;
        let line = request.lines().next().unwrap();
        let query = line
            .strip_prefix("GET /api/v1/values?tags=")
            .and_then(|rest| rest.strip_suffix(" HTTP/1.1"))
            .unwrap_or_else(|| panic!("unexpected values request {line}"));
        let fetched = query.split("%2C").map(str::to_owned).collect();
        let mut snapshot = values(
            live.iter()
                .map(|(name, number)| value(name, *number, 10))
                .collect(),
        );
        snapshot.revision = revision;
        write_json(&mut stream, serde_json::to_string(&snapshot).unwrap()).await;
        (
            socket,
            ReceivedSelection {
                subscribed,
                fetched,
            },
        )
    }

    /// Wait for a `Live` state of a generation newer than `after`.
    async fn wait_live_after(
        receiver: &mut watch::Receiver<TagClientState>,
        after: u64,
    ) -> TagClientState {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let state = receiver.borrow().clone();
                if state.connection_state() == TagClientConnectionState::Live
                    && state.snapshot().is_some_and(|s| s.generation > after)
                {
                    return state;
                }
                receiver.changed().await.unwrap();
            }
        })
        .await
        .unwrap()
    }

    fn resolved_names(state: &TagClientState) -> Vec<String> {
        state
            .snapshot()
            .unwrap()
            .resolution
            .resolved
            .iter()
            .map(|binding| binding.external_name.clone())
            .collect()
    }

    fn value_names(state: &TagClientState) -> Vec<String> {
        state
            .current()
            .unwrap()
            .values
            .iter()
            .map(|value| value.tag.clone())
            .collect()
    }

    fn not_in_catalog(binding_key: &str, external_name: &str) -> UnresolvedBinding {
        UnresolvedBinding {
            binding_key: binding_key.into(),
            external_name: external_name.into(),
            reason: UnresolvedReason::NotInCatalog,
        }
    }

    fn spawn_partial_worker(
        address: String,
        requests: Vec<BindingRequest>,
    ) -> (
        watch::Receiver<TagClientState>,
        oneshot::Sender<()>,
        tokio::task::JoinHandle<Result<()>>,
    ) {
        let (sender, receiver) =
            watch::channel(TagClientState::new(TagClientConnectionState::Stopped));
        let (stop_tx, stop_rx) = oneshot::channel();
        let rest_client = client(address);
        let task = tokio::spawn(async move {
            run_supervisor_with_config(
                &rest_client,
                &requests,
                1,
                &sender,
                stop_rx,
                BackoffConfig::new(Duration::from_millis(1), Duration::from_millis(5)),
            )
            .await
        });
        (receiver, stop_tx, task)
    }

    async fn stop_partial_worker(
        stop_tx: oneshot::Sender<()>,
        task: tokio::task::JoinHandle<Result<()>>,
    ) {
        stop_tx.send(()).unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .is_ok());
    }

    /// #682 PR-2 (design §8 P2, acceptance 1): one resolved and one missing
    /// name go `Live` with only the resolved value, and the missing name is
    /// neither subscribed nor fetched.
    #[tokio::test]
    async fn mixed_resolution_goes_live_and_requests_only_the_resolved_name() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let (received_tx, received_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let (socket, received) =
                serve_multi_generation(&listener, multi_catalog(&["alpha"], 1), &[("alpha", 20.0)])
                    .await;
            received_tx.send(received).unwrap();
            release_rx.await.unwrap();
            drop(socket);
        });
        let (mut receiver, stop_tx, task) =
            spawn_partial_worker(address, binding_requests(&[("a", "alpha"), ("b", "beta")]));

        let received = received_rx.await.unwrap();
        let state = wait_live_after(&mut receiver, 0).await;
        assert_eq!(received.subscribed, ["alpha"]);
        assert_eq!(received.fetched, ["alpha"]);
        assert_same_generation(&state, "a", 20.0);
        assert_eq!(value_names(&state), ["alpha"]);
        assert_eq!(resolved_names(&state), ["alpha"]);
        let snapshot = state.snapshot().unwrap();
        assert_eq!(
            snapshot.resolution.unresolved,
            [not_in_catalog("b", "beta")]
        );
        match snapshot.binding("b") {
            Some(crate::types::BindingView::Unresolved(unresolved)) => {
                assert_eq!(unresolved.external_name, "beta");
                assert_eq!(unresolved.reason, UnresolvedReason::NotInCatalog);
            }
            other => panic!("expected an unresolved binding, got {other:?}"),
        }
        assert_eq!(state.last_error(), None);

        release_tx.send(()).unwrap();
        stop_partial_worker(stop_tx, task).await;
        server.await.unwrap();
    }

    /// #682 PR-2 (design §8 P6, acceptance 2 from `Live`): a name created
    /// while the other one is `Live` resolves on the `config_changed` rebind.
    #[tokio::test]
    async fn created_name_resolves_on_the_config_changed_rebind_while_live() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let (first_tx, first_rx) = oneshot::channel();
        let (create_tx, create_rx) = oneshot::channel::<()>();
        let (second_tx, second_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let (mut old_socket, first) =
                serve_multi_generation(&listener, multi_catalog(&["alpha"], 1), &[("alpha", 20.0)])
                    .await;
            first_tx.send(first).unwrap();
            create_rx.await.unwrap();
            old_socket
                .send(Message::Text(
                    r#"{"op":"config_changed","revision":2}"#.into(),
                ))
                .await
                .unwrap();
            let (new_socket, second) = serve_multi_generation(
                &listener,
                multi_catalog(&["alpha", "beta"], 2),
                &[("alpha", 21.0), ("beta", 30.0)],
            )
            .await;
            second_tx.send(second).unwrap();
            release_rx.await.unwrap();
            drop(new_socket);
            drop(old_socket);
        });
        let (mut receiver, stop_tx, task) =
            spawn_partial_worker(address, binding_requests(&[("a", "alpha"), ("b", "beta")]));

        let first = first_rx.await.unwrap();
        let state = wait_live_after(&mut receiver, 0).await;
        assert_eq!(first.subscribed, ["alpha"]);
        let first_generation = state.snapshot().unwrap().generation;
        assert_eq!(
            state.snapshot().unwrap().resolution.unresolved,
            [not_in_catalog("b", "beta")]
        );

        create_tx.send(()).unwrap();
        let second = second_rx.await.unwrap();
        let state = wait_live_after(&mut receiver, first_generation).await;
        assert_eq!(second.subscribed, ["alpha", "beta"]);
        assert_eq!(second.fetched, ["alpha", "beta"]);
        assert_eq!(resolved_names(&state), ["alpha", "beta"]);
        assert!(state.snapshot().unwrap().resolution.unresolved.is_empty());
        assert_eq!(value_names(&state), ["alpha", "beta"]);
        assert_same_generation(&state, "b", 30.0);

        release_tx.send(()).unwrap();
        stop_partial_worker(stop_tx, task).await;
        server.await.unwrap();
    }

    /// #682 PR-2 (design §8 P5, acceptance 3): a rename while `Live` moves
    /// only that binding to `unresolved`; the other keeps publishing, and a
    /// similar name (`alpha2`) is never assigned to the old binding.
    #[tokio::test]
    async fn rename_while_live_moves_only_that_binding_to_unresolved() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let (first_tx, first_rx) = oneshot::channel();
        let (rename_tx, rename_rx) = oneshot::channel::<()>();
        let (second_tx, second_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let (mut old_socket, first) = serve_multi_generation(
                &listener,
                multi_catalog(&["alpha", "beta"], 1),
                &[("alpha", 20.0), ("beta", 30.0)],
            )
            .await;
            first_tx.send(first).unwrap();
            rename_rx.await.unwrap();
            old_socket
                .send(Message::Text(
                    r#"{"op":"config_changed","revision":2}"#.into(),
                ))
                .await
                .unwrap();
            let (new_socket, second) = serve_multi_generation(
                &listener,
                multi_catalog(&["alpha2", "beta"], 2),
                &[("beta", 31.0)],
            )
            .await;
            second_tx.send(second).unwrap();
            release_rx.await.unwrap();
            drop(new_socket);
            drop(old_socket);
        });
        let (mut receiver, stop_tx, task) =
            spawn_partial_worker(address, binding_requests(&[("a", "alpha"), ("b", "beta")]));

        let first = first_rx.await.unwrap();
        let state = wait_live_after(&mut receiver, 0).await;
        assert_eq!(first.subscribed, ["alpha", "beta"]);
        assert_eq!(resolved_names(&state), ["alpha", "beta"]);
        let first_generation = state.snapshot().unwrap().generation;

        rename_tx.send(()).unwrap();
        let second = second_rx.await.unwrap();
        let state = wait_live_after(&mut receiver, first_generation).await;
        assert_eq!(second.subscribed, ["beta"]);
        assert_eq!(second.fetched, ["beta"]);
        assert_eq!(resolved_names(&state), ["beta"]);
        let snapshot = state.snapshot().unwrap();
        assert_eq!(
            snapshot.resolution.unresolved,
            [not_in_catalog("a", "alpha")]
        );
        assert!(matches!(
            snapshot.binding("a"),
            Some(crate::types::BindingView::Unresolved(_))
        ));
        assert_eq!(value_names(&state), ["beta"]);
        assert_same_generation(&state, "b", 31.0);

        release_tx.send(()).unwrap();
        stop_partial_worker(stop_tx, task).await;
        server.await.unwrap();
    }

    /// #682 PR-2 (design §8 P3, acceptance 7): every duplicate class still
    /// fails the whole attempt through the worker - `Stopped` with the kind in
    /// `last_error`, no snapshot, no WebSocket - even though other requested
    /// names resolve and a partial result would be possible.
    #[tokio::test]
    async fn every_duplicate_class_stops_the_worker_instead_of_a_partial_live() {
        let cases = [
            (
                binding_requests(&[("same", "alpha"), ("same", "beta")]),
                vec!["alpha", "beta"],
                ErrorKind::DuplicateBindingKey,
            ),
            (
                binding_requests(&[("a", "alpha"), ("b", "alpha"), ("c", "beta")]),
                vec!["alpha", "beta"],
                ErrorKind::DuplicateRequestedExternalName,
            ),
            (
                binding_requests(&[("a", "alpha"), ("c", "gamma")]),
                vec!["alpha", "beta", "beta"],
                ErrorKind::DuplicateCatalogExternalName,
            ),
        ];
        for (requests, names, expected) in cases {
            let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let address = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                read_http_request(&mut stream).await;
                write_json(
                    &mut stream,
                    serde_json::to_string(&multi_catalog(&names, 1)).unwrap(),
                )
                .await;
                // No WebSocket (nor a second catalog) may follow.
                tokio::time::timeout(Duration::from_millis(50), listener.accept())
                    .await
                    .is_err()
            });
            let (mut receiver, _stop_tx, task) = spawn_partial_worker(address, requests);
            let result = tokio::time::timeout(Duration::from_secs(1), task)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(result.unwrap_err().kind(), expected);
            wait_state(
                &mut receiver,
                TagClientConnectionState::Stopped,
                Some(expected),
            )
            .await;
            assert!(receiver.borrow().snapshot().is_none());
            assert_eq!(receiver.borrow().current(), None);
            assert!(
                server.await.unwrap(),
                "{expected:?} opened another connection"
            );
        }
    }

    #[tokio::test]
    async fn four_rebind_failures_enter_normal_backoff_without_fast_fifth_attempt() {
        // #682 PR-3: four Unbound handshakes whose re-check catalog moved on.
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
            external_name: "alpha".into(),
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
            Some(ErrorKind::RevisionMismatch),
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
            external_name: "alpha".into(),
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
                "alpha",
                named_catalog("alpha", 1, Some(7), CollectionMode::Configured),
                named_values("alpha", 10.0, 1, Some(7), CollectionMode::Configured),
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
                "alpha",
                recreated_catalog("alpha", 1, Some(7), CollectionMode::Configured),
                named_values("alpha", 20.0, 1, Some(7), CollectionMode::Configured),
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
            external_name: "alpha".into(),
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
                external_name: "alpha".into(),
            },
            BindingRequest {
                binding_key: "b".into(),
                external_name: "beta".into(),
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
            external_name: "alpha".into(),
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
            external_name: "alpha".into(),
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

    // --- #446: close 1008 and the reason -----------------------------------

    fn close_message(code: u16, reason: &str) -> Message {
        use tokio_tungstenite::tungstenite::protocol::{frame::coding::CloseCode, CloseFrame};
        Message::Close(Some(CloseFrame {
            code: CloseCode::from(code),
            reason: reason.into(),
        }))
    }

    fn one_request() -> Vec<BindingRequest> {
        vec![BindingRequest {
            binding_key: "stable".into(),
            external_name: "tag".into(),
        }]
    }

    /// Serve one live generation, close it with `code`/`reason`, then report
    /// whether the worker came back for another catalog within `window`.
    async fn serve_live_then_close(
        listener: TcpListener,
        code: u16,
        reason: &'static str,
        window: Duration,
    ) -> bool {
        let count = AtomicUsize::new(0);
        let mut socket = serve_named_generation(
            &listener,
            "tag",
            named_catalog("tag", 1, Some(7), CollectionMode::Configured),
            named_values("tag", 10.0, 1, Some(7), CollectionMode::Configured),
            &count,
            None,
        )
        .await;
        socket.send(close_message(code, reason)).await.unwrap();
        // Let the client complete the closing handshake.
        let _ = tokio::time::timeout(Duration::from_millis(200), socket.next()).await;
        tokio::time::timeout(window, listener.accept())
            .await
            .is_ok()
    }

    /// Every 1008 reason stops the worker as `Unauthorized` with the reason's
    /// own kind in `last_error`, and never reconnects (no second catalog
    /// request even with a 1 ms backoff). Tripped included: whether to try a
    /// tripped key again is the application's decision.
    #[tokio::test]
    async fn a_1008_close_stops_with_its_reason_and_does_not_reconnect() {
        for (reason, kind) in [
            ("api_key_revoked", ErrorKind::KeyRevoked),
            ("api_key_expired", ErrorKind::KeyExpired),
            ("api_key_tripped", ErrorKind::KeyTripped),
            ("api_key_not_found", ErrorKind::KeyNotFound),
            ("future_reason", ErrorKind::CredentialRejected),
        ] {
            let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let address = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(serve_live_then_close(
                listener,
                1008,
                reason,
                Duration::from_millis(300),
            ));
            let (sender, mut receiver) =
                watch::channel(TagClientState::new(TagClientConnectionState::Stopped));
            let (_stop_tx, stop_rx) = oneshot::channel();
            let rest_client = client(address);
            let requests = one_request();
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
            wait_state(
                &mut receiver,
                TagClientConnectionState::Unauthorized,
                Some(kind),
            )
            .await;
            assert_eq!(receiver.borrow().current(), None, "{reason}");
            let result = tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(result.unwrap_err().kind(), kind, "{reason}");
            assert!(
                !server.await.unwrap(),
                "{reason}: the worker reconnected after a 1008 close"
            );
            // The published state is still the classified one.
            assert_eq!(
                receiver.borrow().connection_state(),
                TagClientConnectionState::Unauthorized
            );
            assert_eq!(receiver.borrow().last_error(), Some(kind));
        }
    }

    /// An ordinary close (not 1008) keeps the existing behavior: reconnect
    /// from the catalog and become live again - whatever the reason text.
    #[tokio::test]
    async fn an_ordinary_close_still_reconnects() {
        for (code, reason) in [(1000, ""), (1013, "api_key_revoked")] {
            let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let address = format!("http://{}", listener.local_addr().unwrap());
            let catalog_count = Arc::new(AtomicUsize::new(0));
            let server_count = Arc::clone(&catalog_count);
            let (release_tx, release_rx) = oneshot::channel::<()>();
            let server = tokio::spawn(async move {
                let mut first = serve_named_generation(
                    &listener,
                    "tag",
                    named_catalog("tag", 1, Some(7), CollectionMode::Configured),
                    named_values("tag", 10.0, 1, Some(7), CollectionMode::Configured),
                    &server_count,
                    None,
                )
                .await;
                first.send(close_message(code, reason)).await.unwrap();
                let _ = tokio::time::timeout(Duration::from_millis(200), first.next()).await;
                let second = serve_named_generation(
                    &listener,
                    "tag",
                    named_catalog("tag", 1, Some(7), CollectionMode::Configured),
                    named_values("tag", 20.0, 1, Some(7), CollectionMode::Configured),
                    &server_count,
                    None,
                )
                .await;
                let _ = release_rx.await;
                drop(second);
            });
            let (sender, mut receiver) =
                watch::channel(TagClientState::new(TagClientConnectionState::Stopped));
            let (stop_tx, stop_rx) = oneshot::channel();
            let rest_client = client(address);
            let requests = one_request();
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
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    {
                        let state = receiver.borrow();
                        if state.connection_state() == TagClientConnectionState::Live
                            && state
                                .current()
                                .is_some_and(|current| current.values[0].v == Some(20.0))
                        {
                            return;
                        }
                    }
                    receiver.changed().await.unwrap();
                }
            })
            .await
            .unwrap_or_else(|_| panic!("code={code}: did not reconnect"));
            assert_eq!(catalog_count.load(Ordering::SeqCst), 2);
            stop_tx.send(()).unwrap();
            assert!(task.await.unwrap().is_ok());
            let _ = release_tx.send(());
            server.await.unwrap();
        }
    }

    // --- #682 PR-3: Unbound (design §5.1 / §5.2 / §7 / §12 PR-3) -----------

    /// What the mock Hub saw, in the order it saw it.
    type HubLog = Arc<Mutex<Vec<&'static str>>>;

    fn record(log: &HubLog, entry: &'static str) {
        log.lock().unwrap().push(entry);
    }

    fn logged(log: &HubLog) -> Vec<&'static str> {
        log.lock().unwrap().clone()
    }

    fn empty_catalog(revision: u64, run_id: Option<u64>) -> CatalogSnapshot {
        let mut snapshot = catalog(Vec::new());
        snapshot.revision = revision;
        snapshot.run_id = run_id;
        snapshot
    }

    async fn serve_catalog_once(listener: &TcpListener, snapshot: &CatalogSnapshot, log: &HubLog) {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_http_request(&mut stream).await;
        assert!(
            request.starts_with("GET /api/v1/tags HTTP/1.1"),
            "expected a catalog request, got {request}"
        );
        record(log, "GET catalog");
        write_json(&mut stream, serde_json::to_string(snapshot).unwrap()).await;
    }

    /// A synchronization point inside the second catalog request: the mock
    /// signals `read` once it has read the request (before answering) and
    /// answers only after `go` fired. Anything the worker publishes before the
    /// answer therefore happens before the test's look.
    struct SecondCatalogGate {
        read: oneshot::Sender<()>,
        go: oneshot::Receiver<()>,
    }

    /// Serve the second catalog request, gated by `gate`.
    async fn serve_catalog_gated(
        listener: &TcpListener,
        snapshot: &CatalogSnapshot,
        log: &HubLog,
        gate: SecondCatalogGate,
    ) {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_http_request(&mut stream).await;
        assert!(
            request.starts_with("GET /api/v1/tags HTTP/1.1"),
            "expected a catalog request, got {request}"
        );
        record(log, "GET catalog");
        gate.read.send(()).unwrap();
        gate.go.await.unwrap();
        write_json(&mut stream, serde_json::to_string(snapshot).unwrap()).await;
    }

    /// `serve_unbound_handshake`, but the second catalog is gated.
    async fn serve_unbound_handshake_gated(
        listener: &TcpListener,
        first: &CatalogSnapshot,
        second: &CatalogSnapshot,
        log: &HubLog,
        gate: SecondCatalogGate,
    ) -> WebSocketStream<tokio::net::TcpStream> {
        serve_catalog_once(listener, first, log).await;
        let mut socket = accept_ws(listener, log).await;
        expect_ping(&mut socket, log).await;
        send_pong(&mut socket, log).await;
        serve_catalog_gated(listener, second, log, gate).await;
        socket
    }

    /// Wait until the mock read the gated second catalog request, assert that
    /// the worker has published neither `Unbound` nor any snapshot, then let
    /// the mock answer.
    async fn assert_nothing_unbound_before_second_catalog(
        receiver: &watch::Receiver<TagClientState>,
        read_rx: oneshot::Receiver<()>,
        go_tx: oneshot::Sender<()>,
    ) {
        tokio::time::timeout(Duration::from_secs(3), read_rx)
            .await
            .expect("second catalog within 3s")
            .unwrap();
        {
            let state = receiver.borrow();
            assert_ne!(state.connection_state(), TagClientConnectionState::Unbound);
            assert!(state.snapshot().is_none());
        }
        go_tx.send(()).unwrap();
    }

    async fn accept_ws(
        listener: &TcpListener,
        log: &HubLog,
    ) -> WebSocketStream<tokio::net::TcpStream> {
        let (stream, _) = listener.accept().await.unwrap();
        let socket = accept_async(stream).await.unwrap();
        record(log, "WS");
        socket
    }

    /// The next application text frame from the client, or `None` once it
    /// closed the socket.
    async fn next_client_text(
        socket: &mut WebSocketStream<tokio::net::TcpStream>,
    ) -> Option<String> {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                match socket.next().await {
                    Some(Ok(Message::Text(text))) => return Some(text.to_string()),
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                    _ => return None,
                }
            }
        })
        .await
        .expect("client frame within 2s")
    }

    async fn expect_ping(socket: &mut WebSocketStream<tokio::net::TcpStream>, log: &HubLog) {
        let text = next_client_text(socket).await;
        assert_eq!(text.as_deref(), Some(r#"{"op":"ping"}"#));
        record(log, "ping");
    }

    async fn send_pong(socket: &mut WebSocketStream<tokio::net::TcpStream>, log: &HubLog) {
        record(log, "pong");
        socket
            .send(Message::Text(r#"{"op":"pong"}"#.into()))
            .await
            .unwrap();
    }

    async fn send_config_changed(
        socket: &mut WebSocketStream<tokio::net::TcpStream>,
        revision: u64,
    ) {
        socket
            .send(Message::Text(
                format!(r#"{{"op":"config_changed","revision":{revision}}}"#).into(),
            ))
            .await
            .unwrap();
    }

    /// One `Unbound` handshake as the Hub sees it: catalog, WS, `ping`,
    /// `pong`, catalog again. Returns the open socket.
    async fn serve_unbound_handshake(
        listener: &TcpListener,
        first: &CatalogSnapshot,
        second: &CatalogSnapshot,
        log: &HubLog,
    ) -> WebSocketStream<tokio::net::TcpStream> {
        serve_catalog_once(listener, first, log).await;
        let mut socket = accept_ws(listener, log).await;
        expect_ping(&mut socket, log).await;
        send_pong(&mut socket, log).await;
        serve_catalog_once(listener, second, log).await;
        socket
    }

    /// Every text frame the client sent until it closed the socket.
    async fn texts_until_close(socket: &mut WebSocketStream<tokio::net::TcpStream>) -> Vec<String> {
        let mut texts = Vec::new();
        while let Some(text) = next_client_text(socket).await {
            texts.push(text);
        }
        texts
    }

    /// Wait for an `Unbound` state of a generation newer than `after`.
    async fn wait_unbound_after(
        receiver: &mut watch::Receiver<TagClientState>,
        after: u64,
    ) -> TagClientState {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let state = receiver.borrow().clone();
                if state.connection_state() == TagClientConnectionState::Unbound
                    && state.snapshot().is_some_and(|s| s.generation > after)
                {
                    return state;
                }
                receiver.changed().await.unwrap();
            }
        })
        .await
        .expect("Unbound within 3s")
    }

    /// Record every connection state the watch shows until `stop` fires.
    fn record_states(
        mut receiver: watch::Receiver<TagClientState>,
    ) -> (
        oneshot::Sender<()>,
        tokio::task::JoinHandle<Vec<TagClientConnectionState>>,
    ) {
        let (stop_tx, mut stop_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut states = vec![receiver.borrow_and_update().connection_state()];
            loop {
                tokio::select! {
                    _ = &mut stop_rx => return states,
                    changed = receiver.changed() => {
                        if changed.is_err() {
                            return states;
                        }
                        states.push(receiver.borrow_and_update().connection_state());
                    }
                }
            }
        });
        (stop_tx, task)
    }

    fn spawn_worker_with_backoff(
        address: String,
        requests: Vec<BindingRequest>,
        backoff: BackoffConfig,
    ) -> (
        watch::Receiver<TagClientState>,
        oneshot::Sender<()>,
        tokio::task::JoinHandle<Result<()>>,
    ) {
        let (sender, receiver) =
            watch::channel(TagClientState::new(TagClientConnectionState::Stopped));
        let (stop_tx, stop_rx) = oneshot::channel();
        let rest_client = client(address);
        let task = tokio::spawn(async move {
            run_supervisor_with_config(&rest_client, &requests, 1, &sender, stop_rx, backoff).await
        });
        (receiver, stop_tx, task)
    }

    fn assert_unbound_shape(state: &TagClientState, unresolved: &[UnresolvedBinding]) {
        assert_eq!(state.connection_state(), TagClientConnectionState::Unbound);
        assert_eq!(state.last_error(), None);
        assert_eq!(state.current(), None);
        let snapshot = state.snapshot().expect("Unbound carries its snapshot");
        assert_eq!(snapshot.values, None);
        assert!(snapshot.resolution.resolved.is_empty());
        assert_eq!(snapshot.resolution.unresolved, unresolved);
        for binding in unresolved {
            assert!(matches!(
                snapshot.binding(&binding.binding_key),
                Some(crate::types::BindingView::Unresolved(_))
            ));
        }
    }

    /// Acceptance 4: nothing resolved → `Unbound` without a subscription. The
    /// Hub sees catalog → WS → `ping` → (nothing until `pong`) → catalog, and
    /// the published catalog is the second one.
    #[tokio::test]
    async fn all_unresolved_goes_unbound_after_ping_pong_and_a_second_catalog_without_subscribing()
    {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let log = HubLog::default();
        let server_log = Arc::clone(&log);
        let server = tokio::spawn(async move {
            serve_catalog_once(&listener, &empty_catalog(4, Some(7)), &server_log).await;
            let mut socket = accept_ws(&listener, &server_log).await;
            expect_ping(&mut socket, &server_log).await;
            // The client must wait for `pong` before fetching the catalog
            // again. (A negative check: it cannot fail a correct client.)
            assert!(
                tokio::time::timeout(Duration::from_millis(100), listener.accept())
                    .await
                    .is_err(),
                "the client fetched the catalog again before pong"
            );
            send_pong(&mut socket, &server_log).await;
            // The second catalog differs only in an unrelated tag.
            let mut second = empty_catalog(4, Some(7));
            second
                .tags
                .push(tag(StableTagId::new(1, 1, 5), "unrelated"));
            serve_catalog_once(&listener, &second, &server_log).await;
            texts_until_close(&mut socket).await
        });
        let (mut receiver, stop_tx, task) =
            spawn_partial_worker(address, binding_requests(&[("a", "alpha"), ("b", "beta")]));
        let state = wait_unbound_after(&mut receiver, 0).await;
        assert_unbound_shape(
            &state,
            &[not_in_catalog("a", "alpha"), not_in_catalog("b", "beta")],
        );
        let snapshot = state.snapshot().unwrap();
        assert_eq!(snapshot.revision(), 4);
        assert_eq!(snapshot.catalog.tags.len(), 1);
        assert_eq!(snapshot.catalog.tags[0].external_name, "unrelated");
        stop_partial_worker(stop_tx, task).await;
        let after_ping = server.await.unwrap();
        assert!(
            after_ping.is_empty(),
            "an Unbound connection sent {after_ping:?}"
        );
        assert_eq!(
            logged(&log),
            ["GET catalog", "WS", "ping", "pong", "GET catalog"]
        );
    }

    /// PR-2's `unresolved_catalog_rebinds_and_recovers_once_the_name_exists`,
    /// rewritten for `Unbound` (acceptance 2 from `Unbound`): `config_changed`
    /// while `Unbound` → `Rebinding` → the created name goes `Live`.
    #[tokio::test]
    async fn unresolved_catalog_goes_unbound_and_goes_live_once_the_name_exists() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let (unbound_seen_tx, unbound_seen_rx) = oneshot::channel::<()>();
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let log = HubLog::default();
            let mut unbound_socket = serve_unbound_handshake(
                &listener,
                &empty_catalog(1, Some(7)),
                &empty_catalog(1, Some(7)),
                &log,
            )
            .await;
            unbound_seen_rx.await.unwrap();
            // The tag is created: revision 2.
            send_config_changed(&mut unbound_socket, 2).await;
            let (socket, received) = serve_multi_generation(
                &listener,
                multi_catalog(&["resolved-name"], 2),
                &[("resolved-name", 20.0)],
            )
            .await;
            release_rx.await.unwrap();
            drop(socket);
            drop(unbound_socket);
            received
        });
        let (mut receiver, stop_tx, task) =
            spawn_partial_worker(address, binding_requests(&[("stable", "resolved-name")]));
        let unbound = wait_unbound_after(&mut receiver, 0).await;
        assert_unbound_shape(&unbound, &[not_in_catalog("stable", "resolved-name")]);
        let unbound_generation = unbound.snapshot().unwrap().generation;
        unbound_seen_tx.send(()).unwrap();
        let live = wait_live_after(&mut receiver, unbound_generation).await;
        assert_eq!(
            assert_same_generation(&live, "stable", 20.0),
            live.snapshot().unwrap().generation
        );
        assert_eq!(resolved_names(&live), ["resolved-name"]);
        assert!(live.snapshot().unwrap().resolution.unresolved.is_empty());
        release_tx.send(()).unwrap();
        stop_partial_worker(stop_tx, task).await;
        let received = server.await.unwrap();
        assert_eq!(received.subscribed, ["resolved-name"]);
    }

    /// PR-2's `rename_leaves_the_old_name_unresolved_until_the_same_name_is_recreated`,
    /// rewritten for `Unbound`: after a rename the only binding is
    /// unresolved, so the worker settles in `Unbound` (no backoff, no current
    /// value) and goes `Live` again once the old name is recreated.
    #[tokio::test]
    async fn rename_leaves_the_old_name_unbound_until_the_same_name_is_recreated() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let (rename_tx, rename_rx) = oneshot::channel::<()>();
        let (recreate_tx, recreate_rx) = oneshot::channel::<()>();
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let count = AtomicUsize::new(0);
            let mut old_socket = serve_named_generation(
                &listener,
                "old-name",
                named_catalog("old-name", 1, Some(7), CollectionMode::Configured),
                named_values("old-name", 10.0, 1, Some(7), CollectionMode::Configured),
                &count,
                None,
            )
            .await;
            rename_rx.await.unwrap();
            send_config_changed(&mut old_socket, 2).await;
            let renamed = named_catalog("new-name", 2, Some(7), CollectionMode::Configured);
            let log = HubLog::default();
            let mut unbound_socket =
                serve_unbound_handshake(&listener, &renamed, &renamed, &log).await;
            recreate_rx.await.unwrap();
            send_config_changed(&mut unbound_socket, 3).await;
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_http_request(&mut stream).await;
            assert!(request.starts_with("GET /api/v1/tags HTTP/1.1"));
            write_json(
                &mut stream,
                serde_json::to_string(&recreated_catalog(
                    "old-name",
                    3,
                    Some(7),
                    CollectionMode::Configured,
                ))
                .unwrap(),
            )
            .await;
            let new_socket = serve_stream_after_catalog(
                &listener,
                "old-name",
                named_values("old-name", 20.0, 3, Some(7), CollectionMode::Configured),
            )
            .await;
            release_rx.await.unwrap();
            drop(new_socket);
            drop(unbound_socket);
            drop(old_socket);
        });
        let (mut receiver, stop_tx, task) =
            spawn_partial_worker(address, binding_requests(&[("stable", "old-name")]));
        let (history_stop, history) = record_states(receiver.clone());
        let first = wait_live_after(&mut receiver, 0).await;
        assert_eq!(value_names(&first), ["old-name"]);
        rename_tx.send(()).unwrap();
        let unbound = wait_unbound_after(&mut receiver, first.snapshot().unwrap().generation).await;
        assert_unbound_shape(&unbound, &[not_in_catalog("stable", "old-name")]);
        assert_eq!(
            unbound.snapshot().unwrap().catalog.tags[0].external_name,
            "new-name"
        );
        recreate_tx.send(()).unwrap();
        let again = wait_live_after(&mut receiver, unbound.snapshot().unwrap().generation).await;
        let current = again.current().unwrap();
        assert_eq!(current.values[0].tag, "old-name");
        assert_eq!(current.values[0].v, Some(20.0));
        assert_eq!(
            again.snapshot().unwrap().catalog.tags[0].ids,
            StableTagId::new(1, 1, 9)
        );
        history_stop.send(()).unwrap();
        let states = history.await.unwrap();
        assert!(
            !states.contains(&TagClientConnectionState::Reconnecting),
            "{states:?}"
        );
        release_tx.send(()).unwrap();
        stop_partial_worker(stop_tx, task).await;
        server.await.unwrap();
    }

    /// Design §5.1 / §5.2 scenes 1-2: the Hub's socket handler registers its
    /// `revision_rx` late (it starts in `on_upgrade`), and a tag is created
    /// before that, so no `config_changed` is ever sent. The mock holds
    /// `pong` until the test created the tag; the second catalog then has a
    /// new revision and the worker rebinds to `Live` without ever publishing
    /// `Unbound`.
    #[tokio::test]
    async fn a_change_before_the_hub_registers_its_watch_never_publishes_unbound() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let (ping_seen_tx, ping_seen_rx) = oneshot::channel::<()>();
        let (created_tx, created_rx) = oneshot::channel::<()>();
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let (read_tx, read_rx) = oneshot::channel::<()>();
        let (go_tx, go_rx) = oneshot::channel::<()>();
        let gate = SecondCatalogGate {
            read: read_tx,
            go: go_rx,
        };
        let log = HubLog::default();
        let server_log = Arc::clone(&log);
        let server = tokio::spawn(async move {
            serve_catalog_once(&listener, &empty_catalog(1, Some(7)), &server_log).await;
            let mut socket = accept_ws(&listener, &server_log).await;
            expect_ping(&mut socket, &server_log).await;
            ping_seen_tx.send(()).unwrap();
            // The tag is created before the Hub registered its watch, so no
            // config_changed is sent for it.
            created_rx.await.unwrap();
            send_pong(&mut socket, &server_log).await;
            let created = multi_catalog(&["alpha"], 2);
            serve_catalog_gated(&listener, &created, &server_log, gate).await;
            let (live_socket, _) =
                serve_multi_generation(&listener, created, &[("alpha", 20.0)]).await;
            release_rx.await.unwrap();
            drop(live_socket);
            drop(socket);
        });
        let (mut receiver, stop_tx, task) =
            spawn_partial_worker(address, binding_requests(&[("a", "alpha")]));
        let (history_stop, history) = record_states(receiver.clone());
        ping_seen_rx.await.unwrap();
        created_tx.send(()).unwrap();
        assert_nothing_unbound_before_second_catalog(&receiver, read_rx, go_tx).await;
        let live = wait_live_after(&mut receiver, 0).await;
        assert_eq!(resolved_names(&live), ["alpha"]);
        history_stop.send(()).unwrap();
        let states = history.await.unwrap();
        assert!(
            !states.contains(&TagClientConnectionState::Unbound),
            "{states:?}"
        );
        assert!(states.contains(&TagClientConnectionState::Rebinding));
        assert_eq!(
            logged(&log),
            ["GET catalog", "WS", "ping", "pong", "GET catalog"]
        );
        release_tx.send(()).unwrap();
        stop_partial_worker(stop_tx, task).await;
        server.await.unwrap();
    }

    /// Design §5.2 scene 3: a `config_changed` that arrives while waiting for
    /// `pong` rebinds directly, without publishing `Unbound`.
    #[tokio::test]
    async fn config_changed_while_waiting_for_pong_rebinds_without_unbound() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let log = HubLog::default();
            serve_catalog_once(&listener, &empty_catalog(1, Some(7)), &log).await;
            let mut socket = accept_ws(&listener, &log).await;
            expect_ping(&mut socket, &log).await;
            send_config_changed(&mut socket, 2).await;
            let (live_socket, _) =
                serve_multi_generation(&listener, multi_catalog(&["alpha"], 2), &[("alpha", 20.0)])
                    .await;
            release_rx.await.unwrap();
            drop(live_socket);
            drop(socket);
            logged(&log)
        });
        let (mut receiver, stop_tx, task) =
            spawn_partial_worker(address, binding_requests(&[("a", "alpha")]));
        let (history_stop, history) = record_states(receiver.clone());
        wait_live_after(&mut receiver, 0).await;
        history_stop.send(()).unwrap();
        let states = history.await.unwrap();
        assert!(
            !states.contains(&TagClientConnectionState::Unbound),
            "{states:?}"
        );
        release_tx.send(()).unwrap();
        stop_partial_worker(stop_tx, task).await;
        // No second catalog of the first attempt: the next request after the
        // ping was already the rebind's catalog (served by
        // serve_multi_generation, not logged).
        assert_eq!(server.await.unwrap(), ["GET catalog", "WS", "ping"]);
    }

    /// Design §5.2 scene 14 (torn Hub read): the first catalog is (old tags,
    /// new revision) and the second (new tags, same revision). The revisions
    /// match, but resolving the second catalog finds the name, so the worker
    /// rebinds to `Live` without ever publishing `Unbound`.
    #[tokio::test]
    async fn a_torn_catalog_read_never_publishes_unbound() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let (read_tx, read_rx) = oneshot::channel::<()>();
        let (go_tx, go_rx) = oneshot::channel::<()>();
        let gate = SecondCatalogGate {
            read: read_tx,
            go: go_rx,
        };
        let server = tokio::spawn(async move {
            let log = HubLog::default();
            let torn = empty_catalog(2, Some(7));
            let created = multi_catalog(&["alpha"], 2);
            let socket =
                serve_unbound_handshake_gated(&listener, &torn, &created, &log, gate).await;
            let (live_socket, _) =
                serve_multi_generation(&listener, created, &[("alpha", 20.0)]).await;
            release_rx.await.unwrap();
            drop(live_socket);
            drop(socket);
        });
        let (mut receiver, stop_tx, task) =
            spawn_partial_worker(address, binding_requests(&[("a", "alpha")]));
        let (history_stop, history) = record_states(receiver.clone());
        assert_nothing_unbound_before_second_catalog(&receiver, read_rx, go_tx).await;
        let live = wait_live_after(&mut receiver, 0).await;
        assert_eq!(resolved_names(&live), ["alpha"]);
        history_stop.send(()).unwrap();
        let states = history.await.unwrap();
        assert!(
            !states.contains(&TagClientConnectionState::Unbound),
            "{states:?}"
        );
        release_tx.send(()).unwrap();
        stop_partial_worker(stop_tx, task).await;
        server.await.unwrap();
    }

    /// Design §5.2 scene 13: the Hub restarted between the first catalog and
    /// `pong` and its revision happens to match. The different `run_id` is a
    /// `runtime_metadata_mismatch` rebind, not `Unbound`.
    #[tokio::test]
    async fn a_run_id_change_before_pong_rebinds_as_metadata_mismatch() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let (rebind_seen_tx, rebind_seen_rx) = oneshot::channel::<()>();
        let (read_tx, read_rx) = oneshot::channel::<()>();
        let (go_tx, go_rx) = oneshot::channel::<()>();
        let gate = SecondCatalogGate {
            read: read_tx,
            go: go_rx,
        };
        let server = tokio::spawn(async move {
            let log = HubLog::default();
            let socket = serve_unbound_handshake_gated(
                &listener,
                &empty_catalog(1, Some(7)),
                &empty_catalog(1, Some(8)),
                &log,
                gate,
            )
            .await;
            // Hold the rebind's catalog request until the test looked.
            let (mut stream, _) = listener.accept().await.unwrap();
            read_http_request(&mut stream).await;
            let _ = rebind_seen_rx.await;
            drop(stream);
            drop(socket);
        });
        let (mut receiver, stop_tx, task) =
            spawn_partial_worker(address, binding_requests(&[("a", "alpha")]));
        let (history_stop, history) = record_states(receiver.clone());
        assert_nothing_unbound_before_second_catalog(&receiver, read_rx, go_tx).await;
        wait_rebinding(&mut receiver, ErrorKind::RuntimeMetadataMismatch).await;
        history_stop.send(()).unwrap();
        let states = history.await.unwrap();
        assert!(
            !states.contains(&TagClientConnectionState::Unbound),
            "{states:?}"
        );
        stop_partial_worker(stop_tx, task).await;
        rebind_seen_tx.send(()).unwrap();
        server.await.unwrap();
    }

    /// Design §5.2 scene 12 (`stop` while waiting for `pong`): `shutdown`
    /// while the Hub does not read the `ping` (nor ever answers it) stops at
    /// once, without another attempt. Scene 11 (the `ping` send itself is
    /// stuck) cannot be reproduced because the 13 bytes fit into the TCP send
    /// buffer; it is covered by the shape of the code instead: `send_ping` is
    /// wrapped in a `biased` `select!` together with `stop`.
    #[tokio::test]
    async fn stop_while_the_hub_does_not_read_the_ping_stops_without_waiting() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let (ws_open_tx, ws_open_rx) = oneshot::channel::<()>();
        let (done_tx, done_rx) = oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let log = HubLog::default();
            serve_catalog_once(&listener, &empty_catalog(1, Some(7)), &log).await;
            let socket = accept_ws(&listener, &log).await;
            ws_open_tx.send(()).unwrap();
            // Never read from the socket.
            done_rx.await.unwrap();
            let extra = tokio::time::timeout(Duration::from_millis(50), listener.accept())
                .await
                .is_ok();
            drop(socket);
            extra
        });
        let (receiver, stop_tx, task) =
            spawn_partial_worker(address, binding_requests(&[("a", "alpha")]));
        ws_open_rx.await.unwrap();
        stop_tx.send(()).unwrap();
        // close_best_effort is bounded; the worker must not wait for pong.
        assert!(tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .is_ok());
        assert_eq!(
            receiver.borrow().connection_state(),
            TagClientConnectionState::Stopped
        );
        assert_eq!(receiver.borrow().last_error(), None);
        done_tx.send(()).unwrap();
        assert!(!server.await.unwrap(), "the worker started another attempt");
    }

    /// Design §5.1 / §14 (2026-11-01 review): a Hub that keeps the WebSocket
    /// but never answers `ping` must not pin the worker. The deadline expires
    /// as a retryable `Transport` failure and, after the backoff, a second
    /// connection is made.
    #[tokio::test]
    async fn a_missing_pong_times_out_as_transport_and_reconnects() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let (second_tx, second_rx) = oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let log = HubLog::default();
            serve_catalog_once(&listener, &empty_catalog(1, Some(7)), &log).await;
            let mut socket = accept_ws(&listener, &log).await;
            expect_ping(&mut socket, &log).await;
            // Read the ping but never answer; keep the socket open.
            serve_catalog_once(&listener, &empty_catalog(1, Some(7)), &log).await;
            second_tx.send(()).unwrap();
            let second = accept_ws(&listener, &log).await;
            (socket, second)
        });
        let backoff = BackoffConfig::new(Duration::from_millis(1), Duration::from_millis(5))
            .with_pong_timeout(Duration::from_millis(100));
        let (mut receiver, stop_tx, task) =
            spawn_worker_with_backoff(address, binding_requests(&[("a", "alpha")]), backoff);
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if receiver.borrow().connection_state() == TagClientConnectionState::Reconnecting {
                    break;
                }
                receiver.changed().await.unwrap();
            }
        })
        .await
        .expect("Reconnecting after the pong deadline");
        assert_eq!(receiver.borrow().last_error(), Some(ErrorKind::Transport));
        tokio::time::timeout(Duration::from_secs(3), second_rx)
            .await
            .expect("second connection attempt")
            .unwrap();
        let _sockets = tokio::time::timeout(Duration::from_secs(3), server)
            .await
            .expect("second WS accepted")
            .unwrap();
        stop_tx.send(()).unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .is_ok());
    }

    /// Design §14 / §5.2 scene 9: a close 1008 while waiting for `pong` keeps
    /// its reason (`receive_text()` errors are not converted).
    #[tokio::test]
    async fn a_1008_close_while_waiting_for_pong_stops_with_its_reason() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let log = HubLog::default();
            serve_catalog_once(&listener, &empty_catalog(1, Some(7)), &log).await;
            let mut socket = accept_ws(&listener, &log).await;
            expect_ping(&mut socket, &log).await;
            socket
                .send(close_message(1008, "api_key_revoked"))
                .await
                .unwrap();
            let _ = tokio::time::timeout(Duration::from_millis(200), socket.next()).await;
            tokio::time::timeout(Duration::from_millis(300), listener.accept())
                .await
                .is_ok()
        });
        let (mut receiver, _stop_tx, task) =
            spawn_partial_worker(address, binding_requests(&[("a", "alpha")]));
        wait_state(
            &mut receiver,
            TagClientConnectionState::Unauthorized,
            Some(ErrorKind::KeyRevoked),
        )
        .await;
        let result = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.unwrap_err().kind(), ErrorKind::KeyRevoked);
        assert!(!server.await.unwrap(), "reconnected after a 1008 close");
    }

    /// Design §5.1: a connection with at least one resolved binding never
    /// sends `ping` (its first frame is `subscribe`, and nothing follows).
    #[tokio::test]
    async fn a_live_connection_never_sends_ping() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, received) =
                serve_multi_generation(&listener, multi_catalog(&["alpha"], 1), &[("alpha", 20.0)])
                    .await;
            (received, texts_until_close(&mut socket).await)
        });
        let (mut receiver, stop_tx, task) =
            spawn_partial_worker(address, binding_requests(&[("a", "alpha"), ("b", "beta")]));
        wait_live_after(&mut receiver, 0).await;
        stop_partial_worker(stop_tx, task).await;
        let (received, after_subscribe) = server.await.unwrap();
        assert_eq!(received.subscribed, ["alpha"]);
        assert!(after_subscribe.is_empty(), "{after_subscribe:?}");
    }

    /// Design §7: `Unbound` → `config_changed` → `Unbound`, repeated more
    /// often than the rebind budget, never falls back to `Reconnecting`
    /// (`Unbound` starts a new budget; `live_rebind_success_starts_a_new_rebind_budget`
    /// for `Unbound`). The long backoff would park a fallen-back worker in
    /// `Reconnecting`, so the next `Unbound` would never come.
    #[tokio::test]
    async fn repeated_unbound_config_changed_never_falls_back_to_reconnecting() {
        const ROUNDS: u64 = 5;
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let (round_tx, mut round_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let server = tokio::spawn(async move {
            let log = HubLog::default();
            let mut sockets = Vec::new();
            for revision in 1..=ROUNDS + 1 {
                let catalog = empty_catalog(revision, Some(7));
                let mut socket = serve_unbound_handshake(&listener, &catalog, &catalog, &log).await;
                if revision <= ROUNDS {
                    round_rx.recv().await.unwrap();
                    send_config_changed(&mut socket, revision + 1).await;
                }
                sockets.push(socket);
            }
            let _ = round_rx.recv().await;
            drop(sockets);
        });
        let (mut receiver, stop_tx, task) = spawn_worker_with_backoff(
            address,
            binding_requests(&[("a", "alpha")]),
            BackoffConfig::new(Duration::from_secs(30), Duration::from_secs(30)),
        );
        let (history_stop, history) = record_states(receiver.clone());
        let mut generation = 0;
        for revision in 1..=ROUNDS + 1 {
            let state = wait_unbound_after(&mut receiver, generation).await;
            assert_eq!(state.snapshot().unwrap().revision(), revision);
            generation = state.snapshot().unwrap().generation;
            if revision <= ROUNDS {
                round_tx.send(()).unwrap();
            }
        }
        history_stop.send(()).unwrap();
        let states = history.await.unwrap();
        assert!(
            !states.contains(&TagClientConnectionState::Reconnecting),
            "{states:?}"
        );
        // The mock does not read the last socket, so the client's close
        // handshake runs into its bounded CLOSE_TIMEOUT (1 s).
        stop_tx.send(()).unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .is_ok());
        drop(round_tx);
        server.await.unwrap();
    }

    /// Design §7: a disconnect while `Unbound` goes `Reconnecting` and comes
    /// back to `Unbound`, and `Unbound` does not reset the backoff: the
    /// delays keep doubling (only lower bounds are asserted, since a sleep
    /// never ends early).
    #[tokio::test]
    async fn unbound_disconnect_reconnects_to_unbound_and_the_backoff_keeps_growing() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let (dropped_tx, mut dropped_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let log = HubLog::default();
            let catalog = empty_catalog(1, Some(7));
            let mut gaps = Vec::new();
            let mut socket = serve_unbound_handshake(&listener, &catalog, &catalog, &log).await;
            for _ in 0..3 {
                dropped_rx.recv().await.unwrap();
                drop(socket);
                let dropped_at = tokio::time::Instant::now();
                let (mut stream, _) = listener.accept().await.unwrap();
                gaps.push(dropped_at.elapsed());
                read_http_request(&mut stream).await;
                write_json(&mut stream, serde_json::to_string(&catalog).unwrap()).await;
                socket = accept_ws(&listener, &log).await;
                expect_ping(&mut socket, &log).await;
                send_pong(&mut socket, &log).await;
                serve_catalog_once(&listener, &catalog, &log).await;
            }
            release_rx.await.unwrap();
            drop(socket);
            gaps
        });
        let (mut receiver, stop_tx, task) = spawn_worker_with_backoff(
            address,
            binding_requests(&[("a", "alpha")]),
            BackoffConfig::new(Duration::from_millis(100), Duration::from_secs(5)),
        );
        let mut generation = wait_unbound_after(&mut receiver, 0)
            .await
            .snapshot()
            .unwrap()
            .generation;
        for _ in 0..3 {
            dropped_tx.send(()).unwrap();
            wait_state(
                &mut receiver,
                TagClientConnectionState::Reconnecting,
                Some(ErrorKind::Transport),
            )
            .await;
            assert_eq!(receiver.borrow().snapshot(), None);
            generation = wait_unbound_after(&mut receiver, generation)
                .await
                .snapshot()
                .unwrap()
                .generation;
        }
        release_tx.send(()).unwrap();
        stop_partial_worker(stop_tx, task).await;
        let gaps = server.await.unwrap();
        for (gap, at_least_ms) in gaps.iter().zip([100, 200, 400]) {
            assert!(
                *gap >= Duration::from_millis(at_least_ms),
                "backoff was reset while Unbound: {gaps:?}"
            );
        }
    }

    /// `stop_during_rebinding_catalog_waits_for_no_extra_attempt` for
    /// `Unbound`: `shutdown` while `Unbound` stops without another attempt.
    #[tokio::test]
    async fn stop_while_unbound_stops_without_an_extra_attempt() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let log = HubLog::default();
            let catalog = empty_catalog(1, Some(7));
            let mut socket = serve_unbound_handshake(&listener, &catalog, &catalog, &log).await;
            let texts = texts_until_close(&mut socket).await;
            let extra = tokio::time::timeout(Duration::from_millis(50), listener.accept())
                .await
                .is_ok();
            (texts, extra)
        });
        let (mut receiver, stop_tx, task) =
            spawn_partial_worker(address, binding_requests(&[("a", "alpha")]));
        wait_unbound_after(&mut receiver, 0).await;
        stop_partial_worker(stop_tx, task).await;
        assert_eq!(
            receiver.borrow().connection_state(),
            TagClientConnectionState::Stopped
        );
        assert_eq!(receiver.borrow().snapshot(), None);
        assert_eq!(receiver.borrow().last_error(), None);
        let (texts, extra) = server.await.unwrap();
        assert!(texts.is_empty(), "{texts:?}");
        assert!(!extra, "the worker started another attempt after shutdown");
    }

    /// `a_1008_close_stops_with_its_reason_and_does_not_reconnect` for
    /// `Unbound`.
    #[tokio::test]
    async fn a_1008_close_while_unbound_stops_with_its_reason_and_does_not_reconnect() {
        for (reason, kind) in [
            ("api_key_revoked", ErrorKind::KeyRevoked),
            ("api_key_expired", ErrorKind::KeyExpired),
            ("api_key_tripped", ErrorKind::KeyTripped),
            ("api_key_not_found", ErrorKind::KeyNotFound),
            ("future_reason", ErrorKind::CredentialRejected),
        ] {
            let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let address = format!("http://{}", listener.local_addr().unwrap());
            let (unbound_seen_tx, unbound_seen_rx) = oneshot::channel::<()>();
            let server = tokio::spawn(async move {
                let log = HubLog::default();
                let catalog = empty_catalog(1, Some(7));
                let mut socket = serve_unbound_handshake(&listener, &catalog, &catalog, &log).await;
                unbound_seen_rx.await.unwrap();
                socket.send(close_message(1008, reason)).await.unwrap();
                let _ = tokio::time::timeout(Duration::from_millis(200), socket.next()).await;
                tokio::time::timeout(Duration::from_millis(300), listener.accept())
                    .await
                    .is_ok()
            });
            let (mut receiver, _stop_tx, task) =
                spawn_partial_worker(address, binding_requests(&[("a", "alpha")]));
            wait_unbound_after(&mut receiver, 0).await;
            unbound_seen_tx.send(()).unwrap();
            wait_state(
                &mut receiver,
                TagClientConnectionState::Unauthorized,
                Some(kind),
            )
            .await;
            assert_eq!(receiver.borrow().snapshot(), None, "{reason}");
            let result = tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(result.unwrap_err().kind(), kind, "{reason}");
            assert!(
                !server.await.unwrap(),
                "{reason}: reconnected after a 1008 close"
            );
        }
    }
}
