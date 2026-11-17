//! #1161 PR6: `GET /api/openapi.json`（`rest/openapi.rs`）のテスト。
//!
//! 文書とコードのずれを止める 3 本（スナップショット・キーを通すルートとの
//! 一致・ルート一覧のスナップショット）と、層の外に置いたことの確かめ。

use std::collections::BTreeSet;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::Value;
use tower::ServiceExt;
use utoipa::OpenApi;

use super::openapi::ApiDoc;
use super::tests::{api_key_routes_for_test, router_with_role_tokens_and_pool};

fn snapshot_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots")
        .join(name)
}

/// `ApiDoc::openapi()` を、コミット済みスナップショット
/// （`tests/snapshots/openapi.json`）と完全一致で比較する（HUB の
/// `openapi_snapshot_matches` と同じ形）。再生成は
/// `UPDATE_OPENAPI_SNAPSHOT=1 cargo test -p chronogazer-core openapi_snapshot`。
#[test]
fn openapi_snapshot_matches() {
    let actual = serde_json::to_value(ApiDoc::openapi()).expect("openapi serialize");
    let path = snapshot_path("openapi.json");
    if matches!(std::env::var("UPDATE_OPENAPI_SNAPSHOT").as_deref(), Ok("1")) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut text = serde_json::to_string_pretty(&actual).unwrap();
        text.push('\n');
        std::fs::write(&path, text).unwrap();
        return;
    }
    let expected: Value = serde_json::from_str(
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
/// 出力から「METHOD path」を復元する。
///
/// **写し**: `apps/banto-hub/core/src/rest/tests/api_keys.rs` の
/// `route_list_from_debug` と同じ（`fallback_router` 以降は読まない。形式が
/// 変わって 1 本も取れなければ呼び出し側の assert が落ちる。axum を上げたときは
/// 両方を見直す）。
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

/// `ApiDoc` の paths×methods を「METHOD path」にする。
fn documented_routes() -> BTreeSet<String> {
    let doc = serde_json::to_value(ApiDoc::openapi()).expect("openapi serialize");
    let mut out = BTreeSet::new();
    for (path, item) in doc["paths"].as_object().expect("paths") {
        for (method, _) in item.as_object().expect("path item") {
            out.insert(format!("{} {path}", method.to_uppercase()));
        }
    }
    out
}

/// **キーを通すルートと文書の一致**: `api_key_routes` が合成する Router の
/// 「METHOD path」が、`ApiDoc` の paths×methods と完全に同じ。キーを通すルートを
/// 足して文書を忘れる（または文書だけ足す）と落ちる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_key_routes_match_the_documented_paths() {
    let routes = route_list_from_debug(&api_key_routes_for_test().await);
    assert!(
        routes.len() > 20 && routes.contains(&"GET /api/collect".to_string()),
        "ルート一覧の復元に失敗（axum の Debug 形式が変わった可能性）: {} 本",
        routes.len()
    );
    let routed: BTreeSet<String> = routes.into_iter().collect();
    let documented = documented_routes();
    let undocumented: Vec<_> = routed.difference(&documented).collect();
    let phantom: Vec<_> = documented.difference(&routed).collect();
    assert!(
        undocumented.is_empty() && phantom.is_empty(),
        "キーを通すルートに対して文書が足りない: {undocumented:?} / 文書にあるがルートが無い: {phantom:?}"
    );
}

/// `api_router` 全体の「METHOD path」を、コミット済みスナップショット
/// （`tests/snapshots/routes.txt`、ソート済み・1 行 1 ルート）と完全一致で比較する。
/// ルートの増減に気づくためのテスト（管理系のルートを足すときも、ここで目に入る）。
/// `HEAD` は axum が `GET` から暗黙に扱うので一覧には出ない。再生成は
/// `UPDATE_ROUTES_SNAPSHOT=1 cargo test -p chronogazer-core routes_snapshot`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn routes_snapshot_matches() {
    let (router, ..) = router_with_role_tokens_and_pool().await;
    let routes = route_list_from_debug(&router);
    assert!(
        routes.len() > 50 && routes.contains(&"GET /api/collect".to_string()),
        "ルート一覧の復元に失敗（axum の Debug 形式が変わった可能性）: {} 本",
        routes.len()
    );
    let mut actual = routes.join("\n");
    actual.push('\n');
    let path = snapshot_path("routes.txt");
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

async fn get(router: &Router, path: &str, authorization: Option<&str>) -> (StatusCode, Value) {
    // CSRF のヘッダー（`X-Banto-Client`）は付けない。
    let mut builder = Request::builder().method("GET").uri(path);
    if let Some(value) = authorization {
        builder = builder.header("Authorization", value);
    }
    let response = router
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

/// `GET /api/openapi.json` は認証なし（セッションも CSRF のヘッダーも無し）で 200 の
/// JSON。キーを通すルート（`/api/tags` など）を含み、管理系の path は含まない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn openapi_json_is_public_and_lists_only_key_routes() {
    let (router, ..) = router_with_role_tokens_and_pool().await;
    let (status, doc) = get(&router, "/api/openapi.json", None).await;
    assert_eq!(status, StatusCode::OK);
    let paths = doc["paths"].as_object().expect("paths");
    assert!(paths.contains_key("/api/tags"));
    assert!(paths.contains_key("/api/collect/restart"));
    for admin_only in [
        "/api/users",
        "/api/api-keys",
        "/api/hub/connect",
        "/api/hub",
        "/api/tags/from-hub",
        "/api/backups",
        "/api/audit-log/list",
        "/api/settings/server",
        "/api/auth/login",
        "/mcp",
    ] {
        assert!(
            !paths.contains_key(admin_only),
            "{admin_only} は文書に載せない"
        );
    }
    assert_eq!(
        doc["components"]["securitySchemes"]["api_key"]["scheme"],
        "bearer"
    );
}

/// 層の外にあることの確かめ: 誤った `cg_…` の bearer が付いていても（層の内側なら
/// `api_key_gate` が 401 にする）、`/api/openapi.json` は 200。対照として、同じ
/// bearer で `/api/tags` を呼ぶと 401。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn openapi_json_ignores_a_wrong_api_key_but_the_routes_do_not() {
    let (router, ..) = router_with_role_tokens_and_pool().await;
    let wrong = "Bearer cg_0000000000000000000000000000000000000000";
    let (status, _) = get(&router, "/api/openapi.json", Some(wrong)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = get(&router, "/api/tags", Some(wrong)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// 文書のスキーマと実際の応答がずれていない: `POST /api/display-groups` の応答の
/// キー集合が `DisplayGroup` のスキーマの `properties` と同じ。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn display_group_response_keys_match_the_schema() {
    let (router, admin, ..) = router_with_role_tokens_and_pool().await;
    let request = Request::builder()
        .method("POST")
        .uri("/api/display-groups")
        .header("X-Banto-Client", "banto")
        .header("Authorization", format!("Bearer {admin}"))
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"name":"L1","kind":"gauge"}"#))
        .unwrap();
    let response = router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let actual: Value = serde_json::from_slice(&bytes).unwrap();
    let actual_keys: BTreeSet<&str> = actual
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();

    let doc = serde_json::to_value(ApiDoc::openapi()).unwrap();
    let schema_keys: BTreeSet<&str> = doc["components"]["schemas"]["DisplayGroup"]["properties"]
        .as_object()
        .expect("DisplayGroup のスキーマ")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(actual_keys, schema_keys);
}
