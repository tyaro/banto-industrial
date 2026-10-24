//! R2b-1d（docs/banto-hub-connection-settings-design.md §5.4・§5.5・§2）: パスワードを
//! 読めない接続を読む側（REST の `passwordState`・`GET /api/sink/config`・
//! `GET /api/ingest/bridge-config`・保存済みの接続のテスト）が空のパスワードに潰さないこと、
//! と `push` の `settings` の REST の形（既定のままなら `null`）と未適用キューの指紋の互換。
//!
//! 「読めない」は 2 通りを作る: 秘密値の行を生の SQL で消す（`missing`）、行の方式を
//! この PC で開けない `dpapi-machine-v1`（blob は 1 バイトの壊れた値）に書き換える
//! （`undecryptable`。protector の無い PC でも、R2b-1e の Windows の DPAPI でも開けない）。
//! PR #777 のオーナーレビュー P2 で 3 通り目: `settings.password` を形の違う参照
//! （`{"$secret": 123}`）に書き換える（`invalid`）。

use super::super::*;
use super::common::{admin_get, admin_post, admin_put, issue_api_key, test_env, v1_get};

const PASSWORD: &str = "s3cret-r2b1d";

async fn create_pg(env: &super::common::TestEnv, name: &str, password: Option<&str>) -> i64 {
    let mut body = json!({ "name": name, "protocol": "postgres", "settings": { "host": "127.0.0.1", "port": 1, "database": "erp", "username": "reader" } });
    if let Some(password) = password {
        body["settings"]["password"] = json!(password);
    }
    let (status, created) =
        admin_post(&env.router, "/api/plc-connections", &env.admin_token, body).await;
    assert_eq!(status, StatusCode::OK, "{created:?}");
    created["id"].as_i64().unwrap()
}

async fn drop_secret_row(pool: &sqlx::SqlitePool, connection_id: i64) {
    sqlx::query("DELETE FROM plc_connection_secrets WHERE connection_id = ?")
        .bind(connection_id)
        .execute(pool)
        .await
        .unwrap();
}

async fn make_undecryptable(pool: &sqlx::SqlitePool, connection_id: i64) {
    sqlx::query(
        "UPDATE plc_connection_secrets SET scheme = 'dpapi-machine-v1', blob = X'00' \
         WHERE connection_id = ?",
    )
    .bind(connection_id)
    .execute(pool)
    .await
    .unwrap();
}

/// PR #777 のオーナーレビュー P2: `settings.password` のキーはあるが形の正しい参照でない
/// （生の SQL で書いた `{"$secret": 123}`）。読む側は「パスワード無し」に潰さず `invalid`。
async fn make_malformed(pool: &sqlx::SqlitePool, connection_id: i64) {
    sqlx::query(
        "UPDATE plc_connections SET settings = json_set(settings, '$.password', \
         json('{\"$secret\":123}')) WHERE id = ?",
    )
    .bind(connection_id)
    .execute(pool)
    .await
    .unwrap();
}

/// §5.5: `passwordState` は `none` / `set` / `unreadable`、`passwordSet` は
/// `passwordState == "set"`（読めないものは `false`）。一覧・単体・更新の応答が同じ。
#[tokio::test]
async fn password_state_tells_none_set_and_unreadable_apart() {
    let env = test_env().await;
    let none = create_pg(&env, "pg-none", None).await;
    let set = create_pg(&env, "pg-set", Some(PASSWORD)).await;
    let missing = create_pg(&env, "pg-missing", Some(PASSWORD)).await;
    let undecryptable = create_pg(&env, "pg-undecryptable", Some(PASSWORD)).await;
    let invalid = create_pg(&env, "pg-invalid", Some(PASSWORD)).await;
    drop_secret_row(&env.pool, missing).await;
    make_undecryptable(&env.pool, undecryptable).await;
    make_malformed(&env.pool, invalid).await;

    let expect = [
        (none, "none", false),
        (set, "set", true),
        (missing, "unreadable", false),
        (undecryptable, "unreadable", false),
        (invalid, "unreadable", false),
    ];
    let (status, listed) = admin_get(&env.router, "/api/plc-connections", &env.admin_token).await;
    assert_eq!(status, StatusCode::OK, "{listed:?}");
    for (id, state, _password_set) in expect {
        let entry = listed
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == json!(id))
            .unwrap();
        // R2b-6b: `settings.password` is `{"$secret": state}`, absent for "none".
        let want = if state == "none" {
            None
        } else {
            Some(json!({ "$secret": state }))
        };
        assert_eq!(
            entry["settings"].get("password").cloned(),
            want,
            "{entry:?}"
        );
        let (status, one) = admin_get(
            &env.router,
            &format!("/api/plc-connections/{id}"),
            &env.admin_token,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(one["settings"].get("password").cloned(), want, "{one:?}");
        assert!(one.get("password").is_none());
    }
    assert!(!listed.to_string().contains(PASSWORD));

    // Re-entering the password fixes it (design §5.5「直し方」).
    let (status, fixed) = admin_put(
        &env.router,
        &format!("/api/plc-connections/{missing}"),
        &env.admin_token,
        json!({ "name": "pg-missing", "protocol": "postgres", "settings": { "host": "127.0.0.1", "port": 1, "database": "erp", "username": "reader", "password": "new-pw" } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{fixed:?}");
    assert_eq!(fixed["settings"]["password"], json!({ "$secret": "set" }));
}

/// §5.4: 保存済みの接続のテストは、読めないパスワードで繋ぎに行かず `ok: false` と理由。
#[tokio::test]
async fn testing_a_saved_connection_with_an_unreadable_password_does_not_connect() {
    let env = test_env().await;
    for reason in ["missing", "undecryptable", "invalid"] {
        let id = create_pg(&env, &format!("pg-test-{reason}"), Some(PASSWORD)).await;
        match reason {
            "missing" => drop_secret_row(&env.pool, id).await,
            "undecryptable" => make_undecryptable(&env.pool, id).await,
            _ => make_malformed(&env.pool, id).await,
        }
        let (status, body) = admin_post(
            &env.router,
            &format!("/api/plc-connections/{id}/test"),
            &env.admin_token,
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(body["ok"], json!(false), "{body:?}");
        let error = body["error"].as_str().unwrap();
        assert!(error.contains("パスワード: 読めません"), "{error}");
        // The reason, not the closed port (1)'s connection failure: the password check
        // comes first and nothing is dialled.
        assert!(error.contains(reason), "{error}");
        assert!(!error.contains("タイムアウト"), "{error}");
    }
}

/// §5.4: `GET /api/sink/config` は読めない接続を `connections` に載せず（`password: null`
/// でも返さない）、`unavailableConnections` に理由つきで出す。読める接続はそのまま。
#[tokio::test]
async fn sink_config_lists_unreadable_connections_apart() {
    let env = test_env().await;
    let (status, plc) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "protocol": "modbus-tcp", "name": "line1", "settings": { "host": "127.0.0.1", "port": 15201 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{plc:?}");
    let (_, group) = admin_post(
        &env.router,
        "/api/collection-groups",
        &env.admin_token,
        json!({ "name": "fast", "plcConnectionId": plc["id"], "periodMs": 100 }),
    )
    .await;
    let (_, tag) = admin_post(
        &env.router,
        "/api/tags",
        &env.admin_token,
        json!({ "name": "temp01", "collectionGroupId": group["id"], "address": "40001",
                "dataType": "i16" }),
    )
    .await;
    let readable = create_pg(&env, "pg-ok", Some(PASSWORD)).await;
    let missing = create_pg(&env, "pg-missing", Some(PASSWORD)).await;
    let undecryptable = create_pg(&env, "pg-undecryptable", Some(PASSWORD)).await;
    let invalid = create_pg(&env, "pg-invalid", Some(PASSWORD)).await;
    for (name, db) in [
        ("g-ok", readable),
        ("g-missing", missing),
        ("g-undec", undecryptable),
        ("g-invalid", invalid),
    ] {
        let (status, body) = admin_post(
            &env.router,
            "/api/sink/groups",
            &env.admin_token,
            json!({ "name": name, "dbConnectionId": db, "mode": "interval", "intervalMs": 1000,
                    "tableName": "public.t", "storeBad": false, "enabled": true,
                    "tagIds": [tag["id"]] }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
    }
    drop_secret_row(&env.pool, missing).await;
    make_undecryptable(&env.pool, undecryptable).await;
    make_malformed(&env.pool, invalid).await;

    let (status, body) = v1_get(&env.router, &env.admin_token, "/api/sink/config").await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let connections = body["connections"].as_array().unwrap();
    assert_eq!(connections.len(), 1, "{body:?}");
    assert_eq!(connections[0]["id"], json!(readable));
    assert_eq!(connections[0]["password"], json!(PASSWORD));
    assert_eq!(
        body["unavailableConnections"],
        json!([
            { "id": missing, "name": "pg-missing", "reason": "missing" },
            { "id": undecryptable, "name": "pg-undecryptable", "reason": "undecryptable" },
            { "id": invalid, "name": "pg-invalid", "reason": "invalid" },
        ])
    );
    // The groups are still listed (the sidecar shows why it does not write them).
    assert_eq!(body["groups"].as_array().unwrap().len(), 4);
}

/// §5.4: `GET /api/ingest/bridge-config` は、読み先のパスワードを読めない source を
/// `sources` に載せず `unavailableSources` に出す（`ingestAllowed` のキーのときだけ開封
/// するので、スコープの無いキーでは今どおり `password: null` で載る）。
#[tokio::test]
async fn bridge_config_lists_sources_with_an_unreadable_password_apart() {
    for reason in ["missing", "invalid"] {
        bridge_config_lists_a_source_apart(reason).await;
    }
}

async fn bridge_config_lists_a_source_apart(reason: &str) {
    let env = test_env().await;
    let pg = create_pg(&env, "erp-db", Some(PASSWORD)).await;
    let bridge = json!({ "stale_timeout_ms": 4000, "bridge": { "kind": "db",
        "db_connection_id": pg, "queries": [ { "name": "line1", "period_ms": 1000,
        "sql": "SELECT temp FROM t", "columns": [ { "column": "temp", "key": "line1.temp" } ] } ] } });
    let (status, push) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "name": "Bridge1", "protocol": "push", "settings": bridge }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{push:?}");
    let push_id = push["id"].as_i64().unwrap();
    if reason == "missing" {
        drop_secret_row(&env.pool, pg).await;
    } else {
        make_malformed(&env.pool, pg).await;
    }

    let scope = format!("ingest:#{push_id}");
    let (_, issued) = issue_api_key(
        &env.router,
        &env.admin_token,
        "bridge",
        &["admin", "read", scope.as_str()],
    )
    .await;
    let key = issued["key"].as_str().unwrap();
    let (status, body) = v1_get(&env.router, key, "/api/ingest/bridge-config").await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["sources"], json!([]), "{body:?}");
    assert_eq!(
        body["unavailableSources"],
        json!([{ "connectionId": push_id, "name": "Bridge1", "dbConnectionId": pg,
                 "reason": reason }])
    );
    // Without the ingest scope nothing is opened: listed as before, no password.
    let (status, body) = v1_get(&env.router, &env.admin_token, "/api/ingest/bridge-config").await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["sources"].as_array().unwrap().len(), 1);
    assert!(body["sources"][0]["db"]["password"].is_null());
    assert_eq!(body["unavailableSources"], json!([]));
}

/// R2b-6b（設計 §5）: 既定のままの `push` 接続の `settings` は `{}`。未適用キューの指紋は
/// 応答の形（秘密のキーは `{"$secret": "set"}`）で、パスワードが読めるかどうかでは変わらない
/// （読めない PC へ移しても、積んだ提案が衝突にならない）。値も参照も入らない。
#[tokio::test]
async fn push_defaults_read_as_empty_and_fingerprints_ignore_readability() {
    let env = test_env().await;
    let (status, push) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "name": "P1", "protocol": "push" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{push:?}");
    assert_eq!(push["settings"], json!({}), "{push:?}");
    let pg = create_pg(&env, "pg", Some(PASSWORD)).await;

    let services = (
        PlcConnectionService::new(env.pool.clone()),
        CollectionGroupService::new(env.pool.clone()),
    );
    let fingerprint = |id: i64| {
        let services = &services;
        async move {
            compute_pending_base_fingerprint(
                &services.0,
                &services.1,
                "plc_connections.update",
                &json!({ "id": id }),
            )
            .await
            .unwrap()
        }
    };
    let before = fingerprint(pg).await;
    assert!(before.contains("\"$secret\":\"set\""), "{before}");
    assert!(!before.contains(PASSWORD), "{before}");
    drop_secret_row(&env.pool, pg).await;
    assert_eq!(fingerprint(pg).await, before);
}
