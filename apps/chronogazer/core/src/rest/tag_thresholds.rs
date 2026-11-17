//! `/api/tag-thresholds/*`（#532、記録計の側のタグごとのしきい値）。
//!
//! | Method | Path                            | Body                   | Response |
//! |--------|---------------------------------|------------------------|----------|
//! | GET    | `/api/tag-thresholds`           | -                      | `TagThresholds[]`（viewer 以上。設定のある（行のある）タグだけ、タグ ID 順） |
//! | GET    | `/api/tag-thresholds/{tagId}`   | -                      | `TagThresholds`（viewer 以上。設定の無いタグは 4 つとも `null`・`revision: 0`。タグが無ければ `404`） |
//! | PUT    | `/api/tag-thresholds/{tagId}`   | `TagThresholdsPayload` | `TagThresholds`（editor 以上。全項目置換。版の食い違いは `409`） |
//!
//! 作法は表示グループ（`rest::display_groups`）と同じ: 読み取りはルーター全体の
//! [`require_session_or_api_key`]（viewer 以上 / API キーの `read`・`config`）、
//! 書き込みは [`ThresholdOps::update`]（#1161 PR5a。editor 以上 / `config`。
//! 拒否は `denied` で監査）、成功した書き込みだけ [`Actor::record`] で監査する
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
use axum::middleware;
use axum::routing::get;
use axum::{Json, Router};
use banto_server::{ApiError, AuthState};

use super::{require_session_or_api_key, Actor, RevisionConflictRejection};
use crate::audit::AuditLogService;
use crate::ops::{ThresholdOps, Via};
use crate::tag_thresholds::{TagThresholdService, TagThresholds, TagThresholdsPayload};

/// #1161 PR5a: 書き込みの本体（権限の確かめ・監査）は [`ThresholdOps::update`]
/// で、MCP のツールも同じものを呼ぶ。
type TagThresholdsState = ThresholdOps;

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
    actor: Actor,
    Path(tag_id): Path<i64>,
    Json(payload): Json<TagThresholdsPayload>,
) -> Result<Json<TagThresholds>, RevisionConflictRejection> {
    let updated = state
        .update(
            &actor,
            &Via::Rest {
                method: "PUT",
                path: "/api/tag-thresholds/{tagId}",
            },
            tag_id,
            &payload,
        )
        .await?;
    Ok(Json(updated))
}

/// `/api/tag-thresholds/*` のルーター（モジュール doc の表）。
pub(super) fn tag_thresholds_router(
    tag_thresholds: TagThresholdService,
    audit: AuditLogService,
    auth: AuthState,
) -> Router {
    let state = TagThresholdsState {
        tag_thresholds,
        audit,
    };
    Router::new()
        .route("/api/tag-thresholds", get(list))
        .route("/api/tag-thresholds/{tag_id}", get(get_one).put(update))
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            auth,
            require_session_or_api_key,
        ))
}

#[cfg(test)]
mod tests;
