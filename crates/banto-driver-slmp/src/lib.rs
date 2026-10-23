//! `banto-driver-slmp`: the MELSEC SLMP driver (docs/banto-driver-api-design.md
//! §2.3, §4.4; docs/banto-hub-driver-io-design.md §4).
//!
//! ## What this crate is
//!
//! Until R1-4a the SLMP driver lived inside `banto-broker`; R1-4a moved it
//! here. R2b-3a-1 added a production [`banto_driver_api::SessionDriver`] (that
//! owned a socket), and **R2b-3d replaced it with the frame-type driver**:
//! the composition root (`banto-drivers`) registers [`driver`] as
//! `DriverEntry::frame`, and the broker wraps it in `banto-driver-engine`'s
//! `FrameSession`, which owns the socket, the response timeout and the
//! "a timeout is fatal" rule. The session-type I/O (`api_session.rs`,
//! `session.rs`, `banto_plc::dial_slmp` / `SlmpClient` /
//! `execute_slmp_batch_reads`, `banto_plc_write::SlmpWriteClient` /
//! `execute_slmp_writes`) is gone.
//!
//! - [`SlmpDriver`] / [`driver`] (`base.rs`): the [`banto_driver_api::DriverBase`]
//!   implementation - identity, `validate_config`, the `cpu_type` setting
//!   (#605), `parse_address` over `Address::parse_slmp`, `offset_address` /
//!   `address_help` ported from the TypeScript, and the connection test's
//!   `probe` point (`D0`, R2b-3a-2 / Q11).
//! - `frame.rs` builds and checks the SLMP 4E binary frames (bulk read /
//!   write, word / bit, Q / R); `slmp_parity.rs` (tests) checks every frame
//!   byte for byte against the `slmp` crate.
//! - `frame_conn.rs` (design §6.2): the [`banto_driver_api::FrameDriver`] /
//!   `FrameConnection`: `plan_read` / `decode_read`, and `write_next` (word
//!   writes, and the bit-in-word read-modify-write as three steps). The
//!   canonical address is turned back into a `banto_plc::Address` privately
//!   inside this crate (`request.rs`, which also holds the read / write
//!   request gates and the last-device-number limit).
//! - [`classify_read`] / [`classify_write`] (`classify.rs`): the §3.4 table
//!   from `PlcError` / `PlcWriteError` to `ItemError` / `FatalError`
//!   (`frame_conn.rs` maps the write errors it makes through it).
//!
//! ## Dependency direction
//!
//! This crate -> `banto-broker` (only for the shared `parse_word_order`),
//! never the reverse: the broker does not depend on any driver crate, not
//! even as a dev-dependency. The tests that pin down SLMP's behaviour
//! *through* the broker live in this crate's `tests/broker.rs`.

use banto_plc::SlmpCpu;

mod base;
mod classify;
mod frame;
mod frame_conn;
mod request;
#[cfg(test)]
mod slmp_parity;

pub use base::{driver, SlmpDriver};
pub use classify::{classify_read, classify_write};

/// The protocol string (`PlcConnection::protocol`) this driver serves - the
/// id `banto-drivers` registers [`driver`] under.
pub const PROTOCOL: &str = "slmp";

/// #605: `"Q"` だけ [`SlmpCpu::Q`]。それ以外は [`SlmpCpu::R`]（保存の検証が Q / R
/// 以外を断るので、それ以外は手で書き換えた行だけ）。
fn parse_cpu_type(value: &str) -> SlmpCpu {
    match value {
        "Q" => SlmpCpu::Q,
        _ => SlmpCpu::R,
    }
}
