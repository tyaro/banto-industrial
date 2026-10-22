//! C16 across the real Hub gate/audit path, broker, FrameSession and TCP.
mod common;
use banto_driver_api::{DataType, DriverBase, DriverEntry, TagKind, TagSpec, Value};
use banto_driver_conformance::{
    fake::echo::{EchoFrameDriver, EchoResponder},
    frame::{Reply, Responder},
};
use banto_hub_core::{
    api_keys::{ApiKeyContext, ApiKeysService},
    broker_glue::{BrokerSimRegistry, HubDrivers, HubSessions},
    computed::{ComputedEngine, ServerTagStore},
    db::init_db,
    hub::CollectorManager,
    write_audit::WriteAuditService,
    write_control::WriteControl,
    write_path::{execute_write, RequestedValue, WriteDeps, WriteRejection},
    write_rate::{WriteRateLimitConfig, WriteRateLimiter},
};

use banto_tags::{
    CollectionGroupInput, CollectionGroupService, PlcConnectionService, TagInput, TagService,
};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{broadcast, mpsc, oneshot, Mutex},
};

struct Cleanup {
    manager: Arc<CollectorManager>,
    pool: sqlx::SqlitePool,
    server: tokio::task::AbortHandle,
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        self.server.abort();
        common::shutdown_test_app(&self.manager, &self.pool);
    }
}
#[derive(Clone, Copy, Debug)]
enum Mode {
    Healthy,
    Timeout,
    Fatal,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c16_hub_gates_and_audit_precede_exclusive_frame_rmw_and_confirmation() {
    tokio::time::timeout(Duration::from_secs(30), run(Mode::Healthy))
        .await
        .unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c16_hub_verify_timeout_is_failed_and_unknown_write_is_not_replayed() {
    tokio::time::timeout(Duration::from_secs(30), run(Mode::Timeout))
        .await
        .unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c16_hub_verify_fatal_is_failed_and_unknown_write_is_not_replayed() {
    tokio::time::timeout(Duration::from_secs(30), run(Mode::Fatal))
        .await
        .unwrap();
}

/// The first RMW read is held until the test observes the pending audit and
/// queues a competing broker read. This avoids sleep-based ordering assertions.
async fn run(mode: Mode) {
    let env = common::TempEnv::new("hub-frame-write", "c16");
    let pool = init_db(env.registry_path()).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (wire_tx, mut wire_rx) = mpsc::unbounded_channel();
    let (release_tx, release_rx) = oneshot::channel();
    let server_pool = pool.clone();
    let server = tokio::spawn(async move {
        let mut peer = EchoResponder::new();
        peer.set_word('W', 10, 0b1010);
        let mut release = Some(release_rx);
        let mut fail_once = !matches!(mode, Mode::Healthy);
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            peer.on_connect();
            let mut reads = 0;
            loop {
                let mut header = [0; 6];
                if stream.read_exact(&mut header).await.is_err() {
                    break;
                }
                let mut request = header.to_vec();
                let len = u16::from_be_bytes([header[4], header[5]]) as usize;
                request.resize(6 + len, 0);
                stream.read_exact(&mut request[6..]).await.unwrap();
                let pending: Option<String> = if header[1] == b'R' && release.is_some() {
                    sqlx::query_scalar("SELECT result FROM hub_write_audit WHERE action='write' ORDER BY id DESC LIMIT 1").fetch_optional(&server_pool).await.unwrap()
                } else {
                    None
                };
                wire_tx.send((request.clone(), pending)).unwrap();
                if header[1] == b'R' {
                    reads += 1;
                    if let Some(release) = release.take() {
                        release.await.unwrap();
                    }
                }
                let Reply::Bytes(mut reply) = peer.reply(&request) else {
                    panic!("healthy echo response");
                };
                // The write was acknowledged and applied; only confirmation fails.
                if reads == 2 && fail_once {
                    fail_once = false;
                    if matches!(mode, Mode::Timeout) {
                        continue;
                    }
                    reply[0] = 0; // well-sized but fatally malformed confirmation
                }
                if stream.write_all(&reply).await.is_err() {
                    break;
                }
            }
        }
    });
    let mut drivers = HubDrivers::none();
    Arc::get_mut(&mut drivers.registry)
        .unwrap()
        .register(DriverEntry::frame(Arc::new(EchoFrameDriver::new())))
        .unwrap();
    let registry = drivers.registry.clone();
    let sessions = Arc::new(HubSessions::new(
        banto_broker::BackoffConfig::default(),
        drivers,
    ));
    // The legacy flat create input only has built-in protocol settings.
    // Store the custom test driver's JSON fixture directly, then exercise
    // the normal registry-backed reads, catalog, gates and write path.
    let settings = serde_json::json!({"host":"127.0.0.1","port":port,"word_order":"high_low"});
    let conn_id =
        sqlx::query("INSERT INTO plc_connections(name,protocol,settings) VALUES ('line','echo',?)")
            .bind(settings.to_string())
            .execute(&pool)
            .await
            .unwrap()
            .last_insert_rowid();
    let conn = PlcConnectionService::with_registry(pool.clone(), registry.clone())
        .get(conn_id)
        .await
        .unwrap();
    let group = CollectionGroupService::with_registry(pool.clone(), registry.clone())
        .create(CollectionGroupInput {
            name: "slow".into(),
            plc_connection_id: conn.id,
            period_ms: 60000,
            enabled: true,
            default_writable: true,
            query_sql: None,
        })
        .await
        .unwrap();
    TagService::with_registry(pool.clone(), registry)
        .create(TagInput {
            name: "bit".into(),
            collection_group_id: group.id,
            address: "W10.0".into(),
            data_type: "bit".into(),
            string_length: None,
            string_encoding: "utf8".into(),
            raw_lo: None,
            raw_hi: None,
            eng_lo: None,
            eng_hi: None,
            unit: None,
            decimals: 0,
            enabled: true,
            writable: true,
            tag_kind: "plc".into(),
            expression: None,
            retain: false,
            expected_revision: None,
        })
        .await
        .unwrap();
    let manager = Arc::new(CollectorManager::new(
        pool.clone(),
        env.data_dir(),
        Arc::new(banto_tstore::SystemClock),
        banto_collect::CollectorOptions::default(),
        sessions.clone(),
        Arc::new(BrokerSimRegistry::new()),
        Arc::new(ComputedEngine::new(Arc::new(ServerTagStore::new()))),
    ));
    manager
        .commit_catalog(&banto_collect::RegistrySnapshot::load(&pool).await.unwrap())
        .await
        .unwrap();
    let _cleanup = Cleanup {
        manager: manager.clone(),
        pool: pool.clone(),
        server: server.abort_handle(),
    };
    let handle = sessions.ensure_connection(&conn).unwrap();
    tokio::time::timeout(
        Duration::from_secs(3),
        handle
            .status_watch()
            .wait_for(|s| *s == banto_broker::BrokerConnectionStatus::Connected),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(wire_rx.recv().await.unwrap().0[1], b'H');
    let keys = ApiKeysService::new(pool.clone());
    let audit = WriteAuditService::new(pool.clone());
    let control = WriteControl::new(false);
    let limiter = Mutex::new(WriteRateLimiter::new(WriteRateLimitConfig::default()));
    let (events, _) = broadcast::channel(16);
    let deps = WriteDeps {
        manager: &manager,
        collection_controller: None,
        api_keys: &keys,
        write_audit: &audit,
        write_control: &control,
        rate_limiter: &limiter,
        events: &events,
    };
    let ctx = ApiKeyContext {
        id: 1,
        name: "writer".into(),
        scopes: vec!["write:line.slow.bit".into()],
        last_used_at_ms: None,
    };
    assert!(matches!(
        execute_write(
            &deps,
            &ctx,
            "line.slow.bit",
            Some(RequestedValue::Bool(true))
        )
        .await,
        Err(WriteRejection::WritesDisabled)
    ));
    assert!(
        wire_rx.try_recv().is_err(),
        "gate refusal must not send any frame"
    );
    control.enable();
    sqlx::query("CREATE TRIGGER refuse_write_audit BEFORE INSERT ON hub_write_audit BEGIN SELECT RAISE(FAIL, 'audit unavailable'); END").execute(&pool).await.unwrap();
    assert!(matches!(
        execute_write(
            &deps,
            &ctx,
            "line.slow.bit",
            Some(RequestedValue::Bool(true))
        )
        .await,
        Err(WriteRejection::AuditWriteFailed)
    ));
    assert!(
        wire_rx.try_recv().is_err(),
        "failed log-before-write must not send any frame"
    );
    sqlx::query("DROP TRIGGER refuse_write_audit")
        .execute(&pool)
        .await
        .unwrap();
    let write = execute_write(
        &deps,
        &ctx,
        "line.slow.bit",
        Some(RequestedValue::Bool(true)),
    );
    tokio::pin!(write);
    let first = tokio::select! { biased; result = &mut write => panic!("write returned before RMW response: {result:?}"), frame = wire_rx.recv() => frame.unwrap() };
    assert_eq!(first.0[1], b'R');
    assert_eq!(
        first.1.as_deref(),
        Some("failed"),
        "audit row must already exist when first frame reaches peer"
    );
    let pending: String = sqlx::query_scalar(
        "SELECT result FROM hub_write_audit WHERE action='write' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        pending, "failed",
        "the durable provisional row exists before first I/O completes"
    );
    let driver = EchoFrameDriver::new();
    let kind = TagKind::numeric(DataType::U16);
    let read_spec = TagSpec::new(driver.parse_address("W10", &kind).unwrap(), kind);
    // Poll the competing request so it is enqueued before releasing the RMW read.
    let read = handle.read(vec![read_spec]);
    tokio::pin!(read);
    tokio::select! { biased; result = &mut read => panic!("read interleaved: {result:?}"), _ = tokio::task::yield_now() => {} }
    release_tx.send(()).unwrap();
    let (written, read) = tokio::join!(&mut write, &mut read);
    let mut codes = vec![b'R'];
    for _ in 0..2 {
        codes.push(wire_rx.recv().await.unwrap().0[1]);
    }
    assert_eq!(
        codes, b"RWR",
        "RMW read/write/confirm is one exclusive logical job"
    );
    let result: String = sqlx::query_scalar(
        "SELECT result FROM hub_write_audit WHERE action='write' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    if matches!(mode, Mode::Healthy) {
        written.unwrap();
        assert_eq!(
            read.unwrap(),
            vec![Ok(Value::F64(11.))],
            "unrelated bits preserved"
        );
        assert_eq!(
            wire_rx.recv().await.unwrap().0[1],
            b'R',
            "competing read follows confirmation"
        );
        assert_eq!(result, "ok");
    } else {
        assert!(
            matches!(written, Err(WriteRejection::WriteFailed(_))),
            "{written:?}"
        );
        if let Err(error) = &read {
            assert!(
                matches!(
                    error,
                    banto_broker::BrokerError::Disconnected { .. }
                        | banto_broker::BrokerError::ConnectionFailed { .. }
                ),
                "{error:?}"
            );
        }
        assert_eq!(result, "failed");
        tokio::time::timeout(
            Duration::from_secs(3),
            handle
                .status_watch()
                .wait_for(|s| *s == banto_broker::BrokerConnectionStatus::Connected),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            wire_rx.recv().await.unwrap().0[1],
            b'H',
            "reconnect has a fresh handshake"
        );
        if let Ok(values) = &read {
            assert_eq!(values, &vec![Ok(Value::F64(11.))]);
            assert_eq!(
                wire_rx.recv().await.unwrap().0[1],
                b'R',
                "queued read may complete only on the fresh connection"
            );
        }
        assert!(
            wire_rx.try_recv().is_err(),
            "unknown write is never automatically replayed"
        );
        // A fresh read is safe and sees the already applied write, proving
        // that this case really is a result-unknown write, rather than no write.
        let kind = TagKind::numeric(DataType::U16);
        let spec = TagSpec::new(driver.parse_address("W10", &kind).unwrap(), kind);
        assert_eq!(
            handle.read(vec![spec]).await.unwrap(),
            vec![Ok(Value::F64(11.))]
        );
        assert_eq!(wire_rx.recv().await.unwrap().0[1], b'R');
        assert!(wire_rx.try_recv().is_err());
    }
    manager.shutdown().await;
    sessions.shutdown().await;
    server.abort();
    let _ = server.await;
    pool.close().await;
}
