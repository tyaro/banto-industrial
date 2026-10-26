//! Explicitly ignored, separate four-word Q25PRH test PLC fixture. Never run in CI.
//! Production registration and the R live fixtures remain unchanged.

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use banto_driver_api::{
    BoxFuture, DataType, DriverBase, FatalError, Session, Settings, TagKind, TagSpec, Value,
    WriteItem,
};
use banto_driver_engine::{
    transport::{Transport, TransportSpec},
    FrameSession,
};
use banto_driver_wasm::WasmDriver;
use sha2::{Digest, Sha256};

const HOST: &str = "192.168.11.10";
const PORT: u16 = 3109;
const MANIFEST: &[u8] = include_bytes!("t12_slmp_wasm_q_write/manifest.json");
// Built from the pinned Q25PRH test guest commit 33171bbb990859c5a46df44cfafc233e58ba5317.
// A separate start signal is still required before running this ignored real-Q test.
const COMPONENT_SHA256: [u8; 32] = [
    0xb8, 0xc2, 0xdc, 0x8d, 0x47, 0xce, 0x71, 0xc4, 0xc1, 0xc3, 0x1a, 0xc0, 0x47, 0x64, 0x32, 0xb8,
    0xfa, 0x5b, 0xe0, 0x8c, 0x2f, 0x39, 0xda, 0xf1, 0x1c, 0xf3, 0x83, 0xda, 0xe3, 0x9e, 0x51, 0x0f,
];

fn audit(stage: &str, detail: impl std::fmt::Display) {
    let unix_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("UTC clock before Unix epoch")
        .as_nanos();
    println!("utc_unix_ns={unix_ns} stage={stage} {detail}");
}

fn require_exact_destination(
    settings: &Settings,
    spec: &TransportSpec,
) -> Result<(), &'static str> {
    let settings_match = settings.iter().count() == 3
        && settings
            .iter()
            .any(|item| item.key == "host" && item.value == HOST)
        && settings
            .iter()
            .any(|item| item.key == "port" && item.value == PORT.to_string())
        && settings
            .iter()
            .any(|item| item.key == "q25prh_d3010_3013_test_write" && item.value == "enabled");
    let transport_match =
        matches!(spec, TransportSpec::Tcp { host, port } if host == HOST && *port == PORT);
    if settings_match && transport_match {
        Ok(())
    } else {
        Err("settings and actual TCP destination must both match the approved Q test PLC")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    TypeName,
    SdCount,
    DRead(u32),
    DWrite([u16; 4]),
}

#[derive(Default)]
struct WireGate {
    commands: Vec<Command>,
    next_write: Option<[u16; 4]>,
    writes_sent: usize,
}

impl WireGate {
    fn authorize_one_write(&mut self, values: [u16; 4]) -> Result<(), &'static str> {
        if self.next_write.is_some() || self.writes_sent >= 2 {
            return Err("a write is already authorized or the two-frame limit was reached");
        }
        self.next_write = Some(values);
        Ok(())
    }

    fn check_before_send(&mut self, bytes: &[u8]) -> Result<(), FatalError> {
        let rejected = || {
            FatalError::Protocol("Q D3010..D3013 live fixture blocked an unexpected frame".into())
        };
        if bytes.len() < 19
            || bytes[..2] != [0x54, 0]
            || bytes[4..11] != [0, 0, 0, 0xff, 0xff, 0x03, 0]
            || bytes[13..15] != [0x10, 0]
            || bytes.len() != 13 + usize::from(u16::from_le_bytes([bytes[11], bytes[12]]))
        {
            return Err(rejected());
        }
        let command = u16::from_le_bytes([bytes[15], bytes[16]]);
        let subcommand = u16::from_le_bytes([bytes[17], bytes[18]]);
        let permitted = match (command, subcommand, bytes.len()) {
            (0x0101, 0, 19) => Command::TypeName,
            (0x0401, 0, 25) => {
                let start = u32::from_le_bytes([bytes[19], bytes[20], bytes[21], 0]);
                let count = u16::from_le_bytes(bytes[23..25].try_into().unwrap());
                match (start, bytes[22], count) {
                    (302, 0xa9, 1) => Command::SdCount,
                    (3010..=3013, 0xa8, 1) => Command::DRead(start),
                    _ => return Err(rejected()),
                }
            }
            (0x1401, 0, 33) => {
                let start = u32::from_le_bytes([bytes[19], bytes[20], bytes[21], 0]);
                let count = u16::from_le_bytes(bytes[23..25].try_into().unwrap());
                let values = std::array::from_fn(|index| {
                    u16::from_le_bytes(bytes[25 + index * 2..27 + index * 2].try_into().unwrap())
                });
                if start != 3010
                    || bytes[22] != 0xa8
                    || count != 4
                    || self.writes_sent >= 2
                    || self.next_write != Some(values)
                {
                    return Err(rejected());
                }
                self.next_write = None;
                self.writes_sent += 1;
                Command::DWrite(values)
            }
            _ => return Err(rejected()),
        };
        self.commands.push(permitted);
        Ok(())
    }
}

struct GuardedTransport {
    inner: Box<dyn Transport>,
    gate: Arc<Mutex<WireGate>>,
}

impl Transport for GuardedTransport {
    fn tx_time(&self, len: usize) -> Duration {
        self.inner.tx_time(len)
    }

    fn send<'a>(&'a mut self, bytes: &'a [u8]) -> BoxFuture<'a, Result<(), FatalError>> {
        Box::pin(async move {
            self.gate.lock().unwrap().check_before_send(bytes)?;
            self.inner.send(bytes).await
        })
    }

    fn recv<'a>(&'a mut self, buf: &'a mut Vec<u8>) -> BoxFuture<'a, Result<usize, FatalError>> {
        self.inner.recv(buf)
    }

    fn discard_input(&mut self) -> BoxFuture<'_, ()> {
        self.inner.discard_input()
    }

    fn close(&mut self) -> BoxFuture<'_, ()> {
        self.inner.close()
    }
}

fn q_request(command: u16, tail: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0x54, 0, 1, 0, 0, 0, 0, 0xff, 0xff, 0x03, 0];
    bytes.extend_from_slice(&u16::try_from(6 + tail.len()).unwrap().to_le_bytes());
    bytes.extend_from_slice(&0x0010u16.to_le_bytes());
    bytes.extend_from_slice(&command.to_le_bytes());
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes.extend_from_slice(tail);
    bytes
}

fn q_device_tail(start: u32, device: u8, count: u16) -> Vec<u8> {
    let mut tail = start.to_le_bytes()[..3].to_vec();
    tail.push(device);
    tail.extend_from_slice(&count.to_le_bytes());
    tail
}

#[test]
fn q_range_wire_guard_rejects_other_frames_and_limits_two_authorized_writes() {
    let mut gate = WireGate::default();
    gate.check_before_send(&q_request(0x0101, &[])).unwrap();
    let mut wrong_route = q_request(0x0101, &[]);
    wrong_route[10] = 1;
    assert!(gate.check_before_send(&wrong_route).is_err());
    gate.check_before_send(&q_request(0x0401, &q_device_tail(302, 0xa9, 1)))
        .unwrap();
    for index in 3010..=3013 {
        gate.check_before_send(&q_request(0x0401, &q_device_tail(index, 0xa8, 1)))
            .unwrap();
    }
    for (start, count) in [(2999, 4), (3000, 4), (3001, 4), (3010, 1), (3011, 4)] {
        assert!(gate
            .check_before_send(&q_request(0x0401, &q_device_tail(start, 0xa8, count + 1)))
            .is_err());
    }
    let mut tail = q_device_tail(3010, 0xa8, 4);
    tail.extend([1, 2, 3, 4].into_iter().flat_map(u16::to_le_bytes));
    assert!(gate.check_before_send(&q_request(0x1401, &tail)).is_err());
    gate.authorize_one_write([1, 2, 3, 4]).unwrap();
    for (start, count) in [(2999, 4), (3000, 4), (3001, 4), (3010, 1), (3011, 4)] {
        let mut wrong_tail = q_device_tail(start, 0xa8, count);
        wrong_tail.extend([1, 2, 3, 4].into_iter().flat_map(u16::to_le_bytes));
        assert!(gate
            .check_before_send(&q_request(0x1401, &wrong_tail))
            .is_err());
    }
    assert!(gate.check_before_send(&q_request(0x1401, &tail)).is_ok());
    assert!(gate.check_before_send(&q_request(0x1401, &tail)).is_err());
    gate.authorize_one_write([4, 3, 2, 1]).unwrap();
    assert!(gate.check_before_send(&q_request(0x1401, &tail)).is_err());
    tail[6..14].copy_from_slice(
        &[4u16, 3, 2, 1]
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    assert!(gate.check_before_send(&q_request(0x1401, &tail)).is_ok());
    assert!(gate.authorize_one_write([1, 2, 3, 4]).is_err());
    assert_eq!(gate.writes_sent, 2);
}

#[test]
fn q_range_destination_guard_rejects_settings_spoofing_before_dial() {
    let settings = |host: &str, port: &str, opt_in: bool| {
        let mut pairs = vec![
            ("host".to_owned(), host.to_owned()),
            ("port".to_owned(), port.to_owned()),
        ];
        if opt_in {
            pairs.push(("q25prh_d3010_3013_test_write".into(), "enabled".into()));
        }
        Settings::from_pairs(pairs).unwrap()
    };
    let destination = TransportSpec::Tcp {
        host: HOST.into(),
        port: PORT,
    };
    assert!(require_exact_destination(&settings(HOST, "3109", true), &destination).is_ok());
    assert!(require_exact_destination(&settings(HOST, "3109", false), &destination).is_err());
    assert!(require_exact_destination(&settings("127.0.0.1", "3109", true), &destination).is_err());
    assert!(require_exact_destination(&settings(HOST, "3100", true), &destination).is_err());
    assert!(require_exact_destination(
        &settings(HOST, "3109", true),
        &TransportSpec::Tcp {
            host: HOST.into(),
            port: 3100
        }
    )
    .is_err());
    assert!(require_exact_destination(
        &settings(HOST, "3109", true),
        &TransportSpec::SerialOverTcp {
            host: HOST.into(),
            port: PORT
        }
    )
    .is_err());
}

async fn read_four(session: &mut FrameSession, specs: &[TagSpec]) -> Result<[u16; 4], String> {
    let values = session
        .read_batch(specs)
        .await
        .map_err(|e| format!("D3010..D3013 read failed: {e:?}"))?;
    if values.len() != 4 {
        return Err(format!("expected four read results, got {}", values.len()));
    }
    let mut words = [0; 4];
    for (word, result) in words.iter_mut().zip(values) {
        match result {
            Ok(Value::F64(value))
                if value.is_finite()
                    && value.fract() == 0.0
                    && (0.0..=65535.0).contains(&value) =>
            {
                *word = value as u16
            }
            other => return Err(format!("D read did not return a u16 value: {other:?}")),
        }
    }
    Ok(words)
}

async fn write_once(
    session: &mut FrameSession,
    specs: &[TagSpec],
    values: [u16; 4],
) -> Result<(), String> {
    let items: Vec<_> = specs
        .iter()
        .cloned()
        .zip(values)
        .map(|(spec, value)| WriteItem::new(spec, Value::F64(f64::from(value))))
        .collect();
    let result = session.write_batch(&items).await;
    match result {
        Ok(values) if values == vec![Ok(()); 4] => Ok(()),
        other => Err(format!(
            "write result uncertain; stop without retry or automatic restoration: {other:?}"
        )),
    }
}

async fn run_once(
    session: &mut FrameSession,
    gate: &Arc<Mutex<WireGate>>,
    specs: &[TagSpec],
) -> Result<(), String> {
    if gate.lock().unwrap().commands.as_slice() != [Command::TypeName, Command::SdCount] {
        return Err("0101 and Q SD302 handshake must precede D access".into());
    }
    audit("handshake_complete", "Q25PRH 0101 and SD302 confirmed");
    let original = read_four(session, specs).await?;
    audit("original_read_1", format!("{original:?}"));
    tokio::time::sleep(Duration::from_secs(1)).await;
    let repeated = read_four(session, specs).await?;
    audit("original_read_2", format!("{repeated:?}"));
    if original != repeated {
        return Err("original four D words changed; no write authorized".into());
    }
    let trial = [1, 2, 3, 4];
    if original == trial {
        return Err("test values equal original values; no write authorized".into());
    }
    gate.lock()
        .unwrap()
        .authorize_one_write(trial)
        .map_err(str::to_owned)?;
    audit("trial_write_start", format!("{trial:?}"));
    write_once(session, specs, trial).await?;
    audit("trial_write_ack", format!("{trial:?}"));
    let observed = read_four(session, specs).await?;
    audit("trial_readback", format!("{observed:?}"));
    if observed != trial {
        return Err("trial readback mismatch; stop without automatic restoration".into());
    }
    gate.lock()
        .unwrap()
        .authorize_one_write(original)
        .map_err(str::to_owned)?;
    audit("restore_write_start", format!("{original:?}"));
    write_once(session, specs, original).await?;
    audit("restore_write_ack", format!("{original:?}"));
    let restored = read_four(session, specs).await?;
    audit("restore_readback", format!("{restored:?}"));
    if restored != original {
        return Err("original four D words not confirmed after one restoration".into());
    }
    if gate.lock().unwrap().writes_sent != 2 {
        return Err("unexpected number of write frames".into());
    }
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "real Q test PLC; separate review, CI success, and explicit start signal required"]
async fn live_q_d3010_to_d3013_range_write_readback_restore_once() {
    assert_eq!(
        std::env::var("BANTO_LIVE_WASM_Q_D3010_RANGE_CONFIRM").as_deref(),
        Ok("192.168.11.10:3109:D3010-D3013"),
        "explicit four-word live-test opt-in is required before any TCP dial"
    );
    let component = std::fs::read(
        std::env::var("BANTO_SLMP_Q_WRITE_COMPONENT_WASM").expect("reviewed Q guest path"),
    )
    .expect("reviewed range guest bytes");
    assert_eq!(
        Sha256::digest(&component).as_slice(),
        COMPONENT_SHA256.as_slice(),
        "component hash differs from reviewed build"
    );
    let driver = WasmDriver::load_controlled_q_range_test_fixture(MANIFEST, &component).unwrap();
    let settings = Settings::from_pairs([
        ("host".to_owned(), HOST.to_owned()),
        ("port".to_owned(), PORT.to_string()),
        ("q25prh_d3010_3013_test_write".into(), "enabled".into()),
    ])
    .unwrap();
    let destination = TransportSpec::Tcp {
        host: HOST.into(),
        port: PORT,
    };
    require_exact_destination(&settings, &destination).expect("destination must match before dial");
    audit("connect_start", format!("destination={HOST}:{PORT}"));
    let gate = Arc::new(Mutex::new(WireGate::default()));
    let wire = destination
        .connect(Duration::from_secs(3))
        .await
        .expect("approved Q test PLC TCP connection");
    let guarded = Box::new(GuardedTransport {
        inner: wire,
        gate: gate.clone(),
    });
    let mut session = FrameSession::with_transport(
        driver.as_ref(),
        &settings,
        guarded,
        Duration::from_secs(3),
        None,
    )
    .await
    .expect("0101 and SD302 handshake");
    let kind = TagKind::numeric(DataType::U16);
    let specs: Vec<_> = (3010..=3013)
        .map(|index| {
            TagSpec::new(
                driver.parse_address(&format!("D{index}"), &kind).unwrap(),
                kind.clone(),
            )
        })
        .collect();
    let result = run_once(&mut session, &gate, &specs).await;
    session.disconnect().await;
    audit("connection_closed", "FrameSession disconnect completed");
    result.unwrap();
}
