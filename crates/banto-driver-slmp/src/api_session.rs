//! The production [`banto_driver_api::SessionDriver`] / [`Session`] of this
//! driver (R2b-3a-1, docs/banto-hub-driver-io-design.md §4.1). It is the
//! former dev-only `LegacySession` (`tests/common/mod.rs`) promoted, with the
//! one thing the test double could not do: **fatal errors keep their type**.
//!
//! - [`SlmpDriver::connect`] builds the [`SlmpConfig`] from the [`Settings`]
//!   (`host`, `port`, `word_order`, `cpu_type`; absent ones take the column
//!   defaults `low_high` / `R`, like the stored connection) with the
//!   [`ConnectContext`]'s two timeouts, dials with `banto_plc::dial_slmp` and
//!   wraps the client in a [`SlmpSession`] (one client for reads and
//!   writes).
//! - [`SlmpApiSession::read_batch`] turns each `TagSpec`'s canonical address
//!   back into a `banto_plc::Address` with this driver's own grammar
//!   (`Address::parse_slmp`; the private [`read_request`] /
//!   [`write_request`] - the public `Address::from_driver_address` is not
//!   used) and runs the existing plan + execute (`SlmpSession::read_typed` /
//!   `write_typed`). The typed `PlcError` / `PlcWriteError` goes through
//!   `classify_read` / `classify_write`; nothing is recovered from text.
//! - A `bit`-addressed `bit` tag (`D100.5`) is `BatchWriteRequest::BitInWord`,
//!   the read-modify-write: the decision the Hub makes today with
//!   `matches!(address, Address::Slmp { bit: Some(_), .. })`
//!   (`write_path.rs`), moved to the driver. A bit *device* (`M50`) stays an
//!   ordinary `Numeric` write. A test pins the two decisions to the same set.
//!
//! Registered by `banto-drivers` as `DriverEntry::session` since R2b-3a-2:
//! the broker dials every connection of this protocol through it.

use banto_driver_api::{
    keys, BoxFuture, ConnectContext, DataType, FatalError, ItemError, Session, SessionDriver,
    Settings, TagKind, TagSpec, Value, WriteItem,
};
use banto_plc::{
    Address, BatchReadRequest, BatchReadResult, ReadRequest, SlmpConfig, StringReadRequest,
    TagValue,
};
use banto_plc_write::{
    BatchWriteRequest, StringWriteRequest, WriteRequest as NumericWriteRequest, WriteResult,
};

use crate::session::SlmpSession;
use crate::{classify_read, classify_write, SlmpDriver};

/// [`SlmpConfig`] from the settings and the context's timeouts.
fn config_from(settings: &Settings, cx: &ConnectContext) -> Result<SlmpConfig, FatalError> {
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
    Ok(SlmpConfig {
        host: host.to_string(),
        port,
        connect_timeout: cx.connect_timeout,
        response_timeout: cx.response_timeout,
        word_order: banto_broker::parse_word_order(
            settings.get(keys::WORD_ORDER).unwrap_or("low_high"),
        ),
        cpu: crate::parse_cpu_type(settings.get(keys::CPU_TYPE).unwrap_or("R")),
        ..SlmpConfig::default()
    })
}

impl SessionDriver for SlmpDriver {
    fn connect(
        &self,
        settings: &Settings,
        cx: ConnectContext,
    ) -> BoxFuture<'static, Result<Box<dyn Session>, FatalError>> {
        let config = config_from(settings, &cx);
        Box::pin(async move {
            let config = config?;
            let client = banto_plc::dial_slmp(&config).await.map_err(connect_error)?;
            Ok(Box::new(SlmpApiSession {
                inner: SlmpSession::new(client, config.word_order),
            }) as Box<dyn Session>)
        })
    }
}

/// The live session (the type behind `Box<dyn Session>`).
pub(crate) struct SlmpApiSession {
    inner: SlmpSession,
}

/// A failed dial. A connect timeout is `FatalError::Timeout` (so the
/// connection test reports `timeout`, as before #788), not the `Io` that
/// `classify_read` makes of it for a session already in use.
fn connect_error(err: banto_plc::PlcError) -> FatalError {
    match err {
        banto_plc::PlcError::ConnectTimeout(_) => FatalError::Timeout,
        other => fatal(classify_read(other)),
    }
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
    let address = match Address::parse_slmp(&spec.address.canonical) {
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

/// The write request of `item`. A `bit`-addressed `bit` tag (`D100.5`) is
/// the read-modify-write `BitInWord`; its value must be a `Bit`. Everything
/// else is an ordinary numeric / string write (a bit *device* `M50` has no
/// `bit`, so it is `Numeric`).
fn write_request(item: &WriteItem) -> Result<Prep<BatchWriteRequest>, FatalError> {
    let spec = &item.spec;
    // An address that does not parse is that tag's problem, not the
    // connection's (R2b-2a, address design §6 Q5).
    let address = match Address::parse_slmp(&spec.address.canonical) {
        Ok(address) => address,
        Err(e) => return classify_read(e).map(Prep::Item),
    };
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

impl Session for SlmpApiSession {
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
    use banto_plc::slmp::simulator::Simulator;

    use super::*;

    #[test]
    fn a_connect_timeout_is_a_timeout_and_a_refusal_is_io() {
        assert_eq!(
            connect_error(banto_plc::PlcError::ConnectTimeout("h:1".into())),
            FatalError::Timeout
        );
        assert!(matches!(
            connect_error(banto_plc::PlcError::Connection("refused".into())),
            FatalError::Io(_)
        ));
    }

    fn n(data_type: DataType) -> TagKind {
        TagKind::numeric(data_type)
    }

    fn spec(text: &str, kind: TagKind) -> TagSpec {
        TagSpec::new(SlmpDriver.parse_address(text, &kind).unwrap(), kind)
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
        SlmpDriver.connect(&settings, cx).await.unwrap()
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
    /// text): a response that never comes is `Timeout`, a closed socket `Io`.
    #[tokio::test]
    async fn fatal_errors_keep_their_type() {
        let sim = Simulator::start().await;
        let specs = [spec("D100", n(DataType::U16))];

        let mut s = connect(&sim, 200).await;
        sim.hang();
        let r = s.read_batch(&specs).await;
        assert!(matches!(r, Err(FatalError::Timeout)), "{r:?}");
        sim.stop_hanging();

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
            ("port", "5007"),
            ("word_order", "high_low"),
            ("cpu_type", "Q"),
        ])
        .unwrap();
        let c = config_from(&s, &cx).unwrap();
        assert_eq!(c.host, "10.0.0.7");
        assert_eq!(c.port, 5007);
        assert_eq!(c.word_order, banto_plc::WordOrder::HighLow);
        assert_eq!(c.cpu, banto_plc::SlmpCpu::Q);
        assert_eq!(c.connect_timeout, Duration::from_millis(1234));
        assert_eq!(c.response_timeout, Duration::from_millis(567));
        // Absent word_order / cpu_type are the column defaults.
        let s = Settings::from_pairs([("host", "h"), ("port", "5000")]).unwrap();
        let c = config_from(&s, &cx).unwrap();
        assert_eq!(c.word_order, banto_plc::WordOrder::LowHigh);
        assert_eq!(c.cpu, banto_plc::SlmpCpu::R);
        for bad in [
            Settings::from_pairs([("port", "5000")]).unwrap(),
            Settings::from_pairs([("host", "h")]).unwrap(),
            Settings::from_pairs([("host", "h"), ("port", "70000")]).unwrap(),
        ] {
            assert!(
                matches!(config_from(&bad, &cx), Err(FatalError::InvalidConfig(_))),
                "{bad:?}"
            );
        }
    }
}
