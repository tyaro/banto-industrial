//! The driver against an in-process `banto-opcua-server` with a None
//! endpoint (docs/chronogazer/opcua-client-design.md §3; #1324 stage 3):
//! cyclic reads of bit / integer / Double, a missing NodeId that is Bad on its
//! own, Uncertain and the SourceTimestamp, writes as the tag's type, a split
//! Write cut in the middle, the probe point, and a stopped server being
//! connection-fatal until a new connect.

mod common;

use std::net::{IpAddr, Ipv4Addr};
use std::num::NonZeroUsize;
use std::time::Duration;

use banto_driver_api::{
    DataType, DriverBase, FatalError, ItemError, ReadQuality, Session, SessionDriver, TagKind,
    TagSpec, Value, WriteItem,
};
use banto_driver_conformance::proxy::FaultProxy;
use banto_driver_opcua::OpcUaDriver;
use banto_opcua_server::model::{MappedValue, Quality};
use common::{cx, relay, sampled_at, settings, tag, TestServer};

const LOCALHOST: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

fn spec(text: &str, data_type: DataType) -> TagSpec {
    let kind = TagKind::numeric(data_type);
    TagSpec::new(OpcUaDriver.parse_address(text, &kind).unwrap(), kind)
}

async fn connect(endpoint: &str) -> Box<dyn Session> {
    OpcUaDriver
        .connect(&settings(endpoint), cx())
        .await
        .expect("connects")
}

fn sampled_ms() -> i64 {
    sampled_at()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reads_bits_integers_and_doubles_and_a_missing_node_alone_is_bad() {
    let server = TestServer::start(LOCALHOST, 0).await;
    let mut s = connect(&server.endpoint()).await;
    let specs = [
        spec(&tag("Flag"), DataType::Bit),
        spec(&tag("Count"), DataType::I16),
        spec(&tag("Big"), DataType::U32),
        spec(&tag("Ratio"), DataType::F64),
        spec(&tag("Ratio"), DataType::F32),
        spec(&tag("Wide"), DataType::I64),
        spec(&tag("Missing"), DataType::U16),
        spec("nsu=urn:not-there;i=1", DataType::U16),
        spec(&tag("Ratio"), DataType::I16),
        spec(&tag("Name"), DataType::F64),
        spec(&tag("Flag"), DataType::U16),
        spec(&tag("Count"), DataType::Bit),
        spec(&tag("Broken"), DataType::F64),
    ];
    let got = s.read_batch(&specs).await.expect("read");
    assert_eq!(got.len(), specs.len());
    assert_eq!(got[0], Ok(Value::Bit(true)));
    assert_eq!(got[1], Ok(Value::F64(-2.0)));
    assert_eq!(got[2], Ok(Value::F64(4e9)));
    assert_eq!(got[3], Ok(Value::F64(0.1)));
    assert_eq!(got[4], Ok(Value::F64(f64::from(0.1_f32))));
    assert_eq!(got[5], Ok(Value::F64(-5.0)));
    // BadNodeIdUnknown: only that tag (also an `nsu=` the server lacks).
    for i in [6, 7] {
        assert!(
            matches!(&got[i], Err(ItemError::Device(e)) if e.code == 0x8034_0000),
            "{i}: {:?}",
            got[i]
        );
    }
    // A fraction for an integer kind, a String, a Boolean as a number and a
    // number as a bit: that tag only, never coerced.
    for (i, r) in got.iter().enumerate().take(12).skip(8) {
        assert!(matches!(r, Err(ItemError::Unsupported(_))), "{i}: {r:?}");
    }
    // Quality::Bad on the server: no value, a Bad status.
    assert!(
        matches!(&got[12], Err(ItemError::Device(e)) if e.code & 0xC000_0000 == 0x8000_0000),
        "{:?}",
        got[12]
    );
    // The session is still usable, and a value change is seen.
    server.set_double("Ratio", 2.5, Quality::Good);
    let got = s.read_batch(&specs[3..4]).await.expect("read again");
    assert_eq!(got, vec![Ok(Value::F64(2.5))]);
    s.disconnect().await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uncertain_and_source_timestamp_reach_read_batch_detailed() {
    let server = TestServer::start(LOCALHOST, 0).await;
    let mut s = connect(&server.endpoint()).await;
    let specs = [
        spec(&tag("Count"), DataType::I16),
        spec(&tag("Stale"), DataType::F64),
        spec(&tag("Missing"), DataType::F64),
    ];
    let got = s.read_batch_detailed(&specs).await.expect("read");
    let good = got[0].as_ref().unwrap();
    assert_eq!(good.value, Value::F64(-2.0));
    assert_eq!(good.quality, ReadQuality::Good);
    assert_eq!(good.source_time_ms, Some(sampled_ms()));
    // Quality::Stale is published as UncertainLastUsableValue with its value.
    let stale = got[1].as_ref().unwrap();
    assert_eq!(stale.value, Value::F64(7.5));
    assert_eq!(stale.quality, ReadQuality::Uncertain);
    assert_eq!(stale.source_time_ms, Some(sampled_ms()));
    assert!(matches!(got[2], Err(ItemError::Device(_))));
    // read_batch gives the same values (it is built from the detailed read).
    let plain = s.read_batch(&specs).await.expect("read");
    for (detailed, plain) in got.iter().zip(&plain) {
        assert_eq!(
            detailed.as_ref().map(|r| r.value.clone()),
            plain.as_ref().cloned()
        );
    }
    s.disconnect().await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_reads_server_state_as_a_number() {
    let server = TestServer::start(LOCALHOST, 0).await;
    let mut s = connect(&server.endpoint()).await;
    let (text, kind) = OpcUaDriver.probe().unwrap();
    let probe = TagSpec::new(OpcUaDriver.parse_address(&text, &kind).unwrap(), kind);
    // ServerState is an Int32 enumeration; Running = 0.
    assert_eq!(
        s.read_batch(&[probe]).await.expect("read"),
        vec![Ok(Value::F64(0.0))]
    );
    s.disconnect().await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn writes_go_out_as_the_tags_type_and_refusals_stay_per_item() {
    let server = TestServer::start(LOCALHOST, 0).await;
    let mut s = connect(&server.endpoint()).await;
    let item = |name: &str, data_type, value| WriteItem::new(spec(&tag(name), data_type), value);
    // `Level` is Int16 on the server. Registered as i16 it is written; as
    // f64 the session still sends a Double (the tag's type, #678) and the
    // server answers BadTypeMismatch for that tag alone.
    let items = [
        item("Setpoint", DataType::F64, Value::F64(2.25)),
        item("Enable", DataType::Bit, Value::Bit(true)),
        item("Level", DataType::I16, Value::F64(321.0)),
        item("Locked", DataType::I16, Value::F64(1.0)),
        item("Flag", DataType::Bit, Value::Bit(false)),
        item("Missing", DataType::U16, Value::F64(1.0)),
        item("Level", DataType::F64, Value::F64(321.0)),
        item("Enable", DataType::U16, Value::F64(1.0)),
        item("Level", DataType::I16, Value::F64(1.5)),
    ];
    let got = s.write_batch(&items).await.expect("write");
    assert_eq!(got[0], Ok(()));
    assert_eq!(got[1], Ok(()));
    assert_eq!(got[2], Ok(()));
    // BadOutOfService (the handler's refusal), BadNotWritable,
    // BadNodeIdUnknown, and BadTypeMismatch for a tag whose type is not the
    // node's (a Double to Int16, a UInt16 to Boolean).
    for (i, code) in [
        (3, 0x808D_0000),
        (4, 0x803B_0000),
        (5, 0x8034_0000),
        (6, 0x8074_0000),
        (7, 0x8074_0000),
    ] {
        assert!(
            matches!(&got[i], Err(ItemError::Device(e)) if e.code == code),
            "{i}: {:?}",
            got[i]
        );
    }
    // 1.5 does not fit the tag's i16: refused before sending.
    assert!(
        matches!(got[8], Err(ItemError::Rejected(_))),
        "{:?}",
        got[8]
    );
    let written = server.writes.lock().unwrap().clone();
    assert_eq!(
        written,
        vec![
            ("Setpoint".to_owned(), MappedValue::Double(2.25)),
            ("Enable".to_owned(), MappedValue::Boolean(true)),
            ("Level".to_owned(), MappedValue::Int16(321)),
        ]
    );
    let back = s
        .read_batch(&[
            spec(&tag("Setpoint"), DataType::F64),
            spec(&tag("Enable"), DataType::Bit),
            spec(&tag("Level"), DataType::I16),
        ])
        .await
        .expect("read back");
    assert_eq!(
        back,
        vec![
            Ok(Value::F64(2.25)),
            Ok(Value::Bit(true)),
            Ok(Value::F64(321.0))
        ]
    );
    s.disconnect().await;
    server.stop().await;
}

/// A Write split one node per request (the server's MaxNodesPerWrite = 1)
/// whose connection is cut while the second request is in the server's
/// handler: the first node is written, the second is "maybe written"
/// (`Indeterminate`), the rest were never sent (`Rejected`) - each per node
/// in an `Ok` - and the session then refuses every call so the host
/// reconnects (#678).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_split_write_cut_in_the_middle_reports_each_nodes_outcome() {
    let (relay_addr, upstream) = relay().await;
    let proxy = FaultProxy::start(relay_addr).await.unwrap();
    let server = TestServer::start_with(
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2)),
        proxy.local_addr().port(),
        NonZeroUsize::new(1),
    )
    .await;
    *upstream.lock().unwrap() = Some(server.addr());
    let mut s = connect(&format!(
        "opc.tcp://127.0.0.1:{}/",
        proxy.local_addr().port()
    ))
    .await;
    let item = |name: &str, data_type, value| WriteItem::new(spec(&tag(name), data_type), value);
    let items = [
        item("Setpoint", DataType::F64, Value::F64(1.25)),
        item("Hold", DataType::I16, Value::F64(5.0)),
        item("Level", DataType::I16, Value::F64(7.0)),
        item("Enable", DataType::Bit, Value::Bit(true)),
    ];
    let hold = server.hold.clone();
    let (got, ()) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(s.write_batch(&items), async {
            hold.entered.notified().await;
            proxy.disconnect_all();
        })
    })
    .await
    .expect("bounded");
    let got = got.expect("a broken split Write is still Ok, per node");
    assert_eq!(got[0], Ok(()), "{got:?}");
    assert!(
        matches!(got[1], Err(ItemError::Indeterminate(_))),
        "{got:?}"
    );
    for (i, r) in got.iter().enumerate().skip(2) {
        assert!(matches!(r, Err(ItemError::Rejected(_))), "{i}: {got:?}");
    }
    // Only the first chunk reached the store (the held one never finished).
    assert_eq!(
        server.writes.lock().unwrap().clone(),
        vec![("Setpoint".to_owned(), MappedValue::Double(1.25))]
    );
    // The session is broken: nothing more is sent, every call is `Err`.
    let sent = proxy.request_count();
    let specs = [spec(&tag("Count"), DataType::I16)];
    assert!(s.read_batch(&specs).await.is_err());
    assert!(s.read_batch_detailed(&specs).await.is_err());
    assert!(s.write_batch(&items[..1]).await.is_err());
    assert_eq!(
        proxy.request_count(),
        sent,
        "a broken session sends nothing"
    );
    hold.release.notify_one();
    s.disconnect().await;
    drop(s);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stopped_server_is_fatal_and_a_new_connect_recovers() {
    let server = TestServer::start(LOCALHOST, 0).await;
    let port = server.addr().port();
    let endpoint = server.endpoint();
    let mut s = connect(&endpoint).await;
    let specs = [spec(&tag("Count"), DataType::I16)];
    assert_eq!(
        s.read_batch(&specs).await.unwrap(),
        vec![Ok(Value::F64(-2.0))]
    );
    server.stop().await;
    let r = tokio::time::timeout(Duration::from_secs(10), s.read_batch(&specs))
        .await
        .expect("bounded");
    assert!(
        matches!(r, Err(FatalError::Io(_) | FatalError::Timeout)),
        "{r:?}"
    );
    let w = tokio::time::timeout(
        Duration::from_secs(10),
        s.write_batch(&[WriteItem::new(
            spec(&tag("Level"), DataType::I16),
            Value::F64(1.0),
        )]),
    )
    .await
    .expect("bounded");
    // Never a success: normally "not sent" (the component knows the link is
    // down), and the session is broken from then on.
    assert!(
        matches!(&w, Ok(r) if matches!(
            r.as_slice(),
            [Err(ItemError::Rejected(_) | ItemError::Indeterminate(_))]
        )),
        "{w:?}"
    );
    assert!(s.read_batch(&specs).await.is_err());
    // Nothing listens: connecting fails (the host backs off and retries).
    assert!(OpcUaDriver
        .connect(&settings(&endpoint), cx())
        .await
        .is_err());
    drop(s);
    // The server comes back on the same port: a new connect reads again.
    let server = TestServer::start(LOCALHOST, port).await;
    let mut s = connect(&endpoint).await;
    assert_eq!(
        s.read_batch(&specs).await.unwrap(),
        vec![Ok(Value::F64(-2.0))]
    );
    s.disconnect().await;
    server.stop().await;
}

#[tokio::test]
async fn invalid_settings_are_fatal_invalid_config_without_io() {
    let s = banto_driver_api::Settings::from_pairs([
        ("endpointUrl", "opc.tcp://127.0.0.1:1/"),
        ("readMode", "subscribe"),
    ])
    .unwrap();
    let r = OpcUaDriver.connect(&s, cx()).await;
    assert!(matches!(r, Err(FatalError::InvalidConfig(_))));
}
