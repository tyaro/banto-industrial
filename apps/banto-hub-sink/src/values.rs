//! banto-tagclient SDK（設計 §5.1「Hub からの値の取得は banto-tagclient
//! SDK」）の薄いラッパー。**サイドカーに 1 本だけ**クライアントを持ち、
//! 全 sink group の対象タグの**和集合**を external name で購読して、外部名で
//! 引ける最新スナップショット（[`ValueView`]）を全グループへ配る。
//!
//! ## なぜグループごとに 1 本ではないのか
//!
//! Hub 側の購読は 250ms の評価ループ（`subscribe_core.rs` の
//! `EVAL_TICK_MS`）で回るので、購読を分けるとその評価が本数分だけ増える。
//! sink group は同じタグを重複して選べる（グループ A と B が同じタグを
//! 別テーブルへ書く）ため、和集合にすると Hub 側の負荷は「実際に使う
//! タグの本数」で頭打ちになる。SDK 側も `BindingRequest` の `external_name` 重複を
//! 拒否する（`validate_start_requests`）ので、和集合化は必須でもある。
//!
//! ## 値の流れと SDK の性質（`docs/banto-tagclient-design.md` §4.5）
//!
//! - 購読は **on-change 配信**で、値も quality も変わらなければ WS フレーム
//!   は 1 つも来ない。初期値は SDK が Live 到達時に REST スナップショットで
//!   埋める（publish gate）ので、`interval` モードのグループは静止した
//!   環境でも毎周期きちんと行を作れる。
//! - SDK が publish する `ValuesSnapshot` は**購読タグ全件**（REST
//!   スナップショットに新しい WS 値を重ねたもの）なので、[`ValueView`] は
//!   毎回まるごと差し替えてよい。差分（どのタグが変わったか）は
//!   `on_change` モードのプロデューサ側が自分で取る
//!   （[`crate::group::OnChangeGate`]）。
//! - Live でない間（Hub 停止・再接続・rebinding 中）は `current()` が
//!   `None` になる。そのとき [`ValueView::live`] は false で、**行は 1 つも
//!   作らない**（設計の実装指示「SDK が切断されていれば行は作られない」）。
//!
//! ## 購読の鍵は external name（リネームの扱い）
//!
//! SDK の Binding は購読も書き込みも `external_name` で行い、Hub の安定 ID
//! （`StableTagId`）は使わない（2026-09-30 オーナー決定、
//! `docs/scada-design.md` §9.6）。ID は削除→同名再作成や CSV 再取り込みで
//! 変わるが、名前は Hub の公開契約（WS subscribe 等）そのものだから。
//! 安定 ID は DB 行（`ts, tag_id, external_name, value, quality`）用に
//! [`crate::hub_api::SinkConfigTag`] へ残るだけで、購読には使わない。
//!
//! catalog との照合と再解決は SDK が行う。名前が一部だけ解決できる間も、
//! 解決済みタグの値を取り込む。全件未解決では SDK が `Unbound` で待ち、
//! 構成変更を受けて同じハンドルのまま再解決する。グループ設定で要求する
//! 名前の集合が変わったときは、サイドカーがハンドルを入れ替える。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use banto_tagclient::{
    BindingRequest, Endpoint, RestClient, SecretApiKey, TagClientConnectionState, TagClientHandle,
};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::config::SidecarConfig;
use crate::hub_api::REQUEST_TIMEOUT;

/// 1 タグの最新値（`banto_tagclient::ValueEntry` から必要な 3 つだけ）。
#[derive(Clone, Debug, PartialEq)]
pub struct ValueSample {
    pub value: Option<f64>,
    /// wire の quality 文字列（`good`/`bad`/`stale`、未知はその値）。
    pub quality: String,
    /// 値の `ptime`（epoch ミリ秒）。
    pub ptime_ms: i64,
}

/// 購読タグ全件の最新スナップショット（外部名で引く）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ValueView {
    /// SDK が Live で、スナップショットを持っているか。false の間は
    /// 行を作らない。
    pub live: bool,
    pub values: HashMap<String, ValueSample>,
}

impl ValueView {
    pub fn get(&self, external_name: &str) -> Option<&ValueSample> {
        if !self.live {
            return None;
        }
        self.values.get(external_name)
    }
}

/// 空・空白のみ・カンマを含む名前は SDK の `start()` が
/// `InvalidTagSelection` で購読全体を拒否するため、要求から外す。
fn is_unsupported_tag_name(name: &str) -> bool {
    name.trim().is_empty() || name.contains(',')
}

/// Hub の REST クライアント。1 回の REST 呼び出し（catalog・値の snapshot。
/// 購読の worker が再接続で引くものも含む）を [`REQUEST_TIMEOUT`] で
/// 頭打ちにする（#589）。SDK worker は停止合図を受けると進行中の要求を中断する。
///
/// 上限は SDK の既定ではなく、この Sink が opt-in で掛ける。SDK の既定に
/// 上限を入れると、外側に自分の期限を持つ利用側（ChronoGazer の Hub 操作の
/// 15 秒）が、その期限より先に `transport` で失敗して期限切れの理由を失う。
fn rest_client(config: &SidecarConfig) -> Result<RestClient, banto_tagclient::Error> {
    let endpoint = Endpoint::new(&config.hub_url)?;
    let secret = SecretApiKey::new(config.api_key.clone())?;
    if let Some(pins) = &config.hub_tls {
        RestClient::with_pinned_tls(endpoint, secret, REQUEST_TIMEOUT, pins.clone())
    } else {
        RestClient::with_request_timeout(endpoint, secret, REQUEST_TIMEOUT)
    }
}

/// 購読 1 本（SDK ハンドル + 変換タスク）。要求する名前の集合が
/// 変わったときか、SDK の worker が終了したときに作り直す。
pub struct Subscription {
    handle: Option<TagClientHandle>,
    pump: Option<JoinHandle<()>>,
    dead: Arc<AtomicBool>,
    /// SDK に渡せない名前も含めた、差分判定用の名前の集合。
    desired: Vec<String>,
}

impl Subscription {
    pub(crate) fn start_with_recovery(
        config: &SidecarConfig,
        desired: Vec<String>,
        view_tx: watch::Sender<Arc<ValueView>>,
        recovery: SubscriptionRecovery,
    ) -> Result<Option<Self>, banto_tagclient::Error> {
        let desired = normalize_names(desired);
        if desired.is_empty() {
            view_tx.send_replace(Arc::new(ValueView::default()));
            return Ok(None);
        }

        let mut unsupported = Vec::new();
        let mut requests = Vec::new();
        for name in &desired {
            if is_unsupported_tag_name(name) {
                unsupported.push(name.clone());
            } else {
                requests.push(BindingRequest {
                    binding_key: name.clone(),
                    external_name: name.clone(),
                });
            }
        }
        let dead = Arc::new(AtomicBool::new(false));
        if requests.is_empty() {
            // 全件がプロトコル非対応なら、設定が変わるまでこの要求集合を覚えておく。
            view_tx.send_replace(Arc::new(ValueView::default()));
            crate::log::log_line(&subscription_message(0, &unsupported));
            return Ok(Some(Self {
                handle: None,
                pump: None,
                dead,
                desired,
            }));
        }

        let handle = rest_client(config)?.start(requests)?;
        let pump = tokio::spawn(pump_values(
            handle.state_watch(),
            view_tx,
            dead.clone(),
            unsupported,
            recovery,
        ));
        Ok(Some(Self {
            handle: Some(handle),
            pump: Some(pump),
            dead,
            desired,
        }))
    }

    /// SDK の worker が終了したか。終端エラーはサイドカーがバックオフ後に再試行する。
    pub fn is_dead(&self) -> bool {
        self.dead.load(Ordering::Relaxed)
    }

    pub fn desired(&self) -> &[String] {
        &self.desired
    }

    /// 変換タスクを停止して join してから SDK worker を停止する。
    pub async fn stop(mut self) {
        if let Some(pump) = self.pump.take() {
            pump.abort();
            // Retire and join the observer BEFORE intentional SDK shutdown:
            // shutdown may publish failure states that must not arm recovery.
            // No late observation may clear/re-arm a replacement's shared latch.
            let _ = pump.await;
        }
        if let Some(handle) = self.handle.take() {
            // 停止時のエラー（`Unauthorized` で終わっていた世代など）は
            // ここでは意味を持たない - 呼び出し側は作り直すだけ。
            let _ = handle.shutdown().await;
        }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        // `stop` を通らずに落ちた場合の保険。`TagClientHandle::drop` 自身が
        // worker を abort するので、ここでは変換タスクだけ畳めばよい。
        if let Some(pump) = self.pump.take() {
            pump.abort();
        }
    }
}

/// Only a failed SDK generation arms recovery: normal startup and catalog
/// rebinding are not outages. Keep the latch across intermediate handshake states.
#[derive(Clone, Default)]
pub(crate) struct SubscriptionRecovery {
    reconnecting: Arc<AtomicBool>,
}

impl SubscriptionRecovery {
    pub(crate) fn failed(&self) {
        self.reconnecting.store(true, Ordering::Relaxed);
    }

    pub(crate) fn reset(&self) {
        self.reconnecting.store(false, Ordering::Relaxed);
    }

    fn observe(&self, state: TagClientConnectionState) -> Option<&'static str> {
        match state {
            TagClientConnectionState::Reconnecting | TagClientConnectionState::Unauthorized => {
                self.failed()
            }
            TagClientConnectionState::Live if self.reconnecting.swap(false, Ordering::Relaxed) => {
                return Some("banto-hub-sink: Hub へのタグ購読が復帰しました");
            }
            _ => {}
        }
        None
    }
}

/// SDK の状態 watch を [`ValueView`] の watch へ変換し続けるタスク。
/// SDK ハンドルを入れ替えても、プロデューサが持つ受信端は維持する。
async fn pump_values(
    mut state_rx: watch::Receiver<banto_tagclient::TagClientState>,
    view_tx: watch::Sender<Arc<ValueView>>,
    dead: Arc<AtomicBool>,
    unsupported: Vec<String>,
    recovery: SubscriptionRecovery,
) {
    let mut saw_active = false;
    let mut generation = None;
    let mut last_resolution = None;
    loop {
        let (recovered, changed_resolution) = {
            let state = state_rx.borrow_and_update();
            let connection = state.connection_state();
            let recovered = recovery.observe(connection);
            let terminal = connection == TagClientConnectionState::Unauthorized
                || (connection == TagClientConnectionState::Stopped
                    && (saw_active || state.last_error().is_some()));
            saw_active |= connection != TagClientConnectionState::Stopped;

            let snapshot = state.snapshot();
            let view = match snapshot.and_then(|snapshot| snapshot.values.as_deref()) {
                Some(values) => ValueView {
                    live: true,
                    values: values
                        .values
                        .iter()
                        .map(|entry| {
                            (
                                entry.tag.clone(),
                                ValueSample {
                                    value: entry.v,
                                    quality: entry.q.as_str().to_string(),
                                    ptime_ms: entry.t,
                                },
                            )
                        })
                        .collect(),
                },
                None => ValueView::default(),
            };
            view_tx.send_replace(Arc::new(view));
            if terminal {
                dead.store(true, Ordering::Relaxed);
            }

            let changed_resolution = if let Some(snapshot) =
                snapshot.filter(|snapshot| generation != Some(snapshot.generation))
            {
                generation = Some(snapshot.generation);
                let mut unresolved = unsupported.clone();
                unresolved.extend(
                    snapshot
                        .resolution
                        .unresolved
                        .iter()
                        .map(|binding| binding.external_name.clone()),
                );
                unresolved.sort_unstable();
                Some((snapshot.resolution.resolved.len(), unresolved))
            } else {
                None
            };
            (recovered, changed_resolution)
        };
        // ログは同期 I/O のため SDK の watch を解放してから書く。
        if let Some(resolution) =
            changed_resolution.filter(|next| Some(next) != last_resolution.as_ref())
        {
            crate::log::log_line(&subscription_message(resolution.0, &resolution.1));
            last_resolution = Some(resolution);
        }
        if let Some(message) = recovered {
            crate::log::log_line(message);
        }
        if state_rx.changed().await.is_err() {
            // 送信端（SDK ハンドル）が落ちた = この世代は終わり。
            view_tx.send_replace(Arc::new(ValueView::default()));
            dead.store(true, Ordering::Relaxed);
            return;
        }
    }
}

/// 重複除去 + ソート。差分判定（`==` 比較）が順序に左右されないように
/// する。
pub fn normalize_names(mut names: Vec<String>) -> Vec<String> {
    names.sort();
    names.dedup();
    names
}

/// 未解決の名前を何件までログへ列挙するか。
const UNRESOLVED_LOG_LIMIT: usize = 5;

/// 購読対象を示すログの本文。未解決が 0 なら「N タグ」だけ、あれば名前を
/// [`UNRESOLVED_LOG_LIMIT`] 件まで列挙し、超えた分は「ほか N 件」。
fn subscription_message(resolved_len: usize, unresolved: &[String]) -> String {
    if unresolved.is_empty() {
        return format!("banto-hub-sink: Hub の購読対象が確定しました（{resolved_len} タグ）");
    }
    let mut names = unresolved
        .iter()
        .take(UNRESOLVED_LOG_LIMIT)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if unresolved.len() > UNRESOLVED_LOG_LIMIT {
        names.push_str(&format!(
            "、ほか {} 件",
            unresolved.len() - UNRESOLVED_LOG_LIMIT
        ));
    }
    format!(
        "banto-hub-sink: Hub の購読対象が確定しました（{resolved_len} タグ。未解決 {}: {names}）",
        unresolved.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio_tungstenite::{accept_async, tungstenite::Message, WebSocketStream};

    #[test]
    fn subscription_message_lists_up_to_five_unresolved_names() {
        assert_eq!(
            subscription_message(3, &[]),
            "banto-hub-sink: Hub の購読対象が確定しました（3 タグ）"
        );
        let names: Vec<String> = ["a", "b", "c"].iter().map(|n| (*n).to_owned()).collect();
        assert_eq!(
            subscription_message(97, &names),
            "banto-hub-sink: Hub の購読対象が確定しました（97 タグ。未解決 3: a, b, c）"
        );
        let many: Vec<String> = (1..=7).map(|n| format!("t{n}")).collect();
        assert_eq!(
            subscription_message(0, &many),
            "banto-hub-sink: Hub の購読対象が確定しました（0 タグ。未解決 7: t1, t2, t3, t4, t5、ほか 2 件）"
        );
    }

    #[test]
    fn subscription_recovery_is_logged_once_after_a_failed_generation() {
        use TagClientConnectionState::*;
        let recovery = SubscriptionRecovery::default();
        for state in [
            Stopped,
            Connecting,
            Handshaking,
            Live,
            Live,
            Rebinding,
            Live,
        ] {
            assert_eq!(
                recovery.observe(state),
                None,
                "startup / healthy rebind: {state}"
            );
        }
        for state in [Reconnecting, Reconnecting, Connecting, Handshaking] {
            assert_eq!(recovery.observe(state), None, "not recovered: {state}");
        }
        assert_eq!(
            recovery.observe(Live),
            Some("banto-hub-sink: Hub へのタグ購読が復帰しました")
        );
        assert_eq!(recovery.observe(Live), None, "repeat updates");
        assert_eq!(recovery.observe(Reconnecting), None);
        assert!(recovery.observe(Live).is_some(), "next outage");
    }

    #[test]
    fn subscription_recovery_survives_replacement_but_not_an_intentional_reset() {
        use TagClientConnectionState::*;
        let owner = SubscriptionRecovery::default();
        {
            let first_pump = owner.clone();
            assert_eq!(first_pump.observe(Live), None, "normal initial startup");
            assert_eq!(first_pump.observe(Reconnecting), None);
            assert_eq!(first_pump.observe(Stopped), None);
        }
        let replacement_pump = owner.clone();
        assert_eq!(replacement_pump.observe(Connecting), None);
        assert_eq!(replacement_pump.observe(Rebinding), None);
        assert!(
            replacement_pump.observe(Live).is_some(),
            "outage survives pump replacement"
        );
        assert_eq!(owner.observe(Live), None, "one line across all generations");
        assert_eq!(owner.clone().observe(Rebinding), None);
        assert_eq!(
            owner.clone().observe(Live),
            None,
            "intentional new generation is not an outage"
        );
        owner.failed(); // Engine observed an unexpected worker death/start error.
        assert!(owner.clone().observe(Live).is_some());
        assert_eq!(owner.observe(Unauthorized), None);
        assert!(owner.clone().observe(Live).is_some());
        owner.failed();
        owner.reset(); // All desired tags removed: a later startup is intentional.
        assert_eq!(owner.clone().observe(Live), None);
    }

    #[test]
    fn normalize_names_sorts_and_deduplicates() {
        let names = normalize_names(vec![
            "line2.g.b".to_owned(),
            "line1.g.z".to_owned(),
            "line2.g.b".to_owned(),
            "line1.g.a".to_owned(),
        ]);
        assert_eq!(names, vec!["line1.g.a", "line1.g.z", "line2.g.b"]);
    }

    fn owned(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn config(hub_url: String) -> SidecarConfig {
        SidecarConfig {
            hub_url,
            api_key: "bh_test.secret".to_owned(),
            hub_tls: None,
            config_refresh: Duration::from_secs(30),
            status_push: Duration::from_secs(10),
            queue_max_rows: 1000,
            flush_interval: Duration::from_millis(100),
            batch_size: 100,
            shutdown_flush: Duration::from_secs(1),
        }
    }

    #[test]
    fn empty_or_unsupported_names_never_start_the_sdk() {
        // SDK を呼べば失敗する URL でも、要求が空ならクライアントを作らない。
        let config = config("not a URL".to_owned());
        let (view_tx, view_rx) = watch::channel(Arc::new(ValueView {
            live: true,
            values: HashMap::new(),
        }));
        assert!(Subscription::start_with_recovery(
            &config,
            Vec::new(),
            view_tx.clone(),
            SubscriptionRecovery::default(),
        )
        .unwrap()
        .is_none());

        let subscription = Subscription::start_with_recovery(
            &config,
            owned(&["a,b", "", "  ", "a,b"]),
            view_tx,
            SubscriptionRecovery::default(),
        )
        .unwrap()
        .expect("全件が購読不可なら要求集合だけを覚える");
        assert_eq!(subscription.desired(), owned(&["", "  ", "a,b"]));
        assert!(subscription.handle.is_none());
        assert!(!subscription.is_dead());
        assert!(!view_rx.borrow().live);
    }

    async fn serve_json(listener: &TcpListener, path: &str, body: serde_json::Value) {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(stream.read_u8().await.unwrap());
        }
        let request = String::from_utf8(request).unwrap();
        assert!(request.starts_with(&format!("GET {path} HTTP/1.1\r\n")));
        let body = body.to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    }

    async fn serve_catalog(listener: &TcpListener, revision: u64, tags: &[&str]) {
        let tags = tags
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let tag_id = revision * 10 + index as u64;
                serde_json::json!({
                    "external_name": name, "tag_key": format!("tag:{tag_id}"),
                    "ids": [1, 1, tag_id],
                    "connection": "c", "group": "g", "name": name, "address": "a",
                    "data_type": "f64", "decimals": 0, "period_ms": 100,
                    "enabled": true, "writable": false, "tag_kind": "plc", "retain": false,
                    "simulation": false, "configured_simulation": false,
                    "effective_simulation": false, "value_source": "real",
                })
            })
            .collect::<Vec<_>>();
        serve_json(
            listener,
            "/api/v1/tags",
            serde_json::json!({
                "revision": revision, "run_id": 1, "collection_mode": "configured", "tags": tags,
            }),
        )
        .await;
    }

    async fn accept_stream(listener: &TcpListener) -> WebSocketStream<TcpStream> {
        let (stream, _) = listener.accept().await.unwrap();
        accept_async(stream).await.unwrap()
    }

    async fn serve_live(
        listener: &TcpListener,
        revision: u64,
        name: &str,
        value: f64,
    ) -> WebSocketStream<TcpStream> {
        serve_catalog(listener, revision, &[name]).await;
        let mut socket = accept_stream(listener).await;
        let subscribe = socket.next().await.unwrap().unwrap().into_text().unwrap();
        let subscribe: serde_json::Value = serde_json::from_str(&subscribe).unwrap();
        assert_eq!(subscribe["op"], "subscribe");
        assert_eq!(subscribe["tags"], serde_json::json!([name]));
        let row = serde_json::json!({"tag": name, "v": value, "q": "good", "t": 20});
        let frame =
            serde_json::json!({"op": "data", "id": subscribe["id"], "t": 20, "values": [row]});
        socket
            .send(Message::Text(frame.to_string().into()))
            .await
            .unwrap();
        serve_json(
            listener,
            &format!("/api/v1/values?tags={name}"),
            serde_json::json!({
                "revision": revision, "run_id": 1, "collection_mode": "configured", "t": 20,
                "values": [{"tag": name, "v": value, "q": "good", "t": 20, "value_source": "real"}],
            }),
        )
        .await;
        socket
    }

    async fn change_catalog(socket: &mut WebSocketStream<TcpStream>, revision: u64) {
        let frame = serde_json::json!({"op": "config_changed", "revision": revision});
        socket
            .send(Message::Text(frame.to_string().into()))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn sdk_keeps_the_subscription_across_partial_and_unbound_generations() {
        use tokio_tungstenite::tungstenite::protocol::{frame::coding::CloseCode, CloseFrame};

        tokio::time::timeout(Duration::from_secs(10), async {
            let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let config = config(format!("http://{}", listener.local_addr().unwrap()));
            let (view_tx, mut view_rx) = watch::channel(Arc::new(ValueView::default()));
            let subscription = Subscription::start_with_recovery(
                &config,
                owned(&["good", "missing", "bad,name"]),
                view_tx,
                SubscriptionRecovery::default(),
            )
            .unwrap()
            .unwrap();
            let mut states = subscription.handle.as_ref().unwrap().state_watch();

            let mut partial = serve_live(&listener, 1, "good", 1.0).await;
            {
                let view = view_rx.wait_for(|view| view.live).await.unwrap();
                assert_eq!(view.get("good").unwrap().value, Some(1.0));
                assert!(view.get("missing").is_none());
            }
            change_catalog(&mut partial, 2).await;
            drop(partial);

            // Unbound では subscribe を送らず、ping/pong と catalog 再確認だけを行う。
            serve_catalog(&listener, 2, &[]).await;
            let mut unbound = accept_stream(&listener).await;
            assert_eq!(
                unbound.next().await.unwrap().unwrap().into_text().unwrap(),
                r#"{"op":"ping"}"#
            );
            unbound
                .send(Message::Text(r#"{"op":"pong"}"#.into()))
                .await
                .unwrap();
            serve_catalog(&listener, 2, &[]).await;
            {
                let state = states
                    .wait_for(|state| state.connection_state() == TagClientConnectionState::Unbound)
                    .await
                    .unwrap();
                assert_eq!(state.snapshot().unwrap().resolution.unresolved.len(), 2);
                assert!(state.current().is_none());
            }
            {
                let view = view_rx.wait_for(|view| !view.live).await.unwrap();
                assert!(view.values.is_empty());
            }
            assert!(!subscription.is_dead());
            change_catalog(&mut unbound, 3).await;
            drop(unbound);

            // 最初に解決できなかった名前を、同じ要求・ハンドルのまま購読する。
            let mut recovered = serve_live(&listener, 3, "missing", 3.0).await;
            {
                let view = view_rx.wait_for(|view| view.live).await.unwrap();
                assert_eq!(view.get("missing").unwrap().value, Some(3.0));
                assert!(view.get("good").is_none());
            }

            recovered
                .send(Message::Close(Some(CloseFrame {
                    code: CloseCode::Policy,
                    reason: "api_key_revoked".into(),
                })))
                .await
                .unwrap();
            while !subscription.is_dead() {
                tokio::task::yield_now().await;
            }
            assert!(view_rx.borrow().values.is_empty());
            subscription.stop().await;
        })
        .await
        .expect("同じハンドルの部分解決・再解決・停止が予算内に終わる");
    }

    #[test]
    fn a_view_that_is_not_live_never_yields_a_sample() {
        let mut view = ValueView {
            live: false,
            values: HashMap::new(),
        };
        view.values.insert(
            "line1.fast.temp01".to_string(),
            ValueSample {
                value: Some(1.0),
                quality: "good".to_string(),
                ptime_ms: 10,
            },
        );
        assert!(view.get("line1.fast.temp01").is_none());
        view.live = true;
        assert!(view.get("line1.fast.temp01").is_some());
    }
}
