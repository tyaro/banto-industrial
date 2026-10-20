use super::*;
use crate::api_keys::ApiKeysService;
use crate::db::migrate_memory;
use crate::pending_changes::PendingChangeState;
use axum::body::Body;
use axum::http::Request as HttpRequest;
use banto_tstore::{Clock, ManualClock, SystemClock};
use tokio::sync::broadcast as tokio_broadcast;
use tower::ServiceExt;

mod api_keys;
mod common;
mod general;

use common::{
    admin_delete, admin_get, admin_post, admin_put, issue_api_key, test_env, test_env_on_file_db,
    test_env_unlocked, test_env_with_clock, test_manager_with_clock, v1_get, v1_put, TestEnv,
    CLIENT_HEADER,
};

// --- H10 ③: per-tag read スコープ(Option B、
// docs/h10-3-read-scope-proposal.md §5・§6) ------------------------------

/// admin 系ルーター(CSRF ヘッダ必須)への `POST` - `issue_api_key` と
/// `line1.fast.temp01`/`line1.fast.temp02`/`line2.slow.press01` の3タグを
/// 別接続・別グループで作る(admin REST 経由 - I1 の書き込みハンドラは
/// 成功のたびに `CollectorManager::rebuild` を自動で呼ぶので、明示的な
/// rebuild 呼び出しは不要、このモジュールの doc comment「I1 CRUD 書き
/// 込み後の再構築」参照)。catalog は registry の読み取りだけで完結する
/// (`crate::hub::build_catalog`)ため、実際に PLC へ繋がらないポートでも
/// rebuild は成功しカタログへ反映される -
/// `tags_create_via_admin_router_rebuilds_the_catalog` と同じ判断。
async fn seed_scope_fixture(router: &Router, admin_token: &str) {
    let (status, conn1) = admin_post(
        router,
        "/api/plc-connections",
        admin_token,
        json!({ "name": "line1", "host": "127.0.0.1", "port": 15101 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn1:?}");
    let (status, group1) = admin_post(
        router,
        "/api/collection-groups",
        admin_token,
        json!({ "name": "fast", "plcConnectionId": conn1["id"], "periodMs": 100 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{group1:?}");
    for (name, address) in [("temp01", "40001"), ("temp02", "40003")] {
        let (status, tag) = admin_post(
            router,
            "/api/tags",
            admin_token,
            json!({
                "name": name,
                "collectionGroupId": group1["id"],
                "address": address,
                "dataType": "i16",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{tag:?}");
    }

    let (status, conn2) = admin_post(
        router,
        "/api/plc-connections",
        admin_token,
        json!({ "name": "line2", "host": "127.0.0.1", "port": 15102 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn2:?}");
    let (status, group2) = admin_post(
        router,
        "/api/collection-groups",
        admin_token,
        json!({ "name": "slow", "plcConnectionId": conn2["id"], "periodMs": 1000 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{group2:?}");
    let (status, tag) = admin_post(
        router,
        "/api/tags",
        admin_token,
        json!({
            "name": "press01",
            "collectionGroupId": group2["id"],
            "address": "40001",
            "dataType": "i16",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{tag:?}");
}

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
/// 昇格する経路は通らない(その経路は`tests/computed.rs`の統合テストで
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
    ));
    assert!(!effective_simulation_for_connection(
        "modbus-tcp",
        true,
        true,
        &stopped,
    ));
    assert!(
        !CatalogTagEntry::from_runtime(&saved_simulation, &stopped, &map, &computed)
            .effective_simulation
    );
}

/// #335（2026-09-14 オーナー決定）: computed の既定ラベルは
/// `"computed"` - `derived_simulation`は AllSimulation 運転中（または
/// 入力が真にシミュレーション中、`tests/computed.rs`の統合テスト参照）
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
    ));
    assert!(!effective_simulation_for_connection(
        "modbus-tcp",
        true,
        false,
        &configured,
    ));
    assert!(!effective_simulation_for_connection(
        "modbus-tcp",
        true,
        false,
        &stopped,
    ));
    assert!(!effective_simulation_for_connection(
        "virtual",
        true,
        false,
        &all_simulation,
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
        json!({ "name": "line1", "host": "127.0.0.1", "port": 15022 }),
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
                    json!({ "name": "line-running", "host": "127.0.0.1", "port": 15022 })
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
        json!({ "name": "line1", "host": "127.0.0.1", "port": 15022 }),
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
        json!({ "name": "line1", "host": "127.0.0.1", "port": 15122 }),
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
        json!({ "name": "line-commissioning", "host": "127.0.0.1", "port": 15123 }),
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
        json!({ "name": "line1", "host": "127.0.0.1", "port": 15124 }),
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
        json!({ "name": "line-apply", "host": "127.0.0.1", "port": 15023 }),
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
/// パスワードが一切現れず（`passwordSet: true`だけが出る）、それでいて
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
        json!({ "name": "line-pw-redact-seed", "host": "127.0.0.1", "port": 15040 }),
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
        json!({
            "name": "pgdb-pw-redact",
            "protocol": "postgres",
            "host": "10.0.0.9",
            "port": 5432,
            "database": "appdb",
            "username": "appuser",
            "password": SECRET
        }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{queued_body:?}");
    assert!(
        !queued_body.to_string().contains(SECRET),
        "202 body must never contain the plaintext password: {queued_body:?}"
    );
    assert_eq!(
        queued_body["pending"]["payload"]["input"]["passwordSet"], true,
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
    assert_eq!(listed["payload"]["input"]["passwordSet"], true);
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
    assert_eq!(get_body["payload"]["input"]["passwordSet"], true);
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
    assert_eq!(created.password.as_deref(), Some(SECRET));
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
        json!({ "name": "line-apply-running", "host": "127.0.0.1", "port": 15024 }),
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
        json!({ "name": "line-requeue-basic", "host": "127.0.0.1", "port": 15034 }),
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
        json!({ "name": "line-live-fail", "host": "127.0.0.1", "port": 15026 }),
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
        json!({ "name": "line-reapply", "host": "127.0.0.1", "port": 15027 }),
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
        json!({ "name": "line-requeue-transient", "host": "127.0.0.1", "port": 15035 }),
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
        json!({ "name": "line-fp-ok", "host": "127.0.0.1", "port": 15030 }),
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
                    json!({ "name": "line-fp-ok-renamed", "host": "127.0.0.1", "port": 15030 })
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
            password: None,
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

// --- S1 (docs/banto-hub-external-db-design.md §4.1・§4.6・§6・§7):
// PostgreSQL 接続エンティティ・接続テスト API ------------------------

/// `POST /api/plc-connections` で `protocol: "postgres"` を作成すると、
/// 応答に `passwordSet: true` が入り、`password` フィールド自体は
/// 出ない（`PlcConnectionResponse`が生の`password`を決して外部へ
/// 出さないことの回帰テスト）。
#[tokio::test]
async fn create_postgres_connection_returns_password_set_without_password_field() {
    let env = test_env().await;
    let (status, body) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({
            "name": "erp-db",
            "protocol": "postgres",
            "host": "10.0.0.50",
            "port": 5432,
            "database": "erp",
            "username": "reader",
            "password": "s3cret",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["passwordSet"], true);
    assert!(
        body.get("password").is_none(),
        "the response must never contain the raw password: {body:?}"
    );
    assert_eq!(body["database"], "erp");
    assert_eq!(body["username"], "reader");
}

/// `PUT`で`password`を省略（`null`/未指定）すると、既存のパスワードは
/// 変更されない（`banto_tags::PlcConnectionInput::password`の doc
/// comment、update の tri-state PATCH 意味論の REST 経由での確認）。
#[tokio::test]
async fn update_omitting_password_keeps_the_existing_password() {
    let env = test_env().await;
    let (status, created) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({
            "name": "erp-db-keep",
            "protocol": "postgres",
            "host": "10.0.0.50",
            "port": 5432,
            "database": "erp",
            "username": "reader",
            "password": "original-secret",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created:?}");
    let conn_id = created["id"].as_i64().unwrap();
    assert_eq!(created["passwordSet"], true);

    // password フィールドを省略した PUT（他は全項目必須）。
    let (status, updated) = admin_put(
        &env.router,
        &format!("/api/plc-connections/{conn_id}"),
        &env.admin_token,
        json!({
            "name": "erp-db-keep-renamed",
            "protocol": "postgres",
            "host": "10.0.0.50",
            "port": 5432,
            "unitId": 1,
            "enabled": true,
            "simulation": false,
            "wordOrder": "low_high",
            "database": "erp",
            "username": "reader",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated:?}");
    assert_eq!(
        updated["passwordSet"], true,
        "omitting password must keep it set"
    );

    let stored = PlcConnectionService::new(env.pool.clone())
        .get(conn_id)
        .await
        .unwrap();
    assert_eq!(stored.password.as_deref(), Some("original-secret"));
}

/// `PUT`で`password: ""`を送ると、パスワードが消去される。
#[tokio::test]
async fn update_with_empty_password_clears_it() {
    let env = test_env().await;
    let (status, created) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({
            "name": "erp-db-clear",
            "protocol": "postgres",
            "host": "10.0.0.50",
            "port": 5432,
            "database": "erp",
            "username": "reader",
            "password": "original-secret",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created:?}");
    let conn_id = created["id"].as_i64().unwrap();

    let (status, updated) = admin_put(
        &env.router,
        &format!("/api/plc-connections/{conn_id}"),
        &env.admin_token,
        json!({
            "name": "erp-db-clear",
            "protocol": "postgres",
            "host": "10.0.0.50",
            "port": 5432,
            "unitId": 1,
            "enabled": true,
            "simulation": false,
            "wordOrder": "low_high",
            "database": "erp",
            "username": "reader",
            "password": "",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated:?}");
    assert_eq!(updated["passwordSet"], false);

    let stored = PlcConnectionService::new(env.pool.clone())
        .get(conn_id)
        .await
        .unwrap();
    assert_eq!(stored.password, None);
}

/// 非 postgres 接続に`database`を付けると`422 Validation`で拒否される
/// (`banto_tags::plc_connection::validate_plc_connection_input`の
/// postgres-only ルール、REST 経由の確認)。
#[tokio::test]
async fn create_non_postgres_with_database_is_rejected() {
    let env = test_env().await;
    let (status, body) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({
            "name": "not-pg",
            "protocol": "modbus-tcp",
            "host": "127.0.0.1",
            "port": 15070,
            "database": "somedb",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    assert_eq!(body["kind"], "validation");
}

/// S1（この段階では postgres 接続配下にグループを作れない）: REST 経由の
/// 確認 - `banto_tags::collection_group`側の単体テストの REST 版。
#[tokio::test]
async fn create_group_under_a_postgres_connection_is_rejected() {
    let env = test_env().await;
    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({
            "name": "erp-db-group-guard",
            "protocol": "postgres",
            "host": "10.0.0.50",
            "port": 5432,
            "database": "erp",
            "username": "reader",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");
    let conn_id = conn["id"].as_i64().unwrap();

    let (status, body) = admin_post(
        &env.router,
        "/api/collection-groups",
        &env.admin_token,
        json!({ "name": "g-on-pg", "plcConnectionId": conn_id, "periodMs": 1000 }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    assert_eq!(body["kind"], "validation");
}

/// `POST /api/plc-connections/{id}/test` - S1a の接続テスト API。
/// 非 postgres 接続に対しては 400 で拒否する（design §4.6・実装指示
/// 「for non-postgres protocols respond 400」）。
#[tokio::test]
async fn test_saved_connection_rejects_non_postgres_with_400() {
    let env = test_env().await;
    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "name": "not-pg-test", "host": "127.0.0.1", "port": 15071 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");
    let conn_id = conn["id"].as_i64().unwrap();

    let (status, body) = admin_post(
        &env.router,
        &format!("/api/plc-connections/{conn_id}/test"),
        &env.admin_token,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body:?}");
}

/// `POST /api/plc-connections/{id}/test` の実 PostgreSQL 経路。
/// `BANTO_TEST_PG_URL`が未設定なら`eprintln!`してスキップする
/// （`crate::db_source`の同種テストと同じ慣習）。
#[tokio::test]
async fn test_saved_connection_against_a_real_postgresql_returns_the_server_version() {
    let Ok(url) = std::env::var("BANTO_TEST_PG_URL") else {
        eprintln!("skipped: BANTO_TEST_PG_URL unset");
        return;
    };
    let options: sqlx::postgres::PgConnectOptions = url
        .parse()
        .expect("BANTO_TEST_PG_URL should be a valid postgres:// URL");
    let password = url
        .split_once("://")
        .and_then(|(_, rest)| rest.split_once('@'))
        .and_then(|(userinfo, _)| userinfo.split_once(':'))
        .map(|(_, password)| password.to_string());

    let env = test_env().await;
    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({
            "name": "real-pg-test",
            "protocol": "postgres",
            "host": options.get_host(),
            "port": options.get_port(),
            "database": options.get_database(),
            "username": options.get_username(),
            "password": password,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");
    let conn_id = conn["id"].as_i64().unwrap();

    let (status, body) = admin_post(
        &env.router,
        &format!("/api/plc-connections/{conn_id}/test"),
        &env.admin_token,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], true, "{body:?}");
    let server_version = body["serverVersion"].as_str().expect("serverVersion");
    eprintln!("POST /api/plc-connections/{{id}}/test -> {server_version}");
    assert!(body.get("error").is_none());
}

/// `POST /api/plc-connections/{id}/test`の失敗経路: 閉じたポートに
/// 対して`ok:false`かつパスワードを含まないエラー文言を返す。
#[tokio::test]
async fn test_saved_connection_against_a_closed_port_reports_failure_without_the_password() {
    let env = test_env().await;
    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({
            "name": "closed-port-pg",
            "protocol": "postgres",
            "host": "127.0.0.1",
            "port": 1,
            "database": "erp",
            "username": "reader",
            "password": "s3cret-password",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");
    let conn_id = conn["id"].as_i64().unwrap();

    let (status, body) = admin_post(
        &env.router,
        &format!("/api/plc-connections/{conn_id}/test"),
        &env.admin_token,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], false, "{body:?}");
    let error = body["error"].as_str().expect("error message");
    assert!(
        !error.contains("s3cret-password"),
        "error text must never contain the plaintext password: {error}"
    );
}

/// キューされた削除（収集稼働中に 202 で保留し、停止後に適用）も同じ
/// カスケード仕様に揃っている - `execute_pending_apply` の
/// `"plc_connections.delete"` 分岐が `cascade_delete_tx` を呼ぶことの
/// 回帰テスト（この分岐を旧 `delete_tx` のまま直し忘れると、即時削除
/// (`plc_connections_delete`) は成功するのにキュー適用だけこの
/// バリデーションで失敗する、という非対称なバグになる）。
#[tokio::test]
async fn pending_apply_plc_connection_delete_cascades_when_it_has_groups_and_tags() {
    let env = test_env().await;

    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "name": "line-cascade-pending", "host": "127.0.0.1", "port": 15062 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");
    let conn_id = conn["id"].as_i64().unwrap();

    let (status, group) = admin_post(
        &env.router,
        "/api/collection-groups",
        &env.admin_token,
        json!({ "name": "g-cascade-pending", "plcConnectionId": conn_id, "periodMs": 1000 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{group:?}");
    let group_id = group["id"].as_i64().unwrap();

    let (status, tag) = admin_post(
        &env.router,
        "/api/tags",
        &env.admin_token,
        json!({
            "name": "t-cascade-pending",
            "collectionGroupId": group_id,
            "address": "40001",
            "dataType": "i16"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{tag:?}");
    let tag_id = tag["id"].as_i64().unwrap();

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

    // While running, the delete is only queued (202) - not yet applied.
    let queued = env
        .router
        .clone()
        .oneshot(
            HttpRequest::delete(format!("/api/plc-connections/{conn_id}"))
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
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

    let apply = env
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
    assert_eq!(apply.status(), StatusCode::OK);
    let apply_bytes = axum::body::to_bytes(apply.into_body(), usize::MAX)
        .await
        .unwrap();
    let apply_body: serde_json::Value = serde_json::from_slice(&apply_bytes).unwrap();
    assert_eq!(apply_body["state"], "applied");

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

/// 実機で再現した不具合の修正2（2026-08-31 オーナー報告）: pending change
/// の適用が validation エラー（名前の重複）で失敗したとき、
/// `failure_reason` にフィールド単位の詳細（`name: この接続内では既に使用されています`、
/// グループ名は 2026-10-19 から接続内で一意 - migration 0022）
/// が含まれることを確認する。修正前は `BantoError` の `Display`
/// （`thiserror`）が種別ごとの定型文だけ（`Validation` は常に
/// `"validation failed"`）だったため、`pending change の適用に失敗
/// しました: validation failed` としか出ず、何が失敗したのか分からな
/// かった - オーナーが収集稼働中に同じ名前（`group1`）で収集グループを
/// 3回作成し、3回とも適用が全滅した際にこれで気づけなかった。
#[tokio::test]
async fn pending_apply_collection_group_create_failure_reason_includes_field_detail() {
    let env = test_env().await;

    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "name": "line-dup-name-detail", "host": "127.0.0.1", "port": 15042 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");
    let conn_id = conn["id"].as_i64().unwrap();

    // 先に "group1" を普通に（収集停止中に）作成しておく - 既存レコード
    // として DB に存在する状態を作る。
    let (status, existing) = admin_post(
        &env.router,
        "/api/collection-groups",
        &env.admin_token,
        json!({ "name": "group1", "plcConnectionId": conn_id, "periodMs": 100 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{existing:?}");

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

    // 収集稼働中に同じ名前 "group1" で再度作成 → オーナーが実機で再現した
    // 状況そのもの: 202 でキューに入るだけで、DB には現れない。
    let queued = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/collection-groups")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "name": "group1", "plcConnectionId": conn_id, "periodMs": 100 })
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

    // 適用すると "name" が重複しているため validation エラーで拒否される。
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
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let pending = PendingChangesService::new(env.pool.clone())
        .get(pending_id)
        .await
        .unwrap();
    assert_eq!(pending.state, PendingChangeState::Failed);
    let failure_reason = pending
        .failure_reason
        .expect("failure_reason should be set");
    assert_ne!(
        failure_reason, "pending change の適用に失敗しました: validation failed",
        "failure_reason should include field-level detail, not just the generic BantoError Display: {failure_reason}"
    );
    assert!(
        failure_reason.contains("name")
            && failure_reason.contains("この接続内では既に使用されています"),
        "failure_reason should say which field failed and why: {failure_reason}"
    );
}

/// TAG-P0-3 follow-up（2026-08-12）: pending change を enqueue した後、
/// キューを経由しない別経路（別セッションなどを模した直接の service
/// 呼び出し）で同じ `plc_connections` 行が書き換えられていた場合、
/// apply は conflict として拒否され、pending 行は `failed` へ遷移して
/// 具体的な `failure_reason` を残す。その後 `failed` からキャンセル
/// できることも確認する（従来は `pending` のみキャンセル可能だった）。
#[tokio::test]
async fn pending_apply_plc_connection_update_conflicts_when_row_changed_out_of_band() {
    let env = test_env().await;

    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "name": "line-fp-conflict", "host": "127.0.0.1", "port": 15031 }),
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
                    json!({ "name": "line-fp-conflict-queued", "host": "127.0.0.1", "port": 15031 })
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

    // 「別経路での編集」を、pending queue を経由しない直接の service 層
    // 呼び出し（`update_tx` ではなく非トランザクション `update`）で
    // 模す - 別セッションが同じ行を先に書き換えたケースに相当する。
    PlcConnectionService::new(env.pool.clone())
        .update(
            conn_id,
            PlcConnectionInput {
                name: "line-fp-conflict-hijacked".to_string(),
                protocol: "modbus-tcp".to_string(),
                host: "127.0.0.1".to_string(),
                port: 15031,
                unit_id: 1,
                enabled: true,
                simulation: false,

                word_order: "low_high".to_string(),
                cpu_type: None,
                database: None,
                username: None,
                password: None,
            },
        )
        .await
        .unwrap();

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
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"], "pending_apply_conflict");
    assert_eq!(body["resource"], "plc_connections");

    let pending = PendingChangesService::new(env.pool.clone())
        .get(pending_id)
        .await
        .unwrap();
    assert_eq!(pending.state, PendingChangeState::Failed);
    let failure_reason = pending
        .failure_reason
        .expect("failure_reason should be set");
    assert!(
        !failure_reason.is_empty() && failure_reason != "pending change の適用に失敗しました",
        "failure_reason should be specific, not generic: {failure_reason}"
    );
    assert!(
        failure_reason.contains("変更されています"),
        "failure_reason should explain the staleness conflict: {failure_reason}"
    );

    // 元の接続は「別経路」の編集内容のまま(適用は拒否されたので上書き
    // されていない)。
    let untouched = PlcConnectionService::new(env.pool.clone())
        .get(conn_id)
        .await
        .unwrap();
    assert_eq!(untouched.name, "line-fp-conflict-hijacked");

    // failed からのキャンセルは新たに許可された復旧経路。
    let cancel_response = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post(format!("/api/pending-changes/{pending_id}/cancel"))
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cancel_response.status(), StatusCode::OK);
    let cancel_bytes = axum::body::to_bytes(cancel_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let cancel_body: serde_json::Value = serde_json::from_slice(&cancel_bytes).unwrap();
    assert_eq!(cancel_body["state"], "canceled");
}

/// TAG-P0-3 follow-up（2026-08-14）: requeue は fingerprint/payload を
/// 一切変更しないため、真のコンフリクト（enqueue 後に対象行が別経路で
/// 変わっている場合）は requeue → 再 apply でも安全に再度 fail し、
/// 対象行を上書きしないことを確認する。
#[tokio::test]
async fn pending_changes_requeue_conflict_still_fails_on_reapply_with_fingerprint_intact() {
    let env = test_env().await;

    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "name": "line-fp-conflict-requeue", "host": "127.0.0.1", "port": 15033 }),
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
                    json!({ "name": "line-fp-conflict-requeue-queued", "host": "127.0.0.1", "port": 15033 })
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

    // 別経路（pending queue を経由しない直接の service 呼び出し）で
    // 同じ行を書き換え、真のコンフリクトを作る。
    PlcConnectionService::new(env.pool.clone())
        .update(
            conn_id,
            PlcConnectionInput {
                name: "line-fp-conflict-requeue-hijacked".to_string(),
                protocol: "modbus-tcp".to_string(),
                host: "127.0.0.1".to_string(),
                port: 15033,
                unit_id: 1,
                enabled: true,
                simulation: false,

                word_order: "low_high".to_string(),
                cpu_type: None,
                database: None,
                username: None,
                password: None,
            },
        )
        .await
        .unwrap();

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

    let first_apply = env
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
    assert_eq!(first_apply.status(), StatusCode::CONFLICT);
    let first_apply_bytes = axum::body::to_bytes(first_apply.into_body(), usize::MAX)
        .await
        .unwrap();
    let first_apply_body: serde_json::Value = serde_json::from_slice(&first_apply_bytes).unwrap();
    assert_eq!(first_apply_body["error"], "pending_apply_conflict");
    assert_eq!(first_apply_body["resource"], "plc_connections");

    let pending_changes = PendingChangesService::new(env.pool.clone());
    let failed = pending_changes.get(pending_id).await.unwrap();
    assert_eq!(failed.state, PendingChangeState::Failed);

    // requeue は成功する（failed -> pending への差し戻しは常に許可）。
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

    // しかし fingerprint/payload は据え置きのままなので、コンフリクトが
    // 解消されていない限り再 apply も同じ理由で再び fail する。
    let second_apply = env
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
    assert_eq!(second_apply.status(), StatusCode::CONFLICT);
    let second_apply_bytes = axum::body::to_bytes(second_apply.into_body(), usize::MAX)
        .await
        .unwrap();
    let second_apply_body: serde_json::Value = serde_json::from_slice(&second_apply_bytes).unwrap();
    assert_eq!(second_apply_body["error"], "pending_apply_conflict");
    assert_eq!(second_apply_body["resource"], "plc_connections");

    let failed_again = pending_changes.get(pending_id).await.unwrap();
    assert_eq!(failed_again.state, PendingChangeState::Failed);

    // 対象行は「別経路」の編集内容のまま(再 apply も拒否されたので
    // 上書きされていない)。
    let untouched = PlcConnectionService::new(env.pool.clone())
        .get(conn_id)
        .await
        .unwrap();
    assert_eq!(untouched.name, "line-fp-conflict-requeue-hijacked");
}

/// TAG-P0-3 follow-up（2026-08-12）: `collection_groups.update` でも
/// 同じ conflict 検出が働く。
#[tokio::test]
async fn pending_apply_collection_group_update_conflicts_when_row_changed_out_of_band() {
    let env = test_env().await;

    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "name": "line-group-fp", "host": "127.0.0.1", "port": 15032 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");
    let conn_id = conn["id"].as_i64().unwrap();

    let (status, group) = admin_post(
        &env.router,
        "/api/collection-groups",
        &env.admin_token,
        json!({ "name": "fast", "plcConnectionId": conn_id, "periodMs": 100 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{group:?}");
    let group_id = group["id"].as_i64().unwrap();

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
            HttpRequest::put(format!("/api/collection-groups/{group_id}"))
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "fast-queued",
                        "plcConnectionId": conn_id,
                        "periodMs": 100
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
    let pending_id = queued_body["pending"]["id"].as_i64().unwrap();

    // 別経路での編集（pending queue を経由しない直接の service 呼び出し）。
    CollectionGroupService::new(env.pool.clone())
        .update(
            group_id,
            CollectionGroupInput {
                name: "fast-hijacked".to_string(),
                plc_connection_id: conn_id,
                period_ms: 100,
                enabled: true,
                default_writable: true,
                query_sql: None,
            },
        )
        .await
        .unwrap();

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
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"], "pending_apply_conflict");
    assert_eq!(body["resource"], "collection_groups");

    let pending = PendingChangesService::new(env.pool.clone())
        .get(pending_id)
        .await
        .unwrap();
    assert_eq!(pending.state, PendingChangeState::Failed);
}

/// TAG-P0-3 follow-up（2026-08-12）、最重要の回帰ガード:
/// フィンガープリントは per-resource でなければならない
/// （グローバル `configured_revision` 比較に「こっそり」置き換えられて
/// いないことを保証する）。無関係な2件の pending change（別々の
/// `plc_connections` 行）を running 中に queue し、片方を適用して
/// `commit_catalog_and_notify` にグローバル revision を進めさせた後、
/// もう片方の適用が「global revision が動いた」ことに巻き込まれず
/// 成功することを確認する。
#[tokio::test]
async fn pending_apply_fingerprint_is_per_resource_not_global_revision() {
    let env = test_env().await;

    let (status, conn_a) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "name": "line-a", "host": "127.0.0.1", "port": 15040 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn_a:?}");
    let conn_a_id = conn_a["id"].as_i64().unwrap();

    let (status, conn_b) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "name": "line-b", "host": "127.0.0.1", "port": 15041 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn_b:?}");
    let conn_b_id = conn_b["id"].as_i64().unwrap();

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

    // A・B それぞれ独立に pending update を queue する。
    let queue_update = |id: i64, name: &'static str, port: i64| {
        let router = env.router.clone();
        let token = env.admin_token.clone();
        async move {
            let response = router
                .oneshot(
                    HttpRequest::put(format!("/api/plc-connections/{id}"))
                        .header("Authorization", format!("Bearer {token}"))
                        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                        .header("content-type", "application/json")
                        .body(Body::from(
                            json!({ "name": name, "host": "127.0.0.1", "port": port }).to_string(),
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
            body["pending"]["id"].as_i64().unwrap()
        }
    };
    let pending_a_id = queue_update(conn_a_id, "line-a-renamed", 15040).await;
    let pending_b_id = queue_update(conn_b_id, "line-b-renamed", 15041).await;

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

    // A を先に適用する - 成功すれば `commit_catalog_and_notify` が
    // グローバル `configured_revision` を進める。
    let apply_a = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post(format!("/api/pending-changes/{pending_a_id}/apply"))
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(apply_a.status(), StatusCode::OK);
    let apply_a_bytes = axum::body::to_bytes(apply_a.into_body(), usize::MAX)
        .await
        .unwrap();
    let apply_a_body: serde_json::Value = serde_json::from_slice(&apply_a_bytes).unwrap();
    assert_eq!(apply_a_body["state"], "applied");

    // B の適用は、A の適用でグローバル revision が進んだことに巻き込ま
    // れず成功しなければならない - B のフィンガープリントは B 自身の
    // 行に対して取られており、A の変更とは無関係だから(per-resource
    // ガードの核心)。
    let apply_b = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post(format!("/api/pending-changes/{pending_b_id}/apply"))
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let apply_b_bytes_status = apply_b.status();
    let apply_b_bytes = axum::body::to_bytes(apply_b.into_body(), usize::MAX)
        .await
        .unwrap();
    let apply_b_body: serde_json::Value = serde_json::from_slice(&apply_b_bytes).unwrap();
    assert_eq!(
        apply_b_bytes_status,
        StatusCode::OK,
        "B's apply must not be rejected by A's global revision bump: {apply_b_body:?}"
    );
    assert_eq!(apply_b_body["state"], "applied");

    let updated_a = PlcConnectionService::new(env.pool.clone())
        .get(conn_a_id)
        .await
        .unwrap();
    assert_eq!(updated_a.name, "line-a-renamed");
    let updated_b = PlcConnectionService::new(env.pool.clone())
        .get(conn_b_id)
        .await
        .unwrap();
    assert_eq!(updated_b.name, "line-b-renamed");
}

// --- 監査ログ retention 設定 (docs/banto-hub-remaining-plan.md P3-a) ----

/// `GET/PUT /api/audit-log/config`: admin 限定（viewer は403）、既定値
/// （90日/100,000件）、保存した値の round-trip、PUT が
/// `settings`リソースへの`settings_change`監査エントリを1件だけ記録する
/// （I3' で banto の `audit_log_router` に替えてからの語彙）ことを1本の
/// テストで固定する（`collection_control_requires_admin_and_csrf_and_is_idempotent`
/// と同じ「1テストで RBAC + 挙動 + 監査を通して確認する」形）。
#[tokio::test]
async fn audit_log_config_round_trips_and_requires_admin() {
    let env = test_env().await;

    let viewer_get = HttpRequest::builder()
        .method("GET")
        .uri("/api/audit-log/config")
        .header("Authorization", format!("Bearer {}", env.viewer_token))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .body(Body::empty())
        .unwrap();
    let response = env.router.clone().oneshot(viewer_get).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let admin_get = HttpRequest::builder()
        .method("GET")
        .uri("/api/audit-log/config")
        .header("Authorization", format!("Bearer {}", env.admin_token))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .body(Body::empty())
        .unwrap();
    let response = env.router.clone().oneshot(admin_get).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["retentionDays"], 90);
    assert_eq!(body["retentionRows"], 100_000);

    let put_body = || {
        Body::from(
            serde_json::to_vec(&serde_json::json!({
                "retentionDays": 30,
                "retentionRows": 5000
            }))
            .unwrap(),
        )
    };

    let viewer_put = HttpRequest::builder()
        .method("PUT")
        .uri("/api/audit-log/config")
        .header("Authorization", format!("Bearer {}", env.viewer_token))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .header("content-type", "application/json")
        .body(put_body())
        .unwrap();
    let response = env.router.clone().oneshot(viewer_put).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let admin_put = HttpRequest::builder()
        .method("PUT")
        .uri("/api/audit-log/config")
        .header("Authorization", format!("Bearer {}", env.admin_token))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .header("content-type", "application/json")
        .body(put_body())
        .unwrap();
    let response = env.router.clone().oneshot(admin_put).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let applied: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(applied["retentionDays"], 30);
    assert_eq!(applied["retentionRows"], 5000);

    let refetch = HttpRequest::builder()
        .method("GET")
        .uri("/api/audit-log/config")
        .header("Authorization", format!("Bearer {}", env.admin_token))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .body(Body::empty())
        .unwrap();
    let response = env.router.clone().oneshot(refetch).await.unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let refetched: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(refetched["retentionDays"], 30);
    assert_eq!(refetched["retentionRows"], 5000);

    // I3'（banto の `audit_log_router`）: 設定の読み書きは `settings`
    // リソース。viewer の拒否 2 件（GET・PUT）と、保存の
    // `settings_change` 1 件（`entityId` なし）。以前の写しは
    // `update`/`audit_log_config`/`"1"` で、拒否は `audit_log` だった。
    let rows: Vec<(String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT action, resource, result, entity_id FROM audit_log              WHERE resource IN ('settings', 'audit_log', 'audit_log_config') ORDER BY id",
    )
    .fetch_all(&env.pool)
    .await
    .unwrap();
    let denied = (
        "denied".to_string(),
        "settings".to_string(),
        "denied".to_string(),
        None,
    );
    assert_eq!(
        rows,
        vec![
            denied.clone(),
            denied,
            (
                "settings_change".to_string(),
                "settings".to_string(),
                "ok".to_string(),
                None
            ),
        ]
    );
}

/// `POST /api/audit-log/list`が読む前に retention 設定に従って
/// opportunistic に剪定すること（`crate::audit::AuditLogService::prune`
/// の配線そのもの、docs/banto-hub-remaining-plan.md P3-a）を、REST
/// エンドポイント越しに確認する - `crate::audit`の単体テストは
/// `prune`自体の正しさを担保済みなので、ここでは「呼ばれること」だけを
/// 見る。
#[tokio::test]
async fn audit_log_list_opportunistically_prunes_by_configured_retention() {
    let env = test_env().await;

    for i in 0..5 {
        sqlx::query(
            "INSERT INTO audit_log (actor_username, actor_role, action, resource, entity_id, detail, origin, result) \
             VALUES ('admin', 'admin', 'create', 'items', ?, NULL, 'rest', 'ok')",
        )
        .bind(i.to_string())
        .execute(&env.pool)
        .await
        .unwrap();
    }

    let put = HttpRequest::builder()
        .method("PUT")
        .uri("/api/audit-log/config")
        .header("Authorization", format!("Bearer {}", env.admin_token))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "retentionDays": null,
                "retentionRows": 2
            }))
            .unwrap(),
        ))
        .unwrap();
    let response = env.router.clone().oneshot(put).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // PUT 自体も1件 audit_log 行を作るため、この時点で 5(シード) + 1(PUT)
    // = 6件。retentionRows=2 で list を叩くと、剪定後は最新2件のみ残る。
    let list = HttpRequest::builder()
        .method("POST")
        .uri("/api/audit-log/list")
        .header("Authorization", format!("Bearer {}", env.admin_token))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({})).unwrap(),
        ))
        .unwrap();
    let response = env.router.clone().oneshot(list).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["totalCount"], 2);

    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(remaining, 2);
}

/// #428: `asOfId` 付きの取得（2 ブロック目以降）では剪定しない - 上限に
/// 張り付いた状態でも、読み込みの途中で行が消えて失効しない。`asOfId`
/// なし（世代の最初・再読み込み）では従来どおり剪定する。
#[tokio::test]
async fn audit_log_list_with_as_of_id_does_not_prune() {
    let env = test_env().await;
    for i in 0..5 {
        sqlx::query(
            "INSERT INTO audit_log (actor_username, actor_role, action, resource, entity_id, detail, origin, result) \
             VALUES ('admin', 'admin', 'create', 'items', ?, NULL, 'rest', 'ok')",
        )
        .bind(i.to_string())
        .execute(&env.pool)
        .await
        .unwrap();
    }
    // 保持件数の上限を 2 にする（`PUT` も 1 行記録する = 6 行、上限超過）。
    let put = HttpRequest::builder()
        .method("PUT")
        .uri("/api/audit-log/config")
        .header("Authorization", format!("Bearer {}", env.admin_token))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "retentionDays": null,
                "retentionRows": 2
            }))
            .unwrap(),
        ))
        .unwrap();
    let response = env.router.clone().oneshot(put).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let count = || async {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM audit_log")
            .fetch_one(&env.pool)
            .await
            .unwrap()
    };
    let before = count().await;
    assert_eq!(before, 6);
    let max_id: i64 = sqlx::query_scalar("SELECT MAX(id) FROM audit_log")
        .fetch_one(&env.pool)
        .await
        .unwrap();

    let list = |uri: String| {
        HttpRequest::builder()
            .method("POST")
            .uri(uri)
            .header("Authorization", format!("Bearer {}", env.admin_token))
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({})).unwrap(),
            ))
            .unwrap()
    };

    // `asOfId` 付き: 上限を超えていても消さない（件数も境界の集合のまま）。
    let response = env
        .router
        .clone()
        .oneshot(list(format!("/api/audit-log/list?asOfId={max_id}")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["totalCount"], 6, "{body}");
    assert_eq!(count().await, before, "asOfId 付きの取得で剪定された");

    // `asOfId` なし: 従来どおり剪定する。
    let response = env
        .router
        .clone()
        .oneshot(list("/api/audit-log/list".to_string()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(count().await, 2);
}

/// `?asOfId=`（#428）: 省略すると従来どおり全行 + 応答の `asOfId` に最大
/// `id`、指定するとその境界より後の行は件数にも行にも入らない。床は
/// `admin` のまま（クエリを付けても editor/viewer は 403）。
#[tokio::test]
async fn audit_log_list_as_of_id_pins_the_snapshot() {
    let env = test_env().await;
    let audit = AuditLogService::new(Db::Sqlite(env.pool.clone()));
    let entry = |action: &'static str| AuditEntry {
        actor_username: Some("admin"),
        actor_role: Some("admin"),
        action,
        resource: "items",
        entity_id: None,
        detail: None,
        origin: "rest",
        result: "ok",
    };
    for action in ["a", "b", "c"] {
        audit.try_record(entry(action)).await.unwrap();
    }

    let list = |uri: String, token: String| {
        HttpRequest::builder()
            .method("POST")
            .uri(uri)
            .header("Authorization", format!("Bearer {token}"))
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({})).unwrap(),
            ))
            .unwrap()
    };
    let call = |request: HttpRequest<Body>| {
        let router = env.router.clone();
        async move {
            let response = router.oneshot(request).await.unwrap();
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            (
                status,
                serde_json::from_slice::<serde_json::Value>(&bytes)
                    .unwrap_or(serde_json::Value::Null),
            )
        }
    };

    let (status, first) = call(list(
        "/api/audit-log/list".to_string(),
        env.admin_token.clone(),
    ))
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let as_of_id = first["asOfId"].as_i64().expect("asOfId が返ること");
    let first_total = first["totalCount"].as_u64().unwrap();
    let max_id = first["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_i64().unwrap())
        .max()
        .unwrap();
    assert_eq!(as_of_id, max_id, "省略時の境界は最大 id: {first}");
    assert_eq!(first["rows"].as_array().unwrap().len() as u64, first_total);

    audit.try_record(entry("after")).await.unwrap();

    let (status, pinned) = call(list(
        format!("/api/audit-log/list?asOfId={as_of_id}"),
        env.admin_token.clone(),
    ))
    .await;
    assert_eq!(status, StatusCode::OK, "{pinned}");
    assert_eq!(pinned["asOfId"], as_of_id);
    assert_eq!(pinned["totalCount"].as_u64().unwrap(), first_total);
    assert!(
        pinned["rows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["action"] != "after"),
        "境界より後の行が混ざった: {pinned}"
    );

    let (_, fresh) = call(list(
        "/api/audit-log/list".to_string(),
        env.admin_token.clone(),
    ))
    .await;
    assert_eq!(fresh["totalCount"].as_u64().unwrap(), first_total + 1);
    assert!(fresh["asOfId"].as_i64().unwrap() > as_of_id);

    // 床は admin のまま: editor（ここで作ってログイン）と viewer は 403。
    UsersService::new(Db::Sqlite(env.pool.clone()))
        .create_user("editor1", "password123", "編集者", Role::Editor)
        .await
        .expect("create_user editor");
    let (status, login) = call(
        HttpRequest::builder()
            .method("POST")
            .uri("/api/auth/login")
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({ "username": "editor1", "password": "password123" }))
                    .unwrap(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{login}");
    let editor_token = login["token"].as_str().unwrap().to_string();
    for token in [editor_token, env.viewer_token.clone()] {
        let (status, body) = call(list(
            format!("/api/audit-log/list?asOfId={as_of_id}"),
            token,
        ))
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    }
}

// --- T19 S2-d (docs/banto-hub-t19-design.md §5.1、UX-39): 履歴の保持
// 期間 - GET/PUT /api/store-settings と POST /api/store-settings/
// prune-preview・prune-now -------------------------------------------

/// `env`が使う実際のtstoreデータディレクトリ（`test_manager_with_clock`
/// が`dir.path().join("data")`を`CollectorManager`へ渡している - この
/// テストではファイルを直接置く/確認するために同じパスを再現する）。
fn store_settings_test_data_dir(env: &TestEnv) -> std::path::PathBuf {
    env._dir.path().join("data")
}

fn touch_data_file(data_dir: &std::path::Path, date: LocalDate, seq: u32) -> std::path::PathBuf {
    std::fs::create_dir_all(data_dir).expect("create data dir");
    let path = data_dir.join(format!("{}-{:03}.sqlite3", date.to_yyyymmdd(), seq));
    std::fs::write(&path, b"").expect("touch data file");
    path
}

/// `GET/PUT /api/store-settings`: admin 限定（viewer は403）、既定値
/// （`Some(7)`）、`null`（無制限）を含めた往復、バリデーション範囲
/// （1〜3650、実装指示どおり）を確認する。
#[tokio::test]
async fn store_settings_round_trips_requires_admin_and_validates_range() {
    let env = test_env().await;

    let get = |token: String| {
        let router = env.router.clone();
        async move {
            let request = HttpRequest::builder()
                .method("GET")
                .uri("/api/store-settings")
                .header("Authorization", format!("Bearer {token}"))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap();
            router.oneshot(request).await.unwrap()
        }
    };

    let response = get(env.viewer_token.clone()).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let response = get(env.admin_token.clone()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["retentionDays"], 7);

    let put = |token: String, value: serde_json::Value| {
        let router = env.router.clone();
        async move {
            let request = HttpRequest::builder()
                .method("PUT")
                .uri("/api/store-settings")
                .header("Authorization", format!("Bearer {token}"))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({ "retentionDays": value })).unwrap(),
                ))
                .unwrap();
            router.oneshot(request).await.unwrap()
        }
    };

    // viewer は書けない。
    let response = put(env.viewer_token.clone(), serde_json::json!(30)).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // バリデーション: 0以下・上限超過は422。
    for bad in [
        serde_json::json!(0),
        serde_json::json!(-5),
        serde_json::json!(3651),
    ] {
        let response = put(env.admin_token.clone(), bad.clone()).await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "retentionDays={bad:?} should be rejected"
        );
    }

    // 正常な保存 → 再取得で往復する。
    let response = put(env.admin_token.clone(), serde_json::json!(30)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let applied: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(applied["retentionDays"], 30);

    let response = get(env.admin_token.clone()).await;
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let refetched: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(refetched["retentionDays"], 30);

    // UX-39: 無制限（null）も保存・読み戻しできる。
    let response = put(env.admin_token.clone(), serde_json::Value::Null).await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let applied: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(applied["retentionDays"], serde_json::Value::Null);

    let response = get(env.admin_token.clone()).await;
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let refetched: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(refetched["retentionDays"], serde_json::Value::Null);

    // `resource = 'store_settings'`は成功した更新（action='update'）に
    // 加えて、viewer の403（`require_session_role`が同じ`resource`名で
    // 記録する`action='denied'`エントリ）も含む - ここでは「保存が
    // 何回成功したか」だけを見るため`action='update'`に絞る。
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT action, resource, result FROM audit_log WHERE resource = 'store_settings' AND action = 'update' ORDER BY id",
    )
    .fetch_all(&env.pool)
    .await
    .unwrap();
    // 正常な PUT（30 → null）の2件だけが記録される（バリデーション
    // エラーになった3件・viewer の403は記録されない）。
    assert_eq!(rows.len(), 2);
    for row in rows {
        assert_eq!(
            row,
            (
                "update".to_string(),
                "store_settings".to_string(),
                "ok".to_string()
            )
        );
    }
}

/// オーナー決定1の核心: **`PUT /api/store-settings` は保存するだけで、
/// 履歴ファイルを一切削除しない**。保持日数を1日に絞っても、既存の
/// 古いデータファイルはディスク上に残ることを確認する。
#[tokio::test]
async fn store_settings_put_never_deletes_history_files() {
    let today = LocalDate::new(2026, 7, 12);
    let now_ms = today.to_days_since_epoch() * 86_400_000 + 12 * 3_600_000;
    let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(now_ms, 0));
    let env = test_env_with_clock(clock).await;

    let data_dir = store_settings_test_data_dir(&env);
    let old_file = touch_data_file(&data_dir, LocalDate::new(2026, 6, 1), 1);
    let today_file = touch_data_file(&data_dir, today, 1);

    // 保持日数を1日という厳しい値に変更しても、PUT 自体は削除しない。
    let put = HttpRequest::builder()
        .method("PUT")
        .uri("/api/store-settings")
        .header("Authorization", format!("Bearer {}", env.admin_token))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({ "retentionDays": 1 })).unwrap(),
        ))
        .unwrap();
    let response = env.router.clone().oneshot(put).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    assert!(old_file.exists(), "PUT must not prune the old file");
    assert!(today_file.exists());
}

/// `POST /api/store-settings/prune-preview`・`prune-now`が**保存済みの**
/// 保持方針で動くこと、preview は削除しないこと、prune-now は実際に
/// 削除して監査ログに残すことを確認する（admin 限定も併せて確認）。
#[tokio::test]
async fn store_settings_prune_preview_and_prune_now_use_saved_policy() {
    let today = LocalDate::new(2026, 7, 12);
    let now_ms = today.to_days_since_epoch() * 86_400_000 + 12 * 3_600_000;
    let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(now_ms, 0));
    let env = test_env_with_clock(clock).await;

    let data_dir = store_settings_test_data_dir(&env);
    // 7日保持のとき、30日以上前のファイルは削除対象、当日ファイルは
    // 対象外 - `banto-tstore`の`prune_files`単体テストと同じ境界判定。
    let old_file = touch_data_file(&data_dir, LocalDate::new(2026, 6, 1), 1);
    let today_file = touch_data_file(&data_dir, today, 1);

    let put = HttpRequest::builder()
        .method("PUT")
        .uri("/api/store-settings")
        .header("Authorization", format!("Bearer {}", env.admin_token))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({ "retentionDays": 7 })).unwrap(),
        ))
        .unwrap();
    let response = env.router.clone().oneshot(put).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // viewer は preview/prune-now どちらも403。
    for path in [
        "/api/store-settings/prune-preview",
        "/api/store-settings/prune-now",
    ] {
        let request = HttpRequest::builder()
            .method("POST")
            .uri(path)
            .header("Authorization", format!("Bearer {}", env.viewer_token))
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .body(Body::empty())
            .unwrap();
        let response = env.router.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "path={path}");
    }

    // preview は件数を返すだけで削除しない。
    let preview = HttpRequest::builder()
        .method("POST")
        .uri("/api/store-settings/prune-preview")
        .header("Authorization", format!("Bearer {}", env.admin_token))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .body(Body::empty())
        .unwrap();
    let response = env.router.clone().oneshot(preview).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["wouldDeleteCount"], 1);
    assert!(old_file.exists(), "preview must not delete anything");

    // prune-now は実際に削除し、削除数を返し、監査ログに残す。
    let prune_now = HttpRequest::builder()
        .method("POST")
        .uri("/api/store-settings/prune-now")
        .header("Authorization", format!("Bearer {}", env.admin_token))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .body(Body::empty())
        .unwrap();
    let response = env.router.clone().oneshot(prune_now).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["deletedCount"], 1);
    assert!(
        !old_file.exists(),
        "prune-now must delete the aged-out file"
    );
    assert!(today_file.exists(), "today's file must survive");

    let rows: Vec<(String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT action, resource, result, detail FROM audit_log WHERE resource = 'store_settings_prune' ORDER BY id",
    )
    .fetch_all(&env.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "delete");
    assert_eq!(rows[0].1, "store_settings_prune");
    assert_eq!(rows[0].2, "ok");
    let detail: serde_json::Value = serde_json::from_str(rows[0].3.as_deref().unwrap()).unwrap();
    assert_eq!(detail["deletedCount"], 1);
}

/// UX-39（無制限の選択肢）: `retentionDays: null`のときは
/// preview/prune-now とも何も削除しない。
#[tokio::test]
async fn store_settings_prune_with_unlimited_policy_deletes_nothing() {
    let today = LocalDate::new(2026, 7, 12);
    let now_ms = today.to_days_since_epoch() * 86_400_000 + 12 * 3_600_000;
    let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(now_ms, 0));
    let env = test_env_with_clock(clock).await;

    let data_dir = store_settings_test_data_dir(&env);
    let old_file = touch_data_file(&data_dir, LocalDate::new(2020, 1, 1), 1);

    let put = HttpRequest::builder()
        .method("PUT")
        .uri("/api/store-settings")
        .header("Authorization", format!("Bearer {}", env.admin_token))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({ "retentionDays": null })).unwrap(),
        ))
        .unwrap();
    let response = env.router.clone().oneshot(put).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let preview = HttpRequest::builder()
        .method("POST")
        .uri("/api/store-settings/prune-preview")
        .header("Authorization", format!("Bearer {}", env.admin_token))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .body(Body::empty())
        .unwrap();
    let response = env.router.clone().oneshot(preview).await.unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["wouldDeleteCount"], 0);

    let prune_now = HttpRequest::builder()
        .method("POST")
        .uri("/api/store-settings/prune-now")
        .header("Authorization", format!("Bearer {}", env.admin_token))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .body(Body::empty())
        .unwrap();
    let response = env.router.clone().oneshot(prune_now).await.unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["deletedCount"], 0);
    assert!(old_file.exists(), "unlimited retention must delete nothing");
}

/// レビュー指摘の安全化: 保持日数が`u32::MAX`を超える値で保存されて
/// いる状態（`parse_retention`は上限クランプをしないため、保存経路を
/// 通らず直接設定された等でこの状態になり得る）で prune-preview /
/// prune-now を叩くと、どちらも削除0件になり（既存ファイルが消えず）、
/// 両者の判定が一致すること（`resolve_prune_retention_days`を両方の
/// ハンドラが共有していることの固定）を確認する。
#[tokio::test]
async fn store_settings_prune_with_out_of_range_retention_days_deletes_nothing() {
    let today = LocalDate::new(2026, 7, 12);
    let now_ms = today.to_days_since_epoch() * 86_400_000 + 12 * 3_600_000;
    let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(now_ms, 0));
    let env = test_env_with_clock(clock).await;

    let data_dir = store_settings_test_data_dir(&env);
    let old_file = touch_data_file(&data_dir, LocalDate::new(2020, 1, 1), 1);

    // `PUT /api/store-settings`のバリデーション（1〜3650）を経由せず、
    // 保存経路そのものへ想定外の巨大値を直接書き込む - レビュー指摘が
    // 想定する「設定が壊れている・DBを直接編集された」状況の再現。
    SettingsService::new(Db::Sqlite(env.pool.clone()))
        .set_store_config(&StoreSettings {
            data_dir: "./data".to_string(),
            retention_days: Some(u32::MAX as i64 + 1),
        })
        .await
        .expect("set_store_config");

    let preview = HttpRequest::builder()
        .method("POST")
        .uri("/api/store-settings/prune-preview")
        .header("Authorization", format!("Bearer {}", env.admin_token))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .body(Body::empty())
        .unwrap();
    let response = env.router.clone().oneshot(preview).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["wouldDeleteCount"], 0);
    assert!(old_file.exists(), "preview must not delete anything");

    let prune_now = HttpRequest::builder()
        .method("POST")
        .uri("/api/store-settings/prune-now")
        .header("Authorization", format!("Bearer {}", env.admin_token))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .body(Body::empty())
        .unwrap();
    let response = env.router.clone().oneshot(prune_now).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["deletedCount"], 0);
    assert!(
        old_file.exists(),
        "out-of-range retention_days must delete nothing, matching the preview"
    );
}

// --- T18-5a 第2段 (docs/banto-hub-t18-design.md §4 決定6): 薄い部品の
// 先行配線 - POST /api/tags/list と GET /api/tags/group-counts --------

/// `admin_post` 経由で PLC接続 → 収集グループ → タグ を1本作る。
/// `group_id` が `None` なら新しい接続+グループも作る、`Some` ならその
/// グループへタグを追加するだけ - 返り値は `(tag_id, group_id)`。
async fn create_tag_via_admin(
    router: &Router,
    token: &str,
    suffix: &str,
    group_id: Option<i64>,
) -> (i64, i64) {
    let group_id = match group_id {
        Some(id) => id,
        None => {
            let (status, conn) = admin_post(
                router,
                "/api/plc-connections",
                token,
                json!({ "name": format!("conn-{suffix}"), "host": "127.0.0.1", "port": 15300 }),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{conn:?}");
            let conn_id = conn["id"].as_i64().unwrap();
            let (status, group) = admin_post(
                router,
                "/api/collection-groups",
                token,
                json!({
                    "name": format!("group-{suffix}"),
                    "plcConnectionId": conn_id,
                    "periodMs": 1000
                }),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{group:?}");
            group["id"].as_i64().unwrap()
        }
    };
    let (status, tag) = admin_post(
        router,
        "/api/tags",
        token,
        json!({
            "name": format!("tag-{suffix}"),
            "collectionGroupId": group_id,
            "address": "D100",
            "dataType": "i16"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{tag:?}");
    (tag["id"].as_i64().unwrap(), group_id)
}

/// ファイル DB の一時パス（テストごとに一意）。
fn temp_db_path(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "banto-hub-{label}-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn remove_db_files(path: &std::path::Path) {
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

/// #528 の回帰テスト共通: `PUT /api/tags/{tag}` を [`TxTestHook`] の下で
/// 始め、ハンドラがトランザクションを始めて**止まっている間**に、別接続
/// （`busy_timeout = 0`）から書き込みを試みる。
///
/// - `BEGIN IMMEDIATE` で始めていれば、ハンドラが書き込みロックを握っているので
///   別接続の `BEGIN IMMEDIATE` は**必ず** `SQLITE_BUSY` で失敗する。
/// - 素の `BEGIN`（DEFERRED）だと、フックの読み取りはスナップショットを張るだけで
///   ロックを握らないので、別接続の書き込みは成功してしまう（テストはここで落ちる。
///   通った場合の後続は、スナップショットが古くなって `UPDATE` が
///   `SQLITE_BUSY_SNAPSHOT`（500）になる実際の失敗の再現でもある）。
///
/// 「待った時間」ではなく「ロックを握っている」という積極的な証拠で判定するので、
/// ハンドラの開始が遅れても結果は変わらない。止めたハンドラを解放して応答を返す。
async fn put_tag_while_probing_the_write_lock(
    env: &TestEnv,
    path: &std::path::Path,
    tag: i64,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let hook = Arc::new(TxTestHook {
        reached: tokio::sync::Notify::new(),
        proceed: tokio::sync::Notify::new(),
    });
    let router = env.router.clone();
    let token = env.admin_token.clone();
    let request = tokio::spawn(TX_TEST_HOOK.scope(hook.clone(), async move {
        admin_put(&router, &format!("/api/tags/{tag}"), &token, body).await
    }));

    // 到達しなければテストの失敗（時間切れは成功の根拠にしない）。
    tokio::time::timeout(std::time::Duration::from_secs(60), hook.reached.notified())
        .await
        .expect("ハンドラがトランザクションを始めてフックへ着くはず");

    // ハンドラは止まっている。別接続（待たない設定）から書き込みを試みる。
    use sqlx::ConnectOptions;
    let mut probe = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(path)
        .busy_timeout(std::time::Duration::ZERO)
        .connect()
        .await
        .expect("connect probe");
    let attempt = sqlx::query("BEGIN IMMEDIATE").execute(&mut probe).await;
    match attempt {
        Err(sqlx::Error::Database(err)) => {
            assert!(
                matches!(err.code().as_deref(), Some("5") | Some("261")),
                "SQLITE_BUSY のはず: {err}"
            );
        }
        other => panic!(
            "ハンドラが書き込みロックを握っているはず（BEGIN IMMEDIATE で始めていれば \
             別接続の BEGIN IMMEDIATE は BUSY になる）: {other:?}"
        ),
    }
    drop(probe);

    hook.proceed.notify_one();
    request.await.expect("join")
}

/// #528: 書き込みロックを握ったまま検証する（`BEGIN IMMEDIATE`）ので
/// `PUT /api/tags/{id}` は 200 になる。素の `BEGIN` だと別接続が割り込めて
/// `database is locked`（REST 500）になりうる。
#[tokio::test]
async fn tags_update_holds_the_write_lock_from_the_start() {
    let path = temp_db_path("tags-update-busy");
    let env = test_env_on_file_db(&path).await;
    let (tag1, group_id) = create_tag_via_admin(&env.router, &env.admin_token, "imm1", None).await;

    let (status, body) = put_tag_while_probing_the_write_lock(
        &env,
        &path,
        tag1,
        json!({
            "name": "tag-imm1-renamed",
            "collectionGroupId": group_id,
            "address": "D100",
            "dataType": "i16"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["name"], "tag-imm1-renamed");

    env.pool.close().await;
    remove_db_files(&path);
}

/// #528: 同じ状況でも、本物の版の食い違いは 500 ではなく 409
/// （`tag_revision_conflict`）で返る。
#[tokio::test]
async fn tags_update_true_revision_conflict_is_409_while_holding_the_write_lock() {
    let path = temp_db_path("tags-update-conflict");
    let env = test_env_on_file_db(&path).await;
    let (tag1, group_id) = create_tag_via_admin(&env.router, &env.admin_token, "cf1", None).await;
    let (_, current) = admin_get(&env.router, &format!("/api/tags/{tag1}"), &env.admin_token).await;
    let stale = current["revision"].as_i64().unwrap() + 100;

    let (status, body) = put_tag_while_probing_the_write_lock(
        &env,
        &path,
        tag1,
        json!({
            "name": "tag-cf1-renamed",
            "collectionGroupId": group_id,
            "address": "D100",
            "dataType": "i16",
            "expectedRevision": stale
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body:?}");
    assert_eq!(body["error"], "tag_revision_conflict", "{body:?}");

    env.pool.close().await;
    remove_db_files(&path);
}

/// (a) 認証なし → 401（CSRF ヘッダは付けている - `admin_routes_require_the_csrf_header`
/// が CSRF 側は別途カバー済みなので、ここでは認証だけを外す）。
#[tokio::test]
async fn tags_list_query_requires_auth() {
    let env = test_env().await;
    let response = env
        .router
        .oneshot(
            HttpRequest::post("/api/tags/list")
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&json!({})).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

/// (b) filter + sort + pagination + totalCount が `TagService::list` に
/// そのまま素通しされること、viewer でも読めること（`GET /api/tags` と
/// 同じ require_auth のみ）、(c) `/api/tags/{id}` が `/api/tags/list` と
/// 衝突せず両方とも正しいハンドラへ解決されることを1本で確認する。
#[tokio::test]
async fn tags_list_query_filters_paginates_and_resolves_beside_tags_id() {
    let env = test_env().await;
    let (tag_a, group_id) = create_tag_via_admin(&env.router, &env.admin_token, "a", None).await;
    let (tag_b, _) = create_tag_via_admin(&env.router, &env.admin_token, "b", Some(group_id)).await;
    let (_tag_c, _) =
        create_tag_via_admin(&env.router, &env.admin_token, "c", Some(group_id)).await;

    let (status, body) = admin_post(
        &env.router,
        "/api/tags/list",
        &env.admin_token,
        json!({
            "filters": [{ "field": "collectionGroupId", "op": "eq", "value": group_id }],
            "sort": [{ "field": "name", "direction": "asc" }],
            "pagination": { "offset": 0, "limit": 2 }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["totalCount"], 3);
    let rows = body["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["name"], "tag-a");
    assert_eq!(rows[1]["name"], "tag-b");

    // viewer（読み取り専用）でも読める - `require_editor` は呼ばない。
    let (status, viewer_body) =
        admin_post(&env.router, "/api/tags/list", &env.viewer_token, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{viewer_body:?}");
    assert_eq!(viewer_body["totalCount"], 3);

    // (c) `/api/tags/{id}` 側も同じルーターで正しく解決される
    // (axum の matchit は静的セグメント `/api/tags/list` を優先するため
    // `{id}: i64` に飲み込まれない - 逆にここでは数値 id が
    // `/api/tags/{id}` へ正しく届くことを確認する)。
    let response = env
        .router
        .clone()
        .oneshot(
            HttpRequest::get(format!("/api/tags/{tag_b}"))
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
    let single: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(single["id"], tag_b);
    assert_eq!(single["name"], "tag-b");

    let _ = tag_a;
}

/// (d) `GET /api/tags/group-counts` がグループ別のタグ件数を返すこと。
/// 200 が返ること自体が (c) の「`/api/tags/{id}` (`{id}: i64`) に
/// 飲み込まれていない」証拠でもある - 飲み込まれていれば `group-counts`
/// は不正な i64 として 400 になる。
#[tokio::test]
async fn tags_group_counts_returns_per_group_totals() {
    let env = test_env().await;
    let (_tag_a1, group1) = create_tag_via_admin(&env.router, &env.admin_token, "ga1", None).await;
    create_tag_via_admin(&env.router, &env.admin_token, "ga2", Some(group1)).await;
    let (_tag_b1, group2) = create_tag_via_admin(&env.router, &env.admin_token, "gb1", None).await;

    let response = env
        .router
        .clone()
        .oneshot(
            HttpRequest::get("/api/tags/group-counts")
                .header("Authorization", format!("Bearer {}", env.viewer_token))
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
    let counts = body.as_array().unwrap();
    let find = |gid: i64| {
        counts
            .iter()
            .find(|c| c["collectionGroupId"] == gid)
            .map(|c| c["tagCount"].as_i64().unwrap())
    };
    assert_eq!(find(group1), Some(2));
    assert_eq!(find(group2), Some(1));
}

// --- T19 S1-b0: GET /api/tags/address-writable (PR #232 レビュー指摘) --
//
// `tags_address_writable`/`AddressWritableResponse`の doc comment が
// 定義する3値の意味論をここで固定する:
//   - `writable: Some(true)`, `area: None`  … 領域制限が「存在しない」
//     （`slmp`/`virtual`）
//   - `writable: Some(bool)`, `area: Some(_)` … `modbus-tcp`でパース成功
//   - `writable: None`, `area: None`         … 判定不能
// 当初案はこれを1つの`null`に統合していたため、"SLMP の既定が永久に
// 効かない"（1と5の混同）と"入力途中の Modbus アドレスで誤って既定
// ON"（3/4と5の混同）の両方の実害を招くところだった - 以下のテストは
// その3値それぞれを区別する。

/// `GET /api/tags/address-writable`をクエリ文字列で叩くヘルパー。
async fn address_writable(
    router: &Router,
    token: &str,
    protocol: &str,
    address: &str,
) -> (StatusCode, serde_json::Value) {
    // テストで使う値はすべて英数字・ハイフン・ドットのみ（`modbus-tcp`・
    // `40001.5`・空文字・`abc`等）で、クエリ文字列としてパーセント
    // エンコード無しでも安全にそのまま埋め込める - 依存を増やしてまで
    // エンコーダを導入する必要はない。
    let uri = format!("/api/tags/address-writable?protocol={protocol}&address={address}");
    let response = router
        .clone()
        .oneshot(
            HttpRequest::get(uri)
                .header("Authorization", format!("Bearer {token}"))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

/// (1) `slmp`は任意のアドレスで `writable: true, area: null`
/// （領域による制限が存在しない）。
#[tokio::test]
async fn slmp_protocol_has_no_area_restriction_regardless_of_address() {
    let env = test_env().await;
    let (status, body) = address_writable(&env.router, &env.admin_token, "slmp", "D100").await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["writable"], serde_json::json!(true), "{body:?}");
    assert!(body["area"].is_null(), "{body:?}");
}

/// (2) `virtual`も同様に `writable: true, area: null`。
#[tokio::test]
async fn virtual_protocol_has_no_area_restriction_regardless_of_address() {
    let env = test_env().await;
    let (status, body) = address_writable(&env.router, &env.admin_token, "virtual", "").await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["writable"], serde_json::json!(true), "{body:?}");
    assert!(body["area"].is_null(), "{body:?}");
}

/// (3) `modbus-tcp` + 書き込み可能領域（holding register `40001`・coil
/// `00001`）は `writable: true` かつ `area`が領域名 - **(5)の
/// `writable: null`（判定不能）と区別できること**が本質。
#[tokio::test]
async fn modbus_writable_areas_report_writable_true_with_area_name() {
    let env = test_env().await;

    let (status, body) =
        address_writable(&env.router, &env.admin_token, "modbus-tcp", "40001").await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["writable"], serde_json::json!(true), "{body:?}");
    assert_eq!(
        body["area"],
        serde_json::json!("holding_register"),
        "{body:?}"
    );

    let (status, body) =
        address_writable(&env.router, &env.admin_token, "modbus-tcp", "00001").await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["writable"], serde_json::json!(true), "{body:?}");
    assert_eq!(body["area"], serde_json::json!("coil"), "{body:?}");
}

/// (4) `modbus-tcp` + 読み取り専用領域（discrete input `10001`・input
/// register `30001`）は `writable: false` かつ `area`が領域名 - **(5)の
/// `writable: null`（判定不能）と絶対に混同してはならない**（`false`と
/// `null`の区別がこのエンドポイントの存在意義そのもの）。
#[tokio::test]
async fn modbus_read_only_areas_report_writable_false_not_null_with_area_name() {
    let env = test_env().await;

    let (status, body) =
        address_writable(&env.router, &env.admin_token, "modbus-tcp", "10001").await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["writable"], serde_json::json!(false), "{body:?}");
    assert_eq!(
        body["area"],
        serde_json::json!("discrete_input"),
        "{body:?}"
    );

    let (status, body) =
        address_writable(&env.router, &env.admin_token, "modbus-tcp", "30001").await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["writable"], serde_json::json!(false), "{body:?}");
    assert_eq!(
        body["area"],
        serde_json::json!("input_register"),
        "{body:?}"
    );
}

/// (5) `modbus-tcp` + パース不能（空文字・`abc`・桁数不正）は
/// `writable: null, area: null`（判定不能）- **(1)の`writable: true`
/// とも(4)の`writable: false`とも異なる第三の値**であることを固定する。
/// 入力途中のアドレス文字列でこれを`false`扱いすると誤って書き込み
/// 拒否UIになり、`true`扱いすると読み取り専用領域へ誤って既定ONに
/// なるところだった（レビュー指摘の核心）。
#[tokio::test]
async fn modbus_unparseable_address_reports_null_writable_and_null_area() {
    let env = test_env().await;

    for address in ["", "abc", "4001", "400001234567"] {
        let (status, body) =
            address_writable(&env.router, &env.admin_token, "modbus-tcp", address).await;
        assert_eq!(status, StatusCode::OK, "address={address:?} body={body:?}");
        assert!(
            body["writable"].is_null(),
            "address={address:?} body={body:?}"
        );
        assert!(body["area"].is_null(), "address={address:?} body={body:?}");
    }
}

/// (6) 未知の protocol は判定不能 - `writable: null, area: null`。
/// SLMP/virtual/modbus-tcp のいずれでもない値は将来プロトコルの
/// 追加候補であり、安全側（既定を適用しない）へ倒れる。
#[tokio::test]
async fn unknown_protocol_reports_null_writable_and_null_area() {
    let env = test_env().await;
    let (status, body) = address_writable(
        &env.router,
        &env.admin_token,
        "some-future-protocol",
        "40001",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert!(body["writable"].is_null(), "{body:?}");
    assert!(body["area"].is_null(), "{body:?}");
}

/// 認可: `require_editor`ではなく`require_auth`のみを要求する意図的な
/// 設計（読み取り専用・副作用なし）なので、viewer でも 200 で呼べる
/// ことを固定する。
#[tokio::test]
async fn viewer_role_can_call_address_writable() {
    let env = test_env().await;
    let (status, body) =
        address_writable(&env.router, &env.viewer_token, "modbus-tcp", "40001").await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["writable"], serde_json::json!(true), "{body:?}");
    assert_eq!(
        body["area"],
        serde_json::json!("holding_register"),
        "{body:?}"
    );
}

/// bit接尾辞（T8 §6.1、`banto_plc::address::Address::parse`）: レジスタ
/// 領域（holding/input register）では`.N`が受理され、`area`は bit を
/// 無視してレジスタ自体の領域を返す - `AddressWritableResponse`は bit
/// 位置を運ばない（doc comment参照）ので、`.5`の有無で判定結果が
/// 変わらないことを固定する。
#[tokio::test]
async fn modbus_bit_suffix_on_register_area_is_accepted_and_reports_register_area() {
    let env = test_env().await;

    let (status, body) =
        address_writable(&env.router, &env.admin_token, "modbus-tcp", "30001.5").await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["writable"], serde_json::json!(false), "{body:?}");
    assert_eq!(
        body["area"],
        serde_json::json!("input_register"),
        "{body:?}"
    );

    let (status, body) =
        address_writable(&env.router, &env.admin_token, "modbus-tcp", "40001.3").await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["writable"], serde_json::json!(true), "{body:?}");
    assert_eq!(
        body["area"],
        serde_json::json!("holding_register"),
        "{body:?}"
    );
}

/// bit接尾辞をコイル/discrete input（既にビット粒度）へ付けるのは
/// `Address::parse`が拒否する（"すでにビット粒度の領域への冗長な`.N`は
/// 無音で受理せず、未知エリアと同様に拒否する"という`parse`自身の doc
/// comment）ので、このエンドポイントでは判定不能（`null`/`null`）に
/// フォールバックすることを固定する。
#[tokio::test]
async fn modbus_bit_suffix_on_coil_area_is_rejected_as_unparseable() {
    let env = test_env().await;
    let (status, body) =
        address_writable(&env.router, &env.admin_token, "modbus-tcp", "00001.0").await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert!(body["writable"].is_null(), "{body:?}");
    assert!(body["area"].is_null(), "{body:?}");
}

// --- 試運転モードとロックダウン (設計 §5.6・2026-08-30 オーナー決定、
// banto v3.0.0 ADR-0017 で grant 方式に、2026-10-04) --------------------

/// loopback の peer。`tower::oneshot` には `ConnectInfo` が無いので、本番の
/// `banto_server::start` の経路と同じものを extensions に明示する（banto の
/// `rest/tests.rs` と同じ手筋）。
const LOOPBACK_PEER: std::net::SocketAddr =
    std::net::SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 40000);
const LAN_PEER: std::net::SocketAddr = std::net::SocketAddr::new(
    std::net::IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 20)),
    40000,
);

/// `POST /api/auth/grant/commissioning` を `peer` から叩く。
async fn grant_request(
    router: &Router,
    peer: Option<std::net::SocketAddr>,
) -> (StatusCode, serde_json::Value) {
    let mut request = HttpRequest::post("/api/auth/grant/commissioning")
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .body(Body::empty())
        .unwrap();
    if let Some(peer) = peer {
        request
            .extensions_mut()
            .insert(axum::extract::ConnectInfo(peer));
    }
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

/// 試運転モードの loopback の呼び出しが受け取る grant トークン。
async fn commissioning_grant(router: &Router) -> String {
    let (status, body) = grant_request(router, Some(LOOPBACK_PEER)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["success"], true, "{body}");
    body["token"].as_str().expect("grant token").to_string()
}

/// `GET /api/auth/status` を `peer` から叩いたときの `grants.commissioning`。
async fn commissioning_availability(
    router: &Router,
    peer: Option<std::net::SocketAddr>,
) -> serde_json::Value {
    let mut request = HttpRequest::get("/api/auth/status")
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .body(Body::empty())
        .unwrap();
    if let Some(peer) = peer {
        request
            .extensions_mut()
            .insert(axum::extract::ConnectInfo(peer));
    }
    let response = router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    body["grants"]["commissioning"].clone()
}

/// `Authorization` 無しで管理 REST を叩いたときの status。
async fn unauthenticated_status(router: &Router, path: &str) -> StatusCode {
    router
        .clone()
        .oneshot(
            HttpRequest::get(path)
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

/// 試運転モード（未ロックダウン）でも、管理 REST は `Authorization` 無しでは
/// 401（banto v3.0.0 追従で「無認証で通す」分岐は無くなった）。代わりに
/// loopback の peer は `POST /api/auth/grant/commissioning` で合成 admin の
/// grant を受け取り、それで `GET /api/users`（`RoleGuard` で admin 限定）が
/// 通る。`GET /api/auth/identity` はその identity を `kind: "commissioning"`
/// 付きで返す（画面の `SessionKind` の由来）。
#[tokio::test]
async fn commissioning_mode_requires_the_grant_instead_of_passing_everyone() {
    let env = test_env_unlocked().await;
    assert_eq!(
        unauthenticated_status(&env.router, "/api/users").await,
        StatusCode::UNAUTHORIZED,
        "no unauthenticated admin request passes, even while commissioning"
    );

    let token = commissioning_grant(&env.router).await;
    let (status, users) = admin_get(&env.router, "/api/users", &token).await;
    assert_eq!(status, StatusCode::OK, "{users:?}");

    let (status, identity) = admin_get(&env.router, "/api/auth/identity", &token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(identity["kind"], "commissioning", "{identity}");
    assert_eq!(identity["id"], crate::commissioning::SYNTHETIC_ACTOR_ID);
    assert_eq!(identity["role"], "admin");
}

/// 発行の条件（ADR-0017 §2、`commissioning_grant_spec`）: loopback の peer
/// だけ。LAN の peer・peer 不明は 403（fail closed）。`GET /api/auth/status`
/// の `grants.commissioning` も同じ判定。ロックダウン済みなら loopback でも
/// 403 で、status も false。
#[tokio::test]
async fn the_commissioning_grant_is_loopback_only_and_closed_once_locked_down() {
    let env = test_env_unlocked().await;
    assert_eq!(
        commissioning_availability(&env.router, Some(LOOPBACK_PEER)).await,
        true
    );
    assert_eq!(
        commissioning_availability(&env.router, Some(LAN_PEER)).await,
        false
    );
    assert_eq!(commissioning_availability(&env.router, None).await, false);
    let (status, _) = grant_request(&env.router, Some(LAN_PEER)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = grant_request(&env.router, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // 登録されていない kind は 404（閲覧公開はこの Hub に無い）。
    let response = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post("/api/auth/grant/publicViewer")
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let locked = test_env().await;
    assert_eq!(
        commissioning_availability(&locked.router, Some(LOOPBACK_PEER)).await,
        false
    );
    let (status, _) = grant_request(&locked.router, Some(LOOPBACK_PEER)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// 対照実験: ロックダウン済み（既存の共有テスト環境 `test_env()`）では
/// 従来どおり `Authorization` ヘッダ無しの管理 API アクセスが 401 になる
/// ことを明示的に固定する。
#[tokio::test]
async fn locked_down_admin_api_requires_a_token() {
    let env = test_env().await;
    assert_eq!(
        unauthenticated_status(&env.router, "/api/users").await,
        StatusCode::UNAUTHORIZED
    );
}

/// `GET /api/commissioning/status` は試運転モード中でも
/// `Authorization` ヘッダ無しで読める必要がある（bootstrap と画面が、
/// grant を取る前に状態を知るため）。
#[tokio::test]
async fn unlocked_commissioning_status_is_readable_without_any_token() {
    let env = test_env_unlocked().await;
    let response = env
        .router
        .oneshot(
            HttpRequest::get("/api/commissioning/status")
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
    assert_eq!(body["lockedDown"], false);
}

/// 同じ読み取り専用ステータスは、ロックダウン済みでも同様に
/// `Authorization` ヘッダ無しで読める。
#[tokio::test]
async fn locked_down_commissioning_status_is_still_readable_without_a_token() {
    let env = test_env().await;
    let response = env
        .router
        .oneshot(
            HttpRequest::get("/api/commissioning/status")
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
    assert_eq!(body["lockedDown"], true);
}

/// `POST /api/commissioning/lock-down`（設計 §5.6「遷移」の唯一の正方向
/// 経路）を試運転の grant 自身で叩く。成功すると状態がロックダウン済みへ
/// 切り替わり、`CommissioningService::lock_down` が保存の直後に試運転 grant
/// のトークンを失効させる（ADR-0017 §5）ので、**同じ grant での次の要求は
/// 401**、`GET /api/auth/identity` は `200 null`。監査には actor
/// `commissioning`（ロックダウンの前に確定した）と `revokedGrants` が残る。
#[tokio::test]
async fn commissioning_lock_down_flips_state_and_revokes_the_grant() {
    let env = test_env_unlocked().await;
    let token = commissioning_grant(&env.router).await;
    let other = commissioning_grant(&env.router).await;

    let (status, body) = admin_post(
        &env.router,
        "/api/commissioning/lock-down",
        &token,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["lockedDown"], true);

    for revoked in [&token, &other] {
        let (status, _) = admin_get(&env.router, "/api/users", revoked).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, identity) = admin_get(&env.router, "/api/auth/identity", revoked).await;
        assert_eq!(status, StatusCode::OK);
        assert!(identity.is_null(), "{identity}");
    }
    // アカウントのセッションは巻き込まない。
    let (status, _) = admin_get(&env.router, "/api/users", &env.admin_token).await;
    assert_eq!(status, StatusCode::OK);
    // 以後 grant は出ない。
    let (status, _) = grant_request(&env.router, Some(LOOPBACK_PEER)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (actor, detail): (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT actor_username, detail FROM audit_log WHERE action = 'lock_down' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(
        actor.as_deref(),
        Some(crate::commissioning::SYNTHETIC_ACTOR_ID)
    );
    let detail: serde_json::Value = serde_json::from_str(&detail.expect("detail")).unwrap();
    assert_eq!(detail["revokedGrants"], 2, "{detail}");
}

/// 実装指示「管理者0件でロックダウンしようとするとエラー」の REST
/// 経由での確認。既存の唯一の admin アカウントを editor へ降格させた
/// 上で（生の SQL 経由 - `UsersService::update_user`自身の「最後の
/// admin を降格できない」ガードを迂回する必要がある）、ロックダウンを
/// 試みると明確なエラー（4xx、`ApiError` 形）になり、状態も試運転モードの
/// まま変わらず、**grant も失効しない**（失敗の経路でフラグも失効も
/// 動かさない）ことを確認する。
#[tokio::test]
async fn commissioning_lock_down_fails_without_any_admin_account() {
    let env = test_env_unlocked().await;
    sqlx::query("UPDATE users SET role = 'editor' WHERE username = 'admin'")
        .execute(&env.pool)
        .await
        .expect("downgrade the only admin via raw SQL");
    let token = commissioning_grant(&env.router).await;

    let (status, _) = admin_post(
        &env.router,
        "/api/commissioning/lock-down",
        &token,
        json!({}),
    )
    .await;
    assert!(
        status.is_client_error(),
        "expected a 4xx error, got {status}"
    );

    let response = env
        .router
        .clone()
        .oneshot(
            HttpRequest::get("/api/commissioning/status")
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["lockedDown"], false);
    let (status, _) = admin_get(&env.router, "/api/users", &token).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the grant survives a failed lock-down"
    );
}

/// ADR-0017 §3: 試運転の grant セッション（行を持たない）でもユーザーを削除
/// できる（自己削除ガードの行 id 照合だけが外れる）。admin の床と「最後の
/// admin は消せない」はそのまま。
#[tokio::test]
async fn the_commissioning_grant_can_delete_users_but_not_the_last_admin() {
    let env = test_env_unlocked().await;
    let token = commissioning_grant(&env.router).await;
    let (status, users) = admin_get(&env.router, "/api/users", &token).await;
    assert_eq!(status, StatusCode::OK);
    let id_of = |name: &str| {
        users
            .as_array()
            .unwrap()
            .iter()
            .find(|u| u["username"] == name)
            .map(|u| u["id"].as_i64().unwrap())
            .unwrap()
    };
    let viewer = id_of("viewer1");
    let admin = id_of("admin");

    // 最後の admin のガードは `BantoError::Other`（従来どおり 500 の形）。
    let (status, body) = admin_delete(&env.router, &format!("/api/users/{admin}"), &token).await;
    assert_ne!(
        status,
        StatusCode::NO_CONTENT,
        "the last admin stays: {body}"
    );
    assert!(!status.is_success(), "{status} {body}");
    let (_, after) = admin_get(&env.router, "/api/users", &token).await;
    assert!(
        after
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u["username"] == "admin"),
        "{after}"
    );
    let (status, _) = admin_delete(&env.router, &format!("/api/users/{viewer}"), &token).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

/// 競合の統合テスト（ADR-0017 §2「発行と失効の直列化」、移行 PR の必須項目）:
/// `enabled()` が true を返した発行要求を**挿入の直前で保留** → ロックダウン
/// （保存 → `revoke_grant_tokens`）が完了 → 発行要求を再開 → **403**。直後の
/// `GET /api/auth/identity`（先に発行されていたトークン）は `200 null`、
/// 認証が必要な `GET /api/users` は 401、status の `grants.commissioning` は
/// false。保留は `GrantSpec.enabled` を包んで「判定の結果を返す前に待つ」
/// ことで作る（判定と挿入のあいだに失効が割り込む形）。開いていたストリームが
/// 再検証で閉じることは `tests/stream.rs` の
/// `a_commissioning_grant_stream_closes_within_an_interval_after_lock_down`
/// （実 TCP が要る）が同じ失効で固定する。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_issuance_parked_before_the_insert_loses_to_a_lock_down_that_completed_meanwhile() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let pool = migrate_memory().await.expect("migrate_memory");
    let users = UsersService::new(Db::Sqlite(pool.clone()));
    let audit = AuditLogService::new(Db::Sqlite(pool.clone()));
    users
        .setup_first_user("admin", "password123", "管理者")
        .await
        .expect("setup_first_user");
    let auth = user_auth_state(users.clone(), audit.clone());
    let settings = SettingsService::new(Db::Sqlite(pool.clone()));
    let commissioning = CommissioningService::load(settings, users.clone(), auth.clone())
        .await
        .expect("CommissioningService::load");

    // 本物の試運転 grant の仕様を包む: `park` が立っている間、判定（true）を
    // 返す前に `judged` を鳴らして `release` を待つ。
    let park = Arc::new(AtomicBool::new(false));
    let judged = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mut spec = commissioning.grant_spec();
    let inner = spec.enabled.clone();
    spec.enabled = {
        let park = park.clone();
        let judged = judged.clone();
        let release = release.clone();
        Arc::new(move || {
            let judgment = inner();
            let park = park.clone();
            let judged = judged.clone();
            let release = release.clone();
            Box::pin(async move {
                let verdict = judgment.await;
                if park.load(Ordering::SeqCst) {
                    judged.notify_one();
                    release.notified().await;
                }
                verdict
            })
        })
    };
    let mut registry = GrantRegistry::new();
    registry.register(spec).expect("register");
    let router = extra_auth_router(
        users.clone(),
        auth.clone(),
        audit.clone(),
        false,
        Arc::new(registry),
        None,
    )
    .merge(auth_routes(auth.clone()))
    .merge(users_router(users, audit, auth.clone()));

    // 先に 1 つ発行しておく（ロックダウンで失効する対象）。
    let earlier = commissioning_grant(&router).await;
    assert!((spec_enabled_probe(&router).await), "still commissioning");

    // 保留する発行要求 B。
    park.store(true, Ordering::SeqCst);
    let parked_router = router.clone();
    let parked =
        tokio::spawn(async move { grant_request(&parked_router, Some(LOOPBACK_PEER)).await });
    judged.notified().await; // B は true を取得し、挿入の直前で止まっている。

    // ロックダウン完了（保存 → 失効）。
    let revoked = commissioning.lock_down().await.expect("lock_down");
    assert_eq!(revoked, 1);
    park.store(false, Ordering::SeqCst);
    release.notify_one();

    let (status, body) = parked.await.unwrap();
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "the parked issuance must not insert after the revocation: {body}"
    );
    assert!(body["token"].is_null(), "{body}");
    assert!(auth.identity_for(&earlier).is_none());

    let (status, identity) = admin_get(&router, "/api/auth/identity", &earlier).await;
    assert_eq!(status, StatusCode::OK);
    assert!(identity.is_null(), "{identity}");
    let (status, _) = admin_get(&router, "/api/users", &earlier).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        commissioning_availability(&router, Some(LOOPBACK_PEER)).await,
        false
    );
}

/// `GET /api/auth/status` の `grants.commissioning`（loopback）を bool で。
async fn spec_enabled_probe(router: &Router) -> bool {
    commissioning_availability(router, Some(LOOPBACK_PEER)).await == true
}

// --- 試運転モード対応: 管理系 `/api/status`・`/api/values`・`/api/tag-catalog`
// (設計 §5.6・2026-08-31 オーナー決定「案A」) -----------------------------
//
// 管理 UI（ブラウザ）が読む `/api/status`・`/api/values`・`/api/tag-catalog`
// は管理系ルーター（`admin_status_router`）にあり、`/api/v1/*`
// （`require_tag_space_auth`、API キー or セッション）とは別。banto v3.0.0
// 追従で、試運転中もこれらは grant トークンで読む（トークン無しは 401）。

/// 試運転モード中: grant トークンで `/api/status`・`/api/values`・
/// `/api/tag-catalog` が読め、トークン無しは 401。
#[tokio::test]
async fn commissioning_grant_reads_admin_status_values_and_catalog() {
    let env = test_env_unlocked().await;
    let token = commissioning_grant(&env.router).await;
    for path in ["/api/status", "/api/values", "/api/tag-catalog"] {
        assert_eq!(
            unauthenticated_status(&env.router, path).await,
            StatusCode::UNAUTHORIZED,
            "{path}"
        );
        let (status, body) = admin_get(&env.router, path, &token).await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
    }
}

/// 対照実験: ロックダウン済みでは、他の管理系ルーターと同様
/// `Authorization` ヘッダ無しの `/api/status`・`/api/values`・
/// `/api/tag-catalog` は 401（`locked_down_admin_api_requires_a_token`と同型）。
#[tokio::test]
async fn locked_down_admin_status_values_and_catalog_require_a_token() {
    let env = test_env().await;
    for path in ["/api/status", "/api/values", "/api/tag-catalog"] {
        assert_eq!(
            unauthenticated_status(&env.router, path).await,
            StatusCode::UNAUTHORIZED,
            "{path}"
        );
        let (status, _) = admin_get(&env.router, path, &env.viewer_token).await;
        assert_eq!(status, StatusCode::OK, "{path}: read-only, any role");
    }
}

/// 回帰確認: catalog本体を`build_catalog_response`へ切り出した後も、
/// `/api/v1/tags`は従来どおり API キー認証で動く（`v1_status_still_works_with_an_api_key_after_the_admin_status_split`
/// と同型、対象だけ`/api/v1/tags`）。
#[tokio::test]
async fn v1_tags_still_works_with_an_api_key_after_the_admin_catalog_split() {
    let env = test_env().await;
    seed_scope_fixture(&env.router, &env.admin_token).await;
    let (status, issued) =
        issue_api_key(&env.router, &env.admin_token, "catalog-reader", &["read"]).await;
    assert_eq!(status, StatusCode::CREATED, "{issued:?}");
    let key = issued["key"].as_str().expect("key should be present");

    let (status, body) = v1_get(&env.router, key, "/api/v1/tags").await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert!(body["tags"].as_array().is_some_and(|tags| !tags.is_empty()));
}

/// 共有ロジックの担保: 管理系 `/api/tag-catalog`（camelCase）と
/// `/api/v1/tags`（snake_case）が同じ情報を返すこと -
/// 両者が[`build_catalog_response`]を共有していることの直接的な証拠
/// （`admin_status_and_values_carry_the_same_information_as_v1`と同型）。
#[tokio::test]
async fn admin_tag_catalog_carries_the_same_information_as_v1_tags() {
    let env = test_env().await;
    seed_scope_fixture(&env.router, &env.admin_token).await;

    let (status, v1_body) = v1_get(&env.router, &env.admin_token, "/api/v1/tags").await;
    assert_eq!(status, StatusCode::OK, "{v1_body:?}");

    let response = env
        .router
        .oneshot(
            HttpRequest::get("/api/tag-catalog")
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
    let admin_body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(admin_body["revision"], v1_body["revision"]);
    assert_eq!(admin_body["collectionMode"], v1_body["collection_mode"]);

    let mut v1_tag_names: Vec<&str> = v1_body["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["external_name"].as_str().unwrap())
        .collect();
    let mut admin_tag_names: Vec<&str> = admin_body["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["externalName"].as_str().unwrap())
        .collect();
    v1_tag_names.sort_unstable();
    admin_tag_names.sort_unstable();
    assert!(
        !v1_tag_names.is_empty(),
        "fixture should seed at least one tag"
    );
    assert_eq!(v1_tag_names, admin_tag_names);

    // camelCase 変換が正しく効いていること（フィールド名だけでなく値も
    // 一致すること）の代表サンプル1件。
    let v1_first = v1_body["tags"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["external_name"] == v1_tag_names[0])
        .unwrap();
    let admin_first = admin_body["tags"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["externalName"] == v1_tag_names[0])
        .unwrap();
    assert_eq!(admin_first["tagKey"], v1_first["tag_key"]);
    assert_eq!(admin_first["dataType"], v1_first["data_type"]);
    assert_eq!(admin_first["periodMs"], v1_first["period_ms"]);
    assert_eq!(
        admin_first["effectiveSimulation"],
        v1_first["effective_simulation"]
    );
    assert_eq!(admin_first["valueSource"], v1_first["value_source"]);
}

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
        json!({ "name": "line1", "host": "127.0.0.1", "port": 15201 }),
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
        json!({
            "name": "erp-db",
            "protocol": "postgres",
            "host": "10.0.0.50",
            "port": 5432,
            "database": "erp",
            "username": "reader",
            "password": "s3cret",
        }),
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

// --- banto v1.7.0 #204: session revocation -------------------------------

fn session_request(
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<serde_json::Value>,
) -> HttpRequest<Body> {
    let mut builder = HttpRequest::builder()
        .method(method)
        .uri(path)
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1);
    if let Some(token) = token {
        builder = builder.header("Authorization", format!("Bearer {token}"));
    }
    match body {
        Some(body) => builder
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    }
}

async fn session_call(
    router: &Router,
    request: HttpRequest<Body>,
) -> (StatusCode, serde_json::Value) {
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

async fn session_login(router: &Router, username: &str, remember: bool) -> String {
    let (status, body) = session_call(
        router,
        session_request(
            "POST",
            "/api/auth/login",
            None,
            Some(json!({ "username": username, "password": "password123", "remember": remember })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["token"]
        .as_str()
        .unwrap_or_else(|| panic!("login for {username} returned no token: {body}"))
        .to_string()
}

/// One session per surface: the admin UI (`require_session`)
/// and the tag space (`require_tag_space_auth`'s session branch). Separate
/// tokens, because the first surface to notice a revocation drops the
/// token from memory - checking one token on both would let the second
/// surface pass without re-checking anything.
struct SurfaceTokens {
    ui: String,
    v1: String,
}

async fn surface_login(router: &Router, username: &str, remember: bool) -> SurfaceTokens {
    SurfaceTokens {
        ui: session_login(router, username, remember).await,
        v1: session_login(router, username, remember).await,
    }
}

/// `(admin UI, tag space)` statuses - both must refuse a revoked session.
async fn session_statuses(router: &Router, tokens: &SurfaceTokens) -> (StatusCode, StatusCode) {
    let admin_ui = session_call(
        router,
        session_request("GET", "/api/tags", Some(&tokens.ui), None),
    )
    .await
    .0;
    let tag_space = session_call(
        router,
        session_request("GET", "/api/v1/tags", Some(&tokens.v1), None),
    )
    .await
    .0;
    (admin_ui, tag_space)
}

#[derive(Debug, Clone, Copy)]
enum AccountChange {
    Delete,
    Demote,
    PasswordChange,
    PasswordReset,
}

/// banto v1.7.0 #204 completion table (banto-hub has no Tauri command
/// surface for accounts - REST is its only transport): deleting,
/// demoting, resetting or changing the password of an account ends its
/// sessions held elsewhere - normal and "Remember me" - on both the admin
/// UI routes and the tag space, while other accounts' sessions are
/// untouched and the session that changed its OWN password keeps
/// working.
#[tokio::test]
async fn account_changes_end_the_accounts_other_sessions() {
    let ok = (StatusCode::OK, StatusCode::OK);
    let revoked = (StatusCode::UNAUTHORIZED, StatusCode::UNAUTHORIZED);
    for change in [
        AccountChange::Delete,
        AccountChange::Demote,
        AccountChange::PasswordChange,
        AccountChange::PasswordReset,
    ] {
        let env = test_env().await;
        let router = &env.router;
        let admin = &env.admin_token;
        let (status, alice) = session_call(
            router,
            session_request(
                "POST",
                "/api/users",
                Some(admin),
                Some(json!({
                    "username": "alice",
                    "password": "password123",
                    "displayName": "Alice",
                    "role": "editor",
                })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{alice}");
        let alice_id = alice["id"].as_i64().unwrap();
        let alice_normal = surface_login(router, "alice", false).await;
        let alice_remember = surface_login(router, "alice", true).await;
        // The session that makes its own password change (on the admin
        // UI surface), plus one on the tag space.
        let alice_self = surface_login(router, "alice", false).await;
        let viewer = surface_login(router, "viewer1", false).await;
        for tokens in [&alice_normal, &alice_remember, &alice_self, &viewer] {
            assert_eq!(
                session_statuses(router, tokens).await,
                ok,
                "{change:?}: before"
            );
        }

        let (status, body) = match change {
            AccountChange::Delete => {
                session_call(
                    router,
                    session_request(
                        "DELETE",
                        &format!("/api/users/{alice_id}"),
                        Some(admin),
                        None,
                    ),
                )
                .await
            }
            AccountChange::Demote => {
                session_call(
                    router,
                    session_request(
                        "PUT",
                        &format!("/api/users/{alice_id}"),
                        Some(admin),
                        Some(json!({ "displayName": "Alice", "role": "viewer" })),
                    ),
                )
                .await
            }
            AccountChange::PasswordReset => {
                session_call(
                    router,
                    session_request(
                        "POST",
                        &format!("/api/users/{alice_id}/reset-password"),
                        Some(admin),
                        Some(json!({ "newPassword": "reset-password1" })),
                    ),
                )
                .await
            }
            AccountChange::PasswordChange => {
                session_call(
                    router,
                    session_request(
                        "POST",
                        "/api/auth/change-password",
                        Some(&alice_self.ui),
                        Some(json!({
                            "currentPassword": "password123",
                            "newPassword": "changed-password1",
                        })),
                    ),
                )
                .await
            }
        };
        assert!(status.is_success(), "{change:?}: {status} {body}");

        assert_eq!(
            session_statuses(router, &alice_normal).await,
            revoked,
            "{change:?}: normal session"
        );
        assert_eq!(
            session_statuses(router, &alice_remember).await,
            revoked,
            "{change:?}: Remember me session"
        );
        // The changer keeps its session; its tag-space sibling (another
        // session of the same account) ends like the rest.
        let expected_self = match change {
            AccountChange::PasswordChange => (StatusCode::OK, StatusCode::UNAUTHORIZED),
            _ => revoked,
        };
        assert_eq!(
            session_statuses(router, &alice_self).await,
            expected_self,
            "{change:?}: the session that made the change"
        );
        assert_eq!(
            session_statuses(router, &viewer).await,
            ok,
            "{change:?}: bystander"
        );
        assert_eq!(
            session_call(
                router,
                session_request("GET", "/api/users", Some(admin), None)
            )
            .await
            .0,
            StatusCode::OK,
            "{change:?}: acting admin"
        );
    }
}

/// #204: a demoted account is authorized with its CURRENT role once it
/// logs in again - and a stale session cannot keep its old role even for
/// one request (it is refused outright, see the table test above).
#[tokio::test]
async fn a_fresh_session_after_a_demotion_uses_the_new_role() {
    let env = test_env().await;
    let router = &env.router;
    let admin = &env.admin_token;
    let (_, admin2) = session_call(
        router,
        session_request(
            "POST",
            "/api/users",
            Some(admin),
            Some(json!({
                "username": "admin2",
                "password": "password123",
                "displayName": "Admin 2",
                "role": "admin",
            })),
        ),
    )
    .await;
    let admin2_id = admin2["id"].as_i64().unwrap();
    let (status, _) = session_call(
        router,
        session_request(
            "PUT",
            &format!("/api/users/{admin2_id}"),
            Some(admin),
            Some(json!({ "displayName": "Admin 2", "role": "viewer" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let fresh = surface_login(router, "admin2", false).await;
    assert_eq!(
        session_call(
            router,
            session_request("GET", "/api/users", Some(&fresh.ui), None)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        session_statuses(router, &fresh).await,
        (StatusCode::OK, StatusCode::OK)
    );
}

/// #204: when the DB cannot answer the re-check, the request fails (500)
/// but the session is KEPT - it works again as soon as the DB answers.
#[tokio::test]
async fn a_failed_recheck_fails_the_request_but_keeps_the_session() {
    let env = test_env().await;
    let router = &env.router;
    let viewer = surface_login(router, "viewer1", true).await;
    sqlx::query("ALTER TABLE users RENAME TO users_away")
        .execute(&env.pool)
        .await
        .unwrap();
    assert_eq!(
        session_statuses(router, &viewer).await,
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::INTERNAL_SERVER_ERROR
        )
    );
    sqlx::query("ALTER TABLE users_away RENAME TO users")
        .execute(&env.pool)
        .await
        .unwrap();
    assert_eq!(
        session_statuses(router, &viewer).await,
        (StatusCode::OK, StatusCode::OK)
    );
}

/// #204: the first account's session from `POST /api/auth/setup` is
/// bound to the new account (`issue_account_token`), so a lookup-enabled
/// `AuthState` accepts it - an unstamped token would be refused on its
/// first use.
#[tokio::test]
async fn the_setup_session_is_bound_to_the_new_account() {
    let pool = migrate_memory().await.expect("migrate_memory");
    let users = UsersService::new(Db::Sqlite(pool.clone()));
    let audit = AuditLogService::new(Db::Sqlite(pool.clone()));
    let auth = user_auth_state(users.clone(), audit.clone());
    let router = extra_auth_router(
        users,
        auth.clone(),
        audit,
        true,
        Arc::new(GrantRegistry::new()),
        None,
    );
    let (status, body) = session_call(
        &router,
        session_request(
            "POST",
            "/api/auth/setup",
            None,
            Some(json!({
                "username": "owner",
                "password": "password123",
                "displayName": "Owner",
            })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let token = body["token"].as_str().expect("setup token");
    let session = auth
        .authenticate(token)
        .await
        .unwrap()
        .expect("the setup session is accepted");
    assert_eq!(session.identity.id, "owner");
    assert_eq!(session.identity.role, "admin");
}

/// #431: ゲートの判断の総当たり表。例外（照合できないのに通す）は
/// 「止める操作」かつ「それまで有効だったセッション」の 1 行だけ。
#[test]
fn session_gate_decision_table() {
    use GateDecision::*;
    use OperationKind::*;
    use SessionCheck::*;
    let table = [
        (Valid, Normal, false, Allow),
        (Valid, Normal, true, Allow),
        (Valid, StopWrites, false, Allow),
        (Valid, StopWrites, true, Allow),
        // 失効が確認できたら、止める操作でも拒否
        (Revoked, Normal, false, Unauthorized),
        (Revoked, Normal, true, Unauthorized),
        (Revoked, StopWrites, false, Unauthorized),
        (Revoked, StopWrites, true, Unauthorized),
        // 照合できない: 通常は 500、止める操作だけ例外
        (Unverified, Normal, false, Unavailable),
        (Unverified, Normal, true, Unavailable),
        (Unverified, StopWrites, false, Unauthorized),
        (Unverified, StopWrites, true, AllowUnverifiedStop),
    ];
    for (check, operation, previously_valid, expected) in table {
        assert_eq!(
            session_gate_decision(check, operation, previously_valid),
            expected,
            "{check:?} / {operation:?} / previously_valid={previously_valid}"
        );
    }
}

/// #431: 例外の入口は宣言だけ。緊急停止は止める操作、再開は通常の操作。
#[test]
fn only_the_write_stop_route_is_declared_as_a_stop_operation() {
    assert_eq!(WRITE_CONTROL_DISABLE_OPERATION, OperationKind::StopWrites);
    assert_eq!(WRITE_CONTROL_ENABLE_OPERATION, OperationKind::Normal);
}

// --- I3': banto のルーターに替えて入ったもの（banto #277/#278/#248） ---
//
// 本番と同じ合成（`api_router`）・本番と同じ `user_auth_state`（監査
// つきの検証関数とアカウント照合）の上で、banto v2.1.0 の修正が
// banto-hub の REST で効いていることを固定する。以前の写し
// （`crate::users`・`crate::rest` の自前のルーター）ではどれも落ちる。

/// 本番と同じ `user_auth_state` で組んだ router。アカウントは作らない
/// （初回セットアップの前の状態）。試運転のまま（ロックダウンしない）。
async fn banto_wiring_env() -> (Router, sqlx::SqlitePool, UsersService, tempfile::TempDir) {
    let pool = migrate_memory().await.expect("migrate_memory");
    let (tx, _rx) = tokio_broadcast::channel(16);
    let users = UsersService::new(Db::Sqlite(pool.clone()));
    let audit = AuditLogService::new(Db::Sqlite(pool.clone()));
    let api_keys = ApiKeysService::new(pool.clone());
    let (manager, dir) = test_manager_with_clock(pool.clone(), Arc::new(SystemClock));
    let auth = user_auth_state(users.clone(), audit.clone());
    let write_control = Arc::new(crate::write_control::WriteControl::new(false));
    let write_audit = crate::write_audit::WriteAuditService::new(pool.clone());
    let mqtt = Arc::new(crate::mqtt::MqttPublisher::new(manager.clone()));
    let rate_limiter = Arc::new(AsyncMutex::new(WriteRateLimiter::new(
        crate::write_rate::WriteRateLimitConfig::default(),
    )));
    let grpc_service = crate::grpc::GrpcService::new(
        manager.clone(),
        api_keys.clone(),
        audit.clone(),
        write_audit.clone(),
        write_control.clone(),
        rate_limiter.clone(),
        tx.clone(),
    );
    let grpc_server = Arc::new(crate::grpc::GrpcServer::new(grpc_service));
    let commissioning = CommissioningService::load(
        SettingsService::new(Db::Sqlite(pool.clone())),
        users.clone(),
        auth.clone(),
    )
    .await
    .expect("CommissioningService::load");
    let router = api_router(
        users.clone(),
        audit,
        PlcConnectionService::new(pool.clone()),
        CollectionGroupService::new(pool.clone()),
        TagService::new(pool.clone()),
        api_keys,
        manager,
        auth,
        commissioning,
        tx,
        true,
        write_control,
        write_audit,
        mqtt,
        grpc_server,
        rate_limiter,
        crate::profile_paths::DEFAULT_PROFILE_ID.to_string(),
    );
    (router, pool, users, dir)
}

async fn audit_rows(pool: &sqlx::SqlitePool, action: &str) -> Vec<Option<String>> {
    sqlx::query_scalar("SELECT actor_username FROM audit_log WHERE action = ? ORDER BY id")
        .bind(action)
        .fetch_all(pool)
        .await
        .unwrap()
}

/// banto #277: 初回セットアップの「表が空か」と INSERT が 1 文。同時に
/// 来たセットアップは 1 つだけ成功し、アカウントも `setup` の監査も 1 件。
/// 以前の写しは「空か確かめる → ハッシュ → INSERT」の 3 段で、全員が空を
/// 見て admin が複数できた。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_first_setups_create_exactly_one_account() {
    let (router, pool, _users, _dir) = banto_wiring_env().await;
    const ATTEMPTS: usize = 8;
    let barrier = Arc::new(tokio::sync::Barrier::new(ATTEMPTS));
    let mut handles = Vec::new();
    for i in 0..ATTEMPTS {
        let router = router.clone();
        let barrier = barrier.clone();
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            session_call(
                &router,
                session_request(
                    "POST",
                    "/api/auth/setup",
                    None,
                    Some(json!({
                        "username": format!("owner{i}"),
                        "password": "password123",
                        "displayName": format!("Owner {i}"),
                    })),
                ),
            )
            .await
        }));
    }
    let mut succeeded = 0;
    for handle in handles {
        let (status, body) = handle.await.unwrap();
        assert_eq!(status, StatusCode::OK, "{body}");
        if body["success"] == true {
            assert!(body["token"].is_string(), "{body}");
            succeeded += 1;
        } else {
            assert_eq!(body["success"], false, "{body}");
            assert!(body["token"].is_null(), "{body}");
        }
    }
    assert_eq!(succeeded, 1, "exactly one setup wins");
    let accounts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(accounts, 1);
    assert_eq!(audit_rows(&pool, "setup").await.len(), 1);
}

/// banto #278: 失敗ログインの監査に残す名前は 32 文字（末尾 `…`）に
/// 切り詰める。名前は攻撃者が決められ、実在のアカウントである必要も
/// ないので、そのまま残すと監査ログを膨らませる口になる。以前の写しは
/// 入力をそのまま記録していた。
#[tokio::test]
async fn a_failed_login_records_a_bounded_username() {
    let (router, pool, users, _dir) = banto_wiring_env().await;
    users
        .setup_first_user("admin", "password123", "管理者")
        .await
        .unwrap();
    let long_name = "x".repeat(4096);
    let (status, body) = session_call(
        &router,
        session_request(
            "POST",
            "/api/auth/login",
            None,
            Some(json!({ "username": long_name, "password": "wrong-password" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["success"], false, "{body}");

    let recorded = audit_rows(&pool, "login_failed").await;
    assert_eq!(recorded.len(), 1);
    let recorded = recorded[0].clone().expect("an actor username");
    assert_eq!(recorded.chars().count(), 32, "{recorded}");
    assert_eq!(recorded, format!("{}…", "x".repeat(31)));

    // 32 文字以内の名前は（実在しなくても）そのまま残る。
    let (_, body) = session_call(
        &router,
        session_request(
            "POST",
            "/api/auth/login",
            None,
            Some(json!({ "username": "nobody", "password": "wrong-password" })),
        ),
    )
    .await;
    assert_eq!(body["success"], false, "{body}");
    assert_eq!(
        audit_rows(&pool, "login_failed")
            .await
            .last()
            .cloned()
            .flatten(),
        Some("nobody".to_string())
    );
}

/// banto #278: ログアウトの監査は、実際にセッションを終えたときだけ。
/// トークン無し・知らないトークン・終えた後のトークンのログアウトは資格
/// 情報なしで何度でも叩けるので、記録すると監査ログを膨らませる口になる。
/// 以前の写しは毎回（actor なしで）1 行残していた。
#[tokio::test]
async fn only_a_logout_that_ends_a_session_is_audited() {
    let (router, pool, users, _dir) = banto_wiring_env().await;
    users
        .setup_first_user("admin", "password123", "管理者")
        .await
        .unwrap();
    let token = session_login(&router, "admin", false).await;

    let logout = |token: Option<String>| {
        let router = router.clone();
        async move {
            session_call(
                &router,
                session_request("POST", "/api/auth/logout", token.as_deref(), None),
            )
            .await
            .0
        }
    };
    assert!(logout(None).await.is_success());
    assert!(logout(Some("not-a-known-token".to_string()))
        .await
        .is_success());
    assert!(audit_rows(&pool, "logout").await.is_empty());

    assert!(logout(Some(token.clone())).await.is_success());
    assert_eq!(
        audit_rows(&pool, "logout").await,
        vec![Some("admin".to_string())]
    );

    // 終えた後の同じトークンは、もう何も終えない。
    assert!(logout(Some(token)).await.is_success());
    assert_eq!(audit_rows(&pool, "logout").await.len(), 1);
}

/// #248 の受け皿: 監査ログ一覧の応答に `deletionEpoch`（保持の剪定が
/// 行を消すたびに進む数）が乗る。画面（PR2）はこれと件数を世代の最初の
/// 応答と比べて、途中で削除が入ったことを知る。以前の写しの応答には
/// 無かった。
#[tokio::test]
async fn the_audit_log_list_carries_the_deletion_epoch() {
    let env = test_env().await;
    let list = |as_of: Option<i64>| {
        let path = match as_of {
            Some(id) => format!("/api/audit-log/list?asOfId={id}"),
            None => "/api/audit-log/list".to_string(),
        };
        session_request("POST", &path, Some(&env.admin_token), Some(json!({})))
    };
    let (status, body) = session_call(&env.router, list(None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["deletionEpoch"], 0, "{body}");
    let as_of = body["asOfId"].as_i64().expect("asOfId");

    // 監査を 2 行にしてから（viewer の拒否 + 設定の保存）保持件数を 1 に
    // し、境界なしの一覧（剪定する）で古い 1 行を消す。
    let (status, _) = session_call(
        &env.router,
        session_request(
            "GET",
            "/api/audit-log/config",
            Some(&env.viewer_token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = session_call(
        &env.router,
        session_request(
            "PUT",
            "/api/audit-log/config",
            Some(&env.admin_token),
            Some(json!({ "retentionDays": null, "retentionRows": 1 })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = session_call(&env.router, list(None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["deletionEpoch"], 1, "{body}");

    // 境界つきの一覧（世代の 2 ブロック目以降）も同じ数を返す。
    let (status, body) = session_call(&env.router, list(Some(as_of))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["deletionEpoch"], 1, "{body}");

    // 読めない `asOfId` は JSON の 400（axum 既定の平文ではない）。
    let (status, body) = session_call(
        &env.router,
        session_request(
            "POST",
            "/api/audit-log/list?asOfId=abc",
            Some(&env.admin_token),
            Some(json!({})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["kind"], "bad_request", "{body}");
}

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
