//! #644 受入条件 4 の場面試験（REST + WebSocket、リダイレクト）。
//!
//! `pinned_tls.rs` の単体試験は「間違った pin なら 1 回断る」を 1 つずつ見る。ここでは
//! 1 つのポート（`SwitchPort`）が Hub の一生（動いている・止まる・別プロセスが先取り・
//! 入れ替わる・戻る・証明書が更新される）を通るあいだ、ダミーのキーが「本物の Hub」
//! 以外に 1 バイトも届かないことを、同じクライアントを使い回して確かめる。
//!
//! `connect_stream` は crate 内だけの口なので、WebSocket を直接呼べるこの場所
//! （crate 内の試験モジュール）に置く。プロキシの環境変数は別プロセスが要るので
//! `tests/proxy_env.rs` に分けた。

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use banto_hub_squat_harness::{Mode, ModeKind, RecordingListener, SwitchPort, TestIdentity};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::AbortHandle;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;

use crate::{BindingRequest, Endpoint, ErrorKind, PinnedHubTls, RestClient, SecretApiKey};

const DUMMY_KEY: &str = "bh_squatdummy.6f1c2a9e0d4b7381";
const CATALOG: &str = r#"{"revision":1,"run_id":2,"collection_mode":"configured","tags":[]}"#;
/// 先取りされた側へ何度つなぎ直しても同じ結果になることを見る回数。
const ATTEMPTS: usize = 5;

/// 「本物の Hub」。自分のポートで TLS を話し、REST の台帳と WebSocket の握手に答える。
/// 届いた要求の頭（ヘッダー）をすべて記録するので、本物にはキーが届くことも確かめられる。
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

    /// ダミーのキーを Bearer に載せた WebSocket の握手を受けた回数。
    fn websocket_upgrades(&self) -> usize {
        let needle = format!("Bearer {DUMMY_KEY}").to_ascii_lowercase();
        self.heads
            .lock()
            .unwrap()
            .iter()
            .filter(|head| {
                let lower = head.to_ascii_lowercase();
                lower.contains("upgrade: websocket") && lower.contains(&needle)
            })
            .count()
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
}

impl Drop for BackendHub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn read_head<S: AsyncReadExt + Unpin>(stream: &mut S) -> Option<String> {
    let mut raw = Vec::new();
    let mut buffer = [0u8; 1024];
    loop {
        let count = stream.read(&mut buffer).await.ok()?;
        if count == 0 {
            return None;
        }
        raw.extend_from_slice(&buffer[..count]);
        if raw.windows(4).any(|window| window == b"\r\n\r\n") {
            return Some(String::from_utf8_lossy(&raw).into_owned());
        }
    }
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
    // keep-alive: 同じ接続で来る次の要求にも答える（クライアントの接続の使い回しを試す）。
    while let Some(head) = read_head(&mut stream).await {
        heads.lock().unwrap().push(head.clone());
        let lower = head.to_ascii_lowercase();
        if lower.contains("upgrade: websocket") {
            // リダイレクトを設定してある間は、WebSocket の握手にも同じ向き先を返す。
            let configured = redirect.lock().unwrap().clone();
            if let Some((status, location)) = configured {
                let response = format!(
                    "HTTP/1.1 {status} Redirect

Location: {location}

Content-Length: 0



"
                );
                if stream.write_all(response.as_bytes()).await.is_err() {
                    return;
                }
                continue;
            }
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
            // 握手のあとは、クライアントが閉じるまで開けておく。
            let mut sink = [0u8; 256];
            while matches!(stream.read(&mut sink).await, Ok(count) if count > 0) {}
            return;
        }
        let configured = redirect.lock().unwrap().clone();
        let response = if let Some((status, location)) = configured {
            format!(
                "HTTP/1.1 {status} Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\n\r\n"
            )
        } else if head.starts_with("GET /api/v1/tags ") {
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{CATALOG}",
                CATALOG.len()
            )
        } else {
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_owned()
        };
        if stream.write_all(response.as_bytes()).await.is_err() {
            return;
        }
    }
}

fn client(url: &str, identities: &[&TestIdentity]) -> RestClient {
    let pems: Vec<String> = identities.iter().map(|identity| identity.pem()).collect();
    RestClient::with_pinned_tls(
        Endpoint::new(url).unwrap(),
        SecretApiKey::new(DUMMY_KEY.to_owned()).unwrap(),
        Duration::from_secs(3),
        PinnedHubTls::from_pem(&pems).unwrap(),
    )
    .unwrap()
}

async fn catalog_ok(client: &RestClient) -> bool {
    client.fetch_catalog().await.is_ok()
}

async fn stream_ok(client: &RestClient) -> bool {
    tokio::time::timeout(Duration::from_secs(6), client.connect_stream())
        .await
        .is_ok_and(|result| result.is_ok())
}

/// 先取りされている間は、何度やっても REST も WebSocket も失敗する。
async fn assert_always_refused(client: &RestClient) {
    for attempt in 0..ATTEMPTS {
        assert!(
            !catalog_ok(client).await,
            "REST attempt {attempt} must be refused"
        );
        assert!(
            !stream_ok(client).await,
            "WebSocket attempt {attempt} must be refused"
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
    for marker in ["http/1.", "authorization", "bearer", "get /", "upgrade:"] {
        assert!(
            !seen.contains(marker),
            "an HTTP request reached a squatter ({marker})"
        );
    }
}

#[tokio::test]
async fn a_hub_that_goes_down_and_is_squatted_never_receives_the_key() {
    let identity_a = TestIdentity::generate();
    let squatter = TestIdentity::generate();
    let hub = BackendHub::start(&identity_a).await;
    let port = SwitchPort::start().await;
    let client = client(&port.https_url(), &[&identity_a]);

    // 本物の Hub が動いている間は通り、キーは本物にだけ届く。
    port.set(Mode::Relay(hub.addr));
    assert!(catalog_ok(&client).await);
    assert!(stream_ok(&client).await);
    assert!(hub.bearer_requests() >= 2);
    let delivered = hub.bearer_requests();

    // 止まって、別の証明書の TLS が同じポートを先取りした。
    port.set(Mode::SquatTls(squatter.server_config()));
    assert_always_refused(&client).await;
    assert!(port.accepts(ModeKind::SquatTls) >= ATTEMPTS);

    // 証明書もない平文が先取りした。
    port.set(Mode::SquatPlain);
    assert_always_refused(&client).await;
    assert!(port.accepts(ModeKind::SquatPlain) >= ATTEMPTS);

    port.assert_secret_absent(DUMMY_KEY);
    assert_eq!(port.squat_tls_handshakes(), 0, "pin must stop the TLS");
    assert!(port.squat_tls_bytes().is_empty());
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
    let client = client(&port.https_url(), &[&identity_a]);

    // 1 回目（確認）は本物に通る。接続は使い回しのために残る。
    port.set(Mode::Relay(hub.addr));
    assert!(catalog_ok(&client).await);

    // 確認のあと送る前に、待受が先取りに入れ替わった。
    port.set(Mode::SquatTls(squatter.server_config()));
    assert!(!catalog_ok(&client).await, "2nd REST on the same client");
    assert!(!stream_ok(&client).await, "WebSocket on the same client");
    // 使い回していた接続が切れた最初の失敗のあと、つなぎ直す要求も断られる
    // （つなぎ直しのたびに pin を確かめ直す）。
    assert!(!catalog_ok(&client).await, "REST redial on the same client");
    assert!(
        port.accepts(ModeKind::SquatTls) >= 2,
        "the client really dialed the squatter and refused it"
    );

    // 平文への入れ替わりも同じ。
    port.set(Mode::SquatPlain);
    assert!(!catalog_ok(&client).await);
    assert!(!stream_ok(&client).await);

    port.assert_secret_absent(DUMMY_KEY);
    assert_no_http_reached(&port);
}

#[tokio::test]
async fn a_swap_between_the_check_and_the_send_does_not_leak_on_a_fresh_client() {
    let identity_a = TestIdentity::generate();
    let squatter = TestIdentity::generate();
    let hub = BackendHub::start(&identity_a).await;
    let port = SwitchPort::start().await;

    port.set(Mode::Relay(hub.addr));
    assert!(catalog_ok(&client(&port.https_url(), &[&identity_a])).await);

    port.set(Mode::SquatTls(squatter.server_config()));
    let fresh = client(&port.https_url(), &[&identity_a]);
    assert!(!catalog_ok(&fresh).await);
    assert!(!stream_ok(&fresh).await);

    port.assert_secret_absent(DUMMY_KEY);
    assert!(port.squat_bytes().is_empty());
    assert_eq!(port.squat_tls_handshakes(), 0);
}

#[tokio::test]
async fn a_websocket_that_drops_does_not_reconnect_to_a_squatter_with_the_key() {
    let identity_a = TestIdentity::generate();
    let squatter = TestIdentity::generate();
    let hub = BackendHub::start(&identity_a).await;
    let port = SwitchPort::start().await;
    let client = client(&port.https_url(), &[&identity_a]);

    port.set(Mode::Relay(hub.addr));
    let live = client.connect_stream().await;
    assert!(live.is_ok(), "the real Hub accepts the stream");
    let delivered = hub.bearer_requests();
    assert_eq!(delivered, 1);

    // Hub が死んだ（つながっていた接続が切れる）あとに別プロセスが先取りした。
    port.set(Mode::SquatTls(squatter.server_config()));
    for attempt in 0..ATTEMPTS {
        assert!(!stream_ok(&client).await, "reconnect {attempt}");
    }
    port.set(Mode::SquatPlain);
    for attempt in 0..ATTEMPTS {
        assert!(!stream_ok(&client).await, "plain reconnect {attempt}");
    }

    port.assert_secret_absent(DUMMY_KEY);
    assert_no_http_reached(&port);
    drop(live);
}

#[tokio::test]
async fn recovery_and_a_renewed_certificate_need_the_new_pin_and_never_leak_to_the_old_one() {
    let identity_a = TestIdentity::generate();
    let identity_b = TestIdentity::generate();
    let hub_a = BackendHub::start(&identity_a).await;
    let hub_b = BackendHub::start(&identity_b).await;
    let port = SwitchPort::start().await;

    // 証明書が更新されて Hub が B で戻ったが、クライアントはまだ A だけを pin している。
    port.set(Mode::Relay(hub_b.addr));
    let stale = client(&port.https_url(), &[&identity_a]);
    assert!(!catalog_ok(&stale).await);
    assert!(!stream_ok(&stale).await);
    assert_eq!(hub_b.requests(), 0, "no request may reach the renewed Hub");
    port.assert_secret_absent(DUMMY_KEY);

    // 切り替えの対（A と B）、または B だけを pin すれば戻れる。キーは本物にだけ届く。
    assert!(catalog_ok(&client(&port.https_url(), &[&identity_a, &identity_b])).await);
    assert!(stream_ok(&client(&port.https_url(), &[&identity_a, &identity_b])).await);
    assert!(catalog_ok(&client(&port.https_url(), &[&identity_b])).await);
    assert!(hub_b.bearer_requests() >= 3);

    // 同じクライアントのまま、停止 → 復帰で元に戻る。
    let client = client(&port.https_url(), &[&identity_a]);
    port.set(Mode::Relay(hub_a.addr));
    assert!(catalog_ok(&client).await);
    port.set(Mode::Down);
    assert!(!catalog_ok(&client).await);
    assert!(!stream_ok(&client).await);
    port.set(Mode::Relay(hub_a.addr));
    assert!(catalog_ok(&client).await);
    assert!(stream_ok(&client).await);
}

#[tokio::test]
async fn a_redirect_from_the_hub_is_never_followed_to_another_listener() {
    let identity_a = TestIdentity::generate();
    let squatter = TestIdentity::generate();
    let hub = BackendHub::start(&identity_a).await;
    let port = SwitchPort::start().await;
    let plain_target = RecordingListener::start().await;
    let tls_target = SwitchPort::start().await;
    tls_target.set(Mode::SquatTls(squatter.server_config()));
    let plain_target_2 = SwitchPort::start().await;
    plain_target_2.set(Mode::SquatPlain);

    port.set(Mode::Relay(hub.addr));
    let client = client(&port.https_url(), &[&identity_a]);
    let locations = [
        format!("{}/stolen", plain_target.http_url()),
        format!("{}/stolen", tls_target.https_url()),
        format!("{}/stolen", plain_target_2.http_url()),
    ];
    for status in [307, 302] {
        for location in &locations {
            hub.set_redirect(status, location);
            assert!(
                !catalog_ok(&client).await,
                "{status} to {location} must be an error"
            );
            // 握手の 3xx はリダイレクトとして分類され（追わずに InvalidEndpoint）、
            // 形の崩れた返事による失敗ではないことを種類で確かめる。
            let error = tokio::time::timeout(Duration::from_secs(6), client.connect_stream())
                .await
                .expect("WebSocket handshake must finish")
                .err()
                .expect("WebSocket redirect must be an error");
            assert_eq!(
                error.kind(),
                ErrorKind::InvalidEndpoint,
                "WebSocket {status} to {location} must be refused as a redirect"
            );
        }
    }

    assert_eq!(plain_target.accepts(), 0);
    assert_eq!(tls_target.accepts(ModeKind::SquatTls), 0);
    assert_eq!(plain_target_2.accepts(ModeKind::SquatPlain), 0);
    plain_target.assert_secret_absent(DUMMY_KEY);
    tls_target.assert_secret_absent(DUMMY_KEY);
    plain_target_2.assert_secret_absent(DUMMY_KEY);
}

/// 監視役（ワーカー）が自分で張り直す経路。つないでいた WebSocket が先取りで切れても、
/// 自動の再接続は先取り側へキーを運ばず、本物が戻れば手を加えずに回復する。
#[tokio::test]
async fn the_worker_reconnects_by_itself_never_to_a_squatter_and_recovers_on_the_real_hub() {
    let identity_a = TestIdentity::generate();
    let squatter = TestIdentity::generate();
    let hub = BackendHub::start(&identity_a).await;
    let port = SwitchPort::start().await;
    // `start` はクライアントを消費する。台帳が空でもワーカーは WebSocket を開く。
    let handle = client(&port.https_url(), &[&identity_a])
        .start(vec![BindingRequest {
            binding_key: "a".to_owned(),
            external_name: "line1.tag".to_owned(),
        }])
        .unwrap();
    port.set(Mode::Relay(hub.addr));

    async fn wait_for(what: &str, limit: Duration, mut done: impl FnMut() -> bool) {
        let waited = tokio::time::timeout(limit, async {
            while !done() {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
        assert!(waited.is_ok(), "timed out waiting for {what}");
    }

    wait_for(
        "the first WebSocket upgrade",
        Duration::from_secs(10),
        || hub.websocket_upgrades() >= 1,
    )
    .await;
    let delivered = hub.websocket_upgrades();

    // つながっている WebSocket ごと、別の証明書の TLS に入れ替わる。
    port.set(Mode::SquatTls(squatter.server_config()));
    wait_for(
        "the worker's own reconnects to the squatter",
        Duration::from_secs(12),
        || port.accepts(ModeKind::SquatTls) >= 2,
    )
    .await;
    assert_eq!(port.squat_tls_handshakes(), 0, "pin must stop the TLS");
    port.assert_secret_absent(DUMMY_KEY);
    assert_no_http_reached(&port);
    assert_eq!(
        hub.websocket_upgrades(),
        delivered,
        "the real Hub got no new upgrade while squatted"
    );

    // 本物が戻る。ワーカーが自分で張り直して、新しい握手が本物に届く。
    port.set(Mode::Relay(hub.addr));
    wait_for(
        "the worker to recover by itself",
        Duration::from_secs(15),
        || hub.websocket_upgrades() > delivered,
    )
    .await;
    let _ = handle.shutdown().await;

    assert_eq!(port.squat_tls_handshakes(), 0);
    assert_no_http_reached(&port);
}
