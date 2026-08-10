//! T16-2（docs/banto-hub-t16-design.md §3「T16-2」・
//! docs/banto-hub-t17-design.md §9「実 HTTP probe は今回入れていない」節）:
//! `crate::hub_health::HubHealthProbe` の実 HTTP 実装。
//!
//! `crate::hub_health`（T17-3）は trait・`HealthOutcome`・モックだけを持ち、
//! 「実 HTTP probe は Windows 実機・T16-2 実配線着手時に追加する」と
//! 明記していた - このモジュールがそれに応える。`host_switch`側の変更は
//! 不要（`HubHealthProbe` trait 境界だけに依存するため、モジュール doc
//! 参照どおり）。
//!
//! ## 実装方針（実装指示どおり最小依存）
//!
//! 新規クレート依存を増やさないため、`reqwest`/`ureq`ではなく
//! `std::net::TcpStream`で素朴な HTTP/1.1 リクエストを組み立てて送る
//! （ボディの JSON 解析だけ既存依存の`serde_json`を使う）。`banto-hub`の
//! `GET /api/v1/openapi.json` は認証不要・小さな JSON を返すだけなので、
//! chunked encoding や keep-alive の複雑なハンドリングは不要 - `Connection:
//! close`を送ってサーバー側に接続を閉じさせ、EOF まで読み切るだけで足りる。
//!
//! ## T16-2 第二スライス（docs/banto-hub-t16-design.md §5 既知の gap
//! 「navigate 先を`127.0.0.1`固定にしている」）で追加した`host`
//!
//! 第一スライスは常に`127.0.0.1`へ接続していた - `BANTO_BIND`をカスタム
//! した環境では、実際にサービス/デスクトップが listen しているアドレスと
//! 食い違いうる。[`HttpHubHealthProbe::with_host`]/
//! [`HttpHubHealthProbe::with_host_and_timeout`]で接続先ホストを指定
//! できるようにした - 呼び出し元（`apps/banto-hub/src-tauri`）が
//! `BANTO_BIND`を解決した文字列を渡す（全インターフェース bind は
//! loopback へ読み替え済みのものを渡す想定 - このモジュール自体は
//! 読み替えを行わない）。`new`/`with_timeout`は後方互換のため
//! 引き続き`127.0.0.1`を既定にする。
//!
//! ## 判定ロジック
//!
//! 1. `expected_port`へ TCP 接続を試みる（タイムアウト短め）。
//!    接続自体が失敗（拒否・タイムアウト）→ [`HealthOutcome::Unreachable`]。
//! 2. 接続はできたが、HTTP 応答が `200` かつ本文が openapi 文書らしい
//!    JSON（`openapi`/`paths`キーを持つ）でない → [`HealthOutcome::PortConflict`]
//!    （「port 競合」- 接続自体は誰かが受けているが banto-hub ではない、
//!    設計 §「非 JSON / 非 openapi は port 競合」）。
//! 3. openapi 応答が取れたら `info.version` を抽出する。
//! 4. profile 確認: このワークスペースの`profile.lock`
//!    （[`crate::profile_lock::ProfileOwnerInfo`]）は所有者 PID・
//!    ホスト種別だけを持ち、profile-id 自体は持たない（ファイルの置き場所
//!    そのものが profile を表す設計 - `crate::profile_paths`のモジュール doc
//!    参照）。そのため、このスライスでは「`expected_profile`が
//!    そもそも解決不能（[`crate::profile_paths::validate_profile_id`]が
//!    拒否）」を[`HealthOutcome::WrongProfileOrVersion`]として扱い、
//!    「期待 profile の`profile.lock`を開けない」場合を
//!    [`HealthOutcome::MutexOwnerUnknown`]として扱う - 実際に応答した
//!    Hub インスタンスの profile-id をワイヤ上で確認する経路（openapi
//!    応答へ profile-id を含める等）は次スライスの課題として残す
//!    （Return セクション「既知の gap」参照）。
//! 5. 上記のいずれでもなければ [`HealthOutcome::Healthy`]。

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::time::Duration;

use crate::hub_health::{HealthOutcome, HubHealthProbe, ProbeError};
use crate::profile_lock::{read_owner_info, LOCK_FILE_NAME};
use crate::profile_paths::{resolve_profile_paths, validate_profile_id};

/// probe 全体（TCP 接続 + HTTP 往復）に許すタイムアウト。desktop-plan の
/// fallback 判定は起動シーケンス上でブロッキングに評価されるため、実装
/// 指示どおり短め（1〜2秒）に抑える。
const DEFAULT_TIMEOUT: Duration = Duration::from_millis(1500);

/// 認証不要の health エンドポイント（`crate::rest`の`openapi_json`参照）。
const HEALTH_PATH: &str = "/api/v1/openapi.json";

/// 接続先ホストの既定値（後方互換 - [`HttpHubHealthProbe::new`]/
/// [`HttpHubHealthProbe::with_timeout`]が使う、モジュール doc「host」節参照）。
const DEFAULT_PROBE_HOST: &str = "127.0.0.1";

/// [`HubHealthProbe`]の実 HTTP 実装。`root`は`expected_profile`から
/// `profile.lock`の場所を組み立てるために持つ（このモジュール doc の
/// 「profile 確認」節参照）- `crate::profile_paths::resolve_profile_paths_from_env`
/// が返す`ProfilePaths::root`をそのまま渡す想定。
pub struct HttpHubHealthProbe {
    root: PathBuf,
    host: String,
    timeout: Duration,
}

impl HttpHubHealthProbe {
    /// 既定ホスト（[`DEFAULT_PROBE_HOST`]）・既定タイムアウト
    /// （[`DEFAULT_TIMEOUT`]）で構築する。
    pub fn new(root: PathBuf) -> Self {
        Self::with_host_and_timeout(root, DEFAULT_PROBE_HOST.to_string(), DEFAULT_TIMEOUT)
    }

    /// タイムアウトを指定して構築する（テスト用・将来の調整用）。ホストは
    /// 既定（[`DEFAULT_PROBE_HOST`]）のまま。
    pub fn with_timeout(root: PathBuf, timeout: Duration) -> Self {
        Self::with_host_and_timeout(root, DEFAULT_PROBE_HOST.to_string(), timeout)
    }

    /// ホストを指定し、タイムアウトは既定（[`DEFAULT_TIMEOUT`]）で構築する。
    /// `BANTO_BIND`解決結果を渡す呼び出し元（`apps/banto-hub/src-tauri`）
    /// 向けの主要コンストラクタ（モジュール doc「host」節）。
    pub fn with_host(root: PathBuf, host: String) -> Self {
        Self::with_host_and_timeout(root, host, DEFAULT_TIMEOUT)
    }

    /// ホスト・タイムアウトの両方を指定して構築する。
    pub fn with_host_and_timeout(root: PathBuf, host: String, timeout: Duration) -> Self {
        Self {
            root,
            host,
            timeout,
        }
    }
}

impl HubHealthProbe for HttpHubHealthProbe {
    fn probe(
        &self,
        expected_profile: &str,
        expected_port: u16,
    ) -> Result<HealthOutcome, ProbeError> {
        let openapi = match fetch_openapi(&self.host, expected_port, self.timeout) {
            FetchOutcome::Unreachable => return Ok(HealthOutcome::Unreachable),
            FetchOutcome::PortConflict => return Ok(HealthOutcome::PortConflict),
            FetchOutcome::Healthy(value) => value,
        };
        let version = extract_version(&openapi);

        if validate_profile_id(expected_profile).is_err() {
            // 呼び出し元が渡した expected_profile 自体が profile-id として
            // 不正 - 比較しようがないので安全側で「別 profile」扱いにする。
            return Ok(HealthOutcome::WrongProfileOrVersion);
        }
        let paths = match resolve_profile_paths(&self.root, expected_profile) {
            Ok(paths) => paths,
            Err(_) => return Ok(HealthOutcome::WrongProfileOrVersion),
        };
        let lock_path = paths.profile_dir.join(LOCK_FILE_NAME);
        match read_owner_info(&lock_path) {
            Some(_owner) => Ok(HealthOutcome::Healthy { version }),
            None => Ok(HealthOutcome::MutexOwnerUnknown),
        }
    }
}

/// [`fetch_openapi`]の内部分類 - [`HealthOutcome`]そのものではなく、
/// profile 確認より前（ネットワーク層だけ）で決まる3値に絞る。
enum FetchOutcome {
    Unreachable,
    PortConflict,
    Healthy(serde_json::Value),
}

/// `{host}:{port}`へ`GET /api/v1/openapi.json`を送り、応答を分類する。
/// `host`の名前解決自体が失敗した場合（不正なホスト名等）も接続不可と
/// 同列に扱い Unreachable とする - 呼び出し元にとっては「到達できない」点で
/// 区別する必要がないため。
///
/// `host`が複数アドレスへ解決される場合（例:`localhost`が環境によって
/// IPv6`::1`を先に返すが、そちらでは何も listen していない）に備えて、
/// 解決できた全アドレスへ順に接続を試み、最初に成功したものを使う -
/// 1つ目のアドレスだけを試して即 Unreachable 扱いにしない
/// （T16-2 第二スライスで`with_host`を追加した際に発覚した実環境依存の
/// 挙動への対応）。
fn fetch_openapi(host: &str, port: u16, timeout: Duration) -> FetchOutcome {
    let addrs: Vec<_> = match (host, port).to_socket_addrs() {
        Ok(it) => it.collect(),
        Err(_) => return FetchOutcome::Unreachable,
    };
    if addrs.is_empty() {
        return FetchOutcome::Unreachable;
    }
    let mut stream = match addrs
        .iter()
        .find_map(|addr| TcpStream::connect_timeout(addr, timeout).ok())
    {
        Some(stream) => stream,
        // どのアドレスへも接続を確立できない(拒否・タイムアウト) - 「health に
        // 到達できない」= Unreachable（このモジュール doc「判定ロジック」1.）。
        None => return FetchOutcome::Unreachable,
    };

    // 接続は確立できた時点で以降の失敗は「誰かが port を使っている」側の
    // 情報が濃い（別プロトコルで listen している等）- Unreachable ではなく
    // PortConflict へ倒す（このモジュール doc「判定ロジック」2.）。
    if stream.set_read_timeout(Some(timeout)).is_err()
        || stream.set_write_timeout(Some(timeout)).is_err()
    {
        return FetchOutcome::PortConflict;
    }

    let request = format!(
        "GET {HEALTH_PATH} HTTP/1.1\r\nHost: {host}:{port}\r\nUser-Agent: banto-hub-shell-health-probe\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return FetchOutcome::PortConflict;
    }

    let mut raw = Vec::new();
    // `Connection: close`を送っているので、サーバーが応答を書き終えたら
    // 接続を閉じるはず - `read_to_end`で EOF まで読み切る。途中でタイムアウト
    // した場合(`Err`)も、それまでに読めたバイト列があれば診断に使う。
    let _ = stream.read_to_end(&mut raw);
    if raw.is_empty() {
        return FetchOutcome::PortConflict;
    }

    parse_http_response(&raw)
}

fn parse_http_response(raw: &[u8]) -> FetchOutcome {
    let Ok(text) = std::str::from_utf8(raw) else {
        return FetchOutcome::PortConflict;
    };
    let Some(header_end) = text.find("\r\n\r\n") else {
        return FetchOutcome::PortConflict;
    };
    let (headers, rest) = text.split_at(header_end);
    let body = &rest[4..];

    let Some(status_line) = headers.lines().next() else {
        return FetchOutcome::PortConflict;
    };
    // 例: "HTTP/1.1 200 OK" - 2番目のトークンがステータスコード。
    let status_code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok());
    if status_code != Some(200) {
        return FetchOutcome::PortConflict;
    }

    match serde_json::from_str::<serde_json::Value>(body) {
        Ok(value) if looks_like_openapi(&value) => FetchOutcome::Healthy(value),
        _ => FetchOutcome::PortConflict,
    }
}

/// `crate::rest`の`ApiDoc`が生成する openapi 文書の最小限の特徴 - 完全な
/// スキーマ検証はしない（`openapi`/`paths`キーの有無だけを見る）。
fn looks_like_openapi(value: &serde_json::Value) -> bool {
    value.get("openapi").is_some() && value.get("paths").is_some()
}

fn extract_version(value: &serde_json::Value) -> String {
    value
        .get("info")
        .and_then(|info| info.get("version"))
        .and_then(|version| version.as_str())
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile_lock::{try_acquire_profile_lock, HubHostKind};
    use crate::test_support::TempDir;
    use std::net::{Ipv4Addr, TcpListener};
    use std::thread;

    const VALID_OPENAPI_BODY: &str =
        r#"{"openapi":"3.1.0","info":{"title":"banto-hub","version":"v1"},"paths":{}}"#;

    fn ok_response(body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    /// 1回だけ接続を受け、リクエストを読み捨ててから`response`を書いて
    /// 閉じる最小限の HTTP サーバー。実 axum は使わず素朴な TCP で十分
    /// （このモジュール doc「実装方針」節と同じ理由 - 依存を増やさない）。
    fn spawn_canned_http_server(response: Vec<u8>) -> u16 {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind ephemeral port");
        let port = listener.local_addr().expect("local_addr").port();
        thread::spawn(move || {
            if let Ok((mut socket, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = socket.read(&mut buf);
                let _ = socket.write_all(&response);
                let _ = socket.shutdown(std::net::Shutdown::Both);
            }
        });
        port
    }

    fn free_port_with_nothing_listening() -> u16 {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind ephemeral port");
        let port = listener.local_addr().expect("local_addr").port();
        drop(listener);
        port
    }

    #[test]
    fn looks_like_openapi_requires_both_keys() {
        let value: serde_json::Value = serde_json::from_str(VALID_OPENAPI_BODY).unwrap();
        assert!(looks_like_openapi(&value));
        assert!(!looks_like_openapi(&serde_json::json!({"paths": {}})));
        assert!(!looks_like_openapi(
            &serde_json::json!({"openapi": "3.1.0"})
        ));
    }

    #[test]
    fn extract_version_reads_info_version() {
        let value: serde_json::Value = serde_json::from_str(VALID_OPENAPI_BODY).unwrap();
        assert_eq!(extract_version(&value), "v1");
        assert_eq!(extract_version(&serde_json::json!({})), "");
    }

    #[test]
    fn parse_http_response_rejects_non_200_status() {
        let raw = b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n";
        assert!(matches!(
            parse_http_response(raw),
            FetchOutcome::PortConflict
        ));
    }

    #[test]
    fn parse_http_response_rejects_non_json_body() {
        let raw = ok_response("not json at all");
        assert!(matches!(
            parse_http_response(&raw),
            FetchOutcome::PortConflict
        ));
    }

    #[test]
    fn parse_http_response_accepts_openapi_body() {
        let raw = ok_response(VALID_OPENAPI_BODY);
        assert!(matches!(
            parse_http_response(&raw),
            FetchOutcome::Healthy(_)
        ));
    }

    #[test]
    fn probe_returns_unreachable_when_nothing_listens() {
        let port = free_port_with_nothing_listening();
        let root = TempDir::new("http-hub-health-unreachable");
        let probe =
            HttpHubHealthProbe::with_timeout(root.path().to_path_buf(), Duration::from_millis(300));
        assert_eq!(probe.probe("default", port), Ok(HealthOutcome::Unreachable));
    }

    #[test]
    fn probe_returns_port_conflict_when_response_is_not_openapi() {
        let port = spawn_canned_http_server(ok_response("not json"));
        let root = TempDir::new("http-hub-health-port-conflict");
        let probe =
            HttpHubHealthProbe::with_timeout(root.path().to_path_buf(), Duration::from_millis(500));
        assert_eq!(
            probe.probe("default", port),
            Ok(HealthOutcome::PortConflict)
        );
    }

    #[test]
    fn probe_returns_wrong_profile_for_invalid_expected_profile() {
        let port = spawn_canned_http_server(ok_response(VALID_OPENAPI_BODY));
        let root = TempDir::new("http-hub-health-invalid-profile");
        let probe =
            HttpHubHealthProbe::with_timeout(root.path().to_path_buf(), Duration::from_millis(500));
        assert_eq!(
            probe.probe("../escape", port),
            Ok(HealthOutcome::WrongProfileOrVersion)
        );
    }

    #[test]
    fn probe_returns_mutex_owner_unknown_when_lock_file_missing() {
        let port = spawn_canned_http_server(ok_response(VALID_OPENAPI_BODY));
        let root = TempDir::new("http-hub-health-no-lock");
        let probe =
            HttpHubHealthProbe::with_timeout(root.path().to_path_buf(), Duration::from_millis(500));
        assert_eq!(
            probe.probe("default", port),
            Ok(HealthOutcome::MutexOwnerUnknown)
        );
    }

    #[test]
    fn probe_returns_healthy_when_openapi_and_lock_both_present() {
        let port = spawn_canned_http_server(ok_response(VALID_OPENAPI_BODY));
        let root = TempDir::new("http-hub-health-healthy");
        let paths = resolve_profile_paths(root.path(), "default").expect("valid profile id");
        let _guard =
            try_acquire_profile_lock(&paths, HubHostKind::Service).expect("acquire lock ok");

        let probe =
            HttpHubHealthProbe::with_timeout(root.path().to_path_buf(), Duration::from_millis(500));
        assert_eq!(
            probe.probe("default", port),
            Ok(HealthOutcome::Healthy {
                version: "v1".to_string()
            })
        );
    }

    /// T16-2 第二スライス: `with_host`が指定したホストへ実際に接続することを
    /// 確認する - `127.0.0.1`固定だった第一スライスからの回帰防止
    /// （明示的に別のループバック表記`localhost`を使い、既定値の
    /// `127.0.0.1`と取り違えていないことを検証する）。
    #[test]
    fn probe_with_host_connects_to_specified_host() {
        let port = spawn_canned_http_server(ok_response(VALID_OPENAPI_BODY));
        let root = TempDir::new("http-hub-health-with-host");
        let paths = resolve_profile_paths(root.path(), "default").expect("valid profile id");
        let _guard =
            try_acquire_profile_lock(&paths, HubHostKind::Service).expect("acquire lock ok");

        let probe = HttpHubHealthProbe::with_host_and_timeout(
            root.path().to_path_buf(),
            "localhost".to_string(),
            Duration::from_millis(500),
        );
        assert_eq!(
            probe.probe("default", port),
            Ok(HealthOutcome::Healthy {
                version: "v1".to_string()
            })
        );
    }
}
