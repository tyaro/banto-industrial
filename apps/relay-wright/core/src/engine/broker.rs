//! The PLC access broker's implementation (W3-A). See `engine/mod.rs`'s
//! module doc for the why; this doc covers the how - the exact concurrency
//! design, and the two policies a reviewer needs to know are deliberate
//! choices rather than gaps.
//!
//! ## Message shape and how serialization is guaranteed
//!
//! One [`tokio::task`] is spawned per SLMP [`banto_tags::PlcConnection`]
//! ([`BrokerSupervisor::spawn`]). Each task owns exactly one bare
//! `slmp::SLMPClient` and a `tokio::sync::mpsc::Receiver<Job>`. [`Job`] has
//! two variants - `Read`/`Write` - each carrying its owned request `Vec` and a
//! `tokio::sync::oneshot::Sender` for the reply. The task's main loop takes
//! jobs off the channel **one at a time** and `.await`s the whole read/write
//! before looking at the next one; there is exactly one mutable borrow of the
//! client alive at any instant, and it never crosses an await point held by
//! two jobs at once. So the one-socket-at-a-time property is structural (the
//! same argument `banto-collect/src/task.rs`'s module doc makes for its
//! single-task-owns-the-client design), not a lock - a read and a write to
//! the same CPU cannot interleave on the wire because nothing ever runs two
//! `execute_slmp_reads`/`execute_slmp_writes` calls concurrently against one
//! client.
//!
//! [`BrokerHandle`] is the clonable submission point (holds the mpsc
//! `Sender`); many callers (a future poller and a future writer) can hold
//! clones and submit concurrently - the mpsc channel is what serializes their
//! requests onto the one task, in arrival order, with no corruption possible
//! because the task itself is the only thing touching the client.
//!
//! ## Reconnect / backoff policy
//!
//! Structure copied from `banto-collect/src/task.rs`'s `run_connection`
//! (`ConnState`/`ConnEvent`/spawned-connect-attempt shape), *not* its
//! `TsWriter`-flavoured content: `ConnState` here is `Backoff { at } |
//! Connecting(JoinHandle<..>) | Connected(slmp::SLMPClient)`, and the initial
//! state is `Backoff { at: now }` so the first connect attempt fires
//! immediately. A failed connect attempt reschedules the next one after
//! [`backoff_delay`] (exponential, parameterized by [`BackoffConfig`],
//! capped, reset to attempt 0 on any success). A connection-fatal failure
//! *while processing a request* (see below) drops the client and re-enters
//! `Backoff` immediately (attempt reset to 0) - the same "no disconnect event
//! needed, we were already using it a moment ago" reasoning `run_connection`
//! uses for its own fatal-read branch.
//!
//! `connect_attempt` necessarily re-implements
//! `banto_plc::slmp::SlmpClient::connect`'s body (build a bare
//! `slmp::SLMPClient` from `SlmpConfig::to_wire_props()`, wire the two
//! per-crate timeouts, wrap the connect in `SlmpConfig::connect_timeout`, map
//! `std::io::Error` the same way). This is deliberate duplication, not an
//! oversight: `banto_plc::SlmpClient` is a `PlcClient` that owns its *own*
//! private `Option<slmp::SLMPClient>` with no seam to hand that socket to
//! `banto_plc_write::execute_slmp_writes` afterward, so the broker cannot
//! reuse it and still keep read and write on one shared session - the whole
//! point of this module. Reconnecting a bare client is the smallest amount of
//! code that lets both `execute_slmp_reads` and `execute_slmp_writes` borrow
//! the *same* `slmp::SLMPClient`.
//!
//! ## Queued-request-while-down policy: fail fast, never queue
//!
//! A [`Job`] is only ever handled while `ConnState::Connected` - the `_ =>
//! None` arm below covers both `Backoff` (including "never connected yet")
//! and `Connecting`, and answers immediately with
//! [`BrokerError::Disconnected`] rather than buffering the request until a
//! session comes back. A condition poller or auto-writer (W3-B) must be able
//! to tell "the session is down right now" apart from "this one request
//! failed", and a request that silently blocked until the next successful
//! reconnect (which may be 30s away under the backoff cap) would be
//! indistinguishable from a hang to its caller. Fail-fast keeps that decision
//! (retry now, retry later, or give up) with the caller, which is where
//! W3-B's rate limiting and edge-trigger logic actually lives.
//!
//! When a request that *was* running against a live session hits a
//! connection-fatal error mid-flight, that one request's oneshot is failed
//! with [`BrokerError::ConnectionFailed`] (carrying the underlying error's
//! message) **and** the task falls back into `Backoff` - both happen from the
//! same match arm, so the two can never drift out of sync.
//!
//! ## Why no explicit call to `is_connection_fatal` appears here
//!
//! Both `banto_plc::execute_slmp_reads` and `banto_plc_write::execute_slmp_writes`
//! already enforce the connection-fatal/per-request split internally (a
//! device-side SLMP end code becomes a `Bad`/per-target failure folded into
//! `Ok(Vec<_>)`; only a connection-fatal condition surfaces as `Err`). So by
//! the time either function returns `Err` here, the caller already knows it
//! is connection-fatal by construction - there is no second classification
//! step to perform, and (incidentally) `banto_plc::PlcError::is_connection_fatal`
//! is `pub(crate)` there and not reachable from this crate anyway.
//! `banto_plc_write::PlcWriteError::is_connection_fatal` *is* `pub`, but this
//! module still never needs to call it for the same reason.

use std::collections::HashMap;
use std::time::Duration;

use banto_plc::{
    execute_slmp_reads, plan_slmp_requests, PlcError, ReadRequest, ReadResult, SlmpConfig,
};
use banto_plc_write::{execute_slmp_writes, plan_slmp_writes, WriteRequest, WriteResult};
use banto_tags::PlcConnection;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

/// SLMP is the only protocol this broker speaks (this app writes MELSEC).
const SLMP_PROTOCOL: &str = "slmp";

/// Default channel depth for one connection's job queue. Generous relative to
/// the request sizes this app deals with (a handful of write targets / source
/// tags per cycle); the fail-fast-when-down policy is what actually bounds
/// caller latency, not this number.
const JOB_CHANNEL_CAPACITY: usize = 32;

/// Reconnect backoff bounds and growth factor. Mirrors
/// `banto_collect::BackoffConfig`'s `base`/`max` shape (renamed `cap` here to
/// read unambiguously next to `factor`) plus an explicit growth `factor`
/// (banto-collect hardcodes doubling; parameterizing it here costs nothing
/// and matches the W3-A brief). Delay before connect attempt `attempt`
/// (1-based) is `base * factor^(attempt-1)`, capped at `cap`; attempt `0` is
/// immediate.
#[derive(Debug, Clone, Copy)]
pub struct BackoffConfig {
    pub base: Duration,
    pub cap: Duration,
    pub factor: u32,
}

impl Default for BackoffConfig {
    fn default() -> Self {
        Self {
            base: Duration::from_secs(1),
            cap: Duration::from_secs(30),
            factor: 2,
        }
    }
}

fn backoff_delay(attempt: u32, cfg: BackoffConfig) -> Duration {
    if attempt == 0 {
        return Duration::ZERO;
    }
    let growth = (cfg.factor as u64)
        .checked_pow(attempt - 1)
        .unwrap_or(u64::MAX);
    let ms = (cfg.base.as_millis() as u64).saturating_mul(growth);
    Duration::from_millis(ms).min(cfg.cap)
}

/// Everything a [`BrokerHandle`]/[`ReadOnlyHandle`] request can fail with.
/// Deliberately small and non-exhaustive-friendly: this is infrastructure
/// (W3-A), not the auto-write engine's richer failure vocabulary.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum BrokerError {
    /// The session for this connection is not currently up (never connected
    /// yet, or mid-backoff/mid-reconnect) - see the module doc's
    /// queued-request-while-down policy. The caller decides whether/when to
    /// retry; this broker never queues the request for them.
    #[error("PLC接続 {connection_id} は現在未接続です（再接続待機中のため要求を実行できません）")]
    Disconnected { connection_id: i64 },

    /// This request was in flight on a live session when a connection-fatal
    /// failure occurred; `reason` is the underlying `PlcError`/`PlcWriteError`'s
    /// message. The broker has already dropped the session and started
    /// reconnecting by the time this reaches the caller.
    #[error("PLC接続 {connection_id} への要求が接続断で失敗しました: {reason}")]
    ConnectionFailed { connection_id: i64, reason: String },

    /// The broker task for this connection is no longer running (its mpsc
    /// receiver was dropped, e.g. after [`BrokerSupervisor::shutdown`]).
    #[error("PLC接続 {connection_id} のブローカータスクは終了しています")]
    TaskGone { connection_id: i64 },

    /// [`BrokerSupervisor::spawn`] was given a connection whose
    /// `protocol` is not `"slmp"`. This broker is SLMP-only (relay-wright
    /// writes MELSEC); a Modbus TCP (or any other protocol) connection is
    /// rejected outright rather than silently skipped, so a caller relying on
    /// `.handle(id)` finds out immediately at startup rather than discovering
    /// a missing handle later as an unexplained `None`.
    #[error("接続 {connection_id} は SLMP ではありません（protocol={protocol}）。ブローカーは SLMP 専用です")]
    UnsupportedProtocol {
        connection_id: i64,
        protocol: String,
    },

    /// `PlcConnection::port` (`i64`, validated 1..=65535 at the service layer)
    /// did not fit in the wire's `u16` - defensive only, should be
    /// unreachable for a row that passed `banto_tags`' own validation.
    #[error("接続 {connection_id} のポート番号が不正です: {port}")]
    InvalidPort { connection_id: i64, port: i64 },
}

/// One thing a broker task can be asked to do, paired with where to send the
/// result. Exactly two variants - this *is* the read/write safety boundary
/// the module doc and `engine/mod.rs` describe: a caller that only ever gets
/// a [`ReadOnlyHandle`] can never construct a `Write` job.
enum Job {
    Read {
        requests: Vec<ReadRequest>,
        respond_to: oneshot::Sender<Result<Vec<ReadResult>, BrokerError>>,
    },
    Write {
        requests: Vec<WriteRequest>,
        respond_to: oneshot::Sender<Result<Vec<WriteResult>, BrokerError>>,
    },
}

/// A clonable submission point for one PLC connection's broker task. Exposes
/// both [`BrokerHandle::read`] and [`BrokerHandle::write`]; see
/// [`BrokerHandle::read_only`] for the read-only subset. W3-B's design keeps
/// this held only by `writer.rs`, handing `poller.rs` a [`ReadOnlyHandle`]
/// instead - a compile-time guarantee that only one module can ever submit a
/// write, enforced entirely by which handle type a function accepts.
#[derive(Clone)]
pub struct BrokerHandle {
    connection_id: i64,
    tx: mpsc::Sender<Job>,
}

impl BrokerHandle {
    /// Submit a read batch and await its result. Fails fast with
    /// [`BrokerError::Disconnected`] if the session is down - see the module
    /// doc's queued-request-while-down policy.
    pub async fn read(&self, requests: Vec<ReadRequest>) -> Result<Vec<ReadResult>, BrokerError> {
        let (respond_to, rx) = oneshot::channel();
        self.tx
            .send(Job::Read {
                requests,
                respond_to,
            })
            .await
            .map_err(|_| BrokerError::TaskGone {
                connection_id: self.connection_id,
            })?;
        rx.await.map_err(|_| BrokerError::TaskGone {
            connection_id: self.connection_id,
        })?
    }

    /// Submit a write batch and await its result. Same fail-fast-when-down
    /// policy as [`Self::read`].
    pub async fn write(
        &self,
        requests: Vec<WriteRequest>,
    ) -> Result<Vec<WriteResult>, BrokerError> {
        let (respond_to, rx) = oneshot::channel();
        self.tx
            .send(Job::Write {
                requests,
                respond_to,
            })
            .await
            .map_err(|_| BrokerError::TaskGone {
                connection_id: self.connection_id,
            })?;
        rx.await.map_err(|_| BrokerError::TaskGone {
            connection_id: self.connection_id,
        })?
    }

    /// A read-only view of this handle - clonable, structurally incapable of
    /// submitting a write (there is no `write` method on [`ReadOnlyHandle`]
    /// at all). W3-B hands this to `poller.rs`.
    pub fn read_only(&self) -> ReadOnlyHandle {
        ReadOnlyHandle {
            inner: self.clone(),
        }
    }
}

/// The read-only subset of [`BrokerHandle`] - see [`BrokerHandle::read_only`].
#[derive(Clone)]
pub struct ReadOnlyHandle {
    inner: BrokerHandle,
}

impl ReadOnlyHandle {
    /// Identical to [`BrokerHandle::read`].
    pub async fn read(&self, requests: Vec<ReadRequest>) -> Result<Vec<ReadResult>, BrokerError> {
        self.inner.read(requests).await
    }
}

/// Test-only fake broker: a [`BrokerHandle`] backed by a task that answers
/// every job successfully with no network and no clock - reads with an empty
/// result set, writes with all-[`WriteResult::Ok`]. Lets sibling modules'
/// unit tests (e.g. `writer.rs`'s rate-limit gate test) drive code that
/// requires a write-capable handle fully deterministically.
#[cfg(test)]
pub(crate) fn spawn_test_handle_answering_ok(connection_id: i64) -> (BrokerHandle, JoinHandle<()>) {
    let (tx, mut rx) = mpsc::channel::<Job>(JOB_CHANNEL_CAPACITY);
    let task = tokio::spawn(async move {
        while let Some(job) = rx.recv().await {
            match job {
                Job::Read { respond_to, .. } => {
                    let _ = respond_to.send(Ok(Vec::new()));
                }
                Job::Write {
                    requests,
                    respond_to,
                } => {
                    let _ = respond_to.send(Ok(requests.iter().map(|_| WriteResult::Ok).collect()));
                }
            }
        }
    });
    (BrokerHandle { connection_id, tx }, task)
}

/// Spawns and owns one broker task per SLMP [`PlcConnection`], and hands out
/// [`BrokerHandle`]s keyed by connection id.
pub struct BrokerSupervisor {
    handles: HashMap<i64, BrokerHandle>,
    tasks: Vec<JoinHandle<()>>,
    /// Out-of-band shutdown trigger shared with every task via a cloned
    /// [`watch::Receiver`] - see [`Self::shutdown`]. Independent of the job
    /// mpsc: a task must stop even while a [`BrokerHandle`] (and therefore a
    /// live `Sender`) is still held by some caller (a poller/writer that
    /// outlives the supervisor by design in W3-B), which the mpsc-closes path
    /// alone can never signal.
    shutdown_tx: watch::Sender<bool>,
}

impl BrokerSupervisor {
    /// Spawn one broker task per connection in `connections`. Every entry
    /// must be `protocol == "slmp"` - the first one that is not aborts the
    /// whole call with [`BrokerError::UnsupportedProtocol`] (see that
    /// variant's doc for why a reject-the-batch policy beats silently
    /// skipping). Tasks already spawned for earlier connections in the slice
    /// are simply dropped in that case: nothing has been handed out yet
    /// (`Self` is only returned on full success), so their mpsc receivers
    /// drop with them and they exit immediately - no explicit cleanup needed.
    pub fn spawn(
        connections: &[PlcConnection],
        backoff: BackoffConfig,
    ) -> Result<Self, BrokerError> {
        let mut handles = HashMap::with_capacity(connections.len());
        let mut tasks = Vec::with_capacity(connections.len());
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        for conn in connections {
            if conn.protocol != SLMP_PROTOCOL {
                return Err(BrokerError::UnsupportedProtocol {
                    connection_id: conn.id,
                    protocol: conn.protocol.clone(),
                });
            }
            let port = u16::try_from(conn.port).map_err(|_| BrokerError::InvalidPort {
                connection_id: conn.id,
                port: conn.port,
            })?;
            let config = SlmpConfig {
                host: conn.host.clone(),
                port,
                ..SlmpConfig::default()
            };

            let (handle, task) = spawn_task(conn.id, config, backoff, shutdown_rx.clone());
            tasks.push(task);
            handles.insert(conn.id, handle);
        }

        Ok(Self {
            handles,
            tasks,
            shutdown_tx,
        })
    }

    /// The handle for one connection, if it was among those `spawn` started a
    /// task for.
    pub fn handle(&self, connection_id: i64) -> Option<BrokerHandle> {
        self.handles.get(&connection_id).cloned()
    }

    /// How many broker tasks are currently spawned. Mainly a test/diagnostic
    /// helper.
    pub fn connection_count(&self) -> usize {
        self.tasks.len()
    }

    /// Clean shutdown: flip the shared shutdown trigger so every task breaks
    /// out of its job loop on its own, then await each task's graceful exit.
    ///
    /// This does *not* rely on every [`BrokerHandle`] having been dropped.
    /// The mpsc `Sender` a `BrokerHandle` holds only closes (`rx.recv() ==
    /// None`) once *all* clones are gone, but a realistic caller (W3-B's
    /// poller/writer) holds a handle for the whole app lifetime - dropping
    /// only `self.handles` would leave that `Sender` alive and the task
    /// blocked in `rx.recv()` forever. The `watch` signal is out-of-band from
    /// that mpsc entirely, so it stops the task regardless of how many
    /// `BrokerHandle`s are still outstanding elsewhere. `self.handles` is
    /// still dropped afterward so any `BrokerHandle` clones this supervisor
    /// itself owned release their `Sender`s too, though the task no longer
    /// depends on that to exit.
    pub async fn shutdown(self) {
        let _ = self.shutdown_tx.send(true);
        drop(self.handles);
        for task in self.tasks {
            let _ = task.await;
        }
    }
}

/// Build one connection's `(handle, task)` pair - the part of
/// [`BrokerSupervisor::spawn`]'s body that does not need to see the whole
/// slice, factored out so the test module can spawn a task against a
/// hand-built [`SlmpConfig`] (e.g. a short `response_timeout` for a fast
/// reconnect test) without going through `PlcConnection`/`BrokerSupervisor`
/// at all. `shutdown_rx` is the task's clone of the supervisor-wide shutdown
/// trigger (see [`BrokerSupervisor::shutdown`]); a caller that does not need
/// that path (e.g. a test that only ever drops its lone `BrokerHandle`) can
/// pass a receiver from a `watch::channel` it never signals.
fn spawn_task(
    connection_id: i64,
    config: SlmpConfig,
    backoff: BackoffConfig,
    shutdown_rx: watch::Receiver<bool>,
) -> (BrokerHandle, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel(JOB_CHANNEL_CAPACITY);
    let task = tokio::spawn(run_broker_task(
        connection_id,
        config,
        backoff,
        rx,
        shutdown_rx,
    ));
    (BrokerHandle { connection_id, tx }, task)
}

/// Connection lifecycle state within one broker task. Structurally identical
/// to `banto_collect::task::ConnState` (see the module doc).
enum ConnState {
    /// Waiting until `at` to spawn the next connect attempt.
    Backoff { at: Instant },
    /// A connect attempt is running in a spawned sub-task, so a slow
    /// `connect()` cannot stall the job loop.
    Connecting(JoinHandle<(Box<slmp::SLMPClient>, Result<(), PlcError>)>),
    /// Connected and owning the live client - the only state in which a
    /// [`Job`] is actually serviced. Boxed (clippy::large_enum_variant):
    /// `slmp::SLMPClient` carries a fixed internal receive buffer that makes
    /// it far larger than `ConnState`'s other variants.
    Connected(Box<slmp::SLMPClient>),
}

/// What woke the connection side of the task's `select!`.
enum ConnEvent {
    Due,
    /// Boxed for the same reason as `ConnState::Connected`.
    Finished(Box<slmp::SLMPClient>, Result<(), PlcError>),
    JoinError,
}

async fn next_conn_event(state: &mut ConnState) -> ConnEvent {
    match state {
        ConnState::Connected(_) => std::future::pending().await,
        ConnState::Backoff { at } => {
            tokio::time::sleep_until(*at).await;
            ConnEvent::Due
        }
        ConnState::Connecting(handle) => match handle.await {
            Ok((client, result)) => ConnEvent::Finished(client, result),
            Err(_join_err) => ConnEvent::JoinError,
        },
    }
}

/// Build a bare `slmp::SLMPClient` from `config` and attempt to connect it,
/// applying the same timeouts/error-mapping `banto_plc::slmp::SlmpClient::connect`
/// does (see the module doc for why this is re-implemented here rather than
/// reused). The client is returned either way - on failure it never held a
/// stream (the wrapped crate's `connect()` clears its own socket before
/// dialing), so there is nothing to close, but returning it uniformly keeps
/// this function's shape simple.
async fn connect_attempt(config: SlmpConfig) -> (Box<slmp::SLMPClient>, Result<(), PlcError>) {
    let addr = format!("{}:{}", config.host, config.port);
    let mut client = Box::new(slmp::SLMPClient::new(config.to_wire_props()));
    client.set_send_timeout(config.response_timeout);
    client.set_recv_timeout(config.response_timeout);

    let result = match tokio::time::timeout(config.connect_timeout, client.connect()).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(match e.kind() {
            std::io::ErrorKind::TimedOut => PlcError::ConnectTimeout(addr.clone()),
            _ => PlcError::Connection(e.to_string()),
        }),
        Err(_elapsed) => Err(PlcError::ConnectTimeout(addr.clone())),
    };

    (client, result)
}

/// One connection's broker loop: owns `config`'s session end-to-end (connect,
/// reconnect-with-backoff, disconnect on shutdown) and services [`Job`]s
/// serialized through `rx`. Exits on either of two independent signals:
/// every [`BrokerHandle`] for this connection has been dropped (`rx.recv()`
/// returns `None`), or the supervisor-wide shutdown trigger fires
/// (`shutdown_rx` changes, or its `Sender` is dropped) - see
/// [`BrokerSupervisor::shutdown`] for why the latter is needed: a `Job`
/// sender clone can legitimately outlive the supervisor (a poller/writer
/// holding a [`BrokerHandle`] for the app's lifetime), so the mpsc alone
/// cannot be relied on to ever close.
async fn run_broker_task(
    connection_id: i64,
    config: SlmpConfig,
    backoff_cfg: BackoffConfig,
    mut rx: mpsc::Receiver<Job>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let word_order = config.word_order;
    let mut attempt: u32 = 0;
    let mut state = ConnState::Backoff { at: Instant::now() };

    loop {
        tokio::select! {
            // Out-of-band shutdown trigger - see BrokerSupervisor::shutdown.
            // `changed()` resolves either when the value flips to `true`
            // (explicit shutdown) or when the Sender is dropped without ever
            // sending (Err case): both mean "stop", so either way we break
            // regardless of how many BrokerHandles/Senders for the job mpsc
            // are still alive elsewhere.
            _ = shutdown_rx.changed() => {
                break;
            }

            conn_event = next_conn_event(&mut state) => {
                match conn_event {
                    ConnEvent::Due => {
                        attempt += 1;
                        let handle = tokio::spawn(connect_attempt(config.clone()));
                        state = ConnState::Connecting(handle);
                    }
                    ConnEvent::Finished(client, Ok(())) => {
                        attempt = 0;
                        state = ConnState::Connected(client);
                    }
                    ConnEvent::Finished(_client, Err(_err)) => {
                        let delay = backoff_delay(attempt, backoff_cfg);
                        state = ConnState::Backoff {
                            at: Instant::now() + delay,
                        };
                    }
                    ConnEvent::JoinError => {
                        let delay = backoff_delay(attempt, backoff_cfg);
                        state = ConnState::Backoff {
                            at: Instant::now() + delay,
                        };
                    }
                }
            }

            maybe_job = rx.recv() => {
                let Some(job) = maybe_job else {
                    // Every BrokerHandle for this connection was dropped:
                    // clean shutdown.
                    break;
                };

                match job {
                    Job::Read { requests, respond_to } => {
                        // Two-step match (compute the outcome, then decide
                        // `state`) mirrors banto-collect's task.rs: it keeps
                        // the borrow of `state` from ending mid-await from
                        // ever overlapping with the reassignment below.
                        let outcome: Option<Result<Vec<ReadResult>, PlcError>> = match &mut state {
                            ConnState::Connected(client) => {
                                let plan = plan_slmp_requests(&requests);
                                Some(execute_slmp_reads(client, &plan, requests.len(), word_order).await)
                            }
                            _ => None,
                        };
                        match outcome {
                            Some(Ok(results)) => {
                                let _ = respond_to.send(Ok(results));
                            }
                            Some(Err(err)) => {
                                let _ = respond_to.send(Err(BrokerError::ConnectionFailed {
                                    connection_id,
                                    reason: err.to_string(),
                                }));
                                state = ConnState::Backoff { at: Instant::now() };
                                attempt = 0;
                            }
                            None => {
                                let _ = respond_to.send(Err(BrokerError::Disconnected { connection_id }));
                            }
                        }
                    }
                    Job::Write { requests, respond_to } => {
                        let outcome: Option<Result<Vec<WriteResult>, banto_plc_write::PlcWriteError>> =
                            match &mut state {
                                ConnState::Connected(client) => {
                                    let plan = plan_slmp_writes(&requests, word_order);
                                    Some(execute_slmp_writes(client, &plan, requests.len()).await)
                                }
                                _ => None,
                            };
                        match outcome {
                            Some(Ok(results)) => {
                                let _ = respond_to.send(Ok(results));
                            }
                            Some(Err(err)) => {
                                let _ = respond_to.send(Err(BrokerError::ConnectionFailed {
                                    connection_id,
                                    reason: err.to_string(),
                                }));
                                state = ConnState::Backoff { at: Instant::now() };
                                attempt = 0;
                            }
                            None => {
                                let _ = respond_to.send(Err(BrokerError::Disconnected { connection_id }));
                            }
                        }
                    }
                }
            }
        }
    }

    if let ConnState::Connected(client) = state {
        client.close().await;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use banto_plc::{Address, DataType, TagValue};
    use banto_plc_write::slmp::simulator::Simulator;
    use futures_util::future::join_all;

    use super::*;

    fn conn(id: i64, protocol: &str, host: &str, port: u16) -> PlcConnection {
        PlcConnection {
            id,
            name: format!("conn-{id}"),
            protocol: protocol.to_string(),
            host: host.to_string(),
            port: port as i64,
            unit_id: 1,
            enabled: true,
        }
    }

    /// Short timeouts pointed at `sim`, for tests that do not need the
    /// default 1s/3s timeouts and want fast failure detection (the
    /// backoff/reconnect test).
    fn fast_config(sim: &Simulator) -> SlmpConfig {
        SlmpConfig {
            host: sim.addr.ip().to_string(),
            port: sim.addr.port(),
            connect_timeout: Duration::from_millis(300),
            response_timeout: Duration::from_millis(150),
            ..Default::default()
        }
    }

    fn rreq(raw: &str, data_type: DataType) -> ReadRequest {
        ReadRequest {
            address: Address::parse_slmp(raw).unwrap_or_else(|e| panic!("{raw}: {e}")),
            data_type,
        }
    }

    fn wreq(raw: &str, data_type: DataType, value: TagValue) -> WriteRequest {
        WriteRequest {
            address: Address::parse_slmp(raw).unwrap_or_else(|e| panic!("{raw}: {e}")),
            data_type,
            value,
        }
    }

    /// Poll `handle.read` until the session comes up (real time - connecting
    /// to a loopback simulator settles in low single-digit milliseconds) or a
    /// generous deadline elapses, so tests don't race the connect task that
    /// starts in the background the instant a broker is spawned.
    async fn read_once_connected(
        handle: &BrokerHandle,
        requests: Vec<ReadRequest>,
    ) -> Vec<ReadResult> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            match handle.read(requests.clone()).await {
                Ok(results) => return results,
                Err(BrokerError::Disconnected { .. }) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                Err(e) => panic!("unexpected broker error while waiting to connect: {e}"),
            }
        }
    }

    // -----------------------------------------------------------------
    // Backoff ladder (pure, virtual time) - mirrors banto-collect's
    // backoff_ladder_advances_virtual_time_deterministically exactly.
    // -----------------------------------------------------------------

    #[test]
    fn backoff_attempt_zero_is_immediate() {
        assert_eq!(backoff_delay(0, BackoffConfig::default()), Duration::ZERO);
    }

    #[test]
    fn backoff_doubles_each_attempt_and_caps() {
        let cfg = BackoffConfig::default();
        assert_eq!(backoff_delay(1, cfg), Duration::from_secs(1));
        assert_eq!(backoff_delay(2, cfg), Duration::from_secs(2));
        assert_eq!(backoff_delay(3, cfg), Duration::from_secs(4));
        assert_eq!(backoff_delay(6, cfg), Duration::from_secs(30)); // 32 -> capped
        assert_eq!(backoff_delay(100, cfg), Duration::from_secs(30)); // no overflow
    }

    #[tokio::test(start_paused = true)]
    async fn backoff_ladder_advances_virtual_time_deterministically() {
        let cfg = BackoffConfig::default();
        let start = Instant::now();
        for attempt in 1..=7 {
            let at = Instant::now() + backoff_delay(attempt, cfg);
            tokio::time::sleep_until(at).await;
        }
        // 1 + 2 + 4 + 8 + 16 + 30 + 30 = 91s of virtual time, instantly.
        assert_eq!(start.elapsed(), Duration::from_secs(91));
    }

    // -----------------------------------------------------------------
    // Supervisor
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn supervisor_rejects_a_modbus_tcp_connection() {
        let connections = [conn(1, "modbus-tcp", "127.0.0.1", 502)];
        let err = match BrokerSupervisor::spawn(&connections, BackoffConfig::default()) {
            Ok(_) => panic!("modbus-tcp must be rejected"),
            Err(e) => e,
        };
        assert_eq!(
            err,
            BrokerError::UnsupportedProtocol {
                connection_id: 1,
                protocol: "modbus-tcp".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn supervisor_hands_out_a_handle_per_slmp_connection() {
        let sim = Simulator::start().await;
        let connections = [conn(7, "slmp", "127.0.0.1", sim.addr.port())];
        let supervisor = BrokerSupervisor::spawn(&connections, BackoffConfig::default())
            .expect("all-slmp connections should spawn");
        assert_eq!(supervisor.connection_count(), 1);
        assert!(supervisor.handle(7).is_some());
        assert!(supervisor.handle(999).is_none());
        supervisor.shutdown().await;
    }

    // -----------------------------------------------------------------
    // Read, write, and read-after-write over one shared session
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn read_returns_simulated_device_values() {
        let sim = Simulator::start().await;
        sim.set_word(banto_plc::SlmpDevice::D, 100, 4321);
        let connections = [conn(1, "slmp", "127.0.0.1", sim.addr.port())];
        let supervisor =
            BrokerSupervisor::spawn(&connections, BackoffConfig::default()).expect("spawn");
        let handle = supervisor.handle(1).expect("handle");

        let results = read_once_connected(&handle, vec![rreq("D100", DataType::U16)]).await;
        assert_eq!(results, vec![ReadResult::Value(TagValue::F64(4321.0))]);

        supervisor.shutdown().await;
    }

    #[tokio::test]
    async fn write_then_read_reflects_the_write_over_the_single_session() {
        let sim = Simulator::start().await;
        let connections = [conn(1, "slmp", "127.0.0.1", sim.addr.port())];
        let supervisor =
            BrokerSupervisor::spawn(&connections, BackoffConfig::default()).expect("spawn");
        let handle = supervisor.handle(1).expect("handle");

        // Wait for the session to come up using an innocuous read, then
        // write and read back - the read-after-write proof that both
        // directions share the one session (a separate session would still
        // pass this if it happened to hit the same simulator process, but a
        // wrong shared-client bug - e.g. two independent slmp::SLMPClients -
        // would still read the simulator's real state correctly too; what
        // this test actually pins down together with `only_one_broker_task_..`
        // below is that a single broker task and a single wire round trip
        // serve both).
        let _ = read_once_connected(&handle, vec![rreq("D200", DataType::U16)]).await;

        let write_results = handle
            .write(vec![wreq("D200", DataType::U16, TagValue::F64(777.0))])
            .await
            .expect("write should succeed");
        assert_eq!(write_results, vec![WriteResult::Ok]);

        let read_results = handle
            .read(vec![rreq("D200", DataType::U16)])
            .await
            .expect("read should succeed");
        assert_eq!(read_results, vec![ReadResult::Value(TagValue::F64(777.0))]);

        supervisor.shutdown().await;
    }

    #[tokio::test]
    async fn serialized_concurrent_requests_do_not_corrupt_each_other() {
        let sim = Simulator::start().await;
        let connections = [conn(1, "slmp", "127.0.0.1", sim.addr.port())];
        let supervisor =
            BrokerSupervisor::spawn(&connections, BackoffConfig::default()).expect("spawn");
        let handle = supervisor.handle(1).expect("handle");

        // Wait for the session before firing the concurrent burst, so every
        // task below exercises the same connected session rather than racing
        // the initial connect.
        let _ = read_once_connected(&handle, vec![rreq("D0", DataType::U16)]).await;

        const N: u32 = 20;
        let writes = (0..N).map(|i| {
            let handle = handle.clone();
            let device_number = 300 + i;
            let value = 1000.0 + i as f64;
            async move {
                let addr = format!("D{device_number}");
                handle
                    .write(vec![wreq(&addr, DataType::U16, TagValue::F64(value))])
                    .await
                    .unwrap_or_else(|e| panic!("write {addr} failed: {e}"))
            }
        });
        let write_outcomes = join_all(writes).await;
        assert!(
            write_outcomes.iter().all(|r| r == &vec![WriteResult::Ok]),
            "every concurrent write should succeed: {write_outcomes:?}"
        );

        let reads = (0..N).map(|i| {
            let handle = handle.clone();
            let device_number = 300 + i;
            async move {
                let addr = format!("D{device_number}");
                let result = handle
                    .read(vec![rreq(&addr, DataType::U16)])
                    .await
                    .unwrap_or_else(|e| panic!("read {addr} failed: {e}"));
                (i, result)
            }
        });
        let read_outcomes = join_all(reads).await;
        for (i, result) in read_outcomes {
            let expected = 1000.0 + i as f64;
            assert_eq!(
                result,
                vec![ReadResult::Value(TagValue::F64(expected))],
                "device D{} should hold this task's own value, not another's",
                300 + i
            );
        }

        supervisor.shutdown().await;
    }

    // -----------------------------------------------------------------
    // Disconnected policy and reconnect/backoff
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn a_request_while_disconnected_fails_fast_not_a_hang() {
        // A loopback port nothing listens on: bind then immediately drop, so
        // the port is real but guaranteed closed - `connect()` gets a prompt
        // refusal rather than this test depending on an unallocated port
        // staying free.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().expect("local_addr").port();
        drop(listener);

        let (_shutdown_tx, shutdown_rx) = watch::channel(false);
        let (handle, task) = spawn_task(
            1,
            SlmpConfig {
                host: "127.0.0.1".to_string(),
                port,
                connect_timeout: Duration::from_secs(3),
                ..Default::default()
            },
            BackoffConfig::default(),
            shutdown_rx,
        );

        let outcome = tokio::time::timeout(
            Duration::from_millis(200),
            handle.read(vec![rreq("D0", DataType::U16)]),
        )
        .await
        .expect("must fail fast, not hang for the full connect_timeout");
        assert_eq!(outcome, Err(BrokerError::Disconnected { connection_id: 1 }));

        drop(handle);
        let _ = task.await;
    }

    #[tokio::test]
    async fn fatal_error_triggers_reconnect_and_later_requests_succeed() {
        let sim = Simulator::start().await;
        let config = fast_config(&sim);
        let backoff = BackoffConfig {
            base: Duration::from_millis(20),
            cap: Duration::from_millis(100),
            factor: 2,
        };
        let (_shutdown_tx, shutdown_rx) = watch::channel(false);
        let (handle, task) = spawn_task(1, config, backoff, shutdown_rx);

        // Establish the session, then make the simulator unresponsive: the
        // next request times out (connection-fatal per classify_io_error),
        // which must fail that request AND drop into reconnect-backoff.
        let _ = read_once_connected(&handle, vec![rreq("D0", DataType::U16)]).await;
        sim.hang();

        let hung = handle.read(vec![rreq("D0", DataType::U16)]).await;
        assert!(
            matches!(
                hung,
                Err(BrokerError::ConnectionFailed {
                    connection_id: 1,
                    ..
                })
            ),
            "a response timeout mid-request should fail that request as connection-fatal: {hung:?}"
        );

        // Recover: the simulator answers again, and the backoff loop must
        // reconnect on its own with no outside intervention.
        sim.stop_hanging();
        sim.set_word(banto_plc::SlmpDevice::D, 400, 55);
        let recovered = read_once_connected(&handle, vec![rreq("D400", DataType::U16)]).await;
        assert_eq!(recovered, vec![ReadResult::Value(TagValue::F64(55.0))]);

        drop(handle);
        let _ = task.await;
    }
}
