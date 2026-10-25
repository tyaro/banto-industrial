//! Run with `BANTO_COMPONENT_WASM=<built canonical component> cargo test -- --ignored`.

use banto_driver_api::{
    DataType, DriverBase, FrameDriver, FrameRule, RawValue, Settings, TagKind, TagSpec,
};
use banto_driver_wasm::WasmDriver;

#[test]
#[ignore = "requires a separately built banto-driver-wasm component"]
fn canonical_component_runs_through_frame_adapter() {
    let bytes = std::fs::read(std::env::var("BANTO_COMPONENT_WASM").expect("BANTO_COMPONENT_WASM"))
        .expect("component bytes");
    let driver = WasmDriver::load(include_bytes!("manifest.json"), &bytes).expect("load component");
    assert_eq!(driver.descriptor().settings_version, 1);
    assert_eq!(driver.descriptor().id.as_str(), "modbus-fc3-wasm");
    assert_eq!(
        driver.probe(),
        Some(("40001".into(), TagKind::numeric(DataType::U16)))
    );
    let address = driver
        .parse_address("40001", &TagKind::numeric(DataType::U16))
        .unwrap();
    assert_eq!(address.canonical, "400001");
    let settings = Settings::from_pairs([("host", "127.0.0.1"), ("port", "502")]).unwrap();
    assert!(driver.validate_config(&settings).is_empty());
    let mut conn = driver.create_connection(&settings).unwrap();
    assert!(conn.handshake_next(None).unwrap().is_none());
    assert_eq!(conn.inter_frame_gap(), std::time::Duration::ZERO);
    let specs = [TagSpec::new(address, TagKind::numeric(DataType::U16))];
    let planned = conn.plan_read(&specs).unwrap();
    assert_eq!(planned.len(), 1);
    assert_eq!(planned[0].covers, [0]);
    assert_eq!(planned[0].step.max_len, 260);
    assert!(matches!(
        planned[0].step.expect,
        FrameRule::LengthPrefixed(_)
    ));
    let request = &planned[0].step.send;
    assert_eq!(&request[2..], &[0, 0, 0, 6, 1, 3, 0, 0, 0, 1]);
    let mut response = request[..2].to_vec();
    response.extend_from_slice(&[0, 0, 0, 5, 1, 3, 2, 0, 42]);
    assert_eq!(
        conn.decode_read(&planned[0], &specs, &response).unwrap(),
        [Ok(RawValue::Uint(42))]
    );
    assert!(driver.diagnostics().snapshot().is_empty());
}
