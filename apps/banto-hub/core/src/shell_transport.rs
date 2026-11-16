//! Process-owned HTTPS origin for the desktop shell. This transport deliberately
//! outlives every Hub runtime; detaching closes clients and serves only 503.
//! Accepted handlers run independently of clients and remain in the Hub's
//! detached-task accounting until completion, even after the bounded drain.
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

use crate::detached::DetachedTasks;
#[cfg(windows)]
use crate::tls_listener::TlsConfig;
use crate::tls_listener::{self, RunningTlsListener};

const HANDLER_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

#[derive(Clone)]
struct AttachedRouter {
    router: Router,
    in_flight: DetachedTasks,
    hub_tasks: DetachedTasks,
}

struct Dispatch {
    generation: u64,
    router: Option<AttachedRouter>,
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
        Self::start_avoiding(&[crate::settings::DEFAULT_PORT]).await
    }

    /// Exclude the Hub's resolved/configured plain listener ports as well as 8723.
    pub async fn start_avoiding(excluded_ports: &[u16]) -> Result<Arc<Self>, String> {
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
        }));
        let router = Router::new().fallback(forward).with_state(dispatch.clone());
        #[cfg(windows)]
        let listener = tls_listener::start_reserved(&config, router, excluded_ports)
            .await
            .map_err(|e| e.to_string())?;
        #[cfg(not(windows))]
        let listener = tls_listener::start_plain_reserved(router, excluded_ports)
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

    /// Production keeps this transport until process exit; tests must join it.
    #[cfg(test)]
    pub(crate) async fn stop_for_test(self: Arc<Self>) {
        Arc::try_unwrap(self)
            .ok()
            .expect("unshared test transport")
            .listener
            .stop()
            .await;
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

    pub(crate) fn attach(self: &Arc<Self>, router: Router, hub_tasks: DetachedTasks) -> Attachment {
        let mut dispatch = self.dispatch.lock().unwrap_or_else(|e| e.into_inner());
        dispatch.generation += 1;
        if let Some(old) = dispatch.router.take() {
            old.in_flight.close();
        }
        let in_flight = DetachedTasks::new();
        dispatch.router = Some(AttachedRouter {
            router,
            in_flight: in_flight.clone(),
            hub_tasks,
        });
        self.listener.disconnect_clients();
        Attachment {
            transport: self.clone(),
            generation: dispatch.generation,
            in_flight,
        }
    }
}

pub(crate) struct Attachment {
    transport: Arc<ShellTransport>,
    generation: u64,
    in_flight: DetachedTasks,
}
impl Attachment {
    fn detach_now(&self) {
        let mut dispatch = self
            .transport
            .dispatch
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if dispatch.generation == self.generation && dispatch.router.is_some() {
            dispatch.router = None;
            self.transport.listener.disconnect_clients();
        }
        self.in_flight.close();
    }

    pub(crate) async fn detach(self) {
        self.detach_with_timeout(HANDLER_DRAIN_TIMEOUT).await;
    }

    async fn detach_with_timeout(self, timeout: std::time::Duration) {
        self.detach_now();
        if !self.in_flight.wait(timeout).await {
            crate::hub_log::log_err_line("[WARN] banto-hub: 管理画面の処理が切り離しの待機期限内に完了しませんでした。処理を中断せず、DB を閉じる前の待機へ引き継ぎます");
        }
    }
}
impl Drop for Attachment {
    fn drop(&mut self) {
        self.detach_now();
    }
}

async fn forward(State(dispatch): State<Arc<Mutex<Dispatch>>>, request: Request) -> Response {
    let response = {
        let dispatch = dispatch.lock().unwrap_or_else(|e| e.into_inner());
        // Acceptance and detachment are ordered by this lock. Spawn before
        // releasing it so shutdown cannot miss a newly accepted handler.
        dispatch.router.as_ref().and_then(|attached| {
            let handler = attached
                .in_flight
                .spawn(attached.router.clone().oneshot(request))?;
            Some(attached.hub_tasks.spawn_always(handler))
        })
    };
    match response {
        Some(response) => match response.await {
            Ok(Ok(Ok(response))) => response,
            _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        },
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
        let old = transport.attach(
            Router::new().route("/", axum::routing::get(|| async { "first Hub" })),
            DetachedTasks::new(),
        );
        assert!(request(&transport).await.ends_with("first Hub"));
        drop(old);
        assert!(request(&transport).await.starts_with("HTTP/1.1 503"));
        assert!(
            std::net::TcpListener::bind(origin).is_err(),
            "origin stays reserved while detached"
        );
        let new = transport.attach(
            Router::new().route("/", axum::routing::get(|| async { "second Hub" })),
            DetachedTasks::new(),
        );
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
    async fn disconnect_and_expired_detach_drain_do_not_drop_an_accepted_handler() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;
        let transport = ShellTransport::start().await.unwrap();
        let hub_tasks = DetachedTasks::new();
        let started = Arc::new(tokio::sync::Notify::new());
        let finish = Arc::new(tokio::sync::Notify::new());
        let completed = Arc::new(AtomicBool::new(false));
        let router = Router::new().route(
            "/",
            axum::routing::get({
                let started = started.clone();
                let finish = finish.clone();
                let completed = completed.clone();
                move || {
                    let started = started.clone();
                    let finish = finish.clone();
                    let completed = completed.clone();
                    async move {
                        started.notify_one();
                        finish.notified().await;
                        // Stand in for the durable write after an await point.
                        completed.store(true, Ordering::SeqCst);
                        "saved"
                    }
                }
            }),
        );
        let attachment = transport.attach(router, hub_tasks.clone());
        let client = tokio::spawn({
            let transport = transport.clone();
            async move { request(&transport).await }
        });
        tokio::time::timeout(Duration::from_secs(2), started.notified())
            .await
            .unwrap();
        client.abort(); // Hyper may now drop the service future independently of detach.
        let _ = client.await;
        attachment
            .detach_with_timeout(Duration::from_millis(20))
            .await;
        assert!(!completed.load(Ordering::SeqCst));
        assert_eq!(
            hub_tasks.pending(),
            1,
            "expired drain must retain DB-close accounting"
        );
        assert!(request(&transport).await.starts_with("HTTP/1.1 503"));
        hub_tasks.close();
        finish.notify_one();
        assert!(hub_tasks.wait(Duration::from_secs(2)).await);
        assert!(
            completed.load(Ordering::SeqCst),
            "client disconnect/detach must not cancel the write"
        );
        Arc::try_unwrap(transport)
            .ok()
            .unwrap()
            .listener
            .stop()
            .await;
    }

    #[tokio::test]
    async fn detach_waits_for_an_accepted_handler_within_its_deadline() {
        use std::time::Duration;
        let transport = ShellTransport::start().await.unwrap();
        let hub_tasks = DetachedTasks::new();
        let started = Arc::new(tokio::sync::Notify::new());
        let finish = Arc::new(tokio::sync::Notify::new());
        let router = Router::new().route(
            "/",
            axum::routing::get({
                let started = started.clone();
                let finish = finish.clone();
                move || {
                    let started = started.clone();
                    let finish = finish.clone();
                    async move {
                        started.notify_one();
                        finish.notified().await;
                        "saved"
                    }
                }
            }),
        );
        let attachment = transport.attach(router, hub_tasks.clone());
        let client = tokio::spawn({
            let transport = transport.clone();
            async move { request(&transport).await }
        });
        tokio::time::timeout(Duration::from_secs(2), started.notified())
            .await
            .unwrap();
        let drain = tokio::spawn(attachment.detach_with_timeout(Duration::from_secs(2)));
        // Observe detachment under the same dispatch lock, without a sleep race.
        tokio::time::timeout(Duration::from_secs(2), async {
            while transport.dispatch.lock().unwrap().router.is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!drain.is_finished());
        finish.notify_one();
        drain.await.unwrap();
        assert!(hub_tasks.wait(Duration::from_secs(2)).await);
        let _ = client.await;
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
