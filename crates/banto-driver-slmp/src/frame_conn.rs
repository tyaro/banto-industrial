//! The frame-type half of the driver (R2b-3d, docs/banto-hub-driver-io-design.md
//! §6.2): [`FrameDriver`] for [`SlmpDriver`] and its [`FrameConnection`].
//! The engine (`banto-driver-engine`) owns the socket; this side only turns
//! specs into the 4E frames of `frame.rs` and responses back into values.
//!
//! - `create_connection` reads `word_order` and `cpu_type` (absent ones are
//!   the column defaults `low_high` / `R`, like the session in
//!   `api_session.rs`). SLMP has no handshake and no farewell.
//! - `plan_read` is the session's planner unchanged: each spec goes through
//!   the same private `read_request` (this driver's grammar on the canonical
//!   address) into `banto_plc::plan_slmp_batch`, and every group becomes one
//!   bulk read step whose `covers` are the specs it serves. A spec the
//!   planner refuses (`immediate_bad`), that does not parse, or that runs
//!   past the last device number 0xFFFFFF (`read_request` makes it an item
//!   error before grouping, #793 のレビュー - grouped, it would take the
//!   healthy specs of its group down with it) is covered by no step, so the
//!   engine answers it `ItemError::Unsupported` (design §5.2 6;
//!   `parse_address` already refuses those when the tag is saved).
//! - `decode_read` takes the device and span from the step's own request
//!   bytes (`frame::parse_request`) and each value's place from its spec, so
//!   it keeps no state between `plan_read` and the responses. A non-zero end
//!   code is `ItemError::Device` for every spec of that step (as
//!   `classify_slmp` does); a response that does not match is
//!   `FatalError::Framing`; a spec that does not fit the step it is covered
//!   by is a broken plan, `FatalError::Protocol`. Strings come back as the
//!   raw `RawValue::Bytes` of their words (low byte first); the engine
//!   decodes them.
//!
//! - `write_next` (R2b-3d-2, design §6.2) is the session's write path,
//!   `banto_plc_write::execute_slmp_writes`, as a state machine. Each item
//!   goes through the session's own `write_request` (`api_session.rs`: a
//!   `bit`-addressed `bit` tag is the read-modify-write `BitInWord`, the
//!   decision the Hub used to make) and the whole batch through the same
//!   `plan_slmp_write_batch` (the integer rounding and range check of
//!   `encode.rs`, the word order, string encoding, merging only exactly
//!   adjacent targets, one RMW per word with its bits mask-composed, a
//!   contradictory pair of bits refused). The steps are sent in the
//!   executor's order: every ordinary write (one `0x1401` step each), then
//!   every RMW as three steps - read the word, write it back with only the
//!   planned bits changed (`(current & !(set | clear)) | set`), read it
//!   again to confirm. A non-zero end code at any step is
//!   `ItemError::Device` for the items of that write or that RMW alone, and
//!   that RMW sends nothing more; the next write or RMW is still sent. A
//!   confirming read that disagrees is `ItemError::Rejected` for the
//!   disagreeing bit alone (`BitWriteVerificationFailed`). A response whose
//!   fields do not match is `FatalError::Framing` and drops the write in
//!   progress. `write_next(None)` always starts afresh.
//!
//! **Not yet**: registering this as `DriverEntry::frame` (3d-join). Until
//! then the broker keeps dialing the session in `api_session.rs`.

use banto_driver_api::{
    keys, DataType, DeviceError, FatalError, FrameCheck, FrameConnection, FrameDriver, ItemError,
    Planned, RawValue, Settings, Step, TagKind, TagSpec, WordOrder, WriteItem, WriteProgress,
};
use banto_plc::{plan_slmp_batch, Address, BatchReadRequest};
use banto_plc_write::{plan_slmp_write_batch, BatchWriteRequest, PlcWriteError, WritePayload};

use crate::api_session::{read_request, write_request, Prep};
use crate::frame::{self, Answer, Route, Series, Span};
use crate::{classify_write, SlmpDriver};

impl FrameDriver for SlmpDriver {
    fn create_connection(
        &self,
        settings: &Settings,
    ) -> Result<Box<dyn FrameConnection>, FatalError> {
        Ok(Box::new(SlmpConnection {
            route: Route::default(),
            series: crate::parse_cpu_type(settings.get(keys::CPU_TYPE).unwrap_or("R")).into(),
            word_order: banto_broker::parse_word_order(
                settings.get(keys::WORD_ORDER).unwrap_or("low_high"),
            ),
            write: None,
        }))
    }
}

/// One connection's protocol state: the header it sends and expects back,
/// the CPU series and the 32-bit word order.
pub(crate) struct SlmpConnection {
    route: Route,
    series: Series,
    word_order: WordOrder,
    /// The write in progress (`None` between writes).
    write: Option<WriteRun>,
}

/// A logical write between `write_next` calls.
struct WriteRun {
    /// One slot per item; every slot is filled by the end.
    results: Vec<Option<Result<(), ItemError>>>,
    /// Every ordinary write, then every RMW (`execute_slmp_writes`'s order).
    jobs: Vec<Job>,
    /// The next job to start.
    next: usize,
    /// The step sent and not yet answered.
    in_flight: Option<InFlight>,
}

/// One planned write or one read-modify-write.
enum Job {
    /// One `0x1401` of `payload` to `span`.
    Write {
        span: Span,
        payload: WritePayload,
        covers: Vec<u32>,
    },
    /// The read-modify-write of the one word `span`.
    Rmw {
        span: Span,
        set_mask: u16,
        clear_mask: u16,
        /// `(item, bit, value)` for each item served.
        bits: Vec<(u32, u8, bool)>,
        covers: Vec<u32>,
    },
}

impl Job {
    fn covers(&self) -> &[u32] {
        match self {
            Job::Write { covers, .. } | Job::Rmw { covers, .. } => covers,
        }
    }
}

/// Which step of a job is in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// An ordinary write.
    Write,
    /// RMW 1: read the current value.
    Read,
    /// RMW 2: write the word back with this value.
    WriteBack(u16),
    /// RMW 3: the confirming read.
    Confirm,
}

#[derive(Debug, Clone, Copy)]
struct InFlight {
    job: usize,
    stage: Stage,
}

fn broken_write(what: impl std::fmt::Display) -> FatalError {
    FatalError::Protocol(format!("SLMP の書き込みの計画が壊れています: {what}"))
}

/// A device refusal as the session reported it (`SlmpEndCode` with the
/// `slmp` crate's name for the code, through `classify_write`).
fn refusal(code: u16) -> Result<ItemError, FatalError> {
    classify_write(PlcWriteError::SlmpEndCode {
        code,
        message: frame::end_code_name(code).to_string(),
    })
}

fn broken_plan(what: impl std::fmt::Display) -> FatalError {
    FatalError::Protocol(format!(
        "SLMP の読み取りの計画と応答の解釈が合いません: {what}"
    ))
}

/// A bulk read's data, in its unit.
enum Window {
    Bits(Vec<bool>),
    Words(Vec<u16>),
}

impl FrameConnection for SlmpConnection {
    fn handshake_next(
        &mut self,
        _prev_response: Option<&[u8]>,
    ) -> Result<Option<Step>, FatalError> {
        Ok(None)
    }

    /// Only for `DriverDecides`, which this driver does not use; answers by
    /// the same length field as [`frame::response_rule`].
    fn is_complete(&self, _step: &Step, buf: &[u8]) -> Result<FrameCheck, FatalError> {
        let Some(field) = buf.get(11..13) else {
            return Ok(FrameCheck::Incomplete);
        };
        let total = 13 + u32::from(u16::from_le_bytes([field[0], field[1]]));
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
        let outcome = plan_slmp_batch(&requests);
        outcome
            .reads
            .iter()
            .map(|group| {
                let span = Span {
                    device: group.device,
                    start: group.start,
                    count: u16::try_from(group.count)
                        .map_err(|_| broken_plan(format_args!("{} 点のまとまり", group.count)))?,
                };
                let send =
                    frame::bulk_read(&self.route, self.series, &span).map_err(broken_plan)?;
                let step = Step::new(
                    send,
                    frame::response_rule(),
                    frame::max_response_len(span.read_data_len()),
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
                Ok(Planned::new(step, covers))
            })
            .collect()
    }

    fn decode_read(
        &self,
        planned: &Planned,
        specs: &[TagSpec],
        response: &[u8],
    ) -> Result<Vec<Result<RawValue, ItemError>>, FatalError> {
        let request = frame::parse_request(&planned.step.send)
            .filter(|r| r.command == frame::COMMAND_BULK_READ && r.series == self.series)
            .ok_or_else(|| broken_plan("この接続が組んだ一括読み出しの手順ではない"))?;
        let span = request.span;
        let data = match frame::check_response(&self.route, response)? {
            Answer::Refused(code) => {
                let refusal = ItemError::Device(DeviceError::new(
                    u32::from(code),
                    frame::end_code_name(code),
                ));
                return Ok(vec![Err(refusal); planned.covers.len()]);
            }
            Answer::Data(data) => data,
        };
        let window = if span.is_bits() {
            Window::Bits(frame::bits(data, span.count)?)
        } else {
            Window::Words(frame::words(data, span.count)?)
        };
        planned
            .covers
            .iter()
            .map(|&cover| {
                let spec = specs.get(cover as usize).ok_or_else(|| {
                    broken_plan(format_args!("covers の添字 {cover} が specs の外"))
                })?;
                decode_one(&span, &window, spec, self.word_order)
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
            None => {
                self.write = None;
                self.begin_write(items)?
            }
            Some(response) => {
                let mut run = self.write.take().ok_or_else(|| {
                    FatalError::Protocol("書き込みの応答が、書き込みの開始なしに届いた".into())
                })?;
                let sent = run.in_flight.take().ok_or_else(|| {
                    FatalError::Protocol("書き込みの応答が、送った step なしに届いた".into())
                })?;
                if let Some(stage) = self.answer(&mut run, sent, response)? {
                    return self.send(run, sent.job, stage);
                }
                run
            }
        };
        if let Some(job) = run.jobs.get(run.next) {
            let stage = match job {
                Job::Write { .. } => Stage::Write,
                Job::Rmw { .. } => Stage::Read,
            };
            let at = run.next;
            run.next += 1;
            return self.send(run, at, stage);
        }
        run.results
            .into_iter()
            .enumerate()
            .map(|(i, r)| {
                r.ok_or_else(|| broken_write(format_args!("タグ {i} の結果が決まらなかった")))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(WriteProgress::Done)
    }
}

impl SlmpConnection {
    /// Gate and plan `items` (the session's `write_request` and
    /// `plan_slmp_write_batch`, in the same order), and start a run.
    fn begin_write(&self, items: &[WriteItem]) -> Result<WriteRun, FatalError> {
        let mut results: Vec<Option<Result<(), ItemError>>> = vec![None; items.len()];
        let mut requests: Vec<BatchWriteRequest> = Vec::new();
        let mut origin: Vec<u32> = Vec::new();
        for (i, item) in items.iter().enumerate() {
            let index = u32::try_from(i).map_err(|_| broken_write("items が多すぎる"))?;
            match write_request(item)? {
                Prep::Item(e) => results[i] = Some(Err(e)),
                Prep::Request(request) => {
                    requests.push(request);
                    origin.push(index);
                }
            }
        }
        let item_of = |n: usize| {
            origin
                .get(n)
                .copied()
                .ok_or_else(|| broken_write("計画の添字が要求の外"))
        };
        let outcome = plan_slmp_write_batch(&requests, self.word_order);
        for (n, reason) in outcome.immediate_bad {
            results[item_of(n)? as usize] = Some(Err(classify_write(reason)?));
        }
        let mut jobs = Vec::with_capacity(outcome.writes.len() + outcome.bit_writes.len());
        for plan in outcome.writes {
            let count = u16::try_from(plan.payload.len())
                .map_err(|_| broken_write(format_args!("{} 点の書き込み", plan.payload.len())))?;
            jobs.push(Job::Write {
                span: Span {
                    device: plan.device,
                    start: plan.start,
                    count,
                },
                covers: plan
                    .request_indices
                    .iter()
                    .map(|&n| item_of(n))
                    .collect::<Result<_, _>>()?,
                payload: plan.payload,
            });
        }
        for plan in outcome.bit_writes {
            let span = Span {
                device: plan.device,
                start: plan.number,
                count: 1,
            };
            if span.is_bits() {
                return Err(broken_write(format_args!(
                    "ビットデバイス {}{} の読み書きの組み直し",
                    plan.device, plan.number
                )));
            }
            let bits = plan
                .mapping
                .iter()
                .map(|m| Ok((item_of(m.request_index)?, m.bit, m.value)))
                .collect::<Result<Vec<_>, FatalError>>()?;
            jobs.push(Job::Rmw {
                span,
                set_mask: plan.set_mask,
                clear_mask: plan.clear_mask,
                covers: bits.iter().map(|&(item, _, _)| item).collect(),
                bits,
            });
        }
        Ok(WriteRun {
            results,
            jobs,
            next: 0,
            in_flight: None,
        })
    }

    /// Send the step of job `at` at `stage`: keep `run` with it in flight.
    fn send(
        &mut self,
        mut run: WriteRun,
        at: usize,
        stage: Stage,
    ) -> Result<WriteProgress, FatalError> {
        let job = run
            .jobs
            .get(at)
            .ok_or_else(|| broken_write("送る書き込みが無い"))?;
        let ack =
            |send: Vec<u8>| Step::new(send, frame::response_rule(), frame::max_response_len(0));
        let step = match (job, stage) {
            (Job::Write { span, payload, .. }, Stage::Write) => ack(match payload {
                WritePayload::Words(words) => {
                    frame::bulk_write_words(&self.route, self.series, span, words)
                }
                WritePayload::Bits(bits) => {
                    frame::bulk_write_bits(&self.route, self.series, span, bits)
                }
            }
            .map_err(broken_write)?),
            (Job::Rmw { span, .. }, Stage::Read | Stage::Confirm) => Step::new(
                frame::bulk_read(&self.route, self.series, span).map_err(broken_write)?,
                frame::response_rule(),
                frame::max_response_len(span.read_data_len()),
            ),
            (Job::Rmw { span, .. }, Stage::WriteBack(word)) => {
                ack(
                    frame::bulk_write_words(&self.route, self.series, span, &[word])
                        .map_err(broken_write)?,
                )
            }
            _ => return Err(broken_write(format_args!("{stage:?} の手順を組めない"))),
        };
        let planned = Planned::new(step, job.covers().to_vec());
        run.in_flight = Some(InFlight { job: at, stage });
        self.write = Some(run);
        Ok(WriteProgress::Send(planned))
    }

    /// Take `response` to the step `sent`: fill the results it decides and
    /// say which step of the same job comes next (`None`: the job is over).
    fn answer(
        &self,
        run: &mut WriteRun,
        sent: InFlight,
        response: &[u8],
    ) -> Result<Option<Stage>, FatalError> {
        let job = run
            .jobs
            .get(sent.job)
            .ok_or_else(|| broken_write("送った step の書き込みが無い"))?;
        let answer = match sent.stage {
            Stage::Write | Stage::WriteBack(_) => frame::write_ack(&self.route, response)?,
            Stage::Read | Stage::Confirm => frame::check_response(&self.route, response)?,
        };
        let data = match answer {
            // The device refused this step: the items of this job alone,
            // and nothing more of it is sent (C15).
            Answer::Refused(code) => {
                let refused = Err(refusal(code)?);
                for &item in job.covers() {
                    run.results[item as usize] = Some(refused.clone());
                }
                return Ok(None);
            }
            Answer::Data(data) => data,
        };
        match (job, sent.stage) {
            (Job::Write { covers, .. }, Stage::Write) => {
                for &item in covers {
                    run.results[item as usize] = Some(Ok(()));
                }
                Ok(None)
            }
            (
                Job::Rmw {
                    set_mask,
                    clear_mask,
                    ..
                },
                Stage::Read,
            ) => {
                let current = frame::words(data, 1)?[0];
                // Only the planned bits change; every other bit is the one
                // just read (`execute_one_bit_write`'s step 2). The two masks
                // are disjoint by construction (`plan_slmp_write_batch`).
                let word = (current & !(set_mask | clear_mask)) | set_mask;
                Ok(Some(Stage::WriteBack(word)))
            }
            (Job::Rmw { .. }, Stage::WriteBack(_)) => Ok(Some(Stage::Confirm)),
            (Job::Rmw { span, bits, .. }, Stage::Confirm) => {
                let confirmed = frame::words(data, 1)?[0];
                // Each item's own bit on its own (`execute_one_bit_write`'s
                // step 4): one disagreeing bit never fails its batch-mate.
                for &(item, bit, value) in bits {
                    let landed = (confirmed >> bit) & 1 == 1;
                    run.results[item as usize] = Some(if landed == value {
                        Ok(())
                    } else {
                        Err(classify_write(PlcWriteError::BitWriteVerificationFailed {
                            area: format!("{}{}", span.device, span.start),
                            bit,
                        })?)
                    });
                }
                Ok(None)
            }
            _ => Err(broken_write(format_args!(
                "{:?} の応答をこの書き込みに当てられない",
                sent.stage
            ))),
        }
    }
}

/// One spec's value out of its step's window. `Err` is a spec that does not
/// fit the step (a broken plan); `Ok(Err(_))` a kind this driver cannot
/// read.
fn decode_one(
    span: &Span,
    window: &Window,
    spec: &TagSpec,
    order: WordOrder,
) -> Result<Result<RawValue, ItemError>, FatalError> {
    let canonical = &spec.address.canonical;
    let Some((device, number, bit)) = Address::parse_slmp(canonical)
        .ok()
        .and_then(|a| a.as_slmp())
    else {
        return Err(broken_plan(format_args!(
            "{canonical:?} は SLMP のアドレスではない"
        )));
    };
    let width = crate::base::occupancy(device, bit, &spec.kind).count;
    let offset = number.checked_sub(span.start);
    let fits = device == span.device
        && offset.is_some_and(|o| u64::from(o) + u64::from(width) <= u64::from(span.count));
    let Some(offset) = offset.filter(|_| fits) else {
        return Err(broken_plan(format_args!(
            "{canonical} が {}{} からの {} 点に入っていない",
            span.device, span.start, span.count
        )));
    };
    let at = offset as usize;
    match (window, &spec.kind, bit) {
        (Window::Bits(bits), TagKind::Numeric(DataType::Bit), None) => {
            Ok(Ok(RawValue::Bool(bits[at])))
        }
        (Window::Words(words), TagKind::Numeric(DataType::Bit), Some(bit)) => {
            Ok(Ok(RawValue::Bool((words[at] >> bit) & 1 == 1)))
        }
        (Window::Words(words), TagKind::Numeric(data_type), None) => {
            let pair = || combine(words[at], words[at + 1], order);
            Ok(match data_type {
                DataType::I16 => Ok(RawValue::Int(i64::from(words[at] as i16))),
                DataType::U16 => Ok(RawValue::Uint(u64::from(words[at]))),
                DataType::I32 => Ok(RawValue::Int(i64::from(pair() as i32))),
                DataType::U32 => Ok(RawValue::Uint(u64::from(pair()))),
                DataType::F32 => Ok(RawValue::Float(f64::from(f32::from_bits(pair())))),
                other => Err(ItemError::Unsupported(format!(
                    "SLMP はデータ型 {other} を読めません"
                ))),
            })
        }
        (Window::Words(words), TagKind::Str { words: n, .. }, None) => Ok(Ok(RawValue::Bytes(
            words[at..at + usize::from(*n)]
                .iter()
                .flat_map(|w| w.to_le_bytes())
                .collect(),
        ))),
        _ => Err(broken_plan(format_args!(
            "{canonical}（{:?}）はこの手順の単位では読めない",
            spec.kind
        ))),
    }
}

/// Two words in the device order (`first` at the lower number) as one
/// 32-bit value per `order` (MELSEC's default is `LowHigh`: the low word
/// first).
fn combine(first: u16, second: u16, order: WordOrder) -> u32 {
    let (hi, lo) = match order {
        WordOrder::HighLow => (first, second),
        WordOrder::LowHigh => (second, first),
    };
    (u32::from(hi) << 16) | u32::from(lo)
}

#[cfg(test)]
mod tests {
    use banto_driver_api::{DriverBase, StringEncoding, Value};
    use banto_plc::SlmpDevice;

    use super::*;

    fn conn(cpu: &str, order: &str) -> SlmpConnection {
        SlmpConnection {
            route: Route::default(),
            series: crate::parse_cpu_type(cpu).into(),
            word_order: banto_broker::parse_word_order(order),
            write: None,
        }
    }

    fn spec(text: &str, kind: TagKind) -> TagSpec {
        TagSpec::new(SlmpDriver.parse_address(text, &kind).unwrap(), kind)
    }

    fn n(data_type: DataType) -> TagKind {
        TagKind::numeric(data_type)
    }

    fn ok(data: &[u8]) -> Vec<u8> {
        let r = Route::default();
        let mut out = vec![0xD4, 0x00];
        out.extend_from_slice(&r.serial_id.to_le_bytes());
        out.extend_from_slice(&[0, 0, r.network_id, r.pc_id]);
        out.extend_from_slice(&r.io_id.to_le_bytes());
        out.push(r.area_id);
        out.extend_from_slice(&((2 + data.len()) as u16).to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(data);
        out
    }

    #[test]
    fn create_connection_reads_the_series_and_the_word_order() {
        let s = Settings::from_pairs([("host", "h"), ("port", "1")]).unwrap();
        let mut c = SlmpDriver.create_connection(&s).unwrap();
        assert_eq!(c.handshake_next(None), Ok(None));
        assert!(c.farewell().is_none());
        let planned = c.plan_read(&[spec("D0", n(DataType::U16))]).unwrap();
        // Absent cpu_type is R (subcommand 0002).
        assert_eq!(&planned[0].step.send[17..19], &[0x02, 0x00]);
        let s = Settings::from_pairs([("host", "h"), ("port", "1"), ("cpu_type", "Q")]).unwrap();
        let mut c = SlmpDriver.create_connection(&s).unwrap();
        let planned = c.plan_read(&[spec("D0", n(DataType::U16))]).unwrap();
        assert_eq!(&planned[0].step.send[17..19], &[0x00, 0x00]);
    }

    #[test]
    fn one_group_serves_mixed_kinds_in_spec_order() {
        let mut c = conn("R", "low_high");
        let specs = [
            spec("D102", n(DataType::F32)),
            spec("M5", n(DataType::Bit)),
            spec("D100", n(DataType::I16)),
            spec("D100.F", n(DataType::Bit)),
            spec("D104", TagKind::string(2, StringEncoding::ShiftJis)),
        ];
        let planned = c.plan_read(&specs).unwrap();
        assert_eq!(planned.len(), 2, "{planned:?}");
        let mut got = vec![None; specs.len()];
        for p in &planned {
            let req = frame::parse_request(&p.step.send).unwrap();
            let data: Vec<u8> = if req.span.is_bits() {
                // M5 alone: 1 point, true.
                vec![0x10]
            } else {
                // D100..D105: -2, 0, 1.5 (low word first), "AB", "C\0".
                [0xFFFEu16, 0, 0x0000, 0x3FC0, 0x4241, 0x0043]
                    .iter()
                    .flat_map(|w| w.to_le_bytes())
                    .collect()
            };
            assert_eq!(
                usize::from(req.span.count),
                if req.span.is_bits() { 1 } else { 6 }
            );
            let values = c.decode_read(p, &specs, &ok(&data)).unwrap();
            for (&cover, v) in p.covers.iter().zip(values) {
                got[cover as usize] = Some(v.unwrap());
            }
        }
        assert_eq!(
            got,
            vec![
                Some(RawValue::Float(1.5)),
                Some(RawValue::Bool(true)),
                Some(RawValue::Int(-2)),
                Some(RawValue::Bool(true)),
                Some(RawValue::Bytes(vec![0x41, 0x42, 0x43, 0x00])),
            ]
        );
    }

    #[test]
    fn the_word_order_decides_32_bit_values() {
        let specs = [spec("D0", n(DataType::U32)), spec("D0", n(DataType::I32))];
        let data: Vec<u8> = [0x0001u16, 0x8000]
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .collect();
        for (order, u, i) in [
            ("low_high", 0x8000_0001u64, 0x8000_0001u32 as i32),
            ("high_low", 0x0001_8000u64, 0x0001_8000),
        ] {
            let mut c = conn("R", order);
            let p = c.plan_read(&specs).unwrap();
            let v = c.decode_read(&p[0], &specs, &ok(&data)).unwrap();
            assert_eq!(
                v,
                vec![Ok(RawValue::Uint(u)), Ok(RawValue::Int(i64::from(i)))],
                "{order}"
            );
        }
    }

    #[test]
    fn a_refusal_is_device_for_the_whole_step_and_breakage_is_fatal() {
        let mut c = conn("Q", "low_high");
        let specs = [spec("D10", n(DataType::U16)), spec("D11", n(DataType::U16))];
        let p = c.plan_read(&specs).unwrap();
        assert_eq!(p.len(), 1);
        let mut refused = ok(&[]);
        refused[13..15].copy_from_slice(&0xC051u16.to_le_bytes());
        let v = c.decode_read(&p[0], &specs, &refused).unwrap();
        assert_eq!(
            v,
            vec![Err(ItemError::Device(DeviceError::new(0xC051, "Unknown Error"))); 2]
        );
        // One word short of the two asked for.
        assert!(matches!(
            c.decode_read(&p[0], &specs, &ok(&[1, 0])),
            Err(FatalError::Framing(_))
        ));
        // A different serial id.
        let mut other = ok(&[1, 0, 2, 0]);
        other[2] = 0x02;
        assert!(matches!(
            c.decode_read(&p[0], &specs, &other),
            Err(FatalError::Framing(_))
        ));
    }

    #[test]
    fn a_spec_outside_its_step_is_a_broken_plan() {
        let mut c = conn("R", "low_high");
        let specs = [spec("D10", n(DataType::U16))];
        let p = c.plan_read(&specs).unwrap();
        let data = ok(&[1, 0]);
        for wrong in [
            spec("D11", n(DataType::U16)),
            spec("D10", n(DataType::U32)),
            spec("W10", n(DataType::U16)),
            spec("D9", n(DataType::U16)),
        ] {
            assert!(
                matches!(
                    c.decode_read(&p[0], std::slice::from_ref(&wrong), &data),
                    Err(FatalError::Protocol(_))
                ),
                "{wrong:?}"
            );
        }
        // A step this connection did not build (another series).
        let q = conn("Q", "low_high").plan_read(&specs).unwrap();
        assert!(matches!(
            c.decode_read(&q[0], &specs, &data),
            Err(FatalError::Protocol(_))
        ));
        // covers pointing outside specs.
        assert!(matches!(
            c.decode_read(&p[0], &[], &data),
            Err(FatalError::Protocol(_))
        ));
    }

    /// A spec as the frame API may hand it over without going through the
    /// save path (`parse_address` refuses these now).
    fn unchecked(canonical: &str, kind: TagKind) -> TagSpec {
        let words = kind.word_count();
        TagSpec::new(
            banto_driver_api::DriverAddress::new(
                SlmpDriver.descriptor().id.clone(),
                canonical,
                "D",
                true,
                words,
            ),
            kind,
        )
    }

    /// #793 のレビュー: 番号の上限をはみ出す spec が混ざっても、計画は致命に
    /// ならず、はみ出す spec だけがどの step にも入らない。同じまとまりに
    /// なるはずだった正常な spec（上限ちょうどの ZR16777215 の U16）も読める。
    #[test]
    fn a_spec_running_past_the_last_device_number_is_left_out_alone() {
        let mut c = conn("R", "low_high");
        let specs = [
            spec("D0", n(DataType::U16)),
            unchecked("ZR16777215", n(DataType::U32)),
            spec("ZR16777215", n(DataType::U16)),
            unchecked("ZR16777214", TagKind::string(4, StringEncoding::ShiftJis)),
            spec("ZR16777213", n(DataType::U16)),
        ];
        let planned = c.plan_read(&specs).unwrap();
        let mut covered: Vec<u32> = planned.iter().flat_map(|p| p.covers.clone()).collect();
        covered.sort();
        assert_eq!(covered, vec![0, 2, 4]);
        for p in &planned {
            let req = frame::parse_request(&p.step.send).unwrap();
            let data = vec![0x07; req.span.read_data_len()];
            let values = c.decode_read(p, &specs, &ok(&data)).unwrap();
            assert!(
                values.iter().all(|v| v == &Ok(RawValue::Uint(0x0707))),
                "{values:?}"
            );
        }
    }

    #[test]
    fn specs_the_planner_refuses_are_covered_by_no_step() {
        let mut c = conn("R", "low_high");
        let specs = [
            spec("D0", n(DataType::U16)),
            spec("D10", TagKind::string(0, StringEncoding::ShiftJis)),
            spec("D20", TagKind::string(481, StringEncoding::ShiftJis)),
        ];
        let p = c.plan_read(&specs).unwrap();
        let covered: Vec<u32> = p.iter().flat_map(|p| p.covers.clone()).collect();
        assert_eq!(covered, vec![0]);
    }

    // ---- write_next (R2b-3d-2) ----

    fn item(text: &str, kind: TagKind, value: Value) -> WriteItem {
        WriteItem::new(spec(text, kind), value)
    }

    fn bit(text: &str, value: bool) -> WriteItem {
        item(text, n(DataType::Bit), Value::Bit(value))
    }

    fn sent(progress: WriteProgress) -> Planned {
        match progress {
            WriteProgress::Send(p) => p,
            other => panic!("a step was expected: {other:?}"),
        }
    }

    fn done(progress: WriteProgress) -> Vec<Result<(), ItemError>> {
        match progress {
            WriteProgress::Done(r) => r,
            other => panic!("Done was expected: {other:?}"),
        }
    }

    /// The command, the span and (for a write) the data of a step.
    fn what(p: &Planned) -> (u16, Span, Vec<u8>) {
        let req = frame::parse_request(&p.step.send).unwrap();
        let data_len = if req.command == frame::COMMAND_BULK_WRITE {
            req.span.read_data_len()
        } else {
            0
        };
        let data = p.step.send[p.step.send.len() - data_len..].to_vec();
        (req.command, req.span, data)
    }

    fn word(w: u16) -> Vec<u8> {
        ok(&w.to_le_bytes())
    }

    fn refused(code: u16) -> Vec<u8> {
        let mut r = ok(&[0; 9]);
        r[13..15].copy_from_slice(&code.to_le_bytes());
        r
    }

    fn device(code: u16) -> ItemError {
        ItemError::Device(DeviceError::new(u32::from(code), "Unknown Error"))
    }

    const READ: u16 = frame::COMMAND_BULK_READ;
    const WRITE: u16 = frame::COMMAND_BULK_WRITE;

    #[test]
    fn a_word_write_is_one_step_and_follows_the_word_order() {
        for (order, data) in [
            ("low_high", [0x70, 0x11, 0x01, 0x00]),
            ("high_low", [0x01, 0x00, 0x70, 0x11]),
        ] {
            let mut c = conn("R", order);
            let items = [item("D300", n(DataType::U32), Value::F64(70000.0))];
            let p = sent(c.write_next(&items, None).unwrap());
            assert_eq!(p.covers, vec![0]);
            let (command, span, sent_data) = what(&p);
            assert_eq!(command, WRITE);
            assert_eq!(
                (span.device, span.start, span.count),
                (SlmpDevice::D, 300, 2)
            );
            assert_eq!(sent_data, data, "{order}");
            assert_eq!(p.step.expect, frame::response_rule());
            assert_eq!(
                done(c.write_next(&items, Some(&ok(&[]))).unwrap()),
                vec![Ok(())]
            );
        }
    }

    #[test]
    fn a_bit_device_write_is_one_bit_unit_step() {
        let mut c = conn("Q", "low_high");
        let items = [bit("M20", true), bit("M21", false)];
        let p = sent(c.write_next(&items, None).unwrap());
        let (command, span, data) = what(&p);
        assert_eq!(command, WRITE);
        assert_eq!(
            (span.device, span.start, span.count),
            (SlmpDevice::M, 20, 2)
        );
        assert_eq!(data, [0x10]);
        // Q: subcommand 0001 (bit unit).
        assert_eq!(&p.step.send[17..19], &[0x01, 0x00]);
        assert_eq!(
            done(c.write_next(&items, Some(&ok(&[]))).unwrap()),
            vec![Ok(()), Ok(())]
        );
    }

    /// Two bits of one word are one RMW: read, write back with only those
    /// bits changed, read again. The other bits of the word are kept.
    #[test]
    fn bits_of_one_word_are_one_read_modify_write_that_keeps_the_others() {
        for cpu in ["R", "Q"] {
            let mut c = conn(cpu, "low_high");
            let items = [bit("D400.3", true), bit("D400.F", false)];
            let p = sent(c.write_next(&items, None).unwrap());
            assert_eq!(p.covers, vec![0, 1]);
            let (command, span, _) = what(&p);
            assert_eq!(
                (command, span.device, span.start, span.count),
                (READ, SlmpDevice::D, 400, 1)
            );
            let p = sent(c.write_next(&items, Some(&word(0x8001))).unwrap());
            let (command, span, data) = what(&p);
            assert_eq!((command, span.start, span.count), (WRITE, 400, 1));
            // 0x8001: bit 15 cleared, bit 3 set, bit 0 kept.
            assert_eq!(data, 0x0009u16.to_le_bytes(), "{cpu}");
            let p = sent(c.write_next(&items, Some(&ok(&[]))).unwrap());
            assert_eq!(what(&p).0, READ);
            assert_eq!(
                done(c.write_next(&items, Some(&word(0x0009))).unwrap()),
                vec![Ok(()), Ok(())]
            );
        }
    }

    /// Ordinary writes first, then each RMW (`execute_slmp_writes`'s order).
    #[test]
    fn ordinary_writes_go_before_the_read_modify_writes() {
        let mut c = conn("R", "low_high");
        let items = [
            bit("D400.3", true),
            item("D300", n(DataType::U16), Value::F64(5.0)),
            bit("D401.0", true),
        ];
        let mut steps = Vec::new();
        let mut progress = c.write_next(&items, None).unwrap();
        loop {
            let p = match progress {
                WriteProgress::Send(p) => p,
                WriteProgress::Done(r) => {
                    assert_eq!(r, vec![Ok(()), Ok(()), Ok(())]);
                    break;
                }
                _ => unreachable!(),
            };
            let (command, span, _) = what(&p);
            steps.push((command, span.start));
            // Each read shows the word with its bit set already.
            let answer = match (command, span.start) {
                (READ, 400) => word(0x0008),
                (READ, _) => word(0x0001),
                _ => ok(&[]),
            };
            progress = c.write_next(&items, Some(&answer)).unwrap();
        }
        assert_eq!(
            steps,
            vec![
                (WRITE, 300),
                (READ, 400),
                (WRITE, 400),
                (READ, 400),
                (READ, 401),
                (WRITE, 401),
                (READ, 401),
            ]
        );
    }

    /// C15 on the driver: a refusal at any of the three steps fails the
    /// items of that RMW alone, nothing more of it is sent, and the next
    /// RMW is still sent.
    #[test]
    fn a_refusal_at_any_step_fails_that_rmw_alone_and_sends_nothing_more_of_it() {
        for at in 0..3 {
            let mut c = conn("R", "low_high");
            let items = [
                bit("D400.3", true),
                bit("D400.5", true),
                bit("D401.0", true),
            ];
            let mut p = sent(c.write_next(&items, None).unwrap());
            for answer in [word(0), ok(&[])].into_iter().take(at) {
                p = sent(c.write_next(&items, Some(&answer)).unwrap());
            }
            assert_eq!(what(&p).1.start, 400);
            // The next step is already the other word's read.
            let p = sent(c.write_next(&items, Some(&refused(0xC051))).unwrap());
            let (command, span, _) = what(&p);
            assert_eq!((command, span.start), (READ, 401), "at {at}");
            let p = sent(c.write_next(&items, Some(&word(0))).unwrap());
            assert_eq!(what(&p).2, 0x0001u16.to_le_bytes());
            sent(c.write_next(&items, Some(&ok(&[]))).unwrap());
            assert_eq!(
                done(c.write_next(&items, Some(&word(1))).unwrap()),
                vec![Err(device(0xC051)), Err(device(0xC051)), Ok(())],
                "at {at}"
            );
        }
    }

    #[test]
    fn a_refused_word_write_fails_its_items_and_the_next_is_still_sent() {
        let mut c = conn("R", "low_high");
        let items = [
            item("D300", n(DataType::U16), Value::F64(1.0)),
            item("D301", n(DataType::U16), Value::F64(2.0)),
            item("W10", n(DataType::U16), Value::F64(3.0)),
        ];
        let p = sent(c.write_next(&items, None).unwrap());
        assert_eq!(p.covers, vec![0, 1]);
        let p = sent(c.write_next(&items, Some(&refused(0xC056))).unwrap());
        assert_eq!(p.covers, vec![2]);
        assert_eq!(
            done(c.write_next(&items, Some(&ok(&[]))).unwrap()),
            vec![Err(device(0xC056)), Err(device(0xC056)), Ok(())]
        );
    }

    /// C14 on the driver: the confirming read disagrees for one bit; that
    /// item alone is `Rejected`.
    #[test]
    fn a_confirmation_mismatch_rejects_that_bit_alone() {
        let mut c = conn("R", "low_high");
        let items = [bit("D400.3", true), bit("D400.5", true)];
        sent(c.write_next(&items, None).unwrap());
        let p = sent(c.write_next(&items, Some(&word(0))).unwrap());
        assert_eq!(what(&p).2, 0x0028u16.to_le_bytes());
        sent(c.write_next(&items, Some(&ok(&[]))).unwrap());
        let r = done(c.write_next(&items, Some(&word(0x0020))).unwrap());
        assert!(
            matches!(&r[0], Err(ItemError::Rejected(m)) if m.contains("D400")),
            "{r:?}"
        );
        assert_eq!(r[1], Ok(()));
    }

    /// Values the planner refuses never reach the wire: out of range,
    /// not integral, a non-bit value to a bit address, and a bit asked
    /// true and false at once.
    #[test]
    fn refused_values_send_nothing() {
        let mut c = conn("R", "low_high");
        let items = [
            item("D0", n(DataType::U16), Value::F64(70000.0)),
            item("D1", n(DataType::I16), Value::F64(1.5)),
            item("D2.3", n(DataType::Bit), Value::F64(1.0)),
            bit("D3.1", true),
            bit("D3.1", false),
            item("D4", n(DataType::I16), Value::F64(-32769.0)),
        ];
        let r = done(c.write_next(&items, None).unwrap());
        assert!(matches!(r[0], Err(ItemError::Unsupported(_))), "{r:?}");
        assert!(matches!(r[1], Err(ItemError::Unsupported(_))), "{r:?}");
        assert!(matches!(r[2], Err(ItemError::Unsupported(_))), "{r:?}");
        assert!(matches!(r[3], Err(ItemError::Rejected(_))), "{r:?}");
        assert!(matches!(r[4], Err(ItemError::Rejected(_))), "{r:?}");
        assert!(matches!(r[5], Err(ItemError::Unsupported(_))), "{r:?}");
        assert_eq!(done(c.write_next(&[], None).unwrap()), vec![]);
    }

    /// A response that does not match is fatal and drops the write; the
    /// next `write_next(None)` starts afresh.
    #[test]
    fn a_broken_response_is_framing_and_a_new_write_starts_afresh() {
        let mut c = conn("R", "low_high");
        let items = [bit("D400.3", true)];
        sent(c.write_next(&items, None).unwrap());
        let mut other = word(0);
        other[2] = 0x02;
        assert!(matches!(
            c.write_next(&items, Some(&other)),
            Err(FatalError::Framing(_))
        ));
        // Nothing left in progress.
        assert!(matches!(
            c.write_next(&items, Some(&word(0))),
            Err(FatalError::Protocol(_))
        ));
        // Data on a write's answer, and a read answer of the wrong size.
        sent(c.write_next(&items, None).unwrap());
        sent(c.write_next(&items, Some(&word(0))).unwrap());
        assert!(matches!(
            c.write_next(&items, Some(&word(0))),
            Err(FatalError::Framing(_))
        ));
        sent(c.write_next(&items, None).unwrap());
        assert!(matches!(
            c.write_next(&items, Some(&ok(&[0, 0, 0, 0]))),
            Err(FatalError::Framing(_))
        ));
        // An interrupted write is forgotten by the next one.
        let p = sent(c.write_next(&items, None).unwrap());
        sent(c.write_next(&items, Some(&word(0))).unwrap());
        let words = [item("D300", n(DataType::U16), Value::F64(7.0))];
        let q = sent(c.write_next(&words, None).unwrap());
        assert_ne!(p, q);
        assert_eq!(what(&q).0, WRITE);
        assert_eq!(
            done(c.write_next(&words, Some(&ok(&[]))).unwrap()),
            vec![Ok(())]
        );
    }

    /// The engine's cap is about `3 × items + 8` steps: an RMW per item
    /// is three, and nothing else is sent.
    #[test]
    fn the_steps_stay_within_three_per_item() {
        let mut c = conn("R", "low_high");
        let items: Vec<WriteItem> = (0..5)
            .map(|i| bit(&format!("D{}.0", 500 + i * 2), true))
            .collect();
        let mut steps = 0;
        let mut progress = c.write_next(&items, None).unwrap();
        while let WriteProgress::Send(p) = progress {
            steps += 1;
            let answer = if what(&p).0 == READ { word(1) } else { ok(&[]) };
            progress = c.write_next(&items, Some(&answer)).unwrap();
        }
        assert_eq!(steps, 15);
        assert!(steps <= 3 * items.len() + 8);
    }

    #[test]
    fn is_complete_follows_the_length_field() {
        let c = conn("R", "low_high");
        let step = Step::new(vec![], frame::response_rule(), 100);
        let full = ok(&[1, 2]);
        assert_eq!(
            c.is_complete(&step, &full[..12]),
            Ok(FrameCheck::Incomplete)
        );
        assert_eq!(
            c.is_complete(&step, &full[..16]),
            Ok(FrameCheck::Incomplete)
        );
        assert_eq!(
            c.is_complete(&step, &full),
            Ok(FrameCheck::Complete { len: 17 })
        );
    }
}
