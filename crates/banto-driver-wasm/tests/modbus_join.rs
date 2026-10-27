//! Offline join test for the pinned banto-driver-wasm Modbus component.
//! Build the component separately and set BANTO_MODBUS_COMPONENT_WASM and
//! BANTO_MODBUS_MANIFEST to the pinned package files. All
//! responses are synthesized here; no socket or PLC is opened.

use banto_driver_api::{
    DataType, DriverBase, FatalError, FrameConnection, FrameDriver, ItemError, RawValue, Settings,
    TagKind, TagSpec, Value, WriteItem, WriteProgress,
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
