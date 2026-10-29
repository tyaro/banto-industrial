//! UDP（#916、io-design §7.6）: ループバックの UDP の相手で、データグラムの契約を実通信で確かめる。
//!
//! 時間切れを待つ試験は、接続（ハンドシェイク）を長い期限で済ませてから、その接続の
//! 応答の期限だけを短くする（遅い CI でハンドシェイクが時間切れにならないように）。
//! 前提（キューに重複が溜まったこと）は固定の sleep ではなく、OS への peek で観測してから進める。

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use banto_driver_api::{
    AddressError, Capabilities, DataType, DriverAddress, DriverBase, DriverDescriptor, DriverId,
    FieldError, Planned, TagKind, Transport as TransportKind,
};

use super::*;
use crate::transport::UdpTransport;

/// 待ちの上限（ハング検出用。成功する経路はこれよりずっと早く終わる）。
const LONG: Duration = Duration::from_secs(10);
/// 時間切れを起こしたい試験の応答の期限。
const SHORT: Duration = Duration::from_millis(300);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Handshake {
    /// `HS` を送り `OK`（2 バイト）を待つ。
    RoundTrip,
    /// ハンドシェイクなし（0 step）。
    Empty,
    /// 応答を待たない 1 step だけ（往復は 0）。
    SendOnly,
}

struct UdpDriver {
    descriptor: DriverDescriptor,
    handshake: Handshake,
    read_max_len: u32,
}

impl UdpDriver {
    fn new(handshake: Handshake) -> Self {
        Self {
            descriptor: DriverDescriptor::new(
                DriverId::new("udp-test").unwrap(),
                "0.1.0",
                "UDP 試験",
                Capabilities::READ_WRITE,
            )
            .with_transports(vec![TransportKind::Udp]),
            handshake,
            read_max_len: 2,
        }
    }
}

impl DriverBase for UdpDriver {
    fn descriptor(&self) -> &DriverDescriptor {
        &self.descriptor
    }
    fn validate_config(&self, _s: &Settings) -> Vec<FieldError> {
        Vec::new()
    }
    fn parse_address(&self, text: &str, kind: &TagKind) -> Result<DriverAddress, AddressError> {
        Ok(DriverAddress::new(
            self.descriptor.id.clone(),
            text,
            "a",
            true,
            kind.word_count(),
        ))
    }
    fn offset_address(&self, text: &str, _kind: &TagKind, _n: i64) -> Result<String, AddressError> {
        Ok(text.into())
    }
    fn address_help(&self) -> String {
        String::new()
    }
}

impl FrameDriver for UdpDriver {
    fn create_connection(&self, _s: &Settings) -> Result<Box<dyn FrameConnection>, FatalError> {
        Ok(Box::new(UdpConn {
            handshake: self.handshake,
            handshake_sent: false,
            read_max_len: self.read_max_len,
            seq: 0,
        }))
    }
}

/// 要求は `[種類, 通し番号]`、応答は `[種類, 値]`（読み取りの値は応答の 2 バイト目）。
struct UdpConn {
    handshake: Handshake,
    handshake_sent: bool,
    read_max_len: u32,
    seq: u8,
}

impl FrameConnection for UdpConn {
    fn handshake_next(&mut self, _previous: Option<&[u8]>) -> Result<Option<Step>, FatalError> {
        if self.handshake_sent {
            return Ok(None);
        }
        self.handshake_sent = true;
        Ok(match self.handshake {
            Handshake::RoundTrip => Some(Step::new(b"HS".to_vec(), FrameRule::Fixed(2), 2)),
            Handshake::Empty => None,
            Handshake::SendOnly => Some(Step::new(b"HS".to_vec(), FrameRule::None, 2)),
        })
    }
    fn is_complete(&self, _step: &Step, _buf: &[u8]) -> Result<FrameCheck, FatalError> {
        Ok(FrameCheck::Incomplete)
    }
    fn plan_read(&mut self, specs: &[TagSpec]) -> Result<Vec<Planned>, FatalError> {
        self.seq += 1;
        Ok(vec![Planned::new(
            Step::new(vec![b'R', self.seq], FrameRule::Fixed(2), self.read_max_len),
            (0..specs.len() as u32).collect(),
        )])
    }
    fn decode_read(
        &self,
        planned: &Planned,
        _specs: &[TagSpec],
        response: &[u8],
    ) -> Result<Vec<Result<RawValue, ItemError>>, FatalError> {
        assert_eq!(response[0], b'R');
        Ok(vec![
            Ok(RawValue::Uint(u64::from(response[1])));
            planned.covers.len()
        ])
    }
    fn write_next(
        &mut self,
        items: &[WriteItem],
        previous: Option<&[u8]>,
    ) -> Result<WriteProgress, FatalError> {
        if previous.is_some() {
            return Ok(WriteProgress::Done(vec![Ok(()); items.len()]));
        }
        self.seq += 1;
        Ok(WriteProgress::Send(Planned::new(
            Step::new(vec![b'W', self.seq], FrameRule::Fixed(2), 2),
            (0..items.len() as u32).collect(),
        )))
    }
}

/// 送信の回数を数え、`hold_discard` なら受信の破棄の前にキューに溜まるのを待つ UDP。
struct Observed {
    inner: UdpTransport,
    sends: Arc<AtomicUsize>,
    hold_discard: Arc<AtomicBool>,
}

impl Transport for Observed {
    fn datagram(&self) -> bool {
        self.inner.datagram()
    }
    fn send<'a>(&'a mut self, bytes: &'a [u8]) -> BoxFuture<'a, Result<(), FatalError>> {
        self.sends.fetch_add(1, Ordering::SeqCst);
        self.inner.send(bytes)
    }
    fn recv<'a>(&'a mut self, buf: &'a mut Vec<u8>) -> BoxFuture<'a, Result<usize, FatalError>> {
        self.inner.recv(buf)
    }
    fn discard_input(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            if self.hold_discard.swap(false, Ordering::SeqCst) {
                timeout(LONG, self.inner.wait_queued())
                    .await
                    .expect("重複のデータグラムがキューに溜まらない");
            }
            self.inner.discard_input().await;
        })
    }
    fn close(&mut self) -> BoxFuture<'_, ()> {
        self.inner.close()
    }
}

/// 偽の機器。未接続のソケットで受け、送信元に返す。
struct Peer {
    socket: tokio::net::UdpSocket,
}

impl Peer {
    async fn bind() -> Self {
        Self {
            socket: tokio::net::UdpSocket::bind(("127.0.0.1", 0)).await.unwrap(),
        }
    }
    fn addr(&self) -> SocketAddr {
        self.socket.local_addr().unwrap()
    }
    /// 次の要求。Windows の未接続の UDP は、閉じたポートへ送った後の ICMP の報告を次の
    /// 受信の ConnectionReset で返すので読み飛ばす。
    async fn next(&self) -> (Vec<u8>, SocketAddr) {
        timeout(LONG, async {
            let mut buf = [0u8; 2048];
            loop {
                match self.socket.recv_from(&mut buf).await {
                    Ok((n, from)) => return (buf[..n].to_vec(), from),
                    Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => continue,
                    Err(e) => panic!("相手の受信: {e}"),
                }
            }
        })
        .await
        .expect("要求が届かない")
    }
    async fn reply(&self, to: SocketAddr, bytes: &[u8]) {
        self.socket.send_to(bytes, to).await.unwrap();
    }
    /// ハンドシェイクに答え、エンジンのソケットのアドレスを返す。
    async fn answer_handshake(&self) -> SocketAddr {
        let (request, from) = self.next().await;
        assert_eq!(request, b"HS");
        self.reply(from, b"OK").await;
        from
    }
}

fn spec() -> TagSpec {
    TagSpec::new(
        DriverAddress::new(DriverId::new("udp-test").unwrap(), "x", "a", true, 1),
        TagKind::numeric(DataType::U16),
    )
}

/// ハンドシェイクを長い期限で済ませた接続と、送信の回数・破棄の待ちの口・エンジン側のアドレス。
async fn open(
    peer: &Peer,
    driver: &UdpDriver,
) -> (FrameSession, Arc<AtomicUsize>, Arc<AtomicBool>, SocketAddr) {
    let sends = Arc::new(AtomicUsize::new(0));
    let hold_discard = Arc::new(AtomicBool::new(false));
    let transport = Observed {
        inner: UdpTransport::connect(peer.addr()).unwrap(),
        sends: sends.clone(),
        hold_discard: hold_discard.clone(),
    };
    let settings = Settings::new();
    let (session, from) = tokio::join!(
        FrameSession::with_transport(driver, &settings, Box::new(transport), LONG, None),
        peer.answer_handshake()
    );
    (session.unwrap(), sends, hold_discard, from)
}

fn shorten(session: &mut FrameSession) {
    session.core.as_mut().unwrap().response_timeout = SHORT;
}

async fn read_one(session: &mut FrameSession) -> Result<RawValue, FatalError> {
    Ok(session
        .read_raw_batch(&[spec()])
        .await?
        .pop()
        .unwrap()
        .unwrap())
}

#[tokio::test]
async fn udp_round_trip_reads_and_writes_with_one_datagram_each_way() {
    let peer = Peer::bind().await;
    let driver = UdpDriver::new(Handshake::RoundTrip);
    let spec_udp = TransportSpec::from_settings(
        &Settings::from_pairs([
            ("transport", "udp"),
            ("host", "127.0.0.1"),
            ("port", &peer.addr().port().to_string()),
        ])
        .unwrap(),
        &driver.descriptor.transports,
    )
    .unwrap();
    let cx = ConnectContext::new("udp-test", LONG, LONG);
    let settings = Settings::new();
    let (session, from) = tokio::join!(
        FrameSession::connect(&driver, &settings, &spec_udp, &cx, None),
        peer.answer_handshake()
    );
    let mut session = session.unwrap();
    let peer_side = async {
        let (request, at) = peer.next().await;
        assert_eq!(at, from, "接続の間は同じエフェメラルポートから送る");
        assert_eq!(request, [b'R', 1]);
        peer.reply(at, &[b'R', 42]).await;
        let (request, at) = peer.next().await;
        assert_eq!(request, [b'W', 2]);
        peer.reply(at, &[b'W', 0]).await;
    };
    let engine_side = async {
        let value = read_one(&mut session).await.unwrap();
        let written = session
            .write_batch(&[WriteItem::new(spec(), Value::F64(1.0))])
            .await
            .unwrap();
        (value, written)
    };
    let (_, (value, written)) = tokio::join!(peer_side, engine_side);
    assert_eq!(value, RawValue::Uint(42));
    assert_eq!(written, vec![Ok(())]);
    assert!(session.is_connected());
    session.disconnect().await;
}

/// フレーム規則は 1 データグラム単体に当てる。足りない（2 つに分かれた応答の 1 つ目も）・
/// 余りがある・`max_len` 超過・空はフレーム不正で、接続を閉じる。次の受信を足し込まない。
#[tokio::test]
async fn a_response_must_be_exactly_one_whole_datagram() {
    let cases: [(&[&[u8]], &str); 5] = [
        (&[b"R"], "完結しない"),
        (&[b"R", &[7]], "完結しない"),
        (&[&[b'R', 7, 9]], "余り"),
        (&[&[b'R', 7, 0, 0, 0]], "max_len"),
        (&[&[]], "完結しない"),
    ];
    for (datagrams, expected) in cases {
        let peer = Peer::bind().await;
        let mut driver = UdpDriver::new(Handshake::RoundTrip);
        driver.read_max_len = 4;
        let (mut session, _, _, _) = open(&peer, &driver).await;
        let peer_side = async {
            let (_, at) = peer.next().await;
            for datagram in datagrams {
                peer.reply(at, datagram).await;
            }
        };
        let (_, result) = tokio::join!(peer_side, read_one(&mut session));
        match result {
            Err(FatalError::Framing(message)) => {
                assert!(message.contains(expected), "{datagrams:?}: {message}")
            }
            other => panic!("{datagrams:?}: {other:?}"),
        }
        assert!(!session.is_connected(), "フレーム不正の後は接続を閉じる");
    }
}

/// 接続済みのソケットなので、相手以外のポートからの応答は OS が捨て、時間切れになる。
#[tokio::test]
async fn a_datagram_from_another_port_is_ignored_and_times_out() {
    let peer = Peer::bind().await;
    let other = Peer::bind().await;
    let driver = UdpDriver::new(Handshake::RoundTrip);
    let (mut session, _, _, _) = open(&peer, &driver).await;
    shorten(&mut session);
    let peer_side = async {
        let (request, at) = peer.next().await;
        assert_eq!(request, [b'R', 1]);
        other.reply(at, &[b'R', 42]).await;
    };
    let (_, result) = tokio::join!(peer_side, read_one(&mut session));
    assert_eq!(result, Err(FatalError::Timeout));
    assert!(!session.is_connected());
}

/// 時間切れの後に届いた応答は、次の接続（新しいエフェメラルポート）には届かない。
#[tokio::test]
async fn a_reply_after_a_timeout_never_reaches_the_next_connection() {
    let peer = Peer::bind().await;
    let driver = UdpDriver::new(Handshake::RoundTrip);
    let (mut first, _, _, first_addr) = open(&peer, &driver).await;
    shorten(&mut first);
    let (_, result) = tokio::join!(peer.next(), read_one(&mut first));
    assert_eq!(result, Err(FatalError::Timeout));
    drop(first);

    let (mut second, _, _, second_addr) = open(&peer, &driver).await;
    assert_ne!(
        second_addr, first_addr,
        "接続ごとに新しいポートを bind する"
    );
    // 遅れた応答（99）を古い接続のポートへ送る。
    peer.reply(first_addr, &[b'R', 99]).await;
    let peer_side = async {
        let (request, at) = peer.next().await;
        assert_eq!(at, second_addr);
        assert_eq!(request, [b'R', 1]);
        peer.reply(at, &[b'R', 7]).await;
    };
    let (_, value) = tokio::join!(peer_side, read_one(&mut second));
    assert_eq!(value, Ok(RawValue::Uint(7)));
}

/// 前の要求への重複の応答がキューに残っていても、次の要求の送信の前に捨てる。
#[tokio::test]
async fn a_queued_duplicate_is_discarded_before_the_next_request() {
    let peer = Peer::bind().await;
    let driver = UdpDriver::new(Handshake::RoundTrip);
    let (mut session, _, hold_discard, _) = open(&peer, &driver).await;
    let peer_side = async {
        let (request, at) = peer.next().await;
        assert_eq!(request, [b'R', 1]);
        peer.reply(at, &[b'R', 1]).await;
        peer.reply(at, &[b'R', 1]).await;
    };
    let (_, value) = tokio::join!(peer_side, read_one(&mut session));
    assert_eq!(value, Ok(RawValue::Uint(1)));

    // 次の要求の破棄は、重複がキューに溜まったのを観測してから行う。
    hold_discard.store(true, Ordering::SeqCst);
    let peer_side = async {
        let (request, at) = peer.next().await;
        assert_eq!(request, [b'R', 2]);
        peer.reply(at, &[b'R', 2]).await;
    };
    let (_, value) = tokio::join!(peer_side, read_one(&mut session));
    assert_eq!(value, Ok(RawValue::Uint(2)), "重複（1）を応答と取り違えた");
    assert!(!hold_discard.load(Ordering::SeqCst), "破棄が呼ばれていない");
}

/// 不変条件: トランスポートでもエンジンでも再送しない。応答が来ない読み取り・書き込みは
/// 要求をちょうど 1 回送って時間切れになる（書き込みは結果不明としてホストが扱う）。
#[tokio::test]
async fn each_step_sends_exactly_one_request_and_never_resends() {
    let driver = UdpDriver::new(Handshake::RoundTrip);
    for write in [false, true] {
        let peer = Peer::bind().await;
        let (mut session, sends, _, _) = open(&peer, &driver).await;
        assert_eq!(sends.load(Ordering::SeqCst), 1, "ハンドシェイクの 1 回");
        shorten(&mut session);
        let engine_side = async {
            if write {
                session
                    .write_batch(&[WriteItem::new(spec(), Value::F64(1.0))])
                    .await
                    .map(|_| ())
            } else {
                read_one(&mut session).await.map(|_| ())
            }
        };
        let ((request, _), result) = tokio::join!(peer.next(), engine_side);
        assert_eq!(request[0], if write { b'W' } else { b'R' });
        assert_eq!(result, Err(FatalError::Timeout));
        assert_eq!(sends.load(Ordering::SeqCst), 2, "write={write}: 再送した");
        assert!(!session.is_connected());
    }
}

/// 相手が居ない（閉じたポート）: ICMP の port unreachable が受信のエラーになり、致命の Io。
#[tokio::test]
async fn no_peer_is_a_fatal_io_error() {
    let closed = std::net::UdpSocket::bind(("127.0.0.1", 0)).unwrap();
    let addr = closed.local_addr().unwrap();
    drop(closed);
    let driver = UdpDriver::new(Handshake::RoundTrip);
    let result = FrameSession::with_transport(
        &driver,
        &Settings::new(),
        Box::new(UdpTransport::connect(addr).unwrap()),
        LONG,
        None,
    )
    .await;
    match result {
        Err(FatalError::Io(_)) => {}
        // ICMP を報告しない環境では時間切れ（どちらも致命で、接続は張られない）。
        #[cfg(not(target_os = "linux"))]
        Err(FatalError::Timeout) => {}
        Err(other) => panic!("{other:?}"),
        Ok(_) => panic!("相手が居ないのに接続できた"),
    }
}

/// UDP では、応答を待つ step が 1 つも無いハンドシェイクを断る（相手が居る根拠が無い）。
#[tokio::test]
async fn a_handshake_without_a_round_trip_is_refused_on_udp() {
    for handshake in [Handshake::Empty, Handshake::SendOnly] {
        let peer = Peer::bind().await;
        let driver = UdpDriver::new(handshake);
        let result = FrameSession::with_transport(
            &driver,
            &Settings::new(),
            Box::new(UdpTransport::connect(peer.addr()).unwrap()),
            LONG,
            None,
        )
        .await;
        match result {
            Err(FatalError::Protocol(message)) => assert!(message.contains("1 往復"), "{message}"),
            Err(other) => panic!("{other:?}"),
            Ok(_) => panic!("0 往復のハンドシェイクを通した"),
        }
    }
}

/// 通信路の単体: 1 回の受信は 1 データグラムを丸ごと（MTU を超える大きさも）返し、
/// `discard_input` は溜まったデータグラムを捨てる。
#[tokio::test]
async fn udp_transport_receives_whole_datagrams_and_discards_queued_ones() {
    let peer = Peer::bind().await;
    let mut transport = UdpTransport::connect(peer.addr()).unwrap();
    assert!(transport.datagram());
    timeout(LONG, transport.send(b"hello"))
        .await
        .unwrap()
        .unwrap();
    let (request, at) = peer.next().await;
    assert_eq!(request, b"hello");
    assert_eq!(at, transport.local_addr().unwrap());

    let big = vec![0x5A; 9000];
    peer.reply(at, &big).await;
    let mut buf = Vec::new();
    let n = timeout(LONG, transport.recv(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert_eq!((n, buf.len()), (9000, 9000));

    peer.reply(at, b"stale").await;
    timeout(LONG, transport.wait_queued()).await.unwrap();
    timeout(LONG, transport.discard_input()).await.unwrap();
    peer.reply(at, b"new").await;
    let mut buf = Vec::new();
    timeout(LONG, transport.recv(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(buf, b"new");
    timeout(LONG, transport.close()).await.unwrap();
}
