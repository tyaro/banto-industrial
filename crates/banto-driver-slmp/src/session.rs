//! [`SlmpSession`]: the one live SLMP client of a connection (moved here
//! from `banto-broker`'s `src/slmp_driver.rs` by R1-4a). It forwards to the
//! same `banto_plc::plan_slmp_batch` / `execute_slmp_batch_reads` and
//! `banto_plc_write::plan_slmp_write_batch` / `execute_slmp_writes` calls the
//! broker's job loop used to make directly. Since R2b-3a-2 the broker no
//! longer calls it directly: the driver API session in `api_session.rs` owns
//! one and calls [`SlmpSession::read_typed`] / [`SlmpSession::write_typed`]
//! (the former `banto_broker::BrokerSession` impl and the `connector` that
//! built it are gone, docs/banto-hub-driver-io-design.md §4.2).

use banto_plc::{
    execute_slmp_batch_reads, plan_slmp_batch, BatchReadRequest, BatchReadResult, PlcError,
    WordOrder,
};
use banto_plc_write::{
    execute_slmp_writes, plan_slmp_write_batch, BatchWriteRequest, PlcWriteError, WriteResult,
};

/// One live SLMP session: a bare `slmp::SLMPClient` plus the `word_order`
/// its 32-bit numeric decode/encode needs (`word_order` does not affect
/// string decoding - see `execute_slmp_batch_reads`'s own doc comment).
/// Not boxed internally: `Box<dyn Session>` already puts the API session
/// around it - large receive buffer included - on the heap.
pub(crate) struct SlmpSession {
    client: slmp::SLMPClient,
    word_order: WordOrder,
}

impl SlmpSession {
    /// A session over an already-dialed `client`.
    pub(crate) fn new(client: slmp::SLMPClient, word_order: WordOrder) -> Self {
        SlmpSession { client, word_order }
    }

    /// Plan and execute one read batch, with the **typed** error (R2b-3a-1):
    /// the `banto-driver-api` session maps it with `classify_read` instead of
    /// recovering a variant from text.
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
