//! [`BrokerSessions`]: the broker sessions a collector reads through, for a
//! host that has no broker of its own (R2b-3a-3, docs/banto-hub-driver-io-design.md
//! §8: ChronoGazer). One `banto_broker::BrokerSupervisor` dialing through the
//! host's driver registry, plus the [`BrokerSimRegistry`] that substitutes a
//! simulator's address for a `simulation = true` connection before its session
//! is ensured.
//!
//! banto-hub keeps its own directory (`HubSessions`, shared with its read-now /
//! write paths and its status surface); its sync (`CollectorManager::
//! sync_broker_sessions_from`) follows the same steps as [`BrokerSessions::sync`]:
//! resolve the dial settings, stop and join the session when they changed, ensure, stamp
//! the plan with the fingerprint, and hand the handles to
//! [`crate::broker_client_factory`].
//!
//! ## Order of operations
//!
//! 1. [`BrokerSessions::sync`] before `Collector::start_with_client_factory` /
//!    `apply_config`: sessions for every broker-read plan, the plan stamped
//!    (`CollectorConfig::set_broker_dial_target`) so a moved session replaces
//!    the task that held the old handle.
//! 2. The collector commit.
//! 3. [`BrokerSessions::remove`] for the stale ids `sync` returned - only now,
//!    because a task reading through one of them has been stopped by step 2.
//! 4. On stopping collection, [`BrokerSessions::stop_all`] after the collector
//!    has stopped, so the PLCs are not left connected while nothing collects
//!    (the direct clients this replaced closed their sockets with the task).
//! 5. [`BrokerSessions::shutdown`] once, at the end of the process: the
//!    sessions first, then the simulators they dial.
//!
//! ## Changed settings
//!
//! When a planned connection's dial settings changed (its fingerprint, see
//! [`BrokerSimRegistry::resolve`]), step 1 stops its old broker task and waits
//! for it (`SessionDirectory::stop_and_join`) before ensuring the new one,
//! rather than only dropping the directory's entry (2026-10-24 owner
//! decision). Dropping the entry also signals the old task to stop (its
//! per-task stop sender goes with it, even though other clones of the handle
//! - the previous factory, which a sibling connection's collect task still
//! holds - live on), but does not wait: the old task's teardown (a running
//! job, the goodbye) could still hold its socket while the new session
//! dials, and a PLC that refuses a second connection on the same port (R08:
//! 10061) would refuse that first dial. The
//! wait is bounded: a job already running finishes within its own job limit,
//! queued jobs are answered with `TaskGone` (never success), a pending dial is
//! aborted, and the goodbye is bounded by `banto_broker::DISCONNECT_LIMIT`.
//! Until step 2 replaces the changed connection's collect task, that task
//! reads through the stopped handle and its reads fail (Bad).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use banto_broker::{BrokerOptions, BrokerSupervisor, ReadOnlyHandle, SessionDirectory};
use banto_driver_api::DriverRegistry;

use crate::broker_sim::{dial_fingerprint, BrokerSimRegistry, DialTarget};
use crate::client::{broker_client_factory, ClientFactory};
use crate::config::{CollectorConfig, RegistrySnapshot};

/// What [`BrokerSessions::sync`] produced for one collector commit.
pub struct SessionSync {
    /// The factory to hand to the collector: a broker read client for every
    /// connection whose session is up (see [`crate::broker_client_factory`]).
    pub factory: ClientFactory,
    /// Connection ids that still have a session but are no longer collected -
    /// to [`BrokerSessions::remove`] after the collector commit succeeded.
    pub stale: Vec<i64>,
    /// Connections whose session could not be started `(connection id,
    /// reason)`: they get a client that never connects (their tags read Bad)
    /// while collection of the others goes on. The caller logs them with its
    /// own logger - this crate no longer prints them.
    pub failures: Vec<(i64, String)>,
}

/// See this module's doc.
pub struct BrokerSessions {
    registry: Arc<DriverRegistry>,
    directory: SessionDirectory,
    /// `Some` until [`Self::shutdown`] consumes it (`BrokerSupervisor::shutdown`
    /// takes `self`); an async mutex because the shutdown awaits every task.
    supervisor: tokio::sync::Mutex<Option<BrokerSupervisor>>,
    sims: BrokerSimRegistry,
}

impl BrokerSessions {
    /// No sessions yet; [`Self::sync`] grows them. The supervisor dials
    /// through `registry` with `options` (backoff, the drivers' timeouts).
    pub fn new(registry: Arc<DriverRegistry>, options: BrokerOptions) -> Self {
        let supervisor = BrokerSupervisor::spawn_with(&[], registry.clone(), options)
            .expect("spawning a broker supervisor with zero connections cannot fail");
        let directory = supervisor.directory();
        Self {
            registry,
            directory,
            supervisor: tokio::sync::Mutex::new(Some(supervisor)),
            sims: BrokerSimRegistry::new(),
        }
    }

    /// The registry the sessions dial through (and the plan is built with).
    pub fn registry(&self) -> &Arc<DriverRegistry> {
        &self.registry
    }

    /// How many sessions are tracked (diagnostics and tests).
    pub fn connection_count(&self) -> usize {
        self.directory.connection_count()
    }

    /// The read-only handle of the session tracked for `connection_id`, if
    /// any (diagnostics and tests: watch one session's status across a sync).
    pub fn handle(&self, connection_id: i64) -> Option<ReadOnlyHandle> {
        self.directory.handle(connection_id).map(|h| h.read_only())
    }

    /// Step 1 of this module's doc: a session for every broker-read
    /// connection `config` plans, its dial settings resolved by the
    /// simulation registry (a changed target stops and joins the old session
    /// first - see this module's doc, "Changed settings"),
    /// and the plan stamped with the session's fingerprint. A connection
    /// whose session cannot be started is reported in
    /// [`SessionSync::failures`] and gets a client that never connects (its
    /// tags read Bad); collection of the others goes on.
    pub async fn sync(
        &self,
        snapshot: &RegistrySnapshot,
        config: &mut CollectorConfig,
    ) -> SessionSync {
        let planned = config.broker_connection_keys();
        let mut handles: HashMap<String, ReadOnlyHandle> = HashMap::new();
        let mut wanted: HashSet<i64> = HashSet::new();
        let mut failures: Vec<(i64, String)> = Vec::new();
        for conn in &snapshot.connections {
            let key = format!("conn:{}", conn.id);
            if !planned.contains(&key) {
                continue;
            }
            let (target, changed) = self.sims.resolve(conn, &self.registry).await;
            wanted.insert(conn.id);
            if changed {
                // Stop and join the old task before the new one dials: its
                // teardown must not still hold the old socket while the new
                // session connects - a PLC that refuses a second connection on
                // the same port would refuse the new one (this module's doc,
                // "Changed settings").
                self.directory.stop_and_join(conn.id).await;
            }
            config.set_broker_dial_target(&key, dial_fingerprint(conn, &target, &self.registry));
            let ensured = match target {
                DialTarget::Real(settings) => self
                    .directory
                    .ensure_connection_with_settings(conn, settings),
                DialTarget::Sim(settings, device) => self
                    .directory
                    .ensure_simulated_connection(conn, settings, device),
            };
            match ensured {
                Ok(handle) => {
                    handles.insert(key, handle.read_only());
                }
                Err(err) => failures.push((conn.id, err.to_string())),
            }
        }
        let stale = self
            .directory
            .connection_ids()
            .into_iter()
            .filter(|id| !wanted.contains(id))
            .collect();
        SessionSync {
            factory: broker_client_factory(Arc::new(handles)),
            stale,
            failures,
        }
    }

    /// Step 3: stop the sessions (and simulators) of `ids` and wait for their
    /// tasks. Call only after the collector no longer reads through them.
    pub async fn remove(&self, ids: &[i64]) {
        for &id in ids {
            self.directory.stop_and_join(id).await;
            self.sims.remove(id).await;
        }
    }

    /// Step 4: [`Self::remove`] for every tracked session.
    pub async fn stop_all(&self) {
        let ids = self.directory.connection_ids();
        self.remove(&ids).await;
    }

    /// Step 5: stop every session, then every simulator. A second call is a
    /// no-op for the sessions.
    pub async fn shutdown(&self) {
        let supervisor = self.supervisor.lock().await.take();
        if let Some(supervisor) = supervisor {
            supervisor.shutdown().await;
        }
        self.sims.shutdown().await;
    }
}
