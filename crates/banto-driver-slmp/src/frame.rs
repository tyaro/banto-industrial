//! The SLMP 4E binary frame, built and checked by this driver itself
//! (R2b-3d, docs/banto-hub-driver-io-design.md §6.2, owner decision Q4):
//! the frame-type driver hands the engine bytes, so the request and the
//! response check can no longer live inside the `slmp` crate's socket code.
//!
//! Only what the planners use: bulk read `0x0401` and bulk write `0x1401`,
//! in word units (word devices) and bit units (bit devices), for the Q / L
//! and R series (#605: the subcommand and the width of the device field
//! differ). The layout is the one the `slmp` crate (v0.2.1,
//! `create_subheader` / `Device::serialize` / the bulk command builders /
//! `validate_response`) puts on and accepts from the wire, which the tests in
//! `slmp_parity.rs` check byte for byte against the crate itself, for every
//! combination.
//!
//! ## Request (`54 00` ...)
//!
//! | bytes | field |
//! | --- | --- |
//! | 0..2 | subheader `54 00` |
//! | 2..4 | serial id (LE) |
//! | 4..6 | `00 00` |
//! | 6, 7 | network id, PC id |
//! | 8..10 | I/O id (LE) |
//! | 10 | area (multi-drop station) id |
//! | 11..13 | data length (LE): the bytes from the CPU timer to the end |
//! | 13..15 | CPU monitoring timer (LE, 250 ms units) |
//! | 15..17 | command (LE) |
//! | 17..19 | subcommand (LE): Q / L `0000` word, `0001` bit; R `0002` word, `0003` bit |
//! | 19.. | device: Q / L number (3 bytes LE) + code (1); R number (4 bytes LE) + code (2 bytes LE) |
//! | then | point count (LE) |
//! | then | write only: the data, words LE, or bits two per byte with the earlier point in the high nibble |
//!
//! ## Response (`D4 00` ...)
//!
//! The same first 11 bytes with `D4 00` and every field echoed, the data
//! length at 11..13 (counting from the end code), the end code at 13..15, the
//! data from 15. A non-zero end code is the device refusing this one request
//! (the stream stays aligned: the frame was complete); everything else that
//! does not match is [`FatalError::Framing`] - the bytes cannot be trusted, so
//! the connection goes.
//!
//! ## Safety of what is sent
//!
//! Every request is range-checked before a byte is built: the device has to
//! be accessed in its own unit (a word-unit write to a bit device would write
//! 16 bits at once), the point count is at least 1 and at most
//! [`MAX_WORD_POINTS`] / [`MAX_BIT_POINTS`], the whole span stays inside the
//! device numbers `0..=0xFFFFFF`, and a write carries exactly as many points
//! as it says. A request that fails a check is not built at all.
//!
//! `serial_id` stays one fixed value per connection (§6.2: a late response
//! is kept out by the engine closing the socket on a timeout).

use banto_driver_api::{FatalError, FrameRule, PrefixSpec};
use banto_plc::slmp::address::MAX_DEVICE_NUMBER;
use banto_plc::{SlmpAccess, SlmpCpu, SlmpDevice};

/// Request subheader.
const REQUEST_SUBHEADER: [u8; 2] = [0x54, 0x00];
/// Response subheader.
const RESPONSE_SUBHEADER: [u8; 2] = [0xD4, 0x00];
/// Bytes before the command of a request, and before the data of a
/// response: the 13-byte fixed part plus the CPU timer / end code.
pub(crate) const HEADER_LEN: usize = 15;
/// Where the data length is (both directions).
const LENGTH_AT: usize = 11;
/// The fixed part the data length does not count.
const FIXED_LEN: usize = 13;
/// Where a response's end code is.
const END_CODE_AT: usize = 13;
/// The error information an abnormal response may carry after its end code
/// (network, PC, I/O, station, command, subcommand).
const ERROR_INFO_LEN: usize = 9;

/// Bulk read.
pub(crate) const COMMAND_BULK_READ: u16 = 0x0401;
/// Bulk write.
pub(crate) const COMMAND_BULK_WRITE: u16 = 0x1401;

/// The most words one bulk read / write asks for: the specification's
/// word-unit limit. `plan_slmp_batch` stays at 480.
pub(crate) const MAX_WORD_POINTS: u16 = 960;
/// The most bits one bulk read / write asks for. Kept at 1920 (what fits the
/// 960 bytes of a word read; `plan_slmp_batch`'s own limit), below the
/// specification's bit-unit limit (7168 by memory, not checked on hardware).
pub(crate) const MAX_BIT_POINTS: u16 = 1920;

/// The 4E header fields a connection sends and expects back (the
/// "access route", the serial id and the CPU timer). The defaults are
/// `banto_plc::SlmpConfig`'s: the CPU on the far end of this socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Route {
    pub(crate) serial_id: u16,
    pub(crate) network_id: u8,
    pub(crate) pc_id: u8,
    pub(crate) io_id: u16,
    pub(crate) area_id: u8,
    pub(crate) cpu_timer: u16,
}

impl Default for Route {
    fn default() -> Self {
        Route {
            serial_id: 0x0001,
            network_id: 0x00,
            pc_id: 0xFF,
            io_id: 0x03FF,
            area_id: 0x00,
            cpu_timer: 0x0010,
        }
    }
}

/// The CPU series as the frame sees it: L is framed like Q.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Series {
    /// Q / L: subcommand `0000` / `0001`, 3-byte number + 1-byte code.
    Q,
    /// iQ-R: subcommand `0002` / `0003`, 4-byte number + 2-byte code.
    R,
}

impl From<SlmpCpu> for Series {
    fn from(cpu: SlmpCpu) -> Self {
        match cpu {
            SlmpCpu::Q | SlmpCpu::L => Series::Q,
            SlmpCpu::R => Series::R,
        }
    }
}

/// One contiguous run of points of one device: `count` points from `start`,
/// in the device's own unit (bits for a bit device, words for a word
/// device).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Span {
    pub(crate) device: SlmpDevice,
    pub(crate) start: u32,
    pub(crate) count: u16,
}

impl Span {
    /// Bit-unit access (a bit device) or word-unit (a word device).
    pub(crate) fn is_bits(&self) -> bool {
        self.device.access() == SlmpAccess::Bit
    }

    /// The response data of a bulk read of this span, in bytes.
    pub(crate) fn read_data_len(&self) -> usize {
        if self.is_bits() {
            usize::from(self.count).div_ceil(2)
        } else {
            usize::from(self.count) * 2
        }
    }

    /// The range checks every request goes through (module doc).
    fn check(&self) -> Result<(), RequestError> {
        let max = if self.is_bits() {
            MAX_BIT_POINTS
        } else {
            MAX_WORD_POINTS
        };
        if self.count == 0 || self.count > max {
            return Err(RequestError(format!(
                "{}{} からの {} 点は 1〜{max} 点の範囲外です",
                self.device, self.start, self.count
            )));
        }
        let last = u64::from(self.start) + u64::from(self.count) - 1;
        if last > u64::from(MAX_DEVICE_NUMBER) {
            return Err(RequestError(format!(
                "{}{} からの {} 点はデバイス番号の上限 0x{MAX_DEVICE_NUMBER:X} を超えます",
                self.device, self.start, self.count
            )));
        }
        Ok(())
    }
}

/// Why a request was not built (module doc, "Safety of what is sent").
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RequestError(pub(crate) String);

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The wire code of a device (the MELSEC device code table; the same codes
/// as `slmp::DeviceType::to_code`, which `slmp_parity.rs` checks for every
/// device).
pub(crate) const fn device_code(device: SlmpDevice) -> u8 {
    match device {
        SlmpDevice::X => 0x9C,
        SlmpDevice::Y => 0x9D,
        SlmpDevice::M => 0x90,
        SlmpDevice::L => 0x92,
        SlmpDevice::F => 0x93,
        SlmpDevice::V => 0x94,
        SlmpDevice::B => 0xA0,
        SlmpDevice::D => 0xA8,
        SlmpDevice::W => 0xB4,
        SlmpDevice::S => 0x98,
        SlmpDevice::Z => 0xCC,
        SlmpDevice::R => 0xAF,
        SlmpDevice::ZR => 0xB0,
        SlmpDevice::TS => 0xC1,
        SlmpDevice::TC => 0xC0,
        SlmpDevice::TN => 0xC2,
        SlmpDevice::SS => 0xC7,
        SlmpDevice::SC => 0xC6,
        SlmpDevice::SN => 0xC8,
        SlmpDevice::CS => 0xC4,
        SlmpDevice::CC => 0xC3,
        SlmpDevice::CN => 0xC5,
        SlmpDevice::SB => 0xA1,
        SlmpDevice::SD => 0xA9,
        SlmpDevice::SM => 0x91,
        SlmpDevice::SW => 0xB5,
        SlmpDevice::DX => 0xA2,
        SlmpDevice::DY => 0xA3,
    }
}

fn device_from_code(code: u8) -> Option<SlmpDevice> {
    SlmpDevice::all()
        .iter()
        .copied()
        .find(|&d| device_code(d) == code)
}

fn subcommand(series: Series, bits: bool) -> u16 {
    match (series, bits) {
        (Series::Q, false) => 0x0000,
        (Series::Q, true) => 0x0001,
        (Series::R, false) => 0x0002,
        (Series::R, true) => 0x0003,
    }
}

/// Header + command + subcommand + device + count, then `data`.
fn request(route: &Route, series: Series, command: u16, span: &Span, data: &[u8]) -> Vec<u8> {
    let number = span.start.to_le_bytes();
    let code = device_code(span.device);
    let device: &[u8] = match series {
        Series::Q => &[number[0], number[1], number[2], code],
        Series::R => &[number[0], number[1], number[2], number[3], code, 0x00],
    };
    // command (2) + subcommand (2) + device + count (2) + data.
    let command_len = 4 + device.len() + 2 + data.len();
    // From the CPU timer: the timer itself (2) + the command.
    let data_length =
        u16::try_from(2 + command_len).expect("checked spans keep a request far below 64 KiB");

    let mut out = Vec::with_capacity(HEADER_LEN + command_len);
    out.extend_from_slice(&REQUEST_SUBHEADER);
    out.extend_from_slice(&route.serial_id.to_le_bytes());
    out.extend_from_slice(&[0x00, 0x00]);
    out.push(route.network_id);
    out.push(route.pc_id);
    out.extend_from_slice(&route.io_id.to_le_bytes());
    out.push(route.area_id);
    out.extend_from_slice(&data_length.to_le_bytes());
    out.extend_from_slice(&route.cpu_timer.to_le_bytes());
    out.extend_from_slice(&command.to_le_bytes());
    out.extend_from_slice(&subcommand(series, span.is_bits()).to_le_bytes());
    out.extend_from_slice(device);
    out.extend_from_slice(&span.count.to_le_bytes());
    out.extend_from_slice(data);
    out
}

/// A bulk read (`0x0401`) of `span`.
pub(crate) fn bulk_read(
    route: &Route,
    series: Series,
    span: &Span,
) -> Result<Vec<u8>, RequestError> {
    span.check()?;
    Ok(request(route, series, COMMAND_BULK_READ, span, &[]))
}

/// A word-unit bulk write (`0x1401`): `words` to the word device `span`
/// starts at. `span.count` must be `words.len()`.
// The write state machine (`write_next`) is the next PR; its frames are
// built and checked against the `slmp` crate already.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn bulk_write_words(
    route: &Route,
    series: Series,
    span: &Span,
    words: &[u16],
) -> Result<Vec<u8>, RequestError> {
    span.check()?;
    if span.is_bits() {
        return Err(RequestError(format!(
            "ビットデバイス {} にワード単位で書き込もうとしました",
            span.device
        )));
    }
    if usize::from(span.count) != words.len() {
        return Err(RequestError(format!(
            "書き込みの点数 {} とデータの {} 語が合いません",
            span.count,
            words.len()
        )));
    }
    let data: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    Ok(request(route, series, COMMAND_BULK_WRITE, span, &data))
}

/// A bit-unit bulk write (`0x1401`): `bits` to the bit device `span` starts
/// at, two points per byte (the earlier in the high nibble, an odd count
/// padded with a zero nibble). `span.count` must be `bits.len()`.
// The write state machine (`write_next`) is the next PR; its frames are
// built and checked against the `slmp` crate already.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn bulk_write_bits(
    route: &Route,
    series: Series,
    span: &Span,
    bits: &[bool],
) -> Result<Vec<u8>, RequestError> {
    span.check()?;
    if !span.is_bits() {
        return Err(RequestError(format!(
            "ワードデバイス {} にビット単位で書き込もうとしました",
            span.device
        )));
    }
    if usize::from(span.count) != bits.len() {
        return Err(RequestError(format!(
            "書き込みの点数 {} とデータの {} 点が合いません",
            span.count,
            bits.len()
        )));
    }
    let data: Vec<u8> = bits
        .chunks(2)
        .map(|pair| {
            let high = u8::from(pair[0]) << 4;
            let low = pair.get(1).copied().map(u8::from).unwrap_or(0);
            high | low
        })
        .collect();
    Ok(request(route, series, COMMAND_BULK_WRITE, span, &data))
}

/// A request this module built, read back: what `decode_read` needs to know
/// about the step it is decoding. `None` for bytes that are not a bulk
/// read / write of this module's shape (a different series' device field is
/// told apart by the subcommand).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ParsedRequest {
    pub(crate) command: u16,
    pub(crate) series: Series,
    pub(crate) span: Span,
}

pub(crate) fn parse_request(bytes: &[u8]) -> Option<ParsedRequest> {
    if bytes.get(..2)? != REQUEST_SUBHEADER {
        return None;
    }
    let declared = usize::from(u16::from_le_bytes([
        *bytes.get(LENGTH_AT)?,
        *bytes.get(12)?,
    ]));
    if bytes.len() != FIXED_LEN + declared {
        return None;
    }
    let command = u16::from_le_bytes([*bytes.get(15)?, *bytes.get(16)?]);
    if command != COMMAND_BULK_READ && command != COMMAND_BULK_WRITE {
        return None;
    }
    let sub = u16::from_le_bytes([*bytes.get(17)?, *bytes.get(18)?]);
    let (series, bits) = match sub {
        0x0000 => (Series::Q, false),
        0x0001 => (Series::Q, true),
        0x0002 => (Series::R, false),
        0x0003 => (Series::R, true),
        _ => return None,
    };
    let field = bytes.get(19..)?;
    let (number, code, count_at) = match series {
        Series::Q => (
            u32::from_le_bytes([*field.first()?, *field.get(1)?, *field.get(2)?, 0]),
            *field.get(3)?,
            4,
        ),
        Series::R => {
            if *field.get(5)? != 0 {
                return None;
            }
            (
                u32::from_le_bytes([
                    *field.first()?,
                    *field.get(1)?,
                    *field.get(2)?,
                    *field.get(3)?,
                ]),
                *field.get(4)?,
                6,
            )
        }
    };
    let device = device_from_code(code)?;
    let count = u16::from_le_bytes([*field.get(count_at)?, *field.get(count_at + 1)?]);
    let span = Span {
        device,
        start: number,
        count,
    };
    if span.is_bits() != bits || span.check().is_err() {
        return None;
    }
    if command == COMMAND_BULK_READ && field.len() != count_at + 2 {
        return None;
    }
    Some(ParsedRequest {
        command,
        series,
        span,
    })
}

/// How the engine knows a response is complete: the data length at 11..13
/// (LE) plus the 13 bytes before the end code (design §6.2).
pub(crate) fn response_rule() -> FrameRule {
    FrameRule::LengthPrefixed(PrefixSpec::new(
        LENGTH_AT as u32,
        2,
        false,
        FIXED_LEN as i32,
    ))
}

/// The longest response to a request whose normal answer carries
/// `data_len` bytes: a normal answer, or an abnormal one with its error
/// information, whichever is longer.
pub(crate) fn max_response_len(data_len: usize) -> u32 {
    u32::try_from(HEADER_LEN + data_len.max(ERROR_INFO_LEN))
        .expect("checked spans keep a response far below 4 GiB")
}

/// A checked response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Answer<'a> {
    /// End code 0: the data after it.
    Data(&'a [u8]),
    /// A non-zero end code (the device refused this request).
    Refused(u16),
}

fn framing(what: impl std::fmt::Display) -> FatalError {
    FatalError::Framing(format!("SLMP の応答が壊れています: {what}"))
}

/// Check a whole response against the header `route` sent: length, the
/// subheader, every echoed field, then the end code. The fields are checked
/// before the end code (stricter than the `slmp` crate, which looks at the
/// end code first): a refusal that does not echo this request's serial id
/// and route is not this request's refusal.
pub(crate) fn check_response<'a>(
    route: &Route,
    response: &'a [u8],
) -> Result<Answer<'a>, FatalError> {
    if response.len() < HEADER_LEN {
        return Err(framing(format_args!(
            "{} バイト（最低 {HEADER_LEN} バイト）",
            response.len()
        )));
    }
    let declared = usize::from(u16::from_le_bytes([response[LENGTH_AT], response[12]]));
    if declared != response.len() - FIXED_LEN {
        return Err(framing(format_args!(
            "データ長 {declared} と実際の {} バイトが合いません",
            response.len() - FIXED_LEN
        )));
    }
    let field = |name: &str, got: &[u8], want: &[u8]| {
        if got == want {
            Ok(())
        } else {
            Err(framing(format_args!(
                "{name} が {got:02X?}（期待は {want:02X?}）"
            )))
        }
    };
    field("サブヘッダ", &response[0..2], &RESPONSE_SUBHEADER)?;
    field(
        "シリアル番号",
        &response[2..4],
        &route.serial_id.to_le_bytes(),
    )?;
    field("予約", &response[4..6], &[0x00, 0x00])?;
    field("ネットワーク番号", &response[6..7], &[route.network_id])?;
    field("PC 番号", &response[7..8], &[route.pc_id])?;
    field(
        "要求先ユニット I/O 番号",
        &response[8..10],
        &route.io_id.to_le_bytes(),
    )?;
    field("要求先ユニット局番号", &response[10..11], &[route.area_id])?;
    let end_code = u16::from_le_bytes([response[END_CODE_AT], response[END_CODE_AT + 1]]);
    if end_code != 0 {
        return Ok(Answer::Refused(end_code));
    }
    Ok(Answer::Data(&response[HEADER_LEN..]))
}

/// The words of a word-unit bulk read of `count` points: exactly
/// `2 × count` bytes, LE.
pub(crate) fn words(data: &[u8], count: u16) -> Result<Vec<u16>, FatalError> {
    if data.len() != usize::from(count) * 2 {
        return Err(framing(format_args!(
            "{count} 語の読み出しに {} バイトのデータ",
            data.len()
        )));
    }
    let (pairs, _) = data.as_chunks::<2>();
    Ok(pairs.iter().map(|&w| u16::from_le_bytes(w)).collect())
}

/// The points of a bit-unit bulk read of `count` points: exactly
/// `ceil(count / 2)` bytes, the earlier point in the high nibble. Each
/// nibble is read by its lowest bit, as the `slmp` crate does.
pub(crate) fn bits(data: &[u8], count: u16) -> Result<Vec<bool>, FatalError> {
    if data.len() != usize::from(count).div_ceil(2) {
        return Err(framing(format_args!(
            "{count} 点のビット読み出しに {} バイトのデータ",
            data.len()
        )));
    }
    Ok(data
        .iter()
        .flat_map(|&b| [(b >> 4) & 0x01 == 1, b & 0x01 == 1])
        .take(usize::from(count))
        .collect())
}

/// A write's answer: no data on success.
// The write state machine (`write_next`) is the next PR; its frames are
// built and checked against the `slmp` crate already.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn write_ack(route: &Route, response: &[u8]) -> Result<Answer<'static>, FatalError> {
    match check_response(route, response)? {
        Answer::Data([]) => Ok(Answer::Data(&[])),
        Answer::Data(extra) => Err(framing(format_args!(
            "書き込みの応答に {} バイトのデータ",
            extra.len()
        ))),
        Answer::Refused(code) => Ok(Answer::Refused(code)),
    }
}

/// The name of an end code (the table of `slmp::end_code_name`, which
/// `slmp_parity.rs` checks).
pub(crate) fn end_code_name(code: u16) -> &'static str {
    match code {
        0xC059 => "WrongCommand",
        0xC05C => "WrongFormat",
        0xC061 => "WrongLength",
        0xCEE0 => "Busy",
        0xCEE1 => "ExceedReqLength",
        0xCEE2 => "ExceedRespLength",
        0xCF10 => "ServerNotFound",
        0xCF20 => "WrongConfigItem",
        0xCF30 => "PrmIDNotFound",
        0xCF31 => "NotStartExclusiveWrite",
        0xCF70 => "RelayFailure",
        0xCF71 => "TimeoutError",
        _ => "Unknown Error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(device: SlmpDevice, start: u32, count: u16) -> Span {
        Span {
            device,
            start,
            count,
        }
    }

    /// A response to `route` with `end_code` and `data`.
    fn response(route: &Route, end_code: u16, data: &[u8]) -> Vec<u8> {
        let mut out = vec![0xD4, 0x00];
        out.extend_from_slice(&route.serial_id.to_le_bytes());
        out.extend_from_slice(&[0, 0, route.network_id, route.pc_id]);
        out.extend_from_slice(&route.io_id.to_le_bytes());
        out.push(route.area_id);
        out.extend_from_slice(&((2 + data.len()) as u16).to_le_bytes());
        out.extend_from_slice(&end_code.to_le_bytes());
        out.extend_from_slice(data);
        out
    }

    /// The layout of the module doc, written out once by hand.
    #[test]
    fn a_q_word_read_and_an_r_bit_read_byte_by_byte() {
        let r = Route::default();
        assert_eq!(
            bulk_read(&r, Series::Q, &span(SlmpDevice::D, 0x01_2345, 3)).unwrap(),
            [
                0x54, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0x03, 0x00, // header
                0x0C, 0x00, 0x10, 0x00, // length 12, timer
                0x01, 0x04, 0x00, 0x00, // 0401 / 0000
                0x45, 0x23, 0x01, 0xA8, // D 0x012345
                0x03, 0x00, // 3 points
            ]
        );
        assert_eq!(
            bulk_read(&r, Series::R, &span(SlmpDevice::M, 100, 5)).unwrap(),
            [
                0x54, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0x03, 0x00, //
                0x0E, 0x00, 0x10, 0x00, // length 14
                0x01, 0x04, 0x03, 0x00, // 0401 / 0003
                0x64, 0x00, 0x00, 0x00, 0x90, 0x00, // M100
                0x05, 0x00,
            ]
        );
    }

    #[test]
    fn writes_carry_their_data() {
        let r = Route::default();
        let w = bulk_write_words(
            &r,
            Series::R,
            &span(SlmpDevice::D, 10, 2),
            &[0x1234, 0xABCD],
        )
        .unwrap();
        assert_eq!(&w[11..13], &[0x12, 0x00]); // 2 + 4 + 6 + 2 + 4
        assert_eq!(&w[15..19], &[0x01, 0x14, 0x02, 0x00]);
        assert_eq!(&w[25..], &[0x02, 0x00, 0x34, 0x12, 0xCD, 0xAB]);
        let b = bulk_write_bits(
            &r,
            Series::Q,
            &span(SlmpDevice::Y, 0x20, 3),
            &[true, false, true],
        )
        .unwrap();
        assert_eq!(&b[17..19], &[0x01, 0x00]);
        assert_eq!(&b[19..], &[0x20, 0x00, 0x00, 0x9D, 0x03, 0x00, 0x10, 0x10]);
    }

    #[test]
    fn requests_out_of_range_are_not_built() {
        let r = Route::default();
        let max = MAX_DEVICE_NUMBER;
        for (s, why) in [
            (span(SlmpDevice::D, 0, 0), "0 点"),
            (span(SlmpDevice::D, 0, MAX_WORD_POINTS + 1), "語の上限 + 1"),
            (
                span(SlmpDevice::M, 0, MAX_BIT_POINTS + 1),
                "ビットの上限 + 1",
            ),
            (span(SlmpDevice::D, max, 2), "番号の上限をまたぐ"),
            (span(SlmpDevice::D, max + 1, 1), "番号の上限 + 1"),
            (span(SlmpDevice::ZR, u32::MAX, 1), "u32 の上限"),
        ] {
            for series in [Series::Q, Series::R] {
                assert!(bulk_read(&r, series, &s).is_err(), "読み出し {why}");
                let words = vec![0; usize::from(s.count)];
                let bits = vec![false; usize::from(s.count)];
                if s.is_bits() {
                    assert!(
                        bulk_write_bits(&r, series, &s, &bits).is_err(),
                        "書き込み {why}"
                    );
                } else {
                    assert!(
                        bulk_write_words(&r, series, &s, &words).is_err(),
                        "書き込み {why}"
                    );
                }
            }
        }
        // The edges themselves are fine.
        assert!(bulk_read(&r, Series::R, &span(SlmpDevice::D, max, 1)).is_ok());
        assert!(bulk_read(&r, Series::R, &span(SlmpDevice::D, max - 959, 960)).is_ok());
        assert!(bulk_read(&r, Series::Q, &span(SlmpDevice::M, 0, MAX_BIT_POINTS)).is_ok());
    }

    #[test]
    fn writes_refuse_the_wrong_unit_and_a_count_that_lies() {
        let r = Route::default();
        // A word write to a bit device would write 16 bits.
        assert!(bulk_write_words(&r, Series::R, &span(SlmpDevice::M, 0, 1), &[1]).is_err());
        assert!(bulk_write_bits(&r, Series::R, &span(SlmpDevice::D, 0, 1), &[true]).is_err());
        assert!(bulk_write_words(&r, Series::R, &span(SlmpDevice::D, 0, 2), &[1]).is_err());
        assert!(bulk_write_words(&r, Series::R, &span(SlmpDevice::D, 0, 1), &[1, 2]).is_err());
        assert!(bulk_write_bits(&r, Series::Q, &span(SlmpDevice::M, 0, 3), &[true]).is_err());
    }

    #[test]
    fn parse_request_reads_back_what_was_built() {
        let r = Route::default();
        for series in [Series::Q, Series::R] {
            for s in [
                span(SlmpDevice::D, 0, 1),
                span(SlmpDevice::ZR, MAX_DEVICE_NUMBER, 1),
                span(SlmpDevice::X, 0x1A, 7),
                span(SlmpDevice::SM, 400, MAX_BIT_POINTS),
            ] {
                let got = parse_request(&bulk_read(&r, series, &s).unwrap()).unwrap();
                assert_eq!(
                    got,
                    ParsedRequest {
                        command: COMMAND_BULK_READ,
                        series,
                        span: s
                    }
                );
            }
        }
        let mut bad = bulk_read(&r, Series::R, &span(SlmpDevice::D, 0, 1)).unwrap();
        assert!(parse_request(&bad[..bad.len() - 1]).is_none());
        bad[17] = 0x07;
        assert!(parse_request(&bad).is_none());
        assert!(parse_request(&[]).is_none());
        assert!(parse_request(&[0x54]).is_none());
    }

    #[test]
    fn a_good_response_and_a_refusal() {
        let r = Route::default();
        assert_eq!(
            check_response(&r, &response(&r, 0, &[1, 2])),
            Ok(Answer::Data(&[1, 2]))
        );
        assert_eq!(
            check_response(&r, &response(&r, 0xC051, &[])),
            Ok(Answer::Refused(0xC051))
        );
        // A refusal with its error information.
        assert_eq!(
            check_response(&r, &response(&r, 0xC059, &[0; ERROR_INFO_LEN])),
            Ok(Answer::Refused(0xC059))
        );
    }

    #[test]
    fn every_field_that_does_not_echo_is_framing() {
        let r = Route {
            serial_id: 0xBEEF,
            network_id: 1,
            pc_id: 2,
            io_id: 0x03E0,
            area_id: 3,
            cpu_timer: 0x20,
        };
        let good = response(&r, 0, &[0, 0]);
        // Every byte of the first 11 that is checked, with end code 0 and with
        // a refusal: both are framing (a refusal must be this request's).
        for at in 0..11 {
            for end in [0u16, 0xC051] {
                let mut bad = response(&r, end, &[0, 0]);
                bad[at] ^= 0x01;
                assert!(
                    matches!(check_response(&r, &bad), Err(FatalError::Framing(_))),
                    "byte {at}, end code {end:04X}"
                );
            }
        }
        // The length: short, too long, and lying either way.
        assert!(matches!(
            check_response(&r, &good[..14]),
            Err(FatalError::Framing(_))
        ));
        let mut long = good.clone();
        long.push(0);
        assert!(matches!(
            check_response(&r, &long),
            Err(FatalError::Framing(_))
        ));
        let mut lie = good.clone();
        lie[11] = 2;
        assert!(matches!(
            check_response(&r, &lie),
            Err(FatalError::Framing(_))
        ));
        assert!(matches!(
            check_response(&r, &[]),
            Err(FatalError::Framing(_))
        ));
    }

    #[test]
    fn data_must_be_exactly_the_points_asked_for() {
        assert_eq!(
            words(&[0x34, 0x12, 0xCD, 0xAB], 2),
            Ok(vec![0x1234, 0xABCD])
        );
        assert!(words(&[0x34, 0x12], 2).is_err());
        assert!(words(&[0x34, 0x12, 0, 0, 0], 2).is_err());
        assert_eq!(bits(&[0x10, 0x01], 3), Ok(vec![true, false, false]));
        assert_eq!(bits(&[0x11], 2), Ok(vec![true, true]));
        assert!(bits(&[0x11], 3).is_err());
        assert!(bits(&[0x11, 0x00], 2).is_err());
    }

    #[test]
    fn a_write_answer_carries_no_data() {
        let r = Route::default();
        assert_eq!(write_ack(&r, &response(&r, 0, &[])), Ok(Answer::Data(&[])));
        assert_eq!(
            write_ack(&r, &response(&r, 0xC051, &[0; 9])),
            Ok(Answer::Refused(0xC051))
        );
        assert!(matches!(
            write_ack(&r, &response(&r, 0, &[0, 0])),
            Err(FatalError::Framing(_))
        ));
    }

    #[test]
    fn the_rule_measures_a_whole_response() {
        let FrameRule::LengthPrefixed(p) = response_rule() else {
            panic!("LengthPrefixed");
        };
        let resp = response(&Route::default(), 0, &[1, 2, 3, 4]);
        let field = u16::from_le_bytes([resp[p.offset as usize], resp[p.offset as usize + 1]]);
        assert_eq!(i64::from(field) + i64::from(p.adjust), resp.len() as i64);
        assert!(!p.big_endian);
        assert_eq!(max_response_len(0), 24);
        assert_eq!(max_response_len(960), 975);
    }
}
