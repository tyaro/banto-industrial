//! T14-2 collection lifecycle state machine.
//!
//! `CollectionController` is deliberately a thin layer above
//! [`crate::hub::CollectorManager`]. The manager keeps ownership of the
//! collector, broker sessions, simulators, and computed engine; this module
//! owns only lifecycle state, transition serialization, and run identifiers.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tokio::sync::watch;
use tokio::sync::Mutex as AsyncMutex;

use crate::hub::CollectorManager;
use crate::write_control::WriteControl;

/// A collection lifecycle state.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CollectionState {
    Stopped,
    Starting,
    Running,
    Stopping,
    Faulted,
}

impl CollectionState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Stopping => "stopping",
            Self::Faulted => "faulted",
        }
    }
}

/// The configured collection mode. `AllSimulation` applies a non-persistent
/// simulation override to enabled physical connections for one run.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunMode {
    Configured,
    AllSimulation,
}

impl RunMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Configured => "configured",
            Self::AllSimulation => "all_simulation",
        }
    }
}

/// Monotonically allocated identifier for one start attempt.
pub type RunId = u64;

/// Identity of the current start attempt while it is starting, running, or
/// faulted. A stopped controller has no active run context.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct RunContext {
    pub mode: RunMode,
    pub run_id: RunId,
}

/// Snapshot returned by lifecycle operations and available through
/// [`CollectionController::status`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CollectionStatus {
    pub state: CollectionState,
    pub mode: RunMode,
    pub run_id: Option<RunId>,
    pub last_error: Option<String>,
    pub configured_revision: u64,
    pub running_revision: u64,
}

/// Public name used by runtime/status consumers for the lifecycle snapshot.
pub type RuntimeStatus = CollectionStatus;

/// The serialized collection lifecycle controller.
pub struct CollectionController {
    manager: Arc<CollectorManager>,
    write_control: Arc<WriteControl>,
    state: Mutex<ControllerState>,
    transition: AsyncMutex<()>,
    run_seq: AtomicU64,
    status_tx: watch::Sender<RuntimeStatus>,
}

#[derive(Clone, Debug)]
struct ControllerState {
    state: CollectionState,
    mode: RunMode,
    context: Option<RunContext>,
    last_error: Option<String>,
}

impl CollectionController {
    pub fn new(manager: Arc<CollectorManager>, write_control: Arc<WriteControl>) -> Self {
        let initial = CollectionStatus {
            state: CollectionState::Stopped,
            mode: RunMode::Configured,
            run_id: None,
            last_error: None,
            configured_revision: manager.configured_revision(),
            running_revision: manager.running_revision(),
        };
        let (status_tx, _status_rx) = watch::channel(initial);
        Self {
            manager,
            write_control,
            state: Mutex::new(ControllerState {
                state: CollectionState::Stopped,
                mode: RunMode::Configured,
                context: None,
                last_error: None,
            }),
            transition: AsyncMutex::new(()),
            run_seq: AtomicU64::new(0),
            status_tx,
        }
    }

    /// Return the current state without waiting for an in-flight transition.
    pub fn status(&self) -> CollectionStatus {
        let mut status = self
            .state
            .lock()
            .expect("collection controller state lock poisoned")
            .status();
        status.configured_revision = self.manager.configured_revision();
        status.running_revision = self.manager.running_revision();
        status
    }

    /// Subscribe to lifecycle transitions. Catalog-only commits do not start
    /// a run and therefore do not cause a running transition notification.
    pub fn subscribe_status(&self) -> watch::Receiver<RuntimeStatus> {
        self.status_tx.subscribe()
    }

    /// Refresh the status watch after an external catalog-only commit.
    pub fn refresh_status(&self) {
        self.status_tx.send_replace(self.status());
    }

    /// Start the requested mode. A request arriving during another transition
    /// is not queued; it returns the state already published by that
    /// transition. Starting the same mode while running is idempotent.
    pub async fn start(&self, mode: RunMode) -> CollectionStatus {
        let Ok(_guard) = self.transition.try_lock() else {
            return self.status();
        };

        if self.status().state == CollectionState::Starting
            || self.status().state == CollectionState::Stopping
        {
            return self.status();
        }

        if self.status().state == CollectionState::Running && self.status().mode == mode {
            return self.status();
        }

        if self.status().state == CollectionState::Running {
            self.stop_locked().await;
        }

        self.start_locked(mode).await
    }

    /// Stop the current run. The collector is flushed first, then every
    /// broker session is stopped and joined. Repeated stops are no-ops.
    pub async fn stop(&self) -> CollectionStatus {
        let Ok(_guard) = self.transition.try_lock() else {
            return self.status();
        };

        if matches!(
            self.status().state,
            CollectionState::Stopped | CollectionState::Stopping
        ) {
            return self.status();
        }

        self.stop_locked().await;
        self.status()
    }

    /// Select a mode while stopped. A running mode switch is serialized as
    /// stop → stopped → start, so configured and all-simulation never switch
    /// in place.
    pub async fn set_mode(&self, mode: RunMode) -> CollectionStatus {
        let Ok(_guard) = self.transition.try_lock() else {
            return self.status();
        };

        let current = self.status();
        if current.state == CollectionState::Stopped {
            if current.mode != mode {
                self.write_control.disable();
            }
            self.set_mode_locked(mode);
            self.publish_status();
            return self.status();
        }
        if current.state == CollectionState::Running && current.mode == mode {
            return current;
        }
        if matches!(
            current.state,
            CollectionState::Starting | CollectionState::Stopping
        ) {
            return current;
        }

        self.stop_locked().await;
        self.set_mode_locked(mode);
        self.start_locked(mode).await
    }

    fn set_mode_locked(&self, mode: RunMode) {
        let mut state = self
            .state
            .lock()
            .expect("collection controller state lock poisoned");
        state.mode = mode;
        state.context = None;
        state.last_error = None;
    }

    async fn start_locked(&self, mode: RunMode) -> CollectionStatus {
        let run_id = self.run_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let context = RunContext { mode, run_id };
        {
            let mut state = self
                .state
                .lock()
                .expect("collection controller state lock poisoned");
            state.state = CollectionState::Starting;
            state.mode = mode;
            state.context = Some(context);
            state.last_error = None;
        }
        self.write_control.disable();
        self.publish_status();

        let result = self.manager.apply_run(mode).await;

        let mut state = self
            .state
            .lock()
            .expect("collection controller state lock poisoned");
        match result {
            Ok(()) => {
                state.state = CollectionState::Running;
                state.last_error = None;
            }
            Err(error) => {
                state.state = CollectionState::Faulted;
                state.last_error = Some(error);
            }
        }
        drop(state);
        self.publish_status();
        self.status()
    }

    async fn stop_locked(&self) {
        {
            let mut state = self
                .state
                .lock()
                .expect("collection controller state lock poisoned");
            state.state = CollectionState::Stopping;
        }
        self.write_control.disable();
        self.publish_status();
        self.manager.stop().await;
        self.manager.advance_running_revision();
        let mut state = self
            .state
            .lock()
            .expect("collection controller state lock poisoned");
        state.state = CollectionState::Stopped;
        state.context = None;
        state.last_error = None;
        drop(state);
        self.publish_status();
    }

    fn publish_status(&self) {
        self.status_tx.send_replace(self.status());
    }
}

impl ControllerState {
    fn status(&self) -> CollectionStatus {
        CollectionStatus {
            state: self.state,
            mode: self.mode,
            run_id: self.context.map(|context| context.run_id),
            last_error: self.last_error.clone(),
            configured_revision: 0,
            running_revision: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker_glue::{HubSessions, SlmpSimRegistry};
    use crate::computed::{ComputedEngine, ServerTagStore};
    use crate::db::init_db;
    use banto_collect::{CollectorOptions, RegistrySnapshot};
    use banto_tstore::SystemClock;
    use std::time::Duration;

    async fn controller_env() -> (crate::test_support::TempDir, Arc<CollectionController>) {
        let dir = crate::test_support::TempDir::new("collection-controller");
        let pool = init_db(&dir.path().join("registry.sqlite3"))
            .await
            .expect("init_db");
        let manager = Arc::new(CollectorManager::new(
            pool,
            dir.path().join("data"),
            Arc::new(SystemClock),
            CollectorOptions {
                connect_timeout: Duration::from_millis(50),
                response_timeout: Duration::from_millis(50),
                ..CollectorOptions::default()
            },
            Arc::new(HubSessions::new(banto_broker::BackoffConfig::default())),
            Arc::new(SlmpSimRegistry::new()),
            Arc::new(ComputedEngine::new(Arc::new(ServerTagStore::new()))),
        ));
        let write_control = Arc::new(WriteControl::new(true));
        let controller = Arc::new(CollectionController::new(manager, write_control.clone()));
        write_control.enable();
        (dir, controller)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn start_stop_is_idempotent_and_run_ids_are_not_reused() {
        let (_dir, controller) = controller_env().await;

        let first = controller.start(RunMode::Configured).await;
        assert_eq!(first.state, CollectionState::Running);
        assert_eq!(first.run_id, Some(1));
        assert!(!controller.write_control.is_enabled());

        let repeated = controller.start(RunMode::Configured).await;
        assert_eq!(repeated, first);

        let stopped = controller.stop().await;
        assert_eq!(stopped.state, CollectionState::Stopped);
        assert_eq!(stopped.run_id, None);

        let second = controller.start(RunMode::Configured).await;
        assert_eq!(second.state, CollectionState::Running);
        assert_eq!(second.run_id, Some(2));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn all_simulation_starts_without_auto_restart() {
        let (_dir, controller) = controller_env().await;

        let running = controller.start(RunMode::AllSimulation).await;
        assert_eq!(running.state, CollectionState::Running);
        assert_eq!(running.run_id, Some(1));
        assert!(running.last_error.is_none());
        assert_eq!(controller.status(), running);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn switching_from_configured_to_all_simulation_starts_the_new_mode() {
        let (_dir, controller) = controller_env().await;
        let running = controller.start(RunMode::Configured).await;
        assert_eq!(running.state, CollectionState::Running);
        assert_eq!(running.run_id, Some(1));

        let switched = controller.set_mode(RunMode::AllSimulation).await;

        // The mode switch must pass through Stopping → Stopped → Starting.
        assert_eq!(switched.state, CollectionState::Running);
        assert_eq!(switched.mode, RunMode::AllSimulation);
        assert_eq!(switched.run_id, Some(2));
        assert!(switched.last_error.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_starts_do_not_allocate_two_running_ids() {
        let (_dir, controller) = controller_env().await;
        let left = controller.clone();
        let right = controller.clone();
        let (left, right) = tokio::join!(
            left.start(RunMode::Configured),
            right.start(RunMode::Configured)
        );

        assert!(matches!(
            left.state,
            CollectionState::Starting | CollectionState::Running
        ));
        assert!(matches!(
            right.state,
            CollectionState::Starting | CollectionState::Running
        ));
        let final_status = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let status = controller.status();
                if status.state != CollectionState::Starting {
                    break status;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the first start should complete");
        assert_eq!(final_status.state, CollectionState::Running);
        assert_eq!(final_status.run_id, Some(1));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn configured_and_running_revisions_are_separate() {
        let (_dir, controller) = controller_env().await;
        let snapshot = RegistrySnapshot::load(&controller.manager.pool())
            .await
            .expect("empty registry snapshot");

        assert_eq!(controller.status().configured_revision, 0);
        assert_eq!(controller.status().running_revision, 0);
        controller
            .manager
            .commit_catalog(&snapshot)
            .await
            .expect("catalog commit");
        controller.refresh_status();

        let stopped = controller.status();
        assert_eq!(stopped.state, CollectionState::Stopped);
        assert_eq!(stopped.configured_revision, 1);
        assert_eq!(stopped.running_revision, 0);
        assert!(controller.manager.current_values().is_none());

        let running = controller.start(RunMode::Configured).await;
        assert_eq!(running.state, CollectionState::Running);
        assert_eq!(running.configured_revision, 1);
        assert_eq!(running.running_revision, 1);

        let stopped = controller.stop().await;
        assert_eq!(stopped.state, CollectionState::Stopped);
        assert_eq!(stopped.configured_revision, 1);
        assert_eq!(stopped.running_revision, 2);
    }
}
