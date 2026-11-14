//! 待ち受けソケットの bind（#1121 PR2a、docs/hub-desktop-tls-design.md §4.5 の 2.）。
//!
//! Windows では、`SO_EXCLUSIVEADDRUSE` を付けずに bind したポートへ、ほかのプロセスが
//! `SO_REUSEADDR` を付けて同じアドレスで bind でき、接続の振り分けを奪える場合がある
//! （ポートの先取り）。Hub は loopback で API キーや ログイン情報を受けるので、bind の
//! 前に必ず `SO_EXCLUSIVEADDRUSE` を付けて、握った後は同じアドレスに誰も bind できない
//! ようにする。Windows 以外は今までどおり `TcpListener::bind`（`SO_REUSEADDR` の扱いを
//! 勝手に変えない）。
//!
//! 文字列（`host:port`）の解決は `TcpListener::bind` と同じく `lookup_host` の結果を順に
//! 試し、最初に bind できたものを使う。失敗は最後のエラーをそのまま返すので、
//! `io::ErrorKind`（`AddrInUse` / `PermissionDenied` など）を見る
//! `runtime::bind_error_allows_port_fallback` の判定はそのまま効く。
//!
//! 平文の 8722 は `banto_server::start`（上流 crate）の中で bind されるので、ここからは
//! 差し替えられない。上流が bind 済みの listener を受け取る API を持った時点で
//! この関数へ寄せる（#1121 PR2a の持ち越し）。

use std::io;
use std::net::SocketAddr;

use tokio::net::{lookup_host, TcpListener, ToSocketAddrs};

/// `addr` を排他的に bind して listen する。Windows では `SO_EXCLUSIVEADDRUSE` 付き。
pub(crate) async fn bind_exclusive(addr: impl ToSocketAddrs) -> io::Result<TcpListener> {
    let mut last_err = None;
    for candidate in lookup_host(addr).await? {
        match bind_one(candidate) {
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
fn bind_one(addr: SocketAddr) -> io::Result<TcpListener> {
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
fn bind_one(addr: SocketAddr) -> io::Result<TcpListener> {
    // 同期の bind は tokio の `TcpListener::bind` と同じ（std で bind → nonblocking）。
    let std_listener = std::net::TcpListener::bind(addr)?;
    std_listener.set_nonblocking(true)?;
    TcpListener::from_std(std_listener)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use socket2::{Domain, Socket, Type};

    /// 別ソケットが `SO_REUSEADDR` 付きで同じアドレスへ bind を試みる。
    pub(crate) fn reuse_bind_fails(addr: SocketAddr) -> bool {
        let socket = Socket::new(Domain::for_address(addr), Type::STREAM, None).unwrap();
        socket.set_reuse_address(true).unwrap();
        socket.bind(&addr.into()).is_err()
    }

    #[tokio::test]
    async fn reuseaddr_socket_cannot_take_an_exclusively_bound_port() {
        let listener = bind_exclusive("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        assert!(reuse_bind_fails(addr), "{addr} was taken over");
    }

    #[tokio::test]
    async fn exclusive_bind_reports_addr_in_use_for_the_fallback_check() {
        let first = bind_exclusive("127.0.0.1:0").await.unwrap();
        let addr = first.local_addr().unwrap();
        let err = bind_exclusive(addr).await.unwrap_err();
        assert!(crate::runtime::bind_error_allows_port_fallback(err.kind()));
    }
}
