//! [`load_pinned_hub_cert`] を偽物の [`TlsTrustOps`] で確かめる（全 OS）: 確かめの順序・鍵を
//! 開かないこと・断る分岐ごとの理由と対処の種類。置き場の手順 1〜3 の細かい表は
//! banto-hub-core の `hub_tls_prepare/tests/load.rs`（同じ [`open_place`] を通る）。

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use banto_safe_root::acl::{
    FileSecurity, ObservedAce, ObservedDacl, PinnedDirInfo, ADMINISTRATORS_SID, MODIFY_ACCESS_MASK,
    READ_EXECUTE_ACCESS_MASK, SYSTEM_SID,
};
use banto_safe_root::guard::{KnownFolders, MachineEnvironment};

use super::*;

const EVIL: &str = "S-1-5-21-9-9-9-1666";
const USERS: &str = "S-1-5-32-545";

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
fn tls() -> PathBuf {
    root().join(TLS_DIR)
}

struct FakeEnv;

impl MachineEnvironment for FakeEnv {
    fn machine_variable(&self, _name: &str) -> Result<Option<String>, String> {
        panic!("置き場の確かめはマシンの環境変数を読まない");
    }
    fn program_data(&self) -> Result<PathBuf, String> {
        Ok(pd())
    }
    fn known_folders(&self) -> Result<KnownFolders, String> {
        Ok(KnownFolders {
            forbidden_inside: vec![abs("Windows"), abs("Program Files")],
            forbidden_itself: vec![pd(), abs("Users")],
        })
    }
}

fn ace(sid: &str, mask: u32, flags: u8) -> ObservedAce {
    ObservedAce {
        ace_type: 0,
        flags,
        mask,
        sid: sid.to_string(),
    }
}

fn trusted_aces(flags: u8) -> Vec<ObservedAce> {
    vec![
        ace(SYSTEM_SID, 0x1F01FF, flags),
        ace(ADMINISTRATORS_SID, 0x1F01FF, flags),
    ]
}

#[derive(Clone)]
struct DirEntry {
    owner: String,
    is_reparse_point: bool,
    real: Option<PathBuf>,
    aces: Vec<ObservedAce>,
}

impl DirEntry {
    fn owned_by(owner: &str) -> Self {
        Self {
            owner: owner.to_string(),
            is_reparse_point: false,
            real: None,
            aces: trusted_aces(0x03),
        }
    }

    fn users_read() -> Self {
        let mut entry = Self::owned_by(ADMINISTRATORS_SID);
        entry.aces.push(ace(USERS, READ_EXECUTE_ACCESS_MASK, 0x03));
        entry
    }
}

#[derive(Clone)]
struct FileEntry {
    security: FileSecurity,
    real: Option<PathBuf>,
    contents: Vec<u8>,
}

impl FileEntry {
    /// 証明書の形（SYSTEM・Administrators にフル、Users に読み取り）。
    fn users_read(contents: &[u8]) -> Self {
        let mut aces = trusted_aces(0);
        aces.push(ace(USERS, READ_EXECUTE_ACCESS_MASK, 0));
        Self {
            security: FileSecurity {
                is_dir: false,
                is_reparse_point: false,
                links: 1,
                dacl: ObservedDacl {
                    protected: true,
                    owner: ADMINISTRATORS_SID.to_string(),
                    aces: Some(aces),
                },
            },
            real: None,
            contents: contents.to_vec(),
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
}

impl TlsDir for FakeDir {
    fn info(&self) -> PinnedDirInfo {
        PinnedDirInfo {
            is_dir: true,
            is_reparse_point: self.entry.is_reparse_point,
            owner: self.entry.owner.clone(),
        }
    }
    fn final_path(&self) -> PathBuf {
        self.entry.real.clone().unwrap_or_else(|| self.path.clone())
    }
    fn security(&self) -> Result<FileSecurity, String> {
        Ok(FileSecurity {
            is_dir: true,
            is_reparse_point: self.entry.is_reparse_point,
            links: 1,
            dacl: ObservedDacl {
                protected: true,
                owner: self.entry.owner.clone(),
                aces: Some(self.entry.aces.clone()),
            },
        })
    }
}

struct FakeFile {
    path: PathBuf,
    entry: FileEntry,
}

impl TlsFile for FakeFile {
    fn security(&self) -> FileSecurity {
        self.entry.security.clone()
    }
    fn final_path(&self) -> PathBuf {
        self.entry.real.clone().unwrap_or_else(|| self.path.clone())
    }
    fn read_all(&mut self, max_len: usize) -> Result<Vec<u8>, crate::ReadError> {
        if self.entry.contents.len() > max_len {
            return Err(crate::ReadError::TooLarge);
        }
        Ok(self.entry.contents.clone())
    }
}

/// 置き場の偽物（読み取りだけ。開いた順を記録する）。
struct FakeOps {
    env: FakeEnv,
    dirs: HashMap<PathBuf, DirEntry>,
    files: HashMap<PathBuf, FileEntry>,
    open_error: HashMap<String, FileOpenError>,
    log: RefCell<Vec<String>>,
}

fn cert_pem() -> String {
    rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string()])
        .unwrap()
        .cert
        .pem()
}

impl FakeOps {
    /// 既定: ルート・`tls\` があり、正しい形の証明書と鍵が置いてある。
    fn new() -> Self {
        let mut dirs = HashMap::new();
        dirs.insert(root(), DirEntry::owned_by(ADMINISTRATORS_SID));
        dirs.insert(tls(), DirEntry::users_read());
        let mut files = HashMap::new();
        files.insert(
            tls().join(CERT_FILE),
            FileEntry::users_read(cert_pem().as_bytes()),
        );
        // 鍵は開かないので、中身は何でもよい（開いたらテストが落ちる）。
        files.insert(
            tls().join(KEY_FILE),
            FileEntry::users_read(b"-----BEGIN PRIVATE KEY-----"),
        );
        Self {
            env: FakeEnv,
            dirs,
            files,
            open_error: HashMap::new(),
            log: RefCell::new(Vec::new()),
        }
    }

    fn cert(&mut self) -> &mut FileEntry {
        self.files.get_mut(&tls().join(CERT_FILE)).unwrap()
    }

    fn calls(&self) -> Vec<String> {
        self.log.borrow().clone()
    }
}

impl TlsTrustOps for FakeOps {
    type Env = FakeEnv;
    type Dir = FakeDir;
    type File = FakeFile;

    fn machine_env(&self) -> &FakeEnv {
        &self.env
    }
    fn pin_dir(&self, dir: &Path) -> Result<Option<FakeDir>, String> {
        self.log.borrow_mut().push(format!("pin {}", short(dir)));
        Ok(self.dirs.get(dir).map(|entry| FakeDir {
            path: dir.to_path_buf(),
            entry: entry.clone(),
        }))
    }
    fn open_child_dir(&self, parent: &FakeDir, name: &str) -> Result<Option<FakeDir>, String> {
        let path = parent.path.join(name);
        self.log.borrow_mut().push(format!("open_dir {name}"));
        Ok(self.dirs.get(&path).map(|entry| FakeDir {
            path: path.clone(),
            entry: entry.clone(),
        }))
    }
    fn open_child_file(
        &self,
        parent: &FakeDir,
        name: &str,
    ) -> Result<Option<FakeFile>, FileOpenError> {
        assert_ne!(name, KEY_FILE, "クライアントは鍵を開かない");
        let path = parent.path.join(name);
        self.log.borrow_mut().push(format!("open_file {name}"));
        if let Some(error) = self.open_error.get(name) {
            return Err(error.clone());
        }
        Ok(self.files.get(&path).map(|entry| FakeFile {
            path: path.clone(),
            entry: entry.clone(),
        }))
    }
}

fn assert_refused(ops: &FakeOps, needle: &str, remedy: Remedy) -> Unavailable {
    let found = load_pinned_hub_cert(ops).expect_err("断るはず");
    assert!(found.reason.contains(needle), "{needle} が無い: {found:#?}");
    assert_eq!(found.remedy, remedy, "{found:#?}");
    assert!(!found.reason.contains("PRIVATE KEY"), "{found:#?}");
    found
}

#[test]
fn loads_the_certificate_in_order_without_opening_the_key() {
    let ops = FakeOps::new();
    let expected = ops.files[&tls().join(CERT_FILE)].contents.clone();
    let pin = load_pinned_hub_cert(&ops).unwrap();
    assert_eq!(pin.cert_pem, expected);
    assert_eq!(pin.cert_path, tls().join(CERT_FILE));
    assert_eq!(
        ops.calls(),
        vec!["pin BantoHub", "open_dir tls", "open_file hub-cert.pem"]
    );
}

#[test]
fn returns_the_real_path_of_the_certificate() {
    let mut ops = FakeOps::new();
    // 本当のパスの大文字小文字の違いは同じ場所（paths_match）。返すのは本当のパス。
    let real = PathBuf::from(tls().join(CERT_FILE).to_string_lossy().to_uppercase());
    if cfg!(windows) {
        ops.cert().real = Some(real.clone());
        assert_eq!(load_pinned_hub_cert(&ops).unwrap().cert_path, real);
    }
}

#[test]
fn refuses_when_nothing_is_prepared() {
    let mut ops = FakeOps::new();
    ops.files.clear();
    assert_refused(
        &ops,
        "に証明書（hub-cert.pem）がありません",
        Remedy::Prepare,
    );

    ops.dirs.remove(&tls());
    assert_refused(&ops, "Hub の TLS の置き場", Remedy::Prepare);

    ops.dirs.clear();
    let found = assert_refused(&ops, "Hub のルート", Remedy::Prepare);
    assert!(found.reason.contains("がありません"), "{found:#?}");
}

#[test]
fn refuses_an_unprotected_root_without_opening_the_tls_dir() {
    // 一般の利用者が先に作ったルート（所有者が利用者）。
    let mut ops = FakeOps::new();
    ops.dirs.insert(root(), DirEntry::owned_by(EVIL));
    assert_refused(&ops, "所有者", Remedy::FixRoot);
    assert_eq!(ops.calls(), vec!["pin BantoHub"]);

    // 所有者は Administrators でも、利用者に削除・子の削除を許すルート。
    let mut ops = FakeOps::new();
    let mut entry = DirEntry::owned_by(ADMINISTRATORS_SID);
    entry.aces.push(ace(EVIL, MODIFY_ACCESS_MASK | 0x40, 0x03));
    ops.dirs.insert(root(), entry);
    assert_refused(&ops, "は保護されていません", Remedy::FixRoot);
    assert_eq!(ops.calls(), vec!["pin BantoHub"]);

    // ルートが reparse point。
    let mut ops = FakeOps::new();
    ops.dirs.get_mut(&root()).unwrap().is_reparse_point = true;
    assert_refused(&ops, "reparse point", Remedy::FixRoot);
}

#[test]
fn refuses_an_exposed_or_foreign_tls_dir() {
    // Users に書き込みを許す tls\。
    let mut ops = FakeOps::new();
    ops.dirs
        .get_mut(&tls())
        .unwrap()
        .aces
        .push(ace(USERS, MODIFY_ACCESS_MASK, 0x03));
    assert_refused(
        &ops,
        "ほかの利用者が鍵・証明書を書き換えた",
        Remedy::RecreateDir(tls()),
    );
    assert_eq!(ops.calls(), vec!["pin BantoHub", "open_dir tls"]);

    // 一般の利用者が先に作った tls\。
    let mut ops = FakeOps::new();
    ops.dirs.insert(tls(), DirEntry::owned_by(EVIL));
    assert_refused(&ops, "所有者", Remedy::RecreateDir(tls()));

    // tls\ の本当の場所が別（ジャンクションの先など）。
    let mut ops = FakeOps::new();
    ops.dirs.get_mut(&tls()).unwrap().real = Some(abs("Users/evil/tls"));
    assert_refused(&ops, "本当の場所", Remedy::RecreateDir(tls()));
}

#[test]
fn refuses_an_exposed_certificate() {
    let mut ops = FakeOps::new();
    if let Some(aces) = ops.cert().security.dacl.aces.as_mut() {
        aces.push(ace(EVIL, MODIFY_ACCESS_MASK, 0));
    }
    assert_refused(&ops, "ほかの利用者に書き換えられた", Remedy::Renew);

    let mut ops = FakeOps::new();
    ops.cert().security.dacl.owner = EVIL.to_string();
    assert_refused(&ops, "証明書", Remedy::Renew);

    // ハードリンク。
    let mut ops = FakeOps::new();
    ops.cert().security.links = 2;
    assert_refused(&ops, "ハードリンク", Remedy::Renew);
}

#[test]
fn refuses_a_directory_link_misplaced_or_busy_certificate() {
    let mut ops = FakeOps::new();
    ops.cert().security.is_dir = true;
    assert_refused(&ops, "フォルダです", Remedy::RecreateDir(tls()));

    let mut ops = FakeOps::new();
    ops.cert().security.is_reparse_point = true;
    assert_refused(&ops, "reparse point", Remedy::RecreateDir(tls()));

    let mut ops = FakeOps::new();
    ops.cert().real = Some(abs("Users/evil/hub-cert.pem"));
    assert_refused(&ops, "本当の場所", Remedy::RecreateDir(tls()));

    let mut ops = FakeOps::new();
    ops.open_error
        .insert(CERT_FILE.to_string(), FileOpenError::InUse);
    assert_refused(&ops, "書き込みに開かれています", Remedy::Retry);

    let mut ops = FakeOps::new();
    ops.cert().contents = vec![b'a'; MAX_PEM_BYTES + 1];
    assert_refused(&ops, "を読めませんでした", Remedy::Retry);
}

#[test]
fn refuses_a_broken_or_multiple_certificate_pem() {
    let cases: Vec<(Vec<u8>, &str)> = vec![
        (b"not a pem".to_vec(), "ちょうど 1 つではありません（0 個）"),
        (
            format!("{}{}", cert_pem(), cert_pem()).into_bytes(),
            "ちょうど 1 つではありません（2 個）",
        ),
        (
            format!("{}-----BEGIN PRIVATE KEY-----\n", cert_pem()).into_bytes(),
            "秘密鍵が入っています",
        ),
        (
            b"-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n".to_vec(),
            "DER",
        ),
        (
            b"-----BEGIN CERTIFICATE-----\n!!!!\n-----END CERTIFICATE-----\n".to_vec(),
            "PEM を読めません",
        ),
    ];
    for (contents, needle) in cases {
        let mut ops = FakeOps::new();
        ops.cert().contents = contents;
        let found = assert_refused(&ops, needle, Remedy::Renew);
        assert!(found.reason.starts_with("証明書（"), "{found:#?}");
    }
}

#[test]
fn root_dacl_problems_do_not_trust_anyone_without_elevated() {
    // 実行した本人を渡したときだけ本人を信頼する（読む側は None）。
    let security = FileSecurity {
        is_dir: true,
        is_reparse_point: false,
        links: 1,
        dacl: ObservedDacl {
            protected: false,
            owner: EVIL.to_string(),
            aces: Some(trusted_aces(0x03)),
        },
    };
    assert!(!root_dacl_problems(&security, None).is_empty());
    assert!(root_dacl_problems(&security, Some(EVIL)).is_empty());
}

mod revocation;
mod user;
