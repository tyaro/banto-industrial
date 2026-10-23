//! The request-building half this driver's frame-type connection
//! (`frame_conn.rs`) shares: the `unit_id` setting, the read / write request
//! of a `TagSpec` / `WriteItem` (this driver's own grammar on the canonical
//! address - `Address::parse`; no public bridge back to `banto_plc::Address`
//! is used) and the fatal half of a classification. It was the shared part of
//! the session-type `api_session.rs` that R2b-3c removed.
//!
//! A bit-in-word write (`bit` set on a `bit` tag) is `ItemError::Unsupported`
//! here: Modbus has no read-modify-write (the SLMP twin makes it `BitInWord`).

use banto_driver_api::{
    keys, DataType, FatalError, ItemError, Settings, TagKind, TagSpec, Value, WriteItem,
};
use banto_plc::{
    Address, BatchReadRequest, ModbusTcpConfig, ReadRequest, StringReadRequest, TagValue,
};
use banto_plc_write::{BatchWriteRequest, StringWriteRequest, WriteRequest as NumericWriteRequest};

use crate::classify_read;

/// The `unit_id` setting (absent: the default, 1).
pub(crate) fn unit_id_of(settings: &Settings) -> Result<u8, FatalError> {
    match settings.get(keys::UNIT_ID) {
        None => Ok(ModbusTcpConfig::default().unit_id),
        Some(u) => u.trim().parse::<u8>().map_err(|_| {
            FatalError::InvalidConfig(format!("unit_id {u:?} は 0〜255 の数でありません"))
        }),
    }
}

/// The fatal half of a classification. An `ItemError` where the executor
/// reported a connection-fatal error is a broken table: fall to `Io`.
pub(crate) fn fatal(c: Result<ItemError, FatalError>) -> FatalError {
    match c {
        Err(f) => f,
        Ok(item) => FatalError::Io(format!("致命として報告された非致命のエラー: {item:?}")),
    }
}

/// Requests for the ones that can be made, item errors for the rest.
pub(crate) enum Prep<R> {
    Request(R),
    Item(ItemError),
}

/// The read request of `spec`: this driver's grammar on the canonical address.
pub(crate) fn read_request(spec: &TagSpec) -> Result<Prep<BatchReadRequest>, FatalError> {
    // An address that does not parse is that tag's problem, not the
    // connection's (R2b-2a, address design §6 Q5).
    let address = match Address::parse(&spec.address.canonical) {
        Ok(address) => address,
        Err(e) => return classify_read(e).map(Prep::Item),
    };
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

/// The write request of `item`. Modbus never reads-modifies-writes a bit of a
/// register: a `bit`-addressed `bit` tag is `Unsupported` (the SLMP twin
/// makes it `BitInWord`).
pub(crate) fn write_request(item: &WriteItem) -> Result<Prep<BatchWriteRequest>, FatalError> {
    let spec = &item.spec;
    if spec.address.bit.is_some() && spec.kind == TagKind::Numeric(DataType::Bit) {
        return Ok(Prep::Item(ItemError::Unsupported(
            "Modbus はレジスタの 1 ビットだけの書き込み（読み出し - 変更 - 書き込み）に対応していません"
                .into(),
        )));
    }
    let address = match Address::parse(&spec.address.canonical) {
        Ok(address) => address,
        Err(e) => return classify_read(e).map(Prep::Item),
    };
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
