//! Shared test support: an in-process `banto-opcua-server` with a None
//! endpoint, a handful of tags, a write handler that stores what was
//! written (the server itself never changes a node's value on a Write) and
//! holds a write to `Hold` until the test releases it, and a TCP relay.

#![allow(dead_code)]

use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use banto_driver_api::{ConnectContext, Settings};
use banto_driver_opcua::keys;
use banto_opcua_server::model::{DataType, MappedValue, Quality, SampleTimestamps, WriteRequest};
use banto_opcua_server::{
    OpcUaServer, RunningServer, ServerConfig, TagReading, WriteFuture, WriteHandler,
};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;

/// The server's tag namespace URI (its default).
pub const TAGS_URI: &str = "urn:banto:opcua:tags";
pub const CONNECTION: &str = "PLC2";
pub const GROUP: &str = "Cell1";

/// The SourceTimestamp the timed tags carry.
pub fn sampled_at() -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(1_700_000_000_123)
}

/// `nsu=` NodeId of `PLC2.Cell1.<tag>`.
pub fn tag(name: &str) -> String {
    format!("nsu={TAGS_URI};s={CONNECTION}.{GROUP}.{name}")
}

/// The tags every test server has: `(name, value, quality, writable)`.
pub fn seeded_tags() -> Vec<(&'static str, MappedValue, Quality, bool)> {
    vec![
        ("Flag", MappedValue::Boolean(true), Quality::Good, false),
        ("Count", MappedValue::Int16(-2), Quality::Good, false),
        (
            "Big",
            MappedValue::UInt32(4_000_000_000),
            Quality::Good,
            false,
        ),
        ("Ratio", MappedValue::Double(0.1), Quality::Good, false),
        ("Wide", MappedValue::Int64(-5), Quality::Good, false),
        (
            "Name",
            MappedValue::String("line-1".into()),
            Quality::Good,
            false,
        ),
        ("Stale", MappedValue::Double(7.5), Quality::Stale, false),
        ("Broken", MappedValue::Double(1.0), Quality::Bad, false),
        ("Setpoint", MappedValue::Double(0.0), Quality::Good, true),
        ("Enable", MappedValue::Boolean(false), Quality::Good, true),
        ("Level", MappedValue::Int16(0), Quality::Good, true),
        ("Locked", MappedValue::Int16(0), Quality::Good, true),
        ("Hold", MappedValue::Int16(0), Quality::Good, true),
    ]
}

/// Stores each accepted write into the node (`Locked` is refused), so a read
/// after a write sees the written value. A write to `Hold` signals
/// [`Hold::entered`] and waits for [`Hold::release`] first.
struct Store {
    server: Arc<OnceLock<Weak<RunningServer>>>,
    writes: Arc<Mutex<Vec<(String, MappedValue)>>>,
    hold: Arc<Hold>,
}

/// The handshake of a write to `Hold`.
#[derive(Default)]
pub struct Hold {
    /// Notified when a write to `Hold` reaches the handler.
    pub entered: Notify,
    /// Notify (`notify_one`) to let it finish.
    pub release: Notify,
}

impl WriteHandler for Store {
    fn write(&self, request: WriteRequest) -> WriteFuture {
        let server = self.server.clone();
        let writes = self.writes.clone();
        let hold = self.hold.clone();
        Box::pin(async move {
            if request.tag == "Locked" {
                return Err(banto_opcua_server::model::WriteRejection::OutOfService);
            }
            if request.tag == "Hold" {
                hold.entered.notify_one();
                hold.release.notified().await;
            }
            writes
                .lock()
                .unwrap()
                .push((request.tag.clone(), request.value.clone()));
            if let Some(server) = server.get().and_then(Weak::upgrade) {
                let now = SystemTime::now();
                server
                    .update_tag_reading(
                        &request.connection,
                        &request.group,
                        &request.tag,
                        TagReading::typed(
                            request.value.data_type(),
                            Some(request.value),
                            Quality::Good,
                            SampleTimestamps::new(Some(now), now),
                        ),
                    )
                    .expect("store the written value");
            }
            Ok(())
        })
    }
}

/// A running test server (the `Arc` is the only strong reference).
pub struct TestServer {
    pub server: Arc<RunningServer>,
    pub writes: Arc<Mutex<Vec<(String, MappedValue)>>>,
    pub hold: Arc<Hold>,
}

impl TestServer {
    /// Start on `bind:port` (port 0 = any) with a None endpoint only, the
    /// sample tags and [`seeded_tags`].
    pub async fn start(bind: IpAddr, port: u16) -> TestServer {
        Self::start_with(bind, port, None).await
    }

    /// [`start`](Self::start), advertising `max_nodes_per_write` when given
    /// (the client splits a larger Write into requests of that many nodes).
    pub async fn start_with(
        bind: IpAddr,
        port: u16,
        max_nodes_per_write: Option<NonZeroUsize>,
    ) -> TestServer {
        let slot = Arc::new(OnceLock::new());
        let writes = Arc::new(Mutex::new(Vec::new()));
        let hold = Arc::new(Hold::default());
        let mut config = ServerConfig::default()
            .with_bind(bind)
            .with_port(port)
            .with_none_endpoint(true)
            .with_security_policies(Vec::new());
        if let Some(max) = max_nodes_per_write {
            config = config.with_max_nodes_per_write(max);
        }
        let server = OpcUaServer::start_with_write_handler(
            config,
            Arc::new(Store {
                server: slot.clone(),
                writes: writes.clone(),
                hold: hold.clone(),
            }),
        )
        .await
        .expect("server starts");
        let server = Arc::new(server);
        assert!(slot.set(Arc::downgrade(&server)).is_ok(), "set once");
        let now = SystemTime::now();
        for (name, value, quality, writable) in seeded_tags() {
            let timestamps = SampleTimestamps::new(Some(sampled_at()), now);
            server
                .add_tag_reading(
                    CONNECTION,
                    GROUP,
                    name,
                    TagReading::typed(value.data_type(), Some(value), quality, timestamps),
                )
                .expect("add tag");
            if writable {
                server
                    .set_tag_writable(CONNECTION, GROUP, name, true)
                    .expect("writable");
            }
        }
        TestServer {
            server,
            writes,
            hold,
        }
    }

    pub fn addr(&self) -> SocketAddr {
        self.server.local_addr()
    }

    pub fn endpoint(&self) -> String {
        self.server.endpoint().to_owned()
    }

    /// Change a tag's value (a `Double` tag).
    pub fn set_double(&self, name: &str, value: f64, quality: Quality) {
        let now = SystemTime::now();
        self.server
            .update_tag_reading(
                CONNECTION,
                GROUP,
                name,
                TagReading::new(
                    DataType::Double,
                    Some(value),
                    quality,
                    SampleTimestamps::new(Some(now), now),
                ),
            )
            .expect("update");
    }

    pub async fn stop(self) {
        let Ok(server) = Arc::try_unwrap(self.server) else {
            panic!("no write in flight holds the server");
        };
        server.shutdown().await.expect("server stops");
    }
}

/// Settings for `endpoint`.
pub fn settings(endpoint: &str) -> Settings {
    Settings::from_pairs([(keys::ENDPOINT_URL, endpoint)]).unwrap()
}

pub fn cx() -> ConnectContext {
    ConnectContext::new("t", Duration::from_secs(5), Duration::from_secs(2))
}

/// A TCP relay whose upstream is set after it starts.
///
/// An OPC UA client opens its session on the endpoint the server advertises
/// (only the host replaced), so a test that puts a `FaultProxy` in front binds
/// the server to `127.0.0.2:<proxy port>` and points the proxy at this relay.
pub async fn relay() -> (SocketAddr, Arc<Mutex<Option<SocketAddr>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let upstream = Arc::new(Mutex::new(None::<SocketAddr>));
    let target = upstream.clone();
    tokio::spawn(async move {
        while let Ok((mut down, _)) = listener.accept().await {
            let Some(to) = *target.lock().unwrap() else {
                continue;
            };
            tokio::spawn(async move {
                if let Ok(mut up) = TcpStream::connect(to).await {
                    let _ = tokio::io::copy_bidirectional(&mut down, &mut up).await;
                }
            });
        }
    });
    (addr, upstream)
}
