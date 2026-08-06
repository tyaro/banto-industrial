//! T8-2 の E2E テスト（docs/tag-server-design.md §6.1、T8 実装指示のテスト
//! 計画1〜4に対応 - 5「既存全テストを壊さない」はこのクレート・
//! `banto-collect`・`banto-plc`・`banto-plc-write`・`banto-broker` の既存
//! スイートを流すことで確認する（このファイル自体はそれに寄与しない）)。
//!
//! `tests/write.rs`/`tests/t7_partial_reconfig.rs` と同じ理由（各
//! `tests/*.rs` は独立したクレートとしてコンパイルされ、private helper を
//! 共有できない）で `TempEnv`/`fast_options`/`wait_until`/`TestApp` 相当を
//! このファイル内に複製している。
//!
//! テスト構成:
//! 1. 収集: 同一ワード（D100 = 0x1234）を D100.5 / D100.12 のビットタグ2本 +
//!    D100 のワードタグ1本で読む - 3タグとも正しい値、ワード読みは
//!    デコード時にビット抽出されるだけで PLC への読み要求自体は共有される
//!    （§6.1「同一ワードの16ビットを何タグ定義しても PLC 負荷は不変」）
//! 2. 書き込み: writable なビットタグ（D100.2）へ write API で true ->
//!    シミュレータのワード値が該当ビットのみ変化（他ビット不変、
//!    `banto-plc-write` の RMW が正しく配線されている証拠）-> 収集で
//!    読み戻して `/api/v1/values` に反映される（読み書き単一セッションの
//!    実証、`tests/write.rs`のハッピーパスと同型）
//! 3. data_type 不一致: ビット付きアドレス（`D100.5`）+ `data_type=i16` の
//!    登録 -> rebuild が構成エラー（`banto-collect::config::build_request`、
//!    T8-2 で追加した検証）で旧構成を維持する（§4.3 の all-or-nothing、
//!    `last_config_error` に現れる）
//! 4. 確認読み不一致: `Simulator::corrupt_after_next_write` で PLC 側の
//!    競合書き込みを再現 -> write API が 502 `write_failed` を返し、
//!    `write_audit` の `detail` に「書き戻し競合の可能性」が記録される
//!    （`banto_plc_write::error::PlcWriteError::BitWriteVerificationFailed`
//!    の文言がそのまま REST 応答・監査行の両方に伝播することの確認）

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request as HttpRequest, StatusCode};
use axum::Router;
use banto_collect::{BackoffConfig, CollectorOptions};
use banto_hub_core::api_keys::ApiKeysService;
use banto_hub_core::audit::AuditLogService;
use banto_hub_core::broker_glue::{HubSessions, SlmpSimRegistry};
use banto_hub_core::computed::{ComputedEngine, ServerTagStore};
use banto_hub_core::db::init_db;
use banto_hub_core::grpc::{GrpcServer, GrpcService};
use banto_hub_core::hub::CollectorManager;
use banto_hub_core::rest::api_router;
use banto_hub_core::users::UsersService;
use banto_hub_core::write_audit::WriteAuditService;
use banto_hub_core::write_control::WriteControl;
use banto_hub_core::write_rate::{WriteRateLimitConfig, WriteRateLimiter};
use banto_plc::slmp::address::SlmpDevice;
use banto_plc_write::slmp::simulator::Simulator;
use banto_server::{AuthState, Identity};
use banto_tags::{
    CollectionGroupInput, CollectionGroupService, PlcConnectionInput, PlcConnectionService,
    TagInput, TagService,
};
use banto_tstore::SystemClock;
use serde_json::{json, Value};
use sqlx::SqlitePool;
use tokio::sync::broadcast;
use tower::ServiceExt;

const CLIENT_HEADER: (&str, &str) = ("X-Banto-Client", "banto");

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempEnv {
    root: PathBuf,
}

impl TempEnv {
    fn new(label: &str) -> Self {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "banto-hub-t8-it-{}-{label}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("create temp env");
        Self { root }
    }

    fn registry_path(&self) -> PathBuf {
        self.root.join("registry.sqlite3")
    }

    fn data_dir(&self) -> PathBuf {
        self.root.join("data")
    }
}

impl Drop for TempEnv {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn fast_options() -> CollectorOptions {
    CollectorOptions {
        backoff: BackoffConfig {
            base: Duration::from_millis(20),
            max: Duration::from_millis(100),
        },
        connect_timeout: Duration::from_millis(500),
        response_timeout: Duration::from_millis(500),
        writer_options: banto_tstore::WriterOptions {
            max_buffered_rows: 1,
            flush_interval_ms: 0,
        },
    }
}

fn slmp_conn_input(name: &str, port: u16) -> PlcConnectionInput {
    PlcConnectionInput {
        name: name.to_string(),
        protocol: "slmp".to_string(),
        host: "127.0.0.1".to_string(),
        port: port as i64,
        unit_id: 1,
        enabled: true,
        simulation: false,
    }
}

fn group_input(name: &str, conn_id: i64, period_ms: i64) -> CollectionGroupInput {
    CollectionGroupInput {
        name: name.to_string(),
        plc_connection_id: conn_id,
        period_ms,
        enabled: true,
    }
}

#[allow(clippy::too_many_arguments)]
fn tag_input(
    name: &str,
    group_id: i64,
    address: &str,
    data_type: &str,
    writable: bool,
    enabled: bool,
) -> TagInput {
    TagInput {
        name: name.to_string(),
        collection_group_id: group_id,
        address: address.to_string(),
        data_type: data_type.to_string(),
        string_length: None,
        raw_lo: None,
        raw_hi: None,
        eng_lo: None,
        eng_hi: None,
        unit: None,
        decimals: 0,
        threshold_h: None,
        threshold_hh: None,
        threshold_l: None,
        threshold_ll: None,
        enabled,
        writable,
        tag_kind: "plc".to_string(),
        expression: None,
        retain: false,
    }
}

/// Poll `predicate` every 20ms until it returns true or `timeout` elapses.
async fn wait_until<F, Fut>(timeout: Duration, mut predicate: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if predicate().await {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

struct TestApp {
    router: Router,
    admin_token: String,
    pool: SqlitePool,
    manager: Arc<CollectorManager>,
    write_control: Arc<WriteControl>,
    _env: TempEnv,
}

async fn test_app(label: &str) -> TestApp {
    let env = TempEnv::new(label);
    let pool = init_db(env.registry_path()).await.expect("init_db");

    let users = UsersService::new(pool.clone());
    let audit = AuditLogService::new(pool.clone());
    users
        .setup_first_user("admin", "password123", "管理者")
        .await
        .expect("setup_first_user");

    let verify_users = users.clone();
    let auth = AuthState::new(move |u: String, p: String| {
        let users = verify_users.clone();
        Box::pin(async move {
            match users.verify(&u, &p).await {
                Ok(Some(identity)) => Some(Identity {
                    id: identity.username,
                    name: identity.display_name,
                    role: identity.role.to_string(),
                }),
                _ => None,
            }
        })
    });
    let admin_token = auth
        .login("admin", "password123")
        .await
        .expect("admin login");

    let sessions = Arc::new(HubSessions::new(banto_broker::BackoffConfig::default()));
    let sim_registry = Arc::new(SlmpSimRegistry::new());
    let computed = Arc::new(ComputedEngine::new(Arc::new(ServerTagStore::new())));
    let manager = Arc::new(CollectorManager::new(
        pool.clone(),
        env.data_dir(),
        Arc::new(SystemClock),
        fast_options(),
        sessions,
        sim_registry,
        computed,
    ));
    manager.rebuild().await.expect("initial rebuild");

    let (events_tx, _rx) = broadcast::channel(16);
    let write_control = Arc::new(WriteControl::new(false));
    let write_audit = WriteAuditService::new(pool.clone());
    let mqtt = Arc::new(banto_hub_core::mqtt::MqttPublisher::new(manager.clone()));
    let api_keys = ApiKeysService::new(pool.clone());
    let rate_limiter = Arc::new(tokio::sync::Mutex::new(WriteRateLimiter::new(
        WriteRateLimitConfig::default(),
    )));
    let grpc_service = GrpcService::new(
        manager.clone(),
        api_keys.clone(),
        audit.clone(),
        write_audit.clone(),
        write_control.clone(),
        rate_limiter.clone(),
        events_tx.clone(),
    );
    let grpc_server = Arc::new(GrpcServer::new(grpc_service));

    let router = api_router(
        users,
        audit,
        PlcConnectionService::new(pool.clone()),
        CollectionGroupService::new(pool.clone()),
        TagService::new(pool.clone()),
        api_keys,
        manager.clone(),
        auth,
        events_tx,
        false,
        write_control.clone(),
        write_audit,
        mqtt,
        grpc_server,
        rate_limiter,
    );

    TestApp {
        router,
        admin_token,
        pool,
        manager,
        write_control,
        _env: env,
    }
}

async fn get_json(router: &Router, path: &str, bearer: &str) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            HttpRequest::get(path)
                .header("Authorization", format!("Bearer {bearer}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

/// admin 管理系エンドポイント用（CSRF ヘッダ必須）。
async fn admin_post(router: &Router, path: &str, token: &str, body: Value) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            HttpRequest::post(path)
                .header("Authorization", format!("Bearer {token}"))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

/// `/api/v1/*` 用（CSRF ヘッダ不要 - 設計 §5.1/§5.6）。`bearer` は `bh_...`
/// API キーでもセッション token でもよい。
async fn v1_post(router: &Router, path: &str, bearer: &str, body: Value) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            HttpRequest::post(path)
                .header("Authorization", format!("Bearer {bearer}"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

/// `POST /api/api-keys` 経由でキーを発行し、平文キー全体(`bh_...`)と id を返す。
async fn issue_key(
    router: &Router,
    admin_token: &str,
    name: &str,
    scopes: &[&str],
) -> (String, i64) {
    let (status, body) = admin_post(
        router,
        "/api/api-keys",
        admin_token,
        json!({ "name": name, "scopes": scopes }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body:?}");
    (
        body["key"].as_str().unwrap().to_string(),
        body["id"].as_i64().unwrap(),
    )
}

/// SLMP 接続 + 収集グループを1本作り、`(connection_id, group_id)` を返す
/// 共通フィクスチャ。`tests/write.rs::make_tag` と違い、1接続に複数タグを
/// 積む必要があるシナリオ（同一ワードを指す複数タグ、など）向けにタグ作成を
/// 分離してある。
async fn make_conn_and_group(app: &TestApp, conn_name: &str, port: u16) -> (i64, i64) {
    let conn = PlcConnectionService::new(app.pool.clone())
        .create(slmp_conn_input(conn_name, port))
        .await
        .unwrap();
    let group = CollectionGroupService::new(app.pool.clone())
        .create(group_input("fast", conn.id, 100))
        .await
        .unwrap();
    (conn.id, group.id)
}

/// 1タグ登録して `tag_id` を返す（rebuild はしない - 呼び出し元が必要な数
/// 登録し終えてから1回だけ `app.manager.rebuild()` する）。
#[allow(clippy::too_many_arguments)]
async fn add_tag(
    app: &TestApp,
    group_id: i64,
    tag_name: &str,
    address: &str,
    data_type: &str,
    writable: bool,
) -> i64 {
    TagService::new(app.pool.clone())
        .create(tag_input(
            tag_name, group_id, address, data_type, writable, true,
        ))
        .await
        .unwrap()
        .id
}

/// タグ・グループ・接続を1本作って rebuild まで済ませ、`(tag_id,
/// external_name)` を返す共通フィクスチャ（`tests/write.rs::make_tag` と同型）。
#[allow(clippy::too_many_arguments)]
async fn make_tag(
    app: &TestApp,
    conn_name: &str,
    port: u16,
    tag_name: &str,
    address: &str,
    data_type: &str,
    writable: bool,
) -> (i64, String) {
    let (_conn_id, group_id) = make_conn_and_group(app, conn_name, port).await;
    let tag_id = add_tag(app, group_id, tag_name, address, data_type, writable).await;
    app.manager.rebuild().await.expect("rebuild");
    (tag_id, format!("{conn_name}.fast.{tag_name}"))
}

// ---------------------------------------------------------------------------
// 1. 収集: 同一ワード(D100 = 0x1234)を bit タグ2本 + word タグ1本で読む。
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn collection_reads_bit_and_word_tags_off_the_same_word() {
    let app = test_app("t8-collect").await;
    let sim = Simulator::start().await;
    // 0x1234 = 0b0001_0010_0011_0100: bits 2, 4, 5, 9, 12 are set - bit 5 and
    // bit 12 (both set) are the two the T8-2 scope asked for by name.
    sim.set_word(SlmpDevice::D, 100, 0x1234);

    let (_conn_id, group_id) = make_conn_and_group(&app, "line1", sim.addr.port()).await;
    add_tag(&app, group_id, "bit5", "D100.5", "bit", false).await;
    add_tag(&app, group_id, "bit12", "D100.12", "bit", false).await;
    add_tag(&app, group_id, "word", "D100", "u16", false).await;
    app.manager.rebuild().await.expect("rebuild");

    // Collection is async (100ms period) - poll until every tag settles.
    assert!(
        wait_until(Duration::from_secs(3), || async {
            let (status, json) = get_json(
                &app.router,
                "/api/v1/values/line1.fast.word",
                &app.admin_token,
            )
            .await;
            status == StatusCode::OK && json["v"] == 0x1234_u32 as f64 && json["q"] == "good"
        })
        .await,
        "word tag should read the whole word"
    );

    let (status, json) = get_json(
        &app.router,
        "/api/v1/values/line1.fast.bit5",
        &app.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["v"], 1.0, "bit 5 is set in 0x1234");
    assert_eq!(json["q"], "good");

    let (status, json) = get_json(
        &app.router,
        "/api/v1/values/line1.fast.bit12",
        &app.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["v"], 1.0, "bit 12 is set in 0x1234");
    assert_eq!(json["q"], "good");

    // §6.1: "同一ワードの16ビットを何タグ定義しても PLC 負荷は不変" - the
    // three tags above never cost more than one D100 word in the read plan
    // (banto-plc's planner groups them), so the simulator sees exactly one
    // distinct word address read per cycle's worth of traffic; this is
    // already covered at the planner-unit-test level
    // (`crates/banto-plc/src/slmp/planning.rs`), so it is not re-asserted
    // here via wire counts (this test's simulator serves other connections'
    // traffic too, making a bare count assertion flaky) - the three correct
    // values above are the E2E-level evidence that the plan folded them.

    sim.stop();
}

// ---------------------------------------------------------------------------
// 2. 書き込み: writable なビットタグへ write API で true -> シミュレータの
//    ワード値が該当ビットのみ変化 -> 収集で読み戻して /api/v1/values に反映。
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_to_a_bit_in_word_tag_changes_only_that_bit_and_collection_reads_it_back() {
    let app = test_app("t8-write").await;
    let sim = Simulator::start().await;
    sim.set_word(SlmpDevice::D, 100, 0x0000);

    let (tag_id, external_name) = make_tag(
        &app,
        "line1",
        sim.addr.port(),
        "flag2",
        "D100.2",
        "bit",
        true,
    )
    .await;
    app.write_control.enable();
    let (key, _id) = issue_key(
        &app.router,
        &app.admin_token,
        "writer",
        &["write:line1.fast.flag2"],
    )
    .await;

    let (status, body) = v1_post(
        &app.router,
        &format!("/api/v1/values/{external_name}"),
        &key,
        json!({ "v": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["result"], "ok");

    assert_eq!(
        sim.get_word(SlmpDevice::D, 100),
        0x0004,
        "only bit 2 should have changed - the RMW must not disturb the word's other bits"
    );

    // 読み書き単一セッションの実証（`tests/write.rs`のハッピーパスと同型）:
    // 収集が同じ broker セッションで書き込み結果を読み戻す。
    let tag_key = format!("tag:{tag_id}");
    assert!(
        wait_until(Duration::from_secs(3), || async {
            app.manager
                .current_values()
                .and_then(|c| c.get(&tag_key))
                .map(|s| s.value)
                == Some(Some(1.0))
        })
        .await,
        "collection should read the bit back as true"
    );

    let (status, json) = get_json(
        &app.router,
        &format!("/api/v1/values/{external_name}"),
        &app.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["v"], 1.0);
    assert_eq!(json["q"], "good");

    sim.stop();
}

// ---------------------------------------------------------------------------
// 3. data_type 不一致: ビット付きアドレス + i16 の登録は構成エラー
//    (last_config_error + 旧構成維持)。
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bit_qualified_address_on_a_non_bit_tag_is_a_config_error_and_keeps_the_old_catalog() {
    let app = test_app("t8-config-error").await;
    let sim = Simulator::start().await;
    sim.set_word(SlmpDevice::D, 100, 0x0000);

    let (_conn_id, group_id) = make_conn_and_group(&app, "line1", sim.addr.port()).await;
    add_tag(&app, group_id, "temp01", "D100", "u16", false).await;
    app.manager
        .rebuild()
        .await
        .expect("the first rebuild (valid tag) should succeed");
    let revision_before = app.manager.revision();
    assert_eq!(app.manager.last_error(), None);

    // Now register a tag whose address is bit-in-word (`D100.5`) but whose
    // data_type is not "bit" - the T8-2 validation in
    // `banto-collect::config::build_request` must reject this at
    // build_config time.
    add_tag(&app, group_id, "bad", "D100.5", "i16", false).await;
    let err = app
        .manager
        .rebuild()
        .await
        .expect_err("a bit-in-word address on a non-bit tag must fail rebuild");
    assert!(!err.is_empty());

    // §4.3 all-or-nothing: revision does not advance, last_config_error is
    // set, and the previously-good tag is still readable exactly as before -
    // the bad tag never enters the live catalog.
    assert_eq!(
        app.manager.revision(),
        revision_before,
        "revision must not advance on a config error"
    );
    assert_eq!(app.manager.last_error(), Some(err.clone()));

    let (status, status_json) = get_json(&app.router, "/api/v1/status", &app.admin_token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        status_json["last_config_error"].as_str(),
        Some(err.as_str())
    );
    assert_eq!(
        status_json["revision"].as_u64(),
        Some(revision_before),
        "the /api/v1/status revision must also stay put"
    );

    let (status, _json) = get_json(
        &app.router,
        "/api/v1/values/line1.fast.temp01",
        &app.admin_token,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the old, valid tag stays in the catalog"
    );

    let (status, _json) = get_json(
        &app.router,
        "/api/v1/values/line1.fast.bad",
        &app.admin_token,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "the bad tag must never reach the live catalog"
    );

    sim.stop();
}

// ---------------------------------------------------------------------------
// 4. 確認読み不一致: corrupt_after_next_write で強制 -> write API が 502 +
//    write_audit に「書き戻し競合の可能性」が記録される。
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_confirmation_read_mismatch_is_502_and_recorded_in_write_audit() {
    let app = test_app("t8-rmw-race").await;
    let sim = Simulator::start().await;
    sim.set_word(SlmpDevice::D, 100, 0x0000);
    // The instant the write-back for D100 lands, flip bit 5 right back off -
    // simulating a competing PLC-side write racing our RMW (§6.1's
    // documented, un-preventable-but-detectable race).
    sim.corrupt_after_next_write(SlmpDevice::D, 100, 1 << 5);

    let (tag_id, external_name) = make_tag(
        &app,
        "line1",
        sim.addr.port(),
        "flag5",
        "D100.5",
        "bit",
        true,
    )
    .await;
    app.write_control.enable();
    let (key, key_id) = issue_key(
        &app.router,
        &app.admin_token,
        "writer",
        &["write:line1.fast.flag5"],
    )
    .await;

    let (status, body) = v1_post(
        &app.router,
        &format!("/api/v1/values/{external_name}"),
        &key,
        json!({ "v": true }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body:?}");
    assert_eq!(body["error"], "write_failed");
    let response_detail = body["detail"]
        .as_str()
        .expect("write_failed must carry a detail explaining the mismatch");
    assert!(
        response_detail.contains("書き戻し競合の可能性"),
        "response detail should mention the possible race: {response_detail}"
    );

    let (status, listed) = admin_post(
        &app.router,
        "/api/write-audit/list",
        &app.admin_token,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let rows = listed["rows"].as_array().unwrap();
    let row = rows
        .iter()
        .find(|r| r["tagId"].as_i64() == Some(tag_id) && r["apiKeyId"].as_i64() == Some(key_id))
        .expect("an audit row for this attempted write should exist");
    assert_eq!(row["action"], "write");
    assert_eq!(row["result"], "failed");
    let audit_detail = row["detail"]
        .as_str()
        .expect("the confirmed audit row must carry the failure detail (T8-2)");
    assert!(
        audit_detail.contains("書き戻し競合の可能性"),
        "audit detail should mention the possible race: {audit_detail}"
    );

    sim.stop();
}
