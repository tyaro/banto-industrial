//! End-to-end tests for the W3-B auto-write engine
//! (`luminous-discovering-goblet.md`), driving the FULL stack - `Engine` →
//! poller → rule engine → writer → broker → in-process SLMP simulator - against
//! a real (in-memory) database. These assert on BOTH the simulator's resulting
//! device state AND the `write_audit_log` rows, exactly as the plan's test
//! matrix requires.
//!
//! ## Anti-hang discipline (W3-A lesson)
//!
//! Every wait for an asynchronous outcome is bounded by [`wait_until`]'s
//! deadline, and [`Engine::shutdown`] is exercised under a `tokio::time::timeout`,
//! so a bug produces a fast assertion failure rather than an infinite hang.
//! These tests use REAL time with short intervals because the broker does real
//! loopback I/O to the simulator, so paused virtual time is not appropriate here
//! (the same choice the broker's own network tests make). The deterministic
//! virtual-time coverage lives in the pure unit tests.

use std::time::Duration;

use banto_plc::SlmpDevice;
use banto_plc_write::slmp::simulator::Simulator;
use banto_tags::{
    CollectionGroupInput, CollectionGroupService, PlcConnectionInput, PlcConnectionService,
    TagInput, TagService,
};
use relay_wright_core::db::init_db_memory;
use relay_wright_core::engine::rate_limiter::RateLimitConfig;
use relay_wright_core::write_rule_conditions::WriteRuleConditionInput;
use relay_wright_core::write_rules::{WriteRuleInput, WriteRuleService};
use relay_wright_core::write_targets::{WriteTargetInput, WriteTargetService};
use relay_wright_core::{Engine, EngineConfig};
use sqlx::SqlitePool;

/// Short cadence so tests settle in tens of milliseconds; generous rate caps so
/// they never trip except in the dedicated rate-limit test.
fn fast_config() -> EngineConfig {
    EngineConfig {
        poll_interval: Duration::from_millis(15),
        eval_interval: Duration::from_millis(15),
        rate: RateLimitConfig {
            window: Duration::from_secs(60),
            global_max: 1000,
            per_connection_max: 1000,
        },
        ..Default::default()
    }
}

/// Poll `check` until it returns `true` or the deadline elapses. Returns whether
/// it succeeded (tests assert on the result, so a stuck condition fails fast).
async fn wait_until<F>(timeout: Duration, mut check: F) -> bool
where
    F: FnMut() -> bool,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if check() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn count(pool: &SqlitePool, action: &str, result: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM write_audit_log WHERE action = ? AND result = ?")
        .bind(action)
        .bind(result)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Poll the audit-row count until it reaches `at_least` or the deadline elapses.
async fn wait_for_count(
    pool: &SqlitePool,
    action: &str,
    result: &str,
    at_least: i64,
    timeout: Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if count(pool, action, result).await >= at_least {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// A fully-wired fixture: an in-memory DB, a running simulator, and one SLMP PLC
/// connection + collection group pointed at it, plus helpers to make source tags
/// / write targets / rules.
struct Fixture {
    pool: SqlitePool,
    sim: Simulator,
    conn_id: i64,
    group_id: i64,
    tags: TagService,
    targets: WriteTargetService,
    rules: WriteRuleService,
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
            targets: WriteTargetService::new(pool.clone()),
            rules: WriteRuleService::new(pool.clone()),
            pool,
            sim,
            conn_id: conn.id,
            group_id: group.id,
        }
    }

    async fn source_tag(&self, name: &str, address: &str) -> i64 {
        self.tags
            .create(TagInput {
                name: name.to_string(),
                collection_group_id: self.group_id,
                address: address.to_string(),
                data_type: "u16".to_string(),
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
            .id
    }

    async fn target(&self, name: &str, address: &str) -> i64 {
        self.targets
            .create(WriteTargetInput {
                name: name.to_string(),
                plc_connection_id: self.conn_id,
                address: address.to_string(),
                data_type: "u16".to_string(),
                raw_lo: None,
                raw_hi: None,
                eng_lo: None,
                eng_hi: None,
                unit: None,
                decimals: 0,
                enabled: true,
            })
            .await
            .unwrap()
            .id
    }

    /// A rule: when `source` (u16) `> threshold`, write the constant `value` to
    /// `target`, on the given edge mode.
    async fn rule(
        &self,
        name: &str,
        edge_mode: &str,
        source_tag_id: i64,
        threshold: f64,
        target_id: i64,
        value: f64,
    ) {
        self.rules
            .create(WriteRuleInput {
                name: name.to_string(),
                enabled: true,
                edge_mode: edge_mode.to_string(),
                cooldown_ms: None,
                write_target_id: target_id,
                write_value_mode: "constant".to_string(),
                write_constant_value: Some(value),
                write_source_tag_id: None,
                conditions: vec![WriteRuleConditionInput {
                    source_tag_id,
                    operator: "gt".to_string(),
                    threshold_value: threshold,
                    threshold_value_2: None,
                }],
            })
            .await
            .expect("create rule");
    }
}

// ---------------------------------------------------------------------------
// Rising edge: exactly one write lands; a held-true condition does not re-fire.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rising_edge_writes_exactly_once_and_does_not_refire_while_held() {
    let f = Fixture::new().await;
    let src = f.source_tag("Src", "D100").await;
    let tgt = f.target("Tgt", "D200").await;
    f.rule("R1", "rising", src, 100.0, tgt, 777.0).await;

    f.sim.set_word(SlmpDevice::D, 100, 0); // start below threshold

    let (engine, control) = Engine::start(f.pool.clone(), connections(&f).await, fast_config())
        .await
        .expect("engine start");
    control.arm(Some("tester")).await.unwrap();

    // Let the poller/evaluator seed the rule at `false` before crossing.
    tokio::time::sleep(Duration::from_millis(120)).await;

    // Cross the threshold -> exactly one write should land.
    f.sim.set_word(SlmpDevice::D, 100, 500);
    assert!(
        wait_until(Duration::from_secs(5), || f
            .sim
            .get_word(SlmpDevice::D, 200)
            == 777)
        .await,
        "the rising edge should have written 777 to the target"
    );
    // The simulator's device value becomes visible mid-`Writer::process`
    // (log-before-write inserts the audit row first, but its result is set to
    // `ok` only AFTER the broker write returns), so bound-wait for the row
    // instead of asserting the count immediately - the gap is tiny but real
    // under CPU load.
    assert!(
        wait_for_count(&f.pool, "rule_fire", "ok", 1, Duration::from_secs(5)).await,
        "the rising-edge write must be audited ok"
    );
    assert_eq!(
        count(&f.pool, "rule_fire", "ok").await,
        1,
        "exactly one physical write"
    );

    // Held true: reset the device and confirm no second write occurs.
    f.sim.set_word(SlmpDevice::D, 200, 0);
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        f.sim.get_word(SlmpDevice::D, 200),
        0,
        "a held-true condition must not re-fire"
    );
    assert_eq!(
        count(&f.pool, "rule_fire", "ok").await,
        1,
        "still exactly one write"
    );

    tokio::time::timeout(Duration::from_secs(5), engine.shutdown())
        .await
        .expect("shutdown must not hang");
}

// ---------------------------------------------------------------------------
// Clear then re-trigger produces a second write.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn condition_clears_then_retriggers_writes_again() {
    let f = Fixture::new().await;
    let src = f.source_tag("Src", "D100").await;
    let tgt = f.target("Tgt", "D200").await;
    f.rule("R1", "rising", src, 100.0, tgt, 777.0).await;

    f.sim.set_word(SlmpDevice::D, 100, 0);
    let (engine, control) = Engine::start(f.pool.clone(), connections(&f).await, fast_config())
        .await
        .expect("engine start");
    control.arm(Some("tester")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(120)).await;

    // First trigger.
    f.sim.set_word(SlmpDevice::D, 100, 500);
    assert!(wait_for_count(&f.pool, "rule_fire", "ok", 1, Duration::from_secs(5)).await);

    // Clear below threshold, wait for the engine to observe the falling side.
    f.sim.set_word(SlmpDevice::D, 100, 0);
    tokio::time::sleep(Duration::from_millis(120)).await;

    // Re-trigger -> a second write.
    f.sim.set_word(SlmpDevice::D, 100, 500);
    assert!(
        wait_for_count(&f.pool, "rule_fire", "ok", 2, Duration::from_secs(5)).await,
        "re-triggering should produce a second write"
    );

    tokio::time::timeout(Duration::from_secs(5), engine.shutdown())
        .await
        .expect("shutdown must not hang");
}

// ---------------------------------------------------------------------------
// Disarmed (the default): no physical write, but a suppressed_disarmed row.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn disarmed_suppresses_the_write_but_logs_it() {
    let f = Fixture::new().await;
    let src = f.source_tag("Src", "D100").await;
    let tgt = f.target("Tgt", "D200").await;
    f.rule("R1", "rising", src, 100.0, tgt, 777.0).await;

    f.sim.set_word(SlmpDevice::D, 100, 0);
    // NOTE: never armed - default disarmed.
    let (engine, _control) = Engine::start(f.pool.clone(), connections(&f).await, fast_config())
        .await
        .expect("engine start");
    tokio::time::sleep(Duration::from_millis(120)).await;

    f.sim.set_word(SlmpDevice::D, 100, 500);
    assert!(
        wait_for_count(
            &f.pool,
            "rule_fire",
            "suppressed_disarmed",
            1,
            Duration::from_secs(5)
        )
        .await,
        "a disarmed engine must audit the suppressed write"
    );
    assert_eq!(
        f.sim.get_word(SlmpDevice::D, 200),
        0,
        "a disarmed engine must NOT physically write"
    );
    assert_eq!(
        count(&f.pool, "rule_fire", "ok").await,
        0,
        "no ok write while disarmed"
    );

    tokio::time::timeout(Duration::from_secs(5), engine.shutdown())
        .await
        .expect("shutdown must not hang");
}

// ---------------------------------------------------------------------------
// Dry-run: no physical write, but a suppressed_dry_run row.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dry_run_suppresses_the_write_but_logs_it() {
    let f = Fixture::new().await;
    let src = f.source_tag("Src", "D100").await;
    let tgt = f.target("Tgt", "D200").await;
    f.rule("R1", "rising", src, 100.0, tgt, 777.0).await;

    f.sim.set_word(SlmpDevice::D, 100, 0);
    let (engine, control) = Engine::start(f.pool.clone(), connections(&f).await, fast_config())
        .await
        .expect("engine start");
    control.arm(Some("tester")).await.unwrap();
    control.set_dry_run(true, Some("tester")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(120)).await;

    f.sim.set_word(SlmpDevice::D, 100, 500);
    assert!(
        wait_for_count(
            &f.pool,
            "rule_fire",
            "suppressed_dry_run",
            1,
            Duration::from_secs(5)
        )
        .await,
        "dry-run must audit the would-be write"
    );
    assert_eq!(
        f.sim.get_word(SlmpDevice::D, 200),
        0,
        "dry-run must NOT physically write"
    );
    assert_eq!(
        count(&f.pool, "rule_fire", "ok").await,
        0,
        "no ok write in dry-run"
    );

    tokio::time::timeout(Duration::from_secs(5), engine.shutdown())
        .await
        .expect("shutdown must not hang");
}

// ---------------------------------------------------------------------------
// Rate limit: a write storm trips the breaker, further writes are suppressed,
// the engine auto-disarms, and a rate_limit_tripped row exists.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rate_limit_storm_trips_breaker_and_auto_disarms() {
    let f = Fixture::new().await;

    // Five independent rules, all writing to the same connection.
    let mut sources = Vec::new();
    for i in 0..5 {
        let src = f
            .source_tag(&format!("Src{i}"), &format!("D{}", 100 + i))
            .await;
        let tgt = f.target(&format!("Tgt{i}"), &format!("D{}", 200 + i)).await;
        f.rule(&format!("R{i}"), "rising", src, 100.0, tgt, 777.0)
            .await;
        sources.push(100 + i as u32);
        f.sim.set_word(SlmpDevice::D, 100 + i as u32, 0);
    }

    // Per-connection cap of 3: the 4th write in the window trips the breaker.
    let config = EngineConfig {
        poll_interval: Duration::from_millis(15),
        eval_interval: Duration::from_millis(15),
        rate: RateLimitConfig {
            window: Duration::from_secs(60),
            global_max: 1000,
            per_connection_max: 3,
        },
        ..Default::default()
    };
    let (engine, control) = Engine::start(f.pool.clone(), connections(&f).await, config)
        .await
        .expect("engine start");
    control.arm(Some("tester")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(120)).await;

    // Storm: cross all five thresholds at once.
    for number in &sources {
        f.sim.set_word(SlmpDevice::D, *number, 500);
    }

    // The breaker must trip and auto-disarm.
    assert!(
        wait_for_count(
            &f.pool,
            "rate_limit_tripped",
            "suppressed_rate_limited",
            1,
            Duration::from_secs(5)
        )
        .await,
        "the storm should have tripped the rate-limit breaker"
    );
    assert!(
        wait_until(Duration::from_secs(5), || !control.is_armed()).await,
        "tripping the breaker must auto-disarm the engine"
    );
    // Exactly the cap's worth of physical writes got through.
    assert_eq!(
        count(&f.pool, "rule_fire", "ok").await,
        3,
        "only the per-connection cap (3) of physical writes should land"
    );

    tokio::time::timeout(Duration::from_secs(5), engine.shutdown())
        .await
        .expect("shutdown must not hang");
}

// ---------------------------------------------------------------------------
// Falling edge mode behaves correctly end to end.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn falling_edge_fires_on_true_to_false() {
    let f = Fixture::new().await;
    let src = f.source_tag("Src", "D100").await;
    let tgt = f.target("Tgt", "D200").await;
    f.rule("R1", "falling", src, 100.0, tgt, 777.0).await;

    f.sim.set_word(SlmpDevice::D, 100, 500); // start ABOVE threshold (true)
    let (engine, control) = Engine::start(f.pool.clone(), connections(&f).await, fast_config())
        .await
        .expect("engine start");
    control.arm(Some("tester")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(120)).await;

    // No write yet (seeded true, no falling edge).
    assert_eq!(count(&f.pool, "rule_fire", "ok").await, 0);

    // Drop below threshold -> falling edge -> one write.
    f.sim.set_word(SlmpDevice::D, 100, 0);
    assert!(
        wait_until(Duration::from_secs(5), || f
            .sim
            .get_word(SlmpDevice::D, 200)
            == 777)
        .await,
        "falling edge should write on true->false"
    );
    // Same audit-row race as the rising-edge test: the device value is
    // observable before `set_result` commits `ok`, so bound-wait for the row.
    assert!(
        wait_for_count(&f.pool, "rule_fire", "ok", 1, Duration::from_secs(5)).await,
        "the falling-edge write must be audited ok"
    );
    assert_eq!(count(&f.pool, "rule_fire", "ok").await, 1);

    tokio::time::timeout(Duration::from_secs(5), engine.shutdown())
        .await
        .expect("shutdown must not hang");
}

// ---------------------------------------------------------------------------
// Clean shutdown returns promptly even with the engine actively armed/running.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn shutdown_returns_promptly_while_running() {
    let f = Fixture::new().await;
    let src = f.source_tag("Src", "D100").await;
    let tgt = f.target("Tgt", "D200").await;
    f.rule("R1", "rising", src, 100.0, tgt, 777.0).await;
    f.sim.set_word(SlmpDevice::D, 100, 500);

    let (engine, control) = Engine::start(f.pool.clone(), connections(&f).await, fast_config())
        .await
        .expect("engine start");
    control.arm(Some("tester")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(60)).await;

    // The whole point of the W3-A watch-signal design: this returns quickly.
    tokio::time::timeout(Duration::from_secs(5), engine.shutdown())
        .await
        .expect("shutdown must not hang even with tasks and a control handle live");
}

// --- helpers ---------------------------------------------------------------

/// The full connection registry the engine should manage (just the one SLMP
/// connection this fixture created).
async fn connections(f: &Fixture) -> Vec<banto_tags::PlcConnection> {
    PlcConnectionService::new(f.pool.clone())
        .list(Default::default())
        .await
        .unwrap()
        .rows
}
