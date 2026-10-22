//! Owner decision Q4 (docs/banto-hub-driver-io-design.md §13): the frames
//! `frame.rs` builds are the frames the `slmp` crate (v0.2.1, the I/O this
//! driver used until R2b-3d) puts on the wire, byte for byte, for every
//! combination of series (Q / L / R), command (read / write) and unit (word
//! / bit), every device, and the edges of the device numbers and point
//! counts; and a response is judged the same way (data, refusal, broken).
//!
//! The crate's builders are private, so its bytes are taken where they leave
//! it: a real `slmp::SLMPClient` talks to a loopback peer (a plain `std`
//! thread) that records every request and answers it.
//!
//! Where the two deliberately differ, a test says so
//! ([`where_this_driver_is_stricter`]).

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::{Arc, Mutex};
use std::thread;

use banto_driver_api::FatalError;
use banto_plc::slmp::address::MAX_DEVICE_NUMBER;
use banto_plc::{SlmpAccess, SlmpConfig, SlmpDevice};

use crate::frame::{self, Answer, Route, Series, Span, MAX_BIT_POINTS, MAX_WORD_POINTS};

/// A loopback peer: records every request, answers with `canned` when set,
/// otherwise with a well-formed success for the request.
struct Peer {
    addr: SocketAddr,
    seen: Arc<Mutex<Vec<Vec<u8>>>>,
    canned: Arc<Mutex<Option<Vec<u8>>>>,
}

impl Peer {
    fn start() -> Peer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let canned = Arc::new(Mutex::new(None::<Vec<u8>>));
        let (seen2, canned2) = (seen.clone(), canned.clone());
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                loop {
                    let mut head = [0u8; frame::HEADER_LEN];
                    if stream.read_exact(&mut head).is_err() {
                        break;
                    }
                    let declared = usize::from(u16::from_le_bytes([head[11], head[12]]));
                    let mut rest = vec![0u8; declared.saturating_sub(2)];
                    if stream.read_exact(&mut rest).is_err() {
                        break;
                    }
                    let mut request = head.to_vec();
                    request.extend_from_slice(&rest);
                    let reply = canned2
                        .lock()
                        .unwrap()
                        .clone()
                        .unwrap_or_else(|| success_for(&request));
                    seen2.lock().unwrap().push(request);
                    if stream.write_all(&reply).is_err() {
                        break;
                    }
                }
            }
        });
        Peer { addr, seen, canned }
    }

    fn last(&self) -> Vec<u8> {
        self.seen
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("a request")
    }

    fn answer_with(&self, response: Option<Vec<u8>>) {
        *self.canned.lock().unwrap() = response;
    }
}

/// The route a request carries (to echo it).
fn route_of(request: &[u8]) -> Route {
    Route {
        serial_id: u16::from_le_bytes([request[2], request[3]]),
        network_id: request[6],
        pc_id: request[7],
        io_id: u16::from_le_bytes([request[8], request[9]]),
        area_id: request[10],
        cpu_timer: u16::from_le_bytes([request[13], request[14]]),
    }
}

fn response(route: &Route, end_code: u16, data: &[u8]) -> Vec<u8> {
    let mut out = vec![0xD4, 0x00];
    out.extend_from_slice(&route.serial_id.to_le_bytes());
    out.extend_from_slice(&[0, 0, route.network_id, route.pc_id]);
    out.extend_from_slice(&route.io_id.to_le_bytes());
    out.push(route.area_id);
    out.extend_from_slice(&((2 + data.len()) as u16).to_le_bytes());
    out.extend_from_slice(&end_code.to_le_bytes());
    out.extend_from_slice(data);
    out
}

/// Zeros for a read (sized by the point count the request carries), no data
/// for a write.
fn success_for(request: &[u8]) -> Vec<u8> {
    let route = route_of(request);
    let data_len = match frame::parse_request(request) {
        Some(r) if r.command == frame::COMMAND_BULK_READ => r.span.read_data_len(),
        _ => 0,
    };
    response(&route, 0, &vec![0; data_len])
}

fn props(addr: SocketAddr, route: &Route, cpu: slmp::CPU) -> slmp::SLMP4EConnectionProps {
    slmp::SLMP4EConnectionProps {
        ip: addr.ip().to_string(),
        port: addr.port(),
        cpu,
        serial_id: route.serial_id,
        network_id: route.network_id,
        pc_id: route.pc_id,
        io_id: route.io_id,
        area_id: route.area_id,
        cpu_timer: route.cpu_timer,
    }
}

async fn client(peer: &Peer, route: &Route, cpu: slmp::CPU) -> slmp::SLMPClient {
    let client = slmp::SLMPClient::new(props(peer.addr, route, cpu));
    client.connect().await.unwrap();
    client
}

fn custom_route() -> Route {
    Route {
        serial_id: 0xBEEF,
        network_id: 0x01,
        pc_id: 0x02,
        io_id: 0x03E0,
        area_id: 0x03,
        cpu_timer: 0x0020,
    }
}

/// The header this driver sends by default is the one the session sends
/// (`SlmpConfig::default()` → `to_wire_props`).
#[test]
fn the_default_route_is_the_sessions() {
    let c = SlmpConfig::default();
    assert_eq!(
        Route::default(),
        Route {
            serial_id: c.serial_id,
            network_id: c.network_id,
            pc_id: c.pc_id,
            io_id: c.io_id,
            area_id: c.area_id,
            cpu_timer: c.cpu_timer,
        }
    );
}

#[test]
fn device_codes_are_the_crates() {
    for &device in SlmpDevice::all() {
        assert_eq!(
            frame::device_code(device),
            device.to_wire().to_code(),
            "{device}"
        );
    }
    assert_eq!(SlmpDevice::all().len(), 28);
}

#[test]
fn end_code_names_are_the_crates() {
    let mut codes: Vec<u16> = vec![
        0xC059, 0xC05C, 0xC061, 0xCEE0, 0xCEE1, 0xCEE2, 0xCF10, 0xCF20, 0xCF30, 0xCF31, 0xCF70,
        0xCF71,
    ];
    codes.extend([0x0000, 0x0001, 0xC051, 0xC056, 0xFFFF]);
    for code in codes {
        assert_eq!(
            frame::end_code_name(code),
            slmp::end_code_name(code),
            "{code:04X}"
        );
    }
}

/// Starts and counts that touch the edges: the first number, a number using
/// all three bytes, the last number, and the largest count.
fn spans(device: SlmpDevice) -> Vec<Span> {
    let max = if device.access() == SlmpAccess::Bit {
        MAX_BIT_POINTS
    } else {
        MAX_WORD_POINTS
    };
    let mut out = vec![
        Span {
            device,
            start: 0,
            count: 1,
        },
        Span {
            device,
            start: 0x01_2345,
            count: 2,
        },
        Span {
            device,
            start: 0xAB_CDEF,
            count: 3,
        },
        Span {
            device,
            start: MAX_DEVICE_NUMBER,
            count: 1,
        },
        Span {
            device,
            start: MAX_DEVICE_NUMBER - 6,
            count: 7,
        },
    ];
    if matches!(
        device,
        SlmpDevice::D | SlmpDevice::M | SlmpDevice::ZR | SlmpDevice::X
    ) {
        out.push(Span {
            device,
            start: 100,
            count: max,
        });
        out.push(Span {
            device,
            start: MAX_DEVICE_NUMBER - u32::from(max) + 1,
            count: max,
        });
    }
    out
}

/// A data pattern of `n` points that is not all zeros (and not symmetric,
/// so a swapped nibble or byte shows).
fn word_data(n: u16) -> Vec<u16> {
    (0..n)
        .map(|i| 0x1234u16.wrapping_mul(i + 1) ^ 0x8001)
        .collect()
}

fn bit_data(n: u16) -> Vec<bool> {
    (0..n).map(|i| i % 3 == 0 || i % 7 == 1).collect()
}

/// Every request byte for byte: Q / L / R × read / write × every device (in
/// its own unit) × the edge spans, with the default and a non-default route.
#[tokio::test]
async fn every_request_is_the_crates_byte_for_byte() {
    let peer = Peer::start();
    let mut checked = 0usize;
    for route in [Route::default(), custom_route()] {
        for (cpu, series) in [
            (slmp::CPU::Q, Series::Q),
            (slmp::CPU::L, Series::Q),
            (slmp::CPU::R, Series::R),
        ] {
            let mut c = client(&peer, &route, cpu).await;
            for &device in SlmpDevice::all() {
                for span in spans(device) {
                    let start = slmp::Device {
                        device_type: device.to_wire(),
                        address: span.start as usize,
                    };
                    let count = usize::from(span.count);
                    let label = format!("{cpu:?} {device}{} ×{count}", span.start);

                    // Read.
                    let unit = if span.is_bits() {
                        slmp::DataType::Bool
                    } else {
                        slmp::DataType::U16
                    };
                    let got = c.bulk_read(start, count, unit).await;
                    assert!(got.is_ok(), "{label}: {got:?}");
                    assert_eq!(
                        frame::bulk_read(&route, series, &span).unwrap(),
                        peer.last(),
                        "read {label}"
                    );

                    // Write.
                    let ours = if span.is_bits() {
                        let bits = bit_data(span.count);
                        let data: Vec<slmp::TypedData> =
                            bits.iter().map(|&b| slmp::TypedData::Bool(b)).collect();
                        c.bulk_write(start, &data).await.unwrap();
                        frame::bulk_write_bits(&route, series, &span, &bits).unwrap()
                    } else {
                        let words = word_data(span.count);
                        let data: Vec<slmp::TypedData> =
                            words.iter().map(|&w| slmp::TypedData::U16(w)).collect();
                        c.bulk_write(start, &data).await.unwrap();
                        frame::bulk_write_words(&route, series, &span, &words).unwrap()
                    };
                    assert_eq!(ours, peer.last(), "write {label}");
                    checked += 2;
                }
            }
            c.close().await;
        }
    }
    // 2 routes × 3 series × (28 devices × 5 spans + 4 devices × 2) × 2.
    assert_eq!(checked, 2 * 3 * (28 * 5 + 4 * 2) * 2);
}

/// What became of one response.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Verdict {
    Words(Vec<u16>),
    Bits(Vec<bool>),
    Refused(u16),
    Framing,
}

/// The crate's verdict, with the one rule the session adds on top of it
/// (`banto_plc::slmp::execute_one`): a response with a different number of
/// points than asked for is fatal.
fn crate_verdict(result: slmp::SlmpResult<Vec<slmp::DeviceData>>, count: usize) -> Verdict {
    match result {
        Ok(points) if points.len() != count => Verdict::Framing,
        Ok(points) => {
            if points
                .iter()
                .all(|p| matches!(p.data, slmp::TypedData::Bool(_)))
            {
                Verdict::Bits(
                    points
                        .iter()
                        .map(|p| matches!(p.data, slmp::TypedData::Bool(true)))
                        .collect(),
                )
            } else {
                Verdict::Words(
                    points
                        .iter()
                        .map(|p| match p.data {
                            slmp::TypedData::U16(w) => w,
                            other => panic!("not a word: {other:?}"),
                        })
                        .collect(),
                )
            }
        }
        Err(slmp::SlmpError::Device { end_code }) => Verdict::Refused(end_code),
        Err(slmp::SlmpError::Framing(_)) => Verdict::Framing,
        Err(other) => panic!("not a verdict on the bytes: {other:?}"),
    }
}

fn our_verdict(route: &Route, span: &Span, response: &[u8]) -> Verdict {
    let fatal = |e: FatalError| {
        assert!(matches!(e, FatalError::Framing(_)), "{e:?}");
        Verdict::Framing
    };
    match frame::check_response(route, response) {
        Err(e) => fatal(e),
        Ok(Answer::Refused(code)) => Verdict::Refused(code),
        Ok(Answer::Data(data)) if span.is_bits() => {
            frame::bits(data, span.count).map_or_else(fatal, Verdict::Bits)
        }
        Ok(Answer::Data(data)) => frame::words(data, span.count).map_or_else(fatal, Verdict::Words),
    }
}

/// The responses to a read of `span`, each `(label, bytes)`: good data,
/// refusals, and one broken field at a time.
fn responses(route: &Route, span: &Span, data: &[u8]) -> Vec<(String, Vec<u8>)> {
    let good = response(route, 0, data);
    let mut out = vec![
        ("good".to_string(), good.clone()),
        ("refused".into(), response(route, 0xC051, &[])),
        (
            "refused + error info".into(),
            response(
                route,
                0xC059,
                &[0x00, 0xFF, 0xFF, 0x03, 0x00, 0x01, 0x04, 0x00, 0x00],
            ),
        ),
    ];
    for (at, name) in [
        (0, "subheader 0"),
        (1, "subheader 1"),
        (2, "serial 0"),
        (3, "serial 1"),
        (4, "blank 0"),
        (5, "blank 1"),
        (6, "network"),
        (7, "pc"),
        (8, "io 0"),
        (9, "io 1"),
        (10, "area"),
    ] {
        let mut bad = good.clone();
        bad[at] ^= 0x01;
        out.push((format!("broken {name}"), bad));
    }
    // A data block one point short (for a word read: one word).
    let short = &data[..data.len() - if span.is_bits() { 1 } else { 2 }];
    out.push(("short".into(), response(route, 0, short)));
    out
}

/// Every response judged as the crate (plus the session's point-count rule)
/// judges it.
#[tokio::test]
async fn responses_are_judged_like_the_crate() {
    let peer = Peer::start();
    for route in [Route::default(), custom_route()] {
        for cpu in [slmp::CPU::Q, slmp::CPU::R] {
            let mut c = client(&peer, &route, cpu).await;
            let cases: [(Span, Vec<u8>); 4] = [
                (
                    Span {
                        device: SlmpDevice::D,
                        start: 100,
                        count: 2,
                    },
                    vec![0x34, 0x12, 0xCD, 0xAB],
                ),
                (
                    Span {
                        device: SlmpDevice::M,
                        start: 0,
                        count: 3,
                    },
                    vec![0x10, 0x10],
                ),
                (
                    Span {
                        device: SlmpDevice::X,
                        start: 0x1A,
                        count: 4,
                    },
                    vec![0x11, 0x01],
                ),
                // Nibbles other than 0 / 1: both read the lowest bit.
                (
                    Span {
                        device: SlmpDevice::SM,
                        start: 400,
                        count: 3,
                    },
                    vec![0x30, 0xF2],
                ),
            ];
            for (span, data) in cases {
                for (label, bytes) in responses(&route, &span, &data) {
                    peer.answer_with(Some(bytes.clone()));
                    let unit = if span.is_bits() {
                        slmp::DataType::Bool
                    } else {
                        slmp::DataType::U16
                    };
                    let start = slmp::Device {
                        device_type: span.device.to_wire(),
                        address: span.start as usize,
                    };
                    let theirs = crate_verdict(
                        c.bulk_read(start, usize::from(span.count), unit).await,
                        usize::from(span.count),
                    );
                    let ours = our_verdict(&route, &span, &bytes);
                    assert_eq!(ours, theirs, "{cpu:?} {:?} {label}", span.device);
                }
            }
            peer.answer_with(None);
            c.close().await;
        }
    }
}

/// A write's answer: the crate takes success / refusal / broken the same.
#[tokio::test]
async fn write_answers_are_judged_like_the_crate() {
    let peer = Peer::start();
    let route = custom_route();
    let mut c = client(&peer, &route, slmp::CPU::R).await;
    let start = slmp::Device {
        device_type: slmp::DeviceType::D,
        address: 300,
    };
    let mut broken = response(&route, 0, &[]);
    broken[2] ^= 0x01;
    for (label, bytes, ours) in [
        ("good", response(&route, 0, &[]), Ok(Answer::Data(&[][..]))),
        (
            "refused",
            response(&route, 0xC051, &[]),
            Ok(Answer::Refused(0xC051)),
        ),
        ("broken serial", broken, Err(())),
    ] {
        peer.answer_with(Some(bytes.clone()));
        let theirs = c.bulk_write(start, &[slmp::TypedData::U16(1)]).await;
        match (&ours, &theirs) {
            (Ok(Answer::Data(_)), Ok(())) => {}
            (Ok(Answer::Refused(a)), Err(slmp::SlmpError::Device { end_code })) => {
                assert_eq!(a, end_code, "{label}")
            }
            (Err(()), Err(slmp::SlmpError::Framing(_))) => {}
            _ => panic!("{label}: ours {ours:?}, crate {theirs:?}"),
        }
        let got = frame::write_ack(&route, &bytes).map_err(|e| {
            assert!(matches!(e, FatalError::Framing(_)), "{e:?}");
        });
        assert_eq!(got, ours, "{label}");
    }
    c.close().await;
}

/// The two places this driver is stricter than the crate, pinned so that a
/// change to either side is seen.
///
/// 1. A refusal whose echoed fields do not match this request: the crate
///    looks at the end code first and reports `Device`; this driver checks
///    the fields first, so it is `Framing` (it is not this request's
///    refusal).
/// 2. A bit read answered with more bytes than its points need: the crate
///    takes the first points and ignores the rest; this driver wants exactly
///    `ceil(count / 2)` bytes.
#[tokio::test]
async fn where_this_driver_is_stricter() {
    let peer = Peer::start();
    let route = Route::default();
    let mut c = client(&peer, &route, slmp::CPU::R).await;

    let mut refusal = response(&route, 0xC051, &[]);
    refusal[2] ^= 0x01;
    peer.answer_with(Some(refusal.clone()));
    let span = Span {
        device: SlmpDevice::D,
        start: 0,
        count: 1,
    };
    let start = slmp::Device {
        device_type: slmp::DeviceType::D,
        address: 0,
    };
    let theirs = crate_verdict(c.bulk_read(start, 1, slmp::DataType::U16).await, 1);
    assert_eq!(theirs, Verdict::Refused(0xC051));
    assert_eq!(our_verdict(&route, &span, &refusal), Verdict::Framing);

    let long = response(&route, 0, &[0x10, 0x00]);
    peer.answer_with(Some(long.clone()));
    let span = Span {
        device: SlmpDevice::M,
        start: 0,
        count: 2,
    };
    let start = slmp::Device {
        device_type: slmp::DeviceType::M,
        address: 0,
    };
    let theirs = crate_verdict(c.bulk_read(start, 2, slmp::DataType::Bool).await, 2);
    assert_eq!(theirs, Verdict::Bits(vec![true, false]));
    assert_eq!(our_verdict(&route, &span, &long), Verdict::Framing);
    c.close().await;
}
