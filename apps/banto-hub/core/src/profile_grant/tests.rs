//! 偽物の [`HubElevOps`] で、確かめの順序・副作用が無いこと・出力を確かめる（全 OS）。
//! 本物の ACL の操作での反証は`win/tests.rs`。

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use banto_secret_store::acl::guard::{
    AccountKind, KnownFolders, MachineEnvironment, MachineRootError,
};
use banto_secret_store::acl::{
    ObservedAce, ObservedDacl, PinnedDirInfo, ResetSkips, ADMINISTRATORS_SID, FULL_ACCESS_MASK,
    MODIFY_ACCESS_MASK, SYSTEM_SID,
};
use banto_secret_store::{DirAccess, InstallUserSid};
use banto_service_control::ConsoleLine;

use super::*;

// --- 偽物 --------------------------------------------------------------------------

const ME: &str = "S-1-5-21-1-2-3-1001";
const ALICE: &str = "S-1-5-21-1-2-3-1002";
const BOB: &str = "S-1-5-21-1-2-3-1003";
const CAROL: &str = "S-1-5-21-1-2-3-1004";
const DAVE: &str = "S-1-5-21-1-2-3-1005";
const DOMAIN_USERS: &str = "S-1-5-21-1-2-3-513";
const EVIL: &str = "S-1-5-21-9-9-9-1666";

fn sid(s: &str) -> InstallUserSid {
    InstallUserSid::parse(s).unwrap()
}

/// 全 OS で絶対パスになる基点（Windows は`C:\`、それ以外は`/`）。
fn abs(rest: &str) -> PathBuf {
    let base = if cfg!(windows) { r"C:\" } else { "/" };
    let mut path = PathBuf::from(base);
    for part in rest.split('/') {
        path.push(part);
    }
    path
}

fn program_data() -> PathBuf {
    abs("ProgramData")
}

fn known() -> KnownFolders {
    KnownFolders {
        forbidden_inside: vec![abs("Windows"), abs("Program Files")],
        forbidden_itself: vec![program_data(), abs("Users")],
    }
}

struct FakeEnv {
    variable: Result<Option<String>, String>,
    program_data: Result<PathBuf, String>,
    known: Result<KnownFolders, String>,
}

impl MachineEnvironment for FakeEnv {
    fn machine_variable(&self, name: &str) -> Result<Option<String>, String> {
        assert_eq!(name, ROOT_ENV);
        self.variable.clone()
    }
    fn program_data(&self) -> Result<PathBuf, String> {
        self.program_data.clone()
    }
    fn known_folders(&self) -> Result<KnownFolders, String> {
        self.known.clone()
    }
}

/// 偽のフォルダ 1 つ。
#[derive(Clone)]
struct FakeDir {
    info: PinnedDirInfo,
    /// 開いたときの本当のパス（`None`ならパスそのもの）。
    final_path: Option<PathBuf>,
    dacl: Result<ObservedDacl, String>,
    reset: Result<ResetSkips, String>,
}

impl FakeDir {
    fn owned_by(owner: &str) -> Self {
        Self {
            info: PinnedDirInfo {
                is_dir: true,
                is_reparse_point: false,
                owner: owner.to_string(),
            },
            final_path: None,
            dacl: Ok(ObservedDacl {
                protected: false,
                owner: owner.to_string(),
                aces: Some(Vec::new()),
            }),
            reset: Ok(ResetSkips::default()),
        }
    }
}

type Log = Rc<RefCell<Vec<String>>>;

struct FakePinned {
    path: PathBuf,
    dir: FakeDir,
    log: Log,
}

impl PinnedProfileDir for FakePinned {
    fn info(&self) -> PinnedDirInfo {
        self.dir.info.clone()
    }
    fn final_path(&self) -> PathBuf {
        self.dir
            .final_path
            .clone()
            .unwrap_or_else(|| self.path.clone())
    }
    fn read_dacl(&self) -> Result<ObservedDacl, String> {
        self.log
            .borrow_mut()
            .push(format!("read_dacl {}", name(&self.path)));
        self.dir.dacl.clone()
    }
    fn reset_acl_tree(&self, access: &DirAccess) -> Result<ResetSkips, String> {
        let users: Vec<&str> = access
            .install_users()
            .iter()
            .map(InstallUserSid::as_str)
            .collect();
        self.log
            .borrow_mut()
            .push(format!("reset {} [{}]", name(&self.path), users.join(",")));
        self.dir.reset.clone()
    }
}

impl Drop for FakePinned {
    fn drop(&mut self) {
        self.log
            .borrow_mut()
            .push(format!("drop {}", name(&self.path)));
    }
}

/// 行の記録に使う短い名前（最後の要素）。
fn name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

struct FakeOps {
    env: FakeEnv,
    me: Result<InstallUserSid, String>,
    names: HashMap<String, Result<Option<String>, String>>,
    kinds: HashMap<String, AccountKind>,
    in_use: Result<bool, String>,
    dirs: RefCell<HashMap<PathBuf, FakeDir>>,
    /// `create_dir`が作るフォルダ（既定は Administrators 所有。すり替えの再現に使う）。
    on_create: RefCell<HashMap<PathBuf, FakeDir>>,
    log: Log,
}

impl FakeOps {
    /// 既定: マシンの変数なし（ProgramData\BantoHub）、自分は利用者、使っていない、
    /// ルート・profiles・default が自分の所有である（利用者の名前は alice・bob など）。
    fn new() -> Self {
        let mut names = HashMap::new();
        for (n, s) in [
            ("me", ME),
            ("alice", ALICE),
            ("Everyone", "S-1-1-0"),
            ("Users", "S-1-5-32-545"),
            ("SYSTEM", "S-1-5-18"),
            ("Domain Users", DOMAIN_USERS),
            ("dave", DAVE),
        ] {
            names.insert(n.to_string(), Ok(Some(s.to_string())));
        }
        names.insert("ghost".to_string(), Ok(None));
        names.insert("broken".to_string(), Err("RPC が届かない".to_string()));
        let mut kinds = HashMap::new();
        for user in [ME, ALICE, BOB, CAROL] {
            kinds.insert(user.to_string(), AccountKind::User);
        }
        kinds.insert(DOMAIN_USERS.to_string(), AccountKind::NotAUser);
        kinds.insert(
            DAVE.to_string(),
            AccountKind::LookupFailed("RPC が届かない".to_string()),
        );
        let ops = Self {
            env: FakeEnv {
                variable: Ok(None),
                program_data: Ok(program_data()),
                known: Ok(known()),
            },
            me: Ok(sid(ME)),
            names,
            kinds,
            in_use: Ok(false),
            dirs: RefCell::new(HashMap::new()),
            on_create: RefCell::new(HashMap::new()),
            log: Rc::new(RefCell::new(Vec::new())),
        };
        for dir in [root(), profiles(), profile()] {
            ops.dirs.borrow_mut().insert(dir, FakeDir::owned_by(ME));
        }
        ops
    }

    fn calls(&self) -> Vec<String> {
        self.log.borrow().clone()
    }

    /// ファイルシステムに触った呼び出し（開く・作る・読む・書く）。
    fn touched_fs(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter(|c| !c.starts_with("in_use"))
            .collect()
    }
}

fn root() -> PathBuf {
    program_data().join(PRODUCT_DIR)
}

fn profiles() -> PathBuf {
    root().join(PROFILES_DIR)
}

fn profile() -> PathBuf {
    profiles().join("default")
}

impl HubElevOps for FakeOps {
    type Env = FakeEnv;
    type Pinned = FakePinned;

    fn machine_env(&self) -> &FakeEnv {
        &self.env
    }
    fn current_user_sid(&self) -> Result<InstallUserSid, String> {
        self.me.clone()
    }
    fn lookup_account_sid(&self, name: &str) -> Result<Option<String>, String> {
        self.names.get(name).cloned().unwrap_or(Ok(None))
    }
    fn account_kind(&self, sid: &InstallUserSid) -> AccountKind {
        self.kinds
            .get(sid.as_str())
            .cloned()
            .unwrap_or(AccountKind::NotAUser)
    }
    fn profile_in_use(&self, profile_id: &str) -> Result<bool, String> {
        self.log.borrow_mut().push(format!("in_use {profile_id}"));
        self.in_use.clone()
    }
    fn pin_dir(&self, dir: &Path) -> Result<Option<FakePinned>, String> {
        self.log.borrow_mut().push(format!("pin {}", name(dir)));
        Ok(self.dirs.borrow().get(dir).cloned().map(|d| FakePinned {
            path: dir.to_path_buf(),
            dir: d,
            log: self.log.clone(),
        }))
    }
    fn create_dir(&self, dir: &Path, how: CreateHow) -> Result<bool, String> {
        self.log
            .borrow_mut()
            .push(format!("create {} {how:?}", name(dir)));
        let made = self
            .on_create
            .borrow_mut()
            .remove(dir)
            .unwrap_or_else(|| FakeDir::owned_by(ADMINISTRATORS_SID));
        self.dirs.borrow_mut().insert(dir.to_path_buf(), made);
        Ok(true)
    }
}

fn run(ops: &FakeOps, user: Option<&str>, id: Option<&str>) -> (Result<(), Refused>, Vec<String>) {
    let mut lines = Vec::new();
    let result = {
        let mut out = |line: ConsoleLine| lines.push(line);
        preflight(ops, user, id, &mut out).and_then(|pre| grant_profile_acl(ops, &pre, &mut out))
    };
    (result, texts(lines))
}

fn texts(lines: Vec<ConsoleLine>) -> Vec<String> {
    lines
        .into_iter()
        .map(|line| match line {
            ConsoleLine::Stdout(text) => format!("out {text}"),
            ConsoleLine::Stderr(text) => format!("err {text}"),
        })
        .collect()
}

fn ace(ace_type: u8, flags: u8, mask: u32, sid: &str) -> ObservedAce {
    ObservedAce {
        ace_type,
        flags,
        mask,
        sid: sid.to_string(),
    }
}

// --- profile-id の許可リスト -------------------------------------------------------

#[test]
fn strict_profile_id_table() {
    use StrictProfileIdError as E;
    let long = "a".repeat(MAX_PROFILE_ID_LEN + 1);
    let ok_max = "a".repeat(MAX_PROFILE_ID_LEN);
    let cases: Vec<(&str, Result<(), E>)> = vec![
        ("default", Ok(())),
        ("line-1_2", Ok(())),
        ("p6-12345-1700000000000", Ok(())),
        ("0abc", Ok(())),
        (ok_max.as_str(), Ok(())),
        ("", Err(E::Empty)),
        ("C:", Err(E::InvalidChar(':'))),
        ("x:y", Err(E::InvalidChar(':'))),
        ("default.", Err(E::InvalidChar('.'))),
        ("default ", Err(E::InvalidChar(' '))),
        ("..", Err(E::InvalidChar('.'))),
        ("a/b", Err(E::InvalidChar('/'))),
        ("a\\b", Err(E::InvalidChar('\\'))),
        ("a*", Err(E::InvalidChar('*'))),
        ("déf", Err(E::InvalidChar('é'))),
        (long.as_str(), Err(E::TooLong(MAX_PROFILE_ID_LEN + 1))),
        ("-lead", Err(E::FirstNotAlphanumeric)),
        ("_lead", Err(E::FirstNotAlphanumeric)),
        ("nul", Err(E::Reserved)),
        ("NUL", Err(E::Reserved)),
        ("Con", Err(E::Reserved)),
        ("COM1", Err(E::Reserved)),
        ("lpt9", Err(E::Reserved)),
    ];
    for (id, expected) in cases {
        assert_eq!(validate_strict_profile_id(id), expected, "{id:?}");
    }
}

/// 許可リストを通るものは、全体の`validate_profile_id`も必ず通る（elev の id は
/// ホストでも同じ profile を指す）。
#[test]
fn strict_ids_also_pass_the_general_validation() {
    for id in ["default", "a", "line-1_2", "COM10", "nul1", "p6-1-2"] {
        assert_eq!(validate_strict_profile_id(id), Ok(()), "{id}");
        crate::profile_paths::validate_profile_id(id).unwrap();
    }
}

// --- 確かめ終わる前に副作用が無い ----------------------------------------------------

/// 受入条件「確かめ終わる前に副作用が無い」: 不正な profile-id・利用者・ルートでは、
/// 開く・作る・読む・書くが 1 回も呼ばれず（稼働中の確かめも呼ばれず）、断る。
#[test]
fn invalid_input_has_no_side_effects() {
    type Setup = Box<dyn Fn(&mut FakeOps)>;
    let nothing: fn() -> Setup = || Box::new(|_| {});
    let var = |value: &str| -> Setup {
        let value = value.to_string();
        Box::new(move |ops: &mut FakeOps| ops.env.variable = Ok(Some(value.clone())))
    };
    let long = "a".repeat(65);
    let mut cases: Vec<(&str, Option<&str>, Option<&str>, Setup)> = vec![
        // profile-id
        ("C:", None, Some("C:"), nothing()),
        ("x:y", None, Some("x:y"), nothing()),
        ("nul", None, Some("nul"), nothing()),
        ("default.", None, Some("default."), nothing()),
        ("..", None, Some(".."), nothing()),
        ("empty id", None, Some(""), nothing()),
        ("long id", None, Some(long.as_str()), nothing()),
        // 利用者
        ("Everyone", Some("Everyone"), None, nothing()),
        ("Users", Some("Users"), None, nothing()),
        ("SYSTEM", Some("SYSTEM"), None, nothing()),
        ("ghost", Some("ghost"), None, nothing()),
        ("lookup error", Some("broken"), None, nothing()),
        ("S-1-1-0", Some("S-1-1-0"), None, nothing()),
        ("S-1-5-32-544", Some("S-1-5-32-544"), None, nothing()),
        ("group sid", Some(DOMAIN_USERS), None, nothing()),
        ("group name", Some("Domain Users"), None, nothing()),
        ("lookup failed", Some("dave"), None, nothing()),
        ("empty user", Some(""), None, nothing()),
        (
            "lowercase sid",
            Some("s-1-5-21-1-2-3-1001"),
            None,
            nothing(),
        ),
        (
            "token user unavailable",
            None,
            None,
            Box::new(|ops: &mut FakeOps| ops.me = Err("SYSTEM のトークン".into())),
        ),
        (
            "token user not an account",
            None,
            None,
            Box::new(|ops: &mut FakeOps| {
                ops.kinds.insert(ME.into(), AccountKind::NotAUser);
            }),
        ),
        // ルート
        (
            "needs expansion",
            None,
            None,
            var(r"%SystemDrive%\BantoHub"),
        ),
        ("relative", None, None, var("BantoHub")),
        (
            "drive root",
            None,
            None,
            var(if cfg!(windows) { r"C:\" } else { "/" }),
        ),
        ("unc", None, None, var(r"\\server\share\BantoHub")),
        (
            "inside program files",
            None,
            None,
            var(&abs("Program Files/BantoHub").to_string_lossy()),
        ),
        (
            "program data itself",
            None,
            None,
            var(&program_data().to_string_lossy()),
        ),
        (
            "dot segment",
            None,
            None,
            var(&abs("x/../BantoHub").to_string_lossy()),
        ),
        (
            "variable unreadable",
            None,
            None,
            Box::new(|ops: &mut FakeOps| ops.env.variable = Err("拒否".into())),
        ),
        (
            "program data unavailable",
            None,
            None,
            Box::new(|ops: &mut FakeOps| ops.env.program_data = Err("無い".into())),
        ),
        (
            "known folders unavailable",
            None,
            None,
            Box::new(|ops: &mut FakeOps| ops.env.known = Err("無い".into())),
        ),
    ];
    // UNC は Windows でだけ UNC（他の OS では相対パスとして断られる - どちらも断る）。
    for (label, user, id, setup) in cases.drain(..) {
        let mut ops = FakeOps::new();
        setup(&mut ops);
        let (result, lines) = run(&ops, user, id);
        assert_eq!(result, Err(Refused), "{label}: {lines:#?}");
        assert_eq!(ops.calls(), Vec::<String>::new(), "{label}: 副作用がある");
        assert!(
            lines.iter().all(|l| l.starts_with("err banto-hub-elev: ")),
            "{label}: {lines:#?}"
        );
        assert!(
            lines.last().unwrap().ends_with("何も変えていません"),
            "{label}: {lines:#?}"
        );
    }
}

/// `service-install`: 確かめが通らなければ登録の手順を呼ばない。
#[test]
fn service_install_does_not_register_when_preflight_fails() {
    for setup in [
        (|ops: &mut FakeOps| ops.env.variable = Ok(Some("relative".into()))) as fn(&mut FakeOps),
        |ops: &mut FakeOps| {
            ops.kinds.insert(ME.into(), AccountKind::NotAUser);
        },
        |ops: &mut FakeOps| ops.env.known = Err("無い".into()),
    ] {
        let mut ops = FakeOps::new();
        setup(&mut ops);
        let registered = std::cell::Cell::new(false);
        let mut lines = Vec::new();
        let result = service_install(
            &ops,
            || -> Result<(), String> {
                registered.set(true);
                Ok(())
            },
            &mut |l| lines.push(l),
        );
        assert!(matches!(result, Err(ServiceInstallError::Refused)));
        assert!(!registered.get(), "確かめの前に登録した");
        assert_eq!(ops.calls(), Vec::<String>::new());
        assert!(texts(lines)
            .last()
            .unwrap()
            .contains("サービスを登録していません"));
    }
}

/// `service-install`: 確かめ → 登録 → 書き直し の順。登録が失敗したら書き直さない。
#[test]
fn service_install_registers_then_grants() {
    let ops = FakeOps::new();
    let log = ops.log.clone();
    let mut lines = Vec::new();
    let result = service_install(
        &ops,
        || -> Result<(), String> {
            log.borrow_mut().push("register".into());
            Ok(())
        },
        &mut |l| lines.push(l),
    );
    assert!(result.is_ok(), "{:#?}", texts(lines));
    assert_eq!(ops.calls()[0], "register");
    assert!(ops.calls().iter().any(|c| c.starts_with("reset default")));

    let ops = FakeOps::new();
    let result = service_install(&ops, || Err("登録できない"), &mut |_| {});
    assert!(matches!(
        result,
        Err(ServiceInstallError::Register("登録できない"))
    ));
    assert_eq!(ops.calls(), Vec::<String>::new());
}

// --- 稼働中 ------------------------------------------------------------------------

#[test]
fn a_profile_in_use_is_refused_before_touching_anything() {
    for in_use in [Ok(true), Err("OpenMutexW が失敗".to_string())] {
        let mut ops = FakeOps::new();
        ops.in_use = in_use;
        let (result, lines) = run(&ops, None, None);
        assert_eq!(result, Err(Refused), "{lines:#?}");
        assert_eq!(ops.calls(), vec!["in_use default".to_string()]);
        assert!(ops.touched_fs().is_empty());
        assert!(lines.last().unwrap().ends_with("何も変えていません"));
    }
}

// --- 順序 --------------------------------------------------------------------------

/// ルート → profiles → profile の順に開き、profile の DACL を読んでから書き、開いた
/// フォルダは書き終わるまで持つ（閉じるのは最後、逆の順）。ルート・profiles は書かない。
#[test]
fn normal_run_opens_in_order_and_holds_until_the_end() {
    let ops = FakeOps::new();
    let (result, lines) = run(&ops, None, None);
    assert_eq!(result, Ok(()), "{lines:#?}");
    assert_eq!(
        ops.calls(),
        vec![
            "in_use default",
            "pin BantoHub",
            "pin profiles",
            "pin default",
            "read_dacl default",
            &format!("reset default [{ME}]"),
            "drop default",
            "drop profiles",
            "drop BantoHub",
        ]
    );
    let profile_dir = profile();
    assert_eq!(
        lines,
        vec![format!(
            "out banto-hub-elev: profile 'default'（{}）: ACL を書き直しました（SYSTEM・Administrators にフル、利用者 {ME} に変更、所有者は Administrators、親から継承しない。中のフォルダ・ファイルも同じ形）",
            profile_dir.display()
        )]
    );
}

/// 無いものは作る: ルート・profiles は親から継承する普通のフォルダ、profile は
/// SYSTEM・Administrators だけで作り、確かめてから書き直す。
#[test]
fn missing_dirs_are_created_then_checked_then_reset() {
    let ops = FakeOps::new();
    ops.dirs.borrow_mut().clear();
    let (result, lines) = run(&ops, Some("alice"), Some("line-2"));
    assert_eq!(result, Ok(()), "{lines:#?}");
    assert_eq!(
        ops.calls(),
        vec![
            "in_use line-2",
            "pin BantoHub",
            "create BantoHub Inherit",
            "pin BantoHub",
            "pin profiles",
            "create profiles Inherit",
            "pin profiles",
            "pin line-2",
            "create line-2 ServiceOnly",
            "pin line-2",
            "read_dacl line-2",
            &format!("reset line-2 [{ALICE}]"),
            "drop line-2",
            "drop profiles",
            "drop BantoHub",
        ]
    );
    assert!(lines[0].contains(": 作りました（"), "{lines:#?}");
}

/// 作った後に確かめが通らない（作った直後にすり替えられた・見知らぬ所有者）なら、
/// 書き直さずに断り、自分で作ったものが残っていることを 1 行ずつ知らせる。
#[test]
fn created_then_refused_leaves_a_notice_and_does_not_reset() {
    let ops = FakeOps::new();
    ops.dirs.borrow_mut().remove(&profile());
    ops.on_create
        .borrow_mut()
        .insert(profile(), FakeDir::owned_by(EVIL));
    let (result, lines) = run(&ops, None, None);
    assert_eq!(result, Err(Refused), "{lines:#?}");
    assert!(!ops.calls().iter().any(|c| c.starts_with("reset")));
    assert!(!ops.calls().iter().any(|c| c.starts_with("read_dacl")));
    assert!(lines.iter().any(|l| l.contains(EVIL)), "{lines:#?}");
    assert_eq!(
        lines.last().unwrap(),
        &format!(
            "err banto-hub-elev: profile（{}）は作りましたが、確かめが通らなかったので残っています（SYSTEM・Administrators だけが触れる DACL）",
            profile().display()
        )
    );

    // ルートを作った後、profiles の本当のパスが食い違う → ルートと profiles の両方を知らせる。
    let ops = FakeOps::new();
    ops.dirs.borrow_mut().clear();
    let mut swapped = FakeDir::owned_by(ADMINISTRATORS_SID);
    swapped.final_path = Some(abs("elsewhere/profiles"));
    ops.on_create.borrow_mut().insert(profiles(), swapped);
    let (result, lines) = run(&ops, None, None);
    assert_eq!(result, Err(Refused), "{lines:#?}");
    let notices: Vec<_> = lines
        .iter()
        .filter(|l| l.contains("残っています"))
        .collect();
    assert_eq!(notices.len(), 2, "{lines:#?}");
    assert!(notices
        .iter()
        .all(|l| l.contains("親から継承した DACL のまま")));
    assert!(!ops.calls().iter().any(|c| c == "pin default"));
}

/// 各段の断る分岐（本当のパス・reparse point・フォルダでない・所有者・開けない・
/// 読めない・書けない）が届き、書き直さない。
#[test]
fn each_refusal_branch_is_reachable() {
    type Tweak = Box<dyn Fn(&FakeOps)>;
    let set = |path: fn() -> PathBuf, f: fn(&mut FakeDir)| -> Tweak {
        Box::new(move |ops: &FakeOps| {
            let mut dirs = ops.dirs.borrow_mut();
            f(dirs.get_mut(&path()).unwrap());
        })
    };
    let cases: Vec<(&str, Tweak, &str)> = vec![
        (
            "root mismatch",
            set(root, |d| d.final_path = Some(abs("elsewhere/BantoHub"))),
            "本当の場所",
        ),
        (
            "root reparse",
            set(root, |d| d.info.is_reparse_point = true),
            "reparse point",
        ),
        (
            "profiles foreign owner",
            set(profiles, |d| d.info.owner = EVIL.into()),
            EVIL,
        ),
        (
            "profiles mismatch",
            set(profiles, |d| d.final_path = Some(abs("x/profiles"))),
            "本当の場所",
        ),
        (
            "profile reparse",
            set(profile, |d| d.info.is_reparse_point = true),
            "reparse point",
        ),
        (
            "profile not a dir",
            set(profile, |d| d.info.is_dir = false),
            "フォルダではありません",
        ),
        (
            "profile foreign owner",
            set(profile, |d| d.info.owner = EVIL.into()),
            "grant-profile-acl <その利用者>",
        ),
        (
            "profile mismatch",
            set(profile, |d| {
                d.final_path = Some(abs("ProgramData/BantoHub/profiles/other"))
            }),
            "本当の場所",
        ),
        (
            "read dacl fails",
            set(profile, |d| d.dacl = Err("読めない".into())),
            "今の DACL を読めませんでした",
        ),
    ];
    for (label, tweak, needle) in cases {
        let ops = FakeOps::new();
        tweak(&ops);
        let (result, lines) = run(&ops, None, None);
        assert_eq!(result, Err(Refused), "{label}: {lines:#?}");
        assert!(
            !ops.calls().iter().any(|c| c.starts_with("reset")),
            "{label}"
        );
        assert!(
            lines.iter().any(|l| l.contains(needle)),
            "{label}: {lines:#?}"
        );
    }

    // 書けない: 書き直しを試みて失敗を出す（成功の行は出さない）。
    let ops = FakeOps::new();
    ops.dirs.borrow_mut().get_mut(&profile()).unwrap().reset = Err("1307".into());
    let (result, lines) = run(&ops, None, None);
    assert_eq!(result, Err(Refused));
    assert!(lines
        .iter()
        .any(|l| l.contains("書き直せませんでした: 1307")));
    assert!(!lines.iter().any(|l| l.contains("ACL を書き直しました")));
}

/// 所有者は SYSTEM・Administrators・指定の利用者・実行した管理者なら通る（引き継いだ
/// 利用者は所有者として許さない - モジュール doc「複数の利用者への拡張点」）。
#[test]
fn owner_allow_list() {
    for (owner, user, elevated, ok) in [
        (SYSTEM_SID, ME, ME, true),
        (ADMINISTRATORS_SID, ME, ME, true),
        (ALICE, "alice", ME, true),
        (ME, "alice", ME, true),
        (BOB, "alice", ME, false),
    ] {
        let mut ops = FakeOps::new();
        ops.me = Ok(sid(elevated));
        {
            let mut dirs = ops.dirs.borrow_mut();
            let dir = dirs.get_mut(&profile()).unwrap();
            dir.info.owner = owner.into();
            // BOB は今の DACL に「変更」を持つ（引き継がれる利用者）が、所有者としては許さない。
            dir.dacl = Ok(ObservedDacl {
                protected: false,
                owner: owner.into(),
                aces: Some(vec![ace(0, 0x03, MODIFY_ACCESS_MASK, BOB)]),
            });
        }
        let (result, lines) = run(&ops, Some(user), None);
        assert_eq!(result.is_ok(), ok, "{owner} {user}: {lines:#?}");
    }
}

// --- 引き継ぎと報告 -----------------------------------------------------------------

/// 足していく意味（論点 4 (b)）: 今の DACL の明示の「変更」以上の利用者を引き継ぎ、
/// 指定の利用者を足す。落とした ACE は 1 行ずつ、拒否・確かめられない・所有者の継承の
/// ACE・フルの狭めは警告として出し、最後にまとめの行。
#[test]
fn carry_over_lines_golden() {
    let ops = FakeOps::new();
    {
        let mut dirs = ops.dirs.borrow_mut();
        let dir = dirs.get_mut(&profile()).unwrap();
        dir.info.owner = CAROL.into();
        dir.dacl = Ok(ObservedDacl {
            protected: false,
            owner: CAROL.into(),
            aces: Some(vec![
                ace(0, 0x03, FULL_ACCESS_MASK, SYSTEM_SID),
                ace(0, 0x03, FULL_ACCESS_MASK, ALICE),
                ace(0, 0x03, MODIFY_ACCESS_MASK, BOB),
                ace(1, 0x03, 0x0001_0000, BOB),
                ace(0, 0x10, FULL_ACCESS_MASK, CAROL),
                ace(0, 0x13, 0x0012_00A9, "S-1-5-32-545"),
                ace(0, 0x07, MODIFY_ACCESS_MASK, ME),
                ace(0, 0x03, 0x0012_00A9, ME),
                ace(0, 0x03, MODIFY_ACCESS_MASK, DOMAIN_USERS),
                ace(0, 0x03, MODIFY_ACCESS_MASK, DAVE),
            ]),
        });
        dir.reset = Ok(ResetSkips {
            reparse: vec![profile().join("x")],
            hardlink: vec![profile().join("f.txt")],
            denied: vec![profile().join("locked")],
        });
    }
    // 所有者 CAROL を許すため、実行した管理者を CAROL にする。
    let mut ops = ops;
    ops.me = Ok(sid(CAROL));
    let (result, lines) = run(&ops, Some(ME), None);
    assert_eq!(result, Ok(()), "{lines:#?}");
    assert!(ops
        .calls()
        .contains(&format!("reset default [{ME},{ALICE},{BOB}]")));
    let p = profile();
    let expected = vec![
        format!("out banto-hub-elev: 今の DACL に「変更」を持つ利用者 {ALICE} を引き継ぎます"),
        format!("out banto-hub-elev: 今の DACL に「変更」を持つ利用者 {BOB} を引き継ぎます"),
        format!("err banto-hub-elev: 警告: 利用者 {ALICE} の今の権限は「変更」より広い（フルコントロールなど）ので、「変更」に狭めます（ACL の変更・所有権の取得はできなくなります）"),
        format!("err banto-hub-elev: 警告: 拒否の ACE（SID {BOB}・アクセス 0x10000・フラグ 0x03）を落とします。拒否を落とすと、その相手のアクセスが広がることがあります。要るなら書き直した後に付け直してください"),
        format!("out banto-hub-elev: 親から継承した ACE（SID {CAROL}・アクセス 0x1f01ff・フラグ 0x10）は引き継ぎません（保護した DACL には親から流れません）"),
        format!("err banto-hub-elev: 警告: この継承された ACE は profile の所有者（作った利用者 {CAROL}）のものです。この利用者は書き直した後にこの profile を開けなくなります。使い続けるなら grant-profile-acl {CAROL} default を実行してください"),
        "out banto-hub-elev: 親から継承した ACE（SID S-1-5-32-545・アクセス 0x1200a9・フラグ 0x13）は引き継ぎません（保護した DACL には親から流れません）".to_string(),
        format!("out banto-hub-elev: フォルダの中全体に効かない ACE（SID {ME}・アクセス 0x1301bf・フラグ 0x07）は引き継ぎません"),
        format!("out banto-hub-elev: 「変更」に足りない ACE（SID {ME}・アクセス 0x1200a9・フラグ 0x03。読み取りだけなど）は引き継ぎません"),
        format!("out banto-hub-elev: この PC の利用者のアカウントでない SID（グループなど）の ACE（SID {DOMAIN_USERS}・アクセス 0x1301bf・フラグ 0x03）は引き継ぎません"),
        format!("err banto-hub-elev: 警告: 利用者のアカウントかを確かめられなかったので、ACE（SID {DAVE}・アクセス 0x1301bf・フラグ 0x03）を引き継ぎません: RPC が届かない。この相手は書き直した後にこの profile を開けなくなります。名前の解決が戻ってから grant-profile-acl {DAVE} default を実行してください"),
        format!("out banto-hub-elev: profile 'default'（{}）: ACL を書き直しました（SYSTEM・Administrators にフル、利用者 {ME}・{ALICE}・{BOB} に変更、所有者は Administrators、親から継承しない。中のフォルダ・ファイルも同じ形）", p.display()),
        format!("err banto-hub-elev: 警告: profile の中の {} はジャンクションかシンボリックリンクなので、辿らず・触らず飛ばしました", p.join("x").display()),
        format!("err banto-hub-elev: 警告: profile の中の {} はハードリンク（ほかの名前と中身を共有）なので、触らず飛ばしました（#624）", p.join("f.txt").display()),
        format!("err banto-hub-elev: 警告: profile の中の {} を開けなかったので、触らず飛ばしました（所有者が Administrators を拒否しているなど。そのまま残ります）", p.join("locked").display()),
        "err banto-hub-elev: 書き直しは終えましたが、上の警告 7 件（飛ばした子・落とした ACE など）を確かめてください".to_string(),
    ];
    assert_eq!(lines, expected);
}

/// null DACL は警告して、指定の利用者だけで書き直す。
#[test]
fn a_null_dacl_is_reported() {
    let ops = FakeOps::new();
    ops.dirs.borrow_mut().get_mut(&profile()).unwrap().dacl = Ok(ObservedDacl {
        protected: false,
        owner: ME.into(),
        aces: None,
    });
    let (result, lines) = run(&ops, None, None);
    assert_eq!(result, Ok(()));
    assert!(lines[0].contains("null"), "{lines:#?}");
    assert!(ops.calls().contains(&format!("reset default [{ME}]")));
    assert!(lines.last().unwrap().contains("警告 1 件"));
}

/// 指定の利用者そのものの継承の ACE・所有者が指定の利用者なら、開けなくなる警告は出さない。
#[test]
fn the_owner_warning_is_only_for_owners_left_out() {
    let dacl = ObservedDacl {
        protected: false,
        owner: ME.into(),
        aces: Some(vec![ace(0, 0x10, FULL_ACCESS_MASK, ME)]),
    };
    let carry = guard::plan_carry_over(&dacl, |_| AccountKind::User);
    let plan = access_plan(&sid(ME), "default", &dacl, &carry);
    assert_eq!(plan.warnings, 0);
    assert_eq!(plan.access, DirAccess::with_install_user(sid(ME)));
}

// --- 名前・SID の引数 ---------------------------------------------------------------

#[test]
fn user_argument_forms() {
    // 名前・SID のどちらでも同じ利用者になる。
    for user in ["alice", ALICE] {
        let ops = FakeOps::new();
        let mut lines = Vec::new();
        let pre = preflight(&ops, Some(user), None, &mut |l| lines.push(l)).unwrap();
        assert_eq!(pre.user, sid(ALICE));
        assert_eq!(pre.elevated_user, Some(sid(ME)));
        assert_eq!(pre.root, root());
        assert_eq!(pre.profile_id, "default");
    }
    // 省けばトークンの利用者。
    let ops = FakeOps::new();
    let pre = preflight(&ops, None, Some("p6-1-2"), &mut |_| {}).unwrap();
    assert_eq!(pre.user, sid(ME));
    assert_eq!(pre.profile_id, "p6-1-2");
    // マシンの変数があればそれ（このプロセスの環境は見ない）。
    let mut ops = FakeOps::new();
    let custom = abs("data/BantoHub");
    ops.env.variable = Ok(Some(custom.to_string_lossy().into_owned()));
    let pre = preflight(&ops, None, None, &mut |_| {}).unwrap();
    assert_eq!(pre.root, custom);
}

// --- reset-password・revert-to-commissioning の対象 ----------------------------------

#[test]
fn target_profile_uses_the_machine_root_and_the_strict_id() {
    let env = FakeEnv {
        variable: Ok(None),
        program_data: Ok(program_data()),
        known: Ok(known()),
    };
    let paths = resolve_target_profile(&env, None).unwrap();
    assert_eq!(paths.profile_dir, profile());
    assert_eq!(
        paths.db_path,
        profile().join("config").join("banto-hub.sqlite3")
    );
    for id in ["C:", "x:y", "nul", "default.", ".."] {
        assert!(
            matches!(
                resolve_target_profile(&env, Some(id)),
                Err(TargetError::ProfileId { .. })
            ),
            "{id}"
        );
    }
    let env = FakeEnv {
        variable: Ok(Some("relative".into())),
        program_data: Ok(program_data()),
        known: Ok(known()),
    };
    let error = resolve_target_profile(&env, None).unwrap_err();
    assert!(matches!(error, TargetError::Root(_)));
    assert!(error
        .to_string()
        .starts_with("banto-hub-elev: Hub のルートを決められません: "));
}

#[test]
fn machine_root_wording() {
    let text = machine_root_text(&MachineRootError::NeedsExpansion("%X%".into()));
    assert!(text.contains("BANTO_HUB_ROOT"), "{text}");
    assert!(text.contains("展開しない絶対パス"), "{text}");
}
