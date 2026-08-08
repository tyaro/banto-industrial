//! [`Collector`]: the running engine. Opens the time-series store, spawns one
//! task per connection, and exposes the three things the UI consumes - the
//! current-value cache, per-connection status, and the live event stream -
//! plus a clean [`Collector::stop`] that drains every task and flushes the
//! writer so no buffered row is lost.
//!
//! ## T7-1: online partial reconfiguration (docs/tag-server-design.md §4.3)
//!
//! > 実現の土台は現行アーキテクチャに既にある: `banto-collect` は**接続毎に
//! > 1タスク**なので、(c) の「接続単位の入れ替え」は自然な粒度。必要なのは
//! > Collector 全体でなく**接続単位の部分再構成 API**... 適用は**編集
//! > トランザクション単位の all-or-nothing**: 検証を通った変更だけが
//! > revision を進める。中途半端な構成が外部へ見える瞬間を作らない。
//!
//! [`Collector::apply_config`] is that API. Its contract is "the influence
//! radius of a config change is exactly the connections that changed" - an
//! unchanged connection's task is never stopped, never respawned, and its
//! collection never so much as blips, no matter what else in the config
//! changed. Three structural changes make that possible, each mirroring a
//! constraint discovered while auditing the pre-T7-1 code (see this task's
//! completion report for the full derivation):
//!
//! 1. **Per-connection stop, not a shared one.** `Collector::tasks`
//!    is keyed by connection key, each entry owning its own
//!    `watch::Sender<bool>` - stopping connection B can never touch A's
//!    channel. [`Collector::stop`] simply signals and joins every entry.
//! 2. **The writer is a `watch` channel, not a bare `Arc`.** Every task holds
//!    a `watch::Receiver<Arc<TsWriter>>` (via `TaskContext::writer_rx`) and
//!    re-borrows it fresh on every append (`task.rs::record_group`) instead
//!    of caching the `Arc` for its lifetime. This is what lets `apply_config`
//!    rotate the writer - which it must do whenever the *aggregate* collected
//!    tag/group set changes, because `banto-tstore`'s frozen-schema design
//!    means any such change needs a new file - **without stopping a single
//!    task**: every live task (changed or not) simply starts writing to the
//!    new file on its very next tick.
//! 3. **`Collector` retains the [`CollectorConfig`] it is currently running**,
//!    so a later `apply_config` call has something to diff the caller's new
//!    snapshot against. The stored config is the *pristine* one the caller
//!    passed in - never the per-run [`CollectorOptions`] timeout overrides
//!    `plan_for_task` bakes into the plan a task actually runs with. Baking
//!    those in before storing would make an otherwise-untouched connection
//!    compare unequal on the next call (a freshly rebuilt `CollectorConfig`
//!    never carries them), spuriously reclassifying it as "changed" and
//!    defeating the entire point of the diff.
//!
//! ### Why the steps inside `apply_config` run in this exact order
//!
//! 1. **Diff first** (pure, no side effects) - classifies every connection
//!    key into added/removed/replaced/unchanged by comparing
//!    `crate::config::ConnectionPlan` equality (now `PartialEq` end to end
//!    - see `config.rs`).
//! 2. **Open the new writer, if needed, before touching anything else.**
//!    This is the all-or-nothing anchor: `TsWriter::open_with_options` either
//!    succeeds (and every later step proceeds) or fails and returns `Err`
//!    immediately, with not one task stopped and `self.config` untouched -
//!    the caller sees the collector in exactly the state it was in before
//!    calling. Every step after this point is expected to succeed (stopping
//!    an already-running task, spawning a new one) and is not wrapped in the
//!    same "roll everything back on failure" discipline; the design's
//!    all-or-nothing guarantee is specifically about the storage-schema
//!    commit point, not the whole operation being transactional in the
//!    database-ACID sense.
//! 3. **Stop and join removed/replaced connections' tasks.** Done before the
//!    writer is redistributed so that a connection whose *plan* changed
//!    never gets a chance to read stale data with its old task.
//! 4. **Distribute the new writer, then retire the old one.** Must happen
//!    *before* step 5 (spawning new tasks): `TsWriter::append`'s
//!    unknown-group error never stops the hot loop
//!    (`task.rs::record_group`; H4, 2026-08-08 owner decision,
//!    docs/improvement-plan.md - the failure itself is now recorded, an
//!    `eprintln!` plus an `append_failure_entered`/`append_failure_cleared`
//!    `collect_events` pair, but is still never fatal, by design), so a
//!    newly spawned task reading a brand-new group must never see the *old*
//!    writer, which has no schema for that group at all - every row it tried
//!    to write would still be permanently lost (no retry, no backfill),
//!    merely no longer silently so. Retiring the old writer reuses `stop`'s own
//!    `Arc::try_unwrap`-or-flush fallback (`close_or_flush_writer`): an
//!    unchanged connection may be mid-append on the old writer at the exact
//!    moment of rotation, so failing to get sole ownership here is an
//!    expected occasional outcome, not a bug - either branch guarantees no
//!    buffered row is silently dropped, which is the invariant that matters.
//! 5. **Spawn tasks for added/replaced connections** - now guaranteed to
//!    subscribe to a writer that already knows about every group they read.
//! 6. **`retain` the current-value cache and status map** down to the new
//!    config's live tag/connection keys, so a removed tag or connection does
//!    not linger forever with a slowly-staling last-known value.
//! 7. **Adopt `new_config` as `self.config`** - the point of no return, done
//!    last so every earlier step could still consult the *old* config.
//!
//! Deliberately unchanged connections are never touched by any of steps
//! 2-6 above other than the writer broadcast in step 4, which they observe
//! passively (re-borrowing on their own next tick) - this is the whole
//! mechanism that keeps their collection running without interruption.
//!
//! ### T9-1 addendum: simulated connections (docs/ux-plan.md §1)
//!
//! A connection whose [`crate::config::ConnectionPlan::simulation`] is `true`
//! gets an in-process simulator ([`crate::simulation`]) instead of dialing
//! its real host/port - started/stopped in exact lockstep with that
//! connection's task, at every point this module already stops or spawns
//! one: step 3 above (stop removed/replaced) also stops that connection's
//! simulator if it has one, and step 5 (spawn added/replaced) also starts
//! one before spawning if the *new* plan wants simulation. Because
//! `simulation` participates in `ConnectionPlan`'s `PartialEq`, flipping it
//! on or off with nothing else changed already makes the diff in step 1
//! classify the connection "replaced" - no separate branch was needed
//! anywhere in this method for "did simulation just turn on/off", the
//! ordinary stop-then-spawn path *is* the simulator's stop-then-start. See
//! `crate::simulation`'s module doc for why the address substitution itself
//! happens here (at task-spawn time) rather than in `crate::config::build_config`.
//!
//! ### Why no `collection_started`/`collection_stopped` event
//!
//! Those two [`EventKind`] variants mean "the whole engine started/stopped"
//! (docs/recorder-requirements.md §3.5) - `apply_config` is neither; it is a
//! reconfiguration of a collector that is already running and remains
//! running. Emitting either would mislead a UI/audit log into thinking
//! collection paused. The connection-scoped lifecycle events
//! (`plc_connected`/`plc_disconnected`/`plc_reconnected`) still flow
//! naturally from whichever tasks actually get spawned/stopped - those are a
//! true per-connection fact, not a synthetic signal this method needs to
//! fabricate.
//!
//! ### The empty-config case
//!
//! `apply_config` never rejects `new_config.connections.is_empty()` as an
//! error the way [`Collector::start_with_client_factory`] does - a running
//! collector must be able to shrink to nothing without the caller having to
//! special-case "was this the last connection, so call `stop()` instead".
//! Every existing task is classified `removed` and stopped/joined normally;
//! no new writer is opened (there is nothing to open one *for* - a
//! `StoreConfig` with zero groups fails `TsWriter`'s own validation, and
//! rightly so), and the still-open writer is flushed (not closed - the
//! collector may still be `apply_config`'d back to a non-empty state later)
//! once no task remains to trigger a flush on its own.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use banto_tstore::{Clock, TsWriter, WriterOptions};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

use crate::config::{CollectorConfig, ConnectionPlan, Protocol, ProtocolConfig};
use crate::current::CurrentValuesHandle;
use crate::error::CollectError;
use crate::event::{CollectEvent, EventKind, EventSink};
use crate::simulation::{self, SimulatorHandle};
use crate::task::{
    default_client_factory, retain_status, run_connection, BackoffConfig, ClientFactory,
    ConnectionStatus, StatusMap, TaskContext,
};

/// Tunables for [`Collector::start`]. `Default` matches the product defaults
/// (recorder-requirements.md / plan.md I2 §2): 1s/30s backoff, 3s connect /
/// 1s response timeouts, and `banto-tstore`'s default buffering. Tests shrink
/// these for fast, deterministic runs.
#[derive(Debug, Clone, Copy)]
pub struct CollectorOptions {
    pub backoff: BackoffConfig,
    /// Applied to every connection's client, overriding the build-time
    /// default - lets tests use sub-second connect timeouts.
    pub connect_timeout: Duration,
    /// Applied to every connection's client, overriding the build-time
    /// default.
    pub response_timeout: Duration,
    pub writer_options: WriterOptions,
}

impl Default for CollectorOptions {
    fn default() -> Self {
        Self {
            backoff: BackoffConfig::default(),
            connect_timeout: Duration::from_secs(3),
            response_timeout: Duration::from_secs(1),
            writer_options: WriterOptions::default(),
        }
    }
}

/// One running connection's task handle plus the stop channel that only it
/// listens to (T7-1: replaces the old single collector-wide `stop_tx`, which
/// made a connection-scoped stop structurally impossible).
struct ConnectionTask {
    handle: JoinHandle<()>,
    stop_tx: watch::Sender<bool>,
}

/// What changed in one [`Collector::apply_config`] call - connection keys
/// grouped by how they were classified, plus whether the tstore writer
/// rotated to a new file. Every `Vec<String>` is sorted for deterministic
/// assertions in tests and stable logging.
#[derive(Debug, Clone, Default)]
pub struct ApplyReport {
    /// Connection keys present in the new config but not the old one - a new
    /// task was spawned for each.
    pub added: Vec<String>,
    /// Connection keys present in the old config but not the new one - each
    /// had its task stopped and joined.
    pub removed: Vec<String>,
    /// Connection keys present in both, but whose `crate::config::ConnectionPlan`
    /// differs (protocol config and/or groups/tags) - each had its old task
    /// stopped and joined, then a fresh task spawned from the new plan.
    pub replaced: Vec<String>,
    /// Connection keys present in both configs with byte-for-byte identical
    /// plans - their tasks were never touched.
    pub unchanged: Vec<String>,
    /// Whether the collected tag/group set changed enough to require a fresh
    /// `banto-tstore` file (a new `TsWriter` was opened and distributed via
    /// [`Collector`]'s writer `watch` channel). Independent of the
    /// added/removed/replaced/unchanged classification above - see
    /// `apply_config`'s doc comment (settings-only connection edits, e.g. a
    /// host/port change, replace the connection but never rotate the
    /// writer).
    pub writer_rotated: bool,
}

/// A running collection engine. Self-driving after [`Collector::start`]
/// (recorder-requirements.md §4: runs independently of the UI); hold it for
/// the process lifetime and call [`Collector::stop`] to shut down cleanly.
pub struct Collector {
    /// The live writer, broadcast to every connection task (T7-1: see this
    /// module's doc comment for why a `watch` channel rather than a bare
    /// `Arc<TsWriter>` field).
    writer_tx: watch::Sender<Arc<TsWriter>>,
    current: CurrentValuesHandle,
    status: StatusMap,
    events: EventSink,
    clock: Arc<dyn Clock>,
    /// Needed by [`Collector::apply_config`] to reopen a rotated writer at
    /// the same location `start_with_client_factory` originally used.
    data_dir: PathBuf,
    /// Retained so `apply_config` can spawn added/replaced connections with
    /// the exact same backoff/timeout/buffering tuning `start` was called
    /// with, without the caller having to pass it again on every call.
    options: CollectorOptions,
    /// The pristine [`CollectorConfig`] currently applied - `apply_config`'s
    /// diff base. "Pristine" matters: see this module's doc comment point 3.
    config: CollectorConfig,
    tasks: HashMap<String, ConnectionTask>,
    /// T9-1 (this module's doc comment, "T9-1 addendum"): one entry per
    /// connection key currently running with `simulation = true`, owning that
    /// connection's in-process simulator + ramp-wave generator task. Kept in
    /// lockstep with `tasks` - never touched for a connection classified
    /// "unchanged" by `apply_config`'s diff, added/removed/replaced exactly
    /// when the corresponding task is.
    simulators: HashMap<String, SimulatorHandle>,
}

impl Collector {
    /// Open the store under `data_dir` (schema frozen from `config`), then
    /// spawn a task per connection and start collecting. Emits
    /// `collection_started`.
    ///
    /// `clock` is shared with the store (rotation) and the cache (staleness)
    /// so every "now" agrees; in production pass `Arc::new(SystemClock)`, in
    /// tests a `ManualClock`. Fails with [`CollectError::Config`] if `config`
    /// has no connections (nothing to collect) or [`CollectError::Tstore`] if
    /// the store cannot be opened.
    ///
    /// Delegates to [`Self::start_with_client_factory`] with
    /// [`default_client_factory`] - every existing caller keeps building the
    /// same `ModbusTcpClient`/`SlmpClient` it always has (T2-2,
    /// docs/tag-server-design.md §6-5: "既存呼び出し互換維持").
    pub async fn start(
        config: CollectorConfig,
        data_dir: &Path,
        clock: Arc<dyn Clock>,
        events: EventSink,
        options: CollectorOptions,
    ) -> Result<Self, CollectError> {
        Self::start_with_client_factory(
            config,
            data_dir,
            clock,
            events,
            options,
            default_client_factory(),
        )
        .await
    }

    /// Identical to [`Self::start`], but the `PlcClient` each connection task
    /// reconnects with comes from `factory` instead of the hardcoded
    /// Modbus/SLMP construction (T2-2, docs/tag-server-design.md §6-5: "hub
    /// が SLMP 接続の読み取りを broker アダプタへ差し替えるための注入口。既定は
    /// 従来どおりの直接クライアント"). banto-hub's `CollectorManager` calls this
    /// directly with a factory that routes SLMP connections through
    /// `banto_broker` (see `apps/banto-hub/core/src/broker_glue.rs`) while
    /// leaving Modbus connections on the default client - see
    /// [`crate::task::ClientFactory`]'s doc comment for the seam's exact
    /// contract (called once per connect attempt, receives a [`crate::task::ClientSpec`]
    /// with every field the pre-T2-2 hardcoded dispatch used).
    pub async fn start_with_client_factory(
        config: CollectorConfig,
        data_dir: &Path,
        clock: Arc<dyn Clock>,
        events: EventSink,
        options: CollectorOptions,
        factory: ClientFactory,
    ) -> Result<Self, CollectError> {
        if config.connections.is_empty() {
            return Err(CollectError::Config(
                "収集対象がありません（有効な接続・グループ・タグを設定してください）".to_string(),
            ));
        }

        let writer = Arc::new(
            TsWriter::open_with_options(
                data_dir,
                config.store_config.clone(),
                clock.clone(),
                options.writer_options,
            )
            .await?,
        );
        let (writer_tx, _writer_rx) = watch::channel(writer);

        let current = CurrentValuesHandle::new(clock.clone());
        let status: StatusMap = Arc::new(RwLock::new(HashMap::new()));

        events
            .emit(CollectEvent::lifecycle(
                clock.now_ms(),
                EventKind::CollectionStarted,
            ))
            .await;

        let mut tasks = HashMap::with_capacity(config.connections.len());
        let mut simulators: HashMap<String, SimulatorHandle> = HashMap::new();
        for plan in &config.connections {
            let sim_addr = if plan.simulation {
                let sim = simulation::start(protocol_of(plan)).await;
                let addr = sim.addr();
                simulators.insert(plan.key.clone(), sim);
                Some(addr)
            } else {
                None
            };
            let ctx = TaskContext {
                writer_rx: writer_tx.subscribe(),
                clock: clock.clone(),
                current: current.clone(),
                events: events.clone(),
                status: status.clone(),
                backoff: options.backoff,
                factory: factory.clone(),
            };
            let (stop_tx, stop_rx) = watch::channel(false);
            let handle = tokio::spawn(run_connection(
                plan_for_task(plan, &options, sim_addr),
                ctx,
                stop_rx,
            ));
            tasks.insert(plan.key.clone(), ConnectionTask { handle, stop_tx });
        }

        Ok(Self {
            writer_tx,
            current,
            status,
            events,
            clock,
            data_dir: data_dir.to_path_buf(),
            options,
            config,
            tasks,
            simulators,
        })
    }

    /// Apply a new configuration to an already-running collector, touching
    /// only what changed (T7-1, docs/tag-server-design.md §4.3 - "変更の
    /// 影響半径 = 触ったものだけ"). See this module's doc comment for the full
    /// safety derivation; in short:
    ///
    /// - An unchanged connection's task is never stopped or respawned, and
    ///   never observes so much as a blip - not even if the tstore writer
    ///   rotates underneath it.
    /// - The tstore writer only rotates (a fresh file) when the *aggregate*
    ///   collected tag/group set actually changes - a connection whose only
    ///   edit is host/port (or any other setting that does not touch its
    ///   groups/tags) is replaced without a writer rotation.
    /// - Opening the new writer (when one is needed) is attempted *before*
    ///   any task is touched, so a failure here (e.g. an unwritable
    ///   `data_dir`) leaves the collector in exactly its prior state -
    ///   `self.config` unchanged, every task still running, `Err` returned.
    /// - `factory` is used only for tasks this call spawns (added/replaced
    ///   connections); already-running unchanged tasks keep whatever factory
    ///   they were originally spawned with.
    pub async fn apply_config(
        &mut self,
        new_config: CollectorConfig,
        factory: ClientFactory,
    ) -> Result<ApplyReport, CollectError> {
        // --- 1. Diff (pure - no side effects yet) ---------------------------
        let current_map: HashMap<&str, &ConnectionPlan> = self
            .config
            .connections
            .iter()
            .map(|c| (c.key.as_str(), c))
            .collect();
        let new_map: HashMap<&str, &ConnectionPlan> = new_config
            .connections
            .iter()
            .map(|c| (c.key.as_str(), c))
            .collect();

        let mut added: Vec<String> = Vec::new();
        let mut replaced: Vec<String> = Vec::new();
        let mut unchanged: Vec<String> = Vec::new();
        for (key, new_plan) in &new_map {
            match current_map.get(key) {
                None => added.push((*key).to_string()),
                Some(cur_plan) => {
                    if cur_plan == new_plan {
                        unchanged.push((*key).to_string());
                    } else {
                        replaced.push((*key).to_string());
                    }
                }
            }
        }
        let mut removed: Vec<String> = current_map
            .keys()
            .filter(|key| !new_map.contains_key(*key))
            .map(|key| (*key).to_string())
            .collect();
        added.sort();
        replaced.sort();
        unchanged.sort();
        removed.sort();

        // --- 2. Writer rotation gate (all-or-nothing anchor) ---------------
        // A `StoreConfig` with zero groups fails `TsWriter`'s own validation
        // (and rightly so - there is nothing to open a schema *for*), which
        // is exactly the empty-config case this method must accept rather
        // than error on (see this module's doc comment). Skip attempting to
        // open in that case; the still-open old writer is flushed near the
        // end of this method once no task remains to use it.
        let writer_rotated = !new_config.store_config.groups.is_empty()
            && new_config.store_config != self.config.store_config;
        let new_writer = if writer_rotated {
            Some(Arc::new(
                TsWriter::open_with_options(
                    &self.data_dir,
                    new_config.store_config.clone(),
                    self.clock.clone(),
                    self.options.writer_options,
                )
                .await?,
            ))
        } else {
            None
        };

        // --- 3. Stop + join removed/replaced connections' tasks only -------
        // T9-1: a removed/replaced connection's simulator (if any) is stopped
        // right alongside its task - see this module's doc comment, "T9-1
        // addendum". Stopped after the task itself (the task may still be
        // mid-`read_batch` against it up to the moment it joins).
        for key in removed.iter().chain(replaced.iter()) {
            if let Some(task) = self.tasks.remove(key) {
                let _ = task.stop_tx.send(true);
                let _ = task.handle.await;
            }
            if let Some(sim) = self.simulators.remove(key) {
                sim.stop().await;
            }
        }

        // --- 4. Distribute the new writer, then retire the old one ---------
        if let Some(new_writer) = new_writer {
            let old_writer = self.writer_tx.borrow().clone();
            let _ = self.writer_tx.send(new_writer);
            close_or_flush_writer(old_writer).await?;
        }

        // --- 5. Spawn tasks for added/replaced connections ------------------
        // T9-1: start a fresh simulator first (if the new plan wants one) so
        // its address is ready before the task that will read from it spawns
        // - see this module's doc comment, "T9-1 addendum".
        for key in added.iter().chain(replaced.iter()) {
            let plan = new_map[key.as_str()];
            let sim_addr = if plan.simulation {
                let sim = simulation::start(protocol_of(plan)).await;
                let addr = sim.addr();
                self.simulators.insert(key.clone(), sim);
                Some(addr)
            } else {
                None
            };
            let ctx = TaskContext {
                writer_rx: self.writer_tx.subscribe(),
                clock: self.clock.clone(),
                current: self.current.clone(),
                events: self.events.clone(),
                status: self.status.clone(),
                backoff: self.options.backoff,
                factory: factory.clone(),
            };
            let (stop_tx, stop_rx) = watch::channel(false);
            let handle = tokio::spawn(run_connection(
                plan_for_task(plan, &self.options, sim_addr),
                ctx,
                stop_rx,
            ));
            self.tasks
                .insert(key.clone(), ConnectionTask { handle, stop_tx });
        }

        // --- 6. Retain cache/status down to the new config's live keys ------
        let live_tag_keys: HashSet<String> = new_config
            .connections
            .iter()
            .flat_map(|c| c.groups.iter())
            .flat_map(|g| g.tags.iter())
            .map(|t| t.key.clone())
            .collect();
        self.current.retain(&live_tag_keys);
        let live_conn_keys: HashSet<String> = new_config
            .connections
            .iter()
            .map(|c| c.key.clone())
            .collect();
        retain_status(&self.status, &live_conn_keys);

        // Nothing left running to ever flush again on its own - push
        // whatever is buffered to disk now rather than leaving it stranded
        // (the empty-config case from this module's doc comment).
        if self.tasks.is_empty() {
            let writer = self.writer_tx.borrow().clone();
            writer.flush().await?;
        }

        // --- 7. Adopt the new config (point of no return) -------------------
        self.config = new_config;

        Ok(ApplyReport {
            added,
            removed,
            replaced,
            unchanged,
            writer_rotated,
        })
    }

    /// A cloneable handle onto the live current-value cache
    /// (recorder-requirements.md §3.2's digital/bar/gauge + health display).
    pub fn current_values(&self) -> CurrentValuesHandle {
        self.current.clone()
    }

    /// A point-in-time snapshot of every connection's status
    /// (recorder-requirements.md §5 health display).
    pub fn status(&self) -> HashMap<String, ConnectionStatus> {
        self.status
            .read()
            .expect("status map lock poisoned")
            .clone()
    }

    /// Subscribe a live event consumer (UI feed). Each subscriber sees events
    /// emitted after it subscribes; the durable `collect_events` table holds
    /// the full history.
    pub fn subscribe_events(&self) -> broadcast::Receiver<CollectEvent> {
        self.events.subscribe()
    }

    /// Stop every connection task, then flush and close the store so no
    /// buffered row is lost (recorder-requirements.md: "stop() で writer が
    /// flush され行が失われない"). Emits `collection_stopped` last. Consumes
    /// `self`.
    pub async fn stop(mut self) -> Result<(), CollectError> {
        // Signal, then drain every task. Each task closes its own socket and
        // marks itself Stopped on the way out, releasing its writer_rx (and
        // any transient per-append writer clone) with it.
        for (_key, task) in self.tasks.drain() {
            let _ = task.stop_tx.send(true);
            let _ = task.handle.await;
        }

        // T9-1: every remaining simulator goes down after its task (this
        // module's doc comment, "T9-1 addendum") - by construction there is
        // one entry here only for a connection that was still running with
        // simulation=true.
        for (_key, sim) in self.simulators.drain() {
            sim.stop().await;
        }

        // Every task has now dropped its writer handle. Grab our own clone of
        // the current writer, then drop the `Sender` itself (the last
        // remaining holder of the channel's internal copy) so `writer` below
        // is - barring an unexpected straggler, see `close_or_flush_writer`'s
        // doc comment - the sole reference.
        let writer = self.writer_tx.borrow().clone();
        drop(self.writer_tx);
        close_or_flush_writer(writer).await?;

        self.events
            .emit(CollectEvent::lifecycle(
                self.clock.now_ms(),
                EventKind::CollectionStopped,
            ))
            .await;
        Ok(())
    }
}

/// Clone `plan` and apply this run's uniform connect/response-timeout
/// overrides (`options`) to its protocol config - the same per-protocol
/// `match` [`Collector::start_with_client_factory`] always applied inline,
/// extracted so [`Collector::apply_config`] can build an identical
/// "task-ready" plan for a newly added/replaced connection without mutating
/// the pristine plan [`Collector::config`] stores for future diffing (see
/// this module's doc comment, point 3, for why that distinction matters).
///
/// T9-1: `sim_addr` is `Some` exactly when `plan.simulation` is true (the
/// caller already started the simulator to get this address) - when present,
/// it overrides `host`/`port` in the returned plan's protocol config, same as
/// the timeout override above. The pristine `plan` passed in is never
/// mutated, for the same reason the timeout override isn't baked into it
/// either (this module's doc comment, point 3): a freshly rebuilt
/// `CollectorConfig` always carries the registry's real host/port, so if the
/// substituted address leaked into `self.config`, an otherwise-untouched
/// simulated connection would spuriously compare unequal on the next
/// `apply_config` call.
fn plan_for_task(
    plan: &ConnectionPlan,
    options: &CollectorOptions,
    sim_addr: Option<SocketAddr>,
) -> ConnectionPlan {
    let mut plan = plan.clone();
    match &mut plan.config {
        ProtocolConfig::ModbusTcp(cfg) => {
            cfg.connect_timeout = options.connect_timeout;
            cfg.response_timeout = options.response_timeout;
            if let Some(addr) = sim_addr {
                cfg.host = addr.ip().to_string();
                cfg.port = addr.port();
            }
        }
        ProtocolConfig::Slmp(cfg) => {
            cfg.connect_timeout = options.connect_timeout;
            cfg.response_timeout = options.response_timeout;
            if let Some(addr) = sim_addr {
                cfg.host = addr.ip().to_string();
                cfg.port = addr.port();
            }
        }
    }
    plan
}

/// Project a [`ConnectionPlan`]'s protocol config down to the
/// [`crate::config::Protocol`] [`crate::simulation::start`] dispatches on -
/// the two are always in lockstep (`ProtocolConfig::ModbusTcp` iff
/// `Protocol::ModbusTcp`, built from the same `build_config` match arm), so
/// this is a plain projection, never a guess.
fn protocol_of(plan: &ConnectionPlan) -> Protocol {
    match &plan.config {
        ProtocolConfig::ModbusTcp(_) => Protocol::ModbusTcp,
        ProtocolConfig::Slmp(_) => Protocol::Slmp,
    }
}

/// Retire one writer `Arc`: close (final flush + pool shutdown) if we hold
/// the sole reference, otherwise fall back to a bare flush - the same
/// `Arc::try_unwrap`-or-flush fallback [`Collector::stop`] has always used
/// ("Should be unreachable... every task joined above"), reused here for
/// writer rotation inside [`Collector::apply_config`]: an unchanged
/// connection's task may be mid-`append` on the *old* writer at the exact
/// moment of rotation (it re-borrows fresh every tick, but a borrow already
/// in flight holds its clone until that one `append` call returns), so
/// failing to unwrap here is an expected occasional outcome, not a bug.
/// Either branch guarantees no buffered row is silently lost, which is the
/// invariant that matters - which branch runs is secondary.
async fn close_or_flush_writer(writer: Arc<TsWriter>) -> Result<(), CollectError> {
    match Arc::try_unwrap(writer) {
        Ok(writer) => writer.close().await.map_err(CollectError::from),
        Err(still_shared) => still_shared.flush().await.map_err(CollectError::from),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ConnectionPlan, GroupPlan, TagPlan, Thresholds};
    use banto_plc::{
        Address, BoxFuture, DataType, ModbusTcpConfig, PlcClient, PlcError, ReadRequest,
        ReadResult, TagValue,
    };
    use banto_tstore::{GroupConfig, StoreConfig, SystemClock, TagColumn};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A retry-cleanup temp dir wrapper (2026-08-08 audit finding): every
    /// `Collector` a test here starts writes real, file-backed tstore data
    /// files into this directory (`EventSink`'s registry pool may be
    /// `:memory:`, but the writer's own data files never are), each opening
    /// its own WAL-mode `SqlitePool`. On Windows, closing such a pool does
    /// not synchronously release its file handles - even after
    /// `collector.stop().await` (which flushes/closes the writer) returns,
    /// the OS can hold the file "in use" for a short additional window - so
    /// plain `tempfile::TempDir`'s single, non-retrying `remove_dir_all`
    /// (which also silently swallows its error) left a directory behind on
    /// most runs. See `apps/banto-hub/core/tests/common/mod.rs`'s module
    /// doc for the fuller writeup and measurements (this crate's own
    /// `tests/integration.rs::TempEnv` had the identical issue - already
    /// fixed there; this is the same fix applied to this crate's unit
    /// tests). Requires a multi-thread tokio runtime with >= 2 workers,
    /// same reasoning as that module - already true for every test below.
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir").keep();
            Self(dir)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    const TEMP_DIR_RETRY_DELAY: Duration = Duration::from_millis(50);
    const TEMP_DIR_MAX_ATTEMPTS: u32 = 40;

    impl Drop for TempDir {
        fn drop(&mut self) {
            for attempt in 1..=TEMP_DIR_MAX_ATTEMPTS {
                match std::fs::remove_dir_all(&self.0) {
                    Ok(()) => return,
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
                    Err(_) if attempt < TEMP_DIR_MAX_ATTEMPTS => {
                        std::thread::sleep(TEMP_DIR_RETRY_DELAY);
                    }
                    Err(err) => {
                        eprintln!(
                            "TempDir: giving up removing {:?} after {attempt} attempts: {err}",
                            self.0
                        );
                    }
                }
            }
        }
    }

    /// A fake `PlcClient` that connects instantly (no socket) and answers
    /// every request with a fixed sentinel, counting how many times
    /// `read_batch` ran - proof a test can drive the collection loop
    /// end-to-end through [`Collector::start_with_client_factory`] without a
    /// real PLC. `host`/`port` in the test's [`CollectorConfig`] point at an
    /// address nothing listens on, so a run that used the *default* factory
    /// (`ModbusTcpClient`) would never connect and this test would time out -
    /// that is exactly what pins down "the factory was actually used", not
    /// merely accepted and ignored.
    struct FakeClient {
        reads: Arc<AtomicUsize>,
    }

    impl PlcClient for FakeClient {
        fn connect(&mut self) -> BoxFuture<'_, Result<(), PlcError>> {
            Box::pin(async { Ok(()) })
        }

        fn read_batch<'a>(
            &'a mut self,
            requests: &'a [ReadRequest],
        ) -> BoxFuture<'a, Result<Vec<ReadResult>, PlcError>> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                Ok(requests
                    .iter()
                    .map(|_| ReadResult::Value(TagValue::F64(42.0)))
                    .collect())
            })
        }

        fn disconnect(&mut self) -> BoxFuture<'_, ()> {
            Box::pin(async {})
        }
    }

    /// One connection, one group, one tag - just enough for
    /// `Collector::start_with_client_factory` to spawn a single task and open
    /// a store. `config`'s `host`/`port` are a throwaway loopback address
    /// nothing listens on (see [`FakeClient`]'s doc comment for why that
    /// matters to the test).
    fn one_tag_config() -> CollectorConfig {
        let requests = vec![ReadRequest {
            address: Address::parse("40001").expect("valid modbus address"),
            data_type: DataType::I16,
        }];
        let tags = vec![TagPlan {
            key: "tag:1".to_string(),
            scaling: None,
            thresholds: Thresholds::default(),
        }];
        let group = GroupPlan {
            key: "grp:1".to_string(),
            period: Duration::from_millis(20),
            period_ms: 20,
            requests,
            tags,
        };
        let conn = ConnectionPlan {
            key: "conn:1".to_string(),
            config: ProtocolConfig::ModbusTcp(ModbusTcpConfig {
                host: "127.0.0.1".to_string(),
                port: 1, // reserved, nothing binds here - see FakeClient's doc comment
                ..ModbusTcpConfig::default()
            }),
            groups: vec![group],
            simulation: false,
        };
        CollectorConfig {
            connections: vec![conn],
            store_config: StoreConfig {
                groups: vec![GroupConfig {
                    key: "grp:1".to_string(),
                    name: "G1".to_string(),
                    period_ms: 20,
                    tags: vec![TagColumn {
                        key: "tag:1".to_string(),
                        name: "t1".to_string(),
                        data_type: "i16".to_string(),
                        unit: None,
                        decimals: 0,
                    }],
                }],
            },
        }
    }

    /// T2-2 (docs/tag-server-design.md §6-5): a caller-supplied
    /// [`crate::task::ClientFactory`] is what the connection task actually
    /// reconnects with, not the built-in `ModbusTcpClient`/`SlmpClient`
    /// dispatch - proven by pointing `CollectorConfig` at an address nothing
    /// listens on and observing the cache fill anyway (only possible if the
    /// injected [`FakeClient`] served the reads).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_with_client_factory_uses_the_injected_client() {
        let dir = TempDir::new();
        let reads = Arc::new(AtomicUsize::new(0));
        let reads_for_factory = reads.clone();
        let factory: ClientFactory = Arc::new(move |_spec| {
            Box::new(FakeClient {
                reads: reads_for_factory.clone(),
            }) as Box<dyn PlcClient>
        });

        let pool = banto_storage::connect_sqlite_memory()
            .await
            .expect("connect sqlite memory");
        let collector = Collector::start_with_client_factory(
            one_tag_config(),
            dir.path(),
            Arc::new(SystemClock),
            EventSink::new(pool),
            CollectorOptions {
                connect_timeout: Duration::from_millis(200),
                response_timeout: Duration::from_millis(200),
                ..CollectorOptions::default()
            },
            factory,
        )
        .await
        .expect("start_with_client_factory should succeed");

        let current = collector.current_values();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            if current.get("tag:1").and_then(|s| s.value) == Some(42.0) {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the injected FakeClient's sentinel value never showed up in the cache - \
                 the default (unreachable) ModbusTcpClient must have been used instead"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            reads.load(Ordering::SeqCst) > 0,
            "the factory's FakeClient should have served at least one read_batch call"
        );

        collector.stop().await.expect("stop should succeed");
    }

    /// Same shape as [`one_tag_config`], but `simulation: true` and pointed at
    /// a host/port nothing could ever answer on (`203.0.113.1` is a
    /// TEST-NET-3 address, RFC 5737 - guaranteed unroutable, not merely
    /// "nothing listens here"). If `Collector` did not substitute the
    /// simulator's loopback address for this before spawning the task, the
    /// connect attempt would simply never succeed within the test's
    /// deadline - the same proof-by-unreachable-address technique
    /// `start_with_client_factory_uses_the_injected_client` uses for the
    /// `ClientFactory` seam, applied here to the T9-1 substitution instead.
    fn one_tag_config_simulated() -> CollectorConfig {
        let mut config = one_tag_config();
        let conn = &mut config.connections[0];
        conn.simulation = true;
        match &mut conn.config {
            ProtocolConfig::ModbusTcp(cfg) => {
                cfg.host = "203.0.113.1".to_string();
                cfg.port = 502;
            }
            ProtocolConfig::Slmp(_) => unreachable!("one_tag_config always builds ModbusTcp"),
        }
        config
    }

    /// T9-1 (docs/ux-plan.md §1) end to end: a `simulation: true` connection,
    /// started through the *default* client factory (real `ModbusTcpClient`,
    /// no injected fake - unlike the test above), still collects live,
    /// changing values - proof that `Collector` actually started an
    /// in-process simulator and pointed the task at it, and that the
    /// ramp-wave generator (`crate::simulation`) is really mutating its
    /// registers over time, not just seeding a fixed value once.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_simulated_connection_collects_live_changing_values() {
        let dir = TempDir::new();
        let pool = banto_storage::connect_sqlite_memory()
            .await
            .expect("connect sqlite memory");
        let collector = Collector::start(
            one_tag_config_simulated(),
            dir.path(),
            Arc::new(SystemClock),
            EventSink::new(pool),
            CollectorOptions {
                connect_timeout: Duration::from_millis(300),
                response_timeout: Duration::from_millis(300),
                ..CollectorOptions::default()
            },
        )
        .await
        .expect("start should succeed against the substituted simulator address");

        let current = collector.current_values();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let first_value = loop {
            if let Some(sample) = current.get("tag:1") {
                if sample.quality == crate::current::Quality::Good {
                    if let Some(v) = sample.value {
                        break v;
                    }
                }
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "no Good reading ever showed up - the connection never reached the \
                 simulator, so the host/port substitution must not have happened"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        };

        // The ramp-wave generator (100ms period) must move the value on -
        // proof this is a live simulator, not a static seeded register.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(sample) = current.get("tag:1") {
                if sample.value.is_some_and(|v| v != first_value) {
                    break;
                }
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the simulated register never changed - the ramp-wave generator task \
                 must not be running"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        collector.stop().await.expect("stop should succeed");
    }

    /// T9-1: toggling only `simulation` (everything else on the connection
    /// unchanged) must still make `apply_config`'s diff classify the
    /// connection "replaced", not "unchanged" - the mechanism this module's
    /// doc comment ("T9-1 addendum") relies on to piggyback the simulator's
    /// stop/start onto the ordinary stop-then-spawn path, with no dedicated
    /// branch of its own.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn toggling_simulation_alone_is_classified_as_replaced() {
        let dir = TempDir::new();
        let pool = banto_storage::connect_sqlite_memory()
            .await
            .expect("connect sqlite memory");
        let options = CollectorOptions {
            connect_timeout: Duration::from_millis(200),
            response_timeout: Duration::from_millis(200),
            ..CollectorOptions::default()
        };

        let mut collector = Collector::start(
            one_tag_config(),
            dir.path(),
            Arc::new(SystemClock),
            EventSink::new(pool),
            options,
        )
        .await
        .expect("start should succeed");

        let simulated = one_tag_config_simulated();
        let report = collector
            .apply_config(simulated, default_client_factory())
            .await
            .expect("apply_config should succeed");
        assert_eq!(report.added, Vec::<String>::new());
        assert_eq!(report.removed, Vec::<String>::new());
        assert_eq!(report.replaced, vec!["conn:1".to_string()]);
        assert_eq!(report.unchanged, Vec::<String>::new());

        collector.stop().await.expect("stop should succeed");
    }
}
