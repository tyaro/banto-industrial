//! R2a-2c の完了の確かめ（docs/banto-hub-ingest-design.md §11 の R2a-2c の行）:
//! `POST /api/v1/ingest/{id}/values` を本物のルーター（`api_router`）越しに叩く。
//!
//! - §6 の応答の表の総当たり（401・403 の 3 種・404 の 2 種・409 の 2 種・400・413・
//!   422・429・200 の件ごとの結果）と、その監査の列。
//! - 改名の後も同じキーと URL で送れる。削除して同名で作り直した接続には古いキーで
//!   送れない。
//! - 上限の境界値（1,000 件・1 MiB・メッセージと件数のバケット）。
//! - 監査の行（`denied`・`ingest_unknown_key` の抑制・`ingest_limit_exceeded` の端・
//!   値を載せない）。照合できたキーの `denied` の間引き（キーごとに 1 時間に 100 行、
//!   窓が明けた後の成功した要求で抑えた数を 1 行、2026-10-21 オーナー決定）。
//!
//! 時計は `ManualClock`（バケットの補充と未知のキーの 1 時間の窓を決定的にする）。
//! 収集は開始しない（`IngestHub::apply` は停止中も受ける、§4.5 Q11）。

use super::super::*;
use super::common::*;
use axum::body::Body;
use axum::http::Request as HttpRequest;
use banto_tstore::ManualClock;
use serde_json::{json, Value};
use tower::ServiceExt;

const MIB: usize = 1024 * 1024;

fn values_path(id: i64) -> String {
    format!("/api/v1/ingest/{id}/values")
}

struct Sent {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Value,
}

async fn send(router: &Router, path: &str, token: Option<&str>, body: Vec<u8>) -> Sent {
    let mut request = HttpRequest::post(path).header("content-type", "application/json");
    if let Some(token) = token {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    let response = router
        .clone()
        .oneshot(request.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    Sent {
        status,
        headers,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    }
}

async fn send_json(router: &Router, id: i64, token: &str, body: Value) -> Sent {
    send(
        router,
        &values_path(id),
        Some(token),
        body.to_string().into_bytes(),
    )
    .await
}

/// `push` 接続（`enabled`）とグループ 1 つ・タグ `line1.temp`（f32）と `line1.run`（bit）。
async fn push_connection(env: &TestEnv, name: &str, enabled: bool) -> i64 {
    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "name": name, "protocol": "push", "host": "", "port": 0, "enabled": enabled }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");
    let id = conn["id"].as_i64().unwrap();
    let (status, group) = admin_post(
        &env.router,
        "/api/collection-groups",
        &env.admin_token,
        json!({ "name": "g", "plcConnectionId": id, "periodMs": 1000 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{group:?}");
    for (tag, key, data_type) in [("temp", "line1.temp", "f32"), ("run", "line1.run", "bit")] {
        let (status, body) = admin_post(
            &env.router,
            "/api/tags",
            &env.admin_token,
            json!({ "name": tag, "collectionGroupId": group["id"], "address": key,
                    "dataType": data_type, "enabled": true }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
    }
    id
}

/// タグの無い `push` 接続（削除・作り直しのテスト用）。
async fn bare_push_connection(env: &TestEnv, name: &str) -> i64 {
    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "name": name, "protocol": "push", "host": "", "port": 0 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");
    conn["id"].as_i64().unwrap()
}

async fn key_with(env: &TestEnv, name: &str, scopes: &[&str]) -> (i64, String) {
    let (status, issued) = issue_api_key(&env.router, &env.admin_token, name, scopes).await;
    assert_eq!(status, StatusCode::CREATED, "{issued:?}");
    (
        issued["id"].as_i64().unwrap(),
        issued["key"].as_str().unwrap().to_string(),
    )
}

/// 取り込みの監査の行（`origin = 'ingest'`）: (action, resource, entity_id, detail)。
async fn ingest_audit_rows(env: &TestEnv) -> Vec<(String, String, Option<String>, Value)> {
    let rows: Vec<(String, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT action, resource, entity_id, detail FROM audit_log WHERE origin = 'ingest' ORDER BY id",
    )
    .fetch_all(&env.pool)
    .await
    .unwrap();
    rows.into_iter()
        .map(|(action, resource, entity_id, detail)| {
            let detail = detail
                .map(|text| serde_json::from_str(&text).unwrap())
                .unwrap_or(Value::Null);
            (action, resource, entity_id, detail)
        })
        .collect()
}

fn one_item() -> Value {
    json!({ "items": [ { "key": "line1.temp", "v": 12.5 } ] })
}

/// §6 の応答の表の総当たり（監査の列も）。どの拒否も値を反映しない。
#[tokio::test]
async fn the_ingest_response_table() {
    let clock = Arc::new(ManualClock::new(1_000_000, 0));
    let env = test_env_with_clock(clock).await;
    let a = push_connection(&env, "Bridge1", true).await;
    let disabled = push_connection(&env, "Off", false).await;
    let (status, plc) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "name": "plc", "protocol": "modbus-tcp", "host": "127.0.0.1", "port": 502 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{plc:?}");
    let plc = plc["id"].as_i64().unwrap();
    let gone = bare_push_connection(&env, "Gone").await;

    let scope = |id: i64| format!("ingest:#{id}");
    let (_, key_a) = key_with(&env, "a", &[&scope(a)]).await;
    let (_, key_read_admin) = key_with(&env, "ra", &["read", "admin"]).await;
    let (_, key_off) = key_with(&env, "off", &[&scope(disabled)]).await;
    let (_, key_plc) = key_with(&env, "plc", &[&scope(plc)]).await;
    let (_, key_gone) = key_with(&env, "gone", &[&scope(gone)]).await;
    let (revoked_id, key_revoked) = key_with(&env, "revoked", &[&scope(a)]).await;
    let (tripped_id, key_tripped) = key_with(&env, "tripped", &[&scope(a)]).await;
    env.api_keys.revoke(revoked_id).await.unwrap();
    env.api_keys.trip(tripped_id).await.unwrap();
    let (status, _) = admin_delete(
        &env.router,
        &format!("/api/plc-connections/{gone}"),
        &env.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let audit_before = ingest_audit_rows(&env).await.len();
    assert_eq!(audit_before, 0);

    let body = one_item().to_string().into_bytes();
    let path_a = values_path(a);
    // (説明, パス, トークン, 状態, error)
    type Case<'a> = (
        &'a str,
        String,
        Option<&'a str>,
        StatusCode,
        Option<&'a str>,
    );
    let cases: Vec<Case> = vec![
        (
            "トークンなし",
            path_a.clone(),
            None,
            StatusCode::UNAUTHORIZED,
            None,
        ),
        (
            "存在しないキー",
            path_a.clone(),
            Some("bh_0000000000000000000000000000000000000000"),
            StatusCode::UNAUTHORIZED,
            None,
        ),
        (
            "失効",
            path_a.clone(),
            Some(&key_revoked),
            StatusCode::UNAUTHORIZED,
            None,
        ),
        (
            "トリップ",
            path_a.clone(),
            Some(&key_tripped),
            StatusCode::FORBIDDEN,
            Some("key_tripped"),
        ),
        (
            "セッション",
            path_a.clone(),
            Some(&env.admin_token),
            StatusCode::FORBIDDEN,
            Some("session_token_cannot_ingest"),
        ),
        (
            "無効なセッション",
            path_a.clone(),
            Some("not-a-session"),
            StatusCode::UNAUTHORIZED,
            None,
        ),
        (
            "パスの形（先頭の 0）",
            format!("/api/v1/ingest/0{a}/values"),
            Some(&key_a),
            StatusCode::NOT_FOUND,
            Some("unknown_connection"),
        ),
        (
            "パスの形（名前）",
            "/api/v1/ingest/Bridge1/values".to_string(),
            Some(&key_a),
            StatusCode::NOT_FOUND,
            Some("unknown_connection"),
        ),
        (
            "スコープなし（read・admin）",
            path_a.clone(),
            Some(&key_read_admin),
            StatusCode::FORBIDDEN,
            Some("missing_ingest_scope"),
        ),
        (
            "スコープなし（別の接続のキー）",
            values_path(disabled),
            Some(&key_a),
            StatusCode::FORBIDDEN,
            Some("missing_ingest_scope"),
        ),
        (
            "スコープなし（削除済みの id でも同じ 403）",
            values_path(gone),
            Some(&key_a),
            StatusCode::FORBIDDEN,
            Some("missing_ingest_scope"),
        ),
        (
            "削除済みの id",
            values_path(gone),
            Some(&key_gone),
            StatusCode::NOT_FOUND,
            Some("unknown_connection"),
        ),
        (
            "push でない",
            values_path(plc),
            Some(&key_plc),
            StatusCode::NOT_FOUND,
            Some("unknown_connection"),
        ),
        (
            "無効",
            values_path(disabled),
            Some(&key_off),
            StatusCode::CONFLICT,
            Some("connection_disabled"),
        ),
    ];
    for (label, path, token, status, error) in &cases {
        let sent = send(&env.router, path, *token, body.clone()).await;
        assert_eq!(sent.status, *status, "{label}: {:?}", sent.body);
        if let Some(error) = error {
            assert_eq!(sent.body["error"], *error, "{label}");
        }
    }
    // 拒否は何も反映していない。
    let info = env.manager.ingest_hub().connection_info(a).unwrap();
    assert_eq!(info.held_values, 0);

    // 監査の列: 失効・トリップ（api_keys）、スコープなし 3 件・削除済み・push でない
    // （ingest）。存在しないキー・セッション・パスの形・無効は記録しない。
    let rows = ingest_audit_rows(&env).await;
    let summary: Vec<(String, String, Option<String>, Value)> = rows
        .iter()
        .map(|(action, resource, entity, detail)| {
            (
                action.clone(),
                resource.clone(),
                entity.clone(),
                detail["reason"].clone(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            (
                "denied".into(),
                "api_keys".into(),
                Some(revoked_id.to_string()),
                json!("revoked")
            ),
            (
                "denied".into(),
                "api_keys".into(),
                Some(tripped_id.to_string()),
                json!("tripped")
            ),
            (
                "denied".into(),
                "ingest".into(),
                Some(a.to_string()),
                json!("missing_ingest_scope")
            ),
            (
                "denied".into(),
                "ingest".into(),
                Some(disabled.to_string()),
                json!("missing_ingest_scope")
            ),
            (
                "denied".into(),
                "ingest".into(),
                Some(gone.to_string()),
                json!("missing_ingest_scope")
            ),
            (
                "denied".into(),
                "ingest".into(),
                Some(gone.to_string()),
                json!("unknown_connection")
            ),
            (
                "denied".into(),
                "ingest".into(),
                Some(plc.to_string()),
                json!("unknown_connection")
            ),
        ],
    );
    // スコープの確認の前の行には接続名の控えを載せない（接続の有無を明かさない）。
    for (_, resource, _, detail) in &rows {
        if resource == "ingest" {
            assert_eq!(detail["connection_name_snapshot"], Value::Null, "{detail}");
            assert_eq!(detail["transport"], "rest");
        }
    }

    // 本文の段（認証を通った後）。
    let invalid = send(&env.router, &path_a, Some(&key_a), b"{not json".to_vec()).await;
    assert_eq!(
        invalid.status,
        StatusCode::BAD_REQUEST,
        "{:?}",
        invalid.body
    );
    assert_eq!(invalid.body["error"], "invalid_message");
    let shape = send_json(&env.router, a, &key_a, json!({ "values": [] })).await;
    assert_eq!(shape.status, StatusCode::BAD_REQUEST);
    let stale = send_json(&env.router, a, &key_a, json!({ "epoch": 99, "items": [] })).await;
    assert_eq!(stale.status, StatusCode::CONFLICT, "{:?}", stale.body);
    assert_eq!(stale.body, json!({ "error": "stale_epoch", "epoch": 1 }));
    let too_big = send(&env.router, &path_a, Some(&key_a), vec![b' '; MIB + 1]).await;
    assert_eq!(too_big.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(too_big.body["error"], "body_too_large");
    let items: Vec<Value> = (0..1_001)
        .map(|_| json!({ "key": "line1.temp", "v": 1 }))
        .collect();
    let too_many = send_json(&env.router, a, &key_a, json!({ "items": items })).await;
    assert_eq!(
        too_many.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{:?}",
        too_many.body
    );
    assert_eq!(too_many.body["error"], "too_many_items");
    assert_eq!(too_many.body["limits"]["max_items"], 1_000);
    assert_eq!(
        env.manager
            .ingest_hub()
            .connection_info(a)
            .unwrap()
            .held_values,
        0
    );

    // 200 と件ごとの結果。同じメッセージの 2 回目のキーは、1 回目が断られていても
    // `duplicate_key`（`IngestHub::apply` はキーの照合を値の検証より先に行う）。
    let ok = send_json(
        &env.router,
        a,
        &key_a,
        json!({ "epoch": 1, "items": [
            { "key": "line1.temp", "v": 12.5, "q": "good", "t": 1_761_000_000_000_i64 },
            { "key": "line1.tmp", "v": 1 },
            { "key": "line1.run", "v": 2 },
            { "key": "line1.temp", "v": 13 },
            { "key": "line1.run", "v": true },
        ] }),
    )
    .await;
    assert_eq!(ok.status, StatusCode::OK, "{:?}", ok.body);
    assert_eq!(
        ok.body,
        json!({
            "accepted": 1,
            "rejected": [
                { "index": 1, "key": "line1.tmp", "code": "unknown_key" },
                { "index": 2, "key": "line1.run", "code": "invalid_value" },
                { "index": 3, "key": "line1.temp", "code": "duplicate_key" },
                { "index": 4, "key": "line1.run", "code": "duplicate_key" },
            ],
            "collecting": false,
            "epoch": 1,
            "stale_timeout_ms": 10_000,
        })
    );
    for (item, code) in [
        (
            json!({ "key": "line1.run", "v": true, "q": "uncertain" }),
            "invalid_quality",
        ),
        (
            json!({ "key": "line1.run", "v": false, "t": "now" }),
            "invalid_time",
        ),
        (json!({ "key": "line1.run", "q": "good" }), "invalid_value"),
    ] {
        let sent = send_json(&env.router, a, &key_a, json!({ "items": [item] })).await;
        assert_eq!(sent.status, StatusCode::OK);
        assert_eq!(sent.body["accepted"], 0);
        assert_eq!(sent.body["rejected"][0]["code"], code);
    }
    let run = send_json(
        &env.router,
        a,
        &key_a,
        json!({ "items": [{ "key": "line1.run", "v": true }] }),
    )
    .await;
    assert_eq!(run.body["accepted"], 1);
    assert_eq!(
        env.manager
            .ingest_hub()
            .connection_info(a)
            .unwrap()
            .held_values,
        2
    );
}

/// 認証の層は本文を読む前に走る: トークンなし・スコープなしの 2 MiB の本文は 413 では
/// なく 401 / 403。本文の上限（1 MiB）はこのルートだけ: 他の `/api/v1/*` の POST は
/// 1 MiB を超える本文でも 413 にならない（axum の既定の 2 MiB のまま）。
#[tokio::test]
async fn the_body_is_not_read_before_authentication_and_the_limit_is_this_route_only() {
    let env = test_env().await;
    let a = push_connection(&env, "Bridge1", true).await;
    let (_, key_read) = key_with(&env, "r", &["read"]).await;
    let big = vec![b' '; 2 * MIB];
    let anonymous = send(&env.router, &values_path(a), None, big.clone()).await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    let unscoped = send(&env.router, &values_path(a), Some(&key_read), big).await;
    assert_eq!(unscoped.status, StatusCode::FORBIDDEN);
    assert_eq!(unscoped.body["error"], "missing_ingest_scope");

    let (_, key_write) = key_with(&env, "w", &["write:Bridge1.g.temp"]).await;
    let mut padded = vec![b' '; MIB + MIB / 2];
    padded.extend_from_slice(br#"{"writes":[]}"#);
    let other = send(
        &env.router,
        "/api/v1/values/batch",
        Some(&key_write),
        padded,
    )
    .await;
    assert_ne!(
        other.status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "{:?}",
        other.body
    );
}

/// 改名しても同じキーと URL で送れる（束縛は id、§5）。削除して同じ名前で作り直した
/// 接続には古いキーで送れない（id は再利用されない、§5.1）。
#[tokio::test]
async fn rename_keeps_the_key_and_url_but_recreation_does_not() {
    let env = test_env().await;
    let a = push_connection(&env, "Bridge1", true).await;
    let (_, key_a) = key_with(&env, "a", &[&format!("ingest:#{a}")]).await;
    assert_eq!(
        send_json(&env.router, a, &key_a, one_item()).await.status,
        StatusCode::OK
    );

    let (status, body) = admin_put(
        &env.router,
        &format!("/api/plc-connections/{a}"),
        &env.admin_token,
        json!({ "name": "Bridge1-renamed", "protocol": "push", "host": "", "port": 0,
                "enabled": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let after_rename = send_json(&env.router, a, &key_a, one_item()).await;
    assert_eq!(
        after_rename.status,
        StatusCode::OK,
        "{:?}",
        after_rename.body
    );

    let old = bare_push_connection(&env, "Again").await;
    let (_, key_old) = key_with(&env, "old", &[&format!("ingest:#{old}")]).await;
    assert_eq!(
        send_json(&env.router, old, &key_old, json!({ "items": [] }))
            .await
            .status,
        StatusCode::OK
    );
    let (status, _) = admin_delete(
        &env.router,
        &format!("/api/plc-connections/{old}"),
        &env.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let new = bare_push_connection(&env, "Again").await;
    assert!(new > old, "id は再利用されない: {old} → {new}");
    let to_new = send_json(&env.router, new, &key_old, json!({ "items": [] })).await;
    assert_eq!(to_new.status, StatusCode::FORBIDDEN);
    assert_eq!(to_new.body["error"], "missing_ingest_scope");
    let to_old = send_json(&env.router, old, &key_old, json!({ "items": [] })).await;
    assert_eq!(to_old.status, StatusCode::NOT_FOUND);
    assert_eq!(to_old.body["error"], "unknown_connection");
}

/// 件数と本文の境界: 1,000 件ちょうどは通り 1,001 件は 422。本文 1 MiB ちょうどは通り、
/// 1 バイト超は 413。
#[tokio::test]
async fn item_count_and_body_size_boundaries() {
    let clock = Arc::new(ManualClock::new(1_000_000, 0));
    let env = test_env_with_clock(clock).await;
    let a = push_connection(&env, "Bridge1", true).await;
    let (_, key) = key_with(&env, "a", &[&format!("ingest:#{a}")]).await;

    let items = |n: usize| -> Value {
        json!({ "items": (0..n).map(|_| json!({ "key": "line1.temp", "v": 1 })).collect::<Vec<_>>() })
    };
    let exact = send_json(&env.router, a, &key, items(1_000)).await;
    assert_eq!(exact.status, StatusCode::OK, "{:?}", exact.body);
    assert_eq!(exact.body["accepted"], 1);
    assert_eq!(exact.body["rejected"].as_array().unwrap().len(), 999);
    let over = send_json(&env.router, a, &key, items(1_001)).await;
    assert_eq!(over.status, StatusCode::UNPROCESSABLE_ENTITY);

    let padded = |len: usize| -> Vec<u8> {
        let body = one_item().to_string();
        let mut bytes = body.into_bytes();
        bytes.resize(len, b' ');
        bytes
    };
    let exact = send(&env.router, &values_path(a), Some(&key), padded(MIB)).await;
    assert_eq!(exact.status, StatusCode::OK, "{:?}", exact.body);
    let over = send(&env.router, &values_path(a), Some(&key), padded(MIB + 1)).await;
    assert_eq!(over.status, StatusCode::PAYLOAD_TOO_LARGE);
}

/// メッセージのバケット: 瞬間の 40 を使い切ると 41 個目は 429（`Retry-After`）。1 秒
/// 後には 20 個が通り、21 個目は 429。超過の始まりと終わりの監査は 1 行ずつ。
#[tokio::test]
async fn message_rate_boundary_and_its_audit_edges() {
    let clock = Arc::new(ManualClock::new(1_000_000, 0));
    let env = test_env_with_clock(clock.clone()).await;
    let a = push_connection(&env, "Bridge1", true).await;
    let (_, key) = key_with(&env, "a", &[&format!("ingest:#{a}")]).await;
    let empty = || json!({ "items": [] });

    for n in 0..40 {
        let sent = send_json(&env.router, a, &key, empty()).await;
        assert_eq!(sent.status, StatusCode::OK, "{n}: {:?}", sent.body);
    }
    for _ in 0..3 {
        let limited = send_json(&env.router, a, &key, empty()).await;
        assert_eq!(
            limited.status,
            StatusCode::TOO_MANY_REQUESTS,
            "{:?}",
            limited.body
        );
        assert_eq!(limited.body["error"], "rate_limited");
        assert_eq!(limited.body["limit"], "messages_per_sec");
        assert_eq!(limited.body["limits"]["messages_per_sec"], 20);
        assert_eq!(limited.headers.get("retry-after").unwrap(), "1");
    }
    clock.advance_ms(1_000);
    for n in 0..20 {
        let sent = send_json(&env.router, a, &key, empty()).await;
        assert_eq!(sent.status, StatusCode::OK, "{n}: {:?}", sent.body);
    }
    let limited = send_json(&env.router, a, &key, empty()).await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);

    let edges: Vec<(String, Value, Value)> = ingest_audit_rows(&env)
        .await
        .into_iter()
        .filter(|(action, ..)| action.starts_with("ingest_limit_"))
        .map(|(action, _, _, detail)| (action, detail["kind"].clone(), detail.clone()))
        .collect();
    let kinds: Vec<(&str, &Value)> = edges.iter().map(|(a, k, _)| (a.as_str(), k)).collect();
    assert_eq!(
        kinds,
        vec![
            ("ingest_limit_exceeded", &json!("messages_per_sec")),
            ("ingest_limit_cleared", &json!("messages_per_sec")),
            ("ingest_limit_exceeded", &json!("messages_per_sec")),
        ]
    );
    assert_eq!(edges[0].2["limit"], 20);
    assert_eq!(edges[1].2["rejected"], 3);
    assert_eq!(edges[0].2["connection_name_snapshot"], "Bridge1");
}

/// 件数のバケット: 1,000 件 × 10 で瞬間の 10,000 を使い切ると 11 個目は 429
/// （`items_per_sec`）。1 秒後には 1,000 件 × 5 が通り、6 個目は 429。
#[tokio::test]
async fn item_rate_boundary() {
    let clock = Arc::new(ManualClock::new(1_000_000, 0));
    let env = test_env_with_clock(clock.clone()).await;
    let a = push_connection(&env, "Bridge1", true).await;
    let (_, key) = key_with(&env, "a", &[&format!("ingest:#{a}")]).await;
    let thousand = json!({ "items": (0..1_000).map(|_| json!({ "key": "line1.temp", "v": 1 })).collect::<Vec<_>>() });

    for n in 0..10 {
        let sent = send_json(&env.router, a, &key, thousand.clone()).await;
        assert_eq!(sent.status, StatusCode::OK, "{n}");
    }
    let limited = send_json(&env.router, a, &key, thousand.clone()).await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(limited.body["limit"], "items_per_sec");
    clock.advance_ms(1_000);
    for n in 0..5 {
        let sent = send_json(&env.router, a, &key, thousand.clone()).await;
        assert_eq!(sent.status, StatusCode::OK, "{n}");
    }
    let limited = send_json(&env.router, a, &key, thousand.clone()).await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(limited.body["limit"], "items_per_sec");
}

/// 未知のキーの監査: (接続, キー) ごとに 1 時間に 1 回、1 時間に 100 キーを超えた分は
/// 数だけを 1 行（窓が明けた後の最初の観測で）。監査の行に値は載らない。
#[tokio::test]
async fn unknown_key_audit_is_throttled_and_carries_no_values() {
    let clock = Arc::new(ManualClock::new(1_000_000, 0));
    let env = test_env_with_clock(clock.clone()).await;
    let a = push_connection(&env, "Bridge1", true).await;
    let (key_id, key) = key_with(&env, "a", &[&format!("ingest:#{a}")]).await;
    let unknown = |range: std::ops::Range<usize>| -> Value {
        json!({ "items": range.map(|n| json!({ "key": format!("nope.{n}"), "v": 98_765.432_1 }))
            .chain([json!({ "key": "line1.temp", "v": 98_765.432_1 })])
            .collect::<Vec<_>>() })
    };

    let first = send_json(&env.router, a, &key, unknown(0..150)).await;
    assert_eq!(first.status, StatusCode::OK);
    assert_eq!(first.body["accepted"], 1);
    let again = send_json(&env.router, a, &key, unknown(0..150)).await;
    assert_eq!(again.status, StatusCode::OK);
    let rows = ingest_audit_rows(&env).await;
    assert_eq!(rows.len(), 100);
    assert!(rows.iter().all(
        |(action, resource, entity, _)| action == "ingest_unknown_key"
            && resource == "ingest"
            && entity.as_deref() == Some(a.to_string().as_str())
    ));
    assert_eq!(rows[0].3["key"], "nope.0");
    assert_eq!(rows[0].3["api_key_id"], key_id);
    assert_eq!(rows[0].3["api_key_name"], "a");

    clock.advance_ms(3_600_000);
    let next_window = send_json(&env.router, a, &key, unknown(0..1)).await;
    assert_eq!(next_window.status, StatusCode::OK);
    let rows = ingest_audit_rows(&env).await;
    assert_eq!(rows.len(), 102);
    assert_eq!(rows[100].3["suppressed"], 100);
    assert_eq!(rows[101].3["key"], "nope.0");

    for (_, _, _, detail) in &rows {
        let text = detail.to_string();
        assert!(!text.contains("98765"), "値が監査に載った: {text}");
        assert!(detail.get("v").is_none() && detail.get("items").is_none());
    }
}

/// 抑えた組の数の行は、窓明けの観測をした要求のキー・接続元を載せない（接続ごとの間引き
/// なので、抑えたのはキー A の組でも、窓明けに観測するのはキー B の要求でありうる）。
/// B の通常の要求の応答は変わらない。
#[tokio::test]
async fn suppressed_limit_edges_do_not_carry_the_observing_key() {
    let clock = Arc::new(ManualClock::new(1_000_000, 0));
    let env = test_env_with_clock(clock.clone()).await;
    let a = push_connection(&env, "Bridge1", true).await;
    let (key_a_id, key_a) = key_with(&env, "key-a", &[&format!("ingest:#{a}")]).await;
    let (key_b_id, key_b) = key_with(&env, "key-b", &[&format!("ingest:#{a}")]).await;
    let empty = || json!({ "items": [] });

    // キー A が 21 組の超過（バケットを使い切って 429 → 1 秒後に通って解消）を起こす。
    // 20 組は個別に記録され、21 組目だけ抑えられる。
    for n in 0..21 {
        loop {
            let sent = send_json(&env.router, a, &key_a, empty()).await;
            if sent.status == StatusCode::TOO_MANY_REQUESTS {
                break;
            }
            assert_eq!(sent.status, StatusCode::OK, "{n}: {:?}", sent.body);
        }
        clock.advance_ms(1_000);
        let sent = send_json(&env.router, a, &key_a, empty()).await;
        assert_eq!(sent.status, StatusCode::OK, "{n}: {:?}", sent.body);
    }
    let edge_rows = || async {
        ingest_audit_rows(&env)
            .await
            .into_iter()
            .filter(|(action, ..)| action.starts_with("ingest_limit_"))
            .collect::<Vec<_>>()
    };
    let before = edge_rows().await;
    assert_eq!(before.len(), 40);
    assert!(before
        .iter()
        .all(|(_, _, _, d)| d["api_key_id"] == key_a_id));

    // 窓が明けた後、キー B の通常の要求が抑えた数を出す。
    clock.advance_ms(3_600_000);
    let sent = send_json(&env.router, a, &key_b, empty()).await;
    assert_eq!(sent.status, StatusCode::OK, "{:?}", sent.body);
    let after = edge_rows().await;
    assert_eq!(after.len(), 41);
    let (action, resource, entity, detail) = &after[40];
    assert_eq!(action, "ingest_limit_exceeded");
    assert_eq!(resource, "ingest");
    assert_eq!(entity.as_deref(), Some(a.to_string().as_str()));
    assert_eq!(detail["suppressed"], 1);
    assert_eq!(detail["connection_id"], a);
    assert_eq!(detail["connection_name_snapshot"], "Bridge1");
    assert!(detail.get("window_start_ms").is_some());
    for gone in ["api_key_id", "api_key_name", "peer", "transport"] {
        assert!(detail.get(gone).is_none(), "{gone}: {detail}");
    }
    assert_ne!(detail.get("api_key_id"), Some(&json!(key_b_id)));
}

/// 照合できたキーの `denied` の間引き（2026-10-21 オーナー決定、§8）: スコープの無い
/// 接続へ送り続けるキーの行は 1 時間に 100 行まで。窓が明けた後、そのキーの**成功した**
/// 要求（拒否でない観測）で抑えた数を 1 行出す（1 回だけ）。
#[tokio::test]
async fn key_denials_are_capped_per_key_and_flushed_by_a_later_success() {
    let clock = Arc::new(ManualClock::new(1_000_000, 0));
    let env = test_env_with_clock(clock.clone()).await;
    let a = push_connection(&env, "Bridge1", true).await;
    let b = push_connection(&env, "Bridge2", true).await;
    let (key_id, key) = key_with(&env, "only-b", &[&format!("ingest:#{b}")]).await;

    for n in 0..105 {
        let sent = send_json(&env.router, a, &key, one_item()).await;
        assert_eq!(sent.status, StatusCode::FORBIDDEN, "{n}");
        assert_eq!(sent.body["error"], "missing_ingest_scope");
    }
    let rows = ingest_audit_rows(&env).await;
    assert_eq!(rows.len(), 100);
    assert!(rows
        .iter()
        .all(|(action, resource, entity, detail)| action == "denied"
            && resource == "ingest"
            && entity.as_deref() == Some(a.to_string().as_str())
            && detail["reason"] == "missing_ingest_scope"
            && detail["api_key_id"] == key_id));

    // 窓の中の成功では出さない。
    let ok = send_json(&env.router, b, &key, one_item()).await;
    assert_eq!(ok.status, StatusCode::OK, "{:?}", ok.body);
    assert_eq!(ingest_audit_rows(&env).await.len(), 100);

    clock.advance_ms(3_600_000);
    let ok = send_json(&env.router, b, &key, one_item()).await;
    assert_eq!(ok.status, StatusCode::OK, "{:?}", ok.body);
    let rows = ingest_audit_rows(&env).await;
    assert_eq!(rows.len(), 101);
    let (action, resource, entity, detail) = &rows[100];
    assert_eq!(action, "denied");
    assert_eq!(resource, "api_keys");
    assert_eq!(entity.as_deref(), Some(key_id.to_string().as_str()));
    assert_eq!(
        detail,
        &json!({ "reason": "suppressed", "api_key_id": key_id, "api_key_name": "only-b",
                 "suppressed": 5, "window_start_ms": 1_000_000 })
    );

    // 二度は出さない。新しい窓ではまた記録する。
    let ok = send_json(&env.router, b, &key, one_item()).await;
    assert_eq!(ok.status, StatusCode::OK);
    assert_eq!(ingest_audit_rows(&env).await.len(), 101);
    let denied = send_json(&env.router, a, &key, one_item()).await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    assert_eq!(ingest_audit_rows(&env).await.len(), 102);
    assert_eq!(env.manager.ingest_hub().tracked_key_denials(), 1);
}
