//! Opt-in Hub API test for the approved R test PLC's D3010/u16 only.
//! The router is in-process; its database and API key are temporary.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use banto_driver_api::{Settings, Transport as TransportKind};
use banto_driver_engine::transport::TransportSpec;
use banto_tags::{ConnectionSettingsInput, PlcConnection, TagInput};
use serde_json::json;
use sha2::{Digest, Sha256};

const HOST: &str = "192.168.11.200";
const PORT: u16 = 3109;
const EXTERNAL_TAG: &str = "r-wasm-test.d3010-only.word";
const MANIFEST: &[u8] = include_bytes!("../t12_slmp_wasm_write/manifest.json");
// Built from reviewed guest fdb161520019b81a22bc2fec2bc9bf3d837f5bd3.
const COMPONENT_SHA256: [u8; 32] = [
    0x3c, 0xb4, 0x45, 0x1f, 0x71, 0x2f, 0xc4, 0xc0, 0xde, 0x3a, 0xb8, 0x3e, 0x85, 0xf9, 0xbb, 0x8a,
    0xef, 0xde, 0x6f, 0xa2, 0x6b, 0x5d, 0xe5, 0xa8, 0xb0, 0x99, 0x71, 0x74, 0x8c, 0x9d, 0x71, 0x4e,
];

fn audit(stage: &str, detail: impl std::fmt::Display) {
    let unix_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("UTC clock before Unix epoch")
        .as_nanos();
    println!("utc_unix_ns={unix_ns} stage={stage} {detail}");
}

fn approved_input(input: &ConnectionSettingsInput) -> bool {
    input.name == "r-wasm-test"
        && input.protocol == "slmp-write-fixture"
        && input.enabled
        && !input.simulation
        && input.secrets.is_empty()
        && input.settings
            == json!({"host": HOST, "port": PORT})
                .as_object()
                .unwrap()
                .clone()
}

fn approved_saved_connection(saved: &PlcConnection) -> bool {
    saved.name == "r-wasm-test"
        && saved.protocol == "slmp-write-fixture"
        && saved.enabled
        && !saved.simulation
        && saved.settings.len() == 2
        && saved.setting_str("host") == Some(HOST)
        && saved.setting_i64("port") == Some(i64::from(PORT))
}

fn approved_broker_destination(saved: &PlcConnection) -> bool {
    if !approved_saved_connection(saved) {
        return false;
    }
    let port = saved.setting_i64("port").unwrap().to_string();
    let Ok(settings) = Settings::from_pairs([
        ("host", saved.setting_str("host").unwrap()),
        ("port", port.as_str()),
    ]) else {
        return false;
    };
    matches!(
        TransportSpec::from_settings(&settings, &[TransportKind::Tcp]),
        Ok(TransportSpec::Tcp { host, port }) if host == HOST && port == PORT
    )
}

fn approved_tag(tag: &TagInput, group_id: i64) -> bool {
    tag.name == "word"
        && tag.collection_group_id == group_id
        && tag.address == "D3010"
        && tag.data_type == "u16"
        && tag.tag_kind == "plc"
        && tag.enabled
        && tag.writable
}

fn read_u16(body: &serde_json::Value) -> u16 {
    assert_eq!(body["q"], "good", "{body:?}");
    let value = body["v"].as_f64().expect("one numeric D3010 value");
    assert!(value.is_finite() && value.fract() == 0.0 && (0.0..=65535.0).contains(&value));
    value as u16
}

#[test]
fn hub_live_fixture_rejects_other_endpoints_and_tags_before_activation() {
    let input = ConnectionSettingsInput {
        name: "r-wasm-test".into(),
        protocol: "slmp-write-fixture".into(),
        enabled: true,
        simulation: false,
        settings: json!({"host": HOST, "port": PORT})
            .as_object()
            .unwrap()
            .clone(),
        secrets: Default::default(),
    };
    assert!(approved_input(&input));
    let saved = PlcConnection::from_settings(
        1,
        input.name.clone(),
        input.protocol.clone(),
        true,
        false,
        input.settings.clone(),
        1,
    );
    assert!(approved_broker_destination(&saved));
    let mut wrong_saved = saved.clone();
    wrong_saved
        .settings
        .insert("host".into(), json!("127.0.0.1"));
    assert!(!approved_broker_destination(&wrong_saved));
    wrong_saved = saved.clone();
    wrong_saved.settings.insert("port".into(), json!(3100));
    assert!(!approved_broker_destination(&wrong_saved));
    wrong_saved = saved;
    wrong_saved.settings.insert("unit_id".into(), json!(1));
    assert!(!approved_broker_destination(&wrong_saved));
    let mut wrong = input.clone();
    wrong.settings.insert("host".into(), json!("127.0.0.1"));
    assert!(!approved_input(&wrong));
    wrong = input.clone();
    wrong.settings.insert("port".into(), json!(3100));
    assert!(!approved_input(&wrong));
    wrong = input.clone();
    wrong
        .settings
        .insert("transport".into(), json!("serial_over_tcp"));
    assert!(!approved_input(&wrong));
    wrong = input;
    wrong.simulation = true;
    assert!(!approved_input(&wrong));

    let mut tag = super::tag_input("word", 1, "D3010", "u16");
    tag.writable = true;
    assert!(approved_tag(&tag, 1));
    for address in ["D3000", "D3001", "D3011"] {
        tag.address = address.into();
        assert!(!approved_tag(&tag, 1));
    }
    tag.address = "D3010".into();
    tag.data_type = "u32".into();
    assert!(!approved_tag(&tag, 1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "real R test PLC; requires parent review and exact Hub opt-in"]
async fn hub_api_wasm_r_d3010_write_readback_restore_once() {
    assert_eq!(
        std::env::var("BANTO_LIVE_WASM_R_D3010_HUB_CONFIRM").as_deref(),
        Ok("192.168.11.200:3109"),
        "explicit Hub live-test opt-in required before a dial"
    );
    let component = std::fs::read(
        std::env::var("BANTO_SLMP_WRITE_COMPONENT_WASM").expect("reviewed component path"),
    )
    .expect("reviewed component bytes");
    assert_eq!(
        Sha256::digest(&component).as_slice(),
        COMPONENT_SHA256.as_slice(),
        "component differs from the reviewed fixed guest build"
    );

    let app = super::test_app_with_controlled_r_test_manifest(
        "slmp-wasm-hub-live-d3010",
        MANIFEST,
        &component,
    )
    .await;
    let input = ConnectionSettingsInput {
        name: "r-wasm-test".into(),
        protocol: "slmp-write-fixture".into(),
        enabled: true,
        simulation: false,
        settings: json!({"host": HOST, "port": PORT})
            .as_object()
            .unwrap()
            .clone(),
        secrets: Default::default(),
    };
    assert!(approved_input(&input));
    let saved = app
        .manager
        .plc_connections()
        .create_settings(input)
        .await
        .unwrap();
    assert!(
        approved_broker_destination(&saved),
        "stored broker endpoint changed"
    );
    let group = super::CollectionGroupService::with_registry(
        app.pool.clone(),
        app.manager.registry().clone(),
    )
    .create(super::group_input("d3010-only", saved.id, 60_000))
    .await
    .unwrap();
    let mut tag = super::tag_input("word", group.id, "D3010", "u16");
    tag.writable = true;
    assert!(approved_tag(&tag, group.id));
    let tag_id = app.manager.tags().create(tag).await.unwrap().id;

    // One exact read/write scope, stored only in TestApp's disposable database.
    // Expiry also limits a token left in an orphaned temp DB after a crash.
    let expires_at = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
        + 10 * 60 * 1000;
    let (status, key_body) = super::admin_write(
        &app.router,
        "POST",
        "/api/api-keys",
        &app.admin_token,
        json!({"name":"r-d3010-fixture","expiresAt":expires_at,"scopes":[
            format!("read:{EXTERNAL_TAG}"), format!("write:{EXTERNAL_TAG}")
        ]}),
    )
    .await;
    assert_eq!(
        status,
        super::StatusCode::CREATED,
        "API key issuance failed"
    );
    let key_id = key_body["id"].as_i64().unwrap();
    let key = key_body["key"].as_str().unwrap();
    let path = format!("/api/v1/values/{EXTERNAL_TAG}");
    let read_path = format!("{path}/read-now");

    audit("broker_connect_start", format!("destination={HOST}:{PORT}"));
    app.manager.rebuild().await.unwrap();
    let mut connection_status = app.sessions.status_watch(saved.id).unwrap();
    tokio::time::timeout(Duration::from_secs(7), async {
        connection_status
            .wait_for(|state| *state == banto_broker::BrokerConnectionStatus::Connected)
            .await
    })
    .await
    .expect("bounded wait for R broker connection")
    .unwrap();
    audit(
        "broker_connected",
        "controlled WASM guest session established",
    );

    let (status, body) = super::admin_write(
        &app.router,
        "POST",
        &path,
        &app.admin_token,
        json!({"v": 1}),
    )
    .await;
    assert_eq!(status, super::StatusCode::FORBIDDEN, "{body:?}");

    // A valid write scope still cannot write while write_control is off.
    let (status, body) = super::admin_write(&app.router, "POST", &path, key, json!({"v": 1})).await;
    assert_eq!(status, super::StatusCode::SERVICE_UNAVAILABLE, "{body:?}");
    assert_eq!(body["error"], "writes_disabled");
    let (status, body) = super::admin_write(
        &app.router,
        "POST",
        "/api/write-control/enable",
        &app.admin_token,
        json!({}),
    )
    .await;
    assert_eq!(status, super::StatusCode::OK, "{body:?}");

    let (status, body) = super::admin_get(&app.router, &read_path, key).await;
    assert_eq!(status, super::StatusCode::OK, "{body:?}");
    let original = read_u16(&body);
    audit("original_read_1", format!("D3010={original}"));
    tokio::time::sleep(Duration::from_secs(1)).await;
    let (status, body) = super::admin_get(&app.router, &read_path, key).await;
    assert_eq!(status, super::StatusCode::OK, "{body:?}");
    let repeated = read_u16(&body);
    audit("original_read_2", format!("D3010={repeated}"));
    assert_eq!(original, repeated, "D3010 changed; no write is permitted");

    let trial = if original == 0 { 1 } else { 0 };
    audit("trial_api_write_start", format!("D3010={trial}"));
    let (status, body) =
        super::admin_write(&app.router, "POST", &path, key, json!({"v": trial})).await;
    assert_eq!(
        status,
        super::StatusCode::OK,
        "write result uncertain: {body:?}"
    );
    audit("trial_api_write_ack", format!("D3010={trial}"));
    let (status, body) = super::admin_get(&app.router, &read_path, key).await;
    assert_eq!(status, super::StatusCode::OK, "{body:?}");
    let observed = read_u16(&body);
    audit("trial_readback", format!("D3010={observed}"));
    assert_eq!(observed, trial, "test value not confirmed; do not restore");

    audit("restore_api_write_start", format!("D3010={original}"));
    let (status, body) =
        super::admin_write(&app.router, "POST", &path, key, json!({"v": original})).await;
    assert_eq!(
        status,
        super::StatusCode::OK,
        "restoration result uncertain: {body:?}"
    );
    audit("restore_api_write_ack", format!("D3010={original}"));
    let (status, body) = super::admin_get(&app.router, &read_path, key).await;
    assert_eq!(status, super::StatusCode::OK, "{body:?}");
    let restored = read_u16(&body);
    audit("restore_readback", format!("D3010={restored}"));
    assert_eq!(restored, original);

    let (status, rows) = super::admin_write(
        &app.router,
        "POST",
        "/api/write-audit/list",
        &app.admin_token,
        json!({}),
    )
    .await;
    assert_eq!(status, super::StatusCode::OK, "{rows:?}");
    assert_eq!(
        rows["rows"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["tagId"].as_i64() == Some(tag_id) && row["result"] == "ok")
            .count(),
        2,
        "both successful writes must be audited"
    );
    let (status, body) = super::admin_write(
        &app.router,
        "POST",
        "/api/write-control/disable",
        &app.admin_token,
        json!({}),
    )
    .await;
    assert_eq!(status, super::StatusCode::OK, "{body:?}");
    let (status, body) = super::admin_write(
        &app.router,
        "POST",
        &format!("/api/api-keys/{key_id}/revoke"),
        &app.admin_token,
        json!({}),
    )
    .await;
    assert_eq!(status, super::StatusCode::OK, "{body:?}");
    drop(app);
    audit(
        "test_app_closed",
        "temporary broker, router and database dropped",
    );
}
