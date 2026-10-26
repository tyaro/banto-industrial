//! Wasmtime Component Model frame driver. The normal loader is read-only; the
//! versioned write export is available only to isolated controlled fixtures.
//! The host owns transport, timeouts and frame limits.

mod manifest;
mod runtime;

pub use manifest::{Manifest, ManifestError};
pub use runtime::{Diagnostic, Diagnostics, RuntimeCause, RuntimeError};

use std::collections::BTreeSet;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use api::{
    AddressError, DataType, DriverAddress, DriverBase, DriverDescriptor, DriverId, FatalError,
    FieldError, FieldGroup, FieldKind, FrameCheck, FrameConnection, FrameDriver, FrameRule,
    ItemError, KindName, Planned, PrefixEncoding, PrefixSpec, RawValue, SettingField, SettingValue,
    Settings, SettingsSchema, Step, TagKind, TagSpec, WriteItem, WriteProgress,
};
use banto_driver_api as api;
use runtime::write_v1::{
    banto::driver::types as write_types,
    exports::banto::driver::{session as write_session, write_v1 as write},
};
use runtime::{
    banto::driver::types,
    exports::banto::driver::{address, config, read, session},
    Instance, Runtime,
};

#[derive(Debug)]
pub enum LoadError {
    Manifest(ManifestError),
    Runtime(RuntimeError),
    Contract(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Manifest(e) => e.fmt(f),
            Self::Runtime(e) => e.fmt(f),
            Self::Contract(e) => f.write_str(e),
        }
    }
}
impl std::error::Error for LoadError {}

/// The only mapping from a host-runtime failure into the existing frame API.
/// The original typed cause remains in [`Diagnostics`].
fn normalize(error: RuntimeError) -> FatalError {
    FatalError::Protocol(format!(
        "WASM {} failed ({:?})",
        error.0.operation, error.0.cause
    ))
}

pub struct WasmDriver {
    descriptor: DriverDescriptor,
    runtime: Arc<Runtime>,
    controlled_write_target: ControlledWriteTarget,
    probe: Option<(String, TagKind)>,
    supported: Vec<KindName>,
    secrets: Vec<String>,
    schema: SettingsSchema,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ControlledWriteTarget {
    None,
    #[cfg(feature = "controlled-write-fixture")]
    Loopback,
    #[cfg(feature = "controlled-live-write-fixture")]
    RTestPlc,
}

impl ControlledWriteTarget {
    fn allows_write(self) -> bool {
        self != Self::None
    }

    fn accepts_settings(self, settings: &Settings) -> bool {
        let _ = settings;
        match self {
            Self::None => true,
            #[cfg(feature = "controlled-write-fixture")]
            Self::Loopback => settings
                .iter()
                .any(|setting| setting.key == "host" && setting.value == "127.0.0.1"),
            #[cfg(feature = "controlled-live-write-fixture")]
            Self::RTestPlc => {
                settings
                    .iter()
                    .any(|setting| setting.key == "host" && setting.value == "192.168.11.200")
                    && settings
                        .iter()
                        .any(|setting| setting.key == "port" && setting.value == "3109")
            }
        }
    }
}

impl WasmDriver {
    /// Validate metadata, compile a read-only component, and evaluate its
    /// stateless exports before making it visible to a registry.
    pub fn load(manifest: &[u8], component: &[u8]) -> Result<Arc<Self>, LoadError> {
        Self::load_inner(manifest, component, ControlledWriteTarget::None)
    }

    /// Explicit entry point for isolated loopback fixtures. Normal
    /// registration remains read-only; production startup never calls this.
    #[cfg(feature = "controlled-write-fixture")]
    pub fn load_controlled_write_fixture(
        manifest: &[u8],
        component: &[u8],
    ) -> Result<Arc<Self>, LoadError> {
        Self::load_inner(manifest, component, ControlledWriteTarget::Loopback)
    }

    /// Explicit test-only loader for the approved R test PLC. The caller must
    /// also verify its TransportSpec before dialing; settings are not the wire
    /// destination. Production registration always uses `load` instead.
    #[cfg(feature = "controlled-live-write-fixture")]
    pub fn load_controlled_r_test_fixture(
        manifest: &[u8],
        component: &[u8],
    ) -> Result<Arc<Self>, LoadError> {
        Self::load_inner(manifest, component, ControlledWriteTarget::RTestPlc)
    }

    fn load_inner(
        manifest: &[u8],
        component: &[u8],
        controlled_write_target: ControlledWriteTarget,
    ) -> Result<Arc<Self>, LoadError> {
        let manifest = Manifest::parse(manifest).map_err(LoadError::Manifest)?;
        if manifest.requests_write != controlled_write_target.allows_write() {
            return Err(LoadError::Contract(
                "write-v1 is available only through the controlled fixture loader".into(),
            ));
        }
        if controlled_write_target.allows_write()
            && manifest.descriptor.id.as_str() != "slmp-write-fixture"
        {
            return Err(LoadError::Contract(
                "controlled write fixture requires the isolated SLMP driver id".into(),
            ));
        }
        let runtime =
            Arc::new(Runtime::new(component, manifest.requests_write).map_err(LoadError::Runtime)?);
        let mut instance = runtime.instantiate().map_err(LoadError::Runtime)?;
        let probe = instance
            .driver
            .banto_driver_address()
            .call_probe(&mut instance.store)
            .map_err(|e| LoadError::Runtime(runtime.record("address.probe", e)))?
            .map(|tag| {
                if tag.data_type != "u16" || tag.string_words.is_some() {
                    Err(LoadError::Contract(
                        "initial WASM probe must be a single U16".into(),
                    ))
                } else {
                    Ok((tag.address, TagKind::numeric(DataType::U16)))
                }
            })
            .transpose()?;
        let supported = instance
            .driver
            .banto_driver_address()
            .call_supported_kinds(&mut instance.store)
            .map_err(|e| LoadError::Runtime(runtime.record("address.supported-kinds", e)))?
            .into_iter()
            .map(|s| {
                KindName::parse(&s)
                    .ok_or_else(|| LoadError::Contract(format!("unknown supported kind: {s}")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        validate_supported_kinds(&supported)?;
        let secrets = instance
            .driver
            .banto_driver_config()
            .call_secret_keys(&mut instance.store)
            .map_err(|e| LoadError::Runtime(runtime.record("config.secret-keys", e)))?;
        let fields: Vec<SettingField> = instance
            .driver
            .banto_driver_config()
            .call_schema(&mut instance.store)
            .map_err(|e| LoadError::Runtime(runtime.record("config.schema", e)))?
            .into_iter()
            .map(convert_field)
            .collect();
        validate_secret_fields(&fields, &secrets)?;
        // Only expose write capability after the extended world has actually
        // bound on this component. The manifest alone never grants it.
        let mut descriptor = manifest.descriptor;
        descriptor.capabilities.write = manifest.requests_write;
        Ok(Arc::new(Self {
            descriptor,
            runtime,
            controlled_write_target,
            probe,
            supported,
            secrets,
            schema: SettingsSchema::new(fields),
        }))
    }

    pub fn diagnostics(&self) -> Diagnostics {
        self.runtime.diagnostics()
    }

    fn with_instance<T>(
        &self,
        operation: &'static str,
        f: impl FnOnce(&mut Instance) -> wasmtime::Result<T>,
    ) -> Result<T, RuntimeError> {
        let mut instance = self.runtime.instantiate()?;
        f(&mut instance).map_err(|e| self.runtime.record(operation, e))
    }
}

/// U16 remains the required probe kind. Other numeric kinds use the existing
/// WIT int/uint/float variants and are validated again at the value boundary.
fn validate_supported_kinds(supported: &[KindName]) -> Result<(), LoadError> {
    let allowed = [
        KindName::Numeric(DataType::U16),
        KindName::Numeric(DataType::U32),
        KindName::Numeric(DataType::I16),
        KindName::Numeric(DataType::I32),
        KindName::Numeric(DataType::I64),
        KindName::Numeric(DataType::U64),
        KindName::Numeric(DataType::F32),
        KindName::Numeric(DataType::F64),
    ];
    if !supported.contains(&allowed[0])
        || supported.len() > allowed.len()
        || supported
            .iter()
            .enumerate()
            .any(|(index, kind)| !allowed.contains(kind) || supported[..index].contains(kind))
    {
        return Err(LoadError::Contract(
            "WASM component must support U16 and may add the other seven numeric read kinds once each"
                .into(),
        ));
    }
    Ok(())
}

fn convert_field(field: config::SettingField) -> SettingField {
    let kind = match field.kind {
        config::FieldKind::Text(v) => FieldKind::Text {
            max_len: v.max_len,
            placeholder: v.placeholder,
        },
        config::FieldKind::Host => FieldKind::Host,
        config::FieldKind::Integer(v) => FieldKind::Integer {
            min: v.min,
            max: v.max,
        },
        config::FieldKind::Choice(options) => FieldKind::Choice {
            options: options
                .into_iter()
                .map(|o| api::ChoiceOption::new(o.value, o.label))
                .collect(),
        },
        config::FieldKind::Bool => FieldKind::Bool,
        config::FieldKind::Secret => FieldKind::Secret,
    };
    let mut result = SettingField::new(field.key, field.label, kind);
    result.help = field.help;
    result.required = field.required;
    result.default = field.default.map(|value| match value {
        config::SettingValue::Text(s) => SettingValue::Text(s),
        config::SettingValue::Integer(n) => SettingValue::Integer(n),
        config::SettingValue::Boolean(b) => SettingValue::Bool(b),
    });
    result.group = match field.group {
        config::FieldGroup::Basic => FieldGroup::Basic,
        config::FieldGroup::Advanced => FieldGroup::Advanced,
    };
    result
}

fn validate_secret_fields(fields: &[SettingField], secrets: &[String]) -> Result<(), LoadError> {
    let declared: BTreeSet<&str> = secrets.iter().map(String::as_str).collect();
    let field_keys: BTreeSet<&str> = fields.iter().map(|field| field.key.as_str()).collect();
    let secret_field_count = fields
        .iter()
        .filter(|field| matches!(field.kind, FieldKind::Secret))
        .count();
    let in_schema: BTreeSet<&str> = fields
        .iter()
        .filter(|field| matches!(field.kind, FieldKind::Secret))
        .map(|field| field.key.as_str())
        .collect();
    if field_keys.len() != fields.len()
        || declared.len() != secrets.len()
        || in_schema.len() != secret_field_count
        || declared != in_schema
    {
        return Err(LoadError::Contract(
            "config schema keys must be unique and secret-keys must match Secret fields".into(),
        ));
    }
    Ok(())
}

/// Check protocol-independent address metadata before passing it to the Hub.
/// The guest owns the grammar, area, and device-specific index limits.
fn checked_parsed_address(
    driver: &DriverId,
    kind: &TagKind,
    parsed: address::Parsed,
) -> Result<DriverAddress, AddressError> {
    let expected_words: u16 = match kind {
        TagKind::Numeric(DataType::U16 | DataType::I16) => 1,
        TagKind::Numeric(DataType::U32 | DataType::I32 | DataType::F32) => 2,
        TagKind::Numeric(DataType::U64 | DataType::I64 | DataType::F64) => 4,
        _ => {
            return Err(AddressError::new(
                "WASM guest supports only numeric register reads",
            ))
        }
    };
    if parsed.canonical.is_empty()
        || parsed.area.is_empty()
        || parsed.word_count != u32::from(expected_words)
        || parsed.bit.is_some()
    {
        return Err(AddressError::new(
            "WASM address metadata violates the tag contract",
        ));
    }
    if parsed
        .index
        .is_some_and(|index| index.checked_add(u64::from(expected_words - 1)).is_none())
    {
        return Err(AddressError::new("WASM address range overflows"));
    }
    let mut address = DriverAddress::new(
        driver.clone(),
        parsed.canonical,
        parsed.area,
        parsed.writable_area,
        expected_words,
    );
    if let Some(index) = parsed.index {
        address = address.with_index(index);
    }
    Ok(address)
}

fn wit_settings(settings: &Settings) -> Vec<types::Setting> {
    settings
        .iter()
        .map(|s| types::Setting {
            key: s.key.clone(),
            value: s.value.clone(),
        })
        .collect()
}

fn kind_parts(kind: &TagKind) -> (String, Option<u32>) {
    match kind {
        TagKind::Numeric(value) => (value.to_string(), None),
        TagKind::Str { words, .. } => ("string".into(), Some(u32::from(*words))),
        _ => ("unsupported".into(), None),
    }
}

fn wit_specs(specs: &[TagSpec]) -> Vec<types::TagSpec> {
    specs
        .iter()
        .map(|spec| {
            let (data_type, string_words) = kind_parts(&spec.kind);
            types::TagSpec {
                address: spec.address.canonical.clone(),
                data_type,
                string_words,
            }
        })
        .collect()
}

fn api_fatal(value: types::FatalError) -> FatalError {
    match value {
        types::FatalError::Framing(m) => FatalError::Framing(m),
        types::FatalError::Protocol(m) => FatalError::Protocol(m),
        types::FatalError::InvalidConfig(m) => FatalError::InvalidConfig(m),
    }
}

fn api_step(step: session::Step) -> Step {
    let rule = match step.expect {
        session::FrameRule::None => FrameRule::None,
        session::FrameRule::Fixed(n) => FrameRule::Fixed(n),
        session::FrameRule::LengthPrefixed(p) => FrameRule::LengthPrefixed(
            PrefixSpec::new(p.offset, p.width, p.big_endian, p.adjust).with_encoding(
                match p.encoding {
                    session::PrefixEncoding::Binary => PrefixEncoding::Binary,
                    session::PrefixEncoding::AsciiHex => PrefixEncoding::AsciiHex,
                    session::PrefixEncoding::AsciiDec => PrefixEncoding::AsciiDec,
                },
            ),
        ),
        session::FrameRule::Terminator(bytes) => FrameRule::Terminator(bytes),
        session::FrameRule::PluginDecides => FrameRule::DriverDecides,
    };
    Step::new(step.send, rule, step.max_len)
}

fn wit_step(step: &Step) -> Result<session::Step, FatalError> {
    let rule = match &step.expect {
        FrameRule::None => session::FrameRule::None,
        FrameRule::Fixed(n) => session::FrameRule::Fixed(*n),
        FrameRule::LengthPrefixed(p) => session::FrameRule::LengthPrefixed(session::PrefixSpec {
            offset: p.offset,
            width: p.width,
            big_endian: p.big_endian,
            adjust: p.adjust,
            encoding: match p.encoding {
                PrefixEncoding::Binary => session::PrefixEncoding::Binary,
                PrefixEncoding::AsciiHex => session::PrefixEncoding::AsciiHex,
                PrefixEncoding::AsciiDec => session::PrefixEncoding::AsciiDec,
                _ => {
                    return Err(FatalError::Protocol(
                        "unsupported frame prefix encoding".into(),
                    ))
                }
            },
        }),
        FrameRule::Terminator(bytes) => session::FrameRule::Terminator(bytes.clone()),
        FrameRule::DriverDecides => session::FrameRule::PluginDecides,
        _ => return Err(FatalError::Protocol("unsupported frame rule".into())),
    };
    Ok(session::Step {
        send: step.send.clone(),
        expect: rule,
        max_len: step.max_len,
    })
}

fn wit_planned(planned: &Planned) -> Result<read::Planned, FatalError> {
    Ok(read::Planned {
        step: wit_step(&planned.step)?,
        covers: planned.covers.clone(),
    })
}

/// Keep the requested kind's variant and exact numeric range checks at the
/// WASM boundary; the engine's RawValue conversion is not a substitute.
fn checked_value(kind: &TagKind, value: types::Value) -> Result<RawValue, FatalError> {
    match (kind, value) {
        (TagKind::Numeric(DataType::U16), types::Value::Uint(n)) if u16::try_from(n).is_ok() => {
            Ok(RawValue::Uint(n))
        }
        (TagKind::Numeric(DataType::U32), types::Value::Uint(n)) if u32::try_from(n).is_ok() => {
            Ok(RawValue::Uint(n))
        }
        (TagKind::Numeric(DataType::I16), types::Value::Int(n)) if i16::try_from(n).is_ok() => {
            Ok(RawValue::Int(n))
        }
        (TagKind::Numeric(DataType::I32), types::Value::Int(n)) if i32::try_from(n).is_ok() => {
            Ok(RawValue::Int(n))
        }
        (TagKind::Numeric(DataType::I64), types::Value::Int(n)) => Ok(RawValue::Int(n)),
        (TagKind::Numeric(DataType::U64), types::Value::Uint(n)) => Ok(RawValue::Uint(n)),
        (TagKind::Numeric(DataType::F32), types::Value::Float(n))
            if n.is_nan() || n as f32 as f64 == n =>
        {
            Ok(RawValue::Float(n))
        }
        (TagKind::Numeric(DataType::F64), types::Value::Float(n)) => Ok(RawValue::Float(n)),
        _ => Err(FatalError::Protocol(
            "WASM result has wrong type or range".into(),
        )),
    }
}

/// The controlled fixture has an independent host allowlist before the guest
/// can produce any frame. The guest still checks the live R model and SD range.
fn controlled_write_item(
    items: &[WriteItem],
    driver_id: &DriverId,
) -> Result<write::WriteItem, WriteProgress> {
    let refused = || {
        WriteProgress::Done(vec![
            Err(ItemError::Unsupported(
                "controlled WASM write accepts only one D3010/u16 fixture item".into(),
            ));
            items.len()
        ])
    };
    if items.len() != 1 {
        return Err(refused());
    }
    let item = &items[0];
    let address = &item.spec.address;
    if address.driver != *driver_id
        || address.canonical != "D3010"
        || address.area != "D"
        || address.index != Some(3010)
        || address.bit.is_some()
        || address.word_count != 1
        || !address.writable_area
        || !matches!(item.spec.kind, TagKind::Numeric(DataType::U16))
    {
        return Err(refused());
    }
    let api::Value::F64(value) = item.value else {
        return Err(refused());
    };
    if !value.is_finite() || value.fract() != 0.0 || !(0.0..=65535.0).contains(&value) {
        return Err(refused());
    }
    Ok(write::WriteItem {
        spec: write_types::TagSpec {
            address: address.canonical.clone(),
            data_type: "u16".into(),
            string_words: None,
        },
        value: write_types::Value::Float(value),
    })
}

fn api_write_fatal(value: write_types::FatalError) -> FatalError {
    match value {
        write_types::FatalError::Framing(m) => FatalError::Framing(m),
        write_types::FatalError::Protocol(m) => FatalError::Protocol(m),
        write_types::FatalError::InvalidConfig(m) => FatalError::InvalidConfig(m),
    }
}

fn api_write_item_error(value: write_types::ItemError) -> ItemError {
    match value {
        write_types::ItemError::Device(e) => {
            ItemError::Device(api::DeviceError::new(e.code, e.message))
        }
        write_types::ItemError::Unsupported(m) => ItemError::Unsupported(m),
        write_types::ItemError::Rejected(m) => ItemError::Rejected(m),
    }
}

fn api_write_step(step: write_session::Step) -> Step {
    let rule = match step.expect {
        write_session::FrameRule::None => FrameRule::None,
        write_session::FrameRule::Fixed(n) => FrameRule::Fixed(n),
        write_session::FrameRule::LengthPrefixed(p) => FrameRule::LengthPrefixed(
            PrefixSpec::new(p.offset, p.width, p.big_endian, p.adjust).with_encoding(
                match p.encoding {
                    write_session::PrefixEncoding::Binary => PrefixEncoding::Binary,
                    write_session::PrefixEncoding::AsciiHex => PrefixEncoding::AsciiHex,
                    write_session::PrefixEncoding::AsciiDec => PrefixEncoding::AsciiDec,
                },
            ),
        ),
        write_session::FrameRule::Terminator(bytes) => FrameRule::Terminator(bytes),
        write_session::FrameRule::PluginDecides => FrameRule::DriverDecides,
    };
    Step::new(step.send, rule, step.max_len)
}

impl DriverBase for WasmDriver {
    fn descriptor(&self) -> &DriverDescriptor {
        &self.descriptor
    }

    fn validate_config(&self, settings: &Settings) -> Vec<FieldError> {
        match self.with_instance("config.validate", |i| {
            i.driver
                .banto_driver_config()
                .call_validate(&mut i.store, &wit_settings(settings))
        }) {
            Ok(errors) => errors
                .into_iter()
                .map(|e| FieldError::new(e.key, e.message))
                .collect(),
            Err(e) => vec![FieldError::new(
                "settings",
                format!("WASM validation failed: {:?}", e.0.cause),
            )],
        }
    }

    fn secret_keys(&self) -> Vec<String> {
        self.secrets.clone()
    }

    fn parse_address(&self, text: &str, kind: &TagKind) -> Result<DriverAddress, AddressError> {
        let (data_type, string_words) = kind_parts(kind);
        let result = self
            .with_instance("address.parse", |i| {
                i.driver.banto_driver_address().call_parse(
                    &mut i.store,
                    text,
                    &data_type,
                    string_words,
                )
            })
            .map_err(|e| AddressError::new(e.to_string()))?
            .map_err(AddressError::new)?;
        // The guest defines its grammar. Round-trip its canonical form instead
        // of imposing a Modbus spelling or deriving an index in the host.
        let round_trip = self
            .with_instance("address.parse", |i| {
                i.driver.banto_driver_address().call_parse(
                    &mut i.store,
                    &result.canonical,
                    &data_type,
                    string_words,
                )
            })
            .map_err(|e| AddressError::new(e.to_string()))?
            .map_err(AddressError::new)?;
        if result.canonical != round_trip.canonical
            || result.area != round_trip.area
            || result.writable_area != round_trip.writable_area
            || result.bit != round_trip.bit
            || result.word_count != round_trip.word_count
            || result.index != round_trip.index
        {
            return Err(AddressError::new("WASM canonical address is not stable"));
        }
        checked_parsed_address(&self.descriptor.id, kind, result)
    }

    fn offset_address(
        &self,
        canonical: &str,
        kind: &TagKind,
        n: i64,
    ) -> Result<String, AddressError> {
        let (data_type, string_words) = kind_parts(kind);
        self.with_instance("address.offset", |i| {
            i.driver.banto_driver_address().call_offset(
                &mut i.store,
                canonical,
                &data_type,
                string_words,
                n,
            )
        })
        .map_err(|e| AddressError::new(e.to_string()))?
        .map_err(AddressError::new)
    }

    fn address_help(&self) -> String {
        self.with_instance("address.help", |i| {
            i.driver.banto_driver_address().call_help(&mut i.store)
        })
        .unwrap_or_else(|e| format!("WASM address help failed: {:?}", e.0.cause))
    }

    fn probe(&self) -> Option<(String, TagKind)> {
        self.probe.clone()
    }
    fn settings_schema(&self) -> SettingsSchema {
        self.schema.clone()
    }
    fn supported_kinds(&self) -> Vec<KindName> {
        self.supported.clone()
    }

    fn offset_words(&self, canonical: &str, n: i64) -> Result<String, AddressError> {
        self.with_instance("address.offset-words", |i| {
            i.driver
                .banto_driver_address()
                .call_offset_words(&mut i.store, canonical, n)
        })
        .map_err(|e| AddressError::new(e.to_string()))?
        .map_err(AddressError::new)
    }
}

impl FrameDriver for WasmDriver {
    fn create_connection(
        &self,
        settings: &Settings,
    ) -> Result<Box<dyn FrameConnection>, FatalError> {
        if !self.controlled_write_target.accepts_settings(settings) {
            return Err(FatalError::InvalidConfig(
                "controlled WASM write fixture endpoint mismatch".into(),
            ));
        }
        let mut instance = self.runtime.instantiate().map_err(normalize)?;
        let resource = instance
            .driver
            .banto_driver_session()
            .connection()
            .call_create(&mut instance.store, &wit_settings(settings))
            .map_err(|e| normalize(self.runtime.record("connection.create", e)))?
            .map_err(api_fatal)?;
        Ok(Box::new(WasmConnection {
            instance: Mutex::new(instance),
            resource,
            runtime: self.runtime.clone(),
            failed: AtomicBool::new(false),
            controlled_write_fixture: self.controlled_write_target.allows_write(),
            driver_id: self.descriptor.id.clone(),
        }))
    }
}

struct WasmConnection {
    instance: Mutex<Instance>,
    resource: wasmtime::component::ResourceAny,
    runtime: Arc<Runtime>,
    failed: AtomicBool,
    controlled_write_fixture: bool,
    driver_id: DriverId,
}

impl WasmConnection {
    fn call<T>(
        &self,
        operation: &'static str,
        f: impl FnOnce(&mut Instance, wasmtime::component::ResourceAny) -> wasmtime::Result<T>,
    ) -> Result<T, FatalError> {
        if self.failed.load(Ordering::Acquire) {
            return Err(FatalError::Protocol(
                "WASM connection was discarded after a runtime failure".into(),
            ));
        }
        let mut instance = self
            .instance
            .lock()
            .map_err(|_| FatalError::Protocol("WASM store lock poisoned".into()))?;
        instance.store.set_epoch_deadline(100);
        f(&mut instance, self.resource).map_err(|e| {
            self.failed.store(true, Ordering::Release);
            normalize(self.runtime.record(operation, e))
        })
    }
}

impl FrameConnection for WasmConnection {
    fn write_next(
        &mut self,
        items: &[WriteItem],
        prev_response: Option<&[u8]>,
    ) -> Result<WriteProgress, FatalError> {
        if !self.controlled_write_fixture {
            return Ok(WriteProgress::Done(vec![
                Err(ItemError::Unsupported(
                    "WASM driver is read-only".into()
                ));
                items.len()
            ]));
        }
        let item = match controlled_write_item(items, &self.driver_id) {
            Ok(item) => item,
            Err(refused) => return Ok(refused),
        };
        self.call("write-v1.write-next", |i, resource| {
            i.write_driver
                .as_ref()
                .expect("write export bound before capability enablement")
                .banto_driver_write_v1()
                .call_write_next(&mut i.store, resource, &[item], prev_response)
        })?
        .map(|progress| match progress {
            write::WriteProgress::Send(planned) => {
                WriteProgress::Send(Planned::new(api_write_step(planned.step), planned.covers))
            }
            write::WriteProgress::Done(results) => WriteProgress::Done(
                results
                    .into_iter()
                    .map(|result| result.map_err(api_write_item_error))
                    .collect(),
            ),
        })
        .map_err(api_write_fatal)
    }

    fn handshake_next(&mut self, prev_response: Option<&[u8]>) -> Result<Option<Step>, FatalError> {
        self.call("session.handshake-next", |i, resource| {
            i.driver
                .banto_driver_session()
                .connection()
                .call_handshake_next(&mut i.store, resource, prev_response)
        })?
        .map(|step| step.map(api_step))
        .map_err(api_fatal)
    }

    fn farewell(&mut self) -> Option<Step> {
        self.call("session.farewell", |i, resource| {
            i.driver
                .banto_driver_session()
                .connection()
                .call_farewell(&mut i.store, resource)
        })
        .ok()
        .flatten()
        .map(api_step)
    }

    fn is_complete(&self, step: &Step, buf: &[u8]) -> Result<FrameCheck, FatalError> {
        let step = wit_step(step)?;
        self.call("session.is-complete", |i, resource| {
            i.driver
                .banto_driver_session()
                .connection()
                .call_is_complete(&mut i.store, resource, &step, buf)
        })?
        .map(|check| match check {
            session::FrameCheck::Incomplete => FrameCheck::Incomplete,
            session::FrameCheck::Complete(len) => FrameCheck::Complete { len },
            session::FrameCheck::Discard(n) => FrameCheck::Discard(n),
        })
        .map_err(api_fatal)
    }

    fn plan_read(&mut self, specs: &[TagSpec]) -> Result<Vec<Planned>, FatalError> {
        self.call("read.plan", |i, resource| {
            i.driver
                .banto_driver_read()
                .call_plan(&mut i.store, resource, &wit_specs(specs))
        })?
        .map(|plans| {
            plans
                .into_iter()
                .map(|p| Planned::new(api_step(p.step), p.covers))
                .collect()
        })
        .map_err(api_fatal)
    }

    fn decode_read(
        &self,
        planned: &Planned,
        specs: &[TagSpec],
        response: &[u8],
    ) -> Result<Vec<Result<RawValue, ItemError>>, FatalError> {
        let plan = wit_planned(planned)?;
        let decoded = self
            .call("read.decode", |i, resource| {
                i.driver.banto_driver_read().call_decode(
                    &mut i.store,
                    resource,
                    &plan,
                    &wit_specs(specs),
                    response,
                )
            })?
            .map_err(api_fatal)?;
        if decoded.len() != planned.covers.len() {
            return Err(FatalError::Protocol(
                "WASM returned a different number of results than planned".into(),
            ));
        }
        decoded
            .into_iter()
            .enumerate()
            .map(|(index, result)| match result {
                Ok(value) => {
                    let spec = planned
                        .covers
                        .get(index)
                        .and_then(|n| specs.get(*n as usize))
                        .ok_or_else(|| {
                            FatalError::Protocol("WASM returned a result outside covers".into())
                        })?;
                    checked_value(&spec.kind, value).map(Ok)
                }
                Err(types::ItemError::Device(e)) => Ok(Err(ItemError::Device(
                    api::DeviceError::new(e.code, e.message),
                ))),
                Err(types::ItemError::Unsupported(m)) => Ok(Err(ItemError::Unsupported(m))),
                Err(types::ItemError::Rejected(m)) => Ok(Err(ItemError::Rejected(m))),
            })
            .collect()
    }
}

impl Drop for WasmConnection {
    fn drop(&mut self) {
        // A failed instance is discarded with its Store. Do not run guest
        // destruction after a trap or record a second, misleading failure.
        if self.failed.load(Ordering::Acquire) {
            return;
        }
        if let Ok(mut instance) = self.instance.lock() {
            // Connection stores may be idle for much longer than the normal
            // invocation budget; destruction starts a fresh bounded call.
            instance.store.set_epoch_deadline(100);
            if let Err(e) = self.resource.resource_drop(&mut instance.store) {
                self.runtime.record("connection.drop", e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture_write_item(value: api::Value) -> WriteItem {
        let id = DriverId::new("slmp-write-fixture").unwrap();
        let address = DriverAddress::new(id, "D3010", "D", true, 1).with_index(3010);
        WriteItem::new(
            TagSpec::new(address, TagKind::numeric(DataType::U16)),
            value,
        )
    }

    #[test]
    fn controlled_write_preflight_is_all_or_nothing_and_exactly_scoped() {
        let id = DriverId::new("slmp-write-fixture").unwrap();
        let valid = fixture_write_item(api::Value::F64(65535.0));
        assert!(controlled_write_item(std::slice::from_ref(&valid), &id).is_ok());
        for value in [f64::NAN, f64::INFINITY, -1.0, 0.5, 65536.0] {
            let item = fixture_write_item(api::Value::F64(value));
            assert!(controlled_write_item(&[item], &id).is_err());
        }
        assert!(controlled_write_item(&[fixture_write_item(api::Value::Bit(true))], &id).is_err());
        let mut wrong = valid.clone();
        wrong.spec.address.canonical = "D3000".into();
        assert!(controlled_write_item(&[wrong], &id).is_err());
        let mut wrong = valid.clone();
        wrong.spec.address.index = Some(3000);
        assert!(controlled_write_item(&[wrong], &id).is_err());
        let mut wrong = valid.clone();
        wrong.spec.address.writable_area = false;
        assert!(controlled_write_item(&[wrong], &id).is_err());
        for data_type in [
            DataType::I16,
            DataType::U32,
            DataType::I32,
            DataType::I64,
            DataType::U64,
            DataType::F32,
            DataType::F64,
        ] {
            let mut wrong = valid.clone();
            wrong.spec.kind = TagKind::numeric(data_type);
            assert!(controlled_write_item(&[wrong], &id).is_err());
        }
        let result = controlled_write_item(&[valid.clone(), valid], &id);
        assert!(
            matches!(result, Err(WriteProgress::Done(results)) if results.len() == 2 && results.iter().all(Result::is_err))
        );
    }

    #[test]
    fn normal_loader_rejects_write_manifest_before_component_execution() {
        let mut manifest: serde_json::Value =
            serde_json::from_slice(include_bytes!("../tests/manifest.json")).unwrap();
        manifest["capabilities"] = json!(["read", "write"]);
        manifest["writeApi"] = json!("1");
        let result = WasmDriver::load(&serde_json::to_vec(&manifest).unwrap(), &[]);
        assert!(matches!(result, Err(LoadError::Contract(_))));
    }

    #[cfg(feature = "controlled-live-write-fixture")]
    #[test]
    fn controlled_r_test_loader_settings_are_exact() {
        let settings = |host: &str, port: &str| {
            Settings::from_pairs([
                ("host".to_owned(), host.to_owned()),
                ("port".to_owned(), port.to_owned()),
            ])
            .unwrap()
        };
        let target = ControlledWriteTarget::RTestPlc;
        assert!(target.accepts_settings(&settings("192.168.11.200", "3109")));
        assert!(!target.accepts_settings(&settings("127.0.0.1", "3109")));
        assert!(!target.accepts_settings(&settings("192.168.11.200", "3100")));
    }

    #[test]
    fn boundary_checks_variant_and_range() {
        let u16_kind = TagKind::numeric(DataType::U16);
        let u32_kind = TagKind::numeric(DataType::U32);
        let i16_kind = TagKind::numeric(DataType::I16);
        let i32_kind = TagKind::numeric(DataType::I32);
        assert_eq!(
            checked_value(&u16_kind, types::Value::Uint(0)).unwrap(),
            RawValue::Uint(0)
        );
        assert_eq!(
            checked_value(&u16_kind, types::Value::Uint(65535)).unwrap(),
            RawValue::Uint(65535)
        );
        assert_eq!(
            checked_value(&u32_kind, types::Value::Uint(u64::from(u32::MAX))).unwrap(),
            RawValue::Uint(u64::from(u32::MAX))
        );
        assert!(checked_value(&u16_kind, types::Value::Uint(65536)).is_err());
        assert!(checked_value(&u32_kind, types::Value::Uint(u64::from(u32::MAX) + 1)).is_err());
        assert!(checked_value(&u32_kind, types::Value::Int(1)).is_err());
        assert!(checked_value(&u16_kind, types::Value::Float(1.0)).is_err());
        for value in [i64::from(i16::MIN), -1, 0, i64::from(i16::MAX)] {
            assert_eq!(
                checked_value(&i16_kind, types::Value::Int(value)).unwrap(),
                RawValue::Int(value)
            );
        }
        for value in [i64::from(i32::MIN), -1, 0, i64::from(i32::MAX)] {
            assert_eq!(
                checked_value(&i32_kind, types::Value::Int(value)).unwrap(),
                RawValue::Int(value)
            );
        }
        for value in [i64::from(i16::MIN) - 1, i64::from(i16::MAX) + 1] {
            assert!(checked_value(&i16_kind, types::Value::Int(value)).is_err());
        }
        for value in [i64::from(i32::MIN) - 1, i64::from(i32::MAX) + 1] {
            assert!(checked_value(&i32_kind, types::Value::Int(value)).is_err());
        }
        assert!(checked_value(&i16_kind, types::Value::Uint(1)).is_err());
        assert!(checked_value(&i32_kind, types::Value::Uint(1)).is_err());
        assert!(checked_value(&u16_kind, types::Value::Int(-1)).is_err());
        assert!(checked_value(&u32_kind, types::Value::Int(-1)).is_err());
        let i64_kind = TagKind::numeric(DataType::I64);
        let u64_kind = TagKind::numeric(DataType::U64);
        let f32_kind = TagKind::numeric(DataType::F32);
        let f64_kind = TagKind::numeric(DataType::F64);
        for value in [i64::MIN, -(1_i64 << 53) - 1, i64::MAX] {
            assert_eq!(
                checked_value(&i64_kind, types::Value::Int(value)).unwrap(),
                RawValue::Int(value)
            );
        }
        for value in [0, (1_u64 << 53) + 1, u64::MAX] {
            assert_eq!(
                checked_value(&u64_kind, types::Value::Uint(value)).unwrap(),
                RawValue::Uint(value)
            );
        }
        for (kind, wrong) in [
            (&i64_kind, types::Value::Uint(1)),
            (&u64_kind, types::Value::Int(1)),
            (&f32_kind, types::Value::Int(1)),
            (&f64_kind, types::Value::Uint(1)),
        ] {
            assert!(checked_value(kind, wrong).is_err());
        }
        for value in [
            f64::from(f32::from_bits(1)),
            f64::from(f32::MIN_POSITIVE),
            1.5_f64,
            f64::from(f32::MAX),
            f64::INFINITY,
            f64::NEG_INFINITY,
        ] {
            assert_eq!(
                checked_value(&f32_kind, types::Value::Float(value)).unwrap(),
                RawValue::Float(value)
            );
        }
        assert!(matches!(
            checked_value(&f32_kind, types::Value::Float(f64::NAN)),
            Ok(RawValue::Float(value)) if value.is_nan()
        ));
        for value in [f64::from_bits(1), f64::MAX] {
            assert!(checked_value(&f32_kind, types::Value::Float(value)).is_err());
            assert_eq!(
                checked_value(&f64_kind, types::Value::Float(value)).unwrap(),
                RawValue::Float(value)
            );
        }
        assert_eq!(
            match checked_value(&f32_kind, types::Value::Float(-0.0)).unwrap() {
                RawValue::Float(value) => value.to_bits(),
                other => panic!("expected float, got {other:?}"),
            },
            (-0.0_f64).to_bits()
        );
        assert!(matches!(
            checked_value(&f64_kind, types::Value::Float(f64::NAN)),
            Ok(RawValue::Float(value)) if value.is_nan()
        ));
        assert_eq!(
            checked_value(&f64_kind, types::Value::Float(f64::NEG_INFINITY)).unwrap(),
            RawValue::Float(f64::NEG_INFINITY)
        );
        assert_eq!(
            match checked_value(&f64_kind, types::Value::Float(-0.0)).unwrap() {
                RawValue::Float(value) => value.to_bits(),
                other => panic!("expected float, got {other:?}"),
            },
            (-0.0_f64).to_bits()
        );
        assert!(checked_value(&TagKind::numeric(DataType::Bit), types::Value::Bool(true)).is_err());
    }

    #[test]
    fn supported_kinds_keep_old_guests_and_reject_unvalidated_kinds() {
        let u16_kind = KindName::Numeric(DataType::U16);
        let u32_kind = KindName::Numeric(DataType::U32);
        let i16_kind = KindName::Numeric(DataType::I16);
        let i32_kind = KindName::Numeric(DataType::I32);
        let i64_kind = KindName::Numeric(DataType::I64);
        let u64_kind = KindName::Numeric(DataType::U64);
        let f32_kind = KindName::Numeric(DataType::F32);
        let f64_kind = KindName::Numeric(DataType::F64);
        assert!(validate_supported_kinds(&[u16_kind]).is_ok());
        assert!(validate_supported_kinds(&[u16_kind, u32_kind]).is_ok());
        assert!(validate_supported_kinds(&[u16_kind, u32_kind, i16_kind, i32_kind]).is_ok());
        assert!(validate_supported_kinds(&[i32_kind, u16_kind, i16_kind, u32_kind]).is_ok());
        assert!(validate_supported_kinds(&[
            u16_kind, u32_kind, i16_kind, i32_kind, i64_kind, u64_kind, f32_kind, f64_kind,
        ])
        .is_ok());
        assert!(validate_supported_kinds(&[]).is_err());
        assert!(validate_supported_kinds(&[i16_kind, i32_kind]).is_err());
        assert!(validate_supported_kinds(&[u16_kind, u16_kind]).is_err());
        assert!(validate_supported_kinds(&[u16_kind, KindName::Numeric(DataType::Bit)]).is_err());
        assert!(validate_supported_kinds(&[u16_kind, KindName::String]).is_err());
    }

    fn parsed() -> address::Parsed {
        address::Parsed {
            canonical: "400001".into(),
            area: "holding_register".into(),
            writable_area: true,
            bit: None,
            word_count: 1,
            index: Some(0),
        }
    }

    #[test]
    fn parsed_address_metadata_is_generic_but_bounded() {
        let driver = DriverId::new("modbus-fc3-wasm").unwrap();
        let kind = TagKind::numeric(DataType::U16);
        let valid = checked_parsed_address(&driver, &kind, parsed()).unwrap();
        assert!(valid.writable_area); // Read-only capability does not change area metadata.
        assert_eq!(valid.index, Some(0));
        let mut wrong = parsed();
        wrong.word_count = 2;
        assert!(checked_parsed_address(&driver, &kind, wrong).is_err());
        let mut wrong = parsed();
        wrong.bit = Some(1);
        assert!(checked_parsed_address(&driver, &kind, wrong).is_err());
        let mut other = parsed();
        other.canonical = "X:1F".into();
        other.area = "X".into();
        other.writable_area = false;
        other.index = Some(31);
        let accepted = checked_parsed_address(&driver, &kind, other).unwrap();
        assert_eq!(accepted.canonical, "X:1F");
        assert_eq!(accepted.area, "X");
        assert_eq!(accepted.index, Some(31));
        assert!(!accepted.writable_area);
        let mut wrong = parsed();
        wrong.canonical.clear();
        assert!(checked_parsed_address(&driver, &kind, wrong).is_err());
        let mut wrong = parsed();
        wrong.area.clear();
        assert!(checked_parsed_address(&driver, &kind, wrong).is_err());
        let i16_kind = TagKind::numeric(DataType::I16);
        assert_eq!(
            checked_parsed_address(&driver, &i16_kind, parsed())
                .unwrap()
                .word_count,
            1
        );
        let mut wrong = parsed();
        wrong.word_count = 2;
        assert!(checked_parsed_address(&driver, &i16_kind, wrong).is_err());

        let u32_kind = TagKind::numeric(DataType::U32);
        let mut two_words = parsed();
        two_words.word_count = 2;
        let valid = checked_parsed_address(&driver, &u32_kind, two_words.clone()).unwrap();
        assert_eq!(valid.word_count, 2);
        assert!(checked_parsed_address(&driver, &u32_kind, parsed()).is_err());
        two_words.canonical = "X:FFFF".into();
        two_words.index = Some(u64::MAX - 1);
        assert!(checked_parsed_address(&driver, &u32_kind, two_words.clone()).is_ok());
        two_words.index = Some(u64::MAX);
        assert!(checked_parsed_address(&driver, &u32_kind, two_words).is_err());

        let i32_kind = TagKind::numeric(DataType::I32);
        let mut two_words = parsed();
        two_words.word_count = 2;
        assert_eq!(
            checked_parsed_address(&driver, &i32_kind, two_words.clone())
                .unwrap()
                .word_count,
            2
        );
        assert!(checked_parsed_address(&driver, &i32_kind, parsed()).is_err());
        two_words.index = Some(u64::MAX - 1);
        assert!(checked_parsed_address(&driver, &i32_kind, two_words.clone()).is_ok());
        two_words.index = Some(u64::MAX);
        assert!(checked_parsed_address(&driver, &i32_kind, two_words).is_err());
        assert!(
            checked_parsed_address(&driver, &TagKind::numeric(DataType::Bit), parsed()).is_err()
        );
    }

    #[test]
    fn wide_numeric_address_metadata_checks_span_and_overflow() {
        let driver = DriverId::new("modbus-fc3-wasm").unwrap();
        for (data_type, words) in [
            (DataType::F32, 2_u32),
            (DataType::I64, 4),
            (DataType::U64, 4),
            (DataType::F64, 4),
        ] {
            let kind = TagKind::numeric(data_type);
            let mut metadata = parsed();
            metadata.word_count = words;
            metadata.index = Some(u64::MAX - u64::from(words - 1));
            let accepted = checked_parsed_address(&driver, &kind, metadata.clone()).unwrap();
            assert_eq!(accepted.word_count, words as u16);
            assert!(accepted.writable_area); // Read support never authorizes writing.
            metadata.word_count = words - 1;
            assert!(checked_parsed_address(&driver, &kind, metadata.clone()).is_err());
            metadata.word_count = words;
            metadata.index = Some(u64::MAX - u64::from(words - 2));
            assert!(checked_parsed_address(&driver, &kind, metadata).is_err());
        }
    }

    #[test]
    fn schema_secret_fields_match_declared_keys() {
        let secret = SettingField::new("password", "Password", FieldKind::Secret);
        let host = SettingField::new("host", "Host", FieldKind::Host);
        assert!(
            validate_secret_fields(&[host.clone(), secret.clone()], &["password".into()]).is_ok()
        );
        assert!(validate_secret_fields(&[host.clone(), secret.clone()], &[]).is_err());
        assert!(validate_secret_fields(std::slice::from_ref(&host), &["password".into()]).is_err());
        assert!(
            validate_secret_fields(&[secret.clone(), secret.clone()], &["password".into()])
                .is_err()
        );
        let conflicting = SettingField::new("password", "Shown as text", FieldKind::text());
        assert!(
            validate_secret_fields(&[secret.clone(), conflicting], &["password".into()]).is_err()
        );
        assert!(
            validate_secret_fields(&[host, secret], &["password".into(), "password".into()])
                .is_err()
        );
    }
}
