//! [`EchoFrameDriver`]: the reference frame-type driver, and
//! [`EchoResponder`], the device it talks to
//! (docs/banto-driver-api-design.md §4.3, §7.4).
//!
//! Small enough to read in one sitting, but it does what real drivers do,
//! so that the harness has something real to check and a third party has
//! something to copy:
//!
//! - a **handshake** that hands out a session id, which every later request
//!   must carry (so a reconnect that reuses an old id fails - C7);
//! - **transaction numbers** shared by reads and writes (`&mut self` in
//!   `plan_read` / `write_next`), checked when decoding;
//! - reads of one word range planned **once for several specs**, each
//!   decoded by its own kind (`U32` and `F32` at `W1` are one frame, two
//!   values - C3);
//! - a **word-bit read-modify-write** through the `write_next` state machine
//!   (read the word, write it back with only that bit changed, read it again
//!   to verify), like SLMP's (design §4.2.1);
//! - three ways of framing the same responses, chosen by the `framing`
//!   setting ([`FRAMING_FIXED`] - the default, [`FRAMING_PREFIXED`],
//!   [`FRAMING_DECIDES`]), so the harness exercises `Fixed`,
//!   `LengthPrefixed` and `DriverDecides` (with `Discard` of leading noise).
//!
//! ## Addresses
//!
//! `W<n>` (holding words, writable) and `R<n>` (input words, read-only),
//! `n` = 0-65535 in decimal; a bit inside a word is `.0`-`.15` in decimal
//! (`W10.3`), for `DataType::Bit` only. Canonical: upper-case area, no
//! leading zeros (`w010` → `W10`, `W10.03` → `W10.3`). The responder has
//! words 0-999 of each area; beyond that it refuses (a device error, C4).
//!
//! ## Wire format
//!
//! Every frame, both ways: `[0xEC, code, session, txn, len_hi, len_lo,
//! payload(len)]`.
//!
//! | Request (`code`) | Payload | Response | Payload |
//! | --- | --- | --- | --- |
//! | `H` hello | - | `h` | `[session]` |
//! | `R` read | `[area, start_hi, start_lo, count]` | `r` | `count` words, big-endian |
//! | `W` write | `[area, start_hi, start_lo, count, words..]` | `w` | `[0]` |
//!
//! A refusal is `E` with `[error code, 0, ..]` padded to the length of the
//! successful response, so every response to one request has the same
//! length (that is what makes `Fixed` framing possible). Error codes: 1 bad
//! request, 2 address out of range, 3 wrong session, 4 read-only area.

use std::fmt;

use banto_driver_api::{
    keys, AddressError, Capabilities, DataType, DeviceError, DriverAddress, DriverBase,
    DriverDescriptor, DriverId, FatalError, FieldError, FrameCheck, FrameConnection, FrameDriver,
    FrameRule, ItemError, Planned, PrefixSpec, RawValue, Settings, Step, StringEncoding, TagKind,
    TagSpec, Value, WordOrder, WriteItem, WriteProgress,
};

use crate::frame::{FrameFixtures, Reply, Responder};
use crate::StaticFixtures;

/// The `framing` setting: fixed-length responses ([`FrameRule::Fixed`]).
/// The default.
pub const FRAMING_FIXED: &str = "fixed";
/// The `framing` setting: [`FrameRule::LengthPrefixed`] on the header's
/// length field.
pub const FRAMING_PREFIXED: &str = "prefixed";
/// The `framing` setting: [`FrameRule::DriverDecides`] (skips bytes before
/// the magic byte with [`FrameCheck::Discard`]).
pub const FRAMING_DECIDES: &str = "decides";
/// The setting key choosing one of the `FRAMING_*` values.
pub const KEY_FRAMING: &str = "framing";

/// Words per area the responder holds (`0..MEMORY_WORDS`).
pub const MEMORY_WORDS: usize = 1000;

const MAGIC: u8 = 0xEC;
const HEADER: usize = 6;
const MAX_COUNT: usize = 255;
const MAX_LEN: u32 = (HEADER + 2 * MAX_COUNT) as u32;
/// The registry's string cap (`banto-tags` `string_length`).
const MAX_STRING_WORDS: u16 = 128;

const ERR_BAD_REQUEST: u8 = 1;
const ERR_ADDRESS: u8 = 2;
const ERR_SESSION: u8 = 3;
const ERR_READ_ONLY: u8 = 4;

/// The reference frame-type driver (see the module doc).
#[derive(Debug, Clone)]
pub struct EchoFrameDriver {
    descriptor: DriverDescriptor,
}

impl EchoFrameDriver {
    /// The driver, with id `echo`.
    pub fn new() -> Self {
        EchoFrameDriver {
            descriptor: DriverDescriptor::new(
                DriverId::new("echo").expect("valid id"),
                env!("CARGO_PKG_VERSION"),
                "Echo（適合テストの参照実装）",
                Capabilities::READ_WRITE,
            )
            .with_vendor("banto-driver-conformance"),
        }
    }
}

impl Default for EchoFrameDriver {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Framing {
    Fixed,
    Prefixed,
    Decides,
}

fn framing_of(settings: &Settings) -> Result<Framing, String> {
    match settings.get(KEY_FRAMING).unwrap_or(FRAMING_FIXED) {
        FRAMING_FIXED => Ok(Framing::Fixed),
        FRAMING_PREFIXED => Ok(Framing::Prefixed),
        FRAMING_DECIDES => Ok(Framing::Decides),
        other => Err(format!(
            "{other:?} は使えません（{FRAMING_FIXED} / {FRAMING_PREFIXED} / {FRAMING_DECIDES}）"
        )),
    }
}

fn word_order_of(settings: &Settings) -> Result<WordOrder, String> {
    match settings.get(keys::WORD_ORDER).unwrap_or("high_low") {
        "high_low" => Ok(WordOrder::HighLow),
        "low_high" => Ok(WordOrder::LowHigh),
        other => Err(format!("{other:?} は使えません（high_low / low_high）")),
    }
}

/// An address in the echo grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EchoAddr {
    area: u8,
    word: u16,
    bit: Option<u8>,
}

impl EchoAddr {
    fn parse(text: &str) -> Result<Self, AddressError> {
        let t = text.trim();
        let mut chars = t.chars();
        let area = match chars.next() {
            Some('W' | 'w') => b'W',
            Some('R' | 'r') => b'R',
            _ => return Err(AddressError::new(format!("{t:?}: W か R で始めます"))),
        };
        let rest = chars.as_str();
        let (num, bit) = match rest.split_once('.') {
            Some((n, b)) => (n, Some(b)),
            None => (rest, None),
        };
        let word = decimal(num, 65535)
            .ok_or_else(|| AddressError::new(format!("{t:?}: 番号は 0〜65535 の 10 進数です")))?;
        let bit = match bit {
            None => None,
            Some(b) if b.len() <= 2 => Some(decimal(b, 15).ok_or_else(|| {
                AddressError::new(format!("{t:?}: ビットは .0〜.15 の 10 進数です"))
            })? as u8),
            Some(_) => {
                return Err(AddressError::new(format!(
                    "{t:?}: ビットは .0〜.15 の 10 進数です"
                )))
            }
        };
        Ok(EchoAddr {
            area,
            word: word as u16,
            bit,
        })
    }
}

impl fmt::Display for EchoAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.area as char, self.word)?;
        if let Some(b) = self.bit {
            write!(f, ".{b}")?;
        }
        Ok(())
    }
}

/// `s` as a decimal number `0..=max` (digits only, non-empty).
fn decimal(s: &str, max: u32) -> Option<u32> {
    if s.is_empty() || s.len() > 5 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<u32>().ok().filter(|&v| v <= max)
}

impl DriverBase for EchoFrameDriver {
    fn descriptor(&self) -> &DriverDescriptor {
        &self.descriptor
    }

    fn validate_config(&self, settings: &Settings) -> Vec<FieldError> {
        let mut errors = Vec::new();
        match settings.get(keys::HOST) {
            Some(h) if !h.trim().is_empty() => {}
            _ => errors.push(FieldError::new(keys::HOST, "接続先を入れてください")),
        }
        match settings.get(keys::PORT).map(str::parse::<u16>) {
            Some(Ok(p)) if p != 0 => {}
            _ => errors.push(FieldError::new(keys::PORT, "1〜65535 の数を入れてください")),
        }
        if let Err(m) = framing_of(settings) {
            errors.push(FieldError::new(KEY_FRAMING, m));
        }
        if let Err(m) = word_order_of(settings) {
            errors.push(FieldError::new(keys::WORD_ORDER, m));
        }
        match settings.get(keys::TRANSPORT) {
            None | Some("tcp") => {}
            Some(other) => errors.push(FieldError::new(
                keys::TRANSPORT,
                format!("{other:?} は使えません（tcp だけ）"),
            )),
        }
        errors
    }

    fn parse_address(&self, text: &str, kind: &TagKind) -> Result<DriverAddress, AddressError> {
        let a = EchoAddr::parse(text)?;
        let words = kind.word_count();
        if u32::from(a.word) + u32::from(words.max(1)) - 1 > 65535 {
            return Err(AddressError::new(format!(
                "{a} から {words} 語は 65535 を超えます"
            )));
        }
        let parsed = DriverAddress::new(
            self.descriptor.id.clone(),
            a.to_string(),
            (a.area as char).to_string(),
            a.area == b'W',
            words,
        );
        Ok(match a.bit {
            Some(b) => parsed.with_bit(b),
            None => parsed,
        })
    }

    fn offset_address(
        &self,
        canonical: &str,
        kind: &TagKind,
        n: i64,
    ) -> Result<String, AddressError> {
        let a = EchoAddr::parse(canonical)?;
        let out_of_range =
            || AddressError::new(format!("{canonical} から {n} ずらすと範囲の外です"));
        let moved = match a.bit {
            // A bit address moves along one 16-bit-per-word number line
            // (the TS `incrementAddress` rule).
            Some(bit) => {
                let pos = (i64::from(a.word) * 16 + i64::from(bit))
                    .checked_add(n)
                    .filter(|p| (0..=65535 * 16 + 15).contains(p))
                    .ok_or_else(out_of_range)?;
                EchoAddr {
                    word: (pos / 16) as u16,
                    bit: Some((pos % 16) as u8),
                    ..a
                }
            }
            None => {
                let words = i64::from(kind.word_count().max(1));
                let word = i64::from(a.word)
                    .checked_add(n)
                    .filter(|w| *w >= 0 && w + words - 1 <= 65535)
                    .ok_or_else(out_of_range)?;
                EchoAddr {
                    word: word as u16,
                    ..a
                }
            }
        };
        Ok(moved.to_string())
    }

    fn address_help(&self) -> String {
        "W<番号>（書ける語）・R<番号>（読み取り専用の語）。番号は 0〜65535 の 10 進数。\
         語の中のビットは .0〜.15（10 進、bit 型だけ）。例: W100、R5、W10.3"
            .to_owned()
    }
}

impl FrameDriver for EchoFrameDriver {
    fn create_connection(
        &self,
        settings: &Settings,
    ) -> Result<Box<dyn FrameConnection>, FatalError> {
        let framing = framing_of(settings).map_err(FatalError::InvalidConfig)?;
        let word_order = word_order_of(settings).map_err(FatalError::InvalidConfig)?;
        Ok(Box::new(EchoConnection {
            framing,
            word_order,
            session: None,
            hello_txn: None,
            txn: 0,
            write: None,
        }))
    }
}

/// One connection's protocol state.
struct EchoConnection {
    framing: Framing,
    word_order: WordOrder,
    /// From the handshake; `None` until it completes.
    session: Option<u8>,
    /// The hello in flight.
    hello_txn: Option<u8>,
    /// Shared by reads and writes.
    txn: u8,
    /// The write in progress (reset by every `write_next(None)`).
    write: Option<WriteRun>,
}

/// A parsed response frame.
struct Frame<'a> {
    code: u8,
    session: u8,
    txn: u8,
    payload: &'a [u8],
}

fn parse_frame(buf: &[u8]) -> Result<Frame<'_>, FatalError> {
    if buf.len() < HEADER {
        return Err(FatalError::Framing(format!(
            "{} バイトはヘッダより短い",
            buf.len()
        )));
    }
    if buf[0] != MAGIC {
        return Err(FatalError::Framing(format!(
            "先頭が 0x{:02X}（0xEC であるべき）",
            buf[0]
        )));
    }
    let len = usize::from(u16::from_be_bytes([buf[4], buf[5]]));
    if HEADER + len != buf.len() {
        return Err(FatalError::Framing(format!(
            "長さ欄 {len} と実際の {} バイトが合わない",
            buf.len() - HEADER
        )));
    }
    Ok(Frame {
        code: buf[1],
        session: buf[2],
        txn: buf[3],
        payload: &buf[HEADER..],
    })
}

/// The frame for one request.
fn frame(code: u8, session: u8, txn: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER + payload.len());
    out.extend_from_slice(&[MAGIC, code, session, txn]);
    out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

impl EchoConnection {
    fn next_txn(&mut self) -> u8 {
        self.txn = self.txn.wrapping_add(1);
        self.txn
    }

    /// A step expecting a successful payload of `payload_len` bytes.
    fn step(&self, send: Vec<u8>, payload_len: usize) -> Step {
        let rule = match self.framing {
            Framing::Fixed => FrameRule::Fixed((HEADER + payload_len) as u32),
            Framing::Prefixed => {
                FrameRule::LengthPrefixed(PrefixSpec::new(4, 2, true, HEADER as i32))
            }
            Framing::Decides => FrameRule::DriverDecides,
        };
        Step::new(send, rule, MAX_LEN)
    }

    fn session(&self) -> Result<u8, FatalError> {
        self.session
            .ok_or_else(|| FatalError::Protocol("ハンドシェイクが済んでいない".into()))
    }

    fn read_request(&mut self, area: u8, start: u16, count: u8) -> Result<Step, FatalError> {
        let session = self.session()?;
        let txn = self.next_txn();
        let [hi, lo] = start.to_be_bytes();
        let send = frame(b'R', session, txn, &[area, hi, lo, count]);
        Ok(self.step(send, 2 * usize::from(count)))
    }

    fn write_request(&mut self, area: u8, start: u16, words: &[u16]) -> Result<Step, FatalError> {
        let session = self.session()?;
        let txn = self.next_txn();
        let [hi, lo] = start.to_be_bytes();
        let mut payload = vec![area, hi, lo, words.len() as u8];
        for w in words {
            payload.extend_from_slice(&w.to_be_bytes());
        }
        let send = frame(b'W', session, txn, &payload);
        Ok(self.step(send, 1))
    }

    /// Check a response against the request it answers. `Ok(Ok(payload))`
    /// for `ok_code`, `Ok(Err(code))` for a device refusal.
    fn answer<'a>(
        &self,
        response: &'a [u8],
        sent: &[u8],
        ok_code: u8,
    ) -> Result<Result<&'a [u8], u8>, FatalError> {
        let f = parse_frame(response)?;
        if sent.len() < HEADER || f.txn != sent[3] {
            return Err(FatalError::Protocol(format!(
                "トランザクション番号 {} が要求と合わない",
                f.txn
            )));
        }
        if Some(f.session) != self.session {
            return Err(FatalError::Protocol(format!(
                "セッション {} が違う",
                f.session
            )));
        }
        match f.code {
            c if c == ok_code => Ok(Ok(f.payload)),
            b'E' => match f.payload.first() {
                Some(&code) => Ok(Err(code)),
                None => Err(FatalError::Framing("エラー応答に終了コードが無い".into())),
            },
            other => Err(FatalError::Protocol(format!(
                "未知の応答コード 0x{other:02X}"
            ))),
        }
    }

    fn words_of(payload: &[u8], count: usize) -> Result<Vec<u16>, FatalError> {
        if payload.len() != 2 * count {
            return Err(FatalError::Framing(format!(
                "{count} 語の応答が {} バイト",
                payload.len()
            )));
        }
        Ok(payload
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect())
    }

    /// `words` (in wire order) as one integer, honouring the word order.
    fn combine(&self, words: &[u16]) -> u64 {
        let fold = |acc: u64, w: &u16| (acc << 16) | u64::from(*w);
        match self.word_order {
            WordOrder::HighLow => words.iter().fold(0, fold),
            WordOrder::LowHigh => words.iter().rev().fold(0, fold),
        }
    }

    /// The inverse of [`combine`](Self::combine): `n` words in wire order.
    fn split(&self, value: u64, n: usize) -> Vec<u16> {
        let mut words: Vec<u16> = (0..n).rev().map(|i| (value >> (16 * i)) as u16).collect();
        if self.word_order == WordOrder::LowHigh {
            words.reverse();
        }
        words
    }

    fn decode(
        &self,
        kind: &TagKind,
        bit: Option<u8>,
        words: &[u16],
    ) -> Result<RawValue, ItemError> {
        let v = self.combine(words);
        Ok(match kind {
            TagKind::Numeric(DataType::Bit) => {
                let bit =
                    bit.ok_or_else(|| ItemError::Unsupported("bit 型には .n が要る".into()))?;
                RawValue::Bool(words[0] >> bit & 1 == 1)
            }
            TagKind::Numeric(DataType::I16) => RawValue::Int(i64::from(v as u16 as i16)),
            TagKind::Numeric(DataType::U16) => RawValue::Uint(v),
            TagKind::Numeric(DataType::I32) => RawValue::Int(i64::from(v as u32 as i32)),
            TagKind::Numeric(DataType::U32) => RawValue::Uint(v),
            TagKind::Numeric(DataType::F32) => RawValue::Float(f64::from(f32::from_bits(v as u32))),
            TagKind::Numeric(DataType::I64) => RawValue::Int(v as i64),
            TagKind::Numeric(DataType::U64) => RawValue::Uint(v),
            TagKind::Numeric(DataType::F64) => RawValue::Float(f64::from_bits(v)),
            // Strings: each word's low byte first (banto-plc's rule); the
            // host cuts at the first NUL and decodes.
            TagKind::Str { .. } => RawValue::Bytes(
                words
                    .iter()
                    .flat_map(|w| [*w as u8, (w >> 8) as u8])
                    .collect(),
            ),
            other => return Err(ItemError::Unsupported(format!("{other:?} は読めない"))),
        })
    }

    /// The word range a spec reads, or why it cannot be read.
    fn read_range(spec: &TagSpec) -> Result<(EchoAddr, u8), ItemError> {
        let a = EchoAddr::parse(&spec.address.canonical)
            .map_err(|e| ItemError::Unsupported(e.to_string()))?;
        match (&spec.kind, a.bit) {
            (TagKind::Numeric(DataType::Bit), Some(_)) => {}
            (TagKind::Numeric(DataType::Bit), None) => {
                return Err(ItemError::Unsupported("bit 型には .n が要る".into()))
            }
            (TagKind::Numeric(_), Some(_)) => {
                return Err(ItemError::Unsupported("語の中のビットは bit 型だけ".into()))
            }
            (TagKind::Numeric(_), None) => {}
            (TagKind::Str { words, .. }, None) if (1..=MAX_STRING_WORDS).contains(words) => {}
            (TagKind::Str { words, .. }, _) => {
                return Err(ItemError::Unsupported(format!(
                    "文字列は 1〜{MAX_STRING_WORDS} 語（{words} 語）"
                )))
            }
            (other, _) => return Err(ItemError::Unsupported(format!("{other:?} は読めない"))),
        }
        let count = spec.kind.word_count();
        if u32::from(a.word) + u32::from(count) - 1 > 65535 {
            return Err(ItemError::Unsupported("65535 を超える".into()));
        }
        Ok((a, count as u8))
    }

    /// Plan one logical write (see the module doc).
    fn plan_write(&self, items: &[WriteItem]) -> WriteRun {
        let mut results: Vec<Option<Result<(), ItemError>>> = vec![None; items.len()];
        let mut groups: Vec<Group> = Vec::new();
        for (i, item) in items.iter().enumerate() {
            let a = match EchoAddr::parse(&item.spec.address.canonical) {
                Ok(a) => a,
                Err(e) => {
                    results[i] = Some(Err(ItemError::Unsupported(e.to_string())));
                    continue;
                }
            };
            if a.area != b'W' {
                results[i] = Some(Err(ItemError::Unsupported(format!("{a} は読み取り専用"))));
                continue;
            }
            match (&item.spec.kind, a.bit, &item.value) {
                (TagKind::Numeric(DataType::Bit), Some(bit), Value::Bit(on)) => {
                    let pos = groups
                        .iter()
                        .position(|g| matches!(g, Group::Rmw { word, .. } if *word == a.word));
                    match pos {
                        Some(p) => {
                            if let Group::Rmw { bits, .. } = &mut groups[p] {
                                bits.push((i, bit, *on));
                            }
                        }
                        None => groups.push(Group::Rmw {
                            word: a.word,
                            bits: vec![(i, bit, *on)],
                            phase: Phase::Read,
                        }),
                    }
                }
                (TagKind::Numeric(DataType::Bit), _, _) => {
                    results[i] = Some(Err(ItemError::Unsupported(
                        "bit 型は .n と true / false で書く".into(),
                    )));
                }
                (kind, None, value) => match self.encode(kind, value) {
                    Ok(words) => groups.push(Group::Direct {
                        item: i,
                        start: a.word,
                        words,
                        sent: false,
                    }),
                    Err(e) => results[i] = Some(Err(e)),
                },
                (_, Some(_), _) => {
                    results[i] = Some(Err(ItemError::Unsupported(
                        "語の中のビットは bit 型だけ".into(),
                    )));
                }
            }
        }
        // The same bit set and cleared in one write: refuse both (like
        // banto-plc-write's ConflictingBitWrite).
        for g in &mut groups {
            if let Group::Rmw { bits, .. } = g {
                let conflicting: Vec<usize> = bits
                    .iter()
                    .filter(|(_, b, on)| bits.iter().any(|(_, b2, on2)| b2 == b && on2 != on))
                    .map(|(i, ..)| *i)
                    .collect();
                for &i in &conflicting {
                    results[i] = Some(Err(ItemError::Rejected(
                        "同じビットに true と false".into(),
                    )));
                }
                bits.retain(|(i, ..)| !conflicting.contains(i));
            }
        }
        groups.retain(|g| !matches!(g, Group::Rmw { bits, .. } if bits.is_empty()));
        WriteRun {
            groups,
            cursor: 0,
            results,
            awaiting: None,
        }
    }

    fn encode(&self, kind: &TagKind, value: &Value) -> Result<Vec<u16>, ItemError> {
        let mismatch = || ItemError::Unsupported(format!("{kind:?} に {value:?} は書けない"));
        // `lo..end` (end exclusive, so 2^63 / 2^64 are exact bounds).
        let int = |v: f64, lo: f64, end: f64| -> Result<f64, ItemError> {
            if v.is_finite() && v.fract() == 0.0 && v >= lo && v < end {
                Ok(v)
            } else {
                Err(ItemError::Unsupported(format!(
                    "{v} は {kind:?} の範囲の外か整数でない"
                )))
            }
        };
        match (kind, value) {
            (TagKind::Numeric(t), Value::F64(v)) => {
                let v = *v;
                let bits: u64 = match t {
                    DataType::I16 => int(v, -32768.0, 32768.0)? as i16 as u16 as u64,
                    DataType::U16 => int(v, 0.0, 65536.0)? as u64,
                    DataType::I32 => int(v, -2147483648.0, 2147483648.0)? as i32 as u32 as u64,
                    DataType::U32 => int(v, 0.0, 4294967296.0)? as u64,
                    DataType::F32 => u64::from((v as f32).to_bits()),
                    DataType::I64 => int(v, -(2f64.powi(63)), 2f64.powi(63))? as i64 as u64,
                    DataType::U64 => int(v, 0.0, 2f64.powi(64))? as u64,
                    DataType::F64 => v.to_bits(),
                    DataType::Bit => return Err(mismatch()),
                };
                Ok(self.split(bits, usize::from(t.register_span())))
            }
            (
                TagKind::Str {
                    words,
                    encoding: StringEncoding::Utf8,
                },
                Value::Str(s),
            ) => {
                let bytes = s.as_bytes();
                let words = usize::from(*words);
                if words == 0 || words > usize::from(MAX_STRING_WORDS) || bytes.len() > 2 * words {
                    return Err(ItemError::Unsupported(format!(
                        "{words} 語に {} バイトは入らない",
                        bytes.len()
                    )));
                }
                Ok((0..words)
                    .map(|i| {
                        let lo = bytes.get(2 * i).copied().unwrap_or(0);
                        let hi = bytes.get(2 * i + 1).copied().unwrap_or(0);
                        u16::from_le_bytes([lo, hi])
                    })
                    .collect())
            }
            (TagKind::Str { .. }, Value::Str(_)) => Err(ItemError::Unsupported(
                "echo は UTF-8 の文字列だけ書ける".into(),
            )),
            _ => Err(mismatch()),
        }
    }

    /// The next step of the current group, or `None` when it is finished.
    fn next_send(&mut self, run: &mut WriteRun) -> Result<Option<Planned>, FatalError> {
        let (step, covers) = match &mut run.groups[run.cursor] {
            Group::Direct {
                item,
                start,
                words,
                sent,
            } => {
                if *sent {
                    return Ok(None);
                }
                *sent = true;
                let (item, start, words) = (*item, *start, words.clone());
                (self.write_request(b'W', start, &words)?, vec![item as u32])
            }
            Group::Rmw { word, bits, phase } => {
                let covers: Vec<u32> = bits.iter().map(|(i, ..)| *i as u32).collect();
                let word = *word;
                match *phase {
                    Phase::Read | Phase::Verify => (self.read_request(b'W', word, 1)?, covers),
                    Phase::Write(new) => (self.write_request(b'W', word, &[new])?, covers),
                    Phase::Done => return Ok(None),
                }
            }
        };
        Ok(Some(Planned::new(step, covers)))
    }

    /// Apply the response to the step the current group sent.
    fn on_write_response(
        &self,
        run: &mut WriteRun,
        sent: &[u8],
        response: &[u8],
    ) -> Result<(), FatalError> {
        let WriteRun {
            groups,
            cursor,
            results,
            ..
        } = run;
        match &mut groups[*cursor] {
            Group::Direct { item, .. } => {
                results[*item] = Some(match self.answer(response, sent, b'w')? {
                    Ok(_) => Ok(()),
                    Err(code) => Err(device(code)),
                });
            }
            Group::Rmw { bits, phase, .. } => {
                let ok_code = if matches!(phase, Phase::Write(_)) {
                    b'w'
                } else {
                    b'r'
                };
                match self.answer(response, sent, ok_code)? {
                    Err(code) => {
                        // Refused at any stage: the whole word fails, and
                        // nothing more is sent for it.
                        for (i, ..) in bits.iter() {
                            results[*i] = Some(Err(device(code)));
                        }
                        *phase = Phase::Done;
                    }
                    Ok(payload) => match *phase {
                        Phase::Read => {
                            let current = Self::words_of(payload, 1)?[0];
                            let set: u16 = bits.iter().filter(|b| b.2).fold(0, |m, b| m | 1 << b.1);
                            let clear: u16 =
                                bits.iter().filter(|b| !b.2).fold(0, |m, b| m | 1 << b.1);
                            *phase = Phase::Write((current & !(set | clear)) | set);
                        }
                        Phase::Write(_) => *phase = Phase::Verify,
                        Phase::Verify => {
                            let got = Self::words_of(payload, 1)?[0];
                            // Bit by bit: a correctly written bit is not
                            // failed because another one was not.
                            for (i, bit, on) in bits.iter() {
                                results[*i] = Some(if (got >> bit & 1 == 1) == *on {
                                    Ok(())
                                } else {
                                    Err(ItemError::Rejected(format!(
                                        "確認読みでビット {bit} が違う"
                                    )))
                                });
                            }
                            *phase = Phase::Done;
                        }
                        Phase::Done => {
                            return Err(FatalError::Protocol("終わったまとまりへの応答".into()))
                        }
                    },
                }
            }
        }
        Ok(())
    }
}

fn device(code: u8) -> ItemError {
    let message = match code {
        ERR_BAD_REQUEST => "要求が不正",
        ERR_ADDRESS => "アドレスが範囲の外",
        ERR_SESSION => "セッションが違う",
        ERR_READ_ONLY => "読み取り専用",
        _ => "不明",
    };
    ItemError::Device(DeviceError::new(u32::from(code), message))
}

/// One logical write in progress.
struct WriteRun {
    groups: Vec<Group>,
    cursor: usize,
    results: Vec<Option<Result<(), ItemError>>>,
    /// The request whose response is awaited.
    awaiting: Option<Vec<u8>>,
}

/// A unit of a write: one item written directly, or the bits of one word
/// written by read-modify-write.
enum Group {
    Direct {
        item: usize,
        start: u16,
        words: Vec<u16>,
        sent: bool,
    },
    Rmw {
        word: u16,
        /// `(item, bit, value)`.
        bits: Vec<(usize, u8, bool)>,
        phase: Phase,
    },
}

#[derive(Clone, Copy)]
enum Phase {
    Read,
    Write(u16),
    Verify,
    Done,
}

impl FrameConnection for EchoConnection {
    fn handshake_next(&mut self, prev_response: Option<&[u8]>) -> Result<Option<Step>, FatalError> {
        match prev_response {
            None => {
                self.session = None;
                self.write = None;
                let txn = self.next_txn();
                self.hello_txn = Some(txn);
                Ok(Some(self.step(frame(b'H', 0, txn, &[]), 1)))
            }
            Some(response) => {
                let txn = self.hello_txn.take().ok_or_else(|| {
                    FatalError::Protocol("ハンドシェイクの応答を待っていない".into())
                })?;
                let f = parse_frame(response)?;
                if f.txn != txn {
                    return Err(FatalError::Protocol(
                        "ハンドシェイクの応答の番号が違う".into(),
                    ));
                }
                match (f.code, f.payload) {
                    (b'h', [session]) if *session != 0 && f.session == *session => {
                        self.session = Some(*session);
                        Ok(None)
                    }
                    (b'E', _) => Err(FatalError::Protocol("ハンドシェイクを断られた".into())),
                    _ => Err(FatalError::Protocol("ハンドシェイクの応答が不正".into())),
                }
            }
        }
    }

    fn is_complete(&self, step: &Step, buf: &[u8]) -> Result<FrameCheck, FatalError> {
        if buf[0] != MAGIC {
            let n = buf.iter().take_while(|&&b| b != MAGIC).count();
            return Ok(FrameCheck::Discard(n as u32));
        }
        if buf.len() < HEADER {
            return Ok(FrameCheck::Incomplete);
        }
        let total = HEADER + usize::from(u16::from_be_bytes([buf[4], buf[5]]));
        if total > step.max_len as usize {
            return Err(FatalError::Framing(format!(
                "長さ欄が {total} バイトを示す"
            )));
        }
        Ok(if buf.len() >= total {
            FrameCheck::Complete { len: total as u32 }
        } else {
            FrameCheck::Incomplete
        })
    }

    fn plan_read(&mut self, specs: &[TagSpec]) -> Result<Vec<Planned>, FatalError> {
        // One request per distinct word range; every spec on it is covered
        // by that one step (and decoded by its own kind).
        let mut ranges: Vec<((u8, u16, u8), Vec<u32>)> = Vec::new();
        for (i, spec) in specs.iter().enumerate() {
            // An unreadable spec is left out of the plan: the engine reports
            // it as ItemError::Unsupported.
            let Ok((a, count)) = Self::read_range(spec) else {
                continue;
            };
            let key = (a.area, a.word, count);
            match ranges.iter_mut().find(|(k, _)| *k == key) {
                Some((_, covers)) => covers.push(i as u32),
                None => ranges.push((key, vec![i as u32])),
            }
        }
        ranges
            .into_iter()
            .map(|((area, start, count), covers)| {
                Ok(Planned::new(self.read_request(area, start, count)?, covers))
            })
            .collect()
    }

    fn decode_read(
        &self,
        planned: &Planned,
        specs: &[TagSpec],
        response: &[u8],
    ) -> Result<Vec<Result<RawValue, ItemError>>, FatalError> {
        let sent = &planned.step.send;
        if sent.len() != HEADER + 4 {
            return Err(FatalError::Protocol("echo の読み取り要求ではない".into()));
        }
        let count = usize::from(sent[HEADER + 3]);
        match self.answer(response, sent, b'r')? {
            Err(code) => Ok(planned.covers.iter().map(|_| Err(device(code))).collect()),
            Ok(payload) => {
                let words = Self::words_of(payload, count)?;
                planned
                    .covers
                    .iter()
                    .map(|&c| {
                        let spec = specs.get(c as usize).ok_or_else(|| {
                            FatalError::Protocol(format!("covers の {c} が specs の外"))
                        })?;
                        let bit = EchoAddr::parse(&spec.address.canonical)
                            .ok()
                            .and_then(|a| a.bit);
                        let n = usize::from(spec.kind.word_count());
                        if n > words.len() {
                            return Ok(Err(ItemError::Unsupported(
                                "計画と spec の語数が違う".into(),
                            )));
                        }
                        Ok(self.decode(&spec.kind, bit, &words[..n]))
                    })
                    .collect()
            }
        }
    }

    fn write_next(
        &mut self,
        items: &[WriteItem],
        prev_response: Option<&[u8]>,
    ) -> Result<WriteProgress, FatalError> {
        let mut run = match prev_response {
            // A new write: whatever an interrupted one left is dropped.
            None => self.plan_write(items),
            Some(response) => {
                let mut run = self
                    .write
                    .take()
                    .ok_or_else(|| FatalError::Protocol("書き込みの途中ではない".into()))?;
                let sent = run
                    .awaiting
                    .take()
                    .ok_or_else(|| FatalError::Protocol("応答を待っていない".into()))?;
                self.on_write_response(&mut run, &sent, response)?;
                run
            }
        };
        while run.cursor < run.groups.len() {
            if let Some(planned) = self.next_send(&mut run)? {
                run.awaiting = Some(planned.step.send.clone());
                self.write = Some(run);
                return Ok(WriteProgress::Send(planned));
            }
            run.cursor += 1;
        }
        Ok(WriteProgress::Done(
            run.results
                .into_iter()
                .map(|r| r.unwrap_or_else(|| Err(ItemError::Rejected("書き込まれなかった".into()))))
                .collect(),
        ))
    }
}

/// The device [`EchoFrameDriver`] talks to: two areas of
/// [`MEMORY_WORDS`] words, sessions handed out by the hello.
#[derive(Debug, Clone)]
pub struct EchoResponder {
    w: Vec<u16>,
    r: Vec<u16>,
    session: Option<u8>,
    last_session: u8,
}

impl EchoResponder {
    /// All words zero.
    pub fn new() -> Self {
        EchoResponder {
            w: vec![0; MEMORY_WORDS],
            r: vec![0; MEMORY_WORDS],
            session: None,
            last_session: 0,
        }
    }

    /// The values [`fixtures`] expects (word order high-low):
    /// `W0 = 0x1234`, `W1..=W2` = 1.5 as `F32`, `W10 = 0x0008` (bit 3),
    /// `W20..=W22` = `"HELLO"`, `W40..=W41` = -100000 as `I32`,
    /// `R0 = 0xFFFF`.
    pub fn with_sample_data() -> Self {
        let mut r = Self::new();
        r.w[0] = 0x1234;
        r.w[1] = 0x3FC0;
        r.w[2] = 0x0000;
        r.w[10] = 0x0008;
        r.w[20] = u16::from_le_bytes(*b"HE");
        r.w[21] = u16::from_le_bytes(*b"LL");
        r.w[22] = u16::from_le_bytes([b'O', 0]);
        let minus = (-100_000i32) as u32;
        r.w[40] = (minus >> 16) as u16;
        r.w[41] = minus as u16;
        r.r[0] = 0xFFFF;
        r
    }

    /// A word (`area` `'W'` or `'R'`), if inside the memory.
    pub fn word(&self, area: char, address: u16) -> Option<u16> {
        self.area(area as u8)?.get(usize::from(address)).copied()
    }

    /// Set a word; `false` if `area` / `address` is outside the memory.
    pub fn set_word(&mut self, area: char, address: u16, value: u16) -> bool {
        match self
            .area_mut(area as u8)
            .and_then(|a| a.get_mut(usize::from(address)))
        {
            Some(slot) => {
                *slot = value;
                true
            }
            None => false,
        }
    }

    fn area(&self, area: u8) -> Option<&Vec<u16>> {
        match area {
            b'W' => Some(&self.w),
            b'R' => Some(&self.r),
            _ => None,
        }
    }

    fn area_mut(&mut self, area: u8) -> Option<&mut Vec<u16>> {
        match area {
            b'W' => Some(&mut self.w),
            b'R' => Some(&mut self.r),
            _ => None,
        }
    }

    /// The response to one request frame.
    fn respond(&mut self, request: &[u8]) -> Vec<u8> {
        let Ok(req) = parse_frame(request) else {
            return frame(
                b'E',
                0,
                request.get(3).copied().unwrap_or(0),
                &[ERR_BAD_REQUEST],
            );
        };
        let (session, txn, p) = (req.session, req.txn, req.payload);
        let refuse = |code: u8, len: usize| {
            let mut payload = vec![0; len.max(1)];
            payload[0] = code;
            frame(b'E', session, txn, &payload)
        };
        match req.code {
            b'H' => {
                self.last_session = self.last_session.checked_add(1).unwrap_or(1);
                self.session = Some(self.last_session);
                frame(b'h', self.last_session, txn, &[self.last_session])
            }
            b'R' if p.len() == 4 => {
                let (area, start, count) = (
                    p[0],
                    usize::from(u16::from_be_bytes([p[1], p[2]])),
                    usize::from(p[3]),
                );
                let len = 2 * count;
                if self.session != Some(session) {
                    return refuse(ERR_SESSION, len);
                }
                if count == 0 {
                    return refuse(ERR_BAD_REQUEST, len);
                }
                let Some(mem) = self.area(area) else {
                    return refuse(ERR_BAD_REQUEST, len);
                };
                let Some(words) = mem.get(start..start + count) else {
                    return refuse(ERR_ADDRESS, len);
                };
                let data: Vec<u8> = words.iter().flat_map(|w| w.to_be_bytes()).collect();
                frame(b'r', session, txn, &data)
            }
            b'W' if p.len() >= 4 && p.len() == 4 + 2 * usize::from(p[3]) => {
                let (area, start, count) = (
                    p[0],
                    usize::from(u16::from_be_bytes([p[1], p[2]])),
                    usize::from(p[3]),
                );
                if self.session != Some(session) {
                    return refuse(ERR_SESSION, 1);
                }
                if area == b'R' {
                    return refuse(ERR_READ_ONLY, 1);
                }
                let Some(mem) = self.area_mut(area) else {
                    return refuse(ERR_BAD_REQUEST, 1);
                };
                let Some(slots) = mem.get_mut(start..start + count) else {
                    return refuse(ERR_ADDRESS, 1);
                };
                for (slot, c) in slots.iter_mut().zip(p[4..].chunks_exact(2)) {
                    *slot = u16::from_be_bytes([c[0], c[1]]);
                }
                frame(b'w', session, txn, &[0])
            }
            _ => refuse(ERR_BAD_REQUEST, 1),
        }
    }
}

impl Default for EchoResponder {
    fn default() -> Self {
        Self::new()
    }
}

impl Responder for EchoResponder {
    fn on_connect(&mut self) {
        self.session = None;
    }

    fn reply(&mut self, request: &[u8]) -> Reply {
        Reply::Bytes(self.respond(request))
    }

    /// The healthy response with an unknown response code (`?`): the length
    /// is right, the content is not.
    fn reply_broken(&mut self, request: &[u8]) -> Option<Reply> {
        let mut bytes = self.respond(request);
        bytes[1] = b'?';
        Some(Reply::Bytes(bytes))
    }
}

/// Fixtures matching [`EchoResponder::with_sample_data`], with the given
/// `framing` setting (one of the `FRAMING_*` constants).
pub fn fixtures(framing: &str) -> FrameFixtures {
    use DataType::*;
    let n = TagKind::numeric;
    let settings = Settings::from_pairs([
        (keys::HOST, "127.0.0.1"),
        (keys::PORT, "5020"),
        (KEY_FRAMING, framing),
    ])
    .expect("distinct keys");
    let statics = StaticFixtures::new(settings)
        .valid("w010", n(U16), "W10")
        .valid("W10.03", n(Bit), "W10.3")
        .valid(" r0 ", n(I16), "R0")
        .valid("W1", n(F32), "W1")
        .valid("W20", TagKind::string(3, StringEncoding::Utf8), "W20")
        .valid("W65532", n(U64), "W65532")
        .invalid("", n(U16))
        .invalid("X1", n(U16))
        .invalid("W", n(U16))
        .invalid("W-1", n(U16))
        .invalid("W70000", n(U16))
        .invalid("W1.16", n(Bit))
        .invalid("W1.", n(Bit))
        .invalid("W65535", n(U32))
        .offset("W10", n(U16), 5, Some("W15"))
        .offset("W10", n(U16), 0, Some("W10"))
        .offset("W10.15", n(Bit), 1, Some("W11.0"))
        .offset("W11.0", n(Bit), -1, Some("W10.15"))
        .offset("W0", n(U16), -1, None)
        .offset("W65535", n(U16), 1, None)
        .offset("W65534", n(U32), 1, None);
    FrameFixtures::new(statics)
        .read("W0", n(U16), RawValue::Uint(0x1234))
        .read("R0", n(U16), RawValue::Uint(0xFFFF))
        .read("R0", n(I16), RawValue::Int(-1))
        .read("W1", n(F32), RawValue::Float(1.5))
        .read("W1", n(U32), RawValue::Uint(0x3FC0_0000))
        .read("W10.3", n(Bit), RawValue::Bool(true))
        .read("W10.2", n(Bit), RawValue::Bool(false))
        .read(
            "W20",
            TagKind::string(3, StringEncoding::Utf8),
            RawValue::Bytes(b"HELLO\0".to_vec()),
        )
        .read("W40", n(I32), RawValue::Int(-100_000))
        .refused("W5000", n(U16))
        .unsupported("W10.3", n(U16))
        .unsupported("W0", n(Bit))
        .write("W200", n(U16), Value::F64(7.0))
        .write("W300.5", n(Bit), Value::Bit(true))
        .write("W400", n(F32), Value::F64(2.5))
        .write(
            "W500",
            TagKind::string(2, StringEncoding::Utf8),
            Value::Str("ab".into()),
        )
        .noise(vec![0x00, 0x11, 0x22, 0x33])
}
