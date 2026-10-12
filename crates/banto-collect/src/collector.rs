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
//!    channel. [`Collector::stop`] simply signals and joins every entry -
//!    **every entry is signalled before the first join** (#588, see
//!    "Stopping several tasks at once" below).
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
//! 4. **Distribute the new writer** (the old one is retired later, in step
//!    5b). Must happen *before* step 5 (spawning new tasks): `TsWriter::append`'s
//!    unknown-group error never stops the hot loop
//!    (`task.rs::record_group`; H4, 2026-08-08 owner decision,
//!    docs/improvement-plan.md - the failure itself is now recorded, an
//!    `eprintln!` plus an `append_failure_entered`/`append_failure_cleared`
//!    `collect_events` pair, but is still never fatal, by design), so a
//!    newly spawned task reading a brand-new group must never see the *old*
//!    writer, which has no schema for that group at all - every row it tried
//!    to write would still be permanently lost (no retry, no backfill),
//!    merely no longer silently so. **Distributed with `send_replace`, not
//!    `send`** - see step 4's own comment in the code (#341: `send` silently
//!    keeps the old value when the channel momentarily has no receivers,
//!    which happens whenever the only connection is `replaced`).
//! 5. **Spawn tasks for added/replaced connections** - now guaranteed to
//!    subscribe to a writer that already knows about every group they read.
//!    **Step 5b - retire the old writer, non-fatally** (#341 レビュー対応,
//!    2026-09-14). Reuses `stop`'s own `Arc::try_unwrap`-or-flush fallback
//!    (`close_or_flush_writer`): an unchanged connection may be mid-append on
//!    the old writer at the exact moment of rotation, so failing to get sole
//!    ownership here is an expected occasional outcome, not a bug - either
//!    branch guarantees no buffered row is silently dropped, which is the
//!    invariant that matters. **It runs after the spawn (not before) and its
//!    failure is logged rather than returned**: when it sat before step 5 and
//!    propagated with `?`, a failing final flush of the *old* file left the
//!    collector with the new writer distributed, `self.config` still the old
//!    one, and **no task at all** for every added/replaced connection - the
//!    caller (`banto-hub`'s `CollectorManager`) would report failure while
//!    the run silently collected nothing. The rows at risk are the old
//!    file's last unflushed buffer, which no longer affects whether the new
//!    configuration is running.
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
//! ### #383 段階3 P3 addendum: external connections
//!
//! A `"hub"` connection's plan ([`crate::config::ProtocolConfig::External`])
//! gets `crate::task::run_external_connection` instead of `run_connection`,
//! sampling the [`ExternalSource`] handed to
//! [`Collector::start_with_sources`] - the one place a source enters the
//! engine, kept for the collector's life so `apply_config` can spawn an
//! added/replaced external connection with it too. It is otherwise an
//! ordinary connection task in every step above (its own stop channel, the
//! shared writer `watch`, the same cache/status/events), and never has a
//! simulator. A config with external connections and no source is refused
//! ([`CollectError::Config`]) rather than left reading "not Live" forever -
//! at start, and also by `apply_config` on a collector started without a
//! source (checked first, before anything is touched: the collector stays
//! exactly as it was, PR #578 review).
//!
//! ### Stopping several tasks at once (#588)
//!
//! A connection task only notices its stop signal between reads
//! (`task.rs::run_connection` awaits `read_batch` inside the select arm), so
//! a task blocked on a PLC that does not answer stops only when that read
//! gives up (the response timeout, 1 s by default). Signalling and joining
//! one task at a time therefore made a stop take roughly *N times* that
//! wait: 20 unresponsive connections took 4.7 s, 80 took 12.4 s on real
//! hardware - longer than banto-hub's shell exit budget (5 s on OS
//! shutdown, 15 s from the tray) and ChronoGazer's (5 s), which then cut the
//! final flush and `collection_stopped` off.
//!
//! [`Collector::stop`] and `apply_config`'s step 3 therefore go through
//! `stop_tasks`: **send every stop signal first, then join**. The joins are
//! still awaited one after another, but every task is already winding down
//! concurrently, so the whole stop takes about one task's worth of waiting.
//! What is deliberately *not* changed:
//!
//! - **The ordering promises.** The final flush (the single writer) and
//!   `collection_stopped` still happen once, after *every* task has been
//!   joined - i.e. after every task has released its `writer_rx`, which is
//!   what lets `close_or_flush_writer` take sole ownership. A simulator
//!   (T9-1) is still stopped only after its task has been joined.
//! - **A failed task does not block the rest.** A task that panicked yields
//!   a `JoinError` that is ignored exactly as before; every other task has
//!   already been signalled and is still joined, and the flush still runs.
//! - **No per-task timeout or `abort`.** Aborting a task mid-append would
//!   weaken the "no buffered row is lost" guarantee; the wait is bounded by
//!   the protocol's own response timeout instead.
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
use crate::external::ExternalSource;
use crate::simulation::{self, SimulatorHandle};
use crate::task::{
    default_client_factory, retain_status, run_connection, run_external_connection, BackoffConfig,
    ClientFactory, ConnectionStatus, StatusMap, TaskContext,
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
    /// #383 段階3 P3: the source external (`"hub"`) connections sample, if
    /// the caller supplied one (this module's doc, "external connections").
    external: Option<Arc<dyn ExternalSource>>,
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
    /// directly with a factory that routes broker-managed connections through
    /// `banto_broker` (see `apps/banto-hub/core/src/broker_glue.rs`) - SLMP
    /// since T2-2, and Modbus TCP too since #337 (2026-09-08), leaving only
    /// connections without a broker session on the default client - see
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
        Self::start_with_sources(config, data_dir, clock, events, options, factory, None).await
    }

    /// #383 段階3 P3: [`Self::start_with_client_factory`] plus the
    /// [`ExternalSource`] that external (`"hub"`) connections sample. A
    /// config with external connections
    /// ([`CollectorConfig::has_external_connections`]) and `external = None`
    /// is refused with [`CollectError::Config`] before anything is opened.
    pub async fn start_with_sources(
        config: CollectorConfig,
        data_dir: &Path,
        clock: Arc<dyn Clock>,
        events: EventSink,
        options: CollectorOptions,
        factory: ClientFactory,
        external: Option<Arc<dyn ExternalSource>>,
    ) -> Result<Self, CollectError> {
        if config.connections.is_empty() {
            return Err(CollectError::Config(
                "収集対象がありません（有効な接続・グループ・タグを設定してください）".to_string(),
            ));
        }
        if external.is_none() && config.has_external_connections() {
            return Err(CollectError::Config(MISSING_EXTERNAL_SOURCE.to_string()));
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
            let sim_addr = match protocol_of(plan) {
                Some(protocol) if plan.simulation => {
                    let sim = simulation::start(protocol).await;
                    let addr = sim.addr();
                    simulators.insert(plan.key.clone(), sim);
                    Some(addr)
                }
                _ => None,
            };
            let ctx = TaskContext {
                writer_rx: writer_tx.subscribe(),
                clock: clock.clone(),
                current: current.clone(),
                events: events.clone(),
                status: status.clone(),
                backoff: options.backoff,
                simulation: plan.simulation,
                factory: factory.clone(),
            };
            let task = spawn_task(
                plan_for_task(plan, &options, sim_addr),
                ctx,
                external.as_ref(),
            );
            tasks.insert(plan.key.clone(), task);
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
            external,
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
    ///
    /// **`Err` の意味（#341 レビュー対応で正確に書き直した、2026-09-14）**:
    /// このメソッドが `Err` を返すのは、**新しい writer を開けなかったとき
    /// だけ**（ステップ2の all-or-nothing アンカー）。そのときは何一つ
    /// 触っていない - `self.config`・全タスク・writer とも呼び出し前の
    /// ままで、呼び出し元（banto-hub の `CollectorManager::apply_run`）が
    /// 走行中の収集をそのまま続けられる。逆に、そのアンカーを越えた後の
    /// 失敗はもう `Err` にしない: **旧 writer の最終 flush 失敗**
    /// （ステップ5b）はログに出すだけで `Ok` を返す。失われうるのは旧
    /// ファイルの最後の未フラッシュ分だけで、その時点で新構成のタスクは
    /// 既に全部立っており、ここで `Err` を返すと「タスクは新構成・
    /// `self.config` は旧構成・呼び出し元は失敗と判断」という、はるかに
    /// 悪い不整合が残るため（モジュール doc のステップ 5b 参照）。
    ///
    /// **もう 1 つの `Err`（#383 段階3 P3、PR #578 レビュー）**: 外部ソース無しで
    /// 起動した collector（[`Self::start`] / [`Self::start_with_client_factory`]）に、
    /// 外部（`"hub"`）接続を含む構成を渡すと [`CollectError::Config`] を返す。
    /// 判定は**何よりも先に**行う（差分も writer も触る前）ので、この `Err` の
    /// ときも collector は呼び出し前のまま（`self.config`・全タスク・writer とも
    /// 不変）。受け入れると、その接続は値の出どころが無いまま「Live でない」
    /// （Bad + null）を永久に記録し続けるため。
    pub async fn apply_config(
        &mut self,
        new_config: CollectorConfig,
        factory: ClientFactory,
    ) -> Result<ApplyReport, CollectError> {
        // --- 0. External connections need a source (before any side effect) -
        if self.external.is_none() && new_config.has_external_connections() {
            return Err(CollectError::Config(MISSING_EXTERNAL_SOURCE.to_string()));
        }

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
        // #588: every task is signalled before the first join (`stop_tasks`,
        // this module's doc comment "Stopping several tasks at once").
        // T9-1: a removed/replaced connection's simulator (if any) is stopped
        // right alongside its task - see this module's doc comment, "T9-1
        // addendum". Stopped after the task itself (the task may still be
        // mid-`read_batch` against it up to the moment it joins).
        let stopping: Vec<ConnectionTask> = removed
            .iter()
            .chain(replaced.iter())
            .filter_map(|key| self.tasks.remove(key))
            .collect();
        stop_tasks(stopping).await;
        for key in removed.iter().chain(replaced.iter()) {
            if let Some(sim) = self.simulators.remove(key) {
                sim.stop().await;
            }
        }

        // --- 4. Distribute the new writer (retired in 5b, below) -----------
        //
        // **`send_replace`, NOT `send`** (#341 で発覚、2026-09-14): step 3 が
        // 直前に「置き換え対象の接続タスク」を全部 join しており、それが
        // *唯一の*接続だった場合、この時点で `writer_tx` の受信者は 0 人に
        // なっている（`Collector` 自身は `watch::Receiver` を保持しない -
        // `start_with_client_factory` の `_writer_rx` はその関数を抜けた
        // 時点で drop される）。`watch::Sender::send` は受信者が 0 だと
        // **値を更新せずに** `Err` を返す仕様なので、`let _ =` で握り潰すと
        // step 5 で spawn した新タスクが `subscribe()` から**古い writer**を
        // 受け取ってしまう。既存タグ1本のグループにタグを1本足した場合で
        // 言えば、新タスクは毎周期2値を append するのに writer のスキーマは
        // 1列のままなので、`TstoreError::ValueCountMismatch` で
        // **履歴が一切書けなくなる**（現在値キャッシュと live event は
        // 流れ続けるので、症状は「値は見えるのに履歴が残らない」）。
        // `send_replace` は受信者の有無に関わらず必ず値を差し替えるため、
        // この経路でも新タスクは確実に新しい writer を見る。
        //
        // 発覚が #341 まで遅れた理由: 既存の回帰テスト
        // （`tests/integration.rs` の
        // `apply_config_writer_rotation_preserves_old_and_new_data`）は
        // 「接続 B を*追加*する」形なので接続 A のタスクが生き残り、受信者が
        // 0 にならない。「唯一の接続が replaced になる」= 既存グループへの
        // タグ追加という、#341 で初めて本番から到達可能になった形でしか
        // 踏めなかった。
        let retired_writer = new_writer.map(|new_writer| {
            let old_writer = self.writer_tx.borrow().clone();
            self.writer_tx.send_replace(new_writer);
            old_writer
        });

        // --- 5. Spawn tasks for added/replaced connections ------------------
        // T9-1: start a fresh simulator first (if the new plan wants one) so
        // its address is ready before the task that will read from it spawns
        // - see this module's doc comment, "T9-1 addendum".
        for key in added.iter().chain(replaced.iter()) {
            let plan = new_map[key.as_str()];
            let sim_addr = match protocol_of(plan) {
                Some(protocol) if plan.simulation => {
                    let sim = simulation::start(protocol).await;
                    let addr = sim.addr();
                    self.simulators.insert(key.clone(), sim);
                    Some(addr)
                }
                _ => None,
            };
            let ctx = TaskContext {
                writer_rx: self.writer_tx.subscribe(),
                clock: self.clock.clone(),
                current: self.current.clone(),
                events: self.events.clone(),
                status: self.status.clone(),
                backoff: self.options.backoff,
                simulation: plan.simulation,
                factory: factory.clone(),
            };
            let task = spawn_task(
                plan_for_task(plan, &self.options, sim_addr),
                ctx,
                self.external.as_ref(),
            );
            self.tasks.insert(key.clone(), task);
        }

        // --- 5b. Retire the old writer (non-fatal) -------------------------
        // #341 レビュー対応（2026-09-14）: この retire は step 5（新タスクの
        // spawn）の**後**に置き、失敗しても `Err` を返さない - このモジュール
        // doc の step 5b 参照。旧ファイルの最後の未フラッシュ分が失われうる
        // ことと、「新構成のタスクが1本も立っていないのに config は旧のまま」
        // という中途半端な状態を残すことを天秤にかけ、後者を避ける。
        if let Some(old_writer) = retired_writer {
            if let Err(err) = close_or_flush_writer(old_writer).await {
                eprintln!(
                    "banto-collect: 旧 writer の最終 flush に失敗しました（新しい構成の収集は継続します）: {err}"
                );
            }
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

    /// A receiver for the live writer (the same `watch` channel the
    /// connection tasks use, so it always yields the writer currently in
    /// use even after [`Collector::apply_config`] rotates it). Lets the
    /// host app coordinate with the writer - e.g. retention pruning that
    /// must not delete the file the writer still holds
    /// ([`banto_tstore::TsWriter::prune_files`], #538).
    pub fn writer_handle(&self) -> watch::Receiver<Arc<TsWriter>> {
        self.writer_tx.subscribe()
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
        // Signal every task, then drain them all (#588: all signals go out
        // before the first join - this module's doc comment, "Stopping
        // several tasks at once"). Each task closes its own socket and marks
        // itself Stopped on the way out, releasing its writer_rx (and any
        // transient per-append writer clone) with it.
        stop_tasks(self.tasks.drain().map(|(_key, task)| task)).await;

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

/// Stop a set of connection tasks: send **every** stop signal first, then
/// join them (#588, this module's doc comment "Stopping several tasks at
/// once"). Sending is synchronous (`watch::Sender::send`), so by the time the
/// first join is awaited every task has been told to stop and they all wind
/// down concurrently - the total wait is about the slowest task's, not the
/// sum. A task that panicked (`JoinError`) is ignored, as it always was, and
/// never keeps the rest from being joined. Returns only once every task has
/// finished, so a caller may rely on every `writer_rx` having been released.
async fn stop_tasks(tasks: impl IntoIterator<Item = ConnectionTask>) {
    let tasks: Vec<ConnectionTask> = tasks.into_iter().collect();
    for task in &tasks {
        let _ = task.stop_tx.send(true);
    }
    for task in tasks {
        let _ = task.handle.await;
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
        // Nothing to dial, so nothing to override.
        ProtocolConfig::External => {}
    }
    plan
}

/// The [`CollectError::Config`] text for a config with external (`"hub"`)
/// connections handed to a collector that has no [`ExternalSource`] - shared
/// by [`Collector::start_with_sources`] and [`Collector::apply_config`].
const MISSING_EXTERNAL_SOURCE: &str = "Hub 経由の接続を収集するための値の取得元が渡されていません";

/// Spawn one connection's task with its own stop channel: the socket loop
/// for a PLC plan, the sampling loop for an external one (#383 段階3 P3).
/// `external` is only read for an external plan; `start_with_sources`
/// guarantees it is `Some` whenever the config has one (a missing source
/// would otherwise only ever read "not Live").
fn spawn_task(
    plan: ConnectionPlan,
    ctx: TaskContext,
    external: Option<&Arc<dyn ExternalSource>>,
) -> ConnectionTask {
    let (stop_tx, stop_rx) = watch::channel(false);
    let handle = match (&plan.config, external) {
        (ProtocolConfig::External, Some(source)) => tokio::spawn(run_external_connection(
            plan,
            ctx,
            Arc::clone(source),
            stop_rx,
        )),
        (ProtocolConfig::External, None) => tokio::spawn(run_external_connection(
            plan,
            ctx,
            Arc::new(MissingSource),
            stop_rx,
        )),
        _ => tokio::spawn(run_connection(plan, ctx, stop_rx)),
    };
    ConnectionTask { handle, stop_tx }
}

/// Stands in for an absent [`ExternalSource`]. Unreachable:
/// [`Collector::start_with_sources`] and [`Collector::apply_config`] both
/// refuse a config with external connections when the collector has no
/// source ([`MISSING_EXTERNAL_SOURCE`]). Kept only so [`spawn_task`] is total -
/// the connection would read "not Live" (Bad + null) instead of panicking.
struct MissingSource;

impl ExternalSource for MissingSource {
    fn snapshot(&self) -> crate::external::ExternalSnapshot {
        crate::external::ExternalSnapshot::down(None)
    }
}

/// Project a [`ConnectionPlan`]'s protocol config down to the
/// [`crate::config::Protocol`] [`crate::simulation::start`] dispatches on -
/// the two are always in lockstep (`ProtocolConfig::ModbusTcp` iff
/// `Protocol::ModbusTcp`, built from the same `build_config` match arm), so
/// this is a plain projection, never a guess. `None` for an external
/// connection, which has no wire protocol and never a simulator.
fn protocol_of(plan: &ConnectionPlan) -> Option<Protocol> {
    match &plan.config {
        ProtocolConfig::ModbusTcp(_) => Some(Protocol::ModbusTcp),
        ProtocolConfig::Slmp(_) => Some(Protocol::Slmp),
        ProtocolConfig::External => None,
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
            external_names: Vec::new(),
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
            _ => unreachable!("one_tag_config always builds ModbusTcp"),
        }
        config
    }

    // --- #344 (2026-09-15): connection-event edges must not flap ----------
    //
    // These drive `crate::task::run_connection`'s state machine through a
    // client shaped exactly like the bug: `connect()` always succeeds while
    // `read_batch` keeps failing - which is precisely what banto-hub's
    // `BrokerReadClient` did before #344 whenever its broker session was
    // down. See `crate::task`'s module doc ("What `plc_reconnected` means")
    // for the rule these pin down.

    /// A fake `PlcClient` whose `connect()` always succeeds instantly (no
    /// socket) and whose `read_batch` fails for the first `fail_reads` calls
    /// and succeeds afterwards. The call counter is shared across every
    /// instance the factory hands out, so it keeps counting across the
    /// reconnects `run_connection` performs between ticks.
    struct FlappingClient {
        reads: Arc<AtomicUsize>,
        fail_reads: usize,
    }

    impl PlcClient for FlappingClient {
        fn connect(&mut self) -> BoxFuture<'_, Result<(), PlcError>> {
            Box::pin(async { Ok(()) })
        }

        fn read_batch<'a>(
            &'a mut self,
            requests: &'a [ReadRequest],
        ) -> BoxFuture<'a, Result<Vec<ReadResult>, PlcError>> {
            let nth = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
            let fail = nth <= self.fail_reads;
            Box::pin(async move {
                if fail {
                    // The exact shape banto-hub's broker adapter produces
                    // for a session that is down.
                    Err(PlcError::Connection(
                        "PLC接続 1 は現在未接続です（再接続待機中…）".to_string(),
                    ))
                } else {
                    Ok(requests
                        .iter()
                        .map(|_| ReadResult::Value(TagValue::F64(7.0)))
                        .collect())
                }
            })
        }

        fn disconnect(&mut self) -> BoxFuture<'_, ()> {
            Box::pin(async {})
        }
    }

    fn flapping_factory(reads: Arc<AtomicUsize>, fail_reads: usize) -> ClientFactory {
        Arc::new(move |_spec| {
            Box::new(FlappingClient {
                reads: reads.clone(),
                fail_reads,
            }) as Box<dyn PlcClient>
        })
    }

    /// Start a collector on [`one_tag_config`] with the given client factory,
    /// plus a live event receiver subscribed *before* anything can emit.
    async fn start_flapping(
        dir: &TempDir,
        factory: ClientFactory,
    ) -> (
        Collector,
        tokio::sync::broadcast::Receiver<crate::event::CollectEvent>,
    ) {
        let pool = banto_storage::connect_sqlite_memory()
            .await
            .expect("connect sqlite memory");
        let events = EventSink::new(pool);
        let rx = events.subscribe();
        let collector = Collector::start_with_client_factory(
            one_tag_config(),
            dir.path(),
            Arc::new(SystemClock),
            events,
            CollectorOptions {
                connect_timeout: Duration::from_millis(200),
                response_timeout: Duration::from_millis(200),
                ..CollectorOptions::default()
            },
            factory,
        )
        .await
        .expect("start_with_client_factory should succeed");
        (collector, rx)
    }

    /// Drain everything currently buffered on the live channel.
    fn drain_events(
        rx: &mut tokio::sync::broadcast::Receiver<crate::event::CollectEvent>,
    ) -> Vec<crate::event::CollectEvent> {
        let mut out = Vec::new();
        while let Ok(event) = rx.try_recv() {
            out.push(event);
        }
        out
    }

    fn count_kind(events: &[crate::event::CollectEvent], kind: crate::event::EventKind) -> usize {
        events.iter().filter(|e| e.kind == kind).count()
    }

    /// Wait until the shared read counter reaches `target`, or panic.
    async fn wait_for_reads(reads: &Arc<AtomicUsize>, target: usize) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while reads.load(Ordering::SeqCst) < target {
            assert!(
                tokio::time::Instant::now() < deadline,
                "only {} of the expected {target} read_batch calls happened",
                reads.load(Ordering::SeqCst)
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// **The #344 regression itself.** A client that connects successfully
    /// but can never read (the pre-fix `BrokerReadClient` against a down
    /// broker session) must produce exactly ONE `plc_disconnected` and NEVER
    /// a `plc_reconnected`, no matter how many collection periods elapse.
    /// Before the fix this emitted one `plc_reconnected`/`plc_disconnected`
    /// pair per tick - 13,000 rows/hour on real hardware.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_connection_that_never_reads_reports_one_disconnect_and_no_reconnect() {
        let dir = TempDir::new();
        let reads = Arc::new(AtomicUsize::new(0));
        let (collector, mut rx) =
            start_flapping(&dir, flapping_factory(reads.clone(), usize::MAX)).await;

        // `one_tag_config`'s group period is 20ms, so 8 reads is well past
        // the point where the old code would have emitted 7 bogus pairs.
        wait_for_reads(&reads, 8).await;
        collector.stop().await.expect("stop should succeed");

        let events = drain_events(&mut rx);
        assert_eq!(
            count_kind(&events, crate::event::EventKind::PlcConnected),
            1,
            "the first connect is still announced exactly once: {events:?}"
        );
        assert_eq!(
            count_kind(&events, crate::event::EventKind::PlcDisconnected),
            1,
            "one outage must produce exactly one plc_disconnected, however many \
             ticks fail inside it: {events:?}"
        );
        assert_eq!(
            count_kind(&events, crate::event::EventKind::PlcReconnected),
            0,
            "nothing ever read successfully, so nothing reconnected: {events:?}"
        );
    }

    /// The companion edge: once reads start succeeding again, exactly one
    /// `plc_reconnected` is emitted - at the successful read, and only once
    /// however many further successful ticks follow.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_first_successful_read_after_an_outage_emits_one_reconnect() {
        let dir = TempDir::new();
        let reads = Arc::new(AtomicUsize::new(0));
        // Reads 1..=4 fail, everything from the 5th on succeeds.
        let (collector, mut rx) = start_flapping(&dir, flapping_factory(reads.clone(), 4)).await;

        let current = collector.current_values();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while current.get("tag:1").and_then(|s| s.value) != Some(7.0) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "reads never recovered"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // Several more successful ticks, to prove the edge does not repeat.
        wait_for_reads(&reads, 12).await;
        collector.stop().await.expect("stop should succeed");

        let events = drain_events(&mut rx);
        assert_eq!(
            count_kind(&events, crate::event::EventKind::PlcConnected),
            1,
            "{events:?}"
        );
        assert_eq!(
            count_kind(&events, crate::event::EventKind::PlcDisconnected),
            1,
            "the 4 failing reads are one outage, not four: {events:?}"
        );
        assert_eq!(
            count_kind(&events, crate::event::EventKind::PlcReconnected),
            1,
            "recovery must be announced exactly once, at the first successful \
             read: {events:?}"
        );

        // Ordering: connected -> disconnected -> reconnected, in that order.
        let kinds: Vec<_> = events
            .iter()
            .map(|e| e.kind)
            .filter(|k| {
                matches!(
                    k,
                    crate::event::EventKind::PlcConnected
                        | crate::event::EventKind::PlcDisconnected
                        | crate::event::EventKind::PlcReconnected
                )
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                crate::event::EventKind::PlcConnected,
                crate::event::EventKind::PlcDisconnected,
                crate::event::EventKind::PlcReconnected,
            ],
            "{events:?}"
        );
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

    // --- #383 段階3 P3: external (`"hub"`) connections -----------------------
    //
    // These drive `crate::task::run_external_connection` end to end through a
    // real `Collector` built from a real registry (one PLC connection on the
    // `FakeClient` + one `"hub"` connection), with a hand-set
    // `ExternalSource`. Waits are "poll a counter until it reaches N, with a
    // deadline" - no fixed sleeps decide the outcome.
    mod external_sources {
        use super::*;
        use crate::config::{build_config_lenient_with_thresholds, HubSource, RegistrySnapshot};
        use crate::current::Quality;
        use crate::event::EventKind;
        use crate::external::{ExternalQuality, ExternalReading, ExternalSnapshot, ExternalSource};
        use banto_tags::{
            CollectionGroupInput, CollectionGroupService, PlcConnectionInput, PlcConnectionService,
            TagInput, TagService,
        };
        use std::collections::HashMap;
        use std::sync::Mutex;

        /// The external names registered on the hub connection, in tag order.
        const MEASURED: &str = "PLC9.G.measured";
        /// Labelled `simulation` by the Hub in real life; the label is not
        /// interpreted any more (2026-10-11), so it is just another Good value.
        const SIMULATED: &str = "PLC9.G.simulated";
        const STALE: &str = "PLC9.G.stale";
        const BAD: &str = "PLC9.G.bad";
        const UNKNOWN_QUALITY: &str = "PLC9.G.unknown";
        /// Registered but never present in the source's snapshot.
        const ABSENT: &str = "PLC9.G.absent";
        const HUB_NAMES: [&str; 6] = [MEASURED, SIMULATED, STALE, BAD, UNKNOWN_QUALITY, ABSENT];

        /// A hand-set source: whatever `set` last stored, counting how many
        /// times the task sampled it (= tick-wakes of the hub connection).
        struct MockSource {
            snapshot: Mutex<ExternalSnapshot>,
            samples: AtomicUsize,
        }

        impl MockSource {
            fn new(snapshot: ExternalSnapshot) -> Arc<Self> {
                Arc::new(Self {
                    snapshot: Mutex::new(snapshot),
                    samples: AtomicUsize::new(0),
                })
            }

            fn set(&self, snapshot: ExternalSnapshot) {
                *self.snapshot.lock().unwrap() = snapshot;
            }

            /// Wait until the task has sampled `more` further times.
            async fn wait_samples(&self, more: usize) {
                let target = self.samples.load(Ordering::SeqCst) + more;
                let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
                while self.samples.load(Ordering::SeqCst) < target {
                    assert!(
                        tokio::time::Instant::now() < deadline,
                        "the hub task sampled only {} of {target} times",
                        self.samples.load(Ordering::SeqCst)
                    );
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        }

        impl ExternalSource for MockSource {
            fn snapshot(&self) -> ExternalSnapshot {
                self.samples.fetch_add(1, Ordering::SeqCst);
                self.snapshot.lock().unwrap().clone()
            }
        }

        fn reading(value: f64, quality: ExternalQuality) -> ExternalReading {
            ExternalReading {
                value: Some(value),
                quality,
            }
        }

        /// Every held value, one per label the mapping cares about. `ABSENT`
        /// is deliberately missing.
        fn held_values() -> HashMap<String, ExternalReading> {
            use ExternalQuality as Q;
            HashMap::from([
                (MEASURED.to_string(), reading(7.0, Q::Good)),
                (SIMULATED.to_string(), reading(8.0, Q::Good)),
                (STALE.to_string(), reading(9.0, Q::Stale)),
                (BAD.to_string(), reading(10.0, Q::Bad)),
                (UNKNOWN_QUALITY.to_string(), reading(11.0, Q::Unknown)),
            ])
        }

        /// Keys of the registry rows [`start`] created.
        struct Keys {
            plc_tag: String,
            plc_group: String,
            hub_conn: String,
            hub_group: String,
            /// In [`HUB_NAMES`] order.
            hub_tags: Vec<String>,
        }

        impl Keys {
            fn hub_tag(&self, name: &str) -> &str {
                let idx = HUB_NAMES.iter().position(|n| *n == name).unwrap();
                &self.hub_tags[idx]
            }
        }

        /// Register a PLC connection (FakeClient, value 42) and a `"hub"`
        /// connection with [`HUB_NAMES`], both on 100 ms groups, and start a
        /// collector on them with `source`.
        async fn start(
            dir: &TempDir,
            source: Arc<MockSource>,
        ) -> (
            Collector,
            tokio::sync::broadcast::Receiver<crate::event::CollectEvent>,
            Keys,
        ) {
            let pool = banto_storage::connect_sqlite_memory().await.unwrap();
            banto_tags::migrate(&pool).await.unwrap();
            let conns = PlcConnectionService::new(pool.clone());
            let groups = CollectionGroupService::new(pool.clone());
            let tags = TagService::new(pool.clone());
            let conn_input = |name: &str, protocol: &str| PlcConnectionInput {
                name: name.to_string(),
                protocol: protocol.to_string(),
                host: "127.0.0.1".to_string(),
                port: 1,
                unit_id: 1,
                enabled: true,
                simulation: false,
                word_order: "high_low".to_string(),
                database: None,
                username: None,
                password: None,
            };
            let group_input = |conn: i64| CollectionGroupInput {
                name: format!("G{conn}"),
                plc_connection_id: conn,
                period_ms: 100,
                enabled: true,
                default_writable: false,
                query_sql: None,
            };
            let tag_input = |name: &str, group: i64, address: &str, data_type: &str| TagInput {
                name: name.to_string(),
                collection_group_id: group,
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
            };

            let plc = conns
                .create(conn_input("PLC1", "modbus-tcp"))
                .await
                .unwrap();
            let plc_group = groups.create(group_input(plc.id)).await.unwrap();
            let plc_tag = tags
                .create(tag_input("plc", plc_group.id, "40001", "i16"))
                .await
                .unwrap();
            let hub = conns
                .create(conn_input("Hub", banto_tags::HUB_PROTOCOL))
                .await
                .unwrap();
            let hub_group = groups.create(group_input(hub.id)).await.unwrap();
            let mut hub_tags = Vec::new();
            for (i, name) in HUB_NAMES.iter().enumerate() {
                let tag = tags
                    .create(tag_input(&format!("h{i}"), hub_group.id, name, "f64"))
                    .await
                    .unwrap();
                hub_tags.push(format!("tag:{}", tag.id));
            }

            let snapshot = RegistrySnapshot::load(&pool).await.unwrap();
            let (config, exclusions) = build_config_lenient_with_thresholds(
                &snapshot,
                &HashMap::new(),
                HubSource::Available { catalog: None },
            );
            assert!(exclusions.is_empty(), "{exclusions:?}");

            let events_pool = banto_storage::connect_sqlite_memory().await.unwrap();
            let events = EventSink::new(events_pool);
            let rx = events.subscribe();
            let reads = Arc::new(AtomicUsize::new(0));
            let factory: ClientFactory = Arc::new(move |_spec| {
                Box::new(FakeClient {
                    reads: reads.clone(),
                }) as Box<dyn PlcClient>
            });
            let collector = Collector::start_with_sources(
                config,
                dir.path(),
                Arc::new(SystemClock),
                events,
                CollectorOptions::default(),
                factory,
                Some(source as Arc<dyn ExternalSource>),
            )
            .await
            .expect("start_with_sources");
            let keys = Keys {
                plc_tag: format!("tag:{}", plc_tag.id),
                plc_group: format!("grp:{}", plc_group.id),
                hub_conn: format!("conn:{}", hub.id),
                hub_group: format!("grp:{}", hub_group.id),
                hub_tags,
            };
            (collector, rx, keys)
        }

        /// Every stored row of `group_key` across the data files in `dir`
        /// (read after `stop()` flushed and closed the writer).
        async fn rows(dir: &TempDir, group_key: &str) -> Vec<Vec<Option<f64>>> {
            let mut rows = Vec::new();
            for file in banto_tstore::list_data_files(dir.path()).unwrap() {
                let reader = banto_tstore::TsReader::open(&file.path).await.unwrap();
                for sample in reader
                    .read_range(group_key, i64::MIN, i64::MAX)
                    .await
                    .unwrap()
                {
                    rows.push(sample.values);
                }
            }
            rows
        }

        /// The PLC connection's rows: after the first tick that read (the
        /// first tick fires while its fake client is still connecting, an
        /// all-NULL row like any PLC connection's), every row carries the
        /// fake client's 42 - the hub connection beside it changes nothing.
        fn assert_plc_rows_unaffected(plc_rows: &[Vec<Option<f64>>]) {
            let read: Vec<&Vec<Option<f64>>> = plc_rows
                .iter()
                .skip_while(|row| row.as_slice() == [None])
                .collect();
            assert!(!read.is_empty(), "{plc_rows:?}");
            assert!(
                read.iter().all(|row| row.as_slice() == [Some(42.0)]),
                "{plc_rows:?}"
            );
        }

        fn connection_kinds(
            events: &[crate::event::CollectEvent],
            conn_key: &str,
        ) -> Vec<EventKind> {
            events
                .iter()
                .filter(|e| e.connection_key.as_deref() == Some(conn_key))
                .map(|e| e.kind)
                .filter(|k| {
                    matches!(
                        k,
                        EventKind::PlcConnected
                            | EventKind::PlcDisconnected
                            | EventKind::PlcReconnected
                    )
                })
                .collect()
        }

        /// The mapping table end to end, and **a steady value stays Good and
        /// is recorded on every cycle** (H4: the subscription is on-change, so
        /// an unchanged value is never re-sent - the snapshot here never
        /// changes at all for the whole run). Stale, bad, unknown-quality
        /// and absent values are never written; the PLC
        /// connection beside it is untouched.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn live_values_are_recorded_every_cycle_and_mapped_by_quality() {
            let dir = TempDir::new();
            let source = MockSource::new(ExternalSnapshot::live(held_values()));
            let (collector, mut rx, keys) = start(&dir, source.clone()).await;

            // Well past the 2.5-period Stale horizon of the cache, with no
            // change at all in what the source holds.
            source.wait_samples(12).await;

            let current = collector.current_values();
            let get = |name: &str| current.get(keys.hub_tag(name)).expect("cached");
            assert_eq!(
                (get(MEASURED).value, get(MEASURED).quality),
                (Some(7.0), Quality::Good)
            );
            assert_eq!(
                (get(SIMULATED).value, get(SIMULATED).quality),
                (Some(8.0), Quality::Good),
                "a Good value is shown whatever the Hub labelled it (2026-10-11)"
            );
            assert_eq!(
                (get(STALE).value, get(STALE).quality),
                (Some(9.0), Quality::Stale)
            );
            for name in [BAD, UNKNOWN_QUALITY, ABSENT] {
                assert_eq!((get(name).value, get(name).quality), (None, Quality::Bad));
            }
            assert_eq!(
                collector.status().get(&keys.hub_conn),
                Some(&ConnectionStatus::Connected)
            );
            assert_eq!(
                current.get(&keys.plc_tag).map(|s| (s.value, s.quality)),
                Some((Some(42.0), Quality::Good))
            );

            collector.stop().await.expect("stop");

            let hub_rows = rows(&dir, &keys.hub_group).await;
            assert!(hub_rows.len() >= 10, "{} rows", hub_rows.len());
            for row in &hub_rows {
                assert_eq!(
                    row,
                    &vec![Some(7.0), Some(8.0), None, None, None, None],
                    "only the Good values are recorded, on every row"
                );
            }
            assert_plc_rows_unaffected(&rows(&dir, &keys.plc_group).await);

            let mut events = Vec::new();
            while let Ok(event) = rx.try_recv() {
                events.push(event);
            }
            assert_eq!(
                connection_kinds(&events, &keys.hub_conn),
                vec![EventKind::PlcConnected]
            );
        }

        /// H4: while the Hub link is not Live **every** hub tag is Bad and
        /// recorded as null - even though the source still holds the last
        /// values - and the link's drop and return are exactly one
        /// `plc_disconnected` and one `plc_reconnected` under the hub
        /// connection's key, however many cycles the outage lasts. The PLC
        /// connection keeps recording through it.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_hub_outage_is_bad_and_null_with_one_disconnect_and_one_reconnect() {
            let dir = TempDir::new();
            let source = MockSource::new(ExternalSnapshot::live(held_values()));
            let (collector, mut rx, keys) = start(&dir, source.clone()).await;
            let current = collector.current_values();
            let measured = || current.get(keys.hub_tag(MEASURED)).expect("cached");

            source.wait_samples(3).await;
            assert_eq!(measured().quality, Quality::Good);

            // Down, but still *holding* the last values: a Down link must
            // never let them through.
            source.set(ExternalSnapshot {
                link: crate::external::ExternalLink::Down {
                    detail: Some("Hub の購読: reconnecting".to_string()),
                },
                values: held_values(),
            });
            source.wait_samples(6).await;
            assert_eq!((measured().value, measured().quality), (None, Quality::Bad));
            for name in HUB_NAMES {
                assert_eq!(
                    current
                        .get(keys.hub_tag(name))
                        .map(|s| (s.value, s.quality)),
                    Some((None, Quality::Bad)),
                    "{name}"
                );
            }
            assert_eq!(
                collector.status().get(&keys.hub_conn),
                Some(&ConnectionStatus::Reconnecting {
                    attempt: crate::task::EXTERNAL_RECONNECT_ATTEMPT
                })
            );
            assert_eq!(
                current.get(&keys.plc_tag).map(|s| s.quality),
                Some(Quality::Good),
                "the PLC connection is unaffected"
            );

            source.set(ExternalSnapshot::live(held_values()));
            source.wait_samples(3).await;
            assert_eq!(
                (measured().value, measured().quality),
                (Some(7.0), Quality::Good)
            );

            collector.stop().await.expect("stop");

            let mut events = Vec::new();
            while let Ok(event) = rx.try_recv() {
                events.push(event);
            }
            assert_eq!(
                connection_kinds(&events, &keys.hub_conn),
                vec![
                    EventKind::PlcConnected,
                    EventKind::PlcDisconnected,
                    EventKind::PlcReconnected,
                ],
                "{events:?}"
            );
            let disconnect = events
                .iter()
                .find(|e| e.kind == EventKind::PlcDisconnected)
                .unwrap();
            assert_eq!(
                disconnect.detail.as_deref(),
                Some("Hub の購読: reconnecting")
            );

            let hub_rows = rows(&dir, &keys.hub_group).await;
            let measured_column: Vec<Option<f64>> = hub_rows.iter().map(|row| row[0]).collect();
            let nulls = measured_column.iter().filter(|v| v.is_none()).count();
            assert!(
                nulls >= 5,
                "the outage is recorded as null rows: {measured_column:?}"
            );
            assert!(measured_column.contains(&Some(7.0)));
            assert!(
                measured_column
                    .iter()
                    .all(|v| v.is_none() || *v == Some(7.0)),
                "{measured_column:?}"
            );
            assert_plc_rows_unaffected(&rows(&dir, &keys.plc_group).await);
        }

        /// PR #578 review: a collector started **without** a source refuses
        /// an `apply_config` that brings in a hub connection - before
        /// touching anything. Its config is unchanged, the PLC task keeps
        /// running and recording, and no hub task appears.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn apply_config_refuses_a_hub_connection_without_a_source() {
            // Build the PLC-only and the PLC + hub configs from one registry.
            let src_dir = TempDir::new();
            let source = MockSource::new(ExternalSnapshot::down(None));
            let (with_hub, _rx, keys) = start(&src_dir, source).await;
            let hub_config = with_hub.config.clone();
            with_hub.stop().await.expect("stop");
            let mut plc_only = hub_config.clone();
            plc_only
                .connections
                .retain(|c| c.config != ProtocolConfig::External);
            plc_only
                .store_config
                .groups
                .retain(|g| g.key != keys.hub_group);
            assert!(!plc_only.has_external_connections());

            let dir = TempDir::new();
            let reads = Arc::new(AtomicUsize::new(0));
            let factory = {
                let reads = reads.clone();
                Arc::new(move |_spec: &crate::task::ClientSpec| {
                    Box::new(FakeClient {
                        reads: reads.clone(),
                    }) as Box<dyn PlcClient>
                }) as ClientFactory
            };
            let pool = banto_storage::connect_sqlite_memory().await.unwrap();
            let mut collector = Collector::start_with_client_factory(
                plc_only.clone(),
                dir.path(),
                Arc::new(SystemClock),
                EventSink::new(pool),
                CollectorOptions::default(),
                factory.clone(),
            )
            .await
            .expect("start without sources");

            let result = collector.apply_config(hub_config, factory).await;
            assert!(
                matches!(&result, Err(CollectError::Config(m)) if m == MISSING_EXTERNAL_SOURCE),
                "{:?}",
                result.map(|r| r.added)
            );
            assert_eq!(collector.config, plc_only, "config unchanged");
            assert!(!collector.tasks.contains_key(&keys.hub_conn));
            assert_eq!(collector.status().get(&keys.hub_conn), None);

            // The PLC task is still running: its reads keep climbing.
            let before = reads.load(Ordering::SeqCst);
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            while reads.load(Ordering::SeqCst) < before + 3 {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "the PLC task stopped reading after the refused apply_config"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            collector.stop().await.expect("stop");
            assert_plc_rows_unaffected(&rows(&dir, &keys.plc_group).await);
        }

        /// A config with a hub connection is refused without a source (it
        /// could only ever read "not Live").
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_hub_config_without_a_source_is_refused() {
            let dir = TempDir::new();
            let source = MockSource::new(ExternalSnapshot::down(None));
            let (collector, _rx, _keys) = start(&dir, source).await;
            let config = collector.config.clone();
            collector.stop().await.expect("stop");

            let pool = banto_storage::connect_sqlite_memory().await.unwrap();
            let result = Collector::start_with_client_factory(
                config,
                dir.path(),
                Arc::new(SystemClock),
                EventSink::new(pool),
                CollectorOptions::default(),
                default_client_factory(),
            )
            .await;
            assert!(
                matches!(result, Err(CollectError::Config(_))),
                "{:?}",
                result.err()
            );
        }
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

    // --- #588: stopping several tasks signals all of them before any join --
    //
    // A connection task notices its stop signal only between reads, so a
    // task blocked on a PLC that never answers stops only when that read
    // gives up. These tests make "every task is signalled before the first
    // join" a hard precondition for finishing at all, independent of timing
    // and scheduling (see `GatedClient`); `tests/integration.rs` covers the
    // same thing against real unresponsive sockets, with a timing assertion.

    /// Counts how many of the given tasks' stop channels have been sent
    /// `true`, by watching receivers subscribed (in the test) to each
    /// [`ConnectionTask::stop_tx`] - the very channel `stop_tasks` sends on.
    /// Returns a receiver of that count.
    fn watch_stop_signals<'a>(
        tasks: impl IntoIterator<Item = &'a ConnectionTask>,
    ) -> watch::Receiver<usize> {
        let (count_tx, count_rx) = watch::channel(0usize);
        let count_tx = Arc::new(count_tx);
        for task in tasks {
            let mut stop_rx = task.stop_tx.subscribe();
            let count_tx = count_tx.clone();
            tokio::spawn(async move {
                if stop_rx.wait_for(|stop| *stop).await.is_ok() {
                    count_tx.send_modify(|count| *count += 1);
                }
            });
        }
        count_rx
    }

    /// A fake `PlcClient` for the #588 tests - a **barrier on the stop
    /// signals**. Its first `read_batch` blocks until `notified` (from
    /// [`watch_stop_signals`]) reaches `all`, i.e. until **every** task
    /// being stopped has been sent its stop signal, and then succeeds; every
    /// later `read_batch` never returns (a PLC that stopped answering).
    ///
    /// So no task can finish its read - and therefore no join can complete -
    /// before every task has been signalled. A stop that signals and joins
    /// one task at a time can never get past the first join (that task waits
    /// for signals the stop will only send after the join): it deadlocks,
    /// always, whatever the timing. A stop that signals everyone first lets
    /// every read finish; each task then sees its stop (the select in
    /// `run_connection` is `biased` towards it) before any second read.
    ///
    /// When `panic_on_read` is set the client panics in `read_batch`
    /// instead, so its task ends with a `JoinError`.
    struct GatedClient {
        nth_read: usize,
        in_first_read: Arc<AtomicUsize>,
        notified: Barrier,
        panic_on_read: bool,
    }

    impl PlcClient for GatedClient {
        fn connect(&mut self) -> BoxFuture<'_, Result<(), PlcError>> {
            Box::pin(async { Ok(()) })
        }

        fn read_batch<'a>(
            &'a mut self,
            requests: &'a [ReadRequest],
        ) -> BoxFuture<'a, Result<Vec<ReadResult>, PlcError>> {
            assert!(
                !self.panic_on_read,
                "GatedClient: deliberate panic for the #588 JoinError test"
            );
            self.nth_read += 1;
            let first = self.nth_read == 1;
            let in_first_read = self.in_first_read.clone();
            let notified = self.notified.clone();
            Box::pin(async move {
                if !first {
                    std::future::pending::<()>().await;
                }
                in_first_read.fetch_add(1, Ordering::SeqCst);
                // The barrier is armed by the test once every task is in
                // here (it needs the tasks' stop channels, which exist only
                // after start); poll until then.
                let (mut count_rx, all) = loop {
                    if let Some(armed) = notified.lock().expect("barrier lock").clone() {
                        break armed;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                };
                let _ = count_rx.wait_for(|count| *count >= all).await;
                Ok(requests
                    .iter()
                    .map(|_| ReadResult::Value(TagValue::F64(5.0)))
                    .collect())
            })
        }

        fn disconnect(&mut self) -> BoxFuture<'_, ()> {
            Box::pin(async {})
        }
    }

    /// The shared, initially unarmed stop-signal barrier of [`GatedClient`].
    type Barrier = Arc<std::sync::Mutex<Option<(watch::Receiver<usize>, usize)>>>;

    /// A [`GatedClient`] factory sharing one barrier and one counter; the
    /// connections whose key is in `panicking` get a panicking client, and
    /// those in `ungated` a plain [`FakeClient`].
    fn gated_factory(
        in_first_read: &Arc<AtomicUsize>,
        barrier: &Barrier,
        panicking: &[&str],
        ungated: &[&str],
    ) -> ClientFactory {
        let in_first_read = in_first_read.clone();
        let barrier = barrier.clone();
        let panicking: HashSet<String> = panicking.iter().map(|k| (*k).to_string()).collect();
        let ungated: HashSet<String> = ungated.iter().map(|k| (*k).to_string()).collect();
        Arc::new(move |spec| {
            if ungated.contains(&spec.connection_key) {
                return Box::new(FakeClient {
                    reads: Arc::new(AtomicUsize::new(0)),
                }) as Box<dyn PlcClient>;
            }
            Box::new(GatedClient {
                nth_read: 0,
                in_first_read: in_first_read.clone(),
                notified: barrier.clone(),
                panic_on_read: panicking.contains(&spec.connection_key),
            }) as Box<dyn PlcClient>
        })
    }

    /// Wait until `target` tasks are blocked in their first (gated) read.
    async fn wait_in_first_read(in_first_read: &Arc<AtomicUsize>, target: usize) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while in_first_read.load(Ordering::SeqCst) < target {
            assert!(
                tokio::time::Instant::now() < deadline,
                "only {} of {target} tasks reached their first read",
                in_first_read.load(Ordering::SeqCst)
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Arm `barrier` on the stop signals of the tasks under `keys`: from now
    /// on a gated read finishes only once all of them have been signalled.
    fn arm_barrier(collector: &Collector, barrier: &Barrier, keys: &[String]) {
        let count_rx = watch_stop_signals(keys.iter().map(|key| &collector.tasks[key]));
        *barrier.lock().expect("barrier lock") = Some((count_rx, keys.len()));
    }

    /// Give `fut` 10 s: past that it is deadlocked on the barrier.
    async fn within_barrier_deadline<F: std::future::Future>(fut: F, what: &str) -> F::Output {
        tokio::time::timeout(Duration::from_secs(10), fut)
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "{what} deadlocked: it awaited a join before signalling every task, \
                     and no task can finish before every task is signalled (#588)"
                )
            })
    }

    /// `n` connections `conn:1..=n`, each with its own one-tag group
    /// `grp:i` / `tag:i` - [`one_tag_config`] repeated.
    fn n_conn_config(n: usize) -> CollectorConfig {
        let mut connections = Vec::with_capacity(n);
        let mut groups = Vec::with_capacity(n);
        for i in 1..=n {
            let mut one = one_tag_config();
            let mut conn = one.connections.remove(0);
            conn.key = format!("conn:{i}");
            conn.groups[0].key = format!("grp:{i}");
            conn.groups[0].tags[0].key = format!("tag:{i}");
            connections.push(conn);
            let mut group = one.store_config.groups.remove(0);
            group.key = format!("grp:{i}");
            group.name = format!("G{i}");
            group.tags[0].key = format!("tag:{i}");
            groups.push(group);
        }
        CollectorConfig {
            connections,
            store_config: StoreConfig { groups },
        }
    }

    /// Writer options under which only `stop()`'s final flush can move rows
    /// to disk, so rows a post-stop reader sees prove that flush ran.
    fn flush_only_on_stop() -> CollectorOptions {
        CollectorOptions {
            writer_options: WriterOptions {
                max_buffered_rows: 100_000,
                flush_interval_ms: 3_600_000,
            },
            ..CollectorOptions::default()
        }
    }

    /// Start `n` gated connections (those in `panicking` panic instead),
    /// wait until every other one is blocked in its first read, arm the
    /// barrier on every task's stop signal (the panicked one's included),
    /// then `stop()`. Returns the live events seen.
    async fn stop_with_gated_reads(
        dir: &TempDir,
        n: usize,
        panicking: &[&str],
    ) -> Vec<crate::event::CollectEvent> {
        let in_first_read = Arc::new(AtomicUsize::new(0));
        let barrier: Barrier = Arc::default();
        let pool = banto_storage::connect_sqlite_memory()
            .await
            .expect("connect sqlite memory");
        let events = EventSink::new(pool);
        let mut rx = events.subscribe();
        let collector = Collector::start_with_client_factory(
            n_conn_config(n),
            dir.path(),
            Arc::new(SystemClock),
            events,
            flush_only_on_stop(),
            gated_factory(&in_first_read, &barrier, panicking, &[]),
        )
        .await
        .expect("start_with_client_factory should succeed");

        wait_in_first_read(&in_first_read, n - panicking.len()).await;
        let all: Vec<String> = collector.tasks.keys().cloned().collect();
        arm_barrier(&collector, &barrier, &all);
        within_barrier_deadline(collector.stop(), "stop()")
            .await
            .expect("stop should succeed");
        drain_events(&mut rx)
    }

    /// Per group in `data_dir` (read after `stop()`), how many rows carry
    /// [`GatedClient`]'s value - the one row its first read produced. (A
    /// group also gets all-null rows for ticks before its connection came
    /// up, which these tests do not count.)
    async fn value_rows_per_group(data_dir: &std::path::Path) -> HashMap<String, usize> {
        let mut counts = HashMap::new();
        for file in banto_tstore::list_data_files(data_dir).expect("list data files") {
            let reader = banto_tstore::TsReader::open(&file.path)
                .await
                .expect("open reader");
            let keys: Vec<String> = reader.groups().iter().map(|g| g.key.clone()).collect();
            for key in keys {
                let rows = reader
                    .read_range(&key, 0, i64::MAX)
                    .await
                    .expect("read range");
                let values = rows.iter().filter(|row| row.values[0] == Some(5.0)).count();
                *counts.entry(key).or_insert(0) += values;
            }
        }
        counts
    }

    /// **#588 regression.** `stop()` must signal every task before it awaits
    /// the first join - with the old signal-then-join loop it deadlocks on
    /// [`GatedClient`]'s barrier. The final flush and `collection_stopped`
    /// still come after every task, exactly once.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stop_signals_every_task_before_joining_any() {
        let dir = TempDir::new();
        let events = stop_with_gated_reads(&dir, 20, &[]).await;

        assert_eq!(
            count_kind(&events, crate::event::EventKind::CollectionStopped),
            1,
            "collection_stopped must be emitted exactly once: {events:?}"
        );
        assert_eq!(
            events.last().map(|e| e.kind),
            Some(crate::event::EventKind::CollectionStopped),
            "collection_stopped must come after every task's own events"
        );
        let counts = value_rows_per_group(dir.path()).await;
        for i in 1..=20 {
            let key = format!("grp:{i}");
            assert_eq!(
                counts.get(&key).copied(),
                Some(1),
                "{key}'s buffered value row must survive stop()'s final flush: {counts:?}"
            );
        }
    }

    /// #588: a task that panicked (its join yields a `JoinError`) must not
    /// keep the other tasks from being stopped, nor the final flush and
    /// `collection_stopped` from happening.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_panicked_task_does_not_block_stopping_the_rest() {
        let dir = TempDir::new();
        let events = stop_with_gated_reads(&dir, 5, &["conn:3"]).await;

        assert_eq!(
            count_kind(&events, crate::event::EventKind::CollectionStopped),
            1,
            "{events:?}"
        );
        let counts = value_rows_per_group(dir.path()).await;
        for i in [1, 2, 4, 5] {
            let key = format!("grp:{i}");
            assert_eq!(counts.get(&key).copied(), Some(1), "{key}: {counts:?}");
        }
        assert_eq!(
            counts.get("grp:3").copied().unwrap_or(0),
            0,
            "the panicked task never read a value: {counts:?}"
        );
    }

    /// #588: `apply_config`'s step 3 stops removed tasks the same way - all
    /// signalled before the first join, or it deadlocks on the barrier
    /// (armed on the removed tasks only).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn apply_config_signals_every_removed_task_before_joining_any() {
        let dir = TempDir::new();
        let in_first_read = Arc::new(AtomicUsize::new(0));
        let barrier: Barrier = Arc::default();
        // conn:1 stays and is never stopped by apply_config, so it reads
        // normally (a gated one would wait on a barrier nobody completes).
        let factory = gated_factory(&in_first_read, &barrier, &[], &["conn:1"]);
        let pool = banto_storage::connect_sqlite_memory()
            .await
            .expect("connect sqlite memory");
        let mut collector = Collector::start_with_client_factory(
            n_conn_config(10),
            dir.path(),
            Arc::new(SystemClock),
            EventSink::new(pool),
            flush_only_on_stop(),
            factory.clone(),
        )
        .await
        .expect("start_with_client_factory should succeed");
        wait_in_first_read(&in_first_read, 9).await;
        let mut removed: Vec<String> = (2..=10).map(|i| format!("conn:{i}")).collect();
        removed.sort(); // ApplyReport sorts its keys (conn:10 before conn:2)
        arm_barrier(&collector, &barrier, &removed);

        // Keep conn:1 only (conn:2..=10 are removed), same store config.
        let mut keep_one = n_conn_config(1);
        keep_one.store_config = n_conn_config(10).store_config;
        let report =
            within_barrier_deadline(collector.apply_config(keep_one, factory), "apply_config")
                .await
                .expect("apply_config should succeed");
        assert_eq!(report.removed, removed, "{report:?}");
        assert_eq!(report.unchanged, vec!["conn:1".to_string()]);
        assert!(!report.writer_rotated);
        assert_eq!(collector.tasks.len(), 1);
        collector.stop().await.expect("stop should succeed");
    }
}
