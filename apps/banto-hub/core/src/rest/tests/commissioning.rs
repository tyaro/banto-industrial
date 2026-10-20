use super::super::*;
use super::common::{
    admin_delete, admin_get, admin_post, issue_api_key, seed_scope_fixture, test_env,
    test_env_unlocked, v1_get, CLIENT_HEADER,
};
use crate::db::migrate_memory;
use axum::body::Body;
use axum::http::Request as HttpRequest;
use tower::ServiceExt;

// --- 試運転モードとロックダウン (設計 §5.6・2026-08-30 オーナー決定、
// banto v3.0.0 ADR-0017 で grant 方式に、2026-10-04) --------------------

/// loopback の peer。`tower::oneshot` には `ConnectInfo` が無いので、本番の
/// `banto_server::start` の経路と同じものを extensions に明示する（banto の
/// `rest/tests.rs` と同じ手筋）。
const LOOPBACK_PEER: std::net::SocketAddr =
    std::net::SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 40000);
const LAN_PEER: std::net::SocketAddr = std::net::SocketAddr::new(
    std::net::IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 20)),
    40000,
);

/// `POST /api/auth/grant/commissioning` を `peer` から叩く。
async fn grant_request(
    router: &Router,
    peer: Option<std::net::SocketAddr>,
) -> (StatusCode, serde_json::Value) {
    let mut request = HttpRequest::post("/api/auth/grant/commissioning")
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .body(Body::empty())
        .unwrap();
    if let Some(peer) = peer {
        request
            .extensions_mut()
            .insert(axum::extract::ConnectInfo(peer));
    }
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

/// 試運転モードの loopback の呼び出しが受け取る grant トークン。
async fn commissioning_grant(router: &Router) -> String {
    let (status, body) = grant_request(router, Some(LOOPBACK_PEER)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["success"], true, "{body}");
    body["token"].as_str().expect("grant token").to_string()
}

/// `GET /api/auth/status` を `peer` から叩いたときの `grants.commissioning`。
async fn commissioning_availability(
    router: &Router,
    peer: Option<std::net::SocketAddr>,
) -> serde_json::Value {
    let mut request = HttpRequest::get("/api/auth/status")
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .body(Body::empty())
        .unwrap();
    if let Some(peer) = peer {
        request
            .extensions_mut()
            .insert(axum::extract::ConnectInfo(peer));
    }
    let response = router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    body["grants"]["commissioning"].clone()
}

/// `Authorization` 無しで管理 REST を叩いたときの status。
async fn unauthenticated_status(router: &Router, path: &str) -> StatusCode {
    router
        .clone()
        .oneshot(
            HttpRequest::get(path)
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

/// 試運転モード（未ロックダウン）でも、管理 REST は `Authorization` 無しでは
/// 401（banto v3.0.0 追従で「無認証で通す」分岐は無くなった）。代わりに
/// loopback の peer は `POST /api/auth/grant/commissioning` で合成 admin の
/// grant を受け取り、それで `GET /api/users`（`RoleGuard` で admin 限定）が
/// 通る。`GET /api/auth/identity` はその identity を `kind: "commissioning"`
/// 付きで返す（画面の `SessionKind` の由来）。
#[tokio::test]
async fn commissioning_mode_requires_the_grant_instead_of_passing_everyone() {
    let env = test_env_unlocked().await;
    assert_eq!(
        unauthenticated_status(&env.router, "/api/users").await,
        StatusCode::UNAUTHORIZED,
        "no unauthenticated admin request passes, even while commissioning"
    );

    let token = commissioning_grant(&env.router).await;
    let (status, users) = admin_get(&env.router, "/api/users", &token).await;
    assert_eq!(status, StatusCode::OK, "{users:?}");

    let (status, identity) = admin_get(&env.router, "/api/auth/identity", &token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(identity["kind"], "commissioning", "{identity}");
    assert_eq!(identity["id"], crate::commissioning::SYNTHETIC_ACTOR_ID);
    assert_eq!(identity["role"], "admin");
}

/// 発行の条件（ADR-0017 §2、`commissioning_grant_spec`）: loopback の peer
/// だけ。LAN の peer・peer 不明は 403（fail closed）。`GET /api/auth/status`
/// の `grants.commissioning` も同じ判定。ロックダウン済みなら loopback でも
/// 403 で、status も false。
#[tokio::test]
async fn the_commissioning_grant_is_loopback_only_and_closed_once_locked_down() {
    let env = test_env_unlocked().await;
    assert_eq!(
        commissioning_availability(&env.router, Some(LOOPBACK_PEER)).await,
        true
    );
    assert_eq!(
        commissioning_availability(&env.router, Some(LAN_PEER)).await,
        false
    );
    assert_eq!(commissioning_availability(&env.router, None).await, false);
    let (status, _) = grant_request(&env.router, Some(LAN_PEER)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = grant_request(&env.router, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // 登録されていない kind は 404（閲覧公開はこの Hub に無い）。
    let response = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/auth/grant/publicViewer")
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let locked = test_env().await;
    assert_eq!(
        commissioning_availability(&locked.router, Some(LOOPBACK_PEER)).await,
        false
    );
    let (status, _) = grant_request(&locked.router, Some(LOOPBACK_PEER)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// 対照実験: ロックダウン済み（既存の共有テスト環境 `test_env()`）では
/// 従来どおり `Authorization` ヘッダ無しの管理 API アクセスが 401 になる
/// ことを明示的に固定する。
#[tokio::test]
async fn locked_down_admin_api_requires_a_token() {
    let env = test_env().await;
    assert_eq!(
        unauthenticated_status(&env.router, "/api/users").await,
        StatusCode::UNAUTHORIZED
    );
}

/// `GET /api/commissioning/status` は試運転モード中でも
/// `Authorization` ヘッダ無しで読める必要がある（bootstrap と画面が、
/// grant を取る前に状態を知るため）。
#[tokio::test]
async fn unlocked_commissioning_status_is_readable_without_any_token() {
    let env = test_env_unlocked().await;
    let response = env
        .router
        .oneshot(
            HttpRequest::get("/api/commissioning/status")
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["lockedDown"], false);
}

/// 同じ読み取り専用ステータスは、ロックダウン済みでも同様に
/// `Authorization` ヘッダ無しで読める。
#[tokio::test]
async fn locked_down_commissioning_status_is_still_readable_without_a_token() {
    let env = test_env().await;
    let response = env
        .router
        .oneshot(
            HttpRequest::get("/api/commissioning/status")
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["lockedDown"], true);
}

/// `POST /api/commissioning/lock-down`（設計 §5.6「遷移」の唯一の正方向
/// 経路）を試運転の grant 自身で叩く。成功すると状態がロックダウン済みへ
/// 切り替わり、`CommissioningService::lock_down` が保存の直後に試運転 grant
/// のトークンを失効させる（ADR-0017 §5）ので、**同じ grant での次の要求は
/// 401**、`GET /api/auth/identity` は `200 null`。監査には actor
/// `commissioning`（ロックダウンの前に確定した）と `revokedGrants` が残る。
#[tokio::test]
async fn commissioning_lock_down_flips_state_and_revokes_the_grant() {
    let env = test_env_unlocked().await;
    let token = commissioning_grant(&env.router).await;
    let other = commissioning_grant(&env.router).await;

    let (status, body) = admin_post(
        &env.router,
        "/api/commissioning/lock-down",
        &token,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["lockedDown"], true);

    for revoked in [&token, &other] {
        let (status, _) = admin_get(&env.router, "/api/users", revoked).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, identity) = admin_get(&env.router, "/api/auth/identity", revoked).await;
        assert_eq!(status, StatusCode::OK);
        assert!(identity.is_null(), "{identity}");
    }
    // アカウントのセッションは巻き込まない。
    let (status, _) = admin_get(&env.router, "/api/users", &env.admin_token).await;
    assert_eq!(status, StatusCode::OK);
    // 以後 grant は出ない。
    let (status, _) = grant_request(&env.router, Some(LOOPBACK_PEER)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (actor, detail): (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT actor_username, detail FROM audit_log WHERE action = 'lock_down' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(
        actor.as_deref(),
        Some(crate::commissioning::SYNTHETIC_ACTOR_ID)
    );
    let detail: serde_json::Value = serde_json::from_str(&detail.expect("detail")).unwrap();
    assert_eq!(detail["revokedGrants"], 2, "{detail}");
}

/// 実装指示「管理者0件でロックダウンしようとするとエラー」の REST
/// 経由での確認。既存の唯一の admin アカウントを editor へ降格させた
/// 上で（生の SQL 経由 - `UsersService::update_user`自身の「最後の
/// admin を降格できない」ガードを迂回する必要がある）、ロックダウンを
/// 試みると明確なエラー（4xx、`ApiError` 形）になり、状態も試運転モードの
/// まま変わらず、**grant も失効しない**（失敗の経路でフラグも失効も
/// 動かさない）ことを確認する。
#[tokio::test]
async fn commissioning_lock_down_fails_without_any_admin_account() {
    let env = test_env_unlocked().await;
    sqlx::query("UPDATE users SET role = 'editor' WHERE username = 'admin'")
        .execute(&env.pool)
        .await
        .expect("downgrade the only admin via raw SQL");
    let token = commissioning_grant(&env.router).await;

    let (status, _) = admin_post(
        &env.router,
        "/api/commissioning/lock-down",
        &token,
        json!({}),
    )
    .await;
    assert!(
        status.is_client_error(),
        "expected a 4xx error, got {status}"
    );

    let response = env
        .router
        .clone()
        .oneshot(
            HttpRequest::get("/api/commissioning/status")
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["lockedDown"], false);
    let (status, _) = admin_get(&env.router, "/api/users", &token).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the grant survives a failed lock-down"
    );
}

/// ADR-0017 §3: 試運転の grant セッション（行を持たない）でもユーザーを削除
/// できる（自己削除ガードの行 id 照合だけが外れる）。admin の床と「最後の
/// admin は消せない」はそのまま。
#[tokio::test]
async fn the_commissioning_grant_can_delete_users_but_not_the_last_admin() {
    let env = test_env_unlocked().await;
    let token = commissioning_grant(&env.router).await;
    let (status, users) = admin_get(&env.router, "/api/users", &token).await;
    assert_eq!(status, StatusCode::OK);
    let id_of = |name: &str| {
        users
            .as_array()
            .unwrap()
            .iter()
            .find(|u| u["username"] == name)
            .map(|u| u["id"].as_i64().unwrap())
            .unwrap()
    };
    let viewer = id_of("viewer1");
    let admin = id_of("admin");

    // 最後の admin のガードは `BantoError::Other`（従来どおり 500 の形）。
    let (status, body) = admin_delete(&env.router, &format!("/api/users/{admin}"), &token).await;
    assert_ne!(
        status,
        StatusCode::NO_CONTENT,
        "the last admin stays: {body}"
    );
    assert!(!status.is_success(), "{status} {body}");
    let (_, after) = admin_get(&env.router, "/api/users", &token).await;
    assert!(
        after
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u["username"] == "admin"),
        "{after}"
    );
    let (status, _) = admin_delete(&env.router, &format!("/api/users/{viewer}"), &token).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

/// 競合の統合テスト（ADR-0017 §2「発行と失効の直列化」、移行 PR の必須項目）:
/// `enabled()` が true を返した発行要求を**挿入の直前で保留** → ロックダウン
/// （保存 → `revoke_grant_tokens`）が完了 → 発行要求を再開 → **403**。直後の
/// `GET /api/auth/identity`（先に発行されていたトークン）は `200 null`、
/// 認証が必要な `GET /api/users` は 401、status の `grants.commissioning` は
/// false。保留は `GrantSpec.enabled` を包んで「判定の結果を返す前に待つ」
/// ことで作る（判定と挿入のあいだに失効が割り込む形）。開いていたストリームが
/// 再検証で閉じることは `tests/stream.rs` の
/// `a_commissioning_grant_stream_closes_within_an_interval_after_lock_down`
/// （実 TCP が要る）が同じ失効で固定する。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_issuance_parked_before_the_insert_loses_to_a_lock_down_that_completed_meanwhile() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let pool = migrate_memory().await.expect("migrate_memory");
    let users = UsersService::new(Db::Sqlite(pool.clone()));
    let audit = AuditLogService::new(Db::Sqlite(pool.clone()));
    users
        .setup_first_user("admin", "password123", "管理者")
        .await
        .expect("setup_first_user");
    let auth = user_auth_state(users.clone(), audit.clone());
    let settings = SettingsService::new(Db::Sqlite(pool.clone()));
    let commissioning = CommissioningService::load(settings, users.clone(), auth.clone())
        .await
        .expect("CommissioningService::load");

    // 本物の試運転 grant の仕様を包む: `park` が立っている間、判定（true）を
    // 返す前に `judged` を鳴らして `release` を待つ。
    let park = Arc::new(AtomicBool::new(false));
    let judged = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mut spec = commissioning.grant_spec();
    let inner = spec.enabled.clone();
    spec.enabled = {
        let park = park.clone();
        let judged = judged.clone();
        let release = release.clone();
        Arc::new(move || {
            let judgment = inner();
            let park = park.clone();
            let judged = judged.clone();
            let release = release.clone();
            Box::pin(async move {
                let verdict = judgment.await;
                if park.load(Ordering::SeqCst) {
                    judged.notify_one();
                    release.notified().await;
                }
                verdict
            })
        })
    };
    let mut registry = GrantRegistry::new();
    registry.register(spec).expect("register");
    let router = extra_auth_router(
        users.clone(),
        auth.clone(),
        audit.clone(),
        false,
        Arc::new(registry),
        None,
    )
    .merge(auth_routes(auth.clone()))
    .merge(users_router(users, audit, auth.clone()));

    // 先に 1 つ発行しておく（ロックダウンで失効する対象）。
    let earlier = commissioning_grant(&router).await;
    assert!((spec_enabled_probe(&router).await), "still commissioning");

    // 保留する発行要求 B。
    park.store(true, Ordering::SeqCst);
    let parked_router = router.clone();
    let parked =
        tokio::spawn(async move { grant_request(&parked_router, Some(LOOPBACK_PEER)).await });
    judged.notified().await; // B は true を取得し、挿入の直前で止まっている。

    // ロックダウン完了（保存 → 失効）。
    let revoked = commissioning.lock_down().await.expect("lock_down");
    assert_eq!(revoked, 1);
    park.store(false, Ordering::SeqCst);
    release.notify_one();

    let (status, body) = parked.await.unwrap();
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "the parked issuance must not insert after the revocation: {body}"
    );
    assert!(body["token"].is_null(), "{body}");
    assert!(auth.identity_for(&earlier).is_none());

    let (status, identity) = admin_get(&router, "/api/auth/identity", &earlier).await;
    assert_eq!(status, StatusCode::OK);
    assert!(identity.is_null(), "{identity}");
    let (status, _) = admin_get(&router, "/api/users", &earlier).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        commissioning_availability(&router, Some(LOOPBACK_PEER)).await,
        false
    );
}

/// `GET /api/auth/status` の `grants.commissioning`（loopback）を bool で。
async fn spec_enabled_probe(router: &Router) -> bool {
    commissioning_availability(router, Some(LOOPBACK_PEER)).await == true
}

// --- 試運転モード対応: 管理系 `/api/status`・`/api/values`・`/api/tag-catalog`
// (設計 §5.6・2026-08-31 オーナー決定「案A」) -----------------------------
//
// 管理 UI（ブラウザ）が読む `/api/status`・`/api/values`・`/api/tag-catalog`
// は管理系ルーター（`admin_status_router`）にあり、`/api/v1/*`
// （`require_tag_space_auth`、API キー or セッション）とは別。banto v3.0.0
// 追従で、試運転中もこれらは grant トークンで読む（トークン無しは 401）。

/// 試運転モード中: grant トークンで `/api/status`・`/api/values`・
/// `/api/tag-catalog` が読め、トークン無しは 401。
#[tokio::test]
async fn commissioning_grant_reads_admin_status_values_and_catalog() {
    let env = test_env_unlocked().await;
    let token = commissioning_grant(&env.router).await;
    for path in ["/api/status", "/api/values", "/api/tag-catalog"] {
        assert_eq!(
            unauthenticated_status(&env.router, path).await,
            StatusCode::UNAUTHORIZED,
            "{path}"
        );
        let (status, body) = admin_get(&env.router, path, &token).await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
    }
}

/// 対照実験: ロックダウン済みでは、他の管理系ルーターと同様
/// `Authorization` ヘッダ無しの `/api/status`・`/api/values`・
/// `/api/tag-catalog` は 401（`locked_down_admin_api_requires_a_token`と同型）。
#[tokio::test]
async fn locked_down_admin_status_values_and_catalog_require_a_token() {
    let env = test_env().await;
    for path in ["/api/status", "/api/values", "/api/tag-catalog"] {
        assert_eq!(
            unauthenticated_status(&env.router, path).await,
            StatusCode::UNAUTHORIZED,
            "{path}"
        );
        let (status, _) = admin_get(&env.router, path, &env.viewer_token).await;
        assert_eq!(status, StatusCode::OK, "{path}: read-only, any role");
    }
}

/// 回帰確認: catalog本体を`build_catalog_response`へ切り出した後も、
/// `/api/v1/tags`は従来どおり API キー認証で動く（`v1_status_still_works_with_an_api_key_after_the_admin_status_split`
/// と同型、対象だけ`/api/v1/tags`）。
#[tokio::test]
async fn v1_tags_still_works_with_an_api_key_after_the_admin_catalog_split() {
    let env = test_env().await;
    seed_scope_fixture(&env.router, &env.admin_token).await;
    let (status, issued) =
        issue_api_key(&env.router, &env.admin_token, "catalog-reader", &["read"]).await;
    assert_eq!(status, StatusCode::CREATED, "{issued:?}");
    let key = issued["key"].as_str().expect("key should be present");

    let (status, body) = v1_get(&env.router, key, "/api/v1/tags").await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert!(body["tags"].as_array().is_some_and(|tags| !tags.is_empty()));
}

/// 共有ロジックの担保: 管理系 `/api/tag-catalog`（camelCase）と
/// `/api/v1/tags`（snake_case）が同じ情報を返すこと -
/// 両者が[`build_catalog_response`]を共有していることの直接的な証拠
/// （`admin_status_and_values_carry_the_same_information_as_v1`と同型）。
#[tokio::test]
async fn admin_tag_catalog_carries_the_same_information_as_v1_tags() {
    let env = test_env().await;
    seed_scope_fixture(&env.router, &env.admin_token).await;

    let (status, v1_body) = v1_get(&env.router, &env.admin_token, "/api/v1/tags").await;
    assert_eq!(status, StatusCode::OK, "{v1_body:?}");

    let response = env
        .router
        .oneshot(
            HttpRequest::get("/api/tag-catalog")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let admin_body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(admin_body["revision"], v1_body["revision"]);
    assert_eq!(admin_body["collectionMode"], v1_body["collection_mode"]);

    let mut v1_tag_names: Vec<&str> = v1_body["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["external_name"].as_str().unwrap())
        .collect();
    let mut admin_tag_names: Vec<&str> = admin_body["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["externalName"].as_str().unwrap())
        .collect();
    v1_tag_names.sort_unstable();
    admin_tag_names.sort_unstable();
    assert!(
        !v1_tag_names.is_empty(),
        "fixture should seed at least one tag"
    );
    assert_eq!(v1_tag_names, admin_tag_names);

    // camelCase 変換が正しく効いていること（フィールド名だけでなく値も
    // 一致すること）の代表サンプル1件。
    let v1_first = v1_body["tags"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["external_name"] == v1_tag_names[0])
        .unwrap();
    let admin_first = admin_body["tags"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["externalName"] == v1_tag_names[0])
        .unwrap();
    assert_eq!(admin_first["tagKey"], v1_first["tag_key"]);
    assert_eq!(admin_first["dataType"], v1_first["data_type"]);
    assert_eq!(admin_first["periodMs"], v1_first["period_ms"]);
    assert_eq!(
        admin_first["effectiveSimulation"],
        v1_first["effective_simulation"]
    );
    assert_eq!(admin_first["valueSource"], v1_first["value_source"]);
}
