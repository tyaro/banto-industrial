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
//! ## Writes
//!
//! The component sends a value as the type it is given (it never converts),
//! so the session learns each node's built-in type from a Read - every Read
//! with a value records it, and a Write reads the nodes it has not seen yet
//! first. A node whose value cannot be read is written as the tag kind's own
//! type (`bit` = Boolean, `i16` = Int16, ..., `f64` = Double); a
//! `BadTypeMismatch` forgets the learned type. The component's results map as:
//!
//! | Component result | Driver API |
//! | --- | --- |
//! | `Ok`, node Good / Uncertain | `Ok(())` |
//! | `Ok`, node Bad (`BadNotWritable`, `BadTypeMismatch`, `BadNodeIdUnknown`, ...) | `ItemError::Device { code }` |
//! | `Err`, every outcome known: `Completed(status)` | as the `Ok` rows |
//! | `Err`, every outcome known: `Rejected(status)` (the server refused the whole request before processing it) | `ItemError::Device { code }` |
//! | `Err`, every outcome known: `NotSent` | `ItemError::Rejected` ("not sent") |
//! | `Err` with any `Unknown` outcome (sent, result lost: it may have been written) | `Err(FatalError)` for the whole batch |
//! | the type-learning Read fails | `Err(FatalError)` (nothing was sent) |
//!
//! Why: `NotSent` and `Rejected` are certain "not written", so they stay
//! per-item and the caller may retry them; `Completed` carries the server's
//! own answer. `Unknown` cannot be expressed per item - the Driver API's
//! `ItemError` has no "maybe written" case, and claiming either success or
//! failure would be a lie - so the batch is connection-fatal, which is also
//! what it is (the request's response was lost). The component *does*
//! distinguish a split Write that broke in the middle (earlier chunks
//! `Completed`, the failing chunk `Unknown` / `NotSent` / `Rejected`, the rest
//! `NotSent`), but once one chunk is `Unknown` that detail is lost here.
//! When an `Err` without `Unknown` leaves the connection itself broken, the
//! next read returns `Err` and the host reconnects then.

use std::collections::HashMap;

use banto_driver_api::{
    BoxFuture, ConnectContext, DeviceError, FatalError, ItemError, Reading, Session, SessionDriver,
    Settings, TagKind, TagSpec, Value, WriteItem,
};
use banto_opcua_client::{
    ClientConfig, ConnectedClient, OpcUaClient, ReconnectPolicy, WriteFailure, WriteOutcome,
};
use banto_opcua_core::{NodeAddress, ScalarType, ScalarValue, StatusCode};

use crate::base::{parse_node, ClientSettings, OpcUaDriver};
use crate::convert::{
    fatal_of, fatal_of_connect, reading_of, scalar_for_write, scalar_type_for, status_error,
};

/// `BadTypeMismatch` (the learned type was wrong: forget it).
const BAD_TYPE_MISMATCH: StatusCode = StatusCode::BAD_TYPE_MISMATCH;

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
                types: HashMap::new(),
            }) as Box<dyn Session>)
        })
    }
}

/// One OPC UA session (one secure channel) - what the host's task owns.
struct OpcUaSession {
    /// `None` after [`Session::disconnect`].
    client: Option<ConnectedClient>,
    /// Each node's built-in type, by canonical NodeId, as last read.
    types: HashMap<String, ScalarType>,
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
        self.client
            .as_ref()
            .ok_or_else(|| FatalError::Io("OPC UA の接続は閉じました".into()))
    }

    /// The one read path: `read_batch` is this without the quality.
    async fn read_detailed(
        &mut self,
        specs: &[TagSpec],
    ) -> Result<Vec<Result<Reading, ItemError>>, FatalError> {
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
            for ((i, node), reading) in positions.into_iter().zip(&nodes).zip(&readings) {
                if let Some(value) = &reading.value {
                    self.types.insert(node.to_string(), value.scalar_type());
                }
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
        let mut results: Vec<Option<Result<(), ItemError>>> = vec![None; items.len()];
        let mut parsed = Vec::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            match node_of(&item.spec) {
                Ok(node) => parsed.push((i, node)),
                Err(e) => results[i] = Some(Err(e)),
            }
        }
        // Learn the types not seen yet (nothing is written before this).
        let mut unknown: Vec<NodeAddress> = Vec::new();
        for (_, node) in &parsed {
            if !self.types.contains_key(&node.to_string()) && !unknown.contains(node) {
                unknown.push(node.clone());
            }
        }
        if !unknown.is_empty() {
            let readings = self
                .client()?
                .read_batch(&unknown)
                .await
                .map_err(|e| fatal_of(&e))?;
            for (node, reading) in unknown.iter().zip(&readings) {
                if let Some(value) = &reading.value {
                    self.types.insert(node.to_string(), value.scalar_type());
                }
            }
        }
        let mut sent_positions = Vec::with_capacity(parsed.len());
        let mut writes: Vec<(NodeAddress, ScalarValue)> = Vec::with_capacity(parsed.len());
        for (i, node) in parsed {
            let TagKind::Numeric(data_type) = items[i].spec.kind else {
                unreachable!("node_of refuses non-numeric kinds");
            };
            let target = self
                .types
                .get(&node.to_string())
                .copied()
                .unwrap_or_else(|| scalar_type_for(data_type));
            match scalar_for_write(&items[i].value, target) {
                Ok(value) => {
                    sent_positions.push(i);
                    writes.push((node, value));
                }
                Err(e) => results[i] = Some(Err(e)),
            }
        }
        if !writes.is_empty() {
            let outcomes = match self.client()?.write_batch(&writes).await {
                Ok(statuses) => statuses.into_iter().map(WriteOutcome::Completed).collect(),
                Err(failure) => self.known_outcomes(failure)?,
            };
            for ((i, (node, _)), outcome) in sent_positions.into_iter().zip(&writes).zip(outcomes) {
                if outcome == WriteOutcome::Completed(BAD_TYPE_MISMATCH) {
                    self.types.remove(&node.to_string());
                }
                results[i] = Some(item_result(outcome));
            }
        }
        Ok(results
            .into_iter()
            .map(|r| r.expect("every position is filled"))
            .collect())
    }

    /// A failed batch's outcomes when each is certain; any `Unknown` makes
    /// the batch connection-fatal (the module doc's table).
    fn known_outcomes(&self, failure: WriteFailure) -> Result<Vec<WriteOutcome>, FatalError> {
        if failure
            .outcomes
            .iter()
            .any(|o| matches!(o, WriteOutcome::Unknown(_)))
        {
            return Err(match fatal_of(&failure.error) {
                // A lost write result is never a plain "framing" problem to
                // the host; keep the class but say it may have been written.
                FatalError::Timeout => FatalError::Timeout,
                other => FatalError::Io(format!(
                    "Write の結果を受け取れませんでした（書かれたかもしれません）: {other}"
                )),
            });
        }
        Ok(failure.outcomes)
    }
}

/// One node's certain outcome as the item's result.
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
        WriteOutcome::Rejected(status) => Err(ItemError::Device(DeviceError::new(
            status.code(),
            format!("サーバーが Write の要求を拒否しました（{status}）"),
        ))),
        WriteOutcome::Completed(_) => Ok(()),
        WriteOutcome::NotSent => Err(ItemError::Rejected(
            "接続が切れたので送っていません（書かれていません）".into(),
        )),
        // `#[non_exhaustive]`: an outcome added later is not known to be
        // unwritten, so it is not a success either.
        _ => Err(ItemError::Rejected(format!(
            "Write の結末が不明です: {outcome:?}"
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
    use super::*;

    #[test]
    fn write_outcomes_map_per_item() {
        assert_eq!(
            item_result(WriteOutcome::Completed(StatusCode::GOOD)),
            Ok(())
        );
        assert_eq!(
            item_result(WriteOutcome::Completed(StatusCode::from_code(0x4090_0000))),
            Ok(())
        );
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
            item_result(WriteOutcome::NotSent),
            Err(ItemError::Rejected(_))
        ));
    }
}
