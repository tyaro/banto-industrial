//! `banto-driver-modbus`: the Modbus TCP driver for `banto-broker` - R1's
//! thin adapter (docs/banto-driver-api-design.md §2.3, §4.4, R1-4a).
//!
//! ## What this crate is
//!
//! Until R1-4a the Modbus TCP driver (#131) lived inside `banto-broker`
//! (`modbus_driver.rs` plus `modbus_config_for` in `lib.rs`), and the
//! broker's own `DriverTable::builtin()` registered it. R1-4a moved both here
//! with `git mv`, unchanged, and deleted that bridge: the broker now only
//! knows the [`banto_broker::DriverTable`] it is handed, and the composition
//! root (`banto-drivers`) builds that table from this crate's [`dial`].
//! Nothing about *how* a Modbus session dials, reads or writes changed in the
//! move.
//!
//! - [`dial`] is the table entry ([`banto_broker::DialFn`]): it builds the
//!   connection's [`ModbusTcpConfig`] (`modbus_config_for`) and wraps it as
//!   a [`banto_broker::Connector`] (`session::connector`). No SLMP CPU type.
//!   No I/O - the broker calls it under its directory lock and runs the
//!   connector later, in the connection's own task.
//! - The session itself (`session::ModbusSession`) owns one `TcpStream`
//!   and the **one** transaction-id counter shared by reads and writes - see
//!   that type's doc, the most safety-critical property of this driver.
//!
//! ## Dependency direction
//!
//! This crate -> `banto-broker` (the [`banto_broker::BrokerSession`] trait and
//! [`banto_broker::DriverDial`]), never the reverse: the broker does not
//! depend on any driver crate, not even as a dev-dependency (cycles and
//! double compilation, §2.3). The broker's own unit tests run on an
//! in-memory fake; the test that pins down Modbus through the broker lives in
//! this crate's `tests/broker.rs`.
//!
//! ## The `banto-driver-api` side (R1-4b)
//!
//! - [`ModbusDriver`] / [`driver`] (`base.rs`): the [`banto_driver_api::DriverBase`]
//!   implementation - identity, `validate_config`, `parse_address`
//!   over `Address::parse`, and `offset_address` / `address_help` ported from the
//!   TypeScript (`continuousRegistration.ts`, `tagAddressHelp.ts`). The
//!   composition root registers it as `DriverKind::HostBound`: the I/O stays
//!   behind [`dial`].
//! - [`classify_read`] / [`classify_write`] (`classify.rs`): the §3.4 table
//!   from `PlcError` / `PlcWriteError` to `ItemError` / `FatalError`.
//!
//! ## The production `Session` (R2b-3a-1)
//!
//! `api_session.rs` implements [`banto_driver_api::SessionDriver`] for
//! [`ModbusDriver`] (docs/banto-hub-driver-io-design.md §4.1): `connect` builds
//! the config from `Settings` + `ConnectContext` and dials as before, and the
//! `Session` reads/writes on the same `ModbusSession` (one shared transaction
//! counter) with the typed `PlcError` -> `FatalError` / `ItemError` mapping
//! (no more text round trip). The canonical address is turned back into a
//! `banto_plc::Address` privately inside this crate. This is step 3a-1: the
//! registration is unchanged (`HostBound`, broker + [`dial`] as above);
//! 3a-2 switches the broker and the registry to this `Session`.
//!
//! Neither of the two above is on the production path in R1 (design §3.3, §6.2): only the
//! tripwire and conformance tests read them.

use banto_broker::{parse_word_order, DriverDial};
use banto_plc::ModbusTcpConfig;
use banto_tags::PlcConnection;

mod api_session;
mod base;
mod classify;
mod session;

pub use base::{driver, ModbusDriver};
pub use classify::{classify_read, classify_write};
#[doc(hidden)]
pub use session::connector;

/// The protocol string (`PlcConnection::protocol`) this driver serves - the
/// id `banto-drivers` registers [`dial`] under.
pub const PROTOCOL: &str = "modbus-tcp";

/// [`banto_broker::DialFn`] for `"modbus-tcp"` (#131, 2026-09-01): build this
/// connection's [`ModbusTcpConfig`] (see `modbus_config_for`) and wrap it
/// as a [`banto_broker::Connector`] - what the former
/// `banto_broker::dial_modbus` did. No SLMP CPU type. `port` is the broker's
/// already-validated `u16`.
pub fn dial(conn: &PlcConnection, port: u16) -> DriverDial {
    DriverDial {
        connector: session::connector(modbus_config_for(conn, port)),
        slmp_cpu: None,
    }
}

/// Build [`dial`]'s [`ModbusTcpConfig`] from `conn` - the Modbus twin of
/// `banto-driver-slmp`'s `slmp_config_for`, factored out for the same
/// test-without-a-socket reason (moved from `banto-broker` by R1-4a,
/// unchanged).
///
/// `unit_id`: `banto_tags::PlcConnection::unit_id` is registry-validated to
/// `0..=255` at input time (`banto-tags`'s `MIN_UNIT_ID..=MAX_UNIT_ID`), so
/// `conn.unit_id: i64` always fits `u8` for any row that passed normal CRUD -
/// the `unwrap_or_else` fallback to `ModbusTcpConfig::default().unit_id` is
/// defense-in-depth for a hand-edited/pre-migration row, exactly the same
/// fail-open posture [`parse_word_order`] documents for `word_order`, not a
/// path expected in practice.
///
/// `word_order`: reuses [`parse_word_order`] (the *same* helper
/// `banto-driver-slmp`'s `slmp_config_for` uses, not a duplicate - it stays in
/// `banto-broker` for exactly that reason) even though
/// [`ModbusTcpConfig::default`]'s own `word_order` is
/// [`banto_plc::WordOrder::HighLow`] - the opposite of
/// `SlmpConfig::default`'s `WordOrder::LowHigh`. `parse_word_order`'s
/// fallback to `WordOrder::LowHigh` on an unrecognized string is
/// unconditional regardless of which protocol calls it; this matches
/// `banto_plc_write::modbus::ModbusWriteClient`'s already-documented behavior
/// of taking the connection's configured order through this same shared
/// `word_order` column, so a hand-edited/pre-migration Modbus row fails open
/// to `LowHigh` too, not to `ModbusTcpConfig::default()`'s `HighLow`.
///
/// `#[doc(hidden)] pub` (R1-5) only for this crate's `tests/conformance.rs`
/// (the dev-only `LegacySession` builds its config exactly as [`dial`]
/// does); not part of the crate's API.
#[doc(hidden)]
pub fn modbus_config_for(conn: &PlcConnection, port: u16) -> ModbusTcpConfig {
    ModbusTcpConfig {
        host: conn.host.clone(),
        port,
        unit_id: u8::try_from(conn.unit_id).unwrap_or_else(|_| ModbusTcpConfig::default().unit_id),
        word_order: parse_word_order(&conn.word_order),
        ..ModbusTcpConfig::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Modbus entry records no SLMP CPU type. (R1-4a: the Modbus half of
    /// the broker's former `builtin_dials_carry_the_slmp_cpu_only_for_slmp`;
    /// the SLMP half is `banto-driver-slmp`'s `dial_carries_the_slmp_cpu`.)
    #[test]
    fn dial_carries_no_slmp_cpu() {
        let conn = PlcConnection {
            id: 2,
            name: "conn-2".to_string(),
            protocol: "modbus-tcp".to_string(),
            host: "127.0.0.1".to_string(),
            port: 502,
            unit_id: 1,
            enabled: true,
            simulation: false,

            word_order: "low_high".to_string(),
            cpu_type: "R".to_string(),
            database: None,
            username: None,
            settings: Default::default(),
            settings_version: 1,
        };
        assert_eq!(dial(&conn, 502).slmp_cpu, None);
    }
}
