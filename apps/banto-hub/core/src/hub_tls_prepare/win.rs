//! [`super::HubTlsOps`] の Windows の実装（本番）。部品はすべて `banto_safe_root`（unsafe な
//! Win32 をこの crate に複製しない）: 辿らずに開く・作るのは `acl::PinnedDir`、置き場の中の
//! 削除・付け替えは、確かめた `tls\` のハンドルを包んだ `ManagedDir`（`remove`・
//! `rename_no_replace`）。

use std::path::{Path, PathBuf};

use banto_safe_root::acl::{FileSecurity, PinnedDir, PinnedDirInfo, PinnedFile};
use banto_safe_root::{InstallUserSid, ManagedDir, SafeRootError};

use super::{DirHow, FileAcl, HubTlsOps, TlsDir, TlsFile};
use crate::profile_grant::win::RegistryMachineEnvironment;
use crate::sink_config_prepare::{secret_error_text, FileOpenError};

/// 本番の操作。
pub struct WindowsHubTlsOps;

/// 辿らずに開いたフォルダ。削除・付け替えに `ManagedDir` の口を使うため、
/// [`ManagedDir::from_pinned`] で包んで持つ（中身は常に辿らずに握ったハンドル）。
pub struct WinTlsDir(ManagedDir);

impl WinTlsDir {
    fn new(pinned: PinnedDir) -> Self {
        Self(ManagedDir::from_pinned(pinned))
    }

    fn pinned(&self) -> &PinnedDir {
        // `from_pinned` で作るので、いつも握ったハンドルがある。
        self.0
            .pinned()
            .expect("WinTlsDir は辿らずに開いたフォルダだけを包む")
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

impl HubTlsOps for WindowsHubTlsOps {
    type Env = RegistryMachineEnvironment;
    type Dir = WinTlsDir;
    type File = PinnedFile;
    type Lock = banto_instance_lock::NamedMutexGuard;

    fn hold_lock(&self) -> Result<Option<Self::Lock>, String> {
        banto_instance_lock::try_hold_named_mutex(super::LOCK_NAME).map_err(|err| err.to_string())
    }

    fn machine_env(&self) -> &RegistryMachineEnvironment {
        &RegistryMachineEnvironment
    }

    fn current_user_sid(&self) -> Result<InstallUserSid, String> {
        banto_safe_root::acl::current_user_sid().map_err(|err| err.to_string())
    }

    fn now_unix(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX))
            .unwrap_or(0)
    }

    fn pin_dir(&self, dir: &Path) -> Result<Option<WinTlsDir>, String> {
        banto_safe_root::acl::pin_dir(dir)
            .map(|found| found.map(WinTlsDir::new))
            .map_err(|err| err.to_string())
    }

    fn create_child_dir(
        &self,
        parent: &WinTlsDir,
        name: &str,
        how: DirHow,
    ) -> Result<Option<WinTlsDir>, String> {
        let made = match how {
            DirHow::Inherit => parent.pinned().create_child_dir(name, None),
            DirHow::UsersRead => parent.pinned().create_child_dir_users_read(name),
        };
        made.map(|found| found.map(WinTlsDir::new))
            .map_err(|err| err.to_string())
    }

    fn open_child_dir(&self, parent: &WinTlsDir, name: &str) -> Result<Option<WinTlsDir>, String> {
        parent
            .pinned()
            .open_child_dir(name)
            .map(|found| found.map(WinTlsDir::new))
            .map_err(|err| err.to_string())
    }

    fn open_child_file(
        &self,
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

    fn create_child_file(
        &self,
        parent: &WinTlsDir,
        name: &str,
        contents: &[u8],
        acl: FileAcl,
    ) -> Result<bool, String> {
        let made = match acl {
            FileAcl::ServiceOnly => parent.pinned().create_child_file(name, contents),
            FileAcl::UsersRead => parent.pinned().create_child_file_users_read(name, contents),
        };
        made.map(|found| found.is_some())
            .map_err(|err| secret_error_text(&err))
    }

    fn remove_child(&self, parent: &WinTlsDir, name: &str) -> Result<bool, String> {
        parent.0.remove(name).map_err(|err| secret_error_text(&err))
    }

    fn rename_no_replace(&self, parent: &WinTlsDir, from: &str, to: &str) -> Result<bool, String> {
        parent
            .0
            .rename_no_replace(from, to)
            .map_err(|err| secret_error_text(&err))
    }
}

#[cfg(test)]
mod tests;
