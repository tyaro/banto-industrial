//! The harness's engine: the Hub engine's request/response procedure
//! (docs/banto-driver-api-design.md §4.2, §4.2.1) over an in-memory wire
//! with virtual time.
//!
//! Faults are the engine's, not the responder's: the responder answers like
//! a healthy device and the engine re-times, splits, delays, truncates or
//! replaces what "arrives". Time is virtual (an arrival has an offset from
//! the send; anything after the response timeout never arrives in time), so
//! every run is deterministic and takes no wall-clock time.
//!
//! Besides producing results, the engine records **contract violations** of
//! the driver it observes on the way (a `FatalError::Io` from a frame
//! driver, `covers` out of range, a `Discard(0)`, a `Done` of the wrong
//! length, ...). They are turned into [`Failure`]s by `run_all`.

use std::collections::BTreeSet;
use std::time::Duration;

use banto_driver_api::{
    FatalError, FrameCheck, FrameConnection, FrameDriver, FrameRule, ItemError, Planned,
    PrefixEncoding, PrefixSpec, RawValue, Settings, Step, TagSpec, WriteItem, WriteProgress,
};

use super::{Reply, Responder};
use crate::Failure;

/// A handshake longer than this is a driver bug (C7).
pub(crate) const MAX_HANDSHAKE_STEPS: usize = 32;

/// Filler bytes of an endless response arrive this often (virtual time).
const ENDLESS_INTERVAL: Duration = Duration::from_millis(1);
/// ... this many at a time.
const ENDLESS_CHUNK: usize = 16;

/// A connection-fatal outcome, and whether the driver or the engine raised
/// it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Fatal {
    pub(crate) error: FatalError,
    pub(crate) by_driver: bool,
}

impl Fatal {
    fn engine(error: FatalError) -> Self {
        Fatal {
            error,
            by_driver: false,
        }
    }
}

/// How every response is cut into arrivals until reset (C5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Split {
    /// Pieces of this many bytes.
    Every(usize),
    /// Pieces of 1, 2, 3, 5, 1, 2, ... bytes, so boundaries fall everywhere.
    Straddle,
}

/// A fault applied to the next exchange that expects a response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Once {
    /// Use [`Responder::reply_broken`] instead of the healthy reply (C4).
    Broken,
    /// The response arrives after the timeout (C6).
    Late,
    /// Filler bytes keep arriving and never form the response (C9).
    Endless,
    /// These bytes arrive before the response (C9; only for
    /// `FrameRule::DriverDecides`, the one rule that can skip them).
    Noise(Vec<u8>),
    /// Half of the response arrives, then the peer closes (C4).
    CloseMidway,
    /// The peer closes instead of answering (C7).
    Close,
}

pub(crate) struct Engine<'a> {
    driver: &'a dyn FrameDriver,
    settings: Settings,
    peer: &'a mut dyn Responder,
    timeout: Duration,
    conn: Option<Box<dyn FrameConnection>>,
    /// Bytes still travelling on the current connection's wire (a late
    /// response). Dropping the connection drops them: that is C6.
    pending: Vec<u8>,
    /// How many connections were created (each runs the handshake).
    pub(crate) connects: usize,
    pub(crate) split: Option<Split>,
    pub(crate) once: Option<Once>,
    /// Whether the last `once` was actually applied.
    pub(crate) fired: bool,
    /// The `covers` of every write step sent, in order (C10).
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
        Engine {
            driver,
            settings,
            peer,
            timeout,
            conn: None,
            pending: Vec::new(),
            connects: 0,
            split: None,
            once: None,
            fired: false,
            write_log: Vec::new(),
            violations: Vec::new(),
            seen_violations: BTreeSet::new(),
        }
    }

    pub(crate) fn into_violations(self) -> Vec<Failure> {
        self.violations
    }

    pub(crate) fn is_connected(&self) -> bool {
        self.conn.is_some()
    }

    /// Record a driver contract violation (each distinct one once).
    pub(crate) fn violation(&mut self, id: &'static str, detail: String) {
        if self.seen_violations.insert(format!("{id}{detail}")) {
            self.violations.push(Failure::new(id, detail));
        }
    }

    /// The engine's reaction to a fatal outcome: forget the connection and
    /// whatever is still on its wire.
    pub(crate) fn drop_connection(&mut self) {
        self.conn = None;
        self.pending.clear();
    }

    /// A fatal error returned by the driver's `method`. Frame drivers must
    /// not return `Io` / `Timeout` (C4). The connection is dropped.
    fn driver_fatal(&mut self, method: &str, error: FatalError) -> Fatal {
        if matches!(error, FatalError::Io(_) | FatalError::Timeout) {
            self.violation(
                "C4",
                format!(
                    "{method} が {error:?} を返した（フレーム型は Io / Timeout を返してはならない）"
                ),
            );
        }
        self.drop_connection();
        Fatal {
            error,
            by_driver: true,
        }
    }

    /// A contract violation that the engine answers with
    /// `FatalError::Protocol`, like the Hub's engine would (never a panic).
    fn violation_fatal(&mut self, id: &'static str, detail: String) -> Fatal {
        self.violation(id, detail.clone());
        self.drop_connection();
        Fatal::engine(FatalError::Protocol(detail))
    }

    /// Create a connection and run its handshake, unless one is up.
    pub(crate) fn ensure_connected(&mut self) -> Result<(), Fatal> {
        if self.conn.is_some() {
            return Ok(());
        }
        self.pending.clear();
        let conn = match self.driver.create_connection(&self.settings) {
            Ok(c) => c,
            Err(e) => return Err(self.driver_fatal("create_connection", e)),
        };
        self.connects += 1;
        self.peer.on_connect();
        self.conn = Some(conn);
        let mut prev: Option<Vec<u8>> = None;
        for _ in 0..MAX_HANDSHAKE_STEPS {
            let next = self.conn_mut().handshake_next(prev.as_deref());
            match next {
                Err(e) => return Err(self.driver_fatal("handshake_next", e)),
                Ok(None) => return Ok(()),
                Ok(Some(step)) => prev = Some(self.exchange(&step)?.unwrap_or_default()),
            }
        }
        Err(self.violation_fatal(
            "C7",
            format!("ハンドシェイクが {MAX_HANDSHAKE_STEPS} step で終わらない"),
        ))
    }

    fn conn_mut(&mut self) -> &mut dyn FrameConnection {
        self.conn.as_deref_mut().expect("connected")
    }

    fn conn_ref(&self) -> &dyn FrameConnection {
        self.conn.as_deref().expect("connected")
    }

    /// Plan, send, receive and decode one read (design §4.2 steps 1-5).
    pub(crate) fn read(
        &mut self,
        specs: &[TagSpec],
    ) -> Result<Vec<Result<RawValue, ItemError>>, Fatal> {
        self.ensure_connected()?;
        let planned = match self.conn_mut().plan_read(specs) {
            Ok(p) => p,
            Err(e) => return Err(self.driver_fatal("plan_read", e)),
        };
        let mut seen = vec![false; specs.len()];
        for p in &planned {
            for &c in &p.covers {
                let Some(slot) = seen.get_mut(c as usize) else {
                    return Err(self.violation_fatal(
                        "C3",
                        format!(
                            "plan_read の covers に範囲外の添字 {c}（specs は {} 件）",
                            specs.len()
                        ),
                    ));
                };
                if *slot {
                    return Err(self.violation_fatal(
                        "C3",
                        format!("plan_read の covers で添字 {c} が 2 回出た"),
                    ));
                }
                *slot = true;
            }
        }
        let mut out: Vec<Option<Result<RawValue, ItemError>>> = vec![None; specs.len()];
        for p in &planned {
            let response = self.exchange(&p.step)?.unwrap_or_default();
            debug_assert!(response.len() <= p.step.max_len as usize);
            let decoded = match self.conn_ref().decode_read(p, specs, &response) {
                Ok(d) => d,
                Err(e) => return Err(self.driver_fatal("decode_read", e)),
            };
            if decoded.len() != p.covers.len() {
                return Err(self.violation_fatal(
                    "C3",
                    format!(
                        "decode_read が {} 件を返した（covers は {} 件）",
                        decoded.len(),
                        p.covers.len()
                    ),
                ));
            }
            for (&c, v) in p.covers.iter().zip(decoded) {
                out[c as usize] = Some(v);
            }
        }
        Ok(out
            .into_iter()
            .map(|o| o.unwrap_or_else(|| Err(ItemError::Unsupported("計画に含まれない".into()))))
            .collect())
    }

    /// One logical write through the `write_next` state machine (design
    /// §4.2.1).
    pub(crate) fn write(
        &mut self,
        items: &[WriteItem],
    ) -> Result<Vec<Result<(), ItemError>>, Fatal> {
        self.ensure_connected()?;
        let max_steps = 3 * items.len() + 8;
        let mut steps = 0usize;
        let mut prev: Option<Vec<u8>> = None;
        loop {
            let progress = match self.conn_mut().write_next(items, prev.as_deref()) {
                Ok(p) => p,
                Err(e) => return Err(self.driver_fatal("write_next", e)),
            };
            match progress {
                WriteProgress::Send(planned) => {
                    steps += 1;
                    if steps > max_steps {
                        return Err(self.violation_fatal(
                            "C16",
                            format!("書き込みの手順が上限 {max_steps} step を超えた"),
                        ));
                    }
                    if let Some(&c) = planned.covers.iter().find(|&&c| c as usize >= items.len()) {
                        return Err(self.violation_fatal(
                            "C10",
                            format!(
                                "write_next の covers に範囲外の添字 {c}（items は {} 件）",
                                items.len()
                            ),
                        ));
                    }
                    self.write_log.push(planned.covers.clone());
                    prev = Some(self.exchange(&planned.step)?.unwrap_or_default());
                }
                WriteProgress::Done(results) => {
                    if results.len() != items.len() {
                        return Err(self.violation_fatal(
                            "C3",
                            format!(
                                "書き込みの結果の件数が要求と合わない（Done が {} 件、items は {} 件）",
                                results.len(),
                                items.len()
                            ),
                        ));
                    }
                    return Ok(results);
                }
                other => {
                    return Err(self.violation_fatal(
                        "C10",
                        format!("write_next が未知の WriteProgress を返した: {other:?}"),
                    ))
                }
            }
        }
    }

    /// Plan a read without sending it (C11).
    pub(crate) fn plan_only(&mut self, specs: &[TagSpec]) -> Result<Vec<Planned>, Fatal> {
        self.ensure_connected()?;
        match self.conn_mut().plan_read(specs) {
            Ok(p) => Ok(p),
            Err(e) => Err(self.driver_fatal("plan_read", e)),
        }
    }

    /// The live connection, for C11's direct calls.
    pub(crate) fn connection(&self) -> Option<&dyn FrameConnection> {
        self.conn.as_deref()
    }

    /// A step's static sanity (the engine could not receive it otherwise).
    fn validate_step(&mut self, step: &Step) -> Result<(), Fatal> {
        let problem = match &step.expect {
            FrameRule::None => None,
            _ if step.max_len == 0 => Some("max_len が 0".to_owned()),
            FrameRule::Fixed(n) if *n == 0 || *n > step.max_len => {
                Some(format!("Fixed({n}) が 1..=max_len（{}）の外", step.max_len))
            }
            FrameRule::Terminator(t) if t.is_empty() => Some("Terminator が空".to_owned()),
            FrameRule::LengthPrefixed(p)
                if p.width == 0 || (p.encoding == PrefixEncoding::Binary && p.width > 8) =>
            {
                Some(format!("LengthPrefixed の width {} は読めない", p.width))
            }
            _ => None,
        };
        match problem {
            Some(p) => Err(self.violation_fatal("C9", format!("step が受信できない: {p}"))),
            None => Ok(()),
        }
    }

    /// Send `step` and receive its response by `step.expect` (design §4.2
    /// steps 2-3). `Ok(None)` for `FrameRule::None`.
    fn exchange(&mut self, step: &Step) -> Result<Option<Vec<u8>>, Fatal> {
        self.validate_step(step)?;
        let expects_response = !matches!(step.expect, FrameRule::None);
        let mut once = if expects_response {
            self.once.take()
        } else {
            None
        };
        if matches!(once, Some(Once::Noise(_))) && step.expect != FrameRule::DriverDecides {
            // Noise can only be skipped under DriverDecides; keep it for a
            // step that can.
            self.once = once.take();
        }
        let reply = match &once {
            Some(Once::Broken) => match self.peer.reply_broken(&step.send) {
                Some(r) => r,
                None => {
                    once = None;
                    self.peer.reply(&step.send)
                }
            },
            Some(Once::Close) => {
                let _ = self.peer.reply(&step.send);
                Reply::Close
            }
            _ => self.peer.reply(&step.send),
        };
        if once.is_some() {
            self.fired = true;
        }

        if !expects_response {
            if matches!(reply, Reply::Close) {
                self.drop_connection();
                return Err(Fatal::engine(FatalError::Io("相手が接続を閉じた".into())));
            }
            return Ok(None);
        }

        // What arrives, and when (relative to the send).
        let mut arrivals: Vec<(Duration, Vec<u8>)> = Vec::new();
        if !self.pending.is_empty() {
            arrivals.push((Duration::ZERO, std::mem::take(&mut self.pending)));
        }
        let mut reply_arrivals: Vec<(Duration, Vec<u8>)> = Vec::new();
        let mut closed = false;
        match reply {
            Reply::Bytes(b) => reply_arrivals.push((Duration::ZERO, b)),
            Reply::Chunks(chunks, gap) => {
                for (i, c) in chunks.into_iter().enumerate() {
                    reply_arrivals.push((gap * i as u32, c));
                }
            }
            Reply::Silence => {}
            Reply::Close => closed = true,
        }
        match &once {
            Some(Once::Late) => {
                for a in &mut reply_arrivals {
                    a.0 += self.timeout + Duration::from_millis(1);
                }
            }
            Some(Once::Noise(noise)) => arrivals.push((Duration::ZERO, noise.clone())),
            Some(Once::CloseMidway) => {
                let total: usize = reply_arrivals.iter().map(|a| a.1.len()).sum();
                let mut keep = total / 2;
                for a in &mut reply_arrivals {
                    let n = a.1.len().min(keep);
                    a.1.truncate(n);
                    keep -= n;
                }
                closed = true;
            }
            Some(Once::Endless) => {
                let filler = filler_byte(&step.expect);
                let count = (self.timeout.as_millis() as usize) + 2;
                reply_arrivals = (0..count)
                    .map(|i| (ENDLESS_INTERVAL * i as u32, vec![filler; ENDLESS_CHUNK]))
                    .collect();
            }
            _ => {}
        }
        arrivals.extend(reply_arrivals);
        let arrivals = self.cut(arrivals);

        let mut buf: Vec<u8> = Vec::new();
        let mut iter = arrivals.into_iter();
        let mut arrived_late = Vec::new();
        for (at, bytes) in iter.by_ref() {
            if at > self.timeout {
                arrived_late.extend(bytes);
                break;
            }
            buf.extend_from_slice(&bytes);
            if let Some(len) = self.check_complete(step, &mut buf)? {
                if len > step.max_len as usize {
                    self.drop_connection();
                    return Err(Fatal::engine(FatalError::Framing(format!(
                        "応答（{len} バイト）が max_len（{}）を超えた",
                        step.max_len
                    ))));
                }
                // The rest of `buf` is discarded, never carried over.
                return Ok(Some(buf[..len].to_vec()));
            }
            if buf.len() > step.max_len as usize {
                self.drop_connection();
                return Err(Fatal::engine(FatalError::Framing(format!(
                    "応答が max_len（{}）を超えても完結しない",
                    step.max_len
                ))));
            }
        }
        for (_, bytes) in iter {
            arrived_late.extend(bytes);
        }
        // Timed out (or closed). Whatever is still coming belongs to this
        // connection's wire - and is dropped with it (C6).
        self.pending = arrived_late;
        let timed_out = !self.pending.is_empty() || !closed;
        self.drop_connection();
        if timed_out {
            Err(Fatal::engine(FatalError::Timeout))
        } else {
            Err(Fatal::engine(FatalError::Io("相手が接続を閉じた".into())))
        }
    }

    /// Apply the persistent [`Split`].
    fn cut(&self, arrivals: Vec<(Duration, Vec<u8>)>) -> Vec<(Duration, Vec<u8>)> {
        let Some(split) = self.split else {
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

    /// `Some(len)` when `buf` starts with a complete response.
    fn check_complete(&mut self, step: &Step, buf: &mut Vec<u8>) -> Result<Option<usize>, Fatal> {
        match &step.expect {
            FrameRule::Fixed(n) => Ok((buf.len() >= *n as usize).then_some(*n as usize)),
            FrameRule::LengthPrefixed(p) => self.prefixed_len(p, step.max_len, buf),
            FrameRule::Terminator(t) => Ok(buf
                .windows(t.len())
                .position(|w| w == &t[..])
                .map(|i| i + t.len())),
            FrameRule::DriverDecides => loop {
                if buf.is_empty() {
                    return Ok(None);
                }
                let check = self.conn_ref().is_complete(step, buf);
                match check {
                    Err(e) => return Err(self.driver_fatal("is_complete", e)),
                    Ok(FrameCheck::Incomplete) => return Ok(None),
                    Ok(FrameCheck::Complete { len }) => {
                        if len == 0 || len as usize > buf.len() {
                            return Err(self.violation_fatal(
                                "C9",
                                format!(
                                    "is_complete が Complete {{ len: {len} }}（受信は {} バイト）",
                                    buf.len()
                                ),
                            ));
                        }
                        return Ok(Some(len as usize));
                    }
                    Ok(FrameCheck::Discard(n)) => {
                        if n == 0 || n as usize > buf.len() {
                            return Err(self.violation_fatal(
                                "C9",
                                format!(
                                    "is_complete が Discard({n})（受信は {} バイト。1 以上・受信以下）",
                                    buf.len()
                                ),
                            ));
                        }
                        buf.drain(..n as usize);
                    }
                    Ok(other) => {
                        return Err(self.violation_fatal(
                            "C9",
                            format!("is_complete が未知の FrameCheck を返した: {other:?}"),
                        ))
                    }
                }
            },
            // `None` is handled before receiving; an unknown future rule
            // cannot be received by this harness version.
            other => {
                Err(self
                    .violation_fatal("C9", format!("このハーネスが知らない FrameRule: {other:?}")))
            }
        }
    }

    fn prefixed_len(
        &mut self,
        p: &PrefixSpec,
        max_len: u32,
        buf: &[u8],
    ) -> Result<Option<usize>, Fatal> {
        let start = p.offset as usize;
        let need = start + p.width as usize;
        if buf.len() < need {
            return Ok(None);
        }
        let field = &buf[start..need];
        let value: Option<u64> = match p.encoding {
            PrefixEncoding::Binary => {
                let mut v = 0u64;
                if p.big_endian {
                    for &b in field {
                        v = (v << 8) | u64::from(b);
                    }
                } else {
                    for &b in field.iter().rev() {
                        v = (v << 8) | u64::from(b);
                    }
                }
                Some(v)
            }
            PrefixEncoding::AsciiHex => std::str::from_utf8(field)
                .ok()
                .and_then(|s| u64::from_str_radix(s, 16).ok()),
            PrefixEncoding::AsciiDec => std::str::from_utf8(field)
                .ok()
                .and_then(|s| s.parse::<u64>().ok()),
            other => {
                return Err(self.violation_fatal(
                    "C9",
                    format!("このハーネスが知らない PrefixEncoding: {other:?}"),
                ))
            }
        };
        let Some(value) = value else {
            self.drop_connection();
            return Err(Fatal::engine(FatalError::Framing(format!(
                "長さ欄 {field:02X?} が読めない"
            ))));
        };
        let total = value as i128 + i128::from(p.adjust);
        if total < need as i128 || total > i128::from(max_len) {
            self.drop_connection();
            return Err(Fatal::engine(FatalError::Framing(format!(
                "長さ欄が総長 {total} を示す（{need}..={max_len} の外）"
            ))));
        }
        let total = total as usize;
        Ok((buf.len() >= total).then_some(total))
    }
}

/// A filler byte that cannot end a response under `rule` by itself.
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
        FieldError, TagKind,
    };

    /// A driver whose only step is a fixed request under a configurable
    /// rule; decode returns the response's length.
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
            // Skip leading 0xFF, complete at the first 0x0A (inclusive).
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
