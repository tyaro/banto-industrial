//! [`super::HubElevOps`] の Windows の実装（本番）。テストは本物の HKLM・ProgramData・
//! サービス・ローカルグループに触らない（ルートは一時フォルダ、マシンの環境は偽物。
//! GitHub の Windows のランナーは管理者で、本当に変えてしまうため）。

use std::path::{Path, PathBuf};

use banto_secret_store::acl::guard::{self, AccountKind};
use banto_secret_store::acl::{ObservedDacl, PinnedDir, PinnedDirInfo, ResetSkips};
use banto_secret_store::{DirAccess, InstallUserSid};

/// マシンの環境を HKLM と既知のフォルダから読む（このプロセスの環境変数は読まない）。
pub use banto_secret_store::acl::guard::win::RegistryMachineEnvironment;

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
        PinnedDir::reset_acl(self, access, banto_secret_store::acl::Recurse::Tree)
            .map_err(|err| err.to_string())
    }
}

/// `dir`を作る（親から継承する普通のフォルダ。無い親も作る）。作ったら`true`。
fn create_inheriting_dir(dir: &Path) -> Result<bool, String> {
    match std::fs::create_dir(dir) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let parent = dir
                .parent()
                .ok_or_else(|| format!("{} の親フォルダが分かりません", dir.display()))?;
            std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
            match std::fs::create_dir(dir) {
                Ok(()) => Ok(true),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
                Err(err) => Err(err.to_string()),
            }
        }
        Err(err) => Err(err.to_string()),
    }
}

/// `Global\BantoHub.<id>`の mutex があるか（`OpenMutexW(SYNCHRONIZE)`。作らない・
/// 持たない）。開けた・アクセス拒否 = ある、`ERROR_FILE_NOT_FOUND` = 無い、それ以外は
/// 確かめられない（理由）。
pub fn mutex_exists(name: &str) -> Result<bool, String> {
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND,
    };
    use windows_sys::Win32::System::Threading::OpenMutexW;
    /// `SYNCHRONIZE`。
    const SYNCHRONIZE: u32 = 0x0010_0000;
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide`は NUL 終端。開けたハンドルはすぐ閉じる。
    let handle = unsafe { OpenMutexW(SYNCHRONIZE, 0, wide.as_ptr()) };
    if !handle.is_null() {
        // SAFETY: `OpenMutexW`が返した有効なハンドル。
        unsafe { CloseHandle(handle) };
        return Ok(true);
    }
    // SAFETY: 直前の呼び出しのスレッドのエラー。
    match unsafe { GetLastError() } {
        ERROR_FILE_NOT_FOUND => Ok(false),
        ERROR_ACCESS_DENIED => Ok(true),
        other => Err(format!(
            "OpenMutexW({name}) が失敗しました: {}",
            std::io::Error::from_raw_os_error(other as i32)
        )),
    }
}

impl HubElevOps for WindowsHubElevOps {
    type Env = RegistryMachineEnvironment;
    type Pinned = PinnedDir;

    fn machine_env(&self) -> &RegistryMachineEnvironment {
        &RegistryMachineEnvironment
    }

    fn current_user_sid(&self) -> Result<InstallUserSid, String> {
        banto_secret_store::acl::current_user_sid().map_err(|err| err.to_string())
    }

    fn lookup_account_sid(&self, name: &str) -> Result<Option<String>, String> {
        banto_service_elev::lookup_account_sid_string(name)
            .map(|found| found.map(|account| account.sid))
            .map_err(|err| err.to_string())
    }

    fn account_kind(&self, sid: &InstallUserSid) -> AccountKind {
        guard::account_kind(sid)
    }

    fn profile_in_use(&self, profile_id: &str) -> Result<bool, String> {
        mutex_exists(&crate::profile_paths::mutex_name(profile_id))
    }

    fn pin_dir(&self, dir: &Path) -> Result<Option<PinnedDir>, String> {
        banto_secret_store::acl::pin_dir(dir).map_err(|err| err.to_string())
    }

    type HeldFile = HeldFile;

    fn pin_file(&self, path: &Path) -> Result<Option<(HeldFile, FileFacts)>, String> {
        pin_file(path)
    }

    fn create_dir(&self, dir: &Path, how: CreateHow) -> Result<bool, String> {
        match how {
            CreateHow::Inherit => create_inheriting_dir(dir),
            CreateHow::ServiceOnly => {
                banto_secret_store::acl::create_dir_if_absent(dir, &DirAccess::service_only())
                    .map_err(|err| err.to_string())
            }
        }
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
