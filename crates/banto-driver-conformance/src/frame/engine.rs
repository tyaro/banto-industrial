//! 本物のエンジンを、記憶上の通信路と止めた Tokio の時計で駆動する。
//! Responder の借用と公開 API は保ち、通信だけを scoped worker 上で実行する。
//! 返答の生成は同期チャネルで行うので、生成待ちに仮想時計だけが進む競合はない。

use std::collections::{BTreeSet, VecDeque};
use std::future::Future;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use banto_driver_api::{
    BoxFuture, FatalError, FrameConnection, FrameDriver, FrameRule, ItemError, Planned, RawValue,
    Session, Settings, TagSpec, WriteItem,
};
use banto_driver_engine::{transport::Transport, EngineEvent, EngineEvents, FrameSession};
use tokio::time::Instant;

use super::{Reply, Responder};
use crate::Failure;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Fatal {
    pub(crate) error: FatalError,
    pub(crate) by_driver: bool,
}

impl Fatal {
    #[cfg(test)]
    fn engine(error: FatalError) -> Self {
        Self {
            error,
            by_driver: false,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Split {
    Every(usize),
    Straddle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Once {
    Broken,
    Late,
    Endless,
    Noise(Vec<u8>),
    CloseMidway,
    Close,
}

#[derive(Default)]
struct Control {
    split: Option<Split>,
    once: Option<Once>,
    fired: bool,
    events: Vec<EngineEvent>,
    rule: Option<FrameRule>,
}

struct Observer(Arc<Mutex<Control>>);
impl EngineEvents for Observer {
    fn record(&self, event: EngineEvent) {
        let mut control = self.0.lock().unwrap();
        if let EngineEvent::StepStarted(step) = &event {
            control.rule = Some(step.expect.clone());
        }
        control.events.push(event);
    }
}

enum Request {
    Reply {
        bytes: Vec<u8>,
        broken: bool,
        answer: mpsc::Sender<Option<Reply>>,
    },
    Finished,
}

pub(crate) struct Engine<'a> {
    driver: &'a dyn FrameDriver,
    settings: Settings,
    peer: &'a mut dyn Responder,
    timeout: Duration,
    session: Option<FrameSession>,
    runtime: Option<tokio::runtime::Runtime>,
    tx: mpsc::Sender<Request>,
    rx: mpsc::Receiver<Request>,
    control: Arc<Mutex<Control>>,
    pub(crate) connects: usize,
    pub(crate) split: Option<Split>,
    pub(crate) once: Option<Once>,
    pub(crate) fired: bool,
    pub(crate) write_log: Vec<Vec<u32>>,
    violations: Vec<Failure>,
    seen_violations: BTreeSet<String>,
}

impl<'a> Engine<'a> {
    pub(crate) fn new(
        driver: &'a dyn FrameDriver,
        settings: Settings,
        peer: &'a mut dyn Responder,
        timeout: Duration,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            driver,
            settings,
            peer,
            timeout,
            session: None,
            runtime: Some(
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .start_paused(true)
                    .build()
                    .unwrap(),
            ),
            tx,
            rx,
            control: Arc::default(),
            connects: 0,
            split: None,
            once: None,
            fired: false,
            write_log: Vec::new(),
            violations: Vec::new(),
            seen_violations: BTreeSet::new(),
        }
    }

    pub(crate) fn into_violations(mut self) -> Vec<Failure> {
        std::mem::take(&mut self.violations)
    }
    pub(crate) fn is_connected(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(FrameSession::is_connected)
    }
    pub(crate) fn drop_connection(&mut self) {
        self.session = None;
    }
    pub(crate) fn violation(&mut self, id: &'static str, detail: String) {
        if self.seen_violations.insert(format!("{id}{detail}")) {
            self.violations.push(Failure::new(id, detail));
        }
    }

    fn drive<T: Send>(
        &mut self,
        future: impl Future<Output = Result<T, FatalError>> + Send,
    ) -> Result<T, Fatal> {
        {
            let mut control = self.control.lock().unwrap();
            control.split = self.split;
            control.once = self.once.take();
            control.fired = self.fired;
        }
        let runtime = self.runtime.as_ref().unwrap();
        let peer = &mut self.peer;
        let rx = &self.rx;
        let tx = self.tx.clone();
        let result = std::thread::scope(|scope| {
            let worker = scope.spawn(move || {
                // panic でも通知し、ハーネスの呼び出し元を待ち続けさせない。
                struct Finished(mpsc::Sender<Request>);
                impl Drop for Finished {
                    fn drop(&mut self) {
                        let _ = self.0.send(Request::Finished);
                    }
                }
                let _finished = Finished(tx);
                runtime.block_on(future)
            });
            while let Ok(request) = rx.recv() {
                match request {
                    Request::Finished => break,
                    Request::Reply {
                        bytes,
                        broken,
                        answer,
                    } => {
                        let reply = if broken {
                            peer.reply_broken(&bytes)
                        } else {
                            Some(peer.reply(&bytes))
                        };
                        let _ = answer.send(reply);
                    }
                }
            }
            worker
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
        });
        let (events, once, fired) = {
            let mut control = self.control.lock().unwrap();
            (
                std::mem::take(&mut control.events),
                control.once.take(),
                control.fired,
            )
        };
        self.once = once;
        self.fired = fired;
        let mut driver_error = None;
        for event in events {
            match event {
                EngineEvent::Violation { check, detail } => self.violation(check, detail),
                EngineEvent::DriverFatal { error, .. } => driver_error = Some(error),
                EngineEvent::WriteStep(covers) => self.write_log.push(covers),
                _ => {}
            }
        }
        result.map_err(|error| Fatal {
            by_driver: driver_error.as_ref() == Some(&error),
            error,
        })
    }

    pub(crate) fn ensure_connected(&mut self) -> Result<(), Fatal> {
        if self.is_connected() {
            return Ok(());
        }
        self.peer.on_connect();
        self.connects += 1;
        let transport = Wire {
            tx: self.tx.clone(),
            control: self.control.clone(),
            timeout: self.timeout,
            arrivals: VecDeque::new(),
            closed: false,
        };
        let driver = self.driver;
        let settings = self.settings.clone();
        let timeout = self.timeout;
        let observer = Arc::new(Observer(self.control.clone()));
        self.session = Some(self.drive(async move {
            FrameSession::with_transport(
                driver,
                &settings,
                Box::new(transport),
                timeout,
                Some(observer),
            )
            .await
        })?);
        Ok(())
    }

    pub(crate) fn read(
        &mut self,
        specs: &[TagSpec],
    ) -> Result<Vec<Result<RawValue, ItemError>>, Fatal> {
        self.ensure_connected()?;
        let mut session = self.session.take().unwrap();
        let result = self.drive(session.read_raw_batch(specs));
        if session.is_connected() {
            self.session = Some(session);
        }
        result
    }

    pub(crate) fn write(
        &mut self,
        items: &[WriteItem],
    ) -> Result<Vec<Result<(), ItemError>>, Fatal> {
        self.ensure_connected()?;
        let mut session = self.session.take().unwrap();
        let result = self.drive(session.write_batch(items));
        if session.is_connected() {
            self.session = Some(session);
        }
        result
    }

    pub(crate) fn plan_only(&mut self, specs: &[TagSpec]) -> Result<Vec<Planned>, Fatal> {
        self.ensure_connected()?;
        let result = self
            .session
            .as_mut()
            .unwrap()
            .connection_mut()
            .unwrap()
            .plan_read(specs);
        result.map_err(|error| {
            if matches!(error, FatalError::Io(_) | FatalError::Timeout) {
                self.violation(
                    "C4",
                    format!("plan_read が {error:?} を返した（フレーム型は返さない）"),
                );
            }
            self.drop_connection();
            Fatal {
                error,
                by_driver: true,
            }
        })
    }

    pub(crate) fn connection(&self) -> Option<&dyn FrameConnection> {
        self.session.as_ref().and_then(FrameSession::connection)
    }
}

impl Drop for Engine<'_> {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

struct Wire {
    tx: mpsc::Sender<Request>,
    control: Arc<Mutex<Control>>,
    timeout: Duration,
    arrivals: VecDeque<(Instant, Vec<u8>)>,
    closed: bool,
}

impl Transport for Wire {
    fn send<'a>(&'a mut self, bytes: &'a [u8]) -> BoxFuture<'a, Result<(), FatalError>> {
        Box::pin(async move {
            let (rule, mut once, split) = {
                let mut control = self.control.lock().unwrap();
                let rule = control.rule.clone().unwrap_or(FrameRule::None);
                let once = if rule == FrameRule::None {
                    None
                } else {
                    control.once.take()
                };
                (rule, once, control.split)
            };
            if matches!(once, Some(Once::Noise(_))) && rule != FrameRule::DriverDecides {
                self.control.lock().unwrap().once = once.take();
            }
            let mut reply = self.request(bytes, matches!(once, Some(Once::Broken)));
            if reply.is_none() {
                once = None;
                reply = self.request(bytes, false);
            }
            let reply = if matches!(once, Some(Once::Close)) {
                Reply::Close
            } else {
                reply.unwrap()
            };
            if once.is_some() {
                self.control.lock().unwrap().fired = true;
            }
            self.arrivals.clear();
            self.closed = false;
            if rule == FrameRule::None {
                return if reply == Reply::Close {
                    Err(FatalError::Io("相手が接続を閉じた".into()))
                } else {
                    Ok(())
                };
            }
            let mut arrivals = match reply {
                Reply::Bytes(bytes) => vec![(Duration::ZERO, bytes)],
                Reply::Chunks(chunks, gap) => chunks
                    .into_iter()
                    .enumerate()
                    .map(|(i, c)| (gap.saturating_mul(i as u32), c))
                    .collect(),
                Reply::Silence => Vec::new(),
                Reply::Close => {
                    self.closed = true;
                    Vec::new()
                }
            };
            match once {
                Some(Once::Late) => {
                    for arrival in &mut arrivals {
                        arrival.0 += self.timeout + Duration::from_millis(1);
                    }
                }
                Some(Once::Noise(bytes)) => arrivals.insert(0, (Duration::ZERO, bytes)),
                Some(Once::CloseMidway) => {
                    let mut keep = arrivals.iter().map(|a| a.1.len()).sum::<usize>() / 2;
                    for arrival in &mut arrivals {
                        let n = keep.min(arrival.1.len());
                        arrival.1.truncate(n);
                        keep -= n;
                    }
                    self.closed = true;
                }
                Some(Once::Endless) => {
                    let count = (self.timeout.as_millis() as usize).saturating_add(2);
                    arrivals = (0..count)
                        .map(|i| {
                            (
                                Duration::from_millis(i as u64),
                                vec![filler_byte(&rule); 16],
                            )
                        })
                        .collect();
                }
                _ => {}
            }
            let now = Instant::now();
            self.arrivals = Self::cut(split, arrivals)
                .into_iter()
                .filter(|(_, b)| !b.is_empty())
                .map(|(at, bytes)| (now + at, bytes))
                .collect();
            Ok(())
        })
    }

    fn recv<'a>(&'a mut self, buf: &'a mut Vec<u8>) -> BoxFuture<'a, Result<usize, FatalError>> {
        Box::pin(async move {
            if let Some((at, bytes)) = self.arrivals.pop_front() {
                tokio::time::sleep_until(at).await;
                let n = bytes.len();
                buf.extend(bytes);
                Ok(n)
            } else if self.closed {
                Err(FatalError::Io("相手が接続を閉じた".into()))
            } else {
                std::future::pending().await
            }
        })
    }
    fn discard_input(&mut self) -> BoxFuture<'_, ()> {
        self.arrivals.clear();
        Box::pin(async {})
    }
    fn close(&mut self) -> BoxFuture<'_, ()> {
        self.arrivals.clear();
        Box::pin(async {})
    }
}

impl Wire {
    fn request(&self, bytes: &[u8], broken: bool) -> Option<Reply> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(Request::Reply {
                bytes: bytes.to_vec(),
                broken,
                answer: tx,
            })
            .unwrap();
        rx.recv().unwrap()
    }
    fn cut(split: Option<Split>, arrivals: Vec<(Duration, Vec<u8>)>) -> Vec<(Duration, Vec<u8>)> {
        let Some(split) = split else {
            return arrivals;
        };
        let mut out = Vec::new();
        let pattern = [1usize, 2, 3, 5];
        let mut k = 0usize;
        for (at, bytes) in arrivals {
            let mut rest = &bytes[..];
            while !rest.is_empty() {
                let n = match split {
                    Split::Every(n) => n.max(1),
                    Split::Straddle => {
                        k += 1;
                        pattern[(k - 1) % pattern.len()]
                    }
                }
                .min(rest.len());
                out.push((at, rest[..n].to_vec()));
                rest = &rest[n..];
            }
        }
        out
    }
}

fn filler_byte(rule: &FrameRule) -> u8 {
    match rule {
        FrameRule::Terminator(t) => (0u8..=255).find(|b| !t.contains(b)).unwrap_or(0),
        _ => 0x00,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use banto_driver_api::{
        AddressError, Capabilities, DriverAddress, DriverBase, DriverDescriptor, DriverId,
        FieldError, FrameCheck, PrefixEncoding, PrefixSpec, Step, TagKind,
    };

    /// 規則を切り替えられる 1 step のドライバ。応答をそのまま返す。
    struct OneStep(DriverDescriptor, FrameRule);

    impl DriverBase for OneStep {
        fn descriptor(&self) -> &DriverDescriptor {
            &self.0
        }
        fn validate_config(&self, _s: &Settings) -> Vec<FieldError> {
            Vec::new()
        }
        fn parse_address(&self, t: &str, k: &TagKind) -> Result<DriverAddress, AddressError> {
            Ok(DriverAddress::new(
                self.0.id.clone(),
                t,
                "a",
                true,
                k.word_count(),
            ))
        }
        fn offset_address(&self, c: &str, _k: &TagKind, _n: i64) -> Result<String, AddressError> {
            Ok(c.to_owned())
        }
        fn address_help(&self) -> String {
            String::new()
        }
    }

    struct OneConn(FrameRule);

    impl FrameConnection for OneConn {
        fn handshake_next(&mut self, _p: Option<&[u8]>) -> Result<Option<Step>, FatalError> {
            Ok(None)
        }
        fn is_complete(&self, _s: &Step, buf: &[u8]) -> Result<FrameCheck, FatalError> {
            // 先頭の 0xFF を捨て、最初の 0x0A までを応答とする。
            if buf[0] == 0xFF {
                let n = buf.iter().take_while(|&&b| b == 0xFF).count();
                return Ok(FrameCheck::Discard(n as u32));
            }
            Ok(match buf.iter().position(|&b| b == 0x0A) {
                Some(i) => FrameCheck::Complete { len: i as u32 + 1 },
                None => FrameCheck::Incomplete,
            })
        }
        fn plan_read(&mut self, specs: &[TagSpec]) -> Result<Vec<Planned>, FatalError> {
            Ok(vec![Planned::new(
                Step::new(b"Q".to_vec(), self.0.clone(), 16),
                (0..specs.len() as u32).collect(),
            )])
        }
        fn decode_read(
            &self,
            p: &Planned,
            _s: &[TagSpec],
            r: &[u8],
        ) -> Result<Vec<Result<RawValue, ItemError>>, FatalError> {
            Ok(p.covers
                .iter()
                .map(|_| Ok(RawValue::Bytes(r.to_vec())))
                .collect())
        }
    }

    impl FrameDriver for OneStep {
        fn create_connection(&self, _s: &Settings) -> Result<Box<dyn FrameConnection>, FatalError> {
            Ok(Box::new(OneConn(self.1.clone())))
        }
    }

    struct Fixed(Vec<u8>);
    impl Responder for Fixed {
        fn reply(&mut self, _request: &[u8]) -> Reply {
            Reply::Bytes(self.0.clone())
        }
    }

    fn read_once(
        rule: FrameRule,
        reply: &[u8],
        split: Option<Split>,
        once: Option<Once>,
    ) -> Result<Vec<u8>, Fatal> {
        let driver = OneStep(
            DriverDescriptor::new(
                DriverId::new("t").unwrap(),
                "0",
                "t",
                Capabilities::READ_ONLY,
            ),
            rule,
        );
        let mut peer = Fixed(reply.to_vec());
        let mut e = Engine::new(
            &driver,
            Settings::new(),
            &mut peer,
            Duration::from_millis(50),
        );
        e.split = split;
        e.once = once;
        let spec = TagSpec::new(
            driver
                .parse_address("x", &TagKind::numeric(banto_driver_api::DataType::U16))
                .unwrap(),
            TagKind::numeric(banto_driver_api::DataType::U16),
        );
        let out = e.read(&[spec])?;
        match out.into_iter().next().unwrap() {
            Ok(RawValue::Bytes(b)) => Ok(b),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn every_rule_receives_whole_split_and_straddled_responses() {
        let prefixed = FrameRule::LengthPrefixed(PrefixSpec::new(1, 1, true, 2));
        let ascii = FrameRule::LengthPrefixed(
            PrefixSpec::new(0, 2, true, 2).with_encoding(PrefixEncoding::AsciiHex),
        );
        let cases: Vec<(FrameRule, Vec<u8>, Vec<u8>)> = vec![
            (FrameRule::Fixed(3), b"abcXX".to_vec(), b"abc".to_vec()),
            (prefixed, vec![9, 3, 1, 2, 3, 99], vec![9, 3, 1, 2, 3]),
            (ascii, b"03xyzQ".to_vec(), b"03xyz".to_vec()),
            (
                FrameRule::Terminator(b"\r\n".to_vec()),
                b"ok\r\nzz".to_vec(),
                b"ok\r\n".to_vec(),
            ),
            (
                FrameRule::DriverDecides,
                b"hi\nrest".to_vec(),
                b"hi\n".to_vec(),
            ),
        ];
        for (rule, reply, want) in cases {
            for split in [None, Some(Split::Every(1)), Some(Split::Straddle)] {
                assert_eq!(
                    read_once(rule.clone(), &reply, split, None).unwrap(),
                    want,
                    "{rule:?} {split:?}"
                );
            }
        }
    }

    #[test]
    fn noise_is_discarded_only_through_driver_decides() {
        let got = read_once(
            FrameRule::DriverDecides,
            b"hi\n",
            Some(Split::Every(1)),
            Some(Once::Noise(vec![0xFF; 5])),
        )
        .unwrap();
        assert_eq!(got, b"hi\n");
    }

    #[test]
    fn timeouts_endless_and_closes_are_fatal_engine_errors() {
        let late = read_once(FrameRule::Fixed(3), b"abc", None, Some(Once::Late)).unwrap_err();
        assert_eq!(late, Fatal::engine(FatalError::Timeout));
        for rule in [
            FrameRule::Terminator(b"\n".to_vec()),
            FrameRule::DriverDecides,
            FrameRule::LengthPrefixed(PrefixSpec::new(0, 1, true, 200)),
        ] {
            let err = read_once(rule.clone(), b"abc\n", None, Some(Once::Endless)).unwrap_err();
            assert!(!err.by_driver, "{rule:?}: {err:?}");
            assert!(
                matches!(err.error, FatalError::Framing(_) | FatalError::Timeout),
                "{rule:?}: {err:?}"
            );
        }
        let closed =
            read_once(FrameRule::Fixed(4), b"abcd", None, Some(Once::CloseMidway)).unwrap_err();
        assert_eq!(
            closed,
            Fatal::engine(FatalError::Io("相手が接続を閉じた".into()))
        );
        let short = read_once(FrameRule::Fixed(5), b"abc", None, None).unwrap_err();
        assert_eq!(short, Fatal::engine(FatalError::Timeout));
    }

    #[test]
    fn a_fixed_rule_beyond_max_len_is_a_violation() {
        let err = read_once(FrameRule::Fixed(17), b"abc", None, None).unwrap_err();
        assert!(matches!(err.error, FatalError::Protocol(_)), "{err:?}");
    }
}
