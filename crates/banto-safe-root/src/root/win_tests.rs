//! [`super`] の Windows の実物テスト（[`Mode::Guarded`]）。ジャンクションとハードリンクは権限
//! なしで作れるので必ず走る。ディレクトリ・ファイルのシンボリックリンクは、作れなければ手元は
//! skip、`CI`（管理者のランナー）では落とす。外（ルートの外の一時フォルダ）に何も作らない・
//! 書かない・消さないことを、外の中身を前後で比べて確かめる。

use super::*;
use crate::acl::tests::windows::{junction, me, set_reparse_to};
use std::io::{Read, Write};

const VICTIM: &[u8] = b"outside content";
/// `IO_REPARSE_TAG_MOUNT_POINT`（ジャンクション）。
const TAG_MOUNT_POINT: u32 = 0xA000_0003;
/// `IO_REPARSE_TAG_SYMLINK`。
const TAG_SYMLINK: u32 = 0xA000_000C;

fn guarded(expected: PathBuf) -> RootSpec {
    RootSpec {
        expected,
        known: KnownFolders::default(),
        owners: vec![me()],
        create_access: None,
        create: true,
        mode: Mode::Guarded,
    }
}

/// 外のフォルダの中身（名前と中身）の写し。前後で比べる。
fn snapshot(dir: &Path) -> Vec<(String, Option<Vec<u8>>)> {
    let mut items: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            let name = e.file_name().to_string_lossy().into_owned();
            let data = std::fs::read(e.path()).ok();
            (name, data)
        })
        .collect();
    items.sort();
    items
}

fn outside_with_victim() -> (tempfile::TempDir, PathBuf) {
    let outside = tempfile::tempdir().unwrap();
    let victim = outside.path().join("victim.txt");
    std::fs::write(&victim, VICTIM).unwrap();
    (outside, victim)
}

/// シンボリックリンクを作る。作れなければ手元は `false`（skip）、`CI` では落とす。
fn try_symlink(make: impl FnOnce() -> std::io::Result<()>) -> bool {
    match make() {
        Ok(()) => true,
        Err(err) if std::env::var_os("CI").is_some() => {
            panic!("CI でシンボリックリンクを作れない: {err}")
        }
        Err(err) => {
            eprintln!("skip: シンボリックリンクを作れません（権限?）: {err}");
            false
        }
    }
}

/// フォルダへのリンクの作り方（ジャンクションは必ず、ディレクトリのシンボリックリンクは
/// 作れたときだけ）。
#[derive(Clone, Copy, Debug)]
enum DirLink {
    Junction,
    Symlink,
}

impl DirLink {
    const ALL: [DirLink; 2] = [DirLink::Junction, DirLink::Symlink];

    /// 作れたら `true`（シンボリックリンクを作れない手元は `false`）。
    fn make(self, link: &Path, target: &Path) -> bool {
        match self {
            DirLink::Junction => {
                junction(link, target);
                true
            }
            DirLink::Symlink => try_symlink(|| std::os::windows::fs::symlink_dir(target, link)),
        }
    }

    fn tag(self) -> u32 {
        match self {
            DirLink::Junction => TAG_MOUNT_POINT,
            DirLink::Symlink => TAG_SYMLINK,
        }
    }
}

fn refusal_of(err: &SafeRootError) -> &Refusal {
    match err {
        SafeRootError::Refused { refusal, .. } => refusal,
        other => panic!("Refused ではない: {other:?}"),
    }
}

#[test]
fn a_guarded_root_and_managed_dirs_work_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let root = open_root(&guarded(dir.path().join("root"))).unwrap();
    assert!(root.as_dir().is_guarded());
    assert!(root.pinned().is_some());
    let set = root
        .open_managed(&[
            spec("logs", None, "logs"),
            spec("run", None, "run"),
            spec("backups", None, "backups"),
            spec("backups-db", Some("backups"), "db"),
        ])
        .unwrap();
    assert!(dir.path().join("root").join("backups").join("db").is_dir());
    let logs = set.get("logs").unwrap();
    assert!(crate::guard::paths_match(
        logs.final_path(),
        &root.final_path().join("logs")
    ));

    let mut probe = logs.create_new("probe").unwrap().expect("作れる");
    probe.write_all(b"p").unwrap();
    drop(probe);
    assert!(logs.create_new("probe").unwrap().is_none());
    assert!(logs.remove("probe").unwrap());
    assert!(!logs.remove("probe").unwrap());
    assert!(!logs.final_path().join("probe").exists());

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
    assert_eq!(text, "one\ntwo\n", "追記は末尾に付く");
    assert!(logs.open_read("none").unwrap().is_none());
    assert!(logs.open_append("none", false).is_err());

    let run = set.get("run").unwrap();
    run.write_atomic("endpoint.json", b"v1").unwrap();
    run.write_atomic("endpoint.json", b"v2").unwrap();
    assert_eq!(
        std::fs::read(run.final_path().join("endpoint.json")).unwrap(),
        b"v2"
    );
    let names: Vec<_> = run.list().unwrap().into_iter().map(|e| e.name).collect();
    assert_eq!(names, ["endpoint.json"], "一時ファイルが残った");

    std::fs::write(logs.final_path().join("old.log"), b"x").unwrap();
    std::fs::create_dir(logs.final_path().join("dir.log")).unwrap();
    let report = logs.remove_where(&|n| n.ends_with(".log")).unwrap();
    let mut removed = report.removed.clone();
    removed.sort();
    assert_eq!(removed, ["a.log", "old.log"]);
    assert!(report.refused.is_empty() && report.failed.is_empty());
    let entries = logs.list().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].kind, EntryKind::Dir);
}

fn spec(key: &'static str, parent: Option<&'static str>, name: &'static str) -> ManagedSpec {
    ManagedSpec {
        key,
        parent,
        name,
        create: true,
    }
}

/// 作るときに付ける記述子（`create_access`）は、ルートにも管理するフォルダにも作るのと同時に
/// 付く。
#[test]
fn created_dirs_carry_the_create_access() {
    let dir = tempfile::tempdir().unwrap();
    let access = DirAccess::with_install_user(me());
    let root = open_root(&RootSpec {
        create_access: Some(access.clone()),
        ..guarded(dir.path().join("root"))
    })
    .unwrap();
    root.open_managed(&[spec("data", None, "data")]).unwrap();
    for path in [
        root.final_path().to_path_buf(),
        root.final_path().join("data"),
    ] {
        let dacl = crate::acl::read_dir_dacl(&path).unwrap();
        crate::acl::check_dacl(&dacl, &access).unwrap_or_else(|e| panic!("{path:?}: {e:?}"));
    }
}

/// ルートの途中の要素がリンク（ジャンクション・ディレクトリのシンボリックリンク）: 先に
/// ルートがあれば本当のパスの食い違いで断り、無ければ親がリンクなので作る前に断る。どちらも
/// 外に何も作らない。
#[test]
fn a_link_in_the_middle_of_the_root_path_is_refused_without_touching_the_outside() {
    for link in DirLink::ALL {
        let dir = tempfile::tempdir().unwrap();
        let (outside, _victim) = outside_with_victim();
        let via = dir.path().join("via");
        if !link.make(&via, outside.path()) {
            continue;
        }
        // (a) 無いルート: 親（via）がリンクなので作らない。
        let before = snapshot(outside.path());
        let refusal = open_root(&guarded(via.join("root"))).unwrap_err();
        assert!(
            matches!(
                refusal,
                RootRefusal::Parent(RootParentRefusal::NotAPlainDirectory)
            ),
            "{link:?}: {refusal:?}"
        );
        assert_eq!(snapshot(outside.path()), before, "{link:?}: 外に作った");

        // (b) 外に同じ名前のルートが既にある: 辿った先を開くが、本当のパスが違うので断る。
        std::fs::create_dir(outside.path().join("root")).unwrap();
        let refusal = open_root(&guarded(via.join("root"))).unwrap_err();
        assert!(
            matches!(refusal, RootRefusal::RealPath(RealPathRefusal::Mismatch)),
            "{link:?}: {refusal:?}"
        );
        assert!(
            std::fs::read_dir(outside.path().join("root"))
                .unwrap()
                .next()
                .is_none(),
            "{link:?}: 外のルートに作った"
        );
    }
}

/// ルートそのものがリンク: 辿らずにリンクそのものを開き、tag つきで断る。
#[test]
fn a_root_that_is_itself_a_link_is_refused_with_its_tag() {
    for link in DirLink::ALL {
        let dir = tempfile::tempdir().unwrap();
        let (outside, _victim) = outside_with_victim();
        let root = dir.path().join("root");
        if !link.make(&root, outside.path()) {
            continue;
        }
        let before = snapshot(outside.path());
        let refusal = open_root(&guarded(root)).unwrap_err();
        assert_eq!(
            refusal,
            RootRefusal::Refused(Refusal::ReparsePoint { tag: link.tag() }),
            "{link:?}"
        );
        assert_eq!(snapshot(outside.path()), before);
    }
}

/// 管理するフォルダがリンク・ファイル: 止まり、外に何も作らない。後ろの spec も開かない。
#[test]
fn a_managed_dir_that_is_a_link_or_a_file_stops_the_whole_set() {
    for link in DirLink::ALL {
        let dir = tempfile::tempdir().unwrap();
        let (outside, _victim) = outside_with_victim();
        let root = open_root(&guarded(dir.path().join("root"))).unwrap();
        if !link.make(&root.final_path().join("logs"), outside.path()) {
            continue;
        }
        let before = snapshot(outside.path());
        let stop = root
            .open_managed(&[spec("logs", None, "logs"), spec("after", None, "after")])
            .unwrap_err();
        assert_eq!(
            stop,
            ManagedStop {
                key: "logs",
                reason: ManagedRefusal::Refused(Refusal::ReparsePoint { tag: link.tag() }),
            },
            "{link:?}"
        );
        assert_eq!(snapshot(outside.path()), before, "{link:?}");
        assert!(
            !root.final_path().join("after").exists(),
            "後ろの spec を作った"
        );
    }
    let dir = tempfile::tempdir().unwrap();
    let root = open_root(&guarded(dir.path().join("root"))).unwrap();
    std::fs::write(root.final_path().join("data"), b"file").unwrap();
    let stop = root
        .open_managed(&[spec("data", None, "data")])
        .unwrap_err();
    assert_eq!(stop.reason, ManagedRefusal::Refused(Refusal::NotADirectory));
}

/// spec の並びの誤りは、何かを開く・作る前に断る（Guarded でも）。
#[test]
fn an_invalid_spec_list_creates_nothing_when_guarded() {
    let dir = tempfile::tempdir().unwrap();
    let root = open_root(&guarded(dir.path().join("root"))).unwrap();
    let stop = root
        .open_managed(&[spec("data", None, "data"), spec("x", None, "nul")])
        .unwrap_err();
    assert_eq!(stop.reason, ManagedRefusal::Spec(SpecProblem::InvalidName));
    assert!(std::fs::read_dir(root.final_path())
        .unwrap()
        .next()
        .is_none());
}

/// フォルダの所有者を今の利用者の SID に明示的に設定する（`SetNamedSecurityInfoW`）。管理者が
/// 作ったフォルダの所有者は Administrators になるので、「許さない所有者」の確かめ（利用者
/// 本人を許さない spec）のために、所有者を利用者本人へ移す。利用者自身の SID へは、昇格した
/// 管理者なら移せる。
pub(crate) fn set_owner_to_me(path: &Path) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{LocalFree, ERROR_SUCCESS};
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSidToSidW, SetNamedSecurityInfoW, SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::OWNER_SECURITY_INFORMATION;

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let sid_text: Vec<u16> = me().as_str().encode_utf16().chain(Some(0)).collect();
    let mut sid = std::ptr::null_mut();
    // SAFETY: NUL 終端の SID 文字列。`sid` は LocalAlloc され、下で LocalFree する。
    assert_ne!(
        unsafe { ConvertStringSidToSidW(sid_text.as_ptr(), &mut sid) },
        0,
        "ConvertStringSidToSidW"
    );
    // SAFETY: NUL 終端のパスと有効な SID。所有者だけを書く。
    let status = unsafe {
        SetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            sid,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    // SAFETY: ConvertStringSidToSidW が LocalAlloc した SID。
    unsafe { LocalFree(sid) };
    assert_eq!(status, ERROR_SUCCESS, "SetNamedSecurityInfoW(owner)");
}

/// 所有者が SYSTEM・Administrators・指定の利用者のどれでもなければ断る（ここでは今の利用者を
/// 許さない spec で、今の利用者が所有するフォルダを開く）。管理者（CI）が作ったフォルダの
/// 所有者は Administrators になり許されるので、そこは別に確かめたうえで、所有者を今の利用者へ
/// 明示的に移してから「許さない所有者は断る」を確かめる。
#[test]
fn a_root_owned_by_someone_not_allowed_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir(&root).unwrap();
    let spec = RootSpec {
        owners: Vec::new(),
        ..guarded(root.clone())
    };
    if crate::acl::tests::windows::is_elevated_admin() {
        // 管理者が作ったフォルダの所有者は Administrators になりうる（「作成者を所有者に」の
        // 方針しだい）。Administrators なら許す。
        match open_root(&spec) {
            Ok(opened) => drop(opened),
            Err(refusal) => {
                assert_eq!(
                    refusal,
                    RootRefusal::ForeignOwner(me().as_str().to_string()),
                    "Administrators 以外の所有者なら今の利用者のはず"
                );
            }
        }
        set_owner_to_me(&root);
    }
    let refusal = open_root(&spec).unwrap_err();
    assert_eq!(
        refusal,
        RootRefusal::ForeignOwner(me().as_str().to_string())
    );
    // 本人を許す spec なら開ける（所有者の確かめだけが断る理由だったこと）。
    open_root(&guarded(root)).unwrap();
}

/// 【要実測 → 実測】握っている間は、ルートの**祖先**の名前の変更も断られる（Fable B-4-1）。
/// 放せばできる。
#[test]
fn an_ancestor_of_a_held_root_cannot_be_renamed() {
    let dir = tempfile::tempdir().unwrap();
    let ancestor = dir.path().join("a");
    std::fs::create_dir_all(ancestor.join("mid")).unwrap();
    let root = open_root(&guarded(ancestor.join("mid").join("root"))).unwrap();
    let moved = dir.path().join("b");
    let err = std::fs::rename(&ancestor, &moved).expect_err("握っている間に祖先の名前を変えられた");
    eprintln!("note: 祖先の名前の変更の失敗: {err:?}");
    let err = std::fs::rename(ancestor.join("mid"), ancestor.join("mid2"))
        .expect_err("握っている間に親の名前を変えられた");
    eprintln!("note: 親の名前の変更の失敗: {err:?}");
    drop(root);
    std::fs::rename(&ancestor, &moved).unwrap();
}

/// ファイルの名前がリンク・ハードリンク: 追記・読み取りは開いた後のハンドルで断り、外は変わらない。
#[test]
fn file_links_are_refused_by_append_and_read() {
    let dir = tempfile::tempdir().unwrap();
    let (outside, victim) = outside_with_victim();
    let root = open_root(&guarded(dir.path().join("root"))).unwrap();
    let d = root.as_dir();

    std::fs::hard_link(&victim, d.final_path().join("hard.log")).unwrap();
    for result in [
        d.open_append("hard.log", true).map(drop),
        d.open_read("hard.log").map(drop),
    ] {
        assert_eq!(
            refusal_of(&result.unwrap_err()),
            &Refusal::HardLinked { links: 2 }
        );
    }

    junction(&d.final_path().join("j.log"), outside.path());
    assert_eq!(
        refusal_of(&d.open_append("j.log", true).unwrap_err()),
        &Refusal::ReparsePoint {
            tag: TAG_MOUNT_POINT
        }
    );
    std::fs::create_dir(d.final_path().join("dir.log")).unwrap();
    assert_eq!(
        refusal_of(&d.open_append("dir.log", true).unwrap_err()),
        &Refusal::NotAFile
    );

    if try_symlink(|| std::os::windows::fs::symlink_file(&victim, d.final_path().join("sym.log"))) {
        assert_eq!(
            refusal_of(&d.open_append("sym.log", true).unwrap_err()),
            &Refusal::ReparsePoint { tag: TAG_SYMLINK }
        );
        assert_eq!(
            refusal_of(&d.open_read("sym.log").unwrap_err()),
            &Refusal::ReparsePoint { tag: TAG_SYMLINK }
        );
        // 壊れた（先の無い）リンクでも、先にファイルを作らない。
        let dangling = outside.path().join("not-yet.log");
        std::os::windows::fs::symlink_file(&dangling, d.final_path().join("dangling.log")).unwrap();
        assert!(d.open_append("dangling.log", true).is_err());
        assert!(!dangling.exists(), "壊れたリンクの先に作った");
    }
    assert_eq!(std::fs::read(&victim).unwrap(), VICTIM);
}

/// `reopen_path`: 本体・付随の名前のリンク・ハードリンク・フォルダを断り（無いのは可）、
/// 返すパスは渡された綴りではなく本当のパス基準（大文字小文字・8.3 違いで開いて確かめる。
/// Fable B-8-2）。
#[test]
fn reopen_path_checks_companions_and_returns_final_path_based_paths() {
    let dir = tempfile::tempdir().unwrap();
    let (outside, victim) = outside_with_victim();
    let long = dir.path().join("Long Root Name");
    std::fs::create_dir(&long).unwrap();
    // 渡す綴り: 8.3 の短い名前があればそれ、無ければ大文字にした綴り。
    let spelled = crate::root::tests::short_path(&long)
        .unwrap_or_else(|| PathBuf::from(long.to_string_lossy().to_uppercase()));
    let root = open_root(&guarded(spelled.clone())).unwrap();
    assert_eq!(root.path(), spelled);
    let d = root.as_dir();
    let ok = d.reopen_path("db.sqlite3", &["-wal", "-shm"]).unwrap();
    assert_eq!(ok.path(), root.final_path().join("db.sqlite3"));
    assert!(ok.path().starts_with(root.final_path()));
    assert!(
        ok.path().to_string_lossy().contains("Long Root Name"),
        "本当の名前（長い名前・元の大文字小文字）でない: {:?}",
        ok.path()
    );

    std::fs::write(d.final_path().join("db.sqlite3"), b"db").unwrap();
    std::fs::hard_link(&victim, d.final_path().join("db.sqlite3-wal")).unwrap();
    assert_eq!(
        refusal_of(&d.reopen_path("db.sqlite3", &["-wal"]).unwrap_err()),
        &Refusal::HardLinked { links: 2 }
    );
    std::fs::remove_file(d.final_path().join("db.sqlite3-wal")).unwrap();
    junction(&d.final_path().join("db.sqlite3-shm"), outside.path());
    assert_eq!(
        refusal_of(&d.reopen_path("db.sqlite3", &["-wal", "-shm"]).unwrap_err()),
        &Refusal::ReparsePoint {
            tag: TAG_MOUNT_POINT
        }
    );
    std::fs::remove_dir(d.final_path().join("db.sqlite3-shm")).unwrap();
    std::fs::create_dir(d.final_path().join("db.sqlite3-journal")).unwrap();
    assert_eq!(
        refusal_of(&d.reopen_path("db.sqlite3", &["-journal"]).unwrap_err()),
        &Refusal::NotAFile
    );
    assert!(d.reopen_path("db.sqlite3", &["-wal", "-shm"]).is_ok());
}

/// `write_atomic` の宛先がフォルダ・ジャンクション・ディレクトリのシンボリックリンクなら断り、
/// 宛先も外も変えず、一時ファイルを残さない（Fable B-6・B-8-4）。
#[test]
fn write_atomic_refuses_a_directory_or_junction_destination() {
    let dir = tempfile::tempdir().unwrap();
    let (outside, _victim) = outside_with_victim();
    let root = open_root(&guarded(dir.path().join("root"))).unwrap();
    let d = root.as_dir();
    std::fs::create_dir(d.final_path().join("plain-dir")).unwrap();
    junction(&d.final_path().join("junction"), outside.path());
    let mut dests = vec!["plain-dir", "junction"];
    if try_symlink(|| {
        std::os::windows::fs::symlink_dir(outside.path(), d.final_path().join("dirsym"))
    }) {
        dests.push("dirsym");
    }
    let before = snapshot(outside.path());
    for dest in &dests {
        let err = d.write_atomic(dest, b"payload").unwrap_err();
        let expected = match *dest {
            "plain-dir" => Refusal::NotAFile,
            "junction" => Refusal::ReparsePoint {
                tag: TAG_MOUNT_POINT,
            },
            _ => Refusal::ReparsePoint { tag: TAG_SYMLINK },
        };
        assert_eq!(refusal_of(&err), &expected, "{dest}");
    }
    assert_eq!(snapshot(outside.path()), before, "外が変わった");
    let mut names: Vec<_> = d.list().unwrap().into_iter().map(|e| e.name).collect();
    names.sort();
    let mut expected: Vec<String> = dests.iter().map(|s| s.to_string()).collect();
    expected.sort();
    assert_eq!(names, expected, "一時ファイルが残った・宛先が変わった");
    assert!(d.final_path().join("plain-dir").is_dir());
}

/// `write_atomic` の宛先がファイルのシンボリックリンク・ハードリンク: 名前だけが置き換わり、
/// リンクの先・別名（外）の中身は変わらない。
#[test]
fn write_atomic_replaces_only_the_name_of_a_file_link() {
    let dir = tempfile::tempdir().unwrap();
    let (_outside, victim) = outside_with_victim();
    let root = open_root(&guarded(dir.path().join("root"))).unwrap();
    let d = root.as_dir();
    std::fs::hard_link(&victim, d.final_path().join("hard.json")).unwrap();
    d.write_atomic("hard.json", b"new").unwrap();
    assert_eq!(
        std::fs::read(d.final_path().join("hard.json")).unwrap(),
        b"new"
    );
    assert_eq!(
        std::fs::read(&victim).unwrap(),
        VICTIM,
        "ハードリンクの先を書いた"
    );

    if try_symlink(|| std::os::windows::fs::symlink_file(&victim, d.final_path().join("sym.json")))
    {
        d.write_atomic("sym.json", b"new").unwrap();
        let meta = std::fs::symlink_metadata(d.final_path().join("sym.json")).unwrap();
        assert!(!meta.file_type().is_symlink(), "リンクのまま");
        assert_eq!(
            std::fs::read(d.final_path().join("sym.json")).unwrap(),
            b"new"
        );
        assert_eq!(
            std::fs::read(&victim).unwrap(),
            VICTIM,
            "リンクの先を書いた"
        );
    }
}

/// 一時名の衝突（Plain と同じ確かめを Guarded で）。
#[test]
fn write_atomic_collisions_when_guarded() {
    let dir = tempfile::tempdir().unwrap();
    let root = open_root(&guarded(dir.path().join("root"))).unwrap();
    crate::root::tests::collisions_on(root.as_dir());
}

/// `remove`: 名前がジャンクション・フォルダ・ファイルのシンボリックリンクなら、開いた後の
/// 属性で断って消さない（ハードリンクは名前だけ消す）。
#[test]
fn remove_refuses_links_and_dirs_by_the_attributes_after_opening() {
    let dir = tempfile::tempdir().unwrap();
    let (outside, victim) = outside_with_victim();
    let root = open_root(&guarded(dir.path().join("root"))).unwrap();
    let d = root.as_dir();
    junction(&d.final_path().join("j"), outside.path());
    std::fs::create_dir(d.final_path().join("sub")).unwrap();
    let before = snapshot(outside.path());
    assert_eq!(
        refusal_of(&d.remove("j").unwrap_err()),
        &Refusal::ReparsePoint {
            tag: TAG_MOUNT_POINT
        }
    );
    assert!(
        std::fs::symlink_metadata(d.final_path().join("j")).is_ok(),
        "リンクを消した"
    );
    assert_eq!(
        refusal_of(&d.remove("sub").unwrap_err()),
        &Refusal::NotAFile
    );
    assert!(d.final_path().join("sub").is_dir());
    if try_symlink(|| std::os::windows::fs::symlink_file(&victim, d.final_path().join("s"))) {
        assert_eq!(
            refusal_of(&d.remove("s").unwrap_err()),
            &Refusal::ReparsePoint { tag: TAG_SYMLINK }
        );
    }
    std::fs::hard_link(&victim, d.final_path().join("h")).unwrap();
    assert!(d.remove("h").unwrap());
    assert!(!d.final_path().join("h").exists());
    assert_eq!(snapshot(outside.path()), before, "外が変わった");
}

/// `remove_where`: 一覧を取った後・消す前に、候補の名前をジャンクションに差し替える →
/// 開いた後の属性で断り（`refused`）、リンクも外も消さない。残りは続けて消す。
#[test]
fn remove_where_rechecks_after_opening_when_a_name_is_swapped() {
    let dir = tempfile::tempdir().unwrap();
    let (outside, _victim) = outside_with_victim();
    let root = open_root(&guarded(dir.path().join("root"))).unwrap();
    let d = root.as_dir();
    std::fs::write(d.final_path().join("a.log"), b"a").unwrap();
    std::fs::write(d.final_path().join("b.log"), b"b").unwrap();
    let swap_dir = d.final_path().to_path_buf();
    let outside_path = outside.path().to_path_buf();
    BEFORE_REMOVE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |name: &str| {
            if name == "a.log" {
                std::fs::remove_file(swap_dir.join(name)).unwrap();
                junction(&swap_dir.join(name), &outside_path);
            }
        }));
    });
    let before = snapshot(outside.path());
    let report = d.remove_where(&|n| n.ends_with(".log"));
    BEFORE_REMOVE.with(|hook| *hook.borrow_mut() = None);
    let report = report.unwrap();
    assert_eq!(report.removed, ["b.log"]);
    assert_eq!(
        report.refused,
        [(
            "a.log".to_string(),
            Refusal::ReparsePoint {
                tag: TAG_MOUNT_POINT
            }
        )]
    );
    assert!(report.failed.is_empty());
    assert!(std::fs::symlink_metadata(d.final_path().join("a.log")).is_ok());
    assert_eq!(snapshot(outside.path()), before, "外が変わった");
}

/// 握ったフォルダを、別のハンドルでその場でジャンクションにする（締める前の古いハンドルの筋。
/// 設計 §2.9）。`reopen_path` は握ったハンドルで自分の属性を読み直して断り、`create_new`・
/// `write_atomic` は外に何も作らず、`list` は外の中身を返さない。
#[test]
fn a_held_dir_turned_into_a_junction_does_not_leak() {
    let dir = tempfile::tempdir().unwrap();
    let (outside, _victim) = outside_with_victim();
    let root = open_root(&guarded(dir.path().join("root"))).unwrap();
    let set = root.open_managed(&[spec("data", None, "data")]).unwrap();
    let data = set.get("data").unwrap();
    let before = snapshot(outside.path());
    if let Err(e) = set_reparse_to(data.final_path(), outside.path()) {
        if std::env::var_os("CI").is_some() {
            panic!("CI で FSCTL_SET_REPARSE_POINT が失敗した: {e}");
        }
        eprintln!("skip: FSCTL_SET_REPARSE_POINT に権限が無い（{e}）- CI で確かめる");
        return;
    }
    assert_eq!(
        refusal_of(&data.reopen_path("x.sqlite3", &["-wal"]).unwrap_err()),
        &Refusal::ReparsePoint {
            tag: TAG_MOUNT_POINT
        }
    );
    // 握ったハンドルがジャンクションに変わった後は、作る・書くが失敗する（1921 =
    // `ERROR_CANT_ACCESS_FILE`。外には何も作られない - 下の snapshot で確かめる）。
    let created = data.create_new("probe");
    eprintln!("note: ジャンクションに変わったハンドルの下の create_new: {created:?}");
    assert!(created.is_err(), "ジャンクションの下で作れてしまった");
    drop(created);
    let written = data.write_atomic("state.json", b"x");
    eprintln!("note: ジャンクションに変わったハンドルの下の write_atomic: {written:?}");
    assert!(written.is_err(), "ジャンクションの下で書けてしまった");
    let listed = data.list();
    eprintln!("note: ジャンクションに変わったハンドルの list: {listed:?}");
    if let Ok(listed) = listed {
        assert!(
            !listed.iter().any(|e| e.name == "victim.txt"),
            "外の中身を列挙した"
        );
    }
    drop(set);
    assert_eq!(
        snapshot(outside.path()),
        before,
        "ジャンクションの先に作った・書いた"
    );
    let _ = std::fs::remove_dir(root.final_path().join("data"));
}

/// 【仕様の実測】`OpenFileById` は木の外へ移された物も開ける（ヒントのハンドルはボリュームを
/// 選ぶだけ）。`open_child_by_id` は開いた後に本当のパスを確かめ、木の外なら `Vanished` を返す
/// （設計 §2.4 の 3、Fable-2 A-3）。管理者でなくても走る。
#[test]
fn an_object_moved_out_after_enumeration_is_not_opened() {
    use crate::acl::imp::{enumerate_dir, open_child_by_id, ChildOpen};
    let dir = tempfile::tempdir().unwrap();
    let (outside, _victim) = outside_with_victim();
    let tree = dir.path().join("tree");
    std::fs::create_dir_all(tree.join("sub")).unwrap();
    std::fs::write(tree.join("sub").join("child.txt"), b"c").unwrap();
    let root = crate::acl::pin_dir(&tree).unwrap().unwrap();
    let sub = root.open_child_dir("sub").unwrap().unwrap();
    let mut ids = Vec::new();
    enumerate_dir::<SafeRootError>(sub.raw_handle(), &mut |e| {
        ids.push((e.name, e.file_id));
        Ok(())
    })
    .unwrap();
    let (_, id) = ids.into_iter().find(|(n, _)| n == "child.txt").unwrap();

    // 木の中にある間は開ける。
    assert!(matches!(
        open_child_by_id(sub.raw_handle(), id, root.final_path()).unwrap(),
        ChildOpen::Opened(_)
    ));
    // 列挙の後に、木の外（利用者のフォルダ）へ移す。
    std::fs::rename(
        tree.join("sub").join("child.txt"),
        outside.path().join("moved.txt"),
    )
    .unwrap();
    assert!(
        matches!(
            open_child_by_id(sub.raw_handle(), id, root.final_path()).unwrap(),
            ChildOpen::Vanished
        ),
        "木の外へ移された物を開いた"
    );
    // 確かめの基準を外のフォルダにすれば開ける = ID はまだ外の物を指している（仕様の実測）。
    assert!(matches!(
        open_child_by_id(
            sub.raw_handle(),
            id,
            &crate::nofollow::normalize_expected(outside.path())
        )
        .unwrap(),
        ChildOpen::Opened(_)
    ));
}

/// `reset_acl`（#626 の昇格ヘルパーの書き直し）も同じ確かめを通る: 列挙の後・開く前に子を木の
/// 外へ移す → 外の物の記述子は変わらない。所有者を移すので管理者でだけ子まで進む（CI）。
#[test]
fn reset_acl_does_not_rewrite_a_child_moved_out_after_enumeration() {
    let admin = crate::acl::tests::windows::is_elevated_admin();
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let tree = dir.path().join("tree");
    std::fs::create_dir(&tree).unwrap();
    std::fs::write(tree.join("child.txt"), b"c").unwrap();
    let moved = outside.path().join("moved.txt");
    // 名前の変更では記述子は変わらないので、移す前の記述子と比べる。
    let before = crate::acl::inspect_file(&tree.join("child.txt"))
        .unwrap()
        .unwrap();
    let pinned = crate::acl::pin_dir(&tree).unwrap().unwrap();
    let (from, to) = (tree.join("child.txt"), moved.clone());
    crate::acl::imp::BEFORE_CHILD_OPEN.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |child: &Path| {
            if child.file_name().is_some_and(|n| n == "child.txt") {
                std::fs::rename(&from, &to).unwrap();
            }
        }));
    });
    let result = pinned.reset_acl(
        &DirAccess::with_install_user(me()),
        crate::acl::Recurse::Tree,
    );
    crate::acl::imp::BEFORE_CHILD_OPEN.with(|hook| *hook.borrow_mut() = None);
    if !admin {
        // `is_elevated_admin` が `CI` では panic するので、ここへ来るのは手元だけ。
        // 手元でも `CI` と同じ扱い（管理者でなければ skip を明示）に揃える。
        if std::env::var_os("CI").is_some() {
            panic!("CI なのに管理者ではない: 所有者を移す確かめが走らない（{result:?}）");
        }
        eprintln!("skip: 所有者を移すのは管理者だけ（{result:?}）- CI で確かめる");
        return;
    }
    let skips = result.unwrap();
    assert!(moved.exists(), "フックが動いていない");
    let after = crate::acl::inspect_file(&moved).unwrap().unwrap();
    assert_eq!(after, before, "木の外へ移された物の記述子を書き換えた");
    assert_eq!(skips, crate::acl::ResetSkips::default(), "外の物を数えた");
}

/// 同じ握ったハンドルの列挙（`list`・`reset_acl` の再帰の最上段）は直列化される（#623 PR-1b の
/// レビュー S-1）。列挙の位置はハンドルに付くので、重なると `reset_acl` が残りの子を書き直さずに
/// 「成功」を返しうる。ロックを持っている間は、どちらも進まない（管理者でなくても確かめられる
/// 形: `reset_acl` は列挙の前にロックを待つので、失敗するにしても待ってから）。
#[test]
fn listing_and_reset_acl_on_the_same_handle_are_serialized() {
    use std::sync::mpsc::{channel, RecvTimeoutError};
    use std::time::Duration;
    let dir = tempfile::tempdir().unwrap();
    let tree = dir.path().join("tree");
    std::fs::create_dir(&tree).unwrap();
    std::fs::write(tree.join("a.txt"), b"a").unwrap();
    let root = open_root(&guarded(tree)).unwrap();
    let pinned = root.pinned().unwrap();
    let hold = pinned.enum_guard();
    std::thread::scope(|scope| {
        let (list_done, list_rx) = channel();
        let (reset_done, reset_rx) = channel();
        let dir = root.as_dir();
        scope.spawn(move || {
            let _ = dir.list();
            let _ = list_done.send(());
        });
        scope.spawn(move || {
            let _ = pinned.reset_acl(
                &DirAccess::with_install_user(me()),
                crate::acl::Recurse::Tree,
            );
            let _ = reset_done.send(());
        });
        assert_eq!(
            list_rx.recv_timeout(Duration::from_millis(500)),
            Err(RecvTimeoutError::Timeout),
            "列挙のロックを持っているのに list が進んだ"
        );
        assert_eq!(
            reset_rx.recv_timeout(Duration::from_millis(500)),
            Err(RecvTimeoutError::Timeout),
            "列挙のロックを持っているのに reset_acl が進んだ"
        );
        drop(hold);
        list_rx.recv_timeout(Duration::from_secs(30)).unwrap();
        reset_rx.recv_timeout(Duration::from_secs(30)).unwrap();
    });
}

/// 列挙バッファ（64 KiB）を超える数の子があっても、別スレッドが同じハンドルを `list()` し続けて
/// いる間の `reset_acl(Tree)` が全部の子を書き直す（S-1）。所有者を移すので管理者でだけ走る。
#[test]
fn reset_acl_rewrites_every_child_while_the_same_handle_is_listed() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let admin = crate::acl::tests::windows::is_elevated_admin();
    if !admin {
        eprintln!("skip: 所有者を移すのは管理者だけ - CI で確かめる");
        return;
    }
    const CHILDREN: usize = 1500;
    let dir = tempfile::tempdir().unwrap();
    let tree = dir.path().join("tree");
    std::fs::create_dir(&tree).unwrap();
    let pad = "x".repeat(64);
    let names: Vec<String> = (0..CHILDREN).map(|i| format!("{pad}-{i:05}.txt")).collect();
    for name in &names {
        std::fs::write(tree.join(name), b"c").unwrap();
    }
    let root = open_root(&guarded(tree.clone())).unwrap();
    let pinned = root.pinned().unwrap();
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            while !stop.load(Ordering::Relaxed) {
                let listed = root.as_dir().list().unwrap();
                assert_eq!(listed.len(), CHILDREN, "列挙が欠けた");
            }
        });
        let skips = pinned
            .reset_acl(
                &DirAccess::with_install_user(me()),
                crate::acl::Recurse::Tree,
            )
            .unwrap();
        stop.store(true, Ordering::Relaxed);
        assert_eq!(skips, crate::acl::ResetSkips::default());
    });
    for name in &names {
        let security = crate::acl::inspect_file(&tree.join(name)).unwrap().unwrap();
        assert_eq!(
            security.dacl.owner,
            crate::acl::ADMINISTRATORS_SID,
            "書き直されなかった子: {name}"
        );
    }
}

/// #677 のレビュー N-5: 無いルートの**親も無い**ときは、何も作らずに `ParentAbsent`。
#[test]
fn a_root_whose_parent_is_absent_is_refused_without_creating_anything() {
    let dir = tempfile::tempdir().unwrap();
    let refusal = open_root(&guarded(dir.path().join("nope").join("root"))).unwrap_err();
    assert_eq!(refusal, RootRefusal::ParentAbsent);
    assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
}

/// #677 のレビュー N-5: 管理するフォルダの所有者が SYSTEM・Administrators・`owners` の
/// どれでもなければ `ManagedRefusal::ForeignOwner`（ルートの確かめと同じ規則）。管理者（CI）が
/// 作ったフォルダの所有者は Administrators になるので、所有者を今の利用者へ移してから、今の
/// 利用者を許さない spec で開く。
#[test]
fn a_managed_dir_owned_by_someone_not_allowed_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(root.join("data")).unwrap();
    if crate::acl::tests::windows::is_elevated_admin() {
        set_owner_to_me(&root.join("data"));
    }
    let opened = open_root(&RootSpec {
        owners: Vec::new(),
        ..guarded(root.clone())
    });
    let Ok(opened) = opened else {
        // ルート自身が今の利用者の所有（非管理者が作った）なら、ルートで先に断られる。所有者を
        // 許す spec でルートを開き直して、管理するフォルダの確かめだけ見る。
        let opened = open_root(&guarded(root.clone())).unwrap();
        // ルートの所有者を許す spec では管理するフォルダの所有者（同じ利用者）も許されるので、
        // ここでは「許す」側を確かめて終える（非管理者では別の所有者を作れない）。
        opened.open_managed(&[spec("data", None, "data")]).unwrap();
        return;
    };
    let stop = opened
        .open_managed(&[spec("data", None, "data")])
        .unwrap_err();
    assert_eq!(stop.key, "data");
    assert_eq!(
        stop.reason,
        ManagedRefusal::ForeignOwner(me().as_str().to_string())
    );
}
