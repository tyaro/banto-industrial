//! P0-3 (docs/banto-hub-driver-refactor-plan.md §8.3), rows 1-4 of
//! docs/banto-hub-driver-io-design.md §11.1, and the other behaviours of the
//! removed `ModbusTcpClient` / `ModbusWriteClient` tests (§11.1 note), on the
//! new I/O: the real `banto-driver-engine` (`FrameSession`) over a real TCP
//! socket to `banto-plc`'s Modbus simulator, driving the frame-type
//! `ModbusDriver`.
//!
//! | here | was (`banto-plc/src/modbus/integration_tests.rs`) |
//! | --- | --- |
//! | `a_response_split_across_segments_is_read_as_one_response` | the same name (#1) |
//! | `a_late_response_after_a_timeout_is_never_returned_as_a_later_value` | the same name (#2a, client level) |
//! | `the_engine_never_sends_on_a_connection_it_closed_after_a_timeout` | `a_late_response_on_a_reused_stream_is_a_fatal_protocol_error` (#2a, stream level: the engine does not reuse the stream, so what is pinned is that it does not) |
//! | `a_response_with_the_wrong_transaction_id_is_fatal` | the same name (#2b; the driver half is also a unit test in `src/frame_conn.rs`) |
//! | `connect_to_a_closed_port_fails_without_hanging` | the same name (#3) |
//! | `a_mixed_batch_reads_numerics_and_keeps_strings_unsupported` | `read_batch_mixed_reads_numerics_and_keeps_strings_bad` (#4) |
//! | `normal_batch_reads_every_data_type_correctly` | the same name (also `frame_conformance.rs` C3) |
//! | `mixed_areas_and_types_in_one_batch_each_get_their_own_group` | the same name |
//! | `individual_bad_address_does_not_kill_the_rest_of_the_batch` | the same name |
//! | `exception_code_surfaces_a_readable_message` | the same name |
//! | `unsupported_combination_is_unsupported_and_the_batch_mate_is_still_read` | `unsupported_combination_is_bad_without_touching_the_wire` |
//! | `response_timeout_fails_the_call_and_tears_down_the_connection` | the same name |
//! | `disconnect_mid_session_fails_the_call_and_tears_down_the_connection` | the same name |
//! | `a_dead_connection_refuses_every_call` (+ `the_engine_never_sends_on_a_connection_it_closed_after_a_timeout`) | `read_batch_before_connect_is_not_connected` (the engine has no unconnected state: `connect` returns the session, a dead one answers `Protocol`) |
//! | `disconnect_then_reconnect_works` | the same name |
//! | `performance_smoke_256_tags_x_1000_read_batch_calls` | the same name |
//!
//! Writes (was `banto-plc-write/src/modbus/integration_tests.rs`, over
//! `banto-plc-write`'s write-capable simulator):
//!
//! | here | was |
//! | --- | --- |
//! | `single_register_write_lands_and_reads_back` | the same name (FC6, one wire write) |
//! | `single_coil_write_lands_and_reads_back` | the same name |
//! | `adjacent_register_writes_merge_into_one_fc16_wire_write` | the same name |
//! | `a_gap_between_targets_never_touches_the_skipped_register` | the same name |
//! | `thirty_two_bit_value_uses_the_configured_word_order` | the same name |
//! | `a_device_side_exception_is_a_per_request_bad_not_a_whole_call_error` | the same name |
//! | `a_write_response_timeout_is_fatal_and_the_connection_is_gone` | `a_response_timeout_is_connection_fatal_and_disconnects` |
//! | `a_malformed_write_response_is_fatal` | `a_malformed_response_is_connection_fatal` |
//! | `a_read_only_area_is_unsupported_and_never_reaches_the_wire` | `a_read_only_area_is_a_per_request_bad_never_reaching_the_wire` |
//! | (`a_dead_connection_refuses_every_call`) | `write_batch_before_connect_is_not_connected`, `disconnect_on_a_never_connected_client_is_a_no_op` (no unconnected state in the engine) |
//! | `modbus_exception_is_not_connection_fatal` | stays in `banto-plc-write/src/modbus/mod.rs` (no I/O) |
//! | `modbus_tcp_config_defaults_to_high_low_word_order` | stays in `banto-plc/src/modbus/mod.rs` (no I/O) |
//!
//! #5 (`collection_over_several_periods_uses_exactly_one_socket`) is
//! `banto-collect`'s, over the broker's frame-type registration (3c-join).

use std::time::{Duration, Instant};

use banto_driver_api::{
    ConnectContext, DataType, DriverAddress, DriverBase, FatalError, ItemError, Session, Settings,
    StringEncoding, TagKind, TagSpec, Value, WriteItem,
};
use banto_driver_engine::transport::TransportSpec;
use banto_driver_engine::FrameSession;
use banto_driver_modbus::ModbusDriver;
use banto_plc::modbus::simulator::Simulator;

fn n(data_type: DataType) -> TagKind {
    TagKind::numeric(data_type)
}

fn spec(text: &str, kind: TagKind) -> TagSpec {
    TagSpec::new(ModbusDriver.parse_address(text, &kind).unwrap(), kind)
}

fn settings(sim: &Simulator) -> Settings {
    Settings::from_pairs([
        ("host", sim.addr.ip().to_string()),
        ("port", sim.addr.port().to_string()),
    ])
    .unwrap()
}

async fn connect(sim: &Simulator, response: Duration) -> Result<FrameSession, FatalError> {
    let spec = TransportSpec::Tcp {
        host: sim.addr.ip().to_string(),
        port: sim.addr.port(),
    };
    let cx = ConnectContext::new("p0-3", Duration::from_secs(2), response);
    FrameSession::connect(&ModbusDriver, &settings(sim), &spec, &cx, None).await
}

/// P0-3 #1: a response that reaches the client in several TCP segments -
/// split inside the 7-byte MBAP header, across the header/PDU boundary, and
/// one byte at a time - is read as one response with the right values.
#[tokio::test]
async fn a_response_split_across_segments_is_read_as_one_response() {
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 42);
    sim.set_holding_register(1, 0xFFFF);
    // 40003 as U32: the default word order is low_high, so the low word first.
    sim.set_holding_registers(2, &[0x0002, 0x0001]);
    sim.set_coil(0, true);

    let mut session = connect(&sim, Duration::from_secs(5)).await.unwrap();
    let specs = [
        spec("40001", n(DataType::U16)),
        spec("40002", n(DataType::I16)),
        spec("40003", n(DataType::U32)),
        spec("00001", n(DataType::Bit)),
    ];
    for chunk in [1usize, 3, 7, 8] {
        sim.set_response_chunking(Some((chunk, Duration::from_millis(2))));
        // Twice: the second read proves the stream is still aligned.
        for _ in 0..2 {
            let values = session
                .read_batch(&specs)
                .await
                .unwrap_or_else(|e| panic!("chunk={chunk}: read_batch failed: {e:?}"));
            assert_eq!(
                values,
                vec![
                    Ok(Value::F64(42.0)),
                    Ok(Value::F64(-1.0)),
                    Ok(Value::F64(f64::from(0x0001_0002_u32))),
                    Ok(Value::Bit(true)),
                ],
                "chunk={chunk}"
            );
        }
    }
    session.disconnect().await;
    sim.stop();
}

/// P0-3 #2 (a): the response to a timed-out request arrives later. The
/// timeout is fatal and the engine closes the socket, so the stale value
/// cannot be returned; the next read, on a new connection, is the *current*
/// value, not the one the late response carried.
#[tokio::test]
async fn a_late_response_after_a_timeout_is_never_returned_as_a_later_value() {
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 1);
    let gate = sim.hold_next_response(); // the response carrying `1` is held

    let mut session = connect(&sim, Duration::from_millis(200)).await.unwrap();
    let specs = [spec("40001", n(DataType::I16))];
    let err = session.read_batch(&specs).await.unwrap_err();
    assert!(matches!(err, FatalError::Timeout), "got {err:?}");

    // The PLC now answers the old request, and the register has moved on.
    sim.set_holding_register(0, 2);
    gate.release();

    let mut again = connect(&sim, Duration::from_secs(5)).await.unwrap();
    let values = again.read_batch(&specs).await.unwrap();
    assert_eq!(values, vec![Ok(Value::F64(2.0))]);
    again.disconnect().await;
    sim.stop();
}

/// P0-3 #2 (a), stream level. The old test reused the stream after a timeout
/// and showed that the late response (transaction id 0) was rejected as a
/// mismatch. The engine does not reuse it: after the timeout the session is
/// gone, the next call is refused without touching the wire, and the late
/// response (released after the timeout) is never read by anyone.
#[tokio::test]
async fn the_engine_never_sends_on_a_connection_it_closed_after_a_timeout() {
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 1);
    let gate = sim.hold_next_response();

    let mut session = connect(&sim, Duration::from_millis(200)).await.unwrap();
    let specs = [spec("40001", n(DataType::I16))];
    let err = session.read_batch(&specs).await.unwrap_err();
    assert!(matches!(err, FatalError::Timeout), "got {err:?}");
    assert!(!session.is_connected());

    sim.set_holding_register(0, 2);
    gate.release(); // the stale response (tid 0, value 1) is now on the wire
    let err = session
        .read_batch(&specs)
        .await
        .expect_err("a closed connection must not answer");
    assert!(matches!(err, FatalError::Protocol(_)), "got {err:?}");
    // Nothing was sent after the first request.
    assert_eq!(sim.connection_count(), 1);
    sim.stop();
}

/// P0-3 #2 (b): a response whose transaction id is not the request's is
/// `FatalError::Protocol`, and the connection is gone.
#[tokio::test]
async fn a_response_with_the_wrong_transaction_id_is_fatal() {
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 9);
    sim.set_wrong_transaction_id(true);

    let mut session = connect(&sim, Duration::from_secs(1)).await.unwrap();
    let specs = [spec("40001", n(DataType::I16))];
    let err = session.read_batch(&specs).await.unwrap_err();
    match &err {
        FatalError::Protocol(message) => {
            assert!(message.contains("トランザクション ID"), "{message}")
        }
        other => panic!("expected Protocol(transaction id), got {other:?}"),
    }
    assert!(!session.is_connected());
    let err = session.read_batch(&specs).await.unwrap_err();
    assert!(matches!(err, FatalError::Protocol(_)), "got {err:?}");
    sim.stop();
}

/// P0-3 #3: connecting to a port nobody listens on fails (refused or timed
/// out) within the connect timeout, without hanging. The closed port is made
/// by binding and dropping a plain listener (immediately and
/// deterministically closed).
#[tokio::test]
async fn connect_to_a_closed_port_fails_without_hanging() {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let settings = Settings::from_pairs([
        ("host", "127.0.0.1".to_string()),
        ("port", port.to_string()),
    ])
    .unwrap();
    let spec = TransportSpec::Tcp {
        host: "127.0.0.1".into(),
        port,
    };
    let cx = ConnectContext::new("p0-3", Duration::from_millis(300), Duration::from_secs(1));
    let started = Instant::now();
    let result = FrameSession::connect(&ModbusDriver, &settings, &spec, &cx, None).await;
    let err = result
        .err()
        .expect("connecting to a closed port should fail");
    assert!(
        matches!(err, FatalError::Io(_) | FatalError::Timeout),
        "got {err:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "took {:?}",
        started.elapsed()
    );
}

/// P0-3 #4: numeric and bit requests of several kinds are read in one call
/// and come back at their original positions; a string never reaches the wire
/// (`parse_address` refuses it at registration, R2b-2a; one that is planned
/// anyway is `ItemError::Unsupported`) and costs its batch-mates nothing.
#[tokio::test]
async fn a_mixed_batch_reads_numerics_and_keeps_strings_unsupported() {
    let sim = Simulator::start().await;
    // Registers under the string request too: if the string were (wrongly)
    // read, it would not be unsupported.
    sim.set_holding_registers(0, &[0x4142, 0x4344, 0x0000, 0x0000]);
    sim.set_holding_register(10, 7);
    // 40021 as U32, low word first.
    sim.set_holding_registers(20, &[0x0002, 0x0001]);
    sim.set_input_register(0, 100);
    sim.set_coil(5, true);

    // A string spec as the frame API may hand it over without going through
    // the save path.
    let string = |canonical: &str, words: u16| {
        TagSpec::new(
            DriverAddress::new(
                ModbusDriver.descriptor().id.clone(),
                canonical,
                "holding_register",
                true,
                words,
            ),
            TagKind::string(words, StringEncoding::ShiftJis),
        )
    };
    let specs = [
        string("400001", 4),
        spec("40011", n(DataType::I16)),
        spec("40021", n(DataType::U32)),
        spec("30001", n(DataType::U16)),
        string("400031", 2),
        spec("00006", n(DataType::Bit)),
    ];
    let mut session = connect(&sim, Duration::from_secs(5)).await.unwrap();
    let values = session.read_batch(&specs).await.expect("the call is Ok");
    assert_eq!(values.len(), specs.len());
    for at in [0usize, 4] {
        assert!(
            matches!(values[at], Err(ItemError::Unsupported(_))),
            "{at}: {:?}",
            values[at]
        );
    }
    assert_eq!(values[1], Ok(Value::F64(7.0)));
    assert_eq!(values[2], Ok(Value::F64(f64::from(0x0001_0002_u32))));
    assert_eq!(values[3], Ok(Value::F64(100.0)));
    assert_eq!(values[5], Ok(Value::Bit(true)));
    session.disconnect().await;
    sim.stop();
}

/// 0xFFFF をまたぐ複数レジスタのタグは、そのタグだけが `Unsupported` になる。
/// となりの正しいタグは同じ要求に混ざらず、値が読める。ちょうど 0xFFFF で
/// 終わるものは通る。
#[tokio::test]
async fn a_span_past_the_address_space_is_unsupported_alone() {
    let sim = Simulator::start().await;
    sim.set_holding_register(65534, 11);
    sim.set_holding_register(65535, 22);
    sim.set_holding_registers(65534, &[0x0003, 0x0004]);

    let mut session = connect(&sim, Duration::from_secs(5)).await.unwrap();
    let specs = [
        spec("465535", n(DataType::U16)),
        // offset 65535 の U32 は 65537 で終わる。`parse_address` は末尾が
        // 範囲を超えるアドレスを断るので、保存を経ない spec（計画の守り）を
        // U16 で解いた住所に U32 の種類を付けて作る。
        TagSpec::new(
            ModbusDriver
                .parse_address("465536", &n(DataType::U16))
                .unwrap(),
            n(DataType::U32),
        ),
        // offset 65534 の U32 は 65536 ちょうどで終わる。
        spec("465535", n(DataType::U32)),
    ];
    let values = session.read_batch(&specs).await.expect("the call is Ok");
    assert_eq!(values.len(), 3);
    assert_eq!(values[0], Ok(Value::F64(3.0)));
    assert!(
        matches!(values[1], Err(ItemError::Unsupported(_))),
        "{:?}",
        values[1]
    );
    assert_eq!(values[2], Ok(Value::F64(f64::from(0x0004_0003_u32))));
    session.disconnect().await;
    sim.stop();
}

// --- the other behaviours of the removed `ModbusTcpClient` tests -------------

fn read_spec_for(text: &str, data_type: DataType) -> TagSpec {
    spec(text, n(data_type))
}

/// Every data type in one batch, in the order asked. The column default
/// `low_high` applies, so the low word of a 32-bit value comes first.
#[tokio::test]
async fn normal_batch_reads_every_data_type_correctly() {
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 42); // u16
    sim.set_holding_register(1, 0xFFFF); // i16 == -1
    sim.set_holding_registers(2, &[0x0002, 0x0001]); // u32 == 0x0001_0002 (low_high)
    sim.set_input_register(0, 100);
    sim.set_coil(0, true);
    sim.set_discrete_input(0, false);

    let mut session = connect(&sim, Duration::from_millis(500)).await.unwrap();
    let specs = [
        read_spec_for("40001", DataType::U16),
        read_spec_for("40002", DataType::I16),
        read_spec_for("40003", DataType::U32),
        read_spec_for("30001", DataType::U16),
        read_spec_for("00001", DataType::Bit),
        read_spec_for("10001", DataType::Bit),
    ];
    let values = session.read_batch(&specs).await.expect("read_batch ok");
    assert_eq!(
        values,
        vec![
            Ok(Value::F64(42.0)),
            Ok(Value::F64(-1.0)),
            Ok(Value::F64(f64::from(0x0001_0002_u32))),
            Ok(Value::F64(100.0)),
            Ok(Value::Bit(true)),
            Ok(Value::Bit(false)),
        ]
    );
    session.disconnect().await;
    sim.stop();
}

#[tokio::test]
async fn mixed_areas_and_types_in_one_batch_each_get_their_own_group() {
    let sim = Simulator::start().await;
    sim.set_holding_register(10, 7);
    sim.set_coil(5, true);

    let mut session = connect(&sim, Duration::from_millis(500)).await.unwrap();
    let values = session
        .read_batch(&[
            read_spec_for("40011", DataType::I16),
            read_spec_for("00006", DataType::Bit),
        ])
        .await
        .unwrap();
    assert_eq!(values, vec![Ok(Value::F64(7.0)), Ok(Value::Bit(true))]);
    session.disconnect().await;
    sim.stop();
}

/// A Modbus exception for one group must not affect an unrelated group in
/// the same batch (docs/plan.md I2's third fixed invariant).
#[tokio::test]
async fn individual_bad_address_does_not_kill_the_rest_of_the_batch() {
    let sim = Simulator::start().await;
    sim.set_holding_register(100, 55);
    // Offsets 0 and 100 are far enough apart that planning puts them in
    // separate groups, so the exception on the group at offset 0 leaves the
    // one at offset 100 untouched.
    sim.inject_exception(0x03, 0, 0x02); // illegal data address

    let mut session = connect(&sim, Duration::from_millis(500)).await.unwrap();
    let values = session
        .read_batch(&[
            read_spec_for("40001", DataType::I16),
            read_spec_for("40101", DataType::I16),
        ])
        .await
        .expect("the whole call is still Ok");
    match &values[0] {
        Err(ItemError::Device(e)) => assert_eq!(e.code, 0x02),
        other => panic!("expected Device(0x02), got {other:?}"),
    }
    assert_eq!(values[1], Ok(Value::F64(55.0)));
    assert!(session.is_connected(), "an exception is not fatal");
    session.disconnect().await;
    sim.stop();
}

#[tokio::test]
async fn exception_code_surfaces_a_readable_message() {
    let sim = Simulator::start().await;
    sim.inject_exception(0x03, 0, 0x02);

    let mut session = connect(&sim, Duration::from_millis(500)).await.unwrap();
    let values = session
        .read_batch(&[read_spec_for("40001", DataType::I16)])
        .await
        .unwrap();
    match &values[0] {
        Err(ItemError::Device(e)) => {
            assert_eq!(e.code, 0x02);
            assert!(
                e.message.contains("0x03"),
                "the function code: {}",
                e.message
            );
            assert!(e.message.len() > "FC=0x03 ".len(), "{}", e.message);
        }
        other => panic!("expected Device, got {other:?}"),
    }
    session.disconnect().await;
    sim.stop();
}

/// A kind the area does not take (a bit at a register address) is decided
/// before the wire (`modbus_accepts`); its batch-mate is still read. (The old
/// test asserted `UnsupportedCombination` as a `Bad`; the frame API's word for
/// it is `ItemError::Unsupported`.)
#[tokio::test]
async fn unsupported_combination_is_unsupported_and_the_batch_mate_is_still_read() {
    let sim = Simulator::start().await;
    sim.set_holding_register(1, 9);

    // `parse_address` refuses a bit at a register at registration; a spec the
    // save path never saw is built by hand.
    let bit_at_register = TagSpec::new(
        DriverAddress::new(
            ModbusDriver.descriptor().id.clone(),
            "400001",
            "holding_register",
            true,
            1,
        ),
        n(DataType::Bit),
    );
    let mut session = connect(&sim, Duration::from_millis(500)).await.unwrap();
    let values = session
        .read_batch(&[bit_at_register, read_spec_for("40002", DataType::I16)])
        .await
        .unwrap();
    assert!(
        matches!(values[0], Err(ItemError::Unsupported(_))),
        "{:?}",
        values[0]
    );
    assert_eq!(values[1], Ok(Value::F64(9.0)));
    session.disconnect().await;
    sim.stop();
}

#[tokio::test]
async fn response_timeout_fails_the_call_and_tears_down_the_connection() {
    let sim = Simulator::start().await;
    sim.hang();

    let mut session = connect(&sim, Duration::from_millis(100)).await.unwrap();
    let specs = [read_spec_for("40001", DataType::I16)];
    let err = session.read_batch(&specs).await.unwrap_err();
    assert!(matches!(err, FatalError::Timeout), "got {err:?}");
    assert!(!session.is_connected());
    // Further calls fail at once instead of waiting on the broken stream.
    let started = Instant::now();
    let err = session.read_batch(&specs).await.unwrap_err();
    assert!(matches!(err, FatalError::Protocol(_)), "got {err:?}");
    assert!(started.elapsed() < Duration::from_millis(50));
    sim.stop();
}

#[tokio::test]
async fn disconnect_mid_session_fails_the_call_and_tears_down_the_connection() {
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 1);

    let mut session = connect(&sim, Duration::from_millis(500)).await.unwrap();
    let specs = [read_spec_for("40001", DataType::I16)];
    session.read_batch(&specs).await.expect("first read");

    sim.stop();

    let err = session.read_batch(&specs).await.unwrap_err();
    assert!(
        matches!(err, FatalError::Io(_) | FatalError::Timeout),
        "got {err:?}"
    );
    assert!(!session.is_connected());
    let err = session.read_batch(&specs).await.unwrap_err();
    assert!(matches!(err, FatalError::Protocol(_)), "got {err:?}");
}

/// What `NotConnected` pinned in the old clients: a dead connection refuses
/// every call, reads and writes alike, and `disconnect` on it is harmless.
/// (The engine has no unconnected state: `connect` returns the session.)
#[tokio::test]
async fn a_dead_connection_refuses_every_call() {
    let sim = Simulator::start().await;
    sim.hang();
    let mut session = connect(&sim, Duration::from_millis(100)).await.unwrap();
    let read = [read_spec_for("40001", DataType::I16)];
    assert!(matches!(
        session.read_batch(&read).await,
        Err(FatalError::Timeout)
    ));
    let write = [WriteItem::new(read[0].clone(), Value::F64(1.0))];
    assert!(matches!(
        session.write_batch(&write).await,
        Err(FatalError::Protocol(_))
    ));
    session.disconnect().await; // must not panic
    session.disconnect().await;
    sim.stop();
}

#[tokio::test]
async fn disconnect_then_reconnect_works() {
    let sim1 = Simulator::start().await;
    let mut session = connect(&sim1, Duration::from_millis(500)).await.unwrap();
    sim1.stop();
    let specs = [read_spec_for("40001", DataType::I16)];
    let err = session.read_batch(&specs).await.unwrap_err();
    assert!(
        matches!(err, FatalError::Io(_) | FatalError::Timeout),
        "got {err:?}"
    );

    // The reconnect loop is the broker's; this is the driver's half: after a
    // fatal failure a fresh connection works like a brand new one.
    let sim2 = Simulator::start().await;
    sim2.set_holding_register(0, 77);
    let mut again = connect(&sim2, Duration::from_millis(500)).await.unwrap();
    let values = again.read_batch(&specs).await.unwrap();
    assert_eq!(values, vec![Ok(Value::F64(77.0))]);
    again.disconnect().await;
    sim2.stop();
}

/// Performance smoke test (docs/plan.md I2 §7): 256 tags over contiguous
/// holding registers, split into 3 wire round trips by the register-quantity
/// limit. Not a gate (no assertion on the timing); the printed number is a
/// data point toward the 100 ms collection-cycle target.
#[tokio::test]
async fn performance_smoke_256_tags_x_1000_read_batch_calls() {
    const TAG_COUNT: u16 = 256;
    const ITERATIONS: u32 = 1000;

    let sim = Simulator::start().await;
    let values: Vec<u16> = (0..TAG_COUNT).collect();
    sim.set_holding_registers(0, &values);

    let mut session = connect(&sim, Duration::from_secs(1)).await.unwrap();
    let specs: Vec<TagSpec> = (0..TAG_COUNT)
        .map(|i| read_spec_for(&format!("4{:05}", u32::from(i) + 1), DataType::U16))
        .collect();
    session.read_batch(&specs).await.unwrap(); // warm-up

    let started = Instant::now();
    for _ in 0..ITERATIONS {
        let results = session.read_batch(&specs).await.unwrap();
        assert_eq!(results.len(), TAG_COUNT as usize);
        assert!(results.iter().all(Result::is_ok));
    }
    let elapsed = started.elapsed();
    println!(
        "[banto-driver-modbus perf smoke] {TAG_COUNT} tags x {ITERATIONS} read_batch calls \
         (loopback simulator, engine) in {elapsed:?} total, {:?}/call average \
         (100ms/cycle target, recorder-requirements.md §3.1)",
        elapsed / ITERATIONS
    );
    session.disconnect().await;
    sim.stop();
}

// --- writes (banto-plc-write's write-capable simulator) ------------------------

mod writes {
    use super::*;
    use banto_plc_write::modbus::simulator::Simulator as WriteSimulator;

    async fn connect_write(
        sim: &WriteSimulator,
        word_order: &str,
        response: Duration,
    ) -> Result<FrameSession, FatalError> {
        let settings = Settings::from_pairs([
            ("host", sim.addr.ip().to_string()),
            ("port", sim.addr.port().to_string()),
            ("word_order", word_order.to_string()),
        ])
        .unwrap();
        let spec = TransportSpec::Tcp {
            host: sim.addr.ip().to_string(),
            port: sim.addr.port(),
        };
        let cx = ConnectContext::new("p0-3", Duration::from_secs(1), response);
        FrameSession::connect(&ModbusDriver, &settings, &spec, &cx, None).await
    }

    fn word(text: &str, data_type: DataType, v: f64) -> WriteItem {
        WriteItem::new(spec(text, n(data_type)), Value::F64(v))
    }

    fn coil(text: &str, v: bool) -> WriteItem {
        WriteItem::new(spec(text, n(DataType::Bit)), Value::Bit(v))
    }

    #[tokio::test]
    async fn single_register_write_lands_and_reads_back() {
        let sim = WriteSimulator::start().await;
        let mut session = connect_write(&sim, "high_low", Duration::from_millis(500))
            .await
            .unwrap();
        let results = session
            .write_batch(&[word("40001", DataType::U16, 42.0)])
            .await
            .unwrap();
        assert_eq!(results, vec![Ok(())]);
        assert_eq!(sim.get_holding_register(0), 42);
        // A single request must use FC6, not a one-element FC16.
        assert_eq!(sim.write_command_count(), 1);
        session.disconnect().await;
        sim.stop();
    }

    #[tokio::test]
    async fn single_coil_write_lands_and_reads_back() {
        let sim = WriteSimulator::start().await;
        let mut session = connect_write(&sim, "high_low", Duration::from_millis(500))
            .await
            .unwrap();
        let results = session.write_batch(&[coil("00001", true)]).await.unwrap();
        assert_eq!(results, vec![Ok(())]);
        assert!(sim.get_coil(0));
        session.disconnect().await;
        sim.stop();
    }

    #[tokio::test]
    async fn adjacent_register_writes_merge_into_one_fc16_wire_write() {
        let sim = WriteSimulator::start().await;
        let mut session = connect_write(&sim, "high_low", Duration::from_millis(500))
            .await
            .unwrap();
        let results = session
            .write_batch(&[
                word("40001", DataType::U16, 1.0),
                word("40002", DataType::U16, 2.0),
                word("40003", DataType::U16, 3.0),
            ])
            .await
            .unwrap();
        assert_eq!(results, vec![Ok(()); 3]);
        assert_eq!(sim.get_holding_register(0), 1);
        assert_eq!(sim.get_holding_register(1), 2);
        assert_eq!(sim.get_holding_register(2), 3);
        // Three adjacent requests cost exactly one wire write (FC16).
        assert_eq!(sim.write_command_count(), 1);
        session.disconnect().await;
        sim.stop();
    }

    /// The load-bearing safety property, end to end: a gap is never bridged,
    /// so the register in between is never touched.
    #[tokio::test]
    async fn a_gap_between_targets_never_touches_the_skipped_register() {
        let sim = WriteSimulator::start().await;
        sim.set_holding_register(1, 0xBEEF);
        let mut session = connect_write(&sim, "high_low", Duration::from_millis(500))
            .await
            .unwrap();
        let results = session
            .write_batch(&[
                word("40001", DataType::U16, 1.0),
                word("40003", DataType::U16, 3.0),
            ])
            .await
            .unwrap();
        assert_eq!(results, vec![Ok(()); 2]);
        assert_eq!(sim.get_holding_register(0), 1);
        assert_eq!(
            sim.get_holding_register(1),
            0xBEEF,
            "must be left untouched"
        );
        assert_eq!(sim.get_holding_register(2), 3);
        assert_eq!(sim.write_command_count(), 2);
        session.disconnect().await;
        sim.stop();
    }

    /// A 32-bit value's word order is the connection's: `high_low` puts the
    /// high word first, `low_high` (the column default) the low word.
    #[tokio::test]
    async fn thirty_two_bit_value_uses_the_configured_word_order() {
        for (order, first, second) in [("high_low", 0x0001, 0x0002), ("low_high", 0x0002, 0x0001)] {
            let sim = WriteSimulator::start().await;
            let mut session = connect_write(&sim, order, Duration::from_millis(500))
                .await
                .unwrap();
            let results = session
                .write_batch(&[word("40001", DataType::U32, f64::from(0x0001_0002_u32))])
                .await
                .unwrap();
            assert_eq!(results, vec![Ok(())], "{order}");
            assert_eq!(sim.get_holding_register(0), first, "{order}");
            assert_eq!(sim.get_holding_register(1), second, "{order}");
            session.disconnect().await;
            sim.stop();
        }
    }

    #[tokio::test]
    async fn a_device_side_exception_is_a_per_request_bad_not_a_whole_call_error() {
        let sim = WriteSimulator::start().await;
        sim.inject_exception(0x06, 0, 0x02); // FC6, offset 0: illegal data address
        let mut session = connect_write(&sim, "high_low", Duration::from_millis(500))
            .await
            .unwrap();
        let results = session
            .write_batch(&[
                word("40001", DataType::U16, 1.0), // exception injected here
                word("40101", DataType::U16, 2.0), // unaffected, different group
            ])
            .await
            .unwrap();
        match &results[0] {
            Err(ItemError::Device(e)) => {
                assert_eq!(e.code, 0x02);
                assert!(e.message.contains("0x06"), "{}", e.message);
            }
            other => panic!("expected Device, got {other:?}"),
        }
        assert_eq!(results[1], Ok(()));
        assert_eq!(sim.get_holding_register(100), 2);
        assert!(session.is_connected(), "an exception is not fatal");
        session.disconnect().await;
        sim.stop();
    }

    #[tokio::test]
    async fn a_write_response_timeout_is_fatal_and_the_connection_is_gone() {
        let sim = WriteSimulator::start().await;
        sim.hang();
        let mut session = connect_write(&sim, "high_low", Duration::from_millis(200))
            .await
            .unwrap();
        let items = [word("40001", DataType::U16, 1.0)];
        let err = session.write_batch(&items).await.unwrap_err();
        assert!(matches!(err, FatalError::Timeout), "got {err:?}");
        assert!(!session.is_connected());
        let again = session.write_batch(&items).await.unwrap_err();
        assert!(matches!(again, FatalError::Protocol(_)), "got {again:?}");
        sim.stop();
    }

    #[tokio::test]
    async fn a_malformed_write_response_is_fatal() {
        let sim = WriteSimulator::start().await;
        sim.emit_malformed_frames();
        let mut session = connect_write(&sim, "high_low", Duration::from_millis(200))
            .await
            .unwrap();
        let err = session
            .write_batch(&[word("40001", DataType::U16, 1.0)])
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                FatalError::Framing(_) | FatalError::Protocol(_) | FatalError::Timeout
            ),
            "got {err:?}"
        );
        assert!(!session.is_connected());
        sim.stop();
    }

    #[tokio::test]
    async fn a_read_only_area_is_unsupported_and_never_reaches_the_wire() {
        let sim = WriteSimulator::start().await;
        let mut session = connect_write(&sim, "high_low", Duration::from_millis(500))
            .await
            .unwrap();
        let results = session
            .write_batch(&[
                coil("10001", true), // discrete input, read-only
                word("40001", DataType::U16, 1.0),
            ])
            .await
            .unwrap();
        assert!(
            matches!(results[0], Err(ItemError::Unsupported(_))),
            "{:?}",
            results[0]
        );
        assert_eq!(results[1], Ok(()));
        // The refused one never reached the wire - only the good one did.
        assert_eq!(sim.write_command_count(), 1);
        session.disconnect().await;
        sim.stop();
    }
}
