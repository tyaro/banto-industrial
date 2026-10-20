//! An in-memory fake driver for this crate's own unit tests (R1-4a,
//! docs/banto-driver-api-design.md §4.4).
//!
//! The real drivers live in `banto-driver-slmp` / `banto-driver-modbus`,
//! which depend on this crate - so this crate cannot use them, not even as a
//! dev-dependency (cycles and double compilation, §2.3). The tests that
//! remain here are about the protocol-independent machinery (the job loop,
//! reconnect/backoff, status, the session directory, stop/abort), and they
//! only need *a* [`BrokerSession`] they can steer:
//!
//! - [`FakePlc`] is the "device": an in-memory word table shared by every
//!   session dialed from it, so a value written on one session (or seeded
//!   with [`FakePlc::set_word`]) reads back on the next one, across
//!   reconnects. Two hooks stand in for what the old tests did to a
//!   simulator: [`FakePlc::refuse_connections`] (a closed port / a stopped
//!   simulator) and [`FakePlc::fail_next_call`] (a hung or severed session:
//!   the next `read_batch`/`write_batch` returns the connection-fatal `Err`).
//! - [`dial`] is a [`crate::DialFn`] for table-driven tests. A `DialFn` is a
//!   plain `fn`, so it cannot capture a test's `FakePlc`; each call makes a
//!   fresh one and the returned [`Connector`]'s closure owns it. Port `0`
//!   refuses to connect, mirroring the old tests' use of a port nothing
//!   listens on.
//! - [`hanging_connector`] never finishes connecting, and reports when an
//!   attempt starts and when that attempt's future is dropped (aborted).
//!
//! Only plain numeric reads/writes (`BatchReadRequest::Numeric`,
//! `BatchWriteRequest::Numeric`) are supported; strings and bit-in-word are
//! protocol behaviour and are tested in the driver crates.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use banto_plc::{
    Address, BatchReadRequest, BatchReadResult, BoxFuture, DataType, PlcValue, TagValue,
};
use banto_plc_write::{BatchWriteRequest, WriteResult};
use banto_tags::PlcConnection;
use tokio::sync::mpsc;

use crate::{BrokerSession, Connector, DriverDial, SessionError};

#[derive(Default)]
struct State {
    words: HashMap<Address, u16>,
    refuse_connect: bool,
    fail_next_call: bool,
}

/// The fake device - see the module doc. Cheap to clone (`Arc`); every clone
/// and every session dialed from it share one state.
#[derive(Clone, Default)]
pub(crate) struct FakePlc {
    state: Arc<Mutex<State>>,
}

impl FakePlc {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("fake plc poisoned")
    }

    /// Seed one word (SLMP notation, e.g. `"D400"`).
    pub(crate) fn set_word(&self, raw: &str, value: u16) {
        let address = Address::parse_slmp(raw).unwrap_or_else(|e| panic!("{raw}: {e}"));
        self.lock().words.insert(address, value);
    }

    /// While `true`, every connect attempt fails (the old "closed port" /
    /// `sim.stop()`).
    pub(crate) fn refuse_connections(&self, refuse: bool) {
        self.lock().refuse_connect = refuse;
    }

    /// The next `read_batch`/`write_batch` on any session of this device
    /// returns the connection-fatal `Err` (the old `sim.hang()` response
    /// timeout / `sim.stop()` severed socket). One-shot.
    pub(crate) fn fail_next_call(&self) {
        self.lock().fail_next_call = true;
    }

    /// A [`Connector`] dialing this device.
    pub(crate) fn connector(&self) -> Connector {
        let plc = self.clone();
        Arc::new(move || {
            let plc = plc.clone();
            Box::pin(async move {
                if plc.lock().refuse_connect {
                    return Err("fake: connection refused".to_string());
                }
                Ok(Box::new(FakeSession { plc }) as Box<dyn BrokerSession>)
            })
        })
    }

    /// `Some(Err)` if a fatal call was armed (and disarms it).
    fn take_fatal(&self) -> Option<SessionError> {
        let mut state = self.lock();
        std::mem::take(&mut state.fail_next_call)
            .then(|| SessionError("fake: injected connection-fatal error".to_string()))
    }
}

/// One live fake session over a [`FakePlc`].
struct FakeSession {
    plc: FakePlc,
}

impl BrokerSession for FakeSession {
    fn read_batch<'a>(
        &'a mut self,
        requests: &'a [BatchReadRequest],
    ) -> BoxFuture<'a, Result<Vec<BatchReadResult>, SessionError>> {
        Box::pin(async move {
            // An await point inside the call, like a real wire round trip,
            // so the job loop's "one call at a time" is actually exercised.
            tokio::task::yield_now().await;
            if let Some(err) = self.plc.take_fatal() {
                return Err(err);
            }
            let state = self.plc.lock();
            Ok(requests
                .iter()
                .map(|request| match request {
                    BatchReadRequest::Numeric(r) => {
                        let word = state.words.get(&r.address).copied().unwrap_or(0);
                        BatchReadResult::Value(match r.data_type {
                            DataType::Bit => PlcValue::Bit(word != 0),
                            _ => PlcValue::F64(f64::from(word)),
                        })
                    }
                    BatchReadRequest::String(_) => {
                        panic!("the fake session supports numeric reads only")
                    }
                })
                .collect())
        })
    }

    fn write_batch<'a>(
        &'a mut self,
        requests: &'a [BatchWriteRequest],
    ) -> BoxFuture<'a, Result<Vec<WriteResult>, SessionError>> {
        Box::pin(async move {
            tokio::task::yield_now().await;
            if let Some(err) = self.plc.take_fatal() {
                return Err(err);
            }
            let mut state = self.plc.lock();
            Ok(requests
                .iter()
                .map(|request| match request {
                    BatchWriteRequest::Numeric(w) => {
                        let word = match w.value {
                            TagValue::Bit(b) => u16::from(b),
                            TagValue::F64(v) => v as u16,
                        };
                        state.words.insert(w.address, word);
                        WriteResult::Ok
                    }
                    _ => panic!("the fake session supports numeric writes only"),
                })
                .collect())
        })
    }

    fn disconnect(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

/// [`crate::DialFn`] over a fresh [`FakePlc`] per call (see the module doc).
/// Port `0` refuses to connect.
pub(crate) fn dial(_conn: &PlcConnection, port: u16) -> DriverDial {
    let plc = FakePlc::new();
    plc.refuse_connections(port == 0);
    DriverDial {
        connector: plc.connector(),
        slmp_cpu: None,
    }
}

/// Sends `()` on drop - lets a test observe that a connect attempt's future
/// was dropped (aborted), like the old test observed the socket closing.
struct DropSignal(mpsc::UnboundedSender<()>);

impl Drop for DropSignal {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

/// A [`Connector`] whose every attempt never completes. Returns the
/// connector, a receiver that gets `()` when an attempt starts, and one that
/// gets `()` when an attempt's future is dropped.
pub(crate) fn hanging_connector() -> (
    Connector,
    mpsc::UnboundedReceiver<()>,
    mpsc::UnboundedReceiver<()>,
) {
    let (started_tx, started_rx) = mpsc::unbounded_channel();
    let (dropped_tx, dropped_rx) = mpsc::unbounded_channel();
    let connector: Connector = Arc::new(move || {
        let started_tx = started_tx.clone();
        let guard = DropSignal(dropped_tx.clone());
        Box::pin(async move {
            let _guard = guard;
            let _ = started_tx.send(());
            std::future::pending().await
        })
    });
    (connector, started_rx, dropped_rx)
}
