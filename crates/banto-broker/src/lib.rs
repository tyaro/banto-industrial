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
//! relay-wright-specific. Relay-wright's pre-extraction test suite was the
//! regression net for the move at the time (every relay-wright caller
//! reached these types via `banto_broker::` instead of
//! `crate::engine::broker::`, with zero behavioral change); relay-wright
//! itself was archived on 2026-09-24 (`archive/relay-wright-2026-09-24`,
//! see docs/plan.md) with its construct-and-drive-the-whole-stack behavior
//! left to this crate's own `#[cfg(test)]` suite below and the driver
//! crates' own tests (`banto-driver-slmp` / `banto-driver-modbus`, R1-4a).
//!
//! ## Protocol abstraction and the drivers (I9 #130 → R1-3a → R2b-3a-2)
//!
//! This crate used to speak MELSEC SLMP exclusively, hardcoded throughout.
//! #130 (I9) put read/write execution behind a small trait and a protocol ->
//! dial-function table; R1-3a made the table injected; R1-4a moved the two
//! drivers out into their own crates. **R2b-3a-2
//! (docs/banto-hub-driver-io-design.md §4.2) removed the table itself**: the
//! broker now dials through the `banto-driver-api`
//! [`DriverRegistry`](banto_driver_api::DriverRegistry) it is given. A
//! connection's protocol is broker-managed exactly when the registry holds a
//! [`DriverKind::Session`](banto_driver_api::DriverKind::Session) for it (from
//! R2b-3b-join also `Frame`) - [`is_broker_managed`] is that one predicate - and
//! [`SessionDirectory::ensure_connection`] dials it with
//! `SessionDriver::connect(settings, ConnectContext)`, where `settings` is
//! `banto_tags::settings_to_api(&conn.settings)` (connection-settings design
//! §3.5), validated by the driver's own `validate_config` first. A
//! `HostProvided` entry (the host's own I/O, e.g. banto-hub's `push`) or an
//! unregistered protocol is [`BrokerError::UnsupportedProtocol`].
//!
//! Requests and results are the driver API's types too: a job carries
//! `Vec<TagSpec>` / `Vec<WriteItem>` and answers `Vec<Result<Value, ItemError>>`
//! / `Vec<Result<(), ItemError>>`; a connection-fatal failure is a typed
//! [`FatalError`]. The session the task owns is a `Box<dyn
//! banto_driver_api::Session>` (the former `BrokerSession` trait is gone; see
//! `session.rs`). This crate depends on no driver - not even as a
//! dev-dependency - and no longer on `banto-plc` / `banto-plc-write`. Its own
//! unit tests drive the job loop with an in-memory fake session driver
//! (`#[cfg(test)] mod fake`); the tests that pin down a protocol's behaviour
//! *through* the broker live in each driver crate's `tests/broker.rs`.
//!
//! ### One trait with both `read_batch` and `write_batch`
//!
//! Both directions run on the *same* `&mut Box<dyn Session>` held in
//! [`ConnState::Connected`], never on two separate objects with their own
//! sockets - that would reopen the "read and write to the same CPU
//! interleave on the wire, or fight over one PLC session slot" problem this
//! crate's extraction (I6) was meant to close for good.
//!
//! ### Bounds on every session future (design §5.5)
//!
//! A session-type driver owns its I/O, so if it does not keep
//! `ConnectContext::response_timeout`, a job (and with it an immediate read
//! or a write request in the Hub) would never return. The task therefore
//! bounds every future it awaits on a session. Session dials are bounded at
//! `connect_timeout + 1 s` ([`CONNECT_SLACK`]); Frame dials additionally
//! allow 32 handshake steps × response_timeout and 100 ms of engine cleanup.
//! A read is bounded at
//! [`read_job_limit`] and a write at [`write_job_limit`] (`step limit × 2T +
//! 1 s`; the step limit is `specs.len() + 8` for a read and
//! `3 × items.len() + 8` for a write, api-design contract 7), and the
//! disconnect on stop at
//! [`DISCONNECT_LIMIT`]. A job that runs out of time fails with
//! [`FatalError::Timeout`] and the session is dropped and re-dialed, exactly
//! like a session that reported its own timeout. `2T` per step (not `T`)
//! because a session-type driver may give its send and its receive one
//! response timeout each (today's `banto-plc` does for SLMP); the outer
//! bound is only the insurance for a driver that breaks its own deadline.
//!
//! ## Message shape and how serialization is guaranteed
//!
//! Each connection's task ([`BrokerSupervisor::spawn`] / the internal
//! `spawn_task_with_connector_limit`) owns exactly one `Box<dyn Session>` and a
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
//! `Session` calls concurrently against one session, and (see "One trait
//! with both `read_batch` and `write_batch`" above) there is only ever one
//! `Session` per connection to begin with, never a separate read-side and
//! write-side instance.
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
//! Connecting(JoinHandle<..>) | Connected(Box<dyn Session>)`, and the initial
//! state is `Backoff { at: now }` so the first connect attempt fires
//! immediately. A failed connect attempt reschedules the next one after
//! [`backoff_delay`] (exponential, parameterized by [`BackoffConfig`],
//! capped, reset to attempt 0 on any success). A connection-fatal failure
//! *while processing a request* (see below) drops the session and re-enters
//! `Backoff` immediately (attempt reset to 0) - the same "no disconnect event
//! needed, we were already using it a moment ago" reasoning `run_connection`
//! uses for its own fatal-read branch.
//!
//! The protocol-specific "dial a fresh session" step is a
//! [`session::Connector`], built once per connection by
//! [`SessionDirectory::ensure_connection`] from the registry's driver and
//! called again on every reconnect attempt - see `connect_attempt`.
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
//! ## The `Err` contract every `Session` must uphold
//!
//! `run_broker_task`'s job loop never inspects an error to decide whether
//! it is fatal - it trusts `Session::read_batch` / `write_batch` completely:
//! `Err(FatalError)` means connection-fatal, full stop, and any per-item
//! failure must already be an [`ItemError`] inside `Ok(Vec<_>)`. See
//! `session.rs`'s module doc. The one check the loop adds is the count: a
//! session that answers a different number of results than it was asked is
//! treated as [`FatalError::Protocol`] (the caller must be able to index the
//! results by request), never trimmed or padded.
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
//! enabled broker-managed connection set to find what to remove).

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

use banto_driver_api::{
    ConnectContext, DriverKind, DriverRegistry, FatalError, ItemError, Session, SessionDriver,
    Settings, TagSpec, Value, WordOrder, WriteItem,
};
use banto_tags::PlcConnection;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod frame_tests;
mod session;

pub use session::{Connector, NoSecretOpener, SecretOpener};

/// Parse a connection's `word_order` setting (`"high_low"` / `"low_high"`)
/// into a [`WordOrder`] (P3-b, 監査指摘 2026-08-12). Falls back to
/// [`WordOrder::LowHigh`] - `banto_plc::SlmpConfig::default`'s own value, so
/// an unrecognized string behaves exactly as every connection did before the
/// setting existed - rather than propagating an error: the save path's
/// validation already makes any other value unreachable through normal CRUD;
/// this is defense in depth for a hand-edited database row.
///
/// `pub` since R1-4a: both driver crates' connection set-up
/// (`banto-driver-slmp`'s `frame_conn.rs`, `banto-driver-modbus`'s `frame_conn.rs`) call this
/// one function, so the two protocols' fallback cannot drift apart. The
/// broker itself does not call it.
pub fn parse_word_order(value: &str) -> WordOrder {
    match value {
        "high_low" => WordOrder::HighLow,
        // "low_high" and anything unrecognized both land here - see this
        // fn's own doc comment for why an unrecognized value fails open to
        // the historical default rather than erroring.
        _ => WordOrder::LowHigh,
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

/// How long a session may take beyond `ConnectContext::connect_timeout` to
/// finish connecting before the broker gives the attempt up as
/// [`FatalError::Timeout`] (design §5.5).
pub const CONNECT_SLACK: Duration = Duration::from_secs(1);

/// The slack added to a job's step budget (design §5.5).
pub const JOB_SLACK: Duration = Duration::from_secs(1);

/// How long stopping a connection waits for `Session::disconnect` before it
/// drops the future and moves on (design §5.5: stopping must not hang).
pub const DISCONNECT_LIMIT: Duration = Duration::from_secs(2);

/// The extra steps a job may take beyond its items (api-design contract 7:
/// `3 × items + 8` for a write; a read is `specs + 8`).
const EXTRA_STEPS: usize = 8;

/// The outer bound on one read job of `specs` items against a session whose
/// response timeout is `response` (design §5.5): `(specs + 8) × 2T + 1 s`.
///
/// `2T` per step because a session-type driver may spend one `T` sending and
/// another receiving (today's SLMP session does: the `slmp` crate's send and
/// receive deadlines are separate); a driver that keeps its own deadlines can
/// therefore never be cut by this bound - only one that breaks them. Frame
/// uses one `T` for the whole step and at most 100 ms for fatal cleanup;
/// the same outer bound also contains that smaller inner budget.
pub fn read_job_limit(specs: usize, response: Duration) -> Duration {
    steps_limit(specs.saturating_add(EXTRA_STEPS), response)
}

/// The outer bound on one write job of `items` items (design §5.5):
/// `(3 × items + 8) × 2T + 1 s` (api-design contract 7's step limit; an SLMP
/// bit-in-word write is three requests: read, write, confirm).
pub fn write_job_limit(items: usize, response: Duration) -> Duration {
    steps_limit(
        items.saturating_mul(3).saturating_add(EXTRA_STEPS),
        response,
    )
}

fn steps_limit(steps: usize, response: Duration) -> Duration {
    let steps = u32::try_from(steps).unwrap_or(u32::MAX);
    response
        .saturating_mul(2)
        .saturating_mul(steps)
        .saturating_add(JOB_SLACK)
}

/// The two timeouts the broker hands every driver in its `ConnectContext`
/// (and sizes its own outer bounds from, design §5.5). The defaults are the
/// ones `banto_plc::ModbusTcpConfig` / `SlmpConfig` have always used (connect
/// 3 s, response 1 s), so moving the dial onto the registry (R2b-3a-2)
/// changed no connection's timing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionTimeouts {
    /// `ConnectContext::connect_timeout`.
    pub connect: Duration,
    /// `ConnectContext::response_timeout`.
    pub response: Duration,
}

impl Default for SessionTimeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(3),
            response: Duration::from_secs(1),
        }
    }
}

/// Everything [`BrokerSupervisor::spawn_with`] takes besides the registry.
#[derive(Clone)]
pub struct BrokerOptions {
    /// Reconnect backoff.
    pub backoff: BackoffConfig,
    /// The timeouts handed to every driver (and the outer bounds' `T`).
    pub timeouts: SessionTimeouts,
    /// How a connection's secrets are opened just before its dial (see
    /// [`SecretOpener`]). Default: [`NoSecretOpener`].
    pub secrets: Arc<dyn SecretOpener>,
}

impl Default for BrokerOptions {
    fn default() -> Self {
        Self {
            backoff: BackoffConfig::default(),
            timeouts: SessionTimeouts::default(),
            secrets: Arc::new(NoSecretOpener),
        }
    }
}

impl std::fmt::Debug for BrokerOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrokerOptions")
            .field("backoff", &self.backoff)
            .field("timeouts", &self.timeouts)
            .finish_non_exhaustive()
    }
}

/// A protocol-independent identity of the settings a session was dialed with
/// (R2b-3a-2, design §4.2): a hash of the `Settings` without the driver's
/// secret keys, in key order, plus - for each secret key the connection holds
/// a reference for - that **reference** (`{"$secret": "<ref>"}`'s id, never
/// the value; a new secret is sealed under a new reference, so changing a
/// password changes the fingerprint and the session is re-dialed). It
/// replaced `BrokerHandle::slmp_cpu` (#605): banto-hub's connection test asks
/// "may I read this form through the running session?" and its simulation
/// registry asks "did the dial target change?" by comparing fingerprints,
/// whatever the protocol.
///
/// Only meaningful within one process (it uses the standard library's
/// hasher); it is never stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SettingsFingerprint(u64);

impl SettingsFingerprint {
    /// The fingerprint of `protocol` + `settings` alone, leaving out every key in
    /// `secret_keys` (the value of a secret never reaches the hash) - for
    /// settings that belong to no stored connection (no secret references).
    pub fn of(protocol: &str, settings: &Settings, secret_keys: &[String]) -> Self {
        Self::of_connection(protocol, None, settings, secret_keys)
    }

    /// The fingerprint a session of `conn` dialed with `settings` carries:
    /// [`Self::of`] plus the secret **references** `conn.settings` holds for
    /// `secret_keys` (`settings_to_api` drops them, so they come from the
    /// connection; a malformed one counts as a marker of its own). `None`
    /// (no stored connection) is the same as a connection without
    /// references. The opened values are never part of it.
    pub fn of_connection(
        protocol: &str,
        conn: Option<&PlcConnection>,
        settings: &Settings,
        secret_keys: &[String],
    ) -> Self {
        let mut pairs: Vec<(&str, &str)> = settings
            .iter()
            .filter(|s| !secret_keys.contains(&s.key))
            .map(|s| (s.key.as_str(), s.value.as_str()))
            .collect();
        pairs.sort_unstable();
        let mut refs: Vec<(&str, &str)> = Vec::new();
        if let Some(conn) = conn {
            for key in secret_keys {
                match conn.secret_ref_lookup(key) {
                    banto_tags::SecretRefLookup::Absent => {}
                    banto_tags::SecretRefLookup::Ref(r#ref) => refs.push((key.as_str(), r#ref)),
                    banto_tags::SecretRefLookup::Malformed => {
                        refs.push((key.as_str(), "\u{0}malformed"))
                    }
                }
            }
        }
        refs.sort_unstable();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        // The driver kind is part of the identity: the same host/port under
        // another protocol is another session (Codex review of #788, P2).
        protocol.hash(&mut hasher);
        pairs.hash(&mut hasher);
        refs.hash(&mut hasher);
        SettingsFingerprint(hasher.finish())
    }
}

/// Whether `registry` makes `protocol` a broker-managed protocol: the
/// registry holds a Session or Frame driver for it (R2b-3a-2 / 3b-join).
/// The single predicate behind [`SessionDirectory::ensure_connection`]'s
/// [`BrokerError::UnsupportedProtocol`] and banto-hub's
/// `HubSessions::is_broker_managed`.
pub fn is_broker_managed(registry: &DriverRegistry, protocol: &str) -> bool {
    resolve_session_driver(registry, protocol).is_some()
}

/// A registry driver adapted to the broker's Session interface, together with
/// the dial budget for its original kind. Hosts performing a direct connection
/// test must use the same resolver and deadline as the directory.
pub struct ResolvedSessionDriver {
    driver: Arc<dyn SessionDriver>,
    frame: bool,
}

impl ResolvedSessionDriver {
    /// The Session driver, or a Frame adapter enforcing the host's transports.
    pub fn driver(&self) -> &Arc<dyn SessionDriver> {
        &self.driver
    }

    /// Outer dial budget (design §5.5). Frame includes transport connect,
    /// 32 handshake exchanges and bounded cleanup; Session keeps its original
    /// `connect + 1 s`. All arithmetic saturates.
    pub fn connect_limit(&self, timeouts: SessionTimeouts) -> Duration {
        let limit = if self.frame {
            timeouts
                .connect
                .saturating_add(
                    timeouts
                        .response
                        .saturating_mul(banto_driver_engine::MAX_HANDSHAKE_STEPS as u32),
                )
                .saturating_add(banto_driver_engine::CLOSE_TIMEOUT)
        } else {
            timeouts.connect
        };
        limit.saturating_add(CONNECT_SLACK)
    }
}

/// Resolve the same Session / Frame entries accepted by [`is_broker_managed`].
/// Frame drivers are wrapped without changing their registry entry. HostProvided
/// and unknown protocols return `None`.
pub fn resolve_session_driver(
    registry: &DriverRegistry,
    protocol: &str,
) -> Option<ResolvedSessionDriver> {
    let (driver, frame): (Arc<dyn SessionDriver>, bool) = match registry.get(protocol)?.kind() {
        DriverKind::Session(driver) => (Arc::clone(driver), false),
        DriverKind::Frame(driver) => (Arc::new(session::FrameDial(Arc::clone(driver))), true),
        _ => return None,
    };
    Some(ResolvedSessionDriver { driver, frame })
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
    /// failure occurred (including the job running out of its outer bound,
    /// design §5.5); `reason` is the [`FatalError`]'s message. The broker has
    /// already dropped the session and started reconnecting by the time this
    /// reaches the caller.
    #[error("PLC接続 {connection_id} への要求が接続断で失敗しました: {reason}")]
    ConnectionFailed { connection_id: i64, reason: String },

    /// The broker task for this connection is no longer running (its mpsc
    /// receiver was dropped, e.g. after [`BrokerSupervisor::shutdown`]).
    #[error("PLC接続 {connection_id} のブローカータスクは終了しています")]
    TaskGone { connection_id: i64 },

    /// The connection's `protocol` has no Session or Frame driver in the
    /// registry the broker was given (unregistered, or a host-provided
    /// protocol whose I/O the host keeps - see [`is_broker_managed`]).
    /// Rejected outright rather than silently skipped, so a caller relying on
    /// `.handle(id)` finds out immediately.
    #[error("接続 {connection_id} のプロトコル {protocol} はブローカー未対応です（セッション型・フレーム型のドライバが登録されていません）")]
    UnsupportedProtocol {
        connection_id: i64,
        protocol: String,
    },

    /// The driver's `validate_config` refused the connection's settings
    /// (R2b-3a-2: replaced the broker's own `InvalidPort` check - the port's
    /// range is the driver's rule, design §4.2). `reason` lists the refused
    /// keys and messages.
    #[error("接続 {connection_id} の設定をドライバが断りました: {reason}")]
    InvalidSettings { connection_id: i64, reason: String },
}

/// One thing a broker task can be asked to do, paired with where to send the
/// result. Exactly two variants - this *is* the read/write safety boundary a
/// caller relies on: a caller that only ever gets a [`ReadOnlyHandle`] can
/// never construct a `Write` job.
enum Job {
    Read {
        specs: Vec<TagSpec>,
        respond_to: oneshot::Sender<ReadOutcome>,
    },
    Write {
        items: Vec<WriteItem>,
        respond_to: oneshot::Sender<WriteOutcome>,
    },
}

/// What a read job answers: one result per spec, in order.
pub type ReadOutcome = Result<Vec<Result<Value, ItemError>>, BrokerError>;

/// What a write job answers: one result per item, in order.
pub type WriteOutcome = Result<Vec<Result<(), ItemError>>, BrokerError>;

/// Observable connection state for one broker task's session (I6 T2-1
/// addition, 2026-08-05) - see the module doc's "Connection status
/// observability" section for the full why. Same three-state shape as
/// `banto_collect::ConnectionStatus` by design (both track the same kind of
/// connect/backoff/stopped lifecycle) but **independently defined here**:
/// this crate must not gain a dependency on `banto-collect`. `attempt` is the
/// connect attempt currently in flight or scheduled (1-based, matching
/// [`BackoffConfig`]'s own numbering).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokerConnectionStatus {
    Connected,
    Reconnecting { attempt: u32 },
    Stopped,
}

/// A clonable submission point for one PLC connection's broker task. Exposes
/// both [`BrokerHandle::read`] and [`BrokerHandle::write`]; see
/// [`BrokerHandle::read_only`] for the read-only subset - a compile-time
/// guarantee that only the holder of a full handle can ever submit a write,
/// enforced entirely by which handle type a function accepts.
#[derive(Clone)]
pub struct BrokerHandle {
    connection_id: i64,
    tx: mpsc::Sender<Job>,
    /// See [`Self::status_watch`].
    status_rx: watch::Receiver<BrokerConnectionStatus>,
    /// See [`Self::settings_fingerprint`].
    fingerprint: Option<SettingsFingerprint>,
}

impl std::fmt::Debug for BrokerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrokerHandle")
            .field("connection_id", &self.connection_id)
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

impl BrokerHandle {
    /// The fingerprint of the settings this session dials with (R2b-3a-2,
    /// replacing #605's `slmp_cpu`): metadata of the same generation as the
    /// handle, never re-read from a registry later. A session re-created with
    /// other settings is a new handle with a new fingerprint. `None` only for
    /// the test-util fakes.
    pub fn settings_fingerprint(&self) -> Option<SettingsFingerprint> {
        self.fingerprint
    }

    /// Submit a read batch (numbers, bits and strings mixed) and await its
    /// result: one `Result<Value, ItemError>` per spec, in order. Fails fast
    /// with [`BrokerError::Disconnected`] if the session is down - see the
    /// module doc's queued-request-while-down policy.
    pub async fn read(&self, specs: Vec<TagSpec>) -> ReadOutcome {
        let (respond_to, rx) = oneshot::channel();
        self.tx
            .send(Job::Read { specs, respond_to })
            .await
            .map_err(|_| BrokerError::TaskGone {
                connection_id: self.connection_id,
            })?;
        rx.await.map_err(|_| BrokerError::TaskGone {
            connection_id: self.connection_id,
        })?
    }

    /// Submit a write batch and await its result: one `Result<(), ItemError>`
    /// per item, in order. The whole batch is one job - nothing else runs on
    /// the session until it is done (one logical write, design §9). Same
    /// fail-fast-when-down policy as [`Self::read`].
    pub async fn write(&self, items: Vec<WriteItem>) -> WriteOutcome {
        let (respond_to, rx) = oneshot::channel();
        self.tx
            .send(Job::Write { items, respond_to })
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
    /// at all).
    pub fn read_only(&self) -> ReadOnlyHandle {
        ReadOnlyHandle {
            inner: self.clone(),
        }
    }

    /// A live view of this connection's session state - see
    /// [`BrokerConnectionStatus`] and the module doc's "Connection status
    /// observability" section for why this exists and who consumes it.
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
    /// Identical to [`BrokerHandle::settings_fingerprint`].
    pub fn settings_fingerprint(&self) -> Option<SettingsFingerprint> {
        self.inner.settings_fingerprint()
    }

    /// Identical to [`BrokerHandle::read`].
    pub async fn read(&self, specs: Vec<TagSpec>) -> ReadOutcome {
        self.inner.read(specs).await
    }

    /// Identical to [`BrokerHandle::status_watch`].
    pub fn status_watch(&self) -> watch::Receiver<BrokerConnectionStatus> {
        self.inner.status_watch()
    }
}

/// Test-only fake broker: a [`BrokerHandle`] backed by a task that answers
/// every job successfully with no network and no clock - reads with an empty
/// result set, writes with an `Ok(())` per item. Lets a caller's own unit
/// tests drive code that requires a write-capable handle fully
/// deterministically, without standing up a real (simulated) PLC session.
///
/// `pub` behind the `test-util` feature so a consumer's own test code can
/// reach it too (see the crate's `[features]` doc in `Cargo.toml`). The
/// returned handle's [`BrokerHandle::status_watch`] reports
/// [`BrokerConnectionStatus::Connected`] once and never changes.
#[cfg(any(test, feature = "test-util"))]
pub fn spawn_test_handle_answering_ok(connection_id: i64) -> (BrokerHandle, JoinHandle<()>) {
    let (handle, _status_tx, task) =
        spawn_test_handle_with_status(connection_id, BrokerConnectionStatus::Connected);
    (handle, task)
}

/// As [`spawn_test_handle_answering_ok`], but the caller keeps the
/// [`watch::Sender`] driving [`BrokerHandle::status_watch`] and picks the
/// status the handle starts in - so a consumer's tests can walk a session
/// through `Reconnecting -> Connected -> Stopped` deterministically, with no
/// socket and no backoff timing to race against.
///
/// Added for banto-hub #344 (2026-09-15): `broker_glue::BrokerReadClient::connect`
/// answers from this very watch channel, and that behavior is only testable
/// if a test can publish the statuses a real `run_broker_task` would.
///
/// **Keep the returned sender alive for as long as the test needs the
/// status.** Dropping it closes the watch channel, which is exactly how a
/// consumer observes "the broker task has exited".
#[cfg(any(test, feature = "test-util"))]
pub fn spawn_test_handle_with_status(
    connection_id: i64,
    initial: BrokerConnectionStatus,
) -> (
    BrokerHandle,
    watch::Sender<BrokerConnectionStatus>,
    JoinHandle<()>,
) {
    let (tx, mut rx) = mpsc::channel::<Job>(JOB_CHANNEL_CAPACITY);
    let (status_tx, status_rx) = watch::channel(initial);
    let task = tokio::spawn(async move {
        while let Some(job) = rx.recv().await {
            match job {
                Job::Read { respond_to, .. } => {
                    let _ = respond_to.send(Ok(Vec::new()));
                }
                Job::Write { items, respond_to } => {
                    let _ = respond_to.send(Ok(items.iter().map(|_| Ok(())).collect()));
                }
            }
        }
    });
    (
        BrokerHandle {
            connection_id,
            tx,
            status_rx,
            fingerprint: None,
        },
        status_tx,
        task,
    )
}

/// A shared, growable directory of live broker sessions - the seam a
/// secondary caller uses to reach the SAME one-session-per-connection broker
/// tasks the engine itself reads/writes through.
///
/// ## Why this exists (hard constraint: one SLMP session per connected port)
///
/// The real R08ENCPU accepts only ONE concurrent SLMP TCP connection **per
/// port** (verified on hardware 2026-08-07: a second connect to a port that
/// already has a live session times out, while separate ports opened via the
/// CPU's own parameters each carry their own simultaneous session fine).
/// This means a monitor-style caller must never open its own client against
/// a connection this crate already manages - every such read AND manual
/// write goes through the broker task that already owns that connection's
/// one session on its one port - because [`banto_tags::PlcConnection`] fixes
/// one port per connection row. (Some Modbus TCP devices, e.g. the オムロン
/// KM-D1-ETN, have the same one-client limit, #337.)
///
/// ## On-demand sessions
///
/// [`BrokerSupervisor::spawn`] seeds this directory with one task per
/// connection the caller's engine manages. A secondary caller may
/// legitimately ask for a connection the engine has no task for: one
/// created/enabled AFTER the engine started. [`SessionDirectory::ensure_connection`]
/// spawns a broker task for such a connection ON FIRST USE and keeps it (an
/// idle task is one parked socket + a reconnect loop - cheap), so subsequent
/// polls reuse the session.
///
/// ## Lifecycle
///
/// All spawned tasks - seeded and on-demand alike - share the supervisor's
/// one shutdown `watch`, so [`BrokerSupervisor::shutdown`] stops every task
/// this directory ever created (the tasks map is shared; shutdown drains
/// and awaits it). Clones of this directory held past shutdown simply get
/// [`BrokerError::TaskGone`] / closed-channel errors - never a new session on
/// a dead engine's watch, because `ensure_connection` on a shut-down
/// directory spawns a task whose `shutdown_rx` already reads `true`, which
/// exits immediately.
///
/// ## Removal ([`Self::remove`], T7-2 backlog item, 2026-08-05)
///
/// [`Self::remove`] drops this directory's own [`BrokerHandle`] clone (and
/// forgets, without `.await`ing, the tracked [`JoinHandle`]) for one
/// connection id. A broker task exits when *every* clone of its handle -
/// wherever else a caller may have stashed one - has been dropped (see
/// `run_broker_task`'s `rx.recv() == None` branch); this directory's own
/// clone is necessary but not always sufficient for that, so a caller with
/// other long-lived handle owners is responsible for releasing those
/// promptly too. We deliberately do not await the removed `JoinHandle`
/// here: nothing in this crate can prove generically that every other
/// reference is gone, so blocking on the join here would risk hanging
/// indefinitely rather than trading away a small, bounded resource leak.
#[derive(Clone)]
pub struct SessionDirectory {
    handles: Arc<std::sync::Mutex<HashMap<i64, BrokerHandle>>>,
    tasks: Arc<std::sync::Mutex<HashMap<i64, TaskEntry>>>,
    /// The drivers this directory dials through (R2b-3a-2: the registry's
    /// Session / Frame entries; see [`is_broker_managed`]).
    registry: Arc<DriverRegistry>,
    options: BrokerOptions,
    /// Shared (via `Arc`) with [`BrokerSupervisor`] so on-demand tasks
    /// subscribe to the SAME shutdown trigger as the seeded ones.
    shutdown_tx: Arc<watch::Sender<bool>>,
}

/// The task-local stop signal travels with the join handle. The supervisor
/// shutdown watch remains a separate, shared signal so stopping one
/// connection cannot stop its siblings.
struct TaskEntry {
    stop_tx: watch::Sender<bool>,
    join_handle: JoinHandle<()>,
}

impl SessionDirectory {
    fn new(
        registry: Arc<DriverRegistry>,
        options: BrokerOptions,
        shutdown_tx: Arc<watch::Sender<bool>>,
    ) -> Self {
        Self {
            handles: Arc::new(std::sync::Mutex::new(HashMap::new())),
            tasks: Arc::new(std::sync::Mutex::new(HashMap::new())),
            registry,
            options,
            shutdown_tx,
        }
    }

    /// The registry this directory dials through.
    pub fn registry(&self) -> &Arc<DriverRegistry> {
        &self.registry
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
    /// running for it - see [`BrokerHandle::status_watch`].
    pub fn status_watch(
        &self,
        connection_id: i64,
    ) -> Option<watch::Receiver<BrokerConnectionStatus>> {
        self.handle(connection_id).map(|h| h.status_watch())
    }

    /// The handle for `conn`, spawning its broker task first if none is
    /// running yet (see the struct doc's on-demand policy), dialing with the
    /// connection's own settings (`banto_tags::settings_to_api(&conn.settings)`,
    /// secrets left out - they are opened only by the dial, see
    /// [`SecretOpener`]). See [`Self::ensure_connection_with_settings`].
    pub fn ensure_connection(&self, conn: &PlcConnection) -> Result<BrokerHandle, BrokerError> {
        self.ensure_connection_with_settings(conn, banto_tags::settings_to_api(&conn.settings))
    }

    /// As [`Self::ensure_connection`], dialing with `settings` instead of the
    /// connection's own (banto-hub's simulation registry hands the settings
    /// with `host` / `port` replaced by its simulator's, design §4.4).
    ///
    /// Rejects a protocol without a Session or Frame driver in the registry with
    /// [`BrokerError::UnsupportedProtocol`], and settings the driver's
    /// `validate_config` refuses with [`BrokerError::InvalidSettings`] - both
    /// before looking for an existing session, both without any I/O (this
    /// runs under the directory's lock). An existing session for `conn.id` is
    /// returned as is: it is the caller's job to [`Self::remove`] a session
    /// whose settings changed first (compare
    /// [`BrokerHandle::settings_fingerprint`]).
    pub fn ensure_connection_with_settings(
        &self,
        conn: &PlcConnection,
        settings: Settings,
    ) -> Result<BrokerHandle, BrokerError> {
        let resolved = resolve_session_driver(&self.registry, &conn.protocol).ok_or_else(|| {
            BrokerError::UnsupportedProtocol {
                connection_id: conn.id,
                protocol: conn.protocol.clone(),
            }
        })?;
        let driver = resolved.driver();
        let refused = driver.validate_config(&settings);
        if !refused.is_empty() {
            let reason = refused
                .iter()
                .map(|e| format!("{}: {}", e.key, e.message))
                .collect::<Vec<_>>()
                .join("、");
            return Err(BrokerError::InvalidSettings {
                connection_id: conn.id,
                reason,
            });
        }

        let mut handles = self.handles.lock().expect("session directory poisoned");
        if let Some(handle) = handles.get(&conn.id) {
            return Ok(handle.clone());
        }

        let secret_keys = driver.secret_keys();
        let fingerprint =
            SettingsFingerprint::of_connection(&conn.protocol, Some(conn), &settings, &secret_keys);
        let connector = session_connector(
            conn.clone(),
            Arc::clone(driver),
            settings,
            secret_keys,
            self.options.timeouts,
            Arc::clone(&self.options.secrets),
        );
        let connect_limit = resolved.connect_limit(self.options.timeouts);
        let (mut handle, task) = spawn_task_with_connector_limit(
            conn.id,
            connector,
            self.options.backoff,
            self.options.timeouts,
            connect_limit,
            self.shutdown_tx.subscribe(),
        );
        handle.fingerprint = Some(fingerprint);
        handles.insert(conn.id, handle.clone());
        self.tasks
            .lock()
            .expect("session directory poisoned")
            .insert(conn.id, task);
        Ok(handle)
    }

    /// How many broker sessions are currently tracked (seeded + on-demand,
    /// minus anything [`Self::remove`]d since). Mainly a test/diagnostic
    /// helper.
    pub fn connection_count(&self) -> usize {
        self.tasks.lock().expect("session directory poisoned").len()
    }

    /// Every connection id currently tracked (seeded + on-demand, minus
    /// anything [`Self::remove`]d since) - the set a caller like banto-hub's
    /// `CollectorManager::rebuild` diffs against the registry's current
    /// enabled broker-managed connection set to find sessions to
    /// [`Self::remove`].
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
    /// task alive: it observes its own signal, disconnects its session
    /// (bounded by [`DISCONNECT_LIMIT`]), and the tracked join handle is
    /// awaited here.
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

/// The [`Connector`] [`SessionDirectory::ensure_connection_with_settings`]
/// builds for one connection: on every attempt, open the driver's secret
/// keys that `conn.settings` holds a reference for (only those, design §4.2),
/// add them to a copy of `settings`, `SessionDriver::connect` with a
/// `ConnectContext` (`conn:{id}`, the two timeouts). The copy lives in an
/// [`OpenedSettings`] from before the first secret is opened, so the opened
/// values are zeroized however the attempt ends - success, error, or the
/// future being dropped (the `connect_attempt` bound of design §5.5, or the
/// task being stopped or aborted while an opener or the dial is pending).
fn session_connector(
    conn: PlcConnection,
    driver: Arc<dyn SessionDriver>,
    settings: Settings,
    secret_keys: Vec<String>,
    timeouts: SessionTimeouts,
    secrets: Arc<dyn SecretOpener>,
) -> Connector {
    let shared = Arc::new((conn, driver, settings, secret_keys, secrets));
    Arc::new(move || {
        let shared = Arc::clone(&shared);
        Box::pin(async move {
            let (conn, driver, settings, secret_keys, secrets) = &*shared;
            let mut guard = OpenedSettings::new(settings.clone());
            for key in secret_keys {
                if matches!(
                    conn.secret_ref_lookup(key),
                    banto_tags::SecretRefLookup::Absent
                ) {
                    continue;
                }
                match secrets.open(conn, key).await {
                    Ok(Some(value)) => guard.insert_opened(key, value.as_str()),
                    Ok(None) => {}
                    Err(reason) => {
                        return Err(FatalError::InvalidConfig(format!(
                            "秘密値 {key} を開けません: {reason}"
                        )));
                    }
                }
            }
            let cx = ConnectContext::new(
                format!("conn:{}", conn.id),
                timeouts.connect,
                timeouts.response,
            );
            driver.connect(guard.settings(), cx).await
        })
    })
}

/// A connect attempt's copy of the settings with the opened secrets in it.
/// On drop (however the attempt ends, including the future being
/// cancelled) the values of the opened keys are overwritten before the
/// copy is freed.
struct OpenedSettings {
    settings: Settings,
    opened: Vec<String>,
}

impl OpenedSettings {
    fn new(settings: Settings) -> Self {
        OpenedSettings {
            settings,
            opened: Vec::new(),
        }
    }

    /// Put an opened secret under `key` (it is zeroized on drop).
    fn insert_opened(&mut self, key: &str, value: &str) {
        // Recorded before the insert: the value is covered from the moment
        // it is in the copy.
        self.opened.push(key.to_string());
        self.settings.insert(key, value);
    }

    fn settings(&self) -> &Settings {
        &self.settings
    }
}

impl Drop for OpenedSettings {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        if self.opened.is_empty() {
            return;
        }
        let items: Vec<banto_driver_api::Setting> = std::mem::take(&mut self.settings).into();
        for mut item in items {
            if self.opened.contains(&item.key) {
                #[cfg(test)]
                zeroized_for_test::record(&item.value);
                item.value.zeroize();
            }
        }
    }
}

/// Test-only record of the secret values [`OpenedSettings`] zeroized, so a
/// test can see the guard ran on a path where the future was dropped. Tests
/// use values unique to them, so parallel tests do not see each other.
#[cfg(test)]
mod zeroized_for_test {
    use std::sync::Mutex;

    static ZEROIZED: Mutex<Vec<String>> = Mutex::new(Vec::new());

    pub(crate) fn record(value: &str) {
        ZEROIZED.lock().unwrap().push(value.to_string());
    }

    /// Whether `value` has been zeroized by a guard.
    pub(crate) fn contains(value: &str) -> bool {
        ZEROIZED.lock().unwrap().iter().any(|v| v == value)
    }
}

/// Spawns and owns one broker task per [`PlcConnection`] it is given, and
/// hands out [`BrokerHandle`]s keyed by connection id. The handle/task
/// bookkeeping lives in a [`SessionDirectory`] (shared, so a secondary
/// caller can add on-demand sessions that this supervisor still shuts down -
/// see that struct's doc).
pub struct BrokerSupervisor {
    directory: SessionDirectory,
    /// Out-of-band shutdown trigger shared with every task via a cloned
    /// [`watch::Receiver`] - see [`Self::shutdown`]. Independent of the job
    /// mpsc: a task must stop even while a [`BrokerHandle`] (and therefore a
    /// live `Sender`) is still held by some caller. `Arc`-shared with the
    /// directory so on-demand tasks subscribe to the same trigger.
    shutdown_tx: Arc<watch::Sender<bool>>,
}

impl BrokerSupervisor {
    /// [`Self::spawn_with`] with `backoff` and the default timeouts and
    /// secret opener ([`BrokerOptions::default`]).
    pub fn spawn(
        connections: &[PlcConnection],
        backoff: BackoffConfig,
        registry: Arc<DriverRegistry>,
    ) -> Result<Self, BrokerError> {
        Self::spawn_with(
            connections,
            registry,
            BrokerOptions {
                backoff,
                ..BrokerOptions::default()
            },
        )
    }

    /// Spawn one broker task per connection in `connections`, dialing through
    /// `registry` (R2b-3a-2: its Session / Frame entries; see
    /// [`is_broker_managed`]). The first connection that cannot be dialed
    /// ([`BrokerError::UnsupportedProtocol`] /
    /// [`BrokerError::InvalidSettings`]) aborts the whole call. Tasks already
    /// spawned for earlier connections in the slice are simply dropped in
    /// that case: nothing has been handed out yet (`Self` is only returned on
    /// full success), so the directory's Arcs - including the shutdown
    /// `Sender` - drop with it, every task's `shutdown_rx.changed()` resolves
    /// `Err`, and they exit immediately.
    pub fn spawn_with(
        connections: &[PlcConnection],
        registry: Arc<DriverRegistry>,
        options: BrokerOptions,
    ) -> Result<Self, BrokerError> {
        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        let shutdown_tx = Arc::new(shutdown_tx);
        let directory = SessionDirectory::new(registry, options, shutdown_tx.clone());

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

    /// A clone of the shared session directory (see [`SessionDirectory`]).
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
    /// This does *not* rely on every [`BrokerHandle`] having been dropped: a
    /// realistic caller (a long-lived poller/writer) holds a handle for the
    /// whole app lifetime, and the `watch` signal is out-of-band from the job
    /// mpsc, so it stops the task regardless. On-demand tasks the
    /// [`SessionDirectory`] added after spawn share the same tasks map, so
    /// they are awaited here exactly like the seeded ones. A connection
    /// [`SessionDirectory::remove`]d before shutdown is no longer in the map,
    /// but its task still receives the same `shutdown_tx` signal and exits on
    /// its own.
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
/// [`Connector`] - the protocol-agnostic core
/// [`SessionDirectory::ensure_connection_with_settings`] calls with the
/// connector it built from the registry's driver (and this crate's unit
/// tests with a `fake` connector). `fingerprint` starts `None`; the caller
/// fills it. `shutdown_rx` is the task's clone of the supervisor-wide
/// shutdown trigger (see [`BrokerSupervisor::shutdown`]); a caller that does
/// not need that path can pass a receiver from a `watch::channel` it never
/// signals.
#[cfg(test)]
fn spawn_task_with_connector(
    connection_id: i64,
    connector: Connector,
    backoff: BackoffConfig,
    timeouts: SessionTimeouts,
    shutdown_rx: watch::Receiver<bool>,
) -> (BrokerHandle, TaskEntry) {
    spawn_task_with_connector_limit(
        connection_id,
        connector,
        backoff,
        timeouts,
        timeouts.connect.saturating_add(CONNECT_SLACK),
        shutdown_rx,
    )
}

fn spawn_task_with_connector_limit(
    connection_id: i64,
    connector: Connector,
    backoff: BackoffConfig,
    timeouts: SessionTimeouts,
    connect_limit: Duration,
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
        timeouts,
        connect_limit,
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
            fingerprint: None,
        },
        TaskEntry {
            stop_tx,
            join_handle: task,
        },
    )
}

/// Connection lifecycle state within one broker task. Structurally identical
/// to `banto_collect::task::ConnState` (see the module doc).
enum ConnState {
    /// Waiting until `at` to spawn the next connect attempt.
    Backoff { at: Instant },
    /// A connect attempt is running in a spawned sub-task, so a slow
    /// `connect()` cannot stall the job loop.
    Connecting(JoinHandle<Result<Box<dyn Session>, FatalError>>),
    /// Connected and owning the live session - the only state in which a
    /// [`Job`] is actually serviced.
    Connected(Box<dyn Session>),
}

/// What woke the connection side of the task's `select!`.
enum ConnEvent {
    Due,
    Finished(Result<Box<dyn Session>, FatalError>),
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
/// section), bounded at `connect_timeout + CONNECT_SLACK` (design §5.5): a
/// driver that does not keep its `ConnectContext::connect_timeout` fails the
/// attempt as [`FatalError::Timeout`] and the backoff retries, instead of
/// leaving the task in `Connecting` for ever.
#[cfg(test)]
async fn connect_attempt(
    connector: Connector,
    timeouts: SessionTimeouts,
) -> Result<Box<dyn Session>, FatalError> {
    connect_attempt_with_limit(connector, timeouts.connect.saturating_add(CONNECT_SLACK)).await
}

async fn connect_attempt_with_limit(
    connector: Connector,
    limit: Duration,
) -> Result<Box<dyn Session>, FatalError> {
    tokio::time::timeout(limit, connector())
        .await
        .unwrap_or(Err(FatalError::Timeout))
}

/// Await one job's session future under `limit` (design §5.5) and check the
/// result count: `expected` results or the job is
/// [`FatalError::Protocol`]. On [`FatalError::Timeout`] the future has been
/// dropped by the time this returns, and the caller drops the session.
async fn bounded_job<T>(
    limit: Duration,
    expected: usize,
    fut: banto_driver_api::BoxFuture<'_, Result<Vec<T>, FatalError>>,
) -> Result<Vec<T>, FatalError> {
    let results = tokio::time::timeout(limit, fut)
        .await
        .unwrap_or(Err(FatalError::Timeout))?;
    if results.len() != expected {
        return Err(FatalError::Protocol(format!(
            "{expected} 件の要求に {} 件の結果が返りました",
            results.len()
        )));
    }
    Ok(results)
}

/// One connection's broker loop: owns this connection's session end-to-end
/// (connect via `connector`, reconnect-with-backoff, disconnect on shutdown)
/// and services [`Job`]s serialized through `rx`, one at a time and each to
/// completion (or to its outer bound, design §5.5). Protocol-agnostic by
/// construction - every read/write/disconnect call here goes through
/// `banto_driver_api::Session`. Exits on either of two independent signals:
/// every [`BrokerHandle`] for this connection has been dropped (`rx.recv()`
/// returns `None`), or the supervisor-wide shutdown / the per-connection
/// stop trigger fires. `status_tx` is sent to at every `ConnState`
/// transition (see the module doc's "Connection status observability"
/// section) - purely observational, it influences no control flow here.
#[allow(clippy::too_many_arguments)]
async fn run_broker_task(
    connection_id: i64,
    connector: Connector,
    backoff_cfg: BackoffConfig,
    timeouts: SessionTimeouts,
    connect_limit: Duration,
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
            // sending (Err case): both mean "stop".
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
                        let handle = tokio::spawn(connect_attempt_with_limit(connector.clone(), connect_limit));
                        state = ConnState::Connecting(handle);
                    }
                    ConnEvent::Finished(Ok(session)) => {
                        attempt = 0;
                        state = ConnState::Connected(session);
                        let _ = status_tx.send(BrokerConnectionStatus::Connected);
                    }
                    ConnEvent::Finished(Err(_)) | ConnEvent::JoinError => {
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

                // Two-step match (compute the outcome, then decide `state`)
                // mirrors banto-collect's task.rs: it keeps the borrow of
                // `state` from ending mid-await from ever overlapping with the
                // reassignment below.
                let fatal = match job {
                    Job::Read { specs, respond_to } => {
                        let outcome = match &mut state {
                            ConnState::Connected(session) => Some(
                                bounded_job(
                                    read_job_limit(specs.len(), timeouts.response),
                                    specs.len(),
                                    session.read_batch(&specs),
                                )
                                .await,
                            ),
                            _ => None,
                        };
                        answer(connection_id, respond_to, outcome)
                    }
                    Job::Write { items, respond_to } => {
                        let outcome = match &mut state {
                            ConnState::Connected(session) => Some(
                                bounded_job(
                                    write_job_limit(items.len(), timeouts.response),
                                    items.len(),
                                    session.write_batch(&items),
                                )
                                .await,
                            ),
                            _ => None,
                        };
                        answer(connection_id, respond_to, outcome)
                    }
                };
                if fatal {
                    state = ConnState::Backoff { at: Instant::now() };
                    attempt = 0;
                    let _ = status_tx.send(BrokerConnectionStatus::Reconnecting { attempt: 1 });
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
            // Design §5.5: stopping must not hang on a session that never
            // finishes its goodbye.
            if tokio::time::timeout(DISCONNECT_LIMIT, session.disconnect())
                .await
                .is_err()
            {
                eprintln!(
                    "banto-broker: 接続 {connection_id} の切断が {} 秒で終わらなかったため打ち切りました",
                    DISCONNECT_LIMIT.as_secs()
                );
            }
        }
    }
    let _ = status_tx.send(BrokerConnectionStatus::Stopped);
}

/// Send a job's `outcome` (`None` = the session was not up) to the caller,
/// mapping a fatal error to [`BrokerError::ConnectionFailed`]. Returns
/// whether it was fatal (the caller drops the session).
fn answer<T>(
    connection_id: i64,
    respond_to: oneshot::Sender<Result<Vec<T>, BrokerError>>,
    outcome: Option<Result<Vec<T>, FatalError>>,
) -> bool {
    match outcome {
        Some(Ok(results)) => {
            let _ = respond_to.send(Ok(results));
            false
        }
        Some(Err(err)) => {
            let _ = respond_to.send(Err(BrokerError::ConnectionFailed {
                connection_id,
                reason: err.to_string(),
            }));
            true
        }
        None => {
            let _ = respond_to.send(Err(BrokerError::Disconnected { connection_id }));
            false
        }
    }
}

#[cfg(test)]
mod tests {
    //! The protocol-independent tests, on [`crate::fake`] - an in-memory
    //! session driver - because this crate does not contain (or depend on) a
    //! real driver. The tests that pin down SLMP / Modbus behaviour through
    //! the broker live in `banto-driver-slmp/tests/broker.rs` and
    //! `banto-driver-modbus/tests/broker.rs`.

    use std::time::Duration;

    use banto_driver_api::{DataType, DriverEntry};
    use futures_util::future::join_all;
    use serde_json::json;
    use zeroize::Zeroizing;

    use super::*;
    use crate::fake::{self, Behaviour, FakeDriver, FakePlc};

    const ID: &str = "fake";

    fn conn_with(id: i64, protocol: &str, settings: serde_json::Value) -> PlcConnection {
        let serde_json::Value::Object(settings) = settings else {
            panic!("settings must be an object");
        };
        PlcConnection::from_settings(
            id,
            format!("conn-{id}"),
            protocol.to_string(),
            true,
            false,
            settings,
            1,
        )
    }

    fn conn(id: i64, port: u16) -> PlcConnection {
        conn_with(id, ID, json!({ "host": "127.0.0.1", "port": port }))
    }

    /// A registry with a fresh fake driver under [`ID`].
    fn fake_registry() -> Arc<DriverRegistry> {
        fake::registry_of(FakeDriver::new(ID, FakePlc::new()))
    }

    fn rspec(canonical: &str, data_type: DataType) -> TagSpec {
        fake::spec(ID, canonical, data_type)
    }

    fn witem(canonical: &str, value: f64) -> WriteItem {
        WriteItem::new(rspec(canonical, DataType::U16), Value::F64(value))
    }

    fn spawn_task(
        connector: Connector,
        backoff: BackoffConfig,
        timeouts: SessionTimeouts,
    ) -> (BrokerHandle, TaskEntry, watch::Sender<bool>) {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (handle, task) =
            spawn_task_with_connector(1, connector, backoff, timeouts, shutdown_rx);
        (handle, task, shutdown_tx)
    }

    /// Poll `handle.read` until the session comes up or a generous deadline
    /// elapses, so tests don't race the connect task that starts in the
    /// background the instant a broker is spawned.
    async fn read_once_connected(
        handle: &BrokerHandle,
        specs: Vec<TagSpec>,
    ) -> Vec<Result<Value, ItemError>> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            match handle.read(specs.clone()).await {
                Ok(results) => return results,
                Err(BrokerError::Disconnected { .. }) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                Err(e) => panic!("unexpected broker error while waiting to connect: {e}"),
            }
        }
    }

    async fn wait_connected(handle: &BrokerHandle) {
        handle
            .status_watch()
            .wait_for(|s| *s == BrokerConnectionStatus::Connected)
            .await
            .expect("the session should come up");
    }

    // -----------------------------------------------------------------
    // Backoff ladder (pure, virtual time)
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
    // P3-b: word_order parsing (shared by both driver crates)
    // -----------------------------------------------------------------

    #[test]
    fn parse_word_order_maps_both_allowed_strings() {
        assert_eq!(parse_word_order("low_high"), WordOrder::LowHigh);
        assert_eq!(parse_word_order("high_low"), WordOrder::HighLow);
    }

    #[test]
    fn parse_word_order_falls_back_to_low_high_for_anything_unrecognized() {
        assert_eq!(parse_word_order("middle_endian"), WordOrder::LowHigh);
        assert_eq!(parse_word_order(""), WordOrder::LowHigh);
    }

    // -----------------------------------------------------------------
    // The registry gate (R2b-3a-2, design §4.2)
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn supervisor_rejects_an_unsupported_protocol_connection() {
        let connections = [conn_with(
            1,
            "bogus-protocol",
            json!({ "host": "127.0.0.1", "port": 502 }),
        )];
        let err = match BrokerSupervisor::spawn(
            &connections,
            BackoffConfig::default(),
            fake_registry(),
        ) {
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

    /// A Session entry is broker-managed: a host-provided protocol
    /// (the host keeps its I/O) is refused like an unregistered one, and
    /// `is_broker_managed` answers the same.
    #[tokio::test]
    async fn only_a_session_type_entry_is_broker_managed() {
        let mut registry = DriverRegistry::new();
        registry
            .register(DriverEntry::session(Arc::new(FakeDriver::new(
                ID,
                FakePlc::new(),
            ))))
            .unwrap();
        registry
            .register(DriverEntry::host_provided(Arc::new(FakeDriver::new(
                "hosted",
                FakePlc::new(),
            ))))
            .unwrap();
        assert!(is_broker_managed(&registry, ID));
        for other in ["hosted", "bogus", "", "FAKE"] {
            assert!(!is_broker_managed(&registry, other), "{other:?}");
        }
        let hosted = conn_with(2, "hosted", json!({ "host": "h", "port": 1 }));
        let err = match BrokerSupervisor::spawn(
            &[hosted],
            BackoffConfig::default(),
            Arc::new(registry),
        ) {
            Ok(_) => panic!("a host-provided protocol must be rejected"),
            Err(e) => e,
        };
        assert!(
            matches!(
                err,
                BrokerError::UnsupportedProtocol {
                    connection_id: 2,
                    ..
                }
            ),
            "{err:?}"
        );
    }

    /// The driver's `validate_config` is the gate the broker's own `u16` port
    /// check used to be: refused settings never spawn a task.
    #[tokio::test]
    async fn settings_the_driver_refuses_are_rejected_before_any_task() {
        let supervisor =
            BrokerSupervisor::spawn(&[], BackoffConfig::default(), fake_registry()).unwrap();
        let directory = supervisor.directory();
        let bad = conn_with(3, ID, json!({ "host": "127.0.0.1", "port": 70000 }));
        let err = directory.ensure_connection(&bad).unwrap_err();
        match err {
            BrokerError::InvalidSettings {
                connection_id,
                reason,
            } => {
                assert_eq!(connection_id, 3);
                assert!(reason.contains("port"), "{reason}");
            }
            other => panic!("expected InvalidSettings, got {other:?}"),
        }
        assert_eq!(directory.connection_count(), 0);
        supervisor.shutdown().await;
    }

    /// The dial gets the connection's settings and the broker's timeouts in
    /// its `ConnectContext` (the defaults are the old `banto_plc` config
    /// defaults: connect 3 s, response 1 s).
    #[tokio::test]
    async fn the_dial_uses_the_connections_settings_and_the_default_timeouts() {
        let driver = FakeDriver::new(ID, FakePlc::new());
        let seen = driver.seen();
        let supervisor = BrokerSupervisor::spawn(
            &[conn_with(
                1,
                ID,
                json!({ "host": "10.0.0.9", "port": 5007, "word_order": "high_low" }),
            )],
            BackoffConfig::default(),
            fake::registry_of(driver),
        )
        .unwrap();
        wait_connected(&supervisor.handle(1).unwrap()).await;
        let first = seen.lock().unwrap()[0].clone();
        assert_eq!(first.settings.get("host"), Some("10.0.0.9"));
        assert_eq!(first.settings.get("port"), Some("5007"));
        assert_eq!(first.settings.get("word_order"), Some("high_low"));
        assert_eq!(first.connect_timeout, Duration::from_secs(3));
        assert_eq!(first.response_timeout, Duration::from_secs(1));
        supervisor.shutdown().await;
    }

    // -----------------------------------------------------------------
    // Settings fingerprint (replaces #605's `slmp_cpu`, design §4.2)
    // -----------------------------------------------------------------

    #[test]
    fn the_fingerprint_ignores_order_and_secret_keys_and_sees_every_other_value() {
        let a = Settings::from_pairs([("host", "h"), ("port", "1"), ("cpu_type", "Q")]).unwrap();
        let b = Settings::from_pairs([("cpu_type", "Q"), ("port", "1"), ("host", "h")]).unwrap();
        assert_eq!(
            SettingsFingerprint::of("p", &a, &[]),
            SettingsFingerprint::of("p", &b, &[])
        );
        let r = Settings::from_pairs([("host", "h"), ("port", "1"), ("cpu_type", "R")]).unwrap();
        assert_ne!(
            SettingsFingerprint::of("p", &a, &[]),
            SettingsFingerprint::of("p", &r, &[])
        );

        assert_ne!(
            SettingsFingerprint::of("slmp", &a, &[]),
            SettingsFingerprint::of("modbus-tcp", &a, &[]),
            "the same settings under another driver kind is another identity"
        );

        let secret = vec!["password".to_string()];
        let with_pw =
            Settings::from_pairs([("host", "h"), ("port", "1"), ("password", "x")]).unwrap();
        let other_pw =
            Settings::from_pairs([("host", "h"), ("port", "1"), ("password", "y")]).unwrap();
        let without = Settings::from_pairs([("host", "h"), ("port", "1")]).unwrap();
        assert_eq!(
            SettingsFingerprint::of("p", &with_pw, &secret),
            SettingsFingerprint::of("p", &other_pw, &secret)
        );
        assert_eq!(
            SettingsFingerprint::of("p", &with_pw, &secret),
            SettingsFingerprint::of("p", &without, &secret)
        );
    }

    /// The handle carries the fingerprint of the settings it was dialed
    /// with; `ensure_connection_with_settings` dials (and fingerprints) the
    /// given settings, not the connection's own.
    #[tokio::test]
    async fn the_handle_carries_the_fingerprint_of_the_settings_it_dials() {
        let driver = FakeDriver::new(ID, FakePlc::new());
        let seen = driver.seen();
        let supervisor =
            BrokerSupervisor::spawn(&[], BackoffConfig::default(), fake::registry_of(driver))
                .unwrap();
        let directory = supervisor.directory();
        let c = conn(1, 5000);
        let own = banto_tags::settings_to_api(&c.settings);
        let handle = directory.ensure_connection(&c).unwrap();
        assert_eq!(
            handle.settings_fingerprint(),
            Some(SettingsFingerprint::of(&c.protocol, &own, &[]))
        );
        assert_eq!(
            handle.read_only().settings_fingerprint(),
            handle.settings_fingerprint()
        );
        wait_connected(&handle).await;

        let simulated = Settings::from_pairs([("host", "127.0.0.1"), ("port", "40000")]).unwrap();
        let other = directory
            .ensure_connection_with_settings(&conn(2, 5000), simulated.clone())
            .unwrap();
        assert_eq!(
            other.settings_fingerprint(),
            Some(SettingsFingerprint::of(
                &conn(2, 5000).protocol,
                &simulated,
                &[]
            ))
        );
        assert_ne!(other.settings_fingerprint(), handle.settings_fingerprint());
        wait_connected(&other).await;
        let ports: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.settings.get("port").unwrap().to_string())
            .collect();
        assert!(ports.contains(&"40000".to_string()), "{ports:?}");
        supervisor.shutdown().await;
    }

    // -----------------------------------------------------------------
    // Secrets (connection-settings design §3.5, design §4.2)
    // -----------------------------------------------------------------

    const REF: &str = "0123456789abcdef0123456789abcdef";

    /// Records every key it is asked to open and answers `pw-<key>`.
    #[derive(Default)]
    struct RecordingOpener {
        asked: std::sync::Mutex<Vec<String>>,
    }

    impl SecretOpener for RecordingOpener {
        fn open(
            &self,
            _conn: &PlcConnection,
            key: &str,
        ) -> banto_driver_api::BoxFuture<'static, Result<Option<Zeroizing<String>>, String>>
        {
            self.asked.lock().unwrap().push(key.to_string());
            let value = format!("pw-{key}");
            Box::pin(async move { Ok(Some(Zeroizing::new(value))) })
        }
    }

    fn secret_conn() -> PlcConnection {
        conn_with(
            1,
            ID,
            json!({
                "host": "127.0.0.1",
                "port": 5000,
                "password": { "$secret": REF },
                "token": { "$secret": REF },
            }),
        )
    }

    /// Only the driver's `secret_keys()` are opened (not every reference in
    /// the settings), the plaintext reaches the dial, and the fingerprint
    /// does not change with it.
    #[tokio::test]
    async fn only_the_drivers_secret_keys_are_opened_and_handed_to_the_dial() {
        let driver = FakeDriver::new(ID, FakePlc::new()).with_secret_keys(&["password"]);
        let seen = driver.seen();
        let opener = Arc::new(RecordingOpener::default());
        let supervisor = BrokerSupervisor::spawn_with(
            &[secret_conn()],
            fake::registry_of(driver),
            BrokerOptions {
                secrets: opener.clone(),
                ..BrokerOptions::default()
            },
        )
        .unwrap();
        let handle = supervisor.handle(1).unwrap();
        wait_connected(&handle).await;

        assert_eq!(*opener.asked.lock().unwrap(), vec!["password".to_string()]);
        let first = seen.lock().unwrap()[0].clone();
        assert_eq!(first.settings.get("password"), Some("pw-password"));
        assert_eq!(
            first.settings.get("token"),
            None,
            "not a secret key of the driver"
        );
        let validated = banto_tags::settings_to_api(&secret_conn().settings);
        assert_eq!(validated.get("password"), None, "validation never sees it");
        assert_eq!(
            handle.settings_fingerprint(),
            Some(SettingsFingerprint::of_connection(
                &secret_conn().protocol,
                Some(&secret_conn()),
                &validated,
                &["password".to_string()]
            ))
        );
        supervisor.shutdown().await;
    }

    const OTHER_REF: &str = "fedcba9876543210fedcba9876543210";

    fn conn_with_password_ref(r#ref: &str) -> PlcConnection {
        conn_with(
            1,
            ID,
            json!({ "host": "127.0.0.1", "port": 5000, "password": { "$secret": r#ref } }),
        )
    }

    /// The fingerprint carries the secret's **reference** (a new secret is
    /// sealed under a new reference): another reference is another
    /// fingerprint, the same reference the same one, and a key the driver
    /// does not list as secret does not count.
    #[test]
    fn the_fingerprint_carries_the_secret_reference_not_the_value() {
        let keys = vec!["password".to_string()];
        let a = conn_with_password_ref(REF);
        let b = conn_with_password_ref(OTHER_REF);
        let settings = banto_tags::settings_to_api(&a.settings);
        assert_eq!(settings, banto_tags::settings_to_api(&b.settings));
        let fa = SettingsFingerprint::of_connection(ID, Some(&a), &settings, &keys);
        let fb = SettingsFingerprint::of_connection(ID, Some(&b), &settings, &keys);
        assert_ne!(fa, fb, "another reference, another fingerprint");
        assert_eq!(
            fa,
            SettingsFingerprint::of_connection(
                ID,
                Some(&conn_with_password_ref(REF)),
                &settings,
                &keys
            )
        );
        assert_ne!(
            fa,
            SettingsFingerprint::of(ID, &settings, &keys),
            "the reference counts"
        );
        // Not a secret key of the driver: the reference is not looked at.
        assert_eq!(
            SettingsFingerprint::of_connection(ID, Some(&a), &settings, &[]),
            SettingsFingerprint::of_connection(ID, Some(&b), &settings, &[])
        );
    }

    /// End to end: a session dialed for reference A and one for reference B
    /// carry different fingerprints (so the Hub re-dials when the secret is
    /// replaced), while the opened value - whatever the opener answers - never
    /// changes the fingerprint.
    #[tokio::test]
    async fn a_new_secret_reference_is_a_new_session_identity_the_value_is_not() {
        struct Fixed(&'static str);
        impl SecretOpener for Fixed {
            fn open(
                &self,
                _conn: &PlcConnection,
                _key: &str,
            ) -> banto_driver_api::BoxFuture<'static, Result<Option<Zeroizing<String>>, String>>
            {
                let value = self.0;
                Box::pin(async move { Ok(Some(Zeroizing::new(value.to_string()))) })
            }
        }
        let mut seen = Vec::new();
        for (r#ref, value) in [(REF, "pw-1"), (REF, "pw-2"), (OTHER_REF, "pw-1")] {
            let driver = FakeDriver::new(ID, FakePlc::new()).with_secret_keys(&["password"]);
            let opened = driver.seen();
            let supervisor = BrokerSupervisor::spawn_with(
                &[conn_with_password_ref(r#ref)],
                fake::registry_of(driver),
                BrokerOptions {
                    secrets: Arc::new(Fixed(value)),
                    ..BrokerOptions::default()
                },
            )
            .unwrap();
            let handle = supervisor.handle(1).unwrap();
            wait_connected(&handle).await;
            assert_eq!(
                opened.lock().unwrap()[0].settings.get("password"),
                Some(value),
                "the dial got the opened value"
            );
            seen.push(handle.settings_fingerprint().unwrap());
            supervisor.shutdown().await;
        }
        assert_eq!(
            seen[0], seen[1],
            "the opened value is not in the fingerprint"
        );
        assert_ne!(seen[0], seen[2], "a new reference is a new identity");
    }

    /// Without an opener the dial fails (never connects without the secret):
    /// the driver's `connect` is not even called.
    #[tokio::test]
    async fn a_secret_that_cannot_be_opened_fails_the_dial() {
        let driver = FakeDriver::new(ID, FakePlc::new()).with_secret_keys(&["password"]);
        let seen = driver.seen();
        let supervisor = BrokerSupervisor::spawn(
            &[secret_conn()],
            BackoffConfig::default(),
            fake::registry_of(driver),
        )
        .unwrap();
        let handle = supervisor.handle(1).unwrap();
        handle
            .status_watch()
            .wait_for(
                |s| matches!(s, BrokerConnectionStatus::Reconnecting { attempt } if *attempt >= 2),
            )
            .await
            .unwrap();
        assert!(
            seen.lock().unwrap().is_empty(),
            "connect must not be called"
        );
        assert_eq!(
            handle.read(vec![rspec("D0", DataType::U16)]).await,
            Err(BrokerError::Disconnected { connection_id: 1 })
        );
        supervisor.shutdown().await;
    }

    // -----------------------------------------------------------------
    // Session removal (T7-2 backlog item, 2026-08-05)
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn remove_untracks_a_connection() {
        let supervisor =
            BrokerSupervisor::spawn(&[conn(7, 5000)], BackoffConfig::default(), fake_registry())
                .expect("spawn");
        let directory = supervisor.directory();
        assert_eq!(directory.connection_ids(), vec![7]);

        assert!(directory.remove(7));
        assert_eq!(supervisor.connection_count(), 0);
        assert!(directory.handle(7).is_none());

        supervisor.shutdown().await;
    }

    #[tokio::test]
    async fn remove_is_false_and_idempotent_for_an_untracked_connection() {
        let directory = SessionDirectory::new(
            fake_registry(),
            BrokerOptions::default(),
            Arc::new(watch::channel(false).0),
        );
        assert!(!directory.remove(42), "nothing was ever tracked for id 42");
        assert!(!directory.remove(42), "removing twice is still just false");
    }

    #[tokio::test]
    async fn ensure_connection_after_remove_spawns_a_fresh_session() {
        let supervisor =
            BrokerSupervisor::spawn(&[conn(7, 5000)], BackoffConfig::default(), fake_registry())
                .expect("spawn");
        let directory = supervisor.directory();

        directory.remove(7);
        assert_eq!(directory.connection_count(), 0);

        let fresh = directory
            .ensure_connection(&conn(7, 5000))
            .expect("re-ensuring a removed connection should spawn a fresh session");
        assert_eq!(directory.connection_count(), 1);
        let _ = read_once_connected(&fresh, vec![rspec("D0", DataType::U16)]).await;

        supervisor.shutdown().await;
    }

    // -----------------------------------------------------------------
    // Many requests over one shared session
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn serialized_concurrent_requests_do_not_corrupt_each_other() {
        let supervisor =
            BrokerSupervisor::spawn(&[conn(1, 5000)], BackoffConfig::default(), fake_registry())
                .expect("spawn");
        let handle = supervisor.handle(1).expect("handle");
        let _ = read_once_connected(&handle, vec![rspec("D0", DataType::U16)]).await;

        const N: u32 = 20;
        let writes = (0..N).map(|i| {
            let handle = handle.clone();
            async move {
                let addr = format!("D{}", 300 + i);
                handle
                    .write(vec![witem(&addr, 1000.0 + i as f64)])
                    .await
                    .unwrap_or_else(|e| panic!("write {addr} failed: {e}"))
            }
        });
        let write_outcomes = join_all(writes).await;
        assert!(
            write_outcomes.iter().all(|r| r == &vec![Ok(())]),
            "every concurrent write should succeed: {write_outcomes:?}"
        );

        let reads = (0..N).map(|i| {
            let handle = handle.clone();
            async move {
                let addr = format!("D{}", 300 + i);
                let result = handle
                    .read(vec![rspec(&addr, DataType::U16)])
                    .await
                    .unwrap_or_else(|e| panic!("read {addr} failed: {e}"));
                (i, result)
            }
        });
        for (i, result) in join_all(reads).await {
            assert_eq!(
                result,
                vec![Ok(Value::F64(1000.0 + i as f64))],
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
        let plc = FakePlc::new();
        plc.refuse_connections(true);
        let (handle, task, _shutdown) = spawn_task(
            plc.connector(),
            BackoffConfig::default(),
            SessionTimeouts::default(),
        );

        let outcome = tokio::time::timeout(
            Duration::from_millis(200),
            handle.read(vec![rspec("D0", DataType::U16)]),
        )
        .await
        .expect("must fail fast, not hang until the next connect attempt");
        assert_eq!(outcome, Err(BrokerError::Disconnected { connection_id: 1 }));

        drop(handle);
        let _ = task.join_handle.await;
    }

    #[tokio::test]
    async fn fatal_error_triggers_reconnect_and_later_requests_succeed() {
        let plc = FakePlc::new();
        let backoff = BackoffConfig {
            base: Duration::from_millis(20),
            cap: Duration::from_millis(100),
            factor: 2,
        };
        let (handle, task, _shutdown) =
            spawn_task(plc.connector(), backoff, SessionTimeouts::default());

        let _ = read_once_connected(&handle, vec![rspec("D0", DataType::U16)]).await;
        plc.fail_next_call();

        let hung = handle.read(vec![rspec("D0", DataType::U16)]).await;
        assert!(
            matches!(
                hung,
                Err(BrokerError::ConnectionFailed {
                    connection_id: 1,
                    ..
                })
            ),
            "a connection-fatal error mid-request should fail that request as connection-fatal: {hung:?}"
        );

        plc.set_word("D400", 55);
        let recovered = read_once_connected(&handle, vec![rspec("D400", DataType::U16)]).await;
        assert_eq!(recovered, vec![Ok(Value::F64(55.0))]);

        drop(handle);
        let _ = task.join_handle.await;
    }

    /// A session that answers a different number of results than it was
    /// asked breaks the contract: the job fails as connection-fatal (never
    /// trimmed or padded) and the session is re-dialed.
    #[tokio::test]
    async fn a_result_count_mismatch_is_connection_fatal() {
        let plc = FakePlc::new();
        let (handle, task, _shutdown) = spawn_task(
            plc.connector(),
            BackoffConfig {
                base: Duration::from_millis(10),
                cap: Duration::from_millis(10),
                factor: 1,
            },
            SessionTimeouts::default(),
        );
        let _ = read_once_connected(&handle, vec![rspec("D0", DataType::U16)]).await;
        plc.set_behaviour(Behaviour::ShortCount);
        let got = handle
            .read(vec![rspec("D0", DataType::U16), rspec("D1", DataType::U16)])
            .await;
        match got {
            Err(BrokerError::ConnectionFailed { reason, .. }) => {
                assert!(reason.contains("2 件の要求に 1 件"), "{reason}")
            }
            other => panic!("expected ConnectionFailed, got {other:?}"),
        }
        let got = handle.write(vec![witem("D0", 1.0), witem("D1", 2.0)]).await;
        assert!(
            matches!(
                got,
                Err(BrokerError::ConnectionFailed { .. }) | Err(BrokerError::Disconnected { .. })
            ),
            "{got:?}"
        );
        drop(handle);
        let _ = task.join_handle.await;
    }

    // -----------------------------------------------------------------
    // Bounds on every session future (design §5.5)
    // -----------------------------------------------------------------

    const T: Duration = Duration::from_millis(1000);

    fn quick_backoff() -> BackoffConfig {
        BackoffConfig {
            base: Duration::from_millis(10),
            cap: Duration::from_millis(10),
            factor: 1,
        }
    }

    #[test]
    fn the_job_limits_are_the_step_limit_times_two_t_plus_one_second() {
        let t = Duration::from_millis(500);
        // read: (n + 8) × 2T + 1 s
        assert_eq!(read_job_limit(0, t), Duration::from_secs(8 + 1));
        assert_eq!(read_job_limit(2, t), Duration::from_secs(10 + 1));
        // write: (3n + 8) × 2T + 1 s
        assert_eq!(write_job_limit(1, t), Duration::from_secs(11 + 1));
        assert_eq!(write_job_limit(4, t), Duration::from_secs(20 + 1));
        // Item counts clamp to u32 steps, without wrapping; huge durations saturate.
        let max_steps = Duration::from_secs(u64::from(u32::MAX) + 1);
        assert_eq!(read_job_limit(usize::MAX, t), max_steps);
        assert_eq!(write_job_limit(usize::MAX, t), max_steps);
        assert_eq!(read_job_limit(1, Duration::MAX), Duration::MAX);
        assert_eq!(write_job_limit(1, Duration::MAX), Duration::MAX);
        assert_eq!(
            read_job_limit(usize::MAX, Duration::ZERO),
            Duration::from_secs(1)
        );
        assert_eq!(
            write_job_limit(usize::MAX, Duration::ZERO),
            Duration::from_secs(1)
        );
    }

    /// A driver that keeps its deadlines - each step `0.95 T` sending and
    /// `0.95 T` receiving, for as many steps as contract 7 allows - is never
    /// cut by the outer bound: the write runs to `Done`. (It takes longer than
    /// `steps × T`, so a bound of `T` per step would have cut it.)
    #[tokio::test(start_paused = true)]
    async fn a_driver_keeping_its_deadlines_is_never_cut_by_the_outer_bound() {
        let plc = FakePlc::new();
        let timeouts = SessionTimeouts {
            connect: Duration::from_secs(3),
            response: T,
        };
        let (handle, task, _shutdown) = spawn_task(plc.connector(), quick_backoff(), timeouts);
        wait_connected(&handle).await;

        let items = vec![witem("D0", 1.0), witem("D1", 2.0)];
        let steps = 3 * items.len() + 8;
        plc.set_behaviour(Behaviour::Steps {
            steps,
            phase: T.mul_f64(0.95),
            deadline: T,
        });
        let started = Instant::now();
        let got = handle.write(items).await;
        let took = started.elapsed();
        assert_eq!(got, Ok(vec![Ok(()), Ok(())]));
        assert!(took > T * steps as u32, "took {took:?}");
        assert!(took < write_job_limit(2, T), "took {took:?}");
        assert_eq!(
            *handle.status_watch().borrow(),
            BrokerConnectionStatus::Connected
        );
        drop(handle);
        let _ = task.join_handle.await;
    }

    /// The same driver with a step of `1.05 T`: its own deadline fires (the
    /// inner one, after the first phase), long before the outer bound.
    #[tokio::test(start_paused = true)]
    async fn a_step_over_t_is_cut_by_the_drivers_own_deadline_not_the_outer_bound() {
        let plc = FakePlc::new();
        let timeouts = SessionTimeouts {
            connect: Duration::from_secs(3),
            response: T,
        };
        let (handle, task, _shutdown) = spawn_task(plc.connector(), quick_backoff(), timeouts);
        wait_connected(&handle).await;

        plc.set_behaviour(Behaviour::Steps {
            steps: 3 + 8,
            phase: T.mul_f64(1.05),
            deadline: T,
        });
        let started = Instant::now();
        let got = handle.write(vec![witem("D0", 1.0)]).await;
        let took = started.elapsed();
        assert_eq!(
            got,
            Err(BrokerError::ConnectionFailed {
                connection_id: 1,
                reason: FatalError::Timeout.to_string(),
            })
        );
        assert!(took < T * 2, "the inner deadline cuts it: took {took:?}");
        drop(handle);
        let _ = task.join_handle.await;
    }

    /// A session-type driver that ignores `response_timeout` and never
    /// answers: the broker's job still returns, at the outer bound, as a
    /// timeout; the session is dropped and re-dialed (design §5.5 / the
    /// conformance item of 3a-2).
    #[tokio::test(start_paused = true)]
    async fn a_driver_ignoring_its_response_timeout_still_returns_at_the_outer_bound() {
        let plc = FakePlc::new();
        let timeouts = SessionTimeouts {
            connect: Duration::from_secs(3),
            response: T,
        };
        let (handle, task, _shutdown) = spawn_task(plc.connector(), quick_backoff(), timeouts);
        wait_connected(&handle).await;

        plc.set_behaviour(Behaviour::Hang);
        let specs = vec![rspec("D0", DataType::U16), rspec("D1", DataType::U16)];
        let started = Instant::now();
        let got = handle.read(specs).await;
        let took = started.elapsed();
        assert_eq!(
            got,
            Err(BrokerError::ConnectionFailed {
                connection_id: 1,
                reason: FatalError::Timeout.to_string(),
            })
        );
        assert_eq!(took, read_job_limit(2, T));

        // The same for a write.
        plc.set_behaviour(Behaviour::Prompt);
        wait_connected(&handle).await;
        plc.set_behaviour(Behaviour::Hang);
        let started = Instant::now();
        let got = handle.write(vec![witem("D0", 1.0)]).await;
        assert!(
            matches!(got, Err(BrokerError::ConnectionFailed { .. })),
            "{got:?}"
        );
        assert_eq!(started.elapsed(), write_job_limit(1, T));

        // The session was dropped and re-dialed: it answers again.
        plc.set_behaviour(Behaviour::Prompt);
        wait_connected(&handle).await;
        assert_eq!(
            handle.read(vec![rspec("D0", DataType::U16)]).await,
            Ok(vec![Ok(Value::F64(0.0))])
        );
        drop(handle);
        let _ = task.join_handle.await;
    }

    /// A connect that never finishes is given up at `connect_timeout + 1 s`
    /// and retried (the task does not sit in `Connecting` for ever).
    #[tokio::test(start_paused = true)]
    async fn a_connect_that_never_finishes_is_given_up_and_retried() {
        let (connector, mut started_rx, mut dropped_rx) = fake::hanging_connector();
        let timeouts = SessionTimeouts {
            connect: Duration::from_secs(3),
            response: T,
        };
        let (handle, task, _shutdown) = spawn_task(connector, quick_backoff(), timeouts);
        started_rx.recv().await.expect("first attempt");
        let started = Instant::now();
        dropped_rx
            .recv()
            .await
            .expect("the first attempt is given up");
        assert_eq!(started.elapsed(), Duration::from_secs(3) + CONNECT_SLACK);
        started_rx.recv().await.expect("and retried");
        drop(handle);
        let _ = task.stop_tx.send(true);
        let _ = task.join_handle.await;
    }

    /// Stopping a connection whose session never finishes `disconnect` is
    /// bounded at [`DISCONNECT_LIMIT`].
    #[tokio::test(start_paused = true)]
    async fn stopping_does_not_hang_on_a_disconnect_that_never_finishes() {
        let plc = FakePlc::new();
        plc.hang_disconnect(true);
        let (handle, task, _shutdown) =
            spawn_task(plc.connector(), quick_backoff(), SessionTimeouts::default());
        wait_connected(&handle).await;
        let started = Instant::now();
        let _ = task.stop_tx.send(true);
        task.join_handle.await.expect("the task ends");
        assert_eq!(started.elapsed(), DISCONNECT_LIMIT);
        assert_eq!(
            *handle.status_watch().borrow(),
            BrokerConnectionStatus::Stopped
        );
    }

    // -----------------------------------------------------------------
    // Connection status observability (I6 T2-1 addition)
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn status_watch_observes_connected_then_reconnecting_after_stop() {
        let plc = FakePlc::new();
        let (handle, task, _shutdown) = spawn_task(
            plc.connector(),
            BackoffConfig::default(),
            SessionTimeouts::default(),
        );
        let mut status = handle.status_watch();

        let _ = read_once_connected(&handle, vec![rspec("D0", DataType::U16)]).await;
        status
            .wait_for(|s| *s == BrokerConnectionStatus::Connected)
            .await
            .expect("status watch should report Connected once the session is up");

        plc.refuse_connections(true);
        plc.fail_next_call();

        let _ = handle.read(vec![rspec("D0", DataType::U16)]).await;
        status
            .wait_for(|s| matches!(s, BrokerConnectionStatus::Reconnecting { .. }))
            .await
            .expect("status watch should report Reconnecting after the session drops");

        drop(handle);
        let _ = task.join_handle.await;
    }

    #[tokio::test]
    async fn status_watch_observes_stopped_on_shutdown() {
        let supervisor =
            BrokerSupervisor::spawn(&[conn(1, 5000)], BackoffConfig::default(), fake_registry())
                .expect("spawn");
        let handle = supervisor.handle(1).expect("handle");
        let mut status = handle.status_watch();

        let _ = read_once_connected(&handle, vec![rspec("D0", DataType::U16)]).await;
        status
            .wait_for(|s| *s == BrokerConnectionStatus::Connected)
            .await
            .expect("status watch should report Connected once the session is up");

        drop(handle);
        supervisor.shutdown().await;

        assert_eq!(*status.borrow(), BrokerConnectionStatus::Stopped);
    }

    #[tokio::test]
    async fn session_directory_stop_and_join_stops_only_the_requested_task() {
        // Port 0: the fake driver refuses to connect, so both tasks sit in
        // backoff.
        let supervisor = BrokerSupervisor::spawn(
            &[conn(1, 0), conn(2, 0)],
            BackoffConfig::default(),
            fake_registry(),
        )
        .expect("spawn");
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
        let (connector, mut started_rx, mut dropped_rx) = fake::hanging_connector();

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let directory = SessionDirectory::new(
            fake_registry(),
            BrokerOptions::default(),
            Arc::new(shutdown_tx),
        );
        let (handle, task) = spawn_task_with_connector(
            9,
            connector,
            BackoffConfig::default(),
            SessionTimeouts::default(),
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

        tokio::time::timeout(Duration::from_secs(1), started_rx.recv())
            .await
            .expect("connect attempt should start")
            .expect("connector should stay alive");
        let stopped = tokio::time::timeout(Duration::from_secs(1), directory.stop_and_join(9))
            .await
            .expect("stop_and_join should not wait for the connect attempt");
        assert!(stopped, "connection should be tracked");
        tokio::time::timeout(Duration::from_secs(1), dropped_rx.recv())
            .await
            .expect("aborting the connect attempt should drop it")
            .expect("connector should stay alive");

        drop(handle);
    }

    // -----------------------------------------------------------------
    // Opened secrets are zeroized when the attempt is cancelled (#788 P2)
    // -----------------------------------------------------------------

    /// Answers `values[key]`, or never answers for a key it has no value for.
    struct OpenerByKey(Vec<(&'static str, &'static str)>);

    impl SecretOpener for OpenerByKey {
        fn open(
            &self,
            _conn: &PlcConnection,
            key: &str,
        ) -> banto_driver_api::BoxFuture<'static, Result<Option<Zeroizing<String>>, String>>
        {
            let value = self.0.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
            Box::pin(async move {
                match value {
                    Some(v) => Ok(Some(Zeroizing::new(v.to_string()))),
                    None => std::future::pending().await,
                }
            })
        }
    }

    fn short_timeouts() -> SessionTimeouts {
        SessionTimeouts {
            connect: Duration::from_millis(20),
            response: Duration::from_millis(20),
        }
    }

    /// The secret is opened and handed to a dial that never returns: the
    /// `connect_attempt` bound drops the future, and the guard zeroizes the
    /// opened value on that path too.
    #[tokio::test(start_paused = true)]
    async fn an_opened_secret_is_zeroized_when_the_dial_times_out() {
        const VALUE: &str = "pw-zeroize-on-connect-timeout";
        let driver = FakeDriver::new(ID, FakePlc::new())
            .with_secret_keys(&["password"])
            .with_hanging_connect();
        let seen = driver.seen();
        let connector = session_connector(
            conn_with_password_ref(REF),
            Arc::new(driver),
            banto_tags::settings_to_api(&conn_with_password_ref(REF).settings),
            vec!["password".to_string()],
            short_timeouts(),
            Arc::new(OpenerByKey(vec![("password", VALUE)])),
        );
        let result = connect_attempt(connector, short_timeouts()).await;
        assert!(matches!(result, Err(FatalError::Timeout)));
        assert_eq!(
            seen.lock().unwrap()[0].settings.get("password"),
            Some(VALUE),
            "the dial got the opened value"
        );
        assert!(
            zeroized_for_test::contains(VALUE),
            "the cancelled attempt must zeroize the opened value"
        );
    }

    /// The first secret is opened, the second opener never answers: when the
    /// attempt is given up the first value - already in the copy - is
    /// zeroized (the guard owns the copy from before the first insert).
    #[tokio::test(start_paused = true)]
    async fn an_opened_secret_is_zeroized_when_a_later_opener_never_answers() {
        const VALUE: &str = "pw-zeroize-on-second-opener-hang";
        let conn = conn_with(
            1,
            ID,
            json!({
                "host": "127.0.0.1",
                "port": 5000,
                "password": { "$secret": REF },
                "token": { "$secret": OTHER_REF },
            }),
        );
        let driver = FakeDriver::new(ID, FakePlc::new()).with_secret_keys(&["password", "token"]);
        let seen = driver.seen();
        let connector = session_connector(
            conn.clone(),
            Arc::new(driver),
            banto_tags::settings_to_api(&conn.settings),
            vec!["password".to_string(), "token".to_string()],
            short_timeouts(),
            Arc::new(OpenerByKey(vec![("password", VALUE)])),
        );
        let result = connect_attempt(connector, short_timeouts()).await;
        assert!(matches!(result, Err(FatalError::Timeout)));
        assert!(seen.lock().unwrap().is_empty(), "never dialed");
        assert!(
            zeroized_for_test::contains(VALUE),
            "the value opened before the hang must be zeroized"
        );
    }

    /// The task is stopped while the dial (with the opened secret) is
    /// pending: the aborted attempt's guard zeroizes the value.
    #[tokio::test]
    async fn an_opened_secret_is_zeroized_when_the_task_stops_mid_dial() {
        const VALUE: &str = "pw-zeroize-on-task-stop";
        let driver = FakeDriver::new(ID, FakePlc::new())
            .with_secret_keys(&["password"])
            .with_hanging_connect();
        let seen = driver.seen();
        let supervisor = BrokerSupervisor::spawn_with(
            &[conn_with_password_ref(REF)],
            fake::registry_of(driver),
            BrokerOptions {
                secrets: Arc::new(OpenerByKey(vec![("password", VALUE)])),
                ..BrokerOptions::default()
            },
        )
        .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while seen.lock().unwrap().is_empty() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the dial should start");
        assert!(!zeroized_for_test::contains(VALUE), "still dialing");
        tokio::time::timeout(Duration::from_secs(1), supervisor.shutdown())
            .await
            .expect("shutdown must not wait for the dial");
        tokio::time::timeout(Duration::from_secs(1), async {
            while !zeroized_for_test::contains(VALUE) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the aborted attempt must zeroize the opened value");
    }
}
