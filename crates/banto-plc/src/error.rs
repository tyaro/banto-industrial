//! [`PlcError`]: every failure mode a [`crate::client::PlcClient`] can report.
//!
//! Two very different call sites share this one type (docs/plan.md I2
//! design decision, 2026-07-12): a whole-call failure returned as
//! `Err(PlcError)` from `connect`/`read_batch` (connection-level - the
//! socket is unusable, e.g. [`PlcError::ResponseTimeout`]/
//! [`PlcError::Connection`]), and a single tag's reason inside
//! `ReadResult::Bad` (per-address - the connection is fine, only this one
//! request failed, e.g. [`PlcError::ModbusException`]). See
//! `src/modbus/mod.rs`'s module doc for exactly which variants land in which
//! bucket and why that split keeps "individual errors don't kill the whole
//! batch" true without also pretending a dead socket produced real data.

use thiserror::Error;

/// All fields are owned `String`/`u8`/`u16` (no wrapped `std::io::Error`,
/// which is not `Clone`) so a single `PlcError` can be cloned into every
/// [`crate::types::ReadResult::Bad`] entry a failed [`crate::planning::PlannedRead`]
/// group's mapped requests share, and so tests can assert on it with `==`.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum PlcError {
    /// TCP connect did not complete within the configured connect timeout
    /// (`ModbusTcpConfig::connect_timeout`, default 3s).
    #[error("接続タイムアウト: {0}")]
    ConnectTimeout(String),

    /// A TCP-level failure: refused, reset, broken pipe, unexpected EOF, DNS
    /// failure, etc. - the `String` is the underlying `std::io::Error`'s
    /// `Display` text captured at the point of failure.
    #[error("接続エラー: {0}")]
    Connection(String),

    /// `read_batch`/`disconnect` called before a successful `connect`, or
    /// after a connection-level failure already tore the socket down.
    #[error("未接続です。connect() を先に呼んでください")]
    NotConnected,

    /// The MBAP header + PDU for one wire request did not arrive within the
    /// configured response timeout (`ModbusTcpConfig::response_timeout`,
    /// default 1s). Treated as connection-level, not per-address: a response
    /// that arrives late/never leaves the byte stream desynchronized for
    /// whatever request comes after it (see `read_exact`'s cancellation
    /// caveat in `src/modbus/mod.rs`), so the whole `read_batch` call fails
    /// and the client tears the connection down rather than risk decoding
    /// misaligned bytes as if they were the *next* request's answer.
    #[error("応答タイムアウト")]
    ResponseTimeout,

    /// [`crate::address::Address::parse`] rejected the given text (docs/plan.md
    /// I2 §3: reference-number notation `0/1/3/4` + 4 or 5 digits).
    #[error("アドレス形式が不正です: {0}")]
    InvalidAddress(String),

    /// A [`crate::types::ReadRequest`]'s `data_type` cannot live in its
    /// `address`'s area - e.g. `DataType::F32` at a coil address, or
    /// `DataType::Bit` at a holding-register address (v1 restriction: "bit
    /// はコイル/ディスクリート領域のみ", docs/plan.md I2 §4). Resolved by
    /// [`crate::planning::plan_requests`] before any wire traffic, so this
    /// only ever appears as a per-request `ReadResult::Bad`, never as a
    /// whole-call `Err`.
    #[error("データ型 {data_type} はアドレス領域 {area} と組み合わせられません")]
    UnsupportedCombination { area: String, data_type: String },

    /// The response frame does not parse as valid Modbus: unexpected
    /// protocol id, transaction id mismatch, truncated/oversized payload,
    /// unexpected function code. Always connection-level (a malformed frame
    /// means the client can no longer trust its position in the byte
    /// stream), same reasoning as [`Self::ResponseTimeout`].
    #[error("プロトコルエラー: {0}")]
    Protocol(String),

    /// The PLC answered with a well-formed Modbus exception response
    /// (function code with the high bit set + a 1-byte exception code) for
    /// one particular request group - e.g. "illegal data address" because a
    /// tag's address does not exist on this device. The connection itself
    /// is fine (the byte stream is still in sync), so this is exactly the
    /// "individual error" the trait's third invariant is about: it becomes
    /// `ReadResult::Bad` for the requests mapped to that group while every
    /// other group in the same `read_batch` call still gets a chance to
    /// succeed.
    #[error("PLC例外応答: function=0x{function:02x} code=0x{code:02x} ({message})")]
    ModbusException {
        function: u8,
        code: u8,
        message: String,
    },
}

impl PlcError {
    /// True for the variants that mean "the socket/framing can no longer be
    /// trusted" (docs/plan.md I2 §2: "再接続ループは持たない" - this crate
    /// does not reconnect itself, but it does need to know when *not* to
    /// keep using a stream, and to say so unambiguously to the caller so I3
    /// knows a fresh `connect()` is required). [`Self::ModbusException`] and
    /// [`Self::UnsupportedCombination`] are deliberately excluded - both are
    /// per-request outcomes that leave the connection perfectly usable for
    /// the next request/group.
    pub(crate) fn is_connection_fatal(&self) -> bool {
        !matches!(
            self,
            PlcError::ModbusException { .. } | PlcError::UnsupportedCombination { .. }
        )
    }
}
