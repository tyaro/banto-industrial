//! Modbus TCP through the broker (R1-4a, docs/banto-driver-api-design.md
//! §4.4): this was one of `banto-broker`'s own unit tests while the Modbus
//! driver lived there. R2b-3a-2 (docs/banto-hub-driver-io-design.md §4.2):
//! the broker dials through a `DriverRegistry` holding this crate's
//! `driver()` as a session-type entry and speaks `TagSpec` / `WriteItem`.
//! R2b-3c: the entry is frame-type now (`DriverEntry::frame`), wrapped by the
//! broker in the engine's `FrameSession`.

use std::sync::Arc;
use std::time::Duration;

use banto_broker::{BackoffConfig, BrokerConnectionStatus, BrokerSupervisor};
use banto_driver_api::{
    DataType, DriverBase, DriverEntry, DriverRegistry, TagKind, TagSpec, Value, WriteItem,
};
use banto_driver_modbus::ModbusDriver;
use banto_plc_write::modbus::simulator::Simulator;
use banto_tags::PlcConnection;

/// The registry with just this driver, as a frame-type entry.
fn modbus_registry() -> Arc<DriverRegistry> {
    let mut registry = DriverRegistry::new();
    registry
        .register(DriverEntry::frame(banto_driver_modbus::driver()))
        .expect("one entry");
    Arc::new(registry)
}

fn conn(id: i64, port: u16) -> PlcConnection {
    let serde_json::Value::Object(settings) = serde_json::json!({
        "host": "127.0.0.1",
        "port": port,
        "unit_id": 1,
        "word_order": "high_low",
    }) else {
        unreachable!()
    };
    PlcConnection::from_settings(
        id,
        format!("conn-{id}"),
        banto_driver_modbus::PROTOCOL.to_string(),
        true,
        false,
        settings,
        1,
    )
}

fn spec(text: &str, data_type: DataType) -> TagSpec {
    let kind = TagKind::numeric(data_type);
    TagSpec::new(ModbusDriver.parse_address(text, &kind).unwrap(), kind)
}

#[tokio::test]
async fn supervisor_hands_out_a_handle_per_modbus_tcp_connection() {
    let sim = Simulator::start().await;
    let connections = [conn(7, sim.addr.port())];
    let supervisor =
        BrokerSupervisor::spawn(&connections, BackoffConfig::default(), modbus_registry())
            .expect("a modbus-tcp connection should spawn");
    assert_eq!(supervisor.connection_count(), 1);
    assert!(supervisor.handle(7).is_some());
    assert!(supervisor.handle(999).is_none());
    supervisor.shutdown().await;
}

/// A write then a read over the one broker connection, through the
/// registry's frame driver (one socket for both directions, #337).
#[tokio::test]
async fn write_then_read_over_the_one_session() {
    let sim = Simulator::start().await;
    let supervisor = BrokerSupervisor::spawn(
        &[conn(1, sim.addr.port())],
        BackoffConfig::default(),
        modbus_registry(),
    )
    .expect("spawn");
    let handle = supervisor.handle(1).expect("handle");
    tokio::time::timeout(
        Duration::from_secs(5),
        handle
            .status_watch()
            .wait_for(|s| *s == BrokerConnectionStatus::Connected),
    )
    .await
    .expect("connects in time")
    .expect("connects");

    let written = handle
        .write(vec![WriteItem::new(
            spec("40011", DataType::U16),
            Value::F64(321.0),
        )])
        .await
        .expect("write");
    assert_eq!(written, vec![Ok(())]);
    let read = handle
        .read(vec![spec("40011", DataType::U16)])
        .await
        .expect("read");
    assert_eq!(read, vec![Ok(Value::F64(321.0))]);
    supervisor.shutdown().await;
}
