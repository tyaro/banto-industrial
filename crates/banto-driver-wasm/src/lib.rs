//! Read-only Wasmtime Component Model frame driver. The host owns transport,
//! timeouts and frame limits; this crate only runs the canonical WIT exports.

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
    Settings, SettingsSchema, Step, TagKind, TagSpec,
};
use banto_driver_api as api;
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
    probe: Option<(String, TagKind)>,
    supported: Vec<KindName>,
    secrets: Vec<String>,
    schema: SettingsSchema,
}

impl WasmDriver {
    /// Validate metadata, compile a read-only component, and evaluate its
    /// stateless exports before making it visible to a registry.
    pub fn load(manifest: &[u8], component: &[u8]) -> Result<Arc<Self>, LoadError> {
        let manifest = Manifest::parse(manifest).map_err(LoadError::Manifest)?;
        let runtime = Arc::new(Runtime::new(component).map_err(LoadError::Runtime)?);
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
        if supported != [KindName::Numeric(DataType::U16)] {
            return Err(LoadError::Contract(
                "initial WASM component must support only U16".into(),
            ));
        }
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
        Ok(Arc::new(Self {
            descriptor: manifest.descriptor,
            runtime,
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

/// Check guest-reported address metadata before passing it to the Hub. The
/// initial FC3 contract is one U16 holding register at a numeric offset.
fn checked_parsed_address(
    driver: &DriverId,
    kind: &TagKind,
    parsed: address::Parsed,
) -> Result<DriverAddress, AddressError> {
    if *kind != TagKind::numeric(DataType::U16)
        || parsed.area != "holding_register"
        || parsed.word_count != 1
        || parsed.bit.is_some()
    {
        return Err(AddressError::new(
            "WASM FC3 address metadata violates the U16 contract",
        ));
    }
    let digits = parsed
        .canonical
        .strip_prefix('4')
        .ok_or_else(|| AddressError::new("WASM FC3 canonical address is invalid"))?;
    if !matches!(digits.len(), 4 | 5) || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(AddressError::new("WASM FC3 canonical address is invalid"));
    }
    let number = digits
        .parse::<u32>()
        .map_err(|_| AddressError::new("WASM FC3 canonical address is invalid"))?;
    let offset = number
        .checked_sub(1)
        .and_then(|n| u16::try_from(n).ok())
        .ok_or_else(|| AddressError::new("WASM FC3 canonical address is out of range"))?;
    if parsed.index != Some(u64::from(offset)) {
        return Err(AddressError::new(
            "WASM FC3 canonical address and index disagree",
        ));
    }
    Ok(DriverAddress::new(
        driver.clone(),
        parsed.canonical,
        parsed.area,
        parsed.writable_area,
        1,
    )
    .with_index(u64::from(offset)))
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

fn checked_fc3_u16(kind: &TagKind, value: types::Value) -> Result<RawValue, FatalError> {
    if *kind != TagKind::numeric(DataType::U16) {
        return Err(FatalError::Protocol(
            "WASM returned a value for a non-U16 tag".into(),
        ));
    }
    match value {
        types::Value::Uint(n) if n <= u16::MAX as u64 => Ok(RawValue::Uint(n)),
        _ => Err(FatalError::Protocol(
            "WASM FC3 U16 result has wrong type or range".into(),
        )),
    }
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
        }))
    }
}

struct WasmConnection {
    instance: Mutex<Instance>,
    resource: wasmtime::component::ResourceAny,
    runtime: Arc<Runtime>,
    failed: AtomicBool,
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
                    checked_fc3_u16(&spec.kind, value).map(Ok)
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

    #[test]
    fn fc3_boundary_checks_variant_and_range() {
        let kind = TagKind::numeric(DataType::U16);
        assert_eq!(
            checked_fc3_u16(&kind, types::Value::Uint(0)).unwrap(),
            RawValue::Uint(0)
        );
        assert_eq!(
            checked_fc3_u16(&kind, types::Value::Uint(65535)).unwrap(),
            RawValue::Uint(65535)
        );
        assert!(checked_fc3_u16(&kind, types::Value::Uint(65536)).is_err());
        assert!(checked_fc3_u16(&kind, types::Value::Int(1)).is_err());
        assert!(checked_fc3_u16(&kind, types::Value::Float(1.0)).is_err());
        assert!(checked_fc3_u16(&TagKind::numeric(DataType::I16), types::Value::Uint(1)).is_err());
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
    fn fc3_parsed_address_metadata_is_checked() {
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
        let mut wrong = parsed();
        wrong.area = "coil".into();
        assert!(checked_parsed_address(&driver, &kind, wrong).is_err());
        let mut wrong = parsed();
        wrong.index = Some(1);
        assert!(checked_parsed_address(&driver, &kind, wrong).is_err());
        let mut wrong = parsed();
        wrong.canonical = "400002".into();
        assert!(checked_parsed_address(&driver, &kind, wrong).is_err());
        assert!(
            checked_parsed_address(&driver, &TagKind::numeric(DataType::I16), parsed()).is_err()
        );
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
