//! Isolated write-v1 acceptance. The socket is loopback only; no PLC is used.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use banto_driver_api::{
    ConnectContext, DataType, DriverBase, FatalError, FrameDriver, ItemError, Session as _,
    Settings, TagKind, TagSpec, Value as DriverValue, WriteItem,
};
use banto_driver_engine::{transport::TransportSpec, FrameSession};
use banto_driver_wasm::WasmDriver;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const MANIFEST: &[u8] = include_bytes!("manifest.json");

#[derive(Clone, Copy)]
enum Peer {
    R,
    RShort,
    RSdRefused,
    Q,
    DeviceRefusal,
    BadAck,
    Timeout,
}

struct Fixture {
    address: SocketAddr,
    commands: Arc<Mutex<Vec<(u16, u32)>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn start(peer: Peer) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let commands = Arc::new(Mutex::new(Vec::new()));
        let events = commands.clone();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            loop {
                let mut head = [0; 13];
                if socket.read_exact(&mut head).await.is_err() {
                    break;
                }
                let length = usize::from(u16::from_le_bytes([head[11], head[12]]));
                let mut request = head.to_vec();
                request.resize(13 + length, 0);
                if socket.read_exact(&mut request[13..]).await.is_err() {
                    break;
                }
                let command = u16::from_le_bytes([request[15], request[16]]);
                let subcommand = u16::from_le_bytes([request[17], request[18]]);
                let start = if matches!(command, 0x0401 | 0x1401) {
                    u32::from_le_bytes([
                        request[19],
                        request[20],
                        request[21],
                        if subcommand == 2 { request[22] } else { 0 },
                    ])
                } else {
                    0
                };
                events.lock().unwrap().push((command, start));
                let (end_code, body): (u16, Vec<u8>) = match command {
                    0x0101 => {
                        let (name, code) = if matches!(peer, Peer::Q) {
                            ("Q02UCPU", 0x0263u16)
                        } else {
                            ("R04CPU", 0x4800u16)
                        };
                        let mut body = vec![b' '; 18];
                        body[..name.len()].copy_from_slice(name.as_bytes());
                        body[16..18].copy_from_slice(&code.to_le_bytes());
                        (0, body)
                    }
                    0x0401 if request[if subcommand == 2 { 23 } else { 22 }] == 0xa9 => {
                        if matches!(peer, Peer::RSdRefused) {
                            (0xc05b, vec![])
                        } else {
                            let limit = if matches!(peer, Peer::RShort) {
                                100
                            } else {
                                4000
                            };
                            let body = if matches!(peer, Peer::Q) {
                                (limit as u16).to_le_bytes().to_vec()
                            } else {
                                (limit as u32).to_le_bytes().to_vec()
                            };
                            (0, body)
                        }
                    }
                    0x0401 => {
                        let count_offset = if subcommand == 2 { 25 } else { 24 };
                        let count =
                            u16::from_le_bytes([request[count_offset], request[count_offset + 1]]);
                        assert!((1..=960).contains(&count));
                        (0, 7u16.to_le_bytes().repeat(usize::from(count)))
                    }
                    0x1401 => {
                        assert_eq!(start, 3010);
                        assert_eq!(u16::from_le_bytes([request[17], request[18]]), 2);
                        assert_eq!(request[23], 0xa8);
                        assert_eq!(u16::from_le_bytes([request[25], request[26]]), 1);
                        assert_eq!(u16::from_le_bytes([request[27], request[28]]), 9);
                        if matches!(peer, Peer::Timeout) {
                            continue;
                        }
                        (
                            if matches!(peer, Peer::DeviceRefusal) {
                                0xc05b
                            } else {
                                0
                            },
                            vec![],
                        )
                    }
                    _ => panic!("unexpected command {command:#x}"),
                };
                let mut response = vec![0xd4, 0];
                response.extend_from_slice(&request[2..11]);
                response.extend_from_slice(&u16::try_from(body.len() + 2).unwrap().to_le_bytes());
                response.extend_from_slice(&end_code.to_le_bytes());
                response.extend_from_slice(&body);
                if matches!(peer, Peer::BadAck) && command == 0x1401 {
                    response[2] ^= 1;
                }
                if socket.write_all(&response).await.is_err() {
                    break;
                }
            }
        });
        Self {
            address,
            commands,
            task,
        }
    }

    fn commands(&self) -> Vec<(u16, u32)> {
        self.commands.lock().unwrap().clone()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn connected_session(peer: Peer) -> (Fixture, Arc<WasmDriver>, FrameSession) {
    let fixture = Fixture::start(peer).await;
    let component =
        std::fs::read(std::env::var("BANTO_SLMP_WRITE_COMPONENT_WASM").unwrap()).unwrap();
    let driver = WasmDriver::load_controlled_write_fixture(MANIFEST, &component).unwrap();
    let settings = Settings::from_pairs([
        ("host".to_owned(), "127.0.0.1".to_owned()),
        ("port".to_owned(), fixture.address.port().to_string()),
    ])
    .unwrap();
    let transport = TransportSpec::Tcp {
        host: "127.0.0.1".into(),
        port: fixture.address.port(),
    };
    let context = ConnectContext::new(
        "slmp-write-fixture",
        Duration::from_millis(300),
        Duration::from_millis(300),
    );
    let session = FrameSession::connect(driver.as_ref(), &settings, &transport, &context, None)
        .await
        .unwrap();
    (fixture, driver, session)
}

fn item(driver: &WasmDriver, address: &str, value: f64) -> WriteItem {
    let kind = TagKind::numeric(DataType::U16);
    let parsed = driver.parse_address(address, &kind).unwrap();
    WriteItem::new(TagSpec::new(parsed, kind), DriverValue::F64(value))
}

#[test]
#[ignore = "set BANTO_SLMP_WRITE_COMPONENT_WASM to the pinned write-v1 component"]
fn controlled_loader_refuses_non_loopback_settings_before_connect() {
    let component =
        std::fs::read(std::env::var("BANTO_SLMP_WRITE_COMPONENT_WASM").unwrap()).unwrap();
    let driver = WasmDriver::load_controlled_write_fixture(MANIFEST, &component).unwrap();
    let settings = Settings::from_pairs([("host", "192.168.11.200"), ("port", "3109")]).unwrap();
    assert!(matches!(
        driver.create_connection(&settings),
        Err(FatalError::InvalidConfig(_))
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_WRITE_COMPONENT_WASM to the pinned write-v1 component"]
async fn read_manifest_never_enables_a_write_export() {
    let fixture = Fixture::start(Peer::R).await;
    let component =
        std::fs::read(std::env::var("BANTO_SLMP_WRITE_COMPONENT_WASM").unwrap()).unwrap();
    let mut manifest: serde_json::Value = serde_json::from_slice(MANIFEST).unwrap();
    manifest["capabilities"] = serde_json::json!(["read"]);
    manifest.as_object_mut().unwrap().remove("writeApi");
    let driver = WasmDriver::load(&serde_json::to_vec(&manifest).unwrap(), &component).unwrap();
    assert!(!driver.descriptor().capabilities.write);
    let settings = Settings::from_pairs([
        ("host".to_owned(), "127.0.0.1".to_owned()),
        ("port".to_owned(), fixture.address.port().to_string()),
    ])
    .unwrap();
    let transport = TransportSpec::Tcp {
        host: "127.0.0.1".into(),
        port: fixture.address.port(),
    };
    let context = ConnectContext::new(
        "slmp-read-manifest",
        Duration::from_millis(300),
        Duration::from_millis(300),
    );
    let mut session = FrameSession::connect(driver.as_ref(), &settings, &transport, &context, None)
        .await
        .unwrap();
    let result = session
        .write_batch(&[item(&driver, "D3010", 9.0)])
        .await
        .unwrap();
    assert!(matches!(&result[0], Err(ItemError::Unsupported(_))));
    assert!(!fixture
        .commands()
        .iter()
        .any(|(command, _)| *command == 0x1401));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_WRITE_COMPONENT_WASM to the pinned write-v1 component"]
async fn controlled_write_v1_rejects_before_send_and_checks_ack() {
    let (fixture, driver, mut session) = connected_session(Peer::R).await;
    let valid = item(&driver, "D3010", 9.0);
    let invalid = item(&driver, "D3000", 9.0);
    let batch = session
        .write_batch(&[valid.clone(), invalid])
        .await
        .unwrap();
    assert_eq!(batch.len(), 2);
    assert!(batch
        .iter()
        .all(|result| matches!(result, Err(ItemError::Unsupported(_)))));
    assert!(!fixture
        .commands()
        .iter()
        .any(|(command, _)| *command == 0x1401));
    assert_eq!(session.write_batch(&[valid]).await.unwrap(), vec![Ok(())]);
    assert_eq!(
        fixture
            .commands()
            .iter()
            .filter(|(command, _)| *command == 0x1401)
            .count(),
        1
    );

    for peer in [Peer::RShort, Peer::RSdRefused, Peer::Q] {
        let (fixture, driver, mut session) = connected_session(peer).await;
        let result = session
            .write_batch(&[item(&driver, "D3010", 9.0)])
            .await
            .unwrap();
        assert!(matches!(&result[0], Err(ItemError::Rejected(_))));
        assert!(!fixture
            .commands()
            .iter()
            .any(|(command, _)| *command == 0x1401));
    }

    let (fixture, driver, mut session) = connected_session(Peer::DeviceRefusal).await;
    let result = session
        .write_batch(&[item(&driver, "D3010", 9.0)])
        .await
        .unwrap();
    assert!(matches!(&result[0], Err(ItemError::Device(error)) if error.code == 0xc05b));
    assert_eq!(
        fixture
            .commands()
            .iter()
            .filter(|(command, _)| *command == 0x1401)
            .count(),
        1
    );

    for peer in [Peer::BadAck, Peer::Timeout] {
        let (fixture, driver, mut session) = connected_session(peer).await;
        let result = session.write_batch(&[item(&driver, "D3010", 9.0)]).await;
        assert!(matches!(
            result,
            Err(FatalError::Framing(_)) | Err(FatalError::Timeout) | Err(FatalError::Protocol(_))
        ));
        assert_eq!(
            fixture
                .commands()
                .iter()
                .filter(|(command, _)| *command == 0x1401)
                .count(),
            1
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_WRITE_COMPONENT_WASM to the pinned write-v1 component"]
async fn broker_serializes_read_write_read_on_one_controlled_session() {
    let fixture = Fixture::start(Peer::R).await;
    let component =
        std::fs::read(std::env::var("BANTO_SLMP_WRITE_COMPONENT_WASM").unwrap()).unwrap();
    let app = super::test_app_with_controlled_write_manifest(
        "t12-slmp-wasm-write-broker",
        MANIFEST,
        &component,
    )
    .await;
    let settings = serde_json::json!({"host":"127.0.0.1","port":fixture.address.port()});
    let saved = app
        .manager
        .plc_connections()
        .create_settings(super::ConnectionSettingsInput {
            name: "slmp-write-fixture-line".into(),
            protocol: "slmp-write-fixture".into(),
            enabled: true,
            simulation: false,
            settings: settings.as_object().unwrap().clone(),
            secrets: Default::default(),
        })
        .await
        .unwrap();
    let group = super::CollectionGroupService::with_registry(
        app.pool.clone(),
        app.manager.registry().clone(),
    )
    .create(super::group_input("slmp-write-group", saved.id, 10_000))
    .await
    .unwrap();
    let mut tag = super::tag_input("word", group.id, "D3010", "u16");
    tag.writable = true;
    app.manager.tags().create(tag).await.unwrap();
    app.manager.rebuild().await.unwrap();
    let mut status = app.sessions.status_watch(saved.id).unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        status.wait_for(|status| *status == banto_broker::BrokerConnectionStatus::Connected),
    )
    .await
    .unwrap()
    .unwrap();
    let driver = app.manager.registry().get("slmp-write-fixture").unwrap();
    let kind = TagKind::numeric(DataType::U16);
    let spec = TagSpec::new(driver.base().parse_address("D3010", &kind).unwrap(), kind);
    let handle = app.manager.write_broker_handle_peek(saved.id).unwrap();
    assert_eq!(handle.read(vec![spec.clone()]).await.unwrap().len(), 1);
    assert_eq!(
        handle
            .write(vec![WriteItem::new(spec.clone(), DriverValue::F64(9.0))])
            .await
            .unwrap(),
        vec![Ok(())]
    );
    assert_eq!(handle.read(vec![spec]).await.unwrap().len(), 1);
    let commands = fixture.commands();
    let write_index = commands
        .iter()
        .position(|(command, _)| *command == 0x1401)
        .unwrap();
    assert!(commands[..write_index]
        .iter()
        .any(|(command, start)| *command == 0x0401 && *start == 3010));
    assert!(commands[write_index + 1..]
        .iter()
        .any(|(command, start)| *command == 0x0401 && *start == 3010));
    assert_eq!(
        commands
            .iter()
            .filter(|(command, _)| *command == 0x1401)
            .count(),
        1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_WRITE_COMPONENT_WASM to the pinned write-v1 component"]
async fn hub_write_gates_and_audit_still_wrap_the_controlled_guest() {
    let fixture = Fixture::start(Peer::R).await;
    let component =
        std::fs::read(std::env::var("BANTO_SLMP_WRITE_COMPONENT_WASM").unwrap()).unwrap();
    let app = super::test_app_with_controlled_write_manifest(
        "t12-slmp-wasm-write-hub-gates",
        MANIFEST,
        &component,
    )
    .await;
    let settings = serde_json::json!({"host":"127.0.0.1","port":fixture.address.port()});
    let saved = app
        .manager
        .plc_connections()
        .create_settings(super::ConnectionSettingsInput {
            name: "slmp-write-fixture-line".into(),
            protocol: "slmp-write-fixture".into(),
            enabled: true,
            simulation: false,
            settings: settings.as_object().unwrap().clone(),
            secrets: Default::default(),
        })
        .await
        .unwrap();
    let group = super::CollectionGroupService::with_registry(
        app.pool.clone(),
        app.manager.registry().clone(),
    )
    .create(super::group_input("slmp-write-group", saved.id, 10_000))
    .await
    .unwrap();
    let mut tag = super::tag_input("word", group.id, "D3010", "u16");
    tag.writable = true;
    let tag_id = app.manager.tags().create(tag).await.unwrap().id;
    let readonly = super::tag_input("readonly", group.id, "D3011", "u16");
    app.manager.tags().create(readonly).await.unwrap();
    let mut disabled = super::tag_input("disabled", group.id, "D3012", "u16");
    disabled.writable = true;
    disabled.enabled = false;
    app.manager.tags().create(disabled).await.unwrap();
    app.manager.rebuild().await.unwrap();
    let mut status = app.sessions.status_watch(saved.id).unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        status.wait_for(|status| *status == banto_broker::BrokerConnectionStatus::Connected),
    )
    .await
    .unwrap()
    .unwrap();

    let path = "/api/v1/values/slmp-write-fixture-line.slmp-write-group.word";
    let (status, key_json) = super::admin_write(
        &app.router,
        "POST",
        "/api/api-keys",
        &app.admin_token,
        serde_json::json!({"name":"fixture-writer","scopes":[
            "write:slmp-write-fixture-line.slmp-write-group.word",
            "write:slmp-write-fixture-line.slmp-write-group.readonly",
            "write:slmp-write-fixture-line.slmp-write-group.disabled"
        ]}),
    )
    .await;
    assert_eq!(status, super::StatusCode::CREATED, "{key_json:?}");
    let key = key_json["key"].as_str().unwrap();
    let (status, body) =
        super::admin_write(&app.router, "POST", path, key, serde_json::json!({"v":9})).await;
    assert_eq!(status, super::StatusCode::SERVICE_UNAVAILABLE, "{body:?}");
    assert!(!fixture
        .commands()
        .iter()
        .any(|(command, _)| *command == 0x1401));

    app.write_control.enable();
    for (name, expected_status, error) in [
        ("readonly", super::StatusCode::FORBIDDEN, "not_writable"),
        ("disabled", super::StatusCode::CONFLICT, "tag_disabled"),
    ] {
        let (status, body) = super::admin_write(
            &app.router,
            "POST",
            &format!("/api/v1/values/slmp-write-fixture-line.slmp-write-group.{name}"),
            key,
            serde_json::json!({"v":9}),
        )
        .await;
        assert_eq!(status, expected_status, "{body:?}");
        assert_eq!(body["error"], error);
        assert!(!fixture
            .commands()
            .iter()
            .any(|(command, _)| *command == 0x1401));
    }
    let (status, no_scope_json) = super::admin_write(
        &app.router,
        "POST",
        "/api/api-keys",
        &app.admin_token,
        serde_json::json!({"name":"fixture-reader","scopes":["read:slmp-write-fixture-line.slmp-write-group.word"]}),
    )
    .await;
    assert_eq!(status, super::StatusCode::CREATED, "{no_scope_json:?}");
    let no_scope_key = no_scope_json["key"].as_str().unwrap();
    let (status, body) = super::admin_write(
        &app.router,
        "POST",
        path,
        no_scope_key,
        serde_json::json!({"v":9}),
    )
    .await;
    assert_eq!(status, super::StatusCode::FORBIDDEN, "{body:?}");
    assert!(!fixture
        .commands()
        .iter()
        .any(|(command, _)| *command == 0x1401));

    let (status, body) =
        super::admin_write(&app.router, "POST", path, key, serde_json::json!({"v":9})).await;
    assert_eq!(status, super::StatusCode::OK, "{body:?}");
    assert_eq!(
        fixture
            .commands()
            .iter()
            .filter(|(command, _)| *command == 0x1401)
            .count(),
        1
    );
    let (status, audit) = super::admin_write(
        &app.router,
        "POST",
        "/api/write-audit/list",
        &app.admin_token,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, super::StatusCode::OK, "{audit:?}");
    assert!(audit["rows"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| { row["tagId"].as_i64() == Some(tag_id) && row["result"] == "ok" }));
}
