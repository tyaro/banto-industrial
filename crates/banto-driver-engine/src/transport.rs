//! 通信の所有権と I/O の境界。各 future の期限は FrameSession が掛ける。

use std::time::Duration;

use banto_driver_api::Transport as TransportKind;
use banto_driver_api::{keys, BoxFuture, FatalError, FieldError, Settings};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// 1 接続の通信路。0 バイトの受信は切断として扱う。
/// キャンセル時にこの値を破棄すれば、通信路も解放されなければならない。
/// 生の future に期限はない。利用者は各呼び出しを必ず timeout で包むこと。
/// FrameSession は送受信を共通の期限で包み、close にも有限の上限を掛ける。
pub trait Transport: Send + 'static {
    /// `len` バイトを線に出しきるのにかかる時間の見積もり（既定は 0）。
    /// シリアルの `send` は drain（送り終えの待ち）をしない（Unix の `flush` は poll の中で
    /// 同期の `tcdrain` を呼び runtime ごと止める。Windows の `flush` は drain にならない）。
    /// そこでエンジンが「送り終えた」時刻を
    /// `max(send が返った時刻, send を始めた時刻 + tx_time)` と見積もるのに使う（設計 §7.3.1・§14）。
    fn tx_time(&self, _len: usize) -> Duration {
        Duration::ZERO
    }
    fn send<'a>(&'a mut self, bytes: &'a [u8]) -> BoxFuture<'a, Result<(), FatalError>>;
    fn recv<'a>(&'a mut self, buf: &'a mut Vec<u8>) -> BoxFuture<'a, Result<usize, FatalError>>;
    fn discard_input(&mut self) -> BoxFuture<'_, ()>;
    fn close(&mut self) -> BoxFuture<'_, ()>;
}

/// 通信路の接続先。broker はシリアルの同期の手順（設計 §7.3）が入るまで `Serial` を開かない。
/// 変種が増えても外の `match` を壊さないよう `#[non_exhaustive]` にした
/// （変種の作成は従来どおり。外に `match` は無い）。
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TransportSpec {
    Tcp {
        host: String,
        port: u16,
    },
    /// ローカルのシリアルポート（COM / tty）。
    Serial {
        port: SerialPortName,
        line: SerialLine,
    },
    /// TCP 経由で生のシリアルのバイトを流す変換器。接続は `Tcp` と同じ。
    SerialOverTcp {
        host: String,
        port: u16,
    },
}

impl TransportSpec {
    /// TCP の名前解決と接続を合わせて `connect_timeout` に制限する（シリアルのポートを開く処理には効かない）。
    pub async fn connect(
        &self,
        connect_timeout: Duration,
    ) -> Result<Box<dyn Transport>, FatalError> {
        match self {
            Self::Tcp { host, port } | Self::SerialOverTcp { host, port } => {
                let stream = tokio::time::timeout(
                    connect_timeout,
                    tokio::net::TcpStream::connect((host.as_str(), *port)),
                )
                .await
                .map_err(|_| FatalError::Timeout)?
                .map_err(io_error)?;
                stream.set_nodelay(true).map_err(io_error)?;
                Ok(Box::new(TcpTransport::new(stream)))
            }
            Self::Serial { port, line } => {
                // ポートを開くのは同期の呼び出しで、`connect_timeout` は効かない（Windows の
                // CreateFileW / Linux の open は通常すぐ返る。ハングする仮想ポートは実機の確かめの対象）。
                SerialTransport::open(port, *line).map(|t| Box::new(t) as Box<dyn Transport>)
            }
        }
    }

    /// 接続設定から接続先を組み立てる純粋な関数（設計 §7.1）。
    /// `transport` キー（未指定は `tcp`）がドライバの `descriptor().transports` に含まれることと、
    /// host / port / serial_port / 線の設定の形を検査する。ネットワークにも OS にも触らない。
    pub fn from_settings(
        settings: &Settings,
        descriptor_transports: &[TransportKind],
    ) -> Result<Self, FieldError> {
        let (kind, label) = match settings.get(keys::TRANSPORT).unwrap_or("tcp") {
            "tcp" => (TransportKind::Tcp, "TCP"),
            "serial" => (TransportKind::Serial, "シリアル"),
            "serial_over_tcp" => (TransportKind::SerialOverTcp, "シリアル over TCP"),
            _ => {
                return Err(FieldError::new(
                    keys::TRANSPORT,
                    "このエンジンで未対応の通信方式です",
                ))
            }
        };
        if !descriptor_transports.contains(&kind) {
            return Err(FieldError::new(
                keys::TRANSPORT,
                format!("このドライバは {label} 通信に対応していません"),
            ));
        }
        if kind == TransportKind::Serial {
            let name = settings
                .get(keys::SERIAL_PORT)
                .ok_or_else(|| FieldError::new(keys::SERIAL_PORT, "ポート名を指定してください"))?;
            let port = SerialPortName::parse(name)
                .map_err(|message| FieldError::new(keys::SERIAL_PORT, message))?;
            return Ok(Self::Serial {
                port,
                line: SerialLine::from_settings(settings)?,
            });
        }
        let host = settings
            .get(keys::HOST)
            .filter(|host| !host.trim().is_empty())
            .ok_or_else(|| FieldError::new(keys::HOST, "接続先を指定してください"))?
            .to_owned();
        let port = settings
            .get(keys::PORT)
            .and_then(|port| port.parse::<u16>().ok())
            .filter(|port| *port != 0)
            .ok_or_else(|| FieldError::new(keys::PORT, "1〜65535 で指定してください"))?;
        Ok(if kind == TransportKind::Tcp {
            Self::Tcp { host, port }
        } else {
            Self::SerialOverTcp { host, port }
        })
    }
}

/// データビット（7 または 8）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataBits {
    Seven,
    Eight,
}

/// パリティ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Parity {
    None,
    Even,
    Odd,
}

/// ストップビット（1 または 2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StopBits {
    One,
    Two,
}

/// シリアルの線の設定。既定は 9600 / 8 / none / 1。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SerialLine {
    pub baud: u32,
    pub data_bits: DataBits,
    pub parity: Parity,
    pub stop_bits: StopBits,
}

impl Default for SerialLine {
    fn default() -> Self {
        Self {
            baud: 9600,
            data_bits: DataBits::Eight,
            parity: Parity::None,
            stop_bits: StopBits::One,
        }
    }
}

impl SerialLine {
    /// 受け付ける baud の範囲。
    pub const BAUD_RANGE: std::ops::RangeInclusive<u32> = 300..=921_600;

    /// 1 文字の時間（スタートビット + データ + パリティ + ストップ）。ナノ秒で切り上げる。
    pub fn char_time(&self) -> Duration {
        let bits: u64 =
            1 + match self.data_bits {
                DataBits::Seven => 7,
                DataBits::Eight => 8,
            } + u64::from(self.parity != Parity::None)
                + match self.stop_bits {
                    StopBits::One => 1,
                    StopBits::Two => 2,
                };
        Duration::from_nanos((bits * 1_000_000_000).div_ceil(u64::from(self.baud.max(1))))
    }

    /// `baud` / `data_bits` / `parity` / `stop_bits` から作る。未指定は既定値。
    pub fn from_settings(settings: &Settings) -> Result<Self, FieldError> {
        let mut line = Self::default();
        if let Some(text) = settings.get(keys::BAUD) {
            line.baud = text
                .trim()
                .parse::<u32>()
                .ok()
                .filter(|b| Self::BAUD_RANGE.contains(b))
                .ok_or_else(|| {
                    FieldError::new(
                        keys::BAUD,
                        format!(
                            "{}〜{} の整数で指定してください",
                            Self::BAUD_RANGE.start(),
                            Self::BAUD_RANGE.end()
                        ),
                    )
                })?;
        }
        if let Some(text) = settings.get(keys::DATA_BITS) {
            line.data_bits = match text.trim() {
                "7" => DataBits::Seven,
                "8" => DataBits::Eight,
                _ => {
                    return Err(FieldError::new(
                        keys::DATA_BITS,
                        "7 か 8 で指定してください",
                    ))
                }
            };
        }
        if let Some(text) = settings.get(keys::PARITY) {
            line.parity = match text.trim() {
                "none" => Parity::None,
                "even" => Parity::Even,
                "odd" => Parity::Odd,
                _ => {
                    return Err(FieldError::new(
                        keys::PARITY,
                        "none / even / odd のいずれかで指定してください",
                    ))
                }
            };
        }
        if let Some(text) = settings.get(keys::STOP_BITS) {
            line.stop_bits = match text.trim() {
                "1" => StopBits::One,
                "2" => StopBits::Two,
                _ => {
                    return Err(FieldError::new(
                        keys::STOP_BITS,
                        "1 か 2 で指定してください",
                    ))
                }
            };
        }
        Ok(line)
    }
}

/// 許可リストの文法で検査したシリアルポート名（checklist §5「外から来た名前」）。
/// Windows 形式 `COM1`〜`COM256`（`COM` は大文字小文字を問わず大文字に正規化）と
/// Unix 形式 `/dev/tty<英字 1 文字以上><数字 1 文字以上>` だけを通す。
/// 検査は OS を問わず両方の文法で行う（開けるかどうかは OS 次第）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SerialPortName(String);

impl SerialPortName {
    pub fn parse(text: &str) -> Result<Self, String> {
        const MESSAGE: &str =
            "COM1〜COM256 か /dev/tty<英字><数字>（例 /dev/ttyUSB0）の形で指定してください";
        let all_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        if text.len() >= 3 && text.is_char_boundary(3) && text[..3].eq_ignore_ascii_case("COM") {
            let digits = &text[3..];
            let number = if all_digits(digits) && !digits.starts_with('0') {
                digits.parse::<u16>().ok().filter(|n| *n <= 256)
            } else {
                None
            };
            return match number {
                Some(n) => Ok(Self(format!("COM{n}"))),
                None => Err(MESSAGE.into()),
            };
        }
        if let Some(rest) = text.strip_prefix("/dev/tty") {
            let letters = rest.bytes().take_while(u8::is_ascii_alphabetic).count();
            if letters >= 1 && all_digits(&rest[letters..]) {
                return Ok(Self(text.to_owned()));
            }
        }
        Err(MESSAGE.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SerialPortName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// `SerialTransport` が包むストリーム。試験で偽のストリームに差し替えるための crate 内の境界。
trait SerialIo: AsyncRead + AsyncWrite + Unpin + Send + 'static {
    fn clear_input(&mut self) -> std::io::Result<()>;
}

impl SerialIo for tokio_serial::SerialStream {
    fn clear_input(&mut self) -> std::io::Result<()> {
        use tokio_serial::SerialPort;
        self.clear(tokio_serial::ClearBuffer::Input)
            .map_err(std::io::Error::from)
    }
}

/// ローカルのシリアルポートの通信路。1 フレームは 1 回の `write_all` で出しきる。
///
/// `flush` は呼ばない。Unix の `tokio-serial` の `poll_flush` は poll の中で同期の `tcdrain` を
/// 呼び、物理的な送信の完了まで戻らないので、外側の期限で打ち切れず runtime も止まる。
/// Windows の `flush` は drain にならない。送り終えはエンジンが `tx_time` の見積もりと
/// 非同期の `sleep_until` で扱う。`write_all` は Unix では `AsyncFd` の非ブロッキングの書き込みで、
/// カーネルのバッファが一杯なら非同期に待つだけなので期限で打ち切れる。
pub struct SerialTransport {
    stream: Box<dyn SerialIo>,
    char_time: Duration,
}

impl SerialTransport {
    /// ポートを開く。失敗は致命（他のアプリが使用中のことが多い。設計 §7.2）。
    pub fn open(port: &SerialPortName, line: SerialLine) -> Result<Self, FatalError> {
        use tokio_serial::SerialPortBuilderExt;
        let stream = tokio_serial::new(port.as_str(), line.baud)
            .data_bits(match line.data_bits {
                DataBits::Seven => tokio_serial::DataBits::Seven,
                DataBits::Eight => tokio_serial::DataBits::Eight,
            })
            .parity(match line.parity {
                Parity::None => tokio_serial::Parity::None,
                Parity::Even => tokio_serial::Parity::Even,
                Parity::Odd => tokio_serial::Parity::Odd,
            })
            .stop_bits(match line.stop_bits {
                StopBits::One => tokio_serial::StopBits::One,
                StopBits::Two => tokio_serial::StopBits::Two,
            })
            .flow_control(tokio_serial::FlowControl::None)
            .open_native_async()
            .map_err(|e| {
                FatalError::Io(format!(
                    "シリアルポート {port} を開けない（他のアプリがこのポートを使っていないか、\
                     ポート名が合っているか確認してください）: {e}"
                ))
            })?;
        Ok(Self::new(stream, line))
    }

    /// 開いたストリーム（試験では pty の対の片側）を包む。
    pub fn new(stream: tokio_serial::SerialStream, line: SerialLine) -> Self {
        Self::from_io(Box::new(stream), line)
    }

    fn from_io(stream: Box<dyn SerialIo>, line: SerialLine) -> Self {
        Self {
            stream,
            char_time: line.char_time(),
        }
    }
}

impl Transport for SerialTransport {
    fn tx_time(&self, len: usize) -> Duration {
        self.char_time
            .saturating_mul(u32::try_from(len).unwrap_or(u32::MAX))
    }

    fn send<'a>(&'a mut self, bytes: &'a [u8]) -> BoxFuture<'a, Result<(), FatalError>> {
        Box::pin(async move {
            // flush（= Unix では同期の tcdrain）は呼ばない。型の doc を参照。
            self.stream.write_all(bytes).await.map_err(io_error)
        })
    }

    fn recv<'a>(&'a mut self, buf: &'a mut Vec<u8>) -> BoxFuture<'a, Result<usize, FatalError>> {
        Box::pin(async move {
            let mut chunk = [0; 256];
            let n = self.stream.read(&mut chunk).await.map_err(io_error)?;
            if n == 0 {
                return Err(FatalError::Io("シリアルポートが閉じた".into()));
            }
            buf.extend_from_slice(&chunk[..n]);
            Ok(n)
        })
    }

    fn discard_input(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            if let Err(error) = self.stream.clear_input() {
                tracing::debug!(%error, "シリアルの受信バッファを捨てられなかった");
            }
        })
    }

    fn close(&mut self) -> BoxFuture<'_, ()> {
        // ストリームを drop するとポートが閉じる。ここで待つものはない。
        Box::pin(async {})
    }
}

/// TCP と同じ送受信を行うストリーム。`tokio::io::duplex` も試験に使える。
pub struct StreamTransport<S>(S);

/// TCP の通信路。
pub type TcpTransport = StreamTransport<tokio::net::TcpStream>;

impl<S> StreamTransport<S> {
    pub fn new(stream: S) -> Self {
        Self(stream)
    }
}

fn io_error(error: std::io::Error) -> FatalError {
    FatalError::Io(error.to_string())
}

impl<S: AsyncRead + AsyncWrite + Unpin + Send + 'static> Transport for StreamTransport<S> {
    fn send<'a>(&'a mut self, bytes: &'a [u8]) -> BoxFuture<'a, Result<(), FatalError>> {
        Box::pin(async move { self.0.write_all(bytes).await.map_err(io_error) })
    }

    fn recv<'a>(&'a mut self, buf: &'a mut Vec<u8>) -> BoxFuture<'a, Result<usize, FatalError>> {
        Box::pin(async move {
            let mut chunk = [0; 4096];
            let n = self.0.read(&mut chunk).await.map_err(io_error)?;
            if n == 0 {
                return Err(FatalError::Io("相手が接続を閉じた".into()));
            }
            buf.extend_from_slice(&chunk[..n]);
            Ok(n)
        })
    }

    fn discard_input(&mut self) -> BoxFuture<'_, ()> {
        // TCP は受信の破棄後に再利用しない。接続そのものを閉じる。
        Box::pin(async {})
    }

    fn close(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let _ = self.0.shutdown().await;
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(pairs: &[(&str, &str)]) -> Settings {
        Settings::from_pairs(pairs.iter().copied()).unwrap()
    }

    fn line(baud: u32, data_bits: DataBits, parity: Parity, stop_bits: StopBits) -> SerialLine {
        SerialLine {
            baud,
            data_bits,
            parity,
            stop_bits,
        }
    }

    #[test]
    fn char_time_counts_start_data_parity_and_stop_bits() {
        use DataBits::{Eight, Seven};
        use Parity::{Even, None as NoParity};
        use StopBits::{One, Two};
        for (l, bits) in [
            (line(9600, Eight, NoParity, One), 10),
            (line(9600, Eight, Even, One), 11),
            (line(9600, Seven, Even, Two), 11),
            (line(9600, Eight, NoParity, Two), 11),
            (line(9600, Seven, NoParity, One), 9),
        ] {
            // 切り上げなので、ビット数 × 1/baud 以上で 1 ns を超えて大きくならない。
            let exact = bits as f64 / 9600.0;
            let got = l.char_time().as_secs_f64();
            assert!(got >= exact - 1e-12 && got - exact < 2e-9, "{l:?}");
        }
        assert_eq!(
            SerialLine::default().char_time(),
            Duration::from_nanos(1_041_667)
        );
        // 3.5 文字 ≈ 3.65 ms（9600 8N1）。
        assert_eq!(
            SerialLine::default().char_time().mul_f64(3.5).as_micros(),
            3645
        );
    }

    #[test]
    fn line_settings_default_and_reject_bad_values() {
        assert_eq!(
            SerialLine::from_settings(&Settings::new()).unwrap(),
            SerialLine::default()
        );
        let l = SerialLine::from_settings(&settings(&[
            ("baud", "38400"),
            ("data_bits", "7"),
            ("parity", "odd"),
            ("stop_bits", "2"),
        ]))
        .unwrap();
        assert_eq!(l, line(38400, DataBits::Seven, Parity::Odd, StopBits::Two));
        for (key, value) in [
            ("baud", "299"),
            ("baud", "921601"),
            ("baud", "abc"),
            ("baud", ""),
            ("baud", "-9600"),
            ("data_bits", "9"),
            ("data_bits", "five"),
            ("parity", "mark"),
            ("parity", "Even"),
            ("stop_bits", "3"),
            ("stop_bits", "1.5"),
        ] {
            let e = SerialLine::from_settings(&settings(&[(key, value)])).unwrap_err();
            assert_eq!(e.key, key, "{key}={value}");
        }
        for value in ["300", "921600"] {
            assert!(SerialLine::from_settings(&settings(&[("baud", value)])).is_ok());
        }
    }

    #[test]
    fn port_names_follow_the_allow_list() {
        for (text, normalized) in [
            ("COM1", "COM1"),
            ("COM10", "COM10"),
            ("COM256", "COM256"),
            ("com3", "COM3"),
            ("Com12", "COM12"),
            ("/dev/ttyS0", "/dev/ttyS0"),
            ("/dev/ttyUSB0", "/dev/ttyUSB0"),
            ("/dev/ttyACM12", "/dev/ttyACM12"),
        ] {
            assert_eq!(SerialPortName::parse(text).unwrap().as_str(), normalized);
        }
        for text in [
            "",
            "COM",
            "COM0",
            "COM257",
            "COM01",
            "COM1:",
            "COM1 ",
            " COM1",
            "COM-1",
            "COM１",
            "COM99999999999",
            "\\\\.\\COM1",
            "NUL",
            "CON",
            "LPT1",
            "/dev/tty",
            "/dev/ttyS",
            "/dev/ttyUSB",
            "/dev/tty0",
            "/dev/ttyS0/x",
            "/dev/ttyS0 ",
            "/dev/../dev/ttyS0",
            "/dev/ttyS0\n",
            "/dev/null",
            "/dev/tty/../sda1",
            "ttyS0",
        ] {
            assert!(SerialPortName::parse(text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn transport_spec_is_built_from_settings() {
        use TransportKind::{Serial, SerialOverTcp, Tcp};
        let tcp = settings(&[("host", "10.0.0.5"), ("port", "502")]);
        assert_eq!(
            TransportSpec::from_settings(&tcp, &[Tcp]).unwrap(),
            TransportSpec::Tcp {
                host: "10.0.0.5".into(),
                port: 502
            }
        );
        let over = settings(&[
            ("transport", "serial_over_tcp"),
            ("host", "conv.local"),
            ("port", "4001"),
        ]);
        assert_eq!(
            TransportSpec::from_settings(&over, &[Tcp, SerialOverTcp]).unwrap(),
            TransportSpec::SerialOverTcp {
                host: "conv.local".into(),
                port: 4001
            }
        );
        let serial = settings(&[
            ("transport", "serial"),
            ("serial_port", "com4"),
            ("baud", "19200"),
            ("parity", "even"),
        ]);
        assert_eq!(
            TransportSpec::from_settings(&serial, &[Serial]).unwrap(),
            TransportSpec::Serial {
                port: SerialPortName::parse("COM4").unwrap(),
                line: line(19200, DataBits::Eight, Parity::Even, StopBits::One),
            }
        );
        // (設定, ドライバの対応, 指すキー)
        let cases: Vec<(Settings, &[TransportKind], &str)> = vec![
            (settings(&[("transport", "udp")]), &[Tcp], "transport"),
            (
                settings(&[("host", "h"), ("port", "1")]),
                &[Serial],
                "transport",
            ),
            (over.clone(), &[Tcp], "transport"),
            (serial.clone(), &[Tcp, SerialOverTcp], "transport"),
            (settings(&[("port", "502")]), &[Tcp], "host"),
            (settings(&[("host", "  "), ("port", "502")]), &[Tcp], "host"),
            (settings(&[("host", "h")]), &[Tcp], "port"),
            (settings(&[("host", "h"), ("port", "0")]), &[Tcp], "port"),
            (
                settings(&[("host", "h"), ("port", "65536")]),
                &[Tcp],
                "port",
            ),
            (
                settings(&[("transport", "serial_over_tcp"), ("host", "h")]),
                &[SerialOverTcp],
                "port",
            ),
            (
                settings(&[("transport", "serial")]),
                &[Serial],
                "serial_port",
            ),
            (
                settings(&[("transport", "serial"), ("serial_port", "NUL")]),
                &[Serial],
                "serial_port",
            ),
            (
                settings(&[
                    ("transport", "serial"),
                    ("serial_port", "COM1"),
                    ("baud", "1"),
                ]),
                &[Serial],
                "baud",
            ),
        ];
        for (s, transports, key) in cases {
            let e = TransportSpec::from_settings(&s, transports).unwrap_err();
            assert_eq!(e.key, key, "{s:?}");
        }
        assert_eq!(
            TransportSpec::from_settings(&settings(&[("transport", "udp")]), &[Tcp])
                .unwrap_err()
                .message,
            "このエンジンで未対応の通信方式です"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn serial_transport_round_trips_and_discards_input_over_a_pty_pair() {
        use tokio::time::timeout;
        const T: Duration = Duration::from_secs(2);
        let (a, mut b) = tokio_serial::SerialStream::pair().unwrap();
        let mut transport = SerialTransport::new(a, SerialLine::default());
        assert_eq!(transport.tx_time(8), SerialLine::default().char_time() * 8);

        timeout(T, transport.send(&[1, 2, 3]))
            .await
            .unwrap()
            .unwrap();
        let mut got = [0u8; 3];
        timeout(T, b.read_exact(&mut got)).await.unwrap().unwrap();
        assert_eq!(got, [1, 2, 3]);

        timeout(T, b.write_all(b"ok")).await.unwrap().unwrap();
        let mut buf = Vec::new();
        while buf.len() < 2 {
            timeout(T, transport.recv(&mut buf)).await.unwrap().unwrap();
        }
        assert_eq!(buf, b"ok");

        // 溜まった受信を捨てると、その後に届いた分だけが読める。
        timeout(T, b.write_all(b"stale")).await.unwrap().unwrap();
        timeout(T, b.flush()).await.unwrap().unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        timeout(T, transport.discard_input()).await.unwrap();
        timeout(T, b.write_all(b"new")).await.unwrap().unwrap();
        let mut buf = Vec::new();
        while buf.len() < 3 {
            timeout(T, transport.recv(&mut buf)).await.unwrap().unwrap();
        }
        assert_eq!(buf, b"new");
        timeout(T, transport.close()).await.unwrap();
    }

    /// 偽のストリーム。`flush_panics` なら `poll_flush` で panic、`write_pending` なら書き込みが
    /// 永遠に `Pending`（送信バッファが一杯の状態）。
    struct FakeIo {
        flush_panics: bool,
        write_pending: bool,
        written: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    }

    impl AsyncRead for FakeIo {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            _: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Pending
        }
    }

    impl AsyncWrite for FakeIo {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            if self.write_pending {
                return std::task::Poll::Pending;
            }
            self.written.lock().unwrap().extend_from_slice(buf);
            std::task::Poll::Ready(Ok(buf.len()))
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            assert!(
                !self.flush_panics,
                "send が flush を呼んだ（同期 tcdrain の再発）"
            );
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    impl SerialIo for FakeIo {
        fn clear_input(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn serial_send_writes_without_calling_flush() {
        let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let io = FakeIo {
            flush_panics: true,
            write_pending: false,
            written: written.clone(),
        };
        let mut transport = SerialTransport::from_io(Box::new(io), SerialLine::default());
        transport.send(&[1, 2, 3]).await.unwrap();
        assert_eq!(*written.lock().unwrap(), vec![1, 2, 3]);
    }

    /// 送信バッファが一杯（書き込みが `Pending`）でも、send は期限で打ち切れ、
    /// current_thread の runtime の他のタイマーも進む。直す前の実装（`flush` が同期の
    /// `tcdrain` でブロックする）では、この期限の `timeout_at` が働かず runtime ごと止まる
    /// （そのため落ちる試験は current_thread を止めてしまうので置かない）。
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn serial_send_is_cancellable_by_deadline_while_runtime_keeps_running() {
        use tokio::time::{sleep, timeout_at, Instant};
        let io = FakeIo {
            flush_panics: true,
            write_pending: true,
            written: Default::default(),
        };
        let mut transport = SerialTransport::from_io(Box::new(io), SerialLine::default());
        let other = tokio::spawn(async {
            sleep(Duration::from_millis(100)).await;
            Instant::now()
        });
        let start = Instant::now();
        let deadline = start + Duration::from_secs(1);
        let result = timeout_at(deadline, transport.send(&[0; 256])).await;
        assert!(result.is_err(), "期限で打ち切れること");
        assert_eq!(Instant::now(), deadline);
        let other_done = other.await.unwrap();
        assert_eq!(other_done, start + Duration::from_millis(100));
    }
}
