//! The frame-type harness: [`run_all`] plays the Hub's engine for a
//! [`FrameDriver`] against a [`Responder`] (docs/banto-driver-api-design.md
//! §7.1, §7.2).
//!
//! The responder answers like a healthy device; the harness itself injects
//! the faults (split, straddled, late, endless, noisy, broken and cut-off
//! responses, and peer closes) by re-timing what "arrives" on an in-memory
//! wire with **virtual time**. Nothing sleeps and nothing depends on the
//! machine's speed, so a run is deterministic.
//!
//! ## What [`run_all`] checks
//!
//! | Check | What must hold |
//! | --- | --- |
//! | C1, C2 | [`static_checks`](crate::static_checks) on `fixtures.statics` |
//! | C3 | one result per spec in spec order - singly, as one batch, reversed, with every spec twice, and **one address read as two kinds giving two different values** (`fixtures.reads` must contain such a pair); `covers` in range and never repeated; `decode_read` / `Done` of the right length |
//! | C4 | a device refusal (`fixtures.refused`) is `ItemError::Device` and keeps the connection; a mismatched kind (`fixtures.unsupported`) is `ItemError::Unsupported` (or refused by `parse_address`); a broken response ([`Responder::reply_broken`]) is fatal `Framing` / `Protocol`; a peer closing mid-response is fatal; **the driver never returns `FatalError::Io` / `Timeout`** |
//! | C5 | responses delivered one byte at a time, and in pieces of 1, 2, 3, 5 bytes (handshake included), decode to the same values |
//! | C6 | a response arriving after the timeout fails that read (`Timeout`) and is never taken as the next read's: the next read runs on a new connection and gets its own value |
//! | C7 | after the peer closes, a new connection (new `create_connection`, new handshake) reads correctly, twice; a handshake ends within 32 steps |
//! | C9 | a response that keeps growing never yields the expected values and ends fatally (`max_len` / timeout); `fixtures.noise` before a `DriverDecides` response is discarded; `Complete` / `Discard` stay within the buffer; steps are receivable (`Fixed(n)` within `max_len`, ...) |
//! | C10 | (`capabilities.write` only) all of `fixtures.writes` - pairwise non-adjacent addresses - written as one batch succeed, no write step covers two of them, and each reads back as written; the step count stays within `3 × items + 8` (C16) |
//! | C11 | 1,000 random byte strings (fixed seed, up to `max_len`, at most 4 KiB) into `decode_read`, `is_complete` (for `DriverDecides` steps) and `handshake_next` never panic |
//! | C12 | a string of 0 words, or of `fixtures.max_string_words + 1`, is `ItemError::Unsupported` (or refused by `parse_address`) |
//!
//! C8 (one socket per connection) is session-type only, and the write
//! state-machine checks C13-C16 need a word-bit fixture set that arrives with
//! R1-5 / R2b-3 (design §7.3).

mod engine;

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Duration;

use banto_driver_api::{
    FatalError, FrameCheck, FrameDriver, FrameRule, ItemError, RawValue, StringEncoding, TagKind,
    TagSpec, Value, WriteItem,
};

use crate::{Failure, StaticFixtures};
use engine::{Engine, Fatal, Once, Split};

/// The device side of the harness's in-memory wire.
///
/// Answer every request like a healthy device would; the harness injects
/// faults itself. Requests arrive whole (the engine owns framing of what is
/// *sent*).
pub trait Responder {
    /// A new connection was opened (forget per-connection state such as a
    /// session id). Default: nothing.
    fn on_connect(&mut self) {}

    /// The response to `request`.
    fn reply(&mut self, request: &[u8]) -> Reply;

    /// A response to `request` that a correct driver must reject as broken
    /// (a wrong response code, a length that contradicts the header): C4.
    /// `None` makes C4 report that it could not check this. Default: `None`.
    fn reply_broken(&mut self, _request: &[u8]) -> Option<Reply> {
        None
    }
}

/// What a [`Responder`] sends back.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// These bytes, at once.
    Bytes(Vec<u8>),
    /// These pieces, one every `Duration` (virtual time).
    Chunks(Vec<Vec<u8>>, Duration),
    /// Nothing (the request times out).
    Silence,
    /// The peer closes the connection.
    Close,
}

/// A read whose value the responder is known to return.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct ReadCase {
    /// The address as written.
    pub text: String,
    /// How to read it.
    pub kind: TagKind,
    /// What the driver must decode.
    pub expected: RawValue,
}

impl ReadCase {
    /// Reading `text` as `kind` gives `expected`.
    pub fn new(text: &str, kind: TagKind, expected: RawValue) -> Self {
        ReadCase {
            text: text.to_owned(),
            kind,
            expected,
        }
    }
}

/// A write the responder accepts.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct WriteCase {
    /// The address as written.
    pub text: String,
    /// Its kind.
    pub kind: TagKind,
    /// What to write.
    pub value: Value,
}

impl WriteCase {
    /// Writing `value` to `text` as `kind` succeeds.
    pub fn new(text: &str, kind: TagKind, value: Value) -> Self {
        WriteCase {
            text: text.to_owned(),
            kind,
            value,
        }
    }
}

/// Inputs for [`run_all`], supplied by the driver's author.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct FrameFixtures {
    /// C1 / C2, and the settings every connection is created with.
    pub statics: StaticFixtures,
    /// Reads with known values (C3-C12). Must contain one address read as
    /// two kinds with different expected values (C3).
    pub reads: Vec<ReadCase>,
    /// `(text, kind)` the device refuses with a complete response (C4).
    pub refused: Vec<(String, TagKind)>,
    /// `(text, kind)` that parse but cannot be read as that kind (C4).
    pub unsupported: Vec<(String, TagKind)>,
    /// Writes with **pairwise non-adjacent** addresses (C10). At least two
    /// when the driver can write.
    pub writes: Vec<WriteCase>,
    /// Bytes the driver must skip when they precede a `DriverDecides`
    /// response (RS-485 echo, line noise). Empty: C9's noise case is not
    /// applicable.
    pub noise: Vec<u8>,
    /// The longest string, in words (C12 tries one more). Default 128, the
    /// registry's `string_length` cap.
    pub max_string_words: u16,
    /// The engine's response timeout (virtual time). Default 1 s.
    pub response_timeout: Duration,
}

impl FrameFixtures {
    /// Fixtures with `statics` and no cases.
    pub fn new(statics: StaticFixtures) -> Self {
        FrameFixtures {
            statics,
            reads: Vec::new(),
            refused: Vec::new(),
            unsupported: Vec::new(),
            writes: Vec::new(),
            noise: Vec::new(),
            max_string_words: 128,
            response_timeout: Duration::from_secs(1),
        }
    }

    /// Add a read with a known value.
    pub fn read(mut self, text: &str, kind: TagKind, expected: RawValue) -> Self {
        self.reads.push(ReadCase::new(text, kind, expected));
        self
    }

    /// Add an address the device refuses.
    pub fn refused(mut self, text: &str, kind: TagKind) -> Self {
        self.refused.push((text.to_owned(), kind));
        self
    }

    /// Add an address that cannot be read as `kind`.
    pub fn unsupported(mut self, text: &str, kind: TagKind) -> Self {
        self.unsupported.push((text.to_owned(), kind));
        self
    }

    /// Add a write (keep the addresses pairwise non-adjacent).
    pub fn write(mut self, text: &str, kind: TagKind, value: Value) -> Self {
        self.writes.push(WriteCase::new(text, kind, value));
        self
    }

    /// Set the noise bytes for C9.
    pub fn noise(mut self, noise: Vec<u8>) -> Self {
        self.noise = noise;
        self
    }
}

/// Run C1-C7 and C9-C12 against `driver` with `peer` and return every
/// failure (design §7.2). An empty result means the driver conforms.
///
/// `async` so that later checks can involve real transports without an API
/// change; this version runs entirely in memory.
pub async fn run_all(
    driver: &dyn FrameDriver,
    fixtures: &FrameFixtures,
    peer: &mut dyn Responder,
) -> Vec<Failure> {
    let mut failures = crate::static_checks(driver, &fixtures.statics);
    let mut engine = Engine::new(
        driver,
        fixtures.statics.settings.clone(),
        peer,
        fixtures.response_timeout,
    );
    let reads = prepare_reads(driver, fixtures, &mut failures);
    if reads.is_empty() {
        failures.push(Failure::new(
            "C3",
            "fixtures.reads が空か、どれも parse_address を通らない（C3〜C12 を確かめられない）",
        ));
    } else {
        let mut cx = Cx {
            driver,
            fixtures,
            reads: &reads,
            engine: &mut engine,
            failures: &mut failures,
        };
        cx.c3_count_order_and_kind();
        cx.c4_classification();
        cx.c5_split();
        cx.c6_late_response();
        cx.c7_reconnect();
        cx.c9_bounded_receive();
        cx.c10_non_coalesced_writes();
        cx.c11_no_panics();
        cx.c12_string_span();
    }
    failures.extend(engine.into_violations());
    failures
}

/// A read case with its parsed spec.
struct Prepared<'f> {
    case: &'f ReadCase,
    spec: TagSpec,
}

fn prepare_reads<'f>(
    driver: &dyn FrameDriver,
    fixtures: &'f FrameFixtures,
    failures: &mut Vec<Failure>,
) -> Vec<Prepared<'f>> {
    let mut out = Vec::new();
    for case in &fixtures.reads {
        match driver.parse_address(&case.text, &case.kind) {
            Ok(address) => out.push(Prepared {
                case,
                spec: TagSpec::new(address, case.kind.clone()),
            }),
            Err(e) => failures.push(Failure::new(
                "C3",
                format!("fixtures.reads の {:?} が読めない: {e}", case.text),
            )),
        }
    }
    out
}

struct Cx<'r, 'e, 'f> {
    driver: &'r dyn FrameDriver,
    fixtures: &'f FrameFixtures,
    reads: &'r [Prepared<'f>],
    engine: &'r mut Engine<'e>,
    failures: &'r mut Vec<Failure>,
}

impl Cx<'_, '_, '_> {
    fn fail(&mut self, id: &'static str, detail: String) {
        self.failures.push(Failure::new(id, detail));
    }

    /// Read the cases at `idx` as one batch; push a failure and return false
    /// unless every value is the expected one.
    fn expect_values(&mut self, id: &'static str, label: &str, idx: &[usize]) -> bool {
        let specs: Vec<TagSpec> = idx.iter().map(|&i| self.reads[i].spec.clone()).collect();
        match self.engine.read(&specs) {
            Ok(values) => {
                let mut ok = true;
                for (&i, got) in idx.iter().zip(&values) {
                    let case = self.reads[i].case;
                    if got.as_ref() != Ok(&case.expected) {
                        ok = false;
                        self.fail(
                            id,
                            format!(
                                "{label}: {:?}（{:?}）が {got:?}（期待は {:?}）",
                                case.text, case.kind, case.expected
                            ),
                        );
                    }
                }
                ok
            }
            Err(f) => {
                self.fail(id, format!("{label}: 読み取りが致命になった: {f:?}"));
                false
            }
        }
    }

    fn all(&self) -> Vec<usize> {
        (0..self.reads.len()).collect()
    }

    fn c3_count_order_and_kind(&mut self) {
        for i in self.all() {
            self.expect_values("C3", "1 件ずつ", &[i]);
        }
        let all = self.all();
        self.expect_values("C3", "まとめて", &all);
        let reversed: Vec<usize> = all.iter().rev().copied().collect();
        self.expect_values("C3", "逆順", &reversed);
        let twice: Vec<usize> = all.iter().chain(all.iter()).copied().collect();
        self.expect_values("C3", "同じ要求を 2 回ずつ", &twice);

        // Every "one address, two kinds, two values" pair (at least one).
        let mut pairs = Vec::new();
        for i in 0..self.reads.len() {
            for j in i + 1..self.reads.len() {
                let (a, b) = (&self.reads[i], &self.reads[j]);
                if a.spec.address.canonical == b.spec.address.canonical
                    && a.case.kind != b.case.kind
                    && a.case.expected != b.case.expected
                {
                    pairs.push((i, j));
                }
            }
        }
        if pairs.is_empty() {
            self.fail(
                "C3",
                "fixtures.reads に「同じアドレスを別の型で読むと別の値」の組が無い".into(),
            );
        }
        for (i, j) in pairs {
            self.expect_values("C3", "同じアドレス・別の型", &[i, j, i]);
        }
    }

    fn c4_classification(&mut self) {
        let refused = self.fixtures.refused.clone();
        for (text, kind) in &refused {
            let address = match self.driver.parse_address(text, kind) {
                Ok(a) => a,
                Err(e) => {
                    self.fail(
                        "C4",
                        format!("fixtures.refused の {text:?} が読めない: {e}"),
                    );
                    continue;
                }
            };
            if !self.expect_values("C4", "拒否の前の読み取り", &[0]) {
                continue;
            }
            let before = self.engine.connects;
            match self.engine.read(&[TagSpec::new(address, kind.clone())]) {
                Ok(v) if matches!(v.as_slice(), [Err(ItemError::Device(_))]) => {}
                other => self.fail(
                    "C4",
                    format!("機器が断る {text:?} が {other:?}（ItemError::Device であるべき）"),
                ),
            }
            if !self.engine.is_connected() {
                self.fail("C4", format!("機器が断った {text:?} で接続を切った"));
            }
            self.expect_values("C4", "拒否の後の読み取り", &[0]);
            if self.engine.connects != before {
                self.fail(
                    "C4",
                    format!("機器が断った {text:?} の後に接続を作り直した（非致命のはず）"),
                );
            }
        }

        let unsupported = self.fixtures.unsupported.clone();
        for (text, kind) in &unsupported {
            let Ok(address) = self.driver.parse_address(text, kind) else {
                continue; // refused before any connection: also fine
            };
            match self.engine.read(&[TagSpec::new(address, kind.clone())]) {
                Ok(v) if matches!(v.as_slice(), [Err(ItemError::Unsupported(_))]) => {}
                other => self.fail(
                    "C4",
                    format!(
                        "{text:?}（{kind:?}）が {other:?}（ItemError::Unsupported であるべき）"
                    ),
                ),
            }
        }

        // A broken response.
        self.engine.fired = false;
        self.engine.once = Some(Once::Broken);
        let result = self.engine.read(&[self.reads[0].spec.clone()]);
        self.engine.once = None;
        if !self.engine.fired {
            self.fail(
                "C4",
                "Responder::reply_broken が None で、壊れたフレームの扱いを確かめられない".into(),
            );
        } else {
            match result {
                Err(Fatal {
                    error: FatalError::Framing(_) | FatalError::Protocol(_),
                    by_driver: true,
                })
                | Err(Fatal {
                    error: FatalError::Framing(_),
                    by_driver: false,
                }) => {}
                other => self.fail(
                    "C4",
                    format!("壊れた応答が {other:?}（致命の Framing / Protocol であるべき）"),
                ),
            }
        }
        self.expect_values("C4", "壊れた応答の後の読み取り", &[0]);

        // The peer closes halfway through a response.
        self.engine.once = Some(Once::CloseMidway);
        match self.engine.read(&[self.reads[0].spec.clone()]) {
            Err(_) => {}
            Ok(v) => self.fail("C4", format!("途中で切れた応答から値を作った: {v:?}")),
        }
        self.engine.once = None;
        self.expect_values("C4", "途中で切れた後の読み取り", &[0]);
    }

    fn c5_split(&mut self) {
        let all = self.all();
        for (split, label) in [
            (Split::Every(1), "1 バイトずつ届く"),
            (Split::Straddle, "1・2・3・5 バイトに分かれて届く"),
        ] {
            // Reconnect so that the handshake is split too.
            self.engine.drop_connection();
            self.engine.split = Some(split);
            self.expect_values("C5", label, &all);
            let reversed: Vec<usize> = all.iter().rev().copied().collect();
            self.expect_values("C5", label, &reversed);
        }
        self.engine.split = None;
    }

    fn c6_late_response(&mut self) {
        if !self.expect_values("C6", "遅れの前の読み取り", &[0]) {
            return;
        }
        let next = (1..self.reads.len())
            .find(|&i| self.reads[i].case.expected != self.reads[0].case.expected)
            .unwrap_or(0);
        self.engine.once = Some(Once::Late);
        match self.engine.read(&[self.reads[0].spec.clone()]) {
            Err(Fatal {
                error: FatalError::Timeout,
                by_driver: false,
            }) => {}
            other => self.fail(
                "C6",
                format!("時間切れのはずの読み取りが {other:?}（Timeout であるべき）"),
            ),
        }
        self.engine.once = None;
        let before = self.engine.connects;
        if self.expect_values("C6", "遅れた応答の次の読み取り", &[next])
            && self.engine.connects != before + 1
        {
            self.fail(
                "C6",
                "時間切れの後、新しい接続を作らずに次の要求を送った".into(),
            );
        }
    }

    fn c7_reconnect(&mut self) {
        let all = self.all();
        for round in 1..=2 {
            if !self.expect_values("C7", "切断の前の読み取り", &[0]) {
                return;
            }
            let before = self.engine.connects;
            self.engine.once = Some(Once::Close);
            if let Ok(v) = self.engine.read(&[self.reads[0].spec.clone()]) {
                self.fail("C7", format!("相手が閉じたのに値が返った: {v:?}"));
            }
            self.engine.once = None;
            let label = format!("{round} 回目の再接続の後");
            if self.expect_values("C7", &label, &all) && self.engine.connects != before + 1 {
                self.fail(
                    "C7",
                    format!(
                        "{label}: 接続を作り直していない（{before} → {}）",
                        self.engine.connects
                    ),
                );
            }
        }
    }

    fn c9_bounded_receive(&mut self) {
        self.engine.once = Some(Once::Endless);
        if let Ok(values) = self.engine.read(&[self.reads[0].spec.clone()]) {
            if values.first().map(Result::as_ref) == Some(Ok(&self.reads[0].case.expected)) {
                self.fail(
                    "C9",
                    format!("完結しないまま伸び続ける応答から値を作った: {values:?}"),
                );
            }
        }
        self.engine.once = None;
        self.expect_values("C9", "伸び続ける応答の後の読み取り", &[0]);

        if !self.fixtures.noise.is_empty() {
            self.engine.fired = false;
            self.engine.once = Some(Once::Noise(self.fixtures.noise.clone()));
            let all = self.all();
            // Applied only to a DriverDecides step; without one it stays
            // unused (not applicable) and is cleared here.
            self.expect_values("C9", "先頭のごみ（Discard）", &all);
            self.engine.once = None;
        }
    }

    fn c10_non_coalesced_writes(&mut self) {
        if !self.driver.descriptor().capabilities.write {
            return;
        }
        if self.fixtures.writes.len() < 2 {
            self.fail(
                "C10",
                "書けるドライバなのに fixtures.writes が 2 件未満（離れた 2 つ以上が要る）".into(),
            );
            return;
        }
        let mut items = Vec::new();
        for w in &self.fixtures.writes {
            match self.driver.parse_address(&w.text, &w.kind) {
                Ok(a) => items.push(WriteItem::new(
                    TagSpec::new(a, w.kind.clone()),
                    w.value.clone(),
                )),
                Err(e) => {
                    self.fail(
                        "C10",
                        format!("fixtures.writes の {:?} が読めない: {e}", w.text),
                    );
                    return;
                }
            }
        }
        self.engine.write_log.clear();
        match self.engine.write(&items) {
            Ok(results) => {
                for (w, r) in self.fixtures.writes.iter().zip(&results) {
                    if r.is_err() {
                        self.fail("C10", format!("{:?} への書き込みが {r:?}", w.text));
                    }
                }
            }
            Err(f) => {
                self.fail("C10", format!("書き込みが致命になった: {f:?}"));
                return;
            }
        }
        let log = std::mem::take(&mut self.engine.write_log);
        for covers in &log {
            let mut distinct = covers.clone();
            distinct.sort_unstable();
            distinct.dedup();
            if distinct.len() > 1 {
                self.fail(
                    "C10",
                    format!("離れた書き込み {distinct:?} を 1 つの step にまとめた"),
                );
            }
        }
        let specs: Vec<TagSpec> = items.iter().map(|i| i.spec.clone()).collect();
        match self.engine.read(&specs) {
            Ok(values) => {
                for (w, got) in self.fixtures.writes.iter().zip(&values) {
                    let ok = matches!(got, Ok(raw) if raw_matches(raw, &w.value));
                    if !ok {
                        self.fail(
                            "C10",
                            format!(
                                "{:?} に {:?} を書いた後の読み取りが {got:?}",
                                w.text, w.value
                            ),
                        );
                    }
                }
            }
            Err(f) => self.fail("C10", format!("書いた後の読み取りが致命になった: {f:?}")),
        }
    }

    fn c11_no_panics(&mut self) {
        const CASES: usize = 1000;
        const CAP: usize = 4096;
        let specs: Vec<TagSpec> = self.reads.iter().map(|p| p.spec.clone()).collect();
        let planned = match self.engine.plan_only(&specs) {
            Ok(p) if !p.is_empty() => p,
            Ok(_) => return,
            Err(f) => {
                self.fail("C11", format!("plan_read が致命になった: {f:?}"));
                return;
            }
        };
        let mut rng = XorShift(0x2545_F491_4F6C_DD1D);
        let mut decode_panicked = false;
        let mut complete_panicked = false;
        let mut io_reported = false;
        if let Some(conn) = self.engine.connection() {
            for i in 0..CASES {
                let p = &planned[i % planned.len()];
                let bytes = rng.bytes((p.step.max_len as usize).min(CAP));
                if !decode_panicked {
                    let r = catch_unwind(AssertUnwindSafe(|| conn.decode_read(p, &specs, &bytes)));
                    match r {
                        Err(_) => {
                            decode_panicked = true;
                            self.failures.push(Failure::new(
                                "C11",
                                format!("decode_read が {} バイトの入力で panic した", bytes.len()),
                            ));
                        }
                        Ok(Err(e @ (FatalError::Io(_) | FatalError::Timeout))) if !io_reported => {
                            io_reported = true;
                            self.failures.push(Failure::new(
                                "C4",
                                format!("decode_read が {e:?} を返した（フレーム型は返さない）"),
                            ));
                        }
                        Ok(_) => {}
                    }
                }
                if p.step.expect == FrameRule::DriverDecides
                    && !bytes.is_empty()
                    && !complete_panicked
                {
                    match catch_unwind(AssertUnwindSafe(|| conn.is_complete(&p.step, &bytes))) {
                        Err(_) => {
                            complete_panicked = true;
                            self.failures.push(Failure::new(
                                "C11",
                                format!("is_complete が {} バイトの入力で panic した", bytes.len()),
                            ));
                        }
                        Ok(Ok(FrameCheck::Complete { len }))
                            if len == 0 || len as usize > bytes.len() =>
                        {
                            self.failures.push(Failure::new(
                                "C9",
                                format!(
                                    "is_complete が Complete {{ len: {len} }}（受信は {} バイト）",
                                    bytes.len()
                                ),
                            ));
                        }
                        Ok(Ok(FrameCheck::Discard(n))) if n == 0 || n as usize > bytes.len() => {
                            self.failures.push(Failure::new(
                                "C9",
                                format!(
                                    "is_complete が Discard({n})（受信は {} バイト）",
                                    bytes.len()
                                ),
                            ));
                        }
                        Ok(_) => {}
                    }
                }
            }
        }

        // handshake_next with an arbitrary "response" to its first step.
        for _ in 0..CASES {
            let Ok(mut conn) = self
                .driver
                .create_connection(&self.fixtures.statics.settings)
            else {
                return;
            };
            let first = match conn.handshake_next(None) {
                Ok(Some(step)) => step,
                _ => return, // no handshake (or none to answer): nothing to fuzz
            };
            let bytes = rng.bytes((first.max_len as usize).min(CAP));
            if catch_unwind(AssertUnwindSafe(|| conn.handshake_next(Some(&bytes)))).is_err() {
                self.fail(
                    "C11",
                    format!(
                        "handshake_next が {} バイトの入力で panic した",
                        bytes.len()
                    ),
                );
                return;
            }
        }
    }

    fn c12_string_span(&mut self) {
        let text = self
            .reads
            .iter()
            .find(|p| matches!(p.case.kind, TagKind::Str { .. }))
            .unwrap_or(&self.reads[0])
            .case
            .text
            .clone();
        for words in [0, self.fixtures.max_string_words.saturating_add(1)] {
            let kind = TagKind::string(words, StringEncoding::Utf8);
            let Ok(address) = self.driver.parse_address(&text, &kind) else {
                continue; // refused before any connection: also fine
            };
            match self.engine.read(&[TagSpec::new(address, kind)]) {
                Ok(v) if matches!(v.as_slice(), [Err(ItemError::Unsupported(_))]) => {}
                other => self.fail(
                    "C12",
                    format!("{words} 語の文字列 {text:?} が {other:?}（ItemError::Unsupported であるべき）"),
                ),
            }
        }
    }
}

/// Whether a decoded raw value is what was written.
fn raw_matches(raw: &RawValue, value: &Value) -> bool {
    match (raw, value) {
        (RawValue::Bool(a), Value::Bit(b)) => a == b,
        (RawValue::Int(a), Value::F64(b)) => *a as f64 == *b,
        (RawValue::Uint(a), Value::F64(b)) => *a as f64 == *b,
        // An F32 tag holds the written value rounded to f32.
        (RawValue::Float(a), Value::F64(b)) => a == b || *a as f32 == *b as f32,
        (RawValue::Bytes(a), Value::Str(s)) => {
            let end = a.iter().position(|&b| b == 0).unwrap_or(a.len());
            &a[..end] == s.as_bytes()
        }
        _ => false,
    }
}

/// A tiny fixed-seed PRNG (xorshift64): C11 must be reproducible and the
/// crate takes no `rand` dependency.
struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// A random byte string of length `0..=max`.
    fn bytes(&mut self, max: usize) -> Vec<u8> {
        let len = (self.next() % (max as u64 + 1)) as usize;
        (0..len).map(|_| self.next() as u8).collect()
    }
}
