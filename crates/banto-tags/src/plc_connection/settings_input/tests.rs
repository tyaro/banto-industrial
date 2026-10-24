//! `settings` の形の書き込みの入口の表のテスト（R2b-6b、設計 §4.2・§5・§10）。

use std::sync::Arc;

use banto_driver_api::{
    AddressError, BoxFuture, Capabilities, ConnectContext, DriverAddress, DriverBase,
    DriverDescriptor, DriverEntry, DriverId, DriverRegistry, FatalError, FieldError as DFE,
    FieldKind, Session, SessionDriver, SettingField, Settings, SettingsSchema, TagKind,
};
use serde_json::json;

use super::*;
use crate::connection_secret::SealedSecretRow;

const REF_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const REF_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

/// A session driver with a schema: `host` (required), `port` (1..=65535, default 502),
/// `unit_id` (0..=255, default 1) and, when `secret` names one, a secret field
/// (`required` as given). `validate_config` refuses `unit_id = 99` so a driver error is
/// observable.
struct SchemaDriver {
    descriptor: DriverDescriptor,
    secret: Option<(&'static str, bool)>,
}

impl DriverBase for SchemaDriver {
    fn descriptor(&self) -> &DriverDescriptor {
        &self.descriptor
    }
    fn validate_config(&self, settings: &Settings) -> Vec<DFE> {
        if settings.get("unit_id") == Some("99") {
            vec![DFE::new("unit_id", "driver says no")]
        } else {
            Vec::new()
        }
    }
    fn settings_schema(&self) -> SettingsSchema {
        let mut fields = vec![
            SettingField::new("host", "ホスト", FieldKind::host()).required(),
            SettingField::new("port", "ポート", FieldKind::integer(1, 65_535))
                .required()
                .default(502),
            SettingField::new("unit_id", "ユニット", FieldKind::integer(0, 255)).default(1),
        ];
        if let Some((key, required)) = self.secret {
            let field = SettingField::new(key, "秘密", FieldKind::Secret);
            fields.push(if required { field.required() } else { field });
        }
        SettingsSchema::new(fields)
    }
    fn secret_keys(&self) -> Vec<String> {
        self.secret.iter().map(|(key, _)| key.to_string()).collect()
    }
    fn parse_address(&self, _: &str, _: &TagKind) -> Result<DriverAddress, AddressError> {
        Err(AddressError::new("none"))
    }
    fn offset_address(&self, c: &str, _: &TagKind, _: i64) -> Result<String, AddressError> {
        Ok(c.to_owned())
    }
    fn address_help(&self) -> String {
        String::new()
    }
}

impl SessionDriver for SchemaDriver {
    fn connect(
        &self,
        _: &Settings,
        _: ConnectContext,
    ) -> BoxFuture<'static, Result<Box<dyn Session>, FatalError>> {
        Box::pin(async { Err(FatalError::Timeout) })
    }
}

/// A host-provided driver (`postgres` with the `password` secret key, `push`).
struct HostDriver(DriverDescriptor, Vec<String>);

impl DriverBase for HostDriver {
    fn descriptor(&self) -> &DriverDescriptor {
        &self.0
    }
    fn validate_config(&self, _: &Settings) -> Vec<DFE> {
        Vec::new()
    }
    fn secret_keys(&self) -> Vec<String> {
        self.1.clone()
    }
    fn parse_address(&self, _: &str, _: &TagKind) -> Result<DriverAddress, AddressError> {
        Err(AddressError::new("none"))
    }
    fn offset_address(&self, c: &str, _: &TagKind, _: i64) -> Result<String, AddressError> {
        Ok(c.to_owned())
    }
    fn address_help(&self) -> String {
        String::new()
    }
}

fn descriptor(id: &str) -> DriverDescriptor {
    DriverDescriptor::new(
        DriverId::new(id).unwrap(),
        "0.0.0",
        id,
        Capabilities::READ_ONLY,
    )
}

async fn service() -> PlcConnectionService {
    let mut registry = DriverRegistry::new();
    for (id, secret) in [
        ("plain", None),
        ("needs-token", Some(("token", true))),
        ("maybe-token", Some(("token", false))),
    ] {
        registry
            .register(DriverEntry::session(Arc::new(SchemaDriver {
                descriptor: descriptor(id),
                secret,
            })))
            .unwrap();
    }
    for (id, keys) in [("postgres", vec!["password".to_string()]), ("push", vec![])] {
        registry
            .register(DriverEntry::host_provided(Arc::new(HostDriver(
                descriptor(id),
                keys,
            ))))
            .unwrap();
    }
    let pool = banto_storage::connect_sqlite_memory()
        .await
        .expect("connect_sqlite_memory");
    crate::migrate(&pool).await.expect("migrate");
    PlcConnectionService::with_registry(pool, Arc::new(registry))
}

fn input(name: &str, protocol: &str, settings: Value) -> ConnectionSettingsInput {
    ConnectionSettingsInput {
        name: name.to_string(),
        protocol: protocol.to_string(),
        enabled: true,
        simulation: false,
        settings: settings.as_object().cloned().unwrap_or_default(),
        secrets: SecretChanges::new(),
        keep_stored: KeepStored::default(),
    }
}

fn set(r#ref: &str) -> SecretChange {
    SecretChange::Set(SealedSecretRow {
        r#ref: r#ref.to_string(),
        scheme: crate::SCHEME_PLAIN_V1.to_string(),
        blob: zeroize::Zeroizing::new(b"plain".to_vec()),
    })
}

fn errors(err: BantoError) -> Vec<(String, String)> {
    match err {
        BantoError::Validation { field_errors } => field_errors
            .into_iter()
            .map(|e| (e.field, e.message))
            .collect(),
        other => panic!("expected Validation, got {other:?}"),
    }
}

fn fields(err: BantoError) -> Vec<String> {
    errors(err).into_iter().map(|(field, _)| field).collect()
}

/// A driver's settings go through the schema: defaults filled, stored as typed JSON.
#[tokio::test]
async fn a_driver_connection_is_built_from_its_schema() {
    let svc = service().await;
    let created = svc
        .create_settings(input("A", "plain", json!({"host": "10.0.0.1"})))
        .await
        .unwrap();
    assert_eq!(
        Value::Object(created.settings.clone()),
        json!({"host": "10.0.0.1", "port": 502, "unit_id": 1})
    );
    // The update replaces the object (an omitted key goes back to its default).
    let updated = svc
        .update_settings(
            created.id,
            input(
                "A",
                "plain",
                json!({"host": "h", "port": 5020, "unit_id": 7}),
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        Value::Object(updated.settings),
        json!({"host": "h", "port": 5020, "unit_id": 7})
    );
    let reset = svc
        .update_settings(created.id, input("A", "plain", json!({"host": "h"})))
        .await
        .unwrap();
    assert_eq!(
        Value::Object(reset.settings),
        json!({"host": "h", "port": 502, "unit_id": 1})
    );
}

/// Q9 and design §4.2: unknown keys, wrong JSON types (no string-to-number reading),
/// out-of-range values and a missing required field are refused by their JSON key,
/// together with the name error; nothing is written.
#[tokio::test]
async fn unknown_keys_and_wrong_types_are_refused_by_json_key() {
    let svc = service().await;
    let err = svc
        .create_settings(input(
            " ",
            "plain",
            json!({"port": "502", "unit_id": 300, "extra": 1}),
        ))
        .await
        .unwrap_err();
    assert_eq!(fields(err), ["name", "host", "port", "unit_id", "extra"]);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM plc_connections")
        .fetch_one(&svc.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

/// The driver's own `validate_config` runs after the schema, named by the JSON key.
#[tokio::test]
async fn the_drivers_errors_come_back_by_json_key() {
    let svc = service().await;
    let err = svc
        .create_settings(input("A", "plain", json!({"host": "h", "unit_id": 99})))
        .await
        .unwrap_err();
    assert_eq!(fields(err), ["unit_id"]);
}

/// Owner decision 2026-10-24: on create a required secret field must be set; omission,
/// Keep and Clear (`null` / `""` at the Hub) are all "必須です".
#[tokio::test]
async fn a_required_secret_must_be_set_on_create() {
    let svc = service().await;
    for change in [None, Some(SecretChange::Clear), Some(SecretChange::Keep)] {
        let mut missing = input("A", "needs-token", json!({"host": "h"}));
        if let Some(change) = change {
            missing.secrets.insert("token".into(), change);
        }
        let err = svc.create_settings(missing).await.unwrap_err();
        assert_eq!(errors(err), [("token".to_string(), "必須です".to_string())]);
    }
    let mut ok = input("A", "needs-token", json!({"host": "h"}));
    ok.secrets.insert("token".into(), set(REF_A));
    let created = svc.create_settings(ok).await.unwrap();
    assert_eq!(created.secret_ref("token"), Some(REF_A));
}

/// On update a required secret is kept when omitted, refused when cleared, and refused
/// when kept but nothing is stored (a protocol change onto the driver).
#[tokio::test]
async fn a_required_secret_on_update_keeps_or_is_refused() {
    let svc = service().await;
    let mut ok = input("A", "needs-token", json!({"host": "h"}));
    ok.secrets.insert("token".into(), set(REF_A));
    let created = svc.create_settings(ok).await.unwrap();

    let kept = svc
        .update_settings(created.id, input("A", "needs-token", json!({"host": "h2"})))
        .await
        .unwrap();
    assert_eq!(kept.secret_ref("token"), Some(REF_A));

    let mut cleared = input("A", "needs-token", json!({"host": "h2"}));
    cleared.secrets.insert("token".into(), SecretChange::Clear);
    let err = svc.update_settings(created.id, cleared).await.unwrap_err();
    assert_eq!(fields(err), ["token"]);
    assert_eq!(
        svc.get(created.id).await.unwrap().secret_ref("token"),
        Some(REF_A)
    );

    let plain = svc
        .create_settings(input("B", "plain", json!({"host": "h"})))
        .await
        .unwrap();
    let err = svc
        .update_settings(plain.id, input("B", "needs-token", json!({"host": "h"})))
        .await
        .unwrap_err();
    assert_eq!(fields(err), ["token"]);

    let mut replaced = input("A", "needs-token", json!({"host": "h"}));
    replaced.secrets.insert("token".into(), set(REF_B));
    let replaced = svc.update_settings(created.id, replaced).await.unwrap();
    assert_eq!(replaced.secret_ref("token"), Some(REF_B));
}

/// An optional secret: Clear removes the reference, omission keeps it.
#[tokio::test]
async fn an_optional_secret_follows_the_tri_state() {
    let svc = service().await;
    let mut first = input("A", "maybe-token", json!({"host": "h"}));
    first.secrets.insert("token".into(), set(REF_A));
    let created = svc.create_settings(first).await.unwrap();
    let kept = svc
        .update_settings(created.id, input("A", "maybe-token", json!({"host": "h"})))
        .await
        .unwrap();
    assert_eq!(kept.secret_ref("token"), Some(REF_A));
    let mut cleared = input("A", "maybe-token", json!({"host": "h"}));
    cleared.secrets.insert("token".into(), SecretChange::Clear);
    let cleared = svc.update_settings(created.id, cleared).await.unwrap();
    assert_eq!(cleared.secret_ref("token"), None);
    assert!(!cleared.settings.contains_key("token"));
}

/// A secret key left in `settings` (a plaintext the entrance did not seal) and a `Set`
/// on a key the driver does not hold as a secret are refused.
#[tokio::test]
async fn a_plaintext_secret_or_a_foreign_secret_key_is_refused() {
    let svc = service().await;
    let err = svc
        .create_settings(input(
            "A",
            "maybe-token",
            json!({"host": "h", "token": "plain"}),
        ))
        .await
        .unwrap_err();
    assert_eq!(fields(err), ["token"]);
    let mut foreign = input("A", "plain", json!({"host": "h"}));
    foreign.secrets.insert("token".into(), set(REF_A));
    let err = svc.create_settings(foreign).await.unwrap_err();
    assert_eq!(fields(err), ["token"]);
}

/// postgres goes through today's rules: the four keys with their JSON types and the
/// password as a secret; an unknown key, a wrong type or a plaintext password is refused.
#[tokio::test]
async fn postgres_takes_its_four_keys_and_the_password_secret() {
    let svc = service().await;
    let mut pg = input(
        "PG",
        "postgres",
        json!({"host": " db ", "port": 5432, "database": "d", "username": "u"}),
    );
    pg.secrets.insert("password".into(), set(REF_A));
    let created = svc.create_settings(pg).await.unwrap();
    assert_eq!(created.host, "db");
    assert_eq!(created.database.as_deref(), Some("d"));
    assert_eq!(created.secret_ref("password"), Some(REF_A));

    let err = svc
        .create_settings(input(
            "PG2",
            "postgres",
            json!({"host": "db", "port": "5432", "database": "d", "username": "u",
                   "sslmode": "x", "password": "plain"}),
        ))
        .await
        .unwrap_err();
    let mut got = fields(err);
    got.sort();
    assert_eq!(got, ["password", "port", "sslmode"]);

    // Missing keys are today's required errors.
    let err = svc
        .create_settings(input("PG3", "postgres", json!({})))
        .await
        .unwrap_err();
    assert_eq!(fields(err), ["host", "port", "database", "username"]);
}

/// push keeps its own validation; the settings object is replaced as given.
#[tokio::test]
async fn push_settings_pass_through_its_own_rules() {
    let svc = service().await;
    let created = svc
        .create_settings(input("P", "push", json!({"stale_timeout_ms": 5000})))
        .await
        .unwrap();
    assert_eq!(created.settings.get("stale_timeout_ms"), Some(&json!(5000)));
    let err = svc
        .update_settings(
            created.id,
            input("P", "push", json!({"stale_timeout_ms": "x"})),
        )
        .await
        .unwrap_err();
    assert!(fields(err).iter().all(|f| f.starts_with("settings")));
    let replaced = svc
        .update_settings(created.id, input("P", "push", json!({})))
        .await
        .unwrap();
    assert!(replaced.settings.is_empty());
}

/// An unknown protocol is one `protocol` error (no host / port noise).
#[tokio::test]
async fn an_unknown_protocol_is_one_protocol_error() {
    let svc = service().await;
    let err = svc
        .create_settings(input("X", "nope", json!({"host": "h"})))
        .await
        .unwrap_err();
    assert_eq!(fields(err), ["protocol"]);
}

fn keeping(
    mut input: ConnectionSettingsInput,
    keys: &[&str],
    push: bool,
) -> ConnectionSettingsInput {
    input.keep_stored = KeepStored {
        keys: keys.iter().map(|key| key.to_string()).collect(),
        push_settings: push,
    };
    input
}

/// #813 review P2: a key marked [`KeepStored`] (the MCP flat adapter's omitted `cpuType`)
/// takes the stored value read **inside the write transaction**. An update committed after
/// the caller built its input (where the old adapter read the row) is not rolled back.
#[tokio::test]
async fn a_kept_key_comes_from_the_row_read_inside_the_write() {
    let svc = service().await;
    let created = svc
        .create_settings(input("A", "plain", json!({"host": "h", "unit_id": 7})))
        .await
        .unwrap();
    // The MCP update is built while the row says unit_id = 7 ...
    let late = keeping(
        input("A", "plain", json!({"host": "h2"})),
        &["unit_id"],
        false,
    );
    // ... and another update commits unit_id = 9 before it is written.
    svc.update_settings(
        created.id,
        input("A", "plain", json!({"host": "h", "unit_id": 9})),
    )
    .await
    .unwrap();
    let updated = svc.update_settings(created.id, late).await.unwrap();
    assert_eq!(
        Value::Object(updated.settings),
        json!({"host": "h2", "port": 502, "unit_id": 9})
    );
}

/// A kept key of a row of another protocol (or with no stored value) stays the schema
/// default; the mark is refused on create, for a key that is also given, for a secret or
/// unknown key, and for `push_settings` on a driver connection.
#[tokio::test]
async fn a_kept_key_falls_back_to_the_default_and_is_refused_where_it_cannot_apply() {
    let svc = service().await;
    let other = svc
        .create_settings(input(
            "B",
            "maybe-token",
            json!({"host": "h", "unit_id": 5}),
        ))
        .await
        .unwrap();
    let switched = svc
        .update_settings(
            other.id,
            keeping(
                input("B", "plain", json!({"host": "h"})),
                &["unit_id"],
                false,
            ),
        )
        .await
        .unwrap();
    assert_eq!(switched.settings.get("unit_id"), Some(&json!(1)));

    let err = svc
        .create_settings(keeping(
            input("C", "plain", json!({"host": "h"})),
            &["unit_id"],
            false,
        ))
        .await
        .unwrap_err();
    assert_eq!(fields(err), ["settings"]);
    for (keys, push, settings, want) in [
        (
            &["unit_id"][..],
            false,
            json!({"host": "h", "unit_id": 3}),
            "unit_id",
        ),
        (&["token"][..], false, json!({"host": "h"}), "token"),
        (&["zzz"][..], false, json!({"host": "h"}), "zzz"),
        (&[][..], true, json!({"host": "h"}), "settings"),
    ] {
        let err = svc
            .update_settings(
                other.id,
                keeping(input("B", "maybe-token", settings), keys, push),
            )
            .await
            .unwrap_err();
        assert_eq!(fields(err), [want], "{keys:?} {push}");
    }
    assert_eq!(
        svc.get(other.id).await.unwrap().settings.get("unit_id"),
        Some(&json!(1)),
        "nothing written by the refused updates"
    );
}

/// `push_settings` keeps the stored push object read inside the write (an update
/// committed in between is not rolled back); a row that is not push gets the default
/// `{}`; the mark is refused with non-empty settings or on another host connection.
#[tokio::test]
async fn kept_push_settings_come_from_the_row_read_inside_the_write() {
    let svc = service().await;
    let created = svc
        .create_settings(input("P", "push", json!({"stale_timeout_ms": 5000})))
        .await
        .unwrap();
    let late = keeping(input("P2", "push", json!({})), &[], true);
    svc.update_settings(
        created.id,
        input("P", "push", json!({"stale_timeout_ms": 7000})),
    )
    .await
    .unwrap();
    let updated = svc.update_settings(created.id, late).await.unwrap();
    assert_eq!(updated.name, "P2");
    assert_eq!(
        Value::Object(updated.settings),
        json!({"stale_timeout_ms": 7000})
    );

    let err = svc
        .update_settings(
            created.id,
            keeping(
                input("P", "push", json!({"stale_timeout_ms": 1})),
                &[],
                true,
            ),
        )
        .await
        .unwrap_err();
    assert_eq!(fields(err), ["settings"]);
    let err = svc
        .update_settings(
            created.id,
            keeping(input("P", "push", json!({})), &["stale_timeout_ms"], false),
        )
        .await
        .unwrap_err();
    assert_eq!(fields(err), ["stale_timeout_ms"]);

    let plain = svc
        .create_settings(input("Q", "plain", json!({"host": "h"})))
        .await
        .unwrap();
    let pushed = svc
        .update_settings(plain.id, keeping(input("Q", "push", json!({})), &[], true))
        .await
        .unwrap();
    assert!(pushed.settings.is_empty(), "{:?}", pushed.settings);
}
