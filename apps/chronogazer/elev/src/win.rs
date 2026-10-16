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
use chronogazer_core::paths::DataLayout;
use chronogazer_core::server_service::{
    install_with, InstallReport, InstallUserUnavailable, INSTALL_USER_SID_VALUE,
};
use chronogazer_core::shell_launch::{chronogazer_service_spec, SERVICE_NAME};

/// マシンの環境を HKLM と既知のフォルダから読む（このプロセスの環境変数は読まない）。
/// 実装は `banto_secret_store::acl::guard::win`（#626 PR-A で移した）。
pub use banto_secret_store::acl::guard::win::RegistryMachineEnvironment;

use crate::{
    resolve_machine_root, AccountCheck, ElevOps, KnownFolders, MachineEnvironment, PinnedDirOps,
    OPERATORS_GROUP, SERVER_EXE_FILE_NAME,
};

/// 本番の操作。
pub struct WindowsOps;

impl PinnedDirOps for PinnedDir {
    fn info(&self) -> PinnedDirInfo {
        PinnedDir::info(self).clone()
    }

    fn final_path(&self) -> PathBuf {
        PinnedDir::final_path(self).to_path_buf()
    }

    fn reset_acl(
        &self,
        access: &DirAccess,
        recurse: banto_secret_store::acl::Recurse,
    ) -> Result<banto_secret_store::acl::ResetSkips, String> {
        PinnedDir::reset_acl(self, access, recurse).map_err(|err| err.to_string())
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
        banto_secret_store::acl::guard::win::program_files_dirs()
    }

    fn registered_command_line(&self) -> Result<Option<String>, String> {
        query_service_command_line(SERVICE_NAME).map_err(|err| err.to_string())
    }

    fn machine_root(&self) -> Result<PathBuf, String> {
        resolve_machine_root(&RegistryMachineEnvironment)
    }

    fn known_folders(&self) -> Result<KnownFolders, String> {
        RegistryMachineEnvironment.known_folders()
    }

    fn current_user_sid(&self) -> Result<InstallUserSid, String> {
        banto_secret_store::acl::current_user_sid().map_err(|err| err.to_string())
    }

    fn check_user_account(&self, sid: &InstallUserSid) -> Result<(), AccountCheck> {
        banto_secret_store::acl::check_user_account(sid).map_err(|err| match err {
            // `ERROR_NONE_MAPPED`・グループなど = アカウントでない（使い方の誤り）。
            banto_secret_store::SecretStoreError::InvalidUserSid(_) => {
                AccountCheck::NotAnAccount(err.to_string())
            }
            // RPC 不達・アクセス拒否など = 一時的な失敗。
            _ => AccountCheck::Transient(err.to_string()),
        })
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
    /// HKLM の読み取りそのもののテストは `banto_secret_store::acl::guard::win` に移した
    /// （#626 PR-A）。
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
        fn known_folders(&self) -> Result<KnownFolders, String> {
            RegistryMachineEnvironment.known_folders()
        }
        fn current_user_sid(&self) -> Result<InstallUserSid, String> {
            WindowsOps.current_user_sid()
        }
        fn check_user_account(&self, sid: &InstallUserSid) -> Result<(), AccountCheck> {
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

    /// 昇格した管理者として走っているか。書き直しは所有者を Administrators に移す
    /// （監査 B5）ので、昇格していなければ失敗する。そのときは理由を出して skip する
    /// （GitHub の Windows のランナーは管理者なので CI では走る）。
    fn elevated_admin(test: &str) -> bool {
        let admin = banto_service_elev::is_current_process_admin().unwrap_or(false);
        // CI で管理者でなければ反証が黙って飛ばされるので落とす（#626 PR-B の監査 B-3）。
        if !admin && std::env::var_os("CI").is_some() {
            panic!("{test}: CI なのに昇格した管理者ではない");
        }
        if !admin {
            eprintln!("skip {test}: 昇格した管理者ではないので、所有者を Administrators に移せない（CI で確かめる）");
        }
        admin
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

    /// tempfile のパスは 8.3 の短い名前（CI のランナーは `C:\Users\RUNNER~1\…`）のことが
    /// あり、`verify_final_path` が本当のパス（`runneradmin`）と食い違って断る。fixture の
    /// 期待のルートは、作った親の**本当のパス**（`canonicalize` から `\\?\` を外す）で
    /// 組み立てる（本番の短い名前を断る確かめは変えない。CI の 8.3 短縮名、再々レビュー）。
    fn long(path: &Path) -> PathBuf {
        let c = std::fs::canonicalize(path).unwrap();
        PathBuf::from(c.to_string_lossy().trim_start_matches(r"\\?\").to_string())
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
        if !elevated_admin("grant_data_acl_creates_and_resets_in_a_temp_root") {
            return;
        }
        let me = WindowsOps.current_user_sid().unwrap();
        let access = DirAccess::with_install_user(me);
        let parent = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(long(parent.path()).join("ChronoGazer"));
        for _ in 0..2 {
            let (code, lines) = grant(layout.root());
            assert_eq!(code, crate::EXIT_OK, "{lines:#?}");
            for dir in [layout.root().to_path_buf(), layout.secrets_dir()] {
                let dacl = banto_secret_store::acl::read_dir_dacl(&dir).unwrap();
                banto_secret_store::acl::check_dacl(&dacl, &access)
                    .unwrap_or_else(|err| panic!("{}: {err:?}", dir.display()));
                // 監査 B5: 所有者は Administrators に移る（インストール利用者の「変更」は残る）。
                assert_eq!(
                    dacl.owner,
                    banto_secret_store::acl::ADMINISTRATORS_SID,
                    "{}",
                    dir.display()
                );
            }
            assert!(!layout.hub_keys_dir().exists());
        }
    }

    /// 反証（実物）: ルートの中のジャンクションの先（ルートの外）の DACL は、
    /// grant-data-acl の書き直しで変わらない。
    #[test]
    fn grant_data_acl_does_not_rewrite_outside_a_child_junction() {
        // 自前の再帰がジャンクション・ハードリンクを辿らないことそのものは
        // banto-secret-store の `reset_acl_*` のテストが固定する（elev はそれを本物の
        // grant-data-acl で一通り通す）。
        if !elevated_admin("grant_data_acl_does_not_rewrite_outside_a_child_junction") {
            return;
        }
        let parent = tempfile::tempdir().unwrap();
        let root = long(parent.path()).join("ChronoGazer");
        std::fs::create_dir(&root).unwrap();
        let outside_parent = tempfile::tempdir().unwrap();
        let outside = long(outside_parent.path()).join("outside");
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
        let outside = long(outside_parent.path()).join("outside");
        std::fs::create_dir(&outside).unwrap();
        let root = long(parent.path()).join("ChronoGazer");
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
