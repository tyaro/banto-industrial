//! #917: explicitly ignored live checks of the WASM SLMP guest stages (E1/E2,
//! A2, D1-D3, G1, F, C1, E4) against the R and Q test PLCs over the direct
//! engine path (`WasmDriver::load_operational_write` + `FrameSession`).
//!
//! Every case is `#[ignore]` and additionally refuses to open any socket
//! unless `BANTO_WASM_LIVE_917=1`. Destinations are fixed to the approved
//! hosts and ports (TCP 3109; UDP 5000 or `BANTO_WASM_LIVE_917_UDP_PORT`).
//! The resident BantoHub service's TCP ports (R 5200, Q 3100) are never used.
//! A `WireGate` inspects every frame before it is sent: reads (0101, 0401)
//! pass, a 1401 word write passes only inside the approved write range and
//! only while the case has authorised a write, anything else is blocked.
//! Results are printed as `audit` lines with a UTC Unix-ns stamp. When a read
//! back disagrees the case stops without retry or automatic restoration.
//!
//! Run one case at a time (one TCP connection per PLC port):
//! `cargo test -p banto-hub-host --test slmp_wasm_live_917 r_s1 -- --ignored --nocapture --test-threads=1`

use std::ops::RangeInclusive;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use banto_driver_api::{
    BoxFuture, DataType, DriverBase, FatalError, ItemError, Session, Settings, StringEncoding,
    TagKind, TagSpec, Value, WriteItem,
};
use banto_driver_engine::{
    transport::{Transport, TransportSpec},
    FrameSession,
};
use banto_driver_wasm::WasmDriver;
use sha2::{Digest, Sha256};

const OPT_IN_ENV: &str = "BANTO_WASM_LIVE_917";
const COMPONENT_ENV: &str = "BANTO_SLMP_LIVE_917_COMPONENT_WASM";
const UDP_PORT_ENV: &str = "BANTO_WASM_LIVE_917_UDP_PORT";
const E4_ENV: &str = "BANTO_WASM_LIVE_917_E4";
const E4_FRAME_ENV: &str = "BANTO_WASM_LIVE_917_E4_FRAME";
const MANIFEST: &[u8] = include_bytes!("t12_slmp_wasm_operational_write/manifest.json");
/// Built from guest `1fac51ad380a9c3b5416660dea6bdd4926b2879f` (the CI pin,
/// `wasm-tools component new` of the debug `banto_slmp_read_component.wasm`).
const COMPONENT_SHA256: &str = "04e3706f2ad54dc174cd44f9fa058250655ce3d72b75ec0bc486e54521131e11";

const DIRECT_PORT: u16 = 3109;
const DEFAULT_UDP_PORT: u16 = 5000;
/// TCP ports of the resident BantoHub service: never dialled by this file.
const FORBIDDEN_PORTS: [u16; 2] = [5200, 3100];
const OWN_ROUTE: [u8; 5] = [0x00, 0xff, 0xff, 0x03, 0x00];
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(3);
const BIT: u8 = 5;

struct Plc {
    label: &'static str,
    host: &'static str,
    /// Approved D word write range (inclusive).
    d_write: RangeInclusive<u32>,
    /// Approved W word write range (inclusive), Q only.
    w_write: Option<RangeInclusive<u32>>,
    /// Guest allowlist: the same ranges plus one word-bit interval.
    allowlist: &'static str,
    /// First D word read by E1/E2 and E4, and the F word-bit target on R.
    d_first: u32,
    /// First D word of the C1 string (4 words).
    d_string: u32,
    /// Word whose bit 5 is written in F.
    word_bit_target: &'static str,
}

const R: Plc = Plc {
    label: "R",
    host: "192.168.11.200",
    d_write: 3020..=3039,
    w_write: None,
    allowlist: "D3020-D3039,D3020.0-D3020.F",
    d_first: 3020,
    d_string: 3030,
    word_bit_target: "D3020",
};

const Q: Plc = Plc {
    label: "Q",
    host: "192.168.11.10",
    d_write: 1020..=1039,
    w_write: Some(0x1020..=0x103f),
    allowlist: "D1020-D1039,W1020-W103F,W1020.0-W1020.F",
    d_first: 1020,
    d_string: 1030,
    word_bit_target: "W1020",
};

fn audit(stage: &str, detail: impl std::fmt::Display) {
    let unix_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("UTC clock before Unix epoch")
        .as_nanos();
    println!("utc_unix_ns={unix_ns} stage={stage} {detail}");
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The explicit opt-in; checked before any socket is opened.
fn require_opt_in() {
    assert_eq!(
        std::env::var(OPT_IN_ENV).as_deref(),
        Ok("1"),
        "{OPT_IN_ENV}=1 is required before any live dial"
    );
}

fn component() -> Vec<u8> {
    let path = std::env::var(COMPONENT_ENV).unwrap_or_else(|_| panic!("set {COMPONENT_ENV}"));
    let bytes = std::fs::read(&path).expect("pinned guest component bytes");
    let digest = hex(&Sha256::digest(&bytes));
    assert_eq!(
        digest, COMPONENT_SHA256,
        "component SHA-256 differs from the pinned guest build"
    );
    audit("component", format!("path={path} sha256={digest}"));
    bytes
}

fn udp_port() -> u16 {
    let port = match std::env::var(UDP_PORT_ENV) {
        Ok(text) => text.parse().expect("UDP port number"),
        Err(_) => DEFAULT_UDP_PORT,
    };
    assert!(!FORBIDDEN_PORTS.contains(&port), "forbidden port {port}");
    port
}

/// Settings and the actual transport must both name the one approved
/// destination; the resident service's ports are refused outright.
fn require_destination(plc: &Plc, settings: &Settings, spec: &TransportSpec) -> Result<(), String> {
    let host_ok = settings.get("host") == Some(plc.host);
    let port: u16 = settings
        .get("port")
        .and_then(|text| text.parse().ok())
        .ok_or("port setting missing")?;
    if FORBIDDEN_PORTS.contains(&port) {
        return Err(format!(
            "port {port} belongs to the resident BantoHub service"
        ));
    }
    let transport = settings.get("transport").unwrap_or("tcp");
    let spec_ok = match spec {
        TransportSpec::Tcp { host, port: actual } => {
            transport == "tcp" && host == plc.host && *actual == port && port == DIRECT_PORT
        }
        TransportSpec::Udp { host, port: actual } => {
            transport == "udp" && host == plc.host && *actual == port && port == udp_port()
        }
        _ => false,
    };
    if host_ok && spec_ok {
        Ok(())
    } else {
        Err(format!(
            "settings {}:{port}/{transport} and transport {spec:?} must both be the approved {} destination",
            settings.get("host").unwrap_or("?"),
            plc.label
        ))
    }
}

fn settings(plc: &Plc, port: u16, extra: &[(&str, &str)]) -> Settings {
    let mut pairs = vec![
        ("host".to_owned(), plc.host.to_owned()),
        ("port".to_owned(), port.to_string()),
        ("d_u16_write_mode".to_owned(), "enabled".to_owned()),
        ("d_u16_write_allowlist".to_owned(), plc.allowlist.to_owned()),
    ];
    pairs.extend(
        extra
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned())),
    );
    Settings::from_pairs(pairs).unwrap()
}

/// One decoded request as it is about to leave the host.
struct Parsed {
    three_e: bool,
    route: [u8; 5],
    command: u16,
    subcommand: u16,
    data: Vec<u8>,
}

fn parse_request(bytes: &[u8]) -> Option<Parsed> {
    let (three_e, head) = match bytes.get(..2)? {
        [0x54, 0x00] => (false, 13usize),
        [0x50, 0x00] => (true, 9usize),
        _ => return None,
    };
    if bytes.len() < head + 6 {
        return None;
    }
    let route: [u8; 5] = bytes[head - 7..head - 2].try_into().ok()?;
    let length = usize::from(u16::from_le_bytes([bytes[head - 2], bytes[head - 1]]));
    if bytes.len() != head + length {
        return None;
    }
    Some(Parsed {
        three_e,
        route,
        command: u16::from_le_bytes([bytes[head + 2], bytes[head + 3]]),
        subcommand: u16::from_le_bytes([bytes[head + 4], bytes[head + 5]]),
        data: bytes[head + 6..].to_vec(),
    })
}

/// Device code, start and point count of a 0401 / 1401 device request.
fn device_span(subcommand: u16, data: &[u8]) -> Option<(u16, u32, u16, usize)> {
    match subcommand {
        0 | 1 => {
            let start = u32::from_le_bytes([data[0], data[1], data[2], 0]);
            let device = u16::from(data[3]);
            let count = u16::from_le_bytes([data[4], data[5]]);
            Some((device, start, count, 6))
        }
        2 | 3 => {
            let start = u32::from_le_bytes(data[..4].try_into().ok()?);
            let device = u16::from_le_bytes([data[4], data[5]]);
            let count = u16::from_le_bytes([data[6], data[7]]);
            Some((device, start, count, 8))
        }
        _ => None,
    }
}

struct WireGate {
    plc: &'static Plc,
    /// Every frame that passed, in order (`cmd/sub detail`).
    frames: Vec<String>,
    /// Word writes the current step still allows.
    write_budget: usize,
    writes_sent: usize,
}

impl WireGate {
    fn new(plc: &'static Plc) -> Self {
        Self {
            plc,
            frames: Vec::new(),
            write_budget: 0,
            writes_sent: 0,
        }
    }

    fn authorize_writes(&mut self, count: usize) {
        assert_eq!(
            self.write_budget, 0,
            "an earlier write authorisation is still open"
        );
        self.write_budget = count;
    }

    fn check_before_send(&mut self, bytes: &[u8]) -> Result<(), FatalError> {
        let blocked = |why: &str| {
            audit("wire_blocked", format!("why={why} frame={}", hex(bytes)));
            FatalError::Protocol(format!("#917 live gate blocked a frame: {why}"))
        };
        let parsed = parse_request(bytes).ok_or_else(|| blocked("unparsable SLMP request"))?;
        if parsed.route != OWN_ROUTE {
            return Err(blocked("route is not the own station"));
        }
        let frame = if parsed.three_e { "3E" } else { "4E" };
        let detail = match parsed.command {
            0x0101 => "type-name".to_owned(),
            0x0401 => match device_span(parsed.subcommand, &parsed.data) {
                Some((device, start, count, _)) if parsed.data.len() >= 6 => {
                    format!("read device={device:#06x} start={start} count={count}")
                }
                _ => format!(
                    "read-ext sub={:#06x} data={}",
                    parsed.subcommand,
                    hex(&parsed.data)
                ),
            },
            0x1401 => {
                if !matches!(parsed.subcommand, 0 | 2) {
                    return Err(blocked("only word writes are approved"));
                }
                let (device, start, count, head) = device_span(parsed.subcommand, &parsed.data)
                    .ok_or_else(|| blocked("bad write"))?;
                let end = start
                    .checked_add(u32::from(count))
                    .ok_or_else(|| blocked("write span overflow"))?;
                if count == 0 || parsed.data.len() != head + usize::from(count) * 2 {
                    return Err(blocked("write length mismatch"));
                }
                let range = match device {
                    0x00a8 => Some(&self.plc.d_write),
                    0x00b4 => self.plc.w_write.as_ref(),
                    _ => None,
                };
                let inside =
                    range.is_some_and(|range| range.contains(&start) && range.contains(&(end - 1)));
                if !inside {
                    return Err(blocked("write outside the approved range"));
                }
                if self.write_budget == 0 {
                    return Err(blocked("no write authorised for this step"));
                }
                self.write_budget -= 1;
                self.writes_sent += 1;
                format!(
                    "write device={device:#06x} start={start} count={count} words={}",
                    hex(&parsed.data[head..])
                )
            }
            other => return Err(blocked(&format!("command {other:#06x} is not approved"))),
        };
        let line = format!(
            "{frame} {:#06x}/{} {detail}",
            parsed.command, parsed.subcommand
        );
        audit("frame", &line);
        self.frames.push(line);
        Ok(())
    }
}

struct GuardedTransport {
    inner: Box<dyn Transport>,
    gate: Arc<Mutex<WireGate>>,
}

impl Transport for GuardedTransport {
    fn tx_time(&self, len: usize) -> Duration {
        self.inner.tx_time(len)
    }

    fn send<'a>(&'a mut self, bytes: &'a [u8]) -> BoxFuture<'a, Result<(), FatalError>> {
        Box::pin(async move {
            self.gate.lock().unwrap().check_before_send(bytes)?;
            self.inner.send(bytes).await
        })
    }

    fn recv<'a>(&'a mut self, buf: &'a mut Vec<u8>) -> BoxFuture<'a, Result<usize, FatalError>> {
        self.inner.recv(buf)
    }

    fn discard_input(&mut self) -> BoxFuture<'_, ()> {
        self.inner.discard_input()
    }

    fn close(&mut self) -> BoxFuture<'_, ()> {
        self.inner.close()
    }
}

struct Live {
    driver: Arc<WasmDriver>,
    session: FrameSession,
    gate: Arc<Mutex<WireGate>>,
}

impl Live {
    fn writes_sent(&self) -> usize {
        self.gate.lock().unwrap().writes_sent
    }

    fn authorize(&self, count: usize) {
        self.gate.lock().unwrap().authorize_writes(count);
    }

    fn numeric(&self, address: &str, data_type: DataType) -> TagSpec {
        let kind = TagKind::numeric(data_type);
        TagSpec::new(
            self.driver
                .parse_address(address, &kind)
                .unwrap_or_else(|error| panic!("{address}: {error:?}")),
            kind,
        )
    }

    fn string(&self, address: &str, words: u16) -> TagSpec {
        let kind = TagKind::string(words, StringEncoding::Utf8);
        TagSpec::new(
            self.driver
                .parse_address(address, &kind)
                .unwrap_or_else(|error| panic!("{address}: {error:?}")),
            kind,
        )
    }

    /// Read, print every item, stop on a fatal error.
    async fn read(
        &mut self,
        stage: &str,
        labels: &[&str],
        specs: &[TagSpec],
    ) -> Vec<Result<Value, ItemError>> {
        let result = self.session.read_batch(specs).await;
        match result {
            Ok(items) => {
                for (label, item) in labels.iter().zip(&items) {
                    audit(stage, format!("read {label} => {item:?}"));
                }
                items
            }
            Err(error) => {
                audit(stage, format!("read fatal {error:?}"));
                panic!("{stage}: fatal read error, stop: {error:?}");
            }
        }
    }

    async fn read_u16(&mut self, stage: &str, address: &str) -> u16 {
        let spec = self.numeric(address, DataType::U16);
        let items = self.read(stage, &[address], &[spec]).await;
        as_u16(&items[0]).unwrap_or_else(|| panic!("{stage}: {address} is not one u16: {items:?}"))
    }

    /// Write, print every item, stop on a fatal error.
    async fn write(&mut self, stage: &str, items: &[WriteItem]) -> Vec<Result<(), ItemError>> {
        let result = self.session.write_batch(items).await;
        match result {
            Ok(results) => {
                for (item, outcome) in items.iter().zip(&results) {
                    audit(stage, format!("write {:?} => {outcome:?}", item.value));
                }
                results
            }
            Err(error) => {
                audit(stage, format!("write fatal {error:?}"));
                panic!("{stage}: fatal write error, stop without retry: {error:?}");
            }
        }
    }

    /// A write that must be refused by the guest or the host before a frame
    /// exists: every item an error and the gate saw no new write.
    async fn write_refused(&mut self, stage: &str, items: &[WriteItem]) {
        let before = self.writes_sent();
        let results = self.write(stage, items).await;
        assert!(
            results.iter().all(Result::is_err),
            "{stage}: a write was accepted: {results:?}"
        );
        assert_eq!(
            self.writes_sent(),
            before,
            "{stage}: a write frame was sent"
        );
        audit(stage, "refused before send, no frame");
    }

    async fn close(mut self) {
        self.session.disconnect().await;
        audit(
            "connection_closed",
            format!("writes_sent={}", self.writes_sent()),
        );
    }
}

fn as_u16(item: &Result<Value, ItemError>) -> Option<u16> {
    match item {
        Ok(Value::F64(value))
            if value.is_finite() && value.fract() == 0.0 && (0.0..=65535.0).contains(value) =>
        {
            Some(*value as u16)
        }
        _ => None,
    }
}

async fn connect(plc: &'static Plc, spec: TransportSpec, extra: &[(&str, &str)]) -> Live {
    require_opt_in();
    let driver = WasmDriver::load_operational_write(MANIFEST, &component()).unwrap();
    let port = match &spec {
        TransportSpec::Tcp { port, .. } | TransportSpec::Udp { port, .. } => *port,
        _ => panic!("only TCP and UDP are approved"),
    };
    let settings = settings(plc, port, extra);
    require_destination(plc, &settings, &spec).expect("destination must match before dial");
    let errors = driver.validate_config(&settings);
    assert!(errors.is_empty(), "guest refused the settings: {errors:?}");
    audit(
        "connect_start",
        format!("plc={} destination={spec:?} extra={extra:?}", plc.label),
    );
    let gate = Arc::new(Mutex::new(WireGate::new(plc)));
    let wire = spec
        .connect(RESPONSE_TIMEOUT)
        .await
        .expect("approved test PLC connection");
    let guarded = Box::new(GuardedTransport {
        inner: wire,
        gate: gate.clone(),
    });
    let session =
        FrameSession::with_transport(driver.as_ref(), &settings, guarded, RESPONSE_TIMEOUT, None)
            .await
            .expect("type-name and SD handshake");
    audit(
        "handshake_complete",
        format!("frames={}", gate.lock().unwrap().frames.len()),
    );
    Live {
        driver,
        session,
        gate,
    }
}

async fn connect_tcp(plc: &'static Plc, extra: &[(&str, &str)]) -> Live {
    connect(
        plc,
        TransportSpec::Tcp {
            host: plc.host.into(),
            port: DIRECT_PORT,
        },
        extra,
    )
    .await
}

/// Read `specs` as exact u16 words, stop when any item is not one.
async fn read_words(live: &mut Live, step: &str, labels: &[&str], specs: &[TagSpec]) -> Vec<u16> {
    let items = live.read(step, labels, specs).await;
    let values: Option<Vec<u16>> = items.iter().map(as_u16).collect();
    values.unwrap_or_else(|| panic!("{step}: not exact u16 words: {items:?}"))
}

fn d_words(plc: &Plc, first: u32, count: u32) -> Vec<String> {
    debug_assert!(plc.d_write.contains(&first));
    (first..first + count).map(|n| format!("D{n}")).collect()
}

// ---------------------------------------------------------------- offline

#[test]
fn gate_passes_reads_and_only_authorised_in_range_word_writes() {
    let mut gate = WireGate::new(&R);
    let request = |three_e: bool, command: u16, sub: u16, data: &[u8]| {
        let mut bytes = if three_e {
            vec![0x50, 0x00]
        } else {
            vec![0x54, 0x00, 0x01, 0x00, 0x00, 0x00]
        };
        bytes.extend_from_slice(&OWN_ROUTE);
        bytes.extend_from_slice(&u16::try_from(6 + data.len()).unwrap().to_le_bytes());
        bytes.extend_from_slice(&[0x10, 0x00]);
        bytes.extend_from_slice(&command.to_le_bytes());
        bytes.extend_from_slice(&sub.to_le_bytes());
        bytes.extend_from_slice(data);
        bytes
    };
    let r_words = |start: u32, device: u16, count: u16, words: &[u16]| {
        let mut data = start.to_le_bytes().to_vec();
        data.extend_from_slice(&device.to_le_bytes());
        data.extend_from_slice(&count.to_le_bytes());
        for word in words {
            data.extend_from_slice(&word.to_le_bytes());
        }
        data
    };
    gate.check_before_send(&request(false, 0x0101, 0, &[]))
        .unwrap();
    gate.check_before_send(&request(true, 0x0101, 0, &[]))
        .unwrap();
    gate.check_before_send(&request(false, 0x0401, 2, &r_words(280, 0xa9, 4, &[])))
        .unwrap();
    gate.check_before_send(&request(false, 0x0401, 2, &r_words(3000, 0xa8, 1, &[])))
        .unwrap();
    // Unit-buffer read extension passes as a read.
    gate.check_before_send(&request(
        false,
        0x0401,
        0x0082,
        &[0, 0, 0, 0, 0, 0, 0, 0, 1, 0],
    ))
    .unwrap();
    // Writes: no budget, outside, bit sub-command, wrong route, other command.
    let inside = r_words(3020, 0xa8, 1, &[1]);
    assert!(gate
        .check_before_send(&request(false, 0x1401, 2, &inside))
        .is_err());
    gate.authorize_writes(1);
    assert!(gate
        .check_before_send(&request(false, 0x1401, 2, &r_words(3019, 0xa8, 2, &[1, 2])))
        .is_err());
    assert!(gate
        .check_before_send(&request(false, 0x1401, 2, &r_words(3039, 0xa8, 2, &[1, 2])))
        .is_err());
    assert!(gate
        .check_before_send(&request(false, 0x1401, 2, &r_words(3010, 0xa8, 1, &[1])))
        .is_err());
    assert!(gate
        .check_before_send(&request(false, 0x1401, 2, &r_words(0x1a, 0xb4, 1, &[1])))
        .is_err());
    assert!(gate
        .check_before_send(&request(false, 0x1401, 3, &r_words(3020, 0xa8, 1, &[1])))
        .is_err());
    let mut routed = request(false, 0x1401, 2, &inside);
    routed[6] = 2;
    assert!(gate.check_before_send(&routed).is_err());
    assert!(gate
        .check_before_send(&request(false, 0x1001, 0, &[1, 0, 0, 0]))
        .is_err());
    assert_eq!(gate.writes_sent, 0);
    gate.check_before_send(&request(true, 0x1401, 2, &inside))
        .unwrap();
    assert!(gate
        .check_before_send(&request(true, 0x1401, 2, &inside))
        .is_err());
    assert_eq!(gate.writes_sent, 1);

    // Q: 3-byte addresses, W range, D1020-D1039 only.
    let mut q_gate = WireGate::new(&Q);
    let q_words = |start: u32, device: u8, count: u16, words: &[u16]| {
        let mut data = start.to_le_bytes()[..3].to_vec();
        data.push(device);
        data.extend_from_slice(&count.to_le_bytes());
        for word in words {
            data.extend_from_slice(&word.to_le_bytes());
        }
        data
    };
    q_gate.authorize_writes(2);
    q_gate
        .check_before_send(&request(false, 0x1401, 0, &q_words(0x1020, 0xb4, 1, &[1])))
        .unwrap();
    assert!(q_gate
        .check_before_send(&request(false, 0x1401, 0, &q_words(0x1040, 0xb4, 1, &[1])))
        .is_err());
    assert!(q_gate
        .check_before_send(&request(false, 0x1401, 0, &q_words(3020, 0xa8, 1, &[1])))
        .is_err());
    q_gate
        .check_before_send(&request(false, 0x1401, 0, &q_words(1039, 0xa8, 1, &[1])))
        .unwrap();
    assert_eq!(q_gate.writes_sent, 2);
}

#[test]
fn destination_guard_refuses_other_hosts_ports_and_transports() {
    let tcp = |host: &str, port: u16| TransportSpec::Tcp {
        host: host.into(),
        port,
    };
    assert!(require_destination(
        &R,
        &settings(&R, DIRECT_PORT, &[]),
        &tcp(R.host, DIRECT_PORT)
    )
    .is_ok());
    assert!(require_destination(
        &Q,
        &settings(&Q, DIRECT_PORT, &[]),
        &tcp(Q.host, DIRECT_PORT)
    )
    .is_ok());
    assert!(require_destination(
        &R,
        &settings(&R, DIRECT_PORT, &[]),
        &tcp(Q.host, DIRECT_PORT)
    )
    .is_err());
    assert!(require_destination(&R, &settings(&R, 5200, &[]), &tcp(R.host, 5200)).is_err());
    assert!(require_destination(&Q, &settings(&Q, 3100, &[]), &tcp(Q.host, 3100)).is_err());
    assert!(require_destination(&R, &settings(&R, 3108, &[]), &tcp(R.host, 3108)).is_err());
    assert!(require_destination(&R, &settings(&R, DIRECT_PORT, &[]), &tcp(R.host, 3108)).is_err());
    assert!(require_destination(
        &R,
        &settings(&R, DIRECT_PORT, &[]),
        &TransportSpec::Udp {
            host: R.host.into(),
            port: DIRECT_PORT
        }
    )
    .is_err());
    assert!(require_destination(
        &R,
        &settings(&R, DIRECT_PORT, &[("transport", "udp")]),
        &tcp(R.host, DIRECT_PORT)
    )
    .is_err());
    assert!(require_destination(
        &R,
        &settings(&R, DIRECT_PORT, &[]),
        &TransportSpec::SerialOverTcp {
            host: R.host.into(),
            port: DIRECT_PORT
        }
    )
    .is_err());
}

#[test]
fn every_live_case_refuses_to_start_without_the_opt_in() {
    if std::env::var(OPT_IN_ENV).is_ok() {
        return;
    }
    assert!(std::panic::catch_unwind(require_opt_in).is_err());
}

// ---------------------------------------------------------- stage 0: UDP

/// One 0101 datagram each in the 4E and the 3E frame to the auto-open UDP
/// port (default 5000). No retransmission; the answer or its absence is
/// recorded. Nothing else is sent.
async fn s0_udp_discovery(plc: &Plc) {
    require_opt_in();
    let port = udp_port();
    let stage = format!("s0_udp_{}", plc.label);
    for (frame, mut bytes) in [
        ("4e", vec![0x54u8, 0x00, 0x01, 0x00, 0x00, 0x00]),
        ("3e", vec![0x50u8, 0x00]),
    ] {
        bytes.extend_from_slice(&OWN_ROUTE);
        bytes.extend_from_slice(&6u16.to_le_bytes());
        bytes.extend_from_slice(&[0x10, 0x00, 0x01, 0x01, 0x00, 0x00]);
        let socket = tokio::net::UdpSocket::bind("0.0.0.0:0").await.unwrap();
        socket
            .connect((plc.host, port))
            .await
            .expect("UDP connect sets the only destination");
        audit(
            &stage,
            format!(
                "send host={} port={port} frame={frame} bytes={}",
                plc.host,
                hex(&bytes)
            ),
        );
        socket.send(&bytes).await.unwrap();
        let mut buffer = vec![0u8; 512];
        match tokio::time::timeout(Duration::from_secs(1), socket.recv(&mut buffer)).await {
            Ok(Ok(len)) => audit(
                &stage,
                format!(
                    "answer port={port} frame={frame} bytes={}",
                    hex(&buffer[..len])
                ),
            ),
            Ok(Err(error)) => audit(&stage, format!("error port={port} frame={frame} {error}")),
            Err(_) => audit(
                &stage,
                format!("no_answer port={port} frame={frame} timeout=1s"),
            ),
        }
    }
}

// ----------------------------------------------------- stage 1: E1 / E2

/// 3E frame over TCP, then a 4E connection whose route settings name the own
/// station (network 0, no station, target_cpu own); the gate proves every
/// frame carries the own route. Reads only.
async fn s1_frame_3e_and_own_route(plc: &'static Plc) {
    let words = d_words(plc, plc.d_first, 4);
    for (name, extra) in [
        ("e1_3e", vec![("frame", "3e")]),
        (
            "e2_own_route_4e",
            vec![("frame", "4e"), ("network_no", "0"), ("target_cpu", "own")],
        ),
        (
            "e2_own_route_3e",
            vec![("frame", "3e"), ("network_no", "0"), ("target_cpu", "own")],
        ),
    ] {
        let stage = format!("s1_{name}_{}", plc.label);
        let mut live = connect_tcp(plc, &extra).await;
        let specs: Vec<TagSpec> = words
            .iter()
            .map(|w| live.numeric(w, DataType::U16))
            .collect();
        let labels: Vec<&str> = words.iter().map(String::as_str).collect();
        let items = live.read(&stage, &labels, &specs).await;
        assert!(items.iter().all(Result::is_ok), "{stage}: {items:?}");
        let wanted = if extra[0].1 == "3e" { "3E " } else { "4E " };
        let frames = live.gate.lock().unwrap().frames.clone();
        assert!(
            frames.iter().all(|line| line.starts_with(wanted)),
            "{stage}: {frames:?}"
        );
        live.close().await;
    }
}

// ------------------------------------------------------------ stage 2: A2

/// W word bits are read from their one W word; the whole word is read too so
/// the extracted bits can be checked against it. Reads only.
async fn s2_w_word_bit_read(plc: &'static Plc) {
    let stage = format!("s2_a2_{}", plc.label);
    let mut live = connect_tcp(plc, &[]).await;
    let labels = ["W1A", "W1A.0", "W1A.F", "W1A.5"];
    let specs = vec![
        live.numeric("W1A", DataType::U16),
        live.numeric("W1A.0", DataType::Bit),
        live.numeric("W1A.F", DataType::Bit),
        live.numeric("W1A.5", DataType::Bit),
    ];
    let items = live.read(&stage, &labels, &specs).await;
    if let Some(word) = as_u16(&items[0]) {
        for (at, bit) in [(1usize, 0u16), (2, 15), (3, 5)] {
            let expected = word & (1 << bit) != 0;
            audit(
                &stage,
                format!(
                    "{} expected_from_word={expected} got={:?}",
                    labels[at], items[at]
                ),
            );
        }
    }
    assert_eq!(live.writes_sent(), 0);
    live.close().await;
}

// ---------------------------------------------------- stage 3: D1 / D2 / D3

/// S, Z, RD, ZR are unknown-range reads; writes to them are refused before
/// anything is sent (the allowlist has no such interval, the gate has no
/// budget). On Q25PRH, ZR has no path in the guest; the refusal is recorded.
async fn s3_unknown_range_reads_and_write_refusals(plc: &'static Plc) {
    let stage = format!("s3_d1_d2_d3_{}", plc.label);
    let mut live = connect_tcp(plc, &[]).await;
    let mut tags = vec![("S10", DataType::Bit), ("Z3", DataType::U16)];
    if plc.label == "R" {
        tags.push(("RD7", DataType::U16));
    }
    tags.push(("ZR10", DataType::U16));
    let labels: Vec<&str> = tags.iter().map(|(a, _)| *a).collect();
    let specs: Vec<TagSpec> = tags.iter().map(|(a, t)| live.numeric(a, *t)).collect();
    let items = live.read(&stage, &labels, &specs).await;
    audit(&stage, format!("results={items:?}"));
    for (spec, (address, data_type)) in specs.iter().zip(&tags) {
        let value = match data_type {
            DataType::Bit => Value::Bit(true),
            _ => Value::F64(0.0),
        };
        let refusal = format!("{stage}_write_{address}");
        live.write_refused(&refusal, &[WriteItem::new(spec.clone(), value)])
            .await;
    }
    live.close().await;
}

// ------------------------------------------------------------ stage 4: G1

/// U\G buffer memory of the Ethernet module (`U0`) and, on R, the CPU buffer
/// `U3E0`. Reads only; a U\G write must be refused before send. On Q25PRH the
/// guest has no U\G path at all, so every item is expected to come back as a
/// refusal; that is recorded, not asserted.
async fn s4_unit_buffer_reads(plc: &'static Plc) {
    let stage = format!("s4_g1_{}", plc.label);
    let mut live = connect_tcp(plc, &[]).await;
    let labels = [
        "U0\\G0 u16",
        "U0\\G0 u32",
        "U0\\G0.0 bit",
        "U0\\G0 str2",
        "U3E0\\G0 u16",
    ];
    let specs = vec![
        live.numeric("U0\\G0", DataType::U16),
        live.numeric("U0\\G0", DataType::U32),
        live.numeric("U0\\G0.0", DataType::Bit),
        live.string("U0\\G0", 2),
        live.numeric("U3E0\\G0", DataType::U16),
    ];
    let items = live.read(&stage, &labels, &specs).await;
    audit(&stage, format!("results={items:?}"));
    live.write_refused(
        &format!("{stage}_write_u0g0"),
        &[WriteItem::new(specs[0].clone(), Value::F64(0.0))],
    )
    .await;
    live.close().await;
}

// ------------------------------------------------------------- stage 5: F

/// Bit 5 of one word (R: D3020, Q: W1020) by the guest's read-modify-write.
/// The word is read twice, the bit is written inverted, the read back must
/// differ in that bit only, then the original word is written back and read.
/// Two word writes in all. On Q, the D word bit must be refused before send.
async fn s5_word_bit_write(plc: &'static Plc) {
    let stage = format!("s5_f_{}", plc.label);
    let mut live = connect_tcp(plc, &[]).await;
    if plc.label == "Q" {
        let spec = live.numeric("D1020.5", DataType::Bit);
        live.write_refused(
            &format!("{stage}_q_d_bit"),
            &[WriteItem::new(spec, Value::Bit(true))],
        )
        .await;
    }
    let word = plc.word_bit_target;
    let bit_address = format!("{word}.{BIT:X}");
    let original = live
        .read_u16(&format!("{stage}_original_read_1"), word)
        .await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let repeated = live
        .read_u16(&format!("{stage}_original_read_2"), word)
        .await;
    assert_eq!(
        original, repeated,
        "{word} changed between reads; nothing written"
    );

    let trial_bit = original & (1 << BIT) == 0;
    let expected = original ^ (1 << BIT);
    let bit_spec = live.numeric(&bit_address, DataType::Bit);
    live.authorize(1);
    let results = live
        .write(
            &format!("{stage}_trial_write"),
            &[WriteItem::new(bit_spec, Value::Bit(trial_bit))],
        )
        .await;
    let observed = live
        .read_u16(&format!("{stage}_trial_readback"), word)
        .await;
    assert_eq!(
        results,
        vec![Ok(())],
        "{stage}: bit write not confirmed ({word} now {observed:#06x}); stop, no restoration"
    );
    assert_eq!(
        observed, expected,
        "{stage}: other bits changed ({word} {original:#06x} -> {observed:#06x}); stop, no restoration"
    );

    let word_spec = live.numeric(word, DataType::U16);
    live.authorize(1);
    let results = live
        .write(
            &format!("{stage}_restore_write"),
            &[WriteItem::new(word_spec, Value::F64(f64::from(original)))],
        )
        .await;
    assert_eq!(results, vec![Ok(())], "{stage}: restoration not confirmed");
    let restored = live
        .read_u16(&format!("{stage}_restore_readback"), word)
        .await;
    assert_eq!(restored, original, "{stage}: original word not confirmed");
    assert_eq!(live.writes_sent(), 2);
    live.close().await;
}

// ------------------------------------------------------------ stage 6: C1

/// A 4-word string at D3030 (R) / D1030 (Q). The four words are read as u16
/// twice (the exact original, NUL padding included), the string tag is read,
/// "AB" is written through the string tag (one 1401 of the padded span), the
/// string and the words are read back, then the four original words are
/// written back as one typed-word frame and read back. Two writes in all.
async fn s6_string_write(plc: &'static Plc) {
    let stage = format!("s6_c1_{}", plc.label);
    let mut live = connect_tcp(plc, &[]).await;
    let words = d_words(plc, plc.d_string, 4);
    let labels: Vec<&str> = words.iter().map(String::as_str).collect();
    let word_specs: Vec<TagSpec> = words
        .iter()
        .map(|w| live.numeric(w, DataType::U16))
        .collect();
    let string_spec = live.string(&words[0], 4);
    let original = read_words(
        &mut live,
        &format!("{stage}_original_read_1"),
        &labels,
        &word_specs,
    )
    .await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let repeated = read_words(
        &mut live,
        &format!("{stage}_original_read_2"),
        &labels,
        &word_specs,
    )
    .await;
    assert_eq!(original, repeated, "{stage}: words changed between reads");
    let text = live
        .read(
            &format!("{stage}_original_string"),
            &[&words[0]],
            std::slice::from_ref(&string_spec),
        )
        .await;
    audit(
        &stage,
        format!("original string={text:?} words={original:04x?}"),
    );

    live.authorize(1);
    let results = live
        .write(
            &format!("{stage}_trial_write"),
            &[WriteItem::new(string_spec.clone(), Value::Str("AB".into()))],
        )
        .await;
    let back = live
        .read(
            &format!("{stage}_trial_string_readback"),
            &[&words[0]],
            std::slice::from_ref(&string_spec),
        )
        .await;
    let back_words = read_words(
        &mut live,
        &format!("{stage}_trial_words_readback"),
        &labels,
        &word_specs,
    )
    .await;
    assert_eq!(
        results,
        vec![Ok(())],
        "{stage}: string write not confirmed; stop"
    );
    assert_eq!(
        back[0],
        Ok(Value::Str("AB".into())),
        "{stage}: string read back differs ({back_words:04x?}); stop, no restoration"
    );
    assert_eq!(
        back_words,
        vec![u16::from_le_bytes(*b"AB"), 0, 0, 0],
        "{stage}: padded words differ; stop, no restoration"
    );

    live.authorize(1);
    let items: Vec<WriteItem> = word_specs
        .iter()
        .zip(&original)
        .map(|(spec, value)| WriteItem::new(spec.clone(), Value::F64(f64::from(*value))))
        .collect();
    let results = live.write(&format!("{stage}_restore_write"), &items).await;
    assert_eq!(
        results,
        vec![Ok(()); 4],
        "{stage}: restoration not confirmed"
    );
    let restored = read_words(
        &mut live,
        &format!("{stage}_restore_readback"),
        &labels,
        &word_specs,
    )
    .await;
    assert_eq!(restored, original, "{stage}: original words not confirmed");
    assert_eq!(live.writes_sent(), 2);
    live.close().await;
}

// ------------------------------------------------------------ stage 7: E4

/// UDP transport to the auto-open port, only with `BANTO_WASM_LIVE_917_E4=1`
/// (set after stage 0 showed an answer). One D word is read twice, written
/// once (bit 0 inverted), read back, restored and read back.
async fn s7_udp_transport(plc: &'static Plc) {
    require_opt_in();
    let stage = format!("s7_e4_{}", plc.label);
    if std::env::var(E4_ENV).as_deref() != Ok("1") {
        audit(
            &stage,
            format!("skip: {E4_ENV}=1 not set (stage 0 answer required)"),
        );
        return;
    }
    let frame = std::env::var(E4_FRAME_ENV).unwrap_or_else(|_| "4e".into());
    let port = udp_port();
    let mut live = connect(
        plc,
        TransportSpec::Udp {
            host: plc.host.into(),
            port,
        },
        &[("transport", "udp"), ("frame", frame.as_str())],
    )
    .await;
    let word = format!("D{}", plc.d_first);
    let original = live
        .read_u16(&format!("{stage}_original_read_1"), &word)
        .await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let repeated = live
        .read_u16(&format!("{stage}_original_read_2"), &word)
        .await;
    assert_eq!(
        original, repeated,
        "{word} changed between reads; nothing written"
    );
    let trial = original ^ 1;
    let spec = live.numeric(&word, DataType::U16);
    live.authorize(1);
    let results = live
        .write(
            &format!("{stage}_trial_write"),
            &[WriteItem::new(spec.clone(), Value::F64(f64::from(trial)))],
        )
        .await;
    let observed = live
        .read_u16(&format!("{stage}_trial_readback"), &word)
        .await;
    assert_eq!(results, vec![Ok(())], "{stage}: write not confirmed; stop");
    assert_eq!(
        observed, trial,
        "{stage}: read back differs; stop, no restoration"
    );
    live.authorize(1);
    let results = live
        .write(
            &format!("{stage}_restore_write"),
            &[WriteItem::new(spec, Value::F64(f64::from(original)))],
        )
        .await;
    assert_eq!(results, vec![Ok(())], "{stage}: restoration not confirmed");
    let restored = live
        .read_u16(&format!("{stage}_restore_readback"), &word)
        .await;
    assert_eq!(restored, original);
    assert_eq!(live.writes_sent(), 2);
    live.close().await;
}

// ---------------------------------------------------------------- cases

macro_rules! live_case {
    ($name:ident, $stage:ident, $plc:expr) => {
        #[tokio::test(flavor = "current_thread")]
        #[ignore = "real test PLC; BANTO_WASM_LIVE_917=1 and owner approval required"]
        async fn $name() {
            $stage(&$plc).await;
        }
    };
}

live_case!(r_s0_udp_discovery, s0_udp_discovery, R);
live_case!(q_s0_udp_discovery, s0_udp_discovery, Q);
live_case!(
    r_s1_e1_e2_frame_3e_and_own_route,
    s1_frame_3e_and_own_route,
    R
);
live_case!(
    q_s1_e1_e2_frame_3e_and_own_route,
    s1_frame_3e_and_own_route,
    Q
);
live_case!(r_s2_a2_w_word_bit_read, s2_w_word_bit_read, R);
live_case!(q_s2_a2_w_word_bit_read, s2_w_word_bit_read, Q);
live_case!(
    r_s3_d1_d2_d3_unknown_range_reads,
    s3_unknown_range_reads_and_write_refusals,
    R
);
live_case!(
    q_s3_d1_d2_d3_unknown_range_reads,
    s3_unknown_range_reads_and_write_refusals,
    Q
);
live_case!(r_s4_g1_unit_buffer_reads, s4_unit_buffer_reads, R);
live_case!(q_s4_g1_unit_buffer_reads, s4_unit_buffer_reads, Q);
live_case!(r_s5_f_word_bit_write_readback_restore, s5_word_bit_write, R);
live_case!(q_s5_f_word_bit_write_readback_restore, s5_word_bit_write, Q);
live_case!(r_s6_c1_string_write_readback_restore, s6_string_write, R);
live_case!(q_s6_c1_string_write_readback_restore, s6_string_write, Q);
live_case!(r_s7_e4_udp_word_write_readback_restore, s7_udp_transport, R);
live_case!(q_s7_e4_udp_word_write_readback_restore, s7_udp_transport, Q);
