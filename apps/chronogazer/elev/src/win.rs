//! [`crate::ElevOps`] の Windows の実装（本番）。**テストからは呼ばない**（本物の
//! サービス・ローカルグループ・HKLM・ProgramData を変えてしまうため）。読むだけの
//! マシンの環境の読み取り（[`RegistryMachineEnvironment`]）も、テストは偽物を使う。

use std::path::{Path, PathBuf};

use banto_secret_store::acl::{PinnedDir, PinnedDirInfo};
use banto_secret_store::{DirAccess, InstallUserSid};
use banto_service_control::{
    query_service_command_line, read_service_parameter, uninstall_service, ServiceManager,
    ServiceManagerError, UninstallError, UninstallOutcome, WindowsServiceManager,
};
use banto_service_elev::{ElevError, GroupOutcome, MemberOutcome};
use chronogazer_core::paths::{DataLayout, ROOT_ENV};
use chronogazer_core::server_service::{
    install_with, InstallReport, InstallUserUnavailable, INSTALL_USER_SID_VALUE,
};
use chronogazer_core::shell_launch::{chronogazer_service_spec, SERVICE_NAME};

use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_SUCCESS};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::System::Registry::{
    RegGetValueW, HKEY_LOCAL_MACHINE, RRF_NOEXPAND, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ,
};
use windows_sys::Win32::UI::Shell::{
    FOLDERID_ProgramData, FOLDERID_ProgramFiles, FOLDERID_ProgramFilesCommon,
    FOLDERID_ProgramFilesCommonX86, FOLDERID_ProgramFilesX86, FOLDERID_Public, FOLDERID_System,
    FOLDERID_SystemX86, FOLDERID_UserProfiles, FOLDERID_Windows, SHGetKnownFolderPath,
};

use crate::{
    resolve_machine_root, ElevOps, KnownFolders, MachineEnvironment, PinnedDirOps,
    MACHINE_ENVIRONMENT_KEY, OPERATORS_GROUP, SERVER_EXE_FILE_NAME,
};

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// マシンの環境を HKLM と既知のフォルダから読む（このプロセスの環境変数は読まない）。
pub struct RegistryMachineEnvironment;

impl MachineEnvironment for RegistryMachineEnvironment {
    fn chronogazer_root(&self) -> Result<Option<String>, String> {
        read_hklm_string(MACHINE_ENVIRONMENT_KEY, ROOT_ENV)
    }

    fn program_data(&self) -> Result<PathBuf, String> {
        known_folder(&FOLDERID_ProgramData)
    }

    fn known_folders(&self) -> Result<KnownFolders, String> {
        // どれか 1 つでも取れなければ止める（断る一覧が欠けたまま進まない）。
        let get = |ids: &[&windows_sys::core::GUID]| -> Result<Vec<PathBuf>, String> {
            ids.iter().map(|id| known_folder(id)).collect()
        };
        Ok(KnownFolders {
            forbidden_inside: get(&[
                &FOLDERID_Windows,
                &FOLDERID_System,
                &FOLDERID_SystemX86,
                &FOLDERID_ProgramFiles,
                &FOLDERID_ProgramFilesX86,
                &FOLDERID_ProgramFilesCommon,
                &FOLDERID_ProgramFilesCommonX86,
                &FOLDERID_Public,
            ])?,
            forbidden_itself: get(&[&FOLDERID_ProgramData, &FOLDERID_UserProfiles])?,
        })
    }
}

/// `HKLM\<key>` の `name`（REG_SZ か REG_EXPAND_SZ。**展開しない**生の値）。無ければ
/// `Ok(None)`。
fn read_hklm_string(key: &str, name: &str) -> Result<Option<String>, String> {
    let key = wide(key);
    let name = wide(name);
    let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND;
    let mut buffer: Vec<u16> = vec![0; 260];
    for _ in 0..4 {
        let mut bytes = (buffer.len() * 2) as u32;
        // SAFETY: `key`・`name` は NUL 終端、`buffer` は `bytes` バイト書ける。
        let status = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                key.as_ptr(),
                name.as_ptr(),
                flags,
                std::ptr::null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut bytes,
            )
        };
        match status {
            ERROR_SUCCESS => {
                let len = (bytes as usize / 2).min(buffer.len());
                let text = &buffer[..len];
                let end = text.iter().position(|&c| c == 0).unwrap_or(text.len());
                return Ok(Some(String::from_utf16_lossy(&text[..end])));
            }
            ERROR_FILE_NOT_FOUND => return Ok(None),
            ERROR_MORE_DATA => {
                buffer = vec![0; (bytes as usize).div_ceil(2) + 1];
            }
            other => return Err(std::io::Error::from_raw_os_error(other as i32).to_string()),
        }
    }
    Err("値が読み取りの間に伸び続けました".to_string())
}

/// 既知のフォルダ（このプロセスの環境変数ではなく、OS の設定から）。
fn known_folder(id: &windows_sys::core::GUID) -> Result<PathBuf, String> {
    let mut path: windows_sys::core::PWSTR = std::ptr::null_mut();
    // SAFETY: `id` は有効な GUID。成功・失敗に関わらず `path` は CoTaskMemFree で解放する。
    let hr = unsafe { SHGetKnownFolderPath(id, 0, std::ptr::null_mut(), &mut path) };
    let result = if hr >= 0 && !path.is_null() {
        let mut len = 0;
        // SAFETY: 成功した SHGetKnownFolderPath は NUL 終端の文字列を返す。
        while unsafe { *path.add(len) } != 0 {
            len += 1;
        }
        // SAFETY: `len` 文字ぶんは上で読めた範囲。
        let slice = unsafe { std::slice::from_raw_parts(path, len) };
        Ok(PathBuf::from(String::from_utf16_lossy(slice)))
    } else {
        Err(format!(
            "SHGetKnownFolderPath が失敗しました (HRESULT {hr:#010x})"
        ))
    };
    // SAFETY: `path` は SHGetKnownFolderPath が確保した領域か null（null は何もしない）。
    unsafe { CoTaskMemFree(path as *const core::ffi::c_void) };
    result
}

/// 本番の操作。
pub struct WindowsOps;

impl PinnedDirOps for PinnedDir {
    fn info(&self) -> PinnedDirInfo {
        PinnedDir::info(self).clone()
    }

    fn reset_acl(&self, access: &DirAccess) -> Result<(), String> {
        PinnedDir::reset_acl(self, access).map_err(|err| err.to_string())
    }
}

impl ElevOps for WindowsOps {
    type Pinned = PinnedDir;

    fn server_executable(&self) -> Result<PathBuf, String> {
        let me = std::env::current_exe().map_err(|err| err.to_string())?;
        let dir = me
            .parent()
            .ok_or_else(|| format!("{} の親フォルダが分かりません", me.display()))?;
        let server = dir.join(SERVER_EXE_FILE_NAME);
        if server.is_file() {
            Ok(server)
        } else {
            Err(format!("{} がありません", server.display()))
        }
    }

    fn program_files_dirs(&self) -> Vec<PathBuf> {
        [&FOLDERID_ProgramFiles, &FOLDERID_ProgramFilesX86]
            .into_iter()
            .filter_map(|id| known_folder(id).ok())
            .collect()
    }

    fn registered_command_line(&self) -> Result<Option<String>, String> {
        query_service_command_line(SERVICE_NAME).map_err(|err| err.to_string())
    }

    fn machine_root(&self) -> Result<PathBuf, String> {
        resolve_machine_root(&RegistryMachineEnvironment)
    }

    fn current_user_sid(&self) -> Result<InstallUserSid, String> {
        banto_secret_store::acl::current_user_sid().map_err(|err| err.to_string())
    }

    fn check_user_account(&self, sid: &InstallUserSid) -> Result<(), String> {
        banto_secret_store::acl::check_user_account(sid).map_err(|err| err.to_string())
    }

    fn install(
        &self,
        executable: PathBuf,
        sid: InstallUserSid,
        layout: &DataLayout,
    ) -> Result<InstallReport, InstallUserUnavailable> {
        // Program Files の外の警告は elev が先に出すので、ここでは渡さない（二重に出さない）。
        install_with(executable, Some(sid), layout, None)
    }

    fn uninstall(&self) -> Result<UninstallOutcome, UninstallError> {
        uninstall_service(SERVICE_NAME)
    }

    fn read_install_user(&self) -> Result<Option<String>, String> {
        read_service_parameter(SERVICE_NAME, INSTALL_USER_SID_VALUE).map_err(|err| err.to_string())
    }

    fn ensure_group(&self) -> Result<GroupOutcome, ElevError> {
        banto_service_elev::ensure_local_group(&OPERATORS_GROUP)
    }

    fn add_member(&self, sid: &InstallUserSid) -> Result<MemberOutcome, ElevError> {
        banto_service_elev::add_member_sid(&OPERATORS_GROUP, sid.as_str())
    }

    fn grant_service_acl(&self) -> Result<(), ElevError> {
        banto_service_elev::grant_service_operators(SERVICE_NAME, &OPERATORS_GROUP)
    }

    fn set_autostart(&self, executable: &Path, enabled: bool) -> Result<(), ServiceManagerError> {
        WindowsServiceManager::for_spec(&chronogazer_service_spec(executable.to_path_buf()))
            .set_auto_start(enabled)
    }

    fn pin_dir(&self, dir: &Path) -> Result<Option<PinnedDir>, String> {
        banto_secret_store::acl::pin_dir(dir).map_err(|err| err.to_string())
    }

    fn create_dir(&self, dir: &Path, access: &DirAccess) -> Result<bool, String> {
        banto_secret_store::acl::create_dir_if_absent(dir, access).map_err(|err| err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 読むだけ: マシンの ProgramData は既知のフォルダから取れる（環境変数を見ない）。
    #[test]
    fn known_folders_resolve() {
        let program_data = RegistryMachineEnvironment.program_data().unwrap();
        assert!(program_data.is_absolute(), "{program_data:?}");
        let dirs = WindowsOps.program_files_dirs();
        assert_eq!(dirs.len(), 2, "{dirs:?}");
        assert!(dirs.iter().all(|d| d.is_absolute()));
        // 既知のフォルダの一覧が取れ、ProgramData の下の既定のルートは断られない。
        let known = RegistryMachineEnvironment.known_folders().unwrap();
        assert_eq!(known.forbidden_inside.len(), 8);
        crate::reject_well_known_root(&program_data.join("ChronoGazer"), &known).unwrap();
        assert!(crate::reject_well_known_root(&program_data, &known).is_err());
    }

    /// 読むだけ: 無い値は `None`、ある値（`Path` は必ずある）は読める。
    #[test]
    fn hklm_strings_read_without_expanding() {
        assert_eq!(
            read_hklm_string(
                MACHINE_ENVIRONMENT_KEY,
                "CHRONOGAZER_ELEV_TEST_VALUE_THAT_DOES_NOT_EXIST"
            ),
            Ok(None)
        );
        // `Path` は REG_EXPAND_SZ で `%SystemRoot%` を含むことが多い。展開しないこと。
        let path = read_hklm_string(MACHINE_ENVIRONMENT_KEY, "Path")
            .unwrap()
            .expect("マシンの Path");
        assert!(!path.is_empty());
    }

    // --- grant-data-acl を本物の ACL の操作で（一時ディレクトリだけ） ------------------

    /// ルートは一時ディレクトリ、インストール利用者はテストの利用者。フォルダの操作
    /// （辿らずに開く・作る・書き直す・利用者のアカウントの確かめ）だけ本物
    /// （`WindowsOps`）で、サービス・グループ・HKLM には触らない（呼ばれたら panic）。
    struct TempRootOps {
        root: PathBuf,
    }

    impl ElevOps for TempRootOps {
        type Pinned = PinnedDir;

        fn server_executable(&self) -> Result<PathBuf, String> {
            unreachable!("テストでは呼ばない")
        }
        fn program_files_dirs(&self) -> Vec<PathBuf> {
            Vec::new()
        }
        fn registered_command_line(&self) -> Result<Option<String>, String> {
            unreachable!("テストでは呼ばない")
        }
        fn machine_root(&self) -> Result<PathBuf, String> {
            Ok(self.root.clone())
        }
        fn current_user_sid(&self) -> Result<InstallUserSid, String> {
            WindowsOps.current_user_sid()
        }
        fn check_user_account(&self, sid: &InstallUserSid) -> Result<(), String> {
            WindowsOps.check_user_account(sid)
        }
        fn install(
            &self,
            _: PathBuf,
            _: InstallUserSid,
            _: &DataLayout,
        ) -> Result<InstallReport, InstallUserUnavailable> {
            unreachable!("テストでは呼ばない")
        }
        fn uninstall(&self) -> Result<UninstallOutcome, UninstallError> {
            unreachable!("テストでは呼ばない")
        }
        fn read_install_user(&self) -> Result<Option<String>, String> {
            Ok(Some(WindowsOps.current_user_sid()?.as_str().to_string()))
        }
        fn ensure_group(&self) -> Result<GroupOutcome, ElevError> {
            unreachable!("テストでは呼ばない")
        }
        fn add_member(&self, _: &InstallUserSid) -> Result<MemberOutcome, ElevError> {
            unreachable!("テストでは呼ばない")
        }
        fn grant_service_acl(&self) -> Result<(), ElevError> {
            unreachable!("テストでは呼ばない")
        }
        fn set_autostart(&self, _: &Path, _: bool) -> Result<(), ServiceManagerError> {
            unreachable!("テストでは呼ばない")
        }
        fn pin_dir(&self, dir: &Path) -> Result<Option<PinnedDir>, String> {
            WindowsOps.pin_dir(dir)
        }
        fn create_dir(&self, dir: &Path, access: &DirAccess) -> Result<bool, String> {
            WindowsOps.create_dir(dir, access)
        }
    }

    fn junction(link: &Path, target: &Path) {
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .stdout(std::process::Stdio::null())
            .status()
            .expect("cmd を起動できない");
        assert!(status.success(), "mklink /J に失敗した");
    }

    fn grant(root: &Path) -> (i32, Vec<banto_service_control::ConsoleLine>) {
        let mut lines = Vec::new();
        let code = crate::run(
            &crate::Action::GrantDataAcl,
            &TempRootOps {
                root: root.to_path_buf(),
            },
            &mut |line| lines.push(line),
        );
        (code, lines)
    }

    /// 無いルートは作り、`secrets\` も作る（`hub-keys\` は作らない）。2 回目は書き直す
    /// だけ（冪等）。どちらも期待の保護された DACL になる。
    #[test]
    fn grant_data_acl_creates_and_resets_in_a_temp_root() {
        let me = WindowsOps.current_user_sid().unwrap();
        let access = DirAccess::with_install_user(me);
        let parent = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(parent.path().join("ChronoGazer"));
        for _ in 0..2 {
            let (code, lines) = grant(layout.root());
            assert_eq!(code, crate::EXIT_OK, "{lines:#?}");
            for dir in [layout.root().to_path_buf(), layout.secrets_dir()] {
                let dacl = banto_secret_store::acl::read_dir_dacl(&dir).unwrap();
                banto_secret_store::acl::check_dacl(&dacl, &access)
                    .unwrap_or_else(|err| panic!("{}: {err:?}", dir.display()));
            }
            assert!(!layout.hub_keys_dir().exists());
        }
    }

    /// 反証（実物）: ルートの中のジャンクションの先（ルートの外）の DACL は、
    /// grant-data-acl の書き直しで変わらない。
    #[test]
    fn grant_data_acl_does_not_rewrite_outside_a_child_junction() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("ChronoGazer");
        std::fs::create_dir(&root).unwrap();
        let outside_parent = tempfile::tempdir().unwrap();
        let outside = outside_parent.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("file.txt"), b"x").unwrap();
        junction(&root.join("link"), &outside);
        let before = (
            banto_secret_store::acl::read_dir_dacl(&outside).unwrap(),
            banto_secret_store::acl::read_dir_dacl(&outside.join("file.txt")).unwrap(),
        );
        let (code, lines) = grant(&root);
        assert_eq!(code, crate::EXIT_OK, "{lines:#?}");
        let after = (
            banto_secret_store::acl::read_dir_dacl(&outside).unwrap(),
            banto_secret_store::acl::read_dir_dacl(&outside.join("file.txt")).unwrap(),
        );
        assert_eq!(after, before);
        std::fs::remove_dir(root.join("link")).unwrap();
    }

    /// 反証（実物）: ルートそのものがジャンクションなら断り（1）、先の DACL も
    /// 変えず、`secrets\` も作らない。
    #[test]
    fn grant_data_acl_refuses_a_junction_root() {
        let parent = tempfile::tempdir().unwrap();
        let outside_parent = tempfile::tempdir().unwrap();
        let outside = outside_parent.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let root = parent.path().join("ChronoGazer");
        junction(&root, &outside);
        let before = banto_secret_store::acl::read_dir_dacl(&outside).unwrap();
        let (code, lines) = grant(&root);
        assert_eq!(code, crate::EXIT_FAILED, "{lines:#?}");
        assert!(lines.iter().any(|line| matches!(
            line,
            banto_service_control::ConsoleLine::Stderr(text) if text.contains("reparse point")
        )));
        assert_eq!(
            banto_secret_store::acl::read_dir_dacl(&outside).unwrap(),
            before
        );
        assert!(!outside.join("secrets").exists());
        std::fs::remove_dir(&root).unwrap();
    }
}
