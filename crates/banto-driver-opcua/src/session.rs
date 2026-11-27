//! The [`SessionDriver`] half: connect with `banto-opcua-client` and run
//! reads and writes on its `ConnectedClient`
//! (docs/chronogazer/opcua-client-design.md §3.3, §3.5-§3.7).
//!
//! ## Connecting
//!
//! `ClientConfig` gets the endpoint URL and the keep-alive settings from the
//! connection's settings, `connect_timeout` / `request_timeout` from the
//! host's [`ConnectContext`], and [`ReconnectPolicy::Disabled`]: the broker
//! reconnects (ADR-0005). SecurityPolicy `None` / Anonymous is the
//! component's default and is not configurable here. With `None` the
//! component creates no key pair (only an encrypted connection needs one),
//! so connecting is cheap. The whole connect (GetEndpoints, the session, the
//! NamespaceArray read) is additionally bounded by `connect_timeout + 2 x
//! response_timeout`, so a host never waits on it unboundedly.
//!
//! ## Writes (#678)
//!
//! A value is sent as the type the **tag** is registered with (`bit` =
//! Boolean, `i16` = Int16, `u16` = UInt16, `i32` = Int32, `u32` = UInt32,
//! `f32` = Float, `i64` = Int64, `u64` = UInt64, `f64` = Double); the session
//! never reads the node first and never learns or guesses its type. A value
//! the tag's type cannot hold (a fraction, out of range or NaN for an integer
//! type) is not sent (`ItemError::Rejected`). A node of another type answers
//! `BadTypeMismatch`, which is that tag's `ItemError::Device` like any other
//! Bad: register the tag with the node's type.
//!
//! The component's results map per node, and `write_batch` is `Ok` even when
//! the component's call is `Err`:
//!
//! | Component result | Driver API |
//! | --- | --- |
//! | `Ok`, node Good (including Good_Clamped and other Good sub-codes) | `Ok(())` |
//! | `Ok`, node `Good_CompletesAsynchronously` (0x002E0000, sub-code compared, info bits ignored) | `ItemError::Indeterminate` (accepted, not confirmed applied) |
//! | `Ok`, node Uncertain | `ItemError::Indeterminate` (reflection not confirmed) |
//! | `Ok`, node Bad (`BadNotWritable`, `BadTypeMismatch`, `BadNodeIdUnknown`, ...) | `ItemError::Device { code }` |
//! | `Err`: `Completed(status)` (a chunk answered before the failure) | as the `Ok` rows |
//! | `Err`: `Rejected(status)` (the server refused the whole request before processing it) | `ItemError::Device { code }` |
//! | `Err`: `NotSent` | `ItemError::Rejected` (not sent, so not written) |
//! | `Err`: `Unknown(status)` (sent, result lost) | `ItemError::Indeterminate` |
//! | `Err`: an outcome added later (`#[non_exhaustive]`) | `ItemError::Indeterminate` |
//! | any `Err` | the session is marked broken: its next `read_batch` / `read_batch_detailed` / `write_batch` sends nothing and returns `Err(FatalError)` |
//!
//! Why: a write the component splits over several requests can break in the
//! middle - earlier chunks answered, the failing chunk maybe written, the
//! rest not sent - and each of those is a fact about its own node, so it is
//! returned per node rather than lost in one batch-wide error. "Maybe
//! written" is its own reason (`Indeterminate`), never a success and never
//! resent automatically; `NotSent` and `Rejected` are certain "not written".
//! Likewise a reply that says the server accepted the write without
//! confirming it reached the target (`Good_CompletesAsynchronously`,
//! OPC UA Part 4 section 5.11.4, or any Uncertain) is `Indeterminate` with
//! the status code in the reason, not a plain success.
//! The component's `Err` always means the request or the connection failed,
//! so the session refuses further calls and the broker reconnects (the
//! `Err` is `Timeout` when a chunk timed out, else the failure's class,
//! normally `Io`).

use banto_driver_api::{
    BoxFuture, ConnectContext, DeviceError, FatalError, ItemError, Reading, Session, SessionDriver,
    Settings, TagKind, TagSpec, Value, WriteItem,
};
use banto_opcua_client::{
    ClientConfig, ConnectedClient, OpcUaClient, ReconnectPolicy, WriteFailure, WriteOutcome,
};
use banto_opcua_core::{NodeAddress, ScalarValue};

use crate::base::{parse_node, ClientSettings, OpcUaDriver};
use crate::convert::{
    fatal_of, fatal_of_connect, reading_of, scalar_for_write, scalar_type_for, status_error,
};

impl SessionDriver for OpcUaDriver {
    fn connect(
        &self,
        settings: &Settings,
        cx: ConnectContext,
    ) -> BoxFuture<'static, Result<Box<dyn Session>, FatalError>> {
        let parsed = ClientSettings::from_settings(settings);
        Box::pin(async move {
            let parsed = parsed.map_err(|errors| {
                FatalError::InvalidConfig(
                    errors
                        .iter()
                        .map(|e| format!("{}: {}", e.key, e.message))
                        .collect::<Vec<_>>()
                        .join("; "),
                )
            })?;
            // The component refuses a zero duration; a host never sends one,
            // but a zero must not turn into "wait forever" either.
            let floor = std::time::Duration::from_millis(1);
            let connect_timeout = cx.connect_timeout.max(floor);
            let request_timeout = cx.response_timeout.max(floor);
            let config = ClientConfig::new(parsed.endpoint_url)
                .with_connect_timeout(connect_timeout)
                .with_request_timeout(request_timeout)
                .with_keep_alive_interval(parsed.keep_alive_interval)
                .with_max_failed_keep_alive_count(parsed.max_failed_keep_alive_count)
                .with_reconnect(ReconnectPolicy::Disabled);
            let bound = connect_timeout + request_timeout * 2;
            let client = match tokio::time::timeout(bound, OpcUaClient::connect_with(config)).await
            {
                Err(_) => return Err(FatalError::Timeout),
                Ok(Err(e)) => return Err(fatal_of_connect(&e)),
                Ok(Ok(client)) => client,
            };
            Ok(Box::new(OpcUaSession {
                client: Some(client),
                broken: None,
            }) as Box<dyn Session>)
        })
    }
}

/// One OPC UA session (one secure channel) - what the host's task owns.
struct OpcUaSession {
    /// `None` after [`Session::disconnect`].
    client: Option<ConnectedClient>,
    /// Set by a Write the component failed (the module doc's table): every
    /// later call returns this without sending, so the host reconnects.
    broken: Option<FatalError>,
}

/// A spec's NodeId, or the tag's own error (an address the driver never
/// produced, a string kind): never connection-fatal (R2b-2a).
fn node_of(spec: &TagSpec) -> Result<NodeAddress, ItemError> {
    if !matches!(spec.kind, TagKind::Numeric(_)) {
        return Err(ItemError::Unsupported(crate::base::STRING_REFUSED.into()));
    }
    parse_node(&spec.address.canonical).map_err(|e| ItemError::Unsupported(e.to_string()))
}

impl OpcUaSession {
    fn client(&self) -> Result<&ConnectedClient, FatalError> {
        if let Some(e) = &self.broken {
            return Err(e.clone());
        }
        self.client
            .as_ref()
            .ok_or_else(|| FatalError::Io("OPC UA の接続は閉じました".into()))
    }

    /// The one read path: `read_batch` is this without the quality.
    async fn read_detailed(
        &mut self,
        specs: &[TagSpec],
    ) -> Result<Vec<Result<Reading, ItemError>>, FatalError> {
        self.client()?;
        let mut results: Vec<Option<Result<Reading, ItemError>>> = vec![None; specs.len()];
        let mut positions = Vec::with_capacity(specs.len());
        let mut nodes = Vec::with_capacity(specs.len());
        for (i, spec) in specs.iter().enumerate() {
            match node_of(spec) {
                Ok(node) => {
                    positions.push(i);
                    nodes.push(node);
                }
                Err(e) => results[i] = Some(Err(e)),
            }
        }
        if !nodes.is_empty() {
            let readings = self
                .client()?
                .read_batch(&nodes)
                .await
                .map_err(|e| fatal_of(&e))?;
            if readings.len() != nodes.len() {
                return Err(FatalError::Protocol(format!(
                    "Read の結果が {} 件の要求に {} 件でした",
                    nodes.len(),
                    readings.len()
                )));
            }
            for (i, reading) in positions.into_iter().zip(&readings) {
                results[i] = Some(reading_of(reading, &specs[i].kind));
            }
        }
        Ok(results
            .into_iter()
            .map(|r| r.expect("every position is filled"))
            .collect())
    }

    async fn write(
        &mut self,
        items: &[WriteItem],
    ) -> Result<Vec<Result<(), ItemError>>, FatalError> {
        self.client()?;
        let mut results: Vec<Option<Result<(), ItemError>>> = vec![None; items.len()];
        let mut sent_positions = Vec::with_capacity(items.len());
        let mut writes: Vec<(NodeAddress, ScalarValue)> = Vec::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            let node = match node_of(&item.spec) {
                Ok(node) => node,
                Err(e) => {
                    results[i] = Some(Err(e));
                    continue;
                }
            };
            let TagKind::Numeric(data_type) = item.spec.kind else {
                unreachable!("node_of refuses non-numeric kinds");
            };
            match scalar_for_write(&item.value, scalar_type_for(data_type)) {
                Ok(value) => {
                    sent_positions.push(i);
                    writes.push((node, value));
                }
                Err(e) => results[i] = Some(Err(e)),
            }
        }
        if !writes.is_empty() {
            let outcomes: Vec<WriteOutcome> = match self.client()?.write_batch(&writes).await {
                Ok(statuses) => statuses.into_iter().map(WriteOutcome::Completed).collect(),
                Err(failure) => {
                    self.broken = Some(broken_by(&failure));
                    failure.outcomes
                }
            };
            if outcomes.len() != writes.len() && self.broken.is_none() {
                self.broken = Some(FatalError::Protocol(format!(
                    "Write の結果が {} 件の要求に {} 件でした",
                    writes.len(),
                    outcomes.len()
                )));
            }
            for (i, result) in sent_positions
                .into_iter()
                .zip(per_node(&outcomes, writes.len()))
            {
                results[i] = Some(result);
            }
        }
        Ok(results
            .into_iter()
            .map(|r| r.expect("every position is filled"))
            .collect())
    }
}

/// `BadTimeout`.
const BAD_TIMEOUT: u32 = 0x800A_0000;

/// The session's state after the component failed a Write: `Timeout` when a
/// chunk timed out, else the failure's class (`Io` for a lost connection).
fn broken_by(failure: &WriteFailure) -> FatalError {
    if failure.outcomes.iter().any(
        |o| matches!(o, WriteOutcome::Unknown(status) if status.code() & 0xFFFF_0000 == BAD_TIMEOUT),
    ) {
        return FatalError::Timeout;
    }
    match fatal_of(&failure.error) {
        FatalError::Io(m) => FatalError::Io(format!(
            "Write の途中で接続が使えなくなりました（つなぎ直します）: {m}"
        )),
        other => other,
    }
}

/// The sent nodes' results, in order, from the component's outcomes. Each
/// node keeps its own outcome; one the component did not return (it promises
/// one per node) is not known to be unwritten.
fn per_node(outcomes: &[WriteOutcome], sent: usize) -> Vec<Result<(), ItemError>> {
    (0..sent)
        .map(|k| match outcomes.get(k) {
            Some(outcome) => item_result(*outcome),
            None => Err(ItemError::Indeterminate(
                "Write の結果が要求より少なく返りました".into(),
            )),
        })
        .collect()
}

/// `Good_CompletesAsynchronously` (0x002E0000): the server accepted the write
/// but has not confirmed it reached the target (Part 4 section 5.11.4).
/// Compared on the upper 16 bits (sub-code); the low info bits are ignored.
const GOOD_COMPLETES_ASYNCHRONOUSLY: u32 = 0x002E_0000;

/// A Good / Uncertain write status that does not confirm the write was
/// applied, as an `Indeterminate` reason; `None` when it is a plain success.
fn unconfirmed(status: banto_opcua_core::StatusCode) -> Option<ItemError> {
    if status.is_good() && status.code() & 0xFFFF_0000 == GOOD_COMPLETES_ASYNCHRONOUSLY {
        Some(ItemError::Indeterminate(format!(
            "サーバーは受け付けましたが、書き込み先への反映を確かめていません（Good_CompletesAsynchronously 0x{GOOD_COMPLETES_ASYNCHRONOUSLY:08X}）"
        )))
    } else if status.is_uncertain() {
        Some(ItemError::Indeterminate(format!(
            "書き込みの結果が不確かです（{status}、0x{:08X}）",
            status.code()
        )))
    } else {
        None
    }
}

/// One node's outcome as the item's result (the module doc's table).
fn item_result(outcome: WriteOutcome) -> Result<(), ItemError> {
    match outcome {
        WriteOutcome::Completed(status) | WriteOutcome::Rejected(status)
            if status.is_bad() || !(status.is_good() || status.is_uncertain()) =>
        {
            Err(match status_error(status) {
                ItemError::Unsupported(_) => {
                    ItemError::Device(DeviceError::new(status.code(), status.to_string()))
                }
                other => other,
            })
        }
        WriteOutcome::Completed(status) | WriteOutcome::Rejected(status)
            if unconfirmed(status).is_some() =>
        {
            Err(unconfirmed(status).expect("checked by the guard"))
        }
        WriteOutcome::Rejected(status) => Err(ItemError::Device(DeviceError::new(
            status.code(),
            format!("サーバーが Write の要求を拒否しました（{status}）"),
        ))),
        WriteOutcome::Completed(_) => Ok(()),
        WriteOutcome::NotSent => Err(ItemError::Rejected(
            "接続が切れたので送っていません（書かれていません）".into(),
        )),
        WriteOutcome::Unknown(status) => Err(ItemError::Indeterminate(format!(
            "送りましたが結果を受け取れませんでした（{status}）。書かれたかもしれません"
        ))),
        // `#[non_exhaustive]`: an outcome added later is not known to be
        // unwritten, so it is neither a success nor "not written".
        _ => Err(ItemError::Indeterminate(format!(
            "Write の結末が分かりません: {outcome:?}"
        ))),
    }
}

impl Session for OpcUaSession {
    fn read_batch<'a>(
        &'a mut self,
        specs: &'a [TagSpec],
    ) -> BoxFuture<'a, Result<Vec<Result<Value, ItemError>>, FatalError>> {
        Box::pin(async move {
            Ok(self
                .read_detailed(specs)
                .await?
                .into_iter()
                .map(|r| r.map(|reading| reading.value))
                .collect())
        })
    }

    fn read_batch_detailed<'a>(
        &'a mut self,
        specs: &'a [TagSpec],
    ) -> BoxFuture<'a, Result<Vec<Result<Reading, ItemError>>, FatalError>> {
        Box::pin(self.read_detailed(specs))
    }

    fn write_batch<'a>(
        &'a mut self,
        items: &'a [WriteItem],
    ) -> BoxFuture<'a, Result<Vec<Result<(), ItemError>>, FatalError>> {
        Box::pin(self.write(items))
    }

    fn disconnect(&mut self) -> BoxFuture<'_, ()> {
        let client = self.client.take();
        Box::pin(async move {
            if let Some(client) = client {
                // A failed close is not the host's concern: the session ends
                // either way (the component stops its event loop).
                let _ = client.disconnect().await;
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use banto_opcua_core::StatusCode;

    use super::*;

    #[test]
    fn write_outcomes_map_per_item() {
        assert_eq!(
            item_result(WriteOutcome::Completed(StatusCode::GOOD)),
            Ok(())
        );
        // Good with other sub-codes / info bits is still a success.
        for code in [0x0000_0000, 0x0000_0400, 0x0030_0000, 0x0030_0400] {
            assert_eq!(
                item_result(WriteOutcome::Completed(StatusCode::from_code(code))),
                Ok(()),
                "{code:#010x}"
            );
        }
        // Accepted but not confirmed applied, or Uncertain: Indeterminate.
        for code in [0x002E_0000, 0x002E_0400, 0x4090_0000, 0x4000_0000] {
            assert!(
                matches!(
                    item_result(WriteOutcome::Completed(StatusCode::from_code(code))),
                    Err(ItemError::Indeterminate(_))
                ),
                "{code:#010x}"
            );
        }
        assert!(matches!(
            item_result(WriteOutcome::Completed(StatusCode::from_code(0x002E_0400))),
            Err(ItemError::Indeterminate(m)) if m.contains("Good_CompletesAsynchronously 0x002E0000")
        ));
        assert_eq!(
            item_result(WriteOutcome::Completed(StatusCode::BAD_NOT_WRITABLE)),
            Err(ItemError::Device(DeviceError::new(
                0x803B_0000,
                "BadNotWritable"
            )))
        );
        assert!(matches!(
            item_result(WriteOutcome::Completed(StatusCode::BAD_NOT_SUPPORTED)),
            Err(ItemError::Device(e)) if e.code == 0x803D_0000
        ));
        assert!(matches!(
            item_result(WriteOutcome::Rejected(StatusCode::BAD_TOO_MANY_OPERATIONS)),
            Err(ItemError::Device(e)) if e.code == 0x8010_0000
        ));
        assert!(matches!(
            item_result(WriteOutcome::Completed(StatusCode::BAD_TYPE_MISMATCH)),
            Err(ItemError::Device(e)) if e.code == 0x8074_0000
        ));
        assert!(matches!(
            item_result(WriteOutcome::NotSent),
            Err(ItemError::Rejected(_))
        ));
        // Sent, result lost: maybe written - never a success, never "not written".
        for status in [
            StatusCode::BAD_CONNECTION_CLOSED,
            StatusCode::from_code(BAD_TIMEOUT),
        ] {
            assert!(matches!(
                item_result(WriteOutcome::Unknown(status)),
                Err(ItemError::Indeterminate(_))
            ));
        }
    }

    #[test]
    fn a_split_write_broken_in_the_middle_keeps_each_nodes_outcome() {
        // Chunk 1 answered (Good, BadNotWritable), chunk 2 lost, chunk 3 not sent.
        let outcomes = [
            WriteOutcome::Completed(StatusCode::GOOD),
            WriteOutcome::Completed(StatusCode::BAD_NOT_WRITABLE),
            WriteOutcome::Unknown(StatusCode::BAD_CONNECTION_CLOSED),
            WriteOutcome::Unknown(StatusCode::BAD_CONNECTION_CLOSED),
            WriteOutcome::NotSent,
        ];
        let got = per_node(&outcomes, outcomes.len());
        assert_eq!(got[0], Ok(()));
        assert!(matches!(&got[1], Err(ItemError::Device(e)) if e.code == 0x803B_0000));
        assert!(matches!(got[2], Err(ItemError::Indeterminate(_))));
        assert!(matches!(got[3], Err(ItemError::Indeterminate(_))));
        assert!(matches!(got[4], Err(ItemError::Rejected(_))));
        // A whole-request refusal is certain "not written", per node.
        let got = per_node(
            &[WriteOutcome::Rejected(StatusCode::BAD_TOO_MANY_OPERATIONS)],
            1,
        );
        assert!(matches!(&got[0], Err(ItemError::Device(e)) if e.code == 0x8010_0000));
        // An outcome the component failed to return is not known to be unwritten.
        let got = per_node(&outcomes[..1], 2);
        assert_eq!(got[0], Ok(()));
        assert!(matches!(got[1], Err(ItemError::Indeterminate(_))));
    }

    #[test]
    fn a_split_write_keeps_async_unknown_and_not_sent_apart() {
        let outcomes = [
            WriteOutcome::Completed(StatusCode::from_code(0x002E_0000)),
            WriteOutcome::Unknown(StatusCode::BAD_CONNECTION_CLOSED),
            WriteOutcome::NotSent,
        ];
        let got = per_node(&outcomes, 3);
        assert!(matches!(&got[0], Err(ItemError::Indeterminate(m)) if m.contains("0x002E0000")));
        assert!(matches!(got[1], Err(ItemError::Indeterminate(_))));
        assert!(matches!(got[2], Err(ItemError::Rejected(_))));
    }

    fn spec(data_type: banto_driver_api::DataType) -> TagSpec {
        use banto_driver_api::DriverBase;
        let kind = TagKind::numeric(data_type);
        TagSpec::new(
            OpcUaDriver.parse_address("ns=2;s=Level", &kind).unwrap(),
            kind,
        )
    }

    #[tokio::test]
    async fn a_broken_session_refuses_every_call_without_sending() {
        let mut s = OpcUaSession {
            client: None,
            broken: Some(FatalError::Timeout),
        };
        let specs = [spec(banto_driver_api::DataType::I16)];
        assert_eq!(s.read_batch(&specs).await, Err(FatalError::Timeout));
        assert_eq!(
            s.read_batch_detailed(&specs).await.map(|_| ()),
            Err(FatalError::Timeout)
        );
        let items = [WriteItem::new(specs[0].clone(), Value::F64(1.0))];
        assert_eq!(s.write_batch(&items).await, Err(FatalError::Timeout));
        // Even an empty batch (nothing to send) is refused.
        assert_eq!(s.write_batch(&[]).await, Err(FatalError::Timeout));
        assert_eq!(s.read_batch(&[]).await, Err(FatalError::Timeout));
    }
}
