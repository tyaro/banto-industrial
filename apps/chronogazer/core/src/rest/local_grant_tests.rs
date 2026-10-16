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
    finish_setup, write_setting, LocalAccess, LocalAccessState, LocalGrantSetting, LocalGrantStart,
    LOCAL_GRANT_MAX_SESSIONS,
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
    auth: banto_server::AuthState,
    /// REST の設定の書き込みの lock（`HostAccess::settings_lock`。外から握って処理を
    /// 決定的に止めるテスト用）。
    settings_lock: Arc<tokio::sync::Mutex<()>>,
}

async fn fixture(start: LocalGrantStart, always_on_listener: bool, accounts: &[Role]) -> Fixture {
    fixture_with(start, always_on_listener, accounts, None).await
}

/// `accounts` を先に作り（最初は setup で admin）、`setting` を保存してからルーターを組む
/// （エンジンが起動時に決める `start` と DB の状態を揃える）。
async fn fixture_with(
    start: LocalGrantStart,
    always_on_listener: bool,
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
    let settings_lock = Arc::new(tokio::sync::Mutex::new(()));
    let host = HostAccess {
        local: LocalAccess {
            start,
            always_on_listener,
        },
        lan: LanControl::detached(),
        settings_lock: settings_lock.clone(),
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
        auth.clone(),
        tx,
        always_on_listener,
        host,
    );
    Fixture {
        router,
        users,
        settings,
        audit,
        auth,
        settings_lock,
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
                && row.actor_username.as_deref() == Some("localDesktop")
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
    assert_eq!(me["id"], "localDesktop", "{me}");
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

/// LAN の listener のルーター（`always_on_listener: false`）は、peer と Host が
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
        assert_eq!(me["id"], "localDesktop", "{me}");
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

    // 変更はこの PC からの要求だけ（2026-10-14 オーナー決定）。LAN の peer・この PC 以外の
    // Host からは admin でも `403` で、保存は変わらない。
    for (peer, host) in [
        (Some(LAN), Some(LOCAL_HOST)),
        (Some(LOCAL), Some("evil.example")),
    ] {
        let (status, body) = send(
            &f.router,
            "PUT",
            "/api/settings/auth",
            peer,
            host,
            Some(&admin),
            Some(json!({ "localGrant": "off" })),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{peer:?} {host:?}: {body}");
    }
    let saved = crate::local_grant::read_setting(&f.settings).await.unwrap();
    assert_eq!(saved, LocalGrantSetting::Role(Role::Admin));
}

/// 常時の listener（`0.0.0.0` にまとめた 1 本を含む）でも、最初の管理者の作成は要求ごと:
/// LAN の peer・この PC 以外の Host は `403`、この PC からは作れる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn setup_on_the_always_on_listener_needs_a_request_from_this_pc() {
    let f = fixture(LocalGrantStart::Uninitialized, true, &[]).await;
    let setup = json!({ "username": "admin", "password": PASSWORD, "displayName": "管理者" });
    for (peer, host) in [
        (Some(LAN), Some(LOCAL_HOST)),
        (Some(LAN), Some("192.168.1.5:8721")),
        (Some(LOCAL), Some("evil.example:8721")),
        (None, Some(LOCAL_HOST)),
    ] {
        let (status, body) = send(
            &f.router,
            "POST",
            "/api/auth/setup",
            peer,
            host,
            None,
            Some(setup.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{peer:?} {host:?}: {body}");
    }
    assert!(!f.users.is_initialized().await.unwrap());
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

/// #600 のレビュー: `localDesktop`（admin）のトークンで `localGrant` を変えると、失効が
/// 要求自身のトークンも消すが、監査の操作者は失効の前に確定しているので残る。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_grant_session_changing_the_setting_is_audited_with_its_actor() {
    let admin = LocalGrantSetting::Role(Role::Admin);
    let f = fixture_with(
        LocalGrantStart::Initialized(admin),
        true,
        &[Role::Admin],
        Some(admin),
    )
    .await;
    let (_, token) = grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await;
    let token = token.expect("token");
    let (status, body) = send(
        &f.router,
        "PUT",
        "/api/settings/auth",
        Some(LOCAL),
        Some(LOCAL_HOST),
        Some(&token),
        Some(json!({ "localGrant": "viewer" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        identity(&f.router, &token).await.1.is_null(),
        "自分のトークンが失効していない"
    );
    let changes = f
        .audit
        .list(ListParams::default())
        .await
        .unwrap()
        .rows
        .into_iter()
        .filter(|row| row.action == "settings_change")
        .collect::<Vec<_>>();
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!(changes[0].actor_username.as_deref(), Some("localDesktop"));
    assert_eq!(changes[0].actor_role.as_deref(), Some("admin"));
}

/// #600 のレビュー: ログイン不要モードの欄は、値が `null` でも欄があれば `400`
/// （保存は変わらない）。欄が無ければ通る。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auth_disabled_fields_are_refused_even_when_null() {
    let f = fixture(
        LocalGrantStart::Initialized(LocalGrantSetting::Off),
        true,
        &[Role::Admin],
    )
    .await;
    let admin = login(&f.router, "admin").await;
    for body in [
        json!({ "localGrant": "viewer", "disabled": null }),
        json!({ "localGrant": "viewer", "disabledRole": null }),
        json!({ "localGrant": "viewer", "disabled": false }),
        json!({ "localGrant": "viewer", "disabledRole": "admin" }),
    ] {
        let (status, response) = send(
            &f.router,
            "PUT",
            "/api/settings/auth",
            Some(LOCAL),
            Some(LOCAL_HOST),
            Some(&admin),
            Some(body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {response}");
    }
    assert_eq!(
        crate::local_grant::read_setting(&f.settings).await.unwrap(),
        LocalGrantSetting::Off
    );
    let (status, response) = send(
        &f.router,
        "PUT",
        "/api/settings/auth",
        Some(LOCAL),
        Some(LOCAL_HOST),
        Some(&admin),
        Some(json!({ "localGrant": "viewer" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
}

/// #600 のオーナーレビュー P2: kind を percent encode した別表記
/// （`local%44esktop`・`%6CocalDesktop`）でも、banto の発行口は decode して発行する。
/// 発行と `login`（`via: localGrant`）の監査は 1 対 1 で、別表記でも LAN の peer・別の
/// Host は `403` のまま。接頭辞・status・setup を別表記にした要求はどのルートにも
/// 当たらない（axum は生のパスで振り分ける。`404` か外側の認証の層の `401`）ので、
/// 確認を素通りしない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn encoded_kind_spellings_are_audited_and_still_checked() {
    let f = fixture(LocalGrantStart::Uninitialized, true, &[]).await;
    let spellings = [
        "/api/auth/grant/localDesktop",
        "/api/auth/grant/local%44esktop",
        "/api/auth/grant/%6CocalDesktop",
        "/api/auth/grant/%6c%6f%63%61%6c%44%65%73%6b%74%6f%70",
    ];
    for path in spellings {
        for (peer, host) in [
            (Some(LAN), Some(LOCAL_HOST)),
            (Some(LOCAL), Some("evil.example")),
        ] {
            let (status, body) = send(&f.router, "POST", path, peer, host, None, None).await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "{path} {peer:?} {host:?}: {body}"
            );
        }
    }
    assert_eq!(grant_logins(&f.audit).await, 0);
    for (i, path) in spellings.iter().enumerate() {
        let (status, body) = send(
            &f.router,
            "POST",
            path,
            Some(LOCAL),
            Some(LOCAL_HOST),
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
        let token = body["token"].as_str().expect("token");
        let (_, me) = identity(&f.router, token).await;
        assert_eq!(me["kind"], "localDesktop", "{path}: {me}");
        assert_eq!(
            grant_logins(&f.audit).await,
            i + 1,
            "{path} の発行が監査に残らない"
        );
    }

    // 別表記の接頭辞・status・setup は、LAN から送っても何も開けない。
    let setup = json!({ "username": "admin", "password": PASSWORD, "displayName": "管理者" });
    for (method, path, body) in [
        ("POST", "/api/auth/gr%61nt/localDesktop", None),
        ("GET", "/api/auth/st%61tus", None),
        ("POST", "/api/auth/s%65tup", Some(setup.clone())),
        ("POST", "/api/auth/setup/", Some(setup)),
    ] {
        let (status, response) = send(
            &f.router,
            method,
            path,
            Some(LAN),
            Some(LOCAL_HOST),
            None,
            body,
        )
        .await;
        // どのルートにも当たらない（`404`、または外側の認証の層の `401`）。
        assert!(
            status == StatusCode::NOT_FOUND || status == StatusCode::UNAUTHORIZED,
            "{path}: {status} {response}"
        );
        assert!(response.get("token").is_none(), "{path}: {response}");
        assert!(response.get("grants").is_none(), "{path}: {response}");
    }
    assert!(!f.users.is_initialized().await.unwrap());
}

/// #600 のレビュー: 合成の identity と同じアカウント名は、REST の setup と
/// `POST /api/users` で欄の誤り（`422`、`username`）になり、アカウントを作らない。
/// 普通の名前は通る。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reserved_usernames_are_refused_on_the_rest_paths() {
    let f = fixture(LocalGrantStart::Uninitialized, true, &[]).await;
    for name in ["localDesktop", " Local ", "PUBLIC"] {
        let (status, body) = send(
            &f.router,
            "POST",
            "/api/auth/setup",
            Some(LOCAL),
            Some(LOCAL_HOST),
            None,
            Some(json!({ "username": name, "password": PASSWORD, "displayName": "x" })),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{name}: {body}");
        assert_eq!(body["field_errors"][0]["field"], "username", "{body}");
    }
    assert!(!f.users.is_initialized().await.unwrap());

    let (status, body) = send(
        &f.router,
        "POST",
        "/api/auth/setup",
        Some(LOCAL),
        Some(LOCAL_HOST),
        None,
        Some(json!({ "username": "admin", "password": PASSWORD, "displayName": "管理者" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let admin = body["token"].as_str().expect("token").to_string();
    for name in ["localdesktop", "local", "Public "] {
        let (status, body) = send(
            &f.router,
            "POST",
            "/api/users",
            Some(LOCAL),
            Some(LOCAL_HOST),
            Some(&admin),
            Some(json!({ "username": name, "password": PASSWORD, "displayName": "x", "role": "viewer" })),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{name}: {body}");
    }
    let (status, body) = send(
        &f.router,
        "POST",
        "/api/users",
        Some(LOCAL),
        Some(LOCAL_HOST),
        Some(&admin),
        Some(json!({ "username": "operator", "password": PASSWORD, "displayName": "運転員", "role": "viewer" })),
    )
    .await;
    assert!(status.is_success(), "{status}: {body}");
    assert_eq!(f.users.list_users().await.unwrap().len(), 2);
}

/// #600 の監査 F1: setup の後の失効は DB を読み直さない。setup が commit した後に
/// アカウントの有無を読めなくても（ここでは閉じた pool の `UsersService` を渡す）、本文が
/// `success: true` なら未初期化の間の admin のトークンを失効させ、監査に残す。
/// `success: false` なら失効しない。応答はそのまま返す。
///
/// 反証: `finish_setup` に `state.users.is_initialized()` の読み直し（`Ok(true)` でなければ
/// 何もしない）を戻すと、このテストが「トークンが残っている」で落ちる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn setup_revokes_even_if_the_db_cannot_be_read_again() {
    use axum::response::IntoResponse;
    let f = fixture(LocalGrantStart::Uninitialized, true, &[]).await;
    let (_, token) = grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await;
    let token = token.expect("token");

    let closed = migrate_memory().await.expect("migrate_memory");
    closed.close().await;
    let unreadable = UsersService::new(Db::Sqlite(closed));
    assert!(unreadable.is_initialized().await.is_err());
    let state = LocalAccessState {
        access: LocalAccess {
            start: LocalGrantStart::Uninitialized,
            always_on_listener: true,
        },
        users: unreadable,
        auth: f.auth.clone(),
        audit: f.audit.clone(),
        settings: f.settings.clone(),
        settings_lock: Arc::new(tokio::sync::Mutex::new(())),
    };
    let revocations = || async {
        f.audit
            .list(ListParams::default())
            .await
            .unwrap()
            .rows
            .into_iter()
            .filter(|row| row.action == "revoke_grants")
            .count()
    };

    let response = finish_setup(
        &state,
        (StatusCode::OK, axum::Json(json!({ "success": false }))).into_response(),
        false,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!identity(&f.router, &token).await.1.is_null());
    assert_eq!(revocations().await, 0);

    let response = finish_setup(
        &state,
        (
            StatusCode::OK,
            axum::Json(json!({ "success": true, "token": "new-admin" })),
        )
            .into_response(),
        false,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["token"], "new-admin", "応答の本文が変わった: {body}");
    assert!(
        identity(&f.router, &token).await.1.is_null(),
        "DB を読めないときに未初期化の間の admin のトークンが残っている"
    );
    assert_eq!(revocations().await, 1);
}

/// #600 の監査 F3: JSON の配列の本文（serde の struct の配列の表現）は、予約名でも
/// 普通の名前でも、setup・`POST /api/users` の両方で `400` になり、アカウントを作らない
/// （上流は配列の表現も受理するので、`username` の欄だけを見ると予約名を素通りした）。
///
/// 反証: `local_access_middleware` の「object 以外は `400`」をやめると、予約名の配列の
/// 本文の setup が `400` にならず（上流が受理する）このテストが落ちる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn array_bodies_are_refused_on_the_account_paths() {
    let f = fixture(LocalGrantStart::Uninitialized, true, &[]).await;
    for body in [
        json!(["localDesktop", PASSWORD, "x"]),
        json!(["admin", PASSWORD, "管理者"]),
    ] {
        let (status, response) = send(
            &f.router,
            "POST",
            "/api/auth/setup",
            Some(LOCAL),
            Some(LOCAL_HOST),
            None,
            Some(body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {response}");
    }
    assert!(!f.users.is_initialized().await.unwrap());

    let (status, body) = send(
        &f.router,
        "POST",
        "/api/auth/setup",
        Some(LOCAL),
        Some(LOCAL_HOST),
        None,
        Some(json!({ "username": "admin", "password": PASSWORD, "displayName": "管理者" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let admin = body["token"].as_str().expect("token").to_string();
    for body in [
        json!(["PUBLIC", PASSWORD, "x", "viewer"]),
        json!(["operator", PASSWORD, "運転員", "viewer"]),
    ] {
        let (status, response) = send(
            &f.router,
            "POST",
            "/api/users",
            Some(LOCAL),
            Some(LOCAL_HOST),
            Some(&admin),
            Some(body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {response}");
    }
    assert_eq!(f.users.list_users().await.unwrap().len(), 1);
}

/// 生の本文を送る（`send` は `Value` を `to_string` するので、`serde_json::Value` が読めない
/// 本文 - 深すぎる入れ子など - を作れない）。
async fn send_raw(
    router: &Router,
    path: &str,
    token: Option<&str>,
    body: String,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(path)
        .header("X-Banto-Client", "banto")
        .header("Host", LOCAL_HOST)
        .header("Content-Type", "application/json");
    if let Some(token) = token {
        builder = builder.header("Authorization", format!("Bearer {token}"));
    }
    let mut request = builder.body(Body::from(body)).unwrap();
    request.extensions_mut().insert(ConnectInfo(LOCAL));
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// #600 のオーナーレビュー P2: `serde_json::Value` の読み手には再帰の上限（128）があるが、
/// 上流の `SetupRequest`・`CreateUserRequest` は知らない欄を `IgnoredAny` で読み飛ばすので
/// その上限に当たらない。知らない欄を 130 段の配列にした本文は「JSON として読めない」
/// になり、上流に素通りして予約名のアカウントができていた。読めない本文は `400` で断る
/// （fail closed）。setup・`POST /api/users` の両方で、アカウントの数は増えない。
///
/// 反証: `local_access_middleware` の `Err(_)` を素通り（`{}`）に戻すと、setup が
/// 成功して `localDesktop` のアカウントができ、このテストが落ちる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deeply_nested_bodies_cannot_bypass_the_reserved_name_check() {
    let nested = format!("{}0{}", "[".repeat(130), "]".repeat(130));
    assert!(
        serde_json::from_str::<Value>(&nested).is_err(),
        "入れ子が Value の再帰の上限を超えていない"
    );
    let f = fixture(LocalGrantStart::Uninitialized, true, &[]).await;
    let setup = format!(
        r#"{{"username":"localDesktop","password":"{PASSWORD}","displayName":"x","unused":{nested}}}"#
    );
    let (status, response) = send_raw(&f.router, "/api/auth/setup", None, setup).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
    assert!(!f.users.is_initialized().await.unwrap());

    let f = fixture(
        LocalGrantStart::Initialized(LocalGrantSetting::Off),
        true,
        &[Role::Admin],
    )
    .await;
    let admin = login(&f.router, "admin").await;
    let create = format!(
        r#"{{"username":"localDesktop","password":"{PASSWORD}","displayName":"x","role":"viewer","unused":{nested}}}"#
    );
    let (status, response) = send_raw(&f.router, "/api/users", Some(&admin), create).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
    assert_eq!(f.users.list_users().await.unwrap().len(), 1);
}

/// #600 の Copilot レビューで「本文を待つ間に grant が失効しても、認可済みの変更は操作者
/// つきで通す」と決めていたが、#392 B2-2 のレビュー（Copilot）で改めた: 認可は lock の
/// 手前で済むので、待つ間に失効したトークンで変更を通すと、setup が保存した `off` を
/// 書き戻せる。lock を取った後に認可を確かめ直し、無効なら何もせず `401`（設定は変わらず、
/// `settings_change` も残らない）。本文の最初の poll の後（= 認可の層を通った後）に
/// 直接の失効で作る。操作者を本文の前に確定する扱い（自分の変更で自分のトークンが
/// 失効する場合）は `a_grant_session_changing_the_setting_is_audited_with_its_actor` が守る。
///
/// 反証: `auth_settings_apply` の lock の下の再確認を外すと、`200` で設定が viewer になって
/// このテストが落ちる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_token_revoked_while_the_body_arrives_is_refused_under_the_lock() {
    let admin = LocalGrantSetting::Role(Role::Admin);
    let f = fixture_with(
        LocalGrantStart::Initialized(admin),
        true,
        &[Role::Admin],
        Some(admin),
    )
    .await;
    let (_, token) = grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await;
    let token = token.expect("token");

    // 最初に poll されたら知らせ、`release` が来るまで本文を出さない。
    let (polled_tx, polled_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let state = Some((polled_tx, release_rx));
    let stream = futures_util::stream::unfold(state, |state| async move {
        let (polled_tx, release_rx) = state?;
        let _ = polled_tx.send(());
        let _ = release_rx.await;
        Some((
            Ok::<_, std::io::Error>(axum::body::Bytes::from(
                json!({ "localGrant": "viewer" }).to_string(),
            )),
            None,
        ))
    });
    let mut request = Request::builder()
        .method("PUT")
        .uri("/api/settings/auth")
        .header("X-Banto-Client", "banto")
        .header("Host", LOCAL_HOST)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {token}"))
        .body(Body::from_stream(stream))
        .unwrap();
    request.extensions_mut().insert(ConnectInfo(LOCAL));
    let router = f.router.clone();
    let pending = tokio::spawn(async move { router.oneshot(request).await.unwrap() });

    polled_rx.await.expect("本文が読まれなかった");
    assert_eq!(
        f.auth
            .revoke_grant_tokens(&crate::local_grant::local_desktop_grant_kind()),
        1
    );
    release_tx.send(()).unwrap();
    let response = pending.await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    assert_eq!(
        crate::local_grant::read_setting(&f.settings).await.unwrap(),
        admin,
        "失効したトークンで設定が変わった"
    );
    let changes = f
        .audit
        .list(ListParams::default())
        .await
        .unwrap()
        .rows
        .into_iter()
        .filter(|row| row.action == "settings_change")
        .count();
    assert_eq!(changes, 0);
}

/// #600 の Copilot レビュー: 予約名（ここでは `localDesktop`）のアカウントがある DB
/// （予約名の検査より前に作られたもの。ここでは REST の前段を通さずに直接作る）では、
/// 初期化後の `localDesktop` を発行しない（fail closed）。status は `false`、発行は
/// 説明つきの `409` で、トークンも監査の `login` も無い（status と発行が食い違わない）。
/// 別表記の kind でも同じ。予約名のアカウントを消せば、再起動なしで開く。
///
/// 反証: `local_desktop_grant_spec` の条件から `has_reserved_accounts` を外すと、status が
/// `true` になってこのテストが落ちる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reserved_name_account_closes_the_initialized_grant() {
    let admin = LocalGrantSetting::Role(Role::Admin);
    let f = fixture_with(
        LocalGrantStart::Initialized(admin),
        true,
        &[Role::Admin],
        Some(admin),
    )
    .await;
    let colliding = f
        .users
        .create_user("localDesktop", PASSWORD, "x", Role::Viewer)
        .await
        .expect("create_user");

    assert_eq!(
        status_grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await,
        false
    );
    for path in [
        "/api/auth/grant/localDesktop",
        "/api/auth/grant/local%44esktop",
    ] {
        let (status, body) = send(
            &f.router,
            "POST",
            path,
            Some(LOCAL),
            Some(LOCAL_HOST),
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{path}: {body}");
        assert!(body.get("token").is_none(), "{path}: {body}");
        assert!(
            body["message"]
                .as_str()
                .is_some_and(|message| message.contains("別の名前")),
            "{path}: {body}"
        );
    }
    // LAN の peer には理由を言わない（条件の `403` のまま）。
    let (status, token) = grant(&f.router, Some(LAN), Some(LOCAL_HOST)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(token.is_none());
    assert_eq!(grant_logins(&f.audit).await, 0);

    // 予約名のアカウントを消せば開く（条件はリクエストのたびに読む）。
    f.users
        .delete_user(colliding.id, None)
        .await
        .expect("delete_user");
    assert_eq!(
        status_grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await,
        true
    );
    let (status, token) = grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(token.is_some());
}

/// 監査の行のうち `settings_change` で `detail.via` が `setup` のもの（#392 B2-2）。
async fn setup_opt_in_changes(audit: &AuditLogService) -> Vec<crate::audit::AuditLogEntry> {
    audit
        .list(ListParams::default())
        .await
        .unwrap()
        .rows
        .into_iter()
        .filter(|row| {
            row.action == "settings_change"
                && row
                    .detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains("\"via\":\"setup\""))
        })
        .collect()
}

/// #392 B2-2: 初回セットアップで「この PC からはログインなしで使う」を選ぶと
/// （本文の `"localGrant": "admin"`）、setup の後処理が設定を `admin` にし、未初期化で
/// 起動したまま（再起動なしで）`localDesktop` を admin で発行し続ける。未初期化の間の
/// トークンは今までどおり失効し、監査には setup・`revoke_grants`・`settings_change`
/// （操作者は作られた管理者、`via: setup`）が残る。その後に設定を `off` にすると
/// すぐ閉じ、`viewer` は再起動から、`admin` に戻すと再起動なしで開く（起動時の role が
/// admin のため）。
///
/// 反証: `local_desktop_grant_spec` の未初期化の条件を「いま未初期化」だけに戻すと、
/// setup の後の status が `false` になって落ちる。`finish_setup` で選択を保存しないと、
/// 同じく落ちる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opting_in_at_setup_keeps_the_admin_grant_without_a_restart() {
    let f = fixture(LocalGrantStart::Uninitialized, true, &[]).await;
    let (_, before) = grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await;
    let before = before.expect("token");

    let (status, body) = send(
        &f.router,
        "POST",
        "/api/auth/setup",
        Some(LOCAL),
        Some(LOCAL_HOST),
        Some(&before),
        Some(json!({
            "username": "owner",
            "password": PASSWORD,
            "displayName": "管理者",
            "localGrant": "admin",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["success"], true, "{body}");
    let owner_token = body["token"].as_str().expect("token").to_string();

    // 未初期化の間のトークンは失効している。
    assert!(
        identity(&f.router, &before).await.1.is_null(),
        "未初期化の間の admin のトークンが残っている"
    );
    // 設定は `admin` で、再起動なしで発行が続く。
    assert_eq!(
        crate::local_grant::read_setting(&f.settings).await.unwrap(),
        LocalGrantSetting::Role(Role::Admin)
    );
    assert_eq!(
        status_grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await,
        true
    );
    let (status, after) = grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await;
    assert_eq!(status, StatusCode::OK);
    let after = after.expect("token");
    let (_, me) = identity(&f.router, &after).await;
    assert_eq!(me["id"], "localDesktop", "{me}");
    assert_eq!(me["role"], "admin", "{me}");
    // LAN の peer には開かない（要求ごとの確認は変わらない）。
    let (status, _) = grant(&f.router, Some(LAN), Some(LOCAL_HOST)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // 監査: setup（banto）・revoke_grants・settings_change（via setup、操作者は owner）。
    let rows = f.audit.list(ListParams::default()).await.unwrap().rows;
    assert!(
        rows.iter()
            .any(|row| row.action == "setup" && row.actor_username.as_deref() == Some("owner")),
        "{rows:?}"
    );
    assert!(
        rows.iter().any(|row| row.action == "revoke_grants"),
        "{rows:?}"
    );
    let changes = setup_opt_in_changes(&f.audit).await;
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!(changes[0].actor_username.as_deref(), Some("owner"));
    assert_eq!(changes[0].actor_role.as_deref(), Some("admin"));
    assert_eq!(changes[0].result, "ok");
    assert_eq!(changes[0].origin, "rest");

    // 設定の画面の読み: `admin` で再起動は要らない。
    let (status, body) = send(
        &f.router,
        "GET",
        "/api/settings/auth",
        Some(LOCAL),
        Some(LOCAL_HOST),
        Some(&owner_token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["localGrant"], "admin", "{body}");
    assert_eq!(body["localGrantRestartRequired"], false, "{body}");

    let put = |value: &'static str| {
        let router = f.router.clone();
        let token = owner_token.clone();
        async move {
            send(
                &router,
                "PUT",
                "/api/settings/auth",
                Some(LOCAL),
                Some(LOCAL_HOST),
                Some(&token),
                Some(json!({ "localGrant": value })),
            )
            .await
        }
    };
    // `off` にするとすぐ閉じ、発行済みは失効する。
    let (status, body) = put("off").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(identity(&f.router, &after).await.1.is_null());
    assert_eq!(
        status_grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await,
        false
    );
    // `viewer` は起動時の role と違うので再起動から。
    let (status, body) = put("viewer").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["localGrantRestartRequired"], true, "{body}");
    assert_eq!(
        status_grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await,
        false
    );
    // `admin` に戻すと再起動なしで開く。
    let (status, body) = put("admin").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["localGrantRestartRequired"], false, "{body}");
    let (status, _) = grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await;
    assert_eq!(status, StatusCode::OK);
}

/// #392 B2-2: 選ばない setup（欄が無い・`null`）は今までどおり: 初期化で grant は閉じ、
/// 設定は `off` のまま、選択の `settings_change` は残らない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn setup_without_opting_in_closes_the_grant() {
    for local_grant in [None, Some(Value::Null)] {
        let f = fixture(LocalGrantStart::Uninitialized, true, &[]).await;
        let mut body = json!({ "username": "owner", "password": PASSWORD, "displayName": "x" });
        if let Some(value) = local_grant.clone() {
            body["localGrant"] = value;
        }
        let (status, body) = send(
            &f.router,
            "POST",
            "/api/auth/setup",
            Some(LOCAL),
            Some(LOCAL_HOST),
            None,
            Some(body),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["success"], true, "{body}");
        assert_eq!(
            status_grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await,
            false,
            "{local_grant:?}"
        );
        let (status, _) = grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(
            crate::local_grant::read_setting(&f.settings).await.unwrap(),
            LocalGrantSetting::Off
        );
        assert!(setup_opt_in_changes(&f.audit).await.is_empty());
    }
}

/// #392 B2-2: `admin` 以外の値（他の role・`off`・型違い）は setup を走らせずに `400`
/// （アカウントを作らない）。発行しうる形でないルーター（デスクトップ・`banto-serve` の
/// `Unsupported`）で選んでも `400`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bad_opt_in_value_is_refused_before_the_account_is_created() {
    let f = fixture(LocalGrantStart::Uninitialized, true, &[]).await;
    for value in [json!("viewer"), json!("off"), json!("root"), json!(true)] {
        let (status, body) = send(
            &f.router,
            "POST",
            "/api/auth/setup",
            Some(LOCAL),
            Some(LOCAL_HOST),
            None,
            Some(json!({
                "username": "owner",
                "password": PASSWORD,
                "displayName": "x",
                "localGrant": value.clone(),
            })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{value}: {body}");
        assert!(!f.users.is_initialized().await.unwrap(), "{value}");
    }
    let f = fixture(LocalGrantStart::Unsupported, true, &[]).await;
    let (status, body) = send(
        &f.router,
        "POST",
        "/api/auth/setup",
        Some(LOCAL),
        Some(LOCAL_HOST),
        None,
        Some(json!({
            "username": "owner",
            "password": PASSWORD,
            "displayName": "x",
            "localGrant": "admin",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(!f.users.is_initialized().await.unwrap());
    assert_eq!(
        crate::local_grant::read_setting(&f.settings).await.unwrap(),
        LocalGrantSetting::Off
    );
}

/// #392 B2-2: 選んでいても、setup が確かに成功したときだけ保存する（`success: false`・
/// 読めない本文では書かない - 失効は今までどおり読めない本文でも行う）。保存に失敗しても
/// 応答は変えず、`result: "failed"` の監査が残って grant は閉じたまま（fail closed）。
///
/// 反証: `finish_setup` の保存の判断を `setup_may_have_initialized` に替えると、読めない
/// 本文で設定が `admin` になって落ちる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_opt_in_is_saved_only_after_a_confirmed_success() {
    use axum::response::IntoResponse;
    let f = fixture(LocalGrantStart::Uninitialized, true, &[]).await;
    let state = |settings: SettingsService| LocalAccessState {
        access: LocalAccess {
            start: LocalGrantStart::Uninitialized,
            always_on_listener: true,
        },
        users: f.users.clone(),
        auth: f.auth.clone(),
        audit: f.audit.clone(),
        settings,
        settings_lock: Arc::new(tokio::sync::Mutex::new(())),
    };
    let ok_state = state(f.settings.clone());

    // `success: false` と読めない本文では書かない。
    for response in [
        (StatusCode::OK, axum::Json(json!({ "success": false }))).into_response(),
        (StatusCode::OK, "not json").into_response(),
    ] {
        let response = finish_setup(&ok_state, response, true).await;
        assert_eq!(response.status(), StatusCode::OK);
    }
    assert_eq!(
        crate::local_grant::read_setting(&f.settings).await.unwrap(),
        LocalGrantSetting::Off
    );
    assert!(setup_opt_in_changes(&f.audit).await.is_empty());

    // 保存に失敗しても応答は成功のまま。監査は `failed`、設定は変わらず grant は閉じる。
    let closed = migrate_memory().await.expect("migrate_memory");
    closed.close().await;
    let failing = state(SettingsService::new(Db::Sqlite(closed)));
    f.users
        .setup_first_user("owner", PASSWORD, "x")
        .await
        .expect("setup_first_user");
    let response = finish_setup(
        &failing,
        (
            StatusCode::OK,
            axum::Json(json!({ "success": true, "token": "unknown-token" })),
        )
            .into_response(),
        true,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let changes = setup_opt_in_changes(&f.audit).await;
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!(changes[0].result, "failed");
    assert_eq!(
        changes[0].actor_username, None,
        "読めないトークンで操作者を作った"
    );
    assert_eq!(
        crate::local_grant::read_setting(&f.settings).await.unwrap(),
        LocalGrantSetting::Off
    );
    assert_eq!(
        status_grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await,
        false
    );
}

/// #392 B2-2 の監査: 初回セットアップの選択は常に決定的。未初期化の間に grant の admin の
/// トークンから `PUT /api/settings/auth {localGrant: "admin"}` を先に保存しておいても、
/// チェックを外して作れば、setup の後処理が `off` にして grant は閉じる（status `false`・
/// 発行は `403`・監査に `settings_change`（`off`・`via: setup`、操作者は作った管理者）が
/// 1 行）。
///
/// 反証: `finish_setup` の `off` の保存を外す（`setup_choice` が選ばなかったときに `None`
/// を返す）と、先に保存した `admin` で grant が開いたままになって落ちる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn setup_without_opting_in_overrides_a_value_saved_before_setup() {
    let f = fixture(LocalGrantStart::Uninitialized, true, &[]).await;
    let (_, token) = grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await;
    let token = token.expect("token");
    let (status, body) = send(
        &f.router,
        "PUT",
        "/api/settings/auth",
        Some(LOCAL),
        Some(LOCAL_HOST),
        Some(&token),
        Some(json!({ "localGrant": "admin" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        crate::local_grant::read_setting(&f.settings).await.unwrap(),
        LocalGrantSetting::Role(Role::Admin)
    );

    let (status, body) = send(
        &f.router,
        "POST",
        "/api/auth/setup",
        Some(LOCAL),
        Some(LOCAL_HOST),
        None,
        Some(json!({ "username": "owner", "password": PASSWORD, "displayName": "x" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["success"], true, "{body}");

    assert_eq!(
        status_grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await,
        false,
        "チェックを外した作成の後も grant が開いている"
    );
    let (status, token) = grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(token.is_none());
    assert_eq!(
        crate::local_grant::read_setting(&f.settings).await.unwrap(),
        LocalGrantSetting::Off
    );
    let changes = setup_opt_in_changes(&f.audit).await;
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert!(
        changes[0]
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("\"localGrant\":\"off\"")),
        "{changes:?}"
    );
    assert_eq!(changes[0].result, "ok");
    assert_eq!(changes[0].actor_username.as_deref(), Some("owner"));
}

/// `revoke_grants`（setup の後の失効）の監査の行の数。
async fn setup_revocations(audit: &AuditLogService) -> usize {
    audit
        .list(ListParams::default())
        .await
        .unwrap()
        .rows
        .into_iter()
        .filter(|row| row.action == "revoke_grants")
        .count()
}

/// 上限つきで `cond` が真になるまで待つ（時間で決めず、状態の変化を待つ）。
async fn wait_until<F, Fut>(what: &str, mut cond: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while !cond().await {
        assert!(tokio::time::Instant::now() < deadline, "{what} にならない");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

/// `POST /api/auth/setup` を別のタスクで送る（応答はハンドルで待つ）。
fn spawn_setup(router: &Router, body: Value) -> tokio::task::JoinHandle<StatusCode> {
    let router = router.clone();
    tokio::spawn(async move {
        let mut request = Request::builder()
            .method("POST")
            .uri("/api/auth/setup")
            .header("X-Banto-Client", "banto")
            .header("Host", LOCAL_HOST)
            .header("Content-Type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        request.extensions_mut().insert(ConnectInfo(LOCAL));
        router.oneshot(request).await.unwrap().status()
    })
}

/// #392 B2-2 のレビュー（Codex P1・オーナー P2-2）: 未初期化の間に `admin` を先に保存した
/// まま、チェックを外して作ると、最初の失効から `off` の保存までの間は先に保存された
/// `admin` で条件が開いているので、別の要求が新しいトークン T を受け取れる。grant の
/// セッションは条件を読み直さないので、`off` の保存の後も T が残っていた。保存と
/// 2 回目の失効を同じ lock の下で行うので、T も失効する。
///
/// lock を外から握って保存の手前で止め（最初の失効は済んでいる = 監査の行で見る）、
/// その間に T を受け取り、lock を放して setup の終わりを待つ。
///
/// 反証: `save_setup_choice` の 2 回目の失効を外すと、T で設定を読めて落ちる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_grant_issued_before_the_setup_choice_is_saved_is_revoked() {
    let f = fixture(LocalGrantStart::Uninitialized, true, &[]).await;
    let (_, first) = grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await;
    let (status, body) = send(
        &f.router,
        "PUT",
        "/api/settings/auth",
        Some(LOCAL),
        Some(LOCAL_HOST),
        Some(&first.expect("token")),
        Some(json!({ "localGrant": "admin" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // 上の保存で失効したので、未初期化の間のトークンをもう一度受け取る（最初の失効の対象）。
    let (_, before) = grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await;
    let before = before.expect("token");

    let held = f.settings_lock.clone().lock_owned().await;
    let setup = spawn_setup(
        &f.router,
        json!({ "username": "owner", "password": PASSWORD, "displayName": "x" }),
    );
    let audit = f.audit.clone();
    wait_until("最初の失効の監査", || {
        let audit = audit.clone();
        async move { setup_revocations(&audit).await >= 1 }
    })
    .await;
    assert!(identity(&f.router, &before).await.1.is_null());

    // 最初の失効の後・`off` の保存の前: 先に保存された admin で発行される。
    let (status, late) = grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await;
    assert_eq!(status, StatusCode::OK);
    let late = late.expect("token");
    assert!(!identity(&f.router, &late).await.1.is_null());

    drop(held);
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(10), setup)
            .await
            .expect("setup が終わらない")
            .unwrap(),
        StatusCode::OK
    );

    assert!(
        identity(&f.router, &late).await.1.is_null(),
        "保存の前に発行されたトークンが残っている"
    );
    let (status, _) = send(
        &f.router,
        "GET",
        "/api/settings/auth",
        Some(LOCAL),
        Some(LOCAL_HOST),
        Some(&late),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        crate::local_grant::read_setting(&f.settings).await.unwrap(),
        LocalGrantSetting::Off
    );
    assert_eq!(
        setup_revocations(&f.audit).await,
        2,
        "2 回目の失効の監査が無い"
    );
    assert_eq!(
        status_grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await,
        false
    );
}

/// #392 B2-2 のレビュー（Copilot）: `PUT /api/settings/auth` は認可を lock の手前で済ませる
/// ので、未初期化の間の admin のトークン T0 で認可を通った `PUT {localGrant: "admin"}` が
/// lock を待つ間に setup（チェックなし）が T0 を失効させ `off` を保存しても、その後で lock を
/// 取って `admin` を書き戻せた。lock の下で認可を確かめ直すので `401` で何も書かない。
///
/// lock を外から握り、T0 の `PUT` を本文の最初の poll（= 認可の層を通った）まで進め、
/// setup を送って最初の失効（監査の行）を待ってから lock を放す。lock の取り合いの順
/// （`PUT` と setup の保存のどちらが先か）に関わらず、`PUT` は `401`・設定は `off`。
///
/// 反証: `auth_settings_apply` の lock の下の再確認を外すと、`PUT` が `200` になって落ちる
/// （`PUT` が先に lock を取れば最後は `off` になるが、応答で分かる）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_settings_put_waiting_for_the_lock_cannot_undo_the_setup_choice() {
    let f = fixture(LocalGrantStart::Uninitialized, true, &[]).await;
    let (_, t0) = grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await;
    let t0 = t0.expect("token");

    let held = f.settings_lock.clone().lock_owned().await;
    let (polled_tx, polled_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let state = Some((polled_tx, release_rx));
    let stream = futures_util::stream::unfold(state, |state| async move {
        let (polled_tx, release_rx) = state?;
        let _ = polled_tx.send(());
        let _ = release_rx.await;
        Some((
            Ok::<_, std::io::Error>(axum::body::Bytes::from(
                json!({ "localGrant": "admin" }).to_string(),
            )),
            None,
        ))
    });
    let mut request = Request::builder()
        .method("PUT")
        .uri("/api/settings/auth")
        .header("X-Banto-Client", "banto")
        .header("Host", LOCAL_HOST)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {t0}"))
        .body(Body::from_stream(stream))
        .unwrap();
    request.extensions_mut().insert(ConnectInfo(LOCAL));
    let router = f.router.clone();
    let put = tokio::spawn(async move { router.oneshot(request).await.unwrap() });
    polled_rx.await.expect("本文が読まれなかった");
    release_tx.send(()).unwrap();

    let setup = spawn_setup(
        &f.router,
        json!({ "username": "owner", "password": PASSWORD, "displayName": "x" }),
    );
    let audit = f.audit.clone();
    wait_until("setup の最初の失効の監査", || {
        let audit = audit.clone();
        async move { setup_revocations(&audit).await >= 1 }
    })
    .await;
    drop(held);

    let put = tokio::time::timeout(std::time::Duration::from_secs(10), put)
        .await
        .expect("PUT が終わらない")
        .unwrap();
    assert_eq!(put.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(10), setup)
            .await
            .expect("setup が終わらない")
            .unwrap(),
        StatusCode::OK
    );
    assert_eq!(
        crate::local_grant::read_setting(&f.settings).await.unwrap(),
        LocalGrantSetting::Off
    );
    let changes = f
        .audit
        .list(ListParams::default())
        .await
        .unwrap()
        .rows
        .into_iter()
        .filter(|row| {
            row.action == "settings_change"
                && row
                    .detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains("\"localGrant\":\"admin\""))
        })
        .count();
    assert_eq!(changes, 0, "失効したトークンの変更が監査に残った");
    assert_eq!(
        status_grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await,
        false
    );
}

/// #392 B2-2 のレビュー: lock の下の認可の再確認（`server_admin::recheck_token`。
/// `PUT /api/settings/server` と `PUT /api/settings/auth` で共有）。有効なトークンと
/// トークンの無い経路（デスクトップの Tauri のコマンド）は通し、失効したトークンは
/// `Unauthorized`。`PUT /api/settings/server` の配線は、このルーターの host では `PUT` が
/// 入れ替えの前に `403` になるので、ここでは関数だけを確かめる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_lock_recheck_refuses_a_revoked_token() {
    use crate::server_admin::recheck_token;
    let f = fixture(LocalGrantStart::Uninitialized, true, &[]).await;
    let (_, token) = grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await;
    let token = token.expect("token");
    assert!(recheck_token(&f.auth, Some(&token)).await.is_ok());
    assert!(recheck_token(&f.auth, None).await.is_ok());
    f.auth
        .revoke_grant_tokens(&crate::local_grant::local_desktop_grant_kind());
    assert!(matches!(
        recheck_token(&f.auth, Some(&token)).await,
        Err(banto_core::BantoError::Unauthorized)
    ));
    assert!(matches!(
        recheck_token(&f.auth, Some("not-a-token")).await,
        Err(banto_core::BantoError::Unauthorized)
    ));
}

/// `login`（`via: localGrant`）の監査の行の actor_role（古い順）。
async fn grant_login_roles(audit: &AuditLogService) -> Vec<Option<String>> {
    let mut rows: Vec<_> = audit
        .list(ListParams::default())
        .await
        .unwrap()
        .rows
        .into_iter()
        .filter(|row| {
            row.action == "login"
                && row.actor_username.as_deref() == Some("localDesktop")
                && row
                    .detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains("localGrant"))
        })
        .collect();
    rows.sort_by_key(|row| row.id);
    rows.into_iter().map(|row| row.actor_role).collect()
}

/// #601 (1): 発行の後・監査の前に、同時の発行で FIFO の上限を超えて発行したトークンが
/// 追い出されても、発行の監査（`login`）は 1 行残り、role は発行したセッションのもの。
///
/// 待ち合わせを決定的に置くため、`local_access_middleware` の下の発行口を、本物の
/// `AuthState::issue_grant_token` で発行した直後に、上限（`max_sessions`）の数だけ
/// 発行し直して追い出してから応答する口に差し替える（= 応答の前に追い出しが必ず済む）。
/// kind は percent encode した別表記で送る（#600 の P2 の約束も保つ）。
///
/// 反証: 監査の前に応答のトークンを `authenticate` してセッションの kind と role を読む
/// 形（#601 の前）に戻すと、追い出されたトークンを認証できず監査の行が 0 で落ちる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_grant_evicted_before_its_audit_is_still_audited_with_its_role() {
    use crate::local_grant::{local_access_middleware, local_desktop_grant_spec};
    use axum::middleware::from_fn_with_state;
    use axum::routing::post;

    let editor = LocalGrantSetting::Role(Role::Editor);
    for (start, accounts, setting, role) in [
        (LocalGrantStart::Uninitialized, &[][..], None, "admin"),
        (
            LocalGrantStart::Initialized(editor),
            &[Role::Admin][..],
            Some(editor),
            "editor",
        ),
    ] {
        let f = fixture_with(start, true, accounts, setting).await;
        let access = LocalAccess {
            start,
            always_on_listener: true,
        };
        let spec = Arc::new(local_desktop_grant_spec(
            access,
            f.users.clone(),
            f.settings.clone(),
        ));
        let issuer = {
            let (auth, spec) = (f.auth.clone(), spec.clone());
            move || async move {
                let token = auth
                    .issue_grant_token(&spec, auth.grant_generation(&spec.kind))
                    .expect("issue");
                // 発行と監査の間に、同時の発行が上限を埋めて追い出した。
                for _ in 0..LOCAL_GRANT_MAX_SESSIONS {
                    auth.issue_grant_token(&spec, auth.grant_generation(&spec.kind))
                        .expect("issue");
                }
                assert!(
                    auth.authenticate(&token).await.unwrap().is_none(),
                    "追い出されていない"
                );
                axum::Json(json!({ "success": true, "token": token }))
            }
        };
        let state = LocalAccessState {
            access,
            users: f.users.clone(),
            auth: f.auth.clone(),
            audit: f.audit.clone(),
            settings: f.settings.clone(),
            settings_lock: f.settings_lock.clone(),
        };
        let router = Router::new()
            .route("/api/auth/grant/{kind}", post(issuer))
            .layer(from_fn_with_state(state, local_access_middleware));
        let (status, body) = send(
            &router,
            "POST",
            "/api/auth/grant/local%44esktop",
            Some(LOCAL),
            Some(LOCAL_HOST),
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            grant_login_roles(&f.audit).await,
            vec![Some(role.to_string())],
            "{start:?}"
        );
    }
}

/// #601 (1): 本物のルーターに上限の 2 倍を超える発行を同時に送っても、成功した発行の
/// 数だけ `login` の監査が残る（タイミング任せの確認。決定的な確認は上のテスト）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_grants_past_the_cap_are_audited_one_to_one() {
    let f = fixture(LocalGrantStart::Uninitialized, true, &[]).await;
    let count = LOCAL_GRANT_MAX_SESSIONS * 2 + 1;
    let handles: Vec<_> = (0..count)
        .map(|_| {
            let router = f.router.clone();
            tokio::spawn(async move { grant(&router, Some(LOCAL), Some(LOCAL_HOST)).await })
        })
        .collect();
    for handle in handles {
        let (status, token) = handle.await.unwrap();
        assert_eq!(status, StatusCode::OK);
        assert!(token.is_some());
    }
    let roles = grant_login_roles(&f.audit).await;
    assert_eq!(roles.len(), count, "発行と監査が 1 対 1 でない");
    assert!(roles.iter().all(|role| role.as_deref() == Some("admin")));
}

/// `PUT /api/settings/auth` を送り、応答の `(localGrant, localGrantRestartRequired)`。
async fn put_local_grant(router: &Router, token: &str, value: &str) -> (String, bool) {
    let (status, body) = send(
        router,
        "PUT",
        "/api/settings/auth",
        Some(LOCAL),
        Some(LOCAL_HOST),
        Some(token),
        Some(json!({ "localGrant": value })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}: {body}");
    (
        body["localGrant"].as_str().expect("localGrant").to_string(),
        body["localGrantRestartRequired"]
            .as_bool()
            .expect("localGrantRestartRequired"),
    )
}

/// #601 (2): 2 件の `PUT /api/settings/auth` が重なっても、各応答は**その要求が保存した
/// 値**と、それに対応する `localGrantRestartRequired` を返す。最後の保存値と失効も保つ。
///
/// 待ち合わせを決定的に置くため、監査の書き込み先だけをファイルの DB に分け、外から
/// 書き込みの lock（`BEGIN IMMEDIATE`）を握る: 要求 A（`editor`）が保存して lock を
/// 外した後、監査の書き込みで止まっている間に、要求 B（`off`）が保存し終える
/// （設定の値で確かめる）。それから監査の lock を放す。
///
/// 反証: 応答を lock の外で読み直す形（#601 の前）に戻すと、A の応答が B の値
/// （`off`・再起動不要）になって落ちる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_auth_put_answers_with_the_value_it_saved() {
    use crate::server_admin::{settings_admin_router, SettingsAdminState};
    use sqlx::{ConnectOptions, Connection};

    let viewer = LocalGrantSetting::Role(Role::Viewer);
    let start = LocalGrantStart::Initialized(viewer);
    let f = fixture_with(start, true, &[Role::Admin], Some(viewer)).await;
    let admin = login(&f.router, "admin").await;
    let (_, local_token) = grant(&f.router, Some(LOCAL), Some(LOCAL_HOST)).await;
    let local_token = local_token.expect("token");

    let dir = tempfile::tempdir().unwrap();
    let audit_path = dir.path().join("audit.sqlite3");
    let audit_pool = crate::db::init_db(&audit_path).await.expect("init_db");
    let audit = AuditLogService::new(Db::Sqlite(audit_pool.clone()));
    let router = settings_admin_router(SettingsAdminState {
        settings: f.settings.clone(),
        users: f.users.clone(),
        audit: audit.clone(),
        auth: f.auth.clone(),
        lan: LanControl::detached(),
        local_grant: start,
        auth_config_lock: Arc::new(tokio::sync::Mutex::new(())),
        log_label: "test",
    });

    let mut blocker = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&audit_path)
        .connect()
        .await
        .unwrap();
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut blocker)
        .await
        .unwrap();

    let read = || {
        let settings = f.settings.clone();
        async move { crate::local_grant::read_setting(&settings).await.unwrap() }
    };
    let a = {
        let (router, admin) = (router.clone(), admin.clone());
        tokio::spawn(async move { put_local_grant(&router, &admin, "editor").await })
    };
    wait_until("A の保存", || async {
        read().await == LocalGrantSetting::Role(Role::Editor)
    })
    .await;
    let b = {
        let (router, admin) = (router.clone(), admin.clone());
        tokio::spawn(async move { put_local_grant(&router, &admin, "off").await })
    };
    wait_until("B の保存", || async {
        read().await == LocalGrantSetting::Off
    })
    .await;
    sqlx::query("ROLLBACK").execute(&mut blocker).await.unwrap();
    blocker.close().await.unwrap();

    let within = std::time::Duration::from_secs(10);
    let a = tokio::time::timeout(within, a)
        .await
        .expect("A が終わらない")
        .unwrap();
    let b = tokio::time::timeout(within, b)
        .await
        .expect("B が終わらない")
        .unwrap();
    // 起動時は viewer: editor は開く向きの変更なので再起動が要り、off はすぐ効く。
    assert_eq!(a, ("editor".to_string(), true), "A の応答に別の要求の値");
    assert_eq!(b, ("off".to_string(), false));
    assert_eq!(read().await, LocalGrantSetting::Off, "最後の保存値");
    assert!(
        identity(&f.router, &local_token).await.1.is_null(),
        "値の変更で grant のトークンが失効していない"
    );
    let changes = audit
        .list(ListParams::default())
        .await
        .unwrap()
        .rows
        .into_iter()
        .filter(|row| row.action == "settings_change")
        .count();
    assert_eq!(changes, 2);
    audit_pool.close().await;
}

/// #601 (2): 異なる値の `PUT /api/settings/auth` を同時に多数送っても、各応答の
/// `localGrant` は自分の要求の値で、`localGrantRestartRequired` はその値に対応する
/// （タイミング任せの確認。決定的な確認は上のテスト）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_auth_puts_each_answer_with_their_own_value() {
    let viewer = LocalGrantSetting::Role(Role::Viewer);
    let f = fixture_with(
        LocalGrantStart::Initialized(viewer),
        true,
        &[Role::Admin],
        Some(viewer),
    )
    .await;
    let admin = login(&f.router, "admin").await;
    // 起動時は viewer: viewer と off は再起動不要、editor と admin は要る。
    let values = [
        ("off", false),
        ("viewer", false),
        ("editor", true),
        ("admin", true),
    ];
    let handles: Vec<_> = (0..24)
        .map(|i| {
            let (value, restart) = values[i % values.len()];
            let (router, admin) = (f.router.clone(), admin.clone());
            tokio::spawn(async move {
                (
                    value,
                    restart,
                    put_local_grant(&router, &admin, value).await,
                )
            })
        })
        .collect();
    for handle in handles {
        let (value, restart, answered) = handle.await.unwrap();
        assert_eq!(answered, (value.to_string(), restart), "{value}");
    }
}
