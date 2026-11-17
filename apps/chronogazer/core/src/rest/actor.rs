//! `/api/*` の主体（[`Actor`]）と、API キーの受け付け（#1161 PR4、
//! docs/chronogazer-agent-api-design.md §4）。
//!
//! ## 道の分け方（§4.1）
//!
//! [`api_key_gate`] を `api_router` のいちばん外に置き、`Authorization: Bearer`
//! の値で道を分ける。
//!
//! 1. `cg_…` → API キーとして照合する（共有 crate `banto-api-keys`）。通れば
//!    [`Actor::ApiKey`] を request の extensions に載せる。通らなければ共有 crate
//!    の応答（失効・期限切れ・無い = 401、照合できない = 500）で止め、**セッション
//!    には落とさない**。
//! 2. ほかのアプリのキーの形（`bh_…` など、小文字の接頭辞と `_`）→ 401 で止める
//!    （照合も記録もしない）。banto のセッションのトークンは UUID で `_` を含まない
//!    ので、セッションと取り違えることはない。
//! 3. それ以外 → 何もしない（今のとおり、各ルーターの `require_auth` がセッション
//!    を確かめる）。
//!
//! CSRF の確認（[`require_csrf_unless_api_key`]）は、キーで通った要求には
//! `X-Banto-Client` を求めない。CSRF はブラウザが資格情報を勝手に付ける場合の脅威
//! で、キーはブラウザが勝手に付けないため（HUB の `/api/v1/*` と同じ考え）。
//! セッションの要求は今のとおり要る。
//!
//! ## キーを通すルートの集まり（既定は通さない）
//!
//! `api_router` はルーターを 2 つの集まりに分けて組む。
//!
//! - **キーを通す集まり**（収集・接続先・収集グループ・記録項目・表示グループ・
//!   しきい値・ドライバ）: 子ルーターの認証は [`require_session_or_api_key`]
//!   （キーの主体はそのまま通し、それ以外は banto の `require_auth`）。床は
//!   [`require_actor_at_least`]（セッションは banto の `require_role_at_least`
//!   と同じ、キーはスコープ）と、ハンドラの中の `require_editor`（[`Actor`] を
//!   受ける）。
//! - **キーを通さない集まり**（ユーザー管理・サーバー設定・HUB 連携・
//!   バックアップ・監査ログ・API キー・認証・UI 設定・SSE）: 集まり全体に
//!   [`reject_api_key`] を掛け、キーの主体は 403（`denied` を記録）。
//!
//! どちらにも入れずに足したルーターは、banto の `require_auth` がキーを
//! セッションとして照合して 401 で断る（閉じる側に倒れる）。キーを通すには、
//! 子ルーターの認証を [`require_session_or_api_key`] にして、キーを通す集まりに
//! 明示して入れる必要がある。
//!
//! ## 監査（§4.2）
//!
//! banto の `record_write` は主体をセッションの bearer から引き、`origin` も
//! `rest` に固定なので、キーの主体では使えない。キーを通すルートは
//! [`Actor::record`] で残す。セッションは `record_write` と同じ値
//! （`actor_username` = アカウントの id、`actor_role` = ロール）、キーは
//! `actor_username` = `名前（prefix）`、`actor_role` = `"api_key"`、`origin` =
//! 呼び出し元（REST は `"rest"`、MCP は `"mcp"`。`crate::ops::Via`、#1161 PR5a）。スコープが足りないときは `denied` を記録する。存在しないキー・形式の
//! 誤りは記録しない（誰か分からないため）。失効・期限切れのキーは
//! `resource: "api_keys"` の `denied` で記録する（HUB の `/api/v1/*` と同じ形）。

use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use banto_core::ErrorBody;
use banto_server::routes::{bearer_token, require_role_at_least, RoleGuard};
use banto_server::{require_auth, require_banto_client_header, AuthState, AuthenticatedSession};
use serde_json::json;

use crate::api_keys::{
    api_key_rejection_response, api_key_verdict, unauthorized_response, ApiKeyRejection,
    ApiKeysService, KeyScopes, KEY_PREFIX,
};
use crate::audit::{AuditEntry, AuditLogService};
use crate::collect::COLLECT_OPERATION_ROLE;
use crate::ops::Via;
use crate::users::Role;

/// キーの主体の監査の `actor_role`（§4.2）。
pub(crate) const API_KEY_ACTOR_ROLE: &str = "api_key";

/// 失効・期限切れのキーの拒否と、キーを通さないルートでの拒否を記録する
/// `resource`（HUB の `/api/v1/*` と同じ）。
const API_KEYS_RESOURCE: &str = "api_keys";

/// `/api/*` の要求の主体（§4.2）。キーを通す集まりのハンドラは引数で受ける
/// （`FromRequestParts`）。
#[derive(Debug, Clone)]
pub(crate) enum Actor {
    /// 今のログイン（閲覧公開・この PC からの接続の grant を含む）。値は banto の
    /// `require_auth` が確かめた `AuthenticatedSession` の物。
    Session { username: String, role: String },
    /// API キー。`label` は監査に載せる `名前（prefix）`。
    ApiKey {
        // 表 `api_keys` の id。REST では使わず、MCP（#1161 PR5）が使う予定。
        #[allow(dead_code)]
        id: i64,
        label: String,
        scopes: KeyScopes,
    },
}

impl Actor {
    fn session_at_least(role: &str, min: Role) -> bool {
        Role::from_str(role)
            .map(|role| role.at_least(min))
            .unwrap_or(false)
    }

    /// 設定と状態を読めるか（viewer 以上 / `read`・`config`）。
    pub(crate) fn can_read(&self) -> bool {
        match self {
            Self::Session { role, .. } => Self::session_at_least(role, Role::Viewer),
            Self::ApiKey { scopes, .. } => scopes.can_read(),
        }
    }

    /// 設定を変えられるか（editor 以上 / `config`）。
    pub(crate) fn can_configure(&self) -> bool {
        match self {
            Self::Session { role, .. } => Self::session_at_least(role, Role::Editor),
            Self::ApiKey { scopes, .. } => scopes.can_configure(),
        }
    }

    /// 収集を操作できるか（[`COLLECT_OPERATION_ROLE`] 以上 / `config`）。
    pub(crate) fn can_operate_collect(&self) -> bool {
        match self {
            Self::Session { role, .. } => Self::session_at_least(role, COLLECT_OPERATION_ROLE),
            Self::ApiKey { scopes, .. } => scopes.can_configure(),
        }
    }

    /// 監査に載せる `(actor_username, actor_role)`。
    fn audit_actor(&self) -> (&str, &str) {
        match self {
            Self::Session { username, role } => (username, role),
            Self::ApiKey { label, .. } => (label, API_KEY_ACTOR_ROLE),
        }
    }

    /// 監査を 1 本残す。成功した書き込みは `result: "ok"`（banto の
    /// `record_write` と同じ値）。`origin` は呼び出し元が渡す（REST は
    /// `"rest"`、MCP は `"mcp"` - [`crate::ops::Via::origin`]。#1161 PR5a）。
    pub(crate) async fn record(
        &self,
        audit: &AuditLogService,
        origin: &str,
        action: &str,
        resource: &str,
        entity_id: Option<&str>,
        detail: Option<serde_json::Value>,
    ) {
        let (username, role) = self.audit_actor();
        audit
            .record(AuditEntry {
                actor_username: Some(username),
                actor_role: Some(role),
                action,
                resource,
                entity_id,
                detail,
                origin,
                result: "ok",
            })
            .await;
    }

    /// 権限不足の `denied` を残す。`origin` と `detail` は [`Via`] から作る:
    /// REST は banto の `require_role_at_least` と同じ形（`origin: "rest"`、
    /// `detail` は `{method, path}`）、MCP は `origin: "mcp"`、`detail` は
    /// `{tool}`（[`Via::denied_detail`]、#1161 PR5a）。
    pub(crate) async fn record_denied(
        &self,
        audit: &AuditLogService,
        via: &Via<'_>,
        resource: &str,
    ) {
        let (username, role) = self.audit_actor();
        audit
            .record(AuditEntry {
                actor_username: Some(username),
                actor_role: Some(role),
                action: "denied",
                resource,
                entity_id: None,
                detail: Some(via.denied_detail()),
                origin: via.origin(),
                result: "denied",
            })
            .await;
    }
}

/// キーで通った要求は [`api_key_gate`] が載せた [`Actor::ApiKey`]、セッションは
/// banto の `require_auth` が載せた `AuthenticatedSession` から作る。どちらも
/// 無いのは認証の層を通っていないルーターの設定の誤りなので 401（開く側に
/// 倒さない）。
impl<S: Send + Sync> FromRequestParts<S> for Actor {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        if let Some(actor) = parts.extensions.get::<Actor>() {
            return Ok(actor.clone());
        }
        if let Some(session) = parts.extensions.get::<AuthenticatedSession>() {
            return Ok(Actor::Session {
                username: session.identity.id.clone(),
                role: session.identity.role.clone(),
            });
        }
        Err(unauthorized_response())
    }
}

fn request_is_api_key(req: &Request) -> bool {
    matches!(req.extensions().get::<Actor>(), Some(Actor::ApiKey { .. }))
}

fn forbidden_response() -> Response {
    (StatusCode::FORBIDDEN, Json(ErrorBody::Forbidden)).into_response()
}

/// `Bearer` の値がどの道か（モジュール doc「道の分け方」）。
#[derive(Debug, PartialEq, Eq)]
enum BearerKind {
    /// `cg_…`: このアプリのキー。
    OwnKey,
    /// ほかのアプリのキーの形（`bh_…` など）。
    ForeignKey,
    /// それ以外（セッションのトークン）。
    Other,
}

fn classify_bearer(token: &str) -> BearerKind {
    if token
        .strip_prefix(KEY_PREFIX)
        .is_some_and(|rest| rest.starts_with('_'))
    {
        return BearerKind::OwnKey;
    }
    match token.split_once('_') {
        Some((head, _))
            if !head.is_empty()
                && head.len() <= 8
                && head.bytes().all(|b| b.is_ascii_lowercase()) =>
        {
            BearerKind::ForeignKey
        }
        _ => BearerKind::Other,
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// [`api_key_gate`] の state。
#[derive(Clone)]
pub(crate) struct ApiKeyGate {
    pub(crate) api_keys: ApiKeysService,
    pub(crate) audit: AuditLogService,
}

/// `api_router` のいちばん外の層（モジュール doc「道の分け方」）。
pub(crate) async fn api_key_gate(
    State(gate): State<ApiKeyGate>,
    mut req: Request,
    next: Next,
) -> Response {
    // 外から extensions に主体が入ってくることは無いが、念のため消してから決める
    // （この層だけがキーの主体を載せる）。
    req.extensions_mut().remove::<Actor>();
    let Some(token) = bearer_token(req.headers()).map(str::to_owned) else {
        return next.run(req).await;
    };
    match classify_bearer(&token) {
        BearerKind::Other => next.run(req).await,
        BearerKind::ForeignKey => unauthorized_response(),
        BearerKind::OwnKey => {
            let now_ms = now_ms();
            match api_key_verdict(gate.api_keys.check(&token, now_ms).await) {
                Ok(ctx) => {
                    if let Err(err) = gate
                        .api_keys
                        .touch_last_used(ctx.id, now_ms, ctx.last_used_at_ms)
                        .await
                    {
                        eprintln!("chronogazer: API キーの最終使用の更新に失敗しました: {err}");
                    }
                    req.extensions_mut().insert(Actor::ApiKey {
                        id: ctx.id,
                        label: ctx.actor_label(),
                        scopes: KeyScopes::from_strings(&ctx.scopes),
                    });
                    next.run(req).await
                }
                Err((rejection, denied)) => {
                    if let Some(denied) = denied {
                        let method = req.method().as_str().to_string();
                        let path = req.uri().path().to_string();
                        gate.audit
                            .record(AuditEntry {
                                actor_username: None,
                                actor_role: None,
                                action: "denied",
                                resource: API_KEYS_RESOURCE,
                                entity_id: Some(&denied.id.to_string()),
                                detail: Some(json!({
                                    "reason": denied.reason,
                                    "name": denied.name,
                                    "method": method,
                                    "path": path,
                                })),
                                origin: "rest",
                                result: "denied",
                            })
                            .await;
                    }
                    if let ApiKeyRejection::Unavailable(err) = &rejection {
                        eprintln!("chronogazer: API キーを照合できませんでした: {err}");
                    }
                    api_key_rejection_response(rejection)
                }
            }
        }
    }
}

/// CSRF の確認（banto の `require_banto_client_header`）。キーで通った要求には
/// 求めない（モジュール doc）。
pub(crate) async fn require_csrf_unless_api_key(req: Request, next: Next) -> Response {
    if request_is_api_key(&req) {
        return next.run(req).await;
    }
    require_banto_client_header(req, next).await
}

/// キーを通さない集まりに掛ける層: キーの主体は 403（`denied` を記録）。
/// セッションの要求は何もせずに通す（認証は各ルーターの `require_auth`）。
pub(crate) async fn reject_api_key(
    State(audit): State<AuditLogService>,
    req: Request,
    next: Next,
) -> Response {
    if let Some(actor @ Actor::ApiKey { .. }) = req.extensions().get::<Actor>() {
        let actor = actor.clone();
        let method = req.method().as_str().to_string();
        let path = req.uri().path().to_string();
        actor
            .record_denied(
                &audit,
                &Via::Rest {
                    method: &method,
                    path: &path,
                },
                API_KEYS_RESOURCE,
            )
            .await;
        return forbidden_response();
    }
    next.run(req).await
}

/// キーを通す集まりの子ルーターの認証: キーの主体はそのまま通し、それ以外は
/// banto の `require_auth`（今と同じ）。
pub(crate) async fn require_session_or_api_key(
    State(auth): State<AuthState>,
    req: Request,
    next: Next,
) -> Response {
    if request_is_api_key(&req) {
        return next.run(req).await;
    }
    require_auth(State(auth), req, next).await
}

/// [`require_actor_at_least`] がキーに求めること。
#[derive(Clone, Copy, Debug)]
pub(crate) enum KeyNeed {
    /// `read` か `config`。
    Read,
    /// 収集の操作（`config`。[`Actor::can_operate_collect`]）。
    OperateCollect,
}

/// [`require_actor_at_least`] の state。セッションは `RoleGuard` の値で banto の
/// `require_role_at_least` に任せ、キーは `key_need` で判定する。
#[derive(Clone)]
pub(crate) struct ActorGuard {
    pub(crate) role: RoleGuard,
    pub(crate) key_need: KeyNeed,
}

/// ルーターの床（`RoleGuard` の置き換え）。セッションの判定と監査は banto の
/// `require_role_at_least` のまま。キーはスコープが足りなければ `denied` を
/// 記録して 403。
pub(crate) async fn require_actor_at_least(
    State(guard): State<ActorGuard>,
    req: Request,
    next: Next,
) -> Response {
    if let Some(actor @ Actor::ApiKey { .. }) = req.extensions().get::<Actor>() {
        let allowed = match guard.key_need {
            KeyNeed::Read => actor.can_read(),
            KeyNeed::OperateCollect => actor.can_operate_collect(),
        };
        if allowed {
            return next.run(req).await;
        }
        let actor = actor.clone();
        let method = req.method().as_str().to_string();
        let path = req.uri().path().to_string();
        actor
            .record_denied(
                &guard.role.audit,
                &Via::Rest {
                    method: &method,
                    path: &path,
                },
                guard.role.resource,
            )
            .await;
        return forbidden_response();
    }
    require_role_at_least(State(guard.role), req, next).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_values_are_split_into_own_key_foreign_key_and_session() {
        assert_eq!(classify_bearer("cg_abcdefgh_secret"), BearerKind::OwnKey);
        assert_eq!(classify_bearer("cg_"), BearerKind::OwnKey);
        assert_eq!(
            classify_bearer("bh_abcdefgh_secret"),
            BearerKind::ForeignKey
        );
        assert_eq!(classify_bearer("xyz_whatever"), BearerKind::ForeignKey);
        // banto のセッションのトークン（UUID）。
        assert_eq!(
            classify_bearer("6f1c2a8e-1d2b-4c3d-9e8f-0a1b2c3d4e5f"),
            BearerKind::Other
        );
        assert_eq!(classify_bearer("cgx"), BearerKind::Other);
        assert_eq!(classify_bearer("CG_abc"), BearerKind::Other);
    }

    #[test]
    fn session_actor_follows_the_role_floors() {
        let viewer = Actor::Session {
            username: "v".into(),
            role: "viewer".into(),
        };
        assert!(viewer.can_read());
        assert!(!viewer.can_configure());
        assert!(!viewer.can_operate_collect());
        let editor = Actor::Session {
            username: "e".into(),
            role: "editor".into(),
        };
        assert!(editor.can_configure());
        assert!(editor.can_operate_collect());
        let unknown = Actor::Session {
            username: "x".into(),
            role: "superuser".into(),
        };
        assert!(!unknown.can_read());
    }

    #[test]
    fn key_actor_follows_the_scopes() {
        let read = Actor::ApiKey {
            id: 1,
            label: "agent（abcdefgh）".into(),
            scopes: KeyScopes::from_strings(&["read".to_string()]),
        };
        assert!(read.can_read());
        assert!(!read.can_configure());
        assert!(!read.can_operate_collect());
        let config = Actor::ApiKey {
            id: 2,
            label: "agent2（abcdefgh）".into(),
            scopes: KeyScopes::from_strings(&["config".to_string()]),
        };
        assert!(config.can_read());
        assert!(config.can_configure());
        assert!(config.can_operate_collect());
        assert_eq!(config.audit_actor().1, "api_key");
    }
}
