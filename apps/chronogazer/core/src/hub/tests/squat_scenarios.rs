//! #644 受入条件 4 の場面試験（ChronoGazer の Hub 購読: 見張り・世代・再 pin）。
//!
//! `hub.rs` の単体試験は「通信の失敗で再接続を続ける世代」「証明書の作り直しで張り直す」を
//! 1 つずつ見る。ここでは 1 つのポート（`SwitchPort`）が Hub の一生（動いている・
//! 別プロセスが先取り・証明書を作り直して再起動・loopback の http）を通るあいだ、
//! ダミーのキーが「本物の Hub」以外に 1 バイトも届かないことを、同じ `HubService`（見張りと
//! 世代つき）で確かめる。本物の Hub は TLS を話す模擬（`TlsHub`）で、届いた要求の頭を
//! すべて記録する。`chronogazer-server` も同じ `HubService`・`HubTlsPolicy::production()` を
//! 使う（`server_host::hub_tls_policy`）ので、別の通信の経路は無く、ここで併せて固める。

use std::net::SocketAddr;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use banto_hub_squat_harness::{Mode, ModeKind, SwitchPort, TestIdentity};
use banto_tagclient::{ValueQuality, ValuesSnapshot};
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::AbortHandle;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::Role;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

use super::*;

const DUMMY_KEY: &str = "bh_squatdummy.6f1c2a9e0d4b7381";
const TAG: &str = "alpha";
const WAIT: Duration = Duration::from_secs(20);

/// 差し替えられる証明書の読み口（`prepare-hub-tls --renew` に当たる）。中身は試験の
/// 本物の Hub が使う証明書の PEM なので、pin と Hub の証明書を一致／不一致にできる。
struct PemCell(StdMutex<String>);

impl PemCell {
    fn new(identity: &TestIdentity) -> Arc<Self> {
        Arc::new(Self(StdMutex::new(identity.pem())))
    }

    fn renew_to(&self, identity: &TestIdentity) {
        *self.0.lock().unwrap() = identity.pem();
    }
}

impl crate::hub_tls::HubCertReader for PemCell {
    fn read_cert_pem(&self) -> Result<Vec<u8>, String> {
        Ok(self.0.lock().unwrap().clone().into_bytes())
    }
}

/// 「本物の Hub」。TLS を話し、catalog・values・WebSocket（subscribe に data を 1 つ返す）に
/// 答える。届いた要求の頭を全部記録する。
struct TlsHub {
    addr: SocketAddr,
    heads: Arc<StdMutex<Vec<String>>>,
    task: AbortHandle,
}

impl TlsHub {
    async fn start(identity: &TestIdentity) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let heads = Arc::new(StdMutex::new(Vec::new()));
        let acceptor = TlsAcceptor::from(identity.server_config());
        let task_heads = Arc::clone(&heads);
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    continue;
                };
                tokio::spawn(serve(stream, acceptor.clone(), Arc::clone(&task_heads)));
            }
        })
        .abort_handle();
        Self { addr, heads, task }
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

impl Drop for TlsHub {
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

fn values_json() -> String {
    let catalog = catalog(&[TAG]);
    serde_json::to_string(&ValuesSnapshot {
        revision: catalog.revision,
        t: 1_000,
        run_id: catalog.run_id,
        collection_mode: catalog.collection_mode,
        values: vec![ValueEntry {
            tag: TAG.to_owned(),
            v: Some(1.0),
            q: ValueQuality::Good,
            t: 1_000,
            value_source: ValueSource::Real,
        }],
    })
    .unwrap()
}

async fn serve(
    stream: tokio::net::TcpStream,
    acceptor: TlsAcceptor,
    heads: Arc<StdMutex<Vec<String>>>,
) {
    let Ok(mut stream) = acceptor.accept(stream).await else {
        return;
    };
    let catalog_body = serde_json::to_string(&catalog(&[TAG])).unwrap();
    let values_body = values_json();
    while let Some(head) = read_head(&mut stream).await {
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
            if stream.write_all(response.as_bytes()).await.is_err() {
                return;
            }
            let mut socket = WebSocketStream::from_raw_socket(stream, Role::Server, None).await;
            let subscribe = tokio::time::timeout(Duration::from_secs(5), socket.next()).await;
            if !matches!(subscribe, Ok(Some(Ok(Message::Text(_))))) {
                return;
            }
            // 購読の ID は 1（`banto-tagclient` の `handle::SUBSCRIPTION_ID`）。
            let frame = format!(
                r#"{{"op":"data","id":1,"t":2000,"values":[{{"tag":"{TAG}","v":21.5,"q":"good","t":2000}}]}}"#
            );
            if socket.send(Message::Text(frame.into())).await.is_err() {
                return;
            }
            while let Some(message) = socket.next().await {
                match message {
                    Ok(Message::Close(reply)) => {
                        let _ = socket.send(Message::Close(reply)).await;
                        break;
                    }
                    Ok(Message::Text(text)) if text == r#"{"op":"ping"}"# => {
                        if socket
                            .send(Message::Text(r#"{"op":"pong"}"#.into()))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            return;
        }
        let body = if head.starts_with("GET /api/v1/values") {
            &values_body
        } else {
            &catalog_body
        };
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        if stream.write_all(response.as_bytes()).await.is_err() {
            return;
        }
    }
}

/// 保存済みのダミーキーと、`endpoint` を保存済みにした `HubService`（loopback は
/// `cert` の証明書を pin する本番の決まり）。購読の対象はタグ 1 つ。
async fn hub_for(endpoint: &str, cert: &Arc<PemCell>) -> HubService {
    use banto_hub_bootstrap::state::memory::MemoryKeyStore;

    let pool = init_db_memory().await.expect("init_db_memory");
    let settings = SettingsService::new(crate::db::Db::Sqlite(pool));
    let keys = Arc::new(MemoryKeyStore::new());
    keys.set(KEYRING_ACCOUNT, DUMMY_KEY).unwrap();
    let hub = HubService::new(
        settings.clone(),
        Arc::clone(&keys) as Arc<dyn KeyStore>,
        HubTlsPolicy::with_reader(Arc::clone(cert) as Arc<dyn crate::hub_tls::HubCertReader>),
    )
    .await
    .expect("HubService::new");
    let record = keyring_record(endpoint, &[]);
    settings
        .set(KEY_HUB_RECORD, &serde_json::to_string(&record).unwrap())
        .await
        .unwrap();
    hub.inner.mirror.reset(Some(record));
    register_for_test(&hub, &[TAG]).await;
    hub
}

async fn wait_for<F: FnMut() -> bool>(what: &str, mut cond: F) {
    tokio::time::timeout(WAIT, async {
        while !cond() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out: {what}"));
}

/// 世代の再接続（backoff は最大 30 秒まで伸びる）だけでは試験が長くなるので、設定画面の
/// 状態の確認（`status`、Hub の catalog を読みに行く）を挟んで、先取り役への接続を
/// `kind` で `count` 本になるまで繰り返す。確認は断られても何も保存・変更しない。
async fn reconnect_until(hub: &HubService, port: &SwitchPort, kind: ModeKind, count: usize) {
    tokio::time::timeout(WAIT, async {
        while port.accepts(kind) < count {
            let _ = hub.status().await;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out: {count} connections for {kind:?}"));
}

/// 購読の状態が `live` になるまで、見張りの周期（`supervise_once`）も回しながら待つ。
async fn wait_live(hub: &HubService, what: &str) {
    tokio::time::timeout(WAIT, async {
        loop {
            if hub.subscription().await.state == "live" {
                return;
            }
            hub.supervise_once().await;
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out: {what}"));
}

/// 先取り側に HTTP の要求（キーを運ぶもの）が 1 バイトも届いていないこと。平文の
/// 先取り役が受けるのは TLS の ClientHello だけで、HTTP の印は出ない。
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

/// 張れた世代が、Hub プロセスの死・先取りで通信の失敗の再接続に入っていること。
async fn assert_reconnecting_on_transport(hub: &HubService) {
    let view = hub.subscription().await;
    assert_ne!(view.state, "live", "{view:?}");
    assert_eq!(view.last_error.as_deref(), Some("transport"), "{view:?}");
}

/// 購読が成立していないこと（再接続中、または状態の確認が断られて世代ごと止まった）。
async fn assert_not_live(hub: &HubService) {
    let view = hub.subscription().await;
    assert!(
        matches!(view.state, "reconnecting" | "connecting" | "stopped"),
        "{view:?}"
    );
}

/// 本物の Hub で購読が成立し、キーは本物にだけ届いた状態まで進める。
async fn live_on(port: &SwitchPort, hub_a: &TlsHub, hub: &HubService) {
    port.set(Mode::Relay(hub_a.addr));
    let view = hub.status().await.expect("status");
    assert!(
        matches!(view.status, HubStatus::Connected { .. }),
        "{:?}",
        view.status
    );
    wait_live(hub, "live on the real hub").await;
    assert!(
        hub_a.bearer_requests() >= 2,
        "the real hub gets the key (catalog + stream), got {}",
        hub_a.bearer_requests()
    );
}

/// a. 接続・購読の最中に Hub が死んで別プロセスが同じポートを先取りしても（TLS でも平文でも）、
/// 見張りと世代の再接続は何度つないでも、キーは相手に届かない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_squatter_on_the_hub_port_never_receives_the_key_from_the_supervisor() {
    let identity_a = TestIdentity::generate();
    let squatter = TestIdentity::generate();
    let hub_a = TlsHub::start(&identity_a).await;
    let port = SwitchPort::start().await;
    let cert = PemCell::new(&identity_a);
    let hub = hub_for(&port.https_url(), &cert).await;
    live_on(&port, &hub_a, &hub).await;
    let delivered = hub_a.requests();

    // TLS の先取り: Hub プロセスが死んで別の証明書の相手が握った。
    port.set(Mode::SquatTls(squatter.server_config()));
    // まず世代自身の再接続（見張りも操作もなし）だけで 2 回、先取り役に当たる。
    wait_for("2 reconnects reach the TLS squatter on their own", || {
        port.accepts(ModeKind::SquatTls) >= 2
    })
    .await;
    assert_reconnecting_on_transport(&hub).await;
    reconnect_until(&hub, &port, ModeKind::SquatTls, 3).await;
    assert_eq!(
        port.squat_tls_handshakes(),
        0,
        "pin must reject the squatter"
    );
    assert_no_http_reached(&port);
    assert_not_live(&hub).await;
    // 見張りの 1 周期も、pin の証明書は同じなので先取り役へは何も出さない。
    hub.supervise_once().await;
    assert_eq!(port.squat_tls_handshakes(), 0);
    assert_no_http_reached(&port);

    // 平文の先取り: 次は TLS を話さない相手が握った。
    port.set(Mode::SquatPlain);
    reconnect_until(&hub, &port, ModeKind::SquatPlain, 3).await;
    assert_no_http_reached(&port);
    assert_not_live(&hub).await;

    port.assert_secret_absent(DUMMY_KEY);
    assert_eq!(
        hub_a.requests(),
        delivered,
        "the real hub was not reached while squatted"
    );
}

/// b. 証明書を作り直して Hub が再起動した。pin が古いうちは新しい Hub へも何も送らず、
/// 作り直しを見張りが読んだ後に、操作なしで張り直して新しい Hub にだけキーが届く。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_renewed_certificate_is_repinned_before_the_key_reaches_the_restarted_hub() {
    let identity_a = TestIdentity::generate();
    let identity_b = TestIdentity::generate();
    let squatter = TestIdentity::generate();
    let hub_a = TlsHub::start(&identity_a).await;
    let hub_b = TlsHub::start(&identity_b).await;
    let port = SwitchPort::start().await;
    let cert = PemCell::new(&identity_a);
    let hub = hub_for(&port.https_url(), &cert).await;
    live_on(&port, &hub_a, &hub).await;
    let delivered_to_a = hub_a.requests();

    // Hub が止まっている間、別プロセスが握る。
    port.set(Mode::SquatTls(squatter.server_config()));
    reconnect_until(&hub, &port, ModeKind::SquatTls, 2).await;
    assert_no_http_reached(&port);

    // 証明書を作り直した Hub が再起動したが、pin の読み口はまだ古い証明書（A）のまま。
    // 新しい Hub（B）へ何度つないでも TLS の確かめで断り、キーも要求も届かない。
    port.set(Mode::Relay(hub_b.addr));
    let before = port.accepts(ModeKind::Relay);
    reconnect_until(&hub, &port, ModeKind::Relay, before + 2).await;
    assert_eq!(hub_b.requests(), 0, "a stale pin must not talk to hub B");
    assert_eq!(hub_b.bearer_requests(), 0);
    assert_eq!(hub_a.requests(), delivered_to_a);

    // 管理者が `--renew` で作り直した（読み口が B になる）。操作なしで張り直す。
    cert.renew_to(&identity_b);
    assert_eq!(hub_b.bearer_requests(), 0, "nothing sent before the repin");
    wait_live(&hub, "repinned to hub B").await;
    assert!(
        hub_b.bearer_requests() >= 2,
        "hub B gets the key (catalog + stream) after the pin became B, got {}",
        hub_b.bearer_requests()
    );
    assert_eq!(hub_a.requests(), delivered_to_a, "hub A never saw it again");
    assert_eq!(port.squat_tls_handshakes(), 0);
    port.assert_secret_absent(DUMMY_KEY);
}

/// c. 保存済みの接続先が loopback の http なら、見張りも「接続」の操作も、接続を 1 本も
/// 開かずに断る（先取り役が平文で待っていても何も届かない）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_loopback_http_endpoint_is_refused_before_any_connection() {
    let identity_a = TestIdentity::generate();
    let port = SwitchPort::start().await;
    port.set(Mode::SquatPlain);
    let cert = PemCell::new(&identity_a);
    let hub = hub_for(&port.http_url(), &cert).await;

    let view = hub.status().await.expect("status");
    assert_eq!(
        view.status,
        HubStatus::Unreachable {
            cause: UnreachableCause::HubTls
        }
    );
    assert!(view.endpoint_problem.is_some());
    hub.supervise_once().await;
    hub.reconcile_with(&HubStatus::Connected { tag_count: 1 }, Trigger::Observe)
        .await;
    assert!(matches!(
        hub.connect(&port.http_url()).await,
        Err(BantoError::Validation { .. })
    ));
    assert_eq!(hub.subscription().await.state, "stopped");

    for kind in [
        ModeKind::Down,
        ModeKind::Relay,
        ModeKind::SquatTls,
        ModeKind::SquatPlain,
    ] {
        assert_eq!(
            port.accepts(kind),
            0,
            "no connection may be opened ({kind:?})"
        );
    }
    assert!(port.squat_bytes().is_empty());
    port.assert_secret_absent(DUMMY_KEY);
}
