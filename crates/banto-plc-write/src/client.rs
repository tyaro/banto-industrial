//! [`PlcWriteClient`]: the write counterpart to `banto_plc::PlcClient`, kept a
//! deliberately separate trait in a deliberately separate crate.
//!
//! ## Why a separate trait/crate rather than a `write` method on `PlcClient`
//!
//! `banto_plc::PlcClient`'s own module doc fixes this as invariant #1, quoted
//! here because it is the entire reason I5 is shaped the way it is:
//!
//! > **read-only** - there is no `write`/`write_batch` method, on purpose; PLC
//! > writes are out of scope for the recorder product ... and, if a future
//! > recipe-download app needs them, that is a deliberately separate
//! > trait/crate rather than a footgun sitting unused on every read-only
//! > consumer
//!
//! So the write surface lives here. The read-only consumers (ChronoGazer,
//! banto-collect) link `banto-plc` and never see a write method; the one app
//! that writes (relay-wright, W1-W5) links this crate too and gets
//! [`PlcWriteClient`] as an additional, opt-in capability. Nothing that only
//! reads can accidentally call a write.
//!
//! ## Same `dyn`-compatibility technique as `PlcClient`
//!
//! This trait returns hand-boxed futures ([`banto_plc::BoxFuture`], reused
//! rather than redefined) for exactly the reason `client.rs` explains for the
//! read trait: an app may hold a `Vec<Box<dyn PlcWriteClient>>` across a mix of
//! protocols/connections, which native `async fn` in traits cannot express
//! (not object-safe). The verbosity of `Box::pin(async move { ... })` at each
//! impl site buys `dyn`-compatibility without a proc-macro dependency.
//!
//! ## Same NotConnected-after-fatal contract
//!
//! Like [`banto_plc::PlcClient`], an implementation is not required to be
//! internally reconnect-safe: after any connection-level failure (see
//! [`crate::error::PlcWriteError::is_connection_fatal`]) it drops its socket and
//! every subsequent call fails with [`crate::error::PlcWriteError::NotConnected`]
//! until [`PlcWriteClient::connect`] is called again. The reconnect loop is the
//! caller's job (in relay-wright, the W3 broker's).

use banto_plc::BoxFuture;

use crate::error::PlcWriteError;
use crate::types::{WriteRequest, WriteResult};

/// Write-capable PLC client, one instance per configured connection. Mirrors
/// [`banto_plc::PlcClient`] method-for-method with `write` in place of `read`.
pub trait PlcWriteClient: Send {
    /// Establish the connection. Calling this again while already connected is
    /// implementation-defined (the [`crate::SlmpWriteClient`] reconnects,
    /// replacing the old socket) - callers that care should track their own
    /// connected state, matching the read trait.
    fn connect(&mut self) -> BoxFuture<'_, Result<(), PlcWriteError>>;

    /// Write every request in one logical operation. Returns
    /// `Ok(Vec<WriteResult>)` with exactly `requests.len()` entries in the same
    /// order as `requests` (callers may zip the two slices), each entry
    /// [`WriteResult::Ok`] or a per-request [`WriteResult::Bad`]. Reserves
    /// `Err` for connection-level failures that make the whole call
    /// untrustworthy - see this module's doc comment and
    /// [`crate::error::PlcWriteError::is_connection_fatal`] for exactly where
    /// that line falls.
    ///
    /// ## Safety note that has no read-side equivalent
    ///
    /// A write has a real side effect on the PLC, so this method's planner
    /// ([`crate::slmp::planning::plan_slmp_writes`]) never coalesces
    /// non-adjacent requests the way the read planner does: over-*reading* a
    /// few extra registers is harmless, but writing a device the caller did not
    /// ask to write is not. See that module for the gap-tolerance-zero rule.
    fn write_batch<'a>(
        &'a mut self,
        requests: &'a [WriteRequest],
    ) -> BoxFuture<'a, Result<Vec<WriteResult>, PlcWriteError>>;

    /// Close the connection, if any. Never fails, same reasoning as the read
    /// trait: nothing useful can be done about a failed disconnect of a
    /// connection about to be discarded.
    fn disconnect(&mut self) -> BoxFuture<'_, ()>;
}
