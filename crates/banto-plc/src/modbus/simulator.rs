//! In-process Modbus TCP simulator (docs/plan.md I2 §6): a minimal server
//! good enough to drive this crate's own integration tests against a real
//! socket instead of hand-decoded byte arrays, and public (behind the
//! `simulator` feature) so I3's later integration tests and R4's 72-hour
//! soak-test harness (docs/recorder-requirements.md §4) can reuse it rather
//! than each standing up their own fake PLC.
//!
//! Not a Modbus conformance test tool: it implements exactly the function
//! codes this repository's clients issue - the four read codes (FC1-4) and,
//! since #363, the four write codes (FC5/6/15/16) - keeps register/coil
//! state in a plain `HashMap` (sparse - any address never explicitly set
//! reads back as `0`/`false`, which is a convenient default for tests that
//! only care about a handful of addresses), and can be told to return a
//! canned exception code or hang instead of answering, for exercising the
//! client's error paths deterministically.
//!
//! ## Write support and the "held" set (#363、2026-09-15 オーナー決定)
//!
//! Until #363 this simulator answered every write function code with
//! `0x01 illegal function`, which made `simulation = true` connections
//! read-only at the wire level and forced banto-hub's write path to reject
//! simulated writes outright (its old gate 4,
//! `WriteRejection::SimulationWriteRejected`). The owner decision of
//! 2026-09-15 is that an external write to a PC-side simulated device must
//! be *applied to the simulator* instead of refused, so a SCADA client can
//! exercise the whole write path with no real PLC present. The wire-level
//! halves of that decision live here: FC5 (write single coil), FC6 (write
//! single register), FC15 (write multiple coils) and FC16 (write multiple
//! registers), with the same response shapes and exception codes
//! `crates/banto-plc-write/src/modbus/simulator.rs` (the dev-only write
//! double this was ported from) already used.
//!
//! The other half is **holding**: `banto-collect`'s
//! `simulation::modbus_ramp_task` overwrites the first
//! `RAMP_ADDRESS_COUNT` addresses of every table every 100 ms, so a value
//! written over the wire would be erased before the next poll could read it
//! back. Any coil or holding register written *over the wire* is therefore
//! recorded in a held set, and the seeding setters
//! ([`Simulator::set_coil`], [`Simulator::set_holding_register`],
//! [`Simulator::set_holding_registers`]) become no-ops for a held address -
//! that is, the ramp stops touching exactly the addresses an operator has
//! taken over, and leaves every other address ramping as before. Reads
//! (FC1-4) and the seeding setters share one `State`, so the next poll (or
//! a `read-now`) observes the written value.
//!
//! Holding is deliberately per-*table*: only the coil and holding-register
//! tables are writable by the Modbus wire protocol at all, so discrete
//! inputs (`1xxxx`) and input registers (`3xxxx`) never become held and keep
//! ramping - which is exactly what a real device does. The held set lives
//! and dies with the simulator instance, so toggling a connection's
//! `simulation` flag or stopping all-simulation mode (both of which make
//! banto-hub's `SlmpSimRegistry` build a fresh simulator) clears it.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

use super::frame::{
    build_data_response_frame, build_exception_response_frame, encode_bits_payload,
    encode_registers_payload, wrap_mbap, FC_READ_COILS, FC_READ_DISCRETE_INPUTS,
    FC_READ_HOLDING_REGISTERS, FC_READ_INPUT_REGISTERS, MBAP_HEADER_LEN,
};

/// Write function codes (#363). Deliberately declared here rather than in
/// `frame.rs`: that module's `FC_READ_*` list describes what this crate's
/// *read* client emits, and mixing write codes into it would blur the
/// "banto-plc is read-only" boundary its own doc comment draws. Only this
/// test double speaks them.
const FC_WRITE_SINGLE_COIL: u8 = 0x05;
const FC_WRITE_SINGLE_REGISTER: u8 = 0x06;
const FC_WRITE_MULTIPLE_COILS: u8 = 0x0F;
const FC_WRITE_MULTIPLE_REGISTERS: u8 = 0x10;

/// Modbus exception codes this simulator produces for a malformed or
/// out-of-range write request (Modbus Application Protocol spec §7).
const EXCEPTION_ILLEGAL_FUNCTION: u8 = 0x01;
const EXCEPTION_ILLEGAL_DATA_ADDRESS: u8 = 0x02;
const EXCEPTION_ILLEGAL_DATA_VALUE: u8 = 0x03;

/// Write quantity caps (Modbus Application Protocol spec §6.11-6.12): FC15
/// carries at most 1968 coils, FC16 at most 123 registers. Over-cap requests
/// get `0x03 illegal data value`, the same as a zero quantity.
const MAX_WRITE_COILS: u16 = 1968;
const MAX_WRITE_REGISTERS: u16 = 123;

#[derive(Debug, Default)]
struct State {
    coils: HashMap<u16, bool>,
    discrete_inputs: HashMap<u16, bool>,
    holding_registers: HashMap<u16, u16>,
    input_registers: HashMap<u16, u16>,
    /// Coil addresses written over the wire (FC5/FC15) - see this module's
    /// "Write support and the held set" section. [`Simulator::set_coil`] is
    /// a no-op for these, so the ramp task stops overwriting them.
    held_coils: HashSet<u16>,
    /// Holding-register addresses written over the wire (FC6/FC16), the
    /// register twin of `held_coils`.
    held_holding_registers: HashSet<u16>,
    /// Exact `(function_code, start_offset)` match -> exception code to
    /// return instead of data, for injecting device-side failures
    /// (docs/plan.md I2 §6).
    exceptions: HashMap<(u8, u16), u8>,
    /// When set, every connection's request handling stalls forever instead
    /// of responding - used to exercise the client's response timeout
    /// without needing a real unreachable host.
    hang: bool,
    /// How many wire *write* commands (FC5/6/15/16) have been served - test
    /// support, exposed as [`Simulator::write_command_count`].
    write_commands: usize,
}

/// A running simulator instance. Dropping this does *not* stop the server
/// (the accept/handler tasks keep running detached, same as any
/// `tokio::spawn`) - call [`Simulator::stop`] to shut it down explicitly,
/// which is what closes every client socket and is how tests exercise the
/// "PLC disconnected mid-session" path.
pub struct Simulator {
    pub addr: SocketAddr,
    state: Arc<Mutex<State>>,
    accept_task: JoinHandle<()>,
    /// One entry per connection accepted so far (including already-closed
    /// ones, which is harmless - aborting a finished task is a no-op). Kept
    /// so [`Simulator::stop`] can sever *live* connections too, not just
    /// stop accepting new ones - without this, a client that connected
    /// before `stop()` would keep chatting with its already-spawned handler
    /// task forever, and the "PLC disconnected mid-session" test path would
    /// have nothing to observe.
    connections: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl Simulator {
    /// Bind a loopback listener on an OS-assigned port and start accepting
    /// connections. Supports multiple concurrent/sequential connections
    /// (each handled by its own spawned task).
    pub async fn start() -> Self {
        Self::start_on("127.0.0.1:0".parse().expect("valid loopback address"))
            .await
            .expect("bind loopback listener")
    }

    /// As [`Simulator::start`], but binds `addr` instead of letting the OS
    /// pick - so a test can **bring the same PLC back on the same port**
    /// after a [`Simulator::stop`], which is what an outage-then-recovery
    /// test needs (a client that reconnects must find the device where it
    /// left it; a fresh OS-assigned port would instead look like a different
    /// device).
    ///
    /// Returns the bind error rather than panicking, because that is a state
    /// a caller legitimately has to retry through: the previous instance's
    /// severed connections can still hold the port for a short window after
    /// `stop()`, so a restart loop should retry on `AddrInUse` for a second
    /// or two rather than fail the test. Added for banto-hub #344
    /// (2026-09-15), whose E2E test stops the simulator, asserts no bogus
    /// `plc_reconnected` while it is down, then restarts it here and asserts
    /// exactly one once it is back.
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

    /// Seed one coil. **No-op if the coil is held** (it was written over the
    /// wire) - see this module's "Write support and the held set" section:
    /// this is what stops `banto-collect`'s ramp task from erasing a value an
    /// external client just wrote.
    pub fn set_coil(&self, offset: u16, value: bool) {
        let mut state = self.state.lock().unwrap();
        if state.held_coils.contains(&offset) {
            return;
        }
        state.coils.insert(offset, value);
    }

    /// Seed one discrete input. Never held: `1xxxx` is read-only on the wire
    /// for every Modbus device, so nothing can take it over.
    pub fn set_discrete_input(&self, offset: u16, value: bool) {
        self.state
            .lock()
            .unwrap()
            .discrete_inputs
            .insert(offset, value);
    }

    /// Seed one holding register. **No-op if the register is held** - see
    /// [`Simulator::set_coil`].
    pub fn set_holding_register(&self, offset: u16, value: u16) {
        let mut state = self.state.lock().unwrap();
        if state.held_holding_registers.contains(&offset) {
            return;
        }
        state.holding_registers.insert(offset, value);
    }

    /// Seed consecutive holding registers, skipping any held one
    /// individually (a partially-held run leaves the held members alone and
    /// seeds the rest).
    pub fn set_holding_registers(&self, start_offset: u16, values: &[u16]) {
        let mut state = self.state.lock().unwrap();
        for (i, &v) in values.iter().enumerate() {
            let offset = start_offset + i as u16;
            if state.held_holding_registers.contains(&offset) {
                continue;
            }
            state.holding_registers.insert(offset, v);
        }
    }

    /// Seed one input register. Never held (`3xxxx` is read-only on the
    /// wire) - see [`Simulator::set_discrete_input`].
    pub fn set_input_register(&self, offset: u16, value: u16) {
        self.state
            .lock()
            .unwrap()
            .input_registers
            .insert(offset, value);
    }

    /// Current value of one coil, as a read (FC1) would see it. Unset coils
    /// read back as `false`.
    pub fn get_coil(&self, offset: u16) -> bool {
        *self
            .state
            .lock()
            .unwrap()
            .coils
            .get(&offset)
            .unwrap_or(&false)
    }

    /// Current value of one holding register, as a read (FC3) would see it.
    /// Unset registers read back as `0`.
    pub fn get_holding_register(&self, offset: u16) -> u16 {
        *self
            .state
            .lock()
            .unwrap()
            .holding_registers
            .get(&offset)
            .unwrap_or(&0)
    }

    /// How many addresses (coils plus holding registers) have been written
    /// over the wire and are therefore excluded from the seeding setters -
    /// the observation API for the held-set behaviour described in this
    /// module's doc comment.
    pub fn held_count(&self) -> usize {
        let state = self.state.lock().unwrap();
        state.held_coils.len() + state.held_holding_registers.len()
    }

    /// How many wire write commands (FC5/6/15/16) this simulator has served.
    pub fn write_command_count(&self) -> usize {
        self.state.lock().unwrap().write_commands
    }

    /// The next request matching `(function, start_offset)` exactly will get
    /// this exception code instead of data. Persists across requests (not
    /// one-shot) - call [`Simulator::clear_exception`] to remove it.
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

    /// Stop responding to any request on any connection (existing or
    /// future) until [`Simulator::stop_hanging`] is called - for exercising
    /// the client's response timeout.
    pub fn hang(&self) {
        self.state.lock().unwrap().hang = true;
    }

    pub fn stop_hanging(&self) {
        self.state.lock().unwrap().hang = false;
    }

    /// How many connections this simulator has accepted in total, including
    /// ones that have since closed - the `connections` field is append-only
    /// (see its own doc comment), so this counts sockets ever accepted, not
    /// sockets currently open.
    ///
    /// Exists for banto-hub's #337 regression test (2026-09-08): a Modbus
    /// connection must occupy exactly **one** socket while collecting, since
    /// some real Modbus/TCP servers (オムロン KM-D1-ETN) accept only a single
    /// client connection and drop any second one immediately. Counting
    /// cumulative accepts is what makes that assertion meaningful - a
    /// second, immediately-closed socket still shows up here.
    pub fn connection_count(&self) -> usize {
        self.connections.lock().unwrap().len()
    }

    /// Stop accepting new connections and sever every connection already
    /// open, simulating a PLC power-cycle/network drop mid-session.
    /// Aborting each handler task drops its `TcpStream`, which closes the
    /// socket - the connected client observes this as an I/O error/EOF on
    /// its next read or write.
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
            return; // client closed the connection
        }
        let transaction_id = u16::from_be_bytes([header_buf[0], header_buf[1]]);
        let length = u16::from_be_bytes([header_buf[4], header_buf[5]]);
        let unit_id = header_buf[6];

        let pdu_len = (length as usize).saturating_sub(1);
        let mut pdu = vec![0u8; pdu_len];
        if pdu_len > 0 && stream.read_exact(&mut pdu).await.is_err() {
            return;
        }
        if pdu.is_empty() {
            return; // malformed request (real client never sends this)
        }

        if state.lock().unwrap().hang {
            // Never resolves - the client's own response-timeout budget is
            // what ends this, not us.
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
    let function = pdu[0];

    match function {
        FC_READ_COILS
        | FC_READ_DISCRETE_INPUTS
        | FC_READ_HOLDING_REGISTERS
        | FC_READ_INPUT_REGISTERS => build_read_response(&state, transaction_id, unit_id, pdu),
        FC_WRITE_SINGLE_COIL | FC_WRITE_SINGLE_REGISTER => {
            state.write_commands += 1;
            build_single_write_response(&mut state, transaction_id, unit_id, pdu)
        }
        FC_WRITE_MULTIPLE_COILS | FC_WRITE_MULTIPLE_REGISTERS => {
            state.write_commands += 1;
            build_multiple_write_response(&mut state, transaction_id, unit_id, pdu)
        }
        _ => build_exception_response_frame(
            transaction_id,
            unit_id,
            function,
            EXCEPTION_ILLEGAL_FUNCTION,
        ),
    }
}

/// FC1-4. Request shape `function + start(2) + quantity(2)`, response
/// `function + byte_count(1) + data`.
fn build_read_response(state: &State, transaction_id: u16, unit_id: u8, pdu: &[u8]) -> Vec<u8> {
    let function = pdu[0];
    if pdu.len() < 5 {
        return build_exception_response_frame(
            transaction_id,
            unit_id,
            function,
            EXCEPTION_ILLEGAL_DATA_VALUE,
        );
    }
    let start_offset = u16::from_be_bytes([pdu[1], pdu[2]]);
    let quantity = u16::from_be_bytes([pdu[3], pdu[4]]);

    if let Some(&code) = state.exceptions.get(&(function, start_offset)) {
        return build_exception_response_frame(transaction_id, unit_id, function, code);
    }
    if let Some(code) = range_exception(start_offset, quantity, u16::MAX) {
        return build_exception_response_frame(transaction_id, unit_id, function, code);
    }

    match function {
        FC_READ_COILS | FC_READ_DISCRETE_INPUTS => {
            let table = if function == FC_READ_COILS {
                &state.coils
            } else {
                &state.discrete_inputs
            };
            let bits: Vec<bool> = (0..quantity)
                .map(|i| *table.get(&(start_offset + i)).unwrap_or(&false))
                .collect();
            let payload = encode_bits_payload(&bits);
            build_data_response_frame(transaction_id, unit_id, function, &payload)
        }
        _ => {
            let table = if function == FC_READ_HOLDING_REGISTERS {
                &state.holding_registers
            } else {
                &state.input_registers
            };
            let regs: Vec<u16> = (0..quantity)
                .map(|i| *table.get(&(start_offset + i)).unwrap_or(&0))
                .collect();
            let payload = encode_registers_payload(&regs);
            build_data_response_frame(transaction_id, unit_id, function, &payload)
        }
    }
}

/// FC5 (single coil) / FC6 (single register): request and success response
/// share one shape, `function + address(2) + value(2)` - a successful write
/// echoes the request PDU back verbatim (spec §6.5-6.6).
fn build_single_write_response(
    state: &mut State,
    transaction_id: u16,
    unit_id: u8,
    pdu: &[u8],
) -> Vec<u8> {
    let function = pdu[0];
    if pdu.len() != 5 {
        return build_exception_response_frame(
            transaction_id,
            unit_id,
            function,
            EXCEPTION_ILLEGAL_DATA_VALUE,
        );
    }
    let address = u16::from_be_bytes([pdu[1], pdu[2]]);
    let value = u16::from_be_bytes([pdu[3], pdu[4]]);

    if let Some(&code) = state.exceptions.get(&(function, address)) {
        return build_exception_response_frame(transaction_id, unit_id, function, code);
    }

    if function == FC_WRITE_SINGLE_COIL {
        // Spec §6.5: only 0xFF00 (on) and 0x0000 (off) are legal values.
        if value != 0xFF00 && value != 0x0000 {
            return build_exception_response_frame(
                transaction_id,
                unit_id,
                function,
                EXCEPTION_ILLEGAL_DATA_VALUE,
            );
        }
        hold_coil(state, address, value == 0xFF00);
    } else {
        hold_holding_register(state, address, value);
    }
    wrap_mbap(transaction_id, unit_id, pdu)
}

/// FC15 (multiple coils) / FC16 (multiple registers): request is
/// `function + start(2) + quantity(2) + byte_count(1) + data`; the success
/// response is `function + start(2) + quantity(2)`, with no byte count or
/// data (spec §6.11-6.12).
fn build_multiple_write_response(
    state: &mut State,
    transaction_id: u16,
    unit_id: u8,
    pdu: &[u8],
) -> Vec<u8> {
    let function = pdu[0];
    if pdu.len() < 6 {
        return build_exception_response_frame(
            transaction_id,
            unit_id,
            function,
            EXCEPTION_ILLEGAL_DATA_VALUE,
        );
    }
    let start = u16::from_be_bytes([pdu[1], pdu[2]]);
    let quantity = u16::from_be_bytes([pdu[3], pdu[4]]);
    let byte_count = pdu[5] as usize;
    let data = &pdu[6..];

    if let Some(&code) = state.exceptions.get(&(function, start)) {
        return build_exception_response_frame(transaction_id, unit_id, function, code);
    }

    let max_quantity = if function == FC_WRITE_MULTIPLE_COILS {
        MAX_WRITE_COILS
    } else {
        MAX_WRITE_REGISTERS
    };
    if let Some(code) = range_exception(start, quantity, max_quantity) {
        return build_exception_response_frame(transaction_id, unit_id, function, code);
    }

    let expected_bytes = if function == FC_WRITE_MULTIPLE_COILS {
        usize::from(quantity).div_ceil(8)
    } else {
        usize::from(quantity) * 2
    };
    if byte_count != expected_bytes || data.len() < expected_bytes {
        return build_exception_response_frame(
            transaction_id,
            unit_id,
            function,
            EXCEPTION_ILLEGAL_DATA_VALUE,
        );
    }

    if function == FC_WRITE_MULTIPLE_COILS {
        for i in 0..quantity {
            let byte = data[usize::from(i) / 8];
            let bit = (byte >> (i % 8)) & 1 == 1;
            hold_coil(state, start + i, bit);
        }
    } else {
        for i in 0..quantity {
            let idx = usize::from(i) * 2;
            let word = u16::from_be_bytes([data[idx], data[idx + 1]]);
            hold_holding_register(state, start + i, word);
        }
    }

    let mut response = Vec::with_capacity(5);
    response.push(function);
    response.extend_from_slice(&start.to_be_bytes());
    response.extend_from_slice(&quantity.to_be_bytes());
    wrap_mbap(transaction_id, unit_id, &response)
}

/// Shared range check: a zero or over-cap quantity is `0x03 illegal data
/// value`, and a range that would run past address `0xFFFF` is
/// `0x02 illegal data address` (which is also what keeps the `start + i`
/// arithmetic in the loops above from overflowing).
fn range_exception(start: u16, quantity: u16, max_quantity: u16) -> Option<u8> {
    if quantity == 0 || quantity > max_quantity {
        return Some(EXCEPTION_ILLEGAL_DATA_VALUE);
    }
    if u32::from(start) + u32::from(quantity) > u32::from(u16::MAX) + 1 {
        return Some(EXCEPTION_ILLEGAL_DATA_ADDRESS);
    }
    None
}

/// Commit a wire write to a coil and mark the address held, so the seeding
/// setters (and therefore `banto-collect`'s ramp task) stop overwriting it -
/// see this module's "Write support and the held set" section.
fn hold_coil(state: &mut State, address: u16, value: bool) {
    state.coils.insert(address, value);
    state.held_coils.insert(address);
}

/// The holding-register twin of [`hold_coil`].
fn hold_holding_register(state: &mut State, address: u16, value: u16) {
    state.holding_registers.insert(address, value);
    state.held_holding_registers.insert(address);
}

/// Wire-level tests for the write function codes and the held set (#363).
/// Hand-built request frames rather than a client: `banto-plc` has no write
/// client (it is read-only on purpose - see this crate's `lib.rs`), and
/// `banto-plc-write` cannot be a dependency here (it depends on this crate),
/// so the only way to exercise FC5/6/15/16 from inside this crate is to put
/// the bytes on the socket directly.
#[cfg(test)]
mod write_tests {
    use super::*;
    use crate::modbus::frame::EXCEPTION_FLAG;
    use tokio::net::TcpStream;

    /// Send one PDU and return the response PDU (MBAP header stripped).
    async fn roundtrip(stream: &mut TcpStream, pdu: &[u8]) -> Vec<u8> {
        let frame = wrap_mbap(1, 1, pdu);
        stream.write_all(&frame).await.expect("write request");
        let mut header = [0u8; MBAP_HEADER_LEN];
        stream.read_exact(&mut header).await.expect("read header");
        let length = u16::from_be_bytes([header[4], header[5]]) as usize;
        let mut response = vec![0u8; length - 1];
        stream
            .read_exact(&mut response)
            .await
            .expect("read response pdu");
        response
    }

    async fn connect(sim: &Simulator) -> TcpStream {
        TcpStream::connect(sim.addr).await.expect("connect")
    }

    fn write_single(function: u8, address: u16, value: u16) -> Vec<u8> {
        let mut pdu = vec![function];
        pdu.extend_from_slice(&address.to_be_bytes());
        pdu.extend_from_slice(&value.to_be_bytes());
        pdu
    }

    fn write_multiple(function: u8, start: u16, quantity: u16, data: &[u8]) -> Vec<u8> {
        let mut pdu = vec![function];
        pdu.extend_from_slice(&start.to_be_bytes());
        pdu.extend_from_slice(&quantity.to_be_bytes());
        pdu.push(data.len() as u8);
        pdu.extend_from_slice(data);
        pdu
    }

    fn read_request(function: u8, start: u16, quantity: u16) -> Vec<u8> {
        let mut pdu = vec![function];
        pdu.extend_from_slice(&start.to_be_bytes());
        pdu.extend_from_slice(&quantity.to_be_bytes());
        pdu
    }

    fn exception(function: u8, code: u8) -> Vec<u8> {
        vec![function | EXCEPTION_FLAG, code]
    }

    #[tokio::test]
    async fn fc6_writes_a_holding_register_and_echoes_the_request() {
        let sim = Simulator::start().await;
        let mut stream = connect(&sim).await;

        let request = write_single(FC_WRITE_SINGLE_REGISTER, 7, 4242);
        let response = roundtrip(&mut stream, &request).await;

        assert_eq!(response, request, "FC6 success echoes the request PDU");
        assert_eq!(sim.get_holding_register(7), 4242);
        assert_eq!(sim.write_command_count(), 1);
    }

    #[tokio::test]
    async fn fc5_writes_a_coil_and_rejects_a_value_that_is_neither_on_nor_off() {
        let sim = Simulator::start().await;
        let mut stream = connect(&sim).await;

        let on = write_single(FC_WRITE_SINGLE_COIL, 3, 0xFF00);
        assert_eq!(roundtrip(&mut stream, &on).await, on);
        assert!(sim.get_coil(3));

        let off = write_single(FC_WRITE_SINGLE_COIL, 3, 0x0000);
        assert_eq!(roundtrip(&mut stream, &off).await, off);
        assert!(!sim.get_coil(3));

        let bogus = write_single(FC_WRITE_SINGLE_COIL, 3, 0x1234);
        assert_eq!(
            roundtrip(&mut stream, &bogus).await,
            exception(FC_WRITE_SINGLE_COIL, EXCEPTION_ILLEGAL_DATA_VALUE)
        );
    }

    #[tokio::test]
    async fn fc16_writes_consecutive_registers_and_answers_without_a_payload() {
        let sim = Simulator::start().await;
        let mut stream = connect(&sim).await;

        // A 32-bit tag is two consecutive words - the shape banto-hub's
        // write path produces for a u32/f32/string tag.
        let data = encode_registers_payload(&[0x0001, 0x0002]);
        let request = write_multiple(FC_WRITE_MULTIPLE_REGISTERS, 10, 2, &data);
        let response = roundtrip(&mut stream, &request).await;

        assert_eq!(
            response,
            vec![FC_WRITE_MULTIPLE_REGISTERS, 0x00, 0x0A, 0x00, 0x02],
            "FC16 success answers function + start + quantity, no data"
        );
        assert_eq!(sim.get_holding_register(10), 0x0001);
        assert_eq!(sim.get_holding_register(11), 0x0002);
    }

    #[tokio::test]
    async fn fc15_writes_consecutive_coils() {
        let sim = Simulator::start().await;
        let mut stream = connect(&sim).await;

        let data = encode_bits_payload(&[true, false, true]);
        let request = write_multiple(FC_WRITE_MULTIPLE_COILS, 0, 3, &data);
        let response = roundtrip(&mut stream, &request).await;

        assert_eq!(response, vec![FC_WRITE_MULTIPLE_COILS, 0, 0, 0, 3]);
        assert!(sim.get_coil(0));
        assert!(!sim.get_coil(1));
        assert!(sim.get_coil(2));
    }

    #[tokio::test]
    async fn a_write_running_past_the_last_address_is_illegal_data_address() {
        let sim = Simulator::start().await;
        let mut stream = connect(&sim).await;

        let data = encode_registers_payload(&[1, 2]);
        let request = write_multiple(FC_WRITE_MULTIPLE_REGISTERS, u16::MAX, 2, &data);
        assert_eq!(
            roundtrip(&mut stream, &request).await,
            exception(FC_WRITE_MULTIPLE_REGISTERS, EXCEPTION_ILLEGAL_DATA_ADDRESS)
        );
    }

    #[tokio::test]
    async fn an_over_cap_quantity_is_illegal_data_value() {
        let sim = Simulator::start().await;
        let mut stream = connect(&sim).await;

        // The byte count is deliberately consistent with the (over-cap)
        // quantity, so the cap - not the length check - is what rejects it.
        let quantity = MAX_WRITE_REGISTERS + 1;
        let data = vec![0u8; usize::from(quantity) * 2];
        let request = write_multiple(FC_WRITE_MULTIPLE_REGISTERS, 0, quantity, &data);
        assert_eq!(
            roundtrip(&mut stream, &request).await,
            exception(FC_WRITE_MULTIPLE_REGISTERS, EXCEPTION_ILLEGAL_DATA_VALUE)
        );
    }

    #[tokio::test]
    async fn an_injected_exception_still_pre_empts_a_write() {
        let sim = Simulator::start().await;
        sim.inject_exception(FC_WRITE_SINGLE_REGISTER, 5, 0x04); // slave device failure
        let mut stream = connect(&sim).await;

        let request = write_single(FC_WRITE_SINGLE_REGISTER, 5, 99);
        assert_eq!(
            roundtrip(&mut stream, &request).await,
            exception(FC_WRITE_SINGLE_REGISTER, 0x04)
        );
        assert_eq!(sim.get_holding_register(5), 0, "the write must not land");
        assert_eq!(sim.held_count(), 0, "a refused write holds nothing");
    }

    #[tokio::test]
    async fn an_unknown_function_code_is_still_illegal_function() {
        let sim = Simulator::start().await;
        let mut stream = connect(&sim).await;

        let request = read_request(0x2B, 0, 1); // encapsulated interface transport
        assert_eq!(
            roundtrip(&mut stream, &request).await,
            exception(0x2B, EXCEPTION_ILLEGAL_FUNCTION)
        );
    }

    #[tokio::test]
    async fn a_wire_written_address_is_held_against_the_seeding_setters() {
        let sim = Simulator::start().await;
        let mut stream = connect(&sim).await;

        roundtrip(
            &mut stream,
            &write_single(FC_WRITE_SINGLE_REGISTER, 2, 4242),
        )
        .await;
        roundtrip(&mut stream, &write_single(FC_WRITE_SINGLE_COIL, 2, 0xFF00)).await;
        assert_eq!(sim.held_count(), 2);

        // `banto-collect`'s ramp task drives exactly these setters.
        sim.set_holding_register(2, 1);
        sim.set_holding_registers(1, &[7, 7, 7]);
        sim.set_coil(2, false);
        assert_eq!(
            sim.get_holding_register(2),
            4242,
            "a held register keeps the written value"
        );
        assert!(sim.get_coil(2), "a held coil keeps the written value");

        // Neighbours are untouched by holding and keep ramping.
        assert_eq!(sim.get_holding_register(1), 7);
        assert_eq!(sim.get_holding_register(3), 7);
        sim.set_coil(3, true);
        assert!(sim.get_coil(3));
    }

    #[tokio::test]
    async fn read_only_areas_never_become_held() {
        let sim = Simulator::start().await;
        let mut stream = connect(&sim).await;

        // A write aimed at a holding register holds only the holding
        // register table; the input-register table at the same offset keeps
        // taking ramp updates, exactly as on a real device (3xxxx/1xxxx are
        // read-only on the wire).
        roundtrip(
            &mut stream,
            &write_single(FC_WRITE_SINGLE_REGISTER, 0, 4242),
        )
        .await;
        sim.set_input_register(0, 9);
        sim.set_discrete_input(0, true);

        let response = roundtrip(&mut stream, &read_request(FC_READ_INPUT_REGISTERS, 0, 1)).await;
        assert_eq!(response, vec![FC_READ_INPUT_REGISTERS, 2, 0x00, 0x09]);
        let response = roundtrip(&mut stream, &read_request(FC_READ_DISCRETE_INPUTS, 0, 1)).await;
        assert_eq!(response, vec![FC_READ_DISCRETE_INPUTS, 1, 0x01]);
    }

    #[tokio::test]
    async fn a_written_value_reads_back_over_the_wire() {
        let sim = Simulator::start().await;
        let mut stream = connect(&sim).await;

        roundtrip(
            &mut stream,
            &write_single(FC_WRITE_SINGLE_REGISTER, 4, 0x1234),
        )
        .await;
        let response = roundtrip(&mut stream, &read_request(FC_READ_HOLDING_REGISTERS, 4, 1)).await;
        assert_eq!(response, vec![FC_READ_HOLDING_REGISTERS, 2, 0x12, 0x34]);
    }
}
