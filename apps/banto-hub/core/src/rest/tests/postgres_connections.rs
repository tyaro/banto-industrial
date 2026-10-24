use super::super::*;
use super::common::{admin_post, admin_put, stored_password, test_env, CLIENT_HEADER};
use crate::pending_changes::PendingChangeState;
use axum::body::Body;
use axum::http::Request as HttpRequest;
use tower::ServiceExt;

// --- S1 (docs/banto-hub-external-db-design.md §4.1・§4.6・§6・§7):
// PostgreSQL 接続エンティティ・接続テスト API ------------------------

/// `POST /api/plc-connections` で `protocol: "postgres"` を作成すると、
/// 応答の `settings.password` は `{"$secret": "set"}`（R2b-6b）で、値そのものは
/// 出ない（`PlcConnectionResponse`が生の`password`を決して外部へ
/// 出さないことの回帰テスト）。
#[tokio::test]
async fn create_postgres_connection_returns_password_set_without_password_field() {
    let env = test_env().await;
    let (status, body) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "name": "erp-db", "protocol": "postgres", "settings": { "host": "10.0.0.50", "port": 5432, "database": "erp", "username": "reader", "password": "s3cret" } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["settings"]["password"], json!({ "$secret": "set" }));
    assert!(
        body.get("password").is_none(),
        "the response must never contain the raw password: {body:?}"
    );
    assert!(
        !body.to_string().contains("s3cret"),
        "the response must never contain the raw password: {body:?}"
    );
    assert_eq!(body["settings"]["database"], "erp");
    assert_eq!(body["settings"]["username"], "reader");
}

/// `PUT`で`password`を省略（`null`/未指定）すると、既存のパスワードは
/// 変更されない（`banto_tags::PlcConnectionInput::password`の doc
/// comment、update の tri-state PATCH 意味論の REST 経由での確認）。
#[tokio::test]
async fn update_omitting_password_keeps_the_existing_password() {
    let env = test_env().await;
    let (status, created) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "name": "erp-db-keep", "protocol": "postgres", "settings": { "host": "10.0.0.50", "port": 5432, "database": "erp", "username": "reader", "password": "original-secret" } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created:?}");
    let conn_id = created["id"].as_i64().unwrap();
    assert_eq!(created["settings"]["password"], json!({ "$secret": "set" }));

    // password フィールドを省略した PUT（他は全項目必須）。
    let (status, updated) = admin_put(
        &env.router,
        &format!("/api/plc-connections/{conn_id}"),
        &env.admin_token,
        json!({ "name": "erp-db-keep-renamed", "protocol": "postgres", "enabled": true, "simulation": false, "settings": { "host": "10.0.0.50", "port": 5432, "database": "erp", "username": "reader" } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated:?}");
    assert_eq!(
        updated["settings"]["password"],
        json!({ "$secret": "set" }),
        "omitting password must keep it set"
    );

    assert_eq!(
        stored_password(&env.pool, conn_id).await.as_deref(),
        Some("original-secret")
    );
}

/// `PUT`で`password: ""`を送ると、パスワードが消去される。
#[tokio::test]
async fn update_with_empty_password_clears_it() {
    let env = test_env().await;
    let (status, created) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "name": "erp-db-clear", "protocol": "postgres", "settings": { "host": "10.0.0.50", "port": 5432, "database": "erp", "username": "reader", "password": "original-secret" } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created:?}");
    let conn_id = created["id"].as_i64().unwrap();

    let (status, updated) = admin_put(
        &env.router,
        &format!("/api/plc-connections/{conn_id}"),
        &env.admin_token,
        json!({ "name": "erp-db-clear", "protocol": "postgres", "enabled": true, "simulation": false, "settings": { "host": "10.0.0.50", "port": 5432, "database": "erp", "username": "reader", "password": "" } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated:?}");
    assert!(updated["settings"].get("password").is_none(), "{updated:?}");

    assert_eq!(stored_password(&env.pool, conn_id).await, None);
}

/// R2b-6b（オーナー決定 2026-10-24）: 秘密のキーの tri-state は、省略 = 保つ、`null` = 消す、
/// 空文字 = 消す（上のテスト）、文字列 = 置き換え。`null` で消えること、置き換えで新しい値に
/// なることを REST で確かめる。型違い（文字列のポート）は 422 で、項目は JSON のキー。
#[tokio::test]
async fn update_with_null_password_clears_it_and_a_string_replaces_it() {
    let env = test_env().await;
    let body = |password: serde_json::Value| {
        json!({ "name": "erp-db-null", "protocol": "postgres", "settings": {
            "host": "10.0.0.50", "port": 5432, "database": "erp", "username": "reader",
            "password": password } })
    };
    let (status, created) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        body(json!("original-secret")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created:?}");
    let conn_id = created["id"].as_i64().unwrap();
    let path = format!("/api/plc-connections/{conn_id}");

    let (status, replaced) = admin_put(
        &env.router,
        &path,
        &env.admin_token,
        body(json!("next-secret")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{replaced:?}");
    assert_eq!(
        stored_password(&env.pool, conn_id).await.as_deref(),
        Some("next-secret")
    );
    assert!(
        !replaced.to_string().contains("next-secret"),
        "{replaced:?}"
    );

    let (status, cleared) =
        admin_put(&env.router, &path, &env.admin_token, body(json!(null))).await;
    assert_eq!(status, StatusCode::OK, "{cleared:?}");
    assert!(cleared["settings"].get("password").is_none(), "{cleared:?}");
    assert_eq!(stored_password(&env.pool, conn_id).await, None);

    let mut wrong = body(json!(null));
    wrong["settings"]["port"] = json!("5432");
    let (status, refused) = admin_put(&env.router, &path, &env.admin_token, wrong).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused:?}");
    assert_eq!(refused["field_errors"][0]["field"], "port", "{refused:?}");
}

/// 非 postgres 接続の `settings` に `database` を付けると `422 Validation` で拒否される
/// （R2b-6b: ドライバのスキーマに無い欄、Q9。REST 経由の確認）。
#[tokio::test]
async fn create_non_postgres_with_database_is_rejected() {
    let env = test_env().await;
    let (status, body) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "name": "not-pg", "protocol": "modbus-tcp", "settings": { "host": "127.0.0.1", "port": 15070, "database": "somedb" } }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    assert_eq!(body["kind"], "validation");
    assert_eq!(body["field_errors"][0]["field"], "database", "{body:?}");
}

/// S1（この段階では postgres 接続配下にグループを作れない）: REST 経由の
/// 確認 - `banto_tags::collection_group`側の単体テストの REST 版。
#[tokio::test]
async fn create_group_under_a_postgres_connection_is_rejected() {
    let env = test_env().await;
    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "name": "erp-db-group-guard", "protocol": "postgres", "settings": { "host": "10.0.0.50", "port": 5432, "database": "erp", "username": "reader" } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");
    let conn_id = conn["id"].as_i64().unwrap();

    let (status, body) = admin_post(
        &env.router,
        "/api/collection-groups",
        &env.admin_token,
        json!({ "name": "g-on-pg", "plcConnectionId": conn_id, "periodMs": 1000 }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    assert_eq!(body["kind"], "validation");
}

/// `POST /api/plc-connections/{id}/test` - S1a の接続テスト API。
/// 非 postgres 接続に対しては 400 で拒否する（design §4.6・実装指示
/// 「for non-postgres protocols respond 400」）。
#[tokio::test]
async fn test_saved_connection_rejects_non_postgres_with_400() {
    let env = test_env().await;
    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "protocol": "modbus-tcp", "name": "not-pg-test", "settings": { "host": "127.0.0.1", "port": 15071 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");
    let conn_id = conn["id"].as_i64().unwrap();

    let (status, body) = admin_post(
        &env.router,
        &format!("/api/plc-connections/{conn_id}/test"),
        &env.admin_token,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body:?}");
}

/// `POST /api/plc-connections/{id}/test` の実 PostgreSQL 経路。
/// `BANTO_TEST_PG_URL`が未設定なら`eprintln!`してスキップする
/// （`crate::db_source`の同種テストと同じ慣習）。
#[tokio::test]
async fn test_saved_connection_against_a_real_postgresql_returns_the_server_version() {
    let Ok(url) = std::env::var("BANTO_TEST_PG_URL") else {
        eprintln!("skipped: BANTO_TEST_PG_URL unset");
        return;
    };
    let options: sqlx::postgres::PgConnectOptions = url
        .parse()
        .expect("BANTO_TEST_PG_URL should be a valid postgres:// URL");
    let password = url
        .split_once("://")
        .and_then(|(_, rest)| rest.split_once('@'))
        .and_then(|(userinfo, _)| userinfo.split_once(':'))
        .map(|(_, password)| password.to_string());

    let env = test_env().await;
    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "name": "real-pg-test", "protocol": "postgres", "settings": { "host": options.get_host(), "port": options.get_port(), "database": options.get_database(), "username": options.get_username(), "password": password } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");
    let conn_id = conn["id"].as_i64().unwrap();

    let (status, body) = admin_post(
        &env.router,
        &format!("/api/plc-connections/{conn_id}/test"),
        &env.admin_token,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], true, "{body:?}");
    let server_version = body["serverVersion"].as_str().expect("serverVersion");
    eprintln!("POST /api/plc-connections/{{id}}/test -> {server_version}");
    assert!(body.get("error").is_none());
}

/// `POST /api/plc-connections/{id}/test`の失敗経路: 閉じたポートに
/// 対して`ok:false`かつパスワードを含まないエラー文言を返す。
#[tokio::test]
async fn test_saved_connection_against_a_closed_port_reports_failure_without_the_password() {
    let env = test_env().await;
    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "name": "closed-port-pg", "protocol": "postgres", "settings": { "host": "127.0.0.1", "port": 1, "database": "erp", "username": "reader", "password": "s3cret-password" } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");
    let conn_id = conn["id"].as_i64().unwrap();

    let (status, body) = admin_post(
        &env.router,
        &format!("/api/plc-connections/{conn_id}/test"),
        &env.admin_token,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], false, "{body:?}");
    let error = body["error"].as_str().expect("error message");
    assert!(
        !error.contains("s3cret-password"),
        "error text must never contain the plaintext password: {error}"
    );
}

/// キューされた削除（収集稼働中に 202 で保留し、停止後に適用）も同じ
/// カスケード仕様に揃っている - `execute_pending_apply` の
/// `"plc_connections.delete"` 分岐が `cascade_delete_tx` を呼ぶことの
/// 回帰テスト（この分岐を旧 `delete_tx` のまま直し忘れると、即時削除
/// (`plc_connections_delete`) は成功するのにキュー適用だけこの
/// バリデーションで失敗する、という非対称なバグになる）。
#[tokio::test]
async fn pending_apply_plc_connection_delete_cascades_when_it_has_groups_and_tags() {
    let env = test_env().await;

    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "protocol": "modbus-tcp", "name": "line-cascade-pending", "settings": { "host": "127.0.0.1", "port": 15062 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");
    let conn_id = conn["id"].as_i64().unwrap();

    let (status, group) = admin_post(
        &env.router,
        "/api/collection-groups",
        &env.admin_token,
        json!({ "name": "g-cascade-pending", "plcConnectionId": conn_id, "periodMs": 1000 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{group:?}");
    let group_id = group["id"].as_i64().unwrap();

    let (status, tag) = admin_post(
        &env.router,
        "/api/tags",
        &env.admin_token,
        json!({
            "name": "t-cascade-pending",
            "collectionGroupId": group_id,
            "address": "40001",
            "dataType": "i16"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{tag:?}");
    let tag_id = tag["id"].as_i64().unwrap();

    let start = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/start")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(start.status(), StatusCode::OK);

    // While running, the delete is only queued (202) - not yet applied.
    let queued = env
        .router
        .clone()
        .oneshot(
            HttpRequest::delete(format!("/api/plc-connections/{conn_id}"))
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(queued.status(), StatusCode::ACCEPTED);
    let queued_bytes = axum::body::to_bytes(queued.into_body(), usize::MAX)
        .await
        .unwrap();
    let queued_body: serde_json::Value = serde_json::from_slice(&queued_bytes).unwrap();
    let pending_id = queued_body["pending"]["id"].as_i64().unwrap();

    let stop = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/stop")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stop.status(), StatusCode::OK);

    let apply = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post(format!("/api/pending-changes/{pending_id}/apply"))
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(apply.status(), StatusCode::OK);
    let apply_bytes = axum::body::to_bytes(apply.into_body(), usize::MAX)
        .await
        .unwrap();
    let apply_body: serde_json::Value = serde_json::from_slice(&apply_bytes).unwrap();
    assert_eq!(apply_body["state"], "applied");

    assert!(matches!(
        PlcConnectionService::new(env.pool.clone())
            .get(conn_id)
            .await
            .unwrap_err(),
        BantoError::NotFound { .. }
    ));
    assert!(matches!(
        CollectionGroupService::new(env.pool.clone())
            .get(group_id)
            .await
            .unwrap_err(),
        BantoError::NotFound { .. }
    ));
    assert!(matches!(
        TagService::new(env.pool.clone())
            .get(tag_id)
            .await
            .unwrap_err(),
        BantoError::NotFound { .. }
    ));
}

/// 実機で再現した不具合の修正2（2026-08-31 オーナー報告）: pending change
/// の適用が validation エラー（名前の重複）で失敗したとき、
/// `failure_reason` にフィールド単位の詳細（`name: この接続内では既に使用されています`、
/// グループ名は 2026-10-19 から接続内で一意 - migration 0022）
/// が含まれることを確認する。修正前は `BantoError` の `Display`
/// （`thiserror`）が種別ごとの定型文だけ（`Validation` は常に
/// `"validation failed"`）だったため、`pending change の適用に失敗
/// しました: validation failed` としか出ず、何が失敗したのか分からな
/// かった - オーナーが収集稼働中に同じ名前（`group1`）で収集グループを
/// 3回作成し、3回とも適用が全滅した際にこれで気づけなかった。
#[tokio::test]
async fn pending_apply_collection_group_create_failure_reason_includes_field_detail() {
    let env = test_env().await;

    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "protocol": "modbus-tcp", "name": "line-dup-name-detail", "settings": { "host": "127.0.0.1", "port": 15042 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");
    let conn_id = conn["id"].as_i64().unwrap();

    // 先に "group1" を普通に（収集停止中に）作成しておく - 既存レコード
    // として DB に存在する状態を作る。
    let (status, existing) = admin_post(
        &env.router,
        "/api/collection-groups",
        &env.admin_token,
        json!({ "name": "group1", "plcConnectionId": conn_id, "periodMs": 100 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{existing:?}");

    let start = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/start")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(start.status(), StatusCode::OK);

    // 収集稼働中に同じ名前 "group1" で再度作成 → オーナーが実機で再現した
    // 状況そのもの: 202 でキューに入るだけで、DB には現れない。
    let queued = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection-groups")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "name": "group1", "plcConnectionId": conn_id, "periodMs": 100 })
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(queued.status(), StatusCode::ACCEPTED);
    let queued_bytes = axum::body::to_bytes(queued.into_body(), usize::MAX)
        .await
        .unwrap();
    let queued_body: serde_json::Value = serde_json::from_slice(&queued_bytes).unwrap();
    let pending_id = queued_body["pending"]["id"].as_i64().unwrap();

    let stop = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/stop")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stop.status(), StatusCode::OK);

    // 適用すると "name" が重複しているため validation エラーで拒否される。
    let response = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post(format!("/api/pending-changes/{pending_id}/apply"))
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let pending = PendingChangesService::new(env.pool.clone())
        .get(pending_id)
        .await
        .unwrap();
    assert_eq!(pending.state, PendingChangeState::Failed);
    let failure_reason = pending
        .failure_reason
        .expect("failure_reason should be set");
    assert_ne!(
        failure_reason, "pending change の適用に失敗しました: validation failed",
        "failure_reason should include field-level detail, not just the generic BantoError Display: {failure_reason}"
    );
    assert!(
        failure_reason.contains("name")
            && failure_reason.contains("この接続内では既に使用されています"),
        "failure_reason should say which field failed and why: {failure_reason}"
    );
}

/// TAG-P0-3 follow-up（2026-08-12）: pending change を enqueue した後、
/// キューを経由しない別経路（別セッションなどを模した直接の service
/// 呼び出し）で同じ `plc_connections` 行が書き換えられていた場合、
/// apply は conflict として拒否され、pending 行は `failed` へ遷移して
/// 具体的な `failure_reason` を残す。その後 `failed` からキャンセル
/// できることも確認する（従来は `pending` のみキャンセル可能だった）。
#[tokio::test]
async fn pending_apply_plc_connection_update_conflicts_when_row_changed_out_of_band() {
    let env = test_env().await;

    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "protocol": "modbus-tcp", "name": "line-fp-conflict", "settings": { "host": "127.0.0.1", "port": 15031 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");
    let conn_id = conn["id"].as_i64().unwrap();

    let start = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/start")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(start.status(), StatusCode::OK);

    let queued = env
        .router
        .clone()
        .oneshot(
            HttpRequest::put(format!("/api/plc-connections/{conn_id}"))
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "protocol": "modbus-tcp", "name": "line-fp-conflict-queued", "settings": { "host": "127.0.0.1", "port": 15031 } })
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(queued.status(), StatusCode::ACCEPTED);
    let queued_bytes = axum::body::to_bytes(queued.into_body(), usize::MAX)
        .await
        .unwrap();
    let queued_body: serde_json::Value = serde_json::from_slice(&queued_bytes).unwrap();
    let pending_id = queued_body["pending"]["id"].as_i64().unwrap();

    // 「別経路での編集」を、pending queue を経由しない直接の service 層
    // 呼び出し（`update_tx` ではなく非トランザクション `update`）で
    // 模す - 別セッションが同じ行を先に書き換えたケースに相当する。
    PlcConnectionService::new(env.pool.clone())
        .update(
            conn_id,
            banto_tags::PlcConnectionInput {
                name: "line-fp-conflict-hijacked".to_string(),
                protocol: "modbus-tcp".to_string(),
                host: "127.0.0.1".to_string(),
                port: 15031,
                unit_id: 1,
                enabled: true,
                simulation: false,

                word_order: "low_high".to_string(),
                cpu_type: None,
                database: None,
                username: None,
                secrets: Default::default(),
                settings: None,
            },
        )
        .await
        .unwrap();

    let stop = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/stop")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stop.status(), StatusCode::OK);

    let response = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post(format!("/api/pending-changes/{pending_id}/apply"))
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"], "pending_apply_conflict");
    assert_eq!(body["resource"], "plc_connections");

    let pending = PendingChangesService::new(env.pool.clone())
        .get(pending_id)
        .await
        .unwrap();
    assert_eq!(pending.state, PendingChangeState::Failed);
    let failure_reason = pending
        .failure_reason
        .expect("failure_reason should be set");
    assert!(
        !failure_reason.is_empty() && failure_reason != "pending change の適用に失敗しました",
        "failure_reason should be specific, not generic: {failure_reason}"
    );
    assert!(
        failure_reason.contains("変更されています"),
        "failure_reason should explain the staleness conflict: {failure_reason}"
    );

    // 元の接続は「別経路」の編集内容のまま(適用は拒否されたので上書き
    // されていない)。
    let untouched = PlcConnectionService::new(env.pool.clone())
        .get(conn_id)
        .await
        .unwrap();
    assert_eq!(untouched.name, "line-fp-conflict-hijacked");

    // failed からのキャンセルは新たに許可された復旧経路。
    let cancel_response = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post(format!("/api/pending-changes/{pending_id}/cancel"))
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cancel_response.status(), StatusCode::OK);
    let cancel_bytes = axum::body::to_bytes(cancel_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let cancel_body: serde_json::Value = serde_json::from_slice(&cancel_bytes).unwrap();
    assert_eq!(cancel_body["state"], "canceled");
}

/// TAG-P0-3 follow-up（2026-08-14）: requeue は fingerprint/payload を
/// 一切変更しないため、真のコンフリクト（enqueue 後に対象行が別経路で
/// 変わっている場合）は requeue → 再 apply でも安全に再度 fail し、
/// 対象行を上書きしないことを確認する。
#[tokio::test]
async fn pending_changes_requeue_conflict_still_fails_on_reapply_with_fingerprint_intact() {
    let env = test_env().await;

    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "protocol": "modbus-tcp", "name": "line-fp-conflict-requeue", "settings": { "host": "127.0.0.1", "port": 15033 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");
    let conn_id = conn["id"].as_i64().unwrap();

    let start = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/start")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(start.status(), StatusCode::OK);

    let queued = env
        .router
        .clone()
        .oneshot(
            HttpRequest::put(format!("/api/plc-connections/{conn_id}"))
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "protocol": "modbus-tcp", "name": "line-fp-conflict-requeue-queued", "settings": { "host": "127.0.0.1", "port": 15033 } })
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(queued.status(), StatusCode::ACCEPTED);
    let queued_bytes = axum::body::to_bytes(queued.into_body(), usize::MAX)
        .await
        .unwrap();
    let queued_body: serde_json::Value = serde_json::from_slice(&queued_bytes).unwrap();
    let pending_id = queued_body["pending"]["id"].as_i64().unwrap();

    // 別経路（pending queue を経由しない直接の service 呼び出し）で
    // 同じ行を書き換え、真のコンフリクトを作る。
    PlcConnectionService::new(env.pool.clone())
        .update(
            conn_id,
            banto_tags::PlcConnectionInput {
                name: "line-fp-conflict-requeue-hijacked".to_string(),
                protocol: "modbus-tcp".to_string(),
                host: "127.0.0.1".to_string(),
                port: 15033,
                unit_id: 1,
                enabled: true,
                simulation: false,

                word_order: "low_high".to_string(),
                cpu_type: None,
                database: None,
                username: None,
                secrets: Default::default(),
                settings: None,
            },
        )
        .await
        .unwrap();

    let stop = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/stop")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stop.status(), StatusCode::OK);

    let first_apply = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post(format!("/api/pending-changes/{pending_id}/apply"))
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first_apply.status(), StatusCode::CONFLICT);
    let first_apply_bytes = axum::body::to_bytes(first_apply.into_body(), usize::MAX)
        .await
        .unwrap();
    let first_apply_body: serde_json::Value = serde_json::from_slice(&first_apply_bytes).unwrap();
    assert_eq!(first_apply_body["error"], "pending_apply_conflict");
    assert_eq!(first_apply_body["resource"], "plc_connections");

    let pending_changes = PendingChangesService::new(env.pool.clone());
    let failed = pending_changes.get(pending_id).await.unwrap();
    assert_eq!(failed.state, PendingChangeState::Failed);

    // requeue は成功する（failed -> pending への差し戻しは常に許可）。
    let requeue_response = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post(format!("/api/pending-changes/{pending_id}/requeue"))
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(requeue_response.status(), StatusCode::OK);
    let requeue_bytes = axum::body::to_bytes(requeue_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let requeue_body: serde_json::Value = serde_json::from_slice(&requeue_bytes).unwrap();
    assert_eq!(requeue_body["state"], "pending");

    // しかし fingerprint/payload は据え置きのままなので、コンフリクトが
    // 解消されていない限り再 apply も同じ理由で再び fail する。
    let second_apply = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post(format!("/api/pending-changes/{pending_id}/apply"))
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(second_apply.status(), StatusCode::CONFLICT);
    let second_apply_bytes = axum::body::to_bytes(second_apply.into_body(), usize::MAX)
        .await
        .unwrap();
    let second_apply_body: serde_json::Value = serde_json::from_slice(&second_apply_bytes).unwrap();
    assert_eq!(second_apply_body["error"], "pending_apply_conflict");
    assert_eq!(second_apply_body["resource"], "plc_connections");

    let failed_again = pending_changes.get(pending_id).await.unwrap();
    assert_eq!(failed_again.state, PendingChangeState::Failed);

    // 対象行は「別経路」の編集内容のまま(再 apply も拒否されたので
    // 上書きされていない)。
    let untouched = PlcConnectionService::new(env.pool.clone())
        .get(conn_id)
        .await
        .unwrap();
    assert_eq!(untouched.name, "line-fp-conflict-requeue-hijacked");
}

/// TAG-P0-3 follow-up（2026-08-12）: `collection_groups.update` でも
/// 同じ conflict 検出が働く。
#[tokio::test]
async fn pending_apply_collection_group_update_conflicts_when_row_changed_out_of_band() {
    let env = test_env().await;

    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "protocol": "modbus-tcp", "name": "line-group-fp", "settings": { "host": "127.0.0.1", "port": 15032 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");
    let conn_id = conn["id"].as_i64().unwrap();

    let (status, group) = admin_post(
        &env.router,
        "/api/collection-groups",
        &env.admin_token,
        json!({ "name": "fast", "plcConnectionId": conn_id, "periodMs": 100 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{group:?}");
    let group_id = group["id"].as_i64().unwrap();

    let start = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/start")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(start.status(), StatusCode::OK);

    let queued = env
        .router
        .clone()
        .oneshot(
            HttpRequest::put(format!("/api/collection-groups/{group_id}"))
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "fast-queued",
                        "plcConnectionId": conn_id,
                        "periodMs": 100
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(queued.status(), StatusCode::ACCEPTED);
    let queued_bytes = axum::body::to_bytes(queued.into_body(), usize::MAX)
        .await
        .unwrap();
    let queued_body: serde_json::Value = serde_json::from_slice(&queued_bytes).unwrap();
    let pending_id = queued_body["pending"]["id"].as_i64().unwrap();

    // 別経路での編集（pending queue を経由しない直接の service 呼び出し）。
    CollectionGroupService::new(env.pool.clone())
        .update(
            group_id,
            CollectionGroupInput {
                name: "fast-hijacked".to_string(),
                plc_connection_id: conn_id,
                period_ms: 100,
                enabled: true,
                default_writable: true,
                query_sql: None,
            },
        )
        .await
        .unwrap();

    let stop = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/stop")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stop.status(), StatusCode::OK);

    let response = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post(format!("/api/pending-changes/{pending_id}/apply"))
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"], "pending_apply_conflict");
    assert_eq!(body["resource"], "collection_groups");

    let pending = PendingChangesService::new(env.pool.clone())
        .get(pending_id)
        .await
        .unwrap();
    assert_eq!(pending.state, PendingChangeState::Failed);
}

/// TAG-P0-3 follow-up（2026-08-12）、最重要の回帰ガード:
/// フィンガープリントは per-resource でなければならない
/// （グローバル `configured_revision` 比較に「こっそり」置き換えられて
/// いないことを保証する）。無関係な2件の pending change（別々の
/// `plc_connections` 行）を running 中に queue し、片方を適用して
/// `commit_catalog_and_notify` にグローバル revision を進めさせた後、
/// もう片方の適用が「global revision が動いた」ことに巻き込まれず
/// 成功することを確認する。
#[tokio::test]
async fn pending_apply_fingerprint_is_per_resource_not_global_revision() {
    let env = test_env().await;

    let (status, conn_a) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "protocol": "modbus-tcp", "name": "line-a", "settings": { "host": "127.0.0.1", "port": 15040 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn_a:?}");
    let conn_a_id = conn_a["id"].as_i64().unwrap();

    let (status, conn_b) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "protocol": "modbus-tcp", "name": "line-b", "settings": { "host": "127.0.0.1", "port": 15041 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn_b:?}");
    let conn_b_id = conn_b["id"].as_i64().unwrap();

    let start = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/start")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(start.status(), StatusCode::OK);

    // A・B それぞれ独立に pending update を queue する。
    let queue_update = |id: i64, name: &'static str, port: i64| {
        let router = env.router.clone();
        let token = env.admin_token.clone();
        async move {
            let response = router
                .oneshot(
                    HttpRequest::put(format!("/api/plc-connections/{id}"))
                        .header("Authorization", format!("Bearer {token}"))
                        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                        .header("content-type", "application/json")
                        .body(Body::from(
                            json!({ "protocol": "modbus-tcp", "name": name, "settings": { "host": "127.0.0.1", "port": port } }).to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::ACCEPTED);
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            body["pending"]["id"].as_i64().unwrap()
        }
    };
    let pending_a_id = queue_update(conn_a_id, "line-a-renamed", 15040).await;
    let pending_b_id = queue_update(conn_b_id, "line-b-renamed", 15041).await;

    let stop = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/stop")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stop.status(), StatusCode::OK);

    // A を先に適用する - 成功すれば `commit_catalog_and_notify` が
    // グローバル `configured_revision` を進める。
    let apply_a = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post(format!("/api/pending-changes/{pending_a_id}/apply"))
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(apply_a.status(), StatusCode::OK);
    let apply_a_bytes = axum::body::to_bytes(apply_a.into_body(), usize::MAX)
        .await
        .unwrap();
    let apply_a_body: serde_json::Value = serde_json::from_slice(&apply_a_bytes).unwrap();
    assert_eq!(apply_a_body["state"], "applied");

    // B の適用は、A の適用でグローバル revision が進んだことに巻き込ま
    // れず成功しなければならない - B のフィンガープリントは B 自身の
    // 行に対して取られており、A の変更とは無関係だから(per-resource
    // ガードの核心)。
    let apply_b = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post(format!("/api/pending-changes/{pending_b_id}/apply"))
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let apply_b_bytes_status = apply_b.status();
    let apply_b_bytes = axum::body::to_bytes(apply_b.into_body(), usize::MAX)
        .await
        .unwrap();
    let apply_b_body: serde_json::Value = serde_json::from_slice(&apply_b_bytes).unwrap();
    assert_eq!(
        apply_b_bytes_status,
        StatusCode::OK,
        "B's apply must not be rejected by A's global revision bump: {apply_b_body:?}"
    );
    assert_eq!(apply_b_body["state"], "applied");

    let updated_a = PlcConnectionService::new(env.pool.clone())
        .get(conn_a_id)
        .await
        .unwrap();
    assert_eq!(updated_a.name, "line-a-renamed");
    let updated_b = PlcConnectionService::new(env.pool.clone())
        .get(conn_b_id)
        .await
        .unwrap();
    assert_eq!(updated_b.name, "line-b-renamed");
}
