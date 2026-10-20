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
//! - **R1-3b scope**: `broker` is still `DriverTable::builtin()` (the
//!   bridge until R1-4a moves the drivers into their own crates) and
//!   `registry` is empty until R1-4b registers the built-in drivers.

use banto_broker::DriverTable;
use banto_collect::ClientFactory;
use banto_driver_api::DriverRegistry;

/// Everything a host injects to give the engine its drivers.
pub struct Builtin {
    /// The driver registry (design §6.1). **Empty in R1-3b**; R1-4b
    /// registers `modbus-tcp` / `slmp` here as `HostBound` entries.
    pub registry: DriverRegistry,
    /// The broker's protocol -> dial function table (`"slmp"`,
    /// `"modbus-tcp"`). R1-4a replaces this bridge with the driver crates'
    /// own `dial` functions.
    pub broker: DriverTable,
    /// The factory for direct (non-broker) collection clients - exactly
    /// `banto_collect::default_client_factory()`.
    pub client_factory: ClientFactory,
}

/// The built-in drivers (Modbus TCP and SLMP).
pub fn builtin() -> Builtin {
    Builtin {
        registry: DriverRegistry::new(),
        broker: DriverTable::builtin(),
        client_factory: banto_collect::default_client_factory(),
    }
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

    #[test]
    fn registry_is_empty_until_r1_4b() {
        assert!(builtin().registry.is_empty());
    }
}
