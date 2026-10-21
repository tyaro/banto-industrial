//! 外部 DB 連携（Sink）の REST API: sink group の CRUD（`/api/sink/groups`）と、
//! サイドカー専用の admin 面（`GET /api/sink/config`・`PUT /api/sink/status`）。
//!
//! `rest.rs` から中身を変えずに移したもの（docs/banto-hub-driver-refactor-plan.md §8.1、
//! P0-1 の手順 6）。I1 CRUD の共通層は `rest/crud.rs` にあり、認証・ガード・監査の補助は
//! 親の `rest.rs` にある。どちらも `use super::{...}` で使う。外から使われるパス
//! （`crate::rest::X`）は親の re-export で保つ。

use super::{
    actor_identity, api_key_rejection_response, api_key_verdict, bearer_token, broadcast,
    forbidden_response, get, json, middleware, put, record_write, require_editor, require_session,
    unauthorized_response, ApiError, ApiKeysService, Arc, AuditLogService, AuthGate, AuthState,
    BantoError, CollectorManager, Deserialize, FieldError, FromStr, HeaderMap, IntoResponse, Json,
    OperationKind, Path, PlcConnectionService, Response, Role, Router, Serialize, ServerEvent,
    SinkGroup, SinkGroupInput, SinkGroupService, SinkGroupStatusPush, SinkStatusStore, State,
    StatusCode, TagEntry,
};

// --- 外部 DB 連携 S4（docs/banto-hub-external-db-design.md §5.2・§6-13）:
// `hub_sink_groups` の CRUD ---------------------------------------------------
//
// `plc_connections`/`collection_groups`/`tags`（[`tag_registry_router`]）と
// 同じ認可（`require_editor`書き込み・viewer 読み取り）だが、**pending queue
// には載らない**（§6-13「収集に影響しないため即時適用」・`crate::sink`の
// モジュール doc comment参照）: sink group は PLC 収集パイプラインに一切
// 関わらないので、`queue_pending_registry_change`分岐を持たない。カタログの
// 再構築（`commit_catalog_and_notify`）も呼ばない - 変更の伝搬は SSE
// `ResourceChanged { resource: "sink_groups" }` の送出のみで足りる
// （サイドカーはこれを検知して`GET /api/sink/config`を取り直す）。

#[derive(Clone)]
struct SinkGroupsState {
    sink_groups: SinkGroupService,
    auth: AuthState,
    audit: AuditLogService,
    events: broadcast::Sender<ServerEvent>,
}

async fn sink_groups_list(
    State(state): State<SinkGroupsState>,
) -> Result<Json<Vec<SinkGroup>>, ApiError> {
    Ok(Json(state.sink_groups.list().await?))
}

async fn sink_groups_get(
    State(state): State<SinkGroupsState>,
    Path(id): Path<i64>,
) -> Result<Json<SinkGroup>, ApiError> {
    Ok(Json(state.sink_groups.get(id).await?))
}

async fn sink_groups_create(
    State(state): State<SinkGroupsState>,
    headers: HeaderMap,
    Json(input): Json<SinkGroupInput>,
) -> Result<Response, ApiError> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "sink_groups",
        "POST",
        "/api/sink/groups",
    )
    .await?;
    let created = state.sink_groups.create(input).await?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "create",
        "sink_groups",
        Some(&created.id.to_string()),
        Some(json!({ "name": created.name, "enabled": created.enabled })),
    )
    .await;
    let _ = state.events.send(ServerEvent::ResourceChanged {
        resource: "sink_groups".to_string(),
    });
    Ok(Json(created).into_response())
}

async fn sink_groups_update(
    State(state): State<SinkGroupsState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(input): Json<SinkGroupInput>,
) -> Result<Response, ApiError> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "sink_groups",
        "PUT",
        "/api/sink/groups/{id}",
    )
    .await?;
    let updated = state.sink_groups.update(id, input).await?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "update",
        "sink_groups",
        Some(&id.to_string()),
        Some(json!({ "name": updated.name, "enabled": updated.enabled })),
    )
    .await;
    let _ = state.events.send(ServerEvent::ResourceChanged {
        resource: "sink_groups".to_string(),
    });
    Ok(Json(updated).into_response())
}

async fn sink_groups_delete(
    State(state): State<SinkGroupsState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Response, ApiError> {
    require_editor(
        &state.auth,
        &state.audit,
        &headers,
        "sink_groups",
        "DELETE",
        "/api/sink/groups/{id}",
    )
    .await?;
    state.sink_groups.delete(id).await?;
    record_write(
        &state.audit,
        &state.auth,
        &headers,
        "delete",
        "sink_groups",
        Some(&id.to_string()),
        None,
    )
    .await;
    let _ = state.events.send(ServerEvent::ResourceChanged {
        resource: "sink_groups".to_string(),
    });
    Ok(StatusCode::NO_CONTENT.into_response())
}

pub(super) fn sink_groups_router(
    sink_groups: SinkGroupService,
    audit: AuditLogService,
    auth: AuthState,
    events: broadcast::Sender<ServerEvent>,
) -> Router {
    let state = SinkGroupsState {
        sink_groups,
        auth: auth.clone(),
        audit,
        events,
    };
    Router::new()
        .route(
            "/api/sink/groups",
            get(sink_groups_list).post(sink_groups_create),
        )
        .route(
            "/api/sink/groups/{id}",
            get(sink_groups_get)
                .put(sink_groups_update)
                .delete(sink_groups_delete),
        )
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            AuthGate {
                auth,
                operation: OperationKind::Normal,
            },
            require_session,
        ))
}

// --- 外部 DB 連携 S4（design §5.2・§6-15）: サイドカー専用の admin 面
// `GET /api/sink/config`・`PUT /api/sink/status` -----------------------------
//
// admin スコープを持つ API キー（`bh_`、サイドカー専用に発行する想定）か、
// admin ロールのセッション（動作確認用）のいずれかを要求する -
// `crate::mcp::require_admin_scope`（MCP の構成ツールは admin スコープの
// API キーしか経路がない）と同じ「admin のみ」判定を、REST では
// API キー/セッションの両方に対して行う点だけが違う。`/api/v1/*`の
// `require_tag_space_auth`（read/write スコープ）とも、管理系ルーターの
// `SessionRoleGuard`（セッションのみ）とも異なる、この2エンドポイント専用の
// ゲート。

/// [`require_sink_admin`]の判定結果。`Err`はそのまま返せる`Response`
/// （401/403）を運ぶ - 呼び出し元のハンドラは`if let Err(resp) = ... { return
/// resp; }`の1行で済む。`Response`は大きい（axum の`http::Response<Body>`）
/// ため`clippy::result_large_err`が付くが、この関数は数エンドポイントだけが
/// 呼ぶ低頻度のゲートで、ボックス化するほどのホットパスではない。
///
/// `Ok` は通った呼び出し手の API キー（admin ロールのセッションなら `None`）。
/// R2a-3b の `GET /api/ingest/bridge-config`（`super::ingest_bridge`）が同じゲートを
/// 使い（docs/banto-hub-ingest-design.md §10.2・§12 Q13「新しい信頼の形を作らない」）、
/// キーの `ingest:#{id}` で source ごとに `ingestAllowed` を詰めるために使う。
#[allow(clippy::result_large_err)]
pub(super) async fn require_sink_admin(
    api_keys: &ApiKeysService,
    auth: &AuthState,
    headers: &HeaderMap,
    now_ms: i64,
) -> Result<Option<crate::api_keys::ApiKeyContext>, Response> {
    let Some(token) = bearer_token(headers) else {
        return Err(unauthorized_response());
    };
    if token.starts_with("bh_") {
        match api_key_verdict(api_keys.check(token, now_ms).await) {
            Ok(ctx) => {
                if let Err(err) = api_keys
                    .touch_last_used(ctx.id, now_ms, ctx.last_used_at_ms)
                    .await
                {
                    eprintln!(
                        "banto-hub: sink admin API キーの last_used_at 更新に失敗しました: {err}"
                    );
                }
                if ctx.has_admin_scope() {
                    Ok(Some(ctx))
                } else {
                    Err(forbidden_response())
                }
            }
            // #435: 使えないキーの応答はタグ空間・MCP と同じ変換
            // （[`api_key_rejection_response`]）。失効・期限切れ・存在しないは
            // 401、トリップは 403 `key_tripped`、照合できないは 500（#434）。
            Err((rejection, _denied)) => Err(api_key_rejection_response(rejection)),
        }
    } else {
        match actor_identity(headers, auth) {
            Some(identity)
                if Role::from_str(&identity.role)
                    .map(|role| role.at_least(Role::Admin))
                    .unwrap_or(false) =>
            {
                Ok(None)
            }
            Some(_) => Err(forbidden_response()),
            None => Err(unauthorized_response()),
        }
    }
}

#[derive(Clone)]
struct SinkAdminState {
    sink_groups: SinkGroupService,
    plc_connections: PlcConnectionService,
    manager: Arc<CollectorManager>,
    sink_status: Arc<SinkStatusStore>,
    api_keys: ApiKeysService,
    auth: AuthState,
}

/// `GET /api/sink/config`の応答（設計 §5.2）。
///
/// R2b-1d（docs/banto-hub-connection-settings-design.md §5.4、追加だけ）: パスワードを
/// 読めない接続（秘密値の行が無い・この PC で開けない）は `connections` に**載せず**、
/// `unavailable_connections` に理由つきで出す。`password: null` で返すことはしない
/// （古いサイドカーの `build_pool` は `null` を空文字にして繋ぐので、`trust` 認証の DB
/// なら繋がってしまう。載っていなければ、そのグループは `MissingConnection` で書かない）。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SinkConfigResponse {
    generated_at: i64,
    groups: Vec<SinkConfigGroupEntry>,
    connections: Vec<SinkConfigConnectionEntry>,
    unavailable_connections: Vec<SinkConfigUnavailableConnection>,
}

/// R2b-1d（設計 §5.4）: パスワードを読めないので載せなかった接続 1 件。値も参照も
/// 持たない。`reason` は `missing`（行が無い）/ `undecryptable`（この PC で開けない）。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SinkConfigUnavailableConnection {
    id: i64,
    name: String,
    reason: &'static str,
}

/// 有効な sink group 1件分。`db_connection_id`は`connections`から対応する
/// 行を引くためのキー（設計本文の例には無いが、サイドカーがどの接続へ
/// INSERT するかを決める唯一の手掛かりなのでここに含める - S4 実装指示の
/// 逸脱点、PR 本文に記載）。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SinkConfigGroupEntry {
    id: i64,
    name: String,
    db_connection_id: i64,
    mode: String,
    interval_ms: i64,
    table_name: String,
    store_bad: bool,
    tags: Vec<SinkConfigTagEntry>,
}

/// 対象タグ1件分。`connectionId`/`groupId`/`tagId`は
/// `crates/banto-tagclient/src/types.rs`の`StableTagId::new(connection_id,
/// group_id, tag_id)`と完全に同じ3つ組 - サイドカーは banto-tagclient SDK
/// でこの3つ組から購読する（設計 §5.1「Hub からの値の取得は
/// banto-tagclient SDK」）。`externalName`はログ・推奨DDL表示など人が読む
///用途のためのおまけ。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SinkConfigTagEntry {
    connection_id: i64,
    group_id: i64,
    tag_id: i64,
    external_name: String,
}

/// 接続1件分。**パスワードを平文で含む**（設計 §6-15、2026-09-06 オーナー
/// 決定「admin スコープの専用 API キー + ループバック」）- このエンドポイント
/// 自体が admin スコープ限定であることと、Hub・サイドカーが同一マシンで
/// ループバック接続する運用前提（docs/banto-hub-external-db-design.md
/// §2.2・§5.2）がその埋め合わせ。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SinkConfigConnectionEntry {
    id: i64,
    name: String,
    host: String,
    port: i64,
    database: Option<String>,
    username: Option<String>,
    password: Option<String>,
}

async fn build_sink_config_response(
    state: &SinkAdminState,
) -> Result<SinkConfigResponse, BantoError> {
    let groups = state.sink_groups.list_enabled().await?;

    let map = state.manager.tag_map();
    let by_tag_id: std::collections::HashMap<i64, &TagEntry> =
        map.iter().map(|entry| (entry.ids.2, entry)).collect();

    let mut connection_ids: Vec<i64> = groups.iter().map(|group| group.db_connection_id).collect();
    connection_ids.sort_unstable();
    connection_ids.dedup();
    let mut connections = Vec::with_capacity(connection_ids.len());
    let mut unavailable_connections = Vec::new();
    for connection_id in connection_ids {
        // R2b-1d (design §5.4): the password is opened from its sealed row right before
        // the response is built. "No password" (`None`) and "unreadable" stay apart: an
        // unreadable one is never sent as an absent/empty password (a `trust`-auth DB
        // would accept that, checklist §5「エラーを空に潰さない」).
        // PR #777 review P2: the connection and its rows come from one read transaction -
        // a password update committing between two separate reads would make a healthy
        // connection read as `missing`, drop it from `connections`, and the sidecar would
        // rebuild its groups and discard their unsent rows.
        let (conn, rows) = state
            .plc_connections
            .get_with_secret_rows(connection_id)
            .await?;
        let password = match crate::secrets::open_connection_secret(
            &crate::secrets::ConnectionSecrets::platform_default(),
            &conn,
            banto_tags::PASSWORD_KEY,
            &rows,
        ) {
            Ok(password) => password.map(|plain| plain.as_str().to_owned()),
            Err(reason) => {
                // Left out of `connections` (a sidecar - old or new - does not connect for
                // its groups: an old one reports `GroupSetupError::MissingConnection`) and
                // listed with the reason (design §5.4).
                unavailable_connections.push(SinkConfigUnavailableConnection {
                    id: conn.id,
                    name: conn.name.clone(),
                    reason: reason.as_str(),
                });
                eprintln!(
                    "banto-hub: warn: GET /api/sink/config: 接続 {}（#{}）のパスワードを読めません\
                     （{}）。この接続を設定に載せません",
                    conn.name,
                    conn.id,
                    reason.as_str()
                );
                continue;
            }
        };
        connections.push(SinkConfigConnectionEntry {
            id: conn.id,
            name: conn.name,
            host: conn.host,
            port: conn.port,
            database: conn.database,
            username: conn.username,
            password,
        });
    }

    let group_entries = groups
        .into_iter()
        .map(|group| SinkConfigGroupEntry {
            id: group.id,
            name: group.name,
            db_connection_id: group.db_connection_id,
            mode: group.mode,
            interval_ms: group.interval_ms,
            table_name: group.table_name,
            store_bad: group.store_bad,
            // §5.2「行の削除で行も消える」の孤児許容分 - `crate::sink`の
            // モジュール doc comment参照: 現在の catalog に無い tag_id は
            // ここで単に読み飛ばす（サイドカー側は存在するタグだけを
            // 購読することになる）。
            tags: group
                .tag_ids
                .iter()
                .filter_map(|tag_id| {
                    by_tag_id.get(tag_id).map(|entry| SinkConfigTagEntry {
                        connection_id: entry.ids.0,
                        group_id: entry.ids.1,
                        tag_id: entry.ids.2,
                        external_name: entry.external_name.clone(),
                    })
                })
                .collect(),
        })
        .collect();

    Ok(SinkConfigResponse {
        generated_at: state.manager.clock().now_ms(),
        groups: group_entries,
        connections,
        unavailable_connections,
    })
}

/// `GET /api/sink/config`のパスワード平文返却はループバック専用という
/// 前提（設計 §6-15）をプロセス起動後の初回呼び出し1回だけ`warn`する -
/// このコードベースにログレベル機構は無い（`eprintln!`のみ）ため、
/// 「初回だけ」は`std::sync::Once`で表現する。
static SINK_CONFIG_LOOPBACK_WARNING: std::sync::Once = std::sync::Once::new();

async fn sink_config_get(State(state): State<SinkAdminState>, headers: HeaderMap) -> Response {
    let now_ms = state.manager.clock().now_ms();
    if let Err(resp) = require_sink_admin(&state.api_keys, &state.auth, &headers, now_ms).await {
        return resp;
    }
    SINK_CONFIG_LOOPBACK_WARNING.call_once(|| {
        eprintln!(
            "banto-hub: warn: GET /api/sink/config はDB接続のパスワードを平文で返します。\
             サイドカーは Hub と同一マシンからのループバック接続のみを前提とします\
             （docs/banto-hub-external-db-design.md §2.2・§5.2・§6-15）。"
        );
    });
    match build_sink_config_response(&state).await {
        Ok(body) => Json(body).into_response(),
        Err(err) => ApiError(err).into_response(),
    }
}

/// `PUT /api/sink/status`のボディ（設計 §5.2）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SinkStatusPushRequest {
    groups: Vec<SinkGroupStatusPush>,
}

async fn sink_status_put(
    State(state): State<SinkAdminState>,
    headers: HeaderMap,
    Json(body): Json<SinkStatusPushRequest>,
) -> Response {
    let now_ms = state.manager.clock().now_ms();
    if let Err(resp) = require_sink_admin(&state.api_keys, &state.auth, &headers, now_ms).await {
        return resp;
    }

    let mut field_errors = Vec::new();
    for group in &body.groups {
        if !crate::sink::is_valid_sink_group_state(&group.state) {
            field_errors.push(FieldError {
                field: "groups".to_string(),
                message: format!(
                    "group {} の state が不正です（{}）: {}",
                    group.id,
                    crate::sink::ALLOWED_SINK_GROUP_STATES.join(", "),
                    group.state
                ),
            });
        }
    }
    if !field_errors.is_empty() {
        return ApiError(BantoError::Validation { field_errors }).into_response();
    }

    // 未知の sink group id は列挙して422で拒否する（実装指示）。
    let known_ids: std::collections::HashSet<i64> = match state.sink_groups.list().await {
        Ok(groups) => groups.into_iter().map(|group| group.id).collect(),
        Err(err) => return ApiError(err).into_response(),
    };
    let mut unknown_ids: Vec<i64> = body
        .groups
        .iter()
        .map(|group| group.id)
        .filter(|id| !known_ids.contains(id))
        .collect();
    unknown_ids.sort_unstable();
    unknown_ids.dedup();
    if !unknown_ids.is_empty() {
        return ApiError(BantoError::Validation {
            field_errors: vec![FieldError {
                field: "groups".to_string(),
                message: format!(
                    "未知の sink group id が含まれています: {}",
                    unknown_ids
                        .iter()
                        .map(|id| id.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }],
        })
        .into_response();
    }

    // T2-4/§5.5「行単位の INSERT は監査しない」・実装指示「本文は info
    // レベルでもログしない」と同じ抑制 - push 本文（`lastError`に接続文字列
    // の断片が混ざりうる）を一切ログに出さない。
    state.sink_status.record(body.groups, now_ms);
    StatusCode::NO_CONTENT.into_response()
}

pub(super) fn sink_admin_router(
    sink_groups: SinkGroupService,
    plc_connections: PlcConnectionService,
    manager: Arc<CollectorManager>,
    sink_status: Arc<SinkStatusStore>,
    api_keys: ApiKeysService,
    auth: AuthState,
) -> Router {
    let state = SinkAdminState {
        sink_groups,
        plc_connections,
        manager,
        sink_status,
        api_keys,
        auth,
    };
    Router::new()
        .route("/api/sink/config", get(sink_config_get))
        .route("/api/sink/status", put(sink_status_put))
        .with_state(state)
}
