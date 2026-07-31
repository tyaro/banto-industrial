//! relay-wright's auto-write engine (W3, `luminous-discovering-goblet.md`).
//!
//! ## Layers
//!
//! - [`broker`] (W3-A): the PLC access broker. One live SLMP session per CPU;
//!   every read and write to that CPU passes through it, serialized on one
//!   socket. Infrastructure only - it has no notion of rules, arming, or
//!   auditing.
//! - The **W3-B auto-write engine** (this module plus [`current_values`],
//!   [`poller`], [`rule_engine`], [`arming`], [`rate_limiter`], [`writer`],
//!   [`write_audit`]): watches source tags, evaluates condition→action rules,
//!   and - only when explicitly armed - writes to live PLCs, behind a stack of
//!   safety guardrails.
//!
//! ## Safety invariants (the reason this milestone is split out and reviewed)
//!
//! 1. **Default disarmed, reset on startup** - [`arming::ArmingState::new`]
//!    hard-codes the live armed flag to `false`; the persisted value is loaded
//!    only as informational history. A restart never resumes live writing.
//! 2. **Structural eval/exec separation** - [`rule_engine`] cannot write (it
//!    never imports a broker handle); only [`writer::Writer`] holds a
//!    write-capable [`broker::BrokerHandle`].
//! 3. **Edge-triggered** - [`rule_engine::RuleEngine::evaluate`] fires on a
//!    state transition, seeding (not firing) on first observation, so a
//!    held-true condition writes exactly once.
//! 4. **Rate limiter / breaker** - [`rate_limiter::RateLimiter`] caps writes
//!    globally and per connection; a trip auto-disarms and needs a manual
//!    re-arm.
//! 5. **Log-before-write** - [`writer::Writer::process`] inserts the audit row
//!    before calling `broker.write`; every suppressed case is audited too.
//! 6. **Dry-run** - dry-run evaluates and audits would-be writes but never
//!    calls `broker.write`.
//!
//! ## Task/shutdown design
//!
//! [`Engine::start`] spawns two tasks - the [`poller`] (reads → cache) and the
//! evaluate+write loop (cache → intents → [`writer::Writer`]) - plus the
//! broker's own per-connection tasks. All of them `select!` on a shared
//! [`tokio::sync::watch`] shutdown signal; [`Engine::shutdown`] flips it, awaits
//! the two engine tasks, then shuts the broker down. Nothing relies on a channel
//! closing (the W3-A shutdown-hang lesson), so shutdown cannot hang even while a
//! caller still holds handles.

pub mod arming;
pub mod broker;
pub mod current_values;
pub mod poller;
pub mod rate_limiter;
pub mod rule_engine;
pub mod write_audit;
pub mod writer;

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use banto_core::BantoError;
use banto_tags::PlcConnection;
use sqlx::SqlitePool;
use tokio::sync::watch;
use tokio::task::JoinHandle;

pub use broker::{BackoffConfig, BrokerError, BrokerHandle, BrokerSupervisor, ReadOnlyHandle};

use arming::ArmingState;
use current_values::CurrentValues;
use poller::{run_poller, ResolvedSource};
use rate_limiter::{RateLimitConfig, RateLimiter};
use rule_engine::{
    CompiledCondition, CompiledRule, EdgeMode, Operator, PendingWrite, RuleEngine, ValueMode,
};
use write_audit::{
    insert_row, load_persisted_armed, persist_armed, AuditAction, AuditResult, AuditRow,
};
use writer::{ResolvedTarget, Writer};

/// The one protocol this engine speaks.
const SLMP_PROTOCOL: &str = "slmp";

/// Tunables for [`Engine::start`]. All have sensible defaults via [`Default`].
#[derive(Debug, Clone, Copy)]
pub struct EngineConfig {
    /// How often the poller reads every source tag (default 500 ms).
    pub poll_interval: Duration,
    /// How often rules are evaluated and firing writes issued (default 500 ms).
    pub eval_interval: Duration,
    /// Broker reconnect backoff.
    pub backoff: BackoffConfig,
    /// Write rate-limit caps.
    pub rate: RateLimitConfig,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_millis(500),
            eval_interval: Duration::from_millis(500),
            backoff: BackoffConfig::default(),
            rate: RateLimitConfig::default(),
        }
    }
}

/// A snapshot of the engine's arm/dry-run state for UI/REST display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineStatus {
    pub armed: bool,
    pub dry_run: bool,
    /// The persisted armed state observed at startup - informational only (the
    /// engine never auto-resumes live writing).
    pub was_armed_before_restart: bool,
}

/// The safe control surface handed to the wiring layer (W3-B2's Tauri commands /
/// REST routes). Cloneable; every arm/disarm/dry-run flip persists to
/// `armed_state` and writes a `write_audit_log` row.
#[derive(Clone)]
pub struct EngineControl {
    pool: SqlitePool,
    arming: std::sync::Arc<ArmingState>,
}

impl EngineControl {
    /// Arm the engine (allow physical writes). Persists + audits.
    pub async fn arm(&self, actor: Option<&str>) -> Result<(), BantoError> {
        self.arming.arm();
        persist_armed(&self.pool, true, actor).await?;
        self.audit_toggle(AuditAction::Arm, actor, "engine armed").await
    }

    /// Disarm the engine (suppress all physical writes). Persists + audits.
    pub async fn disarm(&self, actor: Option<&str>) -> Result<(), BantoError> {
        self.arming.disarm();
        persist_armed(&self.pool, false, actor).await?;
        self.audit_toggle(AuditAction::Disarm, actor, "engine disarmed")
            .await
    }

    /// Turn dry-run on/off. Audits the toggle (dry-run is not part of the
    /// persisted armed state).
    pub async fn set_dry_run(&self, on: bool, actor: Option<&str>) -> Result<(), BantoError> {
        self.arming.set_dry_run(on);
        let detail = if on { "dry-run enabled" } else { "dry-run disabled" };
        self.audit_toggle(AuditAction::DryRunToggle, actor, detail)
            .await
    }

    pub fn is_armed(&self) -> bool {
        self.arming.is_armed()
    }

    pub fn is_dry_run(&self) -> bool {
        self.arming.is_dry_run()
    }

    pub fn status(&self) -> EngineStatus {
        EngineStatus {
            armed: self.arming.is_armed(),
            dry_run: self.arming.is_dry_run(),
            was_armed_before_restart: self.arming.was_armed_before_restart(),
        }
    }

    async fn audit_toggle(
        &self,
        action: AuditAction,
        actor: Option<&str>,
        detail: &str,
    ) -> Result<(), BantoError> {
        let row = AuditRow::new(action, AuditResult::Ok, action.as_str())
            .with_actor(actor)
            .with_detail(detail);
        insert_row(&self.pool, &row).await.map(|_| ())
    }
}

/// The running engine: owns the broker and the two spawned tasks, and the
/// shutdown trigger that stops all of them.
pub struct Engine {
    broker: BrokerSupervisor,
    shutdown_tx: watch::Sender<bool>,
    poller_task: JoinHandle<()>,
    engine_task: JoinHandle<()>,
}

impl Engine {
    /// Build the broker, compile the enabled rules, and start the poller and
    /// evaluate+write tasks. `connections` is the full connection registry;
    /// only `protocol == "slmp"` entries are managed (others are skipped with a
    /// warning). Returns the engine handle plus a [`EngineControl`] for arming.
    pub async fn start(
        pool: SqlitePool,
        connections: Vec<PlcConnection>,
        config: EngineConfig,
    ) -> Result<(Engine, EngineControl), BantoError> {
        // Manage only SLMP connections (this engine writes MELSEC).
        let slmp: Vec<PlcConnection> = connections
            .into_iter()
            .filter(|c| {
                let keep = c.protocol == SLMP_PROTOCOL;
                if !keep {
                    eprintln!(
                        "relay-wright engine: skipping non-SLMP connection {} (protocol={})",
                        c.id, c.protocol
                    );
                }
                keep
            })
            .collect();

        let broker = BrokerSupervisor::spawn(&slmp, config.backoff)
            .map_err(|e| BantoError::Other(e.to_string()))?;

        let managed: HashSet<i64> = slmp.iter().map(|c| c.id).collect();
        let mut read_handles: HashMap<i64, ReadOnlyHandle> = HashMap::new();
        let mut write_handles: HashMap<i64, BrokerHandle> = HashMap::new();
        for id in &managed {
            if let Some(handle) = broker.handle(*id) {
                read_handles.insert(*id, handle.read_only());
                write_handles.insert(*id, handle);
            }
        }

        // Compile the enabled rules against the managed connections.
        let compiled = compile_rules(&pool, &managed).await?;

        let cache = CurrentValues::new();
        let persisted_armed = load_persisted_armed(&pool).await?;
        let arming = std::sync::Arc::new(ArmingState::new(persisted_armed));

        let rule_engine = RuleEngine::new(compiled.rules);
        let writer = Writer::new(
            pool.clone(),
            arming.clone(),
            RateLimiter::new(config.rate),
            write_handles,
            compiled.targets,
        );

        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let poller_task = tokio::spawn(run_poller(
            read_handles,
            compiled.sources_by_conn,
            cache.clone(),
            config.poll_interval,
            shutdown_rx.clone(),
        ));
        let engine_task = tokio::spawn(run_engine_loop(
            rule_engine,
            writer,
            cache,
            config.eval_interval,
            shutdown_rx,
        ));

        let control = EngineControl { pool, arming };
        let engine = Engine {
            broker,
            shutdown_tx,
            poller_task,
            engine_task,
        };
        Ok((engine, control))
    }

    /// Stop the engine cleanly and promptly. Flips the shared shutdown signal so
    /// both engine tasks break out of their `select!` loops, awaits them, then
    /// shuts the broker down. Does NOT hang even if a caller still holds a
    /// control handle (the watch signal is out-of-band from every channel).
    pub async fn shutdown(self) {
        let _ = self.shutdown_tx.send(true);
        let _ = self.poller_task.await;
        let _ = self.engine_task.await;
        self.broker.shutdown().await;
    }
}

/// The evaluate+write task: on each tick, evaluate all rules against the cache
/// and hand every firing intent to the writer (which applies the safety gate).
/// Exits promptly on the shutdown signal.
async fn run_engine_loop(
    mut rule_engine: RuleEngine,
    mut writer: Writer,
    cache: CurrentValues,
    interval: Duration,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = ticker.tick() => {
                let now = std::time::Instant::now();
                let pending: Vec<PendingWrite> = rule_engine.evaluate(&cache, now);
                for write in pending {
                    if let Err(e) = writer.process(write, now).await {
                        eprintln!("relay-wright engine: write audit error: {e}");
                    }
                }
            }
        }
    }
}

/// The compiled output of [`compile_rules`].
struct Compiled {
    rules: Vec<CompiledRule>,
    targets: HashMap<i64, ResolvedTarget>,
    sources_by_conn: HashMap<i64, Vec<ResolvedSource>>,
}

/// Load every ENABLED rule and compile it into the engine's evaluation shape,
/// resolving each source tag and each write target to its wire coordinates. A
/// rule is DROPPED (with a warning) if any of its references cannot be resolved
/// to a managed SLMP connection or its address/type does not parse - a
/// half-resolvable rule must never partially run.
async fn compile_rules(
    pool: &SqlitePool,
    managed: &HashSet<i64>,
) -> Result<Compiled, BantoError> {
    let mut rules = Vec::new();
    let mut targets: HashMap<i64, ResolvedTarget> = HashMap::new();
    let mut sources_by_conn: HashMap<i64, Vec<ResolvedSource>> = HashMap::new();
    let mut polled: HashSet<i64> = HashSet::new();

    let rows: Vec<(i64, String, String, Option<i64>, i64, String, Option<f64>, Option<i64>)> =
        sqlx::query_as(
            "SELECT id, name, edge_mode, cooldown_ms, write_target_id, \
                write_value_mode, write_constant_value, write_source_tag_id \
             FROM write_rules WHERE enabled = 1 ORDER BY id",
        )
        .fetch_all(pool)
        .await
        .map_err(banto_storage::storage_error)?;

    for (id, name, edge_mode, cooldown_ms, write_target_id, value_mode, constant, source_tag) in
        rows
    {
        let Some(edge_mode) = EdgeMode::parse(&edge_mode) else {
            eprintln!("relay-wright engine: rule {id} ({name}) has bad edge_mode; skipped");
            continue;
        };

        // Resolve the target.
        let Some(target) = resolve_target(pool, write_target_id, managed).await? else {
            eprintln!(
                "relay-wright engine: rule {id} ({name}) target {write_target_id} unresolved/non-SLMP; skipped"
            );
            continue;
        };

        // Resolve the value mode.
        let value_mode = match value_mode.as_str() {
            "constant" => match constant {
                Some(v) => ValueMode::Constant(v),
                None => {
                    eprintln!("relay-wright engine: rule {id} ({name}) constant mode without value; skipped");
                    continue;
                }
            },
            "copy_from_source" => match source_tag {
                Some(tag_id) => ValueMode::CopyFromSource(tag_id),
                None => {
                    eprintln!("relay-wright engine: rule {id} ({name}) copy mode without source tag; skipped");
                    continue;
                }
            },
            other => {
                eprintln!("relay-wright engine: rule {id} ({name}) bad write_value_mode {other}; skipped");
                continue;
            }
        };

        // Resolve the conditions. Any unresolved source drops the whole rule.
        let condition_rows: Vec<(i64, String, f64, Option<f64>)> = sqlx::query_as(
            "SELECT source_tag_id, operator, threshold_value, threshold_value_2 \
             FROM write_rule_conditions WHERE write_rule_id = ? ORDER BY id",
        )
        .bind(id)
        .fetch_all(pool)
        .await
        .map_err(banto_storage::storage_error)?;

        let mut conditions = Vec::with_capacity(condition_rows.len());
        let mut resolved_sources: Vec<ResolvedSource> = Vec::new();
        let mut ok = true;
        for (source_tag_id, operator, threshold, threshold_2) in condition_rows {
            let Some(operator) = Operator::parse(&operator) else {
                eprintln!("relay-wright engine: rule {id} ({name}) bad operator; skipped");
                ok = false;
                break;
            };
            let Some(src) = resolve_source(pool, source_tag_id, managed).await? else {
                eprintln!(
                    "relay-wright engine: rule {id} ({name}) source tag {source_tag_id} unresolved/non-SLMP; skipped"
                );
                ok = false;
                break;
            };
            resolved_sources.push(src);
            conditions.push(CompiledCondition {
                source_tag_id,
                operator,
                threshold,
                threshold_2,
            });
        }
        if !ok || conditions.is_empty() {
            continue;
        }

        // A copy-from-source rule also needs its copy tag polled.
        if let ValueMode::CopyFromSource(tag_id) = value_mode {
            let Some(src) = resolve_source(pool, tag_id, managed).await? else {
                eprintln!(
                    "relay-wright engine: rule {id} ({name}) copy source {tag_id} unresolved/non-SLMP; skipped"
                );
                continue;
            };
            resolved_sources.push(src);
        }

        // Commit: register the target, the sources (deduped per connection),
        // and the compiled rule.
        targets.insert(write_target_id, target.clone());
        for src in resolved_sources {
            if polled.insert(src.tag_id) {
                sources_by_conn.entry(src.connection_id).or_default().push(src);
            }
        }
        rules.push(CompiledRule {
            rule_id: id,
            rule_name: name,
            edge_mode,
            cooldown: cooldown_ms
                .filter(|&ms| ms > 0)
                .map(|ms| Duration::from_millis(ms as u64)),
            write_target_id,
            target_data_type: target.data_type,
            value_mode,
            conditions,
        });
    }

    Ok(Compiled {
        rules,
        targets,
        sources_by_conn,
    })
}

/// Resolve a write target id to its wire coordinates, or `None` if it does not
/// exist, is on an unmanaged connection, or its address/type will not parse.
async fn resolve_target(
    pool: &SqlitePool,
    write_target_id: i64,
    managed: &HashSet<i64>,
) -> Result<Option<ResolvedTarget>, BantoError> {
    let row: Option<(i64, String, String)> =
        sqlx::query_as("SELECT plc_connection_id, address, data_type FROM write_targets WHERE id = ?")
            .bind(write_target_id)
            .fetch_optional(pool)
            .await
            .map_err(banto_storage::storage_error)?;
    let Some((connection_id, address, data_type)) = row else {
        return Ok(None);
    };
    if !managed.contains(&connection_id) {
        return Ok(None);
    }
    let (Ok(address), Some(data_type)) = (
        banto_plc::Address::parse_slmp(&address),
        banto_plc::DataType::parse(&data_type),
    ) else {
        return Ok(None);
    };
    Ok(Some(ResolvedTarget {
        connection_id,
        address,
        data_type,
    }))
}

/// Resolve a source tag id to its wire coordinates, or `None` if it does not
/// exist, is on an unmanaged connection, or its address/type will not parse.
async fn resolve_source(
    pool: &SqlitePool,
    tag_id: i64,
    managed: &HashSet<i64>,
) -> Result<Option<ResolvedSource>, BantoError> {
    let row: Option<(i64, String, String)> = sqlx::query_as(
        "SELECT cg.plc_connection_id, t.address, t.data_type \
         FROM tags t JOIN collection_groups cg ON t.collection_group_id = cg.id \
         WHERE t.id = ?",
    )
    .bind(tag_id)
    .fetch_optional(pool)
    .await
    .map_err(banto_storage::storage_error)?;
    let Some((connection_id, address, data_type)) = row else {
        return Ok(None);
    };
    if !managed.contains(&connection_id) {
        return Ok(None);
    }
    let (Ok(address), Some(data_type)) = (
        banto_plc::Address::parse_slmp(&address),
        banto_plc::DataType::parse(&data_type),
    ) else {
        return Ok(None);
    };
    Ok(Some(ResolvedSource {
        tag_id,
        connection_id,
        address,
        data_type,
    }))
}
