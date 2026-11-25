//! banto v8 追従（docs/banto-v8-upgrade-plan.md §5 の 3）: 新しい DB の認証の初期値を、
//! サービスと同じ組み立て（`build_hub_config_from_env(HubHostKind::Service, ..)` →
//! `HubRuntime::start`、profile の排他あり）で起動まで通して確かめる。
//!
//! 上流の `0015_auth_mode_defaults.sql` は、新しい DB に `auth.disabled = true`
//! （ログインなし・`admin`）を入れる。Hub はこの設定を読まない（`AuthState` は banto の
//! 既定のログイン必須のまま）。新しい DB は試運転モード（`commissioning.locked_down` が
//! 未設定）で、試運転は loopback の bind でしか起動できない（`enforce_loopback_when_commissioning`）。
//! ここで確かめること:
//!
//! 1. `create-admin` の前: LAN の bind（`0.0.0.0`）では起動を断り、文言が `create-admin` を
//!    案内する。loopback の bind では起動し、資格情報なしの要求（管理 REST・`/mcp`・
//!    WebSocket の `/api/v1/stream`）は 401、組み込みの `admin` でもログインできない。
//!    loopback の peer だけが試運転の grant を受け取れ、その grant は本番の `AuthState` の
//!    経路で ui-settings を読み書きできる（§6 の 2）。
//! 2. サービスが動いている間（profile の排他を持つ間）の `create-admin`（CLI と同じ
//!    `build_hub_config_from_env(HubHostKind::Console, ..)` → `run_create_admin`）は、
//!    何も書かずに失敗する。
//! 3. 止めてからの `create-admin` の後: 起動すると試運転の grant は閉じ（loopback からも
//!    403）、資格情報なしの要求は 401、作った管理者ではログインできる。LAN の bind の
//!    起動も断られなくなる（ロックダウン済み）。
//!
//! 実際の待ち受けは 127.0.0.1 だけ（LAN の bind で待ち受けると Windows のファイアウォールの
//! 確認が出うるので、LAN の bind は「断られる」側だけを本当に起動し、ロックダウン後の LAN の
//! bind は起動の検査の関数で確かめる）。loopback の peer でさえ通らないものは、LAN の peer
//! でも通らない（試運転の grant の LAN の peer の拒否は `banto-hub-core` の
//! `rest::tests::commissioning` が確かめる）。
//!
//! env（`BANTO_HUB_ROOT` など）を書き換えるので、このファイルの試験は 1 本にまとめる
//! （同じプロセスの別の試験と env が重ならない）。

mod common;

use std::net::SocketAddr;
use std::path::Path;

use banto_hub_core::first_admin::{run_create_admin, NewAdmin};
use banto_hub_core::profile_lock::HubHostKind;
use banto_hub_core::profile_paths::build_hub_config_from_env;
use banto_hub_core::runtime::{HubConfig, HubRuntime, HubStartError};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const PROFILE_ID: &str = "first-admin-service-test";
const ADMIN: &str = "owner";
const PASSWORD: &str = "password123";

/// サービス・CLI が読む env を、この試験の一時の root に向ける（ほかの env は外す）。
fn point_env_at(root: &Path, bind: &str) {
    std::env::set_var("BANTO_HUB_ROOT", root);
    std::env::set_var("BANTO_HUB_PROFILE", PROFILE_ID);
    std::env::set_var("PORT", "0");
    std::env::set_var("BANTO_BIND", bind);
    for key in [
        "BANTO_DB",
        "BANTO_HUB_DATA",
        "BANTO_HUB_TLS_PORT",
        "BANTO_HUB_TLS_CERT",
        "BANTO_HUB_TLS_KEY",
    ] {
        std::env::remove_var(key);
    }
}

fn service_config() -> HubConfig {
    build_hub_config_from_env(HubHostKind::Service, common::hub_drivers())
}

/// bin の `create_admin` と同じ組み立て（`HubHostKind::Console`）。
fn cli_config() -> HubConfig {
    build_hub_config_from_env(HubHostKind::Console, common::hub_drivers())
}

const OWNER: NewAdmin<'static> = NewAdmin {
    username: ADMIN,
    password: PASSWORD,
    display_name: "オーナー",
};

/// 本物の TCP の上の HTTP/1.1 の 1 往復（`Connection: close`）。`(status, JSON の本文)`。
async fn http(
    addr: SocketAddr,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (u16, Value) {
    let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
    let body = body.map(|value| value.to_string()).unwrap_or_default();
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\
         X-Banto-Client: banto\r\nContent-Length: {}\r\n",
        body.len()
    );
    if !body.is_empty() {
        request.push_str("Content-Type: application/json\r\n");
    }
    if let Some(token) = token {
        request.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(&body);
    stream.write_all(request.as_bytes()).await.expect("write");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.expect("read");
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("end of the headers");
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let mut payload = raw[split + 4..].to_vec();
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .expect("status code");
    if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        payload = dechunk(&payload);
    }
    (
        status,
        serde_json::from_slice(&payload).unwrap_or(Value::Null),
    )
}

/// `Transfer-Encoding: chunked` の本文を 1 つにつなぐ。
fn dechunk(mut rest: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let Some(end) = rest.windows(2).position(|w| w == b"\r\n") else {
            return out;
        };
        let size_line = String::from_utf8_lossy(&rest[..end]).to_string();
        let size = usize::from_str_radix(size_line.split(';').next().unwrap_or("").trim(), 16)
            .unwrap_or(0);
        rest = &rest[end + 2..];
        if size == 0 || rest.len() < size {
            return out;
        }
        out.extend_from_slice(&rest[..size]);
        rest = rest.get(size + 2..).unwrap_or(&[]);
    }
}

async fn login(addr: SocketAddr, username: &str, password: &str) -> (u16, Value) {
    http(
        addr,
        "POST",
        "/api/auth/login",
        None,
        Some(json!({ "username": username, "password": password })),
    )
    .await
}

/// 資格情報なしでは、管理 REST・MCP・WebSocket の購読・ui-settings のどれも 401。
async fn assert_closed_without_credentials(addr: SocketAddr, when: &str) {
    for (method, path) in [
        ("GET", "/api/users"),
        ("GET", "/api/api-keys"),
        ("POST", "/api/plc-connections"),
        ("POST", "/api/commissioning/lock-down"),
        ("POST", "/mcp"),
        ("GET", "/api/v1/stream"),
        ("GET", "/api/tag-stream"),
        ("GET", "/api/ui-settings/theme"),
    ] {
        let body = (method == "POST").then(|| json!({}));
        let (status, reply) = http(addr, method, path, None, body).await;
        assert_eq!(status, 401, "{when}: {method} {path}: {reply}");
    }
    // 組み込みの `admin`（ADR-0034）はパスワードを持たず、ログインの口から入れない。
    // （banto のログインの失敗は `200 { success: false }`。）
    let (_, reply) = login(addr, "admin", PASSWORD).await;
    assert_ne!(
        reply["success"], true,
        "{when}: built-in admin logged in: {reply}"
    );
    assert!(reply["token"].is_null(), "{when}: {reply}");
}

async fn commissioning_available(addr: SocketAddr) -> Value {
    let (status, body) = http(addr, "GET", "/api/auth/status", None, None).await;
    assert_eq!(status, 200, "{body}");
    body["grants"]["commissioning"].clone()
}

async fn settings_row(pool: &sqlx::SqlitePool, key: &str) -> Option<String> {
    sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
        .expect("settings")
}

async fn count(pool: &sqlx::SqlitePool, sql: &'static str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.expect(sql)
}

async fn open_db(path: &str) -> sqlx::SqlitePool {
    sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(path))
        .await
        .expect("open the profile DB")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_db_stays_closed_to_the_network_until_create_admin_and_then_requires_login() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("root");

    // --- 1. create-admin の前 ---------------------------------------------------------
    // LAN の bind は起動を断る（DB は開いて migration を当てるが、待ち受けない）。
    point_env_at(&root, "0.0.0.0");
    let config = service_config();
    let db_path = config.db_path.clone();
    assert!(
        Path::new(&db_path).starts_with(&root),
        "the service DB lives in the profile: {db_path}"
    );
    match HubRuntime::start(config).await {
        Err(HubStartError::UnsafeCommissioningBind(message)) => {
            assert!(message.contains("create-admin"), "{message}");
        }
        Err(other) => panic!("expected UnsafeCommissioningBind, got: {other}"),
        Ok(hub) => {
            hub.shutdown().await;
            panic!("a new DB must not start on a LAN bind");
        }
    }

    // 新しい DB は 0015 で `auth.disabled = true`（ログインなし）・試運転モード。
    let pool = open_db(&db_path).await;
    assert_eq!(
        settings_row(&pool, "auth.disabled").await.as_deref(),
        Some("true")
    );
    assert_eq!(settings_row(&pool, "commissioning.locked_down").await, None);
    pool.close().await;

    // loopback の bind では起動する。
    point_env_at(&root, "127.0.0.1");
    let hub = HubRuntime::start(service_config())
        .await
        .expect("a new DB starts on a loopback bind");
    let addr = hub.local_addr();
    assert!(addr.ip().is_loopback(), "{addr}");
    assert_closed_without_credentials(addr, "before create-admin").await;

    // loopback の peer だけが試運転の grant を受け取る（本番の `AuthState`）。
    assert_eq!(commissioning_available(addr).await, true);
    let (status, body) = http(addr, "POST", "/api/auth/grant/commissioning", None, None).await;
    assert_eq!(status, 200, "{body}");
    let grant = body["token"].as_str().expect("grant token").to_string();
    let (status, me) = http(addr, "GET", "/api/auth/identity", Some(&grant), None).await;
    assert_eq!(status, 200, "{me}");
    assert_eq!(me["role"], "admin", "{me}");
    // §6 の 2: 試運転の grant は本番の経路で ui-settings を読み書きできる（持ち主は
    // `grant:commissioning`）。
    let (status, body) = http(
        addr,
        "PUT",
        "/api/ui-settings/theme",
        Some(&grant),
        Some(json!({ "value": "glass" })),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let (status, body) = http(addr, "GET", "/api/ui-settings/theme", Some(&grant), None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["value"], "glass", "{body}");

    // --- 2. サービスが動いている間の create-admin は何も書かない ------------------------
    let refused = run_create_admin(&cli_config(), OWNER)
        .await
        .expect_err("create-admin must not run while the service holds the profile");
    assert!(
        refused.to_string().contains("止めてから"),
        "the reason tells to stop the Hub: {refused}"
    );
    hub.shutdown().await;
    let pool = open_db(&db_path).await;
    assert_eq!(
        count(&pool, "SELECT COUNT(*) FROM users WHERE kind = 'account'").await,
        0
    );
    assert_eq!(settings_row(&pool, "commissioning.locked_down").await, None);
    assert_eq!(
        settings_row(&pool, "auth.disabled").await.as_deref(),
        Some("true")
    );
    assert_eq!(
        count(&pool, "SELECT COUNT(*) FROM audit_log WHERE origin = 'cli'").await,
        0
    );
    pool.close().await;

    // --- 3. 止めてから create-admin。以後はログイン必須 ---------------------------------
    let cli = cli_config();
    assert_eq!(cli.db_path, db_path, "the CLI resolves the service's DB");
    let created = run_create_admin(&cli, OWNER).await.expect("create-admin");
    assert_eq!(created.user.username, ADMIN);
    let pool = open_db(&db_path).await;
    assert_eq!(
        settings_row(&pool, "commissioning.locked_down")
            .await
            .as_deref(),
        Some("true")
    );
    assert_eq!(
        settings_row(&pool, "auth.disabled").await.as_deref(),
        Some("false")
    );
    pool.close().await;
    // ロックダウン済みなので、LAN の bind も起動の検査を通る（実際には待ち受けない。
    // モジュール doc）。
    banto_hub_core::commissioning::enforce_loopback_when_commissioning("0.0.0.0", true)
        .expect("a locked-down DB may start on a LAN bind");

    let hub = HubRuntime::start(service_config())
        .await
        .expect("the service starts after create-admin");
    let addr = hub.local_addr();
    assert_closed_without_credentials(addr, "after create-admin").await;
    // 試運転の grant は loopback からも閉じている。
    assert_eq!(commissioning_available(addr).await, false);
    let (status, body) = http(addr, "POST", "/api/auth/grant/commissioning", None, None).await;
    assert_eq!(status, 403, "{body}");
    // 作った管理者でログインできる。
    let (status, body) = login(addr, ADMIN, PASSWORD).await;
    assert_eq!(status, 200, "{body}");
    let token = body["token"].as_str().expect("token").to_string();
    let (status, me) = http(addr, "GET", "/api/auth/identity", Some(&token), None).await;
    assert_eq!(status, 200, "{me}");
    assert_eq!(me["role"], "admin", "{me}");
    assert_ne!(me["kind"], "commissioning", "{me}");
    let (status, body) = http(addr, "GET", "/api/users", Some(&token), None).await;
    assert_eq!(status, 200, "{body}");
    // 管理者のセッションの ui-settings は自分の行（`acct:<id>`）で、grant の値は見えない。
    let (status, body) = http(addr, "GET", "/api/ui-settings/theme", Some(&token), None).await;
    assert_eq!(status, 200, "{body}");
    assert!(body["value"].is_null(), "{body}");
    hub.shutdown().await;

    for key in ["BANTO_HUB_ROOT", "BANTO_HUB_PROFILE", "PORT", "BANTO_BIND"] {
        std::env::remove_var(key);
    }
}
