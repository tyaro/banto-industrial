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
//!    （SYSTEM・Administrators・TrustedInstaller）・DACL（[`root_dacl_problems`]。実行した管理者
//!    本人は無いので `None`）を確かめる。
//! 3. `tls\` をルートのハンドル基準で辿らずに開き、本当のパス・種類・所有者・
//!    [`exposure_beyond_users_read`] を確かめる。
//! 4. 鍵・証明書を `tls\` のハンドル基準で辿らずに読み取りで開く（共有は読み取りだけ。握って
//!    いる間は誰も書き換えられない）。フォルダ・reparse point・本当の場所の食い違いは断る。
//!    両方あること（片方だけは断る）。鍵は [`exposure`]（SYSTEM・Administrators だけ）、証明書は
//!    [`exposure_beyond_users_read`]。
//! 5. 確かめてから、握ったハンドルから [`MAX_PEM_BYTES`] まで読み、[`inspect_identity`] で中身を
//!    確かめる。
//!
//! 読んだ PEM のバイト列をそのまま TLS に渡す（[`DefaultHubTls`]。確かめた後にパスで開き直さ
//! ない）。鍵は [`Zeroizing`] で持ち、`Debug`・ログに出さない。

use std::path::{Path, PathBuf};

use banto_safe_root::acl::{exposure, exposure_beyond_users_read, FileSecurity};
use banto_safe_root::guard::{self, DataAclRefusal, DirState, MachineEnvironment};
use zeroize::Zeroizing;

use super::{
    inspect_identity, reasons_text, root_dacl_problems, utc_text, HubTlsOps, IdentityInfo, TlsDir,
    TlsFile, CERT_FILE, KEY_FILE, MAX_PEM_BYTES, RENEW_FLAG, TLS_DIR,
};
use crate::profile_grant::well_known_text;
use crate::sink_config_prepare::{real_path_text, FileOpenError, PRODUCT_DIR};

/// サービスが既定で TLS を待ち受けるポート（127.0.0.1 だけ）。
pub const DEFAULT_TLS_PORT: u16 = 8723;

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
const ROOT_LABEL: &str = "Hub のルート";
const TLS_LABEL: &str = "Hub の TLS の置き場";

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

/// 本当の場所の食い違いの理由（`prepare-*` と同じ文言から、末尾の「止めます」を除く）。
fn real_path_reason(
    label: &str,
    expected: &Path,
    real: &Path,
    refusal: guard::RealPathRefusal,
) -> String {
    real_path_text(label, expected, real, refusal)
        .trim_end_matches("。止めます")
        .to_string()
}

fn dir_refusal(label: &str, dir: &Path, refusal: &DataAclRefusal) -> String {
    let reason = match refusal {
        DataAclRefusal::ReparsePoint => {
            "ジャンクションかシンボリックリンク（reparse point）です".to_string()
        }
        DataAclRefusal::NotADirectory => "フォルダではありません".to_string(),
        DataAclRefusal::ForeignOwner(owner) => format!(
            "所有者（{owner}）が SYSTEM・Administrators ではありません（一般の利用者が先に作った恐れがあります）"
        ),
    };
    format!("{label}（{}）は{reason}", dir.display())
}

/// `tls\` の中のファイル 1 つを開いて種類と場所を確かめる（無ければ `None`。まだ読まない）。
fn open_child<O: HubTlsOps>(
    ops: &O,
    tls: &O::Dir,
    tls_path: &Path,
    tls_real: &Path,
    name: &str,
    label: &str,
) -> Result<Option<O::File>, DefaultTlsUnavailable> {
    let shown = tls_path.join(name);
    let file = match ops.open_child_file(tls, name) {
        Ok(Some(file)) => file,
        Ok(None) => return Ok(None),
        Err(FileOpenError::InUse) => {
            return Err(unavailable(
                format!(
                    "{label}（{}）は別のプロセスで書き込みに開かれています",
                    shown.display()
                ),
                retry_remedy(),
            ))
        }
        Err(FileOpenError::Other(error)) => {
            return Err(unavailable(
                format!("{label}（{}）を開けませんでした: {error}", shown.display()),
                retry_remedy(),
            ))
        }
    };
    let security = file.security();
    let problem = if security.is_dir {
        Some("フォルダです".to_string())
    } else if security.is_reparse_point {
        Some("シンボリックリンクなどの reparse point です".to_string())
    } else if !guard::paths_match(&file.final_path(), &tls_real.join(name)) {
        Some(format!(
            "本当の場所（{}）が期待と違います",
            file.final_path().display()
        ))
    } else {
        None
    };
    match problem {
        Some(problem) => Err(unavailable(
            format!("{label}（{}）は{problem}", shown.display()),
            recreate_dir_remedy(tls_path),
        )),
        None => Ok(Some(file)),
    }
}

fn read<F: TlsFile>(
    file: &mut F,
    label: &str,
    path: &Path,
) -> Result<Zeroizing<Vec<u8>>, DefaultTlsUnavailable> {
    file.read_all(MAX_PEM_BYTES)
        .map(Zeroizing::new)
        .map_err(|error| {
            unavailable(
                format!("{label}（{}）を読めませんでした: {error}", path.display()),
                retry_remedy(),
            )
        })
}

/// 既定の TLS の鍵と証明書を、置き場ごと確かめてから読む（モジュール doc「確かめの順序」）。
/// 書き込み・作成はしない。
pub fn load_default_identity<O: HubTlsOps>(
    ops: &O,
) -> Result<DefaultHubTls, DefaultTlsUnavailable> {
    // --- 1. 純粋な確かめ ------------------------------------------------------------
    let program_data = ops.machine_env().program_data().map_err(|error| {
        unavailable(
            format!("ProgramData の場所を取れませんでした: {error}"),
            retry_remedy(),
        )
    })?;
    let root_path = program_data.join(PRODUCT_DIR);
    let tls_path = root_path.join(TLS_DIR);
    if !root_path.is_absolute() {
        return Err(unavailable(
            format!(
                "{ROOT_LABEL}（{}）が絶対パスではありません",
                root_path.display()
            ),
            retry_remedy(),
        ));
    }
    let known = ops.machine_env().known_folders().map_err(|error| {
        unavailable(
            format!("既知のフォルダの場所を取れませんでした: {error}"),
            retry_remedy(),
        )
    })?;
    if let Err(refusal) = guard::reject_well_known_root(&root_path, &known) {
        return Err(unavailable(
            well_known_text(&root_path, &refusal),
            retry_remedy(),
        ));
    }

    // --- 2. ルート ------------------------------------------------------------------
    let root = match ops.pin_dir(&root_path) {
        Ok(Some(root)) => root,
        Ok(None) => {
            return Err(unavailable(
                format!("{ROOT_LABEL}（{}）がありません", root_path.display()),
                prepare_remedy(),
            ))
        }
        Err(error) => {
            return Err(unavailable(
                format!(
                    "{ROOT_LABEL}（{}）を開けませんでした: {error}",
                    root_path.display()
                ),
                retry_remedy(),
            ))
        }
    };
    let root_real = root.final_path();
    let fix_root = || {
        "所有者を Administrators にし（takeown /a）、SYSTEM・Administrators 以外への削除・子の削除・ACL と所有者の変更・書き込み・フルの許可を外してから、Hub のサービスを再起動してください".to_string()
    };
    if let Err(refusal) = guard::check_real_path(&root_real, &root_path, &known) {
        return Err(unavailable(
            real_path_reason(ROOT_LABEL, &root_path, &root_real, refusal),
            fix_root(),
        ));
    }
    if let Err(refusal) = guard::dir_acl_plan(&DirState::Present(root.info()), None, None) {
        return Err(unavailable(
            dir_refusal(ROOT_LABEL, &root_path, &refusal),
            fix_root(),
        ));
    }
    let root_security = root.security().map_err(|error| {
        unavailable(
            format!(
                "{ROOT_LABEL}（{}）の ACL を読めませんでした: {error}",
                root_path.display()
            ),
            retry_remedy(),
        )
    })?;
    let root_open = root_dacl_problems(&root_security, None);
    if !root_open.is_empty() {
        return Err(unavailable(
            format!(
                "{ROOT_LABEL}（{}）は保護されていません（{}）。一般の利用者が {TLS_DIR}\\ を消して差し替えられる恐れがあります",
                root_path.display(),
                reasons_text(&root_open)
            ),
            fix_root(),
        ));
    }

    // --- 3. 置き場 ------------------------------------------------------------------
    let tls = match ops.open_child_dir(&root, TLS_DIR) {
        Ok(Some(tls)) => tls,
        Ok(None) => {
            return Err(unavailable(
                format!("{TLS_LABEL}（{}）がありません", tls_path.display()),
                prepare_remedy(),
            ))
        }
        Err(error) => {
            return Err(unavailable(
                format!(
                    "{TLS_LABEL}（{}）を開けませんでした: {error}",
                    tls_path.display()
                ),
                retry_remedy(),
            ))
        }
    };
    let tls_real = tls.final_path();
    if let Err(refusal) = guard::check_real_path(&tls_real, &root_real.join(TLS_DIR), &known) {
        return Err(unavailable(
            real_path_reason(TLS_LABEL, &tls_path, &tls_real, refusal),
            recreate_dir_remedy(&tls_path),
        ));
    }
    if let Err(refusal) = guard::dir_acl_plan(&DirState::Present(tls.info()), None, None) {
        return Err(unavailable(
            dir_refusal(TLS_LABEL, &tls_path, &refusal),
            recreate_dir_remedy(&tls_path),
        ));
    }
    let tls_security = tls.security().map_err(|error| {
        unavailable(
            format!(
                "{TLS_LABEL}（{}）の ACL を読めませんでした: {error}",
                tls_path.display()
            ),
            retry_remedy(),
        )
    })?;
    let tls_open = exposure_beyond_users_read(&tls_security);
    if !tls_open.is_empty() {
        return Err(unavailable(
            format!(
                "{TLS_LABEL}（{}）は保護されていません（{}）。ほかの利用者が鍵・証明書を書き換えた・読んだ恐れがあります",
                tls_path.display(),
                reasons_text(&tls_open)
            ),
            recreate_dir_remedy(&tls_path),
        ));
    }

    // --- 4. 鍵と証明書（開いて握り、ACL を確かめる） ---------------------------------
    let key_path = tls_path.join(KEY_FILE);
    let cert_path = tls_path.join(CERT_FILE);
    let key = open_child(ops, &tls, &tls_path, &tls_real, KEY_FILE, "秘密鍵")?;
    let cert = open_child(ops, &tls, &tls_path, &tls_real, CERT_FILE, "証明書")?;
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
    let cert_open = exposure_beyond_users_read(&cert.security());
    if !cert_open.is_empty() {
        return Err(unavailable(
            format!(
                "証明書（{}）は保護されていません（{}）。ほかの利用者に書き換えられた恐れがあります",
                cert_path.display(),
                reasons_text(&cert_open)
            ),
            renew_remedy(),
        ));
    }

    // --- 5. 握ったハンドルから読んで、中身を確かめる --------------------------------
    let key_pem = read(&mut key, "秘密鍵", &key_path)?;
    let cert_pem = read(&mut cert, "証明書", &cert_path)?;
    drop((key, cert));
    let now = ops.now_unix();
    DefaultHubTls::from_checked_pem(DEFAULT_TLS_PORT, cert_pem.to_vec(), key_pem, cert_path, now)
        .map_err(|error| {
            unavailable(
                format!(
                    "{TLS_LABEL}（{}）の鍵と証明書は使えません: {error}",
                    tls_path.display()
                ),
                renew_remedy(),
            )
        })
}
