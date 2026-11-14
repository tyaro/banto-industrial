//! #644 受入条件 4 の場面試験（Sink: REST・取り込みの WebSocket・`run` の再試行）。
//!
//! 1 つのポート（`SwitchPort`）が Hub の一生（動いている・止まる・別プロセスが先取り・
//! 入れ替わる・証明書が更新されて戻る）を通るあいだ、ダミーの API キーが「本物の Hub」
//! 以外に 1 バイトも届かないことを、同じクライアントを使い回して確かめる。
//!
//! `connect_ws` は crate 内だけの口なので、直接呼べるよう `session` の子モジュールに置く。
//! プロキシの環境変数は同じ試験バイナリの中では隔離できないので、環境変数を付けて自分自身を
//! 子プロセスとして起こす（`proxy_child` は `--ignored` でだけ走る）。

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use banto_hub_squat_harness::{Mode, ModeKind, RecordingListener, SwitchPort, TestIdentity};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::AbortHandle;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;

use super::{connect_ws, BridgeTasks, ConnectError, SourceContext, SourceHandle};
use crate::config::SidecarConfig;
use crate::hub_api::{
    BridgeColumnConfig, BridgeDbConfig, BridgeQueryConfig, BridgeSourceConfig, HubApi, HubApiError,
    SinkStatusPush,
};
use crate::run::{run, SidecarOptions, StopRequest};
use crate::status::Redactor;

const DUMMY_KEY: &str = "bh_squatdummy.6f1c2a9e0d4b7381";
/// 先取りされた側へ何度つなぎ直しても同じ結果になることを見る回数。
const ATTEMPTS: usize = 5;

/// 「本物の Hub」。自分のポートで TLS を話し、Sink が使う REST と取り込みの WebSocket の
/// 握手に答える。届いた要求の頭（ヘッダー）をすべて記録する。
struct BackendHub {
    addr: SocketAddr,
    heads: Arc<Mutex<Vec<String>>>,
    redirect: Arc<Mutex<Option<(u16, String)>>>,
    task: AbortHandle,
}

impl BackendHub {
    async fn start(identity: &TestIdentity) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let heads = Arc::new(Mutex::new(Vec::new()));
        let redirect = Arc::new(Mutex::new(None));
        let acceptor = TlsAcceptor::from(identity.server_config());
        let (task_heads, task_redirect) = (Arc::clone(&heads), Arc::clone(&redirect));
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    continue;
                };
                tokio::spawn(serve(
                    stream,
                    acceptor.clone(),
                    Arc::clone(&task_heads),
                    Arc::clone(&task_redirect),
                ));
            }
        })
        .abort_handle();
        Self {
            addr,
            heads,
            redirect,
            task,
        }
    }

    fn set_redirect(&self, status: u16, location: &str) {
        *self.redirect.lock().unwrap() = Some((status, location.to_owned()));
    }

    fn requests(&self) -> usize {
        self.heads.lock().unwrap().len()
    }

    /// ダミーのキーを `Authorization: Bearer` で受けた要求の数。
    fn bearer_requests(&self) -> usize {
        let needle = format!("Bearer {DUMMY_KEY}");
        self.heads
            .lock()
            .unwrap()
            .iter()
            .filter(|head| head.contains(&needle))
            .count()
    }

    /// キー付きで届いた WebSocket の upgrade の数。
    fn bearer_upgrades(&self) -> usize {
        let needle = format!("Bearer {DUMMY_KEY}");
        self.heads
            .lock()
            .unwrap()
            .iter()
            .filter(|head| {
                head.contains(&needle) && head.to_ascii_lowercase().contains("upgrade: websocket")
            })
            .count()
    }
}

impl Drop for BackendHub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// 頭を読み、`Content-Length` の本文があれば読み捨てる（keep-alive の次の要求を壊さない）。
async fn read_request<S: AsyncReadExt + Unpin>(stream: &mut S) -> Option<String> {
    let mut raw = Vec::new();
    let mut buffer = [0u8; 1024];
    let head_end = loop {
        if let Some(position) = raw.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
        let count = stream.read(&mut buffer).await.ok()?;
        if count == 0 {
            return None;
        }
        raw.extend_from_slice(&buffer[..count]);
    };
    let head = String::from_utf8_lossy(&raw[..head_end]).into_owned();
    let length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    while raw.len() < head_end + length {
        let count = stream.read(&mut buffer).await.ok()?;
        if count == 0 {
            return None;
        }
        raw.extend_from_slice(&buffer[..count]);
    }
    Some(head)
}

fn json_response(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

async fn serve(
    stream: TcpStream,
    acceptor: TlsAcceptor,
    heads: Arc<Mutex<Vec<String>>>,
    redirect: Arc<Mutex<Option<(u16, String)>>>,
) {
    let Ok(mut stream) = acceptor.accept(stream).await else {
        return;
    };
    while let Some(head) = read_request(&mut stream).await {
        heads.lock().unwrap().push(head.clone());
        if head.to_ascii_lowercase().contains("upgrade: websocket") {
            let key = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("sec-websocket-key")
                        .then(|| value.trim().to_owned())
                })
                .unwrap_or_default();
            let response = format!(
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
                derive_accept_key(key.as_bytes())
            );
            let _ = stream.write_all(response.as_bytes()).await;
            // 握手のあとは、相手が閉じるか Hub が死ぬ（切られる）まで開けておく。
            let mut sink = [0u8; 256];
            while matches!(stream.read(&mut sink).await, Ok(count) if count > 0) {}
            return;
        }
        let configured = redirect.lock().unwrap().clone();
        let response = if let Some((status, location)) = configured {
            format!(
                "HTTP/1.1 {status} Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\n\r\n"
            )
        } else if head.starts_with("GET /api/sink/config ") {
            json_response(r#"{"generatedAt":1,"groups":[],"connections":[]}"#)
        } else if head.starts_with("GET /api/ingest/bridge-config ") {
            json_response(r#"{"generatedAt":1,"sources":[]}"#)
        } else if head.starts_with("PUT /api/sink/status ") {
            "HTTP/1.1 204 No Content\r\n\r\n".to_owned()
        } else {
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_owned()
        };
        if stream.write_all(response.as_bytes()).await.is_err() {
            return;
        }
    }
}

/// `hub_tls_pinned_certs` に `identities`（1〜2 件）を pin した設定。
fn config_for(url: &str, identities: &[&TestIdentity]) -> SidecarConfig {
    let pins: Vec<String> = identities
        .iter()
        .map(|identity| format!("'''{}'''", identity.pem()))
        .collect();
    let raw = format!(
        "hub_url = \"{url}\"\napi_key = \"{DUMMY_KEY}\"\nhub_tls_pinned_certs = [{}]\nconfig_refresh_secs = 5\nstatus_push_secs = 1\n",
        pins.join(", ")
    );
    SidecarConfig::from_toml_str(&raw, std::path::Path::new("banto-hub-sink.toml")).unwrap()
}

fn ws_url(port: &SwitchPort) -> String {
    format!(
        "wss://127.0.0.1:{}/api/v1/ingest/1/stream",
        port.addr().port()
    )
}

fn pins(identities: &[&TestIdentity]) -> banto_tagclient::PinnedHubTls {
    let pems: Vec<String> = identities.iter().map(|identity| identity.pem()).collect();
    banto_tagclient::PinnedHubTls::from_pem(&pems).unwrap()
}

fn empty_status() -> SinkStatusPush {
    SinkStatusPush { groups: Vec::new() }
}

/// Sink が API キーを付けて Hub に送る 3 つの REST（設定・ブリッジ設定・状態 push）。
async fn rest_calls(api: &HubApi) -> [Result<(), HubApiError>; 3] {
    [
        api.fetch_config().await.map(|_| ()),
        api.fetch_bridge_config().await.map(|_| ()),
        api.push_status(&empty_status()).await,
    ]
}

/// 先取りや停止で失敗するときの型（TLS の検証失敗、または接続できない）。
/// 認証の失敗・HTTP の失敗・本文の不一致になった時点で、何かが応答したことになる。
fn assert_refused(label: &str, result: Result<(), HubApiError>) {
    match result {
        Err(HubApiError::Tls) | Err(HubApiError::Transport) => {}
        other => panic!("{label}: must be refused by TLS/transport, got {other:?}"),
    }
}

async fn assert_rest_refused(api: &HubApi, label: &str) {
    for attempt in 0..ATTEMPTS {
        for (index, result) in rest_calls(api).await.into_iter().enumerate() {
            assert_refused(&format!("{label} attempt {attempt} call {index}"), result);
        }
    }
}

async fn ws_is_ok(url: &str, pins: &banto_tagclient::PinnedHubTls) -> bool {
    connect_ws(url, DUMMY_KEY, Some(pins)).await.is_ok()
}

async fn assert_ws_refused(url: &str, pins: &banto_tagclient::PinnedHubTls, label: &str) {
    for attempt in 0..ATTEMPTS {
        let result = connect_ws(url, DUMMY_KEY, Some(pins)).await;
        assert!(
            matches!(result, Err(ConnectError::Transport)),
            "{label} attempt {attempt}: must be refused, got ok={}",
            result.is_ok()
        );
    }
}

/// 先取りした側には TLS の握手（平文の先取りなら ClientHello の生のバイト）しか届かず、
/// HTTP の要求（要求行・Authorization）は 1 バイトも届かない。
fn assert_no_http_reached(port: &SwitchPort) {
    assert!(
        port.squat_tls_bytes().is_empty(),
        "no application byte may reach a TLS squatter"
    );
    let seen = String::from_utf8_lossy(&port.squat_bytes()).to_ascii_lowercase();
    for marker in [
        "http/1.",
        "authorization",
        "bearer",
        "get /",
        "put /",
        "upgrade:",
    ] {
        assert!(
            !seen.contains(marker),
            "an HTTP request reached a squatter ({marker})"
        );
    }
}

async fn wait_until(what: &str, seconds: u64, mut condition: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
    while !condition() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn rest_calls_never_deliver_the_key_to_a_squatter_or_a_stopped_hub() {
    let identity_a = TestIdentity::generate();
    let squatter = TestIdentity::generate();
    let hub = BackendHub::start(&identity_a).await;
    let port = SwitchPort::start().await;
    let api = HubApi::new(&config_for(&port.https_url(), &[&identity_a])).unwrap();

    // 本物の Hub が動いている間は 3 つとも通り、キーは本物にだけ届く。
    port.set(Mode::Relay(hub.addr));
    for (index, result) in rest_calls(&api).await.into_iter().enumerate() {
        assert!(result.is_ok(), "real hub call {index}: {result:?}");
    }
    assert!(hub.bearer_requests() >= 3);
    let delivered = hub.bearer_requests();

    // 止まって、別の証明書の TLS が同じポートを先取りした: TLS の検証失敗で断る。
    port.set(Mode::SquatTls(squatter.server_config()));
    for attempt in 0..ATTEMPTS {
        for (index, result) in rest_calls(&api).await.into_iter().enumerate() {
            // 最初の 1 回だけは、使い回していた接続が切れた Transport になりうる。
            let first = attempt == 0 && index == 0;
            assert!(
                matches!(result, Err(HubApiError::Tls))
                    || (first && matches!(result, Err(HubApiError::Transport))),
                "tls squat attempt {attempt} call {index}: {result:?}"
            );
        }
    }
    assert!(port.accepts(ModeKind::SquatTls) >= ATTEMPTS);

    // 証明書もない平文が先取りした。
    port.set(Mode::SquatPlain);
    assert_rest_refused(&api, "plain squat").await;
    assert!(port.accepts(ModeKind::SquatPlain) >= ATTEMPTS);

    // Hub が止まっていて何も待っていない。
    port.set(Mode::Down);
    assert_rest_refused(&api, "down").await;

    port.assert_secret_absent(DUMMY_KEY);
    assert_eq!(port.squat_tls_handshakes(), 0, "pin must stop the TLS");
    assert_no_http_reached(&port);
    assert_eq!(
        hub.bearer_requests(),
        delivered,
        "the real Hub was not used"
    );
}

#[tokio::test]
async fn a_swap_between_the_check_and_the_send_does_not_leak_on_the_same_client() {
    let identity_a = TestIdentity::generate();
    let squatter = TestIdentity::generate();
    let hub = BackendHub::start(&identity_a).await;
    let port = SwitchPort::start().await;
    let api = HubApi::new(&config_for(&port.https_url(), &[&identity_a])).unwrap();

    // 1 回目（確認）は本物に通る。接続は使い回しのために残る。
    port.set(Mode::Relay(hub.addr));
    assert!(api.fetch_config().await.is_ok());

    // 確認のあと送る前に、待受が先取りに入れ替わった。
    port.set(Mode::SquatTls(squatter.server_config()));
    assert!(matches!(
        api.push_status(&empty_status()).await,
        Err(HubApiError::Tls | HubApiError::Transport)
    ));
    // 使い回していた接続が切れた最初の失敗のあと、つなぎ直す要求も断られる
    // （つなぎ直しのたびに pin を確かめ直す）。
    assert!(matches!(
        api.push_status(&empty_status()).await,
        Err(HubApiError::Tls)
    ));
    assert!(matches!(api.fetch_config().await, Err(HubApiError::Tls)));
    assert!(
        port.accepts(ModeKind::SquatTls) >= 2,
        "the client really dialed the squatter and refused it"
    );

    // 平文への入れ替わりも同じ。
    port.set(Mode::SquatPlain);
    assert_rest_refused(&api, "plain swap").await;

    port.assert_secret_absent(DUMMY_KEY);
    assert_eq!(port.squat_tls_handshakes(), 0);
    assert_no_http_reached(&port);
}

#[tokio::test]
async fn ingest_websocket_never_delivers_the_key_to_a_squatter_or_a_stopped_hub() {
    let identity_a = TestIdentity::generate();
    let squatter = TestIdentity::generate();
    let hub = BackendHub::start(&identity_a).await;
    let port = SwitchPort::start().await;
    let url = ws_url(&port);
    let pins_a = pins(&[&identity_a]);

    port.set(Mode::Relay(hub.addr));
    assert!(ws_is_ok(&url, &pins_a).await, "the real Hub accepts");
    assert_eq!(hub.bearer_upgrades(), 1);

    port.set(Mode::SquatTls(squatter.server_config()));
    assert_ws_refused(&url, &pins_a, "tls squat").await;
    assert!(port.accepts(ModeKind::SquatTls) >= ATTEMPTS);

    port.set(Mode::SquatPlain);
    assert_ws_refused(&url, &pins_a, "plain squat").await;
    assert!(port.accepts(ModeKind::SquatPlain) >= ATTEMPTS);

    port.set(Mode::Down);
    assert_ws_refused(&url, &pins_a, "down").await;

    port.assert_secret_absent(DUMMY_KEY);
    assert_eq!(port.squat_tls_handshakes(), 0);
    assert_no_http_reached(&port);
    assert_eq!(hub.bearer_upgrades(), 1, "the real Hub was not used");
}

fn bridge_source() -> BridgeSourceConfig {
    BridgeSourceConfig {
        connection_id: 1,
        name: "squat".to_string(),
        stale_timeout_ms: 3_000,
        config_fingerprint: "sha256:00".to_string(),
        ingest_allowed: true,
        queries: vec![BridgeQueryConfig {
            name: "q".to_string(),
            period_ms: 500,
            sql: "SELECT 1 AS a".to_string(),
            columns: vec![BridgeColumnConfig {
                column: "a".to_string(),
                key: "k1".to_string(),
            }],
        }],
        db: BridgeDbConfig {
            id: 1,
            name: "pg".to_string(),
            enabled: true,
            host: "127.0.0.1".to_string(),
            port: 0,
            database: None,
            username: None,
            password: None,
        },
    }
}

#[tokio::test]
async fn the_ingest_retry_loop_keeps_redialing_a_squatter_without_leaking_the_key() {
    let identity_a = TestIdentity::generate();
    let squatter = TestIdentity::generate();
    let hub = BackendHub::start(&identity_a).await;
    let port = SwitchPort::start().await;
    port.set(Mode::Relay(hub.addr));

    let (_generation_tx, generation_rx) = tokio::sync::watch::channel(0);
    let ctx = SourceContext {
        ws_url: ws_url(&port),
        hub_tls: Some(pins(&[&identity_a])),
        api_key: DUMMY_KEY.to_string(),
        redactor: Redactor::new(),
        refresh: Arc::new(tokio::sync::Notify::new()),
        fetch_started: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        config_generation: generation_rx,
        tasks: BridgeTasks::default(),
        poller_slot: Arc::new(tokio::sync::Semaphore::new(1)),
    };
    let source = SourceHandle::spawn(bridge_source(), ctx);

    // 本物の Hub へは、キー付きの upgrade が届く。
    wait_until("the real hub upgrade", 10, || hub.bearer_upgrades() >= 1).await;
    let delivered = hub.bearer_upgrades();

    // Hub が死に（つながっていた WS が切れ）、別プロセスが先取りした。再試行は先取りへ向かい、
    // そのたびに pin で断られる（バックオフの 1 秒・2 秒を挟んで 3 回）。
    port.set(Mode::SquatTls(squatter.server_config()));
    wait_until("repeated redials of the squatter", 15, || {
        port.accepts(ModeKind::SquatTls) >= 3
    })
    .await;

    assert!(
        source.stop(Duration::from_secs(10)).await.is_ok(),
        "the source stops"
    );
    port.assert_secret_absent(DUMMY_KEY);
    assert_eq!(port.squat_tls_handshakes(), 0);
    assert!(port.squat_tls_bytes().is_empty());
    assert_eq!(hub.bearer_upgrades(), delivered);
}

#[tokio::test]
async fn a_renewed_certificate_needs_the_pin_rotated_and_never_leaks_before_that() {
    let identity_a = TestIdentity::generate();
    let identity_b = TestIdentity::generate();
    let hub_b = BackendHub::start(&identity_b).await;
    let port = SwitchPort::start().await;
    port.set(Mode::Relay(hub_b.addr));

    // pin が古い [A] のままだと、更新された証明書の Hub にも 1 件も要求を送らない。
    let stale = HubApi::new(&config_for(&port.https_url(), &[&identity_a])).unwrap();
    assert_rest_refused(&stale, "stale pin").await;
    assert_ws_refused(&ws_url(&port), &pins(&[&identity_a]), "stale pin ws").await;
    assert_eq!(hub_b.requests(), 0, "no request may reach the renewed Hub");
    assert_eq!(hub_b.bearer_requests(), 0);

    // 入れ替え用の 2 件（[A, B]）で通る。
    let rotation =
        HubApi::new(&config_for(&port.https_url(), &[&identity_a, &identity_b])).unwrap();
    for (index, result) in rest_calls(&rotation).await.into_iter().enumerate() {
        assert!(result.is_ok(), "rotation pair call {index}: {result:?}");
    }
    assert!(ws_is_ok(&ws_url(&port), &pins(&[&identity_a, &identity_b])).await);

    // 新しい証明書だけを pin しても通る。止まって戻っても、同じクライアントで回復する。
    let renewed = HubApi::new(&config_for(&port.https_url(), &[&identity_b])).unwrap();
    for (index, result) in rest_calls(&renewed).await.into_iter().enumerate() {
        assert!(result.is_ok(), "renewed pin call {index}: {result:?}");
    }
    assert!(ws_is_ok(&ws_url(&port), &pins(&[&identity_b])).await);
    let delivered = hub_b.bearer_requests();
    assert!(delivered >= 8);

    port.set(Mode::Down);
    assert_rest_refused(&renewed, "down").await;
    port.set(Mode::Relay(hub_b.addr));
    for (index, result) in rest_calls(&renewed).await.into_iter().enumerate() {
        assert!(result.is_ok(), "recovered call {index}: {result:?}");
    }
    assert!(hub_b.bearer_requests() > delivered);
}

#[tokio::test]
async fn redirects_are_never_followed_with_the_key() {
    let identity_a = TestIdentity::generate();
    let squatter = TestIdentity::generate();
    let hub = BackendHub::start(&identity_a).await;
    let port = SwitchPort::start().await;
    port.set(Mode::Relay(hub.addr));
    let api = HubApi::new(&config_for(&port.https_url(), &[&identity_a])).unwrap();

    let http_target = RecordingListener::start().await;
    let https_target = SwitchPort::start().await;
    https_target.set(Mode::SquatTls(squatter.server_config()));

    for status in [307u16, 302] {
        for location in [
            format!("{}/api/sink/config", http_target.http_url()),
            format!("{}/api/sink/config", https_target.https_url()),
        ] {
            hub.set_redirect(status, &location);
            let calls = [
                api.fetch_config().await.map(|_| ()),
                api.fetch_bridge_config().await.map(|_| ()),
                api.push_status(&empty_status()).await,
            ];
            for (index, result) in calls.into_iter().enumerate() {
                assert!(
                    matches!(result, Err(HubApiError::Status(code)) if code == status),
                    "{status} to {location} call {index}: {result:?}"
                );
            }
        }
    }

    assert_eq!(http_target.accepts(), 0, "plain redirect target was dialed");
    assert_eq!(https_target.accepts(ModeKind::SquatTls), 0);
    assert_eq!(https_target.accepts(ModeKind::SquatPlain), 0);
    http_target.assert_secret_absent(DUMMY_KEY);
    https_target.assert_secret_absent(DUMMY_KEY);
}

/// 子プロセスの中身。環境変数のプロキシが付いていても、REST も WebSocket もプロキシを
/// 通らないこと（プロキシの受けの数は親が数える）と、先取りへキーが届かないこと。
#[tokio::test]
#[ignore = "spawned by proxy_environment_is_ignored_by_rest_and_ingest_websocket"]
async fn proxy_child() {
    let identity_a = TestIdentity::generate();
    let squatter = TestIdentity::generate();
    let hub = BackendHub::start(&identity_a).await;
    let port = SwitchPort::start().await;
    port.set(Mode::Relay(hub.addr));
    let api = HubApi::new(&config_for(&port.https_url(), &[&identity_a])).unwrap();
    let pins_a = pins(&[&identity_a]);

    for (index, result) in rest_calls(&api).await.into_iter().enumerate() {
        assert!(
            result.is_ok(),
            "REST via the real hub, call {index}: {result:?}"
        );
    }
    assert!(ws_is_ok(&ws_url(&port), &pins_a).await);
    assert_eq!(
        hub.bearer_upgrades(),
        1,
        "the upgrade must reach the real hub"
    );
    assert!(port.accepts(ModeKind::Relay) >= 1);

    port.set(Mode::SquatTls(squatter.server_config()));
    assert_rest_refused(&api, "tls squat").await;
    assert_ws_refused(&ws_url(&port), &pins_a, "tls squat ws").await;
    port.assert_secret_absent(DUMMY_KEY);
    assert_eq!(port.squat_tls_handshakes(), 0);
    assert_no_http_reached(&port);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proxy_environment_is_ignored_by_rest_and_ingest_websocket() {
    let proxy = RecordingListener::start().await;
    // 受けの数が数えられていることの確認（子が足した分だけ増えない、を見るための基準）。
    drop(
        tokio::net::TcpStream::connect(proxy.addr())
            .await
            .expect("connect to the fake proxy"),
    );
    wait_until("the proxy to count the probe", 5, || proxy.accepts() == 1).await;

    let test_name = format!(
        "{}::proxy_child",
        module_path!().split_once("::").unwrap().1
    );
    let proxy_url = proxy.http_url();
    let output = tokio::task::spawn_blocking(move || {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", &test_name, "--ignored", "--nocapture"])
            .env_remove("NO_PROXY")
            .env_remove("no_proxy");
        for name in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            command.env(name, &proxy_url);
        }
        command.output().expect("spawn the child test")
    })
    .await
    .unwrap();
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success() && text.contains("1 passed"),
        "child test failed:\n{text}"
    );
    assert_eq!(proxy.accepts(), 1, "something went through the proxy");
    proxy.assert_secret_absent(DUMMY_KEY);
}

#[tokio::test]
async fn run_keeps_retrying_a_squatter_without_leaking_the_key() {
    let identity_a = TestIdentity::generate();
    let squatter = TestIdentity::generate();
    let hub = BackendHub::start(&identity_a).await;
    let port = SwitchPort::start().await;
    port.set(Mode::Relay(hub.addr));
    let config = config_for(&port.https_url(), &[&identity_a]);

    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(run(config, SidecarOptions::default(), async move {
        let _ = stop_rx.await;
        StopRequest::Graceful
    }));

    // 本物の Hub へは、設定の取得と状態の push がキー付きで届く。
    wait_until("config GET and status PUT on the real hub", 15, || {
        let heads = hub.heads.lock().unwrap();
        let with_key = |prefix: &str| {
            heads.iter().any(|h| {
                h.starts_with(prefix)
                    && h.to_ascii_lowercase().contains(&format!(
                        "authorization: bearer {}",
                        DUMMY_KEY.to_ascii_lowercase()
                    ))
            })
        };
        with_key("GET /api/sink/config ") && with_key("PUT /api/sink/status ")
    })
    .await;
    let delivered = hub.bearer_requests();
    assert!(delivered >= 2, "the real hub gets the key, got {delivered}");

    // Hub が止まって、別プロセスが先取りした。再試行（状態の push は 1 秒ごと、設定の取得は
    // バックオフ）が先取りへ何度も向かっても、pin で断られてキーは届かない。
    port.set(Mode::SquatTls(squatter.server_config()));
    wait_until("repeated retries against the squatter", 20, || {
        port.accepts(ModeKind::SquatTls) >= 5
    })
    .await;

    let _ = stop_tx.send(());
    let result = tokio::time::timeout(Duration::from_secs(30), task)
        .await
        .expect("run stops after the shutdown signal")
        .unwrap();
    assert!(result.is_ok());

    port.assert_secret_absent(DUMMY_KEY);
    assert_eq!(port.squat_tls_handshakes(), 0);
    assert!(port.squat_tls_bytes().is_empty());
    assert_no_http_reached(&port);
    // 切り替え時に本物へ向かっていた要求が後から記録されうる（設定の取得と状態の push は並行）
    // ので、本物側の件数の厳密な比較はしない。漏れの判定は先取り側の観測で行う。
}
