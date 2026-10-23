//! Modbus TCP wire format ([`frame`]), the in-process device simulator
//! ([`simulator`], test / `simulator` feature only) and [`ModbusTcpConfig`].
//!
//! Until R2b-3c this module also held `ModbusTcpClient` (the
//! [`crate::client::PlcClient`] for Modbus TCP), `dial_modbus` and
//! `execute_modbus_reads`. The broker now reaches Modbus through the
//! frame-type driver (`banto-driver-modbus`'s `FrameDriver` over
//! `banto-driver-engine`, docs/banto-hub-driver-io-design.md §6.1), which
//! plans with [`crate::plan_batch_requests`] and decodes with
//! [`crate::decode`] against [`frame`]'s MBAP primitives; the socket, the
//! response timeout and the "a timeout is connection-fatal" rule are the
//! engine's. What is left here is what that driver and the simulator share.
//!
//! The one line this protocol draws between a request's failure and the
//! connection's still holds ([`crate::error::PlcError::is_connection_fatal`]):
//! a **Modbus exception response** is a full, well-formed response, so the
//! byte stream is still in sync and only that request is bad; a timeout, an
//! I/O error, a malformed frame or a transaction-id mismatch is fatal.

// `pub` (2026-09-01, #131): `banto-plc-write` needs a handful of items from
// here (MBAP framing, not the read-only FC1-4 semantics) - see `frame`'s own
// module doc for exactly which items and why. The module path itself being
// public does not widen anything by itself; each item still opts in to `pub`
// individually inside `frame.rs`.
pub mod frame;
#[cfg(any(test, feature = "simulator"))]
pub mod simulator;

use std::time::Duration;

use crate::decode::WordOrder;

/// Everything needed to reach and speak to one Modbus TCP device.
///
/// `PartialEq` (T7-1, docs/tag-server-design.md §4.3): lets `banto-collect`
/// diff a connection's config across a config reload (`Collector::apply_config`)
/// to tell "settings-only change" (host/port edited, same tags/groups - the
/// writer stays open) from "no change at all" (skip the connection's task
/// entirely). `f64` appears nowhere in this struct, so a derived structural
/// `PartialEq` is exact, not an approximation.
#[derive(Debug, Clone, PartialEq)]
pub struct ModbusTcpConfig {
    pub host: String,
    pub port: u16,
    /// Modbus TCP still carries a "unit id" (a holdover from RTU gateways) -
    /// mirrors `banto-tags::PlcConnection::unit_id`.
    pub unit_id: u8,
    /// Default 3s (docs/plan.md I2 §2).
    pub connect_timeout: Duration,
    /// Default 1s (docs/plan.md I2 §2).
    pub response_timeout: Duration,
    /// Default [`WordOrder::HighLow`] (docs/plan.md I2 §5).
    pub word_order: WordOrder,
}

impl Default for ModbusTcpConfig {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 502,
            unit_id: 1,
            connect_timeout: Duration::from_secs(3),
            response_timeout: Duration::from_secs(1),
            word_order: WordOrder::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #131's explicit requirement: the default `word_order` is `HighLow` -
    /// the opposite of SLMP's `LowHigh` default (`SlmpConfig::default`).
    /// Getting this backwards only breaks a 32-bit value's word order on real
    /// hardware, never in a test that hand-picks its own `WordOrder`. (Kept
    /// from the removed `ModbusWriteClient` tests; SLMP reads this default.)
    #[test]
    fn modbus_tcp_config_defaults_to_high_low_word_order() {
        assert_eq!(ModbusTcpConfig::default().word_order, WordOrder::HighLow);
    }
}
