//! R2b-3c（docs/banto-hub-driver-io-design.md §11.2）: the frame-type
//! `ModbusDriver` through `banto-driver-conformance`'s `frame::run_all`, which
//! runs the real `banto-driver-engine` over an in-memory wire on a paused
//! clock (C3-C7, C9-C12: kinds and order, refusals, broken and split
//! responses, a late response, reconnects, a response that never ends, random
//! bytes, string spans) and, since the driver writes, C10 (writes that are not
//! adjacent are not coalesced, and read back). The device on the other end is
//! `banto-plc`'s Modbus simulator, answering request bytes with response bytes
//! (`Simulator::respond`).
//!
//! C13-C15 (bit-in-word writes) have no frame fixtures: Modbus has none
//! (`ItemError::Unsupported`, pinned by the unit tests in `frame_conn.rs` and
//! by `tests/conformance.rs` for the session).

use banto_driver_api::{DataType, RawValue, Settings, StringEncoding, TagKind, Value};
use banto_driver_conformance::frame::{self, FrameFixtures, Reply, Responder};
use banto_driver_conformance::StaticFixtures;
use banto_driver_modbus::ModbusDriver;
use banto_plc::modbus::simulator::Simulator;

/// The simulator as the harness's device. A broken reply is the good one with
/// its MBAP protocol id (bytes 2-3, always 0) corrupted.
struct Device(Simulator);

impl Responder for Device {
    fn reply(&mut self, request: &[u8]) -> Reply {
        Reply::Bytes(self.0.respond(request))
    }

    fn reply_broken(&mut self, request: &[u8]) -> Option<Reply> {
        let mut bytes = self.0.respond(request);
        bytes[2..4].copy_from_slice(&[0x12, 0x34]);
        Some(Reply::Bytes(bytes))
    }
}

fn n(data_type: DataType) -> TagKind {
    TagKind::numeric(data_type)
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

fn fixtures() -> FrameFixtures {
    use DataType::*;
    let settings = Settings::from_pairs([("host", "192.168.0.10"), ("port", "502")]).unwrap();
    FrameFixtures::new(StaticFixtures::new(settings))
        .read("40011", n(U16), RawValue::Uint(0x1234))
        .read("40021", n(F32), RawValue::Float(1.5))
        .read("40021", n(U16), RawValue::Uint(0))
        .read("40031", n(I32), RawValue::Int(-100_000))
        .read("40041", n(I16), RawValue::Int(-2))
        .read("40041", n(U16), RawValue::Uint(65534))
        .read("40051.3", n(Bit), RawValue::Bool(true))
        .read("40051.4", n(Bit), RawValue::Bool(false))
        .read("00001", n(Bit), RawValue::Bool(true))
        .read("10001", n(Bit), RawValue::Bool(true))
        .read("30001", n(U16), RawValue::Uint(777))
        .refused("40501", n(U16))
        .unsupported("40011", n(Bit))
        .unsupported("00001", n(U16))
        .unsupported("40011", TagKind::string(4, StringEncoding::ShiftJis))
        // Pairwise non-adjacent (C10).
        .write("40101", n(U16), Value::F64(321.0), RawValue::Uint(321))
        .write("40111", n(F32), Value::F64(2.25), RawValue::Float(2.25))
        .write("00011", n(Bit), Value::Bit(true), RawValue::Bool(true))
}

#[tokio::test]
async fn the_frame_driver_conforms() {
    let sim = Simulator::start().await;
    seed(&sim);
    let mut device = Device(sim);
    let failures = frame::run_all(&ModbusDriver, &fixtures(), &mut device).await;
    // C10 really wrote: the three writes reached the device.
    let written = device.0.write_command_count();
    device.0.stop();
    let text: Vec<String> = failures.iter().map(ToString::to_string).collect();
    assert!(failures.is_empty(), "{text:#?}");
    assert!(written >= 3, "{written} write commands");
}
