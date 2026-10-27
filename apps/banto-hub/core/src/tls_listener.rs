//! Optional, dedicated loopback TLS transport for the existing Hub router.
//! The operator supplies PEM files; this module neither creates certificates
//! nor changes their permissions. Protecting the private-key file is an
//! installation responsibility. The listener binds IPv4 127.0.0.1 only;
//! callers should use that literal address rather than localhost or ::1.

use std::{
    env,
    ffi::OsString,
    io,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use axum::serve::{Listener, ListenerExt};
use thiserror::Error;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::oneshot,
    task::{JoinHandle, JoinSet},
};
use tokio_rustls::{
    rustls::{
        pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer},
        ServerConfig,
    },
    server::TlsStream,
    TlsAcceptor,
};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_PENDING_HANDSHAKES: usize = 32;
const STOP_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone)]
pub(crate) struct TlsConfig {
    port: u16,
    cert: PathBuf,
    key: PathBuf,
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
        Ok(Self { port, cert, key })
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

    fn acceptor(&self) -> Result<TlsAcceptor, TlsError> {
        // Do not include the key file or parser's input in errors or logs.
        let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(&self.cert)
            .map_err(|_| TlsError::Certificate)?
            .collect::<Result<_, _>>()
            .map_err(|_| TlsError::Certificate)?;
        if certs.is_empty() {
            return Err(TlsError::Certificate);
        }
        let key = PrivateKeyDer::from_pem_file(&self.key).map_err(|_| TlsError::PrivateKey)?;
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

struct TlsListener {
    tcp: TcpListener,
    acceptor: TlsAcceptor,
    handshakes: JoinSet<Option<(TlsStream<TcpStream>, SocketAddr)>>,
}

impl Listener for TlsListener {
    type Io = TlsStream<TcpStream>;
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
                            let acceptor = self.acceptor.clone();
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
    local_addr: SocketAddr,
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl RunningTlsListener {
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
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
    let acceptor = config.acceptor()?;
    let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, config.port);
    let tcp = TcpListener::bind(address).await.map_err(TlsError::Bind)?;
    let local_addr = tcp.local_addr().map_err(TlsError::Bind)?;
    let listener = TlsListener {
        tcp,
        acceptor,
        handshakes: JoinSet::new(),
    };
    let (shutdown, receiver) = oneshot::channel();
    let task = tokio::spawn(async move {
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
    Ok(RunningTlsListener {
        local_addr,
        shutdown,
        task,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::get, Router};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
            .port,
            8723
        );
    }

    #[tokio::test]
    async fn missing_certificate_refuses_before_binding() {
        let temp = tempfile::tempdir().unwrap();
        let config = TlsConfig {
            port: 0,
            cert: temp.path().join("missing-cert.pem"),
            key: temp.path().join("missing-key.pem"),
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
            local_addr: SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0).into(),
            shutdown,
            task,
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
            cert: cert.clone(),
            key: temp.path().join("missing-key.pem"),
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
            cert: cert.clone(),
            key: unrelated_key,
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
            cert: cert.clone(),
            key,
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
}
