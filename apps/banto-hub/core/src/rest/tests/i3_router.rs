use super::super::*;
use super::common::{
    session_call, session_login, session_request, test_env, test_manager_with_clock,
};
use crate::api_keys::ApiKeysService;
use crate::db::migrate_memory;
use banto_tstore::SystemClock;
use tokio::sync::broadcast as tokio_broadcast;

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
