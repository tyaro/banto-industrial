//! The harness's self-check (docs/banto-driver-api-design.md §10 R1-2):
//! the reference `EchoFrameDriver` passes `frame::run_all` under every
//! framing rule, and deliberately broken variants of it are caught by the
//! check that guards the broken contract.

use banto_driver_api::{
    AddressError, DriverAddress, DriverBase, DriverDescriptor, FatalError, FieldError, FrameCheck,
    FrameConnection, FrameDriver, ItemError, Planned, RawValue, Settings, Step, TagKind, TagSpec,
    WriteItem, WriteProgress,
};
use banto_driver_conformance::fake::echo::{
    self, EchoFrameDriver, EchoResponder, FRAMING_DECIDES, FRAMING_FIXED, FRAMING_PREFIXED,
};
use banto_driver_conformance::{frame, Failure};

#[tokio::test]
async fn the_echo_driver_conforms_under_every_framing() {
    for framing in [FRAMING_FIXED, FRAMING_PREFIXED, FRAMING_DECIDES] {
        let mut peer = EchoResponder::with_sample_data();
        let failures =
            frame::run_all(&EchoFrameDriver::new(), &echo::fixtures(framing), &mut peer).await;
        assert!(failures.is_empty(), "{framing}: {failures:#?}");
    }
}

#[tokio::test]
async fn the_echo_driver_conforms_with_low_high_word_order_data() {
    // Same fixtures, but the 32-bit words stored low word first and the
    // connection told so: the values must not change.
    let mut f = echo::fixtures(FRAMING_FIXED);
    f.statics
        .settings
        .insert(banto_driver_api::keys::WORD_ORDER, "low_high");
    let mut peer = EchoResponder::with_sample_data();
    for (a, b) in [(1u16, 2u16), (40, 41)] {
        let (wa, wb) = (peer.word('W', a).unwrap(), peer.word('W', b).unwrap());
        peer.set_word('W', a, wb);
        peer.set_word('W', b, wa);
    }
    let failures = frame::run_all(&EchoFrameDriver::new(), &f, &mut peer).await;
    assert!(failures.is_empty(), "{failures:#?}");
}

/// One deliberate contract break.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bug {
    /// Decodes an `F32` as its raw bits (ignores the spec's kind).
    FloatAsBits,
    /// `Done` with one result too few.
    ShortWriteResult,
    /// Reports a broken frame as `FatalError::Io` (a frame driver has no I/O).
    IoForBrokenFrames,
    /// Panics on a response shorter than its header.
    PanicOnShortResponse,
    /// Answers noise with `Discard(0)`.
    DiscardZero,
    /// Puts two items into one write step's `covers`.
    CoalescedCovers,
}

struct Buggy(EchoFrameDriver, Bug);

impl DriverBase for Buggy {
    fn descriptor(&self) -> &DriverDescriptor {
        self.0.descriptor()
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
}

impl FrameDriver for Buggy {
    fn create_connection(
        &self,
        settings: &Settings,
    ) -> Result<Box<dyn FrameConnection>, FatalError> {
        Ok(Box::new(BuggyConn(
            self.0.create_connection(settings)?,
            self.1,
        )))
    }
}

struct BuggyConn(Box<dyn FrameConnection>, Bug);

impl FrameConnection for BuggyConn {
    fn handshake_next(&mut self, prev: Option<&[u8]>) -> Result<Option<Step>, FatalError> {
        self.0.handshake_next(prev)
    }
    fn is_complete(&self, step: &Step, buf: &[u8]) -> Result<FrameCheck, FatalError> {
        match self.0.is_complete(step, buf)? {
            FrameCheck::Discard(_) if self.1 == Bug::DiscardZero => Ok(FrameCheck::Discard(0)),
            other => Ok(other),
        }
    }
    fn plan_read(&mut self, specs: &[TagSpec]) -> Result<Vec<Planned>, FatalError> {
        self.0.plan_read(specs)
    }
    fn decode_read(
        &self,
        planned: &Planned,
        specs: &[TagSpec],
        response: &[u8],
    ) -> Result<Vec<Result<RawValue, ItemError>>, FatalError> {
        if self.1 == Bug::PanicOnShortResponse && response.len() < 6 {
            panic!("deliberate: short response");
        }
        match self.0.decode_read(planned, specs, response) {
            Err(FatalError::Framing(m) | FatalError::Protocol(m))
                if self.1 == Bug::IoForBrokenFrames =>
            {
                Err(FatalError::Io(m))
            }
            Ok(values) if self.1 == Bug::FloatAsBits => Ok(values
                .into_iter()
                .map(|v| match v {
                    Ok(RawValue::Float(f)) => Ok(RawValue::Uint((f as f32).to_bits().into())),
                    other => other,
                })
                .collect()),
            other => other,
        }
    }
    fn write_next(
        &mut self,
        items: &[WriteItem],
        prev: Option<&[u8]>,
    ) -> Result<WriteProgress, FatalError> {
        match self.0.write_next(items, prev)? {
            WriteProgress::Done(mut r) if self.1 == Bug::ShortWriteResult => {
                r.pop();
                Ok(WriteProgress::Done(r))
            }
            WriteProgress::Send(p) if self.1 == Bug::CoalescedCovers => {
                let mut covers = p.covers.clone();
                covers.push((p.covers[0] + 1) % items.len() as u32);
                Ok(WriteProgress::Send(Planned::new(p.step, covers)))
            }
            other => Ok(other),
        }
    }
}

async fn failures_of(bug: Bug, framing: &str) -> Vec<Failure> {
    let mut peer = EchoResponder::with_sample_data();
    frame::run_all(
        &Buggy(EchoFrameDriver::new(), bug),
        &echo::fixtures(framing),
        &mut peer,
    )
    .await
}

fn assert_caught(bug: Bug, failures: &[Failure], id: &str, needle: &str) {
    assert!(
        failures
            .iter()
            .any(|f| f.id == id && f.detail.contains(needle)),
        "{bug:?} should fail {id} ({needle}): {failures:#?}"
    );
}

#[tokio::test]
async fn a_driver_that_ignores_the_kind_fails_c3() {
    let f = failures_of(Bug::FloatAsBits, FRAMING_FIXED).await;
    assert_caught(Bug::FloatAsBits, &f, "C3", "同じアドレス・別の型");
}

#[tokio::test]
async fn a_short_write_result_is_a_fatal_protocol_error_not_a_panic() {
    let f = failures_of(Bug::ShortWriteResult, FRAMING_FIXED).await;
    assert_caught(
        Bug::ShortWriteResult,
        &f,
        "C3",
        "書き込みの結果の件数が要求と合わない",
    );
    assert_caught(Bug::ShortWriteResult, &f, "C10", "書き込みが致命になった");
}

#[tokio::test]
async fn a_frame_driver_returning_io_fails_c4() {
    let f = failures_of(Bug::IoForBrokenFrames, FRAMING_FIXED).await;
    assert_caught(Bug::IoForBrokenFrames, &f, "C4", "Io");
}

#[tokio::test]
async fn a_panicking_decoder_fails_c11() {
    let f = failures_of(Bug::PanicOnShortResponse, FRAMING_FIXED).await;
    assert_caught(Bug::PanicOnShortResponse, &f, "C11", "decode_read が");
}

#[tokio::test]
async fn discard_zero_fails_c9() {
    let f = failures_of(Bug::DiscardZero, FRAMING_DECIDES).await;
    assert_caught(Bug::DiscardZero, &f, "C9", "Discard(0)");
}

#[tokio::test]
async fn coalesced_write_steps_fail_c10() {
    let f = failures_of(Bug::CoalescedCovers, FRAMING_FIXED).await;
    assert_caught(Bug::CoalescedCovers, &f, "C10", "1 つの step にまとめた");
}

#[tokio::test]
async fn a_responder_without_broken_frames_is_reported() {
    struct Healthy(EchoResponder);
    impl frame::Responder for Healthy {
        fn on_connect(&mut self) {
            self.0.on_connect();
        }
        fn reply(&mut self, request: &[u8]) -> frame::Reply {
            self.0.reply(request)
        }
    }
    let mut peer = Healthy(EchoResponder::with_sample_data());
    let f = frame::run_all(
        &EchoFrameDriver::new(),
        &echo::fixtures(FRAMING_FIXED),
        &mut peer,
    )
    .await;
    assert_eq!(f.len(), 1, "{f:#?}");
    assert_caught(Bug::FloatAsBits, &f, "C4", "reply_broken が None");
}

/// Drive one logical write by hand (the harness's engine is private).
fn write_by_hand(
    conn: &mut dyn FrameConnection,
    peer: &mut EchoResponder,
    items: &[WriteItem],
) -> (Vec<Result<(), ItemError>>, usize) {
    use frame::{Reply, Responder};
    let mut prev: Option<Vec<u8>> = None;
    let mut steps = 0;
    loop {
        match conn.write_next(items, prev.as_deref()).unwrap() {
            WriteProgress::Send(p) => {
                steps += 1;
                match peer.reply(&p.step.send) {
                    Reply::Bytes(b) => prev = Some(b),
                    other => panic!("{other:?}"),
                }
            }
            WriteProgress::Done(r) => return (r, steps),
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn the_word_bit_read_modify_write_keeps_the_other_bits() {
    use banto_driver_api::{DataType, Value};
    use frame::{Reply, Responder};
    let driver = EchoFrameDriver::new();
    let settings = echo::fixtures(FRAMING_FIXED).statics.settings;
    let bit = |text: &str, on: bool| {
        let kind = TagKind::numeric(DataType::Bit);
        WriteItem::new(
            TagSpec::new(driver.parse_address(text, &kind).unwrap(), kind),
            Value::Bit(on),
        )
    };
    for (current, item_bits, want) in [
        (0x0000u16, vec![("W7.3", true)], 0x0008u16),
        (0x0100, vec![("W7.3", true)], 0x0108),
        (0xFFFF, vec![("W7.3", false)], 0xFFF7),
        (
            0x0008,
            vec![("W7.3", true), ("W7.0", true), ("W7.15", false)],
            0x0009,
        ),
    ] {
        let mut peer = EchoResponder::new();
        peer.set_word('W', 7, current);
        peer.on_connect();
        let mut conn = driver.create_connection(&settings).unwrap();
        let hello = conn.handshake_next(None).unwrap().unwrap();
        let Reply::Bytes(ack) = peer.reply(&hello.send) else {
            panic!()
        };
        assert_eq!(conn.handshake_next(Some(&ack)).unwrap(), None);

        let items: Vec<WriteItem> = item_bits.iter().map(|(t, on)| bit(t, *on)).collect();
        let (results, steps) = write_by_hand(&mut *conn, &mut peer, &items);
        assert!(results.iter().all(Result::is_ok), "{results:?}");
        assert_eq!(
            steps, 3,
            "one read-modify-write (read, write, verify) for one word"
        );
        assert_eq!(
            peer.word('W', 7),
            Some(want),
            "0x{current:04X} -> {item_bits:?}"
        );
    }
}

#[test]
fn the_same_bit_set_and_cleared_is_rejected_without_touching_the_word() {
    use banto_driver_api::{DataType, Value};
    use frame::{Reply, Responder};
    let driver = EchoFrameDriver::new();
    let mut peer = EchoResponder::new();
    peer.set_word('W', 7, 0x0100);
    peer.on_connect();
    let mut conn = driver
        .create_connection(&echo::fixtures(FRAMING_FIXED).statics.settings)
        .unwrap();
    let hello = conn.handshake_next(None).unwrap().unwrap();
    let Reply::Bytes(ack) = peer.reply(&hello.send) else {
        panic!()
    };
    conn.handshake_next(Some(&ack)).unwrap();
    let kind = TagKind::numeric(DataType::Bit);
    let spec = TagSpec::new(driver.parse_address("W7.3", &kind).unwrap(), kind);
    let items = [
        WriteItem::new(spec.clone(), Value::Bit(true)),
        WriteItem::new(spec, Value::Bit(false)),
    ];
    let (results, steps) = write_by_hand(&mut *conn, &mut peer, &items);
    assert!(
        results
            .iter()
            .all(|r| matches!(r, Err(ItemError::Rejected(_)))),
        "{results:?}"
    );
    assert_eq!(steps, 0);
    assert_eq!(peer.word('W', 7), Some(0x0100));
}
