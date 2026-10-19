//! [`super::HubElevOps`] の Windows の実装（本番）。テストは本物の HKLM・ProgramData・
//! サービス・ローカルグループに触らない（ルートは一時フォルダ、マシンの環境は偽物。
//! GitHub の Windows のランナーは管理者で、本当に変えてしまうため）。

use std::path::{Path, PathBuf};

use banto_safe_root::acl::{ObservedDacl, PinnedDir, PinnedDirInfo};
use banto_safe_root::guard::{self, AccountKind};
use banto_safe_root::mode::{ModeAccess, RelaxReport};
use banto_safe_root::{DirAccess, InstallUserSid};

/// マシンの環境を HKLM と既知のフォルダから読む（このプロセスの環境変数は読まない）。
pub use banto_safe_root::guard::win::RegistryMachineEnvironment;

use super::{
    is_in_account_domain, CreateHow, FileFacts, HubElevOps, OwnerLookup, OwnersWrite,
    PinnedProfileDir,
};

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

    fn relax_tree(&self, mode: &ModeAccess) -> Result<RelaxReport, String> {
        // 進み具合は出さない（昇格ヘルパーは一度に書き切る）。
        PinnedDir::relax_tree(self, mode, &mut |_| {}).map_err(|err| err.to_string())
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

/// 戻す相手の記録の SID を照会する（[`OwnerLookup`]。#704 レビュー P2）。
/// `LookupAccountSidW`が名前を解けなかった（`ERROR_NONE_MAPPED`）ときは、SID がこの PC の
/// ローカルのアカウントのドメイン（[`account_domain_sid`]）に属するかを添える。共有の
/// `guard::account_kind`は`ERROR_NONE_MAPPED`とグループを分けないので、ここで別に照会する
/// （共有 crate を変えると ChronoGazer の経路にも効くため、Hub 側に置く）。
pub fn owner_lookup(sid: &InstallUserSid) -> OwnerLookup {
    match lookup_sid_use(sid.as_str()) {
        Ok(Some(kind)) if kind == windows_sys::Win32::Security::SidTypeUser => OwnerLookup::User,
        Ok(Some(_)) => OwnerLookup::NotAUser,
        Ok(None) => match account_domain_sid() {
            Ok(domain) => OwnerLookup::Unmapped {
                local: is_in_account_domain(sid.as_str(), &domain),
            },
            Err(why) => OwnerLookup::LookupFailed(format!(
                "名前を解けず、この PC のアカウントのドメインの SID も読めませんでした: {why}"
            )),
        },
        Err(why) => OwnerLookup::LookupFailed(why),
    }
}

/// `LocalAlloc`の領域（`ConvertStringSidToSidW`・`ConvertSidToStringSidW`の結果）を手放す。
struct LocalBox(*mut core::ffi::c_void);

impl Drop for LocalBox {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: LocalAlloc の領域で、ほかに解放する所は無い。
            unsafe { windows_sys::Win32::Foundation::LocalFree(self.0) };
        }
    }
}

fn os_error(api: &str, code: u32) -> String {
    format!(
        "{api} が失敗しました: {}",
        std::io::Error::from_raw_os_error(code as i32)
    )
}

/// SID の文字列を`LookupAccountSidW`で照会し、種類（`SID_NAME_USE`）を返す。名前を
/// 解けない（`ERROR_NONE_MAPPED`）なら`Ok(None)`、それ以外の失敗は`Err`。
fn lookup_sid_use(text: &str) -> Result<Option<i32>, String> {
    use windows_sys::Win32::Foundation::{
        GetLastError, ERROR_INSUFFICIENT_BUFFER, ERROR_NONE_MAPPED,
    };
    use windows_sys::Win32::Security::Authorization::ConvertStringSidToSidW;
    use windows_sys::Win32::Security::{LookupAccountSidW, PSID, SID_NAME_USE};

    let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    let mut sid: PSID = std::ptr::null_mut();
    // SAFETY: `wide`は NUL 終端。成功すれば`sid`は LocalAlloc の領域。
    if unsafe { ConvertStringSidToSidW(wide.as_ptr(), &mut sid) } == 0 {
        // SAFETY: 直前の API の失敗の理由を読むだけ。
        let code = unsafe { GetLastError() };
        return Err(os_error("ConvertStringSidToSidW", code));
    }
    let sid = LocalBox(sid);
    let (mut name_len, mut domain_len) = (256u32, 256u32);
    // 長さが足りなければ、返った長さで 1 回だけ取り直す。
    for _ in 0..2 {
        let mut name = vec![0u16; name_len as usize];
        let mut domain = vec![0u16; domain_len as usize];
        let mut kind: SID_NAME_USE = 0;
        // SAFETY: バッファは長さとともに渡す。名前そのものは使わず、種類だけ見る。
        let ok = unsafe {
            LookupAccountSidW(
                std::ptr::null(),
                sid.0,
                name.as_mut_ptr(),
                &mut name_len,
                domain.as_mut_ptr(),
                &mut domain_len,
                &mut kind,
            )
        };
        if ok != 0 {
            return Ok(Some(kind));
        }
        // SAFETY: 直前の API の失敗の理由を読むだけ。
        let code = unsafe { GetLastError() };
        match code {
            ERROR_NONE_MAPPED => return Ok(None),
            ERROR_INSUFFICIENT_BUFFER => continue,
            _ => return Err(os_error("LookupAccountSidW", code)),
        }
    }
    Err(os_error("LookupAccountSidW", ERROR_INSUFFICIENT_BUFFER))
}

/// この PC のローカルのアカウントのドメインの SID（マシンの SID、`S-1-5-21-a-b-c`）を
/// `LsaQueryInformationPolicy(PolicyAccountDomainInformation)`で読む。
pub fn account_domain_sid() -> Result<String, String> {
    use windows_sys::Win32::Security::Authentication::Identity::{
        LsaClose, LsaFreeMemory, LsaNtStatusToWinError, LsaOpenPolicy, LsaQueryInformationPolicy,
        PolicyAccountDomainInformation, LSA_OBJECT_ATTRIBUTES, POLICY_ACCOUNT_DOMAIN_INFO,
        POLICY_VIEW_LOCAL_INFORMATION,
    };

    // SAFETY: 全部 0 の LSA_OBJECT_ATTRIBUTES は LsaOpenPolicy の求める形（Length 以外は予約）。
    let mut attributes: LSA_OBJECT_ATTRIBUTES = unsafe { std::mem::zeroed() };
    attributes.Length = std::mem::size_of::<LSA_OBJECT_ATTRIBUTES>() as u32;
    let mut policy = 0isize;
    // SAFETY: 引数は有効な領域。成功すれば`policy`は LsaClose で閉じる。
    let status = unsafe {
        LsaOpenPolicy(
            std::ptr::null(),
            &attributes,
            POLICY_VIEW_LOCAL_INFORMATION as u32,
            &mut policy,
        )
    };
    if status != 0 {
        // SAFETY: NTSTATUS を Win32 のエラーに直すだけ。
        let code = unsafe { LsaNtStatusToWinError(status) };
        return Err(os_error("LsaOpenPolicy", code));
    }
    let mut buffer: *mut core::ffi::c_void = std::ptr::null_mut();
    // SAFETY: `policy`は開いたハンドル。成功すれば`buffer`は LsaFreeMemory で手放す。
    let status =
        unsafe { LsaQueryInformationPolicy(policy, PolicyAccountDomainInformation, &mut buffer) };
    let result = if status != 0 {
        // SAFETY: NTSTATUS を Win32 のエラーに直すだけ。
        let code = unsafe { LsaNtStatusToWinError(status) };
        Err(os_error("LsaQueryInformationPolicy", code))
    } else {
        // SAFETY: 成功した PolicyAccountDomainInformation の`buffer`は
        // POLICY_ACCOUNT_DOMAIN_INFO。
        let domain_sid = unsafe { (*(buffer as *const POLICY_ACCOUNT_DOMAIN_INFO)).DomainSid };
        let converted = sid_to_string(domain_sid);
        // SAFETY: LsaQueryInformationPolicy が確保した領域（`domain_sid`はもう使わない）。
        unsafe { LsaFreeMemory(buffer) };
        converted
    };
    // SAFETY: 開いたハンドルを閉じる。
    unsafe { LsaClose(policy) };
    result
}

/// SID を文字列（`S-1-...`）にする（`ConvertSidToStringSidW`）。
fn sid_to_string(sid: windows_sys::Win32::Security::PSID) -> Result<String, String> {
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;

    if sid.is_null() {
        return Err("この PC のアカウントのドメインの SID がありません".to_string());
    }
    let mut text: windows_sys::core::PWSTR = std::ptr::null_mut();
    // SAFETY: `sid`は有効な SID。成功すれば`text`は LocalAlloc の NUL 終端の文字列。
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        // SAFETY: 直前の API の失敗の理由を読むだけ。
        let code = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        return Err(os_error("ConvertSidToStringSidW", code));
    }
    let text = LocalBox(text.cast());
    let start = text.0 as *const u16;
    // SAFETY: NUL 終端の文字列を終端の手前まで読む。
    let len = (0..).take_while(|&i| unsafe { *start.add(i) } != 0).count();
    // SAFETY: 長さ`len`の領域は上で読めた。`text`はこの関数の終わりまで生きている。
    let slice = unsafe { std::slice::from_raw_parts(start, len) };
    Ok(String::from_utf16_lossy(slice))
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

    fn owner_lookup(&self, sid: &InstallUserSid) -> OwnerLookup {
        owner_lookup(sid)
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

    fn read_owners(&self, profile_id: &str) -> Result<Option<Vec<String>>, String> {
        crate::profile_owners::read_owners(profile_id)
    }

    fn write_owners(
        &self,
        profile_id: &str,
        owners: &[InstallUserSid],
    ) -> Result<OwnersWrite, String> {
        if !crate::profile_owners::service_registered()? {
            return Ok(OwnersWrite::NotRegistered);
        }
        crate::profile_owners::write_owners(profile_id, owners).map(|()| OwnersWrite::Written)
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
