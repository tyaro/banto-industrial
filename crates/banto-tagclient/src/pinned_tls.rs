//! A certificate pin for the Hub connection used by the Sink. The public
//! certificates come from the Sink's protected configuration, never from the
//! endpoint being authenticated. Every TLS handshake checks the exact leaf
//! certificate and then performs rustls' normal name, time, chain and
//! handshake-signature verification.

use std::fmt;
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{
    ClientConfig, DigitallySignedStruct, Error as TlsError, RootCertStore, SignatureScheme,
};

use crate::error::{Error, ErrorKind, Result};

#[derive(Clone)]
pub struct PinnedHubTls {
    config: Arc<ClientConfig>,
}

impl fmt::Debug for PinnedHubTls {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PinnedHubTls(<configured>)")
    }
}

impl PinnedHubTls {
    /// Each PEM entry is one trusted, directly pinned Hub server certificate.
    /// Multiple entries permit a bounded old/new certificate rotation.
    pub fn from_pem(pems: &[String]) -> Result<Self> {
        if pems.is_empty() || pems.len() > 2 {
            return Err(Error::new(ErrorKind::InvalidEndpoint));
        }
        let mut roots = RootCertStore::empty();
        let mut pinned = Vec::with_capacity(pems.len());
        for pem in pems {
            let certs = CertificateDer::pem_slice_iter(pem.as_bytes())
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|_| Error::new(ErrorKind::InvalidEndpoint))?;
            if certs.len() != 1 || pem.contains("PRIVATE KEY") {
                return Err(Error::new(ErrorKind::InvalidEndpoint));
            }
            roots
                .add(certs[0].clone())
                .map_err(|_| Error::new(ErrorKind::InvalidEndpoint))?;
            pinned.push(certs[0].as_ref().to_vec());
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let verifier = rustls::client::WebPkiServerVerifier::builder_with_provider(
            Arc::new(roots),
            provider.clone(),
        )
        .build()
        .map_err(|_| Error::new(ErrorKind::InvalidEndpoint))?;
        let config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|_| Error::new(ErrorKind::InvalidEndpoint))?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(ExactCertificateVerifier {
                pinned,
                normal: verifier,
            }))
            .with_no_client_auth();
        Ok(Self {
            config: Arc::new(config),
        })
    }

    pub(crate) fn rustls_config(&self) -> Arc<ClientConfig> {
        self.config.clone()
    }

    /// Apply the same verifier to a separate REST client used by the Sink.
    pub fn configure_reqwest(&self, builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
        builder.tls_backend_preconfigured((*self.config).clone())
    }

    /// Give an independently established WebSocket the same pin verifier.
    pub fn websocket_connector(&self) -> tokio_tungstenite::Connector {
        tokio_tungstenite::Connector::Rustls(self.config.clone())
    }
}

#[derive(Debug)]
struct ExactCertificateVerifier {
    pinned: Vec<Vec<u8>>,
    normal: Arc<dyn ServerCertVerifier>,
}

impl ServerCertVerifier for ExactCertificateVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, TlsError> {
        if !self.pinned.iter().any(|pin| pin == end_entity.as_ref()) {
            return Err(TlsError::InvalidCertificate(
                rustls::CertificateError::UnknownIssuer,
            ));
        }
        self.normal
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, TlsError> {
        self.normal.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, TlsError> {
        self.normal.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.normal.supported_verify_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio_rustls::TlsAcceptor;

    use crate::{Endpoint, RestClient, SecretApiKey};

    const CATALOG: &str = r#"{"revision":1,"run_id":2,"collection_mode":"configured","tags":[]}"#;

    struct TestCertificate {
        _temp: tempfile::TempDir,
        pem: String,
        server: Arc<rustls::ServerConfig>,
    }

    impl TestCertificate {
        fn generate() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let cert = temp.path().join("server.pem");
            let key = temp.path().join("server.key");
            let output = Command::new("openssl")
                .args([
                    "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
                ])
                .arg("-keyout")
                .arg(&key)
                .arg("-out")
                .arg(&cert)
                .args([
                    "-subj",
                    "/CN=localhost",
                    "-addext",
                    "subjectAltName=DNS:localhost,IP:127.0.0.1",
                    "-addext",
                    "basicConstraints=critical,CA:FALSE",
                ])
                .output()
                .expect("OpenSSL is required for the temporary TLS fixture");
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let pem = std::fs::read_to_string(&cert).unwrap();
            let certs = CertificateDer::pem_file_iter(&cert)
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap();
            let key = rustls::pki_types::PrivateKeyDer::from_pem_file(&key).unwrap();
            let server = rustls::ServerConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .unwrap();
            Self {
                _temp: temp,
                pem,
                server: Arc::new(server),
            }
        }
    }

    async fn http_once(
        cert: &TestCertificate,
        response: String,
    ) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("https://{}", listener.local_addr().unwrap());
        let acceptor = TlsAcceptor::from(cert.server.clone());
        let handle = tokio::spawn(async move {
            let Ok(Ok((stream, _))) =
                tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept()).await
            else {
                return String::new();
            };
            let Ok(Ok(mut stream)) =
                tokio::time::timeout(std::time::Duration::from_secs(5), acceptor.accept(stream))
                    .await
            else {
                return String::new();
            };
            let mut request = Vec::new();
            let mut chunk = [0u8; 1024];
            while request.len() < 8192 {
                let count = stream.read(&mut chunk).await.unwrap_or(0);
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..count]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            if !request.is_empty() {
                stream.write_all(response.as_bytes()).await.unwrap();
            }
            String::from_utf8(request).unwrap()
        });
        (url, handle)
    }

    fn catalog_response() -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{CATALOG}",
            CATALOG.len()
        )
    }

    fn client(url: &str, pins: PinnedHubTls) -> RestClient {
        RestClient::with_pinned_tls(
            Endpoint::new(url).unwrap(),
            SecretApiKey::new("bh_dummy.secret".to_string()).unwrap(),
            std::time::Duration::from_secs(3),
            pins,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn rest_pin_accepts_verified_certificate_and_rotation_pair() {
        let first = TestCertificate::generate();
        let second = TestCertificate::generate();
        let pins = PinnedHubTls::from_pem(&[first.pem.clone(), second.pem.clone()]).unwrap();
        for cert in [&first, &second] {
            let (url, server) = http_once(cert, catalog_response()).await;
            assert_eq!(
                client(&url, pins.clone())
                    .fetch_catalog()
                    .await
                    .unwrap()
                    .revision,
                1
            );
            let request = server.await.unwrap();
            assert!(request.contains("Bearer bh_dummy.secret"), "{request}");
        }
    }

    #[tokio::test]
    async fn wrong_pin_fails_before_the_bearer_header_reaches_the_listener() {
        let real = TestCertificate::generate();
        let other = TestCertificate::generate();
        let pins = PinnedHubTls::from_pem(&[other.pem]).unwrap();
        let (url, server) = http_once(&real, catalog_response()).await;
        assert!(client(&url, pins).fetch_catalog().await.is_err());
        assert!(server.await.unwrap().is_empty());
    }

    #[tokio::test]
    #[allow(clippy::result_large_err)] // tungstenite fixes the callback's error type.
    async fn websocket_reconnect_rechecks_the_pin_before_sending_bearer() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

        let first = TestCertificate::generate();
        let replacement = TestCertificate::generate();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("https://{}", listener.local_addr().unwrap());
        let seen = Arc::new(AtomicUsize::new(0));
        let observed = seen.clone();
        let first_server = first.server.clone();
        let replacement_server = replacement.server.clone();
        let server = tokio::spawn(async move {
            for config in [first_server, replacement_server] {
                let (stream, _) =
                    tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept())
                        .await
                        .unwrap()
                        .unwrap();
                let Ok(stream) = TlsAcceptor::from(config).accept(stream).await else {
                    continue;
                };
                let observed = observed.clone();
                let _ = tokio_tungstenite::accept_hdr_async(
                    stream,
                    move |request: &Request, response: Response| {
                        if request
                            .headers()
                            .get("authorization")
                            .is_some_and(|value| value == "Bearer bh_dummy.secret")
                        {
                            observed.fetch_add(1, Ordering::SeqCst);
                        }
                        Ok(response)
                    },
                )
                .await;
            }
        });
        let pins = PinnedHubTls::from_pem(std::slice::from_ref(&first.pem)).unwrap();
        let client = client(&url, pins);
        assert!(client.connect_stream().await.is_ok());
        assert!(client.connect_stream().await.is_err());
        server.await.unwrap();
        assert_eq!(seen.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn redirect_is_not_followed_to_an_unpinned_http_listener() {
        let cert = TestCertificate::generate();
        let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let location = format!("http://{}/stolen", target.local_addr().unwrap());
        let response = format!(
            "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let (url, server) = http_once(&cert, response).await;
        let pins = PinnedHubTls::from_pem(std::slice::from_ref(&cert.pem)).unwrap();
        assert!(client(&url, pins).fetch_catalog().await.is_err());
        assert!(server.await.unwrap().contains("Bearer bh_dummy.secret"));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), target.accept())
                .await
                .is_err()
        );
    }

    #[test]
    fn pin_config_rejects_missing_or_non_certificate_material() {
        assert!(PinnedHubTls::from_pem(&[]).is_err());
        assert!(PinnedHubTls::from_pem(&["not a certificate".into()]).is_err());
        let cert = TestCertificate::generate();
        assert!(PinnedHubTls::from_pem(&[cert.pem.clone(), cert.pem.clone(), cert.pem]).is_err());
    }
}
