//! 記憶上の通信路で、期限・契約違反・致命後の破棄を実通信と同じ実行系に通す。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use banto_driver_api::{
    AddressError, Capabilities, DataType, DriverAddress, DriverBase, DriverDescriptor, DriverId,
    FieldError, Planned, StringEncoding, TagKind,
};

use super::*;

const T: Duration = Duration::from_millis(100);

#[derive(Default)]
struct Stats {
    sends: AtomicUsize,
    closes: AtomicUsize,
    transport_drops: AtomicUsize,
    conn_drops: AtomicUsize,
    writes: AtomicUsize,
}

#[derive(Default)]
struct Recorder(Mutex<Vec<EngineEvent>>);
impl EngineEvents for Recorder {
    fn record(&self, event: EngineEvent) {
        self.0.lock().unwrap().push(event);
    }
}
impl Recorder {
    fn has(&self, check: &str) -> bool {
        self.0
            .lock()
            .unwrap()
            .iter()
            .any(|event| matches!(event, EngineEvent::Violation { check: c, .. } if *c == check))
    }
}

struct Wire {
    stats: Arc<Stats>,
    send_delay: Duration,
    recv_delay: Duration,
    chunks: VecDeque<Vec<u8>>,
    pending_send: bool,
    pending_close: bool,
    zero_recv: bool,
}
impl Wire {
    fn new(stats: Arc<Stats>) -> Self {
        Self {
            stats,
            send_delay: Duration::ZERO,
            recv_delay: Duration::ZERO,
            chunks: VecDeque::new(),
            pending_send: false,
            pending_close: false,
            zero_recv: false,
        }
    }
}
impl Drop for Wire {
    fn drop(&mut self) {
        self.stats.transport_drops.fetch_add(1, Ordering::SeqCst);
    }
}
impl Transport for Wire {
    fn send<'a>(&'a mut self, _bytes: &'a [u8]) -> BoxFuture<'a, Result<(), FatalError>> {
        Box::pin(async move {
            self.stats.sends.fetch_add(1, Ordering::SeqCst);
            if self.pending_send {
                std::future::pending::<()>().await;
            }
            tokio::time::sleep(self.send_delay).await;
            Ok(())
        })
    }
    fn recv<'a>(&'a mut self, buf: &'a mut Vec<u8>) -> BoxFuture<'a, Result<usize, FatalError>> {
        Box::pin(async move {
            if self.zero_recv {
                return Ok(0);
            }
            tokio::time::sleep(self.recv_delay).await;
            let bytes = self.chunks.pop_front().unwrap_or_else(|| vec![7]);
            let n = bytes.len();
            buf.extend(bytes);
            Ok(n)
        })
    }
    fn discard_input(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async {
            panic!("TCP の致命後は破棄せず閉じる");
        })
    }
    fn close(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            self.stats.closes.fetch_add(1, Ordering::SeqCst);
            if self.pending_close {
                std::future::pending::<()>().await;
            }
        })
    }
}

#[derive(Clone)]
struct Conn {
    stats: Arc<Stats>,
    step: Step,
    plan: Option<Vec<Planned>>,
    raw: RawValue,
    decode_count: Option<usize>,
    handshake_steps: usize,
    handshake_sent: usize,
    write_steps: usize,
    write_sent: usize,
    done_count: Option<usize>,
    write_covers: Vec<u32>,
    check: Option<FrameCheck>,
    error: Option<(&'static str, FatalError)>,
    farewell: bool,
}
impl Conn {
    fn new(stats: Arc<Stats>) -> Self {
        Self {
            stats,
            step: Step::new(vec![1], FrameRule::Fixed(1), 32),
            plan: None,
            raw: RawValue::Uint(7),
            decode_count: None,
            handshake_steps: 0,
            handshake_sent: 0,
            write_steps: 1,
            write_sent: 0,
            done_count: None,
            write_covers: vec![0],
            check: None,
            error: None,
            farewell: false,
        }
    }
    fn fail(&self, method: &str) -> Result<(), FatalError> {
        match &self.error {
            Some((m, e)) if *m == method => Err(e.clone()),
            _ => Ok(()),
        }
    }
}
impl Drop for Conn {
    fn drop(&mut self) {
        self.stats.conn_drops.fetch_add(1, Ordering::SeqCst);
    }
}
impl FrameConnection for Conn {
    fn handshake_next(&mut self, _previous: Option<&[u8]>) -> Result<Option<Step>, FatalError> {
        self.fail("handshake_next")?;
        if self.handshake_sent == self.handshake_steps {
            return Ok(None);
        }
        self.handshake_sent += 1;
        Ok(Some(self.step.clone()))
    }
    fn farewell(&mut self) -> Option<Step> {
        self.farewell.then(|| self.step.clone())
    }
    fn is_complete(&self, _step: &Step, _buf: &[u8]) -> Result<FrameCheck, FatalError> {
        self.fail("is_complete")?;
        Ok(self.check.unwrap_or(FrameCheck::Incomplete))
    }
    fn plan_read(&mut self, specs: &[TagSpec]) -> Result<Vec<Planned>, FatalError> {
        self.fail("plan_read")?;
        Ok(self.plan.clone().unwrap_or_else(|| {
            vec![Planned::new(
                self.step.clone(),
                (0..specs.len() as u32).collect(),
            )]
        }))
    }
    fn decode_read(
        &self,
        planned: &Planned,
        _specs: &[TagSpec],
        _response: &[u8],
    ) -> Result<Vec<Result<RawValue, ItemError>>, FatalError> {
        self.fail("decode_read")?;
        Ok(vec![
            Ok(self.raw.clone());
            self.decode_count.unwrap_or(planned.covers.len())
        ])
    }
    fn write_next(
        &mut self,
        items: &[WriteItem],
        previous: Option<&[u8]>,
    ) -> Result<WriteProgress, FatalError> {
        self.stats.writes.fetch_add(1, Ordering::SeqCst);
        self.fail("write_next")?;
        if previous.is_none() {
            self.write_sent = 0;
        }
        if self.write_sent == self.write_steps {
            return Ok(WriteProgress::Done(vec![
                Ok(());
                self.done_count
                    .unwrap_or(items.len())
            ]));
        }
        self.write_sent += 1;
        Ok(WriteProgress::Send(Planned::new(
            self.step.clone(),
            self.write_covers.clone(),
        )))
    }
}

struct Driver {
    descriptor: DriverDescriptor,
    conn: Conn,
}
impl Driver {
    fn new(conn: Conn, can_write: bool) -> Self {
        let mut capabilities = Capabilities::READ_ONLY;
        capabilities.write = can_write;
        Self {
            descriptor: DriverDescriptor::new(
                DriverId::new("test").unwrap(),
                "0.1.0",
                "試験",
                capabilities,
            ),
            conn,
        }
    }
}
impl DriverBase for Driver {
    fn descriptor(&self) -> &DriverDescriptor {
        &self.descriptor
    }
    fn validate_config(&self, _s: &Settings) -> Vec<FieldError> {
        Vec::new()
    }
    fn parse_address(&self, text: &str, kind: &TagKind) -> Result<DriverAddress, AddressError> {
        Ok(DriverAddress::new(
            self.descriptor.id.clone(),
            text,
            "a",
            true,
            kind.word_count(),
        ))
    }
    fn offset_address(&self, text: &str, _kind: &TagKind, _n: i64) -> Result<String, AddressError> {
        Ok(text.into())
    }
    fn address_help(&self) -> String {
        String::new()
    }
}
impl FrameDriver for Driver {
    fn create_connection(&self, _s: &Settings) -> Result<Box<dyn FrameConnection>, FatalError> {
        self.conn.fail("create_connection")?;
        Ok(Box::new(self.conn.clone()))
    }
}

fn spec() -> TagSpec {
    TagSpec::new(
        DriverAddress::new(DriverId::new("test").unwrap(), "x", "a", true, 1),
        TagKind::numeric(DataType::U16),
    )
}
fn item() -> WriteItem {
    WriteItem::new(spec(), Value::F64(7.0))
}
fn session(conn: Conn, wire: Wire, events: Arc<Recorder>) -> FrameSession {
    FrameSession {
        core: Some(Core {
            conn: Box::new(conn),
            transport: Box::new(wire),
            response_timeout: T,
            can_write: true,
            events: Some(events),
        }),
    }
}

#[tokio::test(start_paused = true)]
async fn send_and_receive_share_one_deadline() {
    for (send, recv, succeeds, elapsed) in [(45, 50, true, 95), (60, 45, false, 100)] {
        let stats = Arc::<Stats>::default();
        let mut wire = Wire::new(stats.clone());
        wire.send_delay = Duration::from_millis(send);
        wire.recv_delay = Duration::from_millis(recv);
        let mut s = session(Conn::new(stats.clone()), wire, Arc::default());
        let start = Instant::now();
        let result = s.read_batch(&[spec()]).await;
        assert_eq!(result.is_ok(), succeeds, "{result:?}");
        assert_eq!(Instant::now() - start, Duration::from_millis(elapsed));
        assert_eq!(s.is_connected(), succeeds);
        if !succeeds {
            assert_eq!(result, Err(FatalError::Timeout));
            assert_eq!(stats.closes.load(Ordering::SeqCst), 1);
            assert_eq!(stats.transport_drops.load(Ordering::SeqCst), 1);
            assert_eq!(stats.conn_drops.load(Ordering::SeqCst), 1);
        }
    }
}

#[tokio::test(start_paused = true)]
async fn pending_send_and_close_have_finite_limits() {
    let stats = Arc::<Stats>::default();
    let mut wire = Wire::new(stats.clone());
    wire.pending_send = true;
    wire.pending_close = true;
    let mut s = session(Conn::new(stats.clone()), wire, Arc::default());
    let start = Instant::now();
    assert_eq!(s.write_batch(&[item()]).await, Err(FatalError::Timeout));
    assert_eq!(Instant::now() - start, T + CLOSE_TIMEOUT);
    assert_eq!(stats.transport_drops.load(Ordering::SeqCst), 1);
    assert_eq!(stats.conn_drops.load(Ordering::SeqCst), 1);
    assert!(s.write_batch(&[item()]).await.is_err());
    assert_eq!(stats.sends.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn cancelling_an_operation_drops_transport_and_driver_and_disallows_reuse() {
    for writing in [false, true] {
        let stats = Arc::<Stats>::default();
        let mut wire = Wire::new(stats.clone());
        wire.recv_delay = Duration::from_secs(1);
        let mut s = session(Conn::new(stats.clone()), wire, Arc::default());
        if writing {
            assert!(timeout(Duration::from_millis(5), s.write_batch(&[item()]))
                .await
                .is_err());
        } else {
            assert!(timeout(Duration::from_millis(5), s.read_batch(&[spec()]))
                .await
                .is_err());
        }
        assert!(!s.is_connected());
        assert_eq!(stats.transport_drops.load(Ordering::SeqCst), 1);
        assert_eq!(stats.conn_drops.load(Ordering::SeqCst), 1);
        assert!(s.read_batch(&[spec()]).await.is_err());
        assert_eq!(stats.sends.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test(start_paused = true)]
async fn write_step_budget_allows_slow_steps_and_rejects_an_extra_step() {
    for extra in [false, true] {
        let stats = Arc::<Stats>::default();
        let events = Arc::<Recorder>::default();
        let mut conn = Conn::new(stats.clone());
        conn.write_steps = 11 + usize::from(extra);
        let mut wire = Wire::new(stats.clone());
        wire.send_delay = Duration::from_millis(45);
        wire.recv_delay = Duration::from_millis(50);
        let mut s = session(conn, wire, events.clone());
        let outer_limit = T * 11 + Duration::from_secs(1);
        let result = timeout(outer_limit, s.write_batch(&[item()]))
            .await
            .unwrap();
        assert_eq!(result.is_ok(), !extra);
        assert_eq!(stats.sends.load(Ordering::SeqCst), 11);
        assert_eq!(events.has("C16"), extra);
    }
}

#[tokio::test(start_paused = true)]
async fn read_step_budget_is_checked_before_sending() {
    for count in [9, 10] {
        let stats = Arc::<Stats>::default();
        let events = Arc::<Recorder>::default();
        let mut conn = Conn::new(stats.clone());
        conn.plan = Some(
            (0..count)
                .map(|i| Planned::new(conn.step.clone(), if i == 0 { vec![0] } else { vec![] }))
                .collect(),
        );
        let mut s = session(conn, Wire::new(stats.clone()), events.clone());
        assert_eq!(s.read_batch(&[spec()]).await.is_ok(), count == 9);
        assert_eq!(
            stats.sends.load(Ordering::SeqCst),
            if count == 9 { 9 } else { 0 }
        );
        assert_eq!(events.has("C9"), count == 10);
    }
}

#[tokio::test(start_paused = true)]
async fn handshake_allows_32_steps_and_discards_an_endless_connection() {
    for count in [32, 33] {
        let stats = Arc::<Stats>::default();
        let events = Arc::<Recorder>::default();
        let mut conn = Conn::new(stats.clone());
        conn.handshake_steps = count;
        let driver = Driver::new(conn, true);
        let result = FrameSession::with_transport(
            &driver,
            &Settings::new(),
            Box::new(Wire::new(stats.clone())),
            T,
            Some(events.clone()),
        )
        .await;
        assert_eq!(result.is_ok(), count == 32);
        assert_eq!(stats.sends.load(Ordering::SeqCst), 32);
        assert_eq!(events.has("C7"), count == 33);
        if count == 33 {
            assert_eq!(stats.closes.load(Ordering::SeqCst), 1);
        }
    }
}

#[tokio::test(start_paused = true)]
async fn read_only_driver_does_not_enter_the_write_state_machine() {
    let stats = Arc::<Stats>::default();
    let driver = Driver::new(Conn::new(stats.clone()), false);
    let mut s = FrameSession::with_transport(
        &driver,
        &Settings::new(),
        Box::new(Wire::new(stats.clone())),
        T,
        None,
    )
    .await
    .unwrap();
    assert!(matches!(
        s.write_batch(&[item()]).await.unwrap().as_slice(),
        [Err(ItemError::Unsupported(_))]
    ));
    assert_eq!(stats.writes.load(Ordering::SeqCst), 0);
    assert_eq!(stats.sends.load(Ordering::SeqCst), 0);
    assert!(s.is_connected());
}

#[tokio::test(start_paused = true)]
async fn farewell_sends_without_receiving_and_disconnect_is_bounded() {
    for pending_send in [false, true] {
        let stats = Arc::<Stats>::default();
        let mut conn = Conn::new(stats.clone());
        conn.farewell = true;
        let mut wire = Wire::new(stats.clone());
        wire.pending_send = pending_send;
        wire.pending_close = true;
        let mut s = session(conn, wire, Arc::default());
        s.core.as_mut().unwrap().response_timeout = Duration::from_secs(30);
        let start = Instant::now();
        s.disconnect().await;
        assert!(!s.is_connected());
        assert_eq!(
            Instant::now() - start,
            CLOSE_TIMEOUT
                + if pending_send {
                    Duration::from_secs(1)
                } else {
                    Duration::ZERO
                }
        );
        assert_eq!(stats.sends.load(Ordering::SeqCst), 1);
        assert_eq!(stats.transport_drops.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test(start_paused = true)]
async fn covers_counts_and_completion_checks_remain_fatal_violations() {
    for case in 0..9 {
        let stats = Arc::<Stats>::default();
        let events = Arc::<Recorder>::default();
        let mut conn = Conn::new(stats.clone());
        let writing = case >= 6;
        let check = match case {
            0 => {
                conn.plan = Some(vec![Planned::new(conn.step.clone(), vec![1])]);
                "C3"
            }
            1 => {
                conn.plan = Some(vec![Planned::new(conn.step.clone(), vec![0, 0])]);
                "C3"
            }
            2 => {
                conn.decode_count = Some(0);
                "C3"
            }
            3..=5 => {
                conn.step.expect = FrameRule::DriverDecides;
                conn.check = Some(match case {
                    3 => FrameCheck::Discard(0),
                    4 => FrameCheck::Discard(2),
                    _ => FrameCheck::Complete { len: 2 },
                });
                "C9"
            }
            6 => {
                conn.write_covers = vec![1];
                "C10"
            }
            7 => {
                conn.write_covers = vec![0, 0];
                "C10"
            }
            _ => {
                conn.done_count = Some(0);
                "C3"
            }
        };
        let mut s = session(conn, Wire::new(stats.clone()), events.clone());
        let error = if writing {
            s.write_batch(&[item()]).await.unwrap_err()
        } else {
            s.read_batch(&[spec()]).await.unwrap_err()
        };
        assert!(
            matches!(error, FatalError::Protocol(_)),
            "case {case}: {error:?}"
        );
        assert!(events.has(check), "case {case}: {check}");
        assert!(!s.is_connected());
        assert_eq!(stats.closes.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test(start_paused = true)]
async fn mismatched_raw_values_are_per_item_unsupported_and_keep_the_connection() {
    for raw in [
        RawValue::Bool(true),
        RawValue::Int(7),
        RawValue::Uint(7),
        RawValue::Float(7.0),
    ] {
        let stats = Arc::<Stats>::default();
        let mut conn = Conn::new(stats.clone());
        conn.raw = raw.clone();
        let mut bit = spec();
        bit.kind = TagKind::numeric(DataType::Bit);
        let mut s = session(conn, Wire::new(stats.clone()), Arc::default());
        for _ in 0..2 {
            let values = s.read_batch(&[bit.clone(), spec()]).await.unwrap();
            if matches!(raw, RawValue::Bool(_)) {
                assert_eq!(values[0], Ok(Value::Bit(true)));
                assert!(matches!(&values[1], Err(ItemError::Unsupported(_))));
            } else {
                assert!(matches!(&values[0], Err(ItemError::Unsupported(_))));
                assert_eq!(values[1], Ok(Value::F64(7.0)));
            }
            assert!(s.is_connected());
        }
        assert_eq!(stats.sends.load(Ordering::SeqCst), 2);
        assert_eq!(stats.closes.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test(start_paused = true)]
async fn missing_covers_are_per_item_unsupported_and_keep_the_connection() {
    let stats = Arc::<Stats>::default();
    let mut conn = Conn::new(stats.clone());
    conn.plan = Some(Vec::new());
    let mut s = session(conn, Wire::new(stats), Arc::default());
    assert!(matches!(
        s.read_batch(&[spec()]).await.unwrap().as_slice(),
        [Err(ItemError::Unsupported(_))]
    ));
    assert!(s.is_connected());
}

#[tokio::test(start_paused = true)]
async fn all_frame_methods_report_io_and_timeout_as_violations() {
    for method in [
        "create_connection",
        "handshake_next",
        "plan_read",
        "decode_read",
        "is_complete",
        "write_next",
    ] {
        for error in [FatalError::Io("試験".into()), FatalError::Timeout] {
            let stats = Arc::<Stats>::default();
            let events = Arc::<Recorder>::default();
            let mut conn = Conn::new(stats.clone());
            conn.error = Some((method, error.clone()));
            if method == "is_complete" {
                conn.step.expect = FrameRule::DriverDecides;
            }
            let driver = Driver::new(conn, true);
            let connected = FrameSession::with_transport(
                &driver,
                &Settings::new(),
                Box::new(Wire::new(stats.clone())),
                T,
                Some(events.clone()),
            )
            .await;
            let observed = match connected {
                Err(error) => error,
                Ok(mut s) if method == "write_next" => s.write_batch(&[item()]).await.unwrap_err(),
                Ok(mut s) => s.read_batch(&[spec()]).await.unwrap_err(),
            };
            assert_eq!(observed, error);
            assert!(events.has("C4"), "{method}");
            assert_eq!(stats.closes.load(Ordering::SeqCst), 1);
        }
    }
}

#[tokio::test(start_paused = true)]
async fn invalid_rules_are_rejected_before_any_send() {
    for (rule, max) in [
        (FrameRule::Fixed(0), 32),
        (FrameRule::Fixed(33), 32),
        (FrameRule::Terminator(vec![]), 32),
        (FrameRule::Terminator(vec![0; 33]), 32),
        (
            FrameRule::LengthPrefixed(PrefixSpec::new(0, 0, true, 0)),
            32,
        ),
        (
            FrameRule::LengthPrefixed(PrefixSpec::new(0, 9, true, 0)),
            32,
        ),
        (
            FrameRule::LengthPrefixed(PrefixSpec::new(u32::MAX, 2, true, 0)),
            u32::MAX,
        ),
        (
            FrameRule::LengthPrefixed(PrefixSpec::new(32, 1, true, 0)),
            32,
        ),
        (FrameRule::DriverDecides, 0),
    ] {
        let stats = Arc::<Stats>::default();
        let events = Arc::<Recorder>::default();
        let mut conn = Conn::new(stats.clone());
        conn.step = Step::new(vec![1], rule, max);
        let mut s = session(conn, Wire::new(stats.clone()), events.clone());
        assert!(matches!(
            s.read_batch(&[spec()]).await,
            Err(FatalError::Protocol(_))
        ));
        assert_eq!(stats.sends.load(Ordering::SeqCst), 0);
        assert!(events.has("C9"));
    }
}

#[tokio::test(start_paused = true)]
async fn prefix_encodings_validate_digits_endianness_adjust_and_overflow() {
    let cases = [
        (PrefixSpec::new(0, 2, true, 2), vec![0, 1, 7], true),
        (PrefixSpec::new(0, 2, false, 2), vec![1, 0, 7], true),
        (
            PrefixSpec::new(0, 2, true, 0).with_encoding(PrefixEncoding::AsciiDec),
            b"03x".to_vec(),
            true,
        ),
        (
            PrefixSpec::new(0, 2, true, -1).with_encoding(PrefixEncoding::AsciiHex),
            b"04x".to_vec(),
            true,
        ),
        (
            PrefixSpec::new(0, 2, true, 0).with_encoding(PrefixEncoding::AsciiDec),
            b"+3x".to_vec(),
            false,
        ),
        (
            PrefixSpec::new(0, 2, true, 0).with_encoding(PrefixEncoding::AsciiHex),
            b"+3x".to_vec(),
            false,
        ),
        (
            PrefixSpec::new(0, 2, true, 0).with_encoding(PrefixEncoding::AsciiHex),
            b"GGx".to_vec(),
            false,
        ),
        (
            PrefixSpec::new(0, 2, true, -4).with_encoding(PrefixEncoding::AsciiDec),
            b"03x".to_vec(),
            false,
        ),
        (PrefixSpec::new(0, 8, true, 0), vec![255; 8], false),
        (
            PrefixSpec::new(0, 20, true, 0).with_encoding(PrefixEncoding::AsciiDec),
            b"18446744073709551616".to_vec(),
            false,
        ),
        (
            PrefixSpec::new(0, 16, true, 0).with_encoding(PrefixEncoding::AsciiHex),
            b"FFFFFFFFFFFFFFFF".to_vec(),
            false,
        ),
    ];
    for (prefix, bytes, valid) in cases {
        let stats = Arc::<Stats>::default();
        let mut conn = Conn::new(stats.clone());
        conn.step.expect = FrameRule::LengthPrefixed(prefix);
        let mut wire = Wire::new(stats);
        wire.chunks.push_back(bytes);
        let mut s = session(conn, wire, Arc::default());
        let result = s.read_batch(&[spec()]).await;
        assert_eq!(result.is_ok(), valid, "{prefix:?}: {result:?}");
        if !valid {
            assert!(matches!(result, Err(FatalError::Framing(_))));
        }
    }
}

#[tokio::test(start_paused = true)]
async fn zero_byte_receive_and_overgrown_frames_close_and_drop_state() {
    for zero in [false, true] {
        let stats = Arc::<Stats>::default();
        let mut conn = Conn::new(stats.clone());
        conn.step.expect = FrameRule::Terminator(vec![10]);
        conn.step.max_len = 2;
        let mut wire = Wire::new(stats.clone());
        wire.zero_recv = zero;
        let mut s = session(conn, wire, Arc::default());
        let result = s.read_batch(&[spec()]).await;
        assert!(matches!(
            result,
            Err(FatalError::Io(_)) | Err(FatalError::Framing(_))
        ));
        assert_eq!(stats.transport_drops.load(Ordering::SeqCst), 1);
        assert_eq!(stats.conn_drops.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn numeric_raw_values_match_bit_and_scalar_tag_kinds() {
    for data_type in [
        DataType::Bit,
        DataType::I16,
        DataType::U16,
        DataType::I32,
        DataType::U32,
        DataType::F32,
        DataType::I64,
        DataType::U64,
        DataType::F64,
    ] {
        for raw in [
            RawValue::Bool(true),
            RawValue::Int(-7),
            RawValue::Uint(7),
            RawValue::Float(7.5),
            RawValue::Bytes(vec![7]),
        ] {
            let expected = match (&data_type, &raw) {
                (DataType::Bit, RawValue::Bool(v)) => Some(Value::Bit(*v)),
                (DataType::Bit, _) | (_, RawValue::Bool(_) | RawValue::Bytes(_)) => None,
                (_, RawValue::Int(v)) => Some(Value::F64(*v as f64)),
                (_, RawValue::Uint(v)) => Some(Value::F64(*v as f64)),
                (_, RawValue::Float(v)) => Some(Value::F64(*v)),
                _ => unreachable!(),
            };
            let result = decode::decode_value(&TagKind::numeric(data_type), raw.clone());
            match expected {
                Some(value) => assert_eq!(result, Ok(value), "{data_type:?}: {raw:?}"),
                None => assert!(
                    matches!(result, Err(ItemError::Unsupported(_))),
                    "{data_type:?}: {raw:?}: {result:?}"
                ),
            }
        }
    }
    for raw in [
        RawValue::Bool(true),
        RawValue::Int(7),
        RawValue::Uint(7),
        RawValue::Float(7.0),
    ] {
        assert!(matches!(
            decode::decode_value(&TagKind::string(1, StringEncoding::Utf8), raw),
            Err(ItemError::Unsupported(_))
        ));
    }
}

#[test]
fn strings_are_decoded_at_the_first_nul_without_replacement() {
    assert_eq!(
        decode::decode_string(b"AB\0\xff", StringEncoding::Utf8).unwrap(),
        "AB"
    );
    assert_eq!(
        decode::decode_string(&[0x82, 0xA0], StringEncoding::ShiftJis).unwrap(),
        "あ"
    );
    for encoding in [StringEncoding::Utf8, StringEncoding::ShiftJis] {
        assert!(matches!(
            decode::decode_string(&[0x82], encoding),
            Err(ItemError::Unsupported(_))
        ));
    }
    assert!(
        decode::decode_value(&TagKind::numeric(DataType::U16), RawValue::Bytes(vec![1])).is_err()
    );
    assert!(decode::decode_value(
        &TagKind::string(1, StringEncoding::Utf8),
        RawValue::Bytes(vec![65; 3])
    )
    .is_err());
    assert_eq!(
        decode::decode_value(
            &TagKind::string(1, StringEncoding::Utf8),
            RawValue::Bytes(vec![65, 66])
        )
        .unwrap(),
        Value::Str("AB".into())
    );
}

#[tokio::test]
async fn tcp_transport_receives_split_data_and_closes_on_eof() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let peer = tokio::spawn(async move {
        let (mut socket, _) = timeout(Duration::from_secs(2), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut request = [0; 1];
        timeout(T, socket.read_exact(&mut request))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(request, [1]);
        timeout(T, socket.write_all(b"a")).await.unwrap().unwrap();
        tokio::task::yield_now().await;
        timeout(T, socket.write_all(b"b")).await.unwrap().unwrap();
    });
    let mut transport = TransportSpec::Tcp {
        host: "127.0.0.1".into(),
        port,
    }
    .connect(Duration::from_secs(2))
    .await
    .unwrap();
    timeout(T, transport.send(&[1])).await.unwrap().unwrap();
    let mut buf = Vec::new();
    while buf.len() < 2 {
        timeout(T, transport.recv(&mut buf)).await.unwrap().unwrap();
    }
    assert_eq!(buf, b"ab");
    assert!(matches!(
        timeout(T, transport.recv(&mut buf)).await.unwrap(),
        Err(FatalError::Io(_))
    ));
    timeout(T, transport.close()).await.unwrap();
    timeout(T, peer).await.unwrap().unwrap();
}
