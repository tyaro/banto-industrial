//! End-to-end tests for the タグモニタ surface (feature/tag-monitor):
//! `EngineControl::monitor_group_read` / `monitor_tag_write` driving the FULL
//! stack - broker (one shared SLMP session per CPU) → in-process SLMP
//! simulator - against a real (in-memory) database, asserting on BOTH the
//! simulator's device state AND the `write_audit_log` rows (`manual_write`).
//!
//! Same anti-hang discipline as `engine_integration.rs`: every wait for an
//! asynchronous outcome (the broker's background connect, mainly) is bounded
//! by a deadline, so a bug fails fast instead of hanging.

use std::time::Duration;

use banto_core::BantoError;
use banto_plc::SlmpDevice;
use banto_plc_write::slmp::simulator::Simulator;
use banto_tags::{
    CollectionGroupInput, CollectionGroupService, PlcConnectionInput, PlcConnectionService,
    TagInput, TagService,
};
use relay_wright_core::db::init_db_memory;
use relay_wright_core::engine::MonitorValue;
use relay_wright_core::{Engine, EngineConfig, EngineControl};
use sqlx::SqlitePool;

/// An in-memory DB, a running simulator, and one SLMP PLC connection +
/// collection group pointed at it (the monitor's unit of selection).
struct Fixture {
    pool: SqlitePool,
    sim: Simulator,
    conn_id: i64,
    group_id: i64,
    tags: TagService,
}

impl Fixture {
    async fn new() -> Self {
        let pool = init_db_memory().await.expect("init_db_memory");
        let sim = Simulator::start().await;

        let plc = PlcConnectionService::new(pool.clone());
        let conn = plc
            .create(PlcConnectionInput {
                name: "CPU1".to_string(),
                protocol: "slmp".to_string(),
                host: sim.addr.ip().to_string(),
                port: sim.addr.port() as i64,
                unit_id: 1,
                enabled: true,
            })
            .await
            .expect("create slmp connection");

        let groups = CollectionGroupService::new(pool.clone());
        let group = groups
            .create(CollectionGroupInput {
                name: "G1".to_string(),
                plc_connection_id: conn.id,
                period_ms: 1000,
                enabled: true,
            })
            .await
            .expect("create collection group");

        Self {
            tags: TagService::new(pool.clone()),
            pool,
            sim,
            conn_id: conn.id,
            group_id: group.id,
        }
    }

    /// A tag in the fixture group. `scaling` is `(raw_lo, raw_hi, eng_lo,
    /// eng_hi)`; `string_length` only for `data_type == "string"`.
    #[allow(clippy::too_many_arguments)]
    async fn tag(
        &self,
        name: &str,
        address: &str,
        data_type: &str,
        string_length: Option<i64>,
        scaling: Option<(f64, f64, f64, f64)>,
        unit: Option<&str>,
        decimals: i64,
    ) -> i64 {
        self.tags
            .create(TagInput {
                name: name.to_string(),
                collection_group_id: self.group_id,
                address: address.to_string(),
                data_type: data_type.to_string(),
                string_length,
                raw_lo: scaling.map(|s| s.0),
                raw_hi: scaling.map(|s| s.1),
                eng_lo: scaling.map(|s| s.2),
                eng_hi: scaling.map(|s| s.3),
                unit: unit.map(str::to_string),
                decimals,
                threshold_h: None,
                threshold_hh: None,
                threshold_l: None,
                threshold_ll: None,
                enabled: true,
            })
            .await
            .unwrap()
            .id
    }

    async fn all_connections(&self) -> Vec<banto_tags::PlcConnection> {
        PlcConnectionService::new(self.pool.clone())
            .list(Default::default())
            .await
            .unwrap()
            .rows
    }
}

/// Poll `monitor_group_read` until every value is `good` (the broker connects
/// in the background - single-digit ms against a loopback simulator) or the
/// deadline elapses; panics with the last snapshot on timeout.
async fn read_group_until_good(control: &EngineControl, group_id: i64) -> Vec<MonitorValue> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut last: Vec<MonitorValue> = Vec::new();
    loop {
        last = control
            .monitor_group_read(group_id)
            .await
            .expect("monitor_group_read");
        if !last.is_empty() && last.iter().all(|v| v.quality == "good") {
            return last;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("values never became good: {last:?}");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Retry `monitor_tag_write` while the session is still connecting (the
/// broker fails fast with its Disconnected error rather than queuing).
async fn write_until_ok(control: &EngineControl, tag_id: i64, value: &str, actor: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match control.monitor_tag_write(tag_id, value, Some(actor)).await {
            Ok(()) => return,
            Err(e) if tokio::time::Instant::now() < deadline => {
                let message = e.to_string();
                assert!(
                    message.contains("未接続"),
                    "only the connecting-window error is retried, got: {message}"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(e) => panic!("write never succeeded: {e}"),
        }
    }
}

async fn audit_rows(
    pool: &SqlitePool,
) -> Vec<(String, String, Option<String>, Option<i64>, Option<f64>, Option<String>)> {
    sqlx::query_as(
        "SELECT action, result, actor_username, source_tag_id, target_value_written, detail \
         FROM write_audit_log ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

// ---------------------------------------------------------------------------
// Reads: scaling + decimals applied, string and bit shapes, per-tag quality.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn monitor_group_read_returns_display_ready_values_over_the_engine_session() {
    let f = Fixture::new().await;
    // 温度: raw 0..=4000 → eng 0..=100 ℃, 1 decimal. raw 2000 → 50.0.
    let temp = f
        .tag(
            "温度",
            "D100",
            "u16",
            None,
            Some((0.0, 4000.0, 0.0, 100.0)),
            Some("℃"),
            1,
        )
        .await;
    // 生値: unscaled u16.
    let plain = f.tag("生値", "D110", "u16", None, None, None, 0).await;
    // ビット, 文字列 (4 words = 8 SJIS bytes).
    let bit = f.tag("運転", "M10", "bit", None, None, None, 0).await;
    let text = f.tag("状態", "D300", "string", Some(4), None, None, 0).await;

    f.sim.set_word(SlmpDevice::D, 100, 2000);
    f.sim.set_word(SlmpDevice::D, 110, 1234);
    f.sim.set_bit(SlmpDevice::M, 10, true);
    // "OK" in SJIS, low byte first within the word: 0x4B4F.
    f.sim.set_word(SlmpDevice::D, 300, u16::from_le_bytes([b'O', b'K']));

    let (engine, control) = Engine::start(
        f.pool.clone(),
        f.all_connections().await,
        EngineConfig::default(),
    )
    .await
    .expect("engine start");

    let values = read_group_until_good(&control, f.group_id).await;
    assert_eq!(values.len(), 4);

    let by_id = |id: i64| values.iter().find(|v| v.tag_id == id).unwrap();
    let temp_value = by_id(temp);
    assert_eq!(temp_value.value, Some(serde_json::json!(50.0)));
    assert_eq!(temp_value.unit.as_deref(), Some("℃"));
    assert_eq!(by_id(plain).value, Some(serde_json::json!(1234.0)));
    assert_eq!(by_id(bit).value, Some(serde_json::json!(1)));
    assert_eq!(by_id(text).value, Some(serde_json::json!("OK")));

    tokio::time::timeout(Duration::from_secs(5), engine.shutdown())
        .await
        .expect("shutdown must not hang");
}

// ---------------------------------------------------------------------------
// On-demand sessions: an engine with NO managed connections (no enabled rule
// ever references this CPU) still monitors it - the SessionDirectory spawns
// the broker task on first use and keeps it.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn monitor_spawns_a_broker_session_on_demand_for_an_unmanaged_connection() {
    let f = Fixture::new().await;
    let tag = f.tag("値", "D500", "u16", None, None, None, 0).await;
    f.sim.set_word(SlmpDevice::D, 500, 42);

    // Engine started with an EMPTY connection set: its broker owns zero
    // sessions, so the monitor's read can only work via the on-demand spawn.
    let (engine, control) = Engine::start(f.pool.clone(), Vec::new(), EngineConfig::default())
        .await
        .expect("engine start");

    let values = read_group_until_good(&control, f.group_id).await;
    assert_eq!(values[0].tag_id, tag);
    assert_eq!(values[0].value, Some(serde_json::json!(42.0)));

    // The spawned session is kept and reused - and manual writes ride the
    // same one (write-then-read-back through one session).
    write_until_ok(&control, tag, "43", "debugger").await;
    assert_eq!(f.sim.get_word(SlmpDevice::D, 500), 43);

    // Engine shutdown also stops the on-demand task (no hang, no leak).
    tokio::time::timeout(Duration::from_secs(5), engine.shutdown())
        .await
        .expect("shutdown must stop on-demand sessions too");
}

// ---------------------------------------------------------------------------
// Manual writes: land while DISARMED (no arm gate - the user's explicit
// relaxation), audited as manual_write with the actor; scaling is unscaled.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn manual_write_lands_while_disarmed_and_is_audited() {
    let f = Fixture::new().await;
    let plain = f.tag("生値", "D200", "u16", None, None, None, 0).await;
    // eng 0..=100 → raw 0..=1000: writing eng 50 must land raw 500.
    let scaled = f
        .tag(
            "スケール",
            "D210",
            "u16",
            None,
            Some((0.0, 1000.0, 0.0, 100.0)),
            None,
            1,
        )
        .await;
    let text = f.tag("文字", "D310", "string", Some(4), None, None, 0).await;
    let bit = f.tag("ビット", "M20", "bit", None, None, None, 0).await;

    let (engine, control) = Engine::start(
        f.pool.clone(),
        f.all_connections().await,
        EngineConfig::default(),
    )
    .await
    .expect("engine start");

    // Explicitly assert the relaxation: the engine is DISARMED (it always
    // starts disarmed) and the manual write must succeed anyway.
    assert!(!control.is_armed(), "engine must start disarmed");

    write_until_ok(&control, plain, "777", "debugger").await;
    assert_eq!(f.sim.get_word(SlmpDevice::D, 200), 777);

    write_until_ok(&control, scaled, "50", "debugger").await;
    assert_eq!(
        f.sim.get_word(SlmpDevice::D, 210),
        500,
        "engineering 50 must unscale to raw 500"
    );

    write_until_ok(&control, text, "OK", "debugger").await;
    assert_eq!(
        f.sim.get_word(SlmpDevice::D, 310),
        u16::from_le_bytes([b'O', b'K'])
    );

    write_until_ok(&control, bit, "1", "debugger").await;
    assert!(f.sim.get_bit(SlmpDevice::M, 20));

    // Every landed write has its manual_write/ok audit row, actor attributed,
    // with the address info in the detail JSON. (Failed rows from the
    // connecting-window retries may ALSO exist - fail-fast is audited too -
    // so assert on the ok rows, not on totals.)
    let rows = audit_rows(&f.pool).await;
    let ok_rows: Vec<_> = rows
        .iter()
        .filter(|r| r.0 == "manual_write" && r.1 == "ok")
        .collect();
    assert_eq!(ok_rows.len(), 4, "one ok row per landed write: {rows:?}");
    for row in &ok_rows {
        assert_eq!(row.2.as_deref(), Some("debugger"));
        let detail: serde_json::Value = serde_json::from_str(row.5.as_deref().unwrap()).unwrap();
        assert!(detail["address"].is_string());
        assert_eq!(detail["connectionName"], "CPU1");
    }
    let plain_row = ok_rows
        .iter()
        .find(|r| r.3 == Some(plain))
        .expect("plain tag's row");
    assert_eq!(plain_row.4, Some(777.0));
    let scaled_row = ok_rows
        .iter()
        .find(|r| r.3 == Some(scaled))
        .expect("scaled tag's row");
    assert_eq!(
        scaled_row.4,
        Some(500.0),
        "the audit records the RAW value that went to the wire"
    );
    // No arm/disarm rows: nothing about the manual path touches arming.
    assert!(rows.iter().all(|r| r.0 != "arm" && r.0 != "disarm"));

    tokio::time::timeout(Duration::from_secs(5), engine.shutdown())
        .await
        .expect("shutdown must not hang");
}

// ---------------------------------------------------------------------------
// Rejections: non-SLMP connections and unparseable values - clear errors,
// audited as failed manual writes (debug history).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn monitor_rejects_a_modbus_connection_with_a_clear_error() {
    let f = Fixture::new().await;
    let plc = PlcConnectionService::new(f.pool.clone());
    let modbus = plc
        .create(PlcConnectionInput {
            name: "MODBUS1".to_string(),
            protocol: "modbus-tcp".to_string(),
            host: "127.0.0.1".to_string(),
            port: 502,
            unit_id: 1,
            enabled: true,
        })
        .await
        .expect("create modbus connection");
    let groups = CollectionGroupService::new(f.pool.clone());
    let modbus_group = groups
        .create(CollectionGroupInput {
            name: "GM".to_string(),
            plc_connection_id: modbus.id,
            period_ms: 1000,
            enabled: true,
        })
        .await
        .expect("create modbus group");
    let modbus_tag = TagService::new(f.pool.clone())
        .create(TagInput {
            name: "MB".to_string(),
            collection_group_id: modbus_group.id,
            address: "D0".to_string(),
            data_type: "u16".to_string(),
            string_length: None,
            raw_lo: None,
            raw_hi: None,
            eng_lo: None,
            eng_hi: None,
            unit: None,
            decimals: 0,
            threshold_h: None,
            threshold_hh: None,
            threshold_l: None,
            threshold_ll: None,
            enabled: true,
        })
        .await
        .unwrap()
        .id;

    let (engine, control) = Engine::start(f.pool.clone(), Vec::new(), EngineConfig::default())
        .await
        .expect("engine start");

    let read_err = control
        .monitor_group_read(modbus_group.id)
        .await
        .expect_err("modbus group read must be rejected");
    assert!(
        read_err.to_string().contains("SLMP"),
        "clear protocol error, got: {read_err}"
    );

    let write_err = control
        .monitor_tag_write(modbus_tag, "1", Some("debugger"))
        .await
        .expect_err("modbus tag write must be rejected");
    assert!(
        write_err.to_string().contains("SLMP"),
        "clear protocol error, got: {write_err}"
    );

    // The rejected write attempt is still on the audit trail (failed).
    let rows = audit_rows(&f.pool).await;
    assert!(
        rows.iter()
            .any(|r| r.0 == "manual_write" && r.1 == "failed" && r.3 == Some(modbus_tag)),
        "expected a failed manual_write row: {rows:?}"
    );

    tokio::time::timeout(Duration::from_secs(5), engine.shutdown())
        .await
        .expect("shutdown must not hang");
}

#[tokio::test]
async fn invalid_manual_write_values_error_and_are_audited_failed() {
    let f = Fixture::new().await;
    let numeric = f.tag("数値", "D600", "u16", None, None, None, 0).await;
    let bit = f.tag("ビット", "M30", "bit", None, None, None, 0).await;
    let text = f.tag("文字", "D610", "string", Some(2), None, None, 0).await;

    let (engine, control) = Engine::start(f.pool.clone(), Vec::new(), EngineConfig::default())
        .await
        .expect("engine start");

    // Parse failures (before any wire traffic): field-level validation errors.
    for (tag, value) in [(numeric, "abc"), (bit, "2"), (text, "あいう")] {
        let err = control
            .monitor_tag_write(tag, value, Some("debugger"))
            .await
            .expect_err("invalid value must be rejected");
        assert!(
            matches!(err, BantoError::Validation { .. }),
            "expected a validation error for {value:?}, got {err:?}"
        );
    }
    // "あいう" is 6 SJIS bytes into a 2-word (4-byte) span - the field-level
    // message says so (BantoError::Validation's Display is generic; the
    // reason lives in the field errors).
    let overflow = control
        .monitor_tag_write(text, "あいう", None)
        .await
        .expect_err("overflowing string must be rejected");
    match overflow {
        BantoError::Validation { field_errors } => {
            assert!(
                field_errors
                    .iter()
                    .any(|f| f.field == "value" && f.message.contains("収まりません")),
                "expected the SJIS capacity message, got {field_errors:?}"
            );
        }
        other => panic!("expected Validation, got {other:?}"),
    }

    // A range failure caught by the wire encoder (u16 max 65535): the write
    // reaches the broker, comes back per-request Bad, and errors clearly.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let range_err = loop {
        match control.monitor_tag_write(numeric, "70000", Some("debugger")).await {
            Ok(()) => panic!("out-of-range write must not land"),
            Err(e) if e.to_string().contains("未接続") => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "session never came up"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(e) => break e,
        }
    };
    match range_err {
        BantoError::Validation { field_errors } => {
            assert!(
                field_errors.iter().any(|f| f.message.contains("範囲")),
                "expected the encoder's range message, got {field_errors:?}"
            );
        }
        other => panic!("expected Validation from the wire encoder, got {other:?}"),
    }
    assert_eq!(f.sim.get_word(SlmpDevice::D, 600), 0, "nothing was written");

    // Every rejected attempt is on the audit trail as failed, with the reason
    // (parse failures) or the intent (wire rejections) in the detail JSON.
    let rows = audit_rows(&f.pool).await;
    let failed: Vec<_> = rows
        .iter()
        .filter(|r| r.0 == "manual_write" && r.1 == "failed")
        .collect();
    assert!(
        failed.len() >= 4,
        "parse failures + the range rejection must all be audited: {rows:?}"
    );
    assert!(
        rows.iter()
            .all(|r| !(r.0 == "manual_write" && r.1 == "ok")),
        "no manual write ever succeeded here: {rows:?}"
    );

    tokio::time::timeout(Duration::from_secs(5), engine.shutdown())
        .await
        .expect("shutdown must not hang");
}
