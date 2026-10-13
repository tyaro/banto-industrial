//! #392 B2-1: この PC からの接続（`localDesktop` の grant、`crate::local_grant`）と、
//! 設定の REST（`/api/settings/*`、`crate::server_admin`）のルーターの上の振る舞い。
//!
//! peer は `ConnectInfo` を要求に入れて決め、Host は `Host` ヘッダーで決める（banto の
//! 本物の serve と同じ形 - `banto_server::request_is_loopback_local` が見るもの）。
//! 本物の TCP の上の確認（listener ごとの開け閉め・LAN の入れ替え）は
//! `crate::engine` のテストにある。
//!
//! 反証（コミット本文）: `local_access_middleware` の `ConnectInfo` の取り外しを
//! やめると、`a_loopback_peer_with_a_foreign_host_is_refused` が落ちる（Host が
//! `evil.example` でも発行される）。

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use axum::Router;
use banto_core::ListParams;
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tower::ServiceExt;

use super::{api_router, HostAccess};
use crate::audit::AuditLogService;
use crate::backup::BackupService;
use crate::collect::CollectorService;
use crate::db::{migrate_memory, Db};
use crate::display_groups::DisplayGroupService;
use crate::local_grant::{
    write_setting, LocalAccess, LocalGrantSetting, LocalGrantStart, LOCAL_GRANT_MAX_SESSIONS,
};
use crate::server_admin::LanControl;
use crate::settings::SettingsService;
use crate::users::{Role, UsersService};
use crate::{CollectionGroupService, PlcConnectionService, TagService};

const LOCAL: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 50_000);
const LAN: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20)), 50_000);
const LOCAL_HOST: &str = "127.0.0.1:8721";
const PASSWORD: &str = "password123";

struct Fixture {
    router: Router,
    users: UsersService,
    settings: SettingsService,
    audit: AuditLogService,
}

async fn fixture(start: LocalGrantStart, loopback_listener: bool, accounts: &[Role]) -> Fixture {
    fixture_with(start, loopback_listener, accounts, None).await
}

/// `accounts` を先に作り（最初は setup で admin）、`setting` を保存してからルーターを組む
/// （エンジンが起動時に決める `start` と DB の状態を揃える）。
async fn fixture_with(
    start: LocalGrantStart,
    loopback_listener: bool,
    accounts: &[Role],
    setting: Option<LocalGrantSetting>,
) -> Fixture {
    let pool = migrate_memory().await.expect("migrate_memory");
    let db = Db::Sqlite(pool.clone());
    let users = UsersService::new(db.clone());
    let settings = SettingsService::new(db.clone());
    let audit = AuditLogService::new(db.clone());
    for (i, role) in accounts.iter().enumerate() {
        let name = role.as_str();
        if i == 0 {
            assert_eq!(*role, Role::Admin, "最初のアカウントは admin");
            users
                .setup_first_user(name, PASSWORD, name)
                .await
                .expect("setup_first_user");
        } else {
            users
                .create_user(name, PASSWORD, name, *role)
                .await
                .expect("create_user");
        }
    }
    if let Some(setting) = setting {
        write_setting(&settings, setting).await.expect("write");
    }
    let hub =
        crate::hub::HubService::new(settings.clone(), Arc::new(crate::hub::UnavailableKeyStore))
            .await
            .expect("HubService::new");
    let auth = super::user_auth_state(users.clone(), audit.clone());
    let (tx, _rx) = broadcast::channel(16);
    let host = HostAccess {
        local: LocalAccess {
            start,
            loopback_listener,
        },
        lan: LanControl::detached(),
        ..HostAccess::closed()
    };
    let router = api_router(
        users.clone(),
        settings.clone(),
        audit.clone(),
        BackupService::new(PathBuf::from("unused-in-tests").join("db.sqlite3"), db),
        hub,
        PlcConnectionService::new(pool.clone()),
        CollectionGroupService::new(pool.clone()),
        TagService::new(pool.clone()),
        CollectorService::new(pool.clone(), PathBuf::from("unused-in-tests")),
        DisplayGroupService::new(pool),
        auth,
        tx,
        loopback_listener,
        host,
    );
    Fixture {
        router,
        users,
        settings,
        audit,
    }
}

/// 1 つの要求。`peer` は `ConnectInfo`、`host` は `Host` ヘッダー。
async fn send(
    router: &Router,
    method: &str,
    path: &str,
    peer: Option<SocketAddr>,
    host: Option<&str>,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("X-Banto-Client", "banto");
    if let Some(host) = host {
        builder = builder.header("Host", host);
    }
    if let Some(token) = token {
        builder = builder.header("Authorization", format!("Bearer {token}"));
    }
    let body = match body {
        Some(value) => {
            builder = builder.header("Content-Type", "application/json");
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };
    let mut request = builder.body(body).unwrap();
    if let Some(peer) = peer {
        request.extensions_mut().insert(ConnectInfo(peer));
    }
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

async fn status_grant(router: &Router, peer: Option<SocketAddr>, host: Option<&str>) -> Value {
    let (status, body) = send(router, "GET", "/api/auth/status", peer, host, None, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["grants"]["localDesktop"].clone()
}

async fn grant(
    router: &Router,
    peer: Option<SocketAddr>,
    host: Option<&str>,
) -> (StatusCode, Option<String>) {
    let (status, body) = send(
        router,
        "POST",
        "/api/auth/grant/localDesktop",
        peer,
        host,
        None,
        None,
    )
    .await;
    (status, body["token"].as_str().map(str::to_owned))
}

async fn login(router: &Router, username: &str) -> String {
    let (status, body) = send(
        router,
        "POST",
        "/api/auth/login",
        Some(LOCAL),
        Some(LOCAL_HOST),
        None,
        Some(json!({ "username": username, "password": PASSWORD })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["token"].as_str().expect("token").to_string()
}

async fn identity(router: &Router, token: &str) -> (StatusCode, Value) {
    send(
        router,
        "GET",
        "/api/auth/identity",
        Some(LOCAL),
        Some(LOCAL_HOST),
        Some(token),
        None,
    )
    .await
}

async fn grant_logins(audit: &AuditLogService) -> usize {
    audit
        .list(ListParams::default())
        .await
        .unwrap()
        .rows
        .into_iter()
        .filter(|row| {
            row.action == "login"
                && row.actor_username.as_deref() == Some("local")
                && row
                    .detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains("localGrant"))
        })
        .count()
}

/// 未初期化: peer と Host の両方が loopback なら admin で発行し、監査に
/// `login`（`via: localGrant`）を残す。最初の管理者を作ったら、発行済みのトークンは
/// 失効し、新しい発行は閉じる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uninitialized_issues_admin_until_the_first_account_exists() {
    let f = fixture(LocalGrantStart::Uninitialized, true, &[]).await;

    assert_eq!(
        status_grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await,
        true
    );
    let (status, token) = grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await;
    assert_eq!(status, StatusCode::OK);
    let token = token.expect("token");
    let (status, me) = identity(&f.router, &token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["id"], "local", "{me}");
    assert_eq!(me["role"], "admin", "{me}");
    assert_eq!(me["kind"], "localDesktop", "{me}");
    assert_eq!(
        grant_logins(&f.audit).await,
        1,
        "監査に via: localGrant が無い"
    );

    // admin の口に入れる。
    let (status, body) = send(
        &f.router,
        "GET",
        "/api/settings/auth",
        Some(LOCAL),
        Some(LOCAL_HOST),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["localGrant"], "off", "{body}");
    assert_eq!(body["localGrantSupported"], true, "{body}");

    // 最初のアカウントを grant から作らせない（admin のいない状態を防ぐ）。
    let (status, body) = send(
        &f.router,
        "POST",
        "/api/users",
        Some(LOCAL),
        Some(LOCAL_HOST),
        Some(&token),
        Some(json!({ "username": "op", "password": PASSWORD, "displayName": "op", "role": "viewer" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(!f.users.is_initialized().await.unwrap());

    // setup（Host の確認つき）で最初の管理者を作ると、grant の admin は終わる。
    let setup = json!({ "username": "admin", "password": PASSWORD, "displayName": "管理者" });
    let (status, body) = send(
        &f.router,
        "POST",
        "/api/auth/setup",
        Some(LOCAL),
        Some("evil.example:8721"),
        None,
        Some(setup.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "Host の確認が無い: {body}");
    let (status, body) = send(
        &f.router,
        "POST",
        "/api/auth/setup",
        Some(LOCAL),
        Some(LOCAL_HOST),
        None,
        Some(setup),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["success"], true, "{body}");

    let (status, _) = identity(&f.router, &token).await;
    assert_eq!(status, StatusCode::OK);
    let (_, me) = identity(&f.router, &token).await;
    assert!(
        me.is_null(),
        "未初期化の間の admin のトークンが残っている: {me}"
    );
    assert_eq!(
        status_grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await,
        false
    );
    let (status, _) = grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// 非 loopback の peer・peer 不明は、status も発行も閉じる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_non_loopback_peer_is_refused() {
    let f = fixture(LocalGrantStart::Uninitialized, true, &[]).await;
    for peer in [Some(LAN), None] {
        assert_eq!(status_grant(&f.router, peer, Some(LOCAL_HOST)).await, false);
        let (status, token) = grant(&f.router, peer, Some(LOCAL_HOST)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{peer:?}");
        assert!(token.is_none());
    }
    assert_eq!(grant_logins(&f.audit).await, 0);
}

/// loopback の peer でも、Host が loopback でなければ閉じる（DNS rebinding 対策）。
/// `localhost`・`[::1]` はこの PC の名前として通す。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_loopback_peer_with_a_foreign_host_is_refused() {
    let f = fixture(LocalGrantStart::Uninitialized, true, &[]).await;
    for host in [Some("evil.example:8721"), Some("192.168.1.5:8721"), None] {
        assert_eq!(
            status_grant(&f.router, Some(LOCAL), host).await,
            false,
            "{host:?}"
        );
        let (status, _) = grant(&f.router, Some(LOCAL), host).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{host:?}");
    }
    for host in ["localhost:8721", "[::1]:8721", "127.0.0.1"] {
        let (status, _) = grant(&f.router, Some(LOCAL), Some(host)).await;
        assert_eq!(status, StatusCode::OK, "{host}");
    }
    // 閲覧公開は peer を見ないので、外した `ConnectInfo` の影響を受けない（OFF なので
    // `false` のまま、項目はある）。
    let (_, body) = send(
        &f.router,
        "GET",
        "/api/auth/status",
        Some(LOCAL),
        Some("evil.example"),
        None,
        None,
    )
    .await;
    assert_eq!(body["grants"]["publicViewer"], false, "{body}");
}

/// LAN の listener のルーター（`loopback_listener: false`）は、peer と Host が
/// loopback でも発行しない（`404` ではなく `403`、status は `false`）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lan_listener_never_issues() {
    let f = fixture(LocalGrantStart::Uninitialized, false, &[]).await;
    assert_eq!(
        status_grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await,
        false
    );
    let (status, _) = grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // デスクトップ・banto-serve（Unsupported）も同じ。
    let f = fixture(LocalGrantStart::Unsupported, true, &[]).await;
    let (status, _) = grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// 初期化後: 設定 `off`（既定）は `403`。viewer・editor・admin はその role で発行する。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initialized_issues_the_configured_role() {
    let f = fixture(
        LocalGrantStart::Initialized(LocalGrantSetting::Off),
        true,
        &[Role::Admin],
    )
    .await;
    assert_eq!(
        status_grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await,
        false
    );
    let (status, _) = grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    for role in [Role::Viewer, Role::Editor, Role::Admin] {
        let setting = LocalGrantSetting::Role(role);
        let f = fixture_with(
            LocalGrantStart::Initialized(setting),
            true,
            &[Role::Admin],
            Some(setting),
        )
        .await;
        let (status, token) = grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await;
        assert_eq!(status, StatusCode::OK, "{role:?}");
        let (_, me) = identity(&f.router, &token.unwrap()).await;
        assert_eq!(me["role"], role.as_str(), "{me}");
        assert_eq!(me["id"], "local", "{me}");
    }
}

/// 設定を変えたら、発行済みのトークンはすぐ失効し、新しい role での発行は再起動から
/// （`localGrantRestartRequired`）。変更は admin だけ、ログイン不要モードの欄は断る。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changing_the_setting_closes_now_and_reopens_after_a_restart() {
    let viewer = LocalGrantSetting::Role(Role::Viewer);
    let f = fixture_with(
        LocalGrantStart::Initialized(viewer),
        true,
        &[Role::Admin, Role::Editor],
        Some(viewer),
    )
    .await;
    let admin = login(&f.router, "admin").await;
    let editor = login(&f.router, "editor").await;
    let (_, local_token) = grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await;
    let local_token = local_token.expect("token");

    // editor は変えられない（`denied` が残る）。
    let put = |token: String, body: Value| {
        let router = f.router.clone();
        async move {
            send(
                &router,
                "PUT",
                "/api/settings/auth",
                Some(LOCAL),
                Some(LOCAL_HOST),
                Some(&token),
                Some(body),
            )
            .await
        }
    };
    let (status, _) = put(editor, json!({ "localGrant": "admin" })).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = put(admin.clone(), json!({ "localGrant": "root" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = put(
        admin.clone(),
        json!({ "localGrant": "viewer", "disabled": true }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // 同じ値の保存はセッションを切らない。
    let (status, body) = put(admin.clone(), json!({ "localGrant": "viewer" })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["localGrantRestartRequired"], false, "{body}");
    assert!(!identity(&f.router, &local_token).await.1.is_null());

    let (status, body) = put(admin.clone(), json!({ "localGrant": "admin" })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["localGrant"], "admin", "{body}");
    assert_eq!(body["localGrantRestartRequired"], true, "{body}");
    assert!(
        identity(&f.router, &local_token).await.1.is_null(),
        "発行済みのトークンが失効していない"
    );
    assert_eq!(
        status_grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await,
        false
    );
    let (status, _) = grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "再起動の前に新しい role で発行した"
    );

    let changes = f
        .audit
        .list(ListParams::default())
        .await
        .unwrap()
        .rows
        .into_iter()
        .filter(|row| row.action == "settings_change" && row.origin == "rest")
        .collect::<Vec<_>>();
    assert_eq!(changes.len(), 2, "{changes:?}");
    assert!(
        changes.iter().any(|row| row
            .detail
            .as_deref()
            .is_some_and(|d| d.contains("\"revokedGrants\":1"))),
        "{changes:?}"
    );
    let saved = crate::local_grant::read_setting(&f.settings).await.unwrap();
    assert_eq!(saved, LocalGrantSetting::Role(Role::Admin));
}

/// 発行の上限（`max_sessions`）を超えたら、最古のトークンから失効する。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn issuance_past_the_cap_evicts_the_oldest() {
    let f = fixture(LocalGrantStart::Uninitialized, true, &[]).await;
    let mut tokens = Vec::new();
    for _ in 0..=LOCAL_GRANT_MAX_SESSIONS {
        let (status, token) = grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await;
        assert_eq!(status, StatusCode::OK);
        tokens.push(token.unwrap());
    }
    assert!(
        identity(&f.router, &tokens[0]).await.1.is_null(),
        "最古のトークンが残っている"
    );
    assert!(!identity(&f.router, &tokens[1]).await.1.is_null());
    assert!(!identity(&f.router, tokens.last().unwrap())
        .await
        .1
        .is_null());
}

/// `/api/settings/server`: admin だけが読める。入れ替えられない host（ここでは
/// エンジンに繋がっていないルーター = デスクトップ・`banto-serve` と同じ）の `PUT` は断る。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_settings_are_admin_only_and_put_needs_a_server_host() {
    let f = fixture(
        LocalGrantStart::Unsupported,
        false,
        &[Role::Admin, Role::Viewer],
    )
    .await;
    let admin = login(&f.router, "admin").await;
    let viewer = login(&f.router, "viewer").await;
    let get = |token: String| {
        let router = f.router.clone();
        async move {
            send(
                &router,
                "GET",
                "/api/settings/server",
                Some(LOCAL),
                Some(LOCAL_HOST),
                Some(&token),
                None,
            )
            .await
        }
    };
    let (status, _) = get(viewer).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = get(admin.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["running"], false, "{body}");
    assert_eq!(body["restartRequired"], false, "{body}");
    assert_eq!(body["port"], 8721, "{body}");

    let (status, body) = send(
        &f.router,
        "PUT",
        "/api/settings/server",
        Some(LOCAL),
        Some(LOCAL_HOST),
        Some(&admin),
        Some(json!({ "enabled": false, "bind": "127.0.0.1", "port": 8721, "viewerPublic": false })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, body) = send(
        &f.router,
        "GET",
        "/api/settings/server",
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
}
