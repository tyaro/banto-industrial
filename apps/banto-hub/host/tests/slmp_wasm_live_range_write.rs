//! Explicitly ignored, separate four-word R test PLC fixture. Never run in CI.
//! Production registration and the single-word live fixture remain unchanged.

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

const HOST: &str = "192.168.11.200";
const PORT: u16 = 3109;
const MANIFEST: &[u8] = include_bytes!("t12_slmp_wasm_write/manifest.json");
// Built from pinned guest c95e7f84e0a3b275298901882a9f6b2512c4f41a.
// Recheck against the reviewed head before any live run.
const COMPONENT_SHA256: [u8; 32] = [
    0x77, 0xbe, 0xdb, 0x59, 0x6b, 0x52, 0x6e, 0xcf, 0xa8, 0x46, 0x60, 0xcf, 0x13, 0xdd, 0xa1, 0x5f,
    0xd5, 0xe6, 0x48, 0xc9, 0xc8, 0x88, 0xd4, 0x15, 0xbe, 0x29, 0x03, 0xe0, 0x36, 0x48, 0xdd, 0x4c,
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
    let settings_match = settings.iter().count() == 2
        && settings
            .iter()
            .any(|item| item.key == "host" && item.value == HOST)
        && settings
            .iter()
            .any(|item| item.key == "port" && item.value == PORT.to_string());
    let transport_match =
        matches!(spec, TransportSpec::Tcp { host, port } if host == HOST && *port == PORT);
    if settings_match && transport_match {
        Ok(())
    } else {
        Err("settings and actual TCP destination must both match the approved R test PLC")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    TypeName,
    SdCount,
    DRead,
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
            FatalError::Protocol("R D3010..D3013 live fixture blocked an unexpected frame".into())
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
            (0x0401, 2, 27) => {
                let start = u32::from_le_bytes(bytes[19..23].try_into().unwrap());
                let count = u16::from_le_bytes(bytes[25..27].try_into().unwrap());
                match (start, bytes[23], bytes[24], count) {
                    (280, 0xa9, 0, 4) => Command::SdCount,
                    (3010, 0xa8, 0, 4) => Command::DRead,
                    _ => return Err(rejected()),
                }
            }
            (0x1401, 2, 35) => {
                let start = u32::from_le_bytes(bytes[19..23].try_into().unwrap());
                let count = u16::from_le_bytes(bytes[25..27].try_into().unwrap());
                let values = std::array::from_fn(|index| {
                    u16::from_le_bytes(bytes[27 + index * 2..29 + index * 2].try_into().unwrap())
                });
                if start != 3010
                    || bytes[23..25] != [0xa8, 0]
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

fn r_request(command: u16, subcommand: u16, tail: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0x54, 0, 1, 0, 0, 0, 0, 0xff, 0xff, 0x03, 0];
    bytes.extend_from_slice(&u16::try_from(6 + tail.len()).unwrap().to_le_bytes());
    bytes.extend_from_slice(&0x0010u16.to_le_bytes());
    bytes.extend_from_slice(&command.to_le_bytes());
    bytes.extend_from_slice(&subcommand.to_le_bytes());
    bytes.extend_from_slice(tail);
    bytes
}

fn r_device_tail(start: u32, device: u8, count: u16) -> Vec<u8> {
    let mut tail = start.to_le_bytes().to_vec();
    tail.extend_from_slice(&[device, 0]);
    tail.extend_from_slice(&count.to_le_bytes());
    tail
}

#[test]
fn range_wire_guard_rejects_other_frames_and_limits_two_authorized_writes() {
    let mut gate = WireGate::default();
    gate.check_before_send(&r_request(0x0101, 0, &[])).unwrap();
    let mut wrong_route = r_request(0x0101, 0, &[]);
    wrong_route[10] = 1;
    assert!(gate.check_before_send(&wrong_route).is_err());
    gate.check_before_send(&r_request(0x0401, 2, &r_device_tail(280, 0xa9, 4)))
        .unwrap();
    gate.check_before_send(&r_request(0x0401, 2, &r_device_tail(3010, 0xa8, 4)))
        .unwrap();
    for (start, count) in [(2999, 4), (3000, 4), (3001, 4), (3010, 1), (3011, 4)] {
        assert!(gate
            .check_before_send(&r_request(0x0401, 2, &r_device_tail(start, 0xa8, count)))
            .is_err());
    }
    let mut tail = r_device_tail(3010, 0xa8, 4);
    tail.extend([1, 2, 3, 4].into_iter().flat_map(u16::to_le_bytes));
    assert!(gate
        .check_before_send(&r_request(0x1401, 2, &tail))
        .is_err());
    gate.authorize_one_write([1, 2, 3, 4]).unwrap();
    for (start, count) in [(2999, 4), (3000, 4), (3001, 4), (3010, 1), (3011, 4)] {
        let mut wrong_tail = r_device_tail(start, 0xa8, count);
        wrong_tail.extend([1, 2, 3, 4].into_iter().flat_map(u16::to_le_bytes));
        assert!(gate
            .check_before_send(&r_request(0x1401, 2, &wrong_tail))
            .is_err());
    }
    assert!(gate.check_before_send(&r_request(0x1401, 2, &tail)).is_ok());
    assert!(gate
        .check_before_send(&r_request(0x1401, 2, &tail))
        .is_err());
    gate.authorize_one_write([4, 3, 2, 1]).unwrap();
    assert!(gate
        .check_before_send(&r_request(0x1401, 2, &tail))
        .is_err());
    tail[8..16].copy_from_slice(
        &[4u16, 3, 2, 1]
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    assert!(gate.check_before_send(&r_request(0x1401, 2, &tail)).is_ok());
    assert!(gate.authorize_one_write([1, 2, 3, 4]).is_err());
    assert_eq!(gate.writes_sent, 2);
}

#[test]
fn range_destination_guard_rejects_settings_spoofing_before_dial() {
    let settings = |host: &str, port: &str| {
        Settings::from_pairs([
            ("host".to_owned(), host.to_owned()),
            ("port".to_owned(), port.to_owned()),
        ])
        .unwrap()
    };
    let destination = TransportSpec::Tcp {
        host: HOST.into(),
        port: PORT,
    };
    assert!(require_exact_destination(&settings(HOST, "3109"), &destination).is_ok());
    assert!(require_exact_destination(&settings("127.0.0.1", "3109"), &destination).is_err());
    assert!(require_exact_destination(&settings(HOST, "3100"), &destination).is_err());
    assert!(require_exact_destination(
        &settings(HOST, "3109"),
        &TransportSpec::Tcp {
            host: HOST.into(),
            port: 3100
        }
    )
    .is_err());
    assert!(require_exact_destination(
        &settings(HOST, "3109"),
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
        return Err("0101 and SD handshake must precede D access".into());
    }
    audit("handshake_complete", "0101 and SD confirmed");
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
#[ignore = "real R test PLC; separate review, CI success, and explicit start signal required"]
async fn live_r_d3010_to_d3013_range_write_readback_restore_once() {
    assert_eq!(
        std::env::var("BANTO_LIVE_WASM_R_D3010_RANGE_CONFIRM").as_deref(),
        Ok("192.168.11.200:3109:D3010-D3013"),
        "explicit four-word live-test opt-in is required before any TCP dial"
    );
    let component = std::fs::read(
        std::env::var("BANTO_SLMP_RANGE_WRITE_COMPONENT_WASM").expect("reviewed range guest path"),
    )
    .expect("reviewed range guest bytes");
    assert_eq!(
        Sha256::digest(&component).as_slice(),
        COMPONENT_SHA256.as_slice(),
        "component hash differs from reviewed build"
    );
    let driver = WasmDriver::load_controlled_r_range_test_fixture(MANIFEST, &component).unwrap();
    let settings = Settings::from_pairs([
        ("host".to_owned(), HOST.to_owned()),
        ("port".to_owned(), PORT.to_string()),
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
        .expect("approved R test PLC TCP connection");
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
    .expect("0101 and SD handshake");
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
