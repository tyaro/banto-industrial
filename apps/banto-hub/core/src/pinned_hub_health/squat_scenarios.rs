//! #644 受入条件 4 の場面試験（Hub シェルの Service モードの確かめ）。
//!
//! この probe 自身は秘密を送らない（認証のない openapi を取るだけ）。ただしシェルは、
//! パスワード・セッションを運ぶ WebView2 を、この probe が `Healthy` と言った相手にしか
//! 向けない。つまり probe の判定が門で、先取り・入れ替え・再起動・証明書の作り直しのどの
//! 場面でも、**pin と完全に一致する本物の Hub のときだけ `Healthy`** になり、そうでない相手へは
//! 1 バイトの要求も出ないことが守りの全部になる。`tests.rs` の表は 1 つずつの場面を見る。
//! ここでは 1 つのポート（`SwitchPort`）が Hub の一生を通るあいだ、同じ probe で繰り返し確かめる。

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use banto_hub_squat_harness::{Mode, ModeKind, SwitchPort, TestIdentity};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::AbortHandle;
use tokio_rustls::TlsAcceptor;

use super::*;
use crate::profile_lock::lock_file_path;
use crate::profile_paths::resolve_profile_paths;
use crate::test_support::TempDir;

const OPENAPI: &str = r#"{"openapi":"3.1.0","info":{"title":"banto-hub","version":"v1","x-banto-hub-profile-id":"default"},"paths":{}}"#;
/// 先取りされた側へ何度つなぎ直しても同じ結果になることを見る回数。
const ATTEMPTS: usize = 3;

fn ok_response(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// 何度でも受ける、openapi を返す TLS の「本物の Hub」。届いた要求の頭を記録する。
struct TlsHub {
    addr: SocketAddr,
    heads: Arc<Mutex<Vec<String>>>,
    task: AbortHandle,
}

impl TlsHub {
    async fn start(identity: &TestIdentity) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let heads = Arc::new(Mutex::new(Vec::new()));
        let acceptor = TlsAcceptor::from(identity.server_config());
        let task_heads = Arc::clone(&heads);
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    continue;
                };
                let acceptor = acceptor.clone();
                let heads = Arc::clone(&task_heads);
                tokio::spawn(async move {
                    let Ok(mut stream) = acceptor.accept(stream).await else {
                        return;
                    };
                    let mut raw = Vec::new();
                    let mut chunk = [0u8; 1024];
                    while !raw.windows(4).any(|window| window == b"\r\n\r\n") {
                        match stream.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(count) => raw.extend_from_slice(&chunk[..count]),
                        }
                    }
                    heads
                        .lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(&raw).into_owned());
                    let _ = stream.write_all(ok_response(OPENAPI).as_bytes()).await;
                    let _ = stream.shutdown().await;
                });
            }
        })
        .abort_handle();
        Self { addr, heads, task }
    }

    fn requests(&self) -> usize {
        self.heads.lock().unwrap().len()
    }
}

impl Drop for TlsHub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// 平文の口（期待ポート）に誰かいる状態を保つ、何度でも受ける素朴な相手。
struct PlainPort {
    port: u16,
    task: AbortHandle,
}

impl PlainPort {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    continue;
                };
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let _ = socket.read(&mut buf).await;
                    let _ = socket.write_all(ok_response(OPENAPI).as_bytes()).await;
                });
            }
        })
        .abort_handle();
        Self { port, task }
    }
}

impl Drop for PlainPort {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// 先取り側に HTTP の要求が 1 バイトも届いていないこと。平文の先取り役が受けるのは
/// TLS の ClientHello だけで、HTTP の印は出ない。
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

/// `pin` は差し替えられる pin の PEM（`prepare-hub-tls --renew` に当たる）。
fn probe_with(
    root: &TempDir,
    tls_port: u16,
    plain_host: &str,
    pin: &Arc<Mutex<String>>,
) -> Arc<PinnedHubHealthProbe> {
    let pin = Arc::clone(pin);
    Arc::new(PinnedHubHealthProbe::with_timeout(
        root.path().to_path_buf(),
        plain_host.to_owned(),
        tls_port,
        Box::new(move || Ok(pin.lock().unwrap().clone().into_bytes())),
        Duration::from_millis(1000),
    ))
}

/// 本体は同期の probe なので、ランタイム（`SwitchPort` のタスク）を止めない別スレッドで呼ぶ。
async fn run(probe: &Arc<PinnedHubHealthProbe>, plain_port: u16) -> HealthOutcome {
    let probe = Arc::clone(probe);
    tokio::task::spawn_blocking(move || probe.probe("default", plain_port))
        .await
        .unwrap()
        .expect("probe")
}

/// `profile.lock` の診断ファイルを直に置く（排他の取得はこの PC で main でも落ちることが
/// あるので使わない。`Healthy` の確かめが読むのはこのファイルだけ）。
fn place_lock_file(root: &TempDir) {
    let paths = resolve_profile_paths(root.path(), "default").unwrap();
    let lock_path = lock_file_path(&paths);
    std::fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    std::fs::write(
        &lock_path,
        r#"{"pid":7,"host_kind":"service","acquired_at_unix_ms":1}"#,
    )
    .unwrap();
}

fn healthy() -> HealthOutcome {
    HealthOutcome::Healthy {
        version: "v1".to_string(),
    }
}

/// Hub の一生（動く・死ぬ・先取り・戻る）を 1 つのポートで通し、同じ probe が本物の
/// ときだけ `Healthy` を返し、先取り役へは何も出さないこと。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_gate_opens_only_for_the_real_hub_through_its_whole_lifecycle() {
    let identity_a = TestIdentity::generate();
    let squatter = TestIdentity::generate();
    let hub_a = TlsHub::start(&identity_a).await;
    let plain = PlainPort::start().await;
    let port = SwitchPort::start().await;
    let root = TempDir::new("pinned-hub-health-squat-lifecycle");
    place_lock_file(&root);
    let pin = Arc::new(Mutex::new(identity_a.pem()));
    let probe = probe_with(&root, port.addr().port(), "127.0.0.1", &pin);

    // 本物の Hub が動いている間は通る（要求は本物にだけ届く）。
    port.set(Mode::Relay(hub_a.addr));
    assert_eq!(run(&probe, plain.port).await, healthy());
    assert_eq!(hub_a.requests(), 1);

    // Hub が止まって別の証明書の相手が握る。何度確かめても通らず、要求も出ない。
    port.set(Mode::SquatTls(squatter.server_config()));
    for _ in 0..ATTEMPTS {
        assert_eq!(run(&probe, plain.port).await, HealthOutcome::TlsUntrusted);
    }
    assert!(port.accepts(ModeKind::SquatTls) >= ATTEMPTS);
    assert_eq!(
        port.squat_tls_handshakes(),
        0,
        "pin must reject the squatter"
    );
    assert_no_http_reached(&port);

    // TLS を話さない平文の相手が握る。
    port.set(Mode::SquatPlain);
    for _ in 0..ATTEMPTS {
        assert_eq!(run(&probe, plain.port).await, HealthOutcome::TlsUntrusted);
    }
    assert!(port.accepts(ModeKind::SquatPlain) >= ATTEMPTS);
    assert_no_http_reached(&port);

    // 受けてすぐ閉じる（Hub の口が壊れている・何もいない）。平文の口には誰かいるので、
    // 「消えた」にはならず TLS の側の理由が返る。
    port.set(Mode::Down);
    for _ in 0..ATTEMPTS {
        assert_eq!(run(&probe, plain.port).await, HealthOutcome::TlsUntrusted);
    }
    assert!(port.accepts(ModeKind::Down) >= ATTEMPTS);

    // 平文の口まで空けば、Service→Desktop の待ちが見る「消えた」になる。
    drop(plain);
    let unused = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    assert_eq!(run(&probe, unused).await, HealthOutcome::Unreachable);

    // 同じポートで本物が戻れば、また通る（回復）。
    port.set(Mode::Relay(hub_a.addr));
    assert_eq!(run(&probe, unused).await, healthy());
    assert_eq!(
        hub_a.requests(),
        2,
        "only the two real probes reached hub A"
    );

    assert_eq!(port.squat_tls_handshakes(), 0);
    port.assert_secret_absent("bh_squatdummy.6f1c2a9e0d4b7381");
}

/// 証明書を作り直した Hub が再起動した。pin が古いうちは新しい Hub へも要求を出さず、
/// pin が読み直されてから（シェルの再起動なしで）通る。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_renewed_certificate_opens_the_gate_only_after_the_pin_is_reread() {
    let identity_a = TestIdentity::generate();
    let identity_b = TestIdentity::generate();
    let hub_a = TlsHub::start(&identity_a).await;
    let hub_b = TlsHub::start(&identity_b).await;
    let plain = PlainPort::start().await;
    let port = SwitchPort::start().await;
    let root = TempDir::new("pinned-hub-health-squat-renew");
    place_lock_file(&root);
    let pin = Arc::new(Mutex::new(identity_a.pem()));
    let probe = probe_with(&root, port.addr().port(), "127.0.0.1", &pin);

    port.set(Mode::Relay(hub_a.addr));
    assert_eq!(run(&probe, plain.port).await, healthy());

    // 再起動した Hub は新しい証明書（B）だが、pin はまだ A: 何度確かめても通らず、要求は届かない。
    port.set(Mode::Relay(hub_b.addr));
    for _ in 0..ATTEMPTS {
        assert_eq!(run(&probe, plain.port).await, HealthOutcome::TlsUntrusted);
    }
    assert_eq!(
        hub_b.requests(),
        0,
        "a stale pin must not send a request to hub B"
    );

    // `--renew` で作り直した証明書を pin が読み直す（probe は毎回 pin を読む）。
    *pin.lock().unwrap() = identity_b.pem();
    assert_eq!(run(&probe, plain.port).await, healthy());
    assert_eq!(hub_b.requests(), 1);

    // 古い証明書の Hub（A）がまた同じポートに現れても、もう通らない。
    port.set(Mode::Relay(hub_a.addr));
    let seen_by_a = hub_a.requests();
    assert_eq!(run(&probe, plain.port).await, HealthOutcome::TlsUntrusted);
    assert_eq!(hub_a.requests(), seen_by_a);
}
