//! 偽物の [`SinkConfigOps`] で、確かめの順序・確かめ終わる前に副作用が無いこと・断る分岐ごとの
//! 出力を確かめる（全 OS）。本物の ACL の操作は `win/tests.rs`。

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use banto_secret_store::acl::guard::{KnownFolders, MachineEnvironment};
use banto_secret_store::acl::{
    Exposure, FileSecurity, ObservedAce, ObservedDacl, PinnedDirInfo, ADMINISTRATORS_SID,
    SYSTEM_SID,
};
use banto_secret_store::InstallUserSid;
use banto_service_control::ConsoleLine;

use super::*;

const ME: &str = "S-1-5-21-1-2-3-1001";
const EVIL: &str = "S-1-5-21-9-9-9-1666";

fn abs(rest: &str) -> PathBuf {
    let base = if cfg!(windows) { r"C:\" } else { "/" };
    let mut path = PathBuf::from(base);
    for part in rest.split('/') {
        path.push(part);
    }
    path
}

fn pd() -> PathBuf {
    abs("ProgramData")
}
fn root() -> PathBuf {
    pd().join(PRODUCT_DIR)
}
fn sink() -> PathBuf {
    root().join(SINK_DIR)
}
fn exe_dir() -> PathBuf {
    abs("Program Files/BantoHub")
}

fn known() -> KnownFolders {
    KnownFolders {
        forbidden_inside: vec![abs("Windows"), abs("Program Files")],
        forbidden_itself: vec![pd(), abs("Users")],
    }
}

struct FakeEnv {
    program_data: Result<PathBuf, String>,
    known: Result<KnownFolders, String>,
}

impl MachineEnvironment for FakeEnv {
    fn machine_variable(&self, _name: &str) -> Result<Option<String>, String> {
        panic!("prepare-sink-config はマシンの環境変数を読まない");
    }
    fn program_data(&self) -> Result<PathBuf, String> {
        self.program_data.clone()
    }
    fn known_folders(&self) -> Result<KnownFolders, String> {
        self.known.clone()
    }
}

type Log = Rc<RefCell<Vec<String>>>;

#[derive(Clone)]
struct DirEntry {
    owner: String,
    is_dir: bool,
    is_reparse_point: bool,
    real: Option<PathBuf>,
    reset: Result<(), String>,
    /// 今の DACL の ACE（既定は空 = ほかの利用者へは開いていない）。
    aces: Vec<ObservedAce>,
}

impl DirEntry {
    fn owned_by(owner: &str) -> Self {
        Self {
            owner: owner.to_string(),
            is_dir: true,
            is_reparse_point: false,
            real: None,
            reset: Ok(()),
            aces: Vec::new(),
        }
    }

    /// `%ProgramData%` から継承した形（`Users` のファイル作成・`CREATOR OWNER` のフル）。
    fn inherited_open(owner: &str) -> Self {
        let mut entry = Self::owned_by(owner);
        entry.aces = vec![users_ace(0x0000_0116, 0x12), creator_owner_ace()];
        entry
    }
}

const USERS: &str = "S-1-5-32-545";

fn users_ace(mask: u32, flags: u8) -> ObservedAce {
    ObservedAce {
        ace_type: 0,
        flags,
        mask,
        sid: USERS.to_string(),
    }
}

fn creator_owner_ace() -> ObservedAce {
    ObservedAce {
        ace_type: 0,
        flags: 0x1B,
        mask: 0x1F01FF,
        sid: "S-1-3-0".to_string(),
    }
}

#[derive(Clone)]
struct FileEntry {
    security: FileSecurity,
    real: Option<PathBuf>,
    reset: Result<(), String>,
    delete: Result<(), String>,
}

impl FileEntry {
    fn owned_by(owner: &str) -> Self {
        Self {
            security: FileSecurity {
                is_dir: false,
                is_reparse_point: false,
                links: 1,
                dacl: ObservedDacl {
                    protected: false,
                    owner: owner.to_string(),
                    aces: Some(Vec::new()),
                },
            },
            real: None,
            reset: Ok(()),
            delete: Ok(()),
        }
    }
}

fn short(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

struct FakeDir {
    path: PathBuf,
    entry: DirEntry,
    log: Log,
}

impl SinkDir for FakeDir {
    fn info(&self) -> PinnedDirInfo {
        PinnedDirInfo {
            is_dir: self.entry.is_dir,
            is_reparse_point: self.entry.is_reparse_point,
            owner: self.entry.owner.clone(),
        }
    }
    fn final_path(&self) -> PathBuf {
        self.entry.real.clone().unwrap_or_else(|| self.path.clone())
    }
    fn security(&self) -> Result<FileSecurity, String> {
        Ok(FileSecurity {
            is_dir: self.entry.is_dir,
            is_reparse_point: self.entry.is_reparse_point,
            links: 1,
            dacl: ObservedDacl {
                protected: self.entry.aces.is_empty(),
                owner: self.entry.owner.clone(),
                aces: Some(self.entry.aces.clone()),
            },
        })
    }
    fn reset_service_only(&self) -> Result<(), String> {
        self.log
            .borrow_mut()
            .push(format!("WRITE reset_dir {}", short(&self.path)));
        self.entry.reset.clone()
    }
}

struct FakeFile {
    path: PathBuf,
    entry: FileEntry,
    log: Log,
}

impl SinkFile for FakeFile {
    fn security(&self) -> FileSecurity {
        self.entry.security.clone()
    }
    fn final_path(&self) -> PathBuf {
        self.entry.real.clone().unwrap_or_else(|| self.path.clone())
    }
    fn reset_acl(&self) -> Result<(), String> {
        self.log
            .borrow_mut()
            .push(format!("WRITE reset_file {}", short(&self.path)));
        self.entry.reset.clone()
    }
    fn delete(self) -> Result<(), String> {
        self.log
            .borrow_mut()
            .push(format!("WRITE delete {}", short(&self.path)));
        self.entry.delete.clone()
    }
}

struct FakeOps {
    env: FakeEnv,
    me: Result<InstallUserSid, String>,
    example: Result<Vec<u8>, String>,
    dirs: RefCell<HashMap<PathBuf, Result<DirEntry, String>>>,
    /// 作るときに付ける形（既定は Administrators の所有）。
    on_create: HashMap<PathBuf, DirEntry>,
    files: HashMap<PathBuf, Result<FileEntry, FileOpenError>>,
    existing: HashMap<PathBuf, Result<bool, String>>,
    create_file: Result<bool, String>,
    log: Log,
}

impl FakeOps {
    /// 既定: ProgramData・ルート・置き場があり（Administrators の所有）、中に設定ファイルと
    /// 雛形がある。旧い置き場には何も無い。
    fn new() -> Self {
        let mut dirs = HashMap::new();
        dirs.insert(pd(), Ok(DirEntry::owned_by(SYSTEM_SID)));
        dirs.insert(root(), Ok(DirEntry::owned_by(ADMINISTRATORS_SID)));
        dirs.insert(sink(), Ok(DirEntry::owned_by(ADMINISTRATORS_SID)));
        let mut files = HashMap::new();
        files.insert(sink().join(CONFIG_FILE), Ok(FileEntry::owned_by(ME)));
        files.insert(
            sink().join(EXAMPLE_FILE),
            Ok(FileEntry::owned_by(ADMINISTRATORS_SID)),
        );
        Self {
            env: FakeEnv {
                program_data: Ok(pd()),
                known: Ok(known()),
            },
            me: Ok(InstallUserSid::parse(ME).unwrap()),
            example: Ok(b"# example\n".to_vec()),
            dirs: RefCell::new(dirs),
            on_create: HashMap::new(),
            files,
            existing: HashMap::new(),
            create_file: Ok(true),
            log: Rc::new(RefCell::new(Vec::new())),
        }
    }

    fn calls(&self) -> Vec<String> {
        self.log.borrow().clone()
    }

    fn writes(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter(|c| c.starts_with("WRITE") || c.starts_with("CREATE"))
            .collect()
    }

    fn set_dir(&self, path: PathBuf, entry: DirEntry) {
        self.dirs.borrow_mut().insert(path, Ok(entry));
    }

    fn file_mut(&mut self, name: &str) -> &mut FileEntry {
        self.files
            .get_mut(&sink().join(name))
            .unwrap()
            .as_mut()
            .ok()
            .unwrap()
    }
}

impl SinkConfigOps for FakeOps {
    type Env = FakeEnv;
    type Dir = FakeDir;
    type File = FakeFile;

    fn machine_env(&self) -> &FakeEnv {
        &self.env
    }
    fn current_user_sid(&self) -> Result<InstallUserSid, String> {
        self.me.clone()
    }
    fn example_contents(&self) -> Result<Vec<u8>, String> {
        self.example.clone()
    }
    fn exe_dir(&self) -> Option<PathBuf> {
        Some(exe_dir())
    }
    fn exists(&self, path: &Path) -> Result<bool, String> {
        self.log
            .borrow_mut()
            .push(format!("exists {}", path.display()));
        self.existing.get(path).cloned().unwrap_or(Ok(false))
    }
    fn pin_dir(&self, dir: &Path) -> Result<Option<FakeDir>, String> {
        self.log.borrow_mut().push(format!("pin {}", short(dir)));
        match self.dirs.borrow().get(dir) {
            None => Ok(None),
            Some(Err(error)) => Err(error.clone()),
            Some(Ok(entry)) => Ok(Some(FakeDir {
                path: dir.to_path_buf(),
                entry: entry.clone(),
                log: self.log.clone(),
            })),
        }
    }
    fn create_child_dir(
        &self,
        parent: &FakeDir,
        name: &str,
        how: CreateHow,
    ) -> Result<Option<FakeDir>, String> {
        let path = parent.path.join(name);
        self.log
            .borrow_mut()
            .push(format!("CREATE dir {name} {how:?}"));
        if self.dirs.borrow().contains_key(&path) {
            return Ok(None);
        }
        let entry = self
            .on_create
            .get(&path)
            .cloned()
            .unwrap_or_else(|| DirEntry::owned_by(ADMINISTRATORS_SID));
        self.dirs
            .borrow_mut()
            .insert(path.clone(), Ok(entry.clone()));
        Ok(Some(FakeDir {
            path,
            entry,
            log: self.log.clone(),
        }))
    }
    fn open_child_dir(&self, parent: &FakeDir, name: &str) -> Result<Option<FakeDir>, String> {
        let path = parent.path.join(name);
        self.log.borrow_mut().push(format!("open_dir {name}"));
        match self.dirs.borrow().get(&path) {
            None => Ok(None),
            Some(Err(error)) => Err(error.clone()),
            Some(Ok(entry)) => Ok(Some(FakeDir {
                path,
                entry: entry.clone(),
                log: self.log.clone(),
            })),
        }
    }
    fn open_child_file(
        &self,
        parent: &FakeDir,
        name: &str,
    ) -> Result<Option<FakeFile>, FileOpenError> {
        let path = parent.path.join(name);
        self.log.borrow_mut().push(format!("open_file {name}"));
        match self.files.get(&path) {
            None => Ok(None),
            Some(Err(error)) => Err(error.clone()),
            Some(Ok(entry)) => Ok(Some(FakeFile {
                path,
                entry: entry.clone(),
                log: self.log.clone(),
            })),
        }
    }
    fn create_child_file(
        &self,
        parent: &FakeDir,
        name: &str,
        contents: &[u8],
    ) -> Result<bool, String> {
        self.log.borrow_mut().push(format!(
            "CREATE file {name} under {} ({} bytes)",
            short(&parent.path),
            contents.len()
        ));
        self.create_file.clone()
    }
}

fn run(ops: &FakeOps) -> (Result<(), Refused>, Vec<String>) {
    let mut lines = Vec::new();
    let result = {
        let mut out = |line: ConsoleLine| lines.push(line);
        prepare_sink_config(ops, &mut out)
    };
    let texts = lines
        .into_iter()
        .map(|line| match line {
            ConsoleLine::Stdout(text) => format!("out {text}"),
            ConsoleLine::Stderr(text) => format!("err {text}"),
        })
        .collect();
    (result, texts)
}

fn has(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|l| l.contains(needle))
}

// --- 順序 ----------------------------------------------------------------------------

/// 既にある置き場: 開いて確かめる → 中のファイルを確かめる → 旧い置き場を見る → **ここから**
/// 書く（置き場 → 設定ファイル → 雛形を消して作る）。
#[test]
fn checks_everything_before_the_first_write() {
    let ops = FakeOps::new();
    let (result, lines) = run(&ops);
    assert_eq!(result, Ok(()), "{lines:#?}");
    assert_eq!(
        ops.calls(),
        vec![
            // 旧い置き場は確かめの最初に見る（断ったときにも知らせるため。読むだけ）。
            format!("exists {}", root().join(CONFIG_FILE).display()),
            format!("exists {}", exe_dir().join(CONFIG_FILE).display()),
            "pin BantoHub".to_string(),
            "open_dir sink".to_string(),
            format!("open_file {CONFIG_FILE}"),
            format!("open_file {EXAMPLE_FILE}"),
            "WRITE reset_dir sink".to_string(),
            format!("WRITE reset_file {CONFIG_FILE}"),
            format!("WRITE delete {EXAMPLE_FILE}"),
            format!("CREATE file {EXAMPLE_FILE} under sink (10 bytes)"),
        ]
    );
    assert!(has(&lines, "所有者と ACL を整えました"), "{lines:#?}");
    assert!(has(&lines, "中身は読んでいません"), "{lines:#?}");
    assert!(
        has(&lines, "インストールのたびに差し替えます"),
        "{lines:#?}"
    );
}

/// 無いルート・置き場は、確かめ済みの親のハンドル基準で作る（ルートは継承、置き場は保護）。
/// 作った置き場にも、所有者を Administrators にする書き込みをする。
#[test]
fn creates_a_missing_root_and_place_relative_to_the_checked_parent() {
    let mut ops = FakeOps::new();
    ops.dirs.borrow_mut().remove(&root());
    ops.dirs.borrow_mut().remove(&sink());
    ops.files.clear();
    let (result, lines) = run(&ops);
    assert_eq!(result, Ok(()), "{lines:#?}");
    assert_eq!(
        // 最初の 2 つは旧い置き場の確かめ（読むだけ）。
        ops.calls()[2..7],
        [
            "pin BantoHub".to_string(),
            "pin ProgramData".to_string(),
            "CREATE dir BantoHub Inherit".to_string(),
            "open_dir sink".to_string(),
            "CREATE dir sink ServiceOnly".to_string(),
        ]
    );
    assert!(ops.calls().contains(&"WRITE reset_dir sink".to_string()));
    assert!(has(&lines, "作りました"), "{lines:#?}");
    assert!(has(&lines, "まだありません"), "{lines:#?}");
}

/// 2 回目も同じ（冪等）。
#[test]
fn running_twice_gives_the_same_result() {
    let ops = FakeOps::new();
    let (first, _) = run(&ops);
    let (second, _) = run(&ops);
    assert_eq!((first, second), (Ok(()), Ok(())));
}

// --- 断る分岐（確かめ終わる前は書かない） ---------------------------------------------

#[test]
fn every_refusal_happens_before_any_write() {
    type Setup = fn(&mut FakeOps);
    let cases: Vec<(&str, Setup, &str)> = vec![
        (
            "ProgramData が取れない",
            |ops| ops.env.program_data = Err("取れない".to_string()),
            "ProgramData の場所を取れませんでした",
        ),
        (
            "既知のフォルダが取れない",
            |ops| ops.env.known = Err("取れない".to_string()),
            "既知のフォルダの場所を取れませんでした",
        ),
        (
            "雛形が読めない",
            |ops| ops.example = Err("無い".to_string()),
            "雛形（banto-hub-sink.toml.example）を読めませんでした",
        ),
        (
            "雛形が大きすぎる",
            |ops| ops.example = Ok(vec![b'#'; MAX_EXAMPLE_BYTES + 1]),
            "大きすぎます",
        ),
        (
            "雛形が UTF-8 でない",
            |ops| ops.example = Ok(vec![0xff]),
            "UTF-8 ではありません",
        ),
        (
            "ルートが無く、親もない",
            |ops| {
                ops.dirs.borrow_mut().remove(&root());
                ops.dirs.borrow_mut().remove(&pd());
            },
            "がありません",
        ),
        (
            "ルートが無く、親がジャンクション",
            |ops| {
                ops.dirs.borrow_mut().remove(&root());
                let mut parent = DirEntry::owned_by(SYSTEM_SID);
                parent.is_reparse_point = true;
                ops.set_dir(pd(), parent);
            },
            "何も作らずに止めます",
        ),
        (
            "ルートの本当のパスが違う",
            |ops| {
                let mut entry = DirEntry::owned_by(ADMINISTRATORS_SID);
                entry.real = Some(abs("Elsewhere/BantoHub"));
                ops.set_dir(root(), entry);
            },
            "本当の場所",
        ),
        (
            "ルートの所有者が一般の利用者（所有者を取り直す手順を出す）",
            |ops| ops.set_dir(root(), DirEntry::owned_by(EVIL)),
            "/setowner *S-1-5-32-544",
        ),
        (
            "ルートがジャンクション",
            |ops| {
                let mut entry = DirEntry::owned_by(ADMINISTRATORS_SID);
                entry.is_reparse_point = true;
                ops.set_dir(root(), entry);
            },
            "reparse point",
        ),
        (
            "一般の利用者が先に sink\\ を作っていた（所有者で断る）",
            |ops| ops.set_dir(sink(), DirEntry::owned_by(EVIL)),
            EVIL,
        ),
        (
            "sink\\ がジャンクション",
            |ops| {
                let mut entry = DirEntry::owned_by(ADMINISTRATORS_SID);
                entry.is_reparse_point = true;
                ops.set_dir(sink(), entry);
            },
            "reparse point",
        ),
        (
            "sink\\ がフォルダでない",
            |ops| {
                let mut entry = DirEntry::owned_by(ADMINISTRATORS_SID);
                entry.is_dir = false;
                ops.set_dir(sink(), entry);
            },
            "フォルダではありません",
        ),
        (
            "sink\\ の本当のパスが違う",
            |ops| {
                let mut entry = DirEntry::owned_by(ADMINISTRATORS_SID);
                entry.real = Some(abs("Elsewhere/sink"));
                ops.set_dir(sink(), entry);
            },
            "本当の場所",
        ),
        (
            "sink\\ を開けない",
            |ops| {
                ops.dirs
                    .borrow_mut()
                    .insert(sink(), Err("壊れた".to_string()));
            },
            "開けませんでした",
        ),
        (
            "設定ファイルがシンボリックリンク",
            |ops| ops.file_mut(CONFIG_FILE).security.is_reparse_point = true,
            "reparse point",
        ),
        (
            "設定ファイルがハードリンク",
            |ops| ops.file_mut(CONFIG_FILE).security.links = 2,
            "ハードリンク",
        ),
        (
            "設定ファイルがフォルダ",
            |ops| ops.file_mut(CONFIG_FILE).security.is_dir = true,
            "フォルダです",
        ),
        (
            "設定ファイルの本当のパスが違う",
            |ops| ops.file_mut(CONFIG_FILE).real = Some(abs("Elsewhere/banto-hub-sink.toml")),
            "本当の場所",
        ),
        (
            "設定ファイルが書き込みに開かれている",
            |ops| {
                ops.files
                    .insert(sink().join(CONFIG_FILE), Err(FileOpenError::InUse));
            },
            "書き込みに開かれています",
        ),
        (
            "雛形がフォルダ",
            |ops| ops.file_mut(EXAMPLE_FILE).security.is_dir = true,
            "フォルダです",
        ),
        (
            "雛形が書き込みに開かれている",
            |ops| {
                ops.files
                    .insert(sink().join(EXAMPLE_FILE), Err(FileOpenError::InUse));
            },
            "書き込みに開かれています",
        ),
    ];
    for (name, setup, needle) in cases {
        let mut ops = FakeOps::new();
        setup(&mut ops);
        let (result, lines) = run(&ops);
        assert_eq!(result, Err(Refused), "{name}: {lines:#?}");
        assert!(has(&lines, needle), "{name}: {needle}: {lines:#?}");
        let writes: Vec<String> = ops
            .writes()
            .into_iter()
            .filter(|w| !w.starts_with("CREATE dir"))
            .collect();
        assert!(writes.is_empty(), "{name}: 確かめの前に書いた: {writes:?}");
        // 一般の利用者が作った sink\ は、作り直しもしない。
        assert!(
            !ops.calls()
                .iter()
                .any(|c| c == "CREATE dir sink ServiceOnly"),
            "{name}: {:?}",
            ops.calls()
        );
    }
}

/// 作った後に確かめが通らなかったときは、作ったものが残っていることを知らせる。
#[test]
fn a_refusal_after_creating_reports_what_was_created() {
    let mut ops = FakeOps::new();
    ops.dirs.borrow_mut().remove(&sink());
    ops.on_create.insert(sink(), DirEntry::owned_by(EVIL));
    let (result, lines) = run(&ops);
    assert_eq!(result, Err(Refused));
    assert!(
        has(&lines, "作りましたが、確かめが通らなかったので残っています"),
        "{lines:#?}"
    );
    assert!(ops.writes().iter().all(|w| w.starts_with("CREATE dir")));
}

/// 書く途中の失敗は止めて理由を出す（`Exposed` は人の読める文言で）。
#[test]
fn write_failures_stop_with_a_reason() {
    let mut ops = FakeOps::new();
    ops.file_mut(CONFIG_FILE).reset = Err(secret_error_text(
        &banto_secret_store::SecretStoreError::Exposed(vec![Exposure::HardLinked(2)]),
    ));
    let (result, lines) = run(&ops);
    assert_eq!(result, Err(Refused));
    assert!(
        has(&lines, "ハードリンクです（名前が 2 個あります）"),
        "{lines:#?}"
    );
    assert!(
        !has(&lines, "HardLinked"),
        "Debug の表記を出さない: {lines:#?}"
    );
    assert!(!ops.calls().iter().any(|c| c.contains("CREATE file")));

    let mut ops = FakeOps::new();
    ops.create_file = Ok(false);
    let (result, lines) = run(&ops);
    assert_eq!(result, Err(Refused));
    assert!(has(&lines, "既にありました"), "{lines:#?}");
}

/// 設定ファイルの所有者がほかの利用者でも断らず、整えた後に警告を出す。昇格した本人なら出さない。
#[test]
fn a_foreign_config_owner_is_fixed_with_a_warning() {
    let mut ops = FakeOps::new();
    ops.file_mut(CONFIG_FILE).security.dacl.owner = EVIL.to_string();
    let (result, lines) = run(&ops);
    assert_eq!(result, Ok(()));
    assert!(
        has(&lines, &format!("所有者は {EVIL} でした")),
        "{lines:#?}"
    );
    let ops = FakeOps::new();
    let (_, lines) = run(&ops);
    assert!(!has(&lines, "でした（SYSTEM"), "{lines:#?}");
}

/// 旧い置き場に設定ファイルがあれば知らせる（触らない・失敗にしない）。
#[test]
fn a_legacy_config_is_reported_but_not_touched() {
    let mut ops = FakeOps::new();
    ops.existing.insert(root().join(CONFIG_FILE), Ok(true));
    let (result, lines) = run(&ops);
    assert_eq!(result, Ok(()));
    assert!(has(&lines, "旧い置き場に設定ファイル"), "{lines:#?}");
    assert!(has(&lines, "失効"), "{lines:#?}");
}

#[test]
fn every_exposure_has_a_human_text() {
    for found in [
        Exposure::ReparsePoint,
        Exposure::HardLinked(2),
        Exposure::ForeignOwner(EVIL.to_string()),
        Exposure::NullDacl,
        Exposure::AllowsOther {
            ace_type: 0,
            flags: 0,
            mask: 1,
            sid: EVIL.to_string(),
        },
        Exposure::UnknownAce {
            ace_type: 99,
            flags: 0,
            mask: 1,
        },
        Exposure::UnreadableSid {
            ace_type: 0,
            flags: 0,
            mask: 1,
        },
    ] {
        let text = exposure_text(&found);
        assert!(!text.is_empty() && text.is_char_boundary(0), "{found:?}");
        assert!(!text.contains("Exposure"), "{text}");
    }
}

/// 所有者で断ったときの文言の golden（2026-10-17 オーナー決定）: ルートは所有者を
/// Administrators に取り直す手順、`sink\` は取り直さずに消してから再実行する手順。
#[test]
fn foreign_owner_wording_differs_by_level() {
    let refusal = guard::DataAclRefusal::ForeignOwner(EVIL.to_string());
    let root_text = dir_refusal_text(DirLevel::Root, &root(), &refusal);
    assert_eq!(
        root_text,
        format!(
            "Hub のルート（{r}）は所有者（{EVIL}）が SYSTEM・Administrators・実行した管理者のどれでもありません。乗っ取りの恐れがあるため、止めます（中身を確かめ、このフォルダの所有者を Administrators に取り直してから（takeown /f \"{r}\" /a か icacls \"{r}\" /setowner *S-1-5-32-544）、もう一度実行してください）",
            r = root().display()
        )
    );
    let sink_text = dir_refusal_text(DirLevel::Sink, &sink(), &refusal);
    assert_eq!(
        sink_text,
        format!(
            "設定ファイルの置き場（{s}）は所有者（{EVIL}）が SYSTEM・Administrators・実行した管理者のどれでもありません。一般の利用者が先に作った（乗っ取りの準備の）恐れがあるため、何も変えずに止めます（所有者は取り直さず、中身を確かめてから、このフォルダ（{s}）を消して、もう一度実行してください。中の設定ファイルは使わず、作り直してください）",
            s = sink().display()
        )
    );
    assert!(!sink_text.contains("takeown"), "{sink_text}");
}

// --- 整える前に開いていたか（#655 のレビュー Should 1） ------------------------------

const OTHER_ADMIN: &str = "S-1-5-21-1-2-3-1500";

/// 管理者が手で作った（継承の DACL の）`sink\` に、一般の利用者が先に置いた設定ファイルは、
/// 整え直さずに断る（何も書かない・失効の案内）。
#[test]
fn a_config_planted_in_an_open_place_is_refused() {
    let mut ops = FakeOps::new();
    ops.set_dir(sink(), DirEntry::inherited_open(ADMINISTRATORS_SID));
    let file = ops.file_mut(CONFIG_FILE);
    file.security.dacl.owner = EVIL.to_string();
    file.security.dacl.aces = Some(vec![
        ObservedAce {
            ace_type: 0,
            flags: 0x10,
            mask: 0x1F01FF,
            sid: EVIL.to_string(),
        },
        users_ace(0x1200A9, 0x10),
    ]);
    let (result, lines) = run(&ops);
    assert_eq!(result, Err(Refused), "{lines:#?}");
    assert!(
        has(&lines, "ほかの利用者が用意した・読めた恐れ"),
        "{lines:#?}"
    );
    assert!(has(&lines, "失効"), "{lines:#?}");
    assert!(has(&lines, EVIL), "所有者も手がかりに出す: {lines:#?}");
    assert!(ops.writes().is_empty(), "{:?}", ops.calls());
}

/// 置き場は保護されていても、設定ファイルがほかの利用者に開いていた（外から移動した）なら断る。
#[test]
fn a_config_open_to_others_is_refused_even_in_a_protected_place() {
    let mut ops = FakeOps::new();
    ops.file_mut(CONFIG_FILE)
        .security
        .dacl
        .aces
        .as_mut()
        .unwrap()
        .push(users_ace(0x1200A9, 0x10));
    let (result, lines) = run(&ops);
    assert_eq!(result, Err(Refused), "{lines:#?}");
    assert!(has(&lines, USERS), "{lines:#?}");
    assert!(ops.writes().is_empty(), "{:?}", ops.calls());
}

/// `sink\` だけが開いていて、設定ファイルが無ければ、整えたうえで知らせる。
#[test]
fn an_open_place_without_a_config_is_fixed_with_a_notice() {
    let mut ops = FakeOps::new();
    ops.set_dir(sink(), DirEntry::inherited_open(ADMINISTRATORS_SID));
    ops.files.remove(&sink().join(CONFIG_FILE));
    let (result, lines) = run(&ops);
    assert_eq!(result, Ok(()), "{lines:#?}");
    assert!(ops.calls().contains(&"WRITE reset_dir sink".to_string()));
    assert!(
        has(
            &lines,
            "整える前は、ほかの利用者が設定ファイルの置き場の中に書けました"
        ),
        "{lines:#?}"
    );
}

/// 所有者だけが別の管理者（置き場は保護、ファイルの理由は所有者だけ）なら、整え直して警告する
/// （オーナー決定 その 2）。
#[test]
fn a_config_owned_by_another_admin_is_fixed_with_a_warning() {
    let mut ops = FakeOps::new();
    ops.file_mut(CONFIG_FILE).security.dacl.owner = OTHER_ADMIN.to_string();
    let (result, lines) = run(&ops);
    assert_eq!(result, Ok(()), "{lines:#?}");
    assert!(ops
        .calls()
        .contains(&format!("WRITE reset_file {CONFIG_FILE}")));
    assert!(
        has(&lines, &format!("所有者は {OTHER_ADMIN} でした")),
        "{lines:#?}"
    );
    assert!(!has(&lines, "書けました"), "{lines:#?}");
}

/// 断る規則の表（純関数）。
#[test]
fn config_refusal_table() {
    let owner = Exposure::ForeignOwner(OTHER_ADMIN.to_string());
    let allows = Exposure::AllowsOther {
        ace_type: 0,
        flags: 0,
        mask: 0x1200A9,
        sid: USERS.to_string(),
    };
    let unknown = Exposure::UnknownAce {
        ace_type: 99,
        flags: 0,
        mask: 1,
    };
    let cases: Vec<(&str, Vec<Exposure>, Vec<Exposure>, bool)> = vec![
        ("何も無い", vec![], vec![], false),
        ("所有者だけ", vec![], vec![owner.clone()], false),
        ("置き場が開いていた", vec![allows.clone()], vec![], true),
        (
            "置き場が開いていた・所有者",
            vec![allows.clone()],
            vec![owner.clone()],
            true,
        ),
        ("置き場の null DACL", vec![Exposure::NullDacl], vec![], true),
        ("ファイルが開いていた", vec![], vec![allows.clone()], true),
        (
            "ファイルの分からない ACE",
            vec![],
            vec![unknown.clone()],
            true,
        ),
        (
            "ファイルの null DACL",
            vec![],
            vec![Exposure::NullDacl, owner.clone()],
            true,
        ),
    ];
    for (name, sink_open, file, refuse) in cases {
        assert_eq!(
            config_refusal(&sink_open, &file).is_some(),
            refuse,
            "{name}"
        );
    }
    // 置き場の理由として拾うのは、開いていた形跡だけ（所有者・リンクは拾わない）。
    assert_eq!(
        opened_reasons(&[
            owner.clone(),
            Exposure::ReparsePoint,
            allows.clone(),
            unknown.clone()
        ]),
        vec![allows, unknown]
    );
}

/// 断ったときも、旧い置き場の知らせを出す（#655 のレビュー Nit 2）。
#[test]
fn a_refusal_also_reports_the_legacy_config() {
    let mut ops = FakeOps::new();
    ops.set_dir(root(), DirEntry::owned_by(EVIL));
    ops.existing.insert(root().join(CONFIG_FILE), Ok(true));
    let (result, lines) = run(&ops);
    assert_eq!(result, Err(Refused));
    assert!(has(&lines, "旧い置き場に設定ファイル"), "{lines:#?}");
}
