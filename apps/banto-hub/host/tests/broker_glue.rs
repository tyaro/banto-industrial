//! `banto_hub_core::broker_glue` against a real SLMP session (R2b-0).
//!
//! This was a unit test in `apps/banto-hub/core/src/broker_glue.rs`'s
//! `#[cfg(test)] mod tests`. It checks the SLMP frame itself (the simulator's
//! last subcommand: R = `0x0002`, Q = `0x0000`), which needs the real
//! `banto-driver-slmp` - and `banto-hub-core` no longer depends on
//! `banto-drivers` (docs/banto-driver-api-design.md §2.4). Everything it
//! touches (`HubSessions`, `BrokerSimRegistry`) is public, so it moved here
//! unchanged apart from the paths. The other `broker_glue` unit tests stayed in
//! core on its fake drivers (`crate::test_support::test_drivers`).

mod common;

use banto_hub_core::broker_glue::{BrokerSimRegistry, HubSessions};
use banto_plc_write::slmp::simulator::Simulator;
use banto_tags::PlcConnection;

fn slmp_conn(id: i64, port: u16) -> PlcConnection {
    PlcConnection {
        id,
        name: format!("conn-{id}"),
        protocol: "slmp".to_string(),
        host: "127.0.0.1".to_string(),
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

fn modbus_conn(id: i64, port: u16) -> PlcConnection {
    PlcConnection {
        protocol: "modbus-tcp".to_string(),
        ..slmp_conn(id, port)
    }
}

/// #605: 同じ host/port で CPU だけ R→Q に変えると `resolve` が changed を
/// 返し、セッションを捨てて作り直すとフレームのサブコマンドが Q になる。
/// 捨てずに `ensure_connection` だけ呼ぶと id 再利用で R のまま残る。
/// Modbus は CPU 列をセッションの同一性に含めない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cpu_only_change_replaces_the_slmp_broker_session() {
    use banto_plc::{Address, BatchReadRequest, DataType, ReadRequest};

    let sim = Simulator::start().await;
    let sessions = HubSessions::new(
        banto_broker::BackoffConfig::default(),
        common::hub_drivers(),
    );
    let registry = BrokerSimRegistry::new();
    let mut conn = slmp_conn(1, sim.addr.port());

    let (_, _, changed) = registry.resolve(&conn).await;
    assert!(changed);
    sessions.ensure_connection(&conn).expect("R session");
    read_until_connected(&sessions, conn.id).await;
    assert_eq!(sim.last_subcommand(), Some(0x0002), "既定 R のワード読み");

    let (_, _, changed) = registry.resolve(&conn).await;
    assert!(!changed, "同じ CPU ではセッションを捨てない");

    conn.cpu_type = "Q".to_string();
    let (_, _, changed) = registry.resolve(&conn).await;
    assert!(changed, "CPU だけの変更でも changed");
    assert!(sessions.remove(conn.id));
    sessions.ensure_connection(&conn).expect("Q session");
    read_until_connected(&sessions, conn.id).await;
    assert_eq!(
        sim.last_subcommand(),
        Some(0x0000),
        "作り直したセッションは Q"
    );

    let mut modbus = modbus_conn(2, 502);
    let (_, _, changed) = registry.resolve(&modbus).await;
    assert!(changed);
    modbus.cpu_type = "Q".to_string();
    let (_, _, changed) = registry.resolve(&modbus).await;
    assert!(!changed, "Modbus は CPU 列では再接続しない");

    sessions.shutdown().await;
    sim.stop();

    async fn read_until_connected(sessions: &HubSessions, id: i64) {
        let handle = sessions.handle_for(id).expect("handle");
        let request = BatchReadRequest::Numeric(ReadRequest {
            address: Address::parse_slmp("D0").expect("addr"),
            data_type: DataType::U16,
        });
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match handle.read(vec![request]).await {
                Ok(_) => return,
                Err(_) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
                Err(err) => panic!("read failed: {err}"),
            }
        }
    }
}
