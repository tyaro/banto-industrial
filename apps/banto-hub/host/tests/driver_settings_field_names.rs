//! R2b-7b: every setting exposed by the built-in device drivers uses the
//! `settings.<key>` field path at both validation stages.

mod common;

use banto_driver_api::{DriverKind, FieldKind};
use banto_tags::settings_json::{settings_from_schema, validate_settings_with_registry};
use serde_json::{Map, Value};

#[test]
fn every_builtin_device_setting_uses_its_settings_path() {
    let drivers = common::hub_drivers();
    let registry = &drivers.registry;
    let mut checked = 0;

    for id in registry.ids() {
        let entry = registry.get(id.as_str()).expect("registered driver");
        if !matches!(entry.kind(), DriverKind::Frame(_) | DriverKind::Session(_)) {
            continue;
        }

        let schema = entry.base().settings_schema();
        assert!(!schema.fields.is_empty(), "{id}: no settings schema");
        for field in &schema.fields {
            let expected = format!("settings.{}", field.key);

            // An array violates every current field kind, including Secret.
            // This checks the schema stage independently of driver validation.
            let mut input = Map::new();
            input.insert(field.key.clone(), Value::Array(vec![]));
            let schema_errors = settings_from_schema(&schema, &input).unwrap_err();
            assert!(
                schema_errors.iter().any(|error| error.key == expected),
                "{id} / {}: schema errors: {schema_errors:?}",
                field.key
            );

            // The driver stage has no Secret input: plaintext is sealed and
            // removed before it is called. All other schema fields must map
            // their driver error through rest_field_error to this path.
            let invalid = match field.kind {
                FieldKind::Host => Value::String(String::new()),
                FieldKind::Secret => {
                    checked += 1;
                    continue;
                }
                _ => Value::String("invalid".into()),
            };
            let mut settings = Map::new();
            settings.insert(field.key.clone(), invalid);
            let driver_errors = validate_settings_with_registry(registry, id.as_str(), &settings);
            assert!(
                driver_errors.iter().any(|error| error.field == expected),
                "{id} / {}: driver errors: {driver_errors:?}",
                field.key
            );
            checked += 1;
        }
    }
    assert!(checked > 0, "no built-in device settings checked");
}
