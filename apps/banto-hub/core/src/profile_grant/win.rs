//! [`super::HubElevOps`] の Windows の実装（本番）。テストは本物の HKLM・ProgramData・
//! サービス・ローカルグループに触らない（ルートは一時フォルダ、マシンの環境は偽物。
//! GitHub の Windows のランナーは管理者で、本当に変えてしまうため）。

use std::path::{Path, PathBuf};

use banto_safe_root::acl::{ObservedDacl, PinnedDir, PinnedDirInfo, ResetSkips};
use banto_safe_root::guard::{self, AccountKind};
use banto_safe_root::{DirAccess, InstallUserSid};

/// マシンの環境を HKLM と既知のフォルダから読む（このプロセスの環境変数は読まない）。
pub use banto_safe_root::guard::win::RegistryMachineEnvironment;

use super::{CreateHow, FileFacts, HubElevOps, PinnedProfileDir};

/// 本番の操作。
pub struct WindowsHubElevOps;

impl PinnedProfileDir for PinnedDir {
    fn info(&self) -> PinnedDirInfo {
        PinnedDir::info(self).clone()
    }

    fn final_path(&self) -> PathBuf {
        PinnedDir::final_path(self).to_path_buf()
    }

    fn read_dacl(&self) -> Result<ObservedDacl, String> {
        PinnedDir::read_dacl(self).map_err(|err| err.to_string())
    }

    fn reset_acl_tree(&self, access: &DirAccess) -> Result<ResetSkips, String> {
        PinnedDir::reset_acl(self, access, banto_safe_root::acl::Recurse::Tree)
            .map_err(|err| err.to_string())
    }
}

/// profile の mutex（`Global\BantoHub.<id>`）を取って持つ（#626 PR-B の再レビュー 3）。
/// Hub と同じ`CreateMutexW`（`banto_instance_lock::try_hold_named_mutex`。Hub の
/// `try_acquire`と違い、確かめる前の profile のパスへ診断ファイルを書かない）。既に誰かが
/// 持っていれば`Ok(None)`。昇格した管理者が作った mutex は既定のセキュリティ記述子
/// （管理者・SYSTEM）で、全部のハンドルが閉じれば消えるので、後の Hub の起動には残らない。
pub fn hold_profile_mutex(
    profile_id: &str,
) -> Result<Option<banto_instance_lock::NamedMutexGuard>, String> {
    let name = crate::profile_paths::mutex_name(profile_id);
    banto_instance_lock::try_hold_named_mutex(&name)
        .map_err(|err| format!("{name} を取れませんでした: {err}"))
}

impl HubElevOps for WindowsHubElevOps {
    type Env = RegistryMachineEnvironment;
    type Pinned = PinnedDir;

    fn machine_env(&self) -> &RegistryMachineEnvironment {
        &RegistryMachineEnvironment
    }

    fn current_user_sid(&self) -> Result<InstallUserSid, String> {
        banto_safe_root::acl::current_user_sid().map_err(|err| err.to_string())
    }

    fn lookup_account_sid(&self, name: &str) -> Result<Option<String>, String> {
        banto_service_elev::lookup_account_sid_string(name)
            .map(|found| found.map(|account| account.sid))
            .map_err(|err| err.to_string())
    }

    fn account_kind(&self, sid: &InstallUserSid) -> AccountKind {
        guard::account_kind(sid)
    }

    type ProfileHold = banto_instance_lock::NamedMutexGuard;

    fn hold_profile(
        &self,
        profile_id: &str,
    ) -> Result<Option<banto_instance_lock::NamedMutexGuard>, String> {
        hold_profile_mutex(profile_id)
    }

    fn pin_dir(&self, dir: &Path) -> Result<Option<PinnedDir>, String> {
        banto_safe_root::acl::pin_dir(dir).map_err(|err| err.to_string())
    }

    type HeldFile = HeldFile;

    fn pin_file(&self, path: &Path) -> Result<Option<(HeldFile, FileFacts)>, String> {
        pin_file(path)
    }

    fn create_child(
        &self,
        parent: &PinnedDir,
        name: &str,
        how: CreateHow,
    ) -> Result<Option<PinnedDir>, String> {
        let service_only = DirAccess::service_only();
        let access = match how {
            CreateHow::Inherit => None,
            CreateHow::ServiceOnly => Some(&service_only),
        };
        parent
            .create_child_dir(name, access)
            .map_err(|err| err.to_string())
    }

    fn open_child(&self, parent: &PinnedDir, name: &str) -> Result<Option<PinnedDir>, String> {
        parent.open_child_dir(name).map_err(|err| err.to_string())
    }
}

/// [`pin_file`] で開いたままのファイル（drop で閉じる）。
pub struct HeldFile(windows_sys::Win32::Foundation::HANDLE);

impl Drop for HeldFile {
    fn drop(&mut self) {
        // SAFETY: `pin_file`の`CreateFileW`が返した有効なハンドル。
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}

/// ファイルを辿らずに開いて持つ（#626 PR-B の監査 B-5）。`FILE_FLAG_OPEN_REPARSE_POINT`で
/// 開き（シンボリックリンクなら先ではなくリンクそのもの）、共有は読み・書きだけで削除を
/// 渡さない（持っている間は名前の変更・削除で別のファイルにすり替えられない。データの
/// 読み取りを求めるので、この共有の指定は後から開く側にも効く。SQLite は読み書き・共有
/// 読み書きで開くので両立する）。属性・リンク数・本当のパスはハンドルから読む。
pub fn pin_file(path: &Path) -> Result<Option<(HeldFile, FileFacts)>, String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{
        GetLastError, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, GENERIC_READ,
        INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, GetFileInformationByHandle, GetFinalPathNameByHandleW,
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_NAME_NORMALIZED,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: `wide`は NUL 終端。
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        // SAFETY: 直前の呼び出しのスレッドのエラー。
        return match unsafe { GetLastError() } {
            ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => Ok(None),
            other => Err(std::io::Error::from_raw_os_error(other as i32).to_string()),
        };
    }
    let held = HeldFile(handle);
    // SAFETY: ゼロで初期化してよい POD。`handle`は有効。
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(handle, &mut info) } == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let mut buffer = vec![0u16; 1024];
    let final_path = loop {
        // SAFETY: `buffer`は`buffer.len()`文字書ける。
        let len = unsafe {
            GetFinalPathNameByHandleW(
                handle,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                FILE_NAME_NORMALIZED,
            )
        } as usize;
        if len == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        if len < buffer.len() {
            let text = String::from_utf16_lossy(&buffer[..len]);
            break PathBuf::from(text.trim_start_matches(r"\\?\").to_string());
        }
        buffer = vec![0u16; len + 1];
    };
    let facts = FileFacts {
        is_dir: info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0,
        is_reparse_point: info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0,
        links: info.nNumberOfLinks,
        final_path,
    };
    Ok(Some((held, facts)))
}

#[cfg(test)]
mod tests;
