//! [`AddressArea`]: which Modbus register/coil space a PLC address falls in,
//! and (T19 S1-b0, 2026-09-02 オーナー判断) whether that space can be
//! written to at all.
//!
//! ## Why this is its own crate
//!
//! Before this crate existed, "which Modbus areas are writable" was decided
//! in exactly the wire-protocol sense in `banto-plc`'s `AddressArea` (via
//! which areas *exist*, without an explicit predicate), and re-derived by
//! hand a second time in `banto-tags::tag::modbus_read_only_area` for
//! registration-time validation - `banto-tags` (I1, the tag registry)
//! deliberately does not depend on `banto-plc` (I2), because that would drag
//! I2's tokio/slmp/encoding_rs dependency stack into a crate whose only job
//! is owning SQLite rows (see `crates/banto-tags/src/tag.rs`'s
//! `modbus_read_only_area` doc comment for the full reasoning, which predates
//! this crate and is still correct - I1 still must not depend on I2).
//!
//! `AddressArea` itself needs none of that: it is four bare variants plus a
//! `Display` impl, with **zero** dependencies (not even `thiserror` - unlike
//! `banto_plc::Address`, which needs `PlcError` to report a parse failure,
//! `AddressArea` never fails to construct). Pulling just this enum out into
//! its own crate lets `banto-plc` (the protocol layer, which still owns
//! *parsing* an address into an `AddressArea` - see
//! `crates/banto-plc/src/address.rs`), `banto-tags` (I1, which only needs to
//! ask "is this area writable" once it has classified an address string by
//! its leading digit), and `banto-plc-write` (I5, the write driver, which
//! used to re-derive the same non-writable-variant set a third time in its
//! own planning module) all share one literal definition of "which areas are
//! writable" without any of them taking on a dependency they don't want.
//!
//! `banto-plc` re-exports this type (`banto_plc::AddressArea` /
//! `banto_plc::address::AddressArea`) so nothing outside these three crates
//! needs to know this split exists.

/// Which register/coil space an address falls in - the four areas Modbus
/// reference-number notation (`0xxxx`/`1xxxx`/`3xxxx`/`4xxxx`) selects
/// between. See `banto_plc::address` (this enum's consuming crate) for the
/// parser that turns wire-format text into a value of this type, and
/// `banto_plc::planning`/`banto_plc::modbus::frame` for how each area maps to
/// a Modbus function code on the *read* side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AddressArea {
    /// `0xxxx` - read with FC1 (Read Coils), write with FC5/FC15.
    Coil,
    /// `1xxxx` - read with FC2 (Read Discrete Inputs). No write function code
    /// exists for this area - see [`Self::is_writable`].
    DiscreteInput,
    /// `3xxxx` - read with FC4 (Read Input Registers). No write function code
    /// exists for this area - see [`Self::is_writable`].
    InputRegister,
    /// `4xxxx` - read with FC3 (Read Holding Registers), write with FC6/FC16.
    HoldingRegister,
}

impl AddressArea {
    /// Whether this area can be written to at all, independent of any
    /// particular driver's current feature coverage.
    ///
    /// `true` for [`Self::Coil`] and [`Self::HoldingRegister`], `false` for
    /// [`Self::DiscreteInput`] and [`Self::InputRegister`] - **permanently**,
    /// by the Modbus data model itself (Modbus Application Protocol
    /// Specification V1.1b3 §4.3 "Data Model": Discrete Inputs and Input
    /// Registers are defined as read-only tables; Coils and Holding
    /// Registers are read-write). No Modbus function code exists to write a
    /// discrete input or an input register on *any* conforming device, so
    /// this is a fact about the wire protocol, not a gap any driver could
    /// ever close (contrast a connection's *protocol* not having a write
    /// driver wired up yet at all, which is a temporary capability gap
    /// tracked elsewhere - `banto_broker::is_supported_protocol` /
    /// `apps/banto-hub/core/src/write_path.rs`'s gate 5).
    ///
    /// This is the one fact this crate exists to define exactly once - see
    /// this crate's module doc for who reads it and why each of them cannot
    /// simply depend on `banto-plc` to get it.
    pub fn is_writable(self) -> bool {
        matches!(self, AddressArea::Coil | AddressArea::HoldingRegister)
    }
}

impl std::fmt::Display for AddressArea {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            AddressArea::Coil => "coil",
            AddressArea::DiscreteInput => "discrete_input",
            AddressArea::InputRegister => "input_register",
            AddressArea::HoldingRegister => "holding_register",
        };
        f.write_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coil_and_holding_register_are_writable() {
        assert!(AddressArea::Coil.is_writable());
        assert!(AddressArea::HoldingRegister.is_writable());
    }

    /// The one fact this whole crate exists to pin down - see the module
    /// doc's "why this is its own crate" section.
    #[test]
    fn discrete_input_and_input_register_are_not_writable() {
        assert!(!AddressArea::DiscreteInput.is_writable());
        assert!(!AddressArea::InputRegister.is_writable());
    }

    #[test]
    fn display_matches_the_pre_extraction_strings() {
        // Pinned so `banto-plc`'s re-export is a byte-for-byte behavior
        // no-op for every existing caller that formats an `AddressArea`
        // (error messages, planning logs, ...).
        assert_eq!(AddressArea::Coil.to_string(), "coil");
        assert_eq!(AddressArea::DiscreteInput.to_string(), "discrete_input");
        assert_eq!(AddressArea::InputRegister.to_string(), "input_register");
        assert_eq!(AddressArea::HoldingRegister.to_string(), "holding_register");
    }
}
