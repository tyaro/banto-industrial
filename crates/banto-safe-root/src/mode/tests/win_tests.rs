//! [`crate::mode`] の Windows の実物テスト（設計 §8 PR-1c の 1〜11）。
//!
//! - ジャンクション・ハードリンク・古いハンドルの筋は権限なしで作れるので必ず走る。
//! - **所有者を Administrators に移す**（`close_tree`・`relax_tree`・`ensure_marker`）のは
//!   昇格した管理者・SYSTEM だけなので、それらは `is_elevated_admin()`（`CI` なら管理者で
//!   なければ panic、手元は skip）の後ろ。
//! - 「一般の利用者の代わり」は、今のトークンから `CreateRestrictedToken`（Administrators を
//!   deny-only、特権なし）で作ったトークンを**このスレッドで impersonate** して使う
//!   （[`as_limited_user`]）。管理者（CI）でも、締めた木に名前を作れない・DACL を変えられない・
//!   読める、を確かめられる。
//! - 外（ルートの外の一時フォルダ）の記述子・中身が変わらないことを前後で比べる。

use std::path::{Path, PathBuf};

use super::*;
use crate::acl::tests::windows::{is_elevated_admin, junction, me, set_reparse_to};
use crate::acl::{
    check_dacl, exposure, inspect_file, pin_dir, read_dir_dacl, reset_dir_acl, FileSecurity,
    PinnedDir,
};
use windows_sys::Win32::Foundation::HANDLE;

/// `IO_REPARSE_TAG_MOUNT_POINT`（ジャンクション）。
const TAG_MOUNT_POINT: u32 = 0xA000_0003;
/// `ERROR_ACCESS_DENIED`。
const ACCESS_DENIED: i32 = 5;
/// `ERROR_DIR_NOT_EMPTY`。
const DIR_NOT_EMPTY: i32 = 145;
/// Administrators に拒否・SYSTEM にフル（保護）。所有者の暗黙の READ_CONTROL・WRITE_DAC は残る。
const DENY_ADMINS_SDDL: &str = "D:P(D;;FA;;;BA)(A;;FA;;;SY)";

fn mode() -> ModeAccess {
    ModeAccess::for_users([me()]).unwrap()
}

fn no_stop() -> StopSignal {
    StopSignal::None
}

fn pinned(path: &Path) -> PinnedDir {
    pin_dir(path).unwrap().unwrap()
}

/// 木の全部の名前（ルートを含む。reparse point の中へは下りない）と、辿らずに読んだ記述子。
fn tree_snapshot(root: &Path) -> Vec<(PathBuf, Option<FileSecurity>)> {
    fn walk(dir: &Path, out: &mut Vec<(PathBuf, Option<FileSecurity>)>) {
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        names.sort();
        for path in names {
            // 開けない子（拒否の DACL）は `None`（前後で同じく `None` なら変わっていない）。
            let security = inspect_file(&path).ok().flatten();
            let descend = security
                .as_ref()
                .is_some_and(|s| s.is_dir && !s.is_reparse_point);
            out.push((path.clone(), security));
            if descend {
                walk(&path, out);
            }
        }
    }
    let mut out = vec![(root.to_path_buf(), inspect_file(root).ok().flatten())];
    walk(root, &mut out);
    out
}

/// ファイル用の形（継承のフラグ無し）を、フォルダ用の `check_dacl` の形に読み替えて確かめる。
fn check_file_shape(security: &FileSecurity, access: &DirAccess, what: &Path) {
    let mut as_dir = security.dacl.clone();
    for ace in as_dir.aces.as_mut().expect("DACL がある") {
        assert_eq!(
            ace.flags,
            0,
            "{}: ファイルの ACE に継承のフラグ",
            what.display()
        );
        ace.flags = INHERIT_FLAGS;
    }
    check_dacl(&as_dir, access)
        .unwrap_or_else(|e| panic!("{}: {e:?}: {security:?}", what.display()));
}

/// `path` が、その親フォルダの印の席（OS が `MARKER_NAME` に解決する物。綴りではなくファイル
/// ID で見分ける - 実装と同じ）か。
fn is_marker_slot(path: &Path) -> bool {
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    let Some(parent) = path.parent() else {
        return false;
    };
    let parent = pinned(parent);
    let Some(slot) = crate::acl::imp::marker_entry(parent.raw_handle()).unwrap() else {
        return false;
    };
    let own = open_dir(
        path,
        FILE_READ_ATTRIBUTES,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
    )
    .unwrap();
    crate::acl::imp::file_id_of(own.0).unwrap() == slot.file_id
}

/// 木の全体が `access` の形（フォルダは OICI、ファイルは継承無し、所有者 Administrators、印の
/// 席の物（綴りを問わず）は利用者の ACE なし）であることを確かめる。
fn expect_tree_shape(root: &Path, access: &DirAccess) {
    for (path, security) in tree_snapshot(root) {
        let security = security.unwrap_or_else(|| panic!("{} を読めない", path.display()));
        assert_eq!(
            security.dacl.owner,
            ADMINISTRATORS_SID,
            "{}: 所有者",
            path.display()
        );
        if is_marker_slot(&path) && !security.is_dir {
            assert!(
                exposure(&security).is_empty(),
                "{}: 印に利用者の ACE: {security:?}",
                path.display()
            );
        } else if security.is_dir {
            check_dacl(&security.dacl, access)
                .unwrap_or_else(|e| panic!("{}: {e:?}: {security:?}", path.display()));
        } else {
            check_file_shape(&security, access, &path);
        }
    }
}

/// 試験用の木: `root/{a.txt, .banto-safe-root, data/{d1.txt, d2.txt, .banto-safe-root, deep/x.txt},
/// logs/l.txt}`。
fn make_tree(root: &Path) {
    std::fs::create_dir(root).unwrap();
    std::fs::write(root.join("a.txt"), b"a").unwrap();
    std::fs::write(root.join(MARKER_NAME), b"").unwrap();
    std::fs::create_dir(root.join("data")).unwrap();
    std::fs::write(root.join("data").join("d1.txt"), b"1").unwrap();
    std::fs::write(root.join("data").join("d2.txt"), b"2").unwrap();
    std::fs::write(root.join("data").join(MARKER_NAME), b"").unwrap();
    std::fs::create_dir(root.join("data").join("deep")).unwrap();
    std::fs::write(root.join("data").join("deep").join("x.txt"), b"x").unwrap();
    std::fs::create_dir(root.join("logs")).unwrap();
    std::fs::write(root.join("logs").join("l.txt"), b"l").unwrap();
}

/// `make_tree` の物の数（ルートを含む）。
const TREE_OBJECTS: u64 = 11;
/// 2 周目の後の物の数: `deep`・`logs` にも印が置かれる（+2）。
const CLOSED_OBJECTS: u64 = TREE_OBJECTS + 2;

/// 木のすべてのフォルダに印のファイル（ファイル・リンク数 1・利用者の ACE なし）があること。
fn every_dir_has_a_marker(root: &Path) {
    for (path, security) in tree_snapshot(root) {
        let security = security.unwrap_or_else(|| panic!("{} を読めない", path.display()));
        if security.is_dir && !security.is_reparse_point {
            let marker = inspect_file(&path.join(MARKER_NAME))
                .unwrap()
                .unwrap_or_else(|| panic!("{} に印が無い", path.display()));
            assert!(!marker.is_dir && marker.links == 1 && !marker.is_reparse_point);
            assert!(
                exposure(&marker).is_empty(),
                "{}: {marker:?}",
                path.display()
            );
        }
    }
}

fn outside_with_victim() -> (tempfile::TempDir, PathBuf) {
    let outside = tempfile::tempdir().unwrap();
    let victim = outside.path().join("victim.txt");
    std::fs::write(&victim, b"outside").unwrap();
    (outside, victim)
}

fn wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// `path` のフォルダを `access`・`share` で開く（辿らない・バックアップの意図なし）。
fn open_dir(path: &Path, access: u32, share: u32) -> std::io::Result<Owned> {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, OPEN_EXISTING,
    };
    let wide = wide(path);
    // SAFETY: NUL 終端のパス。
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            access,
            share,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    Ok(Owned(handle))
}

/// テスト用の閉じるハンドル。
struct Owned(HANDLE);
// SAFETY: カーネルのハンドルはどのスレッドからでも使える（見張りのスレッドへ渡すため）。
unsafe impl Send for Owned {}
impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: 開いた有効なハンドル。
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}

/// 開いているフォルダのハンドル `handle` に、`target` へのジャンクションを設定する
/// （`FSCTL_SET_REPARSE_POINT`。古いハンドルの筋）。
fn set_reparse_on_handle(handle: HANDLE, target: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::Ioctl::FSCTL_SET_REPARSE_POINT;
    use windows_sys::Win32::System::IO::DeviceIoControl;
    let sub: Vec<u16> = std::ffi::OsStr::new(r"\??\")
        .encode_wide()
        .chain(target.as_os_str().encode_wide())
        .collect();
    let sub_bytes = (sub.len() * 2) as u16;
    let mut buf: Vec<u8> = Vec::new();
    buf.extend_from_slice(&TAG_MOUNT_POINT.to_le_bytes());
    let data_len = 8 + sub_bytes as usize + 2 + 2;
    buf.extend_from_slice(&(data_len as u16).to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&sub_bytes.to_le_bytes());
    buf.extend_from_slice(&(sub_bytes + 2).to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    for w in &sub {
        buf.extend_from_slice(&w.to_le_bytes());
    }
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    let mut returned = 0u32;
    // SAFETY: `handle` は有効、`buf` は reparse データ。
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
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// `parent`（開いているフォルダのハンドル）を `RootDirectory` に、名前 1 要素のファイルを
/// `NtCreateFile` で作る（古いハンドルを基準にした相対の作成の筋）。失敗は Win32 のエラー番号。
fn create_relative(parent: HANDLE, name: &str) -> Result<(), u32> {
    use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
    use windows_sys::Wdk::Storage::FileSystem::{
        NtCreateFile, FILE_CREATE, FILE_NON_DIRECTORY_FILE, FILE_SYNCHRONOUS_IO_NONALERT,
    };
    use windows_sys::Win32::Foundation::{
        CloseHandle, RtlNtStatusToDosError, OBJ_CASE_INSENSITIVE, UNICODE_STRING,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_WRITE, FILE_SHARE_READ, SYNCHRONIZE,
    };
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;
    let wide_name: Vec<u16> = name.encode_utf16().collect();
    let unicode = UNICODE_STRING {
        Length: (wide_name.len() * 2) as u16,
        MaximumLength: (wide_name.len() * 2) as u16,
        Buffer: wide_name.as_ptr() as *mut u16,
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent,
        ObjectName: &unicode,
        Attributes: OBJ_CASE_INSENSITIVE,
        SecurityDescriptor: std::ptr::null(),
        SecurityQualityOfService: std::ptr::null(),
    };
    let mut handle: HANDLE = std::ptr::null_mut();
    // SAFETY: 全フィールドが整数・ポインタの構造体。
    let mut status_block: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
    // SAFETY: `attributes` の名前は呼び出しの間生きている。`parent` は開いているハンドル。
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            FILE_GENERIC_WRITE | DELETE | SYNCHRONIZE,
            &attributes,
            &mut status_block,
            std::ptr::null(),
            FILE_ATTRIBUTE_NORMAL,
            FILE_SHARE_READ,
            FILE_CREATE,
            FILE_NON_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT,
            std::ptr::null(),
            0,
        )
    };
    if status < 0 {
        // SAFETY: NTSTATUS を Win32 のエラーに写すだけ。
        return Err(unsafe { RtlNtStatusToDosError(status) });
    }
    // SAFETY: 作った有効なハンドル。
    unsafe { CloseHandle(handle) };
    Ok(())
}

/// **一般の利用者の代わり**: 今のプロセスのトークンから、Administrators を deny-only にし
/// 特権をすべて外した制限付きトークン（`CreateRestrictedToken`）を作り、**このスレッドで
/// impersonate** して `f` を走らせる（設計 §8 PR-1c「一般の利用者の代わり（CI）」）。利用者の
/// SID は同じなので、利用者への明示の ACE だけが効く。終わったら `RevertToSelf`。
fn as_limited_user<T>(f: impl FnOnce() -> T) -> T {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Security::Authorization::ConvertStringSidToSidW;
    use windows_sys::Win32::Security::{
        CreateRestrictedToken, ImpersonateLoggedOnUser, RevertToSelf, DISABLE_MAX_PRIVILEGE,
        SID_AND_ATTRIBUTES, TOKEN_ALL_ACCESS,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: 今のプロセスの疑似ハンドル。
    assert_ne!(
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_ALL_ACCESS, &mut token) },
        0,
        "OpenProcessToken: {}",
        std::io::Error::last_os_error()
    );
    let token = Owned(token);
    let admins: Vec<u16> = ADMINISTRATORS_SID.encode_utf16().chain(Some(0)).collect();
    let mut admins_sid = std::ptr::null_mut();
    // SAFETY: NUL 終端の SID 文字列。
    assert_ne!(
        unsafe { ConvertStringSidToSidW(admins.as_ptr(), &mut admins_sid) },
        0
    );
    let mut disable = [SID_AND_ATTRIBUTES {
        Sid: admins_sid,
        Attributes: 0,
    }];
    let mut restricted: HANDLE = std::ptr::null_mut();
    // SAFETY: 有効なトークン・SID の配列。特権はすべて外す（SeChangeNotify を除く）。
    let ok = unsafe {
        CreateRestrictedToken(
            token.0,
            DISABLE_MAX_PRIVILEGE,
            1,
            disable.as_mut_ptr(),
            0,
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            &mut restricted,
        )
    };
    let create_error = std::io::Error::last_os_error();
    // SAFETY: ConvertStringSidToSidW が LocalAlloc した SID。
    unsafe { windows_sys::Win32::Foundation::LocalFree(admins_sid) };
    assert_ne!(ok, 0, "CreateRestrictedToken: {create_error}");
    let restricted = Owned(restricted);
    // SAFETY: 有効なトークン。このスレッドだけが impersonate する。
    assert_ne!(
        unsafe { ImpersonateLoggedOnUser(restricted.0) },
        0,
        "ImpersonateLoggedOnUser: {}",
        std::io::Error::last_os_error()
    );
    struct Revert;
    impl Drop for Revert {
        fn drop(&mut self) {
            // SAFETY: このスレッドの impersonation を戻すだけ。
            unsafe { RevertToSelf() };
        }
    }
    let _revert = Revert;
    let result = f();
    drop(_revert);
    drop(restricted);
    drop(token);
    let _ = CloseHandle;
    result
}

fn os_error_of(result: &std::io::Result<()>) -> Option<i32> {
    result.as_ref().err().and_then(|e| e.raw_os_error())
}

/// SDDL から自己相対の記述子を作る（テスト用。`LocalFree` は呼ぶ側）。
fn descriptor_from_sddl(sddl: &str) -> *mut core::ffi::c_void {
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    let text: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
    let mut descriptor = std::ptr::null_mut();
    // SAFETY: NUL 終端の SDDL。
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            text.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(ok, 0, "SDDL の変換: {}", std::io::Error::last_os_error());
    descriptor
}

/// 開いているハンドル `handle`（`WRITE_DAC` を持つ）に、SDDL の DACL だけを advapi の
/// `SetSecurityInfo` で書く（見張りの通知の実測用）。
fn set_dacl_on_handle_via_advapi(handle: HANDLE, sddl: &str) -> std::io::Result<()> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{SetSecurityInfo, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{
        GetSecurityDescriptorDacl, DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    };
    let descriptor = descriptor_from_sddl(sddl);
    let mut present = 0;
    let mut defaulted = 0;
    let mut dacl = std::ptr::null_mut();
    // SAFETY: 有効な記述子。`dacl` はその中を指す。
    assert_ne!(
        unsafe { GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted) },
        0
    );
    // SAFETY: 有効なハンドル・DACL。所有者・グループ・SACL は触らない。
    let status = unsafe {
        SetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            dacl,
            std::ptr::null(),
        )
    };
    // SAFETY: ConvertString...が LocalAlloc した記述子。
    unsafe { LocalFree(descriptor) };
    if status != 0 {
        return Err(std::io::Error::from_raw_os_error(status as i32));
    }
    Ok(())
}

/// 開いているハンドル `handle`（`WRITE_DAC` を持つ）に、SDDL の DACL だけを書く（所有者は
/// 変えない。古い `WRITE_DAC` のハンドルの筋の実測用 - 非管理者でも書ける）。
fn set_dacl_on_handle(handle: HANDLE, sddl: &str) -> std::io::Result<()> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::{
        SetKernelObjectSecurity, DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    };
    let descriptor = descriptor_from_sddl(sddl);
    // SAFETY: 有効なハンドルと記述子。DACL だけ。
    let ok = unsafe {
        SetKernelObjectSecurity(
            handle,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor,
        )
    };
    let error = std::io::Error::last_os_error();
    // SAFETY: ConvertString...が LocalAlloc した記述子。
    unsafe { LocalFree(descriptor) };
    if ok == 0 {
        return Err(error);
    }
    Ok(())
}

// --- 1 周目（読むだけ。管理者でなくても走る） ----------------------------------------

/// 設計 §8 PR-1c の 1: 1 周目がジャンクション（ルートの直下と孫）・ハードリンク・開けない子で
/// 断り、**どの記述子も変わっていない**。ディレクトリのシンボリックリンクは作れたときだけ
/// （CI では必ず）。
#[test]
fn scan_refuses_links_hardlinks_and_denied_children_without_changing_anything() {
    let dir = tempfile::tempdir().unwrap();
    let (outside, victim) = outside_with_victim();
    let root = dir.path().join("root");
    make_tree(&root);
    junction(&root.join("jlink"), outside.path());
    junction(
        &root.join("data").join("deep").join("jdeep"),
        outside.path(),
    );
    std::fs::hard_link(&victim, root.join("logs").join("hard.txt")).unwrap();
    let denied = root.join("data").join("denied");
    std::fs::create_dir(&denied).unwrap();
    // Administrators を拒否（一般の利用者の deny-only の Administrators にも効く）。所有者の
    // 暗黙の READ_CONTROL・WRITE_DAC は残るので、片付けで戻せる。
    crate::acl::tests::windows::set_dacl_sddl(&denied, DENY_ADMINS_SDDL);
    let symlink = match std::os::windows::fs::symlink_dir(outside.path(), root.join("slink")) {
        Ok(()) => true,
        Err(e) if std::env::var_os("CI").is_some() => {
            panic!("CI でディレクトリのシンボリックリンクを作れない: {e}")
        }
        Err(e) => {
            eprintln!("skip（シンボリックリンクの分）: {e}");
            false
        }
    };
    let before = tree_snapshot(&root);
    let outside_before = tree_snapshot(outside.path());

    let pinned = pinned(&root);
    let real = pinned.final_path().to_path_buf();
    let mut ticks = 0u64;
    let refusal = pinned.scan_tree(&no_stop, &mut |_| ticks += 1).unwrap_err();
    drop(pinned);
    let ScanRefusal::Problems(problems) = refusal else {
        panic!("{refusal:?}");
    };
    let find = |path: &Path| {
        problems
            .iter()
            .find(|p| p.path() == path)
            .unwrap_or_else(|| panic!("{} が無い: {problems:?}", path.display()))
            .clone()
    };
    assert_eq!(
        find(&real.join("jlink")),
        TreeProblem::ReparsePoint {
            path: real.join("jlink"),
            tag: TAG_MOUNT_POINT
        }
    );
    assert_eq!(
        find(&real.join("data").join("deep").join("jdeep")),
        TreeProblem::ReparsePoint {
            path: real.join("data").join("deep").join("jdeep"),
            tag: TAG_MOUNT_POINT
        }
    );
    assert_eq!(
        find(&real.join("logs").join("hard.txt")),
        TreeProblem::Hardlink {
            path: real.join("logs").join("hard.txt"),
            links: 2
        }
    );
    assert_eq!(
        find(&real.join("data").join("denied")),
        TreeProblem::Denied {
            path: real.join("data").join("denied")
        }
    );
    let mut expected = 4;
    if symlink {
        assert!(matches!(
            find(&real.join("slink")),
            TreeProblem::ReparsePoint {
                tag: 0xA000_000C,
                ..
            }
        ));
        expected += 1;
    }
    assert_eq!(problems.len(), expected, "{problems:?}");
    assert!(ticks >= TREE_OBJECTS, "{ticks}");
    assert_eq!(tree_snapshot(&root), before, "記述子が変わった");
    assert_eq!(
        tree_snapshot(outside.path()),
        outside_before,
        "外が変わった"
    );
    // 片付け（拒否のフォルダは所有者として戻せる。ジャンクションは remove_dir で先を消さない）。
    reset_dir_acl(&denied, &DirAccess::with_install_user(me())).unwrap();
    std::fs::remove_dir(root.join("jlink")).unwrap();
    assert!(victim.exists());
}

/// 【要実測 → 実測】使用中の子（利用者が `DELETE` を持つハンドルを、DELETE を共有せずに開いて
/// いる）: **パスで**同じ権利・共有で開くと共有違反（32）になるが、**ファイル ID で**開く
/// （`OpenFileById`。1 周目・2 周目の開き方）と共有違反にならず開ける（Windows 11 26300、
/// 2026-10-18）。したがって 1 周目は `Busy` ではなく通る。安全性には影響しない（その古い
/// `DELETE` のハンドルで締めた後に木の外へ移される筋は、ID で開いた後の本当のパスの確かめで
/// 塞ぐ - `a_child_moved_out_after_enumeration_…`）。`Busy` の種類は、ID で開いても共有違反に
/// なる版・ファイルシステムのために残す。
#[test]
fn scan_passes_or_reports_busy_for_a_child_held_with_delete() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    make_tree(&root);
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, READ_CONTROL, WRITE_DAC, WRITE_OWNER,
    };
    // `std` の `OpenOptions` は共有に DELETE を含めるので、DELETE を**求めて**共有 READ だけで
    // 開く（利用者が消す・名前を変えるために開いたままにしている形）。
    let held = open_dir(
        &root.join("a.txt"),
        DELETE | FILE_READ_ATTRIBUTES,
        FILE_SHARE_READ,
    )
    .unwrap();
    // 診断【要実測】: 同じ権利（`child_access`）をパスで、DELETE を共有せずに開けるか。
    let child_access =
        READ_CONTROL | FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY | WRITE_DAC | WRITE_OWNER;
    let by_path = open_dir(
        &root.join("a.txt"),
        child_access,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
    );
    let by_path_share_delete = open_dir(
        &root.join("a.txt"),
        child_access,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
    );
    eprintln!(
        "実測: DELETE を持つハンドルがある間に child_access でパスで開く: DELETE を共有しない={:?} 共有する={:?}",
        by_path.as_ref().err(),
        by_path_share_delete.as_ref().err()
    );
    assert_eq!(
        by_path.as_ref().err().and_then(|e| e.raw_os_error()),
        Some(32)
    );
    assert!(by_path_share_delete.is_ok());
    drop((by_path, by_path_share_delete));
    let pinned = pinned(&root);
    let scanned = pinned.scan_tree(&no_stop, &mut |_| {});
    drop(held);
    eprintln!("実測: DELETE を持つハンドルがある間の 1 周目（ID で開く）: {scanned:?}");
    match &scanned {
        Ok(report) => assert_eq!(report.examined, TREE_OBJECTS),
        Err(ScanRefusal::Problems(problems)) => {
            assert_eq!(problems.len(), 1, "{problems:?}");
            assert!(
                matches!(&problems[0], TreeProblem::Busy { path, os_error: 32 } if path == &pinned.final_path().join("a.txt")),
                "{problems:?}"
            );
        }
        other => panic!("{other:?}"),
    }
    // 放せば通る（印のファイルは通す）。
    assert_eq!(
        pinned.scan_tree(&no_stop, &mut |_| {}),
        Ok(ScanReport {
            examined: TREE_OBJECTS
        })
    );
}

/// 1 周目は上限（`MAX_DEPTH` 段）を超える木を `TooDeep` で断る（その下は見ない）。
#[test]
fn scan_refuses_a_tree_deeper_than_the_limit() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir(&root).unwrap();
    let mut deep = root.clone();
    for _ in 0..=crate::acl::imp::MAX_DEPTH {
        deep.push("d");
    }
    std::fs::create_dir_all(&deep).unwrap();
    let pinned = pinned(&root);
    let refusal = pinned.scan_tree(&no_stop, &mut |_| {}).unwrap_err();
    let ScanRefusal::Problems(problems) = refusal else {
        panic!("{refusal:?}");
    };
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        matches!(problems[0], TreeProblem::TooDeep { .. }),
        "{problems:?}"
    );
    let depth = problems[0].path().components().count() - pinned.final_path().components().count();
    assert_eq!(
        depth as u32,
        crate::acl::imp::MAX_DEPTH,
        "{}",
        problems[0].path().display()
    );
}

/// 1 周目は EFS で暗号化された物を断る（EFS が使えない PC・方針では skip）。
#[test]
fn scan_refuses_an_encrypted_file() {
    use windows_sys::Win32::Storage::FileSystem::EncryptFileW;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    make_tree(&root);
    let secret = root.join("logs").join("l.txt");
    // SAFETY: NUL 終端のパス。
    if unsafe { EncryptFileW(wide(&secret).as_ptr()) } == 0 {
        let error = std::io::Error::last_os_error();
        if std::env::var_os("CI").is_some() {
            panic!("CI で EncryptFileW が使えない（EFS が無効なら理由を doc に書いて明示の skip に）: {error}");
        }
        eprintln!("skip: EncryptFileW が使えない（{error}）- EFS が無効の PC");
        return;
    }
    let pinned = pinned(&root);
    let refusal = pinned.scan_tree(&no_stop, &mut |_| {}).unwrap_err();
    let ScanRefusal::Problems(problems) = refusal else {
        panic!("{refusal:?}");
    };
    assert_eq!(
        problems,
        vec![TreeProblem::Encrypted {
            path: pinned.final_path().join("logs").join("l.txt")
        }]
    );
}

/// 1 周目の途中の止める合図: 何も変えずに `Stopped` で終わる（設計 §8 PR-1c の 9 の後半）。
#[test]
fn scan_stops_at_a_directory_boundary_without_changing_anything() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    make_tree(&root);
    let before = tree_snapshot(&root);
    let pinned = pinned(&root);
    let calls = std::cell::Cell::new(0u32);
    let stop = || {
        calls.set(calls.get() + 1);
        if calls.get() >= 2 {
            StopSignal::Preshutdown
        } else {
            StopSignal::None
        }
    };
    let mut ticks = 0u64;
    assert_eq!(
        pinned.scan_tree(&stop, &mut |_| ticks += 1),
        Err(ScanRefusal::Stopped(StopSignal::Preshutdown))
    );
    assert!(ticks < TREE_OBJECTS, "{ticks}");
    assert_eq!(tree_snapshot(&root), before);
    // 最初の境で来れば、1 つも確かめない。
    assert_eq!(
        pinned.scan_tree(&|| StopSignal::Stop, &mut |_| {}),
        Err(ScanRefusal::Stopped(StopSignal::Stop))
    );
}

// --- 古いハンドルの筋（NTFS の挙動の実測。管理者でなくても走る） --------------------

/// 【要実測 → 実測】設計 §2.9 (a)・§8 PR-1c の 7: 締める前に開いた**空のフォルダの書き込み
/// ハンドル**で、DACL を締めた後に `FSCTL_SET_REPARSE_POINT` ができる（印が無いとき。サービスが
/// 同じフォルダを握っていても止まらない）。印のファイルがあれば `ERROR_DIR_NOT_EMPTY`(145) で
/// 断られる。締めるのは DACL だけ（所有者は変えない）で、**一般の利用者の代わり**のトークンで
/// 試す（管理者でも Administrators の FA が効かない）。
#[test]
fn an_old_write_handle_can_reparse_an_empty_dir_but_not_one_holding_a_marker() {
    use windows_sys::Win32::Foundation::GENERIC_WRITE;
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
    let dir = tempfile::tempdir().unwrap();
    let (outside, victim) = outside_with_victim();
    let empty = dir.path().join("empty");
    let marked = dir.path().join("marked");
    std::fs::create_dir(&empty).unwrap();
    std::fs::create_dir(&marked).unwrap();
    std::fs::write(marked.join(MARKER_NAME), b"").unwrap();
    let reader = DirAccess::with_readers([me()]);
    // サービスが握っている（`pin_dir`。本番は SYSTEM。ここでは締める前に本当のトークンで握る）。
    let held = pinned(&empty);
    let (empty_result, marked_result) = as_limited_user(|| {
        // 設定モードで開いておいた書き込みのハンドル。
        let old_empty =
            open_dir(&empty, GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE).unwrap();
        let old_marked =
            open_dir(&marked, GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE).unwrap();
        // 締める（DACL だけ。所有者は利用者のまま - 所有者の暗黙の WRITE_DAC では reparse 化は
        // できないので、この実測には影響しない）。
        reset_dir_acl(&empty, &reader).unwrap();
        reset_dir_acl(&marked, &reader).unwrap();
        // 締めた後は新しくは書き込みで開けない。
        let fresh = open_dir(&empty, GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE);
        assert_eq!(
            fresh.err().and_then(|e| e.raw_os_error()),
            Some(ACCESS_DENIED)
        );
        // サービスが握っている間でも止まらない【要実測】。
        let empty_result = set_reparse_on_handle(old_empty.0, outside.path());
        let marked_result = set_reparse_on_handle(old_marked.0, outside.path());
        (empty_result, marked_result)
    });
    // 握ったハンドルでも属性の変化が見える（設計 §2.9 (b)）。
    assert_eq!(
        held.recheck(&reader),
        Err(RecheckFailure::ReparsePoint {
            tag: TAG_MOUNT_POINT
        })
    );
    drop(held);
    eprintln!("実測: 古い書き込みハンドルで空のフォルダを reparse 化: {empty_result:?}");
    eprintln!("実測: 印のあるフォルダを reparse 化: {marked_result:?}");
    assert!(
        empty_result.is_ok(),
        "古いハンドルで reparse 化できなかった（設計の前提が変わる）: {empty_result:?}"
    );
    assert_eq!(
        os_error_of(&marked_result),
        Some(DIR_NOT_EMPTY),
        "印があるのに reparse 化できた: {marked_result:?}"
    );
    assert!(inspect_file(&empty).unwrap().unwrap().is_reparse_point);
    assert!(!inspect_file(&marked).unwrap().unwrap().is_reparse_point);
    assert!(victim.exists());
    // 片付け: ジャンクションそのもの（先は辿らない）は親の FILE_DELETE_CHILD で消せる。
    std::fs::remove_dir(&empty).unwrap();
    assert!(victim.exists());
    reset_dir_acl(&marked, &DirAccess::with_install_user(me())).unwrap();
}

/// 【要実測 → 実測】設計 §2.4・§8 PR-1c の 4: 作成の可否は**作る時点の親の記述子**で決まる。
/// 締める前に開いておいた親のハンドル（`FILE_ADD_FILE` を持つ）を `RootDirectory` にしても、
/// 締めた後は相対で作れない（一般の利用者の代わりのトークン）。
#[test]
fn an_old_parent_handle_cannot_create_names_after_the_dacl_is_tightened() {
    use windows_sys::Win32::Foundation::GENERIC_WRITE;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_LIST_DIRECTORY, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("parent");
    std::fs::create_dir(&parent).unwrap();
    let (before, after) = as_limited_user(|| {
        let old = open_dir(
            &parent,
            GENERIC_WRITE | FILE_LIST_DIRECTORY,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
        )
        .unwrap();
        let before = create_relative(old.0, "before.txt");
        reset_dir_acl(&parent, &DirAccess::with_readers([me()])).unwrap();
        let after = create_relative(old.0, "after.txt");
        (before, after)
    });
    eprintln!("実測: 締める前の親のハンドル基準の作成 前={before:?} 後={after:?}");
    assert_eq!(before, Ok(()));
    assert_eq!(after, Err(ACCESS_DENIED as u32));
    assert!(parent.join("before.txt").exists());
    assert!(!parent.join("after.txt").exists());
    reset_dir_acl(&parent, &DirAccess::with_install_user(me())).unwrap();
}

/// 【要実測 → 実測】設計 §2.9 の表: 締める前に開いた `WRITE_DAC` のハンドルは締めた後も効く
/// （DACL を戻せる - これは assert）。そのとき `ReadDirectoryChangesW(FILE_NOTIFY_CHANGE_SECURITY)`
/// の通知が来るかを、(a) パスの `SetNamedSecurityInfoW`、(b) ハンドルの `SetKernelObjectSecurity`、
/// (c) ハンドルの `SetSecurityInfo` の 3 通りで測る（結果は stderr。PR-3 の見張りの設計の入力。
/// 来ない経路があれば見張りはその経路を捕まえられない）。あわせて、ふつうの書き込み（中身を
/// 書く）で `FILE_NOTIFY_CHANGE_ATTRIBUTES` の通知が来るか（来れば見張りの filter に足すと
/// 誤検知になる）を測る。
#[test]
fn an_old_write_dac_handle_still_works_after_tightening_and_the_security_watch_is_measured() {
    use std::sync::mpsc::channel;
    use std::time::Duration;
    use windows_sys::Win32::Storage::FileSystem::{
        ReadDirectoryChangesW, FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_ATTRIBUTES,
        FILE_NOTIFY_CHANGE_SECURITY, FILE_NOTIFY_INFORMATION, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, READ_CONTROL, WRITE_DAC,
    };
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    make_tree(&root);
    let child = root.join("data");

    /// 1 回分の通知を別スレッドで待つ（同期 I/O）。返るのは変わった名前の一覧。
    fn watch(root: &Path, filter: u32) -> std::sync::mpsc::Receiver<Vec<String>> {
        let handle = open_dir(
            root,
            FILE_LIST_DIRECTORY,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        )
        .unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let handle = handle;
            let mut buffer = vec![0u32; 16 * 1024];
            let mut returned = 0u32;
            // SAFETY: `handle` は FILE_LIST_DIRECTORY を持つ。`buffer` は 4 バイト境界。
            let ok = unsafe {
                ReadDirectoryChangesW(
                    handle.0,
                    buffer.as_mut_ptr().cast(),
                    (buffer.len() * 4) as u32,
                    1,
                    filter,
                    &mut returned,
                    std::ptr::null_mut(),
                    None,
                )
            };
            let mut names = Vec::new();
            if ok != 0 && returned > 0 {
                let base = buffer.as_ptr() as *const u8;
                let mut offset = 0usize;
                loop {
                    // SAFETY: `offset` は `NextEntryOffset` で進めたバッファ内の境界。
                    let info = unsafe { &*(base.add(offset) as *const FILE_NOTIFY_INFORMATION) };
                    let len = info.FileNameLength as usize / 2;
                    // SAFETY: 名前は `FileNameLength` バイト続く。
                    let name = unsafe { std::slice::from_raw_parts(info.FileName.as_ptr(), len) };
                    names.push(String::from_utf16_lossy(name));
                    if info.NextEntryOffset == 0 {
                        break;
                    }
                    offset += info.NextEntryOffset as usize;
                }
            }
            let _ = tx.send(names);
        });
        rx
    }

    let readers = DirAccess::with_readers([me()]);
    let setting = DirAccess::with_install_user(me());
    let wait = |rx: std::sync::mpsc::Receiver<Vec<String>>| -> Option<Vec<String>> {
        rx.recv_timeout(Duration::from_secs(3)).ok()
    };
    // (a) パスで締める（`SetNamedSecurityInfoW`）→ 通知が来るか。
    let rx = watch(&root, FILE_NOTIFY_CHANGE_SECURITY);
    std::thread::sleep(Duration::from_millis(300));
    // `READ_CONTROL` も持つのは (c) の advapi が伝播のために記述子を読み直すため（WRITE_DAC
    // だけだと 5 で失敗する - これも実測）。
    let old = open_dir(
        &child,
        WRITE_DAC | READ_CONTROL,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
    )
    .unwrap();
    reset_dir_acl(&child, &readers).unwrap();
    let by_path = wait(rx);
    // (b) 古い WRITE_DAC のハンドルで戻す（`SetKernelObjectSecurity`）→ 戻せる（assert）・通知が来るか。
    let rx = watch(&root, FILE_NOTIFY_CHANGE_SECURITY);
    std::thread::sleep(Duration::from_millis(300));
    let written = set_dacl_on_handle(old.0, &setting.sddl());
    assert!(
        written.is_ok(),
        "古い WRITE_DAC のハンドルで DACL を戻せない: {written:?}"
    );
    let by_kernel = wait(rx);
    // (c) 古いハンドルで `SetSecurityInfo`（advapi）→ 通知が来るか。
    reset_dir_acl(&child, &readers).unwrap();
    let rx = watch(&root, FILE_NOTIFY_CHANGE_SECURITY);
    std::thread::sleep(Duration::from_millis(300));
    let via_advapi = set_dacl_on_handle_via_advapi(old.0, &setting.sddl());
    let by_advapi = wait(rx);
    // (b') 古いハンドルを閉じたときに（遅れて）通知が来るか。
    reset_dir_acl(&child, &readers).unwrap();
    set_dacl_on_handle(old.0, &setting.sddl()).unwrap();
    let rx = watch(&root, FILE_NOTIFY_CHANGE_SECURITY);
    std::thread::sleep(Duration::from_millis(300));
    drop(old);
    let after_close = wait(rx);
    eprintln!(
        "実測: 古い WRITE_DAC のハンドルで DACL を変えた後にハンドルを閉じたとき: {after_close:?}"
    );
    assert!(
        by_path.is_some(),
        "パスの SetNamedSecurityInfoW で通知が来ない"
    );
    assert!(
        after_close.is_some(),
        "古いハンドルを閉じても通知が来ない（「遅れて来る」の観測が変わった。mode.rs の doc も直す）"
    );
    assert!(
        by_kernel.is_none(),
        "ハンドルの SetKernelObjectSecurity で通知が来た（設計の前提が変わる。mode.rs の doc も直す）: {by_kernel:?}"
    );
    eprintln!(
        "実測: 古い WRITE_DAC のハンドルで DACL を戻す = 可。FILE_NOTIFY_CHANGE_SECURITY の通知: パス(SetNamedSecurityInfoW)={by_path:?} ハンドル(SetKernelObjectSecurity)={by_kernel:?} ハンドル(SetSecurityInfo)={via_advapi:?} → {by_advapi:?}"
    );
    if via_advapi.is_ok() {
        assert_eq!(
            check_dacl(&read_dir_dacl(&child).unwrap(), &setting),
            Ok(()),
            "古いハンドルで戻した形"
        );
    }
    // (d)(e) ファイル（`a.txt`）の DACL を、パスで・古いハンドル（`SetKernelObjectSecurity`）で
    // 変えたときの通知。
    let file = root.join("a.txt");
    let file_sddl = setting.sddl().replace("OICI", "");
    let reader_sddl = readers.sddl().replace("OICI", "");
    let rx = watch(&root, FILE_NOTIFY_CHANGE_SECURITY);
    std::thread::sleep(Duration::from_millis(300));
    let old_file = open_dir(
        &file,
        WRITE_DAC | READ_CONTROL,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
    )
    .unwrap();
    crate::acl::tests::windows::set_dacl_sddl(&file, &reader_sddl);
    let file_by_path = wait(rx);
    let rx = watch(&root, FILE_NOTIFY_CHANGE_SECURITY);
    std::thread::sleep(Duration::from_millis(300));
    let file_written = set_dacl_on_handle(old_file.0, &file_sddl);
    let file_by_kernel = wait(rx);
    let rx = watch(&root, FILE_NOTIFY_CHANGE_SECURITY);
    std::thread::sleep(Duration::from_millis(300));
    drop(old_file);
    let file_after_close = wait(rx);
    eprintln!(
        "実測: ファイルの DACL の変更の通知: パス(SetFileSecurityW)={file_by_path:?} 古いハンドル(SetKernelObjectSecurity)={file_written:?} → {file_by_kernel:?} 閉じたとき={file_after_close:?}"
    );
    assert!(
        file_by_path.is_some(),
        "パスの SetFileSecurityW で通知が来ない"
    );
    assert!(file_written.is_ok(), "{file_written:?}");
    assert!(
        file_by_kernel.is_none(),
        "ハンドルの SetKernelObjectSecurity で通知が来た（設計の前提が変わる）: {file_by_kernel:?}"
    );
    assert!(
        file_after_close.is_some(),
        "ファイルの古いハンドルを閉じても通知が来ない"
    );

    // (2) ふつうの書き込みで FILE_NOTIFY_CHANGE_ATTRIBUTES の通知が来るか【要実測】。
    let attr_rx = watch(&root, FILE_NOTIFY_CHANGE_ATTRIBUTES);
    std::thread::sleep(Duration::from_millis(100));
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(root.join("a.txt"))
            .unwrap();
        f.write_all(b"more").unwrap();
        f.sync_all().unwrap();
    }
    match attr_rx.recv_timeout(Duration::from_secs(2)) {
        Ok(names) => eprintln!(
            "実測: ふつうの書き込みで FILE_NOTIFY_CHANGE_ATTRIBUTES の通知が**来た**（filter に足すと誤検知）: {names:?}"
        ),
        Err(_) => eprintln!("実測: ふつうの書き込みでは FILE_NOTIFY_CHANGE_ATTRIBUTES の通知は 2 秒待っても来ない"),
    }
}

/// 【要実測 → 実測】設計 §2.9 の表の 3 行目: 締める前に開いた `FILE_WRITE_DATA` のハンドルを
/// 持っていても、ファイルの DACL を読み取りに締めた後は、外からそのファイルへハードリンクを
/// 張れない（1903+ の mitigation は**今の記述子**で確かめる）。一般の利用者の代わりのトークン。
#[test]
fn hardlink_mitigation_checks_the_current_descriptor_not_the_old_handle() {
    use windows_sys::Win32::Storage::FileSystem::CreateHardLinkW;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    make_tree(&root);
    let outside = tempfile::tempdir().unwrap();
    let target = root.join("a.txt");
    let (before, after) = as_limited_user(|| {
        let old = std::fs::OpenOptions::new()
            .write(true)
            .open(&target)
            .unwrap();
        let link = |name: &str| -> std::io::Result<()> {
            let link = outside.path().join(name);
            // SAFETY: NUL 終端のパス 2 つ。
            if unsafe {
                CreateHardLinkW(
                    wide(&link).as_ptr(),
                    wide(&target).as_ptr(),
                    std::ptr::null(),
                )
            } == 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        };
        let before = link("before.lnk");
        // ファイル自身の DACL を「読み取りと実行」に（所有者は利用者のまま）。
        let sddl = DirAccess::with_readers([me()]).sddl().replace("OICI", "");
        crate::acl::tests::windows::set_dacl_sddl(&target, &sddl);
        let after = link("after.lnk");
        drop(old);
        (before, after)
    });
    eprintln!("実測: ハードリンク 締める前={before:?} 締めた後（古い書き込みハンドルを持ったまま）={after:?}");
    assert!(before.is_ok(), "{before:?}");
    assert_eq!(os_error_of(&after), Some(ACCESS_DENIED), "{after:?}");
}

// --- 締める・緩める（所有者を移すので管理者 = CI） --------------------------------------

/// 設計 §8 PR-1c の 2・5 の後半: 2 周目の後は全オブジェクトが運用モードの形（保護・所有者
/// Administrators・利用者は読み取り、印は利用者の ACE なし）。一般の利用者の代わりは、締めた木に
/// 名前を作れない・名前を変えられない・DACL を変えられない・印を消せない・読める。緩めた後は
/// 設定モードの形で、名前を作れるが、印は消せない・名前を変えられない。`recheck` はそれぞれの
/// 形で通る・落ちる。
#[test]
fn close_tree_writes_the_operation_shape_and_a_limited_user_cannot_write_or_relax_it() {
    if !is_elevated_admin() {
        eprintln!("skip: 所有者を移すのは管理者だけ（CI で確かめる）");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    make_tree(&root);
    let mode = mode();
    // 管理者が作った物の所有者は既に Administrators なので、所有者を移す確かめが空振りしない
    // ように、一部の所有者を利用者（自分）へ移しておく（設定モードでデスクトップが作った物）。
    crate::root::win_tests::set_owner_to_me(&root.join("data"));
    crate::root::win_tests::set_owner_to_me(&root.join("data").join("d1.txt"));
    assert_eq!(
        inspect_file(&root.join("data"))
            .unwrap()
            .unwrap()
            .dacl
            .owner,
        me().as_str()
    );
    let pinned = pinned(&root);
    assert_eq!(
        pinned.scan_tree(&no_stop, &mut |_| {}),
        Ok(ScanReport {
            examined: TREE_OBJECTS
        })
    );
    let mut ticks = Vec::new();
    let progress = pinned
        .close_tree(&mode, &no_stop, &mut |p| ticks.push(*p))
        .unwrap();
    assert_eq!(
        progress,
        Progress {
            examined: CLOSED_OBJECTS,
            written: CLOSED_OBJECTS
        }
    );
    assert_eq!(ticks.len() as u64, CLOSED_OBJECTS - 1, "子 1 つごとに 1 回");
    assert!(ticks.windows(2).all(|w| w[0].examined < w[1].examined));
    expect_tree_shape(&root, mode.operation());
    every_dir_has_a_marker(&root);
    assert_eq!(pinned.recheck(mode.operation()), Ok(()));
    assert!(matches!(
        pinned.recheck(mode.setting()),
        Err(RecheckFailure::Acl(AclMismatch::UnexpectedAce { .. }))
    ));
    assert_eq!(
        classify_mode(&observe_root(&root).unwrap(), &me()),
        ModeObservation::Operation
    );

    let data = root.join("data");
    as_limited_user(|| {
        let denied = |r: std::io::Result<()>| {
            assert_eq!(r.err().and_then(|e| e.raw_os_error()), Some(ACCESS_DENIED))
        };
        denied(std::fs::write(root.join("new.txt"), b"x"));
        denied(std::fs::write(data.join("new.txt"), b"x"));
        denied(std::fs::create_dir(data.join("newdir")));
        denied(std::fs::rename(
            data.join("d1.txt"),
            data.join("renamed.txt"),
        ));
        denied(std::fs::remove_file(data.join("d1.txt")));
        denied(std::fs::remove_file(data.join(MARKER_NAME)));
        denied(std::fs::hard_link(data.join("d1.txt"), root.join("hl.txt")));
        // DACL を変えられない（所有者が Administrators なので暗黙の WRITE_DAC も無い）。
        assert!(matches!(
            reset_dir_acl(&data, &DirAccess::with_install_user(me())),
            Err(SafeRootError::AclApi { os_error: 5, .. })
        ));
        // 読める。
        assert_eq!(std::fs::read(data.join("d1.txt")).unwrap(), b"1");
        assert_eq!(std::fs::read_dir(&data).unwrap().count(), 4);
    });
    expect_tree_shape(&root, mode.operation());

    let mut relax_ticks = 0u64;
    let report = pinned.relax_tree(&mode, &mut |_| relax_ticks += 1).unwrap();
    assert_eq!(report.written, CLOSED_OBJECTS);
    assert!(report.skipped.is_empty(), "{report:?}");
    expect_tree_shape(&root, mode.setting());
    assert_eq!(
        classify_mode(&observe_root(&root).unwrap(), &me()),
        ModeObservation::Setting
    );
    as_limited_user(|| {
        std::fs::write(data.join("new.txt"), b"x").unwrap();
        std::fs::remove_file(data.join("new.txt")).unwrap();
        // 設定モードでも印は消せない・名前を変えられない（「変更」に FILE_DELETE_CHILD が無く、
        // 印自身に DELETE が無い）。
        assert_eq!(
            std::fs::remove_file(data.join(MARKER_NAME))
                .unwrap_err()
                .raw_os_error(),
            Some(ACCESS_DENIED)
        );
        assert_eq!(
            std::fs::rename(data.join(MARKER_NAME), data.join("moved"))
                .unwrap_err()
                .raw_os_error(),
            Some(ACCESS_DENIED)
        );
        assert_eq!(
            std::fs::hard_link(data.join(MARKER_NAME), root.join("mk.lnk"))
                .unwrap_err()
                .raw_os_error(),
            Some(ACCESS_DENIED)
        );
    });
    // 緩い木に対しても冪等。
    let again = pinned.relax_tree(&mode, &mut |_| {}).unwrap();
    assert_eq!(again.written, CLOSED_OBJECTS);
    every_dir_has_a_marker(&root);
    expect_tree_shape(&root, mode.setting());
}

/// 設計 §8 PR-1c の 3: 2 周目の途中で植える（`data` を締めた直後に、まだ締めていない孫
/// `data/deep` にジャンクションを作る）→ その場で止まり、緩め直すと全体が設定モードの形に戻る。
/// 外は変わらない。
#[test]
fn close_tree_aborts_on_a_link_planted_between_passes_and_relax_restores_the_setting_shape() {
    if !is_elevated_admin() {
        eprintln!("skip: 所有者を移すのは管理者だけ（CI で確かめる）");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (outside, _victim) = outside_with_victim();
    let root = dir.path().join("root");
    make_tree(&root);
    let outside_before = tree_snapshot(outside.path());
    let mode = mode();
    let pinned = pinned(&root);
    let real = pinned.final_path().to_path_buf();
    let late = real.join("data").join("deep").join("late");
    let (late_for_hook, outside_for_hook) = (late.clone(), outside.path().to_path_buf());
    let planted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let planted_for_hook = planted.clone();
    crate::acl::imp::AFTER_CHILD_WRITTEN.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |handle| {
            let written = crate::acl::imp::final_path_of(handle).unwrap();
            if written.file_name().is_some_and(|n| n == "data")
                && !planted_for_hook.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                junction(&late_for_hook, &outside_for_hook);
            }
        }));
    });
    let aborted = pinned.close_tree(&mode, &no_stop, &mut |_| {});
    crate::acl::imp::AFTER_CHILD_WRITTEN.with(|hook| *hook.borrow_mut() = None);
    assert!(
        planted.load(std::sync::atomic::Ordering::SeqCst),
        "フックが動いていない"
    );
    let abort = aborted.unwrap_err();
    assert_eq!(
        abort.reason,
        AbortReason::Problem(TreeProblem::ReparsePoint {
            path: late.clone(),
            tag: TAG_MOUNT_POINT
        })
    );
    assert!(
        abort.written >= 2 && abort.written < CLOSED_OBJECTS,
        "{abort:?}"
    );
    // ルートは締まっている（デスクトップは Operation と見る）。
    assert_eq!(
        classify_mode(&observe_root(&root).unwrap(), &me()),
        ModeObservation::Operation
    );
    // 緩め直す: ジャンクションは飛ばして記録、ほかは設定モードの形に。
    let report = pinned.relax_tree(&mode, &mut |_| {}).unwrap();
    assert_eq!(
        report.skipped,
        vec![TreeProblem::ReparsePoint {
            path: late.clone(),
            tag: TAG_MOUNT_POINT
        }]
    );
    std::fs::remove_dir(&late).unwrap();
    expect_tree_shape(&root, mode.setting());
    assert_eq!(
        tree_snapshot(outside.path()),
        outside_before,
        "外が変わった"
    );
}

/// 設計 §8 PR-1c の 5: `relax_tree` はルートを最後に書く（子を書くたびにルートの DACL を
/// 読むフックで、ルートがまだ締まっていることを確かめる）。
#[test]
fn relax_tree_writes_the_root_last() {
    if !is_elevated_admin() {
        eprintln!("skip: 所有者を移すのは管理者だけ（CI で確かめる）");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    make_tree(&root);
    let mode = mode();
    let pinned = pinned(&root);
    pinned.close_tree(&mode, &no_stop, &mut |_| {}).unwrap();
    let observed = std::sync::Arc::new(std::sync::Mutex::new(
        Vec::<(PathBuf, ModeObservation)>::new(),
    ));
    let observed_for_hook = observed.clone();
    let root_for_hook = root.clone();
    crate::acl::imp::AFTER_CHILD_WRITTEN.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |handle| {
            let written = crate::acl::imp::final_path_of(handle).unwrap();
            let seen = classify_mode(&observe_root(&root_for_hook).unwrap(), &me());
            observed_for_hook.lock().unwrap().push((written, seen));
        }));
    });
    let report = pinned.relax_tree(&mode, &mut |_| {});
    crate::acl::imp::AFTER_CHILD_WRITTEN.with(|hook| *hook.borrow_mut() = None);
    assert_eq!(report.unwrap().written, CLOSED_OBJECTS);
    let observed = observed.lock().unwrap();
    assert_eq!(observed.len() as u64, CLOSED_OBJECTS - 1, "{observed:?}");
    for (written, seen) in observed.iter() {
        assert_eq!(
            *seen,
            ModeObservation::Operation,
            "{} を書いた時点でルートがもう緩んでいる",
            written.display()
        );
    }
    assert_eq!(
        classify_mode(&observe_root(&root).unwrap(), &me()),
        ModeObservation::Setting
    );
}

/// 設計 §8 PR-1c の 9: 2 周目の途中の止める合図。STOP → 止めて（呼ぶ側が）緩め直すと設定
/// モードの形。PRESHUTDOWN → 止めて緩めない = ルートは締まったまま、下の一部は緩い（締めかけ）。
#[test]
fn close_tree_stops_at_a_directory_boundary_and_relax_restores_after_stop() {
    if !is_elevated_admin() {
        eprintln!("skip: 所有者を移すのは管理者だけ（CI で確かめる）");
        return;
    }
    for signal in [StopSignal::Stop, StopSignal::Preshutdown] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        make_tree(&root);
        let mode = mode();
        let pinned = pinned(&root);
        let calls = std::cell::Cell::new(0u32);
        let stop = || {
            calls.set(calls.get() + 1);
            if calls.get() == 2 {
                signal
            } else {
                StopSignal::None
            }
        };
        let abort = pinned.close_tree(&mode, &stop, &mut |_| {}).unwrap_err();
        assert_eq!(abort.reason, AbortReason::Stopped(signal), "{signal:?}");
        assert!(
            abort.written >= 1 && abort.written < CLOSED_OBJECTS,
            "{abort:?}"
        );
        // 締めかけ: ルートは締まっている、下の一部（まだ辿っていないフォルダの中）は緩い。
        // 「緩い」は形で数える（保護されていない = 親から継承したまま。所有者で数えると、管理者が
        // 作った物は最初から Administrators なので CI で数えられない）。
        assert_eq!(
            classify_mode(&observe_root(&root).unwrap(), &me()),
            ModeObservation::Operation
        );
        let loose = tree_snapshot(&root)
            .into_iter()
            .filter(|(_, s)| !s.as_ref().unwrap().dacl.protected)
            .count();
        assert!(loose > 0, "{signal:?}: 全部締まっている");
        if signal == StopSignal::Stop {
            let report = pinned.relax_tree(&mode, &mut |_| {}).unwrap();
            assert!(report.skipped.is_empty(), "{report:?}");
            expect_tree_shape(&root, mode.setting());
        }
        // 最初の境で来れば、ルートにも書かない。
        let dir2 = tempfile::tempdir().unwrap();
        let root2 = dir2.path().join("root");
        make_tree(&root2);
        let before = tree_snapshot(&root2);
        let pinned2 = pin_dir(&root2).unwrap().unwrap();
        let abort = pinned2
            .close_tree(&mode, &|| signal, &mut |_| {})
            .unwrap_err();
        assert_eq!(abort.written, 0);
        assert_eq!(tree_snapshot(&root2), before);
    }
}

/// 設計 §8 PR-1c の 8（Fable-2 A-3）: 列挙の後・開く前のフックで子を木の外へ移す → 外の記述子は
/// 変わらない（2 周目と緩めるの両方）。
#[test]
fn a_child_moved_out_after_enumeration_keeps_its_descriptor_in_close_and_relax() {
    if !is_elevated_admin() {
        eprintln!("skip: 所有者を移すのは管理者だけ（CI で確かめる）");
        return;
    }
    for phase in ["close", "relax"] {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        make_tree(&root);
        let mode = mode();
        let pinned = pinned(&root);
        if phase == "relax" {
            pinned.close_tree(&mode, &no_stop, &mut |_| {}).unwrap();
        }
        let from = root.join("data").join("d2.txt");
        let to = outside.path().join("moved.txt");
        let before = inspect_file(&from).unwrap().unwrap();
        let (from_h, to_h) = (from.clone(), to.clone());
        crate::acl::imp::BEFORE_CHILD_OPEN.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move |child: &Path| {
                if child.file_name().is_some_and(|n| n == "d2.txt") {
                    std::fs::rename(&from_h, &to_h).unwrap();
                }
            }));
        });
        let result = if phase == "close" {
            pinned
                .close_tree(&mode, &no_stop, &mut |_| {})
                .map(|p| p.written)
                .map_err(|e| format!("{e:?}"))
        } else {
            pinned
                .relax_tree(&mode, &mut |_| {})
                .map(|r| r.written)
                .map_err(|e| format!("{e:?}"))
        };
        crate::acl::imp::BEFORE_CHILD_OPEN.with(|hook| *hook.borrow_mut() = None);
        assert!(to.exists(), "{phase}: フックが動いていない");
        assert_eq!(result, Ok(CLOSED_OBJECTS - 1), "{phase}");
        assert_eq!(
            inspect_file(&to).unwrap().unwrap(),
            before,
            "{phase}: 木の外へ移された物の記述子を書き換えた"
        );
    }
}

/// 設計 §8 PR-1c の 7 の後半: `ensure_marker` は印を置き（利用者の ACE なし・所有者
/// Administrators・リンク数 1）、2 回目は `Existed`。印を置く前に古いハンドルで reparse に
/// されたフォルダは `DirIsReparse` で止める。`MARKER_NAME` にフォルダがあれば `Occupied`。
#[test]
fn ensure_marker_places_a_marker_and_detects_a_dir_reparsed_before_it() {
    use windows_sys::Win32::Foundation::GENERIC_WRITE;
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
    if !is_elevated_admin() {
        eprintln!("skip: 所有者を移すのは管理者だけ（CI で確かめる）");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (outside, victim) = outside_with_victim();
    let data = dir.path().join("data");
    std::fs::create_dir(&data).unwrap();
    let held = pinned(&data);
    assert_eq!(held.ensure_marker(), Ok(MarkerOutcome::Created));
    let marker = inspect_file(&data.join(MARKER_NAME)).unwrap().unwrap();
    assert!(!marker.is_dir && marker.links == 1 && !marker.is_reparse_point);
    assert!(exposure(&marker).is_empty(), "{marker:?}");
    assert_eq!(marker.dacl.owner, ADMINISTRATORS_SID);
    assert_eq!(held.ensure_marker(), Ok(MarkerOutcome::Existed));
    // 既にある印を誰かが DELETE で握っていれば `Busy`（偽の印を古いハンドルで消す筋を止める）。
    {
        use windows_sys::Win32::Storage::FileSystem::{
            DELETE, FILE_READ_ATTRIBUTES, FILE_SHARE_READ,
        };
        let holder = open_dir(
            &data.join(MARKER_NAME),
            DELETE | FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ,
        )
        .unwrap();
        assert_eq!(
            held.ensure_marker(),
            Err(MarkerError::Busy { os_error: 32 })
        );
        drop(holder);
        assert_eq!(held.ensure_marker(), Ok(MarkerOutcome::Existed));
    }
    // 置いた後は、古い書き込みハンドルでも reparse 化できない。
    let old = open_dir(&data, GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE).unwrap();
    assert_eq!(
        os_error_of(&set_reparse_on_handle(old.0, outside.path())),
        Some(DIR_NOT_EMPTY)
    );
    drop(old);
    drop(held);

    // 印を置く前に reparse にされた: 握ったハンドルで気付く。
    let empty = dir.path().join("empty");
    std::fs::create_dir(&empty).unwrap();
    let held = pinned(&empty);
    set_reparse_to(&empty, outside.path()).unwrap();
    assert_eq!(
        held.ensure_marker(),
        Err(MarkerError::DirIsReparse {
            tag: TAG_MOUNT_POINT
        })
    );
    drop(held);
    assert!(!outside.path().join(MARKER_NAME).exists(), "外に印を作った");
    assert!(victim.exists());
    std::fs::remove_dir(&empty).unwrap();

    // 名前がフォルダで塞がっている。
    let occupied = dir.path().join("occupied");
    std::fs::create_dir_all(occupied.join(MARKER_NAME)).unwrap();
    assert_eq!(
        pinned(&occupied).ensure_marker(),
        Err(MarkerError::Occupied(Refusal::NotAFile))
    );
}

/// `recheck`: 握ったフォルダがその場で reparse にされたら `ReparsePoint`（管理者でなくても）。
#[test]
fn recheck_notices_a_held_dir_turned_into_a_junction() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let empty = dir.path().join("empty");
    std::fs::create_dir(&empty).unwrap();
    let held = pinned(&empty);
    let expected = DirAccess::with_readers([me()]);
    assert!(matches!(
        held.recheck(&expected),
        Err(RecheckFailure::Acl(AclMismatch::NotProtected))
    ));
    // 形が運用モードでも、所有者が利用者のままなら通らない（DisallowedOwner）- 本番は所有者を
    // 移す（管理者のテスト `close_tree_...` で `Ok` を確かめる）。
    let tightened = dir.path().join("tightened");
    std::fs::create_dir(&tightened).unwrap();
    // 管理者（CI）が作った物の所有者は Administrators なので、利用者へ移してから。
    if inspect_file(&tightened).unwrap().unwrap().dacl.owner != me().as_str() {
        crate::root::win_tests::set_owner_to_me(&tightened);
    }
    let held_tightened = pinned(&tightened);
    reset_dir_acl(&tightened, &expected).unwrap();
    assert!(matches!(
        held_tightened.recheck(&expected),
        Err(RecheckFailure::Acl(AclMismatch::DisallowedOwner { .. }))
    ));
    drop(held_tightened);
    reset_dir_acl(&tightened, &DirAccess::with_install_user(me())).unwrap();
    // その場で reparse にされたら、握ったハンドルでも見える。
    set_reparse_to(&empty, outside.path()).unwrap();
    assert_eq!(
        held.recheck(&expected),
        Err(RecheckFailure::ReparsePoint {
            tag: TAG_MOUNT_POINT
        })
    );
    drop(held);
    std::fs::remove_dir(&empty).unwrap();
}

/// `observe_root`: 無い → `Absent`、ジャンクション → `Refused`、ふつう → `Present`。開けない
/// （拒否）→ `Denied`（一般の利用者の代わりのトークンで、利用者の ACE が無いフォルダ）。
#[test]
fn observe_root_reads_without_following_and_maps_denied() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    assert_eq!(
        observe_root(&dir.path().join("absent")).unwrap(),
        RootObservation::Absent
    );
    let plain = dir.path().join("plain");
    std::fs::create_dir(&plain).unwrap();
    assert!(matches!(
        observe_root(&plain).unwrap(),
        RootObservation::Present(_)
    ));
    junction(&dir.path().join("j"), outside.path());
    assert!(matches!(
        observe_root(&dir.path().join("j")),
        Err(SafeRootError::Refused {
            refusal: Refusal::ReparsePoint {
                tag: TAG_MOUNT_POINT
            },
            ..
        })
    ));
    let closed = dir.path().join("closed");
    std::fs::create_dir(&closed).unwrap();
    // 利用者の ACE が無く、Administrators は拒否（一般の利用者の deny-only の Administrators
    // にも効く）。
    crate::acl::tests::windows::set_dacl_sddl(&closed, DENY_ADMINS_SDDL);
    let seen = as_limited_user(|| observe_root(&closed).unwrap());
    assert_eq!(seen, RootObservation::Denied);
    assert_eq!(classify_mode(&seen, &me()), ModeObservation::NoAccessForMe);
    reset_dir_acl(&closed, &DirAccess::with_install_user(me())).unwrap();
}

// --- SYSTEM での確かめ（設計 §8 PR-1c の 10、Fable-2 C-6。管理者 = CI） ----------------

/// `schtasks /RU SYSTEM` で自分（このテストの実行ファイル）の `system_child_closes_tree` を
/// LocalSystem として起こし、**所有権の特権を外した**うえで `close_tree` が所有者を
/// Administrators にできることを確かめる【要実測 → 実測】。手元（非管理者）は skip。
#[test]
fn local_system_closes_a_tree_without_take_ownership_privileges() {
    if !is_elevated_admin() {
        eprintln!("skip: schtasks /RU SYSTEM は管理者だけ（CI で確かめる）");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    make_tree(&root);
    let out = dir.path().join("result.txt");
    let log = dir.path().join("child.log");
    let script = dir.path().join("run.cmd");
    let exe = std::env::current_exe().unwrap();
    std::fs::write(
        &script,
        format!(
            "@echo off\r\nset BANTO_SR_TREE={}\r\nset BANTO_SR_OUT={}\r\nset BANTO_SR_USER={}\r\n\"{}\" --exact mode::tests::win_tests::system_child_closes_tree --nocapture > \"{}\" 2>&1\r\n",
            root.display(),
            out.display(),
            me().as_str(),
            exe.display(),
            log.display()
        ),
    )
    .unwrap();
    let task = format!("banto-safe-root-{}", std::process::id());
    let run = |args: &[&str]| {
        let output = std::process::Command::new("schtasks")
            .args(args)
            .output()
            .unwrap();
        (
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).into_owned()
                + &String::from_utf8_lossy(&output.stderr),
        )
    };
    let (created, text) = run(&[
        "/Create",
        "/TN",
        &task,
        "/TR",
        &format!("cmd /c \"{}\"", script.display()),
        "/SC",
        "ONCE",
        "/ST",
        "00:00",
        "/RU",
        "SYSTEM",
        "/F",
    ]);
    assert!(created, "schtasks /Create: {text}");
    let (started, text) = run(&["/Run", "/TN", &task]);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let mut result = None;
    if started {
        while std::time::Instant::now() < deadline {
            if let Ok(text) = std::fs::read_to_string(&out) {
                if text.contains("\nend\n") {
                    result = Some(text);
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }
    let _ = run(&["/Delete", "/TN", &task, "/F"]);
    assert!(started, "schtasks /Run: {text}");
    let result = result.unwrap_or_else(|| {
        panic!(
            "SYSTEM の子が 120 秒で結果を書かなかった。log: {:?}",
            std::fs::read_to_string(&log)
        )
    });
    eprintln!("実測（SYSTEM）: {result}");
    assert!(
        result.lines().any(|line| line == "user=S-1-5-18"),
        "SYSTEM で走っていない: {result}"
    );
    assert!(
        result.lines().any(|line| line.starts_with("close=ok")),
        "{result}"
    );
    assert!(
        result.lines().any(|line| line == "root_owner=S-1-5-32-544"),
        "{result}"
    );
    expect_tree_shape(&root, mode().operation());
    // 片付け（管理者なので緩められる）。
    pinned(&root).relax_tree(&mode(), &mut |_| {}).unwrap();
}

/// `local_system_closes_a_tree_without_take_ownership_privileges` が SYSTEM で起こす側。
/// 環境変数が無ければ何もしない（ふつうのテストの実行では素通り）。
#[test]
fn system_child_closes_tree() {
    let (Some(tree), Some(out), Some(user)) = (
        std::env::var_os("BANTO_SR_TREE"),
        std::env::var_os("BANTO_SR_OUT"),
        std::env::var("BANTO_SR_USER").ok(),
    ) else {
        return;
    };
    let mut lines = Vec::new();
    let user_sid = crate::acl::imp::current_user_sid_string().unwrap_or_default();
    lines.push(format!("user={user_sid}"));
    lines.push(format!("privileges={:?}", drop_ownership_privileges()));
    let mode = ModeAccess::for_users([InstallUserSid::parse(&user).unwrap()]).unwrap();
    match pin_dir(Path::new(&tree)) {
        Ok(Some(pinned)) => {
            let scan = pinned.scan_tree(&no_stop, &mut |_| {});
            lines.push(format!("scan={scan:?}"));
            match pinned.close_tree(&mode, &no_stop, &mut |_| {}) {
                Ok(progress) => lines.push(format!("close=ok {progress:?}")),
                Err(abort) => lines.push(format!("close=err {abort:?}")),
            }
            lines.push(format!(
                "root_owner={}",
                read_dir_dacl(Path::new(&tree))
                    .map(|d| d.owner)
                    .unwrap_or_default()
            ));
        }
        other => lines.push(format!("pin={other:?}")),
    }
    lines.push("end".to_owned());
    std::fs::write(&out, lines.join("\n") + "\n").unwrap();
}

/// 所有権に関わる特権（SeTakeOwnership・SeRestore・SeBackup・SeSecurity）をこのプロセスの
/// トークンから**外す**（`SE_PRIVILEGE_REMOVED`）。外せた名前と失敗を返す。
fn drop_ownership_privileges() -> Vec<(String, Result<(), i32>)> {
    use windows_sys::Win32::Security::{
        AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES, SE_PRIVILEGE_REMOVED,
        TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: 今のプロセスの疑似ハンドル。
    if unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
    } == 0
    {
        return vec![(
            "OpenProcessToken".into(),
            Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(-1)),
        )];
    }
    let token = Owned(token);
    let mut out = Vec::new();
    for name in [
        "SeTakeOwnershipPrivilege",
        "SeRestorePrivilege",
        "SeBackupPrivilege",
        "SeSecurityPrivilege",
    ] {
        let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let mut privileges = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: windows_sys::Win32::Foundation::LUID {
                    LowPart: 0,
                    HighPart: 0,
                },
                Attributes: SE_PRIVILEGE_REMOVED,
            }],
        };
        // SAFETY: NUL 終端の名前、書き込み先の LUID。
        let found = unsafe {
            LookupPrivilegeValueW(
                std::ptr::null(),
                wide.as_ptr(),
                &mut privileges.Privileges[0].Luid,
            )
        };
        if found == 0 {
            out.push((
                name.to_owned(),
                Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(-1)),
            ));
            continue;
        }
        // SAFETY: 有効なトークンと構造体。
        let ok = unsafe {
            AdjustTokenPrivileges(
                token.0,
                0,
                &privileges,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        let error = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        out.push((
            name.to_owned(),
            if ok != 0 && error == 0 {
                Ok(())
            } else {
                Err(error)
            },
        ));
    }
    out
}

// --- 時間（設計 §3.8・§8 PR-1c の 11。重いので `--run-ignored` で） --------------------

/// 【要実測】1 万件の木の 1 周目・2 周目・緩めるの時間（目標: 1 周目 + 2 周目で 15 秒以内）。
/// 締める・緩めるは管理者だけ（非管理者は 1 周目だけ測って終わる）。結果は stderr に出す。
#[test]
#[ignore = "時間の実測。cargo nextest run -p banto-safe-root --run-ignored ignored-only -E 'test(ten_thousand)'"]
fn ten_thousand_files_scan_close_relax_timing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    make_tree(&root);
    let data = root.join("data");
    let started = std::time::Instant::now();
    for i in 0..10_000u32 {
        std::fs::write(
            data.join(format!("2026{:04}-{:03}.sqlite3", i / 3, i % 3)),
            b"x",
        )
        .unwrap();
    }
    eprintln!("作成: {:?}", started.elapsed());
    let pinned = pinned(&root);
    let started = std::time::Instant::now();
    let scan = pinned.scan_tree(&no_stop, &mut |_| {}).unwrap();
    let scan_time = started.elapsed();
    eprintln!("1 周目: {:?}（{} 件）", scan_time, scan.examined);
    if !is_elevated_admin() {
        eprintln!("skip: 2 周目・緩めるは管理者だけ");
        return;
    }
    let started = std::time::Instant::now();
    let close = pinned.close_tree(&mode(), &no_stop, &mut |_| {}).unwrap();
    let close_time = started.elapsed();
    eprintln!("2 周目: {:?}（{} 件）", close_time, close.written);
    let started = std::time::Instant::now();
    let relax = pinned.relax_tree(&mode(), &mut |_| {}).unwrap();
    eprintln!("緩める: {:?}（{} 件）", started.elapsed(), relax.written);
    eprintln!(
        "1 周目 + 2 周目 = {:?}（目標 15 秒）",
        scan_time + close_time
    );
}

// --- Codex P1: 締めた直後の空のフォルダ（印を全フォルダに置く） ---------------------

/// 1 周目は、印の名前（`MARKER_NAME`）をフォルダが塞いでいれば `MarkerOccupied` で断る
/// （2 周目で印を置けないため。管理者でなくても走る）。
#[test]
fn scan_refuses_a_directory_occupying_the_marker_name() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    make_tree(&root);
    std::fs::create_dir(root.join("logs").join(MARKER_NAME)).unwrap();
    // 綴り違い（大文字）。OS は同じ名前に解決するので、印を置けない（Fable M-1 の (a)）。
    let upper = MARKER_NAME.to_ascii_uppercase();
    std::fs::create_dir(root.join("data").join("deep").join(&upper)).unwrap();
    let pinned = pinned(&root);
    let refusal = pinned.scan_tree(&no_stop, &mut |_| {}).unwrap_err();
    let ScanRefusal::Problems(mut problems) = refusal else {
        panic!("{refusal:?}");
    };
    problems.sort_by_key(|p| p.path().to_path_buf());
    let mut expected = vec![
        TreeProblem::MarkerOccupied {
            path: pinned.final_path().join("logs").join(MARKER_NAME),
        },
        TreeProblem::MarkerOccupied {
            path: pinned.final_path().join("data").join("deep").join(&upper),
        },
    ];
    expected.sort_by_key(|p| p.path().to_path_buf());
    assert_eq!(problems, expected);
}

/// Fable M-1: `remove` は綴り違いの名前（`.BANTO-SAFE-ROOT`）でも、OS が印の席に解決する物なら
/// 消さない（ファイル ID で見分ける）。管理者でなくても走る。
#[test]
fn remove_refuses_the_marker_by_identity_regardless_of_spelling() {
    use crate::guard::KnownFolders;
    use crate::root::{open_root, Mode, RootSpec};
    let dir = tempfile::tempdir().unwrap();
    let tree = dir.path().join("tree");
    std::fs::create_dir(&tree).unwrap();
    std::fs::write(tree.join(MARKER_NAME), b"").unwrap();
    std::fs::write(tree.join("other.txt"), b"x").unwrap();
    let root = open_root(&RootSpec {
        expected: tree.clone(),
        known: KnownFolders::default(),
        owners: vec![me()],
        create_access: None,
        create: false,
        mode: Mode::Guarded,
    })
    .unwrap();
    let upper = MARKER_NAME.to_ascii_uppercase();
    assert!(
        root.as_dir().remove(&upper).is_err(),
        "綴り違いで印を消せた"
    );
    assert!(root.as_dir().remove(MARKER_NAME).is_err());
    assert!(tree.join(MARKER_NAME).exists());
    assert_eq!(root.as_dir().remove("other.txt"), Ok(true));
    let report = root.as_dir().remove_where(&|_| true).unwrap();
    assert!(report.removed.is_empty(), "{report:?}");
    assert!(tree.join(MARKER_NAME).exists());
}

/// Codex P1: 2 周目は**すべての**フォルダ（印の無かった `logs`・`deep`・空の `data\empty` を含む）に
/// 印を置くので、締める前から開いていた書き込みのハンドルでも、締めた後に reparse 化できない
/// （`ERROR_DIR_NOT_EMPTY`）。印は利用者の ACE なし。
#[test]
fn close_tree_places_a_marker_in_every_directory_so_old_write_handles_cannot_reparse() {
    use windows_sys::Win32::Foundation::GENERIC_WRITE;
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
    if !is_elevated_admin() {
        eprintln!("skip: 所有者を移すのは管理者だけ（CI で確かめる）");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (outside, victim) = outside_with_victim();
    let root = dir.path().join("root");
    make_tree(&root);
    let empty = root.join("data").join("empty");
    std::fs::create_dir(&empty).unwrap();
    let old_empty = open_dir(&empty, GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE).unwrap();
    let old_logs = open_dir(
        &root.join("logs"),
        GENERIC_WRITE,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
    )
    .unwrap();
    let mode = mode();
    let pinned = pinned(&root);
    let progress = pinned.close_tree(&mode, &no_stop, &mut |_| {}).unwrap();
    // 11 + empty + 印 3 つ（deep・logs・empty）。
    assert_eq!(progress.written, CLOSED_OBJECTS + 2, "{progress:?}");
    every_dir_has_a_marker(&root);
    expect_tree_shape(&root, mode.operation());
    assert_eq!(
        os_error_of(&set_reparse_on_handle(old_empty.0, outside.path())),
        Some(DIR_NOT_EMPTY)
    );
    assert_eq!(
        os_error_of(&set_reparse_on_handle(old_logs.0, outside.path())),
        Some(DIR_NOT_EMPTY)
    );
    assert!(!inspect_file(&empty).unwrap().unwrap().is_reparse_point);
    assert!(victim.exists());
    // 緩めても印は残り、空だったフォルダも以後は reparse にできない。
    pinned.relax_tree(&mode, &mut |_| {}).unwrap();
    every_dir_has_a_marker(&root);
    assert_eq!(
        os_error_of(&set_reparse_on_handle(old_empty.0, outside.path())),
        Some(DIR_NOT_EMPTY)
    );
}

/// Codex P1 の窓: フォルダに書いた直後・印を置く前に、古い書き込みのハンドルで reparse に
/// された（`AFTER_CHILD_WRITTEN` のフックで `empty` を書いた直後に変える）→ 印を置けず、属性の
/// 読み直しで `ReparsePoint` として止まる（`Io` ではなく理由が分かる形）。緩め直すとジャンクション
/// だけ飛ばして残りは設定モードの形。外は変わらない。
#[test]
fn close_tree_aborts_when_a_dir_is_reparsed_right_after_it_is_tightened() {
    use windows_sys::Win32::Foundation::GENERIC_WRITE;
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
    if !is_elevated_admin() {
        eprintln!("skip: 所有者を移すのは管理者だけ（CI で確かめる）");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (outside, _victim) = outside_with_victim();
    let outside_before = tree_snapshot(outside.path());
    let root = dir.path().join("root");
    make_tree(&root);
    let empty = root.join("data").join("empty");
    std::fs::create_dir(&empty).unwrap();
    let old_empty = open_dir(&empty, GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE).unwrap();
    let mode = mode();
    let pinned = pinned(&root);
    let real_empty = pinned.final_path().join("data").join("empty");
    let swapped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (swapped_for_hook, outside_for_hook, old_raw) = (
        swapped.clone(),
        outside.path().to_path_buf(),
        old_empty.0 as usize,
    );
    crate::acl::imp::AFTER_CHILD_WRITTEN.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |handle| {
            let written = crate::acl::imp::final_path_of(handle).unwrap();
            if written.file_name().is_some_and(|n| n == "empty")
                && !swapped_for_hook.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                set_reparse_on_handle(old_raw as HANDLE, &outside_for_hook).unwrap();
            }
        }));
    });
    let aborted = pinned.close_tree(&mode, &no_stop, &mut |_| {});
    crate::acl::imp::AFTER_CHILD_WRITTEN.with(|hook| *hook.borrow_mut() = None);
    assert!(
        swapped.load(std::sync::atomic::Ordering::SeqCst),
        "フックが動いていない"
    );
    let abort = aborted.unwrap_err();
    assert_eq!(
        abort.reason,
        AbortReason::Problem(TreeProblem::ReparsePoint {
            path: real_empty.clone(),
            tag: TAG_MOUNT_POINT
        })
    );
    assert!(!outside.path().join(MARKER_NAME).exists(), "外に印を作った");
    let report = pinned.relax_tree(&mode, &mut |_| {}).unwrap();
    assert_eq!(
        report.skipped,
        vec![TreeProblem::ReparsePoint {
            path: real_empty.clone(),
            tag: TAG_MOUNT_POINT
        }]
    );
    drop(old_empty);
    std::fs::remove_dir(&empty).unwrap();
    expect_tree_shape(&root, mode.setting());
    assert_eq!(
        tree_snapshot(outside.path()),
        outside_before,
        "外が変わった"
    );
}

/// Fable M-1 の (b): 設定モードで利用者が置いた綴り違いのファイル `logs\.BANTO-SAFE-ROOT` は、
/// 2 周目で**印として**扱われる（印の記述子・利用者の ACE なし。綴りで見ると普通のファイルとして
/// 上書きされ、緩めると利用者が消せる印になっていた）。利用者がその偽の印を DELETE で握って
/// いれば 2 周目は `Busy` で止まる。緩めた後も利用者は綴り違いの名前で消せない。
#[test]
fn close_tree_treats_a_differently_spelled_marker_as_the_marker() {
    use windows_sys::Win32::Storage::FileSystem::{DELETE, FILE_READ_ATTRIBUTES, FILE_SHARE_READ};
    if !is_elevated_admin() {
        eprintln!("skip: 所有者を移すのは管理者だけ（CI で確かめる）");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    make_tree(&root);
    let upper = MARKER_NAME.to_ascii_uppercase();
    let fake = root.join("logs").join(&upper);
    std::fs::write(&fake, b"fake").unwrap();
    crate::root::win_tests::set_owner_to_me(&fake);
    let mode = mode();
    let pinned = pinned(&root);
    assert_eq!(
        pinned.scan_tree(&no_stop, &mut |_| {}),
        Ok(ScanReport {
            examined: TREE_OBJECTS + 1
        })
    );
    // 偽の印を DELETE で握っている間は Busy で止まる（締めた分は緩め直せる）。
    let holder = open_dir(&fake, DELETE | FILE_READ_ATTRIBUTES, FILE_SHARE_READ).unwrap();
    let abort = pinned.close_tree(&mode, &no_stop, &mut |_| {}).unwrap_err();
    assert_eq!(
        abort.reason,
        AbortReason::Problem(TreeProblem::Busy {
            path: pinned.final_path().join("logs").join(MARKER_NAME),
            os_error: 32
        })
    );
    pinned.relax_tree(&mode, &mut |_| {}).unwrap();
    drop(holder);
    // 放せば締まり、偽の印は印の記述子になる（`logs` に新しい印は作られない = 同じ物）。
    let progress = pinned.close_tree(&mode, &no_stop, &mut |_| {}).unwrap();
    assert_eq!(progress.written, CLOSED_OBJECTS, "{progress:?}");
    let fake_security = inspect_file(&fake).unwrap().unwrap();
    assert!(exposure(&fake_security).is_empty(), "{fake_security:?}");
    assert_eq!(fake_security.dacl.owner, ADMINISTRATORS_SID);
    assert_eq!(std::fs::read_dir(root.join("logs")).unwrap().count(), 2);
    every_dir_has_a_marker(&root);
    pinned.relax_tree(&mode, &mut |_| {}).unwrap();
    expect_tree_shape(&root, mode.setting());
    as_limited_user(|| {
        for name in [upper.as_str(), MARKER_NAME] {
            assert_eq!(
                std::fs::remove_file(root.join("logs").join(name))
                    .unwrap_err()
                    .raw_os_error(),
                Some(ACCESS_DENIED),
                "{name}"
            );
        }
    });
}

// --- オーナーレビュー P2: 印の照会の失敗にも Record の方針 -------------------------------

/// 1 周目は、印の席をフォルダが塞いでいる（`MarkerOccupied`）フォルダでも残りの子の列挙を
/// 続け、同じフォルダのハードリンク・別のフォルダの問題も一緒に集める（管理者でなくても走る）。
#[test]
fn scan_collects_marker_occupied_together_with_the_other_problems() {
    let dir = tempfile::tempdir().unwrap();
    let (outside, victim) = outside_with_victim();
    let root = dir.path().join("root");
    make_tree(&root);
    std::fs::create_dir(root.join("logs").join(MARKER_NAME.to_ascii_uppercase())).unwrap();
    std::fs::hard_link(&victim, root.join("logs").join("zz-hard.txt")).unwrap();
    junction(&root.join("data").join("jlink"), outside.path());
    let pinned = pinned(&root);
    let refusal = pinned.scan_tree(&no_stop, &mut |_| {}).unwrap_err();
    let ScanRefusal::Problems(problems) = refusal else {
        panic!("{refusal:?}");
    };
    let real = pinned.final_path().to_path_buf();
    assert_eq!(problems.len(), 3, "{problems:?}");
    assert!(
        problems
            .iter()
            .any(|p| matches!(p, TreeProblem::MarkerOccupied { .. })),
        "{problems:?}"
    );
    assert!(
        problems.iter().any(|p| matches!(
            p,
            TreeProblem::Hardlink { path, links: 2 } if path == &real.join("logs").join("zz-hard.txt")
        )),
        "同じフォルダの残りの子が集まっていない: {problems:?}"
    );
    assert!(
        problems.iter().any(|p| matches!(
            p,
            TreeProblem::ReparsePoint { path, .. } if path == &real.join("data").join("jlink")
        )),
        "{problems:?}"
    );
    std::fs::remove_dir(root.join("data").join("jlink")).unwrap();
}

/// オーナーレビュー P2: 印の席が開けない（SYSTEM だけの DACL）でも、`relax_tree` と
/// `reset_acl(Tree)` は中断せず、その子を `Denied` に記録して兄弟と親（RootLast のルートも）を
/// 処理する。昇格した非 SYSTEM の管理者（CI）で走る。
#[test]
fn relax_and_reset_record_an_unreadable_marker_and_process_the_siblings() {
    if !is_elevated_admin() {
        eprintln!("skip: 所有者を移すのは管理者だけ（CI で確かめる）");
        return;
    }
    let mode = mode();
    for which in ["relax", "reset"] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        make_tree(&root);
        let pinned = pinned(&root);
        pinned.close_tree(&mode, &no_stop, &mut |_| {}).unwrap();
        let marker = root.join("data").join(MARKER_NAME);
        // 管理者を拒否した印（SYSTEM だけ）。属性すら読めないので照会は開けない。
        crate::acl::tests::windows::set_dacl_sddl(&marker, DENY_ADMINS_SDDL);
        let denied_path = pinned.final_path().join("data").join(MARKER_NAME);
        let recorded = if which == "relax" {
            let report = pinned.relax_tree(&mode, &mut |_| {}).unwrap();
            assert_eq!(report.written, CLOSED_OBJECTS - 1, "{report:?}");
            report.skipped
        } else {
            let skips = pinned
                .reset_acl(mode.setting(), crate::acl::Recurse::Tree)
                .unwrap();
            assert!(
                skips.reparse.is_empty() && skips.hardlink.is_empty(),
                "{skips:?}"
            );
            skips
                .denied
                .iter()
                .map(|p| TreeProblem::Denied { path: p.clone() })
                .collect()
        };
        assert_eq!(
            recorded,
            vec![TreeProblem::Denied {
                path: denied_path.clone()
            }],
            "{which}"
        );
        // 兄弟・親・ルートは設定モードの形に戻っている（印だけは触っていない）。
        for path in [
            root.clone(),
            root.join("data"),
            root.join("data").join("d1.txt"),
            root.join("data").join("deep"),
            root.join("logs").join("l.txt"),
        ] {
            let security = inspect_file(&path).unwrap().unwrap();
            assert_eq!(
                security.dacl.owner,
                ADMINISTRATORS_SID,
                "{which}: {}",
                path.display()
            );
            if security.is_dir {
                check_dacl(&security.dacl, mode.setting())
                    .unwrap_or_else(|e| panic!("{which}: {}: {e:?}", path.display()));
            } else {
                check_file_shape(&security, mode.setting(), &path);
            }
        }
        // 片付け（親の FILE_DELETE_CHILD で消せる）。
        std::fs::remove_file(&marker).unwrap();
    }
}

/// オーナーレビュー P2（2 件目）: 印（綴り違い `.BANTO-SAFE-ROOT`）を呼び出し元に拒否の DACL に
/// しておき、親の列挙の後・ID で開く前（`BEFORE_CHILD_OPEN`）に所有者が許可へ変える。後段の
/// オープンは成功するが、席の同一性は親の列挙で解いているので印として扱われ、`relax_tree`・
/// `reset_acl(Tree)` の後も印の記述子のまま（利用者の ACE なし・記録なし）。昇格した管理者（CI）。
#[test]
fn relax_and_reset_keep_the_marker_descriptor_when_its_dacl_opens_late() {
    if !is_elevated_admin() {
        eprintln!("skip: 所有者を移すのは管理者だけ（CI で確かめる）");
        return;
    }
    let mode = mode();
    let upper = MARKER_NAME.to_ascii_uppercase();
    for which in ["relax", "reset"] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        make_tree(&root);
        // 設定モードで利用者が置いた綴り違いの印（所有者は利用者）。
        let fake = root.join("logs").join(&upper);
        std::fs::write(&fake, b"fake").unwrap();
        crate::root::win_tests::set_owner_to_me(&fake);
        let pinned = pinned(&root);
        pinned.close_tree(&mode, &no_stop, &mut |_| {}).unwrap();
        assert!(exposure(&inspect_file(&fake).unwrap().unwrap()).is_empty());
        // 呼び出し元（管理者）を拒否 → 照会のあと、開く直前に許可へ。
        crate::acl::tests::windows::set_dacl_sddl(&fake, DENY_ADMINS_SDDL);
        let fake_for_hook = fake.clone();
        let flipped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flipped_for_hook = flipped.clone();
        crate::acl::imp::BEFORE_CHILD_OPEN.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move |child: &Path| {
                if child
                    .file_name()
                    .is_some_and(|n| n.eq_ignore_ascii_case(MARKER_NAME))
                    && child
                        .parent()
                        .and_then(|p| p.file_name())
                        .is_some_and(|n| n == "logs")
                {
                    crate::acl::tests::windows::set_dacl_sddl(
                        &fake_for_hook,
                        "D:P(A;;FA;;;BA)(A;;FA;;;SY)",
                    );
                    flipped_for_hook.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            }));
        });
        let result = if which == "relax" {
            pinned
                .relax_tree(&mode, &mut |_| {})
                .map(|r| (r.written, r.skipped.len()))
                .map_err(|e| format!("{e:?}"))
        } else {
            pinned
                .reset_acl(mode.setting(), crate::acl::Recurse::Tree)
                .map(|s| (0, s.denied.len() + s.reparse.len() + s.hardlink.len()))
                .map_err(|e| format!("{e:?}"))
        };
        crate::acl::imp::BEFORE_CHILD_OPEN.with(|hook| *hook.borrow_mut() = None);
        assert!(
            flipped.load(std::sync::atomic::Ordering::SeqCst),
            "{which}: フックが動いていない"
        );
        let (written, skipped) = result.unwrap_or_else(|e| panic!("{which}: {e}"));
        assert_eq!(skipped, 0, "{which}: 記録された");
        if which == "relax" {
            assert_eq!(written, CLOSED_OBJECTS, "{which}");
        }
        let after = inspect_file(&fake).unwrap().unwrap();
        assert!(
            exposure(&after).is_empty(),
            "{which}: 印に利用者の ACE が付いた: {after:?}"
        );
        assert_eq!(after.dacl.owner, ADMINISTRATORS_SID, "{which}");
        expect_tree_shape(&root, mode.setting());
    }
}

/// 見張り（`SecurityWatch`、設計 §2.9 (c)、PR-3）: 何も変わらなければ `Timeout`、パスで子の
/// DACL を変えると `Changed`（変わった名前）か `Overflow`、別のスレッドの止める合図で
/// `Stopped`（以後も `Stopped`）。見張り用のハンドルは握ったルートと同じオブジェクト。
#[test]
fn the_security_watch_reports_a_path_acl_change_and_stops_on_signal() {
    use std::time::Duration;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    make_tree(&root);
    let held = pinned(&root);
    let mut watch = held.watch_security().unwrap();
    assert_eq!(
        watch.wait(Duration::from_millis(300)).unwrap(),
        WatchEvent::Timeout
    );
    let child = root.join("data");
    reset_dir_acl(&child, &DirAccess::with_readers([me()])).unwrap();
    match watch.wait(Duration::from_secs(5)).unwrap() {
        WatchEvent::Changed(names) => {
            // パスの書き直しは中の子へも伝わるので、最初の通知が `data\.banto-safe-root` の
            // こともある。`data` の下のどれかであればよい。
            assert!(
                names
                    .iter()
                    .any(|name| name.to_ascii_lowercase().starts_with("data")),
                "{names:?}"
            );
        }
        WatchEvent::Overflow => {}
        other => panic!("DACL の変更が見張りに来ない: {other:?}"),
    }
    // 伝播の残りの通知（中の子の分）を吸い取ってから、止める合図を試す。
    loop {
        match watch.wait(Duration::from_millis(500)).unwrap() {
            WatchEvent::Changed(_) | WatchEvent::Overflow => continue,
            WatchEvent::Timeout => break,
            WatchEvent::Stopped => panic!("止めていないのに Stopped"),
        }
    }
    let stopper = watch.stopper();
    let signal = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        stopper.stop();
    });
    assert_eq!(
        watch.wait(Duration::from_secs(10)).unwrap(),
        WatchEvent::Stopped
    );
    signal.join().unwrap();
    assert_eq!(
        watch.wait(Duration::from_millis(100)).unwrap(),
        WatchEvent::Stopped
    );
    drop(watch);
    reset_dir_acl(&child, &DirAccess::with_install_user(me())).unwrap();
}
