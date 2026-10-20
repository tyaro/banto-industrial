//! [`FakeSessionDriver`]: a session-type driver over an in-memory map
//! (docs/banto-driver-api-design.md §7.4).
//!
//! For host-side tests that need "a driver" but no device: the values live
//! in a map keyed by canonical address that the test fills and inspects,
//! and [`FakeSessionDriver::fail_next`] makes the next read or write
//! connection-fatal, to drive the host's reconnect path. Clones share the
//! same state, so a test keeps one clone and hands another to the host.
//!
//! Addresses: any text without whitespace (trimmed); the canonical form is
//! the trimmed text. A trailing decimal number can be offset (`X10` + 2 =
//! `X12`); a `.n` suffix (0-15) is a bit index.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use banto_driver_api::{
    keys, AddressError, BoxFuture, Capabilities, ConnectContext, DataType, DeviceError,
    DriverAddress, DriverBase, DriverDescriptor, DriverId, FatalError, FieldError, ItemError,
    Session, SessionDriver, Settings, TagKind, TagSpec, Value, WriteItem,
};

/// The registry's string cap (`banto-tags` `string_length`).
const MAX_STRING_WORDS: u16 = 128;

#[derive(Debug, Default)]
struct State {
    values: HashMap<String, Value>,
    fail_next: Option<FatalError>,
    connects: usize,
    disconnects: usize,
}

/// A session-type driver whose "device" is a shared map (see the module
/// doc).
#[derive(Debug, Clone)]
pub struct FakeSessionDriver {
    descriptor: DriverDescriptor,
    state: Arc<Mutex<State>>,
}

impl FakeSessionDriver {
    /// An empty fake registered as `id` (read and write).
    pub fn new(id: DriverId) -> Self {
        FakeSessionDriver {
            descriptor: DriverDescriptor::new(
                id,
                env!("CARGO_PKG_VERSION"),
                "Fake（適合テストの偽セッション）",
                Capabilities::READ_WRITE,
            ),
            state: Arc::default(),
        }
    }

    /// The same fake with `canonical` holding `value`.
    pub fn with_value(self, canonical: &str, value: Value) -> Self {
        self.set(canonical, value);
        self
    }

    fn state(&self) -> MutexGuard<'_, State> {
        // A test that panicked while holding the lock must not hide the
        // values from the next assertion.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Store `value` at `canonical`.
    pub fn set(&self, canonical: &str, value: Value) {
        self.state().values.insert(canonical.to_owned(), value);
    }

    /// The value at `canonical`, as last set or written.
    pub fn get(&self, canonical: &str) -> Option<Value> {
        self.state().values.get(canonical).cloned()
    }

    /// Make the next `read_batch` / `write_batch` (on any session of this
    /// fake) fail with `error`, once.
    pub fn fail_next(&self, error: FatalError) {
        self.state().fail_next = Some(error);
    }

    /// How many sessions were connected.
    pub fn connect_count(&self) -> usize {
        self.state().connects
    }

    /// How many sessions were disconnected.
    pub fn disconnect_count(&self) -> usize {
        self.state().disconnects
    }
}

/// `(stem, number)` of a trailing decimal number.
fn trailing_number(s: &str) -> Option<(&str, u64)> {
    let digits = s.bytes().rev().take_while(u8::is_ascii_digit).count();
    if digits == 0 || digits > 18 {
        return None;
    }
    let (stem, num) = s.split_at(s.len() - digits);
    Some((stem, num.parse().ok()?))
}

impl DriverBase for FakeSessionDriver {
    fn descriptor(&self) -> &DriverDescriptor {
        &self.descriptor
    }

    /// Nothing is required; only `transport` other than `tcp` is refused
    /// (R1 accepts TCP only, design §8).
    fn validate_config(&self, settings: &Settings) -> Vec<FieldError> {
        match settings.get(keys::TRANSPORT) {
            None | Some("tcp") => Vec::new(),
            Some(other) => vec![FieldError::new(
                keys::TRANSPORT,
                format!("{other:?} は使えません（tcp だけ）"),
            )],
        }
    }

    fn parse_address(&self, text: &str, kind: &TagKind) -> Result<DriverAddress, AddressError> {
        let t = text.trim();
        if t.is_empty() || t.chars().any(char::is_whitespace) {
            return Err(AddressError::new(format!(
                "{text:?}: 空白を含まない 1 語で書きます"
            )));
        }
        let (stem, bit) = match t.rsplit_once('.') {
            Some((stem, b)) => match b.parse::<u8>() {
                Ok(bit) if bit <= 15 && !stem.is_empty() => (stem, Some(bit)),
                _ => return Err(AddressError::new(format!("{t:?}: ビットは .0〜.15"))),
            },
            None => (t, None),
        };
        let area: String = stem.chars().take_while(|c| !c.is_ascii_digit()).collect();
        let a = DriverAddress::new(
            self.descriptor.id.clone(),
            t,
            if area.is_empty() {
                "fake".to_owned()
            } else {
                area
            },
            true,
            kind.word_count(),
        );
        Ok(match bit {
            Some(b) => a.with_bit(b),
            None => a,
        })
    }

    fn offset_address(
        &self,
        canonical: &str,
        _kind: &TagKind,
        n: i64,
    ) -> Result<String, AddressError> {
        if n == 0 {
            return Ok(canonical.to_owned());
        }
        let (stem, num) = trailing_number(canonical)
            .ok_or_else(|| AddressError::new(format!("{canonical:?} は末尾が数字でない")))?;
        let moved = i128::from(num) + i128::from(n);
        if moved < 0 || moved > i128::from(u64::MAX) {
            return Err(AddressError::new(format!(
                "{canonical} から {n} ずらすと範囲の外です"
            )));
        }
        Ok(format!("{stem}{moved}"))
    }

    fn address_help(&self) -> String {
        "空白を含まない 1 語（例 X10、X10.3）。末尾の数字でずらせます".to_owned()
    }
}

impl SessionDriver for FakeSessionDriver {
    fn connect(
        &self,
        _settings: &Settings,
        _cx: ConnectContext,
    ) -> BoxFuture<'static, Result<Box<dyn Session>, FatalError>> {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            state.lock().unwrap_or_else(|e| e.into_inner()).connects += 1;
            Ok(Box::new(FakeSession { state }) as Box<dyn Session>)
        })
    }
}

/// Whether `value` fits `kind`.
fn check_kind(kind: &TagKind, value: &Value) -> Result<(), ItemError> {
    let ok = match (kind, value) {
        (TagKind::Numeric(DataType::Bit), Value::Bit(_)) => true,
        (TagKind::Numeric(DataType::Bit), _) => false,
        (TagKind::Numeric(_), Value::F64(_)) => true,
        (TagKind::Str { words, .. }, Value::Str(_)) => {
            if *words == 0 || *words > MAX_STRING_WORDS {
                return Err(ItemError::Unsupported(format!(
                    "文字列は 1〜{MAX_STRING_WORDS} 語（{words} 語）"
                )));
            }
            true
        }
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(ItemError::Unsupported(format!(
            "{kind:?} と {value:?} は合わない"
        )))
    }
}

struct FakeSession {
    state: Arc<Mutex<State>>,
}

impl FakeSession {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Session for FakeSession {
    fn read_batch<'a>(
        &'a mut self,
        specs: &'a [TagSpec],
    ) -> BoxFuture<'a, Result<Vec<Result<Value, ItemError>>, FatalError>> {
        let result = {
            let mut state = self.state();
            match state.fail_next.take() {
                Some(e) => Err(e),
                None => Ok(specs
                    .iter()
                    .map(|spec| match state.values.get(&spec.address.canonical) {
                        None => Err(ItemError::Device(DeviceError::new(2, "値がありません"))),
                        Some(v) => check_kind(&spec.kind, v).map(|()| v.clone()),
                    })
                    .collect()),
            }
        };
        Box::pin(async move { result })
    }

    fn write_batch<'a>(
        &'a mut self,
        items: &'a [WriteItem],
    ) -> BoxFuture<'a, Result<Vec<Result<(), ItemError>>, FatalError>> {
        let result = {
            let mut state = self.state();
            match state.fail_next.take() {
                Some(e) => Err(e),
                None => Ok(items
                    .iter()
                    .map(|item| {
                        check_kind(&item.spec.kind, &item.value)?;
                        state
                            .values
                            .insert(item.spec.address.canonical.clone(), item.value.clone());
                        Ok(())
                    })
                    .collect()),
            }
        };
        Box::pin(async move { result })
    }

    fn disconnect(&mut self) -> BoxFuture<'_, ()> {
        self.state().disconnects += 1;
        Box::pin(async {})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{static_checks, StaticFixtures};
    use banto_driver_api::StringEncoding;
    use std::time::Duration;

    fn fake() -> FakeSessionDriver {
        FakeSessionDriver::new(DriverId::new("fake").unwrap())
            .with_value("X1", Value::F64(3.0))
            .with_value("M0.2", Value::Bit(true))
            .with_value("S0", Value::Str("ok".into()))
    }

    fn spec(d: &FakeSessionDriver, text: &str, kind: TagKind) -> TagSpec {
        TagSpec::new(d.parse_address(text, &kind).unwrap(), kind)
    }

    fn cx() -> ConnectContext {
        ConnectContext::new("c", Duration::from_secs(1), Duration::from_secs(1))
    }

    #[test]
    fn passes_the_static_checks() {
        let n = TagKind::numeric;
        let f = StaticFixtures::new(Settings::new())
            .valid(" X1 ", n(DataType::U16), "X1")
            .valid("M0.2", n(DataType::Bit), "M0.2")
            .invalid("", n(DataType::U16))
            .invalid("a b", n(DataType::U16))
            .invalid("M0.16", n(DataType::Bit))
            .offset("X1", n(DataType::U16), 9, Some("X10"))
            .offset("X1", n(DataType::U16), -2, None)
            .offset("S", n(DataType::U16), 1, None);
        assert_eq!(static_checks(&fake(), &f), vec![]);
    }

    #[tokio::test]
    async fn reads_writes_and_classifies() {
        let d = fake();
        let mut s = d.connect(&Settings::new(), cx()).await.unwrap();
        assert_eq!(d.connect_count(), 1);
        let n = TagKind::numeric;
        let specs = [
            spec(&d, "X1", n(DataType::F32)),
            spec(&d, "M0.2", n(DataType::Bit)),
            spec(&d, "S0", TagKind::string(2, StringEncoding::Utf8)),
            spec(&d, "Y9", n(DataType::U16)),
            spec(&d, "X1", n(DataType::Bit)),
            spec(&d, "S0", TagKind::string(0, StringEncoding::Utf8)),
        ];
        let got = s.read_batch(&specs).await.unwrap();
        assert_eq!(got[0], Ok(Value::F64(3.0)));
        assert_eq!(got[1], Ok(Value::Bit(true)));
        assert_eq!(got[2], Ok(Value::Str("ok".into())));
        assert!(matches!(got[3], Err(ItemError::Device(_))), "{:?}", got[3]);
        assert!(
            matches!(got[4], Err(ItemError::Unsupported(_))),
            "{:?}",
            got[4]
        );
        assert!(
            matches!(got[5], Err(ItemError::Unsupported(_))),
            "{:?}",
            got[5]
        );

        let items = [
            WriteItem::new(spec(&d, "X2", n(DataType::U16)), Value::F64(5.0)),
            WriteItem::new(spec(&d, "X3", n(DataType::U16)), Value::Bit(true)),
        ];
        let w = s.write_batch(&items).await.unwrap();
        assert_eq!(w[0], Ok(()));
        assert!(matches!(w[1], Err(ItemError::Unsupported(_))));
        assert_eq!(d.get("X2"), Some(Value::F64(5.0)));
        assert_eq!(d.get("X3"), None);

        s.disconnect().await;
        assert_eq!(d.disconnect_count(), 1);
    }

    #[tokio::test]
    async fn fail_next_is_fatal_once() {
        let d = fake();
        let mut s = d.connect(&Settings::new(), cx()).await.unwrap();
        let specs = [spec(&d, "X1", TagKind::numeric(DataType::F64))];
        d.fail_next(FatalError::Timeout);
        assert_eq!(s.read_batch(&specs).await, Err(FatalError::Timeout));
        assert_eq!(
            s.read_batch(&specs).await.unwrap(),
            vec![Ok(Value::F64(3.0))]
        );
        d.fail_next(FatalError::Io("切断".into()));
        assert_eq!(s.write_batch(&[]).await, Err(FatalError::Io("切断".into())));
        // A new session after the fatal error works, and shares the values.
        let mut s2 = d.connect(&Settings::new(), cx()).await.unwrap();
        assert_eq!(
            s2.read_batch(&specs).await.unwrap(),
            vec![Ok(Value::F64(3.0))]
        );
        assert_eq!(d.connect_count(), 2);
    }

    #[test]
    fn only_tcp_transport_is_accepted() {
        let d = fake();
        assert!(d
            .validate_config(&Settings::from_pairs([(keys::TRANSPORT, "tcp")]).unwrap())
            .is_empty());
        let e = d.validate_config(&Settings::from_pairs([(keys::TRANSPORT, "serial")]).unwrap());
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].key, keys::TRANSPORT);
    }
}
