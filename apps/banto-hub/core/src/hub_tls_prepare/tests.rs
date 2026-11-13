//! 偽物の [`HubTlsOps`] で、確かめの順序・確かめ終わる前に副作用が無いこと・断る分岐ごとの
//! 出力を確かめる（全 OS）。作った鍵と証明書の中身（[`generate_identity`]・
//! [`inspect_identity`]）、`tls_listener` の読み込みと `PinnedHubTls` との握手もここ。本物の
//! ACL の操作は `win/tests.rs`。

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use banto_safe_root::acl::{
    FileSecurity, ObservedAce, ObservedDacl, PinnedDirInfo, ADMINISTRATORS_SID, MODIFY_ACCESS_MASK,
    READ_EXECUTE_ACCESS_MASK, SYSTEM_SID,
};
use banto_safe_root::guard::{KnownFolders, MachineEnvironment};
use banto_safe_root::InstallUserSid;
use banto_service_control::ConsoleLine;

use super::*;

const ME: &str = "S-1-5-21-1-2-3-1001";
const EVIL: &str = "S-1-5-21-9-9-9-1666";
const USERS: &str = "S-1-5-32-545";
/// 2026-11-13 00:00:00 UTC。
const NOW: i64 = 1_794_528_000;

/// 本当の今（rustls の握手は本当の時計で期限を見る）。
fn real_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

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

fn known() -> KnownFolders {
    KnownFolders {
        forbidden_inside: vec![abs("Windows"), abs("Program Files")],
        forbidden_itself: vec![pd(), abs("Users")],
    }
}

struct FakeEnv {
    program_data: Result<PathBuf, String>,
}

impl MachineEnvironment for FakeEnv {
    fn machine_variable(&self, _name: &str) -> Result<Option<String>, String> {
        panic!("prepare-hub-tls はマシンの環境変数を読まない");
    }
    fn program_data(&self) -> Result<PathBuf, String> {
        self.program_data.clone()
    }
    fn known_folders(&self) -> Result<KnownFolders, String> {
        Ok(known())
    }
}

type Log = Rc<RefCell<Vec<String>>>;

/// 置き場の形を変える手（表のテスト用）。
type DirEdit = Box<dyn Fn(&mut DirEntry)>;

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
    is_dir: bool,
    is_reparse_point: bool,
    real: Option<PathBuf>,
    aces: Vec<ObservedAce>,
}

impl DirEntry {
    fn owned_by(owner: &str) -> Self {
        Self {
            owner: owner.to_string(),
            is_dir: true,
            is_reparse_point: false,
            real: None,
            aces: trusted_aces(0x03),
        }
    }

    /// `tls\` を作ったときの形（Users に読み取り）。
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
    fn with(acl: FileAcl, contents: &[u8]) -> Self {
        let mut aces = trusted_aces(0);
        if acl == FileAcl::UsersRead {
            aces.push(ace(USERS, READ_EXECUTE_ACCESS_MASK, 0));
        }
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
    fn read_all(&mut self, max_len: usize) -> Result<Vec<u8>, String> {
        if self.entry.contents.len() > max_len {
            return Err("大きすぎます".to_string());
        }
        Ok(self.entry.contents.clone())
    }
}

struct FakeOps {
    env: FakeEnv,
    now: i64,
    dirs: RefCell<HashMap<PathBuf, DirEntry>>,
    /// 作るときに付ける形（既定は [`DirEntry::owned_by`] Administrators / `users_read`）。
    on_create: HashMap<PathBuf, DirEntry>,
    files: RefCell<HashMap<PathBuf, FileEntry>>,
    /// 開くと失敗する名前。
    open_error: HashMap<String, FileOpenError>,
    /// 作ると失敗する名前。
    create_error: HashMap<String, String>,
    /// 消すと失敗する名前。
    remove_error: HashMap<String, String>,
    /// 付け替えると失敗する名前（元の名前）。
    rename_error: HashMap<String, String>,
    /// 作ろうとすると「既にあった」になる名前（確かめた後に誰かが作った形）。
    create_exists: Vec<String>,
    /// 直列化の mutex の状態（既定は取れる）。
    lock: LockState,
    log: Log,
}

#[derive(Clone, Copy, PartialEq)]
enum LockState {
    Free,
    Held,
    Error,
}

impl FakeOps {
    /// 既定: ProgramData・ルート・`tls\` があり、中は空。
    fn new() -> Self {
        let mut dirs = HashMap::new();
        dirs.insert(pd(), DirEntry::owned_by(SYSTEM_SID));
        dirs.insert(root(), DirEntry::owned_by(ADMINISTRATORS_SID));
        dirs.insert(tls(), DirEntry::users_read());
        Self {
            env: FakeEnv {
                program_data: Ok(pd()),
            },
            now: NOW,
            dirs: RefCell::new(dirs),
            on_create: HashMap::new(),
            files: RefCell::new(HashMap::new()),
            open_error: HashMap::new(),
            create_error: HashMap::new(),
            remove_error: HashMap::new(),
            rename_error: HashMap::new(),
            create_exists: Vec::new(),
            lock: LockState::Free,
            log: Rc::new(RefCell::new(Vec::new())),
        }
    }

    /// 既定に、`now` に作った正しい鍵と証明書を置いたもの。
    fn with_identity() -> (Self, Identity) {
        let ops = Self::new();
        let identity = generate_identity(NOW).unwrap();
        ops.put(
            KEY_FILE,
            FileEntry::with(FileAcl::ServiceOnly, identity.key_pem.as_bytes()),
        );
        ops.put(
            CERT_FILE,
            FileEntry::with(FileAcl::UsersRead, identity.cert_pem.as_bytes()),
        );
        (ops, identity)
    }

    fn put(&self, name: &str, entry: FileEntry) {
        self.files.borrow_mut().insert(tls().join(name), entry);
    }

    fn file(&self, name: &str) -> Option<FileEntry> {
        self.files.borrow().get(&tls().join(name)).cloned()
    }

    fn edit_file(&self, name: &str, edit: impl FnOnce(&mut FileEntry)) {
        edit(self.files.borrow_mut().get_mut(&tls().join(name)).unwrap());
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

    fn file_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.files.borrow().keys().map(|p| short(p)).collect();
        names.sort();
        names
    }
}

impl HubTlsOps for FakeOps {
    type Env = FakeEnv;
    type Dir = FakeDir;
    type File = FakeFile;
    type Lock = ();

    fn hold_lock(&self) -> Result<Option<()>, String> {
        self.log.borrow_mut().push("lock".to_string());
        match self.lock {
            LockState::Free => Ok(Some(())),
            LockState::Held => Ok(None),
            LockState::Error => Err("拒否されました".to_string()),
        }
    }
    fn machine_env(&self) -> &FakeEnv {
        &self.env
    }
    fn current_user_sid(&self) -> Result<InstallUserSid, String> {
        Ok(InstallUserSid::parse(ME).unwrap())
    }
    fn now_unix(&self) -> i64 {
        self.now
    }
    fn pin_dir(&self, dir: &Path) -> Result<Option<FakeDir>, String> {
        self.log.borrow_mut().push(format!("pin {}", short(dir)));
        Ok(self.dirs.borrow().get(dir).map(|entry| FakeDir {
            path: dir.to_path_buf(),
            entry: entry.clone(),
        }))
    }
    fn create_child_dir(
        &self,
        parent: &FakeDir,
        name: &str,
        how: DirHow,
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
            .unwrap_or_else(|| match how {
                DirHow::Inherit => DirEntry::owned_by(ADMINISTRATORS_SID),
                DirHow::UsersRead => DirEntry::users_read(),
            });
        self.dirs.borrow_mut().insert(path.clone(), entry.clone());
        Ok(Some(FakeDir { path, entry }))
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
    fn create_child_file(
        &self,
        parent: &FakeDir,
        name: &str,
        contents: &[u8],
        acl: FileAcl,
    ) -> Result<bool, String> {
        self.log
            .borrow_mut()
            .push(format!("CREATE file {name} {acl:?}"));
        if let Some(error) = self.create_error.get(name) {
            return Err(error.clone());
        }
        let path = parent.path.join(name);
        if self.files.borrow().contains_key(&path) || self.create_exists.iter().any(|n| n == name) {
            return Ok(false);
        }
        self.files
            .borrow_mut()
            .insert(path, FileEntry::with(acl, contents));
        Ok(true)
    }
    fn remove_child(&self, parent: &FakeDir, name: &str) -> Result<bool, String> {
        self.log.borrow_mut().push(format!("WRITE remove {name}"));
        if let Some(error) = self.remove_error.get(name) {
            return Err(error.clone());
        }
        Ok(self
            .files
            .borrow_mut()
            .remove(&parent.path.join(name))
            .is_some())
    }
    fn rename_no_replace(&self, parent: &FakeDir, from: &str, to: &str) -> Result<bool, String> {
        self.log
            .borrow_mut()
            .push(format!("WRITE rename {from} {to}"));
        if let Some(error) = self.rename_error.get(from) {
            return Err(error.clone());
        }
        let mut files = self.files.borrow_mut();
        if files.contains_key(&parent.path.join(to)) {
            return Err("宛先があります".to_string());
        }
        match files.remove(&parent.path.join(from)) {
            Some(entry) => {
                files.insert(parent.path.join(to), entry);
                Ok(true)
            }
            None => Ok(false),
        }
    }
}

fn run(ops: &FakeOps, renew: bool) -> (Result<(), Refused>, Vec<String>) {
    let mut lines = Vec::new();
    let result = {
        let mut out = |line: ConsoleLine| lines.push(line);
        prepare_hub_tls(ops, renew, &mut out)
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

/// 断った: `Refused`・書き込みなし・理由の行がある。
fn assert_refused_without_writes(ops: &FakeOps, renew: bool, needle: &str) -> Vec<String> {
    let (result, lines) = run(ops, renew);
    assert_eq!(result, Err(Refused), "{lines:#?}");
    assert_eq!(ops.writes(), Vec::<String>::new(), "{lines:#?}");
    assert!(has(&lines, needle), "{needle} が無い: {lines:#?}");
    lines
}

/// 出力のどの行にも秘密鍵が出ていない。
fn assert_no_key_material(lines: &[String], identity_key: Option<&str>) {
    for line in lines {
        assert!(!line.contains("PRIVATE KEY"), "{line}");
        if let Some(key) = identity_key {
            for body in key.lines().filter(|l| !l.starts_with("-----")) {
                assert!(!line.contains(body), "{line}");
            }
        }
    }
}

// --- 引数 ----------------------------------------------------------------------------

#[test]
fn args_accept_only_nothing_or_renew() {
    assert_eq!(parse_args(&[]), Ok(false));
    assert_eq!(parse_args(&["--renew".to_string()]), Ok(true));
    for bad in [
        vec!["--Renew".to_string()],
        vec!["renew".to_string()],
        vec!["--renew".to_string(), "--renew".to_string()],
        vec!["--renew".to_string(), "C:\\x".to_string()],
        vec!["--force".to_string()],
    ] {
        assert!(parse_args(&bad).is_err(), "{bad:?}");
    }
}

// --- 鍵と証明書の中身 --------------------------------------------------------------

#[test]
fn generated_identity_passes_its_own_inspection() {
    let identity = generate_identity(NOW).unwrap();
    assert!(identity.cert_pem.starts_with("-----BEGIN CERTIFICATE-----"));
    assert!(identity.key_pem.starts_with("-----BEGIN PRIVATE KEY-----"));
    assert!(!format!("{identity:?}").contains("PRIVATE KEY"));
    let info = inspect_identity(
        identity.cert_pem.as_bytes(),
        identity.key_pem.as_bytes(),
        NOW,
    )
    .unwrap();
    assert_eq!(info.not_after_unix, not_after_for(NOW).unwrap());
    assert_eq!(utc_text(info.not_after_unix), "2036-11-13 00:00:00 UTC");
    // 指紋は DER 全体の SHA-256（32 バイト = 95 文字）。
    assert_eq!(info.fingerprint.len(), 32 * 3 - 1);
    assert!(info.fingerprint.split(':').all(|b| b.len() == 2
        && b.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_lowercase())));
    // notBefore は 1 時間前、notAfter は 10 年後の端まで有効。
    assert!(inspect_identity(
        identity.cert_pem.as_bytes(),
        identity.key_pem.as_bytes(),
        NOW - BACKDATE_SECS
    )
    .is_ok());
    assert!(inspect_identity(
        identity.cert_pem.as_bytes(),
        identity.key_pem.as_bytes(),
        not_after_for(NOW).unwrap()
    )
    .is_ok());
    // 中身の確かめ（x509-cert で直接）: CN・CA:FALSE・SAN は IP 127.0.0.1 だけ・serverAuth。
    use x509_cert::der::DecodePem;
    use x509_cert::ext::pkix::name::GeneralName;
    use x509_cert::ext::pkix::{BasicConstraints, ExtendedKeyUsage, SubjectAltName};
    let cert = x509_cert::Certificate::from_pem(identity.cert_pem.as_bytes()).unwrap();
    let tbs = &cert.tbs_certificate;
    assert_eq!(tbs.subject.to_string(), format!("CN={COMMON_NAME}"));
    let (_, basic) = tbs.get::<BasicConstraints>().unwrap().unwrap();
    assert!(!basic.ca);
    let (_, san) = tbs.get::<SubjectAltName>().unwrap().unwrap();
    assert_eq!(san.0.len(), 1);
    assert!(matches!(&san.0[0], GeneralName::IpAddress(ip) if ip.as_bytes() == [127, 0, 0, 1]));
    let (_, eku) = tbs.get::<ExtendedKeyUsage>().unwrap().unwrap();
    assert_eq!(eku.0.len(), 1);
    assert_eq!(eku.0[0].to_string(), "1.3.6.1.5.5.7.3.1");
    assert_eq!(
        tbs.validity.not_before.to_unix_duration().as_secs() as i64,
        NOW - BACKDATE_SECS
    );
    // 署名は ECDSA with SHA-256。
    assert_eq!(
        cert.signature_algorithm.oid.to_string(),
        "1.2.840.10045.4.3.2"
    );
    // 2 回作ると別の鍵。
    let other = generate_identity(NOW).unwrap();
    assert_ne!(other.cert_pem, identity.cert_pem);
}

#[test]
fn inspection_refuses_broken_or_unsafe_identities() {
    let identity = generate_identity(NOW).unwrap();
    let other = generate_identity(NOW).unwrap();
    let cert = identity.cert_pem.as_bytes();
    let key = identity.key_pem.as_bytes();
    let refused = |cert: &[u8], key: &[u8], now: i64, needle: &str| {
        let error = inspect_identity(cert, key, now).unwrap_err();
        assert!(error.contains(needle), "{needle}: {error}");
        assert!(!error.contains("PRIVATE KEY"), "{error}");
    };
    refused(cert, other.key_pem.as_bytes(), NOW, "対になっていません");
    refused(
        cert,
        key,
        not_after_for(NOW).unwrap() + 1,
        "期限が切れています",
    );
    refused(
        cert,
        key,
        NOW - BACKDATE_SECS - 1,
        "まだ有効になっていません",
    );
    refused(b"garbage", key, NOW, "ちょうど 1 つではありません（0 個）");
    let two = format!("{}{}", identity.cert_pem, other.cert_pem);
    refused(
        two.as_bytes(),
        key,
        NOW,
        "ちょうど 1 つではありません（2 個）",
    );
    let with_key = format!("{}{}", identity.cert_pem, identity.key_pem.as_str());
    refused(with_key.as_bytes(), key, NOW, "秘密鍵が入っています");
    refused(cert, cert, NOW, "秘密鍵の PEM を読めません");
    refused(cert, b"", NOW, "秘密鍵の PEM を読めません");

    // CA:TRUE・SAN に 127.0.0.1 が無い・EKU に serverAuth が無い・Basic Constraints が無い。
    let make = |edit: &dyn Fn(&mut rcgen::CertificateParams)| {
        let pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
        let mut params = rcgen::CertificateParams::default();
        params.subject_alt_names = vec![rcgen::SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST))];
        params.is_ca = rcgen::IsCa::ExplicitNoCa;
        params.not_before = time::OffsetDateTime::from_unix_timestamp(NOW - 10).unwrap();
        params.not_after = time::OffsetDateTime::from_unix_timestamp(NOW + 10).unwrap();
        edit(&mut params);
        (
            params.self_signed(&pair).unwrap().pem(),
            pair.serialize_pem(),
        )
    };
    let (ok_cert, ok_key) = make(&|_| {});
    assert!(inspect_identity(ok_cert.as_bytes(), ok_key.as_bytes(), NOW).is_ok());
    let (ca, ca_key) = make(&|p| p.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained));
    refused(ca.as_bytes(), ca_key.as_bytes(), NOW, "CA の証明書");
    let (no_bc, no_bc_key) = make(&|p| p.is_ca = rcgen::IsCa::NoCa);
    refused(
        no_bc.as_bytes(),
        no_bc_key.as_bytes(),
        NOW,
        "Basic Constraints",
    );
    let (dns, dns_key) = make(&|p| {
        p.subject_alt_names = vec![rcgen::SanType::DnsName(
            "localhost".to_string().try_into().unwrap(),
        )]
    });
    refused(
        dns.as_bytes(),
        dns_key.as_bytes(),
        NOW,
        "127.0.0.1 がありません",
    );
    let (client, client_key) =
        make(&|p| p.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth]);
    refused(client.as_bytes(), client_key.as_bytes(), NOW, "serverAuth");
    // Key Usage（レビュー P2）: あって digitalSignature が無ければ断る（無ければ上の ok_cert の
    // とおり通す）。
    let (encipher, encipher_key) =
        make(&|p| p.key_usages = vec![rcgen::KeyUsagePurpose::KeyEncipherment]);
    refused(
        encipher.as_bytes(),
        encipher_key.as_bytes(),
        NOW,
        "digitalSignature",
    );
    let (signing, signing_key) = make(&|p| {
        p.key_usages = vec![
            rcgen::KeyUsagePurpose::DigitalSignature,
            rcgen::KeyUsagePurpose::KeyEncipherment,
        ]
    });
    assert!(inspect_identity(signing.as_bytes(), signing_key.as_bytes(), NOW).is_ok());
    // P-384 の鍵は断る。
    let (p384, p384_key) = {
        let pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P384_SHA384).unwrap();
        let mut params = rcgen::CertificateParams::default();
        params.subject_alt_names = vec![rcgen::SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST))];
        params.is_ca = rcgen::IsCa::ExplicitNoCa;
        (
            params.self_signed(&pair).unwrap().pem(),
            pair.serialize_pem(),
        )
    };
    refused(p384.as_bytes(), p384_key.as_bytes(), NOW, "P-256");
}

/// 署名（レビュー P2）: 署名の 1 バイトを変えた証明書は、鍵と対で期限内でも断る。
#[test]
fn inspection_refuses_a_certificate_whose_signature_does_not_verify() {
    use x509_cert::der::{Decode, DecodePem, Encode, EncodePem};
    let identity = generate_identity(NOW).unwrap();
    let key = identity.key_pem.as_bytes();
    let mut der = x509_cert::Certificate::from_pem(identity.cert_pem.as_bytes())
        .unwrap()
        .to_der()
        .unwrap();
    // DER の最後は署名（BIT STRING の中の ECDSA の s）の最後のバイト。
    *der.last_mut().unwrap() ^= 0x01;
    let tampered = x509_cert::Certificate::from_der(&der)
        .unwrap()
        .to_pem(x509_cert::der::pem::LineEnding::LF)
        .unwrap();
    assert_ne!(tampered, identity.cert_pem);
    let error = inspect_identity(tampered.as_bytes(), key, NOW).unwrap_err();
    assert!(error.contains("自己署名"), "{error}");

    // 中身（tbs）の 1 バイトを変えても断る（notAfter などの書き換え）。署名は元のまま。
    let cert = x509_cert::Certificate::from_pem(identity.cert_pem.as_bytes()).unwrap();
    let mut changed = cert.clone();
    changed.tbs_certificate.serial_number =
        x509_cert::serial_number::SerialNumber::new(&[0x01, 0x02, 0x03]).unwrap();
    let changed = changed.to_pem(x509_cert::der::pem::LineEnding::LF).unwrap();
    let error = inspect_identity(changed.as_bytes(), key, NOW).unwrap_err();
    assert!(error.contains("自己署名"), "{error}");
}

/// 作った鍵と証明書を `tls_listener` の読み込み（`TlsConfig::explicit`）で読み、Sink と同じ pin
/// （`PinnedHubTls::from_pem`）のクライアントと IP 127.0.0.1 の名前で握手できる。別の証明書を
/// pin したクライアントは断られる。
#[tokio::test]
async fn generated_identity_serves_tls_to_a_pinned_client() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_rustls::rustls::pki_types::ServerName;

    let identity = generate_identity(real_now()).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let cert_path = temp.path().join(CERT_FILE);
    let key_path = temp.path().join(KEY_FILE);
    std::fs::write(&cert_path, &identity.cert_pem).unwrap();
    std::fs::write(&key_path, identity.key_pem.as_bytes()).unwrap();
    let config = crate::tls_listener::TlsConfig::explicit(0, cert_path, key_path).unwrap();

    let client_config = |pem: &str| {
        let pins = banto_tagclient::PinnedHubTls::from_pem(&[pem.to_string()]).unwrap();
        match pins.websocket_connector() {
            tokio_tungstenite::Connector::Rustls(config) => config,
            _ => panic!("PinnedHubTls は rustls の設定を返す"),
        }
    };
    let handshake = |client: std::sync::Arc<tokio_rustls::rustls::ClientConfig>| {
        let acceptor = config.acceptor().unwrap();
        async move {
            let (client_io, server_io) = tokio::io::duplex(16 * 1024);
            let connector = tokio_rustls::TlsConnector::from(client);
            let name = ServerName::try_from("127.0.0.1").unwrap();
            let (client, server) = tokio::join!(
                connector.connect(name, client_io),
                acceptor.accept(server_io)
            );
            (client, server)
        }
    };

    let (client, server) = handshake(client_config(&identity.cert_pem)).await;
    let mut client = client.unwrap();
    let mut server = server.unwrap();
    client.write_all(b"pinned-ok").await.unwrap();
    let mut marker = [0; 9];
    server.read_exact(&mut marker).await.unwrap();
    assert_eq!(&marker, b"pinned-ok");

    let other = generate_identity(real_now()).unwrap();
    let (client, _server) = handshake(client_config(&other.cert_pem)).await;
    assert!(client.is_err(), "別の証明書の pin は握手を断る");
}

// --- 手順: 作る ----------------------------------------------------------------------

/// 何も無い: ルート・`tls\` を作り、確かめ終えてから一時名で鍵（SYSTEM・Administrators だけ）・
/// 証明書（Users 読み取り）を作り、付け替える。出力に証明書のパス・指紋・期限があり、鍵は無い。
#[test]
fn creates_root_tls_dir_and_identity_from_nothing() {
    let ops = FakeOps::new();
    ops.dirs.borrow_mut().remove(&root());
    ops.dirs.borrow_mut().remove(&tls());
    let (result, lines) = run(&ops, false);
    assert_eq!(result, Ok(()), "{lines:#?}");
    assert_eq!(
        ops.writes(),
        vec![
            "CREATE dir BantoHub Inherit",
            "CREATE dir tls UsersRead",
            "CREATE file hub-key.pem.new ServiceOnly",
            "CREATE file hub-cert.pem.new UsersRead",
            "WRITE rename hub-key.pem.new hub-key.pem",
            "WRITE rename hub-cert.pem.new hub-cert.pem",
        ]
    );
    // ファイルを開く（確かめ）のは、ファイルを書く前に全部終わっている。
    let calls = ops.calls();
    let last_open = calls
        .iter()
        .rposition(|c| c.starts_with("open_file"))
        .unwrap();
    let first_file_write = calls
        .iter()
        .position(|c| c.starts_with("CREATE file"))
        .unwrap();
    assert!(last_open < first_file_write, "{calls:#?}");
    assert_eq!(ops.file_names(), vec![CERT_FILE, KEY_FILE]);
    let key = ops.file(KEY_FILE).unwrap();
    let cert = ops.file(CERT_FILE).unwrap();
    assert_eq!(exposure(&key.security), vec![]);
    let info = inspect_identity(&cert.contents, &key.contents, NOW).unwrap();
    let cert_path = tls().join(CERT_FILE);
    assert!(
        has(&lines, &format!("証明書: {}", cert_path.display())),
        "{lines:#?}"
    );
    assert!(has(&lines, &info.fingerprint), "{lines:#?}");
    assert!(has(&lines, "2036-11-13 00:00:00 UTC"), "{lines:#?}");
    assert!(has(&lines, "を作りました"), "{lines:#?}");
    assert!(!has(&lines, "残っています"), "{lines:#?}");
    assert_no_key_material(&lines, Some(std::str::from_utf8(&key.contents).unwrap()));
}

/// `tls\` はあるが空: 鍵と証明書だけを作る。
#[test]
fn creates_identity_in_an_existing_empty_tls_dir() {
    let ops = FakeOps::new();
    let (result, lines) = run(&ops, false);
    assert_eq!(result, Ok(()), "{lines:#?}");
    assert_eq!(ops.writes().len(), 4, "{:#?}", ops.writes());
    assert!(
        !has(&lines, "置き場（"),
        "作っていないフォルダを作ったと言わない: {lines:#?}"
    );
}

// --- 手順: 保つ ----------------------------------------------------------------------

/// 正しい鍵と証明書がある: 何も書かずに保ち、証明書の行を出す（再インストールの形）。
#[test]
fn keeps_a_valid_existing_identity_without_writing() {
    let (ops, identity) = FakeOps::with_identity();
    let (result, lines) = run(&ops, false);
    assert_eq!(result, Ok(()), "{lines:#?}");
    assert_eq!(ops.writes(), Vec::<String>::new());
    let info = inspect_identity(
        identity.cert_pem.as_bytes(),
        identity.key_pem.as_bytes(),
        NOW,
    )
    .unwrap();
    assert!(has(&lines, "確かめて保ちました"), "{lines:#?}");
    assert!(has(&lines, &info.fingerprint), "{lines:#?}");
    assert_eq!(
        ops.file(KEY_FILE).unwrap().contents,
        identity.key_pem.as_bytes()
    );
    assert_no_key_material(&lines, Some(&identity.key_pem));
}

/// 前回の途中の一時名は、確かめを通った後に消す（保つときも）。
#[test]
fn removes_leftover_temporaries_after_the_checks() {
    let (ops, _identity) = FakeOps::with_identity();
    ops.put(KEY_TEMP, FileEntry::with(FileAcl::ServiceOnly, b"half"));
    ops.put(CERT_TEMP, FileEntry::with(FileAcl::UsersRead, b"half"));
    let (result, lines) = run(&ops, false);
    assert_eq!(result, Ok(()), "{lines:#?}");
    assert_eq!(
        ops.writes(),
        vec![
            "WRITE remove hub-key.pem.new",
            "WRITE remove hub-cert.pem.new"
        ]
    );
    assert_eq!(ops.file_names(), vec![CERT_FILE, KEY_FILE]);
    assert!(has(&lines, "前回の途中で残った"), "{lines:#?}");

    // 作るときも、一時名を消してから作る。
    let ops = FakeOps::new();
    ops.put(CERT_TEMP, FileEntry::with(FileAcl::UsersRead, b"half"));
    let (result, lines) = run(&ops, false);
    assert_eq!(result, Ok(()), "{lines:#?}");
    assert_eq!(ops.writes()[0], "WRITE remove hub-cert.pem.new");
    assert_eq!(ops.file_names(), vec![CERT_FILE, KEY_FILE]);
}

/// 証明書に Users の読み取りがあるのは正しい形（保つ）。鍵は SYSTEM・Administrators だけ。
#[test]
fn users_read_on_the_certificate_is_expected() {
    let (ops, _identity) = FakeOps::with_identity();
    assert!(ops
        .file(CERT_FILE)
        .unwrap()
        .security
        .dacl
        .aces
        .unwrap()
        .iter()
        .any(|a| a.sid == USERS));
    let (result, _lines) = run(&ops, false);
    assert_eq!(result, Ok(()));
}

// --- 手順: 断る（既存） --------------------------------------------------------------

#[test]
fn refuses_when_only_one_of_the_pair_exists() {
    let (ops, _identity) = FakeOps::with_identity();
    ops.files.borrow_mut().remove(&tls().join(CERT_FILE));
    let lines = assert_refused_without_writes(&ops, false, "片方だけ");
    assert!(has(&lines, "--renew"), "{lines:#?}");

    let (ops, _identity) = FakeOps::with_identity();
    ops.files.borrow_mut().remove(&tls().join(KEY_FILE));
    assert_refused_without_writes(&ops, false, "片方だけ");
}

#[test]
fn refuses_an_exposed_key_but_renew_replaces_it() {
    for (edit, needle) in [
        (
            Box::new(|f: &mut FileEntry| {
                f.security
                    .dacl
                    .aces
                    .as_mut()
                    .unwrap()
                    .push(ace(USERS, READ_EXECUTE_ACCESS_MASK, 0))
            }) as Box<dyn Fn(&mut FileEntry)>,
            "S-1-5-32-545",
        ),
        (
            Box::new(|f: &mut FileEntry| f.security.dacl.owner = EVIL.to_string()),
            EVIL,
        ),
        (
            Box::new(|f: &mut FileEntry| f.security.links = 2),
            "ハードリンク",
        ),
        (
            Box::new(|f: &mut FileEntry| f.security.dacl.aces = None),
            "null",
        ),
    ] {
        let (ops, identity) = FakeOps::with_identity();
        ops.edit_file(KEY_FILE, |f| edit(f));
        let lines = assert_refused_without_writes(&ops, false, "秘密鍵");
        assert!(has(&lines, needle), "{needle}: {lines:#?}");
        assert!(has(&lines, "--renew"), "{lines:#?}");
        assert_no_key_material(&lines, Some(&identity.key_pem));

        // --renew は中身も ACL も確かめずに作り直す。
        let (result, lines) = run(&ops, true);
        assert_eq!(result, Ok(()), "{lines:#?}");
        assert_eq!(
            ops.writes(),
            vec![
                "CREATE file hub-key.pem.new ServiceOnly",
                "CREATE file hub-cert.pem.new UsersRead",
                "WRITE remove hub-cert.pem",
                "WRITE remove hub-key.pem",
                "WRITE rename hub-key.pem.new hub-key.pem",
                "WRITE rename hub-cert.pem.new hub-cert.pem",
            ]
        );
        let key = ops.file(KEY_FILE).unwrap();
        assert_ne!(key.contents, identity.key_pem.as_bytes());
        assert_eq!(exposure(&key.security), vec![]);
        assert!(has(&lines, "作り直しました"), "{lines:#?}");
        assert!(has(&lines, "hub_tls_pinned_certs"), "{lines:#?}");
    }
}

#[test]
fn refuses_a_certificate_writable_by_users() {
    let (ops, _identity) = FakeOps::with_identity();
    ops.edit_file(CERT_FILE, |f| {
        f.security
            .dacl
            .aces
            .as_mut()
            .unwrap()
            .push(ace(USERS, MODIFY_ACCESS_MASK, 0))
    });
    assert_refused_without_writes(&ops, false, "証明書（");
}

#[test]
fn refuses_broken_or_expired_identities() {
    // 期限切れ。
    let (mut ops, _identity) = FakeOps::with_identity();
    ops.now = not_after_for(NOW).unwrap() + 1;
    let lines = assert_refused_without_writes(&ops, false, "期限が切れています");
    assert!(has(&lines, "--renew"), "{lines:#?}");
    let (result, lines) = run(&ops, true);
    assert_eq!(result, Ok(()), "{lines:#?}");
    let renewed = ops.file(CERT_FILE).unwrap();
    let key = ops.file(KEY_FILE).unwrap();
    assert!(inspect_identity(&renewed.contents, &key.contents, ops.now).is_ok());

    // 壊れた鍵・対になっていない鍵。
    let (ops, _identity) = FakeOps::with_identity();
    ops.edit_file(KEY_FILE, |f| f.contents = b"broken".to_vec());
    assert_refused_without_writes(&ops, false, "秘密鍵の PEM を読めません");
    let (ops, _identity) = FakeOps::with_identity();
    let other = generate_identity(NOW).unwrap();
    ops.edit_file(KEY_FILE, |f| f.contents = other.key_pem.as_bytes().to_vec());
    assert_refused_without_writes(&ops, false, "対になっていません");
}

#[test]
fn refuses_directories_links_and_misplaced_files_even_with_renew() {
    for renew in [false, true] {
        for name in [KEY_FILE, CERT_FILE, KEY_TEMP, CERT_TEMP] {
            let (ops, _identity) = FakeOps::with_identity();
            ops.put(name, FileEntry::with(FileAcl::ServiceOnly, b""));
            ops.edit_file(name, |f| f.security.is_dir = true);
            assert_refused_without_writes(&ops, renew, "フォルダです");

            let (ops, _identity) = FakeOps::with_identity();
            ops.put(name, FileEntry::with(FileAcl::ServiceOnly, b""));
            ops.edit_file(name, |f| f.security.is_reparse_point = true);
            assert_refused_without_writes(&ops, renew, "reparse point");

            let (ops, _identity) = FakeOps::with_identity();
            ops.put(name, FileEntry::with(FileAcl::ServiceOnly, b""));
            ops.edit_file(name, |f| f.real = Some(abs("Elsewhere").join(name)));
            assert_refused_without_writes(&ops, renew, "期待と違います");
        }
    }
}

#[test]
fn refuses_files_held_open_for_writing() {
    let (mut ops, _identity) = FakeOps::with_identity();
    ops.open_error
        .insert(KEY_FILE.to_string(), FileOpenError::InUse);
    assert_refused_without_writes(&ops, false, "書き込みに開かれています");
    let (mut ops, _identity) = FakeOps::with_identity();
    ops.open_error.insert(
        CERT_TEMP.to_string(),
        FileOpenError::Other("拒否".to_string()),
    );
    assert_refused_without_writes(&ops, true, "開けませんでした: 拒否");
}

// --- 手順: 断る（置き場・ルート） ----------------------------------------------------

/// 一般の利用者が先に作った `tls\`: 何も変えずに断り、消してからの再実行を案内する（`--renew`
/// でも）。
#[test]
fn refuses_a_tls_dir_created_by_another_user() {
    for renew in [false, true] {
        let (ops, _identity) = FakeOps::with_identity();
        let mut entry = DirEntry::users_read();
        entry.owner = EVIL.to_string();
        ops.dirs.borrow_mut().insert(tls(), entry);
        let lines = assert_refused_without_writes(&ops, renew, "乗っ取り");
        assert!(has(&lines, "消して"), "{lines:#?}");
        // 中のファイルは開きもしない。
        assert!(!ops.calls().iter().any(|c| c.starts_with("open_file")));
    }
}

/// 一般の利用者が書ける `tls\`（Users の変更・Everyone など）・実行した管理者が所有者の
/// `tls\`: `--renew` でも断る。
#[test]
fn refuses_an_exposed_tls_dir_even_with_renew() {
    let cases: Vec<DirEdit> = vec![
        Box::new(|e| e.aces.push(ace(USERS, MODIFY_ACCESS_MASK, 0x03))),
        Box::new(|e| e.aces.push(ace("S-1-1-0", READ_EXECUTE_ACCESS_MASK, 0x03))),
        Box::new(|e| e.aces.push(ace("S-1-3-0", 0x1F01FF, 0x0B))),
        Box::new(|e| e.owner = ME.to_string()),
    ];
    for edit in cases {
        for renew in [false, true] {
            let (ops, _identity) = FakeOps::with_identity();
            let mut entry = DirEntry::users_read();
            edit(&mut entry);
            ops.dirs.borrow_mut().insert(tls(), entry);
            let lines = assert_refused_without_writes(&ops, renew, "保護されていません");
            assert!(has(&lines, "--renew でも作り直しません"), "{lines:#?}");
        }
    }
}

#[test]
fn refuses_a_tls_dir_that_is_a_link_or_elsewhere() {
    let ops = FakeOps::new();
    let mut entry = DirEntry::users_read();
    entry.is_reparse_point = true;
    ops.dirs.borrow_mut().insert(tls(), entry);
    assert_refused_without_writes(&ops, false, "reparse point");

    let ops = FakeOps::new();
    let mut entry = DirEntry::users_read();
    entry.real = Some(abs("Elsewhere/tls"));
    ops.dirs.borrow_mut().insert(tls(), entry);
    assert_refused_without_writes(&ops, true, "期待と違います");
}

/// 作った `tls\` が確かめを通らなければ、作ったことを知らせて断る（中は作らない）。
#[test]
fn reports_a_created_tls_dir_that_failed_the_checks() {
    let mut ops = FakeOps::new();
    ops.dirs.borrow_mut().remove(&tls());
    let mut odd = DirEntry::users_read();
    odd.aces.push(ace(USERS, MODIFY_ACCESS_MASK, 0x03));
    ops.on_create.insert(tls(), odd);
    let (result, lines) = run(&ops, false);
    assert_eq!(result, Err(Refused));
    assert_eq!(ops.writes(), vec!["CREATE dir tls UsersRead"]);
    assert!(
        has(&lines, "作りましたが、確かめが通らなかったので残っています"),
        "{lines:#?}"
    );
}

#[test]
fn refuses_a_root_owned_by_another_user_or_unknown_program_data() {
    let ops = FakeOps::new();
    ops.dirs
        .borrow_mut()
        .insert(root(), DirEntry::owned_by(EVIL));
    assert_refused_without_writes(&ops, false, "takeown");

    let mut ops = FakeOps::new();
    ops.env.program_data = Err("取れない".to_string());
    let lines = assert_refused_without_writes(&ops, false, "何も変えていません");
    assert!(has(&lines, "取れない"));
    assert!(ops.calls().is_empty(), "{:#?}", ops.calls());

    // ProgramData そのものがルートになる形は断る。
    let mut ops = FakeOps::new();
    ops.env.program_data = Ok(abs("Windows"));
    assert_refused_without_writes(&ops, false, "何も変えていません");
}

// --- ルートの DACL（レビュー P1） ----------------------------------------------------

const CREATOR_OWNER: &str = "S-1-3-0";

/// この PC の `%ProgramData%\BantoHub` の実物（ProgramData から継承）: SYSTEM・Administrators
/// `(I)(OI)(CI)(F)`、`CREATOR OWNER:(I)(OI)(CI)(IO)(F)`、`BUILTIN\Users:(I)(OI)(CI)(RX)`、
/// `BUILTIN\Users:(I)(CI)(WD,AD,WEA,WA)`。所有者 Administrators。
fn default_root_aces() -> Vec<ObservedAce> {
    vec![
        ace(SYSTEM_SID, 0x1F01FF, 0x13),
        ace(ADMINISTRATORS_SID, 0x1F01FF, 0x13),
        ace(CREATOR_OWNER, 0x1000_0000, 0x1B),
        ace(USERS, READ_EXECUTE_ACCESS_MASK, 0x13),
        ace(USERS, 0x0000_0116, 0x12),
    ]
}

fn root_security(owner: &str, aces: Option<Vec<ObservedAce>>) -> FileSecurity {
    FileSecurity {
        is_dir: true,
        is_reparse_point: false,
        links: 1,
        dacl: ObservedDacl {
            protected: false,
            owner: owner.to_string(),
            aces,
        },
    }
}

#[test]
fn root_dacl_accepts_the_default_program_data_shape() {
    assert_eq!(
        root_dacl_problems(&root_security(
            ADMINISTRATORS_SID,
            Some(default_root_aces())
        )),
        vec![]
    );
    // 所有者 SYSTEM・TrustedInstaller も通す。
    for owner in [SYSTEM_SID, banto_safe_root::acl::TRUSTED_INSTALLER_SID] {
        assert_eq!(
            root_dacl_problems(&root_security(owner, Some(default_root_aces()))),
            vec![]
        );
    }
    // inherit-only の ACE は、どの相手・どの権利でも無視する。
    let mut aces = default_root_aces();
    aces.push(ace(USERS, 0x1F01FF, 0x0B));
    aces.push(ace(EVIL, 0x1000_0000, 0x08));
    assert_eq!(
        root_dacl_problems(&root_security(ADMINISTRATORS_SID, Some(aces))),
        vec![]
    );
    // 拒否の ACE も理由にしない。
    let mut aces = default_root_aces();
    aces.push(ObservedAce {
        ace_type: 1,
        flags: 0,
        mask: 0x1F01FF,
        sid: EVIL.to_string(),
    });
    assert_eq!(
        root_dacl_problems(&root_security(ADMINISTRATORS_SID, Some(aces))),
        vec![]
    );
}

#[test]
fn root_dacl_refuses_takeover_rights_owners_and_unreadable_aces() {
    // オブジェクト自身に効く危ない権利（1 bit ずつ、継承あり・なし）。
    for bit in [
        0x0001_0000u32, // DELETE
        0x0004_0000,    // WRITE_DAC
        0x0008_0000,    // WRITE_OWNER
        0x0000_0040,    // FILE_DELETE_CHILD
        0x1000_0000,    // GENERIC_ALL
        0x4000_0000,    // GENERIC_WRITE
        0x0200_0000,    // MAXIMUM_ALLOWED
    ] {
        for (sid, flags) in [
            (USERS, 0x00),
            (USERS, 0x13),
            (EVIL, 0x02),
            ("S-1-1-0", 0x10),
        ] {
            let mut aces = default_root_aces();
            aces.push(ace(sid, bit, flags));
            let found = root_dacl_problems(&root_security(ADMINISTRATORS_SID, Some(aces)));
            assert_eq!(
                found,
                vec![Exposure::AllowsOther {
                    ace_type: 0,
                    flags,
                    mask: bit,
                    sid: sid.to_string(),
                }],
                "{bit:#x} {sid} {flags:#x}"
            );
        }
    }
    // ほかの相手へのフル・変更も断る。
    let mut aces = default_root_aces();
    aces.push(ace(USERS, MODIFY_ACCESS_MASK, 0x13));
    assert_eq!(
        root_dacl_problems(&root_security(ADMINISTRATORS_SID, Some(aces))).len(),
        1
    );
    // 所有者が昇格した本人・一般の利用者。
    for owner in [ME, EVIL] {
        assert_eq!(
            root_dacl_problems(&root_security(owner, Some(default_root_aces()))),
            vec![Exposure::ForeignOwner(owner.to_string())]
        );
    }
    // null DACL。
    assert_eq!(
        root_dacl_problems(&root_security(ADMINISTRATORS_SID, None)),
        vec![Exposure::NullDacl]
    );
    // 条件付き（9）・オブジェクト（5）の許可は、読み取りだけ・inherit-only でも断る。
    for (ace_type, flags) in [(9u8, 0x00u8), (5, 0x0B)] {
        let mut aces = default_root_aces();
        aces.push(ObservedAce {
            ace_type,
            flags,
            mask: READ_EXECUTE_ACCESS_MASK,
            sid: USERS.to_string(),
        });
        assert_eq!(
            root_dacl_problems(&root_security(ADMINISTRATORS_SID, Some(aces))).len(),
            1,
            "{ace_type}"
        );
    }
    // 種類の分からない ACE・SID が読めない ACE。
    for (ace_type, sid) in [(0x7Fu8, ""), (0, "")] {
        let mut aces = default_root_aces();
        aces.push(ObservedAce {
            ace_type,
            flags: 0,
            mask: READ_EXECUTE_ACCESS_MASK,
            sid: sid.to_string(),
        });
        assert_eq!(
            root_dacl_problems(&root_security(ADMINISTRATORS_SID, Some(aces))).len(),
            1,
            "{ace_type}"
        );
    }
}

/// ルートが露出していれば、`tls\` を開きも作りもせずに断る（`--renew` でも）。既定の形の
/// ルートは通る。
#[test]
fn refuses_an_exposed_root_without_opening_the_tls_dir() {
    let cases: Vec<DirEdit> = vec![
        Box::new(|e| e.aces.push(ace(USERS, 0x0000_0040, 0x13))),
        Box::new(|e| e.aces.push(ace(USERS, 0x0001_0000, 0x00))),
        Box::new(|e| e.aces.push(ace("S-1-1-0", 0x1000_0000, 0x03))),
        Box::new(|e| e.owner = ME.to_string()),
    ];
    for edit in cases {
        for (renew, tls_exists) in [(false, true), (true, true), (false, false)] {
            let (ops, _identity) = FakeOps::with_identity();
            let mut entry = DirEntry::owned_by(ADMINISTRATORS_SID);
            entry.aces = default_root_aces();
            edit(&mut entry);
            ops.dirs.borrow_mut().insert(root(), entry);
            if !tls_exists {
                ops.dirs.borrow_mut().remove(&tls());
            }
            let lines = assert_refused_without_writes(&ops, renew, "Hub のルート");
            assert!(has(&lines, "保護されていません"), "{lines:#?}");
            assert!(has(&lines, "何も変えずに止めます"), "{lines:#?}");
            assert!(
                !ops.calls().iter().any(|c| c.contains("tls")),
                "{:#?}",
                ops.calls()
            );
        }
    }

    let (ops, _identity) = FakeOps::with_identity();
    let mut entry = DirEntry::owned_by(ADMINISTRATORS_SID);
    entry.aces = default_root_aces();
    ops.dirs.borrow_mut().insert(root(), entry);
    let (result, lines) = run(&ops, false);
    assert_eq!(result, Ok(()), "{lines:#?}");
}

// --- 手順: 書く途中の失敗 ------------------------------------------------------------

#[test]
fn failing_to_create_the_certificate_removes_the_key_temporary() {
    let mut ops = FakeOps::new();
    ops.create_error
        .insert(CERT_TEMP.to_string(), "書けない".to_string());
    let (result, lines) = run(&ops, false);
    assert_eq!(result, Err(Refused));
    assert_eq!(
        ops.writes(),
        vec![
            "CREATE file hub-key.pem.new ServiceOnly",
            "CREATE file hub-cert.pem.new UsersRead",
            "WRITE remove hub-key.pem.new",
        ]
    );
    assert!(ops.file_names().is_empty());
    assert!(has(&lines, "書けない"), "{lines:#?}");
    assert!(has(&lines, "次の実行で消します"), "{lines:#?}");
}

#[test]
fn failing_to_remove_the_old_pair_keeps_the_temporaries_for_the_next_run() {
    let (mut ops, identity) = FakeOps::with_identity();
    ops.remove_error
        .insert(KEY_FILE.to_string(), "消せない".to_string());
    let (result, lines) = run(&ops, true);
    assert_eq!(result, Err(Refused));
    assert!(has(&lines, "消せない"), "{lines:#?}");
    assert_eq!(ops.file_names(), vec![CERT_TEMP, KEY_FILE, KEY_TEMP]);
    assert_eq!(
        ops.file(KEY_FILE).unwrap().contents,
        identity.key_pem.as_bytes()
    );
    // 次の --renew は一時名を消してから作り直す。
    ops.remove_error.clear();
    let (result, lines) = run(&ops, true);
    assert_eq!(result, Ok(()), "{lines:#?}");
    assert_eq!(ops.file_names(), vec![CERT_FILE, KEY_FILE]);
}

/// 鍵だけ付け替わって止まった形（証明書が一時名のまま）: 次の実行は「片方だけ」で断り、
/// `--renew` で直る。
#[test]
fn a_crash_between_the_renames_is_recovered_by_renew() {
    let mut ops = FakeOps::new();
    ops.rename_error
        .insert(CERT_TEMP.to_string(), "止まった".to_string());
    let (result, lines) = run(&ops, false);
    assert_eq!(result, Err(Refused));
    assert!(has(&lines, "--renew"), "{lines:#?}");
    assert_eq!(ops.file_names(), vec![CERT_TEMP, KEY_FILE]);
    ops.rename_error.clear();
    ops.log.borrow_mut().clear();
    assert_refused_without_writes(&ops, false, "片方だけ");
    let (result, lines) = run(&ops, true);
    assert_eq!(result, Ok(()), "{lines:#?}");
    assert_eq!(ops.file_names(), vec![CERT_FILE, KEY_FILE]);
    let key = ops.file(KEY_FILE).unwrap();
    let cert = ops.file(CERT_FILE).unwrap();
    assert!(inspect_identity(&cert.contents, &key.contents, NOW).is_ok());
}

/// 一時名の作成で既にあった（確かめた後に誰かが作った）: 断る。
#[test]
fn refuses_when_a_temporary_appears_after_the_checks() {
    let mut ops = FakeOps::new();
    ops.create_exists.push(KEY_TEMP.to_string());
    let (result, lines) = run(&ops, false);
    assert_eq!(result, Err(Refused));
    assert!(has(&lines, "既にありました"), "{lines:#?}");
    assert_eq!(
        ops.writes(),
        vec!["CREATE file hub-key.pem.new ServiceOnly"]
    );

    let mut ops = FakeOps::new();
    ops.create_exists.push(CERT_TEMP.to_string());
    let (result, _lines) = run(&ops, false);
    assert_eq!(result, Err(Refused));
    assert_eq!(ops.writes().last().unwrap(), "WRITE remove hub-key.pem.new");
    assert!(ops.file_names().is_empty());
}

#[test]
fn refuses_without_touching_anything_when_another_run_holds_the_lock() {
    let mut ops = FakeOps::new();
    ops.lock = LockState::Held;
    let lines = assert_refused_without_writes(&ops, false, "別の prepare-hub-tls が実行中");
    assert!(has(&lines, "何も変えていません"), "{lines:#?}");
    // mutex を取りに行っただけで、フォルダは何も開いていない。
    assert_eq!(ops.calls(), vec!["lock".to_string()]);
    assert!(ops.file_names().is_empty());
}

#[test]
fn refuses_without_touching_anything_when_the_lock_cannot_be_taken() {
    let mut ops = FakeOps::new();
    ops.lock = LockState::Error;
    let lines = assert_refused_without_writes(&ops, true, "を取れませんでした");
    assert!(has(&lines, "拒否されました"), "{lines:#?}");
    assert_eq!(ops.calls(), vec!["lock".to_string()]);
    assert!(ops.file_names().is_empty());
}
