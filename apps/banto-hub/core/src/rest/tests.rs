use super::*;
use crate::api_keys::ApiKeysService;
use crate::db::migrate_memory;
use crate::pending_changes::PendingChangeState;
use axum::body::Body;
use axum::http::Request as HttpRequest;
use banto_tstore::{ManualClock, SystemClock};
use tokio::sync::broadcast as tokio_broadcast;
use tower::ServiceExt;

mod address_writable;
mod api_keys;
mod audit_retention;
mod cascade_delete;
mod common;
mod general;
mod postgres_connections;
mod store_retention;
mod tag_list_query;
mod tag_read_scope;

use common::{
    admin_delete, admin_get, admin_post, admin_put, issue_api_key, seed_scope_fixture, test_env,
    test_env_unlocked, test_env_with_clock, test_manager_with_clock, v1_get, v1_put, CLIENT_HEADER,
};

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

// --- 外部 DB 連携 S4（docs/banto-hub-external-db-design.md §5.2・§6-13・
// §6-15）: DB Sink の Hub 側（`hub_sink_groups` CRUD、`GET
// /api/sink/config`、`PUT /api/sink/status`、`GET /api/status`の
// `sink`節） ------------------------------------------------------------

/// PLC 接続1つ+収集グループ+タグ1つ、および postgres 接続1つを作る -
/// sink group のテストが共通に必要とするフィクスチャ
/// （`seed_scope_fixture`と同じ「admin REST 経由で組み立てる」方針）。
/// 戻り値は `(postgres 接続の id, タグの id)`。
async fn seed_sink_fixture(router: &Router, admin_token: &str) -> (i64, i64) {
    let (status, conn) = admin_post(
        router,
        "/api/plc-connections",
        admin_token,
        json!({ "name": "line1", "host": "127.0.0.1", "port": 15201 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");
    let (status, group) = admin_post(
        router,
        "/api/collection-groups",
        admin_token,
        json!({ "name": "fast", "plcConnectionId": conn["id"], "periodMs": 100 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{group:?}");
    let (status, tag) = admin_post(
        router,
        "/api/tags",
        admin_token,
        json!({
            "name": "temp01",
            "collectionGroupId": group["id"],
            "address": "40001",
            "dataType": "i16",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{tag:?}");

    let (status, pg) = admin_post(
        router,
        "/api/plc-connections",
        admin_token,
        json!({
            "name": "erp-db",
            "protocol": "postgres",
            "host": "10.0.0.50",
            "port": 5432,
            "database": "erp",
            "username": "reader",
            "password": "s3cret",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{pg:?}");

    (pg["id"].as_i64().unwrap(), tag["id"].as_i64().unwrap())
}

fn sink_group_body(db_connection_id: i64, tag_id: i64) -> serde_json::Value {
    json!({
        "name": "line1-log",
        "dbConnectionId": db_connection_id,
        "mode": "interval",
        "intervalMs": 1000,
        "tableName": "public.tag_history",
        "storeBad": false,
        "enabled": true,
        "tagIds": [tag_id],
    })
}

#[tokio::test]
async fn sink_groups_crud_happy_path() {
    let env = test_env().await;
    let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;

    let (status, created) = admin_post(
        &env.router,
        "/api/sink/groups",
        &env.admin_token,
        sink_group_body(db_connection_id, tag_id),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created:?}");
    assert_eq!(created["name"], "line1-log");
    assert_eq!(created["dbConnectionId"], db_connection_id);
    assert_eq!(created["tagIds"], json!([tag_id]));
    let id = created["id"].as_i64().unwrap();

    let (status, listed) = admin_get(&env.router, "/api/sink/groups", &env.admin_token).await;
    assert_eq!(status, StatusCode::OK, "{listed:?}");
    assert_eq!(listed.as_array().unwrap().len(), 1);

    let (status, fetched) = admin_get(
        &env.router,
        &format!("/api/sink/groups/{id}"),
        &env.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{fetched:?}");
    assert_eq!(fetched, created);

    let mut updated_body = sink_group_body(db_connection_id, tag_id);
    updated_body["intervalMs"] = json!(2000);
    updated_body["enabled"] = json!(false);
    let (status, updated) = admin_put(
        &env.router,
        &format!("/api/sink/groups/{id}"),
        &env.admin_token,
        updated_body,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated:?}");
    assert_eq!(updated["intervalMs"], 2000);
    assert_eq!(updated["enabled"], false);

    let (status, _) = admin_delete(
        &env.router,
        &format!("/api/sink/groups/{id}"),
        &env.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = admin_get(
        &env.router,
        &format!("/api/sink/groups/{id}"),
        &env.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn sink_groups_create_requires_editor() {
    let env = test_env().await;
    let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
    let (status, body) = admin_post(
        &env.router,
        "/api/sink/groups",
        &env.viewer_token,
        sink_group_body(db_connection_id, tag_id),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body:?}");
}

#[tokio::test]
async fn sink_groups_create_rejects_empty_name() {
    let env = test_env().await;
    let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
    let mut body = sink_group_body(db_connection_id, tag_id);
    body["name"] = json!("   ");
    let (status, body) = admin_post(&env.router, "/api/sink/groups", &env.admin_token, body).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
}

#[tokio::test]
async fn sink_groups_create_rejects_too_long_name() {
    let env = test_env().await;
    let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
    let mut body = sink_group_body(db_connection_id, tag_id);
    body["name"] = json!("x".repeat(65));
    let (status, body) = admin_post(&env.router, "/api/sink/groups", &env.admin_token, body).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
}

#[tokio::test]
async fn sink_groups_create_rejects_duplicate_name() {
    let env = test_env().await;
    let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
    let (status, _) = admin_post(
        &env.router,
        "/api/sink/groups",
        &env.admin_token,
        sink_group_body(db_connection_id, tag_id),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = admin_post(
        &env.router,
        "/api/sink/groups",
        &env.admin_token,
        sink_group_body(db_connection_id, tag_id),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
}

#[tokio::test]
async fn sink_groups_create_rejects_bad_mode() {
    let env = test_env().await;
    let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
    let mut body = sink_group_body(db_connection_id, tag_id);
    body["mode"] = json!("hourly");
    let (status, body) = admin_post(&env.router, "/api/sink/groups", &env.admin_token, body).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
}

#[tokio::test]
async fn sink_groups_create_rejects_out_of_range_interval_ms() {
    let env = test_env().await;
    let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
    let mut body = sink_group_body(db_connection_id, tag_id);
    body["intervalMs"] = json!(10);
    let (status, body) = admin_post(&env.router, "/api/sink/groups", &env.admin_token, body).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
}

#[tokio::test]
async fn sink_groups_create_rejects_invalid_table_name() {
    let env = test_env().await;
    let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
    let mut body = sink_group_body(db_connection_id, tag_id);
    body["tableName"] = json!("\"quoted\"");
    let (status, body) = admin_post(&env.router, "/api/sink/groups", &env.admin_token, body).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
}

#[tokio::test]
async fn sink_groups_create_rejects_empty_tag_ids() {
    let env = test_env().await;
    let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
    let _ = tag_id;
    let mut body = sink_group_body(db_connection_id, tag_id);
    body["tagIds"] = json!([]);
    let (status, body) = admin_post(&env.router, "/api/sink/groups", &env.admin_token, body).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
}

#[tokio::test]
async fn sink_groups_create_rejects_duplicate_tag_ids() {
    let env = test_env().await;
    let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
    let mut body = sink_group_body(db_connection_id, tag_id);
    body["tagIds"] = json!([tag_id, tag_id]);
    let (status, body) = admin_post(&env.router, "/api/sink/groups", &env.admin_token, body).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
}

#[tokio::test]
async fn sink_groups_create_rejects_unknown_tag_id() {
    let env = test_env().await;
    let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
    let mut body = sink_group_body(db_connection_id, tag_id);
    body["tagIds"] = json!([999_999]);
    let (status, body) = admin_post(&env.router, "/api/sink/groups", &env.admin_token, body).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
}

/// 接続が `protocol: postgres` でなければ422（実装指示: 「connection
/// that is not postgres → 422」）。
#[tokio::test]
async fn sink_groups_create_rejects_non_postgres_connection() {
    let env = test_env().await;
    let (_db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
    let (status, plc) = admin_get(&env.router, "/api/plc-connections", &env.admin_token).await;
    assert_eq!(status, StatusCode::OK, "{plc:?}");
    let modbus_connection_id = plc
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["protocol"] == "modbus-tcp")
        .unwrap()["id"]
        .as_i64()
        .unwrap();

    let mut body = sink_group_body(modbus_connection_id, tag_id);
    body["dbConnectionId"] = json!(modbus_connection_id);
    let (status, body) = admin_post(&env.router, "/api/sink/groups", &env.admin_token, body).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
}

#[tokio::test]
async fn sink_groups_create_rejects_unknown_connection() {
    let env = test_env().await;
    let (_db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
    let body = sink_group_body(999_999, tag_id);
    let (status, body) = admin_post(&env.router, "/api/sink/groups", &env.admin_token, body).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
}

/// この接続を参照する sink group がある間は `plc_connections` の削除を
/// 拒否する（RESTRICT 相当、実装指示: 「connection that a sink group
/// references is rejected with a clear message」）。
#[tokio::test]
async fn deleting_a_connection_referenced_by_a_sink_group_is_rejected() {
    let env = test_env().await;
    let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
    let (status, _) = admin_post(
        &env.router,
        "/api/sink/groups",
        &env.admin_token,
        sink_group_body(db_connection_id, tag_id),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = admin_delete(
        &env.router,
        &format!("/api/plc-connections/{db_connection_id}"),
        &env.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    let message = body["field_errors"][0]["message"].as_str().unwrap_or("");
    assert!(
        message.contains("sink group"),
        "expected the message to mention sink group: {body:?}"
    );

    // 実際にまだ存在すること。
    PlcConnectionService::new(env.pool.clone())
        .get(db_connection_id)
        .await
        .expect("connection should survive");
}

/// 対象タグを削除すると、`hub_sink_group_tags`の対応する行も消える
/// （`crate::sink`のモジュール doc comment「タグ削除の経路では能動的に
/// 消す」）。
#[tokio::test]
async fn deleting_a_tag_removes_it_from_sink_group_membership() {
    let env = test_env().await;
    let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
    let (status, created) = admin_post(
        &env.router,
        "/api/sink/groups",
        &env.admin_token,
        sink_group_body(db_connection_id, tag_id),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created:?}");
    let group_id = created["id"].as_i64().unwrap();

    let (status, _) = admin_delete(
        &env.router,
        &format!("/api/tags/{tag_id}"),
        &env.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, fetched) = admin_get(
        &env.router,
        &format!("/api/sink/groups/{group_id}"),
        &env.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{fetched:?}");
    assert_eq!(fetched["tagIds"], json!([]));
}

/// `GET /api/sink/config` は admin 以外 403、admin なら200でパスワード
/// を含み、タグは stable id（connectionId/groupId/tagId）を運ぶ
/// （実装指示）。
#[tokio::test]
async fn sink_config_get_requires_admin_and_carries_password_and_stable_ids() {
    let env = test_env().await;
    let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
    let (status, _) = admin_post(
        &env.router,
        "/api/sink/groups",
        &env.admin_token,
        sink_group_body(db_connection_id, tag_id),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = v1_get(&env.router, &env.viewer_token, "/api/sink/config").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body:?}");

    let (status, body) = v1_get(&env.router, &env.admin_token, "/api/sink/config").await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert!(body["generatedAt"].as_i64().is_some());
    let groups = body["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 1);
    let tags = groups[0]["tags"].as_array().unwrap();
    assert_eq!(tags.len(), 1);
    assert!(tags[0]["connectionId"].as_i64().is_some());
    assert!(tags[0]["groupId"].as_i64().is_some());
    assert_eq!(tags[0]["tagId"], tag_id);
    assert!(tags[0]["externalName"].as_str().unwrap().contains("temp01"));

    let connections = body["connections"].as_array().unwrap();
    assert_eq!(connections.len(), 1);
    assert_eq!(connections[0]["id"], db_connection_id);
    assert_eq!(connections[0]["password"], "s3cret");

    // admin スコープを持つ API キーでも通ること(サイドカーの本来の
    // 経路)。admin スコープ無しのキーは403。
    let (status, issued) =
        issue_api_key(&env.router, &env.admin_token, "sidecar", &["admin"]).await;
    assert_eq!(status, StatusCode::CREATED, "{issued:?}");
    let admin_key = issued["key"].as_str().unwrap();
    let (status, body) = v1_get(&env.router, admin_key, "/api/sink/config").await;
    assert_eq!(status, StatusCode::OK, "{body:?}");

    let (status, issued) =
        issue_api_key(&env.router, &env.admin_token, "reader-only", &["read"]).await;
    assert_eq!(status, StatusCode::CREATED, "{issued:?}");
    let read_key = issued["key"].as_str().unwrap();
    let (status, body) = v1_get(&env.router, read_key, "/api/sink/config").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body:?}");
}

/// 無効な sink group のみの構成では`groups`/`connections`とも空配列。
#[tokio::test]
async fn sink_config_get_excludes_disabled_groups() {
    let env = test_env().await;
    let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
    let mut body = sink_group_body(db_connection_id, tag_id);
    body["enabled"] = json!(false);
    let (status, _) = admin_post(&env.router, "/api/sink/groups", &env.admin_token, body).await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = v1_get(&env.router, &env.admin_token, "/api/sink/config").await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["groups"].as_array().unwrap().len(), 0);
    assert_eq!(body["connections"].as_array().unwrap().len(), 0);
}

fn sink_status_push_body(group_id: i64) -> serde_json::Value {
    json!({
        "groups": [{
            "id": group_id,
            "state": "running",
            "queued": 3,
            "dropped": 0,
            "lastFlushAt": 1_000,
            "lastError": null,
        }],
    })
}

#[tokio::test]
async fn sink_status_put_requires_admin() {
    let env = test_env().await;
    let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
    let (status, created) = admin_post(
        &env.router,
        "/api/sink/groups",
        &env.admin_token,
        sink_group_body(db_connection_id, tag_id),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let group_id = created["id"].as_i64().unwrap();

    let (status, body) = v1_put(
        &env.router,
        &env.viewer_token,
        "/api/sink/status",
        sink_status_push_body(group_id),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body:?}");
}

#[tokio::test]
async fn sink_status_put_rejects_unknown_group_id() {
    let env = test_env().await;
    let (status, body) = v1_put(
        &env.router,
        &env.admin_token,
        "/api/sink/status",
        sink_status_push_body(999_999),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    let message = body["field_errors"][0]["message"].as_str().unwrap_or("");
    assert!(message.contains("999999"), "{body:?}");
}

/// push 後は `GET /api/status` の `sink.sidecar.state == "online"` と
/// 対応するグループ行が見え、15秒以上経過すると`"unknown"`に戻る
/// （実装指示 - `ManualClock`で決定的に検証する）。
#[tokio::test]
async fn sink_status_push_then_status_shows_online_then_unknown_after_stale_window() {
    let now_ms = 1_700_000_000_000i64;
    let clock = Arc::new(ManualClock::new(now_ms, 0));
    let env = test_env_with_clock(clock.clone()).await;
    let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
    let (status, created) = admin_post(
        &env.router,
        "/api/sink/groups",
        &env.admin_token,
        sink_group_body(db_connection_id, tag_id),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let group_id = created["id"].as_i64().unwrap();

    let (status, _) = v1_put(
        &env.router,
        &env.admin_token,
        "/api/sink/status",
        sink_status_push_body(group_id),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, admin_status_body) = admin_get(&env.router, "/api/status", &env.admin_token).await;
    assert_eq!(status, StatusCode::OK, "{admin_status_body:?}");
    assert_eq!(admin_status_body["sink"]["sidecar"]["state"], "online");
    let groups = admin_status_body["sink"]["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0]["id"], group_id);
    assert_eq!(groups[0]["queued"], 3);

    clock.advance_ms(crate::sink::SINK_SIDECAR_STALE_AFTER_MS);
    let (status, admin_status_body) = admin_get(&env.router, "/api/status", &env.admin_token).await;
    assert_eq!(status, StatusCode::OK, "{admin_status_body:?}");
    assert_eq!(admin_status_body["sink"]["sidecar"]["state"], "unknown");
}

// --- banto v1.7.0 #204: session revocation -------------------------------

fn session_request(
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

async fn session_call(
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

async fn session_login(router: &Router, username: &str, remember: bool) -> String {
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

/// One session per surface: the admin UI (`require_session`)
/// and the tag space (`require_tag_space_auth`'s session branch). Separate
/// tokens, because the first surface to notice a revocation drops the
/// token from memory - checking one token on both would let the second
/// surface pass without re-checking anything.
struct SurfaceTokens {
    ui: String,
    v1: String,
}

async fn surface_login(router: &Router, username: &str, remember: bool) -> SurfaceTokens {
    SurfaceTokens {
        ui: session_login(router, username, remember).await,
        v1: session_login(router, username, remember).await,
    }
}

/// `(admin UI, tag space)` statuses - both must refuse a revoked session.
async fn session_statuses(router: &Router, tokens: &SurfaceTokens) -> (StatusCode, StatusCode) {
    let admin_ui = session_call(
        router,
        session_request("GET", "/api/tags", Some(&tokens.ui), None),
    )
    .await
    .0;
    let tag_space = session_call(
        router,
        session_request("GET", "/api/v1/tags", Some(&tokens.v1), None),
    )
    .await
    .0;
    (admin_ui, tag_space)
}

#[derive(Debug, Clone, Copy)]
enum AccountChange {
    Delete,
    Demote,
    PasswordChange,
    PasswordReset,
}

/// banto v1.7.0 #204 completion table (banto-hub has no Tauri command
/// surface for accounts - REST is its only transport): deleting,
/// demoting, resetting or changing the password of an account ends its
/// sessions held elsewhere - normal and "Remember me" - on both the admin
/// UI routes and the tag space, while other accounts' sessions are
/// untouched and the session that changed its OWN password keeps
/// working.
#[tokio::test]
async fn account_changes_end_the_accounts_other_sessions() {
    let ok = (StatusCode::OK, StatusCode::OK);
    let revoked = (StatusCode::UNAUTHORIZED, StatusCode::UNAUTHORIZED);
    for change in [
        AccountChange::Delete,
        AccountChange::Demote,
        AccountChange::PasswordChange,
        AccountChange::PasswordReset,
    ] {
        let env = test_env().await;
        let router = &env.router;
        let admin = &env.admin_token;
        let (status, alice) = session_call(
            router,
            session_request(
                "POST",
                "/api/users",
                Some(admin),
                Some(json!({
                    "username": "alice",
                    "password": "password123",
                    "displayName": "Alice",
                    "role": "editor",
                })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{alice}");
        let alice_id = alice["id"].as_i64().unwrap();
        let alice_normal = surface_login(router, "alice", false).await;
        let alice_remember = surface_login(router, "alice", true).await;
        // The session that makes its own password change (on the admin
        // UI surface), plus one on the tag space.
        let alice_self = surface_login(router, "alice", false).await;
        let viewer = surface_login(router, "viewer1", false).await;
        for tokens in [&alice_normal, &alice_remember, &alice_self, &viewer] {
            assert_eq!(
                session_statuses(router, tokens).await,
                ok,
                "{change:?}: before"
            );
        }

        let (status, body) = match change {
            AccountChange::Delete => {
                session_call(
                    router,
                    session_request(
                        "DELETE",
                        &format!("/api/users/{alice_id}"),
                        Some(admin),
                        None,
                    ),
                )
                .await
            }
            AccountChange::Demote => {
                session_call(
                    router,
                    session_request(
                        "PUT",
                        &format!("/api/users/{alice_id}"),
                        Some(admin),
                        Some(json!({ "displayName": "Alice", "role": "viewer" })),
                    ),
                )
                .await
            }
            AccountChange::PasswordReset => {
                session_call(
                    router,
                    session_request(
                        "POST",
                        &format!("/api/users/{alice_id}/reset-password"),
                        Some(admin),
                        Some(json!({ "newPassword": "reset-password1" })),
                    ),
                )
                .await
            }
            AccountChange::PasswordChange => {
                session_call(
                    router,
                    session_request(
                        "POST",
                        "/api/auth/change-password",
                        Some(&alice_self.ui),
                        Some(json!({
                            "currentPassword": "password123",
                            "newPassword": "changed-password1",
                        })),
                    ),
                )
                .await
            }
        };
        assert!(status.is_success(), "{change:?}: {status} {body}");

        assert_eq!(
            session_statuses(router, &alice_normal).await,
            revoked,
            "{change:?}: normal session"
        );
        assert_eq!(
            session_statuses(router, &alice_remember).await,
            revoked,
            "{change:?}: Remember me session"
        );
        // The changer keeps its session; its tag-space sibling (another
        // session of the same account) ends like the rest.
        let expected_self = match change {
            AccountChange::PasswordChange => (StatusCode::OK, StatusCode::UNAUTHORIZED),
            _ => revoked,
        };
        assert_eq!(
            session_statuses(router, &alice_self).await,
            expected_self,
            "{change:?}: the session that made the change"
        );
        assert_eq!(
            session_statuses(router, &viewer).await,
            ok,
            "{change:?}: bystander"
        );
        assert_eq!(
            session_call(
                router,
                session_request("GET", "/api/users", Some(admin), None)
            )
            .await
            .0,
            StatusCode::OK,
            "{change:?}: acting admin"
        );
    }
}

/// #204: a demoted account is authorized with its CURRENT role once it
/// logs in again - and a stale session cannot keep its old role even for
/// one request (it is refused outright, see the table test above).
#[tokio::test]
async fn a_fresh_session_after_a_demotion_uses_the_new_role() {
    let env = test_env().await;
    let router = &env.router;
    let admin = &env.admin_token;
    let (_, admin2) = session_call(
        router,
        session_request(
            "POST",
            "/api/users",
            Some(admin),
            Some(json!({
                "username": "admin2",
                "password": "password123",
                "displayName": "Admin 2",
                "role": "admin",
            })),
        ),
    )
    .await;
    let admin2_id = admin2["id"].as_i64().unwrap();
    let (status, _) = session_call(
        router,
        session_request(
            "PUT",
            &format!("/api/users/{admin2_id}"),
            Some(admin),
            Some(json!({ "displayName": "Admin 2", "role": "viewer" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let fresh = surface_login(router, "admin2", false).await;
    assert_eq!(
        session_call(
            router,
            session_request("GET", "/api/users", Some(&fresh.ui), None)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        session_statuses(router, &fresh).await,
        (StatusCode::OK, StatusCode::OK)
    );
}

/// #204: when the DB cannot answer the re-check, the request fails (500)
/// but the session is KEPT - it works again as soon as the DB answers.
#[tokio::test]
async fn a_failed_recheck_fails_the_request_but_keeps_the_session() {
    let env = test_env().await;
    let router = &env.router;
    let viewer = surface_login(router, "viewer1", true).await;
    sqlx::query("ALTER TABLE users RENAME TO users_away")
        .execute(&env.pool)
        .await
        .unwrap();
    assert_eq!(
        session_statuses(router, &viewer).await,
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::INTERNAL_SERVER_ERROR
        )
    );
    sqlx::query("ALTER TABLE users_away RENAME TO users")
        .execute(&env.pool)
        .await
        .unwrap();
    assert_eq!(
        session_statuses(router, &viewer).await,
        (StatusCode::OK, StatusCode::OK)
    );
}

/// #204: the first account's session from `POST /api/auth/setup` is
/// bound to the new account (`issue_account_token`), so a lookup-enabled
/// `AuthState` accepts it - an unstamped token would be refused on its
/// first use.
#[tokio::test]
async fn the_setup_session_is_bound_to_the_new_account() {
    let pool = migrate_memory().await.expect("migrate_memory");
    let users = UsersService::new(Db::Sqlite(pool.clone()));
    let audit = AuditLogService::new(Db::Sqlite(pool.clone()));
    let auth = user_auth_state(users.clone(), audit.clone());
    let router = extra_auth_router(
        users,
        auth.clone(),
        audit,
        true,
        Arc::new(GrantRegistry::new()),
        None,
    );
    let (status, body) = session_call(
        &router,
        session_request(
            "POST",
            "/api/auth/setup",
            None,
            Some(json!({
                "username": "owner",
                "password": "password123",
                "displayName": "Owner",
            })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let token = body["token"].as_str().expect("setup token");
    let session = auth
        .authenticate(token)
        .await
        .unwrap()
        .expect("the setup session is accepted");
    assert_eq!(session.identity.id, "owner");
    assert_eq!(session.identity.role, "admin");
}

/// #431: ゲートの判断の総当たり表。例外（照合できないのに通す）は
/// 「止める操作」かつ「それまで有効だったセッション」の 1 行だけ。
#[test]
fn session_gate_decision_table() {
    use GateDecision::*;
    use OperationKind::*;
    use SessionCheck::*;
    let table = [
        (Valid, Normal, false, Allow),
        (Valid, Normal, true, Allow),
        (Valid, StopWrites, false, Allow),
        (Valid, StopWrites, true, Allow),
        // 失効が確認できたら、止める操作でも拒否
        (Revoked, Normal, false, Unauthorized),
        (Revoked, Normal, true, Unauthorized),
        (Revoked, StopWrites, false, Unauthorized),
        (Revoked, StopWrites, true, Unauthorized),
        // 照合できない: 通常は 500、止める操作だけ例外
        (Unverified, Normal, false, Unavailable),
        (Unverified, Normal, true, Unavailable),
        (Unverified, StopWrites, false, Unauthorized),
        (Unverified, StopWrites, true, AllowUnverifiedStop),
    ];
    for (check, operation, previously_valid, expected) in table {
        assert_eq!(
            session_gate_decision(check, operation, previously_valid),
            expected,
            "{check:?} / {operation:?} / previously_valid={previously_valid}"
        );
    }
}

/// #431: 例外の入口は宣言だけ。緊急停止は止める操作、再開は通常の操作。
#[test]
fn only_the_write_stop_route_is_declared_as_a_stop_operation() {
    assert_eq!(WRITE_CONTROL_DISABLE_OPERATION, OperationKind::StopWrites);
    assert_eq!(WRITE_CONTROL_ENABLE_OPERATION, OperationKind::Normal);
}

// --- I3': banto のルーターに替えて入ったもの（banto #277/#278/#248） ---
//
// 本番と同じ合成（`api_router`）・本番と同じ `user_auth_state`（監査
// つきの検証関数とアカウント照合）の上で、banto v2.1.0 の修正が
// banto-hub の REST で効いていることを固定する。以前の写し
// （`crate::users`・`crate::rest` の自前のルーター）ではどれも落ちる。

/// 本番と同じ `user_auth_state` で組んだ router。アカウントは作らない
/// （初回セットアップの前の状態）。試運転のまま（ロックダウンしない）。
async fn banto_wiring_env() -> (Router, sqlx::SqlitePool, UsersService, tempfile::TempDir) {
    let pool = migrate_memory().await.expect("migrate_memory");
    let (tx, _rx) = tokio_broadcast::channel(16);
    let users = UsersService::new(Db::Sqlite(pool.clone()));
    let audit = AuditLogService::new(Db::Sqlite(pool.clone()));
    let api_keys = ApiKeysService::new(pool.clone());
    let (manager, dir) = test_manager_with_clock(pool.clone(), Arc::new(SystemClock));
    let auth = user_auth_state(users.clone(), audit.clone());
    let write_control = Arc::new(crate::write_control::WriteControl::new(false));
    let write_audit = crate::write_audit::WriteAuditService::new(pool.clone());
    let mqtt = Arc::new(crate::mqtt::MqttPublisher::new(manager.clone()));
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
    let commissioning = CommissioningService::load(
        SettingsService::new(Db::Sqlite(pool.clone())),
        users.clone(),
        auth.clone(),
    )
    .await
    .expect("CommissioningService::load");
    let router = api_router(
        users.clone(),
        audit,
        PlcConnectionService::new(pool.clone()),
        CollectionGroupService::new(pool.clone()),
        TagService::new(pool.clone()),
        api_keys,
        manager,
        auth,
        commissioning,
        tx,
        true,
        write_control,
        write_audit,
        mqtt,
        grpc_server,
        rate_limiter,
        crate::profile_paths::DEFAULT_PROFILE_ID.to_string(),
    );
    (router, pool, users, dir)
}

async fn audit_rows(pool: &sqlx::SqlitePool, action: &str) -> Vec<Option<String>> {
    sqlx::query_scalar("SELECT actor_username FROM audit_log WHERE action = ? ORDER BY id")
        .bind(action)
        .fetch_all(pool)
        .await
        .unwrap()
}

/// banto #277: 初回セットアップの「表が空か」と INSERT が 1 文。同時に
/// 来たセットアップは 1 つだけ成功し、アカウントも `setup` の監査も 1 件。
/// 以前の写しは「空か確かめる → ハッシュ → INSERT」の 3 段で、全員が空を
/// 見て admin が複数できた。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_first_setups_create_exactly_one_account() {
    let (router, pool, _users, _dir) = banto_wiring_env().await;
    const ATTEMPTS: usize = 8;
    let barrier = Arc::new(tokio::sync::Barrier::new(ATTEMPTS));
    let mut handles = Vec::new();
    for i in 0..ATTEMPTS {
        let router = router.clone();
        let barrier = barrier.clone();
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            session_call(
                &router,
                session_request(
                    "POST",
                    "/api/auth/setup",
                    None,
                    Some(json!({
                        "username": format!("owner{i}"),
                        "password": "password123",
                        "displayName": format!("Owner {i}"),
                    })),
                ),
            )
            .await
        }));
    }
    let mut succeeded = 0;
    for handle in handles {
        let (status, body) = handle.await.unwrap();
        assert_eq!(status, StatusCode::OK, "{body}");
        if body["success"] == true {
            assert!(body["token"].is_string(), "{body}");
            succeeded += 1;
        } else {
            assert_eq!(body["success"], false, "{body}");
            assert!(body["token"].is_null(), "{body}");
        }
    }
    assert_eq!(succeeded, 1, "exactly one setup wins");
    let accounts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(accounts, 1);
    assert_eq!(audit_rows(&pool, "setup").await.len(), 1);
}

/// banto #278: 失敗ログインの監査に残す名前は 32 文字（末尾 `…`）に
/// 切り詰める。名前は攻撃者が決められ、実在のアカウントである必要も
/// ないので、そのまま残すと監査ログを膨らませる口になる。以前の写しは
/// 入力をそのまま記録していた。
#[tokio::test]
async fn a_failed_login_records_a_bounded_username() {
    let (router, pool, users, _dir) = banto_wiring_env().await;
    users
        .setup_first_user("admin", "password123", "管理者")
        .await
        .unwrap();
    let long_name = "x".repeat(4096);
    let (status, body) = session_call(
        &router,
        session_request(
            "POST",
            "/api/auth/login",
            None,
            Some(json!({ "username": long_name, "password": "wrong-password" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["success"], false, "{body}");

    let recorded = audit_rows(&pool, "login_failed").await;
    assert_eq!(recorded.len(), 1);
    let recorded = recorded[0].clone().expect("an actor username");
    assert_eq!(recorded.chars().count(), 32, "{recorded}");
    assert_eq!(recorded, format!("{}…", "x".repeat(31)));

    // 32 文字以内の名前は（実在しなくても）そのまま残る。
    let (_, body) = session_call(
        &router,
        session_request(
            "POST",
            "/api/auth/login",
            None,
            Some(json!({ "username": "nobody", "password": "wrong-password" })),
        ),
    )
    .await;
    assert_eq!(body["success"], false, "{body}");
    assert_eq!(
        audit_rows(&pool, "login_failed")
            .await
            .last()
            .cloned()
            .flatten(),
        Some("nobody".to_string())
    );
}

/// banto #278: ログアウトの監査は、実際にセッションを終えたときだけ。
/// トークン無し・知らないトークン・終えた後のトークンのログアウトは資格
/// 情報なしで何度でも叩けるので、記録すると監査ログを膨らませる口になる。
/// 以前の写しは毎回（actor なしで）1 行残していた。
#[tokio::test]
async fn only_a_logout_that_ends_a_session_is_audited() {
    let (router, pool, users, _dir) = banto_wiring_env().await;
    users
        .setup_first_user("admin", "password123", "管理者")
        .await
        .unwrap();
    let token = session_login(&router, "admin", false).await;

    let logout = |token: Option<String>| {
        let router = router.clone();
        async move {
            session_call(
                &router,
                session_request("POST", "/api/auth/logout", token.as_deref(), None),
            )
            .await
            .0
        }
    };
    assert!(logout(None).await.is_success());
    assert!(logout(Some("not-a-known-token".to_string()))
        .await
        .is_success());
    assert!(audit_rows(&pool, "logout").await.is_empty());

    assert!(logout(Some(token.clone())).await.is_success());
    assert_eq!(
        audit_rows(&pool, "logout").await,
        vec![Some("admin".to_string())]
    );

    // 終えた後の同じトークンは、もう何も終えない。
    assert!(logout(Some(token)).await.is_success());
    assert_eq!(audit_rows(&pool, "logout").await.len(), 1);
}

/// #248 の受け皿: 監査ログ一覧の応答に `deletionEpoch`（保持の剪定が
/// 行を消すたびに進む数）が乗る。画面（PR2）はこれと件数を世代の最初の
/// 応答と比べて、途中で削除が入ったことを知る。以前の写しの応答には
/// 無かった。
#[tokio::test]
async fn the_audit_log_list_carries_the_deletion_epoch() {
    let env = test_env().await;
    let list = |as_of: Option<i64>| {
        let path = match as_of {
            Some(id) => format!("/api/audit-log/list?asOfId={id}"),
            None => "/api/audit-log/list".to_string(),
        };
        session_request("POST", &path, Some(&env.admin_token), Some(json!({})))
    };
    let (status, body) = session_call(&env.router, list(None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["deletionEpoch"], 0, "{body}");
    let as_of = body["asOfId"].as_i64().expect("asOfId");

    // 監査を 2 行にしてから（viewer の拒否 + 設定の保存）保持件数を 1 に
    // し、境界なしの一覧（剪定する）で古い 1 行を消す。
    let (status, _) = session_call(
        &env.router,
        session_request(
            "GET",
            "/api/audit-log/config",
            Some(&env.viewer_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = session_call(
        &env.router,
        session_request(
            "PUT",
            "/api/audit-log/config",
            Some(&env.admin_token),
            Some(json!({ "retentionDays": null, "retentionRows": 1 })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = session_call(&env.router, list(None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["deletionEpoch"], 1, "{body}");

    // 境界つきの一覧（世代の 2 ブロック目以降）も同じ数を返す。
    let (status, body) = session_call(&env.router, list(Some(as_of))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["deletionEpoch"], 1, "{body}");

    // 読めない `asOfId` は JSON の 400（axum 既定の平文ではない）。
    let (status, body) = session_call(
        &env.router,
        session_request(
            "POST",
            "/api/audit-log/list?asOfId=abc",
            Some(&env.admin_token),
            Some(json!({})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["kind"], "bad_request", "{body}");
}

// --- #383 段階3 P2: banto-hub は `hub` 接続を作らない ------------------

/// `reject_hub_connection_protocol`: REST の作成も、既存の接続を `hub` へ
/// 変える更新も、`protocol` の項目エラーで拒否し、何も書かない。
#[tokio::test]
async fn plc_connections_create_and_update_reject_the_hub_protocol() {
    let env = test_env().await;

    let (status, body) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "name": "hub", "protocol": "hub", "host": "", "port": 0 }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    assert_eq!(body["field_errors"][0]["field"], "protocol", "{body:?}");
    assert!(
        body["field_errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("Hub 経由の接続"),
        "{body:?}"
    );
    let rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM plc_connections WHERE protocol = 'hub'")
            .fetch_one(&env.pool)
            .await
            .unwrap();
    assert_eq!(rows, 0);

    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "name": "line-hub-switch", "host": "127.0.0.1", "port": 15071 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");
    let id = conn["id"].as_i64().unwrap();
    let (status, body) = admin_put(
        &env.router,
        &format!("/api/plc-connections/{id}"),
        &env.admin_token,
        json!({ "name": "line-hub-switch", "protocol": "hub", "host": "", "port": 0 }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    assert_eq!(body["field_errors"][0]["field"], "protocol", "{body:?}");
    let protocol: String = sqlx::query_scalar("SELECT protocol FROM plc_connections WHERE id = ?")
        .bind(id)
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(protocol, "modbus-tcp");
}

/// 未適用キューに `hub` の作成が残っていても（REST/MCP はキューへ積む前に
/// 拒否するので、今の版では積まれない）、適用時にもう一度拒否して
/// `failed` にし、理由に `protocol` を残す。
#[tokio::test]
async fn pending_apply_rejects_a_queued_hub_connection() {
    let env = test_env().await;
    let payload = json!({
        "input": { "name": "hub", "protocol": "hub", "host": "", "port": 0 }
    });
    let pending = PendingChangesService::new(env.pool.clone())
        .create_pending(
            "plc_connections.create",
            &payload,
            env.manager.configured_revision() as i64,
            None,
            Some("admin"),
            Some("ui"),
        )
        .await
        .unwrap();

    let response = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post(format!("/api/pending-changes/{}/apply", pending.id))
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let pending = PendingChangesService::new(env.pool.clone())
        .get(pending.id)
        .await
        .unwrap();
    assert_eq!(pending.state, PendingChangeState::Failed);
    let reason = pending.failure_reason.expect("failure_reason");
    assert!(reason.contains("protocol"), "{reason}");
    let rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM plc_connections WHERE protocol = 'hub'")
            .fetch_one(&env.pool)
            .await
            .unwrap();
    assert_eq!(rows, 0);
}
