//! R2a-2d の完了の確かめ（docs/banto-hub-ingest-design.md §11 の R2a-2d の行）:
//! `GET /api/v1/ingest/{id}/stream` を本物のルーター（`api_router`）を TCP で立てて、
//! `tokio-tungstenite` のクライアントで叩く。
//!
//! - 認証の前に upgrade しない（401 / 403 / 404 / 409 は HTTP の応答で、101 にならない）。
//!   `Sec-WebSocket-Protocol` からはトークンを取り出さない。
//! - 値が収集に届く（`welcome` → `values` → `ack`、`ping` → `pong`、`error`）。
//! - 2 本目で古い方が 1008 `superseded`。キーの失効で 1008。接続の改名では閉じない・
//!   無効化で `connection_changed`・削除で `connection_removed`。
//! - 256 KiB ちょうどは通り、1 バイト超で 1009。
//! - 収集の停止・開始とブリッジの設定の変更で `state`（epoch が進む）が届く。
//! - `ack` を読まない送り手で 1013。
//! - 切断後 `stale_timeout_ms` までは Live。
//!
//! 待ちはすべて上限つき（`RECV_BOUND`）。固定の sleep は「読まない送り手」の再現で、
//! サーバーが詰まるまでの間を取るときだけ使う。

use super::super::*;
use super::common::*;
use std::time::Duration;

use banto_collect::{ExternalLink, ExternalSource, Quality};
use banto_tags::{PlcConnectionInput, PlcConnectionService};
use banto_tstore::ManualClock;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::ingest::ws::IngestWsTuning;
use crate::stream::StreamRevalidationTiming;

type Client =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// 1 つの応答・close を待つ上限（ハングの検出用）。
const RECV_BOUND: Duration = Duration::from_secs(10);

struct WsEnv {
    env: TestEnv,
    server: banto_server::RunningServer,
}

impl WsEnv {
    fn port(&self) -> u16 {
        self.server.local_addr().port()
    }

    fn url(&self, raw_id: &str) -> String {
        format!(
            "ws://127.0.0.1:{}/api/v1/ingest/{raw_id}/stream",
            self.port()
        )
    }

    /// `Authorization: Bearer {token}`（`token` があれば）と `extra` のヘッダで開く。
    async fn try_connect(
        &self,
        raw_id: &str,
        token: Option<&str>,
        extra: &[(&str, &str)],
    ) -> Result<Client, tokio_tungstenite::tungstenite::Error> {
        let mut request = self.url(raw_id).into_client_request().unwrap();
        if let Some(token) = token {
            request
                .headers_mut()
                .insert("Authorization", format!("Bearer {token}").parse().unwrap());
        }
        for (name, value) in extra {
            request.headers_mut().insert(
                axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        tokio_tungstenite::connect_async(request)
            .await
            .map(|(ws, _)| ws)
    }

    /// 開いて `welcome` を読む。
    async fn open(&self, id: i64, token: &str) -> (Client, Value) {
        let mut ws = self
            .try_connect(&id.to_string(), Some(token), &[])
            .await
            .expect("ws handshake");
        let welcome = recv_json(&mut ws).await;
        assert_eq!(welcome["op"], "welcome", "{welcome}");
        (ws, welcome)
    }
}

async fn ws_env(clock: Arc<dyn banto_tstore::Clock>, tuning: Option<IngestWsTuning>) -> WsEnv {
    let env = test_env_with_clock(clock).await;
    // 再検証を短くする（`/api/v1/stream` のテストと同じ差し込み方）。
    let mut router = env
        .router
        .clone()
        .layer(axum::Extension(StreamRevalidationTiming {
            interval: Duration::from_millis(100),
            timeout: Duration::from_secs(1),
        }));
    if let Some(tuning) = tuning {
        router = router.layer(axum::Extension(tuning));
    }
    let server = banto_server::start(
        banto_server::ServerConfig {
            bind: "127.0.0.1".to_string(),
            port: 0,
        },
        router,
    )
    .await
    .expect("server should start");
    WsEnv { env, server }
}

/// 次のテキストのメッセージ（JSON）。
async fn recv_json(ws: &mut Client) -> Value {
    tokio::time::timeout(RECV_BOUND, async {
        loop {
            match ws.next().await {
                Some(Ok(WsMessage::Text(text))) => return serde_json::from_str(&text).unwrap(),
                Some(Ok(WsMessage::Ping(_) | WsMessage::Pong(_))) => continue,
                other => panic!("stream ended while waiting for a message: {other:?}"),
            }
        }
    })
    .await
    .expect("a message within the bound")
}

/// close フレームを待って (code, reason)。途中のテキストは読み捨てる。
async fn wait_close(ws: &mut Client) -> (u16, String) {
    tokio::time::timeout(RECV_BOUND, async {
        loop {
            match ws.next().await {
                Some(Ok(WsMessage::Close(Some(frame)))) => {
                    return (u16::from(frame.code), frame.reason.to_string())
                }
                Some(Ok(WsMessage::Close(None))) | None | Some(Err(_)) => {
                    panic!("stream ended without a close frame")
                }
                Some(Ok(_)) => continue,
            }
        }
    })
    .await
    .expect("close frame within the bound")
}

async fn send_text(ws: &mut Client, text: impl Into<String>) {
    ws.send(WsMessage::Text(text.into().into()))
        .await
        .expect("send");
}

/// `ping` → `pong`（ループが止まっていない）。
async fn assert_pong(ws: &mut Client, seq: i64) {
    send_text(ws, json!({ "op": "ping", "seq": seq }).to_string()).await;
    let pong = recv_json(ws).await;
    assert_eq!(pong, json!({ "op": "pong", "seq": seq }));
}

/// 上限つきの条件待ち。
async fn wait_until(mut condition: impl FnMut() -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + RECV_BOUND;
    while tokio::time::Instant::now() < deadline {
        if condition() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    condition()
}

/// `push` 接続とグループ（周期 100 ms）・タグ `line1.temp`（f32）。(接続 id, タグ id)。
async fn push_connection(env: &TestEnv, name: &str) -> (i64, i64) {
    let (status, conn) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "name": name, "protocol": "push", "host": "", "port": 0, "enabled": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");
    let id = conn["id"].as_i64().unwrap();
    let tag = push_tag(env, id, "line1.temp").await;
    (id, tag)
}

/// 接続 `conn_id` の下にグループとタグ（キー `key`、f32）を 1 つ。タグ id。
async fn push_tag(env: &TestEnv, conn_id: i64, key: &str) -> i64 {
    let (status, group) = admin_post(
        &env.router,
        "/api/collection-groups",
        &env.admin_token,
        json!({ "name": format!("g-{key}"), "plcConnectionId": conn_id, "periodMs": 100 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{group:?}");
    let (status, tag) = admin_post(
        &env.router,
        "/api/tags",
        &env.admin_token,
        json!({ "name": format!("t-{key}"), "collectionGroupId": group["id"], "address": key,
                "dataType": "f32", "enabled": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{tag:?}");
    tag["id"].as_i64().unwrap()
}

/// タグの無い `push` 接続。
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

async fn ingest_key(env: &TestEnv, name: &str, conn_id: i64) -> (i64, String) {
    let scope = format!("ingest:#{conn_id}");
    let (status, issued) = issue_api_key(&env.router, &env.admin_token, name, &[&scope]).await;
    assert_eq!(status, StatusCode::CREATED, "{issued:?}");
    (
        issued["id"].as_i64().unwrap(),
        issued["key"].as_str().unwrap().to_string(),
    )
}

/// 取り込みの監査の行（`origin = 'ingest'`、`action` が `ingest_` で始まるもの）:
/// (action, result, entity_id, detail)。
async fn session_audit_rows(env: &TestEnv) -> Vec<(String, String, Option<String>, Value)> {
    let rows: Vec<(String, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT action, result, entity_id, detail FROM audit_log \
         WHERE origin = 'ingest' AND action IN ('ingest_connected', 'ingest_disconnected') \
         ORDER BY id",
    )
    .fetch_all(&env.pool)
    .await
    .unwrap();
    rows.into_iter()
        .map(|(action, result, entity_id, detail)| {
            let detail = detail
                .map(|text| serde_json::from_str(&text).unwrap())
                .unwrap_or(Value::Null);
            (action, result, entity_id, detail)
        })
        .collect()
}

/// `ingest_disconnected` の行が `count` 本になるまで待つ（サーバー側の後片付けの終わり）。
async fn wait_disconnected_rows(env: &TestEnv, count: usize) -> Vec<Value> {
    let deadline = tokio::time::Instant::now() + RECV_BOUND;
    loop {
        let rows: Vec<Value> = session_audit_rows(env)
            .await
            .into_iter()
            .filter(|(action, ..)| action == "ingest_disconnected")
            .map(|(_, result, _, detail)| {
                assert_eq!(result, "ok");
                detail
            })
            .collect();
        if rows.len() >= count {
            return rows;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "ingest_disconnected の行が {count} 本になりません: {rows:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 読み出しの統一点で見たタグの値と品質。
fn current(env: &TestEnv, tag_id: i64) -> (Option<f64>, Quality) {
    let map = env.manager.tag_map();
    let entry = map
        .iter()
        .find(|entry| entry.ids.2 == tag_id)
        .expect("tag in catalog");
    let view = crate::hub::read_current_view(
        entry,
        env.manager.current_values().as_ref(),
        &env.manager.server_store(),
        env.manager.clock().now_ms(),
    );
    (view.value, view.quality)
}

fn http_status(err: &tokio_tungstenite::tungstenite::Error) -> Option<u16> {
    match err {
        tokio_tungstenite::tungstenite::Error::Http(response) => Some(response.status().as_u16()),
        _ => None,
    }
}

/// §6・§11 の監査 (2): 認証・スコープ・接続の確認を通らない要求は upgrade されず、HTTP の
/// 応答で断られる。`Sec-WebSocket-Protocol` の `bearer, <key>` からはトークンを取り出さない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_upgrade_happens_only_after_auth() {
    let ws = ws_env(Arc::new(banto_tstore::SystemClock), None).await;
    let env = &ws.env;
    let (a, _) = push_connection(env, "Bridge1").await;
    let (other, _) = push_connection(env, "Other").await;
    let (status, off) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "name": "Off", "protocol": "push", "host": "", "port": 0, "enabled": false }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{off:?}");
    let off = off["id"].as_i64().unwrap();
    let (_, key_a) = ingest_key(env, "a", a).await;
    let (_, key_off) = ingest_key(env, "off", off).await;
    let (status, read_admin) =
        issue_api_key(&env.router, &env.admin_token, "ra", &["read", "admin"]).await;
    assert_eq!(status, StatusCode::CREATED);
    let key_read_admin = read_admin["key"].as_str().unwrap().to_string();
    let protocol_header = format!("bearer, {key_a}");

    let a_id = a.to_string();
    let other_id = other.to_string();
    let off_id = off.to_string();
    let leading_zero = format!("0{a}");
    // (説明, パス, トークン, 追加のヘッダ, 状態)
    type Case<'a> = (
        &'a str,
        &'a str,
        Option<&'a str>,
        Vec<(&'a str, &'a str)>,
        u16,
    );
    let cases: Vec<Case> = vec![
        ("トークンなし", &a_id, None, vec![], 401),
        (
            "Sec-WebSocket-Protocol のトークン",
            &a_id,
            None,
            vec![("Sec-WebSocket-Protocol", protocol_header.as_str())],
            401,
        ),
        (
            "存在しないキー",
            &a_id,
            Some("bh_0000000000000000000000000000000000000000"),
            vec![],
            401,
        ),
        (
            "セッション",
            &a_id,
            Some(env.admin_token.as_str()),
            vec![],
            403,
        ),
        (
            "read・admin のキー",
            &a_id,
            Some(key_read_admin.as_str()),
            vec![],
            403,
        ),
        (
            "別の接続のキー",
            &other_id,
            Some(key_a.as_str()),
            vec![],
            403,
        ),
        ("パスの形", &leading_zero, Some(key_a.as_str()), vec![], 404),
        ("無効な接続", &off_id, Some(key_off.as_str()), vec![], 409),
    ];
    for (label, raw_id, token, extra, expected) in cases {
        let err = match ws.try_connect(raw_id, token, &extra).await {
            Ok(_) => panic!("{label}: upgrade された"),
            Err(err) => err,
        };
        assert_eq!(http_status(&err), Some(expected), "{label}: {err:?}");
    }
    assert!(session_audit_rows(env).await.is_empty());
    assert!(!env.manager.ingest_hub().has_ws_session(a));

    // 正しいキーなら開ける。
    let (mut client, welcome) = ws.open(a, &key_a).await;
    assert_eq!(welcome["connection_id"], a);
    client.close(None).await.unwrap();
}

/// 値が収集に届く: `welcome` → `values` → `ack`（件ごとの結果）、収集のキャッシュに Good。
/// `ping` → `pong`、形の誤り・バイナリ・知らない `op`・件数の超過は `error`（閉じない）。
/// `status` は応答なしで控える。送り手が閉じたら `client_closed` と受け付けた件数を監査。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn values_reach_the_collection_and_are_acknowledged() {
    let ws = ws_env(Arc::new(banto_tstore::SystemClock), None).await;
    let env = &ws.env;
    let (a, temp) = push_connection(env, "Bridge1").await;
    let (_, key_a) = ingest_key(env, "a", a).await;
    let (status, body) = admin_post(
        &env.router,
        "/api/collection/start",
        &env.admin_token,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let hub = env.manager.ingest_hub();
    assert!(wait_until(|| hub.collecting()).await);

    let (mut client, welcome) = ws.open(a, &key_a).await;
    assert_eq!(
        welcome,
        json!({
            "op": "welcome", "connection_id": a, "connection": "Bridge1", "collecting": true,
            "epoch": 1, "stale_timeout_ms": 10000,
            "limits": { "max_items": 1000, "max_message_bytes": 262144,
                        "messages_per_sec": 20, "items_per_sec": 5000 },
        })
    );
    assert!(hub.has_ws_session(a));

    send_text(
        &mut client,
        json!({ "op": "values", "seq": 17, "items": [
            { "key": "line1.temp", "v": 12.5 },
            { "key": "line1.tmp", "v": 1 },
        ] })
        .to_string(),
    )
    .await;
    assert_eq!(
        recv_json(&mut client).await,
        json!({ "op": "ack", "seq": 17, "accepted": 1,
                "rejected": [ { "index": 1, "key": "line1.tmp", "code": "unknown_key" } ] })
    );
    assert!(
        wait_until(|| current(env, temp) == (Some(12.5), Quality::Good)).await,
        "{:?}",
        current(env, temp)
    );

    assert_pong(&mut client, 18).await;
    // 形の誤り・バイナリ・知らない op・seq が整数でない・件数の超過: error で閉じない。
    send_text(&mut client, "not json").await;
    let error = recv_json(&mut client).await;
    assert_eq!(
        (error["op"].as_str(), error["code"].as_str()),
        (Some("error"), Some("invalid_message"))
    );
    client
        .send(WsMessage::Binary(vec![1, 2, 3].into()))
        .await
        .unwrap();
    assert_eq!(recv_json(&mut client).await["code"], "invalid_message");
    send_text(
        &mut client,
        json!({ "op": "subscribe", "seq": 3 }).to_string(),
    )
    .await;
    let error = recv_json(&mut client).await;
    assert_eq!(
        (error["code"].as_str(), error["seq"].as_i64()),
        (Some("invalid_message"), Some(3))
    );
    send_text(&mut client, json!({ "op": "ping", "seq": "x" }).to_string()).await;
    assert_eq!(recv_json(&mut client).await["code"], "invalid_message");
    send_text(
        &mut client,
        json!({ "op": "values", "seq": 4, "items": {} }).to_string(),
    )
    .await;
    let error = recv_json(&mut client).await;
    assert_eq!(
        (error["code"].as_str(), error["seq"].as_i64()),
        (Some("invalid_message"), Some(4))
    );
    let many: Vec<Value> = (0..1001)
        .map(|n| json!({ "key": format!("k{n}"), "v": 1 }))
        .collect();
    send_text(
        &mut client,
        json!({ "op": "values", "seq": 5, "items": many }).to_string(),
    )
    .await;
    let error = recv_json(&mut client).await;
    assert_eq!(
        (error["code"].as_str(), error["seq"].as_i64()),
        (Some("too_many_items"), Some(5))
    );

    // status: 応答なし。制御文字を落として控える。
    send_text(
        &mut client,
        json!({ "op": "status", "detail": "line1: 失敗\n(再試行)" }).to_string(),
    )
    .await;
    assert_pong(&mut client, 19).await;
    assert_eq!(hub.sender_status(a).as_deref(), Some("line1: 失敗(再試行)"));

    client.close(None).await.unwrap();
    let rows = wait_disconnected_rows(env, 1).await;
    assert_eq!(rows[0]["reason"], "client_closed");
    assert_eq!(rows[0]["accepted"], 1);
    assert_eq!(rows[0]["transport"], "ws");
    assert_eq!(rows[0]["connection_name_snapshot"], "Bridge1");
    assert!(wait_until(|| !hub.has_ws_session(a)).await);
    let audit = session_audit_rows(env).await;
    assert_eq!(audit[0].0, "ingest_connected");
    assert_eq!(audit[0].1, "ok");
    assert_eq!(audit[0].2.as_deref(), Some(a.to_string().as_str()));
    // 監査に値は載らない。
    for (_, _, _, detail) in &audit {
        let text = detail.to_string();
        assert!(!text.contains("12.5"), "{text}");
    }
}

/// 1 接続に 1 本: 2 本目が開くと古い方が 1008 `superseded`。新しい方はそのまま使え、
/// 古い方の後片付けは新しい方の登録を消さない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_socket_supersedes_the_first() {
    let ws = ws_env(Arc::new(banto_tstore::SystemClock), None).await;
    let env = &ws.env;
    let (a, _) = push_connection(env, "Bridge1").await;
    let (_, key_a) = ingest_key(env, "a", a).await;
    let hub = env.manager.ingest_hub();

    let (mut first, _) = ws.open(a, &key_a).await;
    let (mut second, _) = ws.open(a, &key_a).await;
    assert_eq!(
        wait_close(&mut first).await,
        (1008, "superseded".to_string())
    );
    let rows = wait_disconnected_rows(env, 1).await;
    assert_eq!(rows[0]["reason"], "superseded");

    // 古い方が片付いた後も、新しい方は登録されたまま使える。
    assert!(hub.has_ws_session(a));
    assert_pong(&mut second, 1).await;
    send_text(
        &mut second,
        json!({ "op": "values", "seq": 2, "items": [ { "key": "line1.temp", "v": 3 } ] })
            .to_string(),
    )
    .await;
    assert_eq!(recv_json(&mut second).await["accepted"], 1);

    // 3 本目でも同じ（2 本目が閉じる）。
    let (mut third, _) = ws.open(a, &key_a).await;
    assert_eq!(
        wait_close(&mut second).await,
        (1008, "superseded".to_string())
    );
    assert_pong(&mut third, 3).await;
    third.close(None).await.unwrap();
    wait_disconnected_rows(env, 3).await;
    assert!(wait_until(|| !hub.has_ws_session(a)).await);
}

/// 再検証: 開いている WS のキーを失効させると 1008 `api_key_revoked`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revoking_the_key_closes_the_socket() {
    let ws = ws_env(Arc::new(banto_tstore::SystemClock), None).await;
    let env = &ws.env;
    let (a, _) = push_connection(env, "Bridge1").await;
    let (key_id, key_a) = ingest_key(env, "a", a).await;

    let (mut client, _) = ws.open(a, &key_a).await;
    assert_pong(&mut client, 1).await;
    env.api_keys.revoke(key_id).await.unwrap();
    assert_eq!(
        wait_close(&mut client).await,
        (1008, "api_key_revoked".to_string())
    );
    let rows = wait_disconnected_rows(env, 1).await;
    assert_eq!(rows[0]["reason"], "revoked");
    assert_eq!(rows[0]["revoked_reason"], "api_key_revoked");
    assert!(!env.manager.ingest_hub().has_ws_session(a));
}

/// 改名では閉じない（束縛は id）。無効化で 1008 `connection_changed`、プロトコルの
/// 変更（`push` でなくなる）も `connection_changed`、削除で 1008 `connection_removed`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rename_keeps_the_socket_while_disable_and_delete_close_it() {
    let ws = ws_env(Arc::new(banto_tstore::SystemClock), None).await;
    let env = &ws.env;
    let (a, _) = push_connection(env, "Bridge1").await;
    let (_, key_a) = ingest_key(env, "a", a).await;
    let put = |name: &str, enabled: bool| json!({ "name": name, "protocol": "push", "host": "", "port": 0, "enabled": enabled });

    let (mut client, _) = ws.open(a, &key_a).await;
    let (status, body) = admin_put(
        &env.router,
        &format!("/api/plc-connections/{a}"),
        &env.admin_token,
        put("Bridge1-renamed", true),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(
        env.manager.ingest_hub().connection_info(a).unwrap().name,
        "Bridge1-renamed"
    );
    assert_pong(&mut client, 1).await;
    send_text(
        &mut client,
        json!({ "op": "values", "seq": 2, "items": [ { "key": "line1.temp", "v": 3 } ] })
            .to_string(),
    )
    .await;
    assert_eq!(recv_json(&mut client).await["accepted"], 1);

    let (status, body) = admin_put(
        &env.router,
        &format!("/api/plc-connections/{a}"),
        &env.admin_token,
        put("Bridge1-renamed", false),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(
        wait_close(&mut client).await,
        (1008, "connection_changed".to_string())
    );

    // プロトコルの変更（タグの無い接続だけが `push` から変えられる）。
    let b = bare_push_connection(env, "Bare").await;
    let (_, key_b) = ingest_key(env, "b", b).await;
    let (mut client_b, _) = ws.open(b, &key_b).await;
    let (status, body) = admin_put(
        &env.router,
        &format!("/api/plc-connections/{b}"),
        &env.admin_token,
        json!({ "name": "Bare", "protocol": "modbus-tcp", "host": "127.0.0.1", "port": 502,
                "enabled": false }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(
        wait_close(&mut client_b).await,
        (1008, "connection_changed".to_string())
    );

    let c = bare_push_connection(env, "Gone").await;
    let (_, key_c) = ingest_key(env, "c", c).await;
    let (mut client_c, _) = ws.open(c, &key_c).await;
    let (status, _) = admin_delete(
        &env.router,
        &format!("/api/plc-connections/{c}"),
        &env.admin_token,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        wait_close(&mut client_c).await,
        (1008, "connection_removed".to_string())
    );

    let rows = wait_disconnected_rows(env, 3).await;
    let reasons: Vec<&str> = rows
        .iter()
        .map(|row| row["reason"].as_str().unwrap())
        .collect();
    assert_eq!(
        reasons,
        vec![
            "connection_changed",
            "connection_changed",
            "connection_removed"
        ]
    );
}

/// 1 メッセージ 256 KiB ちょうどは通り、1 バイト超で 1009 `message_too_big`。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_message_over_256_kib_closes_with_1009() {
    let ws = ws_env(Arc::new(banto_tstore::SystemClock), None).await;
    let env = &ws.env;
    let (a, _) = push_connection(env, "Bridge1").await;
    let (_, key_a) = ingest_key(env, "a", a).await;
    let limit = 256 * 1024;
    let padded = |len: usize| {
        let head = r#"{"op":"ping","seq":1,"pad":""#;
        let tail = r#""}"#;
        format!("{head}{}{tail}", "x".repeat(len - head.len() - tail.len()))
    };

    let (mut client, _) = ws.open(a, &key_a).await;
    let exact = padded(limit);
    assert_eq!(exact.len(), limit);
    send_text(&mut client, exact).await;
    assert_eq!(
        recv_json(&mut client).await,
        json!({ "op": "pong", "seq": 1 })
    );

    send_text(&mut client, padded(limit + 1)).await;
    assert_eq!(
        wait_close(&mut client).await,
        (1009, "message_too_big".to_string())
    );
    let rows = wait_disconnected_rows(env, 1).await;
    assert_eq!(rows[0]["reason"], "message_too_big");
}

/// 収集の開始・停止とブリッジの設定の変更で `state` が届き、ブリッジの epoch が進む
/// （§4.5・§4.6）。設定の変更と同時の改名では閉じない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn state_follows_start_stop_and_bridge_config_changes() {
    let ws = ws_env(Arc::new(banto_tstore::SystemClock), None).await;
    let env = &ws.env;
    let (status, pg) = admin_post(
        &env.router,
        "/api/plc-connections",
        &env.admin_token,
        json!({ "name": "pg", "protocol": "postgres", "host": "127.0.0.1", "port": 5432,
                "database": "plant", "username": "reader", "password": "x" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{pg:?}");
    let pg = pg["id"].as_i64().unwrap();
    let bridge_settings = |keys: &[&str]| {
        let columns: Vec<Value> = keys
            .iter()
            .enumerate()
            .map(|(i, key)| json!({ "column": format!("c{i}"), "key": key }))
            .collect();
        json!({ "bridge": { "kind": "db", "db_connection_id": pg, "queries": [
            { "name": "q1", "period_ms": 1000, "sql": "SELECT 1", "columns": columns } ] } })
    };
    // REST の要求は `settings` をまだ運ばない（R2a-2e）ので、サービスで作る。
    let input: PlcConnectionInput = serde_json::from_value(json!({
        "name": "Bridge", "protocol": "push", "host": "", "port": 0,
        "settings": bridge_settings(&["k1"]),
    }))
    .unwrap();
    let bridge = PlcConnectionService::new(env.pool.clone())
        .create(input)
        .await
        .unwrap()
        .id;
    // タグの登録（REST）が commit を起こし、受信側に slot ができる。
    push_tag(env, bridge, "k1").await;
    let (_, key) = ingest_key(env, "bridge", bridge).await;
    let hub = env.manager.ingest_hub();

    let (mut client, welcome) = ws.open(bridge, &key).await;
    assert_eq!(welcome["collecting"], false);
    let epoch = welcome["epoch"].as_u64().unwrap();

    let (status, body) = admin_post(
        &env.router,
        "/api/collection/start",
        &env.admin_token,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(
        recv_json(&mut client).await,
        json!({ "op": "state", "collecting": true, "epoch": epoch + 1, "config_changed": false })
    );
    let (status, body) = admin_post(
        &env.router,
        "/api/collection/stop",
        &env.admin_token,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(
        recv_json(&mut client).await,
        json!({ "op": "state", "collecting": false, "epoch": epoch + 2, "config_changed": false })
    );

    // 設定の変更（送る集合にキーを足す）。commit は改名の PUT で起こす（改名では閉じない）。
    sqlx::query("UPDATE plc_connections SET settings = ? WHERE id = ?")
        .bind(bridge_settings(&["k1", "k2"]).to_string())
        .bind(bridge)
        .execute(&env.pool)
        .await
        .unwrap();
    let (status, body) = admin_put(
        &env.router,
        &format!("/api/plc-connections/{bridge}"),
        &env.admin_token,
        json!({ "name": "Bridge-renamed", "protocol": "push", "host": "", "port": 0,
                "enabled": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(
        recv_json(&mut client).await,
        json!({ "op": "state", "collecting": false, "epoch": epoch + 3, "config_changed": true })
    );
    assert_eq!(hub.connection_info(bridge).unwrap().epoch, epoch + 3);

    // 古い epoch の値は error stale_epoch（閉じない）、今の epoch なら受ける。
    send_text(
        &mut client,
        json!({ "op": "values", "seq": 1, "epoch": epoch, "items": [ { "key": "k1", "v": 1 } ] })
            .to_string(),
    )
    .await;
    let error = recv_json(&mut client).await;
    assert_eq!(
        (error["code"].as_str(), error["seq"].as_i64()),
        (Some("stale_epoch"), Some(1))
    );
    send_text(
        &mut client,
        json!({ "op": "values", "seq": 2, "epoch": epoch + 3, "items": [ { "key": "k1", "v": 1 } ] })
            .to_string(),
    )
    .await;
    assert_eq!(recv_json(&mut client).await["accepted"], 1);
    assert!(hub.has_ws_session(bridge));
}

/// `ack` を読まない送り手: 送信キューが満杯になると 1013 `backpressure` で閉じる
/// （Hub のメモリが膨らまない）。応答を大きくするため、長い未知のキーを送って `ack` に
/// 映させ、クライアントの受信バッファを小さくして TCP を早く詰まらせる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sender_that_does_not_read_is_closed_with_1013() {
    let ws = ws_env(
        Arc::new(banto_tstore::SystemClock),
        Some(IngestWsTuning {
            queue_capacity: 4,
            // 読み始めるまでの間に送信のタスクを abort しない（close フレームを確かめる）。
            drain_timeout: Duration::from_secs(60),
        }),
    )
    .await;
    let env = &ws.env;
    let (a, _) = push_connection(env, "Bridge1").await;
    let (_, key_a) = ingest_key(env, "a", a).await;

    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(4 * 1024).unwrap();
    let stream = socket
        .connect(format!("127.0.0.1:{}", ws.port()).parse().unwrap())
        .await
        .unwrap();
    let mut request = ws.url(&a.to_string()).into_client_request().unwrap();
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {key_a}").parse().unwrap());
    let (mut client, _) = tokio_tungstenite::client_async(request, stream)
        .await
        .expect("ws handshake");

    // 200 KB の未知のキー 1 つ（ack に 200 KB 映る）。瞬間 40 メッセージで 8 MB。
    let message =
        json!({ "op": "values", "items": [ { "key": "u".repeat(200_000), "v": 1 } ] }).to_string();
    let mut sent = 0;
    for _ in 0..400 {
        let send = client.send(WsMessage::Text(message.clone().into()));
        match tokio::time::timeout(Duration::from_secs(2), send).await {
            Ok(Ok(())) => sent += 1,
            // サーバーが読むのをやめて TCP が詰まった。
            _ => break,
        }
    }
    assert!(sent > 0);
    // サーバーが背圧を検知して閉じるのを待ってから読む。
    let hub = env.manager.ingest_hub();
    assert!(
        wait_until(|| !hub.has_ws_session(a)).await,
        "背圧で閉じていない"
    );
    let close = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match client.next().await {
                Some(Ok(WsMessage::Close(Some(frame)))) => {
                    return Some((u16::from(frame.code), frame.reason.to_string()))
                }
                Some(Ok(_)) => continue,
                _ => return None,
            }
        }
    })
    .await
    .expect("close within the bound");
    assert_eq!(close, Some((1013, "backpressure".to_string())));
    let rows = wait_disconnected_rows(env, 1).await;
    assert_eq!(rows[0]["reason"], "backpressure");
}

/// §4.3: 切断しても、最後の通信から `stale_timeout_ms` までは Live。その後 Down。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_link_stays_live_until_stale_timeout_after_disconnect() {
    let clock = Arc::new(ManualClock::new(1_000_000, 0));
    let ws = ws_env(clock.clone(), None).await;
    let env = &ws.env;
    let (a, _) = push_connection(env, "Bridge1").await;
    let (_, key_a) = ingest_key(env, "a", a).await;
    let hub = env.manager.ingest_hub();
    let key = format!("conn:{a}");

    let (mut client, _) = ws.open(a, &key_a).await;
    // 開いただけでは Live にならない（通信ではない）。
    assert!(matches!(hub.snapshot(&key).link, ExternalLink::Down { .. }));
    send_text(
        &mut client,
        json!({ "op": "values", "seq": 1, "items": [ { "key": "line1.temp", "v": 7 } ] })
            .to_string(),
    )
    .await;
    assert_eq!(recv_json(&mut client).await["accepted"], 1);
    client.close(None).await.unwrap();
    wait_disconnected_rows(env, 1).await;
    assert!(!hub.has_ws_session(a));

    assert_eq!(hub.snapshot(&key).link, ExternalLink::Live);
    clock.advance_ms(10_000);
    assert_eq!(hub.snapshot(&key).link, ExternalLink::Live);
    clock.advance_ms(1);
    assert!(matches!(hub.snapshot(&key).link, ExternalLink::Down { .. }));
}
