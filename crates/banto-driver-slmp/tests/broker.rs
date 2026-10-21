//! SLMP through the broker (R1-4a, docs/banto-driver-api-design.md §4.4):
//! these were `banto-broker`'s own unit tests while the SLMP driver lived
//! there. They moved here with the driver because the broker no longer has
//! (or depends on) a real driver; they now go through the public
//! `BrokerSupervisor::spawn` with a table holding only this crate's `dial`.
//! The assertions are unchanged.

use std::time::Duration;

use banto_broker::{
    BackoffConfig, BrokerError, BrokerHandle, BrokerSupervisor, DialFn, DriverTable,
};
use banto_driver_api::DriverId;
use banto_plc::{
    Address, BatchReadRequest, BatchReadResult, DataType, PlcValue, ReadRequest, StringReadRequest,
    TagValue,
};
use banto_plc_write::slmp::simulator::Simulator;
use banto_plc_write::{BatchWriteRequest, StringWriteRequest, WriteRequest, WriteResult};
use banto_tags::PlcConnection;

/// The broker table with just this driver, under `"slmp"` (what the old
/// tests' `DriverTable::builtin()` dialed an `"slmp"` connection with).
fn slmp_table() -> DriverTable {
    DriverTable::new(vec![(
        DriverId::new(banto_driver_slmp::PROTOCOL).expect("valid driver id"),
        banto_driver_slmp::dial as DialFn,
    )])
    .expect("one entry")
}

fn conn(id: i64, protocol: &str, host: &str, port: u16) -> PlcConnection {
    PlcConnection {
        id,
        name: format!("conn-{id}"),
        protocol: protocol.to_string(),
        host: host.to_string(),
        port: port as i64,
        unit_id: 1,
        enabled: true,
        simulation: false,

        word_order: "low_high".to_string(),
        cpu_type: "R".to_string(),
        database: None,
        username: None,
        password: None,
        settings: None,
    }
}

/// A numeric read request wrapped as the `Numeric` case of the mixed batch
/// the broker speaks (S2 文字列タグ).
fn rreq(raw: &str, data_type: DataType) -> BatchReadRequest {
    BatchReadRequest::Numeric(ReadRequest {
        address: Address::parse_slmp(raw).unwrap_or_else(|e| panic!("{raw}: {e}")),
        data_type,
    })
}

/// A string read request, the `String` case of the mixed batch. T20 ①b:
/// Shift-JIS explicitly (this test simulator seeds strings via
/// `Simulator::set_string`, which is Shift-JIS-only).
fn sreq(raw: &str, words: u16) -> BatchReadRequest {
    BatchReadRequest::String(StringReadRequest {
        address: Address::parse_slmp(raw).unwrap_or_else(|e| panic!("{raw}: {e}")),
        words,
        encoding: banto_plc::StringEncoding::ShiftJis,
    })
}

fn wreq(raw: &str, data_type: DataType, value: TagValue) -> BatchWriteRequest {
    BatchWriteRequest::Numeric(WriteRequest {
        address: Address::parse_slmp(raw).unwrap_or_else(|e| panic!("{raw}: {e}")),
        data_type,
        value,
    })
}

fn swreq(raw: &str, words: u16, value: &str) -> BatchWriteRequest {
    BatchWriteRequest::String(StringWriteRequest {
        address: Address::parse_slmp(raw).unwrap_or_else(|e| panic!("{raw}: {e}")),
        words,
        value: value.to_string(),
        encoding: banto_plc_write::StringEncoding::ShiftJis,
    })
}

/// T8 (docs/tag-server-design.md §6.1) bit-in-word write request.
fn bwreq(raw: &str, value: bool) -> BatchWriteRequest {
    BatchWriteRequest::BitInWord {
        address: Address::parse_slmp(raw).unwrap_or_else(|e| panic!("{raw}: {e}")),
        value,
    }
}

/// Poll `handle.read` until the session comes up (real time - connecting
/// to a loopback simulator settles in low single-digit milliseconds) or a
/// generous deadline elapses, so tests don't race the connect task that
/// starts in the background the instant a broker is spawned.
async fn read_once_connected(
    handle: &BrokerHandle,
    requests: Vec<BatchReadRequest>,
) -> Vec<BatchReadResult> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match handle.read(requests.clone()).await {
            Ok(results) => return results,
            Err(BrokerError::Disconnected { .. }) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            Err(e) => panic!("unexpected broker error while waiting to connect: {e}"),
        }
    }
}

/// #605: 接続の `cpu_type` が、broker の読み取りと書き込みの両方で
/// シミュレータに届くサブコマンドになる。Q のワードは `0x0000`、R は `0x0002`。
/// 既定のまま（このテストを R 固定に戻す）だと Q の断言が落ちる。
#[tokio::test]
async fn cpu_type_selects_the_slmp_subcommand_for_read_and_write() {
    for (cpu_type, word_sub) in [("R", 0x0002u16), ("Q", 0x0000u16)] {
        let sim = Simulator::start().await;
        let mut c = conn(1, "slmp", "127.0.0.1", sim.addr.port());
        c.cpu_type = cpu_type.to_string();
        let supervisor =
            BrokerSupervisor::spawn(&[c], BackoffConfig::default(), slmp_table()).expect("spawn");
        let handle = supervisor.handle(1).expect("handle");
        let _ = read_once_connected(&handle, vec![rreq("D0", DataType::U16)]).await;
        assert_eq!(
            sim.last_subcommand(),
            Some(word_sub),
            "{cpu_type} の読み取りサブコマンド"
        );
        handle
            .write(vec![wreq("D0", DataType::U16, TagValue::F64(1.0))])
            .await
            .expect("write");
        assert_eq!(
            sim.last_subcommand(),
            Some(word_sub),
            "{cpu_type} の書き込みサブコマンド"
        );
        supervisor.shutdown().await;
        sim.stop();
    }
}

#[tokio::test]
async fn supervisor_hands_out_a_handle_per_slmp_connection() {
    let sim = Simulator::start().await;
    let connections = [conn(7, "slmp", "127.0.0.1", sim.addr.port())];
    let supervisor = BrokerSupervisor::spawn(&connections, BackoffConfig::default(), slmp_table())
        .expect("all-slmp connections should spawn");
    assert_eq!(supervisor.connection_count(), 1);
    assert!(supervisor.handle(7).is_some());
    assert!(supervisor.handle(999).is_none());
    supervisor.shutdown().await;
}

// -----------------------------------------------------------------
// Read, write, and read-after-write over one shared session
// -----------------------------------------------------------------

#[tokio::test]
async fn read_returns_simulated_device_values() {
    let sim = Simulator::start().await;
    sim.set_word(banto_plc::SlmpDevice::D, 100, 4321);
    let connections = [conn(1, "slmp", "127.0.0.1", sim.addr.port())];
    let supervisor = BrokerSupervisor::spawn(&connections, BackoffConfig::default(), slmp_table())
        .expect("spawn");
    let handle = supervisor.handle(1).expect("handle");

    let results = read_once_connected(&handle, vec![rreq("D100", DataType::U16)]).await;
    assert_eq!(results, vec![BatchReadResult::Value(PlcValue::F64(4321.0))]);

    supervisor.shutdown().await;
}

#[tokio::test]
async fn write_then_read_reflects_the_write_over_the_single_session() {
    let sim = Simulator::start().await;
    let connections = [conn(1, "slmp", "127.0.0.1", sim.addr.port())];
    let supervisor = BrokerSupervisor::spawn(&connections, BackoffConfig::default(), slmp_table())
        .expect("spawn");
    let handle = supervisor.handle(1).expect("handle");

    // Wait for the session to come up using an innocuous read, then
    // write and read back - the read-after-write proof that both
    // directions share the one session (a separate session would still
    // pass this if it happened to hit the same simulator process, but a
    // wrong shared-client bug - e.g. two independent slmp::SLMPClients -
    // would still read the simulator's real state correctly too; what
    // this test actually pins down together with `only_one_broker_task_..`
    // below is that a single broker task and a single wire round trip
    // serve both).
    let _ = read_once_connected(&handle, vec![rreq("D200", DataType::U16)]).await;

    let write_results = handle
        .write(vec![wreq("D200", DataType::U16, TagValue::F64(777.0))])
        .await
        .expect("write should succeed");
    assert_eq!(write_results, vec![WriteResult::Ok]);

    let read_results = handle
        .read(vec![rreq("D200", DataType::U16)])
        .await
        .expect("read should succeed");
    assert_eq!(
        read_results,
        vec![BatchReadResult::Value(PlcValue::F64(777.0))]
    );

    supervisor.shutdown().await;
}

/// T8 E2E (docs/tag-server-design.md §6.1): a `BitInWord` write submitted
/// through `BrokerHandle::write` - exactly the same entry point
/// `write_then_read_reflects_the_write_over_the_single_session` above
/// uses for an ordinary write - runs its full read/modify/write/confirm
/// RMW sequence inside the one `Job::Write` the broker already knows how
/// to service, and the result is visible to a normal follow-up read. This
/// is the "broker needs no code changes" claim (§6.1, `banto-broker`'s own
/// module doc's "Why SLMP-only" section) demonstrated rather than merely
/// asserted: `banto-broker`'s source has not changed to make this pass -
/// only `banto-plc-write`'s planner/executor did.
#[tokio::test]
async fn bit_in_word_write_through_the_broker_lands_and_reads_back() {
    let sim = Simulator::start().await;
    sim.set_word(banto_plc::SlmpDevice::D, 300, 0x1234);
    let connections = [conn(1, "slmp", "127.0.0.1", sim.addr.port())];
    let supervisor = BrokerSupervisor::spawn(&connections, BackoffConfig::default(), slmp_table())
        .expect("spawn");
    let handle = supervisor.handle(1).expect("handle");

    // Wait for the session to come up, same as the ordinary-write test.
    let _ = read_once_connected(&handle, vec![rreq("D300", DataType::U16)]).await;

    // bit 0 is clear in the seed word (0x1234) - set it.
    let write_results = handle
        .write(vec![bwreq("D300.0", true)])
        .await
        .expect("bit-in-word write should succeed");
    assert_eq!(write_results, vec![WriteResult::Ok]);

    // The bit landed, and every other bit of the seed word survived -
    // read back both as a whole word and as the individual bit tag.
    let read_results = handle
        .read(vec![
            rreq("D300", DataType::U16),
            rreq("D300.0", DataType::Bit),
        ])
        .await
        .expect("read should succeed");
    assert_eq!(
        read_results,
        vec![
            BatchReadResult::Value(PlcValue::F64(0x1235 as f64)),
            BatchReadResult::Value(PlcValue::Bit(true)),
        ]
    );

    supervisor.shutdown().await;
}

#[tokio::test]
async fn string_write_then_read_round_trips_over_the_single_session() {
    // S2 文字列タグ: a mixed-batch string write lands in the simulator and
    // reads back as the same text (NUL-trimmed), proving the broker speaks
    // the string path end to end.
    let sim = Simulator::start().await;
    let connections = [conn(1, "slmp", "127.0.0.1", sim.addr.port())];
    let supervisor = BrokerSupervisor::spawn(&connections, BackoffConfig::default(), slmp_table())
        .expect("spawn");
    let handle = supervisor.handle(1).expect("handle");

    // Wait for the session, then write a 4-word (8-byte) string to D300.
    let _ = read_once_connected(&handle, vec![sreq("D300", 4)]).await;

    let write_results = handle
        .write(vec![swreq("D300", 4, "OK")])
        .await
        .expect("string write should succeed");
    assert_eq!(write_results, vec![WriteResult::Ok]);

    let read_results = handle
        .read(vec![sreq("D300", 4)])
        .await
        .expect("string read should succeed");
    assert_eq!(
        read_results,
        vec![BatchReadResult::Value(PlcValue::Str("OK".to_string()))]
    );

    supervisor.shutdown().await;
}
