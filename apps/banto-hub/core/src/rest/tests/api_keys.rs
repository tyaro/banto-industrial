use super::super::*;
use super::common::{issue_api_key, test_env, test_env_with_clock, CLIENT_HEADER};
use axum::body::Body;
use axum::http::Request as HttpRequest;
use banto_tstore::{Clock, ManualClock};
use tokio::sync::broadcast as tokio_broadcast;
use tower::ServiceExt;

/// [`issue_api_key`] but also sets `expiresAt` (H10 ①) - kept as a
/// separate helper rather than adding a parameter to [`issue_api_key`]
/// so the ~10 existing call sites above (all unlimited keys) stay
/// untouched.
async fn issue_api_key_with_expiry(
    router: &Router,
    token: &str,
    name: &str,
    scopes: &[&str],
    expires_at: Option<i64>,
) -> (StatusCode, serde_json::Value) {
    let response = router
        .clone()
        .oneshot(
            HttpRequest::post("/api/api-keys")
                .header("Authorization", format!("Bearer {token}"))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "name": name,
                        "scopes": scopes,
                        "expiresAt": expires_at,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

// --- T0-2: API キー基盤 ------------------------------------------------

/// 発行 → そのキーで `/api/v1/tags` が読める（E2E、実装指示 §3の1件目）。
#[tokio::test]
async fn issued_api_key_can_read_api_v1_tags() {
    let env = test_env().await;
    let (status, issued) =
        issue_api_key(&env.router, &env.admin_token, "mes-gateway", &["read"]).await;
    assert_eq!(status, StatusCode::CREATED, "{issued:?}");
    let key = issued["key"].as_str().expect("key should be present");
    assert!(key.starts_with("bh_"));
    assert_eq!(issued["scopes"], serde_json::json!(["read"]));

    let response = env
        .router
        .oneshot(
            HttpRequest::get("/api/v1/tags")
                .header("Authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

/// 失効済みキーでのアクセスは 401 + audit_log に "denied"/"api_keys" が
/// 記録される（実装指示 §3の2件目 + §1「失効済みキーでのアクセス試行は
/// audit_log に記録する」）。
#[tokio::test]
async fn revoked_api_key_is_401_and_audited() {
    let env = test_env().await;
    let (_status, issued) =
        issue_api_key(&env.router, &env.admin_token, "revoke-me", &["read"]).await;
    let key = issued["key"].as_str().unwrap().to_string();
    let id = issued["id"].as_i64().unwrap();

    let revoke = env
        .router
        .clone()
        .oneshot(
            HttpRequest::post(format!("/api/api-keys/{id}/revoke"))
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(revoke.status(), StatusCode::OK);

    let response = env
        .router
        .oneshot(
            HttpRequest::get("/api/v1/tags")
                .header("Authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let audit = AuditLogService::new(Db::Sqlite(env.pool.clone()));
    let entries = audit.list(ListParams::default()).await.unwrap();
    let denied = entries
        .rows
        .iter()
        .find(|row| row.action == "denied" && row.resource == "api_keys")
        .expect("a denied/api_keys audit row should exist");
    assert_eq!(denied.entity_id.as_deref(), Some(id.to_string().as_str()));
}

/// #435: 使えない API キーへの HTTP 応答の表（REST・サイドカー用・MCP で共通の
/// 変換）。失効・期限切れ・存在しないは 401、トリップは 403 `key_tripped`、
/// 照合できないは 500。
#[tokio::test]
async fn api_key_rejection_response_table() {
    use crate::api_keys::{ApiKeyRejection as R, UnauthenticatedReason as U};
    let table: Vec<(&str, R, StatusCode, serde_json::Value)> = vec![
        (
            "revoked",
            R::Unauthenticated(U::Revoked),
            StatusCode::UNAUTHORIZED,
            serde_json::json!("unauthorized"),
        ),
        (
            "expired",
            R::Unauthenticated(U::Expired),
            StatusCode::UNAUTHORIZED,
            serde_json::json!("unauthorized"),
        ),
        (
            "not found",
            R::Unauthenticated(U::NotFound),
            StatusCode::UNAUTHORIZED,
            serde_json::json!("unauthorized"),
        ),
        (
            "tripped",
            R::Tripped,
            StatusCode::FORBIDDEN,
            serde_json::json!("key_tripped"),
        ),
        (
            "unavailable",
            R::Unavailable(BantoError::Storage("db down".to_string())),
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!("storage"),
        ),
    ];
    for (label, rejection, status, marker) in table {
        let response = api_key_rejection_response(rejection);
        assert_eq!(response.status(), status, "{label}");
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let got = if body.get("error").is_some() {
            &body["error"]
        } else {
            &body["kind"]
        };
        assert_eq!(*got, marker, "{label}: {body}");
    }
}

/// #434: `GET` を API キーで叩いてステータスと本文を返す。
async fn get_with_key(router: &Router, path: &str, key: &str) -> (StatusCode, serde_json::Value) {
    let response = router
        .clone()
        .oneshot(
            HttpRequest::get(path)
                .header("Authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

/// #434: タグ空間 `/api/v1/*` とサイドカー用の `/api/sink/config`。無効な
/// キーは DB が答えていれば従来どおり（失効・期限切れ・存在しないは 401、
/// タグ空間のトリップは 403 `key_tripped`）。照合そのものができない（DB
/// エラー）ときは、どのキーでも 401 ではなく 500（`ApiError`、セッションの
/// 照合失敗と同じ形）。
#[tokio::test]
async fn api_key_check_failure_is_500_while_invalid_keys_stay_401() {
    let env = test_env().await;
    let scopes = || vec!["read".to_string(), "admin".to_string()];
    let valid = env
        .api_keys
        .issue("state-valid", scopes(), None)
        .await
        .unwrap();
    let revoked = env
        .api_keys
        .issue("state-revoked", scopes(), None)
        .await
        .unwrap();
    env.api_keys.revoke(revoked.id).await.unwrap();
    let tripped = env
        .api_keys
        .issue("state-tripped", scopes(), None)
        .await
        .unwrap();
    env.api_keys.trip(tripped.id).await.unwrap();
    let expired = env
        .api_keys
        .issue("state-expired", scopes(), Some(1))
        .await
        .unwrap();
    let unknown = "bh_ZZZZZZZZ_well-formed-but-unregistered".to_string();
    // (キー, `/api/v1/tags` の期待, `/api/sink/config` の期待)
    let keys: Vec<(&str, String, StatusCode, StatusCode)> = vec![
        ("valid", valid.key, StatusCode::OK, StatusCode::OK),
        (
            "revoked",
            revoked.key,
            StatusCode::UNAUTHORIZED,
            StatusCode::UNAUTHORIZED,
        ),
        (
            "tripped",
            tripped.key,
            StatusCode::FORBIDDEN,
            StatusCode::FORBIDDEN,
        ),
        (
            "expired",
            expired.key,
            StatusCode::UNAUTHORIZED,
            StatusCode::UNAUTHORIZED,
        ),
        (
            "unknown",
            unknown,
            StatusCode::UNAUTHORIZED,
            StatusCode::UNAUTHORIZED,
        ),
    ];
    for (label, key, tags, sink) in &keys {
        let (status, body) = get_with_key(&env.router, "/api/v1/tags", key).await;
        assert_eq!(status, *tags, "tags {label}: {body:?}");
        let (status, sink_body) = get_with_key(&env.router, "/api/sink/config", key).await;
        assert_eq!(status, *sink, "sink {label}: {sink_body:?}");
        // #435: トリップはどちらも 403 `key_tripped`（同じ本文）。
        if *label == "tripped" {
            assert_eq!(body["error"], "key_tripped", "{body:?}");
            assert_eq!(sink_body["error"], "key_tripped", "{sink_body:?}");
        }
    }

    sqlx::query("ALTER TABLE api_keys RENAME TO api_keys_away")
        .execute(&env.pool)
        .await
        .expect("move api_keys away");
    for (label, key, _, _) in &keys {
        for path in ["/api/v1/tags", "/api/sink/config"] {
            let (status, body) = get_with_key(&env.router, path, key).await;
            assert_eq!(
                status,
                StatusCode::INTERNAL_SERVER_ERROR,
                "{path} {label}: the key could not be checked, so it is not a 401: {body:?}"
            );
            assert_eq!(body["kind"], "storage", "{path} {label}: {body:?}");
        }
    }
}

// --- H10 ①: 任意の有効期限 ----------------------------------------------

/// 無期限キー（`expiresAt` 省略）は今までどおり認証できる - 期限切れ
/// 判定の追加が既定動作を変えていないことの回帰防止（実装指示の受け入れ
/// 条件: 「無期限キーの従来動作不変」）。`ManualClock` を大きく進めても
/// 無期限キーは影響を受けないことも合わせて確認する。
#[tokio::test]
async fn unlimited_api_key_still_authenticates_regardless_of_the_clock() {
    let clock = Arc::new(ManualClock::new(1_000_000, 0));
    let env = test_env_with_clock(clock.clone()).await;
    let (status, issued) =
        issue_api_key(&env.router, &env.admin_token, "unlimited", &["read"]).await;
    assert_eq!(status, StatusCode::CREATED, "{issued:?}");
    let key = issued["key"].as_str().unwrap().to_string();

    clock.advance_ms(999_999_999_999);

    let response = env
        .router
        .oneshot(
            HttpRequest::get("/api/v1/tags")
                .header("Authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

/// 期限付きキー: 期限内は 200、`advance_ms` で期限を過ぎさせると 401 に
/// なり audit_log に `denied`/`api_keys`/`{"reason":"expired"}` が記録
/// される（実装指示の受け入れ条件: 「期限切れキーの 401 と UI 警告の
/// テスト」の REST 側、`revoked_api_key_is_401_and_audited` と同型）。
#[tokio::test]
async fn expired_api_key_is_401_and_audited() {
    let clock = Arc::new(ManualClock::new(1_000_000, 0));
    let env = test_env_with_clock(clock.clone()).await;

    let expires_at = clock.now_ms() + 60_000;
    let (status, issued) = issue_api_key_with_expiry(
        &env.router,
        &env.admin_token,
        "expiring",
        &["read"],
        Some(expires_at),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{issued:?}");
    // `IssuedApiKeyResponse`（発行応答）は `id`/`name`/`prefix`/`scopes`/
    // `key` のみで `expiresAt` は含まない（発行時点で入力どおりの値を
    // そのまま返すだけの情報であり、既存の `created_at` 等と同じく
    // 「一覧を見ればわかる」ため - このモジュールの
    // `IssuedApiKeyResponse` doc comment参照）。代わりに一覧
    // （`GET /api/api-keys`）に camelCase の `expiresAt` として正しく
    // 反映されることをここで確認する。
    let key = issued["key"].as_str().unwrap().to_string();
    let id = issued["id"].as_i64().unwrap();

    let list_response = env
        .router
        .clone()
        .oneshot(
            HttpRequest::get("/api/api-keys")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list_response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(list_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let listed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let entry = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"].as_i64() == Some(id))
        .expect("issued key should appear in the list");
    assert_eq!(entry["expiresAt"].as_i64(), Some(expires_at));

    // 期限前: 通常どおり読める。
    let response = env
        .router
        .clone()
        .oneshot(
            HttpRequest::get("/api/v1/tags")
                .header("Authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // 期限を過ぎさせる。
    clock.advance_ms(60_001);

    let response = env
        .router
        .oneshot(
            HttpRequest::get("/api/v1/tags")
                .header("Authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let audit = AuditLogService::new(Db::Sqlite(env.pool.clone()));
    let entries = audit.list(ListParams::default()).await.unwrap();
    let denied = entries
        .rows
        .iter()
        .find(|row| {
            row.action == "denied"
                && row.resource == "api_keys"
                && row
                    .detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains("\"expired\""))
        })
        .expect("a denied/api_keys audit row with reason=expired should exist");
    assert_eq!(denied.entity_id.as_deref(), Some(id.to_string().as_str()));
}

/// 発行時点で既に過去/現在時刻以下の `expiresAt` は 422（`Validation`）で
/// 拒否される（実装指示: 「Some(e) and e <= now_ms、reject...」）。
#[tokio::test]
async fn creating_an_api_key_with_a_past_expiry_is_rejected() {
    let clock = Arc::new(ManualClock::new(1_000_000, 0));
    let env = test_env_with_clock(clock.clone()).await;

    let (status, body) = issue_api_key_with_expiry(
        &env.router,
        &env.admin_token,
        "already-expired",
        &["read"],
        Some(clock.now_ms()), // e <= now_ms(ちょうど今) は拒否
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    assert_eq!(body["kind"], "validation");
}

/// `write:` のみのスコープを持つキーで `/api/v1/*`（read 専用エンド
/// ポイント）にアクセスすると 403（実装指示 §3の3件目）。
#[tokio::test]
async fn write_only_scope_key_reading_api_v1_is_403() {
    let env = test_env().await;
    let (status, issued) = issue_api_key(
        &env.router,
        &env.admin_token,
        "writer-only",
        &["write:line1.fast.temp01"],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{issued:?}");
    let key = issued["key"].as_str().unwrap();

    let response = env
        .router
        .oneshot(
            HttpRequest::get("/api/v1/tags")
                .header("Authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

/// 不正なスコープ構文（ワイルドカード）での発行は 400 相当
/// （`BantoError::Validation` → `422`。実装指示 §3の5件目 - `ApiError`
/// は `Validation` を `422 UNPROCESSABLE_ENTITY` にマップする、
/// `banto_server::response::status_for` 参照。本テストは "400 系" の
/// 実体であるこのステータスを確認する）。
#[tokio::test]
async fn issuing_with_invalid_scope_syntax_is_rejected() {
    let env = test_env().await;
    let (status, body) = issue_api_key(
        &env.router,
        &env.admin_token,
        "bad-scope",
        &["write:line1.fast.*"],
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    assert_eq!(body["kind"], "validation");
}

/// viewer ロールで `POST /api/api-keys` すると 403（admin 限定、
/// 実装指示 §3の6件目）。
#[tokio::test]
async fn viewer_role_cannot_issue_api_keys() {
    let env = test_env().await;
    let (status, _body) =
        issue_api_key(&env.router, &env.viewer_token, "should-fail", &["read"]).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// `GET /api/v1/openapi.json` は認証不要で 200、`/api/v1/values` 等の
/// パスを含む（実装指示 §3の7件目）。
#[tokio::test]
async fn openapi_json_is_public_and_lists_tag_space_paths() {
    let env = test_env().await;
    let response = env
        .router
        .oneshot(
            HttpRequest::get("/api/v1/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let paths = json["paths"].as_object().expect("paths object");
    for path in [
        "/api/v1/tags",
        "/api/v1/values",
        "/api/v1/values/{tag}",
        "/api/v1/status",
        "/api/v1/events",
    ] {
        assert!(paths.contains_key(path), "missing path: {path}");
    }
    // T16-2 第三スライス（docs/banto-hub-t16-design.md §5）:
    // `crate::http_hub_health::HttpHubHealthProbe`がワイヤ上で profile-id
    // を確認できるようにするための拡張フィールド。
    assert_eq!(
        json["info"]["x-banto-hub-profile-id"],
        serde_json::json!(crate::profile_paths::DEFAULT_PROFILE_ID)
    );
}

/// `ApiDoc::openapi()` の出力を、コミット済みスナップショット
/// （`tests/snapshots/openapi.json`）と完全一致で比較する。utoipa の
/// メジャー更新などで API 契約（パス・パラメータ・必須性・型）が意図せず
/// 変わっていないことを検知するためのテスト。再生成は
/// `UPDATE_OPENAPI_SNAPSHOT=1 cargo test -p banto-hub-core openapi_snapshot`。
/// 比較は `serde_json::Value`（キー順非依存）、ファイルは pretty 出力。
#[test]
fn openapi_snapshot_matches() {
    let actual = serde_json::to_value(ApiDoc::openapi()).expect("openapi serialize");
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots")
        .join("openapi.json");
    if matches!(std::env::var("UPDATE_OPENAPI_SNAPSHOT").as_deref(), Ok("1")) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut text = serde_json::to_string_pretty(&actual).unwrap();
        text.push('\n');
        std::fs::write(&path, text).unwrap();
        return;
    }
    let expected: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&path)
            .expect("snapshot missing; run with UPDATE_OPENAPI_SNAPSHOT=1"),
    )
    .expect("snapshot parse");
    assert!(
        actual == expected,
        "OpenAPI output differs from tests/snapshots/openapi.json; review the diff and regenerate with UPDATE_OPENAPI_SNAPSHOT=1"
    );
}

/// axum 0.8 の `Router` はルート一覧を返す公開 API を持たないので、`Debug`
/// 出力（`path_router.routes` の `RouteId(n): MethodRouter { get: Route, .. }`
/// と `node` の `RouteId(n): "/path"`）から「METHOD path」を復元する。
/// `fallback_router` 以降は読まない。形式が変わって 1 本も取れなければ
/// 呼び出し側の assert が落ちる（axum を上げたときはここを見直す）。
fn route_list_from_debug(router: &Router) -> Vec<String> {
    const METHODS: [&str; 9] = [
        "get", "head", "delete", "options", "patch", "post", "put", "trace", "connect",
    ];
    let dump = format!("{router:#?}");
    let dump = dump.split("fallback_router:").next().unwrap();
    let (routes_part, node_part) = dump
        .split_once("        node: Node {")
        .expect("Router Debug に node が無い");
    let id_of = |lines: &mut std::iter::Peekable<std::str::Lines>| -> Option<u32> {
        // `RouteId(` の次の行が `N,`
        let n = lines.next()?.trim().trim_end_matches(',').to_string();
        n.parse().ok()
    };
    // id -> メソッド一覧
    let mut methods: std::collections::BTreeMap<u32, Vec<&str>> = Default::default();
    let mut lines = routes_part.lines().peekable();
    let mut current: Option<u32> = None;
    while let Some(line) = lines.next() {
        let t = line.trim();
        if t == "RouteId(" {
            current = id_of(&mut lines);
            if let Some(id) = current {
                methods.entry(id).or_default();
            }
            continue;
        }
        if let Some(id) = current {
            for m in METHODS {
                if let Some(rest) = t.strip_prefix(&format!("{m}: ")) {
                    if rest != "None," {
                        methods.get_mut(&id).unwrap().push(m);
                    }
                }
            }
        }
    }
    // id -> パス
    let mut paths: std::collections::BTreeMap<u32, String> = Default::default();
    let mut lines = node_part.lines().peekable();
    let mut pending: Option<u32> = None;
    while let Some(line) = lines.next() {
        let t = line.trim();
        if t == "RouteId(" {
            pending = id_of(&mut lines);
            continue;
        }
        if let (Some(id), Some(rest)) = (pending, t.strip_prefix("): \"")) {
            if let Some(path) = rest.strip_suffix("\",") {
                paths.insert(id, path.to_string());
                pending = None;
            }
        }
    }
    let mut out = Vec::new();
    for (id, ms) in &methods {
        let path = paths
            .get(id)
            .unwrap_or_else(|| panic!("RouteId({id}) のパスが node に無い"));
        for m in ms {
            out.push(format!("{} {path}", m.to_uppercase()));
        }
    }
    out.sort();
    out
}

/// `api_router`（本番の `api_router_with_controller` と同じ
/// `api_router_with_controller_mode` の合成）と `opcua_settings_router` が登録する「METHOD path」の
/// 全一覧を、コミット済みスナップショット（`tests/snapshots/routes.txt`、
/// ソート済み・1 行 1 ルート）と完全一致で比較する。rest.rs を子モジュールへ
/// 分割する間に、ルートが増減・変更していないことを検知するためのテスト
/// （docs/banto-hub-driver-refactor-plan.md §8.1）。`HEAD` は axum が `GET`
/// から暗黙に扱うので一覧には出ない。再生成は
/// `UPDATE_ROUTES_SNAPSHOT=1 cargo test -p banto-hub-core routes_snapshot`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn routes_snapshot_matches() {
    let env = test_env().await;
    let mut routes = route_list_from_debug(&env.router);
    // `/api/opcua-*` は `HubRuntime`（runtime.rs）が `api_router_with_controller`
    // とは別に組んで merge するので、同じ手順で別に取って足す。
    let users = UsersService::new(Db::Sqlite(env.pool.clone()));
    let audit = AuditLogService::new(Db::Sqlite(env.pool.clone()));
    let (tx, _rx) = tokio_broadcast::channel(16);
    let opcua_router = opcua_settings_router(
        env.manager.clone(),
        crate::opcua::OpcUaPublisher::new(env.manager.clone()),
        audit.clone(),
        user_auth_state(users, audit),
        tx,
    );
    routes.extend(route_list_from_debug(&opcua_router));
    routes.sort();
    assert!(
        routes.len() > 50 && routes.contains(&"GET /api/status".to_string()),
        "ルート一覧の復元に失敗（axum の Debug 形式が変わった可能性）: {} 本",
        routes.len()
    );
    let mut actual = routes.join("\n");
    actual.push('\n');
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots")
        .join("routes.txt");
    if matches!(std::env::var("UPDATE_ROUTES_SNAPSHOT").as_deref(), Ok("1")) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .expect("snapshot missing; run with UPDATE_ROUTES_SNAPSHOT=1")
        .replace("\r\n", "\n");
    assert!(
        actual == expected,
        "route list differs from tests/snapshots/routes.txt; review the diff and regenerate with UPDATE_ROUTES_SNAPSHOT=1\n--- actual ---\n{actual}"
    );
}

/// `GET /api/v1/swagger-ui/`（末尾スラッシュあり）は認証不要で 200 の
/// Swagger UI HTML を返し、それが読み込む `swagger-initializer.js`
/// （同じく認証不要）が `/api/v1/openapi.json` を指すよう設定されている
/// （`openapi_router` の doc comment、ux-plan.md §5「OpenAPI の
/// Swagger UI 同梱」）。`Config`の urls はテンプレート
/// `swagger-initializer.js`側に埋め込まれる実装（`index.html`自体には
/// 現れない - utoipa-swagger-ui の`format_config`）ため、2ファイルに
/// 分けて確認する。末尾スラッシュ無しは 301/308 でスラッシュ有りへ
/// redirect される（utoipa-swagger-ui の axum 統合の既定挙動）。
#[tokio::test]
async fn swagger_ui_is_public_and_references_openapi_json() {
    let env = test_env().await;

    let redirect = env
        .router
        .clone()
        .oneshot(
            HttpRequest::get("/api/v1/swagger-ui")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        redirect.status().is_redirection(),
        "expected redirect, got {}",
        redirect.status()
    );

    let index_response = env
        .router
        .clone()
        .oneshot(
            HttpRequest::get("/api/v1/swagger-ui/")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(index_response.status(), StatusCode::OK);
    let index_bytes = axum::body::to_bytes(index_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let index_html = String::from_utf8(index_bytes.to_vec()).unwrap();
    assert!(
        index_html.contains("swagger-initializer.js"),
        "swagger-ui index.html should load swagger-initializer.js"
    );

    let init_response = env
        .router
        .oneshot(
            HttpRequest::get("/api/v1/swagger-ui/swagger-initializer.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(init_response.status(), StatusCode::OK);
    let init_bytes = axum::body::to_bytes(init_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let init_js = String::from_utf8(init_bytes.to_vec()).unwrap();
    assert!(
        init_js.contains("/api/v1/openapi.json"),
        "swagger-initializer.js should be configured to fetch /api/v1/openapi.json"
    );
}

/// セッション token でも引き続き `/api/v1/*` が読める（設計 T0-1 からの
/// 互換維持、実装指示 §3の4件目）- API キーが未発行でも管理 UI の
/// bearer セッションだけで動くことを、`test_env` が発行済みキーを
/// 一切作らないまま確認する。
#[tokio::test]
async fn session_token_still_reads_api_v1() {
    let env = test_env().await;
    let response = env
        .router
        .oneshot(
            HttpRequest::get("/api/v1/status")
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

/// `last_used_at` が実際の HTTP リクエスト経由で更新されることの
/// REST レベルの確認（60秒スロットルの純粋関数単体テストは
/// `crate::api_keys` 側にある - 実装指示 §3の8件目）。
#[tokio::test]
async fn last_used_at_is_populated_after_a_request() {
    let env = test_env().await;
    let (_status, issued) =
        issue_api_key(&env.router, &env.admin_token, "touch-me", &["read"]).await;
    let key = issued["key"].as_str().unwrap();
    let id = issued["id"].as_i64().unwrap();

    let before = env.api_keys.list().await.unwrap();
    let before_entry = before.iter().find(|k| k.id == id).unwrap();
    assert_eq!(before_entry.last_used_at, None);

    let response = env
        .router
        .oneshot(
            HttpRequest::get("/api/v1/tags")
                .header("Authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let after = env.api_keys.list().await.unwrap();
    let after_entry = after.iter().find(|k| k.id == id).unwrap();
    assert!(after_entry.last_used_at.is_some());
}
