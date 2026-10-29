//! 送信口の認証の層 [`require_ingest_auth`]（R2a-2c、docs/banto-hub-ingest-design.md
//! §5・§6）。REST の `POST /api/v1/ingest/{id}/values`（[`super::rest`]）と R2a-2d の
//! WebSocket `GET /api/v1/ingest/{id}/stream` が、同じこの層を
//! `Router::route_layer` で被せる（経路は [`IngestAuthState::transport`] で区別し、
//! 監査の `transport` にだけ使う）。
//!
//! `/api/v1/*` の `require_tag_space_auth`（`read` を要求する層）の下には置かない:
//! `ingest:#{id}` は `read` / `write` / `admin` と直交で、`ingest:` だけのキーが
//! 送れなければならず、`read` のキーは送れてはならない（§5「判定」）。
//!
//! ## 判断の順序（§6 の表。この順序そのものが安全の要点）
//!
//! | # | 確かめること | 断るとき | 監査 |
//! | --- | --- | --- | --- |
//! | 1 | `Authorization: Bearer` がある | 401 | しない |
//! | 2 | `bh_` でない（セッション）→ 照合して有効なら | 403 `session_token_cannot_ingest`（無効は 401・照合不能は 500） | しない |
//! | 3 | キーの照合（`ApiKeysService::check` → `api_key_verdict`） | 失効・期限切れ・存在しない 401、トリップ 403 `key_tripped`、照合不能 500 | 失効・期限切れ・トリップだけ（`resource: "api_keys"`） |
//! | 4 | パスの `{id}` が正規形の 10 進（`[1-9][0-9]*`・`i64`） | 404 `unknown_connection` | しない |
//! | 5 | キーが `ingest:#{id}` を持つ（`ApiKeyContext::can_ingest`） | 403 `missing_ingest_scope` | する |
//! | 6 | その id の `push` 接続がある（`IngestHub`） | 404 `unknown_connection` | する |
//! | 7 | 接続が有効 | 409 `connection_disabled` | しない |
//!
//! - **本文はこの層では読まない**。本文の読み込み（と 1 MiB の上限、413）は層の後ろの
//!   ハンドラの抽出で起きるので、認証・権限・接続の確認を通らない要求の本文は
//!   読まれない（§11 のオーナーの監査 (2)）。WS（R2a-2d）も upgrade はこの層の後ろ。
//! - **接続の有無は権限の確認（5）より前に明かさない**。4 は文字列の形だけを見て
//!   id の存在に触れず、6 の照会は 5 を通った後にだけ行う（[`gate_connection`] の
//!   `lookup` は 5 を通るまで呼ばれない - 表のテストで確かめる）。スコープの無い
//!   キーには、存在する id でも削除済みの id でも同じ 403 が返る。
//! - 接続は id で引くので、改名しても同じキーと URL で送れる。削除した接続の id は
//!   再利用されない（§5.1）ので、削除して同名で作り直した接続には古いキーで送れない
//!   （古いキーのスコープの id は新しい接続の id と一致しない）。
//! - `Sec-WebSocket-Protocol` からのトークンの取り出し（`crate::rest` の
//!   `extract_ws_protocol_token`）は**使わない**（ブラウザの送り手は対象外、§6）。
//! - 接続の照会は `IngestHub`（catalog の commit と同じ点で更新される受信側の表）を
//!   読む。DB は読まない（送り手ごとの要求で DB に負荷を掛けない）。
//!
//! - 監査の `denied`（表の 3・5・6）はキーごとに 1 時間に 100 行までに間引く
//!   （2026-10-21 オーナー決定、`super::audit::KeyDenialThrottle`）。キーを照合できた
//!   要求は、断ったものも通ったものも [`IngestHub::throttle_key_denial`] で観測する
//!   （窓が明けた後の最初の観測で抑えた数を 1 行。悪い要求が止み、同じキーが正しく
//!   送るようになっても数は残る）。WS（R2a-2d）もこの層を通るので同じに掛かる。
//!
//! 通った要求には [`IngestCaller`]（キー・接続 id・その時点の名前・接続元・経路）を
//! extension に載せる。ハンドラはこれで監査の行を作る。

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, RawPathParams, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use banto_server::AuthState;
use banto_tstore::Clock;
use serde_json::{json, Value};

use crate::api_keys::{
    api_key_verdict, parse_canonical_connection_id, ApiKeyContext, ApiKeysService,
};
use crate::audit::AuditLogService;
use crate::rest::{api_key_rejection_response, bearer_token, unauthorized_response};

use super::audit::{self as ingest_audit, IngestAuditContext, IngestAuditRow, Transport};
use super::IngestHub;

/// パスの接続 id のパラメータ名（`/api/v1/ingest/{id}/…`）。
pub const PATH_PARAM: &str = "id";

/// [`require_ingest_auth`] の state。
#[derive(Clone)]
pub struct IngestAuthState {
    pub auth: AuthState,
    pub api_keys: ApiKeysService,
    pub audit: AuditLogService,
    pub ingest: Arc<IngestHub>,
    /// キーの期限切れの判定と `last_used_at` の「今」（収集と同じ時計）。
    pub clock: Arc<dyn Clock>,
    /// この層を被せたルートの経路（監査の `transport`）。
    pub transport: Transport,
}

/// 認証を通った送り手（ハンドラが extension で受ける）。
#[derive(Debug, Clone)]
pub struct IngestCaller {
    pub key: ApiKeyContext,
    pub connection_id: i64,
    /// 層を通った時点の接続名（監査の控え・表示用。束縛は id）。
    pub connection_name: String,
    pub peer: Option<String>,
    pub transport: Transport,
}

impl IngestCaller {
    /// 監査の共通の項目。
    pub fn audit_context(&self) -> IngestAuditContext {
        IngestAuditContext {
            api_key_id: self.key.id,
            api_key_name: self.key.name.clone(),
            connection_id: self.connection_id,
            connection_name: Some(self.connection_name.clone()),
            peer: self.peer.clone(),
            transport: self.transport,
        }
    }
}

/// 接続の確認（表の 4〜7）で断る理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestDenial {
    /// 4: パスの `{id}` が正規形でない（id の存在には触れない）。
    PathShape,
    /// 5: キーがその id の `ingest:#{id}` を持たない。
    MissingScope { connection_id: i64 },
    /// 6: その id の `push` 接続が無い（削除済み・`push` でない）。
    UnknownConnection { connection_id: i64 },
    /// 7: 接続が無効。
    ConnectionDisabled { connection_id: i64 },
}

impl IngestDenial {
    pub fn status(self) -> StatusCode {
        match self {
            Self::PathShape | Self::UnknownConnection { .. } => StatusCode::NOT_FOUND,
            Self::MissingScope { .. } => StatusCode::FORBIDDEN,
            Self::ConnectionDisabled { .. } => StatusCode::CONFLICT,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Self::PathShape | Self::UnknownConnection { .. } => "unknown_connection",
            Self::MissingScope { .. } => "missing_ingest_scope",
            Self::ConnectionDisabled { .. } => "connection_disabled",
        }
    }

    /// 監査するときの接続 id（§6 の表の「監査」の列）。
    pub fn audited_connection(self) -> Option<i64> {
        match self {
            Self::MissingScope { connection_id } | Self::UnknownConnection { connection_id } => {
                Some(connection_id)
            }
            Self::PathShape | Self::ConnectionDisabled { .. } => None,
        }
    }
}

/// 接続の確認（表の 4〜7、純関数）。`lookup(id)` は id の `push` 接続の
/// `(名前, 有効)`。**5 を通るまで `lookup` を呼ばない**（接続の有無を権限の確認より
/// 前に明かさない）。
pub fn gate_connection(
    raw_id: &str,
    key: &ApiKeyContext,
    lookup: impl FnOnce(i64) -> Option<(String, bool)>,
) -> Result<(i64, String), IngestDenial> {
    let connection_id = parse_canonical_connection_id(raw_id).ok_or(IngestDenial::PathShape)?;
    if !key.can_ingest(connection_id) {
        return Err(IngestDenial::MissingScope { connection_id });
    }
    match lookup(connection_id) {
        None => Err(IngestDenial::UnknownConnection { connection_id }),
        Some((_, false)) => Err(IngestDenial::ConnectionDisabled { connection_id }),
        Some((name, true)) => Ok((connection_id, name)),
    }
}

/// 送信口の拒否の本文（`{"error": code, ..extra}`）。
pub fn error_response(status: StatusCode, code: &str, extra: Value) -> Response {
    let mut body = json!({ "error": code });
    if let (Value::Object(body), Value::Object(extra)) = (&mut body, extra) {
        body.extend(extra);
    }
    (status, Json(body)).into_response()
}

fn session_token_cannot_ingest_response() -> Response {
    error_response(
        StatusCode::FORBIDDEN,
        "session_token_cannot_ingest",
        json!({
            "message": "取り込みは ingest:#{接続 id} スコープを持つ API キーでのみ可能です。管理 UI のセッション token では送れません。"
        }),
    )
}

/// 接続元（`ConnectInfo` が無い起動経路・テストでは `None`）。
pub fn peer_of(req: &axum::extract::Request) -> Option<String> {
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.to_string())
}

/// 送信口の認証の層（モジュールの doc の表の順序）。
pub async fn require_ingest_auth(
    State(state): State<IngestAuthState>,
    params: RawPathParams,
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    // 1: トークン。`Sec-WebSocket-Protocol` からは取り出さない（§6）。
    let Some(token) = bearer_token(req.headers()).map(str::to_string) else {
        return unauthorized_response();
    };
    let peer = peer_of(&req);

    // 2: セッション。書き込みと同じく「機械のキーだけ」（§5「セッション」）。
    if !token.starts_with("bh_") {
        return match state.auth.authenticate(&token).await {
            Ok(Some(_)) => session_token_cannot_ingest_response(),
            Ok(None) => unauthorized_response(),
            Err(err) => banto_server::ApiError(err).into_response(),
        };
    }

    // 3: キーの照合（`/api/v1/*`・MCP・サイドカーと同じ `api_key_verdict`）。
    let now_ms = state.clock.now_ms();
    let key = match api_key_verdict(state.api_keys.check(&token, now_ms).await) {
        Ok(key) => key,
        Err((rejection, denied)) => {
            if let Some(denied) = denied {
                let row = IngestAuditRow::key_denied(
                    denied.id,
                    &denied.name,
                    denied.reason,
                    req.uri().path(),
                    peer.as_deref(),
                    state.transport,
                );
                let rows =
                    state
                        .ingest
                        .throttle_key_denial(denied.id, &denied.name, Some(row), now_ms);
                ingest_audit::record(&state.audit, rows).await;
            }
            return api_key_rejection_response(rejection);
        }
    };

    // 4〜7: 接続の確認。
    let raw_id = params
        .iter()
        .find(|(name, _)| *name == PATH_PARAM)
        .map(|(_, value)| value)
        .unwrap_or_default();
    let ingest = state.ingest.clone();
    let gate = gate_connection(raw_id, &key, |id| {
        ingest
            .connection_info(id)
            .map(|info| (info.name, info.enabled))
    });
    let (connection_id, connection_name) = match gate {
        Ok(found) => found,
        Err(denial) => {
            let row = denial.audited_connection().map(|connection_id| {
                let ctx = IngestAuditContext {
                    api_key_id: key.id,
                    api_key_name: key.name.clone(),
                    connection_id,
                    // 名前の控えは無い（スコープの無いキーには明かさない・削除済み）。
                    connection_name: None,
                    peer: peer.clone(),
                    transport: state.transport,
                };
                IngestAuditRow::denied(&ctx, denial.code())
            });
            // 記録しない拒否（パスの形・無効）もキーの観測（抑えた数を出す点）にする。
            let rows = state
                .ingest
                .throttle_key_denial(key.id, &key.name, row, now_ms);
            ingest_audit::record(&state.audit, rows).await;
            return error_response(denial.status(), denial.code(), json!({}));
        }
    };
    // 通った要求もキーの観測（窓が明けていれば抑えた `denied` の数を 1 行）。
    let rows = state
        .ingest
        .throttle_key_denial(key.id, &key.name, None, now_ms);
    ingest_audit::record(&state.audit, rows).await;

    if let Err(err) = state
        .api_keys
        .touch_last_used(key.id, now_ms, key.last_used_at_ms)
        .await
    {
        eprintln!("banto-hub: API キーの last_used_at 更新に失敗しました: {err}");
    }
    req.extensions_mut().insert(IngestCaller {
        key,
        connection_id,
        connection_name,
        peer,
        transport: state.transport,
    });
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(scopes: &[&str]) -> ApiKeyContext {
        ApiKeyContext {
            id: 1,
            name: "k".into(),
            prefix: "kkkkkkkk".to_string(),
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
            last_used_at_ms: None,
        }
    }

    /// §6 の表の 4〜7。スコープの無いキーには、接続があってもなくても同じ 403
    /// （`lookup` は呼ばれない）。`read` / `admin` は送れない（直交）。
    #[test]
    fn the_connection_gate_table() {
        let present = |id: i64| match id {
            12 => Some(("Bridge1".to_string(), true)),
            13 => Some(("Off".to_string(), false)),
            _ => None,
        };
        let must_not_look = |_: i64| -> Option<(String, bool)> {
            panic!("スコープを確かめる前に接続を照会した")
        };
        let ingest_12 = key(&["ingest:#12", "ingest:#13", "ingest:#99"]);
        type Row<'a> = (
            &'a str,
            &'a ApiKeyContext,
            Result<(i64, String), IngestDenial>,
        );
        let rows: Vec<Row> = vec![
            ("12", &ingest_12, Ok((12, "Bridge1".into()))),
            (
                "13",
                &ingest_12,
                Err(IngestDenial::ConnectionDisabled { connection_id: 13 }),
            ),
            (
                "99",
                &ingest_12,
                Err(IngestDenial::UnknownConnection { connection_id: 99 }),
            ),
        ];
        for (raw, key, expected) in rows {
            assert_eq!(gate_connection(raw, key, present), expected, "{raw}");
        }
        // パスの形（4）: スコープも接続も見ない。
        for raw in [
            "",
            "0",
            "012",
            "-1",
            "+1",
            " 12",
            "12x",
            "Bridge1",
            "1.0",
            "99999999999999999999",
        ] {
            assert_eq!(
                gate_connection(raw, &ingest_12, must_not_look),
                Err(IngestDenial::PathShape),
                "{raw:?}"
            );
        }
        // スコープ（5）: 接続の有無に関わらず同じ拒否で、照会しない。
        let read_admin = key(&["read", "admin", "write:a.b.c", "ingest:#1"]);
        for raw in ["12", "13", "99"] {
            let id: i64 = raw.parse().unwrap();
            assert_eq!(
                gate_connection(raw, &read_admin, must_not_look),
                Err(IngestDenial::MissingScope { connection_id: id }),
                "{raw}"
            );
        }
        // 応答と監査の列。
        let table = [
            (IngestDenial::PathShape, 404, "unknown_connection", None),
            (
                IngestDenial::MissingScope { connection_id: 5 },
                403,
                "missing_ingest_scope",
                Some(5),
            ),
            (
                IngestDenial::UnknownConnection { connection_id: 5 },
                404,
                "unknown_connection",
                Some(5),
            ),
            (
                IngestDenial::ConnectionDisabled { connection_id: 5 },
                409,
                "connection_disabled",
                None,
            ),
        ];
        for (denial, status, code, audited) in table {
            assert_eq!(denial.status().as_u16(), status, "{denial:?}");
            assert_eq!(denial.code(), code, "{denial:?}");
            assert_eq!(denial.audited_connection(), audited, "{denial:?}");
        }
    }
}
