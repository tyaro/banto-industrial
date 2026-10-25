//! Explicitly ignored WASM write check for one R test PLC endpoint.
//! No production registration or ordinary test command can run the live case.

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
// Built locally from the reviewed fdb161520019b81a22bc2fec2bc9bf3d837f5bd3 guest.
const COMPONENT_SHA256: [u8; 32] = [
    0x3c, 0xb4, 0x45, 0x1f, 0x71, 0x2f, 0xc4, 0xc0, 0xde, 0x3a, 0xb8, 0x3e, 0x85, 0xf9, 0xbb, 0x8a,
    0xef, 0xde, 0x6f, 0xa2, 0x6b, 0x5d, 0xe5, 0xa8, 0xb0, 0x99, 0x71, 0x74, 0x8c, 0x9d, 0x71, 0x4e,
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
    let expected_settings = settings.iter().count() == 2
        && settings
            .iter()
            .any(|item| item.key == "host" && item.value == HOST)
        && settings
            .iter()
            .any(|item| item.key == "port" && item.value == PORT.to_string());
    let expected_transport =
        matches!(spec, TransportSpec::Tcp { host, port } if host == HOST && *port == PORT);
    if expected_settings && expected_transport {
        Ok(())
    } else {
        Err("settings and actual TCP destination must both match the one approved R test PLC")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    TypeName,
    SdCount,
    D3010Read,
    D3010Write(u16),
}

#[derive(Default)]
struct WireGate {
    commands: Vec<Command>,
    next_write: Option<u16>,
    writes_sent: usize,
}

impl WireGate {
    fn authorize_one_write(&mut self, value: u16) -> Result<(), &'static str> {
        if self.next_write.is_some() || self.writes_sent >= 2 {
            return Err("a write is already authorized or the two-write limit was reached");
        }
        self.next_write = Some(value);
        Ok(())
    }

    fn check_before_send(&mut self, bytes: &[u8]) -> Result<(), FatalError> {
        let rejected =
            || FatalError::Protocol("R D3010 live fixture blocked an unexpected frame".into());
        if bytes.len() < 19
            || bytes[..2] != [0x54, 0x00]
            || bytes[6..11] != [0, 0xff, 0xff, 0x03, 0]
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
                    (280, 0xa9, 0, 2) => Command::SdCount,
                    (3010, 0xa8, 0, 1) => Command::D3010Read,
                    _ => return Err(rejected()),
                }
            }
            (0x1401, 2, 29) => {
                let start = u32::from_le_bytes(bytes[19..23].try_into().unwrap());
                let count = u16::from_le_bytes(bytes[25..27].try_into().unwrap());
                let value = u16::from_le_bytes(bytes[27..29].try_into().unwrap());
                if start != 3010
                    || bytes[23..25] != [0xa8, 0]
                    || count != 1
                    || self.writes_sent >= 2
                    || self.next_write != Some(value)
                {
                    return Err(rejected());
                }
                self.next_write = None;
                self.writes_sent += 1;
                Command::D3010Write(value)
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
fn destination_guard_rejects_settings_spoofing_and_other_transports_before_dial() {
    let actual = TransportSpec::Tcp {
        host: HOST.into(),
        port: PORT,
    };
    let settings = |host: &str, port: &str| {
        Settings::from_pairs([
            ("host".to_owned(), host.to_owned()),
            ("port".to_owned(), port.to_owned()),
        ])
        .unwrap()
    };
    assert!(require_exact_destination(&settings(HOST, "3109"), &actual).is_ok());
    assert!(require_exact_destination(&settings("127.0.0.1", "3109"), &actual).is_err());
    assert!(require_exact_destination(&settings(HOST, "3100"), &actual).is_err());
    for wrong in [
        TransportSpec::Tcp {
            host: "127.0.0.1".into(),
            port: PORT,
        },
        TransportSpec::Tcp {
            host: HOST.into(),
            port: 3100,
        },
        TransportSpec::SerialOverTcp {
            host: HOST.into(),
            port: PORT,
        },
    ] {
        assert!(require_exact_destination(&settings(HOST, "3109"), &wrong).is_err());
    }
}

#[test]
fn wire_guard_allows_only_discovery_d3010_read_and_two_explicit_single_writes() {
    let mut gate = WireGate::default();
    gate.check_before_send(&r_request(0x0101, 0, &[])).unwrap();
    gate.check_before_send(&r_request(0x0401, 2, &r_device_tail(280, 0xa9, 2)))
        .unwrap();
    gate.check_before_send(&r_request(0x0401, 2, &r_device_tail(3010, 0xa8, 1)))
        .unwrap();
    assert!(gate
        .check_before_send(&r_request(0x0401, 2, &r_device_tail(3000, 0xa8, 1)))
        .is_err());
    let mut write_tail = r_device_tail(3010, 0xa8, 1);
    write_tail.extend_from_slice(&9u16.to_le_bytes());
    assert!(gate
        .check_before_send(&r_request(0x1401, 2, &write_tail))
        .is_err());
    gate.authorize_one_write(9).unwrap();
    let mut wrong_tail = r_device_tail(3001, 0xa8, 1);
    wrong_tail.extend_from_slice(&9u16.to_le_bytes());
    assert!(gate
        .check_before_send(&r_request(0x1401, 2, &wrong_tail))
        .is_err());
    wrong_tail = r_device_tail(3010, 0xa8, 2);
    wrong_tail.extend_from_slice(&9u16.to_le_bytes());
    assert!(gate
        .check_before_send(&r_request(0x1401, 2, &wrong_tail))
        .is_err());
    assert!(gate
        .check_before_send(&r_request(0x1401, 2, &write_tail))
        .is_ok());
    assert!(gate
        .check_before_send(&r_request(0x1401, 2, &write_tail))
        .is_err());
    gate.authorize_one_write(7).unwrap();
    write_tail[8..10].copy_from_slice(&7u16.to_le_bytes());
    assert!(gate
        .check_before_send(&r_request(0x1401, 2, &write_tail))
        .is_ok());
    assert!(gate.authorize_one_write(9).is_err());
    assert_eq!(gate.writes_sent, 2);
}

async fn read_d3010(session: &mut FrameSession, spec: &TagSpec) -> Result<u16, String> {
    let values = session
        .read_batch(std::slice::from_ref(spec))
        .await
        .map_err(|error| format!("D3010 read failed: {error:?}"))?;
    match values.as_slice() {
        [Ok(Value::F64(value))]
            if value.is_finite() && value.fract() == 0.0 && (0.0..=65535.0).contains(value) =>
        {
            Ok(*value as u16)
        }
        _ => Err(format!(
            "D3010 read did not return one u16 value: {values:?}"
        )),
    }
}

async fn write_once(session: &mut FrameSession, spec: &TagSpec, value: u16) -> Result<(), String> {
    let result = session
        .write_batch(&[WriteItem::new(spec.clone(), Value::F64(f64::from(value)))])
        .await;
    match result {
        Ok(values) if values == vec![Ok(())] => Ok(()),
        other => Err(format!(
            "write acknowledgement not confirmed; stop without retry or automatic restoration: {other:?}"
        )),
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "real R test PLC; requires separate review gate and explicit opt-in"]
async fn live_r_d3010_write_readback_restore_once() {
    assert_eq!(
        std::env::var("BANTO_LIVE_WASM_R_D3010_CONFIRM").as_deref(),
        Ok("192.168.11.200:3109"),
        "explicit live-test opt-in is required before any TCP dial"
    );
    let component = std::fs::read(
        std::env::var("BANTO_SLMP_WRITE_COMPONENT_WASM").expect("pinned guest component path"),
    )
    .expect("pinned guest component bytes");
    assert_eq!(
        Sha256::digest(&component).as_slice(),
        COMPONENT_SHA256.as_slice(),
        "the component must be the reviewed fixed guest build"
    );
    let driver = WasmDriver::load_controlled_r_test_fixture(MANIFEST, &component).unwrap();
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
    .expect("0101 and SD handshake on the same R session");
    audit("handshake_complete", "0101 and SD confirmed");
    assert_eq!(
        gate.lock().unwrap().commands.as_slice(),
        &[Command::TypeName, Command::SdCount],
        "R type and SD range discovery must precede D3010"
    );
    let kind = TagKind::numeric(DataType::U16);
    let spec = TagSpec::new(driver.parse_address("D3010", &kind).unwrap(), kind);
    let original = read_d3010(&mut session, &spec).await.unwrap();
    audit("original_read_1", format!("D3010={original}"));
    tokio::time::sleep(Duration::from_secs(1)).await;
    let repeated = read_d3010(&mut session, &spec).await.unwrap();
    audit("original_read_2", format!("D3010={repeated}"));
    assert_eq!(original, repeated, "D3010 changed; no write was authorized");

    let trial = if original == 0 { 1 } else { 0 };
    gate.lock().unwrap().authorize_one_write(trial).unwrap();
    audit("trial_write_start", format!("D3010={trial}"));
    write_once(&mut session, &spec, trial).await.unwrap();
    audit("trial_write_ack", format!("D3010={trial}"));
    let observed = read_d3010(&mut session, &spec).await.unwrap();
    audit("trial_readback", format!("D3010={observed}"));
    assert_eq!(
        observed, trial,
        "test value not confirmed; no restoration attempt"
    );

    gate.lock().unwrap().authorize_one_write(original).unwrap();
    audit("restore_write_start", format!("D3010={original}"));
    write_once(&mut session, &spec, original).await.unwrap();
    audit("restore_write_ack", format!("D3010={original}"));
    let restored = read_d3010(&mut session, &spec).await.unwrap();
    audit("restore_readback", format!("D3010={restored}"));
    assert_eq!(
        restored, original,
        "original value not confirmed after one restoration"
    );
    assert_eq!(gate.lock().unwrap().writes_sent, 2);
    session.disconnect().await;
    audit("connection_closed", "FrameSession disconnect completed");
}
