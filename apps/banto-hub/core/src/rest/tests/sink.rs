use super::super::*;
use super::common::{
    admin_delete, admin_get, admin_post, admin_put, issue_api_key, test_env, test_env_with_clock,
    v1_get, v1_put,
};
use banto_tstore::ManualClock;

// --- 外部 DB 連携 S4（docs/banto-hub-external-db-design.md §5.2・§6-13・
// §6-15）: DB Sink の Hub 側（`hub_sink_groups` CRUD、`GET
// /api/sink/config`、`PUT /api/sink/status`、`GET /api/status`の
// `sink`節） ------------------------------------------------------------

/// PLC 接続1つ+収集グループ+タグ1つ、および postgres 接続1つを作る -
/// sink group のテストが共通に必要とするフィクスチャ
/// （`seed_scope_fixture`と同じ「admin REST 経由で組み立てる」方針）。
/// 戻り値は `(postgres 接続の id, タグの id)`。
async fn seed_sink_fixture(router: &Router, admin_token: &str) -> (i64, i64) {
    let (status, conn) = admin_post(
        router,
        "/api/plc-connections",
        admin_token,
        json!({ "protocol": "modbus-tcp", "name": "line1", "settings": { "host": "127.0.0.1", "port": 15201 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");
    let (status, group) = admin_post(
        router,
        "/api/collection-groups",
        admin_token,
        json!({ "name": "fast", "plcConnectionId": conn["id"], "periodMs": 100 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{group:?}");
    let (status, tag) = admin_post(
        router,
        "/api/tags",
        admin_token,
        json!({
            "name": "temp01",
            "collectionGroupId": group["id"],
            "address": "40001",
            "dataType": "i16",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{tag:?}");

    let (status, pg) = admin_post(
        router,
        "/api/plc-connections",
        admin_token,
        json!({ "name": "erp-db", "protocol": "postgres", "settings": { "host": "10.0.0.50", "port": 5432, "database": "erp", "username": "reader", "password": "s3cret" } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{pg:?}");

    (pg["id"].as_i64().unwrap(), tag["id"].as_i64().unwrap())
}

fn sink_group_body(db_connection_id: i64, tag_id: i64) -> serde_json::Value {
    json!({
        "name": "line1-log",
        "dbConnectionId": db_connection_id,
        "mode": "interval",
        "intervalMs": 1000,
        "tableName": "public.tag_history",
        "storeBad": false,
        "enabled": true,
        "tagIds": [tag_id],
    })
}

#[tokio::test]
async fn sink_groups_crud_happy_path() {
    let env = test_env().await;
    let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;

    let (status, created) = admin_post(
        &env.router,
        "/api/sink/groups",
        &env.admin_token,
        sink_group_body(db_connection_id, tag_id),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created:?}");
    assert_eq!(created["name"], "line1-log");
    assert_eq!(created["dbConnectionId"], db_connection_id);
    assert_eq!(created["tagIds"], json!([tag_id]));
    let id = created["id"].as_i64().unwrap();

    let (status, listed) = admin_get(&env.router, "/api/sink/groups", &env.admin_token).await;
    assert_eq!(status, StatusCode::OK, "{listed:?}");
    assert_eq!(listed.as_array().unwrap().len(), 1);

    let (status, fetched) = admin_get(
        &env.router,
        &format!("/api/sink/groups/{id}"),
        &env.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{fetched:?}");
    assert_eq!(fetched, created);

    let mut updated_body = sink_group_body(db_connection_id, tag_id);
    updated_body["intervalMs"] = json!(2000);
    updated_body["enabled"] = json!(false);
    let (status, updated) = admin_put(
        &env.router,
        &format!("/api/sink/groups/{id}"),
        &env.admin_token,
        updated_body,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated:?}");
    assert_eq!(updated["intervalMs"], 2000);
    assert_eq!(updated["enabled"], false);

    let (status, _) = admin_delete(
        &env.router,
        &format!("/api/sink/groups/{id}"),
        &env.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = admin_get(
        &env.router,
        &format!("/api/sink/groups/{id}"),
        &env.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn sink_groups_create_requires_editor() {
    let env = test_env().await;
    let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
    let (status, body) = admin_post(
        &env.router,
        "/api/sink/groups",
        &env.viewer_token,
        sink_group_body(db_connection_id, tag_id),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body:?}");
}

#[tokio::test]
async fn sink_groups_create_rejects_empty_name() {
    let env = test_env().await;
    let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
    let mut body = sink_group_body(db_connection_id, tag_id);
    body["name"] = json!("   ");
    let (status, body) = admin_post(&env.router, "/api/sink/groups", &env.admin_token, body).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
}

#[tokio::test]
async fn sink_groups_create_rejects_too_long_name() {
    let env = test_env().await;
    let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
    let mut body = sink_group_body(db_connection_id, tag_id);
    body["name"] = json!("x".repeat(65));
    let (status, body) = admin_post(&env.router, "/api/sink/groups", &env.admin_token, body).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
}

#[tokio::test]
async fn sink_groups_create_rejects_duplicate_name() {
    let env = test_env().await;
    let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
    let (status, _) = admin_post(
        &env.router,
        "/api/sink/groups",
        &env.admin_token,
        sink_group_body(db_connection_id, tag_id),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = admin_post(
        &env.router,
        "/api/sink/groups",
        &env.admin_token,
        sink_group_body(db_connection_id, tag_id),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
}

#[tokio::test]
async fn sink_groups_create_rejects_bad_mode() {
    let env = test_env().await;
    let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
    let mut body = sink_group_body(db_connection_id, tag_id);
    body["mode"] = json!("hourly");
    let (status, body) = admin_post(&env.router, "/api/sink/groups", &env.admin_token, body).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
}

#[tokio::test]
async fn sink_groups_create_rejects_out_of_range_interval_ms() {
    let env = test_env().await;
    let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
    let mut body = sink_group_body(db_connection_id, tag_id);
    body["intervalMs"] = json!(10);
    let (status, body) = admin_post(&env.router, "/api/sink/groups", &env.admin_token, body).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
}

#[tokio::test]
async fn sink_groups_create_rejects_invalid_table_name() {
    let env = test_env().await;
    let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
    let mut body = sink_group_body(db_connection_id, tag_id);
    body["tableName"] = json!("\"quoted\"");
    let (status, body) = admin_post(&env.router, "/api/sink/groups", &env.admin_token, body).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
}

#[tokio::test]
async fn sink_groups_create_rejects_empty_tag_ids() {
    let env = test_env().await;
    let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
    let _ = tag_id;
    let mut body = sink_group_body(db_connection_id, tag_id);
    body["tagIds"] = json!([]);
    let (status, body) = admin_post(&env.router, "/api/sink/groups", &env.admin_token, body).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
}

#[tokio::test]
async fn sink_groups_create_rejects_duplicate_tag_ids() {
    let env = test_env().await;
    let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
    let mut body = sink_group_body(db_connection_id, tag_id);
    body["tagIds"] = json!([tag_id, tag_id]);
    let (status, body) = admin_post(&env.router, "/api/sink/groups", &env.admin_token, body).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
}

#[tokio::test]
async fn sink_groups_create_rejects_unknown_tag_id() {
    let env = test_env().await;
    let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
    let mut body = sink_group_body(db_connection_id, tag_id);
    body["tagIds"] = json!([999_999]);
    let (status, body) = admin_post(&env.router, "/api/sink/groups", &env.admin_token, body).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
}

/// 接続が `protocol: postgres` でなければ422（実装指示: 「connection
/// that is not postgres → 422」）。
#[tokio::test]
async fn sink_groups_create_rejects_non_postgres_connection() {
    let env = test_env().await;
    let (_db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
    let (status, plc) = admin_get(&env.router, "/api/plc-connections", &env.admin_token).await;
    assert_eq!(status, StatusCode::OK, "{plc:?}");
    let modbus_connection_id = plc
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["protocol"] == "modbus-tcp")
        .unwrap()["id"]
        .as_i64()
        .unwrap();

    let mut body = sink_group_body(modbus_connection_id, tag_id);
    body["dbConnectionId"] = json!(modbus_connection_id);
    let (status, body) = admin_post(&env.router, "/api/sink/groups", &env.admin_token, body).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
}

#[tokio::test]
async fn sink_groups_create_rejects_unknown_connection() {
    let env = test_env().await;
    let (_db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
    let body = sink_group_body(999_999, tag_id);
    let (status, body) = admin_post(&env.router, "/api/sink/groups", &env.admin_token, body).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
}

/// この接続を参照する sink group がある間は `plc_connections` の削除を
/// 拒否する（RESTRICT 相当、実装指示: 「connection that a sink group
/// references is rejected with a clear message」）。
#[tokio::test]
async fn deleting_a_connection_referenced_by_a_sink_group_is_rejected() {
    let env = test_env().await;
    let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
    let (status, _) = admin_post(
        &env.router,
        "/api/sink/groups",
        &env.admin_token,
        sink_group_body(db_connection_id, tag_id),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = admin_delete(
        &env.router,
        &format!("/api/plc-connections/{db_connection_id}"),
        &env.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    let message = body["field_errors"][0]["message"].as_str().unwrap_or("");
    assert!(
        message.contains("sink group"),
        "expected the message to mention sink group: {body:?}"
    );

    // 実際にまだ存在すること。
    PlcConnectionService::new(env.pool.clone())
        .get(db_connection_id)
        .await
        .expect("connection should survive");
}

/// 対象タグを削除すると、`hub_sink_group_tags`の対応する行も消える
/// （`crate::sink`のモジュール doc comment「タグ削除の経路では能動的に
/// 消す」）。
#[tokio::test]
async fn deleting_a_tag_removes_it_from_sink_group_membership() {
    let env = test_env().await;
    let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
    let (status, created) = admin_post(
        &env.router,
        "/api/sink/groups",
        &env.admin_token,
        sink_group_body(db_connection_id, tag_id),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created:?}");
    let group_id = created["id"].as_i64().unwrap();

    let (status, _) = admin_delete(
        &env.router,
        &format!("/api/tags/{tag_id}"),
        &env.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, fetched) = admin_get(
        &env.router,
        &format!("/api/sink/groups/{group_id}"),
        &env.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{fetched:?}");
    assert_eq!(fetched["tagIds"], json!([]));
}

/// `GET /api/sink/config` は admin 以外 403、admin なら200でパスワード
/// を含み、タグは stable id（connectionId/groupId/tagId）を運ぶ
/// （実装指示）。
#[tokio::test]
async fn sink_config_get_requires_admin_and_carries_password_and_stable_ids() {
    let env = test_env().await;
    let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
    let (status, _) = admin_post(
        &env.router,
        "/api/sink/groups",
        &env.admin_token,
        sink_group_body(db_connection_id, tag_id),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = v1_get(&env.router, &env.viewer_token, "/api/sink/config").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body:?}");

    let (status, body) = v1_get(&env.router, &env.admin_token, "/api/sink/config").await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert!(body["generatedAt"].as_i64().is_some());
    let groups = body["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 1);
    let tags = groups[0]["tags"].as_array().unwrap();
    assert_eq!(tags.len(), 1);
    assert!(tags[0]["connectionId"].as_i64().is_some());
    assert!(tags[0]["groupId"].as_i64().is_some());
    assert_eq!(tags[0]["tagId"], tag_id);
    assert!(tags[0]["externalName"].as_str().unwrap().contains("temp01"));

    let connections = body["connections"].as_array().unwrap();
    assert_eq!(connections.len(), 1);
    assert_eq!(connections[0]["id"], db_connection_id);
    assert_eq!(connections[0]["password"], "s3cret");

    // admin スコープを持つ API キーでも通ること(サイドカーの本来の
    // 経路)。admin スコープ無しのキーは403。
    let (status, issued) =
        issue_api_key(&env.router, &env.admin_token, "sidecar", &["admin"]).await;
    assert_eq!(status, StatusCode::CREATED, "{issued:?}");
    let admin_key = issued["key"].as_str().unwrap();
    let (status, body) = v1_get(&env.router, admin_key, "/api/sink/config").await;
    assert_eq!(status, StatusCode::OK, "{body:?}");

    let (status, issued) =
        issue_api_key(&env.router, &env.admin_token, "reader-only", &["read"]).await;
    assert_eq!(status, StatusCode::CREATED, "{issued:?}");
    let read_key = issued["key"].as_str().unwrap();
    let (status, body) = v1_get(&env.router, read_key, "/api/sink/config").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body:?}");
}

/// R2a-3b（docs/banto-hub-ingest-design.md §10.2、§11 のオーナーの監査 (5)）:
/// `GET /api/ingest/bridge-config` のゲートは `/api/sink/config` と同じ（admin スコープの
/// キーか admin ロールのセッション）。`ingest:` だけのキー・`read` だけのキー・viewer は
/// 断る。パスワードは呼び出し手のキーがその接続の `ingest:#{id}` を持つときだけ返し、
/// 無効な `push` 接続は載せない。
#[tokio::test]
async fn bridge_config_requires_admin_and_returns_the_password_only_with_the_ingest_scope() {
    let env = test_env().await;
    let (status, pg) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "name": "erp-db", "protocol": "postgres", "settings": { "host": "10.0.0.50", "port": 5432, "database": "erp", "username": "reader", "password": "s3cret" } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{pg:?}");
    let bridge_settings = json!({ "stale_timeout_ms": 4000, "bridge": { "kind": "db",
        "db_connection_id": pg["id"], "queries": [ { "name": "line1", "period_ms": 1000,
        "sql": "SELECT temp FROM t", "columns": [ { "column": "temp", "key": "line1.temp" } ] } ] } });
    let mut push_ids = Vec::new();
    for (name, enabled) in [("Bridge1", true), ("Bridge2", false)] {
        let (status, push) = admin_post(
            &env.router,
            "/api/plc-connections",
            &env.admin_token,
            json!({ "name": name, "protocol": "push", "enabled": enabled, "settings": bridge_settings }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{push:?}");
        push_ids.push(push["id"].as_i64().unwrap());
    }
    let bridge = push_ids[0];
    let path = "/api/ingest/bridge-config";

    let (status, _) = v1_get(&env.router, "", path).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, body) = v1_get(&env.router, &env.viewer_token, path).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body:?}");
    let ingest_scope = format!("ingest:#{bridge}");
    for (name, scopes) in [
        ("read-only", vec!["read"]),
        ("ingest-only", vec![ingest_scope.as_str()]),
    ] {
        let (status, issued) = issue_api_key(&env.router, &env.admin_token, name, &scopes).await;
        assert_eq!(status, StatusCode::CREATED, "{issued:?}");
        let (status, body) = v1_get(&env.router, issued["key"].as_str().unwrap(), path).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{scopes:?}: {body:?}");
    }

    // admin ロールのセッション（動作確認用）: 通るが、送れないのでパスワードは無い。
    let (status, body) = v1_get(&env.router, &env.admin_token, path).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let sources = body["sources"].as_array().unwrap();
    assert_eq!(sources.len(), 1, "disabled push connections are not listed");
    assert_eq!(sources[0]["connectionId"], json!(bridge));
    assert_eq!(sources[0]["ingestAllowed"], json!(false));
    assert!(sources[0]["db"]["password"].is_null());
    assert!(!body.to_string().contains("s3cret"));

    // admin だけのキー: 同じ。admin + ingest:#{id}: パスワードを返す（§10.2 の形）。
    let (_, issued) = issue_api_key(&env.router, &env.admin_token, "sink", &["admin"]).await;
    let (status, body) = v1_get(&env.router, issued["key"].as_str().unwrap(), path).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["sources"][0]["ingestAllowed"], json!(false));
    assert!(body["sources"][0]["db"]["password"].is_null());
    let (_, issued) = issue_api_key(
        &env.router,
        &env.admin_token,
        "bridge",
        &["admin", "read", ingest_scope.as_str()],
    )
    .await;
    let (status, body) = v1_get(&env.router, issued["key"].as_str().unwrap(), path).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let source = &body["sources"][0];
    assert_eq!(source["ingestAllowed"], json!(true));
    assert_eq!(source["name"], json!("Bridge1"));
    assert_eq!(source["staleTimeoutMs"], json!(4000));
    assert!(source["configFingerprint"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
    assert_eq!(
        source["queries"][0]["columns"][0]["key"],
        json!("line1.temp")
    );
    assert_eq!(source["db"]["id"], pg["id"]);
    assert_eq!(source["db"]["enabled"], json!(true));
    assert_eq!(source["db"]["host"], json!("10.0.0.50"));
    assert_eq!(source["db"]["password"], json!("s3cret"));
}

/// 無効な sink group のみの構成では`groups`/`connections`とも空配列。
#[tokio::test]
async fn sink_config_get_excludes_disabled_groups() {
    let env = test_env().await;
    let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
    let mut body = sink_group_body(db_connection_id, tag_id);
    body["enabled"] = json!(false);
    let (status, _) = admin_post(&env.router, "/api/sink/groups", &env.admin_token, body).await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = v1_get(&env.router, &env.admin_token, "/api/sink/config").await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["groups"].as_array().unwrap().len(), 0);
    assert_eq!(body["connections"].as_array().unwrap().len(), 0);
}

fn sink_status_push_body(group_id: i64) -> serde_json::Value {
    json!({
        "groups": [{
            "id": group_id,
            "state": "running",
            "queued": 3,
            "dropped": 0,
            "lastFlushAt": 1_000,
            "lastError": null,
        }],
    })
}

#[tokio::test]
async fn sink_status_put_requires_admin() {
    let env = test_env().await;
    let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
    let (status, created) = admin_post(
        &env.router,
        "/api/sink/groups",
        &env.admin_token,
        sink_group_body(db_connection_id, tag_id),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let group_id = created["id"].as_i64().unwrap();

    let (status, body) = v1_put(
        &env.router,
        &env.viewer_token,
        "/api/sink/status",
        sink_status_push_body(group_id),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body:?}");
}

#[tokio::test]
async fn sink_status_put_rejects_unknown_group_id() {
    let env = test_env().await;
    let (status, body) = v1_put(
        &env.router,
        &env.admin_token,
        "/api/sink/status",
        sink_status_push_body(999_999),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    let message = body["field_errors"][0]["message"].as_str().unwrap_or("");
    assert!(message.contains("999999"), "{body:?}");
}

/// push 後は `GET /api/status` の `sink.sidecar.state == "online"` と
/// 対応するグループ行が見え、15秒以上経過すると`"unknown"`に戻る
/// （実装指示 - `ManualClock`で決定的に検証する）。
#[tokio::test]
async fn sink_status_push_then_status_shows_online_then_unknown_after_stale_window() {
    let now_ms = 1_700_000_000_000i64;
    let clock = Arc::new(ManualClock::new(now_ms, 0));
    let env = test_env_with_clock(clock.clone()).await;
    let (db_connection_id, tag_id) = seed_sink_fixture(&env.router, &env.admin_token).await;
    let (status, created) = admin_post(
        &env.router,
        "/api/sink/groups",
        &env.admin_token,
        sink_group_body(db_connection_id, tag_id),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let group_id = created["id"].as_i64().unwrap();

    let (status, _) = v1_put(
        &env.router,
        &env.admin_token,
        "/api/sink/status",
        sink_status_push_body(group_id),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, admin_status_body) = admin_get(&env.router, "/api/status", &env.admin_token).await;
    assert_eq!(status, StatusCode::OK, "{admin_status_body:?}");
    assert_eq!(admin_status_body["sink"]["sidecar"]["state"], "online");
    let groups = admin_status_body["sink"]["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0]["id"], group_id);
    assert_eq!(groups[0]["queued"], 3);

    clock.advance_ms(crate::sink::SINK_SIDECAR_STALE_AFTER_MS);
    let (status, admin_status_body) = admin_get(&env.router, "/api/status", &env.admin_token).await;
    assert_eq!(status, StatusCode::OK, "{admin_status_body:?}");
    assert_eq!(admin_status_body["sink"]["sidecar"]["state"], "unknown");
}
