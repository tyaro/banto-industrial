//! Component-level checks of the SLMP guest's G2c model identification
//! (banto-driver-wasm #73, FX5 / MELSEC iQ-F) through this crate's adapter and
//! write gate (Hub side acceptance, mock only).
//!
//! Run with `BANTO_SLMP_NEW_COMPONENT_WASM=<built guest> cargo test -- --ignored`.
//! The guest now recognizes an FX5 when the 0101 type code is in the FX5 table
//! (48 codes: FX5U, FX5UC, FX5UJ, FX5S) and the type name starts with `FX5`.
//! It talks to an FX5 over 3E to its own station only, with the Q wire
//! (subcommands 0000/0001, 3-byte device number). The 0101 request keeps the
//! default monitoring timer 0010H (the model is not known yet); every later
//! request (SD, reads, writes) carries 0000H. The SD reads are SD280-283
//! (D and W) once, then SD304-305 (file register R) as a separate request, and
//! SD262-275 only when an M/L/B write is allowed. The file register `R<n>`
//! (device code AFH, decimal, word) is a read-only FX5 device; on Q and R
//! models it is a per-item `Unsupported`. FX5 X, Y, ZR, `U\G` and the like are
//! per-item `Unsupported` too.
//!
//! The previous pin `b705fd2` has no `R` device and classifies every FX5 pair
//! as Unknown, so all seven tests below fail there (checked 2026-11-06). Six
//! of them fail at the first `R10` tag, which does not parse on that pin
//! ("unsupported word device address"): that includes
//! `mismatched_pairs_stay_unknown` and
//! `fx5_over_4e_or_another_station_is_unknown`: their Unknown behaviour is not
//! new, but they use an `R` tag in the same batch, so they fail there for that
//! reason only; they guard that just the exact FX5 rule and the 3E own-station
//! path are recognized. `fx5_d_and_m_writes_...` fails there at the
//! handshake (0101 only, no SD read). The tests drive the real engine
//! (`FrameSession` over TCP)
//! against a loopback SLMP peer on a std thread that records every request
//! byte for byte (3E and 4E, the peer follows the sub-header it receives). The
//! peer is a mock; no PLC is involved. The real FX5 answering a 0010H 0101,
//! and the model name strings it returns, are not confirmed.
//!
//! Findings about the guest that the tests pin down:
//! - The model is classified by code-in-table plus name prefix only; the name
//!   is not paired with the family's code (an `FX5S` name with an `FX5U` code
//!   is still an FX5). Neither half alone is enough.
//! - An FX5 reached by 4E or by another station (network/station set) is
//!   treated like an unknown model: nothing after 0101 is sent, the read batch
//!   fails as a whole with a protocol error, and a write is refused per item.
//! - A read batch of an FX5 mixes freely: D and R are read while X, Y, ZR and
//!   `U\G` tags of the same batch are per-item `Unsupported` and never sent.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use banto_driver_api::{
    ConnectContext, DataType, DriverBase, FatalError, ItemError, Session as _, Settings, TagKind,
    TagSpec, Value, WriteItem,
};
use banto_driver_engine::{transport::TransportSpec, FrameSession};
use banto_driver_wasm::WasmDriver;

/// Same id as the Hub fixture manifest
/// `apps/banto-hub/host/tests/t12_slmp_wasm_operational_write/manifest.json`.
const MANIFEST: &[u8] = br#"{"manifestVersion":1,"id":"slmp-wasm-operational-write","version":"0.1.0","hubApi":"1","writeApi":"1","settingsVersion":1,"displayName":"SLMP G2c fixture","transports":["tcp","udp"],"capabilities":["read","write"]}"#;

fn component() -> Vec<u8> {
    std::fs::read(std::env::var("BANTO_SLMP_NEW_COMPONENT_WASM").expect("guest path")).unwrap()
}

const D: u8 = 0xa8;
const SD: u8 = 0xa9;
const R: u8 = 0xaf;
const END_CODE_C051: u16 = 0xc051;
/// SD280-283 (D, W) and SD304-305 (R) the FX5 peer answers.
const FX5_D_COUNT: u32 = 8000;
const FX5_W_COUNT: u32 = 0x200;
const FX5_R_COUNT: u32 = 32768;
/// SD262-275: Y, M, B, SB, F, (unused), L.
const FX5_BIT_SD: [u32; 7] = [0x100, 8000, 0x100, 0x200, 128, 0, 8000];
/// SD302 (internal D) and SD303 (internal W) a standard Q peer answers.
const Q_D_COUNT: u16 = 4000;
const Q_W_COUNT: u16 = 64;

const OWN: [u8; 5] = [0x00, 0xff, 0xff, 0x03, 0x00];

/// What the peer saw. Frames are whole requests (sub-header included) with the
/// 4E serial zeroed.
#[derive(Default)]
struct Seen {
    all: Vec<Vec<u8>>,
    /// (start, points) of each SD read.
    sd: Vec<(u32, u16)>,
    /// Plain 0401 reads of non-SD devices: (device, start, points).
    plain: Vec<(u8, u32, u16)>,
    /// 0401 device-extension (`U\G`) reads.
    ext: u32,
    /// 1401 writes.
    writes: Vec<Vec<u8>>,
    connections: u32,
}

struct Peer {
    address: SocketAddr,
    seen: Arc<Mutex<Seen>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// One request normalized over 3E and 4E.
struct Request<'a> {
    command: u16,
    sub: u16,
    /// Command data after the subcommand.
    data: &'a [u8],
}

fn parse(frame: &[u8]) -> Request<'_> {
    let at = if frame[0] == 0x50 { 11 } else { 15 };
    Request {
        command: u16::from_le_bytes([frame[at], frame[at + 1]]),
        sub: u16::from_le_bytes([frame[at + 2], frame[at + 3]]),
        data: &frame[at + 4..],
    }
}

impl Peer {
    /// A mock PLC answering 0101 with `name`/`code`. It follows the frame the
    /// request uses (3E `50 00` or 4E `54 00`). D100 reads 7, R10 reads 0x1234;
    /// every other word reads 0.
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
                        let mut sub_header = [0u8; 2];
                        match socket.read_exact(&mut sub_header) {
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
                        socket.set_read_timeout(None).unwrap();
                        let three_e = match sub_header {
                            [0x50, 0x00] => true,
                            [0x54, 0x00] => false,
                            other => panic!("unexpected sub-header {other:02x?}"),
                        };
                        // 3E: 7 more bytes to the length (at 7..9); 4E: 11.
                        let head_len = if three_e { 9 } else { 13 };
                        let mut request = sub_header.to_vec();
                        request.resize(head_len, 0);
                        socket.read_exact(&mut request[2..]).unwrap();
                        let length = usize::from(u16::from_le_bytes([
                            request[head_len - 2],
                            request[head_len - 1],
                        ]));
                        request.resize(head_len + length, 0);
                        socket.read_exact(&mut request[head_len..]).unwrap();
                        socket
                            .set_read_timeout(Some(Duration::from_millis(50)))
                            .unwrap();
                        let (end_code, body) = answer(name, code, &request, &seen);
                        let mut response = if three_e {
                            let mut head = vec![0xd0, 0];
                            head.extend_from_slice(&request[2..7]);
                            head
                        } else {
                            let mut head = vec![0xd4, 0, request[2], request[3], 0, 0];
                            head.extend_from_slice(&request[6..11]);
                            head
                        };
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

    fn sd(&self) -> Vec<(u32, u16)> {
        self.seen.lock().unwrap().sd.clone()
    }

    fn plain(&self) -> Vec<(u8, u32, u16)> {
        self.seen.lock().unwrap().plain.clone()
    }

    fn ext(&self) -> u32 {
        self.seen.lock().unwrap().ext
    }

    fn writes(&self) -> Vec<Vec<u8>> {
        self.seen.lock().unwrap().writes.clone()
    }

    fn connections(&self) -> u32 {
        self.seen.lock().unwrap().connections
    }

    fn clear(&self) {
        let mut seen = self.seen.lock().unwrap();
        seen.all.clear();
        seen.sd.clear();
        seen.plain.clear();
        seen.ext = 0;
        seen.writes.clear();
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

fn words_body(words: impl Iterator<Item = u16>) -> Vec<u8> {
    words.flat_map(u16::to_le_bytes).collect()
}

fn answer(name: &str, code: u16, request: &[u8], seen: &Mutex<Seen>) -> (u16, Vec<u8>) {
    let mut recorded = request.to_vec();
    if recorded[0] == 0x54 {
        recorded[2] = 0;
        recorded[3] = 0;
    }
    seen.lock().unwrap().all.push(recorded.clone());
    let Request { command, sub, data } = parse(request);
    match command {
        0x0101 => {
            let mut body = vec![b' '; 18];
            body[..name.len()].copy_from_slice(name.as_bytes());
            body[16..18].copy_from_slice(&code.to_le_bytes());
            (0, body)
        }
        0x1401 => {
            seen.lock().unwrap().writes.push(recorded);
            (0, Vec::new())
        }
        0x0401 if matches!(sub, 0x0080 | 0x0082) => {
            seen.lock().unwrap().ext += 1;
            (END_CODE_C051, Vec::new())
        }
        0x0401 => {
            // Q wire (sub 0: 3-byte number, code, points) or R wire (sub 2:
            // 4-byte number, 2-byte code, points).
            let (start, device, points) = match sub {
                0 => (
                    u32::from_le_bytes([data[0], data[1], data[2], 0]),
                    data[3],
                    u16::from_le_bytes([data[4], data[5]]),
                ),
                2 => (
                    u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
                    data[4],
                    u16::from_le_bytes([data[6], data[7]]),
                ),
                other => panic!("unexpected subcommand {other:#x}"),
            };
            if device == SD {
                seen.lock().unwrap().sd.push((start, points));
                return match (start, points) {
                    (280, 4) => {
                        let mut body = FX5_D_COUNT.to_le_bytes().to_vec();
                        body.extend_from_slice(&FX5_W_COUNT.to_le_bytes());
                        (0, body)
                    }
                    (304, 2) => (0, FX5_R_COUNT.to_le_bytes().to_vec()),
                    (262, 14) => (
                        0,
                        FX5_BIT_SD
                            .iter()
                            .flat_map(|count| count.to_le_bytes())
                            .collect(),
                    ),
                    (302, 1 | 2) => {
                        let mut body = Q_D_COUNT.to_le_bytes().to_vec();
                        if points == 2 {
                            body.extend_from_slice(&Q_W_COUNT.to_le_bytes());
                        }
                        (0, body)
                    }
                    _ => (END_CODE_C051, Vec::new()),
                };
            }
            seen.lock().unwrap().plain.push((device, start, points));
            let memory: BTreeMap<(u8, u32), u16> =
                [((D, 100), 7), ((R, 10), 0x1234)].into_iter().collect();
            match device {
                D | R => (
                    0,
                    words_body(
                        (0..u32::from(points))
                            .map(|at| memory.get(&(device, start + at)).copied().unwrap_or(0)),
                    ),
                ),
                _ => (END_CODE_C051, Vec::new()),
            }
        }
        other => panic!("unexpected command {other:#x}"),
    }
}

/// A 3E request with `route`, `timer` and the command `data`.
fn frame_3e(route: [u8; 5], timer: u16, data: &[u8]) -> Vec<u8> {
    let mut frame = vec![0x50, 0x00];
    frame.extend_from_slice(&route);
    // The data length counts the 2-byte timer and the command data.
    frame.extend_from_slice(&u16::try_from(data.len() + 2).unwrap().to_le_bytes());
    frame.extend_from_slice(&timer.to_le_bytes());
    frame.extend_from_slice(data);
    frame
}

/// A 4E request (serial zeroed) with the own-station route.
fn frame_4e(timer: u16, data: &[u8]) -> Vec<u8> {
    let mut frame = vec![0x54, 0x00, 0x00, 0x00, 0x00, 0x00];
    frame.extend_from_slice(&OWN);
    frame.extend_from_slice(&u16::try_from(data.len() + 2).unwrap().to_le_bytes());
    frame.extend_from_slice(&timer.to_le_bytes());
    frame.extend_from_slice(data);
    frame
}

const TYPE_NAME: [u8; 4] = [0x01, 0x01, 0x00, 0x00];

/// 0101 on 3E, own station, with the default timer 0010H.
fn type_name_3e() -> Vec<u8> {
    frame_3e(OWN, 0x0010, &TYPE_NAME)
}

/// A Q-wire word read (0401, subcommand 0000): 3-byte number, code, points.
fn q_read(device: u8, start: u32, points: u16, timer: u16) -> Vec<u8> {
    let mut data = vec![0x01, 0x04, 0x00, 0x00];
    data.extend_from_slice(&start.to_le_bytes()[..3]);
    data.push(device);
    data.extend_from_slice(&points.to_le_bytes());
    frame_3e(OWN, timer, &data)
}

/// The 3E handshake of an FX5: 0101 (0010H), SD280-283, SD304-305 (0000H).
fn fx5_handshake() -> Vec<Vec<u8>> {
    vec![type_name_3e(), q_read(SD, 280, 4, 0), q_read(SD, 304, 2, 0)]
}

/// A Q 1401 word write of `words` at `start` of `device`, timer 0000H.
fn q_word_write(device: u8, start: u32, words: &[u16]) -> Vec<u8> {
    let mut data = vec![0x01, 0x14, 0x00, 0x00];
    data.extend_from_slice(&start.to_le_bytes()[..3]);
    data.push(device);
    data.extend_from_slice(&u16::try_from(words.len()).unwrap().to_le_bytes());
    data.extend(words.iter().copied().flat_map(u16::to_le_bytes));
    frame_3e(OWN, 0, &data)
}

/// An iQ-R word read in a 4E frame (subcommand 0002), timer 0010H.
fn r_read_4e(device: u8, start: u32, points: u16) -> Vec<u8> {
    let mut data = vec![0x01, 0x04, 0x02, 0x00];
    data.extend_from_slice(&start.to_le_bytes());
    data.extend_from_slice(&[device, 0x00]);
    data.extend_from_slice(&points.to_le_bytes());
    frame_4e(0x0010, &data)
}

fn tag(driver: &WasmDriver, address: &str, data_type: DataType) -> TagSpec {
    let kind = TagKind::numeric(data_type);
    TagSpec::new(driver.parse_address(address, &kind).unwrap(), kind)
}

fn driver() -> Arc<WasmDriver> {
    WasmDriver::load_operational_write(MANIFEST, &component()).unwrap()
}

async fn connect(driver: &Arc<WasmDriver>, peer: &Peer, extra: &[(&str, &str)]) -> FrameSession {
    let port = peer.address.port().to_string();
    let mut pairs = vec![("host", "127.0.0.1"), ("port", port.as_str())];
    pairs.extend_from_slice(extra);
    let settings = Settings::from_pairs(pairs).unwrap();
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

fn sorted(mut frames: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    frames.sort();
    frames
}

const FX5_3E: &[(&str, &str)] = &[("frame", "3e")];

/// The FX5 code table of the guest (SLMP manual JY997D56001 K pp.109-110),
/// with the name prefix each family carries.
fn fx5_pairs() -> Vec<(String, u16)> {
    let families: [(&str, &[u16]); 4] = [
        (
            "FX5U",
            &[
                0x4a21, 0x4a23, 0x4a24, 0x4a29, 0x4a2b, 0x4a2c, 0x4a31, 0x4a33, 0x4a34, 0x4a41,
                0x4a43, 0x4a44, 0x4a49, 0x4a4b, 0x4a4c, 0x4a51, 0x4a53, 0x4a54,
            ],
        ),
        (
            "FX5UC",
            &[
                0x4a91, 0x4a92, 0x4a93, 0x4a99, 0x4a9a, 0x4a9b, 0x4aa9, 0x4ab1, 0x4ab9,
            ],
        ),
        (
            "FX5UJ",
            &[
                0x4b0d, 0x4b0e, 0x4b0f, 0x4b14, 0x4b15, 0x4b16, 0x4b1b, 0x4b1c, 0x4b1d,
            ],
        ),
        (
            "FX5S",
            &[
                0x4b4e, 0x4b4f, 0x4b50, 0x4b51, 0x4b55, 0x4b56, 0x4b57, 0x4b58, 0x4b5c, 0x4b5d,
                0x4b5e, 0x4b5f,
            ],
        ),
    ];
    families
        .iter()
        .flat_map(|(prefix, codes)| {
            codes
                .iter()
                .map(move |&code| (format!("{prefix}-32MR/ES"), code))
        })
        .collect()
}

/// Leaks a name so the peer thread can own it (a test-only convenience).
fn leak(name: &str) -> &'static str {
    Box::leak(name.to_string().into_boxed_str())
}

/// Every code of the FX5 table (FX5U, FX5UC, FX5UJ, FX5S: 48 codes) with an
/// `FX5` name is an FX5 on a 3E own-station connection: the requests are
/// byte for byte the 0101 with 0010H, SD280-283 and SD304-305 with 0000H, then
/// the batch (a D word read and the file register `R<n>` word read, AFH,
/// decimal) with 0000H on the Q wire. The values come back; the connection is
/// kept.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #73 component"]
async fn fx5_pairs_are_recognized_and_read_d_and_the_file_register() {
    let pairs = fx5_pairs();
    assert_eq!(pairs.len(), 48);
    // Real-looking names of each family first, then every table code.
    let mut all = vec![
        ("FX5U-32MR/ES".to_string(), 0x4a21u16),
        ("FX5U-80MT/DSS".to_string(), 0x4a54),
        ("FX5UC-32MT/D".to_string(), 0x4a91),
        ("FX5UC-96MT/DSS".to_string(), 0x4ab9),
        ("FX5UJ-24MR/ES".to_string(), 0x4b0d),
        ("FX5UJ-60MT/ESS".to_string(), 0x4b1d),
        ("FX5S-30MR/ES".to_string(), 0x4b4e),
        ("FX5S-80MT/DSS".to_string(), 0x4b5f),
        // The name is not paired with the family's code (see the module doc).
        ("FX5S-30MR/ES".to_string(), 0x4a21),
    ];
    all.extend(pairs);
    let driver = driver();
    for (name, code) in all {
        let peer = Peer::start(leak(&name), code);
        let mut session = connect(&driver, &peer, FX5_3E).await;
        let values = session
            .read_batch(&[
                tag(&driver, "D100", DataType::U16),
                tag(&driver, "R10", DataType::U16),
            ])
            .await
            .unwrap();
        assert_eq!(values[0], Ok(Value::F64(7.0)), "{name}/{code:#x}");
        assert_eq!(values[1], Ok(Value::F64(f64::from(0x1234u16))), "{name}");
        assert_eq!(peer.connections(), 1, "{name}: connection kept");
        let mut seen = peer.all();
        let batch = sorted(seen.split_off(3));
        assert_eq!(seen, fx5_handshake(), "{name}/{code:#x}");
        assert_eq!(
            batch,
            sorted(vec![q_read(D, 100, 1, 0), q_read(R, 10, 1, 0)]),
            "{name}/{code:#x}: D (A8H) and R (AFH, decimal 10), timer 0000H"
        );
        assert_eq!(peer.sd(), vec![(280, 4), (304, 2)], "{name}");
        assert_eq!(peer.ext(), 0, "{name}");
        session.disconnect().await;
    }
}

/// R numbers are decimal and read as plain Q words: `R0100` is R100 (AFH, start
/// 100) and a 32-bit tag takes two words; adjacent R tags are merged like D.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #73 component"]
async fn fx5_file_register_numbers_are_decimal_and_adjacent_ones_merge() {
    let driver = driver();
    let peer = Peer::start("FX5U-32MR/ES", 0x4a21);
    let mut session = connect(&driver, &peer, FX5_3E).await;
    let values = session
        .read_batch(&[
            tag(&driver, "R0100", DataType::U16),
            tag(&driver, "R32767", DataType::U16),
            tag(&driver, "R10", DataType::U32),
        ])
        .await
        .unwrap();
    assert_eq!(values.len(), 3);
    assert!(values[0].is_ok() && values[1].is_ok(), "{values:?}");
    // R10 u32 = words R10 (0x1234) and R11 (0), low word first.
    assert_eq!(
        values[2],
        Ok(Value::F64(f64::from(0x1234u32))),
        "{values:?}"
    );
    let mut seen = peer.all();
    let batch = sorted(seen.split_off(3));
    assert_eq!(seen, fx5_handshake());
    assert_eq!(
        batch,
        sorted(vec![
            q_read(R, 100, 1, 0),
            q_read(R, 32767, 1, 0),
            q_read(R, 10, 2, 0)
        ])
    );
    // R past SD304-305 (32768 points) is refused before it is sent.
    peer.clear();
    let values = session
        .read_batch(&[tag(&driver, "R32768", DataType::U16)])
        .await;
    if let Ok(items) = values {
        assert!(items.iter().all(Result::is_err), "{items:?}");
    }
    assert!(
        peer.plain().iter().all(|&(device, ..)| device != R),
        "R32768 is never sent: {:?}",
        peer.plain()
    );
    session.disconnect().await;
}

/// On an FX5 the devices it does not have (or that are not enabled yet) are
/// per-item `Unsupported` and never sent, in a batch that still reads D and R:
/// X, Y, ZR, RD, LTN, LCN and `U\G`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #73 component"]
async fn fx5_unsupported_devices_are_per_item_and_the_batch_still_reads_d_and_r() {
    let driver = driver();
    let peer = Peer::start("FX5UJ-24MR/ES", 0x4b0d);
    let mut session = connect(&driver, &peer, FX5_3E).await;
    let specs = [
        tag(&driver, "D100", DataType::U16),
        tag(&driver, "X0", DataType::Bit),
        tag(&driver, "Y0", DataType::Bit),
        tag(&driver, "ZR10", DataType::U16),
        tag(&driver, "RD10", DataType::U16),
        tag(&driver, "LCN0", DataType::I32),
        tag(&driver, "LTN0", DataType::I32),
        tag(&driver, "U0\\G7", DataType::U16),
        tag(&driver, "U3E0\\G7", DataType::U16),
        tag(&driver, "R10", DataType::U16),
    ];
    let values = session.read_batch(&specs).await.unwrap();
    assert_eq!(values.len(), specs.len());
    assert_eq!(values[0], Ok(Value::F64(7.0)));
    assert_eq!(values[9], Ok(Value::F64(f64::from(0x1234u16))));
    for (at, label) in [
        (1, "X0"),
        (2, "Y0"),
        (3, "ZR10"),
        (4, "RD10"),
        (5, "LCN0"),
        (6, "LTN0"),
        (7, "U0\\G7"),
        (8, "U3E0\\G7"),
    ] {
        assert!(is_unsupported(&values[at]), "{label}: {:?}", values[at]);
    }
    let mut seen = peer.all();
    let batch = sorted(seen.split_off(3));
    assert_eq!(seen, fx5_handshake());
    assert_eq!(
        batch,
        sorted(vec![q_read(D, 100, 1, 0), q_read(R, 10, 1, 0)]),
        "exactly D and R are sent"
    );
    assert_eq!(peer.ext(), 0, "no U\\G request");
    assert_eq!(peer.connections(), 1);
    session.disconnect().await;
}

/// The file register `R<n>` exists on an FX5 only: on a standard Q (3E) and on
/// an iQ-R R08CPU (4E, R wire) it is a per-item `Unsupported` that is never
/// sent, while the D tag of the same batch is read as before.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #73 component"]
async fn file_register_r_is_unsupported_on_standard_q_and_r() {
    // Standard Q (Q06HCPU/0042H) on 3E: 0101 with 0010H, SD302-303, then D.
    let driver = driver();
    let peer = Peer::start("Q06HCPU", 0x0042);
    let mut session = connect(&driver, &peer, FX5_3E).await;
    let values = session
        .read_batch(&[
            tag(&driver, "D100", DataType::U16),
            tag(&driver, "R10", DataType::U16),
        ])
        .await
        .unwrap();
    assert_eq!(values[0], Ok(Value::F64(7.0)));
    assert!(is_unsupported(&values[1]), "Q: {:?}", values[1]);
    assert_eq!(
        peer.all(),
        vec![
            type_name_3e(),
            q_read(SD, 302, 2, 0x0010),
            q_read(D, 100, 1, 0x0010)
        ]
    );
    session.disconnect().await;

    // Standard R (R08CPU/4801H) on 4E: 0101, SD280-283, then D.
    let driver = self::driver();
    let peer = Peer::start("R08CPU", 0x4801);
    let mut session = connect(&driver, &peer, &[]).await;
    let values = session
        .read_batch(&[
            tag(&driver, "D100", DataType::U16),
            tag(&driver, "R10", DataType::U16),
        ])
        .await
        .unwrap();
    assert_eq!(values[0], Ok(Value::F64(7.0)));
    assert!(is_unsupported(&values[1]), "R: {:?}", values[1]);
    assert_eq!(
        peer.all(),
        vec![
            frame_4e(0x0010, &TYPE_NAME),
            r_read_4e(SD, 280, 4),
            r_read_4e(D, 100, 1)
        ]
    );
    assert!(peer.plain().iter().all(|&(device, ..)| device != R));
    session.disconnect().await;
}

/// Both halves of the FX5 rule are needed: an FX5-table code under a name that
/// does not start with `FX5`, and an `FX5` name with a code that is not in the
/// table, stay Unknown. The handshake stops after 0101 (no SD read), the read
/// batch fails as a whole with a protocol error, nothing but 0101 is ever sent
/// and a write is refused per item. This passes on the previous pin too (it
/// never recognized FX5); it guards that only the exact rule is recognized.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #73 component"]
async fn mismatched_pairs_stay_unknown() {
    let driver = driver();
    for (name, code) in [
        // An FX5 code, but not an FX5 name.
        ("FX3U-32MR/ES", 0x4a21),
        ("Q02UCPU", 0x4a21),
        ("R08CPU", 0x4a54),
        ("MXR300-32", 0x4b4e),
        // An FX5 name, but a code outside the FX5 table.
        ("FX5U-32MR/ES", 0x4a22),
        ("FX5U-32MR/ES", 0x4a20),
        ("FX5UC-32MT/D", 0x4a90),
        ("FX5UJ-24MR/ES", 0x4b0c),
        ("FX5S-30MR/ES", 0x4b4d),
        ("FX5U-32MR/ES", 0x0263),
        ("FX5U-32MR/ES", 0x4801),
        ("FX5", 0x0000),
    ] {
        let peer = Peer::start(name, code);
        let mut session = connect(
            &driver,
            &peer,
            &[
                ("frame", "3e"),
                ("d_u16_write_mode", "enabled"),
                ("d_u16_write_allowlist", "D3010-D3013"),
            ],
        )
        .await;
        assert_eq!(peer.all(), vec![type_name_3e()], "{name}/{code:#x}");
        let results = session
            .write_batch(&[WriteItem::new(
                tag(&driver, "D3010", DataType::U16),
                Value::F64(1.0),
            )])
            .await
            .unwrap();
        assert!(
            matches!(results.as_slice(), [Err(_)]),
            "{name}/{code:#x}: {results:?}"
        );
        let read = session
            .read_batch(&[
                tag(&driver, "D100", DataType::U16),
                tag(&driver, "R10", DataType::U16),
            ])
            .await;
        assert!(
            matches!(read, Err(FatalError::Protocol(_))),
            "{name}/{code:#x}: {read:?}"
        );
        assert_eq!(
            peer.all(),
            vec![type_name_3e()],
            "{name}/{code:#x}: nothing but 0101 was sent"
        );
        assert!(peer.writes().is_empty(), "{name}/{code:#x}");
    }
}

/// (label, extra settings, expected 0101 frame; empty = checked structurally)
type Case<'a> = (&'a str, &'a [(&'a str, &'a str)], Vec<u8>);

/// An FX5 pair reached by 4E, or by another station over 3E (network or
/// station set, or a multi-CPU target), is handled like an unknown model: the
/// 0101 (still 0010H) is the only request, the read batch fails with a protocol
/// error, and a write is refused per item without a 1401. The mock answers a
/// 4E 0101 although a real FX5 does not (an FX5 answers 3E only).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #73 component"]
async fn fx5_over_4e_or_another_station_is_unknown() {
    let cases: [Case; 4] = [
        ("4e", &[("frame", "4e")], frame_4e(0x0010, &TYPE_NAME)),
        ("default frame", &[], frame_4e(0x0010, &TYPE_NAME)),
        (
            "3e other station",
            &[("frame", "3e"), ("network_no", "1"), ("station_no", "1")],
            frame_3e([0x01, 0x01, 0xff, 0x03, 0x00], 0x0010, &TYPE_NAME),
        ),
        (
            "3e cpu1",
            &[("frame", "3e"), ("target_cpu", "cpu1")],
            // Checked against the peer's record below, not assumed here.
            Vec::new(),
        ),
    ];
    for (label, extra, expected_0101) in cases {
        let driver = driver();
        let peer = Peer::start("FX5U-32MR/ES", 0x4a21);
        let mut settings = vec![
            ("d_u16_write_mode", "enabled"),
            ("d_u16_write_allowlist", "D3010-D3013"),
        ];
        settings.extend_from_slice(extra);
        let mut session = connect(&driver, &peer, &settings).await;
        let sent = peer.all();
        assert_eq!(sent.len(), 1, "{label}: {sent:02x?}");
        if expected_0101.is_empty() {
            assert_eq!(sent[0][0..2], [0x50, 0x00], "{label}");
            assert_eq!(sent[0][9..11], [0x10, 0x00], "{label}: timer 0010H");
            assert_eq!(sent[0][11..], TYPE_NAME, "{label}");
            assert_ne!(sent[0][2..7], OWN, "{label}: not the own station");
        } else {
            assert_eq!(sent[0], expected_0101, "{label}");
        }
        let results = session
            .write_batch(&[WriteItem::new(
                tag(&driver, "D3010", DataType::U16),
                Value::F64(1.0),
            )])
            .await
            .unwrap();
        assert!(
            matches!(results.as_slice(), [Err(_)]),
            "{label}: {results:?}"
        );
        let read = session
            .read_batch(&[
                tag(&driver, "D100", DataType::U16),
                tag(&driver, "R10", DataType::U16),
            ])
            .await;
        assert!(
            matches!(read, Err(FatalError::Protocol(_))),
            "{label}: {read:?}"
        );
        assert_eq!(peer.all(), sent, "{label}: nothing but 0101 was sent");
        assert!(peer.sd().is_empty(), "{label}: no SD read");
        assert!(peer.plain().is_empty(), "{label}: no device read");
        assert!(peer.writes().is_empty(), "{label}: no 1401");
        session.disconnect().await;
    }
}

/// An allowed FX5 write goes through the Hub gate and the guest's standard-R
/// rules on the Q wire: a D word is one 1401/0000 (3-byte number, A8H) with
/// monitoring timer 0000H; an M bit is 1401/0001 (the SD262-275 request is
/// made at the handshake because M is allowed); `R<n>` is refused per item
/// before anything is sent, even next to an allowed D write in the same batch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #73 component"]
async fn fx5_d_and_m_writes_are_one_1401_with_timer_0000_and_r_is_refused() {
    let driver = driver();
    let peer = Peer::start("FX5U-32MR/ES", 0x4a21);
    let mut session = connect(
        &driver,
        &peer,
        &[
            ("frame", "3e"),
            ("d_u16_write_mode", "enabled"),
            ("d_u16_write_allowlist", "D3010-D3013,M0-M7"),
        ],
    )
    .await;
    // M is allowed, so SD262-275 is read after SD280-283 and SD304-305.
    assert_eq!(
        peer.all(),
        vec![
            type_name_3e(),
            q_read(SD, 280, 4, 0),
            q_read(SD, 304, 2, 0),
            q_read(SD, 262, 14, 0),
        ]
    );

    peer.clear();
    let results = session
        .write_batch(&[
            WriteItem::new(tag(&driver, "D3010", DataType::U16), Value::F64(4660.0)),
            WriteItem::new(tag(&driver, "D3011", DataType::U16), Value::F64(22136.0)),
        ])
        .await
        .unwrap();
    assert!(results.iter().all(Result::is_ok), "{results:?}");
    assert_eq!(
        peer.writes(),
        vec![q_word_write(D, 3010, &[0x1234, 0x5678])],
        "one 1401/0000 for D3010-D3011, timer 0000H"
    );

    peer.clear();
    let results = session
        .write_batch(&[WriteItem::new(
            tag(&driver, "M3", DataType::Bit),
            Value::Bit(true),
        )])
        .await
        .unwrap();
    assert!(results.iter().all(Result::is_ok), "{results:?}");
    assert_eq!(
        peer.writes(),
        vec![frame_3e(
            OWN,
            0,
            &[0x01, 0x14, 0x01, 0x00, 0x03, 0x00, 0x00, 0x90, 0x01, 0x00, 0x10]
        )],
        "one 1401/0001 for M3 (code 90H), timer 0000H"
    );

    // R is never written, alone or next to an allowed D write.
    peer.clear();
    let results = session
        .write_batch(&[
            WriteItem::new(tag(&driver, "R10", DataType::U16), Value::F64(1.0)),
            WriteItem::new(tag(&driver, "D3012", DataType::U16), Value::F64(9.0)),
        ])
        .await
        .unwrap();
    assert_eq!(results.len(), 2);
    assert!(results[0].is_err(), "R write: {:?}", results[0]);
    println!("R write result: {:?}", results[0]);
    for frame in peer.writes() {
        // 3E: command at 11..13, subcommand 13..15, number 15..18, device 18.
        assert_ne!(frame[18], R, "R must never be written: {frame:02x?}");
    }
    assert!(
        peer.plain().iter().all(|&(device, ..)| device != R),
        "R is never read for a write: {:?}",
        peer.plain()
    );
    // Outside the allowlist nothing is sent.
    peer.clear();
    let results = session
        .write_batch(&[WriteItem::new(
            tag(&driver, "D3014", DataType::U16),
            Value::F64(1.0),
        )])
        .await
        .unwrap();
    assert!(matches!(results.as_slice(), [Err(_)]), "{results:?}");
    assert!(peer.writes().is_empty());
    session.disconnect().await;
}
