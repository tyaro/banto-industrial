//! In-process Modbus TCP simulator (docs/plan.md I2 §6): a minimal server
//! good enough to drive this crate's own integration tests against a real
//! socket instead of hand-decoded byte arrays, and public (behind the
//! `simulator` feature) so I3's later integration tests and R4's 72-hour
//! soak-test harness (docs/recorder-requirements.md §4) can reuse it rather
//! than each standing up their own fake PLC.
//!
//! Not a Modbus conformance test tool: it implements exactly the read
//! function codes this crate's client issues (FC1-4), keeps register/coil
//! state in a plain `HashMap` (sparse - any address never explicitly set
//! reads back as `0`/`false`, which is a convenient default for tests that
//! only care about a handful of addresses), and can be told to return a
//! canned exception code or hang instead of answering, for exercising the
//! client's error paths deterministically.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

use super::frame::{
    build_data_response_frame, build_exception_response_frame, encode_bits_payload,
    encode_registers_payload, FC_READ_COILS, FC_READ_DISCRETE_INPUTS, FC_READ_HOLDING_REGISTERS,
    FC_READ_INPUT_REGISTERS, MBAP_HEADER_LEN,
};

#[derive(Debug, Default)]
struct State {
    coils: HashMap<u16, bool>,
    discrete_inputs: HashMap<u16, bool>,
    holding_registers: HashMap<u16, u16>,
    input_registers: HashMap<u16, u16>,
    /// Exact `(function_code, start_offset)` match -> exception code to
    /// return instead of data, for injecting device-side failures
    /// (docs/plan.md I2 §6).
    exceptions: HashMap<(u8, u16), u8>,
    /// When set, every connection's request handling stalls forever instead
    /// of responding - used to exercise the client's response timeout
    /// without needing a real unreachable host.
    hang: bool,
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

    pub fn set_coil(&self, offset: u16, value: bool) {
        self.state.lock().unwrap().coils.insert(offset, value);
    }

    pub fn set_discrete_input(&self, offset: u16, value: bool) {
        self.state
            .lock()
            .unwrap()
            .discrete_inputs
            .insert(offset, value);
    }

    pub fn set_holding_register(&self, offset: u16, value: u16) {
        self.state
            .lock()
            .unwrap()
            .holding_registers
            .insert(offset, value);
    }

    pub fn set_holding_registers(&self, start_offset: u16, values: &[u16]) {
        let mut state = self.state.lock().unwrap();
        for (i, &v) in values.iter().enumerate() {
            state.holding_registers.insert(start_offset + i as u16, v);
        }
    }

    pub fn set_input_register(&self, offset: u16, value: u16) {
        self.state
            .lock()
            .unwrap()
            .input_registers
            .insert(offset, value);
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
        if pdu.len() < 5 {
            return; // malformed request (real client never sends this)
        }
        let function = pdu[0];
        let start_offset = u16::from_be_bytes([pdu[1], pdu[2]]);
        let quantity = u16::from_be_bytes([pdu[3], pdu[4]]);

        if state.lock().unwrap().hang {
            // Never resolves - the client's own response-timeout budget is
            // what ends this, not us.
            std::future::pending::<()>().await;
        }

        let response = build_response(
            &state,
            transaction_id,
            unit_id,
            function,
            start_offset,
            quantity,
        );
        if stream.write_all(&response).await.is_err() {
            return;
        }
    }
}

fn build_response(
    state: &Arc<Mutex<State>>,
    transaction_id: u16,
    unit_id: u8,
    function: u8,
    start_offset: u16,
    quantity: u16,
) -> Vec<u8> {
    let state = state.lock().unwrap();

    if let Some(&code) = state.exceptions.get(&(function, start_offset)) {
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
        FC_READ_HOLDING_REGISTERS | FC_READ_INPUT_REGISTERS => {
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
        _ => build_exception_response_frame(transaction_id, unit_id, function, 0x01), // illegal function
    }
}
