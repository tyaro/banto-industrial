//! Component-level checks of the SLMP guest's UDP transport (banto-driver-wasm
//! #62, stage E4) and D/W string tags (#63, stage C1) through this crate's
//! adapter and write gate (Hub side #933 / #934).
//!
//! Run with `BANTO_SLMP_NEW_COMPONENT_WASM=<built guest> cargo test -- --ignored`.
//! The UDP case drives the real engine (`FrameSession` over `TransportSpec::Udp`)
//! against a loopback UDP SLMP peer on a std thread, with the 4E and the 3E
//! frame. The string cases script the responses without a socket, so the exact
//! bytes the guest returns and sends are checked. Everything is a mock: no PLC.

use std::collections::BTreeMap;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use banto_driver_api::{
    ConnectContext, DataType, DriverBase, FrameConnection, FrameDriver, ItemError, RawValue,
    Session as _, Settings, StringEncoding, TagKind, TagSpec, Transport, Value, WriteItem,
    WriteProgress,
};
use banto_driver_engine::{transport::TransportSpec, FrameSession};
use banto_driver_wasm::WasmDriver;

/// Same id and `transports` (tcp and udp) as the Hub fixture manifest
/// `apps/banto-hub/host/tests/t12_slmp_wasm_operational_write/manifest.json`.
const MANIFEST: &[u8] = br#"{"manifestVersion":1,"id":"slmp-wasm-operational-write","version":"0.1.0","hubApi":"1","writeApi":"1","settingsVersion":1,"displayName":"SLMP UDP and string fixture","transports":["tcp","udp"],"capabilities":["read","write"]}"#;

fn component() -> Vec<u8> {
    std::fs::read(std::env::var("BANTO_SLMP_NEW_COMPONENT_WASM").expect("guest path")).unwrap()
}

/// D/W device codes (R series, 2-byte codes; low byte only here).
const D: u8 = 0xa8;
const W: u8 = 0xb4;
const SD: u8 = 0xa9;

/// One request as the peer saw it: subheader (0x54 4E / 0x50 3E), command,
/// subcommand, device, start, points.
type Seen = (u8, u16, u16, u8, u32, u16);

/// Loopback UDP SLMP peer: an R04CPU with SD280-283 = 4000/4000 and a word
/// memory for D and W. Answers each datagram with exactly one datagram in the
/// request's frame (4E echoes the serial). Other SD reads are refused (C051).
struct UdpPeer {
    address: SocketAddr,
    memory: Arc<Mutex<BTreeMap<(u8, u32), u16>>>,
    seen: Arc<Mutex<Vec<Seen>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl UdpPeer {
    fn start(words: &[(u8, u32, u16)]) -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket
            .set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        let address = socket.local_addr().unwrap();
        let memory = Arc::new(Mutex::new(
            words
                .iter()
                .map(|(device, index, value)| ((*device, *index), *value))
                .collect::<BTreeMap<_, _>>(),
        ));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (memory, seen, stop) = (memory.clone(), seen.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut buf = vec![0u8; 65536];
                while !stop.load(Ordering::SeqCst) {
                    let (len, from) = match socket.recv_from(&mut buf) {
                        Ok(pair) => pair,
                        Err(e)
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::WouldBlock
                                    | std::io::ErrorKind::TimedOut
                                    // Windows reports an ICMP for an earlier send here.
                                    | std::io::ErrorKind::ConnectionReset
                            ) =>
                        {
                            continue
                        }
                        Err(e) => panic!("peer recv: {e}"),
                    };
                    let response = answer(&buf[..len], &memory, &seen);
                    let _ = socket.send_to(&response, from);
                }
            })
        };
        Self {
            address,
            memory,
            seen,
            stop,
            thread: Some(thread),
        }
    }

    fn word(&self, device: u8, index: u32) -> Option<u16> {
        self.memory.lock().unwrap().get(&(device, index)).copied()
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

impl Drop for UdpPeer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Answer one 4E or 3E binary request (R series word subcommand 2 only).
fn answer(
    datagram: &[u8],
    memory: &Mutex<BTreeMap<(u8, u32), u16>>,
    seen: &Mutex<Vec<Seen>>,
) -> Vec<u8> {
    let sub = datagram[0];
    assert!(
        matches!(&datagram[..2], [0x54, 0] | [0x50, 0]),
        "{datagram:02x?}"
    );
    // Normalise 3E to the 4E layout (serial and reserved zero) like the
    // Hub's TCP fixture, so the offsets below are the 4E ones.
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
    let subcommand = u16::from_le_bytes([request[17], request[18]]);
    let (end_code, body): (u16, Vec<u8>) = if command == 0x0101 {
        seen.lock()
            .unwrap()
            .push((sub, command, subcommand, 0, 0, 0));
        let mut body = vec![b' '; 18];
        body[..6].copy_from_slice(b"R04CPU");
        body[16..18].copy_from_slice(&0x4800u16.to_le_bytes());
        (0, body)
    } else {
        assert_eq!(subcommand, 2, "R word access");
        let start = u32::from_le_bytes([request[19], request[20], request[21], request[22]]);
        let device = request[23];
        let points = u16::from_le_bytes([request[25], request[26]]);
        seen.lock()
            .unwrap()
            .push((sub, command, subcommand, device, start, points));
        match (command, device) {
            (0x0401, SD) if (start, points) == (280, 4) => {
                let mut body = 4000u32.to_le_bytes().to_vec();
                body.extend_from_slice(&4000u32.to_le_bytes());
                (0, body)
            }
            (0x0401, SD) => (0xc051, Vec::new()),
            (0x0401, D | W) => {
                let memory = memory.lock().unwrap();
                let body = (start..start + u32::from(points))
                    .flat_map(|index| {
                        memory
                            .get(&(device, index))
                            .copied()
                            .unwrap_or(0)
                            .to_le_bytes()
                    })
                    .collect();
                (0, body)
            }
            (0x1401, D | W) => {
                let data = &request[27..];
                assert_eq!(data.len(), usize::from(points) * 2);
                let mut memory = memory.lock().unwrap();
                for (index, word) in (start..).zip(data.as_chunks::<2>().0) {
                    memory.insert((device, index), u16::from_le_bytes(*word));
                }
                (0, Vec::new())
            }
            other => panic!("unexpected request {other:x?}"),
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

fn numeric(driver: &WasmDriver, address: &str, data_type: DataType) -> TagSpec {
    let kind = TagKind::numeric(data_type);
    TagSpec::new(driver.parse_address(address, &kind).unwrap(), kind)
}

fn string(driver: &WasmDriver, address: &str, words: u16) -> TagSpec {
    let kind = TagKind::string(words, StringEncoding::Utf8);
    TagSpec::new(driver.parse_address(address, &kind).unwrap(), kind)
}

/// #62 (E4) and #933: with `transport=udp` the guest reads D words, a u32 and
/// a D string, and writes one allowed D word, each request one datagram and
/// each answer one datagram, with the 4E and the 3E frame.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #63 component"]
async fn udp_transport_reads_and_writes_one_d_word_with_4e_and_3e() {
    let driver = WasmDriver::load_operational_write(MANIFEST, &component()).unwrap();
    assert_eq!(
        driver.descriptor().transports,
        vec![Transport::Tcp, Transport::Udp]
    );
    for (frame, subheader) in [("4e", 0x54), ("3e", 0x50)] {
        let peer = UdpPeer::start(&[
            (D, 100, 0x1234),
            (D, 101, 0x5678),
            (D, 102, 0x0001),
            (D, 200, u16::from_le_bytes(*b"AB")),
            (D, 201, u16::from_le_bytes(*b"C\0")),
        ]);
        let port = peer.address.port().to_string();
        let settings = Settings::from_pairs([
            ("host", "127.0.0.1"),
            ("port", port.as_str()),
            ("transport", "udp"),
            ("frame", frame),
            ("d_u16_write_mode", "enabled"),
            ("d_u16_write_allowlist", "D3010-D3013"),
        ])
        .unwrap();
        assert!(driver.validate_config(&settings).is_empty(), "{frame}");
        let mut session = FrameSession::connect(
            driver.as_ref(),
            &settings,
            &TransportSpec::Udp {
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
        .unwrap();
        let values = session
            .read_batch(&[
                numeric(&driver, "D100", DataType::U16),
                numeric(&driver, "D101", DataType::U32),
                string(&driver, "D200", 3),
            ])
            .await
            .unwrap();
        assert_eq!(
            values,
            vec![
                Ok(Value::F64(f64::from(0x1234u16))),
                Ok(Value::F64(f64::from(0x0001_5678u32))),
                Ok(Value::Str("ABC".into())),
            ],
            "{frame}"
        );
        let written = session
            .write_batch(&[WriteItem::new(
                numeric(&driver, "D3010", DataType::U16),
                Value::F64(77.0),
            )])
            .await
            .unwrap();
        assert_eq!(written, vec![Ok(())], "{frame}");
        assert_eq!(peer.word(D, 3010), Some(77), "{frame}");
        let seen = peer.seen();
        assert!(
            seen.iter().all(|(sub, ..)| *sub == subheader),
            "{frame}: {seen:x?}"
        );
        assert_eq!(seen[0].1, 0x0101, "handshake over UDP: {seen:x?}");
        assert_eq!(
            seen.iter()
                .filter(|(_, command, ..)| *command == 0x1401)
                .count(),
            1,
            "{frame}: {seen:x?}"
        );
        session.disconnect().await;
    }
}

/// #62 (E4): the guest accepts `transport` tcp and udp only.
#[test]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #63 component"]
fn transport_setting_accepts_tcp_and_udp_only() {
    let driver = WasmDriver::load_operational_write(MANIFEST, &component()).unwrap();
    let with = |transport: &str| {
        driver.validate_config(
            &Settings::from_pairs([
                ("host", "127.0.0.1"),
                ("port", "5000"),
                ("transport", transport),
            ])
            .unwrap(),
        )
    };
    assert!(with("tcp").is_empty());
    assert!(with("udp").is_empty());
    for bad in ["serial", "UDP", ""] {
        let errors = with(bad);
        assert!(
            errors.iter().any(|error| error.key == "transport"),
            "{bad:?}: {errors:?}"
        );
    }
}

fn command(request: &[u8]) -> (u16, u16) {
    (
        u16::from_le_bytes([request[15], request[16]]),
        u16::from_le_bytes([request[17], request[18]]),
    )
}

/// R-series (subcommand 2) word request: device number, device code, points.
fn r_words(request: &[u8]) -> (u32, u8, u16) {
    (
        u32::from_le_bytes([request[19], request[20], request[21], request[22]]),
        request[23],
        u16::from_le_bytes([request[25], request[26]]),
    )
}

/// A 4E response to `request` (serial and route echoed).
fn reply(request: &[u8], end_code: u16, body: &[u8]) -> Vec<u8> {
    assert_eq!(&request[..2], &[0x54, 0x00], "4E request");
    let mut response = vec![0xd4, 0x00, request[2], request[3], 0, 0];
    response.extend_from_slice(&request[6..11]);
    response.extend_from_slice(&u16::try_from(body.len() + 2).unwrap().to_le_bytes());
    response.extend_from_slice(&end_code.to_le_bytes());
    response.extend_from_slice(body);
    response
}

/// Load the guest, connect as an R04CPU with D/W counts of 4000 and the given
/// allowlist over a scripted (socketless) handshake.
fn connected(allowlist: &str) -> (Arc<WasmDriver>, Box<dyn FrameConnection>) {
    let driver = WasmDriver::load_operational_write(MANIFEST, &component()).unwrap();
    let settings = Settings::from_pairs([
        ("host", "127.0.0.1"),
        ("port", "5000"),
        ("d_u16_write_mode", "enabled"),
        ("d_u16_write_allowlist", allowlist),
    ])
    .unwrap();
    assert!(driver.validate_config(&settings).is_empty());
    let mut conn = driver.create_connection(&settings).unwrap();
    let mut response = None;
    while let Some(step) = conn.handshake_next(response.as_deref()).unwrap() {
        let request = step.send;
        response = Some(match command(&request) {
            (0x0101, _) => {
                let mut body = vec![b' '; 18];
                body[..6].copy_from_slice(b"R04CPU");
                body[16..18].copy_from_slice(&0x4800u16.to_le_bytes());
                reply(&request, 0, &body)
            }
            (0x0401, 2) => {
                assert_eq!(r_words(&request), (280, SD, 4), "SD280-283 only");
                let mut body = 4000u32.to_le_bytes().to_vec();
                body.extend_from_slice(&4000u32.to_le_bytes());
                reply(&request, 0, &body)
            }
            other => panic!("unexpected handshake request {other:x?}"),
        });
    }
    (driver, conn)
}

/// #63 (C1): a D or W string tag is one 0401 of its whole span, and the
/// result is the wire data unchanged (each word low byte first), exactly
/// `2 × words` bytes, not cut at NUL. 128 words is accepted, 129 and a word
/// bit are refused at registration.
#[test]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #63 component"]
fn d_and_w_strings_read_low_byte_first_with_exact_length() {
    let (driver, mut conn) = connected("D3010-D3013");
    for (address, canonical, device, start, words) in [
        ("D200", "D200", D, 200u32, 3u16),
        ("W1A", "W1A", W, 0x1a, 2),
        ("D300", "D300", D, 300, 128),
    ] {
        let spec = string(&driver, address, words);
        assert_eq!(spec.address.canonical, canonical);
        assert_eq!(spec.address.word_count, words);
        // Characters 'A'.. with a NUL in the middle and at the end.
        let wire: Vec<u8> = (0..usize::from(words) * 2)
            .map(|at| match at {
                1 => 0,
                at if at + 1 == usize::from(words) * 2 => 0,
                at => b'A' + u8::try_from(at % 26).unwrap(),
            })
            .collect();
        let planned = conn.plan_read(std::slice::from_ref(&spec)).unwrap();
        assert_eq!(planned.len(), 1, "{address}");
        assert_eq!(command(&planned[0].step.send), (0x0401, 2));
        assert_eq!(r_words(&planned[0].step.send), (start, device, words));
        let decoded = conn
            .decode_read(
                &planned[0],
                std::slice::from_ref(&spec),
                &reply(&planned[0].step.send, 0, &wire),
            )
            .unwrap();
        assert_eq!(decoded, vec![Ok(RawValue::Bytes(wire))], "{address}");
    }
    let kind = |words| TagKind::string(words, StringEncoding::Utf8);
    for (address, words) in [("D300", 129), ("D300", 0), ("D100.5", 2), ("M10", 2)] {
        assert!(
            driver.parse_address(address, &kind(words)).is_err(),
            "{address} x {words} accepted"
        );
    }
}

/// #63 (C1) and #934: a D/W string write is one 1401 over the whole span with
/// the Hub's encoded, 0x00-padded bytes, each word low byte first. Before
/// anything is sent, the Hub gate refuses a span reaching outside the
/// allowlist, a string with another item, and text longer than the span.
#[test]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #63 component"]
fn d_and_w_string_write_is_one_1401_of_the_padded_span_and_alone() {
    let (driver, mut conn) = connected("D3010-D3013,W1A-W1F");
    for (address, device, start, words, text, wire) in [
        ("D3010", D, 3010u32, 4u16, "ABC", b"ABC\0\0\0\0\0".to_vec()),
        ("W1C", W, 0x1c, 2, "WXYZ", b"WXYZ".to_vec()),
    ] {
        let item = WriteItem::new(string(&driver, address, words), Value::Str(text.into()));
        let plan = match conn.write_next(std::slice::from_ref(&item), None).unwrap() {
            WriteProgress::Send(plan) => plan,
            other => panic!("expected one 1401 for {address}: {other:?}"),
        };
        assert_eq!(command(&plan.step.send), (0x1401, 2));
        assert_eq!(r_words(&plan.step.send), (start, device, words));
        assert_eq!(&plan.step.send[27..], wire.as_slice(), "{address}");
        assert_eq!(
            conn.write_next(&[item], Some(&reply(&plan.step.send, 0, &[])))
                .unwrap(),
            WriteProgress::Done(vec![Ok(())])
        );
    }
    let refused = |conn: &mut Box<dyn FrameConnection>, items: Vec<WriteItem>| match conn
        .write_next(&items, None)
        .unwrap()
    {
        WriteProgress::Done(results) => {
            assert_eq!(results.len(), items.len());
            assert!(
                results
                    .iter()
                    .all(|result| matches!(result, Err(ItemError::Unsupported(_)))),
                "Hub gate refusal expected: {results:?}"
            );
        }
        other => panic!("a write was planned for {items:?}: {other:?}"),
    };
    let text = |address: &str, words: u16, text: &str| {
        WriteItem::new(string(&driver, address, words), Value::Str(text.into()))
    };
    // D3012-D3015 reaches past D3013.
    refused(&mut conn, vec![text("D3012", 4, "AB")]);
    // A string must be written alone.
    refused(
        &mut conn,
        vec![
            text("D3010", 2, "AB"),
            WriteItem::new(numeric(&driver, "D3013", DataType::U16), Value::F64(1.0)),
        ],
    );
    // Nine bytes do not fit four words; never truncated.
    refused(&mut conn, vec![text("D3010", 4, "ABCDEFGHI")]);
}
