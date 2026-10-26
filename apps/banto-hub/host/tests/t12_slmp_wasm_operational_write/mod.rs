//! Normal D/u16 policy acceptance through an isolated loopback SLMP peer.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use banto_driver_api::{
    ConnectContext, DataType, DriverBase, FatalError, ItemError, Session as _, Settings, TagKind,
    TagSpec, Value, WriteItem,
};
use banto_driver_engine::{transport::TransportSpec, FrameSession};
use banto_driver_wasm::WasmDriver;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const MANIFEST: &[u8] = include_bytes!("manifest.json");
const COMPONENT_ENV: &str = "BANTO_SLMP_OPERATIONAL_WRITE_COMPONENT_WASM";
type RecordedFrames = Arc<Mutex<Vec<(u32, Vec<u16>)>>>;

#[derive(Clone, Copy)]
enum Peer {
    R,
    Q,
}

struct Fixture {
    address: std::net::SocketAddr,
    frames: RecordedFrames,
    commands: Arc<Mutex<Vec<(u16, u32, u16)>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn start(peer: Peer, count: u32, bad_ack: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let frames: RecordedFrames = Arc::new(Mutex::new(Vec::new()));
        let writes = frames.clone();
        let commands = Arc::new(Mutex::new(Vec::new()));
        let events = commands.clone();
        let task = tokio::spawn(async move {
            loop {
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
                    let start = if command == 0x0101 {
                        0
                    } else {
                        u32::from_le_bytes([
                            request[19],
                            request[20],
                            request[21],
                            if subcommand == 2 { request[22] } else { 0 },
                        ])
                    };
                    events.lock().unwrap().push((command, start, subcommand));
                    let (end_code, body): (u16, Vec<u8>) = match command {
                        0x0101 => {
                            let (name, code) = match peer {
                                Peer::R => ("R04CPU", 0x4800u16),
                                Peer::Q => ("Q25PRHCPU", 0x004cu16),
                            };
                            let mut body = vec![b' '; 18];
                            body[..name.len()].copy_from_slice(name.as_bytes());
                            body[16..18].copy_from_slice(&code.to_le_bytes());
                            (0, body)
                        }
                        0x0401 if request[if subcommand == 2 { 23 } else { 22 }] == 0xa9 => {
                            let offset = if subcommand == 2 { 25 } else { 23 };
                            let words = u16::from_le_bytes([request[offset], request[offset + 1]]);
                            match peer {
                                Peer::R => {
                                    assert_eq!((subcommand, start, words), (2, 280, 4));
                                    let mut body = count.to_le_bytes().to_vec();
                                    body.extend_from_slice(&count.to_le_bytes());
                                    (0, body)
                                }
                                Peer::Q => {
                                    assert_eq!((subcommand, start, words), (0, 302, 1));
                                    (0, u16::try_from(count).unwrap().to_le_bytes().to_vec())
                                }
                            }
                        }
                        0x0401 => {
                            let offset = if subcommand == 2 { 25 } else { 23 };
                            let words = usize::from(u16::from_le_bytes([
                                request[offset],
                                request[offset + 1],
                            ]));
                            assert_eq!(request[if subcommand == 2 { 23 } else { 22 }], 0xa8);
                            assert!((3010..=3013).contains(&start));
                            assert!(words > 0 && start as usize + words <= 3014);
                            let stored = writes.lock().unwrap().last().cloned();
                            let values = stored
                                .filter(|(first, data)| {
                                    *first <= start
                                        && start as usize + words <= *first as usize + data.len()
                                })
                                .map(|(first, data)| {
                                    data[(start - first) as usize..(start - first) as usize + words]
                                        .to_vec()
                                })
                                .unwrap_or_else(|| vec![7; words]);
                            (
                                0,
                                values
                                    .iter()
                                    .flat_map(|value| value.to_le_bytes())
                                    .collect(),
                            )
                        }
                        0x1401 => {
                            let offset = if subcommand == 2 { 25 } else { 23 };
                            assert_eq!(request[if subcommand == 2 { 23 } else { 22 }], 0xa8);
                            assert_eq!(subcommand, if matches!(peer, Peer::R) { 2 } else { 0 });
                            let words = usize::from(u16::from_le_bytes([
                                request[offset],
                                request[offset + 1],
                            ]));
                            assert_eq!(request.len(), offset + 2 + words * 2);
                            assert!((3010..=3013).contains(&start));
                            let values = request[offset + 2..]
                                .as_chunks::<2>()
                                .0
                                .iter()
                                .map(|word| u16::from_le_bytes(*word))
                                .collect();
                            writes.lock().unwrap().push((start, values));
                            (0, Vec::new())
                        }
                        _ => panic!("unexpected SLMP command {command:#x}"),
                    };
                    let mut response = vec![0xd4, 0];
                    response.extend_from_slice(&request[2..11]);
                    response
                        .extend_from_slice(&u16::try_from(body.len() + 2).unwrap().to_le_bytes());
                    response.extend_from_slice(&end_code.to_le_bytes());
                    response.extend_from_slice(&body);
                    if bad_ack && command == 0x1401 {
                        response[2] ^= 1;
                    }
                    if socket.write_all(&response).await.is_err() {
                        break;
                    }
                }
            }
        });
        Self {
            address,
            frames,
            commands,
            task,
        }
    }

    fn frames(&self) -> Vec<(u32, Vec<u16>)> {
        self.frames.lock().unwrap().clone()
    }

    fn commands(&self) -> Vec<(u16, u32, u16)> {
        self.commands.lock().unwrap().clone()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn component() -> Vec<u8> {
    std::fs::read(std::env::var(COMPONENT_ENV).expect("set component path")).unwrap()
}

fn settings(port: u16, mode: &str, allowlist: &str) -> Settings {
    Settings::from_pairs([
        ("host".to_owned(), "127.0.0.1".to_owned()),
        ("port".to_owned(), port.to_string()),
        ("d_u16_write_mode".to_owned(), mode.to_owned()),
        ("d_u16_write_allowlist".to_owned(), allowlist.to_owned()),
    ])
    .unwrap()
}

fn specs(driver: &WasmDriver) -> Vec<TagSpec> {
    let kind = TagKind::numeric(DataType::U16);
    (3010..=3013)
        .map(|index| {
            TagSpec::new(
                driver.parse_address(&format!("D{index}"), &kind).unwrap(),
                kind.clone(),
            )
        })
        .collect()
}

fn items(specs: &[TagSpec]) -> Vec<WriteItem> {
    specs
        .iter()
        .cloned()
        .zip(1..=4)
        .map(|(spec, value)| WriteItem::new(spec, Value::F64(f64::from(value))))
        .collect()
}

async fn connect(
    peer: Peer,
    count: u32,
    mode: &str,
    allowlist: &str,
    bad_ack: bool,
) -> (Fixture, Arc<WasmDriver>, FrameSession) {
    let fixture = Fixture::start(peer, count, bad_ack).await;
    let driver = WasmDriver::load_operational_write(MANIFEST, &component()).unwrap();
    let transport = TransportSpec::Tcp {
        host: "127.0.0.1".into(),
        port: fixture.address.port(),
    };
    let context = ConnectContext::new(
        "slmp-wasm-operational-write",
        Duration::from_millis(500),
        Duration::from_millis(500),
    );
    let session = FrameSession::connect(
        driver.as_ref(),
        &settings(fixture.address.port(), mode, allowlist),
        &transport,
        &context,
        None,
    )
    .await
    .unwrap();
    (fixture, driver, session)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_OPERATIONAL_WRITE_COMPONENT_WASM to the reviewed component"]
async fn q_and_r_normal_write_are_one_frame_with_per_connection_gate() {
    for peer in [Peer::R, Peer::Q] {
        let (fixture, driver, mut session) =
            connect(peer, 4000, "enabled", "D3010-D3013", false).await;
        let specs = specs(&driver);
        let mut bad = items(&specs);
        bad[2].value = Value::F64(f64::NAN);
        let mut outside = items(&specs);
        outside[3].spec = TagSpec::new(
            driver
                .parse_address("D3014", &TagKind::numeric(DataType::U16))
                .unwrap(),
            TagKind::numeric(DataType::U16),
        );
        let mut protected = items(&specs);
        protected[0].spec = TagSpec::new(
            driver
                .parse_address("D3000", &TagKind::numeric(DataType::U16))
                .unwrap(),
            TagKind::numeric(DataType::U16),
        );
        for request in [
            bad,
            outside,
            protected,
            items(&specs).into_iter().rev().collect(),
        ] {
            let result = session.write_batch(&request).await.unwrap();
            assert_eq!(result.len(), 4);
            assert!(result
                .iter()
                .all(|entry| matches!(entry, Err(ItemError::Unsupported(_)))));
        }
        assert!(fixture.frames().is_empty());
        assert_eq!(
            session.write_batch(&items(&specs)).await.unwrap(),
            vec![Ok(()); 4]
        );
        assert_eq!(fixture.frames(), vec![(3010, vec![1, 2, 3, 4])]);
        assert_eq!(
            session.read_batch(&specs).await.unwrap(),
            [1.0, 2.0, 3.0, 4.0]
                .map(|value| Ok(Value::F64(value)))
                .to_vec()
        );
        assert_eq!(
            fixture
                .commands()
                .iter()
                .filter(|(command, _, _)| *command == 0x1401)
                .count(),
            1
        );
        session.disconnect().await;
    }
}

#[test]
#[ignore = "set BANTO_SLMP_OPERATIONAL_WRITE_COMPONENT_WASM to the reviewed component"]
fn guest_and_host_reject_empty_allowlist_and_test_mode_combination() {
    let driver = WasmDriver::load_operational_write(MANIFEST, &component()).unwrap();
    assert!(!driver
        .validate_config(&settings(5000, "enabled", ""))
        .is_empty());
    assert!(!driver
        .validate_config(&settings(5000, "enabled", "D2999-D3002"))
        .is_empty());
    let mut both = settings(5000, "enabled", "D3010-D3013");
    both.insert("q25prh_d3010_3013_test_write", "enabled");
    assert!(!driver.validate_config(&both).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_OPERATIONAL_WRITE_COMPONENT_WASM to the reviewed component"]
async fn disabled_short_range_and_bad_ack_never_replay_a_write() {
    for peer in [Peer::R, Peer::Q] {
        let (fixture, driver, mut session) =
            connect(peer, 4000, "disabled", "D3010-D3013", false).await;
        assert!(session
            .write_batch(&items(&specs(&driver)))
            .await
            .unwrap()
            .iter()
            .all(Result::is_err));
        assert!(fixture.frames().is_empty());
        session.disconnect().await;

        let (fixture, driver, mut session) =
            connect(peer, 3013, "enabled", "D3010-D3013", false).await;
        assert!(session
            .write_batch(&items(&specs(&driver)))
            .await
            .unwrap()
            .iter()
            .all(Result::is_err));
        assert!(fixture.frames().is_empty());
        session.disconnect().await;

        let (fixture, driver, mut session) =
            connect(peer, 4000, "enabled", "D3010-D3013", true).await;
        assert!(matches!(
            session.write_batch(&items(&specs(&driver))).await,
            Err(FatalError::Framing(_)) | Err(FatalError::Protocol(_)) | Err(FatalError::Timeout)
        ));
        assert_eq!(fixture.frames(), vec![(3010, vec![1, 2, 3, 4])]);
        session.disconnect().await;
        assert_eq!(
            fixture
                .commands()
                .iter()
                .filter(|(command, _, _)| *command == 0x1401)
                .count(),
            1
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_OPERATIONAL_WRITE_COMPONENT_WASM to the reviewed component"]
async fn saved_hub_settings_and_broker_use_the_same_q_and_r_write_policy() {
    for peer in [Peer::R, Peer::Q] {
        let fixture = Fixture::start(peer, 4000, false).await;
        let component = component();
        let app = super::test_app_with_operational_write_manifest(
            "t12-operational-write-loopback",
            MANIFEST,
            &component,
        )
        .await;
        let saved = app
            .manager
            .plc_connections()
            .create_settings(super::ConnectionSettingsInput {
                name: "operational-write-loopback".into(),
                protocol: "slmp-wasm-operational-write".into(),
                enabled: true,
                simulation: false,
                settings: serde_json::json!({
                    "host": "127.0.0.1",
                    "port": fixture.address.port(),
                    "d_u16_write_mode": "enabled",
                    "d_u16_write_allowlist": "D3010-D3013"
                })
                .as_object()
                .unwrap()
                .clone(),
                secrets: Default::default(),
            })
            .await
            .expect("saved policy must pass guest and host validation");
        let group = super::CollectionGroupService::with_registry(
            app.pool.clone(),
            app.manager.registry().clone(),
        )
        .create(super::group_input(
            "operational-write-group",
            saved.id,
            60_000,
        ))
        .await
        .unwrap();
        for index in 3010..=3013 {
            let mut tag = super::tag_input(
                &format!("word{index}"),
                group.id,
                &format!("D{index}"),
                "u16",
            );
            tag.writable = true;
            app.manager.tags().create(tag).await.unwrap();
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
        let driver = app
            .manager
            .registry()
            .get("slmp-wasm-operational-write")
            .unwrap();
        let kind = TagKind::numeric(DataType::U16);
        let specs: Vec<_> = (3010..=3013)
            .map(|index| {
                TagSpec::new(
                    driver
                        .base()
                        .parse_address(&format!("D{index}"), &kind)
                        .unwrap(),
                    kind.clone(),
                )
            })
            .collect();
        let handle = app.manager.write_broker_handle_peek(saved.id).unwrap();
        assert_eq!(
            handle.read(specs.clone()).await.unwrap(),
            vec![Ok(Value::F64(7.0)); 4]
        );
        assert_eq!(handle.write(items(&specs)).await.unwrap(), vec![Ok(()); 4]);
        assert_eq!(
            handle.read(specs).await.unwrap(),
            [1.0, 2.0, 3.0, 4.0]
                .map(|value| Ok(Value::F64(value)))
                .to_vec()
        );
        assert_eq!(fixture.frames(), vec![(3010, vec![1, 2, 3, 4])]);
        drop(app);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_OPERATIONAL_WRITE_COMPONENT_WASM to the reviewed component"]
async fn rest_session_without_api_key_cannot_send_an_operational_write() {
    for peer in [Peer::R, Peer::Q] {
        let fixture = Fixture::start(peer, 4000, false).await;
        let component = component();
        let app = super::test_app_with_operational_write_manifest(
            "t12-operational-rest-rejection",
            MANIFEST,
            &component,
        )
        .await;
        let saved = app
            .manager
            .plc_connections()
            .create_settings(super::ConnectionSettingsInput {
                name: "normal-write-line".into(),
                protocol: "slmp-wasm-operational-write".into(),
                enabled: true,
                simulation: false,
                settings: serde_json::json!({
                    "host": "127.0.0.1",
                    "port": fixture.address.port(),
                    "d_u16_write_mode": "enabled",
                    "d_u16_write_allowlist": "D3010-D3013"
                })
                .as_object()
                .unwrap()
                .clone(),
                secrets: Default::default(),
            })
            .await
            .unwrap();
        let group = super::CollectionGroupService::with_registry(
            app.pool.clone(),
            app.manager.registry().clone(),
        )
        .create(super::group_input("normal-write-group", saved.id, 60_000))
        .await
        .unwrap();
        let mut tag = super::tag_input("word3010", group.id, "D3010", "u16");
        tag.writable = true;
        app.manager.tags().create(tag).await.unwrap();
        app.manager.rebuild().await.unwrap();
        let mut status = app.sessions.status_watch(saved.id).unwrap();
        tokio::time::timeout(
            Duration::from_secs(5),
            status.wait_for(|state| *state == banto_broker::BrokerConnectionStatus::Connected),
        )
        .await
        .unwrap()
        .unwrap();

        app.write_control.enable();
        let (status, body) = super::admin_write(
            &app.router,
            "POST",
            "/api/v1/values/normal-write-line.normal-write-group.word3010",
            &app.admin_token,
            serde_json::json!({"v": 9}),
        )
        .await;
        app.write_control.disable();
        assert_eq!(status, super::StatusCode::FORBIDDEN, "{body:?}");
        assert!(fixture.frames().is_empty());
        let (audit_status, audit) = super::admin_write(
            &app.router,
            "POST",
            "/api/write-audit/list",
            &app.admin_token,
            serde_json::json!({}),
        )
        .await;
        assert_eq!(audit_status, super::StatusCode::OK, "{audit:?}");
        assert_eq!(audit["rows"].as_array().map(Vec::len), Some(0));
        drop(app);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_OPERATIONAL_WRITE_COMPONENT_WASM to the reviewed component"]
async fn rest_scoped_write_and_denied_spans_are_audited_without_extra_frames() {
    use banto_hub_core::api_keys::ApiKeysService;

    for peer in [Peer::R, Peer::Q] {
        let fixture = Fixture::start(peer, 4000, false).await;
        let component = component();
        let app = super::test_app_with_operational_write_manifest(
            "t12-operational-rest-scoped",
            MANIFEST,
            &component,
        )
        .await;
        let saved = app
            .manager
            .plc_connections()
            .create_settings(super::ConnectionSettingsInput {
                name: "normal-write-line".into(),
                protocol: "slmp-wasm-operational-write".into(),
                enabled: true,
                simulation: false,
                settings: serde_json::json!({
                    "host": "127.0.0.1",
                    "port": fixture.address.port(),
                    "d_u16_write_mode": "enabled",
                    "d_u16_write_allowlist": "D3010-D3013"
                })
                .as_object()
                .unwrap()
                .clone(),
                secrets: Default::default(),
            })
            .await
            .unwrap();
        let group = super::CollectionGroupService::with_registry(
            app.pool.clone(),
            app.manager.registry().clone(),
        )
        .create(super::group_input("normal-write-group", saved.id, 60_000))
        .await
        .unwrap();
        let mut names = Vec::new();
        let mut ids = Vec::new();
        for index in [3010, 3011, 3012, 3013, 3000, 3014] {
            let name = format!("word{index}");
            let mut tag = super::tag_input(&name, group.id, &format!("D{index}"), "u16");
            tag.writable = true;
            ids.push(app.manager.tags().create(tag).await.unwrap().id);
            names.push(format!("normal-write-line.normal-write-group.{name}"));
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

        let api_keys = ApiKeysService::new(app.pool.clone());
        let now_ms = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .unwrap();
        // One short-lived key, limited to this TestApp's six mock tags. It is
        // revoked below and the temporary database is discarded with TestApp.
        let issued = api_keys
            .issue(
                "operational-loopback-writer",
                names.iter().map(|tag| format!("write:{tag}")).collect(),
                Some(now_ms + 600_000),
            )
            .await
            .unwrap();
        app.write_control.enable();
        let allowed_request = serde_json::json!({
            "writes": names[..4]
                .iter()
                .zip(1..=4)
                .map(|(tag, value)| serde_json::json!({"tag": tag, "v": value}))
                .collect::<Vec<_>>()
        });
        let allowed = super::admin_write(
            &app.router,
            "POST",
            "/api/v1/values/batch",
            &issued.key,
            allowed_request,
        )
        .await;
        let denied = [3000, 3014]
            .into_iter()
            .enumerate()
            .map(|(offset, index)| (index, names[offset + 4].clone()))
            .collect::<Vec<_>>();
        let mut denied_responses = Vec::new();
        for (index, name) in denied {
            denied_responses.push((
                index,
                super::admin_write(
                    &app.router,
                    "POST",
                    &format!("/api/v1/values/{name}"),
                    &issued.key,
                    serde_json::json!({"v": 9}),
                )
                .await,
            ));
        }
        let frames = fixture.frames();
        let audit = super::admin_write(
            &app.router,
            "POST",
            "/api/write-audit/list",
            &app.admin_token,
            serde_json::json!({}),
        )
        .await;
        let revoked = api_keys.revoke(issued.id).await;
        app.write_control.disable();
        drop(app);

        assert!(revoked.unwrap().revoked_at.is_some());
        assert_eq!(allowed.0, super::StatusCode::OK, "{:?}", allowed.1);
        let writes = allowed.1["writes"].as_array().unwrap();
        assert_eq!(writes.len(), 4);
        assert!(writes.iter().all(|result| result["ok"] == true));
        assert_eq!(frames, vec![(3010, vec![1, 2, 3, 4])]);
        for (index, (status, body)) in denied_responses {
            assert_ne!(status, super::StatusCode::OK, "D{index}: {body:?}");
        }
        assert_eq!(audit.0, super::StatusCode::OK, "{:?}", audit.1);
        let rows = audit.1["rows"].as_array().unwrap();
        for id in &ids[..4] {
            assert!(rows
                .iter()
                .any(|row| { row["tagId"].as_i64() == Some(*id) && row["result"] == "ok" }));
        }
        for id in &ids[4..] {
            assert!(rows
                .iter()
                .any(|row| { row["tagId"].as_i64() == Some(*id) && row["result"] == "failed" }));
        }
    }
}
