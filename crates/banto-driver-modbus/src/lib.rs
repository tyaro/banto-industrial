//! `banto-driver-modbus`: the Modbus TCP driver (docs/banto-driver-api-design.md
//! §2.3, §4.4; docs/banto-hub-driver-io-design.md §4).
//!
//! ## What this crate is
//!
//! Until R1-4a the Modbus TCP driver (#131) lived inside `banto-broker`;
//! R1-4a moved it here. R2b-3a-1 added the production
//! [`banto_driver_api::SessionDriver`], and **R2b-3a-2 made it the only way
//! in**: the composition root (`banto-drivers`) registers [`driver`] as
//! `DriverEntry::session`, and the broker dials it through the registry with
//! the connection's `Settings` and a `ConnectContext`. The former broker
//! table entry (`dial`) and the `banto_broker::BrokerSession` impl are gone.
//!
//! - [`ModbusDriver`] / [`driver`] (`base.rs`): the [`banto_driver_api::DriverBase`]
//!   implementation - identity, `validate_config`, `parse_address` over
//!   `Address::parse`, `offset_address` / `address_help` ported from the
//!   TypeScript, and the connection test's `probe` point (`40001`, R2b-3a-2 /
//!   Q11).
//! - `api_session.rs`: the [`banto_driver_api::SessionDriver`] / `Session`
//!   (design §4.1). `connect` builds the config from `Settings` +
//!   `ConnectContext` and dials with `banto_plc::dial_modbus`; reads and
//!   writes run on `session::ModbusSession` - **one** socket and **one**
//!   transaction-id counter shared by reads and writes (see that type's doc,
//!   the most safety-critical property of this driver) - with the typed
//!   `PlcError` -> `FatalError` / `ItemError` mapping. The canonical address
//!   is turned back into a `banto_plc::Address` privately inside this crate.
//! - `frame_conn.rs` (R2b-3c, design §6.1): the [`banto_driver_api::FrameDriver`] /
//!   `FrameConnection` of the same driver, for `banto-driver-engine`: MBAP
//!   frames out (`plan_read` / `write_next`), responses in (`decode_read` and
//!   the write echo check), **one** `next_transaction_id` per connection for
//!   reads and writes. It reuses the session's planners and gates
//!   (`plan_batch_requests`, `plan_modbus_writes`, `read_request` /
//!   `write_request`), so what is read, written and refused is the same. Not
//!   registered yet: the broker still dials `api_session.rs` until 3c-join.
//! - [`classify_read`] / [`classify_write`] (`classify.rs`): the §3.4 table
//!   from `PlcError` / `PlcWriteError` to `ItemError` / `FatalError`.
//!
//! ## Dependency direction
//!
//! This crate -> `banto-broker` (only for the shared `parse_word_order`),
//! never the reverse: the broker does not depend on any driver crate, not
//! even as a dev-dependency. The test that pins down Modbus through the
//! broker lives in this crate's `tests/broker.rs`.

mod api_session;
mod base;
mod classify;
mod frame_conn;
mod session;

pub use base::{driver, ModbusDriver};
pub use classify::{classify_read, classify_write};

/// The protocol string (`PlcConnection::protocol`) this driver serves - the
/// id `banto-drivers` registers [`driver`] under.
pub const PROTOCOL: &str = "modbus-tcp";
