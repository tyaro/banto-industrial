//! 待ち受けソケットの bind（#1121 PR2a、docs/hub-desktop-tls-design.md §4.5 の 2.）。
//!
//! Windows では bind の前に `SO_EXCLUSIVEADDRUSE` を付ける。効き方はアドレスで違う
//! （この PC の Windows 11 で同じ利用者のソケットを使って確かめた結果）:
//!
//! - 127.0.0.1 に bind するとき（8723、PR3 のシェル専用の待ち受け）: 同じ利用者が
//!   `SO_REUSEADDR` 付きで同じアドレスへ bind しても、`SO_EXCLUSIVEADDRUSE` の有無に
//!   かかわらず断られる。ここでは別の利用者の bind への守り・二重の守りとして付ける。
//! - ワイルドカード（0.0.0.0）に bind するとき: 付けないと、別のソケットがより具体的な
//!   127.0.0.1 を後から取れて先取りされる。付ければ断られる。効くのは主にこちら。
//!
//! 平文の 8722 は `banto_server::start`（上流 crate）の中で bind されるので、ここからは
//! 差し替えられない。設計（§4.5 の 2.）が求めるのはシェル専用の待ち受けと 8723 で、8722 は
//! ワイルドカード（LAN）で bind すると上の先取りを受けうる。上流 banto に bind 済みの
//! listener を受け取る口が要る（別 Issue）。
//!
//! 閉じた後に接続が残っても再 bind できる（この PC の Windows 11 で、サーバー側 TIME_WAIT・
//! FIN_WAIT_2・接続が開いたまま listener だけ閉じた場合を確かめた。試験
//! `rebind_after_*` / `rebind_while_*`）。証明書の差し替えは待ち受けを閉じずに TLS の設定だけを
//! 替える設計（docs/hub-desktop-tls-design.md §4.2・§4.5）。再 bind に失敗したときは、今までどおり
//! 8723 の TLS 無しで起動して理由をログに出す（#644 のオーナー決定 B）。
//!
//! 文字列（`host:port`）の解決は `TcpListener::bind` と同じく `lookup_host` の結果を順に
//! 試し、最初に bind できたものを使う。失敗は最後のエラーをそのまま返すので、
//! `io::ErrorKind`（`AddrInUse` / `PermissionDenied` など）を見る
//! `runtime::bind_error_allows_port_fallback` の判定はそのまま効く。Windows 以外は
//! `TcpListener::bind` をそのまま使う。

use std::io;
use std::net::SocketAddr;

use tokio::net::{lookup_host, TcpListener, ToSocketAddrs};

/// `addr` を排他的に bind して listen する。Windows では `SO_EXCLUSIVEADDRUSE` 付き。
pub(crate) async fn bind_exclusive(addr: impl ToSocketAddrs) -> io::Result<TcpListener> {
    let mut last_err = None;
    for candidate in lookup_host(addr).await? {
        match bind_one(candidate).await {
            Ok(listener) => return Ok(listener),
            Err(err) => last_err = Some(err),
        }
    }
    Err(last_err.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "could not resolve to any address",
        )
    }))
}

#[cfg(windows)]
async fn bind_one(addr: SocketAddr) -> io::Result<TcpListener> {
    use socket2::{Domain, Protocol, Socket, Type};

    let socket = Socket::new(Domain::for_address(addr), Type::STREAM, Some(Protocol::TCP))?;
    // bind の前。握った後に付けても先取りは防げない。
    set_exclusive_addr_use(&socket)?;
    socket.bind(&addr.into())?;
    // tokio の `TcpListener::bind` と同じ backlog。
    socket.listen(1024)?;
    socket.set_nonblocking(true)?;
    TcpListener::from_std(socket.into())
}

/// `setsockopt(SOL_SOCKET, SO_EXCLUSIVEADDRUSE, 1)`。socket2 0.6 には専用の関数が無い。
#[cfg(windows)]
fn set_exclusive_addr_use(socket: &socket2::Socket) -> io::Result<()> {
    use std::os::windows::io::AsRawSocket;
    use windows_sys::Win32::Networking::WinSock::{
        setsockopt, SOCKET, SOCKET_ERROR, SOL_SOCKET, SO_EXCLUSIVEADDRUSE,
    };

    let on: u32 = 1;
    // SAFETY: ソケットは生きており、値は 4 バイトの u32 を指して長さも 4。
    let rc = unsafe {
        setsockopt(
            socket.as_raw_socket() as SOCKET,
            SOL_SOCKET,
            SO_EXCLUSIVEADDRUSE,
            (&raw const on).cast::<u8>(),
            size_of::<u32>() as i32,
        )
    };
    if rc == SOCKET_ERROR {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(windows))]
async fn bind_one(addr: SocketAddr) -> io::Result<TcpListener> {
    TcpListener::bind(addr).await
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use socket2::{Domain, Socket, Type};

    /// `getsockopt(SOL_SOCKET, SO_EXCLUSIVEADDRUSE)` の値。
    pub(crate) fn exclusive_addr_use(listener: &TcpListener) -> u32 {
        use std::os::windows::io::AsRawSocket;
        use windows_sys::Win32::Networking::WinSock::{
            getsockopt, SOCKET, SOCKET_ERROR, SOL_SOCKET, SO_EXCLUSIVEADDRUSE,
        };

        let mut value: u32 = 0;
        let mut len = size_of::<u32>() as i32;
        // SAFETY: ソケットは生きており、値は 4 バイトの u32 を指して長さも 4。
        let rc = unsafe {
            getsockopt(
                listener.as_raw_socket() as SOCKET,
                SOL_SOCKET,
                SO_EXCLUSIVEADDRUSE,
                (&raw mut value).cast::<u8>(),
                &mut len,
            )
        };
        assert_ne!(rc, SOCKET_ERROR, "{}", io::Error::last_os_error());
        value
    }

    /// 修正の有無で結果が変わる試験: `SO_EXCLUSIVEADDRUSE` が実際に付いている。
    #[tokio::test]
    async fn bound_listener_has_exclusive_addr_use() {
        let listener = bind_exclusive("127.0.0.1:0").await.unwrap();
        assert_eq!(exclusive_addr_use(&listener), 1);
    }

    /// 比較の対照: 素の `TcpListener::bind` では付かない（上の試験が意味を持つ確認）。
    #[tokio::test]
    async fn plain_bind_has_no_exclusive_addr_use() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        assert_eq!(exclusive_addr_use(&listener), 0);
    }

    /// 127.0.0.1 では同じ利用者の `SO_REUSEADDR` の bind は元から断られる（付与の有無に
    /// よらない）ので、これは回帰の確認で、修正の有無は上の試験が確かめる。
    #[tokio::test]
    async fn reuseaddr_socket_cannot_take_a_loopback_port() {
        let listener = bind_exclusive("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let socket = Socket::new(Domain::for_address(addr), Type::STREAM, None).unwrap();
        socket.set_reuse_address(true).unwrap();
        assert!(socket.bind(&addr.into()).is_err(), "{addr} was taken over");
    }

    /// 閉じた後に接続が残っていても、同じアドレスへ排他的に再 bind できる。
    /// `close_order` が場面ごとの閉じ方を決める（引数は accept した側・connect した側）。
    async fn rebind_after_close(
        close_order: impl FnOnce(tokio::net::TcpStream, std::net::TcpStream) -> Vec<Box<dyn Send>>,
        listener_dropped_before_streams: bool,
    ) {
        let listener = bind_exclusive("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let client = std::net::TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().await.unwrap();
        let mut keep: Vec<Box<dyn Send>> = Vec::new();
        if listener_dropped_before_streams {
            drop(listener);
            keep.extend(close_order(server, client));
        } else {
            keep.extend(close_order(server, client));
            drop(listener);
        }
        // `keep` に残した側は再 bind を試す間は開いたまま。
        let again = bind_exclusive(addr)
            .await
            .unwrap_or_else(|e| panic!("could not rebind {addr}: {e}"));
        assert_eq!(again.local_addr().unwrap(), addr);
        drop(keep);
    }

    /// サーバーが先に閉じ、クライアントも閉じる（サーバー側 TIME_WAIT）。
    #[tokio::test]
    async fn rebind_after_server_closes_first_then_client() {
        rebind_after_close(
            |server, client| {
                drop(server);
                drop(client);
                Vec::new()
            },
            false,
        )
        .await;
    }

    /// クライアントが先に閉じる。
    #[tokio::test]
    async fn rebind_after_client_closes_first() {
        rebind_after_close(
            |server, client| {
                drop(client);
                drop(server);
                Vec::new()
            },
            false,
        )
        .await;
    }

    /// サーバーが閉じ、クライアントは開いたまま（サーバー側 FIN_WAIT_2）。
    #[tokio::test]
    async fn rebind_while_client_stays_open_after_server_closes() {
        rebind_after_close(
            |server, client| {
                drop(server);
                vec![Box::new(client)]
            },
            false,
        )
        .await;
    }

    /// 受け付けた接続を開いたまま listener だけ閉じる。
    #[tokio::test]
    async fn rebind_after_listener_closes_with_accepted_connection_open() {
        rebind_after_close(
            |server, client| vec![Box::new(server), Box::new(client)],
            true,
        )
        .await;
    }

    #[tokio::test]
    async fn exclusive_bind_reports_addr_in_use_for_the_fallback_check() {
        let first = bind_exclusive("127.0.0.1:0").await.unwrap();
        let addr = first.local_addr().unwrap();
        let err = bind_exclusive(addr).await.unwrap_err();
        assert!(crate::runtime::bind_error_allows_port_fallback(err.kind()));
    }
}
