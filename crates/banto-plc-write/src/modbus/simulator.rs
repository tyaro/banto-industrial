//! In-process Modbus TCP simulator that accepts write commands (FC5/6/15/16)
//! **and** read commands (FC1/3), so a test can write through
//! [`super::ModbusWriteClient`] and then read the value back - proving the
//! write actually landed, and with the right bytes and word order.
//!
//! ## Why this is self-contained rather than reusing banto-plc's simulator
//!
//! Same call as `crate::slmp::simulator`'s (see that module's doc comment):
//! `banto-plc`'s Modbus simulator (`banto-plc/src/modbus/simulator.rs`) is
//! read-only and is not this crate's to edit. Its device-state model is
//! exactly what a write needs to mutate, but duplicating a small amount of
//! device state in a test double is cheaper than reaching into another
//! crate's internals. Unlike the SLMP write simulator, though, this one
//! *does* reuse `banto_plc::modbus::frame`'s now-`pub` wire-format helpers
//! ([`banto_plc::modbus::frame::wrap_mbap`],
//! [`banto_plc::modbus::frame::encode_bits_payload`],
//! [`banto_plc::modbus::frame::encode_registers_payload`]) for building
//! responses - those are exactly the same "public wire format" pieces #131
//! exposed for the real client, so there is no reason for a test double to
//! hand-roll them a second time. What is NOT reused is *request decoding*:
//! parsing an incoming FC5/6/15/16 request PDU is server-only logic with no
//! client-side equivalent to share.
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use banto_plc::modbus::frame::{encode_bits_payload, encode_registers_payload, wrap_mbap};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

const MBAP_HEADER_LEN: usize = 7;
const EXCEPTION_FLAG: u8 = 0x80;

const FC_WRITE_SINGLE_COIL: u8 = 0x05;
const FC_WRITE_SINGLE_REGISTER: u8 = 0x06;
const FC_READ_HOLDING_REGISTERS: u8 = 0x03;
const FC_READ_COILS: u8 = 0x01;
const FC_WRITE_MULTIPLE_COILS: u8 = 0x0F;
const FC_WRITE_MULTIPLE_REGISTERS: u8 = 0x10;

#[derive(Debug, Default)]
struct State {
    coils: HashMap<u16, bool>,
    holding_registers: HashMap<u16, u16>,
    /// Exact `(function_code, start_offset)` match -> exception code to
    /// return instead of performing the write/read.
    exceptions: HashMap<(u8, u16), u8>,
    malformed: bool,
    hang: bool,
    /// How many wire *write* commands (FC5/6/15/16) this simulator has
    /// served - test support for asserting single-vs-multiple function code
    /// selection and that adjacent requests cost exactly one wire write.
    write_commands: usize,
}

/// A running simulator instance. Dropping this does not stop the server;
/// call [`Simulator::stop`] to sever connections.
pub struct Simulator {
    pub addr: SocketAddr,
    state: Arc<Mutex<State>>,
    accept_task: JoinHandle<()>,
    connections: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl Simulator {
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

    pub fn set_holding_register(&self, offset: u16, value: u16) {
        self.state
            .lock()
            .unwrap()
            .holding_registers
            .insert(offset, value);
    }

    pub fn get_holding_register(&self, offset: u16) -> u16 {
        *self
            .state
            .lock()
            .unwrap()
            .holding_registers
            .get(&offset)
            .unwrap_or(&0)
    }

    pub fn set_coil(&self, offset: u16, value: bool) {
        self.state.lock().unwrap().coils.insert(offset, value);
    }

    pub fn get_coil(&self, offset: u16) -> bool {
        *self
            .state
            .lock()
            .unwrap()
            .coils
            .get(&offset)
            .unwrap_or(&false)
    }

    /// The next request matching `(function, start_offset)` exactly gets this
    /// exception code instead of being performed. Persists until
    /// [`Simulator::clear_exception`].
    pub fn inject_exception(&self, function: u8, start_offset: u16, code: u8) {
        self.state
            .lock()
            .unwrap()
            .exceptions
            .insert((function, start_offset), code);
    }

    pub fn clear_exception(&self, function: u8, start_offset: u16) {
        self.state
            .lock()
            .unwrap()
            .exceptions
            .remove(&(function, start_offset));
    }

    /// How many wire write commands this simulator has served - test support
    /// for asserting adjacent requests merge into one wire operation.
    pub fn write_command_count(&self) -> usize {
        self.state.lock().unwrap().write_commands
    }

    /// Answer with a frame whose declared length disagrees with its actual
    /// payload, so the client rejects it as a framing error.
    pub fn emit_malformed_frames(&self) {
        self.state.lock().unwrap().malformed = true;
    }

    /// Stop responding to any request until [`Simulator::stop_hanging`], for
    /// exercising the client's response timeout.
    pub fn hang(&self) {
        self.state.lock().unwrap().hang = true;
    }

    pub fn stop_hanging(&self) {
        self.state.lock().unwrap().hang = false;
    }

    /// Stop accepting new connections and sever every open one, simulating a
    /// PLC power-cycle/network drop mid-session.
    pub fn stop(self) {
        self.accept_task.abort();
        for handle in self.connections.lock().unwrap().drain(..) {
            handle.abort();
        }
    }
}

async fn handle_connection(mut stream: TcpStream, state: Arc<Mutex<State>>) {
    loop {
        let mut header_buf = [0u8; MBAP_HEADER_LEN];
        if stream.read_exact(&mut header_buf).await.is_err() {
            return;
        }
        let transaction_id = u16::from_be_bytes([header_buf[0], header_buf[1]]);
        let unit_id = header_buf[6];
        let length = u16::from_be_bytes([header_buf[4], header_buf[5]]);
        let pdu_len = (length as usize).saturating_sub(1);

        let mut pdu = vec![0u8; pdu_len];
        if pdu_len > 0 && stream.read_exact(&mut pdu).await.is_err() {
            return;
        }

        if state.lock().unwrap().hang {
            std::future::pending::<()>().await;
        }

        let response = build_response(&state, transaction_id, unit_id, &pdu);
        if stream.write_all(&response).await.is_err() {
            return;
        }
    }
}

fn build_response(
    state: &Arc<Mutex<State>>,
    transaction_id: u16,
    unit_id: u8,
    pdu: &[u8],
) -> Vec<u8> {
    let mut state = state.lock().unwrap();
    let malformed = state.malformed;

    let response_pdu = match pdu.first() {
        None => Err((0x01u8, 0x01u8)),
        Some(&function) => match function {
            FC_WRITE_SINGLE_COIL | FC_WRITE_SINGLE_REGISTER => {
                state.write_commands += 1;
                handle_single_write(&mut state, function, pdu)
            }
            FC_WRITE_MULTIPLE_COILS | FC_WRITE_MULTIPLE_REGISTERS => {
                state.write_commands += 1;
                handle_multiple_write(&mut state, function, pdu)
            }
            FC_READ_COILS => handle_read_coils(&state, pdu),
            FC_READ_HOLDING_REGISTERS => handle_read_holding_registers(&state, pdu),
            other => Err((other, 0x01)), // illegal function
        },
    };

    let mut frame = match response_pdu {
        Ok(response) => wrap_mbap(transaction_id, unit_id, &response),
        Err((func, code)) => wrap_mbap(transaction_id, unit_id, &[func | EXCEPTION_FLAG, code]),
    };
    if malformed {
        // Corrupt the MBAP length field so the client's length-consistency
        // check fails - byte 5 is the low byte of the big-endian length.
        let len_idx = 5;
        frame[len_idx] = frame[len_idx].wrapping_add(1);
    }
    frame
}

/// FC5 (single coil) / FC6 (single register): request and success response
/// share one shape, `function + address(2) + value(2)` - a successful write
/// simply echoes the request PDU verbatim.
fn handle_single_write(state: &mut State, function: u8, pdu: &[u8]) -> Result<Vec<u8>, (u8, u8)> {
    if pdu.len() != 5 {
        return Err((function, 0x03)); // illegal data value
    }
    let address = u16::from_be_bytes([pdu[1], pdu[2]]);
    let value = u16::from_be_bytes([pdu[3], pdu[4]]);

    if let Some(&code) = state.exceptions.get(&(function, address)) {
        return Err((function, code));
    }

    if function == FC_WRITE_SINGLE_COIL {
        state.coils.insert(address, value == 0xFF00);
    } else {
        state.holding_registers.insert(address, value);
    }
    Ok(pdu.to_vec())
}

/// FC15 (multiple coils) / FC16 (multiple registers): request is
/// `function + start(2) + quantity(2) + byte_count(1) + data`; success
/// response is `function + start(2) + quantity(2)` (no byte count/data).
fn handle_multiple_write(state: &mut State, function: u8, pdu: &[u8]) -> Result<Vec<u8>, (u8, u8)> {
    if pdu.len() < 6 {
        return Err((function, 0x03));
    }
    let start = u16::from_be_bytes([pdu[1], pdu[2]]);
    let quantity = u16::from_be_bytes([pdu[3], pdu[4]]);
    let byte_count = pdu[5] as usize;
    let data = &pdu[6..];
    if data.len() < byte_count {
        return Err((function, 0x03));
    }

    if let Some(&code) = state.exceptions.get(&(function, start)) {
        return Err((function, code));
    }

    if function == FC_WRITE_MULTIPLE_COILS {
        for i in 0..quantity {
            let byte = data[(i / 8) as usize];
            let bit = (byte >> (i % 8)) & 1 == 1;
            state.coils.insert(start + i, bit);
        }
    } else {
        for i in 0..quantity {
            let idx = i as usize * 2;
            let word = u16::from_be_bytes([data[idx], data[idx + 1]]);
            state.holding_registers.insert(start + i, word);
        }
    }

    let mut response = Vec::with_capacity(5);
    response.push(function);
    response.extend_from_slice(&start.to_be_bytes());
    response.extend_from_slice(&quantity.to_be_bytes());
    Ok(response)
}

fn handle_read_coils(state: &State, pdu: &[u8]) -> Result<Vec<u8>, (u8, u8)> {
    if pdu.len() != 5 {
        return Err((FC_READ_COILS, 0x03));
    }
    let start = u16::from_be_bytes([pdu[1], pdu[2]]);
    let quantity = u16::from_be_bytes([pdu[3], pdu[4]]);
    let bits: Vec<bool> = (0..quantity)
        .map(|i| *state.coils.get(&(start + i)).unwrap_or(&false))
        .collect();
    let packed = encode_bits_payload(&bits);
    let mut response = Vec::with_capacity(2 + packed.len());
    response.push(FC_READ_COILS);
    response.push(packed.len() as u8);
    response.extend_from_slice(&packed);
    Ok(response)
}

fn handle_read_holding_registers(state: &State, pdu: &[u8]) -> Result<Vec<u8>, (u8, u8)> {
    if pdu.len() != 5 {
        return Err((FC_READ_HOLDING_REGISTERS, 0x03));
    }
    let start = u16::from_be_bytes([pdu[1], pdu[2]]);
    let quantity = u16::from_be_bytes([pdu[3], pdu[4]]);
    let regs: Vec<u16> = (0..quantity)
        .map(|i| *state.holding_registers.get(&(start + i)).unwrap_or(&0))
        .collect();
    let packed = encode_registers_payload(&regs);
    let mut response = Vec::with_capacity(2 + packed.len());
    response.push(FC_READ_HOLDING_REGISTERS);
    response.push(packed.len() as u8);
    response.extend_from_slice(&packed);
    Ok(response)
}
