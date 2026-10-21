//! REST の送信口 `POST /api/v1/ingest/{id}/values`（R2a-2c、
//! docs/banto-hub-ingest-design.md §6.2）。値を一括で送る送り手（スクリプト等）の口。
//! 常時つなぐ送り手（DB ブリッジ）は R2a-2d の WebSocket を使う。
//!
//! ## 1 要求の流れ
//!
//! 1. 認証の層 [`super::auth::require_ingest_auth`]（`route_layer`）: キー・パスの形・
//!    スコープ・接続の有無と有効（§6 の表）。**本文はまだ読まない**。
//! 2. 本文の読み込み: [`DefaultBodyLimit`] で **このルートだけ** 1 MiB
//!    （`PUSH_LIMITS.max_rest_body_bytes`）。超えたら 413 `body_too_large`。他の
//!    ルートは axum の既定（2 MiB）のまま。
//! 3. 形の検証（[`super::validate::parse_values_message`]）: 400 `invalid_message`。
//!    axum の `Json` は使わない（形の誤りが 422 になり、件数の 422
//!    `too_many_items` と区別できなくなるため）。`Content-Type` は見ない。
//! 4. 上限（[`IngestHub::admit`]、REST と WS で共有のバケット）: 422
//!    `too_many_items` / 429 `rate_limited`（`Retry-After`）。超過の端を監査
//!    （接続ごとに 1 時間に 20 組まで、`super::audit::LimitEdgeThrottle`）。
//! 5. 反映（[`IngestHub::apply`]、1 回の書き込みロック）: 409 `stale_epoch`
//!    （ブリッジの接続で epoch が無い・古い）、それ以外は 200 と件ごとの結果
//!    （一部を断っても 200。T20-3a の一括書き込みと同じ）。未知のキーを監査
//!    （間引き、[`super::audit::UnknownKeyThrottle`]）。
//!
//! 層の後で接続が消えた（commit と競った）`unknown_connection` の監査は
//! [`super::audit::refusal_rows`]（キーごとの間引きを通す、WS と共有）。
//!
//! 2〜5 で断ったものは何も反映せず、`last_seen_ms`（§4.3 の Live）も進めない。
//! 4 で断ったメッセージは頻度のバケットのメッセージを 1 つ使う（`super::limits` の
//! doc「判定の順序」）。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{DefaultBodyLimit, Extension, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use banto_server::AuthState;
use banto_tstore::Clock;
use serde::Serialize;
use serde_json::json;
use utoipa::ToSchema;

use crate::api_keys::ApiKeysService;
use crate::audit::AuditLogService;

use super::audit::{self as ingest_audit, IngestAuditRow, Transport};
use super::auth::{error_response, require_ingest_auth, IngestAuthState, IngestCaller};
use super::limits::{LimitKind, PUSH_LIMITS};
use super::validate::parse_values_message;
use super::{ApplyRefusal, IngestHub, RejectCode};

/// REST の送信口のパス。
pub const VALUES_PATH: &str = "/api/v1/ingest/{id}/values";

/// 200 の本文（§6.2）。
#[derive(Debug, Serialize, ToSchema)]
pub struct IngestValuesResponse {
    /// 受け付けた件数。
    pub accepted: usize,
    /// 件ごとに断ったもの（メッセージの中の位置・キー・理由）。
    pub rejected: Vec<IngestRejectedItem>,
    /// 収集が Running か。停止中も受け付ける（値は収集の開始後に記録される）。
    pub collecting: bool,
    /// この接続の今の epoch。ブリッジの接続は次の要求にこれを付ける。
    pub epoch: u64,
    /// この接続の送り手の途絶の判定時間。これより短い間隔で送る。
    pub stale_timeout_ms: u64,
}

/// 件ごとに断った 1 件（§6.3）。
#[derive(Debug, Serialize, ToSchema)]
pub struct IngestRejectedItem {
    pub index: usize,
    pub key: String,
    /// `unknown_key` / `inactive_key` / `not_in_bridge_set` / `duplicate_key` /
    /// `invalid_value` / `invalid_quality` / `invalid_time`
    pub code: String,
}

/// ハンドラの state。
#[derive(Clone)]
pub struct IngestRestState {
    pub ingest: Arc<IngestHub>,
    pub audit: AuditLogService,
    pub clock: Arc<dyn Clock>,
}

/// `POST /api/v1/ingest/{id}/values` のルーター。認証の層は `route_layer` で
/// このルートだけに被せる（パスの `{id}` を層が読むため、ルートに一致した後に
/// 走らせる）。本文の上限も同じくこのルートだけ。
pub fn ingest_rest_router(
    ingest: Arc<IngestHub>,
    clock: Arc<dyn Clock>,
    auth: AuthState,
    api_keys: ApiKeysService,
    audit: AuditLogService,
) -> Router {
    let auth_state = IngestAuthState {
        auth,
        api_keys,
        audit: audit.clone(),
        ingest: ingest.clone(),
        clock: clock.clone(),
        transport: Transport::Rest,
    };
    let body_limit = usize::try_from(PUSH_LIMITS.max_rest_body_bytes).unwrap_or(usize::MAX);
    Router::new()
        .route(
            VALUES_PATH,
            post(v1_ingest_values).layer(DefaultBodyLimit::max(body_limit)),
        )
        .route_layer(middleware::from_fn_with_state(
            auth_state,
            require_ingest_auth,
        ))
        .with_state(IngestRestState {
            ingest,
            audit,
            clock,
        })
}

/// 上限の一覧（422・429 の本文。送り手が合わせられるように）。
fn limits_body() -> serde_json::Value {
    json!({
        "max_items": PUSH_LIMITS.max_items_per_message,
        "max_body_bytes": PUSH_LIMITS.max_rest_body_bytes,
        "messages_per_sec": PUSH_LIMITS.messages_per_sec,
        "messages_burst": PUSH_LIMITS.messages_burst,
        "items_per_sec": PUSH_LIMITS.items_per_sec,
        "items_burst": PUSH_LIMITS.items_burst,
    })
}

fn refusal_response(refusal: ApplyRefusal) -> Response {
    match refusal {
        ApplyRefusal::UnknownConnection => {
            error_response(StatusCode::NOT_FOUND, refusal.as_str(), json!({}))
        }
        ApplyRefusal::ConnectionDisabled => {
            error_response(StatusCode::CONFLICT, refusal.as_str(), json!({}))
        }
        ApplyRefusal::StaleEpoch { epoch } => error_response(
            StatusCode::CONFLICT,
            refusal.as_str(),
            json!({ "epoch": epoch }),
        ),
    }
}

/// 値の一括送信（§6.2）。
#[utoipa::path(
    post,
    path = "/api/v1/ingest/{id}/values",
    params(("id" = i64, Path, description = "push 接続の id（10 進の正規形。改名しても変わらない）")),
    request_body(content = crate::ingest::validate::IngestValuesMessage, description = "1 MiB まで・1,000 件まで"),
    responses(
        (status = 200, description = "受け付けた（一部を断っても 200。件ごとの結果は rejected[]）", body = IngestValuesResponse),
        (status = 400, description = "invalid_message（JSON・形の誤り）"),
        (status = 401, description = "トークンなし・キーが存在しない / 失効 / 期限切れ"),
        (status = 403, description = "missing_ingest_scope / session_token_cannot_ingest / key_tripped"),
        (status = 404, description = "unknown_connection（パスの形の誤り・接続が無い・push でない）"),
        (status = 409, description = "connection_disabled / stale_epoch（本文の epoch は今の epoch）"),
        (status = 413, description = "body_too_large（本文が 1 MiB を超えた）"),
        (status = 422, description = "too_many_items（1 メッセージ 1,000 件を超えた）"),
        (status = 429, description = "rate_limited（接続ごとの頻度の上限。Retry-After）"),
    ),
    tag = "ingest",
)]
pub async fn v1_ingest_values(
    State(state): State<IngestRestState>,
    Extension(caller): Extension<IngestCaller>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let body = match body {
        Ok(body) => body,
        Err(rejection) if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE => {
            return error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "body_too_large",
                json!({ "max_body_bytes": PUSH_LIMITS.max_rest_body_bytes }),
            );
        }
        Err(rejection) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_message",
                json!({ "detail": rejection.body_text() }),
            );
        }
    };
    let message = match parse_values_message(&body) {
        Ok(message) => message,
        Err(err) => {
            let detail = match &err {
                super::validate::MessageError::InvalidMessage { detail } => detail.clone(),
            };
            return error_response(
                StatusCode::BAD_REQUEST,
                err.as_str(),
                json!({ "detail": detail }),
            );
        }
    };
    drop(body);

    let ctx = caller.audit_context();
    let conn_id = caller.connection_id;
    let now_ms = state.clock.now_ms();

    let admission = match state.ingest.admit(conn_id, message.items.len(), now_ms) {
        Ok(admission) => admission,
        Err(refusal) => {
            let rows = ingest_audit::refusal_rows(&state.ingest, &ctx, refusal, now_ms);
            ingest_audit::record(&state.audit, rows).await;
            return refusal_response(refusal);
        }
    };
    let edge_rows: Vec<IngestAuditRow> = admission
        .edges
        .iter()
        .map(|edge| IngestAuditRow::limit_edge(&ctx, edge))
        .collect();
    if let Err(limit) = admission.outcome {
        ingest_audit::record(&state.audit, edge_rows).await;
        return match limit.kind {
            LimitKind::TooManyItems => error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                limit.kind.error_code(),
                json!({ "limit": limit.kind.as_str(), "limits": limits_body() }),
            ),
            LimitKind::MessageRate | LimitKind::ItemRate => {
                let retry_after = limit.retry_after_secs.unwrap_or(1);
                let mut response = error_response(
                    StatusCode::TOO_MANY_REQUESTS,
                    limit.kind.error_code(),
                    json!({
                        "limit": limit.kind.as_str(),
                        "retry_after_secs": retry_after,
                        "limits": limits_body(),
                    }),
                );
                response
                    .headers_mut()
                    .insert(header::RETRY_AFTER, HeaderValue::from(retry_after));
                response
            }
        };
    }

    let outcome = state
        .ingest
        .apply(conn_id, message.epoch, &message.items, now_ms);
    let ack = match outcome {
        Ok(ack) => ack,
        Err(refusal) => {
            let mut rows = edge_rows;
            rows.extend(ingest_audit::refusal_rows(
                &state.ingest,
                &ctx,
                refusal,
                now_ms,
            ));
            ingest_audit::record(&state.audit, rows).await;
            return refusal_response(refusal);
        }
    };

    let unknown = ack
        .rejected
        .iter()
        .filter(|rejection| rejection.code == RejectCode::UnknownKey)
        .map(|rejection| rejection.key.as_str());
    let mut rows = edge_rows;
    rows.extend(
        state
            .ingest
            .throttle_unknown_keys(conn_id, unknown, now_ms)
            .iter()
            .map(|record| IngestAuditRow::unknown_key(&ctx, record)),
    );
    ingest_audit::record(&state.audit, rows).await;

    let response = IngestValuesResponse {
        accepted: ack.accepted,
        rejected: ack
            .rejected
            .into_iter()
            .map(|rejection| IngestRejectedItem {
                index: rejection.index,
                key: rejection.key,
                code: rejection.code.as_str().to_string(),
            })
            .collect(),
        collecting: ack.collecting,
        epoch: ack.epoch,
        stale_timeout_ms: ack.stale_timeout_ms,
    };
    (StatusCode::OK, Json(response)).into_response()
}
