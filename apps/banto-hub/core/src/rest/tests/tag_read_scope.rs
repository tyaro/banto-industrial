use super::super::*;
use super::common::{
    admin_get, admin_post, issue_api_key, seed_scope_fixture, test_env, test_env_unlocked, v1_get,
    CLIENT_HEADER,
};
use crate::pending_changes::PendingChangeState;
use axum::body::Body;
use axum::http::Request as HttpRequest;
use tower::ServiceExt;

// --- H10 ③: per-tag read スコープ(Option B、
// docs/h10-3-read-scope-proposal.md §5・§6) ------------------------------

/// S1/S3/S4(案 B): `read:line1.fast.temp01` キーは catalog は全タグ
/// (line2 のタグ含む)を見られるが、値は自分のタグしか読めない - 単一は
/// 403、バルクは黙って除外、`?tags=` 明示指定は 403。
#[tokio::test]
async fn exact_read_scope_key_sees_full_catalog_but_only_its_own_tag_value() {
    let env = test_env().await;
    seed_scope_fixture(&env.router, &env.admin_token).await;
    let (status, issued) = issue_api_key(
        &env.router,
        &env.admin_token,
        "line1-temp01-reader",
        &["read:line1.fast.temp01"],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{issued:?}");
    let key = issued["key"].as_str().unwrap();

    // catalog は絞らない - line2 のタグも見える(案 B の核)。
    let (status, catalog) = v1_get(&env.router, key, "/api/v1/tags").await;
    assert_eq!(status, StatusCode::OK, "{catalog:?}");
    let names: Vec<&str> = catalog["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["external_name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"line1.fast.temp01"));
    assert!(names.contains(&"line1.fast.temp02"));
    assert!(
        names.contains(&"line2.slow.press01"),
        "catalog must stay unfiltered (Option B): {names:?}"
    );

    // 単一: 自分のタグは 200、他人のタグは 403(404 ではない - catalog
    // に見えている=存在は既知のため)。
    let (status, _) = v1_get(&env.router, key, "/api/v1/values/line1.fast.temp01").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = v1_get(&env.router, key, "/api/v1/values/line2.slow.press01").await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // バルク(?tags= 省略): スコープ外は黙って除かれる。
    let (status, bulk) = v1_get(&env.router, key, "/api/v1/values").await;
    assert_eq!(status, StatusCode::OK, "{bulk:?}");
    let bulk_tags: Vec<&str> = bulk["values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["tag"].as_str().unwrap())
        .collect();
    assert_eq!(bulk_tags, vec!["line1.fast.temp01"]);

    // バルク(?tags= 明示、スコープ外を含む): 403(単一と同じ規律)。
    let (status, _) = v1_get(&env.router, key, "/api/v1/values?tags=line2.slow.press01").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// S1(グループ・ワイルドカード): `read:line1.fast.*` キーはそのグループ
/// の全タグ(temp01・temp02)を読めるが、別グループ(line2.slow)は読め
/// ない。
#[tokio::test]
async fn group_wildcard_read_scope_key_reads_every_tag_in_its_group_but_not_others() {
    let env = test_env().await;
    seed_scope_fixture(&env.router, &env.admin_token).await;
    let (status, issued) = issue_api_key(
        &env.router,
        &env.admin_token,
        "line1-fast-reader",
        &["read:line1.fast.*"],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{issued:?}");
    let key = issued["key"].as_str().unwrap();

    for tag in ["line1.fast.temp01", "line1.fast.temp02"] {
        let (status, body) = v1_get(&env.router, key, &format!("/api/v1/values/{tag}")).await;
        assert_eq!(status, StatusCode::OK, "{tag}: {body:?}");
    }
    let (status, _) = v1_get(&env.router, key, "/api/v1/values/line2.slow.press01").await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, bulk) = v1_get(&env.router, key, "/api/v1/values").await;
    assert_eq!(status, StatusCode::OK, "{bulk:?}");
    let mut bulk_tags: Vec<&str> = bulk["values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["tag"].as_str().unwrap())
        .collect();
    bulk_tags.sort_unstable();
    assert_eq!(bulk_tags, vec!["line1.fast.temp01", "line1.fast.temp02"]);
}

/// S2(後方互換): 素の `read` キーは従来どおり catalog も値(単一・
/// バルク・明示 `?tags=`)も全件読める - per-tag スコープ導入で既定動作
/// が変わっていないことの回帰防止。
#[tokio::test]
async fn bare_read_scope_key_still_reads_every_tag_value() {
    let env = test_env().await;
    seed_scope_fixture(&env.router, &env.admin_token).await;
    let (status, issued) =
        issue_api_key(&env.router, &env.admin_token, "bare-reader", &["read"]).await;
    assert_eq!(status, StatusCode::CREATED, "{issued:?}");
    let key = issued["key"].as_str().unwrap();

    let (status, _) = v1_get(&env.router, key, "/api/v1/values/line2.slow.press01").await;
    assert_eq!(status, StatusCode::OK);

    let (status, bulk) = v1_get(&env.router, key, "/api/v1/values").await;
    assert_eq!(status, StatusCode::OK, "{bulk:?}");
    let mut bulk_tags: Vec<&str> = bulk["values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["tag"].as_str().unwrap())
        .collect();
    bulk_tags.sort_unstable();
    assert_eq!(
        bulk_tags,
        vec![
            "line1.fast.temp01",
            "line1.fast.temp02",
            "line2.slow.press01"
        ]
    );

    // 明示 ?tags= でも全件通る(スコープ外という概念が無い)。
    let (status, _) = v1_get(&env.router, key, "/api/v1/values?tags=line2.slow.press01").await;
    assert_eq!(status, StatusCode::OK);
}

fn metadata_test_tag(tag_kind: &str, simulation: bool, enabled: bool) -> TagEntry {
    TagEntry {
        external_name: "line1.fast.temp".to_string(),
        tag_key: "tag:3".to_string(),
        ids: (1, 2, 3),
        connection: "line1".to_string(),
        group: "fast".to_string(),
        name: "temp".to_string(),
        address: "40001".to_string(),
        data_type: "f32".to_string(),
        unit: Some("C".to_string()),
        decimals: 1,
        period_ms: 100,
        enabled,
        writable: false,
        tag_kind: tag_kind.to_string(),
        expression: (tag_kind == banto_tags::COMPUTED_TAG_KIND)
            .then(|| "line1.fast.temp + 1".to_string()),
        retain: false,
        simulation,
        push: false,
    }
}

fn metadata_test_status(state: CollectionState, mode: RunMode) -> CollectionStatus {
    CollectionStatus {
        state,
        mode,
        run_id: (state == CollectionState::Running).then_some(9),
        last_error: None,
        configured_revision: 1,
        running_revision: 1,
    }
}

/// #335 のテスト用 - `computed`/`map` を要求するようになった
/// `value_source_for_tag`/`effective_simulation_for_tag`/
/// `CatalogTagEntry::from_runtime` に渡す「何もコミットされていない」
/// `ComputedEngine`。PLC/internal/db タグの分類は `map`/`computed` を
/// 見ないので影響を受けない - computed タグは「入力なし」(`None`)扱いに
/// なり、真にシミュレーション中の入力から`derived_simulation`に
/// 昇格する経路は通らない(その経路は`apps/banto-hub/host/tests/computed.rs`の統合テストで
/// 検証する)。
fn empty_computed_engine() -> ComputedEngine {
    ComputedEngine::new(Arc::new(crate::computed::ServerTagStore::new()))
}

#[test]
fn rest_current_invalid_values_serialize_as_null_bad() {
    let mut tag = metadata_test_tag("internal", false, true);
    let store = crate::computed::ServerTagStore::new();
    let status = metadata_test_status(CollectionState::Stopped, RunMode::Configured);
    for data_type in ["f32", "bit"] {
        tag.data_type = data_type.to_string();
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 2.0] {
            if data_type != "bit" && value.is_finite() {
                continue;
            }
            store.set(&tag.tag_key, Some(value), banto_collect::Quality::Good, 123);
            let json = serde_json::to_value(value_entry(
                &tag.external_name,
                &tag,
                None,
                &store,
                999,
                &status,
                &TagMap::default(),
                &empty_computed_engine(),
            ))
            .unwrap();
            assert_eq!(json["v"], serde_json::Value::Null);
            assert_eq!(json["q"], "bad");
            assert_eq!(json["t"], 123);
        }
    }
}

#[test]
fn rest_catalog_dto_separates_configured_and_effective_simulation() {
    let tag = metadata_test_tag(banto_tags::PLC_TAG_KIND, false, true);
    let all_simulation = metadata_test_status(CollectionState::Running, RunMode::AllSimulation);
    let configured = metadata_test_status(CollectionState::Running, RunMode::Configured);
    let stopped = metadata_test_status(CollectionState::Stopped, RunMode::AllSimulation);
    let map = TagMap::default();
    let computed = empty_computed_engine();

    let all_simulation_json = serde_json::to_value(CatalogTagEntry::from_runtime(
        &tag,
        &all_simulation,
        &map,
        &computed,
    ))
    .expect("catalog DTO serializes");
    assert_eq!(all_simulation_json["simulation"], false);
    assert_eq!(all_simulation_json["configured_simulation"], false);
    assert_eq!(all_simulation_json["effective_simulation"], true);
    assert_eq!(all_simulation_json["value_source"], "simulation");

    let all_simulation_catalog = serde_json::to_value(CatalogResponse {
        revision: 4,
        run_id: all_simulation.run_id,
        collection_mode: all_simulation.mode.as_str().to_string(),
        tags: vec![CatalogTagEntry::from_runtime(
            &tag,
            &all_simulation,
            &map,
            &computed,
        )],
    })
    .expect("catalog response serializes");
    assert_eq!(all_simulation_catalog["run_id"], 9);
    assert_eq!(all_simulation_catalog["collection_mode"], "all_simulation");
    assert_eq!(
        all_simulation_catalog["tags"][0]["value_source"],
        "simulation"
    );

    let configured_catalog = serde_json::to_value(CatalogResponse {
        revision: 4,
        run_id: configured.run_id,
        collection_mode: configured.mode.as_str().to_string(),
        tags: vec![CatalogTagEntry::from_runtime(
            &tag,
            &configured,
            &map,
            &computed,
        )],
    })
    .expect("configured catalog response serializes");
    assert_eq!(configured_catalog["run_id"], 9);
    assert_eq!(configured_catalog["collection_mode"], "configured");
    assert_eq!(configured_catalog["tags"][0]["value_source"], "real");

    assert!(
        !CatalogTagEntry::from_runtime(&tag, &configured, &map, &computed).effective_simulation
    );
    assert!(!CatalogTagEntry::from_runtime(&tag, &stopped, &map, &computed).effective_simulation);
    assert_eq!(
        CatalogTagEntry::from_runtime(&tag, &configured, &map, &computed).value_source,
        "real"
    );
    assert_eq!(
        CatalogTagEntry::from_runtime(&tag, &stopped, &map, &computed).value_source,
        "real"
    );
    let stopped_catalog = serde_json::to_value(CatalogResponse {
        revision: 4,
        run_id: stopped.run_id,
        collection_mode: stopped.mode.as_str().to_string(),
        tags: vec![CatalogTagEntry::from_runtime(
            &tag, &stopped, &map, &computed,
        )],
    })
    .expect("stopped catalog response serializes");
    assert!(stopped_catalog["run_id"].is_null());
    assert_eq!(stopped_catalog["collection_mode"], "all_simulation");
    assert!(
        !tag.simulation,
        "the shared saved catalog must stay unchanged"
    );

    let saved_simulation = metadata_test_tag(banto_tags::PLC_TAG_KIND, true, true);
    assert!(effective_simulation_for_connection(
        "modbus-tcp",
        true,
        true,
        &configured,
        &crate::test_support::test_drivers().registry,
    ));
    assert!(!effective_simulation_for_connection(
        "modbus-tcp",
        true,
        true,
        &stopped,
        &crate::test_support::test_drivers().registry,
    ));
    assert!(
        !CatalogTagEntry::from_runtime(&saved_simulation, &stopped, &map, &computed)
            .effective_simulation
    );
}

/// #335（2026-09-14 オーナー決定）: computed の既定ラベルは
/// `"computed"` - `derived_simulation`は AllSimulation 運転中（または
/// 入力が真にシミュレーション中、`apps/banto-hub/host/tests/computed.rs`の統合テスト参照）
/// だけの情報ラベルに変わった。
#[test]
fn rest_value_source_uses_safe_tag_kind_classification() {
    let all_simulation = metadata_test_status(CollectionState::Running, RunMode::AllSimulation);
    let configured = metadata_test_status(CollectionState::Running, RunMode::Configured);
    let plc = metadata_test_tag(banto_tags::PLC_TAG_KIND, false, true);
    let computed_tag = metadata_test_tag(banto_tags::COMPUTED_TAG_KIND, false, true);
    let internal = metadata_test_tag(banto_tags::INTERNAL_TAG_KIND, false, true);
    let map = TagMap::default();
    let computed = empty_computed_engine();

    assert_eq!(
        value_source_for_tag(&plc, &all_simulation, &map, &computed),
        "simulation"
    );
    assert_eq!(
        value_source_for_tag(&plc, &configured, &map, &computed),
        "real"
    );
    assert_eq!(
        value_source_for_tag(&computed_tag, &all_simulation, &map, &computed),
        "derived_simulation",
        "AllSimulation 運転中は入力の有無によらず derived_simulation"
    );
    assert_eq!(
        value_source_for_tag(&computed_tag, &configured, &map, &computed),
        "computed",
        "Configured 運転中・入力がコミットされていない computed の既定は computed"
    );
    assert_eq!(
        value_source_for_tag(&internal, &all_simulation, &map, &computed),
        "internal"
    );
}

#[test]
fn rest_connection_effective_simulation_returns_to_configured_value() {
    let all_simulation = metadata_test_status(CollectionState::Running, RunMode::AllSimulation);
    let configured = metadata_test_status(CollectionState::Running, RunMode::Configured);
    let stopped = metadata_test_status(CollectionState::Stopped, RunMode::AllSimulation);

    assert!(effective_simulation_for_connection(
        "modbus-tcp",
        true,
        false,
        &all_simulation,
        &crate::test_support::test_drivers().registry,
    ));
    assert!(!effective_simulation_for_connection(
        "modbus-tcp",
        true,
        false,
        &configured,
        &crate::test_support::test_drivers().registry,
    ));
    assert!(!effective_simulation_for_connection(
        "modbus-tcp",
        true,
        false,
        &stopped,
        &crate::test_support::test_drivers().registry,
    ));
    assert!(!effective_simulation_for_connection(
        "virtual",
        true,
        false,
        &all_simulation,
        &crate::test_support::test_drivers().registry,
    ));
}

#[test]
fn collection_control_status_is_camel_case_and_all_simulation_is_explicit() {
    let status = CollectionStatusResponse::from(CollectionStatus {
        state: CollectionState::Faulted,
        mode: RunMode::AllSimulation,
        run_id: Some(7),
        last_error: Some("T15未実装".to_string()),
        configured_revision: 3,
        running_revision: 2,
    });
    let value = serde_json::to_value(status).expect("status serializes");
    assert_eq!(value["state"], "faulted");
    assert_eq!(value["mode"], "all_simulation");
    assert_eq!(value["runId"], 7);
    assert_eq!(value["configuredRevision"], 3);
    assert_eq!(value["runningRevision"], 2);
    assert_eq!(value["lastError"], "T15未実装");
}

/// #341（2026-09-14）: 旧 `collection_edit_lock_is_http_409_with_current_status`
/// の置き換え。「収集中は一律 409」は撤回されたので、代わりに検証するのは
/// 「DB へは入ったが実行構成へ反映できなかった」ときの応答形
/// （200 + 警告ではなく 500 + `live_reconfigure_failed`）。
#[tokio::test]
async fn live_apply_failure_is_http_500_with_live_reconfigure_failed() {
    let response =
        RegistryMutationError::LiveApplyFailed("catalog の検証に失敗しました".to_string())
            .into_response();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"], "live_reconfigure_failed");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("catalog の検証に失敗しました"),
        "{body:?}"
    );
}

/// #392 A2 レビュー（2026-10-12）: 終了の受付中に断った開始は 409 で、
/// 本文は管理 UI（`collectionControlAdmin.ts` の `isErrorBody`）が
/// `message` を表示できる共通の [`ErrorBody`] の形。
#[tokio::test]
async fn quit_in_progress_is_http_409_with_a_displayable_error_body() {
    let response = quit_in_progress_response(QuitInProgress);
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["kind"], "other");
    assert_eq!(body["message"], crate::controller::QUIT_IN_PROGRESS_MESSAGE);
}

#[tokio::test]
async fn collection_control_requires_admin_and_csrf_and_is_idempotent() {
    let env = test_env().await;

    let missing_csrf = HttpRequest::builder()
        .method("POST")
        .uri("/api/collection/start")
        .header("Authorization", format!("Bearer {}", env.admin_token))
        .body(Body::empty())
        .unwrap();
    let response = env.router.clone().oneshot(missing_csrf).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let viewer = HttpRequest::builder()
        .method("POST")
        .uri("/api/collection/start")
        .header("Authorization", format!("Bearer {}", env.viewer_token))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .body(Body::empty())
        .unwrap();
    let response = env.router.clone().oneshot(viewer).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let start = || {
        HttpRequest::builder()
            .method("POST")
            .uri("/api/collection/start")
            .header("Authorization", format!("Bearer {}", env.admin_token))
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .body(Body::empty())
            .unwrap()
    };
    let first = env.router.clone().oneshot(start()).await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let first_body = axum::body::to_bytes(first.into_body(), usize::MAX)
        .await
        .unwrap();
    let first_json: serde_json::Value = serde_json::from_slice(&first_body).unwrap();
    assert_eq!(first_json["state"], "running");

    let repeated = env.router.clone().oneshot(start()).await.unwrap();
    let repeated_body = axum::body::to_bytes(repeated.into_body(), usize::MAX)
        .await
        .unwrap();
    let repeated_json: serde_json::Value = serde_json::from_slice(&repeated_body).unwrap();
    assert_eq!(repeated_json["runId"], first_json["runId"]);

    let stop = HttpRequest::builder()
        .method("POST")
        .uri("/api/collection/stop")
        .header("Authorization", format!("Bearer {}", env.admin_token))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .body(Body::empty())
        .unwrap();
    let response = env.router.clone().oneshot(stop).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT action, resource, result FROM audit_log WHERE resource = 'collection' ORDER BY id",
    )
    .fetch_all(&env.pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![
            (
                "denied".to_string(),
                "collection".to_string(),
                "denied".to_string()
            ),
            (
                "start".to_string(),
                "collection".to_string(),
                "ok".to_string()
            ),
            (
                "start".to_string(),
                "collection".to_string(),
                "ok".to_string()
            ),
            (
                "stop".to_string(),
                "collection".to_string(),
                "ok".to_string()
            ),
        ]
    );
}

#[tokio::test]
async fn tags_create_while_running_is_accepted_and_queued() {
    let env = test_env().await;

    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "protocol": "modbus-tcp", "name": "line1", "settings": { "host": "127.0.0.1", "port": 15022 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");

    let (status, group) = admin_post(
        &env.router,
        "/api/collection-groups",
        &env.admin_token,
        json!({ "name": "fast", "plcConnectionId": conn["id"], "periodMs": 100 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{group:?}");

    let start = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/start")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(start.status(), StatusCode::OK);

    let response = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/tags")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "temp01",
                        "collectionGroupId": group["id"],
                        "address": "40001",
                        "dataType": "i16",
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);

    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["queued"], true);
    assert_eq!(
        body["message"],
        "ロックダウン済みで収集中のため、変更を未適用キューに保存しました。"
    );

    let queued_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pending_changes")
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(queued_count, 1);
}

#[tokio::test]
async fn plc_connections_create_while_running_is_accepted_and_queued() {
    let env = test_env().await;

    let start = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/start")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(start.status(), StatusCode::OK);

    let response = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/plc-connections")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "protocol": "modbus-tcp", "name": "line-running", "settings": { "host": "127.0.0.1", "port": 15022 } })
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);

    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["queued"], true);

    let queued_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pending_changes")
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(queued_count, 1);
}

#[tokio::test]
async fn tags_batch_non_dry_run_while_running_is_accepted_and_queued() {
    let env = test_env().await;

    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "protocol": "modbus-tcp", "name": "line1", "settings": { "host": "127.0.0.1", "port": 15022 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");

    let (status, group) = admin_post(
        &env.router,
        "/api/collection-groups",
        &env.admin_token,
        json!({ "name": "fast", "plcConnectionId": conn["id"], "periodMs": 100 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{group:?}");

    let start = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/start")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(start.status(), StatusCode::OK);

    let response = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/tags/batch")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "dryRun": false,
                        "tags": [{
                            "name": "temp01",
                            "collectionGroupId": group["id"],
                            "address": "40001",
                            "dataType": "i16"
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);

    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["queued"], true);

    let queued_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pending_changes")
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(queued_count, 1);
}

/// #341（オーナー決定 2026-09-09、docs/tag-server-design.md §4.3）: 上の
/// `*_while_running_is_accepted_and_queued` 3本の**試運転中の兄弟** -
/// 同じ「収集中」でも、ロックダウン前（`test_env_unlocked`）なら pending
/// queue を経由せずその場で反映される。202 ではなく 200、
/// `pending_changes` 行は増えず、`configured_revision` が進む。
#[tokio::test]
async fn tags_create_while_running_and_commissioning_is_applied_immediately() {
    let env = test_env_unlocked().await;

    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "protocol": "modbus-tcp", "name": "line1", "settings": { "host": "127.0.0.1", "port": 15122 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");

    let (status, group) = admin_post(
        &env.router,
        "/api/collection-groups",
        &env.admin_token,
        json!({ "name": "fast", "plcConnectionId": conn["id"], "periodMs": 100 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{group:?}");

    let start = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/start")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(start.status(), StatusCode::OK);

    let (_, before) = admin_get(&env.router, "/api/status", &env.admin_token).await;
    let revision_before = before["configuredRevision"].as_u64().unwrap();

    let (status, created) = admin_post(
        &env.router,
        "/api/tags",
        &env.admin_token,
        json!({
            "name": "temp01",
            "collectionGroupId": group["id"],
            "address": "40001",
            "dataType": "i16",
            "enabled": true
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created:?}");
    assert_eq!(created["name"], "temp01", "{created:?}");
    assert!(created["queued"].is_null(), "{created:?}");

    let queued_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pending_changes")
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(queued_count, 0, "試運転中は pending queue を経由しない");

    let (_, after) = admin_get(&env.router, "/api/status", &env.admin_token).await;
    assert!(
        after["configuredRevision"].as_u64().unwrap() > revision_before,
        "{after:?}"
    );
    // 収集は止まっていない（無停止反映）。
    assert_eq!(after["collectionState"], "running", "{after:?}");
}

/// #341: 接続の作成版（[`tags_create_while_running_and_commissioning_is_applied_immediately`]
/// と同じ契約）。
#[tokio::test]
async fn plc_connections_create_while_running_and_commissioning_is_applied_immediately() {
    let env = test_env_unlocked().await;

    let start = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/start")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(start.status(), StatusCode::OK);

    let (status, created) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "protocol": "modbus-tcp", "name": "line-commissioning", "settings": { "host": "127.0.0.1", "port": 15123 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created:?}");
    assert_eq!(created["name"], "line-commissioning", "{created:?}");

    let queued_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pending_changes")
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(queued_count, 0);
}

/// #341: 一括作成版（`tags.batch_create`）。
#[tokio::test]
async fn tags_batch_non_dry_run_while_running_and_commissioning_is_applied_immediately() {
    let env = test_env_unlocked().await;

    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "protocol": "modbus-tcp", "name": "line1", "settings": { "host": "127.0.0.1", "port": 15124 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");

    let (status, group) = admin_post(
        &env.router,
        "/api/collection-groups",
        &env.admin_token,
        json!({ "name": "fast", "plcConnectionId": conn["id"], "periodMs": 100 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{group:?}");

    let start = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/start")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(start.status(), StatusCode::OK);

    let (status, body) = admin_post(
        &env.router,
        "/api/tags/batch",
        &env.admin_token,
        json!({
            "dryRun": false,
            "tags": [{
                "name": "temp01",
                "collectionGroupId": group["id"],
                "address": "40001",
                "dataType": "i16",
                "enabled": true
            }]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], true, "{body:?}");
    assert_eq!(body["count"], 1, "{body:?}");

    let queued_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pending_changes")
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(queued_count, 0);
}

#[tokio::test]
async fn pending_changes_cancel_endpoint_returns_canceled_state() {
    let env = test_env().await;
    let pending_changes = PendingChangesService::new(env.pool.clone());
    let pending = pending_changes
        .create_pending(
            "tags.delete",
            &json!({ "id": 1 }),
            1,
            None,
            Some("admin"),
            Some("admin"),
        )
        .await
        .unwrap();

    let response = env
        .router
        .oneshot(
            HttpRequest::post(format!("/api/pending-changes/{}/cancel", pending.id))
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["state"], "canceled");

    let current = pending_changes.get(pending.id).await.unwrap();
    assert_eq!(current.state, PendingChangeState::Canceled);
}

#[tokio::test]
async fn pending_changes_apply_endpoint_applies_queued_tags_create() {
    let env = test_env().await;

    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "protocol": "modbus-tcp", "name": "line-apply", "settings": { "host": "127.0.0.1", "port": 15023 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");

    let (status, group) = admin_post(
        &env.router,
        "/api/collection-groups",
        &env.admin_token,
        json!({ "name": "fast", "plcConnectionId": conn["id"], "periodMs": 100 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{group:?}");

    let start = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/start")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(start.status(), StatusCode::OK);

    let queued = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/tags")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "temp-apply-01",
                        "collectionGroupId": group["id"],
                        "address": "40001",
                        "dataType": "i16"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(queued.status(), StatusCode::ACCEPTED);
    let queued_bytes = axum::body::to_bytes(queued.into_body(), usize::MAX)
        .await
        .unwrap();
    let queued_body: serde_json::Value = serde_json::from_slice(&queued_bytes).unwrap();
    let pending_id = queued_body["pending"]["id"]
        .as_i64()
        .expect("pending id should exist");

    let stop = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/stop")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stop.status(), StatusCode::OK);

    let response = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post(format!("/api/pending-changes/{pending_id}/apply"))
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["state"], "applied");

    let pending = PendingChangesService::new(env.pool.clone())
        .get(pending_id)
        .await
        .unwrap();
    assert_eq!(pending.state, PendingChangeState::Applied);
}

/// S1a レビュー対応（`banto_tags::PlcConnection::password`の doc
/// comment「外部呼び出し元へ決してシリアライズしない」）: 収集稼働中に
/// パスワード付きの`plc_connections.create`をキューへ入れると、
/// (a) `202 Accepted`の応答、(b) `GET /api/pending-changes`、
/// (c) `GET /api/pending-changes/{id}` のいずれの JSON にも平文
/// パスワードが一切現れず（`secretsSet: ["password"]`だけが出る）、それでいて
/// (d) 適用（apply）後は`PlcConnectionService::get`で見える実際の
/// 保存済み行にはパスワードがちゃんと入っている（＝DB へ保存された
/// `pending_changes.payload`自体は redact していない）ことを確認する。
#[tokio::test]
async fn pending_changes_endpoints_never_leak_a_queued_plc_connection_password() {
    const SECRET: &str = "s3cret-pw-do-not-leak";

    let env = test_env().await;

    // Collection needs to be running for a write to be queued instead of
    // applied immediately - seed an unrelated (non-postgres) connection +
    // group for that, same as the other pending-changes tests above.
    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "protocol": "modbus-tcp", "name": "line-pw-redact-seed", "settings": { "host": "127.0.0.1", "port": 15040 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");
    let (status, group) = admin_post(
        &env.router,
        "/api/collection-groups",
        &env.admin_token,
        json!({ "name": "fast", "plcConnectionId": conn["id"], "periodMs": 100 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{group:?}");

    let start = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/start")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(start.status(), StatusCode::OK);

    // Queue a postgres connection create WITH a password while running.
    let (status, queued_body) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "name": "pgdb-pw-redact", "protocol": "postgres", "settings": { "host": "10.0.0.9", "port": 5432, "database": "appdb", "username": "appuser", "password": SECRET } }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{queued_body:?}");
    assert!(
        !queued_body.to_string().contains(SECRET),
        "202 body must never contain the plaintext password: {queued_body:?}"
    );
    assert_eq!(
        queued_body["pending"]["payload"]["input"]["secretsSet"],
        json!(["password"]),
        "{queued_body:?}"
    );
    assert!(
        queued_body["pending"]["payload"]["input"]
            .get("password")
            .is_none(),
        "{queued_body:?}"
    );
    let pending_id = queued_body["pending"]["id"]
        .as_i64()
        .expect("pending id should exist");

    // GET /api/pending-changes (list): the secret must not appear
    // anywhere in the serialized body.
    let list_response = env
        .router
        .clone()
        .oneshot(
            HttpRequest::get("/api/pending-changes")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list_response.status(), StatusCode::OK);
    let list_bytes = axum::body::to_bytes(list_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let list_text = String::from_utf8(list_bytes.to_vec()).unwrap();
    assert!(
        !list_text.contains(SECRET),
        "GET /api/pending-changes must never contain the plaintext password: {list_text}"
    );
    let list_body: serde_json::Value = serde_json::from_str(&list_text).unwrap();
    let listed = list_body
        .as_array()
        .expect("array body")
        .iter()
        .find(|row| row["id"] == pending_id)
        .expect("queued pending change should be listed");
    assert_eq!(
        listed["payload"]["input"]["secretsSet"],
        json!(["password"])
    );
    assert!(listed["payload"]["input"].get("password").is_none());

    // GET /api/pending-changes/{id}: same guarantee.
    let get_response = env
        .router
        .clone()
        .oneshot(
            HttpRequest::get(format!("/api/pending-changes/{pending_id}"))
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get_response.status(), StatusCode::OK);
    let get_bytes = axum::body::to_bytes(get_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let get_text = String::from_utf8(get_bytes.to_vec()).unwrap();
    assert!(
        !get_text.contains(SECRET),
        "GET /api/pending-changes/{{id}} must never contain the plaintext password: {get_text}"
    );
    let get_body: serde_json::Value = serde_json::from_str(&get_text).unwrap();
    assert_eq!(
        get_body["payload"]["input"]["secretsSet"],
        json!(["password"])
    );
    assert!(get_body["payload"]["input"].get("password").is_none());

    // Stop and apply: the queued create must actually go through with
    // the real password - only the OUTBOUND representation is redacted,
    // never the stored `pending_changes.payload` the apply path reads.
    let stop = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/stop")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stop.status(), StatusCode::OK);

    let apply_response = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post(format!("/api/pending-changes/{pending_id}/apply"))
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(apply_response.status(), StatusCode::OK);
    let apply_bytes = axum::body::to_bytes(apply_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let apply_text = String::from_utf8(apply_bytes.to_vec()).unwrap();
    assert!(
        !apply_text.contains(SECRET),
        "apply response must never contain the plaintext password: {apply_text}"
    );
    let apply_body: serde_json::Value = serde_json::from_str(&apply_text).unwrap();
    assert_eq!(apply_body["state"], "applied");

    // But the actually-stored row DOES have the password - proving the
    // stored payload itself was never touched by the redaction above.
    let created = PlcConnectionService::new(env.pool.clone())
        .list(ListParams::default())
        .await
        .unwrap()
        .rows
        .into_iter()
        .find(|c| c.name == "pgdb-pw-redact")
        .expect("the queued postgres connection should have been created");
    assert_eq!(
        super::common::stored_password(&env.pool, created.id)
            .await
            .as_deref(),
        Some(SECRET)
    );
}

/// #341（2026-09-14 オーナー回答「明示適用も無停止」）: 旧
/// `pending_changes_apply_while_running_returns_409_and_keeps_queue_row`
/// の置き換え。ロックダウン済み + 収集中に積まれた pending change は、
/// **収集を止めずに**明示適用でき、queue 行は `applied` へ遷移し、
/// 収集は `Running` のまま続く。
#[tokio::test]
async fn pending_changes_apply_while_running_applies_without_stopping() {
    let env = test_env().await;

    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "protocol": "modbus-tcp", "name": "line-apply-running", "settings": { "host": "127.0.0.1", "port": 15024 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");

    let (status, group) = admin_post(
        &env.router,
        "/api/collection-groups",
        &env.admin_token,
        json!({ "name": "fast", "plcConnectionId": conn["id"], "periodMs": 100 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{group:?}");

    let start = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/start")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(start.status(), StatusCode::OK);

    let queued = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/tags")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "temp-apply-running-01",
                        "collectionGroupId": group["id"],
                        "address": "40001",
                        "dataType": "i16"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(queued.status(), StatusCode::ACCEPTED);
    let queued_bytes = axum::body::to_bytes(queued.into_body(), usize::MAX)
        .await
        .unwrap();
    let queued_body: serde_json::Value = serde_json::from_slice(&queued_bytes).unwrap();
    let pending_id = queued_body["pending"]["id"]
        .as_i64()
        .expect("pending id should exist");

    let response = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post(format!("/api/pending-changes/{pending_id}/apply"))
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["state"], "applied", "{body:?}");

    let pending = PendingChangesService::new(env.pool.clone())
        .get(pending_id)
        .await
        .unwrap();
    assert_eq!(pending.state, PendingChangeState::Applied);

    // 適用されたタグが実際にレジストリへ入っている。
    let (status, tags) = admin_get(&env.router, "/api/tags", &env.admin_token).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        tags.as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "temp-apply-running-01"),
        "{tags:?}"
    );

    // 収集は止まっていない。
    let (status, collection) = admin_get(&env.router, "/api/status", &env.admin_token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(collection["collectionState"], "running", "{collection:?}");
}

/// TAG-P0-3 follow-up（2026-08-14）: failed 状態の提案を requeue すると
/// pending へ戻り、failure_reason はクリアされる。
#[tokio::test]
async fn pending_changes_requeue_endpoint_returns_pending_state() {
    let env = test_env().await;

    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "protocol": "modbus-tcp", "name": "line-requeue-basic", "settings": { "host": "127.0.0.1", "port": 15034 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");

    let (status, group) = admin_post(
        &env.router,
        "/api/collection-groups",
        &env.admin_token,
        json!({ "name": "fast", "plcConnectionId": conn["id"], "periodMs": 100 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{group:?}");

    let start = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/start")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(start.status(), StatusCode::OK);

    let queued = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/tags")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "temp-requeue-basic-01",
                        "collectionGroupId": group["id"],
                        "address": "40001",
                        "dataType": "i16"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(queued.status(), StatusCode::ACCEPTED);
    let queued_bytes = axum::body::to_bytes(queued.into_body(), usize::MAX)
        .await
        .unwrap();
    let queued_body: serde_json::Value = serde_json::from_slice(&queued_bytes).unwrap();
    let pending_id = queued_body["pending"]["id"]
        .as_i64()
        .expect("pending id should exist");

    // #341（2026-09-14）: failed 行の作り方を差し替えた。以前は
    // 「収集稼働中の apply は 409」を使っていたが、その 409 自体が
    // 撤廃された（適用は無停止で通る）ので、代わりに**同じ名前のタグを
    // 先に直接作っておく**ことで、適用時に一意制約で失敗させる
    // （requeue が対象とする「一過性の失敗」の実例そのもの）。
    let stop = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/stop")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stop.status(), StatusCode::OK);
    let (status, blocker) = admin_post(
        &env.router,
        "/api/tags",
        &env.admin_token,
        json!({
            "name": "temp-requeue-basic-01",
            "collectionGroupId": group["id"],
            "address": "40002",
            "dataType": "i16"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{blocker:?}");

    let apply_conflicting = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post(format!("/api/pending-changes/{pending_id}/apply"))
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        !apply_conflicting.status().is_success(),
        "{:?}",
        apply_conflicting.status()
    );

    let pending_changes = PendingChangesService::new(env.pool.clone());
    let failed = pending_changes.get(pending_id).await.unwrap();
    assert_eq!(failed.state, PendingChangeState::Failed);

    let response = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post(format!("/api/pending-changes/{pending_id}/requeue"))
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["state"], "pending");
    assert!(body["failureReason"].is_null());

    let requeued = pending_changes.get(pending_id).await.unwrap();
    assert_eq!(requeued.state, PendingChangeState::Pending);
}

/// #341 レビュー対応（2026-09-14）: エラー変換だけを見る
/// `live_apply_failure_is_http_500_with_live_reconfigure_failed` とは別に、
/// **endpoint 越し**に「DB への適用は成功したが実行構成への反映だけが
/// 失敗した」状態を作り、契約4点を一度に固定する:
///
/// 1. `POST /api/pending-changes/{id}/apply` は 500
///    `live_reconfigure_failed`（回復手段の案内つき）
/// 2. pending change の行は **`applied`**（`failed` にしない - DB へは
///    入っているので requeue で二重適用させない）
/// 3. 監査ログに `result = "failed"` の行が残る
/// 4. 理由が `/api/status` の `lastConfigError` からも読める
///
/// 反映失敗は `CollectorManager` のテスト専用フック
/// （`fail_next_apply_for_test`、同モジュール参照）で決定的に起こす -
/// 本物の失敗要因（tstore の writer を開けない）を OS 越しに再現するのは
/// プラットフォーム依存が強すぎるため。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_apply_live_reconfigure_failure_is_500_and_marks_applied() {
    let env = test_env().await;

    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "protocol": "modbus-tcp", "name": "line-live-fail", "settings": { "host": "127.0.0.1", "port": 15026 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");

    let (status, group) = admin_post(
        &env.router,
        "/api/collection-groups",
        &env.admin_token,
        json!({ "name": "fast", "plcConnectionId": conn["id"], "periodMs": 100 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{group:?}");

    let start = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/start")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(start.status(), StatusCode::OK);

    // ロックダウン済み + 収集中なので queue へ。
    let (status, queued) = admin_post(
        &env.router,
        "/api/tags",
        &env.admin_token,
        json!({
            "name": "temp-live-fail-01",
            "collectionGroupId": group["id"],
            "address": "40001",
            "dataType": "i16",
            "enabled": true
        }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{queued:?}");
    let pending_id = queued["pending"]["id"].as_i64().expect("pending id");

    // 次の `apply_run`（= 明示適用の live re-apply）だけを失敗させる。
    env.manager.fail_next_apply_for_test();

    let response = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post(format!("/api/pending-changes/{pending_id}/apply"))
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"], "live_reconfigure_failed", "{body:?}");
    let message = body["message"].as_str().unwrap();
    assert!(message.contains("テスト用に注入された"), "{body:?}");
    assert!(
        message.contains("POST /api/collection/reapply"),
        "回復手段が案内されること: {body:?}"
    );

    // 2. DB へは適用済みなので行は applied（failed にしない）。
    assert_eq!(body["pending"]["state"], "applied", "{body:?}");
    let pending = PendingChangesService::new(env.pool.clone())
        .get(pending_id)
        .await
        .unwrap();
    assert_eq!(pending.state, PendingChangeState::Applied);
    let (status, tags) = admin_get(&env.router, "/api/tags", &env.admin_token).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        tags.as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "temp-live-fail-01"),
        "レジストリ行自体は入っていること: {tags:?}"
    );

    // #341 レビュー対応2: catalog も直前の成功構成へ戻っている
    // （読み側だけ新構成、という食い違いを残さない -
    // `CollectorManager::rollback_catalog`）。DB には在るが catalog
    // （`/api/v1/tags`）には**まだ現れない**のが正。
    let (status, catalog) = admin_get(&env.router, "/api/v1/tags", &env.admin_token).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !catalog["tags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "temp-live-fail-01"),
        "反映に失敗したので catalog へは載らない: {catalog:?}"
    );

    // 3. 監査ログに失敗行が残る。
    let audited: Vec<(String, String)> = sqlx::query_as(
        "SELECT action, result FROM audit_log WHERE resource = 'pending_changes' ORDER BY id",
    )
    .fetch_all(&env.pool)
    .await
    .unwrap();
    assert!(
        audited
            .iter()
            .any(|(action, result)| action == "apply" && result == "failed"),
        "{audited:?}"
    );

    // 4. 理由が `/api/status` からも読める。
    let (status, runtime) = admin_get(&env.router, "/api/status", &env.admin_token).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        runtime["lastConfigError"]
            .as_str()
            .unwrap_or_default()
            .contains("テスト用に注入された"),
        "{runtime:?}"
    );
    // 収集自体は止まっていない。
    assert_eq!(runtime["collectionState"], "running", "{runtime:?}");

    // 回復手段（`LIVE_APPLY_RECOVERY_HINT`）どおり、再適用すれば載る。
    let reapply = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/reapply")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(reapply.status(), StatusCode::OK);
    let (status, catalog) = admin_get(&env.router, "/api/v1/tags", &env.admin_token).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        catalog["tags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "temp-live-fail-01"),
        "reapply で catalog へ載ること: {catalog:?}"
    );
}

/// #341 レビュー対応（2026-09-14）: 反映失敗からの回復手段
/// `POST /api/collection/reapply` が、収集を止めずに最新の DB 内容を
/// 適用し直せること（`collection_reapply` の doc comment）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn collection_reapply_reapplies_the_latest_registry_without_stopping() {
    let env = test_env().await;

    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "protocol": "modbus-tcp", "name": "line-reapply", "settings": { "host": "127.0.0.1", "port": 15027 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");

    let start = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/start")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(start.status(), StatusCode::OK);
    let (_, before) = admin_get(&env.router, "/api/status", &env.admin_token).await;
    let run_id_before = before["runId"].as_u64();

    let reapply = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/reapply")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(reapply.status(), StatusCode::OK);

    let (_, after) = admin_get(&env.router, "/api/status", &env.admin_token).await;
    assert_eq!(after["collectionState"], "running", "{after:?}");
    assert_eq!(after["runId"].as_u64(), run_id_before, "{after:?}");
    assert!(after["lastConfigError"].is_null(), "{after:?}");

    // viewer は admin 専用ルーターに弾かれる。
    let forbidden = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/reapply")
                .header("Authorization", format!("Bearer {}", env.viewer_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
}

/// TAG-P0-3 follow-up（2026-08-14）: 一過性の失敗は requeue → 失敗要因の
/// 解消 → 再 apply で回復できることを確認する（#341 で「収集稼働中の
/// 409」が無くなったため、失敗要因は同名タグの一意制約違反に差し替えた）。
#[tokio::test]
async fn pending_changes_requeue_then_apply_succeeds_after_transient_failure_clears() {
    let env = test_env().await;

    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "protocol": "modbus-tcp", "name": "line-requeue-transient", "settings": { "host": "127.0.0.1", "port": 15035 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");

    let (status, group) = admin_post(
        &env.router,
        "/api/collection-groups",
        &env.admin_token,
        json!({ "name": "fast", "plcConnectionId": conn["id"], "periodMs": 100 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{group:?}");

    let start = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/start")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(start.status(), StatusCode::OK);

    let queued = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/tags")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "temp-requeue-transient-01",
                        "collectionGroupId": group["id"],
                        "address": "40001",
                        "dataType": "i16"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(queued.status(), StatusCode::ACCEPTED);
    let queued_bytes = axum::body::to_bytes(queued.into_body(), usize::MAX)
        .await
        .unwrap();
    let queued_body: serde_json::Value = serde_json::from_slice(&queued_bytes).unwrap();
    let pending_id = queued_body["pending"]["id"]
        .as_i64()
        .expect("pending id should exist");

    // #341（2026-09-14）: 一過性の失敗の作り方を差し替えた（上の
    // `pending_changes_requeue_endpoint_returns_pending_state` と同じ
    // 理由）- 同名タグを先に直接作って一意制約違反で失敗させ、その
    // 邪魔者を消してから requeue → 再 apply が通ることを確認する。
    let stop = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/stop")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stop.status(), StatusCode::OK);
    let (status, blocker) = admin_post(
        &env.router,
        "/api/tags",
        &env.admin_token,
        json!({
            "name": "temp-requeue-transient-01",
            "collectionGroupId": group["id"],
            "address": "40002",
            "dataType": "i16"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{blocker:?}");
    let blocker_id = blocker["id"].as_i64().expect("blocker tag id");

    let apply_conflicting = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post(format!("/api/pending-changes/{pending_id}/apply"))
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        !apply_conflicting.status().is_success(),
        "{:?}",
        apply_conflicting.status()
    );

    let pending_changes = PendingChangesService::new(env.pool.clone());
    let failed = pending_changes.get(pending_id).await.unwrap();
    assert_eq!(failed.state, PendingChangeState::Failed);

    let requeue_response = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post(format!("/api/pending-changes/{pending_id}/requeue"))
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(requeue_response.status(), StatusCode::OK);
    let requeue_bytes = axum::body::to_bytes(requeue_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let requeue_body: serde_json::Value = serde_json::from_slice(&requeue_bytes).unwrap();
    assert_eq!(requeue_body["state"], "pending");

    // 失敗要因（同名タグ）を取り除く = 一過性の失敗が解消した状態。
    let delete_blocker = env
        .router
        .clone()
        .oneshot(
            HttpRequest::delete(format!("/api/tags/{blocker_id}"))
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(delete_blocker.status().is_success());

    let reapply = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post(format!("/api/pending-changes/{pending_id}/apply"))
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(reapply.status(), StatusCode::OK);
    let reapply_bytes = axum::body::to_bytes(reapply.into_body(), usize::MAX)
        .await
        .unwrap();
    let reapply_body: serde_json::Value = serde_json::from_slice(&reapply_bytes).unwrap();
    assert_eq!(reapply_body["state"], "applied");

    let applied = pending_changes.get(pending_id).await.unwrap();
    assert_eq!(applied.state, PendingChangeState::Applied);
}

/// TAG-P0-3 follow-up（2026-08-12）: 適用対象の `plc_connections` 行が
/// enqueue 時点から変わっていなければ、`base_fingerprint` ガードは何も
/// 妨げない（ハッピーパス）。
#[tokio::test]
async fn pending_apply_plc_connection_update_succeeds_when_row_unchanged() {
    let env = test_env().await;

    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "protocol": "modbus-tcp", "name": "line-fp-ok", "settings": { "host": "127.0.0.1", "port": 15030 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");
    let conn_id = conn["id"].as_i64().unwrap();

    let start = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/start")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(start.status(), StatusCode::OK);

    let queued = env
        .router
        .clone()
        .oneshot(
            HttpRequest::put(format!("/api/plc-connections/{conn_id}"))
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "protocol": "modbus-tcp", "name": "line-fp-ok-renamed", "settings": { "host": "127.0.0.1", "port": 15030 } })
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(queued.status(), StatusCode::ACCEPTED);
    let queued_bytes = axum::body::to_bytes(queued.into_body(), usize::MAX)
        .await
        .unwrap();
    let queued_body: serde_json::Value = serde_json::from_slice(&queued_bytes).unwrap();
    let pending_id = queued_body["pending"]["id"].as_i64().unwrap();

    let stop = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection/stop")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stop.status(), StatusCode::OK);

    let response = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post(format!("/api/pending-changes/{pending_id}/apply"))
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["state"], "applied");

    let updated = PlcConnectionService::new(env.pool.clone())
        .get(conn_id)
        .await
        .unwrap();
    assert_eq!(updated.name, "line-fp-ok-renamed");
}
