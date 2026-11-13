//! #644 PR3b: ChronoGazer が Hub と話すときの TLS の決まり（[`HubTlsPolicy`]）。
//!
//! # 何を塞ぐか
//!
//! 同じ PC の Hub が止まっている間に、その loopback のポートを別のプロセスが先に取ると、
//! 平文の http ではその相手に試運転の発行の要求や API キーが届いてしまう。そこで
//! **接続先が loopback のときは、Hub のサービスが `%ProgramData%\BantoHub\tls\` に置いた
//! 証明書を pin した https でだけ話す**（2026-11-13 オーナー決定）。
//!
//! | 接続先                                   | 本番（[`HubTlsPolicy::production`]）                     |
//! | ---------------------------------------- | -------------------------------------------------------- |
//! | loopback（127.0.0.0/8・`::1`・`localhost`）の http | 断る（[`LOOPBACK_HTTP_REFUSED`] の案内。何も送らない）   |
//! | loopback の https                        | 証明書を確かめて読み、その 1 枚だけを信じる              |
//! | それ以外（LAN）の http                   | 今までどおり pin なし（#644 の範囲外）                   |
//! | それ以外の https                         | 今までどおり断る（pin が無いので bootstrap が `InvalidEndpoint`） |
//!
//! IPv6 の loopback（`::1`、IPv4-mapped の `::ffff:127.x.x.x`）も loopback として扱う（Hub は `::1` では
//! 待ち受けないので https は繋がらないだけ）。それ以外の IPv6 は LAN 扱い。
//!
//! # いつ読むか
//!
//! `banto_hub_bootstrap::HubTlsSource` として 1 回の操作ごとに 1 回聞かれる。証明書は
//! そのたびに置き場を確かめて読み直すので、管理者が `prepare-hub-tls --renew` で作り直しても
//! ChronoGazer の再起動は要らない（購読は [`HubTlsPolicy::pin_marker`] が変わったときに
//! 張り直す。`crate::hub` の `reconcile_generation`）。
//!
//! # 保存済みの loopback の http
//!
//! 以前の版で保存した `http://127.0.0.1:8722` などは**書き換えない**（オーナー決定）。
//! 状態の確認・購読の開始はこの決まりで断られ、画面に [`LOOPBACK_HTTP_REFUSED`] の案内が
//! 出る。https は port も違うので keyring の口座が別になり、接続し直すと新しいキーが
//! 発行される（それでよい）。
//!
//! # テストと開発用
//!
//! [`HubTlsPolicy::plain_loopback_http_for_dev`] は loopback の http を pin なしで受ける。
//! 模擬 Hub（127.0.0.1 の http）を相手にするテスト・実機確認のハーネス・開発用の
//! `banto-serve`（配布しない。E2E が `BANTO_HUB_LOOPBACK_HTTP=1` で選ぶ）だけが使う。
//! 製品の host（デスクトップ・`chronogazer-server`）は [`HubTlsPolicy::production`] を
//! 固定で渡し、設定や環境変数で切り替える口を持たない。

use std::hash::{DefaultHasher, Hash, Hasher};
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, Mutex, PoisonError};

use banto_hub_bootstrap::{
    Error as BootstrapError, ErrorKind as BootstrapErrorKind, HubTlsSource, Url,
};
use banto_tagclient::PinnedHubTls;

/// 同じ PC の Hub（サービス）の接続先。画面の例と案内に使う。
pub const LOOPBACK_HUB_ENDPOINT: &str = "https://127.0.0.1:8723";

/// 保存済み・入力された loopback の http を断るときの案内（キーや秘密は含まない）。
pub const LOOPBACK_HTTP_REFUSED: &str = "同じ PC の Hub へは http では接続しません（API キーを守るため）。https://127.0.0.1:8723 で接続し直してください（Hub をサービスで動かし、管理者で prepare-hub-tls を済ませてあること）。";

/// Hub の証明書（PEM）を読む口。本番は置き場を確かめてから読む
/// （`banto_hub_tls_trust::load_pinned_hub_cert`）。テストは差し替える。
///
/// `Err` は画面にそのまま出す理由（鍵の中身は含めない）。
pub(crate) trait HubCertReader: Send + Sync {
    fn read_cert_pem(&self) -> Result<Vec<u8>, String>;
}

/// 本番の読み方: `%ProgramData%\BantoHub\tls\` の所有者・DACL を確かめ、証明書だけを
/// 読み取り専用で開く（一般利用者で動く）。
struct SystemHubCert;

impl HubCertReader for SystemHubCert {
    #[cfg(windows)]
    fn read_cert_pem(&self) -> Result<Vec<u8>, String> {
        banto_hub_tls_trust::load_pinned_hub_cert(&banto_hub_tls_trust::win::WindowsTlsTrustOps)
            .map(|pin| pin.cert_pem)
            .map_err(|found| {
                format!(
                    "同じ PC の Hub の証明書を使えないため接続しません: {}。{}",
                    found.reason,
                    remedy_text(&found.remedy)
                )
            })
    }

    #[cfg(not(windows))]
    fn read_cert_pem(&self) -> Result<Vec<u8>, String> {
        Err("この OS では同じ PC の Hub の証明書を読めないため、https の loopback には接続できません（Hub のサービスは Windows 専用です）".to_owned())
    }
}

/// 置き場の確かめの失敗の対処（文言は ChronoGazer 向け。ChronoGazer は毎回読み直すので、
/// pin を書き換える手順は要らない）。
#[cfg(windows)]
fn remedy_text(remedy: &banto_hub_tls_trust::Remedy) -> String {
    use banto_hub_tls_trust::Remedy;
    const PREPARE: &str = "banto-hub-elev.exe prepare-hub-tls";
    match remedy {
        Remedy::Retry => "原因を取り除いてからやり直してください。続くときは、管理者の PowerShell で banto-hub-elev.exe prepare-hub-tls を実行して置き場を確かめてください".to_owned(),
        Remedy::Prepare => format!(
            "Hub をサービスで動かし、管理者の PowerShell で {PREPARE} を実行してから、Hub のサービスを再起動してください"
        ),
        Remedy::FixRoot => "Hub の置き場の所有者を Administrators にし、SYSTEM・Administrators 以外への書き込み・削除・ACL と所有者の変更の許可を外してください".to_owned(),
        Remedy::RecreateDir(tls_path) => format!(
            "{} の中身を確かめてこのフォルダを消し、管理者の PowerShell で {PREPARE} を実行してから、Hub のサービスを再起動してください",
            tls_path.display()
        ),
        Remedy::Renew => format!(
            "管理者の PowerShell で {PREPARE} --renew を実行して作り直し、Hub のサービスを再起動してください"
        ),
    }
}

#[derive(Clone)]
enum Loopback {
    /// 本番: loopback は pin した https だけ。
    Pinned(Arc<dyn HubCertReader>),
    /// テスト・開発用: loopback の http を pin なしで受ける（https の loopback は pin が
    /// 無いので bootstrap の規則で断られる）。
    PlainHttp,
}

/// ChronoGazer の Hub 接続の TLS の決まり（モジュール doc）。`Clone` は同じ決まりと同じ
/// [`pin_marker`](Self::pin_marker) を共有する。
#[derive(Clone)]
pub struct HubTlsPolicy {
    loopback: Loopback,
    /// 直近の操作で使った pin の目印（証明書の PEM のハッシュ）。pin を使わなかった
    /// （http・断った）ときは `None`。
    marker: Arc<Mutex<Option<u64>>>,
}

impl HubTlsPolicy {
    /// 製品の決まり: loopback は `%ProgramData%\BantoHub\tls\` の証明書を pin した https
    /// だけ、それ以外は今までどおり。
    pub fn production() -> Self {
        Self::with_reader(Arc::new(SystemHubCert))
    }

    /// **テスト・開発用**（モジュール doc「テストと開発用」）: loopback の http も pin なしで
    /// 受ける。製品の host（デスクトップ・`chronogazer-server`）から呼ばない。
    pub fn plain_loopback_http_for_dev() -> Self {
        Self {
            loopback: Loopback::PlainHttp,
            marker: Arc::default(),
        }
    }

    /// 本番と同じ決まりで、証明書の読み方だけを差し替える（テスト用）。
    pub(crate) fn with_reader(reader: Arc<dyn HubCertReader>) -> Self {
        Self {
            loopback: Loopback::Pinned(reader),
            marker: Arc::default(),
        }
    }

    /// 直近の操作で使った pin の目印。購読の世代の同一性に混ぜ、証明書が作り直されたら
    /// 張り直す（`crate::hub` の `reconcile_generation`）。
    pub(crate) fn pin_marker(&self) -> Option<u64> {
        *self.marker.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn remember(&self, marker: Option<u64>) {
        *self.marker.lock().unwrap_or_else(PoisonError::into_inner) = marker;
    }

    /// 判定の本体（純関数に近い形。証明書を読むのは loopback の https のときだけ）。
    fn decide(&self, base: &Url) -> Result<Option<(PinnedHubTls, u64)>, BootstrapError> {
        if !is_loopback(base) {
            // LAN: http は pin なし（範囲外）。https は pin が無いので bootstrap が断る。
            return Ok(None);
        }
        let reader = match &self.loopback {
            Loopback::PlainHttp => return Ok(None),
            Loopback::Pinned(reader) => reader,
        };
        if base.scheme() != "https" {
            return Err(refused(LOOPBACK_HTTP_REFUSED));
        }
        let pem = reader.read_cert_pem().map_err(refused)?;
        let unreadable = || {
            refused(
                "同じ PC の Hub の証明書（PEM）を pin にできなかったため接続しません。管理者の PowerShell で banto-hub-elev.exe prepare-hub-tls --renew を実行して作り直し、Hub のサービスを再起動してください",
            )
        };
        let text = String::from_utf8(pem).map_err(|_| unreadable())?;
        let pins = PinnedHubTls::from_pem(std::slice::from_ref(&text)).map_err(|_| unreadable())?;
        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        Ok(Some((pins, hasher.finish())))
    }
}

impl HubTlsSource for HubTlsPolicy {
    fn pins_for(&self, base: &Url) -> banto_hub_bootstrap::Result<Option<PinnedHubTls>> {
        match self.decide(base) {
            Ok(Some((pins, marker))) => {
                self.remember(Some(marker));
                Ok(Some(pins))
            }
            Ok(None) => {
                self.remember(None);
                Ok(None)
            }
            Err(error) => {
                self.remember(None);
                Err(error)
            }
        }
    }
}

fn refused(reason: impl Into<String>) -> BootstrapError {
    BootstrapError::with_detail(BootstrapErrorKind::HubTls, reason)
}

/// 接続先が loopback か（127.0.0.0/8、`::1`、IPv4-mapped の `::ffff:127.x.x.x`、
/// `localhost`・`*.localhost`、末尾の `.` は無視）。`Url` は `127.1` のような書き方も
/// `127.0.0.1` に正規化してから渡ってくる。
fn is_loopback(base: &Url) -> bool {
    let Some(host) = base.host_str() else {
        return false;
    };
    // IPv6 は `host_str` が角括弧つきで返す（`url::Host::Ipv6` と同じ判定になる）。
    if let Some(inner) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        return inner.parse::<Ipv6Addr>().is_ok_and(|ip| {
            ip.is_loopback() || ip.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        });
    }
    if let Ok(ip) = host.parse::<Ipv4Addr>() {
        return ip.is_loopback();
    }
    let name = host.trim_end_matches('.').to_ascii_lowercase();
    name == "localhost" || name.ends_with(".localhost")
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// 読んだ回数を数え、決まった答えを返す証明書の読み口。
    struct FakeCert {
        answer: Result<Vec<u8>, String>,
        reads: AtomicUsize,
    }

    impl FakeCert {
        fn new(answer: Result<Vec<u8>, String>) -> Arc<Self> {
            Arc::new(Self {
                answer,
                reads: AtomicUsize::new(0),
            })
        }

        fn reads(&self) -> usize {
            self.reads.load(Ordering::SeqCst)
        }
    }

    impl HubCertReader for FakeCert {
        fn read_cert_pem(&self) -> Result<Vec<u8>, String> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            self.answer.clone()
        }
    }

    fn cert_pem() -> String {
        rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_owned()])
            .unwrap()
            .cert
            .pem()
    }

    fn url(raw: &str) -> Url {
        Url::parse(raw).unwrap()
    }

    fn production_with(cert: &Arc<FakeCert>) -> HubTlsPolicy {
        HubTlsPolicy::with_reader(Arc::clone(cert) as Arc<dyn HubCertReader>)
    }

    fn refusal_of(
        result: banto_hub_bootstrap::Result<Option<PinnedHubTls>>,
    ) -> (BootstrapErrorKind, String) {
        let error = result.expect_err("refused");
        (error.kind(), error.detail().unwrap_or_default().to_owned())
    }

    #[test]
    fn loopback_http_is_refused_with_the_guidance_and_reads_nothing() {
        let cert = FakeCert::new(Ok(cert_pem().into_bytes()));
        let policy = production_with(&cert);
        for raw in [
            "http://127.0.0.1:8722/",
            "http://127.0.0.2:8722/",
            "http://127.1:8722/",
            "http://localhost:8722/",
            "http://LOCALHOST./",
            "http://hub.localhost:8722/",
            "http://[::1]:8722/",
            "http://[::ffff:127.0.0.1]:8722/",
        ] {
            assert_eq!(
                refusal_of(policy.pins_for(&url(raw))),
                (BootstrapErrorKind::HubTls, LOOPBACK_HTTP_REFUSED.to_owned()),
                "{raw}"
            );
        }
        assert_eq!(
            cert.reads(),
            0,
            "http is refused without reading the certificate"
        );
        assert_eq!(policy.pin_marker(), None);
    }

    #[test]
    fn loopback_https_reads_the_certificate_every_time_and_pins_it() {
        let cert = FakeCert::new(Ok(cert_pem().into_bytes()));
        let policy = production_with(&cert);

        assert!(policy
            .pins_for(&url("https://127.0.0.1:8723/"))
            .unwrap()
            .is_some());
        assert_eq!(cert.reads(), 1);
        let marker = policy.pin_marker();
        assert!(marker.is_some());
        assert!(policy
            .pins_for(&url("https://localhost:8723/"))
            .unwrap()
            .is_some());
        assert_eq!(cert.reads(), 2, "read again on every operation (renew)");
        assert_eq!(policy.pin_marker(), marker, "the same certificate");
    }

    #[test]
    fn a_renewed_certificate_changes_the_marker() {
        let first = production_with(&FakeCert::new(Ok(cert_pem().into_bytes())));
        let second = production_with(&FakeCert::new(Ok(cert_pem().into_bytes())));
        let base = url("https://127.0.0.1:8723/");
        first.pins_for(&base).unwrap();
        second.pins_for(&base).unwrap();
        assert_ne!(first.pin_marker(), second.pin_marker());
    }

    #[test]
    fn an_unreadable_certificate_refuses_with_its_reason() {
        let cert = FakeCert::new(Err("置き場がありません".to_owned()));
        let policy = production_with(&cert);
        assert_eq!(
            refusal_of(policy.pins_for(&url("https://127.0.0.1:8723/"))),
            (BootstrapErrorKind::HubTls, "置き場がありません".to_owned())
        );

        // PEM でないもの・秘密鍵入りは pin にしない。
        for broken in [b"not a pem".to_vec(), vec![0xff, 0xfe]] {
            let policy = production_with(&FakeCert::new(Ok(broken)));
            let (kind, detail) = refusal_of(policy.pins_for(&url("https://127.0.0.1:8723/")));
            assert_eq!(kind, BootstrapErrorKind::HubTls);
            assert!(detail.contains("prepare-hub-tls --renew"), "{detail}");
        }
    }

    #[test]
    fn the_lan_keeps_todays_rules_and_reads_nothing() {
        let cert = FakeCert::new(Ok(cert_pem().into_bytes()));
        let policy = production_with(&cert);
        // http: pin なしで受ける。https: pin なし（= bootstrap の規則で InvalidEndpoint）。
        for raw in [
            "http://192.168.11.5:8722/",
            "https://192.168.11.5:8723/",
            "http://hub.example:8722/",
            "http://[fe80::1]:8722/",
            "http://[2001:db8::1]:8722/",
        ] {
            assert!(policy.pins_for(&url(raw)).unwrap().is_none(), "{raw}");
        }
        assert_eq!(cert.reads(), 0);
    }

    #[test]
    fn the_dev_policy_takes_loopback_http_without_a_pin() {
        let policy = HubTlsPolicy::plain_loopback_http_for_dev();
        for raw in ["http://127.0.0.1:8722/", "https://127.0.0.1:8723/"] {
            assert!(policy.pins_for(&url(raw)).unwrap().is_none(), "{raw}");
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn other_systems_cannot_read_the_certificate() {
        let (kind, detail) =
            refusal_of(HubTlsPolicy::production().pins_for(&url("https://127.0.0.1:8723/")));
        assert_eq!(kind, BootstrapErrorKind::HubTls);
        assert!(detail.contains("この OS"), "{detail}");
    }
}
