//! R2b-3d（docs/banto-hub-driver-io-design.md §11.2）: the frame-type
//! `SlmpDriver` through `banto-driver-conformance`'s `frame::run_all`, which
//! runs the real `banto-driver-engine` over an in-memory wire on a paused
//! clock (C3-C7, C9, C11, C12: kinds and order, refusals, broken and split
//! responses, a late response, reconnects, a response that never ends,
//! random bytes, string spans). The device on the other end is `banto-plc`'s
//! SLMP simulator, answering request bytes with response bytes
//! (`Simulator::respond`), for the R and the Q series.
//!
//! `write_next` is the next PR, so the driver runs here as read-only
//! ([`ReadOnly`]); C10 and the frame-type C13-C15 come with it.

use banto_driver_api::{
    AddressError, Capabilities, DataType, DriverAddress, DriverBase, DriverDescriptor, FatalError,
    FieldError, FrameConnection, FrameDriver, RawValue, Settings, StringEncoding, TagKind,
};
use banto_driver_conformance::frame::{self, FrameFixtures, Reply, Responder};
use banto_driver_conformance::StaticFixtures;
use banto_driver_slmp::SlmpDriver;
use banto_plc::slmp::address::SlmpDevice;
use banto_plc::slmp::simulator::Simulator;

/// [`SlmpDriver`] with a read-only descriptor (module doc).
struct ReadOnly(SlmpDriver);

impl DriverBase for ReadOnly {
    fn descriptor(&self) -> &DriverDescriptor {
        static D: std::sync::OnceLock<DriverDescriptor> = std::sync::OnceLock::new();
        D.get_or_init(|| {
            let full = self.0.descriptor();
            DriverDescriptor::new(
                full.id.clone(),
                full.version.clone(),
                full.display_name.clone(),
                Capabilities::READ_ONLY,
            )
        })
    }
    fn validate_config(&self, settings: &Settings) -> Vec<FieldError> {
        self.0.validate_config(settings)
    }
    fn parse_address(&self, text: &str, kind: &TagKind) -> Result<DriverAddress, AddressError> {
        self.0.parse_address(text, kind)
    }
    fn offset_address(&self, c: &str, kind: &TagKind, n: i64) -> Result<String, AddressError> {
        self.0.offset_address(c, kind, n)
    }
    fn address_help(&self) -> String {
        self.0.address_help()
    }
    fn probe(&self) -> Option<(String, TagKind)> {
        self.0.probe()
    }
}

impl FrameDriver for ReadOnly {
    fn create_connection(
        &self,
        settings: &Settings,
    ) -> Result<Box<dyn FrameConnection>, FatalError> {
        self.0.create_connection(settings)
    }
}

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
        .unsupported("M10", sjis(4));
    // One bulk read serves up to 480 words (`plan_slmp_batch`).
    f.max_string_words = 480;
    f
}

async fn conforms(cpu: &str) {
    let sim = Simulator::start().await;
    seed(&sim);
    let mut device = Device(sim);
    let failures = frame::run_all(&ReadOnly(SlmpDriver), &fixtures(cpu), &mut device).await;
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
