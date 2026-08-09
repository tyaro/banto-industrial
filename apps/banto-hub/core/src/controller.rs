//! T14-2 collection lifecycle state machine.
//!
//! `CollectionController` is deliberately a thin layer above
//! [`crate::hub::CollectorManager`]. The manager keeps ownership of the
//! collector, broker sessions, simulators, and computed engine; this module
//! owns only lifecycle state, transition serialization, and run identifiers.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
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

/// The configured collection mode. `AllSimulation` is the T15 extension
/// point; T14-2 provides its state-machine path but not its implementation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunMode {
    Configured,
    AllSimulation,
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
}

/// The serialized collection lifecycle controller.
pub struct CollectionController {
    manager: Arc<CollectorManager>,
    write_control: Arc<WriteControl>,
    state: Mutex<ControllerState>,
    transition: AsyncMutex<()>,
    run_seq: AtomicU64,
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
        }
    }

    /// Return the current state without waiting for an in-flight transition.
    pub fn status(&self) -> CollectionStatus {
        self.state
            .lock()
            .expect("collection controller state lock poisoned")
            .status()
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

        let result = match mode {
            RunMode::Configured => self.manager.rebuild().await,
            RunMode::AllSimulation => Err("all_simulation は T15 で実装されます".to_string()),
        };

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
        state.status()
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
        self.manager.stop().await;
        let mut state = self
            .state
            .lock()
            .expect("collection controller state lock poisoned");
        state.state = CollectionState::Stopped;
        state.context = None;
        state.last_error = None;
    }
}

impl ControllerState {
    fn status(&self) -> CollectionStatus {
        CollectionStatus {
            state: self.state,
            mode: self.mode,
            run_id: self.context.map(|context| context.run_id),
            last_error: self.last_error.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker_glue::{HubSessions, SlmpSimRegistry};
    use crate::computed::{ComputedEngine, ServerTagStore};
    use crate::db::init_db;
    use banto_collect::CollectorOptions;
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
    async fn all_simulation_has_a_faulted_path_without_auto_restart() {
        let (_dir, controller) = controller_env().await;

        let faulted = controller.start(RunMode::AllSimulation).await;
        assert_eq!(faulted.state, CollectionState::Faulted);
        assert_eq!(faulted.run_id, Some(1));
        assert!(faulted.last_error.is_some());
        assert_eq!(controller.status(), faulted);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn switching_from_configured_to_all_simulation_starts_the_new_mode() {
        let (_dir, controller) = controller_env().await;
        let running = controller.start(RunMode::Configured).await;
        assert_eq!(running.state, CollectionState::Running);
        assert_eq!(running.run_id, Some(1));

        let switched = controller.set_mode(RunMode::AllSimulation).await;

        // The mode switch must pass through Stopping → Stopped → Starting.
        // AllSimulation has no T14-2 implementation, so its new start ends
        // in Faulted rather than remaining Stopped after the mode change.
        assert_eq!(switched.state, CollectionState::Faulted);
        assert_eq!(switched.mode, RunMode::AllSimulation);
        assert_eq!(switched.run_id, Some(2));
        assert!(switched.last_error.is_some());
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
}
