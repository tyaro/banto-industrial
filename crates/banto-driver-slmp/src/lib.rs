//! `banto-driver-slmp`: the MELSEC SLMP driver (docs/banto-driver-api-design.md
//! §2.3, §4.4; docs/banto-hub-driver-io-design.md §4).
//!
//! ## What this crate is
//!
//! Until R1-4a the SLMP driver lived inside `banto-broker`; R1-4a moved it
//! here. R2b-3a-1 added the production [`banto_driver_api::SessionDriver`],
//! and **R2b-3a-2 made it the only way in**: the composition root
//! (`banto-drivers`) registers [`driver`] as `DriverEntry::session`, and the
//! broker dials it through the registry with the connection's `Settings`
//! and a `ConnectContext`. The former broker table entry (`dial`,
//! `DriverDial` with the SLMP CPU type for `BrokerHandle::slmp_cpu`) and the
//! `banto_broker::BrokerSession` impl are gone; the broker now compares a
//! protocol-independent settings fingerprint instead of the CPU (#605).
//!
//! - [`SlmpDriver`] / [`driver`] (`base.rs`): the [`banto_driver_api::DriverBase`]
//!   implementation - identity, `validate_config`, the `cpu_type` setting
//!   (#605), `parse_address` over `Address::parse_slmp`, `offset_address` /
//!   `address_help` ported from the TypeScript, and the connection test's
//!   `probe` point (`D0`, R2b-3a-2 / Q11).
//! - `api_session.rs`: the [`banto_driver_api::SessionDriver`] / `Session`
//!   (design §4.1). `connect` builds the `SlmpConfig` from `Settings` +
//!   `ConnectContext` and dials with `banto_plc::dial_slmp`; reads and writes
//!   run on `session::SlmpSession` (the `banto_plc` / `banto_plc_write` SLMP
//!   planner/executor pair) with the typed `PlcError` -> `FatalError` /
//!   `ItemError` mapping. The canonical address is turned back into a
//!   `banto_plc::Address` privately inside this crate, and a `bit`-addressed
//!   `bit` write (`D100.5`) is the read-modify-write `BitInWord` here (the
//!   decision the Hub used to make).
//! - [`classify_read`] / [`classify_write`] (`classify.rs`): the §3.4 table
//!   from `PlcError` / `PlcWriteError` to `ItemError` / `FatalError`.
//! - R2b-3d (§6.2, Q4), first half: `frame.rs` builds and checks the SLMP 4E
//!   binary frames itself (bulk read / write, word / bit, Q / R), and
//!   `frame_conn.rs` is the [`banto_driver_api::FrameDriver`] /
//!   `FrameConnection` with `plan_read` / `decode_read` over them, run by the
//!   real engine in `tests/frame_conformance.rs`. `slmp_parity.rs` (tests)
//!   checks every frame byte for byte against the `slmp` crate. Not yet
//!   registered: the broker still dials the session above, and
//!   `write_next` comes with the switch-over.
//!
//! ## Dependency direction
//!
//! This crate -> `banto-broker` (only for the shared `parse_word_order`),
//! never the reverse: the broker does not depend on any driver crate, not
//! even as a dev-dependency. The tests that pin down SLMP's behaviour
//! *through* the broker live in this crate's `tests/broker.rs`.

use banto_plc::SlmpCpu;

mod api_session;
mod base;
mod classify;
mod frame;
mod frame_conn;
mod session;
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
