use super::super::*;
use super::common::{
    admin_get, admin_post, admin_put, test_env, test_env_on_file_db, TestEnv, CLIENT_HEADER,
};
use axum::body::Body;
use axum::http::Request as HttpRequest;
use tower::ServiceExt;

// --- T18-5a 第2段 (docs/banto-hub-t18-design.md §4 決定6): 薄い部品の
// 先行配線 - POST /api/tags/list と GET /api/tags/group-counts --------

/// `admin_post` 経由で PLC接続 → 収集グループ → タグ を1本作る。
/// `group_id` が `None` なら新しい接続+グループも作る、`Some` ならその
/// グループへタグを追加するだけ - 返り値は `(tag_id, group_id)`。
async fn create_tag_via_admin(
    router: &Router,
    token: &str,
    suffix: &str,
    group_id: Option<i64>,
) -> (i64, i64) {
    let group_id = match group_id {
        Some(id) => id,
        None => {
            let (status, conn) = admin_post(
                router,
                "/api/plc-connections",
                token,
                json!({ "protocol": "modbus-tcp", "name": format!("conn-{suffix}"), "settings": { "host": "127.0.0.1", "port": 15300 } }),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{conn:?}");
            let conn_id = conn["id"].as_i64().unwrap();
            let (status, group) = admin_post(
                router,
                "/api/collection-groups",
                token,
                json!({
                    "name": format!("group-{suffix}"),
                    "plcConnectionId": conn_id,
                    "periodMs": 1000
                }),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{group:?}");
            group["id"].as_i64().unwrap()
        }
    };
    let (status, tag) = admin_post(
        router,
        "/api/tags",
        token,
        json!({
            "name": format!("tag-{suffix}"),
            "collectionGroupId": group_id,
            "address": "D100",
            "dataType": "i16"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{tag:?}");
    (tag["id"].as_i64().unwrap(), group_id)
}

/// ファイル DB の一時パス（テストごとに一意）。
fn temp_db_path(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "banto-hub-{label}-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn remove_db_files(path: &std::path::Path) {
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

/// #528 の回帰テスト共通: `PUT /api/tags/{tag}` を [`TxTestHook`] の下で
/// 始め、ハンドラがトランザクションを始めて**止まっている間**に、別接続
/// （`busy_timeout = 0`）から書き込みを試みる。
///
/// - `BEGIN IMMEDIATE` で始めていれば、ハンドラが書き込みロックを握っているので
///   別接続の `BEGIN IMMEDIATE` は**必ず** `SQLITE_BUSY` で失敗する。
/// - 素の `BEGIN`（DEFERRED）だと、フックの読み取りはスナップショットを張るだけで
///   ロックを握らないので、別接続の書き込みは成功してしまう（テストはここで落ちる。
///   通った場合の後続は、スナップショットが古くなって `UPDATE` が
///   `SQLITE_BUSY_SNAPSHOT`（500）になる実際の失敗の再現でもある）。
///
/// 「待った時間」ではなく「ロックを握っている」という積極的な証拠で判定するので、
/// ハンドラの開始が遅れても結果は変わらない。止めたハンドラを解放して応答を返す。
async fn put_tag_while_probing_the_write_lock(
    env: &TestEnv,
    path: &std::path::Path,
    tag: i64,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let hook = Arc::new(TxTestHook {
        reached: tokio::sync::Notify::new(),
        proceed: tokio::sync::Notify::new(),
    });
    let router = env.router.clone();
    let token = env.admin_token.clone();
    let request = tokio::spawn(TX_TEST_HOOK.scope(hook.clone(), async move {
        admin_put(&router, &format!("/api/tags/{tag}"), &token, body).await
    }));

    // 到達しなければテストの失敗（時間切れは成功の根拠にしない）。
    tokio::time::timeout(std::time::Duration::from_secs(60), hook.reached.notified())
        .await
        .expect("ハンドラがトランザクションを始めてフックへ着くはず");

    // ハンドラは止まっている。別接続（待たない設定）から書き込みを試みる。
    use sqlx::ConnectOptions;
    let mut probe = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(path)
        .busy_timeout(std::time::Duration::ZERO)
        .connect()
        .await
        .expect("connect probe");
    let attempt = sqlx::query("BEGIN IMMEDIATE").execute(&mut probe).await;
    match attempt {
        Err(sqlx::Error::Database(err)) => {
            assert!(
                matches!(err.code().as_deref(), Some("5") | Some("261")),
                "SQLITE_BUSY のはず: {err}"
            );
        }
        other => panic!(
            "ハンドラが書き込みロックを握っているはず（BEGIN IMMEDIATE で始めていれば \
             別接続の BEGIN IMMEDIATE は BUSY になる）: {other:?}"
        ),
    }
    drop(probe);

    hook.proceed.notify_one();
    request.await.expect("join")
}

/// #528: 書き込みロックを握ったまま検証する（`BEGIN IMMEDIATE`）ので
/// `PUT /api/tags/{id}` は 200 になる。素の `BEGIN` だと別接続が割り込めて
/// `database is locked`（REST 500）になりうる。
#[tokio::test]
async fn tags_update_holds_the_write_lock_from_the_start() {
    let path = temp_db_path("tags-update-busy");
    let env = test_env_on_file_db(&path).await;
    let (tag1, group_id) = create_tag_via_admin(&env.router, &env.admin_token, "imm1", None).await;

    let (status, body) = put_tag_while_probing_the_write_lock(
        &env,
        &path,
        tag1,
        json!({
            "name": "tag-imm1-renamed",
            "collectionGroupId": group_id,
            "address": "D100",
            "dataType": "i16"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["name"], "tag-imm1-renamed");

    env.pool.close().await;
    remove_db_files(&path);
}

/// #528: 同じ状況でも、本物の版の食い違いは 500 ではなく 409
/// （`tag_revision_conflict`）で返る。
#[tokio::test]
async fn tags_update_true_revision_conflict_is_409_while_holding_the_write_lock() {
    let path = temp_db_path("tags-update-conflict");
    let env = test_env_on_file_db(&path).await;
    let (tag1, group_id) = create_tag_via_admin(&env.router, &env.admin_token, "cf1", None).await;
    let (_, current) = admin_get(&env.router, &format!("/api/tags/{tag1}"), &env.admin_token).await;
    let stale = current["revision"].as_i64().unwrap() + 100;

    let (status, body) = put_tag_while_probing_the_write_lock(
        &env,
        &path,
        tag1,
        json!({
            "name": "tag-cf1-renamed",
            "collectionGroupId": group_id,
            "address": "D100",
            "dataType": "i16",
            "expectedRevision": stale
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body:?}");
    assert_eq!(body["error"], "tag_revision_conflict", "{body:?}");

    env.pool.close().await;
    remove_db_files(&path);
}

/// (a) 認証なし → 401（CSRF ヘッダは付けている - `admin_routes_require_the_csrf_header`
/// が CSRF 側は別途カバー済みなので、ここでは認証だけを外す）。
#[tokio::test]
async fn tags_list_query_requires_auth() {
    let env = test_env().await;
    let response = env
        .router
        .oneshot(
            HttpRequest::post("/api/tags/list")
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&json!({})).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

/// (b) filter + sort + pagination + totalCount が `TagService::list` に
/// そのまま素通しされること、viewer でも読めること（`GET /api/tags` と
/// 同じ require_auth のみ）、(c) `/api/tags/{id}` が `/api/tags/list` と
/// 衝突せず両方とも正しいハンドラへ解決されることを1本で確認する。
#[tokio::test]
async fn tags_list_query_filters_paginates_and_resolves_beside_tags_id() {
    let env = test_env().await;
    let (tag_a, group_id) = create_tag_via_admin(&env.router, &env.admin_token, "a", None).await;
    let (tag_b, _) = create_tag_via_admin(&env.router, &env.admin_token, "b", Some(group_id)).await;
    let (_tag_c, _) =
        create_tag_via_admin(&env.router, &env.admin_token, "c", Some(group_id)).await;

    let (status, body) = admin_post(
        &env.router,
        "/api/tags/list",
        &env.admin_token,
        json!({
            "filters": [{ "field": "collectionGroupId", "op": "eq", "value": group_id }],
            "sort": [{ "field": "name", "direction": "asc" }],
            "pagination": { "offset": 0, "limit": 2 }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["totalCount"], 3);
    let rows = body["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["name"], "tag-a");
    assert_eq!(rows[1]["name"], "tag-b");

    // viewer（読み取り専用）でも読める - `require_editor` は呼ばない。
    let (status, viewer_body) =
        admin_post(&env.router, "/api/tags/list", &env.viewer_token, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{viewer_body:?}");
    assert_eq!(viewer_body["totalCount"], 3);

    // (c) `/api/tags/{id}` 側も同じルーターで正しく解決される
    // (axum の matchit は静的セグメント `/api/tags/list` を優先するため
    // `{id}: i64` に飲み込まれない - 逆にここでは数値 id が
    // `/api/tags/{id}` へ正しく届くことを確認する)。
    let response = env
        .router
        .clone()
        .oneshot(
            HttpRequest::get(format!("/api/tags/{tag_b}"))
                .header("Authorization", format!("Bearer {}", env.admin_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let single: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(single["id"], tag_b);
    assert_eq!(single["name"], "tag-b");

    let _ = tag_a;
}

/// (d) `GET /api/tags/group-counts` がグループ別のタグ件数を返すこと。
/// 200 が返ること自体が (c) の「`/api/tags/{id}` (`{id}: i64`) に
/// 飲み込まれていない」証拠でもある - 飲み込まれていれば `group-counts`
/// は不正な i64 として 400 になる。
#[tokio::test]
async fn tags_group_counts_returns_per_group_totals() {
    let env = test_env().await;
    let (_tag_a1, group1) = create_tag_via_admin(&env.router, &env.admin_token, "ga1", None).await;
    create_tag_via_admin(&env.router, &env.admin_token, "ga2", Some(group1)).await;
    let (_tag_b1, group2) = create_tag_via_admin(&env.router, &env.admin_token, "gb1", None).await;

    let response = env
        .router
        .clone()
        .oneshot(
            HttpRequest::get("/api/tags/group-counts")
                .header("Authorization", format!("Bearer {}", env.viewer_token))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let counts = body.as_array().unwrap();
    let find = |gid: i64| {
        counts
            .iter()
            .find(|c| c["collectionGroupId"] == gid)
            .map(|c| c["tagCount"].as_i64().unwrap())
    };
    assert_eq!(find(group1), Some(2));
    assert_eq!(find(group2), Some(1));
}
