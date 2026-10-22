//! `banto-driver-slmp`: the MELSEC SLMP driver for `banto-broker` - R1's thin
//! adapter (docs/banto-driver-api-design.md §2.3, §4.4, R1-4a).
//!
//! ## What this crate is
//!
//! Until R1-4a the SLMP driver lived inside `banto-broker` (`slmp_driver.rs`
//! plus `slmp_config_for` in `lib.rs`), and the broker's own
//! `DriverTable::builtin()` registered it. R1-4a moved both here with
//! `git mv`, unchanged, and deleted that bridge: the broker now only knows
//! the [`banto_broker::DriverTable`] it is handed, and the composition root
//! (`banto-drivers`) builds that table from this crate's [`dial`]. Nothing
//! about *how* an SLMP session dials, reads or writes changed in the move.
//!
//! - [`dial`] is the table entry ([`banto_broker::DialFn`]): it builds the
//!   connection's [`SlmpConfig`] (`slmp_config_for`) and wraps it as a
//!   [`banto_broker::Connector`] (`session::connector`), recording the CPU
//!   type for `BrokerHandle::slmp_cpu` (#605). No I/O - the broker calls it
//!   under its directory lock and runs the connector later, in the
//!   connection's own task.
//! - The session itself (`session::SlmpSession`) owns one bare
//!   `slmp::SLMPClient` and forwards to `banto_plc` / `banto_plc_write`'s
//!   SLMP planner/executor pair - see that module's doc.
//!
//! ## Dependency direction
//!
//! This crate -> `banto-broker` (the [`banto_broker::BrokerSession`] trait and
//! [`banto_broker::DriverDial`]), never the reverse: the broker does not
//! depend on any driver crate, not even as a dev-dependency (cycles and
//! double compilation, §2.3). The broker's own unit tests run on an
//! in-memory fake; the tests that pin down SLMP's behaviour *through* the
//! broker live in this crate's `tests/broker.rs`.
//!
//! ## The `banto-driver-api` side (R1-4b)
//!
//! - [`SlmpDriver`] / [`driver`] (`base.rs`): the [`banto_driver_api::DriverBase`]
//!   implementation - identity, `validate_config`, the `cpu` setting (#605), `parse_address`
//!   over `Address::parse_slmp`, and `offset_address` / `address_help` ported from the
//!   TypeScript (`continuousRegistration.ts`, `tagAddressHelp.ts`). The
//!   composition root registers it as `DriverKind::HostBound`: the I/O stays
//!   behind [`dial`].
//! - [`classify_read`] / [`classify_write`] (`classify.rs`): the §3.4 table
//!   from `PlcError` / `PlcWriteError` to `ItemError` / `FatalError`.
//!
//! ## The production `Session` (R2b-3a-1)
//!
//! `api_session.rs` implements [`banto_driver_api::SessionDriver`] for
//! [`SlmpDriver`] (docs/banto-hub-driver-io-design.md §4.1): `connect` builds
//! the config from `Settings` + `ConnectContext` and dials as before, and the
//! `Session` reads/writes on the same `SlmpSession` with the typed
//! `PlcError` -> `FatalError` / `ItemError` mapping (no more text round trip).
//! The canonical address is turned back into a `banto_plc::Address` privately
//! inside this crate, and a `bit`-addressed `bit` write (`D100.5`) is the
//! read-modify-write `BitInWord` here (the Hub's decision, moved in). This is
//! step 3a-1: the registration is unchanged (`HostBound`, broker + [`dial`] as
//! above); 3a-2 switches the broker and the registry to this `Session`.
//!
//! Neither of the two above is on the production path in R1 (design §3.3, §6.2): only the
//! tripwire and conformance tests read them.

use banto_broker::{parse_word_order, DriverDial};
use banto_plc::{SlmpConfig, SlmpCpu};
use banto_tags::PlcConnection;

mod api_session;
mod base;
mod classify;
mod session;

pub use base::{driver, SlmpDriver};
pub use classify::{classify_read, classify_write};
#[doc(hidden)]
pub use session::connector;

/// The protocol string (`PlcConnection::protocol`) this driver serves - the
/// id `banto-drivers` registers [`dial`] under.
pub const PROTOCOL: &str = "slmp";

/// [`banto_broker::DialFn`] for `"slmp"`: build this connection's
/// [`SlmpConfig`] (see `slmp_config_for`) and wrap it as a
/// [`banto_broker::Connector`] - what the former `banto_broker::dial_slmp`
/// (and before R1-3a, `spawn_slmp_driver`) did, including recording the CPU
/// type for `BrokerHandle::slmp_cpu` (#605). `port` is the broker's
/// already-validated `u16`.
pub fn dial(conn: &PlcConnection, port: u16) -> DriverDial {
    let config = slmp_config_for(conn, port);
    let cpu = config.cpu;
    DriverDial {
        connector: session::connector(config),
        slmp_cpu: Some(cpu),
    }
}

/// Build [`dial`]'s [`SlmpConfig`] from `conn` - factored out so
/// `word_order_reflects_the_connections_own_setting_not_the_default` below
/// can exercise the field mapping directly, without spawning a real broker
/// task or dialing a socket. `port` is taken pre-validated (already an
/// [`Result::Ok`] `u16` by the time the caller has one) rather than
/// re-deriving it from `conn.port` here, matching the broker's existing
/// order of operations (moved from `banto-broker` by R1-4a, unchanged).
///
/// `#[doc(hidden)] pub` (R1-5) only for this crate's `tests/conformance.rs`
/// (the dev-only `LegacySession` builds its config exactly as [`dial`]
/// does); not part of the crate's API.
#[doc(hidden)]
pub fn slmp_config_for(conn: &PlcConnection, port: u16) -> SlmpConfig {
    SlmpConfig {
        host: conn.host.clone(),
        port,
        // P3-b (監査指摘 2026-08-12): previously `..SlmpConfig::default()`
        // alone, which fixed every session to `WordOrder::LowHigh` regardless
        // of what the connection actually specified - a device needing
        // `WordOrder::HighLow` would silently get byte-swapped u32/f32 values
        // with no error anywhere. See `parse_word_order`'s doc comment for
        // the fallback this now goes through.
        word_order: parse_word_order(&conn.word_order),
        // #605: `cpu_type` 列（migration 0021）。未認識の文字列は R
        // （この列を足す前の `SlmpConfig::default().cpu`）。CHECK があるので
        // 通常の CRUD では Q/R 以外は来ない。
        cpu: parse_cpu_type(&conn.cpu_type),
        ..SlmpConfig::default()
    }
}

/// #605: `"Q"` だけ [`SlmpCpu::Q`]。それ以外は [`SlmpCpu::R`]。
fn parse_cpu_type(value: &str) -> SlmpCpu {
    match value {
        "Q" => SlmpCpu::Q,
        _ => SlmpCpu::R,
    }
}

#[cfg(test)]
mod tests {
    use banto_plc::WordOrder;

    use super::*;

    fn conn(id: i64, protocol: &str, host: &str, port: u16) -> PlcConnection {
        PlcConnection {
            id,
            name: format!("conn-{id}"),
            protocol: protocol.to_string(),
            host: host.to_string(),
            port: port as i64,
            unit_id: 1,
            enabled: true,
            simulation: false,

            word_order: "low_high".to_string(),
            cpu_type: "R".to_string(),
            database: None,
            username: None,
            settings: Default::default(),
            settings_version: 1,
        }
    }

    // -----------------------------------------------------------------
    // P3-b (監査指摘 2026-08-12): word_order wiring - `slmp_config_for` must
    // reflect the connection's own `word_order`, not silently fix every
    // session to `SlmpConfig::default()`'s `WordOrder::LowHigh`.
    // -----------------------------------------------------------------

    /// The regression test for the audit finding itself: `slmp_config_for`
    /// must carry the connection's own `word_order` into the `SlmpConfig` it
    /// builds, not the old `..SlmpConfig::default()`-only shape that fixed
    /// every session to `WordOrder::LowHigh` regardless of what the
    /// connection specified.
    #[test]
    fn word_order_reflects_the_connections_own_setting_not_the_default() {
        let mut c = conn(1, "slmp", "127.0.0.1", 5007);

        c.word_order = "high_low".to_string();
        assert_eq!(
            slmp_config_for(&c, 5007).word_order,
            WordOrder::HighLow,
            "a connection asking for high_low must not silently get the default"
        );

        c.word_order = "low_high".to_string();
        assert_eq!(slmp_config_for(&c, 5007).word_order, WordOrder::LowHigh);
    }

    /// #605: `cpu_type` はセッションのフレーム形式になる。既定の行（`"R"`）は
    /// この列を足す前と同じ [`SlmpCpu::R`]。`"Q"` は Q フレーム。未知の文字列は
    /// R に倒す（CHECK を通らない手編集行向け）。
    #[test]
    fn slmp_config_for_maps_cpu_type_q_and_r() {
        let mut c = conn(1, "slmp", "10.0.0.5", 5007);
        assert_eq!(slmp_config_for(&c, 5007).cpu, SlmpCpu::R);

        c.cpu_type = "Q".to_string();
        assert_eq!(slmp_config_for(&c, 5007).cpu, SlmpCpu::Q);

        c.cpu_type = "L".to_string();
        assert_eq!(slmp_config_for(&c, 5007).cpu, SlmpCpu::R);
    }

    /// Every other [`SlmpConfig`] field `slmp_config_for` does not set stays
    /// exactly [`SlmpConfig::default`]'s value. CPU 種別は #605 で接続列に
    /// 出した。アクセスルートとタイマーは今も既定のまま（他局指定は範囲外）。
    #[test]
    fn slmp_config_for_only_overrides_host_port_word_order_and_cpu() {
        let c = conn(1, "slmp", "10.0.0.5", 5007);
        let config = slmp_config_for(&c, 5007);
        let default = SlmpConfig::default();
        assert_eq!(config.host, "10.0.0.5");
        assert_eq!(config.port, 5007);
        assert_eq!(config.word_order, WordOrder::LowHigh);
        assert_eq!(config.cpu, SlmpCpu::R);
        assert_eq!(config.connect_timeout, default.connect_timeout);
        assert_eq!(config.response_timeout, default.response_timeout);
        assert_eq!(config.network_id, default.network_id);
        assert_eq!(config.pc_id, default.pc_id);
        assert_eq!(config.io_id, default.io_id);
        assert_eq!(config.area_id, default.area_id);
        assert_eq!(config.serial_id, default.serial_id);
        assert_eq!(config.cpu_timer, default.cpu_timer);
    }

    /// The SLMP entry records the connection's CPU type for
    /// `BrokerHandle::slmp_cpu` (#605) exactly as the old
    /// `spawn_slmp_driver` did. (R1-4a: the SLMP half of the broker's former
    /// `builtin_dials_carry_the_slmp_cpu_only_for_slmp`; the Modbus half is
    /// `banto-driver-modbus`'s `dial_carries_no_slmp_cpu`.)
    #[test]
    fn dial_carries_the_slmp_cpu() {
        let mut c = conn(1, "slmp", "127.0.0.1", 5000);
        c.cpu_type = "Q".to_string();
        assert_eq!(dial(&c, 5000).slmp_cpu, Some(SlmpCpu::Q));
        c.cpu_type = "R".to_string();
        assert_eq!(dial(&c, 5000).slmp_cpu, Some(SlmpCpu::R));
    }
}
