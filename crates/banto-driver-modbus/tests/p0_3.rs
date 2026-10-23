//! P0-3 (docs/banto-hub-driver-refactor-plan.md §8.3), rows 1-4 of
//! docs/banto-hub-driver-io-design.md §11.1, on the new I/O: the real
//! `banto-driver-engine` (`FrameSession`) over a real TCP socket to
//! `banto-plc`'s Modbus simulator, driving the frame-type `ModbusDriver`.
//!
//! | here | was (`banto-plc/src/modbus/integration_tests.rs`) |
//! | --- | --- |
//! | `a_response_split_across_segments_is_read_as_one_response` | the same name (#1) |
//! | `a_late_response_after_a_timeout_is_never_returned_as_a_later_value` | the same name (#2a, client level) |
//! | `the_engine_never_sends_on_a_connection_it_closed_after_a_timeout` | `a_late_response_on_a_reused_stream_is_a_fatal_protocol_error` (#2a, stream level: the engine does not reuse the stream, so what is pinned is that it does not) |
//! | `a_response_with_the_wrong_transaction_id_is_fatal` | the same name (#2b; the driver half is also a unit test in `src/frame_conn.rs`) |
//! | `connect_to_a_closed_port_fails_without_hanging` | the same name (#3) |
//! | `a_mixed_batch_reads_numerics_and_keeps_strings_unsupported` | `read_batch_mixed_reads_numerics_and_keeps_strings_bad` (#4) |
//!
//! #5 (`collection_over_several_periods_uses_exactly_one_socket`) is
//! `banto-collect`'s and moves with the broker's registration (3c-join).

use std::time::{Duration, Instant};

use banto_driver_api::{
    ConnectContext, DataType, DriverAddress, DriverBase, FatalError, ItemError, Session, Settings,
    StringEncoding, TagKind, TagSpec, Value,
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
