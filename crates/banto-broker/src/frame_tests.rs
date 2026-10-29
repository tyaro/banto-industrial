//! C16: the real FrameSession under the broker's job/lifecycle bounds.
use super::*;
use banto_driver_api::{
    AddressError, Capabilities, DataType, DriverAddress, DriverBase, DriverDescriptor, DriverEntry,
    DriverId, FrameCheck, FrameConnection, FrameDriver, FrameRule, Planned, RawValue, Step,
    TagKind, WriteProgress,
};
use banto_driver_engine::{transport::Transport, FrameSession};
use std::sync::atomic::{AtomicUsize, Ordering};

const T: Duration = Duration::from_millis(100);

#[derive(Default)]
struct Stats {
    sends: std::sync::Mutex<Vec<u8>>,
    drops: AtomicUsize,
    transport_drops: AtomicUsize,
    connects: AtomicUsize,
}

struct Driver {
    descriptor: DriverDescriptor,
    stats: Arc<Stats>,
    steps: usize,
    handshake: usize,
    refuse: bool,
    config_errors: Vec<banto_driver_api::FieldError>,
}
impl Driver {
    fn new(stats: Arc<Stats>, steps: usize) -> Self {
        Self {
            descriptor: DriverDescriptor::new(
                DriverId::new("frame-test").unwrap(),
                "1",
                "test",
                Capabilities::READ_WRITE,
            ),
            stats,
            steps,
            handshake: 0,
            refuse: false,
            config_errors: vec![],
        }
    }
}
impl DriverBase for Driver {
    fn descriptor(&self) -> &DriverDescriptor {
        &self.descriptor
    }
    fn validate_config(&self, _: &Settings) -> Vec<banto_driver_api::FieldError> {
        self.config_errors.clone()
    }
    fn offset_address(&self, text: &str, _: &TagKind, _: i64) -> Result<String, AddressError> {
        Ok(text.into())
    }
    fn address_help(&self) -> String {
        "D0".into()
    }
    fn secret_keys(&self) -> Vec<String> {
        vec!["password".into()]
    }
    fn parse_address(&self, text: &str, kind: &TagKind) -> Result<DriverAddress, AddressError> {
        Ok(DriverAddress::new(
            self.descriptor.id.clone(),
            text,
            "D",
            true,
            kind.word_count(),
        ))
    }
}
impl FrameDriver for Driver {
    fn create_connection(&self, _: &Settings) -> Result<Box<dyn FrameConnection>, FatalError> {
        if self.refuse {
            return Err(FatalError::InvalidConfig("refused".into()));
        }
        self.stats.connects.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Connection {
            stats: self.stats.clone(),
            left: 0,
            steps: self.steps,
            handshake: self.handshake,
        }))
    }
}
struct Connection {
    stats: Arc<Stats>,
    left: usize,
    steps: usize,
    handshake: usize,
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.stats.drops.fetch_add(1, Ordering::SeqCst);
    }
}
fn step(code: u8) -> Step {
    Step::new(vec![code], FrameRule::Fixed(1), 1)
}
impl FrameConnection for Connection {
    fn handshake_next(&mut self, _: Option<&[u8]>) -> Result<Option<Step>, FatalError> {
        if self.handshake == 0 {
            Ok(None)
        } else {
            self.handshake -= 1;
            Ok(Some(step(b'H')))
        }
    }
    fn is_complete(&self, _: &Step, _: &[u8]) -> Result<FrameCheck, FatalError> {
        unreachable!()
    }
    fn plan_read(&mut self, specs: &[TagSpec]) -> Result<Vec<Planned>, FatalError> {
        Ok(specs
            .iter()
            .enumerate()
            .map(|(i, _)| Planned::new(step(b'R'), vec![i as u32]))
            .collect())
    }
    fn decode_read(
        &self,
        _: &Planned,
        _: &[TagSpec],
        _: &[u8],
    ) -> Result<Vec<Result<RawValue, ItemError>>, FatalError> {
        Ok(vec![Ok(RawValue::Uint(7))])
    }
    fn write_next(
        &mut self,
        items: &[WriteItem],
        prev: Option<&[u8]>,
    ) -> Result<WriteProgress, FatalError> {
        if prev.is_none() {
            self.left = self.steps;
        }
        if self.left == 0 {
            return Ok(WriteProgress::Done(vec![Ok(()); items.len()]));
        }
        self.left -= 1;
        Ok(WriteProgress::Send(Planned::new(
            step(b'W'),
            (0..items.len() as u32).collect(),
        )))
    }
}
struct Wire {
    stats: Arc<Stats>,
    delay: Duration,
    fatal: bool,
}
impl Drop for Wire {
    fn drop(&mut self) {
        self.stats.transport_drops.fetch_add(1, Ordering::SeqCst);
    }
}
impl Transport for Wire {
    fn send<'a>(
        &'a mut self,
        bytes: &'a [u8],
    ) -> banto_driver_api::BoxFuture<'a, Result<(), FatalError>> {
        self.stats.sends.lock().unwrap().extend(bytes);
        Box::pin(async { Ok(()) })
    }
    fn recv<'a>(
        &'a mut self,
        buf: &'a mut Vec<u8>,
    ) -> banto_driver_api::BoxFuture<'a, Result<usize, FatalError>> {
        Box::pin(async move {
            tokio::time::sleep(self.delay).await;
            if self.fatal {
                Err(FatalError::Framing("broken".into()))
            } else {
                buf.push(0);
                Ok(1)
            }
        })
    }
    fn discard_input(&mut self) -> banto_driver_api::BoxFuture<'_, ()> {
        Box::pin(async {})
    }
    fn close(&mut self) -> banto_driver_api::BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}
fn spec() -> TagSpec {
    TagSpec::new(
        DriverAddress::new(DriverId::new("frame-test").unwrap(), "D0", "D", true, 1),
        TagKind::numeric(DataType::U16),
    )
}
fn items() -> Vec<WriteItem> {
    vec![WriteItem::new(spec(), Value::F64(1.))]
}
fn connector(stats: Arc<Stats>, delay: Duration, fatal: bool, steps: usize) -> Connector {
    Arc::new(move || {
        let stats = stats.clone();
        Box::pin(async move {
            let driver = Driver::new(stats.clone(), steps);
            FrameSession::with_transport(
                &driver,
                &Settings::new(),
                Box::new(Wire {
                    stats,
                    delay,
                    fatal,
                }),
                T,
                None,
            )
            .await
            .map(|s| Box::new(s) as Box<dyn Session>)
        })
    })
}
async fn connected(handle: &BrokerHandle) {
    tokio::time::timeout(
        Duration::from_secs(5),
        handle
            .status_watch()
            .wait_for(|s| *s == BrokerConnectionStatus::Connected),
    )
    .await
    .unwrap()
    .unwrap();
}
fn spawn(connector: Connector) -> (BrokerHandle, TaskEntry, watch::Sender<bool>) {
    let (shutdown, rx) = watch::channel(false);
    let (handle, task) = spawn_task_with_connector(
        1,
        connector,
        BackoffConfig {
            base: Duration::from_millis(10),
            cap: Duration::from_millis(10),
            factor: 1,
        },
        SessionTimeouts {
            connect: T,
            response: T,
        },
        rx,
    );
    (handle, task, shutdown)
}

#[tokio::test(start_paused = true)]
async fn c16_all_write_steps_are_exclusive_and_fit_inside_the_outer_budget() {
    let stats = Arc::<Stats>::default();
    // Maximum permitted step count for one logical write, each at 0.95 T.
    let (handle, task, _shutdown) = spawn(connector(stats.clone(), T.mul_f64(0.95), false, 11));
    connected(&handle).await;
    let writer = {
        let handle = handle.clone();
        tokio::spawn(async move { handle.write(items()).await })
    };
    while stats.sends.lock().unwrap().is_empty() {
        tokio::task::yield_now().await;
    }
    let reader = {
        let handle = handle.clone();
        tokio::spawn(async move { handle.read(vec![spec()]).await })
    };
    assert_eq!(writer.await.unwrap(), Ok(vec![Ok(())]));
    assert_eq!(reader.await.unwrap(), Ok(vec![Ok(Value::F64(7.))]));
    let mut expected = vec![b'W'; 11];
    expected.push(b'R');
    assert_eq!(*stats.sends.lock().unwrap(), expected);
    let _ = task.stop_tx.send(true);
    task.join_handle.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn c16_fatal_and_inner_timeout_discard_state_without_replaying_unknown_writes() {
    for fatal in [false, true] {
        let stats = Arc::<Stats>::default();
        let delay = if fatal {
            Duration::ZERO
        } else {
            T.mul_f64(1.05)
        };
        let (handle, task, _shutdown) = spawn(connector(stats.clone(), delay, fatal, 3));
        connected(&handle).await;
        let started = Instant::now();
        let result = handle.write(items()).await;
        assert!(
            matches!(result, Err(BrokerError::ConnectionFailed { .. })),
            "{result:?}"
        );
        if !fatal {
            assert_eq!(
                result,
                Err(BrokerError::ConnectionFailed {
                    connection_id: 1,
                    reason: FatalError::Timeout.to_string()
                })
            );
            assert_eq!(started.elapsed(), T);
            assert!(started.elapsed() < write_job_limit(1, T));
        }
        assert_eq!(stats.drops.load(Ordering::SeqCst), 1);
        assert_eq!(stats.transport_drops.load(Ordering::SeqCst), 1);
        connected(&handle).await;
        assert_eq!(stats.connects.load(Ordering::SeqCst), 2);
        assert_eq!(
            *stats.sends.lock().unwrap(),
            vec![b'W'],
            "no remainder or automatic resend after reconnect"
        );
        let _ = task.stop_tx.send(true);
        task.join_handle.await.unwrap();
    }
}

#[tokio::test]
async fn c16_registered_frame_dial_allows_all_32_handshake_steps() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        for _ in 0..32 {
            let mut b = [0];
            stream.read_exact(&mut b).await.unwrap();
            assert_eq!(b, [b'H']);
            tokio::time::sleep(Duration::from_millis(45)).await;
            stream.write_all(&[0]).await.unwrap();
        }
        let mut b = [0];
        assert_eq!(stream.read(&mut b).await.unwrap(), 0);
    });
    let mut driver = Driver::new(Arc::default(), 0);
    driver.handshake = 32;
    let mut registry = DriverRegistry::new();
    registry
        .register(DriverEntry::frame(Arc::new(driver)))
        .unwrap();
    let conn = PlcConnection::from_settings(
        1,
        "frame".into(),
        "frame-test".into(),
        true,
        false,
        serde_json::json!({"host":"127.0.0.1","port":port})
            .as_object()
            .unwrap()
            .clone(),
        1,
    );
    let supervisor = BrokerSupervisor::spawn_with(
        &[conn],
        Arc::new(registry),
        BrokerOptions {
            timeouts: SessionTimeouts {
                connect: Duration::from_millis(50),
                response: Duration::from_millis(500),
            },
            ..BrokerOptions::default()
        },
    )
    .unwrap();
    let handle = supervisor.handle(1).unwrap();
    tokio::time::timeout(Duration::from_secs(10), connected(&handle))
        .await
        .expect("32 slow handshakes fit Frame-specific outer bound");
    supervisor.shutdown().await;
    server.await.unwrap();
}

#[tokio::test]
async fn frame_dial_settings_copy_is_zeroized_on_success_failure_cancel_and_unpolled_drop() {
    use tokio::io::AsyncReadExt;
    for (name, refuse, handshake, cancel, poll) in [
        ("success", false, 0, false, true),
        ("refused", true, 0, false, true),
        ("timeout", false, 1, false, true),
        ("cancel", false, 1, true, true),
        ("unpolled", false, 0, true, false),
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0; 32];
            while stream.read(&mut buf).await.unwrap_or(0) != 0 {} // deliberately silent
        });
        let secret = format!("frame-copy-secret-{name}");
        let mut settings = Settings::from_pairs([
            ("host", "127.0.0.1"),
            ("port", &port.to_string()),
            ("password", secret.as_str()),
        ])
        .unwrap();
        let mut driver = Driver::new(Arc::default(), 0);
        driver.refuse = refuse;
        driver.handshake = handshake;
        let dial = session::FrameDial(Arc::new(driver));
        let future = dial.connect(&settings, ConnectContext::new("test", T, T));
        settings.insert("password", "redacted");
        if cancel {
            if poll {
                assert!(tokio::time::timeout(Duration::from_millis(1), future)
                    .await
                    .is_err());
            } else {
                drop(future);
            }
        } else {
            let result = future.await;
            if name == "timeout" {
                assert!(matches!(result, Err(FatalError::Timeout)));
            } else {
                assert_eq!(result.is_err(), refuse);
            }
        }
        assert!(
            zeroized_for_test::contains(&secret),
            "{name}: adapter-owned copy must zeroize"
        );
        server.abort();
        let _ = server.await;
    }
}

#[tokio::test]
async fn frame_registration_is_managed_and_transport_is_validated_before_dial() {
    let mut registry = DriverRegistry::new();
    registry
        .register(DriverEntry::frame(Arc::new(Driver::new(Arc::default(), 0))))
        .unwrap();
    assert!(is_broker_managed(&registry, "frame-test"));
    let supervisor =
        BrokerSupervisor::spawn(&[], BackoffConfig::default(), Arc::new(registry)).unwrap();
    let mut conn = PlcConnection::from_settings(
        1,
        "frame".into(),
        "frame-test".into(),
        true,
        false,
        serde_json::json!({"host":"127.0.0.1","port":1,"transport":"serial"})
            .as_object()
            .unwrap()
            .clone(),
        1,
    );
    assert!(matches!(
        supervisor.directory().ensure_connection(&conn),
        Err(BrokerError::InvalidSettings { .. })
    ));
    conn.settings = serde_json::json!({"host":"127.0.0.1","port":0})
        .as_object()
        .unwrap()
        .clone();
    assert!(matches!(
        supervisor.directory().ensure_connection(&conn),
        Err(BrokerError::InvalidSettings { .. })
    ));
    supervisor.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn c16_step_limit_discards_partial_write_and_only_explicit_requests_start_again() {
    let stats = Arc::<Stats>::default();
    let (handle, task, _shutdown) = spawn(connector(stats.clone(), Duration::ZERO, false, 12));
    connected(&handle).await;
    let first = handle.write(items()).await;
    assert!(
        matches!(&first, Err(BrokerError::ConnectionFailed { reason, .. }) if reason.contains("上限")),
        "{first:?}"
    );
    assert_eq!(
        stats.sends.lock().unwrap().len(),
        11,
        "step 12 is refused before sending"
    );
    connected(&handle).await;
    assert_eq!(stats.connects.load(Ordering::SeqCst), 2);
    assert_eq!(stats.sends.lock().unwrap().len(), 11, "no automatic retry");
    let next = handle.write(items()).await;
    assert!(matches!(next, Err(BrokerError::ConnectionFailed { .. })));
    assert_eq!(
        stats.sends.lock().unwrap().len(),
        22,
        "an explicit new logical write starts from its first step"
    );
    assert_eq!(stats.drops.load(Ordering::SeqCst), 2);
    let _ = task.stop_tx.send(true);
    task.join_handle.await.unwrap();
}

#[tokio::test]
async fn frame_dial_owned_secret_is_guarded_even_before_polling_or_transport_validation() {
    for poll in [false, true] {
        let secret = format!("frame-owned-guard-without-network-{poll}");
        let settings = Settings::from_pairs([("password", secret.as_str())]).unwrap();
        let dial = session::FrameDial(Arc::new(Driver::new(Arc::default(), 0)));
        let future = dial.connect(&settings, ConnectContext::new("test", T, T));
        if poll {
            assert!(matches!(future.await, Err(FatalError::InvalidConfig(_))));
        } else {
            drop(future);
        }
        assert!(zeroized_for_test::contains(&secret));
    }
}

/// TCP-only drivers must never turn converter traffic into ordinary TCP.
/// Descriptor checks are host policy even when the driver's validator accepts all.
#[tokio::test]
async fn frame_transport_policy_rejects_before_directory_or_direct_dial_io() {
    use banto_driver_api::Transport;
    for (transports, transport, message) in [
        (vec![Transport::Tcp], Some("serial_over_tcp"), "未対応"),
        (
            vec![Transport::SerialOverTcp],
            Some("serial_over_tcp"),
            "未対応",
        ),
        (vec![Transport::Serial], Some("tcp"), "対応していません"),
        (vec![Transport::Serial], None, "対応していません"),
        // #916: UDP only when the descriptor lists it, and never TCP instead.
        (
            vec![Transport::Tcp],
            Some("udp"),
            "UDP 通信に対応していません",
        ),
        (vec![Transport::Udp], None, "TCP 通信に対応していません"),
        (
            vec![Transport::Udp],
            Some("tcp"),
            "TCP 通信に対応していません",
        ),
        (
            vec![Transport::Tcp, Transport::Udp],
            Some("serial"),
            "未対応",
        ),
    ] {
        let stats = Arc::<Stats>::default();
        let mut driver = Driver::new(stats.clone(), 0);
        driver.descriptor.transports = transports;
        let mut registry = DriverRegistry::new();
        registry
            .register(DriverEntry::frame(Arc::new(driver)))
            .unwrap();
        let resolved = resolve_session_driver(&registry, "frame-test").unwrap();
        let mut pairs = vec![("host", "127.0.0.1"), ("port", "1")];
        if let Some(transport) = transport {
            pairs.push(("transport", transport));
        }
        let settings = Settings::from_pairs(pairs).unwrap();
        let errors = resolved.driver().validate_config(&settings);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].key, "transport");
        assert!(errors[0].message.contains(message), "{errors:?}");
        // Bypass validate_config: the exposed adapter still refuses before I/O.
        let result = resolved
            .driver()
            .connect(&settings, ConnectContext::new("test", T, T))
            .await;
        assert!(
            matches!(result, Err(FatalError::InvalidConfig(ref reason)) if reason.contains(message))
        );
        let supervisor =
            BrokerSupervisor::spawn(&[], BackoffConfig::default(), Arc::new(registry)).unwrap();
        let conn = PlcConnection::from_settings(
            1,
            "frame".into(),
            "frame-test".into(),
            true,
            false,
            serde_json::Map::new(),
            1,
        );
        assert!(matches!(
            supervisor.directory().ensure_connection_with_settings(&conn, settings),
            Err(BrokerError::InvalidSettings { reason, .. }) if reason.contains(message)
        ));
        assert_eq!(stats.connects.load(Ordering::SeqCst), 0);
        assert_eq!(supervisor.directory().connection_count(), 0);
        supervisor.shutdown().await;
    }
}

/// #916: a driver listing UDP is dialled over UDP through the engine (one
/// datagram each way), and a handshake with no round trip is refused there.
#[tokio::test]
async fn frame_dial_connects_over_udp_when_the_descriptor_lists_it() {
    use banto_driver_api::Transport;
    const LONG: Duration = Duration::from_secs(10);
    let peer = tokio::net::UdpSocket::bind(("127.0.0.1", 0)).await.unwrap();
    let port = peer.local_addr().unwrap().port().to_string();
    let settings = Settings::from_pairs([
        ("transport", "udp"),
        ("host", "127.0.0.1"),
        ("port", port.as_str()),
    ])
    .unwrap();
    let answer = |expect: u8| {
        let peer = &peer;
        async move {
            let mut buf = [0u8; 16];
            let (n, from) = tokio::time::timeout(LONG, peer.recv_from(&mut buf))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(&buf[..n], [expect]);
            peer.send_to(&[expect], from).await.unwrap();
        }
    };
    for handshake in [1, 0] {
        let mut driver = Driver::new(Arc::default(), 0);
        driver.descriptor.transports = vec![Transport::Udp];
        driver.handshake = handshake;
        let mut registry = DriverRegistry::new();
        registry
            .register(DriverEntry::frame(Arc::new(driver)))
            .unwrap();
        let resolved = resolve_session_driver(&registry, "frame-test").unwrap();
        assert_eq!(resolved.driver().validate_config(&settings), vec![]);
        let connect = resolved
            .driver()
            .connect(&settings, ConnectContext::new("test", LONG, LONG));
        if handshake == 0 {
            assert!(matches!(
                connect.await,
                Err(FatalError::Protocol(ref reason)) if reason.contains("1 往復")
            ));
            continue;
        }
        let (session, ()) = tokio::join!(connect, answer(b'H'));
        let mut session = session.unwrap();
        let specs = [TagSpec::new(
            resolved
                .driver()
                .parse_address("D0", &TagKind::numeric(DataType::U16))
                .unwrap(),
            TagKind::numeric(DataType::U16),
        )];
        let (values, ()) = tokio::join!(session.read_batch(&specs), answer(b'R'));
        assert_eq!(values.unwrap(), vec![Ok(Value::F64(7.0))]);
        session.disconnect().await;
    }
}

#[test]
fn frame_validation_preserves_driver_field_errors_without_host_duplicates() {
    use banto_driver_api::FieldError;
    for (field, pairs) in [
        ("host", vec![("port", "1")]),
        ("port", vec![("host", "127.0.0.1"), ("port", "0")]),
    ] {
        let mut driver = Driver::new(Arc::default(), 0);
        driver.config_errors = vec![FieldError::new(field, "driver-specific explanation")];
        let dial = session::FrameDial(Arc::new(driver));
        let errors = dial.validate_config(&Settings::from_pairs(pairs).unwrap());
        assert_eq!(
            errors,
            vec![FieldError::new(field, "driver-specific explanation")]
        );
    }
}

#[test]
fn resolved_dial_limits_include_frame_handshake_and_keep_session_budget() {
    let mut registry = DriverRegistry::new();
    registry
        .register(DriverEntry::frame(Arc::new(Driver::new(Arc::default(), 0))))
        .unwrap();
    registry
        .register(DriverEntry::session(Arc::new(fake::FakeDriver::new(
            "session-test",
            fake::FakePlc::new(),
        ))))
        .unwrap();
    let frame = resolve_session_driver(&registry, "frame-test").unwrap();
    let session = resolve_session_driver(&registry, "session-test").unwrap();
    let timeouts = SessionTimeouts {
        connect: Duration::from_millis(250),
        response: Duration::from_millis(50),
    };
    assert_eq!(frame.connect_limit(timeouts), Duration::from_millis(2950));
    assert_eq!(session.connect_limit(timeouts), Duration::from_millis(1250));
    // Response time never changes legacy Session's dial budget.
    assert_eq!(
        session.connect_limit(SessionTimeouts {
            response: Duration::MAX,
            ..timeouts
        }),
        Duration::from_millis(1250)
    );
    for timeouts in [
        SessionTimeouts {
            connect: Duration::MAX,
            response: Duration::ZERO,
        },
        SessionTimeouts {
            connect: Duration::ZERO,
            response: Duration::MAX,
        },
    ] {
        assert_eq!(frame.connect_limit(timeouts), Duration::MAX);
    }
    assert_eq!(
        session.connect_limit(SessionTimeouts {
            connect: Duration::MAX,
            response: Duration::ZERO
        }),
        Duration::MAX
    );
    assert_eq!(
        frame.connect_limit(SessionTimeouts {
            connect: Duration::ZERO,
            response: Duration::ZERO
        }),
        Duration::from_millis(1100)
    );
    assert!(resolve_session_driver(&registry, "unknown").is_none());
}

#[test]
fn frame_tcp_descriptor_accepts_explicit_and_default_tcp_settings() {
    let dial = session::FrameDial(Arc::new(Driver::new(Arc::default(), 0)));
    for pairs in [
        vec![("host", "127.0.0.1"), ("port", "1")],
        vec![("host", "127.0.0.1"), ("port", "1"), ("transport", "tcp")],
    ] {
        assert!(dial
            .validate_config(&Settings::from_pairs(pairs).unwrap())
            .is_empty());
    }
}
