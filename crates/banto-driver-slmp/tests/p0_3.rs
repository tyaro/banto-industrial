//! P0-3 (docs/banto-hub-driver-refactor-plan.md §8.3), rows 3 and 6 of
//! docs/banto-hub-driver-io-design.md §11.1, and the other behaviours of the
//! removed `SlmpClient` / `SlmpWriteClient` tests (§11.1 note), on the new I/O:
//! the real `banto-driver-engine` (`FrameSession`) over a real TCP socket to
//! `banto-plc`'s SLMP simulator, driving the frame-type `SlmpDriver`
//! (R2b-3d-2, R2b-3d-join).
//!
//! Reads (was `banto-plc/src/slmp/integration_tests.rs`, 32 tests):
//!
//! | here | was |
//! | --- | --- |
//! | `a_response_split_across_segments_is_read_as_one_response` | the same name (#6) |
//! | `connect_to_a_closed_port_fails_without_hanging` | the same name (#3) |
//! | `normal_batch_reads_every_data_type_correctly` | the same name (also `frame_conformance.rs` C3) |
//! | `bit_in_word_tag_shares_the_ordinary_word_read_and_extracts_its_bit` | the same name |
//! | `bit_in_word_tag_decodes_every_bit_position` | the same name |
//! | `default_low_high_word_order_matches_melsec_storage` | the same name |
//! | `mixed_devices_and_types_in_one_batch_each_get_their_own_group` | the same name |
//! | `a_long_odd_length_bit_run_decodes_in_the_right_order` | the same name |
//! | `hexadecimal_device_numbers_reach_the_wire_as_their_numeric_value` | the same name |
//! | `both_cpu_series_frame_layouts_work` | the same name (Q and R; `SlmpCpu::L` is not a `cpu_type` setting and uses Q's frames) |
//! | `request_frames_for_q_and_r_match_the_hand_written_wire_bytes` | the same name (#605; the capture server now sees the engine's bytes) |
//! | `slmp_end_code_is_bad_not_fatal` | the same name (the `slmp` crate half stays; the driver half is `ItemError::Device`) |
//! | `a_malformed_frame_is_fatal_even_though_it_shares_a_kind_with_an_end_code` | the same name (`FatalError::Framing`) |
//! | `unsupported_combination_is_unsupported_and_the_batch_mate_is_still_read` | `unsupported_combination_is_bad_without_touching_the_wire` |
//! | `a_modbus_address_is_unsupported_and_the_batch_mate_is_still_read` | `a_modbus_address_is_bad_without_touching_the_wire` |
//! | `response_timeout_fails_the_call_and_tears_down_the_connection` | the same name |
//! | `disconnect_mid_session_fails_the_call_and_tears_down_the_connection` | the same name |
//! | `a_dead_connection_refuses_every_call` | `read_batch_before_connect_is_not_connected`, `read_batch_mixed_before_connect_is_not_connected` (the engine has no unconnected state: `connect` returns the session, a dead one answers `Protocol`) |
//! | `disconnect_then_read_is_refused_then_a_new_connection_works` | `disconnect_then_read_is_not_connected` |
//! | `disconnect_then_reconnect_after_a_fatal_error_works` | the same name |
//! | `a_batch_of_only_bad_requests_still_returns_one_result_each` | the same name |
//! | `an_empty_batch_is_ok_and_empty` | the same name |
//! | `performance_smoke_256_tags_x_1000_read_batch_calls` | the same name |
//! | `a_mixed_batch_reads_strings_and_numerics_in_one_call` | `read_batch_mixed_reads_strings_and_numerics_in_one_call` |
//! | `a_string_read_trims_at_an_embedded_nul` | `read_batch_mixed_trims_at_an_embedded_nul` |
//! | `string_reads_spanning_the_batching_boundary_split_and_still_decode` | the same name |
//! | `an_over_cap_string_is_unsupported_without_blocking_batch_mates` | `an_over_cap_string_is_bad_through_the_client_without_blocking_batch_mates` |
//! | `a_string_read_decodes_utf8_when_requested` | `read_batch_mixed_decodes_utf8_when_requested` |
//! | `the_same_utf8_wire_bytes_do_not_decode_correctly_as_shift_jis` | the same name |
//! | `a_mixed_batch_decodes_each_string_request_independently` | the same name |
//! | (`tests/broker.rs`: `write_then_read_reflects_the_write_over_the_single_session`) | `execute_slmp_reads_runs_on_a_borrowed_client_for_the_broker` (the borrowed-client seam is gone: the broker's one connection per PLC is the engine's, which `tests/broker.rs` pins through the registry) |
//!
//! Writes (was `banto-plc-write/src/slmp/integration_tests.rs`, 33 tests; 30 ported, 3 mapped):
//!
//! | here | was |
//! | --- | --- |
//! | `write_batch_writes_every_data_type_and_reads_back_through_a_second_session` | `write_batch_writes_every_data_type_and_reads_back_through_the_read_client` (the read client is a second engine session) |
//! | `default_low_high_word_order_round_trips_and_lands_low_word_first` | the same name |
//! | `a_long_odd_length_bit_run_writes_in_the_right_order` | the same name |
//! | `hexadecimal_device_numbers_write_at_their_numeric_value` | the same name |
//! | `both_cpu_series_write_frame_layouts_work` | the same name (Q and R) |
//! | `slmp_write_end_code_is_bad_not_fatal` | the same name (`ItemError::Device`) |
//! | `a_malformed_frame_on_a_write_is_fatal_and_tears_down_the_connection` | `a_malformed_frame_is_fatal_and_tears_down_the_connection` |
//! | `unsupported_combination_write_is_unsupported_and_the_batch_mate_is_still_written` | `unsupported_combination_is_bad_without_touching_the_wire` |
//! | `a_modbus_address_write_is_unsupported_and_the_batch_mate_is_still_written` | `a_modbus_address_is_bad_without_touching_the_wire` |
//! | `an_out_of_range_value_is_unsupported_without_touching_the_wire` | `an_out_of_range_value_is_bad_without_touching_the_wire` |
//! | `write_response_timeout_fails_the_call_and_tears_down_the_connection` | `response_timeout_fails_the_call_and_tears_down_the_connection` |
//! | `write_disconnect_mid_session_fails_the_call_and_tears_down_the_connection` | `disconnect_mid_session_fails_the_call_and_tears_down_the_connection` |
//! | `a_dead_connection_refuses_every_call` (reads section) | `write_batch_before_connect_is_not_connected`, `write_batch_mixed_before_connect_is_not_connected` (mapped: the engine has no unconnected state) |
//! | `disconnect_then_write_is_refused_then_a_new_connection_works` | `disconnect_then_write_is_not_connected_then_reconnect_works` |
//! | `write_reconnect_after_a_fatal_error_works` | `reconnect_after_a_fatal_error_works` |
//! | `a_batch_of_only_bad_writes_still_returns_one_result_each` | `a_batch_of_only_bad_requests_still_returns_one_result_each` |
//! | `an_empty_write_batch_is_ok_and_empty` | `an_empty_batch_is_ok_and_empty` |
//! | (`tests/broker.rs`: `write_then_read_reflects_the_write_over_the_single_session`) | `execute_slmp_writes_runs_on_a_borrowed_client_for_the_broker` (mapped: the borrowed-client seam is gone) |
//! | `string_write_then_read_back_round_trips_ascii_sjis_and_full_spans` | the same name |
//! | `string_write_lands_low_byte_first_with_nul_padding_on_the_device` | the same name |
//! | `utf8_string_write_lands_the_correct_bytes_on_the_wire` | the same name |
//! | `a_mixed_batch_encodes_each_string_write_independently` | `a_mixed_batch_encodes_each_string_request_independently` |
//! | `an_overlong_string_writes_nothing_and_spares_its_batch_mates` | the same name (`ItemError::Unsupported`) |
//! | `mixed_numeric_and_string_batch_round_trips_in_single_calls` | the same name |
//! | `single_bit_set_via_rmw_leaves_other_bits_unchanged` | the same name |
//! | `single_bit_clear_via_rmw_leaves_other_bits_unchanged` | the same name |
//! | `all_sixteen_bit_positions_set_and_clear_correctly` | the same name |
//! | `mask_composed_bits_of_one_word_cost_exactly_one_wire_write` | the same name (on the write crate's simulator, which counts reads) |
//! | `confirmation_mismatch_is_rejected_with_a_race_hint` | `confirmation_mismatch_is_reported_as_bad_with_a_race_hint` (`ItemError::VerificationFailed`; also `conformance.rs` C14) |
//! | `confirmation_mismatch_on_one_bit_does_not_fail_its_batch_mates` | the same name |
//! | `bit_writes_to_different_words_are_independent` | the same name |
//! | `ordinary_write_and_bit_in_word_write_coexist_in_one_batch` | the same name |
//!
//! The session-type unit tests deleted with `api_session.rs` / `session.rs`
//! (R2b-3d-join) and where each property is pinned now:
//!
//! | was (`api_session.rs`) | now |
//! | --- | --- |
//! | `a_connect_timeout_is_a_timeout_and_a_refusal_is_io` | the engine's dial; here `connect_to_a_closed_port_fails_without_hanging` (`Io`, or `Timeout` on Windows where a loopback refusal outlasts the connect timeout). Timeout-vs-refusal is NOT pinned deterministically: a zero-timeout connect to a loopback listener completes before the deadline is checked |
//! | `bit_in_word_is_chosen_exactly_when_the_hub_would` | `src/request.rs` (unit test, same name) |
//! | `a_spec_running_past_the_last_device_number_is_an_item_error` | `src/request.rs` (unit test, same name); the frame side is `frame_conn.rs`'s `a_spec_running_past_the_last_device_number_is_left_out_alone` |
//! | `the_session_never_sends_a_request_past_the_last_device_number` | `the_engine_never_sends_a_request_past_the_last_device_number` below |
//! | `a_write_ending_exactly_at_the_last_device_number_is_sent` | `a_write_ending_exactly_at_the_last_device_number_is_sent` below |
//! | `a_write_mixing_healthy_and_overrunning_tags_writes_only_the_healthy` | `a_write_mixing_healthy_and_overrunning_tags_writes_only_the_healthy` below |
//! | `the_entry_says_words_or_points_like_the_save_path` | `src/request.rs` (unit test, same name) |
//! | `a_result_count_mismatch_is_fatal` | the engine owns the stitching of results to specs now (`banto-driver-engine`); `frame_conn.rs`'s `a_spec_outside_its_step_is_a_broken_plan` pins the driver's half (a plan that does not fit is `Protocol`) |
//! | `fatal_errors_keep_their_type` | `fatal_errors_keep_their_type` below (timeout is `Timeout`, a closed socket `Io`; also `response_timeout_fails_...`) |
//! | `config_comes_from_the_settings_and_the_context` | `frame_conn.rs`'s `create_connection_reads_the_series_and_the_word_order` (cpu_type / word_order and their defaults); host and port are the engine's `TransportSpec` and `banto-broker`'s settings validation, not the driver's |
//!
//! `session.rs` held no test of its own.

use std::time::{Duration, Instant};

use banto_driver_api::{
    ConnectContext, DataType, DriverAddress, DriverBase, FatalError, ItemError, Session, Settings,
    StringEncoding, TagKind, TagSpec, Value, WriteItem,
};
use banto_driver_engine::transport::TransportSpec;
use banto_driver_engine::FrameSession;
use banto_driver_slmp::SlmpDriver;
use banto_plc::slmp::address::SlmpDevice;
use banto_plc::slmp::simulator::Simulator;

/// A short response timeout so failure-path tests (timeout, disconnect) do not
/// slow the suite down.
const FAST: Duration = Duration::from_millis(100);

fn n(data_type: DataType) -> TagKind {
    TagKind::numeric(data_type)
}

fn spec(text: &str, kind: TagKind) -> TagSpec {
    TagSpec::new(SlmpDriver.parse_address(text, &kind).unwrap(), kind)
}

/// A spec that skipped the save path's `parse_address` (which refuses a bit
/// tag at a word device, a span past the last device number, an over-long
/// string...).
fn unchecked(canonical: &str, kind: TagKind) -> TagSpec {
    let words = kind.word_count();
    TagSpec::new(
        DriverAddress::new(
            SlmpDriver.descriptor().id.clone(),
            canonical,
            "D",
            true,
            words,
        ),
        kind,
    )
}

fn settings(host: &str, port: u16, cpu: &str) -> Settings {
    Settings::from_pairs([
        ("host", host.to_string()),
        ("port", port.to_string()),
        ("cpu_type", cpu.to_string()),
    ])
    .unwrap()
}

/// A session to `sim` with the `extra` settings (`cpu_type`, `word_order`)
/// and the given response timeout.
async fn connect_with(
    sim: &Simulator,
    extra: &[(&str, &str)],
    response: Duration,
) -> Result<FrameSession, FatalError> {
    let host = sim.addr.ip().to_string();
    let transport = TransportSpec::Tcp {
        host: host.clone(),
        port: sim.addr.port(),
    };
    let mut pairs = vec![
        ("host".to_string(), host),
        ("port".to_string(), sim.addr.port().to_string()),
    ];
    pairs.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    let settings = Settings::from_pairs(pairs).unwrap();
    let cx = ConnectContext::new("p0-3", Duration::from_secs(2), response);
    FrameSession::connect(&SlmpDriver, &settings, &transport, &cx, None).await
}

async fn connect_fast(sim: &Simulator) -> Result<FrameSession, FatalError> {
    connect_with(sim, &[], FAST).await
}

async fn connect(sim: &Simulator, cpu: &str) -> Result<FrameSession, FatalError> {
    connect_with(sim, &[("cpu_type", cpu)], Duration::from_secs(5)).await
}

/// P0-3 #6: a response that reaches the client in several TCP segments - one
/// byte at a time, inside the 15-byte header (5) and right after the 13
/// bytes the length field does not count (13) - is read as one response with
/// the right values, twice (the second read proves the stream is still
/// aligned), on both series.
#[tokio::test]
async fn a_response_split_across_segments_is_read_as_one_response() {
    for cpu in ["R", "Q"] {
        let sim = Simulator::start().await;
        sim.set_word(SlmpDevice::D, 0, 42);
        sim.set_word(SlmpDevice::D, 1, 0xFFFF);
        // D2 as U32: the default word order is low_high, so the low word first.
        sim.set_words(SlmpDevice::D, 2, &[0x0002, 0x0001]);
        sim.set_word(SlmpDevice::D, 10, 0x0008);
        sim.set_string(SlmpDevice::D, 20, 2, "AB");
        sim.set_bit(SlmpDevice::M, 0, true);

        let mut session = connect(&sim, cpu).await.unwrap();
        let sjis = TagKind::string(2, StringEncoding::ShiftJis);
        let specs = [
            spec("D0", n(DataType::U16)),
            spec("D1", n(DataType::I16)),
            spec("D2", n(DataType::U32)),
            spec("D10.3", n(DataType::Bit)),
            spec("D20", sjis),
            spec("M0", n(DataType::Bit)),
            spec("M1", n(DataType::Bit)),
        ];
        for chunk in [1usize, 5, 13] {
            sim.set_response_chunking(Some((chunk, Duration::from_millis(2))));
            for _ in 0..2 {
                let values = session
                    .read_batch(&specs)
                    .await
                    .unwrap_or_else(|e| panic!("{cpu} chunk={chunk}: read_batch failed: {e:?}"));
                assert_eq!(
                    values,
                    vec![
                        Ok(Value::F64(42.0)),
                        Ok(Value::F64(-1.0)),
                        Ok(Value::F64(f64::from(0x0001_0002_u32))),
                        Ok(Value::Bit(true)),
                        Ok(Value::Str("AB".into())),
                        Ok(Value::Bit(true)),
                        Ok(Value::Bit(false)),
                    ],
                    "{cpu} chunk={chunk}"
                );
            }
        }
        session.disconnect().await;
        sim.stop();
    }
}

/// P0-3 #6 for the write path: an ordinary write's answer (no data) and the
/// three steps of a read-modify-write, split the same ways, land the
/// values, keep the other bits of the word, and leave the stream aligned for
/// the next read.
#[tokio::test]
async fn a_split_write_and_read_modify_write_land_and_stay_aligned() {
    let sim = Simulator::start().await;
    sim.set_word(SlmpDevice::D, 100, 0x8001);
    let mut session = connect(&sim, "R").await.unwrap();
    let bit = n(DataType::Bit);
    for (round, chunk) in [1usize, 5, 13].into_iter().enumerate() {
        sim.set_response_chunking(Some((chunk, Duration::from_millis(2))));
        let value = 1000.0 + round as f64;
        let on = round % 2 == 0;
        let items = [
            WriteItem::new(spec("D200", n(DataType::U16)), Value::F64(value)),
            WriteItem::new(spec("D100.4", bit.clone()), Value::Bit(on)),
        ];
        let results = session.write_batch(&items).await.unwrap();
        assert_eq!(results, vec![Ok(()), Ok(())], "chunk={chunk}");
        let expected = if on { 0x8011 } else { 0x8001 };
        assert_eq!(sim.get_word(SlmpDevice::D, 100), expected, "chunk={chunk}");
        let values = session
            .read_batch(&[
                spec("D200", n(DataType::U16)),
                spec("D100", n(DataType::U16)),
            ])
            .await
            .unwrap();
        assert_eq!(
            values,
            vec![Ok(Value::F64(value)), Ok(Value::F64(f64::from(expected)))],
            "chunk={chunk}"
        );
    }
    session.disconnect().await;
    sim.stop();
}

/// P0-3 #3 (SLMP): connecting to a port nobody listens on fails (refused or
/// timed out) within the connect timeout, without hanging. The closed port is
/// made by binding and dropping a plain listener.
#[tokio::test]
async fn connect_to_a_closed_port_fails_without_hanging() {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let spec = TransportSpec::Tcp {
        host: "127.0.0.1".into(),
        port,
    };
    let connect_timeout = Duration::from_secs(2);
    let cx = ConnectContext::new("p0-3", connect_timeout, Duration::from_secs(1));
    let started = Instant::now();
    let result = FrameSession::connect(
        &SlmpDriver,
        &settings("127.0.0.1", port, "R"),
        &spec,
        &cx,
        None,
    )
    .await;
    // Windows loopback retries a refused SYN for ~2 s, so the connect timeout
    // can win over the refusal (observed: `Timeout`); elsewhere it is an
    // immediate `Io`. Timeout-vs-refusal is not pinned (see the table above).
    assert!(
        matches!(result, Err(FatalError::Io(_) | FatalError::Timeout)),
        "{:?}",
        result.err()
    );
    assert!(
        started.elapsed() < connect_timeout + Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
}

// ---------------------------------------------------------------------------
// Reads (was `banto-plc/src/slmp/integration_tests.rs`)
// ---------------------------------------------------------------------------

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

    let mut session = connect_fast(&sim).await.expect("connect should succeed");
    let specs = [
        spec("D0", n(DataType::U16)),
        spec("D1", n(DataType::I16)),
        spec("D2", n(DataType::U32)),
        spec("R0", n(DataType::U16)),
        spec("M0", n(DataType::Bit)),
        spec("X0", n(DataType::Bit)),
    ];
    let results = session.read_batch(&specs).await.expect("read_batch ok");
    assert_eq!(
        results,
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

/// T8 (docs/tag-server-design.md §6.1) end to end: `D100.5` reads the same
/// word `D100` an ordinary numeric tag would, decoded down to one bit, and a
/// plain numeric read of the same register is unaffected by a bit-in-word tag
/// sharing its group.
#[tokio::test]
async fn bit_in_word_tag_shares_the_ordinary_word_read_and_extracts_its_bit() {
    let sim = Simulator::start().await;
    // 0x1234 = 0b0001_0010_0011_0100 - bit 2, 4, 5, 9, 12 set.
    sim.set_word(SlmpDevice::D, 100, 0x1234);

    let mut session = connect_fast(&sim).await.unwrap();
    let specs = [
        spec("D100.5", n(DataType::Bit)), // set
        spec("D100.0", n(DataType::Bit)), // clear
        spec("D100.F", n(DataType::Bit)), // clear (top bit, hex bit 15 - T20-④)
        spec("D100", n(DataType::U16)),   // plain numeric read of the same word
    ];
    let results = session.read_batch(&specs).await.unwrap();
    assert_eq!(
        results,
        vec![
            Ok(Value::Bit(true)),
            Ok(Value::Bit(false)),
            Ok(Value::Bit(false)),
            Ok(Value::F64(f64::from(0x1234_u16))),
        ]
    );
    session.disconnect().await;
    sim.stop();
}

/// Every bit position 0..=15 decodes correctly, not just a couple of
/// hand-picked ones.
#[tokio::test]
async fn bit_in_word_tag_decodes_every_bit_position() {
    let sim = Simulator::start().await;
    sim.set_word(SlmpDevice::D, 200, 0b1010_1010_1010_1010);

    let mut session = connect_fast(&sim).await.unwrap();
    let specs: Vec<TagSpec> = (0..=15)
        .map(|bit| spec(&format!("D200.{bit:X}"), n(DataType::Bit)))
        .collect();
    let results = session.read_batch(&specs).await.unwrap();
    for (bit, result) in results.iter().enumerate() {
        let expected = bit % 2 == 1; // odd bits set
        assert_eq!(
            *result,
            Ok(Value::Bit(expected)),
            "bit {bit} should decode to {expected}"
        );
    }
    session.disconnect().await;
    sim.stop();
}

/// The default word order is the one thing most likely to be silently wrong
/// against real hardware, so it gets its own end-to-end case in both
/// directions: the same two devices must decode to different numbers.
#[tokio::test]
async fn default_low_high_word_order_matches_melsec_storage() {
    let sim = Simulator::start().await;
    // f32 1.5 = 0x3FC00000. Low word 0x0000 in D0, high word 0x3FC0 in D1.
    sim.set_words(SlmpDevice::D, 0, &[0x0000, 0x3FC0]);

    let mut session = connect_fast(&sim).await.unwrap();
    let results = session
        .read_batch(&[spec("D0", n(DataType::F32))])
        .await
        .unwrap();
    assert_eq!(results, vec![Ok(Value::F64(1.5))]);
    session.disconnect().await;

    // Same bytes, opposite configured order, must *not* decode to 1.5 - proving
    // the setting is actually plumbed through rather than coincidentally right.
    let mut swapped = connect_with(&sim, &[("word_order", "high_low")], FAST)
        .await
        .unwrap();
    let results = swapped
        .read_batch(&[spec("D0", n(DataType::F32))])
        .await
        .unwrap();
    assert!(matches!(results.as_slice(), [Ok(_)]), "{results:?}");
    assert_ne!(results, vec![Ok(Value::F64(1.5))]);
    swapped.disconnect().await;
    sim.stop();
}

#[tokio::test]
async fn mixed_devices_and_types_in_one_batch_each_get_their_own_group() {
    let sim = Simulator::start().await;
    sim.set_word(SlmpDevice::D, 10, 7);
    sim.set_bit(SlmpDevice::M, 5, true);

    let mut session = connect_fast(&sim).await.unwrap();
    let results = session
        .read_batch(&[spec("D10", n(DataType::I16)), spec("M5", n(DataType::Bit))])
        .await
        .unwrap();
    assert_eq!(results, vec![Ok(Value::F64(7.0)), Ok(Value::Bit(true))]);
    session.disconnect().await;
    sim.stop();
}

/// Bit-unit responses pack two points per byte; a run long enough to span
/// several bytes, with a deliberately irregular pattern and an odd length, is
/// what proves the nibble packing is decoded in the right order rather than
/// merely self-consistently.
#[tokio::test]
async fn a_long_odd_length_bit_run_decodes_in_the_right_order() {
    let sim = Simulator::start().await;
    let pattern = [
        true, false, false, true, true, true, false, false, true, false, true,
    ];
    for (i, &v) in pattern.iter().enumerate() {
        sim.set_bit(SlmpDevice::M, i as u32, v);
    }

    let mut session = connect_fast(&sim).await.unwrap();
    let specs: Vec<TagSpec> = (0..pattern.len())
        .map(|i| spec(&format!("M{i}"), n(DataType::Bit)))
        .collect();
    let results = session.read_batch(&specs).await.unwrap();
    for (i, &expected) in pattern.iter().enumerate() {
        assert_eq!(results[i], Ok(Value::Bit(expected)), "M{i} decoded wrong");
    }
    session.disconnect().await;
    sim.stop();
}

/// Hex-notation devices must reach the wire at their numeric value: a tag
/// written `X1A` has to read device 26, not device 1 or 10.
#[tokio::test]
async fn hexadecimal_device_numbers_reach_the_wire_as_their_numeric_value() {
    let sim = Simulator::start().await;
    sim.set_bit(SlmpDevice::X, 0x1A, true);
    sim.set_bit(SlmpDevice::X, 1, false);

    let mut session = connect_fast(&sim).await.unwrap();
    let results = session
        .read_batch(&[spec("X1A", n(DataType::Bit)), spec("X1", n(DataType::Bit))])
        .await
        .unwrap();
    assert_eq!(results, vec![Ok(Value::Bit(true)), Ok(Value::Bit(false))]);
    session.disconnect().await;
    sim.stop();
}

/// Both CPU series must work, since the frame layout differs (4-byte vs 6-byte
/// device field, different subcommands). (The old test also ran `SlmpCpu::L`;
/// `cpu_type` is `Q` or `R` in a connection's settings, and L's frames are Q's.)
#[tokio::test]
async fn both_cpu_series_frame_layouts_work() {
    for cpu in ["Q", "R"] {
        let sim = Simulator::start().await;
        sim.set_word(SlmpDevice::D, 100, 1234);
        sim.set_bit(SlmpDevice::M, 7, true);

        let mut session = connect_with(&sim, &[("cpu_type", cpu)], FAST)
            .await
            .unwrap_or_else(|e| panic!("{cpu} connect: {e:?}"));
        let results = session
            .read_batch(&[spec("D100", n(DataType::U16)), spec("M7", n(DataType::Bit))])
            .await
            .unwrap_or_else(|e| panic!("{cpu} read_batch: {e:?}"));
        assert_eq!(
            results,
            vec![Ok(Value::F64(1234.0)), Ok(Value::Bit(true))],
            "{cpu}"
        );
        session.disconnect().await;
        sim.stop();
    }
}

/// 要求を 1 つ受け取って中身だけ返す偽サーバーに、ドライバがエンジン経由で実際に
/// 送る 4E バイナリの要求フレームを丸ごと捕まえる。応答は返さない（セッションは
/// タイムアウトで失敗するが、このテストは要求のバイト列だけを見る）。
async fn capture_request_frame(cpu: &str, raw: &str, kind: TagKind) -> Vec<u8> {
    use tokio::io::AsyncReadExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        // 先頭 13 バイト（サブヘッダ〜データ長）のあと、データ長ぶん
        // （監視タイマー 2B から後ろ）が続く。
        let mut head = [0u8; 13];
        stream.read_exact(&mut head).await.unwrap();
        let rest_len = u16::from_le_bytes([head[11], head[12]]) as usize;
        let mut rest = vec![0u8; rest_len];
        stream.read_exact(&mut rest).await.unwrap();
        let mut frame = head.to_vec();
        frame.extend(rest);
        frame
    });
    let transport = TransportSpec::Tcp {
        host: addr.ip().to_string(),
        port: addr.port(),
    };
    let cx = ConnectContext::new(
        "p0-3",
        Duration::from_millis(500),
        Duration::from_millis(200),
    );
    let mut session = FrameSession::connect(
        &SlmpDriver,
        &settings(&addr.ip().to_string(), addr.port(), cpu),
        &transport,
        &cx,
        None,
    )
    .await
    .unwrap();
    let _ = session.read_batch(&[spec(raw, kind)]).await;
    server.await.unwrap()
}

/// #605: Q と R の要求フレームを、仕様（MC プロトコル 4E バイナリ、
/// コマンド 0401 一括読み出し）から手で書き下したバイト列で固定する。
/// 実装から導いた値ではない。Q は番地 3B + デバイスコード 1B で
/// サブコマンド 0x0000（ワード）/ 0x0001（ビット）、R は番地 4B +
/// デバイスコード 2B でサブコマンド 0x0002 / 0x0003。
/// 共通部分は、サブヘッダ 5400、シリアル 0100（既定の `serial_id` 1）、
/// 予約 0000、ネットワーク 00、PC FF、要求先 I/O FF03、局番 00、
/// データ長、監視タイマー 1000（既定 0x0010）。
#[tokio::test]
async fn request_frames_for_q_and_r_match_the_hand_written_wire_bytes() {
    // D100 を 1 ワード（D のコード 0xA8）。
    let q_word: Vec<u8> = vec![
        0x54, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0x03, 0x00, // 共通
        0x0C, 0x00, // データ長 = 2 + 2 + 2 + 4 + 2
        0x10, 0x00, // 監視タイマー
        0x01, 0x04, 0x00, 0x00, // コマンド 0401、サブコマンド 0000
        0x64, 0x00, 0x00, 0xA8, // 番地 100 (3B) + デバイスコード D
        0x01, 0x00, // 点数 1
    ];
    let r_word: Vec<u8> = vec![
        0x54, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0x03, 0x00, //
        0x0E, 0x00, // データ長 = 2 + 2 + 2 + 6 + 2
        0x10, 0x00, //
        0x01, 0x04, 0x02, 0x00, // サブコマンド 0002
        0x64, 0x00, 0x00, 0x00, 0xA8, 0x00, // 番地 100 (4B) + デバイスコード D (2B)
        0x01, 0x00,
    ];
    // M100 を 1 点（M のコード 0x90）。
    let q_bit: Vec<u8> = vec![
        0x54, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0x03, 0x00, //
        0x0C, 0x00, //
        0x10, 0x00, //
        0x01, 0x04, 0x01, 0x00, // サブコマンド 0001
        0x64, 0x00, 0x00, 0x90, //
        0x01, 0x00,
    ];
    let r_bit: Vec<u8> = vec![
        0x54, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0x03, 0x00, //
        0x0E, 0x00, //
        0x10, 0x00, //
        0x01, 0x04, 0x03, 0x00, // サブコマンド 0003
        0x64, 0x00, 0x00, 0x00, 0x90, 0x00, //
        0x01, 0x00,
    ];
    assert_eq!(
        capture_request_frame("Q", "D100", n(DataType::U16)).await,
        q_word,
        "Q ワード"
    );
    assert_eq!(
        capture_request_frame("R", "D100", n(DataType::U16)).await,
        r_word,
        "R ワード"
    );
    assert_eq!(
        capture_request_frame("Q", "M100", n(DataType::Bit)).await,
        q_bit,
        "Q ビット"
    );
    assert_eq!(
        capture_request_frame("R", "M100", n(DataType::Bit)).await,
        r_bit,
        "R ビット"
    );
}

/// A real non-zero end code, built by the real simulator from real bytes, is a
/// *per-request* `ItemError::Device` and leaves its batch-mates alone. The first
/// half drives a bare `slmp::SLMPClient` (the reference implementation, a
/// dev-dependency) against the same injected end code and asserts it comes back
/// as `SlmpError::Device { .. }` - the tripwire for the wrapped crate's own
/// classification (H9, docs/h9-slmp-structured-error-spec.md).
#[tokio::test]
async fn slmp_end_code_is_bad_not_fatal() {
    let sim = Simulator::start().await;
    sim.set_word(SlmpDevice::D, 100, 55);
    // D0 and D100 are far enough apart (gap > MAX_GAP) that planning puts them
    // in separate groups, so injecting on the D0 group must leave D100 intact.
    sim.inject_end_code(SlmpDevice::D, 0, 0xC059);

    // Structured check on the reference crate's own error type.
    let wire_config = banto_plc::SlmpConfig {
        host: sim.addr.ip().to_string(),
        port: sim.addr.port(),
        ..Default::default()
    };
    let mut raw = slmp::SLMPClient::new(wire_config.to_wire_props());
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

    // The driver's classification of the same condition, through the engine.
    let mut session = connect_fast(&sim).await.unwrap();
    let results = session
        .read_batch(&[spec("D0", n(DataType::I16)), spec("D100", n(DataType::I16))])
        .await
        .expect("whole call should still be Ok");
    match &results[0] {
        Err(ItemError::Device(e)) => {
            assert_eq!(e.code, 0xC059);
            assert!(
                !e.message.is_empty(),
                "the end code's symbolic name should survive translation"
            );
        }
        other => panic!("expected Err(Device), got {other:?}"),
    }
    assert_eq!(results[1], Ok(Value::F64(55.0)));

    // ...and the connection is still usable afterwards, which is the whole
    // point of calling it non-fatal.
    sim.clear_end_code(SlmpDevice::D, 0);
    sim.set_word(SlmpDevice::D, 0, 9);
    let results = session
        .read_batch(&[spec("D0", n(DataType::I16))])
        .await
        .unwrap();
    assert_eq!(results, vec![Ok(Value::F64(9.0))]);
    session.disconnect().await;
    sim.stop();
}

/// The other half of the pair: a framing failure is a *different*
/// `slmp::SlmpError` variant (`Framing`, not `Device`) and must come out fatal
/// (`FatalError::Framing`) with the connection torn down.
#[tokio::test]
async fn a_malformed_frame_is_fatal_even_though_it_shares_a_kind_with_an_end_code() {
    let sim = Simulator::start().await;
    sim.set_word(SlmpDevice::D, 0, 1);
    sim.emit_malformed_frames();

    // Structured check on the reference crate's own error type first.
    let wire_config = banto_plc::SlmpConfig {
        host: sim.addr.ip().to_string(),
        port: sim.addr.port(),
        ..Default::default()
    };
    let mut raw = slmp::SLMPClient::new(wire_config.to_wire_props());
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
        .expect_err("a malformed (corrupted response code) frame must surface as an Err");
    assert!(
        matches!(raw_err, slmp::SlmpError::Framing(_)),
        "expected SlmpError::Framing(_), got {raw_err:?}"
    );
    raw.close().await;

    let mut session = connect_fast(&sim).await.unwrap();
    let specs = [spec("D0", n(DataType::U16))];
    let err = session
        .read_batch(&specs)
        .await
        .expect_err("a malformed (corrupted response code) frame must fail the whole call");
    assert!(matches!(err, FatalError::Framing(_)), "got {err:?}");

    // Session torn down: the next call must fail immediately rather than keep
    // using a stream whose alignment is now unknown.
    assert!(!session.is_connected());
    assert!(matches!(
        session.read_batch(&specs).await,
        Err(FatalError::Protocol(_))
    ));
    sim.stop();
}

/// A device/data-type mismatch is resolved before any wire traffic and does
/// not stop the rest of the batch from reaching the CPU. (A bit tag at a word
/// device is refused by `parse_address` when saved; this is a spec that never
/// went through the save path.)
#[tokio::test]
async fn unsupported_combination_is_unsupported_and_the_batch_mate_is_still_read() {
    let sim = Simulator::start().await;
    sim.set_word(SlmpDevice::D, 1, 9);

    let mut session = connect_fast(&sim).await.unwrap();
    let results = session
        .read_batch(&[
            unchecked("D0", n(DataType::Bit)), // bit tag at a word device
            spec("D1", n(DataType::I16)),
        ])
        .await
        .unwrap();
    assert!(
        matches!(results[0], Err(ItemError::Unsupported(_))),
        "{results:?}"
    );
    assert_eq!(results[1], Ok(Value::F64(9.0)));
    session.disconnect().await;
    sim.stop();
}

/// The I2a-specific configuration mistake: a Modbus address on an SLMP
/// connection. Must be one item error, not a dead batch.
#[tokio::test]
async fn a_modbus_address_is_unsupported_and_the_batch_mate_is_still_read() {
    let sim = Simulator::start().await;
    sim.set_word(SlmpDevice::D, 0, 3);

    let mut session = connect_fast(&sim).await.unwrap();
    let foreign = TagSpec::new(
        DriverAddress::new(
            SlmpDriver.descriptor().id.clone(),
            "40001",
            "holding_register",
            true,
            1,
        ),
        n(DataType::U16),
    );
    let results = session
        .read_batch(&[foreign, spec("D0", n(DataType::U16))])
        .await
        .unwrap();
    assert!(
        matches!(results[0], Err(ItemError::Unsupported(_))),
        "{results:?}"
    );
    assert_eq!(results[1], Ok(Value::F64(3.0)));
    session.disconnect().await;
    sim.stop();
}

#[tokio::test]
async fn response_timeout_fails_the_call_and_tears_down_the_connection() {
    let sim = Simulator::start().await;
    sim.hang();

    let mut session = connect_fast(&sim).await.unwrap();
    let specs = [spec("D0", n(DataType::U16))];
    let err = session
        .read_batch(&specs)
        .await
        .expect_err("hung simulator should time out the whole call");
    assert!(matches!(err, FatalError::Timeout), "got {err:?}");
    assert!(!session.is_connected());

    // The connection is now considered dead - further calls must not hang
    // again on the same broken session, they must fail immediately.
    let started = Instant::now();
    let err = session.read_batch(&specs).await.unwrap_err();
    assert!(matches!(err, FatalError::Protocol(_)), "got {err:?}");
    assert!(started.elapsed() < Duration::from_millis(50));
    sim.stop();
}

/// A severed session must be fatal and must tear the session down. Whether
/// the broken stream is seen as a zero-byte read or a reset, both are fatal,
/// which is the property that matters.
#[tokio::test]
async fn disconnect_mid_session_fails_the_call_and_tears_down_the_connection() {
    let sim = Simulator::start().await;
    sim.set_word(SlmpDevice::D, 0, 1);

    let mut session = connect_with(&sim, &[], Duration::from_millis(500))
        .await
        .unwrap();
    let specs = [spec("D0", n(DataType::U16))];
    session.read_batch(&specs).await.expect("first read");

    sim.stop();

    let err = session.read_batch(&specs).await.unwrap_err();
    assert!(
        matches!(
            err,
            FatalError::Io(_) | FatalError::Timeout | FatalError::Framing(_)
        ),
        "a severed session must be fatal, got {err:?}"
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
    let mut session = connect_fast(&sim).await.unwrap();
    let read = [spec("D0", n(DataType::U16))];
    assert!(matches!(
        session.read_batch(&read).await,
        Err(FatalError::Timeout)
    ));
    let write = [WriteItem::new(read[0].clone(), Value::F64(1.0))];
    assert!(matches!(
        session.write_batch(&write).await,
        Err(FatalError::Protocol(_))
    ));
    assert!(matches!(
        session.read_batch(&read).await,
        Err(FatalError::Protocol(_))
    ));
    session.disconnect().await; // must not panic
    session.disconnect().await;
    sim.stop();
}

/// `disconnect` ends the session; a new connection to the same simulator
/// works like a brand new one.
#[tokio::test]
async fn disconnect_then_read_is_refused_then_a_new_connection_works() {
    let sim = Simulator::start().await;
    sim.set_word(SlmpDevice::D, 0, 5);

    let mut session = connect_fast(&sim).await.unwrap();
    let specs = [spec("D0", n(DataType::U16))];
    session.read_batch(&specs).await.unwrap();

    session.disconnect().await;
    assert!(matches!(
        session.read_batch(&specs).await,
        Err(FatalError::Protocol(_))
    ));

    let mut again = connect_fast(&sim).await.expect("reconnect should succeed");
    assert_eq!(
        again.read_batch(&specs).await.unwrap(),
        vec![Ok(Value::F64(5.0))]
    );
    again.disconnect().await;
    sim.stop();
}

#[tokio::test]
async fn disconnect_then_reconnect_after_a_fatal_error_works() {
    let sim1 = Simulator::start().await;
    let mut session = connect_fast(&sim1).await.unwrap();
    sim1.stop();
    let specs = [spec("D0", n(DataType::U16))];
    let err = session.read_batch(&specs).await.unwrap_err();
    assert!(
        matches!(
            err,
            FatalError::Io(_) | FatalError::Timeout | FatalError::Framing(_)
        ),
        "got {err:?}"
    );

    // The reconnect loop is the broker's; this is the driver's half: after a
    // fatal failure a fresh connection (here to a new simulator standing in
    // for "the CPU came back") works like a brand new one.
    let sim2 = Simulator::start().await;
    sim2.set_word(SlmpDevice::D, 0, 77);
    let mut again = connect_fast(&sim2).await.unwrap();
    assert_eq!(
        again.read_batch(&specs).await.unwrap(),
        vec![Ok(Value::F64(77.0))]
    );
    again.disconnect().await;
    sim2.stop();
}

/// A batch of exclusively unservable requests must still return one item error
/// per request rather than an `Err` - the result vector is always as long as
/// the request, in order.
#[tokio::test]
async fn a_batch_of_only_bad_requests_still_returns_one_result_each() {
    let sim = Simulator::start().await;
    let mut session = connect_fast(&sim).await.unwrap();
    let results = session
        .read_batch(&[
            unchecked("D0", n(DataType::Bit)),
            unchecked("M0", n(DataType::U16)),
        ])
        .await
        .expect("no wire traffic needed, so no way for this to fail");
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(Result::is_err), "{results:?}");
    session.disconnect().await;
    sim.stop();
}

#[tokio::test]
async fn an_empty_batch_is_ok_and_empty() {
    let sim = Simulator::start().await;
    let mut session = connect_fast(&sim).await.unwrap();
    assert!(session.read_batch(&[]).await.unwrap().is_empty());
    session.disconnect().await;
    sim.stop();
}

/// Performance smoke test (docs/plan.md I2 §7): 256 tags across consecutive `D`
/// registers, realistic for a single collection group near the v1 tag-count
/// target (recorder-requirements.md §3.1), which the planner's word cap fits
/// into a single round trip. Not a gate (no assertion on the timing).
#[tokio::test]
async fn performance_smoke_256_tags_x_1000_read_batch_calls() {
    const TAG_COUNT: u32 = 256;
    const ITERATIONS: u32 = 1000;

    let sim = Simulator::start().await;
    let values: Vec<u16> = (0..TAG_COUNT as u16).collect();
    sim.set_words(SlmpDevice::D, 0, &values);

    let mut session = connect_with(&sim, &[], Duration::from_secs(1))
        .await
        .unwrap();
    let specs: Vec<TagSpec> = (0..TAG_COUNT)
        .map(|i| spec(&format!("D{i}"), n(DataType::U16)))
        .collect();

    // One warm-up call outside the timed loop.
    let warm = session.read_batch(&specs).await.unwrap();
    assert_eq!(warm.len(), TAG_COUNT as usize);
    assert_eq!(warm[255], Ok(Value::F64(255.0)));

    let started = Instant::now();
    for _ in 0..ITERATIONS {
        let results = session.read_batch(&specs).await.unwrap();
        assert_eq!(results.len(), TAG_COUNT as usize);
        assert!(results.iter().all(Result::is_ok));
    }
    let elapsed = started.elapsed();
    let per_call = elapsed / ITERATIONS;
    println!(
        "[banto-driver-slmp perf smoke] {TAG_COUNT} tags x {ITERATIONS} read_batch calls (loopback \
         simulator, 1 round trip/call) in {elapsed:?} total, {per_call:?}/call average \
         (100ms/cycle target, recorder-requirements.md §3.1)"
    );
    session.disconnect().await;
    sim.stop();
}

// --- string reads (S1 文字列タグ) -------------------------------------------

fn sjis(words: u16) -> TagKind {
    TagKind::string(words, StringEncoding::ShiftJis)
}

/// UTF-8 bytes of `text` padded with NUL to `words` words, as the device
/// registers a PLC program would hold them (low byte first).
fn utf8_words(text: &str, words: usize) -> Vec<u16> {
    let mut bytes = text.as_bytes().to_vec();
    bytes.resize(words * 2, 0x00);
    bytes
        .chunks(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect()
}

/// ASCII, multi-byte SJIS, and a full-to-the-brim span, seeded via the
/// simulator's `set_string` and read back through the real wire path in one
/// mixed batch alongside numeric and bit tags.
#[tokio::test]
async fn a_mixed_batch_reads_strings_and_numerics_in_one_call() {
    let sim = Simulator::start().await;
    sim.set_string(SlmpDevice::D, 0, 4, "ABC"); // padded with NULs
    sim.set_string(SlmpDevice::D, 100, 4, "テスト"); // 6 SJIS bytes in 8
    sim.set_string(SlmpDevice::D, 200, 2, "ABCD"); // exactly 2L bytes, no NUL
    sim.set_word(SlmpDevice::D, 4, 42);
    sim.set_bit(SlmpDevice::M, 0, true);

    let mut session = connect_fast(&sim).await.expect("connect");
    // `Simulator::set_string` is Shift-JIS-only, so every string request here
    // is decoded as Shift-JIS - matching what was seeded.
    let specs = [
        spec("D0", sjis(4)),
        spec("D4", n(DataType::U16)), // adjacent: same group as D0..D3
        spec("D100", sjis(4)),
        spec("D200", sjis(2)),
        spec("M0", n(DataType::Bit)),
    ];
    let results = session.read_batch(&specs).await.expect("read_batch ok");
    assert_eq!(
        results,
        vec![
            Ok(Value::Str("ABC".into())), // trailing NUL padding must be trimmed
            Ok(Value::F64(42.0)),
            Ok(Value::Str("テスト".into())),
            Ok(Value::Str("ABCD".into())), // a full span: the whole 2L bytes
            Ok(Value::Bit(true)),
        ]
    );
    session.disconnect().await;
    sim.stop();
}

/// An embedded NUL cuts the string there, even mid-span.
#[tokio::test]
async fn a_string_read_trims_at_an_embedded_nul() {
    let sim = Simulator::start().await;
    // Bytes [0x41, 0x42, 0x00, 0x43] = "AB", NUL, "C" - seeded as raw words
    // (set_string cannot express an embedded NUL, which is the point).
    sim.set_words(SlmpDevice::D, 0, &[0x4241, 0x4300]);

    let mut session = connect_fast(&sim).await.unwrap();
    let results = session.read_batch(&[spec("D0", sjis(2))]).await.unwrap();
    assert_eq!(results, vec![Ok(Value::Str("AB".into()))]);
    session.disconnect().await;
    sim.stop();
}

/// Two wide strings whose combined span exceeds the single-read word cap are
/// split into two wire reads by the planner and both still decode correctly.
#[tokio::test]
async fn string_reads_spanning_the_batching_boundary_split_and_still_decode() {
    let sim = Simulator::start().await;
    // 300 + 300 words > 480 (the single-bulk-read cap), so the planner must
    // split. Each string's text sits at the start of its span, NUL-padded.
    sim.set_string(SlmpDevice::D, 0, 300, "FIRST");
    sim.set_string(SlmpDevice::D, 300, 300, "SECOND");

    let mut session = connect_with(&sim, &[], Duration::from_secs(1))
        .await
        .unwrap();
    let results = session
        .read_batch(&[spec("D0", sjis(300)), spec("D300", sjis(300))])
        .await
        .unwrap();
    assert_eq!(
        results,
        vec![
            Ok(Value::Str("FIRST".into())),
            Ok(Value::Str("SECOND".into()))
        ]
    );
    session.disconnect().await;
    sim.stop();
}

/// A string span the wire cannot serve is that tag's own item error through the
/// full session too - its batch-mates still read.
#[tokio::test]
async fn an_over_cap_string_is_unsupported_without_blocking_batch_mates() {
    let sim = Simulator::start().await;
    sim.set_word(SlmpDevice::D, 0, 7);

    let mut session = connect_fast(&sim).await.unwrap();
    let results = session
        .read_batch(&[unchecked("D1000", sjis(481)), spec("D0", n(DataType::U16))])
        .await
        .unwrap();
    assert!(
        // The frame path carries no per-span reason (the engine answers every
        // spec the planner skipped with this text); the old client's
        // `StringSpanUnsupported { words: 481 }` text is not reproduced.
        matches!(&results[0], Err(ItemError::Unsupported(m)) if m == "計画に含まれない"),
        "{results:?}"
    );
    assert_eq!(results[1], Ok(Value::F64(7.0)));
    session.disconnect().await;
    sim.stop();
}

// --- T20 ①b: StringEncoding::Utf8 read (docs/banto-hub-t20-design.md §3.1) ---

/// The load-bearing UTF-8 read proof: seed the wire directly with UTF-8 bytes
/// (`Simulator::set_string` is Shift-JIS-only by design) and read them back with
/// `StringEncoding::Utf8`. "テスト" is 9 bytes in UTF-8 vs. 6 in Shift-JIS, so
/// this proves the encoding choice is actually threaded into the decode step.
#[tokio::test]
async fn a_string_read_decodes_utf8_when_requested() {
    let sim = Simulator::start().await;
    let text = "テスト";
    sim.set_words(SlmpDevice::D, 0, &utf8_words(text, 5)); // 9 bytes + 1 NUL

    let mut session = connect_fast(&sim).await.unwrap();
    let results = session
        .read_batch(&[spec("D0", TagKind::string(5, StringEncoding::Utf8))])
        .await
        .unwrap();
    assert_eq!(results, vec![Ok(Value::Str(text.into()))]);
    session.disconnect().await;
    sim.stop();
}

/// Companion to the test above: the exact same UTF-8 wire bytes, read with
/// `StringEncoding::ShiftJis`, must NOT decode back to the original text.
#[tokio::test]
async fn the_same_utf8_wire_bytes_do_not_decode_correctly_as_shift_jis() {
    let sim = Simulator::start().await;
    let text = "テスト";
    sim.set_words(SlmpDevice::D, 0, &utf8_words(text, 5));

    let mut session = connect_fast(&sim).await.unwrap();
    let results = session.read_batch(&[spec("D0", sjis(5))]).await.unwrap();
    match &results[0] {
        Ok(Value::Str(s)) => assert_ne!(
            s, text,
            "UTF-8 wire bytes must not read back as the same text under SJIS"
        ),
        // Also an acceptable outcome: the UTF-8 bytes are not valid SJIS at all.
        Err(_) => {}
        other => panic!("unexpected result: {other:?}"),
    }
    session.disconnect().await;
    sim.stop();
}

/// A mixed batch reading the same text at two addresses with two different
/// encodings must decode each independently.
#[tokio::test]
async fn a_mixed_batch_decodes_each_string_request_independently() {
    let sim = Simulator::start().await;
    let text = "テスト";
    sim.set_string(SlmpDevice::D, 0, 4, text); // Shift-JIS, 6 bytes in 4 words.
    sim.set_words(SlmpDevice::D, 100, &utf8_words(text, 5));

    let mut session = connect_fast(&sim).await.unwrap();
    let results = session
        .read_batch(&[
            spec("D0", sjis(4)),
            spec("D100", TagKind::string(5, StringEncoding::Utf8)),
        ])
        .await
        .unwrap();
    assert_eq!(
        results,
        vec![Ok(Value::Str(text.into())), Ok(Value::Str(text.into()))],
        "each span decodes with its own encoding in the same batch"
    );
    session.disconnect().await;
    sim.stop();
}

// ---------------------------------------------------------------------------
// The session-type unit tests of `api_session.rs`, through the engine
// ---------------------------------------------------------------------------

/// The healthy tag is read, the one past the limit is that tag's error, and
/// nothing is written for it.
#[tokio::test]
async fn the_engine_never_sends_a_request_past_the_last_device_number() {
    let sim = Simulator::start().await;
    sim.set_word(SlmpDevice::D, 0, 7);
    let mut s = connect_with(&sim, &[], Duration::from_secs(1))
        .await
        .unwrap();
    let specs = [
        spec("D0", n(DataType::U16)),
        unchecked("D16777215", n(DataType::U32)),
    ];
    let r = s.read_batch(&specs).await.unwrap();
    assert_eq!(r[0], Ok(Value::F64(7.0)));
    assert!(matches!(r[1], Err(ItemError::Unsupported(_))), "{r:?}");
    let w = s
        .write_batch(&[WriteItem::new(specs[1].clone(), Value::F64(1.0))])
        .await
        .unwrap();
    assert!(
        matches!(w.as_slice(), [Err(ItemError::Unsupported(_))]),
        "{w:?}"
    );
    assert_eq!(sim.write_command_count(), 0);
    s.disconnect().await;
    sim.stop();
}

/// #793 の 3 回目: 最後に占める番号がちょうど上限 0xFFFFFF で終わる書き込み
/// は受け付け、シミュレータに実際に書かれる（語・2 語・ビットデバイス）。
#[tokio::test]
async fn a_write_ending_exactly_at_the_last_device_number_is_sent() {
    let sim = Simulator::start().await;
    let mut s = connect_with(&sim, &[], Duration::from_secs(1))
        .await
        .unwrap();
    let items = [
        // 70000 = 0x0001_1170, low word first.
        WriteItem::new(spec("ZR16777214", n(DataType::U32)), Value::F64(70000.0)),
        WriteItem::new(spec("D16777215", n(DataType::U16)), Value::F64(4660.0)),
        WriteItem::new(spec("M16777215", n(DataType::Bit)), Value::Bit(true)),
    ];
    let w = s.write_batch(&items).await.unwrap();
    assert_eq!(w, vec![Ok(()), Ok(()), Ok(())]);
    assert_eq!(sim.get_word(SlmpDevice::ZR, 16_777_214), 0x1170);
    assert_eq!(sim.get_word(SlmpDevice::ZR, 16_777_215), 0x0001);
    assert_eq!(sim.get_word(SlmpDevice::D, 16_777_215), 0x1234);
    assert!(sim.get_bit(SlmpDevice::M, 16_777_215));
    assert_eq!(sim.write_command_count(), 3);
    s.disconnect().await;
    sim.stop();
}

/// #793 の 3 回目: 正常なタグと上限をはみ出すタグを 1 回の書き込みに混ぜる
/// と、正常なタグだけが書かれ、はみ出すタグはそのタグだけの項目エラー。
/// シミュレータは書かれた番号をすべて held の集合に記録するので、その数で
/// 上限を越える番号への書き込みが 1 件も届いていないことを確かめる。
#[tokio::test]
async fn a_write_mixing_healthy_and_overrunning_tags_writes_only_the_healthy() {
    let sim = Simulator::start().await;
    let mut s = connect_with(&sim, &[], Duration::from_secs(1))
        .await
        .unwrap();
    let items = [
        WriteItem::new(spec("D0", n(DataType::U16)), Value::F64(5.0)),
        WriteItem::new(unchecked("D16777215", n(DataType::U32)), Value::F64(1.0)),
        WriteItem::new(spec("M10", n(DataType::Bit)), Value::Bit(true)),
        WriteItem::new(
            unchecked("ZR16777214", TagKind::string(3, StringEncoding::ShiftJis)),
            Value::Str("abc".into()),
        ),
    ];
    let w = s.write_batch(&items).await.unwrap();
    assert_eq!(w[0], Ok(()));
    assert!(
        matches!(&w[1], Err(ItemError::Unsupported(m)) if m.contains("2 語分")),
        "{w:?}"
    );
    assert_eq!(w[2], Ok(()));
    assert!(
        matches!(&w[3], Err(ItemError::Unsupported(m)) if m.contains("3 語分")),
        "{w:?}"
    );
    assert_eq!(sim.get_word(SlmpDevice::D, 0), 5);
    assert!(sim.get_bit(SlmpDevice::M, 10));
    // Two write commands, two devices written: D0 and M10, nothing else.
    assert_eq!(sim.write_command_count(), 2);
    assert_eq!(sim.held_count(), 2);
    assert_eq!(sim.get_word(SlmpDevice::D, 16_777_215), 0);
    assert_eq!(sim.get_word(SlmpDevice::ZR, 16_777_214), 0);
    s.disconnect().await;
    sim.stop();
}

/// The type of a fatal error survives to the host: a response that never comes
/// is `Timeout`, a closed socket `Io` (or the framing error of the empty read).
#[tokio::test]
async fn fatal_errors_keep_their_type() {
    let sim = Simulator::start().await;
    let specs = [spec("D100", n(DataType::U16))];

    let mut s = connect_with(&sim, &[], Duration::from_millis(200))
        .await
        .unwrap();
    sim.hang();
    let r = s.read_batch(&specs).await;
    assert!(matches!(r, Err(FatalError::Timeout)), "{r:?}");
    sim.stop_hanging();

    let mut s = connect_with(&sim, &[], Duration::from_millis(200))
        .await
        .unwrap();
    sim.stop();
    let r = s.read_batch(&specs).await;
    assert!(
        matches!(r, Err(FatalError::Io(_) | FatalError::Framing(_))),
        "{r:?}"
    );
}

// ---------------------------------------------------------------------------
// Writes (was `banto-plc-write/src/slmp/integration_tests.rs`)
// ---------------------------------------------------------------------------

use banto_plc_write::slmp::simulator::Simulator as WriteSimulator;
use banto_plc_write::PlcWriteError;

/// A session to the write crate's simulator (the one with the wire-operation
/// counters and the confirmation-race injection).
async fn connect_ws(sim: &WriteSimulator) -> Result<FrameSession, FatalError> {
    let host = sim.addr.ip().to_string();
    let transport = TransportSpec::Tcp {
        host: host.clone(),
        port: sim.addr.port(),
    };
    let settings = Settings::from_pairs([
        ("host".to_string(), host),
        ("port".to_string(), sim.addr.port().to_string()),
    ])
    .unwrap();
    let cx = ConnectContext::new("p0-3", Duration::from_secs(2), FAST);
    FrameSession::connect(&SlmpDriver, &settings, &transport, &cx, None).await
}

fn num(text: &str, data_type: DataType, v: f64) -> WriteItem {
    WriteItem::new(spec(text, n(data_type)), Value::F64(v))
}

fn bitw(text: &str, v: bool) -> WriteItem {
    WriteItem::new(spec(text, n(DataType::Bit)), Value::Bit(v))
}

fn strw(text: &str, kind: TagKind, v: &str) -> WriteItem {
    WriteItem::new(spec(text, kind), Value::Str(v.into()))
}

fn all_ok(results: &[Result<(), ItemError>]) -> bool {
    results.iter().all(Result::is_ok)
}

#[tokio::test]
async fn write_batch_writes_every_data_type_and_reads_back_through_a_second_session() {
    let sim = Simulator::start().await;
    let mut session = connect_fast(&sim).await.expect("connect");
    let items = [
        num("D0", DataType::U16, 42.0),
        num("D1", DataType::I16, -1.0),
        num("D2", DataType::U32, f64::from(0x0001_0002_u32)),
        num("D4", DataType::F32, 1.5),
        num("R0", DataType::U16, 100.0),
        bitw("M0", true),
        bitw("X0", false),
    ];
    let results = session.write_batch(&items).await.expect("write_batch ok");
    assert_eq!(results.len(), items.len());
    assert!(all_ok(&results), "every write should succeed: {results:?}");

    // Read every value back through a second session (the real read path).
    let mut reader = connect_fast(&sim).await.expect("reader connect");
    let read_back = reader
        .read_batch(&[
            spec("D0", n(DataType::U16)),
            spec("D1", n(DataType::I16)),
            spec("D2", n(DataType::U32)),
            spec("D4", n(DataType::F32)),
            spec("R0", n(DataType::U16)),
            spec("M0", n(DataType::Bit)),
            spec("X0", n(DataType::Bit)),
        ])
        .await
        .expect("read back");
    assert_eq!(
        read_back,
        vec![
            Ok(Value::F64(42.0)),
            Ok(Value::F64(-1.0)),
            Ok(Value::F64(f64::from(0x0001_0002_u32))),
            Ok(Value::F64(1.5)),
            Ok(Value::F64(100.0)),
            Ok(Value::Bit(true)),
            Ok(Value::Bit(false)),
        ]
    );
    reader.disconnect().await;
    session.disconnect().await;
    sim.stop();
}

/// The default word order (`low_high`) is the thing most likely to be silently
/// wrong against real hardware: the raw simulator words must be low-word-first,
/// and a `high_low` write of the same value must produce the mirror image.
#[tokio::test]
async fn default_low_high_word_order_round_trips_and_lands_low_word_first() {
    let sim = Simulator::start().await;
    let mut session = connect_fast(&sim).await.unwrap();

    // f32 1.5 = 0x3FC00000. low_high -> low word 0x0000 in D0, high 0x3FC0 in D1.
    session
        .write_batch(&[num("D0", DataType::F32, 1.5)])
        .await
        .unwrap();
    assert_eq!(sim.get_word(SlmpDevice::D, 0), 0x0000, "low word in D0");
    assert_eq!(sim.get_word(SlmpDevice::D, 1), 0x3FC0, "high word in D1");

    let mut reader = connect_fast(&sim).await.unwrap();
    let rb = reader
        .read_batch(&[spec("D0", n(DataType::F32))])
        .await
        .unwrap();
    assert_eq!(rb, vec![Ok(Value::F64(1.5))]);

    let mut swapped = connect_with(&sim, &[("word_order", "high_low")], FAST)
        .await
        .unwrap();
    swapped
        .write_batch(&[num("D10", DataType::F32, 1.5)])
        .await
        .unwrap();
    assert_eq!(
        sim.get_word(SlmpDevice::D, 10),
        0x3FC0,
        "high word first now"
    );
    assert_eq!(sim.get_word(SlmpDevice::D, 11), 0x0000);
    swapped.disconnect().await;
    reader.disconnect().await;
    session.disconnect().await;
    sim.stop();
}

/// Bit-unit writes pack two points per byte; a run long enough to span several
/// bytes, with an irregular pattern and odd length, proves the nibble packing
/// is emitted in the right order (read back through a second session).
#[tokio::test]
async fn a_long_odd_length_bit_run_writes_in_the_right_order() {
    let sim = Simulator::start().await;
    let mut session = connect_fast(&sim).await.unwrap();

    let pattern = [
        true, false, false, true, true, true, false, false, true, false, true,
    ];
    let items: Vec<WriteItem> = pattern
        .iter()
        .enumerate()
        .map(|(i, &v)| bitw(&format!("M{i}"), v))
        .collect();
    let results = session.write_batch(&items).await.unwrap();
    assert!(all_ok(&results), "{results:?}");

    let mut reader = connect_fast(&sim).await.unwrap();
    let specs: Vec<TagSpec> = (0..pattern.len())
        .map(|i| spec(&format!("M{i}"), n(DataType::Bit)))
        .collect();
    let rb = reader.read_batch(&specs).await.unwrap();
    for (i, &expected) in pattern.iter().enumerate() {
        assert_eq!(rb[i], Ok(Value::Bit(expected)), "M{i} round-tripped wrong");
    }
    reader.disconnect().await;
    session.disconnect().await;
    sim.stop();
}

/// Hex-notation devices must write at their numeric value: `X1A` writes device
/// 26, not 1 or 10.
#[tokio::test]
async fn hexadecimal_device_numbers_write_at_their_numeric_value() {
    let sim = Simulator::start().await;
    let mut session = connect_fast(&sim).await.unwrap();
    session
        .write_batch(&[bitw("X1A", true), bitw("X1", false)])
        .await
        .unwrap();
    assert!(sim.get_bit(SlmpDevice::X, 0x1A));
    assert!(!sim.get_bit(SlmpDevice::X, 0x01));
    session.disconnect().await;
    sim.stop();
}

/// Both CPU series must work, since the write frame's device field differs
/// (4-byte vs 6-byte). (`SlmpCpu::L` is not a `cpu_type` setting.)
#[tokio::test]
async fn both_cpu_series_write_frame_layouts_work() {
    for cpu in ["Q", "R"] {
        let sim = Simulator::start().await;
        let mut session = connect(&sim, cpu)
            .await
            .unwrap_or_else(|e| panic!("{cpu} connect: {e:?}"));
        let results = session
            .write_batch(&[num("D100", DataType::U16, 1234.0), bitw("M7", true)])
            .await
            .unwrap_or_else(|e| panic!("{cpu} write_batch: {e:?}"));
        assert!(all_ok(&results), "{cpu}: {results:?}");
        assert_eq!(sim.get_word(SlmpDevice::D, 100), 1234, "{cpu} word");
        assert!(sim.get_bit(SlmpDevice::M, 7), "{cpu} bit");
        session.disconnect().await;
        sim.stop();
    }
}

/// The write twin of `slmp_end_code_is_bad_not_fatal`: a real non-zero end
/// code on a *write* is a per-item `Device` error that leaves its batch-mates
/// alone, and the connection stays usable. The `slmp` crate's own structured
/// `SlmpError::Device` is checked first (H9).
#[tokio::test]
async fn slmp_write_end_code_is_bad_not_fatal() {
    let sim = Simulator::start().await;
    // D0 and D100 are far apart, so they go in separate groups; injecting on
    // the D0 group must leave the D100 write intact.
    sim.inject_end_code(SlmpDevice::D, 0, 0xC061); // WrongLength

    let wire_config = banto_plc::SlmpConfig {
        host: sim.addr.ip().to_string(),
        port: sim.addr.port(),
        ..Default::default()
    };
    let mut raw = slmp::SLMPClient::new(wire_config.to_wire_props());
    raw.set_send_timeout(Duration::from_millis(100));
    raw.set_recv_timeout(Duration::from_millis(100));
    raw.connect().await.expect("raw connect");
    let raw_err = raw
        .bulk_write(
            slmp::Device {
                device_type: SlmpDevice::D.to_wire(),
                address: 0,
            },
            &[slmp::TypedData::U16(5)],
        )
        .await
        .expect_err("an injected end code must surface as an Err");
    assert!(
        matches!(raw_err, slmp::SlmpError::Device { end_code: 0xC061 }),
        "expected SlmpError::Device {{ end_code: 0xC061 }}, got {raw_err:?}"
    );
    raw.close().await;

    let mut session = connect_fast(&sim).await.unwrap();
    let results = session
        .write_batch(&[
            num("D0", DataType::I16, 5.0),
            num("D100", DataType::I16, 9.0),
        ])
        .await
        .expect("whole call should still be Ok");
    match &results[0] {
        Err(ItemError::Device(e)) => {
            assert_eq!(e.code, 0xC061);
            assert!(
                !e.message.is_empty(),
                "the end code's symbolic name should survive translation"
            );
        }
        other => panic!("expected Err(Device), got {other:?}"),
    }
    assert_eq!(results[1], Ok(()));
    assert_eq!(sim.get_word(SlmpDevice::D, 100), 9);

    sim.clear_end_code(SlmpDevice::D, 0);
    let results = session
        .write_batch(&[num("D0", DataType::I16, 7.0)])
        .await
        .unwrap();
    assert_eq!(results, vec![Ok(())]);
    assert_eq!(sim.get_word(SlmpDevice::D, 0), 7);
    session.disconnect().await;
    sim.stop();
}

/// The other half of the pair: a framing failure on a write is a *different*
/// `slmp::SlmpError` variant (`Framing`) and must come out fatal and tear the
/// session down.
#[tokio::test]
async fn a_malformed_frame_on_a_write_is_fatal_and_tears_down_the_connection() {
    let sim = Simulator::start().await;
    sim.emit_malformed_frames();

    let wire_config = banto_plc::SlmpConfig {
        host: sim.addr.ip().to_string(),
        port: sim.addr.port(),
        ..Default::default()
    };
    let mut raw = slmp::SLMPClient::new(wire_config.to_wire_props());
    raw.set_send_timeout(Duration::from_millis(100));
    raw.set_recv_timeout(Duration::from_millis(100));
    raw.connect().await.expect("raw connect");
    let raw_err = raw
        .bulk_write(
            slmp::Device {
                device_type: SlmpDevice::D.to_wire(),
                address: 0,
            },
            &[slmp::TypedData::U16(1)],
        )
        .await
        .expect_err("a malformed (corrupted response code) frame must surface as an Err");
    assert!(
        matches!(raw_err, slmp::SlmpError::Framing(_)),
        "expected SlmpError::Framing(_), got {raw_err:?}"
    );
    raw.close().await;

    let mut session = connect_fast(&sim).await.unwrap();
    let items = [num("D0", DataType::U16, 1.0)];
    let err = session
        .write_batch(&items)
        .await
        .expect_err("a malformed (corrupted response code) frame must fail the whole call");
    assert!(matches!(err, FatalError::Framing(_)), "got {err:?}");

    assert!(!session.is_connected());
    assert!(matches!(
        session.write_batch(&items).await,
        Err(FatalError::Protocol(_))
    ));
    sim.stop();
}

/// A device/data-type mismatch is resolved before any wire traffic and does not
/// stop the rest of the batch (and must not touch the device).
#[tokio::test]
async fn unsupported_combination_write_is_unsupported_and_the_batch_mate_is_still_written() {
    let sim = Simulator::start().await;
    let mut session = connect_fast(&sim).await.unwrap();
    let results = session
        .write_batch(&[
            WriteItem::new(unchecked("D0", n(DataType::Bit)), Value::Bit(true)),
            num("D1", DataType::I16, 9.0),
        ])
        .await
        .unwrap();
    assert!(
        matches!(results[0], Err(ItemError::Unsupported(_))),
        "{results:?}"
    );
    assert_eq!(results[1], Ok(()));
    assert_eq!(sim.get_word(SlmpDevice::D, 1), 9);
    assert_eq!(sim.write_command_count(), 1, "only D1 reached the wire");
    session.disconnect().await;
    sim.stop();
}

/// A Modbus address on an SLMP write connection: one item error, not a dead
/// batch, and the wire is never touched for it.
#[tokio::test]
async fn a_modbus_address_write_is_unsupported_and_the_batch_mate_is_still_written() {
    let sim = Simulator::start().await;
    let mut session = connect_fast(&sim).await.unwrap();
    let foreign = TagSpec::new(
        DriverAddress::new(
            SlmpDriver.descriptor().id.clone(),
            "40001",
            "holding_register",
            true,
            1,
        ),
        n(DataType::U16),
    );
    let results = session
        .write_batch(&[
            WriteItem::new(foreign, Value::F64(1.0)),
            num("D0", DataType::U16, 3.0),
        ])
        .await
        .unwrap();
    assert!(
        matches!(results[0], Err(ItemError::Unsupported(_))),
        "{results:?}"
    );
    assert_eq!(results[1], Ok(()));
    assert_eq!(sim.get_word(SlmpDevice::D, 0), 3);
    assert_eq!(sim.write_command_count(), 1, "only D0 reached the wire");
    session.disconnect().await;
    sim.stop();
}

/// An un-encodable value is a per-item error and never reaches the CPU.
#[tokio::test]
async fn an_out_of_range_value_is_unsupported_without_touching_the_wire() {
    let sim = Simulator::start().await;
    let mut session = connect_fast(&sim).await.unwrap();
    let results = session
        .write_batch(&[
            num("D0", DataType::U16, 70000.0), // out of range for u16
            num("D1", DataType::U16, 5.0),
        ])
        .await
        .unwrap();
    assert!(
        matches!(results[0], Err(ItemError::Unsupported(_))),
        "{results:?}"
    );
    assert_eq!(results[1], Ok(()));
    assert_eq!(sim.get_word(SlmpDevice::D, 1), 5);
    assert_eq!(sim.get_word(SlmpDevice::D, 0), 0, "never written");
    assert_eq!(sim.write_command_count(), 1, "only D1 reached the wire");
    session.disconnect().await;
    sim.stop();
}

#[tokio::test]
async fn write_response_timeout_fails_the_call_and_tears_down_the_connection() {
    let sim = Simulator::start().await;
    sim.hang();
    let mut session = connect_fast(&sim).await.unwrap();
    let items = [num("D0", DataType::U16, 1.0)];
    let err = session
        .write_batch(&items)
        .await
        .expect_err("hung simulator should time out the whole call");
    assert!(matches!(err, FatalError::Timeout), "got {err:?}");
    assert!(!session.is_connected());
    assert!(matches!(
        session.write_batch(&items).await,
        Err(FatalError::Protocol(_))
    ));
    sim.stop();
}

#[tokio::test]
async fn write_disconnect_mid_session_fails_the_call_and_tears_down_the_connection() {
    let sim = Simulator::start().await;
    let mut session = connect_with(&sim, &[], Duration::from_millis(500))
        .await
        .unwrap();
    session
        .write_batch(&[num("D0", DataType::U16, 1.0)])
        .await
        .expect("first write should succeed");

    sim.stop();

    let err = session
        .write_batch(&[num("D0", DataType::U16, 2.0)])
        .await
        .expect_err("severed connection should fail the call");
    assert!(
        matches!(
            err,
            FatalError::Io(_) | FatalError::Timeout | FatalError::Framing(_)
        ),
        "a severed session must be fatal, got {err:?}"
    );
    assert!(!session.is_connected());
    assert!(matches!(
        session.write_batch(&[num("D0", DataType::U16, 1.0)]).await,
        Err(FatalError::Protocol(_))
    ));
}

#[tokio::test]
async fn disconnect_then_write_is_refused_then_a_new_connection_works() {
    let sim = Simulator::start().await;
    let mut session = connect_fast(&sim).await.unwrap();
    session
        .write_batch(&[num("D0", DataType::U16, 5.0)])
        .await
        .unwrap();

    session.disconnect().await;
    assert!(matches!(
        session.write_batch(&[num("D0", DataType::U16, 1.0)]).await,
        Err(FatalError::Protocol(_))
    ));

    let mut again = connect_fast(&sim).await.expect("reconnect");
    let results = again
        .write_batch(&[num("D0", DataType::U16, 6.0)])
        .await
        .unwrap();
    assert_eq!(results, vec![Ok(())]);
    assert_eq!(sim.get_word(SlmpDevice::D, 0), 6);
    again.disconnect().await;
    sim.stop();
}

#[tokio::test]
async fn write_reconnect_after_a_fatal_error_works() {
    let sim1 = Simulator::start().await;
    let mut session = connect_fast(&sim1).await.unwrap();
    sim1.stop();
    let err = session
        .write_batch(&[num("D0", DataType::U16, 1.0)])
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            FatalError::Io(_) | FatalError::Timeout | FatalError::Framing(_)
        ),
        "got {err:?}"
    );

    let sim2 = Simulator::start().await;
    let mut again = connect_fast(&sim2).await.expect("reconnect to new sim");
    again
        .write_batch(&[num("D0", DataType::U16, 77.0)])
        .await
        .unwrap();
    assert_eq!(sim2.get_word(SlmpDevice::D, 0), 77);
    again.disconnect().await;
    sim2.stop();
}

/// A batch of exclusively unservable requests still returns one item error per
/// request rather than an `Err` - the result vector is always as long as the
/// request, in order.
#[tokio::test]
async fn a_batch_of_only_bad_writes_still_returns_one_result_each() {
    let sim = Simulator::start().await;
    let mut session = connect_fast(&sim).await.unwrap();
    let results = session
        .write_batch(&[
            WriteItem::new(unchecked("D0", n(DataType::Bit)), Value::Bit(true)),
            WriteItem::new(unchecked("M0", n(DataType::U16)), Value::F64(1.0)),
        ])
        .await
        .expect("no wire traffic needed, so no way for this to fail");
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(Result::is_err), "{results:?}");
    assert_eq!(sim.write_command_count(), 0);
    session.disconnect().await;
    sim.stop();
}

#[tokio::test]
async fn an_empty_write_batch_is_ok_and_empty() {
    let sim = Simulator::start().await;
    let mut session = connect_fast(&sim).await.unwrap();
    assert!(session.write_batch(&[]).await.unwrap().is_empty());
    session.disconnect().await;
    sim.stop();
}

// --- string write/read round trips (S1 文字列タグ) ---------------------------

/// ASCII, multi-byte SJIS, and an exactly-full span, all written in one batch
/// and read back equal (with the short ones NUL-trimmed).
#[tokio::test]
async fn string_write_then_read_back_round_trips_ascii_sjis_and_full_spans() {
    let sim = Simulator::start().await;
    let mut session = connect_fast(&sim).await.expect("connect");
    let results = session
        .write_batch(&[
            strw("D0", sjis(4), "ABC"),      // ASCII, padded
            strw("D100", sjis(4), "テスト"), // multi-byte SJIS (6 bytes in 8)
            strw("D200", sjis(2), "ABCD"),   // exactly 2L bytes, no room for NUL
        ])
        .await
        .expect("write_batch ok");
    assert!(all_ok(&results), "{results:?}");

    let mut reader = connect_fast(&sim).await.expect("reader connect");
    let read_back = reader
        .read_batch(&[
            spec("D0", sjis(4)),
            spec("D100", sjis(4)),
            spec("D200", sjis(2)),
        ])
        .await
        .expect("read back");
    assert_eq!(
        read_back,
        vec![
            Ok(Value::Str("ABC".into())),
            Ok(Value::Str("テスト".into())),
            Ok(Value::Str("ABCD".into())),
        ]
    );
    reader.disconnect().await;
    session.disconnect().await;
    sim.stop();
}

/// The wire-level byte-order pin: after writing "AB", the device word must be
/// 0x4241 (low byte = first character), observed directly in the simulator's
/// state, and the padding must overwrite what was there.
#[tokio::test]
async fn string_write_lands_low_byte_first_with_nul_padding_on_the_device() {
    let sim = Simulator::start().await;
    let mut session = connect_fast(&sim).await.unwrap();
    sim.set_word(SlmpDevice::D, 1, 0xDEAD);
    sim.set_word(SlmpDevice::D, 2, 0xBEEF);

    let results = session
        .write_batch(&[strw("D0", sjis(3), "AB")])
        .await
        .unwrap();
    assert_eq!(results, vec![Ok(())]);

    assert_eq!(sim.get_word(SlmpDevice::D, 0), 0x4241, "low byte first");
    assert_eq!(sim.get_word(SlmpDevice::D, 1), 0x0000, "padding overwrites");
    assert_eq!(sim.get_word(SlmpDevice::D, 2), 0x0000, "padding overwrites");
    session.disconnect().await;
    sim.stop();
}

/// T20 ①a: `StringEncoding::Utf8` really changes what lands on the wire.
#[tokio::test]
async fn utf8_string_write_lands_the_correct_bytes_on_the_wire() {
    let sim = Simulator::start().await;
    let mut session = connect_fast(&sim).await.unwrap();

    let text = "テスト";
    let words = 5u16; // 9 UTF-8 bytes fit in 5 words (10 bytes), 1 byte of NUL padding.
    let results = session
        .write_batch(&[strw(
            "D0",
            TagKind::string(words, StringEncoding::Utf8),
            text,
        )])
        .await
        .unwrap();
    assert_eq!(results, vec![Ok(())]);

    let landed: Vec<u8> = (0..words)
        .flat_map(|i| sim.get_word(SlmpDevice::D, u32::from(i)).to_le_bytes())
        .collect();
    let mut expected = text.as_bytes().to_vec();
    expected.resize(usize::from(words) * 2, 0x00);
    assert_eq!(landed, expected, "UTF-8 bytes, NUL-padded, low byte first");

    let mut trimmed = landed.clone();
    while trimmed.last() == Some(&0) {
        trimmed.pop();
    }
    assert_eq!(std::str::from_utf8(&trimmed).unwrap(), text);
    session.disconnect().await;
    sim.stop();
}

/// T20 ①a companion: each item's own `encoding` is honored independently
/// within one batch (the same text lands different bytes).
#[tokio::test]
async fn a_mixed_batch_encodes_each_string_write_independently() {
    let sim = Simulator::start().await;
    let mut session = connect_fast(&sim).await.unwrap();

    let text = "テスト";
    let results = session
        .write_batch(&[
            strw("D0", TagKind::string(5, StringEncoding::Utf8), text),
            strw("D100", sjis(4), text),
        ])
        .await
        .unwrap();
    assert_eq!(results, vec![Ok(()), Ok(())]);

    let utf8_bytes: Vec<u8> = (0..5)
        .flat_map(|i| sim.get_word(SlmpDevice::D, i).to_le_bytes())
        .collect();
    let mut expected_utf8 = text.as_bytes().to_vec();
    expected_utf8.resize(10, 0x00);
    assert_eq!(utf8_bytes, expected_utf8);

    let sjis_bytes: Vec<u8> = (100..104)
        .flat_map(|i| sim.get_word(SlmpDevice::D, i).to_le_bytes())
        .collect();
    // "テスト" in Shift-JIS, NUL-padded to 4 words.
    let expected_sjis = vec![0x83, 0x65, 0x83, 0x58, 0x83, 0x67, 0x00, 0x00];
    assert_eq!(sjis_bytes, expected_sjis);
    assert_ne!(
        utf8_bytes, sjis_bytes,
        "UTF-8 and Shift-JIS must not silently agree on the same text"
    );
    session.disconnect().await;
    sim.stop();
}

/// A string over its span's capacity is a per-item error and NOTHING of it is
/// written (no truncated prefix), while a numeric batch-mate still lands.
#[tokio::test]
async fn an_overlong_string_writes_nothing_and_spares_its_batch_mates() {
    let sim = Simulator::start().await;
    let mut session = connect_fast(&sim).await.unwrap();

    let results = session
        .write_batch(&[
            strw("D0", sjis(2), "ABCDE"), // 5 SJIS bytes > 4-byte capacity
            num("D10", DataType::U16, 77.0),
        ])
        .await
        .expect("the batch call itself succeeds");
    assert!(
        matches!(results[0], Err(ItemError::Unsupported(_))),
        "{results:?}"
    );
    assert_eq!(results[1], Ok(()));

    assert_eq!(sim.get_word(SlmpDevice::D, 0), 0);
    assert_eq!(sim.get_word(SlmpDevice::D, 1), 0);
    assert_eq!(sim.get_word(SlmpDevice::D, 10), 77);
    assert_eq!(sim.write_command_count(), 1, "only D10 reached the wire");
    session.disconnect().await;
    sim.stop();
}

/// Numeric and string writes mix in one batch call, and numeric and string
/// reads mix in one batch call, including an exactly-adjacent numeric+string
/// pair.
#[tokio::test]
async fn mixed_numeric_and_string_batch_round_trips_in_single_calls() {
    let sim = Simulator::start().await;
    let mut session = connect_fast(&sim).await.unwrap();

    let results = session
        .write_batch(&[
            strw("D0", sjis(4), "OK"),        // D0..D3
            num("D4", DataType::U16, 1234.0), // adjacent
            bitw("M0", true),
        ])
        .await
        .unwrap();
    assert!(all_ok(&results), "{results:?}");

    let mut reader = connect_fast(&sim).await.unwrap();
    let read_back = reader
        .read_batch(&[
            spec("D0", sjis(4)),
            spec("D4", n(DataType::U16)),
            spec("M0", n(DataType::Bit)),
        ])
        .await
        .unwrap();
    assert_eq!(
        read_back,
        vec![
            Ok(Value::Str("OK".into())),
            Ok(Value::F64(1234.0)),
            Ok(Value::Bit(true)),
        ]
    );
    reader.disconnect().await;
    session.disconnect().await;
    sim.stop();
}

// --- T8, docs/tag-server-design.md §6.1: bit-in-word RMW writes --------------

/// 0x1234's bit positions: bits 2, 4, 5, 9, 12 are set; every other position
/// in 0..=15 is clear.
const SEED_WORD: u16 = 0x1234;
const SEED_SET_BITS: [u8; 5] = [2, 4, 5, 9, 12];

fn assert_word_matches_seed_except(
    sim: &Simulator,
    device: SlmpDevice,
    number: u32,
    changed_bit: u8,
    new_value: bool,
) {
    let word = sim.get_word(device, number);
    for bit in 0..=15u8 {
        let expected = if bit == changed_bit {
            new_value
        } else {
            SEED_SET_BITS.contains(&bit)
        };
        assert_eq!(
            (word >> bit) & 1 == 1,
            expected,
            "bit {bit} of {device}{number} (0x{word:04X}) should be {expected}"
        );
    }
}

#[tokio::test]
async fn single_bit_set_via_rmw_leaves_other_bits_unchanged() {
    let sim = Simulator::start().await;
    sim.set_word(SlmpDevice::D, 100, SEED_WORD);
    let mut session = connect_fast(&sim).await.expect("connect");

    // bit 0 is clear in the seed word (0x1234).
    let results = session
        .write_batch(&[bitw("D100.0", true)])
        .await
        .expect("write_batch ok");
    assert_eq!(results, vec![Ok(())]);

    assert_eq!(sim.get_word(SlmpDevice::D, 100), 0x1235);
    assert_word_matches_seed_except(&sim, SlmpDevice::D, 100, 0, true);
    session.disconnect().await;
    sim.stop();
}

#[tokio::test]
async fn single_bit_clear_via_rmw_leaves_other_bits_unchanged() {
    let sim = Simulator::start().await;
    sim.set_word(SlmpDevice::D, 100, SEED_WORD);
    let mut session = connect_fast(&sim).await.expect("connect");

    // bit 5 is set in the seed word (0x1234).
    let results = session
        .write_batch(&[bitw("D100.5", false)])
        .await
        .expect("write_batch ok");
    assert_eq!(results, vec![Ok(())]);

    assert_eq!(sim.get_word(SlmpDevice::D, 100), 0x1214);
    assert_word_matches_seed_except(&sim, SlmpDevice::D, 100, 5, false);
    session.disconnect().await;
    sim.stop();
}

/// Every bit position 0..=15, both directions (set on a zeroed word, clear on
/// an all-ones word).
#[tokio::test]
async fn all_sixteen_bit_positions_set_and_clear_correctly() {
    for bit in 0..=15u8 {
        let sim = Simulator::start().await;
        sim.set_word(SlmpDevice::D, 400, 0x0000);
        let mut session = connect_fast(&sim).await.expect("connect");
        let results = session
            .write_batch(&[bitw(&format!("D400.{bit:X}"), true)])
            .await
            .expect("write ok");
        assert_eq!(results, vec![Ok(())], "set bit {bit}");
        assert_eq!(
            sim.get_word(SlmpDevice::D, 400),
            1u16 << bit,
            "setting bit {bit} of an all-zero word"
        );
        session.disconnect().await;
        sim.stop();

        let sim = Simulator::start().await;
        sim.set_word(SlmpDevice::D, 400, 0xFFFF);
        let mut session = connect_fast(&sim).await.expect("connect");
        let results = session
            .write_batch(&[bitw(&format!("D400.{bit:X}"), false)])
            .await
            .expect("write ok");
        assert_eq!(results, vec![Ok(())], "clear bit {bit}");
        assert_eq!(
            sim.get_word(SlmpDevice::D, 400),
            !(1u16 << bit),
            "clearing bit {bit} of an all-ones word"
        );
        session.disconnect().await;
        sim.stop();
    }
}

/// §6.1's mask-composition property at the wire level: three bits of the same
/// word in one batch cost exactly one bulk write and two bulk reads (initial +
/// confirmation). Needs the write crate's simulator's read counter.
#[tokio::test]
async fn mask_composed_bits_of_one_word_cost_exactly_one_wire_write() {
    let sim = WriteSimulator::start().await;
    sim.set_word(SlmpDevice::D, 100, SEED_WORD);
    let mut session = connect_ws(&sim).await.expect("connect");

    let results = session
        .write_batch(&[
            bitw("D100.0", true),  // clear -> set
            bitw("D100.5", false), // set -> clear
            bitw("D100.F", true),  // clear -> set (hex bit 15, T20-④)
        ])
        .await
        .expect("write_batch ok");
    assert!(all_ok(&results), "{results:?}");

    assert_eq!(
        sim.write_command_count(),
        1,
        "one RMW write, however many bits"
    );
    assert_eq!(
        sim.read_command_count(),
        2,
        "one initial read + one confirmation read, however many bits"
    );

    let word = sim.get_word(SlmpDevice::D, 100);
    assert_eq!(word & 1, 1, "bit 0 set");
    assert_eq!((word >> 5) & 1, 0, "bit 5 cleared");
    assert_eq!((word >> 15) & 1, 1, "bit 15 set");
    for bit in [2u8, 4, 9, 12] {
        assert_eq!(
            (word >> bit) & 1,
            1,
            "seed bit {bit} should survive untouched"
        );
    }
    session.disconnect().await;
    sim.stop();
}

/// §6.1's confirmation-read verification, with a deterministic injected race
/// (`corrupt_after_next_write`): the RMW's own read/write succeeds, but the
/// confirmation read disagrees with what was just written, and the affected
/// item comes back as `Rejected`.
#[tokio::test]
async fn confirmation_mismatch_is_rejected_with_a_race_hint() {
    let sim = WriteSimulator::start().await;
    sim.set_word(SlmpDevice::D, 100, 0x0000);
    sim.corrupt_after_next_write(SlmpDevice::D, 100, 1 << 5);

    let mut session = connect_ws(&sim).await.expect("connect");
    let results = session
        .write_batch(&[bitw("D100.5", true)])
        .await
        .expect("the wire call itself succeeds - this is a per-item error");
    assert_eq!(results.len(), 1);
    match &results[0] {
        Err(ItemError::VerificationFailed(m)) => {
            // The full text pins the area, the bit and the race hint.
            assert_eq!(
                m,
                &PlcWriteError::BitWriteVerificationFailed {
                    area: "D100".into(),
                    bit: 5
                }
                .to_string()
            );
        }
        other => panic!("expected Err(VerificationFailed), got {other:?}"),
    }
    session.disconnect().await;
    sim.stop();
}

/// The mismatch is per-item, not per-word: a batch-mate bit in the same RMW
/// that does land correctly is still `Ok`.
#[tokio::test]
async fn confirmation_mismatch_on_one_bit_does_not_fail_its_batch_mates() {
    let sim = WriteSimulator::start().await;
    sim.set_word(SlmpDevice::D, 100, 0x0000);
    sim.corrupt_after_next_write(SlmpDevice::D, 100, 1 << 5);

    let mut session = connect_ws(&sim).await.expect("connect");
    let results = session
        .write_batch(&[bitw("D100.0", true), bitw("D100.5", true)])
        .await
        .expect("the wire call itself succeeds");
    assert_eq!(results.len(), 2);
    assert_eq!(results[0], Ok(()), "bit 0 landed correctly");
    match &results[1] {
        Err(ItemError::VerificationFailed(m)) => assert_eq!(
            m,
            &PlcWriteError::BitWriteVerificationFailed {
                area: "D100".into(),
                bit: 5
            }
            .to_string(),
            "bit 5 was corrupted after the write"
        ),
        other => panic!("expected Err(VerificationFailed) for bit 5, got {other:?}"),
    }
    session.disconnect().await;
    sim.stop();
}

/// Different words in one batch never merge into one RMW.
#[tokio::test]
async fn bit_writes_to_different_words_are_independent() {
    let sim = Simulator::start().await;
    sim.set_word(SlmpDevice::D, 100, 0x0000);
    sim.set_word(SlmpDevice::D, 101, 0xFFFF);
    let mut session = connect_fast(&sim).await.expect("connect");

    let results = session
        .write_batch(&[bitw("D100.0", true), bitw("D101.0", false)])
        .await
        .expect("write_batch ok");
    assert!(all_ok(&results), "{results:?}");

    assert_eq!(sim.get_word(SlmpDevice::D, 100), 0x0001);
    assert_eq!(sim.get_word(SlmpDevice::D, 101), 0xFFFE);
    assert_eq!(
        sim.write_command_count(),
        2,
        "two words, two independent writes"
    );
    session.disconnect().await;
    sim.stop();
}

/// An ordinary word write and a bit-in-word write coexisting in one batch
/// both land correctly.
#[tokio::test]
async fn ordinary_write_and_bit_in_word_write_coexist_in_one_batch() {
    let sim = Simulator::start().await;
    sim.set_word(SlmpDevice::D, 100, 0x0000);
    let mut session = connect_fast(&sim).await.expect("connect");

    let results = session
        .write_batch(&[num("D200", DataType::U16, 42.0), bitw("D100.5", true)])
        .await
        .expect("write_batch ok");
    assert!(all_ok(&results), "{results:?}");

    assert_eq!(sim.get_word(SlmpDevice::D, 200), 42);
    assert_eq!(sim.get_word(SlmpDevice::D, 100), 1 << 5);
    session.disconnect().await;
    sim.stop();
}
