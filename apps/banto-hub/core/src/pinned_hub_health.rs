//! #644 PR4（2026-11-14 オーナー決定）: デスクトップシェルの Service モードが使う、Hub の TLS の口
//! （127.0.0.1:[`banto_hub_tls_trust::DEFAULT_TLS_PORT`]）を **pin した TLS** で確かめる
//! [`HubHealthProbe`]。
//!
//! # 何を塞ぐか
//!
//! 以前のシェルは平文の `http://127.0.0.1:8722` の openapi に profile-id があるか
//! （[`crate::http_hub_health::HttpHubHealthProbe`]）だけで「本物の Hub」と見なし、webview をそこへ
//! 移していた。profile-id は誰でも返せるので、Hub が止まっている間に 8722 を先取りしたプロセスの
//! 偽の画面へパスワード・セッショントークンが渡り得た（#644）。Service と見なすのは、Hub の
//! サービスが `%ProgramData%\BantoHub\tls\` の鍵で待ち受ける TLS の口の相手が、同じ置き場の
//! 証明書（pin）と**完全に一致する証明書**を出したときだけにする。検証器は Sink・ChronoGazer と
//! 同じ [`banto_tagclient::PinnedHubTls`]（leaf の完全一致＋rustls の通常の検証）。
//!
//! # 判定（[`PinnedHubHealthProbe::probe`]）
//!
//! 1. pin を読む（呼ぶ側が渡す [`HubCertPemSource`]。毎回読み直すので、管理者が
//!    `prepare-hub-tls --renew` で作り直してもシェルの再起動は要らない）。読めなければ TLS の口へは
//!    繋がない。
//! 2. 読めたら 127.0.0.1:8723 へ TCP で繋ぎ、TLS の握手を**最後まで済ませてから**（握手の失敗＝
//!    pin 不一致・TLS 以外の相手には 1 バイトの要求も送らない）openapi を取り、平文の probe と同じ
//!    確かめ（profile-id・`profile.lock`、[`crate::http_hub_health`]）を行う。`Healthy` ならそれで
//!    終わり（**Service と見なす唯一の経路**）。
//! 3. そうでなければ、平文の口（期待ポート、既定 8722）にも誰かいるかを、従来の
//!    [`HttpHubHealthProbe`] で見る（認証のない `GET /api/v1/openapi.json` だけで、秘密は送らない。
//!    結果は「消えたか」と診断にだけ使い、そこへ繋ぐ根拠にはしない）:
//!    - 平文の口も応答しない → [`HealthOutcome::Unreachable`]。
//!    - 平文の口に応答がある → TLS の側の理由（[`HealthOutcome::TlsPinUnavailable`]・
//!      [`HealthOutcome::TlsNotListening`]・[`HealthOutcome::TlsUntrusted`]、または TLS の口の
//!      openapi の確かめの結果）。
//!
//! # 「消えた」（`Unreachable`）を平文の口で決める理由
//!
//! `HostSwitchEngine` は Service→Desktop で「旧サービスの health が消えた（`Unreachable`）」を
//! 待ってから Desktop の起動を許す（`crate::host_switch` の不変条件 2）。Desktop が取りに行くのは
//! 平文の期待ポート（8722。Desktop の TLS 化は #1121）と profile のロックで、TLS の口は使わない。
//! そこで「消えた」は従来どおり平文の口が応答しないことで決める:
//! - 旧サービスがまだ 8722 を握っていれば `Unreachable` にならない（従来と同じ）。
//! - 旧サービスが TLS の口だけまだ握っている一瞬は、TLS の確かめが `Healthy` を返すので
//!   `Unreachable` にならない（待つ側に倒れる）。
//! - 別のプロセスが 8723 を先取りしていても、8722 が空けば `Unreachable` - Desktop への切り替えを
//!   8723 の先取りで止めない（Desktop はその口へ繋がない）。
//!
//! Desktop→Service の待ち（`Healthy` まで）は、上の 2. だけが `Healthy` を返すので、pin した TLS で
//! 確かめた後にしか完了しない。

use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use banto_tagclient::PinnedHubTls;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, ClientConnection, StreamOwned};

use crate::http_hub_health::{
    classify_openapi, exchange_openapi, FetchOutcome, HttpHubHealthProbe,
};
use crate::hub_health::{HealthOutcome, HubHealthProbe, ProbeError};

/// Hub の TLS の口のホスト（Hub は 127.0.0.1 だけで TLS を待ち受ける。`BANTO_BIND` には追従しない
/// - `crate::hub_tls_prepare::load`）。
pub const HUB_TLS_HOST: Ipv4Addr = Ipv4Addr::LOCALHOST;

/// TLS の probe 全体（TCP の接続・握手・HTTP の往復）の各段に許す時間。平文の probe と同じ。
const DEFAULT_TIMEOUT: Duration = Duration::from_millis(1500);

/// pin にする Hub の証明書（PEM）を読む口。本番は `banto_hub_tls_trust::load_pinned_hub_cert`
/// （置き場を確かめてから証明書だけを読む）、テストは差し替える。`Err` は表示用の理由と対処
/// （鍵の中身は含めない）。
pub type HubCertPemSource = Box<dyn Fn() -> Result<Vec<u8>, String> + Send + Sync>;

/// pin した TLS で Hub のサービスを確かめる [`HubHealthProbe`]（モジュール doc）。
pub struct PinnedHubHealthProbe {
    root: PathBuf,
    tls_port: u16,
    timeout: Duration,
    cert: HubCertPemSource,
    /// 平文の口（期待ポート）が消えたかを見る（モジュール doc「消えた」）。
    plain: HttpHubHealthProbe,
}

impl PinnedHubHealthProbe {
    /// `root`・`plain_host` は平文の [`HttpHubHealthProbe::with_host`] と同じ（`BANTO_BIND` を
    /// 解決した値）。TLS の口は常に [`HUB_TLS_HOST`]:`tls_port`。
    pub fn new(root: PathBuf, plain_host: String, tls_port: u16, cert: HubCertPemSource) -> Self {
        Self::with_timeout(root, plain_host, tls_port, cert, DEFAULT_TIMEOUT)
    }

    /// 時間を指定して構築する（テスト用）。
    pub fn with_timeout(
        root: PathBuf,
        plain_host: String,
        tls_port: u16,
        cert: HubCertPemSource,
        timeout: Duration,
    ) -> Self {
        Self {
            plain: HttpHubHealthProbe::with_host_and_timeout(root.clone(), plain_host, timeout),
            root,
            tls_port,
            timeout,
            cert,
        }
    }

    /// TLS の口だけを確かめる（モジュール doc「判定」1.・2.）。
    fn probe_tls(&self, expected_profile: &str) -> HealthOutcome {
        let pem = match (self.cert)() {
            Ok(pem) => pem,
            Err(reason) => return HealthOutcome::TlsPinUnavailable { reason },
        };
        let config = match pinned_config(pem) {
            Ok(config) => config,
            Err(reason) => return HealthOutcome::TlsPinUnavailable { reason },
        };
        match fetch_openapi_pinned(config, self.tls_port, self.timeout) {
            PinnedFetch::NotListening => HealthOutcome::TlsNotListening,
            PinnedFetch::Untrusted => HealthOutcome::TlsUntrusted,
            PinnedFetch::Fetched(FetchOutcome::Unreachable) => HealthOutcome::TlsNotListening,
            PinnedFetch::Fetched(FetchOutcome::PortConflict) => HealthOutcome::PortConflict,
            PinnedFetch::Fetched(FetchOutcome::Healthy(openapi)) => {
                classify_openapi(&self.root, &openapi, expected_profile)
            }
        }
    }
}

impl HubHealthProbe for PinnedHubHealthProbe {
    fn probe(
        &self,
        expected_profile: &str,
        expected_port: u16,
    ) -> Result<HealthOutcome, ProbeError> {
        let tls = self.probe_tls(expected_profile);
        if matches!(tls, HealthOutcome::Healthy { .. }) {
            return Ok(tls);
        }
        // 平文の口が消えていれば `Unreachable`（モジュール doc「消えた」）。
        match self.plain.probe(expected_profile, expected_port)? {
            HealthOutcome::Unreachable => Ok(HealthOutcome::Unreachable),
            _ => Ok(tls),
        }
    }
}

/// PEM を Sink・ChronoGazer と同じ pin の検証器にする。
fn pinned_config(pem: Vec<u8>) -> Result<Arc<ClientConfig>, String> {
    const UNUSABLE: &str = "同じ PC の Hub の証明書（PEM）を pin にできませんでした。管理者の PowerShell で banto-hub-elev.exe prepare-hub-tls --renew を実行して作り直し、Hub のサービスを再起動してください";
    let text = String::from_utf8(pem).map_err(|_| UNUSABLE.to_string())?;
    let pins =
        PinnedHubTls::from_pem(std::slice::from_ref(&text)).map_err(|_| UNUSABLE.to_string())?;
    Ok(pins.rustls_client_config())
}

/// [`fetch_openapi_pinned`] の結果。
enum PinnedFetch {
    /// TCP で繋がらない（拒否・時間切れ）。
    NotListening,
    /// 繋がったが、pin と一致する証明書で TLS の握手が済まなかった（別の証明書・TLS 以外・
    /// 握手の途中で黙った）。要求は 1 バイトも送っていない。
    Untrusted,
    /// 握手が済んだ後の往復の結果。
    Fetched(FetchOutcome),
}

fn fetch_openapi_pinned(config: Arc<ClientConfig>, port: u16, timeout: Duration) -> PinnedFetch {
    let addr = SocketAddr::new(IpAddr::V4(HUB_TLS_HOST), port);
    let Ok(mut tcp) = TcpStream::connect_timeout(&addr, timeout) else {
        return PinnedFetch::NotListening;
    };
    if tcp.set_read_timeout(Some(timeout)).is_err() || tcp.set_write_timeout(Some(timeout)).is_err()
    {
        return PinnedFetch::Untrusted;
    }
    let server_name = ServerName::from(IpAddr::V4(HUB_TLS_HOST));
    let Ok(mut conn) = ClientConnection::new(config, server_name) else {
        return PinnedFetch::Untrusted;
    };
    // 握手を先に済ませる（検証器が断ったら `Err`）。済むまで要求を書かない。
    while conn.is_handshaking() {
        if conn.complete_io(&mut tcp).is_err() {
            return PinnedFetch::Untrusted;
        }
    }
    let mut stream = StreamOwned::new(conn, tcp);
    PinnedFetch::Fetched(exchange_openapi(
        &mut stream,
        &HUB_TLS_HOST.to_string(),
        port,
    ))
}

#[cfg(test)]
mod squat_scenarios;
#[cfg(test)]
mod tests;
