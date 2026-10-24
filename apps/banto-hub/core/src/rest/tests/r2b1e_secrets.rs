//! R2b-1e（docs/banto-hub-connection-settings-design.md §5.3・§6.5・§8 の 1・§9）: REST の
//! 全入口（作成・更新・未適用キューに積む・取り消す・適用する）から送ったパスワードが、
//! キューに平文で載らず（積む時点で封印）、応答に平文も封印した値も出ず、起動時の後処理と
//! `VACUUM` の後は DB・WAL のバイト列に残らないこと。
//!
//! 「Windows の Hub の後処理」は偽の protector（`crate::test_support::protected_secrets`）で
//! 全 OS で走らせる（入口の封印は `platform_default`: Windows は本物の DPAPI、それ以外は
//! `plain-v1`）。MCP の入口は `apps/banto-hub/host/tests/mcp.rs` の
//! `mcp_connection_passwords_leave_no_plaintext_after_the_upkeep`。

use super::super::*;
use super::common::{admin_get, admin_post, admin_put, stored_password, test_env_on_file_db};
use axum::body::Body;
use axum::http::Request as HttpRequest;
use tower::ServiceExt;

const P_CREATE: &str = "TOPSECRET-rest-create-1a";
const P_UPDATE: &str = "TOPSECRET-rest-update-2b";
const P_QUEUED_CREATE: &str = "TOPSECRET-rest-qcreate-3c";
const P_QUEUED_UPDATE: &str = "TOPSECRET-rest-qupdate-4d";
const P_CANCELED: &str = "TOPSECRET-rest-canceled-5e";
const P_APPLIED: &str = "TOPSECRET-rest-applied-6f";
const ALL: [&str; 6] = [
    P_CREATE,
    P_UPDATE,
    P_QUEUED_CREATE,
    P_QUEUED_UPDATE,
    P_CANCELED,
    P_APPLIED,
];

fn pg_body(name: &str, password: Option<&str>) -> serde_json::Value {
    let mut body = json!({ "name": name, "protocol": "postgres", "settings": { "host": "127.0.0.1", "port": 1, "database": "erp", "username": "reader" } });
    if let Some(password) = password {
        body["settings"]["password"] = json!(password);
    }
    body
}

async fn post_empty(router: &Router, path: &str, token: &str) -> StatusCode {
    router
        .clone()
        .oneshot(
            HttpRequest::post(path)
                .header("Authorization", format!("Bearer {token}"))
                .header(
                    super::common::CLIENT_HEADER.0,
                    super::common::CLIENT_HEADER.1,
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

fn base64_of(text: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(text.as_bytes())
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

/// `text`（応答・DB の行）に既知のパスワードもその base64 も無いこと。
fn assert_no_secret(text: &str, what: &str) {
    for secret in ALL {
        assert!(!text.contains(secret), "{what} holds {secret}: {text}");
        assert!(
            !text.contains(&base64_of(secret)),
            "{what} holds base64({secret}): {text}"
        );
    }
}

async fn raw_queue(pool: &sqlx::SqlitePool) -> Vec<(i64, String, serde_json::Value)> {
    let rows: Vec<(i64, String, String)> =
        sqlx::query_as("SELECT id, state, payload FROM pending_changes ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap();
    rows.into_iter()
        .map(|(id, state, payload)| (id, state, serde_json::from_str(&payload).unwrap()))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_rest_entrance_leaves_no_known_password_after_the_upkeep() {
    let dir = crate::test_support::TempDir::new("r2b1e-rest-entrances");
    let path = dir.path().join("banto-hub.sqlite3");
    let env = test_env_on_file_db(&path).await;

    // Direct create and update (collection stopped).
    let (status, a) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        pg_body("pg-a", Some(P_CREATE)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{a:?}");
    assert_no_secret(&a.to_string(), "the create response");
    let a = a["id"].as_i64().unwrap();
    let (status, b) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        pg_body("pg-b", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{b:?}");
    let b = b["id"].as_i64().unwrap();
    let (status, updated) = admin_put(
        &env.router,
        &format!("/api/plc-connections/{b}"),
        &env.admin_token,
        pg_body("pg-b", Some(P_UPDATE)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated:?}");
    assert_eq!(
        stored_password(&env.pool, a).await.as_deref(),
        Some(P_CREATE)
    );
    assert_eq!(
        stored_password(&env.pool, b).await.as_deref(),
        Some(P_UPDATE)
    );

    // Queued (locked down + running): create, update, a create that is canceled and an
    // update that is applied.
    assert_eq!(
        post_empty(&env.router, "/api/collection/start", &env.admin_token).await,
        StatusCode::OK
    );
    let mut queued = Vec::new();
    for (method_put, path_id, body) in [
        (false, None, pg_body("pg-c", Some(P_QUEUED_CREATE))),
        (true, Some(a), pg_body("pg-a", Some(P_QUEUED_UPDATE))),
        (false, None, pg_body("pg-d", Some(P_CANCELED))),
        (true, Some(b), pg_body("pg-b", Some(P_APPLIED))),
    ] {
        let (status, response) = if method_put {
            admin_put(
                &env.router,
                &format!("/api/plc-connections/{}", path_id.unwrap()),
                &env.admin_token,
                body,
            )
            .await
        } else {
            admin_post(&env.router, "/api/plc-connections", &env.admin_token, body).await
        };
        assert_eq!(status, StatusCode::ACCEPTED, "{response:?}");
        let text = response.to_string();
        assert_no_secret(&text, "the queued response");
        assert!(!text.contains("sealedSecrets"), "{text}");
        assert_eq!(
            response["pending"]["payload"]["input"]["secretsSet"],
            json!(["password"])
        );
        queued.push(response["pending"]["id"].as_i64().unwrap());
    }
    let (canceled_id, applied_id) = (queued[2], queued[3]);
    assert_eq!(
        post_empty(
            &env.router,
            &format!("/api/pending-changes/{canceled_id}/cancel"),
            &env.admin_token
        )
        .await,
        StatusCode::OK
    );
    assert_eq!(
        post_empty(&env.router, "/api/collection/stop", &env.admin_token).await,
        StatusCode::OK
    );
    assert_eq!(
        post_empty(
            &env.router,
            &format!("/api/pending-changes/{applied_id}/apply"),
            &env.admin_token
        )
        .await,
        StatusCode::OK
    );
    // The password sealed at enqueue time is the one applied (never reopened to plaintext
    // in the queue).
    assert_eq!(
        stored_password(&env.pool, b).await.as_deref(),
        Some(P_APPLIED)
    );

    // The queue rows: unapplied ones carry only the sealed value; history carries neither.
    for (id, state, payload) in raw_queue(&env.pool).await {
        let input = &payload["input"];
        assert!(input.get("password").is_none(), "#{id} {state}: {payload}");
        assert!(
            input["settings"].get("password").is_none(),
            "#{id} {state}: {payload}"
        );
        if cfg!(windows) {
            assert_no_secret(&payload.to_string(), "a DPAPI-sealed queue row");
        }
        match state.as_str() {
            "pending" => assert!(input.get("sealedSecrets").is_some(), "#{id}: {payload}"),
            "applied" | "canceled" => {
                assert!(input.get("sealedSecrets").is_none(), "#{id}: {payload}");
                assert_eq!(input["secretsSet"], json!(["password"]), "#{id}");
            }
            other => panic!("unexpected state {other}"),
        }
    }
    let (status, listed) = admin_get(&env.router, "/api/pending-changes", &env.admin_token).await;
    assert_eq!(status, StatusCode::OK);
    let text = listed.to_string();
    assert_no_secret(&text, "GET /api/pending-changes");
    assert!(!text.contains("sealedSecrets"), "{text}");

    // The startup upkeep of a Windows Hub (a fake protector here) and its VACUUM.
    let report =
        crate::upgrade::secrets_upkeep(&env.pool, &crate::test_support::protected_secrets()).await;
    assert!(report.failures.is_empty(), "{report:?}");
    assert_eq!(report.rewrite_error, None, "{report:?}");
    assert_eq!(report.vacuum_error, None, "{report:?}");
    if cfg!(not(windows)) {
        // plain-v1 rows were resealed (pg-a's and pg-b's secret rows, the two pending
        // queue rows), so the mark was set and VACUUM ran.
        assert_eq!(report.resealed_rows, 2, "{report:?}");
        assert_eq!(report.resealed_queue, 2, "{report:?}");
        assert!(report.vacuumed, "{report:?}");
    }
    env.pool.close().await;

    let mut leaks = Vec::new();
    for file in [
        path.clone(),
        std::path::PathBuf::from(format!("{}-wal", path.display())),
    ] {
        let Ok(bytes) = std::fs::read(&file) else {
            continue;
        };
        for secret in ALL {
            for needle in [secret.to_string(), base64_of(secret)] {
                if contains(&bytes, &needle) {
                    leaks.push(format!("{needle} in {}", file.display()));
                }
            }
        }
    }
    assert!(
        leaks.is_empty(),
        "known passwords left in the files: {leaks:?}"
    );
}

/// 設計 §5.5・§5.8・§6.5（R2b-1e）: 管理系の `GET /api/status` の `secretStorage`（方式・
/// 非 Windows の平文の警告・Windows で平文のまま残った件数）と、`dbSource` の接続ごとの
/// `passwordState`・`passwordUnreadableReason`。`/api/v1/status`（機械クライアント向け）の
/// 形は変えない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_status_shows_the_secret_storage_and_unreadable_db_source_passwords() {
    let env = super::common::test_env().await;
    let (status, pg) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        pg_body("pg-status", Some(P_CREATE)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{pg:?}");
    let pg = pg["id"].as_i64().unwrap();
    let (status, group) = admin_post(
        &env.router,
        "/api/collection-groups",
        &env.admin_token,
        json!({ "name": "q1", "plcConnectionId": pg, "periodMs": 1000, "querySql": "SELECT 1 AS a" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{group:?}");
    let group = group["id"].as_i64().unwrap();
    // The row goes missing; the tag created next re-plans the DB Source.
    sqlx::query("DELETE FROM plc_connection_secrets WHERE connection_id = ?")
        .bind(pg)
        .execute(&env.pool)
        .await
        .unwrap();
    let (status, tag) = admin_post(
        &env.router,
        "/api/tags",
        &env.admin_token,
        json!({
            "name": "A", "collectionGroupId": group, "address": "a",
            "dataType": "f32", "tagKind": "db", "enabled": true
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{tag:?}");

    let (status, admin) = admin_get(&env.router, "/api/status", &env.admin_token).await;
    assert_eq!(status, StatusCode::OK, "{admin:?}");
    let storage = &admin["secretStorage"];
    if cfg!(windows) {
        assert_eq!(storage["scheme"], json!("dpapi-machine-v1"), "{storage}");
        assert_eq!(
            storage["plaintextWarning"],
            serde_json::Value::Null,
            "{storage}"
        );
    } else {
        assert_eq!(storage["scheme"], json!("plain-v1"), "{storage}");
        assert_eq!(
            storage["plaintextWarning"],
            json!("秘密値を平文で保存しています（Windows 以外）"),
            "{storage}"
        );
    }
    assert_eq!(storage["plaintextRemaining"], json!(0), "{storage}");
    assert_eq!(
        storage["plaintextRemainingWarning"],
        serde_json::Value::Null
    );
    let entry = admin["dbSource"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["connectionId"] == json!(pg))
        .unwrap_or_else(|| panic!("no dbSource entry: {admin}"))
        .clone();
    assert_eq!(entry["state"], json!("error"), "{entry}");
    assert_eq!(entry["passwordState"], json!("unreadable"), "{entry}");
    assert_eq!(
        entry["passwordUnreadableReason"],
        json!("missing"),
        "{entry}"
    );
    assert!(
        entry["lastError"]
            .as_str()
            .unwrap()
            .starts_with("パスワード: 読めません（この PC で再入力してください）"),
        "{entry}"
    );

    // The machine-client status keeps its shape (no new fields).
    let (status, v1) = super::common::v1_get(&env.router, &env.admin_token, "/api/v1/status").await;
    assert_eq!(status, StatusCode::OK, "{v1:?}");
    assert!(v1.get("secretStorage").is_none() && v1.get("secret_storage").is_none());
    let v1_entry = v1["db_source"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["connection_id"] == json!(pg))
        .unwrap()
        .clone();
    assert!(v1_entry.get("password_unreadable").is_none(), "{v1_entry}");
    assert!(v1_entry.get("passwordState").is_none(), "{v1_entry}");
}

/// Windows（protector がある）で `plain-v1` の行が残っていると、`secretStorage` に件数と
/// 「移行待ちの秘密値が平文で残っています」が出る（設計 §6.5、本物の DPAPI。windows-latest の CI）。
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_status_counts_plain_rows_left_on_windows() {
    let env = super::common::test_env().await;
    sqlx::query(
        "INSERT INTO plc_connection_secrets (ref, connection_id, key, scheme, blob) \
         VALUES ('0123456789abcdef0123456789abcdef', 99, 'password', 'plain-v1', X'6f')",
    )
    .execute(&env.pool)
    .await
    .unwrap();
    let (status, admin) = admin_get(&env.router, "/api/status", &env.admin_token).await;
    assert_eq!(status, StatusCode::OK, "{admin:?}");
    let storage = &admin["secretStorage"];
    assert_eq!(storage["plaintextRemaining"], json!(1), "{storage}");
    assert_eq!(
        storage["plaintextRemainingWarning"],
        json!("移行待ちの秘密値が平文で残っています（1 件）"),
        "{storage}"
    );
}
