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
use crate::task::{
    default_client_factory, run_connection, BackoffConfig, ClientFactory, ConnectionStatus,
    StatusMap, TaskContext,
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
                factory: factory.clone(),
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
    use tempfile::tempdir;

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
        let dir = tempdir().expect("tempdir");
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
}
