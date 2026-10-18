//! [`super`] の純関数と [`Mode::Plain`] のテスト（全 OS）。Windows の実物（Guarded）は
//! `win_tests`。

use super::*;

/// 8.3 の短い名前（作らないボリュームでは `None`）。Windows のテストの共有の部品。
#[cfg(windows)]
pub(crate) fn short_path(long: &Path) -> Option<PathBuf> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;
    let wide: Vec<u16> = long
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut buf = vec![0u16; 1024];
    // SAFETY: `wide` は NUL 終端、`buf` は書ける長さの領域。
    let len =
        unsafe { GetShortPathNameW(wide.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) } as usize;
    assert!(len > 0 && len < buf.len(), "GetShortPathNameW");
    let short = PathBuf::from(std::ffi::OsString::from_wide(&buf[..len]));
    (short != long).then_some(short)
}

fn spec(key: &'static str, parent: Option<&'static str>, name: &'static str) -> ManagedSpec {
    ManagedSpec {
        key,
        parent,
        name,
        create: true,
    }
}

#[test]
fn check_specs_table() {
    let ok = [
        spec("data", None, "data"),
        spec("backups", None, "backups"),
        spec("backups-db", Some("backups"), "chronogazer.sqlite3"),
    ];
    assert_eq!(check_specs(&ok), Ok(()));
    assert_eq!(check_specs(&[]), Ok(()));

    let cases: Vec<(Vec<ManagedSpec>, &str, SpecProblem)> = vec![
        (
            vec![spec("a", None, "a"), spec("a", None, "b")],
            "a",
            SpecProblem::DuplicateKey,
        ),
        (
            vec![
                spec("child", Some("parent"), "c"),
                spec("parent", None, "p"),
            ],
            "child",
            SpecProblem::UnknownParent("parent"),
        ),
        (
            vec![spec("a", Some("nope"), "a")],
            "a",
            SpecProblem::UnknownParent("nope"),
        ),
        (
            vec![spec("self", Some("self"), "a")],
            "self",
            SpecProblem::UnknownParent("self"),
        ),
    ];
    for (specs, key, problem) in cases {
        assert_eq!(
            check_specs(&specs),
            Err(ManagedStop {
                key,
                reason: ManagedRefusal::Spec(problem),
            }),
            "{specs:?}"
        );
    }
    for bad in [
        "", ".", "..", "a/b", r"a\b", "C:", "x:y", "nul", "COM1.txt", "a.", "a ", "*", "a?",
    ] {
        assert_eq!(
            check_specs(&[spec("k", None, bad)]),
            Err(ManagedStop {
                key: "k",
                reason: ManagedRefusal::Spec(SpecProblem::InvalidName),
            }),
            "{bad:?}"
        );
    }
}

#[test]
fn owner_allowed_table() {
    let user = InstallUserSid::parse("S-1-5-21-1-2-3-1001").unwrap();
    let other = "S-1-5-21-1-2-3-1002";
    assert!(owner_allowed(SYSTEM_SID, &[]));
    assert!(owner_allowed(ADMINISTRATORS_SID, &[]));
    assert!(owner_allowed(user.as_str(), std::slice::from_ref(&user)));
    assert!(!owner_allowed(user.as_str(), &[]));
    assert!(!owner_allowed(other, &[user]));
    assert!(!owner_allowed("S-1-5-32-545", &[])); // Users
}

#[test]
fn temp_names_are_plain_and_bounded() {
    let name = temp_name("endpoint.json", 0x1234_abcd).unwrap();
    assert_eq!(name, ".endpoint.json.tmp-1234abcd");
    assert!(guard::is_plain_child_name(&name));
    assert_ne!(temp_name("a", 1), temp_name("a", 2));
    // 一時名が要素の上限（255）に収まる最長の名前まで。
    let fits = "x".repeat(MAX_NAME_UNITS - ".".len() - ".tmp-00000000".len());
    assert!(temp_name(&fits, 0).is_some());
    let too_long = format!("{fits}x");
    assert_eq!(temp_name(&too_long, 0), None);
    for bad in ["", "a/b", "..", "nul", "a."] {
        assert_eq!(temp_name(bad, 0), None, "{bad:?}");
    }
}

#[test]
fn removal_candidates_are_plain_files_chosen_by_name() {
    let pred = |name: &str| name.ends_with(".log");
    let entry = |name: &str, kind, tag| Entry {
        name: name.to_string(),
        kind,
        reparse_tag: tag,
        len: 0,
    };
    assert!(is_removal_candidate(
        &entry("a.log", EntryKind::File, None),
        &pred
    ));
    assert!(!is_removal_candidate(
        &entry("a.txt", EntryKind::File, None),
        &pred
    ));
    assert!(!is_removal_candidate(
        &entry("d.log", EntryKind::Dir, None),
        &pred
    ));
    assert!(!is_removal_candidate(
        &entry("j.log", EntryKind::Dir, Some(0xA000_0003)),
        &pred
    ));
    assert!(!is_removal_candidate(
        &entry("s.log", EntryKind::File, Some(0xA000_000C)),
        &pred
    ));
}

#[test]
fn root_form_problems() {
    assert_eq!(
        root_form_problem(Path::new("")),
        Some(RootFormProblem::Empty)
    );
    assert_eq!(
        root_form_problem(Path::new("relative")),
        Some(RootFormProblem::NotAbsolute)
    );
    let base = std::env::temp_dir();
    assert_eq!(
        root_form_problem(&base.join("..").join("x")),
        Some(RootFormProblem::DotSegment)
    );
    assert_eq!(root_form_problem(&base.join("x")), None);
}

/// 型で約束する: 握ったルート・フォルダはスレッドをまたいで持てる（サービスがプロセスの終わり
/// まで持つ）。
#[test]
fn handles_are_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<SafeRoot>();
    assert_send_sync::<ManagedDir>();
    assert_send_sync::<ManagedSet>();
}

fn plain_spec(expected: PathBuf) -> RootSpec {
    RootSpec {
        expected,
        known: KnownFolders::default(),
        owners: Vec::new(),
        create_access: None,
        create: true,
        mode: Mode::Plain,
    }
}

#[cfg(not(windows))]
#[test]
fn guarded_is_unsupported_off_windows() {
    let dir = tempfile::tempdir().unwrap();
    let mut spec = plain_spec(dir.path().join("root"));
    spec.mode = Mode::Guarded;
    assert_eq!(
        open_root(&spec).unwrap_err(),
        RootRefusal::UnsupportedPlatform
    );
    assert!(!dir.path().join("root").exists(), "何も作らない");
}

#[test]
fn plain_root_and_managed_dirs_behave_like_std_fs() {
    use std::io::{Read, Write};
    let dir = tempfile::tempdir().unwrap();
    let absent = plain_spec(dir.path().join("missing"));
    let refused = open_root(&RootSpec {
        create: false,
        ..absent
    })
    .unwrap_err();
    assert_eq!(refused, RootRefusal::Absent);

    let root = open_root(&plain_spec(dir.path().join("root"))).unwrap();
    assert!(!root.as_dir().is_guarded());
    // `canonicalize` の結果から verbatim の `\\?\` を外した形（Guarded の本当のパスと同じ形。
    // #677 のレビュー N-3）。非 Windows では `canonicalize` そのまま。
    assert_eq!(
        root.final_path(),
        crate::nofollow::normalize_expected(
            &std::fs::canonicalize(dir.path().join("root")).unwrap()
        )
    );
    assert!(!root.final_path().to_string_lossy().starts_with(r"\\?\"));
    let set = root
        .open_managed(&[
            spec("logs", None, "logs"),
            spec("backups", None, "backups"),
            spec("backups-db", Some("backups"), "db"),
        ])
        .unwrap();
    assert!(dir.path().join("root").join("backups").join("db").is_dir());
    assert_eq!(
        set.keys().collect::<Vec<_>>(),
        ["logs", "backups", "backups-db"]
    );
    assert_eq!(set.get("nope").unwrap_err(), UnknownKey("nope".into()));
    let logs = set.get("logs").unwrap();
    assert_eq!(logs.path(), dir.path().join("root").join("logs"));

    // 作らない spec の無いフォルダは止まる。
    let stop = root
        .open_managed(&[ManagedSpec {
            create: false,
            ..spec("x", None, "absent")
        }])
        .unwrap_err();
    assert_eq!(stop.reason, ManagedRefusal::Absent);

    let mut probe = logs.create_new("probe").unwrap().expect("作れる");
    probe.write_all(b"p").unwrap();
    drop(probe);
    assert!(
        logs.create_new("probe").unwrap().is_none(),
        "既にあれば None"
    );
    assert!(logs.remove("probe").unwrap());
    assert!(!logs.remove("probe").unwrap(), "無ければ false");

    let mut log = logs.open_append("a.log", true).unwrap();
    log.write_all(b"one\n").unwrap();
    drop(log);
    let mut log = logs.open_append("a.log", false).unwrap();
    log.write_all(b"two\n").unwrap();
    drop(log);
    let mut text = String::new();
    logs.open_read("a.log")
        .unwrap()
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    assert_eq!(text, "one\ntwo\n");
    assert!(logs.open_read("none").unwrap().is_none());
    assert!(matches!(
        logs.open_append("none", false).unwrap_err(),
        SafeRootError::Io {
            kind: std::io::ErrorKind::NotFound,
            ..
        }
    ));

    logs.write_atomic("state.json", b"v1").unwrap();
    logs.write_atomic("state.json", b"v2").unwrap();
    assert_eq!(
        std::fs::read(logs.final_path().join("state.json")).unwrap(),
        b"v2"
    );

    std::fs::write(logs.final_path().join("old.log"), b"x").unwrap();
    std::fs::create_dir(logs.final_path().join("dir.log")).unwrap();
    let report = logs.remove_where(&|n| n.ends_with(".log")).unwrap();
    let mut removed = report.removed.clone();
    removed.sort();
    assert_eq!(removed, ["a.log", "old.log"]);
    assert!(report.refused.is_empty() && report.failed.is_empty());
    let mut names: Vec<_> = logs.list().unwrap().into_iter().map(|e| e.name).collect();
    names.sort();
    assert_eq!(names, ["dir.log", "state.json"], "一時ファイルも残らない");

    let reopen = root
        .as_dir()
        .reopen_path("db.sqlite3", &["-wal", "-shm"])
        .unwrap();
    assert_eq!(reopen.path(), root.final_path().join("db.sqlite3"));
    assert_eq!(
        reopen.companions(),
        [
            root.final_path().join("db.sqlite3-wal"),
            root.final_path().join("db.sqlite3-shm")
        ]
    );
    // 名前は Plain でも 1 要素だけ（区切り・`..` を通さない）。
    assert!(root.as_dir().reopen_path("../x", &[]).is_err());
    assert!(root.as_dir().create_new("a/b").is_err());
    assert!(root.as_dir().write_atomic("..", b"x").is_err());
}

/// spec の並びが誤っていれば、前に並べた正しい spec も含めて何も作らない（Fable B-8-1 の
/// 「確かめに落ちたら何も作らない」を、spec の段で固定する）。
#[test]
fn an_invalid_spec_list_creates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = open_root(&plain_spec(dir.path().join("root"))).unwrap();
    let stop = root
        .open_managed(&[spec("data", None, "data"), spec("bad", Some("none"), "b")])
        .unwrap_err();
    assert_eq!(stop.key, "bad");
    assert!(
        std::fs::read_dir(root.final_path())
            .unwrap()
            .next()
            .is_none(),
        "spec の誤りの前に作った"
    );
}

/// 一時名の衝突: nonce を進めて成功する。16 個すべて埋まっていれば断り、何も置き換えない。
#[test]
fn write_atomic_advances_the_nonce_on_collisions_and_gives_up_at_the_limit() {
    let dir = tempfile::tempdir().unwrap();
    let root = open_root(&plain_spec(dir.path().join("root"))).unwrap();
    collisions_on(root.as_dir());
}

/// [`write_atomic_advances_the_nonce_on_collisions_and_gives_up_at_the_limit`] の本体（Guarded
/// でも同じことを確かめる）。
pub(crate) fn collisions_on(dir: &ManagedDir) {
    const SEED: u32 = 0x5eed_0000;
    TEST_NONCE_SEED.with(|s| s.set(Some(SEED)));
    let taken = |n: u32| {
        dir.final_path()
            .join(temp_name("t.json", SEED + n).unwrap())
    };
    for n in 0..3 {
        std::fs::write(taken(n), b"squatter").unwrap();
    }
    dir.write_atomic("t.json", b"v1").unwrap();
    assert_eq!(
        std::fs::read(dir.final_path().join("t.json")).unwrap(),
        b"v1"
    );
    for n in 0..3 {
        assert_eq!(
            std::fs::read(taken(n)).unwrap(),
            b"squatter",
            "衝突した名前を上書きした"
        );
    }
    for n in 3..TEMP_TRIES {
        std::fs::write(taken(n), b"squatter").unwrap();
    }
    let err = dir.write_atomic("t.json", b"v2").unwrap_err();
    TEST_NONCE_SEED.with(|s| s.set(None));
    assert!(
        matches!(
            err,
            SafeRootError::Io {
                op: IoOp::CreateFile,
                kind: std::io::ErrorKind::AlreadyExists,
                ..
            }
        ),
        "{err:?}"
    );
    assert_eq!(
        std::fs::read(dir.final_path().join("t.json")).unwrap(),
        b"v1",
        "上限で断ったのに置き換えた"
    );
}

/// 宛先がフォルダなら名前の付け替えは失敗し、一時ファイルを残さない（Plain の `rename` も同じ）。
#[test]
fn write_atomic_onto_a_directory_fails_without_leaving_a_temp_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = open_root(&plain_spec(dir.path().join("root"))).unwrap();
    std::fs::create_dir(root.final_path().join("dest")).unwrap();
    std::fs::write(root.final_path().join("dest").join("keep"), b"k").unwrap();
    assert!(root.as_dir().write_atomic("dest", b"x").is_err());
    let names: Vec<_> = root
        .as_dir()
        .list()
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names, ["dest"], "一時ファイルが残った");
}

/// 名前に普通のファイルが既にあれば、Plain でもルート・管理フォルダとして開かない（確かめと
/// 作成の間に別のプロセスがファイルを作った競合の、結果が同じになる分岐）。
#[test]
fn a_plain_file_in_place_of_a_directory_is_refused_in_plain_mode() {
    let dir = tempfile::tempdir().unwrap();
    let file_root = dir.path().join("file-root");
    std::fs::write(&file_root, b"file").unwrap();
    assert_eq!(
        open_root(&plain_spec(file_root)).unwrap_err(),
        RootRefusal::Refused(Refusal::NotADirectory)
    );

    let root = open_root(&plain_spec(dir.path().join("root"))).unwrap();
    std::fs::write(root.final_path().join("data"), b"file").unwrap();
    for create in [true, false] {
        let stop = root
            .open_managed(&[ManagedSpec {
                create,
                ..spec("data", None, "data")
            }])
            .unwrap_err();
        assert_eq!(
            stop.reason,
            ManagedRefusal::Refused(Refusal::NotADirectory),
            "create={create}"
        );
    }
}

/// 最後の種類の確かめ（`create_dir` が `AlreadyExists` で通ったあと・`canonicalize` のあとに
/// 使う）はファイルを断り、ディレクトリを通し、無いものは I/O の失敗にする。
#[test]
fn the_final_directory_check_distinguishes_file_dir_and_missing() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("f");
    std::fs::write(&file, b"x").unwrap();
    assert!(plain::is_dir(dir.path()).unwrap());
    assert!(!plain::is_dir(&file).unwrap());
    assert!(plain::is_dir(&dir.path().join("absent")).is_err());
}
