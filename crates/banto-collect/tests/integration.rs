//! End-to-end tests driving the real collection engine against `banto-plc`'s
//! in-process Modbus TCP simulator (the reuse it was made public for -
//! `banto-plc/src/modbus/simulator.rs`'s module doc). These cover the
//! behaviours unit tests cannot: real sockets, real timers, and the full
//! start -> collect -> disconnect -> reconnect -> stop lifecycle.
//!
//! Registry/event state lives in a *file-backed* SQLite database (not
//! `:memory:`): the engine's pool hands out multiple connections
//! (`build_config` reads, several tasks persist events), and each `:memory:`
//! connection would be a separate empty database. A temp file is the shared,
//! concurrency-safe store these tests need.
//!
//! Timings are real (no `tokio::time::pause` here - the engine talks to a
//! real socket, which virtual time cannot drive), shrunk via
//! [`CollectorOptions`] and asserted with generous CI margins.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use banto_collect::{
    build_config, BackoffConfig, Collector, CollectorOptions, ConnectionStatus, EventSink, Quality,
};
use banto_plc::modbus::simulator::Simulator;
use banto_tags::{
    CollectionGroupInput, CollectionGroupService, PlcConnectionInput, PlcConnectionService,
    TagInput, TagService,
};
use banto_tstore::{ManualClock, SystemClock, TsReader, WriterOptions};
use sqlx::SqlitePool;

// ---------------------------------------------------------------------------
// Test scaffolding
// ---------------------------------------------------------------------------

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A temp directory holding the registry database and the tstore data dir,
/// cleaned up on drop (best-effort - SQLite WAL sidecars may briefly linger).
struct TempEnv {
    root: PathBuf,
}

impl TempEnv {
    fn new(label: &str) -> Self {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "banto-collect-it-{}-{label}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("create temp env");
        Self { root }
    }

    fn registry_path(&self) -> PathBuf {
        self.root.join("registry.sqlite3")
    }

    fn data_dir(&self) -> PathBuf {
        self.root.join("data")
    }
}

impl Drop for TempEnv {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Open the shared registry/event pool and apply both crates' schemas
/// (the same startup sequence the ChronoGazer app will run).
async fn open_registry(env: &TempEnv) -> SqlitePool {
    let pool = banto_storage::connect_sqlite(env.registry_path())
        .await
        .expect("connect registry");
    banto_tags::migrate(&pool).await.expect("tags migrate");
    banto_collect::migrate(&pool)
        .await
        .expect("collect migrate");
    pool
}

/// Fast timings so tests finish quickly and reconnect/backoff is observable
/// in well under a second. `response_timeout` stays at 500ms - tight enough
/// that a hung simulator is detected fast, loose enough that a loaded CI
/// runner's loopback round trip never trips it spuriously.
fn fast_options() -> CollectorOptions {
    CollectorOptions {
        backoff: BackoffConfig {
            base: Duration::from_millis(20),
            max: Duration::from_millis(100),
        },
        connect_timeout: Duration::from_millis(500),
        response_timeout: Duration::from_millis(500),
        // Flush on every append so a reader opened after stop() sees every row
        // (individual tests override where buffering itself is under test).
        writer_options: WriterOptions {
            max_buffered_rows: 1,
            flush_interval_ms: 0,
        },
    }
}

fn conn_input(name: &str, port: u16) -> PlcConnectionInput {
    PlcConnectionInput {
        name: name.to_string(),
        protocol: "modbus-tcp".to_string(),
        host: "127.0.0.1".to_string(),
        port: port as i64,
        unit_id: 1,
        enabled: true,
    }
}

fn group_input(name: &str, conn_id: i64, period_ms: i64) -> CollectionGroupInput {
    CollectionGroupInput {
        name: name.to_string(),
        plc_connection_id: conn_id,
        period_ms,
        enabled: true,
    }
}

fn tag_input(name: &str, group_id: i64, address: &str, data_type: &str) -> TagInput {
    TagInput {
        name: name.to_string(),
        collection_group_id: group_id,
        address: address.to_string(),
        data_type: data_type.to_string(),
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
    }
}

/// Poll `predicate` every 20ms until it returns true or `timeout` elapses.
/// Returns whether it became true.
async fn wait_until<F, Fut>(timeout: Duration, mut predicate: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if predicate().await {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn count_events(pool: &SqlitePool, kind: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM collect_events WHERE kind = ?")
        .bind(kind)
        .fetch_one(pool)
        .await
        .unwrap_or(0)
}

/// All rows for the single group in a single-file data dir (open after
/// stop(), when the writer has flushed and closed).
async fn read_single_group_rows(data_dir: &Path) -> Vec<banto_tstore::Sample> {
    let files = banto_tstore::list_data_files(data_dir).expect("list files");
    assert_eq!(files.len(), 1, "expected exactly one data file");
    let reader = TsReader::open(&files[0].path).await.expect("open reader");
    let group_key = reader.groups()[0].key.clone();
    reader
        .read_range(&group_key, 0, i64::MAX)
        .await
        .expect("read range")
}

// ---------------------------------------------------------------------------
// Startup / values / scaling
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn collects_values_in_tag_order_with_scaling_applied() {
    let env = TempEnv::new("values");
    let sim = Simulator::start().await;
    // t1: raw i16 (unscaled). t2: i16 scaled 0..4095 -> 0..100.
    sim.set_holding_register(0, 1234); // 40001
    sim.set_holding_register(1, 2048); // 40002

    let pool = open_registry(&env).await;
    let conn = PlcConnectionService::new(pool.clone())
        .create(conn_input("PLC1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(pool.clone())
        .create(group_input("G1", conn.id, 100))
        .await
        .unwrap();
    let tag_svc = TagService::new(pool.clone());
    tag_svc
        .create(tag_input("t1", group.id, "40001", "i16"))
        .await
        .unwrap();
    let mut scaled = tag_input("t2", group.id, "40002", "i16");
    scaled.raw_lo = Some(0.0);
    scaled.raw_hi = Some(4095.0);
    scaled.eng_lo = Some(0.0);
    scaled.eng_hi = Some(100.0);
    tag_svc.create(scaled).await.unwrap();

    let config = build_config(&pool).await.unwrap();
    let collector = Collector::start(
        config,
        &env.data_dir(),
        Arc::new(SystemClock),
        EventSink::new(pool.clone()),
        fast_options(),
    )
    .await
    .unwrap();

    // Let several ticks land, verified via the cache before stopping.
    let current = collector.current_values();
    assert!(
        wait_until(Duration::from_secs(3), || async {
            current.get("tag:1").map(|s| s.value) == Some(Some(1234.0))
        })
        .await,
        "t1 should read its raw value"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    collector.stop().await.unwrap();
    sim.stop();

    let rows = read_single_group_rows(&env.data_dir()).await;
    let good = rows
        .iter()
        .find(|r| r.values[0].is_some() && r.values[1].is_some())
        .expect("a fully-read row");
    assert_eq!(good.values[0], Some(1234.0), "t1 unscaled");
    let t2 = good.values[1].unwrap();
    assert!(
        (t2 - 50.012).abs() < 0.1,
        "t2 scaled 2048 -> ~50.01, got {t2}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bit_tags_record_zero_or_one() {
    let env = TempEnv::new("bit");
    let sim = Simulator::start().await;
    sim.set_coil(0, true); // 00001

    let pool = open_registry(&env).await;
    let conn = PlcConnectionService::new(pool.clone())
        .create(conn_input("PLC1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(pool.clone())
        .create(group_input("G1", conn.id, 100))
        .await
        .unwrap();
    TagService::new(pool.clone())
        .create(tag_input("bit", group.id, "00001", "bit"))
        .await
        .unwrap();

    let config = build_config(&pool).await.unwrap();
    let collector = Collector::start(
        config,
        &env.data_dir(),
        Arc::new(SystemClock),
        EventSink::new(pool.clone()),
        fast_options(),
    )
    .await
    .unwrap();
    let current = collector.current_values();
    assert!(
        wait_until(Duration::from_secs(3), || async {
            current.get("tag:1").map(|s| s.value) == Some(Some(1.0))
        })
        .await,
        "coil true should read 1.0"
    );
    collector.stop().await.unwrap();
    sim.stop();

    let rows = read_single_group_rows(&env.data_dir()).await;
    assert!(
        rows.iter().any(|r| r.values[0] == Some(1.0)),
        "coil true should record 1.0"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lifecycle_events_are_emitted_and_persisted() {
    let env = TempEnv::new("lifecycle");
    let sim = Simulator::start().await;
    let pool = open_registry(&env).await;
    let conn = PlcConnectionService::new(pool.clone())
        .create(conn_input("PLC1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(pool.clone())
        .create(group_input("G1", conn.id, 100))
        .await
        .unwrap();
    TagService::new(pool.clone())
        .create(tag_input("t1", group.id, "40001", "i16"))
        .await
        .unwrap();

    let config = build_config(&pool).await.unwrap();
    let events = EventSink::new(pool.clone());
    let mut rx = events.subscribe();
    let collector = Collector::start(
        config,
        &env.data_dir(),
        Arc::new(SystemClock),
        events,
        fast_options(),
    )
    .await
    .unwrap();

    assert!(
        wait_until(Duration::from_secs(3), || async {
            count_events(&pool, "plc_connected").await >= 1
        })
        .await,
        "expected plc_connected"
    );

    collector.stop().await.unwrap();
    sim.stop();

    assert_eq!(count_events(&pool, "collection_started").await, 1);
    assert_eq!(count_events(&pool, "collection_stopped").await, 1);
    // A healthy run connects exactly once, and a re-established socket would
    // be plc_reconnected (different kind), so this stays exactly 1.
    assert_eq!(count_events(&pool, "plc_connected").await, 1);

    // The live channel delivered them too (started + connected + stopped).
    let mut live = 0;
    while rx.try_recv().is_ok() {
        live += 1;
    }
    assert!(
        live >= 3,
        "live channel should have delivered events, got {live}"
    );
}

// ---------------------------------------------------------------------------
// Disconnect / reconnect
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hard_drop_keeps_appending_null_rows_and_marks_reconnecting() {
    let env = TempEnv::new("drop");
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 7);
    let pool = open_registry(&env).await;
    let conn = PlcConnectionService::new(pool.clone())
        .create(conn_input("PLC1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(pool.clone())
        .create(group_input("G1", conn.id, 100))
        .await
        .unwrap();
    TagService::new(pool.clone())
        .create(tag_input("t1", group.id, "40001", "i16"))
        .await
        .unwrap();

    let config = build_config(&pool).await.unwrap();
    let conn_key = format!("conn:{}", conn.id);
    let collector = Collector::start(
        config,
        &env.data_dir(),
        Arc::new(SystemClock),
        EventSink::new(pool.clone()),
        fast_options(),
    )
    .await
    .unwrap();

    // Wait for an actual successful read (not just Connected status), so the
    // store is guaranteed to contain real values from before the drop.
    let current = collector.current_values();
    assert!(
        wait_until(Duration::from_secs(3), || async {
            current.get("tag:1").map(|s| s.value) == Some(Some(7.0))
        })
        .await,
        "should read a real value before the drop"
    );

    // Kill the PLC entirely (listener + live sockets severed).
    sim.stop();

    assert!(
        wait_until(Duration::from_secs(5), || async {
            count_events(&pool, "plc_disconnected").await >= 1
        })
        .await,
        "expected plc_disconnected"
    );
    assert!(
        wait_until(Duration::from_secs(5), || async {
            matches!(
                collector.status().get(&conn_key),
                Some(ConnectionStatus::Reconnecting { .. })
            )
        })
        .await,
        "expected Reconnecting status while the PLC is down"
    );

    // Ticks never stop: the cache keeps updating with Bad quality while down.
    assert!(
        wait_until(Duration::from_secs(3), || async {
            current.get("tag:1").map(|s| s.quality) == Some(Quality::Bad)
        })
        .await,
        "cache should go Bad while disconnected"
    );

    // Give it a few more disconnected ticks, then stop and inspect the rows.
    tokio::time::sleep(Duration::from_millis(400)).await;
    collector.stop().await.unwrap();

    let rows = read_single_group_rows(&env.data_dir()).await;
    assert!(
        rows.iter().any(|r| r.values[0] == Some(7.0)),
        "should have real values from before the drop"
    );
    assert!(
        rows.iter().any(|r| r.values[0].is_none()),
        "should have NULL rows recorded while disconnected"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_reconnects_and_values_resume_after_recovery() {
    let env = TempEnv::new("reconnect");
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 55);
    let pool = open_registry(&env).await;
    let conn = PlcConnectionService::new(pool.clone())
        .create(conn_input("PLC1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(pool.clone())
        .create(group_input("G1", conn.id, 100))
        .await
        .unwrap();
    TagService::new(pool.clone())
        .create(tag_input("t1", group.id, "40001", "i16"))
        .await
        .unwrap();

    let config = build_config(&pool).await.unwrap();
    let collector = Collector::start(
        config,
        &env.data_dir(),
        Arc::new(SystemClock),
        EventSink::new(pool.clone()),
        fast_options(),
    )
    .await
    .unwrap();
    let current = collector.current_values();

    assert!(
        wait_until(Duration::from_secs(3), || async {
            current.get("tag:1").map(|s| s.value) == Some(Some(55.0))
        })
        .await,
        "should read Good initially"
    );

    // Make the PLC unresponsive: reads time out (connection-fatal) but the
    // listener stays up, so this exercises the timeout-drop path.
    sim.hang();
    assert!(
        wait_until(Duration::from_secs(5), || async {
            count_events(&pool, "plc_disconnected").await >= 1
        })
        .await,
        "hang should cause a disconnect"
    );

    // Recover: the PLC answers again. The backoff loop must re-establish and
    // values must resume without any outside intervention.
    sim.stop_hanging();
    assert!(
        wait_until(Duration::from_secs(5), || async {
            count_events(&pool, "plc_reconnected").await >= 1
        })
        .await,
        "expected plc_reconnected after recovery"
    );
    assert!(
        wait_until(Duration::from_secs(5), || async {
            current.get("tag:1").map(|s| (s.value, s.quality)) == Some((Some(55.0), Quality::Good))
        })
        .await,
        "values should resume Good after reconnect"
    );

    collector.stop().await.unwrap();
    sim.stop();
}

// ---------------------------------------------------------------------------
// Thresholds
// ---------------------------------------------------------------------------

async fn entered_levels(pool: &SqlitePool) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT level FROM collect_events WHERE kind = 'threshold_entered' AND level IS NOT NULL",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn threshold_entered_and_cleared_fire_only_on_edges() {
    let env = TempEnv::new("threshold");
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 30); // start in the normal band
    let pool = open_registry(&env).await;
    let conn = PlcConnectionService::new(pool.clone())
        .create(conn_input("PLC1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(pool.clone())
        .create(group_input("G1", conn.id, 100))
        .await
        .unwrap();
    let mut t = tag_input("t1", group.id, "40001", "i16");
    t.threshold_ll = Some(5.0);
    t.threshold_l = Some(20.0);
    t.threshold_h = Some(50.0);
    t.threshold_hh = Some(90.0);
    TagService::new(pool.clone()).create(t).await.unwrap();

    let config = build_config(&pool).await.unwrap();
    let collector = Collector::start(
        config,
        &env.data_dir(),
        Arc::new(SystemClock),
        EventSink::new(pool.clone()),
        fast_options(),
    )
    .await
    .unwrap();
    let current = collector.current_values();

    // Several normal-band ticks: no threshold events (state-change only, and
    // repeating the same in-band value is not a change).
    assert!(
        wait_until(Duration::from_secs(3), || async {
            current.get("tag:1").map(|s| s.value) == Some(Some(30.0))
        })
        .await
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        count_events(&pool, "threshold_entered").await,
        0,
        "an in-band value must not raise a threshold event"
    );

    // Cross into H.
    sim.set_holding_register(0, 60);
    assert!(
        wait_until(Duration::from_secs(3), || async {
            entered_levels(&pool).await.contains(&"H".to_string())
        })
        .await,
        "expected threshold_entered at H"
    );

    // Escalate into HH: clears H, enters HH (one edge, one pair of events).
    sim.set_holding_register(0, 95);
    assert!(
        wait_until(Duration::from_secs(3), || async {
            entered_levels(&pool).await.contains(&"HH".to_string())
        })
        .await,
        "expected threshold_entered at HH"
    );

    // Return to normal: clears HH.
    sim.set_holding_register(0, 30);
    assert!(
        wait_until(Duration::from_secs(3), || async {
            count_events(&pool, "threshold_cleared").await >= 2
        })
        .await,
        "expected clears for the H->HH and HH->normal edges"
    );

    // Dip below L to prove the low side works too.
    sim.set_holding_register(0, 10);
    assert!(
        wait_until(Duration::from_secs(3), || async {
            entered_levels(&pool).await.contains(&"L".to_string())
        })
        .await,
        "expected threshold_entered at L"
    );

    collector.stop().await.unwrap();
    sim.stop();
}

// ---------------------------------------------------------------------------
// Current-value cache quality transitions
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn current_value_quality_transitions_good_bad_stale() {
    let env = TempEnv::new("quality");
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 42);
    let pool = open_registry(&env).await;
    let conn = PlcConnectionService::new(pool.clone())
        .create(conn_input("PLC1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(pool.clone())
        .create(group_input("G1", conn.id, 100))
        .await
        .unwrap();
    TagService::new(pool.clone())
        .create(tag_input("t1", group.id, "40001", "i16"))
        .await
        .unwrap();

    let config = build_config(&pool).await.unwrap();
    // ManualClock so staleness can be forced deterministically after stop.
    // (Frozen "now" makes every append share one ptime; the tstore INTEGER
    // PRIMARY KEY rejects the duplicates and the engine swallows that, which
    // is fine here - this test only inspects the in-memory cache.)
    let clock = Arc::new(ManualClock::new(1_760_000_000_000, 0));
    let collector = Collector::start(
        config,
        &env.data_dir(),
        clock.clone(),
        EventSink::new(pool.clone()),
        fast_options(),
    )
    .await
    .unwrap();
    let current = collector.current_values();

    // Good while connected and reading.
    assert!(
        wait_until(Duration::from_secs(3), || async {
            matches!(current.get("tag:1").map(|s| s.quality), Some(Quality::Good))
        })
        .await,
        "expected Good"
    );

    // Bad while the PLC is unresponsive.
    sim.hang();
    assert!(
        wait_until(Duration::from_secs(5), || async {
            matches!(current.get("tag:1").map(|s| s.quality), Some(Quality::Bad))
        })
        .await,
        "expected Bad while hung"
    );

    // Recover to Good (only a stored-Good sample ages into Stale; Bad stays
    // Bad no matter how old - asserted by the unit tests).
    sim.stop_hanging();
    assert!(
        wait_until(Duration::from_secs(5), || async {
            matches!(current.get("tag:1").map(|s| s.quality), Some(Quality::Good))
        })
        .await,
        "expected Good after recovery"
    );

    // Stop updates entirely, then advance the injected clock past
    // period x 2.5 -> the same stored sample now reads Stale.
    collector.stop().await.unwrap();
    sim.stop();
    clock.advance_ms(100 * 3); // period 100ms, factor 2.5 -> >250ms is stale
    assert_eq!(
        current.get("tag:1").map(|s| s.quality),
        Some(Quality::Stale),
        "a Good sample with no further updates should read Stale"
    );
}

// ---------------------------------------------------------------------------
// Stop guarantees
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_flushes_buffered_rows_so_none_are_lost() {
    let env = TempEnv::new("stop-flush");
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 3);
    let pool = open_registry(&env).await;
    let conn = PlcConnectionService::new(pool.clone())
        .create(conn_input("PLC1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(pool.clone())
        .create(group_input("G1", conn.id, 100))
        .await
        .unwrap();
    TagService::new(pool.clone())
        .create(tag_input("t1", group.id, "40001", "i16"))
        .await
        .unwrap();

    let config = build_config(&pool).await.unwrap();
    // Buffer generously and never flush on interval/row-count: only stop()'s
    // final flush can move rows to disk, so every row a post-stop reader sees
    // proves stop() flushed.
    let options = CollectorOptions {
        writer_options: WriterOptions {
            max_buffered_rows: 100_000,
            flush_interval_ms: 3_600_000,
        },
        ..fast_options()
    };
    let collector = Collector::start(
        config,
        &env.data_dir(),
        Arc::new(SystemClock),
        EventSink::new(pool.clone()),
        options,
    )
    .await
    .unwrap();
    let current = collector.current_values();
    assert!(
        wait_until(Duration::from_secs(3), || async {
            current.get("tag:1").map(|s| s.value) == Some(Some(3.0))
        })
        .await
    );
    tokio::time::sleep(Duration::from_millis(400)).await;
    collector.stop().await.unwrap();
    sim.stop();

    let rows = read_single_group_rows(&env.data_dir()).await;
    assert!(
        rows.len() >= 3,
        "buffered rows must survive stop(); got {}",
        rows.len()
    );
    assert!(rows.iter().any(|r| r.values[0] == Some(3.0)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_reports_connected_while_running_and_stop_completes_cleanly() {
    let env = TempEnv::new("status");
    let sim = Simulator::start().await;
    let pool = open_registry(&env).await;
    let conn = PlcConnectionService::new(pool.clone())
        .create(conn_input("PLC1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(pool.clone())
        .create(group_input("G1", conn.id, 100))
        .await
        .unwrap();
    TagService::new(pool.clone())
        .create(tag_input("t1", group.id, "40001", "i16"))
        .await
        .unwrap();

    let config = build_config(&pool).await.unwrap();
    let conn_key = format!("conn:{}", conn.id);
    let collector = Collector::start(
        config,
        &env.data_dir(),
        Arc::new(SystemClock),
        EventSink::new(pool.clone()),
        fast_options(),
    )
    .await
    .unwrap();

    assert!(
        wait_until(Duration::from_secs(3), || async {
            matches!(
                collector.status().get(&conn_key),
                Some(ConnectionStatus::Connected)
            )
        })
        .await,
        "expected Connected status"
    );

    // stop() returning (rather than hanging on a task that never drains) is
    // the task-leak assertion here; the collection_stopped row proves the
    // full shutdown sequence ran.
    collector.stop().await.unwrap();
    sim.stop();
    assert_eq!(count_events(&pool, "collection_stopped").await, 1);
}

// ---------------------------------------------------------------------------
// collect_events persistence round-trip
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn collect_events_rows_carry_the_full_shape() {
    let env = TempEnv::new("events-roundtrip");
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 100);
    let pool = open_registry(&env).await;
    let conn = PlcConnectionService::new(pool.clone())
        .create(conn_input("PLC1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(pool.clone())
        .create(group_input("G1", conn.id, 100))
        .await
        .unwrap();
    let mut t = tag_input("t1", group.id, "40001", "i16");
    t.threshold_h = Some(50.0);
    TagService::new(pool.clone()).create(t).await.unwrap();

    let config = build_config(&pool).await.unwrap();
    let collector = Collector::start(
        config,
        &env.data_dir(),
        Arc::new(SystemClock),
        EventSink::new(pool.clone()),
        fast_options(),
    )
    .await
    .unwrap();

    // Value 100 >= H(50): the first Good tick raises threshold_entered.
    assert!(
        wait_until(Duration::from_secs(3), || async {
            count_events(&pool, "threshold_entered").await >= 1
        })
        .await,
        "expected a threshold event"
    );
    collector.stop().await.unwrap();
    sim.stop();

    let row: (
        i64,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<f64>,
    ) = sqlx::query_as(
        "SELECT ts, kind, connection_key, tag_key, level, value FROM collect_events \
             WHERE kind = 'threshold_entered' LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(row.0 > 1_700_000_000_000, "ts should be a real epoch-ms");
    assert_eq!(row.1, "threshold_entered");
    assert_eq!(row.2.as_deref(), Some(format!("conn:{}", conn.id).as_str()));
    assert_eq!(row.3.as_deref(), Some("tag:1"));
    assert_eq!(row.4.as_deref(), Some("H"));
    assert_eq!(row.5, Some(100.0));

    // A lifecycle row carries no connection/tag/level/value.
    let started: (Option<String>, Option<String>, Option<String>, Option<f64>) = sqlx::query_as(
        "SELECT connection_key, tag_key, level, value FROM collect_events \
         WHERE kind = 'collection_started'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(started, (None, None, None, None));

    // A disconnect row carries a reason in detail (from the stop above there
    // is none - force one by checking the disconnected rows only if present).
    let disconnect_details: Vec<Option<String>> =
        sqlx::query_scalar("SELECT detail FROM collect_events WHERE kind = 'plc_disconnected'")
            .fetch_all(&pool)
            .await
            .unwrap();
    for d in disconnect_details {
        assert!(d.is_some(), "plc_disconnected rows should carry a reason");
    }
}

// ---------------------------------------------------------------------------
// Mini soak
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mini_soak_100ms_three_groups_row_counts_within_tolerance() {
    let env = TempEnv::new("mini-soak");
    let sim = Simulator::start().await;
    for i in 0..3u16 {
        sim.set_holding_register(i, 100 + i);
    }
    let pool = open_registry(&env).await;
    let conn = PlcConnectionService::new(pool.clone())
        .create(conn_input("PLC1", sim.addr.port()))
        .await
        .unwrap();
    let group_svc = CollectionGroupService::new(pool.clone());
    let tag_svc = TagService::new(pool.clone());
    for g in 0..3 {
        let group = group_svc
            .create(group_input(&format!("G{g}"), conn.id, 100))
            .await
            .unwrap();
        tag_svc
            .create(tag_input(
                &format!("t{g}"),
                group.id,
                &format!("4000{}", g + 1),
                "i16",
            ))
            .await
            .unwrap();
    }

    let config = build_config(&pool).await.unwrap();
    assert_eq!(config.group_count(), 3);
    let collector = Collector::start(
        config,
        &env.data_dir(),
        Arc::new(SystemClock),
        EventSink::new(pool.clone()),
        fast_options(),
    )
    .await
    .unwrap();

    let run_secs = 3u64;
    let started = std::time::Instant::now();
    tokio::time::sleep(Duration::from_secs(run_secs)).await;
    // stop() returning at all proves every task drained (no leak/hang).
    collector.stop().await.unwrap();
    let elapsed = started.elapsed();
    sim.stop();

    // ~30 ticks per group in 3s at 100ms; generous CI margins (a busy runner
    // may skip ticks - by design those surface as fewer rows, not bursts).
    let files = banto_tstore::list_data_files(&env.data_dir()).unwrap();
    assert_eq!(files.len(), 1);
    let reader = TsReader::open(&files[0].path).await.unwrap();
    assert_eq!(reader.groups().len(), 3);
    for g in reader.groups() {
        let rows = reader.read_range(&g.key, 0, i64::MAX).await.unwrap();
        println!(
            "mini-soak: group {} recorded {} rows in {elapsed:?} (theoretical ~{})",
            g.key,
            rows.len(),
            run_secs * 10
        );
        // Lower bound is deliberately loose (>=10, i.e. a third of the
        // theoretical ~30): the scheduler is MissedTickBehavior::Skip, so a
        // busy CI runner can only ever LOSE ticks, never burst extra rows -
        // the >=18 bound proved flaky on real CI. What this still catches is
        // a collector that stalls outright (0 rows) or grinds to a crawl,
        // while the upper bound still pins "skip, don't burst". Tight timing
        // guarantees are the #[ignore]d long soak's job, not CI's (same
        // convention as banto-plc's perf smokes: wall-clock numbers are not a
        // CI failure condition).
        assert!(
            rows.len() >= 10 && rows.len() <= 50,
            "group {} expected ~30 rows in 3s @100ms (>=10 tolerated for busy \
             runners), got {}",
            g.key,
            rows.len()
        );
        // Values must be the configured register values, never garbage.
        assert!(rows
            .iter()
            .filter_map(|r| r.values[0])
            .all(|v| (100.0..103.0).contains(&v)));
    }
}

/// Long-form soak, ignored by default - the seed of the future 72h release
/// gate (recorder-requirements.md §4 "ソークテスト"). Run explicitly with
/// `cargo test -p banto-collect --test integration -- --ignored`.
#[ignore = "long-running (60s) soak; run explicitly with --ignored"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn long_soak_sixty_seconds() {
    let env = TempEnv::new("long-soak");
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 1);
    let pool = open_registry(&env).await;
    let conn = PlcConnectionService::new(pool.clone())
        .create(conn_input("PLC1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(pool.clone())
        .create(group_input("G1", conn.id, 100))
        .await
        .unwrap();
    TagService::new(pool.clone())
        .create(tag_input("t1", group.id, "40001", "i16"))
        .await
        .unwrap();

    let config = build_config(&pool).await.unwrap();
    let collector = Collector::start(
        config,
        &env.data_dir(),
        Arc::new(SystemClock),
        EventSink::new(pool.clone()),
        fast_options(),
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_secs(60)).await;
    collector.stop().await.unwrap();
    sim.stop();

    let rows = read_single_group_rows(&env.data_dir()).await;
    // ~600 ticks in 60s @100ms; require the vast majority to have landed.
    println!("long-soak: {} rows in 60s (theoretical ~600)", rows.len());
    assert!(
        rows.len() > 500,
        "60s soak expected >500 rows, got {}",
        rows.len()
    );
    // No unexpected gaps: every recorded value is the configured register.
    assert!(rows.iter().filter_map(|r| r.values[0]).all(|v| v == 1.0));
}
