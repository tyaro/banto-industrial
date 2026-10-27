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
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use banto_collect::{
    build_config, build_config_lenient_with_thresholds, ApplyReport, BackoffConfig, BrokerSessions,
    CollectError, Collector, CollectorConfig, CollectorOptions, ConnectionStatus, EventSink,
    HubSource, Quality, RegistrySnapshot, Thresholds,
};
use banto_driver_api::DriverRegistry;
use banto_plc::modbus::simulator::Simulator;
use banto_plc::slmp::address::SlmpDevice;
use banto_plc::slmp::simulator::Simulator as SlmpSimulator;
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

// #227: この binary の tokio::test は全て multi_thread ランタイムを立てる。
// cargo test の既定並列で複数ランタイムが同時に走ると、少コア CI で
// コアを奪い合い収集タスクが starve し、100ms 周期のティックが 10 秒
// 窓に入らず wait_until がタイムアウトしていた。テストごとに新しい
// ランタイムを立てるのは維持しつつ、この直列ガードで「同時に走る
// 重いランタイムは常に1つ」に制限して starvation を無くす。
// tokio::sync::Mutex を使うのは clippy::await_holding_lock を避けるため
// (std/parking_lot のガードを .await 跨ぎで持つと lint が出る)。
static TEST_SERIAL: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));

/// A temp directory holding the registry database and the tstore data dir.
///
/// ## Cleanup: why `drop` retries (banto-hub-core PR #54/#55 investigation)
///
/// On Windows, closing a WAL-mode `SqlitePool` clone does not synchronously
/// release the underlying file handles - the OS can keep the file "in use"
/// for a short window even after the async close completes. A
/// `remove_dir_all` issued immediately after the last pool clone drops
/// observes `ERROR_SHARING_VIOLATION` almost every time (measured ~7%
/// immediate success across repeated trials in `banto-hub-core`, which
/// shares this exact `TempEnv` shape - see
/// `apps/banto-hub/core/tests/common/mod.rs`'s module doc for the full
/// writeup). [`TempEnv::drop`] retries on a short delay to reliably close
/// this window (measured 100% success, usually converging within the first
/// 1-2 attempts).
///
/// This requires every test owning a `TempEnv` to run on a multi-thread
/// tokio runtime with >= 2 workers (`Drop::drop` is synchronous, so the
/// retry can only block via `std::thread::sleep` - on a single-threaded
/// runtime that would starve the only worker thread and prevent the
/// background close from ever being polled; every `#[tokio::test]` in this
/// file already uses `flavor = "multi_thread"`).
struct TempEnv {
    root: PathBuf,
}

/// Delay between `remove_dir_all` retries in [`TempEnv::drop`].
const TEMP_ENV_RETRY_DELAY: Duration = Duration::from_millis(50);

/// Retry ceiling in [`TempEnv::drop`] - `TEMP_ENV_RETRY_DELAY * TEMP_ENV_MAX_ATTEMPTS`
/// (~2s) is the worst-case teardown block, kept generous because the
/// measured common case converges within 1-2 attempts.
const TEMP_ENV_MAX_ATTEMPTS: u32 = 40;

impl TempEnv {
    fn new(label: &str) -> Self {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        // ナノ秒精度のタイムスタンプも一意性キーに含める(PID 再利用時に
        // 古い(既に初期化済みの)ディレクトリと衝突しないよう -
        // banto-hub-core PR #54 で確認された衝突パターンと同じ対策)。
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "banto-collect-it-{}-{label}-{id}-{nanos}",
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
        for attempt in 1..=TEMP_ENV_MAX_ATTEMPTS {
            match std::fs::remove_dir_all(&self.root) {
                Ok(()) => return,
                Err(_) if attempt < TEMP_ENV_MAX_ATTEMPTS => {
                    std::thread::sleep(TEMP_ENV_RETRY_DELAY);
                }
                Err(err) => {
                    eprintln!(
                        "TempEnv: giving up removing {:?} after {attempt} attempts: {err}",
                        self.root
                    );
                }
            }
        }
    }
}

/// Open the shared registry/event pool and apply both crates' schemas
/// (the same startup sequence the ChronoGazer app will run).
/// The built-in drivers (Modbus TCP and SLMP): every plan is read through
/// them and every PLC read goes through a broker session dialing them
/// (R2b-3a-3, docs/banto-hub-driver-io-design.md §4.3・§8).
fn drivers() -> &'static DriverRegistry {
    &DRIVERS
}

static DRIVERS: LazyLock<Arc<DriverRegistry>> =
    LazyLock::new(|| Arc::new(banto_drivers::builtin().registry));

/// The broker sessions one test's collector reads through
/// ([`BrokerSessions`], what ChronoGazer uses). Dropping it stops every
/// session (the supervisor's shutdown signal goes away with it), so keeping
/// it alive until the end of the test is enough.
struct Broker(BrokerSessions);

/// Start a collector the way a host without a broker of its own does
/// (R2b-3a-3): sync the broker sessions for `config` from the registry in
/// `pool`, then start with the factory that reads through them.
async fn start_collector(
    pool: &SqlitePool,
    mut config: CollectorConfig,
    data_dir: &Path,
    clock: Arc<dyn banto_tstore::Clock>,
    events: EventSink,
    options: CollectorOptions,
) -> Result<(Collector, Broker), CollectError> {
    let broker = Broker(BrokerSessions::new(
        DRIVERS.clone(),
        options.broker_options(),
    ));
    let snapshot = RegistrySnapshot::load(pool).await.unwrap();
    let sync = broker.0.sync(&snapshot, &mut config).await;
    let collector = Collector::start_with_client_factory(
        config,
        data_dir,
        clock,
        events,
        options,
        sync.factory,
    )
    .await?;
    Ok((collector, broker))
}

/// `apply_config` through the same broker sessions: sync for the new config,
/// apply, then stop the sessions that fell out of it.
async fn apply_through(
    collector: &mut Collector,
    broker: &Broker,
    pool: &SqlitePool,
    mut config: CollectorConfig,
) -> Result<ApplyReport, CollectError> {
    let snapshot = RegistrySnapshot::load(pool).await.unwrap();
    let sync = broker.0.sync(&snapshot, &mut config).await;
    let report = collector.apply_config(config, sync.factory).await?;
    broker.0.remove(&sync.stale).await;
    Ok(report)
}

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
        simulation: false,

        word_order: "low_high".to_string(),
        cpu_type: None,
        database: None,
        username: None,
        secrets: Default::default(),
        settings: None,
    }
}

/// I8 (2026-08-05): the `"slmp"` twin of [`conn_input`] - same shape, just
/// `protocol` and a port from [`SlmpSimulator`] instead of the Modbus one.
/// `unit_id` is carried over unused (SLMP has no such concept; `banto-tags`
/// still requires the column, and `banto-collect`'s `slmp_config_for` simply
/// never reads it).
fn slmp_conn_input(name: &str, port: u16) -> PlcConnectionInput {
    PlcConnectionInput {
        protocol: "slmp".to_string(),
        ..conn_input(name, port)
    }
}

/// #334 regression coverage: [`conn_input`] always registers
/// `word_order = "low_high"`, which cannot exercise the "high_low" contrast
/// `word_order_low_high_is_honored_by_the_polling_path` needs to prove the
/// setting is actually read, not just defaulted-and-happened-to-match.
fn conn_input_with_word_order(name: &str, port: u16, word_order: &str) -> PlcConnectionInput {
    PlcConnectionInput {
        word_order: word_order.to_string(),
        ..conn_input(name, port)
    }
}

fn group_input(name: &str, conn_id: i64, period_ms: i64) -> CollectionGroupInput {
    CollectionGroupInput {
        name: name.to_string(),
        plc_connection_id: conn_id,
        period_ms,
        enabled: true,
        default_writable: true,
        query_sql: None,
    }
}

fn tag_input(name: &str, group_id: i64, address: &str, data_type: &str) -> TagInput {
    TagInput {
        comment: None,
        name: name.to_string(),
        collection_group_id: group_id,
        address: address.to_string(),
        data_type: data_type.to_string(),
        string_length: None,
        string_encoding: "utf8".to_string(),
        raw_lo: None,
        raw_hi: None,
        eng_lo: None,
        eng_hi: None,
        unit: None,
        decimals: 0,
        enabled: true,
        writable: false,
        tag_kind: "plc".to_string(),
        expression: None,
        retain: false,
        expected_revision: None,
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

/// All rows for the single group in the data dir (open after stop(), when
/// the writer has flushed and closed). tstore switches files per local date,
/// so a run that crosses local midnight leaves several files: read them all
/// and concatenate in ptime order (#343).
async fn read_all_group_rows(data_dir: &Path) -> Vec<banto_tstore::Sample> {
    let files = banto_tstore::list_data_files(data_dir).expect("list files");
    assert!(!files.is_empty(), "expected at least one data file");
    let mut rows = Vec::new();
    for file in &files {
        let reader = TsReader::open(&file.path).await.expect("open reader");
        let group_key = reader.groups()[0].key.clone();
        rows.extend(
            reader
                .read_range(&group_key, 0, i64::MAX)
                .await
                .expect("read range"),
        );
    }
    rows.sort_by_key(|row| row.ptime_ms);
    rows
}

// ---------------------------------------------------------------------------
// Startup / values / scaling
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn collects_values_in_tag_order_with_scaling_applied() {
    let _serial = TEST_SERIAL.lock().await;
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

    let config = build_config(&pool, drivers()).await.unwrap();
    let (collector, _broker) = start_collector(
        &pool,
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
        wait_until(Duration::from_secs(10), || async {
            current.get("tag:1").map(|s| s.value) == Some(Some(1234.0))
        })
        .await,
        "t1 should read its raw value"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    collector.stop().await.unwrap();
    sim.stop();

    let rows = read_all_group_rows(&env.data_dir()).await;
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

/// T9-1 / R2b-3a-3 (docs/banto-hub-driver-io-design.md §8): a
/// `simulation = true` connection whose real host can never answer
/// (`203.0.113.1`, TEST-NET-3, RFC 5737) still collects live, changing
/// values - proof that [`BrokerSessions`] started an in-process simulator,
/// dialed the broker session at it, and that its ramp generator is running.
/// Simulated values are shown but never recorded (no row in the store).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_simulated_connection_reads_a_simulator_through_the_broker() {
    let _serial = TEST_SERIAL.lock().await;
    let env = TempEnv::new("simulated");
    let pool = open_registry(&env).await;
    let mut input = conn_input("PLC1", 502);
    input.host = "203.0.113.1".to_string();
    input.simulation = true;
    let conn = PlcConnectionService::new(pool.clone())
        .create(input)
        .await
        .unwrap();
    let group = CollectionGroupService::new(pool.clone())
        .create(group_input("G1", conn.id, 100))
        .await
        .unwrap();
    let tag = TagService::new(pool.clone())
        .create(tag_input("t1", group.id, "40001", "i16"))
        .await
        .unwrap();

    let config = build_config(&pool, drivers()).await.unwrap();
    let (collector, broker) = start_collector(
        &pool,
        config,
        &env.data_dir(),
        Arc::new(SystemClock),
        EventSink::new(pool.clone()),
        fast_options(),
    )
    .await
    .unwrap();
    assert_eq!(broker.0.connection_count(), 1);

    let current = collector.current_values();
    let key = format!("tag:{}", tag.id);
    let first = Arc::new(std::sync::Mutex::new(None::<f64>));
    assert!(
        wait_until(Duration::from_secs(10), || async {
            let sample = current.get(&key);
            match sample {
                Some(s) if s.quality == Quality::Good => {
                    *first.lock().unwrap() = s.value;
                    s.value.is_some()
                }
                _ => false,
            }
        })
        .await,
        "no Good reading - the broker session never reached the simulator"
    );
    let first_value = first.lock().unwrap().unwrap();
    assert!(
        wait_until(Duration::from_secs(10), || async {
            current
                .get(&key)
                .and_then(|s| s.value)
                .is_some_and(|v| v != first_value)
        })
        .await,
        "the simulated register never changed - the ramp generator is not running"
    );

    collector.stop().await.unwrap();
    broker.0.shutdown().await;
    assert!(
        read_all_group_rows(&env.data_dir())
            .await
            .iter()
            .all(|row| row.values.iter().all(Option::is_none)),
        "a simulated value must never be recorded"
    );
}

/// #334 regression: `plc_connections.word_order` was ignored on the
/// collection-polling path even after #333 wired it into
/// `banto_collect::config`'s `ModbusTcpConfig`/`SlmpConfig` builders. The
/// missing link was `crate::task::ClientSpec` - it had no `word_order`
/// field, so `crate::task::client_spec` silently dropped the value and
/// `crate::task::default_client_factory` rebuilt the polled client with
/// `..ModbusTcpConfig::default()`, i.e. always `WordOrder::HighLow`,
/// regardless of what the connection was registered with. Read-on-demand
/// (the `banto_broker` path) reads `ProtocolConfig` directly and was never
/// affected, so the two paths silently disagreed on the same tag's value.
/// Since R2b-3a-3 the polling path reads through the broker too, with the
/// connection's own settings - there is no copy left to drop a field; this
/// keeps pinning the result end to end.
///
/// This pins the *polling* path end to end, against a real simulator socket:
/// an f32 tag and an f64 tag (#325, 4 registers) on a `word_order =
/// "low_high"` connection must decode correctly, and the exact same wire
/// bits on a `"high_low"` connection - reading the identical registers -
/// must decode to a different, specific (and wrong, for this data) value.
/// That contrast is the part a naive "does it decode to *something*" test
/// would miss: before the fix, both connections above decoded via
/// `WordOrder::HighLow` (the hardcoded default), so a test that only
/// exercised `"low_high"` without a `"high_low"` counter-example could pass
/// by coincidence if the fix were reverted to a value that happened to still
/// look plausible.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn word_order_low_high_is_honored_by_the_polling_path() {
    let _serial = TEST_SERIAL.lock().await;
    let env = TempEnv::new("word-order");

    // 85.0f32 = 0x42AA0000. A "low_high" device puts the low register of the
    // pair first on the wire, so the correct byte-for-byte placement here is
    // [low, high] = [0x0000, 0x42AA].
    let f32_regs_low_high: [u16; 2] = [0x0000, 0x42AA];
    // 50.0f64 = 0x4049000000000000, as 4 registers [msw..lsw] = [0x4049,
    // 0x0000, 0x0000, 0x0000]. "low_high" reverses the whole 4-register
    // group (see `banto_plc::decode::WordOrder`'s doc comment), so the wire
    // placement is [0x0000, 0x0000, 0x0000, 0x4049].
    let f64_regs_low_high: [u16; 4] = [0x0000, 0x0000, 0x0000, 0x4049];

    // Two independent PLCs (independent sockets/registries), holding the
    // exact same register bytes, one registered "low_high" and one
    // "high_low" - proving the *setting*, not the wire data, drives the
    // decoded value.
    let sim_lh = Simulator::start().await;
    sim_lh.set_holding_registers(0, &f32_regs_low_high); // 40001-40002
    sim_lh.set_holding_registers(2, &f64_regs_low_high); // 40003-40006

    let sim_hl = Simulator::start().await;
    sim_hl.set_holding_registers(0, &f32_regs_low_high);
    sim_hl.set_holding_registers(2, &f64_regs_low_high);

    let pool = open_registry(&env).await;
    let tag_svc = TagService::new(pool.clone());

    let conn_lh = PlcConnectionService::new(pool.clone())
        .create(conn_input_with_word_order(
            "PLC-LH",
            sim_lh.addr.port(),
            "low_high",
        ))
        .await
        .unwrap();
    let group_lh = CollectionGroupService::new(pool.clone())
        .create(group_input("G-LH", conn_lh.id, 100))
        .await
        .unwrap();
    let tag_f32_lh = tag_svc
        .create(tag_input("f32_lh", group_lh.id, "40001", "f32"))
        .await
        .unwrap();
    let tag_f64_lh = tag_svc
        .create(tag_input("f64_lh", group_lh.id, "40003", "f64"))
        .await
        .unwrap();

    let conn_hl = PlcConnectionService::new(pool.clone())
        .create(conn_input_with_word_order(
            "PLC-HL",
            sim_hl.addr.port(),
            "high_low",
        ))
        .await
        .unwrap();
    let group_hl = CollectionGroupService::new(pool.clone())
        .create(group_input("G-HL", conn_hl.id, 100))
        .await
        .unwrap();
    let tag_f32_hl = tag_svc
        .create(tag_input("f32_hl", group_hl.id, "40001", "f32"))
        .await
        .unwrap();
    let tag_f64_hl = tag_svc
        .create(tag_input("f64_hl", group_hl.id, "40003", "f64"))
        .await
        .unwrap();

    let config = build_config(&pool, drivers()).await.unwrap();
    let (collector, _broker) = start_collector(
        &pool,
        config,
        &env.data_dir(),
        Arc::new(SystemClock),
        EventSink::new(pool.clone()),
        fast_options(),
    )
    .await
    .unwrap();

    let current = collector.current_values();
    let f32_lh_key = format!("tag:{}", tag_f32_lh.id);
    let f64_lh_key = format!("tag:{}", tag_f64_lh.id);
    let f32_hl_key = format!("tag:{}", tag_f32_hl.id);
    let f64_hl_key = format!("tag:{}", tag_f64_hl.id);

    assert!(
        wait_until(Duration::from_secs(10), || async {
            current.get(&f32_lh_key).map(|s| s.value) == Some(Some(85.0))
        })
        .await,
        "f32 tag on the low_high connection should decode to 85.0"
    );
    assert!(
        wait_until(Duration::from_secs(10), || async {
            current.get(&f64_lh_key).map(|s| s.value) == Some(Some(50.0))
        })
        .await,
        "f64 tag on the low_high connection should decode to 50.0"
    );

    // Same wire bits, decoded high_low: `combine_u32`/`combine_u64` read the
    // register group in the opposite order, so this must NOT be 85.0/50.0 -
    // it lands on a specific denormal value computed straight from the same
    // bits, deterministically.
    let wrong_f32 =
        f32::from_bits(u32::from(f32_regs_low_high[0]) << 16 | u32::from(f32_regs_low_high[1]))
            as f64;
    let wrong_f64 = f64::from_bits(
        (u64::from(f64_regs_low_high[0]) << 48)
            | (u64::from(f64_regs_low_high[1]) << 32)
            | (u64::from(f64_regs_low_high[2]) << 16)
            | u64::from(f64_regs_low_high[3]),
    );
    assert_ne!(
        wrong_f32, 85.0,
        "sanity: the contrast value must actually differ"
    );
    assert_ne!(
        wrong_f64, 50.0,
        "sanity: the contrast value must actually differ"
    );

    assert!(
        wait_until(Duration::from_secs(10), || async {
            current.get(&f32_hl_key).map(|s| s.value) == Some(Some(wrong_f32))
        })
        .await,
        "f32 tag on the high_low connection must decode to {wrong_f32} (not 85.0) - \
         proving word_order actually changes what the polling path decodes"
    );
    assert!(
        wait_until(Duration::from_secs(10), || async {
            current.get(&f64_hl_key).map(|s| s.value) == Some(Some(wrong_f64))
        })
        .await,
        "f64 tag on the high_low connection must decode to {wrong_f64} (not 50.0)"
    );

    collector.stop().await.unwrap();
    sim_lh.stop();
    sim_hl.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bit_tags_record_zero_or_one() {
    let _serial = TEST_SERIAL.lock().await;
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

    let config = build_config(&pool, drivers()).await.unwrap();
    let (collector, _broker) = start_collector(
        &pool,
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
        wait_until(Duration::from_secs(10), || async {
            current.get("tag:1").map(|s| s.value) == Some(Some(1.0))
        })
        .await,
        "coil true should read 1.0"
    );
    collector.stop().await.unwrap();
    sim.stop();

    let rows = read_all_group_rows(&env.data_dir()).await;
    assert!(
        rows.iter().any(|r| r.values[0] == Some(1.0)),
        "coil true should record 1.0"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lifecycle_events_are_emitted_and_persisted() {
    let _serial = TEST_SERIAL.lock().await;
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

    let config = build_config(&pool, drivers()).await.unwrap();
    let events = EventSink::new(pool.clone());
    let mut rx = events.subscribe();
    let (collector, _broker) = start_collector(
        &pool,
        config,
        &env.data_dir(),
        Arc::new(SystemClock),
        events,
        fast_options(),
    )
    .await
    .unwrap();

    assert!(
        wait_until(Duration::from_secs(10), || async {
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
    let _serial = TEST_SERIAL.lock().await;
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

    let config = build_config(&pool, drivers()).await.unwrap();
    let conn_key = format!("conn:{}", conn.id);
    let (collector, _broker) = start_collector(
        &pool,
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
        wait_until(Duration::from_secs(10), || async {
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
        wait_until(Duration::from_secs(10), || async {
            current.get("tag:1").map(|s| s.quality) == Some(Quality::Bad)
        })
        .await,
        "cache should go Bad while disconnected"
    );

    // Give it a few more disconnected ticks, then stop and inspect the rows.
    tokio::time::sleep(Duration::from_millis(400)).await;
    collector.stop().await.unwrap();

    let rows = read_all_group_rows(&env.data_dir()).await;
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
    let _serial = TEST_SERIAL.lock().await;
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

    let config = build_config(&pool, drivers()).await.unwrap();
    let (collector, _broker) = start_collector(
        &pool,
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
        wait_until(Duration::from_secs(10), || async {
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
    // #344 through the broker (R2b-3a-3, ChronoGazer's path): one outage is
    // exactly one `plc_disconnected` and one `plc_reconnected` - the reads
    // that fail while the broker is reconnecting, and the connect attempts
    // that wait out its grace, report nothing twice.
    assert_eq!(count_events(&pool, "plc_disconnected").await, 1);
    assert_eq!(count_events(&pool, "plc_reconnected").await, 1);
}

// ---------------------------------------------------------------------------
// SLMP (I8, 2026-08-05: banto-collect の SLMP 対応)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slmp_collects_values_and_writes_to_tstore() {
    let _serial = TEST_SERIAL.lock().await;
    let env = TempEnv::new("slmp-values");
    let sim = SlmpSimulator::start().await;
    sim.set_word(SlmpDevice::D, 100, 4321);

    let pool = open_registry(&env).await;
    let conn = PlcConnectionService::new(pool.clone())
        .create(slmp_conn_input("PLC1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(pool.clone())
        .create(group_input("G1", conn.id, 100))
        .await
        .unwrap();
    TagService::new(pool.clone())
        .create(tag_input("t1", group.id, "D100", "i16"))
        .await
        .unwrap();

    let config = build_config(&pool, drivers()).await.unwrap();
    let (collector, _broker) = start_collector(
        &pool,
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
        wait_until(Duration::from_secs(10), || async {
            current.get("tag:1").map(|s| s.value) == Some(Some(4321.0))
        })
        .await,
        "t1 should read the MELSEC device value via SLMP"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    collector.stop().await.unwrap();
    sim.stop();

    let rows = read_all_group_rows(&env.data_dir()).await;
    assert!(
        rows.iter().any(|r| r.values[0] == Some(4321.0)),
        "tstore should have recorded the SLMP-read value"
    );
}

/// PLC 断 (`sim.stop()` - the SLMP twin of
/// `hard_drop_keeps_appending_null_rows_and_marks_reconnecting`): severing
/// every open session and closing the listener must flip the connection to
/// Reconnecting, keep ticking (all-NULL rows), and drive the cache Bad -
/// without tearing the collection loop down.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slmp_hard_drop_keeps_appending_null_rows_and_marks_reconnecting() {
    let _serial = TEST_SERIAL.lock().await;
    let env = TempEnv::new("slmp-drop");
    let sim = SlmpSimulator::start().await;
    sim.set_word(SlmpDevice::D, 0, 7);

    let pool = open_registry(&env).await;
    let conn = PlcConnectionService::new(pool.clone())
        .create(slmp_conn_input("PLC1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(pool.clone())
        .create(group_input("G1", conn.id, 100))
        .await
        .unwrap();
    TagService::new(pool.clone())
        .create(tag_input("t1", group.id, "D0", "i16"))
        .await
        .unwrap();

    let config = build_config(&pool, drivers()).await.unwrap();
    let conn_key = format!("conn:{}", conn.id);
    let (collector, _broker) = start_collector(
        &pool,
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
        wait_until(Duration::from_secs(10), || async {
            current.get("tag:1").map(|s| s.value) == Some(Some(7.0))
        })
        .await,
        "should read a real value over SLMP before the drop"
    );

    // Kill the PLC entirely (listener + live sessions severed).
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
        "expected Reconnecting status while the SLMP PLC is down"
    );
    assert!(
        wait_until(Duration::from_secs(10), || async {
            current.get("tag:1").map(|s| s.quality) == Some(Quality::Bad)
        })
        .await,
        "cache should go Bad while disconnected"
    );

    tokio::time::sleep(Duration::from_millis(400)).await;
    collector.stop().await.unwrap();

    let rows = read_all_group_rows(&env.data_dir()).await;
    assert!(
        rows.iter().any(|r| r.values[0] == Some(7.0)),
        "should have real values from before the drop"
    );
    assert!(
        rows.iter().any(|r| r.values[0].is_none()),
        "should have NULL rows recorded while disconnected"
    );
}

/// 復旧 (the SLMP twin of `auto_reconnects_and_values_resume_after_recovery`):
/// an unresponsive PLC (`sim.hang()`) times out the in-flight read
/// (connection-fatal for SLMP too, same as Modbus), and the backoff loop
/// must notice on its own and resume Good reads once the CPU answers again -
/// no outside intervention, same `Collector`/socket the whole time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slmp_auto_reconnects_and_values_resume_after_recovery() {
    let _serial = TEST_SERIAL.lock().await;
    let env = TempEnv::new("slmp-reconnect");
    let sim = SlmpSimulator::start().await;
    sim.set_word(SlmpDevice::D, 0, 55);
    let pool = open_registry(&env).await;
    let conn = PlcConnectionService::new(pool.clone())
        .create(slmp_conn_input("PLC1", sim.addr.port()))
        .await
        .unwrap();
    let group = CollectionGroupService::new(pool.clone())
        .create(group_input("G1", conn.id, 100))
        .await
        .unwrap();
    TagService::new(pool.clone())
        .create(tag_input("t1", group.id, "D0", "i16"))
        .await
        .unwrap();

    let config = build_config(&pool, drivers()).await.unwrap();
    let (collector, _broker) = start_collector(
        &pool,
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
        wait_until(Duration::from_secs(10), || async {
            current.get("tag:1").map(|s| s.value) == Some(Some(55.0))
        })
        .await,
        "should read Good initially over SLMP"
    );

    // Make the PLC unresponsive: reads time out (connection-fatal) but the
    // listener stays up.
    sim.hang();
    assert!(
        wait_until(Duration::from_secs(5), || async {
            count_events(&pool, "plc_disconnected").await >= 1
        })
        .await,
        "hang should cause a disconnect over SLMP"
    );

    // Recover: the PLC answers again.
    sim.stop_hanging();
    assert!(
        wait_until(Duration::from_secs(5), || async {
            count_events(&pool, "plc_reconnected").await >= 1
        })
        .await,
        "expected plc_reconnected after SLMP recovery"
    );
    assert!(
        wait_until(Duration::from_secs(5), || async {
            current.get("tag:1").map(|s| (s.value, s.quality)) == Some((Some(55.0), Quality::Good))
        })
        .await,
        "values should resume Good after SLMP reconnect"
    );

    collector.stop().await.unwrap();
    sim.stop();
}

// ---------------------------------------------------------------------------
// Thresholds
// ---------------------------------------------------------------------------

/// #532 / #533: the tag registry has no thresholds; a caller that judges
/// them (ChronoGazer) supplies them per tag id. Builds the config the way
/// such a caller does.
async fn config_with_thresholds(
    pool: &SqlitePool,
    tag_id: i64,
    limits: Thresholds,
) -> CollectorConfig {
    let snapshot = RegistrySnapshot::load(pool).await.unwrap();
    let (config, exclusions) = build_config_lenient_with_thresholds(
        &snapshot,
        &std::collections::HashMap::from([(tag_id, limits)]),
        HubSource::Unavailable,
        drivers(),
    );
    assert!(exclusions.is_empty(), "{exclusions:?}");
    config
}

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
    let _serial = TEST_SERIAL.lock().await;
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
    let t = TagService::new(pool.clone())
        .create(tag_input("t1", group.id, "40001", "i16"))
        .await
        .unwrap();

    let config = config_with_thresholds(
        &pool,
        t.id,
        Thresholds {
            ll: Some(5.0),
            l: Some(20.0),
            h: Some(50.0),
            hh: Some(90.0),
        },
    )
    .await;
    let (collector, _broker) = start_collector(
        &pool,
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
        wait_until(Duration::from_secs(10), || async {
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
        wait_until(Duration::from_secs(10), || async {
            entered_levels(&pool).await.contains(&"H".to_string())
        })
        .await,
        "expected threshold_entered at H"
    );

    // Escalate into HH: clears H, enters HH (one edge, one pair of events).
    sim.set_holding_register(0, 95);
    assert!(
        wait_until(Duration::from_secs(10), || async {
            entered_levels(&pool).await.contains(&"HH".to_string())
        })
        .await,
        "expected threshold_entered at HH"
    );

    // Return to normal: clears HH.
    sim.set_holding_register(0, 30);
    assert!(
        wait_until(Duration::from_secs(10), || async {
            count_events(&pool, "threshold_cleared").await >= 2
        })
        .await,
        "expected clears for the H->HH and HH->normal edges"
    );

    // Dip below L to prove the low side works too.
    sim.set_holding_register(0, 10);
    assert!(
        wait_until(Duration::from_secs(10), || async {
            entered_levels(&pool).await.contains(&"L".to_string())
        })
        .await,
        "expected threshold_entered at L"
    );

    collector.stop().await.unwrap();
    sim.stop();
}

/// #533（2026-10-08 オーナー決定「Hub はしきい値を持たず、警報を判定しない」）:
/// banto-hub の組み立て（`build_config` = `RegistrySnapshot::load` +
/// `build_config_from`、Hub の `hub.rs` と同じ関数）で収集すると、
/// **しきい値イベントは 1 件も出ない** - migration 0018 より前の DB で
/// `tags.threshold_*` に値が入っていたタグでも。
///
/// banto-tags の migration を 0017 まで流した DB（列がまだある）に H = 50 /
/// HH = 90 のタグを入れ、`banto_tags::migrate` で 0018 を当ててから、その H/HH を
/// 越える値（100）を何周期も読ませる。値は収集される（収集そのものは止まって
/// いない）が、`threshold_entered` / `threshold_cleared` は 0 件のまま。
///
/// 反証（2026-10-09 実施）: banto-collect の `ThresholdSource::None` が
/// 固定の limits（`h: Some(50.0)`）を返すようにすると、`threshold_entered` が
/// 記録されてこのテストが落ちる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hub_build_raises_no_threshold_event_even_for_pre_0018_threshold_columns() {
    let _serial = TEST_SERIAL.lock().await;
    let env = TempEnv::new("hub-no-threshold");
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 100); // 旧 H(50)・HH(90) を越える値

    let pool = banto_storage::connect_sqlite(env.registry_path())
        .await
        .expect("connect registry");
    // 0017 まで（しきい値の列がまだある形）。
    let mut migrator = sqlx::migrate::Migrator::new(std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../banto-tags/migrations"
    )))
    .await
    .expect("load banto-tags migrations");
    migrator.dangerous_set_table_name(banto_tags::MIGRATIONS_TABLE);
    migrator.run_to(17, &pool).await.expect("migrate to 0017");
    sqlx::query(
        "INSERT INTO plc_connections (id, name, protocol, host, port, unit_id, enabled, \
         simulation, word_order) VALUES (1, 'PLC1', 'modbus-tcp', '127.0.0.1', ?, 1, 1, 0, \
         'high_low')",
    )
    .bind(i64::from(sim.addr.port()))
    .execute(&pool)
    .await
    .expect("seed connection");
    sqlx::query(
        "INSERT INTO collection_groups (id, name, plc_connection_id, period_ms, enabled) \
         VALUES (1, 'G1', 1, 100, 1)",
    )
    .execute(&pool)
    .await
    .expect("seed group");
    sqlx::query(
        "INSERT INTO tags (id, name, collection_group_id, address, data_type, \
         threshold_h, threshold_hh, threshold_l, threshold_ll, enabled) \
         VALUES (1, 't1', 1, '40001', 'i16', 50, 90, 20, 5, 1)",
    )
    .execute(&pool)
    .await
    .expect("seed tag with thresholds in the old columns");

    // 0018 以降（列を落とす）と collect_events。
    banto_tags::migrate(&pool).await.expect("tags migrate");
    banto_collect::migrate(&pool)
        .await
        .expect("collect migrate");

    let config = build_config(&pool, drivers()).await.unwrap();
    let (collector, _broker) = start_collector(
        &pool,
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
        wait_until(Duration::from_secs(10), || async {
            current.get("tag:1").map(|s| s.value) == Some(Some(100.0))
        })
        .await,
        "the tag must still be collected"
    );
    // 何周期か読ませてから止める（イベントの書き込みは stop で流し切る）。
    tokio::time::sleep(Duration::from_millis(500)).await;
    collector.stop().await.unwrap();
    sim.stop();

    assert_eq!(count_events(&pool, "collection_started").await, 1);
    assert_eq!(
        count_events(&pool, "threshold_entered").await,
        0,
        "banto-hub must not judge thresholds"
    );
    assert_eq!(count_events(&pool, "threshold_cleared").await, 0);
}

// ---------------------------------------------------------------------------
// Current-value cache quality transitions
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn current_value_quality_transitions_good_bad_stale() {
    let _serial = TEST_SERIAL.lock().await;
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

    let config = build_config(&pool, drivers()).await.unwrap();
    // ManualClock so staleness can be forced deterministically after stop.
    // (Frozen "now" makes every append share one ptime; `banto-tstore`'s
    // upsert - owner decision 2026-08-08, docs/improvement-plan.md H4 -
    // resolves that by letting the newest write for that ptime replace the
    // stored row rather than rejecting it, so this is harmless here - this
    // test only inspects the in-memory cache, not the stored row count.)
    let clock = Arc::new(ManualClock::new(1_760_000_000_000, 0));
    let (collector, _broker) = start_collector(
        &pool,
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
        wait_until(Duration::from_secs(10), || async {
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
// Clock regression (H4, 2026-08-08 owner decision, docs/improvement-plan.md):
// a backward jump of the injected clock must be reported exactly once as it
// happens (not once per regressed tick) and exactly once more on recovery -
// and the regressed interval's data must survive as an *overwrite* of the
// pre-regression row (`banto-tstore`'s upsert, the storage half of this same
// decision), never a duplicate or a silently dropped append.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clock_regression_emits_edge_events_and_overwrites_the_colliding_row() {
    let _serial = TEST_SERIAL.lock().await;
    let env = TempEnv::new("clock-regression");
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 1); // pre-regression value
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

    let config = build_config(&pool, drivers()).await.unwrap();
    let base_ms: i64 = 1_780_000_000_000;
    // ManualClock so the regression/recovery can be driven deterministically
    // by the test rather than waiting for real wall-clock drift - scheduling
    // (when a tick fires) still runs on real tokio timers regardless (see
    // `task.rs`'s module doc); only the *stamped* `ptime_ms` comes from this
    // clock, which is exactly what H4's detection watches.
    let clock = Arc::new(ManualClock::new(base_ms, 0));
    let (collector, _broker) = start_collector(
        &pool,
        config,
        &env.data_dir(),
        clock.clone(),
        EventSink::new(pool.clone()),
        fast_options(),
    )
    .await
    .unwrap();
    let current = collector.current_values();

    // Phase 1: clock parked at base_ms - let a real tick land there and
    // durably flush (fast_options() flushes on every append).
    assert!(
        wait_until(Duration::from_secs(10), || async {
            current.get("tag:1").map(|s| s.ptime_ms) == Some(base_ms)
        })
        .await,
        "expected a tick recorded at base_ms before advancing the clock"
    );
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Advance forward - this becomes the pre-regression high-water mark.
    let ahead_ms = base_ms + 5_000;
    clock.set_now_ms(ahead_ms);
    assert!(
        wait_until(Duration::from_secs(10), || async {
            current.get("tag:1").map(|s| s.ptime_ms) == Some(ahead_ms)
        })
        .await,
        "expected a tick recorded at ahead_ms"
    );

    // Regress: jump back to base_ms, colliding with the very first row -
    // must be reported exactly once as it happens.
    sim.set_holding_register(0, 99); // the value the overwrite must carry
    clock.set_now_ms(base_ms);
    assert!(
        wait_until(Duration::from_secs(10), || async {
            count_events(&pool, "clock_regression_entered").await >= 1
        })
        .await,
        "expected a clock_regression_entered event"
    );
    // Let a few more regressed-clock ticks land (all still at ptime =
    // base_ms) so the overwrite with register value 99 is durable.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Recover: advance past the prior high-water mark.
    clock.set_now_ms(ahead_ms + 1_000);
    assert!(
        wait_until(Duration::from_secs(10), || async {
            count_events(&pool, "clock_regression_cleared").await >= 1
        })
        .await,
        "expected a clock_regression_cleared event"
    );
    tokio::time::sleep(Duration::from_millis(150)).await;

    collector.stop().await.unwrap();
    sim.stop();

    // Episode edges only - never one per regressed tick, no matter how many
    // ticks actually happened while regressed.
    assert_eq!(
        count_events(&pool, "clock_regression_entered").await,
        1,
        "the regression must be reported exactly once"
    );
    assert_eq!(
        count_events(&pool, "clock_regression_cleared").await,
        1,
        "the recovery must be reported exactly once"
    );

    // The row at base_ms must reflect the OVERWRITE (99), not the original
    // pre-regression value (1) - proof the regressed interval's data
    // survives via last-write-wins, never a rejected or duplicated append.
    let rows = read_all_group_rows(&env.data_dir()).await;
    assert_eq!(
        rows.iter().filter(|r| r.ptime_ms == base_ms).count(),
        1,
        "the ptime collision must resolve to exactly one row, not a duplicate"
    );
    let row_at_base = rows
        .iter()
        .find(|r| r.ptime_ms == base_ms)
        .expect("a row at base_ms must exist");
    assert_eq!(
        row_at_base.values[0],
        Some(99.0),
        "the regressed-interval write must have overwritten the pre-regression row"
    );
}

// ---------------------------------------------------------------------------
// Stop guarantees
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_flushes_buffered_rows_so_none_are_lost() {
    let _serial = TEST_SERIAL.lock().await;
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

    let config = build_config(&pool, drivers()).await.unwrap();
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
    let (collector, _broker) = start_collector(
        &pool,
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
        wait_until(Duration::from_secs(10), || async {
            current.get("tag:1").map(|s| s.value) == Some(Some(3.0))
        })
        .await
    );
    tokio::time::sleep(Duration::from_millis(400)).await;
    collector.stop().await.unwrap();
    sim.stop();

    let rows = read_all_group_rows(&env.data_dir()).await;
    assert!(
        rows.len() >= 3,
        "buffered rows must survive stop(); got {}",
        rows.len()
    );
    assert!(rows.iter().any(|r| r.values[0] == Some(3.0)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_reports_connected_while_running_and_stop_completes_cleanly() {
    let _serial = TEST_SERIAL.lock().await;
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

    let config = build_config(&pool, drivers()).await.unwrap();
    let conn_key = format!("conn:{}", conn.id);
    let (collector, _broker) = start_collector(
        &pool,
        config,
        &env.data_dir(),
        Arc::new(SystemClock),
        EventSink::new(pool.clone()),
        fast_options(),
    )
    .await
    .unwrap();

    assert!(
        wait_until(Duration::from_secs(10), || async {
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

/// P0-3 #5 (docs/banto-hub-driver-refactor-plan.md §8.3): one connection = one
/// socket at the collection engine level. Mirrors banto-hub's #337 test
/// (`apps/banto-hub/core/tests/integration.rs`
/// `modbus_collection_uses_exactly_one_socket`), minus the Hub: a collector
/// that is actually reading values (so "zero sockets" cannot pass) and keeps
/// polling for several periods must have made exactly one accept on the
/// simulator - a second socket, even an immediately-closed one, is counted.
/// The test changes the simulator value several times and waits for each new
/// Good value, so a collector that stops after the first poll cannot pass.
/// Some real Modbus/TCP servers (Omron KM-D1-ETN) take a single client only.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn collection_over_several_periods_uses_exactly_one_socket() {
    let _serial = TEST_SERIAL.lock().await;
    let env = TempEnv::new("one-socket");
    let sim = Simulator::start().await;
    sim.set_holding_register(0, 4321); // 40001
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

    let config = build_config(&pool, drivers()).await.unwrap();
    let (collector, _broker) = start_collector(
        &pool,
        config,
        &env.data_dir(),
        Arc::new(SystemClock),
        EventSink::new(pool.clone()),
        fast_options(),
    )
    .await
    .unwrap();

    // Collection is really happening: a Good value has been read.
    let current = collector.current_values();
    assert!(
        wait_until(Duration::from_secs(10), || async {
            current.get("tag:1").map(|s| (s.value, s.quality))
                == Some((Some(4321.0), Quality::Good))
        })
        .await,
        "collector should read the simulator value as Good"
    );

    // Change the simulator value several times and wait for each new value to
    // arrive as Good. The current-values cache keeps the last value and only
    // ages to Stale, so re-checking an unchanged value cannot prove polling
    // continued; a collector that stops after the first poll cannot pass
    // this. A reconnect-per-tick or a second (e.g. status-probe) socket would
    // show up as an extra accept below.
    for v in [1111u16, 2222, 3333] {
        sim.set_holding_register(0, v);
        assert!(
            wait_until(Duration::from_secs(10), || async {
                current.get("tag:1").map(|s| (s.value, s.quality))
                    == Some((Some(f64::from(v)), Quality::Good))
            })
            .await,
            "collector should keep polling and read the updated value {v} as Good"
        );
    }
    assert_eq!(
        sim.connection_count(),
        1,
        "a collecting connection must occupy exactly one socket"
    );

    collector.stop().await.unwrap();
    sim.stop();
}

/// #588: a TCP listener that accepts every connection and never answers -
/// a PLC that stopped responding. Every accepted socket is kept open (and
/// never read) for the listener task's life; returns its port.
async fn unresponsive_plc() -> (u16, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind an unresponsive PLC");
    let port = listener.local_addr().expect("local addr").port();
    let handle = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((socket, _)) = listener.accept().await {
            held.push(socket);
        }
    });
    (port, handle)
}

/// #588: start `n` Modbus connections against [`unresponsive_plc`] with the
/// product's default timeouts (1 s response), let every task get into a read
/// that will never be answered, and time `stop()`. Rows are only buffered
/// (never flushed on interval or row count), so the per-group rows a
/// post-stop reader sees prove `stop()`'s final flush ran; the returned
/// count is how many of the `n` groups have at least one row.
async fn time_stop_with_unresponsive_plcs(label: &str, n: usize) -> (Duration, usize, i64) {
    let env = TempEnv::new(label);
    let (port, listener) = unresponsive_plc().await;
    let pool = open_registry(&env).await;
    for i in 0..n {
        let conn = PlcConnectionService::new(pool.clone())
            .create(conn_input(&format!("PLC{i}"), port))
            .await
            .unwrap();
        let group = CollectionGroupService::new(pool.clone())
            .create(group_input(&format!("G{i}"), conn.id, 100))
            .await
            .unwrap();
        TagService::new(pool.clone())
            .create(tag_input(&format!("t{i}"), group.id, "40001", "i16"))
            .await
            .unwrap();
    }
    let config = build_config(&pool, drivers()).await.unwrap();
    let options = CollectorOptions {
        writer_options: WriterOptions {
            max_buffered_rows: 100_000,
            flush_interval_ms: 3_600_000,
        },
        ..CollectorOptions::default()
    };
    let (collector, _broker) = start_collector(
        &pool,
        config,
        &env.data_dir(),
        Arc::new(SystemClock),
        EventSink::new(pool.clone()),
        options,
    )
    .await
    .unwrap();
    // Every connection connects (the listener accepts) and then waits on a
    // read that never comes back; let several reads time out and restart so
    // the tasks are spread over the 1 s response timeout, as in the field.
    assert!(
        wait_until(Duration::from_secs(20), || async {
            collector
                .status()
                .values()
                .filter(|s| matches!(s, ConnectionStatus::Connected))
                .count()
                == n
        })
        .await,
        "every connection should connect to the unresponsive listener"
    );
    tokio::time::sleep(Duration::from_millis(2_500)).await;

    let started = std::time::Instant::now();
    collector.stop().await.unwrap();
    let elapsed = started.elapsed();
    listener.abort();

    let mut groups_with_rows = 0;
    for file in banto_tstore::list_data_files(&env.data_dir()).expect("list files") {
        let reader = TsReader::open(&file.path).await.expect("open reader");
        let keys: Vec<String> = reader.groups().iter().map(|g| g.key.clone()).collect();
        for key in keys {
            let rows = reader.read_range(&key, 0, i64::MAX).await.expect("read");
            if !rows.is_empty() {
                groups_with_rows += 1;
            }
        }
    }
    let stopped = count_events(&pool, "collection_stopped").await;
    (elapsed, groups_with_rows, stopped)
}

/// **#588.** `stop()` with many connections whose PLC never answers must
/// take about one response timeout, not one per connection: every task is
/// signalled before the first join, so they all wind down at once. Before
/// the fix (signal and join one at a time) 20 such connections took several
/// seconds here and 4.7 s on real hardware (80 took 12.4 s) - past
/// banto-hub's 5 s OS-shutdown budget and ChronoGazer's 5 s exit budget.
/// The final flush and `collection_stopped` still happen.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_with_many_unresponsive_plcs_takes_about_one_timeout() {
    let _serial = TEST_SERIAL.lock().await;
    let (elapsed, groups_with_rows, stopped) =
        time_stop_with_unresponsive_plcs("stop-unresponsive", 20).await;
    assert!(
        elapsed < Duration::from_secs(3),
        "stop() with 20 unresponsive PLCs took {elapsed:?} - the tasks were \
         stopped one after another instead of together (#588)"
    );
    assert_eq!(
        groups_with_rows, 20,
        "every group's buffered rows must survive stop()'s final flush"
    );
    assert_eq!(stopped, 1, "collection_stopped must be recorded once");
}

// ---------------------------------------------------------------------------
// collect_events persistence round-trip
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn collect_events_rows_carry_the_full_shape() {
    let _serial = TEST_SERIAL.lock().await;
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
    let t = TagService::new(pool.clone())
        .create(tag_input("t1", group.id, "40001", "i16"))
        .await
        .unwrap();

    let config = config_with_thresholds(
        &pool,
        t.id,
        Thresholds {
            h: Some(50.0),
            ..Thresholds::default()
        },
    )
    .await;
    let (collector, _broker) = start_collector(
        &pool,
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
        wait_until(Duration::from_secs(10), || async {
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
    let _serial = TEST_SERIAL.lock().await;
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

    let config = build_config(&pool, drivers()).await.unwrap();
    assert_eq!(config.group_count(), 3);
    let (collector, _broker) = start_collector(
        &pool,
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
        // Lower bound is deliberately loose (H7 ⑤, 2026-08-08: >=2, i.e.
        // ~1/15 of the theoretical ~30): the scheduler is
        // MissedTickBehavior::Skip, so a busy CI runner can only ever LOSE
        // ticks, never burst extra rows. Both the earlier >=18 bound and the
        // subsequent >=10 (a third of theoretical) bound proved flaky on
        // real CI - severe oversubscription was observed to crater counts to
        // roughly 1/10 of theoretical (~3 here). >=2 sits clearly below that
        // worst-observed floor (with margin to spare) while still requiring
        // more than a single fluke row - this test's job is liveness, not
        // precise throughput (that's the #[ignore]d long soak's job below).
        // What this still catches is a collector that stalls outright (0-1
        // rows) or grinds to a crawl, while the upper bound still pins
        // "skip, don't burst". Tight timing guarantees are not CI's job here
        // (same convention as banto-plc's perf smokes: wall-clock numbers
        // are not a CI failure condition).
        assert!(
            rows.len() >= 2 && rows.len() <= 50,
            "group {} expected ~30 rows in 3s @100ms (>=2 liveness floor tolerated for \
             severely busy runners), got {}",
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
    let _serial = TEST_SERIAL.lock().await;
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

    let config = build_config(&pool, drivers()).await.unwrap();
    let (collector, _broker) = start_collector(
        &pool,
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

    let rows = read_all_group_rows(&env.data_dir()).await;
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

// ---------------------------------------------------------------------------
// T7-1 (docs/tag-server-design.md §4.3): online partial reconfiguration -
// `Collector::apply_config`. Every test below drives the real engine (real
// sockets, real `Collector::start`) and proves the core claim: the influence
// radius of a config change is exactly the connections that changed - an
// unchanged connection's task is never restarted and never loses a tick, no
// matter what else in the config changes around it.
// ---------------------------------------------------------------------------

async fn count_events_for_connection(pool: &SqlitePool, kind: &str, conn_key: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM collect_events WHERE kind = ? AND connection_key = ?")
        .bind(kind)
        .bind(conn_key)
        .fetch_one(pool)
        .await
        .unwrap_or(0)
}

/// One connection (A), one group, one `i16` tag on its own simulator -
/// already running under a `Collector`. The minimal fixture for the
/// "connection added" test.
struct OneConnEnv {
    pool: SqlitePool,
    sim_a: Simulator,
    group_a_id: i64,
    conn_a_key: String,
    tag_a_key: String,
    collector: Collector,
    broker: Broker,
    // Declared LAST: Rust drops struct fields in declaration order, and
    // `TempEnv::drop`'s `remove_dir_all` retry only has a chance once every
    // `SqlitePool` clone above (in particular `pool`) has actually been
    // dropped - putting `env` first was a measured, 100%-reproducing leak
    // (`pool` was still alive, still holding its registry file open, for
    // the entire retry window) - see `TempEnv`'s doc comment.
    env: TempEnv,
}

async fn one_conn_setup(label: &str) -> OneConnEnv {
    let env = TempEnv::new(label);
    let sim_a = Simulator::start().await;
    sim_a.set_holding_register(0, 11);

    let pool = open_registry(&env).await;
    let conn_a = PlcConnectionService::new(pool.clone())
        .create(conn_input("A", sim_a.addr.port()))
        .await
        .unwrap();
    let group_a = CollectionGroupService::new(pool.clone())
        .create(group_input("Ga", conn_a.id, 100))
        .await
        .unwrap();
    let tag_a = TagService::new(pool.clone())
        .create(tag_input("t1", group_a.id, "40001", "i16"))
        .await
        .unwrap();

    let config = build_config(&pool, drivers()).await.unwrap();
    let (collector, _broker) = start_collector(
        &pool,
        config,
        &env.data_dir(),
        Arc::new(SystemClock),
        EventSink::new(pool.clone()),
        fast_options(),
    )
    .await
    .unwrap();

    let current = collector.current_values();
    let tag_a_key = format!("tag:{}", tag_a.id);
    assert!(
        wait_until(Duration::from_secs(10), || async {
            current.get(&tag_a_key).map(|s| s.value) == Some(Some(11.0))
        })
        .await,
        "A should read its initial value before any apply_config"
    );

    OneConnEnv {
        env,
        pool,
        sim_a,
        group_a_id: group_a.id,
        conn_a_key: format!("conn:{}", conn_a.id),
        tag_a_key,
        collector,
        broker: _broker,
    }
}

/// Two independent connections (A/B), each one group with one `i16` tag on
/// its own in-process Modbus simulator - already running under a
/// `Collector`. The shared fixture for every T7-1 test that needs to prove
/// "A never notices what happens to B".
struct TwoConnEnv {
    pool: SqlitePool,
    sim_a: Simulator,
    sim_b: Simulator,
    group_a_id: i64,
    group_b_id: i64,
    conn_b_id: i64,
    tag_a_key: String,
    tag_b_key: String,
    conn_a_key: String,
    conn_b_key: String,
    collector: Collector,
    broker: Broker,
    // Declared LAST - see `OneConnEnv`'s `env` field comment.
    env: TempEnv,
}

async fn two_conn_setup(label: &str) -> TwoConnEnv {
    let env = TempEnv::new(label);
    let sim_a = Simulator::start().await;
    let sim_b = Simulator::start().await;
    sim_a.set_holding_register(0, 11);
    sim_b.set_holding_register(0, 22);

    let pool = open_registry(&env).await;
    let conn_a = PlcConnectionService::new(pool.clone())
        .create(conn_input("A", sim_a.addr.port()))
        .await
        .unwrap();
    let group_a = CollectionGroupService::new(pool.clone())
        .create(group_input("Ga", conn_a.id, 100))
        .await
        .unwrap();
    let tag_a = TagService::new(pool.clone())
        .create(tag_input("t1", group_a.id, "40001", "i16"))
        .await
        .unwrap();

    let conn_b = PlcConnectionService::new(pool.clone())
        .create(conn_input("B", sim_b.addr.port()))
        .await
        .unwrap();
    let group_b = CollectionGroupService::new(pool.clone())
        .create(group_input("Gb", conn_b.id, 100))
        .await
        .unwrap();
    let tag_b = TagService::new(pool.clone())
        // Tag names are unique registry-wide (not per-group - see
        // banto-tags's own validation), so this cannot reuse A's "t1".
        .create(tag_input("b1", group_b.id, "40001", "i16"))
        .await
        .unwrap();

    let config = build_config(&pool, drivers()).await.unwrap();
    let (collector, _broker) = start_collector(
        &pool,
        config,
        &env.data_dir(),
        Arc::new(SystemClock),
        EventSink::new(pool.clone()),
        fast_options(),
    )
    .await
    .unwrap();

    let current = collector.current_values();
    let tag_a_key = format!("tag:{}", tag_a.id);
    let tag_b_key = format!("tag:{}", tag_b.id);
    assert!(
        wait_until(Duration::from_secs(10), || async {
            current.get(&tag_a_key).map(|s| s.value) == Some(Some(11.0))
        })
        .await,
        "A should read its initial value before any apply_config"
    );
    assert!(
        wait_until(Duration::from_secs(10), || async {
            current.get(&tag_b_key).map(|s| s.value) == Some(Some(22.0))
        })
        .await,
        "B should read its initial value before any apply_config"
    );

    TwoConnEnv {
        env,
        pool,
        sim_a,
        sim_b,
        group_a_id: group_a.id,
        group_b_id: group_b.id,
        conn_b_id: conn_b.id,
        tag_a_key,
        tag_b_key,
        conn_a_key: format!("conn:{}", conn_a.id),
        conn_b_key: format!("conn:{}", conn_b.id),
        collector,
        broker: _broker,
    }
}

/// Test 1 (task instructions §テスト-1): "無停止の実証" - while A/B both run,
/// adding a tag to B's existing group must not touch A's task at all: no
/// `plc_connected`/`plc_disconnected` event for A, and A's cache keeps
/// advancing throughout. B ends up collecting the new tag too, and the
/// writer rotates (the aggregate collected tag set changed).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn apply_config_adding_a_tag_to_b_never_restarts_a() {
    let _serial = TEST_SERIAL.lock().await;
    let mut setup = two_conn_setup("apply-no-restart").await;
    let current = setup.collector.current_values();

    let a_connected_before =
        count_events_for_connection(&setup.pool, "plc_connected", &setup.conn_a_key).await;
    assert_eq!(
        a_connected_before, 1,
        "A should have connected exactly once so far"
    );
    let a_ptime_before = current.get(&setup.tag_a_key).unwrap().ptime_ms;

    // Add a second tag to B's existing group.
    setup.sim_b.set_holding_register(1, 99);
    let tag_b2 = TagService::new(setup.pool.clone())
        .create(tag_input("t2", setup.group_b_id, "40002", "i16"))
        .await
        .unwrap();
    let tag_b2_key = format!("tag:{}", tag_b2.id);

    let new_config = build_config(&setup.pool, drivers()).await.unwrap();
    let report = apply_through(&mut setup.collector, &setup.broker, &setup.pool, new_config)
        .await
        .expect("apply_config should succeed");

    assert!(
        report.writer_rotated,
        "the collected tag set changed, so the writer must rotate"
    );
    assert!(
        report.unchanged.contains(&setup.conn_a_key),
        "A's plan did not change, so it must be classified unchanged: {report:?}"
    );
    assert!(
        !report.removed.contains(&setup.conn_a_key) && !report.replaced.contains(&setup.conn_a_key),
        "A must never be stopped/replaced by a change scoped to B: {report:?}"
    );

    // A must keep ticking with no reconnect anywhere in its history.
    assert!(
        wait_until(Duration::from_secs(10), || async {
            current.get(&setup.tag_a_key).map(|s| s.ptime_ms) > Some(a_ptime_before)
        })
        .await,
        "A's cache should keep advancing after apply_config"
    );
    assert_eq!(
        count_events_for_connection(&setup.pool, "plc_connected", &setup.conn_a_key).await,
        a_connected_before,
        "A must not have reconnected - its task was never touched"
    );
    assert_eq!(
        count_events_for_connection(&setup.pool, "plc_disconnected", &setup.conn_a_key).await,
        0,
        "A must never have disconnected"
    );

    // B collects the new tag too.
    assert!(
        wait_until(Duration::from_secs(10), || async {
            current.get(&tag_b2_key).map(|s| s.value) == Some(Some(99.0))
        })
        .await,
        "B should collect its newly added tag"
    );

    setup.collector.stop().await.unwrap();
    setup.sim_a.stop();
    setup.sim_b.stop();
}

/// Test 2 (task instructions §テスト-2): adding a brand-new connection must
/// not disturb an already-running one, and the new connection starts
/// collecting with `report.added` naming it and the writer rotating (a whole
/// new group appeared).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn apply_config_adding_a_connection_leaves_the_existing_one_untouched() {
    let _serial = TEST_SERIAL.lock().await;
    let mut setup = one_conn_setup("apply-add-connection").await;
    let current = setup.collector.current_values();

    let a_connected_before =
        count_events_for_connection(&setup.pool, "plc_connected", &setup.conn_a_key).await;
    let a_ptime_before = current.get(&setup.tag_a_key).unwrap().ptime_ms;

    let sim_b = Simulator::start().await;
    sim_b.set_holding_register(0, 33);
    let conn_b = PlcConnectionService::new(setup.pool.clone())
        .create(conn_input("B", sim_b.addr.port()))
        .await
        .unwrap();
    let group_b = CollectionGroupService::new(setup.pool.clone())
        .create(group_input("Gb", conn_b.id, 100))
        .await
        .unwrap();
    let tag_b = TagService::new(setup.pool.clone())
        // Registry-wide unique tag name - A already owns "t1".
        .create(tag_input("b1", group_b.id, "40001", "i16"))
        .await
        .unwrap();
    let tag_b_key = format!("tag:{}", tag_b.id);
    let conn_b_key = format!("conn:{}", conn_b.id);

    let new_config = build_config(&setup.pool, drivers()).await.unwrap();
    let report = apply_through(&mut setup.collector, &setup.broker, &setup.pool, new_config)
        .await
        .expect("apply_config should succeed");

    assert!(report.writer_rotated, "a whole new group appeared");
    assert!(
        report.added.contains(&conn_b_key),
        "B should be classified added: {report:?}"
    );
    assert!(
        report.unchanged.contains(&setup.conn_a_key),
        "A should be classified unchanged: {report:?}"
    );

    assert!(
        wait_until(Duration::from_secs(10), || async {
            current.get(&setup.tag_a_key).map(|s| s.ptime_ms) > Some(a_ptime_before)
        })
        .await,
        "A should keep ticking"
    );
    assert_eq!(
        count_events_for_connection(&setup.pool, "plc_connected", &setup.conn_a_key).await,
        a_connected_before,
        "A must not have reconnected"
    );

    assert!(
        wait_until(Duration::from_secs(10), || async {
            current.get(&tag_b_key).map(|s| s.value) == Some(Some(33.0))
        })
        .await,
        "the newly added connection B should start collecting"
    );

    setup.collector.stop().await.unwrap();
    setup.sim_a.stop();
    sim_b.stop();
}

/// Test 3 (task instructions §テスト-3): disabling (removing from the
/// collected set) a connection must stop only its task and clean up after
/// it - `retain` removes its tag(s) from the current-value snapshot and its
/// key from the status map - while leaving the other connection untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn apply_config_removing_a_connection_retains_the_rest() {
    let _serial = TEST_SERIAL.lock().await;
    let mut setup = two_conn_setup("apply-remove-connection").await;
    let current = setup.collector.current_values();

    let a_connected_before =
        count_events_for_connection(&setup.pool, "plc_connected", &setup.conn_a_key).await;
    let a_ptime_before = current.get(&setup.tag_a_key).unwrap().ptime_ms;

    // Disable B - build_config excludes it exactly like a deletion would.
    let mut disabled = conn_input("B", 1); // port irrelevant once disabled
    disabled.enabled = false;
    PlcConnectionService::new(setup.pool.clone())
        .update(setup.conn_b_id, disabled)
        .await
        .unwrap();

    let new_config = build_config(&setup.pool, drivers()).await.unwrap();
    let report = apply_through(&mut setup.collector, &setup.broker, &setup.pool, new_config)
        .await
        .expect("apply_config should succeed");

    assert!(
        report.removed.contains(&setup.conn_b_key),
        "B should be classified removed: {report:?}"
    );
    assert!(
        report.unchanged.contains(&setup.conn_a_key),
        "A should be classified unchanged: {report:?}"
    );

    assert!(
        wait_until(Duration::from_secs(10), || async {
            current.get(&setup.tag_a_key).map(|s| s.ptime_ms) > Some(a_ptime_before)
        })
        .await,
        "A should keep ticking"
    );
    assert_eq!(
        count_events_for_connection(&setup.pool, "plc_connected", &setup.conn_a_key).await,
        a_connected_before,
        "A must not have reconnected"
    );

    // B's tag must be gone from the snapshot (retain) and B's key gone from
    // status (retain_status) - both checked a moment later so a
    // just-removed task has had time to actually join.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !current.snapshot().contains_key(&setup.tag_b_key),
        "B's tag must be retained out of the current-value cache"
    );
    assert!(
        !setup.collector.status().contains_key(&setup.conn_b_key),
        "B's connection must be retained out of the status map"
    );

    setup.collector.stop().await.unwrap();
    setup.sim_a.stop();
    setup.sim_b.stop();
}

/// Test 4 (task instructions §テスト-4): a settings-only edit to a
/// connection (its target port, standing in for "host changed" - same
/// `ProtocolConfig` field) must replace only that connection's task, with
/// **no** writer rotation (the collected tag/group set is byte-for-byte the
/// same), and must leave every other connection untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn apply_config_settings_only_change_does_not_rotate_the_writer() {
    let _serial = TEST_SERIAL.lock().await;
    let mut setup = two_conn_setup("apply-settings-only").await;
    let current = setup.collector.current_values();

    let a_connected_before =
        count_events_for_connection(&setup.pool, "plc_connected", &setup.conn_a_key).await;
    let a_ptime_before = current.get(&setup.tag_a_key).unwrap().ptime_ms;

    // Point B at a *different* simulator instance (same group/tag shape) -
    // the connection-settings-only edit this test is about.
    let sim_b2 = Simulator::start().await;
    sim_b2.set_holding_register(0, 77);
    let mut moved = conn_input("B", sim_b2.addr.port());
    moved.name = "B".to_string();
    PlcConnectionService::new(setup.pool.clone())
        .update(setup.conn_b_id, moved)
        .await
        .unwrap();

    let new_config = build_config(&setup.pool, drivers()).await.unwrap();
    let report = apply_through(&mut setup.collector, &setup.broker, &setup.pool, new_config)
        .await
        .expect("apply_config should succeed");

    assert!(
        !report.writer_rotated,
        "a host/port-only change must not rotate the writer: {report:?}"
    );
    assert!(
        report.replaced.contains(&setup.conn_b_key),
        "B's protocol config changed, so it must be classified replaced: {report:?}"
    );
    assert!(
        report.unchanged.contains(&setup.conn_a_key),
        "A should be classified unchanged: {report:?}"
    );

    assert!(
        wait_until(Duration::from_secs(10), || async {
            current.get(&setup.tag_a_key).map(|s| s.ptime_ms) > Some(a_ptime_before)
        })
        .await,
        "A should keep ticking"
    );
    assert_eq!(
        count_events_for_connection(&setup.pool, "plc_connected", &setup.conn_a_key).await,
        a_connected_before,
        "A must not have reconnected"
    );

    // B must actually be talking to the *new* target now.
    assert!(
        wait_until(Duration::from_secs(10), || async {
            current.get(&setup.tag_b_key).map(|s| s.value) == Some(Some(77.0))
        })
        .await,
        "B should now read from the new target simulator"
    );

    setup.collector.stop().await.unwrap();
    setup.sim_a.stop();
    setup.sim_b.stop();
    sim_b2.stop();
}

/// Test 5 (task instructions §テスト-5): if opening the rotated writer fails,
/// `apply_config` must return `Err` with absolutely nothing changed - no task
/// stopped, no config adopted, the old writer still the live one. Forced by
/// pre-creating a *directory* at the exact path the rotation would try to
/// open as a SQLite file (a type mismatch that fails regardless of process
/// privilege, unlike a permission-based sabotage).
///
/// 失敗した apply_config は何も触らない。再試行は、変更のあった A だけを
/// ちょうど 1 回置き換える（B は触らない）。
///
/// 経緯（2026-09-23）: 以前は再試行の後も「A の `plc_connected` は増えない」
/// と主張していたが、再試行の構成は A のグループに t2 を足しているので、
/// A は設計どおり `replaced` になり、新しいタスクが初回接続で
/// `plc_connected` を 1 回出す。そのアサーションが通っていたのは、
/// `apply_config` が返った直後に数えていて、新タスクの接続と記録が
/// まだ済んでいなかった（競争に勝っていた）だけだった。CI で 2 回、
/// ローカルで 30 回中 1 回、競争に負けて `left: 2 right: 1` で落ちた。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn apply_config_failed_rotation_touches_nothing_and_retry_replaces_only_a() {
    let _serial = TEST_SERIAL.lock().await;
    let mut setup = two_conn_setup("apply-writer-open-fail").await;
    let current = setup.collector.current_values();

    let a_connected_before =
        count_events_for_connection(&setup.pool, "plc_connected", &setup.conn_a_key).await;
    let b_connected_before =
        count_events_for_connection(&setup.pool, "plc_connected", &setup.conn_b_key).await;

    // Sabotage the exact path the next rotation would try to open.
    let files = banto_tstore::list_data_files(&setup.env.data_dir()).unwrap();
    assert_eq!(files.len(), 1, "exactly one file before any apply_config");
    let sabotage_name = format!(
        "{}-{:03}.sqlite3",
        files[0].date.to_yyyymmdd(),
        files[0].seq + 1
    );
    let sabotage_path = setup.env.data_dir().join(&sabotage_name);
    std::fs::create_dir(&sabotage_path).expect("create sabotage directory");

    // A change that requires a writer rotation (a new tag on A's group).
    setup.sim_a.set_holding_register(1, 55);
    let tag_a2 = TagService::new(setup.pool.clone())
        .create(tag_input("t2", setup.group_a_id, "40002", "i16"))
        .await
        .unwrap();
    let tag_a2_key = format!("tag:{}", tag_a2.id);
    let failing_config = build_config(&setup.pool, drivers()).await.unwrap();

    let err = apply_through(
        &mut setup.collector,
        &setup.broker,
        &setup.pool,
        failing_config,
    )
    .await
    .expect_err("opening the rotated writer must fail");
    assert!(
        matches!(err, CollectError::Tstore(_)),
        "expected a Tstore error, got {err:?}"
    );

    // Nothing must have changed: no reconnects, both connections still
    // ticking on their original tasks.
    assert_eq!(
        count_events_for_connection(&setup.pool, "plc_connected", &setup.conn_a_key).await,
        a_connected_before,
        "A must be completely untouched by the failed apply_config"
    );
    assert_eq!(
        count_events_for_connection(&setup.pool, "plc_connected", &setup.conn_b_key).await,
        b_connected_before,
        "B must be completely untouched by the failed apply_config"
    );
    let a_ptime_after_failure = current.get(&setup.tag_a_key).unwrap().ptime_ms;
    // Wait for a *good read* newer than the failure - the same sample must
    // have advanced `ptime_ms`, carry the real value and be `Quality::Good`.
    // A bare `ptime_ms` advance is not enough: `run_connection` records a
    // Bad/NULL sample on every tick even while unconnected, so a regression
    // that restarted A (e.g. on its old plan) could satisfy it before the new
    // task ever connected.
    assert!(
        wait_until(Duration::from_secs(10), || async {
            current.get(&setup.tag_a_key).is_some_and(|s| {
                s.ptime_ms > a_ptime_after_failure
                    && s.value == Some(11.0)
                    && s.quality == Quality::Good
            })
        })
        .await,
        "A should still be collecting normally (a good read) after the failed apply_config"
    );
    // Counted again *after* that good read: had the failed call restarted A,
    // the new task would have had to connect - recording `plc_connected` -
    // before it could read successfully, so this check cannot be won by a
    // race.
    assert_eq!(
        count_events_for_connection(&setup.pool, "plc_connected", &setup.conn_a_key).await,
        a_connected_before,
        "A must be completely untouched by the failed apply_config (after a good read)"
    );
    assert!(
        current.get(&tag_a2_key).is_none(),
        "the failed apply_config must not have adopted t2"
    );

    // Remove the sabotage and retry with a freshly built config - the
    // collector must still be in a perfectly usable state.
    std::fs::remove_dir(&sabotage_path).expect("remove sabotage directory");
    let retry_config = build_config(&setup.pool, drivers()).await.unwrap();
    let report = apply_through(
        &mut setup.collector,
        &setup.broker,
        &setup.pool,
        retry_config,
    )
    .await
    .expect("retry after removing the sabotage should succeed");
    assert!(report.writer_rotated);
    // The retry's config adds t2 to A's group, so A's plan changed and A is
    // (by design) replaced; B's plan did not change.
    assert_eq!(
        report.replaced,
        vec![setup.conn_a_key.clone()],
        "only A (whose group gained t2) must be replaced: {report:?}"
    );
    assert_eq!(
        report.unchanged,
        vec![setup.conn_b_key.clone()],
        "B must be classified unchanged: {report:?}"
    );
    assert!(
        report.added.is_empty() && report.removed.is_empty(),
        "{report:?}"
    );

    // A's replacement task connects once (`plc_connected` +1) and reads t2.
    let expected_a_connected = a_connected_before + 1;
    assert!(
        wait_until(Duration::from_secs(10), || async {
            count_events_for_connection(&setup.pool, "plc_connected", &setup.conn_a_key).await
                == expected_a_connected
        })
        .await,
        "A's replacement task should record exactly one new plc_connected"
    );
    assert!(
        wait_until(Duration::from_secs(10), || async {
            current.get(&tag_a2_key).map(|s| s.value) == Some(Some(55.0))
        })
        .await,
        "A's replacement task should read the newly added t2 = 55"
    );
    // ...and it is replaced exactly once: poll (up to the same 10 s bound as
    // every other wait here) until at least 1 s has passed *and* A has
    // ticked again, checking on every poll that the count never moved past
    // +1. Ending on "1 s elapsed and A ticked" rather than evaluating the
    // tick once right at 1 s keeps a slow CI runner from turning this into a
    // "A must tick within 1 s" constraint.
    let a_ptime_after_retry = current.get(&setup.tag_a_key).unwrap().ptime_ms;
    let window_start = tokio::time::Instant::now();
    let window_deadline = window_start + Duration::from_secs(10);
    loop {
        assert_eq!(
            count_events_for_connection(&setup.pool, "plc_connected", &setup.conn_a_key).await,
            expected_a_connected,
            "A must be replaced exactly once (no second plc_connected)"
        );
        let a_ticked =
            current.get(&setup.tag_a_key).map(|s| s.ptime_ms) > Some(a_ptime_after_retry);
        if a_ticked && window_start.elapsed() >= Duration::from_secs(1) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < window_deadline,
            "A should have kept ticking during the observation window"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        count_events_for_connection(&setup.pool, "plc_connected", &setup.conn_b_key).await,
        b_connected_before,
        "B must never have been touched, across the failed + retried apply_config"
    );

    setup.collector.stop().await.unwrap();
    setup.sim_a.stop();
    setup.sim_b.stop();
}

/// Test 6 (task instructions §テスト-6): after a writer rotation, the old
/// file must hold exactly what was flushed before rotation (nothing lost),
/// and the new file must hold real rows for the newly-added group (nothing
/// silently swallowed by the "unknown group" path - constraint 3 in the
/// task instructions: the writer must be distributed before the new task is
/// spawned).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn apply_config_writer_rotation_preserves_old_and_new_data() {
    let _serial = TEST_SERIAL.lock().await;
    let mut setup = one_conn_setup("apply-rotation-integrity").await;

    // Let a few real rows land on A before rotating.
    let current = setup.collector.current_values();
    assert!(
        wait_until(Duration::from_secs(10), || async {
            current.get(&setup.tag_a_key).map(|s| s.value) == Some(Some(11.0))
        })
        .await
    );
    tokio::time::sleep(Duration::from_millis(300)).await;

    let sim_b = Simulator::start().await;
    sim_b.set_holding_register(0, 66);
    let conn_b = PlcConnectionService::new(setup.pool.clone())
        .create(conn_input("B", sim_b.addr.port()))
        .await
        .unwrap();
    let group_b = CollectionGroupService::new(setup.pool.clone())
        .create(group_input("Gb", conn_b.id, 100))
        .await
        .unwrap();
    let tag_b = TagService::new(setup.pool.clone())
        // Registry-wide unique tag name - A already owns "t1".
        .create(tag_input("b1", group_b.id, "40001", "i16"))
        .await
        .unwrap();
    let group_b_key = format!("grp:{}", group_b.id);
    let tag_b_key = format!("tag:{}", tag_b.id);

    let new_config = build_config(&setup.pool, drivers()).await.unwrap();
    let report = apply_through(&mut setup.collector, &setup.broker, &setup.pool, new_config)
        .await
        .expect("apply_config should succeed");
    assert!(report.writer_rotated);

    assert!(
        wait_until(Duration::from_secs(10), || async {
            current.get(&tag_b_key).map(|s| s.value) == Some(Some(66.0))
        })
        .await,
        "B should collect real values into the rotated file"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;

    setup.collector.stop().await.unwrap();
    setup.sim_a.stop();
    sim_b.stop();

    let files = banto_tstore::list_data_files(&setup.env.data_dir()).unwrap();
    assert_eq!(
        files.len(),
        2,
        "the rotation should have produced a second file"
    );
    assert_eq!(files[0].seq, 1);
    assert_eq!(files[1].seq, 2);

    // Old file: A's pre-rotation rows must still be there (flushed, not
    // lost - this is the file that was open before rotation).
    let old_group_a_key = format!("grp:{}", setup.group_a_id);
    let old_reader = TsReader::open(&files[0].path).await.unwrap();
    let old_rows = old_reader
        .read_range(&old_group_a_key, 0, i64::MAX)
        .await
        .unwrap();
    assert!(
        old_rows.iter().any(|r| r.values[0] == Some(11.0)),
        "the pre-rotation file should retain A's real values"
    );

    // New file: B's group must have real rows recorded after rotation.
    let new_reader = TsReader::open(&files[1].path).await.unwrap();
    assert!(
        new_reader.groups().iter().any(|g| g.key == group_b_key),
        "the rotated file should describe B's new group"
    );
    let new_rows = new_reader
        .read_range(&group_b_key, 0, i64::MAX)
        .await
        .unwrap();
    assert!(
        new_rows.iter().any(|r| r.values[0] == Some(66.0)),
        "the rotated file should hold B's real recorded value"
    );
}

/// A session that cannot be started is reported in `SessionSync::failures`
/// (not printed by the library), and gets no client.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_that_cannot_start_is_reported_in_failures() {
    let _serial = TEST_SERIAL.lock().await;
    let env = TempEnv::new("session-failure");
    let pool = open_registry(&env).await;
    let conn = PlcConnectionService::new(pool.clone())
        .create(conn_input("PLC1", 502))
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
    // The plan is built with the real drivers, but the sessions dial through
    // an empty registry: the protocol has no driver there.
    let mut config = build_config(&pool, drivers()).await.unwrap();
    let sessions = BrokerSessions::new(
        Arc::new(DriverRegistry::new()),
        fast_options().broker_options(),
    );
    let snapshot = RegistrySnapshot::load(&pool).await.unwrap();
    let sync = sessions.sync(&snapshot, &mut config).await;
    assert_eq!(sync.failures.len(), 1);
    assert_eq!(sync.failures[0].0, conn.id);
    assert!(!sync.failures[0].1.is_empty());
    assert_eq!(sessions.connection_count(), 0);
    sessions.shutdown().await;
}

/// Wait until the broker session behind `handle` reads `Connected`.
async fn connected_within(handle: &banto_broker::ReadOnlyHandle, limit: Duration) -> bool {
    let mut rx = handle.status_watch();
    tokio::time::timeout(
        limit,
        rx.wait_for(|s| *s == banto_broker::BrokerConnectionStatus::Connected),
    )
    .await
    .is_ok_and(|r| r.is_ok())
}

/// A connection whose dial settings changed has its old broker task stopped
/// and joined by [`BrokerSessions::sync`] before the new session is ensured
/// (`SessionDirectory::stop_and_join`, 2026-10-24 owner decision), even while
/// someone still holds the old handle - a sibling connection's collect task
/// holds the whole factory, so the old handle outlives the sync. Only
/// dropping the directory's entry (`remove`) signals the old task but does not
/// wait for it: the new session dials while the old one may still hold its
/// socket, and a PLC that refuses a second connection on the same port (R08:
/// 10061) refuses the new one. The connection that did not change keeps its
/// session.
///
/// A current-thread runtime makes the check deterministic: the old task can
/// only run its teardown while `sync` itself awaits it, so "Stopped" right
/// after `sync` returns (no await in between) proves the join.
#[tokio::test(flavor = "current_thread")]
async fn a_changed_connection_has_its_old_session_joined_before_the_new_one() {
    let _serial = TEST_SERIAL.lock().await;
    let env = TempEnv::new("changed-joins-old");
    let pool = open_registry(&env).await;
    let connections = PlcConnectionService::new(pool.clone());
    let mut input_a = conn_input("PLC-A", 502);
    input_a.simulation = true;
    let a = connections.create(input_a.clone()).await.unwrap();
    let mut input_b = conn_input("PLC-B", 503);
    input_b.simulation = true;
    let b = connections.create(input_b).await.unwrap();
    for (name, conn_id) in [("GA", a.id), ("GB", b.id)] {
        let group = CollectionGroupService::new(pool.clone())
            .create(group_input(name, conn_id, 100))
            .await
            .unwrap();
        TagService::new(pool.clone())
            .create(tag_input(&format!("t-{name}"), group.id, "40001", "i16"))
            .await
            .unwrap();
    }

    let sessions = BrokerSessions::new(DRIVERS.clone(), fast_options().broker_options());
    let mut config = build_config(&pool, drivers()).await.unwrap();
    let snapshot = RegistrySnapshot::load(&pool).await.unwrap();
    let first = sessions.sync(&snapshot, &mut config).await;
    assert!(first.failures.is_empty(), "{:?}", first.failures);
    let old_a = sessions.handle(a.id).expect("A has a session");
    let old_b = sessions.handle(b.id).expect("B has a session");
    assert!(connected_within(&old_a, Duration::from_secs(5)).await);
    assert!(connected_within(&old_b, Duration::from_secs(5)).await);

    // Only A's settings change.
    input_a.port = 1502;
    connections.update(a.id, input_a).await.unwrap();
    let mut config = build_config(&pool, drivers()).await.unwrap();
    let snapshot = RegistrySnapshot::load(&pool).await.unwrap();
    let second = sessions.sync(&snapshot, &mut config).await;

    // `first.factory` (what B's collect task would hold) and `old_a` are
    // still alive here; the old task must already be stopped and joined.
    assert_eq!(
        *old_a.status_watch().borrow(),
        banto_broker::BrokerConnectionStatus::Stopped,
        "the old session of the changed connection must be joined by sync"
    );
    assert!(second.failures.is_empty(), "{:?}", second.failures);
    assert!(second.stale.is_empty());
    let new_a = sessions.handle(a.id).expect("A has a new session");
    assert_ne!(new_a.settings_fingerprint(), old_a.settings_fingerprint());
    assert!(connected_within(&new_a, Duration::from_secs(5)).await);
    assert_eq!(
        *old_b.status_watch().borrow(),
        banto_broker::BrokerConnectionStatus::Connected,
        "the unchanged connection keeps its session"
    );
    assert_eq!(
        sessions.handle(b.id).unwrap().settings_fingerprint(),
        old_b.settings_fingerprint()
    );
    drop(first);
    sessions.shutdown().await;
}
