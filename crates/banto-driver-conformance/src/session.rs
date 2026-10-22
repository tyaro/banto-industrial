//! The session-type harness: [`run_all_against`] plays the Hub's host for a
//! [`SessionDriver`] whose peer is already a TCP server - a device simulator
//! or a test device - reached through a [`FaultProxy`]
//! (docs/banto-driver-api-design.md §7.1-§7.3).
//!
//! The harness connects the driver to the proxy (it overwrites `host` and
//! `port` of the fixtures' settings with [`FaultProxy::local_addr`]), behaves
//! like the host (one session at a time, reads and writes serialized, a
//! connection-fatal `Err` drops the session and the next call connects
//! anew) and injects the faults through the proxy. Unlike
//! [`frame::run_all`](crate::frame::run_all) this is real socket I/O in real
//! time: every call is bounded (the fixtures' timeouts), every wait for an
//! event is a bounded poll of that event, and nothing sleeps to let
//! something happen.
//!
//! ## What [`run_all_against`] checks
//!
//! | Check | What must hold |
//! | --- | --- |
//! | C1, C2 | [`static_checks`](crate::static_checks) on `fixtures.statics` |
//! | C3 | one result per spec in spec order - singly, as one batch, reversed, with every spec twice, and **one address read as two kinds giving two different values** (`fixtures.reads` must contain such a pair) |
//! | C4 | a device refusal (`fixtures.refused`) is `ItemError::Device` and keeps the connection; a mismatched kind (`fixtures.unsupported`, `fixtures.unsupported_writes`) is `ItemError::Unsupported` (or refused by `parse_address`) and keeps the connection; a spec whose canonical address the driver never produced is `ItemError::Unsupported`, never a value and never fatal; a broken response (`fixtures.broken`) is fatal `Framing` / `Protocol`; a peer closing in the middle of a response is fatal; after each fatal error a new connection reads correctly |
//! | C5 | responses delivered in pieces of 1, 2, 3 and 5 bytes decode to the same values |
//! | C6 | a response held back past the response timeout makes that read connection-fatal (never a value); the late response then arrives on the old connection, and the next read - on a new connection - gets its own value |
//! | C7 | after the peer drops the connection the next call is fatal (never a value), and a new connection reads correctly - twice |
//! | C8 | 100 alternating reads and writes use exactly one accepted connection (#337), and every write reads back as written |
//! | C12 | a string of 0 words, or of `fixtures.max_string_words + 1`, is `ItemError::Unsupported` (or refused by `parse_address`) |
//! | C13 | for each [`BitWriteCase`]: on a word holding `0x0000`, `0x0100`, `0xFFFF` and `0x0008`, writing the bit `true` / `false` changes only that bit; writing two bits of the word in one batch sends no more than writing one (one read-modify-write) |
//! | C14 | (`verify_mismatch`) when the check read disagrees for one bit, that bit's request alone is `ItemError::Rejected`, its batch-mate succeeds, the connection is kept and the next write is correct |
//! | C15 | (`refusals`) a device refusal at any step of the read-modify-write makes the bit's request `ItemError::Device`, no further request is sent (counted by turn, [`FaultProxy::request_count`], so TCP segmentation does not matter), the connection is kept and the next write is correct |
//!
//! C9-C11 and C16 are frame-type checks (R2b-3). C13-C15 run only for the
//! [`BitWriteCase`]s the fixtures give: a driver without word-bit writes
//! leaves them empty and pins its refusal in `unsupported_writes` instead.
//!
//! ## Comparing a value with its expected one
//!
//! | Kind | Match when |
//! | --- | --- |
//! | `Numeric(Bit)` | both `Bit` and equal |
//! | `Numeric(F32)` | both `F64` and bit-equal after rounding each to `f32` |
//! | `Numeric(F64)` | both `F64` and bit-equal (`f64::to_bits`) |
//! | the integer types | both `F64` and equal |
//! | `Str { .. }` | both `Str` and equal (a session decodes the text itself and cuts it at the first NUL, design §3.2) |

use std::time::{Duration, Instant};

use banto_driver_api::{
    keys, ConnectContext, DataType, DriverAddress, FatalError, ItemError, Session, SessionDriver,
    Settings, StringEncoding, TagKind, TagSpec, Value, WriteItem,
};

use crate::proxy::{FaultProxy, Faults};
use crate::{Failure, StaticFixtures};

/// A read whose value the peer is known to return.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct ReadCase {
    /// The address as written.
    pub text: String,
    /// How to read it.
    pub kind: TagKind,
    /// What the session must return, compared by the kind's rule (see the
    /// [module docs](self#comparing-a-value-with-its-expected-one)).
    pub expected: Value,
}

impl ReadCase {
    /// Reading `text` as `kind` gives `expected`.
    pub fn new(text: &str, kind: TagKind, expected: Value) -> Self {
        ReadCase {
            text: text.to_owned(),
            kind,
            expected,
        }
    }
}

/// A write the peer accepts; reading `text` as `kind` afterwards gives
/// `value` back (compared by the kind's rule).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct WriteCase {
    /// The address as written.
    pub text: String,
    /// Its kind.
    pub kind: TagKind,
    /// What to write (and read back).
    pub value: Value,
}

impl WriteCase {
    /// Writing `value` to `text` as `kind` succeeds and reads back.
    pub fn new(text: &str, kind: TagKind, value: Value) -> Self {
        WriteCase {
            text: text.to_owned(),
            kind,
            value,
        }
    }
}

/// How to rewrite one response on its way to the driver: `bytes` over the
/// response at `offset`, for the `index`-th response (0 = the first) after
/// the harness arms it ([`Faults::patch_response`]). The bytes are the
/// protocol's, so the fixtures state them.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponsePatch {
    /// Which response, counted from 0 at the start of the call the harness
    /// arms it for.
    pub index: usize,
    /// Where in that response.
    pub offset: usize,
    /// The bytes to put there.
    pub bytes: Vec<u8>,
}

impl ResponsePatch {
    /// Put `bytes` at `offset` of the `index`-th response.
    pub fn new(index: usize, offset: usize, bytes: Vec<u8>) -> Self {
        ResponsePatch {
            index,
            offset,
            bytes,
        }
    }
}

/// One word with two of its bits, for the bit-in-word write checks
/// C13-C15.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BitWriteCase {
    /// The word, read and written as `Numeric(U16)` to set it up and to see
    /// what a bit write left in it.
    pub word: String,
    /// A bit of `word`, written as `Numeric(Bit)` (C13 uses bit 3, design
    /// §7.1). Its bit index is `parse_address(..).bit`.
    pub bit: String,
    /// Another bit of the same word (C13's one-RMW check, C14's batch-mate).
    pub other_bit: String,
    /// C14: rewrites the check read of the write "`bit` = `other_bit` =
    /// true" on a word holding `0x0000`, so that it shows `other_bit` set and
    /// `bit` clear. `None`: C14 is not checked for this word.
    pub verify_mismatch: Option<ResponsePatch>,
    /// C15: each turns one response of the write "`bit` = true" (the first
    /// read, the write-back, the check read) into a device refusal.
    pub refusals: Vec<ResponsePatch>,
}

impl BitWriteCase {
    /// `bit` and `other_bit` of `word`, without C14 / C15 patches.
    pub fn new(word: &str, bit: &str, other_bit: &str) -> Self {
        BitWriteCase {
            word: word.to_owned(),
            bit: bit.to_owned(),
            other_bit: other_bit.to_owned(),
            verify_mismatch: None,
            refusals: Vec::new(),
        }
    }

    /// Set C14's check-read rewrite.
    pub fn verify_mismatch(mut self, patch: ResponsePatch) -> Self {
        self.verify_mismatch = Some(patch);
        self
    }

    /// Add one of C15's refusals.
    pub fn refusal(mut self, patch: ResponsePatch) -> Self {
        self.refusals.push(patch);
        self
    }
}

/// Inputs for [`run_all_against`], supplied by the driver's author.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct SessionFixtures {
    /// C1 / C2, and the settings every connection is made with (`host` and
    /// `port` are replaced by the proxy's).
    pub statics: StaticFixtures,
    /// Reads with known values (C3-C8, C12). Must contain one address read
    /// as two kinds with different expected values (C3), and at least two
    /// different expected values (C6).
    pub reads: Vec<ReadCase>,
    /// `(text, kind)` the peer refuses with a complete response (C4). The
    /// peer is set up for that by the caller (a simulator's injected
    /// exception / end code).
    pub refused: Vec<(String, TagKind)>,
    /// `(text, kind)` that parse but cannot be read as that kind (C4).
    pub unsupported: Vec<(String, TagKind)>,
    /// Writes the peer accepts (C8). Required when the driver can write.
    pub writes: Vec<WriteCase>,
    /// `(text, kind, value)` that parse but cannot be written (C4).
    pub unsupported_writes: Vec<(String, TagKind, Value)>,
    /// How to break the next response so that a correct driver rejects it as
    /// a framing error (a wrong response code, a length that contradicts
    /// the header). `None`: C4 reports that it could not check this.
    pub broken: Option<ResponsePatch>,
    /// Words for the bit-in-word write checks (C13-C15). Empty for a driver
    /// without bit-in-word writes.
    pub bit_writes: Vec<BitWriteCase>,
    /// The longest string, in words (C12 tries one more). Default 128, the
    /// registry's `string_length` cap.
    pub max_string_words: u16,
    /// [`ConnectContext::connect_timeout`]. Default 2 s.
    pub connect_timeout: Duration,
    /// [`ConnectContext::response_timeout`]; also what C6 holds a response
    /// back past. Default 1 s.
    pub response_timeout: Duration,
}

impl SessionFixtures {
    /// Fixtures with `statics` and no cases.
    pub fn new(statics: StaticFixtures) -> Self {
        SessionFixtures {
            statics,
            reads: Vec::new(),
            refused: Vec::new(),
            unsupported: Vec::new(),
            writes: Vec::new(),
            unsupported_writes: Vec::new(),
            broken: None,
            bit_writes: Vec::new(),
            max_string_words: 128,
            connect_timeout: Duration::from_secs(2),
            response_timeout: Duration::from_secs(1),
        }
    }

    /// Add a read with a known value.
    pub fn read(mut self, text: &str, kind: TagKind, expected: Value) -> Self {
        self.reads.push(ReadCase::new(text, kind, expected));
        self
    }

    /// Add an address the peer refuses.
    pub fn refused(mut self, text: &str, kind: TagKind) -> Self {
        self.refused.push((text.to_owned(), kind));
        self
    }

    /// Add an address that cannot be read as `kind`.
    pub fn unsupported(mut self, text: &str, kind: TagKind) -> Self {
        self.unsupported.push((text.to_owned(), kind));
        self
    }

    /// Add a write the peer accepts.
    pub fn write(mut self, text: &str, kind: TagKind, value: Value) -> Self {
        self.writes.push(WriteCase::new(text, kind, value));
        self
    }

    /// Add a write that cannot be made as `kind`.
    pub fn unsupported_write(mut self, text: &str, kind: TagKind, value: Value) -> Self {
        self.unsupported_writes.push((text.to_owned(), kind, value));
        self
    }

    /// Set how to break a response (C4).
    pub fn broken(mut self, patch: ResponsePatch) -> Self {
        self.broken = Some(patch);
        self
    }

    /// Add a word for C13-C15.
    pub fn bit_write(mut self, case: BitWriteCase) -> Self {
        self.bit_writes.push(case);
        self
    }

    /// Set the longest string in words (C12).
    pub fn max_string_words(mut self, words: u16) -> Self {
        self.max_string_words = words;
        self
    }

    /// Set the response timeout.
    pub fn response_timeout(mut self, d: Duration) -> Self {
        self.response_timeout = d;
        self
    }
}

/// Run C1-C8 and C12-C15 against `driver`, connected through `proxy` to
/// the peer behind it, and return every failure (design §7.2, §7.3). An
/// empty result means the driver conforms.
///
/// `proxy` must relay to a running peer (`FaultProxy::start(peer)`); the
/// harness sets and clears its faults and leaves it with none. The caller
/// sets the peer up for `fixtures` (the values of `reads`, the refusals of
/// `refused`). Needs a tokio runtime with the time driver.
pub async fn run_all_against(
    driver: &dyn SessionDriver,
    fixtures: &SessionFixtures,
    proxy: &FaultProxy,
) -> Vec<Failure> {
    let mut failures = crate::static_checks(driver, &fixtures.statics);
    let mut settings = fixtures.statics.settings.clone();
    let local = proxy.local_addr();
    settings.insert(keys::HOST, local.ip().to_string());
    settings.insert(keys::PORT, local.port().to_string());
    proxy.set_faults(Faults::new());

    let reads = prepare_reads(driver, fixtures, &mut failures);
    if reads.is_empty() {
        failures.push(Failure::new(
            "C3",
            "fixtures.reads が空か、どれも parse_address を通らない（C3〜C15 を確かめられない）",
        ));
        return failures;
    }
    let mut host = Host {
        driver,
        settings,
        cx: ConnectContext::new(
            "banto-driver-conformance",
            fixtures.connect_timeout,
            fixtures.response_timeout,
        ),
        // A call that takes longer than this is reported as hung (the
        // driver must bound its own I/O with the context's timeouts).
        bound: fixtures.connect_timeout + fixtures.response_timeout * 16 + Duration::from_secs(2),
        session: None,
        parked: None,
        park_failed: false,
        connects: 0,
        violations: Vec::new(),
    };
    {
        let mut cx = Cx {
            driver,
            fixtures,
            reads: &reads,
            host: &mut host,
            proxy,
            failures: &mut failures,
        };
        cx.c3_count_order_and_kind().await;
        cx.c4_classification().await;
        cx.c5_split().await;
        cx.c6_late_response().await;
        cx.c7_reconnect().await;
        cx.c8_one_socket().await;
        cx.c12_string_span().await;
        for case in &fixtures.bit_writes {
            cx.c13_c15_bit_writes(case).await;
        }
    }
    proxy.set_faults(Faults::new());
    host.shutdown().await;
    failures.extend(host.violations);
    failures
}

/// A read case with its parsed spec.
struct Prepared<'f> {
    case: &'f ReadCase,
    spec: TagSpec,
}

fn prepare_reads<'f>(
    driver: &dyn SessionDriver,
    fixtures: &'f SessionFixtures,
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

/// Why a host call did not give results.
#[derive(Debug)]
enum HostError {
    /// `connect` failed (shown through `Debug` in failure messages).
    #[allow(dead_code)]
    Connect(FatalError),
    /// The call returned a connection-fatal error.
    Fatal(FatalError),
    /// The call did not return within the bound.
    Hung,
    /// `Ok` with the wrong number of results (also recorded as C3).
    WrongLength,
}

type ReadResults = Vec<Result<Value, ItemError>>;
type WriteResults = Vec<Result<(), ItemError>>;

/// The host side: one session at a time, dropped on a fatal error.
struct Host<'d> {
    driver: &'d dyn SessionDriver,
    settings: Settings,
    cx: ConnectContext,
    bound: Duration,
    session: Option<Box<dyn Session>>,
    /// C6: the session a fatal error ended, kept open (not dropped) while
    /// `park_failed` is set.
    parked: Option<Box<dyn Session>>,
    park_failed: bool,
    connects: usize,
    violations: Vec<Failure>,
}

impl Host<'_> {
    async fn take_session(&mut self) -> Result<Box<dyn Session>, HostError> {
        if let Some(s) = self.session.take() {
            return Ok(s);
        }
        let connect = self.driver.connect(&self.settings, self.cx.clone());
        match tokio::time::timeout(self.bound, connect).await {
            Ok(Ok(s)) => {
                self.connects += 1;
                Ok(s)
            }
            Ok(Err(e)) => Err(HostError::Connect(e)),
            Err(_) => Err(HostError::Hung),
        }
    }

    fn fail_session(&mut self, s: Box<dyn Session>) {
        if self.park_failed {
            self.parked = Some(s);
        }
    }

    fn is_connected(&self) -> bool {
        self.session.is_some()
    }

    /// Drop the live session (the next call connects anew).
    fn drop_session(&mut self) {
        self.session = None;
    }

    async fn read(&mut self, specs: &[TagSpec]) -> Result<ReadResults, HostError> {
        let mut s = self.take_session().await?;
        match tokio::time::timeout(self.bound, s.read_batch(specs)).await {
            Ok(Ok(v)) if v.len() == specs.len() => {
                self.session = Some(s);
                Ok(v)
            }
            Ok(Ok(v)) => {
                self.violations.push(Failure::new(
                    "C3",
                    format!(
                        "read_batch が {} 件の要求に {} 件の結果を返した",
                        specs.len(),
                        v.len()
                    ),
                ));
                Err(HostError::WrongLength)
            }
            Ok(Err(e)) => {
                self.fail_session(s);
                Err(HostError::Fatal(e))
            }
            Err(_) => Err(HostError::Hung),
        }
    }

    async fn write(&mut self, items: &[WriteItem]) -> Result<WriteResults, HostError> {
        let mut s = self.take_session().await?;
        match tokio::time::timeout(self.bound, s.write_batch(items)).await {
            Ok(Ok(v)) if v.len() == items.len() => {
                self.session = Some(s);
                Ok(v)
            }
            Ok(Ok(v)) => {
                self.violations.push(Failure::new(
                    "C3",
                    format!(
                        "write_batch が {} 件の要求に {} 件の結果を返した",
                        items.len(),
                        v.len()
                    ),
                ));
                Err(HostError::WrongLength)
            }
            Ok(Err(e)) => {
                self.fail_session(s);
                Err(HostError::Fatal(e))
            }
            Err(_) => Err(HostError::Hung),
        }
    }

    /// Disconnect the live session the way the host does when it stops.
    async fn shutdown(&mut self) {
        self.parked = None;
        if let Some(mut s) = self.session.take() {
            let _ = tokio::time::timeout(self.bound, s.disconnect()).await;
        }
    }
}

/// Poll `cond` until it holds or `bound` passes; whether it held.
async fn wait_until(bound: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + bound;
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

struct Cx<'r, 'd, 'f> {
    driver: &'d dyn SessionDriver,
    fixtures: &'f SessionFixtures,
    reads: &'r [Prepared<'f>],
    host: &'r mut Host<'d>,
    proxy: &'r FaultProxy,
    failures: &'r mut Vec<Failure>,
}

impl Cx<'_, '_, '_> {
    fn fail(&mut self, id: &'static str, detail: String) {
        self.failures.push(Failure::new(id, detail));
    }

    fn all(&self) -> Vec<usize> {
        (0..self.reads.len()).collect()
    }

    /// Read the cases at `idx` as one batch; push a failure and return false
    /// unless every value is the expected one.
    async fn expect_values(&mut self, id: &'static str, label: &str, idx: &[usize]) -> bool {
        let specs: Vec<TagSpec> = idx.iter().map(|&i| self.reads[i].spec.clone()).collect();
        match self.host.read(&specs).await {
            Ok(values) => {
                let mut ok = true;
                for (&i, got) in idx.iter().zip(&values) {
                    let case = self.reads[i].case;
                    if !matches!(got, Ok(v) if value_matches(&case.kind, v, &case.expected)) {
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
            Err(e) => {
                self.fail(id, format!("{label}: 読み取りが失敗した: {e:?}"));
                false
            }
        }
    }

    /// Parse `text` as `kind` for check `id`, reporting a failure.
    fn spec(&mut self, id: &'static str, text: &str, kind: &TagKind) -> Option<TagSpec> {
        match self.driver.parse_address(text, kind) {
            Ok(a) => Some(TagSpec::new(a, kind.clone())),
            Err(e) => {
                self.fail(id, format!("{text:?}（{kind:?}）が読めない: {e}"));
                None
            }
        }
    }

    async fn write_word(&mut self, id: &'static str, word: &TagSpec, value: u16) -> bool {
        let item = WriteItem::new(word.clone(), Value::F64(f64::from(value)));
        match self.host.write(&[item]).await {
            Ok(r) if matches!(r.as_slice(), [Ok(())]) => true,
            other => {
                self.fail(
                    id,
                    format!(
                        "{:?} に 0x{value:04X} を書く準備が {other:?}",
                        word.address.canonical
                    ),
                );
                false
            }
        }
    }

    async fn read_word(&mut self, id: &'static str, word: &TagSpec) -> Option<u16> {
        match self.host.read(std::slice::from_ref(word)).await {
            Ok(v) => match v.as_slice() {
                [Ok(Value::F64(x))] if (0.0..=65535.0).contains(x) && x.fract() == 0.0 => {
                    Some(*x as u16)
                }
                other => {
                    self.fail(
                        id,
                        format!("{:?} の読み取りが {other:?}", word.address.canonical),
                    );
                    None
                }
            },
            Err(e) => {
                self.fail(
                    id,
                    format!("{:?} の読み取りが失敗した: {e:?}", word.address.canonical),
                );
                None
            }
        }
    }

    async fn c3_count_order_and_kind(&mut self) {
        for i in self.all() {
            self.expect_values("C3", "1 件ずつ", &[i]).await;
        }
        let all = self.all();
        self.expect_values("C3", "まとめて", &all).await;
        let reversed: Vec<usize> = all.iter().rev().copied().collect();
        self.expect_values("C3", "逆順", &reversed).await;
        let twice: Vec<usize> = all.iter().chain(all.iter()).copied().collect();
        self.expect_values("C3", "同じ要求を 2 回ずつ", &twice)
            .await;

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
            self.expect_values("C3", "同じアドレス・別の型", &[i, j, i])
                .await;
        }
    }

    /// The connection was kept across a non-fatal outcome: same session, and
    /// the next read works on it.
    async fn expect_kept(&mut self, id: &'static str, label: &str, before: usize) {
        if !self.host.is_connected() {
            self.fail(id, format!("{label} で接続を切った（非致命のはず）"));
        }
        self.expect_values(id, &format!("{label} の後の読み取り"), &[0])
            .await;
        if self.host.connects != before {
            self.fail(
                id,
                format!("{label} の後に接続を作り直した（非致命のはず）"),
            );
        }
    }

    async fn c4_classification(&mut self) {
        // (a) Device refusals keep the connection.
        for (text, kind) in self.fixtures.refused.clone() {
            let Some(spec) = self.spec("C4", &text, &kind) else {
                continue;
            };
            if !self.expect_values("C4", "拒否の前の読み取り", &[0]).await {
                continue;
            }
            let before = self.host.connects;
            match self.host.read(&[spec]).await {
                Ok(v) if matches!(v.as_slice(), [Err(ItemError::Device(_))]) => {}
                other => self.fail(
                    "C4",
                    format!("機器が断る {text:?} が {other:?}（ItemError::Device であるべき）"),
                ),
            }
            self.expect_kept("C4", &format!("機器が断った {text:?}"), before)
                .await;
        }

        // Kind / area mismatches: Unsupported, connection kept.
        for (text, kind) in self.fixtures.unsupported.clone() {
            let Ok(address) = self.driver.parse_address(&text, &kind) else {
                continue; // refused before any connection: also fine
            };
            if !self
                .expect_values("C4", "型の不整合の前の読み取り", &[0])
                .await
            {
                continue;
            }
            let before = self.host.connects;
            match self.host.read(&[TagSpec::new(address, kind.clone())]).await {
                Ok(v) if matches!(v.as_slice(), [Err(ItemError::Unsupported(_))]) => {}
                other => self.fail(
                    "C4",
                    format!(
                        "{text:?}（{kind:?}）の読み取りが {other:?}（ItemError::Unsupported であるべき）"
                    ),
                ),
            }
            self.expect_kept("C4", &format!("型の合わない {text:?}"), before)
                .await;
        }
        for (text, kind, value) in self.fixtures.unsupported_writes.clone() {
            let Ok(address) = self.driver.parse_address(&text, &kind) else {
                continue;
            };
            if !self
                .expect_values("C4", "書けない書き込みの前の読み取り", &[0])
                .await
            {
                continue;
            }
            let before = self.host.connects;
            let item = WriteItem::new(TagSpec::new(address, kind.clone()), value.clone());
            match self.host.write(&[item]).await {
                Ok(v) if matches!(v.as_slice(), [Err(ItemError::Unsupported(_))]) => {}
                other => self.fail(
                    "C4",
                    format!(
                        "{text:?}（{kind:?}）への {value:?} の書き込みが {other:?}（ItemError::Unsupported であるべき）"
                    ),
                ),
            }
            self.expect_kept("C4", &format!("書けない {text:?}"), before)
                .await;
        }

        // A canonical address this driver never produced (after connect).
        let foreign = DriverAddress::new(
            self.driver.descriptor().id.clone(),
            "\u{1}banto-conformance-not-an-address",
            "?",
            false,
            1,
        );
        let kind = TagKind::numeric(DataType::U16);
        match self.host.read(&[TagSpec::new(foreign, kind)]).await {
            // R2b-2a (address design §6 Q5): one tag's address is that tag's
            // problem, so the connection is kept (the read below checks it).
            Ok(v) if matches!(v.as_slice(), [Err(ItemError::Unsupported(_))]) => {}
            other => self.fail(
                "C4",
                format!(
                    "読めない canonical のアドレスが {other:?}（ItemError::Unsupported であるべき。接続は切らない）"
                ),
            ),
        }
        self.expect_values("C4", "読めないアドレスの後の読み取り", &[0])
            .await;

        // (b) A broken response.
        match self.fixtures.broken.clone() {
            None => self.fail(
                "C4",
                "fixtures.broken が無く、壊れたフレームの扱いを確かめられない".into(),
            ),
            Some(patch) => {
                if self
                    .expect_values("C4", "壊れた応答の前の読み取り", &[0])
                    .await
                {
                    self.proxy.set_faults(Faults::new().patch_response(
                        patch.index,
                        patch.offset,
                        patch.bytes,
                    ));
                    let result = self.host.read(&[self.reads[0].spec.clone()]).await;
                    self.proxy.set_faults(Faults::new());
                    match result {
                        Err(HostError::Fatal(FatalError::Framing(_) | FatalError::Protocol(_))) => {
                        }
                        other => self.fail(
                            "C4",
                            format!(
                                "壊れた応答が {other:?}（致命の Framing / Protocol であるべき）"
                            ),
                        ),
                    }
                    self.host.drop_session();
                    self.expect_values("C4", "壊れた応答の後の読み取り", &[0])
                        .await;
                }
            }
        }

        // (c) The peer closes in the middle of a response.
        if self
            .expect_values("C4", "途中の切断の前の読み取り", &[0])
            .await
        {
            self.proxy.set_faults(Faults::new().cut_response(0, 1));
            let result = self.host.read(&[self.reads[0].spec.clone()]).await;
            self.proxy.set_faults(Faults::new());
            match result {
                Err(HostError::Fatal(_)) => {}
                other => self.fail(
                    "C4",
                    format!("応答の途中で相手が切断したのに {other:?}（致命であるべき）"),
                ),
            }
            self.host.drop_session();
            self.expect_values("C4", "途中の切断の後の読み取り", &[0])
                .await;
        }
    }

    async fn c5_split(&mut self) {
        let all = self.all();
        let reversed: Vec<usize> = all.iter().rev().copied().collect();
        for n in [1, 2, 3, 5] {
            // Reconnect so that anything sent on connect is split too.
            self.host.drop_session();
            self.proxy
                .set_faults(Faults::new().split(n).chunk_delay(Duration::from_millis(1)));
            let label = format!("{n} バイトずつ届く");
            self.expect_values("C5", &label, &all).await;
            self.expect_values("C5", &label, &reversed).await;
        }
        self.proxy.set_faults(Faults::new());
    }

    async fn c6_late_response(&mut self) {
        let Some(next) = (1..self.reads.len())
            .find(|&i| self.reads[i].case.expected != self.reads[0].case.expected)
        else {
            self.fail(
                "C6",
                "fixtures.reads に reads[0] と期待値の違う読み取りが無い".into(),
            );
            return;
        };
        if !self.expect_values("C6", "遅れの前の読み取り", &[0]).await {
            return;
        }
        let late = self.fixtures.response_timeout + self.fixtures.response_timeout / 2;
        let delivered = self.proxy.delivered_count();
        self.proxy.set_faults(Faults::new().hold_response(0, late));
        self.host.park_failed = true;
        let result = self.host.read(&[self.reads[0].spec.clone()]).await;
        self.host.park_failed = false;
        match result {
            Err(HostError::Fatal(_)) => {}
            other => self.fail(
                "C6",
                format!("応答時間を過ぎた読み取りが {other:?}（接続致命であるべき）"),
            ),
        }
        // Let the late response reach the old connection (kept open), then
        // drop it, as the host does.
        let proxy = self.proxy;
        wait_until(late * 2, || proxy.delivered_count() > delivered).await;
        self.proxy.set_faults(Faults::new());
        self.host.parked = None;
        self.host.drop_session();
        let before = self.host.connects;
        if self
            .expect_values("C6", "遅れた応答の次の読み取り", &[next])
            .await
            && self.host.connects != before + 1
        {
            self.fail(
                "C6",
                "時間切れの後、新しい接続を作らずに次の要求を送った".into(),
            );
        }
    }

    async fn c7_reconnect(&mut self) {
        let all = self.all();
        for round in 1..=2 {
            if !self.expect_values("C7", "切断の前の読み取り", &[0]).await {
                return;
            }
            let before = self.host.connects;
            self.proxy.close_all().await;
            match self.host.read(&[self.reads[0].spec.clone()]).await {
                Err(HostError::Fatal(_)) => {}
                other => self.fail(
                    "C7",
                    format!("相手が切断した後の読み取りが {other:?}（致命であるべき）"),
                ),
            }
            self.host.drop_session();
            let label = format!("{round} 回目の再接続の後");
            if self.expect_values("C7", &label, &all).await && self.host.connects != before + 1 {
                self.fail(
                    "C7",
                    format!(
                        "{label}: 接続を作り直していない（{before} → {}）",
                        self.host.connects
                    ),
                );
            }
        }
    }

    async fn c8_one_socket(&mut self) {
        let can_write = self.driver.descriptor().capabilities.write;
        let mut items = Vec::new();
        for w in &self.fixtures.writes {
            match self.driver.parse_address(&w.text, &w.kind) {
                Ok(a) => items.push(WriteItem::new(
                    TagSpec::new(a, w.kind.clone()),
                    w.value.clone(),
                )),
                Err(e) => {
                    self.fail(
                        "C8",
                        format!("fixtures.writes の {:?} が読めない: {e}", w.text),
                    );
                    return;
                }
            }
        }
        if can_write && items.is_empty() {
            self.fail(
                "C8",
                "書けるドライバなのに fixtures.writes が空（読みと書きを交互にできない）".into(),
            );
        }
        self.host.drop_session();
        // A relay from an earlier check may still be closing; wait until the
        // proxy is quiet so that the count starts from here.
        self.proxy.close_all().await;
        let accepts = self.proxy.accept_count();
        let connects = self.host.connects;
        for i in 0..100 {
            if i % 2 == 0 || items.is_empty() {
                let r = i % self.reads.len();
                if !self
                    .expect_values("C8", &format!("{i} 回目（読み）"), &[r])
                    .await
                {
                    return;
                }
            } else {
                let item = items[(i / 2) % items.len()].clone();
                match self.host.write(std::slice::from_ref(&item)).await {
                    Ok(v) if matches!(v.as_slice(), [Ok(())]) => {}
                    other => {
                        self.fail(
                            "C8",
                            format!(
                                "{i} 回目（{:?} への書き込み）が {other:?}",
                                item.spec.address.canonical
                            ),
                        );
                        return;
                    }
                }
            }
        }
        let proxy = self.proxy;
        // The proxy counts an accept slightly after the driver's connect
        // returns; one is certainly due.
        wait_until(self.host.bound, || proxy.accept_count() > accepts).await;
        let got = self.proxy.accept_count() - accepts;
        if got != 1 || self.host.connects != connects + 1 {
            self.fail(
                "C8",
                format!(
                    "読みと書きを交互に 100 回行う間にピアが受けた接続が {got} 本、connect が {} 回（どちらも 1 であるべき）",
                    self.host.connects - connects
                ),
            );
        }
        for item in &items {
            match self.host.read(std::slice::from_ref(&item.spec)).await {
                Ok(v) if matches!(v.as_slice(), [Ok(got)] if value_matches(&item.spec.kind, got, &item.value)) =>
                    {}
                other => self.fail(
                    "C8",
                    format!(
                        "{:?} に {:?} を書いた後の読み取りが {other:?}",
                        item.spec.address.canonical, item.value
                    ),
                ),
            }
        }
    }

    async fn c12_string_span(&mut self) {
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
            match self.host.read(&[TagSpec::new(address, kind)]).await {
                Ok(v) if matches!(v.as_slice(), [Err(ItemError::Unsupported(_))]) => {}
                other => self.fail(
                    "C12",
                    format!(
                        "{words} 語の文字列 {text:?} が {other:?}（ItemError::Unsupported であるべき）"
                    ),
                ),
            }
        }
    }

    /// Write `bits` (`(spec, value)`) in one batch.
    async fn write_bits(&mut self, bits: &[(&TagSpec, bool)]) -> Result<WriteResults, HostError> {
        let items: Vec<WriteItem> = bits
            .iter()
            .map(|(s, v)| WriteItem::new((*s).clone(), Value::Bit(*v)))
            .collect();
        self.host.write(&items).await
    }

    fn request_bytes(&self) -> usize {
        self.proxy.requests().iter().map(Vec::len).sum()
    }

    async fn c13_c15_bit_writes(&mut self, case: &BitWriteCase) {
        let bit_kind = TagKind::numeric(DataType::Bit);
        let (Some(word), Some(bit), Some(other)) = (
            self.spec("C13", &case.word, &TagKind::numeric(DataType::U16)),
            self.spec("C13", &case.bit, &bit_kind),
            self.spec("C13", &case.other_bit, &bit_kind),
        ) else {
            return;
        };
        let (Some(b), Some(o)) = (bit.address.bit, other.address.bit) else {
            self.fail(
                "C13",
                format!(
                    "{:?} / {:?} にワード内のビット位置が無い",
                    case.bit, case.other_bit
                ),
            );
            return;
        };
        let (mb, mo) = (1u16 << (b & 15), 1u16 << (o & 15));
        if mb == mo {
            self.fail(
                "C13",
                format!("{:?} と {:?} が同じビット", case.bit, case.other_bit),
            );
            return;
        }

        // C13: the other bits survive.
        for initial in [0x0000u16, 0x0100, 0xFFFF, 0x0008] {
            for value in [true, false] {
                if !self.write_word("C13", &word, initial).await {
                    return;
                }
                match self.write_bits(&[(&bit, value)]).await {
                    Ok(r) if matches!(r.as_slice(), [Ok(())]) => {}
                    other => {
                        self.fail(
                            "C13",
                            format!(
                                "0x{initial:04X} の {:?} に {value} を書くと {other:?}",
                                case.bit
                            ),
                        );
                        continue;
                    }
                }
                let want = if value { initial | mb } else { initial & !mb };
                if let Some(got) = self.read_word("C13", &word).await {
                    if got != want {
                        self.fail(
                            "C13",
                            format!(
                                "0x{initial:04X} の {:?} に {value} を書いた後のワードが 0x{got:04X}（期待は 0x{want:04X}）",
                                case.bit
                            ),
                        );
                    }
                }
            }
        }

        // C13: two bits of one word are one read-modify-write.
        if !self.write_word("C13", &word, 0).await {
            return;
        }
        let start = self.request_bytes();
        let one = self.write_bits(&[(&bit, true)]).await;
        let one_bytes = self.request_bytes() - start;
        if !self.write_word("C13", &word, 0).await {
            return;
        }
        let start = self.request_bytes();
        let two = self.write_bits(&[(&bit, true), (&other, true)]).await;
        let two_bytes = self.request_bytes() - start;
        match (&one, &two) {
            (Ok(a), Ok(b))
                if matches!(a.as_slice(), [Ok(())]) && matches!(b.as_slice(), [Ok(()), Ok(())]) =>
            {
                if two_bytes > one_bytes {
                    self.fail(
                        "C13",
                        format!(
                            "同じワードの 2 ビットの書き込みが {two_bytes} バイト（1 ビットは {one_bytes} バイト。1 回の RMW にまとまっていない）"
                        ),
                    );
                }
                if let Some(got) = self.read_word("C13", &word).await {
                    if got != mb | mo {
                        self.fail(
                            "C13",
                            format!(
                                "2 ビットを書いた後のワードが 0x{got:04X}（期待は 0x{:04X}）",
                                mb | mo
                            ),
                        );
                    }
                }
            }
            _ => self.fail(
                "C13",
                format!("ビットの書き込みが失敗した: 1 ビット {one:?}、2 ビット {two:?}"),
            ),
        }

        // C14: a check read that disagrees rejects only that bit.
        if let Some(patch) = case.verify_mismatch.clone() {
            if self.write_word("C14", &word, 0).await {
                let before = self.host.connects;
                self.proxy.set_faults(Faults::new().patch_response(
                    patch.index,
                    patch.offset,
                    patch.bytes,
                ));
                let result = self.write_bits(&[(&bit, true), (&other, true)]).await;
                self.proxy.set_faults(Faults::new());
                match result {
                    Ok(r)
                        if matches!(r.as_slice(), [Err(ItemError::Rejected(_)), Ok(())]) => {}
                    other => self.fail(
                        "C14",
                        format!(
                            "確認読みで {:?} だけが食い違うと {other:?}（[Rejected, Ok] であるべき）",
                            case.bit
                        ),
                    ),
                }
                self.expect_kept("C14", "確認読みの不一致", before).await;
                self.next_write_is_correct("C14", &word, &bit, mb).await;
            }
        }

        // C15: a refusal at any step stops the sequence there.
        for patch in &case.refusals {
            if !self.write_word("C15", &word, 0x0100).await {
                continue;
            }
            let before = self.host.connects;
            let sent = self.proxy.request_count();
            self.proxy.set_faults(Faults::new().patch_response(
                patch.index,
                patch.offset,
                patch.bytes.clone(),
            ));
            let result = self.write_bits(&[(&bit, true)]).await;
            self.proxy.set_faults(Faults::new());
            let steps = self.proxy.request_count() - sent;
            match result {
                Ok(r) if matches!(r.as_slice(), [Err(ItemError::Device(_))]) => {}
                other => self.fail(
                    "C15",
                    format!(
                        "{} 番目の応答で機器が断ると {other:?}（ItemError::Device であるべき）",
                        patch.index
                    ),
                ),
            }
            if steps != patch.index + 1 {
                self.fail(
                    "C15",
                    format!(
                        "{} 番目の応答で機器が断った後も要求を送った（送った要求は {steps} 件、{} 件であるべき）",
                        patch.index,
                        patch.index + 1
                    ),
                );
            }
            let label = format!("{} 番目の応答での拒否", patch.index);
            self.expect_kept("C15", &label, before).await;
            if self.write_word("C15", &word, 0x0100).await {
                self.next_write_is_correct("C15", &word, &bit, mb).await;
            }
        }
    }

    /// The next bit write after a non-fatal failure works from the start:
    /// setting `bit` on the word as it is now sets exactly that bit.
    async fn next_write_is_correct(
        &mut self,
        id: &'static str,
        word: &TagSpec,
        bit: &TagSpec,
        mb: u16,
    ) {
        let Some(now) = self.read_word(id, word).await else {
            return;
        };
        match self.write_bits(&[(bit, true)]).await {
            Ok(r) if matches!(r.as_slice(), [Ok(())]) => {}
            other => {
                self.fail(id, format!("次の書き込みが {other:?}"));
                return;
            }
        }
        if let Some(got) = self.read_word(id, word).await {
            if got != now | mb {
                self.fail(
                    id,
                    format!(
                        "次の書き込みの後のワードが 0x{got:04X}（期待は 0x{:04X}）",
                        now | mb
                    ),
                );
            }
        }
    }
}

/// Whether `got`, read as `kind`, is the fixture's `expected` (the table in
/// the module docs).
fn value_matches(kind: &TagKind, got: &Value, expected: &Value) -> bool {
    match (kind, got, expected) {
        (TagKind::Numeric(DataType::Bit), Value::Bit(g), Value::Bit(e)) => g == e,
        (TagKind::Numeric(DataType::F32), Value::F64(g), Value::F64(e)) => {
            (*g as f32).to_bits() == (*e as f32).to_bits()
        }
        (TagKind::Numeric(DataType::F64), Value::F64(g), Value::F64(e)) => {
            g.to_bits() == e.to_bits()
        }
        (TagKind::Numeric(t), Value::F64(g), Value::F64(e)) if *t != DataType::Bit => g == e,
        (TagKind::Str { .. }, Value::Str(g), Value::Str(e)) => g == e,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_matches_follows_the_kind() {
        use DataType::*;
        let n = TagKind::numeric;
        let s = TagKind::string(2, StringEncoding::ShiftJis);
        let f32_of = |v: f64| Value::F64(f64::from(v as f32));
        let table: Vec<(TagKind, Value, Value, bool)> = vec![
            (n(Bit), Value::Bit(true), Value::Bit(true), true),
            (n(Bit), Value::Bit(false), Value::Bit(true), false),
            (n(Bit), Value::F64(1.0), Value::Bit(true), false),
            (n(F32), f32_of(0.1), Value::F64(0.1), true),
            (n(F32), f32_of(0.1), Value::F64(0.2), false),
            (n(F64), f32_of(0.1), Value::F64(0.1), false),
            (n(F64), Value::F64(0.1), Value::F64(0.1), true),
            (n(U16), Value::F64(7.0), Value::F64(7.0), true),
            (n(U16), Value::F64(7.0), Value::F64(8.0), false),
            (n(I16), Value::F64(-0.0), Value::F64(0.0), true),
            (n(U16), Value::Bit(true), Value::F64(1.0), false),
            (
                s.clone(),
                Value::Str("あ".into()),
                Value::Str("あ".into()),
                true,
            ),
            (
                s.clone(),
                Value::Str("あ".into()),
                Value::Str("い".into()),
                false,
            ),
            (s, Value::F64(1.0), Value::Str("1".into()), false),
        ];
        for (kind, got, expected, want) in table {
            assert_eq!(
                value_matches(&kind, &got, &expected),
                want,
                "{kind:?}: {got:?} vs {expected:?}"
            );
        }
    }

    #[test]
    fn fixtures_builders_fill_the_fields() {
        let n = TagKind::numeric;
        let f = SessionFixtures::new(StaticFixtures::default())
            .read("A", n(DataType::U16), Value::F64(1.0))
            .refused("B", n(DataType::U16))
            .unsupported("C", n(DataType::Bit))
            .write("D", n(DataType::U16), Value::F64(2.0))
            .unsupported_write("E.1", n(DataType::Bit), Value::Bit(true))
            .broken(ResponsePatch::new(0, 0, vec![0xFF]))
            .bit_write(
                BitWriteCase::new("W", "W.3", "W.5")
                    .verify_mismatch(ResponsePatch::new(2, 9, vec![0x20, 0]))
                    .refusal(ResponsePatch::new(0, 7, vec![1])),
            )
            .max_string_words(480)
            .response_timeout(Duration::from_millis(500));
        assert_eq!(f.reads.len(), 1);
        assert_eq!(f.refused.len(), 1);
        assert_eq!(f.unsupported.len(), 1);
        assert_eq!(f.writes.len(), 1);
        assert_eq!(f.unsupported_writes.len(), 1);
        assert!(f.broken.is_some());
        assert_eq!(f.bit_writes[0].refusals.len(), 1);
        assert!(f.bit_writes[0].verify_mismatch.is_some());
        assert_eq!(f.max_string_words, 480);
        assert_eq!(f.response_timeout, Duration::from_millis(500));
    }
}
