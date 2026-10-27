//! Loopback-only Hub join for the pinned Modbus write guest. No PLC is used.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use banto_driver_api::{DataType, TagKind, TagSpec, Value, WriteItem};
use banto_hub_core::api_keys::ApiKeysService;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

struct Fixture {
    port: u16,
    frames: Arc<Mutex<Vec<(u8, u16, u16)>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let frames = Arc::new(Mutex::new(Vec::new()));
        let recorded = frames.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let frames = recorded.clone();
                tokio::spawn(async move {
                    loop {
                        let mut mbap = [0u8; 6];
                        if socket.read_exact(&mut mbap).await.is_err() {
                            break;
                        }
                        let len = usize::from(u16::from_be_bytes([mbap[4], mbap[5]]));
                        if !(2..=260).contains(&len) {
                            break;
                        }
                        let mut payload = vec![0; len];
                        if socket.read_exact(&mut payload).await.is_err() {
                            break;
                        }
                        let unit = payload[0];
                        let function = payload[1];
                        let start = if payload.len() >= 4 {
                            u16::from_be_bytes([payload[2], payload[3]])
                        } else {
                            0
                        };
                        let count = if payload.len() >= 6 {
                            u16::from_be_bytes([payload[4], payload[5]])
                        } else {
                            0
                        };
                        frames.lock().unwrap().push((function, start, count));
                        let pdu = match function {
                            1 | 2 if count == 1 => vec![function, 1, u8::from(function == 2)],
                            3 | 4 if (1..=4).contains(&count) => {
                                let mut pdu = vec![function, (count * 2) as u8];
                                for offset in start..start + count {
                                    pdu.extend_from_slice(
                                        &(if function == 4 { 42 } else { offset }).to_be_bytes(),
                                    );
                                }
                                pdu
                            }
                            5 if payload.len() == 6 => payload[1..].to_vec(),
                            16 if payload.len() >= 7 && (1..=123).contains(&count) => {
                                payload[1..6].to_vec()
                            }
                            _ => vec![function | 0x80, 1],
                        };
                        let mut response = mbap[..4].to_vec();
                        response.extend_from_slice(
                            &u16::try_from(1 + pdu.len()).unwrap().to_be_bytes(),
                        );
                        response.push(unit);
                        response.extend_from_slice(&pdu);
                        if socket.write_all(&response).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        Self { port, frames, task }
    }

    fn frames(&self) -> Vec<(u8, u16, u16)> {
        self.frames.lock().unwrap().clone()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn component() -> Vec<u8> {
    std::fs::read(
        std::env::var("BANTO_MODBUS_COMPONENT_WASM").expect("BANTO_MODBUS_COMPONENT_WASM"),
    )
    .unwrap()
}

fn manifest() -> Vec<u8> {
    std::fs::read(std::env::var("BANTO_MODBUS_MANIFEST").expect("BANTO_MODBUS_MANIFEST")).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the pinned Modbus WASM package files"]
async fn saved_settings_broker_and_rest_enforce_modbus_policy_and_audit() {
    let fixture = Fixture::start().await;
    let wasm = component();
    let manifest = manifest();
    let app =
        super::test_app_with_operational_write_manifest("t12-modbus-join", &manifest, &wasm).await;
    let saved = app
        .manager
        .plc_connections()
        .create_settings(super::ConnectionSettingsInput {
            name: "modbus-line".into(),
            protocol: "modbus-fc3-wasm".into(),
            enabled: true,
            simulation: false,
            settings: serde_json::json!({
                "host": "127.0.0.1", "port": fixture.port, "unit_id": 1,
                "probe_address": "30001", "word_order": "low_high",
                "write_mode": "enabled", "write_allowlist": "000001-000016,400001-400020",
                "modbus_operational_write_mode": "enabled",
                "modbus_operational_write_allowlist": "000001-000016,400001-400010"
            })
            .as_object()
            .unwrap()
            .clone(),
            secrets: Default::default(),
        })
        .await
        .expect("save guest and host policy");
    let group = super::CollectionGroupService::with_registry(
        app.pool.clone(),
        app.manager.registry().clone(),
    )
    .create(super::group_input("line", saved.id, 60_000))
    .await
    .unwrap();
    let mut records = Vec::new();
    for (name, address, kind, writable) in [
        ("coil", "000001", "bit", true),
        ("holding", "400001", "u32", true),
        ("outside", "400011", "u16", true),
        ("discrete", "100001", "bit", false),
        ("input", "300001", "u16", false),
    ] {
        let mut input = super::tag_input(name, group.id, address, kind);
        input.writable = writable;
        let tag = app.manager.tags().create(input).await.unwrap();
        records.push((name, tag.id));
    }
    app.manager.rebuild().await.unwrap();
    let mut status = app.sessions.status_watch(saved.id).unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        status.wait_for(|state| *state == banto_broker::BrokerConnectionStatus::Connected),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        fixture.frames().first().map(|frame| frame.0),
        Some(4),
        "connection probe must use FC4"
    );

    let driver = app.manager.registry().get("modbus-fc3-wasm").unwrap();
    let specs =
        [("100001", DataType::Bit), ("300001", DataType::U16)].map(|(address, data_type)| {
            let kind = TagKind::numeric(data_type);
            TagSpec::new(driver.base().parse_address(address, &kind).unwrap(), kind)
        });
    let handle = app.manager.write_broker_handle_peek(saved.id).unwrap();
    assert_eq!(
        handle.read(specs.to_vec()).await.unwrap(),
        vec![Ok(Value::Bit(true)), Ok(Value::F64(42.0))]
    );
    let before_denied = fixture.frames().len();
    assert!(handle
        .write(vec![WriteItem::new(specs[0].clone(), Value::Bit(true))])
        .await
        .unwrap()
        .iter()
        .all(Result::is_err));
    assert_eq!(
        fixture.frames().len(),
        before_denied,
        "FC2 is refused before send"
    );
    assert!(handle
        .write(vec![WriteItem::new(specs[1].clone(), Value::F64(1.0))])
        .await
        .unwrap()
        .iter()
        .all(Result::is_err));
    assert_eq!(
        fixture.frames().len(),
        before_denied,
        "FC4 is refused before send"
    );

    app.write_control.enable();
    let coil_path = "/api/v1/values/modbus-line.line.coil";
    let unauthorized = super::admin_write(
        &app.router,
        "POST",
        coil_path,
        &app.admin_token,
        serde_json::json!({"v": true}),
    )
    .await;
    assert_eq!(
        unauthorized.0,
        super::StatusCode::FORBIDDEN,
        "{:?}",
        unauthorized.1
    );
    assert_eq!(fixture.frames().len(), before_denied);

    let names: Vec<String> = records
        .iter()
        .map(|(name, _)| format!("modbus-line.line.{name}"))
        .collect();
    let now_ms = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let keys = ApiKeysService::new(app.pool.clone());
    let issued = keys
        .issue(
            "modbus-loopback-writer",
            names.iter().map(|name| format!("write:{name}")).collect(),
            Some(now_ms + 600_000),
        )
        .await
        .unwrap();
    for (name, value) in [
        ("coil", serde_json::json!({"v": true})),
        ("holding", serde_json::json!({"v": 305419896})),
    ] {
        let result = super::admin_write(
            &app.router,
            "POST",
            &format!("/api/v1/values/modbus-line.line.{name}"),
            &issued.key,
            value,
        )
        .await;
        assert_eq!(result.0, super::StatusCode::OK, "{name}: {:?}", result.1);
    }
    let after_allowed = fixture.frames();
    assert_eq!(after_allowed.iter().filter(|frame| frame.0 == 5).count(), 1);
    assert_eq!(
        after_allowed.iter().filter(|frame| frame.0 == 16).count(),
        1
    );
    for name in ["outside", "discrete", "input"] {
        let result = super::admin_write(
            &app.router,
            "POST",
            &format!("/api/v1/values/modbus-line.line.{name}"),
            &issued.key,
            serde_json::json!({"v": 1}),
        )
        .await;
        assert_ne!(result.0, super::StatusCode::OK, "{name}: {:?}", result.1);
    }
    assert_eq!(
        fixture
            .frames()
            .iter()
            .filter(|frame| matches!(frame.0, 5 | 16))
            .count(),
        2
    );
    let audit = super::admin_write(
        &app.router,
        "POST",
        "/api/write-audit/list",
        &app.admin_token,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(audit.0, super::StatusCode::OK, "{:?}", audit.1);
    let rows = audit.1["rows"].as_array().unwrap();
    for (_, id) in &records[..2] {
        assert!(rows
            .iter()
            .any(|row| row["tagId"].as_i64() == Some(*id) && row["result"] == "ok"));
    }
    assert!(rows
        .iter()
        .any(|row| row["tagId"].as_i64() == Some(records[2].1) && row["result"] == "failed"));
    assert!(keys.revoke(issued.id).await.unwrap().revoked_at.is_some());
    app.write_control.disable();
    drop(app);
}
