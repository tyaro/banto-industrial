//! Exercise the test-only real component's non-native address through the Hub.

use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_DEVICE_COMPONENT_WASM to the built test component"]
async fn saves_collects_reads_now_and_reuses_broker() {
    let component =
        std::fs::read(std::env::var("BANTO_DEVICE_COMPONENT_WASM").expect("device component path"))
            .expect("device component bytes");
    let app = test_app_with_manifest(
        "t12-wasm-device-address",
        include_bytes!(
            "../../../../../crates/banto-driver-wasm/tests/fixtures/device-address/manifest.json"
        ),
        &component,
    )
    .await;
    for native in ["modbus-tcp", "slmp"] {
        assert!(app
            .manager
            .registry()
            .get(native)
            .unwrap()
            .base()
            .parse_address("D:0", &TagKind::numeric(banto_driver_api::DataType::U16))
            .is_err());
    }
    let sim = ModbusSimulator::start().await;
    sim.set_holding_registers(0, &[42]);
    let settings = json!({ "host": "127.0.0.1", "port": sim.addr.port() });
    let payload = json!({
        "protocol": "device-address-wasm", "simulation": false, "settings": settings
    });
    let (status, body) = admin_write(
        &app.router,
        "POST",
        "/api/plc-connections/test",
        &app.admin_token,
        payload.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], true, "{body:?}");
    assert_eq!(sim.connection_count(), 1);

    let saved = app
        .manager
        .plc_connections()
        .create_settings(ConnectionSettingsInput {
            name: "device-line".into(),
            protocol: "device-address-wasm".into(),
            enabled: true,
            simulation: false,
            settings: settings.as_object().unwrap().clone(),
            secrets: Default::default(),
        })
        .await
        .expect("save test connection");
    let group =
        CollectionGroupService::with_registry(app.pool.clone(), app.manager.registry().clone())
            .create(group_input("device-fast", saved.id, 100))
            .await
            .expect("create group");
    let tag = app
        .manager
        .tags()
        .create(tag_input("device-value", group.id, "D:000", "u16"))
        .await
        .expect("save non-native address");
    assert_eq!(tag.address, "D:0");
    app.commit_and_start().await;
    let mut watch = app.sessions.status_watch(saved.id).expect("broker session");
    tokio::time::timeout(
        Duration::from_secs(5),
        watch.wait_for(|status| *status == banto_broker::BrokerConnectionStatus::Connected),
    )
    .await
    .expect("device connection timeout")
    .expect("device connection is connected");
    assert_eq!(sim.connection_count(), 2);
    let key = format!("tag:{}", tag.id);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let value = app
                .manager
                .current_values()
                .and_then(|values| values.get(&key))
                .and_then(|value| value.value);
            if value == Some(42.0) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("device collection value");
    let path = "/api/v1/values/device-line.device-fast.device-value";
    let (status, body) = admin_get(&app.router, path, &app.admin_token).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["v"], 42.0, "{body:?}");
    assert_eq!(body["q"], "good", "{body:?}");
    let (status, body) =
        admin_get(&app.router, &format!("{path}/read-now"), &app.admin_token).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["v"], 42.0, "{body:?}");
    assert_eq!(body["q"], "good", "{body:?}");

    let mut matching = payload;
    matching["connectionId"] = json!(saved.id);
    let (_, body) = admin_write(
        &app.router,
        "POST",
        "/api/plc-connections/test",
        &app.admin_token,
        matching,
    )
    .await;
    assert_eq!(body["ok"], true, "{body:?}");
    assert_eq!(
        sim.connection_count(),
        2,
        "connection test must reuse broker socket"
    );
    sim.stop();
}
