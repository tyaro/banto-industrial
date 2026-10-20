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
