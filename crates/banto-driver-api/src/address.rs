//! The driver-neutral address ([`DriverAddress`], WIT `parsed`) and the
//! driver identifier ([`DriverId`]) it is qualified by.
//!
//! `banto_plc::Address` stays the request type of the broker and `PlcClient`:
//! a closed two-protocol enum cannot be API without breaking on every new
//! protocol (docs/banto-driver-api-design.md §3.3). [`DriverAddress`] is
//! produced by a driver's address parser
//! ([`DriverBase::parse_address`](crate::DriverBase::parse_address)), and
//! since R2b-2 (docs/banto-hub-driver-address-design.md) the production paths
//! go through it: the save path of `banto-tags` (when built with a registry)
//! stores its `canonical`, and the Hub's plan, instant read and write paths
//! parse with it. Below that, the HostBound drivers turn it back into a
//! `banto_plc::Address` with `Address::from_driver_address` until R2b-3 moves
//! requests to `DriverAddress` end to end. ChronoGazer has no registry and
//! keeps the legacy rules until R2b-7.
//!
//! [`DriverId`] is defined here (rather than with
//! [`DriverDescriptor`](crate::DriverDescriptor) in [`crate::driver`])
//! because [`DriverAddress::driver`] needs it from R1-1 on.

use std::fmt;

/// A driver's identifier: `[a-z0-9][a-z0-9-]*`, the value stored in
/// `plc_connections.protocol` (`"modbus-tcp"`, `"slmp"`).
///
/// The grammar is checked by [`DriverId::new`] (and by `TryFrom<String>` /
/// deserialization), so every `DriverId` value is valid. Narrowing the
/// grammar is a breaking change (design §9.1).
///
/// `Borrow<str>` lets a `BTreeMap<DriverId, _>` be looked up with a `&str`
/// ([`DriverRegistry`](crate::DriverRegistry), design §6.1); this is sound because `Ord` / `Eq` /
/// `Hash` are those of the inner string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(try_from = "String", into = "String")
)]
pub struct DriverId(String);

impl DriverId {
    /// Validate `s` against `[a-z0-9][a-z0-9-]*`.
    pub fn new(s: &str) -> Result<Self, InvalidDriverId> {
        if Self::is_valid(s) {
            Ok(DriverId(s.to_owned()))
        } else {
            Err(InvalidDriverId {
                input: s.to_owned(),
            })
        }
    }

    /// The identifier as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn is_valid(s: &str) -> bool {
        let mut bytes = s.bytes();
        match bytes.next() {
            Some(first) if first.is_ascii_lowercase() || first.is_ascii_digit() => {}
            _ => return false,
        }
        bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    }
}

impl std::borrow::Borrow<str> for DriverId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DriverId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for DriverId {
    type Error = InvalidDriverId;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        if Self::is_valid(&s) {
            Ok(DriverId(s))
        } else {
            Err(InvalidDriverId { input: s })
        }
    }
}

impl From<DriverId> for String {
    fn from(id: DriverId) -> Self {
        id.0
    }
}

/// A string that is not a valid [`DriverId`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidDriverId {
    input: String,
}

impl InvalidDriverId {
    /// The rejected input, as given.
    pub fn input(&self) -> &str {
        &self.input
    }
}

impl fmt::Display for InvalidDriverId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ドライバ ID {:?} は使えません（英小文字か数字で始め、英小文字・数字・'-' だけで書きます）",
            self.input
        )
    }
}

impl std::error::Error for InvalidDriverId {}

/// A parsed address, as a driver understands it (WIT
/// `record parsed { canonical, area, writable-area, bit, word-count }`).
///
/// There is deliberately no station / unit number: a multi-drop station is
/// a connection setting ([`keys::UNIT_ID`](crate::settings::keys::UNIT_ID) /
/// [`keys::STATION`](crate::settings::keys::STATION)) and sharing one port
/// between stations is the engine's job (design §0.1 Q6, §8).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DriverAddress {
    /// Which driver parsed it (WIT knows this from context).
    pub driver: DriverId,
    /// The normalized text: parsing `canonical` again yields the same
    /// address (idempotent), and one address has exactly one canonical form.
    /// This is what R2b-2 stores in the database. For Modbus / SLMP it is
    /// `banto_plc::Address`'s `Display` (Modbus always 6 digits, `400001`;
    /// a bit suffix without a leading zero, `400001.3`) - design §3.3, Q8.
    pub canonical: String,
    /// The area's name (Modbus: `"coil"` etc., the same spelling as
    /// `AddressArea`'s `Display`; SLMP: the device name, `"D"` etc.).
    pub area: String,
    /// Whether the protocol can write this area at all - a protocol fact,
    /// never a write permission (D5; the write gates are the host's).
    pub writable_area: bool,
    /// Bit inside a word (SLMP `.0`-`.F` hexadecimal, Modbus `.0`-`.15`
    /// decimal, T20-④). `None` for a whole word / bit device.
    pub bit: Option<u8>,
    /// 16-bit words this address occupies (1 for a bit device); the same
    /// unit as [`TagKind::word_count`](crate::TagKind::word_count). Widened
    /// to WIT's `u32` at the boundary.
    pub word_count: u16,
}

impl DriverAddress {
    /// An address without a bit index (`bit = None`); add one with
    /// [`with_bit`](Self::with_bit).
    pub fn new(
        driver: DriverId,
        canonical: impl Into<String>,
        area: impl Into<String>,
        writable_area: bool,
        word_count: u16,
    ) -> Self {
        DriverAddress {
            driver,
            canonical: canonical.into(),
            area: area.into(),
            writable_area,
            bit: None,
            word_count,
        }
    }

    /// The same address with bit index `bit`. The range is the driver's
    /// grammar (it is not checked here).
    pub fn with_bit(self, bit: u8) -> Self {
        DriverAddress {
            bit: Some(bit),
            ..self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn driver_id_accepts_the_grammar() {
        for ok in [
            "modbus-tcp",
            "slmp",
            "a",
            "0",
            "9x",
            "kv-8000",
            "a-",
            "a--b",
        ] {
            let id = DriverId::new(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
            assert_eq!(id.as_str(), ok);
            assert_eq!(id.to_string(), ok);
            assert_eq!(String::from(id), ok);
        }
    }

    #[test]
    fn driver_id_rejects_outside_the_grammar() {
        for bad in [
            "",
            "-slmp",
            "Modbus",
            "modbus_tcp",
            "modbus tcp",
            " slmp",
            "slmp ",
            "slmp.v2",
            "ｓｌｍｐ",
            "é",
        ] {
            let err = DriverId::new(bad).expect_err(bad);
            assert_eq!(err.input(), bad);
            assert!(err.to_string().contains(&format!("{bad:?}")), "{err}");
            assert_eq!(DriverId::try_from(bad.to_string()), Err(err));
        }
    }

    #[test]
    fn driver_id_is_looked_up_by_str_in_a_btree_map() {
        let mut map = BTreeMap::new();
        map.insert(DriverId::new("slmp").unwrap(), 1);
        map.insert(DriverId::new("modbus-tcp").unwrap(), 2);
        assert_eq!(map.get("slmp"), Some(&1));
        assert_eq!(map.get("modbus-tcp"), Some(&2));
        assert_eq!(map.get("kv"), None);
    }

    #[test]
    fn driver_address_new_has_no_bit_and_with_bit_sets_it() {
        let driver = DriverId::new("slmp").unwrap();
        let a = DriverAddress::new(driver.clone(), "D100", "D", true, 1);
        assert_eq!(a.driver, driver);
        assert_eq!(a.canonical, "D100");
        assert_eq!(a.area, "D");
        assert!(a.writable_area);
        assert_eq!(a.bit, None);
        assert_eq!(a.word_count, 1);

        let b = DriverAddress::new(driver, "D100.F", "D", true, 1).with_bit(15);
        assert_eq!(b.bit, Some(15));
        assert_eq!(b.canonical, "D100.F");
        assert_ne!(a, b);
    }
}
