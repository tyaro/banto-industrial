//! banto-plc-write: I5 PLC *write* client (docs/plan.md I5, the relay-wright
//! plan `luminous-discovering-goblet.md`).
//!
//! The write counterpart to `banto-plc`, kept a **separate crate with a
//! separate trait** on purpose. `banto_plc::PlcClient`'s module doc reserves
//! writes for exactly this: "a deliberately separate trait/crate rather than a
//! footgun sitting unused on every read-only consumer". So the read-only
//! consumers (ChronoGazer, banto-collect) link `banto-plc` and never see a
//! write method; relay-wright (W1-W5) additionally links this crate. The
//! dependency runs one way only - this crate depends on `banto-plc` to reuse
//! its shared vocabulary; `banto-plc` never depends back.
//!
//! ## What is reused from `banto-plc` rather than redefined
//!
//! - [`banto_plc::Address`] - a write target is an `Address::Slmp { .. }`; a
//!   Modbus address handed to the SLMP write planner is a per-request `Bad`.
//! - [`banto_plc::DataType`] / [`banto_plc::TagValue`] - the wire type and the
//!   value to write (see [`types`] for why `TagValue` fits a write payload).
//! - [`banto_plc::WordOrder`] - the 32-bit word-order switch, applied by
//!   [`encode`] as the exact inverse of `banto-plc`'s `decode`, so a value
//!   written here and read back there round-trips.
//! - [`banto_plc::SlmpDevice`] / [`banto_plc::SlmpAccess`] / the SLMP address
//!   parser - not re-implemented; a target's device notation is parsed by
//!   `banto_plc::Address::parse_slmp`.
//! - [`banto_plc::SlmpConfig`] / [`banto_plc::SlmpCpu`] - one config type for
//!   the connection, so the W3 broker's shared session and this crate's
//!   standalone client are configured the same way.
//! - [`banto_plc::BoxFuture`] - the same hand-boxed future the read trait uses
//!   for `dyn`-compatibility.
//!
//! ## Module map
//!
//! - [`types`]: [`WriteRequest`] / [`WriteResult`], the shapes crossing the
//!   [`PlcWriteClient`] boundary.
//! - [`client`]: the [`PlcWriteClient`] trait, and why it is a separate trait.
//! - [`error`]: [`PlcWriteError`], the one error type spanning whole-call
//!   (`Err`) and per-request (`WriteResult::Bad`) failures, with the write-only
//!   value-range cases the read side has no equivalent for.
//! - [`encode`]: `TagValue` -> raw register/bit payload, the inverse of
//!   `banto-plc`'s `decode`.
//! - [`slmp`]: the MELSEC MC / SLMP implementation - [`SlmpWriteClient`] (the
//!   standalone, owns-its-socket form) plus [`slmp::plan_slmp_writes`] and
//!   [`slmp::execute_slmp_writes`], the pure-planner / borrowed-client pair the
//!   W3 broker calls to write over its *shared* single-session-per-CPU socket.
//!
//! ## T8: bit-in-word writes (docs/tag-server-design.md §6.1, 2026-08-06)
//!
//! [`BatchWriteRequest::BitInWord`] sets or clears a single bit of a *word*
//! device (`"D100.5"`) without a dedicated SLMP write command for it - SLMP
//! only writes whole words. `slmp::planning` plans this as a
//! [`SlmpPlannedBitWrite`] (a mask-composed *recipe*, not a ready payload,
//! since the word's other 15 bits are unknown until read), and
//! [`slmp::execute_slmp_writes`] carries it out as a
//! read-modify-write-**confirm** sequence on the broker's single shared
//! session - see that function's and `slmp::planning`'s module docs for the
//! full RMW design (mask composition, the gap-tolerance-zero rule extended to
//! "different words never merge", and why this needed zero `banto-broker`
//! changes).

pub mod client;
pub mod encode;
pub mod error;
pub mod slmp;
pub mod types;

pub use client::PlcWriteClient;
pub use error::PlcWriteError;
pub use slmp::planning::{
    plan_slmp_write_batch, plan_slmp_writes, BitWriteMapping, SlmpPlannedBitWrite,
    SlmpPlannedWrite, SlmpWritePlanOutcome, WritePayload,
};
pub use slmp::{execute_slmp_writes, SlmpWriteClient};
pub use types::{BatchWriteRequest, StringWriteRequest, WriteRequest, WriteResult};

// Re-exported for callers building requests, so they need only depend on this
// crate for a basic write. The canonical definitions stay in `banto-plc`.
pub use banto_plc::{Address, DataType, SlmpConfig, SlmpCpu, TagValue, WordOrder};
