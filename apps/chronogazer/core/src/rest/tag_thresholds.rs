//! `/api/tag-thresholds/*`（#532、記録計の側のタグごとのしきい値）。
//!
//! | Method | Path                            | Body                   | Response |
//! |--------|---------------------------------|------------------------|----------|
//! | GET    | `/api/tag-thresholds`           | -                      | `TagThresholds[]`（viewer 以上。設定のある（行のある）タグだけ、タグ ID 順） |
//! | GET    | `/api/tag-thresholds/{tagId}`   | -                      | `TagThresholds`（viewer 以上。設定の無いタグは 4 つとも `null`・`revision: 0`。タグが無ければ `404`） |
//! | PUT    | `/api/tag-thresholds/{tagId}`   | `TagThresholdsPayload` | `TagThresholds`（editor 以上。全項目置換。版の食い違いは `409`） |
//!
//! 作法は表示グループ（`rest::display_groups`）と同じ: 読み取りはルーター全体の
//! `require_auth`（viewer 以上）、書き込みは [`require_editor`]（拒否は `denied` で
//! 監査）、成功した書き込みだけ [`record_write`] で監査する
//! （`resource: "tag_thresholds"`、`origin: "rest"`）。検証は
//! `crate::tag_thresholds::validate_thresholds_for` だけで、Tauri の
//! `tag_thresholds_*` コマンドも同じサービス・同じ検証を通る。
//!
//! 版の食い違い（`expectedRevision`）だけ `409 Conflict` にし、本文は他の検証
//! エラーと同じ `ErrorBody` 形（`kind: "validation"`、`field_errors` の
//! `expectedRevision`）にする - タグ（#525）・表示グループ（#393）と同じ形。
//! 削除の口は無い（4 つとも `null` で保存すれば設定なし）。タグを消すと、その
//! タグの行も一緒に消える（`DisplayGroupService::delete_tag_unless_referenced`）。

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::middleware;
use axum::routing::get;
use axum::{Json, Router};
use banto_server::routes::record_write;
use banto_server::{require_auth, ApiError, AuthState};

use super::{require_editor, RevisionConflictRejection};
use crate::audit::AuditLogService;
use crate::tag_thresholds::{
    audit_detail, TagThresholdService, TagThresholds, TagThresholdsPayload, AUDIT_RESOURCE,
};

#[derive(Clone)]
struct TagThresholdsState {
    tag_thresholds: TagThresholdService,
    auth: AuthState,
    audit: AuditLogService,
}

async fn list(
    State(state): State<TagThresholdsState>,
) -> Result<Json<Vec<TagThresholds>>, ApiError> {
    Ok(Json(state.tag_thresholds.list().await?))
}

async fn get_one(
    State(state): State<TagThresholdsState>,
    Path(tag_id): Path<i64>,
) -> Result<Json<TagThresholds>, ApiError> {
    Ok(Json(state.tag_thresholds.get(tag_id).await?))
}

async fn update(
    State(state): State<TagThresholdsState>,
    headers: HeaderMap,
    Path(tag_id): Path<i64>,
    Json(payload): Json<TagThresholdsPayload>,
) -> Result<Json<TagThresholds>, RevisionConflictRejection> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        AUDIT_RESOURCE,
        "PUT",
        "/api/tag-thresholds/{tagId}",
    )
    .await?;
    let updated = state.tag_thresholds.update(tag_id, &payload).await?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "update",
        AUDIT_RESOURCE,
        Some(&tag_id.to_string()),
        Some(audit_detail(&updated)),
    )
    .await;
    Ok(Json(updated.thresholds))
}

/// `/api/tag-thresholds/*` のルーター（モジュール doc の表）。
pub(super) fn tag_thresholds_router(
    tag_thresholds: TagThresholdService,
    audit: AuditLogService,
    auth: AuthState,
) -> Router {
    let state = TagThresholdsState {
        tag_thresholds,
        auth: auth.clone(),
        audit,
    };
    Router::new()
        .route("/api/tag-thresholds", get(list))
        .route("/api/tag-thresholds/{tag_id}", get(get_one).put(update))
        .with_state(state)
        .layer(middleware::from_fn_with_state(auth, require_auth))
}

#[cfg(test)]
mod tests;
