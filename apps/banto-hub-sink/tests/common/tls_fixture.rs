//! Temporary, unprivileged certificates used only by Sink integration tests.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use banto_hub_core::runtime::HubTlsConfig;
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};

pub struct TlsFixture {
    cert: PathBuf,
    key: PathBuf,
    pub cert_pem: String,
}

impl TlsFixture {
    pub fn generate(dir: &Path) -> Self {
        let cert = dir.join("test-hub.pem");
        let key = dir.join("test-hub.key");
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
            .expect("OpenSSL is required for temporary Sink test certificates");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        Self {
            cert_pem: std::fs::read_to_string(&cert).unwrap(),
            cert,
            key,
        }
    }

    #[allow(dead_code)] // The raw TLS status fixture does not start a Hub router.
    pub fn hub_config(&self) -> HubTlsConfig {
        HubTlsConfig {
            port: 0,
            certificate_pem: self.cert.clone(),
            private_key_pem: self.key.clone(),
        }
    }

    #[allow(dead_code)] // The raw fake WS fixture in bridge.rs uses this variant.
    pub fn server_config(&self) -> Arc<tokio_rustls::rustls::ServerConfig> {
        let certs = CertificateDer::pem_file_iter(&self.cert)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let key = PrivateKeyDer::from_pem_file(&self.key).unwrap();
        Arc::new(
            tokio_rustls::rustls::ServerConfig::builder_with_provider(Arc::new(
                tokio_rustls::rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .unwrap(),
        )
    }

    pub fn pinned_config(&self, port: u16, api_key: &str) -> String {
        format!(
            "hub_url = \"https://127.0.0.1:{port}\"\napi_key = \"{api_key}\"\n\
             hub_tls_pinned_certs = ['''\n{}\n''']\n",
            self.cert_pem
        )
    }
}
