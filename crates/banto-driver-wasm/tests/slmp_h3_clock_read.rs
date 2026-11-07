//! Component-level checks of the SLMP guest's H3 clock read
//! (`clock-read-v1`, banto-driver-wasm #78) through this crate's adapter and
//! the real engine (Hub side acceptance, mock only).
//!
//! Run with `BANTO_SLMP_NEW_COMPONENT_WASM=<built guest> cargo test -- --ignored`.
//! The Host binds the small world `driver-with-clock-read-v1` and offers the
//! clock read whatever the loader: it writes nothing to the PLC (guest design
//! option 1). On a 0101-confirmed iQ-R (G2b included) or standard Q (QnPH,
//! QnUDPV and L included), own station only, the guest runs two steps: a
//! 0401 bit read of SM210-SM213 (SM213 OFF or SM210 ON is `rejected`), then
//! one 0401 word read of SD210-SD216 (iQ-R, BIN) or SD210-SD213 (Q, BCD).
//! A value that is not a clock (a date outside the calendar, a digit that is
//! not BCD) is `verification-failed`; a PLC end code at either step is
//! `device` and nothing more is sent; a malformed response is fatal.
//! Q25PRH/Q12PRH, FX5 and unknown models are `unsupported` before anything is
//! sent. The guest has no setting for it (`clock_set` is not looked at). The
//! expected request bytes are the guest's own
//! (`slmp-runtime/tests/clock_read.rs`, from the SLMP manual's iQ-R and Q
//! device specifications).
//!
//! The previous pin `ea4d84c` has no `clock-read-v1` export, so every test
//! below fails there (`supports_clock_read` is false and the read is
//! `Ok(None)`); the older pinned guests are checked by `diagnostics_v1.rs`.
//! The tests drive the real engine (`FrameSession` over TCP) against a
//! loopback SLMP peer on a std thread that records every request byte for
//! byte. The peer is a mock; no PLC is involved. Whether a real CPU updates
//! SD210 onward without SM213 and how often while SM213 is ON are not
//! confirmed (guest PLAN.md "H3 の残件").

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
const MANIFEST: &[u8] = br#"{"manifestVersion":1,"id":"slmp-wasm-operational-write","version":"0.1.0","hubApi":"1","writeApi":"1","settingsVersion":1,"displayName":"SLMP H3 fixture","transports":["tcp","udp"],"capabilities":["read","write"]}"#;
/// A read-only manifest for the normal loader.
const READ_MANIFEST: &[u8] = br#"{"manifestVersion":1,"id":"slmp-read-wasm","version":"0.1.0","hubApi":"1","settingsVersion":1,"displayName":"SLMP H3 read-only fixture","transports":["tcp"],"capabilities":["read"]}"#;

fn component() -> Vec<u8> {
    std::fs::read(std::env::var("BANTO_SLMP_NEW_COMPONENT_WASM").expect("guest path")).unwrap()
}

const D: u8 = 0xa8;
const SD: u8 = 0xa9;
const SM: u8 = 0x91;
const END_CODE_C051: u16 = 0xc051;
const END_CODE_C059: u16 = 0xc059;

const OWN: [u8; 5] = [0x00, 0xff, 0xff, 0x03, 0x00];

const R: (&str, u16) = ("R04CPU", 0x4800);
const Q: (&str, u16) = ("Q02UCPU", 0x0263);

/// 2026-11-07 13:04:59 (a Saturday): BIN (iQ-R, SD216 = 6) and BCD (Q).
const R_WORDS: [u16; 7] = [2026, 11, 7, 13, 4, 59, 6];
const Q_WORDS: [u16; 4] = [0x2611, 0x0713, 0x0459, 0x2006];
const EXPECTED: CivilTime = CivilTime::new(2026, 11, 7, 13, 4, 59);

/// Command onward: 0401 subcommand 0003 (bit, iQ-R device specification) of
/// SM210, four points; then 0401 subcommand 0002 (word) of SD210, seven.
const R_STATUS_BODY: [u8; 12] = [1, 4, 3, 0, 0xd2, 0, 0, 0, SM, 0, 4, 0];
const R_DATA_BODY: [u8; 12] = [1, 4, 2, 0, 0xd2, 0, 0, 0, SD, 0, 7, 0];
/// The same on the Q device specification (subcommands 0001 and 0000).
const Q_STATUS_BODY: [u8; 10] = [1, 4, 1, 0, 0xd2, 0, 0, SM, 4, 0];
const Q_DATA_BODY: [u8; 10] = [1, 4, 0, 0, 0xd2, 0, 0, SD, 4, 0];

/// SM210-SM213 as two bytes (two points per byte, high nibble first).
fn status(sm210: bool, sm211: bool, sm212: bool, sm213: bool) -> [u8; 2] {
    [
        (u8::from(sm210) << 4) | u8::from(sm211),
        (u8::from(sm212) << 4) | u8::from(sm213),
    ]
}

/// SM213 ON, the others OFF.
const READY: [u8; 2] = [0x00, 0x01];

/// How the peer answers the clock requests (the 0401 of SM210 and of SD210),
/// counted from 1.
#[derive(Default)]
struct Script {
    /// SM210-SM213 for each status read in turn; [`READY`] when exhausted.
    statuses: VecDeque<[u8; 2]>,
    /// SD210 onward for the data read; the model's 2026-11-07 13:04:59 when
    /// `None` ([`R_WORDS`] on the iQ-R wire, [`Q_WORDS`] on the Q wire).
    words: Option<Vec<u16>>,
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
    /// (SD280-283) and the Q one (SD302-303), other word reads of the
    /// handshake with zeros, D100 = 7, and the clock requests by `script`.
    /// Any write (1401) is refused with C059 and shows in [`Peer::all`].
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
    if command != 0x0401 {
        // A clock read writes nothing; tests compare `all()` frame by frame.
        return (END_CODE_C059, Vec::new());
    }
    // Q wire (sub 0/1: 3-byte number, code, points) or R wire (sub 2/3:
    // 4-byte number, 2-byte code, points).
    let (start, device, points, r_wire) = match sub {
        0 | 1 => (
            u32::from_le_bytes([data[0], data[1], data[2], 0]),
            data[3],
            u16::from_le_bytes([data[4], data[5]]),
            false,
        ),
        2 | 3 => (
            u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
            data[4],
            u16::from_le_bytes([data[6], data[7]]),
            true,
        ),
        other => panic!("unexpected subcommand {other:#x}"),
    };
    if start == 210 && (device == SM || device == SD) {
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
        if device == SM {
            assert_eq!(sub & 1, 1, "SM210 onward is read as bits");
            assert_eq!(points, 4);
            return (0, script.statuses.pop_front().unwrap_or(READY).to_vec());
        }
        assert_eq!(sub & 1, 0, "SD210 onward is read as words");
        let words = script.words.clone().unwrap_or_else(|| {
            if r_wire {
                R_WORDS.to_vec()
            } else {
                Q_WORDS.to_vec()
            }
        });
        return (0, words.iter().flat_map(|w| w.to_le_bytes()).collect());
    }
    if sub & 1 == 1 {
        return (END_CODE_C051, Vec::new());
    }
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
        _ => (0, vec![0; usize::from(points) * 2]),
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

type Frame = fn(&[u8]) -> Vec<u8>;

/// The two requests of a full run on the iQ-R or Q wire.
fn r_frames(frame: Frame) -> Vec<Vec<u8>> {
    vec![frame(&R_STATUS_BODY), frame(&R_DATA_BODY)]
}

fn q_frames(frame: Frame) -> Vec<Vec<u8>> {
    vec![frame(&Q_STATUS_BODY), frame(&Q_DATA_BODY)]
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

/// R04CPU over 4E and 3E: exactly the two guest requests byte for byte, the
/// BIN clock, and the same session then reads D100 on the same connection;
/// a second run starts again from the status read. Nothing is written and
/// the guest's `clock_set` gate (unset here) is not needed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #78 component"]
async fn standard_r_reads_the_clock_with_two_exact_requests() {
    let driver = driver();
    for (frame, extra) in [
        (frame_4e as Frame, &[][..]),
        (frame_3e, &[("frame", "3e")][..]),
    ] {
        let peer = Peer::start(R.0, R.1, Script::default());
        let mut session = connect(&driver, &peer, extra).await;
        assert!(session.connection().unwrap().supports_clock_read());
        peer.clear();
        assert_eq!(session.read_clock().await, Ok(Some(Ok(EXPECTED))));
        assert_eq!(peer.all(), r_frames(frame));
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
        peer.clear();
        assert_eq!(session.read_clock().await, Ok(Some(Ok(EXPECTED))));
        assert_eq!(peer.all(), r_frames(frame));
        assert_eq!(peer.connections(), 1, "connection kept");
        session.disconnect().await;
    }
}

/// Q02UCPU over 4E and 3E: the two requests on the Q device specification
/// and the BCD clock, also the QCPU manual's example (H9307, H3110, H3548,
/// H1905 = 1993-07-31 10:35:48).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #78 component"]
async fn standard_q_reads_the_bcd_clock_with_two_exact_requests() {
    let driver = driver();
    for (frame, extra) in [
        (frame_4e as Frame, &[][..]),
        (frame_3e, &[("frame", "3e")][..]),
    ] {
        let peer = Peer::start(Q.0, Q.1, Script::default());
        let mut session = connect(&driver, &peer, extra).await;
        assert!(session.connection().unwrap().supports_clock_read());
        peer.clear();
        assert_eq!(session.read_clock().await, Ok(Some(Ok(EXPECTED))));
        assert_eq!(peer.all(), q_frames(frame));
        assert!(session.is_connected());
        session.disconnect().await;
    }

    let peer = Peer::start(
        Q.0,
        Q.1,
        Script {
            words: Some(vec![0x9307, 0x3110, 0x3548, 0x1905]),
            ..Script::default()
        },
    );
    let mut session = connect(&driver, &peer, &[]).await;
    peer.clear();
    assert_eq!(
        session.read_clock().await,
        Ok(Some(Ok(CivilTime::new(1993, 7, 31, 10, 35, 48))))
    );
    assert_eq!(peer.all(), q_frames(frame_4e));
    session.disconnect().await;
}

/// SM213 OFF (the SD words are not the clock) or SM210 ON (a clock set is
/// pending) is `rejected` after the one status read, on R and Q; the
/// connection is kept.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #78 component"]
async fn sm213_off_or_sm210_on_is_rejected_after_the_status_read() {
    let driver = driver();
    for (model, status_body, range) in [
        (R, &R_STATUS_BODY[..], "SD210-SD216"),
        (Q, &Q_STATUS_BODY[..], "SD210-SD213"),
    ] {
        for (bits, needle) in [
            (status(false, false, false, false), "SM213 is OFF"),
            (status(true, false, false, true), "SM210 ON"),
        ] {
            let peer = Peer::start(
                model.0,
                model.1,
                Script {
                    statuses: [bits].into(),
                    ..Script::default()
                },
            );
            let mut session = connect(&driver, &peer, &[]).await;
            peer.clear();
            let result = session.read_clock().await;
            assert!(
                matches!(&result, Ok(Some(Err(ItemError::Rejected(m)))) if m.contains(needle) && m.contains(range)),
                "{}: {result:?}",
                model.0
            );
            assert_eq!(peer.all(), vec![frame_4e(status_body)], "only the status");
            assert!(session.is_connected());
            session.disconnect().await;
        }
    }
}

/// Values that are not a clock are `verification-failed`: a Q digit that is
/// not BCD, an iQ-R date outside the calendar. Both requests were sent and
/// the connection is kept.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #78 component"]
async fn values_that_are_not_a_clock_are_verification_failed() {
    let driver = driver();
    let mut feb_30 = R_WORDS;
    feb_30[1] = 2;
    feb_30[2] = 30;
    for (model, words, needle, frames) in [
        (
            Q,
            vec![0x2a11, 0x0713, 0x0459, 0x2006],
            "SD210 is not BCD",
            q_frames(frame_4e),
        ),
        (R, feb_30.to_vec(), "day 30", r_frames(frame_4e)),
    ] {
        let peer = Peer::start(
            model.0,
            model.1,
            Script {
                words: Some(words),
                ..Script::default()
            },
        );
        let mut session = connect(&driver, &peer, &[]).await;
        peer.clear();
        let result = session.read_clock().await;
        assert!(
            matches!(&result, Ok(Some(Err(ItemError::VerificationFailed(m)))) if m.contains(needle)),
            "{}: {result:?}",
            model.0
        );
        assert_eq!(peer.all(), frames);
        assert!(session.is_connected());
        session.disconnect().await;
    }
}

/// A PLC end code at step 1 or step 2 is `device` with that code, and
/// nothing more is sent; the connection is kept.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #78 component"]
async fn an_end_code_at_either_step_is_a_device_error_and_stops_sending() {
    let driver = driver();
    for failing in [1, 2] {
        let peer = Peer::start(
            R.0,
            R.1,
            Script {
                end_code: Some((failing, END_CODE_C059)),
                ..Script::default()
            },
        );
        let mut session = connect(&driver, &peer, &[]).await;
        peer.clear();
        let result = session.read_clock().await;
        assert!(
            matches!(&result, Ok(Some(Err(ItemError::Device(e)))) if e.code == u32::from(END_CODE_C059)),
            "step {failing}: {result:?}"
        );
        assert_eq!(peer.all(), r_frames(frame_4e)[..failing].to_vec());
        assert!(session.is_connected());
        session.disconnect().await;
    }
}

/// Q25PRH (redundant) and FX5 are `unsupported` before anything is sent
/// after the handshake; the connection is kept.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #78 component"]
async fn unsupported_models_send_nothing() {
    let driver = driver();
    for (name, code, extra) in [
        ("Q25PRHCPU", 0x004c, &[][..]),
        ("FX5U-32MR/ES", 0x4a21, &[("frame", "3e")][..]),
    ] {
        let peer = Peer::start(name, code, Script::default());
        let mut session = connect(&driver, &peer, extra).await;
        peer.clear();
        let result = session.read_clock().await;
        assert!(
            matches!(&result, Ok(Some(Err(ItemError::Unsupported(_))))),
            "{name}: {result:?}"
        );
        assert!(peer.all().is_empty(), "{name}: nothing sent");
        assert!(session.is_connected());
        session.disconnect().await;
    }
}

/// A short status response at step 1 is fatal: `Err`, and the engine closes
/// the connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #78 component"]
async fn a_short_response_is_fatal_and_closes_the_connection() {
    let driver = driver();
    let peer = Peer::start(
        R.0,
        R.1,
        Script {
            short: Some(1),
            ..Script::default()
        },
    );
    let mut session = connect(&driver, &peer, &[]).await;
    peer.clear();
    let result = session.read_clock().await;
    assert!(result.is_err(), "{result:?}");
    assert!(!session.is_connected());
    assert_eq!(peer.all(), vec![frame_4e(&R_STATUS_BODY)]);
    assert!(session.read_clock().await.is_err(), "not reused");
}

/// The same component through the read-only loader offers the clock read
/// (it writes nothing) but not the clock set: the read sends the two
/// requests and answers the clock; the clock set is `Ok(None)` and sends
/// nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #78 component"]
async fn the_read_only_loader_offers_the_clock_read() {
    let driver = WasmDriver::load(READ_MANIFEST, &component()).unwrap();
    let peer = Peer::start(R.0, R.1, Script::default());
    let mut session = connect(&driver, &peer, &[]).await;
    assert!(session.connection().unwrap().supports_clock_read());
    assert!(!session.connection().unwrap().supports_clock());
    peer.clear();
    assert_eq!(
        session
            .set_clock(CivilTime::new(2026, 11, 7, 9, 5, 30))
            .await,
        Ok(None)
    );
    assert!(peer.all().is_empty());
    assert_eq!(session.read_clock().await, Ok(Some(Ok(EXPECTED))));
    assert_eq!(peer.all(), r_frames(frame_4e));
    assert!(session.is_connected());
    session.disconnect().await;
}
