//! H3（tyaro/banto-industrial#1053）: `GET /api/plc-connections/{id}/clock` の認可・
//! 404・応答の形（実際に PLC から読む経路と使い回しは
//! `apps/banto-hub/host/tests/t12_slmp_wasm_clock` の模擬受入れと `crate::clock_read` の
//! 単体）。

use super::common::*;
use crate::db::Db;
use crate::users::{Role, UsersService};
use axum::http::StatusCode;
use serde_json::{json, Value};

async fn saved_connection(env: &TestEnv) -> i64 {
    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "protocol": "modbus-tcp", "name": "line-clock-read", "settings": { "host": "127.0.0.1", "port": 15302 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");
    conn["id"].as_i64().unwrap()
}

async fn editor_token(env: &TestEnv) -> String {
    UsersService::new(Db::Sqlite(env.pool.clone()))
        .create_user("editor1", "password123", "編集者", Role::Editor)
        .await
        .expect("create_user editor");
    let (status, login) = admin_post(
        &env.router,
        "/api/auth/login",
        &env.admin_token,
        json!({ "username": "editor1", "password": "password123" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{login:?}");
    login["token"].as_str().unwrap().to_string()
}

async fn audit_count(pool: &sqlx::SqlitePool, resource: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE resource = ?")
        .bind(resource)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// editor は 200 で、収集していない接続（broker のセッションが無い）は送らずに
/// `unavailable`（使い回さないので 2 回目も `cached: false`）。Hub の
/// `clockSync.enabled` は既定の偽のまま。読取りは監査しない。
#[tokio::test]
async fn an_editor_reads_and_the_shape_is_the_reading_plus_cached() {
    let env = test_env().await;
    let id = saved_connection(&env).await;
    let path = format!("/api/plc-connections/{id}/clock");
    let editor = editor_token(&env).await;
    let before = audit_count(&env.pool, "plc_connection_clock_sync").await;
    for token in [&editor, &env.admin_token] {
        let (status, body) = admin_get(&env.router, &path, token).await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        let keys: Vec<&str> = body
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let mut keys = keys;
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "at",
                "cached",
                "code",
                "message",
                "offsetSeconds",
                "pcTime",
                "plcTime",
                "result"
            ],
            "{body:?}"
        );
        assert_eq!(body["result"], "unavailable", "{body:?}");
        assert_eq!(body["cached"], false);
        assert_eq!(body["plcTime"], Value::Null);
        assert_eq!(body["offsetSeconds"], Value::Null);
        assert_eq!(body["code"], Value::Null);
        assert_eq!(body["pcTime"].as_str().unwrap().len(), 19, "{body:?}");
    }
    assert_eq!(
        audit_count(&env.pool, "plc_connection_clock_sync").await,
        before,
        "the read is not audited"
    );
}

#[tokio::test]
async fn unknown_connection_is_404_and_a_viewer_is_403_and_audited() {
    let env = test_env().await;
    let (status, _) = admin_get(
        &env.router,
        "/api/plc-connections/999/clock",
        &env.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let id = saved_connection(&env).await;
    let path = format!("/api/plc-connections/{id}/clock");
    let denied_before = audit_count(&env.pool, "plc_connections").await;
    let (status, _) = admin_get(&env.router, &path, &env.viewer_token).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        audit_count(&env.pool, "plc_connections").await,
        denied_before + 1,
        "the refusal is audited like the other editor endpoints"
    );
}
