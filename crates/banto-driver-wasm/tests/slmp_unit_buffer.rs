//! Component-level checks of the SLMP guest's U\G buffer memory reads
//! (banto-driver-wasm #65, stage G1) through this crate's adapter and write
//! gate (Hub side #917).
//!
//! Run with `BANTO_SLMP_NEW_COMPONENT_WASM=<built guest> cargo test -- --ignored`.
//! The tests drive the real engine (`FrameSession` over TCP) against a
//! loopback SLMP peer on a std thread that records every request byte for
//! byte. The peer is a mock: R04CPU, Q02UCPU or Q25PRHCPU, a D word memory and
//! a buffer memory keyed by module and address. No PLC is involved.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use banto_driver_api::{
    ConnectContext, DataType, DriverBase, ItemError, Session as _, Settings, StringEncoding,
    TagKind, TagSpec, Value, WriteItem,
};
use banto_driver_engine::{transport::TransportSpec, FrameSession};
use banto_driver_wasm::WasmDriver;

/// Same id as the Hub fixture manifest
/// `apps/banto-hub/host/tests/t12_slmp_wasm_operational_write/manifest.json`.
const MANIFEST: &[u8] = br#"{"manifestVersion":1,"id":"slmp-wasm-operational-write","version":"0.1.0","hubApi":"1","writeApi":"1","settingsVersion":1,"displayName":"SLMP U\\G fixture","transports":["tcp","udp"],"capabilities":["read","write"]}"#;

fn component() -> Vec<u8> {
    std::fs::read(std::env::var("BANTO_SLMP_NEW_COMPONENT_WASM").expect("guest path")).unwrap()
}

const D: u8 = 0xa8;
const SD: u8 = 0xa9;
/// Device-extension marker bytes of the requests.
const EXT_F8: u8 = 0xf8;
const EXT_FA: u8 = 0xfa;
const END_CODE_C051: u16 = 0xc051;

#[derive(Clone, Copy, PartialEq)]
enum Model {
    /// R04CPU.
    R,
    /// Q02UCPU (standard Q).
    Q,
    /// Q25PRHCPU: no buffer memory path.
    Q25Prh,
}

/// What the peer saw. `ext` holds each device-extension 0401 as received,
/// from the reserved bytes on (the serial number is left out).
#[derive(Default)]
struct Seen {
    ext: Vec<Vec<u8>>,
    plain: Vec<(u8, u32, u16)>,
    writes: u32,
    connections: u32,
}

struct Peer {
    address: SocketAddr,
    seen: Arc<Mutex<Seen>>,
    /// Module `h` whose buffer reads are answered with C051.
    fail_module: Arc<Mutex<Option<u16>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Peer {
    /// `buffer`: (module `h`, address) to word. `d`: D word to value. Words
    /// not listed read as 0.
    fn start(model: Model, buffer: &[(u16, u32, u16)], d: &[(u32, u16)]) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let buffer: BTreeMap<(u16, u32), u16> =
            buffer.iter().map(|&(h, n, w)| ((h, n), w)).collect();
        let d: BTreeMap<u32, u16> = d.iter().copied().collect();
        let seen = Arc::new(Mutex::new(Seen::default()));
        let fail_module = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (seen, fail_module, stop) = (seen.clone(), fail_module.clone(), stop.clone());
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
                        let (end_code, body) =
                            answer(model, &request, &buffer, &d, &seen, &fail_module);
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
            fail_module,
            stop,
            thread: Some(thread),
        }
    }

    fn ext(&self) -> Vec<Vec<u8>> {
        self.seen.lock().unwrap().ext.clone()
    }

    fn plain(&self) -> Vec<(u8, u32, u16)> {
        self.seen.lock().unwrap().plain.clone()
    }

    fn writes(&self) -> u32 {
        self.seen.lock().unwrap().writes
    }

    fn connections(&self) -> u32 {
        self.seen.lock().unwrap().connections
    }

    fn clear(&self) {
        let mut seen = self.seen.lock().unwrap();
        seen.ext.clear();
        seen.plain.clear();
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

/// Answer one 4E request: the handshake (0101, SD302 on Q / SD280-283 on R),
/// plain D reads, device-extension buffer reads. A 1401 is only counted
/// (the tests expect none) and answered with success.
fn answer(
    model: Model,
    request: &[u8],
    buffer: &BTreeMap<(u16, u32), u16>,
    d: &BTreeMap<u32, u16>,
    seen: &Mutex<Seen>,
    fail_module: &Mutex<Option<u16>>,
) -> (u16, Vec<u8>) {
    let command = u16::from_le_bytes([request[15], request[16]]);
    let sub = u16::from_le_bytes([request[17], request[18]]);
    match command {
        0x0101 => {
            let (name, code) = match model {
                Model::R => ("R04CPU", 0x4800u16),
                Model::Q => ("Q02UCPU", 0x0263),
                Model::Q25Prh => ("Q25PRHCPU", 0x004c),
            };
            let mut body = vec![b' '; 18];
            body[..name.len()].copy_from_slice(name.as_bytes());
            body[16..18].copy_from_slice(&code.to_le_bytes());
            (0, body)
        }
        0x1401 => {
            seen.lock().unwrap().writes += 1;
            (0, Vec::new())
        }
        0x0401 if sub == 0x0080 || sub == 0x0082 => {
            seen.lock().unwrap().ext.push(request[4..].to_vec());
            // Q: ... number(21..24) AB(24) 00 00 h(27..29) F8(29) count(30..32).
            // R: ... number(21..25) AB 00 00 00 h(29..31) F8/FA(31) count(32..34).
            let (h_at, count_at, number) = if sub == 0x0080 {
                (
                    27,
                    30,
                    u32::from_le_bytes([request[21], request[22], request[23], 0]),
                )
            } else {
                (
                    29,
                    32,
                    u32::from_le_bytes([request[21], request[22], request[23], request[24]]),
                )
            };
            let h = u16::from_le_bytes([request[h_at], request[h_at + 1]]);
            let count = u16::from_le_bytes([request[count_at], request[count_at + 1]]);
            if *fail_module.lock().unwrap() == Some(h) {
                return (END_CODE_C051, Vec::new());
            }
            let body = words_body(
                (0..u32::from(count)).map(|at| buffer.get(&(h, number + at)).copied().unwrap_or(0)),
            );
            (0, body)
        }
        0x0401 => {
            let (start, device, points) = if sub == 0 {
                (
                    u32::from_le_bytes([request[19], request[20], request[21], 0]),
                    request[22],
                    u16::from_le_bytes([request[23], request[24]]),
                )
            } else {
                assert_eq!(sub, 2, "R word access");
                (
                    u32::from_le_bytes([request[19], request[20], request[21], request[22]]),
                    request[23],
                    u16::from_le_bytes([request[25], request[26]]),
                )
            };
            match device {
                SD => match (model, start, points) {
                    (Model::R, 280, 4) => {
                        let mut body = 4000u32.to_le_bytes().to_vec();
                        body.extend_from_slice(&4000u32.to_le_bytes());
                        (0, body)
                    }
                    (Model::Q | Model::Q25Prh, 302, 1 | 2) => {
                        let mut body = 4000u16.to_le_bytes().to_vec();
                        if points == 2 {
                            body.extend_from_slice(&64u16.to_le_bytes());
                        }
                        (0, body)
                    }
                    _ => (END_CODE_C051, Vec::new()),
                },
                D => {
                    seen.lock().unwrap().plain.push((device, start, points));
                    let body = words_body(
                        (0..u32::from(points)).map(|at| d.get(&(start + at)).copied().unwrap_or(0)),
                    );
                    (0, body)
                }
                other => panic!("unexpected device {other:#x}"),
            }
        }
        other => panic!("unexpected command {other:#x}"),
    }
}

/// The expected request, from the reserved bytes on, laid out as the guest's
/// contract (docs/PLAN.md "G1 の設計") and the SLMP manual SH-080956ENG N
/// appendix 1 describe it: 4E reserved `00 00`, own-station route
/// `00 FF FF03 00`, data length, timer `10 00`, command 0401, then
/// Q: subcommand 0080, `00 00`, number 3 bytes, `AB`, `00 00`, h, F8, count;
/// R: subcommand 0082, `00 00`, number 4 bytes, `AB 00`, `00 00`, h, F8 (FA for
/// the CPU buffer U3E0-U3E3), count.
fn frame(model: Model, h: u16, number: u32, count: u16) -> Vec<u8> {
    let mut data = vec![0x01, 0x04];
    let ext = if (0x3e0..=0x3e3).contains(&h) {
        EXT_FA
    } else {
        EXT_F8
    };
    if model == Model::R {
        data.extend_from_slice(&[0x82, 0x00, 0x00, 0x00]);
        data.extend_from_slice(&number.to_le_bytes());
        data.extend_from_slice(&[0xab, 0x00, 0x00, 0x00]);
    } else {
        data.extend_from_slice(&[0x80, 0x00, 0x00, 0x00]);
        data.extend_from_slice(&number.to_le_bytes()[..3]);
        data.extend_from_slice(&[0xab, 0x00, 0x00]);
    }
    data.extend_from_slice(&h.to_le_bytes());
    data.push(ext);
    data.extend_from_slice(&count.to_le_bytes());
    let mut frame = vec![0x00, 0x00, 0x00, 0xff, 0xff, 0x03, 0x00];
    // The data length counts the 2-byte timer and the command data.
    frame.extend_from_slice(&u16::try_from(data.len() + 2).unwrap().to_le_bytes());
    frame.extend_from_slice(&[0x10, 0x00]);
    frame.extend_from_slice(&data);
    frame
}

fn numeric(driver: &WasmDriver, address: &str, data_type: DataType) -> TagSpec {
    let kind = TagKind::numeric(data_type);
    TagSpec::new(driver.parse_address(address, &kind).unwrap(), kind)
}

fn string(driver: &WasmDriver, address: &str, words: u16) -> TagSpec {
    let kind = TagKind::string(words, StringEncoding::Utf8);
    TagSpec::new(driver.parse_address(address, &kind).unwrap(), kind)
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

fn driver() -> Arc<WasmDriver> {
    WasmDriver::load_operational_write(MANIFEST, &component()).unwrap()
}

fn sorted(mut frames: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    frames.sort();
    frames
}

fn is_unsupported(result: &Result<Value, ItemError>) -> bool {
    matches!(result, Err(ItemError::Unsupported(_)))
}

/// G1 and #917: the Hub registers `U<h>\G<n>` numeric, word-bit and string
/// tags with the guest's canonical text, a per-module `area` and `n` as the
/// index, read-only; a string tag is accepted on U\G (not only D/W); anything
/// outside the guest's grammar is refused. `U3\G10` and `U5\G10` differ in
/// `area`, which the Hub's overlap check keys on.
#[test]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #65 component"]
fn registration_accepts_unit_buffer_tags_with_per_module_areas() {
    let driver = driver();
    let kind = |data_type| TagKind::numeric(data_type);
    for (text, area, index) in [
        ("U0\\G0", "U0\\G", 0u64),
        ("U3\\G10", "U3\\G", 10),
        ("U5\\G10", "U5\\G", 10),
        ("UFF\\G65535", "UFF\\G", 65535),
        ("U10\\G1", "U10\\G", 1),
        ("U3E0\\G100", "U3E0\\G", 100),
        ("U3E3\\G0", "U3E3\\G", 0),
    ] {
        for data_type in [
            DataType::U16,
            DataType::I16,
            DataType::U32,
            DataType::I32,
            DataType::F32,
            DataType::U64,
            DataType::I64,
            DataType::F64,
        ] {
            let parsed = driver.parse_address(text, &kind(data_type)).unwrap();
            assert_eq!(parsed.canonical, text);
            assert_eq!((parsed.area.as_str(), parsed.index), (area, Some(index)));
            assert!(!parsed.writable_area, "{text} is read-only");
            assert_eq!(parsed.bit, None);
        }
        let string = driver
            .parse_address(text, &TagKind::string(8, StringEncoding::Utf8))
            .unwrap();
        assert_eq!(
            (
                string.canonical.as_str(),
                string.area.as_str(),
                string.word_count
            ),
            (text, area, 8)
        );
        assert!(!string.writable_area);
        let last = driver
            .parse_address(text, &TagKind::string(128, StringEncoding::Utf8))
            .unwrap();
        assert_eq!(last.word_count, 128);
    }
    let bit = driver
        .parse_address("U3\\G20.F", &kind(DataType::Bit))
        .unwrap();
    assert_eq!(
        (
            bit.canonical.as_str(),
            bit.area.as_str(),
            bit.index,
            bit.bit
        ),
        ("U3\\G20.F", "U3\\G", Some(20), Some(15))
    );
    let a = driver
        .parse_address("U3\\G10", &kind(DataType::U16))
        .unwrap();
    let b = driver
        .parse_address("U5\\G10", &kind(DataType::U16))
        .unwrap();
    assert_ne!(a.area, b.area, "different modules never overlap");
    assert_eq!(a.index, b.index);
    // Writable tags are refused by the registry through `writable_area`.
    assert!(!a.writable_area);

    // Input with leading zeros or lowercase hex is stored in the canonical form.
    for (text, canonical) in [
        ("U03\\G01", "U3\\G1"),
        ("U003\\G0010", "U3\\G10"),
        ("U3e0\\G7", "U3E0\\G7"),
        ("Uff\\G1", "UFF\\G1"),
        ("U00\\G0", "U0\\G0"),
    ] {
        let parsed = driver.parse_address(text, &kind(DataType::U16)).unwrap();
        assert_eq!(parsed.canonical, canonical, "{text}");
        let string = driver
            .parse_address(text, &TagKind::string(2, StringEncoding::Utf8))
            .unwrap();
        assert_eq!(string.canonical, canonical, "{text} as string");
    }

    let kind_u16 = kind(DataType::U16);
    for text in ["U3\\G10.G", "U3\\G10.10", "U3\\G10.-1", "U3\\G10."] {
        assert!(
            driver.parse_address(text, &kind(DataType::Bit)).is_err(),
            "{text} accepted"
        );
    }
    for text in [
        "u3\\G1",
        "U3\\g1",
        "U100\\G1",
        "U3DF\\G1",
        "U3E4\\G1",
        "U3\\HG1",
        "U3\\G",
        "U3\\G-1",
        "U3\\G4294967296",
    ] {
        assert!(driver.parse_address(text, &kind_u16).is_err(), "{text}");
        assert!(
            driver
                .parse_address(text, &TagKind::string(4, StringEncoding::Utf8))
                .is_err(),
            "{text} as string"
        );
    }
    for words in [0u16, 129] {
        assert!(
            driver
                .parse_address("U3\\G10", &TagKind::string(words, StringEncoding::Utf8))
                .is_err(),
            "{words} words"
        );
    }
    // A bit needs a word-bit suffix on U\G; a plain bit device is not a thing.
    assert!(driver
        .parse_address("U3\\G10", &kind(DataType::Bit))
        .is_err());
}

/// G1 on an R04CPU: numeric, word-bit and string tags read through subcommand
/// 0082, byte for byte (FAH only for the CPU buffer U3E0), one request per tag
/// (never merged with an adjacent U\G tag, the same module's next address, or
/// the D tags, which are read by plain 0401), values decoded like D words.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #65 component"]
async fn r_unit_buffer_numeric_bit_and_string_reads_are_one_exact_request_per_tag() {
    let driver = driver();
    let peer = Peer::start(
        Model::R,
        &[
            (3, 1, 0x5678),
            (3, 2, 0x0001),
            (3, 3, 0x00aa),
            (3, 10, 0x1234),
            (3, 11, 0x4321),
            (3, 20, 0x0020),
            (3, 30, u16::from_le_bytes(*b"HE")),
            (3, 31, u16::from_le_bytes(*b"LL")),
            (3, 32, u16::from_le_bytes(*b"O\0")),
            (0x3e0, 1, 0xbeef),
            (0x3e0, 2, 0x0001),
            (0xff, 70000, 0x7fff),
            (0, 0, 0xfffe),
        ],
        &[(100, 7), (101, 8)],
    );
    let mut session = connect(&driver, &peer).await;
    let specs = vec![
        numeric(&driver, "U3\\G1", DataType::U32),
        numeric(&driver, "U3\\G10", DataType::U16),
        numeric(&driver, "U3\\G11", DataType::I16),
        numeric(&driver, "D100", DataType::U16),
        numeric(&driver, "D101", DataType::U16),
        numeric(&driver, "U3\\G20.5", DataType::Bit),
        numeric(&driver, "U3\\G20.4", DataType::Bit),
        string(&driver, "U3\\G30", 3),
        numeric(&driver, "U3E0\\G1", DataType::U32),
        numeric(&driver, "UFF\\G70000", DataType::I16),
        numeric(&driver, "U0\\G0", DataType::I16),
    ];
    let values = session.read_batch(&specs).await.unwrap();
    assert_eq!(
        values,
        vec![
            Ok(Value::F64(f64::from(0x0001_5678u32))),
            Ok(Value::F64(f64::from(0x1234u16))),
            Ok(Value::F64(f64::from(0x4321i16))),
            Ok(Value::F64(7.0)),
            Ok(Value::F64(8.0)),
            Ok(Value::Bit(true)),
            Ok(Value::Bit(false)),
            Ok(Value::Str("HELLO".into())),
            Ok(Value::F64(f64::from(0x0001_beefu32))),
            Ok(Value::F64(f64::from(0x7fffi16))),
            Ok(Value::F64(f64::from(-2i16))),
        ]
    );
    // U3E0\G1 with two words is the manual's own R communication example
    // (SH-080956ENG N appendix 1), here in a 4E frame with the guest's timer.
    assert_eq!(
        peer.ext().len(),
        9,
        "one request per U\\G tag: {:x?}",
        peer.ext()
    );
    assert_eq!(
        sorted(peer.ext()),
        sorted(vec![
            frame(Model::R, 3, 1, 2),
            frame(Model::R, 3, 10, 1),
            frame(Model::R, 3, 11, 1),
            frame(Model::R, 3, 20, 1),
            frame(Model::R, 3, 20, 1),
            frame(Model::R, 3, 30, 3),
            frame(Model::R, 0x3e0, 1, 2),
            frame(Model::R, 0xff, 70000, 1),
            frame(Model::R, 0, 0, 1),
        ])
    );
    // FAH exactly once-per-request for the CPU buffer, F8H for the rest.
    let ext_bytes: Vec<u8> = peer.ext().iter().map(|f| f[f.len() - 3]).collect();
    assert_eq!(ext_bytes.iter().filter(|&&b| b == EXT_FA).count(), 1);
    assert_eq!(ext_bytes.iter().filter(|&&b| b == EXT_F8).count(), 8);
    // The D tags stay on plain 0401 and never carry a U\G tag.
    assert!(
        peer.plain().iter().all(|&(device, start, points)| {
            device == D && start >= 100 && start + u32::from(points) <= 102
        }),
        "{:x?}",
        peer.plain()
    );
    assert!(!peer.plain().is_empty());
    assert_eq!(peer.writes(), 0);
    assert_eq!(peer.connections(), 1);
    session.disconnect().await;
}

/// G1 on a Q02UCPU: subcommand 0080 with a 3-byte number. The CPU buffer
/// (U3E0-U3E3) and a span ending past 0xFFFFFF are per-item Unsupported,
/// decided before anything is sent, and the connection stays up; the other
/// tags in the same batch and later batches are read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #65 component"]
async fn q_unit_buffer_reads_use_0080_and_model_limits_are_per_item() {
    let driver = driver();
    let peer = Peer::start(
        Model::Q,
        &[
            (3, 1, 0x5678),
            (3, 2, 0x0001),
            (3, 20, 0x8000),
            (3, 30, u16::from_le_bytes(*b"Q!")),
            (0xff, 0xff_ffff, 0x0abc),
        ],
        &[(100, 9)],
    );
    let mut session = connect(&driver, &peer).await;
    let specs = vec![
        numeric(&driver, "U3\\G1", DataType::U32),
        numeric(&driver, "U3E0\\G1", DataType::U16),
        numeric(&driver, "U3\\G20.F", DataType::Bit),
        numeric(&driver, "UFF\\G16777215", DataType::U32),
        numeric(&driver, "D100", DataType::U16),
        string(&driver, "U3\\G30", 1),
        numeric(&driver, "UFF\\G16777215", DataType::U16),
        string(&driver, "U3E3\\G0", 2),
    ];
    let values = session.read_batch(&specs).await.unwrap();
    assert_eq!(values.len(), specs.len());
    assert_eq!(values[0], Ok(Value::F64(f64::from(0x0001_5678u32))));
    assert!(is_unsupported(&values[1]), "{:?}", values[1]);
    assert_eq!(values[2], Ok(Value::Bit(true)));
    assert!(is_unsupported(&values[3]), "{:?}", values[3]);
    assert_eq!(values[4], Ok(Value::F64(9.0)));
    assert_eq!(values[5], Ok(Value::Str("Q!".into())));
    assert_eq!(values[6], Ok(Value::F64(f64::from(0x0abcu16))));
    assert!(is_unsupported(&values[7]), "{:?}", values[7]);
    // U3\G1 with two words is the manual's own Q communication example.
    assert_eq!(
        sorted(peer.ext()),
        sorted(vec![
            frame(Model::Q, 3, 1, 2),
            frame(Model::Q, 3, 20, 1),
            frame(Model::Q, 3, 30, 1),
            frame(Model::Q, 0xff, 0xff_ffff, 1),
        ]),
        "unsupported tags are never sent"
    );
    // The same session still reads afterwards.
    peer.clear();
    let again = session
        .read_batch(&[numeric(&driver, "U3\\G1", DataType::U32)])
        .await
        .unwrap();
    assert_eq!(again, vec![Ok(Value::F64(f64::from(0x0001_5678u32)))]);
    assert_eq!(sorted(peer.ext()), vec![frame(Model::Q, 3, 1, 2)]);
    assert_eq!(peer.connections(), 1, "the connection was not dropped");
    assert_eq!(peer.writes(), 0);
    session.disconnect().await;
}

/// Q25PRH has no buffer memory path: every U\G tag is a per-item
/// Unsupported, nothing is sent for it, the D tag in the same batch is read
/// and the connection stays up.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #65 component"]
async fn q25prh_unit_buffer_tags_are_unsupported_per_item_and_never_sent() {
    let driver = driver();
    let peer = Peer::start(Model::Q25Prh, &[(3, 1, 1)], &[(100, 5)]);
    let mut session = connect(&driver, &peer).await;
    let values = session
        .read_batch(&[
            numeric(&driver, "U3\\G1", DataType::U16),
            numeric(&driver, "D100", DataType::U16),
            string(&driver, "U0\\G0", 2),
            numeric(&driver, "U3\\G1.0", DataType::Bit),
        ])
        .await
        .unwrap();
    assert!(is_unsupported(&values[0]), "{:?}", values[0]);
    assert_eq!(values[1], Ok(Value::F64(5.0)));
    assert!(is_unsupported(&values[2]), "{:?}", values[2]);
    assert!(is_unsupported(&values[3]), "{:?}", values[3]);
    assert!(peer.ext().is_empty(), "{:x?}", peer.ext());
    assert_eq!(peer.connections(), 1);
    session.disconnect().await;
}

/// A PLC end code on one U\G request is that item's device error only; the
/// other U\G and D tags in the batch are read, and the next batch works on
/// the same connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #65 component"]
async fn plc_end_code_on_a_unit_buffer_request_is_an_item_error() {
    for model in [Model::R, Model::Q] {
        let driver = driver();
        let peer = Peer::start(model, &[(3, 1, 11), (4, 1, 22)], &[(100, 33)]);
        *peer.fail_module.lock().unwrap() = Some(4);
        let mut session = connect(&driver, &peer).await;
        let specs = vec![
            numeric(&driver, "U3\\G1", DataType::U16),
            numeric(&driver, "U4\\G1", DataType::U16),
            string(&driver, "U4\\G1", 2),
            numeric(&driver, "D100", DataType::U16),
        ];
        let values = session.read_batch(&specs).await.unwrap();
        assert_eq!(values[0], Ok(Value::F64(11.0)));
        for failed in [&values[1], &values[2]] {
            match failed {
                Err(ItemError::Device(error)) => assert_eq!(error.code, u32::from(END_CODE_C051)),
                other => panic!("device error expected: {other:?}"),
            }
        }
        assert_eq!(values[3], Ok(Value::F64(33.0)));
        *peer.fail_module.lock().unwrap() = None;
        let again = session.read_batch(&specs).await.unwrap();
        assert_eq!(again[1], Ok(Value::F64(22.0)));
        assert_eq!(peer.connections(), 1);
        session.disconnect().await;
    }
}

/// The buffer memory is read-only: a write to a U\G numeric, word-bit or
/// string tag is refused before anything is sent (the Hub gate takes only
/// D/W; the allowlist cannot name U\G), and the connection reads on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #65 component"]
async fn unit_buffer_writes_are_refused_before_send() {
    for model in [Model::R, Model::Q] {
        let driver = driver();
        let peer = Peer::start(model, &[(3, 10, 0x1234)], &[]);
        let mut session = connect(&driver, &peer).await;
        for item in [
            WriteItem::new(numeric(&driver, "U3\\G10", DataType::U16), Value::F64(1.0)),
            WriteItem::new(numeric(&driver, "U3\\G10", DataType::U32), Value::F64(1.0)),
            WriteItem::new(
                numeric(&driver, "U3\\G10.1", DataType::Bit),
                Value::Bit(true),
            ),
            WriteItem::new(string(&driver, "U3\\G10", 2), Value::Str("AB".into())),
        ] {
            let results = session
                .write_batch(std::slice::from_ref(&item))
                .await
                .unwrap();
            assert!(
                matches!(results.as_slice(), [Err(ItemError::Unsupported(_))]),
                "{results:?} for {}",
                item.spec.address.canonical
            );
        }
        // Mixed with an allowed D word, the whole batch is still refused.
        let results = session
            .write_batch(&[
                WriteItem::new(numeric(&driver, "D3010", DataType::U16), Value::F64(1.0)),
                WriteItem::new(numeric(&driver, "U3\\G10", DataType::U16), Value::F64(1.0)),
            ])
            .await
            .unwrap();
        assert!(
            results
                .iter()
                .all(|r| matches!(r, Err(ItemError::Unsupported(_)))),
            "{results:?}"
        );
        assert_eq!(peer.writes(), 0, "no 1401 reached the PLC");
        let read = session
            .read_batch(&[numeric(&driver, "U3\\G10", DataType::U16)])
            .await
            .unwrap();
        assert_eq!(read, vec![Ok(Value::F64(f64::from(0x1234u16)))]);
        assert_eq!(peer.connections(), 1);
        session.disconnect().await;
    }
}
