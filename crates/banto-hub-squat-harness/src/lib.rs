//! #644 受入条件 4 の試験用に、「1 つのポートが Hub の一生を通す」場面を作る道具。
//!
//! 守りたいこと: Hub が止まっている間に同じ PC の別プロセスが 127.0.0.1 のポートを
//! 先に握っても、確認と送信の間に待受が入れ替わっても、切断・再起動・再試行を
//! 繰り返しても、再利用できる資格情報（API キー）が相手に届かない。
//!
//! 本物の Hub が止まって別プロセスが同じポートを取る様子は、ポートを開け閉めすると
//! OS のポート再利用で試験が不安定になる。そこで [`SwitchPort`] は `127.0.0.1:0` を
//! **一度だけ**開いて試験の間ずっと持ち続け、新しい接続に何を返すかだけを
//! [`Mode`] で切り替える。切り替えるときは、それまでのモードで受けた接続を全部
//! 切る（Hub プロセスが死んだ、に当たる）。
//!
//! - [`Mode::Down`]: 受けてすぐ閉じる（Hub 停止中、または何もいない）。
//! - [`Mode::Relay`]: 裏の「本物の Hub」へそのまま中継する。クライアント側から
//!   裏へ流れた生のバイト（暗号文）を記録する。
//! - [`Mode::SquatTls`]: 別の証明書で TLS を最後まで済ませ、復号したバイトを記録する。
//!   何か届いたら、送った側が成功したと思い込めるよう `200` を返す。
//! - [`Mode::SquatPlain`]: 平文として受け、バイトを記録し、届いたら `200` を返す。
//!
//! 記録は [`SwitchPort::assert_secret_absent`] で「キー全体」と「`.` より後ろの秘密部分」を
//! 探す。見つかったらどこで見つかったかを添えて panic する。
//! [`RecordingListener`] は偽のプロキシ・リダイレクト先として使う、受けるだけの
//! 平文の待受。

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::AbortHandle;
use tokio_rustls::TlsAcceptor;

/// 試験用の自己署名証明書（SAN は `127.0.0.1` と `localhost`）とその鍵。
#[derive(Clone)]
pub struct TestIdentity {
    pem: String,
    server: Arc<rustls::ServerConfig>,
}

impl TestIdentity {
    pub fn generate() -> Self {
        let pair = rcgen::generate_simple_self_signed(vec![
            "127.0.0.1".to_owned(),
            "localhost".to_owned(),
        ])
        .expect("generate test certificate");
        let key = PrivateKeyDer::from(PrivatePkcs8KeyDer::from(pair.signing_key.serialize_der()));
        let server = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("protocol versions")
        .with_no_client_auth()
        .with_single_cert(vec![pair.cert.der().clone()], key)
        .expect("server config");
        Self {
            pem: pair.cert.pem(),
            server: Arc::new(server),
        }
    }

    /// クライアントが pin に渡す証明書の PEM。
    pub fn pem(&self) -> String {
        self.pem.clone()
    }

    /// この証明書で待ち受ける側の TLS 設定。
    pub fn server_config(&self) -> Arc<rustls::ServerConfig> {
        Arc::clone(&self.server)
    }
}

/// [`SwitchPort`] が新しい接続に対してする振る舞い。
#[derive(Clone)]
pub enum Mode {
    Down,
    Relay(SocketAddr),
    /// 最初に受けた接続だけを中継し、裏の最初の返事をクライアントへ流す**前**に
    /// 第 2 引数のモードへ切り替える（確認と送信の間の入れ替え）。中継した接続は
    /// その返事を流し終えたら切り、キープアライブで次の要求を本物へ運ばせない。
    ///
    /// `tls` を渡すと、この待受が本物の Hub と同じ証明書で TLS を終わらせ、**平文の**
    /// 裏（`MockHub::start`）へ中継する。TLS の暗号文の上では「最初の返事」を
    /// 見分けられないので、アプリケーションの返事で切り替えたいときはこちらを使う。
    RelayThen {
        backend: SocketAddr,
        tls: Option<Arc<rustls::ServerConfig>>,
        then: Box<Mode>,
    },
    SquatTls(Arc<rustls::ServerConfig>),
    SquatPlain,
}

/// 受けた接続の数を数える単位（[`Mode`] の種類）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModeKind {
    Down,
    Relay,
    SquatTls,
    SquatPlain,
}

impl Mode {
    fn kind(&self) -> ModeKind {
        match self {
            Mode::Down => ModeKind::Down,
            Mode::Relay(_) | Mode::RelayThen { .. } => ModeKind::Relay,
            Mode::SquatTls(_) => ModeKind::SquatTls,
            Mode::SquatPlain => ModeKind::SquatPlain,
        }
    }
}

const OK_RESPONSE: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}";

#[derive(Default)]
struct Counters {
    down: AtomicUsize,
    relay: AtomicUsize,
    squat_tls: AtomicUsize,
    squat_plain: AtomicUsize,
    squat_tls_handshakes: AtomicUsize,
}

impl Counters {
    fn of(&self, kind: ModeKind) -> &AtomicUsize {
        match kind {
            ModeKind::Down => &self.down,
            ModeKind::Relay => &self.relay,
            ModeKind::SquatTls => &self.squat_tls,
            ModeKind::SquatPlain => &self.squat_plain,
        }
    }
}

#[derive(Default)]
struct Recorded {
    relay: Mutex<Vec<u8>>,
    squat_tls: Mutex<Vec<u8>>,
    squat_plain: Mutex<Vec<u8>>,
}

struct State {
    mode: Mode,
    connections: Vec<(tokio::task::Id, AbortHandle)>,
}

struct Shared {
    state: Mutex<State>,
    counters: Counters,
    recorded: Recorded,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 試験の間ずっと同じポートを持ち、中身だけを切り替える待受。
pub struct SwitchPort {
    addr: SocketAddr,
    shared: Arc<Shared>,
    accept_task: AbortHandle,
}

impl SwitchPort {
    /// `127.0.0.1:0` を開く。最初のモードは [`Mode::Down`]。
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind switch port");
        let addr = listener.local_addr().expect("local_addr");
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                mode: Mode::Down,
                connections: Vec::new(),
            }),
            counters: Counters::default(),
            recorded: Recorded::default(),
        });
        let accept_shared = Arc::clone(&shared);
        let accept_task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    continue;
                };
                // モードの読み取りと接続の登録を同じロックの中でする。`set` との間に
                // はさまると、切り替え後に古いモードで動く接続が残ってしまう。
                let mut state = lock(&accept_shared.state);
                let mode = state.mode.clone();
                accept_shared
                    .counters
                    .of(mode.kind())
                    .fetch_add(1, Ordering::SeqCst);
                let task = tokio::spawn(handle(stream, mode, Arc::clone(&accept_shared)));
                state.connections.push((task.id(), task.abort_handle()));
            }
        })
        .abort_handle();
        Self {
            addr,
            shared,
            accept_task,
        }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn https_url(&self) -> String {
        format!("https://{}", self.addr)
    }

    pub fn http_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// 新しい接続の扱いを切り替え、前のモードで受けた接続を全部切る。
    pub fn set(&self, mode: Mode) {
        let mut state = lock(&self.shared.state);
        for (_, connection) in state.connections.drain(..) {
            connection.abort();
        }
        state.mode = mode;
    }

    /// そのモードで受けた TCP 接続の数（切り替えをまたいだ合計）。
    pub fn accepts(&self, kind: ModeKind) -> usize {
        self.shared.counters.of(kind).load(Ordering::SeqCst)
    }

    /// 先取り側の TLS がクライアントの確認を通って最後まで済んだ回数。
    pub fn squat_tls_handshakes(&self) -> usize {
        self.shared
            .counters
            .squat_tls_handshakes
            .load(Ordering::SeqCst)
    }

    /// 先取り側（TLS の復号後・平文）が受けたバイトの合計。
    pub fn squat_bytes(&self) -> Vec<u8> {
        let mut all = self.squat_tls_bytes();
        all.extend(lock(&self.shared.recorded.squat_plain).iter());
        all
    }

    /// 先取り側が TLS を済ませたあとに受けたアプリケーションのバイト。
    pub fn squat_tls_bytes(&self) -> Vec<u8> {
        lock(&self.shared.recorded.squat_tls).clone()
    }

    /// 中継でクライアントから裏へ流れた生のバイト（TLS の暗号文）。
    pub fn relay_bytes(&self) -> Vec<u8> {
        lock(&self.shared.recorded.relay).clone()
    }

    /// 先取り側が受けたバイトだけを調べる。中継で本物の Hub へ正当に送った分（本物へ
    /// 鍵を 1 度送ったあとの入れ替えの試験）は対象にしない。
    pub fn assert_squat_secret_absent(&self, secret: &str) {
        assert_absent("squat TLS plaintext", &self.squat_tls_bytes(), secret);
        assert_absent(
            "squat plain bytes",
            &lock(&self.shared.recorded.squat_plain),
            secret,
        );
    }

    /// 記録したどのバイトにも、キー全体と `.` より後ろの秘密部分が無いことを確かめる。
    pub fn assert_secret_absent(&self, secret: &str) {
        assert_absent("squat TLS plaintext", &self.squat_tls_bytes(), secret);
        assert_absent(
            "squat plain bytes",
            &lock(&self.shared.recorded.squat_plain),
            secret,
        );
        assert_absent("relay ciphertext", &self.relay_bytes(), secret);
    }
}

impl Drop for SwitchPort {
    fn drop(&mut self) {
        self.accept_task.abort();
        for (_, connection) in lock(&self.shared.state).connections.drain(..) {
            connection.abort();
        }
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn assert_absent(label: &str, bytes: &[u8], secret: &str) {
    let part = secret.split_once('.').map_or(secret, |(_, rest)| rest);
    assert!(
        !contains(bytes, secret.as_bytes()),
        "the full API key was found in {label} ({} bytes recorded)",
        bytes.len()
    );
    assert!(
        !contains(bytes, part.as_bytes()),
        "the secret part of the API key was found in {label} ({} bytes recorded)",
        bytes.len()
    );
}

async fn handle(stream: TcpStream, mode: Mode, shared: Arc<Shared>) {
    match mode {
        Mode::Down => drop(stream),
        Mode::Relay(backend) => relay(stream, backend, &shared, None).await,
        Mode::RelayThen { backend, tls, then } => match tls {
            None => relay(stream, backend, &shared, Some(*then)).await,
            Some(config) => {
                let Ok(stream) = TlsAcceptor::from(config).accept(stream).await else {
                    return;
                };
                relay(stream, backend, &shared, Some(*then)).await;
            }
        },
        Mode::SquatPlain => {
            let mut stream = stream;
            sink(&mut stream, &shared.recorded.squat_plain).await;
        }
        Mode::SquatTls(config) => {
            let Ok(mut stream) = TlsAcceptor::from(config).accept(stream).await else {
                return;
            };
            shared
                .counters
                .squat_tls_handshakes
                .fetch_add(1, Ordering::SeqCst);
            sink(&mut stream, &shared.recorded.squat_tls).await;
        }
    }
}

/// 届いたものを記録し、最初に何か届いたら `200` を返す（送った側が成功したと思う）。
async fn sink<S: AsyncReadExt + AsyncWriteExt + Unpin>(stream: &mut S, record: &Mutex<Vec<u8>>) {
    let mut buffer = [0u8; 4096];
    let mut answered = false;
    loop {
        let Ok(count) = stream.read(&mut buffer).await else {
            return;
        };
        if count == 0 {
            return;
        }
        lock(record).extend_from_slice(&buffer[..count]);
        if !answered {
            answered = true;
            let _ = stream.write_all(OK_RESPONSE).await;
            let _ = stream.flush().await;
        }
    }
}

/// 自分（中継中の接続）だけを残して、ほかの接続を全部切ってモードを切り替える。
fn switch_keeping(shared: &Shared, keep: Option<tokio::task::Id>, mode: Mode) {
    let mut state = lock(&shared.state);
    let mut kept = Vec::new();
    for (id, connection) in state.connections.drain(..) {
        if Some(id) == keep {
            kept.push((id, connection));
        } else {
            connection.abort();
        }
    }
    state.connections = kept;
    state.mode = mode;
}

async fn relay<S>(client: S, backend: SocketAddr, shared: &Shared, then: Option<Mode>)
where
    S: AsyncReadExt + AsyncWriteExt + Unpin,
{
    let Ok(backend) = TcpStream::connect(backend).await else {
        return;
    };
    let (mut client_read, mut client_write) = tokio::io::split(client);
    let (mut backend_read, mut backend_write) = backend.into_split();
    // `then` があるとき、裏の最初の返事を見たら立てる。以後クライアントからの
    // 要求は裏へ運ばない（キープアライブで次の要求が本物へ届くのを防ぐ）。
    let sealed = AtomicBool::new(false);
    let upstream = async {
        let mut buffer = [0u8; 4096];
        while let Ok(count) = client_read.read(&mut buffer).await {
            if count == 0 || sealed.load(Ordering::SeqCst) {
                break;
            }
            lock(&shared.recorded.relay).extend_from_slice(&buffer[..count]);
            if backend_write.write_all(&buffer[..count]).await.is_err() {
                break;
            }
        }
    };
    let mut then = then;
    let downstream = async {
        let mut buffer = [0u8; 4096];
        loop {
            // 切り替え後は、返事の残りを少しだけ待って流し、あとは切る。
            let read = if then.is_none() && sealed.load(Ordering::SeqCst) {
                match tokio::time::timeout(
                    std::time::Duration::from_millis(200),
                    backend_read.read(&mut buffer),
                )
                .await
                {
                    Ok(read) => read,
                    Err(_) => break,
                }
            } else {
                backend_read.read(&mut buffer).await
            };
            let Ok(count) = read else { break };
            if count == 0 {
                break;
            }
            if let Some(next) = then.take() {
                // 返事をクライアントへ流す前に切り替える。流したあとでは、クライアントが
                // すぐ開く新しい接続が本物へ着いてしまう。
                sealed.store(true, Ordering::SeqCst);
                switch_keeping(shared, tokio::task::try_id(), next);
            }
            if client_write.write_all(&buffer[..count]).await.is_err() {
                break;
            }
        }
    };
    // どちらかが終わったら両方畳む（片側だけ開いたままにしない）。
    tokio::select! {
        () = upstream => {},
        () = downstream => {},
    }
}

/// 偽のプロキシ・リダイレクト先。平文で受けるだけで、返事はしない。
pub struct RecordingListener {
    addr: SocketAddr,
    accepts: Arc<AtomicUsize>,
    bytes: Arc<Mutex<Vec<u8>>>,
    task: AbortHandle,
}

impl RecordingListener {
    pub async fn start() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind recording listener");
        Self::from_std(listener)
    }

    /// 先に開いた標準のリスナーから作る。実行環境を作る前にポートが要る試験
    /// （プロキシの環境変数を先に設定する）のための入口で、tokio の実行環境の中で呼ぶ。
    pub fn from_std(listener: std::net::TcpListener) -> Self {
        listener.set_nonblocking(true).expect("set_nonblocking");
        let listener = TcpListener::from_std(listener).expect("from_std");
        let addr = listener.local_addr().expect("local_addr");
        let accepts = Arc::new(AtomicUsize::new(0));
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let (task_accepts, task_bytes) = (Arc::clone(&accepts), Arc::clone(&bytes));
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    continue;
                };
                task_accepts.fetch_add(1, Ordering::SeqCst);
                let record = Arc::clone(&task_bytes);
                tokio::spawn(async move {
                    let mut buffer = [0u8; 4096];
                    while let Ok(count) = stream.read(&mut buffer).await {
                        if count == 0 {
                            break;
                        }
                        lock(&record).extend_from_slice(&buffer[..count]);
                    }
                });
            }
        })
        .abort_handle();
        Self {
            addr,
            accepts,
            bytes,
            task,
        }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn http_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn accepts(&self) -> usize {
        self.accepts.load(Ordering::SeqCst)
    }

    pub fn bytes(&self) -> Vec<u8> {
        lock(&self.bytes).clone()
    }

    pub fn assert_secret_absent(&self, secret: &str) {
        assert_absent("recording listener", &self.bytes(), secret);
    }
}

impl Drop for RecordingListener {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{timeout, Duration};

    async fn is_closed(stream: &mut TcpStream) -> bool {
        let mut out = Vec::new();
        match timeout(Duration::from_secs(3), stream.read_to_end(&mut out))
            .await
            .expect("read did not finish")
        {
            Ok(_) => out.is_empty(),
            Err(_) => true,
        }
    }

    #[tokio::test]
    async fn switching_the_mode_aborts_connections_accepted_under_the_previous_mode() {
        let port = SwitchPort::start().await;
        port.set(Mode::SquatPlain);
        let mut old = TcpStream::connect(port.addr()).await.unwrap();
        // 先取り側が受けて返事をするところまで待ってから切り替える。
        old.write_all(b"hello").await.unwrap();
        let mut buffer = [0u8; 256];
        assert!(old.read(&mut buffer).await.unwrap() > 0);
        port.set(Mode::Down);
        assert!(is_closed(&mut old).await, "the old connection must be cut");
        // 新しい接続は Down として即閉じられる。
        let mut fresh = TcpStream::connect(port.addr()).await.unwrap();
        assert!(is_closed(&mut fresh).await);
        assert_eq!(port.accepts(ModeKind::SquatPlain), 1);
        assert_eq!(port.accepts(ModeKind::Down), 1);
    }

    #[tokio::test]
    async fn relay_forwards_both_ways_and_records_client_bytes() {
        let backend = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend_addr = backend.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = backend.accept().await.unwrap();
            let mut buffer = [0u8; 64];
            let count = stream.read(&mut buffer).await.unwrap();
            stream.write_all(&buffer[..count]).await.unwrap();
            // 受けたものを返したあとも、クライアントが閉じるまで開けておく。
            let _ = stream.read(&mut buffer).await;
        });
        let port = SwitchPort::start().await;
        port.set(Mode::Relay(backend_addr));
        let mut client = TcpStream::connect(port.addr()).await.unwrap();
        client.write_all(b"ping-bytes").await.unwrap();
        let mut echoed = [0u8; 10];
        timeout(Duration::from_secs(3), client.read_exact(&mut echoed))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&echoed, b"ping-bytes");
        assert_eq!(port.relay_bytes(), b"ping-bytes");
        assert_eq!(port.accepts(ModeKind::Relay), 1);
    }

    #[tokio::test]
    async fn relay_then_switches_before_the_first_response_reaches_the_client() {
        let backend = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let backend_addr = backend.local_addr().unwrap();
        let backend_seen = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&backend_seen);
        tokio::spawn(async move {
            let (mut stream, _) = backend.accept().await.unwrap();
            let mut buffer = [0u8; 64];
            while let Ok(count) = stream.read(&mut buffer).await {
                if count == 0 {
                    break;
                }
                lock(&seen).extend_from_slice(&buffer[..count]);
                if stream.write_all(b"first-response").await.is_err() {
                    break;
                }
            }
        });
        let port = SwitchPort::start().await;
        port.set(Mode::RelayThen {
            backend: backend_addr,
            tls: None,
            then: Box::new(Mode::SquatPlain),
        });
        let mut client = TcpStream::connect(port.addr()).await.unwrap();
        client.write_all(b"status").await.unwrap();
        let mut response = [0u8; 14];
        timeout(Duration::from_secs(3), client.read_exact(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&response, b"first-response");
        // 返事を受けた時点で、新しい接続はもう先取り側に着く。
        let mut fresh = TcpStream::connect(port.addr()).await.unwrap();
        fresh.write_all(b"key-bytes").await.unwrap();
        let mut buffer = [0u8; 256];
        assert!(fresh.read(&mut buffer).await.unwrap() > 0);
        // 中継した接続は返事のあとで切られ、続きの要求は裏へ届かない。
        let _ = client.write_all(b"second").await;
        assert!(is_closed(&mut client).await);
        assert_eq!(*lock(&backend_seen), b"status");
        assert_eq!(port.accepts(ModeKind::Relay), 1);
        assert_eq!(port.accepts(ModeKind::SquatPlain), 1);
        assert_eq!(
            lock(&port.shared.recorded.squat_plain).as_slice(),
            b"key-bytes"
        );
    }

    #[tokio::test]
    async fn secret_check_finds_the_whole_key_and_its_secret_part() {
        let port = SwitchPort::start().await;
        port.set(Mode::SquatPlain);
        let mut client = TcpStream::connect(port.addr()).await.unwrap();
        client
            .write_all(b"Authorization: Bearer bh_x.0123abcd")
            .await
            .unwrap();
        let mut buffer = [0u8; 256];
        let _ = client.read(&mut buffer).await.unwrap();
        port.assert_secret_absent("bh_other.ffffffff");
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            port.assert_secret_absent("bh_zzz.0123abcd");
        }));
        assert!(caught.is_err(), "the secret part alone must be detected");
    }
}
