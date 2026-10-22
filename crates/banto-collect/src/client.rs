//! The client a connection task reads through (R2b-3a-3,
//! docs/banto-hub-driver-io-design.md §4.3 and §13 Q6): [`CollectClient`],
//! the [`ClientFactory`] that hands one out per connect attempt, and the one
//! production implementation, [`BrokerReadClient`] - a read-only handle onto
//! the shared `banto-broker` session of the connection.
//!
//! ## One implementation, no copies
//!
//! Until R2b-3a-3 the seam was `banto_plc::PlcClient` fed by a hand-picked
//! projection of the plan (`ClientSpec`): every field that changed a client's
//! behaviour had to be copied by hand into it and back out again in
//! `default_client_factory`, and twice a field was dropped on the way
//! (`word_order` in #334, the SLMP CPU in #605). Now the plan carries the
//! driver's own `TagSpec`s and the connection's `Settings`, the broker dials
//! through the driver registry, and a factory only maps a connection key to a
//! handle. There is no per-protocol field left to copy.
//!
//! ## Why `connect()` reports the broker session's real state (#344)
//!
//! [`BrokerReadClient::connect`] never dials anything itself - the broker task
//! behind the wrapped handle owns the actual session end to end
//! (connect/reconnect/backoff) and is already running by the time any
//! `BrokerReadClient` exists. What it does instead is *observe* that task: it
//! reads [`ReadOnlyHandle::status_watch`] and answers `Ok` only while the
//! session is actually [`BrokerConnectionStatus::Connected`], waiting up to
//! [`BROKER_RECONNECT_GRACE`] for a session that is currently `Reconnecting`
//! before giving up.
//!
//! **This replaced an unconditional immediate `Ok`, and #344 is why.** While a
//! PLC was unreachable every single collection tick ran this cycle: the
//! tick's `read_batch` failed, so `task.rs` emitted `plc_disconnected` and
//! dropped into `Backoff{now}`; the immediately-due connect returned `Ok`, so
//! `task.rs` emitted `plc_reconnected` and went straight back to `Connected`;
//! the next tick failed again. One bogus pair per connection per collection
//! period (≈13,000 rows/hour at 1 s over two connections, measured on real
//! hardware in banto-hub). Reporting the broker's real state keeps the task's
//! `ConnState` in `Backoff` (1 s → 30 s) for as long as the session is down.
//! `task.rs` additionally anchors `plc_reconnected` to the first successful
//! read (its module doc, "What `plc_reconnected` means").
//!
//! Moved here from banto-hub's `broker_glue.rs` in R2b-3a-3 so ChronoGazer,
//! which reads through the broker too since then (design §8), gets the same
//! semantics from the same code.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use banto_broker::{BrokerConnectionStatus, ReadOnlyHandle};
use banto_driver_api::{BoxFuture, FatalError, ItemError, TagSpec, Value};

/// What one [`CollectClient::read_batch`] answers: one result per spec, in
/// order, or a connection-fatal error for the whole call.
pub type ReadBatch = Result<Vec<Result<Value, ItemError>>, FatalError>;

/// The client a connection task reads through. One instance per connect
/// attempt ([`ClientFactory`]); the task owns it while connected and drops it
/// on a failure.
///
/// - `connect` - `Ok` once reading can be attempted.
/// - `read_batch` - one result per spec, aligned 1:1 with `specs`; a per-item
///   [`ItemError`] makes only that tag Bad. `Err` is connection-fatal: the
///   task records the tick as all-Bad, reports `plc_disconnected` and
///   reconnects.
/// - `disconnect` - on the task's graceful exit.
pub trait CollectClient: Send {
    fn connect(&mut self) -> BoxFuture<'_, Result<(), FatalError>>;
    fn read_batch<'a>(&'a mut self, specs: &'a [TagSpec]) -> BoxFuture<'a, ReadBatch>;
    fn disconnect(&mut self) -> BoxFuture<'_, ()>;
}

/// Builds the [`CollectClient`] a connection task reconnects with, from the
/// connection's key (`"conn:{id}"`, the same key the status map and the
/// events use). Called once per connect attempt; a factory may hand out a
/// fresh client every time or close over shared state (the broker handles).
pub type ClientFactory = Arc<dyn Fn(&str) -> Box<dyn CollectClient> + Send + Sync>;

/// How long [`BrokerReadClient::connect`] waits for a broker session that is
/// currently [`BrokerConnectionStatus::Reconnecting`] to come back before it
/// reports the connect attempt as failed (#344, 2026-09-15).
///
/// Sized against `banto_broker`'s own backoff ladder
/// (`banto_broker::BackoffConfig::default()`: 1 s, doubling, capped at 30 s):
/// 3 s covers the first two rungs, so when the broker recovers on its own
/// within one of those retries, the task's own `ConnState` does not stack up
/// `Backoff` on top of it. This does **not** suppress the
/// `plc_disconnected`/`plc_reconnected` pair - `run_connection` emits
/// `plc_disconnected` the moment a read observes the session down,
/// independent of this window, and `plc_reconnected` once on the first
/// successful read afterwards; the pair is the correct record of the outage.
/// Deliberately **not** tied to a connect timeout: that one bounds a real
/// dial, which this client never performs.
pub const BROKER_RECONNECT_GRACE: Duration = Duration::from_secs(3);

/// A [`CollectClient`] that reads through a shared broker session instead of
/// owning a socket (this module's doc). Cheap to construct (wraps a clonable
/// [`ReadOnlyHandle`]); a fresh one is handed out per connect attempt, and
/// every instance for a connection shares the same broker task.
pub struct BrokerReadClient {
    handle: ReadOnlyHandle,
}

impl BrokerReadClient {
    pub fn new(handle: ReadOnlyHandle) -> Self {
        Self { handle }
    }
}

impl CollectClient for BrokerReadClient {
    /// Report whether the shared broker session is actually up, without
    /// dialling anything (this module's doc, #344):
    ///
    /// - every `watch` sender gone (the broker task has exited) -> `Err`
    ///   immediately, checked **before** anything else, so a task that
    ///   published `Connected` and then exited without `Stopped` is not
    ///   mistaken for a healthy session;
    /// - [`BrokerConnectionStatus::Connected`] -> `Ok(())` immediately;
    /// - [`BrokerConnectionStatus::Reconnecting`] -> wait up to
    ///   [`BROKER_RECONNECT_GRACE`] for it to become `Connected`, else `Err`
    ///   naming the broker's own attempt counter;
    /// - [`BrokerConnectionStatus::Stopped`] -> `Err` immediately.
    fn connect(&mut self) -> BoxFuture<'_, Result<(), FatalError>> {
        let mut status = self.handle.status_watch();
        Box::pin(async move {
            if status.has_changed().is_err() {
                return Err(FatalError::Io(
                    "broker セッションのタスクが終了しています".to_string(),
                ));
            }

            match *status.borrow_and_update() {
                BrokerConnectionStatus::Connected => return Ok(()),
                BrokerConnectionStatus::Stopped => {
                    return Err(FatalError::Io(
                        "broker セッションは停止しています".to_string(),
                    ))
                }
                BrokerConnectionStatus::Reconnecting { .. } => {}
            }

            // `wait_for` borrows `status`; copy the observed value straight
            // out so nothing is still borrowed when the timeout arm below
            // re-reads the (possibly newer) attempt counter for its message.
            let waited = tokio::time::timeout(
                BROKER_RECONNECT_GRACE,
                status.wait_for(|s| !matches!(s, BrokerConnectionStatus::Reconnecting { .. })),
            )
            .await
            .map(|result| result.map(|seen| *seen));

            match waited {
                Ok(Ok(BrokerConnectionStatus::Connected)) => Ok(()),
                // Only reachable as `Stopped` - the predicate excludes
                // `Reconnecting`, and `Connected` is the arm above.
                Ok(Ok(_)) => Err(FatalError::Io(
                    "broker セッションは停止しています".to_string(),
                )),
                // Every sender gone: the broker task itself has exited.
                Ok(Err(_)) => Err(FatalError::Io(
                    "broker セッションのタスクが終了しています".to_string(),
                )),
                Err(_elapsed) => {
                    let attempt = match *status.borrow() {
                        BrokerConnectionStatus::Reconnecting { attempt } => attempt,
                        _ => 0,
                    };
                    Err(FatalError::Io(format!(
                        "broker セッションが再接続待機中です（試行 {attempt} 回目）"
                    )))
                }
            }
        })
    }

    /// The specs go to the broker in one call; the results come back in the
    /// same order (`banto_broker::ReadOnlyHandle::read`). Any
    /// `banto_broker::BrokerError` - the session down, the request lost to a
    /// connection failure, the broker task gone - is connection-fatal for
    /// the task ([`FatalError::Io`] with the broker's message).
    fn read_batch<'a>(&'a mut self, specs: &'a [TagSpec]) -> BoxFuture<'a, ReadBatch> {
        let handle = self.handle.clone();
        let batch = specs.to_vec();
        Box::pin(async move {
            handle
                .read(batch)
                .await
                .map_err(|err| FatalError::Io(err.to_string()))
        })
    }

    /// No-op - the broker session is a shared, long-lived asset this client
    /// never owns and must never close.
    fn disconnect(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

/// The client [`broker_client_factory`] hands out for a connection key it has
/// no broker handle for: never connected (every connect attempt fails), so
/// the task records Bad and keeps backing off instead of opening a socket of
/// its own.
struct NoSessionClient {
    connection_key: String,
}

impl CollectClient for NoSessionClient {
    fn connect(&mut self) -> BoxFuture<'_, Result<(), FatalError>> {
        let message = format!(
            "接続 {} の broker セッションがありません",
            self.connection_key
        );
        Box::pin(async move { Err(FatalError::Io(message)) })
    }

    fn read_batch<'a>(&'a mut self, _specs: &'a [TagSpec]) -> BoxFuture<'a, ReadBatch> {
        Box::pin(async {
            Err(FatalError::Io("broker セッションがありません".to_string()))
        })
    }

    fn disconnect(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

/// The production [`ClientFactory`]: a [`BrokerReadClient`] around the handle
/// `handles` holds for the connection key (`"conn:{id}"`). A key with no
/// handle (its session could not be started - the caller logged why) gets a
/// client that never connects, so that connection's tags read Bad; no second
/// socket is ever opened behind the broker's back (#337: some devices accept
/// exactly one client connection).
pub fn broker_client_factory(handles: Arc<HashMap<String, ReadOnlyHandle>>) -> ClientFactory {
    Arc::new(move |connection_key: &str| -> Box<dyn CollectClient> {
        match handles.get(connection_key) {
            Some(handle) => Box::new(BrokerReadClient::new(handle.clone())),
            None => Box::new(NoSessionClient {
                connection_key: connection_key.to_string(),
            }),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- #344 (2026-09-15): `connect()` follows the broker's real state ----
    //
    // Driven through `banto_broker::spawn_test_handle_with_status`, whose
    // `watch::Sender` lets these tests move the session through exactly the
    // transitions a real broker task would publish, with no socket and no
    // backoff timing to race against. Moved from banto-hub's `broker_glue.rs`
    // with the client (R2b-3a-3).

    /// A `Connected` session answers immediately (no grace wait).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn connect_succeeds_immediately_while_the_broker_session_is_connected() {
        let (handle, _status_tx, _task) =
            banto_broker::spawn_test_handle_with_status(1, BrokerConnectionStatus::Connected);
        let mut client = BrokerReadClient::new(handle.read_only());

        let started = std::time::Instant::now();
        assert!(client.connect().await.is_ok());
        assert!(
            started.elapsed() < BROKER_RECONNECT_GRACE,
            "a Connected session must not wait out the reconnect grace"
        );
    }

    /// PR #368 review: the fast path must not trust a stale `Connected` once
    /// the broker task itself is gone.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn connect_fails_when_the_broker_task_is_gone_even_if_the_last_status_was_connected() {
        let (handle, status_tx, _task) =
            banto_broker::spawn_test_handle_with_status(1, BrokerConnectionStatus::Connected);
        let mut client = BrokerReadClient::new(handle.read_only());

        drop(status_tx);

        let err = client.connect().await.expect_err(
            "a session whose broker task has exited must fail even if it was last seen Connected",
        );
        let text = err.to_string();
        assert!(
            text.contains("終了"),
            "the error should say the broker session's task has exited - got {text}"
        );
    }

    /// The transient blip [`BROKER_RECONNECT_GRACE`] exists for: the session
    /// comes back inside the window, so the task sees one slightly slow
    /// connect attempt instead of a failure.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn connect_waits_out_a_reconnecting_session_that_recovers_in_time() {
        let (handle, status_tx, _task) = banto_broker::spawn_test_handle_with_status(
            1,
            BrokerConnectionStatus::Reconnecting { attempt: 1 },
        );
        let mut client = BrokerReadClient::new(handle.read_only());

        let flip = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let _ = status_tx.send(BrokerConnectionStatus::Connected);
        });

        let started = std::time::Instant::now();
        assert!(
            client.connect().await.is_ok(),
            "a session that recovers inside the grace window must connect"
        );
        assert!(
            started.elapsed() < BROKER_RECONNECT_GRACE,
            "it must return as soon as the session is back, not at the end of the window"
        );
        flip.await.expect("the status flipper should not panic");
    }

    /// The #344 case itself: a session still `Reconnecting` when the window
    /// expires fails the connect, naming the broker's attempt counter.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn connect_fails_when_the_broker_session_stays_reconnecting() {
        let (handle, _status_tx, _task) = banto_broker::spawn_test_handle_with_status(
            1,
            BrokerConnectionStatus::Reconnecting { attempt: 7 },
        );
        let mut client = BrokerReadClient::new(handle.read_only());

        let started = std::time::Instant::now();
        let err = client
            .connect()
            .await
            .expect_err("a session that never recovers must fail the connect");
        assert!(
            started.elapsed() >= BROKER_RECONNECT_GRACE - Duration::from_millis(100),
            "the grace window must actually be waited out before giving up"
        );
        let text = err.to_string();
        assert!(
            text.contains("再接続待機中") && text.contains('7'),
            "the error should name the broker's own attempt counter - got {text}"
        );
    }

    /// A stopped session fails immediately - nothing left to wait for.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn connect_fails_immediately_for_a_stopped_broker_session() {
        let (handle, _status_tx, _task) =
            banto_broker::spawn_test_handle_with_status(1, BrokerConnectionStatus::Stopped);
        let mut client = BrokerReadClient::new(handle.read_only());

        let started = std::time::Instant::now();
        assert!(client.connect().await.is_err());
        assert!(
            started.elapsed() < BROKER_RECONNECT_GRACE,
            "a Stopped session must not wait out the reconnect grace"
        );
    }

    /// #337: a connection with a handle reads through it; one without gets a
    /// client that never connects (no fallback socket of its own).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_factory_routes_by_connection_key_and_never_dials_for_a_missing_one() {
        let (handle, _status_tx, _task) =
            banto_broker::spawn_test_handle_with_status(1, BrokerConnectionStatus::Connected);
        let mut handles = HashMap::new();
        handles.insert("conn:1".to_string(), handle.read_only());
        let factory = broker_client_factory(Arc::new(handles));

        let mut routed = factory("conn:1");
        assert!(routed.connect().await.is_ok());
        assert_eq!(
            routed
                .read_batch(&[])
                .await
                .expect("the fake broker answers"),
            Vec::new()
        );

        let mut missing = factory("conn:2");
        let err = missing
            .connect()
            .await
            .expect_err("a connection without a broker session must not connect");
        assert!(err.to_string().contains("conn:2"), "got {err}");
    }
}
