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
        address: Address::ModbusRef {
            area,
            offset,
            bit: None,
        },
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

// --- P0-3 (docs/banto-hub-driver-refactor-plan.md §8.3): behaviours pinned
// before the I/O is unified in R2b-3 ------------------------------------------

/// Config for the tests that need a response to take a while to arrive:
/// a generous response timeout so only the behaviour under test can fail.
fn patient_config(sim: &Simulator) -> ModbusTcpConfig {
    ModbusTcpConfig {
        response_timeout: Duration::from_secs(5),
        ..fast_config(sim)
    }
}

/// P0-3 #1: a response that reaches the client in several TCP segments -
/// split inside the 7-byte MBAP header, across the header/PDU boundary, and
/// one byte at a time - is read as one response with the right values.
#[tokio::test]
async fn a_response_split_across_segments_is_read_as_one_response() {
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 42);
    sim.set_holding_register(1, 0xFFFF);
    sim.set_holding_registers(2, &[0x0001, 0x0002]);
    sim.set_coil(0, true);

    let mut client = ModbusTcpClient::new(patient_config(&sim));
    client.connect().await.unwrap();

    let requests = [
        req(AddressArea::HoldingRegister, 0, DataType::U16),
        req(AddressArea::HoldingRegister, 1, DataType::I16),
        req(AddressArea::HoldingRegister, 2, DataType::U32),
        req(AddressArea::Coil, 0, DataType::Bit),
    ];
    for chunk in [1usize, 3, 7, 8] {
        sim.set_response_chunking(Some((chunk, Duration::from_millis(2))));
        // Twice: the second read proves the stream is still aligned.
        for _ in 0..2 {
            let results = client
                .read_batch(&requests)
                .await
                .unwrap_or_else(|e| panic!("chunk={chunk}: read_batch failed: {e:?}"));
            assert_eq!(
                results[0],
                ReadResult::Value(TagValue::F64(42.0)),
                "chunk={chunk}"
            );
            assert_eq!(
                results[1],
                ReadResult::Value(TagValue::F64(-1.0)),
                "chunk={chunk}"
            );
            assert_eq!(
                results[2],
                ReadResult::Value(TagValue::F64(0x0001_0002_u32 as f64)),
                "chunk={chunk}"
            );
            assert_eq!(
                results[3],
                ReadResult::Value(TagValue::Bit(true)),
                "chunk={chunk}"
            );
        }
    }
}

/// P0-3 #2 (a), client level: the response to a timed-out request arrives
/// later. The timeout tears the connection down, so the next call is a fatal
/// `NotConnected`, never the stale value; after a fresh `connect()` the read
/// returns the *current* value, not the one the late response carried.
#[tokio::test]
async fn a_late_response_after_a_timeout_is_never_returned_as_a_later_value() {
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 1);
    let gate = sim.hold_next_response(); // the response carrying `1` is held

    let mut client = ModbusTcpClient::new(fast_config(&sim));
    client.connect().await.unwrap();
    let requests = [req(AddressArea::HoldingRegister, 0, DataType::I16)];

    let err = client.read_batch(&requests).await.unwrap_err();
    assert!(matches!(err, PlcError::ResponseTimeout), "got {err:?}");
    assert!(err.is_connection_fatal());

    // The PLC now answers the old request, and the register has moved on.
    sim.set_holding_register(0, 2);
    gate.release();

    let err = client.read_batch(&requests).await.unwrap_err();
    assert!(matches!(err, PlcError::NotConnected), "got {err:?}");
    assert!(err.is_connection_fatal());

    client.connect().await.expect("reconnect");
    let results = client.read_batch(&requests).await.unwrap();
    assert_eq!(results[0], ReadResult::Value(TagValue::F64(2.0)));
}

/// P0-3 #2 (a), stream level: if the caller keeps using the *same* stream
/// after a timeout (the broker session does not tear it down itself), the
/// late response to request N sits in front of the response to N+1. It must
/// be rejected as a fatal transaction-id mismatch, never returned as the
/// value of N+1.
#[tokio::test]
async fn a_late_response_on_a_reused_stream_is_a_fatal_protocol_error() {
    use crate::plan_requests;

    let sim = Simulator::start().await;
    sim.set_holding_register(0, 1);
    let gate = sim.hold_next_response();

    let config = fast_config(&sim);
    let mut stream = super::dial_modbus(&config).await.unwrap();
    let mut next_tid = 0u16;
    let outcome = plan_requests(&[req(AddressArea::HoldingRegister, 0, DataType::I16)]);

    let err = super::execute_modbus_reads(
        &mut stream,
        1,
        Duration::from_millis(100),
        &mut next_tid,
        &outcome,
        1,
        crate::decode::WordOrder::HighLow,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, PlcError::ResponseTimeout), "got {err:?}");

    sim.set_holding_register(0, 2);
    gate.release(); // the stale response (tid 0, value 1) is now on the wire

    let err = super::execute_modbus_reads(
        &mut stream,
        1,
        Duration::from_secs(5),
        &mut next_tid,
        &outcome,
        1,
        crate::decode::WordOrder::HighLow,
    )
    .await
    .expect_err("the stale response must not be accepted");
    match &err {
        PlcError::Protocol(msg) => assert!(msg.contains("transaction id mismatch"), "{msg}"),
        other => panic!("expected Protocol(transaction id mismatch), got {other:?}"),
    }
    assert!(err.is_connection_fatal());
}

/// P0-3 #2 (b): a response whose transaction id is not the request's is a
/// fatal protocol error (`error.rs` `Protocol`, connection-fatal), and the
/// client tears the connection down.
#[tokio::test]
async fn a_response_with_the_wrong_transaction_id_is_fatal() {
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 9);
    sim.set_wrong_transaction_id(true);

    let mut client = ModbusTcpClient::new(fast_config(&sim));
    client.connect().await.unwrap();
    let requests = [req(AddressArea::HoldingRegister, 0, DataType::I16)];

    let err = client.read_batch(&requests).await.unwrap_err();
    match &err {
        PlcError::Protocol(msg) => assert!(msg.contains("transaction id mismatch"), "{msg}"),
        other => panic!("expected Protocol(transaction id mismatch), got {other:?}"),
    }
    assert!(err.is_connection_fatal());

    let err = client.read_batch(&requests).await.unwrap_err();
    assert!(matches!(err, PlcError::NotConnected), "got {err:?}");
}

/// P0-3 #3: connecting to a port nobody listens on fails (refused or timed
/// out) without hanging, and leaves the client `NotConnected`. Same shape as
/// `slmp/integration_tests.rs`'s `connect_to_a_closed_port_fails_without_hanging`,
/// except the closed port is made by binding and dropping a plain listener
/// (immediately and deterministically closed) instead of relying on an aborted
/// simulator task having released its port.
#[tokio::test]
async fn connect_to_a_closed_port_fails_without_hanging() {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };

    let mut client = ModbusTcpClient::new(ModbusTcpConfig {
        host: "127.0.0.1".to_string(),
        port,
        connect_timeout: Duration::from_millis(300),
        ..Default::default()
    });
    let err = client
        .connect()
        .await
        .expect_err("connecting to a closed port should fail");
    assert!(
        matches!(err, PlcError::Connection(_) | PlcError::ConnectTimeout(_)),
        "got {err:?}"
    );
    assert!(matches!(
        client
            .read_batch(&[req(AddressArea::HoldingRegister, 0, DataType::I16)])
            .await,
        Err(PlcError::NotConnected)
    ));
}

/// P0-3 #4: the Modbus equivalent of SLMP's `read_batch_mixed_*`. The mixed
/// front door (`plan_batch_requests`, what the broker's Modbus driver calls)
/// run over a real socket: numeric and bit requests of several kinds are read
/// in one call and come back at their original positions; string requests
/// are, as today, `Bad(UnsupportedCombination)` without touching the wire or
/// costing their batch-mates a reading.
#[tokio::test]
async fn read_batch_mixed_reads_numerics_and_keeps_strings_bad() {
    use crate::decode::WordOrder;
    use crate::plan_batch_requests;
    use crate::types::{BatchReadRequest, StringEncoding, StringReadRequest};

    let sim = Simulator::start().await;
    // Registers under the string request too: if the string were (wrongly)
    // read, it would not be Bad.
    sim.set_holding_registers(0, &[0x4142, 0x4344, 0x0000, 0x0000]);
    sim.set_holding_register(10, 7);
    sim.set_holding_registers(20, &[0x0001, 0x0002]);
    sim.set_input_register(0, 100);
    sim.set_coil(5, true);

    let sreq = |offset: u16, words: u16| {
        BatchReadRequest::String(StringReadRequest {
            address: Address::ModbusRef {
                area: AddressArea::HoldingRegister,
                offset,
                bit: None,
            },
            words,
            encoding: StringEncoding::ShiftJis,
        })
    };
    let requests = [
        sreq(0, 4),
        BatchReadRequest::Numeric(req(AddressArea::HoldingRegister, 10, DataType::I16)),
        BatchReadRequest::Numeric(req(AddressArea::HoldingRegister, 20, DataType::U32)),
        BatchReadRequest::Numeric(req(AddressArea::InputRegister, 0, DataType::U16)),
        sreq(30, 2),
        BatchReadRequest::Numeric(req(AddressArea::Coil, 5, DataType::Bit)),
    ];

    let config = fast_config(&sim);
    let mut stream = super::dial_modbus(&config).await.unwrap();
    let mut next_tid = 0u16;
    let outcome = plan_batch_requests(&requests);
    let results = super::execute_modbus_reads(
        &mut stream,
        1,
        Duration::from_secs(5),
        &mut next_tid,
        &outcome,
        requests.len(),
        WordOrder::HighLow,
    )
    .await
    .expect("the whole call is Ok");

    assert_eq!(results.len(), requests.len());
    for string_index in [0usize, 4] {
        match &results[string_index] {
            ReadResult::Bad(PlcError::UnsupportedCombination { area, data_type }) => {
                assert_eq!(area, "modbus-tcp");
                assert_eq!(data_type, "string");
            }
            other => {
                panic!("expected Bad(UnsupportedCombination) at {string_index}, got {other:?}")
            }
        }
    }
    assert_eq!(results[1], ReadResult::Value(TagValue::F64(7.0)));
    assert_eq!(
        results[2],
        ReadResult::Value(TagValue::F64(0x0001_0002_u32 as f64))
    );
    assert_eq!(results[3], ReadResult::Value(TagValue::F64(100.0)));
    assert_eq!(results[5], ReadResult::Value(TagValue::Bit(true)));
}
