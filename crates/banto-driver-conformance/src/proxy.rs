//! [`FaultProxy`]: a TCP relay on 127.0.0.1 that puts faults between a
//! session-type driver and a peer that is already a TCP server (a device
//! simulator) - docs/banto-driver-api-design.md §7.2, §7.3.
//!
//! The driver connects to [`FaultProxy::local_addr`] instead of the peer.
//! Every accepted connection is relayed to the peer, and on the way back
//! the proxy can delay responses, deliver them in small pieces (C5), replace
//! the start of the k-th response or patch bytes anywhere in it (a broken
//! frame for C4, a wrong check read for C14, a device refusal for C15), hold
//! the k-th response back past the driver's timeout (C6), cut the k-th
//! response off halfway and close (C4), and drop every live connection (C7).
//! It records what the driver sent (C13 compares the written-back frames),
//! counts accepted connections (C8: one socket per connection) and counts
//! the responses it delivered (so a test can wait for a late one).
//!
//! A "response" is one read from the peer's socket. Simulators answer one
//! request with one write, so on loopback this is one response; a test that
//! needs exact boundaries should send one request at a time.
//!
//! R1-2 built the relay and its faults; R1-5 added the one-shot
//! [`patch_response`](Faults::patch_response),
//! [`hold_response`](Faults::hold_response) and
//! [`cut_response`](Faults::cut_response), [`FaultProxy::delivered_count`]
//! and [`FaultProxy::close_all`] for
//! [`session::run_all_against`](crate::session::run_all_against), which
//! drives the Modbus / SLMP simulators through it.

use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

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
    /// `(k, offset, bytes)`: overwrite the bytes of the k-th response from now
    /// starting at `offset` with `bytes` (what does not fit is dropped), once.
    /// Unlike [`replace`](Self::replace) it can leave a header that echoes
    /// the request (a transaction or serial id) intact.
    pub patch: Option<(usize, usize, Vec<u8>)>,
    /// `(k, d)`: hold only the k-th response from now back for `d` (on top
    /// of [`delay`](Self::delay)), once - a response that arrives after the
    /// driver's timeout.
    pub hold: Option<(usize, Duration)>,
    /// `(k, n)`: deliver only the first `n` bytes of the k-th response from
    /// now and then close the connection, once - the peer going away in the
    /// middle of a response.
    pub cut: Option<(usize, usize)>,
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

    /// Overwrite the `k`-th response from now at `offset` with `bytes`.
    pub fn patch_response(self, k: usize, offset: usize, bytes: Vec<u8>) -> Self {
        Faults {
            patch: Some((k, offset, bytes)),
            ..self
        }
    }

    /// Hold only the `k`-th response from now back for `d`.
    pub fn hold_response(self, k: usize, d: Duration) -> Self {
        Faults {
            hold: Some((k, d)),
            ..self
        }
    }

    /// Deliver the first `n` bytes of the `k`-th response from now, then
    /// close the connection.
    pub fn cut_response(self, k: usize, n: usize) -> Self {
        Faults {
            cut: Some((k, n)),
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
    /// Responses written to the driver in full.
    delivered: AtomicUsize,
    requests: Mutex<Vec<Vec<u8>>>,
    live: Mutex<Vec<JoinHandle<()>>>,
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

    /// How many responses the proxy has delivered to the driver in full
    /// (a cut or undeliverable response does not count).
    pub fn delivered_count(&self) -> usize {
        self.shared.delivered.load(Ordering::SeqCst)
    }

    /// Close every relayed connection now (the driver sees the peer
    /// disconnect). New connections are still accepted.
    ///
    /// The relays stop asynchronously; [`close_all`](Self::close_all) also
    /// waits until they have.
    pub fn disconnect_all(&self) {
        for h in lock(&self.shared.live).drain(..) {
            h.abort();
        }
    }

    /// [`disconnect_all`](Self::disconnect_all), and return once every
    /// relayed socket is closed - so the driver's next request cannot still
    /// reach the peer through a relay that has not stopped yet.
    pub async fn close_all(&self) {
        let live: Vec<JoinHandle<()>> = lock(&self.shared.live).drain(..).collect();
        for h in &live {
            h.abort();
        }
        for h in live {
            // `Err` is the cancellation we asked for.
            let _ = h.await;
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
        live.push(task);
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
            let mut hold = Duration::ZERO;
            let mut cut = None;
            let faults = {
                let mut f = lock(&shared.faults);
                if matches!(&f.replace, Some((k, _)) if *k == index) {
                    if let Some((_, bytes)) = f.replace.take() {
                        let m = bytes.len().min(response.len());
                        response[..m].copy_from_slice(&bytes[..m]);
                    }
                }
                if matches!(&f.patch, Some((k, _, _)) if *k == index) {
                    if let Some((_, offset, bytes)) = f.patch.take() {
                        for (i, b) in bytes.into_iter().enumerate() {
                            if let Some(slot) = response.get_mut(offset + i) {
                                *slot = b;
                            }
                        }
                    }
                }
                if matches!(&f.hold, Some((k, _)) if *k == index) {
                    if let Some((_, d)) = f.hold.take() {
                        hold = d;
                    }
                }
                if matches!(&f.cut, Some((k, _)) if *k == index) {
                    cut = f.cut.take().map(|(_, n)| n);
                }
                f.clone()
            };
            let wait = faults.delay + hold;
            if !wait.is_zero() {
                tokio::time::sleep(wait).await;
            }
            if let Some(n) = cut {
                response.truncate(n);
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
            if cut.is_some() {
                // Close both sockets halfway through the response.
                return;
            }
            shared.delivered.fetch_add(1, Ordering::SeqCst);
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
    async fn patch_hold_and_cut_apply_once_to_the_kth_response() {
        let proxy = FaultProxy::start(bracket_server().await).await.unwrap();
        let mut c = TcpStream::connect(proxy.local_addr()).await.unwrap();
        c.set_nodelay(true).unwrap();

        // Patch: bytes 2.. of the response after next, once; what does not
        // fit is dropped.
        proxy.set_faults(Faults::new().patch_response(1, 2, b"XYZ".to_vec()));
        assert_eq!(round_trip(&mut c, b"ab", 4).await.0, b"<ab>");
        assert_eq!(round_trip(&mut c, b"ab", 4).await.0, b"<aXY");
        assert_eq!(round_trip(&mut c, b"ab", 4).await.0, b"<ab>");
        assert_eq!(proxy.delivered_count(), 3);

        // Hold: only the next response waits.
        proxy.set_faults(Faults::new().hold_response(0, Duration::from_millis(200)));
        c.write_all(b"h").await.unwrap();
        let mut buf = [0u8; 8];
        let early = tokio::time::timeout(Duration::from_millis(50), c.read(&mut buf)).await;
        assert!(early.is_err(), "the held response is not there yet");
        assert_eq!(round_trip(&mut c, b"", 3).await.0, b"<h>");
        assert_eq!(round_trip(&mut c, b"i", 3).await.0, b"<i>");
        assert_eq!(proxy.delivered_count(), 5);

        // Cut: two bytes of the next response, then the connection closes.
        proxy.set_faults(Faults::new().cut_response(0, 2));
        c.write_all(b"cut").await.unwrap();
        let mut got = Vec::new();
        loop {
            let n = tokio::time::timeout(Duration::from_secs(5), c.read(&mut buf))
                .await
                .expect("closed in time")
                .unwrap_or(0);
            if n == 0 {
                break;
            }
            got.extend_from_slice(&buf[..n]);
        }
        assert_eq!(got, b"<c");
        assert_eq!(
            proxy.delivered_count(),
            5,
            "a cut response is not delivered"
        );
    }

    #[tokio::test]
    async fn close_all_returns_after_the_relays_are_gone() {
        let proxy = FaultProxy::start(bracket_server().await).await.unwrap();
        let mut c = TcpStream::connect(proxy.local_addr()).await.unwrap();
        let _ = round_trip(&mut c, b"a", 3).await;
        proxy.close_all().await;
        // The relay is gone, so this request cannot be answered.
        let _ = c.write_all(b"b").await;
        let mut buf = [0u8; 8];
        let n = tokio::time::timeout(Duration::from_secs(5), c.read(&mut buf))
            .await
            .expect("closed in time")
            .unwrap_or(0);
        assert_eq!(n, 0, "nothing relayed after close_all");
        assert_eq!(proxy.accept_count(), 1);
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
