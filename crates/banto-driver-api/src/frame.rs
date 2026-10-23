//! Frame-type drivers: the driver builds and parses bytes, the host's engine
//! owns the transport (docs/banto-driver-api-design.md §4.2, §4.2.1; the
//! native mirror of WIT §6.2's `connection` / `read` / `write`).
//!
//! A frame-type driver never touches a socket. The engine sends each
//! [`Step::send`], receives according to [`Step::expect`] and hands the
//! response back. Because I/O is the engine's, **a frame-type driver must
//! not return [`FatalError::Io`] or [`FatalError::Timeout`]** (conformance
//! C4); those arise in the engine.
//!
//! ## The read procedure (engine; R1's conformance harness implements the same)
//!
//! 1. [`FrameConnection::plan_read`] turns the specs into [`Planned`] steps.
//! 2. For each step: send `step.send`, then receive by `step.expect`
//!    ([`FrameRule::None`]: do not receive; [`FrameRule::DriverDecides`]:
//!    call [`FrameConnection::is_complete`] on every arrival, dropping the
//!    first `n` bytes on [`FrameCheck::Discard`]).
//! 3. A response longer than `max_len`, or a response timeout, is fatal: the
//!    engine discards its receive buffer and drops the connection, so a late
//!    response can never be taken as the next step's (C6).
//! 4. [`FrameConnection::decode_read`] with the **same** `specs` slice that
//!    was planned (the driver tells `U32` from `F32` at one address by
//!    `specs[covers[i]].kind`).
//! 5. Results are put back in `specs` order by `covers`; an index no step
//!    covers becomes [`ItemError::Unsupported`](crate::ItemError).
//!
//! ## The write procedure (§4.2.1)
//!
//! Writing is a state machine driven by [`FrameConnection::write_next`]:
//! the first call gets `prev_response = None`, every later call the previous
//! step's response, until the driver answers [`WriteProgress::Done`] with
//! one result per item. This is what a word-bit read-modify-write needs (the
//! value written back depends on the value read). The engine caps the number
//! of steps (about `3 × items.len() + 8`), and a `Done` whose length differs
//! from `items.len()` is a driver contract violation that the engine turns
//! into [`FatalError::Protocol`] - never a panic.

use crate::driver::DriverBase;
use crate::error::{FatalError, ItemError};
use crate::settings::Settings;
use crate::types::{RawValue, TagSpec, WriteItem};

/// How the engine knows a response is complete (WIT `session.frame-rule`).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
pub enum FrameRule {
    /// No response (a broadcast, etc.): the engine moves on right after
    /// sending.
    None,
    /// Exactly this many bytes.
    Fixed(u32),
    /// A length field inside the response gives the total length.
    LengthPrefixed(PrefixSpec),
    /// The response ends with these bytes (e.g. a host-link CR).
    Terminator(Vec<u8>),
    /// The engine calls [`FrameConnection::is_complete`] on every arrival;
    /// growing beyond [`Step::max_len`] is fatal (WIT `plugin-decides`).
    DriverDecides,
}

/// How a length field is written (WIT `prefix-encoding`). An SLMP ASCII
/// frame has its length as four ASCII hex digits (design §8).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
pub enum PrefixEncoding {
    /// Binary, `width` bytes, in the byte order of [`PrefixSpec::big_endian`].
    Binary,
    /// `width` ASCII hexadecimal digits.
    AsciiHex,
    /// `width` ASCII decimal digits.
    AsciiDec,
}

/// Where and how the length field is (WIT `prefix-spec`): bytes
/// `offset..offset + width` of the response, read with `encoding`, plus
/// `adjust`, is the response's total length.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PrefixSpec {
    /// The field's first byte.
    pub offset: u32,
    /// The field's length in bytes (characters for the ASCII encodings).
    pub width: u8,
    /// Byte order of a [`PrefixEncoding::Binary`] field.
    pub big_endian: bool,
    /// Added to the field's value to get the total length (usually the
    /// header length the field does not count).
    pub adjust: i32,
    /// How the field is written.
    pub encoding: PrefixEncoding,
}

impl PrefixSpec {
    /// A binary length field (`encoding = Binary`).
    pub fn new(offset: u32, width: u8, big_endian: bool, adjust: i32) -> Self {
        PrefixSpec {
            offset,
            width,
            big_endian,
            adjust,
            encoding: PrefixEncoding::Binary,
        }
    }

    /// The same spec with `encoding` replaced.
    pub fn with_encoding(self, encoding: PrefixEncoding) -> Self {
        PrefixSpec { encoding, ..self }
    }
}

/// One request and how to receive its response (WIT `session.step`).
///
/// The response timeout is the connection's `response_timeout_ms` setting,
/// not a field here; a per-step turnaround can be added later because the
/// struct is `#[non_exhaustive]` (design §8).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Step {
    /// The bytes to send.
    pub send: Vec<u8>,
    /// How to know the response is complete.
    pub expect: FrameRule,
    /// The longest response the engine accepts; beyond it is fatal.
    pub max_len: u32,
}

impl Step {
    /// A step sending `send` and receiving by `expect`, up to `max_len`
    /// bytes.
    pub fn new(send: Vec<u8>, expect: FrameRule, max_len: u32) -> Self {
        Step {
            send,
            expect,
            max_len,
        }
    }
}

/// A step plus the request indices it serves (WIT `read.planned` /
/// `write.planned-write`).
///
/// For a read, `covers` are indices into the `specs` given to
/// [`FrameConnection::plan_read`], and [`FrameConnection::decode_read`]
/// returns one result per entry, in `covers` order. For a write, they are
/// indices into the `items` given to [`FrameConnection::write_next`], for
/// logs and audit only.
/// Write `covers` must not contain duplicate indices within a step (the same
/// index may appear in different steps). A violation is a fatal C10 contract
/// violation: the engine returns [`FatalError::Protocol`] and drops the connection.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Planned {
    /// What to send and how to receive.
    pub step: Step,
    /// Which requests it serves.
    pub covers: Vec<u32>,
}

impl Planned {
    /// `step` serving `covers`.
    pub fn new(step: Step, covers: Vec<u32>) -> Self {
        Planned { step, covers }
    }
}

/// [`FrameConnection::is_complete`]'s answer (WIT `frame-check`: complete,
/// not yet, or "the first n bytes are noise").
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
pub enum FrameCheck {
    /// Not complete yet; keep receiving.
    Incomplete,
    /// The first `len` bytes (`1..=buf.len()`) are one response. The rest is
    /// discarded, never carried into the next step.
    Complete {
        /// The response's length.
        len: u32,
    },
    /// Drop the first `n` bytes (`1..=buf.len()`) and continue (RS-485 self
    /// echo, the rest of an earlier response, line noise).
    Discard(u32),
}

/// [`FrameConnection::write_next`]'s answer (design §4.2.1).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
pub enum WriteProgress {
    /// The next step to send. Its `covers` are the item indices it touches
    /// (for logs and audit; results come with [`Done`](Self::Done)).
    Send(Planned),
    /// The end of this logical write: one result per item, same length and
    /// order as `items`. `write_next` is not called again for this write.
    Done(Vec<Result<(), ItemError>>),
}

/// A request's signature on a serial line (io-design §7.3, 2026-10-23): the
/// driver's own encoding of "what a response to this request looks like"
/// (Modbus RTU: unit id, function code, the normal response's byte count;
/// an exception response is implied by unit id + function code). The engine
/// never reads the bytes: it remembers signatures of requests whose
/// response has not arrived, compares them for equality, hands them back to
/// [`FrameConnection::sync_step`] and asks
/// [`FrameConnection::response_matches`] which request a received frame
/// answers. Two requests that could be answered by the same bytes must have
/// the same signature; the engine refuses a sync step whose signature equals
/// an outstanding one (C17).
#[derive(Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Signature(Vec<u8>);

impl Signature {
    /// A signature from the driver's own bytes.
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Signature(bytes.into())
    }

    /// The driver's bytes, as given.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl std::fmt::Debug for Signature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Signature({:02X?})", self.0)
    }
}

/// A frame-type driver (design §4.2). Makes one [`FrameConnection`] per
/// connection.
pub trait FrameDriver: DriverBase {
    /// Start a connection's protocol state from its settings (WIT
    /// `connection.create`). Word order and the like are read here (design
    /// §4.2: never per request). No I/O happens: the engine opens the
    /// transport and then runs [`FrameConnection::handshake_next`].
    fn create_connection(
        &self,
        settings: &Settings,
    ) -> Result<Box<dyn FrameConnection>, FatalError>;
}

/// One connection's protocol state (WIT `resource connection` + `read` +
/// `write`). Owned by one host task (`Send`, not `Sync`), which serializes
/// every call.
///
/// Methods that change state take `&mut self`: `handshake_next` /
/// `farewell` (connection state), `plan_read` (transaction numbering shared
/// by reads and writes) and `write_next` (numbering and the write in
/// progress). `decode_read` and `is_complete` take `&self`.
pub trait FrameConnection: Send + 'static {
    /// The handshake, one step at a time (WIT `handshake-next`). First call
    /// `None`, then the previous step's response; return `Ok(None)` when
    /// done. A refusal is `Err` (the connection is not considered up).
    fn handshake_next(&mut self, prev_response: Option<&[u8]>) -> Result<Option<Step>, FatalError>;

    /// A step to send right before disconnecting (EtherNet/IP's
    /// UnRegisterSession, etc.). Its response is not awaited. Default: none.
    fn farewell(&mut self) -> Option<Step> {
        None
    }

    /// The minimum silence the engine keeps on the wire between frames
    /// (design §7.3.1). Measured from the last byte of the previous frame,
    /// sent or received, whichever is later, to the first byte of the next
    /// send. Modbus RTU returns 3.5 character times (1.750 ms above 19200
    /// baud). Default: zero (no gap, as for TCP). A value above the engine's
    /// cap (500 ms) is a contract violation and is refused before any send.
    fn inter_frame_gap(&self) -> std::time::Duration {
        std::time::Duration::ZERO
    }

    /// The [`Signature`] of `step`'s request (io-design §7.3, 2026-10-23).
    /// On a serial line (`transports` with `Serial` / `SerialOverTcp`) the
    /// engine records it before sending and keeps it while the response is
    /// outstanding; a driver that names a serial transport must return
    /// `Some` for every step it sends with a response (`None` is a C17
    /// contract violation there). Default: `None` (TCP drivers need none).
    fn signature(&self, _step: &Step) -> Option<Signature> {
        None
    }

    /// The request the engine sends to resynchronise a serial line (io-design
    /// §7.3 step 2) after a timeout, a framing error or a cancelled step,
    /// and once when the line is first opened. `outstanding` are the
    /// signatures of requests whose response never arrived (may be empty).
    /// Return a **read** (never a write) whose normal and exception responses
    /// can be told apart from a response to any of them (Modbus RTU: the
    /// `probe()` point with a different function code or a different word
    /// count than any outstanding request), or `None` when no such request is
    /// left. A driver without a sync procedure keeps the default (`None`):
    /// then the engine never reuses a serial line after a fatal error
    /// (§7.3 step 6). The engine decides "has a procedure" by calling this
    /// with an empty slice, so the choice must not depend on earlier calls
    /// and must not change the connection's state when it returns `None`.
    ///
    /// The engine splits **every** frame that arrives during the sync,
    /// including late responses to outstanding requests, with this step's
    /// `expect`. So the sync step's `expect` must be a rule that delimits a
    /// response from the response itself: [`FrameRule::LengthPrefixed`],
    /// [`FrameRule::Terminator`] or [`FrameRule::DriverDecides`].
    /// [`FrameRule::Fixed`] cannot be used (C17; the engine refuses it). With
    /// Called right after `create_connection`, **before** the handshake (the
    /// engine decides whether the line has a sync procedure by `sync_step(&[])`
    /// there), so it must not depend on state the handshake builds (C17).
    /// `DriverDecides`, [`FrameConnection::is_complete`] called with the sync
    /// step must delimit the first frame of its input at the right length
    /// for the response (normal or exception) to **any** request that can be
    /// sent on that line, not only for the sync request's own response.
    /// The sync step's `max_len` must be no shorter than **any** response
    /// that can flow on that line (handshake, read and write responses; C17,
    /// 2026-10-23): a late response longer than it is still delimited and
    /// skipped by its own length (up to the engine's cap), but it is never
    /// accepted as the sync response.
    fn sync_step(&mut self, _outstanding: &[Signature]) -> Option<Step> {
        None
    }

    /// Whether the complete frame `response` answers the request with
    /// `signature` (a normal or an exception response to it). The engine
    /// uses it to tell the sync step's response from a late response to an
    /// outstanding request, and after synchronisation to refuse a response
    /// that answers a different request (`FatalError::Framing`). Default:
    /// `false`.
    fn response_matches(&self, _signature: &Signature, _response: &[u8]) -> bool {
        false
    }

    /// Whether `buf` holds a complete response to `step` (WIT
    /// `is-complete`). Called only for [`FrameRule::DriverDecides`].
    fn is_complete(&self, step: &Step, buf: &[u8]) -> Result<FrameCheck, FatalError>;

    /// Plan the steps that read `specs` (WIT `read.plan`).
    fn plan_read(&mut self, specs: &[TagSpec]) -> Result<Vec<Planned>, FatalError>;

    /// Decode one step's response (WIT `read.decode`). `specs` is the very
    /// slice given to [`plan_read`](Self::plan_read); the result has
    /// `planned.covers.len()` entries in `covers` order, each interpreted by
    /// `specs[covers[i]].kind` (one frame may be read as `U32` and as `F32`).
    fn decode_read(
        &self,
        planned: &Planned,
        specs: &[TagSpec],
        response: &[u8],
    ) -> Result<Vec<Result<RawValue, ItemError>>, FatalError>;

    /// The write state machine (WIT `write.write-next`, design §4.2.1).
    ///
    /// One logical write (all of `items`) starts with
    /// `prev_response = None` - the driver must reset any state left by an
    /// earlier, interrupted write - and continues with each step's response
    /// until [`WriteProgress::Done`]. `items` is the same slice throughout.
    /// Not called when `capabilities.write` is false; the default refuses
    /// with [`FatalError::Protocol`].
    fn write_next(
        &mut self,
        _items: &[WriteItem],
        _prev_response: Option<&[u8]>,
    ) -> Result<WriteProgress, FatalError> {
        Err(FatalError::Protocol(
            "このドライバは書き込みに対応していません".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_spec_defaults_to_binary() {
        let p = PrefixSpec::new(4, 2, true, 6);
        assert_eq!(p.offset, 4);
        assert_eq!(p.width, 2);
        assert!(p.big_endian);
        assert_eq!(p.adjust, 6);
        assert_eq!(p.encoding, PrefixEncoding::Binary);
        let p = p.with_encoding(PrefixEncoding::AsciiHex);
        assert_eq!(p.encoding, PrefixEncoding::AsciiHex);
        assert_eq!(p.offset, 4);
    }

    #[test]
    fn step_and_planned_constructors() {
        let step = Step::new(vec![1, 2], FrameRule::Fixed(8), 64);
        assert_eq!(step.send, vec![1, 2]);
        assert_eq!(step.expect, FrameRule::Fixed(8));
        assert_eq!(step.max_len, 64);
        let planned = Planned::new(step.clone(), vec![0, 3]);
        assert_eq!(planned.step, step);
        assert_eq!(planned.covers, vec![0, 3]);
    }

    struct ReadOnly;

    impl FrameConnection for ReadOnly {
        fn handshake_next(
            &mut self,
            _prev_response: Option<&[u8]>,
        ) -> Result<Option<Step>, FatalError> {
            Ok(None)
        }
        fn is_complete(&self, _step: &Step, buf: &[u8]) -> Result<FrameCheck, FatalError> {
            Ok(FrameCheck::Complete {
                len: buf.len() as u32,
            })
        }
        fn plan_read(&mut self, _specs: &[TagSpec]) -> Result<Vec<Planned>, FatalError> {
            Ok(Vec::new())
        }
        fn decode_read(
            &self,
            _planned: &Planned,
            _specs: &[TagSpec],
            _response: &[u8],
        ) -> Result<Vec<Result<RawValue, ItemError>>, FatalError> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn defaults_refuse_writes_and_have_no_farewell() {
        let mut c: Box<dyn FrameConnection> = Box::new(ReadOnly);
        assert_eq!(c.farewell(), None);
        let step = Step::new(vec![1], FrameRule::Fixed(1), 1);
        assert_eq!(c.signature(&step), None);
        assert_eq!(c.sync_step(&[]), None);
        assert!(!c.response_matches(&Signature::new([1u8]), &[1]));
        assert_eq!(
            format!("{:?}", Signature::new(vec![1, 0xAB])),
            "Signature([01, AB])"
        );
        assert_eq!(Signature::new([7u8]).as_bytes(), &[7]);
        assert!(matches!(
            c.write_next(&[], None),
            Err(FatalError::Protocol(_))
        ));
    }
}

#[cfg(all(test, feature = "serde"))]
mod serde_tests {
    use super::*;
    use crate::error::{DeviceError, ItemError};

    #[test]
    fn frame_types_round_trip_through_json() {
        let planned = Planned::new(
            Step::new(
                vec![0xEC, b'R'],
                FrameRule::LengthPrefixed(
                    PrefixSpec::new(4, 2, true, 6).with_encoding(PrefixEncoding::AsciiHex),
                ),
                262,
            ),
            vec![0, 2],
        );
        let json = serde_json::to_string(&planned).unwrap();
        assert!(json.contains(r#""ascii_hex""#), "{json}");
        assert_eq!(serde_json::from_str::<Planned>(&json).unwrap(), planned);

        let done = WriteProgress::Done(vec![
            Ok(()),
            Err(ItemError::Device(DeviceError::new(2, "x"))),
        ]);
        let json = serde_json::to_string(&done).unwrap();
        assert_eq!(serde_json::from_str::<WriteProgress>(&json).unwrap(), done);

        let check = FrameCheck::Complete { len: 7 };
        let json = serde_json::to_string(&check).unwrap();
        assert_eq!(json, r#"{"complete":{"len":7}}"#);
        assert_eq!(serde_json::from_str::<FrameCheck>(&json).unwrap(), check);
    }
}
