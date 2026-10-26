//! Explicit, signed WASM registration at Hub startup. No directory scan or
//! installer is performed here: the selected files must already be placed in
//! the protected plugin directory by an administrator.

use std::fs::{self, File, Metadata};
use std::io::Read;
use std::path::{Path, PathBuf};

use banto_driver_api::{DriverId, DriverRegistry};
use banto_driver_wasm::Manifest;
use ring::digest::{Context, SHA256};
use ring::signature::{UnparsedPublicKey, ED25519};

const SELECT_ENV: &str = "BANTO_WASM_PLUGIN";
const WRITE_SELECT_ENV: &str = "BANTO_WASM_WRITE_PLUGIN";
const MANIFEST: &str = "manifest.json";
const COMPONENT: &str = "component.wasm";
const SIGNATURE: &str = "signature.ed25519";
const DOMAIN: &[u8] = b"banto-hub-wasm-directory-v1\0";
const MAX_MANIFEST: u64 = 64 * 1024;
const MAX_COMPONENT: u64 = 64 * 1024 * 1024;

// A test key must never be trusted by a release build. The owner's public
// verification key can be embedded here after its provenance is approved.
const TRUSTED_PUBLIC_KEYS: &[[u8; 32]] = &[];

// Service configuration builds the registry before its log file is opened.
#[cfg(windows)]
static STARTUP_MESSAGE: std::sync::Mutex<Option<(bool, String)>> = std::sync::Mutex::new(None);

#[cfg(windows)]
pub(super) fn take_startup_message() -> Option<(bool, String)> {
    STARTUP_MESSAGE.lock().ok()?.take()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Selection {
    id: String,
    version: String,
}

impl Selection {
    fn parse(value: &str) -> Result<Self, String> {
        let (id, version) = value
            .split_once('@')
            .ok_or_else(|| "expected BANTO_WASM_PLUGIN=id@version".to_owned())?;
        if id.len() > 128 || version.len() > 128 {
            return Err("plugin id/version is too long".into());
        }
        DriverId::new(id).map_err(|_| "invalid plugin id".to_owned())?;
        semver::Version::parse(version).map_err(|_| "invalid plugin version".to_owned())?;
        // The version becomes one path component. SemVer normally excludes
        // separators, but keep this restriction explicit at the filesystem edge.
        if version.is_empty()
            || !version
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+'))
            || version.ends_with('.')
        {
            return Err("invalid plugin version path".into());
        }
        Ok(Self {
            id: id.into(),
            version: version.into(),
        })
    }
}

pub(super) fn register_from_env(registry: &mut DriverRegistry) {
    #[cfg(windows)]
    if let Ok(mut slot) = STARTUP_MESSAGE.lock() {
        *slot = None;
    }
    let selection = match std::env::var(SELECT_ENV) {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => return,
        Err(_) => {
            let message = "WASM plugin rejected: selection is not UTF-8";
            #[cfg(windows)]
            if let Ok(mut slot) = STARTUP_MESSAGE.lock() {
                *slot = Some((false, message.into()));
            }
            banto_hub_core::hub_log::log_err_line(message);
            return;
        }
    };
    let write_selection: Result<Option<String>, String> = match std::env::var(WRITE_SELECT_ENV) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(_) => Err("WASM write selection is not UTF-8".into()),
    };
    let outcome = write_selection.and_then(|write_selection| {
        std::env::var_os("ProgramData")
            .filter(|path| !path.is_empty())
            .ok_or_else(|| "ProgramData is unavailable".to_owned())
            .and_then(|base| {
                register_selected_with_write_opt_in(
                    registry,
                    &PathBuf::from(base).join("BantoHub").join("plugins"),
                    &selection,
                    TRUSTED_PUBLIC_KEYS,
                    write_selection.as_deref(),
                )
            })
    });
    let (success, message) = match outcome {
        Ok(()) => (true, format!("WASM plugin registered: {selection}")),
        Err(reason) => (
            false,
            format!("WASM plugin rejected ({selection:?}): {reason}"),
        ),
    };
    #[cfg(windows)]
    if let Ok(mut slot) = STARTUP_MESSAGE.lock() {
        *slot = Some((success, message.clone()));
    }
    if success {
        banto_hub_core::hub_log::log_line(&message);
    } else {
        banto_hub_core::hub_log::log_err_line(&message);
    }
}

/// The root and keys are arguments so tests can use a temporary directory and
/// an ephemeral key. A bad plugin never changes the existing registry.
#[cfg(test)]
fn register_selected(
    registry: &mut DriverRegistry,
    root: &Path,
    selector: &str,
    trusted_keys: &[[u8; 32]],
) -> Result<(), String> {
    register_selected_with_write_opt_in(registry, root, selector, trusted_keys, None)
}

fn register_selected_with_write_opt_in(
    registry: &mut DriverRegistry,
    root: &Path,
    selector: &str,
    trusted_keys: &[[u8; 32]],
    write_selection: Option<&str>,
) -> Result<(), String> {
    let selection = Selection::parse(selector)?;
    if write_selection.is_some_and(|chosen| chosen != selector) {
        return Err("write opt-in must match the selected plugin id@version".into());
    }
    if trusted_keys.is_empty() {
        return Err("no trusted plugin verification key is configured".into());
    }
    if registry.contains(&selection.id) {
        return Err("driver id already registered".into());
    }

    let id_dir = root.join(&selection.id);
    let directory = id_dir.join(&selection.version);
    for path in [root, id_dir.as_path(), directory.as_path()] {
        let metadata = fs::symlink_metadata(path).map_err(|_| "plugin directory unavailable")?;
        if !metadata.is_dir() || is_reparse_point(&metadata) {
            return Err("plugin directory is not a plain directory".into());
        }
    }
    let mut names = Vec::with_capacity(3);
    for entry in fs::read_dir(&directory).map_err(|_| "cannot list plugin directory".to_owned())? {
        names.push(
            entry
                .map_err(|_| "cannot list plugin directory".to_owned())?
                .file_name(),
        );
        if names.len() > 3 {
            return Err("plugin directory has extra entries".into());
        }
    }
    names.sort();
    let mut expected = [MANIFEST, COMPONENT, SIGNATURE]
        .map(std::ffi::OsString::from)
        .to_vec();
    expected.sort();
    if names != expected {
        return Err("plugin directory must contain only manifest.json, component.wasm and signature.ed25519".into());
    }

    let manifest = read_bounded(&directory.join(MANIFEST), MAX_MANIFEST)?;
    let component = read_bounded(&directory.join(COMPONENT), MAX_COMPONENT)?;
    let signature = read_bounded(&directory.join(SIGNATURE), 64)?;
    if signature.len() != 64 {
        return Err("invalid signature length".into());
    }
    let digest = signed_digest(&manifest, &component);
    if !trusted_keys.iter().any(|key| {
        UnparsedPublicKey::new(&ED25519, key)
            .verify(&digest, &signature)
            .is_ok()
    }) {
        return Err("invalid plugin signature".into());
    }

    let parsed = Manifest::parse(&manifest).map_err(|error| error.to_string())?;
    if parsed.descriptor.id.as_str() != selection.id
        || parsed.descriptor.version != selection.version
    {
        return Err("signed manifest id/version differs from selection".into());
    }
    if parsed.requests_write {
        if write_selection != Some(selector) {
            return Err("write manifest requires an exact BANTO_WASM_WRITE_PLUGIN opt-in".into());
        }
        banto_drivers::register_wasm_operational_write(registry, &manifest, &component)
            .map_err(|error| format!("component registration failed: {error}"))?;
    } else {
        if write_selection.is_some() {
            return Err("write opt-in selected a read-only plugin".into());
        }
        banto_drivers::register_wasm(registry, &manifest, &component)
            .map_err(|error| format!("component registration failed: {error}"))?;
    }
    Ok(())
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let link = fs::symlink_metadata(path).map_err(|_| "plugin file unavailable")?;
    if !link.is_file() || is_reparse_point(&link) {
        return Err("plugin entry is not a plain file".into());
    }
    let file = File::open(path).map_err(|_| "plugin file unavailable".to_owned())?;
    let metadata = file
        .metadata()
        .map_err(|_| "plugin file unavailable".to_owned())?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err("plugin file type or size rejected".into());
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "plugin file unreadable".to_owned())?;
    if bytes.len() as u64 > limit {
        return Err("plugin file exceeds size limit".into());
    }
    Ok(bytes)
}

fn is_reparse_point(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

/// SHA-256 of the exact two allowed entries, in fixed filename order, with
/// domain separation and unambiguous name/content lengths. Ed25519 signs
/// these 32 bytes. The signature file itself is excluded.
fn signed_digest(manifest: &[u8], component: &[u8]) -> [u8; 32] {
    let mut context = Context::new(&SHA256);
    context.update(DOMAIN);
    for (name, bytes) in [(COMPONENT, component), (MANIFEST, manifest)] {
        context.update(&(name.len() as u16).to_be_bytes());
        context.update(name.as_bytes());
        context.update(&(bytes.len() as u64).to_be_bytes());
        context.update(bytes);
    }
    context
        .finish()
        .as_ref()
        .try_into()
        .expect("SHA-256 length")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    const TEST_MANIFEST: &[u8] = include_bytes!(
        "../../../../../../crates/banto-driver-wasm/tests/fixtures/device-address/manifest.json"
    );
    const SELECTOR: &str = "device-address-wasm@0.1.0";

    fn signed_directory(manifest: &[u8], component: &[u8]) -> (tempfile::TempDir, [u8; 32]) {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("device-address-wasm").join("0.1.0");
        fs::create_dir_all(&directory).unwrap();
        let rng = SystemRandom::new();
        let key_bytes = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let key = Ed25519KeyPair::from_pkcs8(key_bytes.as_ref()).unwrap();
        let public = key.public_key().as_ref().try_into().unwrap();
        fs::write(directory.join(MANIFEST), manifest).unwrap();
        fs::write(directory.join(COMPONENT), component).unwrap();
        fs::write(
            directory.join(SIGNATURE),
            key.sign(&signed_digest(manifest, component)).as_ref(),
        )
        .unwrap();
        (temporary, public)
    }

    #[test]
    fn selector_rejects_path_escape_and_unqualified_values() {
        for value in [
            "",
            "modbus-tcp",
            "../x@0.1.0",
            "foo@../0.1.0",
            "foo@1.0.0/other",
            "foo@1.0.0@bar",
        ] {
            assert!(Selection::parse(value).is_err(), "{value}");
        }
    }

    #[test]
    fn without_trust_or_valid_signature_native_registry_is_untouched() {
        let (temporary, public) = signed_directory(TEST_MANIFEST, b"not a component");
        let mut registry = banto_drivers::builtin().registry;
        let original = registry.len();
        let error = register_selected(&mut registry, temporary.path(), SELECTOR, &[]).unwrap_err();
        assert!(error.contains("no trusted"));
        assert_eq!(registry.len(), original);

        let file = temporary
            .path()
            .join("device-address-wasm/0.1.0/component.wasm");
        fs::write(file, b"tampered").unwrap();
        let error =
            register_selected(&mut registry, temporary.path(), SELECTOR, &[public]).unwrap_err();
        assert!(error.contains("signature"));
        assert_eq!(registry.len(), original);
        assert!(registry.contains("modbus-tcp"));
        assert!(registry.contains("slmp"));
    }

    #[test]
    fn signed_manifest_must_match_selection_and_hub_api() {
        let changed = String::from_utf8(TEST_MANIFEST.to_vec())
            .unwrap()
            .replace("0.1.0", "0.2.0");
        let (temporary, public) = signed_directory(changed.as_bytes(), b"not a component");
        let mut registry = banto_drivers::builtin().registry;
        let error =
            register_selected(&mut registry, temporary.path(), SELECTOR, &[public]).unwrap_err();
        assert!(error.contains("differs"));

        let changed = String::from_utf8(TEST_MANIFEST.to_vec())
            .unwrap()
            .replace("\"hubApi\": \"1\"", "\"hubApi\": \"2\"");
        let (temporary, public) = signed_directory(changed.as_bytes(), b"not a component");
        let error =
            register_selected(&mut registry, temporary.path(), SELECTOR, &[public]).unwrap_err();
        assert!(error.contains("hubApi"));
    }

    #[test]
    fn signed_write_manifest_requires_exact_separate_startup_selection() {
        let mut write_manifest: serde_json::Value = serde_json::from_slice(TEST_MANIFEST).unwrap();
        write_manifest["capabilities"] = serde_json::json!(["read", "write"]);
        write_manifest["writeApi"] = serde_json::json!("1");
        let bytes = serde_json::to_vec(&write_manifest).unwrap();
        let (temporary, public) = signed_directory(&bytes, b"not a component");
        let mut registry = banto_drivers::builtin().registry;
        let original = registry.len();
        let missing = register_selected_with_write_opt_in(
            &mut registry,
            temporary.path(),
            SELECTOR,
            &[public],
            None,
        )
        .unwrap_err();
        assert!(missing.contains("exact BANTO_WASM_WRITE_PLUGIN opt-in"));
        let mismatched = register_selected_with_write_opt_in(
            &mut registry,
            temporary.path(),
            SELECTOR,
            &[public],
            Some("another@0.1.0"),
        )
        .unwrap_err();
        assert!(mismatched.contains("must match"));
        let invalid_component = register_selected_with_write_opt_in(
            &mut registry,
            temporary.path(),
            SELECTOR,
            &[public],
            Some(SELECTOR),
        )
        .unwrap_err();
        assert!(invalid_component.contains("component registration failed"));
        assert_eq!(registry.len(), original);

        let (read_only, read_key) = signed_directory(TEST_MANIFEST, b"not a component");
        let not_write = register_selected_with_write_opt_in(
            &mut registry,
            read_only.path(),
            SELECTOR,
            &[read_key],
            Some(SELECTOR),
        )
        .unwrap_err();
        assert!(not_write.contains("read-only plugin"));
        assert_eq!(registry.len(), original);
    }

    #[test]
    fn signed_directory_refuses_unknown_entries_and_native_collision() {
        let (temporary, public) = signed_directory(TEST_MANIFEST, b"not a component");
        let directory = temporary.path().join("device-address-wasm/0.1.0");
        fs::write(directory.join("extra.txt"), b"unsigned").unwrap();
        let mut registry = banto_drivers::builtin().registry;
        let error =
            register_selected(&mut registry, temporary.path(), SELECTOR, &[public]).unwrap_err();
        assert!(error.contains("extra entries"));
        let error = register_selected(
            &mut registry,
            temporary.path(),
            "modbus-tcp@0.1.0",
            &[public],
        )
        .unwrap_err();
        assert!(error.contains("already registered"));
        assert!(registry.contains("modbus-tcp"));
    }

    // CI builds the real fixture before running this test. It exercises
    // registration and a fresh-registry restart without opening a socket.
    #[test]
    #[ignore = "requires BANTO_DEVICE_COMPONENT_WASM from the WASM CI fixture build"]
    fn real_component_registers_on_restart_and_rejects_tampering() {
        let path = std::env::var("BANTO_DEVICE_COMPONENT_WASM").unwrap();
        let component = fs::read(path).unwrap();
        let (temporary, public) = signed_directory(TEST_MANIFEST, &component);
        for _ in 0..2 {
            let mut registry = banto_drivers::builtin().registry;
            register_selected(&mut registry, temporary.path(), SELECTOR, &[public]).unwrap();
            assert!(registry.contains("device-address-wasm"));
            assert!(registry.contains("modbus-tcp"));
            assert!(registry.contains("slmp"));
        }
        let file = temporary
            .path()
            .join("device-address-wasm/0.1.0/component.wasm");
        fs::write(file, b"tampered").unwrap();
        let mut registry = banto_drivers::builtin().registry;
        assert!(register_selected(&mut registry, temporary.path(), SELECTOR, &[public]).is_err());
        assert!(!registry.contains("device-address-wasm"));
    }
}
