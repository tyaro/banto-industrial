//! #332「Hub 側はコード変更ゼロで成立する」という前提を、Hub 側で回帰
//! 固定するテスト。
//!
//! `crates/banto-hub-bootstrap`（クライアント側）はモックサーバー相手に
//! 自分のロジックを固定しているが、**モックが本物と同じ形である**ことまでは
//! 保証できない。このファイルは本物の `api_router` に対して、bootstrap が
//! 依存している 5 つの性質だけを確認する:
//!
//! 1. 試運転中（未ロックダウン）は `GET /api/commissioning/status` が
//!    **未認証で** `{"lockedDown": false}` を返す。
//! 2. その status も `X-Banto-Client` ヘッダ無しでは通らない（CSRF ガードは
//!    管理ルーター全体に掛かる - bootstrap が両方の管理 API にこのヘッダを
//!    付けている根拠）。
//! 3. 試運転中は `POST /api/api-keys` が **未認証で** `read` スコープの
//!    キーを発行し、平文 `key` を一度だけ返す。
//! 4. そのキーで `GET /api/v1/tags` が **タグ 0 件でも 200 + `tags: []`** を
//!    返す（`/api/v1/*` に `X-Banto-Client` は不要）。ここが崩れると
//!    「接続済み・利用可能なタグなし」が「失敗」に化ける。
//! 5. ロックダウン後は `POST /api/api-keys` が未認証で 401 になる
//!    （= クライアントは `NeedsPairing` に落ちる。自己発行の窓が閉じる）。
//!
//! 足場（`TestApp`/`test_app`）は `tests/rbac.rs` のものをベースにした複製
//! （各 `tests/*.rs` は独立クレートとしてコンパイルされ private helper を
//! 共有できないため - `tests/rbac.rs` の doc comment 参照）。**唯一の違いは
//! `lock_down()` を呼ばないこと**で、rbac 側の `test_app` はロックダウン
//! 済み固定なのでそのままでは流用できない。

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

    let users = UsersService::new(pool.clone());
    let audit = AuditLogService::new(pool.clone());
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

    let settings = SettingsService::new(pool.clone());
    let commissioning = CommissioningService::load(settings, users.clone())
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

/// An unauthenticated admin-API request, exactly as `banto-hub-bootstrap`
/// sends it: `X-Banto-Client` but **no** `Authorization` header.
async fn admin_unauthenticated(
    router: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = HttpRequest::builder()
        .method(method)
        .uri(path)
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1);
    let request = match &body {
        Some(value) => builder
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(value).unwrap()))
            .unwrap(),
        None => {
            builder = builder.header("content-type", "application/json");
            builder.body(Body::empty()).unwrap()
        }
    };
    read_response(router, request).await
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_self_issued_read_key_reads_an_empty_catalog_as_200_with_no_tags() {
    let app = test_app("issue").await;

    let (status, issued) = admin_unauthenticated(
        &app.router,
        "POST",
        "/api/api-keys",
        Some(json!({ "name": "chronogazer-inst-1", "scopes": ["read"] })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
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
    let (status, error) = admin_unauthenticated(
        &app.router,
        "POST",
        "/api/api-keys",
        Some(json!({ "name": "chronogazer-inst-1", "scopes": ["read"] })),
    )
    .await;
    assert!(status.is_client_error());
    assert_eq!(error["kind"], json!("validation"));
    assert_eq!(error["field_errors"][0]["field"], json!("name"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn locking_down_closes_the_self_issuing_window() {
    let app = test_app("lockdown").await;
    app.commissioning.lock_down().await.expect("lock_down");

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
}
