//! The conformance checks of `banto-driver-conformance` against the R1
//! MELSEC SLMP driver (docs/banto-driver-api-design.md §7.3, R1-5):
//!
//! - `static_checks` (C1, C2) on [`SlmpDriver`], with fixtures copied from
//!   `banto-plc/src/slmp/address.rs`'s tests (the grammar) and the
//!   TypeScript `continuousRegistration.test.ts` / `offsetCopy.test.ts`
//!   (offsets).
//! - `session::run_all_against` (C1-C8, C12-C15) through the frame-type
//!   `SlmpDriver` wrapped by the real engine (`FrameSession`, R2b-3d-2,
//!   docs/banto-hub-driver-io-design.md §11.2), on the R and the Q series,
//!   against `banto-plc`'s SLMP simulator behind a `FaultProxy`. C14's
//!   check-read mismatch and C15's refusals are made by the proxy rewriting
//!   one response. Until R2b-3d-join the same checks also ran through the
//!   session-type driver (`session_conforms`); that driver is gone. The
//!   wrapper is the one `banto-broker` puts around a
//!   frame-type entry (`FrameDial`, crate-private there), as in
//!   `banto-driver-modbus`'s `tests/conformance.rs`. The bit-in-word writes
//!   go through `write_next`'s three steps, so the same response patches
//!   (read, write-back, check read) apply.
//!
//! Every simulator and proxy binds 127.0.0.1:0, so these tests need no
//! nextest test-group.

use std::time::Duration;

use banto_driver_api::{
    AddressError, BoxFuture, ConnectContext, DataType, DriverAddress, DriverBase, DriverDescriptor,
    FatalError, FieldError, ItemError, Session, SessionDriver, Settings, StringEncoding, TagKind,
    TagSpec, Value,
};
use banto_driver_conformance::proxy::FaultProxy;
use banto_driver_conformance::session::{self, BitWriteCase, ResponsePatch, SessionFixtures};
use banto_driver_conformance::{static_checks, StaticFixtures};
use banto_driver_engine::transport::TransportSpec;
use banto_driver_engine::FrameSession;
use banto_driver_slmp::SlmpDriver;
use banto_plc::slmp::address::SlmpDevice;
use banto_plc::slmp::simulator::Simulator;

fn n(data_type: DataType) -> TagKind {
    TagKind::numeric(data_type)
}

/// The frame-type driver as a `SessionDriver`: connect = the engine's
/// `FrameSession` over TCP to the settings' host and port.
struct EngineSession;

impl DriverBase for EngineSession {
    fn descriptor(&self) -> &DriverDescriptor {
        SlmpDriver.descriptor()
    }
    fn validate_config(&self, settings: &Settings) -> Vec<FieldError> {
        SlmpDriver.validate_config(settings)
    }
    fn secret_keys(&self) -> Vec<String> {
        SlmpDriver.secret_keys()
    }
    fn parse_address(&self, text: &str, kind: &TagKind) -> Result<DriverAddress, AddressError> {
        SlmpDriver.parse_address(text, kind)
    }
    fn offset_address(
        &self,
        canonical: &str,
        kind: &TagKind,
        n: i64,
    ) -> Result<String, AddressError> {
        SlmpDriver.offset_address(canonical, kind, n)
    }
    fn address_help(&self) -> String {
        SlmpDriver.address_help()
    }
    fn probe(&self) -> Option<(String, TagKind)> {
        SlmpDriver.probe()
    }
}

impl SessionDriver for EngineSession {
    fn connect(
        &self,
        settings: &Settings,
        cx: ConnectContext,
    ) -> BoxFuture<'static, Result<Box<dyn Session>, FatalError>> {
        let settings = settings.clone();
        Box::pin(async move {
            let spec = TransportSpec::Tcp {
                host: settings.get("host").expect("host").to_string(),
                port: settings.get("port").expect("port").parse().expect("port"),
            };
            let session = FrameSession::connect(&SlmpDriver, &settings, &spec, &cx, None).await?;
            Ok(Box::new(session) as Box<dyn Session>)
        })
    }
}

fn statics() -> StaticFixtures {
    use DataType::*;
    let one = || n(I16);
    let mut f = StaticFixtures::new(
        Settings::from_pairs([("host", "192.168.0.10"), ("port", "5000")]).unwrap(),
    );
    // banto-plc/src/slmp/address.rs: decimal and hexadecimal devices, the
    // longest-mnemonic-first prefixes, case, trimming, leading zeros, the
    // hex bit suffix (canonical = slmp_address::format).
    for (text, kind, canonical) in [
        ("D100", one(), "D100"),
        ("R1000", one(), "R1000"),
        ("ZR32768", one(), "ZR32768"),
        ("SD0", one(), "SD0"),
        ("M50", n(Bit), "M50"),
        ("SM400", n(Bit), "SM400"),
        ("L0", n(Bit), "L0"),
        ("X1A", n(Bit), "X1A"),
        ("Y20", n(Bit), "Y20"),
        ("B3F", n(Bit), "B3F"),
        ("W1FF", one(), "W1FF"),
        ("SW100", one(), "SW100"),
        ("DX10", n(Bit), "DX10"),
        ("DY3F", n(Bit), "DY3F"),
        ("S100", n(Bit), "S100"),
        ("Z9", one(), "Z9"),
        ("d100", one(), "D100"),
        ("zr5", one(), "ZR5"),
        ("x1a", n(Bit), "X1A"),
        ("Sd7", one(), "SD7"),
        ("  D100  ", one(), "D100"),
        ("X0010", n(Bit), "X10"),
        ("D0000", one(), "D0"),
        ("D16777215", one(), "D16777215"),
        ("D100.5", n(Bit), "D100.5"),
        ("W10.0", n(Bit), "W10.0"),
        ("ZR5.F", n(Bit), "ZR5.F"),
        ("D100.A", n(Bit), "D100.A"),
        ("D100.a", n(Bit), "D100.A"),
        ("d100.f", n(Bit), "D100.F"),
        ("D100", n(F32), "D100"),
        (
            "D100",
            TagKind::string(16, StringEncoding::ShiftJis),
            "D100",
        ),
    ] {
        f = f.valid(text, kind, canonical);
    }
    // R2b-2a (address design §3.2 Q4): refused for the tag's kind although
    // the text itself is a valid device.
    for (text, kind) in [
        ("D100", n(I64)),
        ("D100", n(U64)),
        ("D100", n(F64)),
        ("D100.5", n(U16)),
        ("D100", n(Bit)),
        ("M10", n(U16)),
        ("M10", n(F32)),
        ("M10", TagKind::string(4, StringEncoding::ShiftJis)),
        ("D100.5", TagKind::string(4, StringEncoding::ShiftJis)),
    ] {
        f = f.invalid(text, kind);
    }
    for text in [
        "",
        "   ",
        "  bogus  ",
        "M1A",
        "D1F",
        "X1G",
        "WZZ",
        "D",
        "ZR",
        "T100",
        "C100",
        "P0",
        "K4M0",
        "H10",
        "E1",
        "U3E0",
        "D16777216",
        "D99999999999999999999",
        "40001",
        "00001",
        "465536",
        "M50.0",
        "X1A.3",
        "Y0.F",
        "TS0.0",
        "D100.10",
        "D100.15",
        "D100.99",
        "D100.G",
        "D100.g",
        "D0.",
        "D0..5",
        "D0.5.6",
        "D0.-1",
        "ZZZ",
    ] {
        f = f.invalid(text, one());
    }
    // continuousRegistration.test.ts / offsetCopy.test.ts.
    for (canonical, kind, step, expected) in [
        ("X1E", n(Bit), 0, Some("X1E")),
        ("X1E", n(Bit), 1, Some("X1F")),
        ("X1E", n(Bit), 2, Some("X20")),
        ("W1FF", one(), 0, Some("W1FF")),
        ("W1FF", one(), 1, Some("W200")),
        ("D100.E", n(Bit), 0, Some("D100.E")),
        ("D100.E", n(Bit), 1, Some("D100.F")),
        ("D100.E", n(Bit), 2, Some("D101.0")),
        ("D100", one(), 1, Some("D101")),
        ("D100", n(I32), 1, Some("D102")),
        ("D100", n(F64), 1, Some("D104")),
        ("D100", n(F64), 2, Some("D108")),
        (
            "D4000",
            TagKind::string(16, StringEncoding::Utf8),
            1,
            Some("D4016"),
        ),
        (
            "D4000",
            TagKind::string(0, StringEncoding::Utf8),
            1,
            Some("D4001"),
        ),
        ("D16777215", one(), 0, Some("D16777215")),
        ("D16777215", one(), 1, None),
        ("D16777215.0", n(Bit), 15, Some("D16777215.F")),
        ("D16777215.0", n(Bit), 16, None),
        ("M1A", one(), 0, None),
        ("ZZZ", one(), 0, None),
        ("40001", one(), 1, None),
        ("D3000", one(), 100, Some("D3100")),
        ("D3001", one(), 100, Some("D3101")),
        ("D3000", one(), 6, Some("D3006")),
        ("D16777200", one(), 100, None),
        ("D10", one(), -1, Some("D9")),
        ("D10", one(), -100, None),
        ("D10", one(), i64::MIN, None),
        ("D10", one(), i64::MAX, None),
    ] {
        f = f.offset(canonical, kind, step, expected);
    }
    f
}

#[test]
fn static_checks_pass() {
    assert_eq!(static_checks(&SlmpDriver, &statics()), vec![]);
}

/// The simulator's contents for [`fixtures`] (`word_order` is the column
/// default, `low_high`).
fn seed(sim: &Simulator) {
    sim.set_word(SlmpDevice::D, 100, 0x1234);
    // D110 as F32 = 1.5 (0x3FC0_0000), as U16 = its low word, 0.
    sim.set_words(SlmpDevice::D, 110, &[0x0000, 0x3FC0]);
    // D120 as I32 = -100000 (0xFFFE_7960).
    sim.set_words(SlmpDevice::D, 120, &[0x7960, 0xFFFE]);
    sim.set_word(SlmpDevice::D, 130, 0xFFFE);
    sim.set_word(SlmpDevice::D, 140, 0x0008);
    sim.set_string(SlmpDevice::D, 200, 4, "ABあ");
    sim.set_bit(SlmpDevice::M, 10, true);
    sim.set_bit(SlmpDevice::X, 0x1A, true);
    sim.inject_end_code(SlmpDevice::D, 500, 0xC051);
}

/// SLMP 4E responses (what the `slmp` crate validates): the end code is at
/// bytes 13-14, the data from byte 15. The response code is bytes 0-1
/// (`D4 00`).
const END_CODE_AT: usize = 13;
const DATA_AT: usize = 15;
/// "Device number out of range" - any non-zero end code is a refusal.
const REFUSAL: [u8; 2] = [0x51, 0xC0];

fn fixtures() -> SessionFixtures {
    use DataType::*;
    let sjis = |words| TagKind::string(words, StringEncoding::ShiftJis);
    SessionFixtures::new(statics())
        .read("D100", n(U16), Value::F64(4660.0))
        .read("D110", n(F32), Value::F64(1.5))
        .read("D110", n(U16), Value::F64(0.0))
        .read("D120", n(I32), Value::F64(-100_000.0))
        .read("D130", n(I16), Value::F64(-2.0))
        .read("D130", n(U16), Value::F64(65534.0))
        .read("D140.3", n(Bit), Value::Bit(true))
        .read("D140.4", n(Bit), Value::Bit(false))
        .read("M10", n(Bit), Value::Bit(true))
        .read("M11", n(Bit), Value::Bit(false))
        .read("X1A", n(Bit), Value::Bit(true))
        .read("D200", sjis(4), Value::Str("ABあ".into()))
        .refused("D500", n(U16))
        .unsupported("D100", n(Bit))
        .unsupported("M10", sjis(4))
        .write("D300", n(U16), Value::F64(321.0))
        .write("D310", n(F32), Value::F64(2.25))
        .write("M20", n(Bit), Value::Bit(true))
        .write("D320", sjis(4), Value::Str("あいう".into()))
        .unsupported_write("D100", n(Bit), Value::Bit(true))
        .unsupported_write("M10", sjis(4), Value::Str("x".into()))
        // The response code must be D4 00.
        .broken(ResponsePatch::new(0, 0, vec![0xD4, 0xFF]))
        // One wire bulk read serves up to 480 words.
        .max_string_words(480)
        .bit_write(
            BitWriteCase::new("D400", "D400.3", "D400.5")
                // The check read (3rd response: read, write-back, check) of
                // "bit 3 = bit 5 = true" on 0x0000 shows only bit 5.
                .verify_mismatch(ResponsePatch::new(2, DATA_AT, vec![0x20, 0x00]))
                .refusal(ResponsePatch::new(0, END_CODE_AT, REFUSAL.to_vec()))
                .refusal(ResponsePatch::new(1, END_CODE_AT, REFUSAL.to_vec()))
                .refusal(ResponsePatch::new(2, END_CODE_AT, REFUSAL.to_vec())),
        )
}

/// The frame-type driver in the engine, on `cpu` (module doc).
async fn frame_session_conforms(cpu: &str) {
    let sim = Simulator::start().await;
    seed(&sim);
    let proxy = FaultProxy::start(sim.addr).await.unwrap();
    let mut f = fixtures();
    f.statics.settings.insert("cpu_type", cpu);
    let failures = session::run_all_against(&EngineSession, &f, &proxy).await;
    let last = sim.last_subcommand();
    let text: Vec<String> = failures.iter().map(ToString::to_string).collect();
    assert!(failures.is_empty(), "{cpu}: {text:#?}");
    // The requests really were the series' (bit 1 of the subcommand is set
    // on R - `0002` / `0003` - and clear on Q - `0000` / `0001`).
    let want = if cpu == "Q" { 0x0000 } else { 0x0002 };
    assert_eq!(last.map(|s| s & 0x0002), Some(want), "{cpu}");
    sim.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn frame_session_conforms_on_r() {
    frame_session_conforms("R").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn frame_session_conforms_on_q() {
    frame_session_conforms("Q").await;
}

/// Pinned outside the harness (which also accepts an item `Unsupported`):
/// in the production `Session` an address that does not parse after connect is
/// the tag's own `ItemError::Unsupported`, not fatal (R2b-2a, address design
/// §6 Q5; design §3.4, §7.1 C4).
#[tokio::test]
async fn invalid_address_after_connect_is_unsupported_not_fatal() {
    let sim = Simulator::start().await;
    let settings = Settings::from_pairs([
        ("host", sim.addr.ip().to_string()),
        ("port", sim.addr.port().to_string()),
    ])
    .unwrap();
    let cx = ConnectContext::new("t", Duration::from_secs(2), Duration::from_secs(1));
    let mut s = EngineSession.connect(&settings, cx).await.unwrap();
    let foreign = DriverAddress::new(
        SlmpDriver.descriptor().id.clone(),
        "400001",
        "holding_register",
        true,
        1,
    );
    let r = s
        .read_batch(&[TagSpec::new(foreign, n(DataType::U16))])
        .await;
    assert!(
        matches!(r.as_deref(), Ok([Err(ItemError::Unsupported(_))])),
        "{r:?}"
    );
    sim.stop();
}
