//! Loopback-only Hub join for the pinned Modbus write guest. No PLC is used.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use banto_driver_api::{DataType, TagKind, TagSpec, Value, WriteItem};
use banto_hub_core::api_keys::ApiKeysService;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// A request PDU (function code onwards) and its connection number.
type Request = (usize, Vec<u8>);

struct Fixture {
    port: u16,
    frames: Arc<Mutex<Vec<(u8, u16, u16)>>>,
    task: tokio::task::JoinHandle<()>,
    register_bits: Arc<AtomicBool>,
    /// Every request PDU (function code onwards) with the number of the TCP
    /// connection it arrived on, counted from 1 in accept order.
    requests: Arc<Mutex<Vec<Request>>>,
    accepted: Arc<AtomicUsize>,
    /// FC22 answer: [FC22_ECHO], [FC22_EXCEPTION] or [FC22_BAD_MASK].
    fc22: Arc<AtomicU8>,
}

const FC22_ECHO: u8 = 0;
/// A device without FC22: exception code 1 (illegal function).
const FC22_EXCEPTION: u8 = 1;
/// An acknowledgement whose OR mask differs from the request.
const FC22_BAD_MASK: u8 = 2;

impl Fixture {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let frames = Arc::new(Mutex::new(Vec::new()));
        let recorded = frames.clone();
        let register_bits = Arc::new(AtomicBool::new(false));
        let bit_state = register_bits.clone();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let accepted = Arc::new(AtomicUsize::new(0));
        let fc22 = Arc::new(AtomicU8::new(FC22_ECHO));
        let (all_requests, accept_count, fc22_mode) =
            (requests.clone(), accepted.clone(), fc22.clone());
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let connection = accept_count.fetch_add(1, Ordering::SeqCst) + 1;
                let frames = recorded.clone();
                let bit_state = bit_state.clone();
                let requests = all_requests.clone();
                let fc22_mode = fc22_mode.clone();
                tokio::spawn(async move {
                    loop {
                        let mut mbap = [0u8; 6];
                        if socket.read_exact(&mut mbap).await.is_err() {
                            break;
                        }
                        let len = usize::from(u16::from_be_bytes([mbap[4], mbap[5]]));
                        if !(2..=260).contains(&len) {
                            break;
                        }
                        let mut payload = vec![0; len];
                        if socket.read_exact(&mut payload).await.is_err() {
                            break;
                        }
                        let unit = payload[0];
                        let function = payload[1];
                        let start = if payload.len() >= 4 {
                            u16::from_be_bytes([payload[2], payload[3]])
                        } else {
                            0
                        };
                        let count = if payload.len() >= 6 {
                            u16::from_be_bytes([payload[4], payload[5]])
                        } else {
                            0
                        };
                        frames.lock().unwrap().push((function, start, count));
                        requests
                            .lock()
                            .unwrap()
                            .push((connection, payload[1..].to_vec()));
                        let pdu = match function {
                            1 | 2 if count == 1 => vec![function, 1, u8::from(function == 2)],
                            3 | 4 if (1..=4).contains(&count) => {
                                let mut pdu = vec![function, (count * 2) as u8];
                                for offset in start..start + count {
                                    pdu.extend_from_slice(
                                        &(if bit_state.load(Ordering::SeqCst) {
                                            0x8001_u16
                                        } else if function == 4 {
                                            42
                                        } else {
                                            offset
                                        })
                                        .to_be_bytes(),
                                    );
                                }
                                pdu
                            }
                            5 if payload.len() == 6 => payload[1..].to_vec(),
                            16 if payload.len() >= 7 && (1..=123).contains(&count) => {
                                payload[1..6].to_vec()
                            }
                            22 if payload.len() == 8 => match fc22_mode.load(Ordering::SeqCst) {
                                FC22_ECHO => payload[1..].to_vec(),
                                FC22_EXCEPTION => vec![0x96, 1],
                                _ => {
                                    let mut echo = payload[1..].to_vec();
                                    echo[5] = !echo[5];
                                    echo[6] = !echo[6];
                                    echo
                                }
                            },
                            _ => vec![function | 0x80, 1],
                        };
                        let mut response = mbap[..4].to_vec();
                        response.extend_from_slice(
                            &u16::try_from(1 + pdu.len()).unwrap().to_be_bytes(),
                        );
                        response.push(unit);
                        response.extend_from_slice(&pdu);
                        if socket.write_all(&response).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        Self {
            port,
            frames,
            task,
            register_bits,
            requests,
            accepted,
            fc22,
        }
    }

    fn frames(&self) -> Vec<(u8, u16, u16)> {
        self.frames.lock().unwrap().clone()
    }

    /// Write PDUs (FC5, FC16, FC22) in arrival order with their connection.
    fn writes(&self) -> Vec<(usize, Vec<u8>)> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, pdu)| matches!(pdu[0], 5 | 16 | 22))
            .cloned()
            .collect()
    }

    fn requests_on(&self, connection: usize) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(on, _)| *on == connection)
            .count()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn component() -> Vec<u8> {
    std::fs::read(
        std::env::var("BANTO_MODBUS_COMPONENT_WASM").expect("BANTO_MODBUS_COMPONENT_WASM"),
    )
    .unwrap()
}

fn manifest() -> Vec<u8> {
    std::fs::read(std::env::var("BANTO_MODBUS_MANIFEST").expect("BANTO_MODBUS_MANIFEST")).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the pinned Modbus WASM package files"]
async fn saved_settings_broker_and_rest_enforce_modbus_policy_and_audit() {
    let fixture = Fixture::start().await;
    let wasm = component();
    let manifest = manifest();
    let app =
        super::test_app_with_operational_write_manifest("t12-modbus-join", &manifest, &wasm).await;
    let saved = app
        .manager
        .plc_connections()
        .create_settings(super::ConnectionSettingsInput {
            name: "modbus-line".into(),
            protocol: "modbus-fc3-wasm".into(),
            enabled: true,
            simulation: false,
            settings: serde_json::json!({
                "host": "127.0.0.1", "port": fixture.port, "unit_id": 1,
                "probe_address": "30001", "word_order": "low_high",
                "write_mode": "enabled", "write_allowlist": "000001-000016,400001-400020",
                "modbus_operational_write_mode": "enabled",
                "modbus_operational_write_allowlist": "000001-000016,400001-400010"
            })
            .as_object()
            .unwrap()
            .clone(),
            secrets: Default::default(),
        })
        .await
        .expect("save guest and host policy");
    let group = super::CollectionGroupService::with_registry(
        app.pool.clone(),
        app.manager.registry().clone(),
    )
    .create(super::group_input("line", saved.id, 60_000))
    .await
    .unwrap();
    let mut records = Vec::new();
    for (name, address, kind, writable) in [
        ("coil", "000001", "bit", true),
        ("holding", "400001", "u32", true),
        ("outside", "400011", "u16", true),
        ("discrete", "100001", "bit", false),
        ("input", "300001", "u16", false),
    ] {
        let mut input = super::tag_input(name, group.id, address, kind);
        input.writable = writable;
        let tag = app.manager.tags().create(input).await.unwrap();
        records.push((name, tag.id));
    }
    app.manager.rebuild().await.unwrap();
    let mut status = app.sessions.status_watch(saved.id).unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        status.wait_for(|state| *state == banto_broker::BrokerConnectionStatus::Connected),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        fixture.frames().first().map(|frame| frame.0),
        Some(4),
        "connection probe must use FC4"
    );

    let driver = app.manager.registry().get("modbus-fc3-wasm").unwrap();
    let specs =
        [("100001", DataType::Bit), ("300001", DataType::U16)].map(|(address, data_type)| {
            let kind = TagKind::numeric(data_type);
            TagSpec::new(driver.base().parse_address(address, &kind).unwrap(), kind)
        });
    let handle = app.manager.write_broker_handle_peek(saved.id).unwrap();
    assert_eq!(
        handle.read(specs.to_vec()).await.unwrap(),
        vec![Ok(Value::Bit(true)), Ok(Value::F64(42.0))]
    );
    let before_denied = fixture.frames().len();
    assert!(handle
        .write(vec![WriteItem::new(specs[0].clone(), Value::Bit(true))])
        .await
        .unwrap()
        .iter()
        .all(Result::is_err));
    assert_eq!(
        fixture.frames().len(),
        before_denied,
        "FC2 is refused before send"
    );
    assert!(handle
        .write(vec![WriteItem::new(specs[1].clone(), Value::F64(1.0))])
        .await
        .unwrap()
        .iter()
        .all(Result::is_err));
    assert_eq!(
        fixture.frames().len(),
        before_denied,
        "FC4 is refused before send"
    );

    app.write_control.enable();
    let coil_path = "/api/v1/values/modbus-line.line.coil";
    let unauthorized = super::admin_write(
        &app.router,
        "POST",
        coil_path,
        &app.admin_token,
        serde_json::json!({"v": true}),
    )
    .await;
    assert_eq!(
        unauthorized.0,
        super::StatusCode::FORBIDDEN,
        "{:?}",
        unauthorized.1
    );
    assert_eq!(fixture.frames().len(), before_denied);

    let names: Vec<String> = records
        .iter()
        .map(|(name, _)| format!("modbus-line.line.{name}"))
        .collect();
    let now_ms = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let keys = ApiKeysService::new(app.pool.clone());
    let issued = keys
        .issue(
            "modbus-loopback-writer",
            names.iter().map(|name| format!("write:{name}")).collect(),
            Some(now_ms + 600_000),
        )
        .await
        .unwrap();
    for (name, value) in [
        ("coil", serde_json::json!({"v": true})),
        ("holding", serde_json::json!({"v": 305419896})),
    ] {
        let result = super::admin_write(
            &app.router,
            "POST",
            &format!("/api/v1/values/modbus-line.line.{name}"),
            &issued.key,
            value,
        )
        .await;
        assert_eq!(result.0, super::StatusCode::OK, "{name}: {:?}", result.1);
    }
    let after_allowed = fixture.frames();
    assert_eq!(after_allowed.iter().filter(|frame| frame.0 == 5).count(), 1);
    assert_eq!(
        after_allowed.iter().filter(|frame| frame.0 == 16).count(),
        1
    );
    for name in ["outside", "discrete", "input"] {
        let result = super::admin_write(
            &app.router,
            "POST",
            &format!("/api/v1/values/modbus-line.line.{name}"),
            &issued.key,
            serde_json::json!({"v": 1}),
        )
        .await;
        assert_ne!(result.0, super::StatusCode::OK, "{name}: {:?}", result.1);
    }
    assert_eq!(
        fixture
            .frames()
            .iter()
            .filter(|frame| matches!(frame.0, 5 | 16))
            .count(),
        2
    );
    let audit = super::admin_write(
        &app.router,
        "POST",
        "/api/write-audit/list",
        &app.admin_token,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(audit.0, super::StatusCode::OK, "{:?}", audit.1);
    let rows = audit.1["rows"].as_array().unwrap();
    for (_, id) in &records[..2] {
        assert!(rows
            .iter()
            .any(|row| row["tagId"].as_i64() == Some(*id) && row["result"] == "ok"));
    }
    assert!(rows
        .iter()
        .any(|row| row["tagId"].as_i64() == Some(records[2].1) && row["result"] == "failed"));
    assert!(keys.revoke(issued.id).await.unwrap().revoked_at.is_some());
    app.write_control.disable();
    drop(app);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the pinned Modbus WASM package files"]
async fn register_bits_reach_broker_and_read_now_as_booleans() {
    let fixture = Fixture::start().await;
    let app = super::test_app_with_operational_write_manifest(
        "t12-modbus-register-bits",
        &manifest(),
        &component(),
    )
    .await;
    let saved = app
        .manager
        .plc_connections()
        .create_settings(super::ConnectionSettingsInput {
            name: "bits-line".into(),
            protocol: "modbus-fc3-wasm".into(),
            enabled: true,
            simulation: false,
            settings: serde_json::json!({
                "host": "127.0.0.1", "port": fixture.port, "unit_id": 1,
                "probe_address": "30001", "write_mode": "enabled",
                "write_allowlist": "400001-400010",
                "modbus_operational_write_mode": "enabled",
                "modbus_operational_write_allowlist": "400001-400010"
            })
            .as_object()
            .unwrap()
            .clone(),
            secrets: Default::default(),
        })
        .await
        .unwrap();
    let group = super::CollectionGroupService::with_registry(
        app.pool.clone(),
        app.manager.registry().clone(),
    )
    .create(super::group_input("line", saved.id, 60_000))
    .await
    .unwrap();
    for (name, address) in [("holding_bit", "40001.0"), ("input_bit", "30001.15")] {
        app.manager
            .tags()
            .create(super::tag_input(name, group.id, address, "bit"))
            .await
            .unwrap();
    }
    app.manager.rebuild().await.unwrap();
    let mut status = app.sessions.status_watch(saved.id).unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        status.wait_for(|state| *state == banto_broker::BrokerConnectionStatus::Connected),
    )
    .await
    .unwrap()
    .unwrap();
    let driver = app.manager.registry().get("modbus-fc3-wasm").unwrap();
    let native = app.manager.registry().get("modbus-tcp").unwrap();
    let specs = [("40001.0", "400001.0", 0), ("30001.15", "300001.15", 15)].map(
        |(address, canonical, bit)| {
            let kind = TagKind::numeric(DataType::Bit);
            let parsed = driver.base().parse_address(address, &kind).unwrap();
            let native_parsed = native.base().parse_address(address, &kind).unwrap();
            assert_eq!(parsed.canonical, canonical);
            assert_eq!(parsed.canonical, native_parsed.canonical);
            assert_eq!(parsed.bit, Some(bit));
            assert_eq!(parsed.bit, native_parsed.bit);
            TagSpec::new(parsed, kind)
        },
    );
    let handle = app.manager.write_broker_handle_peek(saved.id).unwrap();
    let names = ["holding_bit", "input_bit"];
    let issued = ApiKeysService::new(app.pool.clone())
        .issue(
            "register-bit-reader",
            names
                .iter()
                .map(|name| format!("read:bits-line.line.{name}"))
                .collect(),
            None,
        )
        .await
        .unwrap();
    let before_reads = fixture.frames().len();
    for expected in [false, true] {
        fixture.register_bits.store(expected, Ordering::SeqCst);
        assert_eq!(
            handle.read(specs.to_vec()).await.unwrap(),
            vec![Ok(Value::Bit(expected)), Ok(Value::Bit(expected))]
        );
        for name in names {
            let (status, body) = super::admin_get(
                &app.router,
                &format!("/api/v1/values/bits-line.line.{name}/read-now"),
                &issued.key,
            )
            .await;
            assert_eq!(status, super::StatusCode::OK, "{name}: {body:?}");
            assert_eq!(body["v"], serde_json::json!(expected), "{name}: {body:?}");
        }
    }
    let frames = fixture.frames();
    for function in [3, 4] {
        assert!(
            frames[before_reads..].contains(&(function, 0, 1)),
            "missing FC{function} register-bit read: {frames:?}"
        );
    }
    // Until #44 (the 02a92645 pin) both writes were refused before send. A
    // holding bit is now one FC22 mask write; an input-register bit is still
    // refused before send.
    let before = fixture.writes().len();
    let [holding, input] = specs;
    assert_eq!(
        handle
            .write(vec![WriteItem::new(holding, Value::Bit(true))])
            .await
            .unwrap(),
        vec![Ok(())]
    );
    assert!(handle
        .write(vec![WriteItem::new(input, Value::Bit(true))])
        .await
        .unwrap()
        .iter()
        .all(Result::is_err));
    let writes: Vec<Vec<u8>> = fixture.writes()[before..]
        .iter()
        .map(|(_, pdu)| pdu.clone())
        .collect();
    assert_eq!(
        writes,
        vec![vec![22, 0, 0, 0xff, 0xfe, 0, 1]],
        "only the holding bit reaches the device, as one FC22"
    );
}

/// One saved Modbus WASM connection with one collection group "line" and
/// writable tags, connected to `fixture`.
async fn save_line(
    app: &super::TestApp,
    fixture: &Fixture,
    name: &str,
    guest: (&str, &str),
    host: (&str, &str),
    tags: &[(&str, &str, &str)],
) -> (i64, Vec<i64>) {
    save_line_with(app, fixture, name, guest, host, &[], tags).await
}

/// [save_line] with extra guest settings (for example `register_bit_write`).
async fn save_line_with(
    app: &super::TestApp,
    fixture: &Fixture,
    name: &str,
    guest: (&str, &str),
    host: (&str, &str),
    extra: &[(&str, &str)],
    tags: &[(&str, &str, &str)],
) -> (i64, Vec<i64>) {
    let mut settings = serde_json::json!({
        "host": "127.0.0.1", "port": fixture.port, "unit_id": 1,
        "probe_address": "30001",
        "write_mode": guest.0, "write_allowlist": guest.1,
        "modbus_operational_write_mode": host.0,
        "modbus_operational_write_allowlist": host.1
    })
    .as_object()
    .unwrap()
    .clone();
    for (key, value) in extra {
        settings.insert((*key).into(), (*value).into());
    }
    let saved = app
        .manager
        .plc_connections()
        .create_settings(super::ConnectionSettingsInput {
            name: name.into(),
            protocol: "modbus-fc3-wasm".into(),
            enabled: true,
            simulation: false,
            settings,
            secrets: Default::default(),
        })
        .await
        .expect("save guest and host policy");
    let group = super::CollectionGroupService::with_registry(
        app.pool.clone(),
        app.manager.registry().clone(),
    )
    .create(super::group_input("line", saved.id, 60_000))
    .await
    .unwrap();
    let mut ids = Vec::new();
    for (tag, address, kind) in tags {
        let mut input = super::tag_input(tag, group.id, address, kind);
        input.writable = true;
        ids.push(app.manager.tags().create(input).await.unwrap().id);
    }
    (saved.id, ids)
}

async fn wait_connected(app: &super::TestApp, connection: i64) {
    let mut status = app.sessions.status_watch(connection).unwrap();
    tokio::time::timeout(
        Duration::from_secs(30),
        status.wait_for(|state| *state == banto_broker::BrokerConnectionStatus::Connected),
    )
    .await
    .unwrap()
    .unwrap();
}

async fn write_key(
    app: &super::TestApp,
    label: &str,
    tags: &[&str],
) -> (ApiKeysService, i64, String) {
    let keys = ApiKeysService::new(app.pool.clone());
    let issued = keys
        .issue(
            label,
            tags.iter().map(|tag| format!("write:{tag}")).collect(),
            None,
        )
        .await
        .unwrap();
    (keys, issued.id, issued.key)
}

async fn post(
    app: &super::TestApp,
    key: &str,
    tag: &str,
    value: bool,
) -> (super::StatusCode, serde_json::Value) {
    super::admin_write(
        &app.router,
        "POST",
        &format!("/api/v1/values/{tag}"),
        key,
        serde_json::json!({ "v": value }),
    )
    .await
}

async fn audit_rows(app: &super::TestApp, tag_id: i64) -> Vec<serde_json::Value> {
    let audit = super::admin_write(
        &app.router,
        "POST",
        "/api/write-audit/list",
        &app.admin_token,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(audit.0, super::StatusCode::OK, "{:?}", audit.1);
    let mut rows: Vec<serde_json::Value> = audit.1["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["tagId"].as_i64() == Some(tag_id))
        .cloned()
        .collect();
    rows.sort_by_key(|row| row["id"].as_i64());
    rows
}

fn fc22_pdus(fixture: &Fixture) -> Vec<Vec<u8>> {
    fixture
        .writes()
        .into_iter()
        .filter(|(_, pdu)| pdu[0] == 22)
        .map(|(_, pdu)| pdu)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the pinned Modbus WASM package files"]
async fn fc22_set_and_clear_pass_all_three_gates_and_are_audited() {
    let fixture = Fixture::start().await;
    let app = super::test_app_with_operational_write_manifest(
        "t12-modbus-fc22",
        &manifest(),
        &component(),
    )
    .await;
    let (connection, ids) = save_line(
        &app,
        &fixture,
        "fc22-line",
        ("enabled", "400001-400010"),
        ("enabled", "400001-400010"),
        &[("hbit", "40002.3", "bit")],
    )
    .await;
    app.manager.rebuild().await.unwrap();
    wait_connected(&app, connection).await;
    let tag = "fc22-line.line.hbit";
    let (keys, key_id, key) = write_key(&app, "fc22-writer", &[tag]).await;

    // Operational gate off: suppressed before the broker.
    let suppressed = post(&app, &key, tag, true).await;
    assert_eq!(
        suppressed.0,
        super::StatusCode::SERVICE_UNAVAILABLE,
        "{:?}",
        suppressed.1
    );
    app.write_control.enable();
    // Permission: a token without the tag's write scope.
    let forbidden = post(&app, &app.admin_token, tag, true).await;
    assert_eq!(
        forbidden.0,
        super::StatusCode::FORBIDDEN,
        "{:?}",
        forbidden.1
    );
    assert!(fixture.writes().is_empty(), "{:?}", fixture.writes());

    // All three gates pass: set, then clear, bit 3 of 400002 (offset 1).
    for value in [true, false] {
        let result = post(&app, &key, tag, value).await;
        assert_eq!(result.0, super::StatusCode::OK, "{value}: {:?}", result.1);
    }
    assert_eq!(
        fc22_pdus(&fixture),
        vec![
            vec![22, 0, 1, 0xff, 0xf7, 0x00, 0x08],
            vec![22, 0, 1, 0xff, 0xf7, 0x00, 0x00],
        ],
        "FC22 address 1, AND mask 0xFFF7, OR mask 0x0008 (set) / 0x0000 (clear)"
    );
    assert_eq!(fixture.writes().len(), 2, "no other write frame");

    let rows = audit_rows(&app, ids[0]).await;
    let results: Vec<&str> = rows
        .iter()
        .map(|row| row["result"].as_str().unwrap())
        .collect();
    assert_eq!(results, ["suppressed_disabled", "ok", "ok"], "{rows:?}");
    for (row, value) in rows[1..].iter().zip([1.0, 0.0]) {
        assert_eq!(row["action"], "write", "{row:?}");
        assert_eq!(row["valueRequested"].as_f64(), Some(value), "{row:?}");
        assert_eq!(row["apiKeyId"].as_i64(), Some(key_id), "{row:?}");
        assert_eq!(row["externalNameSnapshot"], tag, "{row:?}");
        assert_eq!(row["detail"], r#"{"target":"plc"}"#, "{row:?}");
    }
    assert!(keys.revoke(key_id).await.unwrap().revoked_at.is_some());
    app.write_control.disable();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the pinned Modbus WASM package files"]
async fn register_bit_writes_refused_by_either_gate_never_reach_the_device() {
    let fixture = Fixture::start().await;
    let app = super::test_app_with_operational_write_manifest(
        "t12-modbus-fc22-refusals",
        &manifest(),
        &component(),
    )
    .await;
    // Hub allows 400001-400010, guest allows only 400001-400005.
    let (narrow, narrow_ids) = save_line(
        &app,
        &fixture,
        "narrow-line",
        ("enabled", "400001-400005"),
        ("enabled", "400001-400010"),
        &[
            ("guest_outside", "40008.0", "bit"),
            ("inside", "40002.3", "bit"),
        ],
    )
    .await;
    // Hub allows the word, guest write_mode disabled.
    let (guest_off, guest_off_ids) = save_line(
        &app,
        &fixture,
        "guest-off-line",
        ("disabled", ""),
        ("enabled", "400001-400010"),
        &[("hbit", "40002.3", "bit")],
    )
    .await;
    // Guest allows the word, Hub saved policy disabled.
    let (host_off, host_off_ids) = save_line(
        &app,
        &fixture,
        "host-off-line",
        ("enabled", "400001-400010"),
        ("disabled", ""),
        &[("hbit", "40002.3", "bit")],
    )
    .await;
    app.manager.rebuild().await.unwrap();
    for connection in [narrow, guest_off, host_off] {
        wait_connected(&app, connection).await;
    }
    app.write_control.enable();
    let tags = [
        "narrow-line.line.guest_outside",
        "guest-off-line.line.hbit",
        "host-off-line.line.hbit",
    ];
    let (keys, key_id, key) = write_key(&app, "fc22-refusals", &tags).await;

    // Through REST with every Hub-side gate open except the one named.
    for (tag, id, refusal) in [
        // Hub allows + guest denies (outside the guest list / write_mode).
        (tags[0], narrow_ids[0], "ドライバが断りました"),
        (tags[1], guest_off_ids[0], "ドライバが断りました"),
        // Hub saved policy denies + guest allows.
        (tags[2], host_off_ids[0], "扱えない組み合わせです"),
    ] {
        let result = post(&app, &key, tag, true).await;
        assert_eq!(
            result.0,
            super::StatusCode::BAD_GATEWAY,
            "{tag}: {:?}",
            result.1
        );
        assert_eq!(result.1["error"], "write_failed", "{tag}: {:?}", result.1);
        let rows = audit_rows(&app, id).await;
        assert_eq!(rows.len(), 1, "{tag}: {rows:?}");
        assert_eq!(rows[0]["result"], "failed", "{tag}: {rows:?}");
        assert!(
            rows[0]["detail"].as_str().unwrap().contains(refusal),
            "{tag}: {rows:?}"
        );
    }
    // Hub permission denies (the key has no write scope for this tag) while
    // the saved policy and the guest both allow the word.
    let forbidden = post(&app, &key, "narrow-line.line.inside", true).await;
    assert_eq!(
        forbidden.0,
        super::StatusCode::FORBIDDEN,
        "{:?}",
        forbidden.1
    );

    // The remaining refusals through the broker handle of the line whose
    // gates both allow 400001-400005: input-register bit, mixed with a
    // numeric item, two bits in one request, a non-Bool value.
    let driver = app.manager.registry().get("modbus-fc3-wasm").unwrap();
    let spec = |address: &str, data_type: DataType| {
        let kind = TagKind::numeric(data_type);
        TagSpec::new(driver.base().parse_address(address, &kind).unwrap(), kind)
    };
    let handle = app.manager.write_broker_handle_peek(narrow).unwrap();
    for items in [
        vec![WriteItem::new(
            spec("30001.15", DataType::Bit),
            Value::Bit(true),
        )],
        vec![
            WriteItem::new(spec("40001.0", DataType::Bit), Value::Bit(true)),
            WriteItem::new(spec("40002", DataType::U16), Value::F64(1.0)),
        ],
        vec![
            WriteItem::new(spec("40001.0", DataType::Bit), Value::Bit(true)),
            WriteItem::new(spec("40001.1", DataType::Bit), Value::Bit(true)),
        ],
        vec![WriteItem::new(
            spec("40001.0", DataType::Bit),
            Value::F64(1.0),
        )],
    ] {
        let results = handle.write(items.clone()).await.unwrap();
        assert_eq!(results.len(), items.len());
        assert!(results.iter().all(Result::is_err), "{items:?}: {results:?}");
    }
    assert!(
        fixture.writes().is_empty(),
        "no write frame may reach the device: {:?}",
        fixture.writes()
    );
    assert!(keys.revoke(key_id).await.unwrap().revoked_at.is_some());
    app.write_control.disable();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the pinned Modbus WASM package files"]
async fn fc22_exception_is_per_item_and_a_mask_mismatch_is_uncertain_without_replay() {
    let fixture = Fixture::start().await;
    let app = super::test_app_with_operational_write_manifest(
        "t12-modbus-fc22-errors",
        &manifest(),
        &component(),
    )
    .await;
    let (connection, ids) = save_line(
        &app,
        &fixture,
        "err-line",
        ("enabled", "400001-400010"),
        ("enabled", "400001-400010"),
        &[("hbit", "40002.3", "bit"), ("hword", "40003", "u16")],
    )
    .await;
    app.manager.rebuild().await.unwrap();
    wait_connected(&app, connection).await;
    app.write_control.enable();
    let (bit, word) = ("err-line.line.hbit", "err-line.line.hword");
    let (keys, key_id, key) = write_key(&app, "fc22-errors", &[bit, word]).await;

    // A device without FC22 answers exception code 1: the item fails, the
    // connection and other items carry on.
    fixture.fc22.store(FC22_EXCEPTION, Ordering::SeqCst);
    let first_connection = fixture.accepted.load(Ordering::SeqCst);
    let result = post(&app, &key, bit, true).await;
    assert_eq!(result.0, super::StatusCode::BAD_GATEWAY, "{:?}", result.1);
    assert_eq!(result.1["error"], "write_failed", "{:?}", result.1);
    let detail = result.1["detail"].as_str().unwrap();
    assert!(
        detail.contains("コード=0x0001") && detail.contains("Modbus exception 0x01"),
        "{detail}"
    );
    let rows = audit_rows(&app, ids[0]).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["result"], "failed", "{rows:?}");
    let audited: serde_json::Value =
        serde_json::from_str(rows[0]["detail"].as_str().unwrap()).unwrap();
    assert_eq!(audited["target"], "plc", "{audited:?}");
    assert_eq!(audited["detail"], detail, "{audited:?}");
    let word_write = super::admin_write(
        &app.router,
        "POST",
        &format!("/api/v1/values/{word}"),
        &key,
        serde_json::json!({ "v": 7 }),
    )
    .await;
    assert_eq!(word_write.0, super::StatusCode::OK, "{:?}", word_write.1);
    assert_eq!(
        fixture.accepted.load(Ordering::SeqCst),
        first_connection,
        "an exception keeps the connection"
    );
    assert_eq!(
        fixture
            .writes()
            .iter()
            .map(|(on, pdu)| (*on, pdu[0]))
            .collect::<Vec<_>>(),
        [(first_connection, 22), (first_connection, 16)]
    );

    // An acknowledgement with a different OR mask leaves the outcome unknown:
    // the write fails, the connection is dropped, and the request is not
    // replayed on that or the next connection. The audit keeps it apart from
    // the device's exception above (#898): `uncertain`, not `failed`.
    fixture.fc22.store(FC22_BAD_MASK, Ordering::SeqCst);
    let result = post(&app, &key, bit, false).await;
    assert_eq!(result.0, super::StatusCode::BAD_GATEWAY, "{:?}", result.1);
    assert_eq!(result.1["error"], "write_failed", "{:?}", result.1);
    let detail = result.1["detail"].as_str().unwrap();
    assert!(
        detail.contains("write acknowledgement mismatch"),
        "{detail}"
    );
    let rows = audit_rows(&app, ids[0]).await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[1]["result"], "uncertain", "{rows:?}");
    assert_eq!(rows[1]["valueRequested"].as_f64(), Some(0.0), "{rows:?}");
    let audited: serde_json::Value =
        serde_json::from_str(rows[1]["detail"].as_str().unwrap()).unwrap();
    assert_eq!(audited["detail"], detail, "{audited:?}");
    let after_mismatch = fixture.requests_on(first_connection);
    tokio::time::timeout(Duration::from_secs(30), async {
        while fixture.accepted.load(Ordering::SeqCst) == first_connection {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the connection is re-established after the fatal ACK");
    wait_connected(&app, connection).await;
    fixture.fc22.store(FC22_ECHO, Ordering::SeqCst);
    let result = post(&app, &key, bit, true).await;
    assert_eq!(result.0, super::StatusCode::OK, "{:?}", result.1);
    assert_eq!(
        fixture.requests_on(first_connection),
        after_mismatch,
        "nothing more is sent on the connection that saw the mismatch"
    );
    let fc22 = fixture
        .writes()
        .into_iter()
        .filter(|(_, pdu)| pdu[0] == 22)
        .collect::<Vec<_>>();
    assert_eq!(
        fc22.len(),
        3,
        "the mismatched write is not replayed: {fc22:?}"
    );
    assert_eq!(
        fc22[1],
        (first_connection, vec![22, 0, 1, 0xff, 0xf7, 0, 0])
    );
    assert!(fc22[2].0 > first_connection, "{fc22:?}");
    assert_eq!(fc22[2].1, vec![22, 0, 1, 0xff, 0xf7, 0, 8]);
    assert!(keys.revoke(key_id).await.unwrap().revoked_at.is_some());
    app.write_control.disable();
}

/// A coil-only or discrete-input-only device passes the Hub connection test
/// through its one `0xxxx` / `1xxxx` probe point: the handshake reads exactly
/// one bit with FC1 / FC2 and never reaches for a holding register (FC3).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the pinned Modbus WASM package files"]
async fn coil_and_discrete_input_probe_pass_the_connection_test_with_one_bit_read() {
    for (probe, function) in [("00001", 1_u8), ("10001", 2_u8)] {
        let fixture = Fixture::start().await;
        let app = super::test_app_with_operational_write_manifest(
            &format!("t12-modbus-probe-{probe}"),
            &manifest(),
            &component(),
        )
        .await;
        let (status, body) = super::admin_write(
            &app.router,
            "POST",
            "/api/plc-connections/test",
            &app.admin_token,
            serde_json::json!({
                "protocol": "modbus-fc3-wasm", "simulation": false,
                "settings": {
                    "host": "127.0.0.1", "port": fixture.port, "unit_id": 1,
                    "probe_address": probe
                }
            }),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{probe}: {body:?}");
        assert_eq!(body["ok"], true, "{probe}: {body:?}");
        let frames = fixture.frames();
        assert_eq!(
            frames.len(),
            1,
            "{probe}: exactly one bit read expected, got {frames:?}"
        );
        let (code, _, count) = frames[0];
        assert_eq!((code, count), (function, 1), "{probe}: {frames:?}");
        assert_eq!(fixture.accepted.load(Ordering::SeqCst), 1, "{probe}");
    }
}

/// A connection declared without FC22 refuses `4xxxx.N` writes before send:
/// nothing reaches the device, the link stays up and the refusal is audited.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the pinned Modbus WASM package files"]
async fn register_bit_write_disabled_refuses_rest_write_before_send() {
    let fixture = Fixture::start().await;
    let app = super::test_app_with_operational_write_manifest(
        "t12-modbus-no-fc22",
        &manifest(),
        &component(),
    )
    .await;
    let (connection, ids) = save_line_with(
        &app,
        &fixture,
        "no-fc22-line",
        ("enabled", "400001-400010"),
        ("enabled", "400001-400010"),
        &[("register_bit_write", "disabled")],
        &[("hbit", "40002.3", "bit")],
    )
    .await;
    app.manager.rebuild().await.unwrap();
    wait_connected(&app, connection).await;
    app.write_control.enable();
    let tag = "no-fc22-line.line.hbit";
    let (keys, key_id, key) = write_key(&app, "no-fc22", &[tag]).await;

    for value in [true, false] {
        let result = post(&app, &key, tag, value).await;
        assert_eq!(
            result.0,
            super::StatusCode::BAD_GATEWAY,
            "{value}: {:?}",
            result.1
        );
        assert_eq!(result.1["error"], "write_failed", "{:?}", result.1);
    }
    let rows = audit_rows(&app, ids[0]).await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(rows.iter().all(|row| row["result"] == "failed"), "{rows:?}");
    assert!(
        fixture.writes().is_empty(),
        "no write frame may reach the device: {:?}",
        fixture.writes()
    );
    assert_eq!(
        fixture.accepted.load(Ordering::SeqCst),
        1,
        "a refusal must not drop or reopen the TCP connection"
    );
    assert_eq!(
        *app.sessions.status_watch(connection).unwrap().borrow(),
        banto_broker::BrokerConnectionStatus::Connected
    );
    assert!(keys.revoke(key_id).await.unwrap().revoked_at.is_some());
    app.write_control.disable();
}
