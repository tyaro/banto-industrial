//! Component-level checks of the SLMP guest's G3 GX Works3 global labels
//! (banto-driver-wasm #74) through this crate's adapter and write gate (Hub
//! side acceptance, mock only).
//!
//! Run with `BANTO_SLMP_NEW_COMPONENT_WASM=<built guest> cargo test -- --ignored`.
//! A label tag is `@` and the name as written (case kept, non-ASCII allowed),
//! area `@`, no index. The guest reads labels on a standard iQ-R (0101 says
//! `StandardR`) with one 041C (Label Read Random, subcommand 0000, no
//! abbreviation) per request, names in UTF-16LE, and checks every label's data
//! type ID and length against the tag type: a mismatch is that item's
//! `Rejected`. A write is a 041C type check of every label, then one 141B
//! (Label Write Random) of all items, judged by its end code; a mismatch at the
//! check writes nothing. The Hub's own gate (`label_write_allowlist`, same
//! grammar as the guest's) refuses a label outside the list, a label mixed with
//! device tags or named twice before the guest plans anything. On a standard Q
//! the guest reads labels as per-item `Unsupported` and refuses writes, sending
//! neither command.
//!
//! The previous pin `aa3a1c1` has no label grammar (`@...` does not parse), so
//! every test below fails there at the first label tag (checked 2026-11-06).
//! The tests drive the real engine (`FrameSession` over TCP) against a
//! loopback SLMP peer on a std thread that records every request byte for
//! byte. The peer is a mock (4E, iQ-R wire); no PLC is involved. Which end
//! codes a real CPU returns for a missing label or a label without "Access from
//! External Device", and whether GX Works3 compares names case-sensitively,
//! are not confirmed.

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
const MANIFEST: &[u8] = br#"{"manifestVersion":1,"id":"slmp-wasm-operational-write","version":"0.1.0","hubApi":"1","writeApi":"1","settingsVersion":1,"displayName":"SLMP G3 fixture","transports":["tcp","udp"],"capabilities":["read","write"]}"#;

fn component() -> Vec<u8> {
    std::fs::read(std::env::var("BANTO_SLMP_NEW_COMPONENT_WASM").expect("guest path")).unwrap()
}

const D: u8 = 0xa8;
const SD: u8 = 0xa9;
const END_CODE_C051: u16 = 0xc051;
/// The end code the peer returns for a 041C naming a label it does not have.
const END_CODE_NO_LABEL: u16 = 0x40c0;
const HUB_GATE: &str = "SLMP write is disabled or outside the connection allowlist";

const OWN: [u8; 5] = [0x00, 0xff, 0xff, 0x03, 0x00];

/// The peer's labels: name (as sent, without `@`) -> (data type ID, data).
fn labels() -> BTreeMap<String, (u8, Vec<u8>)> {
    [
        ("タンク1.水位", 2u8, 1234u16.to_le_bytes().to_vec()),
        ("Tank1.Level", 6, 2.5f32.to_le_bytes().to_vec()),
        ("Arr[3]", 1, vec![1, 0]),
        ("Arr[4]", 1, vec![0, 0]),
        ("Count", 5, (-2i32).to_le_bytes().to_vec()),
        ("Total", 7, (-2.25f64).to_le_bytes().to_vec()),
        ("Name", 9, b"ABC\0\0\0\0\0".to_vec()),
        ("Word", 4, (-1i16).to_le_bytes().to_vec()),
    ]
    .into_iter()
    .map(|(name, id, data)| (name.to_string(), (id, data)))
    .collect()
}

/// What the peer saw. Frames are whole requests with the 4E serial zeroed.
#[derive(Default)]
struct Seen {
    all: Vec<Vec<u8>>,
    /// Each 041C as its list of names.
    label_reads: Vec<Vec<String>>,
    /// Each 141B, whole.
    label_writes: Vec<Vec<u8>>,
    /// Plain 0401 reads of non-SD devices: (device, start, points).
    plain: Vec<(u8, u32, u16)>,
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
    /// A mock PLC answering 0101 with `name`/`code` and following the frame
    /// it receives (3E `50 00` or 4E `54 00`). It answers the iQ-R handshake
    /// (SD280-283) and the Q one (SD302-303), D100 = 7, and 041C/141B from
    /// [`labels`].
    fn start(name: &'static str, code: u16) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Seen::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (seen, stop) = (seen.clone(), stop.clone());
            std::thread::spawn(move || {
                let memory = labels();
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
                        let (end_code, body) = answer(name, code, &request, &seen, &memory);
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

    fn label_reads(&self) -> Vec<Vec<String>> {
        self.seen.lock().unwrap().label_reads.clone()
    }

    fn label_writes(&self) -> Vec<Vec<u8>> {
        self.seen.lock().unwrap().label_writes.clone()
    }

    fn plain(&self) -> Vec<(u8, u32, u16)> {
        self.seen.lock().unwrap().plain.clone()
    }

    fn connections(&self) -> u32 {
        self.seen.lock().unwrap().connections
    }

    fn clear(&self) {
        let mut seen = self.seen.lock().unwrap();
        seen.all.clear();
        seen.label_reads.clear();
        seen.label_writes.clear();
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

/// The names of a 041C/141B request body (after the label and abbreviation
/// counts), and for 141B the write data after each name.
fn label_names(data: &[u8], with_data: bool) -> Vec<String> {
    let count = usize::from(u16::from_le_bytes([data[0], data[1]]));
    assert_eq!(&data[2..4], &[0, 0], "no abbreviation definition");
    let mut at = 4;
    let mut names = Vec::new();
    for _ in 0..count {
        let units = usize::from(u16::from_le_bytes([data[at], data[at + 1]]));
        at += 2;
        let name = (0..units)
            .map(|i| u16::from_le_bytes([data[at + 2 * i], data[at + 2 * i + 1]]))
            .collect::<Vec<_>>();
        at += 2 * units;
        names.push(String::from_utf16(&name).unwrap());
        if with_data {
            let len = usize::from(u16::from_le_bytes([data[at], data[at + 1]]));
            at += 2 + len;
        }
    }
    assert_eq!(at, data.len(), "whole request consumed");
    names
}

fn answer(
    name: &str,
    code: u16,
    request: &[u8],
    seen: &Mutex<Seen>,
    memory: &BTreeMap<String, (u8, Vec<u8>)>,
) -> (u16, Vec<u8>) {
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
        0x041c => {
            assert_eq!(sub, 0, "041C subcommand 0000");
            let names = label_names(data, false);
            seen.lock().unwrap().label_reads.push(names.clone());
            let mut body = u16::try_from(names.len()).unwrap().to_le_bytes().to_vec();
            for name in &names {
                let Some((type_id, value)) = memory.get(name) else {
                    return (END_CODE_NO_LABEL, Vec::new());
                };
                body.extend_from_slice(&[*type_id, 0]);
                body.extend_from_slice(&u16::try_from(value.len()).unwrap().to_le_bytes());
                body.extend_from_slice(value);
            }
            (0, body)
        }
        0x141b => {
            assert_eq!(sub, 0, "141B subcommand 0000");
            label_names(data, true);
            seen.lock().unwrap().label_writes.push(recorded);
            (0, Vec::new())
        }
        0x1401 => panic!("no device write is expected"),
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
                return match (start, points) {
                    (280, 4) => {
                        let mut body = 8000u32.to_le_bytes().to_vec();
                        body.extend_from_slice(&0x200u32.to_le_bytes());
                        (0, body)
                    }
                    (302, 2) => {
                        let mut body = 4000u16.to_le_bytes().to_vec();
                        body.extend_from_slice(&64u16.to_le_bytes());
                        (0, body)
                    }
                    _ => (END_CODE_C051, Vec::new()),
                };
            }
            seen.lock().unwrap().plain.push((device, start, points));
            match device {
                D => (
                    0,
                    (0..u32::from(points))
                        .flat_map(|at| if start + at == 100 { 7u16 } else { 0 }.to_le_bytes())
                        .collect(),
                ),
                _ => (END_CODE_C051, Vec::new()),
            }
        }
        other => panic!("unexpected command {other:#x}"),
    }
}

/// A 4E request (serial zeroed) with the own-station route and timer 0010H.
fn frame_4e(data: &[u8]) -> Vec<u8> {
    let mut frame = vec![0x54, 0x00, 0x00, 0x00, 0x00, 0x00];
    frame.extend_from_slice(&OWN);
    // The data length counts the 2-byte timer and the command data.
    frame.extend_from_slice(&u16::try_from(data.len() + 2).unwrap().to_le_bytes());
    frame.extend_from_slice(&0x0010u16.to_le_bytes());
    frame.extend_from_slice(data);
    frame
}

/// A name as sent: UTF-16 length in code units, then UTF-16LE.
fn wire_name(name: &str) -> Vec<u8> {
    let units = name.encode_utf16().collect::<Vec<_>>();
    let mut out = u16::try_from(units.len()).unwrap().to_le_bytes().to_vec();
    out.extend(units.iter().flat_map(|unit| unit.to_le_bytes()));
    out
}

/// A 041C (subcommand 0000, no abbreviation) of these names.
fn label_read(names: &[&str]) -> Vec<u8> {
    let mut data = vec![0x1c, 0x04, 0x00, 0x00];
    data.extend_from_slice(&u16::try_from(names.len()).unwrap().to_le_bytes());
    data.extend_from_slice(&[0, 0]);
    for name in names {
        data.extend(wire_name(name));
    }
    frame_4e(&data)
}

/// A 141B (subcommand 0000, no abbreviation) of these (name, data) items.
fn label_write(items: &[(&str, &[u8])]) -> Vec<u8> {
    let mut data = vec![0x1b, 0x14, 0x00, 0x00];
    data.extend_from_slice(&u16::try_from(items.len()).unwrap().to_le_bytes());
    data.extend_from_slice(&[0, 0]);
    for (name, value) in items {
        data.extend(wire_name(name));
        data.extend_from_slice(&u16::try_from(value.len()).unwrap().to_le_bytes());
        data.extend_from_slice(value);
    }
    frame_4e(&data)
}

/// The 4E handshake of an iQ-R: 0101, then SD280-283 (R wire).
fn r_handshake() -> Vec<Vec<u8>> {
    let mut sd = vec![0x01, 0x04, 0x02, 0x00];
    sd.extend_from_slice(&280u32.to_le_bytes());
    sd.extend_from_slice(&[SD, 0x00, 0x04, 0x00]);
    vec![frame_4e(&[0x01, 0x01, 0x00, 0x00]), frame_4e(&sd)]
}

fn tag(driver: &WasmDriver, address: &str, data_type: DataType) -> TagSpec {
    let kind = TagKind::numeric(data_type);
    TagSpec::new(driver.parse_address(address, &kind).unwrap(), kind)
}

fn string(driver: &WasmDriver, address: &str, words: u16) -> TagSpec {
    let kind = TagKind::string(words, StringEncoding::Utf8);
    TagSpec::new(driver.parse_address(address, &kind).unwrap(), kind)
}

fn driver() -> Arc<WasmDriver> {
    WasmDriver::load_operational_write(MANIFEST, &component()).unwrap()
}

/// The label allowlist of the write tests: exact non-ASCII and ASCII labels,
/// an array range and a string label.
const LABEL_ALLOW: &str = "@タンク1.水位;@Tank1.Level;@Arr[0-3];@Name;@Count;@Total;@Word";

async fn connect(driver: &Arc<WasmDriver>, peer: &Peer, extra: &[(&str, &str)]) -> FrameSession {
    let port = peer.address.port().to_string();
    let mut pairs = vec![("host", "127.0.0.1"), ("port", port.as_str())];
    pairs.extend_from_slice(extra);
    let settings = Settings::from_pairs(pairs).unwrap();
    assert_eq!(driver.validate_config(&settings), Vec::new());
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

/// The guest's address grammar reaches the Hub adapter: `@` and the name as
/// written (non-ASCII kept, subscripts canonicalized), area `@`, no index,
/// writable area, word count from the type; `offset` moves the last subscript.
/// Malformed labels, bit specification and 64-bit integers are refused.
#[test]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #74 component"]
fn label_addresses_parse_through_the_adapter() {
    let driver = driver();
    for (text, data_type, canonical, words) in [
        ("@タンク1.水位", DataType::U16, "@タンク1.水位", 1),
        ("@Tank1.Level", DataType::F32, "@Tank1.Level", 2),
        ("@Arr[03]", DataType::Bit, "@Arr[3]", 1),
        ("@M[01,002]", DataType::I32, "@M[1,2]", 2),
        ("@St.Arr[2].x", DataType::F64, "@St.Arr[2].x", 4),
        ("@tank1.level", DataType::I16, "@tank1.level", 1),
    ] {
        let parsed = tag(&driver, text, data_type).address;
        assert_eq!(
            (
                parsed.canonical.as_str(),
                parsed.area.as_str(),
                parsed.index,
                parsed.bit,
                parsed.word_count,
                parsed.writable_area
            ),
            (canonical, "@", None, None, words, true),
            "{text}"
        );
    }
    let name = string(&driver, "@名前", 8).address;
    assert_eq!((name.area.as_str(), name.word_count), ("@", 8));
    for (text, data_type) in [
        ("@X.3", DataType::Bit),
        ("@A", DataType::I64),
        ("@A", DataType::U64),
        ("@", DataType::U16),
        ("@A B", DataType::U16),
        ("@Arr[1,2,3,4]", DataType::U16),
    ] {
        assert!(
            driver
                .parse_address(text, &TagKind::numeric(data_type))
                .is_err(),
            "{text}"
        );
    }
    let u16_kind = TagKind::numeric(DataType::U16);
    assert_eq!(
        driver.offset_address("@M[1,2]", &u16_kind, 3).unwrap(),
        "@M[1,5]"
    );
    assert!(driver.offset_address("@Tank1.Level", &u16_kind, 1).is_err());
}

/// A standard iQ-R (R08CPU/4801H, 4E) reads every label of a batch with one
/// 041C, byte for byte (UTF-16LE names, non-ASCII included, in batch order),
/// next to the D word read of the same batch. Each value is decoded by the
/// tag's type; a label whose data type ID differs from the tag's (`@Word` is
/// ID 4, an i16, read as u16) is that item's `Rejected`, the rest still read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #74 component"]
async fn standard_r_reads_labels_with_one_041c_and_rejects_a_type_mismatch_per_item() {
    let driver = driver();
    let peer = Peer::start("R08CPU", 0x4801);
    let mut session = connect(&driver, &peer, &[]).await;
    assert_eq!(peer.all(), r_handshake());
    peer.clear();
    let specs = [
        tag(&driver, "@タンク1.水位", DataType::U16),
        tag(&driver, "@Tank1.Level", DataType::F32),
        tag(&driver, "D100", DataType::U16),
        tag(&driver, "@Arr[3]", DataType::Bit),
        tag(&driver, "@Count", DataType::I32),
        tag(&driver, "@Total", DataType::F64),
        string(&driver, "@Name", 4),
        tag(&driver, "@Word", DataType::U16),
    ];
    let values = session.read_batch(&specs).await.unwrap();
    assert_eq!(values[0], Ok(Value::F64(1234.0)));
    assert_eq!(values[1], Ok(Value::F64(2.5)));
    assert_eq!(values[2], Ok(Value::F64(7.0)));
    assert_eq!(values[3], Ok(Value::Bit(true)));
    assert_eq!(values[4], Ok(Value::F64(-2.0)));
    assert_eq!(values[5], Ok(Value::F64(-2.25)));
    assert_eq!(values[6], Ok(Value::Str("ABC".into())));
    assert!(
        matches!(&values[7], Err(ItemError::Rejected(m)) if m.contains("ID is 4")),
        "{:?}",
        values[7]
    );
    let names = [
        "タンク1.水位",
        "Tank1.Level",
        "Arr[3]",
        "Count",
        "Total",
        "Name",
        "Word",
    ];
    assert_eq!(peer.label_reads(), vec![names.map(String::from).to_vec()]);
    let mut frames = peer.all();
    frames.sort();
    let mut d_read = vec![0x01, 0x04, 0x02, 0x00];
    d_read.extend_from_slice(&100u32.to_le_bytes());
    d_read.extend_from_slice(&[D, 0x00, 0x01, 0x00]);
    let mut expected = vec![label_read(&names), frame_4e(&d_read)];
    expected.sort();
    assert_eq!(frames, expected, "one 041C and one D read");
    // `タ` is U+30BF: low byte first.
    assert!(label_read(&["タ"]).ends_with(&[0x01, 0x00, 0xbf, 0x30]));
    assert_eq!(peer.connections(), 1, "connection kept");

    // A 041C the PLC refuses is a device error for each of its labels; the
    // D read of the same batch is unaffected.
    peer.clear();
    let values = session
        .read_batch(&[
            tag(&driver, "@Missing", DataType::U16),
            tag(&driver, "@Count", DataType::I32),
            tag(&driver, "D100", DataType::U16),
        ])
        .await
        .unwrap();
    assert!(
        values[..2].iter().all(
            |value| matches!(value, Err(ItemError::Device(e)) if e.code == u32::from(END_CODE_NO_LABEL))
        ),
        "{values:?}"
    );
    assert_eq!(values[2], Ok(Value::F64(7.0)));
    assert_eq!(
        peer.label_reads(),
        vec![vec!["Missing".to_string(), "Count".to_string()]]
    );
    session.disconnect().await;
}

/// A label write allowed by both gates is one 041C type check of every label,
/// then one 141B of all items in order (bit as one word, numbers in the
/// label's own little-endian layout, the string as its encoded bytes, names in
/// UTF-16LE), and nothing else. Only a label list is set: the device list is
/// empty and `enabled` still validates.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #74 component"]
async fn allowed_label_write_is_one_041c_check_then_one_141b() {
    let driver = driver();
    let peer = Peer::start("R08CPU", 0x4801);
    let mut session = connect(
        &driver,
        &peer,
        &[
            ("d_u16_write_mode", "enabled"),
            ("label_write_allowlist", LABEL_ALLOW),
        ],
    )
    .await;
    // No device list: the handshake is the read-only one.
    assert_eq!(peer.all(), r_handshake());
    peer.clear();
    let items = [
        WriteItem::new(
            tag(&driver, "@タンク1.水位", DataType::U16),
            Value::F64(4660.0),
        ),
        WriteItem::new(tag(&driver, "@Tank1.Level", DataType::F32), Value::F64(1.5)),
        WriteItem::new(tag(&driver, "@Arr[3]", DataType::Bit), Value::Bit(false)),
        WriteItem::new(tag(&driver, "@Count", DataType::I32), Value::F64(-3.0)),
        WriteItem::new(tag(&driver, "@Total", DataType::F64), Value::F64(0.5)),
        WriteItem::new(string(&driver, "@Name", 4), Value::Str("XYZ".into())),
    ];
    let results = session.write_batch(&items).await.unwrap();
    assert!(results.iter().all(Result::is_ok), "{results:?}");
    let names = [
        "タンク1.水位",
        "Tank1.Level",
        "Arr[3]",
        "Count",
        "Total",
        "Name",
    ];
    let write = label_write(&[
        ("タンク1.水位", &4660u16.to_le_bytes()),
        ("Tank1.Level", &1.5f32.to_le_bytes()),
        ("Arr[3]", &[0, 0]),
        ("Count", &(-3i32).to_le_bytes()),
        ("Total", &0.5f64.to_le_bytes()),
        ("Name", b"XYZ\0\0\0\0\0"),
    ]);
    assert_eq!(peer.all(), vec![label_read(&names), write.clone()]);
    assert_eq!(peer.label_writes(), vec![write]);
    assert!(peer.plain().is_empty(), "no device read");
    assert_eq!(peer.connections(), 1);
    session.disconnect().await;
}

/// The guest's own checks: a label whose data type differs from the tag's at
/// the 041C check writes nothing (every item `Rejected`, no 141B); a string
/// that fills every byte (no NUL end byte) is refused by the guest (the Hub
/// passes it on); a standard Q (Q06HCPU, 3E) reads labels as per-item
/// `Unsupported` and refuses label writes, sending neither 041C nor 141B.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #74 component"]
async fn guest_refuses_type_mismatch_unterminated_string_and_standard_q() {
    let driver = driver();
    let peer = Peer::start("R08CPU", 0x4801);
    let mut session = connect(
        &driver,
        &peer,
        &[
            ("d_u16_write_mode", "enabled"),
            ("label_write_allowlist", LABEL_ALLOW),
        ],
    )
    .await;
    peer.clear();
    // `@Word` is ID 4 (i16); a u16 tag does not match.
    let results = session
        .write_batch(&[
            WriteItem::new(tag(&driver, "@Count", DataType::I32), Value::F64(1.0)),
            WriteItem::new(tag(&driver, "@Word", DataType::U16), Value::F64(1.0)),
        ])
        .await
        .unwrap();
    assert!(
        results
            .iter()
            .all(|result| matches!(result, Err(ItemError::Rejected(_)))),
        "{results:?}"
    );
    assert_eq!(peer.all(), vec![label_read(&["Count", "Word"])]);
    assert!(peer.label_writes().is_empty(), "no 141B on a mismatch");

    peer.clear();
    let results = session
        .write_batch(&[WriteItem::new(
            string(&driver, "@Name", 4),
            Value::Str("ABCDEFGH".into()),
        )])
        .await
        .unwrap();
    assert!(
        matches!(&results[..], [Err(ItemError::Rejected(m))] if m.contains("NUL")),
        "{results:?}"
    );
    assert!(peer.label_writes().is_empty(), "no 141B without a NUL byte");
    assert!(peer.all().is_empty(), "refused before the 041C check");
    assert_eq!(peer.connections(), 1);
    session.disconnect().await;

    let peer = Peer::start("Q06HCPU", 0x0042);
    let mut session = connect(
        &driver,
        &peer,
        &[
            ("frame", "3e"),
            ("d_u16_write_mode", "enabled"),
            ("label_write_allowlist", LABEL_ALLOW),
        ],
    )
    .await;
    peer.clear();
    let values = session
        .read_batch(&[
            tag(&driver, "@Count", DataType::I32),
            tag(&driver, "D100", DataType::U16),
        ])
        .await
        .unwrap();
    assert!(
        matches!(values[0], Err(ItemError::Unsupported(_))),
        "{values:?}"
    );
    assert_eq!(values[1], Ok(Value::F64(7.0)));
    let results = session
        .write_batch(&[WriteItem::new(
            tag(&driver, "@Count", DataType::I32),
            Value::F64(1.0),
        )])
        .await
        .unwrap();
    assert!(
        matches!(&results[..], [Err(ItemError::Unsupported(m))] if m != HUB_GATE),
        "{results:?}"
    );
    assert!(peer.label_reads().is_empty(), "no 041C on Q");
    assert!(peer.label_writes().is_empty(), "no 141B on Q");
    assert_eq!(peer.plain(), vec![(D, 100, 1)]);
    session.disconnect().await;
}

/// The Hub's gate refuses before the guest plans anything, so the peer sees no
/// request at all: a label outside `label_write_allowlist` (case-sensitive,
/// last subscript outside the range), one such label among allowed ones, a
/// label mixed with an allowed device tag, the same label twice, a disabled
/// mode, and a connection whose only list is the device one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #74 component"]
async fn hub_gate_refuses_labels_outside_the_list_before_anything_is_sent() {
    let driver = driver();
    let peer = Peer::start("R08CPU", 0x4801);
    let mut session = connect(
        &driver,
        &peer,
        &[
            ("d_u16_write_mode", "enabled"),
            ("d_u16_write_allowlist", "D100-D105"),
            ("label_write_allowlist", LABEL_ALLOW),
        ],
    )
    .await;
    let u16_write =
        |address: &str| WriteItem::new(tag(&driver, address, DataType::U16), Value::F64(1.0));
    let cases: Vec<(Vec<WriteItem>, &str)> = vec![
        (vec![u16_write("@Arr[4]")], HUB_GATE),
        (vec![u16_write("@tank1.level")], HUB_GATE),
        (vec![u16_write("@Other")], HUB_GATE),
        (vec![u16_write("@Arr[0]"), u16_write("@Arr[9]")], HUB_GATE),
        (
            vec![u16_write("@Arr[0]"), u16_write("D100")],
            "labels cannot share a write request with device tags",
        ),
        (
            vec![u16_write("D100"), u16_write("@Arr[0]")],
            "labels cannot share a write request with device tags",
        ),
        (
            vec![u16_write("@Arr[0]"), u16_write("@Arr[0]")],
            "a label must not be written twice in one request",
        ),
    ];
    peer.clear();
    for (items, reason) in cases {
        let results = session.write_batch(&items).await.unwrap();
        assert_eq!(results.len(), items.len());
        assert!(
            results
                .iter()
                .all(|result| matches!(result, Err(ItemError::Unsupported(m)) if m == reason)),
            "{reason}: {results:?}"
        );
    }
    assert!(peer.all().is_empty(), "nothing sent: {:02x?}", peer.all());
    session.disconnect().await;

    for settings in [
        &[
            ("d_u16_write_mode", "disabled"),
            ("label_write_allowlist", LABEL_ALLOW),
        ][..],
        &[
            ("d_u16_write_mode", "enabled"),
            ("d_u16_write_allowlist", "D100-D105"),
        ][..],
    ] {
        let peer = Peer::start("R08CPU", 0x4801);
        let mut session = connect(&driver, &peer, settings).await;
        peer.clear();
        let results = session.write_batch(&[u16_write("@Arr[0]")]).await.unwrap();
        assert!(
            matches!(&results[..], [Err(ItemError::Unsupported(m))] if m == HUB_GATE),
            "{settings:?}: {results:?}"
        );
        assert!(peer.all().is_empty(), "{settings:?}: nothing sent");
        session.disconnect().await;
    }
}

/// Saved settings the guest refuses are refused by the Hub too (same keys),
/// and an enabled mode with only a label list is valid on both sides; the Q
/// test write still conflicts with it.
#[test]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #74 component"]
fn label_allowlist_settings_are_validated_by_guest_and_hub_alike() {
    let driver = driver();
    let errors = |pairs: &[(&str, &str)]| {
        let mut all = vec![("host", "127.0.0.1"), ("port", "5000")];
        all.extend_from_slice(pairs);
        let mut keys = driver
            .validate_config(&Settings::from_pairs(all).unwrap())
            .into_iter()
            .map(|error| error.key)
            .collect::<Vec<_>>();
        keys.sort();
        keys
    };
    assert!(errors(&[
        ("d_u16_write_mode", "enabled"),
        ("label_write_allowlist", "@A;@B[0-3];@タンク.水位"),
    ])
    .is_empty());
    assert!(errors(&[("label_write_allowlist", "@A")]).is_empty());
    let entries = |n: usize| {
        (0..n)
            .map(|i| format!("@L{i}"))
            .collect::<Vec<_>>()
            .join(";")
    };
    let long = format!("@{}", "A".repeat(2047));
    for ok in [entries(32), long.clone()] {
        assert!(
            errors(&[
                ("d_u16_write_mode", "enabled"),
                ("label_write_allowlist", ok.as_str())
            ])
            .is_empty(),
            "{ok}"
        );
    }
    // The guest and the Hub both report the same key.
    assert_eq!(
        errors(&[("d_u16_write_mode", "enabled")]),
        vec!["d_u16_write_allowlist", "d_u16_write_allowlist"]
    );
    let too_long = format!("{long}A");
    for bad in [
        "@A;",
        "@A;@A",
        "@A; @B",
        "@B[3-1]",
        "@B[*]",
        "B",
        "@Arr[03]",
        "@X.3",
        entries(33).as_str(),
        too_long.as_str(),
    ] {
        assert_eq!(
            errors(&[
                ("d_u16_write_mode", "enabled"),
                ("d_u16_write_allowlist", "D10-D10"),
                ("label_write_allowlist", bad),
            ]),
            vec!["label_write_allowlist", "label_write_allowlist"],
            "{bad}"
        );
    }
    let conflict = errors(&[
        ("d_u16_write_mode", "enabled"),
        ("label_write_allowlist", "@A"),
        ("q25prh_d3010_3013_test_write", "enabled"),
    ]);
    assert!(
        conflict.contains(&"d_u16_write_mode".to_string()),
        "{conflict:?}"
    );
}
