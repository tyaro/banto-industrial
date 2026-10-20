//! Drivers to test against, or to copy from (docs/banto-driver-api-design.md
//! §4.3, §7.4).
//!
//! - [`echo`]: [`EchoFrameDriver`](echo::EchoFrameDriver), the reference
//!   frame-type driver, and [`EchoResponder`](echo::EchoResponder), the
//!   device it talks to. The harness's own self-check runs it through
//!   [`frame::run_all`](crate::frame::run_all) under every framing rule it
//!   supports.
//! - [`session`]: [`FakeSessionDriver`](session::FakeSessionDriver), a
//!   session-type driver over an in-memory map whose next call can be made
//!   connection-fatal - for host tests that need "a driver" without a
//!   device (R2b-0's core tests, R2a's `push` before it exists).

pub mod echo;
pub mod session;
