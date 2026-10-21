//! R2a-2e の完了の確かめ（docs/banto-hub-ingest-design.md §11 の R2a-2e の行）の
//! サーバー側:
//!
//! - `GET /api/status`（camelCase）・`GET /api/v1/status`（snake_case）の `ingest` 節
//!   （§9）。**資格情報と値を出さない**（キーの値・送った値が応答に無い）。
//! - `push` 接続の `settings` の REST の往復（作成・取得・省略した更新は保つ・`{}` で
//!   ブリッジを外す・push 以外は断る）。
//! - DB ブリッジの読み取り先の `postgres` 接続の削除の拒否（§3.2。ブリッジを外せば
//!   消せる）。

use super::super::*;
use super::common::*;
use axum::body::Body;
use axum::http::Request as HttpRequest;
use banto_tstore::ManualClock;
use serde_json::{json, Value};
use tower::ServiceExt;

async fn post_values(router: &Router, id: i64, token: &str, body: Value) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            HttpRequest::post(format!("/api/v1/ingest/{id}/values"))
                .header("content-type", "application/json")
                .header("Authorization", format!("Bearer {token}"))
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn create(env: &TestEnv, body: Value) -> Value {
    let (status, created) =
        admin_post(&env.router, "/api/plc-connections", &env.admin_token, body).await;
    assert_eq!(status, StatusCode::OK, "{created:?}");
    created
}

async fn postgres(env: &TestEnv, name: &str) -> i64 {
    create(
        env,
        json!({ "name": name, "protocol": "postgres", "host": "10.0.0.5", "port": 5432,
                "database": "mes", "username": "reader", "password": "s3cret" }),
    )
    .await["id"]
        .as_i64()
        .unwrap()
}

fn bridge(db_connection_id: i64) -> Value {
    json!({
        "stale_timeout_ms": 5000,
        "bridge": { "kind": "db", "db_connection_id": db_connection_id, "queries": [
            { "name": "line1", "period_ms": 1000, "sql": "SELECT temp FROM line1_now",
              "columns": [ { "column": "temp", "key": "line1.temp" } ] } ] }
    })
}

/// §9: `ingest` 節。送った値（12.5）・キーの値（`bh_...`）・DB のパスワードは
/// どちらの応答の `ingest` 節にも出ない。タイムアウトの後は `down`。
#[tokio::test]
async fn the_status_ingest_section_reports_the_sender_without_secrets_or_values() {
    let clock = Arc::new(ManualClock::new(1_000_000, 0));
    let env = test_env_with_clock(clock.clone()).await;
    let conn = create(
        &env,
        json!({ "name": "Script1", "protocol": "push", "host": "", "port": 0,
                "settings": { "stale_timeout_ms": 2000 } }),
    )
    .await;
    let id = conn["id"].as_i64().unwrap();
    let (status, group) = admin_post(
        &env.router,
        "/api/collection-groups",
        &env.admin_token,
        json!({ "name": "g", "plcConnectionId": id, "periodMs": 1000 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{group:?}");
    let (status, tag) = admin_post(
        &env.router,
        "/api/tags",
        &env.admin_token,
        json!({ "name": "temp", "collectionGroupId": group["id"], "address": "line1.temp",
                "dataType": "f32", "enabled": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{tag:?}");
    let (status, issued) = issue_api_key(
        &env.router,
        &env.admin_token,
        "script",
        &[&format!("ingest:#{id}")],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{issued:?}");
    let ingest_key = issued["key"].as_str().unwrap().to_string();
    let (status, issued) = issue_api_key(&env.router, &env.admin_token, "reader", &["read"]).await;
    assert_eq!(status, StatusCode::CREATED, "{issued:?}");
    let read_key = issued["key"].as_str().unwrap().to_string();

    let (status, ack) = post_values(
        &env.router,
        id,
        &ingest_key,
        json!({ "items": [ { "key": "line1.temp", "v": 12.5, "t": 1_761_000_000_000_i64 },
                           { "key": "line1.tmp", "v": 1 } ] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ack:?}");
    assert_eq!(ack["accepted"], 1);

    // 管理（camelCase）。
    let (status, admin) = admin_get(&env.router, "/api/status", &env.admin_token).await;
    assert_eq!(status, StatusCode::OK, "{admin:?}");
    let entries = admin["ingest"].as_array().expect("ingest 節");
    assert_eq!(entries.len(), 1, "{entries:?}");
    let entry = &entries[0];
    assert_eq!(entry["connectionId"], id);
    assert_eq!(entry["name"], "Script1");
    assert_eq!(entry["link"], "live");
    assert_eq!(entry["linkDetail"], Value::Null);
    assert_eq!(entry["lastSeenAt"], 1_000_000);
    assert_eq!(entry["staleTimeoutMs"], 2000);
    assert_eq!(entry["collecting"], false);
    assert_eq!(entry["bridge"], false);
    assert_eq!(entry["epoch"], 1);
    assert_eq!(entry["heldValues"], 1);
    assert_eq!(entry["accepted"], 1);
    assert_eq!(entry["rejected"], 1);
    assert_eq!(entry["rateLimited"], 0);
    assert_eq!(entry["tooManyItems"], 0);
    assert_eq!(entry["unknownKeys"], json!(["line1.tmp"]));
    assert_eq!(entry["lastSourceTime"], 1_761_000_000_000_i64);
    assert_eq!(entry["senderStatus"], Value::Null);
    assert_eq!(
        entry["senders"],
        json!([ { "transport": "rest", "apiKeyName": "script", "peer": null,
                  "connectedAt": null, "lastSeenAt": 1_000_000 } ])
    );
    let text = entry.to_string();
    assert!(!text.contains("12.5"), "値を出さない: {text}");
    assert!(!text.contains(&ingest_key), "キーの値を出さない: {text}");
    assert!(!text.contains("bh_"), "キーの値を出さない: {text}");
    assert!(!admin.to_string().contains("s3cret"));

    // 機械向け（snake_case）。
    let (status, v1) = v1_get(&env.router, &read_key, "/api/v1/status").await;
    assert_eq!(status, StatusCode::OK, "{v1:?}");
    let entry = &v1["ingest"][0];
    assert_eq!(entry["connection_id"], id);
    assert_eq!(entry["link"], "live");
    assert_eq!(entry["unknown_keys"], json!(["line1.tmp"]));
    assert_eq!(entry["senders"][0]["api_key_name"], "script");
    assert!(!entry.to_string().contains("12.5"));
    // 接続の一覧の push 接続は外部接続の規則のまま（§9。画面が「送り手待ち」と読む）。
    assert!(v1["connections"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["id"] == id));

    // タイムアウト（2 秒）を過ぎると down（理由つき）。
    clock.advance_ms(2_001);
    let (_, admin) = admin_get(&env.router, "/api/status", &env.admin_token).await;
    assert_eq!(admin["ingest"][0]["link"], "down");
    assert_eq!(
        admin["ingest"][0]["linkDetail"],
        "送り手からの通信が 2 秒ありません"
    );
}

/// `push` 接続が無ければ `ingest` 節は空配列。
#[tokio::test]
async fn the_status_ingest_section_is_empty_without_push_connections() {
    let env = test_env().await;
    let (status, admin) = admin_get(&env.router, "/api/status", &env.admin_token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(admin["ingest"], json!([]));
}

/// §3.2: `settings` の REST の往復。省略した更新は保ち、`{}` でブリッジを外す。push
/// 以外の `settings` は 422。
#[tokio::test]
async fn push_settings_round_trip_through_rest() {
    let env = test_env().await;
    let db = postgres(&env, "pg-a").await;
    let created = create(
        &env,
        json!({ "name": "Bridge1", "protocol": "push", "host": "", "port": 0,
                "settings": bridge(db) }),
    )
    .await;
    let id = created["id"].as_i64().unwrap();
    assert_eq!(created["settings"], bridge(db));
    let (status, got) = admin_get(
        &env.router,
        &format!("/api/plc-connections/{id}"),
        &env.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(got["settings"], bridge(db));
    // postgres 接続の応答は settings を null で返す（パスワードは返さない）。
    let (_, pg) = admin_get(
        &env.router,
        &format!("/api/plc-connections/{db}"),
        &env.admin_token,
    )
    .await;
    assert_eq!(pg["settings"], Value::Null);
    assert!(pg.get("password").is_none());

    // 省略した更新（この項目を知らないクライアント）は保つ。
    let (status, kept) = admin_put(
        &env.router,
        &format!("/api/plc-connections/{id}"),
        &env.admin_token,
        json!({ "name": "Bridge1 renamed", "protocol": "push", "host": "", "port": 0 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{kept:?}");
    assert_eq!(kept["settings"], bridge(db));
    // `{}` でブリッジを外す。
    let (status, cleared) = admin_put(
        &env.router,
        &format!("/api/plc-connections/{id}"),
        &env.admin_token,
        json!({ "name": "Bridge1", "protocol": "push", "host": "", "port": 0, "settings": {} }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{cleared:?}");
    assert_eq!(cleared["settings"], json!({}));

    // push 以外で settings は 422（項目は settings）。
    let (status, refused) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "name": "plc", "protocol": "modbus-tcp", "host": "127.0.0.1", "port": 502,
                "settings": {} }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused:?}");
    assert!(refused.to_string().contains("settings"), "{refused:?}");
    // 見積もりを超えるブリッジは 422（`settings.bridge`、§7.1 の文言）。
    let queries: Vec<Value> = ["a", "b", "c"]
        .iter()
        .map(|n| {
            json!({ "name": n, "period_ms": 100, "sql": "SELECT 1 AS v",
                    "columns": [ { "column": "v", "key": n } ] })
        })
        .collect();
    let fast = json!({ "bridge": { "kind": "db", "db_connection_id": db, "queries": queries } });
    let (status, refused) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "name": "Fast", "protocol": "push", "host": "", "port": 0, "settings": fast }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused:?}");
    assert!(
        refused.to_string().contains("約 30 メッセージ"),
        "{refused:?}"
    );
}

/// §3.2: DB ブリッジの読み取り先の `postgres` 接続は削除できない（422、ブリッジの
/// 名前を出す）。ブリッジを外せば消せる。参照されていない postgres 接続は今までどおり。
#[tokio::test]
async fn deleting_a_postgres_connection_a_bridge_reads_is_refused() {
    let env = test_env().await;
    let db = postgres(&env, "pg-a").await;
    let unused = postgres(&env, "pg-b").await;
    let bridge_id = create(
        &env,
        json!({ "name": "Bridge1", "protocol": "push", "host": "", "port": 0,
                "settings": bridge(db) }),
    )
    .await["id"]
        .as_i64()
        .unwrap();

    let (status, refused) = admin_delete(
        &env.router,
        &format!("/api/plc-connections/{db}"),
        &env.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused:?}");
    let text = refused.to_string();
    assert!(text.contains("Bridge1"), "{text}");
    assert!(text.contains("削除できません"), "{text}");
    let (status, _) = admin_get(
        &env.router,
        &format!("/api/plc-connections/{db}"),
        &env.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "行は残る");

    let (status, _) = admin_delete(
        &env.router,
        &format!("/api/plc-connections/{unused}"),
        &env.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = admin_put(
        &env.router,
        &format!("/api/plc-connections/{bridge_id}"),
        &env.admin_token,
        json!({ "name": "Bridge1", "protocol": "push", "host": "", "port": 0,
                "settings": { "stale_timeout_ms": 5000 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = admin_delete(
        &env.router,
        &format!("/api/plc-connections/{db}"),
        &env.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}
