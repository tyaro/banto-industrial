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
async fn c16_hub_verify_timeout_is_uncertain_and_unknown_write_is_not_replayed() {
    tokio::time::timeout(Duration::from_secs(30), run(Mode::Timeout))
        .await
        .unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c16_hub_verify_fatal_is_uncertain_and_unknown_write_is_not_replayed() {
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
            comment: None,
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
        prefix: "kkkkkkkk".to_string(),
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
        // #898: the write was handed to the device and the outcome became
        // unknown (timeout/fatal after send) - `uncertain`, not `failed`.
        assert_eq!(result, "uncertain");
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
            tokio::time::timeout(Duration::from_millis(100), wire_rx.recv())
                .await
                .is_err(),
            "unknown write must not be replayed, including after reconnect; peer must stay alive"
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
        assert!(
            tokio::time::timeout(Duration::from_millis(100), wire_rx.recv())
                .await
                .is_err(),
            "no delayed replay after the explicit read; peer must stay alive"
        );
    }
    manager.shutdown().await;
    sessions.shutdown().await;
    server.abort();
    let _ = server.await;
    pool.close().await;
}

/// Echo ドライバの `W` 領域を Modbus の保持レジスタ(area `holding_register`)
/// に見せ替えるだけのラッパー(#899)。Hub の write_path は area と bit の
/// 有無で FC22 のヒントを付けるか決めるので、実経路でそれを確かめるために、
/// 送受信は Echo のまま、`DriverAddress` の area だけ差し替える。
struct HoldingRegisterAlias(EchoFrameDriver);
impl DriverBase for HoldingRegisterAlias {
    fn descriptor(&self) -> &banto_driver_api::DriverDescriptor {
        self.0.descriptor()
    }
    fn validate_config(
        &self,
        settings: &banto_driver_api::Settings,
    ) -> Vec<banto_driver_api::FieldError> {
        self.0.validate_config(settings)
    }
    fn parse_address(
        &self,
        text: &str,
        kind: &TagKind,
    ) -> Result<banto_driver_api::DriverAddress, banto_driver_api::AddressError> {
        let mut address = self.0.parse_address(text, kind)?;
        address.area = "holding_register".into();
        Ok(address)
    }
    fn offset_address(
        &self,
        canonical: &str,
        kind: &TagKind,
        n: i64,
    ) -> Result<String, banto_driver_api::AddressError> {
        self.0.offset_address(canonical, kind, n)
    }
    fn address_help(&self) -> String {
        self.0.address_help()
    }
}
impl banto_driver_api::FrameDriver for HoldingRegisterAlias {
    fn create_connection(
        &self,
        settings: &banto_driver_api::Settings,
    ) -> Result<Box<dyn banto_driver_api::FrameConnection>, banto_driver_api::FatalError> {
        self.0.create_connection(settings)
    }
}

/// #899: 保持レジスタのビットの書込みが応答なし(`FatalError::Timeout`)で
/// 終わったとき、実際の単票・バッチの経路で、そのビットの項目だけに FC22 の
/// ヒントが付き(拒否の理由と監査の `detail`)、同じバッチのワードには付かず、
/// 監査の結果はどちらも `uncertain` のまま。項目の並びのずれや `false` 固定を
/// 検出するための試験(ヘルパーだけの単体試験では見えない)。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fc22_hint_reaches_only_the_holding_register_bit_through_the_real_write_paths() {
    tokio::time::timeout(Duration::from_secs(30), fc22_hint_run())
        .await
        .unwrap();
}

async fn fc22_hint_run() {
    let env = common::TempEnv::new("hub-frame-write", "fc22-hint");
    let pool = init_db(env.registry_path()).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    // 握手(`H`)にだけ答え、あとの要求には一切答えない(= 応答なし)。
    let server = tokio::spawn(async move {
        let mut peer = EchoResponder::new();
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            peer.on_connect();
            loop {
                let mut header = [0; 6];
                if stream.read_exact(&mut header).await.is_err() {
                    break;
                }
                let mut request = header.to_vec();
                let len = u16::from_be_bytes([header[4], header[5]]) as usize;
                request.resize(6 + len, 0);
                stream.read_exact(&mut request[6..]).await.unwrap();
                if header[1] != b'H' {
                    continue;
                }
                let Reply::Bytes(reply) = peer.reply(&request) else {
                    panic!("healthy echo handshake");
                };
                if stream.write_all(&reply).await.is_err() {
                    break;
                }
            }
        }
    });
    let mut drivers = HubDrivers::none();
    Arc::get_mut(&mut drivers.registry)
        .unwrap()
        .register(DriverEntry::frame(Arc::new(HoldingRegisterAlias(
            EchoFrameDriver::new(),
        ))))
        .unwrap();
    let registry = drivers.registry.clone();
    let sessions = Arc::new(HubSessions::new(
        banto_broker::BackoffConfig::default(),
        drivers,
    ));
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
    let tags = TagService::with_registry(pool.clone(), registry);
    for (name, address, data_type) in [("bit", "W10.0", "bit"), ("word", "W11", "u16")] {
        tags.create(TagInput {
            comment: None,
            name: name.into(),
            collection_group_id: group.id,
            address: address.into(),
            data_type: data_type.into(),
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
    }
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
    let connected = |handle: banto_broker::BrokerHandle| async move {
        tokio::time::timeout(
            Duration::from_secs(5),
            handle
                .status_watch()
                .wait_for(|s| *s == banto_broker::BrokerConnectionStatus::Connected),
        )
        .await
        .unwrap()
        .unwrap();
    };
    connected(handle.clone()).await;
    let keys = ApiKeysService::new(pool.clone());
    let audit = WriteAuditService::new(pool.clone());
    let control = WriteControl::new(false);
    control.enable();
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
        prefix: "kkkkkkkk".to_string(),
        scopes: vec!["write:line.slow.bit".into(), "write:line.slow.word".into()],
        last_used_at_ms: None,
    };
    let audit_rows = || {
        let pool = pool.clone();
        async move {
            sqlx::query_as::<_, (String, String, Option<String>)>(
                "SELECT external_name_snapshot, result, detail FROM hub_write_audit \
                 WHERE action='write' ORDER BY id",
            )
            .fetch_all(&pool)
            .await
            .unwrap()
        }
    };
    let hinted = |text: &str| text.contains("FC22");

    // 単票: 保持レジスタのビット。
    let single = execute_write(
        &deps,
        &ctx,
        "line.slow.bit",
        Some(RequestedValue::Bool(true)),
    )
    .await;
    let Err(WriteRejection::WriteFailed(message)) = single else {
        panic!("{single:?}");
    };
    assert!(hinted(&message), "{message}");
    let rows = audit_rows().await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].1, "uncertain");
    assert!(hinted(rows[0].2.as_deref().unwrap()), "{rows:?}");

    // 切断 → 再接続を待ってから、バッチ(ワードが先、ビットが後)。
    connected(handle.clone()).await;
    let batch = banto_hub_core::write_path::execute_write_batch(
        &deps,
        &ctx,
        vec![
            ("line.slow.word".into(), Some(RequestedValue::Num(7.0))),
            ("line.slow.bit".into(), Some(RequestedValue::Bool(true))),
        ],
    )
    .await;
    assert_eq!(batch.len(), 2);
    let message_of = |tag: &str| match &batch.iter().find(|o| o.tag == tag).unwrap().result {
        Err(WriteRejection::WriteFailed(message)) => message.clone(),
        other => panic!("{tag}: {other:?}"),
    };
    assert!(hinted(&message_of("line.slow.bit")));
    assert!(!hinted(&message_of("line.slow.word")));
    let rows = audit_rows().await;
    assert_eq!(rows.len(), 3, "{rows:?}");
    for (name, result, detail) in &rows[1..] {
        assert_eq!(result, "uncertain", "{name}");
        assert_eq!(
            hinted(detail.as_deref().unwrap()),
            name == "line.slow.bit",
            "{name}: {detail:?}"
        );
    }
}
