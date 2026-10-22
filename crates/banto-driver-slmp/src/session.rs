//! The SLMP [`banto_broker::BrokerSession`] implementation (moved here from
//! `banto-broker`'s `src/slmp_driver.rs` by R1-4a,
//! docs/banto-driver-api-design.md §4.4 - unchanged). See `banto-broker`'s
//! module doc ("Protocol abstraction (I9 / Issue #130)") for why the broker
//! is organized as a generic job loop/reconnect state machine plus a
//! per-protocol driver like this one, and `banto-broker`'s `session.rs`
//! module doc for the trait contract this module must uphold.
//!
//! [`SlmpSession`] is a thin adapter: it owns exactly the bare
//! `slmp::SLMPClient` the broker's `ConnState::Connected` held directly before
//! #130, plus the `word_order` that client's decode/encode side
//! needs, and forwards `read_batch`/`write_batch` to the same
//! `banto_plc::plan_slmp_batch` / `execute_slmp_batch_reads` and
//! `banto_plc_write::plan_slmp_write_batch` / `execute_slmp_writes` calls
//! the broker's job loop used to make directly - moved here,
//! not changed, so this extraction is behavior-preserving by construction.

use banto_plc::{
    dial_slmp, execute_slmp_batch_reads, plan_slmp_batch, BatchReadRequest, BatchReadResult,
    BoxFuture, PlcError, SlmpConfig, WordOrder,
};
use banto_plc_write::{
    execute_slmp_writes, plan_slmp_write_batch, BatchWriteRequest, PlcWriteError, WriteResult,
};

use banto_broker::{BrokerSession, Connector, SessionError};

/// One live SLMP session: a bare `slmp::SLMPClient` plus the `word_order`
/// its 32-bit numeric decode/encode needs (`word_order` does not affect
/// string decoding - see `execute_slmp_batch_reads`'s own doc comment).
/// Not boxed internally (unlike the pre-#130 `ConnState::Connected(Box<
/// slmp::SLMPClient>)`): `Box<dyn BrokerSession>` already puts this whole
/// struct - large receive buffer included - on the heap, so a second,
/// SLMP-specific `Box` here would be redundant. See `banto-broker`'s
/// `ConnState` doc comment.
pub(crate) struct SlmpSession {
    client: slmp::SLMPClient,
    word_order: WordOrder,
}

impl SlmpSession {
    /// A session over an already-dialed `client`.
    pub(crate) fn new(client: slmp::SLMPClient, word_order: WordOrder) -> Self {
        SlmpSession { client, word_order }
    }

    /// The read half of [`BrokerSession::read_batch`] with the **typed**
    /// error (R2b-3a-1): the `banto-driver-api` session maps it with
    /// `classify_read` instead of recovering a variant from text. The broker's
    /// `BrokerSession` impl below calls this too, so there is one body.
    pub(crate) async fn read_typed(
        &mut self,
        requests: &[BatchReadRequest],
    ) -> Result<Vec<BatchReadResult>, PlcError> {
        let plan = plan_slmp_batch(requests);
        execute_slmp_batch_reads(&mut self.client, &plan, requests.len(), self.word_order).await
    }

    /// The write half, typed error (see [`Self::read_typed`]).
    pub(crate) async fn write_typed(
        &mut self,
        requests: &[BatchWriteRequest],
    ) -> Result<Vec<WriteResult>, PlcWriteError> {
        let plan = plan_slmp_write_batch(requests, self.word_order);
        execute_slmp_writes(&mut self.client, &plan, requests.len()).await
    }

    pub(crate) async fn close(&mut self) {
        self.client.close().await
    }
}

impl BrokerSession for SlmpSession {
    fn read_batch<'a>(
        &'a mut self,
        requests: &'a [BatchReadRequest],
    ) -> BoxFuture<'a, Result<Vec<BatchReadResult>, SessionError>> {
        Box::pin(async move {
            self.read_typed(requests)
                .await
                .map_err(|e| SessionError(e.to_string()))
        })
    }

    fn write_batch<'a>(
        &'a mut self,
        requests: &'a [BatchWriteRequest],
    ) -> BoxFuture<'a, Result<Vec<WriteResult>, SessionError>> {
        Box::pin(async move {
            self.write_typed(requests)
                .await
                .map_err(|e| SessionError(e.to_string()))
        })
    }

    fn disconnect(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(self.close())
    }
}

/// Build this connection's [`Connector`] from its [`SlmpConfig`] (see
/// [`crate::slmp_config_for`]) - what [`crate::dial`] (this driver's
/// `banto_broker::DriverTable` entry) hands the broker, which then runs it in
/// the connection's broker task.
///
/// Dials through [`banto_plc::dial_slmp`], the one shared implementation of
/// the connect sequence (build a bare `slmp::SLMPClient` from
/// `SlmpConfig::to_wire_props()`, wire the two per-crate timeouts, wrap the
/// connect in `SlmpConfig::connect_timeout`, map the structured
/// `slmp::SlmpError` - H9, docs/h9-slmp-structured-error-spec.md) that
/// `banto_plc::slmp::SlmpClient::connect` and
/// `banto_plc_write::slmp::SlmpWriteClient::connect` also call (H9 transport
/// 共通化, docs/improvement-plan.md §H9) - exactly what the pre-#130
/// `connect_attempt` did inline. What still differs here, and is not
/// shareable, is what happens to the client *after* the dial: this broker
/// wraps it in [`SlmpSession`] rather than handing it to `SlmpClient`'s or
/// `SlmpWriteClient`'s own private `Option<slmp::SLMPClient>`, so `read_batch`
/// and `write_batch` can borrow the *same* session - see `banto-broker`'s
/// `session.rs` module doc, "Why one trait with both `read_batch` and `write_batch`".
///
/// `config` is captured by the returned closure (called again on every
/// reconnect attempt) and cloned into each individual connect attempt's
/// `'static` future - mirrors the pre-#130 `tokio::spawn(connect_attempt(
/// config.clone()))` call site, just moved one layer down.
///
/// `#[doc(hidden)] pub` (R1-5) only so that this crate's `tests/conformance.rs`
/// can build a session with the conformance harness's timeouts; not part of
/// the crate's API.
#[doc(hidden)]
pub fn connector(config: SlmpConfig) -> Connector {
    std::sync::Arc::new(move || {
        let config = config.clone();
        Box::pin(async move {
            let word_order = config.word_order;
            let client = dial_slmp(&config).await.map_err(|e| e.to_string())?;
            Ok(Box::new(SlmpSession::new(client, word_order)) as Box<dyn BrokerSession>)
        })
    })
}
