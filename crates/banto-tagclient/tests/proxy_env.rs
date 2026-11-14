//! #644 受入条件 4: プロキシの環境変数があっても、pin したクライアントはプロキシへ
//! つながず、キーも渡さない（`RestClient` は `.no_proxy()`、WebSocket は直接つなぐ）。
//! 本物の Hub（試験用）で WebSocket の握手が実際に起きることを確かめたうえで、
//! 先取りに切り替えて断ることを見る。
//!
//! 環境変数はプロセス全体に効くので、この試験だけを専用のバイナリ（このファイル）に
//! 分け、`#[test]` は 1 つだけにして、環境変数を設定したあとで tokio の実行環境を
//! 作る（`#[tokio::test]` は設定より先に実行環境ができてしまう）。

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use banto_hub_squat_harness::{Mode, ModeKind, RecordingListener, SwitchPort, TestIdentity};
use banto_tagclient::{BindingRequest, Endpoint, PinnedHubTls, RestClient, SecretApiKey};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;

const DUMMY_KEY: &str = "bh_squatdummy.6f1c2a9e0d4b7381";
const CATALOG: &str = r#"{"revision":1,"run_id":2,"collection_mode":"configured","tags":[]}"#;

/// 本物の Hub の代わり。台帳（接続ごとに 1 要求）と WebSocket の握手に答え、
/// 受けた要求の先頭部分をすべて `heads` に記録する。
async fn real_hub(identity: &TestIdentity) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let acceptor = TlsAcceptor::from(identity.server_config());
    let heads = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&heads);
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                continue;
            };
            let acceptor = acceptor.clone();
            let heads = Arc::clone(&recorded);
            tokio::spawn(async move {
                let Ok(mut stream) = acceptor.accept(stream).await else {
                    return;
                };
                if let Some(head) = read_head(&mut stream).await {
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
                        let _ = stream.flush().await;
                        // 握手のあとは、クライアントが閉じるまで開けておく。
                        let mut sink = [0u8; 256];
                        while matches!(stream.read(&mut sink).await, Ok(count) if count > 0) {}
                        return;
                    }
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{CATALOG}",
                        CATALOG.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.shutdown().await;
                }
            });
        }
    });
    (addr, heads)
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

/// 本物の Hub が、このキーを Bearer に載せた WebSocket の握手を受けた回数。
fn websocket_upgrades(heads: &Mutex<Vec<String>>) -> usize {
    let bearer = format!("Bearer {DUMMY_KEY}").to_ascii_lowercase();
    heads
        .lock()
        .unwrap()
        .iter()
        .filter(|head| {
            let lower = head.to_ascii_lowercase();
            lower.contains("upgrade: websocket") && lower.contains(&bearer)
        })
        .count()
}

fn client(url: &str, identity: &TestIdentity) -> RestClient {
    RestClient::with_pinned_tls(
        Endpoint::new(url).unwrap(),
        SecretApiKey::new(DUMMY_KEY.to_owned()).unwrap(),
        Duration::from_secs(3),
        PinnedHubTls::from_pem(&[identity.pem()]).unwrap(),
    )
    .unwrap()
}

#[test]
fn proxy_environment_variables_are_ignored_and_the_key_never_goes_to_the_proxy() {
    // 先に、プロキシが「ある」状態を作る。NO_PROXY は設定しない（loopback が
    // 除外されて見かけ上通る、という抜け道を作らない）。ポートだけ先に開いておく。
    let proxy_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy_url = format!("http://{}", proxy_listener.local_addr().unwrap());
    for name in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        std::env::set_var(name, &proxy_url);
    }
    for name in ["NO_PROXY", "no_proxy"] {
        std::env::remove_var(name);
    }

    // 環境変数を設定したあとで、試験本体の実行環境を作る。
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let proxy = RecordingListener::from_std(proxy_listener);
        let identity = TestIdentity::generate();
        let squatter = TestIdentity::generate();
        let (hub, hub_heads) = real_hub(&identity).await;
        let port = SwitchPort::start().await;
        let client = client(&port.https_url(), &identity);
        // `start` はクライアントを消費するので、監視役用に同じ設定をもう 1 つ作る。
        let worker_client = self::client(&port.https_url(), &identity);

        // 本物の Hub へは、プロキシを通さず直接つながる。
        port.set(Mode::Relay(hub));
        assert!(client.fetch_catalog().await.is_ok());
        assert_eq!(proxy.accepts(), 0, "the proxy must not be dialed");
        assert!(port.accepts(ModeKind::Relay) >= 1);

        // WebSocket の経路も、プロキシの環境変数があるまま本物の Hub へ直接つなぐ。
        // 監視役は台帳を取ったあとに `connect_stream()` へ進むので（空の台帳でも
        // 進む）、Hub が握手の要求を記録するのは WebSocket の接続を試みた証拠。
        // ワーカーが一度も `connect_stream()` を呼ばなければ、ここで時間切れになって
        // 試験が落ちる。
        let handle = worker_client
            .start(vec![BindingRequest {
                binding_key: "a".to_owned(),
                external_name: "line1.tag".to_owned(),
            }])
            .unwrap();
        let upgraded = tokio::time::timeout(Duration::from_secs(10), async {
            while websocket_upgrades(&hub_heads) == 0 {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
        assert!(
            upgraded.is_ok(),
            "the worker should reach the real hub's WebSocket upgrade with the key"
        );
        assert_eq!(
            proxy.accepts(),
            0,
            "the WebSocket must not be dialed via the proxy"
        );

        // 先取りされたら、プロキシ経由で回り込まず、断る。
        port.set(Mode::SquatTls(squatter.server_config()));
        let baseline = port.finished(ModeKind::SquatTls);
        for _ in 0..5 {
            assert!(client.fetch_catalog().await.is_err());
        }
        // 監視役の再接続（台帳の取得）も、プロキシを使わず先取りを断る。受けた数では
        // なく、先取り側で最後まで扱われた（握手が断られて閉じた）接続の数で待つ。
        port.wait_finished(ModeKind::SquatTls, baseline + 7, Duration::from_secs(10))
            .await;
        let _ = handle.shutdown().await;
        // 監視役が止まったあと、受けた接続をすべて読み終えてから記録を調べる。
        port.settle(ModeKind::SquatTls, baseline + 7).await;

        assert_eq!(proxy.accepts(), 0, "the proxy must still not be dialed");
        proxy.assert_secret_absent(DUMMY_KEY);
        port.assert_secret_absent(DUMMY_KEY);
        assert_eq!(port.squat_tls_handshakes(), 0);
        assert!(port.squat_tls_bytes().is_empty());
    });
}
