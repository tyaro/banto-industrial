//! #917: explicitly ignored checks of the pinned Modbus WASM guest (A1, B1,
//! B2, C1, UDP probe) against the local ModbusSlave at `127.0.0.1:502`,
//! unit 1, over the direct engine path (`WasmDriver::load_operational_write`
//! + `FrameSession`).
//!
//! Every case is `#[ignore]` and refuses to open a socket unless
//! `BANTO_WASM_LIVE_917=1`. The destination is fixed; a `WireGate` inspects
//! every MBAP frame: reads (FC1-FC4) pass, writes pass only for coils
//! 00001-00008 and holding registers 40010-40029 and only while the case has
//! authorised a write. Results are `audit` lines with a UTC Unix-ns stamp.
//! When a read back disagrees the case stops without retry or restoration.
//!
//! Run one case at a time:
//! `cargo test -p banto-hub-host --test modbus_wasm_live_917 b2 -- --ignored --nocapture --test-threads=1`

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
const COMPONENT_ENV: &str = "BANTO_MODBUS_LIVE_917_COMPONENT_WASM";
const MANIFEST_ENV: &str = "BANTO_MODBUS_LIVE_917_MANIFEST";
/// Built from guest `f6c8781bab6f85a6c866e4d552039b547c4390ac` (the CI pin,
/// `wasm-tools component new` of the debug `banto_modbus_tcp_component.wasm`).
const COMPONENT_SHA256: &str = "af6cb1685991b5296f3423ac129e6c1482c371dc0a18e1b9135b1482e9eb888a";
/// `component/manifest.json` of the same guest commit (`modbus-fc3-wasm` 0.9.0).
const MANIFEST_SHA256: &str = "7399a90c0a145d803c73ed6ded9b28c695d9462f8989cf608f30dca8df2f6c43";

const HOST: &str = "127.0.0.1";
const PORT: u16 = 502;
const UNIT: u8 = 1;
/// Coils 00001-00008 as protocol addresses.
const COIL_RANGE: std::ops::RangeInclusive<u16> = 0..=7;
/// Holding registers 40010-40029 as protocol addresses.
const HOLDING_RANGE: std::ops::RangeInclusive<u16> = 9..=28;
const COIL_ALLOWLIST: &str = "000001-000008";
const HOLDING_ALLOWLIST: &str = "400010-400029";
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(3);

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

fn require_opt_in() {
    assert_eq!(
        std::env::var(OPT_IN_ENV).as_deref(),
        Ok("1"),
        "{OPT_IN_ENV}=1 is required before any live dial"
    );
}

fn pinned_file(env: &str, expected: &str, what: &str) -> Vec<u8> {
    let path = std::env::var(env).unwrap_or_else(|_| panic!("set {env}"));
    let bytes = std::fs::read(&path).unwrap_or_else(|error| panic!("{what} {path}: {error}"));
    let digest = hex(&Sha256::digest(&bytes));
    assert_eq!(
        digest, expected,
        "{what} SHA-256 differs from the pinned guest"
    );
    audit(what, format!("path={path} sha256={digest}"));
    bytes
}

fn component() -> Vec<u8> {
    pinned_file(COMPONENT_ENV, COMPONENT_SHA256, "component")
}

fn manifest() -> Vec<u8> {
    pinned_file(MANIFEST_ENV, MANIFEST_SHA256, "manifest")
}

fn require_destination(settings: &Settings, spec: &TransportSpec) -> Result<(), String> {
    let settings_ok = settings.get("host") == Some(HOST)
        && settings.get("port") == Some("502")
        && settings.get("unit_id") == Some("1")
        && settings.get("transport").unwrap_or("tcp") == "tcp";
    let spec_ok =
        matches!(spec, TransportSpec::Tcp { host, port } if host == HOST && *port == PORT);
    if settings_ok && spec_ok {
        Ok(())
    } else {
        Err(format!(
            "settings and transport {spec:?} must both be {HOST}:{PORT} unit {UNIT}"
        ))
    }
}

fn settings(probe: &str, write: Option<(&str, &str)>, extra: &[(&str, &str)]) -> Settings {
    let mut pairs = vec![
        ("host", HOST),
        ("port", "502"),
        ("unit_id", "1"),
        ("probe_address", probe),
    ];
    if let Some((guest, host)) = write {
        pairs.extend([
            ("write_mode", "enabled"),
            ("write_allowlist", guest),
            ("modbus_operational_write_mode", "enabled"),
            ("modbus_operational_write_allowlist", host),
        ]);
    }
    pairs.extend_from_slice(extra);
    Settings::from_pairs(pairs).unwrap()
}

struct WireGate {
    /// Every frame that passed (`FCn detail`).
    frames: Vec<String>,
    write_budget: usize,
    writes_sent: usize,
}

fn be16(bytes: &[u8]) -> u16 {
    u16::from_be_bytes([bytes[0], bytes[1]])
}

impl WireGate {
    fn new() -> Self {
        Self {
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
            FatalError::Protocol(format!("#917 Modbus live gate blocked a frame: {why}"))
        };
        if bytes.len() < 8
            || bytes[2..4] != [0, 0]
            || usize::from(be16(&bytes[4..6])) != bytes.len() - 6
            || bytes[6] != UNIT
        {
            return Err(blocked("malformed MBAP or wrong unit"));
        }
        let pdu = &bytes[7..];
        let function = pdu[0];
        let span = |range: std::ops::RangeInclusive<u16>, start: u16, count: u16| {
            count >= 1
                && range.contains(&start)
                && start
                    .checked_add(count - 1)
                    .is_some_and(|last| range.contains(&last))
        };
        let (is_write, detail) = match function {
            1..=4 if pdu.len() == 5 => (
                false,
                format!("read start={} count={}", be16(&pdu[1..3]), be16(&pdu[3..5])),
            ),
            5 if pdu.len() == 5 => {
                let start = be16(&pdu[1..3]);
                if !span(COIL_RANGE, start, 1) {
                    return Err(blocked("FC5 outside coils 00001-00008"));
                }
                (
                    true,
                    format!("coil start={start} value={}", hex(&pdu[3..5])),
                )
            }
            15 if pdu.len() >= 7 => {
                let (start, count) = (be16(&pdu[1..3]), be16(&pdu[3..5]));
                if !span(COIL_RANGE, start, count)
                    || pdu.len() != 6 + usize::from(pdu[5])
                    || usize::from(pdu[5]) != usize::from(count).div_ceil(8)
                {
                    return Err(blocked("FC15 outside coils 00001-00008 or malformed"));
                }
                (
                    true,
                    format!("coils start={start} count={count} bits={}", hex(&pdu[6..])),
                )
            }
            6 if pdu.len() == 5 => {
                let start = be16(&pdu[1..3]);
                if !span(HOLDING_RANGE, start, 1) {
                    return Err(blocked("FC6 outside holding 40010-40029"));
                }
                (
                    true,
                    format!("register start={start} value={}", hex(&pdu[3..5])),
                )
            }
            16 if pdu.len() >= 8 => {
                let (start, count) = (be16(&pdu[1..3]), be16(&pdu[3..5]));
                if !span(HOLDING_RANGE, start, count)
                    || count > 123
                    || usize::from(pdu[5]) != usize::from(count) * 2
                    || pdu.len() != 6 + usize::from(pdu[5])
                {
                    return Err(blocked("FC16 outside holding 40010-40029 or malformed"));
                }
                (
                    true,
                    format!(
                        "registers start={start} count={count} words={}",
                        hex(&pdu[6..])
                    ),
                )
            }
            22 if pdu.len() == 7 => {
                let start = be16(&pdu[1..3]);
                if !span(HOLDING_RANGE, start, 1) {
                    return Err(blocked("FC22 outside holding 40010-40029"));
                }
                (
                    true,
                    format!(
                        "mask start={start} and={} or={}",
                        hex(&pdu[3..5]),
                        hex(&pdu[5..7])
                    ),
                )
            }
            other => return Err(blocked(&format!("function {other} is not approved"))),
        };
        if is_write {
            if self.write_budget == 0 {
                return Err(blocked("no write authorised for this step"));
            }
            self.write_budget -= 1;
            self.writes_sent += 1;
        }
        let line = format!("FC{function} {detail}");
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

    fn frames(&self) -> Vec<String> {
        self.gate.lock().unwrap().frames.clone()
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

    async fn read(
        &mut self,
        stage: &str,
        labels: &[&str],
        specs: &[TagSpec],
    ) -> Vec<Result<Value, ItemError>> {
        match self.session.read_batch(specs).await {
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

    async fn write(&mut self, stage: &str, items: &[WriteItem]) -> Vec<Result<(), ItemError>> {
        match self.session.write_batch(items).await {
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

fn as_number(item: &Result<Value, ItemError>) -> Option<f64> {
    match item {
        Ok(Value::F64(value)) if value.is_finite() && value.fract() == 0.0 => Some(*value),
        _ => None,
    }
}

fn as_bits(items: &[Result<Value, ItemError>]) -> Option<Vec<bool>> {
    items
        .iter()
        .map(|item| match item {
            Ok(Value::Bit(bit)) => Some(*bit),
            _ => None,
        })
        .collect()
}

async fn connect(settings: Settings) -> Live {
    require_opt_in();
    let driver = WasmDriver::load_operational_write(&manifest(), &component()).unwrap();
    let spec = TransportSpec::Tcp {
        host: HOST.into(),
        port: PORT,
    };
    require_destination(&settings, &spec).expect("destination must match before dial");
    let errors = driver.validate_config(&settings);
    assert!(errors.is_empty(), "guest refused the settings: {errors:?}");
    audit(
        "connect_start",
        format!("destination={HOST}:{PORT} unit={UNIT}"),
    );
    let gate = Arc::new(Mutex::new(WireGate::new()));
    let wire = spec
        .connect(RESPONSE_TIMEOUT)
        .await
        .expect("local ModbusSlave connection");
    let guarded = Box::new(GuardedTransport {
        inner: wire,
        gate: gate.clone(),
    });
    let session =
        FrameSession::with_transport(driver.as_ref(), &settings, guarded, RESPONSE_TIMEOUT, None)
            .await
            .expect("probe handshake");
    audit(
        "handshake_complete",
        format!("frames={:?}", gate.lock().unwrap().frames),
    );
    Live {
        driver,
        session,
        gate,
    }
}

// ---------------------------------------------------------------- offline

#[test]
fn gate_passes_reads_and_only_authorised_in_range_writes() {
    let mut gate = WireGate::new();
    let frame = |pdu: &[u8]| {
        let mut bytes = vec![0x00, 0x01, 0x00, 0x00];
        bytes.extend_from_slice(&u16::try_from(pdu.len() + 1).unwrap().to_be_bytes());
        bytes.push(UNIT);
        bytes.extend_from_slice(pdu);
        bytes
    };
    for function in 1..=4u8 {
        gate.check_before_send(&frame(&[function, 0, 0, 0, 1]))
            .unwrap();
    }
    gate.check_before_send(&frame(&[3, 0, 100, 0, 10])).unwrap();
    // Wrong unit, unknown function, writes without budget.
    let mut other_unit = frame(&[3, 0, 0, 0, 1]);
    other_unit[6] = 2;
    assert!(gate.check_before_send(&other_unit).is_err());
    assert!(gate.check_before_send(&frame(&[8, 0, 0, 0, 0])).is_err());
    assert!(gate.check_before_send(&frame(&[5, 0, 0, 0xff, 0])).is_err());
    gate.authorize_writes(6);
    // Coils: 00001-00008 only (addresses 0-7).
    gate.check_before_send(&frame(&[5, 0, 7, 0xff, 0])).unwrap();
    assert!(gate.check_before_send(&frame(&[5, 0, 8, 0xff, 0])).is_err());
    gate.check_before_send(&frame(&[15, 0, 0, 0, 8, 1, 0xa5]))
        .unwrap();
    assert!(gate
        .check_before_send(&frame(&[15, 0, 1, 0, 8, 1, 0xa5]))
        .is_err());
    assert!(gate
        .check_before_send(&frame(&[15, 0, 0, 0, 9, 2, 0xa5, 0x01]))
        .is_err());
    // Holding: 40010-40029 only (addresses 9-28).
    gate.check_before_send(&frame(&[6, 0, 9, 0x12, 0x34]))
        .unwrap();
    assert!(gate
        .check_before_send(&frame(&[6, 0, 8, 0x12, 0x34]))
        .is_err());
    assert!(gate
        .check_before_send(&frame(&[6, 0, 29, 0x12, 0x34]))
        .is_err());
    gate.check_before_send(&frame(&[16, 0, 19, 0, 4, 8, 0, 1, 0, 2, 0, 3, 0, 4]))
        .unwrap();
    assert!(gate
        .check_before_send(&frame(&[16, 0, 28, 0, 2, 4, 0, 1, 0, 2]))
        .is_err());
    gate.check_before_send(&frame(&[22, 0, 9, 0xff, 0xf7, 0, 8]))
        .unwrap();
    assert!(gate
        .check_before_send(&frame(&[22, 0, 30, 0xff, 0xf7, 0, 8]))
        .is_err());
    assert_eq!(gate.writes_sent, 5);
    gate.check_before_send(&frame(&[6, 0, 28, 0, 0])).unwrap();
    assert!(gate.check_before_send(&frame(&[6, 0, 28, 0, 0])).is_err());
    assert_eq!(gate.writes_sent, 6);
}

#[test]
fn destination_guard_refuses_other_hosts_ports_units_and_transports() {
    let tcp = |host: &str, port: u16| TransportSpec::Tcp {
        host: host.into(),
        port,
    };
    let good = settings("40001", None, &[]);
    assert!(require_destination(&good, &tcp(HOST, PORT)).is_ok());
    assert!(require_destination(&good, &tcp("192.168.11.200", PORT)).is_err());
    assert!(require_destination(&good, &tcp(HOST, 5020)).is_err());
    assert!(require_destination(
        &good,
        &TransportSpec::Udp {
            host: HOST.into(),
            port: PORT
        }
    )
    .is_err());
    let other_unit =
        Settings::from_pairs([("host", HOST), ("port", "502"), ("unit_id", "2")]).unwrap();
    assert!(require_destination(&other_unit, &tcp(HOST, PORT)).is_err());
    let other_port =
        Settings::from_pairs([("host", HOST), ("port", "5020"), ("unit_id", "1")]).unwrap();
    assert!(require_destination(&other_port, &tcp(HOST, PORT)).is_err());
}

#[test]
fn every_live_case_refuses_to_start_without_the_opt_in() {
    if std::env::var(OPT_IN_ENV).is_ok() {
        return;
    }
    assert!(std::panic::catch_unwind(require_opt_in).is_err());
}

// ----------------------------------------------------------------- cases

/// A1: a coil-only connection (probe 00001) connects with exactly one FC1
/// read of one point; no holding register is touched.
#[tokio::test(flavor = "current_thread")]
#[ignore = "local ModbusSlave; BANTO_WASM_LIVE_917=1 and owner approval required"]
async fn modbus_a1_coil_only_probe_connects_with_one_fc1_read() {
    let live = connect(settings("00001", None, &[])).await;
    let frames = live.frames();
    audit("a1", format!("handshake frames={frames:?}"));
    assert_eq!(frames.len(), 1, "exactly one probe read: {frames:?}");
    assert!(frames[0].starts_with("FC1 "), "{frames:?}");
    assert_eq!(live.writes_sent(), 0);
    live.close().await;
}

/// B1: with `register_bit_write=disabled` a `40010.3` write is refused by the
/// guest before send (`Rejected`); the bit is still readable.
#[tokio::test(flavor = "current_thread")]
#[ignore = "local ModbusSlave; BANTO_WASM_LIVE_917=1 and owner approval required"]
async fn modbus_b1_register_bit_write_without_fc22_is_refused_before_send() {
    let mut live = connect(settings(
        "40010",
        Some((HOLDING_ALLOWLIST, HOLDING_ALLOWLIST)),
        &[("register_bit_write", "disabled")],
    ))
    .await;
    let spec = live.numeric("40010.3", DataType::Bit);
    let items = live
        .read("b1_read", &["40010.3"], std::slice::from_ref(&spec))
        .await;
    audit("b1", format!("read={items:?}"));
    for value in [true, false] {
        live.write_refused(
            "b1_write",
            &[WriteItem::new(spec.clone(), Value::Bit(value))],
        )
        .await;
    }
    live.close().await;
}

/// B2: coils 00001-00008 are read twice, written inverted as one FC15, read
/// back, 40010 is read twice, written (`register_write=fc6`, one FC6) and
/// read back; then both are restored and read back. Four writes in all.
#[tokio::test(flavor = "current_thread")]
#[ignore = "local ModbusSlave; BANTO_WASM_LIVE_917=1 and owner approval required"]
async fn modbus_b2_fc15_coils_and_fc6_register_write_readback_restore() {
    let allowlist = format!("{COIL_ALLOWLIST},{HOLDING_ALLOWLIST}");
    let mut live = connect(settings(
        "40010",
        Some((&allowlist, &allowlist)),
        &[("register_write", "fc6")],
    ))
    .await;
    let coil_addresses: Vec<String> = (1..=8).map(|n| format!("{n:05}")).collect();
    let coil_labels: Vec<&str> = coil_addresses.iter().map(String::as_str).collect();
    let coil_specs: Vec<TagSpec> = coil_addresses
        .iter()
        .map(|a| live.numeric(a, DataType::Bit))
        .collect();
    let word_spec = live.numeric("40010", DataType::U16);

    let coils_1 = as_bits(
        &live
            .read("b2_coils_read_1", &coil_labels, &coil_specs)
            .await,
    )
    .expect("eight coil bits");
    let word_1 = as_number(
        &live
            .read(
                "b2_40010_read_1",
                &["40010"],
                std::slice::from_ref(&word_spec),
            )
            .await[0],
    )
    .expect("one u16");
    tokio::time::sleep(Duration::from_secs(1)).await;
    let coils_2 = as_bits(
        &live
            .read("b2_coils_read_2", &coil_labels, &coil_specs)
            .await,
    )
    .expect("eight coil bits");
    let word_2 = as_number(
        &live
            .read(
                "b2_40010_read_2",
                &["40010"],
                std::slice::from_ref(&word_spec),
            )
            .await[0],
    )
    .expect("one u16");
    assert_eq!(
        coils_1, coils_2,
        "coils changed between reads; nothing written"
    );
    assert_eq!(
        word_1, word_2,
        "40010 changed between reads; nothing written"
    );

    let inverted: Vec<WriteItem> = coil_specs
        .iter()
        .zip(&coils_1)
        .map(|(spec, bit)| WriteItem::new(spec.clone(), Value::Bit(!bit)))
        .collect();
    live.authorize(1);
    let results = live.write("b2_coils_trial_write", &inverted).await;
    let coils_back = as_bits(
        &live
            .read("b2_coils_trial_readback", &coil_labels, &coil_specs)
            .await,
    );
    assert_eq!(
        results,
        vec![Ok(()); 8],
        "coil run not confirmed; stop, no restoration"
    );
    assert!(
        live.frames()
            .iter()
            .any(|f| f.starts_with("FC15 coils start=0 count=8")),
        "one FC15 over the run expected: {:?}",
        live.frames()
    );
    let expected: Vec<bool> = coils_1.iter().map(|bit| !bit).collect();
    assert_eq!(
        coils_back,
        Some(expected),
        "coils read back differ; stop, no restoration"
    );

    let trial = if word_1 == 0.0 { 1.0 } else { 0.0 };
    live.authorize(1);
    let results = live
        .write(
            "b2_40010_trial_write",
            &[WriteItem::new(word_spec.clone(), Value::F64(trial))],
        )
        .await;
    let word_back = as_number(
        &live
            .read(
                "b2_40010_trial_readback",
                &["40010"],
                std::slice::from_ref(&word_spec),
            )
            .await[0],
    );
    assert_eq!(
        results,
        vec![Ok(())],
        "40010 write not confirmed; stop, no restoration"
    );
    assert!(
        live.frames().iter().any(|f| f.starts_with("FC6 ")),
        "FC6 expected: {:?}",
        live.frames()
    );
    assert_eq!(
        word_back,
        Some(trial),
        "40010 read back differs; stop, no restoration"
    );

    let restore: Vec<WriteItem> = coil_specs
        .iter()
        .zip(&coils_1)
        .map(|(spec, bit)| WriteItem::new(spec.clone(), Value::Bit(*bit)))
        .collect();
    live.authorize(1);
    let results = live.write("b2_coils_restore_write", &restore).await;
    assert_eq!(results, vec![Ok(()); 8], "coil restoration not confirmed");
    let coils_restored = as_bits(
        &live
            .read("b2_coils_restore_readback", &coil_labels, &coil_specs)
            .await,
    );
    assert_eq!(
        coils_restored,
        Some(coils_1),
        "original coils not confirmed"
    );
    live.authorize(1);
    let results = live
        .write(
            "b2_40010_restore_write",
            &[WriteItem::new(word_spec.clone(), Value::F64(word_1))],
        )
        .await;
    assert_eq!(results, vec![Ok(())], "40010 restoration not confirmed");
    let word_restored = as_number(
        &live
            .read(
                "b2_40010_restore_readback",
                &["40010"],
                std::slice::from_ref(&word_spec),
            )
            .await[0],
    );
    assert_eq!(word_restored, Some(word_1), "original 40010 not confirmed");
    assert_eq!(live.writes_sent(), 4);
    live.close().await;
}

/// C1: a 4-word string at 40020. The span is read exactly as two u32 twice,
/// the string tag is read, "AB" is written as one FC16 over the padded span,
/// read back, then the two original u32 are written back as one FC16
/// (contiguous span) and read back. Two writes in all.
#[tokio::test(flavor = "current_thread")]
#[ignore = "local ModbusSlave; BANTO_WASM_LIVE_917=1 and owner approval required"]
async fn modbus_c1_holding_string_write_readback_restore() {
    let mut live = connect(settings(
        "40010",
        Some((HOLDING_ALLOWLIST, HOLDING_ALLOWLIST)),
        &[],
    ))
    .await;
    let labels = ["40020 u32", "40022 u32"];
    let span_specs = vec![
        live.numeric("40020", DataType::U32),
        live.numeric("40022", DataType::U32),
    ];
    let string_spec = live.string("40020", 4);
    let read_span = |items: Vec<Result<Value, ItemError>>| -> Vec<f64> {
        items
            .iter()
            .map(|item| as_number(item).unwrap_or_else(|| panic!("not a u32: {items:?}")))
            .collect()
    };
    let original = read_span(live.read("c1_span_read_1", &labels, &span_specs).await);
    tokio::time::sleep(Duration::from_secs(1)).await;
    let repeated = read_span(live.read("c1_span_read_2", &labels, &span_specs).await);
    assert_eq!(
        original, repeated,
        "span changed between reads; nothing written"
    );
    let text = live
        .read(
            "c1_string_read",
            &["40020 str4"],
            std::slice::from_ref(&string_spec),
        )
        .await;
    audit("c1", format!("original string={text:?} span={original:?}"));

    live.authorize(1);
    let results = live
        .write(
            "c1_trial_write",
            &[WriteItem::new(string_spec.clone(), Value::Str("AB".into()))],
        )
        .await;
    let back = live
        .read(
            "c1_trial_string_readback",
            &["40020 str4"],
            std::slice::from_ref(&string_spec),
        )
        .await;
    let back_span = read_span(
        live.read("c1_trial_span_readback", &labels, &span_specs)
            .await,
    );
    assert_eq!(
        results,
        vec![Ok(())],
        "string write not confirmed; stop, no restoration"
    );
    assert!(
        live.frames()
            .iter()
            .any(|f| f.starts_with("FC16 registers start=19 count=4")),
        "one FC16 over the span expected: {:?}",
        live.frames()
    );
    assert_eq!(
        back[0],
        Ok(Value::Str("AB".into())),
        "string read back differs ({back_span:?}); stop, no restoration"
    );

    live.authorize(1);
    let restore: Vec<WriteItem> = span_specs
        .iter()
        .zip(&original)
        .map(|(spec, value)| WriteItem::new(spec.clone(), Value::F64(*value)))
        .collect();
    let results = live.write("c1_restore_write", &restore).await;
    assert_eq!(results, vec![Ok(()); 2], "restoration not confirmed");
    let restored = read_span(live.read("c1_restore_readback", &labels, &span_specs).await);
    assert_eq!(restored, original, "original span not confirmed");
    assert_eq!(live.writes_sent(), 2);
    live.close().await;
}

/// UDP: one FC3 datagram (40010, one register) to 127.0.0.1:502/udp; the
/// answer or its absence is recorded. The guest manifest lists TCP only, so
/// nothing more is attempted over UDP.
#[tokio::test(flavor = "current_thread")]
#[ignore = "local ModbusSlave; BANTO_WASM_LIVE_917=1 and owner approval required"]
async fn modbus_udp_probe_records_whether_the_slave_answers() {
    require_opt_in();
    let request = [
        0x00, 0x01, 0x00, 0x00, 0x00, 0x06, UNIT, 0x03, 0x00, 0x09, 0x00, 0x01,
    ];
    let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    socket.connect((HOST, PORT)).await.unwrap();
    audit("udp", format!("send {HOST}:{PORT} bytes={}", hex(&request)));
    socket.send(&request).await.unwrap();
    let mut buffer = vec![0u8; 260];
    match tokio::time::timeout(Duration::from_secs(1), socket.recv(&mut buffer)).await {
        Ok(Ok(len)) => audit("udp", format!("answer bytes={}", hex(&buffer[..len]))),
        Ok(Err(error)) => audit("udp", format!("error {error} (port closed or refused)")),
        Err(_) => audit("udp", "no_answer timeout=1s"),
    }
}
