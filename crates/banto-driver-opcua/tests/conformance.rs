//! The conformance checks of `banto-driver-conformance` against the OPC UA
//! driver (docs/hub/driver/banto-driver-api-design.md §7; #1324 stage 3).
//!
//! - `static_checks` (C1, C2, C17-C22) on [`OpcUaDriver`].
//! - `session::run_all_against` (C1-C8, C12) against an in-process
//!   `banto-opcua-server` behind the harness's `FaultProxy`.
//!
//! ## Getting the session through the proxy
//!
//! The harness writes the proxy's `host` / `port` into the settings; a thin
//! wrapper ([`Proxied`]) turns them into `endpointUrl`. An OPC UA client
//! does not stay on the URL it is given, though: it reads the server's
//! endpoints (GetEndpoints, its own TCP connection) and opens the session on
//! the endpoint the server advertises, replacing only the host
//! (async-opcua's `find_matching_endpoint`). So the server must advertise the
//! proxy's port. The test binds the server to `127.0.0.2:<proxy port>` and
//! points the proxy at it through a small relay (the proxy's upstream is
//! fixed when it starts, before the port is known): the advertised
//! `opc.tcp://127.0.0.2:<P>/` becomes `opc.tcp://127.0.0.1:<P>/`, the proxy.
//!
//! ## Checks OPC UA cannot meet as written (not silently dropped)
//!
//! [`EXPECTED`] lists them with the reason; the test fails if any *other*
//! check fails, and also if an expected one starts passing (so the list
//! stays true).

mod common;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use banto_driver_api::{
    AddressError, BoxFuture, ConnectContext, DataType, DriverAddress, DriverBase, DriverDescriptor,
    FatalError, FieldError, KindName, Session, SessionDriver, Settings, SettingsSchema,
    StringEncoding, TagKind, Value,
};
use banto_driver_conformance::proxy::FaultProxy;
use banto_driver_conformance::session::{self, ResponsePatch, SessionFixtures};
use banto_driver_conformance::{static_checks, StaticFixtures};
use banto_driver_opcua::{keys, OpcUaDriver};
use common::{tag, TestServer};
use tokio::net::{TcpListener, TcpStream};

/// The driver with `endpointUrl` taken from the harness's `host` / `port`.
struct Proxied;

impl DriverBase for Proxied {
    fn descriptor(&self) -> &DriverDescriptor {
        OpcUaDriver.descriptor()
    }
    fn validate_config(&self, settings: &Settings) -> Vec<FieldError> {
        OpcUaDriver.validate_config(settings)
    }
    fn secret_keys(&self) -> Vec<String> {
        OpcUaDriver.secret_keys()
    }
    fn parse_address(&self, text: &str, kind: &TagKind) -> Result<DriverAddress, AddressError> {
        OpcUaDriver.parse_address(text, kind)
    }
    fn offset_address(
        &self,
        canonical: &str,
        kind: &TagKind,
        n: i64,
    ) -> Result<String, AddressError> {
        OpcUaDriver.offset_address(canonical, kind, n)
    }
    fn address_help(&self) -> String {
        OpcUaDriver.address_help()
    }
    fn probe(&self) -> Option<(String, TagKind)> {
        OpcUaDriver.probe()
    }
    fn settings_schema(&self) -> SettingsSchema {
        OpcUaDriver.settings_schema()
    }
    fn supported_kinds(&self) -> Vec<KindName> {
        OpcUaDriver.supported_kinds()
    }
    fn offset_words(&self, canonical: &str, n: i64) -> Result<String, AddressError> {
        OpcUaDriver.offset_words(canonical, n)
    }
}

impl SessionDriver for Proxied {
    fn connect(
        &self,
        settings: &Settings,
        cx: ConnectContext,
    ) -> BoxFuture<'static, Result<Box<dyn Session>, FatalError>> {
        let mut settings = settings.clone();
        let url = format!(
            "opc.tcp://{}:{}/",
            settings.get("host").expect("host"),
            settings.get("port").expect("port")
        );
        settings.insert(keys::ENDPOINT_URL, url);
        OpcUaDriver.connect(&settings, cx)
    }
}

/// A TCP relay whose upstream is set after it starts.
async fn relay() -> (SocketAddr, Arc<Mutex<Option<SocketAddr>>>) {
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

fn n(data_type: DataType) -> TagKind {
    TagKind::numeric(data_type)
}

fn statics() -> StaticFixtures {
    use DataType::*;
    let mut f = StaticFixtures::new(
        Settings::from_pairs([(keys::ENDPOINT_URL, "opc.tcp://127.0.0.1:4840/")]).unwrap(),
    );
    for (text, kind, canonical) in [
        ("i=2259", n(I32), "ns=0;i=2259"),
        ("ns=0;i=2259", n(U16), "ns=0;i=2259"),
        ("  ns=3;s=Line1.Speed ", n(F64), "ns=3;s=Line1.Speed"),
        ("ns=2;i=1001", n(Bit), "ns=2;i=1001"),
        (
            "nsu=urn:banto:opcua:tags;s=A.B.C",
            n(U64),
            "nsu=urn:banto:opcua:tags;s=A.B.C",
        ),
        ("nsu=urn:x%3by;i=7", n(I16), "nsu=urn:x%3By;i=7"),
        ("svr=0;ns=1;s=a;b", n(F32), "ns=1;s=a;b"),
    ] {
        f = f.valid(text, kind, canonical);
    }
    for (text, kind) in [
        ("", n(U16)),
        ("D100", n(U16)),
        ("40001", n(U16)),
        ("ns=1", n(U16)),
        ("ns=x;i=1", n(U16)),
        ("nsu=;i=1", n(U16)),
        ("i=abc", n(U16)),
        ("ns=1;g=09087e75-8e5e-499b-954f-f2a9603db28a", n(U16)),
        ("ns=1;b=AAEC", n(U16)),
        ("svr=2;i=1", n(U16)),
        ("ns=2;s=Name", TagKind::string(8, StringEncoding::Utf8)),
    ] {
        f = f.invalid(text, kind);
    }
    for (canonical, step, expected) in [
        ("ns=2;i=1001", 0, Some("ns=2;i=1001")),
        ("ns=2;i=1001", 1, None),
        ("ns=2;i=1001", -1, None),
        ("ns=2;s=Line1.Speed", 1, None),
        ("bogus", 0, None),
    ] {
        f = f.offset(canonical, n(U16), step, expected);
    }
    for kind in [Bit, I16, U16, I32, U32, F32, I64, U64, F64] {
        f = f.kind_example(kind, "ns=2;s=Any");
    }
    f = f.kind_example(KindName::String, "ns=2;s=Any");
    for (canonical, step) in [("ns=2;i=1001", 1), ("ns=2;i=1001", -1)] {
        f = f.word_offset(canonical, step, None);
    }
    f
}

#[test]
fn static_checks_pass() {
    assert_eq!(static_checks(&OpcUaDriver, &statics()), vec![]);
}

fn fixtures() -> SessionFixtures {
    use DataType::*;
    SessionFixtures::new(statics())
        .read(&tag("Count"), n(I16), Value::F64(-2.0))
        .read(&tag("Flag"), n(Bit), Value::Bit(true))
        .read(&tag("Big"), n(U32), Value::F64(4e9))
        // One node, two kinds, two values (f32 rounds to f32 precision).
        .read(&tag("Ratio"), n(F64), Value::F64(0.1))
        .read(&tag("Ratio"), n(F32), Value::F64(f64::from(0.1_f32)))
        .read(&tag("Wide"), n(I64), Value::F64(-5.0))
        .read("i=2259", n(I32), Value::F64(0.0))
        .refused(&tag("Missing"), n(U16))
        .unsupported(&tag("Ratio"), n(I16))
        .unsupported(&tag("Flag"), n(U16))
        .unsupported(&tag("Count"), n(Bit))
        .unsupported(&tag("Name"), n(F64))
        .write(&tag("Setpoint"), n(F64), Value::F64(2.25))
        .write(&tag("Enable"), n(Bit), Value::Bit(true))
        .write(&tag("Level"), n(I16), Value::F64(321.0))
        .unsupported_write(&tag("Enable"), n(U16), Value::F64(1.0))
        .unsupported_write(&tag("Setpoint"), n(Bit), Value::Bit(true))
        // The OPC UA binary message type of the next response ("MSG").
        .broken(ResponsePatch::new(0, 0, b"XYZ".to_vec()))
}

/// Checks OPC UA does not meet as the harness words them, with why. Each is
/// `(check, a phrase of its failure detail)`.
///
/// - **C4 (b), a broken response is `Framing` / `Protocol`**: async-opcua
///   closes the transport when a chunk does not decode, and the pending
///   request only sees `BadConnectionClosed` ("Transport is closing"); the
///   component passes on no decoding status. The read is still
///   connection-fatal (`Io`) and the next connect reads correctly (the rest
///   of C4 passes), only the class differs.
/// - **C5, responses in 1 / 2 / 3 / 5 byte pieces**: the harness reconnects
///   for each split so that the connect is split too, and an OPC UA connect
///   moves kilobytes (GetEndpoints and CreateSession carry the server's
///   certificate). Delivered a few bytes per millisecond-delay chunk it
///   outlasts the 1 s request timeout GetEndpoints runs under, so the
///   connect times out (`BadTimeout`). Reads on an established session are
///   small and are not what fails.
/// - **C8, one accepted connection per connect**: `banto-opcua-client`
///   reads the endpoints on a TCP connection of its own (GetEndpoints) and
///   then opens the session on a second one, so the peer accepts 2 per
///   connect. The 100 alternating reads and writes do share one session (the
///   harness counts 1 `connect`), which is the property #337 is about.
const EXPECTED: &[(&str, &str)] = &[
    ("C4", "壊れた応答が"),
    ("C5", "バイトずつ届く"),
    ("C8", "ピアが受けた接続が 2 本、connect が 1 回"),
];

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_conforms() {
    let (relay_addr, upstream) = relay().await;
    let proxy = FaultProxy::start(relay_addr).await.unwrap();
    let server = TestServer::start(
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2)),
        proxy.local_addr().port(),
    )
    .await;
    *upstream.lock().unwrap() = Some(server.addr());
    let failures = session::run_all_against(&Proxied, &fixtures(), &proxy).await;
    let text: Vec<String> = failures.iter().map(ToString::to_string).collect();
    let unexpected: Vec<&String> = text
        .iter()
        .filter(|t| {
            !EXPECTED
                .iter()
                .any(|(id, phrase)| t.starts_with(&format!("{id}:")) && t.contains(phrase))
        })
        .collect();
    let stale: Vec<&(&str, &str)> = EXPECTED
        .iter()
        .filter(|(id, phrase)| {
            !text
                .iter()
                .any(|t| t.starts_with(&format!("{id}:")) && t.contains(phrase))
        })
        .collect();
    assert!(
        unexpected.is_empty() && stale.is_empty(),
        "unexpected: {unexpected:#?}\nexpected but passing: {stale:#?}\nall: {text:#?}"
    );
    server.stop().await;
}
