//! An in-memory fake session driver for this crate's own unit tests (R1-4a;
//! rewritten for the driver API's `Session` by R2b-3a-2,
//! docs/banto-hub-driver-io-design.md §4.2).
//!
//! The real drivers live in `banto-driver-slmp` / `banto-driver-modbus`,
//! which depend on this crate - so this crate cannot use them, not even as a
//! dev-dependency (cycles and double compilation). The tests that remain here
//! are about the protocol-independent machinery (the job loop, the bounds of
//! design §5.5, reconnect/backoff, status, the session directory, the
//! registry gate, secrets, stop/abort), and they only need *a* session they
//! can steer:
//!
//! - [`FakePlc`] is the "device": an in-memory word table keyed by the
//!   canonical address, shared by every session dialed from it, so a value
//!   written on one session (or seeded with [`FakePlc::set_word`]) reads back
//!   on the next one, across reconnects. Hooks: [`FakePlc::refuse_connections`]
//!   (a closed port), [`FakePlc::fail_next_call`] (the next read/write is
//!   connection-fatal), [`FakePlc::set_behaviour`] (slow, hanging or
//!   miscounting sessions for the §5.5 tests).
//! - [`FakeDriver`] is a `SessionDriver` over a [`FakePlc`] for registry-driven
//!   tests: `port = "0"` refuses to connect, `validate_config` refuses a
//!   missing `host` or a `port` that is not a `u16`, it can declare secret
//!   keys, and it records the settings every `connect` saw.
//! - [`hanging_connector`] never finishes connecting, and reports when an
//!   attempt starts and when that attempt's future is dropped (aborted).
//!
//! Only numeric reads/writes are supported; strings and bit-in-word are
//! protocol behaviour and are tested in the driver crates.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use banto_driver_api::{
    AddressError, BoxFuture, Capabilities, ConnectContext, DataType, DriverAddress, DriverBase,
    DriverDescriptor, DriverEntry, DriverId, DriverRegistry, FatalError, FieldError, ItemError,
    Session, SessionDriver, Settings, TagKind, TagSpec, Value, WriteItem,
};
use tokio::sync::mpsc;

use crate::Connector;

/// How the fake's sessions run a read / write (design §5.5 tests).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) enum Behaviour {
    /// Answer at once.
    #[default]
    Prompt,
    /// Run `steps` steps, each "sending" for `phase` and then "receiving" for
    /// `phase`, each phase under its own `deadline` (a driver that keeps its
    /// response timeout the way today's `banto-plc` SLMP session does: one
    /// `T` for the send, one for the receive). A phase longer than the
    /// deadline is the session's own `FatalError::Timeout`.
    Steps {
        steps: usize,
        phase: Duration,
        deadline: Duration,
    },
    /// Never answer (a driver that ignores its response timeout).
    Hang,
    /// Answer one result too few (a broken count).
    ShortCount,
}

#[derive(Default)]
struct State {
    words: HashMap<String, u16>,
    refuse_connect: bool,
    fail_next_call: bool,
    behaviour: Behaviour,
    hang_disconnect: bool,
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

    /// Seed one word at a canonical address (e.g. `"D400"`).
    pub(crate) fn set_word(&self, canonical: &str, value: u16) {
        self.lock().words.insert(canonical.to_string(), value);
    }

    /// While `true`, every connect attempt fails (a closed port).
    pub(crate) fn refuse_connections(&self, refuse: bool) {
        self.lock().refuse_connect = refuse;
    }

    /// The next read/write on any session of this device returns the
    /// connection-fatal `Err` (a hung or severed session). One-shot.
    pub(crate) fn fail_next_call(&self) {
        self.lock().fail_next_call = true;
    }

    /// How reads and writes run from now on.
    pub(crate) fn set_behaviour(&self, behaviour: Behaviour) {
        self.lock().behaviour = behaviour;
    }

    /// While `true`, `disconnect` never finishes.
    pub(crate) fn hang_disconnect(&self, hang: bool) {
        self.lock().hang_disconnect = hang;
    }

    /// A [`Connector`] dialing this device (no registry, no settings).
    pub(crate) fn connector(&self) -> Connector {
        let plc = self.clone();
        Arc::new(move || {
            let plc = plc.clone();
            Box::pin(async move { plc.dial() })
        })
    }

    fn dial(&self) -> Result<Box<dyn Session>, FatalError> {
        if self.lock().refuse_connect {
            return Err(FatalError::Io("fake: connection refused".to_string()));
        }
        Ok(Box::new(FakeSession { plc: self.clone() }))
    }

    /// `Some(Err)` if a fatal call was armed (and disarms it).
    fn take_fatal(&self) -> Option<FatalError> {
        let mut state = self.lock();
        std::mem::take(&mut state.fail_next_call)
            .then(|| FatalError::Io("fake: injected connection-fatal error".to_string()))
    }

    /// Run the configured behaviour; `Err` is fatal. `Ok(short)` says whether
    /// to drop one result.
    async fn run_behaviour(&self) -> Result<bool, FatalError> {
        let behaviour = self.lock().behaviour;
        match behaviour {
            Behaviour::Prompt => Ok(false),
            Behaviour::Steps {
                steps,
                phase,
                deadline,
            } => {
                for _ in 0..steps {
                    for _ in 0..2 {
                        tokio::time::timeout(deadline, tokio::time::sleep(phase))
                            .await
                            .map_err(|_| FatalError::Timeout)?;
                    }
                }
                Ok(false)
            }
            Behaviour::Hang => std::future::pending().await,
            Behaviour::ShortCount => Ok(true),
        }
    }
}

/// One live fake session over a [`FakePlc`].
struct FakeSession {
    plc: FakePlc,
}

impl Session for FakeSession {
    fn read_batch<'a>(
        &'a mut self,
        specs: &'a [TagSpec],
    ) -> BoxFuture<'a, Result<Vec<Result<Value, ItemError>>, FatalError>> {
        Box::pin(async move {
            // An await point inside the call, like a real wire round trip,
            // so the job loop's "one call at a time" is actually exercised.
            tokio::task::yield_now().await;
            if let Some(err) = self.plc.take_fatal() {
                return Err(err);
            }
            let short = self.plc.run_behaviour().await?;
            let state = self.plc.lock();
            let mut results: Vec<Result<Value, ItemError>> = specs
                .iter()
                .map(|spec| {
                    let word = state
                        .words
                        .get(&spec.address.canonical)
                        .copied()
                        .unwrap_or(0);
                    Ok(match spec.kind {
                        TagKind::Numeric(DataType::Bit) => Value::Bit(word != 0),
                        TagKind::Numeric(_) => Value::F64(f64::from(word)),
                        _ => panic!("the fake session supports numeric reads only"),
                    })
                })
                .collect();
            if short {
                results.pop();
            }
            Ok(results)
        })
    }

    fn write_batch<'a>(
        &'a mut self,
        items: &'a [WriteItem],
    ) -> BoxFuture<'a, Result<Vec<Result<(), ItemError>>, FatalError>> {
        Box::pin(async move {
            tokio::task::yield_now().await;
            if let Some(err) = self.plc.take_fatal() {
                return Err(err);
            }
            let short = self.plc.run_behaviour().await?;
            let mut state = self.plc.lock();
            let mut results: Vec<Result<(), ItemError>> = items
                .iter()
                .map(|item| {
                    let word = match item.value {
                        Value::Bit(b) => u16::from(b),
                        Value::F64(v) => v as u16,
                        Value::Str(_) => panic!("the fake session supports numeric writes only"),
                    };
                    state
                        .words
                        .insert(item.spec.address.canonical.clone(), word);
                    Ok(())
                })
                .collect();
            if short {
                results.pop();
            }
            Ok(results)
        })
    }

    fn disconnect(&mut self) -> BoxFuture<'_, ()> {
        let hang = self.plc.lock().hang_disconnect;
        Box::pin(async move {
            if hang {
                std::future::pending::<()>().await;
            }
        })
    }
}

/// The settings one `FakeDriver::connect` saw, and its context's timeouts.
#[derive(Debug, Clone)]
pub(crate) struct SeenConnect {
    pub(crate) settings: Settings,
    pub(crate) connect_timeout: Duration,
    pub(crate) response_timeout: Duration,
}

/// A `SessionDriver` over a [`FakePlc`] (see the module doc).
pub(crate) struct FakeDriver {
    descriptor: DriverDescriptor,
    plc: FakePlc,
    secret_keys: Vec<String>,
    seen: Arc<Mutex<Vec<SeenConnect>>>,
}

impl FakeDriver {
    pub(crate) fn new(id: &str, plc: FakePlc) -> Self {
        FakeDriver {
            descriptor: DriverDescriptor::new(
                DriverId::new(id).expect("valid driver id"),
                "0.0.0-test",
                id,
                Capabilities::READ_WRITE,
            ),
            plc,
            secret_keys: Vec::new(),
            seen: Arc::default(),
        }
    }

    /// The same driver declaring `keys` as secret.
    pub(crate) fn with_secret_keys(mut self, keys: &[&str]) -> Self {
        self.secret_keys = keys.iter().map(|k| k.to_string()).collect();
        self
    }

    /// What every `connect` so far saw (shared with the driver).
    pub(crate) fn seen(&self) -> Arc<Mutex<Vec<SeenConnect>>> {
        Arc::clone(&self.seen)
    }
}

impl DriverBase for FakeDriver {
    fn descriptor(&self) -> &DriverDescriptor {
        &self.descriptor
    }

    fn validate_config(&self, settings: &Settings) -> Vec<FieldError> {
        let mut errors = Vec::new();
        if settings.get("host").is_none_or(|h| h.trim().is_empty()) {
            errors.push(FieldError::new("host", "host がありません"));
        }
        if let Some(port) = settings.get("port") {
            if port.parse::<u16>().is_err() {
                errors.push(FieldError::new("port", "0〜65535 の数でありません"));
            }
        }
        errors
    }

    fn secret_keys(&self) -> Vec<String> {
        self.secret_keys.clone()
    }

    fn parse_address(&self, text: &str, kind: &TagKind) -> Result<DriverAddress, AddressError> {
        Ok(DriverAddress::new(
            self.descriptor.id.clone(),
            text.trim(),
            "word",
            true,
            kind.word_count(),
        ))
    }

    fn offset_address(
        &self,
        canonical: &str,
        _kind: &TagKind,
        _n: i64,
    ) -> Result<String, AddressError> {
        Ok(canonical.to_string())
    }

    fn address_help(&self) -> String {
        String::new()
    }
}

impl SessionDriver for FakeDriver {
    fn connect(
        &self,
        settings: &Settings,
        cx: ConnectContext,
    ) -> BoxFuture<'static, Result<Box<dyn Session>, FatalError>> {
        self.seen.lock().expect("seen poisoned").push(SeenConnect {
            settings: settings.clone(),
            connect_timeout: cx.connect_timeout,
            response_timeout: cx.response_timeout,
        });
        let refused = settings.get("port") == Some("0");
        let plc = self.plc.clone();
        Box::pin(async move {
            if refused {
                return Err(FatalError::Io("fake: port 0 refuses".to_string()));
            }
            plc.dial()
        })
    }
}

/// A registry holding `driver` as a session-type driver.
pub(crate) fn registry_of(driver: FakeDriver) -> Arc<DriverRegistry> {
    let mut registry = DriverRegistry::new();
    registry
        .register(DriverEntry::session(Arc::new(driver)))
        .expect("one fake driver");
    Arc::new(registry)
}

/// A `Numeric(kind)` spec at `canonical` for the fake driver `id`.
pub(crate) fn spec(id: &str, canonical: &str, data_type: DataType) -> TagSpec {
    let kind = TagKind::numeric(data_type);
    TagSpec::new(
        DriverAddress::new(
            DriverId::new(id).expect("valid driver id"),
            canonical,
            "word",
            true,
            kind.word_count(),
        ),
        kind,
    )
}

/// Sends `()` on drop - lets a test observe that a connect attempt's future
/// was dropped (aborted).
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
