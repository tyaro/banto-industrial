use super::super::*;
use super::common::{admin_delete, admin_post, test_env};

// --- T19 S2-b (UX-38, docs/banto-hub-t19-design.md §3.4/§7.5,
// 2026-09-02 オーナー決定「タグがあってもグループ・接続を削除可。定義の
// み削除し、履歴は残す」) --------------------------------------------

/// REST 経由の主張: `DELETE /api/plc-connections/{id}` は、配下に収集
/// グループ・タグがあっても拒否せず、まとめて削除する
/// （収集停止中は即時 - `plc_connections_delete` が
/// `cascade_delete_tx` を呼ぶ経路）。
#[tokio::test]
async fn plc_connections_delete_cascades_when_it_has_groups_and_tags() {
    let env = test_env().await;

    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "name": "line-cascade", "host": "127.0.0.1", "port": 15060 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");
    let conn_id = conn["id"].as_i64().unwrap();

    let (status, group) = admin_post(
        &env.router,
        "/api/collection-groups",
        &env.admin_token,
        json!({ "name": "g-cascade", "plcConnectionId": conn_id, "periodMs": 1000 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{group:?}");
    let group_id = group["id"].as_i64().unwrap();

    let (status, tag) = admin_post(
        &env.router,
        "/api/tags",
        &env.admin_token,
        json!({
            "name": "t-cascade",
            "collectionGroupId": group_id,
            "address": "40001",
            "dataType": "i16"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{tag:?}");
    let tag_id = tag["id"].as_i64().unwrap();

    // 削除前: 拒否ではなく成功する（旧仕様なら 422 Validation だった）。
    let (status, _body) = admin_delete(
        &env.router,
        &format!("/api/plc-connections/{conn_id}"),
        &env.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

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

/// `DELETE /api/collection-groups/{id}` の同じ主張: タグがあっても
/// 拒否せず、まとめて削除する。
#[tokio::test]
async fn collection_groups_delete_cascades_when_it_has_tags() {
    let env = test_env().await;

    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "name": "line-cascade-2", "host": "127.0.0.1", "port": 15061 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");
    let conn_id = conn["id"].as_i64().unwrap();

    let (status, group) = admin_post(
        &env.router,
        "/api/collection-groups",
        &env.admin_token,
        json!({ "name": "g-cascade-2", "plcConnectionId": conn_id, "periodMs": 1000 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{group:?}");
    let group_id = group["id"].as_i64().unwrap();

    let (status, tag) = admin_post(
        &env.router,
        "/api/tags",
        &env.admin_token,
        json!({
            "name": "t-cascade-2",
            "collectionGroupId": group_id,
            "address": "40001",
            "dataType": "i16"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{tag:?}");
    let tag_id = tag["id"].as_i64().unwrap();

    let (status, _body) = admin_delete(
        &env.router,
        &format!("/api/collection-groups/{group_id}"),
        &env.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

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
    // The connection itself is untouched by a group-level delete.
    PlcConnectionService::new(env.pool.clone())
        .get(conn_id)
        .await
        .expect("the connection must survive a group delete");
}

/// UX-38 の対象外: 予約接続（calc/mem）はカスケード経路でも削除できない
/// （§7.5「virtual 接続（calc/mem）の削除・編集禁止はそのまま維持
/// する」）。
#[tokio::test]
async fn plc_connections_delete_still_refuses_a_virtual_connection() {
    let env = test_env().await;
    let calc = PlcConnectionService::new(env.pool.clone())
        .create(banto_tags::PlcConnectionInput {
            name: banto_tags::CALC_CONNECTION_NAME.to_string(),
            protocol: banto_tags::VIRTUAL_PROTOCOL.to_string(),
            host: String::new(),
            port: 0,
            unit_id: 1,
            enabled: true,
            simulation: false,
            word_order: "low_high".to_string(),
            cpu_type: None,
            database: None,
            username: None,
            secrets: Default::default(),
            settings: None,
        })
        .await
        .unwrap();

    let (status, body) = admin_delete(
        &env.router,
        &format!("/api/plc-connections/{}", calc.id),
        &env.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");

    PlcConnectionService::new(env.pool.clone())
        .get(calc.id)
        .await
        .expect("calc should survive");
}
