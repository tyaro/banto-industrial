//! [`DriverRegistry`]: driver id → implementation
//! (docs/banto-driver-api-design.md §6.1).
//!
//! A plain `BTreeMap` lookup; no logic. In R1 the host builds it (the
//! `banto-drivers` assembler) and only the tripwire tests read it; the
//! production lookups (`DriverTable::is_supported`) move to it in R2b
//! (design §6.2).
//!
//! ## Why [`DriverEntry`] keeps a separate `&dyn DriverBase`
//!
//! [`DriverEntry::base`] gives the static half of any kind of driver. With
//! trait upcasting (`&dyn FrameDriver` → `&dyn DriverBase`, stable since
//! Rust 1.86) it would be a plain coercion, but this crate's MSRV is 1.85
//! (design §9.1). So the constructors also keep a private forwarding wrapper
//! as an `Arc<dyn DriverBase>`; nothing about it is public. When the MSRV
//! reaches 1.86 the wrapper can go without an API change.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use crate::address::{DriverAddress, DriverId};
use crate::driver::{AddressError, DriverBase, DriverDescriptor};
use crate::frame::FrameDriver;
use crate::session::SessionDriver;
use crate::settings::{FieldError, Settings};
use crate::types::TagKind;

/// How a driver does its I/O (design §4.4, §5).
#[non_exhaustive]
#[derive(Clone)]
pub enum DriverKind {
    /// A frame-type driver: the engine owns the transport.
    Frame(Arc<dyn FrameDriver>),
    /// A session-type driver: it owns its I/O.
    Session(Arc<dyn SessionDriver>),
    /// **R1 only.** A built-in whose identification, settings and address
    /// functions are in API form, while its I/O is wired by the host through
    /// a separate table (`banto_broker::DriverTable`). R2b-3 moves the
    /// built-ins to [`Frame`](Self::Frame) and removes this variant (a 0.x
    /// minor break, design §9.1).
    HostBound(Arc<dyn DriverBase>),
}

impl fmt::Debug for DriverKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (kind, id) = match self {
            DriverKind::Frame(d) => ("Frame", &d.descriptor().id),
            DriverKind::Session(d) => ("Session", &d.descriptor().id),
            DriverKind::HostBound(d) => ("HostBound", &d.descriptor().id),
        };
        f.debug_tuple(kind).field(id).finish()
    }
}

/// One registered driver. Holds no descriptor of its own (it would
/// duplicate `base().descriptor()`).
#[non_exhaustive]
#[derive(Clone)]
pub struct DriverEntry {
    kind: DriverKind,
    /// `kind`'s driver as a `DriverBase` (see the module doc).
    base: Arc<dyn DriverBase>,
}

impl DriverEntry {
    /// A frame-type driver.
    pub fn frame(driver: Arc<dyn FrameDriver>) -> Self {
        DriverEntry {
            base: Arc::new(FrameBase(Arc::clone(&driver))),
            kind: DriverKind::Frame(driver),
        }
    }

    /// A session-type driver.
    pub fn session(driver: Arc<dyn SessionDriver>) -> Self {
        DriverEntry {
            base: Arc::new(SessionBase(Arc::clone(&driver))),
            kind: DriverKind::Session(driver),
        }
    }

    /// **R1 only**: a host-bound built-in (see [`DriverKind::HostBound`]).
    pub fn host_bound(driver: Arc<dyn DriverBase>) -> Self {
        DriverEntry {
            base: Arc::clone(&driver),
            kind: DriverKind::HostBound(driver),
        }
    }

    /// The static half, whatever the kind.
    pub fn base(&self) -> &dyn DriverBase {
        &*self.base
    }

    /// `base().descriptor()`.
    pub fn descriptor(&self) -> &DriverDescriptor {
        self.base().descriptor()
    }

    /// How the driver does its I/O.
    pub fn kind(&self) -> &DriverKind {
        &self.kind
    }
}

impl fmt::Debug for DriverEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DriverEntry")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

/// The registered drivers, by id (design §6.1). One implementation per id
/// in R1; R2b-4 may add per-connection alternates with a new, defaulted
/// method.
#[derive(Default)]
pub struct DriverRegistry {
    entries: BTreeMap<DriverId, DriverEntry>,
}

impl DriverRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add `entry` under its descriptor's id. An id already present is
    /// refused (a built-in id cannot be overridden, plugin-design §4.2).
    pub fn register(&mut self, entry: DriverEntry) -> Result<(), RegistryError> {
        let id = entry.descriptor().id.clone();
        if self.entries.contains_key(&id) {
            return Err(RegistryError::DuplicateId(id));
        }
        self.entries.insert(id, entry);
        Ok(())
    }

    /// The driver registered as `id`.
    pub fn get(&self, id: &str) -> Option<&DriverEntry> {
        self.entries.get(id)
    }

    /// Whether `id` is registered.
    pub fn contains(&self, id: &str) -> bool {
        self.entries.contains_key(id)
    }

    /// The registered ids, in order.
    pub fn ids(&self) -> impl Iterator<Item = &DriverId> {
        self.entries.keys()
    }

    /// How many drivers are registered.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether none is.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl fmt::Debug for DriverRegistry {
    /// Only the ids: the drivers themselves are `Arc<dyn ..>`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DriverRegistry")
            .field("ids", &self.entries.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// Why [`DriverRegistry::register`] refused.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    /// A driver with this id is already registered.
    DuplicateId(DriverId),
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RegistryError::DuplicateId(id) => {
                write!(f, "ドライバ ID {id} はすでに登録されています")
            }
        }
    }
}

impl std::error::Error for RegistryError {}

/// Forwards every [`DriverBase`] method - including the defaulted ones, so a
/// driver's override is not lost. **A method added to `DriverBase` must be
/// forwarded here and in `SessionBase` too** (the `forwards_every_method`
/// test catches a missing one for the current methods).
macro_rules! forward_driver_base {
    ($wrapper:ident, $inner:ty) => {
        struct $wrapper(Arc<$inner>);

        impl DriverBase for $wrapper {
            fn descriptor(&self) -> &DriverDescriptor {
                self.0.descriptor()
            }
            fn validate_config(&self, settings: &Settings) -> Vec<FieldError> {
                self.0.validate_config(settings)
            }
            fn secret_keys(&self) -> Vec<String> {
                self.0.secret_keys()
            }
            fn parse_address(
                &self,
                text: &str,
                kind: &TagKind,
            ) -> Result<DriverAddress, AddressError> {
                self.0.parse_address(text, kind)
            }
            fn offset_address(
                &self,
                canonical: &str,
                kind: &TagKind,
                n: i64,
            ) -> Result<String, AddressError> {
                self.0.offset_address(canonical, kind, n)
            }
            fn address_help(&self) -> String {
                self.0.address_help()
            }
        }
    };
}

forward_driver_base!(FrameBase, dyn FrameDriver);
forward_driver_base!(SessionBase, dyn SessionDriver);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::Capabilities;
    use crate::error::FatalError;
    use crate::frame::FrameConnection;
    use crate::session::{BoxFuture, ConnectContext, Session};
    use crate::types::DataType;

    /// Answers every `DriverBase` method with something derived from its id,
    /// so a wrapper that falls back to a default (or to another method) is
    /// visible.
    struct Probe(DriverDescriptor);

    impl Probe {
        fn new(id: &str) -> Self {
            Probe(DriverDescriptor::new(
                DriverId::new(id).unwrap(),
                "1.2.3",
                id,
                Capabilities::READ_WRITE,
            ))
        }
        fn id(&self) -> &str {
            self.0.id.as_str()
        }
    }

    impl DriverBase for Probe {
        fn descriptor(&self) -> &DriverDescriptor {
            &self.0
        }
        fn validate_config(&self, settings: &Settings) -> Vec<FieldError> {
            vec![FieldError::new(
                self.id(),
                settings.get("x").unwrap_or("-").to_owned(),
            )]
        }
        fn secret_keys(&self) -> Vec<String> {
            vec![format!("{}-secret", self.id())]
        }
        fn parse_address(&self, text: &str, kind: &TagKind) -> Result<DriverAddress, AddressError> {
            Ok(DriverAddress::new(
                self.0.id.clone(),
                text,
                "a",
                true,
                kind.word_count(),
            ))
        }
        fn offset_address(
            &self,
            canonical: &str,
            _kind: &TagKind,
            n: i64,
        ) -> Result<String, AddressError> {
            Ok(format!("{canonical}+{n}"))
        }
        fn address_help(&self) -> String {
            format!("{} help", self.id())
        }
    }

    impl FrameDriver for Probe {
        fn create_connection(
            &self,
            _settings: &Settings,
        ) -> Result<Box<dyn FrameConnection>, FatalError> {
            Err(FatalError::InvalidConfig("probe".into()))
        }
    }

    impl SessionDriver for Probe {
        fn connect(
            &self,
            _settings: &Settings,
            _cx: ConnectContext,
        ) -> BoxFuture<'static, Result<Box<dyn Session>, FatalError>> {
            Box::pin(async { Err(FatalError::Io("probe".into())) })
        }
    }

    fn assert_forwards(entry: &DriverEntry, id: &str) {
        let base = entry.base();
        assert_eq!(entry.descriptor().id.as_str(), id);
        assert_eq!(base.descriptor().version, "1.2.3");
        let settings = Settings::from_pairs([("x", "1")]).unwrap();
        assert_eq!(
            base.validate_config(&settings),
            vec![FieldError::new(id, "1")]
        );
        assert_eq!(base.secret_keys(), vec![format!("{id}-secret")]);
        let kind = TagKind::numeric(DataType::F64);
        let a = base.parse_address("A1", &kind).unwrap();
        assert_eq!((a.driver.as_str(), a.word_count), (id, 4));
        assert_eq!(base.offset_address("A1", &kind, 2).unwrap(), "A1+2");
        assert_eq!(base.address_help(), format!("{id} help"));
    }

    #[test]
    fn forwards_every_method_for_each_kind() {
        let frame = DriverEntry::frame(Arc::new(Probe::new("f")));
        assert!(matches!(frame.kind(), DriverKind::Frame(_)));
        assert_forwards(&frame, "f");

        let session = DriverEntry::session(Arc::new(Probe::new("s")));
        assert!(matches!(session.kind(), DriverKind::Session(_)));
        assert_forwards(&session, "s");

        let host = DriverEntry::host_bound(Arc::new(Probe::new("h")));
        assert!(matches!(host.kind(), DriverKind::HostBound(_)));
        assert_forwards(&host, "h");
    }

    #[test]
    fn register_get_contains_and_refuse_duplicates() {
        let mut reg = DriverRegistry::new();
        assert!(reg.is_empty());
        reg.register(DriverEntry::host_bound(Arc::new(Probe::new("slmp"))))
            .unwrap();
        reg.register(DriverEntry::frame(Arc::new(Probe::new("modbus-tcp"))))
            .unwrap();
        assert_eq!(reg.len(), 2);
        assert!(reg.contains("slmp"));
        assert!(!reg.contains("kv"));
        assert_eq!(
            reg.get("modbus-tcp").unwrap().descriptor().id.as_str(),
            "modbus-tcp"
        );
        assert!(reg.get("kv").is_none());
        let ids: Vec<&str> = reg.ids().map(DriverId::as_str).collect();
        assert_eq!(ids, ["modbus-tcp", "slmp"]);

        // A duplicate is refused whatever its kind, and the first stays.
        let err = reg
            .register(DriverEntry::session(Arc::new(Probe::new("slmp"))))
            .unwrap_err();
        assert_eq!(
            err,
            RegistryError::DuplicateId(DriverId::new("slmp").unwrap())
        );
        assert_eq!(err.to_string(), "ドライバ ID slmp はすでに登録されています");
        assert!(matches!(
            reg.get("slmp").unwrap().kind(),
            DriverKind::HostBound(_)
        ));
        assert_eq!(reg.len(), 2);
    }

    #[test]
    fn debug_shows_ids_only() {
        let mut reg = DriverRegistry::new();
        reg.register(DriverEntry::frame(Arc::new(Probe::new("echo"))))
            .unwrap();
        assert_eq!(
            format!("{reg:?}"),
            r#"DriverRegistry { ids: [DriverId("echo")] }"#
        );
        assert_eq!(
            format!("{:?}", reg.get("echo").unwrap()),
            r#"DriverEntry { kind: Frame(DriverId("echo")), .. }"#
        );
    }
}
