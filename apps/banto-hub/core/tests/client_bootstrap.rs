//! #332 の Hub 自動接続（`crates/banto-hub-bootstrap`）が依存する Hub 側の
//! 性質を、本物の `api_router` で回帰固定するテスト。
//!
//! bootstrap（クライアント側）はモックサーバー相手に自分のロジックを固定して
//! いるが、**モックが本物と同じ形である**ことまでは保証できない。banto v3.0.0
//! 追従（ADR-0017、2026-10-04）で「試運転中は管理 REST を未認証で叩ける」は
//! 無くなり、bootstrap の自己発行は「status → `POST /api/auth/grant/commissioning`
//! → grant の bearer で `POST /api/api-keys` → `POST /api/auth/logout`」の 4 手順に
//! なった。ここで確認する性質:
//!
//! 1. 試運転中（未ロックダウン）は `GET /api/commissioning/status` が
//!    **未認証で** `{"lockedDown": false}` を返す。
//! 2. その status も `X-Banto-Client` ヘッダ無しでは通らない（CSRF ガードは
//!    管理ルーター全体に掛かる - bootstrap が全部の管理 API にこのヘッダを
//!    付けている根拠）。
//! 3. 試運転中でも `POST /api/api-keys` は **未認証なら 401**。
//!    `POST /api/auth/grant/commissioning` は **loopback の peer にだけ**
//!    `{ success, token }` を返し（LAN の peer・peer 不明は 403）、
//!    `GET /api/auth/status` の `grants.commissioning` も同じ判定。
//! 4. grant の bearer で `POST /api/api-keys` が `read` スコープのキーを発行し、
//!    平文 `key` を一度だけ返す。同名は `validation`（409 ではない）。
//!    `POST /api/auth/logout` で grant は無効になる（以後 401）。
//! 5. そのキーで `GET /api/v1/tags` が **タグ 0 件でも 200 + `tags: []`** を
//!    返す（`/api/v1/*` に `X-Banto-Client` は不要）。ここが崩れると
//!    「接続済み・利用可能なタグなし」が「失敗」に化ける。
//! 6. ロックダウン後は grant が 403、ロックダウン前に取った grant も 401
//!    （保存の直後に失効）、未認証の `POST /api/api-keys` は 401
//!    （= クライアントは `NeedsPairing` に落ちる。自己発行の窓が閉じる）。
//!
//! 足場（`TestApp`/`test_app`）は `tests/rbac.rs` のものをベースにした複製
//! （各 `tests/*.rs` は独立クレートとしてコンパイルされ private helper を
//! 共有できないため - `tests/rbac.rs` の doc comment 参照）。**唯一の違いは
//! `lock_down()` を呼ばないこと**で、rbac 側の `test_app` はロックダウン
//! 済み固定なのでそのままでは流用できない。grant の判定は接続の peer
//! （`ConnectInfo<SocketAddr>`）を見るが `tower::oneshot` には無いので、本番の
//! `banto_server::start` が供給するのと同じものを extensions に明示する。

use axum::body::Body;
use axum::http::{Request as HttpRequest, StatusCode};
use axum::Router;
use banto_collect::{BackoffConfig, CollectorOptions};
use banto_hub_core::api_keys::ApiKeysService;
use banto_hub_core::audit::AuditLogService;
use banto_hub_core::broker_glue::{BrokerSimRegistry, HubSessions};
use banto_hub_core::commissioning::CommissioningService;
use banto_hub_core::computed::{ComputedEngine, ServerTagStore};
use banto_hub_core::db::init_db;
use banto_hub_core::db::Db;
use banto_hub_core::grpc::{GrpcServer, GrpcService};
use banto_hub_core::hub::CollectorManager;
use banto_hub_core::rest::api_router;
use banto_hub_core::rest::user_session_lookup;
use banto_hub_core::settings::SettingsService;
use banto_hub_core::users::UsersService;
use banto_hub_core::write_audit::WriteAuditService;
use banto_hub_core::write_control::WriteControl;
use banto_hub_core::write_rate::{WriteRateLimitConfig, WriteRateLimiter};
use banto_server::SessionValidation;
use banto_server::{AuthState, Identity};
use banto_tags::{CollectionGroupService, PlcConnectionService, TagService};
use serde_json::{json, Value};
use sqlx::SqlitePool;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tower::ServiceExt;

mod common;
use common::TempEnv;

const CLIENT_HEADER: (&str, &str) = ("X-Banto-Client", "banto");
const TEMP_ENV_PREFIX: &str = "banto-hub-client-bootstrap-it";

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

struct TestApp {
    router: Router,
    /// Clone of the `CommissioningService` the router was built with. It is
    /// `Clone` and both clones share one `CommissioningState` handle, so
    /// locking down through this clone is immediately visible to the
    /// router's middleware (that is exactly what scenario 5 needs).
    commissioning: CommissioningService,
    pool: SqlitePool,
    manager: Arc<CollectorManager>,
    _env: TempEnv,
}

// See `tests/common/mod.rs`'s module doc ("Why `TestApp` also needs
// `shutdown_test_app`") for why this is required, not optional.
impl Drop for TestApp {
    fn drop(&mut self) {
        common::shutdown_test_app(&self.manager, &self.pool);
    }
}

/// Like `rbac.rs::test_app`, but **still in commissioning mode** (no
/// `lock_down()` call): that is the window #332's self-issuing depends on.
async fn test_app(label: &str) -> TestApp {
    let env = TempEnv::new(TEMP_ENV_PREFIX, label);
    let pool = init_db(env.registry_path()).await.expect("init_db");

    let users = UsersService::new(Db::Sqlite(pool.clone()));
    let audit = AuditLogService::new(Db::Sqlite(pool.clone()));
    // An admin account has to exist for `lock_down()` to be allowed at all
    // (`CommissioningService::lock_down`'s `has_admin` guard), which
    // scenario 5 needs.
    users
        .setup_first_user("admin", "password123", "管理者")
        .await
        .expect("setup_first_user");

    let verify_users = users.clone();
    let verify_users_lookup = users.clone();
    let auth = AuthState::new(
        move |u: String, p: String| {
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
        },
        SessionValidation::lookup(user_session_lookup(verify_users_lookup)),
    );

    let sessions = Arc::new(HubSessions::new(banto_broker::BackoffConfig::default()));
    let sim_registry = Arc::new(BrokerSimRegistry::new());
    let computed = Arc::new(ComputedEngine::new(Arc::new(ServerTagStore::new())));
    let manager = Arc::new(CollectorManager::new(
        pool.clone(),
        env.data_dir(),
        Arc::new(banto_tstore::SystemClock),
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

    let settings = SettingsService::new(Db::Sqlite(pool.clone()));
    let commissioning = CommissioningService::load(settings, users.clone(), auth.clone())
        .await
        .expect("CommissioningService::load");
    let commissioning_for_test = commissioning.clone();

    let router = api_router(
        users.clone(),
        audit,
        PlcConnectionService::new(pool.clone()),
        CollectionGroupService::new(pool.clone()),
        TagService::new(pool.clone()),
        api_keys,
        manager.clone(),
        auth,
        commissioning,
        events_tx,
        false,
        write_control,
        write_audit,
        mqtt,
        grpc_server,
        rate_limiter,
        banto_hub_core::profile_paths::DEFAULT_PROFILE_ID.to_string(),
    );

    TestApp {
        router,
        commissioning: commissioning_for_test,
        pool,
        manager,
        _env: env,
    }
}

const LOOPBACK_PEER: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 40000);
const LAN_PEER: SocketAddr = SocketAddr::new(
    std::net::IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 20)),
    40000,
);

/// An admin-API request as `banto-hub-bootstrap` sends it: `X-Banto-Client`
/// always; `Authorization` only when `bearer` is given (the commissioning
/// grant); `peer` is the connection's `ConnectInfo` (absent under `oneshot`).
async fn admin_request(
    router: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    bearer: Option<&str>,
    peer: Option<SocketAddr>,
) -> (StatusCode, Value) {
    let mut builder = HttpRequest::builder()
        .method(method)
        .uri(path)
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .header("content-type", "application/json");
    if let Some(bearer) = bearer {
        builder = builder.header("Authorization", format!("Bearer {bearer}"));
    }
    let mut request = match &body {
        Some(value) => builder
            .body(Body::from(serde_json::to_vec(value).unwrap()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    if let Some(peer) = peer {
        request
            .extensions_mut()
            .insert(axum::extract::ConnectInfo(peer));
    }
    read_response(router, request).await
}

/// An unauthenticated admin-API request (`X-Banto-Client`, no `Authorization`).
async fn admin_unauthenticated(
    router: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    admin_request(router, method, path, body, None, None).await
}

/// `POST /api/auth/grant/commissioning` from `peer`.
async fn commissioning_grant(router: &Router, peer: Option<SocketAddr>) -> (StatusCode, Value) {
    admin_request(
        router,
        "POST",
        "/api/auth/grant/commissioning",
        None,
        None,
        peer,
    )
    .await
}

/// The commissioning grant a loopback caller gets while commissioning.
async fn loopback_grant(router: &Router) -> String {
    let (status, body) = commissioning_grant(router, Some(LOOPBACK_PEER)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["success"], json!(true));
    body["token"].as_str().expect("grant token").to_owned()
}

/// A tag-space request: bearer API key, and deliberately **no**
/// `X-Banto-Client` (that header is admin-router-only).
async fn tag_space_get(router: &Router, path: &str, key: &str) -> (StatusCode, Value) {
    let request = HttpRequest::builder()
        .method("GET")
        .uri(path)
        .header("Authorization", format!("Bearer {key}"))
        .body(Body::empty())
        .unwrap();
    read_response(router, request).await
}

async fn read_response(router: &Router, request: HttpRequest<Body>) -> (StatusCode, Value) {
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn commissioning_status_is_readable_without_auth_but_needs_the_client_header() {
    let app = test_app("status").await;

    let (status, body) =
        admin_unauthenticated(&app.router, "GET", "/api/commissioning/status", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["lockedDown"], json!(false));

    // Without the CSRF marker the whole admin router refuses - this is why
    // `banto-hub-bootstrap` sends it on BOTH admin requests.
    let response = app
        .router
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri("/api/commissioning/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        response.status().is_client_error(),
        "missing X-Banto-Client must be refused, got {}",
        response.status()
    );
}

/// 3.: the grant is the only credential-less entry, and only for loopback.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_commissioning_grant_is_issued_to_loopback_only() {
    let app = test_app("grant").await;

    // No bearer at all: the admin router refuses even while commissioning.
    let (status, _) = admin_unauthenticated(
        &app.router,
        "POST",
        "/api/api-keys",
        Some(json!({ "name": "chronogazer-inst-0", "scopes": ["read"] })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "banto v3.0.0: no unauthenticated admin request passes while commissioning"
    );

    // `status` and the issuing route share one judgment.
    let (status, body) = admin_request(
        &app.router,
        "GET",
        "/api/auth/status",
        None,
        None,
        Some(LOOPBACK_PEER),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["grants"]["commissioning"], json!(true), "{body}");
    let (status, body) =
        admin_request(&app.router, "GET", "/api/auth/status", None, None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["grants"]["commissioning"],
        json!(false),
        "an unknown peer is never offered the grant: {body}"
    );

    let (status, _) = commissioning_grant(&app.router, Some(LAN_PEER)).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a LAN peer gets no admin grant"
    );
    let (status, _) = commissioning_grant(&app.router, None).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "an unknown peer gets no admin grant"
    );

    let token = loopback_grant(&app.router).await;
    let (status, identity) = admin_request(
        &app.router,
        "GET",
        "/api/auth/identity",
        None,
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(identity["kind"], json!("commissioning"), "{identity}");
    assert_eq!(identity["id"], json!("commissioning"), "{identity}");
    assert_eq!(identity["role"], json!("admin"), "{identity}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_self_issued_read_key_reads_an_empty_catalog_as_200_with_no_tags() {
    let app = test_app("issue").await;
    let grant = loopback_grant(&app.router).await;

    let (status, issued) = admin_request(
        &app.router,
        "POST",
        "/api/api-keys",
        Some(json!({ "name": "chronogazer-inst-1", "scopes": ["read"] })),
        Some(&grant),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{issued}");
    assert_eq!(issued["scopes"], json!(["read"]));
    assert!(issued["id"].is_i64(), "an id is needed to revoke later");
    let key = issued["key"].as_str().expect("plaintext key").to_owned();

    // THE contract #332 depends on: a Hub with no tags configured answers
    // 200 with an empty list, not an error. "接続済み・利用可能なタグなし"
    // must stay distinguishable from a failure.
    let (status, catalog) = tag_space_get(&app.router, "/api/v1/tags", &key).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(catalog["tags"], json!([]));

    // An invalid key is 401 (the client's `AuthFailed` state).
    let (status, _) = tag_space_get(&app.router, "/api/v1/tags", "bh_not_a_real_key").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // A duplicate name comes back as a `validation` error on `name`, NOT a
    // 409 - that is what `banto-hub-bootstrap` matches on before retrying
    // with a later timestamp in the name.
    let (status, error) = admin_request(
        &app.router,
        "POST",
        "/api/api-keys",
        Some(json!({ "name": "chronogazer-inst-1", "scopes": ["read"] })),
        Some(&grant),
        None,
    )
    .await;
    assert!(status.is_client_error());
    assert_eq!(error["kind"], json!("validation"));
    assert_eq!(error["field_errors"][0]["field"], json!("name"));

    // The 4th step: the grant is logged out and is no bearer any more. The
    // issued API key is unaffected (it is the installation's own credential).
    let (status, _) = admin_request(
        &app.router,
        "POST",
        "/api/auth/logout",
        None,
        Some(&grant),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = admin_request(
        &app.router,
        "POST",
        "/api/api-keys",
        Some(json!({ "name": "chronogazer-inst-after-logout", "scopes": ["read"] })),
        Some(&grant),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a logged-out grant is dead"
    );
    let (status, catalog) = tag_space_get(&app.router, "/api/v1/tags", &key).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(catalog["tags"], json!([]));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn locking_down_closes_the_self_issuing_window() {
    let app = test_app("lockdown").await;
    let earlier_grant = loopback_grant(&app.router).await;
    let revoked = app.commissioning.lock_down().await.expect("lock_down");
    assert_eq!(
        revoked, 1,
        "the grant taken before lock-down is revoked by it"
    );

    let (status, body) =
        admin_unauthenticated(&app.router, "GET", "/api/commissioning/status", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["lockedDown"],
        json!(true),
        "the status route stays readable so a client can see WHY it may not issue"
    );

    let (status, _) = admin_unauthenticated(
        &app.router,
        "POST",
        "/api/api-keys",
        Some(json!({ "name": "chronogazer-inst-2", "scopes": ["read"] })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "after lockdown an unauthenticated client must not be able to mint a key"
    );

    // The grant is refused even to loopback once locked down, `status` says
    // so too, and the grant taken before lock-down is already dead.
    let (status, _) = commissioning_grant(&app.router, Some(LOOPBACK_PEER)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = admin_request(
        &app.router,
        "GET",
        "/api/auth/status",
        None,
        None,
        Some(LOOPBACK_PEER),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["grants"]["commissioning"], json!(false), "{body}");
    let (status, _) = admin_request(
        &app.router,
        "POST",
        "/api/api-keys",
        Some(json!({ "name": "chronogazer-inst-3", "scopes": ["read"] })),
        Some(&earlier_grant),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a grant taken before lock-down must not mint a key after it"
    );
}
