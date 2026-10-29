//! Offline join test for the pinned banto-driver-wasm Modbus component.
//! Build the component separately and set BANTO_MODBUS_COMPONENT_WASM and
//! BANTO_MODBUS_MANIFEST to the pinned package files. All
//! responses are synthesized here; no socket or PLC is opened.

use banto_driver_api::{
    DataType, DriverBase, FatalError, FrameConnection, FrameDriver, ItemError, RawValue, Settings,
    StringEncoding, TagKind, TagSpec, Value, WriteItem, WriteProgress,
};
use banto_driver_wasm::WasmDriver;

const READ_MANIFEST: &[u8] = br#"{"manifestVersion":1,"id":"modbus-fc3-wasm","version":"0.5.0","hubApi":"1","settingsVersion":1,"displayName":"Modbus simulated join","transports":["tcp"],"capabilities":["read"]}"#;

fn write_manifest() -> Vec<u8> {
    std::fs::read(std::env::var("BANTO_MODBUS_MANIFEST").expect("BANTO_MODBUS_MANIFEST"))
        .expect("published package manifest bytes")
}

fn component() -> Vec<u8> {
    std::fs::read(
        std::env::var("BANTO_MODBUS_COMPONENT_WASM").expect("BANTO_MODBUS_COMPONENT_WASM"),
    )
    .expect("component bytes")
}

fn settings(probe: &str, host_enabled: bool) -> Settings {
    Settings::from_pairs([
        ("host", "127.0.0.1"),
        ("port", "502"),
        ("unit_id", "1"),
        ("probe_address", probe),
        ("write_mode", "enabled"),
        ("write_allowlist", "000001-000016,400001-400010"),
        (
            "modbus_operational_write_mode",
            if host_enabled { "enabled" } else { "disabled" },
        ),
        (
            "modbus_operational_write_allowlist",
            "000001-000016,400001-400010",
        ),
    ])
    .unwrap()
}

fn read_response(request: &[u8], body: &[u8]) -> Vec<u8> {
    let mut response = request[..2].to_vec();
    response.extend_from_slice(&[0, 0]);
    response.extend_from_slice(&(2 + body.len() as u16).to_be_bytes());
    response.push(request[6]);
    response.push(request[7]);
    response.extend_from_slice(body);
    response
}

fn spec(driver: &WasmDriver, address: &str, kind: DataType) -> TagSpec {
    let kind = TagKind::numeric(kind);
    TagSpec::new(driver.parse_address(address, &kind).unwrap(), kind)
}

#[test]
#[ignore = "requires BANTO_MODBUS_COMPONENT_WASM from the pinned guest"]
fn read_world_keeps_old_loader_and_handshake_checks_fc4() {
    let driver = WasmDriver::load(READ_MANIFEST, &component()).unwrap();
    assert!(!driver.descriptor().capabilities.write);
    assert!(driver.probe().is_none());
    let mut conn = driver.create_connection(&settings("30001", false)).unwrap();
    let step = conn.handshake_next(None).unwrap().unwrap();
    assert_eq!(step.send[7], 4);
    assert!(conn
        .handshake_next(Some(&read_response(&step.send, &[2, 0, 42])))
        .unwrap()
        .is_none());
    let input = spec(&driver, "100001", DataType::Bit);
    assert!(!input.address.writable_area);
    let planned = conn.plan_read(std::slice::from_ref(&input)).unwrap();
    assert_eq!(planned[0].step.send[7], 2);
    assert_eq!(
        conn.decode_read(
            &planned[0],
            &[input],
            &read_response(&planned[0].step.send, &[1, 1])
        )
        .unwrap(),
        [Ok(RawValue::Bool(true))]
    );
    let numeric = spec(&driver, "300001", DataType::U32);
    let planned = conn.plan_read(std::slice::from_ref(&numeric)).unwrap();
    assert_eq!(planned[0].step.send[7], 4);
    assert_eq!(
        conn.decode_read(
            &planned[0],
            &[numeric],
            &read_response(&planned[0].step.send, &[4, 0x34, 0x12, 0x78, 0x56])
        )
        .unwrap(),
        [Ok(RawValue::Uint(0x7856_3412))]
    );
    assert!(driver.diagnostics().snapshot().is_empty());
}

#[test]
#[ignore = "requires BANTO_MODBUS_COMPONENT_WASM from the pinned guest"]
fn handshake_rejects_exception_and_wrong_transaction() {
    let driver = WasmDriver::load_operational_write(&write_manifest(), &component()).unwrap();
    for mut response in [
        vec![0, 0, 0, 0, 0, 3, 1, 0x84, 2],
        vec![0, 1, 0, 0, 0, 5, 1, 4, 2, 0, 1],
    ] {
        let mut conn = driver.create_connection(&settings("30001", false)).unwrap();
        let step = conn.handshake_next(None).unwrap().unwrap();
        if response[7] == 0x84 {
            response[..2].copy_from_slice(&step.send[..2]);
        }
        assert!(conn.handshake_next(Some(&response)).is_err());
    }
}

#[test]
#[ignore = "requires BANTO_MODBUS_COMPONENT_WASM from the pinned guest"]
fn write_world_gates_fc5_fc16_before_send_and_checks_ack() {
    let driver = WasmDriver::load_operational_write(&write_manifest(), &component()).unwrap();
    assert!(driver.descriptor().capabilities.write);
    let coil = WriteItem::new(spec(&driver, "000001", DataType::Bit), Value::Bit(true));
    let holding = WriteItem::new(
        spec(&driver, "400001", DataType::U32),
        Value::F64(0x1234_5678 as f64),
    );

    let mut disabled = driver.create_connection(&settings("40001", false)).unwrap();
    assert!(
        matches!(disabled.write_next(std::slice::from_ref(&coil), None).unwrap(), WriteProgress::Done(result) if result.iter().all(Result::is_err))
    );
    let mut conn = driver.create_connection(&settings("40001", true)).unwrap();
    let probe = conn.handshake_next(None).unwrap().unwrap();
    assert_eq!(probe.send[7], 3);
    conn.handshake_next(Some(&read_response(&probe.send, &[2, 0, 1])))
        .unwrap();

    for (address, kind, value) in [
        ("100001", DataType::Bit, Value::Bit(true)),
        ("300001", DataType::U16, Value::F64(1.0)),
        ("400011", DataType::U16, Value::F64(1.0)),
    ] {
        let bad = WriteItem::new(spec(&driver, address, kind), value);
        assert!(
            matches!(conn.write_next(&[bad], None).unwrap(), WriteProgress::Done(result) if result.iter().all(Result::is_err))
        );
    }
    let mut malformed = holding.clone();
    malformed.spec.address.word_count = 1;
    assert!(
        matches!(conn.write_next(&[malformed], None).unwrap(), WriteProgress::Done(result) if result.iter().all(Result::is_err))
    );

    let coil_step = match conn.write_next(std::slice::from_ref(&coil), None).unwrap() {
        WriteProgress::Send(plan) => plan.step,
        other => panic!("expected FC5: {other:?}"),
    };
    assert_eq!(coil_step.send[7], 5);
    assert!(
        matches!(conn.write_next(&[coil], Some(&coil_step.send)).unwrap(), WriteProgress::Done(result) if result == [Ok(())])
    );

    let holding_step = match conn
        .write_next(std::slice::from_ref(&holding), None)
        .unwrap()
    {
        WriteProgress::Send(plan) => plan.step,
        other => panic!("expected FC16: {other:?}"),
    };
    assert_eq!(holding_step.send[7], 16);
    let mut bad_ack = holding_step.send[..12].to_vec();
    bad_ack[4..6].copy_from_slice(&6_u16.to_be_bytes());
    bad_ack[10..12].copy_from_slice(&1_u16.to_be_bytes());
    assert!(
        conn.write_next(&[holding], Some(&bad_ack)).is_err()
            || !driver.diagnostics().snapshot().is_empty()
    );
}

#[test]
#[ignore = "requires the pinned Modbus WASM package files"]
fn register_bits_round_trip_and_holding_bit_writes_use_fc22() {
    let driver = WasmDriver::load_operational_write(&write_manifest(), &component()).unwrap();
    let mut conn = driver.create_connection(&settings("30001", true)).unwrap();
    for (address, canonical, bit, function) in [
        ("40001.0", "400001.0", 0, 3),
        ("30001.15", "300001.15", 15, 4),
    ] {
        let tag = spec(&driver, address, DataType::Bit);
        assert_eq!(tag.address.canonical, canonical);
        assert_eq!(tag.address.bit, Some(bit));
        for (word, expected) in [(0_u16, false), (1_u16 << bit, true)] {
            let planned = conn.plan_read(std::slice::from_ref(&tag)).unwrap();
            assert_eq!(planned[0].step.send[7], function);
            let [hi, lo] = word.to_be_bytes();
            assert_eq!(
                conn.decode_read(
                    &planned[0],
                    std::slice::from_ref(&tag),
                    &read_response(&planned[0].step.send, &[2, hi, lo])
                )
                .unwrap(),
                [Ok(RawValue::Bool(expected))]
            );
        }
        let item = WriteItem::new(tag, Value::Bit(true));
        match conn.write_next(std::slice::from_ref(&item), None).unwrap() {
            // Until #44 (the 02a92645 pin) every register-bit write was
            // refused here before send. A holding bit now becomes one FC22.
            WriteProgress::Send(plan) if function == 3 => {
                assert_eq!(plan.step.send[6..], [1, 22, 0, 0, 0xff, 0xfe, 0, 1]);
                assert!(matches!(
                    conn.write_next(&[item], Some(&plan.step.send)).unwrap(),
                    WriteProgress::Done(results) if results == [Ok(())]
                ));
            }
            // An input-register bit stays refused before send.
            WriteProgress::Done(results) if function == 4 => {
                assert!(results.iter().all(Result::is_err), "{results:?}");
            }
            other => panic!("unexpected register-bit write for {address}: {other:?}"),
        }
    }
}

fn bit_settings(
    guest_mode: &str,
    guest_allow: &str,
    host_mode: &str,
    host_allow: &str,
) -> Settings {
    Settings::from_pairs([
        ("host", "127.0.0.1"),
        ("port", "502"),
        ("unit_id", "1"),
        ("probe_address", "40001"),
        ("write_mode", guest_mode),
        ("write_allowlist", guest_allow),
        ("modbus_operational_write_mode", host_mode),
        ("modbus_operational_write_allowlist", host_allow),
    ])
    .unwrap()
}

fn connected(driver: &WasmDriver, settings: &Settings) -> Box<dyn FrameConnection> {
    let mut conn = driver.create_connection(settings).unwrap();
    let probe = conn.handshake_next(None).unwrap().unwrap();
    assert!(conn
        .handshake_next(Some(&read_response(&probe.send, &[2, 0, 0])))
        .unwrap()
        .is_none());
    conn
}

fn bit_write(driver: &WasmDriver, address: &str, value: Value) -> WriteItem {
    WriteItem::new(spec(driver, address, DataType::Bit), value)
}

#[test]
#[ignore = "requires the pinned Modbus WASM package files"]
fn fc22_sets_and_clears_one_holding_bit_with_exact_masks() {
    let driver = WasmDriver::load_operational_write(&write_manifest(), &component()).unwrap();
    let mut conn = connected(
        &driver,
        &bit_settings("enabled", "400001-400010", "enabled", "400001-400010"),
    );
    for (address, value, offset, and_mask, or_mask) in [
        ("40002.3", true, 1_u16, 0xfff7_u16, 0x0008_u16),
        ("40002.3", false, 1, 0xfff7, 0x0000),
        ("40010.15", true, 9, 0x7fff, 0x8000),
        ("40001.0", false, 0, 0xfffe, 0x0000),
    ] {
        let item = bit_write(&driver, address, Value::Bit(value));
        let plan = match conn.write_next(std::slice::from_ref(&item), None).unwrap() {
            WriteProgress::Send(plan) => plan,
            other => panic!("expected FC22 for {address}={value}: {other:?}"),
        };
        assert_eq!(plan.covers, [0]);
        let mut expected = plan.step.send[..2].to_vec();
        expected.extend_from_slice(&[0, 0, 0, 8, 1, 22]);
        expected.extend_from_slice(&offset.to_be_bytes());
        expected.extend_from_slice(&and_mask.to_be_bytes());
        expected.extend_from_slice(&or_mask.to_be_bytes());
        assert_eq!(plan.step.send, expected, "{address}={value}");
        // The acknowledgement echoes the whole request.
        assert!(matches!(
            conn.write_next(&[item], Some(&plan.step.send)).unwrap(),
            WriteProgress::Done(results) if results == [Ok(())]
        ));
    }
}

#[test]
#[ignore = "requires the pinned Modbus WASM package files"]
fn fc22_exception_is_per_item_and_a_mask_mismatch_poisons_writes() {
    let driver = WasmDriver::load_operational_write(&write_manifest(), &component()).unwrap();
    let mut conn = connected(
        &driver,
        &bit_settings("enabled", "400001-400010", "enabled", "400001-400010"),
    );
    let item = bit_write(&driver, "40002.3", Value::Bit(true));
    let send = |conn: &mut Box<dyn FrameConnection>| match conn
        .write_next(std::slice::from_ref(&item), None)
        .unwrap()
    {
        WriteProgress::Send(plan) => plan.step.send,
        other => panic!("expected FC22: {other:?}"),
    };

    // A device without FC22: exception code 1 is an item-level device error
    // and the connection keeps working.
    let request = send(&mut conn);
    let mut exception = request[..4].to_vec();
    exception.extend_from_slice(&[0, 3, 1, 0x96, 1]);
    match conn
        .write_next(std::slice::from_ref(&item), Some(&exception))
        .unwrap()
    {
        WriteProgress::Done(results) => match results.as_slice() {
            [Err(ItemError::Device(error))] => assert_eq!(error.code, 1, "{error:?}"),
            other => panic!("expected a device error: {other:?}"),
        },
        other => panic!("expected Done: {other:?}"),
    }
    let request = send(&mut conn);
    assert!(matches!(
        conn.write_next(std::slice::from_ref(&item), Some(&request)).unwrap(),
        WriteProgress::Done(results) if results == [Ok(())]
    ));

    // An acknowledgement whose OR mask differs leaves the outcome unknown:
    // fatal, and no further write is planned on this connection.
    let request = send(&mut conn);
    let mut mismatch = request.clone();
    mismatch[13] = 0;
    assert!(matches!(
        conn.write_next(std::slice::from_ref(&item), Some(&mismatch)),
        Err(FatalError::Protocol(_))
    ));
    assert!(matches!(
        conn.write_next(std::slice::from_ref(&item), None),
        Err(FatalError::Protocol(_))
    ));
}

#[test]
#[ignore = "requires the pinned Modbus WASM package files"]
fn register_bit_writes_are_refused_before_send_by_the_layer_that_owns_each_rule() {
    let driver = WasmDriver::load_operational_write(&write_manifest(), &component()).unwrap();
    let refused = |settings: Settings, items: Vec<WriteItem>| -> Vec<ItemError> {
        let mut conn = connected(&driver, &settings);
        match conn.write_next(&items, None).unwrap() {
            WriteProgress::Done(results) => {
                assert_eq!(results.len(), items.len());
                results.into_iter().map(Result::unwrap_err).collect()
            }
            other => panic!("a write frame was planned for {items:?}: {other:?}"),
        }
    };
    let both = || bit_settings("enabled", "400001-400010", "enabled", "400001-400010");
    let bit = |address: &str| bit_write(&driver, address, Value::Bit(true));
    let numeric = WriteItem::new(spec(&driver, "40003", DataType::U16), Value::F64(1.0));

    // The guest's own rules, reached because the host permission allows the
    // word: guest write_mode disabled, and a word outside the guest list.
    for (settings, item) in [
        (
            bit_settings("disabled", "", "enabled", "400001-400010"),
            bit("40002.3"),
        ),
        (
            bit_settings("enabled", "400001-400005", "enabled", "400001-400010"),
            bit("40008.0"),
        ),
    ] {
        let errors = refused(settings, vec![item]);
        assert!(
            matches!(errors.as_slice(), [ItemError::Rejected(_)]),
            "guest refusal expected: {errors:?}"
        );
    }

    // Rules the host gate enforces first (the guest has the same rules; its
    // own refusal is covered by the guest's unit tests): input-register bit,
    // mixed with numeric, two bits in one request, non-Bool value, a word
    // outside the host list, and host permission disabled.
    for (settings, items) in [
        (both(), vec![bit("30001.15")]),
        (both(), vec![bit("40001.0"), numeric.clone()]),
        (both(), vec![bit("40001.0"), bit("40001.1")]),
        (both(), vec![bit_write(&driver, "40001.0", Value::F64(1.0))]),
        (
            bit_settings("enabled", "400001-400010", "enabled", "400001-400005"),
            vec![bit("40008.0")],
        ),
        (
            bit_settings("enabled", "400001-400010", "disabled", ""),
            vec![bit("40002.3")],
        ),
    ] {
        let errors = refused(settings, items);
        assert!(
            errors
                .iter()
                .all(|error| matches!(error, ItemError::Unsupported(_))),
            "host refusal expected: {errors:?}"
        );
    }
}

/// Guest and host policy settings plus extra guest settings
/// (for example `register_write`).
fn policy_settings(guest_allow: &str, host_allow: &str, extra: &[(&str, &str)]) -> Settings {
    let mut pairs = vec![
        ("host", "127.0.0.1"),
        ("port", "502"),
        ("unit_id", "1"),
        ("probe_address", "40001"),
        ("write_mode", "enabled"),
        ("write_allowlist", guest_allow),
        ("modbus_operational_write_mode", "enabled"),
        ("modbus_operational_write_allowlist", host_allow),
    ];
    pairs.extend_from_slice(extra);
    Settings::from_pairs(pairs).unwrap()
}

fn coils(driver: &WasmDriver, first: u32, values: &[bool]) -> Vec<WriteItem> {
    values
        .iter()
        .zip(first..)
        .map(|(value, number)| bit_write(driver, &format!("{number:05}"), Value::Bit(*value)))
        .collect()
}

fn refused_before_send(conn: &mut Box<dyn FrameConnection>, items: &[WriteItem]) -> Vec<ItemError> {
    match conn.write_next(items, None).unwrap() {
        WriteProgress::Done(results) => {
            assert_eq!(results.len(), items.len());
            results.into_iter().map(Result::unwrap_err).collect()
        }
        other => panic!("a write frame was planned for {items:?}: {other:?}"),
    }
}

/// #915: a contiguous coil run inside one host interval passes the host
/// gate and the guest sends it as one FC15 frame, packed LSB-first.
#[test]
#[ignore = "requires the pinned Modbus WASM package files"]
fn fc15_coil_run_inside_one_host_interval_is_one_frame_with_per_item_exceptions() {
    let driver = WasmDriver::load_operational_write(&write_manifest(), &component()).unwrap();
    // The guest allows 00001-00016 as one interval; the host splits it in two.
    let mut conn = connected(
        &driver,
        &policy_settings("000001-000016", "000001-000008,000009-000016", &[]),
    );
    let values = [true, false, true, true, false, false, true, true];
    let items = coils(&driver, 1, &values);
    let plan = match conn.write_next(&items, None).unwrap() {
        WriteProgress::Send(plan) => plan,
        other => panic!("expected FC15: {other:?}"),
    };
    assert_eq!(plan.covers, (0..8).collect::<Vec<u32>>());
    let mut expected = plan.step.send[..2].to_vec();
    // Byte count 1; bits 0..7 = 1,0,1,1,0,0,1,1 LSB-first = 0xCD.
    expected.extend_from_slice(&[0, 0, 0, 8, 1, 15, 0, 0, 0, 8, 1, 0xcd]);
    assert_eq!(plan.step.send, expected);
    let mut ack = plan.step.send[..12].to_vec();
    ack[4..6].copy_from_slice(&6_u16.to_be_bytes());
    assert!(matches!(
        conn.write_next(&items, Some(&ack)).unwrap(),
        WriteProgress::Done(results) if results == vec![Ok(()); 8]
    ));

    // Eight coils filling the second host interval.
    let items = coils(&driver, 9, &[true; 8]);
    let plan = match conn.write_next(&items, None).unwrap() {
        WriteProgress::Send(plan) => plan,
        other => panic!("expected FC15: {other:?}"),
    };
    assert_eq!(plan.step.send[7..], [15, 0, 8, 0, 8, 1, 0xff]);

    // An exception is an item-level device error for every coil of the run,
    // and the connection keeps working.
    let mut exception = plan.step.send[..4].to_vec();
    exception.extend_from_slice(&[0, 3, 1, 0x8f, 2]);
    match conn.write_next(&items, Some(&exception)).unwrap() {
        WriteProgress::Done(results) => {
            assert_eq!(results.len(), items.len());
            for result in results {
                match result {
                    Err(ItemError::Device(error)) => assert_eq!(error.code, 2, "{error:?}"),
                    other => panic!("expected a device error: {other:?}"),
                }
            }
        }
        other => panic!("expected Done: {other:?}"),
    }
    let plan = match conn.write_next(&items, None).unwrap() {
        WriteProgress::Send(plan) => plan,
        other => panic!("expected FC15 again: {other:?}"),
    };
    let mut ack = plan.step.send[..12].to_vec();
    ack[4..6].copy_from_slice(&6_u16.to_be_bytes());
    assert!(matches!(
        conn.write_next(&items, Some(&ack)).unwrap(),
        WriteProgress::Done(results) if results == vec![Ok(()); 8]
    ));

    // Refused by the host gate before send: a run straddling the two host
    // intervals (the guest alone would allow it), a gap, a descending pair,
    // and a coil mixed with a holding register.
    let holding = WriteItem::new(spec(&driver, "40001", DataType::U16), Value::F64(1.0));
    let mut mixed = coils(&driver, 1, &[true]);
    mixed.push(holding);
    for items in [
        coils(&driver, 7, &[true, false, true, false]),
        vec![
            bit_write(&driver, "00001", Value::Bit(true)),
            bit_write(&driver, "00003", Value::Bit(true)),
        ],
        vec![
            bit_write(&driver, "00002", Value::Bit(true)),
            bit_write(&driver, "00001", Value::Bit(true)),
        ],
        mixed,
    ] {
        let errors = refused_before_send(&mut conn, &items);
        assert!(
            errors
                .iter()
                .all(|error| matches!(error, ItemError::Unsupported(_))),
            "host refusal expected for {items:?}: {errors:?}"
        );
    }

    // Inside the host interval but outside the guest's list: the guest
    // refuses before send.
    let mut narrow = connected(
        &driver,
        &policy_settings("000001-000004", "000001-000016", &[]),
    );
    let errors = refused_before_send(&mut narrow, &coils(&driver, 3, &[true, true, true]));
    assert!(
        errors
            .iter()
            .all(|error| matches!(error, ItemError::Rejected(_))),
        "guest refusal expected: {errors:?}"
    );
}

/// #915: `register_write=fc6` sends one i16/u16 holding write as FC6; a
/// write needing more than one word is refused by the guest before send.
#[test]
#[ignore = "requires the pinned Modbus WASM package files"]
fn register_write_fc6_sends_one_word_and_refuses_wider_writes_before_send() {
    let driver = WasmDriver::load_operational_write(&write_manifest(), &component()).unwrap();
    let both = "000001-000016,400001-400010";
    let mut conn = connected(
        &driver,
        &policy_settings(both, both, &[("register_write", "fc6")]),
    );
    for (address, kind, value, offset, word) in [
        ("40003", DataType::U16, 0x1234 as f64, 2_u16, 0x1234_u16),
        ("40010", DataType::I16, -2.0, 9, 0xfffe),
    ] {
        let item = WriteItem::new(spec(&driver, address, kind), Value::F64(value));
        let plan = match conn.write_next(std::slice::from_ref(&item), None).unwrap() {
            WriteProgress::Send(plan) => plan,
            other => panic!("expected FC6 for {address}: {other:?}"),
        };
        let mut expected = plan.step.send[..2].to_vec();
        expected.extend_from_slice(&[0, 0, 0, 6, 1, 6]);
        expected.extend_from_slice(&offset.to_be_bytes());
        expected.extend_from_slice(&word.to_be_bytes());
        assert_eq!(plan.step.send, expected, "{address}");
        // The acknowledgement echoes the address and value.
        assert!(matches!(
            conn.write_next(&[item], Some(&plan.step.send)).unwrap(),
            WriteProgress::Done(results) if results == [Ok(())]
        ));
    }

    // An exception is an item-level device error.
    let item = WriteItem::new(spec(&driver, "40003", DataType::U16), Value::F64(7.0));
    let plan = match conn.write_next(std::slice::from_ref(&item), None).unwrap() {
        WriteProgress::Send(plan) => plan,
        other => panic!("expected FC6: {other:?}"),
    };
    let mut exception = plan.step.send[..4].to_vec();
    exception.extend_from_slice(&[0, 3, 1, 0x86, 2]);
    match conn
        .write_next(std::slice::from_ref(&item), Some(&exception))
        .unwrap()
    {
        WriteProgress::Done(results) => match results.as_slice() {
            [Err(ItemError::Device(error))] => assert_eq!(error.code, 2, "{error:?}"),
            other => panic!("expected a device error: {other:?}"),
        },
        other => panic!("expected Done: {other:?}"),
    }

    // Wider than one word: passes the host gate (inside its allowlist) and is
    // refused by the guest before send.
    for items in [
        vec![WriteItem::new(
            spec(&driver, "40001", DataType::U32),
            Value::F64(1.0),
        )],
        vec![
            WriteItem::new(spec(&driver, "40001", DataType::U16), Value::F64(1.0)),
            WriteItem::new(spec(&driver, "40002", DataType::U16), Value::F64(2.0)),
        ],
    ] {
        let errors = refused_before_send(&mut conn, &items);
        assert!(
            errors
                .iter()
                .all(|error| matches!(error, ItemError::Rejected(_))),
            "guest refusal expected for {items:?}: {errors:?}"
        );
    }
    // Outside the host allowlist: the host refuses first.
    let errors = refused_before_send(
        &mut conn,
        &[WriteItem::new(
            spec(&driver, "40011", DataType::U16),
            Value::F64(1.0),
        )],
    );
    assert!(
        matches!(errors.as_slice(), [ItemError::Unsupported(_)]),
        "{errors:?}"
    );

    // register_write does not affect coils: one coil stays FC5, a run FC15.
    for (values, function) in [(&[true][..], 5_u8), (&[true, false][..], 15)] {
        let items = coils(&driver, 1, values);
        let plan = match conn.write_next(&items, None).unwrap() {
            WriteProgress::Send(plan) => plan,
            other => panic!("expected FC{function}: {other:?}"),
        };
        assert_eq!(plan.step.send[7], function);
        let mut ack = plan.step.send[..12].to_vec();
        ack[4..6].copy_from_slice(&6_u16.to_be_bytes());
        assert!(matches!(
            conn.write_next(&items, Some(&ack)).unwrap(),
            WriteProgress::Done(results) if results.iter().all(Result::is_ok)
        ));
    }
}

fn string_spec(driver: &WasmDriver, address: &str, words: u16) -> TagSpec {
    let kind = TagKind::string(words, StringEncoding::Utf8);
    TagSpec::new(driver.parse_address(address, &kind).unwrap(), kind)
}

/// #63 (C1): a holding or input register string is one FC3 / FC4 over its
/// whole span, and the result is the register data unchanged (each register
/// high byte first, which is character order), exactly `2 × words` bytes and
/// not cut at NUL. 123 words is accepted; 124, a coil and a register bit are
/// refused at registration.
#[test]
#[ignore = "requires the pinned Modbus WASM package files"]
fn holding_and_input_register_strings_read_high_byte_first_with_exact_length() {
    let driver = WasmDriver::load_operational_write(&write_manifest(), &component()).unwrap();
    let mut conn = connected(
        &driver,
        &policy_settings("400101-400300", "400101-400300", &[]),
    );
    for (address, canonical, function, offset, words) in [
        ("40101", "400101", 3_u8, 100_u16, 3_u16),
        ("30001", "300001", 4, 0, 2),
        ("40201", "400201", 3, 200, 123),
    ] {
        let spec = string_spec(&driver, address, words);
        assert_eq!(spec.address.canonical, canonical);
        assert_eq!(spec.address.word_count, words);
        // Characters 'A'.. with a NUL in the middle and at the end.
        let data: Vec<u8> = (0..usize::from(words) * 2)
            .map(|at| match at {
                1 => 0,
                at if at + 1 == usize::from(words) * 2 => 0,
                at => b'A' + u8::try_from(at % 26).unwrap(),
            })
            .collect();
        let planned = conn.plan_read(std::slice::from_ref(&spec)).unwrap();
        assert_eq!(planned.len(), 1, "{address}");
        let request = &planned[0].step.send;
        assert_eq!(request[7], function, "{address}");
        assert_eq!(
            request[8..12],
            [offset.to_be_bytes(), words.to_be_bytes()].concat()
        );
        let mut body = vec![u8::try_from(data.len()).unwrap()];
        body.extend_from_slice(&data);
        assert_eq!(
            conn.decode_read(
                &planned[0],
                std::slice::from_ref(&spec),
                &read_response(request, &body)
            )
            .unwrap(),
            [Ok(RawValue::Bytes(data))],
            "{address}"
        );
    }
    let kind = |words| TagKind::string(words, StringEncoding::Utf8);
    for (address, words) in [
        ("40101", 124),
        ("40101", 0),
        ("00001", 1),
        ("10001", 1),
        ("40001.3", 1),
    ] {
        assert!(
            driver.parse_address(address, &kind(words)).is_err(),
            "{address} x {words} accepted"
        );
    }
}

/// #63 (C1) and #934: a holding register string write is one FC16 over the
/// whole span with the Hub's encoded, 0x00-padded bytes, high byte first per
/// register; 123 words is one FC16. With `register_write=fc6` a one-word
/// string is one FC6 and a wider one is refused by the guest before send. The
/// host gate refuses first a span reaching outside its allowlist, an input
/// register, a string with another item, and text longer than the span.
#[test]
#[ignore = "requires the pinned Modbus WASM package files"]
fn holding_string_write_is_one_fc16_of_the_padded_span_and_alone() {
    let driver = WasmDriver::load_operational_write(&write_manifest(), &component()).unwrap();
    let text = |address: &str, words: u16, value: &str| {
        WriteItem::new(
            string_spec(&driver, address, words),
            Value::Str(value.into()),
        )
    };
    let mut conn = connected(
        &driver,
        &policy_settings("400101-400400", "400101-400400", &[]),
    );
    let mut max = vec![b'A'];
    max.resize(246, 0);
    for (item, offset, words, data) in [
        (
            text("40101", 4, "ABC"),
            100_u16,
            4_u16,
            b"ABC\0\0\0\0\0".to_vec(),
        ),
        (text("40201", 123, "A"), 200, 123, max),
    ] {
        let plan = match conn.write_next(std::slice::from_ref(&item), None).unwrap() {
            WriteProgress::Send(plan) => plan,
            other => panic!("expected FC16: {other:?}"),
        };
        let mut expected = plan.step.send[..2].to_vec();
        expected.extend_from_slice(&[0, 0]);
        expected.extend_from_slice(&(7 + u16::try_from(data.len()).unwrap()).to_be_bytes());
        expected.extend_from_slice(&[1, 16]);
        expected.extend_from_slice(&offset.to_be_bytes());
        expected.extend_from_slice(&words.to_be_bytes());
        expected.push(u8::try_from(data.len()).unwrap());
        expected.extend_from_slice(&data);
        assert_eq!(plan.step.send, expected, "{words} words");
        let mut ack = plan.step.send[..12].to_vec();
        ack[4..6].copy_from_slice(&6_u16.to_be_bytes());
        assert!(matches!(
            conn.write_next(&[item], Some(&ack)).unwrap(),
            WriteProgress::Done(results) if results == [Ok(())]
        ));
    }

    // Refused by the host gate before send.
    for items in [
        vec![text("40399", 4, "AB")],
        vec![text("40101", 4, "ABCDEFGHI")],
        vec![
            text("40101", 2, "AB"),
            WriteItem::new(spec(&driver, "40110", DataType::U16), Value::F64(1.0)),
        ],
        vec![text("30001", 2, "AB")],
    ] {
        let errors = refused_before_send(&mut conn, &items);
        assert!(
            errors
                .iter()
                .all(|error| matches!(error, ItemError::Unsupported(_))),
            "host refusal expected for {items:?}: {errors:?}"
        );
    }

    let mut fc6 = connected(
        &driver,
        &policy_settings(
            "400101-400300",
            "400101-400300",
            &[("register_write", "fc6")],
        ),
    );
    let item = text("40103", 1, "AB");
    let plan = match fc6.write_next(std::slice::from_ref(&item), None).unwrap() {
        WriteProgress::Send(plan) => plan,
        other => panic!("expected FC6: {other:?}"),
    };
    let mut expected = plan.step.send[..2].to_vec();
    expected.extend_from_slice(&[0, 0, 0, 6, 1, 6, 0, 102, b'A', b'B']);
    assert_eq!(plan.step.send, expected);
    assert!(matches!(
        fc6.write_next(&[item], Some(&plan.step.send)).unwrap(),
        WriteProgress::Done(results) if results == [Ok(())]
    ));
    let errors = refused_before_send(&mut fc6, &[text("40101", 2, "ABC")]);
    assert!(
        matches!(errors.as_slice(), [ItemError::Rejected(_)]),
        "guest refusal expected: {errors:?}"
    );
}
