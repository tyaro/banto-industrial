//! Component-level checks of the SLMP guest's G2a model identification
//! (banto-driver-wasm #69) through this crate's adapter and write gate (Hub
//! side acceptance, mock only).
//!
//! Run with `BANTO_SLMP_NEW_COMPONENT_WASM=<built guest> cargo test -- --ignored`.
//! The guest now recognizes, by the exact 0101 (name, code) pair: the Q process
//! CPU QnPH (standard non-U Q), the universal process CPU QnUDPV (like QnUDV),
//! MELSEC-L (like standard Q) and Q12PRHCPU/004BH (like Q25PRHCPU/004CH). The
//! previous pin `1fac51a` classified every one of these pairs as Unknown, so
//! the four model tests below fail there (checked 2026-11-06): its handshake
//! ends without a range, the Q reads are not planned, and the Q12PRH write is
//! refused. `mismatched_name_and_code_pairs_stay_unknown` passes on both pins;
//! it guards that only the exact pairs are recognized. The tests drive the
//! real engine (`FrameSession` over TCP) against a loopback SLMP peer on a std
//! thread that records every request byte for byte. The peer is a mock Q-wire
//! PLC whose 0101 answer is chosen per test; no PLC is involved.
//!
//! Findings about the guest that the tests pin down (they differ from what one
//! might assume from the stage description):
//! - The component's handshake never reads SD282-285 (the QnUDV fast D/W pair)
//!   for any model; only the driver library plans it. "QnUDPV is treated like
//!   QnUDV" is therefore proved here by identical request sequences.
//! - An Unknown model (a pair not in the table) is not a per-item Unsupported:
//!   the handshake ends after 0101 and the whole read batch fails with a
//!   protocol error ("configured range is unresolved"). Per-item Unsupported is
//!   what a *known* model without the path gets (Q25PRH/Q12PRH for ZR and U\G).
//! - The D3010-D3013 test-write opt-in (`q25prh_d3010_3013_test_write`) belongs
//!   to the controlled-fixture loader; the operational loader refuses it
//!   together with an enabled write mode. The shared Q25PRH/Q12PRH write path
//!   is exercised here through the operational allowlist instead.

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
const MANIFEST: &[u8] = br#"{"manifestVersion":1,"id":"slmp-wasm-operational-write","version":"0.1.0","hubApi":"1","writeApi":"1","settingsVersion":1,"displayName":"SLMP G2a fixture","transports":["tcp","udp"],"capabilities":["read","write"]}"#;

fn component() -> Vec<u8> {
    std::fs::read(std::env::var("BANTO_SLMP_NEW_COMPONENT_WASM").expect("guest path")).unwrap()
}

const D: u8 = 0xa8;
const SD: u8 = 0xa9;
const W: u8 = 0xb4;
const ZR: u8 = 0xb0;
const END_CODE_C051: u16 = 0xc051;
/// SD302 (internal D) and SD303 (internal W) the peer answers.
const D_COUNT: u16 = 4000;
const W_COUNT: u16 = 64;
/// SD310-311 (internal + extended W total) the peer answers when asked.
const W_TOTAL: u32 = 8000;

/// What the peer saw, from the reserved bytes on (the serial is left out).
#[derive(Default)]
struct Seen {
    all: Vec<Vec<u8>>,
    /// (start, points) of each SD read.
    sd: Vec<(u32, u16)>,
    /// Plain 0401 reads of non-SD devices: (device, start, points).
    plain: Vec<(u8, u32, u16)>,
    /// 0401 device-extension (`U\G`) reads.
    ext: Vec<Vec<u8>>,
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

impl Peer {
    /// A Q-wire (4E, subcommand 0000/0080) PLC answering 0101 with
    /// `name`/`code`. Words not in `mem` (device, number) read as 0.
    fn start(name: &'static str, code: u16, mem: &[(u8, u32, u16)]) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let mem: BTreeMap<(u8, u32), u16> = mem.iter().map(|&(d, n, w)| ((d, n), w)).collect();
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
                        let (end_code, body) = answer(name, code, &request, &mem, &seen);
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

    fn sd(&self) -> Vec<(u32, u16)> {
        self.seen.lock().unwrap().sd.clone()
    }

    fn plain(&self) -> Vec<(u8, u32, u16)> {
        self.seen.lock().unwrap().plain.clone()
    }

    fn ext(&self) -> Vec<Vec<u8>> {
        self.seen.lock().unwrap().ext.clone()
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
        seen.ext.clear();
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

/// Answer one 4E request: 0101, the SD handshake reads, plain word reads,
/// device-extension reads and writes (counted and acknowledged).
fn answer(
    name: &str,
    code: u16,
    request: &[u8],
    mem: &BTreeMap<(u8, u32), u16>,
    seen: &Mutex<Seen>,
) -> (u16, Vec<u8>) {
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
        0x1401 => {
            seen.lock().unwrap().writes.push(request[4..].to_vec());
            (0, Vec::new())
        }
        0x0401 if sub == 0x0080 => {
            seen.lock().unwrap().ext.push(request[4..].to_vec());
            (0, words_body([0x0abc].into_iter()))
        }
        0x0401 => {
            assert_eq!(sub, 0, "Q word access");
            let start = u32::from_le_bytes([request[19], request[20], request[21], 0]);
            let device = request[22];
            let points = u16::from_le_bytes([request[23], request[24]]);
            if device == SD {
                seen.lock().unwrap().sd.push((start, points));
                return match (start, points) {
                    (302, 1 | 2) => {
                        let mut body = D_COUNT.to_le_bytes().to_vec();
                        if points == 2 {
                            body.extend_from_slice(&W_COUNT.to_le_bytes());
                        }
                        (0, body)
                    }
                    (310, 2) => (0, W_TOTAL.to_le_bytes().to_vec()),
                    _ => (END_CODE_C051, Vec::new()),
                };
            }
            assert!(
                matches!(device, D | W | ZR),
                "unexpected device {device:#x}"
            );
            seen.lock().unwrap().plain.push((device, start, points));
            (
                0,
                words_body(
                    (0..u32::from(points))
                        .map(|at| mem.get(&(device, start + at)).copied().unwrap_or(0)),
                ),
            )
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

fn with_length(data: &[u8]) -> Vec<u8> {
    let mut frame = vec![0x00, 0x00, 0x00, 0xff, 0xff, 0x03, 0x00];
    // The data length counts the 2-byte timer and the command data.
    frame.extend_from_slice(&u16::try_from(data.len() + 2).unwrap().to_le_bytes());
    frame.extend_from_slice(&[0x10, 0x00]);
    frame.extend_from_slice(data);
    frame
}

/// A plain Q word read (0401, subcommand 0000): 3-byte number, device code,
/// points.
fn plain_frame(device: u8, start: u32, points: u16) -> Vec<u8> {
    let mut data = vec![0x01, 0x04, 0x00, 0x00];
    data.extend_from_slice(&start.to_le_bytes()[..3]);
    data.push(device);
    data.extend_from_slice(&points.to_le_bytes());
    with_length(&data)
}

/// A Q buffer-memory read (0401, subcommand 0080) of module `h`.
fn ug_frame(h: u16, number: u32, count: u16) -> Vec<u8> {
    let mut data = vec![0x01, 0x04, 0x80, 0x00, 0x00, 0x00];
    data.extend_from_slice(&number.to_le_bytes()[..3]);
    data.extend_from_slice(&[0xab, 0x00, 0x00]);
    data.extend_from_slice(&h.to_le_bytes());
    data.push(0xf8);
    data.extend_from_slice(&count.to_le_bytes());
    with_length(&data)
}

/// A Q 1401 write of D words starting at `start`.
fn d_write_frame(start: u32, words: &[u16]) -> Vec<u8> {
    let mut data = vec![0x01, 0x14, 0x00, 0x00];
    data.extend_from_slice(&start.to_le_bytes()[..3]);
    data.push(D);
    data.extend_from_slice(&u16::try_from(words.len()).unwrap().to_le_bytes());
    data.extend(words.iter().copied().flat_map(u16::to_le_bytes));
    with_length(&data)
}

/// The handshake of a standard Q: 0101, then SD302-303.
fn q_handshake() -> Vec<Vec<u8>> {
    vec![type_name_frame(), plain_frame(SD, 302, 2)]
}

fn numeric(driver: &WasmDriver, address: &str, data_type: DataType) -> TagSpec {
    let kind = TagKind::numeric(data_type);
    TagSpec::new(driver.parse_address(address, &kind).unwrap(), kind)
}

fn driver() -> Arc<WasmDriver> {
    WasmDriver::load_operational_write(MANIFEST, &component()).unwrap()
}

async fn connect(driver: &Arc<WasmDriver>, peer: &Peer) -> FrameSession {
    let port = peer.address.port().to_string();
    let settings = Settings::from_pairs([
        ("host", "127.0.0.1"),
        ("port", port.as_str()),
        ("d_u16_write_mode", "enabled"),
        ("d_u16_write_allowlist", "D3010-D3013"),
    ])
    .unwrap();
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

fn memory() -> Vec<(u8, u32, u16)> {
    vec![(D, 100, 7), (ZR, 10, 0x2222), (W, 0x100, 0x3333)]
}

/// Connects as `name`/`code`, reads the common tag set once and returns the
/// peer (with the handshake and the batch in its record) and the results.
async fn run_reads(
    name: &'static str,
    code: u16,
    tags: &[(&str, DataType)],
) -> (Peer, Vec<Result<Value, ItemError>>) {
    let driver = driver();
    let peer = Peer::start(name, code, &memory());
    let mut session = connect(&driver, &peer).await;
    let specs: Vec<TagSpec> = tags
        .iter()
        .map(|&(address, data_type)| numeric(&driver, address, data_type))
        .collect();
    let values = session.read_batch(&specs).await.unwrap();
    assert_eq!(values.len(), specs.len());
    assert_eq!(peer.connections(), 1, "{name}: connection kept");
    session.disconnect().await;
    (peer, values)
}

const COMMON_TAGS: &[(&str, DataType)] = &[
    ("D100", DataType::U16),
    ("ZR10", DataType::U16),
    ("UFF\\G7", DataType::U16),
    ("U3E0\\G7", DataType::U16),
];

/// QnPH is a standard non-U Q: the handshake is 0101 and SD302-303 only (no
/// SD310-311), D and ZR are read as plain Q words, `U0\G` uses the Q 0080
/// extension, the CPU buffer U3E0 is a per-item Unsupported never sent, and
/// an extended W (past the SD303 count) is not read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #69 component"]
async fn qnph_is_a_standard_non_u_q() {
    let mut tags = COMMON_TAGS.to_vec();
    tags.push(("W100", DataType::U16));
    for (name, code) in [
        ("Q02PHCPU", 0x0041),
        ("Q06PHCPU", 0x0042),
        ("Q12PHCPU", 0x0043),
        ("Q25PHCPU", 0x0044),
    ] {
        let (peer, values) = run_reads(name, code, &tags).await;
        assert_eq!(values[0], Ok(Value::F64(7.0)), "{name}");
        assert_eq!(values[1], Ok(Value::F64(f64::from(0x2222u16))), "{name}");
        assert_eq!(values[2], Ok(Value::F64(f64::from(0x0abcu16))), "{name}");
        assert!(is_unsupported(&values[3]), "{name}: {:?}", values[3]);
        assert!(
            values[4].is_err(),
            "{name}: extended W is not QnU: {values:?}"
        );
        let mut seen = peer.all();
        let batch = sorted(seen.split_off(2));
        assert_eq!(seen, q_handshake(), "{name}");
        assert_eq!(
            batch,
            sorted(vec![
                plain_frame(D, 100, 1),
                plain_frame(ZR, 10, 1),
                ug_frame(0xff, 7, 1),
            ]),
            "{name}: exactly D, ZR and UFF\\G are sent"
        );
        assert!(
            peer.sd().iter().all(|&(start, _)| start == 302),
            "{name}: no SD310-311 read: {:?}",
            peer.sd()
        );
        assert_eq!(peer.ext(), vec![ug_frame(0xff, 7, 1)], "{name}");
        assert!(
            peer.plain().iter().all(|&(device, ..)| device != W),
            "{name}: W100 is never sent: {:?}",
            peer.plain()
        );
    }
}

/// QnUDPV is treated exactly like QnUDV: the same request sequence (handshake
/// with SD302-303 and SD310-311, then the reads) for the same workload, with
/// the extended W (W100, past SD303 but below the SD310-311 total) read. QnH
/// would not read it (see `qnph_is_a_standard_non_u_q`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #69 component"]
async fn qnudpv_discovery_and_reads_equal_qnudv() {
    let mut tags = COMMON_TAGS.to_vec();
    tags.push(("W100", DataType::U16));
    for ((pv, pv_code), (v, v_code)) in [
        (("Q04UDPVCPU", 0x0367), ("Q04UDVCPU", 0x0367)),
        (("Q06UDPVCPU", 0x0368), ("Q06UDVCPU", 0x0368)),
        (("Q13UDPVCPU", 0x036a), ("Q13UDVCPU", 0x036a)),
        (("Q26UDPVCPU", 0x036c), ("Q26UDVCPU", 0x036c)),
    ] {
        let (pv_peer, pv_values) = run_reads(pv, pv_code, &tags).await;
        let (v_peer, v_values) = run_reads(v, v_code, &tags).await;
        assert_eq!(pv_values, v_values, "{pv} vs {v}");
        assert_eq!(pv_values[0], Ok(Value::F64(7.0)), "{pv}");
        assert_eq!(
            pv_values[1],
            Ok(Value::F64(f64::from(0x2222u16))),
            "{pv}: ZR"
        );
        assert_eq!(
            pv_values[2],
            Ok(Value::F64(f64::from(0x0abcu16))),
            "{pv}: UFF\\G"
        );
        assert!(is_unsupported(&pv_values[3]), "{pv}: U3E0");
        assert_eq!(
            pv_values[4],
            Ok(Value::F64(f64::from(0x3333u16))),
            "{pv}: QnU extended W (SD310-311) is read"
        );
        assert_eq!(pv_peer.all(), v_peer.all(), "{pv} vs {v}: same requests");
        assert_eq!(
            pv_peer.all()[..3].to_vec(),
            vec![
                type_name_frame(),
                plain_frame(SD, 302, 2),
                plain_frame(SD, 310, 2)
            ],
            "{pv}: SD302-303 then SD310-311"
        );
        assert_eq!(pv_peer.sd(), vec![(302, 2), (310, 2)], "{pv}");
        assert!(
            pv_peer.plain().contains(&(W, 0x100, 1)),
            "{pv}: {:?}",
            pv_peer.plain()
        );
    }
}

/// MELSEC-L is treated exactly like a standard non-U Q: its request sequence
/// equals Q06HCPU's for the same workload, ZR is read, `UFF\G` is sent with
/// 0080, `U3E0\G` is a per-item Unsupported never sent, and SD310-311 is
/// never read (the codes are not QnU). A D write inside the allowlist is
/// refused by the guest like on any standard Q (no SD proof of the D span, so
/// `Rejected`) and sends nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #69 component"]
async fn melsec_l_is_treated_like_a_standard_q() {
    let mut tags = COMMON_TAGS.to_vec();
    tags.push(("W100", DataType::U16));
    let (q_peer, q_values) = run_reads("Q06HCPU", 0x0042, &tags).await;
    assert_eq!(q_values[1], Ok(Value::F64(f64::from(0x2222u16))));
    for (name, code) in [
        ("L02SCPU", 0x0543),
        ("L02SCPU-P", 0x0543),
        ("L02CPU", 0x0541),
        ("L02CPU-P", 0x0541),
        ("L06CPU", 0x0544),
        ("L06CPU-P", 0x0544),
        ("L26CPU", 0x0545),
        ("L26CPU-P", 0x0545),
        ("L26CPU-BT", 0x0542),
        ("L26CPU-PBT", 0x0542),
    ] {
        let (peer, values) = run_reads(name, code, &tags).await;
        assert_eq!(values, q_values, "{name}");
        assert_eq!(values[0], Ok(Value::F64(7.0)), "{name}");
        assert_eq!(
            values[1],
            Ok(Value::F64(f64::from(0x2222u16))),
            "{name}: ZR"
        );
        assert_eq!(values[2], Ok(Value::F64(f64::from(0x0abcu16))), "{name}");
        assert!(is_unsupported(&values[3]), "{name}: U3E0");
        assert_eq!(peer.all(), q_peer.all(), "{name}: same requests as Q06H");
        assert_eq!(peer.sd(), vec![(302, 2)], "{name}: no SD310-311");
        assert_eq!(peer.ext(), vec![ug_frame(0xff, 7, 1)], "{name}");
        assert!(peer.plain().contains(&(ZR, 10, 1)), "{name}");
    }

    // The write gate: no proof of the D span on a standard Q, so refused.
    for (name, code) in [
        ("Q06HCPU", 0x0042),
        ("L02CPU", 0x0541),
        ("L26CPU-PBT", 0x0542),
    ] {
        let driver = driver();
        let peer = Peer::start(name, code, &memory());
        let mut session = connect(&driver, &peer).await;
        let results = session
            .write_batch(&[WriteItem::new(
                numeric(&driver, "D3010", DataType::U16),
                Value::F64(1.0),
            )])
            .await
            .unwrap();
        // Inside the allowlist, so the Hub gate passes; the guest refuses.
        assert!(
            matches!(results.as_slice(), [Err(ItemError::Rejected(_))]),
            "{name}: {results:?}"
        );
        assert!(peer.writes().is_empty(), "{name}");
        session.disconnect().await;
    }
}

/// Q12PRHCPU/004BH is the same family as Q25PRHCPU/004CH: Q wire, SD302-303
/// internal D/W only (no SD310-311), D read, ZR and U\G per-item Unsupported
/// and never sent, and an allowlisted D write inside the SD302-proven range
/// is the identical 1401 frame. The test-write opt-in is not reachable through
/// the operational loader (see the module comment).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #69 component"]
async fn q12prh_matches_q25prh_reads_and_writes() {
    let tags = [
        ("D100", DataType::U16),
        ("ZR10", DataType::U16),
        ("U3\\G1", DataType::U16),
        ("D101", DataType::U16),
    ];
    let mut recorded = Vec::new();
    for (name, code) in [("Q25PRHCPU", 0x004c), ("Q12PRHCPU", 0x004b)] {
        let driver = driver();
        let peer = Peer::start(name, code, &memory());
        let mut session = connect(&driver, &peer).await;
        let specs: Vec<TagSpec> = tags
            .iter()
            .map(|&(address, data_type)| numeric(&driver, address, data_type))
            .collect();
        let values = session.read_batch(&specs).await.unwrap();
        assert_eq!(values[0], Ok(Value::F64(7.0)), "{name}");
        assert!(is_unsupported(&values[1]), "{name}: ZR {:?}", values[1]);
        assert!(is_unsupported(&values[2]), "{name}: U\\G {:?}", values[2]);
        assert_eq!(values[3], Ok(Value::F64(0.0)), "{name}");
        assert_eq!(peer.sd(), vec![(302, 2)], "{name}: SD302-303 only");
        assert!(peer.ext().is_empty(), "{name}: U\\G never sent");
        assert!(
            peer.plain()
                .iter()
                .all(|&(device, ..)| device == D || device == SD),
            "{name}: ZR never sent: {:?}",
            peer.plain()
        );
        let reads = peer.all();
        assert_eq!(reads[..2].to_vec(), q_handshake(), "{name}");
        assert_eq!(
            sorted(reads[2..].to_vec()),
            sorted(vec![plain_frame(D, 100, 1), plain_frame(D, 101, 1)]),
            "{name}"
        );

        // A D write inside the allowlist and the SD302 range is one 1401.
        peer.clear();
        let results = session
            .write_batch(&[
                WriteItem::new(numeric(&driver, "D3010", DataType::U16), Value::F64(4660.0)),
                WriteItem::new(
                    numeric(&driver, "D3011", DataType::U16),
                    Value::F64(22136.0),
                ),
            ])
            .await
            .unwrap();
        assert!(results.iter().all(Result::is_ok), "{name}: {results:?}");
        assert_eq!(
            peer.writes(),
            vec![d_write_frame(3010, &[0x1234, 0x5678])],
            "{name}"
        );
        // Outside the allowlist nothing is sent.
        peer.clear();
        let results = session
            .write_batch(&[WriteItem::new(
                numeric(&driver, "D3014", DataType::U16),
                Value::F64(1.0),
            )])
            .await
            .unwrap();
        assert!(
            matches!(results.as_slice(), [Err(ItemError::Unsupported(_))]),
            "{name}: {results:?}"
        );
        assert!(peer.writes().is_empty(), "{name}");
        recorded.push((reads, name));
        session.disconnect().await;
    }
    assert_eq!(recorded[0].0, recorded[1].0, "Q12PRH and Q25PRH read alike");
}

/// A (name, code) pair that is not in the table stays Unknown: the handshake
/// stops after 0101 (no SD read), and the guest has no range for the
/// connection. The contract observed here: the read batch fails as a whole
/// with a protocol error (the configured range is unresolved), and nothing but
/// 0101 is ever sent. A write is refused per item.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #69 component"]
async fn mismatched_name_and_code_pairs_stay_unknown() {
    for (name, code) in [
        ("L02CPU", 0x0544),
        ("L26CPU-BT", 0x0545),
        ("Q12PRHCPU", 0x004c),
        ("Q25PRHCPU", 0x004b),
        ("Q06PHCPU", 0x0043),
        ("Q06UDPVCPU", 0x0367),
        ("L02CPU", 0x0543),
    ] {
        let driver = driver();
        let peer = Peer::start(name, code, &memory());
        let mut session = connect(&driver, &peer).await;
        assert_eq!(peer.all(), vec![type_name_frame()], "{name}/{code:#x}");
        let read = session
            .read_batch(&[
                numeric(&driver, "D100", DataType::U16),
                numeric(&driver, "ZR10", DataType::U16),
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
