//! Explicit optional-export compatibility check with two pinned components.

use banto_driver_api::{FrameDriver, Settings};
use banto_driver_wasm::WasmDriver;

const MANIFEST: &[u8] = br#"{"manifestVersion":1,"id":"slmp-wasm-operational-write","version":"0.1.0","hubApi":"1","writeApi":"1","settingsVersion":1,"displayName":"SLMP diagnostic compatibility fixture","transports":["tcp"],"capabilities":["read","write"]}"#;

fn connection(component: &[u8]) -> Box<dyn banto_driver_api::FrameConnection> {
    let driver = WasmDriver::load_operational_write(MANIFEST, component).unwrap();
    let settings = Settings::from_pairs([
        ("host", "127.0.0.1"),
        ("port", "3100"),
        ("d_u16_write_mode", "disabled"),
        ("d_u16_write_allowlist", ""),
    ])
    .unwrap();
    driver.create_connection(&settings).unwrap()
}

#[test]
#[ignore = "set BANTO_SLMP_LEGACY_COMPONENT_WASM and BANTO_SLMP_DIAGNOSTICS_COMPONENT_WASM"]
fn optional_export_preserves_the_legacy_world_and_binds_the_new_world() {
    let old = std::fs::read(std::env::var("BANTO_SLMP_LEGACY_COMPONENT_WASM").unwrap()).unwrap();
    let new =
        std::fs::read(std::env::var("BANTO_SLMP_DIAGNOSTICS_COMPONENT_WASM").unwrap()).unwrap();
    let mut legacy = connection(&old);
    assert!(!legacy.supports_diagnostics());
    assert!(legacy.diagnose_next(&[], None).is_err());
    let current = connection(&new);
    assert!(current.supports_diagnostics());
}
