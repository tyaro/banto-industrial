//! The frame-type half of the driver (R2b-3c, docs/banto-hub-driver-io-design.md
//! §6.1): [`FrameDriver`] for [`ModbusDriver`] and its [`FrameConnection`].
//! The engine (`banto-driver-engine`) owns the socket; this side only turns
//! specs into MBAP frames and responses back into values.
//!
//! - `create_connection` reads `unit_id` and `word_order` (absent ones are the
//!   column defaults `1` / `low_high`, like the stored connection).
//!   Modbus TCP has no handshake and no farewell. The connection keeps **one**
//!   transaction-id counter, `next_transaction_id`, for every request it
//!   builds - reads and writes alike. `plan_read` and `write_next` both take
//!   `&mut self`, so the single-counter property that the former session
//!   (`session.rs`, removed in R2b-3c) documented at length (a late response to a
//!   timed-out read must not be taken for the answer to the next write that
//!   reused its id) stays true by construction.
//!   The counter is taken when a step is built, in the order the engine sends
//!   them, and wraps (`u16`).
//! - `plan_read` is the former session's planner unchanged: each spec goes through
//!   the same private `read_request` into `banto_plc::plan_batch_requests`,
//!   and every group becomes one FC1/2/3/4 step (`expect` is the MBAP length
//!   field: the response is `6 + length` bytes) whose `covers` are the specs
//!   it serves. A spec the planner refuses (a string, a combination
//!   `modbus_accepts` denies) or whose address does not parse is covered by no
//!   step, so the engine answers it `ItemError::Unsupported` (design §5.2 6).
//! - `decode_read` takes the transaction id, unit id, function code, start and
//!   count from the step's own request bytes and each value's place from its
//!   spec, so it keeps no state between `plan_read` and the responses. A
//!   transaction id or unit id that is not the request's is
//!   `FatalError::Protocol` (a response to another request, P0-3 2b); a
//!   broken header or PDU is `FatalError::Framing` (as `classify_read` does
//!   for the session); a Modbus exception is `ItemError::Device` for every
//!   spec of that step; a spec that does not fit the step it is covered by is
//!   a broken plan, `FatalError::Protocol`. Values are the session's:
//!   `decode_register_value` / `decode_register_bit` with the connection's
//!   word order, handed over typed (`Int` / `Uint` / `Float` / `Bool`) so the
//!   engine's `f64` is the same number the session produced.
//! - `write_next` is the session's write path as a state machine: the same
//!   `write_request` gate (a `.N` bit write is `Unsupported`; Modbus never
//!   reads-modifies-writes), the same `plan_modbus_writes` (which merges only
//!   exactly adjacent targets, never across a gap), and one step per planned
//!   write (FC5/6/15/16 by `build_write_frame`). Each response is checked for
//!   the id, the unit and the verbatim echo (`parse_write_response_pdu`); an
//!   exception answers the requests of that step alone with
//!   `ItemError::Device` and the next step is still sent, while a mismatch is
//!   fatal. A string never reaches the wire (`Unsupported`, as in
//!   `write_typed`).
//!
//! Registered by `banto-drivers` as `DriverEntry::frame` since R2b-3c; the
//! broker wraps it in the engine's `FrameSession`.

use banto_driver_api::{
    keys, DataType, FatalError, FrameCheck, FrameConnection, FrameDriver, FrameRule, ItemError,
    Planned, PrefixSpec, RawValue, Settings, Step, TagKind, TagSpec, WordOrder, WriteItem,
    WriteProgress,
};
use banto_plc::modbus::frame::{
    build_request_frame, exception_message, function_code_for, parse_mbap_header,
    parse_response_pdu, ParsedResponse, MBAP_HEADER_LEN,
};
use banto_plc::{
    decode::{decode_register_bit, decode_register_value},
    modbus_accepts, plan_batch_requests, Address, AddressArea, BatchReadRequest, PlcError,
    TagValue,
};
use banto_plc_write::modbus::{build_write_frame, parse_write_response_pdu};
use banto_plc_write::{plan_modbus_writes, BatchWriteRequest, ModbusPlannedWrite, PlcWriteError};

use crate::request::{fatal, read_request, unit_id_of, write_request, Prep};
use crate::{classify_read, classify_write, ModbusDriver};

/// The longest Modbus TCP frame (MBAP header + the 253-byte PDU).
const MAX_RESPONSE_LEN: u32 = 260;

/// A request frame is `MBAP (7) + function + 2 words`.
const READ_FRAME_LEN: usize = MBAP_HEADER_LEN + 5;

impl FrameDriver for ModbusDriver {
    fn create_connection(
        &self,
        settings: &Settings,
    ) -> Result<Box<dyn FrameConnection>, FatalError> {
        Ok(Box::new(ModbusConnection {
            unit_id: unit_id_of(settings)?,
            word_order: banto_broker::parse_word_order(
                settings.get(keys::WORD_ORDER).unwrap_or("low_high"),
            ),
            next_transaction_id: 0,
            write: None,
        }))
    }
}

/// One connection's protocol state.
pub(crate) struct ModbusConnection {
    unit_id: u8,
    word_order: WordOrder,
    /// The **one** transaction-id counter (module doc).
    next_transaction_id: u16,
    /// The write in progress (`None` between writes).
    write: Option<WriteRun>,
}

/// A logical write between `write_next` calls.
struct WriteRun {
    /// One slot per item; every slot is filled by the end.
    results: Vec<Option<Result<(), ItemError>>>,
    /// The planned writes still to send, with the items each one serves.
    plans: Vec<(ModbusPlannedWrite, Vec<u32>)>,
    next: usize,
    in_flight: Option<InFlight>,
}

/// The step sent and not yet answered.
struct InFlight {
    transaction_id: u16,
    expected_echo: [u8; 5],
    covers: Vec<u32>,
}

fn response_rule() -> FrameRule {
    // MBAP: the length field (offset 4, 2 bytes, big endian) counts the bytes
    // after it, so the frame is `6 + length` long.
    FrameRule::LengthPrefixed(PrefixSpec::new(4, 2, true, 6))
}

fn broken_plan(what: impl std::fmt::Display) -> FatalError {
    FatalError::Protocol(format!(
        "Modbus の読み取りの計画と応答の解釈が合いません: {what}"
    ))
}

impl ModbusConnection {
    fn take_transaction_id(&mut self) -> u16 {
        let id = self.next_transaction_id;
        self.next_transaction_id = id.wrapping_add(1);
        id
    }
}

/// The MBAP header of `response` checked against the request's ids, and the
/// PDU after it.
fn response_pdu(response: &[u8], transaction_id: u16, unit_id: u8) -> Result<&[u8], FatalError> {
    let header: &[u8; MBAP_HEADER_LEN] = response
        .get(..MBAP_HEADER_LEN)
        .and_then(|h| h.try_into().ok())
        .ok_or_else(|| {
            FatalError::Framing(format!(
                "MBAP ヘッダ（{MBAP_HEADER_LEN} バイト）に足りない応答（{} バイト）",
                response.len()
            ))
        })?;
    let header = parse_mbap_header(header).map_err(|e| fatal(classify_read(e)))?;
    if header.transaction_id != transaction_id {
        return Err(FatalError::Protocol(format!(
            "トランザクション ID が合いません: 送信 {transaction_id}、応答 {}",
            header.transaction_id
        )));
    }
    if header.unit_id != unit_id {
        return Err(FatalError::Protocol(format!(
            "ユニット ID が合いません: 送信 {unit_id}、応答 {}",
            header.unit_id
        )));
    }
    let pdu = &response[MBAP_HEADER_LEN..];
    // The length field counts the unit id (already in the header) and the PDU.
    if pdu.len() != usize::from(header.length) - 1 {
        return Err(FatalError::Framing(format!(
            "MBAP の長さ欄 {} と PDU の長さ {} が合いません",
            header.length,
            pdu.len()
        )));
    }
    Ok(pdu)
}

/// What a read step asked for, recovered from its own request bytes.
struct ReadStep {
    transaction_id: u16,
    unit_id: u8,
    function: u8,
    start: u16,
    count: u16,
}

impl ReadStep {
    fn parse(send: &[u8]) -> Option<ReadStep> {
        let send: &[u8; READ_FRAME_LEN] = send.try_into().ok()?;
        let protocol_id = u16::from_be_bytes([send[2], send[3]]);
        let length = u16::from_be_bytes([send[4], send[5]]);
        let function = send[7];
        let is_read = (1..=4).contains(&function);
        (protocol_id == 0 && length == 6 && is_read).then(|| ReadStep {
            transaction_id: u16::from_be_bytes([send[0], send[1]]),
            unit_id: send[6],
            function,
            start: u16::from_be_bytes([send[8], send[9]]),
            count: u16::from_be_bytes([send[10], send[11]]),
        })
    }
}

/// A read step's data, in its unit.
enum Window {
    Bits(Vec<bool>),
    Registers(Vec<u16>),
}

impl FrameConnection for ModbusConnection {
    fn handshake_next(
        &mut self,
        _prev_response: Option<&[u8]>,
    ) -> Result<Option<Step>, FatalError> {
        Ok(None)
    }

    /// Only for `DriverDecides`, which this driver does not use; answers by
    /// the same length field as [`response_rule`].
    fn is_complete(&self, _step: &Step, buf: &[u8]) -> Result<FrameCheck, FatalError> {
        let Some(field) = buf.get(4..6) else {
            return Ok(FrameCheck::Incomplete);
        };
        let total = 6 + u32::from(u16::from_be_bytes([field[0], field[1]]));
        Ok(if buf.len() as u64 >= u64::from(total) {
            FrameCheck::Complete { len: total }
        } else {
            FrameCheck::Incomplete
        })
    }

    fn plan_read(&mut self, specs: &[TagSpec]) -> Result<Vec<Planned>, FatalError> {
        let mut requests: Vec<BatchReadRequest> = Vec::new();
        let mut spec_of: Vec<u32> = Vec::new();
        for (i, spec) in specs.iter().enumerate() {
            if let Prep::Request(request) = read_request(spec)? {
                requests.push(request);
                spec_of.push(u32::try_from(i).map_err(|_| broken_plan("specs が多すぎる"))?);
            }
        }
        let outcome = plan_batch_requests(&requests);
        let mut planned = Vec::with_capacity(outcome.reads.len());
        for group in &outcome.reads {
            let send = build_request_frame(
                self.take_transaction_id(),
                self.unit_id,
                function_code_for(group.area),
                group.start_offset,
                group.count,
            );
            let covers = group
                .mapping
                .iter()
                .map(|m| {
                    spec_of
                        .get(m.request_index)
                        .copied()
                        .ok_or_else(|| broken_plan("計画の添字が要求の外"))
                })
                .collect::<Result<Vec<u32>, _>>()?;
            planned.push(Planned::new(
                Step::new(send, response_rule(), MAX_RESPONSE_LEN),
                covers,
            ));
        }
        Ok(planned)
    }

    fn decode_read(
        &self,
        planned: &Planned,
        specs: &[TagSpec],
        response: &[u8],
    ) -> Result<Vec<Result<RawValue, ItemError>>, FatalError> {
        let step = ReadStep::parse(&planned.step.send)
            .filter(|s| s.unit_id == self.unit_id)
            .ok_or_else(|| broken_plan("この接続が組んだ読み取りの手順ではない"))?;
        let pdu = response_pdu(response, step.transaction_id, step.unit_id)?;
        let window = match parse_response_pdu(pdu, step.function, step.count)
            .map_err(|e| fatal(classify_read(e)))?
        {
            ParsedResponse::Exception { code } => {
                let refusal = classify_read(PlcError::ModbusException {
                    function: step.function,
                    code,
                    message: exception_message(code).to_string(),
                })?;
                return Ok(vec![Err(refusal); planned.covers.len()]);
            }
            ParsedResponse::Bits(bits) => Window::Bits(bits),
            ParsedResponse::Registers(regs) => Window::Registers(regs),
        };
        planned
            .covers
            .iter()
            .map(|&cover| {
                let spec = specs.get(cover as usize).ok_or_else(|| {
                    broken_plan(format_args!("covers の添字 {cover} が specs の外"))
                })?;
                decode_one(&step, &window, spec, self.word_order)
            })
            .collect()
    }

    fn write_next(
        &mut self,
        items: &[WriteItem],
        prev_response: Option<&[u8]>,
    ) -> Result<WriteProgress, FatalError> {
        // A fatal error leaves no write behind: `take` drops the run.
        let mut run = match prev_response {
            None => self.begin_write(items)?,
            Some(response) => {
                let mut run = self.write.take().ok_or_else(|| {
                    FatalError::Protocol("書き込みの応答が、書き込みの開始なしに届いた".into())
                })?;
                let sent = run.in_flight.take().ok_or_else(|| {
                    FatalError::Protocol("書き込みの応答が、送った step なしに届いた".into())
                })?;
                let outcome = response_pdu(response, sent.transaction_id, self.unit_id)
                    .map(|pdu| parse_write_response_pdu(pdu, &sent.expected_echo))?;
                let result = match outcome {
                    Ok(()) => Ok(()),
                    Err(e) => Err(classify_write(e)?),
                };
                for &cover in &sent.covers {
                    run.results[cover as usize] = Some(result.clone());
                }
                run
            }
        };
        if let Some((plan, covers)) = run.plans.get(run.next).cloned() {
            let transaction_id = self.take_transaction_id();
            let (_function, send) = build_write_frame(transaction_id, self.unit_id, &plan);
            let mut expected_echo = [0u8; 5];
            expected_echo.copy_from_slice(&send[MBAP_HEADER_LEN..MBAP_HEADER_LEN + 5]);
            run.in_flight = Some(InFlight {
                transaction_id,
                expected_echo,
                covers: covers.clone(),
            });
            run.next += 1;
            self.write = Some(run);
            return Ok(WriteProgress::Send(Planned::new(
                Step::new(send, response_rule(), MAX_RESPONSE_LEN),
                covers,
            )));
        }
        run.results
            .into_iter()
            .enumerate()
            .map(|(i, r)| {
                r.ok_or_else(|| {
                    FatalError::Protocol(format!("書き込みの計画がタグ {i} の結果を決めなかった"))
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map(WriteProgress::Done)
    }
}

impl ModbusConnection {
    /// Gate and plan `items` (the session's `write_request` and
    /// `write_typed`, in the same order), and start a run.
    fn begin_write(&mut self, items: &[WriteItem]) -> Result<WriteRun, FatalError> {
        let mut results: Vec<Option<Result<(), ItemError>>> = vec![None; items.len()];
        let mut numeric = Vec::new();
        let mut origin: Vec<u32> = Vec::new();
        for (i, item) in items.iter().enumerate() {
            let index = u32::try_from(i).map_err(|_| broken_plan("items が多すぎる"))?;
            match write_request(item)? {
                Prep::Item(e) => results[i] = Some(Err(e)),
                Prep::Request(BatchWriteRequest::Numeric(r)) => {
                    numeric.push(r);
                    origin.push(index);
                }
                Prep::Request(other) => {
                    let kind = match other {
                        BatchWriteRequest::String(_) => "文字列書き込み",
                        _ => "ビット単体(BitInWord)書き込み",
                    };
                    let refusal = classify_write(PlcWriteError::UnsupportedRequestKind {
                        kind: kind.to_string(),
                    })?;
                    results[i] = Some(Err(refusal));
                }
            }
        }
        let outcome = plan_modbus_writes(&numeric, self.word_order);
        for (n, reason) in outcome.immediate_bad {
            let at = origin
                .get(n)
                .copied()
                .ok_or_else(|| broken_plan("書き込みの計画の添字が要求の外"))?;
            results[at as usize] = Some(Err(classify_write(reason)?));
        }
        let plans = outcome
            .writes
            .into_iter()
            .map(|plan| {
                let covers = plan
                    .request_indices
                    .iter()
                    .map(|&n| {
                        origin
                            .get(n)
                            .copied()
                            .ok_or_else(|| broken_plan("書き込みの計画の添字が要求の外"))
                    })
                    .collect::<Result<Vec<u32>, _>>()?;
                Ok((plan, covers))
            })
            .collect::<Result<Vec<_>, FatalError>>()?;
        Ok(WriteRun {
            results,
            plans,
            next: 0,
            in_flight: None,
        })
    }
}

/// One spec's value out of its step's window. `Err` is a spec that does not
/// fit the step (a broken plan); `Ok(Err(_))` a value the data could not give.
fn decode_one(
    step: &ReadStep,
    window: &Window,
    spec: &TagSpec,
    order: WordOrder,
) -> Result<Result<RawValue, ItemError>, FatalError> {
    let canonical = &spec.address.canonical;
    let Some((area, offset, bit)) = Address::parse(canonical)
        .ok()
        .and_then(|a| a.as_modbus_ref())
    else {
        return Err(broken_plan(format_args!(
            "{canonical:?} は Modbus のアドレスではない"
        )));
    };
    let TagKind::Numeric(data_type) = spec.kind else {
        return Err(broken_plan(format_args!(
            "{canonical}（{:?}）は数値ではない",
            spec.kind
        )));
    };
    // `register_span` of a bit is 1: a coil, or one register holding the bit.
    let width = u32::from(data_type.register_span());
    let at = offset.checked_sub(step.start).filter(|at| {
        function_code_for(area) == step.function
            && modbus_accepts(area, bit, data_type)
            && u32::from(*at) + width <= u32::from(step.count)
    });
    let Some(at) = at.map(usize::from) else {
        return Err(broken_plan(format_args!(
            "{canonical} が FC{} の {} からの {} 点に入っていない",
            step.function, step.start, step.count
        )));
    };
    let value = match (window, bit, area) {
        (Window::Bits(bits), None, AddressArea::Coil | AddressArea::DiscreteInput) => {
            Ok(TagValue::Bit(bits[at]))
        }
        (Window::Registers(regs), Some(bit), _) => decode_register_bit(regs, at, bit),
        (Window::Registers(regs), None, _) => decode_register_value(regs, at, data_type, order),
        _ => {
            return Err(broken_plan(format_args!(
                "{canonical} はこの手順の単位では読めない"
            )))
        }
    }
    .map_err(|e| fatal(classify_read(e)))?;
    Ok(Ok(raw_of(data_type, value)))
}

/// The session's number as the typed raw value: integers as integers (the
/// engine turns them back into the same `f64`), floats as floats.
fn raw_of(data_type: DataType, value: TagValue) -> RawValue {
    match (data_type, value) {
        (_, TagValue::Bit(b)) => RawValue::Bool(b),
        (DataType::I16 | DataType::I32 | DataType::I64, TagValue::F64(x)) => {
            RawValue::Int(x as i64)
        }
        (DataType::U16 | DataType::U32 | DataType::U64, TagValue::F64(x)) => {
            RawValue::Uint(x as u64)
        }
        (_, TagValue::F64(x)) => RawValue::Float(x),
    }
}

#[cfg(test)]
mod tests {
    use banto_driver_api::{DriverBase, StringEncoding, Value};
    use banto_plc::modbus::frame::wrap_mbap;

    use super::*;

    fn conn(unit_id: u8, order: &str) -> ModbusConnection {
        ModbusConnection {
            unit_id,
            word_order: banto_broker::parse_word_order(order),
            next_transaction_id: 0,
            write: None,
        }
    }

    fn spec(text: &str, kind: TagKind) -> TagSpec {
        TagSpec::new(ModbusDriver.parse_address(text, &kind).unwrap(), kind)
    }

    fn n(data_type: DataType) -> TagKind {
        TagKind::numeric(data_type)
    }

    fn tid_of(send: &[u8]) -> u16 {
        u16::from_be_bytes([send[0], send[1]])
    }

    /// A read response with `data` (the part after the byte count).
    fn read_response(tid: u16, unit: u8, function: u8, data: &[u8]) -> Vec<u8> {
        let mut pdu = vec![function, data.len() as u8];
        pdu.extend_from_slice(data);
        wrap_mbap(tid, unit, &pdu)
    }

    fn registers(words: &[u16]) -> Vec<u8> {
        words.iter().flat_map(|w| w.to_be_bytes()).collect()
    }

    fn write_item(text: &str, kind: TagKind, value: Value) -> WriteItem {
        WriteItem::new(spec(text, kind), value)
    }

    /// The step of a `WriteProgress::Send`.
    fn sent(progress: WriteProgress) -> Planned {
        match progress {
            WriteProgress::Send(planned) => planned,
            other => panic!("Send was expected: {other:?}"),
        }
    }

    /// The echo a healthy device answers a write request with.
    fn echo(send: &[u8]) -> Vec<u8> {
        wrap_mbap(tid_of(send), send[6], &send[7..12])
    }

    #[test]
    fn create_connection_reads_the_unit_and_the_word_order() {
        let s = Settings::from_pairs([("host", "h"), ("port", "1")]).unwrap();
        let mut c = ModbusDriver.create_connection(&s).unwrap();
        assert_eq!(c.handshake_next(None), Ok(None));
        assert!(c.farewell().is_none());
        let planned = c.plan_read(&[spec("40001", n(DataType::U16))]).unwrap();
        // The default unit is 1.
        assert_eq!(planned[0].step.send[6], 1);
        let s = Settings::from_pairs([("host", "h"), ("port", "1"), ("unit_id", "9")]).unwrap();
        let mut c = ModbusDriver.create_connection(&s).unwrap();
        let planned = c.plan_read(&[spec("40001", n(DataType::U16))]).unwrap();
        assert_eq!(planned[0].step.send[6], 9);
        for bad in ["256", "-1", "x"] {
            let s = Settings::from_pairs([("host", "h"), ("port", "1"), ("unit_id", bad)]).unwrap();
            assert!(
                matches!(
                    ModbusDriver.create_connection(&s),
                    Err(FatalError::InvalidConfig(_))
                ),
                "{bad}"
            );
        }
    }

    #[test]
    fn plan_read_groups_and_frames_one_step_per_group() {
        let mut c = conn(7, "low_high");
        let specs = [
            spec("40011", n(DataType::U16)),
            spec("00001", n(DataType::Bit)),
            spec("40012", n(DataType::I16)),
            spec("40051.3", n(DataType::Bit)),
        ];
        let planned = c.plan_read(&specs).unwrap();
        // How the planner groups them is its business; every spec is served.
        let mut covered: Vec<u32> = planned.iter().flat_map(|p| p.covers.clone()).collect();
        covered.sort();
        assert_eq!(covered, vec![0, 1, 2, 3]);
        let mut ids: Vec<u16> = planned.iter().map(|p| tid_of(&p.step.send)).collect();
        assert_eq!(ids.len(), planned.len());
        ids.sort();
        assert_eq!(ids, (0..planned.len() as u16).collect::<Vec<_>>());
        for p in &planned {
            assert_eq!(p.step.send[6], 7);
            assert_eq!(p.step.expect, response_rule());
            assert_eq!(p.step.max_len, MAX_RESPONSE_LEN);
        }
    }

    #[test]
    fn one_group_serves_mixed_kinds() {
        let mut c = conn(1, "low_high");
        let specs = [
            spec("40003", n(DataType::F32)),
            spec("40001", n(DataType::I16)),
            spec("40001.15", n(DataType::Bit)),
            spec("40002", n(DataType::U16)),
        ];
        let planned = c.plan_read(&specs).unwrap();
        assert_eq!(planned.len(), 1, "{planned:?}");
        let send = &planned[0].step.send;
        let words = [0xFFFEu16, 7, 0x0000, 0x3FC0];
        let response = read_response(tid_of(send), 1, 0x03, &registers(&words));
        let values = c.decode_read(&planned[0], &specs, &response).unwrap();
        let mut got = vec![None; specs.len()];
        for (&cover, v) in planned[0].covers.iter().zip(values) {
            got[cover as usize] = Some(v.unwrap());
        }
        assert_eq!(
            got,
            vec![
                Some(RawValue::Float(1.5)),
                Some(RawValue::Int(-2)),
                Some(RawValue::Bool(true)),
                Some(RawValue::Uint(7)),
            ]
        );
    }

    #[test]
    fn a_coil_group_decodes_bits() {
        let mut c = conn(1, "low_high");
        let specs = [
            spec("00001", n(DataType::Bit)),
            spec("00003", n(DataType::Bit)),
        ];
        let planned = c.plan_read(&specs).unwrap();
        assert_eq!(planned.len(), 1);
        let response = read_response(tid_of(&planned[0].step.send), 1, 0x01, &[0b101]);
        let values = c.decode_read(&planned[0], &specs, &response).unwrap();
        assert_eq!(
            values,
            vec![Ok(RawValue::Bool(true)), Ok(RawValue::Bool(true))]
        );
    }

    #[test]
    fn the_word_order_decides_32_bit_values() {
        let specs = [
            spec("40001", n(DataType::U32)),
            spec("40001", n(DataType::I32)),
        ];
        let data = registers(&[0x0001, 0x8000]);
        for (order, u, i) in [
            ("low_high", 0x8000_0001u64, 0x8000_0001u32 as i32),
            ("high_low", 0x0001_8000u64, 0x0001_8000),
        ] {
            let mut c = conn(1, order);
            let p = c.plan_read(&specs).unwrap();
            let response = read_response(tid_of(&p[0].step.send), 1, 0x03, &data);
            let v = c.decode_read(&p[0], &specs, &response).unwrap();
            assert_eq!(
                v,
                vec![Ok(RawValue::Uint(u)), Ok(RawValue::Int(i64::from(i)))],
                "{order}"
            );
        }
    }

    #[test]
    fn an_id_or_unit_mismatch_is_protocol_and_a_broken_frame_is_framing() {
        let mut c = conn(1, "low_high");
        let specs = [spec("40001", n(DataType::U16))];
        let p = c.plan_read(&specs).unwrap();
        let tid = tid_of(&p[0].step.send);
        let data = registers(&[5]);
        let ok = c
            .decode_read(&p[0], &specs, &read_response(tid, 1, 0x03, &data))
            .unwrap();
        assert_eq!(ok, vec![Ok(RawValue::Uint(5))]);
        for (what, response) in [
            ("tid", read_response(tid.wrapping_add(1), 1, 0x03, &data)),
            ("unit", read_response(tid, 2, 0x03, &data)),
        ] {
            let r = c.decode_read(&p[0], &specs, &response);
            assert!(matches!(r, Err(FatalError::Protocol(_))), "{what}: {r:?}");
        }
        // A protocol id other than 0.
        let mut bad = read_response(tid, 1, 0x03, &data);
        bad[2] = 0x12;
        // The wrong function code and a wrong byte count.
        let wrong_function = read_response(tid, 1, 0x04, &data);
        let wrong_count = read_response(tid, 1, 0x03, &registers(&[1, 2]));
        // A length field that disagrees with what arrived.
        let mut wrong_length = read_response(tid, 1, 0x03, &data);
        wrong_length[5] += 1;
        for (what, response) in [
            ("protocol id", bad),
            ("function", wrong_function),
            ("byte count", wrong_count),
            ("length", wrong_length),
            ("empty", vec![]),
            ("short", vec![0, 0, 0]),
        ] {
            let r = c.decode_read(&p[0], &specs, &response);
            assert!(matches!(r, Err(FatalError::Framing(_))), "{what}: {r:?}");
        }
    }

    #[test]
    fn an_exception_is_device_for_the_whole_step() {
        let mut c = conn(1, "low_high");
        let specs = [
            spec("40010", n(DataType::U16)),
            spec("40011", n(DataType::U16)),
        ];
        let p = c.plan_read(&specs).unwrap();
        assert_eq!(p.len(), 1);
        let exception = wrap_mbap(tid_of(&p[0].step.send), 1, &[0x83, 0x02]);
        let v = c.decode_read(&p[0], &specs, &exception).unwrap();
        let want = classify_read(PlcError::ModbusException {
            function: 0x03,
            code: 0x02,
            message: exception_message(0x02).to_string(),
        })
        .unwrap();
        assert_eq!(v, vec![Err(want); 2]);
        assert!(matches!(v[0], Err(ItemError::Device(_))));
    }

    #[test]
    fn a_spec_outside_its_step_is_a_broken_plan() {
        let mut c = conn(1, "low_high");
        let specs = [spec("40010", n(DataType::U16))];
        let p = c.plan_read(&specs).unwrap();
        let response = read_response(tid_of(&p[0].step.send), 1, 0x03, &registers(&[1]));
        for wrong in [
            spec("40011", n(DataType::U16)),
            spec("40010", n(DataType::U32)),
            spec("30010", n(DataType::U16)),
            spec("40009", n(DataType::U16)),
        ] {
            assert!(
                matches!(
                    c.decode_read(&p[0], std::slice::from_ref(&wrong), &response),
                    Err(FatalError::Protocol(_))
                ),
                "{wrong:?}"
            );
        }
        // covers pointing outside specs.
        assert!(matches!(
            c.decode_read(&p[0], &[], &response),
            Err(FatalError::Protocol(_))
        ));
        // A step another connection (unit) built.
        let other = conn(2, "low_high").plan_read(&specs).unwrap();
        assert!(matches!(
            c.decode_read(&other[0], &specs, &response),
            Err(FatalError::Protocol(_))
        ));
        // Not a read step at all.
        let mut not_read = p[0].clone();
        not_read.step.send[7] = 0x06;
        assert!(matches!(
            c.decode_read(&not_read, &specs, &response),
            Err(FatalError::Protocol(_))
        ));
    }

    #[test]
    fn specs_the_planner_refuses_are_covered_by_no_step() {
        let mut c = conn(1, "low_high");
        let string = TagSpec::new(
            banto_driver_api::DriverAddress::new(
                ModbusDriver.descriptor().id.clone(),
                "400001",
                "holding_register",
                true,
                4,
            ),
            TagKind::string(4, StringEncoding::ShiftJis),
        );
        let broken = TagSpec::new(
            banto_driver_api::DriverAddress::new(
                ModbusDriver.descriptor().id.clone(),
                "D100",
                "holding_register",
                true,
                1,
            ),
            n(DataType::U16),
        );
        let specs = [spec("40001", n(DataType::U16)), string, broken];
        let p = c.plan_read(&specs).unwrap();
        let covered: Vec<u32> = p.iter().flat_map(|p| p.covers.clone()).collect();
        assert_eq!(covered, vec![0]);
    }

    /// The most safety-critical property (the former `session.rs`): reads and writes
    /// number their requests from the same counter, and it wraps.
    #[test]
    fn reads_and_writes_share_one_transaction_counter() {
        let mut c = conn(1, "low_high");
        let r = c.plan_read(&[spec("40001", n(DataType::U16))]).unwrap();
        assert_eq!(tid_of(&r[0].step.send), 0);
        let items = [write_item("40101", n(DataType::U16), Value::F64(1.0))];
        let w = sent(c.write_next(&items, None).unwrap());
        assert_eq!(tid_of(&w.step.send), 1);
        let r = c.plan_read(&[spec("40001", n(DataType::U16))]).unwrap();
        assert_eq!(tid_of(&r[0].step.send), 2);
        c.next_transaction_id = u16::MAX;
        let r = c.plan_read(&[spec("40001", n(DataType::U16))]).unwrap();
        assert_eq!(tid_of(&r[0].step.send), u16::MAX);
        let w = sent(c.write_next(&items, None).unwrap());
        assert_eq!(tid_of(&w.step.send), 0);
    }

    #[test]
    fn a_write_is_one_step_per_planned_write_with_the_session_frames() {
        let mut c = conn(3, "low_high");
        let items = [
            write_item("40101", n(DataType::U16), Value::F64(321.0)),
            write_item("00011", n(DataType::Bit), Value::Bit(true)),
            write_item("40201", n(DataType::U32), Value::F64(0x0001_0002 as f64)),
            // Adjacent to 40201..40202: merged into the one FC16 by the
            // planner, exactly as in the session.
            write_item("40203", n(DataType::U16), Value::F64(9.0)),
        ];
        let mut progress = c.write_next(&items, None).unwrap();
        let mut steps = Vec::new();
        let done = loop {
            match progress {
                WriteProgress::Send(planned) => {
                    let response = echo(&planned.step.send);
                    steps.push(planned);
                    progress = c.write_next(&items, Some(&response)).unwrap();
                }
                WriteProgress::Done(results) => break results,
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(done, vec![Ok(()); 4]);
        // The coil (FC5), the single register (FC6) and the merged FC16.
        let mut shape: Vec<(u8, Vec<u32>)> = steps
            .iter()
            .map(|p| (p.step.send[7], p.covers.clone()))
            .collect();
        shape.sort();
        assert_eq!(
            shape,
            vec![(0x05, vec![1]), (0x06, vec![0]), (0x10, vec![2, 3])]
        );
        assert!(steps.iter().all(|p| p.step.send[6] == 3));
        let mut ids: Vec<u16> = steps.iter().map(|p| tid_of(&p.step.send)).collect();
        ids.sort();
        assert_eq!(ids, vec![0, 1, 2]);
    }

    #[test]
    fn writes_across_a_gap_are_never_merged() {
        let mut c = conn(1, "low_high");
        let items = [
            write_item("40101", n(DataType::U16), Value::F64(1.0)),
            write_item("40103", n(DataType::U16), Value::F64(2.0)),
        ];
        let first = sent(c.write_next(&items, None).unwrap());
        assert_eq!(first.step.send[7], 0x06);
        let second = sent(c.write_next(&items, Some(&echo(&first.step.send))).unwrap());
        assert_eq!(second.step.send[7], 0x06);
        assert_eq!(first.covers.len() + second.covers.len(), 2);
    }

    #[test]
    fn a_write_exception_fails_only_its_step_and_the_next_is_still_sent() {
        let mut c = conn(1, "low_high");
        let items = [
            write_item("40101", n(DataType::U16), Value::F64(1.0)),
            write_item("40103", n(DataType::U16), Value::F64(2.0)),
        ];
        let first = sent(c.write_next(&items, None).unwrap());
        let refusal = wrap_mbap(tid_of(&first.step.send), 1, &[0x86, 0x02]);
        let second = sent(c.write_next(&items, Some(&refusal)).unwrap());
        let done = match c
            .write_next(&items, Some(&echo(&second.step.send)))
            .unwrap()
        {
            WriteProgress::Done(done) => done,
            other => panic!("{other:?}"),
        };
        let failed = first.covers[0] as usize;
        assert!(
            matches!(done[failed], Err(ItemError::Device(_))),
            "{done:?}"
        );
        assert_eq!(done[second.covers[0] as usize], Ok(()));
    }

    #[test]
    fn a_write_response_that_does_not_match_is_fatal_and_drops_the_run() {
        let items = [write_item("40101", n(DataType::U16), Value::F64(1.0))];
        let fresh = || {
            let mut c = conn(1, "low_high");
            let first = sent(c.write_next(&items, None).unwrap());
            (c, first)
        };
        // A wrong transaction id and a wrong unit id are Protocol.
        let (mut c, first) = fresh();
        let wrong_id = wrap_mbap(
            tid_of(&first.step.send).wrapping_add(1),
            1,
            &first.step.send[7..12],
        );
        assert!(matches!(
            c.write_next(&items, Some(&wrong_id)),
            Err(FatalError::Protocol(_))
        ));
        let (mut c, first) = fresh();
        let wrong_unit = wrap_mbap(tid_of(&first.step.send), 2, &first.step.send[7..12]);
        assert!(matches!(
            c.write_next(&items, Some(&wrong_unit)),
            Err(FatalError::Protocol(_))
        ));
        // An echo that differs in its value is Framing (the session's
        // `PlcWriteError::Protocol`).
        let (mut c, first) = fresh();
        let mut pdu = first.step.send[7..12].to_vec();
        pdu[4] ^= 0xFF;
        let bad_echo = wrap_mbap(tid_of(&first.step.send), 1, &pdu);
        assert!(matches!(
            c.write_next(&items, Some(&bad_echo)),
            Err(FatalError::Framing(_))
        ));
        // The run is gone: a response with no run is refused, and a fresh
        // `None` starts again.
        assert!(matches!(
            c.write_next(&items, Some(&bad_echo)),
            Err(FatalError::Protocol(_))
        ));
        assert!(matches!(
            c.write_next(&items, None),
            Ok(WriteProgress::Send(_))
        ));
    }

    #[test]
    fn a_new_write_forgets_an_interrupted_one() {
        let mut c = conn(1, "low_high");
        let items = [write_item("40101", n(DataType::U16), Value::F64(1.0))];
        let _abandoned = sent(c.write_next(&items, None).unwrap());
        let again = sent(c.write_next(&items, None).unwrap());
        let done = c.write_next(&items, Some(&echo(&again.step.send))).unwrap();
        assert_eq!(done, WriteProgress::Done(vec![Ok(())]));
    }

    #[test]
    fn writes_the_session_refuses_are_refused_without_a_step() {
        let mut c = conn(1, "low_high");
        let string = WriteItem::new(
            TagSpec::new(
                banto_driver_api::DriverAddress::new(
                    ModbusDriver.descriptor().id.clone(),
                    "400001",
                    "holding_register",
                    true,
                    2,
                ),
                TagKind::string(2, StringEncoding::ShiftJis),
            ),
            Value::Str("AB".into()),
        );
        let items = [
            // A bit of a register: no read-modify-write on Modbus.
            write_item("40051.3", n(DataType::Bit), Value::Bit(true)),
            // A read-only area.
            write_item("30001", n(DataType::U16), Value::F64(1.0)),
            // Out of the type's range.
            write_item("40101", n(DataType::U16), Value::F64(70000.0)),
            string,
        ];
        match c.write_next(&items, None).unwrap() {
            WriteProgress::Done(done) => {
                assert_eq!(done.len(), 4);
                assert!(
                    done.iter()
                        .all(|r| matches!(r, Err(ItemError::Unsupported(_)))),
                    "{done:?}"
                );
            }
            other => panic!("no step was expected: {other:?}"),
        }
        // Nothing was sent, so no id was used.
        assert_eq!(c.next_transaction_id, 0);
    }

    #[test]
    fn is_complete_follows_the_length_field() {
        let c = conn(1, "low_high");
        let step = Step::new(vec![], response_rule(), 100);
        let full = read_response(1, 1, 0x03, &registers(&[1]));
        assert_eq!(c.is_complete(&step, &full[..5]), Ok(FrameCheck::Incomplete));
        assert_eq!(
            c.is_complete(&step, &full[..full.len() - 1]),
            Ok(FrameCheck::Incomplete)
        );
        assert_eq!(
            c.is_complete(&step, &full),
            Ok(FrameCheck::Complete {
                len: full.len() as u32
            })
        );
    }
}
