//! `/api/free-trends/*`（#1207・#1231 PR 8a、docs/chronogazer/trend-design.md §2・§3・§9）。
//!
//! | Method | Path                     | Body               | Response |
//! |--------|--------------------------|--------------------|----------|
//! | GET    | `/api/free-trends`       | -                  | `FreeTrend[]`（名前順。各件に今の主体の `canEdit`） |
//! | POST   | `/api/free-trends`       | `FreeTrendPayload` | `FreeTrend`（アカウントのログイン（viewer 以上）と `localDesktop` だけ） |
//! | GET    | `/api/free-trends/{id}`  | -                  | `FreeTrend` |
//! | PUT    | `/api/free-trends/{id}`  | `FreeTrendPayload` | `FreeTrend`（作った人か editor 以上。版の食い違いは `409`） |
//! | DELETE | `/api/free-trends/{id}`  | -                  | 204（作った人か editor 以上） |
//!
//! 読み取りのためにキーを通す集まり（`rest::api_key_routes`）に入れる（`read`・`config` のキーで
//! 読める）。書き込みは [`FreeTrendOps`] が主体（[`Actor::free_trend_actor`]）で決め、ログイン不要
//! モード（REST には無く、デスクトップだけ）・閲覧公開・API キーは役割・スコープに関わらず 403 で
//! 断って `denied` を監査する（`crate::free_trends` のモジュール doc「権限」）。成功した書き込みは
//! [`Actor::record`] で監査する（`resource: "free_trends"`、`origin: "rest"`）。CSRF は他の
//! ルートと同じ（セッションは `X-Banto-Client` が要る。`api_router` の層）。
//!
//! 作成・更新・削除は、権限の確かめと監査まで含めて [`run_detached`] で切り離す（接続が切れても、
//! コミット済みの変更だけが残って監査が欠けないように。`docs/implementation-checklist.md` の
//! 「handler の中で `await` をまたぐ副作用」）。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::middleware;
use axum::routing::get;
use axum::{Json, Router};
use banto_server::{ApiError, AuthState};

use super::{require_session_or_api_key, Actor, RevisionConflictRejection};
use crate::audit::AuditLogService;
use crate::free_trends::{FreeTrend, FreeTrendPayload, FreeTrendService};
use crate::local_grant::run_detached;
use crate::ops::{FreeTrendOps, Via};

#[utoipa::path(
    get,
    path = "/api/free-trends",
    tag = "free-trends",
    operation_id = "free_trends_list",
    summary = "自由トレンドの一覧",
    description = "自由トレンドの一覧（名前順）。読み取り（`read` キーで足りる）。各件の `canEdit` は、今の主体がその自由トレンドを変えられるか（API キーは常に false）。",
    responses(
        (status = 200, description = "自由トレンドの一覧", body = Vec<FreeTrend>),
        (status = 401, description = "API キーが無い・誤り・失効・期限切れ、またはログインのセッションが無い"),
    ),
    security(("api_key" = [])),
)]
pub(super) async fn list(
    State(state): State<FreeTrendOps>,
    actor: Actor,
) -> Result<Json<Vec<FreeTrend>>, ApiError> {
    Ok(Json(
        state.free_trends.list(&actor.free_trend_actor()).await?,
    ))
}

#[utoipa::path(
    get,
    path = "/api/free-trends/{id}",
    tag = "free-trends",
    operation_id = "free_trends_get",
    summary = "自由トレンドの取得",
    description = "自由トレンドを 1 件返す。読み取り（`read` キーで足りる）。",
    params(("id" = i64, Path, description = "対象の id")),
    responses(
        (status = 200, description = "自由トレンド", body = FreeTrend),
        (status = 401, description = "API キーが無い・誤り・失効・期限切れ、またはログインのセッションが無い"),
        (status = 404, description = "指定の id が無い"),
    ),
    security(("api_key" = [])),
)]
pub(super) async fn get_one(
    State(state): State<FreeTrendOps>,
    actor: Actor,
    Path(id): Path<i64>,
) -> Result<Json<FreeTrend>, ApiError> {
    Ok(Json(
        state.free_trends.get(&actor.free_trend_actor(), id).await?,
    ))
}

#[utoipa::path(
    post,
    path = "/api/free-trends",
    tag = "free-trends",
    operation_id = "free_trends_create",
    summary = "自由トレンドの作成",
    description = "自由トレンドを作る。**API キーでは断る（403）。ログインのセッションが要る**（`Authorization: Bearer` のセッションのトークンと `X-Banto-Client` の見出し。`config` のキーでも書けないので、この操作には `security` を宣言しない）。アカウントのログイン（viewer 以上）とこの PC からの接続（localDesktop）だけが作れる。閲覧公開のセッションも 403。ペンは登録済みのタグ（`tagId`）だけ。",
    request_body = FreeTrendPayload,
    responses(
        (status = 200, description = "作成した自由トレンド", body = FreeTrend),
        (status = 401, description = "API キーが無い・誤り・失効・期限切れ、またはログインのセッションが無い"),
        (status = 403, description = "API キー（`read`・`config` とも。ログインのセッションが要る）、または閲覧公開などのログインなしのセッション"),
        (status = 422, description = "入力の検証エラー（`field_errors` に項目ごとの理由）"),
    ),
)]
pub(super) async fn create(
    State(state): State<FreeTrendOps>,
    actor: Actor,
    Json(payload): Json<FreeTrendPayload>,
) -> Result<Json<FreeTrend>, RevisionConflictRejection> {
    let created = run_detached("chronogazer: 自由トレンドの作成", async move {
        state
            .create(
                &actor,
                &Via::Rest {
                    method: "POST",
                    path: "/api/free-trends",
                },
                &payload,
            )
            .await
    })
    .await??;
    Ok(Json(created))
}

#[utoipa::path(
    put,
    path = "/api/free-trends/{id}",
    tag = "free-trends",
    operation_id = "free_trends_update",
    summary = "自由トレンドの更新",
    description = "自由トレンドを更新する（全項目を送る。ペンは丸ごと置き換え）。**API キーでは断る（403）。ログインのセッションが要る**（`Authorization: Bearer` のセッションのトークンと `X-Banto-Client` の見出し。`config` のキーでも書けないので、この操作には `security` を宣言しない）。作った人か editor 以上だけ。",
    params(("id" = i64, Path, description = "対象の id")),
    request_body = FreeTrendPayload,
    responses(
        (status = 200, description = "更新後の自由トレンド", body = FreeTrend),
        (status = 401, description = "API キーが無い・誤り・失効・期限切れ、またはログインのセッションが無い"),
        (status = 403, description = "API キー（`read`・`config` とも。ログインのセッションが要る）、作った人でも editor 以上でもない、またはログインなしのセッション"),
        (status = 404, description = "指定の id が無い"),
        (status = 409, description = "`expectedRevision` が古い（他で先に更新された。本文は `kind: validation`、`field_errors` の `expectedRevision`）"),
        (status = 422, description = "入力の検証エラー（`field_errors` に項目ごとの理由）"),
    ),
)]
pub(super) async fn update(
    State(state): State<FreeTrendOps>,
    actor: Actor,
    Path(id): Path<i64>,
    Json(payload): Json<FreeTrendPayload>,
) -> Result<Json<FreeTrend>, RevisionConflictRejection> {
    let updated = run_detached("chronogazer: 自由トレンドの更新", async move {
        state
            .update(
                &actor,
                &Via::Rest {
                    method: "PUT",
                    path: "/api/free-trends/{id}",
                },
                id,
                &payload,
            )
            .await
    })
    .await??;
    Ok(Json(updated))
}

#[utoipa::path(
    delete,
    path = "/api/free-trends/{id}",
    tag = "free-trends",
    operation_id = "free_trends_delete",
    summary = "自由トレンドの削除",
    description = "自由トレンドを削除する。**API キーでは断る（403）。ログインのセッションが要る**（`Authorization: Bearer` のセッションのトークンと `X-Banto-Client` の見出し。`config` のキーでも書けないので、この操作には `security` を宣言しない）。作った人か editor 以上だけ。",
    params(("id" = i64, Path, description = "対象の id")),
    responses(
        (status = 204, description = "削除した"),
        (status = 401, description = "API キーが無い・誤り・失効・期限切れ、またはログインのセッションが無い"),
        (status = 403, description = "API キー（`read`・`config` とも。ログインのセッションが要る）、作った人でも editor 以上でもない、またはログインなしのセッション"),
        (status = 404, description = "指定の id が無い"),
    ),
)]
pub(super) async fn delete(
    State(state): State<FreeTrendOps>,
    actor: Actor,
    Path(id): Path<i64>,
) -> Result<StatusCode, RevisionConflictRejection> {
    run_detached("chronogazer: 自由トレンドの削除", async move {
        state
            .delete(
                &actor,
                &Via::Rest {
                    method: "DELETE",
                    path: "/api/free-trends/{id}",
                },
                id,
            )
            .await
    })
    .await??;
    Ok(StatusCode::NO_CONTENT)
}

/// `/api/free-trends/*` のルーター（モジュール doc の表）。
pub(super) fn free_trends_router(
    free_trends: FreeTrendService,
    audit: AuditLogService,
    auth: AuthState,
) -> Router {
    let state = FreeTrendOps { free_trends, audit };
    Router::new()
        .route("/api/free-trends", get(list).post(create))
        .route(
            "/api/free-trends/{id}",
            get(get_one).put(update).delete(delete),
        )
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            auth,
            require_session_or_api_key,
        ))
}
