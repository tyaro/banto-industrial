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
//!   planner refuses (`immediate_bad`) or that does not parse is covered by
//!   no step, so the engine answers it `ItemError::Unsupported` (design §5.2
//!   6; `parse_address` already refuses those when the tag is saved).
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
//! **Not yet**: `write_next` (the word write and the bit-in-word
//! read-modify-write state machine) is the next PR, and so is registering
//! this as `DriverEntry::frame`. Until then the broker keeps dialing the
//! session in `api_session.rs`, and `write_next` is the API's default
//! refusal.

use banto_driver_api::{
    keys, DataType, DeviceError, FatalError, FrameCheck, FrameConnection, FrameDriver, ItemError,
    Planned, RawValue, Settings, Step, TagKind, TagSpec, WordOrder,
};
use banto_plc::{plan_slmp_batch, Address, BatchReadRequest};

use crate::api_session::{read_request, Prep};
use crate::frame::{self, Answer, Route, Series, Span};
use crate::SlmpDriver;

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
        }))
    }
}

/// One connection's protocol state: the header it sends and expects back,
/// the CPU series and the 32-bit word order.
pub(crate) struct SlmpConnection {
    route: Route,
    series: Series,
    word_order: WordOrder,
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
                // #793 のレビュー: 番号の上限からはみ出す spec はまとめる前に外す
                // （どの step にも入らず、エンジンがその spec だけを
                // `Unsupported` にする）。まとめた後の `bulk_read` の範囲検査で
                // 落とすと、同じまとまりの正常な spec まで巻き込み、計画全体が
                // 致命になる。
                if !fits_device_numbers(spec) {
                    continue;
                }
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
}

/// Whether the device numbers `spec` occupies stay within the device's
/// range ([`crate::base::occupied_fits`]). A canonical address that does not
/// parse never gets here (`read_request` made it an item error).
fn fits_device_numbers(spec: &TagSpec) -> bool {
    Address::parse_slmp(&spec.address.canonical)
        .ok()
        .and_then(|a| a.as_slmp())
        .is_some_and(|(_, number, bit)| crate::base::occupied_fits(number, bit, &spec.kind))
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
    let width = crate::base::occupied_words(bit, &spec.kind);
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
    use banto_driver_api::{DriverBase, StringEncoding};

    use super::*;

    fn conn(cpu: &str, order: &str) -> SlmpConnection {
        SlmpConnection {
            route: Route::default(),
            series: crate::parse_cpu_type(cpu).into(),
            word_order: banto_broker::parse_word_order(order),
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
