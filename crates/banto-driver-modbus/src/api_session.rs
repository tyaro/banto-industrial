//! The production [`banto_driver_api::SessionDriver`] / [`Session`] of this
//! driver (R2b-3a-1, docs/banto-hub-driver-io-design.md §4.1). It is the
//! former dev-only `LegacySession` (`tests/common/mod.rs`) promoted, with the
//! one thing the test double could not do: **fatal errors keep their type**.
//!
//! - [`ModbusDriver::connect`] builds the [`ModbusTcpConfig`] from the
//!   [`Settings`] (`host`, `port`, `unit_id`, `word_order`; an absent
//!   `word_order` is the column default `low_high`, like the stored
//!   connection) with the [`ConnectContext`]'s two timeouts, dials with
//!   `banto_plc::dial_modbus` and wraps the stream in the same
//!   [`ModbusSession`] (one socket) - so reads and writes still share the
//!   **one** transaction-id counter.
//! - [`ModbusApiSession::read_batch`] turns each `TagSpec`'s canonical address
//!   back into a `banto_plc::Address` with this driver's own grammar
//!   (`Address::parse`; the private [`read_request`] / [`write_request`] - the
//!   public `Address::from_driver_address` is not used) and runs the existing
//!   plan + execute (`ModbusSession::read_typed` / `write_typed`). The typed
//!   `PlcError` / `PlcWriteError` goes through `classify_read` /
//!   `classify_write`; nothing is recovered from text.
//! - A bit-in-word write (`bit` set on a `bit` tag) is
//!   `ItemError::Unsupported` here: Modbus has no read-modify-write
//!   (the SLMP twin sends it as `BitInWord`).
//!
//! Registered by `banto-drivers` as `DriverEntry::session` since R2b-3a-2:
//! the broker dials every connection of this protocol through it.

use banto_driver_api::{
    keys, BoxFuture, ConnectContext, DataType, FatalError, ItemError, Session, SessionDriver,
    Settings, TagKind, TagSpec, Value, WriteItem,
};
use banto_plc::{
    Address, BatchReadRequest, BatchReadResult, ModbusTcpConfig, ReadRequest, StringReadRequest,
    TagValue,
};
use banto_plc_write::{
    BatchWriteRequest, StringWriteRequest, WriteRequest as NumericWriteRequest, WriteResult,
};

use crate::session::ModbusSession;
use crate::{classify_read, classify_write, ModbusDriver};

/// [`ModbusTcpConfig`] from the settings and the context's timeouts.
fn config_from(settings: &Settings, cx: &ConnectContext) -> Result<ModbusTcpConfig, FatalError> {
    let host = settings
        .get(keys::HOST)
        .filter(|h| !h.trim().is_empty())
        .ok_or_else(|| FatalError::InvalidConfig("host がありません".into()))?;
    let port: u16 = settings
        .get(keys::PORT)
        .and_then(|p| p.trim().parse().ok())
        .ok_or_else(|| {
            FatalError::InvalidConfig("port が無いか 0〜65535 の数でありません".into())
        })?;
    let unit_id = match settings.get(keys::UNIT_ID) {
        None => ModbusTcpConfig::default().unit_id,
        Some(u) => u.trim().parse::<u8>().map_err(|_| {
            FatalError::InvalidConfig(format!("unit_id {u:?} は 0〜255 の数でありません"))
        })?,
    };
    Ok(ModbusTcpConfig {
        host: host.to_string(),
        port,
        unit_id,
        connect_timeout: cx.connect_timeout,
        response_timeout: cx.response_timeout,
        word_order: banto_broker::parse_word_order(
            settings.get(keys::WORD_ORDER).unwrap_or("low_high"),
        ),
    })
}

impl SessionDriver for ModbusDriver {
    fn connect(
        &self,
        settings: &Settings,
        cx: ConnectContext,
    ) -> BoxFuture<'static, Result<Box<dyn Session>, FatalError>> {
        let config = config_from(settings, &cx);
        Box::pin(async move {
            let config = config?;
            let stream = banto_plc::dial_modbus(&config)
                .await
                .map_err(|e| fatal(classify_read(e)))?;
            Ok(Box::new(ModbusApiSession {
                inner: ModbusSession::new(stream, &config),
            }) as Box<dyn Session>)
        })
    }
}

/// The live session (the type behind `Box<dyn Session>`).
pub(crate) struct ModbusApiSession {
    inner: ModbusSession,
}

/// The fatal half of a classification. An `ItemError` where the executor
/// reported a connection-fatal error is a broken table: fall to `Io`.
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

/// Fill the slots of `prepared` from `results` (one per `Request`, in order).
/// A count the executor broke is a contract violation: fatal, not a panic.
fn stitch<R, T>(
    prepared: Vec<Prep<R>>,
    results: Vec<Result<T, ItemError>>,
) -> Result<Vec<Result<T, ItemError>>, FatalError> {
    let requested = prepared
        .iter()
        .filter(|p| matches!(p, Prep::Request(_)))
        .count();
    if results.len() != requested {
        return Err(FatalError::Protocol(format!(
            "{requested} 件の要求に {} 件の結果",
            results.len()
        )));
    }
    let mut results = results.into_iter();
    Ok(prepared
        .into_iter()
        .map(|p| match p {
            Prep::Item(e) => Err(e),
            Prep::Request(_) => results
                .next()
                .expect("one result per request (counted above)"),
        })
        .collect())
}

/// The read request of `spec`: this driver's grammar on the canonical address.
fn read_request(spec: &TagSpec) -> Result<Prep<BatchReadRequest>, FatalError> {
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
fn write_request(item: &WriteItem) -> Result<Prep<BatchWriteRequest>, FatalError> {
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

impl Session for ModbusApiSession {
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
                .read_typed(&requests)
                .await
                .map_err(|e| fatal(classify_read(e)))?;
            let mut mapped = Vec::with_capacity(results.len());
            for r in results {
                mapped.push(match r {
                    BatchReadResult::Value(v) => Ok(v),
                    BatchReadResult::Bad(e) => Err(classify_read(e)?),
                });
            }
            stitch(prepared, mapped)
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
                .write_typed(&requests)
                .await
                .map_err(|e| fatal(classify_write(e)))?;
            let mut mapped = Vec::with_capacity(results.len());
            for r in results {
                mapped.push(match r {
                    WriteResult::Ok => Ok(()),
                    WriteResult::Bad(e) => Err(classify_write(e)?),
                });
            }
            stitch(prepared, mapped)
        })
    }

    fn disconnect(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(self.inner.close())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use banto_driver_api::DriverBase;
    use banto_plc::modbus::simulator::Simulator;

    use super::*;

    fn n(data_type: DataType) -> TagKind {
        TagKind::numeric(data_type)
    }

    fn spec(text: &str, kind: TagKind) -> TagSpec {
        TagSpec::new(ModbusDriver.parse_address(text, &kind).unwrap(), kind)
    }

    async fn connect(sim: &Simulator, response_ms: u64) -> Box<dyn Session> {
        let settings = Settings::from_pairs([
            ("host", sim.addr.ip().to_string()),
            ("port", sim.addr.port().to_string()),
        ])
        .unwrap();
        let cx = ConnectContext::new(
            "t",
            Duration::from_secs(2),
            Duration::from_millis(response_ms),
        );
        ModbusDriver.connect(&settings, cx).await.unwrap()
    }

    /// The bit-in-word write decision, written out independently of the
    /// implementation: Modbus never makes a `BitInWord`, and a `bit` tag with
    /// a `.N` address is `Unsupported` (the SLMP twin's decision differs; the
    /// Hub's `matches!(address, Address::Slmp { bit: Some(_), .. })` can never
    /// hold for a Modbus address).
    #[test]
    fn a_bit_in_register_write_is_unsupported_and_never_bit_in_word() {
        for (text, kind, value, bit_suffix) in [
            ("40051.3", n(DataType::Bit), Value::Bit(true), true),
            ("40051.0", n(DataType::Bit), Value::Bit(false), true),
            ("00011", n(DataType::Bit), Value::Bit(true), false),
            ("40101", n(DataType::U16), Value::F64(1.0), false),
        ] {
            let item = WriteItem::new(spec(text, kind), value);
            assert_eq!(item.spec.address.bit.is_some(), bit_suffix, "{text}");
            match write_request(&item).unwrap() {
                Prep::Request(BatchWriteRequest::BitInWord { .. }) => {
                    panic!("{text}: Modbus must never build BitInWord")
                }
                Prep::Request(_) => assert!(!bit_suffix, "{text}: a .N write must not be sent"),
                Prep::Item(ItemError::Unsupported(_)) => {
                    assert!(bit_suffix, "{text}: only a .N write is refused here")
                }
                Prep::Item(other) => panic!("{text}: {other:?}"),
            }
        }
    }

    /// A broken result count is fatal, not trimmed and not a panic.
    #[test]
    fn a_result_count_mismatch_is_fatal() {
        let prepared = vec![
            Prep::Request(()),
            Prep::Item(ItemError::Unsupported("x".into())),
        ];
        let got = stitch::<(), Value>(prepared, vec![Ok(Value::Bit(true)), Ok(Value::Bit(true))]);
        assert!(matches!(got, Err(FatalError::Protocol(_))), "{got:?}");
        let prepared = vec![Prep::Request(()), Prep::Request(())];
        let got = stitch::<(), Value>(prepared, vec![Ok(Value::Bit(true))]);
        assert!(matches!(got, Err(FatalError::Protocol(_))), "{got:?}");
    }

    /// The type of a fatal error survives to the host (it used to travel as
    /// text): a response with the wrong transaction id is `Framing`, a
    /// response that never comes is `Timeout`, a closed socket is `Io`.
    #[tokio::test]
    async fn fatal_errors_keep_their_type() {
        let sim = Simulator::start().await;
        let mut s = connect(&sim, 200).await;
        let specs = [spec("40011", n(DataType::U16))];

        sim.set_wrong_transaction_id(true);
        let r = s.read_batch(&specs).await;
        assert!(matches!(r, Err(FatalError::Framing(_))), "{r:?}");

        let mut s = connect(&sim, 200).await;
        sim.set_wrong_transaction_id(false);
        sim.hang();
        let r = s.read_batch(&specs).await;
        assert!(matches!(r, Err(FatalError::Timeout)), "{r:?}");
        sim.stop_hanging();
        let w = s
            .write_batch(&[WriteItem::new(
                spec("40101", n(DataType::U16)),
                Value::F64(1.0),
            )])
            .await;
        // After a timeout the connection is the host's to drop; a write on
        // it must not succeed with a late response.
        assert!(w.is_err() || matches!(w.as_deref(), Ok([Err(_)])), "{w:?}");

        let mut s = connect(&sim, 200).await;
        sim.stop();
        let r = s.read_batch(&specs).await;
        assert!(matches!(r, Err(FatalError::Io(_))), "{r:?}");
    }

    #[test]
    fn config_comes_from_the_settings_and_the_context() {
        let cx = ConnectContext::new("k", Duration::from_millis(1234), Duration::from_millis(567));
        let s = Settings::from_pairs([
            ("host", "10.0.0.7"),
            ("port", "1502"),
            ("unit_id", "9"),
            ("word_order", "high_low"),
        ])
        .unwrap();
        let c = config_from(&s, &cx).unwrap();
        assert_eq!(c.host, "10.0.0.7");
        assert_eq!((c.port, c.unit_id), (1502, 9));
        assert_eq!(c.word_order, banto_plc::WordOrder::HighLow);
        assert_eq!(c.connect_timeout, Duration::from_millis(1234));
        assert_eq!(c.response_timeout, Duration::from_millis(567));
        // Absent word_order is the column default, low_high.
        let s = Settings::from_pairs([("host", "h"), ("port", "502")]).unwrap();
        let c = config_from(&s, &cx).unwrap();
        assert_eq!(c.word_order, banto_plc::WordOrder::LowHigh);
        assert_eq!(c.unit_id, 1);
        for bad in [
            Settings::from_pairs([("port", "502")]).unwrap(),
            Settings::from_pairs([("host", "h")]).unwrap(),
            Settings::from_pairs([("host", "h"), ("port", "70000")]).unwrap(),
            Settings::from_pairs([("host", "h"), ("port", "1"), ("unit_id", "256")]).unwrap(),
        ] {
            assert!(
                matches!(config_from(&bad, &cx), Err(FatalError::InvalidConfig(_))),
                "{bad:?}"
            );
        }
    }
}
