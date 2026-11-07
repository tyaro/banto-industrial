//! Component-level checks of the SLMP guest's H clock set (`clock-v1`,
//! banto-driver-wasm #76) through this crate's adapter and the real engine
//! (Hub side acceptance, mock only).
//!
//! Run with `BANTO_SLMP_NEW_COMPONENT_WASM=<built guest> cargo test -- --ignored`.
//! The Host binds the small world `driver-with-clock-v1` and offers the clock
//! set only on a component loaded through the operational write loader. The
//! guest gate is the connection setting `clock_set` (default disabled). On a
//! 0101-confirmed iQ-R (own station only) the guest runs four steps: 0401 bit
//! read of SM210-SM211 (SM210 ON refuses), 1401 word write of SD210-SD216
//! (year, month, day, hour, minute, second, weekday 0 = Sunday), 1401 bit
//! write of SM210 ON, then up to three 0401 reads of SM210-SM211 (SM211 ON is
//! `device` 211, both OFF is success, SM210 still ON after the third read is
//! `verification-failed`). A PLC end code at any step is `device` and nothing
//! more is sent; a malformed response is fatal. Standard Q and every other
//! model are `unsupported` before anything is sent. The expected request
//! bytes are the guest's own (`slmp-runtime/tests/clock.rs`, from the SLMP
//! manual's iQ-R device specification).
//!
//! The previous pin `fc0bd10` has no `clock-v1` export and no `clock_set`
//! setting, so every test below fails there (checked 2026-11-07: the guest
//! refuses `clock_set` as an unsupported setting before any clock request).
//! The tests drive the real engine (`FrameSession` over TCP) against a
//! loopback SLMP peer on a std thread that records every request byte for
//! byte. The peer is a mock; no PLC is involved. Whether a real CPU accepts a
//! clock set in STOP, turns SM211 OFF after a later success, and how a
//! standard Q's SM210 behaves are not confirmed (guest PLAN.md "H の残件").

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use banto_driver_api::{
    CivilTime, ConnectContext, DataType, DriverBase, ItemError, Session as _, Settings, TagKind,
    TagSpec, Value,
};
use banto_driver_engine::{transport::TransportSpec, FrameSession};
use banto_driver_wasm::WasmDriver;

/// Same id as the Hub fixture manifest
/// `apps/banto-hub/host/tests/t12_slmp_wasm_operational_write/manifest.json`.
const MANIFEST: &[u8] = br#"{"manifestVersion":1,"id":"slmp-wasm-operational-write","version":"0.1.0","hubApi":"1","writeApi":"1","settingsVersion":1,"displayName":"SLMP H fixture","transports":["tcp","udp"],"capabilities":["read","write"]}"#;
/// A read-only manifest for the normal loader.
const READ_MANIFEST: &[u8] = br#"{"manifestVersion":1,"id":"slmp-read-wasm","version":"0.1.0","hubApi":"1","settingsVersion":1,"displayName":"SLMP H read-only fixture","transports":["tcp"],"capabilities":["read"]}"#;

fn component() -> Vec<u8> {
    std::fs::read(std::env::var("BANTO_SLMP_NEW_COMPONENT_WASM").expect("guest path")).unwrap()
}

const D: u8 = 0xa8;
const SD: u8 = 0xa9;
const SM: u8 = 0x91;
const END_CODE_C051: u16 = 0xc051;
const END_CODE_C059: u16 = 0xc059;

const OWN: [u8; 5] = [0x00, 0xff, 0xff, 0x03, 0x00];
const ENABLED: (&str, &str) = ("clock_set", "enabled");

/// 2026-11-07 (a Saturday) 09:05:30.
const TIME: CivilTime = CivilTime::new(2026, 11, 7, 9, 5, 30);

/// Command onward: 0401 subcommand 0003 (bit, iQ-R device specification) of
/// SM210, two points.
const STATUS_BODY: [u8; 12] = [1, 4, 3, 0, 0xd2, 0, 0, 0, SM, 0, 2, 0];
/// 1401 subcommand 0002 (word) of SD210-SD216: 2026, 11, 7, 9, 5, 30, 6.
const DATA_BODY: [u8; 26] = [
    1, 0x14, 2, 0, 0xd2, 0, 0, 0, SD, 0, 7, 0, 0xea, 0x07, 11, 0, 7, 0, 9, 0, 5, 0, 30, 0, 6, 0,
];
/// 1401 subcommand 0003 (bit) of SM210 ON.
const SET_BODY: [u8; 13] = [1, 0x14, 3, 0, 0xd2, 0, 0, 0, SM, 0, 1, 0, 0x10];

/// How the peer answers the clock requests (0401 of SM, 1401), counted from 1.
#[derive(Default)]
struct Script {
    /// SM210/SM211 for each status read in turn; both OFF when exhausted.
    statuses: VecDeque<(bool, bool)>,
    /// Answer the n-th clock request with this end code.
    end_code: Option<(usize, u16)>,
    /// Answer the n-th clock request with an empty body (end code 0).
    short: Option<usize>,
}

/// What the peer saw. Frames are whole requests with the 4E serial zeroed.
#[derive(Default)]
struct Seen {
    all: Vec<Vec<u8>>,
    clock_requests: usize,
    connections: u32,
}

struct Peer {
    address: SocketAddr,
    seen: Arc<Mutex<Seen>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Peer {
    /// A mock PLC answering 0101 with `name`/`code` and following the frame
    /// it receives (3E `50 00` or 4E `54 00`). It answers the iQ-R handshake
    /// (SD280-283) and the Q one (SD302-303), D100 = 7, and the clock
    /// requests by `script`.
    fn start(name: &'static str, code: u16, script: Script) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Seen::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (seen, stop) = (seen.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut script = script;
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
                        let (end_code, body) = answer(name, code, &request, &seen, &mut script);
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

    fn connections(&self) -> u32 {
        self.seen.lock().unwrap().connections
    }

    fn clear(&self) {
        let mut seen = self.seen.lock().unwrap();
        seen.all.clear();
        seen.clock_requests = 0;
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

fn answer(
    name: &str,
    code: u16,
    request: &[u8],
    seen: &Mutex<Seen>,
    script: &mut Script,
) -> (u16, Vec<u8>) {
    let mut recorded = request.to_vec();
    if recorded[0] == 0x54 {
        recorded[2] = 0;
        recorded[3] = 0;
    }
    seen.lock().unwrap().all.push(recorded);
    let at = if request[0] == 0x50 { 11 } else { 15 };
    let command = u16::from_le_bytes([request[at], request[at + 1]]);
    let sub = u16::from_le_bytes([request[at + 2], request[at + 3]]);
    let data = &request[at + 4..];
    if command == 0x0101 {
        let mut body = vec![b' '; 18];
        body[..name.len()].copy_from_slice(name.as_bytes());
        body[16..18].copy_from_slice(&code.to_le_bytes());
        return (0, body);
    }
    let clock = command == 0x1401 || (command == 0x0401 && sub == 3);
    if clock {
        let n = {
            let mut seen = seen.lock().unwrap();
            seen.clock_requests += 1;
            seen.clock_requests
        };
        if let Some((at, end_code)) = script.end_code {
            if at == n {
                return (end_code, Vec::new());
            }
        }
        if script.short == Some(n) {
            return (0, Vec::new());
        }
        return match command {
            0x1401 => (0, Vec::new()),
            _ => {
                assert_eq!(data[4], SM, "only SM is read as bits");
                let (sm210, sm211) = script.statuses.pop_front().unwrap_or((false, false));
                (0, vec![(u8::from(sm210) << 4) | u8::from(sm211)])
            }
        };
    }
    assert_eq!(command, 0x0401, "unexpected command {command:#x}");
    // Q wire (sub 0: 3-byte number, code, points) or R wire (sub 2: 4-byte
    // number, 2-byte code, points).
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
    match (device, start, points) {
        (SD, 280, 4) => {
            let mut body = 8000u32.to_le_bytes().to_vec();
            body.extend_from_slice(&0x200u32.to_le_bytes());
            (0, body)
        }
        (SD, 302, 2) => {
            let mut body = 4000u16.to_le_bytes().to_vec();
            body.extend_from_slice(&64u16.to_le_bytes());
            (0, body)
        }
        (D, _, _) => (
            0,
            (0..u32::from(points))
                .flat_map(|at| if start + at == 100 { 7u16 } else { 0 }.to_le_bytes())
                .collect(),
        ),
        _ => (END_CODE_C051, Vec::new()),
    }
}

/// A 4E request (serial zeroed) with the own-station route and timer 0010H.
fn frame_4e(data: &[u8]) -> Vec<u8> {
    let mut frame = vec![0x54, 0x00, 0x00, 0x00, 0x00, 0x00];
    frame.extend_from_slice(&OWN);
    frame.extend_from_slice(&u16::try_from(data.len() + 2).unwrap().to_le_bytes());
    frame.extend_from_slice(&0x0010u16.to_le_bytes());
    frame.extend_from_slice(data);
    frame
}

/// The same request as 3E.
fn frame_3e(data: &[u8]) -> Vec<u8> {
    let mut frame = vec![0x50, 0x00];
    frame.extend_from_slice(&OWN);
    frame.extend_from_slice(&u16::try_from(data.len() + 2).unwrap().to_le_bytes());
    frame.extend_from_slice(&0x0010u16.to_le_bytes());
    frame.extend_from_slice(data);
    frame
}

/// The handshake of an iQ-R: 0101, then SD280-283 (R wire).
fn r_handshake(frame: fn(&[u8]) -> Vec<u8>) -> Vec<Vec<u8>> {
    let mut sd = vec![0x01, 0x04, 0x02, 0x00];
    sd.extend_from_slice(&280u32.to_le_bytes());
    sd.extend_from_slice(&[SD, 0x00, 0x04, 0x00]);
    vec![frame(&[0x01, 0x01, 0x00, 0x00]), frame(&sd)]
}

/// A full successful run: status, data, set, status.
fn success_frames(frame: fn(&[u8]) -> Vec<u8>) -> Vec<Vec<u8>> {
    vec![
        frame(&STATUS_BODY),
        frame(&DATA_BODY),
        frame(&SET_BODY),
        frame(&STATUS_BODY),
    ]
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

/// R08CPU over 4E and 3E with `clock_set=enabled`: exactly the four guest
/// requests byte for byte, success, and the same session then reads D100 on
/// the same connection (no leftover clock state).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #76 component"]
async fn standard_r_sets_the_clock_with_four_exact_requests() {
    let driver = driver();
    for (frame, extra) in [
        (frame_4e as fn(&[u8]) -> Vec<u8>, &[ENABLED][..]),
        (frame_3e, &[ENABLED, ("frame", "3e")][..]),
    ] {
        let peer = Peer::start("R08CPU", 0x4801, Script::default());
        let mut session = connect(&driver, &peer, extra).await;
        assert!(session.connection().unwrap().supports_clock());
        assert_eq!(peer.all(), r_handshake(frame));
        peer.clear();
        assert_eq!(session.set_clock(TIME).await, Ok(Some(Ok(()))));
        assert_eq!(peer.all(), success_frames(frame));
        assert!(session.is_connected());

        peer.clear();
        let mut d_read = vec![0x01, 0x04, 0x02, 0x00];
        d_read.extend_from_slice(&100u32.to_le_bytes());
        d_read.extend_from_slice(&[D, 0x00, 0x01, 0x00]);
        assert_eq!(
            session
                .read_batch(&[tag(&driver, "D100", DataType::U16)])
                .await
                .unwrap(),
            vec![Ok(Value::F64(7.0))]
        );
        assert_eq!(peer.all(), vec![frame(&d_read)]);
        // A second run starts again from the status read.
        peer.clear();
        assert_eq!(session.set_clock(TIME).await, Ok(Some(Ok(()))));
        assert_eq!(peer.all(), success_frames(frame));
        assert_eq!(peer.connections(), 1, "connection kept");
        session.disconnect().await;
    }
}

/// SM210 already ON refuses after the one status read; SM211 ON at the
/// confirm read is `device` 211; SM210 ON at three confirm reads is
/// `verification-failed`. Each keeps the connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #76 component"]
async fn sm210_and_sm211_outcomes_are_item_errors_and_keep_the_connection() {
    let driver = driver();

    let peer = Peer::start(
        "R08CPU",
        0x4801,
        Script {
            statuses: [(true, false)].into(),
            ..Script::default()
        },
    );
    let mut session = connect(&driver, &peer, &[ENABLED]).await;
    peer.clear();
    let result = session.set_clock(TIME).await;
    assert!(
        matches!(&result, Ok(Some(Err(ItemError::Rejected(m)))) if m.contains("SM210 ON")),
        "{result:?}"
    );
    assert_eq!(peer.all(), vec![frame_4e(&STATUS_BODY)], "only the read");
    assert!(session.is_connected());
    session.disconnect().await;

    let peer = Peer::start(
        "R08CPU",
        0x4801,
        Script {
            statuses: [(false, false), (false, true)].into(),
            ..Script::default()
        },
    );
    let mut session = connect(&driver, &peer, &[ENABLED]).await;
    peer.clear();
    let result = session.set_clock(TIME).await;
    assert!(
        matches!(&result, Ok(Some(Err(ItemError::Device(e)))) if e.code == 211 && e.message.contains("SM211")),
        "{result:?}"
    );
    assert_eq!(peer.all(), success_frames(frame_4e));
    assert!(session.is_connected());
    session.disconnect().await;

    let peer = Peer::start(
        "R08CPU",
        0x4801,
        Script {
            statuses: [(false, false), (true, false), (true, false), (true, false)].into(),
            ..Script::default()
        },
    );
    let mut session = connect(&driver, &peer, &[ENABLED]).await;
    peer.clear();
    let result = session.set_clock(TIME).await;
    assert!(
        matches!(&result, Ok(Some(Err(ItemError::VerificationFailed(m)))) if m.contains("3 reads")),
        "{result:?}"
    );
    let mut expected = success_frames(frame_4e);
    expected.extend([frame_4e(&STATUS_BODY), frame_4e(&STATUS_BODY)]);
    assert_eq!(peer.all(), expected, "three confirm reads, nothing else");
    assert!(session.is_connected());
    assert_eq!(peer.connections(), 1);
    session.disconnect().await;
}

/// A PLC end code at step 2 (the SD210-SD216 write) is `device` with that
/// code, and nothing more is sent; the connection is kept.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #76 component"]
async fn an_end_code_is_a_device_error_and_stops_sending() {
    let driver = driver();
    let peer = Peer::start(
        "R08CPU",
        0x4801,
        Script {
            end_code: Some((2, END_CODE_C059)),
            ..Script::default()
        },
    );
    let mut session = connect(&driver, &peer, &[ENABLED]).await;
    peer.clear();
    let result = session.set_clock(TIME).await;
    assert!(
        matches!(&result, Ok(Some(Err(ItemError::Device(e)))) if e.code == u32::from(END_CODE_C059)),
        "{result:?}"
    );
    assert_eq!(
        peer.all(),
        vec![frame_4e(&STATUS_BODY), frame_4e(&DATA_BODY)]
    );
    assert!(session.is_connected());
    session.disconnect().await;
}

/// `clock_set` unset or disabled is `rejected`, and standard Q (3E) is
/// `unsupported`, both before anything is sent after the handshake.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #76 component"]
async fn the_guest_gate_and_standard_q_send_nothing() {
    let driver = driver();
    for extra in [&[][..], &[("clock_set", "disabled")][..]] {
        let peer = Peer::start("R08CPU", 0x4801, Script::default());
        let mut session = connect(&driver, &peer, extra).await;
        peer.clear();
        let result = session.set_clock(TIME).await;
        assert!(
            matches!(&result, Ok(Some(Err(ItemError::Rejected(m)))) if m.contains("clock_set")),
            "{extra:?}: {result:?}"
        );
        assert!(peer.all().is_empty(), "{extra:?}: nothing sent");
        assert!(session.is_connected());
        session.disconnect().await;
    }

    let peer = Peer::start("Q06HCPU", 0x0042, Script::default());
    let mut session = connect(&driver, &peer, &[ENABLED, ("frame", "3e")]).await;
    peer.clear();
    let result = session.set_clock(TIME).await;
    assert!(
        matches!(&result, Ok(Some(Err(ItemError::Unsupported(_))))),
        "{result:?}"
    );
    assert!(peer.all().is_empty(), "nothing sent on Q");
    assert!(session.is_connected());
    session.disconnect().await;
}

/// A short status response at step 1 is fatal: `Err`, and the engine closes
/// the connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #76 component"]
async fn a_short_response_is_fatal_and_closes_the_connection() {
    let driver = driver();
    let peer = Peer::start(
        "R08CPU",
        0x4801,
        Script {
            short: Some(1),
            ..Script::default()
        },
    );
    let mut session = connect(&driver, &peer, &[ENABLED]).await;
    peer.clear();
    let result = session.set_clock(TIME).await;
    assert!(result.is_err(), "{result:?}");
    assert!(!session.is_connected());
    assert_eq!(peer.all(), vec![frame_4e(&STATUS_BODY)]);
    assert!(session.set_clock(TIME).await.is_err(), "not reused");
}

/// The same component through the read-only loader never offers the clock
/// set: `Ok(None)` and nothing is sent, even with `clock_set=enabled`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #76 component"]
async fn the_read_only_loader_does_not_offer_the_clock_set() {
    let driver = WasmDriver::load(READ_MANIFEST, &component()).unwrap();
    let peer = Peer::start("R08CPU", 0x4801, Script::default());
    let mut session = connect(&driver, &peer, &[ENABLED]).await;
    assert!(!session.connection().unwrap().supports_clock());
    peer.clear();
    assert_eq!(session.set_clock(TIME).await, Ok(None));
    assert!(peer.all().is_empty());
    assert!(session.is_connected());
    assert!(session
        .connection_mut()
        .unwrap()
        .set_clock_next(&TIME, None)
        .is_err());
    assert!(peer.all().is_empty());
    session.disconnect().await;
}
