//! Isolated write-v1 acceptance. The socket is loopback only; no PLC is used.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use banto_driver_api::{
    ConnectContext, DataType, DriverBase, FatalError, FrameDriver, ItemError, Session as _,
    Settings, TagKind, TagSpec, Value as DriverValue, WriteItem,
};
use banto_driver_engine::{transport::TransportSpec, EngineEvent, EngineEvents, FrameSession};
use banto_driver_wasm::WasmDriver;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const MANIFEST: &[u8] = include_bytes!("manifest.json");
type RecordedWriteFrames = Arc<Mutex<Vec<(u32, Vec<u16>)>>>;

#[derive(Clone, Copy)]
enum Peer {
    R,
    RShort,
    RSdRefused,
    Q,
    DeviceRefusal,
    BadAck,
    Timeout,
    TimeoutOnce,
}

struct Fixture {
    address: SocketAddr,
    commands: Arc<Mutex<Vec<(u16, u32)>>>,
    write_values: Arc<Mutex<Vec<u16>>>,
    write_frames: RecordedWriteFrames,
    d_reads: Arc<Mutex<Vec<(u32, u16)>>>,
    accepted: tokio::sync::watch::Receiver<usize>,
    closed: tokio::sync::watch::Receiver<usize>,
    task: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn start(peer: Peer) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let commands = Arc::new(Mutex::new(Vec::new()));
        let events = commands.clone();
        let write_values = Arc::new(Mutex::new(Vec::new()));
        let writes = write_values.clone();
        let write_frames: RecordedWriteFrames = Arc::new(Mutex::new(Vec::new()));
        let frames = write_frames.clone();
        let d_reads = Arc::new(Mutex::new(Vec::new()));
        let reads = d_reads.clone();
        let (accepted_tx, accepted) = tokio::sync::watch::channel(0usize);
        let (closed_tx, closed) = tokio::sync::watch::channel(0usize);
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                accepted_tx.send_modify(|count| *count += 1);
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
                            let count_offset = if subcommand == 2 { 25 } else { 23 };
                            let count = u16::from_le_bytes([
                                request[count_offset],
                                request[count_offset + 1],
                            ]);
                            if matches!(peer, Peer::RSdRefused) {
                                (0xc05b, vec![])
                            } else {
                                let limit = if matches!(peer, Peer::RShort) {
                                    100
                                } else {
                                    4000
                                };
                                let body = if matches!(peer, Peer::Q) && count == 1 {
                                    (limit as u16).to_le_bytes().to_vec()
                                } else if count == 2 {
                                    (limit as u32).to_le_bytes().to_vec()
                                } else if subcommand == 2 && count == 4 {
                                    let mut words = (limit as u32).to_le_bytes().to_vec();
                                    words.extend_from_slice(&(limit as u32).to_le_bytes());
                                    words
                                } else {
                                    panic!("unexpected SD count {count}")
                                };
                                (0, body)
                            }
                        }
                        0x0401 => {
                            let count_offset = if subcommand == 2 { 25 } else { 23 };
                            let count = u16::from_le_bytes([
                                request[count_offset],
                                request[count_offset + 1],
                            ]);
                            assert!((1..=960).contains(&count));
                            reads.lock().unwrap().push((start, count));
                            let stored = frames.lock().unwrap().last().cloned();
                            let data = stored
                                .filter(|(first, values)| {
                                    start >= *first
                                        && u64::from(start) + u64::from(count)
                                            <= u64::from(*first) + values.len() as u64
                                        && values.len() == 4
                                })
                                .map(|(first, values)| {
                                    let offset = (start - first) as usize;
                                    values[offset..offset + usize::from(count)].to_vec()
                                })
                                .unwrap_or_else(|| vec![7; usize::from(count)]);
                            (
                                0,
                                data.iter().flat_map(|value| value.to_le_bytes()).collect(),
                            )
                        }
                        0x1401 => {
                            assert!(matches!(start, 3010 | 3012));
                            assert_eq!(u16::from_le_bytes([request[17], request[18]]), 2);
                            assert_eq!(request[23], 0xa8);
                            let count = usize::from(u16::from_le_bytes([request[25], request[26]]));
                            assert!(count == 1 || (start == 3010 && count == 4));
                            assert_eq!(request.len(), 27 + count * 2);
                            let values: Vec<_> = request[27..]
                                .as_chunks::<2>()
                                .0
                                .iter()
                                .map(|word| u16::from_le_bytes(*word))
                                .collect();
                            assert!(values.iter().all(|value| matches!(*value, 9 | 10)));
                            writes.lock().unwrap().extend(&values);
                            let write_count = {
                                let mut sent = frames.lock().unwrap();
                                sent.push((start, values));
                                sent.len()
                            };
                            if matches!(peer, Peer::Timeout)
                                || (matches!(peer, Peer::TimeoutOnce) && write_count == 1)
                            {
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
                    response
                        .extend_from_slice(&u16::try_from(body.len() + 2).unwrap().to_le_bytes());
                    response.extend_from_slice(&end_code.to_le_bytes());
                    response.extend_from_slice(&body);
                    if matches!(peer, Peer::BadAck) && command == 0x1401 {
                        response[2] ^= 1;
                    }
                    if socket.write_all(&response).await.is_err() {
                        break;
                    }
                }
                closed_tx.send_modify(|count| *count += 1);
            }
        });
        Self {
            address,
            commands,
            write_values,
            write_frames,
            d_reads,
            accepted,
            closed,
            task,
        }
    }

    fn commands(&self) -> Vec<(u16, u32)> {
        self.commands.lock().unwrap().clone()
    }

    fn write_values(&self) -> Vec<u16> {
        self.write_values.lock().unwrap().clone()
    }

    fn write_frames(&self) -> Vec<(u32, Vec<u16>)> {
        self.write_frames.lock().unwrap().clone()
    }

    fn d_reads(&self) -> Vec<(u32, u16)> {
        self.d_reads.lock().unwrap().clone()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct WriteSteps(Mutex<Vec<Vec<u32>>>);

impl EngineEvents for WriteSteps {
    fn record(&self, event: EngineEvent) {
        if let EngineEvent::WriteStep(covers) = event {
            self.0.lock().unwrap().push(covers);
        }
    }
}

async fn connected_session_with(
    peer: Peer,
    component_env: &str,
    events: Option<Arc<dyn EngineEvents>>,
) -> (Fixture, Arc<WasmDriver>, FrameSession) {
    let fixture = Fixture::start(peer).await;
    let component = std::fs::read(std::env::var(component_env).unwrap()).unwrap();
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
    let session = FrameSession::connect(driver.as_ref(), &settings, &transport, &context, events)
        .await
        .unwrap_or_else(|error| {
            panic!(
                "SLMP fixture connect failed: {error:?}; commands={:?}",
                fixture.commands()
            )
        });
    (fixture, driver, session)
}

async fn connected_session(peer: Peer) -> (Fixture, Arc<WasmDriver>, FrameSession) {
    connected_session_with(peer, "BANTO_SLMP_WRITE_COMPONENT_WASM", None).await
}

fn item(driver: &WasmDriver, address: &str, value: f64) -> WriteItem {
    let kind = TagKind::numeric(DataType::U16);
    let parsed = driver.parse_address(address, &kind).unwrap();
    WriteItem::new(TagSpec::new(parsed, kind), DriverValue::F64(value))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_WRITE_COMPONENT_WASM to the pinned write-v1 component"]
async fn q_and_r_two_word_reads_use_their_respective_count_offsets() {
    for peer in [Peer::Q, Peer::R] {
        let (fixture, driver, mut session) = connected_session(peer).await;
        let kind = TagKind::numeric(DataType::U16);
        let specs = ["D3010", "D3011"].map(|address| {
            TagSpec::new(driver.parse_address(address, &kind).unwrap(), kind.clone())
        });
        assert_eq!(
            session.read_batch(&specs).await.unwrap(),
            vec![Ok(DriverValue::F64(7.0)), Ok(DriverValue::F64(7.0))]
        );
        assert_eq!(fixture.d_reads(), vec![(3010, 2)]);
    }
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
async fn loopback_allowlist_accepts_only_named_d_words() {
    let (fixture, driver, mut session) = connected_session(Peer::R).await;
    for address in ["D3000", "D3001", "D3011", "D3013"] {
        let result = session
            .write_batch(&[item(&driver, address, 10.0)])
            .await
            .unwrap();
        assert!(matches!(
            result.as_slice(),
            [Err(ItemError::Unsupported(_))]
        ));
    }
    assert!(!fixture
        .commands()
        .iter()
        .any(|(command, _)| *command == 0x1401));
    assert_eq!(
        session
            .write_batch(&[item(&driver, "D3012", 10.0)])
            .await
            .unwrap(),
        vec![Ok(())]
    );
    assert_eq!(
        fixture
            .commands()
            .iter()
            .filter(|(command, _)| *command == 0x1401)
            .copied()
            .collect::<Vec<_>>(),
        vec![(0x1401, 3012)]
    );
}

fn range_items(driver: &WasmDriver) -> Vec<WriteItem> {
    ["D3010", "D3011", "D3012", "D3013"]
        .into_iter()
        .zip([9.0, 10.0, 9.0, 10.0])
        .map(|(address, value)| item(driver, address, value))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_WRITE_COMPONENT_WASM to the pinned write-v1 component"]
async fn range_host_preflight_rejects_every_bad_item_without_a_write_frame() {
    let (fixture, driver, mut session) = connected_session(Peer::R).await;
    let valid = range_items(&driver);
    let mut invalid = Vec::new();
    let mut reversed = valid.clone();
    reversed.reverse();
    invalid.push(reversed);
    let mut hole = valid.clone();
    hole[1] = item(&driver, "D3014", 10.0);
    invalid.push(hole);
    let mut duplicate = valid.clone();
    duplicate[1] = valid[0].clone();
    invalid.push(duplicate);
    let mut protected = valid.clone();
    protected[0] = item(&driver, "D3000", 9.0);
    invalid.push(protected);
    let mut protected = valid.clone();
    protected[0] = item(&driver, "D3001", 9.0);
    invalid.push(protected);
    let mut outside = valid.clone();
    outside[3] = item(&driver, "D3014", 10.0);
    invalid.push(outside);
    let mut overlap = valid.clone();
    overlap[0].spec.address.word_count = 2;
    invalid.push(overlap);
    let mut protected_span = valid.clone();
    protected_span[0] = item(&driver, "D2999", 9.0);
    protected_span[0].spec.address.word_count = 4;
    invalid.push(protected_span);
    let mut wrong_kind = valid.clone();
    wrong_kind[2].spec.kind = TagKind::numeric(DataType::U32);
    invalid.push(wrong_kind);
    let mut bad_value = valid.clone();
    bad_value[3].value = DriverValue::F64(f64::NAN);
    invalid.push(bad_value);
    invalid.push(valid[..3].to_vec());
    invalid.push([valid.clone(), vec![item(&driver, "D3014", 9.0)]].concat());
    for items in invalid {
        let result = session.write_batch(&items).await.unwrap();
        assert_eq!(result.len(), items.len());
        assert!(result
            .iter()
            .all(|entry| matches!(entry, Err(ItemError::Unsupported(_)))));
        assert!(fixture.write_frames().is_empty());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_RANGE_WRITE_COMPONENT_WASM to the reviewed range-write component"]
async fn contiguous_four_word_range_is_one_frame_with_ordered_covers_and_readback() {
    let steps = Arc::new(WriteSteps(Mutex::new(Vec::new())));
    let (fixture, driver, mut session) = connected_session_with(
        Peer::R,
        "BANTO_SLMP_RANGE_WRITE_COMPONENT_WASM",
        Some(steps.clone()),
    )
    .await;
    let items = range_items(&driver);
    assert_eq!(session.write_batch(&items).await.unwrap(), vec![Ok(()); 4]);
    assert_eq!(fixture.write_frames(), vec![(3010, vec![9, 10, 9, 10])]);
    assert_eq!(*steps.0.lock().unwrap(), vec![vec![0, 1, 2, 3]]);
    let specs: Vec<_> = items.iter().map(|item| item.spec.clone()).collect();
    assert_eq!(
        session.read_batch(&specs).await.unwrap(),
        [9.0, 10.0, 9.0, 10.0]
            .map(|value| Ok(DriverValue::F64(value)))
            .to_vec()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_RANGE_WRITE_COMPONENT_WASM to the reviewed range-write component"]
async fn hub_rest_range_batch_keeps_scope_audit_and_one_broker_frame() {
    use banto_hub_core::api_keys::ApiKeysService;

    let fixture = Fixture::start(Peer::R).await;
    let component =
        std::fs::read(std::env::var("BANTO_SLMP_RANGE_WRITE_COMPONENT_WASM").unwrap()).unwrap();
    let app = super::test_app_with_controlled_write_manifest(
        "t12-slmp-range-write-rest",
        MANIFEST,
        &component,
    )
    .await;
    let settings = serde_json::json!({"host":"127.0.0.1","port":fixture.address.port()});
    let saved = app
        .manager
        .plc_connections()
        .create_settings(super::ConnectionSettingsInput {
            name: "slmp-range-fixture-line".into(),
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
    .create(super::group_input("slmp-range-group", saved.id, 60_000))
    .await
    .unwrap();
    let mut tag_ids = Vec::new();
    let mut tag_names = Vec::new();
    for index in 3010..=3013 {
        let name = format!("word{index}");
        let mut tag = super::tag_input(&name, group.id, &format!("D{index}"), "u16");
        tag.writable = true;
        tag_ids.push(app.manager.tags().create(tag).await.unwrap().id);
        tag_names.push(format!("slmp-range-fixture-line.slmp-range-group.{name}"));
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
    let issued = api_keys
        .issue(
            "fixture-range-writer",
            tag_names.iter().map(|tag| format!("write:{tag}")).collect(),
            Some(now_ms + 600_000),
        )
        .await
        .expect("issue a scoped key in the temporary test database");
    app.write_control.enable();
    let request = serde_json::json!({
        "writes": tag_names
            .iter()
            .zip([9, 10, 9, 10])
            .map(|(tag, value)| serde_json::json!({"tag":tag,"v":value}))
            .collect::<Vec<_>>()
    });
    let response = tokio::time::timeout(
        Duration::from_secs(15),
        super::admin_write(
            &app.router,
            "POST",
            "/api/v1/values/batch",
            &issued.key,
            request,
        ),
    )
    .await;
    let frames = fixture.write_frames();
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

    assert!(revoked.expect("revoke temporary key").revoked_at.is_some());
    let (status, body) = response.expect("bounded REST range write");
    assert_eq!(status, super::StatusCode::OK, "{body:?}");
    let writes = body["writes"].as_array().unwrap();
    assert_eq!(writes.len(), 4);
    for (result, tag) in writes.iter().zip(&tag_names) {
        assert_eq!(result["tag"], *tag);
        assert_eq!(result["ok"], true, "{result:?}");
    }
    assert_eq!(frames, vec![(3010, vec![9, 10, 9, 10])]);
    assert_eq!(audit.0, super::StatusCode::OK);
    let rows = audit.1["rows"].as_array().unwrap();
    for tag_id in tag_ids {
        assert!(rows
            .iter()
            .any(|row| row["tagId"].as_i64() == Some(tag_id) && row["result"] == "ok"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_RANGE_WRITE_COMPONENT_WASM to the reviewed range-write component"]
async fn uncertain_four_word_ack_closes_session_without_a_second_write_frame() {
    for peer in [Peer::BadAck, Peer::Timeout] {
        let (mut fixture, driver, mut session) =
            connected_session_with(peer, "BANTO_SLMP_RANGE_WRITE_COMPONENT_WASM", None).await;
        let result = session.write_batch(&range_items(&driver)).await;
        assert!(matches!(
            result,
            Err(FatalError::Framing(_)) | Err(FatalError::Protocol(_)) | Err(FatalError::Timeout)
        ));
        tokio::time::timeout(
            Duration::from_secs(3),
            fixture.closed.wait_for(|count| *count >= 1),
        )
        .await
        .expect("failed range write closes the socket")
        .unwrap();
        assert_eq!(fixture.write_frames(), vec![(3010, vec![9, 10, 9, 10])]);

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
            "slmp-range-reconnect",
            Duration::from_millis(300),
            Duration::from_millis(300),
        );
        let mut recovered =
            FrameSession::connect(driver.as_ref(), &settings, &transport, &context, None)
                .await
                .unwrap();
        let kind = TagKind::numeric(DataType::U16);
        let spec = TagSpec::new(driver.parse_address("D3010", &kind).unwrap(), kind);
        assert_eq!(
            recovered.read_batch(&[spec]).await.unwrap(),
            vec![Ok(DriverValue::F64(9.0))]
        );
        assert_eq!(fixture.write_frames(), vec![(3010, vec![9, 10, 9, 10])]);
        assert_eq!(*fixture.accepted.borrow(), 2);
        recovered.disconnect().await;
        tokio::time::timeout(
            Duration::from_secs(3),
            fixture.closed.wait_for(|count| *count >= 2),
        )
        .await
        .expect("reconnected range session closes")
        .unwrap();
        assert_eq!(fixture.write_frames(), vec![(3010, vec![9, 10, 9, 10])]);
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
async fn broker_timeout_after_write_closes_socket_without_resending_on_reconnect() {
    let mut fixture = Fixture::start(Peer::Timeout).await;
    let component =
        std::fs::read(std::env::var("BANTO_SLMP_WRITE_COMPONENT_WASM").unwrap()).unwrap();
    let app = super::test_app_with_controlled_write_manifest(
        "t12-slmp-wasm-write-timeout",
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
    app.manager
        .tags()
        .create(super::tag_input("word", group.id, "D3010", "u16"))
        .await
        .unwrap();
    app.manager.rebuild().await.unwrap();
    let mut status = app.sessions.status_watch(saved.id).unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        status.wait_for(|state| *state == banto_broker::BrokerConnectionStatus::Connected),
    )
    .await
    .expect("first broker connection")
    .unwrap();

    let driver = app.manager.registry().get("slmp-write-fixture").unwrap();
    let kind = TagKind::numeric(DataType::U16);
    let spec = TagSpec::new(driver.base().parse_address("D3010", &kind).unwrap(), kind);
    let handle = app.manager.write_broker_handle_peek(saved.id).unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        handle.write(vec![WriteItem::new(spec.clone(), DriverValue::F64(9.0))]),
    )
    .await
    .expect("bounded broker write");
    assert!(matches!(
        result,
        Err(banto_broker::BrokerError::ConnectionFailed { .. })
    ));
    tokio::time::timeout(
        Duration::from_secs(3),
        fixture.closed.wait_for(|count| *count >= 1),
    )
    .await
    .expect("failed session closes the first socket")
    .unwrap();

    // The broker reconnects for future work, but it must not replay the failed write.
    tokio::time::timeout(
        Duration::from_secs(8),
        fixture.accepted.wait_for(|count| *count >= 2),
    )
    .await
    .expect("broker reconnects after the write failure")
    .unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        status.wait_for(|state| *state == banto_broker::BrokerConnectionStatus::Connected),
    )
    .await
    .expect("second broker connection is ready")
    .unwrap();
    // A round trip on the same broker handle proves the second socket is usable.
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), handle.read(vec![spec]))
            .await
            .expect("bounded read after reconnect")
            .unwrap(),
        vec![Ok(DriverValue::F64(7.0))]
    );
    assert_eq!(*fixture.accepted.borrow(), 2);
    assert_eq!(*fixture.closed.borrow(), 1);
    let write_events = fixture.commands.clone();
    let write_count = || {
        write_events
            .lock()
            .unwrap()
            .iter()
            .filter(|(command, _)| *command == 0x1401)
            .count()
    };
    assert_eq!(
        write_count(),
        1,
        "a failed write must not be replayed after reconnect"
    );

    drop(handle);
    drop(app);
    tokio::time::timeout(
        Duration::from_secs(5),
        fixture.closed.wait_for(|count| *count >= 2),
    )
    .await
    .expect("test app shutdown closes the second socket")
    .unwrap();
    assert_eq!(
        write_count(),
        1,
        "shutdown must not replay the failed write"
    );
    assert_eq!(
        *fixture.accepted.borrow(),
        2,
        "shutdown must not open a third socket"
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_WRITE_COMPONENT_WASM to the pinned write-v1 component"]
async fn hub_api_timeout_then_explicit_recovery_and_revoked_key_rejection() {
    use banto_hub_core::api_keys::{ApiKeyLookup, ApiKeysService};

    let mut fixture = Fixture::start(Peer::TimeoutOnce).await;
    let component =
        std::fs::read(std::env::var("BANTO_SLMP_WRITE_COMPONENT_WASM").unwrap()).unwrap();
    let app = super::test_app_with_controlled_write_manifest(
        "t12-slmp-wasm-write-rest-timeout",
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
    .create(super::group_input("slmp-write-group", saved.id, 60_000))
    .await
    .unwrap();
    let mut tag = super::tag_input("word", group.id, "D3010", "u16");
    tag.writable = true;
    let tag_id = app.manager.tags().create(tag).await.unwrap().id;
    app.manager.rebuild().await.unwrap();
    let mut status = app.sessions.status_watch(saved.id).unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        status.wait_for(|state| *state == banto_broker::BrokerConnectionStatus::Connected),
    )
    .await
    .expect("first broker connection")
    .unwrap();

    let driver = app.manager.registry().get("slmp-write-fixture").unwrap();
    let kind = TagKind::numeric(DataType::U16);
    let spec = TagSpec::new(driver.base().parse_address("D3010", &kind).unwrap(), kind);
    let handle = app.manager.write_broker_handle_peek(saved.id).unwrap();
    let api_keys = ApiKeysService::new(app.pool.clone());
    let now_ms = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let expires_at = now_ms + 600_000;
    let scope = "write:slmp-write-fixture-line.slmp-write-group.word".to_owned();
    let issued = api_keys
        .issue(
            "fixture-timeout-writer",
            vec![scope.clone()],
            Some(expires_at),
        )
        .await
        .expect("issue scoped key in the temporary test database");
    let issued_id = issued.id;
    let issued_scopes = issued.scopes.clone();
    app.write_control.enable();

    // Do not assert until after revoke and TestApp cleanup. A failed assertion
    // must not leave the temporary test key active or the DB directory behind.
    let path = "/api/v1/values/slmp-write-fixture-line.slmp-write-group.word";
    let first_write = tokio::time::timeout(
        Duration::from_secs(15),
        super::admin_write(
            &app.router,
            "POST",
            path,
            &issued.key,
            serde_json::json!({"v":9}),
        ),
    )
    .await;
    let first_closed = tokio::time::timeout(
        Duration::from_secs(3),
        fixture.closed.wait_for(|count| *count >= 1),
    )
    .await
    .is_ok_and(|result| result.is_ok());
    let second_accepted = tokio::time::timeout(
        Duration::from_secs(8),
        fixture.accepted.wait_for(|count| *count >= 2),
    )
    .await
    .is_ok_and(|result| result.is_ok());
    let second_connected = tokio::time::timeout(
        Duration::from_secs(5),
        status.wait_for(|state| *state == banto_broker::BrokerConnectionStatus::Connected),
    )
    .await
    .is_ok_and(|result| result.is_ok());
    let read_after_reconnect =
        tokio::time::timeout(Duration::from_secs(5), handle.read(vec![spec])).await;
    // This is an explicit second request, distinguishable from a replay of v=9.
    let recovery_write = if matches!(
        &first_write,
        Ok((super::StatusCode::BAD_GATEWAY, body)) if body["error"] == "write_failed"
    ) && matches!(
        &read_after_reconnect,
        Ok(Ok(values)) if values == &vec![Ok(DriverValue::F64(7.0))]
    ) {
        Some(
            tokio::time::timeout(
                Duration::from_secs(15),
                super::admin_write(
                    &app.router,
                    "POST",
                    path,
                    &issued.key,
                    serde_json::json!({"v":10}),
                ),
            )
            .await,
        )
    } else {
        None
    };
    let accepted_before_shutdown = *fixture.accepted.borrow();
    let closed_before_shutdown = *fixture.closed.borrow();
    let audit_before_revoke = tokio::time::timeout(
        Duration::from_secs(5),
        super::admin_write(
            &app.router,
            "POST",
            "/api/write-audit/list",
            &app.admin_token,
            serde_json::json!({}),
        ),
    )
    .await;
    let commands_before_revoke = fixture.commands();
    let accepted_before_revoke = *fixture.accepted.borrow();

    let key_summaries = tokio::time::timeout(Duration::from_secs(5), api_keys.list()).await;
    let revoked = tokio::time::timeout(Duration::from_secs(5), api_keys.revoke(issued_id)).await;
    // Do not attempt another write if revocation failed or its result is unknown.
    let revoked_write = if matches!(&revoked, Ok(Ok(_))) {
        Some(
            tokio::time::timeout(
                Duration::from_secs(5),
                super::admin_write(
                    &app.router,
                    "POST",
                    path,
                    &issued.key,
                    serde_json::json!({"v":10}),
                ),
            )
            .await,
        )
    } else {
        None
    };
    let commands_after_revoke = fixture.commands();
    let accepted_after_revoke = *fixture.accepted.borrow();
    let audit_after_revoke = tokio::time::timeout(
        Duration::from_secs(5),
        super::admin_write(
            &app.router,
            "POST",
            "/api/write-audit/list",
            &app.admin_token,
            serde_json::json!({}),
        ),
    )
    .await;

    app.write_control.disable();
    let write_control_disabled = !app.write_control.is_enabled();
    let lookup_after_revoke =
        tokio::time::timeout(Duration::from_secs(5), api_keys.lookup(&issued.key, now_ms)).await;
    drop(issued);
    drop(api_keys);
    drop(handle);
    let db_path = app._env.registry_path();
    let temp_dir = db_path.parent().unwrap().to_path_buf();
    drop(app);
    let second_closed = tokio::time::timeout(
        Duration::from_secs(5),
        fixture.closed.wait_for(|count| *count >= 2),
    )
    .await
    .is_ok_and(|result| result.is_ok());
    let final_accepted = *fixture.accepted.borrow();
    let final_closed = *fixture.closed.borrow();
    let commands = fixture.commands();
    let db_removed = !db_path.exists();
    let temp_dir_removed = !temp_dir.exists();

    let revoked = revoked
        .expect("bounded key revocation")
        .expect("revoke temporary test key");
    assert!(revoked.revoked_at.is_some());
    assert!(matches!(
        lookup_after_revoke,
        Ok(Ok(ApiKeyLookup::Revoked { id, .. })) if id == issued_id
    ));
    let key_summaries = key_summaries
        .expect("bounded key metadata lookup")
        .expect("read temporary key metadata");
    let key_summary = key_summaries
        .iter()
        .find(|key| key.id == issued_id)
        .unwrap();
    assert_eq!(issued_scopes, vec![scope]);
    assert_eq!(key_summary.scopes, issued_scopes);
    assert_eq!(key_summary.expires_at, Some(expires_at));
    assert!(write_control_disabled);
    assert!(db_removed, "temporary test database must be removed");
    assert!(temp_dir_removed, "temporary test directory must be removed");

    let (write_status, write_body) = first_write.expect("bounded failed REST write");
    assert_eq!(
        write_status,
        super::StatusCode::BAD_GATEWAY,
        "{write_body:?}"
    );
    assert_eq!(write_body["error"], "write_failed");
    let timeout_reason = write_body["detail"].as_str().unwrap();
    assert!(timeout_reason.contains(&FatalError::Timeout.to_string()));
    let (recovery_status, recovery_body) = recovery_write
        .expect("recovery write must be attempted")
        .expect("bounded recovery REST write");
    assert_eq!(recovery_status, super::StatusCode::OK, "{recovery_body:?}");
    assert_eq!(recovery_body["result"], "ok");
    let (revoked_status, revoked_body) = revoked_write
        .expect("revoked key must be tested through REST")
        .expect("bounded revoked-key REST request");
    assert_eq!(revoked_status, super::StatusCode::UNAUTHORIZED);
    assert!(revoked_body["kind"] == "unauthorized" || revoked_body["error"] == "unauthorized");

    let (audit_status, audit) = audit_before_revoke.expect("bounded first audit list");
    assert_eq!(audit_status, super::StatusCode::OK, "{audit:?}");
    let matching_rows: Vec<_> = audit["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["tagId"].as_i64() == Some(tag_id))
        .collect();
    assert_eq!(matching_rows.len(), 2, "{audit:?}");
    let uncertain_row = matching_rows
        .iter()
        // #898: a response timeout after the request was sent is `uncertain`.
        .find(|row| row["result"] == "uncertain")
        .unwrap();
    let uncertain_detail: serde_json::Value =
        serde_json::from_str(uncertain_row["detail"].as_str().unwrap()).unwrap();
    assert_eq!(uncertain_detail["target"], "plc");
    assert_eq!(uncertain_detail["detail"], timeout_reason);
    assert_eq!(
        matching_rows
            .iter()
            .filter(|row| row["result"] == "ok")
            .count(),
        1
    );
    let (audit_status_after, audit_after) = audit_after_revoke.expect("bounded final audit list");
    assert_eq!(audit_status_after, super::StatusCode::OK);
    let rows_after: Vec<_> = audit_after["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["tagId"].as_i64() == Some(tag_id))
        .collect();
    assert_eq!(
        rows_after, matching_rows,
        "revoked request must not create a write audit row"
    );
    assert_eq!(commands_before_revoke, commands_after_revoke);
    assert_eq!(accepted_before_revoke, accepted_after_revoke);
    assert!(first_closed);
    assert!(second_accepted);
    assert!(second_connected);
    assert_eq!(
        read_after_reconnect.unwrap().unwrap(),
        vec![Ok(DriverValue::F64(7.0))]
    );
    assert_eq!((accepted_before_shutdown, closed_before_shutdown), (2, 1));
    assert!(second_closed);
    assert_eq!((final_accepted, final_closed), (2, 2));
    assert_eq!(fixture.write_values(), vec![9, 10]);
    assert_eq!(
        commands
            .iter()
            .filter(|(command, _)| *command == 0x1401)
            .count(),
        2,
        "only the failed write and the explicit recovery write may be sent"
    );
}
