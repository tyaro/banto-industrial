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

fn conn_of(id: i64, protocol: &str, settings: serde_json::Value) -> PlcConnection {
    let serde_json::Value::Object(settings) = settings else {
        unreachable!("a JSON object literal")
    };
    PlcConnection::from_settings(
        id,
        format!("conn-{id}"),
        protocol.to_string(),
        true,
        false,
        settings,
        1,
    )
}

fn slmp_conn(id: i64, port: u16, cpu: &str) -> PlcConnection {
    conn_of(
        id,
        "slmp",
        serde_json::json!({
            "host": "127.0.0.1", "port": port, "word_order": "low_high", "cpu_type": cpu,
        }),
    )
}

/// #605 → R2b-3a-2: 同じ host/port で CPU だけ R→Q に変えると `resolve` が changed を
/// 返し（設定の指紋が変わる）、セッションを捨てて作り直すとフレームのサブコマンドが
/// Q になる。Modbus の設定は CPU を持たないので、平らな欄の CPU が変わっても
/// changed にならない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cpu_only_change_replaces_the_slmp_broker_session() {
    use banto_driver_api::{DataType, TagKind, TagSpec};

    let sim = Simulator::start().await;
    let sessions = HubSessions::new(
        banto_broker::BackoffConfig::default(),
        common::hub_drivers(),
    );
    let registry = BrokerSimRegistry::new();
    let drivers = sessions.drivers().registry.clone();
    let conn = slmp_conn(1, sim.addr.port(), "R");

    let (settings, changed) = registry.resolve(&conn, &drivers).await;
    let settings = settings.settings().clone();
    assert!(changed);
    sessions
        .ensure_connection_with_settings(&conn, settings)
        .expect("R session");
    read_until_connected(&sessions, conn.id).await;
    assert_eq!(sim.last_subcommand(), Some(0x0002), "既定 R のワード読み");

    let (_, changed) = registry.resolve(&conn, &drivers).await;
    assert!(!changed, "同じ CPU ではセッションを捨てない");

    let conn = slmp_conn(1, sim.addr.port(), "Q");
    let (settings, changed) = registry.resolve(&conn, &drivers).await;
    let settings = settings.settings().clone();
    assert!(changed, "CPU だけの変更でも changed");
    assert!(sessions.remove(conn.id));
    sessions
        .ensure_connection_with_settings(&conn, settings)
        .expect("Q session");
    read_until_connected(&sessions, conn.id).await;
    assert_eq!(
        sim.last_subcommand(),
        Some(0x0000),
        "作り直したセッションは Q"
    );

    let mut modbus = conn_of(
        2,
        "modbus-tcp",
        serde_json::json!({ "host": "127.0.0.1", "port": 502, "unit_id": 1 }),
    );
    let (_, changed) = registry.resolve(&modbus, &drivers).await;
    assert!(changed);
    modbus.cpu_type = "Q".to_string();
    let (_, changed) = registry.resolve(&modbus, &drivers).await;
    assert!(!changed, "Modbus は CPU 列では再接続しない");

    sessions.shutdown().await;
    sim.stop();

    async fn read_until_connected(sessions: &HubSessions, id: i64) {
        let handle = sessions.handle_for(id).expect("handle");
        let kind = TagKind::numeric(DataType::U16);
        let spec = TagSpec::new(
            common::hub_drivers()
                .registry
                .get("slmp")
                .expect("slmp driver")
                .base()
                .parse_address("D0", &kind)
                .expect("addr"),
            kind,
        );
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match handle.read(vec![spec.clone()]).await {
                Ok(_) => return,
                Err(_) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
                Err(err) => panic!("read failed: {err}"),
            }
        }
    }
}
