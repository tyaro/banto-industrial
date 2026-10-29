//! Normal D/u16 policy acceptance through an isolated loopback SLMP peer.

use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, Ordering};
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

/// Wire behaviour the new-guest E1/E2 tests choose explicitly. The default is
/// the 4E, own-station behaviour every earlier test relies on.
#[derive(Clone, Copy, Default)]
struct Wire {
    /// Answer a 3E request with a 4E (`D4`) response header.
    answer_4e_to_3e: bool,
    /// Answer with the own-station route (00 FF FF03 00) instead of the
    /// request's route.
    fold_own_route: bool,
    /// Added to the response length field (the bytes sent do not change).
    length_delta: i32,
    /// End code for D/W word reads outside the SD handshake (0 = answer);
    /// guest #61 (F) step 1 and 3 of the word-bit read-modify-write.
    word_read_end_code: u16,
    /// Answer D/W word reads as if no write had been stored (every word 7),
    /// so a word-bit confirm read disagrees with the write-back (guest #61).
    stale_word_reads: bool,
}

/// Subheader (first two bytes) and route (network, station, I/O LE, multidrop)
/// of one request as it arrived on the wire.
type RecordedHeader = ([u8; 2], [u8; 5]);

struct Fixture {
    wire: Arc<Mutex<Wire>>,
    headers: Arc<Mutex<Vec<RecordedHeader>>>,
    connections: Arc<AtomicU32>,
    address: std::net::SocketAddr,
    frames: RecordedFrames,
    commands: Arc<Mutex<Vec<(u16, u32, u16)>>>,
    read_spans: RecordedReads,
    count: Arc<AtomicU32>,
    diagnostic_bit_counts: Arc<Mutex<[u32; 3]>>,
    refuse_bit_sd: Arc<AtomicBool>,
    q_mb_count: Arc<AtomicU32>,
    write_devices: Arc<Mutex<Vec<u8>>>,
    write_end_code: Arc<AtomicU16>,
    bit_frames: RecordedBits,
    q_w_total: Arc<Mutex<Option<u32>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn start(peer: Peer, count: u32, bad_ack: bool) -> Self {
        Self::start_wire(peer, count, bad_ack, Wire::default()).await
    }

    async fn start_wire(peer: Peer, count: u32, bad_ack: bool, wire: Wire) -> Self {
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
        let q_mb_count = Arc::new(AtomicU32::new(64));
        let q_mb_count_on_wire = q_mb_count.clone();
        let write_devices = Arc::new(Mutex::new(Vec::new()));
        let devices = write_devices.clone();
        let write_end_code = Arc::new(AtomicU16::new(0));
        let write_end_code_on_wire = write_end_code.clone();
        let bit_frames: RecordedBits = Arc::new(Mutex::new(Vec::new()));
        let bits_written = bit_frames.clone();
        let wire = Arc::new(Mutex::new(wire));
        let wire_on_wire = wire.clone();
        let headers: Arc<Mutex<Vec<RecordedHeader>>> = Arc::new(Mutex::new(Vec::new()));
        let headers_on_wire = headers.clone();
        let connections = Arc::new(AtomicU32::new(0));
        let connections_on_wire = connections.clone();
        let q_w_total: Arc<Mutex<Option<u32>>> = Arc::new(Mutex::new(None));
        let q_w_total_on_wire = q_w_total.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                connections_on_wire.fetch_add(1, Ordering::SeqCst);
                loop {
                    // 4E: 5400 serial reserved route length(11..13) ...;
                    // 3E: 5000 route length(7..9) .... A 3E request is
                    // normalised to the 4E layout below so the handlers
                    // keep their offsets; only the response framing differs.
                    let mut sub = [0; 2];
                    if socket.read_exact(&mut sub).await.is_err() {
                        break;
                    }
                    let three_e = sub == [0x50, 0x00];
                    assert!(three_e || sub == [0x54, 0x00], "subheader {sub:02x?}");
                    let mut rest = vec![0; if three_e { 7 } else { 11 }];
                    if socket.read_exact(&mut rest).await.is_err() {
                        break;
                    }
                    let route: [u8; 5] = rest[rest.len() - 7..rest.len() - 2].try_into().unwrap();
                    let length = usize::from(u16::from_le_bytes([
                        rest[rest.len() - 2],
                        rest[rest.len() - 1],
                    ]));
                    let mut request = if three_e {
                        vec![0x54, 0, 0, 0, 0, 0]
                    } else {
                        sub.to_vec()
                    };
                    request.extend_from_slice(&rest);
                    request.resize(13 + length, 0);
                    if socket.read_exact(&mut request[13..]).await.is_err() {
                        break;
                    }
                    headers_on_wire.lock().unwrap().push((sub, route));
                    let wire = *wire_on_wire.lock().unwrap();
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
                                    if (subcommand, start, words) == (0, 310, 2) {
                                        // Guest #60 (D3): QnU reads the internal +
                                        // extended W total. Refused unless a test
                                        // opts in, which keeps the internal-W-only
                                        // behaviour every earlier test relies on.
                                        match *q_w_total_on_wire.lock().unwrap() {
                                            Some(total) => (0, total.to_le_bytes().to_vec()),
                                            None => (0xc051, Vec::new()),
                                        }
                                    } else if (subcommand, start, words) == (0, 290, 5) {
                                        if refuse_bit_sd_on_wire.load(Ordering::SeqCst) {
                                            (0xc051, Vec::new())
                                        } else {
                                            let [x, _, _] = *bit_counts_on_wire.lock().unwrap();
                                            let mb = u16::try_from(
                                                q_mb_count_on_wire.load(Ordering::SeqCst),
                                            )
                                            .unwrap();
                                            let body = [u16::try_from(x).unwrap(), 64, mb, 64, mb]
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
                                        && ((start == 0 && end == 1) // connection-test probe D0
                                            || (start == 10 && end == 11)
                                            || (start == 3000 && end == 3001)
                                            || (start >= 3010 && end <= 3015)))
                                        || (device == 0xb4 && start >= 0x1a && end <= 0x20)
                                        // Q extended W below the opted-in SD310-311
                                        // total (guest #60 D3, new-guest tests only).
                                        || (device == 0xb4
                                            && matches!(peer, Peer::StandardQ)
                                            && start >= 0x40
                                            && q_w_total_on_wire
                                                .lock()
                                                .unwrap()
                                                .is_some_and(|total| end <= total))
                                        || (device == 0x90 && start >= 10 && end <= 13)
                                        // Explicit reads at the Q/R M boundary and past a
                                        // saturated Q M/B count (new-guest tests only).
                                        || (device == 0x90 && start >= 63 && end <= 65)
                                        || (matches!(device, 0x90 | 0xa0)
                                            && matches!(peer, Peer::StandardQ)
                                            && start >= 32767
                                            && end <= 40001)
                                        || (device == 0x9c
                                            && matches!(peer, Peer::StandardQ)
                                            && end <= 32768)
                                        || (device == 0xa1
                                            && matches!(peer, Peer::R)
                                            && end <= 65539)
                                        || (device == 0x93 && matches!(peer, Peer::R) && end <= 3)
                                        // Unknown-range read-only devices (new-guest tests only):
                                        // S10, Z3, RD7 and ZR10, one tag per request.
                                        || (device == 0x98 && (start, end) == (10, 11))
                                        || (device == 0xcc && (start, end) == (3, 4))
                                        || (device == 0x2c && (start, end) == (7, 8))
                                        || (device == 0xb0 && (start, end) == (0x10, 0x11)))
                            );
                            if matches!(device, 0x90 | 0x9c | 0xa1 | 0x93 | 0xa0 | 0x98) {
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
                            } else if wire.word_read_end_code != 0 && matches!(device, 0xa8 | 0xb4)
                            {
                                (wire.word_read_end_code, Vec::new())
                            } else {
                                let stored = if wire.stale_word_reads {
                                    None
                                } else {
                                    writes.lock().unwrap().last().cloned()
                                };
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
                            (write_end_code_on_wire.load(Ordering::SeqCst), Vec::new())
                        }
                        _ => panic!("unexpected SLMP command {command:#x}"),
                    };
                    let route_out: [u8; 5] = if wire.fold_own_route {
                        [0, 0xff, 0xff, 0x03, 0]
                    } else {
                        route
                    };
                    let mut response = if three_e && !wire.answer_4e_to_3e {
                        vec![0xd0, 0]
                    } else {
                        let mut head = vec![0xd4, 0];
                        head.extend_from_slice(&request[2..6]);
                        head
                    };
                    response.extend_from_slice(&route_out);
                    let announced = i32::try_from(body.len() + 2).unwrap() + wire.length_delta;
                    response.extend_from_slice(&u16::try_from(announced).unwrap().to_le_bytes());
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
            wire,
            headers,
            connections,
            address,
            frames,
            commands,
            read_spans,
            count: live_count,
            diagnostic_bit_counts,
            refuse_bit_sd,
            q_mb_count,
            write_devices,
            write_end_code,
            bit_frames,
            q_w_total,
            task,
        }
    }

    /// Answer the QnU SD310-311 total (guest #60 D3) with `total`; the default
    /// `None` refuses it with an end code.
    fn set_q_w_total(&self, total: Option<u32>) {
        *self.q_w_total.lock().unwrap() = total;
    }

    fn set_wire(&self, wire: Wire) {
        *self.wire.lock().unwrap() = wire;
    }

    fn headers(&self) -> Vec<RecordedHeader> {
        self.headers.lock().unwrap().clone()
    }

    fn connections(&self) -> u32 {
        self.connections.load(Ordering::SeqCst)
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

    /// Q M/B point count the SD answers with (default 64; 32768 = saturated field).
    fn set_q_mb_count(&self, count: u32) {
        self.q_mb_count.store(count, Ordering::SeqCst);
    }

    /// End code the fixture answers to every 0x1401 write with (0 = success).
    fn set_write_end_code(&self, end_code: u16) {
        self.write_end_code.store(end_code, Ordering::SeqCst);
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
    connect_new_guest_with(peer, "disabled", "").await
}

async fn connect_new_guest_with(
    peer: Peer,
    mode: &str,
    allowlist: &str,
) -> (Fixture, Arc<WasmDriver>, FrameSession) {
    let (fixture, driver, session) =
        connect_new_guest_cfg(peer, Wire::default(), mode, allowlist, &[]).await;
    (fixture, driver, session.unwrap())
}

/// Like `connect_new_guest_with`, with extra connection settings (E1 `frame`,
/// E2 `network_no`/`station_no`/`target_cpu`) and a chosen wire behaviour. The
/// connect result is returned as is so a test can expect it to fail.
async fn connect_new_guest_cfg(
    peer: Peer,
    wire: Wire,
    mode: &str,
    allowlist: &str,
    extra: &[(&str, &str)],
) -> (
    Fixture,
    Arc<WasmDriver>,
    Result<FrameSession, impl std::fmt::Debug>,
) {
    let fixture = Fixture::start_wire(peer, 4000, false, wire).await;
    let (driver, session) = connect_new_guest_fixture(&fixture, mode, allowlist, extra).await;
    (fixture, driver, session)
}

/// Connect the new guest to an already started fixture, so a test can set the
/// fixture's handshake answers (D3 SD310-311) before the handshake runs.
async fn connect_new_guest_fixture(
    fixture: &Fixture,
    mode: &str,
    allowlist: &str,
    extra: &[(&str, &str)],
) -> (Arc<WasmDriver>, Result<FrameSession, impl std::fmt::Debug>) {
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
    let mut pairs: Vec<(String, String)> = [
        ("host", "127.0.0.1".to_owned()),
        ("port", fixture.address.port().to_string()),
        ("d_u16_write_mode", mode.to_owned()),
        ("d_u16_write_allowlist", allowlist.to_owned()),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value))
    .collect();
    pairs.extend(
        extra
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned())),
    );
    let session = FrameSession::connect(
        driver.as_ref(),
        &Settings::from_pairs(pairs).unwrap(),
        &transport,
        &context,
        None,
    )
    .await;
    (driver, session)
}

fn bit_specs(driver: &WasmDriver, addresses: &[&str]) -> Vec<TagSpec> {
    let bit = TagKind::numeric(DataType::Bit);
    addresses
        .iter()
        .map(|address| TagSpec::new(driver.parse_address(address, &bit).unwrap(), bit.clone()))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #44 component"]
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
    assert_eq!(values[0], Ok(Value::Bit(false)));
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
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #44 component"]
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
    assert_eq!(sb_values[0], Ok(Value::Bit(true)));
    assert!(sb_values[1].is_err());
    let values = session.read_batch(&specs[2..]).await.unwrap();
    assert_eq!(values[0], Ok(Value::Bit(true)));
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
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #44 component"]
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
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #44 component"]
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

const PLAN_EXCLUDED: &str = "計画に含まれない";
const HUB_OUTSIDE: &str = "diagnostic proved tag outside the configured range";

fn is_unsupported(result: &Result<Value, ItemError>, reason: &str) -> bool {
    matches!(result, Err(ItemError::Unsupported(text)) if text == reason)
}

fn covered(session: &mut FrameSession, specs: &[TagSpec]) -> Vec<u32> {
    let plans = session.connection_mut().unwrap().plan_read(specs).unwrap();
    let mut covers: Vec<u32> = plans.iter().flat_map(|plan| plan.covers.clone()).collect();
    covers.sort_unstable();
    covers
}

/// Guest #43: with an exact same-connection SD count, a bit tag at or past it is
/// left out of the plan (the guest only leaves the tag out of `covers`). The Host
/// engine then reports its default per-item `Unsupported` for indices the guest
/// left out ("計画に含まれない"); no frame is sent and neighbours keep values.
/// Once the Hub also adopts the diagnostic, its own earlier gate answers with
/// `HUB_OUTSIDE` instead. Both are per-item `Err(Unsupported)`, never Good.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #44 component"]
async fn new_guest_outside_q_x_bit_is_left_out_of_the_plan_and_never_sent() {
    let (fixture, driver, mut session) = connect_new_guest(Peer::StandardQ).await;
    fixture.set_diagnostic_bit_counts(16, 0, 0);
    // X10 is hexadecimal 16: the first point past the count. XF is N-1.
    let specs = bit_specs(&driver, &["X1", "XF", "X10"]);
    // No count yet: the same tag is still read one-by-one explicitly.
    let before = fixture.read_spans().len();
    let values = session.read_batch(&specs[2..]).await.unwrap();
    assert_eq!(values, vec![Ok(Value::Bit(false))]);
    assert_eq!(fixture.read_spans()[before..], [(0x9c, 16, 1)]);

    let report = session.diagnose_batch(&specs).await.unwrap().unwrap();
    assert!(matches!(report.tags[1].verdict, TagVerdict::Inside(_)));
    assert!(matches!(report.tags[2].verdict, TagVerdict::Outside(_)));
    // Diagnostics are not accepted by the Hub here, so this is the guest's gate.
    let before = fixture.read_spans().len();
    let values = session.read_batch(&specs).await.unwrap();
    assert_eq!(values[0], Ok(Value::Bit(true)));
    assert_eq!(values[1], Ok(Value::Bit(false)));
    assert!(is_unsupported(&values[2], PLAN_EXCLUDED), "{values:?}");
    let spans = fixture.read_spans();
    assert!(!spans[before..].is_empty());
    assert!(spans[before..]
        .iter()
        .all(|(device, start, points)| { *device == 0x9c && start + u32::from(*points) <= 16 }));

    // After the Hub adopts the diagnostic its own gate answers first.
    session.accept_diagnostic(&specs, &report);
    let before = fixture.commands().len();
    let values = session.read_batch(&specs[2..]).await.unwrap();
    assert!(is_unsupported(&values[0], HUB_OUTSIDE), "{values:?}");
    assert_eq!(fixture.commands().len(), before);
    // plan_read bypasses the Hub gate: the guest alone leaves X10 uncovered.
    assert_eq!(covered(&mut session, &specs), vec![0, 1]);
    session.disconnect().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #44 component"]
async fn new_guest_outside_r_sb_and_f_bits_are_left_out_of_the_plan_and_never_sent() {
    let (fixture, driver, mut session) = connect_new_guest(Peer::R).await;
    fixture.set_diagnostic_bit_counts(0, 65538, 2);
    let specs = bit_specs(&driver, &["SB10001", "SB10002", "F1", "F2"]);
    // No count yet: SB10002 and F2 are read explicitly, one request per tag.
    let before = fixture.read_spans().len();
    let values = session
        .read_batch(&[specs[1].clone(), specs[3].clone()])
        .await
        .unwrap();
    assert!(values.iter().all(Result::is_ok), "{values:?}");
    assert_eq!(
        fixture.read_spans()[before..],
        [(0xa1, 65538, 1), (0x93, 2, 1)]
    );

    let report = session.diagnose_batch(&specs).await.unwrap().unwrap();
    assert!(matches!(report.tags[1].verdict, TagVerdict::Outside(_)));
    assert!(matches!(report.tags[3].verdict, TagVerdict::Outside(_)));
    let before = fixture.read_spans().len();
    let values = session.read_batch(&specs).await.unwrap();
    assert_eq!(values[0], Ok(Value::Bit(true)));
    assert!(is_unsupported(&values[1], PLAN_EXCLUDED), "{values:?}");
    assert_eq!(values[2], Ok(Value::Bit(true)));
    assert!(is_unsupported(&values[3], PLAN_EXCLUDED), "{values:?}");
    let spans = fixture.read_spans();
    assert!(spans[before..].iter().all(|(device, start, points)| {
        let end = start + u32::from(*points);
        (*device == 0xa1 && end <= 65538) || (*device == 0x93 && end <= 2)
    }));
    assert!(spans[before..].iter().any(|(device, _, _)| *device == 0xa1));
    assert!(spans[before..].iter().any(|(device, _, _)| *device == 0x93));
    session.disconnect().await;
}

/// The write handshake obtains exact M counts on the same connection, at
/// connect time and before any write. A D-only allowlist obtains none.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #44 component"]
async fn new_guest_write_handshake_count_leaves_outside_m_bit_out_of_the_plan() {
    for peer in [Peer::StandardQ, Peer::R] {
        // M63 is N-1 and M64 the first point past the exact count of 64.
        let (fixture, driver, mut session) =
            connect_new_guest_with(peer, "enabled", "M10-M11").await;
        let specs = bit_specs(&driver, &["M63", "M64"]);
        let values = session.read_batch(&specs).await.unwrap();
        assert!(values[0].is_ok(), "{values:?}");
        assert!(is_unsupported(&values[1], PLAN_EXCLUDED), "{values:?}");
        assert!(!fixture.read_spans().is_empty());
        assert!(fixture
            .read_spans()
            .iter()
            .all(|(device, start, points)| *device == 0x90 && start + u32::from(*points) <= 64));
        session.disconnect().await;

        let (fixture, _driver, mut session) =
            connect_new_guest_with(peer, "enabled", "D3010-D3013").await;
        let values = session.read_batch(&specs).await.unwrap();
        assert!(values.iter().all(Result::is_ok), "{values:?}");
        assert!(fixture.read_spans().contains(&(0x90, 64, 1)));
        session.disconnect().await;
    }
}

/// A refused SD or a saturated Q M/B field proves nothing, so the tag is still
/// read one-by-one as before #43 (and an `Unknown` verdict stays readable).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #44 component"]
async fn new_guest_refused_or_saturated_counts_keep_explicit_bit_reads() {
    // SD refused: Q X and R SB/F.
    let (fixture, driver, mut session) = connect_new_guest(Peer::StandardQ).await;
    fixture.set_bit_sd_refused(true);
    let specs = bit_specs(&driver, &["X10"]);
    let report = session.diagnose_batch(&specs).await.unwrap().unwrap();
    assert!(matches!(&report.tags[0].verdict, TagVerdict::Unknown(r) if r == "sd-refused"));
    let before = fixture.read_spans().len();
    assert_eq!(
        session.read_batch(&specs).await.unwrap(),
        vec![Ok(Value::Bit(false))]
    );
    assert_eq!(fixture.read_spans()[before..], [(0x9c, 16, 1)]);
    session.disconnect().await;

    let (fixture, driver, mut session) = connect_new_guest(Peer::R).await;
    fixture.set_bit_sd_refused(true);
    let specs = bit_specs(&driver, &["SB10002", "F2"]);
    let report = session.diagnose_batch(&specs).await.unwrap().unwrap();
    assert!(report
        .tags
        .iter()
        .all(|tag| matches!(&tag.verdict, TagVerdict::Unknown(r) if r == "sd-refused")));
    let before = fixture.read_spans().len();
    let values = session.read_batch(&specs).await.unwrap();
    assert!(values.iter().all(Result::is_ok), "{values:?}");
    assert_eq!(
        fixture.read_spans()[before..],
        [(0xa1, 65538, 1), (0x93, 2, 1)]
    );
    session.disconnect().await;

    // Q M/B saturated at 32768: only the leading area is guaranteed, so the tags
    // past it stay readable while 32767 (the last guaranteed point) is normal.
    let (fixture, driver, mut session) = connect_new_guest(Peer::StandardQ).await;
    fixture.set_q_mb_count(32768);
    let specs = bit_specs(&driver, &["M32767", "M40000", "B7FFF", "B9C40"]);
    let report = session.diagnose_batch(&specs).await.unwrap().unwrap();
    assert!(matches!(report.tags[0].verdict, TagVerdict::Inside(_)));
    for (index, expected) in [
        (1, "configured-range-unproven"),
        (3, "configured-range-unproven"),
    ] {
        assert!(
            matches!(&report.tags[index].verdict, TagVerdict::Unknown(r) if r == expected),
            "{:?}",
            report.tags[index].verdict
        );
    }
    assert!(matches!(report.tags[2].verdict, TagVerdict::Inside(_)));
    let before = fixture.read_spans().len();
    let values = session.read_batch(&specs).await.unwrap();
    assert!(values.iter().all(Result::is_ok), "{values:?}");
    let spans = fixture.read_spans();
    for expected in [(0x90, 40000, 1), (0xa0, 40000, 1)] {
        assert!(spans[before..].contains(&expected), "{expected:?}");
    }
    session.disconnect().await;
}

/// Guest #43 through the real Hub path: the write handshake of a saved
/// `write_mode=enabled` connection obtains the exact M count (64), so M64 is
/// left out of the plan. Collection stores it as `None`/Bad, read-now answers
/// 502 `read_failed` carrying the Host engine's default per-item `Unsupported`
/// text for a tag the guest left out of `covers`, the
/// neighbours M10/M63 stay Good, and no read frame ever covers M64.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #44 component"]
async fn new_guest_saved_outside_m_bit_is_bad_and_never_sent_through_the_hub() {
    let fixture = Fixture::start(Peer::StandardQ, 4000, false).await;
    let app = super::test_app_with_operational_write_manifest(
        "t12-new-guest-outside-m-hub",
        MANIFEST,
        &new_guest_component(),
    )
    .await;
    let saved = app
        .manager
        .plc_connections()
        .create_settings(super::ConnectionSettingsInput {
            name: "m-line".into(),
            protocol: "slmp-wasm-operational-write".into(),
            enabled: true,
            simulation: false,
            settings: serde_json::json!({
                "host": "127.0.0.1",
                "port": fixture.address.port(),
                "d_u16_write_mode": "enabled",
                "d_u16_write_allowlist": "M10-M11"
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
    .create(super::group_input("m-group", saved.id, 100))
    .await
    .unwrap();
    let mut ids = Vec::new();
    for (name, address) in [("m10", "M10"), ("m63", "M63"), ("m64", "M64")] {
        let tag = app
            .manager
            .tags()
            .create(super::tag_input(name, group.id, address, "bit"))
            .await
            .unwrap();
        ids.push(tag.id);
    }
    app.manager.rebuild().await.unwrap();
    let sample_of = |id: i64| {
        app.manager
            .current_values()
            .and_then(|values| values.get(&format!("tag:{id}")))
    };
    // M64 is Bad with no value; M10 and M63 are Good with a value.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if sample_of(ids[0]).is_some_and(|s| s.quality == Quality::Good && s.value.is_some())
                && sample_of(ids[1])
                    .is_some_and(|s| s.quality == Quality::Good && s.value.is_some())
                && sample_of(ids[2]).is_some_and(|s| s.quality == Quality::Bad)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("M10/M63 Good, M64 Bad");
    // Checks M64 stays Bad over several 100 ms collection cycles (the never-sent
    // check is the final `read_spans` assertion).
    // A few more ticks: M64 never turns Good or gains a value.
    tokio::time::sleep(Duration::from_millis(400)).await;
    let outside = sample_of(ids[2]).unwrap();
    assert_eq!(outside.quality, Quality::Bad);
    assert_eq!(outside.value, None);

    for name in ["m10", "m63"] {
        let (status, body) = super::admin_get(
            &app.router,
            &format!("/api/v1/values/m-line.m-group.{name}/read-now"),
            &app.admin_token,
        )
        .await;
        assert_eq!(status, super::StatusCode::OK, "{body:?}");
        assert_eq!(body["q"], "good", "{body:?}");
    }
    let (status, body) = super::admin_get(
        &app.router,
        "/api/v1/values/m-line.m-group.m64/read-now",
        &app.admin_token,
    )
    .await;
    assert_eq!(status, super::StatusCode::BAD_GATEWAY, "{body:?}");
    assert_eq!(body["error"], "read_failed", "{body:?}");
    assert_eq!(
        body["detail"],
        format!("扱えない組み合わせです: {PLAN_EXCLUDED}"),
        "{body:?}"
    );

    // The mock PLC never received a read covering M64 (device 0x90, point 64).
    let spans = fixture.read_spans();
    assert!(!spans.is_empty());
    assert!(
        spans
            .iter()
            .all(|(device, start, points)| *device == 0x90 && start + u32::from(*points) <= 64),
        "{spans:?}"
    );
    drop(app);
}

fn typed_specs(driver: &WasmDriver, tags: &[(&str, DataType)]) -> Vec<TagSpec> {
    tags.iter()
        .map(|(address, data_type)| {
            let kind = TagKind::numeric(*data_type);
            TagSpec::new(driver.parse_address(address, &kind).unwrap(), kind)
        })
        .collect()
}

/// Guest #51 (A2): `W1A.0` reads the one hexadecimal W word and extracts the
/// bit. The handshake count of the write-enabled connection (R 4000 words,
/// standard Q SD303 = 64) proves the word in or out of range, so a word-bit tag
/// at or past it is left out of the plan and no read frame covers it. The
/// fixture only answers W reads inside 0x1A..0x20, so a frame for the outside
/// word would also break the connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #58 component"]
async fn new_guest_w_word_bit_is_read_from_one_w_word_and_outside_is_never_sent() {
    for (peer, outside) in [(Peer::StandardQ, "W40.0"), (Peer::R, "WFA0.0")] {
        let (fixture, driver, mut session) =
            connect_new_guest_with(peer, "enabled", "W1A-W1F").await;
        // Fixture words read 7 (0b111): bit 0 and 2 are set, 3 and F are clear.
        let specs = bit_specs(&driver, &["W1A.0", "W1A.2", "W1A.3", "W1F.F", outside]);
        let before = fixture.read_spans().len();
        let values = session.read_batch(&specs).await.unwrap();
        assert_eq!(values[0], Ok(Value::Bit(true)), "{values:?}");
        assert_eq!(values[1], Ok(Value::Bit(true)));
        assert_eq!(values[2], Ok(Value::Bit(false)));
        assert_eq!(values[3], Ok(Value::Bit(false)));
        assert!(is_unsupported(&values[4], PLAN_EXCLUDED), "{values:?}");
        let spans = fixture.read_spans();
        assert!(!spans[before..].is_empty());
        assert!(spans[before..]
            .iter()
            .all(|(device, start, points)| *device == 0xb4 && *start + u32::from(*points) <= 0x20));
        // plan_read: the guest alone leaves the outside word uncovered.
        assert_eq!(covered(&mut session, &specs), vec![0, 1, 2, 3]);
        // A word interval (`W1A-W1F`) never authorizes a bit of the word
        // (guest #61 F): the Hub gate refuses it and nothing reaches the PLC.
        let results = session
            .write_batch(&[WriteItem::new(specs[0].clone(), Value::Bit(true))])
            .await
            .unwrap();
        assert!(results.iter().all(Result::is_err), "{results:?}");
        assert!(fixture.frames().is_empty() && fixture.bit_frames().is_empty());
        session.disconnect().await;
    }
}

/// Guest #55 (D1) and #56 (D2): S (bit), Z, RD and ZR are read-only devices
/// with range `Unknown`. The Hub therefore never gates them on a count: each tag
/// is one explicit single-point 0401 request, and no write ever reaches the PLC
/// even when the write policy is enabled for D. LZ needs 0403 and is refused at
/// the address; RD is R-only; Q25PRH has no ZR path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #58 component"]
async fn new_guest_s_z_rd_zr_are_read_only_unknown_range_reads() {
    type Case = (Peer, Vec<(&'static str, DataType)>, Vec<(u8, u32)>);
    let cases: [Case; 2] = [
        (
            Peer::R,
            vec![
                ("S10", DataType::Bit),
                ("Z3", DataType::U16),
                ("RD7", DataType::U16),
                ("ZR10", DataType::U16),
            ],
            vec![(0x98, 10), (0xcc, 3), (0x2c, 7), (0xb0, 0x10)],
        ),
        (
            Peer::StandardQ,
            vec![
                ("S10", DataType::Bit),
                ("Z3", DataType::U16),
                ("ZR10", DataType::U16),
            ],
            vec![(0x98, 10), (0xcc, 3), (0xb0, 0x10)],
        ),
    ];
    for (peer, tags, wire) in cases {
        let (fixture, driver, mut session) =
            connect_new_guest_with(peer, "enabled", "D3010-D3013").await;
        let specs = typed_specs(&driver, &tags);
        let before = fixture.read_spans().len();
        let values = session.read_batch(&specs).await.unwrap();
        // Fixture: S10 is set; Z, RD and ZR words read 7.
        assert_eq!(values[0], Ok(Value::Bit(true)), "{values:?}");
        for value in &values[1..] {
            assert_eq!(*value, Ok(Value::F64(7.0)), "{values:?}");
        }
        // One request per tag, one point each.
        let mut spans: Vec<_> = fixture.read_spans()[before..]
            .iter()
            .map(|(device, start, points)| {
                assert_eq!(*points, 1);
                (*device, *start)
            })
            .collect();
        spans.sort_unstable();
        let mut expected = wire.clone();
        expected.sort_unstable();
        assert_eq!(spans, expected);
        // Unknown range: nothing is left out of the plan.
        assert_eq!(
            covered(&mut session, &specs),
            (0..u32::try_from(specs.len()).unwrap()).collect::<Vec<_>>()
        );
        // Writes are refused before any frame is built.
        let writes = [
            Value::Bit(true),
            Value::F64(1.0),
            Value::F64(1.0),
            Value::F64(1.0),
        ];
        for (spec, value) in specs.iter().zip(writes) {
            let results = session
                .write_batch(&[WriteItem::new(spec.clone(), value)])
                .await
                .unwrap();
            assert!(results.iter().all(Result::is_err), "{results:?}");
        }
        assert!(fixture
            .commands()
            .iter()
            .all(|(command, _, _)| *command != 0x1401));
        assert!(
            fixture.frames().is_empty()
                && fixture.bit_frames().is_empty()
                && fixture.write_devices().is_empty()
        );
        session.disconnect().await;
    }

    // LZ is refused at the address on every model; RD only exists on R; Q25PRH
    // has no ZR path. None of them sends a read frame.
    let (fixture, driver, mut session) = connect_new_guest(Peer::R).await;
    let word = TagKind::numeric(DataType::U32);
    for address in ["LZ0", "LZ1"] {
        assert!(driver.parse_address(address, &word).is_err(), "{address}");
    }
    session.disconnect().await;
    assert!(fixture.read_spans().is_empty());

    let (fixture, driver, mut session) = connect_new_guest(Peer::StandardQ).await;
    let specs = typed_specs(&driver, &[("RD7", DataType::U16)]);
    let values = session.read_batch(&specs).await.unwrap();
    assert!(values[0].is_err(), "{values:?}");
    session.disconnect().await;
    assert!(fixture.read_spans().is_empty());

    let (fixture, driver, mut session) = connect_new_guest(Peer::Q).await;
    let specs = typed_specs(&driver, &[("ZR10", DataType::U16)]);
    let values = session.read_batch(&specs).await.unwrap();
    assert!(values[0].is_err(), "{values:?}");
    session.disconnect().await;
    assert!(fixture.read_spans().is_empty());
}

/// Runs one saved connection with one group through the real Hub path and
/// returns the tag ids in `tags` order. The tags are `(name, address, type)`.
async fn saved_new_guest_collection(
    label: &str,
    peer: Peer,
    allowlist: &str,
    tags: &[(&str, &str, &str)],
) -> (Fixture, super::TestApp, Vec<i64>) {
    saved_new_guest_collection_with(label, peer, Wire::default(), &[], allowlist, tags).await
}

/// `saved_new_guest_collection` with a wire behaviour and extra settings (E1/E2).
async fn saved_new_guest_collection_with(
    label: &str,
    peer: Peer,
    wire: Wire,
    extra: &[(&str, &str)],
    allowlist: &str,
    tags: &[(&str, &str, &str)],
) -> (Fixture, super::TestApp, Vec<i64>) {
    let fixture = Fixture::start_wire(peer, 4000, false, wire).await;
    let mut connection_settings = serde_json::json!({
        "host": "127.0.0.1",
        "port": fixture.address.port(),
        "d_u16_write_mode": "enabled",
        "d_u16_write_allowlist": allowlist
    });
    for (key, value) in extra {
        // The Hub schema types `network_no` and `station_no` as integers.
        connection_settings[*key] = match value.parse::<i64>() {
            Ok(number) => serde_json::json!(number),
            Err(_) => serde_json::json!(value),
        };
    }
    let app =
        super::test_app_with_operational_write_manifest(label, MANIFEST, &new_guest_component())
            .await;
    let saved = app
        .manager
        .plc_connections()
        .create_settings(super::ConnectionSettingsInput {
            name: "line".into(),
            protocol: "slmp-wasm-operational-write".into(),
            enabled: true,
            simulation: false,
            settings: connection_settings.as_object().unwrap().clone(),
            secrets: Default::default(),
        })
        .await
        .unwrap();
    let group = super::CollectionGroupService::with_registry(
        app.pool.clone(),
        app.manager.registry().clone(),
    )
    .create(super::group_input("group", saved.id, 100))
    .await
    .unwrap();
    let mut ids = Vec::new();
    for (name, address, data_type) in tags {
        let tag = app
            .manager
            .tags()
            .create(super::tag_input(name, group.id, address, data_type))
            .await
            .unwrap();
        ids.push(tag.id);
    }
    app.manager.rebuild().await.unwrap();
    (fixture, app, ids)
}

/// Guest #51 through the real Hub path: a saved `W1A.0` is collected Good and
/// answers read-now with the extracted bit, while a saved word-bit tag past the
/// W count (SD303 = 64, so W40) stays Bad, answers 502 `read_failed`, and no
/// read frame ever covers the outside word.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #58 component"]
async fn new_guest_saved_w_word_bit_is_good_inside_and_bad_outside_through_the_hub() {
    let (fixture, app, ids) = saved_new_guest_collection(
        "t12-new-guest-w-bit-hub",
        Peer::StandardQ,
        "W1A-W1F",
        &[
            ("w1a0", "W1A.0", "bit"),
            ("w1a3", "W1A.3", "bit"),
            ("w40", "W40.0", "bit"),
        ],
    )
    .await;
    let sample_of = |id: i64| {
        app.manager
            .current_values()
            .and_then(|values| values.get(&format!("tag:{id}")))
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if [ids[0], ids[1]].iter().all(|&id| {
                sample_of(id).is_some_and(|s| s.quality == Quality::Good && s.value.is_some())
            }) && sample_of(ids[2]).is_some_and(|s| s.quality == Quality::Bad)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("W1A.0/W1A.3 Good, W40.0 Bad");
    // W40.0 stays Bad over several 100 ms cycles (never-sent: `read_spans` below).
    tokio::time::sleep(Duration::from_millis(400)).await;
    let outside = sample_of(ids[2]).unwrap();
    assert_eq!(outside.quality, Quality::Bad);
    assert_eq!(outside.value, None);

    for (name, value) in [("w1a0", true), ("w1a3", false)] {
        let (status, body) = super::admin_get(
            &app.router,
            &format!("/api/v1/values/line.group.{name}/read-now"),
            &app.admin_token,
        )
        .await;
        assert_eq!(status, super::StatusCode::OK, "{body:?}");
        assert_eq!(body["v"], serde_json::json!(value), "{body:?}");
        assert_eq!(body["q"], "good", "{body:?}");
    }
    let (status, body) = super::admin_get(
        &app.router,
        "/api/v1/values/line.group.w40/read-now",
        &app.admin_token,
    )
    .await;
    assert_eq!(status, super::StatusCode::BAD_GATEWAY, "{body:?}");
    assert_eq!(body["error"], "read_failed", "{body:?}");
    assert_eq!(
        body["detail"],
        format!("扱えない組み合わせです: {PLAN_EXCLUDED}"),
        "{body:?}"
    );
    let spans = fixture.read_spans();
    assert!(!spans.is_empty());
    assert!(
        spans
            .iter()
            .all(|(device, start, points)| *device == 0xb4 && start + u32::from(*points) <= 0x20),
        "{spans:?}"
    );
    drop(app);
}

/// Guests #55 and #56 through the real Hub path: saved S, Z, RD and ZR tags on
/// an R CPU are collected Good (range `Unknown` never turns them Bad) and
/// answer read-now; every read is one single-point request.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #58 component"]
async fn new_guest_saved_s_z_rd_zr_are_collected_good_through_the_hub() {
    let (fixture, app, ids) = saved_new_guest_collection(
        "t12-new-guest-s-z-rd-zr-hub",
        Peer::R,
        "D3010-D3013",
        &[
            ("s10", "S10", "bit"),
            ("z3", "Z3", "u16"),
            ("rd7", "RD7", "u16"),
            ("zr10", "ZR10", "u16"),
        ],
    )
    .await;
    let sample_of = |id: i64| {
        app.manager
            .current_values()
            .and_then(|values| values.get(&format!("tag:{id}")))
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if ids.iter().all(|&id| {
                sample_of(id).is_some_and(|s| s.quality == Quality::Good && s.value.is_some())
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("S/Z/RD/ZR Good");
    for (name, value) in [
        ("s10", serde_json::json!(true)),
        ("z3", serde_json::json!(7.0)),
        ("rd7", serde_json::json!(7.0)),
        ("zr10", serde_json::json!(7.0)),
    ] {
        let (status, body) = super::admin_get(
            &app.router,
            &format!("/api/v1/values/line.group.{name}/read-now"),
            &app.admin_token,
        )
        .await;
        assert_eq!(status, super::StatusCode::OK, "{body:?}");
        assert_eq!(body["v"], value, "{body:?}");
        assert_eq!(body["q"], "good", "{body:?}");
    }
    let spans = fixture.read_spans();
    for device in [0x98, 0xcc, 0x2c, 0xb0] {
        assert!(
            spans.iter().any(|(seen, _, _)| *seen == device),
            "{device:#x} was never read: {spans:?}"
        );
    }
    assert!(spans.iter().all(|(_, _, points)| *points == 1), "{spans:?}");
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_OPERATIONAL_WRITE_COMPONENT_WASM to the reviewed D/W component"]
async fn malformed_allowlist_notation_is_rejected_and_sends_no_write_frame() {
    for peer in [Peer::R, Peer::StandardQ] {
        for bad in [
            "w1a-w1f",
            "D3010-D3013, W1A-W1F",
            "W1A - W1F",
            "W1G-W1F",
            "W1A-D27",
            "D3010-D3013,W1A-W1F,D3012-D3014",
            "W1A-W1F,D3010-D3013,W1C-W1D",
        ] {
            let driver = WasmDriver::load_operational_write(MANIFEST, &component()).unwrap();
            assert!(
                !driver
                    .validate_config(&settings(5000, "enabled", bad))
                    .is_empty(),
                "guest/host validation must reject {bad:?}"
            );
            let fixture = Fixture::start(peer, 4000, false).await;
            let transport = TransportSpec::Tcp {
                host: "127.0.0.1".into(),
                port: fixture.address.port(),
            };
            let context = ConnectContext::new(
                "slmp-wasm-operational-write",
                Duration::from_millis(500),
                Duration::from_millis(500),
            );
            let connected = FrameSession::connect(
                driver.as_ref(),
                &settings(fixture.address.port(), "enabled", bad),
                &transport,
                &context,
                None,
            )
            .await;
            assert!(
                matches!(connected, Err(FatalError::InvalidConfig(_))),
                "{bad:?} must fail the connection as invalid config"
            );
            assert!(fixture.frames().is_empty(), "{bad:?} sent a write frame");
            assert!(
                fixture
                    .commands()
                    .iter()
                    .all(|(command, _, _)| *command != 0x1401),
                "{bad:?} sent a write command"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_OPERATIONAL_WRITE_COMPONENT_WASM to the reviewed D/W component"]
async fn w_write_with_unknown_ack_or_plc_error_never_succeeds_or_replays() {
    for peer in [Peer::R, Peer::StandardQ] {
        let w_items = |driver: &WasmDriver| -> Vec<WriteItem> {
            w_specs(driver)
                .into_iter()
                .zip(1..=6)
                .map(|(spec, value)| WriteItem::new(spec, Value::F64(f64::from(value))))
                .collect()
        };
        let w_commands = |fixture: &Fixture| {
            fixture
                .commands()
                .iter()
                .filter(|(command, _, _)| *command == 0x1401)
                .count()
        };

        // Unknown outcome: the response does not match the request, so the
        // write is an error and must not be sent a second time.
        let (fixture, driver, mut session) = connect(peer, 4000, "enabled", "W1A-W1F", true).await;
        let result = session.write_batch(&w_items(&driver)).await;
        assert!(
            matches!(
                result,
                Err(FatalError::Framing(_))
                    | Err(FatalError::Protocol(_))
                    | Err(FatalError::Timeout)
            ),
            "{result:?}"
        );
        assert_eq!(fixture.frames(), vec![(0x1a, vec![1, 2, 3, 4, 5, 6])]);
        assert_eq!(fixture.write_devices(), vec![0xb4]);
        session.disconnect().await;
        assert_eq!(w_commands(&fixture), 1, "unknown outcome must not replay");

        // PLC device error (end code 0xC051): never reported as success, and
        // the single frame is not retried.
        let (fixture, driver, mut session) =
            connect(peer, 4000, "enabled", "D3010-D3013,W1A-W1F", false).await;
        fixture.set_write_end_code(0xc051);
        let result = session.write_batch(&w_items(&driver)).await;
        let items = result.expect("a PLC end code is reported per item, not as a fatal error");
        assert_eq!(items.len(), 6, "{items:?}");
        assert!(
            items
                .iter()
                .all(|item| matches!(item, Err(ItemError::Device(_)))),
            "PLC error must fail every item as a device error: {items:?}"
        );
        assert_eq!(fixture.frames(), vec![(0x1a, vec![1, 2, 3, 4, 5, 6])]);
        assert_eq!(fixture.write_devices(), vec![0xb4]);
        session.disconnect().await;
        assert_eq!(w_commands(&fixture), 1, "PLC error must not replay");
    }
}

// ---------------------------------------------------------------------------
// Guests #57 (E1: 3E binary frame) and #58 (E2: routed access), accepted
// against the loopback fixture above.
// ---------------------------------------------------------------------------

const OWN_ROUTE: [u8; 5] = [0x00, 0xff, 0xff, 0x03, 0x00];
/// network 2, station 5, request destination I/O 03E1H (cpu2), multidrop 0.
const ROUTE_2_5_CPU2: [u8; 5] = [0x02, 0x05, 0xe1, 0x03, 0x00];

fn word_spec(driver: &WasmDriver, address: &str) -> TagSpec {
    let kind = TagKind::numeric(DataType::U16);
    TagSpec::new(driver.parse_address(address, &kind).unwrap(), kind)
}

/// A read that came back is not Good: a fatal error or a per-item error.
fn is_not_good(result: &Result<Vec<Result<Value, ItemError>>, FatalError>) -> bool {
    match result {
        Err(_) => true,
        Ok(items) => items.iter().all(Result::is_err),
    }
}

/// Guest #57 through the real Hub path: with `frame=3e` the handshake (0101, SD)
/// passes, saved tags are collected Good and answer read-now, and every frame
/// the peer receives is a 3E (`5000`) request.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #58 component"]
async fn new_guest_frame_3e_handshake_collection_and_read_now_use_only_3e() {
    for peer in [Peer::R, Peer::Q, Peer::StandardQ] {
        let (fixture, app, ids) = saved_new_guest_collection_with(
            "t12-new-guest-frame-3e-hub",
            peer,
            Wire::default(),
            &[("frame", "3e")],
            "D3010-D3013",
            &[("d3010", "D3010", "u16"), ("d3011", "D3011", "u16")],
        )
        .await;
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if ids.iter().all(|id| {
                    app.manager
                        .current_values()
                        .and_then(|values| values.get(&format!("tag:{id}")))
                        .is_some_and(|s| s.quality == Quality::Good && s.value.is_some())
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("3E tags collected Good");
        for name in ["d3010", "d3011"] {
            let (status, body) = super::admin_get(
                &app.router,
                &format!("/api/v1/values/line.group.{name}/read-now"),
                &app.admin_token,
            )
            .await;
            assert_eq!(status, super::StatusCode::OK, "{body:?}");
            assert_eq!(body["v"], serde_json::json!(7.0), "{body:?}");
            assert_eq!(body["q"], "good", "{body:?}");
        }
        let commands = fixture.commands();
        assert!(commands.iter().any(|(command, _, _)| *command == 0x0101));
        assert!(commands.iter().any(|(command, _, _)| *command == 0x0401));
        let headers = fixture.headers();
        // Collection keeps running: a header is recorded before its command
        // and is snapshotted after `commands`, so it can only be ahead.
        assert!(
            headers.len() >= commands.len(),
            "{headers:02x?} {commands:?}"
        );
        assert!(
            headers
                .iter()
                .all(|(sub, route)| *sub == [0x50, 0x00] && *route == OWN_ROUTE),
            "{headers:02x?}"
        );
        drop(app);
    }
}

/// Guest #57: an allowed write on a `frame=3e` connection is one 3E frame, is
/// applied, and leaves an `ok` audit row (the same REST path as the 4E test).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #58 component"]
async fn new_guest_frame_3e_write_is_one_3e_frame_and_audited() {
    use banto_hub_core::api_keys::ApiKeysService;

    let fixture = Fixture::start(Peer::R, 4000, false).await;
    let app = super::test_app_with_operational_write_manifest(
        "t12-new-guest-frame-3e-write",
        MANIFEST,
        &new_guest_component(),
    )
    .await;
    let saved = app
        .manager
        .plc_connections()
        .create_settings(super::ConnectionSettingsInput {
            name: "line".into(),
            protocol: "slmp-wasm-operational-write".into(),
            enabled: true,
            simulation: false,
            settings: serde_json::json!({
                "host": "127.0.0.1",
                "port": fixture.address.port(),
                "frame": "3e",
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
    .create(super::group_input("group", saved.id, 60_000))
    .await
    .unwrap();
    let mut names = Vec::new();
    let mut ids = Vec::new();
    for index in [3010, 3011, 3012, 3013] {
        let name = format!("word{index}");
        let mut tag = super::tag_input(&name, group.id, &format!("D{index}"), "u16");
        tag.writable = true;
        ids.push(app.manager.tags().create(tag).await.unwrap().id);
        names.push(format!("line.group.{name}"));
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
    // One short-lived key limited to these four tags; revoked below.
    let issued = api_keys
        .issue(
            "frame-3e-loopback-writer",
            names.iter().map(|tag| format!("write:{tag}")).collect(),
            Some(now_ms + 600_000),
        )
        .await
        .unwrap();
    app.write_control.enable();
    let allowed = super::admin_write(
        &app.router,
        "POST",
        "/api/v1/values/batch",
        &issued.key,
        serde_json::json!({
            "writes": names
                .iter()
                .zip(1..=4)
                .map(|(tag, value)| serde_json::json!({"tag": tag, "v": value}))
                .collect::<Vec<_>>()
        }),
    )
    .await;
    let frames = fixture.frames();
    let headers = fixture.headers();
    let commands = fixture.commands();
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
    assert_eq!(
        commands
            .iter()
            .filter(|(command, _, _)| *command == 0x1401)
            .count(),
        1
    );
    assert!(
        headers
            .iter()
            .all(|(sub, route)| *sub == [0x50, 0x00] && *route == OWN_ROUTE),
        "{headers:02x?}"
    );
    assert_eq!(audit.0, super::StatusCode::OK, "{:?}", audit.1);
    let rows = audit.1["rows"].as_array().unwrap();
    for id in &ids {
        assert!(rows
            .iter()
            .any(|row| { row["tagId"].as_i64() == Some(*id) && row["result"] == "ok" }));
    }
}

/// Guest #57: a peer that answers a 3E request with a 4E header, or with a
/// length that does not match the bytes sent, never yields a Good value and
/// the connection is not usable afterwards. A write whose answer is bad is
/// not replayed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #58 component"]
async fn new_guest_frame_3e_bad_answer_is_never_good_and_cuts_the_connection() {
    let bad_wires = [
        (
            "4E header to a 3E request",
            Wire {
                answer_4e_to_3e: true,
                ..Wire::default()
            },
        ),
        (
            "length one too long",
            Wire {
                length_delta: 1,
                ..Wire::default()
            },
        ),
        (
            "length one too short",
            Wire {
                length_delta: -1,
                ..Wire::default()
            },
        ),
    ];
    for (label, bad) in bad_wires {
        let (fixture, driver, session) = connect_new_guest_cfg(
            Peer::R,
            Wire::default(),
            "enabled",
            "D3010-D3013",
            &[("frame", "3e")],
        )
        .await;
        let mut session = session.unwrap();
        let read_specs = [word_spec(&driver, "D3010"), word_spec(&driver, "D3011")];
        // Healthy first: the same session reads Good over 3E.
        assert_eq!(
            session.read_batch(&read_specs).await.unwrap(),
            vec![Ok(Value::F64(7.0)); 2],
            "{label}"
        );
        fixture.set_wire(bad);
        let result = session.read_batch(&read_specs).await;
        assert!(is_not_good(&result), "{label}: {result:?}");
        // The connection was cut: even a healthy peer no longer yields Good.
        fixture.set_wire(Wire::default());
        let result = session.read_batch(&read_specs).await;
        assert!(is_not_good(&result), "{label} (after): {result:?}");
        assert_eq!(fixture.connections(), 1, "{label}: no hidden reconnect");
        session.disconnect().await;

        // A write answered badly is reported as not successful and sent once.
        let (fixture, driver, session) = connect_new_guest_cfg(
            Peer::R,
            Wire::default(),
            "enabled",
            "D3010-D3013",
            &[("frame", "3e")],
        )
        .await;
        let mut session = session.unwrap();
        fixture.set_wire(bad);
        let write_specs: Vec<_> = (3010..=3013)
            .map(|index| word_spec(&driver, &format!("D{index}")))
            .collect();
        let result = session.write_batch(&items(&write_specs)).await;
        let succeeded = matches!(&result, Ok(items) if items.iter().any(Result::is_ok));
        assert!(!succeeded, "{label}: {result:?}");
        assert_eq!(
            fixture
                .commands()
                .iter()
                .filter(|(command, _, _)| *command == 0x1401)
                .count(),
            1,
            "{label}: a bad answer must not replay the write"
        );
        assert_eq!(fixture.frames().len(), 1, "{label}");
        session.disconnect().await;
    }
}

/// Guest #58 (with #57): `network_no=2`, `station_no=5`, `target_cpu=cpu2` puts
/// the route (02, 05, 03E1H) into every frame — 0101, SD, tag reads and the
/// write — in both 4E and 3E, and the handshake and reads pass.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #58 component"]
async fn new_guest_routed_connection_puts_the_route_in_every_frame() {
    for frame in ["4e", "3e"] {
        let (fixture, driver, session) = connect_new_guest_cfg(
            Peer::R,
            Wire::default(),
            "enabled",
            "D3010-D3013",
            &[
                ("frame", frame),
                ("network_no", "2"),
                ("station_no", "5"),
                ("target_cpu", "cpu2"),
            ],
        )
        .await;
        let mut session = session.unwrap();
        let read_specs = [word_spec(&driver, "D3010"), word_spec(&driver, "D3011")];
        assert_eq!(
            session.read_batch(&read_specs).await.unwrap(),
            vec![Ok(Value::F64(7.0)); 2],
            "{frame}"
        );
        let write_specs: Vec<_> = (3010..=3013)
            .map(|index| word_spec(&driver, &format!("D{index}")))
            .collect();
        assert_eq!(
            session.write_batch(&items(&write_specs)).await.unwrap(),
            vec![Ok(()); 4],
            "{frame}"
        );
        let commands = fixture.commands();
        for wanted in [0x0101, 0x0401, 0x1401] {
            assert!(
                commands.iter().any(|(command, _, _)| *command == wanted),
                "{frame}: {wanted:#x} {commands:?}"
            );
        }
        let headers = fixture.headers();
        assert_eq!(headers.len(), commands.len());
        let sub = if frame == "3e" {
            [0x50, 0x00]
        } else {
            [0x54, 0x00]
        };
        assert!(
            headers
                .iter()
                .all(|(got, route)| *got == sub && *route == ROUTE_2_5_CPU2),
            "{frame}: {headers:02x?}"
        );
        session.disconnect().await;
    }
}

/// Guest #58 through the real Hub path: a saved routed connection collects Good
/// and answers read-now, and its frames all carry the route.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #58 component"]
async fn new_guest_saved_routed_connection_is_collected_good_through_the_hub() {
    let (fixture, app, ids) = saved_new_guest_collection_with(
        "t12-new-guest-routed-hub",
        Peer::StandardQ,
        Wire::default(),
        &[
            ("network_no", "2"),
            ("station_no", "5"),
            ("target_cpu", "cpu2"),
        ],
        "D3010-D3013",
        &[("d3010", "D3010", "u16")],
    )
    .await;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if app
                .manager
                .current_values()
                .and_then(|values| values.get(&format!("tag:{}", ids[0])))
                .is_some_and(|s| s.quality == Quality::Good && s.value.is_some())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("routed tag collected Good");
    let (status, body) = super::admin_get(
        &app.router,
        "/api/v1/values/line.group.d3010/read-now",
        &app.admin_token,
    )
    .await;
    assert_eq!(status, super::StatusCode::OK, "{body:?}");
    assert_eq!(body["q"], "good", "{body:?}");
    let headers = fixture.headers();
    assert!(headers.len() >= 3, "{headers:02x?}");
    assert!(
        headers
            .iter()
            .all(|(sub, route)| *sub == [0x54, 0x00] && *route == ROUTE_2_5_CPU2),
        "{headers:02x?}"
    );
    drop(app);
}

/// Guest #58: a peer that folds the own-station route back instead of the
/// requested one is a frame error. The handshake fails, and a connected session
/// stops yielding Good values once the route is folded.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #58 component"]
async fn new_guest_routed_connection_rejects_a_folded_own_route() {
    let routed = [
        ("network_no", "2"),
        ("station_no", "5"),
        ("target_cpu", "cpu2"),
    ];
    for frame in ["4e", "3e"] {
        let mut extra = vec![("frame", frame)];
        extra.extend(routed);
        // Folded from the start: connect (handshake) fails.
        let (fixture, _driver, session) = connect_new_guest_cfg(
            Peer::R,
            Wire {
                fold_own_route: true,
                ..Wire::default()
            },
            "disabled",
            "",
            &extra,
        )
        .await;
        assert!(session.is_err(), "{frame}: handshake must fail");
        assert!(fixture.connections() >= 1);

        // Folded after the handshake: the read is not Good.
        let (fixture, driver, session) =
            connect_new_guest_cfg(Peer::R, Wire::default(), "disabled", "", &extra).await;
        let mut session = session.unwrap();
        let read_specs = [word_spec(&driver, "D3010")];
        assert_eq!(
            session.read_batch(&read_specs).await.unwrap(),
            vec![Ok(Value::F64(7.0))]
        );
        fixture.set_wire(Wire {
            fold_own_route: true,
            ..Wire::default()
        });
        let result = session.read_batch(&read_specs).await;
        assert!(is_not_good(&result), "{frame}: {result:?}");
        session.disconnect().await;
    }
}

/// Guests #57 and #58: the connection test API accepts a valid frame/route and
/// refuses invalid ones before any connection reaches the peer; the saved
/// connection is refused the same way; and `GET /api/drivers` lists the four
/// settings in the guest's schema.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #58 component"]
async fn new_guest_frame_and_route_settings_are_validated_before_connecting() {
    let fixture = Fixture::start(Peer::R, 4000, false).await;
    let app = super::test_app_with_operational_write_manifest(
        "t12-new-guest-frame-route-validation",
        MANIFEST,
        &new_guest_component(),
    )
    .await;
    let payload = |extra: serde_json::Value| {
        let mut settings = serde_json::json!({
            "host": "127.0.0.1",
            "port": fixture.address.port(),
            "d_u16_write_mode": "disabled",
            "d_u16_write_allowlist": ""
        });
        for (key, value) in extra.as_object().unwrap() {
            settings[key] = value.clone();
        }
        serde_json::json!({
            "protocol": "slmp-wasm-operational-write",
            "simulation": false,
            "settings": settings
        })
    };
    let test = |body: serde_json::Value| {
        super::admin_write(
            &app.router,
            "POST",
            "/api/plc-connections/test",
            &app.admin_token,
            body,
        )
    };

    for good in [
        serde_json::json!({}),
        serde_json::json!({"frame": "3e"}),
        serde_json::json!({"network_no": 2, "station_no": 5, "target_cpu": "cpu2"}),
        serde_json::json!({"frame": "3e", "network_no": 2, "station_no": 5}),
    ] {
        let (status, body) = test(payload(good.clone())).await;
        assert_eq!(status, super::StatusCode::OK, "{good}: {body:?}");
        assert_eq!(body["ok"], true, "{good}: {body:?}");
    }
    let connected = fixture.connections();
    assert!(connected >= 4);

    let bad_settings = [
        serde_json::json!({"network_no": 2}),
        serde_json::json!({"network_no": 0, "station_no": 5}),
        serde_json::json!({"network_no": 240, "station_no": 5}),
        serde_json::json!({"network_no": 2, "station_no": 121}),
        serde_json::json!({"network_no": 2, "station_no": 0}),
        serde_json::json!({"target_cpu": "cpu9"}),
        serde_json::json!({"frame": "2e"}),
    ];
    for bad in &bad_settings {
        let (status, body) = test(payload(bad.clone())).await;
        // Either a 4xx refusal or an `ok: false` answer, never a success.
        assert!(
            !status.is_success() || body["ok"] == false,
            "{bad}: {status} {body:?}"
        );
    }
    // Saving the same settings is refused too.
    for bad in &bad_settings {
        let settings = payload(bad.clone())["settings"]
            .as_object()
            .unwrap()
            .clone();
        let saved = app
            .manager
            .plc_connections()
            .create_settings(super::ConnectionSettingsInput {
                name: "bad".into(),
                protocol: "slmp-wasm-operational-write".into(),
                enabled: false,
                simulation: false,
                settings,
                secrets: Default::default(),
            })
            .await;
        assert!(saved.is_err(), "{bad}: saved");
    }
    assert_eq!(
        fixture.connections(),
        connected,
        "an invalid setting must not reach the peer"
    );

    let (status, body) = super::admin_write(
        &app.router,
        "GET",
        "/api/drivers",
        &app.admin_token,
        serde_json::Value::Null,
    )
    .await;
    assert_eq!(status, super::StatusCode::OK, "{body:?}");
    let entry = body
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item.to_string().contains("slmp-wasm-operational-write"))
        .unwrap_or_else(|| panic!("driver not listed: {body:?}"));
    let schema = entry.to_string();
    for key in ["frame", "network_no", "station_no", "target_cpu"] {
        assert!(
            schema.contains(&format!("\"key\":\"{key}\"")),
            "{key} missing from the schema: {schema}"
        );
    }
    drop(app);
}

// ---------------------------------------------------------------------------
// Guests #60 (D3: QnU extended W read) and #61 (F: D/W word-bit write by
// read-modify-write), accepted against the loopback fixture above.
// ---------------------------------------------------------------------------

/// The Hub gate's own refusal (`operational_write_item`), as opposed to the
/// guest's `Rejected`: nothing is handed to the guest.
const HUB_GATE: &str = "SLMP write is disabled or outside the connection allowlist";

fn is_hub_gate_refusal(result: &Result<(), ItemError>) -> bool {
    matches!(result, Err(ItemError::Unsupported(text)) if text == HUB_GATE)
}

fn word_bit_writes(driver: &WasmDriver, bits: &[(&str, bool)]) -> Vec<WriteItem> {
    let addresses: Vec<&str> = bits.iter().map(|(address, _)| *address).collect();
    bit_specs(driver, &addresses)
        .into_iter()
        .zip(bits)
        .map(|(spec, (_, value))| WriteItem::new(spec, Value::Bit(*value)))
        .collect()
}

/// Guest #61 (F): a write of distinct bits of one R D word and one R W word,
/// and of a standard Q W word (SD303), is exactly 0401 read, 1401 write-back
/// of `(current & !(set | clear)) | set` and 0401 confirm read. The fixture's
/// words read 7, so bits 1 and 2 must survive. Standard Q D has no write range
/// proof and is refused by the guest before anything is sent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #61 component"]
async fn new_guest_word_bit_write_is_read_modify_write_and_keeps_other_bits() {
    let allowlist = "D3010.0-D3010.F,W1A.0-W1A.F";
    for (peer, subcommand) in [(Peer::R, 2), (Peer::StandardQ, 0)] {
        let (fixture, driver, mut session) =
            connect_new_guest_with(peer, "enabled", allowlist).await;
        let mut cases = vec![(0xb4, 0x1a, vec![("W1A.0", false), ("W1A.F", true)], 0x8006)];
        if matches!(peer, Peer::R) {
            cases.insert(
                0,
                (
                    0xa8,
                    3010,
                    vec![("D3010.5", true), ("D3010.0", false)],
                    0x0026,
                ),
            );
        }
        for (device, word, bits, merged) in cases {
            let before = fixture.commands().len();
            let results = session
                .write_batch(&word_bit_writes(&driver, &bits))
                .await
                .unwrap();
            assert_eq!(results, vec![Ok(()); bits.len()], "{bits:?}");
            assert_eq!(
                fixture.commands()[before..],
                [
                    (0x0401, word, subcommand),
                    (0x1401, word, subcommand),
                    (0x0401, word, subcommand),
                ],
                "{bits:?}"
            );
            assert_eq!(fixture.frames().last(), Some(&(word, vec![merged])));
            assert_eq!(fixture.write_devices().last(), Some(&device));
        }
        if matches!(peer, Peer::StandardQ) {
            let before = fixture.commands().len();
            let results = session
                .write_batch(&word_bit_writes(&driver, &[("D3010.5", true)]))
                .await
                .unwrap();
            assert!(
                matches!(results.as_slice(), [Err(ItemError::Rejected(_))]),
                "standard Q D has no write range proof: {results:?}"
            );
            assert_eq!(fixture.commands().len(), before, "nothing is sent");
        }
        session.disconnect().await;
    }
}

/// Guest #61 (F): the Hub's own gate refuses, before the guest plans a frame,
/// a bit outside every bit interval, a bit covered only by a word interval, and
/// a request mixing a word bit with a word. A bit interval touching D3000/D3001
/// is refused by both the guest and the Hub when the settings are tested or
/// saved, while a valid bit interval passes the Hub's settings check.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #61 component"]
async fn new_guest_hub_gate_refuses_word_bits_outside_bit_intervals_before_send() {
    let (fixture, driver, mut session) =
        connect_new_guest_with(Peer::R, "enabled", "D3010-D3013,D3010.3-D3010.5").await;
    let before = fixture.commands().len();
    let outside = word_bit_writes(&driver, &[("D3010.6", true)]);
    let word_interval_only = word_bit_writes(&driver, &[("D3011.0", true)]);
    let mut mixed = word_bit_writes(&driver, &[("D3010.4", true)]);
    mixed.push(WriteItem::new(word_spec(&driver, "D3011"), Value::F64(1.0)));
    let mut mixed_word_first = vec![WriteItem::new(word_spec(&driver, "D3011"), Value::F64(1.0))];
    mixed_word_first.extend(word_bit_writes(&driver, &[("D3010.4", true)]));
    for items in [outside, word_interval_only, mixed, mixed_word_first] {
        let results = session.write_batch(&items).await.unwrap();
        assert_eq!(results.len(), items.len());
        assert!(results.iter().all(is_hub_gate_refusal), "{results:?}");
    }
    assert_eq!(fixture.commands().len(), before, "no frame for a refusal");
    assert!(fixture.frames().is_empty());
    session.disconnect().await;

    // A word interval alone: the bit is refused by the Hub as well.
    let (fixture, driver, mut session) =
        connect_new_guest_with(Peer::R, "enabled", "D3010-D3013").await;
    let before = fixture.commands().len();
    let results = session
        .write_batch(&word_bit_writes(&driver, &[("D3010.4", true)]))
        .await
        .unwrap();
    assert!(results.iter().all(is_hub_gate_refusal), "{results:?}");
    assert_eq!(fixture.commands().len(), before);
    session.disconnect().await;

    // Settings: the Hub accepts the guest's bit grammar and refuses D3000/D3001.
    let accepted = settings(5000, "enabled", "D3010.0-D3010.F,W1A.0-W1B.F");
    assert!(driver.validate_config(&accepted).is_empty());
    for bad in ["D2999.0-D3000.0", "D3001.F-D3001.F", "D3000.0-D3000.0"] {
        let errors = driver.validate_config(&settings(5000, "enabled", bad));
        // One error from the guest and one from the Hub's own parse.
        assert_eq!(
            errors
                .iter()
                .filter(|error| error.message == "D3000 and D3001 are protected")
                .count(),
            2,
            "{bad}: {errors:?}"
        );
    }
    let fixture = Fixture::start(Peer::R, 4000, false).await;
    let app = super::test_app_with_operational_write_manifest(
        "t12-new-guest-word-bit-settings",
        MANIFEST,
        &new_guest_component(),
    )
    .await;
    let settings_json = |allowlist: &str| {
        serde_json::json!({
            "host": "127.0.0.1",
            "port": fixture.address.port(),
            "d_u16_write_mode": "enabled",
            "d_u16_write_allowlist": allowlist
        })
    };
    let test = |allowlist: &str| {
        super::admin_write(
            &app.router,
            "POST",
            "/api/plc-connections/test",
            &app.admin_token,
            serde_json::json!({
                "protocol": "slmp-wasm-operational-write",
                "simulation": false,
                "settings": settings_json(allowlist)
            }),
        )
    };
    let (status, body) = test("D3010.0-D3010.F").await;
    assert_eq!(status, super::StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], true, "{body:?}");
    let connected = fixture.connections();
    for bad in ["D2999.0-D3000.0", "D3001.F-D3001.F"] {
        let (status, body) = test(bad).await;
        assert!(
            !status.is_success() || body["ok"] == false,
            "{bad}: {status} {body:?}"
        );
        let saved = app
            .manager
            .plc_connections()
            .create_settings(super::ConnectionSettingsInput {
                name: "bad".into(),
                protocol: "slmp-wasm-operational-write".into(),
                enabled: false,
                simulation: false,
                settings: settings_json(bad).as_object().unwrap().clone(),
                secrets: Default::default(),
            })
            .await;
        assert!(saved.is_err(), "{bad}: saved");
    }
    assert_eq!(fixture.connections(), connected, "never reaches the peer");
    assert!(fixture
        .commands()
        .iter()
        .all(|(command, _, _)| *command != 0x1401));
    drop(app);
}

/// Guest #61 (F): a confirm read that disagrees is `verification-failed` for
/// that item only, and an end code at the first read is `device` for every
/// item with no write-back sent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #61 component"]
async fn new_guest_word_bit_confirm_mismatch_and_read_refusal_reach_the_hub() {
    let (fixture, driver, mut session) =
        connect_new_guest_with(Peer::R, "enabled", "D3010.0-D3010.F").await;
    // Confirm reads 7 again: bit 5 (set) disagrees, bit 0 (set) agrees.
    fixture.set_wire(Wire {
        stale_word_reads: true,
        ..Wire::default()
    });
    let results = session
        .write_batch(&word_bit_writes(
            &driver,
            &[("D3010.5", true), ("D3010.0", true)],
        ))
        .await
        .unwrap();
    assert!(
        matches!(
            results.as_slice(),
            [Err(ItemError::VerificationFailed(_)), Ok(())]
        ),
        "{results:?}"
    );
    assert_eq!(fixture.frames(), vec![(3010, vec![0x0027])]);

    fixture.set_wire(Wire {
        word_read_end_code: 0xc051,
        ..Wire::default()
    });
    let before = fixture.commands().len();
    let results = session
        .write_batch(&word_bit_writes(
            &driver,
            &[("D3010.5", true), ("D3010.1", false)],
        ))
        .await
        .unwrap();
    assert!(
        results
            .iter()
            .all(|result| matches!(result, Err(ItemError::Device(_)))),
        "{results:?}"
    );
    assert_eq!(fixture.commands()[before..], [(0x0401, 3010, 2)]);
    assert_eq!(
        fixture.frames().len(),
        1,
        "no write-back after a refused read"
    );
    session.disconnect().await;
}

/// Guest #61 (F) through the real Hub path: saved writable word-bit tags are
/// written over REST by an API key. A batch of two bits of D3010 and a W1A bit
/// succeed and are audited `ok`; a confirm mismatch is audited `uncertain`; an
/// end code at the first read is audited `failed` with no write-back; a bit
/// outside the bit interval and a request mixing a word and a bit are refused
/// by the Hub gate, audited `failed` and send nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #61 component"]
async fn new_guest_rest_word_bit_writes_are_audited_by_outcome() {
    use banto_hub_core::api_keys::ApiKeysService;

    let fixture = Fixture::start(Peer::R, 4000, false).await;
    let app = super::test_app_with_operational_write_manifest(
        "t12-new-guest-word-bit-rest",
        MANIFEST,
        &new_guest_component(),
    )
    .await;
    let saved = app
        .manager
        .plc_connections()
        .create_settings(super::ConnectionSettingsInput {
            name: "line".into(),
            protocol: "slmp-wasm-operational-write".into(),
            enabled: true,
            simulation: false,
            settings: serde_json::json!({
                "host": "127.0.0.1",
                "port": fixture.address.port(),
                "d_u16_write_mode": "enabled",
                "d_u16_write_allowlist": "D3011-D3011,D3010.0-D3010.9,W1A.0-W1A.F"
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
    .create(super::group_input("group", saved.id, 60_000))
    .await
    .unwrap();
    let tags = [
        ("d3010_5", "D3010.5", "bit"),
        ("d3010_0", "D3010.0", "bit"),
        ("w1a_f", "W1A.F", "bit"),
        ("d3010_7", "D3010.7", "bit"),
        ("d3010_9", "D3010.9", "bit"),
        ("d3010_a", "D3010.A", "bit"),
        ("d3011", "D3011", "u16"),
        ("d3010_2", "D3010.2", "bit"),
    ];
    let mut names = Vec::new();
    let mut ids = Vec::new();
    for (name, address, data_type) in tags {
        let mut tag = super::tag_input(name, group.id, address, data_type);
        tag.writable = true;
        ids.push(app.manager.tags().create(tag).await.unwrap().id);
        names.push(format!("line.group.{name}"));
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
            "word-bit-loopback-writer",
            names.iter().map(|tag| format!("write:{tag}")).collect(),
            Some(now_ms + 600_000),
        )
        .await
        .unwrap();
    app.write_control.enable();
    let batch = |writes: Vec<(usize, serde_json::Value)>| {
        super::admin_write(
            &app.router,
            "POST",
            "/api/v1/values/batch",
            &issued.key,
            serde_json::json!({
                "writes": writes
                    .into_iter()
                    .map(|(at, value)| serde_json::json!({"tag": names[at], "v": value}))
                    .collect::<Vec<_>>()
            }),
        )
    };
    let write_commands = || {
        fixture
            .commands()
            .iter()
            .filter(|(command, _, _)| *command == 0x1401)
            .count()
    };
    let t = serde_json::json!(true);
    let f = serde_json::json!(false);
    let one_word = batch(vec![(0, t.clone()), (1, f.clone())]).await;
    let w_bit = batch(vec![(2, t.clone())]).await;
    let after_ok = (fixture.frames(), write_commands());
    fixture.set_wire(Wire {
        stale_word_reads: true,
        ..Wire::default()
    });
    let mismatch = batch(vec![(3, t.clone())]).await;
    fixture.set_wire(Wire {
        word_read_end_code: 0xc051,
        ..Wire::default()
    });
    let refused_read = batch(vec![(4, t.clone())]).await;
    let after_refused_read = write_commands();
    fixture.set_wire(Wire::default());
    let outside = batch(vec![(5, t.clone())]).await;
    let mixed = batch(vec![(6, serde_json::json!(1)), (7, t.clone())]).await;
    let after_gate = write_commands();
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
    for (label, (status, body), oks) in [
        ("one word", &one_word, 2),
        ("W bit", &w_bit, 1),
        ("mismatch", &mismatch, 0),
        ("refused read", &refused_read, 0),
        ("outside", &outside, 0),
        ("mixed", &mixed, 0),
    ] {
        assert_eq!(*status, super::StatusCode::OK, "{label}: {body:?}");
        let writes = body["writes"].as_array().unwrap();
        assert_eq!(
            writes.iter().filter(|result| result["ok"] == true).count(),
            oks,
            "{label}: {body:?}"
        );
        assert!(
            writes.iter().all(|result| result["ok"] == (oks > 0)),
            "{label}: {body:?}"
        );
    }
    // 7 = 0b0111: D3010 bit 5 set, bit 0 cleared; W1A bit F set.
    let (frames, commands_ok) = after_ok;
    assert_eq!(frames, vec![(3010, vec![0x0026]), (0x1a, vec![0x8007])]);
    assert_eq!(commands_ok, 2);
    assert_eq!(after_refused_read, 3, "only the mismatch wrote back");
    assert_eq!(after_gate, 3, "Hub gate refusals send nothing");
    assert_eq!(audit.0, super::StatusCode::OK, "{:?}", audit.1);
    let rows = audit.1["rows"].as_array().unwrap();
    for (at, result) in [
        (0, "ok"),
        (1, "ok"),
        (2, "ok"),
        (3, "uncertain"),
        (4, "failed"),
        (5, "failed"),
        (6, "failed"),
        (7, "failed"),
    ] {
        assert!(
            rows.iter()
                .any(|row| row["tagId"].as_i64() == Some(ids[at]) && row["result"] == result),
            "{}: expected {result}: {rows:?}",
            names[at]
        );
    }
}

/// Guest #60 (D3): a QnU CPU (Q02UCPU) whose SD310-311 total T exceeds the
/// internal W count WC (SD303 = 64) reads W tags in [WC, T) as extended W with
/// plain B4H reads. A tag at T is outside and never sent, and a tag crossing
/// WC is not read. Writes still use the internal W range only.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #61 component"]
async fn new_guest_qnu_extended_w_is_read_below_the_sd310_total_only() {
    let fixture = Fixture::start(Peer::StandardQ, 4000, false).await;
    fixture.set_q_w_total(Some(0x100));
    let (driver, session) =
        connect_new_guest_fixture(&fixture, "enabled", "W1A-W1F,W40-W41", &[]).await;
    let mut session = session.unwrap();
    assert!(fixture.commands().contains(&(0x0401, 310, 0)), "SD310 read");
    let specs = typed_specs(
        &driver,
        &[
            ("W1A", DataType::U16),
            ("W40", DataType::U16),
            ("WFF", DataType::U16),
            ("W100", DataType::U16),
            ("W3F", DataType::U32),
        ],
    );
    let before = fixture.read_spans().len();
    let values = session.read_batch(&specs).await.unwrap();
    assert_eq!(values[0], Ok(Value::F64(7.0)), "{values:?}");
    assert_eq!(values[1], Ok(Value::F64(7.0)), "{values:?}");
    assert_eq!(values[2], Ok(Value::F64(7.0)), "{values:?}");
    assert!(values[3].is_err() && values[4].is_err(), "{values:?}");
    let spans = fixture.read_spans();
    assert!(spans[before..].iter().all(|(device, start, points)| {
        let end = *start + u32::from(*points);
        *device == 0xb4 && ((*start >= 0x1a && end <= 0x20) || (*start >= 0x40 && end <= 0x100))
    }));
    // The extended W total never authorizes a write past the internal count.
    let before = fixture.commands().len();
    let results = session
        .write_batch(&[WriteItem::new(specs[1].clone(), Value::F64(1.0))])
        .await
        .unwrap();
    assert!(
        matches!(results.as_slice(), [Err(ItemError::Rejected(_))]),
        "{results:?}"
    );
    assert_eq!(fixture.commands().len(), before);
    // plan_read (last: it leaves the plans undecoded): the guest alone covers
    // the internal and extended tags only.
    assert_eq!(covered(&mut session, &specs), vec![0, 1, 2]);
    session.disconnect().await;
}

/// Loopback UDP SLMP peer for guest #62 (E4, #916): an R04CPU with SD280-283
/// = 4000/4000 and a D word memory, answering each datagram with exactly one
/// datagram in the request's frame (4E echoes the serial). Other SD reads are
/// refused (C051). It records `(subheader, command)` of every request.
struct UdpPeer {
    address: std::net::SocketAddr,
    seen: Arc<Mutex<Vec<(u8, u16)>>>,
    task: tokio::task::JoinHandle<()>,
}

impl UdpPeer {
    async fn start(words: &[(u32, u16)]) -> Self {
        let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = socket.local_addr().unwrap();
        let words: Arc<Mutex<std::collections::BTreeMap<u32, u16>>> =
            Arc::new(Mutex::new(words.iter().copied().collect()));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let task = {
            let (words, seen) = (words.clone(), seen.clone());
            tokio::spawn(async move {
                let mut buf = vec![0u8; 65536];
                loop {
                    let (len, from) = match socket.recv_from(&mut buf).await {
                        Ok(pair) => pair,
                        // Windows reports an ICMP for an earlier send here.
                        Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => continue,
                        Err(e) => panic!("UDP peer recv: {e}"),
                    };
                    let response = Self::answer(&buf[..len], &words, &seen);
                    let _ = socket.send_to(&response, from).await;
                }
            })
        };
        Self {
            address,
            seen,
            task,
        }
    }

    fn answer(
        datagram: &[u8],
        words: &Mutex<std::collections::BTreeMap<u32, u16>>,
        seen: &Mutex<Vec<(u8, u16)>>,
    ) -> Vec<u8> {
        let sub = datagram[0];
        assert!(
            matches!(&datagram[..2], [0x54, 0] | [0x50, 0]),
            "{datagram:02x?}"
        );
        // 3E is normalised to the 4E layout, as `Fixture` does.
        let request: Vec<u8> = if sub == 0x50 {
            [0x54, 0, 0, 0, 0, 0]
                .into_iter()
                .chain(datagram[2..].iter().copied())
                .collect()
        } else {
            datagram.to_vec()
        };
        let length = usize::from(u16::from_le_bytes([request[11], request[12]]));
        assert_eq!(request.len(), 13 + length, "one datagram is one request");
        let command = u16::from_le_bytes([request[15], request[16]]);
        seen.lock().unwrap().push((sub, command));
        let (end_code, body): (u16, Vec<u8>) = if command == 0x0101 {
            let mut body = vec![b' '; 18];
            body[..6].copy_from_slice(b"R04CPU");
            body[16..18].copy_from_slice(&0x4800u16.to_le_bytes());
            (0, body)
        } else {
            assert_eq!(u16::from_le_bytes([request[17], request[18]]), 2);
            let start = u32::from_le_bytes([request[19], request[20], request[21], request[22]]);
            let points = u16::from_le_bytes([request[25], request[26]]);
            match (command, request[23]) {
                (0x0401, 0xa9) if (start, points) == (280, 4) => {
                    let mut body = 4000u32.to_le_bytes().to_vec();
                    body.extend_from_slice(&4000u32.to_le_bytes());
                    (0, body)
                }
                (0x0401, 0xa9) => (0xc051, Vec::new()),
                (0x0401, 0xa8) => {
                    let words = words.lock().unwrap();
                    let body = (start..start + u32::from(points))
                        .flat_map(|index| words.get(&index).copied().unwrap_or(0).to_le_bytes())
                        .collect();
                    (0, body)
                }
                other => panic!("unexpected UDP request {command:#x} {other:x?}"),
            }
        };
        let mut response = if sub == 0x50 {
            vec![0xd0, 0]
        } else {
            vec![0xd4, 0, request[2], request[3], 0, 0]
        };
        response.extend_from_slice(&request[6..11]);
        response.extend_from_slice(&u16::try_from(body.len() + 2).unwrap().to_le_bytes());
        response.extend_from_slice(&end_code.to_le_bytes());
        response.extend_from_slice(&body);
        response
    }

    fn seen(&self) -> Vec<(u8, u16)> {
        self.seen.lock().unwrap().clone()
    }
}

impl Drop for UdpPeer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Guest #62 (E4) through the real Hub path (#916): a saved connection with
/// `transport=udp` passes the connection test, is collected Good and answers
/// read-now through the broker over loopback UDP, with the 4E and the 3E
/// frame; every request reaches the peer as one datagram in that frame. With
/// a manifest that lists only `tcp`, the same settings fail the connection
/// test and no datagram is sent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #63 component"]
async fn new_guest_saved_udp_connection_is_collected_and_read_now_over_udp() {
    for (frame, subheader) in [("4e", 0x54u8), ("3e", 0x50)] {
        let peer = UdpPeer::start(&[(3010, 4321), (3011, 0x8001)]).await;
        let settings = serde_json::json!({
            "host": "127.0.0.1",
            "port": peer.address.port(),
            "transport": "udp",
            "frame": frame,
            "d_u16_write_mode": "enabled",
            "d_u16_write_allowlist": "D3010-D3013"
        });
        let app = super::test_app_with_operational_write_manifest(
            &format!("t12-new-guest-udp-{frame}"),
            MANIFEST,
            &new_guest_component(),
        )
        .await;
        let (status, body) = super::admin_write(
            &app.router,
            "POST",
            "/api/plc-connections/test",
            &app.admin_token,
            serde_json::json!({
                "protocol": "slmp-wasm-operational-write",
                "simulation": false,
                "settings": settings
            }),
        )
        .await;
        assert_eq!(status, super::StatusCode::OK, "{frame}: {body:?}");
        assert_eq!(body["ok"], true, "{frame}: {body:?}");
        let saved = app
            .manager
            .plc_connections()
            .create_settings(super::ConnectionSettingsInput {
                name: "line".into(),
                protocol: "slmp-wasm-operational-write".into(),
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
        .create(super::group_input("group", saved.id, 100))
        .await
        .unwrap();
        let mut ids = Vec::new();
        for (name, address) in [("d3010", "D3010"), ("d3011", "D3011")] {
            let tag = super::tag_input(name, group.id, address, "u16");
            ids.push(app.manager.tags().create(tag).await.unwrap().id);
        }
        app.manager.rebuild().await.unwrap();
        let sample_of = |id: i64| {
            app.manager
                .current_values()
                .and_then(|values| values.get(&format!("tag:{id}")))
        };
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if ids.iter().all(|&id| {
                    sample_of(id).is_some_and(|s| s.quality == Quality::Good && s.value.is_some())
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("D3010/D3011 Good over UDP");
        for (name, value) in [("d3010", 4321.0), ("d3011", 32769.0)] {
            let (status, body) = super::admin_get(
                &app.router,
                &format!("/api/v1/values/line.group.{name}/read-now"),
                &app.admin_token,
            )
            .await;
            assert_eq!(status, super::StatusCode::OK, "{frame}: {body:?}");
            assert_eq!(body["v"], serde_json::json!(value), "{frame}: {body:?}");
            assert_eq!(body["q"], "good", "{frame}: {body:?}");
        }
        drop(app);
        let seen = peer.seen();
        assert!(seen.iter().any(|(_, command)| *command == 0x0101));
        assert!(
            seen.iter()
                .filter(|(_, command)| *command == 0x0401)
                .count()
                >= 3,
            "{frame}: {seen:x?}"
        );
        assert!(
            seen.iter().all(|(sub, _)| *sub == subheader),
            "{frame}: {seen:x?}"
        );
    }

    // The same settings with a manifest that lists only TCP.
    let peer = UdpPeer::start(&[]).await;
    let tcp_only = String::from_utf8(MANIFEST.to_vec()).unwrap().replace(
        r#""transports": ["tcp", "udp"]"#,
        r#""transports": ["tcp"]"#,
    );
    assert!(!tcp_only.contains("udp"));
    let app = super::test_app_with_operational_write_manifest(
        "t12-new-guest-udp-tcp-only",
        tcp_only.as_bytes(),
        &new_guest_component(),
    )
    .await;
    let (status, body) = super::admin_write(
        &app.router,
        "POST",
        "/api/plc-connections/test",
        &app.admin_token,
        serde_json::json!({
            "protocol": "slmp-wasm-operational-write",
            "simulation": false,
            "settings": {
                "host": "127.0.0.1",
                "port": peer.address.port(),
                "transport": "udp",
                "d_u16_write_mode": "disabled",
                "d_u16_write_allowlist": ""
            }
        }),
    )
    .await;
    assert!(
        !status.is_success() || body["ok"] == false,
        "{status} {body:?}"
    );
    assert!(
        body.to_string().contains("UDP 通信に対応していません"),
        "{status} {body:?}"
    );
    drop(app);
    assert!(peer.seen().is_empty(), "no datagram: {:?}", peer.seen());
}

/// Guest #63 (C1) through the real Hub path (#914): saved D and W string tags
/// written over REST by an API key pass the Hub gate and the guest allowlist,
/// reach the PLC as one 1401 over the whole span with the encoded, 0x00-padded
/// text low byte first per word, are audited `ok`, and read back over
/// read-now. A span reaching past the allowlist and text longer than the span
/// are refused before send and audited `failed`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #63 component"]
async fn new_guest_saved_d_and_w_strings_are_written_audited_and_read_back_over_rest() {
    use banto_hub_core::api_keys::ApiKeysService;

    let fixture = Fixture::start(Peer::R, 4000, false).await;
    let app = super::test_app_with_operational_write_manifest(
        "t12-new-guest-string-rest",
        MANIFEST,
        &new_guest_component(),
    )
    .await;
    let saved = app
        .manager
        .plc_connections()
        .create_settings(super::ConnectionSettingsInput {
            name: "line".into(),
            protocol: "slmp-wasm-operational-write".into(),
            enabled: true,
            simulation: false,
            settings: serde_json::json!({
                "host": "127.0.0.1",
                "port": fixture.address.port(),
                "d_u16_write_mode": "enabled",
                "d_u16_write_allowlist": "D3010-D3013,W1A-W1F"
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
    .create(super::group_input("group", saved.id, 60_000))
    .await
    .unwrap();
    let mut names = Vec::new();
    let mut ids = Vec::new();
    for (name, address, words) in [
        ("d3010s", "D3010", 4),
        ("w1as", "W1A", 2),
        ("d3011s", "D3011", 4),
    ] {
        let mut tag = super::tag_input(name, group.id, address, "string");
        tag.string_length = Some(words);
        tag.writable = true;
        ids.push(app.manager.tags().create(tag).await.unwrap().id);
        names.push(format!("line.group.{name}"));
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
            "string-loopback-writer",
            names.iter().map(|tag| format!("write:{tag}")).collect(),
            Some(now_ms + 600_000),
        )
        .await
        .unwrap();
    app.write_control.enable();
    let write = |at: usize, value: &str| {
        super::admin_write(
            &app.router,
            "POST",
            "/api/v1/values/batch",
            &issued.key,
            serde_json::json!({ "writes": [{ "tag": names[at], "v": value }] }),
        )
    };
    let read_now = |at: usize| {
        let path = format!("/api/v1/values/{}/read-now", names[at]);
        let (router, token) = (app.router.clone(), app.admin_token.clone());
        async move { super::admin_get(&router, &path, &token).await }
    };
    let write_commands = || {
        fixture
            .commands()
            .iter()
            .filter(|(command, _, _)| *command == 0x1401)
            .count()
    };

    let d_written = write(0, "ABC").await;
    let d_frames = (fixture.frames(), fixture.write_devices());
    let d_read = read_now(0).await;
    let w_written = write(1, "WXYZ").await;
    let w_frames = (fixture.frames(), fixture.write_devices());
    let w_read = read_now(1).await;
    let outside = write(2, "AB").await;
    let too_long = write(0, "ABCDEFGHI").await;
    let after_refusals = write_commands();
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
    for (label, (status, body), ok) in [
        ("D string", &d_written, true),
        ("W string", &w_written, true),
        ("outside", &outside, false),
        ("too long", &too_long, false),
    ] {
        assert_eq!(*status, super::StatusCode::OK, "{label}: {body:?}");
        assert_eq!(body["writes"][0]["ok"], ok, "{label}: {body:?}");
    }
    // One 1401 each, low byte first per word, 0x00-padded to the whole span.
    assert_eq!(
        d_frames,
        (
            vec![(3010, vec![0x4241, 0x0043, 0x0000, 0x0000])],
            vec![0xa8]
        )
    );
    assert_eq!(
        w_frames,
        (
            vec![
                (3010, vec![0x4241, 0x0043, 0x0000, 0x0000]),
                (0x1a, vec![0x5857, 0x5a59])
            ],
            vec![0xa8, 0xb4]
        )
    );
    assert_eq!(after_refusals, 2, "refusals send nothing");
    for (label, (status, body), value) in [("D", &d_read, "ABC"), ("W", &w_read, "WXYZ")] {
        assert_eq!(*status, super::StatusCode::OK, "{label}: {body:?}");
        assert_eq!(body["v"], value, "{label}: {body:?}");
        assert_eq!(body["q"], "good", "{label}: {body:?}");
    }
    assert!(fixture.read_spans().contains(&(0xa8, 3010, 4)));
    assert!(fixture.read_spans().contains(&(0xb4, 0x1a, 2)));
    assert_eq!(audit.0, super::StatusCode::OK, "{:?}", audit.1);
    let rows = audit.1["rows"].as_array().unwrap();
    let results_of = |at: usize| -> Vec<&str> {
        let mut rows: Vec<_> = rows
            .iter()
            .filter(|row| row["tagId"].as_i64() == Some(ids[at]))
            .collect();
        rows.sort_by_key(|row| row["id"].as_i64());
        rows.iter()
            .map(|row| row["result"].as_str().unwrap())
            .collect()
    };
    assert_eq!(results_of(0), ["ok", "failed"], "{rows:?}");
    assert_eq!(results_of(1), ["ok"], "{rows:?}");
    assert_eq!(results_of(2), ["failed"], "{rows:?}");
}

/// Loopback TCP SLMP peer for guest #65 (G1, #917): an R04CPU or Q02UCPU with
/// a D word memory and a buffer memory keyed by module `h` and address. It
/// answers the handshake, plain D reads and device-extension 0401 (Q 0080, R
/// 0082), and records every extension request from the reserved bytes on, the
/// count of 1401 writes, and the connections made.
struct UnitBufferPeer {
    address: std::net::SocketAddr,
    ext: Arc<Mutex<Vec<Vec<u8>>>>,
    writes: Arc<AtomicU32>,
    task: tokio::task::JoinHandle<()>,
}

impl UnitBufferPeer {
    async fn start(r: bool, buffer: &[(u16, u32, u16)], d: &[(u32, u16)]) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let buffer: Arc<std::collections::BTreeMap<(u16, u32), u16>> =
            Arc::new(buffer.iter().map(|&(h, n, w)| ((h, n), w)).collect());
        let d: Arc<std::collections::BTreeMap<u32, u16>> = Arc::new(d.iter().copied().collect());
        let ext = Arc::new(Mutex::new(Vec::new()));
        let writes = Arc::new(AtomicU32::new(0));
        let task = {
            let (ext, writes) = (ext.clone(), writes.clone());
            tokio::spawn(async move {
                loop {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    loop {
                        let mut head = [0u8; 13];
                        if socket.read_exact(&mut head).await.is_err() {
                            break;
                        }
                        assert_eq!(&head[..2], &[0x54, 0x00], "4E request");
                        let length = usize::from(u16::from_le_bytes([head[11], head[12]]));
                        let mut request = head.to_vec();
                        request.resize(13 + length, 0);
                        if socket.read_exact(&mut request[13..]).await.is_err() {
                            break;
                        }
                        let command = u16::from_le_bytes([request[15], request[16]]);
                        let sub = u16::from_le_bytes([request[17], request[18]]);
                        let (end_code, body): (u16, Vec<u8>) = match (command, sub) {
                            (0x0101, _) => {
                                let (name, code) = if r {
                                    ("R04CPU", 0x4800u16)
                                } else {
                                    ("Q02UCPU", 0x0263)
                                };
                                let mut body = vec![b' '; 18];
                                body[..name.len()].copy_from_slice(name.as_bytes());
                                body[16..18].copy_from_slice(&code.to_le_bytes());
                                (0, body)
                            }
                            (0x1401, _) => {
                                writes.fetch_add(1, Ordering::SeqCst);
                                (0, Vec::new())
                            }
                            (0x0401, 0x0080 | 0x0082) => {
                                ext.lock().unwrap().push(request[4..].to_vec());
                                let (h_at, count_at, number) = if sub == 0x0080 {
                                    (
                                        27,
                                        30,
                                        u32::from_le_bytes([
                                            request[21],
                                            request[22],
                                            request[23],
                                            0,
                                        ]),
                                    )
                                } else {
                                    (
                                        29,
                                        32,
                                        u32::from_le_bytes([
                                            request[21],
                                            request[22],
                                            request[23],
                                            request[24],
                                        ]),
                                    )
                                };
                                let h = u16::from_le_bytes([request[h_at], request[h_at + 1]]);
                                let count =
                                    u16::from_le_bytes([request[count_at], request[count_at + 1]]);
                                let body = (0..u32::from(count))
                                    .flat_map(|at| {
                                        buffer
                                            .get(&(h, number + at))
                                            .copied()
                                            .unwrap_or(0)
                                            .to_le_bytes()
                                    })
                                    .collect();
                                (0, body)
                            }
                            (0x0401, _) => {
                                let (start, device, points) = if sub == 0 {
                                    (
                                        u32::from_le_bytes([
                                            request[19],
                                            request[20],
                                            request[21],
                                            0,
                                        ]),
                                        request[22],
                                        u16::from_le_bytes([request[23], request[24]]),
                                    )
                                } else {
                                    (
                                        u32::from_le_bytes([
                                            request[19],
                                            request[20],
                                            request[21],
                                            request[22],
                                        ]),
                                        request[23],
                                        u16::from_le_bytes([request[25], request[26]]),
                                    )
                                };
                                match device {
                                    0xa9 if r && (start, points) == (280, 4) => {
                                        let mut body = 4000u32.to_le_bytes().to_vec();
                                        body.extend_from_slice(&4000u32.to_le_bytes());
                                        (0, body)
                                    }
                                    0xa9 if !r && start == 302 && matches!(points, 1 | 2) => {
                                        let mut body = 4000u16.to_le_bytes().to_vec();
                                        if points == 2 {
                                            body.extend_from_slice(&64u16.to_le_bytes());
                                        }
                                        (0, body)
                                    }
                                    0xa9 => (0xc051, Vec::new()),
                                    0xa8 => (
                                        0,
                                        (0..u32::from(points))
                                            .flat_map(|at| {
                                                d.get(&(start + at))
                                                    .copied()
                                                    .unwrap_or(0)
                                                    .to_le_bytes()
                                            })
                                            .collect(),
                                    ),
                                    other => panic!("unexpected device {other:#x}"),
                                }
                            }
                            other => panic!("unexpected command {other:x?}"),
                        };
                        let mut response = vec![0xd4, 0, request[2], request[3], 0, 0];
                        response.extend_from_slice(&request[6..11]);
                        response.extend_from_slice(
                            &u16::try_from(body.len() + 2).unwrap().to_le_bytes(),
                        );
                        response.extend_from_slice(&end_code.to_le_bytes());
                        response.extend_from_slice(&body);
                        if socket.write_all(&response).await.is_err() {
                            break;
                        }
                    }
                }
            })
        };
        Self {
            address,
            ext,
            writes,
            task,
        }
    }

    fn ext(&self) -> Vec<Vec<u8>> {
        self.ext.lock().unwrap().clone()
    }
}

impl Drop for UnitBufferPeer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The expected extension request from the reserved bytes on (see the layout
/// in `crates/banto-driver-wasm/tests/slmp_unit_buffer.rs`).
fn unit_buffer_frame(r: bool, h: u16, number: u32, count: u16) -> Vec<u8> {
    let mut data = vec![0x01, 0x04];
    if r {
        data.extend_from_slice(&[0x82, 0x00, 0x00, 0x00]);
        data.extend_from_slice(&number.to_le_bytes());
        data.extend_from_slice(&[0xab, 0x00, 0x00, 0x00]);
    } else {
        data.extend_from_slice(&[0x80, 0x00, 0x00, 0x00]);
        data.extend_from_slice(&number.to_le_bytes()[..3]);
        data.extend_from_slice(&[0xab, 0x00, 0x00]);
    }
    data.extend_from_slice(&h.to_le_bytes());
    data.push(if (0x3e0..=0x3e3).contains(&h) {
        0xfa
    } else {
        0xf8
    });
    data.extend_from_slice(&count.to_le_bytes());
    let mut frame = vec![0x00, 0x00, 0x00, 0xff, 0xff, 0x03, 0x00];
    frame.extend_from_slice(&u16::try_from(data.len() + 2).unwrap().to_le_bytes());
    frame.extend_from_slice(&[0x10, 0x00]);
    frame.extend_from_slice(&data);
    frame
}

/// `(name, address, dataType, stringLength, expected read-now value, Good on this model)`.
type UnitBufferTag<'a> = (
    &'a str,
    &'a str,
    &'a str,
    Option<i64>,
    serde_json::Value,
    bool,
);

/// Guest #65 (G1) through the real Hub path (#917), R and Q: saved U\G
/// numeric, word-bit and string tags pass registration over REST (the string
/// on U\G is accepted, a writable U\G tag is refused), are collected Good by
/// the periodic collection (a string tag only by read-now, as for D/W
/// strings) and answer read-now, each tag as its own exact
/// request (never merged with `U3\G11` next to `U3\G10`, nor with the D tag).
/// `U3\G10` and `U5\G10` register with different `area`s (`U3\G`, `U5\G`), so
/// the registration screen does not treat them as overlapping, and read their
/// own module. On Q the CPU buffer `U3E0` and a span past 0xFFFFFF stay Bad
/// and are never sent. A write to a U\G tag with an API key sends nothing.
async fn unit_buffer_tags_register_collect_and_read_now(r: bool) {
    use banto_hub_core::api_keys::ApiKeysService;

    let model = if r { "r" } else { "q" };
    let peer = UnitBufferPeer::start(
        r,
        &[
            (3, 10, 0x1111),
            (3, 11, 0x3333),
            (5, 10, 0x2222),
            (3, 12, 0x5678),
            (3, 13, 0x0001),
            (3, 20, 0x0020),
            (3, 30, u16::from_le_bytes(*b"HE")),
            (3, 31, u16::from_le_bytes(*b"LL")),
            (3, 32, u16::from_le_bytes(*b"O!")),
            (0x3e0, 1, 0xbeef),
            (0xff, 0xff_ffff, 0x0abc),
        ],
        &[(100, 77)],
    )
    .await;
    let app = super::test_app_with_operational_write_manifest(
        &format!("t12-new-guest-unit-buffer-{model}"),
        MANIFEST,
        &new_guest_component(),
    )
    .await;
    let saved = app
        .manager
        .plc_connections()
        .create_settings(super::ConnectionSettingsInput {
            name: "line".into(),
            protocol: "slmp-wasm-operational-write".into(),
            enabled: true,
            simulation: false,
            settings: serde_json::json!({
                "host": "127.0.0.1",
                "port": peer.address.port(),
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
    .create(super::group_input("group", saved.id, 100))
    .await
    .unwrap();

    let mut tags: Vec<UnitBufferTag> = vec![
        (
            "u3g10",
            "U3\\G10",
            "u16",
            None,
            serde_json::json!(4369.0),
            true,
        ),
        (
            "u3g11",
            "U3\\G11",
            "u16",
            None,
            serde_json::json!(13107.0),
            true,
        ),
        (
            "u5g10",
            "U5\\G10",
            "u16",
            None,
            serde_json::json!(8738.0),
            true,
        ),
        (
            "u3g12",
            "U3\\G12",
            "u32",
            None,
            serde_json::json!(87_672.0),
            true,
        ),
        (
            "u3g20b5",
            "U3\\G20.5",
            "bit",
            None,
            serde_json::json!(true),
            true,
        ),
        (
            "u3g30s",
            "U3\\G30",
            "string",
            Some(3),
            serde_json::json!("HELLO!"),
            true,
        ),
        ("d100", "D100", "u16", None, serde_json::json!(77.0), true),
        (
            "cpu",
            "U3E0\\G1",
            "u16",
            None,
            serde_json::json!(48879.0),
            r,
        ),
    ];
    if !r {
        tags.push((
            "over",
            "UFF\\G16777215",
            "u32",
            None,
            serde_json::Value::Null,
            false,
        ));
    }
    let payload = |name: &str, address: &str, data_type: &str, length: Option<i64>, writable| {
        let mut body = serde_json::json!({
            "name": name, "collectionGroupId": group.id, "address": address,
            "dataType": data_type, "enabled": true, "writable": writable
        });
        if let Some(length) = length {
            body["stringLength"] = serde_json::json!(length);
        }
        body
    };
    // Registration over REST. A writable U\G tag is refused: read-only area.
    let (status, body) = super::admin_write(
        &app.router,
        "POST",
        "/api/tags",
        &app.admin_token,
        payload("w", "U3\\G10", "u16", None, true),
    )
    .await;
    assert_eq!(status, super::StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    assert!(body.to_string().contains("読み取り専用"), "{body:?}");
    let mut ids = Vec::new();
    for (name, address, data_type, length, ..) in &tags {
        let (status, body) = super::admin_write(
            &app.router,
            "POST",
            "/api/tags",
            &app.admin_token,
            payload(name, address, data_type, *length, false),
        )
        .await;
        assert_eq!(status, super::StatusCode::OK, "{address}: {body:?}");
        assert_eq!(body["address"], *address, "{body:?}");
        ids.push(body["id"].as_i64().unwrap());
    }
    // The address facts the registration screen's overlap check uses.
    let (status, parsed) = super::admin_write(
        &app.router,
        "POST",
        "/api/tags/parse-addresses",
        &app.admin_token,
        serde_json::json!({
            "connectionId": saved.id,
            "items": [
                {"tagKind": "plc", "address": "U3\\G10", "dataType": "u16"},
                {"tagKind": "plc", "address": "U5\\G10", "dataType": "u16"},
                {"tagKind": "plc", "address": "U3\\G20.5", "dataType": "bit"},
                {"tagKind": "plc", "address": "U03\\G010", "dataType": "u32"},
                {"tagKind": "plc", "address": "U3\\G30", "dataType": "string", "stringLength": 3},
                {"tagKind": "plc", "address": "U100\\G1", "dataType": "u16"},
            ]
        }),
    )
    .await;
    assert_eq!(status, super::StatusCode::OK, "{parsed:?}");
    let items = parsed["items"].as_array().unwrap();
    for (at, canonical, area, index, bit, words) in [
        (0, "U3\\G10", "U3\\G", 10, None, 1),
        (1, "U5\\G10", "U5\\G", 10, None, 1),
        (2, "U3\\G20.5", "U3\\G", 20, Some(5), 1),
        (3, "U3\\G10", "U3\\G", 10, None, 2),
        (4, "U3\\G30", "U3\\G", 30, None, 3),
    ] {
        let item = &items[at];
        assert_eq!(item["ok"], true, "{item:?}");
        assert_eq!(item["canonical"], canonical, "{item:?}");
        assert_eq!(item["area"], area, "{item:?}");
        assert_eq!(item["index"], index, "{item:?}");
        assert_eq!(item["writableArea"], false, "{item:?}");
        assert_eq!(item["wordCount"], words, "{item:?}");
        assert_eq!(
            item["bit"],
            bit.map_or(serde_json::Value::Null, |bit| serde_json::json!(bit)),
            "{item:?}"
        );
    }
    assert_ne!(
        items[0]["area"], items[1]["area"],
        "U3\\G10 and U5\\G10 differ"
    );
    assert_eq!(items[0]["index"], items[1]["index"]);
    assert_eq!(
        items[5]["ok"], false,
        "U100 is outside the grammar: {:?}",
        items[5]
    );

    app.manager.rebuild().await.unwrap();
    let mut status = app.sessions.status_watch(saved.id).unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        status.wait_for(|state| *state == banto_broker::BrokerConnectionStatus::Connected),
    )
    .await
    .unwrap()
    .unwrap();
    let sample_of = |id: i64| {
        app.manager
            .current_values()
            .and_then(|values| values.get(&format!("tag:{id}")))
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            // The periodic collection skips string tags (they are read by
            // read-now only, as for D/W strings), so they are not awaited.
            if tags
                .iter()
                .zip(&ids)
                .all(|((_, _, data_type, _, _, good), &id)| {
                    !good
                        || *data_type == "string"
                        || sample_of(id).is_some_and(|s| s.quality == Quality::Good)
                })
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        let states: Vec<_> = tags
            .iter()
            .zip(&ids)
            .map(|((name, ..), &id)| (*name, sample_of(id).map(|s| s.quality)))
            .collect();
        panic!(
            "every readable U\\G tag is collected Good: {states:?} {:x?}",
            peer.ext()
        )
    });
    // Let a few more cycles run so a merged or stray request would show.
    tokio::time::sleep(Duration::from_millis(400)).await;
    for ((name, address, _, _, value, good), &id) in tags.iter().zip(&ids) {
        let (status, body) = super::admin_get(
            &app.router,
            &format!("/api/v1/values/line.group.{name}/read-now"),
            &app.admin_token,
        )
        .await;
        if *good {
            assert_eq!(status, super::StatusCode::OK, "{address}: {body:?}");
            assert_eq!(body["v"], *value, "{address}: {body:?}");
            assert_eq!(body["q"], "good", "{address}: {body:?}");
        } else {
            assert!(
                !status.is_success(),
                "{address} read-now: {status} {body:?}"
            );
            assert_ne!(
                sample_of(id).map(|s| s.quality),
                Some(Quality::Good),
                "{address}"
            );
        }
    }

    // One exact request per U\G tag, nothing else on the extension path.
    let mut expected = vec![
        unit_buffer_frame(r, 3, 10, 1),
        unit_buffer_frame(r, 3, 11, 1),
        unit_buffer_frame(r, 5, 10, 1),
        unit_buffer_frame(r, 3, 12, 2),
        unit_buffer_frame(r, 3, 20, 1),
        unit_buffer_frame(r, 3, 30, 3),
    ];
    if r {
        expected.push(unit_buffer_frame(true, 0x3e0, 1, 1));
    }
    expected.sort();
    let mut seen = peer.ext();
    seen.sort();
    seen.dedup();
    assert_eq!(seen, expected, "{model}: each tag is its own request");

    // A write to a U\G tag never reaches the PLC, whatever the key allows.
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
            "unit-buffer-writer",
            vec![
                "write:line.group.u3g10".into(),
                "write:line.group.u3g30s".into(),
            ],
            Some(now_ms + 600_000),
        )
        .await
        .unwrap();
    app.write_control.enable();
    for (tag, value) in [
        ("line.group.u3g10", serde_json::json!(1.0)),
        ("line.group.u3g30s", serde_json::json!("AB")),
    ] {
        let (status, body) = super::admin_write(
            &app.router,
            "POST",
            "/api/v1/values/batch",
            &issued.key,
            serde_json::json!({ "writes": [{ "tag": tag, "v": value }] }),
        )
        .await;
        assert!(
            !status.is_success() || body["writes"][0]["ok"] == false,
            "{tag}: {status} {body:?}"
        );
    }
    let revoked = api_keys.revoke(issued.id).await;
    app.write_control.disable();
    assert!(revoked.unwrap().revoked_at.is_some());
    assert_eq!(app.sessions.connection_count(), 1);
    drop(app);
    assert_eq!(peer.writes.load(Ordering::SeqCst), 0, "no 1401 was sent");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #65 component"]
async fn new_guest_saved_unit_buffer_tags_are_collected_and_read_now_on_r() {
    unit_buffer_tags_register_collect_and_read_now(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #65 component"]
async fn new_guest_saved_unit_buffer_tags_are_collected_and_read_now_on_q() {
    unit_buffer_tags_register_collect_and_read_now(false).await;
}
