//! Dev-only `LegacySession` (docs/banto-driver-api-design.md §7.3 step 2,
//! R1-5): this crate's `banto_broker::BrokerSession` wrapped as a
//! `banto_driver_api::Session`, so that `banto-driver-conformance`'s
//! `session::run_all_against` can check the R1 Modbus driver as it runs in
//! production. Not a driver: nothing outside the tests uses it.
//!
//! - [`LegacyDriver`] is [`ModbusDriver`]'s `DriverBase` plus a `connect`
//!   that builds the connection the way `dial` does (`modbus_config_for` on
//!   a `PlcConnection` made from the settings, the inverse of
//!   `banto_tags::settings_to_api`), with the context's
//!   timeouts, and runs this crate's `connector`.
//! - [`LegacySession`] turns each `TagSpec` back into the request the
//!   production read / write paths build (`Address::parse` of the canonical
//!   address; a `.N` write stays `Numeric`, never `BitInWord`, exactly like
//!   `write_path::build_plc_write_request` for a Modbus connection) and maps
//!   every error with this crate's `classify_read` / `classify_write`.
//!
//! **Fatal errors arrive as text.** `BrokerSession` reports a fatal error as
//! `SessionError(String)` (the broker only ever logs it), so the adapter
//! recovers the `PlcError` / `PlcWriteError` variant from its `Display`
//! (`plc_error_from` / `plc_write_error_from`, pinned by a round-trip test)
//! before classifying it. Anything unrecognised is `Connection` (fatal
//! `Io`), the classification's own default direction.
//!
//! The SLMP twin is `banto-driver-slmp/tests/common/mod.rs`; the two differ
//! only in the parser, the config and the `BitInWord` rule.

#![allow(dead_code)]

use banto_broker::BrokerSession;
use banto_driver_api::{
    keys, AddressError, BoxFuture, ConnectContext, DriverAddress, DriverBase, DriverDescriptor,
    FatalError, FieldError, ItemError, Session, SessionDriver, Settings, TagKind, TagSpec, Value,
    WriteItem,
};
use banto_driver_modbus::{
    classify_read, classify_write, connector, modbus_config_for, ModbusDriver,
};
use banto_plc::{
    Address, BatchReadRequest, BatchReadResult, PlcError, ReadRequest, StringReadRequest, TagValue,
};
use banto_plc_write::{
    BatchWriteRequest, PlcWriteError, StringWriteRequest, WriteRequest, WriteResult,
};
use banto_tags::PlcConnection;

/// The protocol's parser for a canonical address.
/// A word address of this protocol (for the tests below).
const WORD: &str = "40001";

fn parse(canonical: &str) -> Result<Address, PlcError> {
    Address::parse(canonical)
}

/// The write request for a numeric / bit `item` at `address`: always
/// `Numeric` on Modbus (no bit-in-word writes in R1).
fn numeric_write(
    address: Address,
    data_type: banto_plc::DataType,
    value: TagValue,
) -> BatchWriteRequest {
    BatchWriteRequest::Numeric(WriteRequest {
        address,
        data_type,
        value,
    })
}

/// [`ModbusDriver`] with a `connect` (see the module doc).
#[derive(Debug, Clone, Copy, Default)]
pub struct LegacyDriver;

impl DriverBase for LegacyDriver {
    fn descriptor(&self) -> &DriverDescriptor {
        ModbusDriver.descriptor()
    }
    fn validate_config(&self, settings: &Settings) -> Vec<FieldError> {
        ModbusDriver.validate_config(settings)
    }
    fn secret_keys(&self) -> Vec<String> {
        ModbusDriver.secret_keys()
    }
    fn parse_address(&self, text: &str, kind: &TagKind) -> Result<DriverAddress, AddressError> {
        ModbusDriver.parse_address(text, kind)
    }
    fn offset_address(
        &self,
        canonical: &str,
        kind: &TagKind,
        n: i64,
    ) -> Result<String, AddressError> {
        ModbusDriver.offset_address(canonical, kind, n)
    }
    fn address_help(&self) -> String {
        ModbusDriver.address_help()
    }
}

/// The `PlcConnection` whose `banto_tags::settings_to_api` is
/// `settings` (absent keys take the column defaults).
fn connection_from(settings: &Settings) -> Result<(PlcConnection, u16), FatalError> {
    let port: u16 = settings
        .get(keys::PORT)
        .and_then(|p| p.parse().ok())
        .ok_or_else(|| FatalError::InvalidConfig("port が無いか数でない".into()))?;
    let conn = PlcConnection {
        id: 1,
        name: "conformance".to_string(),
        protocol: banto_driver_modbus::PROTOCOL.to_string(),
        host: settings.get(keys::HOST).unwrap_or_default().to_string(),
        port: i64::from(port),
        unit_id: settings
            .get(keys::UNIT_ID)
            .and_then(|u| u.parse().ok())
            .unwrap_or(1),
        enabled: true,
        simulation: false,
        word_order: settings
            .get(keys::WORD_ORDER)
            .unwrap_or("low_high")
            .to_string(),
        cpu_type: settings.get(keys::CPU_TYPE).unwrap_or("R").to_string(),
        database: None,
        username: None,
        password: None,
        settings: None,
    };
    Ok((conn, port))
}

impl SessionDriver for LegacyDriver {
    fn connect(
        &self,
        settings: &Settings,
        cx: ConnectContext,
    ) -> BoxFuture<'static, Result<Box<dyn Session>, FatalError>> {
        let conn = connection_from(settings);
        Box::pin(async move {
            let (conn, port) = conn?;
            let mut config = modbus_config_for(&conn, port);
            config.connect_timeout = cx.connect_timeout;
            config.response_timeout = cx.response_timeout;
            let inner = connector(config)()
                .await
                .map_err(|m| fatal(classify_read(plc_error_from(&m))))?;
            Ok(Box::new(LegacySession { inner }) as Box<dyn Session>)
        })
    }
}

/// A `BrokerSession` as an `api::Session`.
pub struct LegacySession {
    inner: Box<dyn BrokerSession>,
}

/// The fatal half of a classification (an `ItemError` where a fatal error
/// was reported is itself a broken contract: fall to `Io`).
fn fatal(c: Result<ItemError, FatalError>) -> FatalError {
    match c {
        Err(f) => f,
        Ok(item) => FatalError::Io(format!("致命として報告された非致命のエラー: {item:?}")),
    }
}

/// Requests for the ones that can be made, item errors for the rest.
enum Prep<R> {
    Request(R),
    Item(ItemError),
}

/// Fill `prepared`'s slots from `results` (one per `Request`, in order).
///
/// If the inner `BrokerSession` broke its count contract (`results` is not
/// one per `Request`), the mismatch is **passed on, not hidden**: the
/// output is the item errors followed by every inner result, so its length
/// differs from the caller's request count exactly as the inner one did,
/// and the harness's own count check reports it as C3 ("N 件の要求に M 件の
/// 結果"). A `FatalError` instead would read as an ordinary connection
/// failure (and would even satisfy the checks that expect one, C4 / C6 /
/// C7), and a panic would abort the whole run with every other check
/// unreported.
fn stitch<R, T>(
    prepared: Vec<Prep<R>>,
    results: Vec<Result<T, ItemError>>,
) -> Vec<Result<T, ItemError>> {
    let requested = prepared
        .iter()
        .filter(|p| matches!(p, Prep::Request(_)))
        .count();
    if results.len() != requested {
        return prepared
            .into_iter()
            .filter_map(|p| match p {
                Prep::Item(e) => Some(Err(e)),
                Prep::Request(_) => None,
            })
            .chain(results)
            .collect();
    }
    let mut results = results.into_iter();
    prepared
        .into_iter()
        .map(|p| match p {
            Prep::Item(e) => Err(e),
            Prep::Request(_) => results
                .next()
                .expect("one result per request (counted above)"),
        })
        .collect()
}

fn read_request(spec: &TagSpec) -> Result<Prep<BatchReadRequest>, FatalError> {
    let address = parse(&spec.address.canonical).map_err(|e| fatal(classify_read(e)))?;
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

fn write_request(item: &WriteItem) -> Result<Prep<BatchWriteRequest>, FatalError> {
    let address = parse(&item.spec.address.canonical).map_err(|e| fatal(classify_read(e)))?;
    Ok(match (&item.spec.kind, &item.value) {
        (TagKind::Str { words, encoding }, Value::Str(s)) => {
            Prep::Request(BatchWriteRequest::String(StringWriteRequest {
                address,
                words: *words,
                value: s.clone(),
                encoding: *encoding,
            }))
        }
        (TagKind::Numeric(data_type), Value::Bit(b)) => {
            Prep::Request(numeric_write(address, *data_type, TagValue::Bit(*b)))
        }
        (TagKind::Numeric(data_type), Value::F64(x)) => {
            Prep::Request(numeric_write(address, *data_type, TagValue::F64(*x)))
        }
        (kind, value) => Prep::Item(ItemError::Unsupported(format!(
            "{kind:?} に {value:?} は書けない"
        ))),
    })
}

impl Session for LegacySession {
    fn read_batch<'a>(
        &'a mut self,
        specs: &'a [TagSpec],
    ) -> BoxFuture<'a, Result<Vec<Result<Value, ItemError>>, FatalError>> {
        Box::pin(async move {
            let prepared = specs
                .iter()
                .map(read_request)
                .collect::<Result<Vec<_>, _>>()?;
            let requests: Vec<BatchReadRequest> = prepared
                .iter()
                .filter_map(|p| match p {
                    Prep::Request(r) => Some(*r),
                    Prep::Item(_) => None,
                })
                .collect();
            let results = self
                .inner
                .read_batch(&requests)
                .await
                .map_err(|e| fatal(classify_read(plc_error_from(&e.0))))?;
            let mut mapped = Vec::with_capacity(results.len());
            for r in results {
                mapped.push(match r {
                    BatchReadResult::Value(v) => Ok(v),
                    BatchReadResult::Bad(e) => Err(classify_read(e)?),
                });
            }
            Ok(stitch(prepared, mapped))
        })
    }

    fn write_batch<'a>(
        &'a mut self,
        items: &'a [WriteItem],
    ) -> BoxFuture<'a, Result<Vec<Result<(), ItemError>>, FatalError>> {
        Box::pin(async move {
            let prepared = items
                .iter()
                .map(write_request)
                .collect::<Result<Vec<_>, _>>()?;
            let requests: Vec<BatchWriteRequest> = prepared
                .iter()
                .filter_map(|p| match p {
                    Prep::Request(r) => Some(r.clone()),
                    Prep::Item(_) => None,
                })
                .collect();
            let results = self
                .inner
                .write_batch(&requests)
                .await
                .map_err(|e| fatal(classify_write(plc_write_error_from(&e.0))))?;
            let mut mapped = Vec::with_capacity(results.len());
            for r in results {
                mapped.push(match r {
                    WriteResult::Ok => Ok(()),
                    WriteResult::Bad(e) => Err(classify_write(e)?),
                });
            }
            Ok(stitch(prepared, mapped))
        })
    }

    fn disconnect(&mut self) -> BoxFuture<'_, ()> {
        self.inner.disconnect()
    }
}

/// The fatal `PlcError` whose `Display` is `message` (see the module doc).
pub fn plc_error_from(message: &str) -> PlcError {
    let m = message;
    if m == PlcError::ResponseTimeout.to_string() {
        PlcError::ResponseTimeout
    } else if m == PlcError::NotConnected.to_string() {
        PlcError::NotConnected
    } else if let Some(rest) = m.strip_prefix("プロトコルエラー: ") {
        PlcError::Protocol(rest.to_string())
    } else if let Some(rest) = m.strip_prefix("接続タイムアウト: ") {
        PlcError::ConnectTimeout(rest.to_string())
    } else if let Some(rest) = m.strip_prefix("接続エラー: ") {
        PlcError::Connection(rest.to_string())
    } else if let Some(rest) = m.strip_prefix("アドレス形式が不正です: ") {
        PlcError::InvalidAddress(rest.to_string())
    } else {
        PlcError::Connection(m.to_string())
    }
}

/// The fatal `PlcWriteError` whose `Display` is `message`.
pub fn plc_write_error_from(message: &str) -> PlcWriteError {
    let m = message;
    if m == PlcWriteError::ResponseTimeout.to_string() {
        PlcWriteError::ResponseTimeout
    } else if m == PlcWriteError::NotConnected.to_string() {
        PlcWriteError::NotConnected
    } else if let Some(rest) = m.strip_prefix("プロトコルエラー: ") {
        PlcWriteError::Protocol(rest.to_string())
    } else if let Some(rest) = m.strip_prefix("接続タイムアウト: ") {
        PlcWriteError::ConnectTimeout(rest.to_string())
    } else if let Some(rest) = m.strip_prefix("接続エラー: ") {
        PlcWriteError::Connection(rest.to_string())
    } else {
        PlcWriteError::Connection(m.to_string())
    }
}

/// Every fatal variant survives `Display` -> `*_from` (a reworded message
/// would silently become `Io`).
#[test]
fn fatal_errors_round_trip_through_their_text() {
    for e in [
        PlcError::ResponseTimeout,
        PlcError::NotConnected,
        PlcError::Protocol("x".into()),
        PlcError::ConnectTimeout("h:1".into()),
        PlcError::Connection("reset".into()),
        PlcError::InvalidAddress("??".into()),
    ] {
        assert_eq!(plc_error_from(&e.to_string()), e);
        assert!(e.is_connection_fatal());
    }
    for e in [
        PlcWriteError::ResponseTimeout,
        PlcWriteError::NotConnected,
        PlcWriteError::Protocol("x".into()),
        PlcWriteError::ConnectTimeout("h:1".into()),
        PlcWriteError::Connection("reset".into()),
    ] {
        assert_eq!(plc_write_error_from(&e.to_string()), e);
    }
}

/// A `BrokerSession` that breaks the count contract: one result more than
/// requested, for reads and writes.
struct OneTooMany;

impl BrokerSession for OneTooMany {
    fn read_batch<'a>(
        &'a mut self,
        requests: &'a [BatchReadRequest],
    ) -> BoxFuture<'a, Result<Vec<BatchReadResult>, banto_broker::SessionError>> {
        let n = requests.len() + 1;
        Box::pin(async move { Ok(vec![BatchReadResult::Value(Value::F64(1.0)); n]) })
    }
    fn write_batch<'a>(
        &'a mut self,
        requests: &'a [BatchWriteRequest],
    ) -> BoxFuture<'a, Result<Vec<WriteResult>, banto_broker::SessionError>> {
        let n = requests.len() + 1;
        Box::pin(async move { Ok(vec![WriteResult::Ok; n]) })
    }
    fn disconnect(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

/// The adapter passes an inner count violation on (so the harness reports
/// C3) instead of trimming it to the request count.
#[tokio::test]
async fn an_extra_inner_result_is_not_hidden() {
    let kind = TagKind::numeric(banto_plc::DataType::U16);
    let spec = TagSpec::new(LegacyDriver.parse_address(WORD, &kind).unwrap(), kind);
    // One request plus one item error that never reaches the inner session.
    let bad = TagSpec::new(
        LegacyDriver
            .parse_address(WORD, &TagKind::numeric(banto_plc::DataType::Bit))
            .unwrap(),
        TagKind::numeric(banto_plc::DataType::Bit),
    );
    let mut s = LegacySession {
        inner: Box::new(OneTooMany),
    };
    let specs = [spec.clone(), spec.clone()];
    let got = s.read_batch(&specs).await.unwrap();
    assert_eq!(got.len(), specs.len() + 1, "{got:?}");
    let items = [
        WriteItem::new(spec.clone(), Value::F64(1.0)),
        WriteItem::new(bad, Value::Str("x".into())),
    ];
    let got = s.write_batch(&items).await.unwrap();
    assert_eq!(got.len(), items.len() + 1, "{got:?}");
    assert!(matches!(got[0], Err(ItemError::Unsupported(_))), "{got:?}");
}
