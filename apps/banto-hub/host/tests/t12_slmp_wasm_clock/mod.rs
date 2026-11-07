//! H2（tyaro/banto-industrial#1053 の H の後半）: PLC の時計合わせの Hub 模擬受入れ。
//!
//! `BANTO_SLMP_NEW_COMPONENT_WASM`（固定 `ea4d84c`、banto-driver-wasm #76）を運用書込みの
//! loader で読み込んだ Hub の実際の経路（保存済みの接続・収集の broker セッション・
//! REST・`crate::clock_sync` のスケジューラ・監査）を、std のスレッドで動く模擬の
//! SLMP 機器に対して通す。模擬機器の応答と期待するバイト列は H1 の component 試験
//! `crates/banto-driver-wasm/tests/slmp_h_clock.rs`（ゲストの
//! `slmp-runtime/tests/clock.rs` と同じ）から写した。実機は使わない。
//!
//! 確かめること:
//! 1. 定期: Hub の `enabled` が偽のあいだは時刻表に合う分でも出さない。真にすると
//!    合う分で 1 回だけ（同じ分の再評価・巻き戻しでも 2 回目は無い）4 要求を送り、
//!    SD210–SD216 に渡した現地時刻が入る。成功が監査（`origin = "schedule"`）と GET に出る。
//! 2. 手動: `POST` で成功（監査は `origin = "rest"`・操作者）。
//! 3. ゲート: ゲストの `clock_set` が無効なら `rejected` で何も送らない。読取り専用の
//!    loader では `notOffered` で何も送らない。
//! 4. 各結果の監査: `unsupported`（標準 Q）・`device`（終了コード）・
//!    `verificationFailed`（SM210 が 3 回 ON）・`rejected`（SM210 が最初から ON）。
//! 5. 時間切れ: 応答しない PLC は `unknown`、接続は張り直され、時計合わせは再送しない。
//! 6. editor でない認証は 403。

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use banto_hub_core::audit::AuditLogService;
use chrono::NaiveDate;
use serde_json::{json, Value};

const OPERATIONAL_MANIFEST: &[u8] =
    include_bytes!("../t12_slmp_wasm_operational_write/manifest.json");
const READ_MANIFEST: &[u8] = include_bytes!("../t12_slmp_wasm/manifest.json");
const COMPONENT_ENV: &str = "BANTO_SLMP_NEW_COMPONENT_WASM";

fn component() -> Vec<u8> {
    std::fs::read(std::env::var(COMPONENT_ENV).expect("set new SLMP guest path")).unwrap()
}

const D: u8 = 0xa8;
const SD: u8 = 0xa9;
const SM: u8 = 0x91;
const END_CODE_C051: u16 = 0xc051;
const END_CODE_C059: u16 = 0xc059;
const OWN: [u8; 5] = [0x00, 0xff, 0xff, 0x03, 0x00];

/// 0401 subcommand 0003 (bit, iQ-R device specification) of SM210, two points.
const STATUS_BODY: [u8; 12] = [1, 4, 3, 0, 0xd2, 0, 0, 0, SM, 0, 2, 0];
/// 1401 subcommand 0002 (word) of SD210-SD216: 2026, 11, 7, 9, 5, 30, 6 (Saturday).
const DATA_BODY: [u8; 26] = [
    1, 0x14, 2, 0, 0xd2, 0, 0, 0, SD, 0, 7, 0, 0xea, 0x07, 11, 0, 7, 0, 9, 0, 5, 0, 30, 0, 6, 0,
];
/// 1401 subcommand 0003 (bit) of SM210 ON.
const SET_BODY: [u8; 13] = [1, 0x14, 3, 0, 0xd2, 0, 0, 0, SM, 0, 1, 0, 0x10];

/// The Hub PC's local time the tests inject: 2026-11-07 (Saturday) 09:05:30.
fn local(hour: u32, minute: u32, second: u32) -> chrono::NaiveDateTime {
    NaiveDate::from_ymd_opt(2026, 11, 7)
        .unwrap()
        .and_hms_opt(hour, minute, second)
        .unwrap()
}

/// How the peer answers the clock requests (0401 of SM, 1401), counted from 1.
#[derive(Default)]
struct Script {
    /// SM210/SM211 for each status read in turn; both OFF when exhausted.
    statuses: VecDeque<(bool, bool)>,
    /// Answer the n-th clock request with this end code.
    end_code: Option<(usize, u16)>,
    /// Never answer the n-th clock request (the PLC goes silent).
    hang: Option<usize>,
}

#[derive(Default)]
struct Seen {
    /// Clock requests only (4E serial zeroed), in order.
    clock: Vec<Vec<u8>>,
    connections: u32,
}

/// A mock SLMP PLC on a std thread: answers 0101 with `name`/`code`, the iQ-R
/// (SD280-283) and Q (SD302-303) handshakes, every D word as 7, and the clock
/// requests by `script`.
struct Peer {
    address: SocketAddr,
    seen: Arc<Mutex<Seen>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Peer {
    fn start(name: &'static str, code: u16, script: Script) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Seen::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (seen, stop) = (seen.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut script = script;
                while !stop.load(Ordering::SeqCst) {
                    let Ok((mut socket, _)) = listener.accept() else {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    };
                    socket.set_nonblocking(false).unwrap();
                    socket
                        .set_read_timeout(Some(Duration::from_millis(50)))
                        .unwrap();
                    seen.lock().unwrap().connections += 1;
                    loop {
                        if stop.load(Ordering::SeqCst) {
                            return;
                        }
                        let mut sub_header = [0u8; 2];
                        match socket.read_exact(&mut sub_header) {
                            Ok(()) => {}
                            Err(e)
                                if matches!(
                                    e.kind(),
                                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                                ) =>
                            {
                                continue
                            }
                            Err(_) => break,
                        }
                        socket.set_read_timeout(None).unwrap();
                        let three_e = match sub_header {
                            [0x50, 0x00] => true,
                            [0x54, 0x00] => false,
                            other => panic!("unexpected sub-header {other:02x?}"),
                        };
                        let head_len = if three_e { 9 } else { 13 };
                        let mut request = sub_header.to_vec();
                        request.resize(head_len, 0);
                        if socket.read_exact(&mut request[2..]).is_err() {
                            break;
                        }
                        let length = usize::from(u16::from_le_bytes([
                            request[head_len - 2],
                            request[head_len - 1],
                        ]));
                        request.resize(head_len + length, 0);
                        if socket.read_exact(&mut request[head_len..]).is_err() {
                            break;
                        }
                        socket
                            .set_read_timeout(Some(Duration::from_millis(50)))
                            .unwrap();
                        let Some((end_code, body)) =
                            answer(name, code, &request, &seen, &mut script)
                        else {
                            continue; // silent
                        };
                        let mut response = if three_e {
                            let mut head = vec![0xd0, 0];
                            head.extend_from_slice(&request[2..7]);
                            head
                        } else {
                            let mut head = vec![0xd4, 0, request[2], request[3], 0, 0];
                            head.extend_from_slice(&request[6..11]);
                            head
                        };
                        response.extend_from_slice(
                            &u16::try_from(body.len() + 2).unwrap().to_le_bytes(),
                        );
                        response.extend_from_slice(&end_code.to_le_bytes());
                        response.extend_from_slice(&body);
                        if socket.write_all(&response).is_err() {
                            break;
                        }
                    }
                }
            })
        };
        Self {
            address,
            seen,
            stop,
            thread: Some(thread),
        }
    }

    fn clock(&self) -> Vec<Vec<u8>> {
        self.seen.lock().unwrap().clock.clone()
    }

    fn connections(&self) -> u32 {
        self.seen.lock().unwrap().connections
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn answer(
    name: &str,
    code: u16,
    request: &[u8],
    seen: &Mutex<Seen>,
    script: &mut Script,
) -> Option<(u16, Vec<u8>)> {
    let at = if request[0] == 0x50 { 11 } else { 15 };
    let command = u16::from_le_bytes([request[at], request[at + 1]]);
    let sub = u16::from_le_bytes([request[at + 2], request[at + 3]]);
    let data = &request[at + 4..];
    if command == 0x0101 {
        let mut body = vec![b' '; 18];
        body[..name.len()].copy_from_slice(name.as_bytes());
        body[16..18].copy_from_slice(&code.to_le_bytes());
        return Some((0, body));
    }
    if command == 0x1401 || (command == 0x0401 && sub == 3) {
        let mut recorded = request.to_vec();
        if recorded[0] == 0x54 {
            recorded[2] = 0;
            recorded[3] = 0;
        }
        let n = {
            let mut seen = seen.lock().unwrap();
            seen.clock.push(recorded);
            seen.clock.len()
        };
        if script.hang == Some(n) {
            return None;
        }
        if let Some((at, end_code)) = script.end_code {
            if at == n {
                return Some((end_code, Vec::new()));
            }
        }
        return Some(match command {
            0x1401 => (0, Vec::new()),
            _ => {
                assert_eq!(data[4], SM, "only SM is read as bits");
                let (sm210, sm211) = script.statuses.pop_front().unwrap_or((false, false));
                (0, vec![(u8::from(sm210) << 4) | u8::from(sm211)])
            }
        });
    }
    assert_eq!(command, 0x0401, "unexpected command {command:#x}");
    let (start, device, points) = match sub {
        0 => (
            u32::from_le_bytes([data[0], data[1], data[2], 0]),
            data[3],
            u16::from_le_bytes([data[4], data[5]]),
        ),
        2 => (
            u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
            data[4],
            u16::from_le_bytes([data[6], data[7]]),
        ),
        other => panic!("unexpected subcommand {other:#x}"),
    };
    Some(match (device, start, points) {
        (SD, 280, 4) => {
            let mut body = 8000u32.to_le_bytes().to_vec();
            body.extend_from_slice(&0x200u32.to_le_bytes());
            (0, body)
        }
        (SD, 302, 2) => {
            let mut body = 4000u16.to_le_bytes().to_vec();
            body.extend_from_slice(&64u16.to_le_bytes());
            (0, body)
        }
        (D, _, _) => (0, (0..points).flat_map(|_| 7u16.to_le_bytes()).collect()),
        _ => (END_CODE_C051, Vec::new()),
    })
}

/// A 4E request (serial zeroed) with the own-station route and timer 0010H.
fn frame_4e(data: &[u8]) -> Vec<u8> {
    let mut frame = vec![0x54, 0x00, 0x00, 0x00, 0x00, 0x00];
    frame.extend_from_slice(&OWN);
    frame.extend_from_slice(&u16::try_from(data.len() + 2).unwrap().to_le_bytes());
    frame.extend_from_slice(&0x0010u16.to_le_bytes());
    frame.extend_from_slice(data);
    frame
}

/// A full successful run: status, data, set, status.
fn success_frames() -> Vec<Vec<u8>> {
    vec![
        frame_4e(&STATUS_BODY),
        frame_4e(&DATA_BODY),
        frame_4e(&SET_BODY),
        frame_4e(&STATUS_BODY),
    ]
}

/// A Hub with the new guest registered through `manifest`'s loader.
async fn hub(label: &str, operational: bool) -> super::TestApp {
    let app = if operational {
        super::test_app_with_operational_write_manifest(label, OPERATIONAL_MANIFEST, &component())
            .await
    } else {
        super::test_app_with_manifest(label, READ_MANIFEST, &component()).await
    };
    app.manager
        .clock_sync()
        .set_local_now(Arc::new(|| local(9, 5, 30)));
    app
}

/// A saved connection `name` to `peer` (one group, one D100 tag), with
/// `extra` driver settings. Collection is not started here.
async fn add_connection(
    app: &super::TestApp,
    name: &str,
    peer: &Peer,
    operational: bool,
    extra: Value,
) -> i64 {
    let mut settings = json!({ "host": "127.0.0.1", "port": peer.address.port() });
    for (key, value) in extra.as_object().unwrap() {
        settings[key] = value.clone();
    }
    let protocol = if operational {
        "slmp-wasm-operational-write"
    } else {
        "slmp-read-wasm"
    };
    let saved = app
        .manager
        .plc_connections()
        .create_settings(super::ConnectionSettingsInput {
            name: name.into(),
            protocol: protocol.into(),
            enabled: true,
            simulation: false,
            settings: settings.as_object().unwrap().clone(),
            secrets: Default::default(),
        })
        .await
        .unwrap();
    let group = super::CollectionGroupService::with_registry(
        app.pool.clone(),
        app.manager.registry().clone(),
    )
    .create(super::group_input("group", saved.id, 60_000))
    .await
    .unwrap();
    let (status, body) = super::admin_write(
        &app.router,
        "POST",
        "/api/tags",
        &app.admin_token,
        json!({
            "name": "d100", "collectionGroupId": group.id, "address": "D100",
            "dataType": "u16", "enabled": true, "writable": false
        }),
    )
    .await;
    assert_eq!(status, super::StatusCode::OK, "{body:?}");
    saved.id
}

async fn wait_connected(app: &super::TestApp, id: i64) {
    let mut status = app.sessions.status_watch(id).expect("a broker session");
    tokio::time::timeout(
        Duration::from_secs(10),
        status.wait_for(|state| *state == banto_broker::BrokerConnectionStatus::Connected),
    )
    .await
    .expect("connected in time")
    .unwrap();
}

fn path(id: i64) -> String {
    format!("/api/plc-connections/{id}/clock-sync")
}

async fn post(app: &super::TestApp, id: i64) -> Value {
    let (status, body) =
        super::admin_write(&app.router, "POST", &path(id), &app.admin_token, json!({})).await;
    assert_eq!(status, super::StatusCode::OK, "{body:?}");
    body
}

/// `(actor, origin, result, entity, detail)` of the clock-sync audit rows.
async fn audit_rows(app: &super::TestApp) -> Vec<(Option<String>, String, String, String, Value)> {
    use sqlx::Row;
    sqlx::query(
        "SELECT actor_username, origin, result, entity_id, detail FROM audit_log \
         WHERE resource = 'plc_connection_clock_sync' AND action = 'clock_sync' ORDER BY id",
    )
    .fetch_all(&app.pool)
    .await
    .unwrap()
    .iter()
    .map(|row| {
        (
            row.get("actor_username"),
            row.get("origin"),
            row.get("result"),
            row.get("entity_id"),
            serde_json::from_str(&row.get::<String, _>("detail")).unwrap(),
        )
    })
    .collect()
}

/// 1. 定期の実行。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #76 component"]
async fn scheduled_clock_sync_runs_once_in_the_matching_minute_and_is_audited() {
    let peer = Peer::start("R08CPU", 0x4801, Script::default());
    let app = hub("t12-clock-schedule", true).await;
    let id = add_connection(&app, "line", &peer, true, json!({ "clock_set": "enabled" })).await;
    app.commit_and_start().await;
    wait_connected(&app, id).await;

    let now = Arc::new(Mutex::new(local(8, 5, 0)));
    let source = now.clone();
    app.manager
        .clock_sync()
        .set_local_now(Arc::new(move || *source.lock().unwrap()));
    let audit = AuditLogService::new(super::Db::Sqlite(app.pool.clone()));
    let mut scheduler = banto_hub_core::clock_sync::spawn_scheduler_with(
        app.manager.clone(),
        audit,
        Duration::from_millis(10),
    );

    // Hub の enabled が偽（既定の「毎時 5 分」に合う分）では出さない。
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(peer.clock().is_empty(), "disabled: nothing sent");
    assert!(app.manager.clock_sync().last(id).is_none());

    let (status, body) = super::admin_write(
        &app.router,
        "PUT",
        &path(id),
        &app.admin_token,
        json!({ "enabled": true, "schedule": { "kind": "hourly", "minute": 5 } }),
    )
    .await;
    assert_eq!(status, super::StatusCode::OK, "{body:?}");
    // 08:05 は評価済みなので、次に合う 09:05 まで出ない（追いつかない）。
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(peer.clock().is_empty(), "08:05 is not caught up");
    *now.lock().unwrap() = local(9, 5, 30);
    tokio::time::timeout(Duration::from_secs(10), async {
        while app.manager.clock_sync().last(id).is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the scheduled run in time");
    // 同じ分の再評価、次の分、巻き戻した同じ分のどれでも 2 回目は無い。
    *now.lock().unwrap() = local(9, 5, 59);
    tokio::time::sleep(Duration::from_millis(200)).await;
    *now.lock().unwrap() = local(9, 6, 0);
    tokio::time::sleep(Duration::from_millis(200)).await;
    *now.lock().unwrap() = local(9, 5, 30);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(scheduler.stop_and_join(Duration::from_secs(10)).await);
    assert_eq!(
        peer.clock(),
        success_frames(),
        "one run, SD210-SD216 = 09:05:30"
    );

    let (status, body) =
        super::admin_write(&app.router, "GET", &path(id), &app.admin_token, json!({})).await;
    assert_eq!(status, super::StatusCode::OK);
    assert_eq!(body["settings"]["enabled"], true);
    assert_eq!(body["last"]["result"], "ok", "{body:?}");
    assert_eq!(body["last"]["trigger"], "schedule");
    assert_eq!(body["last"]["time"], "2026-11-07T09:05:30");
    let rows = audit_rows(&app).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    let (actor, origin, result, entity, detail) = &rows[0];
    assert_eq!(actor, &None);
    assert_eq!((origin.as_str(), result.as_str()), ("schedule", "ok"));
    assert_eq!(entity, &id.to_string());
    assert_eq!(detail["time"], "2026-11-07T09:05:30");
    assert_eq!(detail["result"], "ok");
    assert_eq!(peer.connections(), 1, "the collection session was reused");
    drop(app);
}

/// 2. 手動の実行（Hub の enabled は既定の偽のまま）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #76 component"]
async fn manual_clock_sync_sets_the_clock_and_is_audited_with_the_operator() {
    let peer = Peer::start("R08CPU", 0x4801, Script::default());
    let app = hub("t12-clock-manual", true).await;
    let id = add_connection(&app, "line", &peer, true, json!({ "clock_set": "enabled" })).await;
    app.commit_and_start().await;
    wait_connected(&app, id).await;
    let body = post(&app, id).await;
    assert_eq!(body["result"], "ok", "{body:?}");
    assert_eq!(body["trigger"], "manual");
    assert_eq!(body["time"], "2026-11-07T09:05:30");
    assert_eq!(peer.clock(), success_frames());
    let rows = audit_rows(&app).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].0.as_deref(), Some("admin"));
    assert_eq!((rows[0].1.as_str(), rows[0].2.as_str()), ("rest", "ok"));
    let (_, got) =
        super::admin_write(&app.router, "GET", &path(id), &app.admin_token, json!({})).await;
    assert_eq!(got["last"], body);
    assert_eq!(peer.connections(), 1);
    drop(app);
}

/// 3. ゲート: ゲストの `clock_set` が無効なら `rejected`、読取り専用の loader では
/// `notOffered`。どちらも時計合わせの要求を 1 つも送らない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #76 component"]
async fn the_guest_gate_and_the_read_only_loader_send_nothing() {
    let unset = Peer::start("R08CPU", 0x4801, Script::default());
    let disabled = Peer::start("R08CPU", 0x4801, Script::default());
    let app = hub("t12-clock-gate", true).await;
    let unset_id = add_connection(&app, "unset", &unset, true, json!({})).await;
    let disabled_id = add_connection(
        &app,
        "disabled",
        &disabled,
        true,
        json!({ "clock_set": "disabled" }),
    )
    .await;
    app.commit_and_start().await;
    for (id, peer) in [(unset_id, &unset), (disabled_id, &disabled)] {
        wait_connected(&app, id).await;
        let body = post(&app, id).await;
        assert_eq!(body["result"], "rejected", "{body:?}");
        assert!(
            body["message"].as_str().unwrap().contains("clock_set"),
            "{body:?}"
        );
        assert!(peer.clock().is_empty());
    }
    drop(app);

    let read_only = Peer::start("R08CPU", 0x4801, Script::default());
    let app = hub("t12-clock-read-only", false).await;
    let id = add_connection(&app, "line", &read_only, false, json!({})).await;
    app.commit_and_start().await;
    wait_connected(&app, id).await;
    let body = post(&app, id).await;
    assert_eq!(body["result"], "notOffered", "{body:?}");
    assert_eq!(body["time"], Value::Null);
    assert!(read_only.clock().is_empty());
    let rows = audit_rows(&app).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].2, "failed");
    assert_eq!(rows[0].4["result"], "notOffered");
    drop(app);
}

/// 接続 id・模擬機器・期待する result・code・時計合わせの要求。
type Case<'a> = (i64, &'a Peer, &'a str, Option<u32>, Vec<Vec<u8>>);

/// 4. 各結果が直近の結果と監査に残る（どれも接続は保つ）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #76 component"]
async fn each_refusal_is_audited_with_its_result() {
    let standard_q = Peer::start("Q06HCPU", 0x0042, Script::default());
    let device = Peer::start(
        "R08CPU",
        0x4801,
        Script {
            end_code: Some((2, END_CODE_C059)),
            ..Script::default()
        },
    );
    let unverified = Peer::start(
        "R08CPU",
        0x4801,
        Script {
            statuses: [(false, false), (true, false), (true, false), (true, false)].into(),
            ..Script::default()
        },
    );
    let pending = Peer::start(
        "R08CPU",
        0x4801,
        Script {
            statuses: [(true, false)].into(),
            ..Script::default()
        },
    );
    let app = hub("t12-clock-results", true).await;
    let enabled = json!({ "clock_set": "enabled" });
    let q_id = add_connection(
        &app,
        "q",
        &standard_q,
        true,
        json!({ "clock_set": "enabled", "frame": "3e" }),
    )
    .await;
    let device_id = add_connection(&app, "device", &device, true, enabled.clone()).await;
    let unverified_id =
        add_connection(&app, "unverified", &unverified, true, enabled.clone()).await;
    let pending_id = add_connection(&app, "pending", &pending, true, enabled).await;
    app.commit_and_start().await;

    let mut expected_unverified = success_frames();
    expected_unverified.extend([frame_4e(&STATUS_BODY), frame_4e(&STATUS_BODY)]);
    let cases: [Case<'_>; 4] = [
        (q_id, &standard_q, "unsupported", None, Vec::new()),
        (
            device_id,
            &device,
            "device",
            Some(u32::from(END_CODE_C059)),
            vec![frame_4e(&STATUS_BODY), frame_4e(&DATA_BODY)],
        ),
        (
            unverified_id,
            &unverified,
            "verificationFailed",
            None,
            expected_unverified,
        ),
        (
            pending_id,
            &pending,
            "rejected",
            None,
            vec![frame_4e(&STATUS_BODY)],
        ),
    ];
    for (id, peer, result, code, frames) in &cases {
        wait_connected(&app, *id).await;
        let body = post(&app, *id).await;
        assert_eq!(body["result"], *result, "{body:?}");
        assert_eq!(body["code"], json!(code), "{body:?}");
        assert_eq!(body["time"], "2026-11-07T09:05:30", "{body:?}");
        assert_eq!(&peer.clock(), frames, "{result}");
        assert_eq!(peer.connections(), 1, "{result}: the connection is kept");
    }
    let rows = audit_rows(&app).await;
    assert_eq!(rows.len(), 4, "{rows:?}");
    for ((_, _, result, code, _), row) in cases.iter().zip(&rows) {
        assert_eq!(row.2, "failed");
        assert_eq!(row.4["result"], *result);
        assert_eq!(row.4["code"], json!(code));
    }
    drop(app);
}

/// 5. 応答しない PLC: `unknown`、接続は張り直され、時計合わせは再送しない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #76 component"]
async fn a_silent_plc_is_unknown_reconnects_and_is_never_resent() {
    let peer = Peer::start(
        "R08CPU",
        0x4801,
        Script {
            hang: Some(1),
            ..Script::default()
        },
    );
    let app = hub("t12-clock-timeout", true).await;
    let id = add_connection(&app, "line", &peer, true, json!({ "clock_set": "enabled" })).await;
    app.commit_and_start().await;
    wait_connected(&app, id).await;
    let body = post(&app, id).await;
    assert_eq!(body["result"], "unknown", "{body:?}");
    assert_eq!(body["time"], "2026-11-07T09:05:30");
    tokio::time::timeout(Duration::from_secs(15), async {
        while peer.connections() < 2 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the broker reconnects");
    wait_connected(&app, id).await;
    // 再接続の後もしばらく待ち、時計合わせの要求が増えないこと。
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(peer.clock(), vec![frame_4e(&STATUS_BODY)], "never resent");
    let rows = audit_rows(&app).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].2, "failed");
    assert_eq!(rows[0].4["result"], "unknown");
    drop(app);
}

/// 6. editor でない認証は 403（設定・直近の結果・実行のどれも）。収集していない接続の
/// POST は 404。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "set BANTO_SLMP_NEW_COMPONENT_WASM to the pinned #76 component"]
async fn a_viewer_is_forbidden_and_nothing_runs() {
    let peer = Peer::start("R08CPU", 0x4801, Script::default());
    let app = hub("t12-clock-viewer", true).await;
    let id = add_connection(&app, "line", &peer, true, json!({ "clock_set": "enabled" })).await;
    let (status, _) =
        super::admin_write(&app.router, "POST", &path(id), &app.admin_token, json!({})).await;
    assert_eq!(status, super::StatusCode::NOT_FOUND, "not collecting yet");
    app.commit_and_start().await;
    wait_connected(&app, id).await;
    app.users
        .create_user("viewer1", "password123", "閲覧者", super::Role::Viewer)
        .await
        .unwrap();
    let viewer = app.auth.login("viewer1", "password123").await.unwrap();
    let (status, _) = super::admin_write(&app.router, "GET", &path(id), &viewer, json!({})).await;
    assert_eq!(status, super::StatusCode::FORBIDDEN);
    for method in ["PUT", "POST"] {
        let (status, _) = super::admin_write(
            &app.router,
            method,
            &path(id),
            &viewer,
            json!({ "enabled": true, "schedule": { "kind": "hourly", "minute": 5 } }),
        )
        .await;
        assert_eq!(status, super::StatusCode::FORBIDDEN, "{method}");
    }
    assert!(peer.clock().is_empty());
    assert!(audit_rows(&app).await.is_empty());
    drop(app);
}
