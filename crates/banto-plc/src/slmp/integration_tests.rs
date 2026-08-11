//! End-to-end tests for [`super::SlmpClient`] against
//! [`super::simulator::Simulator`] - the scenarios a hand-built byte array
//! cannot exercise: real TCP framing through the wrapped `slmp` crate, real
//! timeouts, a real dropped connection. `address.rs`/`planning.rs` already
//! cover the pure logic in isolation, and `mod.rs`'s unit tests cover the error
//! classifier over hand-built `slmp::SlmpError` values; this file is about the parts that only
//! exist once the wrapped crate and a socket are involved.
//!
//! Structured to mirror `modbus/integration_tests.rs` case for case, so the
//! two implementations' behaviour can be compared by reading them side by side.
//! The trait's three invariants (`client.rs`) are supposed to hold identically
//! for both, and the only way to keep that true is to test it the same way
//! twice.

use std::time::Duration;

use super::address::SlmpDevice;
use super::simulator::Simulator;
use super::{SlmpClient, SlmpConfig, SlmpCpu};
use crate::address::Address;
use crate::client::PlcClient;
use crate::decode::WordOrder;
use crate::error::PlcError;
use crate::types::{DataType, ReadRequest, ReadResult, TagValue};

fn req(raw: &str, data_type: DataType) -> ReadRequest {
    ReadRequest {
        address: Address::parse_slmp(raw).unwrap_or_else(|e| panic!("{raw} should parse: {e}")),
        data_type,
    }
}

/// Config pointed at `sim`, with short timeouts so failure-path tests
/// (timeout, disconnect) do not slow the suite down.
fn fast_config(sim: &Simulator) -> SlmpConfig {
    SlmpConfig {
        host: sim.addr.ip().to_string(),
        port: sim.addr.port(),
        connect_timeout: Duration::from_millis(500),
        response_timeout: Duration::from_millis(100),
        ..Default::default()
    }
}

#[tokio::test]
async fn normal_batch_reads_every_data_type_correctly() {
    let sim = Simulator::start().await;
    sim.set_word(SlmpDevice::D, 0, 42); // u16
    sim.set_word(SlmpDevice::D, 1, 0xFFFF); // i16 == -1
                                            // MELSEC stores 32-bit values low word first, so D2 is the low half.
    sim.set_words(SlmpDevice::D, 2, &[0x0002, 0x0001]); // u32 == 0x0001_0002
    sim.set_word(SlmpDevice::R, 0, 100);
    sim.set_bit(SlmpDevice::M, 0, true);
    sim.set_bit(SlmpDevice::X, 0, false);

    let mut client = SlmpClient::new(fast_config(&sim));
    client.connect().await.expect("connect should succeed");

    let requests = [
        req("D0", DataType::U16),
        req("D1", DataType::I16),
        req("D2", DataType::U32),
        req("R0", DataType::U16),
        req("M0", DataType::Bit),
        req("X0", DataType::Bit),
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

/// T8 (docs/tag-server-design.md §6.1), end-to-end through a real `SLMPClient`
/// + wire round trip: `D100.5` reads the same word `D100` an ordinary numeric
/// tag would, decoded down to one bit - and a plain numeric read of the same
/// register (the regression half of this test) is completely unaffected by a
/// bit-in-word tag sharing its group.
#[tokio::test]
async fn bit_in_word_tag_shares_the_ordinary_word_read_and_extracts_its_bit() {
    let sim = Simulator::start().await;
    // 0x1234 = 0b0001_0010_0011_0100 - bit 2, 4, 5, 9, 12 set.
    sim.set_word(SlmpDevice::D, 100, 0x1234);

    let mut client = SlmpClient::new(fast_config(&sim));
    client.connect().await.expect("connect should succeed");

    let requests = [
        req("D100.5", DataType::Bit),  // set
        req("D100.0", DataType::Bit),  // clear
        req("D100.15", DataType::Bit), // clear (top bit)
        req("D100", DataType::U16),    // plain numeric read of the same word
    ];
    let results = client.read_batch(&requests).await.expect("read_batch ok");

    assert_eq!(results.len(), requests.len());
    assert_eq!(results[0], ReadResult::Value(TagValue::Bit(true)));
    assert_eq!(results[1], ReadResult::Value(TagValue::Bit(false)));
    assert_eq!(results[2], ReadResult::Value(TagValue::Bit(false)));
    assert_eq!(results[3], ReadResult::Value(TagValue::F64(0x1234 as f64)));
}

/// Every bit position 0..=15 decodes correctly, not just a couple of
/// hand-picked ones - the full-coverage complement to the test above.
#[tokio::test]
async fn bit_in_word_tag_decodes_every_bit_position() {
    let sim = Simulator::start().await;
    sim.set_word(SlmpDevice::D, 200, 0b1010_1010_1010_1010);

    let mut client = SlmpClient::new(fast_config(&sim));
    client.connect().await.expect("connect should succeed");

    let requests: Vec<ReadRequest> = (0..=15)
        .map(|bit| req(&format!("D200.{bit}"), DataType::Bit))
        .collect();
    let results = client.read_batch(&requests).await.expect("read_batch ok");

    for (bit, result) in results.iter().enumerate() {
        let expected = bit % 2 == 1; // 0b1010_1010_1010_1010: odd bits set
        assert_eq!(
            *result,
            ReadResult::Value(TagValue::Bit(expected)),
            "bit {bit} should decode to {expected}"
        );
    }
}

/// The default word order is the one thing most likely to be silently wrong
/// against real hardware, so it gets its own end-to-end case in both
/// directions: the same two devices must decode to different numbers.
#[tokio::test]
async fn default_low_high_word_order_matches_melsec_storage() {
    let sim = Simulator::start().await;
    // f32 1.5 = 0x3FC00000. Low word 0x0000 in D0, high word 0x3FC0 in D1.
    sim.set_words(SlmpDevice::D, 0, &[0x0000, 0x3FC0]);

    let mut client = SlmpClient::new(fast_config(&sim));
    client.connect().await.unwrap();
    let results = client
        .read_batch(&[req("D0", DataType::F32)])
        .await
        .unwrap();
    assert_eq!(results[0], ReadResult::Value(TagValue::F64(1.5)));

    // Same bytes, opposite configured order, must *not* decode to 1.5 - proving
    // the setting is actually plumbed through rather than coincidentally right.
    let mut swapped = SlmpClient::new(SlmpConfig {
        word_order: WordOrder::HighLow,
        ..fast_config(&sim)
    });
    swapped.connect().await.unwrap();
    let results = swapped
        .read_batch(&[req("D0", DataType::F32)])
        .await
        .unwrap();
    assert_ne!(results[0], ReadResult::Value(TagValue::F64(1.5)));
}

#[tokio::test]
async fn mixed_devices_and_types_in_one_batch_each_get_their_own_group() {
    let sim = Simulator::start().await;
    sim.set_word(SlmpDevice::D, 10, 7);
    sim.set_bit(SlmpDevice::M, 5, true);

    let mut client = SlmpClient::new(fast_config(&sim));
    client.connect().await.unwrap();

    let results = client
        .read_batch(&[req("D10", DataType::I16), req("M5", DataType::Bit)])
        .await
        .unwrap();
    assert_eq!(results[0], ReadResult::Value(TagValue::F64(7.0)));
    assert_eq!(results[1], ReadResult::Value(TagValue::Bit(true)));
}

/// Bit-unit responses pack two points per byte (see the simulator's
/// `build_response`); a run long enough to span several bytes, with a
/// deliberately irregular pattern and an odd length, is what proves the
/// nibble packing is decoded in the right order rather than merely
/// self-consistently.
#[tokio::test]
async fn a_long_odd_length_bit_run_decodes_in_the_right_order() {
    let sim = Simulator::start().await;
    let pattern = [
        true, false, false, true, true, true, false, false, true, false, true,
    ];
    for (i, &v) in pattern.iter().enumerate() {
        sim.set_bit(SlmpDevice::M, i as u32, v);
    }

    let mut client = SlmpClient::new(fast_config(&sim));
    client.connect().await.unwrap();

    let requests: Vec<ReadRequest> = (0..pattern.len())
        .map(|i| req(&format!("M{i}"), DataType::Bit))
        .collect();
    let results = client.read_batch(&requests).await.unwrap();

    for (i, &expected) in pattern.iter().enumerate() {
        assert_eq!(
            results[i],
            ReadResult::Value(TagValue::Bit(expected)),
            "M{i} decoded wrong"
        );
    }
}

/// Hex-notation devices must reach the wire at their numeric value: a tag
/// written `X1A` has to read device 26, not device 1 or 10.
#[tokio::test]
async fn hexadecimal_device_numbers_reach_the_wire_as_their_numeric_value() {
    let sim = Simulator::start().await;
    sim.set_bit(SlmpDevice::X, 0x1A, true);
    sim.set_bit(SlmpDevice::X, 1, false);

    let mut client = SlmpClient::new(fast_config(&sim));
    client.connect().await.unwrap();

    let results = client
        .read_batch(&[req("X1A", DataType::Bit), req("X1", DataType::Bit)])
        .await
        .unwrap();
    assert_eq!(results[0], ReadResult::Value(TagValue::Bit(true)));
    assert_eq!(results[1], ReadResult::Value(TagValue::Bit(false)));
}

/// Both CPU series must work, since the frame layout differs (4-byte vs 6-byte
/// device field, different subcommands) and only one of them can be the
/// default.
#[tokio::test]
async fn both_cpu_series_frame_layouts_work() {
    for cpu in [SlmpCpu::Q, SlmpCpu::R, SlmpCpu::L] {
        let sim = Simulator::start().await;
        sim.set_word(SlmpDevice::D, 100, 1234);
        sim.set_bit(SlmpDevice::M, 7, true);

        let mut client = SlmpClient::new(SlmpConfig {
            cpu,
            ..fast_config(&sim)
        });
        client
            .connect()
            .await
            .unwrap_or_else(|e| panic!("{cpu:?} connect: {e}"));

        let results = client
            .read_batch(&[req("D100", DataType::U16), req("M7", DataType::Bit)])
            .await
            .unwrap_or_else(|e| panic!("{cpu:?} read_batch: {e}"));
        assert_eq!(
            results[0],
            ReadResult::Value(TagValue::F64(1234.0)),
            "{cpu:?} word read"
        );
        assert_eq!(
            results[1],
            ReadResult::Value(TagValue::Bit(true)),
            "{cpu:?} bit read"
        );
    }
}

/// The tripwire this module exists for (see `mod.rs`'s doc comment and the
/// `slmp` note in the workspace `Cargo.toml`): a real non-zero end code, built
/// by the real wrapped crate from real bytes, must classify as a *per-request*
/// `Bad` and leave its batch-mates alone.
///
/// H9 (docs/h9-slmp-structured-error-spec.md, 2026-08-12) replaced this test's
/// original message-text tripwire (`slmp` 0.1.x reported everything as
/// `std::io::Error`, so this asserted on the exact wording of its message)
/// with a direct check on the real crate's structured `slmp::SlmpError`: the
/// first half below drives a bare `slmp::SLMPClient` against the same
/// injected end code and asserts it comes back as `SlmpError::Device { .. }`,
/// not merely "some `Err`". If a future `slmp` release ever reclassified a
/// non-zero end code as `Framing` instead, this is what fails - not a string
/// comparison.
#[tokio::test]
async fn slmp_end_code_is_bad_not_fatal() {
    let sim = Simulator::start().await;
    sim.set_word(SlmpDevice::D, 100, 55);
    // D0 and D100 are far enough apart (gap > MAX_GAP) that planning puts them
    // in separate groups, so injecting on the D0 group must leave D100 intact.
    sim.inject_end_code(SlmpDevice::D, 0, 0xC059);

    // Structured check on the wrapped crate's own error type, bypassing this
    // crate's classification entirely.
    let mut raw = slmp::SLMPClient::new(fast_config(&sim).to_wire_props());
    raw.set_send_timeout(Duration::from_millis(100));
    raw.set_recv_timeout(Duration::from_millis(100));
    raw.connect().await.expect("raw connect");
    let raw_err = raw
        .bulk_read(
            slmp::Device {
                device_type: SlmpDevice::D.to_wire(),
                address: 0,
            },
            1,
            slmp::DataType::I16,
        )
        .await
        .expect_err("an injected end code must surface as an Err");
    assert!(
        matches!(raw_err, slmp::SlmpError::Device { end_code: 0xC059 }),
        "expected SlmpError::Device {{ end_code: 0xC059 }}, got {raw_err:?}"
    );
    raw.close().await;

    // This crate's own classification of the same condition, through the
    // full client - proves `classify_slmp_error` maps `Device` onto
    // `PlcError::SlmpEndCode` and that `read_batch` treats it as per-request.
    let mut client = SlmpClient::new(fast_config(&sim));
    client.connect().await.unwrap();

    let results = client
        .read_batch(&[req("D0", DataType::I16), req("D100", DataType::I16)])
        .await
        .expect("whole call should still be Ok");

    match &results[0] {
        ReadResult::Bad(PlcError::SlmpEndCode { code, message }) => {
            assert_eq!(*code, 0xC059);
            assert!(
                !message.is_empty(),
                "the end code's symbolic name should survive translation"
            );
        }
        other => panic!("expected Bad(SlmpEndCode), got {other:?}"),
    }
    assert_eq!(results[1], ReadResult::Value(TagValue::F64(55.0)));

    // ...and the connection is still usable afterwards, which is the whole
    // point of calling it non-fatal.
    sim.clear_end_code(SlmpDevice::D, 0);
    sim.set_word(SlmpDevice::D, 0, 9);
    let results = client
        .read_batch(&[req("D0", DataType::I16)])
        .await
        .unwrap();
    assert_eq!(results[0], ReadResult::Value(TagValue::F64(9.0)));
}

/// The other half of the pair: a framing failure reaches
/// [`super::classify_slmp_error`] as a *different* `slmp::SlmpError` variant
/// (`Framing`, not `Device`) even though the pre-H9 wrapped crate reported
/// both through the same `io::ErrorKind::InvalidData` - and must still come
/// out fatal. Together with the test above, this is what proves H9's
/// structured `SlmpError` is actually doing the separating, not this crate
/// merely assuming it.
#[tokio::test]
async fn a_malformed_frame_is_fatal_even_though_it_shares_a_kind_with_an_end_code() {
    let sim = Simulator::start().await;
    sim.set_word(SlmpDevice::D, 0, 1);
    sim.emit_malformed_frames();

    // Structured check on the wrapped crate's own error type first.
    let mut raw = slmp::SLMPClient::new(fast_config(&sim).to_wire_props());
    raw.set_send_timeout(Duration::from_millis(100));
    raw.set_recv_timeout(Duration::from_millis(100));
    raw.connect().await.expect("raw connect");
    let raw_err = raw
        .bulk_read(
            slmp::Device {
                device_type: SlmpDevice::D.to_wire(),
                address: 0,
            },
            1,
            slmp::DataType::U16,
        )
        .await
        .expect_err("a length-inconsistent frame must surface as an Err");
    assert!(
        matches!(raw_err, slmp::SlmpError::Framing(_)),
        "expected SlmpError::Framing(_), got {raw_err:?}"
    );
    raw.close().await;

    let mut client = SlmpClient::new(fast_config(&sim));
    client.connect().await.unwrap();

    let err = client
        .read_batch(&[req("D0", DataType::U16)])
        .await
        .expect_err("a length-inconsistent frame must fail the whole call");
    assert!(matches!(err, PlcError::Protocol(_)), "got {err:?}");
    assert!(err.is_connection_fatal());

    // Session torn down: the next call must fail immediately rather than keep
    // using a stream whose alignment is now unknown.
    assert!(matches!(
        client.read_batch(&[req("D0", DataType::U16)]).await,
        Err(PlcError::NotConnected)
    ));
}

/// A device/data-type mismatch is resolved before any wire traffic and does
/// not stop the rest of the batch from reaching the CPU (`client.rs`'s third
/// invariant).
#[tokio::test]
async fn unsupported_combination_is_bad_without_touching_the_wire() {
    let sim = Simulator::start().await;
    sim.set_word(SlmpDevice::D, 1, 9);

    let mut client = SlmpClient::new(fast_config(&sim));
    client.connect().await.unwrap();

    let results = client
        .read_batch(&[
            req("D0", DataType::Bit), // bit tag at a word device
            req("D1", DataType::I16),
        ])
        .await
        .unwrap();
    assert!(matches!(
        results[0],
        ReadResult::Bad(PlcError::UnsupportedCombination { .. })
    ));
    assert_eq!(results[1], ReadResult::Value(TagValue::F64(9.0)));
}

/// The I2a-specific configuration mistake: a Modbus address on an SLMP
/// connection. Must be one `Bad` entry, not a dead batch.
#[tokio::test]
async fn a_modbus_address_is_bad_without_touching_the_wire() {
    let sim = Simulator::start().await;
    sim.set_word(SlmpDevice::D, 0, 3);

    let mut client = SlmpClient::new(fast_config(&sim));
    client.connect().await.unwrap();

    let results = client
        .read_batch(&[
            ReadRequest {
                address: Address::parse("40001").unwrap(),
                data_type: DataType::U16,
            },
            req("D0", DataType::U16),
        ])
        .await
        .unwrap();
    assert!(matches!(
        results[0],
        ReadResult::Bad(PlcError::AddressProtocolMismatch { .. })
    ));
    assert_eq!(results[1], ReadResult::Value(TagValue::F64(3.0)));
}

#[tokio::test]
async fn response_timeout_fails_the_call_and_tears_down_the_connection() {
    let sim = Simulator::start().await;
    sim.hang();

    let mut client = SlmpClient::new(fast_config(&sim));
    client.connect().await.unwrap();

    let err = client
        .read_batch(&[req("D0", DataType::U16)])
        .await
        .expect_err("hung simulator should time out the whole call");
    assert!(matches!(err, PlcError::ResponseTimeout), "got {err:?}");

    // The connection is now considered dead - further calls must not hang
    // again on the same broken session, they must fail immediately.
    assert!(matches!(
        client.read_batch(&[req("D0", DataType::U16)]).await,
        Err(PlcError::NotConnected)
    ));
}

/// A severed session must be fatal and must tear the client down. Note the
/// variant differs from the Modbus equivalent (`Connection`): the wrapped
/// crate reads a response with a single `read` rather than `read_exact`, so a
/// closed socket surfaces as a zero-byte read, which its frame validation
/// reports as invalid data rather than as an I/O error. Both are fatal, which
/// is the property that matters, so this asserts on that rather than on the
/// variant.
#[tokio::test]
async fn disconnect_mid_session_fails_the_call_and_tears_down_the_connection() {
    let sim = Simulator::start().await;
    sim.set_word(SlmpDevice::D, 0, 1);

    let mut client = SlmpClient::new(fast_config(&sim));
    client.connect().await.unwrap();

    // Prove the connection works before severing it.
    client
        .read_batch(&[req("D0", DataType::U16)])
        .await
        .expect("first read should succeed");

    sim.stop();

    let err = client
        .read_batch(&[req("D0", DataType::U16)])
        .await
        .expect_err("severed connection should fail the call");
    assert!(
        err.is_connection_fatal(),
        "a severed session must be fatal, got {err:?}"
    );

    assert!(matches!(
        client.read_batch(&[req("D0", DataType::U16)]).await,
        Err(PlcError::NotConnected)
    ));
}

#[tokio::test]
async fn read_batch_before_connect_is_not_connected() {
    let sim = Simulator::start().await;
    let mut client = SlmpClient::new(fast_config(&sim));
    assert!(matches!(
        client.read_batch(&[req("D0", DataType::U16)]).await,
        Err(PlcError::NotConnected)
    ));
}

#[tokio::test]
async fn connect_to_a_closed_port_fails_without_hanging() {
    // Bind and immediately stop accepting, so the port is (very likely) closed.
    let sim = Simulator::start().await;
    let config = fast_config(&sim);
    sim.stop();
    // Give the aborted listener a moment to actually release the port.
    tokio::task::yield_now().await;

    let mut client = SlmpClient::new(SlmpConfig {
        connect_timeout: Duration::from_millis(300),
        ..config
    });
    let err = client
        .connect()
        .await
        .expect_err("connecting to a dead listener should fail");
    assert!(
        matches!(err, PlcError::Connection(_) | PlcError::ConnectTimeout(_)),
        "got {err:?}"
    );
    assert!(matches!(
        client.read_batch(&[req("D0", DataType::U16)]).await,
        Err(PlcError::NotConnected)
    ));
}

#[tokio::test]
async fn disconnect_then_read_is_not_connected() {
    let sim = Simulator::start().await;
    sim.set_word(SlmpDevice::D, 0, 5);

    let mut client = SlmpClient::new(fast_config(&sim));
    client.connect().await.unwrap();
    client
        .read_batch(&[req("D0", DataType::U16)])
        .await
        .unwrap();

    client.disconnect().await;
    assert!(matches!(
        client.read_batch(&[req("D0", DataType::U16)]).await,
        Err(PlcError::NotConnected)
    ));

    // ...and reconnecting on the same instance works.
    client.connect().await.expect("reconnect should succeed");
    let results = client
        .read_batch(&[req("D0", DataType::U16)])
        .await
        .unwrap();
    assert_eq!(results[0], ReadResult::Value(TagValue::F64(5.0)));
}

#[tokio::test]
async fn disconnect_then_reconnect_after_a_fatal_error_works() {
    let sim1 = Simulator::start().await;
    let mut client = SlmpClient::new(fast_config(&sim1));
    client.connect().await.unwrap();
    sim1.stop();
    let err = client
        .read_batch(&[req("D0", DataType::U16)])
        .await
        .unwrap_err();
    assert!(err.is_connection_fatal());

    // I3 owns the actual reconnect loop (docs/plan.md I2 §2: "再接続ループは
    // 持たない") - this only proves the *client* half of that contract: after a
    // connection-fatal failure it is not permanently wedged, a fresh
    // `connect()` (here, to a new simulator standing in for "the CPU came
    // back") followed by `read_batch` works like a brand new client would.
    let sim2 = Simulator::start().await;
    sim2.set_word(SlmpDevice::D, 0, 77);
    let mut client2 = SlmpClient::new(fast_config(&sim2));
    client2
        .connect()
        .await
        .expect("reconnect to the new simulator");
    let results = client2
        .read_batch(&[req("D0", DataType::U16)])
        .await
        .unwrap();
    assert_eq!(results[0], ReadResult::Value(TagValue::F64(77.0)));
}

/// A batch of exclusively unservable requests must still return one `Bad` per
/// request rather than an `Err` - `read_batch`'s result vector is always
/// `requests.len()` long, in order (`client.rs`'s contract).
#[tokio::test]
async fn a_batch_of_only_bad_requests_still_returns_one_result_each() {
    let sim = Simulator::start().await;
    let mut client = SlmpClient::new(fast_config(&sim));
    client.connect().await.unwrap();

    let results = client
        .read_batch(&[req("D0", DataType::Bit), req("M0", DataType::U16)])
        .await
        .expect("no wire traffic needed, so no way for this to fail");
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| matches!(r, ReadResult::Bad(_))));
}

#[tokio::test]
async fn an_empty_batch_is_ok_and_empty() {
    let sim = Simulator::start().await;
    let mut client = SlmpClient::new(fast_config(&sim));
    client.connect().await.unwrap();
    assert!(client.read_batch(&[]).await.unwrap().is_empty());
}

/// The broker-sharing seam (W3): [`super::execute_slmp_reads`] runs against a
/// **borrowed** `slmp::SLMPClient` the caller owns, so the W3 broker can drive
/// reads over the *same* session it also writes on (via
/// `banto_plc_write::execute_slmp_writes`). Here we stand up a bare
/// `slmp::SLMPClient` - exactly what the broker owns per CPU - plan, and execute,
/// with no `SlmpClient` wrapper involved. The read twin of
/// `banto-plc-write`'s `execute_slmp_writes_runs_on_a_borrowed_client_for_the_broker`.
#[tokio::test]
async fn execute_slmp_reads_runs_on_a_borrowed_client_for_the_broker() {
    let sim = Simulator::start().await;
    sim.set_word(SlmpDevice::D, 0, 11);
    sim.set_word(SlmpDevice::D, 1, 22);
    sim.set_bit(SlmpDevice::M, 0, true);

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
        req("D0", DataType::U16),
        req("D1", DataType::U16),
        req("M0", DataType::Bit),
    ];
    let outcome = super::planning::plan_slmp_requests(&requests);
    let results =
        super::execute_slmp_reads(&mut shared, &outcome, requests.len(), WordOrder::LowHigh)
            .await
            .expect("execute on borrowed client");
    assert_eq!(results[0], ReadResult::Value(TagValue::F64(11.0)));
    assert_eq!(results[1], ReadResult::Value(TagValue::F64(22.0)));
    assert_eq!(results[2], ReadResult::Value(TagValue::Bit(true)));

    shared.close().await;
}

/// Performance smoke test, the SLMP twin of `modbus/integration_tests.rs`'s
/// (docs/plan.md I2 §7): 256 tags across consecutive `D` registers, realistic
/// for a single collection group near the v1 tag-count target
/// (recorder-requirements.md §3.1), which `slmp/planning.rs`'s word cap fits
/// into a single round trip. Not a CI gate (no timing assertion, per I2 §7:
/// "CI失敗条件にはしない") - loopback-to-a-Tokio-task latency says nothing about
/// a real MELSEC network. The printed number exists to be compared against the
/// Modbus one, since the two now differ in a way worth watching: SLMP gets one
/// round trip where Modbus needs three, but pays for it by going through the
/// wrapped crate's per-request `Vec<DeviceData>` allocation.
#[tokio::test]
async fn performance_smoke_256_tags_x_1000_read_batch_calls() {
    const TAG_COUNT: u32 = 256;
    const ITERATIONS: u32 = 1000;

    let sim = Simulator::start().await;
    let values: Vec<u16> = (0..TAG_COUNT as u16).collect();
    sim.set_words(SlmpDevice::D, 0, &values);

    let mut client = SlmpClient::new(fast_config(&sim));
    client.connect().await.unwrap();

    let requests: Vec<ReadRequest> = (0..TAG_COUNT)
        .map(|i| req(&format!("D{i}"), DataType::U16))
        .collect();

    // One warm-up call outside the timed loop (first-connection TCP/Tokio
    // scheduling overhead is not representative of steady-state polling).
    let warm = client.read_batch(&requests).await.unwrap();
    assert_eq!(warm.len(), TAG_COUNT as usize);
    assert_eq!(warm[255], ReadResult::Value(TagValue::F64(255.0)));

    let started = std::time::Instant::now();
    for _ in 0..ITERATIONS {
        let results = client.read_batch(&requests).await.unwrap();
        assert_eq!(results.len(), TAG_COUNT as usize);
    }
    let elapsed = started.elapsed();
    let per_call = elapsed / ITERATIONS;

    println!(
        "[banto-plc perf smoke, slmp] {TAG_COUNT} tags x {ITERATIONS} read_batch calls (loopback \
         simulator, 1 round trip/call) in {elapsed:?} total, {per_call:?}/call average \
         (100ms/cycle target, recorder-requirements.md §3.1)"
    );
}

// --- string reads (S1 文字列タグ) -------------------------------------------

/// ASCII, multi-byte SJIS, and a full-to-the-brim span, seeded via the
/// simulator's `set_string` and read back through the real wire path in one
/// mixed batch alongside numeric and bit tags - proving one `read_batch_mixed`
/// call serves all three kinds in a single planning pass.
#[tokio::test]
async fn read_batch_mixed_reads_strings_and_numerics_in_one_call() {
    use crate::types::{BatchReadRequest, BatchReadResult, PlcValue, StringReadRequest};

    let sim = Simulator::start().await;
    sim.set_string(SlmpDevice::D, 0, 4, "ABC"); // padded with NULs
    sim.set_string(SlmpDevice::D, 100, 4, "テスト"); // 6 SJIS bytes in 8
    sim.set_string(SlmpDevice::D, 200, 2, "ABCD"); // exactly 2L bytes, no NUL
    sim.set_word(SlmpDevice::D, 4, 42);
    sim.set_bit(SlmpDevice::M, 0, true);

    let mut client = SlmpClient::new(fast_config(&sim));
    client.connect().await.expect("connect");

    let sreq = |raw: &str, words: u16| {
        BatchReadRequest::String(StringReadRequest {
            address: Address::parse_slmp(raw).unwrap(),
            words,
        })
    };
    let requests = [
        sreq("D0", 4),
        BatchReadRequest::Numeric(req("D4", DataType::U16)), // adjacent: same group as D0..D3
        sreq("D100", 4),
        sreq("D200", 2),
        BatchReadRequest::Numeric(req("M0", DataType::Bit)),
    ];
    let results = client
        .read_batch_mixed(&requests)
        .await
        .expect("read_batch_mixed ok");

    assert_eq!(results.len(), requests.len());
    assert_eq!(
        results[0],
        BatchReadResult::Value(PlcValue::Str("ABC".to_string())),
        "trailing NUL padding must be trimmed"
    );
    assert_eq!(results[1], BatchReadResult::Value(PlcValue::F64(42.0)));
    assert_eq!(
        results[2],
        BatchReadResult::Value(PlcValue::Str("テスト".to_string()))
    );
    assert_eq!(
        results[3],
        BatchReadResult::Value(PlcValue::Str("ABCD".to_string())),
        "a full span with no terminator is the whole 2L bytes"
    );
    assert_eq!(results[4], BatchReadResult::Value(PlcValue::Bit(true)));
}

/// An embedded NUL cuts the string there, even mid-span.
#[tokio::test]
async fn read_batch_mixed_trims_at_an_embedded_nul() {
    use crate::types::{BatchReadRequest, BatchReadResult, PlcValue, StringReadRequest};

    let sim = Simulator::start().await;
    // Bytes [0x41, 0x42, 0x00, 0x43] = "AB", NUL, "C" - seeded as raw words
    // (set_string cannot express an embedded NUL, which is the point).
    sim.set_words(SlmpDevice::D, 0, &[0x4241, 0x4300]);

    let mut client = SlmpClient::new(fast_config(&sim));
    client.connect().await.unwrap();

    let results = client
        .read_batch_mixed(&[BatchReadRequest::String(StringReadRequest {
            address: Address::parse_slmp("D0").unwrap(),
            words: 2,
        })])
        .await
        .unwrap();
    assert_eq!(
        results[0],
        BatchReadResult::Value(PlcValue::Str("AB".to_string()))
    );
}

/// Two wide strings whose combined span exceeds the single-read word cap are
/// split into two wire reads by the planner and both still decode correctly -
/// the string case of "a read spanning the batching boundary", proven over the
/// real wire path rather than only at plan level.
#[tokio::test]
async fn string_reads_spanning_the_batching_boundary_split_and_still_decode() {
    use crate::types::{BatchReadRequest, BatchReadResult, PlcValue, StringReadRequest};

    let sim = Simulator::start().await;
    // 300 + 300 words > 480 (the single-bulk-read cap), so the planner must
    // split. Each string's text sits at the start of its span, NUL-padded.
    sim.set_string(SlmpDevice::D, 0, 300, "FIRST");
    sim.set_string(SlmpDevice::D, 300, 300, "SECOND");

    let mut client = SlmpClient::new(fast_config(&sim));
    client.connect().await.unwrap();

    let sreq = |raw: &str, words: u16| {
        BatchReadRequest::String(StringReadRequest {
            address: Address::parse_slmp(raw).unwrap(),
            words,
        })
    };
    let results = client
        .read_batch_mixed(&[sreq("D0", 300), sreq("D300", 300)])
        .await
        .unwrap();
    assert_eq!(
        results[0],
        BatchReadResult::Value(PlcValue::Str("FIRST".to_string()))
    );
    assert_eq!(
        results[1],
        BatchReadResult::Value(PlcValue::Str("SECOND".to_string()))
    );
}

/// A string span the wire cannot serve is a per-request Bad through the full
/// client path too - its batch-mates still read.
#[tokio::test]
async fn an_over_cap_string_is_bad_through_the_client_without_blocking_batch_mates() {
    use crate::types::{BatchReadRequest, BatchReadResult, PlcValue, StringReadRequest};

    let sim = Simulator::start().await;
    sim.set_word(SlmpDevice::D, 0, 7);

    let mut client = SlmpClient::new(fast_config(&sim));
    client.connect().await.unwrap();

    let results = client
        .read_batch_mixed(&[
            BatchReadRequest::String(StringReadRequest {
                address: Address::parse_slmp("D1000").unwrap(),
                words: 481,
            }),
            BatchReadRequest::Numeric(req("D0", DataType::U16)),
        ])
        .await
        .unwrap();
    assert!(matches!(
        results[0],
        BatchReadResult::Bad(PlcError::StringSpanUnsupported { words: 481, .. })
    ));
    assert_eq!(results[1], BatchReadResult::Value(PlcValue::F64(7.0)));
}

#[tokio::test]
async fn read_batch_mixed_before_connect_is_not_connected() {
    use crate::types::{BatchReadRequest, StringReadRequest};

    let mut client = SlmpClient::new(SlmpConfig {
        host: "127.0.0.1".to_string(),
        ..Default::default()
    });
    let requests = [BatchReadRequest::String(StringReadRequest {
        address: Address::parse_slmp("D0").unwrap(),
        words: 4,
    })];
    assert!(matches!(
        client.read_batch_mixed(&requests).await,
        Err(PlcError::NotConnected)
    ));
}
