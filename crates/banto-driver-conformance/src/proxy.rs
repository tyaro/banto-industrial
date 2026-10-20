//! [`FaultProxy`]: a TCP relay on 127.0.0.1 that puts faults between a
//! session-type driver and a peer that is already a TCP server (a device
//! simulator) - docs/banto-driver-api-design.md §7.2, §7.3.
//!
//! The driver connects to [`FaultProxy::local_addr`] instead of the peer.
//! Every accepted connection is relayed to the peer, and on the way back
//! the proxy can delay responses, deliver them in small pieces (C5), replace
//! the start of the k-th response (a broken frame for C4, a wrong check read
//! for C14), and drop every live connection (C7). It records what the
//! driver sent (C13 compares the written-back frames) and counts accepted
//! connections (C8: one socket per connection).
//!
//! A "response" is one read from the peer's socket. Simulators answer one
//! request with one write, so on loopback this is one response; a test that
//! needs exact boundaries should send one request at a time.
//!
//! R1-2 builds the relay and its faults; R1-5's `session::run_all_against`
//! drives Modbus / SLMP simulators through it.

use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{AbortHandle, JoinHandle};

/// What the proxy does to responses (peer → driver). Requests are relayed
/// untouched (and recorded).
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Faults {
    /// Deliver each response in pieces of at most this many bytes.
    pub split: Option<usize>,
    /// Wait this long between two pieces of a split response.
    pub chunk_delay: Duration,
    /// Wait this long before delivering each response.
    pub delay: Duration,
    /// `(k, bytes)`: overwrite the first bytes of the k-th response from now
    /// (0 = the next one) with `bytes`, once.
    pub replace: Option<(usize, Vec<u8>)>,
}

impl Faults {
    /// No faults: a transparent relay.
    pub fn new() -> Self {
        Self::default()
    }

    /// Split responses into pieces of `n` bytes (at least 1).
    pub fn split(self, n: usize) -> Self {
        Faults {
            split: Some(n.max(1)),
            ..self
        }
    }

    /// Wait `d` between pieces.
    pub fn chunk_delay(self, d: Duration) -> Self {
        Faults {
            chunk_delay: d,
            ..self
        }
    }

    /// Wait `d` before each response.
    pub fn delay(self, d: Duration) -> Self {
        Faults { delay: d, ..self }
    }

    /// Overwrite the start of the `k`-th response from now with `bytes`.
    pub fn replace_response(self, k: usize, bytes: Vec<u8>) -> Self {
        Faults {
            replace: Some((k, bytes)),
            ..self
        }
    }
}

#[derive(Default)]
struct Shared {
    faults: Mutex<Faults>,
    /// Responses seen since `faults` was last set (for `replace`).
    responses_since_set: AtomicUsize,
    accepts: AtomicUsize,
    requests: Mutex<Vec<Vec<u8>>>,
    live: Mutex<Vec<AbortHandle>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A fault-injecting TCP relay (see the module doc). Dropping it stops the
/// listener and every relayed connection.
pub struct FaultProxy {
    local: SocketAddr,
    shared: Arc<Shared>,
    acceptor: JoinHandle<()>,
}

impl FaultProxy {
    /// Listen on a free port of 127.0.0.1 and relay to `upstream`.
    /// Needs a tokio runtime (the relay runs on spawned tasks).
    pub async fn start(upstream: SocketAddr) -> io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let local = listener.local_addr()?;
        let shared = Arc::new(Shared::default());
        let acceptor = tokio::spawn(accept_loop(listener, upstream, Arc::clone(&shared)));
        Ok(FaultProxy {
            local,
            shared,
            acceptor,
        })
    }

    /// Where the driver should connect.
    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    /// How many connections the proxy has accepted.
    pub fn accept_count(&self) -> usize {
        self.shared.accepts.load(Ordering::SeqCst)
    }

    /// Replace the faults (for responses relayed from now on).
    pub fn set_faults(&self, faults: Faults) {
        *lock(&self.shared.faults) = faults;
        self.shared.responses_since_set.store(0, Ordering::SeqCst);
    }

    /// Everything the driver sent, one entry per read from its socket.
    pub fn requests(&self) -> Vec<Vec<u8>> {
        lock(&self.shared.requests).clone()
    }

    /// Close every relayed connection now (the driver sees the peer
    /// disconnect). New connections are still accepted.
    pub fn disconnect_all(&self) {
        for h in lock(&self.shared.live).drain(..) {
            h.abort();
        }
    }
}

impl Drop for FaultProxy {
    fn drop(&mut self) {
        self.acceptor.abort();
        self.disconnect_all();
    }
}

async fn accept_loop(listener: TcpListener, upstream: SocketAddr, shared: Arc<Shared>) {
    loop {
        let Ok((client, _)) = listener.accept().await else {
            continue;
        };
        shared.accepts.fetch_add(1, Ordering::SeqCst);
        let task = tokio::spawn(relay(client, upstream, Arc::clone(&shared)));
        let mut live = lock(&shared.live);
        live.retain(|h| !h.is_finished());
        live.push(task.abort_handle());
    }
}

/// One relayed connection. Ends (closing both sockets) when either side
/// closes or errors, or when aborted.
async fn relay(client: TcpStream, upstream: SocketAddr, shared: Arc<Shared>) {
    let Ok(peer) = TcpStream::connect(upstream).await else {
        return;
    };
    let _ = client.set_nodelay(true);
    let _ = peer.set_nodelay(true);
    let (mut client_rx, mut client_tx) = client.into_split();
    let (mut peer_rx, mut peer_tx) = peer.into_split();

    let requests = {
        let shared = Arc::clone(&shared);
        async move {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let n = match client_rx.read(&mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                lock(&shared.requests).push(buf[..n].to_vec());
                if peer_tx.write_all(&buf[..n]).await.is_err() {
                    return;
                }
            }
        }
    };
    let responses = async move {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = match peer_rx.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            let mut response = buf[..n].to_vec();
            let index = shared.responses_since_set.fetch_add(1, Ordering::SeqCst);
            let faults = {
                let mut f = lock(&shared.faults);
                if matches!(&f.replace, Some((k, _)) if *k == index) {
                    if let Some((_, bytes)) = f.replace.take() {
                        let m = bytes.len().min(response.len());
                        response[..m].copy_from_slice(&bytes[..m]);
                    }
                }
                f.clone()
            };
            if !faults.delay.is_zero() {
                tokio::time::sleep(faults.delay).await;
            }
            let piece = faults.split.unwrap_or(response.len()).max(1);
            for (i, chunk) in response.chunks(piece).enumerate() {
                if i > 0 && !faults.chunk_delay.is_zero() {
                    tokio::time::sleep(faults.chunk_delay).await;
                }
                if client_tx.write_all(chunk).await.is_err() {
                    return;
                }
                let _ = client_tx.flush().await;
            }
        }
    };
    // Either direction ending ends the connection.
    tokio::select! {
        _ = requests => {}
        _ = responses => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A peer that answers each read with `"<" + bytes + ">"`.
    async fn bracket_server() -> SocketAddr {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (mut s, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let mut buf = [0u8; 256];
                    loop {
                        let n = match s.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => n,
                        };
                        let mut out = vec![b'<'];
                        out.extend_from_slice(&buf[..n]);
                        out.push(b'>');
                        if s.write_all(&out).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        addr
    }

    async fn round_trip(s: &mut TcpStream, req: &[u8], want: usize) -> (Vec<u8>, usize) {
        s.write_all(req).await.unwrap();
        let mut got = Vec::new();
        let mut reads = 0;
        let mut buf = [0u8; 256];
        while got.len() < want {
            let n = tokio::time::timeout(Duration::from_secs(5), s.read(&mut buf))
                .await
                .expect("response in time")
                .unwrap();
            assert!(n > 0, "closed early");
            got.extend_from_slice(&buf[..n]);
            reads += 1;
        }
        (got, reads)
    }

    #[tokio::test]
    async fn relays_records_splits_replaces_and_counts() {
        let proxy = FaultProxy::start(bracket_server().await).await.unwrap();
        let mut c = TcpStream::connect(proxy.local_addr()).await.unwrap();
        c.set_nodelay(true).unwrap();

        let (got, _) = round_trip(&mut c, b"abc", 5).await;
        assert_eq!(got, b"<abc>");
        assert_eq!(proxy.requests(), vec![b"abc".to_vec()]);

        proxy.set_faults(
            Faults::new()
                .split(1)
                .chunk_delay(Duration::from_millis(5))
                .replace_response(1, b"[".to_vec()),
        );
        let (got, reads) = round_trip(&mut c, b"xy", 4).await;
        assert_eq!(got, b"<xy>");
        assert!(reads >= 2, "a split response arrived in {reads} read(s)");
        let (got, _) = round_trip(&mut c, b"z", 3).await;
        assert_eq!(
            got, b"[z>",
            "the second response from now has its start replaced"
        );
        let (got, _) = round_trip(&mut c, b"q", 3).await;
        assert_eq!(got, b"<q>", "replace happens once");

        assert_eq!(proxy.accept_count(), 1);
        let mut c2 = TcpStream::connect(proxy.local_addr()).await.unwrap();
        let _ = round_trip(&mut c2, b"2", 3).await;
        assert_eq!(proxy.accept_count(), 2);
    }

    #[tokio::test]
    async fn disconnect_all_closes_live_connections_and_accepts_new_ones() {
        let proxy = FaultProxy::start(bracket_server().await).await.unwrap();
        let mut c = TcpStream::connect(proxy.local_addr()).await.unwrap();
        let _ = round_trip(&mut c, b"a", 3).await;
        proxy.disconnect_all();
        let mut buf = [0u8; 8];
        let n = tokio::time::timeout(Duration::from_secs(5), c.read(&mut buf))
            .await
            .expect("closed in time")
            .unwrap_or(0);
        assert_eq!(n, 0, "the relayed connection was closed");

        let mut c = TcpStream::connect(proxy.local_addr()).await.unwrap();
        let (got, _) = round_trip(&mut c, b"b", 3).await;
        assert_eq!(got, b"<b>");
        assert_eq!(proxy.accept_count(), 2);
    }

    #[tokio::test]
    async fn delay_holds_the_response_back() {
        let proxy = FaultProxy::start(bracket_server().await).await.unwrap();
        proxy.set_faults(Faults::new().delay(Duration::from_millis(200)));
        let mut c = TcpStream::connect(proxy.local_addr()).await.unwrap();
        c.write_all(b"d").await.unwrap();
        let mut buf = [0u8; 8];
        let early = tokio::time::timeout(Duration::from_millis(50), c.read(&mut buf)).await;
        assert!(early.is_err(), "nothing arrives before the delay");
        let (got, _) = round_trip(&mut c, b"", 3).await;
        assert_eq!(got, b"<d>");
    }
}
