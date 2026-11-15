//! Optional, dedicated loopback TLS transport for the existing Hub router.
//! The identity comes either from PEM files named by the operator
//! (`BANTO_HUB_TLS_PORT/CERT/KEY`, read here by path) or, for the Windows
//! service by default (#644 PR1b), from PEM bytes that the service already
//! read through pinned handles and checked
//! (`crate::hub_tls_prepare::load`; the files are created by
//! `banto-hub-elev prepare-hub-tls`). This module neither creates
//! certificates nor changes their permissions, and never logs the key.
//! The listener binds IPv4 127.0.0.1 only; callers should use that literal
//! address rather than localhost or ::1.
//!
//! ## 失効による停止（#1121 PR2c）
//!
//! 通常の停止（[`RunningTlsListener::stop`]）は graceful shutdown を送り、上限を過ぎたら serve の
//! タスクを abort する。axum の serve は接続ごとにタスクを spawn するので、abort しても upgrade
//! 済みの WebSocket などの接続は残りうる（通常の終了ではこれでよい。ランタイムごと終わる）。
//!
//! 証明書の失効（露出を見た）で止めるときは、その証明書で張られた接続を**すぐ確実に**切る
//! （[`TlsRevoker::revoke`]）。そのため、受け付けた TCP の接続はすべて [`SeverableTcp`] で包んで
//! から TLS の handshake に渡す。包むときに、下の TCP のソケットの複製（`try_clone`。同じ接続を
//! 指す別のハンドル）を共有の [`Severance`] に登録しておき、失効のときは印を立ててから複製の
//! すべてに `shutdown(Both)` をかける。OS の上で接続が閉じるので、どのタスクがその接続を
//! 持っていても、次の読み書きは EOF かエラーになり（待っている読み書きも起こされる）、相手にも
//! FIN が届く。印が立った後の [`SeverableTcp`] の読み書きはエラーを返し、印が立った後に
//! 登録しようとした接続（失効と受け付けが競った）もすぐ閉じる。複製は包んだ側の drop で
//! 登録から外して閉じる。最後に serve のタスクを abort するので、待ち受けのポートも閉じ、
//! handshake の途中のものも捨てる。

use std::{
    collections::HashMap,
    env,
    ffi::OsString,
    io,
    net::{Ipv4Addr, Shutdown, SocketAddr, SocketAddrV4},
    path::PathBuf,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, RwLock,
    },
    task::{Context, Poll},
    time::Duration,
};

use axum::serve::{Listener, ListenerExt};
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
    sync::oneshot,
    task::{AbortHandle, JoinHandle, JoinSet},
};
use tokio_rustls::{
    rustls::{
        pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer},
        ServerConfig,
    },
    server::TlsStream,
    TlsAcceptor,
};
use zeroize::Zeroizing;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_PENDING_HANDSHAKES: usize = 32;
const STOP_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone)]
pub(crate) struct TlsConfig {
    port: u16,
    identity: TlsIdentity,
}

/// Where the certificate chain and private key come from.
#[derive(Clone)]
enum TlsIdentity {
    /// PEM files read by path when the acceptor is built (operator settings).
    Files { cert: PathBuf, key: PathBuf },
    /// PEM bytes already read and checked by the caller (#644 PR1b). The
    /// files are not reopened by path.
    Memory {
        cert: Vec<u8>,
        key: Zeroizing<Vec<u8>>,
    },
}

impl std::fmt::Debug for TlsIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Files { cert, key } => f
                .debug_struct("Files")
                .field("cert", cert)
                .field("key", key)
                .finish(),
            Self::Memory { cert, .. } => f
                .debug_struct("Memory")
                .field("cert_len", &cert.len())
                .field("key", &"<redacted>")
                .finish(),
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum TlsError {
    #[error("set BANTO_HUB_TLS_PORT, BANTO_HUB_TLS_CERT and BANTO_HUB_TLS_KEY together")]
    IncompleteConfig,
    #[error("BANTO_HUB_TLS_PORT must be a nonzero port number")]
    InvalidPort,
    #[error("BANTO_HUB_TLS_CERT and BANTO_HUB_TLS_KEY must be nonempty paths")]
    EmptyPath,
    #[error("could not read a valid Hub TLS certificate PEM")]
    Certificate,
    #[error("could not read a valid Hub TLS private-key PEM")]
    PrivateKey,
    #[error("Hub TLS certificate and private key do not match or are unsupported")]
    InvalidIdentity,
    #[error("could not bind Hub TLS loopback listener: {0}")]
    Bind(#[source] io::Error),
}

impl TlsConfig {
    pub(crate) fn explicit(port: u16, cert: PathBuf, key: PathBuf) -> Result<Self, TlsError> {
        if cert.as_os_str().is_empty() || key.as_os_str().is_empty() {
            return Err(TlsError::EmptyPath);
        }
        Ok(Self {
            port,
            identity: TlsIdentity::Files { cert, key },
        })
    }

    /// PEM bytes the caller already read and checked (#644 PR1b).
    pub(crate) fn in_memory(port: u16, cert_pem: Vec<u8>, key_pem: Zeroizing<Vec<u8>>) -> Self {
        Self {
            port,
            identity: TlsIdentity::Memory {
                cert: cert_pem,
                key: key_pem,
            },
        }
    }

    /// Whether any of the three operator settings is present (they take
    /// precedence over the service's default identity).
    pub(crate) fn env_present() -> bool {
        [
            "BANTO_HUB_TLS_PORT",
            "BANTO_HUB_TLS_CERT",
            "BANTO_HUB_TLS_KEY",
        ]
        .iter()
        .any(|name| env::var_os(name).is_some())
    }

    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    #[cfg(windows)]
    pub(crate) fn thumbprint(&self) -> Option<banto_hub_tls_trust::CertThumbprint> {
        match &self.identity {
            TlsIdentity::Memory { cert, .. } => {
                banto_hub_tls_trust::CertThumbprint::of_pem(cert).ok()
            }
            _ => None,
        }
    }

    pub(crate) fn from_env() -> Result<Option<Self>, TlsError> {
        Self::parse(
            env::var_os("BANTO_HUB_TLS_PORT"),
            env::var_os("BANTO_HUB_TLS_CERT"),
            env::var_os("BANTO_HUB_TLS_KEY"),
        )
    }

    fn parse(
        port: Option<OsString>,
        cert: Option<OsString>,
        key: Option<OsString>,
    ) -> Result<Option<Self>, TlsError> {
        match (port, cert, key) {
            (None, None, None) => Ok(None),
            (Some(port), Some(cert), Some(key)) => {
                let port = port
                    .to_str()
                    .and_then(|value| value.parse::<u16>().ok())
                    .filter(|port| *port != 0)
                    .ok_or(TlsError::InvalidPort)?;
                if cert.is_empty() || key.is_empty() {
                    return Err(TlsError::EmptyPath);
                }
                Ok(Some(Self::explicit(port, cert.into(), key.into())?))
            }
            _ => Err(TlsError::IncompleteConfig),
        }
    }

    pub(crate) fn acceptor(&self) -> Result<TlsAcceptor, TlsError> {
        // Do not include the key file or parser's input in errors or logs.
        let (certs, key): (Vec<CertificateDer<'static>>, _) = match &self.identity {
            TlsIdentity::Files { cert, key } => (
                CertificateDer::pem_file_iter(cert)
                    .map_err(|_| TlsError::Certificate)?
                    .collect::<Result<_, _>>()
                    .map_err(|_| TlsError::Certificate)?,
                PrivateKeyDer::from_pem_file(key),
            ),
            TlsIdentity::Memory { cert, key } => (
                CertificateDer::pem_slice_iter(cert)
                    .collect::<Result<_, _>>()
                    .map_err(|_| TlsError::Certificate)?,
                PrivateKeyDer::from_pem_slice(key),
            ),
        };
        if certs.is_empty() {
            return Err(TlsError::Certificate);
        }
        let key = key.map_err(|_| TlsError::PrivateKey)?;
        let config = ServerConfig::builder_with_provider(Arc::new(
            tokio_rustls::rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|_| TlsError::InvalidIdentity)?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|_| TlsError::InvalidIdentity)?;
        Ok(TlsAcceptor::from(Arc::new(config)))
    }
}

/// 受け付けた接続の、下の TCP のソケットの複製（失効のときに `shutdown(Both)` をかける）。
/// モジュール doc「失効による停止」。
#[derive(Default)]
pub(crate) struct Severance {
    severed: AtomicBool,
    next_id: AtomicU64,
    sockets: Mutex<HashMap<u64, std::net::TcpStream>>,
}

impl Severance {
    fn sockets(&self) -> std::sync::MutexGuard<'_, HashMap<u64, std::net::TcpStream>> {
        self.sockets.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// 印を立て、登録したすべての接続を OS の上で閉じる（`shutdown(Both)`）。何度呼んでもよい。
    fn sever(&self) {
        let mut sockets = self.sockets();
        self.severed.store(true, Ordering::SeqCst);
        for (_, socket) in sockets.drain() {
            let _ = socket.shutdown(Shutdown::Both);
        }
    }

    fn is_severed(&self) -> bool {
        self.severed.load(Ordering::SeqCst)
    }

    /// `stream` を包む。複製を作れなければ接続を閉じて `None`（切れない接続は受け付けない）。
    fn wrap(self: &Arc<Self>, stream: TcpStream) -> Option<SeverableTcp> {
        let std_stream = stream.into_std().ok()?;
        let clone = match std_stream.try_clone() {
            Ok(clone) => clone,
            Err(_) => {
                let _ = std_stream.shutdown(Shutdown::Both);
                return None;
            }
        };
        let inner = TcpStream::from_std(std_stream).ok()?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        {
            // 印と登録は同じロックの下で見る（失効と受け付けが競っても、閉じ損ねない）。
            let mut sockets = self.sockets();
            if self.is_severed() {
                let _ = clone.shutdown(Shutdown::Both);
                return None;
            }
            sockets.insert(id, clone);
        }
        Some(SeverableTcp {
            inner,
            id,
            severance: self.clone(),
        })
    }
}

/// 失効で切れる TCP の接続（モジュール doc「失効による停止」）。印が立った後の読み書きは
/// `ConnectionAborted` を返す。
pub(crate) struct SeverableTcp {
    inner: TcpStream,
    id: u64,
    severance: Arc<Severance>,
}

impl SeverableTcp {
    fn severed_error() -> io::Error {
        io::Error::new(
            io::ErrorKind::ConnectionAborted,
            "Hub の TLS の証明書が失効したので接続を切りました",
        )
    }
}

impl Drop for SeverableTcp {
    fn drop(&mut self) {
        // 複製を登録から外して閉じる（接続そのものは `inner` の drop で閉じる）。
        self.severance.sockets().remove(&self.id);
    }
}

impl AsyncRead for SeverableTcp {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.severance.is_severed() {
            return Poll::Ready(Err(Self::severed_error()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for SeverableTcp {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.severance.is_severed() {
            return Poll::Ready(Err(Self::severed_error()));
        }
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.severance.is_severed() {
            return Poll::Ready(Err(Self::severed_error()));
        }
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.severance.is_severed() {
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

struct TlsListener {
    tcp: TcpListener,
    acceptor: Arc<RwLock<TlsAcceptor>>,
    severance: Arc<Severance>,
    handshakes: JoinSet<Option<(TlsStream<SeverableTcp>, SocketAddr)>>,
}

impl Listener for TlsListener {
    type Io = TlsStream<SeverableTcp>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            tokio::select! {
                ready = self.handshakes.join_next(), if !self.handshakes.is_empty() => {
                    if let Some(Ok(Some(connection))) = ready {
                        return connection;
                    }
                }
                accepted = self.tcp.accept(), if self.handshakes.len() < MAX_PENDING_HANDSHAKES => {
                    match accepted {
                        Ok((stream, peer)) => {
                            let Some(stream) = self.severance.wrap(stream) else {
                                continue;
                            };
                            let acceptor = self.acceptor.read().unwrap_or_else(|p| p.into_inner()).clone();
                            self.handshakes.spawn(async move {
                                // Idle local peers cannot hold the listener indefinitely.
                                tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream))
                                    .await.ok()?.ok().map(|tls| (tls, peer))
                            });
                        }
                        Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
                    }
                }
            }
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.tcp.local_addr()
    }
}

pub struct RunningTlsListener {
    #[cfg(windows)]
    acceptor: Option<Arc<RwLock<TlsAcceptor>>>,
    local_addr: SocketAddr,
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
    severance: Arc<Severance>,
    // Shell transport keeps its origin reserved even if serve unexpectedly exits.
    _reservation: Option<std::net::TcpListener>,
}

/// 失効で 8723 の待ち受けを止め、その証明書で張られた接続をすぐ切る口（#1121 PR2c。
/// [`RunningTlsListener::revoker`]。複製して別のスレッド・タスクから呼べる）。
#[derive(Clone)]
pub struct TlsRevoker {
    target: RevocationTarget,
}

#[derive(Clone)]
enum RevocationTarget {
    Listener {
        severance: Arc<Severance>,
        abort: Arc<AbortHandle>,
    },
    #[cfg(windows)]
    Following(Arc<Mutex<FollowingRevocation>>),
}

#[cfg(windows)]
struct FollowingRevocation {
    external_stop: bool,
    retired_identity: bool,
    current: Option<TlsRevoker>,
}

impl TlsRevoker {
    /// 張られた接続をすべて OS の上で閉じ、serve のタスクを abort する（待ち受けのポートも閉じる）。
    /// 待たない（同期）。何度呼んでもよい。止めた後は再開しない。
    /// デスクトップ監視の共有ハンドルは、復帰した listener にも同じ停止の印を伝える。
    pub fn revoke(&self) {
        match &self.target {
            RevocationTarget::Listener { severance, abort } => {
                severance.sever();
                abort.abort();
            }
            #[cfg(windows)]
            RevocationTarget::Following(shared) => {
                let mut shared = shared.lock().unwrap_or_else(|p| p.into_inner());
                shared.external_stop = true;
                if let Some(current) = &shared.current {
                    current.revoke();
                }
            }
        }
    }

    /// 失効で止めたか（共有ハンドルの印は listener の交換後も消えない）。
    pub fn is_revoked(&self) -> bool {
        match &self.target {
            RevocationTarget::Listener { severance, .. } => severance.is_severed(),
            #[cfg(windows)]
            RevocationTarget::Following(shared) => {
                let shared = shared.lock().unwrap_or_else(|p| p.into_inner());
                shared.external_stop || shared.retired_identity
            }
        }
    }

    /// 監視の寿命に対応する安定した停止口。current には listener 固有の停止口だけを渡す。
    #[cfg(windows)]
    pub(crate) fn following(current: TlsRevoker) -> Self {
        Self {
            target: RevocationTarget::Following(Arc::new(Mutex::new(FollowingRevocation {
                external_stop: false,
                retired_identity: false,
                current: Some(current),
            }))),
        }
    }

    /// 失効と登録を同じロックで直列化する。既に失効していれば新 listener も即座に止める。
    /// 呼び手は、この登録が済むまで新 listener に accept させない。
    #[cfg(windows)]
    fn follow(&self, current: TlsRevoker) {
        let RevocationTarget::Following(shared) = &self.target else {
            unreachable!("monitor revoker must follow listeners")
        };
        let mut shared = shared.lock().unwrap_or_else(|p| p.into_inner());
        if shared.external_stop || shared.retired_identity {
            current.revoke();
        }
        shared.current = Some(current);
    }

    #[cfg(windows)]
    pub(crate) fn idle_following() -> Self {
        Self {
            target: RevocationTarget::Following(Arc::new(Mutex::new(FollowingRevocation {
                external_stop: false,
                retired_identity: false,
                current: None,
            }))),
        }
    }

    /// Monitor-only stop for the old certificate; an authorized renewal can
    /// replace this identity, while public external stops are never cleared.
    #[cfg(windows)]
    pub(crate) fn retire_identity(&self) {
        let RevocationTarget::Following(shared) = &self.target else {
            unreachable!()
        };
        let mut shared = shared.lock().unwrap_or_else(|p| p.into_inner());
        shared.retired_identity = true;
        if let Some(current) = &shared.current {
            current.revoke();
        }
    }

    #[cfg(windows)]
    pub(crate) fn externally_stopped(&self) -> bool {
        let RevocationTarget::Following(shared) = &self.target else {
            unreachable!()
        };
        shared
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .external_stop
    }

    #[cfg(windows)]
    fn follow_renewed(&self, current: TlsRevoker) -> bool {
        let RevocationTarget::Following(shared) = &self.target else {
            unreachable!()
        };
        let mut shared = shared.lock().unwrap_or_else(|p| p.into_inner());
        if shared.external_stop {
            current.revoke();
            return false;
        }
        shared.retired_identity = false;
        shared.current = Some(current);
        true
    }
}

impl RunningTlsListener {
    #[cfg(windows)]
    pub(crate) fn replace_identity(
        &self,
        config: &TlsConfig,
        following: &TlsRevoker,
    ) -> Result<(), String> {
        let next = config.acceptor().map_err(|e| e.to_string())?;
        let acceptor = self
            .acceptor
            .as_ref()
            .ok_or("TLS acceptor is unavailable")?;
        let mut acceptor = acceptor.write().unwrap_or_else(|p| p.into_inner());
        if !self.is_serving() || following.externally_stopped() {
            return Err("TLS は外部から停止されたため証明書を差し替えられません".into());
        }
        *acceptor = next;
        self.disconnect_clients();
        if !following.follow_renewed(self.revoker()) || !self.is_serving() {
            return Err("TLS の外部停止と作り直しが競合しました".into());
        }
        Ok(())
    }
    pub(crate) fn is_serving(&self) -> bool {
        !self.task.is_finished()
    }
    /// Close current connections (including upgraded WS), retaining the bound socket.
    pub(crate) fn disconnect_clients(&self) {
        for (_, socket) in self.severance.sockets().drain() {
            let _ = socket.shutdown(Shutdown::Both);
        }
    }
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// 失効で止める口（モジュール doc「失効による停止」）。
    pub fn revoker(&self) -> TlsRevoker {
        TlsRevoker {
            target: RevocationTarget::Listener {
                severance: self.severance.clone(),
                abort: Arc::new(self.task.abort_handle()),
            },
        }
    }

    pub async fn stop(mut self) {
        let _ = self.shutdown.send(());
        if tokio::time::timeout(STOP_TIMEOUT, &mut self.task)
            .await
            .is_err()
        {
            // Long-lived upgraded WebSockets can outlive graceful shutdown.
            self.task.abort();
            let _ = self.task.await;
        }
    }
}

pub(crate) async fn start(
    config: &TlsConfig,
    app: axum::Router,
) -> Result<RunningTlsListener, TlsError> {
    #[cfg(windows)]
    {
        start_inner(config, app, false, None).await
    }
    #[cfg(not(windows))]
    {
        start_inner(config, app, false).await
    }
}

/// 再 bind の serve は、永続の失効印を登録してからだけ動かす。
#[cfg(windows)]
pub(crate) async fn start_following(
    config: &TlsConfig,
    app: axum::Router,
    follows: &TlsRevoker,
) -> Result<RunningTlsListener, TlsError> {
    start_inner(config, app, false, Some((follows, false))).await
}

#[cfg(windows)]
pub(crate) async fn start_reserved(
    config: &TlsConfig,
    app: axum::Router,
) -> Result<RunningTlsListener, TlsError> {
    start_inner(
        config,
        app,
        true,
        #[cfg(windows)]
        None,
    )
    .await
}

#[cfg(windows)]
pub(crate) async fn start_renewed(
    config: &TlsConfig,
    app: axum::Router,
    follows: &TlsRevoker,
) -> Result<RunningTlsListener, TlsError> {
    start_inner(config, app, false, Some((follows, true))).await
}

/// Bound OS-selected shell port; never consume the disk identity's 8723 port.
/// Keep rejected sockets bound until another choice succeeds, so the OS cannot
/// immediately hand the same excluded port back. Four attempts bound recovery.
async fn reserved_shell_socket() -> io::Result<(TcpListener, std::net::TcpListener)> {
    let mut rejected = Vec::new();
    for _ in 0..4 {
        let socket =
            crate::bind_exclusive::bind_exclusive(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
                .await?;
        if !shell_port_allowed(socket.local_addr()?.port()) {
            rejected.push(socket);
            continue;
        }
        let socket = socket.into_std()?;
        let reservation = socket.try_clone()?;
        return Ok((TcpListener::from_std(socket)?, reservation));
    }
    Err(io::Error::new(
        io::ErrorKind::AddrInUse,
        "OS did not provide a shell port outside reserved 8723",
    ))
}

fn shell_port_allowed(port: u16) -> bool {
    port != 0 && port != banto_hub_tls_trust::DEFAULT_TLS_PORT
}

// WebKit hosts retain their HTTP UI compatibility path with an exact runtime
// capability and the same process-owned reserved socket; no TLS bypass is added.
#[cfg(not(windows))]
pub(crate) async fn start_plain_reserved(
    app: axum::Router,
) -> Result<RunningTlsListener, TlsError> {
    struct Plain {
        tcp: TcpListener,
        severance: Arc<Severance>,
    }
    impl Listener for Plain {
        type Io = SeverableTcp;
        type Addr = SocketAddr;
        async fn accept(&mut self) -> (Self::Io, Self::Addr) {
            loop {
                match self.tcp.accept().await {
                    Ok((stream, address)) => {
                        if let Some(stream) = self.severance.wrap(stream) {
                            return (stream, address);
                        }
                    }
                    Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
                }
            }
        }
        fn local_addr(&self) -> io::Result<Self::Addr> {
            self.tcp.local_addr()
        }
    }
    let (tcp, reservation) = reserved_shell_socket().await.map_err(TlsError::Bind)?;
    let local_addr = tcp.local_addr().map_err(TlsError::Bind)?;
    let severance = Arc::new(Severance::default());
    let listener = Plain {
        tcp,
        severance: severance.clone(),
    };
    let (shutdown, receiver) = oneshot::channel();
    let task = tokio::spawn(async move {
        let _ = axum::serve(
            listener.tap_io(|_| {}),
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async {
            let _ = receiver.await;
        })
        .await;
    });
    Ok(RunningTlsListener {
        local_addr,
        shutdown,
        task,
        severance,
        _reservation: Some(reservation),
    })
}

async fn start_inner(
    config: &TlsConfig,
    app: axum::Router,
    reserve: bool,
    #[cfg(windows)] follows: Option<(&TlsRevoker, bool)>,
) -> Result<RunningTlsListener, TlsError> {
    let acceptor = Arc::new(RwLock::new(config.acceptor()?));
    let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, config.port);
    let (tcp, reservation) = if reserve {
        let (tcp, reservation) = reserved_shell_socket().await.map_err(TlsError::Bind)?;
        (tcp, Some(reservation))
    } else {
        (
            crate::bind_exclusive::bind_exclusive(address)
                .await
                .map_err(TlsError::Bind)?,
            None,
        )
    };
    let local_addr = tcp.local_addr().map_err(TlsError::Bind)?;
    let severance = Arc::new(Severance::default());
    let listener = TlsListener {
        tcp,
        acceptor: acceptor.clone(),
        severance: severance.clone(),
        handshakes: JoinSet::new(),
    };
    let (shutdown, receiver) = oneshot::channel();
    // spawn と共有停止口への登録の間にも別 worker は動ける。accept は登録後まで待たせる。
    let (begin, begun) = oneshot::channel();
    let task = tokio::spawn(async move {
        if begun.await.is_err() {
            return;
        }
        let _ = axum::serve(
            // TapIo supplies axum's generic Connected<Addr> implementation
            // for custom listeners, preserving ConnectInfo<SocketAddr>.
            listener.tap_io(|_| {}),
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async {
            let _ = receiver.await;
        })
        .await;
    });
    let running = RunningTlsListener {
        #[cfg(windows)]
        acceptor: Some(acceptor),
        local_addr,
        shutdown,
        task,
        severance,
        _reservation: reservation,
    };
    #[cfg(windows)]
    if let Some((follows, renewed)) = follows {
        if renewed {
            if !follows.follow_renewed(running.revoker()) {
                running.stop().await;
                return Err(TlsError::Bind(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "TLS was externally stopped",
                )));
            }
        } else {
            follows.follow(running.revoker());
        }
    }
    let _ = begin.send(());
    Ok(running)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::get, Router};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[cfg(windows)]
    #[tokio::test]
    async fn renewal_preserves_external_stop_latch_and_old_revokers_target_new_listener() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let first = crate::hub_tls_prepare::generate_identity(now).unwrap();
        let second = crate::hub_tls_prepare::generate_identity(now).unwrap();
        let config = TlsConfig::in_memory(
            0,
            first.cert_pem.into_bytes(),
            Zeroizing::new(first.key_pem.as_bytes().to_vec()),
        );
        let listener = start(&config, Router::new()).await.unwrap();
        let address = listener.local_addr();
        let following = TlsRevoker::following(listener.revoker());
        let stale = following.clone();
        let next = TlsConfig::in_memory(
            address.port(),
            second.cert_pem.into_bytes(),
            Zeroizing::new(second.key_pem.as_bytes().to_vec()),
        );
        listener.replace_identity(&next, &following).unwrap();
        assert_eq!(listener.local_addr(), address);
        following.retire_identity();
        assert!(stale.is_revoked());
        assert!(!stale.externally_stopped());
        listener.stop().await;
        let replacement = start_renewed(&next, Router::new(), &following)
            .await
            .unwrap();
        assert!(
            !stale.is_revoked(),
            "trusted renewal only clears internal identity retirement"
        );
        stale.revoke();
        assert!(following.externally_stopped());
        replacement.stop().await;
        assert!(
            start_renewed(&next, Router::new(), &following)
                .await
                .is_err(),
            "public external stop is permanent across all renewals"
        );
    }

    #[test]
    fn shell_port_selection_excludes_disk_tls_and_unassigned_ports() {
        assert!(!shell_port_allowed(0));
        assert!(!shell_port_allowed(banto_hub_tls_trust::DEFAULT_TLS_PORT));
        assert!(shell_port_allowed(49152));
    }

    #[test]
    fn tls_env_requires_all_three_values_and_a_nonzero_port() {
        assert!(TlsConfig::parse(None, None, None).unwrap().is_none());
        assert!(matches!(
            TlsConfig::parse(Some("8723".into()), None, None),
            Err(TlsError::IncompleteConfig)
        ));
        for port in ["", "0", "65536", "not-a-port"] {
            assert!(matches!(
                TlsConfig::parse(
                    Some(port.into()),
                    Some("cert.pem".into()),
                    Some("key.pem".into())
                ),
                Err(TlsError::InvalidPort)
            ));
        }
        assert!(matches!(
            TlsConfig::parse(Some("8723".into()), Some("".into()), Some("key.pem".into())),
            Err(TlsError::EmptyPath)
        ));
        assert_eq!(
            TlsConfig::parse(
                Some("8723".into()),
                Some("cert.pem".into()),
                Some("key.pem".into())
            )
            .unwrap()
            .unwrap()
            .port(),
            8723
        );
    }

    #[tokio::test]
    async fn missing_certificate_refuses_before_binding() {
        let temp = tempfile::tempdir().unwrap();
        let config = TlsConfig {
            port: 0,
            identity: TlsIdentity::Files {
                cert: temp.path().join("missing-cert.pem"),
                key: temp.path().join("missing-key.pem"),
            },
        };
        assert!(matches!(
            start(&config, Router::new()).await,
            Err(TlsError::Certificate)
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn stop_aborts_a_stalled_server_task() {
        let (shutdown, _receiver) = oneshot::channel();
        let (held_sender, held_receiver) = oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            let _held_sender = held_sender;
            std::future::pending::<()>().await;
        });
        let server = RunningTlsListener {
            #[cfg(windows)]
            acceptor: None,
            local_addr: SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0).into(),
            shutdown,
            task,
            severance: Arc::default(),
            _reservation: None,
        };
        server.stop().await;
        assert!(held_receiver.await.is_err());
    }

    #[tokio::test]
    async fn temporary_tls_identity_serves_the_same_router_on_loopback() {
        let temp = tempfile::tempdir().unwrap();
        let cert = temp.path().join("server.pem");
        let key = temp.path().join("server.key");
        // This test-only identity exists solely inside a temporary directory.
        // No operator certificate or user key is created or saved by Hub.
        let output = std::process::Command::new("openssl")
            .args(["req", "-x509", "-newkey", "rsa:2048", "-nodes"])
            .arg("-keyout")
            .arg(&key)
            .arg("-out")
            .arg(&cert)
            .args([
                "-days",
                "1",
                "-subj",
                "/CN=localhost",
                "-addext",
                "subjectAltName=DNS:localhost,IP:127.0.0.1",
                "-addext",
                "basicConstraints=critical,CA:FALSE",
            ])
            .output();
        let output = match output {
            Ok(output) => output,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                eprintln!("openssl unavailable; skipping temporary TLS fixture test");
                return;
            }
            Err(err) => panic!("could not generate temporary TLS fixture: {err}"),
        };
        assert!(
            output.status.success(),
            "temporary TLS fixture generation failed"
        );

        let missing_key = TlsConfig {
            port: 0,
            identity: TlsIdentity::Files {
                cert: cert.clone(),
                key: temp.path().join("missing-key.pem"),
            },
        };
        assert!(matches!(missing_key.acceptor(), Err(TlsError::PrivateKey)));

        let unrelated_key = temp.path().join("unrelated.key");
        let output = std::process::Command::new("openssl")
            .args([
                "genpkey",
                "-algorithm",
                "RSA",
                "-pkeyopt",
                "rsa_keygen_bits:2048",
            ])
            .arg("-out")
            .arg(&unrelated_key)
            .output()
            .expect("openssl was available for the certificate fixture");
        assert!(output.status.success(), "temporary key generation failed");
        let mismatched_identity = TlsConfig {
            port: 0,
            identity: TlsIdentity::Files {
                cert: cert.clone(),
                key: unrelated_key,
            },
        };
        assert!(matches!(
            mismatched_identity.acceptor(),
            Err(TlsError::InvalidIdentity)
        ));

        let app = Router::new().route("/probe", get(|| async { "hub-route" }));
        let plain = banto_server::start(
            banto_server::ServerConfig {
                bind: "127.0.0.1".to_string(),
                port: 0,
            },
            app.clone(),
        )
        .await
        .unwrap();
        let config = TlsConfig {
            port: 0,
            identity: TlsIdentity::Files {
                cert: cert.clone(),
                key,
            },
        };
        let server = start(&config, app).await.unwrap();
        assert_eq!(server.local_addr().ip(), Ipv4Addr::LOCALHOST);

        let roots = {
            let certs: Vec<_> = CertificateDer::pem_file_iter(&cert)
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            let mut roots = tokio_rustls::rustls::RootCertStore::empty();
            roots.add(certs[0].clone()).unwrap();
            roots
        };
        let client_config = Arc::new(
            tokio_rustls::rustls::ClientConfig::builder_with_provider(Arc::new(
                tokio_rustls::rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth(),
        );
        // Exercise certificate validation and encryption without a socket;
        // some test sandboxes prohibit even loopback connects.
        let (client_io, server_io) = tokio::io::duplex(16 * 1024);
        let connector = tokio_rustls::TlsConnector::from(client_config.clone());
        let acceptor = config.acceptor().unwrap();
        let (mut memory_client, mut memory_server) = tokio::try_join!(
            connector.connect("localhost".try_into().unwrap(), client_io),
            acceptor.accept(server_io),
        )
        .unwrap();
        memory_client.write_all(b"tls-ok").await.unwrap();
        let mut marker = [0; 6];
        memory_server.read_exact(&mut marker).await.unwrap();
        assert_eq!(&marker, b"tls-ok");

        let stream = match TcpStream::connect(server.local_addr()).await {
            Ok(stream) => stream,
            Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {
                eprintln!("loopback connect denied by sandbox; router transport check skipped");
                server.stop().await;
                plain.stop().await;
                return;
            }
            Err(err) => panic!("loopback TLS connection failed: {err}"),
        };
        let mut tls = tokio_rustls::TlsConnector::from(client_config)
            .connect("localhost".try_into().unwrap(), stream)
            .await
            .unwrap();
        tls.write_all(b"GET /probe HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        tls.read_to_end(&mut response).await.unwrap();
        let response = String::from_utf8(response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.contains("hub-route"));

        let mut http = TcpStream::connect(plain.local_addr()).await.unwrap();
        http.write_all(b"GET /probe HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut http_response = Vec::new();
        http.read_to_end(&mut http_response).await.unwrap();
        assert!(String::from_utf8(http_response)
            .unwrap()
            .starts_with("HTTP/1.1 200"));
        server.stop().await;
        plain.stop().await;
    }

    /// #1121 PR2a: TLS の経路で、握った 127.0.0.1 のポートを同じ利用者の `SO_REUSEADDR` の
    /// 別ソケットが奪えない（回帰の確認）。127.0.0.1 では `SO_EXCLUSIVEADDRUSE` が無くても
    /// 断られるので、付与そのものの確認は `bind_exclusive` の
    /// `bound_listener_has_exclusive_addr_use` が行う（`start` が持つ listener は task に
    /// 渡るので、ここからは getsockopt できない）。
    #[cfg(windows)]
    #[tokio::test]
    async fn tls_listener_loopback_port_rejects_reuseaddr_bind() {
        use socket2::{Domain, Socket, Type};

        let key_pair = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let cert = temp.path().join("server.pem");
        let key = temp.path().join("server.key");
        std::fs::write(&cert, key_pair.cert.pem()).unwrap();
        std::fs::write(&key, key_pair.signing_key.serialize_pem()).unwrap();
        let config = TlsConfig {
            port: 0,
            identity: TlsIdentity::Files { cert, key },
        };
        let server = start(&config, Router::new()).await.unwrap();
        let addr = SocketAddr::V4(SocketAddrV4::new(
            Ipv4Addr::LOCALHOST,
            server.local_addr().port(),
        ));
        let socket = Socket::new(Domain::for_address(addr), Type::STREAM, None).unwrap();
        socket.set_reuse_address(true).unwrap();
        assert!(socket.bind(&addr.into()).is_err(), "{addr} was taken over");
        server.stop().await;
    }

    /// 失効の試験用: rcgen で作った一時の証明書で待ち受け、応答を流し続ける `/stream` を持つ。
    async fn streaming_server() -> (RunningTlsListener, Arc<tokio_rustls::rustls::ClientConfig>) {
        let key_pair = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        let cert_pem = key_pair.cert.pem().into_bytes();
        let key_pem = Zeroizing::new(key_pair.signing_key.serialize_pem().into_bytes());
        let config = TlsConfig::in_memory(0, cert_pem, key_pem);
        let app = Router::new()
            .route(
                "/stream",
                get(|| async {
                    let ticks = futures_util::stream::unfold((), |()| async {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        Some((Ok::<_, io::Error>(b"tick\n".to_vec()), ()))
                    });
                    axum::body::Body::from_stream(ticks)
                }),
            )
            .route(
                "/hold",
                get(|| async {
                    // 1 回だけ流して、あとは書きも読みもしない（失効の印だけでは起きない接続）。
                    let once = futures_util::stream::unfold(true, |first| async move {
                        if first {
                            Some((Ok::<_, io::Error>(b"tick\n".to_vec()), false))
                        } else {
                            std::future::pending::<()>().await;
                            None
                        }
                    });
                    axum::body::Body::from_stream(once)
                }),
            );
        let server = start(&config, app).await.unwrap();
        let mut roots = tokio_rustls::rustls::RootCertStore::empty();
        roots.add(key_pair.cert.der().clone()).unwrap();
        let client = Arc::new(
            tokio_rustls::rustls::ClientConfig::builder_with_provider(Arc::new(
                tokio_rustls::rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth(),
        );
        (server, client)
    }

    /// `path` に TLS で繋いで、流れ始めるまで読む。
    async fn open_stream(
        addr: SocketAddr,
        client: Arc<tokio_rustls::rustls::ClientConfig>,
        path: &str,
    ) -> tokio_rustls::client::TlsStream<TcpStream> {
        let stream = TcpStream::connect(addr).await.unwrap();
        let mut tls = tokio_rustls::TlsConnector::from(client)
            .connect("localhost".try_into().unwrap(), stream)
            .await
            .unwrap();
        tls.write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut seen = Vec::new();
        let mut buf = [0u8; 1024];
        while !String::from_utf8_lossy(&seen).contains("tick") {
            let n = tokio::time::timeout(Duration::from_secs(5), tls.read(&mut buf))
                .await
                .expect("流れ始めない")
                .unwrap();
            assert!(n > 0, "流れる前に閉じた");
            seen.extend_from_slice(&buf[..n]);
        }
        tls
    }

    /// 流れ続けている接続が、`within` のうちに閉じる（EOF かエラー）か。
    async fn closes_within(
        tls: &mut tokio_rustls::client::TlsStream<TcpStream>,
        within: Duration,
    ) -> bool {
        let deadline = tokio::time::Instant::now() + within;
        let mut buf = [0u8; 1024];
        loop {
            match tokio::time::timeout_at(deadline, tls.read(&mut buf)).await {
                Err(_) => return false,
                Ok(Ok(0)) | Ok(Err(_)) => return true,
                Ok(Ok(_)) => continue,
            }
        }
    }

    /// #1121 PR2c: 失効の停止は、張られた長く続く接続（応答を流し続ける）をすぐ切り、待ち受けの
    /// ポートも閉じる。比べとして、通常の停止（graceful → 上限で abort）では、その接続は残る
    /// （axum の serve は接続ごとにタスクを spawn するため。モジュール doc「失効による停止」）。
    #[tokio::test]
    async fn revoke_cuts_a_long_lived_connection_that_graceful_stop_leaves() {
        // 失効の停止: すぐ閉じる。
        let (server, client) = streaming_server().await;
        let addr = server.local_addr();
        let mut tls = open_stream(addr, client.clone(), "/stream").await;
        // 書きも読みもしていない接続も、OS の上で閉じるので切れる（印だけでは起きない）。
        let mut idle = open_stream(addr, client.clone(), "/hold").await;
        let revoker = server.revoker();
        assert!(!revoker.is_revoked());
        revoker.revoke();
        assert!(revoker.is_revoked());
        assert!(
            closes_within(&mut tls, Duration::from_secs(2)).await,
            "失効の停止の後も接続が流れ続けた"
        );
        assert!(
            closes_within(&mut idle, Duration::from_secs(2)).await,
            "失効の停止の後も止まっている接続が残った"
        );
        // 待ち受けも閉じる（abort は非同期なので少し待つ）。
        let mut refused = false;
        for _ in 0..50 {
            if TcpStream::connect(addr).await.is_err() {
                refused = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(refused, "失効の停止の後も {addr} が受け付けた");
        server.stop().await;

        // 通常の停止: 上限（STOP_TIMEOUT）で serve を abort しても、接続は流れ続ける。
        let (server, client) = streaming_server().await;
        let mut tls = open_stream(server.local_addr(), client, "/stream").await;
        server.stop().await;
        assert!(
            !closes_within(&mut tls, Duration::from_millis(500)).await,
            "通常の停止で接続が切れた（比べの前提が変わった）"
        );
    }
    #[cfg(windows)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn following_revoker_severs_new_listener_when_revoked_before_registration() {
        let identity = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let config = TlsConfig::in_memory(
            0,
            identity.cert.pem().into_bytes(),
            Zeroizing::new(identity.signing_key.serialize_pem().into_bytes()),
        );
        let initial = start(&config, Router::new()).await.unwrap();
        let stable = TlsRevoker::following(initial.revoker());
        initial.stop().await;
        assert!(
            !stable.is_revoked(),
            "temporary listener stop must not permanently revoke the monitor"
        );
        stable.revoke();
        let replacement = start_following(&config, Router::new(), &stable)
            .await
            .unwrap();
        // 登録時に既に失効なら、開始ゲートを開ける前に必ず serve と TCP を失効させる。
        assert!(replacement.revoker().is_revoked());
        let address = replacement.local_addr();
        replacement.stop().await;
        assert!(TcpStream::connect(address).await.is_err());
        assert!(stable.is_revoked());
    }
}
