//! `/api/display-groups/*`（#393 = #524 の段階 1、R1-B の残り）。
//!
//! | Method | Path                        | Body                    | Response |
//! |--------|-----------------------------|-------------------------|----------|
//! | GET    | `/api/display-groups`       | -                       | `DisplayGroup[]`（viewer 以上、並び順） |
//! | POST   | `/api/display-groups`       | `DisplayGroupPayload`   | `DisplayGroup`（editor 以上） |
//! | PUT    | `/api/display-groups/order` | `{ ids: number[] }`     | `DisplayGroup[]`（editor 以上、並べ替え） |
//! | GET    | `/api/display-groups/{id}`  | -                       | `DisplayGroup`（viewer 以上） |
//! | PUT    | `/api/display-groups/{id}`  | `DisplayGroupPayload`   | `DisplayGroup`（editor 以上。版の食い違いは `409`） |
//! | DELETE | `/api/display-groups/{id}`  | -                       | 204（editor 以上） |
//!
//! 作法は親モジュールの `tag_registry_router` と同じ: 読み取りはルーター全体の
//! `require_auth`（viewer 以上）、書き込みはハンドラごとの [`require_editor`]
//! （拒否は `denied` で監査）、成功した書き込みだけ [`record_write`] で監査する
//! （`resource: "display_groups"`、`origin: "rest"`）。読み取りは監査しない。
//! 検証は `crate::display_groups::validate_display_group` だけで、Tauri の
//! `display_groups_*` コマンドも同じサービス・同じ検証を通る。
//!
//! 版の食い違い（`expectedRevision`）だけ `409 Conflict` にし、本文は他の検証
//! エラーと同じ `ErrorBody` 形（`kind: "validation"`、`field_errors` の
//! `expectedRevision`）にする - タグの楽観ロック（#525）と同じ形。

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use axum::{Json, Router};
use banto_core::BantoError;
use banto_server::routes::record_write;
use banto_server::{require_auth, ApiError, AuthState};
use serde_json::json;

use super::require_editor;
use crate::audit::AuditLogService;
use crate::display_groups::{
    audit_detail, is_revision_conflict, DisplayGroup, DisplayGroupPayload, DisplayGroupService,
    ReorderPayload, AUDIT_RESOURCE,
};

#[derive(Clone)]
struct DisplayGroupsState {
    display_groups: DisplayGroupService,
    auth: AuthState,
    audit: AuditLogService,
}

/// 書き込みの失敗。版の食い違いだけ `409`。
enum WriteRejection {
    Api(ApiError),
    RevisionConflict(BantoError),
}

impl From<BantoError> for WriteRejection {
    fn from(err: BantoError) -> Self {
        if is_revision_conflict(&err) {
            Self::RevisionConflict(err)
        } else {
            Self::Api(ApiError(err))
        }
    }
}

impl IntoResponse for WriteRejection {
    fn into_response(self) -> Response {
        match self {
            Self::Api(err) => err.into_response(),
            Self::RevisionConflict(err) => (
                StatusCode::CONFLICT,
                Json(banto_core::ErrorBody::from(&err)),
            )
                .into_response(),
        }
    }
}

async fn list(
    State(state): State<DisplayGroupsState>,
) -> Result<Json<Vec<DisplayGroup>>, ApiError> {
    Ok(Json(state.display_groups.list().await?))
}

async fn get_one(
    State(state): State<DisplayGroupsState>,
    Path(id): Path<i64>,
) -> Result<Json<DisplayGroup>, ApiError> {
    Ok(Json(state.display_groups.get(id).await?))
}

async fn create(
    State(state): State<DisplayGroupsState>,
    headers: HeaderMap,
    Json(payload): Json<DisplayGroupPayload>,
) -> Result<Json<DisplayGroup>, ApiError> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        AUDIT_RESOURCE,
        "POST",
        "/api/display-groups",
    )
    .await?;
    let created = state.display_groups.create(&payload).await?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "create",
        AUDIT_RESOURCE,
        Some(&created.id.to_string()),
        Some(audit_detail(&created)),
    )
    .await;
    Ok(Json(created))
}

async fn update(
    State(state): State<DisplayGroupsState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(payload): Json<DisplayGroupPayload>,
) -> Result<Json<DisplayGroup>, WriteRejection> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        AUDIT_RESOURCE,
        "PUT",
        "/api/display-groups/{id}",
    )
    .await?;
    let updated = state.display_groups.update(id, &payload).await?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "update",
        AUDIT_RESOURCE,
        Some(&id.to_string()),
        Some(audit_detail(&updated)),
    )
    .await;
    Ok(Json(updated))
}

async fn delete(
    State(state): State<DisplayGroupsState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        AUDIT_RESOURCE,
        "DELETE",
        "/api/display-groups/{id}",
    )
    .await?;
    let deleted = state.display_groups.delete(id).await?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "delete",
        AUDIT_RESOURCE,
        Some(&id.to_string()),
        Some(json!({ "name": deleted.name })),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn reorder(
    State(state): State<DisplayGroupsState>,
    headers: HeaderMap,
    Json(payload): Json<ReorderPayload>,
) -> Result<Json<Vec<DisplayGroup>>, ApiError> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        AUDIT_RESOURCE,
        "PUT",
        "/api/display-groups/order",
    )
    .await?;
    let groups = state.display_groups.reorder(&payload.ids).await?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "reorder",
        AUDIT_RESOURCE,
        None,
        Some(json!({ "ids": payload.ids })),
    )
    .await;
    Ok(Json(groups))
}

/// `/api/display-groups/*` のルーター（モジュール doc の表）。
pub(super) fn display_groups_router(
    display_groups: DisplayGroupService,
    audit: AuditLogService,
    auth: AuthState,
) -> Router {
    let state = DisplayGroupsState {
        display_groups,
        auth: auth.clone(),
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
        .layer(middleware::from_fn_with_state(auth, require_auth))
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
}
