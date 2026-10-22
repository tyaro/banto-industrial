//! The conformance checks of `banto-driver-conformance` against the R1
//! Modbus TCP driver (docs/banto-driver-api-design.md §7.3, R1-5):
//!
//! - `static_checks` (C1, C2) on [`ModbusDriver`], with fixtures copied from
//!   `banto-plc/src/address.rs`'s tests (the grammar) and the TypeScript
//!   `continuousRegistration.test.ts` / `offsetCopy.test.ts` (offsets).
//! - `session::run_all_against` (C1-C8, C12) through the dev-only
//!   `LegacySession` (`tests/common`), against `banto-plc`'s Modbus TCP
//!   simulator behind a `FaultProxy`. C13-C15 have no fixtures: Modbus has
//!   no bit-in-word writes (`banto-plc-write/src/modbus/planning.rs`), so a
//!   `.N` write is pinned as `ItemError::Unsupported` instead.
//!
//! Every simulator and proxy binds 127.0.0.1:0, so these tests need no
//! nextest test-group.

mod common;

use std::time::Duration;

use banto_driver_api::{
    ConnectContext, DataType, DriverAddress, DriverBase, ItemError, SessionDriver, Settings,
    StringEncoding, TagKind, TagSpec, Value, WriteItem,
};
use banto_driver_conformance::proxy::FaultProxy;
use banto_driver_conformance::session::{self, ResponsePatch, SessionFixtures};
use banto_driver_conformance::{static_checks, StaticFixtures};
use banto_driver_modbus::ModbusDriver;
use banto_plc::modbus::simulator::Simulator;
use common::LegacyDriver;

fn n(data_type: DataType) -> TagKind {
    TagKind::numeric(data_type)
}

fn statics() -> StaticFixtures {
    use DataType::*;
    let one = || n(I16);
    let mut f = StaticFixtures::new(
        Settings::from_pairs([("host", "192.168.0.10"), ("port", "502")]).unwrap(),
    );
    // banto-plc/src/address.rs: every area, 5 and 6 digits, trimming,
    // bit-in-word (the review's promised `40001.3`), Display = canonical.
    for (text, kind, canonical) in [
        ("00001", n(Bit), "000001"),
        ("10001", n(Bit), "100001"),
        ("30001", one(), "300001"),
        ("40001", one(), "400001"),
        ("40010", one(), "400010"),
        ("49999", one(), "409999"),
        ("400001", one(), "400001"),
        ("410000", one(), "410000"),
        ("465536", one(), "465536"),
        ("  40001  ", one(), "400001"),
        ("40001.3", n(Bit), "400001.3"),
        ("40001.03", n(Bit), "400001.3"),
        ("400001.3", n(Bit), "400001.3"),
        ("30001.15", n(Bit), "300001.15"),
        ("40001", n(F32), "400001"),
        ("40001", n(U64), "400001"),
    ] {
        f = f.valid(text, kind, canonical);
    }
    // R2b-2a (address design §3.2 Q4): refused for the tag's kind although
    // the text itself is a valid reference number.
    for (text, kind) in [
        ("40001.3", n(U16)),
        ("40001.3", n(F32)),
        ("00001", n(F32)),
        ("00001", n(U16)),
        ("10001", n(I32)),
        ("40001", n(Bit)),
        ("30001", n(Bit)),
        ("40001", TagKind::string(16, StringEncoding::Utf8)),
    ] {
        f = f.invalid(text, kind);
    }
    for text in [
        "",
        "   ",
        "  bogus  ",
        "D100",
        "M50",
        "X1A",
        "D40001",
        "20001",
        "50001",
        "60001",
        "70001",
        "80001",
        "90001",
        "40000",
        "400000",
        "465537",
        "4001",
        "4000001",
        "4000A",
        "40001.16",
        "00001.0",
        "10001.0",
        "00001.3",
        "40001.",
        "40001.3.4",
    ] {
        f = f.invalid(text, one());
    }
    // continuousRegistration.test.ts / offsetCopy.test.ts (Modbus reference
    // numbers), plus the bounds base.rs pins.
    for (canonical, kind, step, expected) in [
        ("400001", one(), 1, Some("400002")),
        ("400001", n(I64), 1, Some("400005")),
        ("400001", n(I64), 2, Some("400009")),
        ("400001", one(), 100, Some("400101")),
        ("400101", n(F32), 1, Some("400103")),
        ("400101", n(F64), 1, Some("400105")),
        (
            "400101",
            TagKind::string(16, StringEncoding::Utf8),
            1,
            Some("400117"),
        ),
        (
            "400101",
            TagKind::string(0, StringEncoding::Utf8),
            1,
            Some("400102"),
        ),
        ("400101", one(), -100, Some("400001")),
        ("300010", one(), 5, Some("300015")),
        ("000010", n(Bit), 5, Some("000015")),
        ("100010", one(), -9, Some("100001")),
        ("465535", one(), 1, Some("465536")),
        ("400001.3", n(Bit), 0, Some("400001.3")),
        ("400001", one(), -1, None),
        ("465536", one(), 1, None),
        ("400001", one(), i64::MAX, None),
        ("400001", one(), i64::MIN, None),
        ("ZZZ", one(), 1, None),
        ("D100", one(), 1, None),
        ("400001.3", n(Bit), 1, None),
    ] {
        f = f.offset(canonical, kind, step, expected);
    }
    f
}

#[test]
fn static_checks_pass() {
    assert_eq!(static_checks(&ModbusDriver, &statics()), vec![]);
}

/// FC3 at offset 500 (`40501`) is refused with "illegal data address".
const REFUSED_OFFSET: u16 = 500;

/// The simulator's contents for [`fixtures`] (`word_order` is the column
/// default, `low_high`: the low word of a 32-bit value comes first).
fn seed(sim: &Simulator) {
    sim.set_holding_register(10, 0x1234);
    // 40021 as F32 = 1.5 (0x3FC0_0000), as U16 = its low word, 0.
    sim.set_holding_registers(20, &[0x0000, 0x3FC0]);
    // 40031 as I32 = -100000 (0xFFFE_7960).
    sim.set_holding_registers(30, &[0x7960, 0xFFFE]);
    sim.set_holding_register(40, 0xFFFE);
    sim.set_holding_register(50, 0x0008);
    sim.set_coil(0, true);
    sim.set_discrete_input(0, true);
    sim.set_input_register(0, 777);
    sim.inject_exception(0x03, REFUSED_OFFSET, 0x02);
}

fn fixtures() -> SessionFixtures {
    use DataType::*;
    SessionFixtures::new(statics())
        .read("40011", n(U16), Value::F64(4660.0))
        .read("40021", n(F32), Value::F64(1.5))
        .read("40021", n(U16), Value::F64(0.0))
        .read("40031", n(I32), Value::F64(-100_000.0))
        .read("40041", n(I16), Value::F64(-2.0))
        .read("40041", n(U16), Value::F64(65534.0))
        .read("40051.3", n(Bit), Value::Bit(true))
        .read("40051.4", n(Bit), Value::Bit(false))
        .read("00001", n(Bit), Value::Bit(true))
        .read("10001", n(Bit), Value::Bit(true))
        .read("30001", n(U16), Value::F64(777.0))
        .refused("40501", n(U16))
        .unsupported("40011", n(Bit))
        .unsupported("00001", n(U16))
        .unsupported("40011", TagKind::string(4, StringEncoding::ShiftJis))
        .write("40101", n(U16), Value::F64(321.0))
        .write("40111", n(F32), Value::F64(2.25))
        .write("00011", n(Bit), Value::Bit(true))
        // No bit-in-word writes on Modbus (C13-C15 have no fixtures).
        .unsupported_write("40051.3", n(Bit), Value::Bit(true))
        .unsupported_write("30001", n(U16), Value::F64(1.0))
        // The MBAP protocol id (bytes 2-3) must be 0.
        .broken(ResponsePatch::new(0, 2, vec![0x12, 0x34]))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_session_conforms() {
    let sim = Simulator::start().await;
    seed(&sim);
    let proxy = FaultProxy::start(sim.addr).await.unwrap();
    let failures = session::run_all_against(&LegacyDriver, &fixtures(), &proxy).await;
    let text: Vec<String> = failures.iter().map(ToString::to_string).collect();
    assert!(failures.is_empty(), "{text:#?}");
    sim.stop();
}

fn cx() -> ConnectContext {
    ConnectContext::new("t", Duration::from_secs(2), Duration::from_secs(1))
}

/// Pinned outside the harness (which also accepts an item `Unsupported`):
/// in R1's `LegacySession` an address that does not parse after connect is
/// the tag's own `ItemError::Unsupported`, not fatal (R2b-2a, address design
/// §6 Q5; design §3.4, §7.1 C4), and a `.N` write is
/// `ItemError::Unsupported` (design §7.3).
#[tokio::test]
async fn invalid_address_is_unsupported_and_bit_write_is_unsupported() {
    let sim = Simulator::start().await;
    let settings = Settings::from_pairs([
        ("host", sim.addr.ip().to_string()),
        ("port", sim.addr.port().to_string()),
    ])
    .unwrap();
    let mut s = LegacyDriver.connect(&settings, cx()).await.unwrap();
    let bit = n(DataType::Bit);
    let spec = TagSpec::new(LegacyDriver.parse_address("40051.3", &bit).unwrap(), bit);
    let r = s
        .write_batch(&[WriteItem::new(spec, Value::Bit(true))])
        .await
        .unwrap();
    assert!(
        matches!(r.as_slice(), [Err(ItemError::Unsupported(_))]),
        "{r:?}"
    );
    let foreign = DriverAddress::new(LegacyDriver.descriptor().id.clone(), "D100", "D", true, 1);
    let r = s
        .read_batch(&[TagSpec::new(foreign, n(DataType::U16))])
        .await;
    assert!(
        matches!(r.as_deref(), Ok([Err(ItemError::Unsupported(_))])),
        "{r:?}"
    );
    sim.stop();
}
