//! 置き場の確かめ（crate doc「何を信じるか」の手順 1〜3）と、その後のファイルの開き方・読み方、
//! クライアントの入口 [`load_pinned_hub_cert`]。書き込み・作成はしない。

use std::path::{Path, PathBuf};

use banto_safe_root::acl::exposure_beyond_users_read;
use banto_safe_root::guard::{self, DataAclRefusal, DirState, KnownFolders, MachineEnvironment};

use crate::text::{real_path_reason, reasons_text, well_known_text};
use crate::{
    root_dacl_problems, FileOpenError, TlsDir, TlsFile, TlsTrustOps, CERT_FILE, MAX_PEM_BYTES,
    PRODUCT_DIR, ROOT_LABEL, TLS_DIR, TLS_LABEL,
};

/// 対処の種類（文言は呼ぶ側が決める。crate doc「失敗の形」）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Remedy {
    /// 一時的かもしれない（開けない・読めない・場所を取れない）。原因を取り除いてやり直す。
    Retry,
    /// まだ作られていない（ルート・`tls\`・証明書が無い）。管理者が `prepare-hub-tls` を実行する。
    Prepare,
    /// ルートが保護されていない・乗っ取りの恐れがある。管理者が所有者と ACL を直す。
    FixRoot,
    /// `tls\`（このパス）が露出・乗っ取りの恐れがある。中身を確かめて消し、作り直す。
    RecreateDir(PathBuf),
    /// 鍵・証明書が壊れている・露出している。管理者が `prepare-hub-tls --renew` で作り直す。
    Renew,
}

/// 置き場を信じられない理由（表示用の日本語の文。鍵の中身は含めない）と対処の種類。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unavailable {
    /// 何が合わなかったか。
    pub reason: String,
    /// 対処の種類。
    pub remedy: Remedy,
}

fn unavailable(reason: String, remedy: Remedy) -> Unavailable {
    Unavailable { reason, remedy }
}

/// 確かめ終えた置き場（ルートと `tls\` を辿らずに握ったまま持つ）。
pub struct CheckedPlace<D> {
    root: D,
    root_path: PathBuf,
    root_real: PathBuf,
    known: KnownFolders,
    tls: D,
    tls_path: PathBuf,
    tls_real: PathBuf,
}

impl<D> CheckedPlace<D> {
    /// 確かめたルートのハンドル（サービスの失効の記録の置き場を、このハンドル基準で開くため。
    /// #1121 PR2b）。
    pub fn root(&self) -> &D {
        &self.root
    }

    /// ルートの期待のパス（`<ProgramData>\BantoHub`。文言用）。
    pub fn root_path(&self) -> &Path {
        &self.root_path
    }

    /// ルートの本当のパス。
    pub fn root_real(&self) -> &Path {
        &self.root_real
    }

    /// 確かめに使った既知のフォルダ。
    pub fn known(&self) -> &KnownFolders {
        &self.known
    }

    /// 確かめた `tls\` のハンドル。
    pub fn tls(&self) -> &D {
        &self.tls
    }

    /// `tls\` の期待のパス（`<ProgramData>\BantoHub\tls`。文言用）。
    pub fn tls_path(&self) -> &Path {
        &self.tls_path
    }

    /// `tls\` の本当のパス。
    pub fn tls_real(&self) -> &Path {
        &self.tls_real
    }
}

pub(crate) fn dir_refusal(label: &str, dir: &Path, refusal: &DataAclRefusal) -> String {
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

/// 確かめたルート（[`open_root`] が返す）。
pub(crate) struct CheckedRoot<D> {
    pub(crate) root: D,
    pub(crate) root_path: PathBuf,
    pub(crate) root_real: PathBuf,
    pub(crate) known: KnownFolders,
}

/// ルートと `tls\` を確かめて開く（crate doc の手順 1〜3）。書き込み・作成はしない。
pub fn open_place<O: TlsTrustOps>(ops: &O) -> Result<CheckedPlace<O::Dir>, Unavailable> {
    let CheckedRoot {
        root,
        root_path,
        root_real,
        known,
    } = open_root(ops)?;
    let tls_path = root_path.join(TLS_DIR);

    // --- 3. 置き場 ------------------------------------------------------------------
    open_tls(ops, root, root_path, root_real, known, tls_path)
}

/// crate doc の手順 1・2: ProgramData とルートを確かめて開く（無ければ [`Remedy::Prepare`]）。
/// サービスの失効の記録の読み口（`crate::service_revocation`）と共有する。
pub(crate) fn open_root<O: TlsTrustOps>(ops: &O) -> Result<CheckedRoot<O::Dir>, Unavailable> {
    // --- 1. 純粋な確かめ ------------------------------------------------------------
    let program_data = ops.machine_env().program_data().map_err(|error| {
        unavailable(
            format!("ProgramData の場所を取れませんでした: {error}"),
            Remedy::Retry,
        )
    })?;
    let root_path = program_data.join(PRODUCT_DIR);
    if !root_path.is_absolute() {
        return Err(unavailable(
            format!(
                "{ROOT_LABEL}（{}）が絶対パスではありません",
                root_path.display()
            ),
            Remedy::Retry,
        ));
    }
    let known = ops.machine_env().known_folders().map_err(|error| {
        unavailable(
            format!("既知のフォルダの場所を取れませんでした: {error}"),
            Remedy::Retry,
        )
    })?;
    if let Err(refusal) = guard::reject_well_known_root(&root_path, &known) {
        return Err(unavailable(
            well_known_text(&root_path, &refusal),
            Remedy::Retry,
        ));
    }

    // --- 2. ルート ------------------------------------------------------------------
    let root = match ops.pin_dir(&root_path) {
        Ok(Some(root)) => root,
        Ok(None) => {
            return Err(unavailable(
                format!("{ROOT_LABEL}（{}）がありません", root_path.display()),
                Remedy::Prepare,
            ))
        }
        Err(error) => {
            return Err(unavailable(
                format!(
                    "{ROOT_LABEL}（{}）を開けませんでした: {error}",
                    root_path.display()
                ),
                Remedy::Retry,
            ))
        }
    };
    let root_real = root.final_path();
    if let Err(refusal) = guard::check_real_path(&root_real, &root_path, &known) {
        return Err(unavailable(
            real_path_reason(ROOT_LABEL, &root_path, &root_real, refusal),
            Remedy::FixRoot,
        ));
    }
    if let Err(refusal) = guard::dir_acl_plan(&DirState::Present(root.info()), None, None) {
        return Err(unavailable(
            dir_refusal(ROOT_LABEL, &root_path, &refusal),
            Remedy::FixRoot,
        ));
    }
    let root_security = root.security().map_err(|error| {
        unavailable(
            format!(
                "{ROOT_LABEL}（{}）の ACL を読めませんでした: {error}",
                root_path.display()
            ),
            Remedy::Retry,
        )
    })?;
    // 読む側は実行した本人を信頼しない（crate doc「所有者に『実行した本人』を含めない」）。
    let root_open = root_dacl_problems(&root_security, None);
    if !root_open.is_empty() {
        return Err(unavailable(
            format!(
                "{ROOT_LABEL}（{}）は保護されていません（{}）。一般の利用者が {TLS_DIR}\\ を消して差し替えられる恐れがあります",
                root_path.display(),
                reasons_text(&root_open)
            ),
            Remedy::FixRoot,
        ));
    }

    Ok(CheckedRoot {
        root,
        root_path,
        root_real,
        known,
    })
}

/// crate doc の手順 3: 確かめたルートのハンドル基準で `tls\` を開いて確かめる。
fn open_tls<O: TlsTrustOps>(
    ops: &O,
    root: O::Dir,
    root_path: PathBuf,
    root_real: PathBuf,
    known: KnownFolders,
    tls_path: PathBuf,
) -> Result<CheckedPlace<O::Dir>, Unavailable> {
    let recreate = || Remedy::RecreateDir(tls_path.clone());
    let tls = match ops.open_child_dir(&root, TLS_DIR) {
        Ok(Some(tls)) => tls,
        Ok(None) => {
            return Err(unavailable(
                format!("{TLS_LABEL}（{}）がありません", tls_path.display()),
                Remedy::Prepare,
            ))
        }
        Err(error) => {
            return Err(unavailable(
                format!(
                    "{TLS_LABEL}（{}）を開けませんでした: {error}",
                    tls_path.display()
                ),
                Remedy::Retry,
            ))
        }
    };
    let tls_real = tls.final_path();
    if let Err(refusal) = guard::check_real_path(&tls_real, &root_real.join(TLS_DIR), &known) {
        return Err(unavailable(
            real_path_reason(TLS_LABEL, &tls_path, &tls_real, refusal),
            recreate(),
        ));
    }
    if let Err(refusal) = guard::dir_acl_plan(&DirState::Present(tls.info()), None, None) {
        return Err(unavailable(
            dir_refusal(TLS_LABEL, &tls_path, &refusal),
            recreate(),
        ));
    }
    let tls_security = tls.security().map_err(|error| {
        unavailable(
            format!(
                "{TLS_LABEL}（{}）の ACL を読めませんでした: {error}",
                tls_path.display()
            ),
            Remedy::Retry,
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
            recreate(),
        ));
    }

    Ok(CheckedPlace {
        root,
        root_path,
        root_real,
        known,
        tls,
        tls_path,
        tls_real,
    })
}

/// `tls\` の中のファイル 1 つを、`tls\` のハンドル基準で辿らずに読み取りで開き、種類と場所を
/// 確かめる（無ければ `None`。まだ読まない）。フォルダ・reparse point・本当の場所の食い違いは
/// 断る。`label` は文言の中の呼び名（「証明書」など）。
pub fn open_checked_file<O: TlsTrustOps>(
    ops: &O,
    place: &CheckedPlace<O::Dir>,
    name: &str,
    label: &str,
) -> Result<Option<O::File>, Unavailable> {
    let tls_path = place.tls_path();
    open_file_in(ops, place.tls(), tls_path, place.tls_real(), name, label).map_err(|refusal| {
        let remedy = if refusal.transient {
            Remedy::Retry
        } else {
            Remedy::RecreateDir(tls_path.to_path_buf())
        };
        unavailable(refusal.reason, remedy)
    })
}

/// [`open_file_in`] が断った理由。
pub(crate) struct FileRefusal {
    /// 何が合わなかったか（表示用）。
    pub(crate) reason: String,
    /// 一時的かもしれない（開けない・別のプロセスが書き込みに開いている）。偽なら、種類か場所が
    /// 違う（フォルダ・reparse point・本当の場所の食い違い）。
    pub(crate) transient: bool,
}

/// [`open_checked_file`] と利用者ごとの置き場（`crate::user`）が共有する中身: 確かめ済みの
/// フォルダ `dir`（期待のパス `dir_path`・本当のパス `dir_real`）のハンドル基準で、名前 1 要素の
/// ファイルを辿らずに読み取りで開き、種類と場所を確かめる（無ければ `None`。ACL は見ない）。
pub(crate) fn open_file_in<O: TlsTrustOps>(
    ops: &O,
    dir: &O::Dir,
    dir_path: &Path,
    dir_real: &Path,
    name: &str,
    label: &str,
) -> Result<Option<O::File>, FileRefusal> {
    let shown = dir_path.join(name);
    let transient = |reason: String| FileRefusal {
        reason,
        transient: true,
    };
    let file = match ops.open_child_file(dir, name) {
        Ok(Some(file)) => file,
        Ok(None) => return Ok(None),
        Err(FileOpenError::InUse) => {
            return Err(transient(format!(
                "{label}（{}）は別のプロセスで書き込みに開かれています",
                shown.display()
            )))
        }
        Err(FileOpenError::Other(error)) => {
            return Err(transient(format!(
                "{label}（{}）を開けませんでした: {error}",
                shown.display()
            )))
        }
    };
    let security = file.security();
    let problem = if security.is_dir {
        Some("フォルダです".to_string())
    } else if security.is_reparse_point {
        Some("シンボリックリンクなどの reparse point です".to_string())
    } else if !guard::paths_match(&file.final_path(), &dir_real.join(name)) {
        Some(format!(
            "本当の場所（{}）が期待と違います",
            file.final_path().display()
        ))
    } else {
        None
    };
    match problem {
        Some(problem) => Err(FileRefusal {
            reason: format!("{label}（{}）は{problem}", shown.display()),
            transient: false,
        }),
        None => Ok(Some(file)),
    }
}

/// 開いた証明書の ACL が Users の読み取りより広くないことを確かめる
/// （`exposure_beyond_users_read`）。`cert_path` は文言用。
pub fn check_cert_exposure<F: TlsFile>(cert: &F, cert_path: &Path) -> Result<(), Unavailable> {
    let cert_open = exposure_beyond_users_read(&cert.security());
    if cert_open.is_empty() {
        return Ok(());
    }
    Err(unavailable(
        format!(
            "証明書（{}）は保護されていません（{}）。ほかの利用者に書き換えられた恐れがあります",
            cert_path.display(),
            reasons_text(&cert_open)
        ),
        Remedy::Renew,
    ))
}

/// 握ったハンドルから [`MAX_PEM_BYTES`] まで読む（超えたら断る）。`label`・`path` は文言用。
pub fn read_checked<F: TlsFile>(
    file: &mut F,
    label: &str,
    path: &Path,
) -> Result<Vec<u8>, Unavailable> {
    file.read_all(MAX_PEM_BYTES).map_err(|error| {
        unavailable(
            format!("{label}（{}）を読めませんでした: {error}", path.display()),
            Remedy::Retry,
        )
    })
}

/// 証明書の PEM から、ちょうど 1 つの証明書の DER を取り出す（純関数）。秘密鍵が入っている・
/// PEM が読めない・証明書が 1 つでないなら理由（入力の中身は含めない）。DER の中身の解析は
/// しない（呼ぶ側が続けて確かめる）。
pub fn parse_single_certificate(cert_pem: &[u8]) -> Result<Vec<u8>, String> {
    use rustls_pki_types::pem::PemObject;
    use rustls_pki_types::CertificateDer;

    if cert_pem
        .windows(b"PRIVATE KEY".len())
        .any(|window| window == b"PRIVATE KEY")
    {
        return Err("証明書のファイルに秘密鍵が入っています".to_string());
    }
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(cert_pem)
        .collect::<Result<_, _>>()
        .map_err(|_| "証明書の PEM を読めません".to_string())?;
    let [cert_der] = certs.as_slice() else {
        return Err(format!(
            "証明書の PEM に証明書がちょうど 1 つではありません（{} 個）",
            certs.len()
        ));
    };
    Ok(cert_der.as_ref().to_vec())
}

/// 確かめて読んだ、Hub の証明書（pin の根）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubCertPin {
    /// 証明書の PEM（読んだバイト列そのまま）。
    pub cert_pem: Vec<u8>,
    /// 証明書の本当のパス（ログ用。読み直しには使わない）。
    pub cert_path: PathBuf,
}

/// クライアントの入口: 置き場を確かめ（[`open_place`]）、証明書だけを辿らずに開いて
/// （[`open_checked_file`]）ACL を確かめ（[`check_cert_exposure`]）、握ったハンドルから読み
/// （[`read_checked`]）、PEM に証明書がちょうど 1 つで DER として解析できることを確かめる。
/// 鍵は開かない（crate doc「なぜ鍵を読まないか」）。書き込み・作成はしない。
pub fn load_pinned_hub_cert<O: TlsTrustOps>(ops: &O) -> Result<HubCertPin, Unavailable> {
    use x509_cert::der::Decode;

    let place = open_place(ops)?;
    let cert_path = place.tls_path().join(CERT_FILE);
    let Some(mut cert) = open_checked_file(ops, &place, CERT_FILE, "証明書")? else {
        return Err(unavailable(
            format!(
                "{TLS_LABEL}（{}）に証明書（{CERT_FILE}）がありません",
                place.tls_path().display()
            ),
            Remedy::Prepare,
        ));
    };
    check_cert_exposure(&cert, &cert_path)?;
    let real = cert.final_path();
    let cert_pem = read_checked(&mut cert, "証明書", &cert_path)?;
    drop(cert);
    let broken = |error: String| {
        unavailable(
            format!("証明書（{}）は使えません: {error}", cert_path.display()),
            Remedy::Renew,
        )
    };
    let der = parse_single_certificate(&cert_pem).map_err(broken)?;
    x509_cert::Certificate::from_der(&der)
        .map_err(|_| broken("証明書（DER）を読めません".to_string()))?;
    Ok(HubCertPin {
        cert_pem,
        cert_path: real,
    })
}
