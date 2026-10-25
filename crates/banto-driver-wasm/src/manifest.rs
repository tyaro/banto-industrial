//! Validation of TCP component manifests. Native descriptor defaults do not
//! apply; a write declaration is only a request until its export is bound.

use banto_driver_api::{Capabilities, DriverDescriptor, DriverId};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub descriptor: DriverDescriptor,
    /// A request to negotiate the optional extension, never permission by itself.
    pub requests_write: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestError(pub String);

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ManifestError {}

impl Manifest {
    pub fn parse(bytes: &[u8]) -> Result<Self, ManifestError> {
        let value: Value = serde_json::from_slice(bytes)
            .map_err(|e| ManifestError(format!("invalid manifest JSON: {e}")))?;
        let object = value
            .as_object()
            .ok_or_else(|| ManifestError("manifest must be an object".into()))?;
        let version = object
            .get("settingsVersion")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n > 0)
            .ok_or_else(|| {
                ManifestError("settingsVersion must be a positive u32 integer".into())
            })?;
        let text = |key| -> Result<&str, ManifestError> {
            object
                .get(key)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| ManifestError(format!("{key} must be a nonempty string")))
        };
        if object.get("manifestVersion").and_then(Value::as_u64) != Some(1) {
            return Err(ManifestError("manifestVersion must be 1".into()));
        }
        if text("hubApi")? != "1" {
            return Err(ManifestError("unsupported hubApi".into()));
        }
        if object.get("transports").and_then(Value::as_array)
            != Some(&vec![Value::String("tcp".into())])
        {
            return Err(ManifestError(
                "initial WASM driver requires only tcp transport".into(),
            ));
        }
        let requests_write = match object.get("capabilities").and_then(Value::as_array) {
            Some(values) if values == &vec![Value::String("read".into())] => {
                if object.contains_key("writeApi") {
                    return Err(ManifestError(
                        "read-only manifest must omit writeApi".into(),
                    ));
                }
                false
            }
            Some(values)
                if values == &vec![Value::String("read".into()), Value::String("write".into())] =>
            {
                if object.get("writeApi").and_then(Value::as_str) != Some("1") {
                    return Err(ManifestError("write capability requires writeApi 1".into()));
                }
                true
            }
            _ => return Err(ManifestError("unsupported WASM capabilities".into())),
        };
        let id = DriverId::new(text("id")?).map_err(|e| ManifestError(e.to_string()))?;
        let plugin_version = text("version")?;
        semver::Version::parse(plugin_version)
            .map_err(|e| ManifestError(format!("version must be SemVer: {e}")))?;
        let mut descriptor = DriverDescriptor::new(
            id,
            plugin_version,
            text("displayName")?,
            Capabilities::READ_ONLY,
        )
        .with_settings_version(version);
        if let Some(vendor) = object.get("vendor") {
            descriptor.vendor = Some(
                vendor
                    .as_str()
                    .ok_or_else(|| ManifestError("vendor must be a string".into()))?
                    .into(),
            );
        }
        Ok(Self {
            descriptor,
            requests_write,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn settings_version_is_required_positive_integer() {
        let sample = include_bytes!("../tests/manifest.json");
        assert_eq!(
            Manifest::parse(sample).unwrap().descriptor.settings_version,
            1
        );
        let base: Value = serde_json::from_slice(sample).unwrap();
        for invalid in [
            Value::Null,
            json!(0),
            json!(-1),
            json!(1.0),
            json!("1"),
            json!(true),
            json!(4294967296u64),
        ] {
            let mut changed = base.clone();
            changed["settingsVersion"] = invalid;
            assert!(Manifest::parse(&serde_json::to_vec(&changed).unwrap()).is_err());
        }
        let mut missing = base;
        missing.as_object_mut().unwrap().remove("settingsVersion");
        assert!(Manifest::parse(&serde_json::to_vec(&missing).unwrap()).is_err());
    }

    #[test]
    fn version_is_semver() {
        let sample = include_bytes!("../tests/manifest.json");
        let mut manifest: Value = serde_json::from_slice(sample).unwrap();
        for invalid in ["1", "v1.2.3", "1.2.3.4", ""] {
            manifest["version"] = json!(invalid);
            assert!(Manifest::parse(&serde_json::to_vec(&manifest).unwrap()).is_err());
        }
    }

    #[test]
    fn write_request_never_sets_descriptor_capability() {
        let sample = include_bytes!("../tests/manifest.json");
        let mut manifest: Value = serde_json::from_slice(sample).unwrap();
        assert!(!Manifest::parse(sample).unwrap().requests_write);
        manifest["capabilities"] = json!(["read", "write"]);
        assert!(Manifest::parse(&serde_json::to_vec(&manifest).unwrap()).is_err());
        manifest["writeApi"] = json!("1");
        let parsed = Manifest::parse(&serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(parsed.requests_write);
        assert!(!parsed.descriptor.capabilities.write);
        manifest["capabilities"] = json!(["read"]);
        assert!(Manifest::parse(&serde_json::to_vec(&manifest).unwrap()).is_err());
    }
}
