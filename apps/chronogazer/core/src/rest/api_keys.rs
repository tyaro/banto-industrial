//! `/api/api-keys*`（#1161 PR4、docs/chronogazer/chronogazer-agent-api-design.md §3.4）。
//!
//! | Method | Path                          | Body                                  | Response |
//! |--------|-------------------------------|---------------------------------------|----------|
//! | GET    | `/api/api-keys`               | -                                     | `ApiKeySummary[]`（作成順。ハッシュは返さない） |
//! | POST   | `/api/api-keys`               | `{ name, scopes, expiresAt? }`        | 201 `{ id, name, prefix, scopes, key }`（平文のキーはこの応答だけ） |
//! | POST   | `/api/api-keys/{id}/revoke`   | -                                     | `ApiKeySummary`（冪等） |
//! | DELETE | `/api/api-keys/{id}`          | -                                     | 204。失効していないキーは 409、無い id は 404 |
//!
//! **ログインした管理者のセッションだけ**（`users_router` と同じ `require_auth` →
//! `RoleGuard`（admin））。キーでは呼べない: このルーターは `api_router` のキーを
//! 通さない集まりにあり、キーの主体は 403（§2「MCP からのキーの発行・失効」と同じ
//! 理由 - エージェントが自分で権限を増やせないように）。
//!
//! スコープは `read` と `config` だけ（`crate::api_keys::CgScopes`）。期限は任意で、
//! 指定するなら今より後（HUB と同じ検証と文言）。発行・失効・削除は監査に残す
//! （主体は操作した管理者。キーの平文とハッシュは載せない）。変更と監査は
//! [`run_detached`] で切り離す（接続が切れても変更だけが確定して監査が欠けないように。
//! `docs/implementation-checklist.md` の「handler の中で `await` をまたぐ副作用」）。
//!
//! 一時停止（`tripped_at`）は ChronoGazer では付けないので、解除の口
//! （HUB の `clear-trip`）は無い。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use banto_core::{BantoError, ErrorBody, FieldError};
use banto_server::routes::{record_write, require_role_at_least, AuditActor, RoleGuard};
use banto_server::{require_auth, ApiError, AuthState};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::api_keys::{ApiKeyDeleteOutcome, ApiKeySummary, ApiKeysService, IssuedApiKey};
use crate::audit::AuditLogService;
use crate::local_grant::{run_detached, DetachedTaskFailed};
use crate::users::Role;

/// 監査の `resource`（HUB と同じ）。
const AUDIT_RESOURCE: &str = "api_keys";

#[derive(Clone)]
struct ApiKeysAdminState {
    api_keys: ApiKeysService,
    audit: AuditLogService,
}

/// `POST /api/api-keys` の本文。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateApiKeyRequest {
    name: String,
    scopes: Vec<String>,
    /// 任意の期限（epoch ミリ秒）。省略・`null` は無期限。
    #[serde(default)]
    expires_at: Option<i64>,
}

/// `POST /api/api-keys` の応答（HUB の `IssuedApiKeyResponse` と同じ形）。
#[derive(Debug, Serialize)]
struct IssuedApiKeyResponse {
    id: i64,
    name: String,
    prefix: String,
    scopes: Vec<String>,
    /// 平文のキー全体。この応答限り。
    key: String,
}

impl From<IssuedApiKey> for IssuedApiKeyResponse {
    fn from(issued: IssuedApiKey) -> Self {
        Self {
            id: issued.id,
            name: issued.name,
            prefix: issued.prefix,
            scopes: issued.scopes,
            key: issued.key,
        }
    }
}

/// 変更の口の失敗（サービスの誤り、または切り離したタスクが最後まで走らなかった）。
enum RouteError {
    Api(ApiError),
    Detached(DetachedTaskFailed),
}

impl From<BantoError> for RouteError {
    fn from(err: BantoError) -> Self {
        Self::Api(ApiError(err))
    }
}

impl From<DetachedTaskFailed> for RouteError {
    fn from(err: DetachedTaskFailed) -> Self {
        Self::Detached(err)
    }
}

impl IntoResponse for RouteError {
    fn into_response(self) -> Response {
        match self {
            Self::Api(err) => err.into_response(),
            Self::Detached(err) => err.into_response(),
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// 期限は今より後に限る（HUB の `api_keys_create` と同じ文言）。
fn validate_expires_at(expires_at: Option<i64>, now_ms: i64) -> Result<(), BantoError> {
    match expires_at {
        Some(expires_at) if expires_at <= now_ms => Err(BantoError::Validation {
            field_errors: vec![FieldError {
                field: "expiresAt".to_string(),
                message: "有効期限は現在時刻より後の日時を指定してください".to_string(),
            }],
        }),
        _ => Ok(()),
    }
}

async fn api_keys_list(
    State(state): State<ApiKeysAdminState>,
) -> Result<Json<Vec<ApiKeySummary>>, ApiError> {
    Ok(Json(state.api_keys.list().await?))
}

async fn api_keys_create(
    State(state): State<ApiKeysAdminState>,
    axum::Extension(actor): axum::Extension<AuditActor>,
    Json(body): Json<CreateApiKeyRequest>,
) -> Result<Response, RouteError> {
    validate_expires_at(body.expires_at, now_ms())?;
    let issued = run_detached("chronogazer: API キーの発行", async move {
        let issued = state
            .api_keys
            .issue(&body.name, body.scopes, body.expires_at)
            .await?;
        record_write(
            &state.audit,
            &actor,
            "create",
            AUDIT_RESOURCE,
            Some(&issued.id.to_string()),
            Some(json!({
                "name": issued.name,
                "prefix": issued.prefix,
                "scopes": issued.scopes,
                "expiresAt": body.expires_at,
            })),
        )
        .await;
        Ok::<_, BantoError>(issued)
    })
    .await??;
    Ok((
        StatusCode::CREATED,
        Json(IssuedApiKeyResponse::from(issued)),
    )
        .into_response())
}

async fn api_keys_revoke(
    State(state): State<ApiKeysAdminState>,
    axum::Extension(actor): axum::Extension<AuditActor>,
    Path(id): Path<i64>,
) -> Result<Json<ApiKeySummary>, RouteError> {
    let summary = run_detached("chronogazer: API キーの失効", async move {
        let summary = state.api_keys.revoke(id).await?;
        record_write(
            &state.audit,
            &actor,
            "revoke",
            AUDIT_RESOURCE,
            Some(&id.to_string()),
            Some(json!({ "name": summary.name, "prefix": summary.prefix })),
        )
        .await;
        Ok::<_, BantoError>(summary)
    })
    .await??;
    Ok(Json(summary))
}

async fn api_keys_delete(
    State(state): State<ApiKeysAdminState>,
    axum::Extension(actor): axum::Extension<AuditActor>,
    Path(id): Path<i64>,
) -> Result<Response, RouteError> {
    let outcome = run_detached("chronogazer: API キーの削除", async move {
        let outcome = state.api_keys.delete(id).await?;
        if let ApiKeyDeleteOutcome::Deleted(summary) = &outcome {
            record_write(
                &state.audit,
                &actor,
                "delete",
                AUDIT_RESOURCE,
                Some(&id.to_string()),
                Some(json!({ "name": summary.name, "prefix": summary.prefix })),
            )
            .await;
        }
        Ok::<_, BantoError>(outcome)
    })
    .await??;
    Ok(match outcome {
        ApiKeyDeleteOutcome::Deleted(_) => StatusCode::NO_CONTENT.into_response(),
        ApiKeyDeleteOutcome::StillActive => (
            StatusCode::CONFLICT,
            Json(ErrorBody::Other {
                message: "有効な API キーは削除できません。先に失効させてください".to_string(),
            }),
        )
            .into_response(),
    })
}

/// `/api/api-keys*` のルーター（モジュール doc の表）。
pub(super) fn api_keys_router(
    api_keys: ApiKeysService,
    audit: AuditLogService,
    auth: AuthState,
) -> Router {
    let state = ApiKeysAdminState {
        api_keys,
        audit: audit.clone(),
    };
    Router::new()
        .route("/api/api-keys", get(api_keys_list).post(api_keys_create))
        .route("/api/api-keys/{id}", axum::routing::delete(api_keys_delete))
        .route("/api/api-keys/{id}/revoke", post(api_keys_revoke))
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            RoleGuard {
                auth: auth.clone(),
                min: Role::Admin,
                resource: AUDIT_RESOURCE,
                audit,
            },
            require_role_at_least,
        ))
        .layer(middleware::from_fn_with_state(auth, require_auth))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expires_at_must_be_in_the_future() {
        assert!(validate_expires_at(None, 1_000).is_ok());
        assert!(validate_expires_at(Some(1_001), 1_000).is_ok());
        assert!(validate_expires_at(Some(1_000), 1_000).is_err());
        assert!(validate_expires_at(Some(0), 1_000).is_err());
    }
}
