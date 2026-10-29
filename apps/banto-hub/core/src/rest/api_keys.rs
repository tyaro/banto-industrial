//! API キー管理 API（`/api/api-keys*`）。
//!
//! `rest.rs` から中身を変えずに移したもの（docs/banto-hub-driver-refactor-plan.md §8.1、
//! P0-1 の手順 2）。共通部品（認証・ガード・監査の補助）は親の `rest.rs` にあり、
//! `use super::{...}` で使う。外から使われるパス（`crate::rest::X`）は親の re-export で
//! 保つ。

use super::{
    get, json, middleware, post, record_write, require_session, require_session_role, ApiError,
    ApiKeysService, Arc, AuditLogService, AuthGate, AuthState, BantoError, CollectorManager,
    Deserialize, ErrorBody, FieldError, HeaderMap, IntoResponse, IssuedApiKey, Json, OperationKind,
    Path, Response, Role, Router, Serialize, SessionRoleGuard, State, StatusCode, ToSchema,
};
use crate::api_keys::ApiKeyDeleteOutcome;
use axum::routing::delete;

// --- API キー管理 (docs/tag-server-design.md §5.6・T0-2 実装指示 §1「管理
// REST」): admin ロール限定、CSRF + bearer セッション（このルーター自体は
// `crate::api_keys::ApiKeysService`（発行される bh_ キー）を消費する側では
// なく、管理 UI セッションから叩く前提 - `/api/v1/*` の API キー認証とは
// 別物）。---------------------------------------------------------------------

#[derive(Clone)]
struct ApiKeysAdminState {
    api_keys: ApiKeysService,
    auth: AuthState,
    audit: AuditLogService,
    /// H10 ①: `api_keys_create` の「有効期限は未来限定」検証で使う時計
    /// （`manager.clock()`）。他の `*AdminState`（`WriteControlAdminState`
    /// 等）が `manager` を持つのと同じ規約 - テストでは
    /// `ManualClock` に差し替えられる。
    manager: Arc<CollectorManager>,
}

// T21 S3（docs/banto-hub-t21-design.md、構成補助 MCP の
// create_api_key）: `crate::mcp`の`create_api_key`ツールが同じ
// request 型・「現在時刻より未来」検証を再利用する（`GrpcSettingsBody`等と
// 同じ`pub(crate)`化の理由 - 二重実装しない）。フィールドも
// `pub(crate)`にする必要がある点が`GrpcSettingsBody`と同じ（応答専用の
// `IssuedApiKeyResponse`と違い、呼び出し元が`name`/`scopes`/`expires_at`を
// 個別に読む）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateApiKeyRequest {
    pub(crate) name: String,
    pub(crate) scopes: Vec<String>,
    /// H10 ①（docs/improvement-plan.md、2026-08-08 オーナー決定）: 任意の
    /// 有効期限（絶対 epoch ミリ秒、wire は `expiresAt` -
    /// `crate::api_keys::ApiKeySummary` の `expires_at`/`FieldError::field`
    /// 命名規約と同じ camelCase に揃えるため、この構造体自体にも
    /// `rename_all = "camelCase"` を追加した - `name`/`scopes` は
    /// 1語なので実質無変化）。省略/`null` = 無期限（既定・動作不変、
    /// `#[serde(default)]` は既存クライアントの後方互換のため -
    /// `GrpcSettingsBody::bind` 等と同じ規約）。`Some` の場合は
    /// [`api_keys_create`]（および`crate::mcp`の`create_api_key`）が
    /// 「現在時刻より未来」を検証してから[`ApiKeysService::issue`]に渡す
    /// （`issue` 自体は再検証しない）。
    #[serde(default)]
    pub(crate) expires_at: Option<i64>,
}

/// `POST /api/api-keys` の応答 - `IssuedApiKey` をそのまま返すと `key`
/// フィールド名がスネークケースのままになる（`crate::api_keys` は機械
/// クライアント向け `/api/v1/*` と同じ snake_case 規約）ので、それに
/// 合わせてここでも変換なしでそのまま公開する（T0-2 実装指示の応答例
/// `{ "id", "name", "prefix", "scopes", "key": "bh_..." }` と一致）。
///
/// H10 ①で `expiresAt` を追加していない: 発行応答は元々
/// `created_at`/`revoked_at`/`tripped_at` も含まない最小限の形（「平文
/// key を一度だけ返す」ことが主目的）で、入力どおりの値をそのまま返すだけの
/// `expiresAt` もこの最小性に合わせた - 必要なら直後の `GET /api/api-keys`
/// 一覧（`crate::api_keys::ApiKeySummary`）で確認できる。
// T21 S3: `crate::mcp`の`create_api_key`ツールが同じ応答型を再利用する
// （`MqttSettingsResponse`等と同じ`pub(crate)`化の理由）。応答専用型
// なので（`CreateApiKeyRequest`と違い）フィールド自体は`pub(crate)`に
// しない - `From<IssuedApiKey>`経由で構築し`Serialize`するだけで、
// 呼び出し元が個々のフィールドを読むことはない。
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct IssuedApiKeyResponse {
    id: i64,
    name: String,
    prefix: String,
    scopes: Vec<String>,
    /// 平文キー全体。この応答限りでしか手に入らない（設計: 「key はこの
    /// 応答限り」）。
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

/// `POST /api/api-keys` - 発行。監査ログには **キー平文・ハッシュを
/// 含めない**（設計 T0-2 実装指示: 「監査ログに record_write — ただし
/// キー平文・ハッシュは監査 detail に入れない」）。`expiresAt`（H10 ①）は
/// 秘密ではないので監査 detail に含めてよい。
///
/// `expiresAt` の「未来限定」検証はここで行う（`crate::api_keys` の
/// サービス層は `now_ms` を持たないため） - `state.manager.clock()` は
/// `require_tag_space_auth` 等と同じ、テストで差し替え可能な時計。
/// 不正なら `validate_scopes`/重複名と同じ `BantoError::Validation` 経由の
/// 4xx で弾く（新しい HTTP パスは作らない）。
async fn api_keys_create(
    State(state): State<ApiKeysAdminState>,
    headers: HeaderMap,
    Json(body): Json<CreateApiKeyRequest>,
) -> Result<(StatusCode, Json<IssuedApiKeyResponse>), ApiError> {
    if let Some(expires_at) = body.expires_at {
        let now_ms = state.manager.clock().now_ms();
        if expires_at <= now_ms {
            return Err(ApiError(BantoError::Validation {
                field_errors: vec![FieldError {
                    field: "expiresAt".to_string(),
                    message: "有効期限は現在時刻より後の日時を指定してください".to_string(),
                }],
            }));
        }
    }

    // クライアントの切断で、変更だけが確定して監査が欠けることがないよう切り離す（#940 のレビュー）。
    let issued = tokio::spawn(async move {
        let issued = state
            .api_keys
            .issue(&body.name, body.scopes, body.expires_at)
            .await?;
        record_write(
            &state.audit,
            &state.auth,
            &headers,
            "create",
            "api_keys",
            Some(&issued.id.to_string()),
            Some(json!({
                "name": issued.name,
                "scopes": issued.scopes,
                "expiresAt": body.expires_at,
            })),
        )
        .await;
        Ok::<_, BantoError>(issued)
    })
    .await
    .map_err(|err| ApiError(BantoError::Other(format!("API キー発行タスク: {err}"))))??;
    Ok((StatusCode::CREATED, Json(issued.into())))
}

/// `GET /api/api-keys` - 一覧（`key_hash` は含まない、設計: 「key_hash は
/// 返さない」）。
async fn api_keys_list(
    State(state): State<ApiKeysAdminState>,
) -> Result<Json<Vec<crate::api_keys::ApiKeySummary>>, ApiError> {
    Ok(Json(state.api_keys.list().await?))
}

/// `POST /api/api-keys/{id}/revoke` - 失効（冪等）。物理削除は失効の後に
/// [`api_keys_delete`]（#713）。
async fn api_keys_revoke(
    State(state): State<ApiKeysAdminState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<crate::api_keys::ApiKeySummary>, ApiError> {
    // クライアントの切断で、変更だけが確定して監査が欠けることがないよう切り離す（#940 のレビュー）。
    let summary = tokio::spawn(async move {
        let summary = state.api_keys.revoke(id).await?;
        record_write(
            &state.audit,
            &state.auth,
            &headers,
            "revoke",
            "api_keys",
            Some(&id.to_string()),
            None,
        )
        .await;
        Ok::<_, BantoError>(summary)
    })
    .await
    .map_err(|err| ApiError(BantoError::Other(format!("API キー失効タスク: {err}"))))??;
    Ok(Json(summary))
}

/// `DELETE /api/api-keys/{id}` - 失効済みキーの削除（#713、2026-10-29
/// オーナー決定。それまでは「DELETE は設けない（失効履歴を残す方針）」）。
/// 削除すると同じ名前で発行し直せる。失効していないキーは `409`（先に
/// `revoke`）、存在しない id は `404`。書込み監査などはキーの名前と
/// プレフィックスのスナップショットを持つので、削除の後も読める。監査ログ
/// には名前とプレフィックスを `delete` として残す。成功は `204`。
async fn api_keys_delete(
    State(state): State<ApiKeysAdminState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Response, ApiError> {
    // クライアントの切断で、変更だけが確定して監査が欠けることがないよう切り離す（#940 のレビュー）。
    let outcome = tokio::spawn(async move {
        let outcome = state.api_keys.delete(id).await?;
        if let ApiKeyDeleteOutcome::Deleted(summary) = &outcome {
            record_write(
                &state.audit,
                &state.auth,
                &headers,
                "delete",
                "api_keys",
                Some(&id.to_string()),
                Some(json!({ "name": summary.name, "prefix": summary.prefix })),
            )
            .await;
        }
        Ok::<_, BantoError>(outcome)
    })
    .await
    .map_err(|err| ApiError(BantoError::Other(format!("API キー削除タスク: {err}"))))??;
    match outcome {
        ApiKeyDeleteOutcome::Deleted(_) => Ok(StatusCode::NO_CONTENT.into_response()),
        ApiKeyDeleteOutcome::StillActive => Ok((
            StatusCode::CONFLICT,
            Json(ErrorBody::Other {
                message: "有効な API キーは削除できません。先に失効させてください".to_string(),
            }),
        )
            .into_response()),
    }
}

/// `POST /api/api-keys/{id}/clear-trip` - トリップ解除（冪等、T2-4・
/// 設計 §6-4「復帰は管理 UI から手動」）。`revoke` と違い `tripped_at` は
/// `NULL` に戻せる - `crate::api_keys` のモジュール doc comment「トリップ」
/// 参照。
async fn api_keys_clear_trip(
    State(state): State<ApiKeysAdminState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<crate::api_keys::ApiKeySummary>, ApiError> {
    // クライアントの切断で、変更だけが確定して監査が欠けることがないよう切り離す（#940 のレビュー）。
    let summary = tokio::spawn(async move {
        let summary = state.api_keys.clear_trip(id).await?;
        record_write(
            &state.audit,
            &state.auth,
            &headers,
            "clear_trip",
            "api_keys",
            Some(&id.to_string()),
            None,
        )
        .await;
        Ok::<_, BantoError>(summary)
    })
    .await
    .map_err(|err| {
        ApiError(BantoError::Other(format!(
            "API キートリップ解除タスク: {err}"
        )))
    })??;
    Ok(Json(summary))
}

/// `/api/api-keys/*`（設計 §5.6・T0-2 実装指示: 「管理系ルーターに追加 —
/// CSRF + bearer + RBAC admin 限定」）。T0-2 実装指示は発行/一覧/失効
/// いずれも admin 限定と明記しているため、[`require_editor`]（editor 以上）
/// ではなく [`SessionRoleGuard`]（admin ちょうど）をルーター全体に掛ける -
/// `users_router`/`audit_log_router` と同型（ハンドラ内で個別に role
/// チェックし直さない: 到達した時点で呼び出し元は admin であることが
/// ルーター層で保証済み）。
pub(super) fn api_keys_router(
    api_keys: ApiKeysService,
    audit: AuditLogService,
    auth: AuthState,
    manager: Arc<CollectorManager>,
) -> Router {
    let state = ApiKeysAdminState {
        api_keys,
        auth: auth.clone(),
        audit: audit.clone(),
        manager,
    };
    Router::new()
        .route("/api/api-keys", get(api_keys_list).post(api_keys_create))
        .route("/api/api-keys/{id}", delete(api_keys_delete))
        .route("/api/api-keys/{id}/revoke", post(api_keys_revoke))
        .route("/api/api-keys/{id}/clear-trip", post(api_keys_clear_trip))
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            SessionRoleGuard {
                auth: auth.clone(),
                min: Role::Admin,
                resource: "api_keys",
                audit,
            },
            require_session_role,
        ))
        .layer(middleware::from_fn_with_state(
            AuthGate {
                auth,
                operation: OperationKind::Normal,
            },
            require_session,
        ))
}
