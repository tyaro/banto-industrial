//! H2（tyaro/banto-industrial#1053）: `GET`・`PUT`・`POST /api/plc-connections/{id}/clock-sync`
//! の認可・検証・保存・監査（実際に PLC へ書く経路は
//! `apps/banto-hub/host/tests/t12_slmp_wasm_clock` の模擬受入れ）。

use super::common::*;
use axum::http::StatusCode;
use serde_json::{json, Value};

async fn saved_connection(env: &TestEnv) -> i64 {
    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "protocol": "modbus-tcp", "name": "line-clock", "settings": { "host": "127.0.0.1", "port": 15301 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");
    conn["id"].as_i64().unwrap()
}

async fn clock_audit(
    pool: &sqlx::SqlitePool,
) -> Vec<(String, String, String, Option<String>, Value)> {
    use sqlx::Row;
    sqlx::query(
        "SELECT action, origin, result, entity_id, detail FROM audit_log \
         WHERE resource = 'plc_connection_clock_sync' ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
    .iter()
    .map(|row| {
        (
            row.get("action"),
            row.get("origin"),
            row.get("result"),
            row.get("entity_id"),
            serde_json::from_str(&row.get::<String, _>("detail")).unwrap(),
        )
    })
    .collect()
}

#[tokio::test]
async fn get_returns_the_default_and_put_saves_and_audits() {
    let env = test_env().await;
    let id = saved_connection(&env).await;
    let path = format!("/api/plc-connections/{id}/clock-sync");

    let (status, body) = admin_get(&env.router, &path, &env.admin_token).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(
        body,
        json!({
            "settings": { "enabled": false, "schedule": { "kind": "hourly", "minute": 5 } },
            "last": null
        })
    );

    let daily =
        json!({ "enabled": true, "schedule": { "kind": "daily", "hour": 23, "minute": 59 } });
    let (status, body) = admin_put(&env.router, &path, &env.admin_token, daily.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["settings"], daily);
    let (_, body) = admin_get(&env.router, &path, &env.admin_token).await;
    assert_eq!(body["settings"], daily);

    let rows = clock_audit(&env.pool).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        (rows[0].0.as_str(), rows[0].1.as_str(), rows[0].2.as_str()),
        ("update", "rest", "ok")
    );
    assert_eq!(rows[0].3.as_deref(), Some(id.to_string().as_str()));
    assert_eq!(rows[0].4, daily);
}

#[tokio::test]
async fn put_refuses_unknown_keys_and_out_of_range_with_400_and_saves_nothing() {
    let env = test_env().await;
    let id = saved_connection(&env).await;
    let path = format!("/api/plc-connections/{id}/clock-sync");
    for bad in [
        json!({ "enabled": true, "schedule": { "kind": "hourly", "minute": 60 } }),
        json!({ "enabled": true, "schedule": { "kind": "daily", "hour": 24, "minute": 0 } }),
        json!({ "enabled": true, "schedule": { "kind": "hourly", "minute": 5, "hour": 1 } }),
        json!({ "enabled": true, "schedule": { "kind": "cron", "minute": 5 } }),
        json!({ "enabled": true, "schedule": { "kind": "hourly", "minute": 5 }, "extra": 1 }),
        json!({ "enabled": true }),
    ] {
        let (status, body) = admin_put(&env.router, &path, &env.admin_token, bad.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body:?}");
    }
    let (_, body) = admin_get(&env.router, &path, &env.admin_token).await;
    assert_eq!(body["settings"]["enabled"], false);
    assert!(clock_audit(&env.pool).await.is_empty());
}

#[tokio::test]
async fn unknown_connection_is_404_and_a_viewer_is_403() {
    let env = test_env().await;
    let ok = json!({ "enabled": true, "schedule": { "kind": "hourly", "minute": 5 } });
    let (status, _) = admin_get(
        &env.router,
        "/api/plc-connections/999/clock-sync",
        &env.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = admin_put(
        &env.router,
        "/api/plc-connections/999/clock-sync",
        &env.admin_token,
        ok.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let id = saved_connection(&env).await;
    let path = format!("/api/plc-connections/{id}/clock-sync");
    // 収集を始めていないので、適用済みのカタログに無い（POST は 404、何も実行しない）。
    let (status, body) = admin_post(&env.router, &path, &env.admin_token, json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body:?}");

    let (status, _) = admin_get(&env.router, &path, &env.viewer_token).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = admin_put(&env.router, &path, &env.viewer_token, ok).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = admin_post(&env.router, &path, &env.viewer_token, json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(
        clock_audit(&env.pool).await.is_empty(),
        "拒否は plc_connections の denied として監査され、実行・更新は記録されない"
    );
    assert!(env.manager.clock_sync().last(id).is_none());
}

/// Hub の終了中（切り離したタスクの受付を閉じた後）は PUT・POST とも 503 で、保存も
/// 実行も監査もしない。
#[tokio::test]
async fn put_and_post_are_503_after_the_shutdown_closed_them() {
    let env = test_env().await;
    let id = saved_connection(&env).await;
    let path = format!("/api/plc-connections/{id}/clock-sync");
    env.manager.clock_sync().close_detached();
    let ok = json!({ "enabled": true, "schedule": { "kind": "hourly", "minute": 5 } });
    let (status, body) = admin_put(&env.router, &path, &env.admin_token, ok).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body:?}");
    let (status, body) = admin_post(&env.router, &path, &env.admin_token, json!({})).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body:?}");
    let (_, body) = admin_get(&env.router, &path, &env.admin_token).await;
    assert_eq!(body["settings"]["enabled"], false, "nothing saved");
    assert!(env.manager.clock_sync().last(id).is_none(), "nothing run");
    assert!(clock_audit(&env.pool).await.is_empty());
}
