//! 値の層のシミュレータ（docs/banto-hub-driver-sim-design.md §3、R2b-5a）。
//!
//! ワイヤ（Modbus / SLMP のフレーム）を作らず、`Session` の契約だけを守る。値は
//! [`SimDevice`] が持つ表（鍵は `DriverAddress::canonical` と `TagKind` の組。`D100.5` と
//! `D100`、同じ `D100` でも種類（型・文字列の語数・符号化）が違えば別のセル）で決まる。読み取りは、書かれた値（held）があればそれを返し、無ければ経過時間
//! と「初めて見た順」の index だけから決定的に生成する（背景タスクは無い）。書き込みは
//! 領域・値の種類・範囲・文字列長を検査し、通れば held にする。
//!
//! - 表は `Clone`（`Arc` の共有）で、[`SimDevice::connect`] で何度でもセッションを作れる。
//!   セッションを張り直しても held は残る。
//! - 生成は 100 ms ごとに変わる（ビットは反転、整数・浮動小数はランプ、文字列は
//!   `SIM-{index}-{tick}` の末尾を語数 × 2 バイト分だけ残す。短い文字列でも変わる）。index が隣のセルと違うので、隣は同じ値にならない。
//! - 待たない。致命は返さない（テストの鉤 `fail_next` だけが返す）。
//! - RMW（語の中のビット）と多語の重なりは再現しない（ドライバ crate の適合テストの仕事）。
//! - held は同じ種類のセルにだけ効く（種類が違えば別のセルで、変換も流用もしない）。
//! - 文字列の長さはタグの符号化で数える（Hub の `encode_string_value` と同じ規則。表せない
//!   文字と容量超過は断る）。
//!
//! `SessionDriver`（`DriverBase`）は実装しない: 設定の schema やアドレスの解析はシミュ
//! レータに意味が無いので、接続は [`SimDevice::connect`] で直接作る。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::decode::encoding_table;
use banto_driver_api::{
    BoxFuture, DataType, FatalError, ItemError, Session, TagKind, TagSpec, Value, WriteItem,
};

/// 生成値が変わる間隔。
pub const SIM_TICK: Duration = Duration::from_millis(100);

/// 浮動小数のランプの周期（0 以上この値未満）。
const FLOAT_RANGE: u128 = 1 << 16;

type Cells = HashMap<String, Vec<(TagKind, SimCell)>>;

struct SimCell {
    held: Option<Value>,
    /// 初めて見た順（位相に使う）。
    index: u32,
}

#[derive(Default)]
struct SimState {
    cells: Cells,
    /// 初めて見た順の次の番号（全セル通し）。
    next_index: u32,
    writes: u64,
    fail_next: Option<FatalError>,
}

impl SimState {
    fn cell(&mut self, canonical: &str, kind: &TagKind) -> &mut SimCell {
        let list = self.cells.entry(canonical.to_owned()).or_default();
        let pos = match list.iter().position(|(k, _)| k == kind) {
            Some(pos) => pos,
            None => {
                let index = self.next_index;
                self.next_index += 1;
                list.push((kind.clone(), SimCell { held: None, index }));
                list.len() - 1
            }
        };
        &mut list[pos].1
    }
}

/// シミュレーションの値の表。複製は同じ表を共有する。
#[derive(Clone)]
pub struct SimDevice {
    inner: Arc<Mutex<SimState>>,
    epoch: Instant,
}

impl Default for SimDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl SimDevice {
    pub fn new() -> Self {
        SimDevice {
            inner: Arc::new(Mutex::new(SimState::default())),
            epoch: Instant::now(),
        }
    }

    fn state(&self) -> MutexGuard<'_, SimState> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// この表を読み書きするセッションを作る（何度でも。held は共有される）。
    pub fn connect(&self) -> Box<dyn Session> {
        Box::new(SimSession::new(self.clone()))
    }

    /// 書かれた値（held）。生成値は返さない。
    pub fn get(&self, canonical: &str, kind: &TagKind) -> Option<Value> {
        self.state()
            .cells
            .get(canonical)
            .and_then(|l| l.iter().find(|(k, _)| k == kind))
            .and_then(|(_, c)| c.held.clone())
    }

    /// 値を held にする（書き込みの検査は通さない）。
    pub fn set(&self, canonical: &str, kind: TagKind, value: Value) {
        self.state().cell(canonical, &kind).held = Some(value);
    }

    /// 書き込みが通った回数。
    pub fn write_count(&self) -> u64 {
        self.state().writes
    }

    /// held のあるセルの数。
    pub fn held_count(&self) -> usize {
        self.state()
            .cells
            .values()
            .flatten()
            .filter(|(_, c)| c.held.is_some())
            .count()
    }

    /// 次の読み取りか書き込み 1 回を、この致命で失敗させる（テスト用）。
    #[doc(hidden)]
    pub fn fail_next(&self, error: FatalError) {
        self.state().fail_next = Some(error);
    }

    fn tick(&self) -> u64 {
        (self.epoch.elapsed().as_millis() / SIM_TICK.as_millis()) as u64
    }
}

/// [`SimDevice`] の 1 接続。
pub struct SimSession {
    device: SimDevice,
}

impl SimSession {
    pub fn new(device: SimDevice) -> Self {
        SimSession { device }
    }
}

/// 型の非負側の範囲（ランプの周期）。
fn ramp_range(data_type: DataType) -> u128 {
    match data_type {
        DataType::Bit => 2,
        DataType::I16 => 1 << 15,
        DataType::U16 => 1 << 16,
        DataType::I32 => 1 << 31,
        DataType::U32 => 1 << 32,
        DataType::I64 => 1 << 63,
        DataType::U64 => 1 << 64,
        DataType::F32 | DataType::F64 => FLOAT_RANGE,
    }
}

/// 生成値。時刻（tick）と index だけで決まる。
fn generate(spec: &TagSpec, index: u32, tick: u64) -> Value {
    let phase = tick as u128 + index as u128 * 16;
    match &spec.kind {
        TagKind::Str { words, .. } => {
            // ASCII なのでバイト位置で切ってよい。短い語数でも変わるよう末尾を残す。
            let s = format!("SIM-{index}-{tick}");
            let keep = (*words as usize * 2).min(s.len());
            Value::Str(s[s.len() - keep..].to_owned())
        }
        TagKind::Numeric(dt) => {
            if *dt == DataType::Bit || spec.address.bit.is_some() {
                Value::Bit((tick + index as u64) % 2 == 1)
            } else {
                Value::F64((phase % ramp_range(*dt)) as f64)
            }
        }
        // TagKind は non_exhaustive: 将来の種類は 0 を返す。
        #[allow(unreachable_patterns)]
        _ => Value::F64(0.0),
    }
}

/// 書き込みの検査。通れば held にする値を返す。
fn check_write(item: &WriteItem) -> Result<Value, ItemError> {
    let reject = |m: &str| Err(ItemError::Rejected(m.to_owned()));
    if !item.spec.address.writable_area {
        return reject("この領域は書き込めません");
    }
    match (&item.spec.kind, &item.value) {
        (TagKind::Numeric(DataType::Bit), Value::Bit(b)) => Ok(Value::Bit(*b)),
        (TagKind::Numeric(DataType::Bit), _) => reject("ビットには真偽値だけを書けます"),
        (TagKind::Numeric(dt), Value::F64(v)) => {
            // 整数: Hub の値の検査（apps/banto-hub/core/src/write_path.rs の `fract() != 0.0` で断る）と同じく、小数は断る。
            // 丸めず、範囲 [lo, hi) を検査してから、受けた値そのものを held にする。
            let (lo, hi) = match dt {
                DataType::I16 => (-32768.0, 32768.0),
                DataType::U16 => (0.0, 65536.0),
                DataType::I32 => (-2147483648.0, 2147483648.0),
                DataType::U32 => (0.0, 4294967296.0),
                DataType::I64 => (-9223372036854775808.0, 9223372036854775808.0),
                DataType::U64 => (0.0, 18446744073709551616.0),
                DataType::F32 => {
                    if v.is_finite() && !(*v as f32).is_finite() {
                        return reject("f32 の範囲外です");
                    }
                    return Ok(Value::F64(*v as f32 as f64));
                }
                _ => return Ok(Value::F64(*v)),
            };
            if !v.is_finite() || v.fract() != 0.0 {
                return reject("整数ではありません");
            }
            if *v < lo || *v >= hi {
                return reject("値が型の範囲外です");
            }
            Ok(Value::F64(*v))
        }
        (TagKind::Numeric(_), _) => reject("数値には数値だけを書けます"),
        (TagKind::Str { words, encoding }, Value::Str(s)) => {
            // Hub の `encode_string_value` と同じ規則: 表せない文字は断り、容量超過も断る。
            let (bytes, _, had_errors) = encoding_table(*encoding).encode(s);
            if had_errors {
                reject("このエンコーディングで表せない文字があります")
            } else if bytes.len() > *words as usize * 2 {
                reject("文字列が長すぎます")
            } else {
                Ok(Value::Str(s.clone()))
            }
        }
        (TagKind::Str { .. }, _) => reject("文字列タグには文字列だけを書けます"),
        #[allow(unreachable_patterns)]
        _ => reject("扱えない種類です"),
    }
}

impl Session for SimSession {
    fn read_batch<'a>(
        &'a mut self,
        specs: &'a [TagSpec],
    ) -> BoxFuture<'a, Result<Vec<Result<Value, ItemError>>, FatalError>> {
        Box::pin(async move {
            let tick = self.device.tick();
            let mut st = self.device.state();
            if let Some(e) = st.fail_next.take() {
                return Err(e);
            }
            Ok(specs
                .iter()
                .map(|spec| {
                    let cell = st.cell(&spec.address.canonical, &spec.kind);
                    Ok(match &cell.held {
                        Some(v) => v.clone(),
                        None => generate(spec, cell.index, tick),
                    })
                })
                .collect())
        })
    }

    fn write_batch<'a>(
        &'a mut self,
        items: &'a [WriteItem],
    ) -> BoxFuture<'a, Result<Vec<Result<(), ItemError>>, FatalError>> {
        Box::pin(async move {
            let mut st = self.device.state();
            if let Some(e) = st.fail_next.take() {
                return Err(e);
            }
            Ok(items
                .iter()
                .map(|item| {
                    let v = check_write(item)?;
                    st.cell(&item.spec.address.canonical, &item.spec.kind).held = Some(v);
                    st.writes += 1;
                    Ok(())
                })
                .collect())
        })
    }

    fn disconnect(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use banto_driver_api::{DriverAddress, DriverId, StringEncoding};

    fn addr(c: &str, writable: bool, words: u16) -> DriverAddress {
        DriverAddress::new(DriverId::new("sim").unwrap(), c, "D", writable, words)
    }
    fn spec(c: &str, kind: TagKind) -> TagSpec {
        let words = kind.word_count();
        TagSpec::new(addr(c, true, words), kind)
    }
    fn num(c: &str, dt: DataType) -> TagSpec {
        spec(c, TagKind::numeric(dt))
    }
    fn wr(s: &TagSpec, v: Value) -> WriteItem {
        WriteItem::new(s.clone(), v)
    }
    async fn read1(s: &mut Box<dyn Session>, sp: &TagSpec) -> Result<Value, ItemError> {
        s.read_batch(std::slice::from_ref(sp))
            .await
            .unwrap()
            .remove(0)
    }

    #[test]
    fn generation_is_a_pure_function_of_tick_and_index() {
        let s = num("D1", DataType::U16);
        assert_eq!(generate(&s, 3, 7), generate(&s, 3, 7));
        assert_eq!(generate(&s, 0, 5), Value::F64(5.0));
        assert_eq!(generate(&s, 1, 5), Value::F64(21.0));
        // u16 は 65536 でラップする。
        assert_eq!(generate(&s, 0, 65536 + 2), Value::F64(2.0));
        // 符号付きは非負の側だけ。
        assert_eq!(
            generate(&num("D1", DataType::I16), 0, 32768 + 1),
            Value::F64(1.0)
        );
        let b = num("M1", DataType::Bit);
        assert_eq!(generate(&b, 0, 1), Value::Bit(true));
        assert_eq!(generate(&b, 0, 2), Value::Bit(false));
        assert_eq!(generate(&b, 1, 2), Value::Bit(true));
        // bit: Some のアドレスは種類に関わらずビット。
        let bit_in_word = TagSpec::new(
            addr("D1.3", true, 1).with_bit(3),
            TagKind::numeric(DataType::U16),
        );
        assert_eq!(generate(&bit_in_word, 0, 1), Value::Bit(true));
    }

    #[tokio::test]
    async fn values_are_stable_within_a_tick_and_change_across_ticks() {
        let dev = SimDevice::new();
        let mut s = dev.connect();
        let sp = num("D1", DataType::U32);
        // 同じ tick の中で 2 度読めば同じ。tick をまたいだ回は捨てて取り直す。
        let mut stable = false;
        for _ in 0..50 {
            let t0 = dev.tick();
            let a = read1(&mut s, &sp).await.unwrap();
            let b = read1(&mut s, &sp).await.unwrap();
            if dev.tick() == t0 {
                assert_eq!(a, b);
                stable = true;
                break;
            }
        }
        assert!(stable, "同じ tick の中で読めなかった");
        // 時間が進めば変わる（期限つきで待つ）。
        let first = read1(&mut s, &sp).await.unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if read1(&mut s, &sp).await.unwrap() != first {
                break;
            }
            assert!(Instant::now() < deadline, "値が変わらない");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn neighbouring_cells_differ() {
        let dev = SimDevice::new();
        let mut s = dev.connect();
        let specs = [
            num("D1", DataType::U16),
            num("D2", DataType::U16),
            num("D3", DataType::U16),
        ];
        let r = s.read_batch(&specs).await.unwrap();
        let v: Vec<_> = r.into_iter().map(|x| x.unwrap()).collect();
        assert_ne!(v[0], v[1]);
        assert_ne!(v[1], v[2]);
        assert_ne!(v[0], v[2]);
    }

    #[tokio::test]
    async fn read_returns_one_result_per_spec_in_order_and_empty_is_empty() {
        let dev = SimDevice::new();
        let mut s = dev.connect();
        assert!(s.read_batch(&[]).await.unwrap().is_empty());
        assert!(s.write_batch(&[]).await.unwrap().is_empty());
        let a = num("D1", DataType::U16);
        let b = num("D2", DataType::U16);
        dev.set("D2", TagKind::numeric(DataType::U16), Value::F64(7.0));
        let r = s.read_batch(&[b.clone(), a.clone(), b]).await.unwrap();
        assert_eq!(r.len(), 3);
        assert_eq!(r[0], Ok(Value::F64(7.0)));
        assert_eq!(r[2], Ok(Value::F64(7.0)));
    }

    #[tokio::test]
    async fn held_survives_a_new_session_and_is_observable() {
        let dev = SimDevice::new();
        let sp = num("D10", DataType::I16);
        let mut s1 = dev.connect();
        let k = TagKind::numeric(DataType::I16);
        assert_eq!(dev.get("D10", &k), None);
        s1.write_batch(&[wr(&sp, Value::F64(42.0))]).await.unwrap()[0]
            .clone()
            .unwrap();
        s1.disconnect().await;
        drop(s1);
        let mut s2 = dev.connect();
        assert_eq!(read1(&mut s2, &sp).await, Ok(Value::F64(42.0)));
        assert_eq!(dev.get("D10", &k), Some(Value::F64(42.0)));
        assert_eq!(dev.write_count(), 1);
        assert_eq!(dev.held_count(), 1);
        // 別の SimDevice とは共有しない。
        assert_eq!(SimDevice::new().get("D10", &k), None);
        // D10.5 は別のセル。
        assert_eq!(dev.get("D10.5", &k), None);
    }

    #[tokio::test]
    async fn write_rejections() {
        let dev = SimDevice::new();
        let mut s = dev.connect();
        let rejected = |r: &Result<(), ItemError>| matches!(r, Err(ItemError::Rejected(_)));
        let ro = TagSpec::new(addr("X1", false, 1), TagKind::numeric(DataType::U16));
        let bit = num("M1", DataType::Bit);
        let u16s = num("D1", DataType::U16);
        let i16s = num("D2", DataType::I16);
        let s4 = spec("D3", TagKind::string(2, StringEncoding::Utf8));
        let items = [
            wr(&ro, Value::F64(1.0)),            // 0 書けない領域
            wr(&bit, Value::F64(1.0)),           // 1 種類違い
            wr(&u16s, Value::Bit(true)),         // 2 種類違い
            wr(&u16s, Value::F64(65536.0)),      // 3 範囲外
            wr(&u16s, Value::F64(-1.0)),         // 4 範囲外
            wr(&u16s, Value::F64(f64::NAN)),     // 5 非有限
            wr(&s4, Value::Str("abcde".into())), // 6 長すぎ
            wr(&s4, Value::F64(1.0)),            // 7 種類違い
            wr(&u16s, Value::F64(1.5)),          // 8 小数
            wr(&u16s, Value::F64(65535.0)),      // 9 通る
            wr(&i16s, Value::F64(-32768.0)),     // 10 通る
            wr(&s4, Value::Str("abcd".into())),  // 11 通る
            wr(&bit, Value::Bit(true)),          // 12 通る
        ];
        let r = s.write_batch(&items).await.unwrap();
        for (i, x) in r.iter().enumerate().take(9) {
            assert!(rejected(x), "{i}: {x:?}");
        }
        for (i, x) in r.iter().enumerate().skip(9) {
            assert_eq!(x, &Ok(()), "{i}");
        }
        assert_eq!(dev.write_count(), 4);
        assert_eq!(
            dev.get("D1", &TagKind::numeric(DataType::U16)),
            Some(Value::F64(65535.0))
        );
        assert_eq!(dev.get("X1", &TagKind::numeric(DataType::U16)), None);
    }

    #[tokio::test]
    async fn strings_are_truncated_to_the_word_span() {
        let dev = SimDevice::new();
        let mut s = dev.connect();
        let long = spec("D1", TagKind::string(64, StringEncoding::Utf8));
        let short = spec("D200", TagKind::string(1, StringEncoding::Utf8));
        let r = s.read_batch(&[long, short]).await.unwrap();
        let Ok(Value::Str(a)) = &r[0] else {
            panic!("{r:?}")
        };
        let Ok(Value::Str(b)) = &r[1] else {
            panic!("{r:?}")
        };
        assert!(a.starts_with("SIM-0-"), "{a}");
        // 末尾を残す: 2 語目は最初の読み取りの tick の末尾 2 バイト。
        assert_eq!(b.len(), 2);
        assert!(b.chars().all(|c| c.is_ascii_digit() || c == '-'), "{b}");
    }

    #[test]
    fn short_strings_keep_the_tail_and_change_across_ticks() {
        for words in 1..=3u16 {
            let sp = spec("D1", TagKind::string(words, StringEncoding::Utf8));
            let a = generate(&sp, 0, 10);
            let b = generate(&sp, 0, 11);
            assert_ne!(a, b, "words={words}");
            let Value::Str(s) = &a else { panic!() };
            assert_eq!(s.len(), words as usize * 2);
            assert!("SIM-0-10".ends_with(s.as_str()));
        }
    }

    #[tokio::test]
    async fn cells_are_typed_by_kind() {
        let dev = SimDevice::new();
        let mut s = dev.connect();
        let s4 = spec("D100", TagKind::string(4, StringEncoding::Utf8));
        s.write_batch(&[wr(&s4, Value::Str("abcdefgh".into()))])
            .await
            .unwrap()[0]
            .clone()
            .unwrap();
        assert_eq!(read1(&mut s, &s4).await, Ok(Value::Str("abcdefgh".into())));
        // 種類が違えば held は効かず、生成値になる。
        let u = num("D100", DataType::U16);
        assert!(matches!(read1(&mut s, &u).await, Ok(Value::F64(_))));
        let s2 = spec("D100", TagKind::string(2, StringEncoding::Utf8));
        let Ok(Value::Str(g)) = read1(&mut s, &s2).await else {
            panic!()
        };
        assert!(g.len() <= 4 && g != "abcdefgh");
        let s4sj = spec("D100", TagKind::string(4, StringEncoding::ShiftJis));
        assert_ne!(
            read1(&mut s, &s4sj).await,
            Ok(Value::Str("abcdefgh".into()))
        );
        assert_eq!(dev.held_count(), 1);
        assert_eq!(dev.get("D100", &u.kind), None);
    }

    #[tokio::test]
    async fn string_length_is_counted_in_the_tag_encoding() {
        let dev = SimDevice::new();
        let mut s = dev.connect();
        let sj = spec("D1", TagKind::string(1, StringEncoding::ShiftJis));
        let u8t = spec("D2", TagKind::string(1, StringEncoding::Utf8));
        let sj2 = spec("D3", TagKind::string(4, StringEncoding::ShiftJis));
        let r = s
            .write_batch(&[
                wr(&sj, Value::Str("あ".into())),
                wr(&u8t, Value::Str("あ".into())),
                wr(&sj2, Value::Str("😀".into())),
            ])
            .await
            .unwrap();
        assert_eq!(r[0], Ok(()));
        assert!(matches!(r[1], Err(ItemError::Rejected(_))), "{:?}", r[1]);
        assert!(matches!(r[2], Err(ItemError::Rejected(_))), "{:?}", r[2]);
    }

    #[tokio::test]
    async fn fail_next_fails_exactly_one_call() {
        let dev = SimDevice::new();
        let mut s = dev.connect();
        let sp = num("D1", DataType::U16);
        dev.fail_next(FatalError::Io("x".into()));
        assert_eq!(
            s.read_batch(std::slice::from_ref(&sp)).await,
            Err(FatalError::Io("x".into()))
        );
        assert!(s.read_batch(std::slice::from_ref(&sp)).await.is_ok());
        dev.fail_next(FatalError::Timeout);
        assert_eq!(
            s.write_batch(&[wr(&sp, Value::F64(1.0))]).await,
            Err(FatalError::Timeout)
        );
        assert_eq!(dev.write_count(), 0);
        assert!(s.write_batch(&[wr(&sp, Value::F64(1.0))]).await.is_ok());
    }
}
