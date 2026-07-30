//! End-to-end tests for [`super::ModbusTcpClient`] against
//! [`super::simulator::Simulator`] - the scenarios a hand-built byte array
//! cannot exercise: real TCP framing, real timeouts, a real dropped
//! connection. `frame.rs`/`decode.rs`/`planning.rs` already cover the pure
//! logic in isolation; this file is about the parts that only exist once a
//! socket is involved.

use std::time::Duration;

use super::simulator::Simulator;
use super::{ModbusTcpClient, ModbusTcpConfig};
use crate::address::{Address, AddressArea};
use crate::client::PlcClient;
use crate::error::PlcError;
use crate::types::{DataType, ReadRequest, ReadResult, TagValue};

fn req(area: AddressArea, offset: u16, data_type: DataType) -> ReadRequest {
    ReadRequest {
        address: Address::ModbusRef { area, offset },
        data_type,
    }
}

/// Config pointed at `sim`, with short timeouts so failure-path tests
/// (timeout, disconnect) do not slow the suite down.
fn fast_config(sim: &Simulator) -> ModbusTcpConfig {
    ModbusTcpConfig {
        host: sim.addr.ip().to_string(),
        port: sim.addr.port(),
        unit_id: 1,
        connect_timeout: Duration::from_millis(500),
        response_timeout: Duration::from_millis(100),
        ..Default::default()
    }
}

#[tokio::test]
async fn normal_batch_reads_every_data_type_correctly() {
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 42); // u16
    sim.set_holding_register(1, 0xFFFF); // i16 == -1
    sim.set_holding_registers(2, &[0x0001, 0x0002]); // u32, high-low, == 0x0001_0002
    sim.set_input_register(0, 100);
    sim.set_coil(0, true);
    sim.set_discrete_input(0, false);

    let mut client = ModbusTcpClient::new(fast_config(&sim));
    client.connect().await.expect("connect should succeed");

    let requests = [
        req(AddressArea::HoldingRegister, 0, DataType::U16),
        req(AddressArea::HoldingRegister, 1, DataType::I16),
        req(AddressArea::HoldingRegister, 2, DataType::U32),
        req(AddressArea::InputRegister, 0, DataType::U16),
        req(AddressArea::Coil, 0, DataType::Bit),
        req(AddressArea::DiscreteInput, 0, DataType::Bit),
    ];
    let results = client.read_batch(&requests).await.expect("read_batch ok");

    assert_eq!(results.len(), requests.len());
    assert_eq!(results[0], ReadResult::Value(TagValue::F64(42.0)));
    assert_eq!(results[1], ReadResult::Value(TagValue::F64(-1.0)));
    assert_eq!(
        results[2],
        ReadResult::Value(TagValue::F64(0x0001_0002_u32 as f64))
    );
    assert_eq!(results[3], ReadResult::Value(TagValue::F64(100.0)));
    assert_eq!(results[4], ReadResult::Value(TagValue::Bit(true)));
    assert_eq!(results[5], ReadResult::Value(TagValue::Bit(false)));
}

#[tokio::test]
async fn mixed_areas_and_types_in_one_batch_each_get_their_own_group() {
    let sim = Simulator::start().await;
    sim.set_holding_register(10, 7);
    sim.set_coil(5, true);

    let mut client = ModbusTcpClient::new(fast_config(&sim));
    client.connect().await.unwrap();

    let requests = [
        req(AddressArea::HoldingRegister, 10, DataType::I16),
        req(AddressArea::Coil, 5, DataType::Bit),
    ];
    let results = client.read_batch(&requests).await.unwrap();
    assert_eq!(results[0], ReadResult::Value(TagValue::F64(7.0)));
    assert_eq!(results[1], ReadResult::Value(TagValue::Bit(true)));
}

/// A Modbus exception for one group must not affect an unrelated group in
/// the same batch (docs/plan.md I2's third fixed invariant).
#[tokio::test]
async fn individual_bad_address_does_not_kill_the_rest_of_the_batch() {
    let sim = Simulator::start().await;
    sim.set_holding_register(100, 55);
    // Requests at offset 0 and offset 100 are far enough apart (gap > 5)
    // that planning puts them in separate groups - see planning.rs's
    // MAX_GAP - so injecting an exception on the group starting at offset 0
    // must leave the offset-100 group untouched.
    sim.inject_exception(super::frame::FC_READ_HOLDING_REGISTERS, 0, 0x02); // illegal data address

    let mut client = ModbusTcpClient::new(fast_config(&sim));
    client.connect().await.unwrap();

    let requests = [
        req(AddressArea::HoldingRegister, 0, DataType::I16),
        req(AddressArea::HoldingRegister, 100, DataType::I16),
    ];
    let results = client
        .read_batch(&requests)
        .await
        .expect("whole call should still be Ok");

    match &results[0] {
        ReadResult::Bad(PlcError::ModbusException { code, .. }) => assert_eq!(*code, 0x02),
        other => panic!("expected Bad(ModbusException), got {other:?}"),
    }
    assert_eq!(results[1], ReadResult::Value(TagValue::F64(55.0)));
}

#[tokio::test]
async fn exception_code_surfaces_a_readable_message() {
    let sim = Simulator::start().await;
    sim.inject_exception(super::frame::FC_READ_HOLDING_REGISTERS, 0, 0x02);

    let mut client = ModbusTcpClient::new(fast_config(&sim));
    client.connect().await.unwrap();

    let requests = [req(AddressArea::HoldingRegister, 0, DataType::I16)];
    let results = client.read_batch(&requests).await.unwrap();
    match &results[0] {
        ReadResult::Bad(PlcError::ModbusException {
            function,
            code,
            message,
        }) => {
            assert_eq!(*function, super::frame::FC_READ_HOLDING_REGISTERS);
            assert_eq!(*code, 0x02);
            assert!(!message.is_empty());
        }
        other => panic!("expected Bad(ModbusException), got {other:?}"),
    }
}

/// A group and data-type mismatch (bit at a register address) is resolved
/// before any wire traffic and does not prevent the rest of the batch from
/// reaching the PLC.
#[tokio::test]
async fn unsupported_combination_is_bad_without_touching_the_wire() {
    let sim = Simulator::start().await;
    sim.set_holding_register(1, 9);

    let mut client = ModbusTcpClient::new(fast_config(&sim));
    client.connect().await.unwrap();

    let requests = [
        req(AddressArea::HoldingRegister, 0, DataType::Bit), // invalid combo
        req(AddressArea::HoldingRegister, 1, DataType::I16),
    ];
    let results = client.read_batch(&requests).await.unwrap();
    assert!(matches!(
        results[0],
        ReadResult::Bad(PlcError::UnsupportedCombination { .. })
    ));
    assert_eq!(results[1], ReadResult::Value(TagValue::F64(9.0)));
}

#[tokio::test]
async fn response_timeout_fails_the_call_and_tears_down_the_connection() {
    let sim = Simulator::start().await;
    sim.hang();

    let mut client = ModbusTcpClient::new(fast_config(&sim));
    client.connect().await.unwrap();

    let requests = [req(AddressArea::HoldingRegister, 0, DataType::I16)];
    let err = client
        .read_batch(&requests)
        .await
        .expect_err("hung simulator should time out the whole call");
    assert!(matches!(err, PlcError::ResponseTimeout));

    // The connection is now considered dead - further calls must not hang
    // again waiting on the same broken stream, they must fail immediately.
    let err2 = client.read_batch(&requests).await.unwrap_err();
    assert!(matches!(err2, PlcError::NotConnected));
}

#[tokio::test]
async fn disconnect_mid_session_fails_the_call_and_tears_down_the_connection() {
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 1);

    let mut client = ModbusTcpClient::new(fast_config(&sim));
    client.connect().await.unwrap();

    // Prove the connection works before severing it.
    let requests = [req(AddressArea::HoldingRegister, 0, DataType::I16)];
    client
        .read_batch(&requests)
        .await
        .expect("first read should succeed");

    sim.stop();

    let err = client
        .read_batch(&requests)
        .await
        .expect_err("severed connection should fail the call");
    assert!(matches!(err, PlcError::Connection(_)));

    let err2 = client.read_batch(&requests).await.unwrap_err();
    assert!(matches!(err2, PlcError::NotConnected));
}

#[tokio::test]
async fn read_batch_before_connect_is_not_connected() {
    let sim = Simulator::start().await;
    let mut client = ModbusTcpClient::new(fast_config(&sim));
    let requests = [req(AddressArea::HoldingRegister, 0, DataType::I16)];
    let err = client.read_batch(&requests).await.unwrap_err();
    assert!(matches!(err, PlcError::NotConnected));
}

#[tokio::test]
async fn disconnect_then_reconnect_works() {
    let sim1 = Simulator::start().await;
    let mut client = ModbusTcpClient::new(fast_config(&sim1));
    client.connect().await.unwrap();
    sim1.stop();
    let err = client
        .read_batch(&[req(AddressArea::HoldingRegister, 0, DataType::I16)])
        .await
        .unwrap_err();
    assert!(matches!(err, PlcError::Connection(_)));

    // I3 owns the actual reconnect loop (docs/plan.md I2 §2: "再接続ループ
    // は持たない") - this only proves the *client* half of that contract:
    // after a connection-fatal failure it is not permanently wedged, a
    // fresh `connect()` (here, to a new simulator instance standing in for
    // "the PLC came back") followed by `read_batch` works exactly like a
    // brand new client would.
    let sim2 = Simulator::start().await;
    sim2.set_holding_register(0, 77);
    let mut client2 = ModbusTcpClient::new(fast_config(&sim2));
    client2
        .connect()
        .await
        .expect("reconnect to the new simulator");
    let results = client2
        .read_batch(&[req(AddressArea::HoldingRegister, 0, DataType::I16)])
        .await
        .unwrap();
    assert_eq!(results[0], ReadResult::Value(TagValue::F64(77.0)));
}

/// Performance smoke test (docs/plan.md I2 §7): 256 tags spread across
/// contiguous holding registers - realistic for a single collection group
/// near the v1 tag-count target (recorder-requirements.md §3.1) - split
/// into ceil(256/125) = 3 wire round trips by planning.rs's register-quantity
/// limit. Not a CI gate (no assertion on the timing, per docs/plan.md I2 §7:
/// "CI失敗条件にはしない") - loopback-to-a-Tokio-task latency does not
/// represent real PLC/network latency, so a hard threshold here would only
/// ever tell us about this machine's scheduler, not about whether the
/// 100ms collection-cycle target is achievable against a real device. The
/// printed number is this crate's first real data point toward answering
/// that question for I3.
#[tokio::test]
async fn performance_smoke_256_tags_x_1000_read_batch_calls() {
    const TAG_COUNT: u16 = 256;
    const ITERATIONS: u32 = 1000;

    let sim = Simulator::start().await;
    let values: Vec<u16> = (0..TAG_COUNT).collect();
    sim.set_holding_registers(0, &values);

    let mut client = ModbusTcpClient::new(fast_config(&sim));
    client.connect().await.unwrap();

    let requests: Vec<ReadRequest> = (0..TAG_COUNT)
        .map(|i| req(AddressArea::HoldingRegister, i, DataType::U16))
        .collect();

    // One warm-up call outside the timed loop (first-connection TCP/Tokio
    // scheduling overhead is not representative of steady-state polling).
    client.read_batch(&requests).await.unwrap();

    let started = std::time::Instant::now();
    for _ in 0..ITERATIONS {
        let results = client.read_batch(&requests).await.unwrap();
        assert_eq!(results.len(), TAG_COUNT as usize);
    }
    let elapsed = started.elapsed();
    let per_call = elapsed / ITERATIONS;

    println!(
        "[banto-plc perf smoke] {TAG_COUNT} tags x {ITERATIONS} read_batch calls (loopback \
         simulator, 3 round trips/call) in {elapsed:?} total, {per_call:?}/call average \
         (100ms/cycle target, recorder-requirements.md §3.1)"
    );
}
