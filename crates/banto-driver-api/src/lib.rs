//! banto-driver-api: the driver boundary of banto-hub / ChronoGazer - types
//! and traits only (docs/banto-driver-api-design.md,
//! docs/banto-hub-driver-refactor-plan.md §4 R1).
//!
//! This crate is the one place a PLC/instrument driver and the host (Hub's
//! engine, ChronoGazer's collector) agree on vocabulary. It is published to
//! crates.io so drivers can be written outside this repository; everything
//! here is therefore chosen to be stable, dependency-free, and free of
//! logic.
//!
//! ## Design rules (docs/banto-driver-api-design.md §0, §2.1, §9)
//!
//! - **Types and traits only.** No planning, decoding, word-order math, I/O,
//!   tokio or `thiserror`: `Display` is written by hand. The only dependency
//!   is `serde`, behind the optional `serde` feature.
//! - **Types moved from `banto-plc` are not `#[non_exhaustive]`**
//!   ([`DataType`], [`TagValue`], [`Value`], [`StringEncoding`],
//!   [`WordOrder`] - R1-1, design §3.1). Callers outside this crate match on
//!   them exhaustively today (`banto-plc-write`'s encoder, banto-hub's write
//!   path), and `banto-plc` re-exports them under their old names
//!   (`banto_plc::PlcValue` is [`Value`]), so moving them changed no caller.
//!   Adding a variant to one of these is a breaking (0.x minor) change.
//! - **Types new in this crate are `#[non_exhaustive]` and have
//!   constructors** ([`RawValue`], [`TagKind`], [`TagSpec`], [`WriteItem`],
//!   [`DriverAddress`], [`ItemError`], [`DeviceError`], [`FatalError`],
//!   [`Settings`], [`Setting`], [`FieldError`], design §3.2-§3.5): variants
//!   and fields can be added in a patch/minor release without breaking a
//!   driver, which in turn means code outside this crate cannot build them
//!   with a struct literal - use `new` and the `with_*` builders, and match
//!   with a trailing `_` / `..`.
//! - **The R1 request types stay in `banto-plc`.** `ReadRequest` /
//!   `BatchReadRequest` carry `banto_plc::Address`, a closed two-protocol
//!   enum that must not become API (every new protocol would break it). This
//!   crate instead has the opaque [`DriverAddress`] (WIT `parsed`), which R1
//!   only computes alongside the real path; R2b-2 switches requests to
//!   [`TagSpec`] (design §3.3, §13 item 7).
//! - **Connection-fatal vs per-item is a type, not a predicate.**
//!   [`ItemError`] (that tag is Bad, the connection lives) and [`FatalError`]
//!   (drop and reconnect) replace `PlcError::is_connection_fatal`; anything a
//!   driver cannot place in `ItemError` is fatal by default (design §3.4).
//!
//! ## Module map
//!
//! - [`types`]: values and data types (moved from `banto-plc`), plus the
//!   per-tag read/write unit ([`TagKind`], [`TagSpec`], [`WriteItem`],
//!   [`RawValue`])
//! - [`address`]: [`DriverAddress`] and the driver identifier [`DriverId`]
//!   it names
//! - [`error`]: [`ItemError`] / [`DeviceError`] / [`FatalError`]
//! - [`settings`]: ordered, duplicate-free connection settings ([`Settings`])
//!   and the shared key names ([`settings::keys`])
//! - [`driver`]: the static half every driver implements ([`DriverBase`]:
//!   identity, settings validation, address functions) with
//!   [`DriverDescriptor`] / [`Capabilities`] / [`Transport`] /
//!   [`AddressError`] (R1-2, design §4.1)
//! - [`frame`]: frame-type drivers, which build and parse bytes while the
//!   host's engine owns the transport ([`FrameDriver`] /
//!   [`FrameConnection`], [`Step`] / [`FrameRule`] / [`Planned`], the write
//!   state machine [`WriteProgress`]; R1-2, design §4.2, §4.2.1)
//! - [`session`]: session-type drivers, which own their I/O
//!   ([`SessionDriver`] / [`Session`], [`BoxFuture`]; R1-2, design §5)
//! - [`registry`]: [`DriverRegistry`], driver id → [`DriverEntry`] of a
//!   [`DriverKind`] (R1-2, design §6.1)
//!
//! ## Traits: what R1 implements
//!
//! In R1 no production driver is a [`FrameDriver`] or [`SessionDriver`]:
//! Modbus TCP / SLMP implement only [`DriverBase`] and are registered as
//! [`DriverKind::HostBound`], their I/O still wired by `banto-broker`
//! (design §4.4). Since R2b-1a a host may also register a protocol whose I/O
//! it keeps itself as [`DriverKind::HostProvided`] (banto-hub's `push` /
//! `postgres` / `virtual`; permanent, unlike `HostBound`). The traits are
//! exercised by the separate conformance crate
//! `banto-driver-conformance` (a reference frame driver, a fake session
//! driver and the harness, design §7), which a third-party driver uses to
//! check itself without a Hub.
//!
//! Adding a trait method *with* a default implementation is not breaking;
//! adding one without is (design §9.1).
//!
//! Every public item is also re-exported at the crate root.
//!
//! ## Versioning
//!
//! 0.x: a minor bump may break, a patch never does (design §9.1). What broke
//! and how to adapt is written in `CHANGELOG.md`. The minimum supported Rust
//! version is the `rust-version` in `Cargo.toml`, checked by CI.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod address;
pub mod driver;
pub mod error;
pub mod frame;
pub mod registry;
pub mod session;
pub mod settings;
pub mod types;

pub use address::{DriverAddress, DriverId, InvalidDriverId};
pub use driver::{
    AddressError, Capabilities, DriverBase, DriverDescriptor, Transport, API_VERSION,
};
pub use error::{DeviceError, FatalError, ItemError};
pub use frame::{
    FrameCheck, FrameConnection, FrameDriver, FrameRule, Planned, PrefixEncoding, PrefixSpec, Step,
    WriteProgress,
};
pub use registry::{DriverEntry, DriverKind, DriverRegistry, RegistryError};
pub use session::{BoxFuture, ConnectContext, Session, SessionDriver};
pub use settings::{keys, DuplicateKey, FieldError, Setting, Settings};
pub use types::{
    DataType, RawValue, StringEncoding, TagKind, TagSpec, TagValue, Value, WordOrder, WriteItem,
};
