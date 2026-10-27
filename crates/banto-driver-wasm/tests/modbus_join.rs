//! Offline join test for the pinned banto-driver-wasm Modbus component.
//! Build the component separately and set BANTO_MODBUS_COMPONENT_WASM and
//! BANTO_MODBUS_MANIFEST to the pinned package files. All
//! responses are synthesized here; no socket or PLC is opened.

use banto_driver_api::{
    DataType, DriverBase, FrameDriver, RawValue, Settings, TagKind, TagSpec, Value, WriteItem,
    WriteProgress,
};
use banto_driver_wasm::WasmDriver;

const READ_MANIFEST: &[u8] = br#"{"manifestVersion":1,"id":"modbus-fc3-wasm","version":"0.4.0","hubApi":"1","settingsVersion":1,"displayName":"Modbus simulated join","transports":["tcp"],"capabilities":["read"]}"#;

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
