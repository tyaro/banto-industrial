//! The driver against an in-process `banto-opcua-server` with a None
//! endpoint (docs/chronogazer/opcua-client-design.md §3; #1324 stage 3):
//! cyclic reads of bit / integer / Double, a missing NodeId that is Bad on its
//! own, Uncertain and the SourceTimestamp, writes, the probe point, and a
//! stopped server being connection-fatal until a new connect.

mod common;

use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use banto_driver_api::{
    DataType, DriverBase, FatalError, ItemError, ReadQuality, Session, SessionDriver, TagKind,
    TagSpec, Value, WriteItem,
};
use banto_driver_opcua::OpcUaDriver;
use banto_opcua_server::model::{MappedValue, Quality};
use common::{cx, sampled_at, settings, tag, TestServer};

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
async fn writes_go_out_as_the_nodes_type_and_refusals_stay_per_item() {
    let server = TestServer::start(LOCALHOST, 0).await;
    let mut s = connect(&server.endpoint()).await;
    let item = |name: &str, data_type, value| WriteItem::new(spec(&tag(name), data_type), value);
    // `Level` is Int16 on the server but registered as f64 here: the
    // session sends an Int16 (learned from a Read), not a Double.
    let items = [
        item("Setpoint", DataType::F64, Value::F64(2.25)),
        item("Enable", DataType::Bit, Value::Bit(true)),
        item("Level", DataType::F64, Value::F64(321.0)),
        item("Locked", DataType::I16, Value::F64(1.0)),
        item("Flag", DataType::Bit, Value::Bit(false)),
        item("Missing", DataType::U16, Value::F64(1.0)),
        item("Level", DataType::F64, Value::F64(1.5)),
        item("Enable", DataType::U16, Value::F64(1.0)),
    ];
    let got = s.write_batch(&items).await.expect("write");
    assert_eq!(got[0], Ok(()));
    assert_eq!(got[1], Ok(()));
    assert_eq!(got[2], Ok(()));
    // BadOutOfService (the handler's refusal), BadNotWritable, BadNodeIdUnknown.
    for (i, code) in [(3, 0x808D_0000), (4, 0x803B_0000), (5, 0x8034_0000)] {
        assert!(
            matches!(&got[i], Err(ItemError::Device(e)) if e.code == code),
            "{i}: {:?}",
            got[i]
        );
    }
    // 1.5 does not fit Int16: refused before sending. A number to a
    // Boolean node: a configuration mismatch.
    assert!(
        matches!(got[6], Err(ItemError::Rejected(_))),
        "{:?}",
        got[6]
    );
    assert!(
        matches!(got[7], Err(ItemError::Unsupported(_))),
        "{:?}",
        got[7]
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
    assert!(w.is_err(), "{w:?}");
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
