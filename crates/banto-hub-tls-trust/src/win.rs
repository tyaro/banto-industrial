//! [`TlsTrustOps`] の Windows の実装（本番）。部品はすべて `banto_safe_root`（unsafe な Win32 を
//! この crate に複製しない）: 辿らずに開くのは `acl::PinnedDir`。フォルダは、banto-hub-core の
//! `prepare-hub-tls` が削除・付け替えに `ManagedDir` の口を使うため、[`ManagedDir::from_pinned`]
//! で包んで持つ（[`WinTlsDir`]。中身は常に辿らずに握ったハンドル）。
//!
//! フォルダの開き方は 2 つ。クライアント（[`WindowsTlsTrustOps`]）は**読み取り専用**の pin
//! （[`pin_dir_read_only`]・[`open_child_dir_read_only`]。`WRITE_DAC | WRITE_OWNER` を求めない）で
//! 開く: 一般利用者（昇格なし）のデスクトップでも動くため（ルート・`tls\` に `WRITE_DAC` を持た
//! ない）。Hub（banto-hub-core の `WindowsHubTlsOps`。SYSTEM・昇格した管理者）は今までどおり
//! DACL を書ける pin（[`pin_dir_for_hub`]・[`open_child_dir_for_hub`]）で開く。

use std::path::{Path, PathBuf};

use banto_safe_root::acl::{FileSecurity, PinnedDir, PinnedDirInfo, PinnedFile};
use banto_safe_root::guard::win::RegistryMachineEnvironment;
use banto_safe_root::{ManagedDir, SafeRootError};

use crate::text::secret_error_text;
use crate::{FileOpenError, TlsDir, TlsFile, TlsTrustOps};

/// 本番の読み取りの操作（クライアント用。フォルダは読み取り専用の pin で開くので、一般利用者
/// でも動く）。サービスの Hub は banto-hub-core の `WindowsHubTlsOps` が同じ部品で同じことを
/// する（フォルダは DACL を書ける pin）。
pub struct WindowsTlsTrustOps;

/// 辿らずに開いたフォルダ。
pub struct WinTlsDir(ManagedDir);

impl WinTlsDir {
    /// 辿らずに開いたフォルダを包む。
    pub fn new(pinned: PinnedDir) -> Self {
        Self(ManagedDir::from_pinned(pinned))
    }

    /// 握ったハンドル。
    pub fn pinned(&self) -> &PinnedDir {
        // `from_pinned` で作るので、いつも握ったハンドルがある。
        self.0
            .pinned()
            .expect("WinTlsDir は辿らずに開いたフォルダだけを包む")
    }

    /// 削除・付け替えの口（banto-hub-core の `prepare-hub-tls` 用）。
    pub fn managed(&self) -> &ManagedDir {
        &self.0
    }
}

impl TlsDir for WinTlsDir {
    fn info(&self) -> PinnedDirInfo {
        self.pinned().info().clone()
    }

    fn final_path(&self) -> PathBuf {
        self.pinned().final_path().to_path_buf()
    }

    fn security(&self) -> Result<FileSecurity, String> {
        self.pinned()
            .security()
            .map_err(|err| secret_error_text(&err))
    }
}

impl TlsFile for PinnedFile {
    fn security(&self) -> FileSecurity {
        PinnedFile::security(self).clone()
    }

    fn final_path(&self) -> PathBuf {
        PinnedFile::final_path(self).to_path_buf()
    }

    fn read_all(&mut self, max_len: usize) -> Result<Vec<u8>, String> {
        self.read_to_end(max_len)
            .map_err(|err| secret_error_text(&err))
    }
}

/// `ERROR_SHARING_VIOLATION`。
const ERROR_SHARING_VIOLATION: i32 = 32;

/// Hub 用: DACL・所有者を書ける権利（`banto_safe_root::acl::pin_dir`）で辿らずに開く（無ければ
/// `Ok(None)`）。一般利用者では開けない（ルートに `WRITE_DAC` を持たない）ので、クライアントは
/// [`pin_dir_read_only`] を使う。
pub fn pin_dir_for_hub(dir: &Path) -> Result<Option<WinTlsDir>, String> {
    banto_safe_root::acl::pin_dir(dir)
        .map(|found| found.map(WinTlsDir::new))
        .map_err(|err| err.to_string())
}

/// Hub 用: `parent` のハンドル基準に、名前 1 要素の子のフォルダを DACL・所有者を書ける権利で
/// 辿らずに開く。
pub fn open_child_dir_for_hub(parent: &WinTlsDir, name: &str) -> Result<Option<WinTlsDir>, String> {
    parent
        .pinned()
        .open_child_dir(name)
        .map(|found| found.map(WinTlsDir::new))
        .map_err(|err| err.to_string())
}

/// クライアント用: 読み取り専用の pin（`banto_safe_root::acl::pin_dir_read_only`）で辿らずに
/// 開く（無ければ `Ok(None)`）。持っている間は名前の変更・削除を止める。
pub fn pin_dir_read_only(dir: &Path) -> Result<Option<WinTlsDir>, String> {
    banto_safe_root::acl::pin_dir_read_only(dir)
        .map(|found| found.map(WinTlsDir::new))
        .map_err(|err| err.to_string())
}

/// クライアント用: `parent` のハンドル基準に、名前 1 要素の子のフォルダを読み取り専用の pin で
/// 辿らずに開く。
pub fn open_child_dir_read_only(
    parent: &WinTlsDir,
    name: &str,
) -> Result<Option<WinTlsDir>, String> {
    parent
        .pinned()
        .open_child_dir_read_only(name)
        .map(|found| found.map(WinTlsDir::new))
        .map_err(|err| err.to_string())
}

/// `parent` のハンドル基準に、名前 1 要素の子を読み取りで辿らずに開く（共有は読み取りだけ）。
pub fn open_child_file(
    parent: &WinTlsDir,
    name: &str,
) -> Result<Option<PinnedFile>, FileOpenError> {
    parent
        .pinned()
        .open_child_file(name)
        .map_err(|err| match err {
            SafeRootError::Io {
                os_error: Some(ERROR_SHARING_VIOLATION),
                ..
            } => FileOpenError::InUse,
            other => FileOpenError::Other(secret_error_text(&other)),
        })
}

impl TlsTrustOps for WindowsTlsTrustOps {
    type Env = RegistryMachineEnvironment;
    type Dir = WinTlsDir;
    type File = PinnedFile;

    fn machine_env(&self) -> &RegistryMachineEnvironment {
        &RegistryMachineEnvironment
    }

    fn pin_dir(&self, dir: &Path) -> Result<Option<WinTlsDir>, String> {
        pin_dir_read_only(dir)
    }

    fn open_child_dir(&self, parent: &WinTlsDir, name: &str) -> Result<Option<WinTlsDir>, String> {
        open_child_dir_read_only(parent, name)
    }

    fn open_child_file(
        &self,
        parent: &WinTlsDir,
        name: &str,
    ) -> Result<Option<PinnedFile>, FileOpenError> {
        open_child_file(parent, name)
    }
}

/// 利用者ごとの置き場（#1121 PR1a）の読み取り。部品は `banto_safe_root` のもの:
/// `%LOCALAPPDATA%` は既知のフォルダ（`guard::win::local_app_data`）、SID はプロセスのトークン
/// （`acl::current_user_sid`）、ドライブの種類は `guard::win::drive_kind`。フォルダは読み取り
/// 専用の pin で開く（[`WindowsTlsTrustOps`] のまま）。
impl crate::UserTlsOps for WindowsTlsTrustOps {
    fn local_app_data(&self) -> Result<PathBuf, String> {
        banto_safe_root::guard::win::local_app_data()
    }

    fn current_user_sid(&self) -> Result<String, String> {
        banto_safe_root::acl::current_user_sid()
            .map(|sid| sid.as_str().to_string())
            .map_err(|err| err.to_string())
    }

    fn drive_kind(&self, real: &Path) -> Result<banto_safe_root::guard::DriveKind, String> {
        banto_safe_root::guard::win::drive_kind(real)
    }
}
