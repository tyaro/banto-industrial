//! The SLMP [`crate::session::BrokerSession`] implementation - the only
//! driver [`crate::DRIVERS`] registers today. See `lib.rs`'s module doc
//! ("Protocol abstraction (I9 / Issue #130)") for why this crate is
//! organized as a generic job loop/reconnect state machine plus a
//! per-protocol driver module like this one, and `session.rs`'s module doc
//! for the trait contract this module must uphold.
//!
//! [`SlmpSession`] is a thin adapter: it owns exactly the bare
//! `slmp::SLMPClient` [`crate::ConnState::Connected`] held directly before
//! this refactor, plus the `word_order` that client's decode/encode side
//! needs, and forwards `read_batch`/`write_batch` to the same
//! `banto_plc::plan_slmp_batch` / `execute_slmp_batch_reads` and
//! `banto_plc_write::plan_slmp_write_batch` / `execute_slmp_writes` calls
//! [`crate::run_broker_task`]'s job loop used to make directly - moved here,
//! not changed, so this extraction is behavior-preserving by construction.

use banto_plc::{
    dial_slmp, execute_slmp_batch_reads, plan_slmp_batch, BatchReadRequest, BatchReadResult,
    BoxFuture, SlmpConfig, WordOrder,
};
use banto_plc_write::{execute_slmp_writes, plan_slmp_write_batch, BatchWriteRequest, WriteResult};

use crate::session::{BrokerSession, Connector, SessionError};

/// One live SLMP session: a bare `slmp::SLMPClient` plus the `word_order`
/// its 32-bit numeric decode/encode needs (`word_order` does not affect
/// string decoding - see `execute_slmp_batch_reads`'s own doc comment).
/// Not boxed internally (unlike the pre-#130 `ConnState::Connected(Box<
/// slmp::SLMPClient>)`): `Box<dyn BrokerSession>` already puts this whole
/// struct - large receive buffer included - on the heap, so a second,
/// SLMP-specific `Box` here would be redundant. See `lib.rs`'s `ConnState`
/// doc comment.
pub(crate) struct SlmpSession {
    client: slmp::SLMPClient,
    word_order: WordOrder,
}

impl BrokerSession for SlmpSession {
    fn read_batch<'a>(
        &'a mut self,
        requests: &'a [BatchReadRequest],
    ) -> BoxFuture<'a, Result<Vec<BatchReadResult>, SessionError>> {
        Box::pin(async move {
            let plan = plan_slmp_batch(requests);
            execute_slmp_batch_reads(&mut self.client, &plan, requests.len(), self.word_order)
                .await
                .map_err(|e| SessionError(e.to_string()))
        })
    }

    fn write_batch<'a>(
        &'a mut self,
        requests: &'a [BatchWriteRequest],
    ) -> BoxFuture<'a, Result<Vec<WriteResult>, SessionError>> {
        Box::pin(async move {
            let plan = plan_slmp_write_batch(requests, self.word_order);
            execute_slmp_writes(&mut self.client, &plan, requests.len())
                .await
                .map_err(|e| SessionError(e.to_string()))
        })
    }

    fn disconnect(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async move { self.client.close().await })
    }
}

/// Build this connection's [`Connector`] from its [`SlmpConfig`] (see
/// [`crate::slmp_config_for`]) - the SLMP entry [`crate::DRIVERS`] dispatches
/// to via [`crate::spawn_task`].
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
/// and `write_batch` can borrow the *same* session - see `session.rs`'s
/// module doc, "Why one trait with both `read_batch` and `write_batch`".
///
/// `config` is captured by the returned closure (called again on every
/// reconnect attempt) and cloned into each individual connect attempt's
/// `'static` future - mirrors the pre-#130 `tokio::spawn(connect_attempt(
/// config.clone()))` call site, just moved one layer down.
pub(crate) fn connector(config: SlmpConfig) -> Connector {
    std::sync::Arc::new(move || {
        let config = config.clone();
        Box::pin(async move {
            let word_order = config.word_order;
            let client = dial_slmp(&config).await.map_err(|e| e.to_string())?;
            Ok(Box::new(SlmpSession { client, word_order }) as Box<dyn BrokerSession>)
        })
    })
}
