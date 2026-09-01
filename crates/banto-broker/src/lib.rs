//! `banto-broker`: the PLC access broker (I6, docs/tag-server-design.md
//! §6-5). One live SLMP session per PLC CPU ([`BrokerSupervisor::spawn`]
//! spawns one [`tokio::task`] per [`banto_tags::PlcConnection`]), and every
//! read/write caller reaches it through this crate so that a read and a
//! write to the same CPU can never interleave on the wire - see "Message
//! shape and how serialization is guaranteed" below for exactly how that is
//! a structural property, not a lock.
//!
//! ## Extraction history (I6, 2026-08-05)
//!
//! This crate *is* `apps/relay-wright/core/src/engine/broker.rs` (W3-A, 1119
//! 行) - extracted verbatim (same types, same field names, same policies, no
//! logic changes) so a second consumer (banto-hub's write path, wired in
//! T2-2) can reuse the exact "read/write serialized on one socket" guarantee
//! instead of re-implementing it and risking the two copies drifting apart -
//! the motivating risk docs/tag-server-design.md §10 item 3 records: "同型
//! 再実装は2実装の乖離リスクが最悪". Relay-wright's own behavior is
//! unchanged by the move: `apps/relay-wright/core/src/engine/mod.rs` still
//! documents the *why* of this module from inside relay-wright's W3 engine
//! (safety invariant #2, "structural eval/exec separation" - only
//! `writer::Writer` holds a write-capable [`BrokerHandle`]); this crate's doc
//! is this module's *how*, now portable to any caller rather than
//! relay-wright-specific. Relay-wright's pre-extraction test suite (`cargo
//! test -p relay-wright-core`) is the regression net for the move: every
//! relay-wright caller now reaches these types via `banto_broker::` instead
//! of `crate::engine::broker::`, with zero behavioral change.
//!
//! ## Protocol abstraction (I9 / Issue #130, 2026-09-01) and the one driver
//! registered today
//!
//! This crate used to speak MELSEC SLMP exclusively, hardcoded throughout.
//! Since #130, read/write execution against a live session is behind a small
//! trait, [`session::BrokerSession`] (see that module's doc for the full
//! contract) - this file's reconnect state machine ([`ConnState`],
//! [`ConnEvent`], [`run_broker_task`]) and its job-processing loop only ever
//! call `BrokerSession::read_batch`/`write_batch`/`disconnect` on a
//! `Box<dyn BrokerSession>`, never a protocol-specific type or function. The
//! one place [`banto_tags::PlcConnection::protocol`] is still inspected is
//! [`SessionDirectory::ensure_connection`], and that is now a small data
//! table ([`DRIVERS`]) mapping a protocol string to a driver's own
//! "build this connection's session and spawn its task" constructor, in
//! place of the old `conn.protocol != SLMP_PROTOCOL` equality check.
//!
//! Two drivers are registered as of #131 (2026-09-01): `"slmp"`
//! ([`slmp_driver`] module, wrapping `banto_plc`'s
//! `plan_slmp_batch`/`execute_slmp_batch_reads` and `banto_plc_write`'s
//! `plan_slmp_write_batch`/`execute_slmp_writes` behind
//! [`slmp_driver::SlmpSession`] - moved out of this file's job loop by #130,
//! not changed), and `"modbus-tcp"` ([`modbus_driver`] module, wrapping
//! `banto_plc`'s `plan_batch_requests`/`execute_modbus_reads` and
//! `banto_plc_write`'s `plan_modbus_writes`/`execute_modbus_writes` behind
//! [`modbus_driver::ModbusSession`] - #131's own addition, landed in the same
//! PR as this paragraph's update). Every other `protocol` value is still
//! rejected with [`BrokerError::UnsupportedProtocol`] (see
//! [`is_supported_protocol`] for the single source of truth on which
//! protocol strings are currently registered). This refactor was tracked
//! since docs/tag-server-design.md §6 item 7 as **I9**, "Modbus 書き込み
//! （banto-plc-write への FC5/6/15/16 追加 + broker のプロトコル抽象化）は
//! I9 バックログ" - #130 did the protocol-abstraction half (no Modbus
//! behavior), and #131 (this PR) adds the `"modbus-tcp"` driver itself, so
//! I9 is now fully done.
//!
//! ### Why a new trait, not `banto_plc::PlcClient` + `banto_plc_write::PlcWriteClient`
//!
//! [`session::BrokerSession`] is a **new** trait defined in this crate, not a
//! reuse of the two existing read/write traits, and that is a deliberate,
//! load-bearing choice - see `session.rs`'s module doc ("Why one trait with
//! both `read_batch` and `write_batch`") for the full reasoning: composing
//! `PlcClient` (reads) with `PlcWriteClient` (writes) would give this broker
//! two independent trait objects, each wrapping its own private
//! `Option<slmp::SLMPClient>` socket, which reopens exactly the "read and
//! write to the same CPU can interleave on the wire, or fight over one PLC
//! session slot" problem this crate's extraction (I6) was meant to close for
//! good - see "Message shape and how serialization is guaranteed" below.
//!
//! ## Message shape and how serialization is guaranteed
//!
//! Each connection's task ([`BrokerSupervisor::spawn`] / the internal
//! `spawn_task`) owns exactly one `Box<dyn session::BrokerSession>` and a
//! `tokio::sync::mpsc::Receiver<Job>`. [`Job`] has two variants -
//! `Read`/`Write` - each carrying its owned request `Vec` and a
//! `tokio::sync::oneshot::Sender` for the reply. The task's main loop takes
//! jobs off the channel **one at a time** and `.await`s the whole
//! `read_batch`/`write_batch` call before looking at the next one; there is
//! exactly one mutable borrow of the session alive at any instant, and it
//! never crosses an await point held by two jobs at once. So the
//! one-session-at-a-time property is structural (the same argument
//! `banto-collect/src/task.rs`'s module doc makes for its
//! single-task-owns-the-client design), not a lock - a read and a write to
//! the same CPU cannot interleave on the wire because nothing ever runs two
//! `BrokerSession` calls concurrently against one session, and (see "Why a
//! new trait" above) there is only ever one `BrokerSession` per connection to
//! begin with, never a separate read-side and write-side instance.
//!
//! [`BrokerHandle`] is the clonable submission point (holds the mpsc
//! `Sender`); many callers (a poller and a writer, in either consuming app)
//! can hold clones and submit concurrently - the mpsc channel is what
//! serializes their requests onto the one task, in arrival order, with no
//! corruption possible because the task itself is the only thing touching
//! the session.
//!
//! ## Reconnect / backoff policy
//!
//! Structure copied from `banto-collect/src/task.rs`'s `run_connection`
//! (`ConnState`/`ConnEvent`/spawned-connect-attempt shape), *not* its
//! `TsWriter`-flavoured content: `ConnState` here is `Backoff { at } |
//! Connecting(JoinHandle<..>) | Connected(Box<dyn session::BrokerSession>)`,
//! and the initial state is `Backoff { at: now }` so the first connect
//! attempt fires immediately. A failed connect attempt reschedules the next
//! one after [`backoff_delay`] (exponential, parameterized by
//! [`BackoffConfig`], capped, reset to attempt 0 on any success). A
//! connection-fatal failure *while processing a request* (see below) drops
//! the session and re-enters `Backoff` immediately (attempt reset to 0) -
//! the same "no disconnect event needed, we were already using it a moment
//! ago" reasoning `run_connection` uses for its own fatal-read branch.
//!
//! Each driver supplies its own reusable "dial a fresh session" step (a
//! [`session::Connector`], built once per connection and called again on
//! every reconnect attempt) rather than this file calling a
//! protocol-specific connect function directly - see [`connect_attempt`].
//! For the one driver registered today, [`slmp_driver::connector`] dials
//! through [`banto_plc::dial_slmp`], the one shared implementation of the
//! connect sequence (build a bare `slmp::SLMPClient` from
//! `SlmpConfig::to_wire_props()`, wire the two per-crate timeouts, wrap the
//! connect in `SlmpConfig::connect_timeout`, map the structured
//! `slmp::SlmpError` (H9, docs/h9-slmp-structured-error-spec.md)) that
//! `banto_plc::slmp::SlmpClient::connect` and
//! `banto_plc_write::slmp::SlmpWriteClient::connect` also call - see
//! `dial_slmp`'s own doc comment (H9 transport 共通化,
//! docs/improvement-plan.md §H9) - exactly what this file's own
//! `connect_attempt` did inline before #130 moved the SLMP-specific half of
//! it into [`slmp_driver::connector`]. What still differs here, and is not
//! shareable, is what happens to the client *after* the dial - see "Why a new
//! trait" above for why this broker wraps it in its own session type
//! ([`slmp_driver::SlmpSession`]) rather than handing it to `SlmpClient`'s or
//! `SlmpWriteClient`'s own private `Option<slmp::SLMPClient>`.
//!
//! ## Queued-request-while-down policy: fail fast, never queue
//!
//! A [`Job`] is only ever handled while `ConnState::Connected` - the `_ =>
//! None` arm below covers both `Backoff` (including "never connected yet")
//! and `Connecting`, and answers immediately with
//! [`BrokerError::Disconnected`] rather than buffering the request until a
//! session comes back. A caller (a condition poller or auto-writer in
//! relay-wright, or a write request handler in banto-hub) must be able to
//! tell "the session is down right now" apart from "this one request
//! failed", and a request that silently blocked until the next successful
//! reconnect (which may be 30s away under the backoff cap) would be
//! indistinguishable from a hang to its caller. Fail-fast keeps that decision
//! (retry now, retry later, or give up) with the caller, which is where
//! relay-wright's rate limiting and edge-trigger logic (and banto-hub's own
//! write-path policy) actually lives.
//!
//! When a request that *was* running against a live session hits a
//! connection-fatal error mid-flight, that one request's oneshot is failed
//! with [`BrokerError::ConnectionFailed`] (carrying the underlying error's
//! message) **and** the task falls back into `Backoff` - both happen from the
//! same match arm, so the two can never drift out of sync.
//!
//! ## The `Err` contract every `BrokerSession` implementation must uphold
//!
//! (Formerly "Why no explicit call to `is_connection_fatal` appears here" -
//! renamed for #130 because the contract this section describes now lives on
//! a trait, not just on two SLMP-specific functions.)
//!
//! [`run_broker_task`]'s job loop never calls anything resembling
//! `is_connection_fatal` - it trusts [`session::BrokerSession::read_batch`]/
//! [`write_batch`][session::BrokerSession::write_batch] completely: `Err`
//! means connection-fatal, full stop, and any per-device failure must already
//! be folded into `Ok(Vec<_>)` by the time it gets here. For
//! [`slmp_driver::SlmpSession`] that split is inherited for free: both
//! `banto_plc::execute_slmp_batch_reads` and
//! `banto_plc_write::execute_slmp_writes` already enforce it internally (a
//! device-side SLMP end code becomes a `Bad`/per-target failure folded into
//! `Ok(Vec<_>)`; only a connection-fatal condition surfaces as `Err`), so
//! `SlmpSession` just maps their `Err` straight through to its own. A future
//! driver will not get this for granted the same way - see
//! [`session::BrokerSession`]'s own doc comment, which now carries this
//! contract explicitly (rather than it living only in `banto_plc`'s and
//! `banto_plc_write`'s own doc comments, as it did pre-#130) precisely
//! because the next implementer (Modbus TCP, #131) has to do that
//! classification itself and needs to find the rule stated somewhere that
//! is not SLMP-specific.
//!
//! ## Connection status observability (I6 T2-1 addition, 2026-08-05)
//!
//! [`BrokerConnectionStatus`] plus [`BrokerHandle::status_watch`] /
//! [`SessionDirectory::status_watch`] expose the same `ConnState` lifecycle
//! the previous sections describe as a `tokio::sync::watch` value external
//! code can observe: banto-hub's `/api/v1/status` (docs/tag-server-design.md
//! §8) needs to report whether each managed connection is up, and banto-hub's
//! PLC断/再接続 event generation needs a transition to fire on - both need to
//! see this broker's connection state from *outside* the task. This is a
//! purely additive extension: the watch `send`s happen at the same state
//! transitions `run_broker_task` already performs (entering `Connected`,
//! entering `Backoff` - whether from a failed connect or a mid-flight fatal
//! request error, and task shutdown), so they ride along with the existing
//! job-processing and fail-fast-policy code without changing when or how a
//! [`Job`] is accepted, queued, or rejected. relay-wright does not read this
//! watch today (its own arm/disarm/rate-limiter state is unrelated); it is
//! wired for banto-hub's T2-2 slice.
//!
//! ## Session removal (T7-2 backlog item, 2026-08-05)
//!
//! [`SessionDirectory::remove`] completes the session-sync contract T2-2
//! originally left one-directional (see [`SessionDirectory`]'s own doc
//! comment, "Removal", for the full mechanism and its explicit non-guarantee:
//! a broker task only exits once *every* clone of its handle is dropped, and
//! `remove` only ever accounts for this directory's own clone). relay-wright
//! does not call `remove` (its own session set only ever grows for the
//! process lifetime, §6-5/T2-2's original design); it is wired for
//! banto-hub's T7-2 slice (banto-hub's `CollectorManager::rebuild` diffs
//! [`SessionDirectory::connection_ids`] against the registry's current
//! enabled-SLMP-connection set to find what to remove).

use std::collections::HashMap;
use std::time::Duration;

use banto_plc::{BatchReadRequest, BatchReadResult, SlmpConfig, WordOrder};
use banto_plc_write::{BatchWriteRequest, WriteResult};
use banto_tags::PlcConnection;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

mod modbus_driver;
mod session;
mod slmp_driver;

use session::{BrokerSession, Connector, SessionError};

/// The protocol string identifying the SLMP driver ([`slmp_driver`]) in
/// [`DRIVERS`] (see the module doc's "Protocol abstraction" section).
const SLMP_PROTOCOL: &str = "slmp";

/// The protocol string identifying the Modbus TCP driver ([`modbus_driver`])
/// in [`DRIVERS`] (#131, 2026-09-01) - see the module doc's "Protocol
/// abstraction" section.
const MODBUS_PROTOCOL: &str = "modbus-tcp";

/// Parse [`PlcConnection::word_order`]'s wire string into the
/// [`WordOrder`] [`SlmpConfig::word_order`] wants (P3-b, 監査指摘
/// 2026-08-12: this function replacing the old hardcoded
/// `..SlmpConfig::default()` is the fix). Falls back to
/// [`WordOrder::LowHigh`] - [`SlmpConfig::default`]'s own value, so an
/// unrecognized string behaves exactly as every connection did before this
/// column existed - rather than propagating an error, because
/// `banto_tags::plc_connection::ALLOWED_WORD_ORDERS` plus the SQL `CHECK`
/// migration `0010` added already make any other value unreachable through
/// normal CRUD; this is defense in depth for a hand-edited or
/// pre-migration-0010 database row, not a path this broker expects to take
/// in practice.
fn parse_word_order(value: &str) -> WordOrder {
    match value {
        "high_low" => WordOrder::HighLow,
        // "low_high" and anything unrecognized both land here - see this
        // fn's own doc comment for why an unrecognized value fails open to
        // the historical default rather than erroring.
        _ => WordOrder::LowHigh,
    }
}

/// Build [`ensure_connection`][SessionDirectory::ensure_connection]'s
/// [`SlmpConfig`] from `conn` - factored out of that method so
/// `word_order_reflects_the_connections_own_setting_not_the_default` below
/// can exercise the field mapping directly, without spawning a real broker
/// task or dialing a socket. `port` is taken pre-validated (already an
/// [`Result::Ok`] `u16` by the time the caller has one) rather than
/// re-deriving it from `conn.port` here, matching that caller's existing
/// order of operations.
fn slmp_config_for(conn: &PlcConnection, port: u16) -> SlmpConfig {
    SlmpConfig {
        host: conn.host.clone(),
        port,
        // P3-b (監査指摘 2026-08-12): previously `..SlmpConfig::default()`
        // alone, which fixed every session to `WordOrder::LowHigh` regardless
        // of what the connection actually specified - a device needing
        // `WordOrder::HighLow` would silently get byte-swapped u32/f32 values
        // with no error anywhere. See `parse_word_order`'s doc comment for
        // the fallback this now goes through.
        word_order: parse_word_order(&conn.word_order),
        ..SlmpConfig::default()
    }
}

/// Build [`ensure_connection`][SessionDirectory::ensure_connection]'s
/// [`banto_plc::ModbusTcpConfig`] from `conn` - the Modbus twin of
/// [`slmp_config_for`], factored out for the same test-without-a-socket
/// reason.
///
/// `unit_id`: `banto_tags::PlcConnection::unit_id` is registry-validated to
/// `0..=255` at input time (`banto-tags`'s `MIN_UNIT_ID..=MAX_UNIT_ID`), so
/// `conn.unit_id: i64` always fits `u8` for any row that passed normal CRUD -
/// the `unwrap_or_else` fallback to `ModbusTcpConfig::default().unit_id` is
/// defense-in-depth for a hand-edited/pre-migration row, exactly the same
/// fail-open posture [`parse_word_order`] documents for `word_order`, not a
/// path expected in practice.
///
/// `word_order`: reuses [`parse_word_order`] (the *same* helper
/// [`slmp_config_for`] uses, not a duplicate) even though
/// [`banto_plc::ModbusTcpConfig::default`]'s own `word_order` is
/// [`WordOrder::HighLow`] - the opposite of [`SlmpConfig::default`]'s
/// `WordOrder::LowHigh`. `parse_word_order`'s fallback to `WordOrder::LowHigh`
/// on an unrecognized string is unconditional regardless of which protocol
/// calls it; this matches `banto_plc_write::modbus::ModbusWriteClient`'s
/// already-documented behavior of taking the connection's configured order
/// through this same shared `word_order` column, so a hand-edited/
/// pre-migration Modbus row fails open to `LowHigh` too, not to
/// `ModbusTcpConfig::default()`'s `HighLow`.
fn modbus_config_for(conn: &PlcConnection, port: u16) -> banto_plc::ModbusTcpConfig {
    banto_plc::ModbusTcpConfig {
        host: conn.host.clone(),
        port,
        unit_id: u8::try_from(conn.unit_id)
            .unwrap_or_else(|_| banto_plc::ModbusTcpConfig::default().unit_id),
        word_order: parse_word_order(&conn.word_order),
        ..banto_plc::ModbusTcpConfig::default()
    }
}

/// Default channel depth for one connection's job queue. Generous relative to
/// the request sizes either consuming app deals with per cycle; the
/// fail-fast-when-down policy is what actually bounds caller latency, not
/// this number.
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
/// (W3-A / I6), not a richer application-level failure vocabulary.
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

    /// [`BrokerSupervisor::spawn`] was given a connection whose `protocol`
    /// has no [`DRIVERS`] entry - as of #131 (2026-09-01), that means
    /// anything other than `"slmp"` or `"modbus-tcp"`. Such a connection is
    /// rejected outright rather than silently skipped, so a caller relying on
    /// `.handle(id)` finds out immediately at startup rather than discovering
    /// a missing handle later as an unexplained `None`.
    ///
    /// The "slmp, modbus-tcp" list below is hand-written rather than derived
    /// from [`DRIVERS`] at the `#[error(...)]` site (a `thiserror` format
    /// string cannot call a function), so it must be kept in sync with
    /// [`DRIVERS`] by hand if a third driver is ever added.
    #[error("接続 {connection_id} のプロトコル {protocol} はブローカー未対応です（対応: slmp, modbus-tcp）")]
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
/// result. Exactly two variants - this *is* the read/write safety boundary a
/// caller like relay-wright's engine relies on: a caller that only ever gets
/// a [`ReadOnlyHandle`] can never construct a `Write` job.
enum Job {
    Read {
        requests: Vec<BatchReadRequest>,
        respond_to: oneshot::Sender<Result<Vec<BatchReadResult>, BrokerError>>,
    },
    Write {
        requests: Vec<BatchWriteRequest>,
        respond_to: oneshot::Sender<Result<Vec<WriteResult>, BrokerError>>,
    },
}

/// Observable connection state for one broker task's session (I6 T2-1
/// addition, 2026-08-05) - see the module doc's "Connection status
/// observability" section for the full why. Same three-state shape as
/// `banto_collect::ConnectionStatus` by design (both track the same kind of
/// connect/backoff/stopped lifecycle) but **independently defined here**:
/// this crate must not gain a dependency on `banto-collect` (that dependency
/// would run backwards from every other dependency this crate has, and
/// banto-collect's own SLMP collection tasks are unrelated to - and unaware
/// of - this broker's sessions). `attempt` is the connect attempt currently
/// in flight or scheduled (1-based, matching [`BackoffConfig`]'s own
/// numbering).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokerConnectionStatus {
    Connected,
    Reconnecting { attempt: u32 },
    Stopped,
}

/// A clonable submission point for one PLC connection's broker task. Exposes
/// both [`BrokerHandle::read`] and [`BrokerHandle::write`]; see
/// [`BrokerHandle::read_only`] for the read-only subset. relay-wright's W3-B
/// design keeps this held only by its `writer` module, handing its `poller`
/// module a [`ReadOnlyHandle`] instead - a compile-time guarantee that only
/// one module can ever submit a write, enforced entirely by which handle type
/// a function accepts.
#[derive(Clone)]
pub struct BrokerHandle {
    connection_id: i64,
    tx: mpsc::Sender<Job>,
    /// See [`Self::status_watch`].
    status_rx: watch::Receiver<BrokerConnectionStatus>,
}

impl BrokerHandle {
    /// Submit a (possibly mixed numeric + string, S2 文字列タグ) read batch
    /// and await its result. Fails fast with [`BrokerError::Disconnected`] if
    /// the session is down - see the module doc's queued-request-while-down
    /// policy.
    pub async fn read(
        &self,
        requests: Vec<BatchReadRequest>,
    ) -> Result<Vec<BatchReadResult>, BrokerError> {
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

    /// Submit a (possibly mixed numeric + string) write batch and await its
    /// result. Same fail-fast-when-down policy as [`Self::read`].
    pub async fn write(
        &self,
        requests: Vec<BatchWriteRequest>,
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
    /// at all). relay-wright's W3-B hands this to its `poller` module.
    pub fn read_only(&self) -> ReadOnlyHandle {
        ReadOnlyHandle {
            inner: self.clone(),
        }
    }

    /// A live view of this connection's session state - see
    /// [`BrokerConnectionStatus`] and the module doc's "Connection status
    /// observability" section for why this exists and who consumes it.
    /// Cloning the returned [`watch::Receiver`] (as this method does
    /// internally) is cheap and every clone tracks the same underlying value
    /// independently - standard `tokio::sync::watch` semantics.
    pub fn status_watch(&self) -> watch::Receiver<BrokerConnectionStatus> {
        self.status_rx.clone()
    }
}

/// The read-only subset of [`BrokerHandle`] - see [`BrokerHandle::read_only`].
#[derive(Clone)]
pub struct ReadOnlyHandle {
    inner: BrokerHandle,
}

impl ReadOnlyHandle {
    /// Identical to [`BrokerHandle::read`].
    pub async fn read(
        &self,
        requests: Vec<BatchReadRequest>,
    ) -> Result<Vec<BatchReadResult>, BrokerError> {
        self.inner.read(requests).await
    }

    /// Identical to [`BrokerHandle::status_watch`].
    pub fn status_watch(&self) -> watch::Receiver<BrokerConnectionStatus> {
        self.inner.status_watch()
    }
}

/// Test-only fake broker: a [`BrokerHandle`] backed by a task that answers
/// every job successfully with no network and no clock - reads with an empty
/// result set, writes with all-[`WriteResult::Ok`]. Lets a caller's own unit
/// tests (e.g. relay-wright's `writer.rs` rate-limit gate test) drive code
/// that requires a write-capable handle fully deterministically, without
/// standing up a real (simulated) PLC session.
///
/// `pub` behind the `test-util` feature (rather than `pub(crate)`, as it was
/// pre-extraction) so a consumer's own test code can reach it too - this is
/// the whole reason the feature exists (see the crate's `[features]` doc in
/// `Cargo.toml`). The returned handle's [`BrokerHandle::status_watch`]
/// reports [`BrokerConnectionStatus::Connected`] once and never changes: this
/// fake never disconnects, so there is nothing to transition to.
#[cfg(any(test, feature = "test-util"))]
pub fn spawn_test_handle_answering_ok(connection_id: i64) -> (BrokerHandle, JoinHandle<()>) {
    let (tx, mut rx) = mpsc::channel::<Job>(JOB_CHANNEL_CAPACITY);
    let (_status_tx, status_rx) = watch::channel(BrokerConnectionStatus::Connected);
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
    (
        BrokerHandle {
            connection_id,
            tx,
            status_rx,
        },
        task,
    )
}

/// A shared, growable directory of live broker sessions - the seam a caller
/// like relay-wright's タグモニタ (tag monitor, feature/tag-monitor) uses to
/// reach the SAME one-session-per-connection broker tasks the engine itself
/// reads/writes through.
///
/// ## Why this exists (hard constraint: one SLMP session per connected port)
///
/// The real R08ENCPU accepts only ONE concurrent SLMP TCP connection **per
/// port** (verified on hardware 2026-08-07: a second connect to a port that
/// already has a live session times out, while separate ports opened via the
/// CPU's own parameters each carry their own simultaneous session fine - the
/// 2026-08-06 note this doc previously carried, "only ONE concurrent SLMP TCP
/// connection" with no qualifier, over-read that single-port observation as a
/// whole-CPU limit). This still means a monitor-style caller must never open
/// its own `SlmpClient` against a connection this crate already manages -
/// every such read AND manual write goes through the broker task that already
/// owns that connection's one session on its one port - because
/// [`banto_tags::PlcConnection`] fixes one port per connection row; nothing
/// here changes with the correction, since the broker's value was always
/// "serialize read/write on the one session a connection actually holds", not
/// "the CPU can only ever have one session total".
///
/// ## On-demand sessions
///
/// [`BrokerSupervisor::spawn`] seeds this directory with one task per
/// connection the caller's engine manages (e.g. every enabled SLMP connection
/// at relay-wright's engine start). A secondary caller (relay-wright's
/// monitor) may legitimately ask for a connection the engine has no task for:
/// one created/enabled AFTER the engine started, or one on an engine built
/// with an explicit connection subset. [`SessionDirectory::ensure_connection`]
/// spawns a broker task for such a connection ON FIRST USE and keeps it (an
/// idle task is one parked socket + a reconnect loop - cheap), so subsequent
/// polls reuse the session exactly like the engine's own poller does.
///
/// ## Lifecycle
///
/// All spawned tasks - seeded and on-demand alike - share the supervisor's
/// one shutdown `watch`, so [`BrokerSupervisor::shutdown`] stops every task
/// this directory ever created (the tasks map is shared; shutdown drains
/// and awaits it). Clones of this directory held past shutdown (e.g. by a
/// caller-defined control handle kept around) simply get
/// [`BrokerError::TaskGone`] / closed-channel errors - never a new session on
/// a dead engine's watch, because `ensure_connection` on a shut-down
/// directory spawns a task whose `shutdown_rx` already reads `true`, which
/// exits immediately.
///
/// ## Removal ([`Self::remove`], T7-2 backlog item, 2026-08-05)
///
/// T2-2 originally left session sync one-directional (`ensure_connection`
/// only - see banto-hub's `broker_glue` module doc for that history).
/// [`Self::remove`] completes it: it drops this directory's own
/// [`BrokerHandle`] clone (and forgets, without `.await`ing, the tracked
/// [`JoinHandle`]) for one connection id. A broker task exits when *every*
/// clone of its handle - wherever else a caller may have stashed one, e.g.
/// captured inside a long-lived `ClientFactory` closure - has been dropped
/// (see `run_broker_task`'s `rx.recv() == None` branch); this directory's own
/// clone is necessary but not always sufficient for that, so a caller with
/// other long-lived handle owners is responsible for releasing those
/// promptly too if it wants removal to actually free the task's resources in
/// a timely manner. We deliberately do not await the removed `JoinHandle`
/// here: by the time a caller like banto-hub's rebuild calls `remove`, its
/// OWN other references are typically already gone (it orders its collector
/// reconfiguration - which stops the connection's collect task - before
/// calling this), but nothing in this crate can prove that generically for
/// every caller, so blocking on the join here would risk hanging
/// indefinitely rather than trading away a small, bounded resource leak - a
/// worse failure mode than a task that simply finishes a little later in the
/// background.
#[derive(Clone)]
pub struct SessionDirectory {
    handles: std::sync::Arc<std::sync::Mutex<HashMap<i64, BrokerHandle>>>,
    tasks: std::sync::Arc<std::sync::Mutex<HashMap<i64, TaskEntry>>>,
    backoff: BackoffConfig,
    /// Shared (via `Arc`) with [`BrokerSupervisor`] so on-demand tasks
    /// subscribe to the SAME shutdown trigger as the seeded ones.
    shutdown_tx: std::sync::Arc<watch::Sender<bool>>,
}

/// The task-local stop signal travels with the join handle. The supervisor
/// shutdown watch remains a separate, shared signal so stopping one
/// connection cannot stop its siblings.
struct TaskEntry {
    stop_tx: watch::Sender<bool>,
    join_handle: JoinHandle<()>,
}

/// One [`DRIVERS`] entry: given a connection already confirmed to match this
/// driver's protocol string, its already-validated `u16` port, the shared
/// backoff config, and this task's clone of the supervisor-wide shutdown
/// signal, build and spawn that connection's whole broker task. Every driver
/// produces the same `(BrokerHandle, TaskEntry)` pair regardless of its own
/// config type or [`session::BrokerSession`] implementation - see
/// [`spawn_slmp_driver`] for the one registered today.
type DriverSpawn =
    fn(&PlcConnection, u16, BackoffConfig, watch::Receiver<bool>) -> (BrokerHandle, TaskEntry);

/// The protocol -> driver dispatch table [`SessionDirectory::ensure_connection`]
/// consults (Issue #130 D5). Two entries as of #131 (2026-09-01, Modbus TCP
/// write support) - `ensure_connection`'s own lookup logic did not need to
/// change to add the second one.
const DRIVERS: &[(&str, DriverSpawn)] = &[
    (SLMP_PROTOCOL, spawn_slmp_driver),
    (MODBUS_PROTOCOL, spawn_modbus_driver),
];

/// [`DriverSpawn`] for `"slmp"`: build this connection's [`SlmpConfig`] (see
/// [`slmp_config_for`]) and hand it to [`spawn_task`] - the same SLMP-specific
/// entry point this crate's own unit tests call directly (with a hand-built
/// `SlmpConfig`, bypassing `PlcConnection`/`ensure_connection` entirely - see
/// `spawn_task`'s own doc comment).
fn spawn_slmp_driver(
    conn: &PlcConnection,
    port: u16,
    backoff: BackoffConfig,
    shutdown_rx: watch::Receiver<bool>,
) -> (BrokerHandle, TaskEntry) {
    spawn_task(conn.id, slmp_config_for(conn, port), backoff, shutdown_rx)
}

/// [`DriverSpawn`] for `"modbus-tcp"` (#131, 2026-09-01): build this
/// connection's [`banto_plc::ModbusTcpConfig`] (see [`modbus_config_for`])
/// and hand it, already wrapped as a [`modbus_driver::connector`], to
/// [`spawn_task_with_connector`] - unlike [`spawn_slmp_driver`], this cannot
/// go through [`spawn_task`] itself, since that wrapper is hardcoded to
/// [`slmp_driver::connector`].
fn spawn_modbus_driver(
    conn: &PlcConnection,
    port: u16,
    backoff: BackoffConfig,
    shutdown_rx: watch::Receiver<bool>,
) -> (BrokerHandle, TaskEntry) {
    spawn_task_with_connector(
        conn.id,
        modbus_driver::connector(modbus_config_for(conn, port)),
        backoff,
        shutdown_rx,
    )
}

/// Whether `protocol` has a registered [`DRIVERS`] entry - the single source
/// of truth for "does the broker manage this connection's protocol", so a
/// caller outside this crate (banto-hub) never needs to hardcode the list of
/// supported protocol strings itself. Mirrors
/// [`SessionDirectory::ensure_connection`]'s own lookup.
pub fn is_supported_protocol(protocol: &str) -> bool {
    DRIVERS.iter().any(|(p, _)| *p == protocol)
}

impl SessionDirectory {
    fn new(backoff: BackoffConfig, shutdown_tx: std::sync::Arc<watch::Sender<bool>>) -> Self {
        Self {
            handles: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
            tasks: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
            backoff,
            shutdown_tx,
        }
    }

    /// The handle for one connection, if a task is already running for it.
    pub fn handle(&self, connection_id: i64) -> Option<BrokerHandle> {
        self.handles
            .lock()
            .expect("session directory poisoned")
            .get(&connection_id)
            .cloned()
    }

    /// The connection-status watch for `connection_id`, if a task is already
    /// running for it - see [`BrokerHandle::status_watch`]. A convenience
    /// wrapper (`handle(id).map(BrokerHandle::status_watch)`) for a caller
    /// (banto-hub's `/api/v1/status`, T2-2) that holds a [`SessionDirectory`]
    /// rather than an individual [`BrokerHandle`].
    pub fn status_watch(
        &self,
        connection_id: i64,
    ) -> Option<watch::Receiver<BrokerConnectionStatus>> {
        self.handle(connection_id).map(|h| h.status_watch())
    }

    /// The handle for `conn`, spawning its broker task first if none is
    /// running yet (see the struct doc's on-demand policy). Rejects a
    /// connection whose `protocol` has no [`DRIVERS`] entry with
    /// [`BrokerError::UnsupportedProtocol`] - `"slmp"` and `"modbus-tcp"` are
    /// the two registered as of #131 (see the module doc's "Protocol
    /// abstraction" section), so this still rejects everything else exactly
    /// as it did when this was a direct `!= SLMP_PROTOCOL` check.
    pub fn ensure_connection(&self, conn: &PlcConnection) -> Result<BrokerHandle, BrokerError> {
        let driver = DRIVERS
            .iter()
            .find(|(protocol, _)| *protocol == conn.protocol)
            .map(|(_, spawn)| *spawn)
            .ok_or_else(|| BrokerError::UnsupportedProtocol {
                connection_id: conn.id,
                protocol: conn.protocol.clone(),
            })?;
        let port = u16::try_from(conn.port).map_err(|_| BrokerError::InvalidPort {
            connection_id: conn.id,
            port: conn.port,
        })?;

        let mut handles = self.handles.lock().expect("session directory poisoned");
        if let Some(handle) = handles.get(&conn.id) {
            return Ok(handle.clone());
        }

        let (handle, task) = driver(conn, port, self.backoff, self.shutdown_tx.subscribe());
        handles.insert(conn.id, handle.clone());
        self.tasks
            .lock()
            .expect("session directory poisoned")
            .insert(conn.id, task);
        Ok(handle)
    }

    /// How many broker sessions are currently tracked (seeded + on-demand,
    /// minus anything [`Self::remove`]d since). Mainly a test/diagnostic
    /// helper - banto-hub's T7-2 E2E suite uses it to confirm a deleted
    /// connection's session was actually untracked.
    pub fn connection_count(&self) -> usize {
        self.tasks.lock().expect("session directory poisoned").len()
    }

    /// Every connection id currently tracked (seeded + on-demand, minus
    /// anything [`Self::remove`]d since) - the set a caller like banto-hub's
    /// `CollectorManager::rebuild` diffs against the registry's current
    /// enabled-SLMP-connection set to find sessions to [`Self::remove`].
    pub fn connection_ids(&self) -> Vec<i64> {
        self.handles
            .lock()
            .expect("session directory poisoned")
            .keys()
            .copied()
            .collect()
    }

    /// Drop this directory's own [`BrokerHandle`] (and tracked [`JoinHandle`])
    /// for `connection_id`, if one is currently tracked - see this struct's
    /// "Removal" doc section for exactly what this does and does not
    /// guarantee. Returns `true` if a session was tracked and is now
    /// untracked, `false` if none was (already removed, or never added).
    pub fn remove(&self, connection_id: i64) -> bool {
        let had_handle = self
            .handles
            .lock()
            .expect("session directory poisoned")
            .remove(&connection_id)
            .is_some();
        self.tasks
            .lock()
            .expect("session directory poisoned")
            .remove(&connection_id);
        had_handle
    }

    /// Stop one broker task and await its completion.
    ///
    /// The per-task signal is independent of the shared supervisor shutdown
    /// signal. Outstanding [`BrokerHandle`] clones therefore cannot keep this
    /// task alive: it observes its own signal, closes its socket, and the
    /// tracked join handle is awaited here.
    pub async fn stop_and_join(&self, connection_id: i64) -> bool {
        let entry = self
            .tasks
            .lock()
            .expect("session directory poisoned")
            .remove(&connection_id);
        // Remove the directory's handle after the task entry. `ensure_connection`
        // checks the handle map first; while the old handle is still present it
        // can only return a clone of the task that is already being stopped.
        let removed_handle = self
            .handles
            .lock()
            .expect("session directory poisoned")
            .remove(&connection_id);

        let was_tracked = removed_handle.is_some() || entry.is_some();
        if let Some(entry) = entry {
            let _ = entry.stop_tx.send(true);
            let _ = entry.join_handle.await;
        }
        was_tracked
    }
}

/// Spawns and owns one broker task per SLMP [`PlcConnection`], and hands out
/// [`BrokerHandle`]s keyed by connection id. The handle/task bookkeeping
/// lives in a [`SessionDirectory`] (shared, so a secondary caller like
/// relay-wright's tag monitor can add on-demand sessions that this supervisor
/// still shuts down - see that struct's doc).
pub struct BrokerSupervisor {
    directory: SessionDirectory,
    /// Out-of-band shutdown trigger shared with every task via a cloned
    /// [`watch::Receiver`] - see [`Self::shutdown`]. Independent of the job
    /// mpsc: a task must stop even while a [`BrokerHandle`] (and therefore a
    /// live `Sender`) is still held by some caller (a poller/writer that
    /// outlives the supervisor by design), which the mpsc-closes path alone
    /// can never signal. `Arc`-shared with the directory so on-demand tasks
    /// subscribe to the same trigger.
    shutdown_tx: std::sync::Arc<watch::Sender<bool>>,
}

impl BrokerSupervisor {
    /// Spawn one broker task per connection in `connections`. Every entry
    /// must be `protocol == "slmp"` - the first one that is not aborts the
    /// whole call with [`BrokerError::UnsupportedProtocol`] (see that
    /// variant's doc for why a reject-the-batch policy beats silently
    /// skipping). Tasks already spawned for earlier connections in the slice
    /// are simply dropped in that case: nothing has been handed out yet
    /// (`Self` is only returned on full success), so the directory's Arcs -
    /// including the shutdown `Sender` - drop with it, every task's
    /// `shutdown_rx.changed()` resolves `Err`, and they exit immediately -
    /// no explicit cleanup needed.
    pub fn spawn(
        connections: &[PlcConnection],
        backoff: BackoffConfig,
    ) -> Result<Self, BrokerError> {
        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        let shutdown_tx = std::sync::Arc::new(shutdown_tx);
        let directory = SessionDirectory::new(backoff, shutdown_tx.clone());

        for conn in connections {
            directory.ensure_connection(conn)?;
        }

        Ok(Self {
            directory,
            shutdown_tx,
        })
    }

    /// The handle for one connection, if it was among those `spawn` started a
    /// task for (or one the [`SessionDirectory`] has since added on demand).
    pub fn handle(&self, connection_id: i64) -> Option<BrokerHandle> {
        self.directory.handle(connection_id)
    }

    /// A clone of the shared session directory - the seam a caller like
    /// relay-wright's monitor carries alongside its own control handle (see
    /// [`SessionDirectory`]).
    pub fn directory(&self) -> SessionDirectory {
        self.directory.clone()
    }

    /// How many broker tasks are currently spawned. Mainly a test/diagnostic
    /// helper.
    pub fn connection_count(&self) -> usize {
        self.directory.connection_count()
    }

    /// Clean shutdown: flip the shared shutdown trigger so every task breaks
    /// out of its job loop on its own, then await each task's graceful exit.
    ///
    /// This does *not* rely on every [`BrokerHandle`] having been dropped.
    /// The mpsc `Sender` a `BrokerHandle` holds only closes (`rx.recv() ==
    /// None`) once *all* clones are gone, but a realistic caller (a
    /// long-lived poller/writer) holds a handle for the whole app lifetime -
    /// dropping only the handle map would leave that `Sender` alive and the
    /// task blocked in `rx.recv()` forever. The `watch` signal is
    /// out-of-band from that mpsc entirely, so it stops the task regardless
    /// of how many `BrokerHandle`s are still outstanding elsewhere. The
    /// directory's handle map is cleared too so `BrokerHandle` clones the
    /// directory itself owned release their `Sender`s, though the tasks no
    /// longer depend on that to exit. On-demand tasks the [`SessionDirectory`]
    /// added after spawn share the same tasks map, so they are awaited here
    /// exactly like the seeded ones. A connection [`SessionDirectory::remove`]d
    /// before shutdown is no longer in the map (`remove` forgets its
    /// `JoinHandle` without awaiting it - see that method's doc) so this loop
    /// does not wait on it specifically, but the task still receives the same
    /// `shutdown_tx` signal every other task does (subscribed independently
    /// of whether this directory still tracks it) and exits on its own.
    pub async fn shutdown(self) {
        let _ = self.shutdown_tx.send(true);
        self.directory
            .handles
            .lock()
            .expect("session directory poisoned")
            .clear();
        let tasks: HashMap<i64, TaskEntry> = std::mem::take(
            &mut self
                .directory
                .tasks
                .lock()
                .expect("session directory poisoned"),
        );
        for task in tasks.into_values() {
            let _ = task.stop_tx.send(true);
            let _ = task.join_handle.await;
        }
    }
}

/// Build one connection's `(handle, task)` pair from an already-built
/// [`Connector`] - the protocol-agnostic core [`spawn_task`] (SLMP) and
/// [`spawn_modbus_driver`] (Modbus TCP) both call, factored out (#131,
/// 2026-09-01) so a second driver did not need its own copy of this
/// bookkeeping. `shutdown_rx` is the task's clone of the supervisor-wide
/// shutdown trigger (see [`BrokerSupervisor::shutdown`]); a caller that does
/// not need that path (e.g. a test that only ever drops its lone
/// `BrokerHandle`) can pass a receiver from a `watch::channel` it never
/// signals.
fn spawn_task_with_connector(
    connection_id: i64,
    connector: Connector,
    backoff: BackoffConfig,
    shutdown_rx: watch::Receiver<bool>,
) -> (BrokerHandle, TaskEntry) {
    let (tx, rx) = mpsc::channel(JOB_CHANNEL_CAPACITY);
    let (stop_tx, stop_rx) = watch::channel(false);
    // Initial value mirrors `banto_collect::task::run_connection`'s own
    // startup send: the task's `ConnState` starts at `Backoff { at: now }`
    // (immediate first attempt), i.e. attempt 1 is about to fire.
    let (status_tx, status_rx) =
        watch::channel(BrokerConnectionStatus::Reconnecting { attempt: 1 });
    let task = tokio::spawn(run_broker_task(
        connection_id,
        connector,
        backoff,
        rx,
        shutdown_rx,
        stop_rx,
        status_tx,
    ));
    (
        BrokerHandle {
            connection_id,
            tx,
            status_rx,
        },
        TaskEntry {
            stop_tx,
            join_handle: task,
        },
    )
}

/// SLMP-specific wrapper over [`spawn_task_with_connector`], kept only so
/// existing tests (and any external caller) that pass a bare [`SlmpConfig`]
/// directly keep working unchanged after #131 generalized the underlying
/// spawn logic to any protocol's [`Connector`]. [`spawn_slmp_driver`] is the
/// `DRIVERS`-facing entry point; this is the lower-level one this crate's own
/// unit tests call directly (with a hand-built `SlmpConfig`, bypassing
/// `PlcConnection`/`ensure_connection` entirely).
fn spawn_task(
    connection_id: i64,
    config: SlmpConfig,
    backoff: BackoffConfig,
    shutdown_rx: watch::Receiver<bool>,
) -> (BrokerHandle, TaskEntry) {
    spawn_task_with_connector(
        connection_id,
        slmp_driver::connector(config),
        backoff,
        shutdown_rx,
    )
}

/// Connection lifecycle state within one broker task. Structurally identical
/// to `banto_collect::task::ConnState` (see the module doc).
enum ConnState {
    /// Waiting until `at` to spawn the next connect attempt.
    Backoff { at: Instant },
    /// A connect attempt is running in a spawned sub-task, so a slow
    /// `connect()` cannot stall the job loop.
    Connecting(JoinHandle<Result<Box<dyn BrokerSession>, String>>),
    /// Connected and owning the live session - the only state in which a
    /// [`Job`] is actually serviced. `Box<dyn BrokerSession>` is required for
    /// `dyn` dispatch, and incidentally keeps this variant small the same way
    /// the pre-#130 `Box<slmp::SLMPClient>` did (`slmp::SLMPClient` carries a
    /// fixed internal receive buffer that would otherwise make this variant
    /// far larger than `ConnState`'s other variants; that data now simply
    /// lives on the same heap allocation, inside whichever session type a
    /// driver's `Connector` produced).
    Connected(Box<dyn BrokerSession>),
}

/// What woke the connection side of the task's `select!`.
enum ConnEvent {
    Due,
    Finished(Result<Box<dyn BrokerSession>, String>),
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
            Ok(result) => ConnEvent::Finished(result),
            Err(_join_err) => ConnEvent::JoinError,
        },
    }
}

/// Call `connector` once (see the module doc's "Reconnect / backoff policy"
/// section) - the generic half of what a pre-#130 single `connect_attempt`
/// did inline for SLMP specifically; the protocol-specific half now lives in
/// each driver's own `Connector` (e.g. [`slmp_driver::connector`]).
async fn connect_attempt(connector: Connector) -> Result<Box<dyn BrokerSession>, String> {
    connector().await
}

/// One connection's broker loop: owns this connection's session end-to-end
/// (connect via `connector`, reconnect-with-backoff, disconnect on shutdown)
/// and services [`Job`]s serialized through `rx`. Protocol-agnostic by
/// construction (Issue #130 D4) - every read/write/disconnect call here goes
/// through [`session::BrokerSession`], never a protocol-specific type or
/// function; see the module doc's "Protocol abstraction" section. Exits on
/// either of two independent signals:
/// every [`BrokerHandle`] for this connection has been dropped (`rx.recv()`
/// returns `None`), or the supervisor-wide shutdown trigger fires
/// (`shutdown_rx` changes, or its `Sender` is dropped) - see
/// [`BrokerSupervisor::shutdown`] for why the latter is needed: a `Job`
/// sender clone can legitimately outlive the supervisor (a poller/writer
/// holding a [`BrokerHandle`] for the app's lifetime), so the mpsc alone
/// cannot be relied on to ever close. `status_tx` is sent to at every
/// `ConnState` transition (see the module doc's "Connection status
/// observability" section) - purely observational, it influences no control
/// flow here.
async fn run_broker_task(
    connection_id: i64,
    connector: Connector,
    backoff_cfg: BackoffConfig,
    mut rx: mpsc::Receiver<Job>,
    mut shutdown_rx: watch::Receiver<bool>,
    mut stop_rx: watch::Receiver<bool>,
    status_tx: watch::Sender<BrokerConnectionStatus>,
) {
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

            // Per-connection stop-and-join; unlike `shutdown_rx`, this does
            // not affect any sibling broker session.
            _ = stop_rx.changed() => {
                break;
            }

            conn_event = next_conn_event(&mut state) => {
                match conn_event {
                    ConnEvent::Due => {
                        attempt += 1;
                        let _ = status_tx.send(BrokerConnectionStatus::Reconnecting { attempt });
                        let handle = tokio::spawn(connect_attempt(connector.clone()));
                        state = ConnState::Connecting(handle);
                    }
                    ConnEvent::Finished(Ok(session)) => {
                        attempt = 0;
                        state = ConnState::Connected(session);
                        let _ = status_tx.send(BrokerConnectionStatus::Connected);
                    }
                    ConnEvent::Finished(Err(_err)) => {
                        let delay = backoff_delay(attempt, backoff_cfg);
                        state = ConnState::Backoff {
                            at: Instant::now() + delay,
                        };
                        let _ = status_tx.send(BrokerConnectionStatus::Reconnecting { attempt: attempt + 1 });
                    }
                    ConnEvent::JoinError => {
                        let delay = backoff_delay(attempt, backoff_cfg);
                        state = ConnState::Backoff {
                            at: Instant::now() + delay,
                        };
                        let _ = status_tx.send(BrokerConnectionStatus::Reconnecting { attempt: attempt + 1 });
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
                        let outcome: Option<Result<Vec<BatchReadResult>, SessionError>> = match &mut state {
                            ConnState::Connected(session) => {
                                Some(session.read_batch(&requests).await)
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
                                let _ = status_tx.send(BrokerConnectionStatus::Reconnecting { attempt: 1 });
                            }
                            None => {
                                let _ = respond_to.send(Err(BrokerError::Disconnected { connection_id }));
                            }
                        }
                    }
                    Job::Write { requests, respond_to } => {
                        let outcome: Option<Result<Vec<WriteResult>, SessionError>> =
                            match &mut state {
                                ConnState::Connected(session) => {
                                    Some(session.write_batch(&requests).await)
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
                                let _ = status_tx.send(BrokerConnectionStatus::Reconnecting { attempt: 1 });
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

    match state {
        ConnState::Backoff { .. } => {}
        ConnState::Connecting(handle) => {
            // `next_conn_event` awaits the inner connect attempt through a
            // borrowed JoinHandle. If the outer select is interrupted by a
            // stop signal, the handle remains in `state`; abort and await it
            // here so stopping a session never leaves a detached connection
            // attempt behind.
            handle.abort();
            let _ = handle.await;
        }
        ConnState::Connected(mut session) => {
            session.disconnect().await;
        }
    }
    let _ = status_tx.send(BrokerConnectionStatus::Stopped);
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use banto_plc::{Address, DataType, PlcValue, ReadRequest, StringReadRequest, TagValue};
    use banto_plc_write::slmp::simulator::Simulator;
    use banto_plc_write::{StringWriteRequest, WriteRequest};
    use futures_util::future::join_all;
    use tokio::io::AsyncReadExt;

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
            simulation: false,

            word_order: "low_high".to_string(),
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

    /// A numeric read request wrapped as the `Numeric` case of the mixed batch
    /// the broker speaks (S2 文字列タグ).
    fn rreq(raw: &str, data_type: DataType) -> BatchReadRequest {
        BatchReadRequest::Numeric(ReadRequest {
            address: Address::parse_slmp(raw).unwrap_or_else(|e| panic!("{raw}: {e}")),
            data_type,
        })
    }

    /// A string read request, the `String` case of the mixed batch.
    fn sreq(raw: &str, words: u16) -> BatchReadRequest {
        BatchReadRequest::String(StringReadRequest {
            address: Address::parse_slmp(raw).unwrap_or_else(|e| panic!("{raw}: {e}")),
            words,
        })
    }

    fn wreq(raw: &str, data_type: DataType, value: TagValue) -> BatchWriteRequest {
        BatchWriteRequest::Numeric(WriteRequest {
            address: Address::parse_slmp(raw).unwrap_or_else(|e| panic!("{raw}: {e}")),
            data_type,
            value,
        })
    }

    fn swreq(raw: &str, words: u16, value: &str) -> BatchWriteRequest {
        BatchWriteRequest::String(StringWriteRequest {
            address: Address::parse_slmp(raw).unwrap_or_else(|e| panic!("{raw}: {e}")),
            words,
            value: value.to_string(),
        })
    }

    /// T8 (docs/tag-server-design.md §6.1) bit-in-word write request.
    fn bwreq(raw: &str, value: bool) -> BatchWriteRequest {
        BatchWriteRequest::BitInWord {
            address: Address::parse_slmp(raw).unwrap_or_else(|e| panic!("{raw}: {e}")),
            value,
        }
    }

    /// Poll `handle.read` until the session comes up (real time - connecting
    /// to a loopback simulator settles in low single-digit milliseconds) or a
    /// generous deadline elapses, so tests don't race the connect task that
    /// starts in the background the instant a broker is spawned.
    async fn read_once_connected(
        handle: &BrokerHandle,
        requests: Vec<BatchReadRequest>,
    ) -> Vec<BatchReadResult> {
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
    // P3-b (監査指摘 2026-08-12): word_order wiring - `slmp_config_for` must
    // reflect the connection's own `word_order`, not silently fix every
    // session to `SlmpConfig::default()`'s `WordOrder::LowHigh`.
    // -----------------------------------------------------------------

    #[test]
    fn parse_word_order_maps_both_allowed_strings() {
        assert_eq!(parse_word_order("low_high"), WordOrder::LowHigh);
        assert_eq!(parse_word_order("high_low"), WordOrder::HighLow);
    }

    /// Defense-in-depth path (this fn's own doc comment): a value that could
    /// only reach here via a hand-edited or pre-migration-0010 database row
    /// fails open to the historical default rather than panicking or
    /// erroring.
    #[test]
    fn parse_word_order_falls_back_to_low_high_for_anything_unrecognized() {
        assert_eq!(parse_word_order("middle_endian"), WordOrder::LowHigh);
        assert_eq!(parse_word_order(""), WordOrder::LowHigh);
    }

    /// The regression test for the audit finding itself: `slmp_config_for`
    /// must carry the connection's own `word_order` into the `SlmpConfig` it
    /// builds, not the old `..SlmpConfig::default()`-only shape that fixed
    /// every session to `WordOrder::LowHigh` regardless of what the
    /// connection specified.
    #[test]
    fn word_order_reflects_the_connections_own_setting_not_the_default() {
        let mut c = conn(1, "slmp", "127.0.0.1", 5007);

        c.word_order = "high_low".to_string();
        assert_eq!(
            slmp_config_for(&c, 5007).word_order,
            WordOrder::HighLow,
            "a connection asking for high_low must not silently get the default"
        );

        c.word_order = "low_high".to_string();
        assert_eq!(slmp_config_for(&c, 5007).word_order, WordOrder::LowHigh);
    }

    /// Every other [`SlmpConfig`] field `slmp_config_for` does not set stays
    /// exactly [`SlmpConfig::default`]'s value - the "known limitation" this
    /// module's fix deliberately did not expand (CPU series, access route,
    /// timers; see the P3-b task's own report for why). Pins that scope so a
    /// future edit to this function does not silently widen it.
    #[test]
    fn slmp_config_for_only_overrides_host_port_and_word_order() {
        let c = conn(1, "slmp", "10.0.0.5", 5007);
        let config = slmp_config_for(&c, 5007);
        let default = SlmpConfig::default();
        assert_eq!(config.host, "10.0.0.5");
        assert_eq!(config.port, 5007);
        assert_eq!(config.word_order, WordOrder::LowHigh);
        assert_eq!(config.cpu, default.cpu);
        assert_eq!(config.connect_timeout, default.connect_timeout);
        assert_eq!(config.response_timeout, default.response_timeout);
        assert_eq!(config.network_id, default.network_id);
        assert_eq!(config.pc_id, default.pc_id);
        assert_eq!(config.io_id, default.io_id);
        assert_eq!(config.area_id, default.area_id);
        assert_eq!(config.serial_id, default.serial_id);
        assert_eq!(config.cpu_timer, default.cpu_timer);
    }

    // -----------------------------------------------------------------
    // Supervisor
    // -----------------------------------------------------------------

    /// Renamed from `supervisor_rejects_a_modbus_tcp_connection` (#131,
    /// 2026-09-01): that test's premise is now wrong - `"modbus-tcp"` is a
    /// registered [`DRIVERS`] protocol as of this PR, no longer rejected. The
    /// property this test guards (an unregistered protocol string is
    /// rejected outright, not silently skipped) is unchanged, so it is kept
    /// with a made-up unsupported protocol string instead of dropped.
    #[tokio::test]
    async fn supervisor_rejects_an_unsupported_protocol_connection() {
        let connections = [conn(1, "bogus-protocol", "127.0.0.1", 502)];
        let err = match BrokerSupervisor::spawn(&connections, BackoffConfig::default()) {
            Ok(_) => panic!("an unregistered protocol must be rejected"),
            Err(e) => e,
        };
        assert_eq!(
            err,
            BrokerError::UnsupportedProtocol {
                connection_id: 1,
                protocol: "bogus-protocol".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn supervisor_hands_out_a_handle_per_modbus_tcp_connection() {
        let sim = banto_plc_write::modbus::simulator::Simulator::start().await;
        let connections = [conn(7, "modbus-tcp", "127.0.0.1", sim.addr.port())];
        let supervisor = BrokerSupervisor::spawn(&connections, BackoffConfig::default())
            .expect("a modbus-tcp connection should spawn");
        assert_eq!(supervisor.connection_count(), 1);
        assert!(supervisor.handle(7).is_some());
        assert!(supervisor.handle(999).is_none());
        supervisor.shutdown().await;
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
    // Session removal (T7-2 backlog item, 2026-08-05)
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn remove_untracks_a_connection() {
        let sim = Simulator::start().await;
        let connections = [conn(7, "slmp", "127.0.0.1", sim.addr.port())];
        let supervisor = BrokerSupervisor::spawn(&connections, BackoffConfig::default())
            .expect("all-slmp connections should spawn");
        let directory = supervisor.directory();
        assert_eq!(directory.connection_ids(), vec![7]);

        assert!(
            directory.remove(7),
            "removing a tracked connection should report true"
        );
        assert_eq!(supervisor.connection_count(), 0);
        assert!(
            directory.handle(7).is_none(),
            "a removed connection's handle should no longer be reachable via the directory"
        );

        supervisor.shutdown().await;
    }

    #[tokio::test]
    async fn remove_is_false_and_idempotent_for_an_untracked_connection() {
        let directory = SessionDirectory::new(
            BackoffConfig::default(),
            std::sync::Arc::new(watch::channel(false).0),
        );
        assert!(!directory.remove(42), "nothing was ever tracked for id 42");
        assert!(!directory.remove(42), "removing twice is still just false");
    }

    #[tokio::test]
    async fn ensure_connection_after_remove_spawns_a_fresh_session() {
        let sim = Simulator::start().await;
        let connections = [conn(7, "slmp", "127.0.0.1", sim.addr.port())];
        let supervisor = BrokerSupervisor::spawn(&connections, BackoffConfig::default())
            .expect("all-slmp connections should spawn");
        let directory = supervisor.directory();

        directory.remove(7);
        assert_eq!(directory.connection_count(), 0);

        // ensure_connection does not remember that id 7 ever existed -
        // removal is a real forget, not a soft-delete - so this spawns a
        // brand new session exactly like the very first ensure_connection
        // did.
        let fresh = directory
            .ensure_connection(&conn(7, "slmp", "127.0.0.1", sim.addr.port()))
            .expect("re-ensuring a removed connection should spawn a fresh session");
        assert_eq!(directory.connection_count(), 1);
        let _ = read_once_connected(&fresh, vec![rreq("D0", DataType::U16)]).await;

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
        assert_eq!(results, vec![BatchReadResult::Value(PlcValue::F64(4321.0))]);

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
        assert_eq!(
            read_results,
            vec![BatchReadResult::Value(PlcValue::F64(777.0))]
        );

        supervisor.shutdown().await;
    }

    /// T8 E2E (docs/tag-server-design.md §6.1): a `BitInWord` write submitted
    /// through `BrokerHandle::write` - exactly the same entry point
    /// `write_then_read_reflects_the_write_over_the_single_session` above
    /// uses for an ordinary write - runs its full read/modify/write/confirm
    /// RMW sequence inside the one `Job::Write` this broker already knows how
    /// to service, and the result is visible to a normal follow-up read. This
    /// is the "broker needs no code changes" claim (§6.1, this crate's own
    /// module doc's "Why SLMP-only" section) demonstrated rather than merely
    /// asserted: `banto-broker`'s source has not changed to make this pass -
    /// only `banto-plc-write`'s planner/executor did.
    #[tokio::test]
    async fn bit_in_word_write_through_the_broker_lands_and_reads_back() {
        let sim = Simulator::start().await;
        sim.set_word(banto_plc::SlmpDevice::D, 300, 0x1234);
        let connections = [conn(1, "slmp", "127.0.0.1", sim.addr.port())];
        let supervisor =
            BrokerSupervisor::spawn(&connections, BackoffConfig::default()).expect("spawn");
        let handle = supervisor.handle(1).expect("handle");

        // Wait for the session to come up, same as the ordinary-write test.
        let _ = read_once_connected(&handle, vec![rreq("D300", DataType::U16)]).await;

        // bit 0 is clear in the seed word (0x1234) - set it.
        let write_results = handle
            .write(vec![bwreq("D300.0", true)])
            .await
            .expect("bit-in-word write should succeed");
        assert_eq!(write_results, vec![WriteResult::Ok]);

        // The bit landed, and every other bit of the seed word survived -
        // read back both as a whole word and as the individual bit tag.
        let read_results = handle
            .read(vec![
                rreq("D300", DataType::U16),
                rreq("D300.0", DataType::Bit),
            ])
            .await
            .expect("read should succeed");
        assert_eq!(
            read_results,
            vec![
                BatchReadResult::Value(PlcValue::F64(0x1235 as f64)),
                BatchReadResult::Value(PlcValue::Bit(true)),
            ]
        );

        supervisor.shutdown().await;
    }

    #[tokio::test]
    async fn string_write_then_read_round_trips_over_the_single_session() {
        // S2 文字列タグ: a mixed-batch string write lands in the simulator and
        // reads back as the same text (NUL-trimmed), proving the broker speaks
        // the string path end to end.
        let sim = Simulator::start().await;
        let connections = [conn(1, "slmp", "127.0.0.1", sim.addr.port())];
        let supervisor =
            BrokerSupervisor::spawn(&connections, BackoffConfig::default()).expect("spawn");
        let handle = supervisor.handle(1).expect("handle");

        // Wait for the session, then write a 4-word (8-byte) string to D300.
        let _ = read_once_connected(&handle, vec![sreq("D300", 4)]).await;

        let write_results = handle
            .write(vec![swreq("D300", 4, "OK")])
            .await
            .expect("string write should succeed");
        assert_eq!(write_results, vec![WriteResult::Ok]);

        let read_results = handle
            .read(vec![sreq("D300", 4)])
            .await
            .expect("string read should succeed");
        assert_eq!(
            read_results,
            vec![BatchReadResult::Value(PlcValue::Str("OK".to_string()))]
        );

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
                vec![BatchReadResult::Value(PlcValue::F64(expected))],
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
        let _ = task.join_handle.await;
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
        // next request times out (connection-fatal per
        // `banto_plc_write::execute_slmp_writes`'s classification of
        // `slmp::SlmpError::Timeout`), which must fail that request AND drop
        // into reconnect-backoff.
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
        assert_eq!(recovered, vec![BatchReadResult::Value(PlcValue::F64(55.0))]);

        drop(handle);
        let _ = task.join_handle.await;
    }

    // -----------------------------------------------------------------
    // Connection status observability (I6 T2-1 addition)
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn status_watch_observes_connected_then_reconnecting_after_stop() {
        // spawn_task (not BrokerSupervisor) so `fast_config` timeouts apply -
        // `BrokerSupervisor`/`SessionDirectory::ensure_connection` always
        // build `SlmpConfig::default()` internally, which is fine for the
        // other tests but would make this one wait out the default 1s
        // response_timeout if the severed connection ever needed it.
        let sim = Simulator::start().await;
        let config = fast_config(&sim);
        let (_shutdown_tx, shutdown_rx) = watch::channel(false);
        let (handle, task) = spawn_task(1, config, BackoffConfig::default(), shutdown_rx);
        let mut status = handle.status_watch();

        // Wait for the session to come up (via a read, like the other tests)
        // and confirm the watch agrees.
        let _ = read_once_connected(&handle, vec![rreq("D0", DataType::U16)]).await;
        status
            .wait_for(|s| *s == BrokerConnectionStatus::Connected)
            .await
            .expect("status watch should report Connected once the session is up");

        // Sever the simulator's listener/connections entirely (unlike
        // `hang()`, this closes the socket outright) - the in-flight read
        // below must fail connection-fatally, dropping the task into
        // Backoff/Reconnecting, which the watch must reflect.
        sim.stop();

        let _ = handle.read(vec![rreq("D0", DataType::U16)]).await;
        status
            .wait_for(|s| matches!(s, BrokerConnectionStatus::Reconnecting { .. }))
            .await
            .expect("status watch should report Reconnecting after the session drops");

        drop(handle);
        let _ = task.join_handle.await;
    }

    #[tokio::test]
    async fn status_watch_observes_stopped_on_shutdown() {
        let sim = Simulator::start().await;
        let connections = [conn(1, "slmp", "127.0.0.1", sim.addr.port())];
        let supervisor =
            BrokerSupervisor::spawn(&connections, BackoffConfig::default()).expect("spawn");
        let handle = supervisor.handle(1).expect("handle");
        let mut status = handle.status_watch();

        let _ = read_once_connected(&handle, vec![rreq("D0", DataType::U16)]).await;
        status
            .wait_for(|s| *s == BrokerConnectionStatus::Connected)
            .await
            .expect("status watch should report Connected once the session is up");

        drop(handle);
        supervisor.shutdown().await;

        // `wait_for` returns `Err` once the Sender side has dropped along
        // with the task exiting, but the last value it ever sent - Stopped -
        // is still readable via `borrow()`.
        assert_eq!(*status.borrow(), BrokerConnectionStatus::Stopped);
    }

    #[tokio::test]
    async fn session_directory_stop_and_join_stops_only_the_requested_task() {
        let connections = [
            conn(1, "slmp", "127.0.0.1", 0),
            conn(2, "slmp", "127.0.0.1", 0),
        ];
        let supervisor =
            BrokerSupervisor::spawn(&connections, BackoffConfig::default()).expect("spawn");
        let directory = supervisor.directory();
        // Keep a clone alive to prove that stop-and-join does not depend on
        // every caller dropping its BrokerHandle first.
        let retained_handle = supervisor.handle(1).expect("connection 1 handle");

        let stopped = tokio::time::timeout(Duration::from_secs(1), directory.stop_and_join(1))
            .await
            .expect("per-connection stop should be bounded");
        assert!(stopped);
        assert_eq!(directory.connection_count(), 1);
        assert!(directory.handle(1).is_none());
        assert!(directory.handle(2).is_some());

        assert!(directory.stop_and_join(2).await);
        assert_eq!(directory.connection_count(), 0);
        assert!(!directory.stop_and_join(1).await);

        drop(retained_handle);
        supervisor.shutdown().await;
    }

    #[tokio::test]
    async fn stop_and_join_aborts_an_inflight_connect_attempt() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().expect("local_addr").port();
        let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
        let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
        let server_task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let _ = accepted_tx.send(());
            let mut bytes = Vec::new();
            let _ = socket.read_to_end(&mut bytes).await;
            let _ = closed_tx.send(());
        });

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let directory =
            SessionDirectory::new(BackoffConfig::default(), std::sync::Arc::new(shutdown_tx));
        let (handle, task) = spawn_task(
            9,
            SlmpConfig {
                host: "127.0.0.1".to_string(),
                port,
                connect_timeout: Duration::from_secs(30),
                response_timeout: Duration::from_secs(30),
                ..Default::default()
            },
            BackoffConfig::default(),
            shutdown_rx,
        );
        directory
            .handles
            .lock()
            .expect("session directory poisoned")
            .insert(9, handle.clone());
        directory
            .tasks
            .lock()
            .expect("session directory poisoned")
            .insert(9, task);

        tokio::time::timeout(Duration::from_secs(1), accepted_rx)
            .await
            .expect("connect attempt should reach the local listener")
            .expect("listener task should stay alive");
        let stopped = tokio::time::timeout(Duration::from_secs(1), directory.stop_and_join(9))
            .await
            .expect("stop_and_join should not wait for connect_timeout");
        assert!(stopped, "connection should be tracked");
        tokio::time::timeout(Duration::from_secs(1), closed_rx)
            .await
            .expect("aborting the connect attempt should close its socket")
            .expect("listener task should observe the close");

        drop(handle);
        server_task.abort();
        let _ = server_task.await;
    }
}
