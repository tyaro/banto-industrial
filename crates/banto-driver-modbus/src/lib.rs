//! `banto-driver-modbus`: the Modbus TCP driver (docs/banto-driver-api-design.md
//! §2.3, §4.4; docs/banto-hub-driver-io-design.md §4).
//!
//! ## What this crate is
//!
//! Until R1-4a the Modbus TCP driver (#131) lived inside `banto-broker`;
//! R1-4a moved it here. R2b-3a-1 added a production
//! [`banto_driver_api::SessionDriver`] (that owned a socket), and
//! **R2b-3c replaced it with the frame-type driver**: the composition root
//! (`banto-drivers`) registers [`driver`] as `DriverEntry::frame`, and the
//! broker wraps it in `banto-driver-engine`'s `FrameSession`, which owns the
//! socket, the response timeout and the "a timeout is fatal" rule. The
//! session-type I/O (`api_session.rs`, `session.rs`, `banto_plc::dial_modbus`
//! and `execute_modbus_reads`, `banto_plc_write::execute_modbus_writes`) is
//! gone.
//!
//! - [`ModbusDriver`] / [`driver`] (`base.rs`): the [`banto_driver_api::DriverBase`]
//!   implementation - identity, `validate_config`, `parse_address` over
//!   `Address::parse`, `offset_address` / `address_help` ported from the
//!   TypeScript, and the connection test's `probe` point (`40001`, R2b-3a-2 /
//!   Q11).
//! - `frame_conn.rs` (design §6.1): the [`banto_driver_api::FrameDriver`] /
//!   `FrameConnection`: MBAP frames out (`plan_read` / `write_next`),
//!   responses in (`decode_read` and the write echo check), **one**
//!   `next_transaction_id` per connection for reads and writes (the most
//!   safety-critical property of this driver). The canonical address is turned
//!   back into a `banto_plc::Address` privately inside this crate
//!   (`request.rs`, which also holds the read / write request gates).
//! - [`classify_read`] / [`classify_write`] (`classify.rs`): the §3.4 table
//!   from `PlcError` / `PlcWriteError` to `ItemError` / `FatalError`.
//!
//! ## Dependency direction
//!
//! This crate -> `banto-broker` (only for the shared `parse_word_order`),
//! never the reverse: the broker does not depend on any driver crate, not
//! even as a dev-dependency. The test that pins down Modbus through the
//! broker lives in this crate's `tests/broker.rs`.

mod base;
mod classify;
mod frame_conn;
mod request;

pub use base::{driver, ModbusDriver};
pub use classify::{classify_read, classify_write};

/// The protocol string (`PlcConnection::protocol`) this driver serves - the
/// id `banto-drivers` registers [`driver`] under.
pub const PROTOCOL: &str = "modbus-tcp";
