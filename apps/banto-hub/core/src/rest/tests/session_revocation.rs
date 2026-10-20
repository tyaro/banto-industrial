use super::super::*;
use super::common::{session_call, session_login, session_request, test_env};
use crate::db::migrate_memory;

// --- banto v1.7.0 #204: session revocation -------------------------------

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
