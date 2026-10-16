//! `acl::guard` の純関数のテスト（全 OS。chronogazer-elev の同じテストを移して、文言の
//! 代わりに型の付いた理由を確かめる形にした - #626 PR-A）。

use std::path::PathBuf;

use super::*;
use crate::acl::{ObservedAce, ObservedDacl, ACCESS_ALLOWED_ACE_TYPE, FULL_ACCESS_MASK};

const USER: &str = "S-1-5-21-1111111111-2222222222-3333333333-1001";
const OTHER: &str = "S-1-5-21-1111111111-2222222222-3333333333-1002";
const ADMIN: &str = "S-1-5-21-1111111111-2222222222-3333333333-500";
const STRANGER: &str = "S-1-5-21-1111111111-2222222222-3333333333-1003";
const GROUP: &str = "S-1-5-21-1111111111-2222222222-3333333333-513";

const VAR: &str = "PRODUCT_ROOT";
const PRODUCT: &str = "Product";

fn sid(text: &str) -> InstallUserSid {
    InstallUserSid::parse(text).unwrap()
}

// --- ルート（マシンの環境） ---------------------------------------------------------

struct FakeEnv {
    root: Result<Option<String>, String>,
    program_data: Result<PathBuf, String>,
    known: Result<KnownFolders, String>,
}

impl MachineEnvironment for FakeEnv {
    fn machine_variable(&self, name: &str) -> Result<Option<String>, String> {
        // 呼ぶ側が渡した変数名だけを読む。
        assert_eq!(name, VAR);
        self.root.clone()
    }
    fn program_data(&self) -> Result<PathBuf, String> {
        self.program_data.clone()
    }
    fn known_folders(&self) -> Result<KnownFolders, String> {
        self.known.clone()
    }
}

/// この PC 風の既知のフォルダ（Windows は本物と同じ形の文字列、それ以外は似せたもの）。
fn known() -> KnownFolders {
    #[cfg(windows)]
    let (inside, itself) = (
        vec![
            r"C:\Windows",
            r"C:\Windows\system32",
            r"C:\Windows\SysWOW64",
            r"C:\Program Files",
            r"C:\Program Files (x86)",
            r"C:\Program Files\Common Files",
            r"C:\Program Files (x86)\Common Files",
            r"C:\Users\Public",
        ],
        vec![r"C:\ProgramData", r"C:\Users"],
    );
    #[cfg(not(windows))]
    let (inside, itself) = (
        vec!["/win", "/win/system32", "/pf", "/pf86", "/users/public"],
        vec!["/var/lib", "/users"],
    );
    KnownFolders {
        forbidden_inside: inside.into_iter().map(PathBuf::from).collect(),
        forbidden_itself: itself.into_iter().map(PathBuf::from).collect(),
    }
}

fn env(root: Result<Option<&str>, &str>, program_data: Result<&str, &str>) -> FakeEnv {
    FakeEnv {
        root: root.map(|v| v.map(str::to_string)).map_err(str::to_string),
        program_data: program_data.map(PathBuf::from).map_err(str::to_string),
        known: Ok(known()),
    }
}

fn resolve(env: &FakeEnv) -> Result<PathBuf, MachineRootError> {
    resolve_machine_root(env, VAR, PRODUCT)
}

#[cfg(windows)]
const PD: &str = r"C:\ProgramData";
#[cfg(not(windows))]
const PD: &str = "/var/lib";
#[cfg(windows)]
const CUSTOM: &str = r"D:\Product";
#[cfg(not(windows))]
const CUSTOM: &str = "/srv/Product";

#[test]
fn the_root_comes_only_from_the_machine_environment() {
    // 無い・空 → ProgramData + 製品のフォルダ。
    for root in [Ok(None), Ok(Some(""))] {
        assert_eq!(
            resolve(&env(root, Ok(PD))),
            Ok(PathBuf::from(PD).join(PRODUCT))
        );
    }
    // 指定があればそれ（ProgramData は読まない）。
    assert_eq!(
        resolve(&env(Ok(Some(CUSTOM)), Err("読まないはず"))),
        Ok(PathBuf::from(CUSTOM))
    );
}

/// 反証: 読めない値で ProgramData へ落とさない・展開の要る値・相対パス・`..` を断る・
/// ProgramData・既知のフォルダが取れなければ止める。理由は種類ごとに分かれる。
#[test]
fn unusable_machine_roots_are_refused_with_typed_reasons() {
    assert_eq!(
        resolve(&env(Err("os error 5"), Ok(PD))),
        Err(MachineRootError::VariableUnreadable("os error 5".into()))
    );
    for value in [r"%SystemDrive%\Product", "%ProgramData%"] {
        assert_eq!(
            resolve(&env(Ok(Some(value)), Ok(PD))),
            Err(MachineRootError::NeedsExpansion(value.into()))
        );
    }
    for value in ["relative\\Product", "Product"] {
        assert_eq!(
            resolve(&env(Ok(Some(value)), Ok(PD))),
            Err(MachineRootError::InvalidForm {
                root: PathBuf::from(value),
                problem: RootFormProblem::NotAbsolute
            })
        );
    }
    assert_eq!(
        resolve(&env(Ok(None), Err("E_FAIL"))),
        Err(MachineRootError::ProgramDataUnavailable("E_FAIL".into()))
    );
    assert_eq!(
        resolve(&env(Ok(None), Ok("relative"))),
        Err(MachineRootError::InvalidForm {
            root: PathBuf::from("relative").join(PRODUCT),
            problem: RootFormProblem::NotAbsolute
        })
    );
    let dotted = format!(
        "{CUSTOM}{}..{}x",
        std::path::MAIN_SEPARATOR,
        std::path::MAIN_SEPARATOR
    );
    assert_eq!(
        resolve(&env(Ok(Some(&dotted)), Ok(PD))),
        Err(MachineRootError::InvalidForm {
            root: PathBuf::from(&dotted),
            problem: RootFormProblem::DotSegment
        })
    );
    // ProgramData が空（あり得ないが）でも、形の確かめで止まる。
    assert!(matches!(
        resolve(&env(Ok(None), Ok(""))),
        Err(MachineRootError::InvalidForm { .. })
    ));
    let mut no_known = env(Ok(None), Ok(PD));
    no_known.known = Err("E_FAIL".to_string());
    assert_eq!(
        resolve(&no_known),
        Err(MachineRootError::KnownFoldersUnavailable("E_FAIL".into()))
    );
}

/// ドライブのルート・UNC・既知のシステムフォルダ（とその祖先・中）をルートにしない。
/// ProgramData・Users の**下**の専用のフォルダは使える。
#[cfg(windows)]
#[test]
fn well_known_roots_are_refused() {
    use WellKnownRootRefusal as R;
    let cases: [(&str, R); 14] = [
        (r"C:\", R::DriveRoot),
        (r"D:\", R::DriveRoot),
        (r"\\srv\share\cg", R::NetworkOrSpecial),
        (r"\\?\C:\x", R::NetworkOrSpecial),
        (
            r"C:\ProgramData",
            R::ContainsProtected(r"C:\ProgramData".into()),
        ),
        (
            r"c:\programdata\",
            R::ContainsProtected(r"C:\ProgramData".into()),
        ),
        // `C:\Users\Public`（中も断る側）が中にあるので、先にそちらで断る。
        (r"C:\Users", R::OverlapsProtected(r"C:\Users\Public".into())),
        (r"C:\Windows\x", R::OverlapsProtected(r"C:\Windows".into())),
        (r"C:\Windows", R::OverlapsProtected(r"C:\Windows".into())),
        (
            r"C:\Program Files",
            R::OverlapsProtected(r"C:\Program Files".into()),
        ),
        (
            r"C:\Program Files\Product",
            R::OverlapsProtected(r"C:\Program Files".into()),
        ),
        (
            r"C:\Program Files (x86)\cg",
            R::OverlapsProtected(r"C:\Program Files (x86)".into()),
        ),
        (
            r"C:\Program Files\Common Files\cg",
            R::OverlapsProtected(r"C:\Program Files".into()),
        ),
        (
            r"C:\Users\Public\cg",
            R::OverlapsProtected(r"C:\Users\Public".into()),
        ),
    ];
    for (bad, reason) in cases {
        assert_eq!(
            resolve(&env(Ok(Some(bad)), Ok(PD))),
            Err(MachineRootError::WellKnown {
                root: PathBuf::from(bad),
                refusal: reason
            }),
            "{bad}"
        );
    }
    // `\\.\C:\x` は `.` の要素を含むので、形の確かめで先に止まる（どちらにしても断る）。
    assert_eq!(
        resolve(&env(Ok(Some(r"\\.\C:\x")), Ok(PD))),
        Err(MachineRootError::InvalidForm {
            root: PathBuf::from(r"\\.\C:\x"),
            problem: RootFormProblem::DotSegment
        })
    );
    // 祖先（`C:\` は先にドライブのルートで止まるので、`C:\Windows` の祖先は中間の例で）。
    let mut deep = known();
    deep.forbidden_inside = vec![PathBuf::from(r"D:\data\Windows")];
    assert_eq!(
        reject_well_known_root(std::path::Path::new(r"D:\data"), &deep),
        Err(R::OverlapsProtected(PathBuf::from(r"D:\data\Windows")))
    );
    for good in [
        r"D:\Product",
        r"C:\ProgramData\Product",
        r"C:\Users\me\cg",
        r"C:\Program Files Evil\cg",
        r"C:\WindowsData\cg",
    ] {
        assert_eq!(
            resolve(&env(Ok(Some(good)), Ok(PD))),
            Ok(PathBuf::from(good)),
            "{good}"
        );
    }
}

#[cfg(not(windows))]
#[test]
fn well_known_roots_are_refused() {
    use WellKnownRootRefusal as R;
    let cases: [(&str, R); 8] = [
        ("/", R::DriveRoot),
        ("//srv/share", R::NetworkOrSpecial),
        ("/var/lib", R::ContainsProtected("/var/lib".into())),
        ("/var", R::ContainsProtected("/var/lib".into())),
        ("/users", R::OverlapsProtected("/users/public".into())),
        ("/win/x", R::OverlapsProtected("/win".into())),
        ("/pf", R::OverlapsProtected("/pf".into())),
        ("/pf86/cg", R::OverlapsProtected("/pf86".into())),
    ];
    for (bad, reason) in cases {
        assert_eq!(
            resolve(&env(Ok(Some(bad)), Ok(PD))),
            Err(MachineRootError::WellKnown {
                root: PathBuf::from(bad),
                refusal: reason
            }),
            "{bad}"
        );
    }
    for good in [
        "/srv/Product",
        "/var/lib/Product",
        "/users/me/cg",
        "/pfx/cg",
    ] {
        assert_eq!(
            resolve(&env(Ok(Some(good)), Ok(PD))),
            Ok(PathBuf::from(good)),
            "{good}"
        );
    }
}

// --- 本当のパス ---------------------------------------------------------------------

#[cfg(windows)]
#[test]
fn real_paths_are_compared_per_component_ignoring_case() {
    use std::path::Path;
    assert!(paths_match(
        Path::new(r"D:\Data\Product"),
        Path::new(r"d:\data\product")
    ));
    assert!(paths_match(
        Path::new(r"D:\Data\.\Product"),
        Path::new(r"D:\Data\Product")
    ));
    for (real, expected) in [
        (r"D:\other\Product", r"D:\data\Product"),
        (r"D:\data\Product\x", r"D:\data\Product"),
        (r"D:\data", r"D:\data\Product"),
        // 8.3 の短い名前は本当のパス（長い名前）と一致しない。
        (r"C:\Users\runneradmin\x", r"C:\Users\RUNNER~1\x"),
    ] {
        assert!(!paths_match(Path::new(real), Path::new(expected)), "{real}");
        assert_eq!(
            check_real_path(Path::new(real), Path::new(expected), &known()),
            Err(RealPathRefusal::Mismatch),
            "{real}"
        );
    }
    assert_eq!(
        check_real_path(
            Path::new(r"D:\data\Product"),
            Path::new(r"D:\DATA\product"),
            &known()
        ),
        Ok(())
    );
    // 一致しても、本当のパスが既知のフォルダなら断る。
    assert_eq!(
        check_real_path(
            Path::new(r"C:\Windows\Temp\x"),
            Path::new(r"C:\Windows\Temp\x"),
            &known()
        ),
        Err(RealPathRefusal::WellKnown(
            WellKnownRootRefusal::OverlapsProtected(PathBuf::from(r"C:\Windows"))
        ))
    );
    // 要素の境界で比べる。
    assert!(is_same_or_inside(
        Path::new(r"C:\Program Files\x"),
        Path::new(r"c:\program files")
    ));
    assert!(!is_same_or_inside(
        Path::new(r"C:\Program Files Evil\x"),
        Path::new(r"C:\Program Files")
    ));
    assert!(!is_same_or_inside(Path::new(r"C:\x"), Path::new("")));
}

#[cfg(not(windows))]
#[test]
fn real_paths_are_compared_per_component_ignoring_case() {
    use std::path::Path;
    assert!(paths_match(
        Path::new("/srv/Data/Product"),
        Path::new("/srv/data/product")
    ));
    assert_eq!(
        check_real_path(
            Path::new("/srv/other/Product"),
            Path::new("/srv/data/Product"),
            &known()
        ),
        Err(RealPathRefusal::Mismatch)
    );
    assert_eq!(
        check_real_path(Path::new("/win/x"), Path::new("/win/x"), &known()),
        Err(RealPathRefusal::WellKnown(
            WellKnownRootRefusal::OverlapsProtected(PathBuf::from("/win"))
        ))
    );
    assert_eq!(
        check_real_path(Path::new("/srv/p"), Path::new("/srv/P"), &known()),
        Ok(())
    );
    assert!(!is_same_or_inside(Path::new("/pfx/a"), Path::new("/pf")));
}

// --- データの ACL の計画 ----------------------------------------------------------

fn info(is_dir: bool, is_reparse_point: bool, owner: &str) -> DirState {
    DirState::Present(PinnedDirInfo {
        is_dir,
        is_reparse_point,
        owner: owner.to_string(),
    })
}

#[test]
fn data_acl_plan_creates_absent_and_resets_trusted_dirs() {
    let me = sid(USER);
    assert_eq!(
        data_acl_plan(&DirState::Absent, &me, None),
        Ok(DataAclStep::Create)
    );
    for owner in ["S-1-5-18", "S-1-5-32-544", USER] {
        assert_eq!(
            data_acl_plan(&info(true, false, owner), &me, None),
            Ok(DataAclStep::Reset),
            "{owner}"
        );
    }
    // この昇格した利用者（管理者）が所有者でも書き直す。
    assert_eq!(
        data_acl_plan(&info(true, false, ADMIN), &me, Some(ADMIN)),
        Ok(DataAclStep::Reset)
    );
}

/// 反証: reparse point・フォルダでないもの・許す範囲の外の所有者は断る（reparse
/// point は所有者が許す範囲でも断る）。
#[test]
fn data_acl_plan_refuses_links_files_and_foreign_owners() {
    let me = sid(USER);
    for owner in ["S-1-5-18", USER, STRANGER] {
        assert_eq!(
            data_acl_plan(&info(true, true, owner), &me, None),
            Err(DataAclRefusal::ReparsePoint)
        );
    }
    assert_eq!(
        data_acl_plan(&info(false, false, "S-1-5-32-544"), &me, None),
        Err(DataAclRefusal::NotADirectory)
    );
    for owner in [
        STRANGER,
        OTHER,
        "S-1-5-32-545",
        "S-1-1-0",
        "S-1-5-11",
        "S-1-3-0",
        "",
    ] {
        assert_eq!(
            data_acl_plan(&info(true, false, owner), &me, Some(ADMIN)),
            Err(DataAclRefusal::ForeignOwner(owner.to_string())),
            "{owner:?}"
        );
    }
    // 昇格した利用者を渡さなければ、その利用者の所有は許さない。
    assert_eq!(
        data_acl_plan(&info(true, false, ADMIN), &me, None),
        Err(DataAclRefusal::ForeignOwner(ADMIN.to_string()))
    );
}

/// #630: 対象の利用者を持たない置き場（`sink\`）。所有者として許すのは SYSTEM・
/// Administrators・この昇格した利用者だけで、利用者の領域の SID は誰であっても断る。
/// `Some` を渡せば [`data_acl_plan`] と同じ。
#[test]
fn dir_acl_plan_without_a_user_trusts_only_system_admins_and_the_elevated_user() {
    let cases: [(DirState, Option<&str>, Result<DataAclStep, DataAclRefusal>); 10] = [
        (DirState::Absent, None, Ok(DataAclStep::Create)),
        (info(true, false, "S-1-5-18"), None, Ok(DataAclStep::Reset)),
        (
            info(true, false, "S-1-5-32-544"),
            None,
            Ok(DataAclStep::Reset),
        ),
        (
            info(true, false, ADMIN),
            Some(ADMIN),
            Ok(DataAclStep::Reset),
        ),
        (
            info(true, false, ADMIN),
            None,
            Err(DataAclRefusal::ForeignOwner(ADMIN.to_string())),
        ),
        (
            info(true, false, USER),
            Some(ADMIN),
            Err(DataAclRefusal::ForeignOwner(USER.to_string())),
        ),
        (
            info(true, false, "S-1-5-32-545"),
            Some(ADMIN),
            Err(DataAclRefusal::ForeignOwner("S-1-5-32-545".to_string())),
        ),
        (
            info(true, false, ""),
            None,
            Err(DataAclRefusal::ForeignOwner(String::new())),
        ),
        (
            info(true, true, "S-1-5-32-544"),
            Some(ADMIN),
            Err(DataAclRefusal::ReparsePoint),
        ),
        (
            info(false, false, "S-1-5-18"),
            None,
            Err(DataAclRefusal::NotADirectory),
        ),
    ];
    for (state, elevated, expected) in cases {
        assert_eq!(
            dir_acl_plan(&state, None, elevated),
            expected,
            "{state:?} {elevated:?}"
        );
    }
    // `Some` は `data_acl_plan` と同じ判断。
    let me = sid(USER);
    for owner in ["S-1-5-18", USER, STRANGER, ADMIN] {
        let state = info(true, false, owner);
        assert_eq!(
            dir_acl_plan(&state, Some(&me), Some(ADMIN)),
            data_acl_plan(&state, &me, Some(ADMIN)),
            "{owner}"
        );
    }
}

// --- 引き継ぐ利用者 -----------------------------------------------------------------

const OI_CI: u8 = 0x03;
const INHERITED: u8 = 0x10;
const INHERIT_ONLY: u8 = 0x08;
const DENY: u8 = 1;
/// 読み取りと実行（`0x1200A9`）。
const READ_EXECUTE: u32 = 0x0012_00A9;
/// `GENERIC_ALL`。
const GENERIC_ALL: u32 = 0x1000_0000;

fn ace(ace_type: u8, flags: u8, mask: u32, sid: &str) -> ObservedAce {
    ObservedAce {
        ace_type,
        flags,
        mask,
        sid: sid.to_string(),
    }
}

fn allow(flags: u8, mask: u32, sid: &str) -> ObservedAce {
    ace(ACCESS_ALLOWED_ACE_TYPE, flags, mask, sid)
}

fn dacl(aces: Vec<ObservedAce>) -> ObservedDacl {
    ObservedDacl {
        protected: false,
        owner: ADMINISTRATORS_SID.to_string(),
        aces: Some(aces),
    }
}

/// 偽の種類の問い合わせ: USER・OTHER・STRANGER は利用者、GROUP はグループ、ADMIN は
/// 確かめられない。
fn kinds(sid: &InstallUserSid) -> AccountKind {
    match sid.as_str() {
        USER | OTHER | STRANGER => AccountKind::User,
        GROUP => AccountKind::NotAUser,
        _ => AccountKind::LookupFailed("RPC_S_SERVER_UNAVAILABLE".into()),
    }
}

/// 表: 1 つの ACE ごとに、引き継ぐ・落とす（理由）・どちらでもない（SY・BA）を決める。
#[test]
fn carry_over_table() {
    enum Expect {
        Carry,
        Drop(DropReason),
        Silent,
        Block,
    }
    use DropReason as D;
    use Expect::*;
    let table: Vec<(ObservedAce, Expect)> = vec![
        // 今の banto-hub の profile_acl が足す owner の ACE（OICI・変更）。
        (allow(OI_CI, MODIFY_ACCESS_MASK, USER), Carry),
        // フルコントロールは「変更」を含むので、「変更」に狭めて引き継ぐ。
        (allow(OI_CI, FULL_ACCESS_MASK, OTHER), Carry),
        // SYSTEM・Administrators は書き直した DACL がフルで持つ（明示・継承・どのマスクでも）。
        (allow(OI_CI, FULL_ACCESS_MASK, SYSTEM_SID), Silent),
        (
            allow(OI_CI | INHERITED, FULL_ACCESS_MASK, ADMINISTRATORS_SID),
            Silent,
        ),
        (allow(0, READ_EXECUTE, ADMINISTRATORS_SID), Silent),
        // 拒否は落とす（SY・BA でも）。
        (
            ace(DENY, OI_CI, MODIFY_ACCESS_MASK, STRANGER),
            Drop(D::NotAllow),
        ),
        (
            ace(DENY, OI_CI, FULL_ACCESS_MASK, SYSTEM_SID),
            Drop(D::NotAllow),
        ),
        // 条件付き・オブジェクトの許可（種類 5・9・11）は引き継がない（#626 PR-B の
        // 再レビュー 2。条件を外すと広がる）。条件付き・オブジェクトの拒否（6・10・12）は拒否。
        (
            ace(5, OI_CI, MODIFY_ACCESS_MASK, STRANGER),
            Drop(D::ConditionalAllow),
        ),
        (
            ace(9, OI_CI, MODIFY_ACCESS_MASK, STRANGER),
            Drop(D::ConditionalAllow),
        ),
        (
            ace(11, OI_CI, MODIFY_ACCESS_MASK, STRANGER),
            Drop(D::ConditionalAllow),
        ),
        (ace(6, OI_CI, 0x0004_0000, STRANGER), Drop(D::NotAllow)),
        (ace(10, OI_CI, 0x0004_0000, STRANGER), Drop(D::NotAllow)),
        (ace(12, OI_CI, 0x0004_0000, STRANGER), Drop(D::NotAllow)),
        // 種類の分からない ACE・SID を読めない ACE は決めない（呼ぶ側が断る）。
        (ace(0x13, OI_CI, MODIFY_ACCESS_MASK, STRANGER), Block),
        (ace(DENY, OI_CI, MODIFY_ACCESS_MASK, ""), Block),
        (ace(10, OI_CI, MODIFY_ACCESS_MASK, ""), Block),
        // ProgramData から継承した Users・CREATOR OWNER・利用者。
        (
            allow(OI_CI | INHERITED, READ_EXECUTE, "S-1-5-32-545"),
            Drop(D::Inherited),
        ),
        (
            allow(OI_CI | INHERIT_ONLY | INHERITED, GENERIC_ALL, "S-1-3-0"),
            Drop(D::Inherited),
        ),
        (
            allow(OI_CI | INHERITED, MODIFY_ACCESS_MASK, STRANGER),
            Drop(D::Inherited),
        ),
        // 中全体に効かない形。
        (allow(0, MODIFY_ACCESS_MASK, STRANGER), Drop(D::NotTreeWide)),
        (
            allow(0x01, MODIFY_ACCESS_MASK, STRANGER),
            Drop(D::NotTreeWide),
        ),
        (
            allow(0x02, MODIFY_ACCESS_MASK, STRANGER),
            Drop(D::NotTreeWide),
        ),
        (
            allow(OI_CI | INHERIT_ONLY, MODIFY_ACCESS_MASK, STRANGER),
            Drop(D::NotTreeWide),
        ),
        // no-propagate（すぐ下の子までしか伝わらない）。OICI で書き直すと広がる。
        (
            allow(OI_CI | 0x04, MODIFY_ACCESS_MASK, STRANGER),
            Drop(D::NotTreeWide),
        ),
        // 変更に足りない（読み取りだけ・汎用の権利だけ）。
        (
            allow(OI_CI, READ_EXECUTE, STRANGER),
            Drop(D::LessThanModify),
        ),
        (allow(OI_CI, GENERIC_ALL, STRANGER), Drop(D::LessThanModify)),
        (
            allow(OI_CI, MODIFY_ACCESS_MASK & !0x0001_0000, STRANGER),
            Drop(D::LessThanModify),
        ),
        // 利用者の領域でない SID（Everyone・Authenticated Users・Users・サービスの SID）。
        (
            allow(OI_CI, MODIFY_ACCESS_MASK, "S-1-1-0"),
            Drop(D::NotAUserSid),
        ),
        (
            allow(OI_CI, MODIFY_ACCESS_MASK, "S-1-5-11"),
            Drop(D::NotAUserSid),
        ),
        (
            allow(OI_CI, MODIFY_ACCESS_MASK, "S-1-5-32-545"),
            Drop(D::NotAUserSid),
        ),
        (
            allow(OI_CI, MODIFY_ACCESS_MASK, "S-1-5-80-1-2-3-4-5"),
            Drop(D::NotAUserSid),
        ),
        (allow(OI_CI, MODIFY_ACCESS_MASK, ""), Block),
        // 利用者の領域のグループ（Domain Users など）・確かめられない。
        (
            allow(OI_CI, MODIFY_ACCESS_MASK, GROUP),
            Drop(D::NotAnAccount),
        ),
        (
            allow(OI_CI, MODIFY_ACCESS_MASK, ADMIN),
            Drop(D::LookupFailed("RPC_S_SERVER_UNAVAILABLE".into())),
        ),
    ];
    for (row, expect) in table {
        let plan = plan_carry_over(&dacl(vec![row.clone()]), kinds);
        assert!(!plan.null_dacl);
        match expect {
            Carry => {
                assert_eq!(plan.carried, vec![sid(&row.sid)], "{row:?}");
                assert!(plan.dropped.is_empty(), "{row:?}: {plan:?}");
            }
            Drop(reason) => {
                assert!(plan.carried.is_empty(), "{row:?}: {plan:?}");
                assert_eq!(
                    plan.dropped,
                    vec![DroppedAce {
                        ace: row.clone(),
                        reason
                    }],
                    "{row:?}"
                );
            }
            Silent => assert_eq!(plan, CarryOver::default(), "{row:?}"),
            Block => {
                assert_eq!(plan.blocked, vec![row.clone()], "{row:?}");
                assert!(
                    plan.carried.is_empty() && plan.dropped.is_empty(),
                    "{row:?}"
                );
            }
        }
    }
}

/// 全体: 重複は 1 つにまとめ、DACL に現れた順に引き継ぐ。拒否の ACE を持つ利用者は
/// 引き継がない。引き継いだ利用者のほかの ACE（読み取りだけ）は落とした側に残る。種類の問い合わせは引き継いだ SID には
/// 2 回目をしない。
#[test]
fn carry_over_keeps_order_dedups_and_reports_the_rest() {
    let aces = vec![
        allow(OI_CI | INHERITED, FULL_ACCESS_MASK, SYSTEM_SID),
        allow(OI_CI, MODIFY_ACCESS_MASK, OTHER),
        allow(OI_CI, MODIFY_ACCESS_MASK, USER),
        allow(OI_CI, FULL_ACCESS_MASK, OTHER),
        allow(OI_CI, READ_EXECUTE, USER),
        ace(DENY, OI_CI, 0x0001_0000, USER),
        allow(OI_CI | INHERITED, READ_EXECUTE, "S-1-5-32-545"),
        allow(OI_CI, MODIFY_ACCESS_MASK, GROUP),
    ];
    let mut asked = Vec::new();
    let plan = plan_carry_over(&dacl(aces.clone()), |s| {
        asked.push(s.as_str().to_string());
        kinds(s)
    });
    // USER は拒否の ACE を持つので引き継がない（#626 PR-B の監査 B-2）。
    assert_eq!(plan.carried, vec![sid(OTHER)]);
    assert_eq!(asked, vec![OTHER, GROUP]);
    let reasons: Vec<(String, DropReason)> = plan
        .dropped
        .iter()
        .map(|d| (d.ace.sid.clone(), d.reason.clone()))
        .collect();
    assert_eq!(
        reasons,
        vec![
            (USER.to_string(), DropReason::HasDeny),
            (USER.to_string(), DropReason::LessThanModify),
            (USER.to_string(), DropReason::NotAllow),
            ("S-1-5-32-545".to_string(), DropReason::Inherited),
            (GROUP.to_string(), DropReason::NotAnAccount),
        ]
    );
    assert!(!plan.null_dacl);
}

/// null DACL（全員にフル）は、引き継ぐものも落とす ACE も無く、印だけ立てる。空の DACL
/// （だれにも許可が無い）は何も無い。
#[test]
fn carry_over_of_null_and_empty_dacls() {
    let null = ObservedDacl {
        protected: false,
        owner: USER.to_string(),
        aces: None,
    };
    let plan = plan_carry_over(&null, |_| unreachable!("問い合わせない"));
    assert_eq!(
        plan,
        CarryOver {
            carried: Vec::new(),
            dropped: Vec::new(),
            null_dacl: true,
            blocked: Vec::new(),
        }
    );
    let plan = plan_carry_over(&dacl(Vec::new()), |_| unreachable!("問い合わせない"));
    assert_eq!(plan, CarryOver::default());
}

/// 引き継いだ利用者を `DirAccess::with_install_users` に渡すと、保護された DACL の
/// 期待の形（SY・BA のフルと、利用者ごとの変更）になる。
#[test]
fn carried_users_build_the_multi_user_access() {
    let plan = plan_carry_over(
        &dacl(vec![
            allow(OI_CI, MODIFY_ACCESS_MASK, USER),
            allow(OI_CI, MODIFY_ACCESS_MASK, OTHER),
        ]),
        kinds,
    );
    let access = crate::DirAccess::with_install_users(plan.carried);
    assert_eq!(
        access.sddl(),
        format!(
            "D:P(A;OICI;FA;;;S-1-5-18)(A;OICI;FA;;;S-1-5-32-544)(A;OICI;0x1301bf;;;{USER})(A;OICI;0x1301bf;;;{OTHER})"
        )
    );
}

/// #626 PR-B の監査 B-2: 拒否の ACE（種類・継承・並びの前後を問わない）を持つ利用者の
/// 許可は引き継がない（拒否が消えて権利が広がるため）。拒否の無い利用者は引き継ぐ。
#[test]
fn a_user_with_any_deny_is_not_carried() {
    let modify = allow(OI_CI, MODIFY_ACCESS_MASK, USER);
    for deny in [
        ace(DENY, OI_CI, 0x0001_0000, USER),
        ace(DENY, 0, 0x0000_0002, USER),
        ace(DENY, OI_CI | INHERITED, FULL_ACCESS_MASK, USER),
    ] {
        for aces in [
            vec![modify.clone(), deny.clone()],
            vec![deny.clone(), modify.clone()],
        ] {
            let plan = plan_carry_over(&dacl(aces.clone()), kinds);
            assert!(plan.carried.is_empty(), "{aces:?}: {plan:?}");
            assert!(
                plan.dropped.contains(&DroppedAce {
                    ace: modify.clone(),
                    reason: DropReason::HasDeny
                }),
                "{aces:?}: {plan:?}"
            );
            assert!(
                plan.dropped.contains(&DroppedAce {
                    ace: deny.clone(),
                    reason: DropReason::NotAllow
                }),
                "{aces:?}: {plan:?}"
            );
        }
    }
    // 条件付きの拒否（callback・object）も拒否として数える（#626 PR-B の再レビュー 2）。
    for deny_type in [6u8, 10, 12] {
        let plan = plan_carry_over(
            &dacl(vec![
                modify.clone(),
                ace(deny_type, OI_CI, 0x0004_0000, USER),
            ]),
            kinds,
        );
        assert!(plan.carried.is_empty(), "{deny_type}: {plan:?}");
    }
    // 条件付きの許可は拒否として数えない（同じ利用者の普通の「変更」は引き継ぐ）。
    let plan = plan_carry_over(
        &dacl(vec![
            modify.clone(),
            ace(9, OI_CI, MODIFY_ACCESS_MASK, USER),
        ]),
        kinds,
    );
    assert_eq!(plan.carried, vec![sid(USER)]);
    // 別の利用者の拒否は関係しない。
    let plan = plan_carry_over(
        &dacl(vec![modify.clone(), ace(DENY, OI_CI, 0x0001_0000, OTHER)]),
        kinds,
    );
    assert_eq!(plan.carried, vec![sid(USER)]);
}

// --- 無いルートを作る前の、親の確かめ（#642） ---------------------------------------

fn plain_dir(is_dir: bool, is_reparse_point: bool) -> PinnedDirInfo {
    PinnedDirInfo {
        is_dir,
        is_reparse_point,
        owner: ADMINISTRATORS_SID.to_string(),
    }
}

/// 親として認めるもの（ProgramData そのもの・ドライブのルート・普通のフォルダ）と、断るもの
/// （本当のパスの食い違い・reparse point・ファイル・ネットワーク・OS の置き場の中）。
#[test]
fn root_parent_table() {
    use RootParentRefusal as R;
    let ok = plain_dir(true, false);
    let pd = PathBuf::from(PD);
    let custom_parent = PathBuf::from(CUSTOM).parent().unwrap().to_path_buf();
    #[cfg(windows)]
    let (windows_child, unc) = (
        PathBuf::from(r"C:\Windows\x"),
        PathBuf::from(r"\\server\share"),
    );
    #[cfg(not(windows))]
    let (windows_child, unc) = (PathBuf::from("/win/x"), PathBuf::from("//server/share"));
    let cases: Vec<(PathBuf, PathBuf, PinnedDirInfo, Result<(), R>)> = vec![
        (pd.clone(), pd.clone(), ok.clone(), Ok(())),
        (
            custom_parent.clone(),
            custom_parent.clone(),
            ok.clone(),
            Ok(()),
        ),
        (pd.join("x"), pd.clone(), ok.clone(), Err(R::Mismatch)),
        (
            pd.clone(),
            pd.clone(),
            plain_dir(true, true),
            Err(R::NotAPlainDirectory),
        ),
        (
            pd.clone(),
            pd.clone(),
            plain_dir(false, false),
            Err(R::NotAPlainDirectory),
        ),
        (
            unc.clone(),
            unc.clone(),
            ok.clone(),
            Err(R::NetworkOrSpecial),
        ),
        (
            windows_child.clone(),
            windows_child.clone(),
            ok.clone(),
            Err(R::InsideProtected(known().forbidden_inside[0].clone())),
        ),
    ];
    for (real, expected, info, want) in cases {
        assert_eq!(
            check_root_parent(&real, &expected, &info, &known()),
            want,
            "{real:?} {expected:?} {info:?}"
        );
    }
}

// --- 名前 1 要素（#641 のオーナーレビュー Nit 2・Nit 4） -----------------------------

#[test]
fn reserved_device_names_include_com0_and_lpt0() {
    for name in [
        "nul",
        "NUL",
        "Con",
        "prn",
        "aux",
        "COM0",
        "com1",
        "COM9",
        "LPT0",
        "lpt0",
        "lpt9",
        "nul.txt",
        "CON .log",
        "com\u{b9}",
        "LPT\u{b3}",
        "CONIN$",
        "conout$",
    ] {
        assert!(is_reserved_device_name(name), "{name:?}");
    }
    for name in [
        "console",
        "nul1",
        "com10",
        "lpt00",
        "auxiliary",
        "COM",
        "secrets",
    ] {
        assert!(!is_reserved_device_name(name), "{name:?}");
    }
}

#[test]
fn plain_child_names_refuse_what_win32_would_read_differently() {
    for name in [
        "secrets",
        "hub-keys",
        "ChronoGazer",
        "BantoHub",
        "a.b",
        "x y",
        "default",
    ] {
        assert!(is_plain_child_name(name), "{name:?}");
    }
    for name in [
        "",
        ".",
        "..",
        r"a\b",
        "a/b",
        "c:",
        "x:y",
        "a\0b",
        "a\tb",
        "a*",
        "a?",
        "a<b",
        "a>b",
        "a|b",
        "a\"b",
        "secrets.",
        "secrets ",
        "secrets. ",
        "nul",
        "COM0",
        "lpt0",
        "con.txt",
    ] {
        assert!(!is_plain_child_name(name), "{name:?}");
    }
}
