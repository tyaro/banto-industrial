//! R2b-3d（docs/banto-hub-driver-io-design.md §11.2）: the frame-type
//! `SlmpDriver` through `banto-driver-conformance`'s `frame::run_all`, which
//! runs the real `banto-driver-engine` over an in-memory wire on a paused
//! clock (C3-C7, C9-C12: kinds and order, refusals, broken and split
//! responses, a late response, reconnects, a response that never ends,
//! random bytes, string spans) and, since R2b-3d-2 the driver writes, C10
//! (writes that are not adjacent are not coalesced, and read back - a word,
//! a 32-bit float, a bit device, a string and a bit in a word through the
//! read-modify-write). The device on the other end is `banto-plc`'s SLMP
//! simulator, answering request bytes with response bytes
//! (`Simulator::respond`), for the R and the Q series.
//!
//! C13-C15 (the read-modify-write's other bits, refusals at each step and
//! the check read's mismatch) run over real TCP through the engine in
//! `tests/conformance.rs`, and on the driver alone in `src/frame_conn.rs`.

use banto_driver_api::{DataType, RawValue, Settings, StringEncoding, TagKind, Value};
use banto_driver_conformance::frame::{self, FrameFixtures, Reply, Responder};
use banto_driver_conformance::StaticFixtures;
use banto_driver_slmp::SlmpDriver;
use banto_plc::slmp::address::SlmpDevice;
use banto_plc::slmp::simulator::Simulator;

/// The simulator as the harness's device. A broken reply is the good one with
/// its subheader corrupted (`D4 FF`), as `Simulator::emit_malformed_frames`.
struct Device(Simulator);

impl Responder for Device {
    fn reply(&mut self, request: &[u8]) -> Reply {
        Reply::Bytes(self.0.respond(request))
    }

    fn reply_broken(&mut self, request: &[u8]) -> Option<Reply> {
        let mut bytes = self.0.respond(request);
        bytes[1] = 0xFF;
        Some(Reply::Bytes(bytes))
    }
}

fn n(data_type: DataType) -> TagKind {
    TagKind::numeric(data_type)
}

fn seed(sim: &Simulator) {
    sim.set_word(SlmpDevice::D, 100, 0x1234);
    // D110 as F32 = 1.5 (0x3FC0_0000, low word first), as U16 = 0.
    sim.set_words(SlmpDevice::D, 110, &[0x0000, 0x3FC0]);
    // D120 as I32 = -100000 (0xFFFE_7960).
    sim.set_words(SlmpDevice::D, 120, &[0x7960, 0xFFFE]);
    sim.set_word(SlmpDevice::D, 130, 0xFFFE);
    sim.set_word(SlmpDevice::D, 140, 0x0008);
    sim.set_string(SlmpDevice::D, 200, 4, "ABあ");
    sim.set_bit(SlmpDevice::M, 10, true);
    sim.set_bit(SlmpDevice::X, 0x1A, true);
    sim.set_word(SlmpDevice::ZR, 0x00FF_FFFF, 0x0102);
    sim.inject_end_code(SlmpDevice::D, 500, 0xC051);
}

fn fixtures(cpu: &str) -> FrameFixtures {
    use DataType::*;
    let sjis = |words| TagKind::string(words, StringEncoding::ShiftJis);
    let settings = Settings::from_pairs([
        ("host", "192.168.0.10"),
        ("port", "5000"),
        ("cpu_type", cpu),
    ])
    .unwrap();
    let mut f = FrameFixtures::new(StaticFixtures::new(settings))
        .read("D100", n(U16), RawValue::Uint(0x1234))
        .read("D110", n(F32), RawValue::Float(1.5))
        .read("D110", n(U16), RawValue::Uint(0))
        .read("D120", n(I32), RawValue::Int(-100_000))
        .read("D130", n(I16), RawValue::Int(-2))
        .read("D130", n(U16), RawValue::Uint(65534))
        .read("D140.3", n(Bit), RawValue::Bool(true))
        .read("D140.4", n(Bit), RawValue::Bool(false))
        .read("M10", n(Bit), RawValue::Bool(true))
        .read("M11", n(Bit), RawValue::Bool(false))
        .read("X1A", n(Bit), RawValue::Bool(true))
        .read("ZR16777215", n(U16), RawValue::Uint(0x0102))
        // "ABあ" in Shift_JIS, low byte first per word.
        .read(
            "D200",
            sjis(4),
            RawValue::Bytes(vec![0x41, 0x42, 0x82, 0xA0]),
        )
        .refused("D500", n(U16))
        .unsupported("D100", n(Bit))
        .unsupported("M10", sjis(4))
        .write("D300", n(U16), Value::F64(321.0), RawValue::Uint(321))
        .write("D310", n(F32), Value::F64(2.25), RawValue::Float(2.25))
        .write("M20", n(Bit), Value::Bit(true), RawValue::Bool(true))
        // "AB" padded with NUL to the 2 words, low byte first.
        .write(
            "D330",
            sjis(2),
            Value::Str("AB".into()),
            RawValue::Bytes(vec![0x41, 0x42, 0x00, 0x00]),
        )
        // D140 holds 0x0008: the RMW sets bit 0 and keeps bit 3.
        .write("D140.0", n(Bit), Value::Bit(true), RawValue::Bool(true));
    // One bulk read serves up to 480 words (`plan_slmp_batch`).
    f.max_string_words = 480;
    f
}

async fn conforms(cpu: &str) {
    let sim = Simulator::start().await;
    seed(&sim);
    let mut device = Device(sim);
    let failures = frame::run_all(&SlmpDriver, &fixtures(cpu), &mut device).await;
    let last = device.0.last_subcommand();
    device.0.stop();
    let text: Vec<String> = failures.iter().map(ToString::to_string).collect();
    assert!(failures.is_empty(), "{cpu}: {text:#?}");
    // The requests really were the series' (bit 1 of the subcommand is set
    // on R - `0002` / `0003` - and clear on Q - `0000` / `0001`).
    let want = if cpu == "Q" { 0x0000 } else { 0x0002 };
    assert_eq!(last.map(|s| s & 0x0002), Some(want & 0x0002), "{cpu}");
}

#[tokio::test]
async fn the_frame_driver_conforms_on_r() {
    conforms("R").await;
}

#[tokio::test]
async fn the_frame_driver_conforms_on_q() {
    conforms("Q").await;
}
