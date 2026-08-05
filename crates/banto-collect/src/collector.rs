//! [`Collector`]: the running engine. Opens the time-series store, spawns one
//! task per connection, and exposes the three things the UI consumes - the
//! current-value cache, per-connection status, and the live event stream -
//! plus a clean [`Collector::stop`] that drains every task and flushes the
//! writer so no buffered row is lost.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use banto_tstore::{Clock, TsWriter, WriterOptions};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

use crate::config::{CollectorConfig, ProtocolConfig};
use crate::current::CurrentValuesHandle;
use crate::error::CollectError;
use crate::event::{CollectEvent, EventKind, EventSink};
use crate::task::{run_connection, BackoffConfig, ConnectionStatus, StatusMap, TaskContext};

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

/// A running collection engine. Self-driving after [`Collector::start`]
/// (recorder-requirements.md §4: runs independently of the UI); hold it for
/// the process lifetime and call [`Collector::stop`] to shut down cleanly.
pub struct Collector {
    // `Option` so `stop` can take sole ownership of the writer Arc to close
    // it (every task holds a clone until it exits).
    writer: Option<Arc<TsWriter>>,
    current: CurrentValuesHandle,
    status: StatusMap,
    events: EventSink,
    clock: Arc<dyn Clock>,
    stop_tx: watch::Sender<bool>,
    tasks: Vec<JoinHandle<()>>,
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
    pub async fn start(
        config: CollectorConfig,
        data_dir: &Path,
        clock: Arc<dyn Clock>,
        events: EventSink,
        options: CollectorOptions,
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

        let current = CurrentValuesHandle::new(clock.clone());
        let status: StatusMap = Arc::new(RwLock::new(HashMap::new()));
        let (stop_tx, stop_rx) = watch::channel(false);

        events
            .emit(CollectEvent::lifecycle(
                clock.now_ms(),
                EventKind::CollectionStarted,
            ))
            .await;

        let mut tasks = Vec::with_capacity(config.connections.len());
        for mut plan in config.connections {
            // Apply the option timeouts to this connection's client config -
            // one match arm per protocol (I8: SLMP joins Modbus TCP), same
            // uniform override either way.
            match &mut plan.config {
                ProtocolConfig::ModbusTcp(cfg) => {
                    cfg.connect_timeout = options.connect_timeout;
                    cfg.response_timeout = options.response_timeout;
                }
                ProtocolConfig::Slmp(cfg) => {
                    cfg.connect_timeout = options.connect_timeout;
                    cfg.response_timeout = options.response_timeout;
                }
            }

            let ctx = TaskContext {
                writer: writer.clone(),
                clock: clock.clone(),
                current: current.clone(),
                events: events.clone(),
                status: status.clone(),
                backoff: options.backoff,
            };
            let stop_rx = stop_rx.clone();
            tasks.push(tokio::spawn(run_connection(plan, ctx, stop_rx)));
        }

        Ok(Self {
            writer: Some(writer),
            current,
            status,
            events,
            clock,
            stop_tx,
            tasks,
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
        // marks itself Stopped on the way out, releasing its writer Arc clone.
        let _ = self.stop_tx.send(true);
        for task in self.tasks.drain(..) {
            let _ = task.await;
        }

        // Every task has now dropped its writer clone, so we hold the sole
        // reference - unwrap it and close (final flush) exactly once.
        if let Some(writer) = self.writer.take() {
            match Arc::try_unwrap(writer) {
                Ok(writer) => writer.close().await?,
                Err(still_shared) => {
                    // Should be unreachable (all tasks joined above). Flush at
                    // least, so buffered rows are not lost even if some clone
                    // unexpectedly outlived its task.
                    still_shared.flush().await?;
                }
            }
        }

        self.events
            .emit(CollectEvent::lifecycle(
                self.clock.now_ms(),
                EventKind::CollectionStopped,
            ))
            .await;
        Ok(())
    }
}
