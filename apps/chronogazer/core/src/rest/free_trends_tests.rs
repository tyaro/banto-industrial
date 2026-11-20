//! `/api/free-trends/*` の権限の表（#1207・#1231 PR 8a、2026-11-20 オーナー決定。
//! `crate::free_trends` のモジュール doc「権限」）。
//!
//! どれも**本物の `api_router`**（CSRF の層・`api_key_gate`・子ルーターの認証を含む）に要求を
//! 通す。grant のセッション（この PC からの接続 `localDesktop`・閲覧公開 `publicViewer`）の
//! トークンは、ルーターと同じ `AuthState` に grant として発行する（`require_auth` が grant の
//! kind を `AuthenticatedSession::grant` に載せる - 本番の発行の口と同じ形のセッション）。
//! ログイン不要モードは REST には無い（デスクトップの合成のセッションだけ）ので、その拒否は
//! `src-tauri` のテストで確かめる。
//!
//! | 主体 | 開く | 作成 | 他人の物の変更・削除 | 自分の物の変更・削除 |
//! |---|---|---|---|---|
//! | viewer のアカウント | 可 | 可 | 403（`denied`） | 可 |
//! | editor のアカウント | 可 | 可 | 可 | 可 |
//! | `localDesktop`（viewer） | 可 | 可（作った人 `localDesktop`） | 403 | 可 |
//! | 閲覧公開 | 可 | 403 | 403 | - |
//! | API キー（`read`・`config`） | 可 | 403 | 403 | - |

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use banto_server::{AuthState, GrantKind, GrantSpec, Identity};
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::api_keys::ApiKeysService;
use crate::db::Db;
use crate::display_groups::tests::seed_tags;
use crate::local_grant::local_desktop_grant_kind;
use crate::rest::tests::router_with_role_tokens_pool_hub_and_auth;
use crate::users::{Role, UsersService};

/// 要求の主体。セッションは `X-Banto-Client` を付け、キーは付けない（キーは CSRF の確認を
/// 求めない、`rest/actor.rs`）。
enum Who<'a> {
    Session(&'a str),
    Key(&'a str),
}

async fn send(
    router: &Router,
    who: &Who<'_>,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(path);
    builder = match who {
        Who::Session(token) => builder
            .header("X-Banto-Client", "banto")
            .header("Authorization", format!("Bearer {token}")),
        Who::Key(key) => builder.header("Authorization", format!("Bearer {key}")),
    };
    let body = match body {
        Some(value) => {
            builder = builder.header("Content-Type", "application/json");
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };
    let response = router
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

/// grant のセッションを 1 つ発行する（本番の発行の口と同じく、identity は spec の物）。
fn grant_token(auth: &AuthState, kind: GrantKind, id: &str, role: &str) -> String {
    let spec = GrantSpec::new(
        kind.clone(),
        Identity {
            id: id.to_string(),
            name: id.to_string(),
            role: role.to_string(),
        },
        Arc::new(|| Box::pin(async { Ok(true) })),
    );
    auth.issue_grant_token(&spec, auth.grant_generation(&kind))
        .expect("grant token")
}

/// `audit_log` の `resource = "free_trends"` の (action, result, origin, actor_username, actor_role)。
async fn audit_rows(pool: &sqlx::SqlitePool) -> Vec<(String, String, String, String, String)> {
    sqlx::query_as(
        "SELECT action, result, origin, COALESCE(actor_username, ''), COALESCE(actor_role, '') \
         FROM audit_log WHERE resource = 'free_trends' ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

fn body(name: &str, tag_ids: &[i64], revision: Option<i64>) -> Value {
    let pens: Vec<Value> = tag_ids.iter().map(|id| json!({ "tagId": id })).collect();
    let mut value = json!({ "name": name, "timeWindowSec": 300, "pens": pens });
    if let Some(revision) = revision {
        value["expectedRevision"] = json!(revision);
    }
    value
}

fn can_edit_by_name(list: &Value) -> Vec<(String, bool)> {
    list.as_array()
        .expect("array")
        .iter()
        .map(|t| {
            (
                t["name"].as_str().unwrap().to_string(),
                t["canEdit"].as_bool().unwrap(),
            )
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn free_trend_permissions_follow_the_owner_decision() {
    let (router, _admin, editor, viewer, pool, _hub, auth) =
        router_with_role_tokens_pool_hub_and_auth().await;
    let (_tags, ids) = seed_tags(&pool, 2).await;
    UsersService::new(Db::Sqlite(pool.clone()))
        .create_user("viewer2", "password123", "閲覧者 2", Role::Viewer)
        .await
        .expect("create viewer2");
    let viewer2 = auth
        .login("viewer2", "password123")
        .await
        .expect("viewer2 login");
    let local = grant_token(&auth, local_desktop_grant_kind(), "localDesktop", "viewer");
    let public = grant_token(
        &auth,
        GrantKind::new(GrantKind::PUBLIC_VIEWER).unwrap(),
        banto_server::PUBLIC_VIEWER_ID,
        "viewer",
    );
    let keys = ApiKeysService::new(pool.clone());
    let read_key = keys
        .issue("reader", vec!["read".to_string()], None)
        .await
        .expect("issue read")
        .key;
    let config_key = keys
        .issue("writer", vec!["config".to_string()], None)
        .await
        .expect("issue config")
        .key;

    let viewer = Who::Session(&viewer);
    let viewer2 = Who::Session(&viewer2);
    let editor = Who::Session(&editor);
    let local = Who::Session(&local);
    let public = Who::Session(&public);
    let read_key = Who::Key(&read_key);
    let config_key = Who::Key(&config_key);

    // viewer のアカウントは作れる。作った人が残り、自分には canEdit。
    let (status, created) = send(
        &router,
        &viewer,
        "POST",
        "/api/free-trends",
        Some(body("V", &ids, None)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["createdBy"], "viewer");
    assert_eq!(created["timeWindowSec"], 300);
    assert_eq!(created["canEdit"], true);
    let v_id = created["id"].as_i64().unwrap();
    let v_path = format!("/api/free-trends/{v_id}");

    // この PC からの接続（localDesktop）も作れる。作った人は localDesktop。
    let (status, by_local) = send(
        &router,
        &local,
        "POST",
        "/api/free-trends",
        Some(body("L", &ids[..1], None)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{by_local}");
    assert_eq!(by_local["createdBy"], "localDesktop");
    assert_eq!(by_local["canEdit"], true);
    let l_path = format!("/api/free-trends/{}", by_local["id"]);

    // 開く: 全員が読めて、canEdit は主体ごと。
    for (who, want) in [
        (&viewer, vec![("L", false), ("V", true)]),
        (&viewer2, vec![("L", false), ("V", false)]),
        (&editor, vec![("L", true), ("V", true)]),
        (&local, vec![("L", true), ("V", false)]),
        (&public, vec![("L", false), ("V", false)]),
        (&read_key, vec![("L", false), ("V", false)]),
        (&config_key, vec![("L", false), ("V", false)]),
    ] {
        let (status, list) = send(&router, who, "GET", "/api/free-trends", None).await;
        assert_eq!(status, StatusCode::OK, "{list}");
        let want: Vec<(String, bool)> = want
            .into_iter()
            .map(|(name, edit)| (name.to_string(), edit))
            .collect();
        assert_eq!(can_edit_by_name(&list), want);
    }
    let (status, one) = send(&router, &read_key, "GET", &v_path, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(one["canEdit"], false);

    assert!(audit_rows(&pool).await.iter().all(|row| row.1 == "ok"));
    let denied_before = 0usize;

    // 作成を断る主体: 閲覧公開・read キー・config キー。
    for who in [&public, &read_key, &config_key] {
        let (status, _) = send(
            &router,
            who,
            "POST",
            "/api/free-trends",
            Some(body("X", &ids, None)),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    // 他人の物の変更・削除を断る主体: viewer2・localDesktop（viewer）・閲覧公開・キー。
    for who in [&viewer2, &local, &public, &read_key, &config_key] {
        let (status, _) = send(
            &router,
            who,
            "PUT",
            &v_path,
            Some(body("V改", &ids, Some(1))),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = send(&router, who, "DELETE", &v_path, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    // viewer は localDesktop の物を変えられない。
    let (status, _) = send(&router, &viewer, "DELETE", &l_path, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // 断った書き込みは全部 `denied` で監査され、主体が分かる（3 + 5×2 + 1 = 14 件）。
    let rows = audit_rows(&pool).await;
    let denied: Vec<_> = rows.iter().filter(|row| row.1 == "denied").collect();
    assert_eq!(denied.len(), denied_before + 14, "{rows:?}");
    assert!(denied
        .iter()
        .all(|row| row.0 == "denied" && row.2 == "rest"));
    let actors: std::collections::BTreeSet<(String, String)> = denied
        .iter()
        .map(|row| (row.3.clone(), row.4.clone()))
        .collect();
    assert!(actors.contains(&("public".to_string(), "viewer".to_string())));
    assert!(actors.contains(&("viewer2".to_string(), "viewer".to_string())));
    assert!(actors.contains(&("localDesktop".to_string(), "viewer".to_string())));
    assert!(actors.contains(&("viewer".to_string(), "viewer".to_string())));
    assert_eq!(
        actors.iter().filter(|a| a.1 == "api_key").count(),
        2,
        "{actors:?}"
    );
    // 何も変わっていない。
    let (_, one) = send(&router, &viewer, "GET", &v_path, None).await;
    assert_eq!(one["revision"], 1);
    assert_eq!(one["name"], "V");

    // editor は他人の物を変えられる。作った人は残り、最後に変えた人が editor。
    let (status, by_editor) = send(
        &router,
        &editor,
        "PUT",
        &v_path,
        Some(body("V改", &ids[..1], Some(1))),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{by_editor}");
    assert_eq!(by_editor["createdBy"], "viewer");
    assert_eq!(by_editor["updatedBy"], "editor");
    assert_eq!(by_editor["revision"], 2);

    // 古い版は 409（表示グループと同じ形）。
    let (status, conflict) = send(
        &router,
        &viewer,
        "PUT",
        &v_path,
        Some(body("V改2", &ids, Some(1))),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
    assert_eq!(conflict["kind"], "validation");
    assert_eq!(conflict["field_errors"][0]["field"], "expectedRevision");

    // 作った人（viewer）は自分の物を変え・消せる。localDesktop も自分の物を消せる。
    let (status, _) = send(
        &router,
        &viewer,
        "PUT",
        &v_path,
        Some(body("V改2", &ids, Some(2))),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(&router, &viewer, "DELETE", &v_path, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(&router, &local, "DELETE", &l_path, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(&router, &viewer, "GET", &v_path, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 成功した書き込みの監査（action・origin・主体）。
    let ok: Vec<(String, String, String)> = audit_rows(&pool)
        .await
        .into_iter()
        .filter(|row| row.1 == "ok")
        .map(|row| (row.0, row.2, row.3))
        .collect();
    let s = |a: &str, b: &str, c: &str| (a.to_string(), b.to_string(), c.to_string());
    assert_eq!(
        ok,
        vec![
            s("create", "rest", "viewer"),
            s("create", "rest", "localDesktop"),
            s("update", "rest", "editor"),
            s("update", "rest", "viewer"),
            s("delete", "rest", "viewer"),
            s("delete", "rest", "localDesktop"),
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validation_errors_are_422_and_a_session_still_needs_the_csrf_header() {
    let (router, _admin, _editor, viewer, pool, _hub, _auth) =
        router_with_role_tokens_pool_hub_and_auth().await;
    let (_tags, ids) = seed_tags(&pool, 1).await;
    let who = Who::Session(&viewer);
    let (status, err) = send(
        &router,
        &who,
        "POST",
        "/api/free-trends",
        Some(json!({
            "name": " ",
            "timeWindowSec": 7,
            "pens": [{ "tagId": ids[0], "lineWidth": 9 }, { "tagId": ids[0] }],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{err}");
    let fields: Vec<&str> = err["field_errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["field"].as_str().unwrap())
        .collect();
    for want in [
        "name",
        "timeWindowSec",
        "pens[0].lineWidth",
        "pens[1].tagId",
    ] {
        assert!(fields.contains(&want), "{want}: {fields:?}");
    }

    // セッションは CSRF のヘッダーが無ければ断られる（既存の作法のまま）。
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/free-trends")
                .header("Authorization", format!("Bearer {viewer}"))
                .header("Content-Type", "application/json")
                .body(Body::from(json!({ "name": "x" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(response.status(), StatusCode::OK);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM free_trends")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}
