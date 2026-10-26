//! Normal D/u16 policy acceptance through an isolated loopback SLMP peer.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use banto_collect::current::Quality;
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
    StandardQ,
}

struct Fixture {
    address: std::net::SocketAddr,
    frames: RecordedFrames,
    commands: Arc<Mutex<Vec<(u16, u32, u16)>>>,
    write_devices: Arc<Mutex<Vec<u8>>>,
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
        let write_devices = Arc::new(Mutex::new(Vec::new()));
        let devices = write_devices.clone();
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
                                    assert_eq!((subcommand, start, words), (2, 280, 4));
                                    let mut body = count.to_le_bytes().to_vec();
                                    body.extend_from_slice(&count.to_le_bytes());
                                    (0, body)
                                }
                                Peer::Q => {
                                    assert_eq!((subcommand, start, words), (0, 302, 1));
                                    (0, u16::try_from(count).unwrap().to_le_bytes().to_vec())
                                }
                                Peer::StandardQ => {
                                    assert_eq!((subcommand, start, words), (0, 302, 2));
                                    let mut body =
                                        u16::try_from(count).unwrap().to_le_bytes().to_vec();
                                    body.extend_from_slice(&64u16.to_le_bytes());
                                    (0, body)
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
                            let end = start.checked_add(u32::try_from(words).unwrap()).unwrap();
                            assert!(
                                words > 0
                                    && ((device == 0xa8
                                        && ((start == 10 && end == 11)
                                            || (start == 3000 && end == 3001)
                                            || (start >= 3010 && end <= 3015)))
                                        || (device == 0xb4 && start >= 0x1a && end <= 0x20)
                                        || (device == 0x90 && start >= 10 && end <= 13))
                            );
                            if device == 0x90 {
                                assert_eq!(subcommand, if is_r { 3 } else { 1 });
                                if start == 12 {
                                    (0xc051, Vec::new())
                                } else {
                                    let bits =
                                        (start..end).map(|index| index == 10).collect::<Vec<_>>();
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
                            let offset = if subcommand == 2 { 25 } else { 23 };
                            let device = request[if subcommand == 2 { 23 } else { 22 }];
                            assert!(matches!(device, 0xa8 | 0xb4));
                            assert_eq!(subcommand, if matches!(peer, Peer::R) { 2 } else { 0 });
                            let words = usize::from(u16::from_le_bytes([
                                request[offset],
                                request[offset + 1],
                            ]));
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
            write_devices,
            task,
        }
    }

    fn frames(&self) -> Vec<(u32, Vec<u16>)> {
        self.frames.lock().unwrap().clone()
    }

    fn commands(&self) -> Vec<(u16, u32, u16)> {
        self.commands.lock().unwrap().clone()
    }

    fn write_devices(&self) -> Vec<u8> {
        self.write_devices.lock().unwrap().clone()
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
