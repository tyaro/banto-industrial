//! End-to-end tests for [`super::SlmpWriteClient`] against
//! [`super::simulator::Simulator`] - the scenarios only real TCP framing
//! through the wrapped `slmp` crate can exercise. `planning.rs`/`encode.rs`
//! cover the pure logic in isolation and `mod.rs`'s unit tests cover the error
//! classifier over hand-written strings; this file is about the parts that only
//! exist once the wrapped crate and a socket are involved - and, crucially, the
//! write/read *round trips* that prove a written value lands with the right
//! bytes and word order (read back through the real `banto_plc::SlmpClient`, not
//! just inspected in the simulator).

use std::time::Duration;

use banto_plc::{
    Address, DataType, PlcClient, ReadRequest, ReadResult, SlmpClient, SlmpConfig, SlmpCpu,
    SlmpDevice, TagValue, WordOrder,
};

use super::simulator::Simulator;
use super::SlmpWriteClient;
use crate::client::PlcWriteClient;
use crate::error::PlcWriteError;
use crate::types::{WriteRequest, WriteResult};

fn wreq(raw: &str, data_type: DataType, value: TagValue) -> WriteRequest {
    WriteRequest {
        address: Address::parse_slmp(raw).unwrap_or_else(|e| panic!("{raw} should parse: {e}")),
        data_type,
        value,
    }
}

fn word(raw: &str, data_type: DataType, v: f64) -> WriteRequest {
    wreq(raw, data_type, TagValue::F64(v))
}

fn bit(raw: &str, v: bool) -> WriteRequest {
    wreq(raw, DataType::Bit, TagValue::Bit(v))
}

fn rreq(raw: &str, data_type: DataType) -> ReadRequest {
    ReadRequest {
        address: Address::parse_slmp(raw).unwrap_or_else(|e| panic!("{raw} should parse: {e}")),
        data_type,
    }
}

/// Config pointed at `sim` with short timeouts. Reuses the shared
/// `banto_plc::SlmpConfig` - the same type the read client uses - so the write
/// client and a read client can be built from configs that differ only where
/// intended.
fn fast_config(sim: &Simulator) -> SlmpConfig {
    SlmpConfig {
        host: sim.addr.ip().to_string(),
        port: sim.addr.port(),
        connect_timeout: Duration::from_millis(500),
        response_timeout: Duration::from_millis(100),
        ..Default::default()
    }
}

/// A read client pointed at the same simulator, for reading back what a write
/// landed - the strongest possible check, since it exercises the real read path
/// too and proves the two crates agree on wire encoding.
async fn connected_reader(sim: &Simulator) -> SlmpClient {
    let mut reader = SlmpClient::new(fast_config(sim));
    reader.connect().await.expect("reader connect");
    reader
}

#[tokio::test]
async fn write_batch_writes_every_data_type_and_reads_back_through_the_read_client() {
    let sim = Simulator::start().await;
    let mut client = SlmpWriteClient::new(fast_config(&sim));
    client.connect().await.expect("connect");

    let requests = [
        word("D0", DataType::U16, 42.0),
        word("D1", DataType::I16, -1.0),
        word("D2", DataType::U32, 0x0001_0002u32 as f64),
        word("D4", DataType::F32, 1.5),
        word("R0", DataType::U16, 100.0),
        bit("M0", true),
        bit("X0", false),
    ];
    let results = client.write_batch(&requests).await.expect("write_batch ok");
    assert_eq!(results.len(), requests.len());
    assert!(
        results.iter().all(|r| *r == WriteResult::Ok),
        "every write should succeed: {results:?}"
    );

    // Read every value back through the real read client.
    let mut reader = connected_reader(&sim).await;
    let read_back = reader
        .read_batch(&[
            rreq("D0", DataType::U16),
            rreq("D1", DataType::I16),
            rreq("D2", DataType::U32),
            rreq("D4", DataType::F32),
            rreq("R0", DataType::U16),
            rreq("M0", DataType::Bit),
            rreq("X0", DataType::Bit),
        ])
        .await
        .expect("read back");

    assert_eq!(read_back[0], ReadResult::Value(TagValue::F64(42.0)));
    assert_eq!(read_back[1], ReadResult::Value(TagValue::F64(-1.0)));
    assert_eq!(
        read_back[2],
        ReadResult::Value(TagValue::F64(0x0001_0002u32 as f64))
    );
    assert_eq!(read_back[3], ReadResult::Value(TagValue::F64(1.5)));
    assert_eq!(read_back[4], ReadResult::Value(TagValue::F64(100.0)));
    assert_eq!(read_back[5], ReadResult::Value(TagValue::Bit(true)));
    assert_eq!(read_back[6], ReadResult::Value(TagValue::Bit(false)));
}

/// The default word order (`LowHigh`) is the thing most likely to be silently
/// wrong against real hardware, so it gets its own round-trip in both
/// directions: the raw simulator words must be low-word-first, and a `HighLow`
/// write of the same value must produce the mirror image.
#[tokio::test]
async fn default_low_high_word_order_round_trips_and_lands_low_word_first() {
    let sim = Simulator::start().await;
    let mut client = SlmpWriteClient::new(fast_config(&sim));
    client.connect().await.unwrap();

    // f32 1.5 = 0x3FC00000. LowHigh -> low word 0x0000 in D0, high 0x3FC0 in D1.
    client
        .write_batch(&[word("D0", DataType::F32, 1.5)])
        .await
        .unwrap();
    assert_eq!(sim.get_word(SlmpDevice::D, 0), 0x0000, "low word in D0");
    assert_eq!(sim.get_word(SlmpDevice::D, 1), 0x3FC0, "high word in D1");

    // Reading it back with a LowHigh reader recovers 1.5.
    let mut reader = connected_reader(&sim).await;
    let rb = reader
        .read_batch(&[rreq("D0", DataType::F32)])
        .await
        .unwrap();
    assert_eq!(rb[0], ReadResult::Value(TagValue::F64(1.5)));

    // The same value written HighLow must land high-word-first - the mirror
    // image - proving the setting is really plumbed through.
    let mut swapped = SlmpWriteClient::new(SlmpConfig {
        word_order: WordOrder::HighLow,
        ..fast_config(&sim)
    });
    swapped.connect().await.unwrap();
    swapped
        .write_batch(&[word("D10", DataType::F32, 1.5)])
        .await
        .unwrap();
    assert_eq!(
        sim.get_word(SlmpDevice::D, 10),
        0x3FC0,
        "high word first now"
    );
    assert_eq!(sim.get_word(SlmpDevice::D, 11), 0x0000);
}

/// Bit-unit writes pack two points per byte; a run long enough to span several
/// bytes, with an irregular pattern and odd length, proves the nibble packing
/// is emitted in the right order (read back through the real read client).
#[tokio::test]
async fn a_long_odd_length_bit_run_writes_in_the_right_order() {
    let sim = Simulator::start().await;
    let mut client = SlmpWriteClient::new(fast_config(&sim));
    client.connect().await.unwrap();

    let pattern = [
        true, false, false, true, true, true, false, false, true, false, true,
    ];
    let requests: Vec<WriteRequest> = pattern
        .iter()
        .enumerate()
        .map(|(i, &v)| bit(&format!("M{i}"), v))
        .collect();
    let results = client.write_batch(&requests).await.unwrap();
    assert!(results.iter().all(|r| *r == WriteResult::Ok));

    let mut reader = connected_reader(&sim).await;
    let read_requests: Vec<ReadRequest> = (0..pattern.len())
        .map(|i| rreq(&format!("M{i}"), DataType::Bit))
        .collect();
    let rb = reader.read_batch(&read_requests).await.unwrap();
    for (i, &expected) in pattern.iter().enumerate() {
        assert_eq!(
            rb[i],
            ReadResult::Value(TagValue::Bit(expected)),
            "M{i} round-tripped wrong"
        );
    }
}

/// Hex-notation devices must write at their numeric value: `X1A` writes device
/// 26, not 1 or 10.
#[tokio::test]
async fn hexadecimal_device_numbers_write_at_their_numeric_value() {
    let sim = Simulator::start().await;
    let mut client = SlmpWriteClient::new(fast_config(&sim));
    client.connect().await.unwrap();

    client
        .write_batch(&[bit("X1A", true), bit("X1", false)])
        .await
        .unwrap();
    assert!(sim.get_bit(SlmpDevice::X, 0x1A));
    assert!(!sim.get_bit(SlmpDevice::X, 0x01));
}

/// Both CPU series must work, since the write frame's device field differs
/// (4-byte vs 6-byte) and only one can be the default.
#[tokio::test]
async fn both_cpu_series_write_frame_layouts_work() {
    for cpu in [SlmpCpu::Q, SlmpCpu::R, SlmpCpu::L] {
        let sim = Simulator::start().await;
        let mut client = SlmpWriteClient::new(SlmpConfig {
            cpu,
            ..fast_config(&sim)
        });
        client
            .connect()
            .await
            .unwrap_or_else(|e| panic!("{cpu:?} connect: {e}"));

        let results = client
            .write_batch(&[word("D100", DataType::U16, 1234.0), bit("M7", true)])
            .await
            .unwrap_or_else(|e| panic!("{cpu:?} write_batch: {e}"));
        assert!(results.iter().all(|r| *r == WriteResult::Ok), "{cpu:?}");
        assert_eq!(sim.get_word(SlmpDevice::D, 100), 1234, "{cpu:?} word");
        assert!(sim.get_bit(SlmpDevice::M, 7), "{cpu:?} bit");
    }
}

/// The tripwire this crate's write side needs (the write twin of the read
/// side's `slmp_end_code_is_bad_not_fatal`): a real non-zero end code, built by
/// the real wrapped crate from real bytes on a *write*, must classify as a
/// per-request `Bad` and leave its batch-mates alone, and the connection must
/// stay usable afterwards.
#[tokio::test]
async fn slmp_write_end_code_is_bad_not_fatal() {
    let sim = Simulator::start().await;
    // D0 and D100 are far apart, so the planner puts them in separate groups;
    // injecting on the D0 group must leave the D100 write intact.
    sim.inject_end_code(SlmpDevice::D, 0, 0xC061); // WrongLength

    let mut client = SlmpWriteClient::new(fast_config(&sim));
    client.connect().await.unwrap();

    let results = client
        .write_batch(&[
            word("D0", DataType::I16, 5.0),
            word("D100", DataType::I16, 9.0),
        ])
        .await
        .expect("whole call should still be Ok");

    match &results[0] {
        WriteResult::Bad(PlcWriteError::SlmpEndCode { code, message }) => {
            assert_eq!(*code, 0xC061);
            assert!(
                !message.is_empty(),
                "the end code's symbolic name should survive translation - if the `slmp` \
                 crate changed its error message format, `super::parse_end_code` needs updating"
            );
        }
        other => panic!("expected Bad(SlmpEndCode), got {other:?}"),
    }
    // The un-refused write still landed.
    assert_eq!(results[1], WriteResult::Ok);
    assert_eq!(sim.get_word(SlmpDevice::D, 100), 9);

    // ...and the connection is still usable, which is the whole point of "not
    // fatal": clearing the injection and rewriting D0 works.
    sim.clear_end_code(SlmpDevice::D, 0);
    let results = client
        .write_batch(&[word("D0", DataType::I16, 7.0)])
        .await
        .unwrap();
    assert_eq!(results[0], WriteResult::Ok);
    assert_eq!(sim.get_word(SlmpDevice::D, 0), 7);
}

/// The other half of the pair: a framing failure reaches `classify_io_error`
/// with the same `ErrorKind::InvalidData` as an end code and must still come out
/// fatal and tear the client down.
#[tokio::test]
async fn a_malformed_frame_is_fatal_and_tears_down_the_connection() {
    let sim = Simulator::start().await;
    sim.emit_malformed_frames();

    let mut client = SlmpWriteClient::new(fast_config(&sim));
    client.connect().await.unwrap();

    let err = client
        .write_batch(&[word("D0", DataType::U16, 1.0)])
        .await
        .expect_err("a length-inconsistent frame must fail the whole call");
    assert!(matches!(err, PlcWriteError::Protocol(_)), "got {err:?}");
    assert!(err.is_connection_fatal());

    // Session torn down.
    assert!(matches!(
        client.write_batch(&[word("D0", DataType::U16, 1.0)]).await,
        Err(PlcWriteError::NotConnected)
    ));
}

/// A device/data-type mismatch is resolved before any wire traffic and does not
/// stop the rest of the batch (and must not touch the device).
#[tokio::test]
async fn unsupported_combination_is_bad_without_touching_the_wire() {
    let sim = Simulator::start().await;
    let mut client = SlmpWriteClient::new(fast_config(&sim));
    client.connect().await.unwrap();

    let results = client
        .write_batch(&[
            wreq("D0", DataType::Bit, TagValue::Bit(true)), // bit type at word device
            word("D1", DataType::I16, 9.0),
        ])
        .await
        .unwrap();
    assert!(matches!(
        results[0],
        WriteResult::Bad(PlcWriteError::UnsupportedCombination { .. })
    ));
    assert_eq!(results[1], WriteResult::Ok);
    assert_eq!(sim.get_word(SlmpDevice::D, 1), 9);
}

/// A Modbus address on an SLMP write connection: one `Bad`, not a dead batch,
/// and the wire is never touched for it.
#[tokio::test]
async fn a_modbus_address_is_bad_without_touching_the_wire() {
    let sim = Simulator::start().await;
    let mut client = SlmpWriteClient::new(fast_config(&sim));
    client.connect().await.unwrap();

    let results = client
        .write_batch(&[
            WriteRequest {
                address: Address::parse("40001").unwrap(),
                data_type: DataType::U16,
                value: TagValue::F64(1.0),
            },
            word("D0", DataType::U16, 3.0),
        ])
        .await
        .unwrap();
    assert!(matches!(
        results[0],
        WriteResult::Bad(PlcWriteError::AddressProtocolMismatch { .. })
    ));
    assert_eq!(results[1], WriteResult::Ok);
    assert_eq!(sim.get_word(SlmpDevice::D, 0), 3);
}

/// An un-encodable value is a per-request `Bad` and never reaches the CPU.
#[tokio::test]
async fn an_out_of_range_value_is_bad_without_touching_the_wire() {
    let sim = Simulator::start().await;
    let mut client = SlmpWriteClient::new(fast_config(&sim));
    client.connect().await.unwrap();

    let results = client
        .write_batch(&[
            word("D0", DataType::U16, 70000.0), // out of range for u16
            word("D1", DataType::U16, 5.0),
        ])
        .await
        .unwrap();
    assert!(matches!(
        results[0],
        WriteResult::Bad(PlcWriteError::ValueOutOfRange { .. })
    ));
    assert_eq!(results[1], WriteResult::Ok);
    assert_eq!(sim.get_word(SlmpDevice::D, 1), 5);
    // The out-of-range target was never written.
    assert_eq!(sim.get_word(SlmpDevice::D, 0), 0);
}

#[tokio::test]
async fn response_timeout_fails_the_call_and_tears_down_the_connection() {
    let sim = Simulator::start().await;
    sim.hang();

    let mut client = SlmpWriteClient::new(fast_config(&sim));
    client.connect().await.unwrap();

    let err = client
        .write_batch(&[word("D0", DataType::U16, 1.0)])
        .await
        .expect_err("hung simulator should time out the whole call");
    assert!(matches!(err, PlcWriteError::ResponseTimeout), "got {err:?}");

    assert!(matches!(
        client.write_batch(&[word("D0", DataType::U16, 1.0)]).await,
        Err(PlcWriteError::NotConnected)
    ));
}

#[tokio::test]
async fn disconnect_mid_session_fails_the_call_and_tears_down_the_connection() {
    let sim = Simulator::start().await;
    let mut client = SlmpWriteClient::new(fast_config(&sim));
    client.connect().await.unwrap();

    // Prove the connection works before severing it.
    client
        .write_batch(&[word("D0", DataType::U16, 1.0)])
        .await
        .expect("first write should succeed");

    sim.stop();

    let err = client
        .write_batch(&[word("D0", DataType::U16, 2.0)])
        .await
        .expect_err("severed connection should fail the call");
    assert!(
        err.is_connection_fatal(),
        "a severed session must be fatal, got {err:?}"
    );

    assert!(matches!(
        client.write_batch(&[word("D0", DataType::U16, 1.0)]).await,
        Err(PlcWriteError::NotConnected)
    ));
}

#[tokio::test]
async fn write_batch_before_connect_is_not_connected() {
    let sim = Simulator::start().await;
    let mut client = SlmpWriteClient::new(fast_config(&sim));
    assert!(matches!(
        client.write_batch(&[word("D0", DataType::U16, 1.0)]).await,
        Err(PlcWriteError::NotConnected)
    ));
}

#[tokio::test]
async fn disconnect_then_write_is_not_connected_then_reconnect_works() {
    let sim = Simulator::start().await;
    let mut client = SlmpWriteClient::new(fast_config(&sim));
    client.connect().await.unwrap();
    client
        .write_batch(&[word("D0", DataType::U16, 5.0)])
        .await
        .unwrap();

    client.disconnect().await;
    assert!(matches!(
        client.write_batch(&[word("D0", DataType::U16, 1.0)]).await,
        Err(PlcWriteError::NotConnected)
    ));

    // Reconnecting on the same instance works.
    client.connect().await.expect("reconnect");
    let results = client
        .write_batch(&[word("D0", DataType::U16, 6.0)])
        .await
        .unwrap();
    assert_eq!(results[0], WriteResult::Ok);
    assert_eq!(sim.get_word(SlmpDevice::D, 0), 6);
}

#[tokio::test]
async fn reconnect_after_a_fatal_error_works() {
    let sim1 = Simulator::start().await;
    let mut client = SlmpWriteClient::new(fast_config(&sim1));
    client.connect().await.unwrap();
    sim1.stop();
    let err = client
        .write_batch(&[word("D0", DataType::U16, 1.0)])
        .await
        .unwrap_err();
    assert!(err.is_connection_fatal());

    // A fresh connect (to a new simulator standing in for "the CPU came back")
    // followed by write works like a brand-new client - the reconnect loop is
    // the caller's job, this only proves the client is not permanently wedged.
    let sim2 = Simulator::start().await;
    let mut client2 = SlmpWriteClient::new(fast_config(&sim2));
    client2.connect().await.expect("reconnect to new sim");
    client2
        .write_batch(&[word("D0", DataType::U16, 77.0)])
        .await
        .unwrap();
    assert_eq!(sim2.get_word(SlmpDevice::D, 0), 77);
}

/// A batch of exclusively unservable requests still returns one `Bad` per
/// request rather than an `Err` - the result vector is always `requests.len()`
/// long, in order.
#[tokio::test]
async fn a_batch_of_only_bad_requests_still_returns_one_result_each() {
    let sim = Simulator::start().await;
    let mut client = SlmpWriteClient::new(fast_config(&sim));
    client.connect().await.unwrap();

    let results = client
        .write_batch(&[
            wreq("D0", DataType::Bit, TagValue::Bit(true)), // bit at word device
            word("M0", DataType::U16, 1.0),                 // numeric at bit device
        ])
        .await
        .expect("no wire traffic needed, so no way for this to fail");
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| matches!(r, WriteResult::Bad(_))));
}

#[tokio::test]
async fn an_empty_batch_is_ok_and_empty() {
    let sim = Simulator::start().await;
    let mut client = SlmpWriteClient::new(fast_config(&sim));
    client.connect().await.unwrap();
    assert!(client.write_batch(&[]).await.unwrap().is_empty());
}

/// The broker-sharing seam (W3): [`super::execute_slmp_writes`] runs against a
/// borrowed `slmp::SLMPClient` the caller owns, so the broker can drive writes
/// over the *same* session it reads on. Here we borrow a read client's own
/// concept by standing up a bare `slmp::SLMPClient`, planning, and executing -
/// no `SlmpWriteClient` involved - then read the result back.
#[tokio::test]
async fn execute_slmp_writes_runs_on_a_borrowed_client_for_the_broker() {
    let sim = Simulator::start().await;

    // A bare wrapped client, exactly what the W3 broker will own per CPU.
    let props = slmp::SLMP4EConnectionProps {
        ip: sim.addr.ip().to_string(),
        port: sim.addr.port(),
        cpu: slmp::CPU::R,
        serial_id: 0x0001,
        network_id: 0x00,
        pc_id: 0xFF,
        io_id: 0x03FF,
        area_id: 0x00,
        cpu_timer: 0x0010,
    };
    let mut shared = slmp::SLMPClient::new(props);
    shared.set_send_timeout(Duration::from_millis(100));
    shared.set_recv_timeout(Duration::from_millis(100));
    shared.connect().await.expect("shared connect");

    let requests = [
        word("D0", DataType::U16, 11.0),
        word("D1", DataType::U16, 22.0),
        bit("M0", true),
    ];
    let outcome = crate::plan_slmp_writes(&requests, WordOrder::LowHigh);
    let results = super::execute_slmp_writes(&mut shared, &outcome, requests.len())
        .await
        .expect("execute on borrowed client");
    assert!(results.iter().all(|r| *r == WriteResult::Ok));

    assert_eq!(sim.get_word(SlmpDevice::D, 0), 11);
    assert_eq!(sim.get_word(SlmpDevice::D, 1), 22);
    assert!(sim.get_bit(SlmpDevice::M, 0));

    shared.close().await;
}
