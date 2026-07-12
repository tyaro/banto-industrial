//! [`PlcClient`]: the protocol-agnostic boundary I3 (収集エンジン) codes
//! against. docs/plan.md I2 fixes exactly three invariants for every
//! implementation, present and future (Modbus TCP today, MELSEC MC/SLMP
//! next):
//!
//! 1. **read-only** - there is no `write`/`write_batch` method, on purpose;
//!    PLC writes are out of scope for the recorder product
//!    (docs/recorder-requirements.md §7) and, if a future recipe-download
//!    app needs them, that is a deliberately separate trait/crate rather
//!    than a footgun sitting unused on every read-only consumer
//! 2. **bulk read** - one `read_batch` call per collection group per poll
//!    cycle (recorder-requirements.md §3.1: "収集周期は...収集グループ毎"),
//!    not one round trip per tag
//! 3. **individual errors don't kill the batch** - `read_batch` returns
//!    `Ok(Vec<ReadResult>)` with per-request `Bad` entries for anything that
//!    is wrong about one specific address, and reserves `Err` for failures
//!    that make the *whole* response untrustworthy (see `error.rs`'s module
//!    doc and `modbus/mod.rs`'s for exactly where that line falls for
//!    Modbus TCP)
//!
//! ## Why this trait is hand-written to be `dyn`-compatible
//!
//! Rust's native `async fn` in traits (stable since 1.75) is the more
//! ergonomic way to write this, but it is not object-safe: a trait using it
//! cannot be the `T` in `Box<dyn T>`. I3 (per docs/plan.md, "PLC プロトコル
//! クライアント...共通 trait + Modbus TCP 先行 → MC/SLMP 続行") will hold up
//! to 4 concurrently-polled PLC connections, potentially a mix of protocols
//! once MC/SLMP lands, each driven by its own task - a `Vec<Box<dyn
//! PlcClient>>` (or similar) is the natural shape for that, not a
//! per-protocol enum that I3 would have to keep widening. So this trait
//! returns hand-boxed futures (the same technique the `async-trait` crate's
//! macro expands to) instead of using `async fn` directly, trading a little
//! implementation-site verbosity (`Box::pin(async move { ... })`) for
//! `dyn`-compatibility today, without adding a proc-macro dependency for
//! what is, for now, a 3-method trait.
use std::future::Future;
use std::pin::Pin;

use crate::error::PlcError;
use crate::types::{ReadRequest, ReadResult};

/// A future boxed for `dyn` compatibility, matching what `async-trait`
/// generates by hand. `'a` ties the future's lifetime to `&'a mut self`
/// (and, for `read_batch`, to the borrowed `requests` slice) so
/// implementations can freely borrow from `self`/`requests` inside their
/// `async move` block.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Read-only PLC client, one instance per [`crate`]-configured connection
/// (`banto-tags::PlcConnection`, from I3's perspective). Implementations are
/// not required to be internally reconnect-safe: docs/plan.md I2 §2 is
/// explicit that "再接続ループは持たない（I3の責務）" - after any
/// connection-level failure (see `error.rs`), the implementation drops its
/// socket and every subsequent call fails with [`PlcError::NotConnected`]
/// until the caller calls [`PlcClient::connect`] again.
pub trait PlcClient: Send {
    /// Establish the connection. Calling this again while already connected
    /// is implementation-defined (the [`crate::modbus::ModbusTcpClient`]
    /// simply reconnects, replacing the old socket) - callers that care
    /// should track their own connected/disconnected state rather than rely
    /// on `connect` to reject a redundant call.
    fn connect(&mut self) -> BoxFuture<'_, Result<(), PlcError>>;

    /// Read every request in one logical operation. See this module's doc
    /// comment for the `Ok(Vec<ReadResult>)`-with-per-item-`Bad` vs. `Err`
    /// split. The returned `Vec` always has exactly `requests.len()`
    /// entries, in the same order as `requests` - callers may zip the two
    /// slices together.
    fn read_batch<'a>(
        &'a mut self,
        requests: &'a [ReadRequest],
    ) -> BoxFuture<'a, Result<Vec<ReadResult>, PlcError>>;

    /// Close the connection, if any. Never fails: there is nothing a caller
    /// can usefully do about a failed disconnect of a connection it is
    /// about to discard anyway, so implementations swallow any I/O error
    /// here rather than surface one.
    fn disconnect(&mut self) -> BoxFuture<'_, ()>;
}
