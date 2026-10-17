//! `grant-profile-acl`を本物の ACL の操作で（ルートは一時フォルダ、マシンの環境は
//! 偽物）。#626 の受入条件「外の DACL が変わらない」の反証 5 種（中のジャンクション・
//! 末尾のリンク・途中の要素のジャンクション・中のハードリンク・その場の FSCTL）と、
//! 書き直した形の確かめ。
//!
//! - **外の対象は継承を受け入れる DACL**（一時フォルダの下に普通に作ったもの。各テストの
//!   最初に`!protected`を確かめる）。保護された対象では、自動の伝播があっても変わらず
//!   偽の緑になるため（実装チェックリスト §5）。
//! - 書き直しは所有者を Administrators に移すので、昇格した管理者でしか走らないものは
//!   [`elevated_admin`] で理由を出して skip する（GitHub の Windows のランナーは管理者
//!   なので CI では走る）。断る経路のテストは管理者でなくても走る。
//! - 一時パスは**本当のパス（長い名前）**で組み立てる（CI の TEMP は`RUNNER~1`の短い
//!   名前で、本当のパスの比較が食い違って断る - #622 で落ちた型）。

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use banto_safe_root::acl::{
    check_dacl, read_dir_dacl, ObservedDacl, PinnedDir, PinnedDirInfo, ResetSkips,
    ADMINISTRATORS_SID, INHERIT_FLAGS,
};
use banto_safe_root::guard::{KnownFolders, MachineEnvironment};
use banto_safe_root::{DirAccess, InstallUserSid};
use banto_service_control::ConsoleLine;

use super::super::{
    grant_profile_acl, open_target_profile, preflight, AccountKind, CreateHow, FileFacts,
    HubElevOps, OpenTargetError, PinnedProfileDir, Refused,
};
use super::{RegistryMachineEnvironment, WindowsHubElevOps};

fn me() -> InstallUserSid {
    banto_safe_root::acl::current_user_sid().expect("今の利用者の SID")
}

/// 昇格した管理者として走っているか。違えば理由を出す（呼ぶ側が skip する）。`CI`の環境変数が
/// あるのに管理者でなければ panic する（#626 PR-B の監査 B-3）。
fn elevated_admin(test: &str) -> bool {
    let admin = banto_service_elev::is_current_process_admin().unwrap_or(false);
    // CI（GitHub の Windows のランナー）は管理者のはず。管理者でなければ反証が黙って
    // 飛ばされるので、skip せずに落とす（#626 PR-B の監査 B-3）。
    if !admin && std::env::var_os("CI").is_some() {
        panic!("{test}: CI なのに昇格した管理者ではない（書き直しの反証が走らない）");
    }
    if !admin {
        eprintln!(
            "skip {test}: 昇格した管理者ではないので、所有者を Administrators に移せない（CI で確かめる）"
        );
    }
    admin
}

/// tempfile のパスを本当のパス（長い名前。`\\?\`を外す）にする。
fn long(path: &Path) -> PathBuf {
    let c = std::fs::canonicalize(path).unwrap();
    PathBuf::from(c.to_string_lossy().trim_start_matches(r"\\?\").to_string())
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

/// マシンの環境の偽物: `BANTO_HUB_ROOT`だけ一時フォルダを返し、既知のフォルダは本物。
struct TempEnv {
    root: PathBuf,
}

impl MachineEnvironment for TempEnv {
    fn machine_variable(&self, _name: &str) -> Result<Option<String>, String> {
        Ok(Some(self.root.to_string_lossy().into_owned()))
    }
    fn program_data(&self) -> Result<PathBuf, String> {
        RegistryMachineEnvironment.program_data()
    }
    fn known_folders(&self) -> Result<KnownFolders, String> {
        RegistryMachineEnvironment.known_folders()
    }
}

type Hook = Box<dyn FnOnce()>;

/// 本物の`PinnedDir`に、書き直しの直前に走るフックを付けたもの（その場のすり替え）。
struct Hooked {
    inner: PinnedDir,
    hook: RefCell<Option<Hook>>,
}

impl PinnedProfileDir for Hooked {
    fn info(&self) -> PinnedDirInfo {
        PinnedProfileDir::info(&self.inner)
    }
    fn final_path(&self) -> PathBuf {
        PinnedProfileDir::final_path(&self.inner)
    }
    fn read_dacl(&self) -> Result<ObservedDacl, String> {
        PinnedProfileDir::read_dacl(&self.inner)
    }
    fn reset_acl_tree(&self, access: &DirAccess) -> Result<ResetSkips, String> {
        if let Some(hook) = self.hook.borrow_mut().take() {
            hook();
        }
        self.inner.reset_acl_tree(access)
    }
}

/// フォルダの操作・利用者の確かめは本物（`WindowsHubElevOps`）、ルートは一時フォルダ、
/// 稼働中の確かめは「使っていない」。
struct TempOps {
    env: TempEnv,
    /// このパスを開いたときに付けるフック。
    hook: RefCell<Option<(PathBuf, Hook)>>,
    /// 子を作る直前に 1 回だけ走るフック（親のその場のすり替え）。
    before_create: RefCell<Option<Hook>>,
}

impl TempOps {
    fn new(root: &Path) -> Self {
        Self {
            env: TempEnv {
                root: root.to_path_buf(),
            },
            hook: RefCell::new(None),
            before_create: RefCell::new(None),
        }
    }
}

impl HubElevOps for TempOps {
    type Env = TempEnv;
    type Pinned = Hooked;

    fn machine_env(&self) -> &TempEnv {
        &self.env
    }
    fn current_user_sid(&self) -> Result<InstallUserSid, String> {
        WindowsHubElevOps.current_user_sid()
    }
    fn lookup_account_sid(&self, name: &str) -> Result<Option<String>, String> {
        WindowsHubElevOps.lookup_account_sid(name)
    }
    fn account_kind(&self, sid: &InstallUserSid) -> AccountKind {
        WindowsHubElevOps.account_kind(sid)
    }
    type ProfileHold = ();
    fn hold_profile(&self, _profile_id: &str) -> Result<Option<()>, String> {
        Ok(Some(()))
    }
    fn pin_dir(&self, dir: &Path) -> Result<Option<Hooked>, String> {
        let pinned = WindowsHubElevOps.pin_dir(dir)?;
        Ok(pinned.map(|inner| {
            let mut slot = self.hook.borrow_mut();
            let hook = match slot.take() {
                Some((path, hook)) if path == dir => Some(hook),
                other => {
                    *slot = other;
                    None
                }
            };
            Hooked {
                inner,
                hook: RefCell::new(hook),
            }
        }))
    }
    fn create_child(
        &self,
        parent: &Hooked,
        name: &str,
        how: CreateHow,
    ) -> Result<Option<Hooked>, String> {
        if let Some(hook) = self.before_create.borrow_mut().take() {
            hook();
        }
        Ok(WindowsHubElevOps
            .create_child(&parent.inner, name, how)?
            .map(|inner| Hooked {
                inner,
                hook: RefCell::new(None),
            }))
    }
    fn open_child(&self, parent: &Hooked, name: &str) -> Result<Option<Hooked>, String> {
        Ok(WindowsHubElevOps
            .open_child(&parent.inner, name)?
            .map(|inner| Hooked {
                inner,
                hook: RefCell::new(None),
            }))
    }
    type HeldFile = super::HeldFile;
    fn pin_file(&self, path: &Path) -> Result<Option<(super::HeldFile, FileFacts)>, String> {
        WindowsHubElevOps.pin_file(path)
    }
}

fn grant(ops: &TempOps, user: Option<&str>) -> (Result<(), Refused>, Vec<ConsoleLine>) {
    let mut lines = Vec::new();
    let result = {
        let mut out = |line| lines.push(line);
        preflight(ops, user, None, &mut out).and_then(|pre| grant_profile_acl(ops, &pre, &mut out))
    };
    (result, lines)
}

fn has_line(lines: &[ConsoleLine], needle: &str) -> bool {
    lines.iter().any(|line| match line {
        ConsoleLine::Stdout(text) | ConsoleLine::Stderr(text) => text.contains(needle),
    })
}

/// ファイル用の確かめ（ファイルへは継承のフラグを外した記述子を書く）。
fn check_file(path: &Path, access: &DirAccess) {
    let observed = read_dir_dacl(path).unwrap();
    let mut dacl = observed.clone();
    for ace in dacl.aces.as_mut().expect("DACL がある") {
        assert_eq!(ace.flags, 0, "{}: {observed:?}", path.display());
        ace.flags = INHERIT_FLAGS;
    }
    check_dacl(&dacl, access).unwrap_or_else(|e| panic!("{}: {e:?}", path.display()));
    assert_eq!(dacl.owner, ADMINISTRATORS_SID, "{}", path.display());
}

fn check_dir(path: &Path, access: &DirAccess) {
    let dacl = read_dir_dacl(path).unwrap();
    check_dacl(&dacl, access).unwrap_or_else(|e| panic!("{}: {e:?}", path.display()));
    assert_eq!(dacl.owner, ADMINISTRATORS_SID, "{}", path.display());
}

/// 外の対象（継承を受け入れる DACL）: `outside`・`outside\f.txt`・`outside\sub`。
fn make_outside(parent: &Path) -> (PathBuf, Vec<PathBuf>) {
    let outside = parent.join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("f.txt"), b"x").unwrap();
    std::fs::create_dir(outside.join("sub")).unwrap();
    let watched = vec![outside.clone(), outside.join("f.txt"), outside.join("sub")];
    for path in &watched {
        let dacl = read_dir_dacl(path).unwrap();
        assert!(
            !dacl.protected,
            "外の対象は継承を受け入れる DACL にする（保護されていると偽の緑になる）: {}",
            path.display()
        );
    }
    (outside, watched)
}

fn snapshot(paths: &[PathBuf]) -> Vec<ObservedDacl> {
    paths.iter().map(|p| read_dir_dacl(p).unwrap()).collect()
}

fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// 無い profile は作り（ルート・`profiles\`は親から継承する普通のフォルダ、profile は
/// 保護された DACL・所有者 Administrators）、2 回目も同じ形（冪等）。ルートと`profiles\`
/// の DACL は書かない。
#[test]
fn creates_a_missing_profile_and_does_not_write_root_or_profiles() {
    if !elevated_admin("creates_a_missing_profile_and_does_not_write_root_or_profiles") {
        return;
    }
    let parent = tempfile::tempdir().unwrap();
    let root = long(parent.path()).join("BantoHub");
    let ops = TempOps::new(&root);
    let access = DirAccess::with_install_user(me());
    for round in 0..2 {
        let (result, lines) = grant(&ops, None);
        assert_eq!(result, Ok(()), "{round}: {lines:#?}");
        let profile = root.join("profiles").join("default");
        check_dir(&profile, &access);
        for dir in [root.clone(), root.join("profiles")] {
            assert!(
                !read_dir_dacl(&dir).unwrap().protected,
                "{}: ルート・profiles は書かない",
                dir.display()
            );
        }
        let expected = if round == 0 {
            "作りました"
        } else {
            "ACL を書き直しました"
        };
        assert!(has_line(&lines, expected), "{lines:#?}");
    }
}

/// 既にある profile の木（DB・-wal 相当・孫まで）を書き直す。ルート・`profiles\`の
/// DACL は前後で同じ。
#[test]
fn rewrites_an_existing_profile_tree() {
    if !elevated_admin("rewrites_an_existing_profile_tree") {
        return;
    }
    let parent = tempfile::tempdir().unwrap();
    let root = long(parent.path()).join("BantoHub");
    let profile = root.join("profiles").join("default");
    std::fs::create_dir_all(profile.join("config")).unwrap();
    std::fs::write(profile.join("config").join("banto-hub.sqlite3"), b"db").unwrap();
    std::fs::write(profile.join("config").join("banto-hub.sqlite3-wal"), b"w").unwrap();
    std::fs::create_dir_all(profile.join("data").join("sub")).unwrap();
    std::fs::write(profile.join("data").join("sub").join("f.bin"), b"x").unwrap();
    let upper = vec![root.clone(), root.join("profiles")];
    let before = snapshot(&upper);

    let (result, lines) = grant(&TempOps::new(&root), None);
    assert_eq!(result, Ok(()), "{lines:#?}");
    let access = DirAccess::with_install_user(me());
    for dir in [
        profile.clone(),
        profile.join("config"),
        profile.join("data"),
        profile.join("data").join("sub"),
    ] {
        check_dir(&dir, &access);
    }
    for file in [
        profile.join("config").join("banto-hub.sqlite3"),
        profile.join("config").join("banto-hub.sqlite3-wal"),
        profile.join("data").join("sub").join("f.bin"),
    ] {
        check_file(&file, &access);
    }
    assert_eq!(
        snapshot(&upper),
        before,
        "ルート・profiles の DACL が変わった"
    );
}

/// 反証 1（中のジャンクション）: profile の中のジャンクションの先（継承を受け入れる
/// DACL）は、書き直しで変わらない。飛ばした行が出る。
#[test]
fn an_inner_junction_target_is_not_rewritten() {
    if !elevated_admin("an_inner_junction_target_is_not_rewritten") {
        return;
    }
    let parent = tempfile::tempdir().unwrap();
    let base = long(parent.path());
    let root = base.join("BantoHub");
    let profile = root.join("profiles").join("default");
    std::fs::create_dir_all(&profile).unwrap();
    let (outside, watched) = make_outside(&base);
    junction(&profile.join("x"), &outside);
    let before = snapshot(&watched);

    let (result, lines) = grant(&TempOps::new(&root), None);
    assert_eq!(result, Ok(()), "{lines:#?}");
    assert_eq!(snapshot(&watched), before, "ジャンクションの先が変わった");
    assert!(
        has_line(&lines, "ジャンクションかシンボリックリンクなので"),
        "{lines:#?}"
    );
    std::fs::remove_dir(profile.join("x")).unwrap();
}

/// 反証 2（末尾のリンク）: profile そのものがジャンクションなら断り、先は変えず、先に
/// 何も作らない（管理者でなくても走る）。
#[test]
fn a_profile_that_is_a_junction_is_refused() {
    let parent = tempfile::tempdir().unwrap();
    let base = long(parent.path());
    let root = base.join("BantoHub");
    std::fs::create_dir_all(root.join("profiles")).unwrap();
    let (outside, watched) = make_outside(&base);
    let profile = root.join("profiles").join("default");
    junction(&profile, &outside);
    let before = (snapshot(&watched), listing(&outside));

    let (result, lines) = grant(&TempOps::new(&root), None);
    assert_eq!(result, Err(Refused), "{lines:#?}");
    assert!(has_line(&lines, "reparse point"), "{lines:#?}");
    assert_eq!((snapshot(&watched), listing(&outside)), before);
    std::fs::remove_dir(&profile).unwrap();
}

/// 反証 2'（ルートそのものがジャンクション）: 断り、先に`profiles\`も作らない。
#[test]
fn a_root_that_is_a_junction_is_refused() {
    let parent = tempfile::tempdir().unwrap();
    let base = long(parent.path());
    let (outside, watched) = make_outside(&base);
    let root = base.join("BantoHub");
    junction(&root, &outside);
    let before = (snapshot(&watched), listing(&outside));

    let (result, lines) = grant(&TempOps::new(&root), None);
    assert_eq!(result, Err(Refused), "{lines:#?}");
    assert!(has_line(&lines, "reparse point"), "{lines:#?}");
    assert_eq!((snapshot(&watched), listing(&outside)), before);
    std::fs::remove_dir(&root).unwrap();
}

/// 反証 3（途中の要素のジャンクション）: ルートの親をジャンクションにして別の場所の
/// `BantoHub`を開かせても、本当のパスの比較で断る。先の profile とその中は変わらない
/// （管理者でなくても走る）。
#[test]
fn a_junction_in_the_middle_of_the_path_is_refused() {
    let parent = tempfile::tempdir().unwrap();
    let base = long(parent.path());
    let elsewhere = base.join("elsewhere");
    let real_profile = elsewhere.join("BantoHub").join("profiles").join("default");
    std::fs::create_dir_all(&real_profile).unwrap();
    std::fs::write(real_profile.join("f.txt"), b"x").unwrap();
    let watched = vec![real_profile.clone(), real_profile.join("f.txt")];
    for path in &watched {
        assert!(!read_dir_dacl(path).unwrap().protected);
    }
    let before = snapshot(&watched);
    let via = base.join("via");
    junction(&via, &elsewhere);

    let (result, lines) = grant(&TempOps::new(&via.join("BantoHub")), None);
    assert_eq!(result, Err(Refused), "{lines:#?}");
    assert!(has_line(&lines, "本当の場所"), "{lines:#?}");
    assert_eq!(snapshot(&watched), before);
    std::fs::remove_dir(&via).unwrap();
}

/// 反証 4（中のハードリンク）: profile の中のハードリンクの先（外のファイル）の DACL は
/// 変わらない。飛ばした行が出る。
#[test]
fn a_hardlink_target_is_not_rewritten() {
    if !elevated_admin("a_hardlink_target_is_not_rewritten") {
        return;
    }
    let parent = tempfile::tempdir().unwrap();
    let base = long(parent.path());
    let root = base.join("BantoHub");
    let profile = root.join("profiles").join("default");
    std::fs::create_dir_all(&profile).unwrap();
    let (outside, _) = make_outside(&base);
    let target = outside.join("f.txt");
    std::fs::hard_link(&target, profile.join("f.txt")).unwrap();
    let before = read_dir_dacl(&target).unwrap();

    let (result, lines) = grant(&TempOps::new(&root), None);
    assert_eq!(result, Ok(()), "{lines:#?}");
    assert_eq!(read_dir_dacl(&target).unwrap(), before);
    assert!(has_line(&lines, "ハードリンク"), "{lines:#?}");
}

/// 反証 5（その場の FSCTL）: profile を開いて確かめた後・書き直しの直前に、別のハンドル
/// で profile を`FSCTL_SET_REPARSE_POINT`で外へのジャンクションにしても、書き直しは
/// 開いたハンドル（リンクそのもの）に向き、外の中身は変わらない。
#[test]
fn an_in_place_fsctl_swap_stays_on_the_link() {
    if !elevated_admin("an_in_place_fsctl_swap_stays_on_the_link") {
        return;
    }
    let parent = tempfile::tempdir().unwrap();
    let base = long(parent.path());
    let root = base.join("BantoHub");
    let profile = root.join("profiles").join("default");
    std::fs::create_dir_all(&profile).unwrap();
    let (outside, watched) = make_outside(&base);
    let before = snapshot(&watched);
    let ops = TempOps::new(&root);
    let swapped = std::rc::Rc::new(std::cell::Cell::new(false));
    {
        let (profile, outside, swapped) = (profile.clone(), outside.clone(), swapped.clone());
        *ops.hook.borrow_mut() = Some((
            profile.clone(),
            Box::new(move || {
                swapped.set(set_reparse_to(&profile, &outside).is_ok());
            }),
        ));
    }

    let (_result, lines) = grant(&ops, None);
    assert!(
        swapped.get(),
        "FSCTL_SET_REPARSE_POINT が成功していない（反証にならない）: {lines:#?}"
    );
    assert_eq!(snapshot(&watched), before, "その場のすり替えで外が変わった");
    let _ = std::fs::remove_dir(&profile);
}

/// 稼働中の確かめ: mutex を持っている間だけ「使用中」（管理者でなくても走る）。
#[test]
fn the_profile_mutex_is_held_and_released() {
    let id = format!("t626-{}-{}", std::process::id(), line!());
    let name = crate::profile_paths::mutex_name(&id);
    let held = WindowsHubElevOps
        .hold_profile(&id)
        .unwrap()
        .expect("取れる");
    // 持っている間は、Hub と同じ口（try_acquire）も、別のスレッドからも取れない。
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(
        banto_instance_lock::try_acquire(&name, &dir.path().join("a.lock"), "hub"),
        Err(banto_instance_lock::InstanceLockError::AlreadyHeld { .. })
    ));
    let other = id.clone();
    assert!(
        std::thread::spawn(move || WindowsHubElevOps.hold_profile(&other).unwrap().is_none())
            .join()
            .unwrap()
    );
    drop(held);
    // 放せば Hub が取れる（mutex は残らない）。
    let guard = banto_instance_lock::try_acquire(&name, &dir.path().join("b.lock"), "hub")
        .expect("放した後は Hub が取れる");
    // Hub が持っている間は、elev は使用中として扱う。
    assert!(WindowsHubElevOps.hold_profile(&id).unwrap().is_none());
    drop(guard);
}

/// 本物の名前の解決: 組み込みのアカウント・無い名前・グループの SID は断り、ルートも
/// 作らない（管理者でなくても走る）。
#[test]
fn non_user_accounts_are_refused_without_side_effects() {
    let parent = tempfile::tempdir().unwrap();
    let root = long(parent.path()).join("BantoHub");
    let ops = TempOps::new(&root);
    for user in [
        "SYSTEM",
        "banto-no-such-user-626",
        "S-1-1-0",
        "S-1-5-32-545",
        "S-1-5-18",
    ] {
        let (result, lines) = grant(&ops, Some(user));
        assert_eq!(result, Err(Refused), "{user}: {lines:#?}");
        assert!(!root.exists(), "{user}: ルートを作った");
    }
    // 自分（利用者のアカウント）は名前でも SID でも通る（確かめだけ。書き直さない）。
    let mut lines = Vec::new();
    let me_sid = me();
    let pre = preflight(&ops, Some(me_sid.as_str()), None, &mut |l| lines.push(l));
    assert_eq!(pre.map(|p| p.user), Ok(me_sid), "{lines:#?}");
}

/// `dir`（空のフォルダ）を`target`へのジャンクションにする（`FSCTL_SET_REPARSE_POINT`。
/// テスト専用 - banto-safe-root のテストの同名の関数と同じ）。
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

// --- B-5: reset-password・revert-to-commissioning の対象（読むだけ。管理者でなくても走る） --

/// 対象の profile の木（ルート・profiles・default・config と DB のファイル）を作る。
fn make_target(base: &Path) -> PathBuf {
    let root = base.join("BantoHub");
    let config = root.join("profiles").join("default").join("config");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("banto-hub.sqlite3"), b"db").unwrap();
    root
}

fn open_target(root: &Path) -> Result<PathBuf, OpenTargetError> {
    open_target_profile(&TempOps::new(root), None).map(|(paths, _held)| paths.db_path)
}

#[test]
fn the_target_db_opens_when_nothing_is_linked() {
    let parent = tempfile::tempdir().unwrap();
    let root = make_target(&long(parent.path()));
    let db = open_target(&root).unwrap();
    assert_eq!(
        db,
        root.join("profiles")
            .join("default")
            .join("config")
            .join("banto-hub.sqlite3")
    );
}

/// 持っている間は DB のファイルを消せない・名前を変えられない（すり替えられない）。
/// SQLite と同じ読み書き・共有読み書きでは開ける。
#[test]
fn a_held_target_db_cannot_be_swapped_but_can_be_opened() {
    let parent = tempfile::tempdir().unwrap();
    let root = make_target(&long(parent.path()));
    let (paths, held) = open_target_profile(&TempOps::new(&root), None).unwrap();
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&paths.db_path)
        .expect("SQLite と同じ開き方は通る");
    assert!(std::fs::remove_file(&paths.db_path).is_err());
    assert!(std::fs::rename(&paths.db_path, paths.db_path.with_extension("old")).is_err());
    drop(held);
    std::fs::rename(&paths.db_path, paths.db_path.with_extension("old")).unwrap();
}

/// config がジャンクションなら、その先の DB を開かずに断る。
#[test]
fn a_config_junction_is_refused_before_opening_the_db() {
    let parent = tempfile::tempdir().unwrap();
    let base = long(parent.path());
    let root = base.join("BantoHub");
    let profile = root.join("profiles").join("default");
    std::fs::create_dir_all(&profile).unwrap();
    let outside = base.join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("banto-hub.sqlite3"), b"other").unwrap();
    junction(&profile.join("config"), &outside);
    match open_target(&root) {
        Err(OpenTargetError::Unsafe(text)) => assert!(text.contains("reparse point"), "{text}"),
        other => panic!("{other:?}"),
    }
    std::fs::remove_dir(profile.join("config")).unwrap();
}

/// 途中の要素（ルートの親）がジャンクションなら、本当のパスの比較で断る。
#[test]
fn a_target_behind_a_junction_in_the_middle_is_refused() {
    let parent = tempfile::tempdir().unwrap();
    let base = long(parent.path());
    let elsewhere = base.join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    make_target(&elsewhere);
    let via = base.join("via");
    junction(&via, &elsewhere);
    match open_target(&via.join("BantoHub")) {
        Err(OpenTargetError::Unsafe(text)) => assert!(text.contains("本当の場所"), "{text}"),
        other => panic!("{other:?}"),
    }
    std::fs::remove_dir(&via).unwrap();
}

/// DB のファイルがハードリンクなら断る。
#[test]
fn a_hardlinked_target_db_is_refused() {
    let parent = tempfile::tempdir().unwrap();
    let base = long(parent.path());
    let root = base.join("BantoHub");
    let config = root.join("profiles").join("default").join("config");
    std::fs::create_dir_all(&config).unwrap();
    let outside = base.join("outside.sqlite3");
    std::fs::write(&outside, b"other").unwrap();
    std::fs::hard_link(&outside, config.join("banto-hub.sqlite3")).unwrap();
    match open_target(&root) {
        Err(OpenTargetError::Unsafe(text)) => assert!(text.contains("ハードリンク"), "{text}"),
        other => panic!("{other:?}"),
    }
}

// --- #626 PR-B の再レビュー 1: 無いルートを確かめ済みの親の下だけに作る ---------------

/// ルートが無く、その親（途中の要素）が外へのジャンクションのとき、外に何も作らず、外の
/// 一覧と DACL も変えずに断る（パスで作ってから断っていた穴の回帰。管理者でなくても走る）。
#[test]
fn a_missing_root_behind_a_junction_creates_nothing_outside() {
    let parent = tempfile::tempdir().unwrap();
    let base = long(parent.path());
    let (outside, mut watched) = make_outside(&base);
    std::fs::create_dir(outside.join("data")).unwrap();
    watched.push(outside.join("data"));
    let before = (
        snapshot(&watched),
        listing(&outside),
        listing(&outside.join("data")),
    );
    let via = base.join("via");
    junction(&via, &outside);
    // (a) ルートの親そのものがジャンクション、(b) ルートの親の親がジャンクション。
    for (root, needle) in [
        (via.join("BantoHub"), "reparse point"),
        (via.join("data").join("BantoHub"), "本当の場所"),
    ] {
        let (result, lines) = grant(&TempOps::new(&root), None);
        assert_eq!(result, Err(Refused), "{lines:#?}");
        assert!(has_line(&lines, needle), "{lines:#?}");
        assert!(has_line(&lines, "何も作らずに止めます"), "{lines:#?}");
        assert_eq!(
            (
                snapshot(&watched),
                listing(&outside),
                listing(&outside.join("data"))
            ),
            before
        );
    }
    std::fs::remove_dir(&via).unwrap();
}

/// ルートの親も無ければ、何も作らずに断る（`create_dir_all`をしない）。
#[test]
fn a_missing_root_parent_creates_nothing() {
    let parent = tempfile::tempdir().unwrap();
    let base = long(parent.path());
    let root = base.join("missing").join("BantoHub");
    let (result, lines) = grant(&TempOps::new(&root), None);
    assert_eq!(result, Err(Refused), "{lines:#?}");
    assert!(has_line(&lines, "親から作ることはしません"), "{lines:#?}");
    assert!(!base.join("missing").exists());
    assert_eq!(listing(&base), Vec::<String>::new());
}

/// ルートの親を確かめた後・ルートを作る直前に、親をその場で外へのジャンクションに
/// 変えても（FSCTL）、親のハンドルを基準に作るので外には何もできない（パスで作り直すと
/// 外にできる - 反証で確かめた。管理者でなくても走る）。
#[test]
fn a_root_parent_swapped_before_create_does_not_leak() {
    let parent = tempfile::tempdir().unwrap();
    let base = long(parent.path());
    let (outside, watched) = make_outside(&base);
    let before = (snapshot(&watched), listing(&outside));
    let root_parent = base.join("rp");
    std::fs::create_dir(&root_parent).unwrap();
    let ops = TempOps::new(&root_parent.join("BantoHub"));
    let swapped = std::rc::Rc::new(std::cell::Cell::new(false));
    {
        let (root_parent, outside, swapped) =
            (root_parent.clone(), outside.clone(), swapped.clone());
        *ops.before_create.borrow_mut() = Some(Box::new(move || {
            swapped.set(set_reparse_to(&root_parent, &outside).is_ok());
        }));
    }
    let (result, lines) = grant(&ops, None);
    if !swapped.get() {
        if std::env::var_os("CI").is_some() {
            panic!("CI で FSCTL_SET_REPARSE_POINT が失敗した");
        }
        eprintln!("skip: FSCTL_SET_REPARSE_POINT に権限が無い - CI で確かめる");
        return;
    }
    assert_eq!(result, Err(Refused), "{lines:#?}");
    assert_eq!((snapshot(&watched), listing(&outside)), before);
    assert!(!outside.join("BantoHub").exists());
    let _ = std::fs::remove_dir(&root_parent);
}
