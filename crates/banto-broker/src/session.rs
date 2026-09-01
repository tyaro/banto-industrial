//! [`BrokerSession`]: the protocol abstraction seam this broker's job loop
//! and reconnect state machine (`lib.rs`'s [`crate::ConnState`] /
//! [`crate::run_broker_task`]) are written against, so that neither one
//! contains a single protocol-specific name (Issue #130, I9). See
//! `lib.rs`'s module doc, section "Protocol abstraction (I9 / Issue #130)",
//! for the full history and why this is a *new* trait rather than a reuse of
//! `banto_plc::PlcClient`/`banto_plc_write::PlcWriteClient`.
//!
//! ## Why one trait with both `read_batch` and `write_batch`
//!
//! [`BrokerHandle::read`][crate::BrokerHandle::read] and
//! [`BrokerHandle::write`][crate::BrokerHandle::write] both end up calling
//! methods on the *same* `&mut Box<dyn BrokerSession>` stored in
//! [`crate::ConnState::Connected`] - never two separate trait objects. That is
//! deliberate and is this crate's whole reason for existing: a caller reusing
//! `banto_plc::PlcClient` for reads and `banto_plc_write::PlcWriteClient` for
//! writes would get two independent instances (each of `SlmpClient` and
//! `SlmpWriteClient` owns its own private `Option<slmp::SLMPClient>`, i.e. its
//! own socket), which reopens exactly the "read and write can interleave on
//! the wire, or fight over one PLC session slot" problem this crate's
//! extraction was meant to close for good. A single trait whose methods share
//! `&mut self` makes "one session serves both directions" a type-level fact
//! rather than a convention a future driver author could get wrong.
//!
//! ## The `Err` contract (Issue #130 D3)
//!
//! **`Err` from either method must mean the connection itself is no longer
//! usable** - a broken socket, a timeout, a framing error, anything that
//! makes every future call on this session untrustworthy. A per-request or
//! per-device failure (an out-of-range device address, a device-side end
//! code/exception, a type mismatch) must instead be folded into the
//! `Ok(Vec<_>)` result as that request's own `Bad`/error entry, at the same
//! index as its request, alongside every other request's outcome.
//!
//! This matters because [`crate::run_broker_task`] trusts it completely: on
//! `Err` it drops the whole session and falls back into
//! [`crate::ConnState::Backoff`], failing the in-flight request with
//! [`crate::BrokerError::ConnectionFailed`] - see `lib.rs`'s module doc,
//! "Reconnect / backoff policy" and "The `Err` contract every `BrokerSession`
//! implementation must uphold". [`slmp_driver::SlmpSession`][crate::slmp_driver::SlmpSession]
//! gets this for free because `banto_plc::execute_slmp_batch_reads` and
//! `banto_plc_write::execute_slmp_writes` already enforce the split
//! internally (a device-side SLMP end code becomes a per-request `Bad`; only
//! a connection-fatal condition surfaces as their own `Err`). A future
//! Modbus TCP driver (Issue #131) will **not** get this for free - a Modbus
//! exception response (illegal data address, illegal function, ...) is a
//! per-request failure, not a connection failure, and that driver's
//! `read_batch`/`write_batch` must classify it into `Ok(Vec<_>)` itself.
//! Getting this wrong would make the broker tear down and reconnect a
//! perfectly healthy session every time a caller asks for one bad address -
//! exactly the failure mode this doc comment exists to prevent.
//!
//! ## `dyn`-compatibility
//!
//! Same technique as `banto_plc::PlcClient` and
//! `banto_plc_write::PlcWriteClient` (see the latter's module doc, "Same
//! `dyn`-compatibility technique as `PlcClient`"): hand-boxed futures
//! ([`banto_plc::BoxFuture`], reused rather than redefined) so this trait
//! stays object-safe and [`crate::ConnState::Connected`] can hold a plain
//! `Box<dyn BrokerSession>` regardless of which driver produced it.

use banto_plc::{BatchReadRequest, BatchReadResult, BoxFuture};
use banto_plc_write::{BatchWriteRequest, WriteResult};

/// A live PLC session, owning one connection-level resource (for
/// [`crate::slmp_driver::SlmpSession`], one `slmp::SLMPClient` socket) and
/// exposing both read and write over it. See this module's doc comment for
/// the full contract, especially the `Err` = connection-fatal rule every
/// implementation must uphold.
pub(crate) trait BrokerSession: Send {
    /// Execute one read batch against this session. `Ok` carries exactly
    /// `requests.len()` results in `requests` order, each either a decoded
    /// value or a per-request `Bad` outcome - see this module's doc comment
    /// for what may and may not become `Err` instead.
    fn read_batch<'a>(
        &'a mut self,
        requests: &'a [BatchReadRequest],
    ) -> BoxFuture<'a, Result<Vec<BatchReadResult>, SessionError>>;

    /// Execute one write batch against this session. Same per-request-vs-`Err`
    /// contract as [`Self::read_batch`].
    fn write_batch<'a>(
        &'a mut self,
        requests: &'a [BatchWriteRequest],
    ) -> BoxFuture<'a, Result<Vec<WriteResult>, SessionError>>;

    /// Close this session's underlying connection gracefully. Called once,
    /// from [`crate::run_broker_task`]'s shutdown path, on whatever session
    /// happened to be live when the task was asked to stop - never called
    /// concurrently with (or after) a `read_batch`/`write_batch` call.
    fn disconnect(&mut self) -> BoxFuture<'_, ()>;
}

/// The error [`BrokerSession::read_batch`]/[`BrokerSession::write_batch`]
/// return on a connection-fatal failure - see this module's doc comment for
/// the contract. Carries only a display message (the underlying
/// `PlcError`/`PlcWriteError`/driver-specific error's own `to_string()`)
/// because that is all [`crate::run_broker_task`] ever does with it -
/// `BrokerError::ConnectionFailed { reason, .. }` - so there is nothing to
/// gain from preserving a richer, protocol-specific error type this far up
/// the stack.
#[derive(Debug, Clone)]
pub(crate) struct SessionError(pub(crate) String);

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for SessionError {}

/// A reusable "dial a fresh session" step for one broker task, boxed so
/// [`crate::run_broker_task`] can hold and repeatedly call it (on every
/// reconnect attempt) without knowing which driver or config type produced
/// it. `Arc` (not `Box`) because [`crate::ConnEvent::Due`]'s handling clones
/// it into a spawned sub-task on every attempt - see
/// [`crate::connect_attempt`] - while the original stays owned by the task
/// loop for the next attempt after that. `Fn` (not `FnOnce`) for the same
/// reason: a failed attempt must be retryable, not consumed.
///
/// Each driver builds its own `Connector` from its own config type (e.g.
/// [`crate::slmp_driver::connector`] from a `SlmpConfig`) - the closure is
/// what erases that type difference, so [`crate::spawn_task`]'s generic
/// pieces (`ConnState`, `run_broker_task`) never need to know it.
pub(crate) type Connector = std::sync::Arc<
    dyn Fn() -> BoxFuture<'static, Result<Box<dyn BrokerSession>, String>> + Send + Sync,
>;
