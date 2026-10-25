//! Hub/broker acceptance for the pinned, separately built SLMP WASM guest.
//! This fixture only listens on loopback and never contacts a PLC.

use super::*;
use banto_collect::current::Quality;
use banto_driver_api::{ConnectContext, DataType, ItemError, RawValue, TagSpec};
use banto_driver_engine::{transport::TransportSpec, FrameSession};
use banto_driver_wasm::{LoadError, WasmDriver};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

const MANIFEST: &[u8] = include_bytes!("manifest.json");
const PROTOCOL: &str = "slmp-read-wasm";
const MAX_D_RESPONSE_LEN: usize = 15 + 960 * 2;

#[test]
#[ignore = "set BANTO_SLMP_COMPONENT_WASM to the pinned legacy read component"]
fn legacy_read_component_cannot_satisfy_a_write_manifest() {
    let component = std::fs::read(std::env::var("BANTO_SLMP_COMPONENT_WASM").unwrap()).unwrap();
    let mut manifest: Value = serde_json::from_slice(MANIFEST).unwrap();
    manifest["id"] = serde_json::json!("slmp-write-fixture");
    manifest["capabilities"] = serde_json::json!(["read", "write"]);
    manifest["writeApi"] = serde_json::json!("1");
    let manifest = serde_json::to_vec(&manifest).unwrap();
    assert!(matches!(
        WasmDriver::load(&manifest, &component),
        Err(LoadError::Contract(_))
    ));
    assert!(matches!(
        WasmDriver::load_controlled_write_fixture(&manifest, &component),
        Err(LoadError::Runtime(error)) if error.0.operation == "bind-write-v1"
    ));
    assert!(WasmDriver::load(MANIFEST, &component).is_ok());
}

#[derive(Clone, Copy, Debug)]
enum Scenario {
    Q,
    R,
    SdRefused,
    UnknownModel,
    TypeRefused,
    TypeFraming,
    ReadRefused,
    ReadFraming,
    RZero,
    Multi,
    MultiItemRefusal,
    BatchRefusal,
    Q25Prh,
    Q25PrhWrongName,
    Q25PrhWrongCode,
    LargeSplit,
    LargeShort,
    LargeOverlong,
}

#[derive(Clone, Debug)]
struct Request {
    connection: usize,
    command: u16,
    subcommand: u16,
    device: Option<u8>,
    start: Option<u32>,
    count: Option<u16>,
}

impl Request {
    fn from_bytes(connection: usize, bytes: &[u8]) -> Self {
        assert_eq!(&bytes[..2], &[0x54, 0]);
        let command = u16::from_le_bytes([bytes[15], bytes[16]]);
        let subcommand = u16::from_le_bytes([bytes[17], bytes[18]]);
        let (device, start, count) = if command == 0x0401 {
            let r = subcommand == 2;
            let start = u32::from_le_bytes([
                bytes[19],
                bytes[20],
                bytes[21],
                if r { bytes[22] } else { 0 },
            ]);
            let device = bytes[if r { 23 } else { 22 }];
            let offset = if r { 25 } else { 23 };
            let count = u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
            (Some(device), Some(start), Some(count))
        } else {
            (None, None, None)
        };
        Self {
            connection,
            command,
            subcommand,
            device,
            start,
            count,
        }
    }
}

struct SlmpFixture {
    addr: SocketAddr,
    connections: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<Request>>>,
    clients: Arc<Mutex<Vec<JoinHandle<()>>>>,
    version: Arc<AtomicUsize>,
    listener: JoinHandle<()>,
}

impl SlmpFixture {
    async fn start(scenario: Scenario) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let connections = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let clients = Arc::new(Mutex::new(Vec::new()));
        let version = Arc::new(AtomicUsize::new(0));
        let counts = connections.clone();
        let events = requests.clone();
        let handlers = clients.clone();
        let reply_version = version.clone();
        let listener = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let connection = counts.fetch_add(1, Ordering::SeqCst) + 1;
                let events = events.clone();
                let version = reply_version.clone();
                let handler = tokio::spawn(async move {
                    serve_connection(stream, connection, scenario, events, version).await;
                });
                handlers.lock().unwrap().push(handler);
            }
        });
        Self {
            addr,
            connections,
            requests,
            clients,
            version,
            listener,
        }
    }

    fn connection_count(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }

    fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }

    fn close_clients(&self) {
        for client in self.clients.lock().unwrap().drain(..) {
            client.abort();
        }
    }

    fn set_version(&self, version: usize) {
        self.version.store(version, Ordering::SeqCst);
    }
}

impl Drop for SlmpFixture {
    fn drop(&mut self) {
        self.listener.abort();
        self.close_clients();
    }
}

async fn serve_connection(
    mut stream: TcpStream,
    connection: usize,
    scenario: Scenario,
    requests: Arc<Mutex<Vec<Request>>>,
    version: Arc<AtomicUsize>,
) {
    loop {
        let mut head = [0; 13];
        if stream.read_exact(&mut head).await.is_err() {
            return;
        }
        let length = usize::from(u16::from_le_bytes([head[11], head[12]]));
        if !(6..=64).contains(&length) {
            return;
        }
        let mut frame = head.to_vec();
        frame.resize(13 + length, 0);
        if stream.read_exact(&mut frame[13..]).await.is_err() {
            return;
        }
        let request = Request::from_bytes(connection, &frame);
        requests.lock().unwrap().push(request.clone());
        let (end_code, body, bad_serial) =
            reply(scenario, &request, version.load(Ordering::SeqCst));
        let mut response = wire_response(&frame, end_code, &body, bad_serial);
        if matches!(
            scenario,
            Scenario::LargeSplit | Scenario::LargeShort | Scenario::LargeOverlong
        ) && request.device == Some(0xa8)
        {
            assert_eq!(response.len(), MAX_D_RESPONSE_LEN);
            match scenario {
                Scenario::LargeSplit => {
                    // Pause around the header, length field, payload and final word so
                    // a full-size response must be accumulated over multiple reads.
                    let mut start = 0;
                    for end in [
                        2,
                        11,
                        13,
                        15,
                        513,
                        1024,
                        MAX_D_RESPONSE_LEN - 1,
                        MAX_D_RESPONSE_LEN,
                    ] {
                        if stream.write_all(&response[start..end]).await.is_err() {
                            return;
                        }
                        start = end;
                        tokio::time::sleep(Duration::from_millis(2)).await;
                    }
                    continue;
                }
                Scenario::LargeShort => {
                    let _ = stream.write_all(&response[..response.len() - 2]).await;
                    return;
                }
                Scenario::LargeOverlong => {
                    response[11..13].copy_from_slice(&1924u16.to_le_bytes());
                    let _ = stream.write_all(&response).await;
                    return;
                }
                _ => unreachable!(),
            }
        }
        if stream.write_all(&response).await.is_err() {
            return;
        }
    }
}

fn wire_response(request: &[u8], end_code: u16, body: &[u8], bad_serial: bool) -> Vec<u8> {
    let mut response = vec![0xd4, 0];
    response.extend_from_slice(&request[2..11]);
    response.extend_from_slice(&(u16::try_from(body.len() + 2).unwrap()).to_le_bytes());
    response.extend_from_slice(&end_code.to_le_bytes());
    response.extend_from_slice(body);
    if bad_serial {
        response[2] ^= 1;
    }
    response
}

fn model(name: &str, code: u16) -> Vec<u8> {
    let mut body = vec![b' '; 18];
    body[..name.len()].copy_from_slice(name.as_bytes());
    body[16..18].copy_from_slice(&code.to_le_bytes());
    body
}

fn reply(scenario: Scenario, request: &Request, version: usize) -> (u16, Vec<u8>, bool) {
    match (request.command, request.device) {
        (0x0101, None) => match scenario {
            Scenario::R | Scenario::RZero => (0, model("R04CPU", 0x4800), false),
            Scenario::Q25Prh => (0, model("Q25PRHCPU", 0x004c), false),
            Scenario::Q25PrhWrongName => (0, model("Q25PRXCPU", 0x004c), false),
            Scenario::Q25PrhWrongCode => (0, model("Q25PRHCPU", 0x004d), false),
            Scenario::UnknownModel => (0, model("RCPU", 0x0360), false),
            Scenario::TypeRefused => (0xc05b, vec![], false),
            Scenario::TypeFraming => (0, model("Q02UCPU", 0x0263), true),
            _ => (0, model("Q02UCPU", 0x0263), false),
        },
        (0x0401, Some(0xa9)) => {
            if matches!(scenario, Scenario::SdRefused) {
                (0xc05b, vec![], false)
            } else if matches!(scenario, Scenario::R | Scenario::RZero) {
                let count: u32 = if matches!(scenario, Scenario::RZero) {
                    0
                } else {
                    32
                };
                (0, count.to_le_bytes().to_vec(), false)
            } else if matches!(
                scenario,
                Scenario::LargeSplit | Scenario::LargeShort | Scenario::LargeOverlong
            ) {
                (0, 1000u16.to_le_bytes().to_vec(), false)
            } else {
                (0, 100u16.to_le_bytes().to_vec(), false)
            }
        }
        (0x0401, Some(0xa8)) => match scenario {
            Scenario::ReadRefused => (0xc05b, vec![], false),
            Scenario::ReadFraming => (0, 42u16.to_le_bytes().to_vec(), true),
            // D100 is outside the confirmed Q range and must stay an individual request.
            Scenario::MultiItemRefusal if request.start == Some(100) => (0xc05b, vec![], false),
            Scenario::BatchRefusal if request.start == Some(0) && request.count != Some(1) => {
                (0xc05b, vec![], false)
            }
            Scenario::LargeSplit | Scenario::LargeShort | Scenario::LargeOverlong => {
                assert_eq!((request.start, request.count), (Some(0), Some(960)));
                let mut body = Vec::with_capacity(960 * 2);
                for offset in 0..960u16 {
                    body.extend_from_slice(&(1000 + offset).to_le_bytes());
                }
                (0, body, false)
            }
            Scenario::Q25Prh => {
                let value = match request.start {
                    Some(0) => 42u16, // connection-test probe
                    Some(3000 | 3001) if request.count == Some(1) => {
                        u16::try_from(request.connection * 1_000 + version * 100).unwrap()
                            + u16::try_from(request.start.unwrap() - 3000).unwrap()
                    }
                    _ => return (0xc05b, vec![], false),
                };
                (0, value.to_le_bytes().to_vec(), false)
            }
            Scenario::Multi | Scenario::MultiItemRefusal | Scenario::BatchRefusal => {
                let start = request.start.unwrap();
                let count = request.count.unwrap();
                assert!(
                    (1..=100).contains(&count),
                    "unexpected D word count: {request:?}"
                );
                let mut body = Vec::with_capacity(usize::from(count) * 2);
                for address in start..start + u32::from(count) {
                    let value = u16::try_from(request.connection * 1_000 + version * 100).unwrap()
                        + u16::try_from(address).unwrap();
                    body.extend_from_slice(&value.to_le_bytes());
                }
                (0, body, false)
            }
            _ => {
                let value: u16 = match request.start.unwrap() {
                    31 => 0xabcd,
                    100 => 88,
                    _ => 42,
                };
                (0, value.to_le_bytes().to_vec(), false)
            }
        },
        _ => panic!("unexpected SLMP request: {request:?}"),
    }
}

async fn app(label: &str) -> TestApp {
    let component = std::fs::read(std::env::var("BANTO_SLMP_COMPONENT_WASM").unwrap()).unwrap();
    test_app_with_manifest(label, MANIFEST, &component).await
}

async fn connection_test(app: &TestApp, fixture: &SlmpFixture) -> Value {
    let (status, body) = admin_write(
        &app.router,
        "POST",
        "/api/plc-connections/test",
        &app.admin_token,
        test_payload(PROTOCOL, "127.0.0.1", fixture.addr.port(), false),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    body
}

type Command = (u16, Option<u8>, Option<u32>, Option<u16>);

fn commands(requests: &[Request], connection: usize) -> Vec<Command> {
    requests
        .iter()
        .filter(|request| request.connection == connection)
        .map(|request| {
            (
                request.command,
                request.device,
                request.start,
                request.count,
            )
        })
        .collect()
}

async fn multi_collection(app: &TestApp, fixture: &SlmpFixture, period_ms: i64) -> [i64; 3] {
    let settings = json!({"host":"127.0.0.1", "port":fixture.addr.port()});
    let saved = app
        .manager
        .plc_connections()
        .create_settings(ConnectionSettingsInput {
            name: "slmp-multi-line".into(),
            protocol: PROTOCOL.into(),
            enabled: true,
            simulation: false,
            settings: settings.as_object().unwrap().clone(),
            secrets: Default::default(),
        })
        .await
        .unwrap();
    let group =
        CollectionGroupService::with_registry(app.pool.clone(), app.manager.registry().clone())
            .create(group_input("slmp-multi-group", saved.id, period_ms))
            .await
            .unwrap();
    let mut ids = [0; 3];
    for (index, (name, address)) in [("first", "D0"), ("second", "D1"), ("unknown", "D100")]
        .into_iter()
        .enumerate()
    {
        ids[index] = app
            .manager
            .tags()
            .create(tag_input(name, group.id, address, "u16"))
            .await
            .unwrap()
            .id;
    }
    app.manager.rebuild().await.unwrap();
    let mut watch = app.sessions.status_watch(saved.id).unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        watch.wait_for(|status| *status == banto_broker::BrokerConnectionStatus::Connected),
    )
    .await
    .unwrap()
    .unwrap();
    ids
}

async fn wait_multi_values(app: &TestApp, ids: [i64; 3], expected: [(Option<f64>, Quality); 3]) {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let actual = ids.map(|id| {
                app.manager
                    .current_values()
                    .and_then(|values| values.get(&format!("tag:{id}")))
                    .map(|sample| (sample.value, sample.quality))
            });
            if actual == expected.map(Some) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("three separately decoded SLMP tag values");
}

fn multi_path(name: &str) -> String {
    format!("/api/v1/values/slmp-multi-line.slmp-multi-group.{name}")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_COMPONENT_WASM to the pinned SLMP component"]
async fn q_and_r_discovery_collect_read_now_reuse_and_reconnect() {
    for (scenario, label, address, value, sd_start, sd_count) in [
        (Scenario::Q, "t12-slmp-wasm-q", "D100", 88.0, 302, 1),
        (Scenario::R, "t12-slmp-wasm-r", "D31", 0xabcd as f64, 280, 2),
    ] {
        let fixture = SlmpFixture::start(scenario).await;
        let app = app(label).await;
        assert!(app.manager.registry().contains(PROTOCOL));
        assert!(app.manager.registry().contains("slmp"));
        assert!(app.manager.registry().contains("modbus-tcp"));
        let body = connection_test(&app, &fixture).await;
        assert_eq!(body["ok"], true, "{body:?}");
        assert_eq!(fixture.connection_count(), 1);
        assert_eq!(
            commands(&fixture.requests(), 1),
            vec![
                (0x0101, None, None, None),
                (0x0401, Some(0xa9), Some(sd_start), Some(sd_count)),
                (0x0401, Some(0xa8), Some(0), Some(1)),
            ],
        );
        let expected_subcommand = if matches!(scenario, Scenario::Q) {
            0
        } else {
            2
        };
        assert!(fixture
            .requests()
            .iter()
            .filter(|request| request.command == 0x0401)
            .all(|request| request.subcommand == expected_subcommand));

        let settings = json!({"host":"127.0.0.1", "port":fixture.addr.port()});
        let saved = app
            .manager
            .plc_connections()
            .create_settings(ConnectionSettingsInput {
                name: "slmp-wasm-line".into(),
                protocol: PROTOCOL.into(),
                enabled: true,
                simulation: false,
                settings: settings.as_object().unwrap().clone(),
                secrets: Default::default(),
            })
            .await
            .unwrap();
        let group =
            CollectionGroupService::with_registry(app.pool.clone(), app.manager.registry().clone())
                .create(group_input("slmp-wasm-group", saved.id, 100))
                .await
                .unwrap();
        let tag = app
            .manager
            .tags()
            .create(tag_input("word", group.id, address, "u16"))
            .await
            .unwrap();
        assert_eq!(tag.address, address);
        app.manager.rebuild().await.unwrap();
        let mut watch = app.sessions.status_watch(saved.id).unwrap();
        tokio::time::timeout(
            Duration::from_secs(5),
            watch.wait_for(|status| *status == banto_broker::BrokerConnectionStatus::Connected),
        )
        .await
        .unwrap()
        .unwrap();
        tokio::time::timeout(Duration::from_secs(6), async {
            loop {
                let key = format!("tag:{}", tag.id);
                let actual = app
                    .manager
                    .current_values()
                    .and_then(|values| values.get(&key))
                    .and_then(|value| value.value);
                if actual == Some(value) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let path = "/api/v1/values/slmp-wasm-line.slmp-wasm-group.word";
        let (status, body) = admin_get(&app.router, path, &app.admin_token).await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(body["v"], value);
        assert_eq!(body["q"], "good");
        let (status, body) =
            admin_get(&app.router, &format!("{path}/read-now"), &app.admin_token).await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(body["v"], value);
        assert_eq!(body["q"], "good");

        let mut matching = test_payload(PROTOCOL, "127.0.0.1", fixture.addr.port(), false);
        matching["connectionId"] = json!(saved.id);
        let before = fixture.connection_count();
        let (status, body) = admin_write(
            &app.router,
            "POST",
            "/api/plc-connections/test",
            &app.admin_token,
            matching,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(body["ok"], true, "{body:?}");
        assert_eq!(
            fixture.connection_count(),
            before,
            "broker session must be reused"
        );
        assert!(fixture
            .requests()
            .iter()
            .any(|event| event.connection == before
                && event.device == Some(0xa8)
                && event.start
                    == Some(if matches!(scenario, Scenario::Q) {
                        100
                    } else {
                        31
                    })));

        fixture.close_clients();
        tokio::time::timeout(Duration::from_secs(12), async {
            loop {
                let events = fixture.requests();
                if fixture.connection_count() > before
                    && events
                        .iter()
                        .any(|event| event.connection > before && event.command == 0x0101)
                    && events
                        .iter()
                        .any(|event| event.connection > before && event.device == Some(0xa9))
                    && events.iter().any(|event| {
                        event.connection > before
                            && event.device == Some(0xa8)
                            && event.start
                                == Some(if matches!(scenario, Scenario::Q) {
                                    100
                                } else {
                                    31
                                })
                    })
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("broker reconnect and fresh discovery");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_COMPONENT_WASM to the pinned SLMP component"]
async fn unknown_model_and_type_refusal_stop_before_sd_or_tag_read() {
    for (scenario, label) in [
        (Scenario::UnknownModel, "t12-slmp-wasm-unknown"),
        (Scenario::TypeRefused, "t12-slmp-wasm-type-refused"),
    ] {
        let fixture = SlmpFixture::start(scenario).await;
        let app = app(label).await;
        let body = connection_test(&app, &fixture).await;
        assert_eq!(body["ok"], false, "{body:?}");
        assert_eq!(body["error"]["kind"], "protocol", "{body:?}");
        assert_eq!(
            commands(&fixture.requests(), 1),
            vec![(0x0101, None, None, None)]
        );
        // handshake-next completed, but the guest cannot choose Q/R 0401 framing.
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_COMPONENT_WASM to the pinned Q25PRH-capable SLMP component"]
async fn q25prh_unknown_range_collects_individual_values_and_read_now() {
    let component = std::fs::read(std::env::var("BANTO_SLMP_COMPONENT_WASM").unwrap()).unwrap();
    let driver = WasmDriver::load(MANIFEST, &component).unwrap();
    let settings = Settings::from_pairs([("host", "127.0.0.1"), ("port", "5000")]).unwrap();
    let mut connection = driver.create_connection(&settings).unwrap();
    let type_step = connection.handshake_next(None).unwrap().unwrap();
    assert_eq!(Request::from_bytes(1, &type_step.send).command, 0x0101);
    let type_response = wire_response(&type_step.send, 0, &model("Q25PRHCPU", 0x004c), false);
    assert!(
        connection
            .handshake_next(Some(&type_response))
            .unwrap()
            .is_none(),
        "Q25PRHCPU must not use the standard-Q SD range map"
    );
    let kind = TagKind::numeric(DataType::U16);
    let specs = ["D3000", "D3001"]
        .map(|address| TagSpec::new(driver.parse_address(address, &kind).unwrap(), kind.clone()));
    let planned = connection.plan_read(&specs).unwrap();
    assert_eq!(
        planned.len(),
        2,
        "adjacent tags with Unknown range stay separate"
    );
    for (index, plan) in planned.iter().enumerate() {
        assert_eq!(plan.covers.as_slice(), &[index as u32]);
        let request = Request::from_bytes(1, &plan.step.send);
        assert_eq!(request.device, Some(0xa8));
        assert_eq!(request.start, Some(3000 + index as u32));
        assert_eq!(request.count, Some(1));
        assert_eq!(request.subcommand, 0, "Q word wire");
    }

    let fixture = SlmpFixture::start(Scenario::Q25Prh).await;
    let app = app("t12-slmp-wasm-q25prh").await;
    let settings = json!({"host":"127.0.0.1", "port":fixture.addr.port()});
    let saved = app
        .manager
        .plc_connections()
        .create_settings(ConnectionSettingsInput {
            name: "q25-line".into(),
            protocol: PROTOCOL.into(),
            enabled: true,
            simulation: false,
            settings: settings.as_object().unwrap().clone(),
            secrets: Default::default(),
        })
        .await
        .unwrap();
    let group =
        CollectionGroupService::with_registry(app.pool.clone(), app.manager.registry().clone())
            .create(group_input("q25-group", saved.id, 1_000))
            .await
            .unwrap();
    let mut ids = [0; 2];
    for (index, (name, address)) in [("first", "D3000"), ("second", "D3001")]
        .into_iter()
        .enumerate()
    {
        ids[index] = app
            .manager
            .tags()
            .create(tag_input(name, group.id, address, "u16"))
            .await
            .unwrap()
            .id;
    }
    app.manager.rebuild().await.unwrap();
    let mut watch = app.sessions.status_watch(saved.id).unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        watch.wait_for(|status| *status == banto_broker::BrokerConnectionStatus::Connected),
    )
    .await
    .unwrap()
    .unwrap();
    let collected = tokio::time::timeout(Duration::from_secs(6), async {
        loop {
            let values = ids.map(|id| {
                app.manager
                    .current_values()
                    .and_then(|values| values.get(&format!("tag:{id}")))
                    .map(|sample| (sample.value, sample.quality))
            });
            if values
                == [
                    Some((Some(1_000.0), Quality::Good)),
                    Some((Some(1_001.0), Quality::Good)),
                ]
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(
        collected.is_ok(),
        "Q25PRH collection: requests={:?}, values={:?}",
        fixture.requests(),
        ids.map(|id| app
            .manager
            .current_values()
            .and_then(|values| values.get(&format!("tag:{id}"))))
    );
    let path = "/api/v1/values/q25-line.q25-group.second";
    let (status, cached) = admin_get(&app.router, path, &app.admin_token).await;
    assert_eq!(status, StatusCode::OK, "{cached:?}");
    assert_eq!(cached["v"], 1_001.0);
    assert_eq!(cached["q"], "good");
    // Stop the periodic collector, not the broker session. Wait for any
    // in-flight poll to finish before inspecting the request sequence.
    app.manager.shutdown().await;
    assert!(app.sessions.handle_for(saved.id).is_some());
    assert_eq!(fixture.connection_count(), 1);
    let events = fixture.requests();
    let expected_poll = [
        (0x0401, Some(0xa8), Some(3000), Some(1)),
        (0x0401, Some(0xa8), Some(3001), Some(1)),
    ];
    let observed = commands(&events, 1);
    let (polls, remainder) = observed[1..].as_chunks::<2>();
    assert_eq!(observed.first(), Some(&(0x0101, None, None, None)));
    assert!(
        observed.len() >= 3
            && polls.iter().all(|pair| pair == &expected_poll)
            && remainder.is_empty(),
        "each periodic collection must read both tags separately without SD: {observed:?}"
    );
    assert!(events[1..].iter().all(|request| request.subcommand == 0));

    let before_read_now = fixture.requests().len();
    fixture.set_version(1);
    let (status, fresh) =
        admin_get(&app.router, &format!("{path}/read-now"), &app.admin_token).await;
    assert_eq!(status, StatusCode::OK, "{fresh:?}");
    assert_eq!(fresh["v"], 1_101.0);
    assert_eq!(fresh["q"], "good");
    assert_eq!(
        fixture.connection_count(),
        1,
        "read-now must reuse the broker"
    );
    let events = fixture.requests();
    assert_eq!(events.len(), before_read_now + 1);
    assert_eq!(
        (
            events.last().unwrap().device,
            events.last().unwrap().start,
            events.last().unwrap().count
        ),
        (Some(0xa8), Some(3001), Some(1))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_COMPONENT_WASM to the pinned Q25PRH-capable SLMP component"]
async fn q25prh_mismatched_name_or_code_suppresses_sd_and_d_reads() {
    for (scenario, label) in [
        (Scenario::Q25PrhWrongName, "t12-slmp-wasm-q25-wrong-name"),
        (Scenario::Q25PrhWrongCode, "t12-slmp-wasm-q25-wrong-code"),
    ] {
        let fixture = SlmpFixture::start(scenario).await;
        let app = app(label).await;
        let body = connection_test(&app, &fixture).await;
        assert_eq!(body["ok"], false, "{scenario:?}: {body:?}");
        assert_eq!(body["error"]["kind"], "protocol", "{body:?}");
        assert_eq!(
            commands(&fixture.requests(), 1),
            [(0x0101, None, None, None)]
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_COMPONENT_WASM to the pinned SLMP component"]
async fn sd_refusal_keeps_range_unknown_but_explicit_read_runs() {
    let fixture = SlmpFixture::start(Scenario::SdRefused).await;
    let app = app("t12-slmp-wasm-sd-refused").await;
    let body = connection_test(&app, &fixture).await;
    assert_eq!(body["ok"], true, "{body:?}");
    assert_eq!(
        commands(&fixture.requests(), 1),
        vec![
            (0x0101, None, None, None),
            (0x0401, Some(0xa9), Some(302), Some(1)),
            (0x0401, Some(0xa8), Some(0), Some(1)),
        ]
    );
    // Current WIT has no structured way to expose this Unknown reason to Hub.
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_COMPONENT_WASM to the pinned SLMP component"]
async fn device_refusal_differs_from_framing_fatal_and_r_outside_suppresses_send() {
    for (scenario, label, kind, expected_commands) in [
        (
            Scenario::ReadRefused,
            "t12-slmp-wasm-device-refused",
            "device",
            3,
        ),
        (
            Scenario::ReadFraming,
            "t12-slmp-wasm-read-framing",
            "protocol",
            3,
        ),
        (
            Scenario::TypeFraming,
            "t12-slmp-wasm-type-framing",
            "protocol",
            1,
        ),
        // Uncovered R addresses become a per-item Unsupported in the Host.
        (Scenario::RZero, "t12-slmp-wasm-r-outside", "device", 2),
    ] {
        let fixture = SlmpFixture::start(scenario).await;
        let app = app(label).await;
        let body = connection_test(&app, &fixture).await;
        assert_eq!(body["ok"], false, "{scenario:?}: {body:?}");
        assert_eq!(body["error"]["kind"], kind, "{scenario:?}: {body:?}");
        let events = commands(&fixture.requests(), 1);
        assert_eq!(events.len(), expected_commands, "{scenario:?}: {events:?}");
        if matches!(scenario, Scenario::RZero) {
            assert_eq!(events[1], (0x0401, Some(0xa9), Some(280), Some(2)));
            assert!(!fixture
                .requests()
                .iter()
                .any(|event| event.device == Some(0xa8)));
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_COMPONENT_WASM to the pinned multi-tag SLMP component"]
async fn multiple_tags_collect_and_read_now_returns_a_fresh_value() {
    let fixture = SlmpFixture::start(Scenario::Multi).await;
    let app = app("t12-slmp-wasm-multi-fresh").await;
    // A long period keeps the cached sample fixed while read-now uses the broker.
    let ids = multi_collection(&app, &fixture, 10_000).await;
    wait_multi_values(
        &app,
        ids,
        [
            (Some(1_000.0), Quality::Good),
            (Some(1_001.0), Quality::Good),
            (Some(1_100.0), Quality::Good),
        ],
    )
    .await;
    assert_eq!(fixture.connection_count(), 1);
    let events = fixture.requests();
    assert!(events.len() >= 4, "{events:?}");
    assert_eq!(
        commands(&events, 1)[..4],
        [
            (0x0101, None, None, None),
            (0x0401, Some(0xa9), Some(302), Some(1)),
            (0x0401, Some(0xa8), Some(0), Some(2)),
            (0x0401, Some(0xa8), Some(100), Some(1)),
        ]
    );
    for (address, count) in [(0, 2), (100, 1)] {
        assert!(events[2..].iter().any(|request| {
            request.connection == 1
                && request.device == Some(0xa8)
                && request.start == Some(address)
                && request.count == Some(count)
                && request.subcommand == 0
        }));
    }
    for (name, value) in [
        ("first", 1_000.0),
        ("second", 1_001.0),
        ("unknown", 1_100.0),
    ] {
        let (status, body) = admin_get(&app.router, &multi_path(name), &app.admin_token).await;
        assert_eq!(status, StatusCode::OK, "{name}: {body:?}");
        assert_eq!(body["v"], value, "{name}: {body:?}");
        assert_eq!(body["q"], "good", "{name}: {body:?}");
    }

    fixture.set_version(1);
    let before = fixture.requests().len();
    let path = multi_path("second");
    let (status, body) =
        admin_get(&app.router, &format!("{path}/read-now"), &app.admin_token).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(
        body["v"], 1_101.0,
        "read-now must not return the cached 1001"
    );
    assert_eq!(body["q"], "good");
    assert_eq!(fixture.connection_count(), 1, "read-now reuses the broker");
    let later = fixture.requests();
    assert_eq!(
        later.len(),
        before + 1,
        "read-now must issue one fresh read"
    );
    assert_eq!(later[before].device, Some(0xa8));
    assert_eq!(later[before].start, Some(1));
    let (status, cached) = admin_get(&app.router, &path, &app.admin_token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cached["v"], 1_001.0, "read-now must not alter the cache");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_COMPONENT_WASM to the pinned multi-tag SLMP component"]
async fn one_device_refusal_marks_only_its_tag_bad() {
    let fixture = SlmpFixture::start(Scenario::MultiItemRefusal).await;
    let app = app("t12-slmp-wasm-multi-refusal").await;
    let ids = multi_collection(&app, &fixture, 1_000).await;
    wait_multi_values(
        &app,
        ids,
        [
            (Some(1_000.0), Quality::Good),
            (Some(1_001.0), Quality::Good),
            (None, Quality::Bad),
        ],
    )
    .await;
    assert_eq!(fixture.connection_count(), 1, "item refusal is not fatal");
    let events = fixture.requests();
    assert!(events.iter().any(|event| {
        event.device == Some(0xa8) && event.start == Some(0) && event.count == Some(2)
    }));
    assert!(events.iter().any(|event| event.start == Some(100)));
    for (name, expected) in [
        ("first", Some(1_000.0)),
        ("second", Some(1_001.0)),
        ("unknown", None),
    ] {
        let (status, body) = admin_get(&app.router, &multi_path(name), &app.admin_token).await;
        assert_eq!(status, StatusCode::OK, "{name}: {body:?}");
        assert_eq!(body["v"].as_f64(), expected, "{name}: {body:?}");
        assert_eq!(body["q"], if expected.is_some() { "good" } else { "bad" });
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_COMPONENT_WASM to the pinned batch-capable SLMP component"]
async fn batch_device_refusal_marks_its_covers_bad_but_not_unknown_individual() {
    let fixture = SlmpFixture::start(Scenario::BatchRefusal).await;
    let app = app("t12-slmp-wasm-batch-refusal").await;
    let ids = multi_collection(&app, &fixture, 1_000).await;
    wait_multi_values(
        &app,
        ids,
        [
            (None, Quality::Bad),
            (None, Quality::Bad),
            (Some(1_100.0), Quality::Good),
        ],
    )
    .await;
    assert_eq!(fixture.connection_count(), 1, "device refusal is not fatal");
    let events = fixture.requests();
    assert!(events.iter().any(|event| {
        event.device == Some(0xa8) && event.start == Some(0) && event.count == Some(2)
    }));
    assert!(events.iter().any(|event| {
        event.device == Some(0xa8) && event.start == Some(100) && event.count == Some(1)
    }));
    for (name, expected) in [
        ("first", None),
        ("second", None),
        ("unknown", Some(1_100.0)),
    ] {
        let (status, body) = admin_get(&app.router, &multi_path(name), &app.admin_token).await;
        assert_eq!(status, StatusCode::OK, "{name}: {body:?}");
        assert_eq!(body["v"].as_f64(), expected, "{name}: {body:?}");
        assert_eq!(body["q"], if expected.is_some() { "good" } else { "bad" });
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_COMPONENT_WASM to the pinned multi-tag SLMP component"]
async fn reconnect_rediscovers_and_refreshes_all_tags_on_one_new_connection() {
    let fixture = SlmpFixture::start(Scenario::Multi).await;
    let app = app("t12-slmp-wasm-multi-reconnect").await;
    let ids = multi_collection(&app, &fixture, 500).await;
    wait_multi_values(
        &app,
        ids,
        [
            (Some(1_000.0), Quality::Good),
            (Some(1_001.0), Quality::Good),
            (Some(1_100.0), Quality::Good),
        ],
    )
    .await;
    fixture.close_clients();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if fixture.connection_count() >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("broker reconnects after socket close");
    wait_multi_values(
        &app,
        ids,
        [
            (Some(2_000.0), Quality::Good),
            (Some(2_001.0), Quality::Good),
            (Some(2_100.0), Quality::Good),
        ],
    )
    .await;
    let events: Vec<_> = fixture
        .requests()
        .into_iter()
        .filter(|request| request.connection == 2)
        .collect();
    assert!(events.len() >= 4, "{events:?}");
    assert_eq!(
        commands(&events, 2)[..4],
        [
            (0x0101, None, None, None),
            (0x0401, Some(0xa9), Some(302), Some(1)),
            (0x0401, Some(0xa8), Some(0), Some(2)),
            (0x0401, Some(0xa8), Some(100), Some(1)),
        ],
        "new physical connection must discover before reading tags"
    );
    for (address, count) in [(0, 2), (100, 1)] {
        assert!(events[2..].iter().any(|request| {
            request.device == Some(0xa8)
                && request.start == Some(address)
                && request.count == Some(count)
                && request.subcommand == 0
        }));
    }
}

#[test]
#[ignore = "set BANTO_SLMP_COMPONENT_WASM to the pinned SLMP component"]
fn noncontiguous_tags_plan_separate_requests_and_keep_result_indices() {
    let component = std::fs::read(std::env::var("BANTO_SLMP_COMPONENT_WASM").unwrap()).unwrap();
    let driver = WasmDriver::load(MANIFEST, &component).unwrap();
    let settings = Settings::from_pairs([("host", "127.0.0.1"), ("port", "5000")]).unwrap();
    let mut connection = driver.create_connection(&settings).unwrap();
    let type_step = connection.handshake_next(None).unwrap().unwrap();
    let model = wire_response(&type_step.send, 0, &model("Q02UCPU", 0x0263), false);
    let sd_step = connection.handshake_next(Some(&model)).unwrap().unwrap();
    let count = wire_response(&sd_step.send, 0, &100u16.to_le_bytes(), false);
    assert!(connection.handshake_next(Some(&count)).unwrap().is_none());
    let kind = TagKind::numeric(DataType::U16);
    // The gaps and Unknown boundary require one request per input index.
    let specs = ["D1", "D3", "D100"]
        .map(|text| TagSpec::new(driver.parse_address(text, &kind).unwrap(), kind.clone()));
    let planned = connection.plan_read(&specs).unwrap();
    assert_eq!(planned.len(), 3, "one D/u16 request per tag");
    let mut seen = [false; 3];
    for item in planned {
        assert_eq!(
            item.covers.len(),
            1,
            "no known/Unknown mixing in one request"
        );
        let index = item.covers[0] as usize;
        assert!(index < specs.len());
        assert!(!seen[index], "duplicate result index {index}");
        seen[index] = true;
        let request = Request::from_bytes(1, &item.step.send);
        assert_eq!(request.device, Some(0xa8));
        assert_eq!(request.start, Some([1, 3, 100][index]));
        assert_eq!(request.count, Some(1));
        let response = if index == 0 {
            wire_response(&item.step.send, 0xc05b, &[], false)
        } else {
            wire_response(&item.step.send, 0, &[21, 0], false)
        };
        let decoded = connection.decode_read(&item, &specs, &response).unwrap();
        assert_eq!(decoded.len(), 1);
        if index == 0 {
            assert!(matches!(&decoded[0], Err(ItemError::Device(error)) if error.code == 0xc05b));
        } else {
            assert_eq!(decoded[0], Ok(RawValue::Uint(21)));
        }
    }
    assert_eq!(seen, [true; 3]);
}

#[test]
#[ignore = "set BANTO_SLMP_COMPONENT_WASM to the pinned batch-capable SLMP component"]
fn confirmed_contiguous_words_batch_without_filling_gaps_or_mixing_unknown() {
    let component = std::fs::read(std::env::var("BANTO_SLMP_COMPONENT_WASM").unwrap()).unwrap();
    let driver = WasmDriver::load(MANIFEST, &component).unwrap();
    let settings = Settings::from_pairs([("host", "127.0.0.1"), ("port", "5000")]).unwrap();
    let mut connection = driver.create_connection(&settings).unwrap();
    let type_step = connection.handshake_next(None).unwrap().unwrap();
    let model = wire_response(&type_step.send, 0, &model("Q02UCPU", 0x0263), false);
    let sd_step = connection.handshake_next(Some(&model)).unwrap().unwrap();
    let count = wire_response(&sd_step.send, 0, &100u16.to_le_bytes(), false);
    assert!(connection.handshake_next(Some(&count)).unwrap().is_none());

    let kind = TagKind::numeric(DataType::U16);
    // D2 is deliberately absent; D100 is outside the confirmed 0..100 range.
    // The batch is not at index zero, so covers must map back to input tags.
    let addresses: [u32; 5] = [3, 0, 1, 99, 100];
    let specs = addresses.map(|address| {
        TagSpec::new(
            driver.parse_address(&format!("D{address}"), &kind).unwrap(),
            kind.clone(),
        )
    });
    let planned = connection.plan_read(&specs).unwrap();
    assert_eq!(planned.len(), 4, "only D0-D1 may share one request");
    let mut seen = [false; 5];
    for item in planned {
        let request = Request::from_bytes(1, &item.step.send);
        assert_eq!(request.device, Some(0xa8));
        let (start, count, covers): (u32, u16, &[u32]) = match (request.start, request.count) {
            (Some(0), Some(2)) => (0, 2, &[1, 2]),
            (Some(3), Some(1)) => (3, 1, &[0]),
            (Some(99), Some(1)) => (99, 1, &[3]),
            (Some(100), Some(1)) => (100, 1, &[4]),
            other => panic!("gap, boundary or Unknown was batched: {other:?}"),
        };
        assert_eq!(item.covers.as_slice(), covers);
        let mut body = Vec::new();
        for address in start..start + u32::from(count) {
            body.extend_from_slice(&u16::try_from(200 + address).unwrap().to_le_bytes());
        }
        let response = wire_response(&item.step.send, 0, &body, false);
        let decoded = connection.decode_read(&item, &specs, &response).unwrap();
        assert_eq!(decoded.len(), covers.len());
        for (&index, value) in item.covers.iter().zip(decoded) {
            let index = index as usize;
            assert!(!seen[index], "duplicate result index {index}");
            seen[index] = true;
            assert_eq!(value, Ok(RawValue::Uint(u64::from(200 + addresses[index]))));
        }
    }
    assert_eq!(seen, [true; 5]);
}

#[test]
#[ignore = "set BANTO_SLMP_COMPONENT_WASM to the pinned batch-capable SLMP component"]
fn confirmed_run_splits_at_the_960_word_request_limit() {
    let component = std::fs::read(std::env::var("BANTO_SLMP_COMPONENT_WASM").unwrap()).unwrap();
    let driver = WasmDriver::load(MANIFEST, &component).unwrap();
    let settings = Settings::from_pairs([("host", "127.0.0.1"), ("port", "5000")]).unwrap();
    let mut connection = driver.create_connection(&settings).unwrap();
    let type_step = connection.handshake_next(None).unwrap().unwrap();
    let model = wire_response(&type_step.send, 0, &model("Q02UCPU", 0x0263), false);
    let sd_step = connection.handshake_next(Some(&model)).unwrap().unwrap();
    let count = wire_response(&sd_step.send, 0, &1000u16.to_le_bytes(), false);
    assert!(connection.handshake_next(Some(&count)).unwrap().is_none());

    let kind = TagKind::numeric(DataType::U16);
    let specs = (0..=960)
        .map(|address| {
            TagSpec::new(
                driver.parse_address(&format!("D{address}"), &kind).unwrap(),
                kind.clone(),
            )
        })
        .collect::<Vec<_>>();
    let planned = connection.plan_read(&specs).unwrap();
    assert_eq!(planned.len(), 2);
    assert_eq!(planned[0].covers.len(), 960);
    assert_eq!(planned[0].covers[0], 0);
    assert_eq!(planned[0].covers[959], 959);
    assert_eq!(planned[1].covers.as_slice(), &[960]);
    let first_request = Request::from_bytes(1, &planned[0].step.send);
    assert_eq!(
        (first_request.start, first_request.count),
        (Some(0), Some(960))
    );
    let second_request = Request::from_bytes(1, &planned[1].step.send);
    assert_eq!(
        (second_request.start, second_request.count),
        (Some(960), Some(1))
    );

    let mut words = vec![0; 960 * 2];
    words[..2].copy_from_slice(&0x1234u16.to_le_bytes());
    words[1918..].copy_from_slice(&0x5678u16.to_le_bytes());
    let response = wire_response(&planned[0].step.send, 0, &words, false);
    let decoded = connection
        .decode_read(&planned[0], &specs, &response)
        .unwrap();
    assert_eq!(decoded.len(), 960);
    assert_eq!(decoded[0], Ok(RawValue::Uint(0x1234)));
    assert_eq!(decoded[959], Ok(RawValue::Uint(0x5678)));
    let response = wire_response(&planned[1].step.send, 0, &42u16.to_le_bytes(), false);
    let decoded = connection
        .decode_read(&planned[1], &specs, &response)
        .unwrap();
    assert_eq!(decoded, vec![Ok(RawValue::Uint(42))]);
}

async fn large_frame_session(scenario: Scenario) -> (SlmpFixture, FrameSession, Vec<TagSpec>) {
    let fixture = SlmpFixture::start(scenario).await;
    let component = std::fs::read(std::env::var("BANTO_SLMP_COMPONENT_WASM").unwrap()).unwrap();
    let driver = WasmDriver::load(MANIFEST, &component).unwrap();
    let settings = Settings::from_pairs([
        ("host".to_owned(), "127.0.0.1".to_owned()),
        ("port".to_owned(), fixture.addr.port().to_string()),
    ])
    .unwrap();
    let transport = TransportSpec::Tcp {
        host: "127.0.0.1".into(),
        port: fixture.addr.port(),
    };
    let cx = ConnectContext::new(
        "slmp-large-frame",
        Duration::from_secs(2),
        Duration::from_secs(2),
    );
    let mut session = FrameSession::connect(driver.as_ref(), &settings, &transport, &cx, None)
        .await
        .unwrap();
    let kind = TagKind::numeric(DataType::U16);
    let specs = (0..960)
        .map(|address| {
            TagSpec::new(
                driver.parse_address(&format!("D{address}"), &kind).unwrap(),
                kind.clone(),
            )
        })
        .collect::<Vec<_>>();
    let planned = session.connection_mut().unwrap().plan_read(&specs).unwrap();
    assert_eq!(planned.len(), 1);
    assert_eq!(planned[0].covers.len(), 960);
    assert_eq!(planned[0].step.max_len, MAX_D_RESPONSE_LEN as u32);
    for (index, &cover) in planned[0].covers.iter().enumerate() {
        assert_eq!(cover as usize, index, "word must cover its original tag");
    }
    let request = Request::from_bytes(1, &planned[0].step.send);
    assert_eq!((request.start, request.count), (Some(0), Some(960)));
    (fixture, session, specs)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_COMPONENT_WASM to the pinned batch-capable SLMP component"]
async fn max_size_split_tcp_response_reaches_every_original_cover() {
    let (fixture, mut session, specs) = large_frame_session(Scenario::LargeSplit).await;
    let values = tokio::time::timeout(Duration::from_secs(10), session.read_raw_batch(&specs))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(values.len(), 960);
    for (index, value) in values.into_iter().enumerate() {
        assert_eq!(
            value,
            Ok(RawValue::Uint(1000 + index as u64)),
            "D{index} did not receive its own response word"
        );
    }
    assert_eq!(fixture.connection_count(), 1);
    assert_eq!(
        commands(&fixture.requests(), 1),
        [
            (0x0101, None, None, None),
            (0x0401, Some(0xa9), Some(302), Some(1)),
            (0x0401, Some(0xa8), Some(0), Some(960)),
        ]
    );
    assert!(session.is_connected());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_COMPONENT_WASM to the pinned batch-capable SLMP component"]
async fn short_or_overlong_max_size_tcp_frame_is_fatal_without_partial_values() {
    for scenario in [Scenario::LargeShort, Scenario::LargeOverlong] {
        let (fixture, mut session, specs) = large_frame_session(scenario).await;
        let error = tokio::time::timeout(Duration::from_secs(10), session.read_raw_batch(&specs))
            .await
            .unwrap()
            .unwrap_err();
        assert!(
            match scenario {
                Scenario::LargeShort => matches!(error, FatalError::Io(_)),
                Scenario::LargeOverlong => matches!(error, FatalError::Framing(_)),
                _ => unreachable!(),
            },
            "{scenario:?}: {error:?}"
        );
        assert!(!session.is_connected(), "{scenario:?} must close session");
        assert_eq!(fixture.connection_count(), 1);
        assert!(fixture.requests().iter().any(|request| {
            request.device == Some(0xa8) && request.start == Some(0) && request.count == Some(960)
        }));
    }
}
