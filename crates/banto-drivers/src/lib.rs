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
//!   library code (`banto-hub-core`'s `src/*.rs` outside `#[cfg(test)]`) must
//!   not reference it (2026-10-20 owner decision, §6.4). Since R1-7a the
//!   `banto-hub` bin lives in `banto-hub-host`, which is the only normal
//!   dependent on the Hub side; `banto-hub-core` keeps this crate as a
//!   dev-dependency only (until R2b-0).
//! - **No copies.** [`Builtin::client_factory`] *is*
//!   `banto_collect::default_client_factory()`: re-building the
//!   `ClientSpec` -> client mapping here would silently drop fields exactly
//!   like issues #334 / #605 did. Tripwire tests below only assert the
//!   composition is not swapped out. (It stays until R2b-3a-3 moves the
//!   collection plan to `TagSpec`, docs/banto-hub-driver-io-design.md §4.3.)
//! - **R2b-3a-2 scope** (docs/banto-hub-driver-io-design.md §4.2): `registry`
//!   holds Modbus TCP and SLMP as `DriverKind::Session` entries (their
//!   `SessionDriver`: identity, settings validation, address functions *and*
//!   the I/O). The broker dials through this registry - the former side
//!   table `Builtin::broker` (`banto_broker::DriverTable` of `dial`
//!   functions) and R1's `DriverKind::HostBound` are gone. The `Settings` a
//!   driver gets come from `banto_tags::settings_to_api` over the
//!   connection's `settings` (connection-settings design §3.5).
//! - `examples/real_plc_verify.rs` (the real-PLC check) spawns its broker
//!   with `builtin().registry`, so it exercises exactly the drivers a host
//!   injects.

use banto_collect::ClientFactory;
use banto_driver_api::{DriverEntry, DriverRegistry};

/// Everything a host injects to give the engine its drivers.
pub struct Builtin {
    /// The driver registry (design §6.1): `modbus-tcp` and `slmp` as
    /// session-type entries (R2b-3a-2; `HostBound` in R1-4b to R2b-3a-1).
    /// The broker dials through it.
    pub registry: DriverRegistry,
    /// The factory for direct (non-broker) collection clients - exactly
    /// `banto_collect::default_client_factory()`.
    pub client_factory: ClientFactory,
}

/// The built-in drivers (Modbus TCP and SLMP).
pub fn builtin() -> Builtin {
    Builtin {
        registry: registry(),
        client_factory: banto_collect::default_client_factory(),
    }
}

/// The registry of the built-in drivers (see [`Builtin::registry`]).
fn registry() -> DriverRegistry {
    let mut registry = DriverRegistry::new();
    for driver in [banto_driver_slmp::driver(), banto_driver_modbus::driver()] {
        registry
            .register(DriverEntry::session(driver))
            .expect("builtin driver ids are distinct");
    }
    registry
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

    use banto_driver_api::{DriverId, DriverKind};
    use banto_tags::ALLOWED_PROTOCOLS;

    fn session_ids(registry: &DriverRegistry) -> BTreeSet<String> {
        registry
            .ids()
            .filter(|id| {
                matches!(
                    registry.get(id.as_str()).map(DriverEntry::kind),
                    Some(DriverKind::Session(_))
                )
            })
            .map(|id| id.as_str().to_string())
            .collect()
    }

    /// The registry holds exactly the two built-ins, both session-type
    /// (R2b-3a-2), and each entry's descriptor id is the key it is found
    /// under.
    #[test]
    fn registry_holds_modbus_tcp_and_slmp_as_session_drivers() {
        let registry = builtin().registry;
        assert_eq!(registry.len(), 2);
        let want: BTreeSet<String> = ["slmp", "modbus-tcp"].map(String::from).into();
        assert_eq!(session_ids(&registry), want);
        for id in ["slmp", "modbus-tcp"] {
            let entry = registry.get(id).expect("registered");
            assert_eq!(entry.descriptor().id.as_str(), id);
            assert!(matches!(entry.kind(), DriverKind::Session(_)));
        }
        assert_eq!(
            registry.get("modbus-tcp").unwrap().descriptor().id.as_str(),
            banto_driver_modbus::PROTOCOL
        );
        assert_eq!(
            registry.get("slmp").unwrap().descriptor().id.as_str(),
            banto_driver_slmp::PROTOCOL
        );
    }

    /// Tripwire 1 (design §6.3; re-read by R2b-1f, connection-settings
    /// design §4.1): every id `builtin()` registers is in the *legacy* rule
    /// list `ALLOWED_PROTOCOLS`. `builtin()` is the registry ChronoGazer
    /// shares (`modbus-tcp`, `slmp`), and ChronoGazer still validates with
    /// that legacy list until R2b-7, so an id outside it could never be
    /// looked up from a ChronoGazer connection row. It is not the Hub's rule:
    /// the Hub's source of truth is its registry (`push`/`postgres`/`virtual`
    /// are registered by the host; see `host/tests/registry_tripwire.rs`).
    #[test]
    fn builtin_registry_ids_are_in_the_legacy_protocol_list() {
        let registry = builtin().registry;
        assert!(!registry.is_empty());
        for id in registry.ids() {
            assert!(
                ALLOWED_PROTOCOLS.contains(&id.as_str()),
                "{id} is not in banto_tags::ALLOWED_PROTOCOLS"
            );
        }
    }

    /// Tripwire 2 (design §6.3; rewritten by R2b-3a-2,
    /// docs/banto-hub-driver-io-design.md §4.2): every entry `builtin()`
    /// registers is a frame- or session-type driver - one the broker dials
    /// itself (the broker table and `HostBound` are gone, so "the broker's ids
    /// == the registry's HostBound ids" became this), and the broker manages
    /// exactly those ids.
    #[test]
    fn every_builtin_entry_is_a_driver_the_broker_dials() {
        let registry = builtin().registry;
        for id in registry.ids() {
            let entry = registry.get(id.as_str()).expect("registered");
            assert!(
                matches!(entry.kind(), DriverKind::Session(_) | DriverKind::Frame(_)),
                "{id}: {:?}",
                entry.kind()
            );
            assert!(
                banto_broker::is_broker_managed(&registry, id.as_str()),
                "{id}"
            );
        }
    }

    /// Each built-in names a connection-test point its own grammar accepts
    /// (R2b-3a-2, Q11): the Hub's connection test reads it without knowing the
    /// protocol.
    #[test]
    fn every_builtin_names_a_probe_its_grammar_accepts() {
        let registry = builtin().registry;
        for id in registry.ids() {
            let base = registry.get(id.as_str()).unwrap().base();
            let (text, kind) = base.probe().unwrap_or_else(|| panic!("{id}: no probe"));
            base.parse_address(&text, &kind)
                .unwrap_or_else(|e| panic!("{id}: {text}: {e}"));
        }
    }

    /// Tripwire 4 (design §6.3): every descriptor id satisfies the
    /// `DriverId` grammar `[a-z0-9][a-z0-9-]*` (checked by re-parsing the
    /// text, independent of how the id was built).
    #[test]
    fn descriptor_ids_satisfy_the_driver_id_grammar() {
        let registry = builtin().registry;
        for id in registry.ids() {
            let text = id.as_str();
            let entry = registry.get(text).expect("registered");
            assert_eq!(entry.descriptor().id.as_str(), text);
            let parsed = DriverId::new(text).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(&parsed, id);
            let mut bytes = text.bytes();
            let first = bytes.next().expect("not empty");
            assert!(
                first.is_ascii_lowercase() || first.is_ascii_digit(),
                "{text}"
            );
            assert!(
                bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
                "{text}"
            );
        }
    }

    // ---- settings_to_api(settings_from_flat(..)) (connection-settings design §3.5) ----

    use banto_driver_api::Settings;
    use banto_tags::settings_json::legacy::settings_from_flat;
    use banto_tags::{settings_to_api, PlcConnectionInput};

    fn pairs(settings: &Settings) -> Vec<(&str, &str)> {
        let mut pairs: Vec<(&str, &str)> = settings
            .iter()
            .map(|s| (s.key.as_str(), s.value.as_str()))
            .collect();
        pairs.sort();
        pairs
    }

    fn input(protocol: &str, port: i64) -> PlcConnectionInput {
        PlcConnectionInput {
            name: "c".to_string(),
            protocol: protocol.to_string(),
            host: "127.0.0.1".to_string(),
            port,
            unit_id: 1,
            enabled: true,
            simulation: false,
            word_order: "low_high".to_string(),
            cpu_type: None,
            database: None,
            username: None,
            secrets: Default::default(),
            settings: None,
        }
    }

    fn settings_of(input: &PlcConnectionInput) -> Settings {
        settings_to_api(&settings_from_flat(input))
    }

    #[test]
    fn settings_from_a_modbus_connection() {
        let mut c = input("modbus-tcp", 502);
        c.unit_id = 7;
        c.word_order = "high_low".to_string();
        c.cpu_type = Some("Q".to_string()); // a stray value: Modbus does not carry it
        assert_eq!(
            pairs(&settings_of(&c)),
            [
                ("host", "127.0.0.1"),
                ("port", "502"),
                ("unit_id", "7"),
                ("word_order", "high_low"),
            ]
        );
    }

    /// SLMP carries `cpu_type` and (unlike R1's `settings_from_connection`) no
    /// `unit_id`: the driver never read it (design §3.2).
    #[test]
    fn settings_from_an_slmp_connection_carry_the_cpu_type_and_no_unit_id() {
        let mut c = input("slmp", 5000);
        c.cpu_type = Some("Q".to_string());
        assert_eq!(
            pairs(&settings_of(&c)),
            [
                ("cpu_type", "Q"),
                ("host", "127.0.0.1"),
                ("port", "5000"),
                ("word_order", "low_high"),
            ]
        );
    }

    /// What the host does with the pair: the registry's `validate_config`
    /// accepts a valid row's settings, and reports (not panics on) a row
    /// whose settings are out of range.
    #[test]
    fn registered_drivers_validate_settings_from_flat() {
        let registry = builtin().registry;
        for (protocol, port) in [("modbus-tcp", 502), ("slmp", 5000)] {
            let driver = registry.get(protocol).unwrap().base();
            let ok = settings_of(&input(protocol, port));
            assert_eq!(driver.validate_config(&ok), vec![], "{protocol}");

            let mut bad = input(protocol, 70_000);
            bad.host = String::new();
            let mut keys: Vec<String> = driver
                .validate_config(&settings_of(&bad))
                .into_iter()
                .map(|e| e.key)
                .collect();
            keys.sort();
            assert_eq!(keys, ["host", "port"], "{protocol}");
        }
        let mut bad_cpu = input("slmp", 5000);
        bad_cpu.cpu_type = Some("L".to_string());
        let errors = registry
            .get("slmp")
            .unwrap()
            .base()
            .validate_config(&settings_of(&bad_cpu));
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].key, "cpu_type");
    }
}
