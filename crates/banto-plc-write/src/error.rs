//! [`PlcWriteError`]: every failure mode a [`crate::client::PlcWriteClient`]
//! can report. The write-side twin of `banto_plc::PlcError` (which this crate
//! deliberately does not reuse: a write has failure modes a read does not - a
//! value that will not fit its target register - and reusing the read type
//! would either force those into an ill-fitting variant or grow the read type
//! with write-only cases, exactly the coupling I5 exists to avoid).
//!
//! Same two-bucket split as the read side (see `banto-plc/src/error.rs`'s
//! module doc): a whole-call failure returned as `Err(PlcWriteError)` from
//! `connect`/`write_batch` (connection-level - the socket is unusable), versus
//! a single request's reason inside [`crate::types::WriteResult::Bad`]
//! (per-request - the connection is fine, only this one write failed). The
//! `is_connection_fatal` switch below is what decides which bucket a failure
//! lands in, and it defaults to *fatal* for the same safety reason the read
//! side does.

use thiserror::Error;

/// All fields are owned (`String`/`u16`) and never wrap a non-`Clone`
/// `std::io::Error`, so one `PlcWriteError` can be cloned into every
/// [`crate::types::WriteResult::Bad`] entry a failed group's requests share and
/// tests can assert on it with `==` - identical reasoning to
/// `banto_plc::PlcError`.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum PlcWriteError {
    /// TCP connect did not complete within the configured connect timeout.
    /// Connection-fatal.
    #[error("接続タイムアウト: {0}")]
    ConnectTimeout(String),

    /// A TCP-level failure (refused, reset, broken pipe, unexpected EOF, DNS
    /// failure). The `String` is the underlying `std::io::Error`'s `Display`
    /// text. Connection-fatal.
    #[error("接続エラー: {0}")]
    Connection(String),

    /// `write_batch`/`disconnect` called before a successful `connect`, or
    /// after a connection-level failure already tore the socket down.
    /// Connection-fatal.
    #[error("未接続です。connect() を先に呼んでください")]
    NotConnected,

    /// The CPU did not answer a write within the configured response timeout.
    /// Connection-fatal for the same reason it is on the read side: a
    /// late/never response leaves the byte stream desynchronized for whatever
    /// comes next, so the whole call fails and the socket is torn down rather
    /// than risk reading a stale reply as the next write's acknowledgement.
    #[error("応答タイムアウト")]
    ResponseTimeout,

    /// The response frame did not parse as valid SLMP: bad response code,
    /// serial-id/route mismatch, truncated or length-inconsistent frame.
    /// Connection-fatal (a malformed frame means the client can no longer
    /// trust its position in the byte stream).
    #[error("プロトコルエラー: {0}")]
    Protocol(String),

    /// A [`crate::types::WriteRequest`] carries an address written in a
    /// different protocol's notation than the client speaks - e.g. a Modbus
    /// reference number handed to the SLMP write planner. A configuration
    /// error, resolved before any wire traffic and reported as a per-request
    /// `Bad` so one mis-configured target never costs its batch-mates their
    /// write. The write-side analogue of `banto_plc::PlcError::AddressProtocolMismatch`.
    #[error("アドレス表記 {actual} は {expected} クライアントでは扱えません")]
    AddressProtocolMismatch { expected: String, actual: String },

    /// A [`crate::types::WriteRequest`]'s `data_type` cannot live at its
    /// address's device - a `bit` type at a word device or a numeric type at a
    /// bit device. Resolved by [`crate::slmp::planning::plan_slmp_writes`]
    /// before any wire traffic, so it only ever appears as a per-request `Bad`.
    #[error("データ型 {data_type} はデバイス {area} と組み合わせられません")]
    UnsupportedCombination { area: String, data_type: String },

    /// The [`banto_plc::TagValue`] handed in does not match the kind its
    /// `data_type` needs - a [`banto_plc::TagValue::Bit`] for a numeric
    /// data type, or a [`banto_plc::TagValue::F64`] for `bit`. A caller mistake
    /// (the read path never produces a mismatched pair, but a rule engine
    /// building a write from a constant can), caught before any wire traffic,
    /// so it is a per-request `Bad`. Write-only: the read side never carries a
    /// value *in*, so it has no equivalent.
    #[error("書き込み値 {value_kind} はデータ型 {data_type} と一致しません")]
    ValueTypeMismatch {
        data_type: String,
        value_kind: String,
    },

    /// The numeric value cannot be represented in its target register width -
    /// out of range (e.g. 70000 into a `u16`) or non-integral into an integer
    /// type (e.g. 1.5 into an `i16`). Caught before any wire traffic - writing
    /// a silently-truncated value to a PLC output is exactly the kind of
    /// destructive surprise this crate's safety posture exists to prevent - so
    /// it is a per-request `Bad`. Write-only: decoding a read *widens* into
    /// `f64` and can never overflow, so there is no read-side twin.
    #[error("書き込み値 {value} はデータ型 {data_type} で表現できません: {detail}")]
    ValueOutOfRange {
        data_type: String,
        value: String,
        detail: String,
    },

    /// A [`crate::types::StringWriteRequest`]'s word span cannot be served:
    /// zero words, or more words than one SLMP bulk write carries. The write
    /// twin of `banto_plc::PlcError::StringSpanUnsupported`; resolved by the
    /// planner before any wire traffic, so a per-request `Bad`, never a
    /// panic. (The registry caps `string_length` at 128, far under the wire
    /// cap, so reaching this means a caller bypassed `banto-tags`
    /// validation.)
    #[error("文字列長 {words} 語は扱えません（1〜{max} 語）")]
    StringSpanUnsupported { words: u16, max: u16 },

    /// The MELSEC CPU answered a bulk write with a well-formed SLMP response
    /// frame carrying a non-zero end code - e.g. `0xC059` (wrong command) or
    /// `0xC061` (wrong length), or a device-protection/latch refusal. The write
    /// analogue of `banto_plc::PlcError::SlmpEndCode`, and non-fatal for the
    /// same reason: the wrapped `slmp` crate validates the frame's declared
    /// data length against what arrived *before* it looks at the end code, so
    /// reaching an end code proves a complete, length-consistent frame and the
    /// byte stream is still aligned to a request boundary. Every other group in
    /// the same `write_batch` call still gets its chance.
    ///
    /// See `src/slmp/mod.rs`'s module doc for how this is told apart from a
    /// framing failure, which is fatal.
    #[error("PLC異常応答: SLMP終了コード=0x{code:04X} ({message})")]
    SlmpEndCode { code: u16, message: String },

    /// #131 (2026-09-01): a [`crate::types::WriteRequest`] targets a Modbus
    /// address in DiscreteInput (`1xxxx`) or InputRegister (`3xxxx`) - both
    /// read-only by Modbus wire protocol, permanently, for every
    /// implementation (see `banto-tags::tag`'s `validate_tag_input` doc
    /// comment for the registry-side half of this same 2026-09-01 owner
    /// decision, docs/tag-server-design.md §6 決定A). Resolved by
    /// [`crate::modbus::planning::plan_modbus_writes`] before any wire
    /// traffic - a per-request `Bad`, never a whole-batch `Err`, exactly like
    /// [`Self::UnsupportedCombination`]. Kept as its own variant rather than
    /// folded into `UnsupportedCombination` because the reason has nothing to
    /// do with `data_type`: unlike a bit-at-a-word-device mismatch, *no*
    /// `data_type` would make a DiscreteInput/InputRegister address writable.
    #[error("{area} は Modbus 仕様上の読み取り専用領域のため書き込めません")]
    ModbusReadOnlyArea { area: String },

    /// The PLC answered a Modbus write with a well-formed exception response
    /// (function code with the high bit set + a 1-byte exception code) - e.g.
    /// "illegal data address" because a tag's address does not exist on this
    /// device. The write analogue of `banto_plc::PlcError::ModbusException`,
    /// and non-fatal for the identical reason: the byte stream is still in
    /// sync (a complete, well-formed response was received, it just carries
    /// an error code instead of an ack), so this is exactly the "individual
    /// error" [`Self::is_connection_fatal`]'s per-request bucket exists for -
    /// **not** connection-fatal, unlike every genuine framing/timeout/socket
    /// failure above. Getting this backwards would mean every ordinary
    /// device-side refusal (e.g. a write to a protected register) forces a
    /// reconnect instead of just failing that one request - see this crate's
    /// module-level write-driver doc and docs/tag-server-design.md §6 for why
    /// the broker relies on this exact line.
    #[error("PLC異常応答: function=0x{function:02x} code=0x{code:02x} ({message})")]
    ModbusException {
        function: u8,
        code: u8,
        message: String,
    },

    /// T8 (docs/tag-server-design.md §6.1): two `BitInWord` requests in the
    /// same batch target the same bit of the same word with conflicting
    /// values (one `true`, one `false`). RMW cannot satisfy both, and
    /// picking a winner would be a silent, order-dependent behavior (which
    /// request "wins" would depend on iteration order, not anything the
    /// caller controls) - so both requests are rejected before any wire
    /// traffic instead. A per-request `Bad`, never a whole-batch failure:
    /// every *other* bit of the same word (and every other word entirely)
    /// still proceeds.
    #[error(
        "ワード {area} のビット {bit} に競合する書き込み要求があります（true と false が同時に指定されています）"
    )]
    ConflictingBitWrite { area: String, bit: u8 },

    /// T8 (docs/tag-server-design.md §6.1): the RMW confirmation read showed
    /// this bit did not land as written. The read/modify/write cycle itself
    /// succeeded (no wire-level error) - the CPU's own scan wrote the same
    /// word between our read and our write is the most likely explanation,
    /// which is exactly the race §6.1 documents as un-preventable and only
    /// detectable (see `slmp::mod`'s module doc: "外部から書くビットを含む
    /// ワードは PLC 側から書かない" is the operational mitigation, this
    /// error is the detection). A per-request `Bad`: every other bit written
    /// in the same RMW is checked independently, so one bit's mismatch does
    /// not invalidate its batch-mates' successful writes.
    #[error(
        "ビット書き込みの確認読みで不一致を検出しました（デバイス={area}, ビット={bit}）。書き戻し競合の可能性があります"
    )]
    BitWriteVerificationFailed { area: String, bit: u8 },
}

impl PlcWriteError {
    /// True for the variants that mean "the socket/framing can no longer be
    /// trusted", so the owner must drop the session and the caller must
    /// `connect()` again. Mirrors `banto_plc::PlcError::is_connection_fatal`
    /// exactly, including the *fatal-by-default* posture: a variant added later
    /// is treated as connection-fatal until someone deliberately lists it among
    /// the per-request exclusions here, because a needless reconnect costs one
    /// cycle whereas trusting a desynchronized stream after a write could
    /// acknowledge a write that never landed.
    ///
    /// The per-request exclusions are exactly the outcomes that leave the
    /// connection perfectly usable for the next group: a device-side
    /// [`Self::SlmpEndCode`] (the CPU refused one write but answered in full),
    /// the four configuration/value errors resolved before any wire
    /// traffic ([`Self::AddressProtocolMismatch`],
    /// [`Self::UnsupportedCombination`], [`Self::ValueTypeMismatch`],
    /// [`Self::ValueOutOfRange`]), and T8's two RMW-specific outcomes
    /// ([`Self::ConflictingBitWrite`], resolved before any wire traffic like
    /// the other configuration errors; [`Self::BitWriteVerificationFailed`],
    /// which by construction only occurs *after* a complete, successful RMW
    /// read/write/confirm cycle - the connection is unquestionably fine, only
    /// the PLC-side race the confirmation read exists to catch happened).
    /// #131 (2026-09-01) adds two more Modbus-specific exclusions:
    /// [`Self::ModbusReadOnlyArea`] (a configuration error resolved before any
    /// wire traffic, like `UnsupportedCombination`) and
    /// [`Self::ModbusException`] (a device-side exception response - the
    /// Modbus twin of `SlmpEndCode`, same "complete, well-formed answer"
    /// reasoning). Getting `ModbusException` wrong here is the one mistake
    /// this whole switch exists to prevent for #131: a broker that treated an
    /// ordinary device refusal as connection-fatal would drop and reconnect
    /// the session on every single write rejection instead of just failing
    /// that one write.
    pub fn is_connection_fatal(&self) -> bool {
        !matches!(
            self,
            PlcWriteError::SlmpEndCode { .. }
                | PlcWriteError::AddressProtocolMismatch { .. }
                | PlcWriteError::UnsupportedCombination { .. }
                | PlcWriteError::ValueTypeMismatch { .. }
                | PlcWriteError::ValueOutOfRange { .. }
                | PlcWriteError::StringSpanUnsupported { .. }
                | PlcWriteError::ConflictingBitWrite { .. }
                | PlcWriteError::BitWriteVerificationFailed { .. }
                | PlcWriteError::ModbusReadOnlyArea { .. }
                | PlcWriteError::ModbusException { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`PlcWriteError::is_connection_fatal`] is the single switch deciding
    /// whether a failure costs one target its write or costs the whole
    /// connection, so every variant is pinned down explicitly rather than left
    /// to the `!matches!` default - same test shape as the read side.
    #[test]
    fn per_request_variants_are_not_connection_fatal() {
        let per_request = [
            PlcWriteError::SlmpEndCode {
                code: 0xC059,
                message: "WrongCommand".to_string(),
            },
            PlcWriteError::AddressProtocolMismatch {
                expected: "slmp".to_string(),
                actual: "modbus-ref".to_string(),
            },
            PlcWriteError::UnsupportedCombination {
                area: "D (word_device)".to_string(),
                data_type: "bit".to_string(),
            },
            PlcWriteError::ValueTypeMismatch {
                data_type: "u16".to_string(),
                value_kind: "bit".to_string(),
            },
            PlcWriteError::ValueOutOfRange {
                data_type: "u16".to_string(),
                value: "70000".to_string(),
                detail: "out of range".to_string(),
            },
            PlcWriteError::StringSpanUnsupported {
                words: 961,
                max: 960,
            },
            PlcWriteError::ConflictingBitWrite {
                area: "D100".to_string(),
                bit: 5,
            },
            PlcWriteError::BitWriteVerificationFailed {
                area: "D100".to_string(),
                bit: 5,
            },
            PlcWriteError::ModbusReadOnlyArea {
                area: "discrete input（1xxxx）".to_string(),
            },
            PlcWriteError::ModbusException {
                function: 0x06,
                code: 0x02,
                message: "illegal data address".to_string(),
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
            PlcWriteError::ConnectTimeout("host:5007".to_string()),
            PlcWriteError::Connection("connection reset".to_string()),
            PlcWriteError::NotConnected,
            PlcWriteError::ResponseTimeout,
            PlcWriteError::Protocol("truncated frame".to_string()),
        ];
        for err in fatal {
            assert!(err.is_connection_fatal(), "{err:?} should be fatal");
        }
    }
}
