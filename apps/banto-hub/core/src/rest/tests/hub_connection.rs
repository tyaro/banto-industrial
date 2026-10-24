use super::super::*;
use super::common::{admin_post, admin_put, test_env, CLIENT_HEADER};
use crate::pending_changes::PendingChangeState;
use axum::body::Body;
use axum::http::Request as HttpRequest;
use tower::ServiceExt;

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
        json!({ "name": "hub", "protocol": "hub" }),
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
        json!({ "protocol": "modbus-tcp", "name": "line-hub-switch", "settings": { "host": "127.0.0.1", "port": 15071 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");
    let id = conn["id"].as_i64().unwrap();
    let (status, body) = admin_put(
        &env.router,
        &format!("/api/plc-connections/{id}"),
        &env.admin_token,
        json!({ "name": "line-hub-switch", "protocol": "hub" }),
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
        "input": { "name": "hub", "protocol": "hub", "settings": {} }
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

// --- R2a-2a / R2a-1c: `push` 接続（docs/banto-hub-ingest-design.md §3.1・§4.2） ----
//
// REST は `push` を `hub` のように入口で断らない（Hub が作る種類、§1.4）。R2a-2a の
// 時点では受信（`IngestHub`）が収集に繋がっておらず、preflight（厳格版の
// `build_config_from`、`PushSource::Unavailable`）が**有効な** `push` 接続を
// `pushSourceUnavailable` で断っていた。R2a-1c で Hub の計画と preflight を
// `crate::ingest::build_hub_config`（`PushSource::Available`）に替えたので、有効な
// `push` 接続も保存でき、収集に載る。

/// 有効な `push` 接続も作れる（R2a-1c で preflight の拒否を外した）。無効でも作れ、
/// host/port は正規化される。REST の更新は `settings` を運ばないので、保存済みの
/// `settings` を消さない（`PlcConnectionInput::settings` の「省略は保つ」）。有効へ
/// 戻す更新も通る。
#[tokio::test]
async fn push_connections_are_accepted_by_preflight_now_that_the_receiver_exists() {
    let env = test_env().await;

    let (status, body) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "name": "bridge-0", "protocol": "push" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["protocol"], "push");
    assert_eq!(body["enabled"], true);
    let rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM plc_connections WHERE protocol = 'push'")
            .fetch_one(&env.pool)
            .await
            .unwrap();
    assert_eq!(rows, 1);
    // 収集に載る形（グループとタグ）まで preflight を通る。
    let (status, group) = admin_post(
        &env.router,
        "/api/collection-groups",
        &env.admin_token,
        json!({ "name": "g", "plcConnectionId": body["id"], "periodMs": 1000 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{group:?}");
    let (status, tag) = admin_post(
        &env.router,
        "/api/tags",
        &env.admin_token,
        json!({ "name": "temp", "collectionGroupId": group["id"],
                "address": " line1.temp ", "dataType": "f32" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{tag:?}");
    assert_eq!(tag["address"], "line1.temp");

    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "name": "bridge-1", "protocol": "push", "enabled": false }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");
    assert_eq!(conn["protocol"], "push");
    assert_eq!(conn["settings"], json!({}));
    let id = conn["id"].as_i64().unwrap();

    sqlx::query("UPDATE plc_connections SET settings = '{\"stale_timeout_ms\":2000}' WHERE id = ?")
        .bind(id)
        .execute(&env.pool)
        .await
        .unwrap();
    let (status, body) = admin_put(
        &env.router,
        &format!("/api/plc-connections/{id}"),
        &env.admin_token,
        // R2b-6b: the update replaces `settings`, so it carries the stored object.
        json!({ "name": "bridge-1-renamed", "protocol": "push", "enabled": false,
                "settings": { "stale_timeout_ms": 2000 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let settings: Option<String> =
        sqlx::query_scalar("SELECT settings FROM plc_connections WHERE id = ?")
            .bind(id)
            .fetch_one(&env.pool)
            .await
            .unwrap();
    assert_eq!(settings.as_deref(), Some(r#"{"stale_timeout_ms":2000}"#));

    let (status, body) = admin_put(
        &env.router,
        &format!("/api/plc-connections/{id}"),
        &env.admin_token,
        json!({ "name": "bridge-1-renamed", "protocol": "push", "enabled": true,
                "settings": { "stale_timeout_ms": 2000 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["enabled"], true);
}
