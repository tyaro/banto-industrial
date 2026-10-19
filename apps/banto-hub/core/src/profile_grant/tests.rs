//! 偽物の [`HubElevOps`] で、確かめの順序・副作用が無いこと・出力を確かめる（全 OS）。
//! 本物の ACL の操作での反証は`win/tests.rs`。

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use banto_safe_root::acl::{
    ObservedAce, ObservedDacl, PinnedDirInfo, ADMINISTRATORS_SID, FULL_ACCESS_MASK,
    MODIFY_ACCESS_MASK, READ_EXECUTE_ACCESS_MASK, SYSTEM_SID,
};
use banto_safe_root::guard::{AccountKind, KnownFolders, MachineEnvironment, MachineRootError};
use banto_safe_root::mode::{ModeAccess, RelaxReport, TreeProblem};
use banto_safe_root::{DirAccess, InstallUserSid};
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
    relax: Result<RelaxReport, String>,
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
            relax: Ok(RelaxReport {
                written: 1,
                skipped: Vec::new(),
            }),
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
    fn relax_tree(&self, mode: &ModeAccess) -> Result<RelaxReport, String> {
        // 書く形は設定モードの形（利用者に「変更」）。下から・自身を最後に書くのは
        // `PinnedDir::relax_tree`（banto-safe-root）の中で、そのテストが受け持つ。
        assert_eq!(
            mode.setting(),
            &DirAccess::with_install_users(mode.users().iter().cloned())
        );
        let users: Vec<&str> = mode.users().iter().map(InstallUserSid::as_str).collect();
        self.log
            .borrow_mut()
            .push(format!("relax {} [{}]", name(&self.path), users.join(",")));
        self.dir.relax.clone()
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
    /// `owner_lookup`の結果（無ければ`kinds`から: 利用者・グループなど・確かめられない）。
    lookups: HashMap<String, OwnerLookup>,
    in_use: Result<bool, String>,
    dirs: RefCell<HashMap<PathBuf, FakeDir>>,
    /// `create_dir`が作るフォルダ（既定は Administrators 所有。すり替えの再現に使う）。
    on_create: RefCell<HashMap<PathBuf, FakeDir>>,
    /// `create_child`の直前に誰かが作っていた段（`create_child`は`Ok(None)`を返す）。
    /// 親のハンドルから相対で開いたとき（`open_child`）だけ見え、パスで開くと見えない
    /// （その間にパスが別の場所を指すようになった、の再現。#641 Nit 5）。
    appear_on_create: RefCell<HashMap<PathBuf, FakeDir>>,
    /// `appear_on_create`から移した、相対で開くときだけ見える段。
    relative_only: RefCell<HashMap<PathBuf, FakeDir>>,
    /// `pin_file`が開けるファイル。
    files: RefCell<HashMap<PathBuf, FileFacts>>,
    /// HKLM の戻す相手の記録（`read_owners`が返し、`write_owners`が書き換える）。
    owners: RefCell<Result<Option<Vec<String>>, String>>,
    /// サービスが登録されているか（`Err`は`write_owners`の失敗）。
    registered: Result<bool, String>,
    log: Log,
}

/// 偽の開いたままのファイル（drop を記録する）。
struct FakeFile {
    name: String,
    log: Log,
}

impl Drop for FakeFile {
    fn drop(&mut self) {
        self.log.borrow_mut().push(format!("drop {}", self.name));
    }
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
            lookups: HashMap::new(),
            in_use: Ok(false),
            dirs: RefCell::new(HashMap::new()),
            on_create: RefCell::new(HashMap::new()),
            appear_on_create: RefCell::new(HashMap::new()),
            relative_only: RefCell::new(HashMap::new()),
            files: RefCell::new(HashMap::new()),
            owners: RefCell::new(Ok(None)),
            registered: Ok(true),
            log: Rc::new(RefCell::new(Vec::new())),
        };
        for dir in [root(), profiles(), profile()] {
            ops.dirs.borrow_mut().insert(dir, FakeDir::owned_by(ME));
        }
        // ルートの親（ProgramData）。
        ops.dirs
            .borrow_mut()
            .insert(program_data(), FakeDir::owned_by(SYSTEM_SID));
        ops
    }

    /// ルート・profiles・profile を消す（ルートの親の ProgramData は残す）。
    fn remove_tree(&self) {
        let mut dirs = self.dirs.borrow_mut();
        for dir in [root(), profiles(), profile()] {
            dirs.remove(&dir);
        }
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
    fn owner_lookup(&self, sid: &InstallUserSid) -> OwnerLookup {
        if let Some(found) = self.lookups.get(sid.as_str()) {
            return found.clone();
        }
        match self.account_kind(sid) {
            AccountKind::User => OwnerLookup::User,
            AccountKind::NotAUser => OwnerLookup::NotAUser,
            AccountKind::LookupFailed(why) => OwnerLookup::LookupFailed(why),
        }
    }
    type ProfileHold = FakeFile;
    fn hold_profile(&self, profile_id: &str) -> Result<Option<FakeFile>, String> {
        self.log.borrow_mut().push(format!("in_use {profile_id}"));
        self.in_use.clone().map(|in_use| {
            (!in_use).then(|| FakeFile {
                name: format!("hold {profile_id}"),
                log: self.log.clone(),
            })
        })
    }
    fn pin_dir(&self, dir: &Path) -> Result<Option<FakePinned>, String> {
        self.log.borrow_mut().push(format!("pin {}", name(dir)));
        Ok(self.dirs.borrow().get(dir).cloned().map(|d| FakePinned {
            path: dir.to_path_buf(),
            dir: d,
            log: self.log.clone(),
        }))
    }
    type HeldFile = FakeFile;
    fn pin_file(&self, path: &Path) -> Result<Option<(FakeFile, FileFacts)>, String> {
        self.log
            .borrow_mut()
            .push(format!("pin_file {}", name(path)));
        Ok(self.files.borrow().get(path).cloned().map(|facts| {
            (
                FakeFile {
                    name: name(path),
                    log: self.log.clone(),
                },
                facts,
            )
        }))
    }
    fn create_child(
        &self,
        parent: &FakePinned,
        name_: &str,
        how: CreateHow,
    ) -> Result<Option<FakePinned>, String> {
        let dir = parent.path.join(name_);
        self.log.borrow_mut().push(format!(
            "create {} under {} {how:?}",
            name_,
            name(&parent.path)
        ));
        if self.dirs.borrow().contains_key(&dir) {
            return Ok(None);
        }
        if let Some(raced) = self.appear_on_create.borrow_mut().remove(&dir) {
            self.relative_only.borrow_mut().insert(dir, raced);
            return Ok(None);
        }
        let made = self
            .on_create
            .borrow_mut()
            .remove(&dir)
            .unwrap_or_else(|| FakeDir::owned_by(ADMINISTRATORS_SID));
        self.dirs.borrow_mut().insert(dir.clone(), made.clone());
        Ok(Some(FakePinned {
            path: dir,
            dir: made,
            log: self.log.clone(),
        }))
    }
    fn read_owners(&self, profile_id: &str) -> Result<Option<Vec<String>>, String> {
        self.log
            .borrow_mut()
            .push(format!("read_owners {profile_id}"));
        self.owners.borrow().clone()
    }
    fn write_owners(
        &self,
        profile_id: &str,
        owners: &[InstallUserSid],
    ) -> Result<OwnersWrite, String> {
        assert!(!owners.is_empty(), "空の組を書こうとした");
        let items: Vec<String> = owners.iter().map(|s| s.as_str().to_string()).collect();
        self.log
            .borrow_mut()
            .push(format!("write_owners {profile_id} [{}]", items.join(",")));
        match self.registered.clone()? {
            true => {
                *self.owners.borrow_mut() = Ok(Some(items));
                Ok(OwnersWrite::Written)
            }
            false => Ok(OwnersWrite::NotRegistered),
        }
    }
    fn open_child(&self, parent: &FakePinned, name_: &str) -> Result<Option<FakePinned>, String> {
        let dir = parent.path.join(name_);
        self.log
            .borrow_mut()
            .push(format!("open {} under {}", name_, name(&parent.path)));
        let found = self
            .relative_only
            .borrow()
            .get(&dir)
            .cloned()
            .or_else(|| self.dirs.borrow().get(&dir).cloned());
        Ok(found.map(|d| FakePinned {
            path: dir,
            dir: d,
            log: self.log.clone(),
        }))
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
        // #641 のオーナーレビュー Nit 4: `COM0`・`LPT0` も予約名。
        ("COM0", Err(E::Reserved)),
        ("lpt0", Err(E::Reserved)),
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
    assert!(ops.calls().iter().any(|c| c.starts_with("relax default")));

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
            &format!("write_owners default [{ME}]"),
            &format!("relax default [{ME}]"),
            "drop default",
            "drop profiles",
            "drop BantoHub",
            "drop hold default",
        ]
    );
    let profile_dir = profile();
    assert_eq!(
        lines,
        vec![
            format!("out banto-hub-elev: 戻す相手の記録（{OWNERS_AT}）を利用者 {ME} に更新しました"),
            format!(
                "out banto-hub-elev: profile 'default'（{}）: ACL を書き直しました（SYSTEM・Administrators にフル、利用者 {ME} に変更、所有者は Administrators、親から継承しない。中のフォルダ・ファイルも同じ形で、下から書き profile 自身は最後。書いた数 1）",
                profile_dir.display()
            ),
        ]
    );
    assert_eq!(*ops.owners.borrow(), Ok(Some(vec![ME.to_string()])));
}

/// 無いものは作る: ルート・profiles は親から継承する普通のフォルダ、profile は
/// SYSTEM・Administrators だけで作り、確かめてから書き直す。
#[test]
fn missing_dirs_are_created_then_checked_then_reset() {
    let ops = FakeOps::new();
    ops.remove_tree();
    let (result, lines) = run(&ops, Some("alice"), Some("line-2"));
    assert_eq!(result, Ok(()), "{lines:#?}");
    // 無い段は、確かめ済みの親のハンドルを基準に作り、作ったハンドルをそのまま使う
    // （パスで開き直さない - #626 PR-B の再レビュー 1）。ルートの親は先に開いて確かめる。
    assert_eq!(
        ops.calls(),
        vec![
            "in_use line-2",
            "pin BantoHub",
            "pin ProgramData",
            "create BantoHub under ProgramData Inherit",
            "pin profiles",
            "create profiles under BantoHub Inherit",
            "pin line-2",
            "create line-2 under profiles ServiceOnly",
            "read_dacl line-2",
            &format!("write_owners line-2 [{ALICE}]"),
            &format!("relax line-2 [{ALICE}]"),
            "drop line-2",
            "drop profiles",
            "drop BantoHub",
            "drop ProgramData",
            "drop hold line-2",
        ]
    );
    assert!(lines[1].contains(": 作りました（"), "{lines:#?}");
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
    assert!(!ops.calls().iter().any(|c| c.starts_with("relax")));
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
    ops.remove_tree();
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

/// #641 のオーナーレビュー Nit 5: 確かめてから作るまでの間に誰かが作っていた段（`create_child`
/// が`Ok(None)`）は、パスではなく**親のハンドルから相対で**開き直す。偽物では、その段は
/// 相対で開いたときだけ見え、パスで開くと見えない（パスが別の場所を指すようになった）ので、
/// パスで開き直すと「無くなっていました」で断る。
#[test]
fn a_level_created_by_someone_else_is_reopened_relative_to_the_parent() {
    let ops = FakeOps::new();
    ops.dirs.borrow_mut().remove(&profile());
    ops.appear_on_create
        .borrow_mut()
        .insert(profile(), FakeDir::owned_by(ME));
    let (result, lines) = run(&ops, None, None);
    assert_eq!(result, Ok(()), "{lines:#?}");
    assert_eq!(
        ops.calls(),
        vec![
            "in_use default",
            "pin BantoHub",
            "pin profiles",
            "pin default",
            "create default under profiles ServiceOnly",
            // パスで開き直さない（`pin default`は 1 回だけ）。
            "open default under profiles",
            "read_dacl default",
            &format!("write_owners default [{ME}]"),
            &format!("relax default [{ME}]"),
            "drop default",
            "drop profiles",
            "drop BantoHub",
            "drop hold default",
        ]
    );
    // 自分で作ったものではないので「作りました」ではない。
    assert!(lines[1].contains(": ACL を書き直しました（"), "{lines:#?}");

    // ルートの段（親はルートの親のハンドル）も同じ。
    let ops = FakeOps::new();
    ops.remove_tree();
    ops.appear_on_create
        .borrow_mut()
        .insert(root(), FakeDir::owned_by(ME));
    let (result, lines) = run(&ops, None, None);
    assert_eq!(result, Ok(()), "{lines:#?}");
    let calls = ops.calls();
    assert_eq!(
        calls[..5],
        [
            "in_use default",
            "pin BantoHub",
            "pin ProgramData",
            "create BantoHub under ProgramData Inherit",
            "open BantoHub under ProgramData",
        ]
    );
    assert_eq!(calls.iter().filter(|c| *c == "pin BantoHub").count(), 1);
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
            !ops.calls().iter().any(|c| c.starts_with("relax")),
            "{label}"
        );
        assert!(
            lines.iter().any(|l| l.contains(needle)),
            "{label}: {lines:#?}"
        );
    }

    // 書けない: 書き直しを試みて失敗を出す（成功の行は出さない）。
    let ops = FakeOps::new();
    ops.dirs.borrow_mut().get_mut(&profile()).unwrap().relax = Err("1307".into());
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
        dir.relax = Ok(RelaxReport {
            written: 5,
            skipped: vec![
                TreeProblem::ReparsePoint {
                    path: profile().join("x"),
                    tag: 0xA000_0003,
                },
                TreeProblem::Hardlink {
                    path: profile().join("f.txt"),
                    links: 2,
                },
                TreeProblem::Denied {
                    path: profile().join("locked"),
                },
                TreeProblem::Busy {
                    path: profile().join("busy"),
                    os_error: 32,
                },
                TreeProblem::MovedOut {
                    path: profile().join("moved"),
                    now: abs("elsewhere"),
                },
            ],
        });
    }
    // 所有者 CAROL を許すため、実行した管理者を CAROL にする。
    let mut ops = ops;
    ops.me = Ok(sid(CAROL));
    let (result, lines) = run(&ops, Some(ME), None);
    assert_eq!(result, Ok(()), "{lines:#?}");
    assert!(ops
        .calls()
        .contains(&format!("relax default [{ME},{ALICE}]")));
    assert!(ops
        .calls()
        .contains(&format!("write_owners default [{ME},{ALICE}]")));
    let p = profile();
    let expected = vec![
        format!("out banto-hub-elev: 今の DACL に「変更」を持つ利用者 {ALICE} を引き継ぎます"),
        format!("err banto-hub-elev: 警告: 利用者 {ALICE} の今の権限は「変更」より広い（フルコントロールなど）ので、「変更」に狭めます（ACL の変更・所有権の取得はできなくなります）"),
        format!("err banto-hub-elev: 警告: 利用者 {BOB} には拒否の ACE があったので、許可の ACE（SID {BOB}・アクセス 0x1301bf・フラグ 0x03）を引き継ぎません（引き継ぐと拒否が消えて権利が広がるため）。要るなら grant-profile-acl {BOB} default を手で実行してください"),
        format!("err banto-hub-elev: 警告: 拒否の ACE（SID {BOB}・アクセス 0x10000・フラグ 0x03）を落とします。拒否を落とすと、その相手のアクセスが広がることがあります。要るなら書き直した後に付け直してください"),
        format!("out banto-hub-elev: 親から継承した ACE（SID {CAROL}・アクセス 0x1f01ff・フラグ 0x10）は引き継ぎません（保護した DACL には親から流れません）"),
        format!("err banto-hub-elev: 警告: この継承された ACE は profile の所有者（作った利用者 {CAROL}）のものです。この利用者は書き直した後にこの profile を開けなくなります。使い続けるなら grant-profile-acl {CAROL} default を実行してください"),
        "out banto-hub-elev: 親から継承した ACE（SID S-1-5-32-545・アクセス 0x1200a9・フラグ 0x13）は引き継ぎません（保護した DACL には親から流れません）".to_string(),
        format!("out banto-hub-elev: フォルダの中全体に効かない ACE（SID {ME}・アクセス 0x1301bf・フラグ 0x07）は引き継ぎません"),
        format!("out banto-hub-elev: 「変更」に足りない ACE（SID {ME}・アクセス 0x1200a9・フラグ 0x03。読み取りだけなど）は引き継ぎません"),
        format!("out banto-hub-elev: この PC の利用者のアカウントでない SID（グループなど）の ACE（SID {DOMAIN_USERS}・アクセス 0x1301bf・フラグ 0x03）は引き継ぎません"),
        format!("err banto-hub-elev: 警告: 利用者のアカウントかを確かめられなかったので、ACE（SID {DAVE}・アクセス 0x1301bf・フラグ 0x03）を引き継ぎません: RPC が届かない。この相手は書き直した後にこの profile を開けなくなります。名前の解決が戻ってから grant-profile-acl {DAVE} default を実行してください"),
        format!("out banto-hub-elev: 戻す相手の記録（{OWNERS_AT}）を利用者 {ME}・{ALICE} に更新しました"),
        format!("out banto-hub-elev: profile 'default'（{}）: ACL を書き直しました（SYSTEM・Administrators にフル、利用者 {ME}・{ALICE} に変更、所有者は Administrators、親から継承しない。中のフォルダ・ファイルも同じ形で、下から書き profile 自身は最後。書いた数 5）", p.display()),
        format!("err banto-hub-elev: 警告: profile の中の {}: リンク（reparse point、tag 0xa0000003）。ジャンクションかシンボリックリンクなので、辿らず・触らず飛ばしました", p.join("x").display()),
        format!("err banto-hub-elev: 警告: profile の中の {}: ハードリンクの別名があります（リンク数 2）。ハードリンク（ほかの名前と中身を共有）なので、触らず飛ばしました（#624）", p.join("f.txt").display()),
        format!("err banto-hub-elev: 警告: profile の中の {}: 開けません（アクセス拒否）。開けなかったので、触らず飛ばしました（所有者が Administrators を拒否しているなど。そのまま残ります）", p.join("locked").display()),
        format!("err banto-hub-elev: 警告: profile の中の {}: 使用中で開けません（os error 32）。使用中で開けなかったので、触らず飛ばしました（そのまま残ります）", p.join("busy").display()),
        format!("err banto-hub-elev: 警告: profile の中の {}: 書いている最中に木の外（{}）へ移されました。書いている最中に profile の外へ移されたので、そのフォルダは書かず飛ばしました（移される前に書いた中身は移った先に残ります。#623 S-1）", p.join("moved").display(), abs("elsewhere").display()),
        "err banto-hub-elev: 書き直しは終えましたが、上の警告 10 件（飛ばした子・落とした ACE など）を確かめてください".to_string(),
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
    assert!(ops.calls().contains(&format!("relax default [{ME}]")));
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

// --- #626 PR-B の監査の対応 ---------------------------------------------------------

/// B-1: `service-install`では、profile が使用中のときだけ ACL の付け直しを飛ばして警告し、
/// 成功にする（登録・Operators・サービス ACL は済んでいる）。使用中以外の断りは失敗。
#[test]
fn service_install_skips_the_acl_when_the_profile_is_in_use() {
    let mut ops = FakeOps::new();
    ops.in_use = Ok(true);
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
    assert!(result.is_ok());
    assert_eq!(ops.calls(), vec!["register", "in_use default"]);
    assert_eq!(
        texts(lines),
        vec![format!(
            "err banto-hub-elev: 警告: profile 'default' が使用中なので ACL の付け直しを飛ばしました（{}）。サービスとアプリを止めてから grant-profile-acl を実行してください",
            crate::profile_paths::mutex_name("default")
        )]
    );

    // 使用中かを確かめられない・所有者の断りは失敗のまま。
    for setup in [
        (|ops: &mut FakeOps| ops.in_use = Err("OpenMutexW".into())) as fn(&mut FakeOps),
        |ops: &mut FakeOps| {
            ops.dirs
                .borrow_mut()
                .get_mut(&profile())
                .unwrap()
                .info
                .owner = EVIL.into();
        },
    ] {
        let mut ops = FakeOps::new();
        setup(&mut ops);
        let result = service_install(&ops, || -> Result<(), String> { Ok(()) }, &mut |_| {});
        assert!(matches!(result, Err(ServiceInstallError::Refused)));
    }

    // 手で実行する grant-profile-acl は、使用中なら断って失敗のまま。
    let mut ops = FakeOps::new();
    ops.in_use = Ok(true);
    let (result, lines) = run(&ops, None, None);
    assert_eq!(result, Err(Refused));
    assert!(lines[0].contains("は使用中です"), "{lines:#?}");
}

/// B-2: 指定の利用者そのものに拒否の ACE があっても指定どおり付けるが、拒否が消える旨の
/// 警告を出す。ほかの利用者は拒否があれば引き継がない。
#[test]
fn deny_aces_block_carry_over_and_warn_for_the_named_user() {
    let ops = FakeOps::new();
    ops.dirs.borrow_mut().get_mut(&profile()).unwrap().dacl = Ok(ObservedDacl {
        protected: false,
        owner: ME.into(),
        aces: Some(vec![
            ace(0, 0x03, MODIFY_ACCESS_MASK, ME),
            ace(1, 0x03, 0x0001_0000, ME),
            ace(0, 0x03, MODIFY_ACCESS_MASK, ALICE),
            ace(1, 0x10, 0x0000_0002, ALICE),
        ]),
    });
    let (result, lines) = run(&ops, None, None);
    assert_eq!(result, Ok(()), "{lines:#?}");
    assert!(ops.calls().contains(&format!("relax default [{ME}]")));
    let named = format!(
        "err banto-hub-elev: 警告: 指定の利用者 {ME} には拒否の ACE がありましたが、指定どおり「変更」を付けます（拒否は消えます）"
    );
    assert!(lines.contains(&named), "{lines:#?}");
    let other = format!("利用者 {ALICE} には拒否の ACE があったので");
    assert!(lines.iter().any(|l| l.contains(&other)), "{lines:#?}");
}

/// C-1: 「変更」より広い警告は引き継いだ ACE だけを見る（引き継がない形の広い ACE では
/// 出さない）。
#[test]
fn the_narrowing_warning_only_looks_at_carried_aces() {
    let dacl = ObservedDacl {
        protected: false,
        owner: ME.into(),
        aces: Some(vec![
            // 指定の利用者のフル（NP つき = 引き継がない形）。
            ace(0, 0x07, FULL_ACCESS_MASK, ME),
            // ALICE は「変更」で引き継ぎ、別に「このフォルダだけ」のフルを持つ。
            ace(0, 0x03, MODIFY_ACCESS_MASK, ALICE),
            ace(0, 0x00, FULL_ACCESS_MASK, ALICE),
        ]),
    };
    let carry = guard::plan_carry_over(&dacl, |_| AccountKind::User);
    let plan = access_plan(&sid(ME), "default", &dacl, &carry);
    let narrowing = plan
        .lines
        .iter()
        .any(|l| matches!(l, ConsoleLine::Stderr(t) if t.contains("狭めます")));
    assert!(!narrowing, "{:#?}", plan.lines);
    assert_eq!(plan.warnings, 0);
}

/// C-3: 小文字の`s-1-`は名前として引かずに、大文字で書くよう案内して断る。
#[test]
fn a_lowercase_sid_is_refused_with_guidance() {
    let ops = FakeOps::new();
    let (result, lines) = run(&ops, Some("s-1-5-21-1-2-3-1001"), None);
    assert_eq!(result, Err(Refused));
    assert!(lines[0].contains("S-1- を大文字で"), "{lines:#?}");
    assert!(ops.calls().is_empty());
}

/// B-6: ルート・profiles の段の所有者の断りは、所有者の取り直しを案内する
/// （`grant-profile-acl <その利用者>`は profile の段だけ）。
#[test]
fn foreign_owner_guidance_depends_on_the_level() {
    for (dir, expect_takeown) in [(root(), true), (profiles(), true), (profile(), false)] {
        let ops = FakeOps::new();
        ops.dirs.borrow_mut().get_mut(&dir).unwrap().info.owner = EVIL.into();
        let (result, lines) = run(&ops, None, None);
        assert_eq!(result, Err(Refused));
        let line = lines.iter().find(|l| l.contains(EVIL)).unwrap();
        if expect_takeown {
            let tail = format!(
                "（中身を確かめ、このフォルダの所有者を Administrators に取り直してから（takeown /f \"{}\" /a か icacls \"{}\" /setowner *S-1-5-32-544）、もう一度実行してください）",
                dir.display(),
                dir.display()
            );
            assert!(line.ends_with(&tail), "{line}");
            assert!(!line.contains("grant-profile-acl"), "{line}");
        } else {
            assert!(line.contains("grant-profile-acl <その利用者>"), "{line}");
            assert!(!line.contains("takeown"), "{line}");
        }
    }
}

// --- B-5: reset-password・revert-to-commissioning の対象 ------------------------------

fn config() -> PathBuf {
    profile().join("config")
}

fn db() -> PathBuf {
    config().join("banto-hub.sqlite3")
}

fn target_ops() -> FakeOps {
    let ops = FakeOps::new();
    ops.dirs
        .borrow_mut()
        .insert(config(), FakeDir::owned_by(ME));
    ops.files.borrow_mut().insert(
        db(),
        FileFacts {
            is_dir: false,
            is_reparse_point: false,
            links: 1,
            final_path: db(),
        },
    );
    ops
}

/// 正常: profile の mutex を取り（#661 661-3）、ルート → profiles → profile → config →
/// DB の順に辿らずに開き、持ったまま返す（閉じるのは返したものを drop したとき、逆の順。
/// mutex は最後）。
#[test]
fn open_target_profile_pins_every_level_and_holds_until_dropped() {
    let ops = target_ops();
    let (paths, held) = open_target_profile(&ops, None).unwrap();
    assert_eq!(paths.db_path, db());
    assert_eq!(
        ops.calls(),
        vec![
            "in_use default",
            "pin BantoHub",
            "pin profiles",
            "pin default",
            "pin config",
            "pin_file banto-hub.sqlite3"
        ]
    );
    drop(held);
    assert_eq!(
        ops.calls()[6..],
        [
            "drop banto-hub.sqlite3",
            "drop config",
            "drop default",
            "drop profiles",
            "drop BantoHub",
            "drop hold default"
        ]
    );
}

/// #661 661-3: `reset-password`・`revert-to-commissioning`は、profile が使用中（Hub の
/// サービスかアプリが mutex を持っている）なら何も開かずに断る（稼働中の DB を開かない）。
/// 確かめられないときも開かずに断る。
#[test]
fn open_target_profile_refuses_a_profile_in_use_before_opening_anything() {
    let mut ops = target_ops();
    ops.in_use = Ok(true);
    assert!(matches!(
        open_target_profile(&ops, None),
        Err(OpenTargetError::InUse(id)) if id == "default"
    ));
    assert_eq!(ops.calls(), vec!["in_use default"]);

    let mut ops = target_ops();
    ops.in_use = Err("CreateMutexW が失敗".to_string());
    match open_target_profile(&ops, None) {
        Err(OpenTargetError::Unsafe(text)) => {
            assert!(text.contains("使用中かを確かめられませんでした"), "{text}");
            assert!(text.contains("CreateMutexW が失敗"), "{text}");
        }
        Err(other) => panic!("{other:?}"),
        Ok(_) => panic!("通ってしまった"),
    }
    assert_eq!(ops.calls(), vec!["in_use default"]);
}

/// 断る: config がジャンクション・途中の段の本当のパスが違う（すり替え）・DB が
/// ハードリンク・reparse point・フォルダ・本当のパスが違う。無ければ DbNotFound。
#[test]
fn open_target_profile_refuses_links_and_swaps() {
    type Tweak = Box<dyn Fn(&FakeOps)>;
    let dir = |path: fn() -> PathBuf, f: fn(&mut FakeDir)| -> Tweak {
        Box::new(move |ops: &FakeOps| f(ops.dirs.borrow_mut().get_mut(&path()).unwrap()))
    };
    let file = |f: fn(&mut FileFacts)| -> Tweak {
        Box::new(move |ops: &FakeOps| f(ops.files.borrow_mut().get_mut(&db()).unwrap()))
    };
    let cases: Vec<(&str, Tweak, &str)> = vec![
        (
            "config junction",
            dir(config, |d| d.info.is_reparse_point = true),
            "reparse point",
        ),
        (
            "config swapped",
            dir(config, |d| d.final_path = Some(abs("elsewhere/config"))),
            "本当の場所",
        ),
        (
            "profile swapped",
            dir(profile, |d| d.final_path = Some(abs("elsewhere/default"))),
            "本当の場所",
        ),
        (
            "profiles not a dir",
            dir(profiles, |d| d.info.is_dir = false),
            "フォルダではありません",
        ),
        ("db hardlink", file(|f| f.links = 2), "ハードリンク"),
        (
            "db reparse",
            file(|f| f.is_reparse_point = true),
            "reparse point",
        ),
        ("db dir", file(|f| f.is_dir = true), "フォルダです"),
        (
            "db elsewhere",
            file(|f| f.final_path = abs("elsewhere/banto-hub.sqlite3")),
            "本当の場所",
        ),
    ];
    for (label, tweak, needle) in cases {
        let ops = target_ops();
        tweak(&ops);
        match open_target_profile(&ops, None) {
            Err(OpenTargetError::Unsafe(text)) => assert!(text.contains(needle), "{label}: {text}"),
            Err(other) => panic!("{label}: {other:?}"),
            Ok(_) => panic!("{label}: 通ってしまった"),
        }
    }
    // 無い。
    let ops = target_ops();
    ops.files.borrow_mut().clear();
    assert!(matches!(
        open_target_profile(&ops, None),
        Err(OpenTargetError::DbNotFound(path)) if path == db()
    ));
    let ops = target_ops();
    ops.dirs.borrow_mut().remove(&config());
    assert!(matches!(
        open_target_profile(&ops, None),
        Err(OpenTargetError::DbNotFound(_))
    ));
    // 不正な profile-id はどこも開かない。
    let ops = target_ops();
    assert!(matches!(
        open_target_profile(&ops, Some("C:")),
        Err(OpenTargetError::Target(_))
    ));
    assert!(ops.calls().is_empty());
}

// --- #626 PR-B の再レビュー ----------------------------------------------------------

/// 1: ルートが無いとき、ルートの親を開いて確かめるまで何も作らない。親が無い・本当の
/// 場所が違う（途中のジャンクション）・reparse point・フォルダでない・OS の置き場の中なら、
/// 何も作らずに断る。
#[test]
fn a_missing_root_is_created_only_under_a_checked_parent() {
    type Tweak = Box<dyn Fn(&FakeOps)>;
    let parent = |f: fn(&mut FakeDir)| -> Tweak {
        Box::new(move |ops: &FakeOps| f(ops.dirs.borrow_mut().get_mut(&program_data()).unwrap()))
    };
    let cases: Vec<(&str, Tweak, &str)> = vec![
        (
            "no parent",
            Box::new(|ops: &FakeOps| {
                ops.dirs.borrow_mut().remove(&program_data());
            }),
            "親から作ることはしません",
        ),
        (
            "parent behind a junction",
            parent(|d| d.final_path = Some(abs("elsewhere/ProgramData"))),
            "本当の場所",
        ),
        (
            "parent is a reparse point",
            parent(|d| d.info.is_reparse_point = true),
            "reparse point",
        ),
        (
            "parent not a dir",
            parent(|d| d.info.is_dir = false),
            "フォルダではありません",
        ),
        (
            "parent inside Windows",
            parent(|d| d.final_path = Some(abs("Windows"))),
            "本当の場所",
        ),
    ];
    for (label, tweak, needle) in cases {
        let ops = FakeOps::new();
        ops.remove_tree();
        tweak(&ops);
        let (result, lines) = run(&ops, None, None);
        assert_eq!(result, Err(Refused), "{label}: {lines:#?}");
        assert!(
            !ops.calls().iter().any(|c| c.starts_with("create")),
            "{label}: 作った: {:?}",
            ops.calls()
        );
        assert!(
            lines.iter().any(|l| l.contains(needle)),
            "{label}: {lines:#?}"
        );
        assert!(lines.iter().all(|l| !l.contains("残っています")), "{label}");
    }
}

/// 1（純関数）: ルートの親の確かめ。ProgramData そのもの・ドライブのルートは親として
/// 認め、OS の置き場の中・本当の場所の食い違いは断る。
#[test]
fn root_parent_table() {
    let dir = |reparse: bool, is_dir: bool| PinnedDirInfo {
        is_dir,
        is_reparse_point: reparse,
        owner: SYSTEM_SID.into(),
    };
    let ok = dir(false, true);
    assert_eq!(
        root_parent_problem(&program_data(), &program_data(), &ok, &known()),
        None
    );
    let drive = abs("");
    assert_eq!(root_parent_problem(&drive, &drive, &ok, &known()), None);
    assert!(root_parent_problem(&abs("x"), &program_data(), &ok, &known()).is_some());
    assert!(root_parent_problem(&abs("Windows/x"), &abs("Windows/x"), &ok, &known()).is_some());
    assert!(root_parent_problem(&abs("data"), &abs("data"), &dir(true, true), &known()).is_some());
    assert!(
        root_parent_problem(&abs("data"), &abs("data"), &dir(false, false), &known()).is_some()
    );
}

/// 2: 種類の分からない ACE・SID を読めない ACE があれば、引き継ぎを決めずに断る
/// （書き直さない）。条件付きの許可は引き継がずに警告する。
#[test]
fn unknown_or_unreadable_aces_refuse_and_conditional_allows_are_not_carried() {
    for blocked in [
        ace(0x13, 0x03, MODIFY_ACCESS_MASK, ALICE),
        ace(10, 0x03, 0x0004_0000, ""),
    ] {
        let ops = FakeOps::new();
        ops.dirs.borrow_mut().get_mut(&profile()).unwrap().dacl = Ok(ObservedDacl {
            protected: false,
            owner: ME.into(),
            aces: Some(vec![
                ace(0, 0x03, MODIFY_ACCESS_MASK, ALICE),
                blocked.clone(),
            ]),
        });
        let (result, lines) = run(&ops, None, None);
        assert_eq!(result, Err(Refused), "{blocked:?}: {lines:#?}");
        assert!(!ops.calls().iter().any(|c| c.starts_with("relax")));
        assert!(
            lines
                .iter()
                .any(|l| l.contains("種類の分からない ACE か SID を読めない ACE")),
            "{lines:#?}"
        );
    }

    let ops = FakeOps::new();
    ops.dirs.borrow_mut().get_mut(&profile()).unwrap().dacl = Ok(ObservedDacl {
        protected: false,
        owner: ME.into(),
        aces: Some(vec![ace(9, 0x03, MODIFY_ACCESS_MASK, ALICE)]),
    });
    let (result, lines) = run(&ops, None, None);
    assert_eq!(result, Ok(()), "{lines:#?}");
    assert!(ops.calls().contains(&format!("relax default [{ME}]")));
    assert!(
        lines
            .iter()
            .any(|l| l.contains("条件付き・オブジェクトの許可の ACE")),
        "{lines:#?}"
    );
}

/// 3: profile の mutex は書き直しが終わるまで持つ（放すのは最後）。
#[test]
fn the_profile_mutex_is_held_until_the_rewrite_ends() {
    let ops = FakeOps::new();
    let (result, _) = run(&ops, None, None);
    assert_eq!(result, Ok(()));
    let calls = ops.calls();
    let reset = calls.iter().position(|c| c.starts_with("relax")).unwrap();
    let released = calls.iter().position(|c| c == "drop hold default").unwrap();
    assert_eq!(calls[0], "in_use default");
    assert!(released > reset, "{calls:?}");
    assert_eq!(released, calls.len() - 1, "{calls:?}");
}

// --- #661 661-3: 戻す相手の記録・締まったまま残った profile の修復 -------------------

const OWNERS_AT: &str =
    r"HKLM\SYSTEM\CurrentControlSet\Services\BantoHub\Parameters\Profiles\default\Owners";

/// 運用モードの形（締まっている）の profile の DACL（利用者は「読み取りと実行」だけ）。
fn tight_profile(ops: &FakeOps, users: &[&str]) {
    let mut aces = vec![
        ace(0, 0x03, FULL_ACCESS_MASK, SYSTEM_SID),
        ace(0, 0x03, FULL_ACCESS_MASK, ADMINISTRATORS_SID),
    ];
    for user in users {
        aces.push(ace(0, 0x03, READ_EXECUTE_ACCESS_MASK, user));
    }
    let mut dirs = ops.dirs.borrow_mut();
    let dir = dirs.get_mut(&profile()).unwrap();
    dir.info.owner = ADMINISTRATORS_SID.into();
    dir.dacl = Ok(ObservedDacl {
        protected: true,
        owner: ADMINISTRATORS_SID.into(),
        aces: Some(aces),
    });
}

/// 記録を書けない（権限・レジストリの失敗）なら、ACL を変えずに断る。サービスが登録されて
/// いなければ記録は書かずに知らせ、緩めるのは続ける（デスクトップだけで使うとき）。
#[test]
fn the_owners_record_is_written_before_relaxing_or_the_run_is_refused() {
    let mut ops = FakeOps::new();
    ops.registered = Err("記録を書く権限がありません".to_string());
    let (result, lines) = run(&ops, None, None);
    assert_eq!(result, Err(Refused), "{lines:#?}");
    assert!(!ops.calls().iter().any(|c| c.starts_with("relax")));
    assert_eq!(
        lines.last().unwrap(),
        &format!("err banto-hub-elev: 戻す相手の記録（{OWNERS_AT}）を書けませんでした: 記録を書く権限がありません。ACL は変えていません")
    );

    let mut ops = FakeOps::new();
    ops.registered = Ok(false);
    let (result, lines) = run(&ops, None, None);
    assert_eq!(result, Ok(()), "{lines:#?}");
    assert!(ops.calls().contains(&format!("relax default [{ME}]")));
    assert_eq!(
        lines[0],
        format!("out banto-hub-elev: サービス BantoHub が登録されていないので、戻す相手の記録（{OWNERS_AT}）は書きません（デスクトップだけで使うときは要りません）")
    );
    assert_eq!(*ops.owners.borrow(), Ok(None));
}

/// 修復（記録あり）: 締まったまま残った profile は、DACL からではなく記録の組 + 指定の
/// 利用者で緩め、記録もその組に書き直す。
#[test]
fn a_tight_profile_is_relaxed_with_the_recorded_owners() {
    let ops = FakeOps::new();
    tight_profile(&ops, &[ALICE, BOB]);
    *ops.owners.borrow_mut() = Ok(Some(vec![ALICE.to_string()]));
    let (result, lines) = run(&ops, None, None);
    assert_eq!(result, Ok(()), "{lines:#?}");
    assert_eq!(
        ops.calls()[4..8],
        [
            "read_dacl default".to_string(),
            "read_owners default".to_string(),
            format!("write_owners default [{ME},{ALICE}]"),
            format!("relax default [{ME},{ALICE}]"),
        ]
    );
    assert_eq!(
        lines,
        vec![
            "out banto-hub-elev: profile 'default' は運用モードの形（締まったまま）です。設定モードの形に戻します（修復）".to_string(),
            format!("out banto-hub-elev: 戻す相手の記録（{OWNERS_AT}）の利用者 {ALICE} に「変更」を戻します"),
            format!("out banto-hub-elev: 指定の利用者 {ME} も足します"),
            format!("out banto-hub-elev: 戻す相手の記録（{OWNERS_AT}）を利用者 {ME}・{ALICE} に更新しました"),
            format!("out banto-hub-elev: profile 'default'（{}）: ACL を書き直しました（SYSTEM・Administrators にフル、利用者 {ME}・{ALICE} に変更、所有者は Administrators、親から継承しない。中のフォルダ・ファイルも同じ形で、下から書き profile 自身は最後。書いた数 1）", profile().display()),
        ]
    );
    assert_eq!(
        *ops.owners.borrow(),
        Ok(Some(vec![ME.to_string(), ALICE.to_string()]))
    );
}

/// 修復（記録が無い・使えない）: 指定の利用者 + 今の DACL で「読み取りと実行」を持つ利用者の
/// アカウントで緩め、記録が使えない旨を警告する（確かめられない SID があれば断る。下の別のテスト）。
#[test]
fn a_tight_profile_without_a_usable_record_uses_the_named_user_and_the_tight_dacl() {
    type Record = Result<Option<Vec<String>>, String>;
    let records: Vec<(Record, &str)> = vec![
        (Ok(None), "記録がありません"),
        (Ok(Some(Vec::new())), "記録が空です"),
        (
            Ok(Some(vec!["S-1-1-0".to_string()])),
            "記録に利用者の SID でない値があります（\"S-1-1-0\"）",
        ),
        (Err("読めない".to_string()), "読めない"),
    ];
    for (record, reason) in records {
        let ops = FakeOps::new();
        tight_profile(&ops, &[ALICE, DOMAIN_USERS, ME]);
        *ops.owners.borrow_mut() = record;
        let (result, lines) = run(&ops, None, None);
        assert_eq!(result, Ok(()), "{reason}: {lines:#?}");
        assert!(ops
            .calls()
            .contains(&format!("write_owners default [{ME},{ALICE}]")));
        assert!(ops
            .calls()
            .contains(&format!("relax default [{ME},{ALICE}]")));
        assert_eq!(
            lines,
            vec![
                "out banto-hub-elev: profile 'default' は運用モードの形（締まったまま）です。設定モードの形に戻します（修復）".to_string(),
                format!("err banto-hub-elev: 警告: 戻す相手の記録（{OWNERS_AT}）が使えません（{reason}）。指定の利用者 {ME} と、今の DACL で「読み取りと実行」を持つ利用者（締めたときの組）に「変更」を戻します。ほかにも戻す利用者がいれば、grant-profile-acl <利用者> default を続けて実行してください"),
                format!("out banto-hub-elev: 今の DACL で「読み取りと実行」を持つ利用者 {ALICE} に「変更」を戻します"),
                format!("out banto-hub-elev: この PC の利用者のアカウントでない SID（グループなど）の ACE（SID {DOMAIN_USERS}・アクセス 0x1200a9・フラグ 0x03）には戻しません"),
                format!("out banto-hub-elev: 戻す相手の記録（{OWNERS_AT}）を利用者 {ME}・{ALICE} に更新しました"),
                format!("out banto-hub-elev: profile 'default'（{}）: ACL を書き直しました（SYSTEM・Administrators にフル、利用者 {ME}・{ALICE} に変更、所有者は Administrators、親から継承しない。中のフォルダ・ファイルも同じ形で、下から書き profile 自身は最後。書いた数 1）", profile().display()),
                "err banto-hub-elev: 書き直しは終えましたが、上の警告 1 件（飛ばした子・落とした ACE など）を確かめてください".to_string(),
            ],
            "{reason}"
        );
    }
}

/// 設定モードの形（緩んでいる）なら記録は読まず、DACL から採った組 + 指定の利用者を記録に
/// 書く（記録を書き換えても DACL の組が勝つ）。
#[test]
fn a_loose_profile_ignores_the_record_and_rewrites_it() {
    let ops = FakeOps::new();
    *ops.owners.borrow_mut() = Ok(Some(vec![BOB.to_string()]));
    let (result, lines) = run(&ops, None, None);
    assert_eq!(result, Ok(()), "{lines:#?}");
    assert!(!ops.calls().iter().any(|c| c.starts_with("read_owners")));
    assert_eq!(*ops.owners.borrow(), Ok(Some(vec![ME.to_string()])));
}

/// 修復（記録に削除済みの利用者。#704 レビュー）: 記録の SID は形しか確かめていないので、
/// この PC のローカルで削除済みの Alice（ローカルの SID の`ERROR_NONE_MAPPED`）が残る。Bob を指定して実行すると、
/// Alice は警告して外し、Bob で緩め、記録も Bob に書き直す（残すと緩めるときの全利用者の
/// 確かめで断られ、修復で戻せない）。記録の組が空になっても今の DACL の「読み取りと実行」の
/// 利用者（Carol）には頼らず、指定の利用者だけで進める。
#[test]
fn a_deleted_account_in_the_record_is_dropped_with_a_warning() {
    let mut ops = FakeOps::new();
    ops.names
        .insert("bob".to_string(), Ok(Some(BOB.to_string())));
    ops.lookups
        .insert(ALICE.into(), OwnerLookup::Unmapped { local: true });
    tight_profile(&ops, &[ALICE, CAROL]);
    *ops.owners.borrow_mut() = Ok(Some(vec![ALICE.to_string()]));
    let (result, lines) = run(&ops, Some("bob"), None);
    assert_eq!(result, Ok(()), "{lines:#?}");
    assert!(ops
        .calls()
        .contains(&format!("write_owners default [{BOB}]")));
    assert!(ops.calls().contains(&format!("relax default [{BOB}]")));
    assert_eq!(*ops.owners.borrow(), Ok(Some(vec![BOB.to_string()])));
    assert_eq!(
        lines[..3],
        [
            "out banto-hub-elev: profile 'default' は運用モードの形（締まったまま）です。設定モードの形に戻します（修復）".to_string(),
            format!("err banto-hub-elev: 警告: 戻す相手の記録（{OWNERS_AT}）の {ALICE} はこの PC のローカルのアカウントにありません（削除済み）。記録から外し、「変更」は戻しません"),
            format!("out banto-hub-elev: 指定の利用者 {BOB} も足します"),
        ],
        "{lines:#?}"
    );
    assert!(
        lines.last().unwrap().contains("上の警告 1 件"),
        "{lines:#?}"
    );

    // 反証: 記録に有効な利用者（Carol）が並んでいれば、外すのは削除済みの Alice だけ。
    let mut ops = FakeOps::new();
    ops.names
        .insert("bob".to_string(), Ok(Some(BOB.to_string())));
    ops.lookups
        .insert(ALICE.into(), OwnerLookup::Unmapped { local: true });
    tight_profile(&ops, &[ALICE, CAROL]);
    *ops.owners.borrow_mut() = Ok(Some(vec![ALICE.to_string(), CAROL.to_string()]));
    let (result, lines) = run(&ops, Some("bob"), None);
    assert_eq!(result, Ok(()), "{lines:#?}");
    assert!(ops
        .calls()
        .contains(&format!("relax default [{BOB},{CAROL}]")));
    assert_eq!(
        *ops.owners.borrow(),
        Ok(Some(vec![BOB.to_string(), CAROL.to_string()]))
    );
    assert!(lines.contains(&format!(
        "out banto-hub-elev: 戻す相手の記録（{OWNERS_AT}）の利用者 {CAROL} に「変更」を戻します"
    )));
}

/// 修復（記録の SID を確かめられない。#704 レビュー）: ドメインに届かないなどの一時的な
/// 失敗は削除済みと区別できないので、黙って落とさずに断る（緩めない・記録を書き換えない）。
#[test]
fn an_unverifiable_account_in_the_record_refuses_the_repair() {
    let mut ops = FakeOps::new();
    ops.names
        .insert("bob".to_string(), Ok(Some(BOB.to_string())));
    tight_profile(&ops, &[ALICE, DAVE]);
    let record = Some(vec![ALICE.to_string(), DAVE.to_string()]);
    *ops.owners.borrow_mut() = Ok(record.clone());
    let (result, lines) = run(&ops, Some("bob"), None);
    assert_eq!(result, Err(Refused), "{lines:#?}");
    assert!(!ops.calls().iter().any(|c| c.starts_with("write_owners")));
    assert!(!ops.calls().iter().any(|c| c.starts_with("relax")));
    assert_eq!(*ops.owners.borrow(), Ok(record));
    assert!(lines.contains(&format!(
        "err banto-hub-elev: 戻す相手の記録（{OWNERS_AT}）の {DAVE} が利用者のアカウントかを確かめられませんでした: RPC が届かない。削除済みかを決められないので、緩めずに止めます（記録も書き換えません）。名前の解決が戻ってから、もう一度実行してください"
    )), "{lines:#?}");
}

/// 修復（記録に名前を解けないドメインの利用者。#704 レビュー P2）: ドメインの SID の
/// `ERROR_NONE_MAPPED`はネットワークのタイムアウトでも返るので、削除済みと決めずに断る
/// （緩めない・記録を書き換えない）。反証: 同じ SID がローカルなら外して進む（上の
/// `a_deleted_account_in_the_record_is_dropped_with_a_warning`）。
#[test]
fn an_unmapped_domain_account_in_the_record_refuses_the_repair() {
    let mut ops = FakeOps::new();
    ops.names
        .insert("bob".to_string(), Ok(Some(BOB.to_string())));
    ops.lookups
        .insert(ALICE.into(), OwnerLookup::Unmapped { local: false });
    tight_profile(&ops, &[ALICE, CAROL]);
    let record = Some(vec![ALICE.to_string(), CAROL.to_string()]);
    *ops.owners.borrow_mut() = Ok(record.clone());
    let (result, lines) = run(&ops, Some("bob"), None);
    assert_eq!(result, Err(Refused), "{lines:#?}");
    assert!(!ops.calls().iter().any(|c| c.starts_with("write_owners")));
    assert!(!ops.calls().iter().any(|c| c.starts_with("relax")));
    assert_eq!(*ops.owners.borrow(), Ok(record));
    assert!(lines.contains(&format!(
        "err banto-hub-elev: 戻す相手の記録（{OWNERS_AT}）の {ALICE} の名前を解けませんでした（この PC のローカルのアカウントではないので、ドメインに届かないだけか削除済みかを決められません）。緩めずに止めます（記録も書き換えません）。ドメインに届くようになってから、もう一度実行してください。その利用者を外すなら、ドメインで削除済みを確かめてから記録（{OWNERS_AT}）を消して実行すると、今の DACL から組を採り直します（名前を解けない利用者には戻しません）"
    )), "{lines:#?}");

    // 記録が無いときも、締まった DACL に名前を解けないドメインの利用者がいれば断る
    // （緩めない・記録を書き換えない）。
    let mut ops = FakeOps::new();
    ops.lookups
        .insert(ALICE.into(), OwnerLookup::Unmapped { local: false });
    tight_profile(&ops, &[ALICE, CAROL]);
    *ops.owners.borrow_mut() = Ok(None);
    let (result, lines) = run(&ops, None, None);
    assert_eq!(result, Err(Refused), "{lines:#?}");
    assert!(!ops.calls().iter().any(|c| c.starts_with("write_owners")));
    assert!(!ops.calls().iter().any(|c| c.starts_with("relax")));
    assert_eq!(*ops.owners.borrow(), Ok(None));
    assert!(lines.contains(&format!(
        "err banto-hub-elev: 今の DACL で「読み取りと実行」を持つ {ALICE} の名前を解けませんでした（この PC のローカルのアカウントではないので、ドメインに届かないだけか削除済みかを決められません）。緩めずに止めます（記録も書き換えません）。ドメインに届くようになってから、もう一度実行してください"
    )), "{lines:#?}");

    // 反証: 同じ SID がローカル（削除済み）なら、記録が無くても戻さずに警告して進む。
    let mut ops = FakeOps::new();
    ops.lookups
        .insert(ALICE.into(), OwnerLookup::Unmapped { local: true });
    tight_profile(&ops, &[ALICE, CAROL]);
    *ops.owners.borrow_mut() = Ok(None);
    let (result, lines) = run(&ops, None, None);
    assert_eq!(result, Ok(()), "{lines:#?}");
    assert!(ops
        .calls()
        .contains(&format!("relax default [{ME},{CAROL}]")));
}

/// 修復（記録が無く、締まった DACL に確かめられない利用者がいる）: 削除済みと区別できないので、
/// 外して進まずに断る（緩めない・記録を書き換えない）。反証: 確かめられれば進む。
#[test]
fn an_unverifiable_account_in_the_dacl_refuses_the_repair_without_a_record() {
    let mut ops = FakeOps::new();
    tight_profile(&ops, &[ALICE, DAVE]);
    *ops.owners.borrow_mut() = Ok(None);
    let (result, lines) = run(&ops, None, None);
    assert_eq!(result, Err(Refused), "{lines:#?}");
    assert!(!ops.calls().iter().any(|c| c.starts_with("write_owners")));
    assert!(!ops.calls().iter().any(|c| c.starts_with("relax")));
    assert_eq!(*ops.owners.borrow(), Ok(None));
    assert!(lines.contains(&format!(
        "err banto-hub-elev: 今の DACL で「読み取りと実行」を持つ {DAVE} が利用者のアカウントかを確かめられませんでした: RPC が届かない。削除済みかを決められないので、緩めずに止めます（記録も書き換えません）。名前の解決が戻ってから、もう一度実行してください"
    )), "{lines:#?}");

    // 反証: 確かめられれば（利用者）、戻して進む。
    ops.lookups.insert(DAVE.into(), OwnerLookup::User);
    let (result, lines) = run(&ops, None, None);
    assert_eq!(result, Ok(()), "{lines:#?}");
    assert!(ops.calls().iter().any(|c| c.starts_with("relax")));
}

/// 修復（記録にグループ。#704 レビュー P2）: 照会に成功して利用者でないと分かった SID は、
/// ローカルかどうかに関わらず警告して外す（従来どおり）。
#[test]
fn a_group_in_the_record_is_dropped_with_a_warning() {
    let mut ops = FakeOps::new();
    ops.names
        .insert("bob".to_string(), Ok(Some(BOB.to_string())));
    tight_profile(&ops, &[CAROL]);
    *ops.owners.borrow_mut() = Ok(Some(vec![DOMAIN_USERS.to_string(), CAROL.to_string()]));
    let (result, lines) = run(&ops, Some("bob"), None);
    assert_eq!(result, Ok(()), "{lines:#?}");
    assert!(ops
        .calls()
        .contains(&format!("relax default [{BOB},{CAROL}]")));
    assert_eq!(
        *ops.owners.borrow(),
        Ok(Some(vec![BOB.to_string(), CAROL.to_string()]))
    );
    assert!(lines.contains(&format!(
        "err banto-hub-elev: 警告: 戻す相手の記録（{OWNERS_AT}）の {DOMAIN_USERS} はこの PC の利用者のアカウントではありません（グループなど）。記録から外し、「変更」は戻しません"
    )), "{lines:#?}");
}

/// ローカルのアカウントのドメインの直下（`<domain>-<RID>`）だけを属するとみなす（#704
/// レビュー P2）。
#[test]
fn is_in_account_domain_matches_only_direct_accounts() {
    let domain = "S-1-5-21-1-2-3";
    assert!(is_in_account_domain("S-1-5-21-1-2-3-1001", domain));
    assert!(is_in_account_domain("s-1-5-21-1-2-3-500", domain));
    // 前方一致だけの別のドメイン・RID が無い・深すぎる・数字でない。
    assert!(!is_in_account_domain("S-1-5-21-1-2-33-1001", domain));
    assert!(!is_in_account_domain("S-1-5-21-1-2-3", domain));
    assert!(!is_in_account_domain("S-1-5-21-1-2-3-", domain));
    assert!(!is_in_account_domain("S-1-5-21-1-2-3-4-1001", domain));
    assert!(!is_in_account_domain("S-1-5-21-1-2-3-x", domain));
    assert!(!is_in_account_domain("S-1-5-21-9-9-9-1001", domain));
    assert!(!is_in_account_domain("S-1-5-21-1-2-3-1001", ""));
}
