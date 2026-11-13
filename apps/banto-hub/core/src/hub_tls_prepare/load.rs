//! Hub の Windows サービスが既定で使う loopback TLS の鍵と証明書を読む（#644 PR1b、
//! 2026-11-13 オーナー決定 A・B）。
//!
//! `banto-hub-elev prepare-hub-tls`（[`super`]）が `<ProgramData>\BantoHub\tls\` に作った
//! [`KEY_FILE`]・[`CERT_FILE`] を、**書き込み・作成を一切せずに**、置き場ごと確かめてから読む。
//! サービス（`HubHostKind::Service`）は、Hub の TLS の環境変数（`BANTO_HUB_TLS_PORT/CERT/KEY`）
//! がどれも無いときにここを呼び、読めたら 127.0.0.1:[`DEFAULT_TLS_PORT`] で TLS を待ち受ける
//! （`crate::runtime::HubConfig::default_tls`）。読めなければ（無い・片方だけ・壊れている・
//! 期限切れ・ACL が露出・ルートが保護されていない）、TLS 無しで Hub を起動し、サービスログに
//! 理由と対処を出す（[`DefaultTlsUnavailable::log_lines`]。オーナー決定 A）。
//!
//! ## 確かめの順序（[`load_default_identity`]）
//!
//! 1. ProgramData は既知のフォルダ（`SHGetKnownFolderPath`）から取る（`BANTO_HUB_ROOT`・
//!    環境変数には追従しない）。ルート `<ProgramData>\BantoHub` が絶対パスで、既知のフォルダ
//!    そのものでないこと。
//! 2. ルートを辿らずに開き、本当のパス・reparse point でないフォルダ・所有者
//!    （SYSTEM・Administrators・TrustedInstaller）・DACL（[`root_dacl_problems`](super::root_dacl_problems)。実行した管理者
//!    本人は無いので `None`）を確かめる。
//! 3. `tls\` をルートのハンドル基準で辿らずに開き、本当のパス・種類・所有者・
//!    [`exposure_beyond_users_read`](banto_safe_root::acl::exposure_beyond_users_read) を確かめる。
//! 4. 鍵・証明書を `tls\` のハンドル基準で辿らずに読み取りで開く（共有は読み取りだけ。握って
//!    いる間は誰も書き換えられない）。フォルダ・reparse point・本当の場所の食い違いは断る。
//!    両方あること（片方だけは断る）。鍵は [`exposure`]（SYSTEM・Administrators だけ）、証明書は
//!    [`exposure_beyond_users_read`](banto_safe_root::acl::exposure_beyond_users_read)。
//! 5. 確かめてから、握ったハンドルから [`MAX_PEM_BYTES`](super::MAX_PEM_BYTES) まで読み、[`inspect_identity`] で中身を
//!    確かめる。
//!
//! 手順 1〜3 と、4 のファイルの開き方・証明書の ACL・5 の読み方は、クライアント（ChronoGazer、
//! #644 PR3b）と同じコードを使うため共有 crate `banto_hub_tls_trust` にある（[`open_place`]・
//! [`open_checked_file`]・[`check_cert_exposure`]・[`read_checked`]。#644 PR3a）。ここに残るのは、
//! 鍵の確かめ（両方あること・[`exposure`]）・[`inspect_identity`]・対処の文言（[`Remedy`] から）。
//! 判定・順序・文言は移す前と同じ。
//!
//! 読んだ PEM のバイト列をそのまま TLS に渡す（[`DefaultHubTls`]。確かめた後にパスで開き直さ
//! ない）。鍵は [`Zeroizing`] で持ち、`Debug`・ログに出さない。

use std::path::{Path, PathBuf};

use banto_hub_tls_trust::{
    check_cert_exposure, open_checked_file, open_place, read_checked, Remedy, Unavailable,
    TLS_LABEL,
};
use banto_safe_root::acl::{exposure, FileSecurity};
use zeroize::Zeroizing;

use super::{
    inspect_identity, reasons_text, utc_text, HubTlsOps, IdentityInfo, TlsFile, CERT_FILE,
    KEY_FILE, RENEW_FLAG,
};

/// サービスが既定で TLS を待ち受けるポート（127.0.0.1 だけ。#644 PR3a で共有 crate へ）。
pub use banto_hub_tls_trust::DEFAULT_TLS_PORT;

/// 確かめて読んだ、既定の TLS の鍵と証明書（PEM のバイト列）とポート。
#[derive(Clone)]
pub struct DefaultHubTls {
    port: u16,
    cert_pem: Vec<u8>,
    key_pem: Zeroizing<Vec<u8>>,
    cert_path: PathBuf,
    info: IdentityInfo,
}

impl std::fmt::Debug for DefaultHubTls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DefaultHubTls")
            .field("port", &self.port)
            .field("cert_path", &self.cert_path)
            .field("info", &self.info)
            .field("key_pem", &"<redacted>")
            .finish()
    }
}

impl DefaultHubTls {
    /// PEM を [`inspect_identity`] で確かめてから持つ（`now_unix` で期限を見る）。テスト・
    /// 呼ぶ側の組み立て用で、置き場の確かめはしない（本番は [`load_default_identity`]）。
    pub fn from_checked_pem(
        port: u16,
        cert_pem: Vec<u8>,
        key_pem: impl Into<Zeroizing<Vec<u8>>>,
        cert_path: PathBuf,
        now_unix: i64,
    ) -> Result<Self, String> {
        let key_pem = key_pem.into();
        let info = inspect_identity(&cert_pem, &key_pem, now_unix)?;
        Ok(Self {
            port,
            cert_pem,
            key_pem,
            cert_path,
            info,
        })
    }

    /// 待ち受けるポート（本番は [`DEFAULT_TLS_PORT`]）。
    pub fn port(&self) -> u16 {
        self.port
    }

    /// ポートを差し替える（テスト用。本番は [`DEFAULT_TLS_PORT`] のまま）。
    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// 証明書の PEM。
    pub fn cert_pem(&self) -> &[u8] {
        &self.cert_pem
    }

    /// 証明書のパス（ログ用。読み直しには使わない）。
    pub fn cert_path(&self) -> &Path {
        &self.cert_path
    }

    /// 証明書の SHA-256 指紋と notAfter。
    pub fn info(&self) -> &IdentityInfo {
        &self.info
    }

    /// TLS の待受に渡す（証明書、鍵）。鍵を複製しないよう取り出し、後には空を残す（ログの行の
    /// ための証明書のパス・指紋・期限は残る）。
    pub(crate) fn take_pem(&mut self) -> (Vec<u8>, Zeroizing<Vec<u8>>) {
        (
            std::mem::take(&mut self.cert_pem),
            std::mem::take(&mut self.key_pem),
        )
    }

    /// 待ち受けたときのログの 1 行。
    pub fn listening_line(&self, local: std::net::SocketAddr) -> String {
        format!(
            "banto-hub: 既定の TLS を https://{local} で待ち受けています（証明書 {}、SHA-256 指紋 {}、notAfter {}）",
            self.cert_path.display(),
            self.info.fingerprint,
            utc_text(self.info.not_after_unix)
        )
    }
}

/// 既定の TLS を使えない理由と対処（鍵の中身は含めない）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultTlsUnavailable {
    /// 何が合わなかったか。
    pub reason: String,
    /// 管理者がすること。
    pub remedy: String,
}

impl DefaultTlsUnavailable {
    /// サービスログに出す行（警告として出す）。
    pub fn log_lines(&self) -> Vec<String> {
        vec![
            format!(
                "[WARN] banto-hub: 既定の TLS（https://127.0.0.1:{DEFAULT_TLS_PORT}）を待ち受けずに、TLS 無しで起動します: {}",
                self.reason
            ),
            format!("[WARN] banto-hub: 対処: {}", self.remedy),
        ]
    }
}

/// bind できなかったとき（オーナー決定 B）の、サービスログに出す行。
pub fn bind_failure_lines(port: u16, error: &str) -> Vec<String> {
    vec![
        format!(
            "[WARN] banto-hub: 既定の TLS（https://127.0.0.1:{port}）を待ち受けられないので、TLS 無しで起動します: {error}"
        ),
        format!(
            "[WARN] banto-hub: 対処: 127.0.0.1:{port} を使っている別のプロセス（netstat -ano | findstr :{port} で PID を確かめる）を止めてから、Hub のサービスを再起動してください。それまで、証明書を pin したクライアント（banto-hub-sink など）は Hub に繋がりません"
        ),
    ]
}

const ELEV: &str = "banto-hub-elev.exe prepare-hub-tls";

fn unavailable(reason: String, remedy: String) -> DefaultTlsUnavailable {
    DefaultTlsUnavailable { reason, remedy }
}

/// 無い・片方だけのときの対処。
fn prepare_remedy() -> String {
    format!("管理者の PowerShell で {ELEV} を実行してから、Hub のサービスを再起動してください")
}

/// 壊れている・期限切れ・鍵や証明書の露出のときの対処。
fn renew_remedy() -> String {
    format!(
        "管理者の PowerShell で {ELEV} {RENEW_FLAG} を実行して作り直し、この証明書を pin しているクライアント（banto-hub-sink の hub_tls_pinned_certs など）を新しい証明書に書き換えてから、Hub と Sink のサービスを再起動してください"
    )
}

/// `tls\` が露出・乗っ取りの恐れがあるときの対処。
fn recreate_dir_remedy(tls_path: &Path) -> String {
    format!(
        "{} の中身を確かめ、このフォルダを消してから、管理者の PowerShell で {ELEV} を実行し、この証明書を pin しているクライアント（banto-hub-sink の hub_tls_pinned_certs など）を新しい証明書に書き換えてから、Hub と Sink のサービスを再起動してください",
        tls_path.display()
    )
}

/// 開けない・読めないなど、一時的かもしれないときの対処。
fn retry_remedy() -> String {
    "原因を取り除いてから Hub のサービスを再起動してください。続くときは、管理者の PowerShell で banto-hub-elev.exe prepare-hub-tls を実行して置き場を確かめてください".to_string()
}

/// ルートが保護されていないときの対処。
fn fix_root_remedy() -> String {
    "所有者を Administrators にし（takeown /a）、SYSTEM・Administrators 以外への削除・子の削除・ACL と所有者の変更・書き込み・フルの許可を外してから、Hub のサービスを再起動してください".to_string()
}

/// 共有 crate の確かめの失敗を、サービスログの理由と対処にする（対処の文言は移す前と同じ）。
fn from_trust(found: Unavailable) -> DefaultTlsUnavailable {
    let remedy = match &found.remedy {
        Remedy::Retry => retry_remedy(),
        Remedy::Prepare => prepare_remedy(),
        Remedy::FixRoot => fix_root_remedy(),
        Remedy::RecreateDir(tls_path) => recreate_dir_remedy(tls_path),
        Remedy::Renew => renew_remedy(),
    };
    unavailable(found.reason, remedy)
}

/// 既定の TLS の鍵と証明書を、置き場ごと確かめてから読む（モジュール doc「確かめの順序」）。
/// 書き込み・作成はしない。
pub fn load_default_identity<O: HubTlsOps>(
    ops: &O,
) -> Result<DefaultHubTls, DefaultTlsUnavailable> {
    // --- 1〜3. ルートと置き場（共有 crate。モジュール doc「確かめの順序」） ----------
    let place = open_place(ops).map_err(from_trust)?;
    let tls_path = place.tls_path().to_path_buf();

    // --- 4. 鍵と証明書（開いて握り、ACL を確かめる） ---------------------------------
    let key_path = tls_path.join(KEY_FILE);
    let cert_path = tls_path.join(CERT_FILE);
    let key = open_checked_file(ops, &place, KEY_FILE, "秘密鍵").map_err(from_trust)?;
    let cert = open_checked_file(ops, &place, CERT_FILE, "証明書").map_err(from_trust)?;
    let (mut key, mut cert) = match (key, cert) {
        (Some(key), Some(cert)) => (key, cert),
        (None, None) => {
            return Err(unavailable(
                format!(
                    "{TLS_LABEL}（{}）に鍵（{KEY_FILE}）と証明書（{CERT_FILE}）がありません",
                    tls_path.display()
                ),
                prepare_remedy(),
            ))
        }
        (key, _) => {
            let (have, missing) = if key.is_some() {
                (&key_path, &cert_path)
            } else {
                (&cert_path, &key_path)
            };
            return Err(unavailable(
                format!(
                    "{} はありますが {} がありません（片方だけ）",
                    have.display(),
                    missing.display()
                ),
                renew_remedy(),
            ));
        }
    };
    let key_security: FileSecurity = key.security();
    let key_open = exposure(&key_security);
    if !key_open.is_empty() {
        return Err(unavailable(
            format!(
                "秘密鍵（{}）は保護されていません（{}）。ほかの利用者に読まれた・すり替えられた恐れがあります",
                key_path.display(),
                reasons_text(&key_open)
            ),
            renew_remedy(),
        ));
    }
    check_cert_exposure(&cert, &cert_path).map_err(from_trust)?;

    // --- 5. 握ったハンドルから読んで、中身を確かめる --------------------------------
    let key_pem = read_checked(&mut key, "秘密鍵", &key_path)
        .map(Zeroizing::new)
        .map_err(from_trust)?;
    let cert_pem = read_checked(&mut cert, "証明書", &cert_path).map_err(from_trust)?;
    drop((key, cert));
    let now = ops.now_unix();
    DefaultHubTls::from_checked_pem(DEFAULT_TLS_PORT, cert_pem, key_pem, cert_path, now).map_err(
        |error| {
            unavailable(
                format!(
                    "{TLS_LABEL}（{}）の鍵と証明書は使えません: {error}",
                    tls_path.display()
                ),
                renew_remedy(),
            )
        },
    )
}
