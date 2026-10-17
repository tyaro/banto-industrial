//! [`crate::ElevOps`] の Windows の実装（本番）。**テストからは呼ばない**（本物の
//! サービス・ローカルグループ・HKLM・ProgramData を変えてしまうため）。読むだけの
//! マシンの環境の読み取り（[`RegistryMachineEnvironment`]）も、テストは偽物を使う。

use std::path::{Path, PathBuf};

use banto_safe_root::acl::{PinnedDir, PinnedDirInfo};
use banto_safe_root::{DirAccess, InstallUserSid};
use banto_service_control::{
    query_service_command_line, read_service_parameter, uninstall_service, ServiceManager,
    ServiceManagerError, UninstallError, UninstallOutcome, WindowsServiceManager,
};
use banto_service_elev::{ElevError, GroupOutcome, MemberOutcome, ServiceAclOutcome};
use chronogazer_core::paths::DataLayout;
use chronogazer_core::server_service::{
    install_with, InstallReport, InstallUserUnavailable, INSTALL_USER_SID_VALUE,
};
use chronogazer_core::shell_launch::{chronogazer_service_spec, SERVICE_NAME};

/// マシンの環境を HKLM と既知のフォルダから読む（このプロセスの環境変数は読まない）。
/// 実装は `banto_safe_root::guard::win`（#626 PR-A で移した）。
pub use banto_safe_root::guard::win::RegistryMachineEnvironment;

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
        recurse: banto_safe_root::acl::Recurse,
    ) -> Result<banto_safe_root::acl::ResetSkips, String> {
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
        banto_safe_root::guard::win::program_files_dirs()
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
        banto_safe_root::acl::current_user_sid().map_err(|err| err.to_string())
    }

    fn check_user_account(&self, sid: &InstallUserSid) -> Result<(), AccountCheck> {
        banto_safe_root::acl::check_user_account(sid).map_err(|err| match err {
            // `ERROR_NONE_MAPPED`・グループなど = アカウントでない（使い方の誤り）。
            banto_safe_root::SafeRootError::InvalidUserSid(_) => {
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

    fn grant_service_acl(&self) -> Result<ServiceAclOutcome, ElevError> {
        banto_service_elev::grant_service_operators(SERVICE_NAME, &OPERATORS_GROUP)
    }

    fn set_autostart(&self, executable: &Path, enabled: bool) -> Result<(), ServiceManagerError> {
        WindowsServiceManager::for_spec(&chronogazer_service_spec(executable.to_path_buf()))
            .set_auto_start(enabled)
    }

    fn pin_dir(&self, dir: &Path) -> Result<Option<PinnedDir>, String> {
        banto_safe_root::acl::pin_dir(dir).map_err(|err| err.to_string())
    }

    fn create_child(
        &self,
        parent: &PinnedDir,
        name: &str,
        access: &DirAccess,
    ) -> Result<Option<PinnedDir>, String> {
        parent
            .create_child_dir(name, Some(access))
            .map_err(|err| err.to_string())
    }

    fn open_child(&self, parent: &PinnedDir, name: &str) -> Result<Option<PinnedDir>, String> {
        parent.open_child_dir(name).map_err(|err| err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 読むだけ: マシンの ProgramData は既知のフォルダから取れる（環境変数を見ない）。
    /// HKLM の読み取りそのもののテストは `banto_safe_root::guard::win` に移した
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
        /// 子を作る直前に 1 回だけ走るフック（確かめた親のその場のすり替え。#642）。
        before_create: std::cell::RefCell<Option<Box<dyn FnOnce()>>>,
    }

    impl TempRootOps {
        fn new(root: &Path) -> Self {
            Self {
                root: root.to_path_buf(),
                before_create: std::cell::RefCell::new(None),
            }
        }
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
        fn grant_service_acl(&self) -> Result<ServiceAclOutcome, ElevError> {
            unreachable!("テストでは呼ばない")
        }
        fn set_autostart(&self, _: &Path, _: bool) -> Result<(), ServiceManagerError> {
            unreachable!("テストでは呼ばない")
        }
        fn pin_dir(&self, dir: &Path) -> Result<Option<PinnedDir>, String> {
            WindowsOps.pin_dir(dir)
        }
        fn create_child(
            &self,
            parent: &PinnedDir,
            name: &str,
            access: &DirAccess,
        ) -> Result<Option<PinnedDir>, String> {
            if let Some(hook) = self.before_create.borrow_mut().take() {
                hook();
            }
            WindowsOps.create_child(parent, name, access)
        }
        fn open_child(&self, parent: &PinnedDir, name: &str) -> Result<Option<PinnedDir>, String> {
            WindowsOps.open_child(parent, name)
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
            &TempRootOps::new(root),
            &mut |line| lines.push(line),
        );
        (code, lines)
    }

    fn grant_with(ops: &TempRootOps) -> (i32, Vec<banto_service_control::ConsoleLine>) {
        let mut lines = Vec::new();
        let code = crate::run(&crate::Action::GrantDataAcl, ops, &mut |line| {
            lines.push(line)
        });
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
                let dacl = banto_safe_root::acl::read_dir_dacl(&dir).unwrap();
                banto_safe_root::acl::check_dacl(&dacl, &access)
                    .unwrap_or_else(|err| panic!("{}: {err:?}", dir.display()));
                // 監査 B5: 所有者は Administrators に移る（インストール利用者の「変更」は残る）。
                assert_eq!(
                    dacl.owner,
                    banto_safe_root::acl::ADMINISTRATORS_SID,
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
        // banto-safe-root の `reset_acl_*` のテストが固定する（elev はそれを本物の
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
            banto_safe_root::acl::read_dir_dacl(&outside).unwrap(),
            banto_safe_root::acl::read_dir_dacl(&outside.join("file.txt")).unwrap(),
        );
        let (code, lines) = grant(&root);
        assert_eq!(code, crate::EXIT_OK, "{lines:#?}");
        let after = (
            banto_safe_root::acl::read_dir_dacl(&outside).unwrap(),
            banto_safe_root::acl::read_dir_dacl(&outside.join("file.txt")).unwrap(),
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
        let before = banto_safe_root::acl::read_dir_dacl(&outside).unwrap();
        let (code, lines) = grant(&root);
        assert_eq!(code, crate::EXIT_FAILED, "{lines:#?}");
        assert!(lines.iter().any(|line| matches!(
            line,
            banto_service_control::ConsoleLine::Stderr(text) if text.contains("reparse point")
        )));
        assert_eq!(
            banto_safe_root::acl::read_dir_dacl(&outside).unwrap(),
            before
        );
        assert!(!outside.join("secrets").exists());
        std::fs::remove_dir(&root).unwrap();
    }

    // --- #642: 無いルートを確かめ済みの親の下だけに作る（#626 PR-B の Hub と同じ形） -----

    fn has_line(lines: &[banto_service_control::ConsoleLine], needle: &str) -> bool {
        lines.iter().any(|line| match line {
            banto_service_control::ConsoleLine::Stdout(text)
            | banto_service_control::ConsoleLine::Stderr(text) => text.contains(needle),
        })
    }

    fn listing(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn snapshot(paths: &[PathBuf]) -> Vec<banto_safe_root::acl::ObservedDacl> {
        paths
            .iter()
            .map(|p| banto_safe_root::acl::read_dir_dacl(p).unwrap())
            .collect()
    }

    /// 外の対象（継承を受け入れる DACL）: `outside`・`outside\f.txt`・`outside\data`。
    /// 保護された対象では、書き換えがあっても変わらず偽の緑になるので、最初に確かめる。
    fn make_outside(parent: &Path) -> (PathBuf, Vec<PathBuf>) {
        let outside = parent.join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("f.txt"), b"x").unwrap();
        std::fs::create_dir(outside.join("data")).unwrap();
        let watched = vec![outside.clone(), outside.join("f.txt"), outside.join("data")];
        for path in &watched {
            assert!(
                !banto_safe_root::acl::read_dir_dacl(path).unwrap().protected,
                "外の対象は継承を受け入れる DACL にする: {}",
                path.display()
            );
        }
        (outside, watched)
    }

    /// 反証（実物）: ルートが無く、その親（途中の要素）が外へのジャンクションのとき、外に
    /// 何も作らず、外の一覧と DACL も変えずに断る（以前はパスで作ってから確かめていたので、
    /// 断っても外にフォルダが残った。管理者でなくても走る）。(a) ルートの親そのものが
    /// ジャンクション、(b) ルートの親の親がジャンクション。
    #[test]
    fn a_missing_root_behind_a_junction_creates_nothing_outside() {
        let parent = tempfile::tempdir().unwrap();
        let base = long(parent.path());
        let (outside, watched) = make_outside(&base);
        let before = (
            snapshot(&watched),
            listing(&outside),
            listing(&outside.join("data")),
        );
        let via = base.join("via");
        junction(&via, &outside);
        for (root, needle) in [
            (via.join("ChronoGazer"), "reparse point"),
            (via.join("data").join("ChronoGazer"), "本当の場所"),
        ] {
            let (code, lines) = grant(&root);
            assert_eq!(code, crate::EXIT_FAILED, "{lines:#?}");
            assert!(has_line(&lines, needle), "{lines:#?}");
            assert!(has_line(&lines, "何も作らずに止めます"), "{lines:#?}");
            assert_eq!(
                (
                    snapshot(&watched),
                    listing(&outside),
                    listing(&outside.join("data"))
                ),
                before,
                "{}",
                root.display()
            );
        }
        std::fs::remove_dir(&via).unwrap();
    }

    /// ルートの親も無ければ、何も作らずに断る（前提が無い = 2。親から作ることはしない）。
    #[test]
    fn a_missing_root_parent_creates_nothing() {
        let parent = tempfile::tempdir().unwrap();
        let base = long(parent.path());
        let root = base.join("missing").join("ChronoGazer");
        let (code, lines) = grant(&root);
        assert_eq!(code, crate::EXIT_PRECONDITION, "{lines:#?}");
        assert!(has_line(&lines, "親から作ることはしません"), "{lines:#?}");
        assert_eq!(listing(&base), Vec::<String>::new());
    }

    /// ルートの親を確かめた後・ルートを作る直前に、親をその場で外へのジャンクションに
    /// 変えても（FSCTL）、親のハンドルを基準に作るので外には何もできない（パスで作ると
    /// 外にできる。管理者でなくても走る）。
    #[test]
    fn a_root_parent_swapped_before_create_does_not_leak() {
        let parent = tempfile::tempdir().unwrap();
        let base = long(parent.path());
        let (outside, watched) = make_outside(&base);
        let before = (snapshot(&watched), listing(&outside));
        let root_parent = base.join("rp");
        std::fs::create_dir(&root_parent).unwrap();
        let ops = TempRootOps::new(&root_parent.join("ChronoGazer"));
        let swapped = std::rc::Rc::new(std::cell::Cell::new(false));
        {
            let (root_parent, outside, swapped) =
                (root_parent.clone(), outside.clone(), swapped.clone());
            *ops.before_create.borrow_mut() = Some(Box::new(move || {
                swapped.set(set_reparse_to(&root_parent, &outside).is_ok());
            }));
        }
        let (code, lines) = grant_with(&ops);
        if !swapped.get() {
            if std::env::var_os("CI").is_some() {
                panic!("CI で FSCTL_SET_REPARSE_POINT が失敗した");
            }
            eprintln!("skip: FSCTL_SET_REPARSE_POINT に権限が無い - CI で確かめる");
            return;
        }
        // 断った理由は「作れませんでした」（親のハンドルを基準にした作成が、すり替えられた
        // 親では失敗する）。成功扱い（0）や別の理由（前提が無い 2・確かめの失敗）ではない（#646 Nit 4）。
        assert_eq!(code, crate::EXIT_FAILED, "{lines:#?}");
        assert!(has_line(&lines, "を作れませんでした"), "{lines:#?}");
        assert!(has_line(&lines, "NtCreateFile"), "{lines:#?}");
        assert!(has_line(&lines, "ChronoGazer"), "{lines:#?}");
        assert_eq!((snapshot(&watched), listing(&outside)), before);
        assert!(!outside.join("ChronoGazer").exists());
        let _ = std::fs::remove_dir(&root_parent);
    }

    /// `dir`（空のフォルダ）を`target`へのジャンクションにする（`FSCTL_SET_REPARSE_POINT`。
    /// テスト専用 - banto-hub-core の `profile_grant::win` のテストの同名の関数と同じ）。
    fn set_reparse_to(dir: &Path, target: &Path) -> std::io::Result<()> {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_WRITE, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
            FILE_SHARE_WRITE, OPEN_EXISTING,
        };
        use windows_sys::Win32::System::Ioctl::FSCTL_SET_REPARSE_POINT;
        use windows_sys::Win32::System::IO::DeviceIoControl;

        let sub: Vec<u16> = std::ffi::OsStr::new(r"\??\")
            .encode_wide()
            .chain(target.as_os_str().encode_wide())
            .collect();
        let sub_bytes = (sub.len() * 2) as u16;
        let mut buf: Vec<u8> = Vec::new();
        buf.extend_from_slice(&0xA000_0003u32.to_le_bytes()); // IO_REPARSE_TAG_MOUNT_POINT
        let data_len = 8 + sub_bytes as usize + 2 + 2;
        buf.extend_from_slice(&(data_len as u16).to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // Reserved
        buf.extend_from_slice(&0u16.to_le_bytes()); // SubstituteNameOffset
        buf.extend_from_slice(&sub_bytes.to_le_bytes()); // SubstituteNameLength
        buf.extend_from_slice(&(sub_bytes + 2).to_le_bytes()); // PrintNameOffset
        buf.extend_from_slice(&0u16.to_le_bytes()); // PrintNameLength
        for w in &sub {
            buf.extend_from_slice(&w.to_le_bytes());
        }
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());

        let path: Vec<u16> = dir
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: `path`は NUL 終端。
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error());
        }
        let mut returned = 0u32;
        // SAFETY: `handle`は有効、`buf`は reparse データ。
        let ok = unsafe {
            DeviceIoControl(
                handle,
                FSCTL_SET_REPARSE_POINT,
                buf.as_ptr().cast(),
                buf.len() as u32,
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        let err = std::io::Error::last_os_error();
        // SAFETY: `handle`は CreateFileW が返したもの。
        unsafe { CloseHandle(handle) };
        if ok == 0 {
            return Err(err);
        }
        Ok(())
    }
}
