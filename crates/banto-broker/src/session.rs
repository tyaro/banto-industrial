//! What the broker's job loop runs a connection on: a `banto-driver-api`
//! [`Session`], dialed by a [`Connector`] (R2b-3a-2,
//! docs/banto-hub-driver-io-design.md §4.2).
//!
//! ## History: `BrokerSession` is gone
//!
//! From #130 (I9) to R2b-3a-1 this module defined the broker's own session
//! trait, `BrokerSession`, whose requests were `banto_plc::BatchReadRequest` /
//! `banto_plc_write::BatchWriteRequest` and whose fatal error was a
//! `SessionError(String)`. R2b-3a-2 replaced it with the driver API's
//! [`Session`]: requests are [`TagSpec`](banto_driver_api::TagSpec) /
//! [`WriteItem`](banto_driver_api::WriteItem), results are per item
//! `Result<Value, ItemError>` / `Result<(), ItemError>`, and the fatal error
//! keeps its type ([`FatalError`]). The contract is the same one
//! `BrokerSession` carried, now stated on the API trait:
//!
//! - **`Err` means the connection itself is no longer usable** - a broken
//!   socket, a timeout, a framing error. A per-item failure (a device-side end
//!   code / exception, an address the device refuses) is that item's
//!   [`ItemError`](banto_driver_api::ItemError) inside `Ok(Vec<_>)`, at the
//!   same index as its request. `run_broker_task` trusts this completely: on
//!   `Err` it drops the session and reconnects with backoff.
//! - **One session serves both directions.** `read_batch` and `write_batch`
//!   share `&mut self` on the one `Box<dyn Session>` the task owns, so a read
//!   and a write to the same device can never interleave on the wire (the
//!   reason this crate exists, I6).
//!
//! ## [`Connector`]
//!
//! A reusable "dial a fresh session" step. [`crate::SessionDirectory`] builds
//! one per connection from the registry's Session or Frame entry
//! (`SessionDriver::connect` or the Frame adapter below, with `Settings` and a
//! `ConnectContext`), and the task calls it again on every reconnect attempt.
//! The broker's own unit tests build connectors directly (`fake.rs`).
//!
//! [`DriverKind::Session`]: banto_driver_api::DriverKind::Session
//!
//! ## [`SecretOpener`]
//!
//! connection-settings design §3.5: a secret (`{"$secret": "<ref>"}` in the
//! connection's `settings`) is never given to validation, only to the
//! connection, opened just before the dial. Opening needs the host's sealing
//! (banto-hub's `secrets` module) and its database, neither of which this
//! crate may depend on, so the host injects the "open" step as a
//! [`SecretOpener`]. The connector opens **only the keys the driver lists in
//! `secret_keys()`** and holds the plaintext for the one dial, zeroizing it
//! afterwards.

use banto_driver_api::{BoxFuture, FatalError, Session};
use banto_tags::PlcConnection;
use zeroize::Zeroizing;

/// A reusable "dial a fresh session" step for one broker task, boxed so
/// `run_broker_task` can hold and repeatedly call it (on every reconnect
/// attempt) without knowing which driver produced it. `Arc` (not `Box`)
/// because each attempt runs on its own spawned sub-task while the original
/// stays with the task loop; `Fn` (not `FnOnce`) because a failed attempt
/// must be retryable.
pub type Connector = std::sync::Arc<
    dyn Fn() -> BoxFuture<'static, Result<Box<dyn Session>, FatalError>> + Send + Sync,
>;

/// The host's "open this connection's secret" step (see the module doc).
///
/// `Ok(None)`: the connection has no value for `key`. `Err(reason)`: there is
/// a reference but it cannot be opened (missing row, undecryptable, malformed),
/// and the dial fails with [`FatalError::InvalidConfig`]; it is never turned
/// into an empty value (checklist §5「エラーを空に潰さない」). `reason` must
/// not contain the secret.
pub trait SecretOpener: Send + Sync + 'static {
    /// Open `conn`'s secret `key` (a key the driver's `secret_keys()` lists
    /// and `conn.settings` holds a reference for).
    fn open(
        &self,
        conn: &PlcConnection,
        key: &str,
    ) -> BoxFuture<'static, Result<Option<Zeroizing<String>>, String>>;
}

/// The default [`SecretOpener`]: opens nothing. A connection whose driver
/// needs a secret it holds a reference for fails to dial
/// ([`FatalError::InvalidConfig`]) instead of dialing without it. The
/// built-in Modbus / SLMP drivers have no secret keys, so they never reach it.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoSecretOpener;

impl SecretOpener for NoSecretOpener {
    fn open(
        &self,
        _conn: &PlcConnection,
        _key: &str,
    ) -> BoxFuture<'static, Result<Option<Zeroizing<String>>, String>> {
        Box::pin(async {
            Err("この broker には秘密値を開く口がありません".to_string())
        })
    }
}

/// Only the dial is adapted: the broker still owns one API Session per task.
/// No driver registry entry is changed by this wrapper.
pub(crate) struct FrameDial {
    pub(crate) driver: std::sync::Arc<dyn banto_driver_api::FrameDriver>,
    /// The owner mark this dial records on the serial line it leases
    /// (io-design §7.3): one per broker task, so the task's release closes only
    /// the line it leased last, never one a newer task for the same
    /// connection has since leased or opened.
    pub(crate) owner: banto_driver_engine::line::LineOwner,
}

impl FrameDial {
    pub(crate) fn new(driver: std::sync::Arc<dyn banto_driver_api::FrameDriver>) -> Self {
        Self {
            driver,
            owner: banto_driver_engine::line::LineOwner::new(),
        }
    }
}

impl banto_driver_api::DriverBase for FrameDial {
    fn descriptor(&self) -> &banto_driver_api::DriverDescriptor {
        self.driver.descriptor()
    }
    fn validate_config(
        &self,
        settings: &banto_driver_api::Settings,
    ) -> Vec<banto_driver_api::FieldError> {
        let mut errors = self.driver.validate_config(settings);
        if let Err(error) = frame_transport(settings, self.driver.descriptor()) {
            // Keep the driver's more specific message for a field it already rejected.
            if !errors.iter().any(|existing| existing.key == error.key) {
                errors.push(error);
            }
        }
        errors
    }
    fn secret_keys(&self) -> Vec<String> {
        self.driver.secret_keys()
    }
    fn parse_address(
        &self,
        text: &str,
        kind: &banto_driver_api::TagKind,
    ) -> Result<banto_driver_api::DriverAddress, banto_driver_api::AddressError> {
        self.driver.parse_address(text, kind)
    }
    fn offset_address(
        &self,
        canonical: &str,
        kind: &banto_driver_api::TagKind,
        n: i64,
    ) -> Result<String, banto_driver_api::AddressError> {
        self.driver.offset_address(canonical, kind, n)
    }
    fn address_help(&self) -> String {
        self.driver.address_help()
    }
    fn probe(&self) -> Option<(String, banto_driver_api::TagKind)> {
        self.driver.probe()
    }
}

impl banto_driver_api::SessionDriver for FrameDial {
    fn connect(
        &self,
        settings: &banto_driver_api::Settings,
        cx: banto_driver_api::ConnectContext,
    ) -> BoxFuture<'static, Result<Box<dyn Session>, FatalError>> {
        let driver = self.driver.clone();
        let owner = self.owner;
        // The owned copy must zeroize too, including when never polled.
        let mut settings = super::OpenedSettings::new(settings.clone());
        settings.opened = driver.secret_keys();
        Box::pin(async move {
            let spec = frame_transport(settings.settings(), driver.descriptor())
                .map_err(|error| FatalError::InvalidConfig(error.to_string()))?;
            banto_driver_engine::FrameSession::connect_as(
                driver.as_ref(),
                settings.settings(),
                &spec,
                &cx,
                None,
                Some(owner),
            )
            .await
            .map(|session| Box::new(session) as Box<dyn Session>)
        })
    }
}

/// Transport validation belongs to the host that opens it (R2b-3e-2: serial
/// and the converter's TCP are accepted when the driver's descriptor names
/// them; the engine's line table then owns the port and resynchronises it,
/// io-design §7.3). Never silently dial TCP for an unknown kind.
fn frame_transport(
    settings: &banto_driver_api::Settings,
    descriptor: &banto_driver_api::DriverDescriptor,
) -> Result<banto_driver_engine::transport::TransportSpec, banto_driver_api::FieldError> {
    banto_driver_engine::transport::TransportSpec::from_settings(settings, &descriptor.transports)
}

impl FrameDial {
    /// The serial line `settings` dial (io-design §7.3), or `None` for TCP and
    /// for settings the transport validation refuses. The broker closes this
    /// line in the engine's table when it stops the connection, so a stopped or
    /// re-configured connection releases the port and a later start begins
    /// with a fresh line (§7.3 steps 4 and 6).
    pub(crate) fn line_key(
        &self,
        settings: &banto_driver_api::Settings,
    ) -> Option<banto_driver_engine::line::LineKey> {
        frame_transport(settings, self.driver.descriptor())
            .ok()
            .and_then(|spec| banto_driver_engine::line::LineKey::of(&spec))
    }
}
