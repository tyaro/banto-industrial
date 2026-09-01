//! End-to-end tests for [`super::ModbusWriteClient`] against
//! [`super::simulator::Simulator`] - real Modbus TCP bytes over a loopback
//! socket, not hand-decoded byte arrays. Mirrors
//! `crate::slmp::integration_tests`'s shape and, one level further back,
//! `banto-plc`'s own `modbus/integration_tests.rs` for the read side.

use std::time::Duration;

use banto_plc::{Address, DataType, ModbusTcpConfig, TagValue, WordOrder};

use super::simulator::Simulator;
use super::ModbusWriteClient;
use crate::client::PlcWriteClient;
use crate::error::PlcWriteError;
use crate::types::{WriteRequest, WriteResult};

fn config(addr: std::net::SocketAddr) -> ModbusTcpConfig {
    ModbusTcpConfig {
        host: addr.ip().to_string(),
        port: addr.port(),
        unit_id: 1,
        connect_timeout: Duration::from_secs(1),
        response_timeout: Duration::from_millis(500),
        word_order: WordOrder::HighLow,
    }
}

fn wreq(raw: &str, data_type: DataType, value: TagValue) -> WriteRequest {
    WriteRequest {
        address: Address::parse(raw).unwrap_or_else(|e| panic!("{raw} should parse: {e}")),
        data_type,
        value,
    }
}

fn word(raw: &str, data_type: DataType, v: f64) -> WriteRequest {
    wreq(raw, data_type, TagValue::F64(v))
}

fn coil(raw: &str, v: bool) -> WriteRequest {
    wreq(raw, DataType::Bit, TagValue::Bit(v))
}

/// #131's explicit requirement: the write client's default `word_order`
/// matches the read side's default ([`banto_plc::ModbusTcpConfig::default`]),
/// which is `HighLow` - the opposite of SLMP's `LowHigh` default
/// (`banto_plc::SlmpConfig::default`). Getting this backwards only breaks a
/// 32-bit value's byte order on real hardware, never in a test that hand-picks
/// its own `WordOrder` - so this test exists specifically to catch that.
#[test]
fn modbus_write_client_config_defaults_to_high_low_word_order() {
    assert_eq!(ModbusTcpConfig::default().word_order, WordOrder::HighLow);
}

#[tokio::test]
async fn single_register_write_lands_and_reads_back() {
    let sim = Simulator::start().await;
    let mut client = ModbusWriteClient::new(config(sim.addr));
    client.connect().await.unwrap();

    let results = client
        .write_batch(&[word("40001", DataType::U16, 42.0)])
        .await
        .unwrap();
    assert_eq!(results, vec![WriteResult::Ok]);
    assert_eq!(sim.get_holding_register(0), 42);
    // A single request must use FC6, not a one-element FC16.
    assert_eq!(sim.write_command_count(), 1);
}

#[tokio::test]
async fn single_coil_write_lands_and_reads_back() {
    let sim = Simulator::start().await;
    let mut client = ModbusWriteClient::new(config(sim.addr));
    client.connect().await.unwrap();

    let results = client.write_batch(&[coil("00001", true)]).await.unwrap();
    assert_eq!(results, vec![WriteResult::Ok]);
    assert!(sim.get_coil(0));
}

#[tokio::test]
async fn adjacent_register_writes_merge_into_one_fc16_wire_write() {
    let sim = Simulator::start().await;
    let mut client = ModbusWriteClient::new(config(sim.addr));
    client.connect().await.unwrap();

    let results = client
        .write_batch(&[
            word("40001", DataType::U16, 1.0),
            word("40002", DataType::U16, 2.0),
            word("40003", DataType::U16, 3.0),
        ])
        .await
        .unwrap();
    assert_eq!(results, vec![WriteResult::Ok; 3]);
    assert_eq!(sim.get_holding_register(0), 1);
    assert_eq!(sim.get_holding_register(1), 2);
    assert_eq!(sim.get_holding_register(2), 3);
    // Three adjacent requests must cost exactly one wire write (FC16).
    assert_eq!(sim.write_command_count(), 1);
}

/// The load-bearing safety property, proven end-to-end: a gap must never be
/// bridged, so the register in between is never touched by this write.
#[tokio::test]
async fn a_gap_between_targets_never_touches_the_skipped_register() {
    let sim = Simulator::start().await;
    sim.set_holding_register(1, 0xBEEF); // the register that must survive untouched
    let mut client = ModbusWriteClient::new(config(sim.addr));
    client.connect().await.unwrap();

    let results = client
        .write_batch(&[
            word("40001", DataType::U16, 1.0),
            word("40003", DataType::U16, 3.0),
        ])
        .await
        .unwrap();
    assert_eq!(results, vec![WriteResult::Ok; 2]);
    assert_eq!(sim.get_holding_register(0), 1);
    assert_eq!(
        sim.get_holding_register(1),
        0xBEEF,
        "must be left untouched"
    );
    assert_eq!(sim.get_holding_register(2), 3);
    // Two separate wire writes - the planner refused to bridge the gap.
    assert_eq!(sim.write_command_count(), 2);
}

/// A 32-bit value's word order must match the read side's configured order -
/// pinned end-to-end with `HighLow` (the Modbus default), the opposite of
/// SLMP's default.
#[tokio::test]
async fn thirty_two_bit_value_uses_the_configured_word_order() {
    let sim = Simulator::start().await;
    let mut cfg = config(sim.addr);
    cfg.word_order = WordOrder::HighLow;
    let mut client = ModbusWriteClient::new(cfg);
    client.connect().await.unwrap();

    let results = client
        .write_batch(&[word("40001", DataType::U32, 0x0001_0002u32 as f64)])
        .await
        .unwrap();
    assert_eq!(results, vec![WriteResult::Ok]);
    // HighLow: high word first.
    assert_eq!(sim.get_holding_register(0), 0x0001);
    assert_eq!(sim.get_holding_register(1), 0x0002);
}

#[tokio::test]
async fn a_device_side_exception_is_a_per_request_bad_not_a_whole_call_error() {
    let sim = Simulator::start().await;
    sim.inject_exception(0x06, 0, 0x02); // FC6, offset 0: illegal data address
    let mut client = ModbusWriteClient::new(config(sim.addr));
    client.connect().await.unwrap();

    let results = client
        .write_batch(&[
            word("40001", DataType::U16, 1.0), // exception injected here
            word("40101", DataType::U16, 2.0), // unaffected, different group
        ])
        .await
        .unwrap();

    match &results[0] {
        WriteResult::Bad(PlcWriteError::ModbusException { function, code, .. }) => {
            assert_eq!(*function, 0x06);
            assert_eq!(*code, 0x02);
        }
        other => panic!("expected ModbusException Bad, got {other:?}"),
    }
    assert_eq!(results[1], WriteResult::Ok);
    // The connection itself must still be usable - not torn down.
    assert_eq!(sim.get_holding_register(100), 2);
}

/// The one classification #131's brief calls out explicitly: a device
/// exception must NOT be connection-fatal, or a broker would reconnect on
/// every ordinary device refusal.
#[test]
fn modbus_exception_is_not_connection_fatal() {
    let err = PlcWriteError::ModbusException {
        function: 0x06,
        code: 0x02,
        message: "illegal data address".to_string(),
    };
    assert!(!err.is_connection_fatal());
}

#[tokio::test]
async fn a_response_timeout_is_connection_fatal_and_disconnects() {
    let sim = Simulator::start().await;
    sim.hang();
    let mut client = ModbusWriteClient::new(config(sim.addr));
    client.connect().await.unwrap();

    let err = client
        .write_batch(&[word("40001", DataType::U16, 1.0)])
        .await
        .unwrap_err();
    assert_eq!(err, PlcWriteError::ResponseTimeout);
    assert!(err.is_connection_fatal());

    // The client must have torn its session down - a further call reports
    // NotConnected, exactly like the read client after a fatal error.
    let again = client
        .write_batch(&[word("40001", DataType::U16, 1.0)])
        .await
        .unwrap_err();
    assert_eq!(again, PlcWriteError::NotConnected);
}

#[tokio::test]
async fn a_malformed_response_is_connection_fatal() {
    let sim = Simulator::start().await;
    sim.emit_malformed_frames();
    let mut client = ModbusWriteClient::new(ModbusTcpConfig {
        response_timeout: Duration::from_millis(200),
        ..config(sim.addr)
    });
    client.connect().await.unwrap();

    let err = client
        .write_batch(&[word("40001", DataType::U16, 1.0)])
        .await
        .unwrap_err();
    assert!(err.is_connection_fatal());
}

#[tokio::test]
async fn write_batch_before_connect_is_not_connected() {
    let mut client = ModbusWriteClient::new(ModbusTcpConfig {
        host: "127.0.0.1".to_string(),
        ..Default::default()
    });
    let requests = [word("40001", DataType::U16, 1.0)];
    assert!(matches!(
        client.write_batch(&requests).await,
        Err(PlcWriteError::NotConnected)
    ));
}

#[tokio::test]
async fn disconnect_on_a_never_connected_client_is_a_no_op() {
    let mut client = ModbusWriteClient::new(ModbusTcpConfig::default());
    client.disconnect().await; // must not panic
    assert!(client.stream.is_none());
}

#[tokio::test]
async fn a_read_only_area_is_a_per_request_bad_never_reaching_the_wire() {
    let sim = Simulator::start().await;
    let mut client = ModbusWriteClient::new(config(sim.addr));
    client.connect().await.unwrap();

    let results = client
        .write_batch(&[
            wreq("10001", DataType::Bit, TagValue::Bit(true)), // DiscreteInput, read-only
            word("40001", DataType::U16, 1.0),                 // fine
        ])
        .await
        .unwrap();
    assert!(matches!(
        results[0],
        WriteResult::Bad(PlcWriteError::ModbusReadOnlyArea { .. })
    ));
    assert_eq!(results[1], WriteResult::Ok);
    // The bad request never reached the wire - only the good one did.
    assert_eq!(sim.write_command_count(), 1);
}
