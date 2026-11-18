//! `/api/display-groups/*`（#393 = #524 の段階 1、R1-B の残り）。
//!
//! | Method | Path                        | Body                    | Response |
//! |--------|-----------------------------|-------------------------|----------|
//! | GET    | `/api/display-groups`       | -                       | `DisplayGroup[]`（viewer 以上、並び順） |
//! | POST   | `/api/display-groups`       | `DisplayGroupPayload`   | `DisplayGroupSaveResponse`（editor 以上） |
//! | PUT    | `/api/display-groups/order` | `{ ids: number[] }`     | `DisplayGroup[]`（editor 以上、並べ替え） |
//! | GET    | `/api/display-groups/{id}`  | -                       | `DisplayGroup`（viewer 以上） |
//! | PUT    | `/api/display-groups/{id}`  | `DisplayGroupPayload`   | `DisplayGroupSaveResponse`（editor 以上。版の食い違いは `409`。本文の `sortOrder` は無視 - 並びは `/order` だけで変える） |
//! | DELETE | `/api/display-groups/{id}`  | -                       | 204（editor 以上） |
//!
//! 作法は親モジュールの `tag_registry_router` と同じ: 読み取りはルーター全体の
//! [`require_session_or_api_key`]（viewer 以上、API キーは `read`・`config`）、
//! 書き込みは [`DisplayGroupOps`] のメソッド（#1161 PR5a。editor 以上 / `config`。
//! 拒否は `denied` で監査）、成功した書き込みだけ [`Actor::record`] で監査する
//! （`resource: "display_groups"`、`origin: "rest"`。#1161 PR4 で banto の
//! `record_write` から置き換えた - キーの主体も残せるように）。読み取りは監査しない。
//! 検証は `crate::display_groups::validate_display_group` だけで、Tauri の
//! `display_groups_*` コマンドも同じサービス・同じ検証を通る。
//!
//! 作成・更新はペンに Hub のカタログのタグ（`hubTag`）を指せ、未登録のものは
//! 同じ保存の中で記録項目として登録する（#1179 PR5、`crate::display_groups` の
//! モジュール doc）。応答は `DisplayGroup` の項目に、今回登録した記録項目
//! （`registeredTags`、無ければ出さない）を足した形。
//!
//! 版の食い違い（`expectedRevision`）だけ `409 Conflict` にし、本文は他の検証
//! エラーと同じ `ErrorBody` 形（`kind: "validation"`、`field_errors` の
//! `expectedRevision`）にする - タグの楽観ロック（#525）と同じ形。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::middleware;
use axum::routing::{get, put};
use axum::{Json, Router};
use banto_server::{ApiError, AuthState};

use super::{require_session_or_api_key, Actor, RevisionConflictRejection};
use crate::audit::AuditLogService;
use crate::display_groups::{
    DisplayGroup, DisplayGroupPayload, DisplayGroupSaveResponse, DisplayGroupService,
    ReorderPayload,
};
use crate::hub::HubService;
use crate::hub_tags::HubTagRegistrar;
use crate::ops::{DisplayGroupOps, Via};

/// #1161 PR5a: 書き込みの本体（権限の確かめ・監査）は [`DisplayGroupOps`] の
/// メソッドで、MCP のツールも同じものを呼ぶ。
type DisplayGroupsState = DisplayGroupOps;

#[utoipa::path(
    get,
    path = "/api/display-groups",
    tag = "display-groups",
    operation_id = "display_groups_list",
    summary = "表示グループの一覧",
    description = "表示グループの一覧（並び順）。読み取り（`read` キーで足りる）。",
    responses(
        (status = 200, description = "表示グループの一覧", body = Vec<DisplayGroup>),
        (status = 401, description = "API キーが無い・誤り・失効・期限切れ、またはログインのセッションが無い"),
        (status = 403, description = "ロールが足りない"),
    ),
    security(("api_key" = [])),
)]
pub(super) async fn list(
    State(state): State<DisplayGroupsState>,
) -> Result<Json<Vec<DisplayGroup>>, ApiError> {
    Ok(Json(state.display_groups.list().await?))
}

#[utoipa::path(
    get,
    path = "/api/display-groups/{id}",
    tag = "display-groups",
    operation_id = "display_groups_get",
    summary = "表示グループの取得",
    description = "表示グループを 1 件返す。読み取り（`read` キーで足りる）。",
    params(("id" = i64, Path, description = "対象の id")),
    responses(
        (status = 200, description = "表示グループ", body = DisplayGroup),
        (status = 401, description = "API キーが無い・誤り・失効・期限切れ、またはログインのセッションが無い"),
        (status = 403, description = "ロールが足りない"),
        (status = 404, description = "指定の id が無い"),
    ),
    security(("api_key" = [])),
)]
pub(super) async fn get_one(
    State(state): State<DisplayGroupsState>,
    Path(id): Path<i64>,
) -> Result<Json<DisplayGroup>, ApiError> {
    Ok(Json(state.display_groups.get(id).await?))
}

#[utoipa::path(
    post,
    path = "/api/display-groups",
    tag = "display-groups",
    operation_id = "display_groups_create",
    summary = "表示グループの作成",
    description = "表示グループを作る。書き込み（`config` キーが要る）。成功した書き込みは監査ログに残る。ペンは `tagId`（登録済みの記録項目）か `hubTag`（Hub のカタログのタグ）のどちらか 1 つ。未登録の `hubTag` は `hubTagGroup` の収集グループに記録項目として同じ保存の中で登録し（全部か無しか）、応答の `registeredTags` に返す。登録済みの `hubTag` は既存の記録項目を参照する。",
    request_body = DisplayGroupPayload,
    responses(
        (status = 200, description = "作成した表示グループ（今回登録した記録項目があれば `registeredTags`）", body = DisplayGroupSaveResponse),
        (status = 401, description = "API キーが無い・誤り・失効・期限切れ、またはログインのセッションが無い"),
        (status = 403, description = "`read` キー（`config` スコープが無い）、または editor 未満のロール"),
        (status = 422, description = "入力の検証エラー（`field_errors` に項目ごとの理由）"),
    ),
    security(("api_key" = [])),
)]
pub(super) async fn create(
    State(state): State<DisplayGroupsState>,
    actor: Actor,
    Json(payload): Json<DisplayGroupPayload>,
) -> Result<Json<DisplayGroupSaveResponse>, ApiError> {
    let created = state
        .create(
            &actor,
            &Via::Rest {
                method: "POST",
                path: "/api/display-groups",
            },
            &payload,
        )
        .await?;
    Ok(Json(created))
}

#[utoipa::path(
    put,
    path = "/api/display-groups/{id}",
    tag = "display-groups",
    operation_id = "display_groups_update",
    summary = "表示グループの更新",
    description = "表示グループを更新する（全項目を送る。本文の `sortOrder` は無視され、並びは `/api/display-groups/order` だけで変える）。書き込み（`config` キーが要る）。成功した書き込みは監査ログに残る。ペンの `hubTag`・`hubTagGroup` は作成と同じ。",
    params(("id" = i64, Path, description = "対象の id")),
    request_body = DisplayGroupPayload,
    responses(
        (status = 200, description = "更新後の表示グループ（今回登録した記録項目があれば `registeredTags`）", body = DisplayGroupSaveResponse),
        (status = 401, description = "API キーが無い・誤り・失効・期限切れ、またはログインのセッションが無い"),
        (status = 403, description = "`read` キー（`config` スコープが無い）、または editor 未満のロール"),
        (status = 404, description = "指定の id が無い"),
        (status = 422, description = "入力の検証エラー（`field_errors` に項目ごとの理由）"),
        (status = 409, description = "`expectedRevision` が古い（他で先に更新された。本文は `kind: validation`、`field_errors` の `expectedRevision`）"),
    ),
    security(("api_key" = [])),
)]
pub(super) async fn update(
    State(state): State<DisplayGroupsState>,
    actor: Actor,
    Path(id): Path<i64>,
    Json(payload): Json<DisplayGroupPayload>,
) -> Result<Json<DisplayGroupSaveResponse>, RevisionConflictRejection> {
    let updated = state
        .update(
            &actor,
            &Via::Rest {
                method: "PUT",
                path: "/api/display-groups/{id}",
            },
            id,
            &payload,
        )
        .await?;
    Ok(Json(updated))
}

#[utoipa::path(
    delete,
    path = "/api/display-groups/{id}",
    tag = "display-groups",
    operation_id = "display_groups_delete",
    summary = "表示グループの削除",
    description = "表示グループを削除する。書き込み（`config` キーが要る）。成功した書き込みは監査ログに残る。",
    params(("id" = i64, Path, description = "対象の id")),
    responses(
        (status = 204, description = "削除した"),
        (status = 401, description = "API キーが無い・誤り・失効・期限切れ、またはログインのセッションが無い"),
        (status = 403, description = "`read` キー（`config` スコープが無い）、または editor 未満のロール"),
        (status = 404, description = "指定の id が無い"),
    ),
    security(("api_key" = [])),
)]
pub(super) async fn delete(
    State(state): State<DisplayGroupsState>,
    actor: Actor,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    state
        .delete(
            &actor,
            &Via::Rest {
                method: "DELETE",
                path: "/api/display-groups/{id}",
            },
            id,
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    put,
    path = "/api/display-groups/order",
    tag = "display-groups",
    operation_id = "display_groups_reorder",
    summary = "表示グループの並べ替え",
    description = "表示グループの並びを `ids` の順に変える。書き込み（`config` キーが要る）。成功した書き込みは監査ログに残る。",
    request_body = ReorderPayload,
    responses(
        (status = 200, description = "並べ替え後の表示グループ", body = Vec<DisplayGroup>),
        (status = 401, description = "API キーが無い・誤り・失効・期限切れ、またはログインのセッションが無い"),
        (status = 403, description = "`read` キー（`config` スコープが無い）、または editor 未満のロール"),
        (status = 422, description = "入力の検証エラー（`field_errors` に項目ごとの理由）"),
    ),
    security(("api_key" = [])),
)]
pub(super) async fn reorder(
    State(state): State<DisplayGroupsState>,
    actor: Actor,
    Json(payload): Json<ReorderPayload>,
) -> Result<Json<Vec<DisplayGroup>>, ApiError> {
    let groups = state
        .reorder(
            &actor,
            &Via::Rest {
                method: "PUT",
                path: "/api/display-groups/order",
            },
            &payload.ids,
        )
        .await?;
    Ok(Json(groups))
}

/// `/api/display-groups/*` のルーター（モジュール doc の表）。
pub(super) fn display_groups_router(
    display_groups: DisplayGroupService,
    registrar: HubTagRegistrar,
    hub: HubService,
    audit: AuditLogService,
    auth: AuthState,
) -> Router {
    let state = DisplayGroupsState {
        display_groups,
        registrar,
        hub,
        audit,
    };
    Router::new()
        .route("/api/display-groups", get(list).post(create))
        .route("/api/display-groups/order", put(reorder))
        .route(
            "/api/display-groups/{id}",
            get(get_one).put(update).delete(delete),
        )
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            auth,
            require_session_or_api_key,
        ))
}

#[cfg(test)]
mod tests {
    use crate::display_groups::tests::seed_tags;
    use crate::rest::tests::router_with_role_tokens_and_pool;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::Router;
    use serde_json::{json, Value};
    use tower::ServiceExt;

    async fn send(
        router: &Router,
        method: &str,
        path: &str,
        token: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header("X-Banto-Client", "banto")
            .header("Authorization", format!("Bearer {token}"));
        let body = match body {
            Some(value) => {
                builder = builder.header("Content-Type", "application/json");
                Body::from(value.to_string())
            }
            None => Body::empty(),
        };
        let response = router
            .clone()
            .oneshot(builder.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (status, value)
    }

    /// `audit_log` の (action, result, origin) を古い順に。`resource` で絞る。
    async fn audit_rows(pool: &sqlx::SqlitePool, resource: &str) -> Vec<(String, String, String)> {
        sqlx::query_as(
            "SELECT action, result, origin FROM audit_log WHERE resource = ? ORDER BY id",
        )
        .bind(resource)
        .fetch_all(pool)
        .await
        .unwrap()
    }

    /// REST の CRUD 一巡と権限（viewer は読むだけ・書き込みは 403 で `denied` を
    /// 監査）・成功した書き込みの監査・並べ替え・版の食い違いの `409`。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn display_group_routes_crud_rbac_audit_and_conflict() {
        let (router, _admin, editor, viewer, pool) = router_with_role_tokens_and_pool().await;
        let (_tags, ids) = seed_tags(&pool, 3).await;

        // viewer は書けない（403、`denied` を監査）。
        let body = json!({ "name": "L1", "kind": "trend", "pens": [{ "tagId": ids[0] }] });
        let (status, _) = send(
            &router,
            "POST",
            "/api/display-groups",
            &viewer,
            Some(body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        // editor は作れる。
        let (status, a) = send(&router, "POST", "/api/display-groups", &editor, Some(body)).await;
        assert_eq!(status, StatusCode::OK, "{a}");
        assert_eq!(a["attributes"], json!({ "timeWindowSec": 600 }));
        let (_, b) = send(
            &router,
            "POST",
            "/api/display-groups",
            &editor,
            Some(json!({ "name": "L2", "kind": "gauge" })),
        )
        .await;
        let (a_id, b_id) = (a["id"].as_i64().unwrap(), b["id"].as_i64().unwrap());

        // viewer は読める。
        let (status, list) = send(&router, "GET", "/api/display-groups", &viewer, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(list.as_array().unwrap().len(), 2);
        let (status, one) = send(
            &router,
            "GET",
            &format!("/api/display-groups/{a_id}"),
            &viewer,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(one, a);

        // 検証エラーは人間可読な `field_errors`（未知の種別・属性も serde で弾かない）。
        let (status, err) = send(
            &router,
            "POST",
            "/api/display-groups",
            &editor,
            Some(json!({ "name": "L3", "kind": "pie", "attributes": { "x": 1 } })),
        )
        .await;
        assert!(status.is_client_error(), "{status}");
        assert_eq!(err["kind"], "validation");
        assert_eq!(err["field_errors"][0]["field"], "kind");

        // 更新（読んだ応答をそのまま送り返す）。
        let mut edit = a.clone();
        edit["name"] = json!("L1改");
        edit["pens"] = json!([{ "tagId": ids[1], "colorSlot": 3 }, { "tagId": ids[2] }]);
        edit["expectedRevision"] = a["revision"].clone();
        let (status, updated) = send(
            &router,
            "PUT",
            &format!("/api/display-groups/{a_id}"),
            &editor,
            Some(edit.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{updated}");
        assert_eq!(updated["revision"], json!(2));

        // 古い版での更新は 409、本文は `expectedRevision` の検証エラー。保存は変わらない。
        edit["name"] = json!("古い版");
        let (status, conflict) = send(
            &router,
            "PUT",
            &format!("/api/display-groups/{a_id}"),
            &editor,
            Some(edit),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(conflict["kind"], "validation");
        assert_eq!(conflict["field_errors"][0]["field"], "expectedRevision");
        let (_, reread) = send(
            &router,
            "GET",
            &format!("/api/display-groups/{a_id}"),
            &editor,
            None,
        )
        .await;
        assert_eq!(reread, updated);

        // 並べ替え（viewer は 403）。
        let order = json!({ "ids": [b_id, a_id] });
        let (status, _) = send(
            &router,
            "PUT",
            "/api/display-groups/order",
            &viewer,
            Some(order.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, reordered) = send(
            &router,
            "PUT",
            "/api/display-groups/order",
            &editor,
            Some(order),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{reordered}");
        assert_eq!(reordered[0]["name"], "L2");

        // 削除（viewer は 403）。
        let (status, _) = send(
            &router,
            "DELETE",
            &format!("/api/display-groups/{b_id}"),
            &viewer,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = send(
            &router,
            "DELETE",
            &format!("/api/display-groups/{b_id}"),
            &editor,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = send(
            &router,
            "GET",
            &format!("/api/display-groups/{b_id}"),
            &editor,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // 監査: 拒否 3 件と成功した書き込みだけ（読み取り・検証エラー・409 は記録しない）。
        let rows = audit_rows(&pool, "display_groups").await;
        let actions: Vec<(&str, &str)> = rows
            .iter()
            .map(|(action, result, origin)| {
                assert_eq!(origin, "rest");
                (action.as_str(), result.as_str())
            })
            .collect();
        assert_eq!(
            actions,
            [
                ("denied", "denied"),
                ("create", "ok"),
                ("create", "ok"),
                ("update", "ok"),
                ("denied", "denied"),
                ("reorder", "ok"),
                ("denied", "denied"),
                ("delete", "ok"),
            ]
        );
    }

    /// §3.7.9 の 8（REST 経路）: ペンに割り当てられたタグの `DELETE /api/tags/{id}`
    /// は参照元のグループ名つきの検証エラーで拒否され、タグは残る。
    ///
    /// 反証（2026-10-08 実施）: 親モジュールの `tags_delete` を
    /// `state.tags.delete(id)` に戻すと、最初の `assert!` が落ちる（204 で消える）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn deleting_a_tag_used_by_a_display_group_is_refused_over_rest() {
        let (router, _admin, editor, _viewer, pool) = router_with_role_tokens_and_pool().await;
        let (_tags, ids) = seed_tags(&pool, 2).await;
        let (status, _) = send(
            &router,
            "POST",
            "/api/display-groups",
            &editor,
            Some(json!({ "name": "ライン1", "kind": "digital", "pens": [{ "tagId": ids[0] }] })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, err) = send(
            &router,
            "DELETE",
            &format!("/api/tags/{}", ids[0]),
            &editor,
            None,
        )
        .await;
        assert!(
            status.is_client_error(),
            "参照されているタグが消せてしまった: {status}"
        );
        assert_eq!(err["kind"], "validation");
        assert_eq!(err["field_errors"][0]["field"], "displayGroups");
        assert!(
            err["field_errors"][0]["message"]
                .as_str()
                .unwrap()
                .contains("「ライン1」"),
            "{err}"
        );
        let (status, _) = send(
            &router,
            "GET",
            &format!("/api/tags/{}", ids[0]),
            &editor,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "タグは残っている");

        // 参照されていないタグは従来どおり消せる。
        let (status, _) = send(
            &router,
            "DELETE",
            &format!("/api/tags/{}", ids[1]),
            &editor,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    /// `(resource, action, origin)` を古い順に（記録項目と表示グループの監査の順を
    /// 確かめる）。
    async fn audit_rows_of(
        pool: &sqlx::SqlitePool,
        resources: &[&str],
    ) -> Vec<(String, String, String)> {
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT resource, action, origin FROM audit_log WHERE result = 'ok' ORDER BY id",
        )
        .fetch_all(pool)
        .await
        .unwrap();
        rows.into_iter()
            .filter(|(resource, _, _)| resources.contains(&resource.as_str()))
            .collect()
    }

    /// **#1179 PR5（REST）**: 作成でペンの Hub のタグを登録する一括保存。応答に
    /// `registeredTags`、監査は記録項目の `create`（`detail` は「Hub から追加」と
    /// 同じ形）→ 表示グループの `create` の順。登録先が無ければ 422 で何も書かず、
    /// 古い版の更新は 409 で何も登録しない。登録の無い保存の応答に
    /// `registeredTags` は出ない。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn display_group_save_registers_hub_tags_over_rest() {
        let (router, _admin, editor, _viewer, pool) = router_with_role_tokens_and_pool().await;
        let (_tags, ids) = seed_tags(&pool, 1).await;
        let hub_pen = |external_name: &str, name: &str| json!({ "hubTag": { "externalName": external_name, "name": name, "dataType": "f32" } });

        let missing_target = json!({
            "name": "L1", "kind": "trend",
            "pens": [hub_pen("L.G.a", "a")],
        });
        let (status, body) = send(
            &router,
            "POST",
            "/api/display-groups",
            &editor,
            Some(missing_target),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["field_errors"][0]["field"], "hubTagGroup", "{body}");
        assert!(audit_rows_of(&pool, &["tags", "display_groups"])
            .await
            .is_empty());

        let create = json!({
            "name": "L1", "kind": "trend",
            "pens": [hub_pen("L.G.a", "a"), { "tagId": ids[0] }, hub_pen("L.G.b", "b")],
            "hubTagGroup": { "newGroup": { "name": "hub-g", "periodMs": 1000 } },
        });
        let (status, created) = send(
            &router,
            "POST",
            "/api/display-groups",
            &editor,
            Some(create),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        let registered = created["registeredTags"]
            .as_array()
            .expect("registeredTags");
        assert_eq!(registered.len(), 2);
        assert_eq!(created["pens"][0]["tagId"], registered[0]["id"]);
        assert_eq!(created["pens"][1]["tagId"], ids[0]);
        assert_eq!(created["pens"][2]["tagId"], registered[1]["id"]);
        assert_eq!(
            audit_rows_of(&pool, &["tags", "display_groups"]).await,
            [("tags", "create"), ("display_groups", "create")].map(|(r, a)| (
                r.to_string(),
                a.to_string(),
                "rest".to_string()
            ))
        );
        let detail: String = sqlx::query_scalar(
            "SELECT detail FROM audit_log WHERE resource = 'tags' AND action = 'create'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let detail: Value = serde_json::from_str(&detail).unwrap();
        assert_eq!(detail["source"], "hub");
        assert_eq!(detail["count"], 2);
        assert_eq!(detail["groupCreated"], true);

        // 古い版: 409、何も登録しない。
        let id = created["id"].as_i64().unwrap();
        let mut stale = created.clone();
        stale["expectedRevision"] = json!(created["revision"].as_i64().unwrap() + 5);
        stale["pens"]
            .as_array_mut()
            .unwrap()
            .push(hub_pen("L.G.c", "c"));
        stale["hubTagGroup"] = json!({ "newGroup": { "name": "hub-g2", "periodMs": 1000 } });
        let (status, body) = send(
            &router,
            "PUT",
            &format!("/api/display-groups/{id}"),
            &editor,
            Some(stale),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        let tag_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tags")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(tag_count, 3, "1 本 + 登録した 2 本のまま");

        // 応答をそのまま送り返す更新（登録なし）: `registeredTags` は出ない。
        let mut round_trip = created.clone();
        round_trip["expectedRevision"] = created["revision"].clone();
        let (status, updated) = send(
            &router,
            "PUT",
            &format!("/api/display-groups/{id}"),
            &editor,
            Some(round_trip),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{updated}");
        assert!(updated.get("registeredTags").is_none(), "{updated}");
        assert_eq!(updated["pens"], created["pens"]);
    }
}
