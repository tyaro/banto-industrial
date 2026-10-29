//! #916（docs/banto-hub-driver-io-design.md §7.6）: SLMP の 4E フレームを UDP で、本物の
//! エンジン（`FrameSession` + `UdpTransport`）に通す結合試験。相手は `banto-plc` の SLMP
//! シミュレータの `respond`（要求のバイト列 → 応答のバイト列）を UDP で返す偽の機器。
//!
//! UDP の WASM ゲスト（banto-driver-wasm の E4）はまだ無いので、ここでは組み込みの
//! `SlmpDriver` の 4E のフレームを借りる。組み込みの SLMP は TCP だけを名乗り（`check_transport`
//! は変えない）、ハンドシェイクも持たないので、試験の中だけで包む: UDP を名乗り、最初の読み取りの
//! 計画（D0 の 1 語）をハンドシェイクの 1 往復にする（UDP は 0 往復のハンドシェイクを断る）。
//! 3E は組み込みのドライバが組まないので、ゲストの E4 と一緒に足す。

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use banto_driver_api::{
    AddressError, DataType, DriverAddress, DriverBase, DriverDescriptor, FatalError, FieldError,
    FrameCheck, FrameConnection, FrameDriver, ItemError, Planned, RawValue, Session, Settings,
    Step, TagKind, TagSpec, Transport, Value, WriteItem, WriteProgress,
};
use banto_driver_engine::transport::UdpTransport;
use banto_driver_engine::FrameSession;
use banto_driver_slmp::SlmpDriver;
use banto_plc::slmp::address::SlmpDevice;
use banto_plc::slmp::simulator::Simulator;

/// ハング検出用の上限。
const LONG: Duration = Duration::from_secs(10);

fn n(data_type: DataType) -> TagKind {
    TagKind::numeric(data_type)
}

fn spec(text: &str, kind: TagKind) -> TagSpec {
    TagSpec::new(SlmpDriver.parse_address(text, &kind).unwrap(), kind)
}

/// UDP を名乗り、1 往復のハンドシェイクを足した `SlmpDriver`（試験の中だけ）。
struct UdpSlmp {
    descriptor: DriverDescriptor,
}

impl UdpSlmp {
    fn new() -> Self {
        Self {
            descriptor: SlmpDriver
                .descriptor()
                .clone()
                .with_transports(vec![Transport::Udp]),
        }
    }
}

impl DriverBase for UdpSlmp {
    fn descriptor(&self) -> &DriverDescriptor {
        &self.descriptor
    }
    fn validate_config(&self, settings: &Settings) -> Vec<FieldError> {
        SlmpDriver.validate_config(settings)
    }
    fn parse_address(&self, text: &str, kind: &TagKind) -> Result<DriverAddress, AddressError> {
        SlmpDriver.parse_address(text, kind)
    }
    fn offset_address(&self, text: &str, kind: &TagKind, n: i64) -> Result<String, AddressError> {
        SlmpDriver.offset_address(text, kind, n)
    }
    fn address_help(&self) -> String {
        SlmpDriver.address_help()
    }
}

impl FrameDriver for UdpSlmp {
    fn create_connection(
        &self,
        settings: &Settings,
    ) -> Result<Box<dyn FrameConnection>, FatalError> {
        Ok(Box::new(Handshaking {
            inner: SlmpDriver.create_connection(settings)?,
            done: false,
        }))
    }
}

struct Handshaking {
    inner: Box<dyn FrameConnection>,
    done: bool,
}

impl FrameConnection for Handshaking {
    fn handshake_next(&mut self, previous: Option<&[u8]>) -> Result<Option<Step>, FatalError> {
        if self.done {
            return Ok(None);
        }
        if previous.is_some() {
            self.done = true;
            return Ok(None);
        }
        let mut planned = self.inner.plan_read(&[spec("D0", n(DataType::U16))])?;
        Ok(Some(planned.remove(0).step))
    }
    fn is_complete(&self, step: &Step, buf: &[u8]) -> Result<FrameCheck, FatalError> {
        self.inner.is_complete(step, buf)
    }
    fn plan_read(&mut self, specs: &[TagSpec]) -> Result<Vec<Planned>, FatalError> {
        self.inner.plan_read(specs)
    }
    fn decode_read(
        &self,
        planned: &Planned,
        specs: &[TagSpec],
        response: &[u8],
    ) -> Result<Vec<Result<RawValue, ItemError>>, FatalError> {
        self.inner.decode_read(planned, specs, response)
    }
    fn write_next(
        &mut self,
        items: &[WriteItem],
        previous: Option<&[u8]>,
    ) -> Result<WriteProgress, FatalError> {
        self.inner.write_next(items, previous)
    }
}

/// 4E の要求のコマンド（オフセット 15..17、LE）。
fn command(request: &[u8]) -> u16 {
    u16::from_le_bytes([request[15], request[16]])
}
const BATCH_WRITE: u16 = 0x1401;

/// 偽の 4E UDP の機器。受けた要求を全部記録し、`drop_replies` の間は答えない
/// （シミュレータには適用する = 書き込みは機器に効いたが応答が失われた状態）。
struct Device {
    sim: Arc<Simulator>,
    addr: SocketAddr,
    requests: Arc<Mutex<Vec<Vec<u8>>>>,
    drop_replies: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<()>,
}

impl Device {
    async fn start() -> Self {
        let sim = Arc::new(Simulator::start().await);
        let socket = tokio::net::UdpSocket::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = socket.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let drop_replies = Arc::new(AtomicBool::new(false));
        let task = {
            let sim = sim.clone();
            let requests = requests.clone();
            let drop_replies = drop_replies.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 65536];
                loop {
                    let (len, from) = match socket.recv_from(&mut buf).await {
                        Ok(pair) => pair,
                        // Windows の未接続の UDP が返す、前の送信への ICMP の報告。
                        Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => continue,
                        Err(e) => panic!("機器の受信: {e}"),
                    };
                    let request = buf[..len].to_vec();
                    let response = sim.respond(&request);
                    requests.lock().unwrap().push(request);
                    if !drop_replies.load(Ordering::SeqCst) {
                        let _ = socket.send_to(&response, from).await;
                    }
                }
            })
        };
        Self {
            sim,
            addr,
            requests,
            drop_replies,
            task,
        }
    }

    fn write_requests(&self) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| command(r) == BATCH_WRITE)
            .count()
    }

    async fn connect(&self, response_timeout: Duration) -> FrameSession {
        let settings = Settings::from_pairs([
            ("host", "127.0.0.1".to_string()),
            ("port", self.addr.port().to_string()),
            ("cpu_type", "R".to_string()),
        ])
        .unwrap();
        FrameSession::with_transport(
            &UdpSlmp::new(),
            &settings,
            Box::new(UdpTransport::connect(self.addr).unwrap()),
            response_timeout,
            None,
        )
        .await
        .unwrap()
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[tokio::test]
async fn slmp_4e_reads_and_writes_over_udp() {
    let device = Device::start().await;
    device.sim.set_word(SlmpDevice::D, 100, 1234);
    device.sim.set_words(SlmpDevice::D, 102, &[0x5678, 0x0001]);
    let mut session = device.connect(LONG).await;
    // ハンドシェイクの 1 往復（D0 の読み取り）が届いている。
    assert_eq!(device.requests.lock().unwrap().len(), 1);

    let values = session
        .read_batch(&[
            spec("D100", n(DataType::U16)),
            spec("D102", n(DataType::U32)),
        ])
        .await
        .unwrap();
    assert_eq!(
        values,
        vec![
            Ok(Value::F64(1234.0)),
            Ok(Value::F64(f64::from(0x0001_5678u32)))
        ]
    );

    let written = session
        .write_batch(&[WriteItem::new(
            spec("D200", n(DataType::U16)),
            Value::F64(77.0),
        )])
        .await
        .unwrap();
    assert_eq!(written, vec![Ok(())]);
    assert_eq!(device.sim.get_word(SlmpDevice::D, 200), 77);
    assert_eq!(device.write_requests(), 1);
    session.disconnect().await;
}

/// 応答が失われた書き込み: 機器には効いたが、エンジンは時間切れ（致命）を返し、要求は
/// ちょうど 1 回しか送らない（再送しない。ホストはこれを結果不明 `Uncertain` として監査に残す）。
#[tokio::test]
async fn a_write_whose_reply_is_lost_times_out_without_a_resend() {
    let device = Device::start().await;
    let mut session = device.connect(Duration::from_secs(1)).await;
    device.drop_replies.store(true, Ordering::SeqCst);
    let result = session
        .write_batch(&[WriteItem::new(
            spec("D300", n(DataType::U16)),
            Value::F64(5.0),
        )])
        .await;
    assert_eq!(result, Err(FatalError::Timeout));
    assert!(!session.is_connected());
    // 機器が要求を処理したことを観測してから数える。
    tokio::time::timeout(LONG, async {
        while device.write_requests() == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("書き込みの要求が機器に届かない");
    assert_eq!(device.sim.get_word(SlmpDevice::D, 300), 5, "機器には効いた");
    assert_eq!(device.write_requests(), 1, "再送した");
}
