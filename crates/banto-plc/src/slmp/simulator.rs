//! In-process SLMP simulator: the MELSEC counterpart to
//! `modbus/simulator.rs`, feature-gated the same way and for the same reasons
//! (docs/plan.md I2 §6). Real MELSEC hardware cannot be a test dependency, and
//! the plan's own testing section (「テスト方針」) has this shared by I2a's
//! crate tests, I5's write client, and W3's engine integration tests rather
//! than each standing up its own fake CPU.
//!
//! Not an SLMP conformance tool: it implements exactly the one command this
//! crate's client issues - bulk read (`0x0401`), both bit-unit and word-unit -
//! keeps device state in plain `HashMap`s (sparse: any device never explicitly
//! set reads back as `0`/`false`, convenient for tests that care about a
//! handful of addresses), and can be told to return a canned end code, emit a
//! deliberately malformed frame, or hang instead of answering.
//!
//! ## Why this speaks real SLMP bytes rather than mocking the crate
//!
//! The alternative - a fake in place of `slmp::SLMPClient` behind a trait -
//! would have been less code, and would have tested nothing that matters. The
//! two things most likely to be wrong in `slmp/mod.rs` are (a) whether
//! [`super::classify_io_error`]'s reading of the wrapped crate's error
//! *messages* still holds, and (b) whether a bit-unit response's nibble
//! packing is decoded the way the crate expects. Both live strictly *inside*
//! the crate, so only real bytes on a real socket exercise them. That is also
//! what makes `slmp_end_code_is_bad_not_fatal` a working tripwire on the
//! dependency rather than a restatement of this crate's own assumptions.
//!
//! The frame layout implemented below is the 4E binary request/response pair as
//! the wrapped crate builds and validates it (`slmp::SLMPClient`'s
//! `create_subheader` / `validate_response`): a 15-byte prefix, then a 2-byte
//! end code, then payload.

use std::collections::HashMap;
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
    /// Exact `(device, start_number)` match -> end code to return instead of
    /// data, for injecting CPU-side refusals (the SLMP analogue of
    /// `modbus/simulator.rs`'s `exceptions`).
    end_codes: HashMap<(SlmpDevice, u32), u16>,
    /// When set, every response is emitted with a data-length field that
    /// disagrees with the bytes actually sent. Exists to exercise the *other*
    /// half of the wrapped crate's `InvalidData` errors: a framing failure,
    /// which must be classified connection-fatal even though it shares an
    /// `ErrorKind` with a perfectly recoverable end code.
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
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback listener");
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

        Simulator {
            addr,
            state,
            accept_task,
            connections,
        }
    }

    /// Set one word device (`D`/`W`/`R`/...). Panics on a bit device: writing a
    /// word to `M100` is a mistake in the *test*, not a condition worth
    /// simulating, and a silent no-op there would show up as a confusing
    /// all-zeros assertion failure much later.
    pub fn set_word(&self, device: SlmpDevice, number: u32, value: u16) {
        assert_eq!(
            device.access(),
            super::SlmpAccess::Word,
            "{device} is a bit device - use set_bit"
        );
        self.state
            .lock()
            .unwrap()
            .words
            .insert((device, number), value);
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
        for (i, chunk) in padded.chunks_exact(2).enumerate() {
            self.set_word(
                device,
                start + i as u32,
                u16::from_le_bytes([chunk[0], chunk[1]]),
            );
        }
    }

    /// Set one bit device (`M`/`X`/`Y`/...). Panics on a word device, mirroring
    /// [`Simulator::set_word`].
    pub fn set_bit(&self, device: SlmpDevice, number: u32, value: bool) {
        assert_eq!(
            device.access(),
            super::SlmpAccess::Bit,
            "{device} is a word device - use set_word"
        );
        self.state
            .lock()
            .unwrap()
            .bits
            .insert((device, number), value);
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

/// One parsed bulk read request.
struct BulkRead {
    device: SlmpDevice,
    start: u32,
    count: usize,
    bit_access: bool,
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

fn parse_bulk_read(command: &[u8]) -> Option<BulkRead> {
    if command.len() < 4 {
        return None;
    }
    let code = u16::from_le_bytes([command[0], command[1]]);
    if code != COMMAND_BULK_READ {
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

    Some(BulkRead {
        device,
        start,
        count,
        bit_access,
    })
}

fn build_response(state: &Arc<Mutex<State>>, route: &Route, command: &[u8]) -> Vec<u8> {
    let state = state.lock().unwrap();

    let Some(request) = parse_bulk_read(command) else {
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
