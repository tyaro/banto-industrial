//! Normal D/u16 policy acceptance through an isolated loopback SLMP peer.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use banto_collect::current::Quality;
use banto_driver_api::{
    ConnectContext, DataType, DriverBase, FatalError, ItemError, Session as _, Settings, TagKind,
    TagSpec, TagVerdict, Value, WriteItem,
};
use banto_driver_engine::{transport::TransportSpec, FrameSession};
use banto_driver_wasm::WasmDriver;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const MANIFEST: &[u8] = include_bytes!("manifest.json");
const COMPONENT_ENV: &str = "BANTO_SLMP_OPERATIONAL_WRITE_COMPONENT_WASM";
const DIAGNOSTIC_COMPONENT_ENV: &str = "BANTO_SLMP_DIAGNOSTICS_COMPONENT_WASM";
type RecordedFrames = Arc<Mutex<Vec<(u32, Vec<u16>)>>>;
type RecordedBits = Arc<Mutex<Vec<(u32, u8, Vec<bool>)>>>;
type RecordedReads = Arc<Mutex<Vec<(u8, u32, u16)>>>;

#[derive(Clone, Copy)]
enum Peer {
    R,
    Q,
    StandardQ,
}

struct Fixture {
    address: std::net::SocketAddr,
    frames: RecordedFrames,
    commands: Arc<Mutex<Vec<(u16, u32, u16)>>>,
    read_spans: RecordedReads,
    count: Arc<AtomicU32>,
    diagnostic_bit_counts: Arc<Mutex<[u32; 3]>>,
    refuse_bit_sd: Arc<AtomicBool>,
    write_devices: Arc<Mutex<Vec<u8>>>,
    bit_frames: RecordedBits,
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
        let read_spans: RecordedReads = Arc::new(Mutex::new(Vec::new()));
        let reads_on_wire = read_spans.clone();
        let live_count = Arc::new(AtomicU32::new(count));
        let count_on_wire = live_count.clone();
        let diagnostic_bit_counts = Arc::new(Mutex::new([0u32; 3]));
        let bit_counts_on_wire = diagnostic_bit_counts.clone();
        let refuse_bit_sd = Arc::new(AtomicBool::new(false));
        let refuse_bit_sd_on_wire = refuse_bit_sd.clone();
        let write_devices = Arc::new(Mutex::new(Vec::new()));
        let devices = write_devices.clone();
        let bit_frames: RecordedBits = Arc::new(Mutex::new(Vec::new()));
        let bits_written = bit_frames.clone();
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
                    let count = count_on_wire.load(Ordering::SeqCst);
                    let subcommand = u16::from_le_bytes([request[17], request[18]]);
                    let start = if command == 0x0101 {
                        0
                    } else {
                        u32::from_le_bytes([
                            request[19],
                            request[20],
                            request[21],
                            if matches!(subcommand, 2 | 3) {
                                request[22]
                            } else {
                                0
                            },
                        ])
                    };
                    events.lock().unwrap().push((command, start, subcommand));
                    let (end_code, body): (u16, Vec<u8>) = match command {
                        0x0101 => {
                            let (name, code) = match peer {
                                Peer::R => ("R04CPU", 0x4800u16),
                                Peer::Q => ("Q25PRHCPU", 0x004cu16),
                                Peer::StandardQ => ("Q02UCPU", 0x0263u16),
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
                                    if (subcommand, start, words) == (2, 262, 14) {
                                        if refuse_bit_sd_on_wire.load(Ordering::SeqCst) {
                                            (0xc051, Vec::new())
                                        } else {
                                            let mut body = vec![0; 28];
                                            for at in [0, 2, 4, 12] {
                                                body[at * 2..at * 2 + 4]
                                                    .copy_from_slice(&64u32.to_le_bytes());
                                            }
                                            let [_, sb, f] = *bit_counts_on_wire.lock().unwrap();
                                            body[6 * 2..8 * 2].copy_from_slice(&sb.to_le_bytes());
                                            body[8 * 2..10 * 2].copy_from_slice(&f.to_le_bytes());
                                            (0, body)
                                        }
                                    } else {
                                        assert_eq!((subcommand, start, words), (2, 280, 4));
                                        let mut body = count.to_le_bytes().to_vec();
                                        body.extend_from_slice(&count.to_le_bytes());
                                        (0, body)
                                    }
                                }
                                Peer::Q => {
                                    assert_eq!((subcommand, start), (0, 302));
                                    assert!(matches!(words, 1 | 2));
                                    let mut body =
                                        u16::try_from(count).unwrap().to_le_bytes().to_vec();
                                    if words == 2 {
                                        body.extend_from_slice(&64u16.to_le_bytes());
                                    }
                                    (0, body)
                                }
                                Peer::StandardQ => {
                                    if (subcommand, start, words) == (0, 290, 5) {
                                        if refuse_bit_sd_on_wire.load(Ordering::SeqCst) {
                                            (0xc051, Vec::new())
                                        } else {
                                            let [x, _, _] = *bit_counts_on_wire.lock().unwrap();
                                            let body = [u16::try_from(x).unwrap(), 64, 64, 64, 64]
                                                .into_iter()
                                                .flat_map(u16::to_le_bytes)
                                                .collect();
                                            (0, body)
                                        }
                                    } else {
                                        assert_eq!((subcommand, start, words), (0, 302, 2));
                                        let mut body =
                                            u16::try_from(count).unwrap().to_le_bytes().to_vec();
                                        body.extend_from_slice(&64u16.to_le_bytes());
                                        (0, body)
                                    }
                                }
                            }
                        }
                        0x0401 => {
                            let is_r = matches!(subcommand, 2 | 3);
                            let offset = if is_r { 25 } else { 23 };
                            let words = usize::from(u16::from_le_bytes([
                                request[offset],
                                request[offset + 1],
                            ]));
                            let device = request[if is_r { 23 } else { 22 }];
                            reads_on_wire.lock().unwrap().push((
                                device,
                                start,
                                u16::try_from(words).unwrap(),
                            ));
                            let end = start.checked_add(u32::try_from(words).unwrap()).unwrap();
                            assert!(
                                words > 0
                                    && ((device == 0xa8
                                        && ((start == 10 && end == 11)
                                            || (start == 3000 && end == 3001)
                                            || (start >= 3010 && end <= 3015)))
                                        || (device == 0xb4 && start >= 0x1a && end <= 0x20)
                                        || (device == 0x90 && start >= 10 && end <= 13)
                                        || (device == 0x9c
                                            && matches!(peer, Peer::StandardQ)
                                            && end <= 32768)
                                        || (device == 0xa1
                                            && matches!(peer, Peer::R)
                                            && end <= 65538)
                                        || (device == 0x93 && matches!(peer, Peer::R) && end <= 3))
                            );
                            if matches!(device, 0x90 | 0x9c | 0xa1 | 0x93) {
                                assert_eq!(subcommand, if is_r { 3 } else { 1 });
                                if device == 0x90 && start == 12 {
                                    (0xc051, Vec::new())
                                } else {
                                    let bits = (start..end)
                                        .map(|index| index == 10 || index == 1 || index == 65537)
                                        .collect::<Vec<_>>();
                                    let body = bits
                                        .chunks(2)
                                        .map(|pair| {
                                            (u8::from(pair[0]) << 4)
                                                | u8::from(pair.get(1).copied().unwrap_or(false))
                                        })
                                        .collect();
                                    (0, body)
                                }
                            } else {
                                let stored = writes.lock().unwrap().last().cloned();
                                let values: Vec<u16> = (start..end)
                                    .map(|index| {
                                        if device == 0xa8 && index == 10 {
                                            return 0x8000;
                                        }
                                        stored
                                            .as_ref()
                                            .and_then(|(first, data)| {
                                                index.checked_sub(*first).and_then(|offset| {
                                                    data.get(usize::try_from(offset).unwrap())
                                                })
                                            })
                                            .copied()
                                            .unwrap_or(7)
                                    })
                                    .collect();
                                (
                                    0,
                                    values
                                        .iter()
                                        .flat_map(|value| value.to_le_bytes())
                                        .collect(),
                                )
                            }
                        }
                        0x1401 => {
                            let offset = if matches!(subcommand, 2 | 3) { 25 } else { 23 };
                            let device = request[if matches!(subcommand, 2 | 3) { 23 } else { 22 }];
                            let is_bit = matches!(device, 0x90 | 0x9d | 0x92 | 0xa0);
                            assert!(matches!(device, 0xa8 | 0xb4 | 0x90 | 0x9d | 0x92 | 0xa0));
                            assert_eq!(
                                subcommand,
                                if matches!(peer, Peer::R) {
                                    if is_bit {
                                        3
                                    } else {
                                        2
                                    }
                                } else {
                                    if is_bit {
                                        1
                                    } else {
                                        0
                                    }
                                }
                            );
                            let words = usize::from(u16::from_le_bytes([
                                request[offset],
                                request[offset + 1],
                            ]));
                            if is_bit {
                                assert!(words > 0 && words <= 64);
                                assert_eq!(request.len(), offset + 2 + words.div_ceil(2));
                                let values = (0..words)
                                    .map(|n| {
                                        let pair = request[offset + 2 + n / 2];
                                        if n % 2 == 0 {
                                            pair & 0x10 != 0
                                        } else {
                                            pair & 1 != 0
                                        }
                                    })
                                    .collect();
                                bits_written.lock().unwrap().push((start, device, values));
                            } else {
                                assert_eq!(request.len(), offset + 2 + words * 2);
                                assert!(
                                    (device == 0xa8 && (3010..=3013).contains(&start))
                                        || (device == 0xb4 && (0x1a..=0x1f).contains(&start))
                                );
                                let values = request[offset + 2..]
                                    .as_chunks::<2>()
                                    .0
                                    .iter()
                                    .map(|word| u16::from_le_bytes(*word))
                                    .collect();
                                writes.lock().unwrap().push((start, values));
                            }
                            devices.lock().unwrap().push(device);
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
            read_spans,
            count: live_count,
            diagnostic_bit_counts,
            refuse_bit_sd,
            write_devices,
            bit_frames,
            task,
        }
    }

    fn frames(&self) -> Vec<(u32, Vec<u16>)> {
        self.frames.lock().unwrap().clone()
    }

    fn commands(&self) -> Vec<(u16, u32, u16)> {
        self.commands.lock().unwrap().clone()
    }

    fn read_spans(&self) -> Vec<(u8, u32, u16)> {
        self.read_spans.lock().unwrap().clone()
    }

    fn set_count(&self, count: u32) {
        self.count.store(count, Ordering::SeqCst);
    }

    fn set_diagnostic_bit_counts(&self, x: u32, sb: u32, f: u32) {
        *self.diagnostic_bit_counts.lock().unwrap() = [x, sb, f];
    }

    fn set_bit_sd_refused(&self, refused: bool) {
        self.refuse_bit_sd.store(refused, Ordering::SeqCst);
    }

    fn write_devices(&self) -> Vec<u8> {
        self.write_devices.lock().unwrap().clone()
    }

    fn bit_frames(&self) -> Vec<(u32, u8, Vec<bool>)> {
        self.bit_frames.lock().unwrap().clone()
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_DIAGNOSTICS_COMPONENT_WASM to the pinned diagnostic guest"]
async fn diagnostic_guest_uses_existing_connection_and_preserves_read() {
    let bytes = std::fs::read(
        std::env::var(DIAGNOSTIC_COMPONENT_ENV).expect("set diagnostic component path"),
    )
    .unwrap();
    let driver = WasmDriver::load_operational_write(MANIFEST, &bytes).unwrap();
    let fixture = Fixture::start(Peer::R, 4000, false).await;
    let transport = TransportSpec::Tcp {
        host: "127.0.0.1".into(),
        port: fixture.address.port(),
    };
    let context = ConnectContext::new(
        "slmp-wasm-operational-write",
        Duration::from_millis(500),
        Duration::from_millis(500),
    );
    let mut session = FrameSession::connect(
        driver.as_ref(),
        &settings(fixture.address.port(), "disabled", ""),
        &transport,
        &context,
        None,
    )
    .await
    .unwrap();
    let kind = TagKind::numeric(DataType::U16);
    let bit = TagKind::numeric(DataType::Bit);
    let specs = [
        TagSpec::new(driver.parse_address("D3010", &kind).unwrap(), kind.clone()),
        TagSpec::new(driver.parse_address("D4000", &kind).unwrap(), kind.clone()),
        TagSpec::new(driver.parse_address("M10", &bit).unwrap(), bit),
    ];
    let report = session
        .diagnose_batch(&specs)
        .await
        .unwrap()
        .expect("diagnostics export");
    assert_eq!(report.model.as_ref().unwrap().name, "R04CPU");
    assert!(matches!(report.tags[0].verdict, TagVerdict::Inside(_)));
    assert!(matches!(report.tags[1].verdict, TagVerdict::Outside(_)));
    assert!(matches!(report.tags[2].verdict, TagVerdict::Inside(_)));
    assert!(fixture
        .commands()
        .iter()
        .any(|(command, start, _)| *command == 0x0401 && *start == 262));
    session.accept_diagnostic(&specs, &report);
    let values = session.read_batch(&specs[..1]).await.unwrap();
    assert_eq!(values.len(), 1);
    assert!(values[0].is_ok());
    let before = fixture.commands().len();
    let outside = session.read_batch(&specs[1..2]).await.unwrap();
    assert_eq!(outside.len(), 1);
    assert!(outside[0].is_err());
    assert_eq!(
        fixture.commands().len(),
        before,
        "outside tag must not reach the PLC"
    );
    let mixed = session.read_batch(&specs[..2]).await.unwrap();
    assert!(mixed[0].is_ok());
    assert!(mixed[1].is_err());
    assert!(fixture.commands()[before..]
        .iter()
        .all(|(_, start, _)| *start != 4000));
    fixture.set_count(2000);
    session.refresh_diagnostic().await.unwrap();
    let refreshed = session.diagnose_batch(&specs).await.unwrap().unwrap();
    assert!(matches!(refreshed.tags[0].verdict, TagVerdict::Outside(_)));
    assert_eq!(
        fixture
            .commands()
            .iter()
            .filter(|(command, _, _)| *command == 0x0101)
            .count(),
        2,
        "recheck must fetch the model on the same physical session again"
    );
    assert!(fixture.frames().is_empty());
    session.disconnect().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_DIAGNOSTICS_COMPONENT_WASM to the pinned diagnostic guest"]
async fn saved_tags_are_diagnosed_through_the_authenticated_rest_and_broker_path() {
    let bytes = std::fs::read(
        std::env::var(DIAGNOSTIC_COMPONENT_ENV).expect("set diagnostic component path"),
    )
    .unwrap();
    let fixture = Fixture::start(Peer::R, 4000, false).await;
    let app = super::test_app_with_operational_write_manifest(
        "t12-range-diagnostic-rest",
        MANIFEST,
        &bytes,
    )
    .await;
    let saved = app
        .manager
        .plc_connections()
        .create_settings(super::ConnectionSettingsInput {
            name: "diagnostic-line".into(),
            protocol: "slmp-wasm-operational-write".into(),
            enabled: true,
            simulation: false,
            settings: serde_json::json!({
                "host": "127.0.0.1", "port": fixture.address.port(),
                "d_u16_write_mode": "disabled", "d_u16_write_allowlist": ""
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
    .create(super::group_input("diagnostic-group", saved.id, 60_000))
    .await
    .unwrap();
    for index in 0..256 {
        app.manager
            .tags()
            .create(super::tag_input(
                &format!("x-{index}"),
                group.id,
                &format!("X{index:X}"),
                "bit",
            ))
            .await
            .unwrap();
    }
    // The first chunk is exclusively X bits, which do not fetch M/Y/L/B SD.
    // M10 is the very next saved tag and must not make the aggregate evidence
    // disagree with that first chunk.
    let bit = app
        .manager
        .tags()
        .create(super::tag_input("bit", group.id, "M10", "bit"))
        .await
        .unwrap();
    let inside = app
        .manager
        .tags()
        .create(super::tag_input("inside", group.id, "D3010", "u16"))
        .await
        .unwrap();
    let outside = app
        .manager
        .tags()
        .create(super::tag_input("outside", group.id, "D4000", "u16"))
        .await
        .unwrap();
    app.manager.rebuild().await.unwrap();
    let mut status = app.sessions.status_watch(saved.id).unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        status.wait_for(|state| *state == banto_broker::BrokerConnectionStatus::Connected),
    )
    .await
    .unwrap()
    .unwrap();
    let path = format!("/api/plc-connections/{}/diagnostics", saved.id);
    let (status, report) = super::admin_write(
        &app.router,
        "POST",
        &path,
        &app.admin_token,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, super::StatusCode::OK, "{report:?}");
    assert_eq!(report["status"], "current");
    assert_eq!(report["tags"].as_array().map(Vec::len), Some(259));
    assert!(report["tags"].as_array().is_some_and(|tags| {
        tags.iter()
            .any(|tag| tag["tagId"] == inside.id && tag["verdict"] == "inside")
            && tags
                .iter()
                .any(|tag| tag["tagId"] == outside.id && tag["verdict"] == "outside")
            && tags
                .iter()
                .any(|tag| tag["tagId"] == bit.id && tag["verdict"] == "inside")
    }));
    assert!(report["counts"].as_array().is_some_and(|counts| counts
        .iter()
        .any(|count| { count["area"] == "M" && count["bound"] != "unknown" })));
    let all_ids: Vec<_> = report["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tag| tag["tagId"].as_i64().unwrap())
        .collect();
    let (too_many, _) = super::admin_write(
        &app.router,
        "POST",
        &path,
        &app.admin_token,
        serde_json::json!({ "tagIds": all_ids }),
    )
    .await;
    assert_eq!(too_many, super::StatusCode::BAD_REQUEST);
    let (status, cached) = super::admin_write(
        &app.router,
        "GET",
        &path,
        &app.admin_token,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, super::StatusCode::OK);
    assert_eq!(cached["status"], "current");
    assert_eq!(cached["tags"], report["tags"]);
    fixture.set_count(2000);
    let (status, refreshed) = super::admin_write(
        &app.router,
        "POST",
        &path,
        &app.admin_token,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, super::StatusCode::OK, "{refreshed:?}");
    assert!(refreshed["tags"].as_array().is_some_and(|tags| tags
        .iter()
        .any(|tag| { tag["tagId"] == inside.id && tag["verdict"] == "outside" })));
    assert_eq!(app.sessions.connection_count(), 1);
    assert_eq!(
        fixture
            .commands()
            .iter()
            .filter(|(command, _, _)| *command == 0x0101)
            .count(),
        3,
        "every recheck must reacquire model and SD on the broker connection"
    );
    assert!(fixture.frames().is_empty());
    drop(app);
}

const NEW_GUEST_COMPONENT_ENV: &str = "BANTO_SLMP_NEW_COMPONENT_WASM";
const V_REASON: &str = "V edge relay cannot be collected reliably: a one-scan edge may be missed";

fn new_guest_component() -> Vec<u8> {
    std::fs::read(std::env::var(NEW_GUEST_COMPONENT_ENV).expect("set new SLMP guest path")).unwrap()
}

async fn connect_new_guest(peer: Peer) -> (Fixture, Arc<WasmDriver>, FrameSession) {
    let fixture = Fixture::start(peer, 4000, false).await;
    let driver = WasmDriver::load_operational_write(MANIFEST, &new_guest_component()).unwrap();
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
        &settings(fixture.address.port(), "disabled", ""),
        &transport,
        &context,
        None,
    )
    .await
    .unwrap();
    (fixture, driver, session)
}

fn bit_specs(driver: &WasmDriver, addresses: &[&str]) -> Vec<TagSpec> {
    let bit = TagKind::numeric(DataType::Bit);
    addresses
        .iter()
        .map(|address| TagSpec::new(driver.parse_address(address, &bit).unwrap(), bit.clone()))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #35 component"]
async fn new_guest_q_x_sd290_exact_bound_and_refusal_reach_hub() {
    use banto_driver_api::CountBound;

    let (fixture, driver, mut session) = connect_new_guest(Peer::StandardQ).await;
    fixture.set_diagnostic_bit_counts(16, 0, 0);
    let specs = bit_specs(&driver, &["XF", "X10"]);
    let report = session.diagnose_batch(&specs).await.unwrap().unwrap();
    let x = report
        .counts
        .iter()
        .find(|count| count.area == "X")
        .unwrap();
    assert_eq!(
        (x.unit.as_str(), x.scope.as_str(), x.source.as_str()),
        ("bit", "total", "SD290")
    );
    assert_eq!(x.bound, CountBound::Exact(16));
    assert!(matches!(report.tags[0].verdict, TagVerdict::Inside(_)));
    assert!(matches!(report.tags[1].verdict, TagVerdict::Outside(_)));
    session.accept_diagnostic(&specs, &report);
    let before = fixture.read_spans().len();
    let values = session.read_batch(&specs).await.unwrap();
    assert!(values[0].is_ok());
    assert!(values[1].is_err());
    let spans = fixture.read_spans();
    let x_spans: Vec<_> = spans[before..]
        .iter()
        .filter(|(device, _, _)| *device == 0x9c)
        .collect();
    assert!(!x_spans.is_empty());
    assert!(x_spans.iter().all(|(_, start, points)| {
        start
            .checked_add(u32::from(*points))
            .is_some_and(|end| end <= 16)
    }));

    fixture.set_diagnostic_bit_counts(0, 0, 0);
    session.refresh_diagnostic().await.unwrap();
    let zero = session.diagnose_batch(&specs).await.unwrap().unwrap();
    assert_eq!(
        zero.counts
            .iter()
            .find(|count| count.area == "X")
            .unwrap()
            .bound,
        CountBound::Exact(0)
    );
    assert!(zero
        .tags
        .iter()
        .all(|tag| matches!(tag.verdict, TagVerdict::Outside(_))));

    fixture.set_diagnostic_bit_counts(32768, 0, 0);
    session.refresh_diagnostic().await.unwrap();
    let edge = bit_specs(&driver, &["X7FFF", "X8000"]);
    let exact = session.diagnose_batch(&edge).await.unwrap().unwrap();
    assert_eq!(
        exact
            .counts
            .iter()
            .find(|count| count.area == "X")
            .unwrap()
            .bound,
        CountBound::Exact(32768)
    );
    assert!(matches!(exact.tags[0].verdict, TagVerdict::Inside(_)));
    assert!(matches!(exact.tags[1].verdict, TagVerdict::Outside(_)));

    fixture.set_bit_sd_refused(true);
    session.refresh_diagnostic().await.unwrap();
    let unknown_spec = bit_specs(&driver, &["X1"]);
    let unknown = session
        .diagnose_batch(&unknown_spec)
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(&unknown.counts.iter().find(|count| count.area == "X").unwrap().bound, CountBound::Unknown(reason) if reason == "sd-refused")
    );
    assert!(
        matches!(&unknown.tags[0].verdict, TagVerdict::Unknown(reason) if reason == "sd-refused")
    );
    session.accept_diagnostic(&unknown_spec, &unknown);
    let before = fixture.commands().len();
    assert!(session.read_batch(&unknown_spec).await.unwrap()[0].is_ok());
    assert!(
        fixture.commands().len() > before,
        "unknown must remain readable"
    );
    session.disconnect().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #35 component"]
async fn new_guest_r_sb_f_low_high_bounds_and_refusal_reach_hub() {
    use banto_driver_api::CountBound;

    let (fixture, driver, mut session) = connect_new_guest(Peer::R).await;
    fixture.set_diagnostic_bit_counts(0, 65538, 2);
    let specs = bit_specs(&driver, &["SB10001", "SB10002", "F1", "F2"]);
    let report = session.diagnose_batch(&specs).await.unwrap().unwrap();
    for (area, source, bound) in [("SB", "SD268-269", 65538), ("F", "SD270-271", 2)] {
        let count = report
            .counts
            .iter()
            .find(|count| count.area == area)
            .unwrap();
        assert_eq!(
            (
                count.unit.as_str(),
                count.scope.as_str(),
                count.source.as_str()
            ),
            ("bit", "total", source)
        );
        assert_eq!(count.bound, CountBound::Exact(bound));
    }
    for (index, expected_inside) in [(0, true), (1, false), (2, true), (3, false)] {
        assert_eq!(
            matches!(report.tags[index].verdict, TagVerdict::Inside(_)),
            expected_inside
        );
        assert_eq!(
            matches!(report.tags[index].verdict, TagVerdict::Outside(_)),
            !expected_inside
        );
    }
    session.accept_diagnostic(&specs, &report);
    let before = fixture.read_spans().len();
    let sb_values = session.read_batch(&specs[..2]).await.unwrap();
    assert!(sb_values[0].is_ok());
    assert!(sb_values[1].is_err());
    let values = session.read_batch(&specs[2..]).await.unwrap();
    assert!(values[0].is_ok());
    assert!(values[1].is_err());
    let spans = fixture.read_spans();
    let bit_spans: Vec<_> = spans[before..]
        .iter()
        .filter(|(device, _, _)| matches!(*device, 0xa1 | 0x93))
        .collect();
    assert!(bit_spans.iter().any(|(device, _, _)| *device == 0xa1));
    assert!(bit_spans.iter().any(|(device, _, _)| *device == 0x93));
    assert!(bit_spans.iter().all(|(device, start, points)| {
        start
            .checked_add(u32::from(*points))
            .is_some_and(|end| end <= if *device == 0xa1 { 65538 } else { 2 })
    }));

    fixture.set_diagnostic_bit_counts(0, 0, 0);
    session.refresh_diagnostic().await.unwrap();
    let zero_specs = bit_specs(&driver, &["SB0", "F0"]);
    let zero = session.diagnose_batch(&zero_specs).await.unwrap().unwrap();
    assert!(zero
        .tags
        .iter()
        .all(|tag| matches!(tag.verdict, TagVerdict::Outside(_))));

    fixture.set_bit_sd_refused(true);
    session.refresh_diagnostic().await.unwrap();
    let unknown = session.diagnose_batch(&zero_specs).await.unwrap().unwrap();
    assert!(unknown
        .tags
        .iter()
        .all(|tag| matches!(&tag.verdict, TagVerdict::Unknown(reason) if reason == "sd-refused")));
    session.accept_diagnostic(&zero_specs, &unknown);
    let before = fixture.commands().len();
    assert!(session
        .read_batch(&zero_specs)
        .await
        .unwrap()
        .iter()
        .all(Result::is_ok));
    assert!(
        fixture.commands().len() > before,
        "unknown must remain readable"
    );
    session.disconnect().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #35 component"]
async fn new_guest_rejects_v_and_keeps_mixed_read_covers() {
    use banto_driver_api::DriverAddress;

    let bytes = new_guest_component();
    let driver = WasmDriver::load_operational_write(MANIFEST, &bytes).unwrap();
    let bit = TagKind::numeric(DataType::Bit);
    for address in ["V0", "V14"] {
        assert_eq!(
            driver.parse_address(address, &bit).unwrap_err().message(),
            V_REASON
        );
        assert_eq!(
            driver
                .offset_address(address, &bit, 1)
                .unwrap_err()
                .message(),
            V_REASON
        );
        assert_eq!(
            driver.offset_words(address, 1).unwrap_err().message(),
            V_REASON
        );
    }
    assert!(driver
        .address_help()
        .contains("V edge relays cannot be registered"));
    let fixture = Fixture::start(Peer::R, 4000, false).await;
    let transport = TransportSpec::Tcp {
        host: "127.0.0.1".into(),
        port: fixture.address.port(),
    };
    let context = ConnectContext::new(
        "slmp-wasm-operational-write",
        Duration::from_millis(500),
        Duration::from_millis(500),
    );
    let mut session = FrameSession::connect(
        driver.as_ref(),
        &settings(fixture.address.port(), "disabled", ""),
        &transport,
        &context,
        None,
    )
    .await
    .unwrap();
    let word = TagKind::numeric(DataType::U16);
    // A saved V tag cannot be created with this guest. Supply one manually here
    // to check that read planning and diagnostics still defend the boundary.
    let v = TagSpec::new(
        DriverAddress::new(driver.descriptor().id.clone(), "V14", "V", false, 1).with_index(14),
        bit.clone(),
    );
    let specs = vec![
        TagSpec::new(driver.parse_address("D3010", &word).unwrap(), word.clone()),
        v,
        TagSpec::new(driver.parse_address("D3011", &word).unwrap(), word),
        TagSpec::new(driver.parse_address("M10", &bit).unwrap(), bit),
    ];
    let report = session.diagnose_batch(&specs).await.unwrap().unwrap();
    assert_eq!(report.tags.len(), 4);
    assert!(matches!(&report.tags[1].verdict, TagVerdict::Invalid(reason) if reason == V_REASON));
    assert!(matches!(report.tags[0].verdict, TagVerdict::Inside(_)));
    assert!(matches!(report.tags[2].verdict, TagVerdict::Inside(_)));
    assert!(matches!(report.tags[3].verdict, TagVerdict::Inside(_)));
    let values = session.read_batch(&specs).await.unwrap();
    assert_eq!(values[0], Ok(Value::F64(7.0)));
    assert!(matches!(values[1], Err(ItemError::Unsupported(_))));
    assert_eq!(values[2], Ok(Value::F64(7.0)));
    assert_eq!(values[3], Ok(Value::Bit(true)));
    let plans = session.connection_mut().unwrap().plan_read(&specs).unwrap();
    assert_eq!(
        plans
            .iter()
            .map(|plan| plan.covers.clone())
            .collect::<Vec<_>>(),
        vec![vec![0], vec![2], vec![3]]
    );
    assert!(fixture
        .commands()
        .iter()
        .all(|(command, start, _)| *command != 0x0401 || *start != 14));
    assert!(fixture.frames().is_empty());
    session.disconnect().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #35 component"]
async fn new_guest_registry_rest_broker_and_diagnostics_reject_v() {
    let fixture = Fixture::start(Peer::R, 4000, false).await;
    let bytes = new_guest_component();
    let app =
        super::test_app_with_operational_write_manifest("t12-slmp-v-guest", MANIFEST, &bytes).await;
    let saved = app.manager.plc_connections().create_settings(super::ConnectionSettingsInput {
        name: "v-line".into(),
        protocol: "slmp-wasm-operational-write".into(),
        enabled: true,
        simulation: false,
        settings: serde_json::json!({"host":"127.0.0.1", "port":fixture.address.port(), "d_u16_write_mode":"disabled", "d_u16_write_allowlist":""})
            .as_object().unwrap().clone(),
        secrets: Default::default(),
    }).await.unwrap();
    let group = super::CollectionGroupService::with_registry(
        app.pool.clone(),
        app.manager.registry().clone(),
    )
    .create(super::group_input("v-group", saved.id, 100))
    .await
    .unwrap();
    let rejected = app
        .manager
        .tags()
        .create(super::tag_input("v-direct", group.id, "V14", "bit"))
        .await
        .unwrap_err();
    assert!(format!("{rejected:?}").contains(V_REASON), "{rejected:?}");
    let payload = |name: &str, address: &str, data_type: &str| {
        serde_json::json!({
            "name": name, "collectionGroupId": group.id, "address": address,
            "dataType": data_type, "enabled": true, "writable": false
        })
    };
    let (status, body) = super::admin_write(
        &app.router,
        "POST",
        "/api/tags",
        &app.admin_token,
        payload("v-rest", "V14", "bit"),
    )
    .await;
    assert_eq!(status, super::StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    assert_eq!(body["field_errors"][0]["field"], "address");
    assert!(body.to_string().contains(V_REASON), "{body:?}");
    let (status, body) = super::admin_write(&app.router, "POST", "/api/tags/batch", &app.admin_token,
        serde_json::json!({"tags": [payload("mixed-m", "M10", "bit"), payload("mixed-v", "V14", "bit")], "dryRun": false})).await;
    assert_eq!(status, super::StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], false);
    assert_eq!(body["count"], 0);
    assert_eq!(body["errors"][0]["index"], 1);
    assert_eq!(body["errors"][0]["fieldErrors"][0]["field"], "address");
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tags WHERE collection_group_id = ?")
        .bind(group.id)
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    let mut ids = Vec::new();
    for (name, address, kind) in [
        ("word", "D3010", "u16"),
        ("bit", "M10", "bit"),
        ("sb-outside", "SB0", "bit"),
        ("f-outside", "F0", "bit"),
    ] {
        let (status, body) = super::admin_write(
            &app.router,
            "POST",
            "/api/tags",
            &app.admin_token,
            payload(name, address, kind),
        )
        .await;
        assert_eq!(status, super::StatusCode::OK, "{body:?}");
        ids.push(body["id"].as_i64().unwrap());
    }
    app.manager.rebuild().await.unwrap();
    let mut connection_status = app.sessions.status_watch(saved.id).unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        connection_status
            .wait_for(|state| *state == banto_broker::BrokerConnectionStatus::Connected),
    )
    .await
    .unwrap()
    .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if ids.iter().all(|id| {
                app.manager
                    .current_values()
                    .and_then(|values| values.get(&format!("tag:{id}")))
                    .is_some_and(|sample| sample.quality == Quality::Good)
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    for (name, value) in [
        ("word", serde_json::json!(7.0)),
        ("bit", serde_json::json!(true)),
    ] {
        let (status, body) = super::admin_get(
            &app.router,
            &format!("/api/v1/values/v-line.v-group.{name}/read-now"),
            &app.admin_token,
        )
        .await;
        assert_eq!(status, super::StatusCode::OK, "{body:?}");
        assert_eq!(body["v"], value, "{body:?}");
        assert_eq!(body["q"], "good");
    }
    let (status, report) = super::admin_write(
        &app.router,
        "POST",
        &format!("/api/plc-connections/{}/diagnostics", saved.id),
        &app.admin_token,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, super::StatusCode::OK, "{report:?}");
    assert_eq!(report["status"], "current");
    assert_eq!(report["tags"].as_array().map(Vec::len), Some(4));
    for (index, verdict) in ["inside", "inside", "outside", "outside"]
        .into_iter()
        .enumerate()
    {
        assert_eq!(report["tags"][index]["verdict"], verdict);
    }
    for (area, source) in [("SB", "SD268-269"), ("F", "SD270-271")] {
        assert!(report["counts"].as_array().unwrap().iter().any(|count| {
            count["area"] == area
                && count["source"] == source
                && count["bound"] == "exact"
                && count["points"] == 0
        }));
    }
    for id in &ids[2..] {
        let sample = app
            .manager
            .current_values()
            .unwrap()
            .get(&format!("tag:{id}"))
            .unwrap();
        assert_eq!(sample.quality, Quality::Bad);
    }
    assert_eq!(app.sessions.connection_count(), 1);
    assert!(fixture.frames().is_empty());
    assert!(app.sessions.stop_and_join(saved.id).await);
    let (status, expired) = super::admin_write(
        &app.router,
        "GET",
        &format!("/api/plc-connections/{}/diagnostics", saved.id),
        &app.admin_token,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, super::StatusCode::OK);
    assert_eq!(expired["status"], "unavailable");
    drop(app);
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

fn w_specs(driver: &WasmDriver) -> Vec<TagSpec> {
    let kind = TagKind::numeric(DataType::U16);
    (0x1a..=0x1f)
        .map(|index| {
            TagSpec::new(
                driver.parse_address(&format!("W{index:X}"), &kind).unwrap(),
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
    connect_with_order(peer, count, mode, allowlist, bad_ack, "low_high").await
}

async fn connect_with_order(
    peer: Peer,
    count: u32,
    mode: &str,
    allowlist: &str,
    bad_ack: bool,
    word_order: &str,
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
    let mut connection_settings = settings(fixture.address.port(), mode, allowlist);
    if word_order != "low_high" {
        let port_text = fixture.address.port().to_string();
        connection_settings = Settings::from_pairs([
            ("host", "127.0.0.1"),
            ("port", port_text.as_str()),
            ("d_u16_write_mode", mode),
            ("d_u16_write_allowlist", allowlist),
            ("word_order", word_order),
        ])
        .unwrap();
    }
    let session = FrameSession::connect(
        driver.as_ref(),
        &connection_settings,
        &transport,
        &context,
        None,
    )
    .await
    .unwrap();
    (fixture, driver, session)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_OPERATIONAL_WRITE_COMPONENT_WASM to the typed bit guest"]
async fn high_low_word_order_is_preserved_through_host_and_guest() {
    let (fixture, driver, mut session) =
        connect_with_order(Peer::R, 4000, "enabled", "D3010-D3015", false, "high_low").await;
    let items = [
        ("D3010", DataType::U32, 305419896.0),
        ("D3012", DataType::F64, 1.5),
    ]
    .map(|(address, data_type, value)| {
        let kind = TagKind::numeric(data_type);
        WriteItem::new(
            TagSpec::new(driver.parse_address(address, &kind).unwrap(), kind),
            Value::F64(value),
        )
    });
    assert_eq!(session.write_batch(&items).await.unwrap(), vec![Ok(()); 2]);
    assert_eq!(
        fixture.frames(),
        vec![(3010, vec![0x1234, 0x5678, 0x3ff8, 0, 0, 0])]
    );
    session.disconnect().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_OPERATIONAL_WRITE_COMPONENT_WASM to the typed bit guest"]
async fn typed_words_are_one_complete_frame_and_bad_items_send_nothing() {
    for peer in [Peer::R, Peer::StandardQ] {
        let (area, first, allowlist) = if matches!(peer, Peer::R) {
            ("D", 3010_u32, "D3010-D3029")
        } else {
            ("W", 0x1a_u32, "W1A-W2D")
        };
        let (fixture, driver, mut session) = connect(peer, 4000, "enabled", allowlist, false).await;
        let typed = [
            (0, DataType::I16, -2.0),
            (1, DataType::U16, 65535.0),
            (2, DataType::I32, -1.0),
            (4, DataType::U32, 305419896.0),
            (6, DataType::I64, -2.0),
            (10, DataType::U64, 9_007_199_254_740_991.0),
            (14, DataType::F32, 1.5),
            (16, DataType::F64, 1.5),
        ];
        let valid = typed
            .iter()
            .map(|(offset, kind, value)| {
                let kind = TagKind::numeric(*kind);
                let index = first + offset;
                let address = if area == "D" {
                    format!("D{index}")
                } else {
                    format!("W{index:X}")
                };
                WriteItem::new(
                    TagSpec::new(driver.parse_address(&address, &kind).unwrap(), kind),
                    Value::F64(*value),
                )
            })
            .collect::<Vec<_>>();
        let mut bad = valid.clone();
        bad[3].value = Value::Bit(true);
        assert!(session
            .write_batch(&bad)
            .await
            .unwrap()
            .iter()
            .all(|result| matches!(result, Err(ItemError::Unsupported(_)))));
        bad = valid.clone();
        bad[6].value = Value::F64(f64::NAN);
        assert!(session
            .write_batch(&bad)
            .await
            .unwrap()
            .iter()
            .all(|result| matches!(result, Err(ItemError::Unsupported(_)))));
        assert!(fixture.frames().is_empty());
        assert_eq!(session.write_batch(&valid).await.unwrap(), vec![Ok(()); 8]);
        assert_eq!(
            fixture.frames(),
            vec![(
                first,
                vec![
                    0xfffe, 0xffff, 0xffff, 0xffff, 0x5678, 0x1234, 0xfffe, 0xffff, 0xffff, 0xffff,
                    0xffff, 0xffff, 0xffff, 0x001f, 0, 0x3fc0, 0, 0, 0, 0x3ff8,
                ]
            )]
        );
        session.disconnect().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_OPERATIONAL_WRITE_COMPONENT_WASM to the typed bit guest"]
async fn standard_q_and_r_bit_areas_use_bool_and_one_frame_per_allowed_run() {
    for peer in [Peer::R, Peer::StandardQ] {
        for (name, start, device, allowlist) in [
            ("M", "10", 0x90, "M10-M11"),
            ("Y", "1A", 0x9d, "Y1A-Y1B"),
            ("L", "10", 0x92, "L10-L11"),
            ("B", "20", 0xa0, "B20-B21"),
        ] {
            let (fixture, driver, mut session) =
                connect(peer, 4000, "enabled", allowlist, false).await;
            let index = u32::from_str_radix(start, if matches!(name, "M" | "L") { 10 } else { 16 })
                .unwrap();
            let valid = (0..2)
                .map(|offset| {
                    let kind = TagKind::numeric(DataType::Bit);
                    let address = if matches!(name, "M" | "L") {
                        format!("{name}{}", index + offset)
                    } else {
                        format!("{name}{:X}", index + offset)
                    };
                    WriteItem::new(
                        TagSpec::new(driver.parse_address(&address, &kind).unwrap(), kind),
                        Value::Bit(offset == 0),
                    )
                })
                .collect::<Vec<_>>();
            let mut bad = valid.clone();
            bad[1].value = Value::F64(0.0);
            assert!(session
                .write_batch(&bad)
                .await
                .unwrap()
                .iter()
                .all(|result| matches!(result, Err(ItemError::Unsupported(_)))));
            assert!(fixture.bit_frames().is_empty());
            assert_eq!(session.write_batch(&valid).await.unwrap(), vec![Ok(()); 2]);
            assert_eq!(
                fixture.bit_frames(),
                vec![(index, device, vec![true, false])]
            );
            session.disconnect().await;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_OPERATIONAL_WRITE_COMPONENT_WASM to the reviewed component"]
async fn q_and_r_normal_write_are_one_frame_with_per_connection_gate() {
    for peer in [Peer::R, Peer::Q] {
        let (fixture, driver, mut session) =
            connect(peer, 4000, "enabled", "D3010-D3013", false).await;
        let specs = specs(&driver);
        // These are all saved collection tags in the REST fixture. A collector
        // tick may read them before or after the explicit write.
        let read_specs: Vec<_> = [3000, 3010, 3011, 3012, 3013, 3014]
            .map(|index| {
                let kind = TagKind::numeric(DataType::U16);
                TagSpec::new(
                    driver.parse_address(&format!("D{index}"), &kind).unwrap(),
                    kind,
                )
            })
            .into();
        assert_eq!(
            session.read_batch(&read_specs).await.unwrap(),
            vec![Ok(Value::F64(7.0)); read_specs.len()]
        );
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
            session.read_batch(&read_specs).await.unwrap(),
            [7.0, 1.0, 2.0, 3.0, 4.0, 7.0]
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_OPERATIONAL_WRITE_COMPONENT_WASM to the reviewed D/W component"]
async fn bit_bool_values_and_device_error_survive_the_host_read_boundary() {
    let (fixture, driver, mut session) = connect(Peer::R, 4000, "disabled", "", false).await;
    let bit = TagKind::numeric(DataType::Bit);
    for (address, expected) in [
        ("M10", Value::Bit(true)),
        ("M11", Value::Bit(false)),
        ("D10.F", Value::Bit(true)),
        ("D10.0", Value::Bit(false)),
    ] {
        let spec = TagSpec::new(driver.parse_address(address, &bit).unwrap(), bit.clone());
        assert_eq!(
            session.read_batch(&[spec]).await.unwrap(),
            vec![Ok(expected)]
        );
    }
    let rejected = TagSpec::new(driver.parse_address("M12", &bit).unwrap(), bit);
    assert!(matches!(
        session.read_batch(&[rejected]).await.unwrap().as_slice(),
        [Err(ItemError::Device(_))]
    ));
    assert!(fixture.frames().is_empty());
    session.disconnect().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_OPERATIONAL_WRITE_COMPONENT_WASM to the reviewed D/W component"]
async fn saved_bit_tags_keep_values_and_quality_after_collection() {
    let fixture = Fixture::start(Peer::R, 4000, false).await;
    let app = super::test_app_with_operational_write_manifest(
        "t12-operational-bit-read-loopback",
        MANIFEST,
        &component(),
    )
    .await;
    let saved = app
        .manager
        .plc_connections()
        .create_settings(super::ConnectionSettingsInput {
            name: "bit-read-loopback".into(),
            protocol: "slmp-wasm-operational-write".into(),
            enabled: true,
            simulation: false,
            settings: serde_json::json!({
                "host": "127.0.0.1",
                "port": fixture.address.port(),
                "d_u16_write_mode": "disabled"
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
    .create(super::group_input("bit-group", saved.id, 100))
    .await
    .unwrap();
    let mut tags = Vec::new();
    for (name, address, value, quality) in [
        ("set", "M10", Some(1.0), Quality::Good),
        ("clear", "M11", Some(0.0), Quality::Good),
        ("word-bit", "D10.F", Some(1.0), Quality::Good),
        ("device-error", "M12", None, Quality::Bad),
    ] {
        let tag = app
            .manager
            .tags()
            .create(super::tag_input(name, group.id, address, "bit"))
            .await
            .unwrap();
        assert!(!tag.writable);
        tags.push((tag.id, value, quality));
    }
    app.manager.rebuild().await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if tags.iter().all(|(id, value, quality)| {
                app.manager
                    .current_values()
                    .and_then(|values| values.get(&format!("tag:{id}")))
                    .is_some_and(|sample| sample.value == *value && sample.quality == *quality)
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("saved bit values and per-tag quality");
    assert!(fixture.frames().is_empty());
    drop(app);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_OPERATIONAL_WRITE_COMPONENT_WASM to the reviewed D/W component"]
async fn explicit_w_range_uses_the_same_q_and_r_gate_without_broadening_d() {
    for peer in [Peer::R, Peer::StandardQ] {
        let (fixture, driver, mut session) =
            connect(peer, 4000, "enabled", "D3010-D3013", false).await;
        let specs = w_specs(&driver);
        let items: Vec<_> = specs
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, spec)| WriteItem::new(spec, Value::F64((index + 1) as f64)))
            .collect();
        assert!(session
            .write_batch(&items)
            .await
            .unwrap()
            .iter()
            .all(Result::is_err));
        assert!(fixture.frames().is_empty());
        session.disconnect().await;

        let (fixture, driver, mut session) =
            connect(peer, 4000, "enabled", "D3010-D3013,W1A-W1F", false).await;
        let specs = w_specs(&driver);
        let items: Vec<_> = specs
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, spec)| WriteItem::new(spec, Value::F64((index + 1) as f64)))
            .collect();
        let mut invalid = items.clone();
        invalid[2].spec = specs[3].clone(); // hole and duplicate
        let mut mixed = items.clone();
        mixed[2].spec = self::specs(&driver)[2].clone();
        let mut nonfinite = items.clone();
        nonfinite[2].value = Value::F64(f64::NAN);
        for request in [
            invalid,
            mixed,
            nonfinite,
            items.iter().cloned().rev().collect(),
        ] {
            assert!(session
                .write_batch(&request)
                .await
                .unwrap()
                .iter()
                .all(Result::is_err));
        }
        assert!(fixture.frames().is_empty());
        assert_eq!(session.write_batch(&items).await.unwrap(), vec![Ok(()); 6]);
        assert_eq!(fixture.frames(), vec![(0x1a, vec![1, 2, 3, 4, 5, 6])]);
        assert_eq!(fixture.write_devices(), vec![0xb4]);
        assert_eq!(
            session.read_batch(&specs).await.unwrap(),
            [1.0, 2.0, 3.0, 4.0, 5.0, 6.0]
                .map(|value| Ok(Value::F64(value)))
                .to_vec()
        );
        session.disconnect().await;

        let (fixture, driver, mut session) = connect(peer, 4000, "enabled", "W1A-W1F", true).await;
        let specs = w_specs(&driver);
        let items: Vec<_> = specs
            .into_iter()
            .enumerate()
            .map(|(index, spec)| WriteItem::new(spec, Value::F64((index + 1) as f64)))
            .collect();
        assert!(session.write_batch(&items).await.is_err());
        session.disconnect().await;
        assert_eq!(fixture.frames().len(), 1);
        assert_eq!(fixture.write_devices(), vec![0xb4]);
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
