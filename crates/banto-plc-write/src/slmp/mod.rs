//! [`SlmpWriteClient`]: the [`crate::client::PlcWriteClient`] implementation for
//! MELSEC MC / SLMP writes (I5). The write mirror of `banto-plc/src/slmp/mod.rs`,
//! wrapping the same `slmp` crate - here its `bulk_write` command rather than
//! `bulk_read` - and confining itself to the same two things the crate does not
//! do: deciding *what* to write ([`planning`]) and translating *how it failed*
//! into this crate's vocabulary ([`classify_io_error`]).
//!
//! ## The broker-sharing seam (get this shape right for W3)
//!
//! The relay-wright plan's PLC access broker (W3) owns **one**
//! `slmp::SLMPClient` per CPU and drives *both* reads and writes over it (a
//! single session per CPU resolves the MELSEC concurrent-session limit and
//! serializes read/write access to one point). So the actual bulk-write
//! execution is factored out of [`SlmpWriteClient`] into a free function,
//! [`execute_slmp_writes`], that operates on a borrowed `&mut slmp::SLMPClient`.
//! The broker can call [`planning::plan_slmp_writes`] (pure) and
//! [`execute_slmp_writes`] directly against the very same
//! `slmp::SLMPClient` it also issues reads on - without going through
//! [`SlmpWriteClient`]'s own socket at all. [`SlmpWriteClient`] is just the
//! standalone, owns-its-own-socket wrapper over that same function, which is
//! what this crate's unit tests and simulator exercise. This is exactly how the
//! read side keeps `plan_slmp_requests` (pure) separate from the socket I/O.
//!
//! ## Where the connection-fatal / per-request line falls
//!
//! Identical to the read side, because it wraps the identical error surface:
//! the crate reports everything as `std::io::Error`, and only a *non-zero SLMP
//! end code* is safe to continue past (the CPU refused this one write but
//! answered with a complete, length-consistent frame, so the byte stream is
//! still aligned). Everything else - timeout, socket error, malformed frame -
//! is fatal: [`execute_slmp_writes`] returns `Err`, and the owner drops the
//! session. Telling the two apart means reading the crate's error *message
//! text* (it exposes the end code no other way); that coupling is guarded by
//! `slmp_write_end_code_is_bad_not_fatal` in `integration_tests.rs`, the write
//! twin of the read side's tripwire.
//!
//! ## Two deliberate differences from the read client
//!
//! 1. **No outer `tokio::time::timeout` around each write** - same reasoning as
//!    the read client: the wrapped crate applies its own send/receive deadlines
//!    (wired to [`banto_plc::SlmpConfig::response_timeout`]), so a second outer
//!    timeout would reintroduce the mid-frame cancellation the inner one exists
//!    to avoid.
//! 2. **Planning never coalesces across a gap** - a write has a side effect, so
//!    [`planning`] merges only exactly-adjacent targets (the read planner
//!    tolerates a small gap to save round trips). See its module doc.

pub mod planning;

#[cfg(any(test, feature = "simulator"))]
pub mod simulator;

#[cfg(test)]
mod integration_tests;

use std::io::ErrorKind;

use banto_plc::{BoxFuture, SlmpConfig, SlmpCpu, SlmpDevice};

use crate::client::PlcWriteClient;
use crate::error::PlcWriteError;
use crate::types::{WriteRequest, WriteResult};

use planning::{plan_slmp_writes, SlmpWritePlanOutcome, WritePayload};

/// Map [`banto_plc::SlmpCpu`] onto the wrapped crate's `CPU`. Re-derived here
/// (rather than reusing `banto-plc`'s mapping, which is private) for the same
/// reason the device mapping below is.
fn cpu_to_wire(cpu: SlmpCpu) -> slmp::CPU {
    match cpu {
        SlmpCpu::Q => slmp::CPU::Q,
        SlmpCpu::R => slmp::CPU::R,
        SlmpCpu::L => slmp::CPU::L,
    }
}

/// Map [`banto_plc::SlmpDevice`] onto the wrapped crate's `DeviceType`.
///
/// This duplicates `banto-plc`'s own `SlmpDevice::to_wire`, which is
/// `pub(crate)` there and so not reachable from this crate. The duplication is
/// total and one-for-one, and kept honest exactly as the original is: by
/// `device_wire_codes_match_the_wrapped_crate`, which checks every device
/// against the actual byte the crate puts on the wire, so a transposed pair
/// here cannot silently write the wrong device.
fn device_to_wire(device: SlmpDevice) -> slmp::DeviceType {
    use slmp::DeviceType as W;
    match device {
        SlmpDevice::X => W::X,
        SlmpDevice::Y => W::Y,
        SlmpDevice::M => W::M,
        SlmpDevice::L => W::L,
        SlmpDevice::F => W::F,
        SlmpDevice::V => W::V,
        SlmpDevice::B => W::B,
        SlmpDevice::D => W::D,
        SlmpDevice::W => W::W,
        SlmpDevice::S => W::S,
        SlmpDevice::Z => W::Z,
        SlmpDevice::R => W::R,
        SlmpDevice::ZR => W::ZR,
        SlmpDevice::TS => W::TS,
        SlmpDevice::TC => W::TC,
        SlmpDevice::TN => W::TN,
        SlmpDevice::SS => W::SS,
        SlmpDevice::SC => W::SC,
        SlmpDevice::SN => W::SN,
        SlmpDevice::CS => W::CS,
        SlmpDevice::CC => W::CC,
        SlmpDevice::CN => W::CN,
        SlmpDevice::SB => W::SB,
        SlmpDevice::SD => W::SD,
        SlmpDevice::SM => W::SM,
        SlmpDevice::SW => W::SW,
        SlmpDevice::DX => W::DX,
        SlmpDevice::DY => W::DY,
    }
}

/// Build the wrapped crate's connection props from a [`SlmpConfig`]. Reuses the
/// shared read-side config type (its fields are all `pub`); `banto-plc`'s own
/// `to_wire_props` is private, so the small assembly is repeated here.
fn to_wire_props(config: &SlmpConfig) -> slmp::SLMP4EConnectionProps {
    slmp::SLMP4EConnectionProps {
        ip: config.host.clone(),
        port: config.port,
        cpu: cpu_to_wire(config.cpu),
        serial_id: config.serial_id,
        network_id: config.network_id,
        pc_id: config.pc_id,
        io_id: config.io_id,
        area_id: config.area_id,
        cpu_timer: config.cpu_timer,
    }
}

/// The marker the wrapped crate puts at the front of the `std::io::Error` it
/// builds for a non-zero SLMP end code - identical string to the one the read
/// side matches (the crate builds it in one place, `validate_response`, for
/// reads and writes alike).
const END_CODE_MARKER: &str = "SLMP Returns Error:";

/// Pull `(code, symbolic name)` out of the wrapped crate's end-code message,
/// shape `"SLMP Returns Error: {name} (0x{code:X})"`. Fails *closed* (returns
/// `None`, which [`classify_io_error`] then treats as a fatal framing failure)
/// for anything not matching exactly - the same deliberate direction-to-be-
/// wrong-in as the read side: a needless reconnect costs one cycle, whereas
/// assuming an unparsed message was "just a device error" could acknowledge a
/// write off a desynchronized stream.
fn parse_end_code(text: &str) -> Option<(u16, String)> {
    let after_marker = text.split_once(END_CODE_MARKER)?.1;
    let (name, tail) = after_marker.rsplit_once("(0x")?;
    let hex = tail.strip_suffix(')')?;
    let code = u16::from_str_radix(hex, 16).ok()?;
    Some((code, name.trim().to_string()))
}

/// Translate the wrapped crate's one-size-fits-all `std::io::Error` into a
/// [`PlcWriteError`], deciding connection-fatal vs per-request `Bad`. Same
/// table as the read side's `classify_io_error`.
fn classify_io_error(err: &std::io::Error) -> PlcWriteError {
    let text = err.to_string();
    match err.kind() {
        ErrorKind::TimedOut => PlcWriteError::ResponseTimeout,
        ErrorKind::NotConnected => PlcWriteError::NotConnected,
        ErrorKind::InvalidData => match parse_end_code(&text) {
            Some((code, message)) => PlcWriteError::SlmpEndCode { code, message },
            None => PlcWriteError::Protocol(text),
        },
        _ => PlcWriteError::Connection(text),
    }
}

/// Execute a planned batch of writes on a **borrowed** `slmp::SLMPClient`, the
/// reusable core the W3 broker calls directly on its shared per-CPU session (see
/// this module's doc comment). Issues one `bulk_write` per group in
/// `outcome.writes`, folds `outcome.immediate_bad` in by index, and returns a
/// `Vec<WriteResult>` of length `total_requests` in original request order.
///
/// `Err` is reserved for a connection-fatal failure (the caller must drop the
/// session and reconnect); a device-side end code becomes a per-request `Bad`
/// for that group's requests and the loop continues. On a fatal `Err`, any
/// groups already written have landed on the PLC - a partial batch - exactly as
/// the read side discards partial results on `Err`; the caller decides how to
/// recover.
///
/// Does not own or reconnect the socket: connection lifecycle is the caller's
/// ([`SlmpWriteClient`] for the standalone form, the broker for the shared one).
pub async fn execute_slmp_writes(
    client: &mut slmp::SLMPClient,
    outcome: &SlmpWritePlanOutcome,
    total_requests: usize,
) -> Result<Vec<WriteResult>, PlcWriteError> {
    let mut results: Vec<Option<WriteResult>> = vec![None; total_requests];
    for (index, reason) in &outcome.immediate_bad {
        results[*index] = Some(WriteResult::Bad(reason.clone()));
    }

    for group in &outcome.writes {
        let start_device = slmp::Device {
            device_type: device_to_wire(group.device),
            address: group.start as usize,
        };
        // Homogeneous by construction: a group is all-word or all-bit, so the
        // `TypedData` vector is uniform and the wrapped crate's bulk_write picks
        // the right access type (it treats an all-`Bool` slice as bit access,
        // anything else as word access).
        let data: Vec<slmp::TypedData> = match &group.payload {
            WritePayload::Words(words) => words.iter().map(|&w| slmp::TypedData::U16(w)).collect(),
            WritePayload::Bits(bits) => bits.iter().map(|&b| slmp::TypedData::Bool(b)).collect(),
        };

        match client.bulk_write(start_device, &data).await {
            Ok(()) => {
                for &index in &group.request_indices {
                    results[index] = Some(WriteResult::Ok);
                }
            }
            Err(e) => {
                let err = classify_io_error(&e);
                if err.is_connection_fatal() {
                    return Err(err);
                }
                // Device-side end code: the CPU refused this one group but the
                // frame was complete, so only these requests are bad.
                for &index in &group.request_indices {
                    results[index] = Some(WriteResult::Bad(err.clone()));
                }
            }
        }
    }

    Ok(results
        .into_iter()
        .enumerate()
        .map(|(i, r)| {
            r.unwrap_or_else(|| {
                panic!("plan_slmp_writes must account for every input index, missing {i}")
            })
        })
        .collect())
}

/// The standalone [`PlcWriteClient`] for MELSEC MC / SLMP. Owns its own socket -
/// this is the form unit tests and the simulator drive. The W3 broker does *not*
/// use this; it calls [`execute_slmp_writes`] on its own shared session (see the
/// module doc). One instance per connection, not `Clone`, not internally
/// reconnecting.
pub struct SlmpWriteClient {
    config: SlmpConfig,
    /// `Some` exactly while connected. Cleared on any connection-fatal failure,
    /// which is what enforces "every call returns `NotConnected` until
    /// `connect()`", matching the read client.
    inner: Option<slmp::SLMPClient>,
}

impl SlmpWriteClient {
    pub fn new(config: SlmpConfig) -> Self {
        Self {
            config,
            inner: None,
        }
    }
}

impl PlcWriteClient for SlmpWriteClient {
    fn connect(&mut self) -> BoxFuture<'_, Result<(), PlcWriteError>> {
        Box::pin(async move {
            // Replace any previous session, same as the read client.
            if let Some(previous) = self.inner.take() {
                previous.close().await;
            }

            let addr = format!("{}:{}", self.config.host, self.config.port);
            let mut client = slmp::SLMPClient::new(to_wire_props(&self.config));
            client.set_send_timeout(self.config.response_timeout);
            client.set_recv_timeout(self.config.response_timeout);

            tokio::time::timeout(self.config.connect_timeout, client.connect())
                .await
                .map_err(|_| PlcWriteError::ConnectTimeout(addr.clone()))?
                .map_err(|e| match e.kind() {
                    ErrorKind::TimedOut => PlcWriteError::ConnectTimeout(addr.clone()),
                    _ => PlcWriteError::Connection(e.to_string()),
                })?;

            self.inner = Some(client);
            Ok(())
        })
    }

    fn write_batch<'a>(
        &'a mut self,
        requests: &'a [WriteRequest],
    ) -> BoxFuture<'a, Result<Vec<WriteResult>, PlcWriteError>> {
        Box::pin(async move {
            if self.inner.is_none() {
                return Err(PlcWriteError::NotConnected);
            }

            let outcome = plan_slmp_writes(requests, self.config.word_order);

            // Thin wrapper over the shared execute function: run it on our own
            // socket, and on a fatal error drop the session so the next call is
            // NotConnected (the broker does the equivalent for its own socket).
            let client = self
                .inner
                .as_mut()
                .expect("checked Some above, only cleared on the fatal branch below");

            match execute_slmp_writes(client, &outcome, requests.len()).await {
                Ok(results) => Ok(results),
                Err(err) => {
                    self.inner = None;
                    Err(err)
                }
            }
        })
    }

    fn disconnect(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            if let Some(client) = self.inner.take() {
                // Real shutdown, not just a drop: a CPU has a finite number of
                // SLMP sessions and a half-open one ties up a slot. The whole
                // broker design (W3) exists to economize on those sessions, so
                // leaking one here would be doubly wrong.
                client.close().await;
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use banto_plc::{Address, DataType, TagValue};

    /// The tripwire for [`device_to_wire`]: this crate's device table and the
    /// wrapped crate's must agree on every device's *actual wire code*, not
    /// merely map to a same-named variant. Same check as `banto-plc`'s
    /// `slmp_device_wire_codes_match_the_wrapped_crate`, repeated here because
    /// this crate carries its own copy of the mapping.
    #[test]
    fn device_wire_codes_match_the_wrapped_crate() {
        let expected: &[(&str, u8)] = &[
            ("X", 0x9C),
            ("Y", 0x9D),
            ("M", 0x90),
            ("L", 0x92),
            ("F", 0x93),
            ("V", 0x94),
            ("B", 0xA0),
            ("D", 0xA8),
            ("W", 0xB4),
            ("S", 0x98),
            ("Z", 0xCC),
            ("R", 0xAF),
            ("ZR", 0xB0),
            ("TS", 0xC1),
            ("TC", 0xC0),
            ("TN", 0xC2),
            ("SS", 0xC7),
            ("SC", 0xC6),
            ("SN", 0xC8),
            ("CS", 0xC4),
            ("CC", 0xC3),
            ("CN", 0xC5),
            ("SB", 0xA1),
            ("SD", 0xA9),
            ("SM", 0x91),
            ("SW", 0xB5),
            ("DX", 0xA2),
            ("DY", 0xA3),
        ];
        assert_eq!(expected.len(), 28);

        for (mnemonic, code) in expected {
            let device = *SlmpDevice::all()
                .iter()
                .find(|d| d.mnemonic() == *mnemonic)
                .unwrap_or_else(|| panic!("{mnemonic} should be a known device"));
            assert_eq!(
                device_to_wire(device).to_code(),
                *code,
                "{mnemonic} should serialize as wire code 0x{code:02X}"
            );
        }
    }

    #[test]
    fn parse_end_code_extracts_code_and_name() {
        assert_eq!(
            parse_end_code("SLMP Returns Error: WrongLength (0xC061)"),
            Some((0xC061, "WrongLength".to_string()))
        );
    }

    #[test]
    fn parse_end_code_rejects_anything_that_is_not_that_shape() {
        for text in [
            "Received Invalid Data Frame",
            "SLMP Returns Error: WrongLength", // no code
            "",
        ] {
            assert_eq!(parse_end_code(text), None, "{text:?} should not parse");
        }
    }

    #[test]
    fn classify_io_error_splits_fatal_from_per_request() {
        use std::io::Error;

        let end_code = classify_io_error(&Error::new(
            ErrorKind::InvalidData,
            "SLMP Returns Error: WrongLength (0xC061)",
        ));
        assert_eq!(
            end_code,
            PlcWriteError::SlmpEndCode {
                code: 0xC061,
                message: "WrongLength".to_string()
            }
        );
        assert!(!end_code.is_connection_fatal());

        let framing = classify_io_error(&Error::new(
            ErrorKind::InvalidData,
            "Received Invalid Data Frame",
        ));
        assert!(matches!(framing, PlcWriteError::Protocol(_)));
        assert!(framing.is_connection_fatal());

        for kind in [
            ErrorKind::TimedOut,
            ErrorKind::ConnectionReset,
            ErrorKind::BrokenPipe,
            ErrorKind::UnexpectedEof,
            ErrorKind::ConnectionRefused,
        ] {
            assert!(classify_io_error(&Error::new(kind, "boom")).is_connection_fatal());
        }
        assert_eq!(
            classify_io_error(&Error::new(ErrorKind::TimedOut, "x")),
            PlcWriteError::ResponseTimeout
        );
    }

    /// An unparseable end-code message must fail *closed* (fatal), the direction
    /// that costs a cycle rather than trusting a possibly-desynchronized stream.
    #[test]
    fn an_end_code_message_of_an_unexpected_shape_is_treated_as_fatal() {
        let err = classify_io_error(&std::io::Error::new(
            ErrorKind::InvalidData,
            "SLMP Returns Error: something new and unparsed",
        ));
        assert!(err.is_connection_fatal());
    }

    #[tokio::test]
    async fn write_batch_before_connect_is_not_connected() {
        let mut client = SlmpWriteClient::new(SlmpConfig {
            host: "127.0.0.1".to_string(),
            ..Default::default()
        });
        let requests = [WriteRequest {
            address: Address::parse_slmp("D0").unwrap(),
            data_type: DataType::U16,
            value: TagValue::F64(1.0),
        }];
        assert!(matches!(
            client.write_batch(&requests).await,
            Err(PlcWriteError::NotConnected)
        ));
    }

    #[tokio::test]
    async fn disconnect_on_a_never_connected_client_is_a_no_op() {
        let mut client = SlmpWriteClient::new(SlmpConfig::default());
        client.disconnect().await; // must not panic
        assert!(client.inner.is_none());
    }
}
