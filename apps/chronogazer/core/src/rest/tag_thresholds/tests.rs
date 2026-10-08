//! `/api/tag-thresholds/*` の REST の経路（権限・監査・`409`・`404`・タグの
//! 削除との連動）。検証の総当たりと並行の保存は `crate::tag_thresholds` のテスト。

use crate::display_groups::tests::seed_tags;
use crate::rest::tests::router_with_role_tokens_and_pool;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

async fn send(
    router: &Router,
    method: &str,
    path: &str,
    token: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("X-Banto-Client", "banto")
        .header("Authorization", format!("Bearer {token}"));
    let body = match body {
        Some(value) => {
            builder = builder.header("Content-Type", "application/json");
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };
    let response = router
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value)
}

/// `audit_log` の (action, result, origin, detail) を古い順に。
async fn audit_rows(pool: &sqlx::SqlitePool) -> Vec<(String, String, String, Option<String>)> {
    sqlx::query_as(
        "SELECT action, result, origin, detail FROM audit_log \
         WHERE resource = 'tag_thresholds' ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

/// 一巡: viewer は読めて書けない（403 + `denied` の監査）、editor は保存できる
/// （成功だけ監査。値を `detail` に残す）、古い版は `409`（`expectedRevision`）、
/// 大小関係の誤りは `422` で欄の名前付き、存在しないタグは `404`。設定の無いタグ
/// は `revision: 0` の「設定なし」。タグを消すと一覧からも消える。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tag_threshold_routes_rbac_audit_conflict_and_cascade() {
    let (router, _admin, editor, viewer, pool) = router_with_role_tokens_and_pool().await;
    let (_tags, ids) = seed_tags(&pool, 2).await;
    let path = format!("/api/tag-thresholds/{}", ids[0]);

    // 設定の無いタグ: 4 つとも null・版 0。一覧は空。
    let (status, unset) = send(&router, "GET", &path, &viewer, None).await;
    assert_eq!(status, StatusCode::OK, "{unset}");
    assert_eq!(
        unset,
        json!({
            "tagId": ids[0], "thresholdLl": null, "thresholdL": null,
            "thresholdH": null, "thresholdHh": null, "revision": 0
        })
    );
    let (status, list) = send(&router, "GET", "/api/tag-thresholds", &viewer, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list, json!([]));

    // viewer は書けない。
    let body = json!({ "thresholdH": 80.0, "thresholdHh": 90.0, "expectedRevision": 0 });
    let (status, _) = send(&router, "PUT", &path, &viewer, Some(body.clone())).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // editor は保存できる。読んだ応答をそのまま送り返す形も通る（`tagId`/`revision` は無視）。
    let (status, saved) = send(&router, "PUT", &path, &editor, Some(body)).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["thresholdH"], 80.0);
    assert_eq!(saved["revision"], 1);
    let mut echo = saved.clone();
    echo["thresholdL"] = json!(10.0);
    echo["expectedRevision"] = saved["revision"].clone();
    let (status, echoed) = send(&router, "PUT", &path, &editor, Some(echo)).await;
    assert_eq!(status, StatusCode::OK, "{echoed}");
    assert_eq!(echoed["revision"], 2);

    // 古い版は 409（本文は検証エラーの形）。保存は変わらない。
    let (status, conflict) = send(
        &router,
        "PUT",
        &path,
        &editor,
        Some(json!({ "thresholdH": 1.0, "expectedRevision": 1 })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
    assert_eq!(conflict["kind"], "validation");
    assert_eq!(conflict["field_errors"][0]["field"], "expectedRevision");
    let (_, reread) = send(&router, "GET", &path, &viewer, None).await;
    assert_eq!(reread, echoed);

    // 大小関係の誤りは 422、崩れた側の欄の名前で返る。
    let (status, invalid) = send(
        &router,
        "PUT",
        &path,
        &editor,
        Some(json!({ "thresholdL": 50.0, "thresholdH": 10.0, "expectedRevision": 2 })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{invalid}");
    assert_eq!(invalid["field_errors"][0]["field"], "thresholdH");

    // 存在しないタグは 404（読み取りも書き込みも）。
    let (status, _) = send(&router, "GET", "/api/tag-thresholds/999999", &viewer, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(
        &router,
        "PUT",
        "/api/tag-thresholds/999999",
        &editor,
        Some(json!({ "thresholdH": 1.0 })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 一覧には設定のあるタグだけ。
    let (_, list) = send(&router, "GET", "/api/tag-thresholds", &viewer, None).await;
    assert_eq!(list, json!([echoed]));

    // タグを消すと、しきい値も消える。
    let (status, _) = send(
        &router,
        "DELETE",
        &format!("/api/tags/{}", ids[0]),
        &editor,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, list) = send(&router, "GET", "/api/tag-thresholds", &viewer, None).await;
    assert_eq!(list, json!([]));

    // 監査: 拒否 1 件と成功した保存 2 件だけ（読み取り・409・検証エラー・404 は記録しない）。
    let rows = audit_rows(&pool).await;
    let actions: Vec<(&str, &str, &str)> = rows
        .iter()
        .map(|(action, result, origin, _)| (action.as_str(), result.as_str(), origin.as_str()))
        .collect();
    assert_eq!(
        actions,
        [
            ("denied", "denied", "rest"),
            ("update", "ok", "rest"),
            ("update", "ok", "rest"),
        ]
    );
    let detail: Value = serde_json::from_str(rows[2].3.as_deref().unwrap()).unwrap();
    assert_eq!(
        detail,
        json!({
            "name": "tag0", "thresholdLl": null, "thresholdL": 10.0,
            "thresholdH": 80.0, "thresholdHh": 90.0
        })
    );
}
