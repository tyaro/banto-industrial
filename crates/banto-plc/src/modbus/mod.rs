//! [`ModbusTcpClient`]: the [`crate::client::PlcClient`] implementation for
//! Modbus TCP (docs/plan.md I2 §2), the protocol chosen to go first
//! (recorder-requirements.md §1: "シミュレータ/ツールが豊富でデバッグ容易な
//! ため"). MELSEC MC/SLMP is the eventual primary target and will live
//! alongside this as a sibling module implementing the same
//! [`crate::client::PlcClient`] trait.
//!
//! ## Where the connection-level/per-request line falls for this protocol
//!
//! [`ModbusTcpClient::read_batch`] issues one Modbus request per
//! [`crate::planning::PlannedRead`] group and classifies every failure via
//! [`crate::error::PlcError::is_connection_fatal`]:
//!
//! - **Modbus exception response** (device says "no" to a specific
//!   request, e.g. illegal data address) - not fatal. The byte stream is
//!   still perfectly in sync (a full, well-formed response was received,
//!   it just carries an error code instead of data), so this becomes
//!   `ReadResult::Bad` for only the requests mapped to that one group, and
//!   the loop moves on to the next group.
//! - **Anything else** (timeout, I/O error, malformed frame, transaction id
//!   mismatch) - fatal. Each leaves the byte stream in a state the client
//!   can no longer trust to be aligned with request boundaries (see the
//!   `read_exact`/cancellation note on [`ModbusTcpClient::execute_one`]), so
//!   `read_batch` stops issuing further groups, drops the socket
//!   (`self.stream = None`), and returns `Err` - the caller must `connect()`
//!   again before the next `read_batch` call. Per docs/plan.md I2 §2, this
//!   crate does not retry/reconnect on its own; that loop is I3's.

mod frame;
#[cfg(test)]
mod integration_tests;
#[cfg(any(test, feature = "simulator"))]
pub mod simulator;

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::client::{BoxFuture, PlcClient};
use crate::decode::{decode_register_value, WordOrder};
use crate::error::PlcError;
use crate::planning::{plan_requests, PlannedRead};
use crate::types::{ReadRequest, ReadResult, TagValue};

use frame::{
    build_request_frame, function_code_for, parse_mbap_header, parse_response_pdu, ParsedResponse,
    MBAP_HEADER_LEN,
};

/// Everything needed to reach and speak to one Modbus TCP device.
#[derive(Debug, Clone)]
pub struct ModbusTcpConfig {
    pub host: String,
    pub port: u16,
    /// Modbus TCP still carries a "unit id" (a holdover from RTU gateways) -
    /// mirrors `banto-tags::PlcConnection::unit_id`.
    pub unit_id: u8,
    /// Default 3s (docs/plan.md I2 §2).
    pub connect_timeout: Duration,
    /// Default 1s (docs/plan.md I2 §2).
    pub response_timeout: Duration,
    /// Default [`WordOrder::HighLow`] (docs/plan.md I2 §5).
    pub word_order: WordOrder,
}

impl Default for ModbusTcpConfig {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 502,
            unit_id: 1,
            connect_timeout: Duration::from_secs(3),
            response_timeout: Duration::from_secs(1),
            word_order: WordOrder::default(),
        }
    }
}

/// The [`crate::client::PlcClient`] implementation for Modbus TCP. One
/// instance per PLC connection - not `Clone`, not internally reconnecting
/// (see this module's doc comment and docs/plan.md I2 §2).
pub struct ModbusTcpClient {
    config: ModbusTcpConfig,
    stream: Option<TcpStream>,
    /// Wraps on overflow (`u16`, matching the wire field's width) - a stale
    /// in-flight response from transaction id `N` colliding with a *new*
    /// request that reused `N` after wrapping is theoretically possible but
    /// requires 65,536 outstanding requests, which cannot happen here since
    /// this client only ever has one request in flight at a time
    /// (`read_batch` awaits each group's response before sending the next).
    next_transaction_id: u16,
}

impl ModbusTcpClient {
    pub fn new(config: ModbusTcpConfig) -> Self {
        Self {
            config,
            stream: None,
            next_transaction_id: 0,
        }
    }

    fn next_tid(&mut self) -> u16 {
        let tid = self.next_transaction_id;
        self.next_transaction_id = self.next_transaction_id.wrapping_add(1);
        tid
    }

    /// Send one wire request for `group` and decode its response.
    /// `Err(PlcError::ModbusException { .. })` is the *only* non-fatal
    /// outcome (see this module's doc comment) - every other `Err` means
    /// the stream itself is no longer trustworthy.
    ///
    /// Cancellation note: the `tokio::time::timeout` wrapping this method's
    /// call site can cancel it mid-`read_exact`. Tokio's `read_exact` is not
    /// cancellation-safe - bytes already pulled out of the kernel socket
    /// buffer into its internal progress are lost if the future is dropped
    /// before completing. That is exactly why a timeout is treated as
    /// connection-fatal here rather than "just this group failed, try the
    /// next one": after a cancelled read, this client can no longer be sure
    /// where the byte stream's next request boundary is.
    async fn execute_one(
        stream: &mut TcpStream,
        transaction_id: u16,
        unit_id: u8,
        group: &PlannedRead,
    ) -> Result<GroupValues, PlcError> {
        let function = function_code_for(group.area);
        let frame = build_request_frame(
            transaction_id,
            unit_id,
            function,
            group.start_offset,
            group.count,
        );
        stream
            .write_all(&frame)
            .await
            .map_err(|e| PlcError::Connection(e.to_string()))?;

        let mut header_buf = [0u8; MBAP_HEADER_LEN];
        stream
            .read_exact(&mut header_buf)
            .await
            .map_err(|e| PlcError::Connection(e.to_string()))?;
        let header = parse_mbap_header(&header_buf)?;
        if header.transaction_id != transaction_id {
            return Err(PlcError::Protocol(format!(
                "transaction id mismatch: sent {transaction_id}, received {}",
                header.transaction_id
            )));
        }
        if header.unit_id != unit_id {
            return Err(PlcError::Protocol(format!(
                "unit id mismatch: sent {unit_id}, received {}",
                header.unit_id
            )));
        }

        let pdu_len = header.length as usize - 1; // length counts unit id (already consumed above) + pdu
        let mut pdu_buf = vec![0u8; pdu_len];
        stream
            .read_exact(&mut pdu_buf)
            .await
            .map_err(|e| PlcError::Connection(e.to_string()))?;

        match parse_response_pdu(&pdu_buf, function, group.count)? {
            ParsedResponse::Bits(bits) => Ok(GroupValues::Bits(bits)),
            ParsedResponse::Registers(regs) => Ok(GroupValues::Registers(regs)),
            ParsedResponse::Exception { code } => Err(PlcError::ModbusException {
                function,
                code,
                message: frame::exception_message(code).to_string(),
            }),
        }
    }
}

enum GroupValues {
    Bits(Vec<bool>),
    Registers(Vec<u16>),
}

impl PlcClient for ModbusTcpClient {
    fn connect(&mut self) -> BoxFuture<'_, Result<(), PlcError>> {
        Box::pin(async move {
            let addr = format!("{}:{}", self.config.host, self.config.port);
            let stream =
                tokio::time::timeout(self.config.connect_timeout, TcpStream::connect(&addr))
                    .await
                    .map_err(|_| PlcError::ConnectTimeout(addr.clone()))?
                    .map_err(|e| PlcError::Connection(e.to_string()))?;
            // Modbus request/response pairs are small (a handful of bytes to
            // tens of bytes for a 256-tag group's worth of registers) and
            // this client always waits for a reply before sending the next
            // request - Nagle's algorithm's batching would only ever add
            // latency here, never save a packet, which directly fights the
            // 100ms-cycle performance target (recorder-requirements.md
            // §3.1). Best-effort: a platform that rejects `set_nodelay`
            // still works, just potentially slower.
            let _ = stream.set_nodelay(true);
            self.stream = Some(stream);
            self.next_transaction_id = 0;
            Ok(())
        })
    }

    fn read_batch<'a>(
        &'a mut self,
        requests: &'a [ReadRequest],
    ) -> BoxFuture<'a, Result<Vec<ReadResult>, PlcError>> {
        Box::pin(async move {
            if self.stream.is_none() {
                return Err(PlcError::NotConnected);
            }

            let outcome = plan_requests(requests);
            let mut results: Vec<Option<ReadResult>> = vec![None; requests.len()];
            for (index, reason) in outcome.immediate_bad {
                results[index] = Some(ReadResult::Bad(reason));
            }

            let response_timeout = self.config.response_timeout;
            let unit_id = self.config.unit_id;
            let word_order = self.config.word_order;

            for group in &outcome.reads {
                let tid = self.next_tid();
                // `self.stream` is guaranteed `Some` here: the only place
                // that clears it is the fatal-error branch a few lines
                // below, which returns immediately afterward.
                let stream = self
                    .stream
                    .as_mut()
                    .expect("checked Some above, only cleared on early return");

                let attempt = tokio::time::timeout(
                    response_timeout,
                    Self::execute_one(stream, tid, unit_id, group),
                )
                .await
                .unwrap_or(Err(PlcError::ResponseTimeout));

                match attempt {
                    Ok(values) => {
                        for m in &group.mapping {
                            let value = match &values {
                                GroupValues::Bits(bits) => {
                                    TagValue::Bit(bits[m.offset_in_read as usize])
                                }
                                GroupValues::Registers(regs) => {
                                    match decode_register_value(
                                        regs,
                                        m.offset_in_read as usize,
                                        m.data_type,
                                        word_order,
                                    ) {
                                        Ok(v) => v,
                                        Err(e) => {
                                            results[m.request_index] = Some(ReadResult::Bad(e));
                                            continue;
                                        }
                                    }
                                }
                            };
                            results[m.request_index] = Some(ReadResult::Value(value));
                        }
                    }
                    Err(err) if !err.is_connection_fatal() => {
                        // Modbus exception: only this group is bad, keep going.
                        for m in &group.mapping {
                            results[m.request_index] = Some(ReadResult::Bad(err.clone()));
                        }
                    }
                    Err(err) => {
                        // Connection-fatal: stream may be desynchronized or
                        // dead. Stop, tear it down, and hand the error up -
                        // per this module's doc comment, reconnecting is
                        // the caller's (I3's) job.
                        self.stream = None;
                        return Err(err);
                    }
                }
            }

            Ok(results
                .into_iter()
                .enumerate()
                .map(|(i, r)| {
                    r.unwrap_or_else(|| {
                        panic!("plan_requests must account for every input index, missing {i}")
                    })
                })
                .collect())
        })
    }

    fn disconnect(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            self.stream = None; // dropping the TcpStream closes the socket
        })
    }
}
