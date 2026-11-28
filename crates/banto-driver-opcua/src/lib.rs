//! `banto-driver-opcua`: the OPC UA client driver, a shared session-type
//! driver for banto-hub and ChronoGazer (docs/chronogazer/opcua-client-design.md
//! §3, #1324 stage 3, the body of #678).
//!
//! ## What this crate is
//!
//! OPC UA does framing, the secure channel and the session inside its client
//! library, so this is a [`banto_driver_api::SessionDriver`] (design §5 of
//! docs/hub/driver/banto-driver-api-design.md), not a frame-type driver. The
//! protocol part is banto-opcua's `banto-opcua-client` (its ADR-0005): this
//! crate turns the Driver API's vocabulary into the component's and back, and
//! never lets an async-opcua type out (the component's API is the core
//! vocabulary only).
//!
//! Not registered yet: `banto-drivers`' `builtin()` gets it in the next PR
//! together with the Hub's screen that says the connection is plain text and
//! anonymous. Broker / collect / Hub read paths are unchanged.
//!
//! ## Decisions (design §3, §7)
//!
//! - **Settings** ([`keys`]): `endpointUrl` (required, `opc.tcp://`),
//!   `readMode` (`read` only; `subscribe` is refused as "not supported yet"),
//!   `keepAliveIntervalMs` / `maxFailedKeepAliveCount` (the component's
//!   defaults, 5000 ms and 3; ADR-0005 wants them always set to find a
//!   silent loss). The two timeouts come from the host's
//!   [`banto_driver_api::ConnectContext`]. SecurityPolicy and user
//!   authentication are not settings: `None` / Anonymous until banto-opcua#54.
//! - **Reconnection is the broker's** (ADR-0005's default): the component's
//!   automatic reconnect is off ([`banto_opcua_client::ReconnectPolicy::Disabled`]),
//!   so a lost connection is an `Err` and the host connects anew (which also
//!   re-reads the NamespaceArray, so `nsu=` resolves again).
//! - **Addresses** are NodeId strings parsed by `banto-opcua-core`'s
//!   `NodeAddress`; the canonical form is its `Display`. GUID / ByteString
//!   identifiers are refused, and there is no "next" NodeId, so continuous
//!   registration and word offsets are refused.
//! - **Kinds**: bit and every number, 64-bit ones included. No string yet:
//!   the API's `TagKind::Str { words, encoding }` is shaped for PLC words and
//!   how an OPC UA String maps onto it is decided in #678.
//! - **Values** (`convert.rs`): Boolean is a bit, numbers widen to `f64`. An
//!   integer-typed tag that receives a fraction, an out-of-range value or a
//!   NaN is that tag's `ItemError`, never silently rounded (design §3.4,
//!   §7 item 5). A value of the wrong type, an array or a structure is that
//!   tag's `ItemError` too.
//! - **Quality and time** (design §3.5, §3.6): the StatusCode is mapped by
//!   its severity bits (never `== Good`): Good is
//!   [`banto_driver_api::ReadQuality::Good`], Uncertain with a value is
//!   `Uncertain`, Bad or no value is an `ItemError`. The SourceTimestamp, when
//!   the server sends one, becomes `source_time_ms`; it is never filled with
//!   the current time. `read_batch` is `read_batch_detailed` without the
//!   quality: Good items and their order are the same, and an Uncertain item
//!   with a value is `Ok(Reading)` in the detailed read but that tag's
//!   `ItemError::Device` (its StatusCode) in the plain one.
//! - **Writes** (`session.rs`, #678): the value is sent as the tag's type (a
//!   node of another type answers `BadTypeMismatch`, that tag's
//!   `ItemError::Device`), and the component's per-node outcomes - also of a
//!   split Write broken in the middle (`ItemError::Indeterminate` for "sent,
//!   result lost") - are mapped by the table in `session.rs`'s module doc.
//!
//! ## Module map
//!
//! - [`OpcUaDriver`] / [`driver`] (`base.rs`): the [`banto_driver_api::DriverBase`]
//!   half - identity, settings, the address functions, kinds, the probe.
//! - `convert.rs`: pure mappings (value, StatusCode, time, write value,
//!   component error to `FatalError`), unit-tested without a server.
//! - `session.rs`: [`banto_driver_api::SessionDriver::connect`] and the
//!   `Session` over `banto_opcua_client::ConnectedClient`.

mod base;
mod convert;
mod session;

pub use base::{driver, keys, OpcUaDriver};

/// The protocol string (`PlcConnection::protocol`) this driver serves - the
/// id `banto-drivers` will register [`driver`] under.
pub const PROTOCOL: &str = "opcua";
