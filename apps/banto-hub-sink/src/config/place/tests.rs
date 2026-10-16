//! 置き場の決め方（純関数の表）・読み方（偽物の [`PlaceOps`] で順序と断る分岐）・文言。
//! Windows の実物（一時フォルダ）は下の `windows` モジュール。

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use banto_secret_store::acl::{
    Exposure, FileSecurity, ObservedAce, ObservedDacl, ADMINISTRATORS_SID, SYSTEM_SID,
};

use super::*;
use crate::config::{ConfigError, ENV_CONFIG_PATH, PREPARE_COMMAND};

/// 全 OS で絶対パスになる基点（Windows は`C:\`、それ以外は`/`）。
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

fn exe_dir() -> PathBuf {
    abs("Program Files/BantoHub")
}

fn sink_dir() -> PathBuf {
    pd().join("BantoHub").join("sink")
}

fn sink_file() -> PathBuf {
    sink_dir().join("banto-hub-sink.toml")
}

fn legacy_root() -> PathBuf {
    pd().join("BantoHub").join("banto-hub-sink.toml")
}

fn legacy_exe() -> PathBuf {
    exe_dir().join("banto-hub-sink.toml")
}

// --- どこを読むか（純関数の表） -----------------------------------------------------

#[test]
fn decide_paths_table() {
    let env_file = abs("work/dev/sink.toml");
    struct Case {
        name: &'static str,
        mode: RunMode,
        env: Option<&'static str>,
        pd_ok: bool,
        expect: Result<(PathBuf, Source), &'static str>,
    }
    let env_text: &'static str =
        Box::leak(env_file.to_string_lossy().into_owned().into_boxed_str());
    let cases = [
        Case {
            name: "サービスは既定の置き場",
            mode: RunMode::Service,
            env: None,
            pd_ok: true,
            expect: Ok((sink_file(), Source::Default)),
        },
        Case {
            name: "サービスは環境変数を見ない",
            mode: RunMode::Service,
            env: Some(env_text),
            pd_ok: true,
            expect: Ok((sink_file(), Source::Default)),
        },
        Case {
            name: "サービスは環境変数が不正でも見ない",
            mode: RunMode::Service,
            env: Some("relative.toml"),
            pd_ok: true,
            expect: Ok((sink_file(), Source::Default)),
        },
        Case {
            name: "コンソールは環境変数が無ければ既定",
            mode: RunMode::Console,
            env: None,
            pd_ok: true,
            expect: Ok((sink_file(), Source::Default)),
        },
        Case {
            name: "コンソールの空の環境変数は無いのと同じ",
            mode: RunMode::Console,
            env: Some(""),
            pd_ok: true,
            expect: Ok((sink_file(), Source::Default)),
        },
        Case {
            name: "コンソールは環境変数を使う",
            mode: RunMode::Console,
            env: Some(env_text),
            pd_ok: true,
            expect: Ok((env_file.clone(), Source::Env)),
        },
        Case {
            name: "コンソールの環境変数は ProgramData が無くても使える",
            mode: RunMode::Console,
            env: Some(env_text),
            pd_ok: false,
            expect: Ok((env_file.clone(), Source::Env)),
        },
        Case {
            name: "相対パスの環境変数は断る",
            mode: RunMode::Console,
            env: Some("relative.toml"),
            pd_ok: true,
            expect: Err("env"),
        },
        Case {
            name: "ProgramData が無ければ断る",
            mode: RunMode::Service,
            env: None,
            pd_ok: false,
            expect: Err("pd"),
        },
    ];
    for case in cases {
        let program_data = if case.pd_ok {
            Ok(pd())
        } else {
            Err("取れない".to_string())
        };
        let got = decide_paths(
            case.mode,
            case.env.map(OsString::from),
            program_data,
            Some(&exe_dir()),
        );
        match (got, &case.expect) {
            (Ok(paths), Ok((file, source))) => {
                assert_eq!(&paths.file(), file, "{}", case.name);
                assert_eq!(&paths.source, source, "{}", case.name);
                let mut legacy = Vec::new();
                if case.pd_ok {
                    legacy.push(legacy_root());
                    assert_eq!(paths.default_file, Some(sink_file()), "{}", case.name);
                } else {
                    assert_eq!(paths.default_file, None, "{}", case.name);
                }
                legacy.push(legacy_exe());
                assert_eq!(paths.legacy, legacy, "{}", case.name);
            }
            (Err(ConfigError::EnvInvalid { .. }), Err("env")) => {}
            (Err(ConfigError::ProgramDataUnavailable(_)), Err("pd")) => {}
            (got, expect) => panic!("{}: {got:?} / {expect:?}", case.name),
        }
    }
}

#[test]
fn env_with_a_dot_segment_is_refused() {
    let value = abs("work/../evil/sink.toml");
    let got = decide_paths(
        RunMode::Console,
        Some(value.clone().into_os_string()),
        Ok(pd()),
        None,
    );
    assert!(
        matches!(got, Err(ConfigError::EnvInvalid { value: ref v, .. }) if v == &value),
        "{got:?}"
    );
}

// --- 偽物 --------------------------------------------------------------------------

fn protected(is_dir: bool) -> FileSecurity {
    FileSecurity {
        is_dir,
        is_reparse_point: false,
        links: 1,
        dacl: ObservedDacl {
            protected: true,
            owner: ADMINISTRATORS_SID.to_string(),
            aces: Some(vec![
                ObservedAce {
                    ace_type: 0,
                    flags: if is_dir { 3 } else { 0 },
                    mask: 0x1F01FF,
                    sid: SYSTEM_SID.to_string(),
                },
                ObservedAce {
                    ace_type: 0,
                    flags: if is_dir { 3 } else { 0 },
                    mask: 0x1F01FF,
                    sid: ADMINISTRATORS_SID.to_string(),
                },
            ]),
        },
    }
}

const USERS: &str = "S-1-5-32-545";
const ALICE: &str = "S-1-5-21-1-2-3-1001";

/// 偽の置き場・ファイル 1 つ。
#[derive(Clone)]
struct Entry {
    security: FileSecurity,
    /// 開いたときの本当のパス（`None`ならパスそのもの）。
    real: Option<PathBuf>,
}

impl Entry {
    fn dir() -> Self {
        Self {
            security: protected(true),
            real: None,
        }
    }
    fn file() -> Self {
        Self {
            security: protected(false),
            real: None,
        }
    }
}

struct FakeOps {
    /// 旧い置き場のうち、あるもの（`Err` は確かめられない）。
    existing: HashMap<PathBuf, Result<bool, String>>,
    dirs: HashMap<PathBuf, Result<Entry, OpenError>>,
    files: HashMap<PathBuf, Result<Entry, OpenError>>,
    contents: Result<Vec<u8>, String>,
    log: RefCell<Vec<String>>,
}

impl FakeOps {
    /// 既定: 旧い置き場は無く、置き場とファイルは保護されている。
    fn new() -> Self {
        let mut dirs = HashMap::new();
        dirs.insert(sink_dir(), Ok(Entry::dir()));
        let mut files = HashMap::new();
        files.insert(sink_file(), Ok(Entry::file()));
        Self {
            existing: HashMap::new(),
            dirs,
            files,
            contents: Ok(b"hub_url = \"http://127.0.0.1:8722\"\napi_key = \"bh_x.y\"\n".to_vec()),
            log: RefCell::new(Vec::new()),
        }
    }

    fn dir_mut(&mut self) -> &mut Entry {
        self.dirs
            .get_mut(&sink_dir())
            .unwrap()
            .as_mut()
            .ok()
            .unwrap()
    }

    fn file_mut(&mut self) -> &mut Entry {
        self.files
            .get_mut(&sink_file())
            .unwrap()
            .as_mut()
            .ok()
            .unwrap()
    }

    fn calls(&self) -> Vec<String> {
        self.log.borrow().clone()
    }
}

fn short(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

struct FakeDir {
    path: PathBuf,
    entry: Entry,
}

struct FakeFile {
    path: PathBuf,
    entry: Entry,
}

impl PlaceOps for FakeOps {
    type Dir = FakeDir;
    type File = FakeFile;

    fn exists(&self, path: &Path) -> Result<bool, String> {
        self.log
            .borrow_mut()
            .push(format!("exists {}", path.display()));
        self.existing.get(path).cloned().unwrap_or(Ok(false))
    }
    fn pin_dir(&self, dir: &Path) -> Result<Option<FakeDir>, OpenError> {
        self.log.borrow_mut().push(format!("pin {}", short(dir)));
        match self.dirs.get(dir) {
            None => Ok(None),
            Some(Err(error)) => Err(error.clone()),
            Some(Ok(entry)) => Ok(Some(FakeDir {
                path: dir.to_path_buf(),
                entry: entry.clone(),
            })),
        }
    }
    fn dir_final_path(&self, dir: &FakeDir) -> PathBuf {
        dir.entry.real.clone().unwrap_or_else(|| dir.path.clone())
    }
    fn dir_security(&self, dir: &FakeDir) -> Result<FileSecurity, String> {
        self.log
            .borrow_mut()
            .push(format!("dir_security {}", short(&dir.path)));
        Ok(dir.entry.security.clone())
    }
    fn open_child_file(&self, dir: &FakeDir, name: &str) -> Result<Option<FakeFile>, OpenError> {
        let path = dir.path.join(name);
        self.log.borrow_mut().push(format!("open {name}"));
        match self.files.get(&path) {
            None => Ok(None),
            Some(Err(error)) => Err(error.clone()),
            Some(Ok(entry)) => Ok(Some(FakeFile {
                path,
                entry: entry.clone(),
            })),
        }
    }
    fn file_final_path(&self, file: &FakeFile) -> PathBuf {
        file.entry.real.clone().unwrap_or_else(|| file.path.clone())
    }
    fn file_security(&self, file: &FakeFile) -> FileSecurity {
        file.entry.security.clone()
    }
    fn read(&self, file: &FakeFile, max_len: usize) -> Result<Vec<u8>, String> {
        assert_eq!(max_len, MAX_CONFIG_BYTES);
        self.log
            .borrow_mut()
            .push(format!("read {}", short(&file.path)));
        self.contents.clone()
    }
}

fn default_paths() -> Paths {
    decide_paths(RunMode::Service, None, Ok(pd()), Some(&exe_dir())).unwrap()
}

fn read(ops: &FakeOps) -> Result<(PathBuf, String), ConfigError> {
    read_protected(ops, &default_paths())
}

fn allows(sid: &str) -> ObservedAce {
    ObservedAce {
        ace_type: 0,
        flags: 0,
        mask: 0x1200A9,
        sid: sid.to_string(),
    }
}

// --- 読み方（偽物） ------------------------------------------------------------------

/// 保護されていれば、旧い置き場 → 置き場 → ファイルの順に確かめてから読む。
#[test]
fn a_protected_place_is_read_after_every_check() {
    let ops = FakeOps::new();
    let (path, text) = read(&ops).unwrap();
    assert_eq!(path, sink_file());
    assert!(text.contains("hub_url"));
    assert_eq!(
        ops.calls(),
        vec![
            format!("exists {}", legacy_root().display()),
            format!("exists {}", legacy_exe().display()),
            "pin sink".to_string(),
            "dir_security sink".to_string(),
            "open banto-hub-sink.toml".to_string(),
            "read banto-hub-sink.toml".to_string(),
        ]
    );
}

/// 断る分岐ごとに: 理由（の形）と、**中身を読んでいない**こと。
#[test]
fn every_refusal_is_reached_without_reading() {
    type Setup = fn(&mut FakeOps);
    type Check = fn(&ConfigError) -> bool;
    let cases: Vec<(&str, Setup, Check)> = vec![
        (
            "旧い置き場（ルート直下）にファイル",
            |ops| {
                ops.existing.insert(legacy_root(), Ok(true));
            },
            |e| matches!(e, ConfigError::LegacyFound { paths, .. } if paths == &[legacy_root()]),
        ),
        (
            "旧い置き場（exe の隣）にファイル",
            |ops| {
                ops.existing.insert(legacy_exe(), Ok(true));
            },
            |e| matches!(e, ConfigError::LegacyFound { paths, .. } if paths == &[legacy_exe()]),
        ),
        (
            "旧い置き場の両方",
            |ops| {
                ops.existing.insert(legacy_root(), Ok(true));
                ops.existing.insert(legacy_exe(), Ok(true));
            },
            |e| matches!(e, ConfigError::LegacyFound { paths, .. } if paths.len() == 2),
        ),
        (
            "旧い置き場を確かめられない",
            |ops| {
                ops.existing.insert(legacy_exe(), Err("拒否".to_string()));
            },
            |e| matches!(e, ConfigError::Unverifiable { path, .. } if path == &legacy_exe()),
        ),
        (
            "置き場が無い",
            |ops| {
                ops.dirs.clear();
            },
            |e| matches!(e, ConfigError::PlaceMissing { env: false, .. }),
        ),
        (
            "置き場を開けない（拒否）",
            |ops| {
                ops.dirs.insert(sink_dir(), Err(OpenError::Denied));
            },
            |e| matches!(e, ConfigError::AccessDenied { path } if path == &sink_dir()),
        ),
        (
            "置き場を開けない（ほか）",
            |ops| {
                ops.dirs
                    .insert(sink_dir(), Err(OpenError::Other("壊れた".to_string())));
            },
            |e| matches!(e, ConfigError::Unverifiable { path, .. } if path == &sink_dir()),
        ),
        (
            "置き場の本当のパスが違う",
            |ops| {
                ops.dir_mut().real = Some(abs("Users/evil/sink"));
            },
            |e| matches!(e, ConfigError::Unprotected { dir_problems, .. } if matches!(dir_problems[..], [Problem::RealPathDiffers(_)])),
        ),
        (
            "置き場の所有者が利用者",
            |ops| {
                ops.dir_mut().security.dacl.owner = ALICE.to_string();
            },
            |e| matches!(e, ConfigError::Unprotected { dir_problems, .. } if dir_problems == &[Problem::Exposure(Exposure::ForeignOwner(ALICE.to_string()))]),
        ),
        (
            "置き場を Users が読める",
            |ops| {
                ops.dir_mut()
                    .security
                    .dacl
                    .aces
                    .as_mut()
                    .unwrap()
                    .push(allows(USERS));
            },
            |e| matches!(e, ConfigError::Unprotected { dir_problems, .. } if matches!(&dir_problems[..], [Problem::Exposure(Exposure::AllowsOther { sid, .. })] if sid == USERS)),
        ),
        (
            "置き場がジャンクション（中へ進まない）",
            |ops| {
                ops.dir_mut().security.is_reparse_point = true;
            },
            |e| matches!(e, ConfigError::Unprotected { dir_problems, file_problems, .. } if dir_problems == &[Problem::Exposure(Exposure::ReparsePoint)] && file_problems.is_empty()),
        ),
        (
            "置き場がフォルダでない",
            |ops| {
                ops.dir_mut().security.is_dir = false;
            },
            |e| matches!(e, ConfigError::Unprotected { dir_problems, .. } if dir_problems == &[Problem::NotADirectory]),
        ),
        (
            "ファイルが無い",
            |ops| {
                ops.files.clear();
            },
            |e| matches!(e, ConfigError::FileMissing { env: false, .. }),
        ),
        (
            "ファイルを開けない（拒否）",
            |ops| {
                ops.files.insert(sink_file(), Err(OpenError::Denied));
            },
            |e| matches!(e, ConfigError::AccessDenied { path } if path == &sink_file()),
        ),
        (
            "ファイルが別のプロセスで書き込みに開かれている（共有違反）",
            |ops| {
                ops.files.insert(sink_file(), Err(OpenError::InUse));
            },
            |e| matches!(e, ConfigError::FileInUse { path } if path == &sink_file()),
        ),
        (
            "ファイルの所有者が利用者（メモ帳で編集した管理者）",
            |ops| {
                ops.file_mut().security.dacl.owner = ALICE.to_string();
            },
            |e| matches!(e, ConfigError::Unprotected { dir_problems, file_problems, .. } if dir_problems.is_empty() && file_problems == &[Problem::Exposure(Exposure::ForeignOwner(ALICE.to_string()))]),
        ),
        (
            "ファイルを Users が読める（継承の ACE も）",
            |ops| {
                let mut ace = allows(USERS);
                ace.flags = 0x10;
                ops.file_mut()
                    .security
                    .dacl
                    .aces
                    .as_mut()
                    .unwrap()
                    .push(ace);
            },
            |e| matches!(e, ConfigError::Unprotected { file_problems, .. } if matches!(&file_problems[..], [Problem::Exposure(Exposure::AllowsOther { sid, .. })] if sid == USERS)),
        ),
        (
            "ファイルが null DACL",
            |ops| {
                ops.file_mut().security.dacl.aces = None;
            },
            |e| matches!(e, ConfigError::Unprotected { file_problems, .. } if file_problems == &[Problem::Exposure(Exposure::NullDacl)]),
        ),
        (
            "ファイルがハードリンク",
            |ops| {
                ops.file_mut().security.links = 2;
            },
            |e| matches!(e, ConfigError::Unprotected { file_problems, .. } if file_problems == &[Problem::Exposure(Exposure::HardLinked(2))]),
        ),
        (
            "ファイルがシンボリックリンク",
            |ops| {
                ops.file_mut().security.is_reparse_point = true;
            },
            |e| matches!(e, ConfigError::Unprotected { file_problems, .. } if file_problems == &[Problem::Exposure(Exposure::ReparsePoint)]),
        ),
        (
            "ファイルがフォルダ",
            |ops| {
                ops.file_mut().security.is_dir = true;
            },
            |e| matches!(e, ConfigError::Unprotected { file_problems, .. } if file_problems == &[Problem::IsADirectory]),
        ),
        (
            "ファイルの本当のパスが違う",
            |ops| {
                ops.file_mut().real = Some(abs("Users/evil/banto-hub-sink.toml"));
            },
            |e| matches!(e, ConfigError::Unprotected { file_problems, .. } if matches!(file_problems[..], [Problem::RealPathDiffers(_)])),
        ),
        (
            "置き場もファイルも保護されていない（全部の理由を出す）",
            |ops| {
                ops.dir_mut().security.dacl.owner = ALICE.to_string();
                ops.file_mut().security.links = 3;
            },
            |e| matches!(e, ConfigError::Unprotected { dir_problems, file_problems, .. } if dir_problems.len() == 1 && file_problems.len() == 1),
        ),
    ];
    for (name, setup, check) in cases {
        let mut ops = FakeOps::new();
        setup(&mut ops);
        let got = read(&ops);
        match &got {
            Err(error) => assert!(check(error), "{name}: {error:?}"),
            Ok(_) => panic!("{name}: 断らずに読んだ"),
        }
        assert!(
            !ops.calls().iter().any(|c| c.starts_with("read ")),
            "{name}: 中身を読んだ: {:?}",
            ops.calls()
        );
    }
}

/// 旧い置き場にファイルがあれば、置き場を開きもしない。
#[test]
fn a_legacy_file_stops_before_opening_the_place() {
    let mut ops = FakeOps::new();
    ops.existing.insert(legacy_root(), Ok(true));
    assert!(read(&ops).is_err());
    assert!(
        !ops.calls().iter().any(|c| c.starts_with("pin ")),
        "{:?}",
        ops.calls()
    );
}

/// 読めない・UTF-8 でないものは、空に潰さずに理由を返す。
#[test]
fn unreadable_or_non_utf8_contents_are_reported() {
    let mut ops = FakeOps::new();
    ops.contents = Err("大きすぎます".to_string());
    assert!(matches!(read(&ops), Err(ConfigError::Unverifiable { .. })));
    let mut ops = FakeOps::new();
    ops.contents = Ok(vec![0xff, 0xfe]);
    assert!(matches!(read(&ops), Err(ConfigError::Parse { .. })));
}

/// 環境変数で指したファイルにも、同じ確かめをかける（案内の文言は変わる）。
#[test]
fn an_env_override_gets_the_same_checks() {
    let file = abs("work/dev/sink.toml");
    let paths = decide_paths(
        RunMode::Console,
        Some(file.clone().into_os_string()),
        Ok(pd()),
        Some(&exe_dir()),
    )
    .unwrap();
    let mut ops = FakeOps::new();
    ops.dirs.insert(abs("work/dev"), Ok(Entry::dir()));
    let mut entry = Entry::file();
    entry.security.dacl.owner = ALICE.to_string();
    ops.files.insert(file.clone(), Ok(entry));
    let got = read_protected(&ops, &paths);
    match &got {
        Err(error @ ConfigError::Unprotected { env: true, .. }) => {
            let text = error.to_string();
            assert!(text.contains(ENV_CONFIG_PATH), "{text}");
            assert!(!text.contains(PREPARE_COMMAND), "{text}");
        }
        other => panic!("{other:?}"),
    }
    // 保護されていれば読む。旧い置き場の確かめも同じ。
    ops.files.insert(file.clone(), Ok(Entry::file()));
    assert_eq!(read_protected(&ops, &paths).unwrap().0, file);
    ops.existing.insert(legacy_exe(), Ok(true));
    assert!(matches!(
        read_protected(&ops, &paths),
        Err(ConfigError::LegacyFound { .. })
    ));
}

// --- 文言 ----------------------------------------------------------------------------

/// 理由ごとに 1 行、`prepare-sink-config` の案内、キーが漏れた可能性があるときだけ失効の案内。
/// パス・SID は出る。
#[test]
fn unprotected_lines_have_one_reason_per_line_and_the_prepare_guide() {
    let error = ConfigError::Unprotected {
        dir: sink_dir(),
        dir_problems: vec![Problem::Exposure(Exposure::ForeignOwner(ALICE.to_string()))],
        file: sink_file(),
        file_problems: vec![
            Problem::Exposure(Exposure::HardLinked(2)),
            Problem::Exposure(Exposure::AllowsOther {
                ace_type: 0,
                flags: 0,
                mask: 0x1200A9,
                sid: USERS.to_string(),
            }),
        ],
        env: false,
    };
    let lines = error.lines();
    assert_eq!(lines.len(), 6, "{lines:#?}");
    assert!(lines[0].starts_with(&format!(
        "置き場 {} は保護されていません: ",
        sink_dir().display()
    )));
    assert!(lines[0].contains(ALICE));
    assert!(lines[1].contains("ハードリンク"));
    assert!(lines[2].contains(USERS));
    assert!(lines[3].contains("失効"), "{lines:#?}");
    assert!(lines[4].contains(PREPARE_COMMAND), "{lines:#?}");
    assert!(lines[5].contains("中身は読んでいません"), "{lines:#?}");

    // 所有者だけ（管理者がメモ帳で編集した後）なら、失効の案内は出さない。
    let error = ConfigError::Unprotected {
        dir: sink_dir(),
        dir_problems: vec![],
        file: sink_file(),
        file_problems: vec![Problem::Exposure(Exposure::ForeignOwner(ALICE.to_string()))],
        env: false,
    };
    assert!(!error.lines().iter().any(|l| l.contains("失効")));
}

#[test]
fn legacy_lines_guide_the_move_and_the_reissue() {
    let error = ConfigError::LegacyFound {
        paths: vec![legacy_root(), legacy_exe()],
        new_place: Some(sink_file()),
    };
    let lines = error.lines();
    assert!(lines[0].contains(&legacy_root().display().to_string()));
    assert!(lines[1].contains(&legacy_exe().display().to_string()));
    let rest = lines[2..].join("\n");
    assert!(rest.contains(&sink_file().display().to_string()), "{rest}");
    assert!(rest.contains(PREPARE_COMMAND), "{rest}");
    assert!(rest.contains("失効"), "{rest}");
    assert!(rest.contains("消してください"), "{rest}");
    assert!(rest.contains("起動を止めます"), "{rest}");
}

#[test]
fn missing_lines_guide_the_prepare_command() {
    for error in [
        ConfigError::PlaceMissing {
            dir: sink_dir(),
            env: false,
        },
        ConfigError::FileMissing {
            path: sink_file(),
            env: false,
        },
    ] {
        let text = error.to_string();
        assert!(text.contains(PREPARE_COMMAND), "{text}");
        assert!(text.contains(".example"), "{text}");
    }
}

#[test]
fn every_exposure_has_a_line() {
    let all = [
        Exposure::ReparsePoint,
        Exposure::HardLinked(2),
        Exposure::ForeignOwner(ALICE.to_string()),
        Exposure::NullDacl,
        Exposure::AllowsOther {
            ace_type: 0,
            flags: 0,
            mask: 1,
            sid: USERS.to_string(),
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
    ];
    let texts: Vec<String> = all.iter().map(exposure_text).collect();
    for (found, text) in all.iter().zip(&texts) {
        assert!(!text.is_empty(), "{found:?}");
    }
    let mut unique = texts.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), texts.len(), "{texts:#?}");
}

// --- Windows の実物（一時フォルダ） ---------------------------------------------------

/// 本物の部品（`WindowsPlaceOps`）で、一時フォルダの置き場を確かめる。一時パスは本当のパス
/// （長い名前）で組み立てる（CI の TEMP は`RUNNER~1`の短い名前で、本当のパスの比較が
/// 食い違う - #622）。保護された置き場から読むのは管理者でしか作れないので CI で確かめる。
#[cfg(windows)]
mod windows {
    use std::path::{Path, PathBuf};

    use banto_secret_store::acl::{pin_dir, Exposure, Recurse};
    use banto_secret_store::DirAccess;

    use super::super::win::WindowsPlaceOps;
    use super::super::{read_protected, Paths, Problem, Source};
    use crate::config::ConfigError;

    /// 昇格した管理者か。`CI`があるのに管理者でなければ panic（管理者でしか走らない確かめが
    /// 黙って飛ばされないように。#626 PR-B の監査 B-3 と同じ）。
    fn is_elevated_admin(test: &str) -> bool {
        let admin = banto_service_elev::is_current_process_admin().unwrap_or(false);
        if !admin && std::env::var_os("CI").is_some() {
            panic!("{test}: CI なのに昇格した管理者ではない");
        }
        if !admin {
            eprintln!("skip {test}: 保護された置き場は管理者でしか作れない（CI で確かめる）");
        }
        admin
    }

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

    fn paths(dir: &Path, legacy: Vec<PathBuf>) -> Paths {
        Paths {
            dir: dir.to_path_buf(),
            file_name: "banto-hub-sink.toml".to_string(),
            source: Source::Default,
            default_file: Some(dir.join("banto-hub-sink.toml")),
            legacy,
        }
    }

    const TOML: &[u8] = b"hub_url = \"http://127.0.0.1:8722\"\napi_key = \"bh_dummy.key\"\n";

    /// 今のルートと同じ「継承の DACL」の置き場・ファイルは、中身を読まずに断る（所有者が
    /// 利用者・利用者への許可）。管理者でなくても走る。
    #[test]
    fn an_inherited_place_and_file_are_refused() {
        let base = tempfile::tempdir().unwrap();
        let dir = long(base.path()).join("sink");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("banto-hub-sink.toml"), TOML).unwrap();
        let got = read_protected(&WindowsPlaceOps, &paths(&dir, vec![]));
        match got {
            Err(ConfigError::Unprotected {
                dir_problems,
                file_problems,
                ..
            }) => {
                assert!(
                    dir_problems
                        .iter()
                        .any(|p| matches!(p, Problem::Exposure(Exposure::AllowsOther { .. }))),
                    "{dir_problems:?}"
                );
                assert!(
                    file_problems
                        .iter()
                        .any(|p| matches!(p, Problem::Exposure(Exposure::AllowsOther { .. }))),
                    "{file_problems:?}"
                );
            }
            other => panic!("{other:?}"),
        }
    }

    /// 置き場がジャンクションなら、先を開かずに断る。
    #[test]
    fn a_junction_place_is_refused_without_following() {
        let base = tempfile::tempdir().unwrap();
        let base = long(base.path());
        let target = base.join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("banto-hub-sink.toml"), TOML).unwrap();
        let link = base.join("sink");
        junction(&link, &target);
        let got = read_protected(&WindowsPlaceOps, &paths(&link, vec![]));
        assert!(
            matches!(
                &got,
                Err(ConfigError::Unprotected { dir_problems, file_problems, .. })
                    if dir_problems.contains(&Problem::Exposure(Exposure::ReparsePoint))
                        && file_problems.is_empty()
            ),
            "{got:?}"
        );
        std::fs::remove_dir(&link).unwrap();
    }

    /// 途中の要素がジャンクションなら、本当のパスが違うので断る。
    #[test]
    fn a_junction_in_the_middle_is_refused() {
        let base = tempfile::tempdir().unwrap();
        let base = long(base.path());
        let real = base.join("real");
        std::fs::create_dir_all(real.join("sink")).unwrap();
        std::fs::write(real.join("sink").join("banto-hub-sink.toml"), TOML).unwrap();
        let mid = base.join("mid");
        junction(&mid, &real);
        let got = read_protected(&WindowsPlaceOps, &paths(&mid.join("sink"), vec![]));
        assert!(
            matches!(
                &got,
                Err(ConfigError::Unprotected { dir_problems, .. })
                    if dir_problems.iter().any(|p| matches!(p, Problem::RealPathDiffers(_)))
            ),
            "{got:?}"
        );
        std::fs::remove_dir(&mid).unwrap();
    }

    /// 旧い置き場にファイルがあれば、置き場を開かずに断る（旧いファイルは消さない・動かさない）。
    #[test]
    fn a_legacy_file_is_refused_and_left_alone() {
        let base = tempfile::tempdir().unwrap();
        let base = long(base.path());
        let legacy = base.join("banto-hub-sink.toml");
        std::fs::write(&legacy, TOML).unwrap();
        let got = read_protected(
            &WindowsPlaceOps,
            &paths(&base.join("sink"), vec![legacy.clone()]),
        );
        assert!(
            matches!(&got, Err(ConfigError::LegacyFound { paths, .. }) if paths == std::slice::from_ref(&legacy)),
            "{got:?}"
        );
        assert_eq!(
            std::fs::read(&legacy).unwrap(),
            TOML,
            "旧いファイルはそのまま"
        );
    }

    /// ハードリンクのファイルは断る（置き場が保護されていなくても、理由に入る）。
    #[test]
    fn a_hardlinked_file_is_refused() {
        let base = tempfile::tempdir().unwrap();
        let base = long(base.path());
        let dir = base.join("sink");
        std::fs::create_dir(&dir).unwrap();
        let outside = base.join("outside.toml");
        std::fs::write(&outside, TOML).unwrap();
        std::fs::hard_link(&outside, dir.join("banto-hub-sink.toml")).unwrap();
        let got = read_protected(&WindowsPlaceOps, &paths(&dir, vec![]));
        assert!(
            matches!(
                &got,
                Err(ConfigError::Unprotected { file_problems, .. })
                    if file_problems.contains(&Problem::Exposure(Exposure::HardLinked(2)))
            ),
            "{got:?}"
        );
    }

    /// 別のプロセスが設定ファイルを書き込みに開いていれば、開くこと自体が共有違反で失敗し、
    /// 「書き込みに開かれています」として起動しない（`open_child_file` の共有は読み取りだけ -
    /// #648 の P1）。閉じれば、次は保護の確かめまで進む。管理者でなくても走る。
    #[test]
    fn a_file_open_for_writing_is_refused() {
        let base = tempfile::tempdir().unwrap();
        let base = long(base.path());
        let dir = base.join("sink");
        std::fs::create_dir(&dir).unwrap();
        let file = dir.join("banto-hub-sink.toml");
        std::fs::write(&file, TOML).unwrap();
        let writer = std::fs::OpenOptions::new().write(true).open(&file).unwrap();
        let got = read_protected(&WindowsPlaceOps, &paths(&dir, vec![]));
        assert!(
            matches!(&got, Err(ConfigError::FileInUse { path }) if path == &file),
            "{got:?}"
        );
        assert!(
            got.as_ref()
                .unwrap_err()
                .to_string()
                .contains("書き込みに開かれています"),
            "{got:?}"
        );
        drop(writer);
        let got = read_protected(&WindowsPlaceOps, &paths(&dir, vec![]));
        assert!(
            matches!(&got, Err(ConfigError::Unprotected { .. })),
            "{got:?}"
        );
    }

    /// `prepare-sink-config` と同じ形（SYSTEM・Administrators だけ・所有者 Administrators）の
    /// 置き場とファイルは読める。管理者でしか作れない（CI）。
    #[test]
    fn a_protected_place_is_read() {
        if !is_elevated_admin("a_protected_place_is_read") {
            return;
        }
        let base = tempfile::tempdir().unwrap();
        let base = long(base.path());
        let parent = pin_dir(&base).unwrap().unwrap();
        let sink = parent
            .create_child_dir("sink", Some(&DirAccess::service_only()))
            .unwrap()
            .unwrap();
        sink.reset_acl(&DirAccess::service_only(), Recurse::ObjectOnly)
            .unwrap();
        drop(
            sink.create_child_file("banto-hub-sink.toml", TOML)
                .unwrap()
                .unwrap(),
        );
        let dir = base.join("sink");
        drop((sink, parent));
        let (path, text) = read_protected(&WindowsPlaceOps, &paths(&dir, vec![])).unwrap();
        assert_eq!(path, dir.join("banto-hub-sink.toml"));
        assert_eq!(text.as_bytes(), TOML);
        let config = crate::config::SidecarConfig::from_toml_str(&text, &path).unwrap();
        assert_eq!(config.hub_url, "http://127.0.0.1:8722");
    }
}
