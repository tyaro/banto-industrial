//! P0-3 (docs/banto-hub-driver-refactor-plan.md §8.3) on the new I/O for
//! SLMP: row 6 (and the SLMP half of row 3) of
//! docs/banto-hub-driver-io-design.md §11.1. The real `banto-driver-engine`
//! (`FrameSession`) over a real TCP socket to `banto-plc`'s SLMP simulator,
//! driving the frame-type `SlmpDriver` (R2b-3d-2).
//!
//! | here | P0-3 |
//! | --- | --- |
//! | `a_response_split_across_segments_is_read_as_one_response` | #6: 1, 5 and 13 bytes at a time, twice each, the same values |
//! | `a_split_write_and_read_modify_write_land_and_stay_aligned` | #6 for the write path: the 4E write answer and the three RMW steps split the same ways |
//! | `connect_to_a_closed_port_fails_without_hanging` | #3 (SLMP) |
//!
//! The rest of the old `SlmpClient` / `SlmpWriteClient` tests move here when
//! their I/O is removed (3d-join).

use std::time::{Duration, Instant};

use banto_driver_api::{
    ConnectContext, DataType, DriverBase, FatalError, Session, Settings, StringEncoding, TagKind,
    TagSpec, Value, WriteItem,
};
use banto_driver_engine::transport::TransportSpec;
use banto_driver_engine::FrameSession;
use banto_driver_slmp::SlmpDriver;
use banto_plc::slmp::address::SlmpDevice;
use banto_plc::slmp::simulator::Simulator;

fn n(data_type: DataType) -> TagKind {
    TagKind::numeric(data_type)
}

fn spec(text: &str, kind: TagKind) -> TagSpec {
    TagSpec::new(SlmpDriver.parse_address(text, &kind).unwrap(), kind)
}

fn settings(host: &str, port: u16, cpu: &str) -> Settings {
    Settings::from_pairs([
        ("host", host.to_string()),
        ("port", port.to_string()),
        ("cpu_type", cpu.to_string()),
    ])
    .unwrap()
}

async fn connect(sim: &Simulator, cpu: &str) -> Result<FrameSession, FatalError> {
    let host = sim.addr.ip().to_string();
    let spec = TransportSpec::Tcp {
        host: host.clone(),
        port: sim.addr.port(),
    };
    let cx = ConnectContext::new("p0-3", Duration::from_secs(2), Duration::from_secs(5));
    FrameSession::connect(
        &SlmpDriver,
        &settings(&host, sim.addr.port(), cpu),
        &spec,
        &cx,
        None,
    )
    .await
}

/// P0-3 #6: a response that reaches the client in several TCP segments - one
/// byte at a time, inside the 15-byte header (5) and right after the 13
/// bytes the length field does not count (13) - is read as one response with
/// the right values, twice (the second read proves the stream is still
/// aligned), on both series.
#[tokio::test]
async fn a_response_split_across_segments_is_read_as_one_response() {
    for cpu in ["R", "Q"] {
        let sim = Simulator::start().await;
        sim.set_word(SlmpDevice::D, 0, 42);
        sim.set_word(SlmpDevice::D, 1, 0xFFFF);
        // D2 as U32: the default word order is low_high, so the low word first.
        sim.set_words(SlmpDevice::D, 2, &[0x0002, 0x0001]);
        sim.set_word(SlmpDevice::D, 10, 0x0008);
        sim.set_string(SlmpDevice::D, 20, 2, "AB");
        sim.set_bit(SlmpDevice::M, 0, true);

        let mut session = connect(&sim, cpu).await.unwrap();
        let sjis = TagKind::string(2, StringEncoding::ShiftJis);
        let specs = [
            spec("D0", n(DataType::U16)),
            spec("D1", n(DataType::I16)),
            spec("D2", n(DataType::U32)),
            spec("D10.3", n(DataType::Bit)),
            spec("D20", sjis),
            spec("M0", n(DataType::Bit)),
            spec("M1", n(DataType::Bit)),
        ];
        for chunk in [1usize, 5, 13] {
            sim.set_response_chunking(Some((chunk, Duration::from_millis(2))));
            for _ in 0..2 {
                let values = session
                    .read_batch(&specs)
                    .await
                    .unwrap_or_else(|e| panic!("{cpu} chunk={chunk}: read_batch failed: {e:?}"));
                assert_eq!(
                    values,
                    vec![
                        Ok(Value::F64(42.0)),
                        Ok(Value::F64(-1.0)),
                        Ok(Value::F64(f64::from(0x0001_0002_u32))),
                        Ok(Value::Bit(true)),
                        Ok(Value::Str("AB".into())),
                        Ok(Value::Bit(true)),
                        Ok(Value::Bit(false)),
                    ],
                    "{cpu} chunk={chunk}"
                );
            }
        }
        session.disconnect().await;
        sim.stop();
    }
}

/// P0-3 #6 for the write path: an ordinary write's answer (no data) and the
/// three steps of a read-modify-write, split the same ways, land the
/// values, keep the other bits of the word, and leave the stream aligned for
/// the next read.
#[tokio::test]
async fn a_split_write_and_read_modify_write_land_and_stay_aligned() {
    let sim = Simulator::start().await;
    sim.set_word(SlmpDevice::D, 100, 0x8001);
    let mut session = connect(&sim, "R").await.unwrap();
    let bit = n(DataType::Bit);
    for (round, chunk) in [1usize, 5, 13].into_iter().enumerate() {
        sim.set_response_chunking(Some((chunk, Duration::from_millis(2))));
        let value = 1000.0 + round as f64;
        let on = round % 2 == 0;
        let items = [
            WriteItem::new(spec("D200", n(DataType::U16)), Value::F64(value)),
            WriteItem::new(spec("D100.4", bit.clone()), Value::Bit(on)),
        ];
        let results = session.write_batch(&items).await.unwrap();
        assert_eq!(results, vec![Ok(()), Ok(())], "chunk={chunk}");
        let expected = if on { 0x8011 } else { 0x8001 };
        assert_eq!(sim.get_word(SlmpDevice::D, 100), expected, "chunk={chunk}");
        let values = session
            .read_batch(&[
                spec("D200", n(DataType::U16)),
                spec("D100", n(DataType::U16)),
            ])
            .await
            .unwrap();
        assert_eq!(
            values,
            vec![Ok(Value::F64(value)), Ok(Value::F64(f64::from(expected)))],
            "chunk={chunk}"
        );
    }
    session.disconnect().await;
    sim.stop();
}

/// P0-3 #3 (SLMP): connecting to a port nobody listens on fails (refused or
/// timed out) within the connect timeout, without hanging. The closed port is
/// made by binding and dropping a plain listener.
#[tokio::test]
async fn connect_to_a_closed_port_fails_without_hanging() {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let spec = TransportSpec::Tcp {
        host: "127.0.0.1".into(),
        port,
    };
    let connect_timeout = Duration::from_secs(2);
    let cx = ConnectContext::new("p0-3", connect_timeout, Duration::from_secs(1));
    let started = Instant::now();
    let result = FrameSession::connect(
        &SlmpDriver,
        &settings("127.0.0.1", port, "R"),
        &spec,
        &cx,
        None,
    )
    .await;
    assert!(
        matches!(result, Err(FatalError::Io(_) | FatalError::Timeout)),
        "{:?}",
        result.err()
    );
    assert!(
        started.elapsed() < connect_timeout + Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
}
