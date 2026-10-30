//! #917: explicitly ignored Hub REST checks of the WASM guest stages against
//! the R and Q test PLCs (TCP 3108) and the Modbus targets (local ModbusSlave `127.0.0.1:502` or R `192.168.11.200:502`).
//! The router is in-process (no listener); the database and the API key are
//! temporary: the key carries only the read/write scopes of the case's tags,
//! expires after 10 minutes and is revoked at the end; the database is
//! deleted with the `TestApp`. Reads and writes both go through REST
//! (`read-now`, single and batch writes) and the broker.
//!
//! Every case is `#[ignore]` and refuses to dial unless
//! `BANTO_WASM_LIVE_917=1`. The resident BantoHub service's ports (R 5200,
//! Q 3100) are refused before the connection is saved and again before the
//! broker starts. When a read back disagrees, the case stops without retry or
//! automatic restoration. API key plaintext is never printed.
//!
//! Run one case at a time:
//! `cargo test -p banto-hub-host --test t12_connection_test wasm_live_917_test::hub_r_write -- --ignored --nocapture --test-threads=1`

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use banto_driver_api::{Settings, Transport as TransportKind};
use banto_driver_engine::transport::TransportSpec;
use banto_tags::{ConnectionSettingsInput, PlcConnection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const OPT_IN_ENV: &str = "BANTO_WASM_LIVE_917";
const SLMP_COMPONENT_ENV: &str = "BANTO_SLMP_LIVE_917_COMPONENT_WASM";
const SLMP_COMPONENT_SHA256: &str =
    "04e3706f2ad54dc174cd44f9fa058250655ce3d72b75ec0bc486e54521131e11";
const SLMP_MANIFEST: &[u8] = include_bytes!("../t12_slmp_wasm_operational_write/manifest.json");
const MODBUS_COMPONENT_ENV: &str = "BANTO_MODBUS_LIVE_917_COMPONENT_WASM";
const MODBUS_COMPONENT_SHA256: &str =
    "af6cb1685991b5296f3423ac129e6c1482c371dc0a18e1b9135b1482e9eb888a";
const MODBUS_MANIFEST_ENV: &str = "BANTO_MODBUS_LIVE_917_MANIFEST";
const MODBUS_MANIFEST_SHA256: &str =
    "7399a90c0a145d803c73ed6ded9b28c695d9462f8989cf608f30dca8df2f6c43";
const HUB_FRAME_ENV: &str = "BANTO_WASM_LIVE_917_HUB_FRAME";

const SLMP_PROTOCOL: &str = "slmp-wasm-operational-write";
const MODBUS_PROTOCOL: &str = "modbus-fc3-wasm";
const HUB_PORT: u16 = 3108;
const FORBIDDEN_PORTS: [u16; 2] = [5200, 3100];
const MODBUS_PORT: u16 = 502;
const MODBUS_TARGET_ENV: &str = "BANTO_MODBUS_LIVE_917_TARGET";
const GROUP: &str = "g";
const BIT: u8 = 5;

struct Plc {
    label: &'static str,
    name: &'static str,
    host: &'static str,
    allowlist: &'static str,
    d_first: u32,
    d_string: u32,
    /// Word whose bit 5 is written in F and which is restored as a word.
    word_bit_target: &'static str,
    /// Q only: a D word bit whose write must be refused before send.
    refused_bit: Option<&'static str>,
}

const R: Plc = Plc {
    label: "R",
    name: "r-live-917",
    host: "192.168.11.200",
    allowlist: "D3020-D3039,D3020.0-D3020.F",
    d_first: 3020,
    d_string: 3030,
    word_bit_target: "D3020",
    refused_bit: None,
};

const Q: Plc = Plc {
    label: "Q",
    name: "q-live-917",
    host: "192.168.11.10",
    allowlist: "D1020-D1039,W1020-W103F,W1020.0-W1020.F",
    d_first: 1020,
    d_string: 1030,
    word_bit_target: "W1020",
    refused_bit: Some("D1020.5"),
};

fn audit(stage: &str, detail: impl std::fmt::Display) {
    let unix_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("UTC clock before Unix epoch")
        .as_nanos();
    println!("utc_unix_ns={unix_ns} stage={stage} {detail}");
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn require_opt_in() {
    assert_eq!(
        std::env::var(OPT_IN_ENV).as_deref(),
        Ok("1"),
        "{OPT_IN_ENV}=1 is required before any live dial"
    );
}

fn pinned_file(env: &str, expected: &str, what: &str) -> Vec<u8> {
    let path = std::env::var(env).unwrap_or_else(|_| panic!("set {env}"));
    let bytes = std::fs::read(&path).unwrap_or_else(|error| panic!("{what} {path}: {error}"));
    let digest = hex(&Sha256::digest(&bytes));
    assert_eq!(
        digest, expected,
        "{what} SHA-256 differs from the pinned guest"
    );
    audit(what, format!("path={path} sha256={digest}"));
    bytes
}

/// The saved connection must still name the approved TCP destination (and
/// never a resident-service port) when the broker is about to dial it.
fn approved_destination(saved: &PlcConnection, protocol: &str, host: &str, port: u16) -> bool {
    let Some(saved_port) = saved.setting_i64("port") else {
        return false;
    };
    if FORBIDDEN_PORTS.iter().any(|p| i64::from(*p) == saved_port) {
        return false;
    }
    let pairs: Vec<(String, String)> = saved
        .settings
        .iter()
        .map(|(key, value)| {
            let text = match value {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            (key.clone(), text)
        })
        .collect();
    let Ok(settings) = Settings::from_pairs(pairs) else {
        return false;
    };
    saved.protocol == protocol
        && saved.enabled
        && !saved.simulation
        && saved.setting_str("host") == Some(host)
        && saved_port == i64::from(port)
        && matches!(
            TransportSpec::from_settings(&settings, &[TransportKind::Tcp]),
            Ok(TransportSpec::Tcp { host: h, port: p }) if h == host && p == port
        )
}

fn unix_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

/// One temporary key with exactly these scopes, valid for 10 minutes.
async fn temp_key(app: &super::TestApp, label: &str, scopes: &[String]) -> (i64, String) {
    let (status, body) = super::admin_write(
        &app.router,
        "POST",
        "/api/api-keys",
        &app.admin_token,
        json!({"name": label, "expiresAt": unix_ms() + 10 * 60 * 1000, "scopes": scopes}),
    )
    .await;
    assert_eq!(
        status,
        super::StatusCode::CREATED,
        "API key issuance: {body:?}"
    );
    audit(
        "api_key_issued",
        format!("id={} scopes={}", body["id"], scopes.len()),
    );
    (
        body["id"].as_i64().unwrap(),
        body["key"].as_str().unwrap().to_owned(),
    )
}

async fn revoke_key(app: &super::TestApp, key_id: i64) {
    let (status, body) = super::admin_write(
        &app.router,
        "POST",
        &format!("/api/api-keys/{key_id}/revoke"),
        &app.admin_token,
        json!({}),
    )
    .await;
    assert_eq!(status, super::StatusCode::OK, "{body:?}");
    audit("api_key_revoked", format!("id={key_id}"));
}

async fn wait_connected(app: &super::TestApp, connection: i64) {
    let mut status = app.sessions.status_watch(connection).unwrap();
    tokio::time::timeout(
        Duration::from_secs(10),
        status.wait_for(|state| *state == banto_broker::BrokerConnectionStatus::Connected),
    )
    .await
    .expect("bounded wait for the broker connection")
    .unwrap();
    audit("broker_connected", format!("connection={connection}"));
}

async fn read_now(app: &super::TestApp, key: &str, tag: &str) -> (super::StatusCode, Value) {
    let (status, body) =
        super::admin_get(&app.router, &format!("/api/v1/values/{tag}/read-now"), key).await;
    audit("read_now", format!("tag={tag} status={status} body={body}"));
    (status, body)
}

async fn write_one(
    app: &super::TestApp,
    key: &str,
    tag: &str,
    value: Value,
) -> (super::StatusCode, Value) {
    let (status, body) = super::admin_write(
        &app.router,
        "POST",
        &format!("/api/v1/values/{tag}"),
        key,
        json!({"v": value}),
    )
    .await;
    audit(
        "write",
        format!("tag={tag} v={value} status={status} body={body}"),
    );
    (status, body)
}

async fn write_batch(
    app: &super::TestApp,
    key: &str,
    entries: &[(String, Value)],
) -> (super::StatusCode, Value) {
    let (status, body) = super::admin_write(
        &app.router,
        "POST",
        "/api/v1/values/batch",
        key,
        json!({"writes": entries.iter().map(|(tag, v)| json!({"tag": tag, "v": v})).collect::<Vec<_>>()}),
    )
    .await;
    audit(
        "write_batch",
        format!("entries={entries:?} status={status} body={body}"),
    );
    (status, body)
}

fn good_u16(body: &Value) -> Option<u16> {
    if body["q"] != "good" {
        return None;
    }
    let value = body["v"].as_f64()?;
    (value.is_finite() && value.fract() == 0.0 && (0.0..=65535.0).contains(&value))
        .then_some(value as u16)
}

/// Audit `result` values of the rows for `tag_ids`, in list order.
async fn audited_results(app: &super::TestApp, tag_ids: &[i64]) -> Vec<String> {
    let (status, rows) = super::admin_write(
        &app.router,
        "POST",
        "/api/write-audit/list",
        &app.admin_token,
        json!({}),
    )
    .await;
    assert_eq!(status, super::StatusCode::OK, "{rows:?}");
    let results: Vec<String> = rows["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| {
            row["tagId"]
                .as_i64()
                .is_some_and(|id| tag_ids.contains(&id))
        })
        .map(|row| row["result"].as_str().unwrap_or("?").to_string())
        .collect();
    audit("write_audit_results", format!("{results:?}"));
    results
}

async fn audited_ok_writes(app: &super::TestApp, tag_ids: &[i64]) -> usize {
    let (status, rows) = super::admin_write(
        &app.router,
        "POST",
        "/api/write-audit/list",
        &app.admin_token,
        json!({}),
    )
    .await;
    assert_eq!(status, super::StatusCode::OK, "{rows:?}");
    let rows = rows["rows"].as_array().unwrap();
    for row in rows {
        audit(
            "write_audit",
            format!("tagId={} result={} ", row["tagId"], row["result"]),
        );
    }
    rows.iter()
        .filter(|row| {
            row["tagId"]
                .as_i64()
                .is_some_and(|id| tag_ids.contains(&id))
                && row["result"] == "ok"
        })
        .count()
}

/// `(name, address, dataType, stringLength, writable)`.
type Tag<'a> = (&'a str, &'a str, &'a str, Option<i64>, bool);

/// Save a connection and its tags, check the destination before and after
/// saving, start the broker, wait for the connection.
#[allow(clippy::too_many_arguments)]
async fn saved_app(
    label: &str,
    protocol: &str,
    name: &str,
    host: &str,
    port: u16,
    settings: Value,
    manifest: &[u8],
    component: &[u8],
    tags: &[Tag<'_>],
) -> (super::TestApp, i64, Vec<i64>, Vec<String>) {
    require_opt_in();
    assert!(
        !FORBIDDEN_PORTS.contains(&port),
        "resident service port {port}"
    );
    assert_eq!(settings["host"], host);
    assert_eq!(settings["port"], port);
    assert!(settings.get("transport").is_none_or(|t| t == "tcp"));
    let app = super::test_app_with_operational_write_manifest(label, manifest, component).await;
    let saved = app
        .manager
        .plc_connections()
        .create_settings(ConnectionSettingsInput {
            name: name.into(),
            protocol: protocol.into(),
            enabled: true,
            simulation: false,
            settings: settings.as_object().unwrap().clone(),
            secrets: Default::default(),
        })
        .await
        .expect("save the connection");
    assert!(
        approved_destination(&saved, protocol, host, port),
        "stored destination changed: {:?}",
        saved.settings
    );
    let group = super::CollectionGroupService::with_registry(
        app.pool.clone(),
        app.manager.registry().clone(),
    )
    .create(super::group_input(GROUP, saved.id, 60_000))
    .await
    .unwrap();
    let mut ids = Vec::new();
    let mut names = Vec::new();
    for (tag_name, address, data_type, string_length, writable) in tags {
        let mut input = super::tag_input(tag_name, group.id, address, data_type);
        input.string_length = *string_length;
        input.writable = *writable;
        let created = app.manager.tags().create(input).await;
        match created {
            Ok(tag) => {
                audit(
                    "tag_registered",
                    format!("{tag_name} {address} {data_type} id={}", tag.id),
                );
                ids.push(tag.id);
                names.push(format!("{name}.{GROUP}.{tag_name}"));
            }
            Err(error) => {
                audit(
                    "tag_refused",
                    format!("{tag_name} {address} {data_type}: {error:?}"),
                );
                ids.push(-1);
                names.push(String::new());
            }
        }
    }
    let reloaded = app
        .manager
        .plc_connections()
        .get(saved.id)
        .await
        .expect("saved connection");
    assert!(approved_destination(&reloaded, protocol, host, port));
    audit("broker_connect_start", format!("destination={host}:{port}"));
    app.commit_and_start().await;
    wait_connected(&app, saved.id).await;
    (app, saved.id, ids, names)
}

fn slmp_settings(plc: &Plc) -> Value {
    let frame = std::env::var(HUB_FRAME_ENV).unwrap_or_else(|_| "4e".into());
    assert!(
        matches!(frame.as_str(), "4e" | "3e"),
        "{HUB_FRAME_ENV} must be 4e or 3e"
    );
    json!({
        "host": plc.host,
        "port": HUB_PORT,
        "frame": frame,
        "network_no": 0,
        "target_cpu": "own",
        "d_u16_write_mode": "enabled",
        "d_u16_write_allowlist": plc.allowlist
    })
}

fn slmp_component() -> Vec<u8> {
    pinned_file(SLMP_COMPONENT_ENV, SLMP_COMPONENT_SHA256, "slmp_component")
}

fn modbus_component() -> Vec<u8> {
    pinned_file(
        MODBUS_COMPONENT_ENV,
        MODBUS_COMPONENT_SHA256,
        "modbus_component",
    )
}

fn modbus_manifest() -> Vec<u8> {
    pinned_file(
        MODBUS_MANIFEST_ENV,
        MODBUS_MANIFEST_SHA256,
        "modbus_manifest",
    )
}

// ---------------------------------------------------------------- offline

#[test]
fn hub_live_917_destination_check_refuses_resident_ports_and_other_hosts() {
    let saved = |host: &str, port: i64, extra: &[(&str, Value)]| {
        let mut settings = slmp_settings(&R).as_object().unwrap().clone();
        settings.insert("host".into(), json!(host));
        settings.insert("port".into(), json!(port));
        for (key, value) in extra {
            settings.insert((*key).into(), value.clone());
        }
        PlcConnection::from_settings(
            1,
            R.name.into(),
            SLMP_PROTOCOL.into(),
            true,
            false,
            settings,
            1,
        )
    };
    assert!(approved_destination(
        &saved(R.host, 3108, &[]),
        SLMP_PROTOCOL,
        R.host,
        HUB_PORT
    ));
    assert!(!approved_destination(
        &saved(R.host, 5200, &[]),
        SLMP_PROTOCOL,
        R.host,
        5200
    ));
    assert!(!approved_destination(
        &saved(Q.host, 3100, &[]),
        SLMP_PROTOCOL,
        Q.host,
        3100
    ));
    assert!(!approved_destination(
        &saved(R.host, 3109, &[]),
        SLMP_PROTOCOL,
        R.host,
        HUB_PORT
    ));
    assert!(!approved_destination(
        &saved(Q.host, 3108, &[]),
        SLMP_PROTOCOL,
        R.host,
        HUB_PORT
    ));
    assert!(!approved_destination(
        &saved(R.host, 3108, &[("transport", json!("udp"))]),
        SLMP_PROTOCOL,
        R.host,
        HUB_PORT
    ));
    assert!(!approved_destination(
        &saved(R.host, 3108, &[]),
        MODBUS_PROTOCOL,
        R.host,
        HUB_PORT
    ));
    if std::env::var(OPT_IN_ENV).is_err() {
        assert!(std::panic::catch_unwind(require_opt_in).is_err());
    }
}

#[test]
fn hub_live_917_destination_modbus_targets_are_kept_apart() {
    let saved = |target: &ModbusTarget, host: &str, port: i64| {
        let mut settings = modbus_settings(target, "", "", &[])
            .as_object()
            .unwrap()
            .clone();
        settings.insert("host".into(), json!(host));
        settings.insert("port".into(), json!(port));
        PlcConnection::from_settings(
            1,
            "mb-live-917".into(),
            MODBUS_PROTOCOL.into(),
            true,
            false,
            settings,
            1,
        )
    };
    for (target, other) in [(&MODBUS_SLAVE, &MODBUS_R), (&MODBUS_R, &MODBUS_SLAVE)] {
        let port = i64::from(MODBUS_PORT);
        assert!(approved_destination(
            &saved(target, target.host, port),
            MODBUS_PROTOCOL,
            target.host,
            MODBUS_PORT
        ));
        // The saved connection names the other target, or the approved
        // destination is the other target: both refused.
        assert!(!approved_destination(
            &saved(target, other.host, port),
            MODBUS_PROTOCOL,
            target.host,
            MODBUS_PORT
        ));
        assert!(!approved_destination(
            &saved(target, target.host, port),
            MODBUS_PROTOCOL,
            other.host,
            MODBUS_PORT
        ));
        assert!(!approved_destination(
            &saved(target, target.host, 5020),
            MODBUS_PROTOCOL,
            target.host,
            MODBUS_PORT
        ));
        assert!(!approved_destination(
            &saved(target, target.host, 5200),
            MODBUS_PROTOCOL,
            target.host,
            5200
        ));
    }
    assert_eq!(parse_modbus_target(None).label, "slave");
    assert_eq!(parse_modbus_target(Some("slave")).label, "slave");
    assert_eq!(parse_modbus_target(Some("r")).label, "r");
    for bad in ["", "R", "q", "127.0.0.1", "192.168.11.200"] {
        assert!(
            std::panic::catch_unwind(|| parse_modbus_target(Some(bad))).is_err(),
            "{bad:?}"
        );
    }
}

// -------------------------------------------------------- SLMP: reads

/// E1/E2 (frame and own-station route from the connection settings), A2, D1-D3,
/// G1 and the C1 string read, each as a saved tag answered by `read-now`
/// with a read-only key. A write with that key must be refused (403) and the
/// write control stays disabled, so nothing can be written.
async fn hub_slmp_reads(plc: &Plc) {
    require_opt_in();
    let d: Vec<String> = (plc.d_first..plc.d_first + 4)
        .map(|n| format!("D{n}"))
        .collect();
    let string_address = format!("D{}", plc.d_string);
    let mut tags: Vec<Tag<'_>> = vec![
        ("d0", &d[0], "u16", None, false),
        ("d1", &d[1], "u16", None, false),
        ("d2", &d[2], "u16", None, false),
        ("d3", &d[3], "u16", None, false),
        ("w1a", "W1A", "u16", None, false),
        ("w1a0", "W1A.0", "bit", None, false),
        ("w1af", "W1A.F", "bit", None, false),
        ("s10", "S10", "bit", None, false),
        ("z3", "Z3", "u16", None, false),
    ];
    if plc.label == "R" {
        tags.push(("rd7", "RD7", "u16", None, false));
    }
    tags.extend([
        ("zr10", "ZR10", "u16", None, false),
        ("u0g0", "U0\\G0", "u16", None, false),
        ("u0g0b", "U0\\G0.0", "bit", None, false),
        ("u0g0s", "U0\\G0", "string", Some(2), false),
        ("u3e0", "U3E0\\G0", "u16", None, false),
        ("str", &string_address, "string", Some(4), false),
    ]);
    let (app, _connection, _ids, names) = saved_app(
        &format!("wasm-live-917-{}-reads", plc.label),
        SLMP_PROTOCOL,
        plc.name,
        plc.host,
        HUB_PORT,
        slmp_settings(plc),
        SLMP_MANIFEST,
        &slmp_component(),
        &tags,
    )
    .await;
    let registered: Vec<&String> = names.iter().filter(|n| !n.is_empty()).collect();
    let scopes: Vec<String> = registered.iter().map(|n| format!("read:{n}")).collect();
    let (key_id, key) = temp_key(&app, &format!("{}-reads-917", plc.name), &scopes).await;
    // Tags that must read on this PLC; the rest are recorded only.
    let mut required: Vec<&str> = vec!["d0", "d1", "d2", "d3", "w1a", "w1a0", "w1af", "str"];
    if plc.label == "R" {
        required.extend(["z3", "rd7", "zr10", "u0g0", "u0g0b", "u0g0s", "u3e0"]);
    }
    let mut a2: Vec<Option<Value>> = Vec::new();
    for (name, tag) in names.iter().zip(&tags) {
        let must_read = required.contains(&tag.0);
        if name.is_empty() {
            assert!(!must_read, "tag {} failed to register", tag.0);
            continue;
        }
        let (status, body) = read_now(&app, &key, name).await;
        audit(
            "read_checked",
            format!("tag={} record_only={}", tag.0, !must_read),
        );
        if must_read {
            assert_eq!(status, super::StatusCode::OK, "{}: {body:?}", tag.0);
            assert_eq!(body.get("q"), Some(&json!("good")), "{}: {body:?}", tag.0);
            let v = body.get("v").unwrap_or(&Value::Null);
            match tag.2 {
                "string" => assert!(v.is_string(), "{} not a string: {body:?}", tag.0),
                "bit" => assert!(v.is_boolean(), "{} not a bool: {body:?}", tag.0),
                _ => assert!(v.is_number(), "{} not a number: {body:?}", tag.0),
            }
        }
        if ["w1a", "w1a0", "w1af"].contains(&tag.0) {
            a2.push(body.get("v").cloned());
        }
    }
    let word = a2[0]
        .as_ref()
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite() && v.fract() == 0.0 && (0.0..=65535.0).contains(v))
        .map(|v| v as u64)
        .unwrap_or_else(|| panic!("W1A not a u16: {a2:?}"));
    assert_eq!(
        a2[1],
        Some(json!(word & 1 != 0)),
        "W1A.0 differs from W1A={word:#06x}"
    );
    assert_eq!(
        a2[2],
        Some(json!(word & (1 << 15) != 0)),
        "W1A.F differs from W1A={word:#06x}"
    );
    let (status, _) = write_one(&app, &key, registered[0], json!(0)).await;
    assert_eq!(
        status,
        super::StatusCode::FORBIDDEN,
        "a read-only key must not write"
    );
    revoke_key(&app, key_id).await;
    drop(app);
    audit(
        "test_app_closed",
        "temporary broker, router and database dropped",
    );
}

// ------------------------------------------------------- SLMP: writes

/// F and C1 through REST: bit 5 of one word (R D3020, Q W1020) is written
/// inverted, the word is read back, the original word is written back; the
/// 4-word string at D3030/D1030 is read as four words, written as "AB",
/// read back, and the four original words are written back as one batch.
/// On Q, the D word bit write must fail before send. Every successful write
/// is audited.
async fn hub_slmp_writes(plc: &Plc) {
    require_opt_in();
    let bit_address = format!("{}.{BIT:X}", plc.word_bit_target);
    let words: Vec<String> = (plc.d_string..plc.d_string + 4)
        .map(|n| format!("D{n}"))
        .collect();
    let mut tags: Vec<Tag<'_>> = vec![
        ("word", plc.word_bit_target, "u16", None, true),
        ("bit", &bit_address, "bit", None, true),
        ("s0", &words[0], "u16", None, true),
        ("s1", &words[1], "u16", None, true),
        ("s2", &words[2], "u16", None, true),
        ("s3", &words[3], "u16", None, true),
        ("str", &words[0], "string", Some(4), true),
    ];
    if let Some(refused) = plc.refused_bit {
        tags.push(("dbit", refused, "bit", None, true));
    }
    let (app, _connection, ids, names) = saved_app(
        &format!("wasm-live-917-{}-writes", plc.label),
        SLMP_PROTOCOL,
        plc.name,
        plc.host,
        HUB_PORT,
        slmp_settings(plc),
        SLMP_MANIFEST,
        &slmp_component(),
        &tags,
    )
    .await;
    assert!(
        names.iter().all(|n| !n.is_empty()),
        "every tag must register: {names:?}"
    );
    let mut scopes = Vec::new();
    for name in &names {
        scopes.push(format!("read:{name}"));
        scopes.push(format!("write:{name}"));
    }
    let (key_id, key) = temp_key(&app, &format!("{}-writes-917", plc.name), &scopes).await;
    let (word, bit, s0, s1, s2, s3, string) = (
        &names[0], &names[1], &names[2], &names[3], &names[4], &names[5], &names[6],
    );

    // A valid write scope still cannot write while write_control is off.
    let (status, body) = write_one(&app, &key, word, json!(0)).await;
    assert_eq!(status, super::StatusCode::SERVICE_UNAVAILABLE, "{body:?}");
    let (status, body) = super::admin_write(
        &app.router,
        "POST",
        "/api/write-control/enable",
        &app.admin_token,
        json!({}),
    )
    .await;
    assert_eq!(status, super::StatusCode::OK, "{body:?}");

    if let Some(dbit) = names.get(7) {
        let (status, body) = write_one(&app, &key, dbit, json!(true)).await;
        assert_ne!(
            status,
            super::StatusCode::OK,
            "Q D word bit must be refused: {body:?}"
        );
    }

    // F
    let original = good_u16(&read_now(&app, &key, word).await.1).expect("word read 1");
    tokio::time::sleep(Duration::from_secs(1)).await;
    let repeated = good_u16(&read_now(&app, &key, word).await.1).expect("word read 2");
    assert_eq!(
        original, repeated,
        "word changed between reads; nothing written"
    );
    let trial_bit = original & (1 << BIT) == 0;
    let (status, body) = write_one(&app, &key, bit, json!(trial_bit)).await;
    let observed = good_u16(&read_now(&app, &key, word).await.1);
    assert_eq!(
        status,
        super::StatusCode::OK,
        "bit write uncertain ({observed:?}); stop: {body:?}"
    );
    assert_eq!(
        observed,
        Some(original ^ (1 << BIT)),
        "other bits changed ({original:#06x}); stop, no restoration"
    );
    let (status, body) = write_one(&app, &key, word, json!(original)).await;
    assert_eq!(
        status,
        super::StatusCode::OK,
        "restoration uncertain: {body:?}"
    );
    let restored = good_u16(&read_now(&app, &key, word).await.1);
    assert_eq!(restored, Some(original), "original word not confirmed");

    // C1
    let read_words = |app: &super::TestApp, key: String| {
        let tags = [s0.clone(), s1.clone(), s2.clone(), s3.clone()];
        let router = app.router.clone();
        async move {
            let mut values = Vec::new();
            for tag in tags {
                let (status, body) =
                    super::admin_get(&router, &format!("/api/v1/values/{tag}/read-now"), &key)
                        .await;
                audit("read_now", format!("tag={tag} status={status} body={body}"));
                values.push(good_u16(&body).expect("one u16 word"));
            }
            values
        }
    };
    let original_words = read_words(&app, key.clone()).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let repeated_words = read_words(&app, key.clone()).await;
    assert_eq!(
        original_words, repeated_words,
        "string words changed; nothing written"
    );
    read_now(&app, &key, string).await;
    let (status, body) = write_one(&app, &key, string, json!("AB")).await;
    let back = read_now(&app, &key, string).await.1;
    let back_words = read_words(&app, key.clone()).await;
    assert_eq!(
        status,
        super::StatusCode::OK,
        "string write uncertain; stop: {body:?}"
    );
    assert_eq!(
        back["v"], "AB",
        "string read back differs ({back_words:04x?}); stop"
    );
    assert_eq!(
        back_words,
        vec![u16::from_le_bytes(*b"AB"), 0, 0, 0],
        "padded words differ; stop, no restoration"
    );
    let entries: Vec<(String, Value)> = [s0, s1, s2, s3]
        .into_iter()
        .zip(&original_words)
        .map(|(tag, value)| (tag.clone(), json!(value)))
        .collect();
    let (status, body) = write_batch(&app, &key, &entries).await;
    assert_eq!(
        status,
        super::StatusCode::OK,
        "restoration uncertain: {body:?}"
    );
    assert!(
        body["writes"]
            .as_array()
            .is_some_and(|w| w.len() == 4 && w.iter().all(|r| r["ok"] == true)),
        "restoration not confirmed: {body:?}"
    );
    let restored_words = read_words(&app, key.clone()).await;
    assert_eq!(
        restored_words, original_words,
        "original words not confirmed"
    );

    let ok_rows = audited_ok_writes(&app, &ids).await;
    audit("write_audit_ok_rows", ok_rows);
    assert_eq!(ok_rows, 7, "bit, word, string and four restored words");
    let (status, body) = super::admin_write(
        &app.router,
        "POST",
        "/api/write-control/disable",
        &app.admin_token,
        json!({}),
    )
    .await;
    assert_eq!(status, super::StatusCode::OK, "{body:?}");
    revoke_key(&app, key_id).await;
    drop(app);
    audit(
        "test_app_closed",
        "temporary broker, router and database dropped",
    );
}

// ------------------------------------------------------------- Modbus

/// One approved Modbus destination: the local ModbusSlave or the R CPU.
/// Allowlists and addresses use the guest's notation.
struct ModbusTarget {
    label: &'static str,
    host: &'static str,
    coil_allowlist: &'static str,
    holding_allowlist: &'static str,
    /// B2: the 1-based number of the first of eight coils.
    first_coil_number: u16,
    probe_coil: &'static str,
    probe_holding: &'static str,
    bit_address: &'static str,
    word_address: &'static str,
    string_address: &'static str,
    string_second: &'static str,
}

const MODBUS_SLAVE: ModbusTarget = ModbusTarget {
    label: "slave",
    host: "127.0.0.1",
    coil_allowlist: "000001-000008",
    holding_allowlist: "400010-400029",
    first_coil_number: 1,
    probe_coil: "00001",
    probe_holding: "40010",
    bit_address: "40010.3",
    word_address: "40010",
    string_address: "40020",
    string_second: "40022",
};

/// R CPU: coil offset 8192+n = M n, holding offset n = D n. Coils
/// 09193-09200 = M1000-M1007, holding 43021-43040 = D3020-D3039.
const MODBUS_R: ModbusTarget = ModbusTarget {
    label: "r",
    host: "192.168.11.200",
    coil_allowlist: "009193-009200",
    holding_allowlist: "403021-403040",
    first_coil_number: 9193,
    probe_coil: "09193",
    probe_holding: "43021",
    bit_address: "43021.3",
    word_address: "43021",
    string_address: "43031",
    string_second: "43033",
};

fn parse_modbus_target(value: Option<&str>) -> &'static ModbusTarget {
    match value {
        None | Some("slave") => &MODBUS_SLAVE,
        Some("r") => &MODBUS_R,
        Some(other) => panic!("{MODBUS_TARGET_ENV} must be `slave` or `r`, not {other:?}"),
    }
}

fn modbus_target() -> &'static ModbusTarget {
    let target = parse_modbus_target(std::env::var(MODBUS_TARGET_ENV).ok().as_deref());
    audit(
        "modbus_target",
        format!(
            "target={} destination={}:{MODBUS_PORT}",
            target.label, target.host
        ),
    );
    target
}

fn modbus_settings(
    target: &ModbusTarget,
    guest_allowlist: &str,
    host_allowlist: &str,
    extra: &[(&str, &str)],
) -> Value {
    let mut settings = json!({
        "host": target.host, "port": MODBUS_PORT, "unit_id": 1,
        "probe_address": target.probe_holding,
        "write_mode": "enabled", "write_allowlist": guest_allowlist,
        "modbus_operational_write_mode": "enabled",
        "modbus_operational_write_allowlist": host_allowlist
    });
    for (key, value) in extra {
        settings[*key] = json!(value);
    }
    settings
}

/// A1: the Hub connection test of a coil-only device (probe 00001) over REST.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Modbus target (BANTO_MODBUS_LIVE_917_TARGET); BANTO_WASM_LIVE_917=1 and owner approval required"]
async fn hub_modbus_a1_coil_only_connection_test() {
    require_opt_in();
    let target = modbus_target();
    let app = super::test_app_with_operational_write_manifest(
        "wasm-live-917-modbus-a1",
        &modbus_manifest(),
        &modbus_component(),
    )
    .await;
    let (status, body) = super::admin_write(
        &app.router,
        "POST",
        "/api/plc-connections/test",
        &app.admin_token,
        json!({
            "protocol": MODBUS_PROTOCOL, "simulation": false,
            "settings": {"host": target.host, "port": MODBUS_PORT, "unit_id": 1, "probe_address": target.probe_coil}
        }),
    )
    .await;
    audit(
        "a1_connection_test",
        format!("target={} status={status} body={body}", target.label),
    );
    assert_eq!(status, super::StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], true, "{body:?}");
    drop(app);
}

/// B1: `register_bit_write=disabled` refuses a register-bit write before send
/// (502 `write_failed`, audited `failed`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Modbus target (BANTO_MODBUS_LIVE_917_TARGET); BANTO_WASM_LIVE_917=1 and owner approval required"]
async fn hub_modbus_b1_register_bit_write_refused_without_fc22() {
    require_opt_in();
    let target = modbus_target();
    let (app, _connection, ids, names) = saved_app(
        "wasm-live-917-modbus-b1",
        MODBUS_PROTOCOL,
        "mb-live-917",
        target.host,
        MODBUS_PORT,
        modbus_settings(
            target,
            target.holding_allowlist,
            target.holding_allowlist,
            &[("register_bit_write", "disabled")],
        ),
        &modbus_manifest(),
        &modbus_component(),
        &[("hbit", target.bit_address, "bit", None, true)],
    )
    .await;
    let scopes = vec![format!("read:{}", names[0]), format!("write:{}", names[0])];
    let (key_id, key) = temp_key(&app, "mb-b1-917", &scopes).await;
    app.write_control.enable();
    // Evidence on this path is the driver's refusal reason plus an audit row
    // `failed` (refused before send), not an observed wire frame. `uncertain`
    // (sent, outcome unknown) stops the test at once.
    let (status, before) = read_now(&app, &key, &names[0]).await;
    assert_eq!(status, super::StatusCode::OK, "{before:?}");
    assert_eq!(before["q"], "good", "{before:?}");
    assert!(before["v"].is_boolean(), "{before:?}");
    let mut rows = audited_results(&app, &ids).await.len();
    for value in [true, false] {
        let (status, body) = write_one(&app, &key, &names[0], json!(value)).await;
        assert_eq!(status, super::StatusCode::BAD_GATEWAY, "{body:?}");
        assert_eq!(body["error"], "write_failed", "{body:?}");
        assert!(
            body.to_string().contains("register_bit_write=disabled"),
            "{body:?}"
        );
        let results = audited_results(&app, &ids).await;
        assert_eq!(results.len(), rows + 1, "{results:?}");
        rows += 1;
        if results.iter().any(|r| r == "uncertain") {
            panic!("audit shows `uncertain`: outcome unknown, stopping: {results:?}");
        }
        // The count grew by one and every row is `failed` (none `ok`).
        assert!(results.iter().all(|r| r == "failed"), "{results:?}");
    }
    let (status, after) = read_now(&app, &key, &names[0]).await;
    assert_eq!(status, super::StatusCode::OK, "{after:?}");
    assert_eq!(after["v"], before["v"], "{after:?}");
    assert_eq!(audited_ok_writes(&app, &ids).await, 0);
    app.write_control.disable();
    revoke_key(&app, key_id).await;
    drop(app);
}

/// B2: eight coils inverted as one batch (FC15), the word written with
/// `register_write=fc6`, both read back and restored. Writes: 8 + 1 + 8 + 1
/// audit rows, well inside the default 30/60 s global and 10/60 s per-tag
/// rate limits of the test app.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Modbus target (BANTO_MODBUS_LIVE_917_TARGET); BANTO_WASM_LIVE_917=1 and owner approval required"]
async fn hub_modbus_b2_fc15_coils_and_fc6_register_write_readback_restore() {
    require_opt_in();
    let target = modbus_target();
    let coil_names: Vec<String> = (1..=8).map(|n| format!("c{n}")).collect();
    let coil_addresses: Vec<String> = (0..8)
        .map(|i| format!("{:05}", target.first_coil_number + i))
        .collect();
    let mut tags: Vec<Tag<'_>> = coil_names
        .iter()
        .zip(&coil_addresses)
        .map(|(name, address)| (name.as_str(), address.as_str(), "bit", None, true))
        .collect();
    tags.push(("h", target.word_address, "u16", None, true));
    let allowlist = format!("{},{}", target.coil_allowlist, target.holding_allowlist);
    let (app, _connection, ids, names) = saved_app(
        "wasm-live-917-modbus-b2",
        MODBUS_PROTOCOL,
        "mb-live-917",
        target.host,
        MODBUS_PORT,
        modbus_settings(target, &allowlist, &allowlist, &[("register_write", "fc6")]),
        &modbus_manifest(),
        &modbus_component(),
        &tags,
    )
    .await;
    let mut scopes = Vec::new();
    for name in &names {
        scopes.push(format!("read:{name}"));
        scopes.push(format!("write:{name}"));
    }
    let (key_id, key) = temp_key(&app, "mb-b2-917", &scopes).await;
    app.write_control.enable();
    let coils = &names[..8];
    let holding = &names[8];
    let read_coils = |app: &super::TestApp, key: String| {
        let tags = coils.to_vec();
        let router = app.router.clone();
        async move {
            let mut bits = Vec::new();
            for tag in tags {
                let (status, body) =
                    super::admin_get(&router, &format!("/api/v1/values/{tag}/read-now"), &key)
                        .await;
                audit("read_now", format!("tag={tag} status={status} body={body}"));
                assert_eq!(body["q"], "good", "{body:?}");
                bits.push(body["v"].as_bool().expect("coil bool"));
            }
            bits
        }
    };
    let coils_1 = read_coils(&app, key.clone()).await;
    let word_1 = good_u16(&read_now(&app, &key, holding).await.1).expect("word u16");
    tokio::time::sleep(Duration::from_secs(1)).await;
    let coils_2 = read_coils(&app, key.clone()).await;
    let word_2 = good_u16(&read_now(&app, &key, holding).await.1).expect("word u16");
    assert_eq!(coils_1, coils_2, "coils changed; nothing written");
    assert_eq!(word_1, word_2, "word changed; nothing written");

    let batch = |values: &[bool]| -> Vec<(String, Value)> {
        coils
            .iter()
            .zip(values)
            .map(|(tag, bit)| (tag.clone(), json!(bit)))
            .collect()
    };
    let inverted: Vec<bool> = coils_1.iter().map(|b| !b).collect();
    let (status, body) = write_batch(&app, &key, &batch(&inverted)).await;
    let coils_back = read_coils(&app, key.clone()).await;
    assert_eq!(
        status,
        super::StatusCode::OK,
        "coil batch uncertain; stop: {body:?}"
    );
    assert!(
        body["writes"]
            .as_array()
            .is_some_and(|w| w.iter().all(|r| r["ok"] == true)),
        "coil batch not confirmed; stop: {body:?}"
    );
    assert_eq!(
        coils_back, inverted,
        "coils read back differ; stop, no restoration"
    );

    let trial = if word_1 == 0 { 1 } else { 0 };
    let (status, body) = write_one(&app, &key, holding, json!(trial)).await;
    let word_back = good_u16(&read_now(&app, &key, holding).await.1);
    assert_eq!(
        status,
        super::StatusCode::OK,
        "word write uncertain; stop: {body:?}"
    );
    assert_eq!(
        word_back,
        Some(trial),
        "word read back differs; stop, no restoration"
    );

    let (status, body) = write_batch(&app, &key, &batch(&coils_1)).await;
    assert_eq!(
        status,
        super::StatusCode::OK,
        "coil restoration uncertain: {body:?}"
    );
    assert_eq!(
        read_coils(&app, key.clone()).await,
        coils_1,
        "original coils not confirmed"
    );
    let (status, body) = write_one(&app, &key, holding, json!(word_1)).await;
    assert_eq!(
        status,
        super::StatusCode::OK,
        "word restoration uncertain: {body:?}"
    );
    assert_eq!(
        good_u16(&read_now(&app, &key, holding).await.1),
        Some(word_1)
    );
    let ok_rows = audited_ok_writes(&app, &ids).await;
    audit("write_audit_ok_rows", ok_rows);
    assert_eq!(ok_rows, 18);
    app.write_control.disable();
    revoke_key(&app, key_id).await;
    drop(app);
}

/// C1: the 4-word string at the target's string address is read as two u32, written as "AB" (one
/// FC16), read back, and the two original u32 are written back as one batch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Modbus target (BANTO_MODBUS_LIVE_917_TARGET); BANTO_WASM_LIVE_917=1 and owner approval required"]
async fn hub_modbus_c1_holding_string_write_readback_restore() {
    require_opt_in();
    let target = modbus_target();
    let first_register: u32 = target.string_address.parse().expect("numeric address");
    let register_addresses: Vec<String> =
        (0..4).map(|n| (first_register + n).to_string()).collect();
    let (app, _connection, ids, names) = saved_app(
        "wasm-live-917-modbus-c1",
        MODBUS_PROTOCOL,
        "mb-live-917",
        target.host,
        MODBUS_PORT,
        modbus_settings(
            target,
            target.holding_allowlist,
            target.holding_allowlist,
            &[],
        ),
        &modbus_manifest(),
        &modbus_component(),
        &[
            ("w0", target.string_address, "u32", None, true),
            ("w1", target.string_second, "u32", None, true),
            ("str", target.string_address, "string", Some(4), true),
            ("r0", &register_addresses[0], "u16", None, false),
            ("r1", &register_addresses[1], "u16", None, false),
            ("r2", &register_addresses[2], "u16", None, false),
            ("r3", &register_addresses[3], "u16", None, false),
        ],
    )
    .await;
    let mut scopes = Vec::new();
    for (index, name) in names.iter().enumerate() {
        scopes.push(format!("read:{name}"));
        if index < 3 {
            scopes.push(format!("write:{name}"));
        }
    }
    let (key_id, key) = temp_key(&app, "mb-c1-917", &scopes).await;
    app.write_control.enable();
    let (w0, w1, string) = (&names[0], &names[1], &names[2]);
    let read_span = |app: &super::TestApp, key: String| {
        let tags = [w0.clone(), w1.clone()];
        let router = app.router.clone();
        async move {
            let mut values = Vec::new();
            for tag in tags {
                let (status, body) =
                    super::admin_get(&router, &format!("/api/v1/values/{tag}/read-now"), &key)
                        .await;
                audit("read_now", format!("tag={tag} status={status} body={body}"));
                assert_eq!(body["q"], "good", "{body:?}");
                values.push(body["v"].as_f64().expect("u32 as number"));
            }
            values
        }
    };
    let original = read_span(&app, key.clone()).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let repeated = read_span(&app, key.clone()).await;
    assert_eq!(original, repeated, "span changed; nothing written");
    read_now(&app, &key, string).await;
    let (status, body) = write_one(&app, &key, string, json!("AB")).await;
    let back = read_now(&app, &key, string).await.1;
    let back_span = read_span(&app, key.clone()).await;
    let mut back_registers = Vec::new();
    for tag in &names[3..7] {
        let (_, reg) = read_now(&app, &key, tag).await;
        back_registers.push(reg["v"].as_f64());
    }
    assert_eq!(
        status,
        super::StatusCode::OK,
        "string write uncertain; stop: {body:?}"
    );
    assert_eq!(
        back_registers,
        vec![Some(f64::from(0x4142u16)), Some(0.0), Some(0.0), Some(0.0)],
        "registers read back differ (span {back_span:?}); stop, no restoration"
    );
    assert_eq!(
        back["v"], "AB",
        "string read back differs ({back_span:?}); stop"
    );
    let entries = vec![
        (w0.clone(), json!(original[0])),
        (w1.clone(), json!(original[1])),
    ];
    let (status, body) = write_batch(&app, &key, &entries).await;
    assert_eq!(
        status,
        super::StatusCode::OK,
        "restoration uncertain: {body:?}"
    );
    assert!(
        body["writes"]
            .as_array()
            .is_some_and(|w| w.iter().all(|r| r["ok"] == true)),
        "restoration not confirmed: {body:?}"
    );
    assert_eq!(
        read_span(&app, key.clone()).await,
        original,
        "original span not confirmed"
    );
    let ok_rows = audited_ok_writes(&app, &ids).await;
    audit("write_audit_ok_rows", ok_rows);
    assert_eq!(ok_rows, 3);
    app.write_control.disable();
    revoke_key(&app, key_id).await;
    drop(app);
}

// ----------------------------------------------------------------- cases

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "real R test PLC; BANTO_WASM_LIVE_917=1 and owner approval required"]
async fn hub_r_read_stages_e1_e2_a2_d1_d3_g1_c1() {
    hub_slmp_reads(&R).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "real Q test PLC; BANTO_WASM_LIVE_917=1 and owner approval required"]
async fn hub_q_read_stages_e1_e2_a2_d1_d3_g1_c1() {
    hub_slmp_reads(&Q).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "real R test PLC; BANTO_WASM_LIVE_917=1 and owner approval required"]
async fn hub_r_write_stages_f_c1_readback_restore() {
    hub_slmp_writes(&R).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "real Q test PLC; BANTO_WASM_LIVE_917=1 and owner approval required"]
async fn hub_q_write_stages_f_c1_readback_restore() {
    hub_slmp_writes(&Q).await;
}
