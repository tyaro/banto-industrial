//! #644 PR4: [`PinnedHubHealthProbe`] の判定の表（モジュール doc「判定」）。TLS の相手は rcgen で
//! 作った証明書（`prepare-hub-tls` と同じ `generate_identity`）で待ち受ける素朴な rustls の
//! サーバー、平文の口は素朴な TCP のサーバーで立てる。

use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::rustls::{ServerConfig, ServerConnection};

use super::*;
use crate::profile_lock::lock_file_path;
use crate::profile_paths::resolve_profile_paths;
use crate::test_support::TempDir;

const OPENAPI: &str = r#"{"openapi":"3.1.0","info":{"title":"banto-hub","version":"v1","x-banto-hub-profile-id":"default"},"paths":{}}"#;

fn ok_response(body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

fn identity() -> crate::hub_tls_prepare::Identity {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    crate::hub_tls_prepare::generate_identity(now).unwrap()
}

/// 1 回だけ TLS で受け、受け取った要求（握手が済まなければ空）を返す。
fn spawn_tls_server(identity: &crate::hub_tls_prepare::Identity) -> (u16, JoinHandle<Vec<u8>>) {
    let certs = CertificateDer::pem_slice_iter(identity.cert_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let key = PrivateKeyDer::from_pem_slice(identity.key_pem.as_bytes()).unwrap();
    let config = Arc::new(
        ServerConfig::builder_with_provider(Arc::new(
            tokio_rustls::rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap(),
    );
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = thread::spawn(move || {
        let Ok((tcp, _)) = listener.accept() else {
            return Vec::new();
        };
        tcp.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let conn = ServerConnection::new(config).unwrap();
        let mut stream = StreamOwned::new(conn, tcp);
        let mut request = Vec::new();
        let mut chunk = [0u8; 1024];
        while request.len() < 8192 {
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(count) => request.extend_from_slice(&chunk[..count]),
            }
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        if !request.is_empty() {
            let _ = stream.write_all(&ok_response(OPENAPI));
            stream.conn.send_close_notify();
            let _ = stream.flush();
        }
        request
    });
    (port, handle)
}

/// 平文の口: 1 回だけ受けて openapi を返す。
fn spawn_plain_server() -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        if let Ok((mut socket, _)) = listener.accept() {
            let mut buf = [0u8; 4096];
            let _ = socket.read(&mut buf);
            let _ = socket.write_all(&ok_response(OPENAPI));
        }
    });
    port
}

fn free_port() -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.local_addr().unwrap().port()
}

fn pin_of(identity: &crate::hub_tls_prepare::Identity) -> HubCertPemSource {
    let pem = identity.cert_pem.clone().into_bytes();
    Box::new(move || Ok(pem.clone()))
}

fn probe(root: &TempDir, tls_port: u16, cert: HubCertPemSource) -> PinnedHubHealthProbe {
    PinnedHubHealthProbe::with_timeout(
        root.path().to_path_buf(),
        "127.0.0.1".to_string(),
        tls_port,
        cert,
        Duration::from_millis(1000),
    )
}

#[test]
fn healthy_only_through_the_pinned_tls_port() {
    let hub = identity();
    let (tls_port, server) = spawn_tls_server(&hub);
    let root = TempDir::new("pinned-hub-health-healthy");
    // `profile.lock` の診断ファイルを直に置く（排他の取得はこの PC で main でも落ちることがある
    // ので使わない。確かめるのは「読めた」ことだけ）。
    let paths = resolve_profile_paths(root.path(), "default").unwrap();
    let lock_path = lock_file_path(&paths);
    std::fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    std::fs::write(
        &lock_path,
        r#"{"pid":7,"host_kind":"service","acquired_at_unix_ms":1}"#,
    )
    .unwrap();
    // 平文の口には誰もいない - Healthy は TLS の口だけで決まる。
    let outcome = probe(&root, tls_port, pin_of(&hub)).probe("default", free_port());
    assert_eq!(
        outcome,
        Ok(HealthOutcome::Healthy {
            version: "v1".to_string()
        })
    );
    let request = String::from_utf8(server.join().unwrap()).unwrap();
    assert!(request.starts_with("GET /api/v1/openapi.json"), "{request}");
}

#[test]
fn a_different_certificate_is_untrusted_and_gets_no_request() {
    let hub = identity();
    let squatter = identity();
    let (tls_port, server) = spawn_tls_server(&squatter);
    let root = TempDir::new("pinned-hub-health-untrusted");
    let outcome = probe(&root, tls_port, pin_of(&hub)).probe("default", spawn_plain_server());
    assert_eq!(outcome, Ok(HealthOutcome::TlsUntrusted));
    assert!(server.join().unwrap().is_empty(), "要求を送ってはいけない");
}

#[test]
fn a_plain_listener_on_the_tls_port_is_untrusted() {
    let hub = identity();
    // TLS の口に平文の HTTP の相手（握手が済まない）。
    let tls_port = spawn_plain_server();
    let root = TempDir::new("pinned-hub-health-plain-on-tls");
    let outcome = probe(&root, tls_port, pin_of(&hub)).probe("default", spawn_plain_server());
    assert_eq!(outcome, Ok(HealthOutcome::TlsUntrusted));
}

#[test]
fn an_untrusted_tls_port_counts_as_gone_once_the_plain_port_is_gone() {
    // Service→Desktop の「消えた」: 8723 が先取りされていても、平文の口が空けば Unreachable。
    let hub = identity();
    let squatter = identity();
    let (tls_port, _server) = spawn_tls_server(&squatter);
    let root = TempDir::new("pinned-hub-health-gone");
    let outcome = probe(&root, tls_port, pin_of(&hub)).probe("default", free_port());
    assert_eq!(outcome, Ok(HealthOutcome::Unreachable));
}

#[test]
fn an_unreadable_pin_never_connects_to_the_tls_port() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let tls_port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let root = TempDir::new("pinned-hub-health-no-pin");
    let outcome = probe(
        &root,
        tls_port,
        Box::new(|| Err("証明書がありません".to_string())),
    )
    .probe("default", spawn_plain_server());
    assert_eq!(
        outcome,
        Ok(HealthOutcome::TlsPinUnavailable {
            reason: "証明書がありません".to_string()
        })
    );
    assert!(listener.accept().is_err(), "TLS の口へ繋いではいけない");
}

#[test]
fn a_pem_that_is_not_a_certificate_is_unavailable() {
    let root = TempDir::new("pinned-hub-health-bad-pem");
    let outcome = probe(&root, free_port(), Box::new(|| Ok(b"not a pem".to_vec())))
        .probe("default", spawn_plain_server());
    assert!(
        matches!(outcome, Ok(HealthOutcome::TlsPinUnavailable { .. })),
        "{outcome:?}"
    );
}

#[test]
fn a_hub_without_tls_is_not_listening() {
    let hub = identity();
    let root = TempDir::new("pinned-hub-health-no-tls");
    let outcome = probe(&root, free_port(), pin_of(&hub)).probe("default", spawn_plain_server());
    assert_eq!(outcome, Ok(HealthOutcome::TlsNotListening));
}

#[test]
fn nothing_listening_anywhere_is_unreachable() {
    let hub = identity();
    let root = TempDir::new("pinned-hub-health-nothing");
    let read = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&read);
    let pem = hub.cert_pem.clone().into_bytes();
    let cert: HubCertPemSource = Box::new(move || {
        seen.store(true, Ordering::SeqCst);
        Ok(pem.clone())
    });
    let outcome = probe(&root, free_port(), cert).probe("default", free_port());
    assert_eq!(outcome, Ok(HealthOutcome::Unreachable));
    // pin は毎回読み直す（renew の後もシェルの再起動は要らない）。
    assert!(read.load(Ordering::SeqCst));
}
