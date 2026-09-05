//! The Modbus TCP [`crate::session::BrokerSession`] implementation - the
//! second driver [`crate::DRIVERS`] registers (#131, 2026-09-01). See
//! `lib.rs`'s module doc ("Protocol abstraction (I9 / Issue #130)") for why
//! this crate is organized as a generic job loop/reconnect state machine plus
//! a per-protocol driver module like this one, and `session.rs`'s module doc
//! for the trait contract this module must uphold - especially the `Err` =
//! connection-fatal rule, which (unlike [`crate::slmp_driver::SlmpSession`])
//! this driver does **not** get for free: `banto_plc::execute_modbus_reads`
//! and `banto_plc_write::execute_modbus_writes` already fold a Modbus
//! exception response into a per-request `Bad`, but the manual `String`/
//! `BitInWord` splitting [`ModbusSession::write_batch`] does below is new
//! code this driver is responsible for getting right.
//!
//! [`ModbusSession`] mirrors [`crate::slmp_driver::SlmpSession`]'s shape: it
//! owns the one live `TcpStream` [`crate::ConnState::Connected`] holds for a
//! Modbus connection, plus everything `banto_plc::execute_modbus_reads` and
//! `banto_plc_write::execute_modbus_writes` need between calls. See
//! [`ModbusSession::next_transaction_id`]'s own doc comment for the single
//! most safety-critical property in this file.

use std::time::Duration;

use banto_plc::{
    execute_modbus_reads, plan_batch_requests, BatchReadRequest, BatchReadResult, BoxFuture,
    ModbusTcpConfig, PlcValue, WordOrder,
};
use banto_plc_write::{
    execute_modbus_writes, plan_modbus_writes, BatchWriteRequest, PlcWriteError, WriteRequest,
    WriteResult,
};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

use crate::session::{BrokerSession, Connector, SessionError};

/// One live Modbus TCP session: a bare `TcpStream` plus everything
/// `banto_plc::execute_modbus_reads`/`banto_plc_write::execute_modbus_writes`
/// need between calls. Not boxed internally, for the same reason
/// [`crate::slmp_driver::SlmpSession`] is not: `Box<dyn BrokerSession>`
/// already puts this whole struct on the heap.
pub(crate) struct ModbusSession {
    stream: TcpStream,
    unit_id: u8,
    response_timeout: Duration,
    word_order: WordOrder,
    /// The **one** transaction-id counter shared by both
    /// [`Self::read_batch`] and [`Self::write_batch`] - this is the single
    /// most safety-critical thing in this whole file, so read this doc
    /// comment in full before touching either method.
    ///
    /// `banto_plc::modbus::ModbusTcpClient::next_transaction_id`'s own doc
    /// comment gives the safety argument for why wraparound collisions can be
    /// ignored: "this client only ever has one request in flight at a time
    /// ... a stale in-flight response from transaction id `N` colliding with
    /// a *new* request that reused `N` after wrapping ... requires 65,536
    /// outstanding requests". That argument depends entirely on there being
    /// exactly **one** counter per live TCP session, counting every request
    /// (read or write alike) sent on that socket. If `read_batch` and
    /// `write_batch` each kept their own counter, both starting at 0, the
    /// two would independently reuse the same low tid values almost
    /// immediately (tid 0, then 1, then 2, ... on *each* side): a late/stale
    /// response to a *timed-out read* (tid 3, say) could then be misread as
    /// the response to an *immediately-following write*'s transaction id
    /// (also tid 3), or vice versa - exactly the collision the single-counter
    /// argument was supposed to rule out, reintroduced by splitting the
    /// counter instead of the request rate.
    ///
    /// [`ModbusSession`] is the single owner of the one physical `TcpStream`
    /// for this connection's whole lifetime (this crate's core invariant -
    /// see `lib.rs`'s "Message shape and how serialization is guaranteed"
    /// section: one `Box<dyn BrokerSession>` per connection, and read/write
    /// jobs never run concurrently against it), so keeping exactly one
    /// `next_transaction_id` field here - threaded by `&mut` into both
    /// `execute_modbus_reads` and `execute_modbus_writes` on every call - is
    /// what preserves the single-counter argument for this shared session.
    /// `modbus_driver_shares_one_transaction_id_counter_across_reads_and_writes`
    /// in this module's own test module pins this down directly against the
    /// private field.
    next_transaction_id: u16,
}

impl BrokerSession for ModbusSession {
    fn read_batch<'a>(
        &'a mut self,
        requests: &'a [BatchReadRequest],
    ) -> BoxFuture<'a, Result<Vec<BatchReadResult>, SessionError>> {
        Box::pin(async move {
            // `plan_batch_requests` (not `plan_requests`) already resolves
            // `BatchReadRequest::String` into `immediate_bad` with indices
            // remapped to the full batch, so the resulting `PlanOutcome` is
            // sized to `requests.len()` for free - no manual splitting
            // needed, unlike `write_batch` below (which has no such
            // batch-aware helper on the write side).
            let outcome = plan_batch_requests(requests);
            let results = execute_modbus_reads(
                &mut self.stream,
                self.unit_id,
                self.response_timeout,
                &mut self.next_transaction_id,
                &outcome,
                requests.len(),
                self.word_order,
            )
            .await
            .map_err(|e| SessionError(e.to_string()))?;

            Ok(results
                .into_iter()
                .map(|r| match r {
                    banto_plc::ReadResult::Value(v) => BatchReadResult::Value(PlcValue::from(v)),
                    banto_plc::ReadResult::Bad(e) => BatchReadResult::Bad(e),
                })
                .collect())
        })
    }

    fn write_batch<'a>(
        &'a mut self,
        requests: &'a [BatchWriteRequest],
    ) -> BoxFuture<'a, Result<Vec<WriteResult>, SessionError>> {
        Box::pin(async move {
            // `banto_plc_write` has no `plan_batch`-style helper for writes
            // (only `plan_modbus_writes(&[WriteRequest], WordOrder)`,
            // numeric-only by the front-half's explicit design - see
            // `banto_plc_write::modbus::planning`'s module doc, "What is
            // deliberately out of scope"), so this driver does the
            // Numeric/String/BitInWord split by hand.
            let mut results: Vec<Option<WriteResult>> = vec![None; requests.len()];
            let mut numeric: Vec<WriteRequest> = Vec::new();
            let mut numeric_to_original: Vec<usize> = Vec::new();

            for (index, req) in requests.iter().enumerate() {
                match req {
                    BatchWriteRequest::Numeric(r) => {
                        numeric.push(*r);
                        numeric_to_original.push(index);
                    }
                    BatchWriteRequest::String(_) => {
                        results[index] =
                            Some(WriteResult::Bad(PlcWriteError::UnsupportedRequestKind {
                                kind: "文字列書き込み".to_string(),
                            }));
                    }
                    BatchWriteRequest::BitInWord { .. } => {
                        results[index] =
                            Some(WriteResult::Bad(PlcWriteError::UnsupportedRequestKind {
                                kind: "ビット単体(BitInWord)書き込み".to_string(),
                            }));
                    }
                }
            }

            let outcome = plan_modbus_writes(&numeric, self.word_order);
            // `execute_modbus_writes` returns a `Vec<WriteResult>` indexed by
            // `numeric`'s own positions (0..numeric.len()), not remapped to
            // the mixed batch - scatter each entry back via
            // `numeric_to_original` rather than remapping `outcome`'s
            // indices up front, which is simpler to get right here.
            let numeric_results = execute_modbus_writes(
                &mut self.stream,
                self.unit_id,
                self.response_timeout,
                &mut self.next_transaction_id,
                &outcome,
                numeric.len(),
            )
            .await
            .map_err(|e| SessionError(e.to_string()))?;

            for (numeric_index, result) in numeric_results.into_iter().enumerate() {
                let original_index = numeric_to_original[numeric_index];
                results[original_index] = Some(result);
            }

            Ok(results
                .into_iter()
                .enumerate()
                .map(|(i, r)| {
                    r.unwrap_or_else(|| panic!("every index must be accounted for, missing {i}"))
                })
                .collect())
        })
    }

    fn disconnect(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let _ = self.stream.shutdown().await;
        })
    }
}

/// Build this connection's [`Connector`] from its [`ModbusTcpConfig`] (see
/// [`crate::modbus_config_for`]) - the `"modbus-tcp"` entry [`crate::DRIVERS`]
/// dispatches to via [`crate::spawn_task_with_connector`].
///
/// Dials through [`banto_plc::dial_modbus`], the one shared implementation of
/// the Modbus TCP connect sequence (H9 transport 共通化,
/// docs/improvement-plan.md §H9) that `banto_plc::modbus::ModbusTcpClient::connect`
/// also calls - see that function's own doc comment. What still differs
/// here, and is not shareable, is what happens to the stream *after* the
/// dial: this broker wraps it in [`ModbusSession`] rather than handing it to
/// `ModbusTcpClient`'s or `ModbusWriteClient`'s own private
/// `Option<TcpStream>`, so `read_batch` and `write_batch` can borrow the
/// *same* session and share the *same* transaction-id counter - see
/// `session.rs`'s module doc, "Why one trait with both `read_batch` and
/// `write_batch`", and [`ModbusSession::next_transaction_id`]'s own doc
/// comment.
pub(crate) fn connector(config: ModbusTcpConfig) -> Connector {
    std::sync::Arc::new(move || {
        let config = config.clone();
        Box::pin(async move {
            let stream = banto_plc::dial_modbus(&config)
                .await
                .map_err(|e| e.to_string())?;
            Ok(Box::new(ModbusSession {
                stream,
                unit_id: config.unit_id,
                response_timeout: config.response_timeout,
                word_order: config.word_order,
                next_transaction_id: 0,
            }) as Box<dyn BrokerSession>)
        })
    })
}

#[cfg(test)]
mod tests {
    use banto_plc::{Address, AddressArea, DataType, ReadRequest, TagValue};
    use banto_plc_write::modbus::simulator::Simulator;
    use banto_plc_write::StringWriteRequest;
    use banto_plc_write::WriteRequest as WriteReq;

    use super::*;

    fn modbus_ref(area: AddressArea, offset: u16) -> Address {
        Address::ModbusRef {
            area,
            offset,
            bit: None,
        }
    }

    fn rreq(area: AddressArea, offset: u16, data_type: DataType) -> BatchReadRequest {
        BatchReadRequest::Numeric(ReadRequest {
            address: modbus_ref(area, offset),
            data_type,
        })
    }

    fn wreq(
        area: AddressArea,
        offset: u16,
        data_type: DataType,
        value: TagValue,
    ) -> BatchWriteRequest {
        BatchWriteRequest::Numeric(WriteReq {
            address: modbus_ref(area, offset),
            data_type,
            value,
        })
    }

    async fn dial_session(sim: &Simulator) -> ModbusSession {
        let config = ModbusTcpConfig {
            host: sim.addr.ip().to_string(),
            port: sim.addr.port(),
            connect_timeout: Duration::from_millis(500),
            response_timeout: Duration::from_millis(300),
            ..Default::default()
        };
        let stream = banto_plc::dial_modbus(&config)
            .await
            .expect("dial simulator");
        ModbusSession {
            stream,
            unit_id: config.unit_id,
            response_timeout: config.response_timeout,
            word_order: config.word_order,
            next_transaction_id: 0,
        }
    }

    /// The load-bearing property this module's doc comment on
    /// `next_transaction_id` describes: a `ModbusSession` built directly
    /// (bypassing the driver/connector, dialing the simulator manually so
    /// this test can inspect the private field) advances its ONE counter by
    /// exactly one wire request per read/write, across both directions,
    /// never resetting or diverging between them. Chosen over inspecting the
    /// simulator's received transaction ids because the field itself is the
    /// actual source of truth this driver relies on, and it is `pub(crate)`
    /// module-private, so a test in this same file can read it directly
    /// without extending the simulator's API just for this assertion.
    #[tokio::test]
    async fn modbus_driver_shares_one_transaction_id_counter_across_reads_and_writes() {
        let sim = Simulator::start().await;
        let mut session = dial_session(&sim).await;
        assert_eq!(session.next_transaction_id, 0);

        // One read (one wire request: a single contiguous group).
        session
            .read_batch(&[rreq(AddressArea::HoldingRegister, 0, DataType::U16)])
            .await
            .expect("read should succeed");
        assert_eq!(session.next_transaction_id, 1);

        // One write (one wire request).
        session
            .write_batch(&[wreq(
                AddressArea::HoldingRegister,
                0,
                DataType::U16,
                TagValue::F64(7.0),
            )])
            .await
            .expect("write should succeed");
        assert_eq!(
            session.next_transaction_id, 2,
            "the write must continue the SAME counter the read just advanced, not reset to 0"
        );

        // Another read, proving the counter keeps advancing across repeated
        // direction switches rather than settling into two independent
        // interleaved sequences.
        session
            .read_batch(&[rreq(AddressArea::HoldingRegister, 0, DataType::U16)])
            .await
            .expect("read should succeed");
        assert_eq!(session.next_transaction_id, 3);
    }

    /// A Modbus exception response must surface as a per-request `Bad`
    /// through the broker, never as a whole-call `Err` - and the session
    /// must not be torn down, proven here by successfully issuing another
    /// request against the same `ModbusSession` afterward.
    #[tokio::test]
    async fn modbus_exception_response_is_bad_not_err_and_session_survives() {
        let sim = Simulator::start().await;
        let mut session = dial_session(&sim).await;

        // FC6 (write single register) at offset 5 will return an exception.
        sim.inject_exception(0x06, 5, 0x02); // illegal data address
        let write_results = session
            .write_batch(&[wreq(
                AddressArea::HoldingRegister,
                5,
                DataType::U16,
                TagValue::F64(1.0),
            )])
            .await
            .expect("a device exception must not surface as Err");
        assert_eq!(write_results.len(), 1);
        assert!(
            matches!(
                &write_results[0],
                WriteResult::Bad(PlcWriteError::ModbusException { .. })
            ),
            "expected a ModbusException Bad, got {:?}",
            write_results[0]
        );

        // FC3 (read holding registers) at offset 9 will return an exception.
        sim.inject_exception(0x03, 9, 0x02);
        let read_results = session
            .read_batch(&[rreq(AddressArea::HoldingRegister, 9, DataType::U16)])
            .await
            .expect("a device exception must not surface as Err");
        assert_eq!(read_results.len(), 1);
        assert!(
            matches!(
                &read_results[0],
                BatchReadResult::Bad(banto_plc::PlcError::ModbusException { .. })
            ),
            "expected a ModbusException Bad, got {:?}",
            read_results[0]
        );

        // The session must still be usable - proof it was not torn down.
        let after = session
            .write_batch(&[wreq(
                AddressArea::HoldingRegister,
                20,
                DataType::U16,
                TagValue::F64(42.0),
            )])
            .await
            .expect("session must still be usable after a per-request exception");
        assert_eq!(after, vec![WriteResult::Ok]);
        assert_eq!(sim.get_holding_register(20), 42);
    }

    /// `String`/`BitInWord` entries in a mixed write batch become
    /// per-request `Bad(PlcWriteError::UnsupportedRequestKind)`, and a
    /// `Numeric` batch-mate in the same call still lands on the wire.
    #[tokio::test]
    async fn string_and_bit_in_word_are_bad_and_numeric_batch_mate_still_succeeds() {
        let sim = Simulator::start().await;
        let mut session = dial_session(&sim).await;

        let requests = vec![
            wreq(
                AddressArea::HoldingRegister,
                30,
                DataType::U16,
                TagValue::F64(99.0),
            ),
            BatchWriteRequest::String(StringWriteRequest {
                address: modbus_ref(AddressArea::HoldingRegister, 40),
                words: 4,
                value: "hi".to_string(),
                encoding: banto_plc_write::StringEncoding::ShiftJis,
            }),
            BatchWriteRequest::BitInWord {
                address: modbus_ref(AddressArea::HoldingRegister, 50),
                value: true,
            },
        ];

        let results = session
            .write_batch(&requests)
            .await
            .expect("batch with unsupported kinds must still return Ok overall");
        assert_eq!(results.len(), 3);
        assert_eq!(
            results[0],
            WriteResult::Ok,
            "the Numeric entry must succeed"
        );
        assert!(
            matches!(
                &results[1],
                WriteResult::Bad(PlcWriteError::UnsupportedRequestKind { .. })
            ),
            "String must be Bad(UnsupportedRequestKind), got {:?}",
            results[1]
        );
        assert!(
            matches!(
                &results[2],
                WriteResult::Bad(PlcWriteError::UnsupportedRequestKind { .. })
            ),
            "BitInWord must be Bad(UnsupportedRequestKind), got {:?}",
            results[2]
        );

        // The Numeric write actually landed on the wire.
        assert_eq!(sim.get_holding_register(30), 99);
    }
}
