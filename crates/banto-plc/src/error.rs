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

    /// [`crate::address::Address::parse`] or
    /// [`crate::address::Address::parse_slmp`] rejected the given text
    /// (docs/plan.md I2 §3: reference-number notation `0/1/3/4` + 4 or 5
    /// digits; I2a: MELSEC device notation such as `D100`/`X1A`).
    #[error("アドレス形式が不正です: {0}")]
    InvalidAddress(String),

    /// A [`crate::types::ReadRequest`] carries an address written in a
    /// different protocol's notation than the client it was handed to speaks -
    /// e.g. an [`crate::address::Address::Slmp`] address in a batch given to
    /// [`crate::modbus::ModbusTcpClient`]. Means the `PlcConnection`'s
    /// `protocol` column and the tag's `address` column disagree, so it is a
    /// configuration error, resolved before any wire traffic and reported as a
    /// per-request `ReadResult::Bad` exactly like
    /// [`Self::UnsupportedCombination`] - one mis-configured tag must not cost
    /// its batch-mates their reading.
    #[error("アドレス表記 {actual} は {expected} クライアントでは扱えません")]
    AddressProtocolMismatch { expected: String, actual: String },

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

    /// The MELSEC CPU answered one bulk read with a well-formed SLMP response
    /// frame carrying a non-zero end code - e.g. `0xC059` (wrong command) or
    /// `0xCEE1` (request too long) because a tag names a device number the CPU
    /// does not have. The SLMP analogue of [`Self::ModbusException`], and
    /// non-fatal for the same reason: the response was complete and
    /// length-consistent (the wrapped `slmp` crate validates the frame's
    /// declared data length against what arrived *before* it looks at the end
    /// code), so the byte stream is still aligned to a request boundary and
    /// every other group in the same `read_batch` call still gets its chance.
    ///
    /// See `src/slmp/mod.rs`'s module doc for how this is told apart from a
    /// framing failure, which is fatal.
    #[error("PLC異常応答: SLMP終了コード=0x{code:04X} ({message})")]
    SlmpEndCode { code: u16, message: String },
}

impl PlcError {
    /// True for the variants that mean "the socket/framing can no longer be
    /// trusted" (docs/plan.md I2 §2: "再接続ループは持たない" - this crate
    /// does not reconnect itself, but it does need to know when *not* to
    /// keep using a stream, and to say so unambiguously to the caller so I3
    /// knows a fresh `connect()` is required). The exclusions are exactly the
    /// per-request outcomes, which leave the connection perfectly usable for
    /// the next request/group: [`Self::ModbusException`] and
    /// [`Self::SlmpEndCode`] (the device refused one specific read but
    /// answered in full), plus [`Self::UnsupportedCombination`] and
    /// [`Self::AddressProtocolMismatch`] (configuration errors resolved before
    /// any wire traffic).
    ///
    /// Note the default is *fatal*: a variant added later is treated as
    /// "stop using this socket" until someone deliberately lists it here,
    /// which is the safe direction to be wrong in - a needless reconnect costs
    /// one poll cycle, whereas decoding a desynchronized stream produces
    /// plausible-looking wrong readings.
    pub(crate) fn is_connection_fatal(&self) -> bool {
        !matches!(
            self,
            PlcError::ModbusException { .. }
                | PlcError::SlmpEndCode { .. }
                | PlcError::UnsupportedCombination { .. }
                | PlcError::AddressProtocolMismatch { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`PlcError::is_connection_fatal`] is the single switch deciding whether
    /// a failure costs one tag its reading or costs the whole connection, so
    /// every variant's classification is pinned down explicitly rather than
    /// left to the `!matches!` default.
    #[test]
    fn per_request_variants_are_not_connection_fatal() {
        let per_request = [
            PlcError::ModbusException {
                function: 0x03,
                code: 0x02,
                message: "illegal data address".to_string(),
            },
            PlcError::SlmpEndCode {
                code: 0xC059,
                message: "WrongCommand".to_string(),
            },
            PlcError::UnsupportedCombination {
                area: "holding_register".to_string(),
                data_type: "bit".to_string(),
            },
            PlcError::AddressProtocolMismatch {
                expected: "modbus-tcp".to_string(),
                actual: "slmp".to_string(),
            },
        ];
        for err in per_request {
            assert!(
                !err.is_connection_fatal(),
                "{err:?} must stay a per-request Bad, not tear the connection down"
            );
        }
    }

    #[test]
    fn connection_level_variants_are_connection_fatal() {
        let fatal = [
            PlcError::ConnectTimeout("host:502".to_string()),
            PlcError::Connection("connection reset".to_string()),
            PlcError::NotConnected,
            PlcError::ResponseTimeout,
            PlcError::Protocol("truncated frame".to_string()),
            PlcError::InvalidAddress("bogus".to_string()),
        ];
        for err in fatal {
            assert!(err.is_connection_fatal(), "{err:?} should be fatal");
        }
    }
}
