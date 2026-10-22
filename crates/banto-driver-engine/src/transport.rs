//! 通信の所有権と I/O の境界。各 future の期限は FrameSession が掛ける。

use std::time::Duration;

use banto_driver_api::{BoxFuture, FatalError};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// 1 接続の通信路。0 バイトの受信は切断として扱う。
/// キャンセル時にこの値を破棄すれば、通信路も解放されなければならない。
/// 生の future に期限はない。利用者は各呼び出しを必ず timeout で包むこと。
/// FrameSession は送受信を共通の期限で包み、close にも有限の上限を掛ける。
pub trait Transport: Send + 'static {
    fn send<'a>(&'a mut self, bytes: &'a [u8]) -> BoxFuture<'a, Result<(), FatalError>>;
    fn recv<'a>(&'a mut self, buf: &'a mut Vec<u8>) -> BoxFuture<'a, Result<usize, FatalError>>;
    fn discard_input(&mut self) -> BoxFuture<'_, ()>;
    fn close(&mut self) -> BoxFuture<'_, ()>;
}

/// TCP と、TCP 経由で生データを流す変換器の接続先。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TransportSpec {
    Tcp { host: String, port: u16 },
}

impl TransportSpec {
    /// 名前解決と接続を合わせて `connect_timeout` に制限する。
    pub async fn connect(
        &self,
        connect_timeout: Duration,
    ) -> Result<Box<dyn Transport>, FatalError> {
        match self {
            Self::Tcp { host, port } => {
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
        }
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
