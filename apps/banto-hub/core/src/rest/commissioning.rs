//! 試運転モード/ロックダウン（設計 §5.6・2026-08-30 オーナー決定）:
//! `GET /api/commissioning/status` と `POST /api/commissioning/lock-down`。
//!
//! `rest.rs` から中身を変えずに移したもの（docs/banto-hub-driver-refactor-plan.md §8.1、
//! P0-1 の手順 8）。認証・ガード・監査の補助は親の `rest.rs` にあり、`use super::{...}` で
//! 使う。外から使われるパス（`crate::rest::X`）は親の re-export で保つ。

use super::{
    actor_identity, get, json, middleware, post, require_session, require_session_role, ApiError,
    AuditEntry, AuditLogService, AuthGate, AuthState, CommissioningService, HeaderMap, Json,
    OperationKind, Role, Router, SessionRoleGuard, State,
};

// --- 試運転モード/ロックダウン (設計 §5.6・2026-08-30 オーナー決定) --------
//
// `GET /api/commissioning/status`: 現在ロックダウン済みかどうか。試運転
// モード中は管理 UI がまだログインできない（ログインという概念自体が
// バイパスされている）ため、**この読み取りだけは`require_session`
// の対象外にして常に未認証で叩けるようにする** - 実装指示「未認証でも
// 取得できる必要がある」のとおり。読み取り専用のため監査エントリは
// 記録しない（`crate::audit`のモジュール doc「read routes are never
// audited」と同じ規約）。
//
// `POST /api/commissioning/lock-down`: 試運転モード → ロックダウン済みへの
// 唯一の正方向遷移（`CommissioningService::lock_down`）。他の admin
// エンドポイントと同じ`SessionRoleGuard`（admin ちょうど）+
// `require_session`を掛ける - 試運転モード中はその
// ガード自体が素通しになるので実質誰でも叩けるが、ロックダウン済みに
// なった後は admin セッションが無いと叩けなくなる（＝ロックダウン後に
// 再度ロックダウンし直すことはできるが admin 権限が要る、という自然な
// 挙動）。
//
// 試運転モードへ戻す REST エンドポイントは意図的に存在しない
// （`crate::commissioning`のモジュール doc「REST では絶対に公開しない
// こと」参照 - 設計 §5.6「UI・REST からは解除できない」・
// `banto-hub-elev.exe`経由限定）。

#[derive(Clone)]
struct CommissioningAdminState {
    commissioning: CommissioningService,
    auth: AuthState,
    audit: AuditLogService,
}

async fn commissioning_status(
    State(state): State<CommissioningAdminState>,
) -> Json<crate::commissioning::CommissioningStatus> {
    Json(crate::commissioning::CommissioningStatus {
        locked_down: state.commissioning.is_locked_down(),
    })
}

/// `POST /api/commissioning/lock-down`。`CommissioningService::lock_down` が
/// 「フラグの保存 → 同じ関数内で試運転 grant の失効」を行う（ADR-0017 §5）。
/// 呼び出し元の actor は**ロックダウンの前に**確定する: 試運転の grant
/// セッションからの操作なら、ロックダウンの中で自分のトークンも失効するので、
/// 後から `actor_identity` で引くと `None` になる。監査の detail には失効させた
/// 件数を `revokedGrants` として残す（発行は監査しないので、閉じた側に件数を残す）。
async fn commissioning_lock_down(
    State(state): State<CommissioningAdminState>,
    headers: HeaderMap,
) -> Result<Json<crate::commissioning::CommissioningStatus>, ApiError> {
    let identity = actor_identity(&headers, &state.auth);
    let revoked = state.commissioning.lock_down().await?;
    state
        .audit
        .record(AuditEntry {
            actor_username: identity.as_ref().map(|i| i.id.as_str()),
            actor_role: identity.as_ref().map(|i| i.role.as_str()),
            action: "lock_down",
            resource: "commissioning",
            entity_id: Some("1"),
            detail: Some(json!({ "revokedGrants": revoked })),
            origin: "rest",
            result: "ok",
        })
        .await;
    Ok(Json(crate::commissioning::CommissioningStatus {
        locked_down: true,
    }))
}

pub(super) fn commissioning_router(
    commissioning: CommissioningService,
    audit: AuditLogService,
    auth: AuthState,
) -> Router {
    let state = CommissioningAdminState {
        commissioning: commissioning.clone(),
        auth: auth.clone(),
        audit: audit.clone(),
    };

    // status は未認証で読める必要がある（設計 §5.6）ので、他の admin
    // ルーターと違い `require_session`/`SessionRoleGuard` を一切
    // 掛けない - `require_banto_client_header`（CSRF、`admin`ルーター全体に
    // 掛かる）だけは他の admin エンドポイントと同様に適用される
    // （`X-Banto-Client`ヘッダはログイン資格情報ではなく「自前のフロント
    // エンドから来た」ことを示すだけなので、未ログインの `GET
    // /api/auth/status`等と同じく問題ない - このファイル冒頭の
    // 「二系統に分かれたルーター」節参照）。
    let status_route = Router::new()
        .route("/api/commissioning/status", get(commissioning_status))
        .with_state(state.clone());

    let lock_down_route = Router::new()
        .route(
            "/api/commissioning/lock-down",
            post(commissioning_lock_down),
        )
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            SessionRoleGuard {
                auth: auth.clone(),
                min: Role::Admin,
                resource: "commissioning",
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
        ));

    status_route.merge(lock_down_route)
}
