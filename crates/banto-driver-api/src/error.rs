//! The connection-fatal / per-item split as types
//! (docs/banto-driver-api-design.md §3.4).
//!
//! `banto_plc::PlcError` / `banto_plc_write::PlcWriteError` carry one flat
//! enum and decide fatality with `is_connection_fatal()` (default: fatal).
//! Here the decision is the type: [`ItemError`] only marks the affected tag
//! Bad and keeps the connection; [`FatalError`] drops the connection and
//! reconnects. **The default stays fatal**: an error a driver cannot place
//! in an `ItemError` variant is a `FatalError` (WIT §6.1).
//!
//! R1 does not route production errors through these types; R1-4b adds
//! `classify_read` / `classify_write` in the Modbus / SLMP adapters, with a
//! table test over every `PlcError` / `PlcWriteError` variant.
//!
//! `Display` is hand-written (no `thiserror`, design §2.1) and in Japanese,
//! like `PlcError`'s messages.

use std::fmt;

/// A per-item (non-fatal) error: only that tag is Bad (WIT
/// `variant item-error { device(device-error), unsupported(string), rejected(string) }`).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
pub enum ItemError {
    /// The device refused with a complete, well-formed response (an SLMP end
    /// code, a Modbus exception code).
    Device(DeviceError),
    /// A configuration mismatch decided before anything reaches the wire
    /// (data type vs. area, string length out of range, the other
    /// protocol's notation).
    Unsupported(String),
    /// The driver itself refused, or its verification found a mismatch
    /// (true and false written to the same bit of one word in one batch, a
    /// read-modify-write check read that disagrees). Neither a device
    /// refusal nor a configuration mismatch; still non-fatal.
    Rejected(String),
}

impl fmt::Display for ItemError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ItemError::Device(e) => fmt::Display::fmt(e, f),
            ItemError::Unsupported(m) => write!(f, "扱えない組み合わせです: {m}"),
            ItemError::Rejected(m) => write!(f, "ドライバが断りました: {m}"),
        }
    }
}

impl std::error::Error for ItemError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ItemError::Device(e) => Some(e),
            _ => None,
        }
    }
}

/// A device's refusal (WIT `device-error`): the device's raw code and a
/// human-readable message.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DeviceError {
    /// The device's raw value (SLMP end code, Modbus exception code).
    pub code: u32,
    /// What it means, as the driver explains it (for Modbus the adapter
    /// puts the function code here, `"FC=0x03 ..."`, design §3.4).
    pub message: String,
}

impl DeviceError {
    /// A refusal with `code` and `message`.
    pub fn new(code: u32, message: impl Into<String>) -> Self {
        DeviceError {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for DeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "機器の異常応答: コード=0x{:04X}", self.code)?;
        if !self.message.is_empty() {
            write!(f, " ({})", self.message)?;
        }
        Ok(())
    }
}

impl std::error::Error for DeviceError {}

/// A connection-fatal error: the connection is dropped and re-established
/// (WIT `variant fatal-error { framing, protocol, invalid-config }`).
///
/// `Io` and `Timeout` are not in WIT (under WASM the host does the I/O, so
/// they arise on the host side). **Frame-type drivers must not return them**
/// (conformance C4); only session-type drivers do.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
pub enum FatalError {
    /// The wire structure is broken (bad length, bad header, truncated
    /// frame).
    Framing(String),
    /// The peer broke the protocol in a well-formed frame (a response to a
    /// different request, an impossible state).
    Protocol(String),
    /// The connection's settings cannot work (R1 also maps
    /// `PlcError::InvalidAddress` here to keep it fatal, design §3.4).
    InvalidConfig(String),
    /// Connecting failed or the connection broke (session-type only).
    Io(String),
    /// No (complete) response in time (session-type only).
    Timeout,
}

impl fmt::Display for FatalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FatalError::Framing(m) => write!(f, "フレームが壊れています: {m}"),
            FatalError::Protocol(m) => write!(f, "プロトコルエラー: {m}"),
            FatalError::InvalidConfig(m) => write!(f, "接続設定が不正です: {m}"),
            FatalError::Io(m) => write!(f, "通信エラー: {m}"),
            FatalError::Timeout => f.write_str("応答タイムアウト"),
        }
    }
}

impl std::error::Error for FatalError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as _;

    #[test]
    fn device_error_new_and_display() {
        let e = DeviceError::new(0xC059, "コマンドが違います");
        assert_eq!(e.code, 0xC059);
        assert_eq!(e.message, "コマンドが違います");
        assert_eq!(
            e.to_string(),
            "機器の異常応答: コード=0xC059 (コマンドが違います)"
        );
        // Modbus exception codes are small; an empty message is omitted.
        assert_eq!(
            DeviceError::new(2, "").to_string(),
            "機器の異常応答: コード=0x0002"
        );
    }

    #[test]
    fn item_error_display_names_each_variant() {
        let device = ItemError::Device(DeviceError::new(2, "FC=0x03 Illegal data address"));
        assert_eq!(
            device.to_string(),
            "機器の異常応答: コード=0x0002 (FC=0x03 Illegal data address)"
        );
        assert!(device.source().is_some());
        let unsupported = ItemError::Unsupported("bit は 3xxxx に置けません".into());
        assert_eq!(
            unsupported.to_string(),
            "扱えない組み合わせです: bit は 3xxxx に置けません"
        );
        assert!(unsupported.source().is_none());
        assert_eq!(
            ItemError::Rejected("同じビットに true と false".into()).to_string(),
            "ドライバが断りました: 同じビットに true と false"
        );
    }

    #[test]
    fn fatal_error_display_names_each_variant() {
        assert_eq!(
            FatalError::Framing("長さが合いません".into()).to_string(),
            "フレームが壊れています: 長さが合いません"
        );
        assert_eq!(
            FatalError::Protocol("x".into()).to_string(),
            "プロトコルエラー: x"
        );
        assert_eq!(
            FatalError::InvalidConfig("port".into()).to_string(),
            "接続設定が不正です: port"
        );
        assert_eq!(
            FatalError::Io("refused".into()).to_string(),
            "通信エラー: refused"
        );
        assert_eq!(FatalError::Timeout.to_string(), "応答タイムアウト");
    }
}
