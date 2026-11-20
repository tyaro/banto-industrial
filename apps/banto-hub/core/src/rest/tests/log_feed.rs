//! #1253: `GET /api/log-feed`（下のログのパネルの取得の口、`crate::rest::log_feed`）の
//! 見せる範囲・認証・続きの位置。

use super::common::*;
use crate::write_audit::{WriteAuditAction, WriteAuditResult, WriteAuditRow, WriteAuditService};
use axum::http::StatusCode;
use serde_json::{json, Value};

async fn saved_connection(env: &TestEnv, name: &str) -> i64 {
    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "protocol": "modbus-tcp", "name": name, "settings": { "host": "127.0.0.1", "port": 15303 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");
    conn["id"].as_i64().unwrap()
}

async fn collect_event(env: &TestEnv, ts: i64, kind: &str, connection_key: &str) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO collect_events (ts, kind, connection_key, detail) \
         VALUES (?, ?, ?, 'read timeout') RETURNING id",
    )
    .bind(ts)
    .bind(kind)
    .bind(connection_key)
    .fetch_one(&env.pool)
    .await
    .unwrap()
}

async fn audit_row(
    env: &TestEnv,
    ts: &str,
    origin: &str,
    action: &str,
    entity_id: &str,
    detail: Value,
) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO audit_log (ts, actor_username, actor_role, action, resource, entity_id, \
         detail, origin, result) VALUES (?, 'admin', 'admin', ?, 'ingest', ?, ?, ?, 'ok') \
         RETURNING id",
    )
    .bind(ts)
    .bind(action)
    .bind(entity_id)
    .bind(detail.to_string())
    .bind(origin)
    .fetch_one(&env.pool)
    .await
    .unwrap()
}

fn write_row() -> WriteAuditRow {
    WriteAuditRow::new(
        7,
        "secret-writer-key",
        "bh_writer",
        10,
        "line1.temp",
        WriteAuditAction::Write,
        WriteAuditResult::Ok,
    )
    .with_value_requested(42.5)
}

async fn feed(env: &TestEnv, token: &str, query: &str) -> (StatusCode, Value) {
    admin_get(&env.router, &format!("/api/log-feed{query}"), token).await
}

fn entries(body: &Value) -> &Vec<Value> {
    body["entries"].as_array().expect("entries")
}

fn cursor(body: &Value) -> String {
    body["cursor"].as_str().expect("cursor").to_string()
}

/// 管理者は 3 種と詳細を、管理者未満は接続の行だけを受け取る。受け入れの切断の
/// API キーの名前・接続元・監査の生の detail は、管理者未満の応答のどこにも出ない。
#[tokio::test]
async fn admins_see_all_three_and_viewers_see_connections_only() {
    let env = test_env().await;
    let plc = saved_connection(&env, "line-log-feed").await;
    let plc_event = collect_event(
        &env,
        1_700_000_000_000,
        "plc_disconnected",
        &format!("conn:{plc}"),
    )
    .await;
    // しきい値の行は接続の記録ではないので、誰にも出さない。
    collect_event(
        &env,
        1_700_000_000_001,
        "threshold_entered",
        &format!("conn:{plc}"),
    )
    .await;
    let ingest = audit_row(
        &env,
        "2026-11-21 01:02:03",
        "ingest",
        "ingest_disconnected",
        "42",
        json!({
            "api_key_id": 9,
            "api_key_name": "secret-ingest-key",
            "connection_id": 42,
            "connection_name_snapshot": "push-line",
            "peer": "10.9.8.7:5555",
            "transport": "ws",
            "reason": "client_closed",
            "accepted": 3
        }),
    )
    .await;
    let operation = audit_row(
        &env,
        "2026-11-21 01:02:04",
        "rest",
        "update",
        "5",
        json!({ "changed": ["name"] }),
    )
    .await;
    let write_audit = WriteAuditService::new(env.pool.clone());
    let write = write_audit.insert_row(&write_row()).await.unwrap();

    let (status, admin) = feed(&env, &env.admin_token, "").await;
    assert_eq!(status, StatusCode::OK, "{admin}");
    let find = |body: &Value, key: &str| {
        entries(body)
            .iter()
            .find(|entry| entry["key"] == key)
            .cloned()
            .unwrap_or_else(|| panic!("{key} missing in {body}"))
    };
    let plc_entry = find(&admin, &format!("c:{plc_event}"));
    assert_eq!(plc_entry["source"], "connection");
    assert_eq!(plc_entry["kind"], "plc_disconnected");
    assert_eq!(plc_entry["connection_id"], plc);
    assert_eq!(plc_entry["connection_name"], "line-log-feed");
    assert_eq!(plc_entry["ts_ms"], 1_700_000_000_000_i64);
    let ingest_entry = find(&admin, &format!("a:{ingest}"));
    assert_eq!(ingest_entry["source"], "connection");
    assert_eq!(ingest_entry["kind"], "ingest_disconnected");
    assert_eq!(ingest_entry["connection_name"], "push-line");
    assert_eq!(
        ingest_entry["audit_detail"]["api_key_name"],
        "secret-ingest-key"
    );
    // 2026-11-21 01:02:03 UTC
    assert_eq!(ingest_entry["ts_ms"], 1_795_222_923_000_i64);
    let operation_entry = find(&admin, &format!("a:{operation}"));
    assert_eq!(operation_entry["source"], "operation");
    assert_eq!(operation_entry["detail"]["changed"][0], "name");
    let write_entry = find(&admin, &format!("w:{write}"));
    assert_eq!(write_entry["source"], "write");
    assert_eq!(write_entry["result"], "ok");
    assert_eq!(write_entry["tag_name"], "line1.temp");
    assert!(
        entries(&admin)
            .iter()
            .all(|entry| entry["kind"] != "threshold_entered"),
        "{admin}"
    );
    // 発生の時刻の降順。
    let times: Vec<i64> = entries(&admin)
        .iter()
        .map(|entry| entry["ts_ms"].as_i64().unwrap())
        .collect();
    assert!(times.windows(2).all(|pair| pair[0] >= pair[1]), "{times:?}");

    let (status, viewer) = feed(&env, &env.viewer_token, "").await;
    assert_eq!(status, StatusCode::OK, "{viewer}");
    assert!(
        entries(&viewer)
            .iter()
            .all(|entry| entry["source"] == "connection"),
        "{viewer}"
    );
    assert_eq!(entries(&viewer).len(), 2, "{viewer}");
    let ingest_entry = find(&viewer, &format!("a:{ingest}"));
    assert_eq!(ingest_entry["connection_name"], "push-line");
    assert_eq!(ingest_entry["transport"], "ws");
    assert_eq!(ingest_entry["reason"], "client_closed");
    let text = viewer.to_string();
    for leaked in [
        "secret-ingest-key",
        "10.9.8.7",
        "audit_detail",
        "api_key",
        "peer",
        "secret-writer-key",
        "line1.temp",
    ] {
        assert!(!text.contains(leaked), "{leaked} leaked: {text}");
    }
    // 管理者未満の続きの位置は書き込みの先頭を表さない。
    assert!(cursor(&viewer).ends_with(".w0"), "{viewer}");

    // 管理者未満が w を含む続きの位置を渡しても、書き込みの行は返らない。
    let (status, viewer) = feed(&env, &env.viewer_token, "?cursor=c0.a0.w0").await;
    assert_eq!(status, StatusCode::OK, "{viewer}");
    assert!(
        entries(&viewer)
            .iter()
            .all(|entry| entry["source"] == "connection"),
        "{viewer}"
    );
    assert!(!viewer.to_string().contains("secret-writer-key"));
}

/// API キー（`bh_`）・セッション無しでは使えない。
#[tokio::test]
async fn api_keys_and_anonymous_callers_are_refused() {
    let env = test_env().await;
    let (status, issued) =
        issue_api_key(&env.router, &env.admin_token, "feed-probe", &["read"]).await;
    assert!(status.is_success(), "{status}: {issued}");
    let key = issued["key"].as_str().expect("key").to_string();
    assert!(key.starts_with("bh_"), "{key}");
    let (status, _) = feed(&env, &key, "").await;
    assert!(
        status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN,
        "{status}"
    );
    let (status, _) = feed(&env, "not-a-session", "").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// 暫定の failed で入れた書き込みを読んだ後に結果を確定させると、続きの位置から
/// 同じ id の確定した行が届く。
#[tokio::test]
async fn a_settled_write_comes_back_with_the_same_id() {
    let env = test_env().await;
    let (_, first) = feed(&env, &env.admin_token, "").await;
    let write_audit = WriteAuditService::new(env.pool.clone());
    let id = write_audit.insert_pending(&write_row()).await.unwrap();

    let (status, pending) = feed(
        &env,
        &env.admin_token,
        &format!("?cursor={}", cursor(&first)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{pending}");
    let writes: Vec<&Value> = entries(&pending)
        .iter()
        .filter(|entry| entry["source"] == "write")
        .collect();
    assert_eq!(writes.len(), 1, "{pending}");
    assert_eq!(writes[0]["id"], id);
    assert_eq!(writes[0]["result"], "failed");

    write_audit
        .set_result(id, WriteAuditResult::Ok, None)
        .await
        .unwrap();
    let (status, settled) = feed(
        &env,
        &env.admin_token,
        &format!("?cursor={}", cursor(&pending)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{settled}");
    let writes: Vec<&Value> = entries(&settled)
        .iter()
        .filter(|entry| entry["source"] == "write")
        .collect();
    assert_eq!(writes.len(), 1, "{settled}");
    assert_eq!(writes[0]["id"], id);
    assert_eq!(writes[0]["key"], format!("w:{id}"));
    assert_eq!(writes[0]["result"], "ok");
    assert!(
        writes[0]["change_seq"].as_i64()
            > pending["entries"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["source"] == "write")
                .unwrap()["change_seq"]
                .as_i64()
    );

    // 変わらなければ、次の回には届かない。
    let (_, quiet) = feed(
        &env,
        &env.admin_token,
        &format!("?cursor={}", cursor(&settled)),
    )
    .await;
    assert!(
        entries(&quiet)
            .iter()
            .all(|entry| entry["source"] != "write"),
        "{quiet}"
    );
}

/// 保留から元の古い時刻のまま後で入った `audit_log` の行も、続きの位置から取る。
#[tokio::test]
async fn a_late_audit_row_with_an_old_time_is_not_missed() {
    let env = test_env().await;
    let (_, first) = feed(&env, &env.admin_token, "").await;
    let late = audit_row(
        &env,
        "2000-01-01 00:00:00",
        "mcp",
        "update",
        "1",
        json!({ "spooled": true }),
    )
    .await;
    let (status, next) = feed(
        &env,
        &env.admin_token,
        &format!("?cursor={}", cursor(&first)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{next}");
    let entry = entries(&next)
        .iter()
        .find(|entry| entry["key"] == format!("a:{late}"))
        .unwrap_or_else(|| panic!("late row missing: {next}"));
    assert_eq!(entry["ts_ms"], 946_684_800_000_i64);
    assert_eq!(entry["source"], "operation");
}

/// 上限に当たると `more: true` で、続きから残りを取れる。不正な続きの位置は 400。
#[tokio::test]
async fn the_limit_sets_more_and_the_rest_follows_and_bad_cursors_are_refused() {
    let env = test_env().await;
    let (_, first) = feed(&env, &env.viewer_token, "").await;
    let mut inserted = Vec::new();
    for n in 0..5 {
        inserted
            .push(collect_event(&env, 1_700_000_000_000 + n, "plc_reconnected", "conn:1").await);
    }
    let mut seen = Vec::new();
    let mut position = cursor(&first);
    let mut mores = Vec::new();
    for _ in 0..4 {
        let (status, page) = feed(
            &env,
            &env.viewer_token,
            &format!("?cursor={position}&limit=2"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{page}");
        assert!(entries(&page).len() <= 2, "{page}");
        seen.extend(
            entries(&page)
                .iter()
                .map(|entry| entry["id"].as_i64().unwrap()),
        );
        mores.push(page["more"].as_bool().unwrap());
        position = cursor(&page);
    }
    seen.sort_unstable();
    assert_eq!(seen, inserted);
    assert_eq!(mores, vec![true, true, false, false]);

    // 初回も limit 件に切る。
    let (_, newest) = feed(&env, &env.viewer_token, "?limit=2").await;
    assert_eq!(entries(&newest).len(), 2, "{newest}");
    assert_eq!(newest["more"], false);

    for bad in ["garbage", "c1.a1", "c1.a1.w1.x1", "c-1.a0.w0"] {
        let (status, body) = feed(&env, &env.admin_token, &format!("?cursor={bad}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
    }
}
