//! Run the conformance harness against the reference frame-type driver
//! (docs/banto-driver-api-design.md §4.3).
//!
//! `cargo run -p banto-driver-conformance --example echo_driver`
//!
//! The driver itself is `banto_driver_conformance::fake::echo` (read its
//! source as the model for writing a frame-type driver: `#[non_exhaustive]`
//! API types are built with their constructors, and matches on them end in
//! `_`). A third-party driver is checked the same way, with its own
//! `frame::Responder` and fixtures, in a `#[tokio::test]`.

use banto_driver_conformance::fake::echo::{
    self, EchoFrameDriver, EchoResponder, FRAMING_DECIDES, FRAMING_FIXED, FRAMING_PREFIXED,
};
use banto_driver_conformance::frame;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let driver = EchoFrameDriver::new();
    let mut total = 0;
    for framing in [FRAMING_FIXED, FRAMING_PREFIXED, FRAMING_DECIDES] {
        let mut peer = EchoResponder::with_sample_data();
        let failures = frame::run_all(&driver, &echo::fixtures(framing), &mut peer).await;
        println!("framing = {framing}: {} failure(s)", failures.len());
        for f in &failures {
            println!("  {f}");
        }
        total += failures.len();
    }
    if total > 0 {
        std::process::exit(1);
    }
}
