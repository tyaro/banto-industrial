//! The request gates of the SLMP frame driver (`frame_conn.rs`): the canonical
//! address turned back into a `banto_plc::Address` with this driver's own
//! grammar (`Address::parse_slmp`; no public bridge back to `banto_plc::Address`
//! is used), the limit on the last device number, and the decision that makes a
//! `bit`-addressed `bit` write (`D100.5`) the read-modify-write `BitInWord`
//! (the decision the Hub used to make, now this driver's). R2b-3d-join: this is
//! what stays of the session-type `api_session.rs`, which is gone.

use banto_driver_api::{DataType, FatalError, ItemError, TagKind, TagSpec, Value, WriteItem};
use banto_plc::{Address, BatchReadRequest, ReadRequest, StringReadRequest, TagValue};
use banto_plc_write::{BatchWriteRequest, StringWriteRequest, WriteRequest as NumericWriteRequest};

use crate::classify_read;

/// Requests for the ones that can be made, item errors for the rest
/// (the frame connection's `plan_read` / `write_next`, `frame_conn.rs`).
pub(crate) enum Prep<R> {
    Request(R),
    Item(ItemError),
}

/// #793 のレビュー: 保存時の `parse_address` を経ずに来た spec（手で組んだ
/// `TagSpec` など）でも、占める最後の番号が上限 0xFFFFFF を超えるものは、
/// そのタグだけの `ItemError::Unsupported` にして要求を組まない。読み取り
/// （セッションと、フレーム型の `plan_read`）と書き込みの入口で使う。これが
/// 無いと、セッションの計画は上限をまたぐ要求をそのまま送り、フレーム型の
/// 計画は同じまとまりの正常なタグまで巻き込んで致命になる。
/// 判定と文言は保存時と同じ `base::past_last_number`。
fn past_last_number(spec: &TagSpec, address: &Address) -> Option<ItemError> {
    let (device, number, bit) = address.as_slmp()?;
    crate::base::past_last_number(&spec.address.canonical, device, number, bit, &spec.kind)
        .map(ItemError::Unsupported)
}

/// The read request of `spec`: this driver's grammar on the canonical address.
pub(crate) fn read_request(spec: &TagSpec) -> Result<Prep<BatchReadRequest>, FatalError> {
    // An address that does not parse is that tag's problem, not the
    // connection's (R2b-2a, address design §6 Q5).
    let address = match Address::parse_slmp(&spec.address.canonical) {
        Ok(address) => address,
        Err(e) => return classify_read(e).map(Prep::Item),
    };
    if let Some(item) = past_last_number(spec, &address) {
        return Ok(Prep::Item(item));
    }
    Ok(match &spec.kind {
        TagKind::Numeric(data_type) => Prep::Request(BatchReadRequest::Numeric(ReadRequest {
            address,
            data_type: *data_type,
        })),
        TagKind::Str { words, encoding } => {
            Prep::Request(BatchReadRequest::String(StringReadRequest {
                address,
                words: *words,
                encoding: *encoding,
            }))
        }
        other => Prep::Item(ItemError::Unsupported(format!("{other:?} は扱えない"))),
    })
}

/// The write request of `item`. A `bit`-addressed `bit` tag (`D100.5`) is
/// the read-modify-write `BitInWord`; its value must be a `Bit`. Everything
/// else is an ordinary numeric / string write (a bit *device* `M50` has no
/// `bit`, so it is `Numeric`). Also the frame connection's `write_next`
/// (`frame_conn.rs`), so the two make the same decision by construction.
pub(crate) fn write_request(item: &WriteItem) -> Result<Prep<BatchWriteRequest>, FatalError> {
    let spec = &item.spec;
    // An address that does not parse is that tag's problem, not the
    // connection's (R2b-2a, address design §6 Q5).
    let address = match Address::parse_slmp(&spec.address.canonical) {
        Ok(address) => address,
        Err(e) => return classify_read(e).map(Prep::Item),
    };
    if let Some(item) = past_last_number(spec, &address) {
        return Ok(Prep::Item(item));
    }
    if spec.address.bit.is_some() && spec.kind == TagKind::Numeric(DataType::Bit) {
        return Ok(match &item.value {
            Value::Bit(value) => Prep::Request(BatchWriteRequest::BitInWord {
                address,
                value: *value,
            }),
            other => Prep::Item(ItemError::Unsupported(format!(
                "ビット指定のアドレスに {other:?} は書けない"
            ))),
        });
    }
    let numeric = |data_type, value| {
        Prep::Request(BatchWriteRequest::Numeric(NumericWriteRequest {
            address,
            data_type,
            value,
        }))
    };
    Ok(match (&spec.kind, &item.value) {
        (TagKind::Str { words, encoding }, Value::Str(s)) => {
            Prep::Request(BatchWriteRequest::String(StringWriteRequest {
                address,
                words: *words,
                value: s.clone(),
                encoding: *encoding,
            }))
        }
        (TagKind::Numeric(data_type), Value::Bit(b)) => numeric(*data_type, TagValue::Bit(*b)),
        (TagKind::Numeric(data_type), Value::F64(x)) => numeric(*data_type, TagValue::F64(*x)),
        (kind, value) => Prep::Item(ItemError::Unsupported(format!(
            "{kind:?} に {value:?} は書けない"
        ))),
    })
}

#[cfg(test)]
mod tests {
    use banto_driver_api::DriverBase;

    use super::*;
    use crate::SlmpDriver;

    fn n(data_type: DataType) -> TagKind {
        TagKind::numeric(data_type)
    }

    fn spec(text: &str, kind: TagKind) -> TagSpec {
        TagSpec::new(SlmpDriver.parse_address(text, &kind).unwrap(), kind)
    }

    /// The RMW decision is the Hub's (`write_path.rs`:
    /// `matches!(address, Address::Slmp { bit: Some(_), .. })`) over the same
    /// addresses: `BitInWord` exactly when the canonical address has a `.N`
    /// suffix, and a bit device or word address stays `Numeric`.
    #[test]
    fn bit_in_word_is_chosen_exactly_when_the_hub_would() {
        for (text, kind, value) in [
            ("D100.5", n(DataType::Bit), Value::Bit(true)),
            ("D100.F", n(DataType::Bit), Value::Bit(false)),
            ("W10.0", n(DataType::Bit), Value::Bit(true)),
            ("ZR5.A", n(DataType::Bit), Value::Bit(true)),
            ("M50", n(DataType::Bit), Value::Bit(true)),
            ("X1A", n(DataType::Bit), Value::Bit(false)),
            ("SM400", n(DataType::Bit), Value::Bit(true)),
            ("D100", n(DataType::U16), Value::F64(1.0)),
            ("D110", n(DataType::F32), Value::F64(1.5)),
        ] {
            let item = WriteItem::new(spec(text, kind), value);
            let hub_says_rmw = matches!(
                Address::parse_slmp(&item.spec.address.canonical),
                Ok(Address::Slmp { bit: Some(_), .. })
            );
            let ours = match write_request(&item).unwrap() {
                Prep::Request(BatchWriteRequest::BitInWord { .. }) => true,
                Prep::Request(BatchWriteRequest::Numeric(_)) => false,
                _ => panic!("{text}: neither BitInWord nor Numeric"),
            };
            assert_eq!(ours, hub_says_rmw, "{text}");
            assert_eq!(item.spec.address.bit.is_some(), hub_says_rmw, "{text}");
        }
    }

    /// A spec that skipped the save path's `parse_address` (which refuses
    /// these since #793).
    fn unchecked(canonical: &str, kind: TagKind) -> TagSpec {
        let words = kind.word_count();
        TagSpec::new(
            banto_driver_api::DriverAddress::new(
                SlmpDriver.descriptor().id.clone(),
                canonical,
                "D",
                true,
                words,
            ),
            kind,
        )
    }

    /// #793 のレビュー（非ブロッカー）: 上限をはみ出す spec は、読み取りでも
    /// 書き込みでもそのタグだけの `Unsupported` で、要求を組まない。上限
    /// ちょうどで終わるものは要求になる。
    #[test]
    fn a_spec_running_past_the_last_device_number_is_an_item_error() {
        let sjis = |w| TagKind::string(w, banto_driver_api::StringEncoding::ShiftJis);
        for spec in [
            unchecked("D16777215", n(DataType::U32)),
            unchecked("ZR16777215", n(DataType::F32)),
            unchecked("ZR16777214", sjis(3)),
        ] {
            assert!(
                matches!(
                    read_request(&spec),
                    Ok(Prep::Item(ItemError::Unsupported(_)))
                ),
                "read {spec:?}"
            );
            let value = match spec.kind {
                TagKind::Str { .. } => Value::Str("x".into()),
                _ => Value::F64(1.0),
            };
            let item = WriteItem::new(spec.clone(), value);
            assert!(
                matches!(
                    write_request(&item),
                    Ok(Prep::Item(ItemError::Unsupported(_)))
                ),
                "write {spec:?}"
            );
        }
        for spec in [
            spec("D16777214", n(DataType::U32)),
            spec("ZR16777214", sjis(2)),
            spec("D16777215.F", n(DataType::Bit)),
        ] {
            assert!(
                matches!(read_request(&spec), Ok(Prep::Request(_))),
                "{spec:?}"
            );
        }
    }

    /// #793 の 3 回目: 入口の文言は保存時と同じで、ビットデバイスは「点」。
    #[test]
    fn the_entry_says_words_or_points_like_the_save_path() {
        let word = unchecked("D16777215", n(DataType::U32));
        let Ok(Prep::Item(ItemError::Unsupported(m))) = read_request(&word) else {
            panic!("D16777215 U32 should be an item error");
        };
        let saved = SlmpDriver
            .parse_address("D16777215", &n(DataType::U32))
            .unwrap_err();
        // `AddressError` shows itself with a 「アドレスが不正です: 」 prefix.
        assert!(saved.to_string().ends_with(&m), "{saved} / {m}");
        assert!(m.contains("2 語分を占めると"), "{m}");
        // A bit device holding a 32-bit value (only by hand: check_kind
        // refuses it when saved) spreads over 32 points.
        let bits = unchecked("M16777200", n(DataType::U32));
        let Ok(Prep::Item(ItemError::Unsupported(m))) = read_request(&bits) else {
            panic!("M16777200 U32 should be an item error");
        };
        assert!(m.contains("32 点分を占めると"), "{m}");
        assert!(!m.contains("語"), "{m}");
    }
}
