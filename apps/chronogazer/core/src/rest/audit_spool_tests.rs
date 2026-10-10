//! #437（banto ADR-0019）: 監査の保留の REST の経路。
//!
//! `audit_log` の表を取り除いたまま（DB の障害を模す）管理者が設定を変えると、
//! その監査は保留ファイルに 1 件退避され、`GET /api/audit-log/spool` が件数を
//! 返す。表を戻して流し込むと**ちょうど 1 件**が `detail.spooled = true` で入り、
//! もう一度流し込んでも・サービスを組み直して（再起動）も増えない。
//!
//! 反証（コミット本文）: `audit_spool::build_audit_service` の `with_spool` を外すと、
//! 「保留ファイルが 1 件」で落ちる（監査は標準エラー出力に出るだけで失われる）。

use super::tests::router_with_role_tokens_audit_pool_data_dir_and_spool;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use sqlx::SqlitePool;
use std::path::{Path, PathBuf};
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
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

async fn rename_table(pool: &SqlitePool, from: &str, to: &str) {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "ALTER TABLE {from} RENAME TO {to}"
    )))
    .execute(pool)
    .await
    .unwrap_or_else(|err| panic!("rename {from} -> {to}: {err}"));
}

fn spooled_files(db_dir: &Path) -> Vec<PathBuf> {
    let dir = crate::audit_spool::spool_dir(db_dir);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    entries
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "json"))
        .collect()
}

async fn settings_change_rows(pool: &SqlitePool, table: &str) -> Vec<Option<String>> {
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT detail FROM {table} WHERE action = 'settings_change' AND resource = 'settings'"
    )))
    .fetch_all(pool)
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_audit_written_while_the_table_is_missing_lands_exactly_once_after_recovery() {
    let db_dir = tempfile::tempdir().unwrap();
    let (router, audit, pool, admin, _editor, _viewer) =
        router_with_role_tokens_audit_pool_data_dir_and_spool(
            PathBuf::from("unused-in-tests"),
            Some(db_dir.path()),
        )
        .await;
    assert!(spooled_files(db_dir.path()).is_empty());

    // 何も溜まっていなければ 0。
    let (status, json) = send(&router, "GET", "/api/audit-log/spool", &admin, None).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["pendingCount"], 0, "{json}");
    assert_eq!(json["pendingOldestTs"], Value::Null, "{json}");
    assert_eq!(json["droppedCount"], 0, "{json}");
    assert_eq!(json["failedCount"], 0, "{json}");

    rename_table(&pool, "audit_log", "audit_log_away").await;

    // 設定の変更は成功する（監査の失敗は操作を失敗させない）。
    let (status, body) = send(
        &router,
        "PUT",
        "/api/audit-log/config",
        &admin,
        Some(json!({ "retentionDays": 30, "retentionRows": null })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        settings_change_rows(&pool, "audit_log_away")
            .await
            .is_empty(),
        "the audit could not reach the table"
    );
    assert_eq!(spooled_files(db_dir.path()).len(), 1, "spooled once");

    let (status, json) = send(&router, "GET", "/api/audit-log/spool", &admin, None).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["pendingCount"], 1, "{json}");
    assert!(json["pendingOldestTs"].is_string(), "{json}");
    assert_eq!(json["droppedCount"], 0, "{json}");
    assert_eq!(json["failedCount"], 0, "{json}");

    rename_table(&pool, "audit_log_away", "audit_log").await;
    let report = audit.flush_spool().await.expect("flush");
    assert_eq!(report.flushed, 1, "{report:?}");
    assert_eq!(report.remaining, 0, "{report:?}");
    assert!(spooled_files(db_dir.path()).is_empty());

    let rows = settings_change_rows(&pool, "audit_log").await;
    assert_eq!(rows.len(), 1, "exactly one row after recovery: {rows:?}");
    let detail: Value = serde_json::from_str(rows[0].as_deref().unwrap()).unwrap();
    assert_eq!(detail["spooled"], true, "{detail}");
    assert_eq!(detail["retentionDays"], 30, "{detail}");

    let (status, json) = send(&router, "GET", "/api/audit-log/spool", &admin, None).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["pendingCount"], 0, "{json}");
    assert_eq!(json["pendingOldestTs"], Value::Null, "{json}");

    // もう一度流し込んでも、サービスを組み直して（再起動）も増えない。
    let report = audit.flush_spool().await.expect("flush again");
    assert_eq!(report.flushed, 0, "{report:?}");
    let restarted =
        crate::audit_spool::build_audit_service(crate::db::Db::Sqlite(pool.clone()), db_dir.path());
    let report = restarted.flush_spool().await.expect("flush after restart");
    assert_eq!(report.flushed, 0, "{report:?}");
    assert_eq!(settings_change_rows(&pool, "audit_log").await.len(), 1);
}

/// 状態の口は `admin` 限定（editor・viewer は 403）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_spool_status_is_admin_only() {
    let db_dir = tempfile::tempdir().unwrap();
    let (router, _audit, _pool, admin, editor, viewer) =
        router_with_role_tokens_audit_pool_data_dir_and_spool(
            PathBuf::from("unused-in-tests"),
            Some(db_dir.path()),
        )
        .await;
    for token in [&editor, &viewer] {
        let (status, _) = send(&router, "GET", "/api/audit-log/spool", token, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    let (status, _) = send(&router, "GET", "/api/audit-log/spool", &admin, None).await;
    assert_eq!(status, StatusCode::OK);
}
