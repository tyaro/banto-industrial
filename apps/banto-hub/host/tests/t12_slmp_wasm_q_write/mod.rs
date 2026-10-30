//! Q25PRH write-v1 acceptance through an isolated loopback PLC.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use banto_driver_api::{
    ConnectContext, DataType, DriverBase, ItemError, Session as _, Settings, TagKind, TagSpec,
    Value, WriteItem,
};
use banto_driver_engine::{transport::TransportSpec, FrameSession};
use banto_driver_wasm::WasmDriver;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const MANIFEST: &[u8] = include_bytes!("manifest.json");
type Frames = Arc<Mutex<Vec<[u16; 4]>>>;

struct Fixture {
    address: std::net::SocketAddr,
    frames: Frames,
    commands: Arc<Mutex<Vec<(u16, u32, u16)>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn start(wrong_code: bool) -> Self {
        Self::start_with_sd(wrong_code, Some(32768)).await
    }

    async fn start_with_sd(wrong_code: bool, sd_count: Option<u16>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let frames: Frames = Arc::new(Mutex::new(Vec::new()));
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
                        u32::from_le_bytes([request[19], request[20], request[21], 0])
                    };
                    events.lock().unwrap().push((command, start, subcommand));
                    let (end_code, body): (u16, Vec<u8>) = match (command, subcommand) {
                        (0x0101, 0) => {
                            assert_eq!(request.len(), 19);
                            let mut body = vec![b' '; 18];
                            body[..9].copy_from_slice(b"Q25PRHCPU");
                            let code: u16 = if wrong_code { 0x004d } else { 0x004c };
                            body[16..18].copy_from_slice(&code.to_le_bytes());
                            (0, body)
                        }
                        (0x0401, 0) if request[22] == 0xa9 => {
                            assert_eq!(request.len(), 25);
                            assert_eq!(start, 302);
                            assert_eq!(u16::from_le_bytes([request[23], request[24]]), 1);
                            match sd_count {
                                Some(count) => (0, count.to_le_bytes().to_vec()),
                                None => (0xc059, vec![]),
                            }
                        }
                        (0x0401, 0) => {
                            assert_eq!(request.len(), 25);
                            assert_eq!(request[22], 0xa8);
                            assert!((3010..=3013).contains(&start));
                            assert_eq!(u16::from_le_bytes([request[23], request[24]]), 1);
                            let latest = writes.lock().unwrap().last().copied().unwrap_or([7; 4]);
                            (0, latest[(start - 3010) as usize].to_le_bytes().to_vec())
                        }
                        (0x1401, 0) => {
                            assert_eq!(request.len(), 33);
                            assert_eq!(start, 3010);
                            assert_eq!(request[22], 0xa8);
                            assert_eq!(u16::from_le_bytes([request[23], request[24]]), 4);
                            let words = std::array::from_fn(|index| {
                                u16::from_le_bytes(
                                    request[25 + index * 2..27 + index * 2].try_into().unwrap(),
                                )
                            });
                            writes.lock().unwrap().push(words);
                            (0, vec![])
                        }
                        _ => panic!("unexpected Q command {command:#x}/{subcommand:#x}"),
                    };
                    let mut response = vec![0xd4, 0];
                    response.extend_from_slice(&request[2..11]);
                    response
                        .extend_from_slice(&u16::try_from(body.len() + 2).unwrap().to_le_bytes());
                    response.extend_from_slice(&end_code.to_le_bytes());
                    response.extend_from_slice(&body);
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

    fn frames(&self) -> Vec<[u16; 4]> {
        self.frames.lock().unwrap().clone()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn settings(port: u16) -> Settings {
    Settings::from_pairs([
        ("host".to_owned(), "127.0.0.1".to_owned()),
        ("port".to_owned(), port.to_string()),
        (
            "q25prh_d3010_3013_test_write".to_owned(),
            "enabled".to_owned(),
        ),
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

fn items(specs: &[TagSpec], values: [u16; 4]) -> Vec<WriteItem> {
    specs
        .iter()
        .cloned()
        .zip(values)
        .map(|(spec, value)| WriteItem::new(spec, Value::F64(f64::from(value))))
        .collect()
}

async fn connect(wrong_code: bool) -> (Fixture, Arc<WasmDriver>, FrameSession) {
    connect_with_sd(wrong_code, Some(32768)).await
}

async fn connect_with_sd(
    wrong_code: bool,
    sd_count: Option<u16>,
) -> (Fixture, Arc<WasmDriver>, FrameSession) {
    let fixture = Fixture::start_with_sd(wrong_code, sd_count).await;
    let component =
        std::fs::read(std::env::var("BANTO_SLMP_Q_WRITE_COMPONENT_WASM").unwrap()).unwrap();
    let driver = WasmDriver::load_controlled_q_test_fixture(MANIFEST, &component).unwrap();
    let transport = TransportSpec::Tcp {
        host: "127.0.0.1".into(),
        port: fixture.address.port(),
    };
    let context = ConnectContext::new(
        "slmp-q-write-fixture",
        Duration::from_millis(500),
        Duration::from_millis(500),
    );
    let session = FrameSession::connect(
        driver.as_ref(),
        &settings(fixture.address.port()),
        &transport,
        &context,
        None,
    )
    .await
    .unwrap();
    (fixture, driver, session)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_Q_WRITE_COMPONENT_WASM to the reviewed Q25PRH component"]
async fn q25prh_sd302_missing_or_short_range_never_sends_write() {
    for count in [Some(3013), None] {
        let (fixture, driver, mut session) = connect_with_sd(false, count).await;
        let result = session
            .write_batch(&items(&specs(&driver), [1, 2, 3, 4]))
            .await
            .unwrap();
        assert_eq!(result.len(), 4);
        assert!(result.iter().all(Result::is_err));
        assert!(fixture.frames().is_empty());
        assert_eq!(
            fixture.commands.lock().unwrap().as_slice(),
            &[(0x0101, 0, 0), (0x0401, 302, 0)]
        );
        session.disconnect().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_Q_WRITE_COMPONENT_WASM to the reviewed Q25PRH component"]
async fn q25prh_range_is_one_q_frame_and_unknown_reads_remain_individual() {
    let (fixture, driver, mut session) = connect(false).await;
    assert_eq!(
        fixture.commands.lock().unwrap().as_slice(),
        &[(0x0101, 0, 0), (0x0401, 302, 0)]
    );
    let specs = specs(&driver);
    assert_eq!(
        session.read_batch(&specs).await.unwrap(),
        vec![Ok(Value::F64(7.0)); 4]
    );
    assert_eq!(
        session
            .write_batch(&items(&specs, [1, 2, 3, 4]))
            .await
            .unwrap(),
        vec![Ok(()); 4]
    );
    assert_eq!(fixture.frames(), vec![[1, 2, 3, 4]]);
    assert_eq!(
        session.read_batch(&specs).await.unwrap(),
        [1.0, 2.0, 3.0, 4.0]
            .map(|value| Ok(Value::F64(value)))
            .to_vec()
    );
    let commands = fixture.commands.lock().unwrap().clone();
    assert_eq!(
        commands
            .iter()
            .filter(|(command, _, _)| *command == 0x1401)
            .count(),
        1
    );
    assert_eq!(
        commands
            .iter()
            .filter(|(command, start, _)| *command == 0x0401 && (3010..=3013).contains(start))
            .count(),
        8
    );
    session.disconnect().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_Q_WRITE_COMPONENT_WASM to the reviewed Q25PRH component"]
async fn q25prh_wrong_model_or_bad_span_cannot_send_write() {
    for wrong_code in [true, false] {
        let (fixture, driver, mut session) = connect(wrong_code).await;
        let specs = specs(&driver);
        let mut bad = items(&specs, [1, 2, 3, 4]);
        bad.reverse();
        let mut invalid = vec![bad];
        let mut hole = items(&specs, [1, 2, 3, 4]);
        let kind = TagKind::numeric(DataType::U16);
        hole[2].spec = TagSpec::new(driver.parse_address("D3014", &kind).unwrap(), kind.clone());
        invalid.push(hole);
        let mut protected = items(&specs, [1, 2, 3, 4]);
        protected[0].spec = TagSpec::new(driver.parse_address("D3000", &kind).unwrap(), kind);
        invalid.push(protected);
        let mut nonfinite = items(&specs, [1, 2, 3, 4]);
        nonfinite[3].value = Value::F64(f64::NAN);
        invalid.push(nonfinite);
        for batch in invalid {
            let result = session.write_batch(&batch).await.unwrap();
            assert_eq!(result.len(), 4);
            assert!(result
                .iter()
                .all(|entry| matches!(entry, Err(ItemError::Unsupported(_)))));
        }
        if wrong_code {
            assert!(session
                .write_batch(&items(&specs, [1, 2, 3, 4]))
                .await
                .unwrap()
                .iter()
                .all(Result::is_err));
        }
        assert!(fixture.frames().is_empty());
        session.disconnect().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_Q_WRITE_COMPONENT_WASM to the reviewed Q25PRH component"]
async fn q25prh_normal_hub_settings_connect_and_broker_write_one_frame() {
    let fixture = Fixture::start(false).await;
    let component =
        std::fs::read(std::env::var("BANTO_SLMP_Q_WRITE_COMPONENT_WASM").unwrap()).unwrap();
    let app = super::test_app_with_controlled_q_write_manifest(
        "t12-q25prh-loopback",
        MANIFEST,
        &component,
    )
    .await;
    let saved = app
        .manager
        .plc_connections()
        .create_settings(super::ConnectionSettingsInput {
            name: "q25prh-fixture-line".into(),
            protocol: "slmp-write-fixture".into(),
            enabled: true,
            simulation: false,
            settings: serde_json::json!({
                "host":"127.0.0.1",
                "port":fixture.address.port(),
                "q25prh_d3010_3013_test_write":"enabled"
            })
            .as_object()
            .unwrap()
            .clone(),
            secrets: Default::default(),
        })
        .await
        .expect("Q test opt-in must pass the guest's declared settings schema");
    let group = super::CollectionGroupService::with_registry(
        app.pool.clone(),
        app.manager.registry().clone(),
    )
    .create(super::group_input("q25prh-write-group", saved.id, 60_000))
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
    app.commit_and_start().await;
    let mut status = app.sessions.status_watch(saved.id).unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        status.wait_for(|state| *state == banto_broker::BrokerConnectionStatus::Connected),
    )
    .await
    .unwrap()
    .unwrap();
    let driver = app.manager.registry().get("slmp-write-fixture").unwrap();
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
    assert_eq!(
        handle.write(items(&specs, [1, 2, 3, 4])).await.unwrap(),
        vec![Ok(()); 4]
    );
    assert_eq!(
        handle.read(specs).await.unwrap(),
        [1.0, 2.0, 3.0, 4.0]
            .map(|value| Ok(Value::F64(value)))
            .to_vec()
    );
    assert_eq!(fixture.frames(), vec![[1, 2, 3, 4]]);
    let commands = fixture.commands.lock().unwrap().clone();
    assert_eq!(commands[..2], [(0x0101, 0, 0), (0x0401, 302, 0)]);
    assert_eq!(
        commands
            .iter()
            .filter(|(command, _, _)| *command == 0x1401)
            .count(),
        1
    );
    assert_eq!(
        commands
            .iter()
            .filter(|(command, start, _)| *command == 0x0401 && (3010..=3013).contains(start))
            .count(),
        8
    );
    drop(app);
}
