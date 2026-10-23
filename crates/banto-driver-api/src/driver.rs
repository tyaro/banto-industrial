//! The static, stateless half of every driver: identification, settings
//! validation and the address functions ([`DriverBase`],
//! docs/banto-driver-api-design.md §4.1).
//!
//! Every driver kind - frame ([`FrameDriver`](crate::FrameDriver)), session
//! ([`SessionDriver`](crate::SessionDriver)) and the
//! host-provided protocols whose I/O the host keeps
//! ([`DriverKind::HostProvided`](crate::DriverKind::HostProvided)) - implements
//! [`DriverBase`], so REST / MCP / CSV import / configuration packages / the
//! UI can validate a connection and parse an address without opening one.
//! These calls come in concurrently from many places (design §6.5), hence
//! `Send + Sync` and `&self` only.
//!
//! [`DriverId`] itself lives in [`crate::address`] (R1-1 needed it for
//! [`DriverAddress::driver`](crate::DriverAddress::driver)); it is
//! re-exported at the crate root like everything else.

use std::fmt;

use crate::address::{DriverAddress, DriverId};
use crate::schema::{KindName, SettingsSchema};
use crate::settings::{FieldError, Settings};
use crate::types::{DataType, TagKind};

/// The API's version: the major of WIT's `manifest.hubApi`.
///
/// [`DriverDescriptor::new`] stamps it into
/// [`DriverDescriptor::api_version`], so a host can tell which contract a
/// driver was built against.
pub const API_VERSION: u32 = 1;

/// A driver's identity (WIT `manifest`: id / version / displayName / vendor
/// / transports / capabilities / hubApi).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DriverDescriptor {
    /// `plc_connections.protocol`'s value (`"modbus-tcp"`, `"slmp"`).
    pub id: DriverId,
    /// The driver's own SemVer version (a built-in driver uses its crate's
    /// version).
    pub version: String,
    /// The name shown to people.
    pub display_name: String,
    /// Who made it, if stated.
    pub vendor: Option<String>,
    /// The transports it can run over (design §8). R1's Modbus / SLMP are
    /// `[Tcp]`.
    pub transports: Vec<Transport>,
    /// Read / write support (WIT `world driver` / `driver-rw`).
    pub capabilities: Capabilities,
    /// [`API_VERSION`] at build time (set by [`DriverDescriptor::new`]).
    pub api_version: u32,
    /// The version of the shape of this driver's `settings` (R2b-1a,
    /// docs/banto-hub-connection-settings-design.md §3.4). A host stores it
    /// next to a connection's settings when it writes them and refuses to
    /// read settings written under a *newer* version than the driver's
    /// (a rolled-back host or plugin). Starts at 1
    /// ([`DriverDescriptor::new`]); a driver that changes its settings shape
    /// raises it with [`with_settings_version`](Self::with_settings_version).
    #[cfg_attr(feature = "serde", serde(default = "default_settings_version"))]
    pub settings_version: u32,
}

/// The `settings_version` of a descriptor that does not state one.
const fn default_settings_version() -> u32 {
    1
}

impl DriverDescriptor {
    /// A descriptor with `transports = [Tcp]`, no vendor,
    /// `api_version = API_VERSION` and `settings_version = 1`.
    pub fn new(
        id: DriverId,
        version: impl Into<String>,
        display_name: impl Into<String>,
        capabilities: Capabilities,
    ) -> Self {
        DriverDescriptor {
            id,
            version: version.into(),
            display_name: display_name.into(),
            vendor: None,
            transports: vec![Transport::Tcp],
            capabilities,
            api_version: API_VERSION,
            settings_version: default_settings_version(),
        }
    }

    /// The same descriptor with `vendor` set.
    pub fn with_vendor(self, vendor: impl Into<String>) -> Self {
        DriverDescriptor {
            vendor: Some(vendor.into()),
            ..self
        }
    }

    /// The same descriptor with `settings_version` replaced.
    pub fn with_settings_version(self, settings_version: u32) -> Self {
        DriverDescriptor {
            settings_version,
            ..self
        }
    }

    /// The same descriptor with `transports` replaced.
    pub fn with_transports(self, transports: Vec<Transport>) -> Self {
        DriverDescriptor { transports, ..self }
    }
}

/// What a driver can do on a connection (WIT `world driver` = read only,
/// `driver-rw` = read and write).
///
/// A protocol fact, not a permission: the host's write gates (tag
/// `writable`, API key scope, the acceptance toggle, rate, audit) are
/// separate and never consult this alone (design §11).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Capabilities {
    /// The driver can read.
    pub read: bool,
    /// The driver can write.
    pub write: bool,
}

impl Capabilities {
    /// Read-only.
    pub const READ_ONLY: Self = Self::new(true, false);
    /// Read and write.
    pub const READ_WRITE: Self = Self::new(true, true);

    /// The given combination.
    pub const fn new(read: bool, write: bool) -> Self {
        Capabilities { read, write }
    }
}

/// How bytes reach the device (design §8).
///
/// R1 accepts only [`Tcp`](Transport::Tcp) (a driver's `validate_config`
/// refuses the others with a [`FieldError`]); R2b-3's engine adds the serial
/// transports without changing the API.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
pub enum Transport {
    /// A TCP socket to the device.
    Tcp,
    /// A local serial port (COM).
    Serial,
    /// Raw serial bytes through a serial-to-TCP converter.
    SerialOverTcp,
}

/// The stateless part of a driver (design §4.1). Called concurrently from
/// REST / MCP / CSV / configuration packages / the UI (design §6.5).
pub trait DriverBase: Send + Sync + 'static {
    /// Who this driver is.
    fn descriptor(&self) -> &DriverDescriptor;

    /// Check a connection's settings (WIT `config.validate`).
    ///
    /// Unknown keys must not be refused (the host may carry keys for other
    /// layers); a missing required key or a malformed value is reported per
    /// key. An empty result means "valid" (conformance C2).
    fn validate_config(&self, settings: &Settings) -> Vec<FieldError>;

    /// Setting keys whose values are secrets (WIT `config.secret-keys`): the
    /// host masks them in logs and audit. Must be keys the driver actually
    /// reads (C2). Default: none.
    fn secret_keys(&self) -> Vec<String> {
        Vec::new()
    }

    /// Parse an address as written by a person (WIT
    /// `address.parse(text, data-type, string-words)`). `kind` is needed to
    /// know how many words the tag spans.
    ///
    /// The result's `canonical` is idempotent: parsing it again gives the
    /// same address (C1). A grammar error stops here, before any connection
    /// (C4).
    fn parse_address(&self, text: &str, kind: &TagKind) -> Result<DriverAddress, AddressError>;

    /// The canonical address `n` units away from `canonical` (WIT
    /// `address.offset(canonical, data-type, string-words, n)`), for
    /// continuous registration and offset copy. `n = 0` returns `canonical`
    /// unchanged (C1).
    fn offset_address(
        &self,
        canonical: &str,
        kind: &TagKind,
        n: i64,
    ) -> Result<String, AddressError>;

    /// A short explanation of the address grammar for the UI (WIT
    /// `address.help`; Japanese, plugin-design §6.2).
    fn address_help(&self) -> String;

    /// The one point a host's connection test reads (R2b-3a-2,
    /// docs/banto-hub-driver-io-design.md §4.2 / Q11): an address as a
    /// person would write it (the host runs it through
    /// [`parse_address`](Self::parse_address)) and its kind. The host
    /// connects, reads this point once and disconnects, so every driver gets
    /// a connection test without the host knowing its grammar.
    ///
    /// Default: `None` - the test only connects (and, for a frame-type
    /// driver, completes the handshake). Adding this with a default is not a
    /// breaking change (design §9.1).
    fn probe(&self) -> Option<(String, TagKind)> {
        None
    }

    /// The fields of a connection's settings, for the host's generic form
    /// (WIT `config.schema`; R2b-6a, docs/banto-hub-driver-ui-design.md §3).
    ///
    /// Default: empty - the generic form shows no fields for this driver.
    /// A non-empty schema must agree with the driver (C17-C19): unique keys,
    /// defaults that fit the field, `Secret` fields exactly
    /// [`secret_keys`](Self::secret_keys), and defaults plus the minimal
    /// settings passing [`validate_config`](Self::validate_config).
    fn settings_schema(&self) -> SettingsSchema {
        SettingsSchema::default()
    }

    /// The tag kinds this driver can read and write (WIT
    /// `address.supported-kinds`), for the host's data-type choices and its
    /// kind check. Each kind listed here must be accepted by
    /// [`parse_address`](Self::parse_address) at some address, and a kind
    /// left out must be refused (C20).
    ///
    /// Default: every number and bit except the 64-bit ones (no string).
    fn supported_kinds(&self) -> Vec<KindName> {
        [
            DataType::Bit,
            DataType::I16,
            DataType::U16,
            DataType::I32,
            DataType::U32,
            DataType::F32,
        ]
        .into_iter()
        .map(KindName::Numeric)
        .collect()
    }

    /// The canonical address `n` *words* away from `canonical`, whatever the
    /// tag's kind (WIT `address.offset-words`;
    /// docs/banto-hub-driver-ui-design.md §3.4, §4.3), for offset copy and
    /// struct registration (where [`offset_address`](Self::offset_address)
    /// would step by the kind's width). The contract:
    ///
    /// - a word address moves `n` words;
    /// - a bit inside a word keeps its bit and moves the word
    ///   (`D100.5` + 1 = `D101.5`, `400001.3` + 1 = `400002.3`);
    /// - a bit device (no word unit) moves `n` points;
    /// - leaving the address space is an error. `n` may be negative.
    ///
    /// Default: an error saying this driver has no word-wise offset (adding
    /// it with a default is not a breaking change).
    fn offset_words(&self, canonical: &str, n: i64) -> Result<String, AddressError> {
        let _ = (canonical, n);
        Err(AddressError::new(
            "このドライバはワード単位のオフセットに対応しません",
        ))
    }
}

/// An address the driver's grammar does not accept, or an offset that
/// leaves the address space.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressError {
    message: String,
}

impl AddressError {
    /// An error with a message for the person who wrote the address.
    pub fn new(message: impl Into<String>) -> Self {
        AddressError {
            message: message.into(),
        }
    }

    /// The message, as given to [`new`](Self::new).
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for AddressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "アドレスが不正です: {}", self.message)
    }
}

impl std::error::Error for AddressError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_new_defaults_and_builders() {
        let id = DriverId::new("echo").unwrap();
        let d = DriverDescriptor::new(id.clone(), "0.1.0", "Echo", Capabilities::READ_ONLY);
        assert_eq!(d.id, id);
        assert_eq!(d.version, "0.1.0");
        assert_eq!(d.display_name, "Echo");
        assert_eq!(d.vendor, None);
        assert_eq!(d.transports, vec![Transport::Tcp]);
        assert_eq!(d.capabilities, Capabilities::new(true, false));
        assert_eq!(d.api_version, API_VERSION);
        assert_eq!(d.settings_version, 1);

        let d = d
            .with_settings_version(3)
            .with_vendor("tyaro")
            .with_transports(vec![Transport::Serial, Transport::SerialOverTcp]);
        assert_eq!(d.settings_version, 3);
        assert_eq!(d.vendor.as_deref(), Some("tyaro"));
        assert_eq!(
            d.transports,
            vec![Transport::Serial, Transport::SerialOverTcp]
        );
    }

    #[test]
    fn capabilities_constants() {
        assert_eq!(Capabilities::READ_ONLY, Capabilities::new(true, false));
        assert_eq!(Capabilities::READ_WRITE, Capabilities::new(true, true));
    }

    #[test]
    fn address_error_message_and_display() {
        let e = AddressError::new("W の後ろは 0〜65535");
        assert_eq!(e.message(), "W の後ろは 0〜65535");
        assert_eq!(e.to_string(), "アドレスが不正です: W の後ろは 0〜65535");
    }

    struct Minimal(DriverDescriptor);

    impl DriverBase for Minimal {
        fn descriptor(&self) -> &DriverDescriptor {
            &self.0
        }
        fn validate_config(&self, _settings: &Settings) -> Vec<FieldError> {
            Vec::new()
        }
        fn parse_address(
            &self,
            _text: &str,
            _kind: &TagKind,
        ) -> Result<DriverAddress, AddressError> {
            Err(AddressError::new("none"))
        }
        fn offset_address(
            &self,
            canonical: &str,
            _kind: &TagKind,
            _n: i64,
        ) -> Result<String, AddressError> {
            Ok(canonical.to_owned())
        }
        fn address_help(&self) -> String {
            String::new()
        }
    }

    #[test]
    fn probe_default_is_none() {
        let d = Minimal(DriverDescriptor::new(
            DriverId::new("m").unwrap(),
            "0",
            "M",
            Capabilities::READ_ONLY,
        ));
        assert_eq!(d.probe(), None);
    }

    #[test]
    fn schema_kinds_and_word_offset_defaults() {
        let d = Minimal(DriverDescriptor::new(
            DriverId::new("m").unwrap(),
            "0",
            "M",
            Capabilities::READ_ONLY,
        ));
        assert!(d.settings_schema().is_empty());
        let kinds = d.supported_kinds();
        assert!(kinds.contains(&KindName::Numeric(DataType::Bit)));
        assert!(kinds.contains(&KindName::Numeric(DataType::F32)));
        assert!(!kinds.contains(&KindName::Numeric(DataType::F64)));
        assert!(!kinds.contains(&KindName::Numeric(DataType::I64)));
        assert!(!kinds.contains(&KindName::Numeric(DataType::U64)));
        assert!(!kinds.contains(&KindName::String));
        let err = d.offset_words("A1", 1).unwrap_err();
        assert!(err.message().contains("ワード単位のオフセット"));
    }

    #[test]
    fn secret_keys_default_is_empty_and_the_trait_is_object_safe() {
        let d: Box<dyn DriverBase> = Box::new(Minimal(DriverDescriptor::new(
            DriverId::new("m").unwrap(),
            "0",
            "M",
            Capabilities::READ_ONLY,
        )));
        assert!(d.secret_keys().is_empty());
        assert_eq!(d.descriptor().id.as_str(), "m");
    }

    /// A descriptor written before `settings_version` existed reads as 1, and
    /// the field round-trips.
    #[cfg(feature = "serde")]
    #[test]
    fn settings_version_serde_defaults_to_one() {
        let d = DriverDescriptor::new(
            DriverId::new("echo").unwrap(),
            "0.1.0",
            "Echo",
            Capabilities::READ_ONLY,
        )
        .with_settings_version(2);
        let json = serde_json::to_string(&d).unwrap();
        assert_eq!(serde_json::from_str::<DriverDescriptor>(&json).unwrap(), d);

        let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
        value.as_object_mut().unwrap().remove("settings_version");
        let old: DriverDescriptor = serde_json::from_value(value).unwrap();
        assert_eq!(old.settings_version, 1);
    }
}
