//! Modbus TCP through the broker (R1-4a, docs/banto-driver-api-design.md
//! §4.4): this was one of `banto-broker`'s own unit tests while the Modbus
//! driver lived there. It moved here with the driver because the broker no
//! longer has (or depends on) a real driver; it now goes through the public
//! `BrokerSupervisor::spawn` with a table holding only this crate's `dial`.
//! The assertions are unchanged.

use banto_broker::{BackoffConfig, BrokerSupervisor, DialFn, DriverTable};
use banto_driver_api::DriverId;
use banto_plc_write::modbus::simulator::Simulator;
use banto_tags::PlcConnection;

/// The broker table with just this driver, under `"modbus-tcp"` (what the
/// old test's `DriverTable::builtin()` dialed a `"modbus-tcp"` connection
/// with).
fn modbus_table() -> DriverTable {
    DriverTable::new(vec![(
        DriverId::new(banto_driver_modbus::PROTOCOL).expect("valid driver id"),
        banto_driver_modbus::dial as DialFn,
    )])
    .expect("one entry")
}

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

#[tokio::test]
async fn supervisor_hands_out_a_handle_per_modbus_tcp_connection() {
    let sim = Simulator::start().await;
    let connections = [conn(7, "modbus-tcp", "127.0.0.1", sim.addr.port())];
    let supervisor =
        BrokerSupervisor::spawn(&connections, BackoffConfig::default(), modbus_table())
            .expect("a modbus-tcp connection should spawn");
    assert_eq!(supervisor.connection_count(), 1);
    assert!(supervisor.handle(7).is_some());
    assert!(supervisor.handle(999).is_none());
    supervisor.shutdown().await;
}
