//! シェルがサービスの health を確かめる probe（#392 段階 B の B2-4a、
//! `docs/chronogazer-service-design.md` §3.2・2026-10-14 オーナー決定 O8）。
//!
//! 起動判定（[`crate::shell_launch::decide`]）が SCM の `Running` を見たときだけ
//! 呼ぶ（[`crate::shell_launch::needs_probe`]）。手順:
//!
//! 1. `run\endpoint.json`（[`crate::endpoint::read_live_endpoint`]）を読む。無い・
//!    壊れている・書いたプロセスが生きていない → [`ServiceHealth::EndpointMissing`]。
//! 2. 自分の DB の `scope`（[`own_db_scope`]）を求める。**ルートが既にあるときだけ**
//!    （`banto_instance_lock::path_scope_id` は親ディレクトリを作るので、無いルートを
//!    probe が作らないため）。
//! 3. `http://127.0.0.1:<port>/api/host` に std の TCP で 1 回だけ GET する
//!    （`Host` は loopback、`X-Banto-Client: banto`、`Connection: close`）。接続・
//!    読み書きを合わせて**全体で** [`PROBE_TIMEOUT`]（1.5 秒、banto-hub の
//!    `HttpHubHealthProbe` と同じ）まで。少しずつ送り続ける相手でも打ち切る。
//! 4. 応答を純関数 [`classify_host_response`] で分類する。
//!
//! # Healthy の条件（O8）
//!
//! `200` かつ本文が JSON で、`product == "chronogazer"`・`hostKind` が期待の host
//! （本番は `service`）・`scope` が endpoint の値に一致し、2. で自分の DB の `scope`
//! が求まっていればそれにも一致したときだけ。pid は再利用されるので、endpoint.json
//! の pid が生きていても別のプロセスかもしれない - 最終の確認はこの応答で行う。
//!
//! | 起きたこと                                   | 結果                     |
//! | -------------------------------------------- | ------------------------ |
//! | endpoint.json が使えない                     | `EndpointMissing`        |
//! | TCP でつながらない（拒否・時間切れ）         | `Unreachable`            |
//! | つながった後に読み書きが失敗・時間切れ・途中で切れた | `ProbeFailed`    |
//! | HTTP でない・`200` でない・JSON でない・`product` が違う・欄が無い | `NotChronoGazer` |
//! | `hostKind` が違う                            | `WrongHostKind`          |
//! | `scope` が違う                               | `WrongScope`             |
//!
//! 期待の host を引数で受けるのは、B2-4c のデバッグビルドだけの確認の口（O7。
//! `chronogazer-server` のコンソール版を相手に「Running → 接続」を実機で確かめる）
//! のため。本番の経路は [`probe_service`]（`service` 固定）。
//!
//! # しないこと
//!
//! HTTP の実装は最小（`reqwest` などの依存を足さない。本文は 200 バイト程度）:
//! keep-alive・リダイレクト・TLS・圧縮は扱わない。`Content-Length` と chunked の
//! 両方を読む（axum は前者を返すが、形に依らず読めるように）。

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use crate::endpoint::{read_live_endpoint, EndpointInfo};
use crate::host_info::PRODUCT;
use crate::instance_lock::ChronoGazerHost;
use crate::paths::DataLayout;
use crate::shell_launch::ServiceHealth;

/// probe 全体（接続 + 往復）の上限。
pub const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

/// probe の経路。
pub const HOST_PATH: &str = "/api/host";

/// 読む応答の上限（本物は数百バイト）。超えたら ChronoGazer の応答ではない。
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

/// サービスの health を確かめる（本番の経路。期待の host は `service`）。
pub fn probe_service(layout: &DataLayout) -> ServiceHealth {
    probe_service_as(layout, ChronoGazerHost::Service, PROBE_TIMEOUT)
}

/// 期待の host と上限を指定して確かめる（O7 の確認の口とテスト用）。
pub fn probe_service_as(
    layout: &DataLayout,
    expected_host: ChronoGazerHost,
    timeout: Duration,
) -> ServiceHealth {
    let Some(endpoint) = read_live_endpoint(&layout.endpoint_file()) else {
        return ServiceHealth::EndpointMissing;
    };
    let own_scope = match own_db_scope(layout) {
        Ok(scope) => scope,
        // 求められないときは確かめずに通すのではなく、つながない（fail closed）。
        Err(err) => {
            return ServiceHealth::ProbeFailed(format!(
                "自分の DB の scope を求められません: {err}"
            ))
        }
    };
    probe_endpoint(&endpoint, expected_host, own_scope.as_deref(), timeout)
}

/// 自分の DB の `scope`（[`crate::instance_lock::InstanceGuard::scope`] と同じ値）。
/// ルートがまだ無ければ `None`（作らない。モジュール doc の 2.）。
pub fn own_db_scope(layout: &DataLayout) -> std::io::Result<Option<String>> {
    if !layout.root().is_dir() {
        return Ok(None);
    }
    banto_instance_lock::path_scope_id(&layout.db_path()).map(|(id, _)| Some(id))
}

/// endpoint の port に GET して分類する。
pub fn probe_endpoint(
    endpoint: &EndpointInfo,
    expected_host: ChronoGazerHost,
    own_scope: Option<&str>,
    timeout: Duration,
) -> ServiceHealth {
    let port = endpoint.port;
    let raw = match fetch_host(port, timeout) {
        Ok(raw) => raw,
        Err(FetchError::Connect) => return ServiceHealth::Unreachable { port },
        Err(FetchError::Io(message)) => return ServiceHealth::ProbeFailed(message),
        Err(FetchError::TooLarge) => return ServiceHealth::NotChronoGazer { port },
    };
    match parse_http_response(&raw) {
        HttpParse::Response { status, body } => {
            classify_host_response(status, &body, endpoint, expected_host, own_scope)
        }
        HttpParse::NotHttp => ServiceHealth::NotChronoGazer { port },
        HttpParse::Truncated => ServiceHealth::ProbeFailed("応答が途中で切れました".to_string()),
    }
}

/// `GET /api/host` の応答を分類する（純関数。モジュール doc の表）。
pub fn classify_host_response(
    status: u16,
    body: &[u8],
    endpoint: &EndpointInfo,
    expected_host: ChronoGazerHost,
    own_scope: Option<&str>,
) -> ServiceHealth {
    let port = endpoint.port;
    let not_chronogazer = ServiceHealth::NotChronoGazer { port };
    if status != 200 {
        return not_chronogazer;
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return not_chronogazer;
    };
    let field = |name: &str| value.get(name).and_then(serde_json::Value::as_str);
    if field("product") != Some(PRODUCT) {
        return not_chronogazer;
    }
    let (Some(kind), Some(scope)) = (field("hostKind"), field("scope")) else {
        return not_chronogazer;
    };
    if kind != expected_host.as_str() {
        return ServiceHealth::WrongHostKind {
            port,
            kind: kind.to_string(),
        };
    }
    if scope != endpoint.scope || own_scope.is_some_and(|own| own != scope) {
        return ServiceHealth::WrongScope { port };
    }
    ServiceHealth::Healthy {
        port,
        version: field("version").unwrap_or_default().to_string(),
    }
}

#[derive(Debug)]
enum FetchError {
    /// TCP でつながらない。
    Connect,
    /// つながった後の失敗（時間切れを含む）。
    Io(String),
    /// 応答が上限を超えた。
    TooLarge,
}

fn remaining(deadline: Instant) -> Option<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|left| !left.is_zero())
}

fn fetch_host(port: u16, timeout: Duration) -> Result<Vec<u8>, FetchError> {
    let deadline = Instant::now() + timeout;
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&addr, timeout).map_err(|_| FetchError::Connect)?;
    let io =
        |err: std::io::Error| FetchError::Io(format!("{HOST_PATH} の往復に失敗しました: {err}"));
    let timed_out = || {
        FetchError::Io(format!(
            "{HOST_PATH} の応答が {timeout:?} 以内に返りませんでした"
        ))
    };

    let request = format!(
        "GET {HOST_PATH} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-Banto-Client: banto\r\nAccept: application/json\r\nUser-Agent: chronogazer-shell-probe\r\nConnection: close\r\n\r\n"
    );
    stream
        .set_write_timeout(Some(remaining(deadline).ok_or_else(timed_out)?))
        .map_err(io)?;
    stream.write_all(request.as_bytes()).map_err(io)?;

    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        // 1 回の read ごとに残りの時間だけ待つ（全体の上限。少しずつ送り続ける
        // 相手でも打ち切る）。
        let left = remaining(deadline).ok_or_else(timed_out)?;
        stream.set_read_timeout(Some(left)).map_err(io)?;
        match stream.read(&mut buf) {
            Ok(0) => return Ok(raw),
            Ok(n) => {
                raw.extend_from_slice(&buf[..n]);
                if raw.len() > MAX_RESPONSE_BYTES {
                    return Err(FetchError::TooLarge);
                }
            }
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Err(timed_out());
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(io(err)),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum HttpParse {
    Response {
        status: u16,
        body: Vec<u8>,
    },
    /// HTTP/1.x の応答の形ではない。
    NotHttp,
    /// HTTP の応答だが、本文が宣言より短いまま閉じられた。
    Truncated,
}

/// 接続が閉じるまでに読んだ全体を、状態コードと本文に分ける。
fn parse_http_response(raw: &[u8]) -> HttpParse {
    let Some(head_end) = raw.windows(4).position(|w| w == b"\r\n\r\n") else {
        return if raw.starts_with(b"HTTP/1.") {
            HttpParse::Truncated
        } else {
            HttpParse::NotHttp
        };
    };
    let Ok(head) = std::str::from_utf8(&raw[..head_end]) else {
        return HttpParse::NotHttp;
    };
    let rest = &raw[head_end + 4..];
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let mut parts = status_line.split(' ');
    let (Some(version), Some(code)) = (parts.next(), parts.next()) else {
        return HttpParse::NotHttp;
    };
    if !version.starts_with("HTTP/1.") || code.len() != 3 {
        return HttpParse::NotHttp;
    }
    let Ok(status) = code.parse::<u16>() else {
        return HttpParse::NotHttp;
    };
    let mut content_length = None;
    let mut chunked = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            match value.parse::<usize>() {
                Ok(len) => content_length = Some(len),
                Err(_) => return HttpParse::NotHttp,
            }
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            chunked = value.to_ascii_lowercase().contains("chunked");
        }
    }
    let body = if chunked {
        match decode_chunked(rest) {
            Some(body) => body,
            None => return HttpParse::Truncated,
        }
    } else if let Some(len) = content_length {
        if rest.len() < len {
            return HttpParse::Truncated;
        }
        rest[..len].to_vec()
    } else {
        // `Connection: close` で長さの無い応答は、閉じるまでが本文。
        rest.to_vec()
    };
    HttpParse::Response { status, body }
}

/// chunked の本文をつなぐ。最後の 0 の chunk まで揃っていなければ `None`。
fn decode_chunked(mut rest: &[u8]) -> Option<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let line_end = rest.windows(2).position(|w| w == b"\r\n")?;
        let size_text = std::str::from_utf8(&rest[..line_end]).ok()?;
        let size_text = size_text.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_text, 16).ok()?;
        rest = &rest[line_end + 2..];
        if size == 0 {
            return Some(body);
        }
        if rest.len() < size + 2 {
            return None;
        }
        body.extend_from_slice(&rest[..size]);
        rest = &rest[size + 2..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoint::write_endpoint;
    use crate::host_info::{host_router, HostInfo};
    use std::net::TcpListener;

    const SCOPE: &str = "0123456789abcdef";

    fn endpoint(port: u16, scope: &str) -> EndpointInfo {
        EndpointInfo::for_current_process(port, scope, "service")
    }

    fn body(product: &str, kind: &str, scope: &str) -> Vec<u8> {
        serde_json::json!({
            "product": product,
            "hostKind": kind,
            "version": "0.1.0",
            "scope": scope,
        })
        .to_string()
        .into_bytes()
    }

    fn classify(status: u16, body: &[u8], own: Option<&str>) -> ServiceHealth {
        classify_host_response(
            status,
            body,
            &endpoint(8721, SCOPE),
            ChronoGazerHost::Service,
            own,
        )
    }

    // --- 分類（純関数）の全分岐 -------------------------------------------------

    #[test]
    fn healthy_only_when_product_kind_and_both_scopes_match() {
        let healthy = ServiceHealth::Healthy {
            port: 8721,
            version: "0.1.0".to_string(),
        };
        assert_eq!(
            classify(200, &body("chronogazer", "service", SCOPE), None),
            healthy
        );
        assert_eq!(
            classify(200, &body("chronogazer", "service", SCOPE), Some(SCOPE)),
            healthy
        );
    }

    #[test]
    fn non_200_non_json_and_other_products_are_not_chronogazer() {
        let not = ServiceHealth::NotChronoGazer { port: 8721 };
        let good = body("chronogazer", "service", SCOPE);
        assert_eq!(classify(403, &good, None), not);
        assert_eq!(classify(404, &good, None), not);
        assert_eq!(classify(200, b"<html>hello</html>", None), not);
        assert_eq!(classify(200, b"", None), not);
        assert_eq!(
            classify(200, &body("banto-hub", "service", SCOPE), None),
            not
        );
        // 欄が無い・型が違う。
        assert_eq!(
            classify(
                200,
                br#"{"product":"chronogazer","scope":"0123456789abcdef"}"#,
                None
            ),
            not
        );
        assert_eq!(
            classify(
                200,
                br#"{"product":"chronogazer","hostKind":"service"}"#,
                None
            ),
            not
        );
        assert_eq!(
            classify(
                200,
                br#"{"product":"chronogazer","hostKind":1,"scope":"x"}"#,
                None
            ),
            not
        );
        assert_eq!(classify(200, b"[1,2]", None), not);
    }

    #[test]
    fn other_host_kinds_are_reported_with_the_kind() {
        for kind in ["desktop", "serve", "server"] {
            assert_eq!(
                classify(200, &body("chronogazer", kind, SCOPE), Some(SCOPE)),
                ServiceHealth::WrongHostKind {
                    port: 8721,
                    kind: kind.to_string(),
                }
            );
        }
    }

    #[test]
    fn scope_must_match_the_endpoint_and_the_own_db() {
        let wrong = ServiceHealth::WrongScope { port: 8721 };
        // 応答が endpoint と食い違う（pid の再利用・別の DB のエンジン）。
        assert_eq!(
            classify(
                200,
                &body("chronogazer", "service", "ffffffffffffffff"),
                None
            ),
            wrong
        );
        // endpoint と応答は一致するが、自分の DB とは別。
        assert_eq!(
            classify(
                200,
                &body("chronogazer", "service", SCOPE),
                Some("ffffffffffffffff")
            ),
            wrong
        );
    }

    #[test]
    fn expected_host_kind_is_a_parameter_for_the_debug_hook() {
        assert_eq!(
            classify_host_response(
                200,
                &body("chronogazer", "server", SCOPE),
                &endpoint(8721, SCOPE),
                ChronoGazerHost::Server,
                Some(SCOPE),
            ),
            ServiceHealth::Healthy {
                port: 8721,
                version: "0.1.0".to_string()
            }
        );
    }

    // --- HTTP の読み方 ----------------------------------------------------------

    #[test]
    fn parses_content_length_chunked_and_close_delimited_bodies() {
        assert_eq!(
            parse_http_response(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}extra"),
            HttpParse::Response {
                status: 200,
                body: b"{}".to_vec()
            }
        );
        assert_eq!(
            parse_http_response(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\n{\"a\r\n4;x=y\r\n\":1}\r\n0\r\n\r\n"
            ),
            HttpParse::Response {
                status: 200,
                body: b"{\"a\":1}".to_vec()
            }
        );
        assert_eq!(
            parse_http_response(b"HTTP/1.0 403 Forbidden\r\n\r\nno"),
            HttpParse::Response {
                status: 403,
                body: b"no".to_vec()
            }
        );
    }

    #[test]
    fn rejects_non_http_and_detects_truncation() {
        assert_eq!(
            parse_http_response(b"SSH-2.0-OpenSSH_9.0\r\n"),
            HttpParse::NotHttp
        );
        assert_eq!(parse_http_response(b""), HttpParse::NotHttp);
        assert_eq!(
            parse_http_response(b"FOO/1.1 200 OK\r\n\r\n{}"),
            HttpParse::NotHttp
        );
        assert_eq!(
            parse_http_response(b"HTTP/1.1 2000 OK\r\n\r\n{}"),
            HttpParse::NotHttp
        );
        assert_eq!(
            parse_http_response(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n{}"),
            HttpParse::Truncated
        );
        assert_eq!(
            parse_http_response(b"HTTP/1.1 200 OK\r\nContent-Le"),
            HttpParse::Truncated
        );
        assert_eq!(
            parse_http_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\n{}"),
            HttpParse::Truncated
        );
    }

    // --- 本物の listener 越し ---------------------------------------------------

    /// 本物の `host_router` を 127.0.0.1 の空きポートで動かす（テストの間だけ）。
    async fn serve_host_router(host: ChronoGazerHost, scope: &str) -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let router = host_router(HostInfo::new(host, scope));
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        port
    }

    async fn probe_blocking(
        endpoint: EndpointInfo,
        expected: ChronoGazerHost,
        own: Option<String>,
    ) -> ServiceHealth {
        tokio::task::spawn_blocking(move || {
            probe_endpoint(&endpoint, expected, own.as_deref(), PROBE_TIMEOUT)
        })
        .await
        .unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn round_trip_against_the_real_host_router() {
        let port = serve_host_router(ChronoGazerHost::Service, SCOPE).await;
        assert_eq!(
            probe_blocking(
                endpoint(port, SCOPE),
                ChronoGazerHost::Service,
                Some(SCOPE.into())
            )
            .await,
            ServiceHealth::Healthy {
                port,
                version: env!("CARGO_PKG_VERSION").to_string(),
            }
        );
        assert_eq!(
            probe_blocking(
                endpoint(port, SCOPE),
                ChronoGazerHost::Service,
                Some("ffffffffffffffff".into())
            )
            .await,
            ServiceHealth::WrongScope { port }
        );

        let server_port = serve_host_router(ChronoGazerHost::Server, SCOPE).await;
        assert_eq!(
            probe_blocking(endpoint(server_port, SCOPE), ChronoGazerHost::Service, None).await,
            ServiceHealth::WrongHostKind {
                port: server_port,
                kind: "server".to_string(),
            }
        );
    }

    /// endpoint.json から自分の DB の scope まで、ルートの配置どおりに通す。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn probe_service_reads_the_endpoint_file_and_checks_the_own_db_scope() {
        let dir = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(dir.path().join("root"));

        // ルートが無い: endpoint.json も無い。probe はルートを作らない。
        let missing = {
            let layout = layout.clone();
            tokio::task::spawn_blocking(move || probe_service(&layout))
                .await
                .unwrap()
        };
        assert_eq!(missing, ServiceHealth::EndpointMissing);
        assert!(!layout.root().exists());
        assert_eq!(own_db_scope(&layout).unwrap(), None);
        assert!(!layout.root().exists());

        std::fs::create_dir_all(layout.root()).unwrap();
        let own = own_db_scope(&layout)
            .unwrap()
            .expect("ルートがあれば求まる");
        assert_eq!(own.len(), 16);

        // 自分の DB のサービス。
        let port = serve_host_router(ChronoGazerHost::Service, &own).await;
        write_endpoint(&layout.endpoint_file(), &endpoint(port, &own)).unwrap();
        let health = {
            let layout = layout.clone();
            tokio::task::spawn_blocking(move || probe_service(&layout))
                .await
                .unwrap()
        };
        assert!(
            matches!(health, ServiceHealth::Healthy { port: p, .. } if p == port),
            "{health:?}"
        );

        // endpoint と応答は一致するが、別の DB のサービス。
        let other = serve_host_router(ChronoGazerHost::Service, SCOPE).await;
        write_endpoint(&layout.endpoint_file(), &endpoint(other, SCOPE)).unwrap();
        let health = {
            let layout = layout.clone();
            tokio::task::spawn_blocking(move || probe_service(&layout))
                .await
                .unwrap()
        };
        assert_eq!(health, ServiceHealth::WrongScope { port: other });
    }

    fn free_port() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    }

    #[test]
    fn nothing_listening_is_unreachable() {
        let port = free_port();
        assert_eq!(
            probe_endpoint(
                &endpoint(port, SCOPE),
                ChronoGazerHost::Service,
                None,
                PROBE_TIMEOUT
            ),
            ServiceHealth::Unreachable { port }
        );
    }

    /// 要求の頭（空行まで）を読み捨てる。読まずに閉じると Windows は RST を送り、
    /// 相手の応答を読む前に probe の側の read が失敗しうるため。
    fn read_request_head(stream: &mut TcpStream) {
        let mut seen = Vec::new();
        let mut buf = [0u8; 512];
        while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
            match stream.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => seen.extend_from_slice(&buf[..n]),
            }
        }
    }

    /// 相手のふるまいを決めた listener（1 接続だけ受ける）。
    fn spawn_listener(behaviour: impl FnOnce(TcpStream) + Send + 'static) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                behaviour(stream);
            }
        });
        port
    }

    #[test]
    fn a_non_http_listener_is_not_chronogazer() {
        let port = spawn_listener(|mut stream| {
            read_request_head(&mut stream);
            let _ = stream.write_all(b"SSH-2.0-OpenSSH_9.0\r\n");
        });
        assert_eq!(
            probe_endpoint(
                &endpoint(port, SCOPE),
                ChronoGazerHost::Service,
                None,
                PROBE_TIMEOUT
            ),
            ServiceHealth::NotChronoGazer { port }
        );
    }

    /// 「返ってこない」相手: 黙ったまま・少しずつ送り続ける、のどちらも全体の上限で
    /// 打ち切る。
    #[test]
    fn silent_or_dripping_listeners_are_cut_off_by_the_overall_deadline() {
        let timeout = Duration::from_millis(300);
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let silent = spawn_listener(move |_stream| {
            let _ = release_rx.recv_timeout(Duration::from_secs(10));
        });
        let started = Instant::now();
        let health = probe_endpoint(
            &endpoint(silent, SCOPE),
            ChronoGazerHost::Service,
            None,
            timeout,
        );
        assert!(
            matches!(health, ServiceHealth::ProbeFailed(_)),
            "{health:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        let _ = release_tx.send(());

        let dripping = spawn_listener(|mut stream| {
            // 小さな書き込みを溜めずにすぐ送る（溜まると間が空いて、1 回の read の
            // 時間切れで終わってしまい、全体の上限を確かめられない）。なお、通信を
            // 検査するソフトが入った PC では loopback の 2 つ目以降の送信が閉じるまで
            // 溜められることがあり、そこではこの相手も「黙った相手」と同じになる
            // （どちらでも上限で終わる、は確かめられる）。
            let _ = stream.set_nodelay(true);
            read_request_head(&mut stream);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 20000\r\n\r\n");
            for _ in 0..200 {
                if stream.write_all(&[b' '; 100]).is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        let started = Instant::now();
        let health = probe_endpoint(
            &endpoint(dripping, SCOPE),
            ChronoGazerHost::Service,
            None,
            timeout,
        );
        assert!(
            matches!(health, ServiceHealth::ProbeFailed(_)),
            "{health:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn an_oversized_response_is_not_chronogazer() {
        let port = spawn_listener(|mut stream| {
            read_request_head(&mut stream);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n");
            let _ = stream.write_all(&vec![b'a'; MAX_RESPONSE_BYTES + 10]);
        });
        assert_eq!(
            probe_endpoint(
                &endpoint(port, SCOPE),
                ChronoGazerHost::Service,
                None,
                PROBE_TIMEOUT
            ),
            ServiceHealth::NotChronoGazer { port }
        );
    }
}
