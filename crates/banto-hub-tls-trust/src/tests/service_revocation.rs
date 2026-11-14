//! サービスの失効の記録（`crate::service_revocation`、#1121 PR2b）を偽物の
//! [`ServiceRevocationOps`] で確かめる（全 OS）: 読み口の「無い」「読めた」と使えない 3 種類の
//! 分け方、無ければ作る（置き換えない）、壊れた記録の作り直しと一時ファイルの片付け。
//! docs/hub-desktop-tls-design.md §4.4.2 の場面の番号を試験の名前に入れる。elev とサービスの
//! Hub からの手順は banto-hub-core の `hub_tls_prepare/tests.rs`・`tests/load.rs`。

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use banto_safe_root::acl::{ADMINISTRATORS_SID, MODIFY_ACCESS_MASK};

use super::{ace, root, DirEntry, FakeDir, FakeEnv, FakeFile, FileEntry, EVIL, USERS};
use crate::*;

/// 置き場のフォルダの形を変える手（表のテスト用）。
type DirEdit = Box<dyn Fn(&mut DirEntry)>;

/// 記録のファイルの形を変える手（表のテスト用）。
type FileEdit = Box<dyn Fn(&mut FileEntry)>;

fn records() -> PathBuf {
    root().join(SERVICE_REVOCATIONS_DIR)
}

fn record_file() -> PathBuf {
    records().join(REVOCATIONS_FILE)
}

fn thumb(n: u8) -> CertThumbprint {
    CertThumbprint::from_hex(&format!("{n:02x}").repeat(32)).unwrap()
}

fn revoked(n: u8) -> RevokedCert {
    RevokedCert {
        thumbprint: thumb(n),
        place: RevokedCertPlace::Service,
        recorded_at: 10,
    }
}

fn encoded(thumbs: &[u8]) -> Vec<u8> {
    RevocationRecord::new(RecordState::Committed, thumbs.iter().map(|n| revoked(*n))).encode()
}

/// 書ける偽物（開いた順と書き込みを記録する）。
struct FakeServiceOps {
    env: FakeEnv,
    dirs: RefCell<HashMap<PathBuf, DirEntry>>,
    files: RefCell<HashMap<PathBuf, FileEntry>>,
    /// 開くと失敗する名前。
    open_error: HashMap<String, FileOpenError>,
    /// 作ると失敗する。
    create_error: Option<String>,
    /// 置き換えずに作る直前に、別の書き手が作った記録（競合の形）。
    raced: RefCell<Option<Vec<u8>>>,
    log: RefCell<Vec<String>>,
}

impl FakeServiceOps {
    /// 既定: ルート・`tls\` があり、サービスの記録の置き場と記録 `thumbs` がある。
    fn with_record(thumbs: &[u8]) -> Self {
        let ops = Self::without_records();
        ops.dirs
            .borrow_mut()
            .insert(records(), DirEntry::users_read());
        ops.put(&encoded(thumbs));
        ops
    }

    /// ルート・`tls\` だけ（記録の置き場は無い）。
    fn without_records() -> Self {
        let mut dirs = HashMap::new();
        dirs.insert(root(), DirEntry::owned_by(ADMINISTRATORS_SID));
        dirs.insert(super::tls(), DirEntry::users_read());
        Self {
            env: FakeEnv,
            dirs: RefCell::new(dirs),
            files: RefCell::new(HashMap::new()),
            open_error: HashMap::new(),
            create_error: None,
            raced: RefCell::new(None),
            log: RefCell::new(Vec::new()),
        }
    }

    fn put(&self, contents: &[u8]) {
        self.files
            .borrow_mut()
            .insert(record_file(), FileEntry::users_read(contents));
    }

    fn stored(&self) -> Option<RevocationRecord> {
        self.files
            .borrow()
            .get(&record_file())
            .map(|entry| RevocationRecord::parse(&entry.contents).unwrap())
    }

    fn writes(&self) -> Vec<String> {
        self.log
            .borrow()
            .iter()
            .filter(|c| c.starts_with("WRITE") || c.starts_with("CREATE"))
            .cloned()
            .collect()
    }

    fn place(&self) -> CheckedPlace<FakeDir> {
        open_place(self).unwrap()
    }
}

impl TlsTrustOps for FakeServiceOps {
    type Env = FakeEnv;
    type Dir = FakeDir;
    type File = FakeFile;

    fn machine_env(&self) -> &FakeEnv {
        &self.env
    }
    fn pin_dir(&self, dir: &Path) -> Result<Option<FakeDir>, String> {
        Ok(self.dirs.borrow().get(dir).map(|entry| FakeDir {
            path: dir.to_path_buf(),
            entry: entry.clone(),
        }))
    }
    fn open_child_dir(&self, parent: &FakeDir, name: &str) -> Result<Option<FakeDir>, String> {
        let path = parent.path.join(name);
        self.log.borrow_mut().push(format!("open_dir {name}"));
        Ok(self.dirs.borrow().get(&path).map(|entry| FakeDir {
            path: path.clone(),
            entry: entry.clone(),
        }))
    }
    fn open_child_file(
        &self,
        parent: &FakeDir,
        name: &str,
    ) -> Result<Option<FakeFile>, FileOpenError> {
        let path = parent.path.join(name);
        self.log.borrow_mut().push(format!("open_file {name}"));
        if let Some(error) = self.open_error.get(name) {
            return Err(error.clone());
        }
        Ok(self.files.borrow().get(&path).map(|entry| FakeFile {
            path: path.clone(),
            entry: entry.clone(),
        }))
    }
}

impl ServiceRevocationOps for FakeServiceOps {
    fn create_protected_child_dir(
        &self,
        parent: &FakeDir,
        name: &str,
    ) -> Result<Option<FakeDir>, String> {
        self.log.borrow_mut().push(format!("CREATE dir {name}"));
        let path = parent.path.join(name);
        if self.dirs.borrow().contains_key(&path) {
            return Ok(None);
        }
        let entry = DirEntry::users_read();
        self.dirs.borrow_mut().insert(path.clone(), entry.clone());
        Ok(Some(FakeDir { path, entry }))
    }
    fn create_file_no_replace(
        &self,
        dir: &FakeDir,
        name: &str,
        contents: &[u8],
    ) -> Result<bool, String> {
        self.log.borrow_mut().push(format!("CREATE file {name}"));
        if let Some(error) = &self.create_error {
            return Err(error.clone());
        }
        if let Some(other) = self.raced.borrow_mut().take() {
            self.put(&other);
        }
        let path = dir.path.join(name);
        if self.files.borrow().contains_key(&path) {
            return Ok(false);
        }
        self.files
            .borrow_mut()
            .insert(path, FileEntry::users_read(contents));
        Ok(true)
    }
    fn write_file_atomic(&self, dir: &FakeDir, name: &str, contents: &[u8]) -> Result<(), String> {
        self.log.borrow_mut().push(format!("WRITE replace {name}"));
        self.files
            .borrow_mut()
            .insert(dir.path.join(name), FileEntry::users_read(contents));
        Ok(())
    }
    fn remove_stale_file_temps(&self, dir: &FakeDir, name: &str) -> Result<(), String> {
        let prefix = format!(".{name}.tmp-");
        let mut files = self.files.borrow_mut();
        let stale: Vec<PathBuf> = files
            .keys()
            .filter(|p| {
                p.parent() == Some(dir.path.as_path())
                    && p.file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with(&prefix))
            })
            .cloned()
            .collect();
        for path in stale {
            self.log.borrow_mut().push(format!(
                "WRITE tidy {}",
                path.file_name().unwrap().to_string_lossy()
            ));
            files.remove(&path);
        }
        Ok(())
    }
}

fn unusable(ops: &FakeServiceOps) -> ServiceRevocationProblem {
    read_service_revocations(ops).expect_err("使えないはず")
}

// --- 読み口 --------------------------------------------------------------------------

#[test]
fn reads_a_committed_record_without_writing() {
    let ops = FakeServiceOps::with_record(&[2, 1]);
    let found = read_service_revocations(&ops).unwrap();
    assert!(found.is_revoked(&thumb(1)) && found.is_revoked(&thumb(2)));
    assert!(!found.is_revoked(&thumb(3)));
    assert_eq!(found.entries().len(), 2);
    assert_eq!(ops.writes(), Vec::<String>::new());
    assert_eq!(
        *ops.log.borrow(),
        vec!["open_dir tls-revocations", "open_file revocations.txt"]
    );
}

/// 場面 25: 「無い」は空（ルート・置き場・ファイルのどれが無くても）。読み口は作らない。
#[test]
fn scene_25_an_absent_record_reads_as_empty() {
    let ops = FakeServiceOps::with_record(&[]);
    ops.files.borrow_mut().clear();
    assert_eq!(
        read_service_revocations(&ops).unwrap(),
        ServiceRevocations::Absent
    );
    let ops = FakeServiceOps::without_records();
    assert_eq!(
        read_service_revocations(&ops).unwrap(),
        ServiceRevocations::Absent
    );
    ops.dirs.borrow_mut().clear();
    assert_eq!(
        read_service_revocations(&ops).unwrap(),
        ServiceRevocations::Absent
    );
    assert!(!ServiceRevocations::Absent.is_revoked(&thumb(1)));
    assert_eq!(ops.writes(), Vec::<String>::new());
}

/// 場面 26: 形が壊れている（読めない形・`準備中`・大きすぎる・ファイルが露出・リンク・フォルダ）
/// は `Broken`。行ごとに読める拇印は拾う。
#[test]
fn scene_26_a_broken_record_is_broken_and_salvages_readable_lines() {
    let ops = FakeServiceOps::with_record(&[]);
    ops.put(
        format!(
            "garbage\nrevoked {} service 7\nrevoked zz service 1\n",
            thumb(4)
        )
        .as_bytes(),
    );
    let found = unusable(&ops);
    assert_eq!(found.fault, ServiceRevocationFault::Broken, "{found:#?}");
    assert!(found.reason.contains("形が壊れています"), "{found:#?}");
    assert_eq!(
        found.salvaged,
        vec![RevokedCert {
            recorded_at: 7,
            ..revoked(4)
        }]
    );

    let ops = FakeServiceOps::with_record(&[]);
    ops.put(&RevocationRecord::new(RecordState::Pending, [revoked(5)]).encode());
    let found = unusable(&ops);
    assert_eq!(found.fault, ServiceRevocationFault::Broken);
    assert!(found.reason.contains("準備中"), "{found:#?}");
    assert_eq!(found.salvaged, vec![revoked(5)]);

    let ops = FakeServiceOps::with_record(&[]);
    ops.put(&vec![b'x'; MAX_REVOCATIONS_BYTES + 1]);
    let found = unusable(&ops);
    assert_eq!(found.fault, ServiceRevocationFault::Broken);
    assert!(found.salvaged.is_empty());

    // ファイルの ACL・リンク・種類（置き場は通る）: 中身が読めれば拾う。
    for edit in [
        Box::new(|e: &mut FileEntry| {
            if let Some(aces) = e.security.dacl.aces.as_mut() {
                aces.push(ace(USERS, MODIFY_ACCESS_MASK, 0));
            }
        }) as FileEdit,
        Box::new(|e: &mut FileEntry| e.security.links = 2),
        Box::new(|e: &mut FileEntry| e.security.dacl.owner = EVIL.to_string()),
    ] {
        let ops = FakeServiceOps::with_record(&[6]);
        edit(ops.files.borrow_mut().get_mut(&record_file()).unwrap());
        let found = unusable(&ops);
        assert_eq!(found.fault, ServiceRevocationFault::Broken, "{found:#?}");
        assert!(found.reason.contains("保護されていません"), "{found:#?}");
        assert_eq!(found.salvaged, vec![revoked(6)]);
    }
    let ops = FakeServiceOps::with_record(&[6]);
    ops.files
        .borrow_mut()
        .get_mut(&record_file())
        .unwrap()
        .security
        .is_reparse_point = true;
    let found = unusable(&ops);
    assert_eq!(found.fault, ServiceRevocationFault::Broken);
    assert!(found.salvaged.is_empty(), "リンクの中身は読まない");
}

/// 場面 26: 置き場（ルート・記録のフォルダ）が確かめに通らないのは `Untrusted`。
#[test]
fn scene_26_an_untrusted_place_is_untrusted() {
    let cases: Vec<DirEdit> = vec![
        Box::new(|e| e.aces.push(ace(USERS, MODIFY_ACCESS_MASK, 0x03))),
        Box::new(|e| e.aces.push(ace(EVIL, 0x1F01FF, 0x03))),
        Box::new(|e| e.owner = EVIL.to_string()),
        Box::new(|e| e.is_reparse_point = true),
        Box::new(|e| e.real = Some(super::abs("Elsewhere/tls-revocations"))),
    ];
    for edit in cases {
        let ops = FakeServiceOps::with_record(&[1]);
        edit(ops.dirs.borrow_mut().get_mut(&records()).unwrap());
        let found = unusable(&ops);
        assert_eq!(found.fault, ServiceRevocationFault::Untrusted, "{found:#?}");
        assert!(
            found.reason.contains(SERVICE_REVOCATIONS_LABEL),
            "{found:#?}"
        );
        // 書き口も、確かめに通らない置き場には書かない。
        let place = ops.place();
        let found = ensure_service_revocations(&ops, &place).expect_err("通らない");
        assert_eq!(found.fault, ServiceRevocationFault::Untrusted);
        assert_eq!(ops.writes(), Vec::<String>::new());
    }
    // ルートが保護されていない。
    let ops = FakeServiceOps::with_record(&[1]);
    ops.dirs
        .borrow_mut()
        .get_mut(&root())
        .unwrap()
        .aces
        .push(ace(USERS, MODIFY_ACCESS_MASK, 0x03));
    assert_eq!(unusable(&ops).fault, ServiceRevocationFault::Untrusted);
}

#[test]
fn a_busy_or_unopenable_record_is_transient() {
    let mut ops = FakeServiceOps::with_record(&[1]);
    ops.open_error
        .insert(REVOCATIONS_FILE.to_string(), FileOpenError::InUse);
    assert_eq!(unusable(&ops).fault, ServiceRevocationFault::Transient);
    ops.open_error.insert(
        REVOCATIONS_FILE.to_string(),
        FileOpenError::Other("拒否".to_string()),
    );
    assert_eq!(unusable(&ops).fault, ServiceRevocationFault::Transient);
}

// --- 書き口 --------------------------------------------------------------------------

/// 場面 25: サービスの Hub の起動は、置き場と記録が無ければ作る（置き換えない）。2 回目は読むだけ。
#[test]
fn scene_25_ensure_creates_the_place_and_an_empty_record() {
    let ops = FakeServiceOps::without_records();
    let place = ops.place();
    let (record, created) = ensure_service_revocations(&ops, &place).unwrap();
    assert!(created);
    assert_eq!(record.entries(), &[]);
    assert_eq!(
        ops.writes(),
        vec!["CREATE dir tls-revocations", "CREATE file revocations.txt"]
    );
    assert_eq!(ops.stored().unwrap().entries(), &[]);
    ops.log.borrow_mut().clear();
    let (record, created) = ensure_service_revocations(&ops, &place).unwrap();
    assert!(!created);
    assert_eq!(record.entries(), &[]);
    assert_eq!(ops.writes(), Vec::<String>::new());
}

/// 「無い」を見た 2 者（サービスの Hub と elev）: 後から作る者は、先に作られた記録を上書きしない
/// （置き換えずに作る。読み直したものを使う）。
#[test]
fn creating_an_absent_record_never_overwrites_one_made_meanwhile() {
    let ops = FakeServiceOps::with_record(&[]);
    ops.files.borrow_mut().clear();
    *ops.raced.borrow_mut() = Some(encoded(&[8]));
    let place = ops.place();
    let (record, created) = ensure_service_revocations(&ops, &place).unwrap();
    assert!(!created);
    assert!(record.contains(&thumb(8)));
    assert!(ops.stored().unwrap().contains(&thumb(8)));
}

/// 作れない（I/O）は `Transient`（呼ぶ側は閉じる側に倒す）。壊れた記録は書き換えない。
#[test]
fn ensure_reports_io_failures_and_leaves_a_broken_record_alone() {
    let mut ops = FakeServiceOps::with_record(&[]);
    ops.files.borrow_mut().clear();
    ops.create_error = Some("ディスクが一杯".to_string());
    let place = ops.place();
    let found = ensure_service_revocations(&ops, &place).expect_err("作れない");
    assert_eq!(found.fault, ServiceRevocationFault::Transient);
    assert!(found.reason.contains("ディスクが一杯"), "{found:#?}");

    let ops = FakeServiceOps::with_record(&[]);
    ops.put(b"garbage");
    let place = ops.place();
    let found = ensure_service_revocations(&ops, &place).expect_err("壊れている");
    assert_eq!(found.fault, ServiceRevocationFault::Broken);
    assert_eq!(ops.writes(), Vec::<String>::new());
}

/// 場面 11・26: 作り直しは、残った一時ファイルを片付けてから置き換えで書く（`確定`・拇印の昇順）。
#[test]
fn scene_11_rewrite_tidies_temporaries_and_replaces() {
    let ops = FakeServiceOps::with_record(&[]);
    ops.put(b"garbage");
    let temp = records().join(".revocations.txt.tmp-0000abcd");
    ops.files
        .borrow_mut()
        .insert(temp.clone(), FileEntry::users_read(b"half"));
    let place = ops.place();
    let dir = open_service_revocations_dir(&ops, &ServiceRoot::of_place(&place))
        .unwrap()
        .unwrap();
    let record =
        rewrite_service_revocations(&ops, &dir, [revoked(3), revoked(1), revoked(3)]).unwrap();
    assert_eq!(
        ops.writes(),
        vec![
            "WRITE tidy .revocations.txt.tmp-0000abcd",
            "WRITE replace revocations.txt"
        ]
    );
    assert!(!ops.files.borrow().contains_key(&temp));
    assert_eq!(ops.stored().unwrap(), record);
    assert_eq!(record.state(), RecordState::Committed);
    assert_eq!(
        record
            .entries()
            .iter()
            .map(|e| e.thumbprint)
            .collect::<Vec<_>>(),
        vec![thumb(1), thumb(3)]
    );
    assert_eq!(
        read_service_revocations(&ops).unwrap(),
        ServiceRevocations::Record(record)
    );
}

#[test]
fn salvage_takes_only_well_formed_revoked_lines() {
    let text = format!(
        "revoked {} user 3\nrevoked {} service x\nrevoked {} service 1 extra\nrevoked {} other 2\n",
        thumb(1),
        thumb(2),
        thumb(3),
        thumb(4)
    );
    let found = salvage_revocations(text.as_bytes());
    assert_eq!(
        found,
        vec![
            RevokedCert {
                thumbprint: thumb(1),
                place: RevokedCertPlace::User,
                recorded_at: 3
            },
            RevokedCert {
                thumbprint: thumb(2),
                place: RevokedCertPlace::Service,
                recorded_at: 0
            },
            RevokedCert {
                thumbprint: thumb(4),
                place: RevokedCertPlace::Service,
                recorded_at: 2
            },
        ]
    );
    assert!(salvage_revocations(&[0xff, 0xfe]).is_empty());
}
