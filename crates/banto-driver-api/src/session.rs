//! Session-type drivers: the driver owns its I/O
//! (docs/banto-driver-api-design.md §5).
//!
//! For protocols whose client library does framing and I/O together (OPC UA,
//! a database, the `push` receiver of external-db-design §9.5), the driver
//! connects by itself and returns a [`Session`]. Since R2b-3a-2 the broker
//! (`banto-broker`) runs every connection on one: **`Err` means the
//! connection is unusable** (the host drops it and reconnects with backoff),
//! and `Ok` carries exactly one result per request, in request order, with
//! per-item failures as [`ItemError`].
//!
//! The API has no tokio dependency: futures are hand-boxed [`BoxFuture`]s
//! (the same definition as `banto_plc::BoxFuture`), and the host runs
//! [`SessionDriver::connect`] on its own runtime, so an implementation may
//! call `tokio::spawn` directly.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use crate::driver::DriverBase;
use crate::error::{FatalError, ItemError};
use crate::settings::Settings;
use crate::types::{TagSpec, Value, WriteItem};

/// A boxed, `Send` future - what keeps [`SessionDriver`] and [`Session`]
/// object-safe. The same definition as `banto_plc::BoxFuture`.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What the host tells a driver when connecting. No runtime handle (tokio
/// is not part of the API).
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct ConnectContext {
    /// The host's key for this connection (for logs).
    pub connection_key: String,
    /// How long establishing the connection may take.
    pub connect_timeout: Duration,
    /// How long one request may wait for its response.
    pub response_timeout: Duration,
}

impl ConnectContext {
    /// A context for `connection_key` with the two timeouts.
    pub fn new(
        connection_key: impl Into<String>,
        connect_timeout: Duration,
        response_timeout: Duration,
    ) -> Self {
        ConnectContext {
            connection_key: connection_key.into(),
            connect_timeout,
            response_timeout,
        }
    }
}

/// A session-type driver (design §5).
pub trait SessionDriver: DriverBase {
    /// Connect and return the live session. Failure is fatal (the host backs
    /// off and retries). `'static` for the same reason as
    /// `banto_broker::Connector`: the attempt runs on its own task.
    fn connect(
        &self,
        settings: &Settings,
        cx: ConnectContext,
    ) -> BoxFuture<'static, Result<Box<dyn Session>, FatalError>>;
}

/// One live connection (what `banto-broker`'s job loop runs since R2b-3a-2).
/// Owned by one host task, which serializes reads and writes (`&mut self`).
pub trait Session: Send + 'static {
    /// Reacquire device evidence on the existing physical session before a
    /// new diagnostic attempt. Old drivers have no optional diagnostics.
    fn refresh_diagnostic<'a>(&'a mut self) -> BoxFuture<'a, Result<(), FatalError>> {
        Box::pin(async { Ok(()) })
    }

    /// Optional read-only diagnostic on the same connection as normal reads.
    /// `None` means the driver has no diagnostic contract, not an unlimited
    /// range or successful validation.
    fn diagnose_batch<'a>(
        &'a mut self,
        _specs: &'a [TagSpec],
    ) -> BoxFuture<'a, Result<Option<crate::DiagnosticReport>, FatalError>> {
        Box::pin(async { Ok(None) })
    }

    /// Apply a diagnostic only after its caller has accepted the complete
    /// report. A timed-out or superseded diagnostic must have no read effect.
    fn accept_diagnostic(&mut self, _specs: &[TagSpec], _report: &crate::DiagnosticReport) {}

    /// Drop a previously accepted diagnostic when publication lost its
    /// catalog/driver generation race.
    fn discard_diagnostic(&mut self) {}

    /// Read `specs`. `Ok` has `specs.len()` results in `specs` order; `Err`
    /// is connection-fatal.
    fn read_batch<'a>(
        &'a mut self,
        specs: &'a [TagSpec],
    ) -> BoxFuture<'a, Result<Vec<Result<Value, ItemError>>, FatalError>>;

    /// Write `items`, with the same contract as
    /// [`read_batch`](Self::read_batch). The default refuses with
    /// [`FatalError::Protocol`] (a read-only driver).
    fn write_batch<'a>(
        &'a mut self,
        _items: &'a [WriteItem],
    ) -> BoxFuture<'a, Result<Vec<Result<(), ItemError>>, FatalError>> {
        Box::pin(async {
            Err(FatalError::Protocol(
                "このドライバは書き込みに対応していません".into(),
            ))
        })
    }

    /// Close the connection gracefully. Called once, when the host task
    /// stops; never concurrently with a read or write.
    fn disconnect(&mut self) -> BoxFuture<'_, ()>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::{Context, Poll, Waker};

    /// Polls a future that never actually waits (std only; no runtime).
    fn block_on_ready<T>(mut fut: BoxFuture<'_, T>) -> T {
        let mut cx = Context::from_waker(Waker::noop());
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(v) => v,
            Poll::Pending => panic!("future was not ready"),
        }
    }

    struct ReadOnly;

    impl Session for ReadOnly {
        fn read_batch<'a>(
            &'a mut self,
            specs: &'a [TagSpec],
        ) -> BoxFuture<'a, Result<Vec<Result<Value, ItemError>>, FatalError>> {
            Box::pin(async move { Ok(specs.iter().map(|_| Ok(Value::Bit(true))).collect()) })
        }
        fn disconnect(&mut self) -> BoxFuture<'_, ()> {
            Box::pin(async {})
        }
    }

    #[test]
    fn connect_context_new() {
        let cx = ConnectContext::new("c1", Duration::from_secs(3), Duration::from_millis(500));
        assert_eq!(cx.connection_key, "c1");
        assert_eq!(cx.connect_timeout, Duration::from_secs(3));
        assert_eq!(cx.response_timeout, Duration::from_millis(500));
    }

    #[test]
    fn default_write_batch_refuses_and_the_trait_is_object_safe() {
        let mut s: Box<dyn Session> = Box::new(ReadOnly);
        assert!(matches!(
            block_on_ready(s.write_batch(&[])),
            Err(FatalError::Protocol(_))
        ));
        assert_eq!(block_on_ready(s.read_batch(&[])), Ok(Vec::new()));
        block_on_ready(s.disconnect());
    }
}
