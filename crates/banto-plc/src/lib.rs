//! banto-plc: I2 PLC通信クライアント (docs/plan.md I2, docs/recorder-requirements.md
//! §1 "対象環境", §3.1 "タグ・収集").
//!
//! Read-only, bulk-read PLC access behind one protocol-agnostic trait
//! ([`client::PlcClient`]) so the collection engine (I3) never has to know
//! which wire protocol a given [`banto_tags`-style `PlcConnection`] speaks -
//! see `client.rs`'s module doc for the trait's three fixed invariants and
//! why it is written to be `dyn`-compatible.
//!
//! ## Module map
//!
//! - [`address`]: the [`address::Address`] sum type and both notations'
//!   parsers - Modbus reference numbers (`"40001"` -> holding register offset
//!   0) and MELSEC device codes (`"D100"`) - pure and thoroughly tested in
//!   isolation
//! - [`types`]: the request/result/value shapes that cross the
//!   `PlcClient` boundary
//! - [`client`]: the `PlcClient` trait itself
//! - [`planning`]: turns a flat tag list into the minimal set of wire reads
//!   (register/coil grouping within Modbus's quantity limits) - this is
//!   what makes 256-tag/100ms cycles (recorder-requirements.md §3.1)
//!   feasible. [`slmp::planning`] is its MELSEC counterpart.
//! - [`decode`]: raw register window -> typed value, including the
//!   per-device word-order switch for 32-bit types. Shared by both protocol
//!   implementations, which is why SLMP word groups are fetched as raw `u16`
//!   windows rather than pre-typed by the wrapped crate.
//! - [`modbus`]: the first protocol implementation
//!   ([`modbus::ModbusTcpClient`]), chosen to go first for debuggability
//!   (recorder-requirements.md §1)
//! - [`slmp`]: the MELSEC MC/SLMP implementation ([`slmp::SlmpClient`], I2a) -
//!   the eventual primary target, a sibling behind the same trait. Unlike
//!   `modbus`, it wraps an external crate for the wire framing rather than
//!   hand-implementing it; see its module doc for why, and for where the
//!   connection-fatal/per-request line falls once someone else owns the
//!   socket.
//! - [`error`]: [`error::PlcError`], the one error type spanning both
//!   whole-call (`Err`) and per-request (`ReadResult::Bad`) failures
//!
//! ## What this crate deliberately does not do
//!
//! - **No writes.** Not even a stubbed-out `write` method - PLC writes are
//!   out of scope for the recorder product (docs/recorder-requirements.md
//!   §7) and a future write-capable client (e.g. for a recipe-download app,
//!   docs/plan.md §3) should be its own trait/crate rather than an unused
//!   footgun on every read-only consumer. This holds even though the `slmp`
//!   crate I2a added as a dependency implements write commands too: they are
//!   simply not called from here, and I5's plan puts the write client in a
//!   separate `banto-plc-write` crate with its own `PlcWriteClient` trait for
//!   exactly this reason.
//! - **No reconnect loop.** A connection-fatal error leaves the client in a
//!   disconnected state (`PlcClient::read_batch` returns `Err`, further
//!   calls return `PlcError::NotConnected`) and stays there until the
//!   caller calls `connect()` again. Retrying/backing off across
//!   PLC-connection drops is I3's job (docs/plan.md I2 §2, "再接続ループは
//!   持たない"), since only I3 knows the collection engine's overall
//!   schedule and how to fold "PLC断" into a quality flag
//!   (recorder-requirements.md §3.1: "PLC 断で Bad を記録し続け、復旧後に
//!   自動再接続").

pub mod address;
pub mod client;
pub mod decode;
pub mod error;
pub mod modbus;
pub mod planning;
pub mod slmp;
pub mod types;

pub use address::{Address, AddressArea};
pub use client::{BoxFuture, PlcClient};
pub use decode::WordOrder;
pub use error::PlcError;
pub use modbus::{ModbusTcpClient, ModbusTcpConfig};
pub use planning::{plan_requests, MappedRequest, PlanOutcome, PlannedRead};
pub use slmp::address::{SlmpAccess, SlmpDevice};
pub use slmp::planning::{plan_slmp_requests, SlmpMappedRequest, SlmpPlanOutcome, SlmpPlannedRead};
pub use slmp::{SlmpClient, SlmpConfig, SlmpCpu};
pub use types::{DataType, ReadRequest, ReadResult, TagValue};
