use super::super::*;
use super::common::{test_env, CLIENT_HEADER};
use axum::body::Body;
use axum::http::Request as HttpRequest;
use tower::ServiceExt;

// --- 監査ログ retention 設定 (docs/banto-hub-remaining-plan.md P3-a) ----

/// `GET/PUT /api/audit-log/config`: admin 限定（viewer は403）、既定値
/// （90日/100,000件）、保存した値の round-trip、PUT が
/// `settings`リソースへの`settings_change`監査エントリを1件だけ記録する
/// （I3' で banto の `audit_log_router` に替えてからの語彙）ことを1本の
/// テストで固定する（`collection_control_requires_admin_and_csrf_and_is_idempotent`
/// と同じ「1テストで RBAC + 挙動 + 監査を通して確認する」形）。
#[tokio::test]
async fn audit_log_config_round_trips_and_requires_admin() {
    let env = test_env().await;

    let viewer_get = HttpRequest::builder()
        .method("GET")
        .uri("/api/audit-log/config")
        .header("Authorization", format!("Bearer {}", env.viewer_token))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .body(Body::empty())
        .unwrap();
    let response = env.router.clone().oneshot(viewer_get).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let admin_get = HttpRequest::builder()
        .method("GET")
        .uri("/api/audit-log/config")
        .header("Authorization", format!("Bearer {}", env.admin_token))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .body(Body::empty())
        .unwrap();
    let response = env.router.clone().oneshot(admin_get).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["retentionDays"], 90);
    assert_eq!(body["retentionRows"], 100_000);

    let put_body = || {
        Body::from(
            serde_json::to_vec(&serde_json::json!({
                "retentionDays": 30,
                "retentionRows": 5000
            }))
            .unwrap(),
        )
    };

    let viewer_put = HttpRequest::builder()
        .method("PUT")
        .uri("/api/audit-log/config")
        .header("Authorization", format!("Bearer {}", env.viewer_token))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .header("content-type", "application/json")
        .body(put_body())
        .unwrap();
    let response = env.router.clone().oneshot(viewer_put).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let admin_put = HttpRequest::builder()
        .method("PUT")
        .uri("/api/audit-log/config")
        .header("Authorization", format!("Bearer {}", env.admin_token))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .header("content-type", "application/json")
        .body(put_body())
        .unwrap();
    let response = env.router.clone().oneshot(admin_put).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let applied: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(applied["retentionDays"], 30);
    assert_eq!(applied["retentionRows"], 5000);

    let refetch = HttpRequest::builder()
        .method("GET")
        .uri("/api/audit-log/config")
        .header("Authorization", format!("Bearer {}", env.admin_token))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .body(Body::empty())
        .unwrap();
    let response = env.router.clone().oneshot(refetch).await.unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let refetched: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(refetched["retentionDays"], 30);
    assert_eq!(refetched["retentionRows"], 5000);

    // I3'（banto の `audit_log_router`）: 設定の読み書きは `settings`
    // リソース。viewer の拒否 2 件（GET・PUT）と、保存の
    // `settings_change` 1 件（`entityId` なし）。以前の写しは
    // `update`/`audit_log_config`/`"1"` で、拒否は `audit_log` だった。
    let rows: Vec<(String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT action, resource, result, entity_id FROM audit_log              WHERE resource IN ('settings', 'audit_log', 'audit_log_config') ORDER BY id",
    )
    .fetch_all(&env.pool)
    .await
    .unwrap();
    let denied = (
        "denied".to_string(),
        "settings".to_string(),
        "denied".to_string(),
        None,
    );
    assert_eq!(
        rows,
        vec![
            denied.clone(),
            denied,
            (
                "settings_change".to_string(),
                "settings".to_string(),
                "ok".to_string(),
                None
            ),
        ]
    );
}

/// `POST /api/audit-log/list`が読む前に retention 設定に従って
/// opportunistic に剪定すること（`crate::audit::AuditLogService::prune`
/// の配線そのもの、docs/banto-hub-remaining-plan.md P3-a）を、REST
/// エンドポイント越しに確認する - `crate::audit`の単体テストは
/// `prune`自体の正しさを担保済みなので、ここでは「呼ばれること」だけを
/// 見る。
#[tokio::test]
async fn audit_log_list_opportunistically_prunes_by_configured_retention() {
    let env = test_env().await;

    for i in 0..5 {
        sqlx::query(
            "INSERT INTO audit_log (actor_username, actor_role, action, resource, entity_id, detail, origin, result) \
             VALUES ('admin', 'admin', 'create', 'items', ?, NULL, 'rest', 'ok')",
        )
        .bind(i.to_string())
        .execute(&env.pool)
        .await
        .unwrap();
    }

    let put = HttpRequest::builder()
        .method("PUT")
        .uri("/api/audit-log/config")
        .header("Authorization", format!("Bearer {}", env.admin_token))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "retentionDays": null,
                "retentionRows": 2
            }))
            .unwrap(),
        ))
        .unwrap();
    let response = env.router.clone().oneshot(put).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // PUT 自体も1件 audit_log 行を作るため、この時点で 5(シード) + 1(PUT)
    // = 6件。retentionRows=2 で list を叩くと、剪定後は最新2件のみ残る。
    let list = HttpRequest::builder()
        .method("POST")
        .uri("/api/audit-log/list")
        .header("Authorization", format!("Bearer {}", env.admin_token))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({})).unwrap(),
        ))
        .unwrap();
    let response = env.router.clone().oneshot(list).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["totalCount"], 2);

    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(remaining, 2);
}

/// #428: `asOfId` 付きの取得（2 ブロック目以降）では剪定しない - 上限に
/// 張り付いた状態でも、読み込みの途中で行が消えて失効しない。`asOfId`
/// なし（世代の最初・再読み込み）では従来どおり剪定する。
#[tokio::test]
async fn audit_log_list_with_as_of_id_does_not_prune() {
    let env = test_env().await;
    for i in 0..5 {
        sqlx::query(
            "INSERT INTO audit_log (actor_username, actor_role, action, resource, entity_id, detail, origin, result) \
             VALUES ('admin', 'admin', 'create', 'items', ?, NULL, 'rest', 'ok')",
        )
        .bind(i.to_string())
        .execute(&env.pool)
        .await
        .unwrap();
    }
    // 保持件数の上限を 2 にする（`PUT` も 1 行記録する = 6 行、上限超過）。
    let put = HttpRequest::builder()
        .method("PUT")
        .uri("/api/audit-log/config")
        .header("Authorization", format!("Bearer {}", env.admin_token))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "retentionDays": null,
                "retentionRows": 2
            }))
            .unwrap(),
        ))
        .unwrap();
    let response = env.router.clone().oneshot(put).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let count = || async {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM audit_log")
            .fetch_one(&env.pool)
            .await
            .unwrap()
    };
    let before = count().await;
    assert_eq!(before, 6);
    let max_id: i64 = sqlx::query_scalar("SELECT MAX(id) FROM audit_log")
        .fetch_one(&env.pool)
        .await
        .unwrap();

    let list = |uri: String| {
        HttpRequest::builder()
            .method("POST")
            .uri(uri)
            .header("Authorization", format!("Bearer {}", env.admin_token))
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({})).unwrap(),
            ))
            .unwrap()
    };

    // `asOfId` 付き: 上限を超えていても消さない（件数も境界の集合のまま）。
    let response = env
        .router
        .clone()
        .oneshot(list(format!("/api/audit-log/list?asOfId={max_id}")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["totalCount"], 6, "{body}");
    assert_eq!(count().await, before, "asOfId 付きの取得で剪定された");

    // `asOfId` なし: 従来どおり剪定する。
    let response = env
        .router
        .clone()
        .oneshot(list("/api/audit-log/list".to_string()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(count().await, 2);
}

/// `?asOfId=`（#428）: 省略すると従来どおり全行 + 応答の `asOfId` に最大
/// `id`、指定するとその境界より後の行は件数にも行にも入らない。床は
/// `admin` のまま（クエリを付けても editor/viewer は 403）。
#[tokio::test]
async fn audit_log_list_as_of_id_pins_the_snapshot() {
    let env = test_env().await;
    let audit = AuditLogService::new(Db::Sqlite(env.pool.clone()));
    let entry = |action: &'static str| AuditEntry {
        actor_username: Some("admin"),
        actor_role: Some("admin"),
        action,
        resource: "items",
        entity_id: None,
        detail: None,
        origin: "rest",
        result: "ok",
    };
    for action in ["a", "b", "c"] {
        audit.try_record(entry(action)).await.unwrap();
    }

    let list = |uri: String, token: String| {
        HttpRequest::builder()
            .method("POST")
            .uri(uri)
            .header("Authorization", format!("Bearer {token}"))
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({})).unwrap(),
            ))
            .unwrap()
    };
    let call = |request: HttpRequest<Body>| {
        let router = env.router.clone();
        async move {
            let response = router.oneshot(request).await.unwrap();
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            (
                status,
                serde_json::from_slice::<serde_json::Value>(&bytes)
                    .unwrap_or(serde_json::Value::Null),
            )
        }
    };

    let (status, first) = call(list(
        "/api/audit-log/list".to_string(),
        env.admin_token.clone(),
    ))
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let as_of_id = first["asOfId"].as_i64().expect("asOfId が返ること");
    let first_total = first["totalCount"].as_u64().unwrap();
    let max_id = first["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_i64().unwrap())
        .max()
        .unwrap();
    assert_eq!(as_of_id, max_id, "省略時の境界は最大 id: {first}");
    assert_eq!(first["rows"].as_array().unwrap().len() as u64, first_total);

    audit.try_record(entry("after")).await.unwrap();

    let (status, pinned) = call(list(
        format!("/api/audit-log/list?asOfId={as_of_id}"),
        env.admin_token.clone(),
    ))
    .await;
    assert_eq!(status, StatusCode::OK, "{pinned}");
    assert_eq!(pinned["asOfId"], as_of_id);
    assert_eq!(pinned["totalCount"].as_u64().unwrap(), first_total);
    assert!(
        pinned["rows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["action"] != "after"),
        "境界より後の行が混ざった: {pinned}"
    );

    let (_, fresh) = call(list(
        "/api/audit-log/list".to_string(),
        env.admin_token.clone(),
    ))
    .await;
    assert_eq!(fresh["totalCount"].as_u64().unwrap(), first_total + 1);
    assert!(fresh["asOfId"].as_i64().unwrap() > as_of_id);

    // 床は admin のまま: editor（ここで作ってログイン）と viewer は 403。
    UsersService::new(Db::Sqlite(env.pool.clone()))
        .create_user("editor1", "password123", "編集者", Role::Editor)
        .await
        .expect("create_user editor");
    let (status, login) = call(
        HttpRequest::builder()
            .method("POST")
            .uri("/api/auth/login")
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({ "username": "editor1", "password": "password123" }))
                    .unwrap(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{login}");
    let editor_token = login["token"].as_str().unwrap().to_string();
    for token in [editor_token, env.viewer_token.clone()] {
        let (status, body) = call(list(
            format!("/api/audit-log/list?asOfId={as_of_id}"),
            token,
        ))
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    }
}
