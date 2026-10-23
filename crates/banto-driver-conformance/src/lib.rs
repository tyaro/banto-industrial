//! banto-driver-conformance: check a [`banto_driver_api`] driver without a
//! Hub (docs/banto-driver-api-design.md §7).
//!
//! A driver written against `banto-driver-api` - in this repository or in
//! a third party's - proves here that it keeps the contracts the Hub's
//! engine relies on: canonical addresses, per-item vs connection-fatal
//! errors, one result per request in request order, split and late
//! responses, reconnecting without carrying old state, bounded receives,
//! non-coalesced writes, and no panics on arbitrary bytes. Each contract has
//! a number (C1-C16, design §7.1; C17-C22 are the static checks of the
//! settings schema, the kind list and the address position, R2b-6a); a run returns every [`Failure`] it found
//! rather than stopping at the first, so one test run shows them all.
//!
//! ## How a third party uses it
//!
//! Add it as a dev-dependency next to `banto-driver-api`, give it a peer
//! that answers like your device, and assert that nothing failed:
//!
//! ```no_run
//! use banto_driver_conformance::fake::echo::{self, EchoFrameDriver, EchoResponder};
//! use banto_driver_conformance::frame;
//!
//! # async fn conforms() {
//! // Your driver and a `frame::Responder` that answers like your device;
//! // here, the reference echo driver of this crate.
//! let driver = EchoFrameDriver::new();
//! let mut peer = EchoResponder::with_sample_data();
//! let failures = frame::run_all(&driver, &echo::fixtures(echo::FRAMING_FIXED), &mut peer).await;
//! assert!(failures.is_empty(), "{failures:#?}");
//! # }
//! ```
//!
//! In a test, wrap that in `#[tokio::test] async fn conforms() { .. }`.
//!
//! - [`static_checks`] (C1, C2): address parsing / offsets / settings
//!   validation of any [`DriverBase`](banto_driver_api::DriverBase). No I/O, no runtime.
//! - [`frame::run_all`] (C1-C7, C9-C12): a frame-type driver
//!   ([`FrameDriver`](banto_driver_api::FrameDriver)) against a
//!   [`frame::Responder`]. The harness runs the production FrameSession
//!   (design §4.2, §4.2.1) in memory with virtual time, so it is deterministic and fast;
//!   faults (split, late, broken, endless, noisy, closed responses) are
//!   injected by the harness, not by the responder.
//! - [`fake`]: drivers to test against or to copy from:
//!   [`EchoFrameDriver`](fake::echo::EchoFrameDriver), the reference
//!   frame-type driver (fixed-length frames, a handshake, a word-bit
//!   read-modify-write) with its peer, and
//!   [`FakeSessionDriver`](fake::session::FakeSessionDriver), a session-type
//!   driver over an in-memory map whose next call can be made fatal.
//! - [`session::run_all_against`] (C1-C8, C12-C15): a session-type driver
//!   ([`SessionDriver`](banto_driver_api::SessionDriver)) against a peer that
//!   is already a TCP server (a device simulator), through a
//!   [`proxy::FaultProxy`]. The harness plays the host (one session, a
//!   fatal error drops it) in real time with bounded waits; the faults
//!   (split, held back, rewritten and cut-off responses, dropped
//!   connections) are injected by the proxy.
//! - [`proxy::FaultProxy`]: a TCP relay on 127.0.0.1 that splits, delays,
//!   holds back, rewrites and cuts off responses, drops connections and
//!   counts accepts - for session-type drivers whose peer is a real TCP
//!   server (a simulator).
//!
//! A session-type driver's test looks like this (R1-5 runs the Modbus / SLMP
//! drivers this way, design §7.3):
//!
//! ```no_run
//! # use banto_driver_api::SessionDriver;
//! use banto_driver_conformance::proxy::FaultProxy;
//! use banto_driver_conformance::session::{self, SessionFixtures};
//!
//! # async fn conforms(driver: &dyn SessionDriver, fixtures: SessionFixtures, peer: std::net::SocketAddr) {
//! // `peer`: your device simulator, already listening; `fixtures`: what it holds.
//! let proxy = FaultProxy::start(peer).await.unwrap();
//! let failures = session::run_all_against(driver, &fixtures, &proxy).await;
//! assert!(failures.is_empty(), "{failures:#?}");
//! # }
//! ```
//!
//! ## Writing fixtures
//!
//! The fixture types are `#[non_exhaustive]` so that new checks can add
//! fields without breaking you: start from `new` / `Default` and use the
//! builder methods (or push to the public `Vec`s).

#![deny(missing_docs)]
#![forbid(unsafe_code)]

use std::fmt;

use banto_driver_api::{KindName, Settings, TagKind};

pub mod fake;
pub mod frame;
pub mod proxy;
pub mod session;
mod statics;

pub use statics::static_checks;

/// One broken contract: which check ([`id`](Self::id), `"C1"`-`"C22"`) and
/// what was observed.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// The check's number in design §7.1 (`"C3"` etc.).
    pub id: &'static str,
    /// What went wrong, for a person (Japanese, like the rest of the
    /// project's messages).
    pub detail: String,
}

impl Failure {
    /// A failure of check `id`.
    pub fn new(id: &'static str, detail: impl Into<String>) -> Self {
        Failure {
            id,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.id, self.detail)
    }
}

/// Inputs for [`static_checks`] (C1, C2), supplied by the driver's author.
#[non_exhaustive]
#[derive(Debug, Clone, Default)]
pub struct StaticFixtures {
    /// The smallest valid connection settings. C2 expects no error for them;
    /// [`frame::run_all`] also connects with them. A key listed by
    /// [`DriverBase::secret_keys`](banto_driver_api::DriverBase::secret_keys) must appear here (that is how C2 knows
    /// the driver reads it).
    pub settings: Settings,
    /// `(text, kind, expected canonical)`: addresses the grammar accepts.
    pub valid: Vec<(String, TagKind, String)>,
    /// `(text, kind)`: addresses `parse_address` must refuse.
    pub invalid: Vec<(String, TagKind)>,
    /// `(canonical, kind, n, expected)`: `offset_address(canonical, kind, n)`
    /// must give `Some(expected)`, or fail when `expected` is `None`.
    pub offsets: Vec<(String, TagKind, i64, Option<String>)>,
    /// `(kind name, address text)`: C20 parses the text as that kind. It must
    /// succeed exactly when the kind is in
    /// [`supported_kinds`](banto_driver_api::DriverBase::supported_kinds). When
    /// this list is not empty, every supported kind needs an entry.
    pub kind_examples: Vec<(KindName, String)>,
    /// `(canonical, n, expected)`: `offset_words(canonical, n)` must give
    /// `Some(expected)`, or fail when `expected` is `None` (C21). Empty for a
    /// driver without word-wise offsets.
    pub word_offsets: Vec<(String, i64, Option<String>)>,
}

impl StaticFixtures {
    /// Fixtures with the driver's minimal valid `settings` and no cases.
    pub fn new(settings: Settings) -> Self {
        StaticFixtures {
            settings,
            ..Self::default()
        }
    }

    /// Add an accepted address and its canonical form.
    pub fn valid(mut self, text: &str, kind: TagKind, canonical: &str) -> Self {
        self.valid
            .push((text.to_owned(), kind, canonical.to_owned()));
        self
    }

    /// Add an address the grammar must refuse.
    pub fn invalid(mut self, text: &str, kind: TagKind) -> Self {
        self.invalid.push((text.to_owned(), kind));
        self
    }

    /// Add an offset case (`expected = None`: the offset must fail).
    pub fn offset(
        mut self,
        canonical: &str,
        kind: TagKind,
        n: i64,
        expected: Option<&str>,
    ) -> Self {
        self.offsets
            .push((canonical.to_owned(), kind, n, expected.map(str::to_owned)));
        self
    }

    /// Add a C20 case: `text` is an address to try `kind` on.
    pub fn kind_example(mut self, kind: impl Into<KindName>, text: &str) -> Self {
        self.kind_examples.push((kind.into(), text.to_owned()));
        self
    }

    /// Add a C21 case: `offset_words(canonical, n)` gives `expected` (`None`:
    /// it must fail).
    pub fn word_offset(mut self, canonical: &str, n: i64, expected: Option<&str>) -> Self {
        self.word_offsets
            .push((canonical.to_owned(), n, expected.map(str::to_owned)));
        self
    }
}
