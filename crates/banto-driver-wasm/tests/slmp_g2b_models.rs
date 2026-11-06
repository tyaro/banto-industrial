//! Component-level checks of the SLMP guest's G2b model identification
//! (banto-driver-wasm #72) through this crate's adapter (Hub side acceptance,
//! mock only).
//!
//! Run with `BANTO_SLMP_NEW_COMPONENT_WASM=<built guest> cargo test -- --ignored`.
//! The guest now recognizes, by the exact 0101 (name, code) pair, the iQ-R
//! process CPU (RnPCPU), SIL2 process CPU (RnPSFCPU), safety CPU (RnSFCPU) and
//! MX-R as standard R. The process CPU reads S like R08CPU; the other three
//! groups have no S, so an S tag is a per-item `Unsupported` that is never
//! sent. The previous pin `abb1a1a` classified every one of these pairs as
//! Unknown, so the model tests below fail there (checked 2026-11-06): its
//! handshake ends after 0101 and the whole read batch is a protocol error.
//! `mismatched_pairs_and_route_fallbacks_stay_unknown` passes on both pins; it
//! guards that only the exact pairs are recognized. The tests drive the real
//! engine (`FrameSession` over TCP) against a loopback SLMP peer on a std
//! thread that records every request byte for byte. The peer is a mock iQ-R
//! wire PLC (4E, subcommand 0002 word / 0003 bit) whose 0101 answer is chosen
//! per test; no PLC is involved.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use banto_driver_api::{
    ConnectContext, DataType, DriverBase, FatalError, ItemError, Session as _, Settings, TagKind,
    TagSpec, Value,
};
use banto_driver_engine::{transport::TransportSpec, FrameSession};
use banto_driver_wasm::WasmDriver;

/// Same id as the Hub fixture manifest
/// `apps/banto-hub/host/tests/t12_slmp_wasm_operational_write/manifest.json`.
const MANIFEST: &[u8] = br#"{"manifestVersion":1,"id":"slmp-wasm-operational-write","version":"0.1.0","hubApi":"1","writeApi":"1","settingsVersion":1,"displayName":"SLMP G2b fixture","transports":["tcp","udp"],"capabilities":["read","write"]}"#;

fn component() -> Vec<u8> {
    std::fs::read(std::env::var("BANTO_SLMP_NEW_COMPONENT_WASM").expect("guest path")).unwrap()
}

const D: u8 = 0xa8;
const SD: u8 = 0xa9;
const S: u8 = 0x98;
const END_CODE_C051: u16 = 0xc051;

/// What the peer saw, from the reserved bytes on (the serial is left out).
#[derive(Default)]
struct Seen {
    all: Vec<Vec<u8>>,
    /// (device code, start, points, subcommand) of each plain 0401 read.
    reads: Vec<(u8, u32, u16, u16)>,
    connections: u32,
}

struct Peer {
    address: SocketAddr,
    seen: Arc<Mutex<Seen>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Peer {
    /// An iQ-R wire PLC answering 0101 with `name`/`code`. D100 reads 7, every
    /// S bit reads 1.
    fn start(name: &'static str, code: u16) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Seen::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (seen, stop) = (seen.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let Ok((mut socket, _)) = listener.accept() else {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    };
                    socket.set_nonblocking(false).unwrap();
                    socket
                        .set_read_timeout(Some(Duration::from_millis(50)))
                        .unwrap();
                    seen.lock().unwrap().connections += 1;
                    loop {
                        if stop.load(Ordering::SeqCst) {
                            return;
                        }
                        let mut head = [0u8; 13];
                        match socket.read_exact(&mut head) {
                            Ok(()) => {}
                            Err(e)
                                if matches!(
                                    e.kind(),
                                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                                ) =>
                            {
                                continue
                            }
                            Err(_) => break,
                        }
                        assert_eq!(&head[..2], &[0x54, 0x00], "4E request");
                        let length = usize::from(u16::from_le_bytes([head[11], head[12]]));
                        let mut request = head.to_vec();
                        request.resize(13 + length, 0);
                        socket.set_read_timeout(None).unwrap();
                        socket.read_exact(&mut request[13..]).unwrap();
                        socket
                            .set_read_timeout(Some(Duration::from_millis(50)))
                            .unwrap();
                        let (end_code, body) = answer(name, code, &request, &seen);
                        let mut response = vec![0xd4, 0, request[2], request[3], 0, 0];
                        response.extend_from_slice(&request[6..11]);
                        response.extend_from_slice(
                            &u16::try_from(body.len() + 2).unwrap().to_le_bytes(),
                        );
                        response.extend_from_slice(&end_code.to_le_bytes());
                        response.extend_from_slice(&body);
                        if socket.write_all(&response).is_err() {
                            break;
                        }
                    }
                }
            })
        };
        Self {
            address,
            seen,
            stop,
            thread: Some(thread),
        }
    }

    fn all(&self) -> Vec<Vec<u8>> {
        self.seen.lock().unwrap().all.clone()
    }

    fn reads(&self) -> Vec<(u8, u32, u16, u16)> {
        self.seen.lock().unwrap().reads.clone()
    }

    fn connections(&self) -> u32 {
        self.seen.lock().unwrap().connections
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Answer one 4E request: 0101, the SD280-283 handshake, D word reads and S
/// bit reads. Anything else (a write, another device) fails the test.
fn answer(name: &str, code: u16, request: &[u8], seen: &Mutex<Seen>) -> (u16, Vec<u8>) {
    let command = u16::from_le_bytes([request[15], request[16]]);
    let sub = u16::from_le_bytes([request[17], request[18]]);
    seen.lock().unwrap().all.push(request[4..].to_vec());
    match command {
        0x0101 => {
            let mut body = vec![b' '; 18];
            body[..name.len()].copy_from_slice(name.as_bytes());
            body[16..18].copy_from_slice(&code.to_le_bytes());
            (0, body)
        }
        0x0401 => {
            assert!(matches!(sub, 2 | 3), "R word or bit access, got {sub:#x}");
            let start = u32::from_le_bytes([request[19], request[20], request[21], request[22]]);
            let device = request[23];
            let points = u16::from_le_bytes([request[25], request[26]]);
            seen.lock()
                .unwrap()
                .reads
                .push((device, start, points, sub));
            match (device, sub) {
                (SD, 2) if (start, points) == (280, 4) => {
                    let mut body = 4000u32.to_le_bytes().to_vec();
                    body.extend_from_slice(&4000u32.to_le_bytes());
                    (0, body)
                }
                (SD, _) => (END_CODE_C051, Vec::new()),
                (D, 2) => (
                    0,
                    (0..u32::from(points))
                        .flat_map(|at| if start + at == 100 { 7u16 } else { 0 }.to_le_bytes())
                        .collect(),
                ),
                // Bit units, two points per byte, high nibble first; an odd
                // count leaves the last low nibble 0.
                (S, 3) => {
                    let mut body = vec![0x11; usize::from(points).div_ceil(2)];
                    if points % 2 == 1 {
                        *body.last_mut().unwrap() = 0x10;
                    }
                    (0, body)
                }
                other => panic!("unexpected read {other:x?}"),
            }
        }
        other => panic!("unexpected command {other:#x}"),
    }
}

/// The 0101 request from the reserved bytes on.
fn type_name_frame() -> Vec<u8> {
    vec![
        0x00, 0x00, 0x00, 0xff, 0xff, 0x03, 0x00, 0x06, 0x00, 0x10, 0x00, 0x01, 0x01, 0x00, 0x00,
    ]
}

/// An iQ-R 0401 read (subcommand `sub`): 4-byte number, 2-byte device code,
/// points.
fn r_frame(sub: u16, device: u8, start: u32, points: u16) -> Vec<u8> {
    let mut data = vec![0x01, 0x04];
    data.extend_from_slice(&sub.to_le_bytes());
    data.extend_from_slice(&start.to_le_bytes());
    data.extend_from_slice(&[device, 0x00]);
    data.extend_from_slice(&points.to_le_bytes());
    let mut frame = vec![0x00, 0x00, 0x00, 0xff, 0xff, 0x03, 0x00];
    // The data length counts the 2-byte timer and the command data.
    frame.extend_from_slice(&u16::try_from(data.len() + 2).unwrap().to_le_bytes());
    frame.extend_from_slice(&[0x10, 0x00]);
    frame.extend_from_slice(&data);
    frame
}

/// The handshake of an iQ-R: 0101, then SD280-283.
fn r_handshake() -> Vec<Vec<u8>> {
    vec![type_name_frame(), r_frame(2, SD, 280, 4)]
}

fn tag(driver: &WasmDriver, address: &str, data_type: DataType) -> TagSpec {
    let kind = TagKind::numeric(data_type);
    TagSpec::new(driver.parse_address(address, &kind).unwrap(), kind)
}

fn driver() -> Arc<WasmDriver> {
    WasmDriver::load_operational_write(MANIFEST, &component()).unwrap()
}

async fn connect(driver: &Arc<WasmDriver>, peer: &Peer) -> FrameSession {
    let port = peer.address.port().to_string();
    let settings = Settings::from_pairs([("host", "127.0.0.1"), ("port", port.as_str())]).unwrap();
    assert!(driver.validate_config(&settings).is_empty());
    FrameSession::connect(
        driver.as_ref(),
        &settings,
        &TransportSpec::Tcp {
            host: "127.0.0.1".into(),
            port: peer.address.port(),
        },
        &ConnectContext::new(
            "slmp-wasm-operational-write",
            Duration::from_secs(2),
            Duration::from_secs(2),
        ),
        None,
    )
    .await
    .unwrap()
}

fn is_unsupported(result: &Result<Value, ItemError>) -> bool {
    matches!(result, Err(ItemError::Unsupported(_)))
}

/// Connects as `name`/`code`, reads `D100` and `S10` in one batch and returns
/// the peer (with the handshake and the batch in its record) and the results.
async fn run_reads(name: &'static str, code: u16) -> (Peer, Vec<Result<Value, ItemError>>) {
    let driver = driver();
    let peer = Peer::start(name, code);
    let mut session = connect(&driver, &peer).await;
    let specs = [
        tag(&driver, "D100", DataType::U16),
        tag(&driver, "S10", DataType::Bit),
    ];
    let values = session.read_batch(&specs).await.unwrap();
    assert_eq!(values.len(), specs.len());
    assert_eq!(peer.connections(), 1, "{name}: connection kept");
    session.disconnect().await;
    (peer, values)
}

fn sorted(mut frames: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    frames.sort();
    frames
}

/// The process CPU (RnPCPU) is standard R: S is read like R08CPU's, and the
/// request sequence is byte for byte the same as R08CPU's for the same batch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #72 component"]
async fn process_cpu_reads_s_like_standard_r() {
    let (std_peer, std_values) = run_reads("R08CPU", 0x4801).await;
    assert_eq!(std_values[0], Ok(Value::F64(7.0)));
    assert_eq!(std_values[1], Ok(Value::Bit(true)));
    for (name, code) in [
        ("R08PCPU", 0x4841),
        ("R16PCPU", 0x4842),
        ("R32PCPU", 0x4843),
        ("R120PCPU", 0x4844),
    ] {
        let (peer, values) = run_reads(name, code).await;
        assert_eq!(values, std_values, "{name}");
        let mut seen = peer.all();
        let batch = sorted(seen.split_off(2));
        assert_eq!(seen, r_handshake(), "{name}");
        assert_eq!(
            batch,
            sorted(vec![r_frame(2, D, 100, 1), r_frame(3, S, 10, 1)]),
            "{name}: D and S are both sent"
        );
        assert_eq!(
            sorted(peer.all()),
            sorted(std_peer.all()),
            "{name}: same requests as R08CPU"
        );
    }
}

/// SIL2 process CPU, safety CPU and MX-R have no S: the S tag is a per-item
/// `Unsupported` and no frame is sent for it, while D in the same batch is
/// read with the same frames as R08CPU's D.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #72 component"]
async fn sil2_safety_and_mxr_have_no_s_but_read_d_like_standard_r() {
    let (std_peer, _) = run_reads("R08CPU", 0x4801).await;
    // R08CPU's frames without the S read.
    let std_frames: Vec<Vec<u8>> = std_peer
        .all()
        .into_iter()
        .filter(|frame| frame != &r_frame(3, S, 10, 1))
        .collect();
    assert_eq!(std_frames.len(), 3);
    for (name, code) in [
        ("R08PSFCPU", 0x4851),
        ("R120PSFCPU", 0x4854),
        ("R08SFCPU", 0x4891),
        ("R32SFCPU", 0x4893),
        ("R120SFCPU", 0x4894),
        ("MXR300-16", 0x48e9),
        ("MXR300-64", 0x48eb),
        ("MXR500-128", 0x48ee),
        ("MXR500-256", 0x48ef),
    ] {
        let (peer, values) = run_reads(name, code).await;
        assert_eq!(values[0], Ok(Value::F64(7.0)), "{name}");
        assert!(is_unsupported(&values[1]), "{name}: S {:?}", values[1]);
        assert_eq!(peer.all(), std_frames, "{name}: same requests as R08CPU");
        assert_eq!(
            peer.reads(),
            vec![(SD, 280, 4, 2), (D, 100, 1, 2)],
            "{name}: no S read was sent"
        );
    }
}

/// The three no-S groups still read standard R's other devices with the same
/// handshake: a batch of only S tags sends nothing but the handshake and every
/// item is `Unsupported`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #72 component"]
async fn only_s_tags_send_nothing_on_a_no_s_model() {
    for (name, code) in [
        ("R16PSFCPU", 0x4852),
        ("R16SFCPU", 0x4892),
        ("MXR300-32", 0x48ea),
    ] {
        let driver = driver();
        let peer = Peer::start(name, code);
        let mut session = connect(&driver, &peer).await;
        let values = session
            .read_batch(&[
                tag(&driver, "S10", DataType::Bit),
                tag(&driver, "S11", DataType::Bit),
            ])
            .await
            .unwrap();
        assert!(values.iter().all(is_unsupported), "{name}: {values:?}");
        assert_eq!(peer.all(), r_handshake(), "{name}: only the handshake");
        session.disconnect().await;
    }
}

/// A (name, code) pair that is not in the table stays Unknown: the handshake
/// stops after 0101 (no SD read), and the read batch fails as a whole with a
/// protocol error (the configured range is unresolved); nothing but 0101 is
/// ever sent. This covers pairs mixed across the G2b groups and the route
/// fallbacks `MX-R`/037EH and `RCPU`/0360H.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #72 component"]
async fn mismatched_pairs_and_route_fallbacks_stay_unknown() {
    for (name, code) in [
        ("R08PCPU", 0x4851),
        ("R08PSFCPU", 0x4841),
        ("R08SFCPU", 0x4801),
        ("R16SFCPU", 0x4891),
        ("R120PSFCPU", 0x4894),
        ("MXR300-16", 0x48ea),
        ("MXR300-16", 0x4801),
        ("MXR500-64", 0x48ec),
        ("R04PCPU", 0x4840),
        ("MX-R", 0x037e),
        ("RCPU", 0x0360),
    ] {
        let driver = driver();
        let peer = Peer::start(name, code);
        let mut session = connect(&driver, &peer).await;
        assert_eq!(peer.all(), vec![type_name_frame()], "{name}/{code:#x}");
        let read = session
            .read_batch(&[
                tag(&driver, "D100", DataType::U16),
                tag(&driver, "S10", DataType::Bit),
            ])
            .await;
        assert!(
            matches!(read, Err(FatalError::Protocol(_))),
            "{name}/{code:#x}: {read:?}"
        );
        assert_eq!(
            peer.all(),
            vec![type_name_frame()],
            "{name}/{code:#x}: nothing but 0101 was sent"
        );
    }
}
