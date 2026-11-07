//! Explicit optional-export compatibility check with two pinned components.
//! Both pins predate `clock-v1` (H): they still load through the operational
//! write loader, and the clock set is not offered (`Ok(None)`, nothing sent).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use banto_driver_api::{BoxFuture, CivilTime, FatalError, FrameDriver, Session as _, Settings};
use banto_driver_engine::{transport::Transport, FrameSession};
use banto_driver_wasm::WasmDriver;

const MANIFEST: &[u8] = br#"{"manifestVersion":1,"id":"slmp-wasm-operational-write","version":"0.1.0","hubApi":"1","writeApi":"1","settingsVersion":1,"displayName":"SLMP diagnostic compatibility fixture","transports":["tcp"],"capabilities":["read","write"]}"#;

fn settings() -> Settings {
    Settings::from_pairs([
        ("host", "127.0.0.1"),
        ("port", "3100"),
        ("d_u16_write_mode", "disabled"),
        ("d_u16_write_allowlist", ""),
    ])
    .unwrap()
}

fn driver(component: &[u8]) -> Arc<WasmDriver> {
    WasmDriver::load_operational_write(MANIFEST, component).unwrap()
}

fn connection(component: &[u8]) -> Box<dyn banto_driver_api::FrameConnection> {
    driver(component).create_connection(&settings()).unwrap()
}

/// An in-memory 4E iQ-R peer for the handshake only: 0101 answers R08CPU,
/// every word read answers zeros. Counts the requests it was sent.
struct HandshakePeer {
    sent: Arc<Mutex<usize>>,
    pending: Option<Vec<u8>>,
}

impl Transport for HandshakePeer {
    fn send<'a>(&'a mut self, bytes: &'a [u8]) -> BoxFuture<'a, Result<(), FatalError>> {
        Box::pin(async move {
            *self.sent.lock().unwrap() += 1;
            assert_eq!(&bytes[..2], &[0x54, 0x00], "4E request");
            let body = if bytes[15..17] == [0x01, 0x01] {
                let mut model = vec![b' '; 18];
                model[..6].copy_from_slice(b"R08CPU");
                model[16..].copy_from_slice(&0x4801u16.to_le_bytes());
                model
            } else {
                let n = bytes.len();
                let points = u16::from_le_bytes([bytes[n - 2], bytes[n - 1]]);
                vec![0; usize::from(points) * 2]
            };
            let mut response = vec![0xd4, 0x00, bytes[2], bytes[3], 0x00, 0x00];
            response.extend_from_slice(&bytes[6..11]);
            response.extend_from_slice(&u16::try_from(body.len() + 2).unwrap().to_le_bytes());
            response.extend_from_slice(&[0x00, 0x00]);
            response.extend_from_slice(&body);
            self.pending = Some(response);
            Ok(())
        })
    }
    fn recv<'a>(&'a mut self, buf: &'a mut Vec<u8>) -> BoxFuture<'a, Result<usize, FatalError>> {
        Box::pin(async move {
            let response = self.pending.take().expect("a request before each receive");
            buf.extend_from_slice(&response);
            Ok(response.len())
        })
    }
    fn discard_input(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
    fn close(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

/// A component without `clock-v1`, connected through the real engine: the
/// clock set is `Ok(None)` and sends nothing after the handshake.
async fn assert_no_clock(component: &[u8]) {
    assert!(!connection(component).supports_clock());
    let sent = Arc::new(Mutex::new(0));
    let driver = driver(component);
    let mut session = FrameSession::with_transport(
        driver.as_ref(),
        &settings(),
        Box::new(HandshakePeer {
            sent: sent.clone(),
            pending: None,
        }),
        Duration::from_secs(2),
        None,
    )
    .await
    .unwrap();
    let handshake = *sent.lock().unwrap();
    assert!(handshake > 0, "the handshake reached the peer");
    assert_eq!(
        session
            .set_clock(CivilTime::new(2026, 11, 7, 9, 5, 30))
            .await,
        Ok(None)
    );
    assert_eq!(*sent.lock().unwrap(), handshake, "nothing sent");
    assert!(session.is_connected());
    assert!(matches!(
        session
            .connection_mut()
            .unwrap()
            .set_clock_next(&CivilTime::new(2026, 11, 7, 9, 5, 30), None),
        Err(FatalError::Protocol(_))
    ));
}

#[tokio::test]
#[ignore = "set BANTO_SLMP_LEGACY_COMPONENT_WASM and BANTO_SLMP_DIAGNOSTICS_COMPONENT_WASM"]
async fn optional_export_preserves_the_legacy_world_and_binds_the_new_world() {
    let old = std::fs::read(std::env::var("BANTO_SLMP_LEGACY_COMPONENT_WASM").unwrap()).unwrap();
    let new =
        std::fs::read(std::env::var("BANTO_SLMP_DIAGNOSTICS_COMPONENT_WASM").unwrap()).unwrap();
    let mut legacy = connection(&old);
    assert!(!legacy.supports_diagnostics());
    assert!(legacy.diagnose_next(&[], None).is_err());
    let current = connection(&new);
    assert!(current.supports_diagnostics());
    // Neither pin exports clock-v1 (H): both still load and offer no clock set.
    assert_no_clock(&old).await;
    assert_no_clock(&new).await;
}
