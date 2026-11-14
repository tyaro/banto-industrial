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
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
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
    /// 受けた接続のうち、扱いが終わった（握手の失敗・相手が閉じた・切られた）数。
    finished: Arc<AtomicUsize>,
    task: AbortHandle,
}

impl TlsHub {
    async fn start(identity: &TestIdentity) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let heads = Arc::new(StdMutex::new(Vec::new()));
        let acceptor = TlsAcceptor::from(identity.server_config());
        let finished = Arc::new(AtomicUsize::new(0));
        let task_heads = Arc::clone(&heads);
        let task_finished = Arc::clone(&finished);
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    continue;
                };
                let serving = serve(stream, acceptor.clone(), Arc::clone(&task_heads));
                let finished = Arc::clone(&task_finished);
                tokio::spawn(async move {
                    serving.await;
                    finished.fetch_add(1, AtomicOrdering::SeqCst);
                });
            }
        })
        .abort_handle();
        Self {
            addr,
            heads,
            finished,
            task,
        }
    }

    /// 受けた接続が `at_least` 本、最後まで扱われるのを待つ。中継が「終わった」あとも
    /// この Hub は届いたバイトをまだ読んでいることがあるので、「要求が 1 つも
    /// 届いていない」を確かめる前に、Hub 自身が接続を読み終えたことを待つ。
    async fn wait_finished(&self, at_least: usize) {
        wait_for("the real hub to finish its connections", || {
            self.finished.load(AtomicOrdering::SeqCst) >= at_least
        })
        .await;
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

/// 購読の状態が、**手を加えずに** `live` になるまで待つ。見張りの周期（`supervise_once`）も
/// 状態の確認（`status`）もここからは呼ばない - 呼ぶと、自分で張れる・戻ることの確かめに
/// ならない（`subscription` はメモリ上の状態を読むだけで、Hub へは何も出さない）。
async fn wait_live_unaided(hub: &HubService, limit: Duration, what: &str) {
    tokio::time::timeout(limit, async {
        while hub.subscription().await.state != "live" {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out: {what}"));
}

/// 設定画面の状態の確認（`status`、Hub の catalog を読みに行く）を `times` 回する。先取り役が
/// 相手なら毎回「つながらない」になり、そのたびに実際に `kind` の相手へ掛けにいっている。
/// 戻る前に、先取り役がその接続を読み終えるのを待つ。
///
/// **断られた `status` は世代ごと止める**ので、世代自身の再接続を確かめたあとにだけ呼ぶ。
async fn refused_status_checks(hub: &HubService, port: &SwitchPort, kind: ModeKind, times: usize) {
    let before = port.accepts(kind);
    for _ in 0..times {
        let view = hub.status().await.expect("status");
        assert!(
            matches!(view.status, HubStatus::Unreachable { .. }),
            "{:?}",
            view.status
        );
    }
    port.settle(kind, before + times).await;
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

/// 張れた世代が、Hub プロセスの死・先取りで通信の失敗の再接続に入っていること（世代は
/// 止まっておらず、自分で再接続を続けている）。worker は試行の前に error なしの
/// `Connecting` を出すので、満たされるまで `WAIT` の範囲で待つ。
async fn assert_reconnecting_on_transport(hub: &HubService) {
    let mut last = hub.subscription().await;
    let ok = tokio::time::timeout(WAIT, async {
        loop {
            if last.state != "live" && last.last_error.as_deref() == Some("transport") {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
            last = hub.subscription().await;
        }
    })
    .await;
    if ok.is_err() {
        panic!("never reached a non-live state with last_error=transport: {last:?}");
    }
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
///
/// 製品の起動と同じ `resume()` で始める: 保存済みの接続先で購読を張り、**本物の見張り**
/// （`spawn_supervisor`、周期は [`SUPERVISOR_INTERVAL`]）を起動する。以後、試験は見張りを
/// 手で回さない。
async fn live_on(port: &SwitchPort, hub_a: &TlsHub, hub: &HubService) {
    port.set(Mode::Relay(hub_a.addr));
    hub.resume().await;
    wait_live_unaided(hub, WAIT, "live on the real hub").await;
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
    // 世代自身の再接続（見張りも操作もなし）だけで 2 回、先取り役に当たり、どちらも
    // 先取り役の側で最後まで扱われる（握手が断られて閉じる）のを待つ。TCP を受けた数は
    // 握手より先に増えるので、それで待つと、試行が済む前に記録を調べてしまう。
    port.wait_finished(ModeKind::SquatTls, 2, WAIT).await;
    assert_reconnecting_on_transport(&hub).await;
    assert_eq!(
        port.squat_tls_handshakes(),
        0,
        "pin must reject the squatter"
    );
    assert_no_http_reached(&port);

    // 平文の先取り: 次は TLS を話さない相手が握った。世代は止まっておらず、次の再接続が
    // 自分で平文の相手に当たる。
    port.set(Mode::SquatPlain);
    port.wait_finished(ModeKind::SquatPlain, 1, WAIT).await;
    assert_no_http_reached(&port);
    assert_not_live(&hub).await;

    // 設定画面の状態の確認（操作）も、平文・TLS のどちらの先取り役へもキーを運ばない。
    // 断られた確認は世代を止めるので、世代自身の再接続を確かめ終えたここで初めて呼ぶ。
    refused_status_checks(&hub, &port, ModeKind::SquatPlain, 2).await;
    assert_no_http_reached(&port);
    assert_not_live(&hub).await;
    port.set(Mode::SquatTls(squatter.server_config()));
    refused_status_checks(&hub, &port, ModeKind::SquatTls, 2).await;
    assert_eq!(port.squat_tls_handshakes(), 0);
    assert_no_http_reached(&port);
    assert_not_live(&hub).await;

    // 見張りの 1 周期を**手で**回す（ここだけ）。確かめたいのは「見張りが動いても先取り役へ
    // 何も出さない」で、自動で動くことではないので、30 秒の周期を待たずに 1 回分を起こす。
    // 周期が掛けにいった接続があれば、先取り役が読み終えてから記録を調べる。
    let before_tick = port.accepts(ModeKind::SquatTls);
    hub.supervise_once().await;
    // 周期が実際に掛けにいった（新しい接続が増えた）ことを要求してから、読み終えを待つ。
    port.settle(ModeKind::SquatTls, before_tick + 1).await;
    assert_eq!(port.squat_tls_handshakes(), 0);
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

    // Hub が止まっている間、別プロセスが握る。世代自身の再接続が先取り役に当たって
    // 断るのを待つ（`status` は呼ばない - 断られた確認は世代を止めてしまい、このあと
    // 確かめたい「再接続中の世代が、作り直された証明書に張り直される」経路でなくなる）。
    port.set(Mode::SquatTls(squatter.server_config()));
    port.wait_finished(ModeKind::SquatTls, 1, WAIT).await;
    assert_eq!(port.squat_tls_handshakes(), 0);
    assert_no_http_reached(&port);

    // 証明書を作り直した Hub が再起動したが、pin の読み口はまだ古い証明書（A）のまま。
    // 世代自身の再接続が新しい Hub（B）に当たっても TLS の確かめで断り、キーも要求も届かない。
    // 中継の本数の基準は、切り替える前（中継が新しく増えない間）に取る。
    let relayed_before = port.accepts(ModeKind::Relay);
    port.set(Mode::Relay(hub_b.addr));
    port.wait_finished(ModeKind::Relay, relayed_before + 1, WAIT)
        .await;
    hub_b.wait_finished(1).await;
    assert_eq!(hub_b.requests(), 0, "a stale pin must not talk to hub B");
    assert_eq!(hub_a.requests(), delivered_to_a);
    // 世代は止まっておらず、通信の失敗で再接続を続けている（#644 の張り直しの前提）。
    assert_reconnecting_on_transport(&hub).await;

    // 管理者が `--renew` で作り直した（読み口が B になる）。ここから先は何も操作しない:
    // `live_on` の `resume()` が起動した本物の見張りが、次の周期で証明書を読み直して
    // 張り直すのを待つ。周期を縮める試験用の口は無いので、実際の周期（30 秒）を待つ。
    cert.renew_to(&identity_b);
    wait_live_unaided(
        &hub,
        SUPERVISOR_INTERVAL * 2 + WAIT,
        "repinned to hub B by the supervisor on its own",
    )
    .await;
    assert!(
        hub_b.bearer_requests() >= 2,
        "hub B gets the key (catalog + stream) after the pin became B, got {}",
        hub_b.bearer_requests()
    );
    assert_eq!(hub_a.requests(), delivered_to_a, "hub A never saw it again");
    assert_eq!(port.squat_tls_handshakes(), 0);
    port.assert_secret_absent(DUMMY_KEY);
    assert_no_http_reached(&port);
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
    // 見張りの 1 周期を手で回す: 確かめたいのは「周期が来ても接続を開かない」なので、
    // 周期を待たずに 1 回分を起こす（どの呼び出しも戻ってから数を調べる）。
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
