//! banto-plc: I2 PLC通信の共通部分 (docs/plan.md I2, docs/recorder-requirements.md
//! §1 "対象環境", §3.1 "タグ・収集").
//!
//! Read-side building blocks shared by the PLC drivers: addresses, the request
//! planners, the typed decode and the in-process simulators. R2b-3d-join
//! (docs/banto-hub-driver-io-design.md §6.1, §6.2) removed the I/O this crate
//! used to own - the `PlcClient` trait and its `ModbusTcpClient` /
//! `SlmpClient` implementations, `dial_modbus` / `dial_slmp` and the
//! `execute_*_reads` functions. The sockets are owned by `banto-driver-engine`
//! now, and the protocol drivers (`banto-driver-modbus`, `banto-driver-slmp`)
//! plan with this crate and decode with it.
//!
//! ## Module map
//!
//! - [`address`]: the [`address::Address`] sum type and both notations'
//!   parsers - Modbus reference numbers (`"40001"` -> holding register offset
//!   0) and MELSEC device codes (`"D100"`) - pure and thoroughly tested in
//!   isolation
//! - [`types`]: the request/result/value shapes of a read
//! - [`planning`]: turns a flat tag list into the minimal set of wire reads
//!   (register/coil grouping within Modbus's quantity limits) - this is
//!   what makes 256-tag/100ms cycles (recorder-requirements.md §3.1)
//!   feasible. [`slmp::planning`] is its MELSEC counterpart.
//! - [`decode`]: raw register window -> typed value, including the
//!   per-device word-order switch for 32-bit types. Shared by both protocols.
//! - [`modbus`]: the Modbus TCP wire format ([`modbus::frame`]), the
//!   in-process simulator and [`ModbusTcpConfig`], chosen to go first for
//!   debuggability (recorder-requirements.md §1).
//! - [`slmp`]: the MELSEC MC/SLMP device table, planner, simulator, [`SlmpConfig`]
//!   and the classification of the `slmp` crate's errors.
//! - [`error`]: [`error::PlcError`], the one error type spanning both
//!   whole-call (`Err`) and per-request (`ReadResult::Bad`) failures
//!
//! ## What this crate deliberately does not do
//!
//! - **No writes.** PLC writes are out of scope for the recorder product
//!   (docs/recorder-requirements.md §7); the write side is the separate
//!   `banto-plc-write` crate.
//! - **No reconnect loop.** Retrying/backing off across PLC-connection drops
//!   is the collection engine's job (docs/plan.md I2 §2, "再接続ループは
//!   持たない"; recorder-requirements.md §3.1: "PLC 断で Bad を記録し続け、復旧後に
//!   自動再接続").

pub mod address;
pub mod decode;
pub mod error;
pub mod modbus;
pub mod planning;
pub mod slmp;
pub mod types;

pub use address::{Address, AddressArea};
pub use decode::WordOrder;
pub use error::PlcError;
pub use modbus::ModbusTcpConfig;
pub use planning::{
    modbus_accepts, plan_batch_requests, plan_requests, MappedRequest, PlanOutcome, PlannedRead,
};
pub use slmp::address::{SlmpAccess, SlmpDevice};
pub use slmp::planning::{
    plan_slmp_batch, plan_slmp_requests, slmp_accepts_numeric, slmp_accepts_string, ReadKind,
    SlmpMappedRequest, SlmpPlanOutcome, SlmpPlannedRead,
};
pub use slmp::{classify_slmp, SlmpConfig, SlmpCpu, SlmpErrorClass};
pub use types::{
    BatchReadRequest, BatchReadResult, DataType, PlcValue, ReadRequest, ReadResult, StringEncoding,
    StringReadRequest, TagValue,
};
