//! #644 受入条件 4: プロキシの環境変数があっても、pin したクライアントはプロキシへ
//! つながず、キーも渡さない（`RestClient` は `.no_proxy()`、WebSocket は直接つなぐ）。
//!
//! 環境変数はプロセス全体に効くので、この試験だけを専用のバイナリ（このファイル）に
//! 分け、`#[test]` は 1 つだけにして、環境変数を設定したあとで tokio の実行環境を
//! 作る（`#[tokio::test]` は設定より先に実行環境ができてしまう）。

use std::net::SocketAddr;
use std::time::Duration;

use banto_hub_squat_harness::{Mode, ModeKind, RecordingListener, SwitchPort, TestIdentity};
use banto_tagclient::{BindingRequest, Endpoint, PinnedHubTls, RestClient, SecretApiKey};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

const DUMMY_KEY: &str = "bh_squatdummy.6f1c2a9e0d4b7381";
const CATALOG: &str = r#"{"revision":1,"run_id":2,"collection_mode":"configured","tags":[]}"#;

/// 本物の Hub の代わり。台帳だけに答える（接続ごとに 1 要求）。
async fn real_hub(identity: &TestIdentity) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let acceptor = TlsAcceptor::from(identity.server_config());
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                continue;
            };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(mut stream) = acceptor.accept(stream).await else {
                    return;
                };
                let mut raw = Vec::new();
                let mut buffer = [0u8; 1024];
                while !raw.windows(4).any(|window| window == b"\r\n\r\n") {
                    match stream.read(&mut buffer).await {
                        Ok(count) if count > 0 => raw.extend_from_slice(&buffer[..count]),
                        _ => return,
                    }
                }
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{CATALOG}",
                    CATALOG.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    addr
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
        let hub = real_hub(&identity).await;
        let port = SwitchPort::start().await;
        let client = client(&port.https_url(), &identity);

        // 本物の Hub へは、プロキシを通さず直接つながる。
        port.set(Mode::Relay(hub));
        assert!(client.fetch_catalog().await.is_ok());
        assert_eq!(proxy.accepts(), 0, "the proxy must not be dialed");
        assert!(port.accepts(ModeKind::Relay) >= 1);

        // 先取りされたら、プロキシ経由で回り込まず、断る。
        port.set(Mode::SquatTls(squatter.server_config()));
        for _ in 0..5 {
            assert!(client.fetch_catalog().await.is_err());
        }
        // WebSocket 側（監視役が再接続する）も、プロキシを使わず先取りを断る。
        let handle = client
            .start(vec![BindingRequest {
                binding_key: "a".to_owned(),
                external_name: "line1.tag".to_owned(),
            }])
            .unwrap();
        let waited = tokio::time::timeout(Duration::from_secs(10), async {
            while port.accepts(ModeKind::SquatTls) < 7 {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
        assert!(
            waited.is_ok(),
            "the worker should keep retrying the squatter"
        );
        let _ = handle.shutdown().await;

        assert_eq!(proxy.accepts(), 0, "the proxy must still not be dialed");
        proxy.assert_secret_absent(DUMMY_KEY);
        port.assert_secret_absent(DUMMY_KEY);
        assert_eq!(port.squat_tls_handshakes(), 0);
        assert!(port.squat_tls_bytes().is_empty());
    });
}
