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
//! resolve the dial settings, drop the session when they changed, ensure, stamp
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

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use banto_broker::{BrokerOptions, BrokerSupervisor, ReadOnlyHandle, SessionDirectory};
use banto_driver_api::DriverRegistry;

use crate::broker_sim::{dial_fingerprint, BrokerSimRegistry};
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

    /// Step 1 of this module's doc: a session for every broker-read
    /// connection `config` plans, its dial settings resolved by the
    /// simulation registry (a changed target drops the old session first),
    /// and the plan stamped with the session's fingerprint. A connection
    /// whose session cannot be started is logged and gets a client that
    /// never connects (its tags read Bad); collection of the others goes on.
    pub async fn sync(
        &self,
        snapshot: &RegistrySnapshot,
        config: &mut CollectorConfig,
    ) -> SessionSync {
        let planned = config.broker_connection_keys();
        let mut handles: HashMap<String, ReadOnlyHandle> = HashMap::new();
        let mut wanted: HashSet<i64> = HashSet::new();
        for conn in &snapshot.connections {
            let key = format!("conn:{}", conn.id);
            if !planned.contains(&key) {
                continue;
            }
            // A `simulation = true` connection without a simulator is not
            // planned (`resolve_connection`), so it never gets here; should it
            // anyway, `resolve` refuses it and no session is ensured (it is
            // left out of `wanted`, so an old session goes as stale).
            let Some((settings, changed)) = self.sims.resolve(conn, &self.registry).await else {
                self.directory.remove(conn.id);
                continue;
            };
            wanted.insert(conn.id);
            if changed {
                self.directory.remove(conn.id);
            }
            config.set_broker_dial_target(&key, dial_fingerprint(conn, &settings, &self.registry));
            match self
                .directory
                .ensure_connection_with_settings(conn, settings)
            {
                Ok(handle) => {
                    handles.insert(key, handle.read_only());
                }
                Err(err) => eprintln!(
                    "banto-collect: ブローカーセッションの起動に失敗しました (接続 {}): {err}",
                    conn.id
                ),
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
