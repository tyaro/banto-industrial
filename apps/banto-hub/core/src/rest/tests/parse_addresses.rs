use super::super::*;
use super::common::{admin_post, test_env, TestEnv, CLIENT_HEADER};
use axum::body::Body;
use axum::http::Request as HttpRequest;
use tower::ServiceExt;

// --- R2b-2d: POST /api/tags/parse-addresses ------------------------------
//
// 保存と同じ関数（`banto_tags::parse_tag_address` → `normalize_tag_address`）に、
// 保存の入口と同じドライバのレジストリ（`CollectorManager::registry()`）を渡した
// 結果を返す口の固定（設計 docs/banto-hub-driver-address-design.md §7.1）。
// テストの環境のレジストリは `test_support::test_drivers()` の代役のドライバ
// （`banto_plc` の文法を持つ）。

/// 接続を作って id を返す（`extra` は `name`・`protocol` 以外の項目）。
async fn create_connection(
    env: &TestEnv,
    name: &str,
    protocol: &str,
    extra: serde_json::Value,
) -> i64 {
    let mut body =
        json!({ "name": name, "protocol": protocol, "host": "127.0.0.1", "port": 15300 });
    for (key, value) in extra.as_object().into_iter().flatten() {
        body[key] = value.clone();
    }
    let (status, conn) =
        admin_post(&env.router, "/api/plc-connections", &env.admin_token, body).await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");
    conn["id"].as_i64().unwrap()
}

async fn modbus_connection(env: &TestEnv) -> i64 {
    create_connection(env, "plc1", "modbus-tcp", json!({})).await
}

async fn postgres_connection(env: &TestEnv) -> i64 {
    create_connection(
        env,
        "erp-db",
        "postgres",
        json!({ "host": "10.0.0.50", "port": 5432, "database": "erp", "username": "reader", "password": "s3cret" }),
    )
    .await
}

fn item(tag_kind: &str, address: &str, data_type: &str) -> serde_json::Value {
    json!({ "tagKind": tag_kind, "address": address, "dataType": data_type, "stringLength": null, "stringEncoding": "utf8" })
}

async fn parse(
    env: &TestEnv,
    token: &str,
    connection_id: i64,
    items: Vec<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    admin_post(
        &env.router,
        "/api/tags/parse-addresses",
        token,
        json!({ "connectionId": connection_id, "items": items }),
    )
    .await
}

#[tokio::test]
async fn plc_address_is_normalized_with_area_info() {
    let env = test_env().await;
    let conn = modbus_connection(&env).await;
    let (status, body) = parse(
        &env,
        &env.admin_token,
        conn,
        vec![
            item("plc", "40001", "u16"),
            item("plc", "30001", "u16"),
            item("plc", "40001.3", "bit"),
            item("plc", "zzz", "u16"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 4, "{body:?}");
    assert_eq!(items[0]["ok"], true, "{body:?}");
    assert_eq!(items[0]["canonical"], "400001");
    assert_eq!(items[0]["area"], "holding_register");
    assert_eq!(items[0]["writableArea"], true);
    assert!(items[0]["bit"].is_null());
    assert_eq!(items[0]["wordCount"], 1);
    // 読み取り専用の領域: 判定は `writableArea: false`（`ok` は true）。
    assert_eq!(items[1]["ok"], true, "{body:?}");
    assert_eq!(items[1]["canonical"], "300001");
    assert_eq!(items[1]["writableArea"], false);
    assert_eq!(items[2]["canonical"], "400001.3");
    assert_eq!(items[2]["bit"], 3);
    assert_eq!(items[3]["ok"], false, "{body:?}");
    assert!(items[3]["message"].as_str().is_some_and(|m| !m.is_empty()));
    assert!(items[3].get("canonical").is_none());
}

#[tokio::test]
async fn db_computed_and_internal_follow_the_per_kind_rules() {
    let env = test_env().await;
    let pg = postgres_connection(&env).await;
    let (status, body) = parse(
        &env,
        &env.admin_token,
        pg,
        vec![
            item("db", "  temperature ", "f64"),
            item("db", "1bad", "f64"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let items = body["items"].as_array().unwrap();
    assert_eq!(items[0]["ok"], true, "{body:?}");
    assert_eq!(items[0]["canonical"], "temperature");
    for key in ["area", "writableArea", "bit", "wordCount"] {
        assert!(items[0][key].is_null(), "{key}: {body:?}");
    }
    assert_eq!(items[1]["ok"], false, "{body:?}");

    let calc = create_connection(&env, "calc", "virtual", json!({})).await;
    let (_, body) = parse(
        &env,
        &env.admin_token,
        calc,
        vec![item("computed", "", "f64"), item("computed", "x", "f64")],
    )
    .await;
    let items = body["items"].as_array().unwrap();
    assert_eq!(items[0]["ok"], true, "{body:?}");
    assert_eq!(items[0]["canonical"], "");
    assert_eq!(items[1]["ok"], false, "{body:?}");

    let mem = create_connection(&env, "mem", "virtual", json!({})).await;
    let (_, body) = parse(
        &env,
        &env.admin_token,
        mem,
        vec![item("internal", "", "f64"), item("internal", "x", "f64")],
    )
    .await;
    let items = body["items"].as_array().unwrap();
    assert_eq!(items[0]["ok"], true, "{body:?}");
    assert_eq!(items[0]["canonical"], "");
    assert_eq!(items[1]["ok"], false, "{body:?}");
}

/// 配置の誤り（`postgres` の下の `plc`）の文言は保存（`POST /api/tags`）と同じ。
#[tokio::test]
async fn wrong_placement_gives_the_same_message_as_save() {
    let env = test_env().await;
    let pg = postgres_connection(&env).await;
    let (_, body) = parse(
        &env,
        &env.admin_token,
        pg,
        vec![item("plc", "40001", "u16")],
    )
    .await;
    let message = body["items"][0]["message"].as_str().unwrap().to_string();
    assert_eq!(body["items"][0]["ok"], false, "{body:?}");

    // postgres 接続の下にはグループを作れないので、保存の入口が呼ぶ関数
    // （`normalize_tag_address`）を同じレジストリで直接呼んで文言を比べる。
    let conn = env.manager.plc_connections().get(pg).await.unwrap();
    let input: TagInput = serde_json::from_value(json!({
        "name": "t", "collection_group_id": 1, "address": "40001", "data_type": "u16", "tag_kind": "plc"
    }))
    .unwrap();
    let saved = banto_tags::normalize_tag_address(
        &conn.name,
        &conn.protocol,
        &input,
        env.manager.registry(),
    )
    .unwrap_err();
    assert_eq!(saved[0].message, message);
}

/// 項目の検証が先: 不正な `dataType` は、アドレスの綴りが何であれ `ok: false`
/// （`normalize_tag_address` 単体は種類を作れないとき綴りを返すだけ）。
#[tokio::test]
async fn invalid_data_type_is_refused_before_the_address() {
    let env = test_env().await;
    let conn = modbus_connection(&env).await;
    let (status, body) = parse(
        &env,
        &env.admin_token,
        conn,
        vec![
            item("plc", "40001", "nope"),
            json!({ "tagKind": "plc", "address": "40001", "dataType": "string", "stringLength": null }),
            json!({ "tagKind": "plc", "address": "40001", "dataType": "u16", "stringEncoding": "klingon" }),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    for i in 0..3 {
        assert_eq!(body["items"][i]["ok"], false, "{i}: {body:?}");
    }
}

#[tokio::test]
async fn unknown_connection_is_not_found() {
    let env = test_env().await;
    let (status, body) = parse(
        &env,
        &env.admin_token,
        9999,
        vec![item("plc", "40001", "u16")],
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body:?}");
}

#[tokio::test]
async fn too_many_items_is_a_validation_error() {
    let env = test_env().await;
    let conn = modbus_connection(&env).await;
    let items = vec![item("plc", "40001", "u16"); 10_001];
    let (status, body) = parse(&env, &env.admin_token, conn, items).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
}

#[tokio::test]
async fn missing_tag_kind_is_a_client_error() {
    let env = test_env().await;
    let conn = modbus_connection(&env).await;
    let (status, body) = admin_post(
        &env.router,
        "/api/tags/parse-addresses",
        &env.admin_token,
        json!({ "connectionId": conn, "items": [ { "address": "40001", "dataType": "u16" } ] }),
    )
    .await;
    assert!(status.is_client_error(), "{status} {body:?}");
}

#[tokio::test]
async fn viewer_may_call_and_anonymous_may_not() {
    let env = test_env().await;
    let conn = modbus_connection(&env).await;
    let (status, body) = parse(
        &env,
        &env.viewer_token,
        conn,
        vec![item("plc", "40001", "u16")],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["items"][0]["canonical"], "400001");

    let response = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/tags/parse-addresses")
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "connectionId": conn, "items": [] }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

/// 保存と同じ結果: 同じ入力を、レジストリつきのタグサービス（本番の入口
/// `CollectorManager::tags()` と同じ組み立て）で保存した行の `address` が、
/// API の `canonical` と一致する（`normalize_tag_address` を共有していること）。
#[tokio::test]
async fn api_canonical_equals_what_saving_stores() {
    let env = test_env().await;
    let conn = modbus_connection(&env).await;
    let (status, group) = admin_post(
        &env.router,
        "/api/collection-groups",
        &env.admin_token,
        json!({ "name": "g", "plcConnectionId": conn, "periodMs": 1000 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{group:?}");
    let group_id = group["id"].as_i64().unwrap();
    let tags = env.manager.tags();
    for (n, address) in ["40001", " 400002 ", "40003.5"].into_iter().enumerate() {
        let data_type = if address.contains('.') { "bit" } else { "u16" };
        let (_, body) = parse(
            &env,
            &env.admin_token,
            conn,
            vec![item("plc", address, data_type)],
        )
        .await;
        let input: TagInput = serde_json::from_value(json!({
            "name": format!("t{n}"), "collection_group_id": group_id, "address": address,
            "data_type": data_type, "tag_kind": "plc"
        }))
        .unwrap();
        let saved = tags.create(input).await.unwrap();
        assert_eq!(body["items"][0]["canonical"], saved.address, "{body:?}");
    }
}
