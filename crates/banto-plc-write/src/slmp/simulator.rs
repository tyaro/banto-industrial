//! In-process SLMP simulator that accepts **both** bulk write (`0x1401`) and
//! bulk read (`0x0401`), so a test can write through [`super::SlmpWriteClient`]
//! and then read the value back - proving the write actually landed, and with
//! the right bytes and word order - either through the same simulator's read
//! path or through `banto_plc::SlmpClient` pointed at it.
//!
//! ## Why this is self-contained rather than reusing banto-plc's simulator
//!
//! `banto-plc`'s simulator (`banto-plc/src/slmp/simulator.rs`) is read-only and
//! is not this crate's to edit. Its device-state model is exactly what a write
//! needs to mutate, but I5's brief is explicit that a small amount of
//! device-state duplication in a test double is acceptable rather than reaching
//! into another crate's internals. So this is a standalone fake CPU: it speaks
//! real SLMP 4E bytes (the whole point - the two things most likely to be wrong
//! in the write client are the wrapped crate's error-*message* classification
//! and the bit-unit nibble *packing*, both of which only real bytes exercise),
//! keeps state in plain `HashMap`s, and can inject an end code, a malformed
//! frame, or a hang.
//!
//! The frame layout is the 4E binary request/response pair as the wrapped
//! `slmp` crate builds and validates it: a 15-byte prefix, a 2-byte end code (on
//! responses), then payload.
//!
//! ## T8 additions (docs/tag-server-design.md §6.1, RMW bit-in-word writes)
//!
//! [`Simulator::write_command_count`]/[`Simulator::read_command_count`] let a
//! test assert on the *number* of wire operations an RMW issued (one write
//! however many bits were mask-composed into it; two reads - initial +
//! confirmation), and [`Simulator::corrupt_after_next_write`] deterministically
//! simulates the PLC-side race `execute_slmp_writes`'s confirmation read
//! exists to catch: a real "another controller wrote the same word between
//! our write and our confirmation read" race cannot be reproduced on demand
//! by timing alone, but its observable effect - the confirmation read
//! disagreeing with what was just written - can be, by having the simulator
//! itself apply the corruption at the one moment that matters (right after
//! the write it followed commits, before any later read observes it).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use banto_plc::{SlmpAccess, SlmpDevice};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

const FRAME_PREFIX_LEN: usize = 15;

const COMMAND_BULK_READ: u16 = 0x0401;
const COMMAND_BULK_WRITE: u16 = 0x1401;

const SUBCOMMAND_BIT_ACCESS: u16 = 0x0001;
const SUBCOMMAND_R_SERIES: u16 = 0x0002;

/// SLMP "wrong command", what a real CPU answers for a command it cannot make
/// sense of.
const END_CODE_WRONG_COMMAND: u16 = 0xC059;

#[derive(Debug, Default)]
struct State {
    words: HashMap<(SlmpDevice, u32), u16>,
    bits: HashMap<(SlmpDevice, u32), bool>,
    /// Exact `(device, start_number)` match -> end code returned instead of
    /// performing the request, for injecting CPU-side refusals on either a read
    /// or a write.
    end_codes: HashMap<(SlmpDevice, u32), u16>,
    malformed: bool,
    hang: bool,
    /// T8 (docs/tag-server-design.md §6.1) RMW test support: how many bulk
    /// read/write commands this simulator has served, for tests that assert
    /// a mask-composed RMW costs exactly one wire write (not one per bit) -
    /// see `write_command_count`/`read_command_count`.
    read_commands: usize,
    write_commands: usize,
    /// T8 RMW race simulation: a one-shot XOR mask applied to `(device,
    /// number)`'s word immediately after the *next* write that lands there
    /// completes - simulating a PLC scan clobbering a bit between our write
    /// and our confirmation read, deterministically rather than by racing
    /// real wall-clock timing. Consumed (removed) the first time it fires -
    /// see `corrupt_after_next_write`.
    corrupt_after_write: HashMap<(SlmpDevice, u32), u16>,
}

/// A running simulator instance. Dropping this does not stop the server; call
/// [`Simulator::stop`] to sever connections (used by the "PLC disconnected
/// mid-session" tests).
pub struct Simulator {
    pub addr: SocketAddr,
    state: Arc<Mutex<State>>,
    accept_task: JoinHandle<()>,
    connections: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl Simulator {
    /// Bind a loopback listener on an OS-assigned port and start accepting.
    /// Serves Q/L and R series interchangeably (series is read off each
    /// request's subcommand) and both read and write commands.
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
                    Err(_) => return,
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

    /// Pre-seed one word device (for read-back-baseline or read tests). Panics
    /// on a bit device, mirroring banto-plc's simulator.
    pub fn set_word(&self, device: SlmpDevice, number: u32, value: u16) {
        assert_eq!(
            device.access(),
            SlmpAccess::Word,
            "{device} is a bit device - use set_bit"
        );
        self.state
            .lock()
            .unwrap()
            .words
            .insert((device, number), value);
    }

    /// Pre-seed one bit device. Panics on a word device.
    pub fn set_bit(&self, device: SlmpDevice, number: u32, value: bool) {
        assert_eq!(
            device.access(),
            SlmpAccess::Bit,
            "{device} is a word device - use set_word"
        );
        self.state
            .lock()
            .unwrap()
            .bits
            .insert((device, number), value);
    }

    /// Inspect a word device's current state - how a test observes what a write
    /// landed. Unset devices read back as `0`.
    pub fn get_word(&self, device: SlmpDevice, number: u32) -> u16 {
        *self
            .state
            .lock()
            .unwrap()
            .words
            .get(&(device, number))
            .unwrap_or(&0)
    }

    /// Inspect a bit device's current state. Unset devices read back as `false`.
    pub fn get_bit(&self, device: SlmpDevice, number: u32) -> bool {
        *self
            .state
            .lock()
            .unwrap()
            .bits
            .get(&(device, number))
            .unwrap_or(&false)
    }

    /// Every request whose group *starts* at `(device, start_number)` gets this
    /// end code instead of being performed. Persists until cleared.
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

    /// How many bulk *write* commands this simulator has served since it
    /// started - T8 (docs/tag-server-design.md §6.1) test support for
    /// asserting an RMW with several composed bits still costs exactly one
    /// wire write, not one per bit.
    pub fn write_command_count(&self) -> usize {
        self.state.lock().unwrap().write_commands
    }

    /// How many bulk *read* commands this simulator has served - the T8 RMW
    /// twin of [`Self::write_command_count`] (an RMW issues exactly two:
    /// the initial read and the confirmation read).
    pub fn read_command_count(&self) -> usize {
        self.state.lock().unwrap().read_commands
    }

    /// T8 (docs/tag-server-design.md §6.1) RMW race test support: XOR
    /// `xor_mask` into `(device, number)`'s word immediately after the next
    /// write that lands there completes, simulating the PLC's own scan
    /// clobbering a bit between our write-back and our confirmation read.
    /// One-shot - consumed the first time a matching write occurs, so a test
    /// arms it once per RMW it wants to corrupt. Deterministic (no reliance
    /// on real timing), which is what makes
    /// `execute_slmp_writes`'s confirmation-read verification testable at
    /// all: a real PLC-scan race cannot be reproduced on demand, but its
    /// *observable effect* - the confirmation read disagreeing with what we
    /// just wrote - can.
    pub fn corrupt_after_next_write(&self, device: SlmpDevice, number: u32, xor_mask: u16) {
        self.state
            .lock()
            .unwrap()
            .corrupt_after_write
            .insert((device, number), xor_mask);
    }

    /// Answer with a frame whose declared data length disagrees with its actual
    /// payload, so the wrapped crate rejects it as a framing error.
    pub fn emit_malformed_frames(&self) {
        self.state.lock().unwrap().malformed = true;
    }

    /// Stop responding to any request until [`Simulator::stop_hanging`], for
    /// exercising the response timeout.
    pub fn hang(&self) {
        self.state.lock().unwrap().hang = true;
    }

    pub fn stop_hanging(&self) {
        self.state.lock().unwrap().hang = false;
    }

    /// Stop accepting new connections and sever every open one, simulating a
    /// CPU power-cycle or network drop mid-session.
    pub fn stop(self) {
        self.accept_task.abort();
        for handle in self.connections.lock().unwrap().drain(..) {
            handle.abort();
        }
    }
}

/// Reverse of [`super::device_to_wire`]: recover the device from its wire code.
fn device_from_wire_code(code: u8) -> Option<SlmpDevice> {
    SlmpDevice::all()
        .iter()
        .copied()
        .find(|d| super::device_to_wire(*d).to_code() == code)
}

/// The access-route fields a response must echo back verbatim; the wrapped
/// crate checks every one and rejects a mismatch.
struct Route {
    serial_id: u16,
    network_id: u8,
    pc_id: u8,
    io_id: u16,
    area_id: u8,
}

async fn handle_connection(mut stream: TcpStream, state: Arc<Mutex<State>>) {
    loop {
        let mut prefix = [0u8; FRAME_PREFIX_LEN];
        if stream.read_exact(&mut prefix).await.is_err() {
            return;
        }
        let route = Route {
            serial_id: u16::from_le_bytes([prefix[2], prefix[3]]),
            network_id: prefix[6],
            pc_id: prefix[7],
            io_id: u16::from_le_bytes([prefix[8], prefix[9]]),
            area_id: prefix[10],
        };
        let declared_len = u16::from_le_bytes([prefix[11], prefix[12]]) as usize;
        let command_len = declared_len.saturating_sub(2);

        let mut command = vec![0u8; command_len];
        if command_len > 0 && stream.read_exact(&mut command).await.is_err() {
            return;
        }

        if state.lock().unwrap().hang {
            std::future::pending::<()>().await;
        }

        let response = build_response(&state, &route, &command);
        if stream.write_all(&response).await.is_err() {
            return;
        }
    }
}

/// Parsed device field common to read and write commands: 3 address bytes plus
/// a device code, positioned per CPU series.
struct DeviceField {
    device: SlmpDevice,
    start: u32,
    bit_access: bool,
    /// Byte offset just past the device field, where the point count begins.
    after: usize,
}

fn parse_device_field(command: &[u8]) -> Option<DeviceField> {
    if command.len() < 4 {
        return None;
    }
    let subcommand = u16::from_le_bytes([command[2], command[3]]);
    let bit_access = subcommand & SUBCOMMAND_BIT_ACCESS != 0;
    let r_series = subcommand & SUBCOMMAND_R_SERIES != 0;

    let (device_field_len, device_code_index) = if r_series { (6, 4) } else { (4, 3) };
    let body = command.get(4..)?;
    if body.len() < device_field_len {
        return None;
    }
    let start = u32::from_le_bytes([body[0], body[1], body[2], 0]);
    let device = device_from_wire_code(body[device_code_index])?;
    Some(DeviceField {
        device,
        start,
        bit_access,
        after: 4 + device_field_len,
    })
}

fn build_response(state: &Arc<Mutex<State>>, route: &Route, command: &[u8]) -> Vec<u8> {
    let mut state = state.lock().unwrap();

    if command.len() < 2 {
        return frame(route, END_CODE_WRONG_COMMAND, &[], state.malformed);
    }
    let code = u16::from_le_bytes([command[0], command[1]]);

    match code {
        COMMAND_BULK_READ => {
            state.read_commands += 1;
            build_read_response(&state, route, command)
        }
        COMMAND_BULK_WRITE => {
            state.write_commands += 1;
            build_write_response(&mut state, route, command)
        }
        _ => frame(route, END_CODE_WRONG_COMMAND, &[], state.malformed),
    }
}

fn build_read_response(state: &State, route: &Route, command: &[u8]) -> Vec<u8> {
    let Some(field) = parse_device_field(command) else {
        return frame(route, END_CODE_WRONG_COMMAND, &[], state.malformed);
    };
    // Point count sits just past the device field.
    let Some(count_bytes) = command.get(field.after..field.after + 2) else {
        return frame(route, END_CODE_WRONG_COMMAND, &[], state.malformed);
    };
    let count = u16::from_le_bytes([count_bytes[0], count_bytes[1]]) as usize;

    if let Some(&end) = state.end_codes.get(&(field.device, field.start)) {
        return frame(route, end, &[], state.malformed);
    }

    let expects_bit = field.device.access() == SlmpAccess::Bit;
    if expects_bit != field.bit_access {
        return frame(route, END_CODE_WRONG_COMMAND, &[], state.malformed);
    }

    let payload = if field.bit_access {
        let mut bytes = Vec::with_capacity(count.div_ceil(2));
        for pair_index in 0..count.div_ceil(2) {
            let mut byte = 0u8;
            for (nibble, shift) in [(0usize, 4u32), (1, 0)] {
                let point = pair_index * 2 + nibble;
                if point < count {
                    let number = field.start + point as u32;
                    if *state.bits.get(&(field.device, number)).unwrap_or(&false) {
                        byte |= 1 << shift;
                    }
                }
            }
            bytes.push(byte);
        }
        bytes
    } else {
        let mut bytes = Vec::with_capacity(count * 2);
        for i in 0..count {
            let number = field.start + i as u32;
            let word = *state.words.get(&(field.device, number)).unwrap_or(&0);
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        bytes
    };

    frame(route, 0, &payload, state.malformed)
}

fn build_write_response(state: &mut State, route: &Route, command: &[u8]) -> Vec<u8> {
    let Some(field) = parse_device_field(command) else {
        return frame(route, END_CODE_WRONG_COMMAND, &[], state.malformed);
    };
    let Some(count_bytes) = command.get(field.after..field.after + 2) else {
        return frame(route, END_CODE_WRONG_COMMAND, &[], state.malformed);
    };
    let count = u16::from_le_bytes([count_bytes[0], count_bytes[1]]) as usize;
    let data = &command[field.after + 2..];

    if let Some(&end) = state.end_codes.get(&(field.device, field.start)) {
        return frame(route, end, &[], state.malformed);
    }

    let expects_bit = field.device.access() == SlmpAccess::Bit;
    if expects_bit != field.bit_access {
        return frame(route, END_CODE_WRONG_COMMAND, &[], state.malformed);
    }

    if field.bit_access {
        // Bit-unit payload: two points per byte, earlier point in the high
        // nibble (the layout the wrapped crate emits:
        // `(x[1]) + ((x[0]) << 4)`), device_size_code = point count.
        let needed = count.div_ceil(2);
        if data.len() < needed {
            return frame(route, END_CODE_WRONG_COMMAND, &[], state.malformed);
        }
        for point in 0..count {
            let byte = data[point / 2];
            let bit = if point % 2 == 0 {
                (byte >> 4) & 0x01
            } else {
                byte & 0x01
            };
            let number = field.start + point as u32;
            state.bits.insert((field.device, number), bit == 1);
        }
    } else {
        // Word-unit payload: `count` little-endian words back to back.
        if data.len() < count * 2 {
            return frame(route, END_CODE_WRONG_COMMAND, &[], state.malformed);
        }
        for i in 0..count {
            let word = u16::from_le_bytes([data[i * 2], data[i * 2 + 1]]);
            let number = field.start + i as u32;
            state.words.insert((field.device, number), word);

            // T8 (docs/tag-server-design.md §6.1) RMW race simulation: if a
            // test armed a corruption for this exact (device, number) via
            // `corrupt_after_next_write`, apply it now - immediately after
            // committing the caller's own write, one-shot - so the *next*
            // read (the RMW's confirmation read) observes a word that no
            // longer matches what was just written, exactly as a PLC scan
            // landing between our write and our confirmation read would.
            if let Some(xor_mask) = state.corrupt_after_write.remove(&(field.device, number)) {
                let corrupted = word ^ xor_mask;
                state.words.insert((field.device, number), corrupted);
            }
        }
    }

    // A successful write returns end code 0 and no payload.
    frame(route, 0, &[], state.malformed)
}

/// Assemble a 4E binary response frame. `end_code` of `0` means success.
/// `malformed` corrupts the declared data length so the wrapped crate's
/// length-consistency check fails.
fn frame(route: &Route, end_code: u16, payload: &[u8], malformed: bool) -> Vec<u8> {
    const RESPONSE_CODE: [u8; 2] = [0xD4, 0x00];
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
