use super::super::*;
use crate::api_keys::ApiKeysService;
use crate::db::migrate_memory;
use crate::hub::CollectorManager;
use axum::body::Body;
use axum::http::Request as HttpRequest;
use banto_collect::CollectorOptions;
use banto_server::{Identity, SessionValidation};
use banto_tstore::{Clock, SystemClock};
use tokio::sync::broadcast as tokio_broadcast;
use tower::ServiceExt;

/// R2b-1d: the stored password of connection `id`, opened from its sealed row the way
/// the readers open it (`crate::secrets::open_connection_secret`). `None` = no password.
/// Panics when one is set but unreadable (a test bug, never collapsed to `None`).
pub(super) async fn stored_password(pool: &sqlx::SqlitePool, id: i64) -> Option<String> {
    let service = PlcConnectionService::new(pool.clone());
    let conn = service.get(id).await.unwrap();
    let rows = service.secret_rows(id).await.unwrap();
    crate::secrets::open_connection_secret(
        &crate::secrets::ConnectionSecrets::platform_default(),
        &conn,
        banto_tags::PASSWORD_KEY,
        &rows,
    )
    .expect("a stored password must be readable")
    .map(|plain| plain.as_str().to_owned())
}

pub(super) const CLIENT_HEADER: (&str, &str) = ("X-Banto-Client", "banto");

/// A `CollectorManager` for router-composition tests that never actually
/// rebuild (I1 CRUD tests below only cover auth/RBAC/plumbing, not the
/// collector lifecycle itself - that is `hub.rs`'s and the integration
/// test's job). Points at a real temp dir since `CollectorManager` always
/// needs a `data_dir`, even if `rebuild` is never called in a given test.
///
/// `clock` is injectable (H10 ①): every pre-existing caller goes through
/// [`test_env`], which passes `Arc::new(SystemClock)` - same behavior as
/// before this parameter existed. The expiry E2E tests below go through
/// [`test_env_with_clock`] instead, passing a [`ManualClock`] so they can
/// advance past a key's `expiresAt` deterministically instead of
/// depending on real wall-clock time.
pub(super) fn test_manager_with_clock(
    pool: sqlx::SqlitePool,
    clock: Arc<dyn Clock>,
) -> (Arc<CollectorManager>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let sessions = Arc::new(crate::broker_glue::HubSessions::new(
        banto_broker::BackoffConfig::default(),
        crate::test_support::test_drivers(),
    ));
    let sim_registry = Arc::new(crate::broker_glue::BrokerSimRegistry::new());
    let computed = Arc::new(crate::computed::ComputedEngine::new(Arc::new(
        crate::computed::ServerTagStore::new(),
    )));
    let manager = CollectorManager::new(
        pool,
        dir.path().join("data"),
        clock,
        CollectorOptions::default(),
        sessions,
        sim_registry,
        computed,
    );
    (Arc::new(manager), dir)
}

/// Everything a T0-2 test needs: the assembled router, an admin session
/// token, a viewer session token (RBAC-negative tests, e.g. "viewer may
/// not issue API keys"), the `ApiKeysService` (so a test can seed/inspect
/// keys directly instead of only through REST), and the owning temp dir.
pub(super) struct TestEnv {
    pub(super) router: Router,
    pub(super) admin_token: String,
    pub(super) viewer_token: String,
    pub(super) api_keys: ApiKeysService,
    pub(super) pool: sqlx::SqlitePool,
    /// #341 レビュー対応: router が使っているものと**同じ**
    /// `CollectorManager`。`fail_next_apply_for_test`（テスト専用の
    /// 失敗注入、`crate::hub` 参照）を endpoint 越しのテストから
    /// 呼ぶために持つ。
    pub(super) manager: Arc<CollectorManager>,
    pub(super) _dir: tempfile::TempDir,
}

pub(super) async fn test_env() -> TestEnv {
    test_env_with_clock(Arc::new(SystemClock)).await
}

/// 試運転モード（設計 §5.6・2026-08-30 オーナー決定）のテスト用: [`test_env`]
/// と同じ環境を、明示的に**ロックダウンしないまま**（＝試運転モードの
/// まま）返す - `commissioning_mode_tests`（このモジュール下部）専用。
pub(super) async fn test_env_unlocked() -> TestEnv {
    test_env_with_clock_and_lock(Arc::new(SystemClock), false).await
}

/// [`test_env`] but with an injectable clock (H10 ①) - lets a test
/// create a key with `expiresAt = clock.now_ms() + small`, assert it
/// authenticates, then `advance_ms` past the deadline and assert 401 -
/// deterministically, without depending on real wall-clock time. See
/// [`test_manager_with_clock`]'s doc comment for the same reasoning.
/// Always locked down - see [`test_env_with_clock_and_lock`].
pub(super) async fn test_env_with_clock(clock: Arc<dyn Clock>) -> TestEnv {
    test_env_with_clock_and_lock(clock, true).await
}

/// [`test_env_with_clock`]で共有している実体。`locked_down: false`は
/// [`test_env_unlocked`]専用（試運転モードのテスト） - このファイルの
/// 大半の既存テストは「ロックダウン済み」の挙動そのものを検証している
/// ため、`test_env_with_clock`自体は常に`true`を渡して既存の意味論を
/// 一切変えない（実装指示「既存の認証・監査の挙動を、ロックダウン済み
/// 状態では一切変えないこと」）。
async fn test_env_with_clock_and_lock(clock: Arc<dyn Clock>, locked_down: bool) -> TestEnv {
    let pool = migrate_memory().await.expect("migrate_memory");
    test_env_on_pool(pool, clock, locked_down).await
}

/// 別接続からの書き込みと競合させるテスト用（#528）: メモリ DB ではなく
/// **ファイルの DB**（接続が複数開く）の上に [`test_env`] と同じ環境を組む。
pub(super) async fn test_env_on_file_db(path: &std::path::Path) -> TestEnv {
    let pool = crate::db::init_db(path).await.expect("init_db");
    test_env_on_pool(pool, Arc::new(SystemClock), true).await
}

/// [`test_env_with_clock_and_lock`] の本体（渡された `pool` の上に環境を組む）。
async fn test_env_on_pool(
    pool: sqlx::SqlitePool,
    clock: Arc<dyn Clock>,
    locked_down: bool,
) -> TestEnv {
    let (tx, _rx) = tokio_broadcast::channel(16);
    let users = UsersService::new(Db::Sqlite(pool.clone()));
    let audit = AuditLogService::new(Db::Sqlite(pool.clone()));
    let plc_connections = PlcConnectionService::new(pool.clone());
    let collection_groups = CollectionGroupService::new(pool.clone());
    let tags = TagService::new(pool.clone());
    let api_keys = ApiKeysService::new(pool.clone());
    let (manager, dir) = test_manager_with_clock(pool.clone(), clock);

    users
        .setup_first_user("admin", "password123", "管理者")
        .await
        .expect("setup_first_user");
    users
        .create_user("viewer1", "password123", "閲覧者", Role::Viewer)
        .await
        .expect("create_user viewer");
    let verify_users = users.clone();
    // banto v1.7.0 #204: 本番（`user_auth_state`）と同じ照合を入れる。
    // 検証関数は監査を記録しない版のまま（既存テストの監査件数を変えない）。
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
        SessionValidation::lookup(user_session_lookup(users.clone())),
    );
    let admin_token = auth
        .login("admin", "password123")
        .await
        .expect("admin login");
    let viewer_token = auth
        .login("viewer1", "password123")
        .await
        .expect("viewer login");

    let write_control = Arc::new(crate::write_control::WriteControl::new(false));
    let write_audit = crate::write_audit::WriteAuditService::new(pool.clone());
    let mqtt = Arc::new(crate::mqtt::MqttPublisher::new(manager.clone()));
    // T4: REST/gRPC で共有する1個の rate_limiter（`tag_space_router`の
    // フィールド doc comment参照）と `GrpcServer`（`crate::grpc`のテストで
    // 直接叩かない限り listen はしない - `apply`を呼ばないため）。
    let rate_limiter = Arc::new(AsyncMutex::new(WriteRateLimiter::new(
        crate::write_rate::WriteRateLimitConfig::default(),
    )));
    let grpc_service = crate::grpc::GrpcService::new(
        manager.clone(),
        api_keys.clone(),
        audit.clone(),
        write_audit.clone(),
        write_control.clone(),
        rate_limiter.clone(),
        tx.clone(),
    );
    let grpc_server = Arc::new(crate::grpc::GrpcServer::new(grpc_service));

    // 試運転モードとロックダウン（設計 §5.6・2026-08-30 オーナー決定）:
    // このファイルの大半の既存テストは「ロックダウン済み（従来どおり
    // bearer セッションのログイン必須）」の挙動そのものを検証している
    // ため、共有テスト環境は明示的にロックダウンしてから router を
    // 組み立てる - こうしないと未ロックダウン（試運転モード）の
    // バイパスが効いてしまい、`admin_token`/`viewer_token`を使わない
    // リクエストまで通ってしまう（実装指示「既存の認証・監査の挙動を、
    // ロックダウン済み状態では一切変えないこと」）。試運転モード側の
    // 挙動は`commissioning_mode_tests`（このモジュール下部）で別途
    // 専用のテスト環境を組んで検証する。
    let settings = SettingsService::new(Db::Sqlite(pool.clone()));
    let commissioning = CommissioningService::load(settings, users.clone(), auth.clone())
        .await
        .expect("CommissioningService::load");
    if locked_down {
        commissioning
            .lock_down()
            .await
            .expect("lock_down the shared test environment");
    }

    let router = api_router(
        users,
        audit,
        plc_connections,
        collection_groups,
        tags,
        api_keys.clone(),
        manager.clone(),
        auth,
        commissioning,
        tx,
        false,
        write_control,
        write_audit,
        mqtt,
        grpc_server,
        rate_limiter,
        crate::profile_paths::DEFAULT_PROFILE_ID.to_string(),
    );
    TestEnv {
        router,
        admin_token,
        viewer_token,
        api_keys,
        pool,
        manager,
        _dir: dir,
    }
}

/// Backward-compatible shim for the pre-T0-2 tests below that only need
/// an admin session token.
pub(super) async fn router_with_token() -> (Router, String, tempfile::TempDir) {
    let env = test_env().await;
    (env.router, env.admin_token, env._dir)
}

/// `POST /api/api-keys` through the admin surface (bearer + CSRF +
/// admin RBAC). Returns the parsed JSON body (`{ id, name, prefix,
/// scopes, key }` on success).
pub(super) async fn issue_api_key(
    router: &Router,
    token: &str,
    name: &str,
    scopes: &[&str],
) -> (StatusCode, serde_json::Value) {
    let response = router
        .clone()
        .oneshot(
            HttpRequest::post("/api/api-keys")
                .header("Authorization", format!("Bearer {token}"))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "name": name, "scopes": scopes }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

/// 同型の汎用ヘルパ(H10 ③ のタグ seed フィクスチャで複数リソースを
/// 作るため、`apps/banto-hub/host/tests/grpc.rs`/`apps/banto-hub/host/tests/computed.rs` の `admin_post` と
/// 同じ形をここにも用意する)。
pub(super) async fn admin_post(
    router: &Router,
    path: &str,
    token: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let response = router
        .clone()
        .oneshot(
            HttpRequest::post(path)
                .header("Authorization", format!("Bearer {token}"))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

/// `admin_post` の `PUT` 版 - S1（外部 DB 連携）のパスワード意味論
/// テスト（`password: None`が既存値維持、`Some("")`が消去、
/// `Some(s)`が置換）がまとめて必要とするため追加。
pub(super) async fn admin_put(
    router: &Router,
    path: &str,
    token: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let response = router
        .clone()
        .oneshot(
            HttpRequest::put(path)
                .header("Authorization", format!("Bearer {token}"))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

/// `admin_post` の `DELETE` 版 - T19 S2-b（UX-38）のカスケード削除
/// テストがまとめて必要とするため追加。
pub(super) async fn admin_delete(
    router: &Router,
    path: &str,
    token: &str,
) -> (StatusCode, serde_json::Value) {
    let response = router
        .clone()
        .oneshot(
            HttpRequest::delete(path)
                .header("Authorization", format!("Bearer {token}"))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

/// `admin_post`/`admin_put`/`admin_delete`の`GET`版 - 外部 DB 連携 S4の
/// sink group テストがまとめて必要とするため追加。
pub(super) async fn admin_get(
    router: &Router,
    path: &str,
    token: &str,
) -> (StatusCode, serde_json::Value) {
    let response = router
        .clone()
        .oneshot(
            HttpRequest::get(path)
                .header("Authorization", format!("Bearer {token}"))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

/// 認証ヘッダ付きで `/api/v1/*` に `GET` する小さなヘルパ。`/api/v1/*`
/// は CSRF 対象外なので `X-Banto-Client` は付けない
/// （`v1_tags_requires_auth_but_not_the_csrf_header` 参照）。
pub(super) async fn v1_get(
    router: &Router,
    key: &str,
    path: &str,
) -> (StatusCode, serde_json::Value) {
    let response = router
        .clone()
        .oneshot(
            HttpRequest::get(path)
                .header("Authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

/// `v1_get`の`PUT`版 - 外部 DB 連携 S4の`PUT /api/sink/status`テスト用。
/// `/api/sink/*`も`require_sink_admin`が自前で認証するため CSRF 対象外
/// （`/api/v1/*`・`POST /mcp`と同じ扱い）。
pub(super) async fn v1_put(
    router: &Router,
    key: &str,
    path: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let response = router
        .clone()
        .oneshot(
            HttpRequest::put(path)
                .header("Authorization", format!("Bearer {key}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

/// admin 系ルーター(CSRF ヘッダ必須)への `POST` - `issue_api_key` と
/// `line1.fast.temp01`/`line1.fast.temp02`/`line2.slow.press01` の3タグを
/// 別接続・別グループで作る(admin REST 経由 - I1 の書き込みハンドラは
/// 成功のたびに `CollectorManager::rebuild` を自動で呼ぶので、明示的な
/// rebuild 呼び出しは不要、このモジュールの doc comment「I1 CRUD 書き
/// 込み後の再構築」参照)。catalog は registry の読み取りだけで完結する
/// (`crate::hub::build_catalog`)ため、実際に PLC へ繋がらないポートでも
/// rebuild は成功しカタログへ反映される -
/// `tags_create_via_admin_router_rebuilds_the_catalog` と同じ判断。
pub(super) async fn seed_scope_fixture(router: &Router, admin_token: &str) {
    let (status, conn1) = admin_post(
        router,
        "/api/plc-connections",
        admin_token,
        json!({ "name": "line1", "host": "127.0.0.1", "port": 15101 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn1:?}");
    let (status, group1) = admin_post(
        router,
        "/api/collection-groups",
        admin_token,
        json!({ "name": "fast", "plcConnectionId": conn1["id"], "periodMs": 100 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{group1:?}");
    for (name, address) in [("temp01", "40001"), ("temp02", "40003")] {
        let (status, tag) = admin_post(
            router,
            "/api/tags",
            admin_token,
            json!({
                "name": name,
                "collectionGroupId": group1["id"],
                "address": address,
                "dataType": "i16",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{tag:?}");
    }

    let (status, conn2) = admin_post(
        router,
        "/api/plc-connections",
        admin_token,
        json!({ "name": "line2", "host": "127.0.0.1", "port": 15102 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn2:?}");
    let (status, group2) = admin_post(
        router,
        "/api/collection-groups",
        admin_token,
        json!({ "name": "slow", "plcConnectionId": conn2["id"], "periodMs": 1000 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{group2:?}");
    let (status, tag) = admin_post(
        router,
        "/api/tags",
        admin_token,
        json!({
            "name": "press01",
            "collectionGroupId": group2["id"],
            "address": "40001",
            "dataType": "i16",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{tag:?}");
}

pub(super) fn session_request(
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<serde_json::Value>,
) -> HttpRequest<Body> {
    let mut builder = HttpRequest::builder()
        .method(method)
        .uri(path)
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1);
    if let Some(token) = token {
        builder = builder.header("Authorization", format!("Bearer {token}"));
    }
    match body {
        Some(body) => builder
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    }
}

pub(super) async fn session_call(
    router: &Router,
    request: HttpRequest<Body>,
) -> (StatusCode, serde_json::Value) {
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

pub(super) async fn session_login(router: &Router, username: &str, remember: bool) -> String {
    let (status, body) = session_call(
        router,
        session_request(
            "POST",
            "/api/auth/login",
            None,
            Some(json!({ "username": username, "password": "password123", "remember": remember })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["token"]
        .as_str()
        .unwrap_or_else(|| panic!("login for {username} returned no token: {body}"))
        .to_string()
}
