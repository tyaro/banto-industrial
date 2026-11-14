//! [`super::HubTlsOps`] の Windows の実装（本番）。部品はすべて `banto_safe_root`（unsafe な
//! Win32 をこの crate に複製しない）: 辿らずに開く・作るのは `acl::PinnedDir`、置き場の中の
//! 削除・付け替えは、確かめた `tls\` のハンドルを包んだ `ManagedDir`（`remove`・
//! `rename_no_replace`）。

use std::path::Path;

use banto_hub_tls_trust::win as trust;
use banto_safe_root::acl::PinnedFile;
use banto_safe_root::guard::win::RegistryMachineEnvironment;
use banto_safe_root::InstallUserSid;

use super::{DirHow, FileAcl, HubTlsOps, ServiceRevocationOps, TlsTrustOps};
use crate::sink_config_prepare::{secret_error_text, FileOpenError};

/// 辿らずに開いたフォルダ（共有 crate の `banto_hub_tls_trust::win::WinTlsDir`。削除・付け替えに
/// `ManagedDir` の口を使うため、`ManagedDir::from_pinned` で包んで持つ）。
pub use banto_hub_tls_trust::win::WinTlsDir;

/// 本番の操作。
pub struct WindowsHubTlsOps;

/// 開く・読む操作は共有 crate の実装（#644 PR3a）。フォルダは DACL・所有者を書ける pin
/// （`*_for_hub`。クライアントの `WindowsTlsTrustOps` は読み取り専用の pin）。
impl TlsTrustOps for WindowsHubTlsOps {
    type Env = RegistryMachineEnvironment;
    type Dir = WinTlsDir;
    type File = PinnedFile;

    fn machine_env(&self) -> &RegistryMachineEnvironment {
        &RegistryMachineEnvironment
    }

    fn pin_dir(&self, dir: &Path) -> Result<Option<WinTlsDir>, String> {
        trust::pin_dir_for_hub(dir)
    }

    fn open_child_dir(&self, parent: &WinTlsDir, name: &str) -> Result<Option<WinTlsDir>, String> {
        trust::open_child_dir_for_hub(parent, name)
    }

    fn open_child_file(
        &self,
        parent: &WinTlsDir,
        name: &str,
    ) -> Result<Option<PinnedFile>, FileOpenError> {
        trust::open_child_file(parent, name)
    }
}

/// サービスの失効の記録を書く操作（#1121 PR2b）。部品は共有 crate の `win` の関数（中身は
/// `banto_safe_root`）: フォルダは `tls\` と同じ記述子で作り、記録のファイルは証明書と同じ記述子
/// （所有者 Administrators を明示）で作る・置き換える。
impl ServiceRevocationOps for WindowsHubTlsOps {
    fn create_protected_child_dir(
        &self,
        parent: &WinTlsDir,
        name: &str,
    ) -> Result<Option<WinTlsDir>, String> {
        trust::create_child_dir_users_read(parent, name)
    }

    fn create_file_no_replace(
        &self,
        dir: &WinTlsDir,
        name: &str,
        contents: &[u8],
    ) -> Result<bool, String> {
        trust::create_file_no_replace_users_read(dir, name, contents)
    }

    fn write_file_atomic(
        &self,
        dir: &WinTlsDir,
        name: &str,
        contents: &[u8],
    ) -> Result<(), String> {
        trust::write_file_atomic_users_read(dir, name, contents)
    }

    fn remove_stale_file_temps(&self, dir: &WinTlsDir, name: &str) -> Result<(), String> {
        trust::remove_stale_temps(dir, name)
    }
}

impl HubTlsOps for WindowsHubTlsOps {
    type Lock = banto_instance_lock::NamedMutexGuard;

    fn hold_lock(&self) -> Result<Option<Self::Lock>, String> {
        banto_instance_lock::try_hold_named_mutex(super::LOCK_NAME).map_err(|err| err.to_string())
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
        parent
            .managed()
            .remove(name)
            .map_err(|err| secret_error_text(&err))
    }

    fn rename_no_replace(&self, parent: &WinTlsDir, from: &str, to: &str) -> Result<bool, String> {
        parent
            .managed()
            .rename_no_replace(from, to)
            .map_err(|err| secret_error_text(&err))
    }
}

#[cfg(test)]
mod tests;
