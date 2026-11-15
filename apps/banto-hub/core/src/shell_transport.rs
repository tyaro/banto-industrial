//! Process-owned HTTPS origin for the desktop shell. This transport deliberately
//! outlives every Hub runtime; detaching closes clients and serves only 503.
//! The shell retains it until process exit, including during host switches.
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use axum::{
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Router,
};
#[cfg(windows)]
use std::time::{SystemTime, UNIX_EPOCH};
use tower::ServiceExt;
#[cfg(windows)]
use zeroize::Zeroizing;

#[cfg(windows)]
use crate::tls_listener::TlsConfig;
use crate::tls_listener::{self, RunningTlsListener};

struct Dispatch {
    generation: u64,
    router: Option<Router>,
    cancel: tokio::sync::watch::Sender<bool>,
}

/// A shell must retain this handle for its entire process lifetime. No runtime
/// shutdown path consumes it or closes its OS-assigned exclusive loopback port.
pub struct ShellTransport {
    listener: RunningTlsListener,
    certificate: Vec<u8>,
    dispatch: Arc<Mutex<Dispatch>>,
}

impl ShellTransport {
    pub async fn start() -> Result<Arc<Self>, String> {
        #[cfg(windows)]
        let (certificate, config) = {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_secs() as i64;
            let identity = crate::hub_tls_prepare::generate_identity(now)?;
            let certificate = identity.cert_pem.into_bytes();
            let config = TlsConfig::in_memory(
                0,
                certificate.clone(),
                Zeroizing::new(identity.key_pem.as_bytes().to_vec()),
            );
            (certificate, config)
        };
        #[cfg(not(windows))]
        let certificate = Vec::new();
        let dispatch = Arc::new(Mutex::new(Dispatch {
            generation: 0,
            router: None,
            cancel: tokio::sync::watch::channel(false).0,
        }));
        let router = Router::new().fallback(forward).with_state(dispatch.clone());
        #[cfg(windows)]
        let listener = tls_listener::start_reserved(&config, router)
            .await
            .map_err(|e| e.to_string())?;
        #[cfg(not(windows))]
        let listener = tls_listener::start_plain_reserved(router)
            .await
            .map_err(|e| e.to_string())?;
        Ok(Arc::new(Self {
            listener,
            certificate,
            dispatch,
        }))
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.listener.local_addr()
    }
    pub fn is_serving(&self) -> bool {
        self.listener.is_serving()
    }
    pub fn certificate_pem(&self) -> &[u8] {
        &self.certificate
    }
    pub fn url(&self) -> String {
        format!(
            "{}://{}/",
            if cfg!(windows) { "https" } else { "http" },
            self.local_addr()
        )
    }

    pub(crate) fn attach(self: &Arc<Self>, router: Router) -> Attachment {
        let mut dispatch = self.dispatch.lock().unwrap_or_else(|e| e.into_inner());
        dispatch.generation += 1;
        dispatch.cancel.send_replace(true);
        dispatch.cancel = tokio::sync::watch::channel(false).0;
        dispatch.router = Some(router);
        self.listener.disconnect_clients();
        Attachment {
            transport: self.clone(),
            generation: dispatch.generation,
        }
    }
}

pub(crate) struct Attachment {
    transport: Arc<ShellTransport>,
    generation: u64,
}
impl Drop for Attachment {
    fn drop(&mut self) {
        let mut dispatch = self
            .transport
            .dispatch
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if dispatch.generation == self.generation {
            dispatch.router = None;
            dispatch.cancel.send_replace(true);
            self.transport.listener.disconnect_clients();
        }
    }
}

async fn forward(State(dispatch): State<Arc<Mutex<Dispatch>>>, request: Request) -> Response {
    let (router, mut cancelled) = {
        let dispatch = dispatch.lock().unwrap_or_else(|e| e.into_inner());
        (dispatch.router.clone(), dispatch.cancel.subscribe())
    };
    match router {
        Some(router) => {
            if *cancelled.borrow() {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            tokio::select! { biased;
                _ = cancelled.changed() => StatusCode::SERVICE_UNAVAILABLE.into_response(),
                response = router.oneshot(request) => response.unwrap(),
            }
        }
        None => (StatusCode::SERVICE_UNAVAILABLE, "Hub is stopped").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpStream,
    };
    #[cfg(windows)]
    use tokio_rustls::rustls::{
        self,
        pki_types::{pem::PemObject, CertificateDer, ServerName},
    };

    async fn request(transport: &ShellTransport) -> String {
        let tcp = TcpStream::connect(transport.local_addr()).await.unwrap();
        #[cfg(windows)]
        let mut stream = {
            let certificate = CertificateDer::from_pem_slice(transport.certificate_pem()).unwrap();
            let mut roots = rustls::RootCertStore::empty();
            roots.add(certificate).unwrap();
            let config = rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
            tokio_rustls::TlsConnector::from(Arc::new(config))
                .connect(
                    ServerName::IpAddress(std::net::Ipv4Addr::LOCALHOST.into()),
                    tcp,
                )
                .await
                .unwrap()
        };
        #[cfg(not(windows))]
        let mut stream = tcp;
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        String::from_utf8(response).unwrap()
    }

    #[tokio::test]
    async fn detach_and_reattach_keep_origin_and_memory_pin_and_serve_no_stopped_hub() {
        let transport = ShellTransport::start().await.unwrap();
        let origin = transport.local_addr();
        assert_ne!(origin.port(), banto_hub_tls_trust::DEFAULT_TLS_PORT);
        let certificate = transport.certificate_pem().to_vec();
        assert!(request(&transport).await.starts_with("HTTP/1.1 503"));
        let old = transport
            .attach(Router::new().route("/", axum::routing::get(|| async { "first Hub" })));
        assert!(request(&transport).await.ends_with("first Hub"));
        drop(old);
        assert!(request(&transport).await.starts_with("HTTP/1.1 503"));
        assert!(
            std::net::TcpListener::bind(origin).is_err(),
            "origin stays reserved while detached"
        );
        let new = transport
            .attach(Router::new().route("/", axum::routing::get(|| async { "second Hub" })));
        assert!(request(&transport).await.ends_with("second Hub"));
        assert_eq!(origin, transport.local_addr());
        assert_eq!(certificate, transport.certificate_pem());
        drop(new);
        // The process shell keeps its handle until exit. Test teardown consumes it.
        Arc::try_unwrap(transport)
            .ok()
            .unwrap()
            .listener
            .stop()
            .await;
    }

    #[tokio::test]
    async fn abnormal_serve_exit_does_not_release_authorized_origin() {
        let transport = ShellTransport::start().await.unwrap();
        let origin = transport.local_addr();
        assert_ne!(origin.port(), banto_hub_tls_trust::DEFAULT_TLS_PORT);
        transport.listener.revoker().revoke();
        tokio::task::yield_now().await;
        assert!(std::net::TcpListener::bind(origin).is_err());
        Arc::try_unwrap(transport)
            .ok()
            .unwrap()
            .listener
            .stop()
            .await;
    }
}
