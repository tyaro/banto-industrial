//! フレーム型の適合テスト。FrameSession の本番実行系を、記憶上の通信路と
//! 止めた Tokio の時計で駆動する（設計 §5.6、api-design §7.1・§7.2）。
//!
//! Responder は正常な機器として応え、分割・遅延・伸び続ける応答・先頭のごみ・
//! 壊れた応答・途中の切断は通信路が挟む。実時間の sleep は使わない。
//! 契約違反は本番の EngineEvents から収集する。C11 の純粋な呼び出しは
//! ハーネスが直接調べ、RawValue の比較は f64 や文字列への変換前に行う。
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
//! | C10 | (`capabilities.write` only) all of `fixtures.writes` - pairwise non-adjacent addresses - written as one batch succeed, no write step covers two of them, and each reads back as its `expected` value; the step count stays within `3 × items + 8` (C16) |
//! | C11 | 1,000 random byte strings (fixed seed, up to `max_len`, at most 4 KiB) into `decode_read`, `is_complete` (for `DriverDecides` steps) and `handshake_next` never panic |
//! | C12 | a string of 0 words, or of `fixtures.max_string_words + 1`, is `ItemError::Unsupported` (or refused by `parse_address`) |
//!
//! ## Comparing a value with its expected one
//!
//! C3's reads, C10's read-backs (and C9's "must not yield the expected
//! value") compare what the driver decoded with the fixture's expected
//! [`RawValue`] by **one rule that depends on the tag's [`TagKind`]**:
//!
//! | Kind | Match when |
//! | --- | --- |
//! | `Numeric(Bit)` | both `Bool` and equal |
//! | `Numeric(F32)` | both `Float` and bit-equal **after rounding each to `f32`** (an `F32` tag holds the written value rounded) |
//! | `Numeric(F64)` | both `Float` and **bit-equal** (`f64::to_bits`: no tolerance; a NaN matches the same NaN, `0.0` does not match `-0.0`) |
//! | the integer types | both `Int` or both `Uint`, and equal |
//! | `Str { words, .. }` | both `Bytes`; the decoded bytes are at most `words × 2` long, and the two are equal **up to their first NUL** (the host's decode rule: it cuts there, so padding after the NUL - or none at all - is the same string). The bytes are compared as given, never re-encoded: the fixture states them in the tag's `encoding` (design §3.2) |
//!
//! Anything else (a variant that does not fit the kind) is a mismatch.
//!
//! C8 (one socket per connection) is session-type only
//! ([`session::run_all_against`](crate::session::run_all_against), which
//! also runs C13-C15 for session-type drivers since R1-5); the frame-type
//! C13〜C15 の Frame 実装は後続へ、broker と組み合わせる C16 は 3b-join へ残す。

mod engine;

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Duration;

use banto_driver_api::{
    DataType, FatalError, FrameCheck, FrameDriver, FrameRule, ItemError, RawValue, StringEncoding,
    TagKind, TagSpec, Value, WriteItem,
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
    /// What the driver must decode, compared by the kind's rule (see the
    /// [module docs](self#comparing-a-value-with-its-expected-one)).
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

/// A write the responder accepts, and what reading it back gives.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct WriteCase {
    /// The address as written.
    pub text: String,
    /// Its kind.
    pub kind: TagKind,
    /// What to write.
    pub value: Value,
    /// What the driver must decode when it reads `text` as `kind` after the
    /// write, compared by the kind's rule like [`ReadCase::expected`] (see
    /// the [module docs](self#comparing-a-value-with-its-expected-one)). The
    /// harness does not derive it from `value`: a string's bytes are in the
    /// tag's encoding (`"あ"` in Shift_JIS is `Bytes(vec![0x82, 0xA0])`),
    /// which the harness does not encode.
    pub expected: RawValue,
}

impl WriteCase {
    /// Writing `value` to `text` as `kind` succeeds, and reading it back
    /// gives `expected`.
    pub fn new(text: &str, kind: TagKind, value: Value, expected: RawValue) -> Self {
        WriteCase {
            text: text.to_owned(),
            kind,
            value,
            expected,
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

    /// Add a write and what reading it back gives (keep the addresses
    /// pairwise non-adjacent).
    pub fn write(mut self, text: &str, kind: TagKind, value: Value, expected: RawValue) -> Self {
        self.writes
            .push(WriteCase::new(text, kind, value, expected));
        self
    }

    /// Set the noise bytes for C9.
    pub fn noise(mut self, noise: Vec<u8>) -> Self {
        self.noise = noise;
        self
    }
}

/// C1〜C7・C9〜C12 を検証する。空ならすべて適合した（設計 §7.2）。
/// 本物のエンジンを別スレッドの止めた時計で駆動するため、呼び出し元の
/// Tokio runtime の種類や時計の状態には依存しない。公開の async API は維持する。
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
                    if !matches!(got, Ok(raw) if raw_matches(&case.kind, raw, &case.expected)) {
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
            let case = self.reads[0].case;
            if matches!(values.first(), Some(Ok(raw)) if raw_matches(&case.kind, raw, &case.expected))
            {
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
                    let ok = matches!(got, Ok(raw) if raw_matches(&w.kind, raw, &w.expected));
                    if !ok {
                        self.fail(
                            "C10",
                            format!(
                                "{:?}（{:?}）に {:?} を書いた後の読み取りが {got:?}（期待は {:?}）",
                                w.text, w.kind, w.value, w.expected
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
                        Ok(Err(e @ (FatalError::Io(_) | FatalError::Timeout))) if !io_reported => {
                            io_reported = true;
                            self.failures.push(Failure::new(
                                "C4",
                                format!("is_complete が {e:?} を返した（フレーム型は返さない）"),
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
            match catch_unwind(AssertUnwindSafe(|| conn.handshake_next(Some(&bytes)))) {
                Err(_) => {
                    self.fail(
                        "C11",
                        format!(
                            "handshake_next が {} バイトの入力で panic した",
                            bytes.len()
                        ),
                    );
                    return;
                }
                Ok(Err(e @ (FatalError::Io(_) | FatalError::Timeout))) if !io_reported => {
                    io_reported = true;
                    self.fail(
                        "C4",
                        format!("handshake_next が {e:?} を返した（フレーム型は返さない）"),
                    );
                }
                Ok(_) => {}
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

/// Whether `got`, decoded as `kind`, is the fixture's `expected` - the one
/// rule for C3, C9 and C10 (the table in the module docs). Taking the kind
/// is the point: `f32` rounding is tolerated only for an `F32` tag, and a
/// string's bytes are compared as given (in the tag's encoding), never
/// against a UTF-8 rendering of the written text.
fn raw_matches(kind: &TagKind, got: &RawValue, expected: &RawValue) -> bool {
    match (kind, got, expected) {
        (TagKind::Numeric(DataType::Bit), RawValue::Bool(g), RawValue::Bool(e)) => g == e,
        (TagKind::Numeric(DataType::F32), RawValue::Float(g), RawValue::Float(e)) => {
            (*g as f32).to_bits() == (*e as f32).to_bits()
        }
        (TagKind::Numeric(DataType::F64), RawValue::Float(g), RawValue::Float(e)) => {
            g.to_bits() == e.to_bits()
        }
        (TagKind::Numeric(t), RawValue::Int(g), RawValue::Int(e))
            if !matches!(t, DataType::Bit | DataType::F32 | DataType::F64) =>
        {
            g == e
        }
        (TagKind::Numeric(t), RawValue::Uint(g), RawValue::Uint(e))
            if !matches!(t, DataType::Bit | DataType::F32 | DataType::F64) =>
        {
            g == e
        }
        (TagKind::Str { words, .. }, RawValue::Bytes(g), RawValue::Bytes(e)) => {
            g.len() <= usize::from(*words) * 2 && up_to_nul(g) == up_to_nul(e)
        }
        _ => false,
    }
}

/// `bytes` up to (not including) the first NUL: what the host decodes.
fn up_to_nul(bytes: &[u8]) -> &[u8] {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    &bytes[..end]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_matches_follows_the_kind() {
        use DataType::*;
        use RawValue::*;
        let n = TagKind::numeric;
        let sjis = TagKind::string(2, StringEncoding::ShiftJis);
        let f32_of = |v: f64| Float(f64::from(v as f32));
        // (kind, got, expected, match)
        let table: Vec<(TagKind, RawValue, RawValue, bool)> = vec![
            (n(Bit), Bool(true), Bool(true), true),
            (n(Bit), Bool(false), Bool(true), false),
            (n(Bit), Uint(1), Bool(true), false),
            // F32: rounding to f32 is tolerated, nothing more.
            (n(F32), f32_of(0.1), Float(0.1), true),
            (n(F32), Float(2.5), Float(2.5), true),
            (n(F32), f32_of(0.1), Float(0.2), false),
            (n(F32), Uint(0x3FC0_0000), Float(1.5), false),
            // F64: bit-equal.
            (n(F64), Float(1.0), Float(1.000_000_000_1), false),
            (n(F64), f32_of(0.1), Float(0.1), false),
            (n(F64), Float(0.1), Float(0.1), true),
            (n(F64), Float(f64::NAN), Float(f64::NAN), true),
            (n(F64), Float(-0.0), Float(0.0), false),
            // Integers: same variant, equal.
            (n(U16), Uint(7), Uint(7), true),
            (n(U16), Uint(7), Uint(8), false),
            (n(U16), Int(7), Uint(7), false),
            (n(I32), Int(-100_000), Int(-100_000), true),
            (n(I16), Float(-1.0), Int(-1), false),
            // Strings: as given, up to the first NUL, within the span.
            (
                sjis.clone(),
                Bytes(vec![0x82, 0xA0, 0, 0]),
                Bytes(vec![0x82, 0xA0]),
                true,
            ),
            (
                sjis.clone(),
                Bytes(vec![0x82, 0xA0, 0, 0x41]),
                Bytes(vec![0x82, 0xA0, 0, 0]),
                true,
            ),
            (
                sjis.clone(),
                Bytes(vec![0x82, 0xA0]),
                Bytes(vec![0x82, 0xA0, 0, 0]),
                true,
            ),
            (
                sjis.clone(),
                Bytes(vec![0x82, 0xA0, 0, 0]),
                Bytes("あ".as_bytes().to_vec()),
                false,
            ),
            (
                sjis.clone(),
                Bytes(vec![0x82, 0xA0, 0, 0, 0]),
                Bytes(vec![0x82, 0xA0]),
                false,
            ),
            (
                sjis.clone(),
                Bytes(b"abcd".to_vec()),
                Bytes(b"abc".to_vec()),
                false,
            ),
            (sjis, Uint(0xA082), Bytes(vec![0x82, 0xA0]), false),
        ];
        for (kind, got, expected, want) in table {
            assert_eq!(
                raw_matches(&kind, &got, &expected),
                want,
                "{kind:?}: {got:?} vs {expected:?}"
            );
        }
    }
}
