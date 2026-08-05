//! T2-2 (docs/tag-server-design.md §6-5, 2026-08-05 決定): wires banto-hub's
//! SLMP collection reads through the shared `banto-broker` (I6) session
//! instead of banto-collect opening its own socket, so a later write (T2-4)
//! can share that same session ("読み書き単一セッション") - Modbus connections
//! are unaffected (§6 item 7: "Modbus 接続は現行の直接クライアントのまま").
//!
//! ## Two pieces
//!
//! - [`BrokerReadClient`]: a `banto_collect::PlcClient` adapter around a
//!   [`banto_broker::ReadOnlyHandle`] - the client
//!   [`crate::hub::CollectorManager`]'s `banto_collect::ClientFactory`
//!   (T2-2's injection seam, `crates/banto-collect/src/task.rs`) hands back
//!   for every SLMP connection.
//! - [`HubSessions`]: the broker session directory itself, owned **outside**
//!   `CollectorManager` (design §6-5: "broker 本体は CollectorManager の外で
//!   生存させ、構成再構築を跨いで SLMP セッションを維持する") so a
//!   `CollectorManager::rebuild` never tears down a live SLMP socket - only
//!   `bin/banto-hub.rs`'s own shutdown does, via [`HubSessions::shutdown`].
//!
//! ## Why `connect()` always returns `Ok` immediately
//!
//! [`BrokerReadClient::connect`] never touches the network - the broker task
//! behind the wrapped handle owns the actual TCP session end-to-end
//! (connect/reconnect/backoff, `banto-broker`'s own `run_broker_task`) and
//! that task is *already running* by the time any `BrokerReadClient` exists
//! (spawned by [`HubSessions::ensure_connection`] during
//! `CollectorManager::rebuild`, independent of when banto-collect's
//! connection task happens to call `connect()`). Reporting immediate success
//! here just means banto-collect's own `ConnState` moves straight to
//! `Connected` and starts calling `read_batch` - which is exactly what should
//! happen, because whether the *broker's* session is actually up is a
//! question `read_batch`/[`banto_broker::BrokerError::Disconnected`] answers
//! per call, not something `connect()` could usefully pre-check (a check now
//! would just describe a state that may have already changed by the time the
//! next `read_batch` runs).
//!
//! ## The two-backoff double bookkeeping this deliberately creates
//!
//! banto-collect's own per-connection task (`crates/banto-collect/src/task.rs`)
//! still runs its own `ConnState`/backoff loop against this adapter, exactly
//! as it does for a direct `ModbusTcpClient`/`SlmpClient` - T2-2's
//! instructions are explicit that "banto-collect の接続タスク構造は変えず"
//! ("それ以外のタスク構造(ConnState/バックオフ/イベント)は一切変更しない" per
//! I8/T2-2's shared discipline). So there end up being **two** independent
//! backoff loops for one physical SLMP session once a connection is
//! broker-managed:
//!
//! 1. **banto-broker's own** (`crates/banto-broker/src/lib.rs`'s
//!    `run_broker_task`) - the one that actually owns the socket and
//!    reconnects it.
//! 2. **banto-collect's** (`task.rs`'s `run_connection`) - which, from this
//!    adapter's perspective, only ever sees "read succeeded" or "read
//!    failed" (`BrokerReadClient::connect` never fails, so banto-collect's
//!    own `ConnState` is `Connected` almost all the time; a `read_batch`
//!    `Err` drops it into `Backoff` for one cycle, then the very next
//!    connect attempt succeeds immediately per the point above and it is
//!    right back to calling `read_batch`).
//!
//! This is not a bug or wasted work: banto-collect's loop degrades into a
//! **retry-interval governor** for "how often to try `read_batch` again while
//! the broker is down" (its backoff no longer gates an actual socket connect,
//! just the polling cadence), while the broker's loop is the one true
//! reconnect/backoff authority for the physical session. The design's own
//! `/api/v1/status` decision (see `crate::hub::CollectorManager::broker_status`'s
//! doc comment) follows from this: banto-collect's `ConnectionStatus` for a
//! broker-managed connection is not a lie exactly, but it answers a less
//! useful question ("is banto-collect's own retry loop momentarily backing
//! off") than the broker's status answers ("is the physical session up") -
//! so `/api/v1/status` surfaces the broker's answer for SLMP connections,
//! per the design decision this module implements.
//!
//! ## Value type coverage: numeric/bit only
//!
//! [`BrokerReadClient::read_batch`] only ever needs to translate
//! `banto_plc::TagValue::{Bit, F64}` - `banto_collect::config::build_config`
//! (S1, `crates/banto-collect/src/config.rs`) skips every `"string"`-typed
//! tag before it ever becomes a `ReadRequest`
//! (`banto_tags::STRING_DATA_TYPE`), so no `ReadRequest` this adapter ever
//! receives can decode as `banto_plc::PlcValue::Str`. The mapping still
//! handles that case defensively (folds it into a per-request `Bad` rather
//! than panicking) in case that invariant is ever broken, but it is not a
//! path this crate's tests need to exercise as a *product* behavior - S2
//! string-tag reads are relay-wright's engine, which talks to
//! [`banto_broker::BrokerHandle::read`] directly with `BatchReadRequest::String`
//! entries, never through banto-collect/this adapter.

use std::collections::HashMap;
use std::sync::Arc;

use banto_broker::{
    BrokerConnectionStatus, BrokerError, BrokerHandle, BrokerSupervisor, ReadOnlyHandle,
    SessionDirectory,
};
use banto_plc::{
    BatchReadRequest, BatchReadResult, BoxFuture, PlcClient, PlcError, PlcValue, ReadRequest,
    ReadResult, TagValue,
};
use banto_tags::PlcConnection;
use tokio::sync::{watch, Mutex as AsyncMutex};

/// A `banto_collect::PlcClient` that reads through a shared broker session
/// instead of owning a socket - see this module's doc comment for the full
/// rationale. Cheap to construct (wraps a clonable [`ReadOnlyHandle`]); a
/// fresh one is handed out per connect attempt by
/// [`crate::hub::CollectorManager`]'s `banto_collect::ClientFactory` closure,
/// same as a real `ModbusTcpClient`/`SlmpClient` would be, even though every
/// instance shares the exact same underlying broker task.
pub struct BrokerReadClient {
    handle: ReadOnlyHandle,
}

impl BrokerReadClient {
    pub fn new(handle: ReadOnlyHandle) -> Self {
        Self { handle }
    }
}

/// Map one [`BatchReadResult`] (the broker's string-capable superset) back to
/// the numeric-only [`ReadResult`] banto-collect's `PlcClient` trait speaks -
/// see this module's doc comment ("Value type coverage") for why the `Str`
/// arm is unreachable in practice but handled anyway.
fn to_read_result(result: BatchReadResult) -> ReadResult {
    match result {
        BatchReadResult::Value(PlcValue::Bit(b)) => ReadResult::Value(TagValue::Bit(b)),
        BatchReadResult::Value(PlcValue::F64(v)) => ReadResult::Value(TagValue::F64(v)),
        BatchReadResult::Value(PlcValue::Str(_)) => ReadResult::Bad(PlcError::Protocol(
            "broker から数値タグに対する予期しない文字列応答がありました".to_string(),
        )),
        BatchReadResult::Bad(err) => ReadResult::Bad(err),
    }
}

impl PlcClient for BrokerReadClient {
    /// Always succeeds immediately without touching the network - see this
    /// module's doc comment ("Why `connect()` always returns `Ok`
    /// immediately").
    fn connect(&mut self) -> BoxFuture<'_, Result<(), PlcError>> {
        Box::pin(async { Ok(()) })
    }

    /// Every request in `requests` is numeric/bit (see this module's doc
    /// comment) - wrapped as `BatchReadRequest::Numeric` and submitted to the
    /// broker in one call, then unwrapped back to `ReadResult` in the same
    /// order the broker guarantees (`banto_broker::BrokerHandle::read`'s doc:
    /// results align 1:1 with the request `Vec`). A [`BrokerError`] of any
    /// variant - the session being down
    /// ([`BrokerError::Disconnected`]/[`BrokerError::ConnectionFailed`]) or
    /// the broker task itself having exited
    /// ([`BrokerError::TaskGone`]) - becomes a whole-call
    /// [`PlcError::Connection`], which `banto-collect`'s `task.rs` treats as
    /// connection-fatal unconditionally (it does not branch on `PlcError`
    /// variant - see this module's doc comment for how that folds into the
    /// existing Bad-row/backoff machinery).
    fn read_batch<'a>(
        &'a mut self,
        requests: &'a [ReadRequest],
    ) -> BoxFuture<'a, Result<Vec<ReadResult>, PlcError>> {
        let handle = self.handle.clone();
        let batch: Vec<BatchReadRequest> = requests
            .iter()
            .copied()
            .map(BatchReadRequest::Numeric)
            .collect();
        Box::pin(async move {
            match handle.read(batch).await {
                Ok(results) => Ok(results.into_iter().map(to_read_result).collect()),
                Err(err) => Err(PlcError::Connection(err.to_string())),
            }
        })
    }

    /// No-op - the broker session is a shared, long-lived asset this client
    /// never owns and must never close (module doc comment: "broker セッションは
    /// 共有資産なので閉じない"). `banto-collect`'s own task calls this on
    /// graceful shutdown; the broker session simply outlives it, exactly as
    /// designed (§6-5: "broker 本体は CollectorManager の外で生存").
    fn disconnect(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

/// The broker session directory itself, owned outside `CollectorManager` (see
/// this module's doc comment for why) and shared (via `Arc`) between
/// `bin/banto-hub.rs` (construction + final shutdown) and
/// `crate::hub::CollectorManager` (session sync on every `rebuild`, status
/// lookups for `/api/v1/status`).
///
/// ## Session sync policy: `ensure_connection`-only, no removal (2026-08-05
/// 判断記録)
///
/// [`banto_broker::SessionDirectory`]/[`BrokerSupervisor`] expose no "stop
/// this one connection's task" API (only whole-supervisor
/// [`BrokerSupervisor::shutdown`]) - by design, per that crate's own doc
/// comment, a broker session is meant to be a long-lived shared asset, not
/// something callers churn. So `CollectorManager::rebuild`'s SLMP session sync
/// (T2-2 instructions: "レジストリの SLMP 接続集合と broker のセッション集合を
/// 同期") is one-directional: every enabled SLMP connection the registry
/// currently has gets [`HubSessions::ensure_connection`]'d (spawns a fresh
/// broker task on first sight, returns the existing handle - and therefore
/// the existing live session - on every later rebuild for the same
/// connection id, which is exactly the "維持" §6-5 requires). A connection
/// that is deleted or disabled leaves its broker task **running, parked,
/// idle** (still holding its socket and retrying its own backoff loop
/// forever) rather than being torn down - a known, accepted resource-leak
/// limitation until either an operator restarts banto-hub or
/// `banto-broker`/`SessionDirectory` grows a removal API (I7/I9-adjacent
/// backlog, not in T2-2's scope). This is the "不要分の放置" option T2-2's
/// instructions call out as acceptable when the shared crate's API does not
/// support anything better; it was chosen over "stop everything and respawn"
/// specifically because a full stop-and-respawn would re-open every SLMP
/// session on *every* rebuild, defeating the entire point of T2-2 (SLMP
/// session continuity across registry writes, §6-5's "構成再構築を跨いで SLMP
/// セッションを維持する（T0 既知の「再構築時の二重接続窓」も SLMP については
/// 解消）").
pub struct HubSessions {
    directory: SessionDirectory,
    /// `Some` until [`Self::shutdown`] consumes it (`BrokerSupervisor::shutdown`
    /// takes `self` by value) - guarded by an async mutex rather than
    /// `std::sync::Mutex` because `shutdown` itself is `.await`-heavy (drains
    /// every broker task) and must not hold a sync lock across that.
    supervisor: AsyncMutex<Option<BrokerSupervisor>>,
}

impl HubSessions {
    /// Start with zero sessions - `BrokerSupervisor::spawn` with an empty
    /// connection slice cannot fail (its only failure mode,
    /// [`BrokerError::UnsupportedProtocol`], requires a connection to reject)
    /// and gives a real, empty [`SessionDirectory`] that
    /// [`Self::ensure_connection`] grows on demand as `CollectorManager`
    /// discovers SLMP connections during its first (and every later) rebuild.
    pub fn new(backoff: banto_broker::BackoffConfig) -> Self {
        let supervisor = BrokerSupervisor::spawn(&[], backoff)
            .expect("spawning a broker supervisor with zero connections cannot fail");
        let directory = supervisor.directory();
        Self {
            directory,
            supervisor: AsyncMutex::new(Some(supervisor)),
        }
    }

    /// The handle for `conn` (must be `protocol == "slmp"`), spawning its
    /// broker task on first sight and reusing the same live session on every
    /// later call for the same connection id - see this struct's "Session
    /// sync policy" doc section.
    pub fn ensure_connection(&self, conn: &PlcConnection) -> Result<BrokerHandle, BrokerError> {
        self.directory.ensure_connection(conn)
    }

    /// The connection-status watch for `connection_id`, if a broker task is
    /// running for it - `None` before the first `ensure_connection` call for
    /// that id (e.g. no rebuild has run yet, or the connection has never been
    /// SLMP). Consumed by `crate::hub::CollectorManager::broker_status` for
    /// `/api/v1/status` (see `broker_glue`'s module doc, "The two-backoff
    /// double bookkeeping" section, for why SLMP status is sourced here
    /// rather than from banto-collect's own status map).
    pub fn status_watch(
        &self,
        connection_id: i64,
    ) -> Option<watch::Receiver<BrokerConnectionStatus>> {
        self.directory.status_watch(connection_id)
    }

    /// How many broker tasks have been spawned so far (seeded-empty +
    /// every later `ensure_connection`, including ones for connections since
    /// deleted/disabled - see this struct's "Session sync policy" doc
    /// section). Mainly a test/diagnostic helper - proxies
    /// [`SessionDirectory::connection_count`] - the test suite uses it to
    /// assert a rebuild reused an existing session rather than spawning a
    /// second one for the same connection id.
    pub fn connection_count(&self) -> usize {
        self.directory.connection_count()
    }

    /// Stop every broker task this directory ever spawned (seeded-empty +
    /// every later `ensure_connection`) and await their clean exit. Called
    /// once, from `bin/banto-hub.rs`'s shutdown sequence, **after**
    /// `CollectorManager::shutdown` has already stopped the `Collector` (so
    /// no `BrokerReadClient` is still mid-`read_batch` when the session it
    /// depends on goes away) - see that binary's doc comment for the full
    /// ordering. A second call is a no-op (the `Option` is already `None`).
    pub async fn shutdown(&self) {
        let supervisor = self.supervisor.lock().await.take();
        if let Some(supervisor) = supervisor {
            supervisor.shutdown().await;
        }
    }
}

/// Build the `banto_collect::ClientFactory`
/// [`crate::hub::CollectorManager::rebuild`] hands to
/// `banto_collect::Collector::start_with_client_factory`: SLMP connections
/// (looked up by `banto_collect::ClientSpec::connection_key`, the same
/// `"conn:{id}"` key `slmp_handles` is keyed by - see
/// `crate::hub::CollectorManager`'s session-sync step) get a
/// [`BrokerReadClient`]; everything else (Modbus, and the defensive fallback
/// for an SLMP connection somehow missing from `slmp_handles`) gets
/// banto-collect's own `default_client_factory` - the same direct
/// `ModbusTcpClient`/`SlmpClient` construction every non-hub caller still
/// gets (T2-2: "Modbus 接続は現行の直接クライアントのまま").
pub fn hub_client_factory(
    slmp_handles: Arc<HashMap<String, ReadOnlyHandle>>,
) -> banto_collect::ClientFactory {
    let default = banto_collect::default_client_factory();
    Arc::new(
        move |spec: &banto_collect::ClientSpec| -> Box<dyn PlcClient> {
            match spec.protocol {
                banto_collect::ClientProtocol::Slmp => {
                    match slmp_handles.get(&spec.connection_key) {
                        Some(handle) => Box::new(BrokerReadClient::new(handle.clone())),
                        // Defensive only: every enabled SLMP connection was
                        // ensure_connection'd (and inserted here) earlier in the
                        // same rebuild, before this factory was built - see
                        // `crate::hub::CollectorManager::rebuild`. Falling back to
                        // the default direct SlmpClient rather than panicking
                        // means a task still runs (degraded to an unshared
                        // session) instead of the whole connection silently
                        // never collecting.
                        None => default(spec),
                    }
                }
                banto_collect::ClientProtocol::ModbusTcp => default(spec),
            }
        },
    )
}
