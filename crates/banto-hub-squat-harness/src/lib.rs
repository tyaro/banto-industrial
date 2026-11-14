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
use std::time::Duration;

use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
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
    /// その返事（HTTP/1.1 の返事 1 つ）を**最後まで**流し終えたら切り、キープアライブで
    /// 次の要求を本物へ運ばせない。返事の終わりは `Content-Length`・chunked・接続の
    /// 終わりで見分け、時間では区切らない（返事が分かれて遅れて届いても途中で切らない）。
    /// 流し終えた回数は [`SwitchPort::handed_over_responses`] で確かめられる。
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
    handed_over_responses: AtomicUsize,
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
    /// 受けた接続のうち、扱いが終わった（相手が閉じた・切った・握手が失敗した）数。
    finished: Counters,
    finished_notify: Notify,
    recorded: Recorded,
}

/// 接続の扱いが終わったら（中断で落とされたときも）数える。
struct FinishedGuard {
    shared: Arc<Shared>,
    kind: ModeKind,
}

impl Drop for FinishedGuard {
    fn drop(&mut self) {
        self.shared
            .finished
            .of(self.kind)
            .fetch_add(1, Ordering::SeqCst);
        self.shared.finished_notify.notify_waiters();
    }
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
            finished: Counters::default(),
            finished_notify: Notify::new(),
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
                let guard = FinishedGuard {
                    shared: Arc::clone(&accept_shared),
                    kind: mode.kind(),
                };
                let handler_shared = Arc::clone(&accept_shared);
                let task = tokio::spawn(async move {
                    let _finished = guard;
                    handle(stream, mode, handler_shared).await;
                });
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

    /// そのモードで受けた接続のうち、扱いが終わった数（切り替えをまたいだ合計）。
    ///
    /// [`Self::accepts`] は TCP を受けた時点で増えるので、TLS の握手や要求の処理が
    /// 済んだことは示さない。先取り側では、相手（クライアント）が接続を閉じるか
    /// 握手が失敗するまで読み続けるので、「終わった」はその接続で相手が送ったものを
    /// すべて記録し終えたことを意味する。記録したバイトを調べる前にこちらを待つ。
    pub fn finished(&self, kind: ModeKind) -> usize {
        self.shared.finished.of(kind).load(Ordering::SeqCst)
    }

    /// そのモードで扱いが終わった接続が `at_least` 本になるまで待つ。`limit` を
    /// 過ぎたら、受けた数と終わった数を添えて panic する。
    pub async fn wait_finished(&self, kind: ModeKind, at_least: usize, limit: Duration) {
        let waited = tokio::time::timeout(limit, async {
            loop {
                let notified = self.shared.finished_notify.notified();
                tokio::pin!(notified);
                // 数を読む前に待ちを登録する（読んだ直後の通知を取りこぼさない）。
                notified.as_mut().enable();
                if self.finished(kind) >= at_least {
                    return;
                }
                notified.await;
            }
        })
        .await;
        assert!(
            waited.is_ok(),
            "timed out waiting for {at_least} finished {kind:?} connections (accepted {}, finished {})",
            self.accepts(kind),
            self.finished(kind)
        );
    }

    /// そのモードで**いままでに受けた**接続がすべて終わるまで待ち、その数を返す。
    /// 呼び出し側の要求がすべて返ったあと（新しい接続がもう来ないとき）に、記録を
    /// 調べる前の区切りとして使う。`at_least` 本に満たなければ panic する（一度も
    /// 掛けにいっていないのに「何も届いていない」が通るのを防ぐ）。
    pub async fn settle(&self, kind: ModeKind, at_least: usize) -> usize {
        let accepted = self.accepts(kind);
        assert!(
            accepted >= at_least,
            "only {accepted} {kind:?} connections were accepted, expected at least {at_least}"
        );
        self.wait_finished(kind, accepted, Duration::from_secs(10))
            .await;
        accepted
    }

    /// [`Mode::RelayThen`] が、裏の最初の返事を最後までクライアントへ流し終えた回数。
    pub fn handed_over_responses(&self) -> usize {
        self.shared
            .counters
            .handed_over_responses
            .load(Ordering::SeqCst)
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
            if count == 0 {
                break;
            }
            if sealed.load(Ordering::SeqCst) {
                // 返事を流している途中で接続を畳まないよう、読み捨てるだけにする。
                continue;
            }
            lock(&shared.recorded.relay).extend_from_slice(&buffer[..count]);
            if backend_write.write_all(&buffer[..count]).await.is_err() {
                break;
            }
        }
    };
    let mut first_response = then.is_some().then(ResponseEnd::default);
    let mut then = then;
    let downstream = async {
        let mut buffer = [0u8; 4096];
        while let Ok(count) = backend_read.read(&mut buffer).await {
            if count == 0 {
                // 長さの手がかりが無い返事は、裏が閉じたところが終わり。
                if first_response
                    .as_ref()
                    .is_some_and(ResponseEnd::ends_at_close)
                {
                    let _ = client_write.shutdown().await;
                    shared
                        .counters
                        .handed_over_responses
                        .fetch_add(1, Ordering::SeqCst);
                }
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
            if let Some(response) = first_response.as_mut() {
                // 最初の返事を最後まで流し終えたら切る。断片が分かれて遅れて届いても、
                // 終わりが見えるまでは流し続ける（時間で区切らない）。
                if response.feed(&buffer[..count]) {
                    let _ = client_write.flush().await;
                    let _ = client_write.shutdown().await;
                    shared
                        .counters
                        .handed_over_responses
                        .fetch_add(1, Ordering::SeqCst);
                    break;
                }
            }
        }
    };
    // どちらかが終わったら両方畳む（片側だけ開いたままにしない）。
    tokio::select! {
        () = upstream => {},
        () = downstream => {},
    }
}

/// HTTP/1.1 の返事 1 つがどこで終わるかを、流れてくる断片から追う。
///
/// 試験の裏（`MockHub` など）が返す形だけを扱う: `Content-Length`、chunked（終わりの
/// 長さ 0 のチャンクを探すだけで、トレーラーは扱わない）、どちらも無ければ接続の
/// 終わりまで。本文を持たない状態（1xx・204・304）はヘッダーの終わりが返事の終わり。
#[derive(Default)]
struct ResponseEnd {
    head: Vec<u8>,
    body: Option<Body>,
}

enum Body {
    Remaining(usize),
    Chunked(Vec<u8>),
    UntilClose,
}

impl ResponseEnd {
    /// 断片を足す。ここまでで返事が完結したら `true`。
    fn feed(&mut self, bytes: &[u8]) -> bool {
        let rest = match self.body {
            Some(_) => bytes.to_vec(),
            None => {
                self.head.extend_from_slice(bytes);
                let Some(end) = self
                    .head
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                else {
                    return false;
                };
                let rest = self.head.split_off(end + 4);
                self.body = Some(body_of(&String::from_utf8_lossy(&self.head)));
                rest
            }
        };
        match self.body.as_mut() {
            Some(Body::Remaining(left)) => {
                *left = left.saturating_sub(rest.len());
                *left == 0
            }
            Some(Body::Chunked(seen)) => {
                seen.extend_from_slice(&rest);
                seen.as_slice() == b"0\r\n\r\n" || seen.ends_with(b"\r\n0\r\n\r\n")
            }
            Some(Body::UntilClose) | None => false,
        }
    }

    /// ヘッダーは届いていて、返事の終わりが「接続の終わり」で決まる形か。
    fn ends_at_close(&self) -> bool {
        matches!(self.body, Some(Body::UntilClose))
    }
}

fn body_of(head: &str) -> Body {
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0);
    if (100..200).contains(&status) || status == 204 || status == 304 {
        return Body::Remaining(0);
    }
    let mut length = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("transfer-encoding")
            && value.to_ascii_lowercase().contains("chunked")
        {
            return Body::Chunked(Vec::new());
        }
        if name.eq_ignore_ascii_case("content-length") {
            length = value.parse::<usize>().ok();
        }
    }
    length.map_or(Body::UntilClose, Body::Remaining)
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
    use tokio::time::timeout;

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
        // 返事を 3 つの断片に分け、間を空けて返す（ヘッダーの途中・本文の途中で切れる）。
        // 断片の間は、以前の「残りを 200ms だけ待つ」より長い。
        const FRAGMENTS: [&[u8]; 3] = [
            b"HTTP/1.1 200 OK\r\nContent-Len",
            b"gth: 10\r\n\r\nhello",
            b"world",
        ];
        tokio::spawn(async move {
            let (mut stream, _) = backend.accept().await.unwrap();
            let mut buffer = [0u8; 64];
            let mut answered = false;
            while let Ok(count) = stream.read(&mut buffer).await {
                if count == 0 {
                    break;
                }
                lock(&seen).extend_from_slice(&buffer[..count]);
                if answered {
                    continue;
                }
                answered = true;
                for (index, fragment) in FRAGMENTS.iter().enumerate() {
                    if index > 0 {
                        tokio::time::sleep(Duration::from_millis(400)).await;
                    }
                    if stream.write_all(fragment).await.is_err() {
                        return;
                    }
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
        // 最初の断片が届いた時点で、新しい接続はもう先取り側に着く。
        let mut first = [0u8; 28];
        timeout(Duration::from_secs(3), client.read_exact(&mut first))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&first, FRAGMENTS[0]);
        let mut fresh = TcpStream::connect(port.addr()).await.unwrap();
        fresh.write_all(b"key-bytes").await.unwrap();
        let mut buffer = [0u8; 256];
        assert!(fresh.read(&mut buffer).await.unwrap() > 0);
        // 返事の途中でクライアントが次の要求を送っても、裏へは運ばれず、返事も途中で
        // 切られない。返事は最後まで届き、そのあとで接続が切られる。
        client.write_all(b"second").await.unwrap();
        let mut rest = Vec::new();
        timeout(Duration::from_secs(5), client.read_to_end(&mut rest))
            .await
            .expect("the relayed connection must be cut after the response")
            .unwrap();
        assert_eq!(rest, [FRAGMENTS[1], FRAGMENTS[2]].concat());
        assert_eq!(port.handed_over_responses(), 1);
        port.wait_finished(ModeKind::Relay, 1, Duration::from_secs(3))
            .await;
        assert_eq!(*lock(&backend_seen), b"status");
        assert_eq!(port.accepts(ModeKind::Relay), 1);
        assert_eq!(port.accepts(ModeKind::SquatPlain), 1);
        assert_eq!(
            lock(&port.shared.recorded.squat_plain).as_slice(),
            b"key-bytes"
        );
    }

    #[test]
    fn response_end_follows_content_length_chunked_and_close() {
        let mut sized = ResponseEnd::default();
        assert!(!sized.feed(b"HTTP/1.1 401 Test\r\ncontent-length: 2\r\n"));
        assert!(!sized.feed(b"\r\n{"));
        assert!(sized.feed(b"}"));

        let mut empty = ResponseEnd::default();
        assert!(empty.feed(b"HTTP/1.1 307 Redirect\r\nContent-Length: 0\r\n\r\n"));

        let mut switching = ResponseEnd::default();
        assert!(switching.feed(b"HTTP/1.1 101 Switching Protocols\r\n\r\n"));

        let mut chunked = ResponseEnd::default();
        assert!(!chunked.feed(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}"));
        assert!(!chunked.feed(b"\r\n0\r\n"));
        assert!(chunked.feed(b"\r\n"));

        let mut until_close = ResponseEnd::default();
        assert!(!until_close.ends_at_close());
        assert!(!until_close.feed(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{}"));
        assert!(until_close.ends_at_close());
    }

    #[tokio::test]
    async fn finished_counts_a_connection_only_after_the_peer_closed_it() {
        let port = SwitchPort::start().await;
        port.set(Mode::SquatPlain);
        let mut client = TcpStream::connect(port.addr()).await.unwrap();
        client.write_all(b"first").await.unwrap();
        let mut buffer = [0u8; 256];
        assert!(client.read(&mut buffer).await.unwrap() > 0);
        // 受けて返事もしたが、相手がまだ閉じていないので「終わった」には数えない。
        assert_eq!(port.accepts(ModeKind::SquatPlain), 1);
        assert_eq!(port.finished(ModeKind::SquatPlain), 0);
        client.write_all(b"-late").await.unwrap();
        drop(client);
        assert_eq!(port.settle(ModeKind::SquatPlain, 1).await, 1);
        // 終わったあとなら、閉じる直前に送った分も記録に入っている。
        assert_eq!(port.squat_bytes(), b"first-late");
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
