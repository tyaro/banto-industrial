//! banto-drivers: the set of drivers a host (banto-hub's bin, its shell, a
//! test) injects into the engine (docs/banto-driver-api-design.md §6.3,
//! §6.4, R1-3b).
//!
//! ## Why this crate exists
//!
//! `banto-hub-core` used to reach for `DriverTable::builtin()` and
//! `banto_collect::default_client_factory()` itself, so adding or replacing a
//! driver meant editing the engine. R1-3b turns that around: the engine
//! (`HubSessions`, `HubConfig`) receives a `HubDrivers` by injection and
//! never names a concrete driver; the composition root builds it from
//! [`builtin`]. This crate is that composition root's only dependency on
//! "which drivers exist".
//!
//! ## Rules
//!
//! - **Only a host may depend on this crate** - a bin, or test code. Engine
//!   library code (`banto-hub-core`'s `src/*.rs` outside `src/bin/` and
//!   `#[cfg(test)]`) must not reference it (2026-10-20 owner decision, §6.4).
//! - **No copies.** [`Builtin::client_factory`] *is*
//!   `banto_collect::default_client_factory()`: re-building the
//!   `ClientSpec` -> client mapping here would silently drop fields exactly
//!   like issues #334 / #605 did. Tripwire tests below only assert the
//!   composition is not swapped out.
//! - **R1-4a scope**: `broker` is built here from the driver crates' own
//!   `dial` functions (`banto-driver-slmp`, `banto-driver-modbus`) - the
//!   broker itself no longer contains any driver, nor the R1-3a bridge
//!   `DriverTable::builtin()`. `registry` is still empty until R1-4b
//!   registers the built-in drivers.
//! - `examples/real_plc_verify.rs` (the real-PLC check, moved here from
//!   `banto-broker` by R1-4a) spawns its broker with `builtin().broker`, so
//!   it exercises exactly the table a host injects.

use banto_broker::{DialFn, DriverTable};
use banto_collect::ClientFactory;
use banto_driver_api::{DriverId, DriverRegistry};

/// Everything a host injects to give the engine its drivers.
pub struct Builtin {
    /// The driver registry (design §6.1). **Empty in R1-3b**; R1-4b
    /// registers `modbus-tcp` / `slmp` here as `HostBound` entries.
    pub registry: DriverRegistry,
    /// The broker's protocol -> dial function table: `"slmp"`
    /// (`banto_driver_slmp::dial`) and `"modbus-tcp"`
    /// (`banto_driver_modbus::dial`), in that order (R1-4a; the same entries
    /// the broker's former built-in table held).
    pub broker: DriverTable,
    /// The factory for direct (non-broker) collection clients - exactly
    /// `banto_collect::default_client_factory()`.
    pub client_factory: ClientFactory,
}

/// The built-in drivers (Modbus TCP and SLMP).
pub fn builtin() -> Builtin {
    Builtin {
        registry: DriverRegistry::new(),
        broker: broker_table(),
        client_factory: banto_collect::default_client_factory(),
    }
}

/// The broker table of the built-in drivers (see [`Builtin::broker`]).
fn broker_table() -> DriverTable {
    let id = |s: &str| DriverId::new(s).expect("builtin driver ids are valid");
    DriverTable::new(vec![
        (
            id(banto_driver_slmp::PROTOCOL),
            banto_driver_slmp::dial as DialFn,
        ),
        (
            id(banto_driver_modbus::PROTOCOL),
            banto_driver_modbus::dial as DialFn,
        ),
    ])
    .expect("builtin driver ids are distinct")
}

#[cfg(test)]
mod tests {
    use super::*;
    use banto_collect::{ClientProtocol, ClientSpec};
    use banto_plc::slmp::SlmpCpu;
    use banto_plc::WordOrder;
    use std::collections::BTreeSet;
    use std::time::Duration;

    fn spec(protocol: ClientProtocol) -> ClientSpec {
        // Constructing a client never connects; the address is never dialed.
        ClientSpec {
            connection_key: "conn:1".to_string(),
            protocol,
            host: "127.0.0.1".to_string(),
            port: 1,
            unit_id: 1,
            connect_timeout: Duration::from_millis(10),
            response_timeout: Duration::from_millis(10),
            word_order: WordOrder::default(),
            cpu: SlmpCpu::R,
        }
    }

    /// Tripwire 3 (design §6.3): the factory yields a client for both
    /// protocols (content is covered by banto-collect's own tests).
    #[test]
    fn client_factory_builds_a_client_for_both_protocols() {
        let factory = builtin().client_factory;
        for protocol in [ClientProtocol::ModbusTcp, ClientProtocol::Slmp] {
            let _client: Box<dyn banto_plc::PlcClient> = factory(&spec(protocol));
        }
    }

    /// Tripwire 5 (design §6.3): the broker table is exactly today's two
    /// drivers.
    #[test]
    fn broker_table_ids_are_slmp_and_modbus_tcp() {
        let ids: BTreeSet<String> = builtin()
            .broker
            .ids()
            .map(|id| id.as_str().to_string())
            .collect();
        let want: BTreeSet<String> = ["slmp", "modbus-tcp"].map(String::from).into();
        assert_eq!(ids, want);
    }

    /// The table holds exactly the two drivers the broker's former `const
    /// DRIVERS` (and the R1-3a bridge `DriverTable::builtin()`) held, in the
    /// same order (moved from `banto-broker` by R1-4a).
    #[test]
    fn builtin_table_registers_exactly_slmp_and_modbus_tcp() {
        let table = builtin().broker;
        let ids: Vec<&str> = table.ids().map(DriverId::as_str).collect();
        assert_eq!(ids, ["slmp", "modbus-tcp"]);
        assert!(table.is_supported("slmp"));
        assert!(table.is_supported("modbus-tcp"));
        for other in ["virtual", "bogus-protocol", "", "SLMP", "modbus"] {
            assert!(
                !table.is_supported(other),
                "{other:?} must not be supported"
            );
            assert!(table.dial_for(other).is_none());
        }
    }

    /// The SLMP entry records the connection's CPU type for
    /// `BrokerHandle::slmp_cpu` (#605) exactly as the old
    /// `spawn_slmp_driver` did; the Modbus entry records none (moved from
    /// `banto-broker` by R1-4a, through the table a host actually injects).
    #[test]
    fn builtin_dials_carry_the_slmp_cpu_only_for_slmp() {
        let table = builtin().broker;
        let slmp = table.dial_for("slmp").expect("slmp entry");
        let mut c = conn(1, "slmp", 5000);
        c.cpu_type = "Q".to_string();
        assert_eq!(slmp(&c, 5000).slmp_cpu, Some(SlmpCpu::Q));
        c.cpu_type = "R".to_string();
        assert_eq!(slmp(&c, 5000).slmp_cpu, Some(SlmpCpu::R));
        let modbus = table.dial_for("modbus-tcp").expect("modbus-tcp entry");
        let dial = modbus(&conn(2, "modbus-tcp", 502), 502);
        assert_eq!(dial.slmp_cpu, None);
    }

    fn conn(id: i64, protocol: &str, port: u16) -> banto_tags::PlcConnection {
        banto_tags::PlcConnection {
            id,
            name: format!("conn-{id}"),
            protocol: protocol.to_string(),
            host: "127.0.0.1".to_string(),
            port: port as i64,
            unit_id: 1,
            enabled: true,
            simulation: false,
            word_order: "low_high".to_string(),
            cpu_type: "R".to_string(),
            database: None,
            username: None,
            password: None,
        }
    }

    #[test]
    fn registry_is_empty_until_r1_4b() {
        assert!(builtin().registry.is_empty());
    }
}
