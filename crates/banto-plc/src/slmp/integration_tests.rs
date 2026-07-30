//! End-to-end tests for [`super::SlmpClient`] against
//! [`super::simulator::Simulator`] - the scenarios a hand-built byte array
//! cannot exercise: real TCP framing through the wrapped `slmp` crate, real
//! timeouts, a real dropped connection. `address.rs`/`planning.rs` already
//! cover the pure logic in isolation, and `mod.rs`'s unit tests cover the error
//! classifier over hand-written strings; this file is about the parts that only
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
/// `Bad` and leave its batch-mates alone. If a future `slmp` release changes
/// how it words that error, this is what fails.
#[tokio::test]
async fn slmp_end_code_is_bad_not_fatal() {
    let sim = Simulator::start().await;
    sim.set_word(SlmpDevice::D, 100, 55);
    // D0 and D100 are far enough apart (gap > MAX_GAP) that planning puts them
    // in separate groups, so injecting on the D0 group must leave D100 intact.
    sim.inject_end_code(SlmpDevice::D, 0, 0xC059);

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
        other => panic!(
            "expected Bad(SlmpEndCode) - if the `slmp` crate changed its error \
             message format, `super::parse_end_code` needs updating. Got {other:?}"
        ),
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
/// [`super::classify_io_error`] with the *same* `ErrorKind::InvalidData` as an
/// end code and must still come out fatal. Together with the test above, this
/// is what proves the message-text match is doing real work.
#[tokio::test]
async fn a_malformed_frame_is_fatal_even_though_it_shares_a_kind_with_an_end_code() {
    let sim = Simulator::start().await;
    sim.set_word(SlmpDevice::D, 0, 1);
    sim.emit_malformed_frames();

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
