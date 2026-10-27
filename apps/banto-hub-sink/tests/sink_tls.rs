//! The Sink's separate REST client must use the configured Hub pin.

#[path = "common/tls_fixture.rs"]
mod tls_fixture;

use std::time::Duration;

use banto_hub_sink::config::ConfigError;
use banto_hub_sink::hub_api::{HubApi, SinkStatusPush};
use banto_hub_sink::SidecarConfig;
use tls_fixture::TlsFixture;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

#[test]
fn installer_template_requires_a_pin_even_with_a_dummy_api_key() {
    let template = include_str!("../../banto-hub/installer/assets/banto-hub-sink.toml.example");
    assert!(template.contains("hub_url = \"https://127.0.0.1:9443\""));
    assert!(template.contains("hub_tls_pinned_certs = []"));
    assert!(template.contains("api_key = \"\""));

    // Provisioning the API key alone must not make the shipped template runnable.
    let provisioned_key = template.replacen("api_key = \"\"", "api_key = \"bh_dummy.secret\"", 1);
    let error = SidecarConfig::from_toml_str(
        &provisioned_key,
        std::path::Path::new("banto-hub-sink.toml"),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ConfigError::Invalid {
            field: "hub_tls_pinned_certs",
            ..
        }
    ));
}

async fn status_listener(cert: &TlsFixture) -> (u16, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let acceptor = TlsAcceptor::from(cert.server_config());
    let task = tokio::spawn(async move {
        let Ok(Ok((stream, _))) =
            tokio::time::timeout(Duration::from_secs(5), listener.accept()).await
        else {
            return String::new();
        };
        let Ok(mut stream) = acceptor.accept(stream).await else {
            return String::new();
        };
        let mut request = Vec::new();
        let mut chunk = [0u8; 1024];
        while request.len() < 8192 {
            let Ok(Ok(count)) =
                tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk)).await
            else {
                break;
            };
            if count == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..count]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        if !request.is_empty() {
            stream
                .write_all(
                    b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
        }
        String::from_utf8(request).unwrap()
    });
    (port, task)
}

#[tokio::test]
async fn hub_api_status_uses_the_pin_before_sending_bearer() {
    let real_dir = tempfile::tempdir().unwrap();
    let other_dir = tempfile::tempdir().unwrap();
    let real = TlsFixture::generate(real_dir.path());
    let other = TlsFixture::generate(other_dir.path());

    let (port, server) = status_listener(&real).await;
    let config = SidecarConfig::from_toml_str(
        &real.pinned_config(port, "bh_dummy.secret"),
        real_dir.path(),
    )
    .unwrap();
    HubApi::new(&config)
        .unwrap()
        .push_status(&SinkStatusPush { groups: vec![] })
        .await
        .unwrap();
    let request = server.await.unwrap();
    assert!(request.contains("PUT /api/sink/status"));
    assert!(request.contains("Bearer bh_dummy.secret"));

    let (port, server) = status_listener(&real).await;
    let config = SidecarConfig::from_toml_str(
        &other.pinned_config(port, "bh_dummy.secret"),
        other_dir.path(),
    )
    .unwrap();
    assert!(HubApi::new(&config)
        .unwrap()
        .push_status(&SinkStatusPush { groups: vec![] })
        .await
        .is_err());
    assert!(server.await.unwrap().is_empty());
}
