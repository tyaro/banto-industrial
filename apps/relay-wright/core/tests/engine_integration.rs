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
                simulation: false,

                word_order: "low_high".to_string(),
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
                default_writable: true,
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
                writable: false,
                tag_kind: "plc".to_string(),
                expression: None,
                retain: false,
                expected_revision: None,
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
                string_length: None,
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

    /// A STRING source tag (`length` words) at `address`.
    async fn string_source_tag(&self, name: &str, address: &str, length: i64) -> i64 {
        self.tags
            .create(TagInput {
                name: name.to_string(),
                collection_group_id: self.group_id,
                address: address.to_string(),
                data_type: "string".to_string(),
                string_length: Some(length),
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
                writable: false,
                tag_kind: "plc".to_string(),
                expression: None,
                retain: false,
                expected_revision: None,
            })
            .await
            .unwrap()
            .id
    }

    /// A STRING write target (`length` words) at `address`.
    async fn string_target(&self, name: &str, address: &str, length: i64) -> i64 {
        self.targets
            .create(WriteTargetInput {
                name: name.to_string(),
                plc_connection_id: self.conn_id,
                address: address.to_string(),
                data_type: "string".to_string(),
                string_length: Some(length),
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

    /// A rule: when string `source` `eq threshold_text`, write the string
    /// constant `constant_text` to a string `target`, on `edge_mode`.
    async fn string_rule(
        &self,
        name: &str,
        edge_mode: &str,
        source_tag_id: i64,
        threshold_text: &str,
        target_id: i64,
        constant_text: &str,
    ) {
        self.rules
            .create(WriteRuleInput {
                name: name.to_string(),
                enabled: true,
                edge_mode: edge_mode.to_string(),
                cooldown_ms: None,
                write_target_id: target_id,
                write_value_mode: "constant".to_string(),
                write_constant_value: None,
                write_constant_text: Some(constant_text.to_string()),
                write_source_tag_id: None,
                conditions: vec![WriteRuleConditionInput {
                    source_tag_id,
                    operator: "eq".to_string(),
                    threshold_value: None,
                    threshold_value_2: None,
                    threshold_text: Some(threshold_text.to_string()),
                }],
            })
            .await
            .expect("create string rule");
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
                write_constant_text: None,
                write_source_tag_id: None,
                conditions: vec![WriteRuleConditionInput {
                    source_tag_id,
                    operator: "gt".to_string(),
                    threshold_value: Some(threshold),
                    threshold_value_2: None,
                    threshold_text: None,
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
// H10 ②: a tiny auto-disarm window fires, persists, and is audited - even
// with the engine otherwise idle (no rule ever firing). A large/disabled
// window must leave the engine armed.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tiny_auto_disarm_window_fires_persists_and_is_audited_while_idle() {
    let f = Fixture::new().await;
    // No rules/tags at all - proves `run_engine_loop` checks expiry on EVERY
    // tick unconditionally, not only when `Writer::process` runs (it cannot:
    // there is nothing here for it to ever process).
    let config = EngineConfig {
        auto_disarm: Some(Duration::from_millis(200)),
        ..fast_config()
    };
    let (engine, control) = Engine::start(f.pool.clone(), connections(&f).await, config)
        .await
        .expect("engine start");
    control.arm(Some("tester")).await.unwrap();
    assert!(control.is_armed());

    assert!(
        wait_until(Duration::from_secs(5), || !control.is_armed()).await,
        "the tiny arm window should have auto-disarmed the idle engine"
    );
    assert!(
        wait_for_count(&f.pool, "disarm", "ok", 1, Duration::from_secs(5)).await,
        "the auto-disarm must be audited as a disarm/ok row"
    );
    assert_eq!(
        count(&f.pool, "disarm", "ok").await,
        1,
        "exactly one auto-disarm audit row, not a double-audit"
    );

    // H10 ②: unlike the rate-limit trip path, expiry must PERSIST the disarm
    // to `armed_state` (so the DB/UI history is not left stale).
    let persisted: i64 = sqlx::query_scalar("SELECT armed_persisted FROM armed_state WHERE id = 1")
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(persisted, 0, "the auto-disarm must be persisted");

    let (detail, actor): (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT detail, actor_username FROM write_audit_log \
         WHERE action = 'disarm' AND result = 'ok'",
    )
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(
        detail.as_deref(),
        Some("engine auto-disarmed: arm window elapsed")
    );
    assert_eq!(actor, None, "an automatic expiry has no human actor");

    tokio::time::timeout(Duration::from_secs(5), engine.shutdown())
        .await
        .expect("shutdown must not hang");
}

#[tokio::test]
async fn a_large_auto_disarm_window_leaves_the_engine_armed() {
    let f = Fixture::new().await;
    let config = EngineConfig {
        auto_disarm: Some(Duration::from_secs(3600)),
        ..fast_config()
    };
    let (engine, control) = Engine::start(f.pool.clone(), connections(&f).await, config)
        .await
        .expect("engine start");
    control.arm(Some("tester")).await.unwrap();

    // Give the loop plenty of ticks to (incorrectly) expire if the window
    // were not being respected.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        control.is_armed(),
        "a window far in the future must not auto-disarm"
    );
    assert_eq!(
        count(&f.pool, "disarm", "ok").await,
        0,
        "no auto-disarm audit row should exist"
    );

    tokio::time::timeout(Duration::from_secs(5), engine.shutdown())
        .await
        .expect("shutdown must not hang");
}

#[tokio::test]
async fn auto_disarm_disabled_never_expires_even_when_armed_a_long_time() {
    let f = Fixture::new().await;
    let config = EngineConfig {
        auto_disarm: None,
        ..fast_config()
    };
    let (engine, control) = Engine::start(f.pool.clone(), connections(&f).await, config)
        .await
        .expect("engine start");
    control.arm(Some("tester")).await.unwrap();

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        control.is_armed(),
        "auto_disarm: None must disable the feature entirely"
    );
    assert_eq!(count(&f.pool, "disarm", "ok").await, 0);

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

// ---------------------------------------------------------------------------
// S2 文字列タグ: a string eq rule writes a string constant that lands in the
// simulator, fires exactly once on the edge, and audits the text; disarmed
// suppresses the physical write while still auditing the text.
// ---------------------------------------------------------------------------

/// Pack ASCII `text` into `words` consecutive D-registers at `number`
/// (low-byte-first per word, NUL-padded) - the wire layout S1's decoder reads
/// back. ASCII-only keeps the test independent of an SJIS encoder.
fn seed_string(f: &Fixture, number: u32, words: u32, text: &str) {
    let bytes = text.as_bytes();
    for w in 0..words {
        let lo = bytes.get((w * 2) as usize).copied().unwrap_or(0) as u16;
        let hi = bytes.get((w * 2 + 1) as usize).copied().unwrap_or(0) as u16;
        f.sim.set_word(SlmpDevice::D, number + w, (hi << 8) | lo);
    }
}

/// The inverse of [`seed_string`]: read `words` registers, low-byte-first, and
/// trim at the first NUL - what the engine's string write leaves behind.
fn read_string(f: &Fixture, number: u32, words: u32) -> String {
    let mut bytes = Vec::new();
    for w in 0..words {
        let word = f.sim.get_word(SlmpDevice::D, number + w);
        bytes.push((word & 0xFF) as u8);
        bytes.push((word >> 8) as u8);
    }
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).to_string()
}

async fn ok_detail(pool: &SqlitePool) -> serde_json::Value {
    let detail: Option<String> = sqlx::query_scalar(
        "SELECT detail FROM write_audit_log WHERE action = 'rule_fire' AND result = 'ok' LIMIT 1",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    serde_json::from_str(&detail.expect("string write records a detail JSON")).unwrap()
}

#[tokio::test]
async fn string_eq_writes_once_lands_in_sim_and_audits_the_text() {
    let f = Fixture::new().await;
    let src = f.string_source_tag("Src", "D100", 4).await;
    let tgt = f.string_target("Tgt", "D200", 4).await;
    f.string_rule("SR", "rising", src, "OK", tgt, "NG").await;

    seed_string(&f, 100, 4, "NG"); // start not-matching

    let (engine, control) = Engine::start(f.pool.clone(), connections(&f).await, fast_config())
        .await
        .expect("engine start");
    control.arm(Some("tester")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(120)).await;

    // Match the comparand -> rising edge -> the string constant lands.
    seed_string(&f, 100, 4, "OK");
    assert!(
        wait_until(Duration::from_secs(5), || read_string(&f, 200, 4) == "NG").await,
        "the string rule should have written 'NG' to the target"
    );
    assert!(
        wait_for_count(&f.pool, "rule_fire", "ok", 1, Duration::from_secs(5)).await,
        "the string write must be audited ok"
    );
    // The numeric snapshot column is NULL; the text lives in the detail JSON.
    let (src_val, tgt_val): (Option<f64>, Option<f64>) = sqlx::query_as(
        "SELECT source_value_snapshot, target_value_written FROM write_audit_log \
         WHERE action = 'rule_fire' AND result = 'ok' LIMIT 1",
    )
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!((src_val, tgt_val), (None, None));
    let detail = ok_detail(&f.pool).await;
    assert_eq!(detail["sourceText"], "OK");
    assert_eq!(detail["writtenText"], "NG");

    // Held true: clear the target and confirm no second write.
    seed_string(&f, 200, 4, ""); // zero it out
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        read_string(&f, 200, 4),
        "",
        "a held-true match must not re-fire"
    );
    assert_eq!(count(&f.pool, "rule_fire", "ok").await, 1);

    tokio::time::timeout(Duration::from_secs(5), engine.shutdown())
        .await
        .expect("shutdown must not hang");
}

#[tokio::test]
async fn string_write_disarmed_is_suppressed_but_audited() {
    let f = Fixture::new().await;
    let src = f.string_source_tag("Src", "D100", 4).await;
    let tgt = f.string_target("Tgt", "D200", 4).await;
    f.string_rule("SR", "rising", src, "OK", tgt, "NG").await;

    seed_string(&f, 100, 4, "NG");
    // NOTE: never armed - default disarmed.
    let (engine, _control) = Engine::start(f.pool.clone(), connections(&f).await, fast_config())
        .await
        .expect("engine start");
    tokio::time::sleep(Duration::from_millis(120)).await;

    seed_string(&f, 100, 4, "OK");
    assert!(
        wait_for_count(
            &f.pool,
            "rule_fire",
            "suppressed_disarmed",
            1,
            Duration::from_secs(5)
        )
        .await,
        "a disarmed engine must audit the suppressed string write"
    );
    assert_eq!(
        read_string(&f, 200, 4),
        "",
        "a disarmed engine must NOT physically write the string"
    );
    // Even suppressed, the audited row carries the string context.
    let detail: Option<String> = sqlx::query_scalar(
        "SELECT detail FROM write_audit_log \
         WHERE action = 'rule_fire' AND result = 'suppressed_disarmed' LIMIT 1",
    )
    .fetch_one(&f.pool)
    .await
    .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&detail.expect("detail present")).unwrap();
    assert_eq!(parsed["writtenText"], "NG");

    tokio::time::timeout(Duration::from_secs(5), engine.shutdown())
        .await
        .expect("shutdown must not hang");
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
