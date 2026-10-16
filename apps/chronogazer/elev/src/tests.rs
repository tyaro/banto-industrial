//! `chronogazer_elev` のテスト。**OS に触らない**: 手順は偽の [`ElevOps`] で、引数・
//! 行・計画・ルートの解決は純関数で確かめる（本物のローカルグループ・サービスの
//! DACL・HKLM・ProgramData には触らない。GitHub の Windows のランナーは管理者）。

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use banto_secret_store::acl::PinnedDirInfo;
use banto_secret_store::{DirAccess, InstallUserSid};
use banto_service_control::{
    ConsoleLine, InstallError, InstallKind, InstallOutcome, PreshutdownReport, ServiceManagerError,
    StopAttempt, UninstallError, UninstallOutcome,
};
use banto_service_elev::{ElevError, GroupOutcome, MemberOutcome};
use chronogazer_core::paths::DataLayout;
use chronogazer_core::server_service::{
    InstallReport, InstallUserUnavailable, RootPreparation, SidRecordResult,
};

use super::*;

const USER: &str = "S-1-5-21-1111111111-2222222222-3333333333-1001";
const OTHER: &str = "S-1-5-21-1111111111-2222222222-3333333333-1002";
const ADMIN: &str = "S-1-5-21-1111111111-2222222222-3333333333-500";
const STRANGER: &str = "S-1-5-21-1111111111-2222222222-3333333333-1003";

fn sid(text: &str) -> InstallUserSid {
    InstallUserSid::parse(text).unwrap()
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn stdout(text: &str) -> ConsoleLine {
    ConsoleLine::Stdout(text.to_string())
}

fn stderr(text: &str) -> ConsoleLine {
    ConsoleLine::Stderr(text.to_string())
}

// --- 引数 ---------------------------------------------------------------------------

#[test]
fn every_action_parses_by_its_exact_name() {
    let expected = [
        Action::ServiceInstall {
            install_user_sid: None,
        },
        Action::ServiceUninstall,
        Action::SetupOperators,
        Action::GrantServiceAcl,
        Action::GrantDataAcl,
        Action::AutostartEnable,
        Action::AutostartDisable,
    ];
    for (name, action) in Action::ALL_NAMES.iter().zip(expected) {
        assert_eq!(parse_args(&args(&[name])), Ok(action.clone()), "{name}");
        assert_eq!(action.name(), *name);
    }
}

#[test]
fn service_install_takes_only_one_install_user_sid() {
    assert_eq!(
        parse_args(&args(&["service-install", "--install-user-sid", USER])),
        Ok(Action::ServiceInstall {
            install_user_sid: Some(sid(USER))
        })
    );
    // Entra の利用者の領域も形としては通す（利用者のアカウントかは実行のときに確かめる）。
    assert!(parse_args(&args(&[
        "service-install",
        "--install-user-sid",
        "S-1-12-1-1234567890-1234567890-1234567890-1234567890"
    ]))
    .is_ok());
}

/// 反証: 一覧に無い・大文字小文字が違う・空・余計な引数・重ねた引数・SID の誤りは
/// すべて使い方の誤り。
#[test]
fn everything_else_is_a_usage_error() {
    let cases: &[&[&str]] = &[
        &[],
        &[""],
        &["Service-Install"],
        &["SERVICE-UNINSTALL"],
        &["grant-profile-acl"],
        &["reset-password"],
        &["service-install "],
        &[" setup-operators"],
        &["--install-user-sid", USER],
        &["service-install", "--install-user-sid"],
        &["service-install", USER],
        &["service-install", "--install-user-sid", USER, "extra"],
        &[
            "service-install",
            "--install-user-sid",
            USER,
            "--install-user-sid",
            OTHER,
        ],
        &["service-install", "--INSTALL-USER-SID", USER],
        &["service-install", "--install-user-sid=S-1-5-21-1-2-3-1001"],
        &["service-install", "--install-user-sid", ""],
        &["service-install", "--install-user-sid", "S-1-5-32-544"],
        &["service-install", "--install-user-sid", "S-1-5-18"],
        &["service-install", "--install-user-sid", "S-1-1-0"],
        &[
            "service-install",
            "--install-user-sid",
            "S-1-5-21-1-2)(A;;FA;;;WD",
        ],
        &["service-install", "--install-user-sid", "BUILTIN\\Users"],
        &["service-uninstall", "--force"],
        &["setup-operators", "someone"],
        &["setup-operators", "--install-user-sid", USER],
        &["grant-service-acl", "BantoHub"],
        &["grant-data-acl", "--install-user-sid", USER],
        &["grant-data-acl", "C:\\Users\\Public"],
        &["autostart-enable", "now"],
        &["autostart-disable", ""],
        &["service-install", "service-install"],
    ];
    for case in cases {
        assert!(parse_args(&args(case)).is_err(), "{case:?}");
    }
}

#[test]
fn usage_and_not_windows_lines_are_labelled() {
    let lines = usage_lines("不明なアクションです");
    assert_eq!(
        lines,
        vec![
            stderr("chronogazer-elev: 不明なアクションです"),
            stderr("chronogazer-elev: 使用法: chronogazer-elev.exe <action>"),
            stderr("chronogazer-elev:   action = service-install | service-uninstall | setup-operators | grant-service-acl | grant-data-acl | autostart-enable | autostart-disable"),
            stderr("chronogazer-elev:   service-install だけが --install-user-sid S-1-5-21-… を受けます（省けば実行した利用者）"),
        ]
    );
    assert_eq!(
        not_windows_lines(),
        vec![stderr("chronogazer-elev: このツールは Windows 専用です（UAC の昇格・ローカルグループ・SCM・NTFS の ACL を使います）")]
    );
}

#[test]
fn relabel_swaps_only_the_server_prefix() {
    assert_eq!(
        relabel(vec![
            stdout("chronogazer-server: 登録しました"),
            stderr("chronogazer-server:   warn"),
            stdout("chronogazer-elev: そのまま"),
            stdout("x chronogazer-server: 途中はそのまま"),
        ]),
        vec![
            stdout("chronogazer-elev: 登録しました"),
            stderr("chronogazer-elev:   warn"),
            stdout("chronogazer-elev: そのまま"),
            stdout("x chronogazer-server: 途中はそのまま"),
        ]
    );
}

// --- マニフェスト ---------------------------------------------------------------------

#[test]
fn the_manifest_requires_administrator_and_the_rc_embeds_it() {
    let manifest = include_str!("../chronogazer-elev.manifest");
    assert!(
        manifest.contains(
            r#"<requestedExecutionLevel level="requireAdministrator" uiAccess="false"/>"#
        ),
        "{manifest}"
    );
    assert!(!manifest.contains("asInvoker"));
    assert!(!manifest.contains("highestAvailable"));
    let rc = include_str!("../chronogazer-elev.rc");
    assert!(rc.contains(r#"1 24 "chronogazer-elev.manifest""#), "{rc}");
    let build = include_str!("../build.rs");
    assert!(build.contains(r#"["chronogazer-elev"]"#));
    assert!(build.contains(".manifest_required()"));
}

#[test]
fn the_operators_group_and_service_are_fixed() {
    assert_eq!(OPERATORS_GROUP.name, "ChronoGazer Operators");
    assert_ne!(OPERATORS_GROUP.name, "BantoHub Operators");
    assert_eq!(SERVICE_NAME, "ChronoGazer");
    assert_eq!(SERVER_EXE_FILE_NAME, "chronogazer-server.exe");
}

// --- ルート（マシンの環境） ---------------------------------------------------------

struct FakeEnv {
    root: Result<Option<String>, String>,
    program_data: Result<PathBuf, String>,
    known: Result<KnownFolders, String>,
}

impl MachineEnvironment for FakeEnv {
    fn chronogazer_root(&self) -> Result<Option<String>, String> {
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

#[cfg(windows)]
const PD: &str = r"C:\ProgramData";
#[cfg(not(windows))]
const PD: &str = "/var/lib";
#[cfg(windows)]
const CUSTOM: &str = r"D:\ChronoGazer";
#[cfg(not(windows))]
const CUSTOM: &str = "/srv/ChronoGazer";

#[test]
fn the_root_comes_only_from_the_machine_environment() {
    // 無い・空 → ProgramData + ChronoGazer。
    for root in [Ok(None), Ok(Some(""))] {
        assert_eq!(
            resolve_machine_root(&env(root, Ok(PD))),
            Ok(PathBuf::from(PD).join("ChronoGazer"))
        );
    }
    // 指定があればそれ（ProgramData は読まない）。
    assert_eq!(
        resolve_machine_root(&env(Ok(Some(CUSTOM)), Err("読まないはず"))),
        Ok(PathBuf::from(CUSTOM))
    );
}

/// 反証: 読めない値で ProgramData へ落とさない・展開の要る値・相対パス・`..` を断る・
/// ProgramData が取れなければ止める。
#[test]
fn unusable_machine_roots_are_refused() {
    let refused = [
        env(Err("os error 5"), Ok(PD)),
        env(Ok(Some(r"%SystemDrive%\ChronoGazer")), Ok(PD)),
        env(Ok(Some("%ProgramData%")), Ok(PD)),
        env(Ok(Some("relative\\ChronoGazer")), Ok(PD)),
        env(Ok(Some("ChronoGazer")), Ok(PD)),
        env(Ok(None), Err("E_FAIL")),
        env(Ok(None), Ok("relative")),
    ];
    for case in &refused {
        assert!(
            resolve_machine_root(case).is_err(),
            "{:?} / {:?}",
            case.root,
            case.program_data
        );
    }
    let dotted = format!(
        "{CUSTOM}{}..{}x",
        std::path::MAIN_SEPARATOR,
        std::path::MAIN_SEPARATOR
    );
    assert!(resolve_machine_root(&env(Ok(Some(&dotted)), Ok(PD))).is_err());
    assert!(resolve_machine_root(&env(Err("x"), Ok(PD)))
        .unwrap_err()
        .contains("CHRONOGAZER_ROOT"));
    // 既知のフォルダが取れなければ止める。
    let mut no_known = env(Ok(None), Ok(PD));
    no_known.known = Err("E_FAIL".to_string());
    assert!(resolve_machine_root(&no_known).is_err());
}

/// 監査 B1 の反証: ドライブのルート・UNC・既知のシステムフォルダ（とその祖先・中）を
/// ルートにしない。ProgramData・Users の**下**の専用のフォルダは使える。
#[cfg(windows)]
#[test]
fn well_known_roots_are_refused() {
    for bad in [
        r"C:\",
        r"D:\",
        r"\\srv\share\cg",
        r"\\?\C:\x",
        r"\\.\C:\x",
        r"C:\ProgramData",
        r"c:\programdata\",
        r"C:\Users",
        r"C:\Windows\x",
        r"C:\Windows",
        r"C:\Program Files",
        r"C:\Program Files\ChronoGazer",
        r"C:\Program Files (x86)\cg",
        r"C:\Program Files\Common Files\cg",
        r"C:\Users\Public\cg",
    ] {
        assert!(
            resolve_machine_root(&env(Ok(Some(bad)), Ok(PD))).is_err(),
            "{bad}"
        );
    }
    for good in [
        r"D:\ChronoGazer",
        r"C:\ProgramData\ChronoGazer",
        r"C:\Users\me\cg",
        r"C:\Program Files Evil\cg",
        r"C:\WindowsData\cg",
    ] {
        assert_eq!(
            resolve_machine_root(&env(Ok(Some(good)), Ok(PD))),
            Ok(PathBuf::from(good)),
            "{good}"
        );
    }
}

#[cfg(not(windows))]
#[test]
fn well_known_roots_are_refused() {
    for bad in [
        "/",
        "//srv/share",
        "/var/lib",
        "/var",
        "/users",
        "/win/x",
        "/pf",
        "/pf86/cg",
    ] {
        assert!(
            resolve_machine_root(&env(Ok(Some(bad)), Ok(PD))).is_err(),
            "{bad}"
        );
    }
    for good in [
        "/srv/ChronoGazer",
        "/var/lib/ChronoGazer",
        "/users/me/cg",
        "/pfx/cg",
    ] {
        assert!(
            resolve_machine_root(&env(Ok(Some(good)), Ok(PD))).is_ok(),
            "{good}"
        );
    }
}

/// 監査 B1: ルートを断ったら `grant-data-acl` はフォルダを 1 つも開かない（2）。
#[test]
fn grant_data_acl_does_not_touch_a_refused_root() {
    let mut fake = Fake::new();
    fake.root = resolve_machine_root(&env(Ok(Some(PD)), Ok(PD)));
    assert!(fake.root.is_err());
    let (code, _) = exec(Action::GrantDataAcl, &fake);
    assert_eq!(code, EXIT_PRECONDITION);
    assert!(!fake
        .calls()
        .iter()
        .any(|c| c.starts_with("pin") || c.starts_with("create")));
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
}

// --- 偽の操作 ---------------------------------------------------------------------

type Log = Rc<RefCell<Vec<String>>>;

struct FakePinned {
    path: PathBuf,
    info: PinnedDirInfo,
    reset: Result<(), String>,
    log: Log,
}

impl PinnedDirOps for FakePinned {
    fn info(&self) -> PinnedDirInfo {
        self.info.clone()
    }
    fn reset_acl(&self, access: &DirAccess) -> Result<(), String> {
        self.log.borrow_mut().push(format!(
            "reset {} {}",
            self.path.display(),
            access.install_user().map_or("-", InstallUserSid::as_str)
        ));
        self.reset.clone()
    }
}

struct Fake {
    log: Log,
    server_exe: Result<PathBuf, String>,
    program_files: Vec<PathBuf>,
    /// 登録されているコマンドライン（`registered_command_line`）。
    registered: Result<Option<String>, String>,
    root: Result<PathBuf, String>,
    me: Result<InstallUserSid, String>,
    not_accounts: Vec<String>,
    install: Result<InstallReport, InstallUserUnavailable>,
    uninstall: Result<UninstallOutcome, UninstallError>,
    recorded: Result<Option<String>, String>,
    group: Result<GroupOutcome, ElevError>,
    member: Result<MemberOutcome, ElevError>,
    service_acl: Result<(), ElevError>,
    autostart: Result<(), ServiceManagerError>,
    /// 今あるフォルダ（`pin_dir` が返す）。
    dirs: RefCell<BTreeMap<PathBuf, PinnedDirInfo>>,
    /// `create_dir` が作るフォルダの所有者（`None` なら作ったと言うが置かない＝消された）。
    created_owner: Option<String>,
    create_error: Option<PathBuf>,
    reset_error: Option<PathBuf>,
    pin_error: Option<PathBuf>,
}

fn root_path() -> PathBuf {
    PathBuf::from(PD).join("ChronoGazer")
}

#[cfg(windows)]
const PF: &str = r"C:\Program Files";
#[cfg(not(windows))]
const PF: &str = "/pf";
#[cfg(windows)]
const PF86: &str = r"C:\Program Files (x86)";
#[cfg(not(windows))]
const PF86: &str = "/pf86";

fn exe_path() -> PathBuf {
    PathBuf::from(PF)
        .join("ChronoGazer")
        .join("chronogazer-server.exe")
}

fn registered_line(exe: &Path) -> String {
    format!("\"{}\" run-service", exe.display())
}

fn ok_report() -> InstallReport {
    InstallReport {
        executable: exe_path(),
        program_files: None,
        outcome: Ok(InstallOutcome {
            kind: InstallKind::Created,
            description_error: None,
            preshutdown: PreshutdownReport::Applied("PRESHUTDOWN の行".to_string()),
        }),
        sid: Some(SidRecordResult::Written {
            sid: USER.to_string(),
            replaced: None,
        }),
        root: Some(RootPreparation::Created(root_path())),
    }
}

impl Fake {
    fn new() -> Self {
        Self {
            log: Rc::new(RefCell::new(Vec::new())),
            server_exe: Ok(exe_path()),
            program_files: vec![PathBuf::from(PF), PathBuf::from(PF86)],
            registered: Ok(Some(registered_line(&exe_path()))),
            root: Ok(root_path()),
            me: Ok(sid(USER)),
            not_accounts: Vec::new(),
            install: Ok(ok_report()),
            uninstall: Ok(UninstallOutcome {
                stop: StopAttempt::NotRunning,
            }),
            recorded: Ok(Some(USER.to_string())),
            group: Ok(GroupOutcome::Created),
            member: Ok(MemberOutcome::Added),
            service_acl: Ok(()),
            autostart: Ok(()),
            dirs: RefCell::new(BTreeMap::new()),
            created_owner: Some("S-1-5-32-544".to_string()),
            create_error: None,
            reset_error: None,
            pin_error: None,
        }
    }

    fn with_dir(self, dir: PathBuf, is_dir: bool, reparse: bool, owner: &str) -> Self {
        self.dirs.borrow_mut().insert(
            dir,
            PinnedDirInfo {
                is_dir,
                is_reparse_point: reparse,
                owner: owner.to_string(),
            },
        );
        self
    }

    fn calls(&self) -> Vec<String> {
        self.log.borrow().clone()
    }

    fn record(&self, call: String) {
        self.log.borrow_mut().push(call);
    }
}

impl ElevOps for Fake {
    type Pinned = FakePinned;

    fn server_executable(&self) -> Result<PathBuf, String> {
        self.record("server_executable".into());
        self.server_exe.clone()
    }
    fn program_files_dirs(&self) -> Vec<PathBuf> {
        self.program_files.clone()
    }
    fn registered_command_line(&self) -> Result<Option<String>, String> {
        self.record("registered".into());
        self.registered.clone()
    }
    fn machine_root(&self) -> Result<PathBuf, String> {
        self.record("machine_root".into());
        self.root.clone()
    }
    fn current_user_sid(&self) -> Result<InstallUserSid, String> {
        self.me.clone()
    }
    fn check_user_account(&self, sid: &InstallUserSid) -> Result<(), String> {
        self.record(format!("check {}", sid.as_str()));
        if self.not_accounts.iter().any(|s| s == sid.as_str()) {
            Err("利用者のアカウントではありません（グループなど）".to_string())
        } else {
            Ok(())
        }
    }
    fn install(
        &self,
        executable: PathBuf,
        sid: InstallUserSid,
        layout: &DataLayout,
    ) -> Result<InstallReport, InstallUserUnavailable> {
        self.record(format!(
            "install {} {} {}",
            executable.display(),
            sid.as_str(),
            layout.root().display()
        ));
        self.install.clone()
    }
    fn uninstall(&self) -> Result<UninstallOutcome, UninstallError> {
        self.record("uninstall".into());
        self.uninstall.clone()
    }
    fn read_install_user(&self) -> Result<Option<String>, String> {
        self.record("read_install_user".into());
        self.recorded.clone()
    }
    fn ensure_group(&self) -> Result<GroupOutcome, ElevError> {
        self.record("ensure_group".into());
        self.group.clone()
    }
    fn add_member(&self, sid: &InstallUserSid) -> Result<MemberOutcome, ElevError> {
        self.record(format!("add_member {}", sid.as_str()));
        self.member.clone()
    }
    fn grant_service_acl(&self) -> Result<(), ElevError> {
        self.record("grant_service_acl".into());
        self.service_acl.clone()
    }
    fn set_autostart(&self, executable: &Path, enabled: bool) -> Result<(), ServiceManagerError> {
        self.record(format!("autostart {} {enabled}", executable.display()));
        self.autostart.clone()
    }
    fn pin_dir(&self, dir: &Path) -> Result<Option<Self::Pinned>, String> {
        self.record(format!("pin {}", dir.display()));
        if self.pin_error.as_deref() == Some(dir) {
            return Err("os error 32".to_string());
        }
        let shared = self.dirs.borrow().get(dir).cloned();
        Ok(shared.map(|info| FakePinned {
            path: dir.to_path_buf(),
            info,
            reset: if self.reset_error.as_deref() == Some(dir) {
                Err("os error 5".to_string())
            } else {
                Ok(())
            },
            log: self.log.clone(),
        }))
    }
    fn create_dir(&self, dir: &Path, access: &DirAccess) -> Result<bool, String> {
        self.record(format!(
            "create {} {}",
            dir.display(),
            access.install_user().map_or("-", InstallUserSid::as_str)
        ));
        if self.create_error.as_deref() == Some(dir) {
            return Err("os error 5".to_string());
        }
        if let Some(owner) = &self.created_owner {
            self.dirs.borrow_mut().insert(
                dir.to_path_buf(),
                PinnedDirInfo {
                    is_dir: true,
                    is_reparse_point: false,
                    owner: owner.clone(),
                },
            );
        }
        Ok(true)
    }
}

/// 実行して、終了コード・出した行・操作の記録（reset はフォルダごとの記録から）を返す。
fn exec(action: Action, fake: &Fake) -> (i32, Vec<ConsoleLine>) {
    let mut lines = Vec::new();
    let code = run(&action, fake, &mut |line| lines.push(line));
    (code, lines)
}

/// 書き直した（`reset_acl`）フォルダ。
fn resets(fake: &Fake) -> Vec<String> {
    fake.calls()
        .into_iter()
        .filter(|call| call.starts_with("reset "))
        .collect()
}

fn secrets() -> PathBuf {
    root_path().join("secrets")
}

fn hub_keys() -> PathBuf {
    secrets().join("hub-keys")
}

// --- service-install ---------------------------------------------------------------

#[test]
fn service_install_runs_every_step_in_order() {
    let fake = Fake::new();
    let (code, lines) = exec(
        Action::ServiceInstall {
            install_user_sid: None,
        },
        &fake,
    );
    assert_eq!(code, EXIT_OK, "{lines:#?}");
    let root = root_path();
    assert_eq!(
        fake.calls(),
        vec![
            "server_executable".to_string(),
            "machine_root".to_string(),
            format!("check {USER}"),
            // 登録の前にルートを確かめる（監査 B4。まだ無い）。
            format!("pin {}", root.display()),
            format!("install {} {USER} {}", exe_path().display(), root.display()),
            // setup-operators（記録を読み直す）
            "read_install_user".to_string(),
            format!("check {USER}"),
            "ensure_group".to_string(),
            format!("add_member {USER}"),
            // grant-service-acl
            "grant_service_acl".to_string(),
            // grant-data-acl
            "read_install_user".to_string(),
            format!("check {USER}"),
            "machine_root".to_string(),
            format!("pin {}", root.display()),
            format!("create {} {USER}", root.display()),
            format!("pin {}", root.display()),
            // 作った直後でも開いて確かめ、書き直す。
            format!("reset {} {USER}", root.display()),
            format!("pin {}", secrets().display()),
            format!("create {} {USER}", secrets().display()),
            format!("pin {}", secrets().display()),
            format!("reset {} {USER}", secrets().display()),
            format!("pin {}", hub_keys().display()),
        ]
    );
    // install_with の行は頭を付け替えて出す。
    assert!(lines.contains(&stdout(
        "chronogazer-elev: Windows サービス 'ChronoGazer' を登録しました"
    )));
    assert!(lines
        .iter()
        .all(|line| matches!(line, ConsoleLine::Stdout(t) | ConsoleLine::Stderr(t) if t.starts_with("chronogazer-elev: "))),
        "{lines:#?}");
    assert_eq!(
        lines.last(),
        Some(&stdout(
            "chronogazer-elev: 'service-install' が完了しました"
        ))
    );
}

#[test]
fn service_install_uses_the_given_sid_after_checking_it_is_an_account() {
    let fake = Fake::new();
    let (code, _) = exec(
        Action::ServiceInstall {
            install_user_sid: Some(sid(OTHER)),
        },
        &fake,
    );
    assert_eq!(code, EXIT_OK);
    let calls = fake.calls();
    assert_eq!(calls[2], format!("check {OTHER}"));
    assert!(calls[4].starts_with(&format!("install {} {OTHER} ", exe_path().display())));

    // 反証: 利用者のアカウントでない SID（グループなど）なら何も登録しない（64）。
    let mut fake = Fake::new();
    fake.not_accounts = vec![OTHER.to_string()];
    let (code, lines) = exec(
        Action::ServiceInstall {
            install_user_sid: Some(sid(OTHER)),
        },
        &fake,
    );
    assert_eq!(code, EXIT_USAGE);
    assert!(!fake.calls().iter().any(|c| c.starts_with("install")));
    assert!(lines
        .iter()
        .any(|l| matches!(l, ConsoleLine::Stderr(t) if t.contains("何も登録していません"))));
}

/// 反証: 最初の失敗で止まる（後の手順を呼ばない）。終了コードはその手順のもの。
#[test]
fn service_install_stops_at_the_first_failure() {
    let first_calls = |fake: &Fake| fake.calls();

    // 隣に chronogazer-server.exe が無い → 2、何もしない。
    let mut fake = Fake::new();
    fake.server_exe = Err("無い".to_string());
    let install = Action::ServiceInstall {
        install_user_sid: None,
    };
    assert_eq!(exec(install.clone(), &fake).0, EXIT_PRECONDITION);
    assert_eq!(first_calls(&fake), vec!["server_executable"]);

    // ルートを決められない → 2、登録しない。
    let mut fake = Fake::new();
    fake.root = Err("相対パス".to_string());
    assert_eq!(exec(install.clone(), &fake).0, EXIT_PRECONDITION);
    assert!(!fake.calls().iter().any(|c| c.starts_with("install")));

    // 監査 B4: 既にあるルートが見知らぬ所有者・reparse point なら、何も登録せずに 1。
    for (reparse, owner) in [(false, STRANGER), (true, "S-1-5-32-544")] {
        let fake = Fake::new().with_dir(root_path(), true, reparse, owner);
        let (code, lines) = exec(install.clone(), &fake);
        assert_eq!(code, EXIT_FAILED, "{lines:#?}");
        assert!(!fake
            .calls()
            .iter()
            .any(|c| c.starts_with("install") || c == "ensure_group" || c == "grant_service_acl"));
        assert!(resets(&fake).is_empty());
        assert!(lines.iter().any(
            |l| matches!(l, ConsoleLine::Stderr(t) if t.contains("（何も登録していません）"))
        ));
    }
    // 信頼できる所有者の既存のルートなら続ける。
    let fake = Fake::new().with_dir(root_path(), true, false, "S-1-5-32-544");
    assert_eq!(exec(install.clone(), &fake).0, EXIT_OK);

    // 実行した利用者を決められない → 2。
    let mut fake = Fake::new();
    fake.me = Err("LocalSystem".to_string());
    assert_eq!(exec(install.clone(), &fake).0, EXIT_PRECONDITION);
    assert!(!fake.calls().iter().any(|c| c.starts_with("install")));

    // 登録に失敗 → 1、Operators 以降に進まない。
    let mut fake = Fake::new();
    let mut report = ok_report();
    report.outcome = Err(InstallError::Create("os error 1073".to_string()));
    report.sid = None;
    report.root = None;
    fake.install = Ok(report);
    assert_eq!(exec(install.clone(), &fake).0, EXIT_FAILED);
    assert!(!fake.calls().contains(&"ensure_group".to_string()));

    // 記録に失敗 → 1。
    let mut fake = Fake::new();
    let mut report = ok_report();
    report.sid = Some(SidRecordResult::WriteFailed {
        sid: USER.to_string(),
        error: "拒否".to_string(),
    });
    report.root = None;
    fake.install = Ok(report);
    assert_eq!(exec(install.clone(), &fake).0, EXIT_FAILED);
    assert!(!fake.calls().contains(&"ensure_group".to_string()));

    // グループを作れない → 1、サービスの ACL・データの ACL に進まない。
    let mut fake = Fake::new();
    fake.group = Err(ElevError::CreateGroup {
        group: "ChronoGazer Operators".to_string(),
        status: 5,
    });
    assert_eq!(exec(install.clone(), &fake).0, EXIT_FAILED);
    assert!(!fake.calls().contains(&format!("add_member {USER}")));
    assert!(!fake.calls().contains(&"grant_service_acl".to_string()));

    // メンバーに足せない → 1。
    let mut fake = Fake::new();
    fake.member = Err(ElevError::AddMember {
        member: USER.to_string(),
        group: "ChronoGazer Operators".to_string(),
        status: 2221,
    });
    assert_eq!(exec(install.clone(), &fake).0, EXIT_FAILED);
    assert!(!fake.calls().contains(&"grant_service_acl".to_string()));

    // サービスの ACL に失敗 → 1、データの ACL に進まない。
    let mut fake = Fake::new();
    fake.service_acl = Err(ElevError::SetServiceSecurity {
        service: "ChronoGazer".to_string(),
        os_error: 5,
    });
    assert_eq!(exec(install.clone(), &fake).0, EXIT_FAILED);
    // 登録の前の確かめ（監査 B4）の 1 回だけ開き、データの ACL には進まない。
    assert_eq!(
        fake.calls().iter().filter(|c| c.starts_with("pin")).count(),
        1
    );
    assert!(!fake.calls().iter().any(|c| c.starts_with("create")));

    // データの ACL を断った → 1（最後の行は「完了しませんでした」）。
    let fake = Fake::new().with_dir(root_path(), true, true, "S-1-5-32-544");
    let (code, lines) = exec(install, &fake);
    assert_eq!(code, EXIT_FAILED);
    assert_eq!(
        lines.last(),
        Some(&stderr(
            "chronogazer-elev: 'service-install' は完了しませんでした（終了コード 1）"
        ))
    );
}

/// D2: 違う SID が記録済みなら、後の手順は**記録の SID**を使う（install の行が警告する）。
#[test]
fn later_steps_use_the_recorded_sid_not_the_requested_one() {
    let mut fake = Fake::new();
    let mut report = ok_report();
    report.sid = Some(SidRecordResult::KeptDifferent {
        recorded: OTHER.to_string(),
        requested: USER.to_string(),
    });
    fake.install = Ok(report);
    fake.recorded = Ok(Some(OTHER.to_string()));
    let (code, _) = exec(
        Action::ServiceInstall {
            install_user_sid: None,
        },
        &fake,
    );
    assert_eq!(code, EXIT_OK);
    assert!(fake.calls().contains(&format!("add_member {OTHER}")));
    assert!(fake
        .calls()
        .contains(&format!("create {} {OTHER}", root_path().display())));
}

// --- 単独のアクション ---------------------------------------------------------------

#[test]
fn actions_other_than_install_need_the_record() {
    for action in [Action::SetupOperators, Action::GrantDataAcl] {
        let mut fake = Fake::new();
        fake.recorded = Ok(None);
        let (code, lines) = exec(action.clone(), &fake);
        assert_eq!(code, EXIT_PRECONDITION, "{action:?}");
        assert!(lines.contains(&stderr(
            r"chronogazer-elev: インストール利用者の記録（HKLM\SYSTEM\CurrentControlSet\Services\ChronoGazer\Parameters\InstallUserSid）がありません。先に service-install を実行してください"
        )));
        assert_eq!(fake.calls(), vec!["read_install_user"], "{action:?}");

        // 不正な記録・利用者のアカウントでない記録も 2、読めなければ 1。
        let mut fake = Fake::new();
        fake.recorded = Ok(Some("S-1-5-32-545".to_string()));
        assert_eq!(exec(action.clone(), &fake).0, EXIT_PRECONDITION);
        let mut fake = Fake::new();
        fake.not_accounts = vec![USER.to_string()];
        assert_eq!(exec(action.clone(), &fake).0, EXIT_PRECONDITION);
        assert!(!fake.calls().contains(&"ensure_group".to_string()));
        assert!(!fake.calls().iter().any(|c| c.starts_with("pin")));
        let mut fake = Fake::new();
        fake.recorded = Err("os error 5".to_string());
        assert_eq!(exec(action, &fake).0, EXIT_FAILED);
    }
}

#[test]
fn setup_operators_golden() {
    let fake = Fake::new();
    let (code, lines) = exec(Action::SetupOperators, &fake);
    assert_eq!(code, EXIT_OK);
    assert_eq!(
        lines,
        vec![
            stdout("chronogazer-elev: ローカルグループ 'ChronoGazer Operators' を作りました"),
            stdout("chronogazer-elev: インストール利用者（S-1-5-21-1111111111-2222222222-3333333333-1001）を 'ChronoGazer Operators' に足しました（反映は次のサインインから）"),
            stdout("chronogazer-elev: 'setup-operators' が完了しました"),
        ]
    );
    let mut fake = Fake::new();
    fake.group = Ok(GroupOutcome::Existed);
    fake.member = Ok(MemberOutcome::AlreadyMember);
    let (code, lines) = exec(Action::SetupOperators, &fake);
    assert_eq!(code, EXIT_OK);
    assert_eq!(
        lines[..2],
        [
            stdout("chronogazer-elev: ローカルグループ 'ChronoGazer Operators' は既にあります"),
            stdout("chronogazer-elev: インストール利用者（S-1-5-21-1111111111-2222222222-3333333333-1001）は既に 'ChronoGazer Operators' のメンバーです"),
        ]
    );
}

#[test]
fn grant_service_acl_exit_codes() {
    let fake = Fake::new();
    let (code, lines) = exec(Action::GrantServiceAcl, &fake);
    assert_eq!(code, EXIT_OK);
    assert_eq!(
        lines[0],
        stdout("chronogazer-elev: サービス 'ChronoGazer' の DACL に 'ChronoGazer Operators' の ACE（CCLCRPWP: 照会・開始・停止）を足しました")
    );
    // 記録は読まない（SID を使わない）。
    assert_eq!(fake.calls(), vec!["grant_service_acl"]);

    for (error, code) in [
        (
            ElevError::GroupNotFound("ChronoGazer Operators".to_string()),
            EXIT_PRECONDITION,
        ),
        (
            ElevError::OpenService {
                service: "ChronoGazer".to_string(),
                os_error: 1060,
            },
            EXIT_PRECONDITION,
        ),
        (
            ElevError::OpenService {
                service: "ChronoGazer".to_string(),
                os_error: 5,
            },
            EXIT_FAILED,
        ),
        (ElevError::OpenScm(5), EXIT_FAILED),
    ] {
        let mut fake = Fake::new();
        fake.service_acl = Err(error.clone());
        assert_eq!(exec(Action::GrantServiceAcl, &fake).0, code, "{error:?}");
    }
}

#[test]
fn autostart_toggles_with_the_sibling_server_exe() {
    for (action, enabled, text) in [
        (
            Action::AutostartEnable,
            true,
            "chronogazer-elev: サービス 'ChronoGazer' を自動開始（遅延）にしました（OS の起動の後に始まります）",
        ),
        (
            Action::AutostartDisable,
            false,
            "chronogazer-elev: サービス 'ChronoGazer' を手動開始に戻しました（OS の再起動だけでは始まりません）",
        ),
    ] {
        let fake = Fake::new();
        let (code, lines) = exec(action.clone(), &fake);
        assert_eq!(code, EXIT_OK);
        assert_eq!(lines[0], stdout(text));
        assert_eq!(
            fake.calls(),
            vec![
                "server_executable".to_string(),
                "registered".to_string(),
                format!("autostart {} {enabled}", exe_path().display())
            ]
        );
        // 監査 B3: 登録の exe が違えば変えずに 1、未登録なら 2、読めなければ 1。
        for (registered, code) in [
            (
                Ok(Some(registered_line(Path::new(r"C:\Users\me\x\chronogazer-server.exe")))),
                EXIT_FAILED,
            ),
            (Ok(Some(format!("{} --other", registered_line(&exe_path())))), EXIT_FAILED),
            (Ok(None), EXIT_PRECONDITION),
            (Err("os error 5".to_string()), EXIT_FAILED),
        ] {
            let mut fake = Fake::new();
            fake.registered = registered.clone();
            assert_eq!(exec(action.clone(), &fake).0, code, "{registered:?}");
            assert!(!fake.calls().iter().any(|c| c.starts_with("autostart")), "{registered:?}");
        }
        let mut fake = Fake::new();
        fake.autostart = Err(ServiceManagerError::NotFound("ChronoGazer".to_string()));
        assert_eq!(exec(action.clone(), &fake).0, EXIT_PRECONDITION);
        let mut fake = Fake::new();
        fake.autostart = Err(ServiceManagerError::AccessDenied);
        assert_eq!(exec(action.clone(), &fake).0, EXIT_FAILED);
        let mut fake = Fake::new();
        fake.server_exe = Err("無い".to_string());
        assert_eq!(exec(action.clone(), &fake).0, EXIT_PRECONDITION);
        assert_eq!(fake.calls(), vec!["server_executable"]);
    }
}

/// 監査 B2: Program Files（x86 を含む）の外の `chronogazer-server.exe` では、何も登録・
/// 変更せずに 2。名前が前方一致するだけの `Program Files Evil` は外、Program Files の
/// 場所が分からなければ止める。
#[test]
fn a_server_exe_outside_program_files_is_refused() {
    let install = Action::ServiceInstall {
        install_user_sid: None,
    };
    let evil = format!("{PF} Evil");
    for (exe, dirs) in [
        (
            PathBuf::from(PD).join("me").join("chronogazer-server.exe"),
            vec![PathBuf::from(PF), PathBuf::from(PF86)],
        ),
        (
            PathBuf::from(&evil).join("chronogazer-server.exe"),
            vec![PathBuf::from(PF), PathBuf::from(PF86)],
        ),
        // Program Files そのもの（中ではない）。
        (PathBuf::from(PF), vec![PathBuf::from(PF)]),
        (exe_path(), Vec::new()),
    ] {
        for action in [install.clone(), Action::AutostartEnable] {
            let mut fake = Fake::new();
            fake.server_exe = Ok(exe.clone());
            fake.program_files = dirs.clone();
            let (code, lines) = exec(action.clone(), &fake);
            assert_eq!(code, EXIT_PRECONDITION, "{exe:?} {dirs:?} {lines:#?}");
            assert_eq!(fake.calls(), vec!["server_executable"], "{exe:?}");
        }
    }
    let mut fake = Fake::new();
    fake.server_exe = Ok(PathBuf::from(&evil).join("chronogazer-server.exe"));
    let (_, lines) = exec(Action::AutostartEnable, &fake);
    assert!(
        matches!(&lines[0], ConsoleLine::Stderr(t) if t.contains("は Program Files の外にあります"))
    );

    // Program Files (x86) の下は通る。
    let mut fake = Fake::new();
    let x86 = PathBuf::from(PF86)
        .join("ChronoGazer")
        .join("chronogazer-server.exe");
    fake.server_exe = Ok(x86.clone());
    fake.registered = Ok(Some(registered_line(&x86)));
    assert_eq!(exec(Action::AutostartEnable, &fake).0, EXIT_OK);
    assert!(fake
        .calls()
        .contains(&format!("autostart {} true", x86.display())));
}

/// 監査 B3: 登録のコマンドラインの比べ方（引用符・空白・大文字小文字は同じとみなす）。
#[test]
fn registered_command_lines_are_compared_as_quoted_path_and_run_service() {
    let exe = exe_path();
    let text = exe.display().to_string();
    for same in [
        format!("\"{text}\" run-service"),
        format!("  \"{text}\"   run-service  "),
        format!("\"{}\" run-service", text.to_uppercase()),
    ] {
        assert!(registered_command_matches(&same, &exe), "{same}");
    }
    if !text.contains(' ') {
        assert!(registered_command_matches(
            &format!("{text} run-service"),
            &exe
        ));
    }
    for other in [
        format!("\"{text}\""),
        format!("\"{text}\" run-service x"),
        format!("\"{text}x\" run-service"),
        format!("\"{text}"),
        String::new(),
    ] {
        assert!(!registered_command_matches(&other, &exe), "{other}");
    }
}

#[test]
fn service_uninstall_keeps_the_group_and_the_data() {
    let fake = Fake::new();
    let (code, lines) = exec(Action::ServiceUninstall, &fake);
    assert_eq!(code, EXIT_OK);
    assert_eq!(
        lines,
        vec![
            stdout("chronogazer-elev: Windows サービス 'ChronoGazer' の登録を解除しました（インストール利用者の記録も一緒に消えます）"),
            stdout("chronogazer-elev: （サービスが止まり終わる・このプロセスのハンドルが閉じるまで、完全な削除が遅れることがあります - Windows API の仕様）"),
            stdout(&format!("chronogazer-elev: データ（{}）は残します", root_path().display())),
            stdout("chronogazer-elev: ローカルグループ 'ChronoGazer Operators' は残します（要らなければ lusrmgr.msc で消してください）"),
            stdout("chronogazer-elev: 'service-uninstall' が完了しました"),
        ]
    );
    // データ・グループには触らない。
    assert_eq!(fake.calls(), vec!["uninstall", "machine_root"]);

    let mut fake = Fake::new();
    fake.uninstall = Err(UninstallError::Open("os error 1060".to_string()));
    assert_eq!(exec(Action::ServiceUninstall, &fake).0, EXIT_PRECONDITION);
    let mut fake = Fake::new();
    fake.uninstall = Err(UninstallError::Delete {
        stop: StopAttempt::NotRunning,
        error: "os error 1072".to_string(),
    });
    assert_eq!(exec(Action::ServiceUninstall, &fake).0, EXIT_FAILED);
    // ルートを決められなくても解除はする。
    let mut fake = Fake::new();
    fake.root = Err("x".to_string());
    assert_eq!(exec(Action::ServiceUninstall, &fake).0, EXIT_OK);
}

// --- grant-data-acl ---------------------------------------------------------------

#[test]
fn grant_data_acl_resets_trusted_existing_dirs() {
    let fake = Fake::new()
        .with_dir(root_path(), true, false, "S-1-5-32-544")
        .with_dir(secrets(), true, false, "S-1-5-18")
        .with_dir(hub_keys(), true, false, USER);
    let (code, lines) = exec(Action::GrantDataAcl, &fake);
    assert_eq!(code, EXIT_OK, "{lines:#?}");
    assert_eq!(
        resets(&fake),
        vec![
            format!("reset {} {USER}", root_path().display()),
            format!("reset {} {USER}", secrets().display()),
            format!("reset {} {USER}", hub_keys().display()),
        ]
    );
    assert!(!fake.calls().iter().any(|c| c.starts_with("create")));
    assert_eq!(
        lines[2],
        stdout(&format!(
            r"chronogazer-elev: Hub の API キーの保存先（secrets\hub-keys）: {}（ACL を書き直しました。SYSTEM・Administrators にフル、インストール利用者 {USER} に変更、親から継承しない）",
            hub_keys().display()
        ))
    );
}

#[test]
fn grant_data_acl_creates_root_and_secrets_but_not_hub_keys() {
    let fake = Fake::new();
    let (code, lines) = exec(Action::GrantDataAcl, &fake);
    assert_eq!(code, EXIT_OK, "{lines:#?}");
    assert_eq!(
        lines,
        vec![
            stdout(&format!("chronogazer-elev: データのルート: {}（作りました。SYSTEM・Administrators にフル、インストール利用者 {USER} に変更、親から継承しない）", root_path().display())),
            stdout(&format!("chronogazer-elev: 秘密の置き場（secrets）: {}（作りました。SYSTEM・Administrators にフル、インストール利用者 {USER} に変更、親から継承しない）", secrets().display())),
            stdout(&format!(r"chronogazer-elev: Hub の API キーの保存先（secrets\hub-keys）（{}）はまだありません（エンジンが初めて使うときに作ります）", hub_keys().display())),
            stdout("chronogazer-elev: 'grant-data-acl' が完了しました"),
        ]
    );
    assert!(!fake
        .calls()
        .contains(&format!("create {} {USER}", hub_keys().display())));
}

/// 反証（O2）: ルートがジャンクションなら、書き直さず・下も見ずに 1。
#[test]
fn grant_data_acl_refuses_a_reparse_point_root() {
    let fake = Fake::new()
        .with_dir(root_path(), true, true, "S-1-5-32-544")
        .with_dir(secrets(), true, false, "S-1-5-18");
    let (code, lines) = exec(Action::GrantDataAcl, &fake);
    assert_eq!(code, EXIT_FAILED);
    assert!(resets(&fake).is_empty());
    assert_eq!(
        lines[0],
        stderr(&format!("chronogazer-elev: データのルート（{}）はジャンクションかシンボリックリンク（reparse point）です。リンクの先を書き換えないよう、書き直さずに止めます（リンクを外して、普通のフォルダにしてからもう一度実行してください）", root_path().display()))
    );
    assert!(!fake
        .calls()
        .contains(&format!("pin {}", secrets().display())));
    assert!(!fake.calls().iter().any(|c| c.starts_with("create")));
}

/// 反証（O2）: 所有者が許す範囲の外（ProgramData に一般の利用者が先に作った）なら 1。
#[test]
fn grant_data_acl_refuses_a_root_owned_by_someone_else() {
    let fake = Fake::new().with_dir(root_path(), true, false, STRANGER);
    let (code, lines) = exec(Action::GrantDataAcl, &fake);
    assert_eq!(code, EXIT_FAILED);
    assert!(resets(&fake).is_empty());
    assert_eq!(
        lines[0],
        stderr(&format!("chronogazer-elev: データのルート（{}）は所有者（{STRANGER}）が SYSTEM・Administrators・インストール利用者のどれでもありません。乗っ取りの恐れがあるため、書き直さずに止めます（中身と所有者を確かめ、必要なら管理者が所有権を取り直してからもう一度実行してください）", root_path().display()))
    );
}

/// 反証: `secrets\` がジャンクション・ファイルなら、ルートは直しても `secrets\` から先は
/// 止める（hub-keys は見ない）。
#[test]
fn grant_data_acl_refuses_a_reparse_point_secrets_and_stops() {
    for (is_dir, reparse) in [(true, true), (false, false)] {
        let fake = Fake::new()
            .with_dir(root_path(), true, false, "S-1-5-32-544")
            .with_dir(secrets(), is_dir, reparse, USER)
            .with_dir(hub_keys(), true, false, USER);
        let (code, lines) = exec(Action::GrantDataAcl, &fake);
        assert_eq!(code, EXIT_FAILED);
        assert_eq!(
            resets(&fake),
            vec![format!("reset {} {USER}", root_path().display())],
            "{lines:#?}"
        );
        assert!(!fake
            .calls()
            .contains(&format!("pin {}", hub_keys().display())));
    }
}

/// 作った直後に、誰かが所有者の違うフォルダにすり替えていたら断る（開いて確かめ直す）。
#[test]
fn a_dir_swapped_right_after_creation_is_refused() {
    let mut fake = Fake::new();
    fake.created_owner = Some(STRANGER.to_string());
    let (code, _) = exec(Action::GrantDataAcl, &fake);
    assert_eq!(code, EXIT_FAILED);
    assert!(resets(&fake).is_empty());

    // 作ったと言うのに開けない（消された）ときも、作り直さずに止める。
    let mut fake = Fake::new();
    fake.created_owner = None;
    let (code, lines) = exec(Action::GrantDataAcl, &fake);
    assert_eq!(code, EXIT_FAILED);
    assert_eq!(
        fake.calls()
            .iter()
            .filter(|call| call.starts_with("create "))
            .count(),
        1
    );
    assert!(lines.iter().any(
        |l| matches!(l, ConsoleLine::Stderr(t) if t.contains("を作った後に開けませんでした"))
    ));
}

#[test]
fn grant_data_acl_failures_are_1_and_stop() {
    let mut fake = Fake::new();
    fake.create_error = Some(root_path());
    assert_eq!(exec(Action::GrantDataAcl, &fake).0, EXIT_FAILED);
    assert!(!fake
        .calls()
        .contains(&format!("pin {}", secrets().display())));

    let mut fake = Fake::new().with_dir(root_path(), true, false, "S-1-5-18");
    fake.reset_error = Some(root_path());
    let (code, lines) = exec(Action::GrantDataAcl, &fake);
    assert_eq!(code, EXIT_FAILED);
    assert!(lines.iter().any(|l| matches!(l, ConsoleLine::Stderr(t) if t.contains("の ACL を書き直せませんでした: os error 5"))));
    assert!(!fake
        .calls()
        .contains(&format!("pin {}", secrets().display())));

    let mut fake = Fake::new();
    fake.pin_error = Some(root_path());
    assert_eq!(exec(Action::GrantDataAcl, &fake).0, EXIT_FAILED);

    let mut fake = Fake::new();
    fake.root = Err("相対パス".to_string());
    assert_eq!(exec(Action::GrantDataAcl, &fake).0, EXIT_PRECONDITION);
    assert!(!fake.calls().iter().any(|c| c.starts_with("pin")));
}

/// この昇格した利用者（管理者）が作ったフォルダ（「作成者を所有者に」の方針の PC）は
/// 書き直してよい。
#[test]
fn a_dir_owned_by_the_elevated_admin_is_accepted() {
    let mut fake = Fake::new().with_dir(root_path(), true, false, ADMIN);
    fake.me = Ok(sid(ADMIN));
    let (code, lines) = exec(Action::GrantDataAcl, &fake);
    assert_eq!(code, EXIT_OK, "{lines:#?}");
}
