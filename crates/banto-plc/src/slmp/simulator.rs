//! In-process SLMP simulator: the MELSEC counterpart to
//! `modbus/simulator.rs`, feature-gated the same way and for the same reasons
//! (docs/plan.md I2 §6). Real MELSEC hardware cannot be a test dependency, and
//! the plan's own testing section (「テスト方針」) has this shared by I2a's
//! crate tests, I5's write client, and W3's engine integration tests rather
//! than each standing up its own fake CPU.
//!
//! Not an SLMP conformance tool: it implements exactly the two commands this
//! repository's clients issue - bulk read (`0x0401`) and, since #363, bulk
//! write (`0x1401`), both bit-unit and word-unit - keeps device state in
//! plain `HashMap`s (sparse: any device never explicitly set reads back as
//! `0`/`false`, convenient for tests that care about a handful of addresses),
//! and can be told to return a canned end code, emit a deliberately
//! malformed frame, or hang instead of answering.
//!
//! ## Write support and the "held" set (#363、2026-09-15 オーナー決定)
//!
//! The MELSEC twin of `modbus/simulator.rs`'s section of the same name - read
//! that one for the full rationale. In short: the owner decision of
//! 2026-09-15 makes an external write to a PC-side simulated device *land on
//! the simulator* rather than be refused (banto-hub's old write gate 4,
//! `WriteRejection::SimulationWriteRejected`, is gone), so this simulator
//! needs to accept `0x1401`; and because `banto-collect`'s
//! `simulation::slmp_ramp_task` rewrites `D0..D15`/`M0..M15` every 100 ms, a
//! device written over the wire is recorded in a held set and the seeding
//! setters ([`Simulator::set_word`], [`Simulator::set_words`],
//! [`Simulator::set_string`], [`Simulator::set_bit`]) become no-ops for it.
//! The ramp therefore keeps running on every device the operator has *not*
//! taken over, and the value that was written reads back on the next poll.
//! The held set lives and dies with the simulator instance.
//!
//! ## Why this speaks real SLMP bytes rather than mocking the crate
//!
//! The alternative - a fake in place of `slmp::SLMPClient` behind a trait -
//! would have been less code, and would have tested nothing that matters. The
//! two things most likely to be wrong in `slmp/mod.rs` are (a) whether the
//! wrapped crate actually keeps returning `slmp::SlmpError::Device { .. }`
//! for a non-zero end code and `slmp::SlmpError::Framing(_)` for a corrupt
//! frame - the structured distinction [`super::classify_slmp_error`] matches
//! on (H9, docs/h9-slmp-structured-error-spec.md) - and (b) whether a
//! bit-unit response's nibble packing is decoded the way the crate expects.
//! Both live strictly *inside* the crate, so only real bytes on a real socket
//! exercise them. That is also what makes `slmp_end_code_is_bad_not_fatal` a
//! working tripwire on the dependency rather than a restatement of this
//! crate's own assumptions.
//!
//! The frame layout implemented below is the 4E binary request/response pair as
//! the wrapped crate builds and validates it (`slmp::SLMPClient`'s
//! `create_subheader` / `validate_response`): a 15-byte prefix, then a 2-byte
//! end code, then payload.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

use super::address::SlmpDevice;

/// Bytes before the payload in both directions: subheader (2), serial id (2),
/// blank (2), network id (1), PC id (1), I/O id (2), area id (1), data length
/// (2), then a final 2 bytes that are the CPU timer on a request and the end
/// code on a response.
const FRAME_PREFIX_LEN: usize = 15;

/// SLMP bulk read command code, little-endian on the wire.
const COMMAND_BULK_READ: u16 = 0x0401;

/// SLMP bulk write command code (#363) - the write twin of
/// [`COMMAND_BULK_READ`], same device-field layout, payload instead of point
/// count only.
const COMMAND_BULK_WRITE: u16 = 0x1401;

/// Subcommand bit 0: set = bit-unit access, clear = word-unit access.
const SUBCOMMAND_BIT_ACCESS: u16 = 0x0001;
/// Subcommand bit 1: set = R series (6-byte device field), clear = Q/L series
/// (4-byte). Reading the CPU series straight off the request is what lets one
/// simulator serve both without being told which is under test.
const SUBCOMMAND_R_SERIES: u16 = 0x0002;

/// End code the simulator returns for a request it cannot make sense of -
/// SLMP's "wrong command", which is also what a real CPU answers for a
/// command it does not implement.
const END_CODE_WRONG_COMMAND: u16 = 0xC059;

#[derive(Debug, Default)]
struct State {
    words: HashMap<(SlmpDevice, u32), u16>,
    bits: HashMap<(SlmpDevice, u32), bool>,
    /// Word devices written over the wire (`0x1401`, word unit) - see this
    /// module's "Write support and the held set" section. [`Simulator::set_word`]
    /// is a no-op for these, so the ramp task stops overwriting them.
    held_words: HashSet<(SlmpDevice, u32)>,
    /// Bit devices written over the wire (`0x1401`, bit unit), the bit twin
    /// of `held_words`.
    held_bits: HashSet<(SlmpDevice, u32)>,
    /// How many bulk *write* commands have been served - test support,
    /// exposed as [`Simulator::write_command_count`].
    write_commands: usize,
    /// Exact `(device, start_number)` match -> end code to return instead of
    /// data, for injecting CPU-side refusals (the SLMP analogue of
    /// `modbus/simulator.rs`'s `exceptions`).
    end_codes: HashMap<(SlmpDevice, u32), u16>,
    /// When set, every response is emitted with a data-length field that
    /// disagrees with the bytes actually sent. Exists to exercise the *other*
    /// half of the end-code/framing pair: a framing failure
    /// (`slmp::SlmpError::Framing(_)`), which must be classified
    /// connection-fatal even though the pre-H9 wrapped crate used to report
    /// it through the same `io::ErrorKind::InvalidData` as a perfectly
    /// recoverable end code.
    malformed: bool,
    /// When set, requests are never answered - for exercising the client's
    /// response timeout without needing an unreachable host.
    hang: bool,
}

/// A running simulator instance. Dropping this does *not* stop the server (the
/// accept/handler tasks keep running detached, same as any `tokio::spawn`) -
/// call [`Simulator::stop`] to shut it down, which is what closes every client
/// socket and is how tests exercise the "PLC disconnected mid-session" path.
pub struct Simulator {
    pub addr: SocketAddr,
    state: Arc<Mutex<State>>,
    accept_task: JoinHandle<()>,
    /// One entry per connection accepted so far (including already-closed
    /// ones, which is harmless - aborting a finished task is a no-op). Kept so
    /// [`Simulator::stop`] can sever *live* connections too, not just stop
    /// accepting new ones; without it, a client that connected before `stop()`
    /// would keep talking to its already-spawned handler and the
    /// "PLC disconnected mid-session" path would have nothing to observe.
    connections: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl Simulator {
    /// Bind a loopback listener on an OS-assigned port and start accepting
    /// connections. Supports multiple concurrent/sequential connections (each
    /// handled by its own spawned task), and serves Q/L and R series clients
    /// interchangeably - the CPU series is read off each request's subcommand.
    pub async fn start() -> Self {
        Self::start_on("127.0.0.1:0".parse().expect("valid loopback address"))
            .await
            .expect("bind loopback listener")
    }

    /// As [`Simulator::start`], but binds `addr` instead of letting the OS
    /// pick - the SLMP twin of `modbus/simulator.rs`'s `start_on` (#344).
    ///
    /// Added for ChronoGazer's R1-C C-4 (2026-09-23): the dev PLC example
    /// (`apps/chronogazer/core/examples/dev_plc.rs`) has to listen on a
    /// **fixed port** so an operator - or the E2E suite's `webServer` - can
    /// register it as an ordinary SLMP connection. Returns the bind error
    /// rather than panicking so that example can report "port in use" and
    /// exit non-zero instead of dumping a panic.
    pub async fn start_on(addr: SocketAddr) -> std::io::Result<Self> {
        let listener = TcpListener::bind(addr).await?;
        let addr = listener.local_addr().expect("local_addr");
        let state = Arc::new(Mutex::new(State::default()));
        let connections: Arc<Mutex<Vec<JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));

        let accept_state = state.clone();
        let accept_connections = connections.clone();
        let accept_task = tokio::spawn(async move {
            loop {
                let (stream, _peer) = match listener.accept().await {
                    Ok(pair) => pair,
                    Err(_) => return, // listener dropped/closed - stop() path
                };
                let conn_state = accept_state.clone();
                let handle = tokio::spawn(async move {
                    handle_connection(stream, conn_state).await;
                });
                accept_connections.lock().unwrap().push(handle);
            }
        });

        Ok(Simulator {
            addr,
            state,
            accept_task,
            connections,
        })
    }

    /// Set one word device (`D`/`W`/`R`/...). Panics on a bit device: writing a
    /// word to `M100` is a mistake in the *test*, not a condition worth
    /// simulating, and a silent no-op there would show up as a confusing
    /// all-zeros assertion failure much later.
    ///
    /// **No-op if the device is held** (it was written over the wire, #363) -
    /// see this module's "Write support and the held set" section: this is
    /// what stops `banto-collect`'s ramp task from erasing a value an
    /// external client just wrote.
    pub fn set_word(&self, device: SlmpDevice, number: u32, value: u16) {
        assert_eq!(
            device.access(),
            super::SlmpAccess::Word,
            "{device} is a bit device - use set_bit"
        );
        let mut state = self.state.lock().unwrap();
        if state.held_words.contains(&(device, number)) {
            return;
        }
        state.words.insert((device, number), value);
    }

    /// Set consecutive word devices starting at `start`.
    pub fn set_words(&self, device: SlmpDevice, start: u32, values: &[u16]) {
        for (i, &v) in values.iter().enumerate() {
            self.set_word(device, start + i as u32, v);
        }
    }

    /// Seed a MELSEC string (S1 文字列タグ): Shift-JIS-encode `s`, pad with
    /// 0x00 to exactly `words` word devices (2 bytes each), and lay the bytes
    /// in low-byte-first per word - the storage convention
    /// `decode.rs::decode_string_value` documents. Panics if `s` cannot be
    /// SJIS-encoded or exceeds `2 * words` bytes: that is a mistake in the
    /// *test*, not a condition worth simulating (same stance as
    /// [`Simulator::set_word`] on a bit device).
    pub fn set_string(&self, device: SlmpDevice, start: u32, words: u16, s: &str) {
        let (bytes, _, had_errors) = encoding_rs::SHIFT_JIS.encode(s);
        assert!(!had_errors, "{s:?} is not representable in Shift-JIS");
        let capacity = words as usize * 2;
        assert!(
            bytes.len() <= capacity,
            "{s:?} is {} SJIS bytes, over the {capacity}-byte capacity of {words} words",
            bytes.len()
        );
        let mut padded = bytes.into_owned();
        padded.resize(capacity, 0x00);
        for (i, chunk) in padded.as_chunks::<2>().0.iter().enumerate() {
            self.set_word(device, start + i as u32, u16::from_le_bytes(*chunk));
        }
    }

    /// Set one bit device (`M`/`X`/`Y`/...). Panics on a word device, mirroring
    /// [`Simulator::set_word`], and is likewise a no-op for a held device
    /// (#363).
    pub fn set_bit(&self, device: SlmpDevice, number: u32, value: bool) {
        assert_eq!(
            device.access(),
            super::SlmpAccess::Bit,
            "{device} is a word device - use set_word"
        );
        let mut state = self.state.lock().unwrap();
        if state.held_bits.contains(&(device, number)) {
            return;
        }
        state.bits.insert((device, number), value);
    }

    /// Current value of one word device, as a read would see it. Unset
    /// devices read back as `0`.
    pub fn get_word(&self, device: SlmpDevice, number: u32) -> u16 {
        *self
            .state
            .lock()
            .unwrap()
            .words
            .get(&(device, number))
            .unwrap_or(&0)
    }

    /// Current value of one bit device. Unset devices read back as `false`.
    pub fn get_bit(&self, device: SlmpDevice, number: u32) -> bool {
        *self
            .state
            .lock()
            .unwrap()
            .bits
            .get(&(device, number))
            .unwrap_or(&false)
    }

    /// How many devices (words plus bits) have been written over the wire
    /// and are therefore excluded from the seeding setters - the observation
    /// API for the held-set behaviour described in this module's doc comment.
    pub fn held_count(&self) -> usize {
        let state = self.state.lock().unwrap();
        state.held_words.len() + state.held_bits.len()
    }

    /// How many bulk write commands (`0x1401`) this simulator has served.
    pub fn write_command_count(&self) -> usize {
        self.state.lock().unwrap().write_commands
    }

    /// Every request whose group *starts* at `(device, start_number)` gets this
    /// end code instead of data. Persists until [`Simulator::clear_end_code`]
    /// (not one-shot), same contract as `modbus/simulator.rs`'s
    /// `inject_exception`.
    pub fn inject_end_code(&self, device: SlmpDevice, start_number: u32, code: u16) {
        self.state
            .lock()
            .unwrap()
            .end_codes
            .insert((device, start_number), code);
    }

    pub fn clear_end_code(&self, device: SlmpDevice, start_number: u32) {
        self.state
            .lock()
            .unwrap()
            .end_codes
            .remove(&(device, start_number));
    }

    /// Answer every request with a frame whose declared data length disagrees
    /// with its actual payload, so the wrapped crate rejects it as a framing
    /// error rather than reading an end code out of it.
    pub fn emit_malformed_frames(&self) {
        self.state.lock().unwrap().malformed = true;
    }

    pub fn stop_emitting_malformed_frames(&self) {
        self.state.lock().unwrap().malformed = false;
    }

    /// Stop responding to any request on any connection (existing or future)
    /// until [`Simulator::stop_hanging`] is called - for exercising the
    /// client's response timeout.
    pub fn hang(&self) {
        self.state.lock().unwrap().hang = true;
    }

    pub fn stop_hanging(&self) {
        self.state.lock().unwrap().hang = false;
    }

    /// Stop accepting new connections and sever every connection already open,
    /// simulating a CPU power-cycle or network drop mid-session. Aborting each
    /// handler task drops its `TcpStream`, which closes the socket - the
    /// connected client observes this on its next read or write.
    pub fn stop(self) {
        self.accept_task.abort();
        for handle in self.connections.lock().unwrap().drain(..) {
            handle.abort();
        }
    }
}

/// Reverse of [`SlmpDevice::to_wire`]: recover the device from the byte on the
/// wire. Linear over 28 entries, which is irrelevant at simulator speeds and
/// avoids a second hand-maintained table that could disagree with the first.
fn device_from_wire_code(code: u8) -> Option<SlmpDevice> {
    SlmpDevice::all()
        .iter()
        .copied()
        .find(|d| d.to_wire().to_code() == code)
}

/// One parsed bulk read/write request: the device field plus the point
/// count, which sit in the same place in both commands (`0x0401` ends
/// there, `0x1401` carries the payload after it).
struct BulkRequest {
    device: SlmpDevice,
    start: u32,
    count: usize,
    bit_access: bool,
    /// Byte offset just past the point count - where a bulk write's payload
    /// begins, and the end of a bulk read's command.
    payload_at: usize,
}

async fn handle_connection(mut stream: TcpStream, state: Arc<Mutex<State>>) {
    loop {
        let mut prefix = [0u8; FRAME_PREFIX_LEN];
        if stream.read_exact(&mut prefix).await.is_err() {
            return; // client closed the connection
        }
        let serial_id = u16::from_le_bytes([prefix[2], prefix[3]]);
        let network_id = prefix[6];
        let pc_id = prefix[7];
        let io_id = u16::from_le_bytes([prefix[8], prefix[9]]);
        let area_id = prefix[10];
        // The request's length field counts from the CPU timer, i.e. the two
        // CPU-timer bytes (already inside `prefix`) plus the command payload.
        let declared_len = u16::from_le_bytes([prefix[11], prefix[12]]) as usize;
        let command_len = declared_len.saturating_sub(2);

        let mut command = vec![0u8; command_len];
        if command_len > 0 && stream.read_exact(&mut command).await.is_err() {
            return;
        }

        if state.lock().unwrap().hang {
            // Never resolves - the client's own response-timeout budget is
            // what ends this, not us.
            std::future::pending::<()>().await;
        }

        let route = Route {
            serial_id,
            network_id,
            pc_id,
            io_id,
            area_id,
        };
        let response = build_response(&state, &route, &command);
        if stream.write_all(&response).await.is_err() {
            return;
        }
    }
}

/// The access-route fields a response must echo back verbatim; the wrapped
/// crate checks every one of them and rejects a mismatch, so getting these
/// wrong would look like a framing bug rather than a simulator bug.
struct Route {
    serial_id: u16,
    network_id: u8,
    pc_id: u8,
    io_id: u16,
    area_id: u8,
}

fn parse_bulk_request(command: &[u8]) -> Option<BulkRequest> {
    if command.len() < 4 {
        return None;
    }
    let subcommand = u16::from_le_bytes([command[2], command[3]]);
    let bit_access = subcommand & SUBCOMMAND_BIT_ACCESS != 0;
    let r_series = subcommand & SUBCOMMAND_R_SERIES != 0;

    // Device field: 3 address bytes + 1 device code (Q/L), or 3 address bytes
    // + a 0x00 pad + device code + a 0x00 pad (R). Either way the address is
    // the same little-endian 3 bytes and the device code's position is what
    // moves.
    let (device_field_len, device_code_index) = if r_series { (6, 4) } else { (4, 3) };
    let body = command.get(4..)?;
    if body.len() < device_field_len + 2 {
        return None;
    }

    let start = u32::from_le_bytes([body[0], body[1], body[2], 0]);
    let device = device_from_wire_code(body[device_code_index])?;
    let count = u16::from_le_bytes([body[device_field_len], body[device_field_len + 1]]) as usize;

    Some(BulkRequest {
        device,
        start,
        count,
        bit_access,
        payload_at: 4 + device_field_len + 2,
    })
}

fn build_response(state: &Arc<Mutex<State>>, route: &Route, command: &[u8]) -> Vec<u8> {
    let mut state = state.lock().unwrap();

    let code = if command.len() >= 2 {
        u16::from_le_bytes([command[0], command[1]])
    } else {
        0
    };
    match code {
        COMMAND_BULK_READ => build_read_response(&state, route, command),
        COMMAND_BULK_WRITE => {
            state.write_commands += 1;
            build_write_response(&mut state, route, command)
        }
        _ => frame(route, END_CODE_WRONG_COMMAND, &[], state.malformed),
    }
}

fn build_read_response(state: &State, route: &Route, command: &[u8]) -> Vec<u8> {
    let Some(request) = parse_bulk_request(command) else {
        return frame(route, END_CODE_WRONG_COMMAND, &[], state.malformed);
    };

    if let Some(&code) = state.end_codes.get(&(request.device, request.start)) {
        return frame(route, code, &[], state.malformed);
    }

    // A bit-unit request against a word device (or the reverse) is something a
    // real CPU rejects, and something this crate's planner should never emit -
    // answering with an end code makes a planner regression visible as a
    // failed assertion instead of as silently plausible zeros.
    let expects_bit = request.device.access() == super::SlmpAccess::Bit;
    if expects_bit != request.bit_access {
        return frame(route, END_CODE_WRONG_COMMAND, &[], state.malformed);
    }

    let payload = if request.bit_access {
        // Bit-unit response packing: two points per byte, the earlier device in
        // the high nibble. This is the layout the wrapped crate decodes with
        // `[(x >> 4) & 0x01, x & 0x01]`, and the reason this simulator has to
        // emit real bytes rather than be mocked out.
        let mut bytes = Vec::with_capacity(request.count.div_ceil(2));
        for pair_index in 0..request.count.div_ceil(2) {
            let mut byte = 0u8;
            for (nibble, shift) in [(0usize, 4u32), (1, 0)] {
                let point = pair_index * 2 + nibble;
                if point < request.count {
                    let number = request.start + point as u32;
                    let set = *state.bits.get(&(request.device, number)).unwrap_or(&false);
                    if set {
                        byte |= 1 << shift;
                    }
                }
            }
            bytes.push(byte);
        }
        bytes
    } else {
        // Word-unit response: two little-endian bytes per point.
        let mut bytes = Vec::with_capacity(request.count * 2);
        for i in 0..request.count {
            let number = request.start + i as u32;
            let word = *state.words.get(&(request.device, number)).unwrap_or(&0);
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        bytes
    };

    frame(route, 0, &payload, state.malformed)
}

/// Bulk write (`0x1401`, #363): the same device field and point count as a
/// bulk read, followed by the payload - `count` little-endian words
/// (word unit) or `count` points packed two per byte with the earlier point
/// in the high nibble (bit unit, the layout the wrapped `slmp` crate emits).
/// A successful write answers end code `0` with no payload.
///
/// Every device this commits is recorded as **held**, which is what takes it
/// out of `banto-collect`'s ramp updates - see this module's "Write support
/// and the held set" section.
fn build_write_response(state: &mut State, route: &Route, command: &[u8]) -> Vec<u8> {
    let Some(request) = parse_bulk_request(command) else {
        return frame(route, END_CODE_WRONG_COMMAND, &[], state.malformed);
    };

    if let Some(&code) = state.end_codes.get(&(request.device, request.start)) {
        return frame(route, code, &[], state.malformed);
    }

    // Same access-unit symmetry check the read path makes, and for the same
    // reason: a real CPU refuses a bit-unit write to a word device.
    let expects_bit = request.device.access() == super::SlmpAccess::Bit;
    if expects_bit != request.bit_access {
        return frame(route, END_CODE_WRONG_COMMAND, &[], state.malformed);
    }

    let data = &command[request.payload_at.min(command.len())..];
    let needed = if request.bit_access {
        request.count.div_ceil(2)
    } else {
        request.count * 2
    };
    if request.count == 0 || data.len() < needed {
        return frame(route, END_CODE_WRONG_COMMAND, &[], state.malformed);
    }

    if request.bit_access {
        for point in 0..request.count {
            let byte = data[point / 2];
            let bit = if point.is_multiple_of(2) {
                (byte >> 4) & 0x01
            } else {
                byte & 0x01
            };
            let number = request.start + point as u32;
            state.bits.insert((request.device, number), bit == 1);
            state.held_bits.insert((request.device, number));
        }
    } else {
        for i in 0..request.count {
            let word = u16::from_le_bytes([data[i * 2], data[i * 2 + 1]]);
            let number = request.start + i as u32;
            state.words.insert((request.device, number), word);
            state.held_words.insert((request.device, number));
        }
    }

    frame(route, 0, &[], state.malformed)
}

/// Assemble a 4E binary response frame. `end_code` of `0` means success;
/// anything else is a CPU-side refusal and carries no payload.
///
/// `malformed` deliberately corrupts the declared data length so the wrapped
/// crate's length-consistency check fails - see [`State::malformed`].
fn frame(route: &Route, end_code: u16, payload: &[u8], malformed: bool) -> Vec<u8> {
    const RESPONSE_CODE: [u8; 2] = [0xD4, 0x00];
    // The response's length field counts the end code plus the payload.
    let mut data_len = (2 + payload.len()) as u16;
    if malformed {
        data_len = data_len.wrapping_add(1);
    }

    let mut out = Vec::with_capacity(FRAME_PREFIX_LEN + payload.len());
    out.extend_from_slice(&RESPONSE_CODE);
    out.extend_from_slice(&route.serial_id.to_le_bytes());
    out.extend_from_slice(&[0x00, 0x00]);
    out.push(route.network_id);
    out.push(route.pc_id);
    out.extend_from_slice(&route.io_id.to_le_bytes());
    out.push(route.area_id);
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(&end_code.to_le_bytes());
    out.extend_from_slice(payload);
    out
}

/// Wire-level tests for bulk write (`0x1401`) and the held set (#363).
/// Hand-built 4E request frames rather than a client: `banto-plc` has no
/// write client (it is read-only on purpose - see this crate's `lib.rs`) and
/// `banto-plc-write` cannot be a dependency here, so the bytes go on the
/// socket directly.
#[cfg(test)]
mod write_tests {
    use super::*;
    use tokio::net::TcpStream;

    /// Q/L-series 4E binary request: 15-byte prefix (the last two bytes of
    /// which are the CPU timer) followed by the command.
    fn request_frame(command: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(FRAME_PREFIX_LEN + command.len());
        out.extend_from_slice(&[0x54, 0x00]); // 4E request subheader
        out.extend_from_slice(&1u16.to_le_bytes()); // serial id
        out.extend_from_slice(&[0x00, 0x00]); // blank
        out.push(0x00); // network id
        out.push(0xFF); // PC id
        out.extend_from_slice(&0x03FFu16.to_le_bytes()); // I/O id
        out.push(0x00); // area id
        out.extend_from_slice(&((command.len() + 2) as u16).to_le_bytes());
        out.extend_from_slice(&0x0010u16.to_le_bytes()); // CPU timer
        out.extend_from_slice(command);
        out
    }

    fn bulk_command(
        code: u16,
        device: SlmpDevice,
        start: u32,
        count: u16,
        payload: &[u8],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&code.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // word unit, Q/L series
        out.extend_from_slice(&start.to_le_bytes()[..3]);
        out.push(device.to_wire().to_code());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(payload);
        out
    }

    fn bulk_bit_command(
        code: u16,
        device: SlmpDevice,
        start: u32,
        count: u16,
        payload: &[u8],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&code.to_le_bytes());
        out.extend_from_slice(&SUBCOMMAND_BIT_ACCESS.to_le_bytes());
        out.extend_from_slice(&start.to_le_bytes()[..3]);
        out.push(device.to_wire().to_code());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(payload);
        out
    }

    /// Send one command and return `(end_code, payload)`.
    async fn roundtrip(stream: &mut TcpStream, command: &[u8]) -> (u16, Vec<u8>) {
        stream
            .write_all(&request_frame(command))
            .await
            .expect("write request");
        let mut prefix = [0u8; FRAME_PREFIX_LEN];
        stream.read_exact(&mut prefix).await.expect("read prefix");
        let end_code = u16::from_le_bytes([prefix[13], prefix[14]]);
        let declared = u16::from_le_bytes([prefix[11], prefix[12]]) as usize;
        let mut payload = vec![0u8; declared.saturating_sub(2)];
        if !payload.is_empty() {
            stream
                .read_exact(&mut payload)
                .await
                .expect("read response payload");
        }
        (end_code, payload)
    }

    async fn connect(sim: &Simulator) -> TcpStream {
        TcpStream::connect(sim.addr).await.expect("connect")
    }

    #[tokio::test]
    async fn bulk_write_lands_consecutive_words_and_reads_back() {
        let sim = Simulator::start().await;
        let mut stream = connect(&sim).await;

        // Two consecutive words - the shape a 32-bit tag's write takes.
        let payload = [0x34, 0x12, 0x78, 0x56];
        let (end_code, body) = roundtrip(
            &mut stream,
            &bulk_command(COMMAND_BULK_WRITE, SlmpDevice::D, 100, 2, &payload),
        )
        .await;
        assert_eq!(end_code, 0, "a successful write answers end code 0");
        assert!(body.is_empty(), "a successful write carries no payload");
        assert_eq!(sim.get_word(SlmpDevice::D, 100), 0x1234);
        assert_eq!(sim.get_word(SlmpDevice::D, 101), 0x5678);
        assert_eq!(sim.write_command_count(), 1);

        let (end_code, body) = roundtrip(
            &mut stream,
            &bulk_command(COMMAND_BULK_READ, SlmpDevice::D, 100, 2, &[]),
        )
        .await;
        assert_eq!(end_code, 0);
        assert_eq!(body, vec![0x34, 0x12, 0x78, 0x56]);
    }

    #[tokio::test]
    async fn bulk_bit_write_lands_points_two_per_byte() {
        let sim = Simulator::start().await;
        let mut stream = connect(&sim).await;

        // M20 = on, M21 = off, M22 = on: the earlier point of each pair sits
        // in the high nibble.
        let payload = [0x10, 0x10];
        let (end_code, _) = roundtrip(
            &mut stream,
            &bulk_bit_command(COMMAND_BULK_WRITE, SlmpDevice::M, 20, 3, &payload),
        )
        .await;
        assert_eq!(end_code, 0);
        assert!(sim.get_bit(SlmpDevice::M, 20));
        assert!(!sim.get_bit(SlmpDevice::M, 21));
        assert!(sim.get_bit(SlmpDevice::M, 22));
    }

    #[tokio::test]
    async fn a_bit_unit_write_to_a_word_device_is_refused() {
        let sim = Simulator::start().await;
        let mut stream = connect(&sim).await;

        let (end_code, _) = roundtrip(
            &mut stream,
            &bulk_bit_command(COMMAND_BULK_WRITE, SlmpDevice::D, 0, 2, &[0x11]),
        )
        .await;
        assert_eq!(end_code, END_CODE_WRONG_COMMAND);
        assert_eq!(sim.held_count(), 0);
    }

    #[tokio::test]
    async fn a_truncated_write_payload_is_refused_without_landing() {
        let sim = Simulator::start().await;
        let mut stream = connect(&sim).await;

        // Claims two words but carries one.
        let (end_code, _) = roundtrip(
            &mut stream,
            &bulk_command(COMMAND_BULK_WRITE, SlmpDevice::D, 0, 2, &[0x01, 0x00]),
        )
        .await;
        assert_eq!(end_code, END_CODE_WRONG_COMMAND);
        assert_eq!(sim.get_word(SlmpDevice::D, 0), 0);
        assert_eq!(sim.held_count(), 0);
    }

    #[tokio::test]
    async fn an_injected_end_code_still_pre_empts_a_write() {
        let sim = Simulator::start().await;
        sim.inject_end_code(SlmpDevice::D, 7, 0xC051);
        let mut stream = connect(&sim).await;

        let (end_code, _) = roundtrip(
            &mut stream,
            &bulk_command(COMMAND_BULK_WRITE, SlmpDevice::D, 7, 1, &[0x99, 0x00]),
        )
        .await;
        assert_eq!(end_code, 0xC051);
        assert_eq!(sim.get_word(SlmpDevice::D, 7), 0, "the write must not land");
        assert_eq!(sim.held_count(), 0, "a refused write holds nothing");
    }

    #[tokio::test]
    async fn an_unknown_command_is_still_wrong_command() {
        let sim = Simulator::start().await;
        let mut stream = connect(&sim).await;

        let (end_code, _) =
            roundtrip(&mut stream, &bulk_command(0x1001, SlmpDevice::D, 0, 1, &[])).await;
        assert_eq!(end_code, END_CODE_WRONG_COMMAND);
    }

    #[tokio::test]
    async fn a_wire_written_device_is_held_against_the_seeding_setters() {
        let sim = Simulator::start().await;
        let mut stream = connect(&sim).await;

        roundtrip(
            &mut stream,
            &bulk_command(COMMAND_BULK_WRITE, SlmpDevice::D, 5, 1, &[0x92, 0x10]),
        )
        .await;
        roundtrip(
            &mut stream,
            &bulk_bit_command(COMMAND_BULK_WRITE, SlmpDevice::M, 3, 1, &[0x10]),
        )
        .await;
        assert_eq!(sim.held_count(), 2);

        // `banto-collect`'s ramp task drives exactly these setters.
        sim.set_word(SlmpDevice::D, 5, 1);
        sim.set_words(SlmpDevice::D, 4, &[7, 7, 7]);
        sim.set_bit(SlmpDevice::M, 3, false);
        assert_eq!(
            sim.get_word(SlmpDevice::D, 5),
            0x1092,
            "a held word keeps the written value"
        );
        assert!(sim.get_bit(SlmpDevice::M, 3), "a held bit keeps its value");

        // Neighbours are untouched by holding and keep ramping.
        assert_eq!(sim.get_word(SlmpDevice::D, 4), 7);
        assert_eq!(sim.get_word(SlmpDevice::D, 6), 7);
        sim.set_bit(SlmpDevice::M, 4, true);
        assert!(sim.get_bit(SlmpDevice::M, 4));
    }
}
