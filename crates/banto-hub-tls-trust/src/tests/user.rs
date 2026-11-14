//! 利用者ごとの置き場（`crate::user`、#1121 PR1a）を偽物の [`UserTlsOps`] で確かめる（全 OS）:
//! 既定の置き場のパス・確かめの順序・「無い」と「あるが通らない」の区別・断る分岐ごとの理由と
//! 対処の種類。最後に、実 Windows の `%LOCALAPPDATA%` の既定の ACL で通ることを 1 つ確かめる。

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use banto_safe_root::acl::{
    FileSecurity, ObservedAce, ObservedDacl, ADMINISTRATORS_SID, FULL_ACCESS_MASK,
    MODIFY_ACCESS_MASK, READ_EXECUTE_ACCESS_MASK, SYSTEM_SID,
};
use banto_safe_root::guard::DriveKind;

use super::{abs, ace, cert_pem, short, DirEntry, FakeDir, FakeEnv, FakeFile, FileEntry, EVIL};
use crate::*;

/// 今の利用者。
const ME: &str = "S-1-5-21-1-2-3-1001";

fn local() -> PathBuf {
    abs("Users/me/AppData/Local")
}
fn product() -> PathBuf {
    local().join(PRODUCT_DIR)
}
fn user_tls() -> PathBuf {
    product().join(TLS_DIR)
}
fn rev_a() -> PathBuf {
    product().join(USER_REVOCATIONS_A_DIR)
}
fn rev_b() -> PathBuf {
    local().join(USER_REVOCATIONS_B_DIR)
}

/// `%LOCALAPPDATA%` の既定の形（SYSTEM・Administrators・本人にフル）。
fn user_aces(flags: u8) -> Vec<ObservedAce> {
    vec![
        ace(SYSTEM_SID, FULL_ACCESS_MASK, flags),
        ace(ADMINISTRATORS_SID, FULL_ACCESS_MASK, flags),
        ace(ME, FULL_ACCESS_MASK, flags),
    ]
}

fn user_dir() -> DirEntry {
    DirEntry {
        owner: ME.to_string(),
        is_reparse_point: false,
        real: None,
        aces: user_aces(0x03),
    }
}

fn user_file(contents: &[u8]) -> FileEntry {
    FileEntry {
        security: FileSecurity {
            is_dir: false,
            is_reparse_point: false,
            links: 1,
            dacl: ObservedDacl {
                protected: false,
                owner: ME.to_string(),
                aces: Some(user_aces(0x10)),
            },
        },
        real: None,
        contents: contents.to_vec(),
    }
}

/// 利用者の置き場の偽物（読み取りだけ。開いた順を記録する）。
struct FakeUserOps {
    env: FakeEnv,
    known_local: Result<PathBuf, String>,
    sid: Result<String, String>,
    drive: Result<DriveKind, String>,
    dirs: HashMap<PathBuf, DirEntry>,
    files: HashMap<PathBuf, FileEntry>,
    open_error: HashMap<String, FileOpenError>,
    log: RefCell<Vec<String>>,
}

impl FakeUserOps {
    /// 既定: `%LOCALAPPDATA%` だけがある（デスクトップ版の Hub を動かしたことが無い利用者）。
    fn empty() -> Self {
        let mut dirs = HashMap::new();
        dirs.insert(local(), user_dir());
        Self {
            env: FakeEnv,
            known_local: Ok(local()),
            sid: Ok(ME.to_string()),
            drive: Ok(DriveKind::Fixed),
            dirs,
            files: HashMap::new(),
            open_error: HashMap::new(),
            log: RefCell::new(Vec::new()),
        }
    }

    /// 4 つの置き場がすべてあり、正しい形の証明書と鍵が置いてある。
    fn new() -> Self {
        let mut ops = Self::empty();
        for dir in [product(), user_tls(), rev_a(), rev_b()] {
            ops.dirs.insert(dir, user_dir());
        }
        ops.files
            .insert(user_tls().join(CERT_FILE), user_file(cert_pem().as_bytes()));
        // 鍵の中身は読まない（読もうとしたら `read_all` が大きさで断る）。
        ops.files.insert(
            user_tls().join(KEY_FILE),
            user_file(&vec![b'k'; MAX_PEM_BYTES + 1]),
        );
        ops.files
            .insert(local().join(USER_REVOCATIONS_LOCK_FILE), user_file(b""));
        ops
    }

    fn place(&self) -> DefaultUserTlsPlace {
        DefaultUserTlsPlace::new(local(), ME.to_string())
    }

    fn dir(&mut self, path: PathBuf) -> &mut DirEntry {
        self.dirs.get_mut(&path).unwrap()
    }

    fn file(&mut self, path: PathBuf) -> &mut FileEntry {
        self.files.get_mut(&path).unwrap()
    }

    fn cert(&mut self) -> &mut FileEntry {
        self.file(user_tls().join(CERT_FILE))
    }

    fn key(&mut self) -> &mut FileEntry {
        self.file(user_tls().join(KEY_FILE))
    }

    fn calls(&self) -> Vec<String> {
        self.log.borrow().clone()
    }

    fn load(&self) -> UserCertPin {
        load_pinned_user_cert(self, &self.place())
    }
}

impl TlsTrustOps for FakeUserOps {
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

impl UserTlsOps for FakeUserOps {
    fn local_app_data(&self) -> Result<PathBuf, String> {
        self.known_local.clone()
    }
    fn current_user_sid(&self) -> Result<String, String> {
        self.sid.clone()
    }
    fn drive_kind(&self, real: &Path) -> Result<DriveKind, String> {
        self.log.borrow_mut().push(format!("drive {}", short(real)));
        self.drive.clone()
    }
}

/// 「あるが通らない」で、理由に `needle` を含み、対処が `remedy`。
fn assert_unusable(ops: &FakeUserOps, needle: &str, remedy: UserRemedy) -> UserPlaceProblem {
    let UserCertPin::Unusable(found) = ops.load() else {
        panic!("断るはず（{needle}）: {:#?}", ops.load());
    };
    assert!(found.reason.contains(needle), "{needle} が無い: {found:#?}");
    assert_eq!(found.remedy, remedy, "{found:#?}");
    found
}

fn open(
    ops: &FakeUserOps,
    kind: UserPlaceKind,
) -> Result<UserPlaceState<FakeDir>, UserPlaceProblem> {
    open_user_place(ops, &ops.place(), kind)
}

fn present(ops: &FakeUserOps, kind: UserPlaceKind) -> CheckedUserPlace<FakeDir> {
    match open(ops, kind) {
        Ok(UserPlaceState::Present(checked)) => checked,
        Ok(UserPlaceState::Absent(missing)) => panic!("無い: {:?}", missing.missing()),
        Err(found) => panic!("通らない: {found:#?}"),
    }
}

fn absent(ops: &FakeUserOps, kind: UserPlaceKind) -> MissingUserPlace<FakeDir> {
    match open(ops, kind) {
        Ok(UserPlaceState::Absent(missing)) => missing,
        Ok(UserPlaceState::Present(_)) => panic!("あるはずがない"),
        Err(found) => panic!("通らない: {found:#?}"),
    }
}

fn refused(ops: &FakeUserOps, kind: UserPlaceKind, needle: &str, remedy: UserRemedy) {
    match open(ops, kind) {
        Err(found) => {
            assert!(found.reason.contains(needle), "{needle} が無い: {found:#?}");
            assert_eq!(found.remedy, remedy, "{found:#?}");
        }
        Ok(_) => panic!("断るはず（{needle}）"),
    }
}

// --- 置き場の指定 -------------------------------------------------------------------

#[test]
fn default_place_returns_the_four_places_under_local_app_data() {
    let place = DefaultUserTlsPlace::new(local(), ME.to_string());
    let path = |kind| place.spec(kind).path();
    assert_eq!(
        path(UserPlaceKind::Tls),
        local().join("BantoHub").join("tls")
    );
    assert_eq!(
        path(UserPlaceKind::RevocationsA),
        local().join("BantoHub").join("revocations")
    );
    assert_eq!(
        path(UserPlaceKind::RevocationsB),
        local().join("BantoHubRevocations")
    );
    assert_eq!(
        path(UserPlaceKind::RevocationsLock),
        local().join("BantoHubRevocations.lock")
    );
    for kind in UserPlaceKind::ALL {
        let spec = place.spec(kind);
        assert_eq!(spec.anchor, local());
        assert_eq!(spec.owners, vec![ME.to_string()]);
        assert!(spec.policy.anchor_is_local_app_data);
        assert!(spec.policy.require_local_fixed_drive);
        assert_eq!(
            spec.file.is_some(),
            kind == UserPlaceKind::RevocationsLock,
            "{kind:?}"
        );
        check_spec(&spec).unwrap();
    }
    // ロックファイルを入れるフォルダは起点そのもの（A・B の外）。
    assert_eq!(
        place.spec(UserPlaceKind::RevocationsLock).dir_path(),
        local()
    );
    check_place_layout(&place).unwrap();
}

#[test]
fn default_place_resolves_the_known_folder_and_the_token_user() {
    let ops = FakeUserOps::empty();
    assert_eq!(
        DefaultUserTlsPlace::resolve(&ops).unwrap(),
        DefaultUserTlsPlace::new(local(), ME.to_string())
    );

    let mut ops = FakeUserOps::empty();
    ops.known_local = Err("取れない".to_string());
    let found = DefaultUserTlsPlace::resolve(&ops).unwrap_err();
    assert!(found.reason.contains("%LOCALAPPDATA%"), "{found:#?}");
    assert_eq!(found.remedy, UserRemedy::Retry);

    let mut ops = FakeUserOps::empty();
    ops.sid = Err("取れない".to_string());
    let found = DefaultUserTlsPlace::resolve(&ops).unwrap_err();
    assert!(found.reason.contains("SID"), "{found:#?}");
    assert_eq!(found.remedy, UserRemedy::Retry);
}

/// 差し替えた置き場（テスト用。既定の指定を `edit` で書き換える）。
struct Edited<F: Fn(UserPlaceKind, &mut UserPlaceSpec)>(F);

impl<F: Fn(UserPlaceKind, &mut UserPlaceSpec)> UserTlsPlace for Edited<F> {
    fn spec(&self, kind: UserPlaceKind) -> UserPlaceSpec {
        let mut spec = DefaultUserTlsPlace::new(local(), ME.to_string()).spec(kind);
        (self.0)(kind, &mut spec);
        spec
    }
}

#[test]
fn layout_check_refuses_overlapping_or_malformed_places() {
    let names = |names: &[&str]| -> Vec<String> { names.iter().map(|n| n.to_string()).collect() };
    let refuse = |edit: &dyn Fn(UserPlaceKind, &mut UserPlaceSpec), needle: &str| {
        let found = check_place_layout(&Edited(edit)).expect_err(needle);
        assert!(found.contains(needle), "{needle} が無い: {found}");
    };
    // B が A と同じ・A の中。
    refuse(
        &|kind, spec| {
            if kind == UserPlaceKind::RevocationsB {
                spec.dirs = names(&[PRODUCT_DIR, USER_REVOCATIONS_A_DIR]);
            }
        },
        "同じか、片方がもう片方の中",
    );
    refuse(
        &|kind, spec| {
            if kind == UserPlaceKind::RevocationsB {
                spec.dirs = names(&[PRODUCT_DIR, USER_REVOCATIONS_A_DIR, "b"]);
            }
        },
        "同じか、片方がもう片方の中",
    );
    // A が tls\ の中。
    refuse(
        &|kind, spec| {
            if kind == UserPlaceKind::RevocationsA {
                spec.dirs = names(&[PRODUCT_DIR, TLS_DIR, "revocations"]);
            }
        },
        "同じか、片方がもう片方の中",
    );
    // ロックファイルが B の中。
    refuse(
        &|kind, spec| {
            if kind == UserPlaceKind::RevocationsLock {
                spec.dirs = names(&[USER_REVOCATIONS_B_DIR]);
            }
        },
        "の中にあります",
    );
    // 種類が違う（ロックがフォルダ、A がファイル）。
    refuse(
        &|kind, spec| {
            if kind == UserPlaceKind::RevocationsLock {
                spec.file = None;
                spec.dirs = names(&["lock"]);
            }
        },
        "種類",
    );
    // 名前が 1 要素でない・起点が相対・起点そのもの。
    refuse(
        &|kind, spec| {
            if kind == UserPlaceKind::Tls {
                spec.dirs = names(&["..", "tls"]);
            }
        },
        "普通の 1 要素ではありません",
    );
    refuse(
        &|_, spec| spec.anchor = PathBuf::from("relative"),
        "絶対パスではありません",
    );
    refuse(
        &|kind, spec| {
            if kind == UserPlaceKind::Tls {
                spec.dirs.clear();
            }
        },
        "そのものは置き場にできません",
    );
}

// --- 読み取りの入口 -----------------------------------------------------------------

#[test]
fn loads_the_user_certificate_after_checking_every_folder_and_the_key() {
    let ops = FakeUserOps::new();
    let expected = ops.files[&user_tls().join(CERT_FILE)].contents.clone();
    let UserCertPin::Usable(pin) = ops.load() else {
        panic!("通るはず: {:#?}", ops.load());
    };
    assert_eq!(pin.cert_pem, expected);
    assert_eq!(pin.cert_path, user_tls().join(CERT_FILE));
    assert_eq!(
        ops.calls(),
        vec![
            "pin Local",
            "drive Local",
            "open_dir BantoHub",
            "open_dir tls",
            "open_file hub-cert.pem",
            "open_file hub-key.pem",
        ]
    );
}

#[test]
fn system_or_administrators_owned_places_pass() {
    for owner in [SYSTEM_SID, ADMINISTRATORS_SID] {
        let mut ops = FakeUserOps::new();
        for dir in [local(), product(), user_tls()] {
            ops.dir(dir).owner = owner.to_string();
        }
        ops.cert().security.dacl.owner = owner.to_string();
        ops.key().security.dacl.owner = owner.to_string();
        assert!(matches!(ops.load(), UserCertPin::Usable(_)), "{owner}");
    }
}

#[test]
fn a_missing_place_is_absent_not_unusable() {
    // BantoHub\ が無い（デスクトップ版を動かしたことが無い）。
    let ops = FakeUserOps::empty();
    assert_eq!(ops.load(), UserCertPin::Absent);
    assert_eq!(
        ops.calls(),
        vec!["pin Local", "drive Local", "open_dir BantoHub"]
    );
    let missing = absent(&ops, UserPlaceKind::Tls);
    assert_eq!(missing.missing(), ["BantoHub", "tls"]);
    assert_eq!(missing.deepest_path(), local());
    assert_eq!(missing.deepest_real(), local());
    assert_eq!(missing.deepest().path, local());
    assert_eq!(missing.owners(), [ME.to_string()]);

    // tls\ だけが無い。
    let mut ops = FakeUserOps::new();
    ops.dirs.remove(&user_tls());
    assert_eq!(ops.load(), UserCertPin::Absent);
    let missing = absent(&ops, UserPlaceKind::Tls);
    assert_eq!(missing.missing(), ["tls"]);
    assert_eq!(missing.deepest_path(), product());

    // tls\ はあるが証明書が無い（鍵の有無は問わない）。
    let mut ops = FakeUserOps::new();
    ops.files.remove(&user_tls().join(CERT_FILE));
    assert_eq!(ops.load(), UserCertPin::Absent);
    assert_eq!(ops.calls().last().unwrap(), "open_file hub-cert.pem");
}

#[test]
fn a_missing_anchor_is_refused() {
    let mut ops = FakeUserOps::new();
    ops.dirs.remove(&local());
    assert_unusable(&ops, "がありません", UserRemedy::FixAnchor(local()));
    assert_eq!(ops.calls(), vec!["pin Local"]);
}

#[test]
fn a_certificate_without_a_key_is_unusable() {
    let mut ops = FakeUserOps::new();
    ops.files.remove(&user_tls().join(KEY_FILE));
    assert_unusable(&ops, "鍵（hub-key.pem）がありません", UserRemedy::Renew);
}

#[test]
fn refuses_a_foreign_owner_anywhere_on_the_way() {
    // 起点。
    let mut ops = FakeUserOps::new();
    ops.dir(local()).owner = EVIL.to_string();
    let found = assert_unusable(&ops, "所有者", UserRemedy::FixAnchor(local()));
    assert!(found.reason.contains("利用者本人"), "{found:#?}");
    assert_eq!(ops.calls(), vec!["pin Local"]);

    // BantoHub\（別のアカウントが先に作った）。tls\ は開かない。
    let mut ops = FakeUserOps::new();
    ops.dir(product()).owner = EVIL.to_string();
    assert_unusable(&ops, "所有者", UserRemedy::RecreateDir(product()));
    assert_eq!(ops.calls().last().unwrap(), "open_dir BantoHub");

    // tls\。
    let mut ops = FakeUserOps::new();
    ops.dir(user_tls()).owner = EVIL.to_string();
    let found = assert_unusable(&ops, "所有者", UserRemedy::RecreateDir(user_tls()));
    assert!(found.reason.contains("利用者の TLS の置き場"), "{found:#?}");

    // 証明書・鍵。
    let mut ops = FakeUserOps::new();
    ops.cert().security.dacl.owner = EVIL.to_string();
    assert_unusable(&ops, "証明書", UserRemedy::Renew);
    let mut ops = FakeUserOps::new();
    ops.key().security.dacl.owner = EVIL.to_string();
    assert_unusable(&ops, "鍵", UserRemedy::Renew);

    // 「今の利用者」は置き場の指定の owners で決まる（別の利用者の SID の置き場は通らない）。
    let ops = FakeUserOps::new();
    let other = DefaultUserTlsPlace::new(local(), EVIL.to_string());
    assert!(matches!(
        load_pinned_user_cert(&ops, &other),
        UserCertPin::Unusable(_)
    ));
}

#[test]
fn refuses_a_loosened_dacl_on_any_folder() {
    // 書き込み・削除・WRITE_DAC・WRITE_OWNER のどれか 1 つでも、3 者以外に許していれば断る。
    let loose_masks = [
        MODIFY_ACCESS_MASK,
        0x0001_0000, // DELETE
        0x0004_0000, // WRITE_DAC
        0x0008_0000, // WRITE_OWNER
        0x0000_0002, // FILE_ADD_FILE
        0x0000_0040, // FILE_DELETE_CHILD
        0x4000_0000, // GENERIC_WRITE
        0x1000_0000, // GENERIC_ALL
    ];
    for (dir, remedy) in [
        (local(), UserRemedy::FixAnchor(local())),
        (product(), UserRemedy::RecreateDir(product())),
        (user_tls(), UserRemedy::RecreateDir(user_tls())),
    ] {
        for mask in loose_masks {
            let mut ops = FakeUserOps::new();
            ops.dir(dir.clone()).aces.push(ace(EVIL, mask, 0x03));
            assert_unusable(&ops, "は保護されていません", remedy.clone());
        }
        // 継承だけの ACE（inherit-only）でも、書き込みを含めば断る。
        let mut ops = FakeUserOps::new();
        ops.dir(dir.clone())
            .aces
            .push(ace(EVIL, MODIFY_ACCESS_MASK, 0x0B));
        assert_unusable(&ops, "は保護されていません", remedy.clone());
        // 読み取りだけなら通す（フォルダは 3 者以外が読めてもよい）。
        let mut ops = FakeUserOps::new();
        ops.dir(dir.clone())
            .aces
            .push(ace(EVIL, READ_EXECUTE_ACCESS_MASK, 0x03));
        // 鍵には継承していない形（鍵そのものの ACL は別に確かめる）。
        assert!(matches!(ops.load(), UserCertPin::Usable(_)), "{dir:?}");
    }
}

#[test]
fn refuses_an_exposed_key_but_allows_a_readable_certificate() {
    // 鍵が 3 者以外に読める（露出）。
    let mut ops = FakeUserOps::new();
    if let Some(aces) = ops.key().security.dacl.aces.as_mut() {
        aces.push(ace(EVIL, READ_EXECUTE_ACCESS_MASK, 0x10));
    }
    let found = assert_unusable(&ops, "ほかの利用者に読まれた恐れ", UserRemedy::Renew);
    assert!(found.reason.contains("鍵（"), "{found:#?}");

    // 証明書は 3 者以外が読めてもよい。書き換えられるなら断る。
    let mut ops = FakeUserOps::new();
    if let Some(aces) = ops.cert().security.dacl.aces.as_mut() {
        aces.push(ace(EVIL, READ_EXECUTE_ACCESS_MASK, 0x10));
    }
    assert!(matches!(ops.load(), UserCertPin::Usable(_)));
    let mut ops = FakeUserOps::new();
    if let Some(aces) = ops.cert().security.dacl.aces.as_mut() {
        aces.push(ace(EVIL, MODIFY_ACCESS_MASK, 0x10));
    }
    assert_unusable(&ops, "ほかの利用者に書き換えられた恐れ", UserRemedy::Renew);
    // 鍵は開かずに断る（証明書が先）。
    assert_eq!(ops.calls().last().unwrap(), "open_file hub-cert.pem");

    // null DACL の鍵・ハードリンクの鍵。
    let mut ops = FakeUserOps::new();
    ops.key().security.dacl.aces = None;
    assert_unusable(&ops, "DACL が null", UserRemedy::Renew);
    let mut ops = FakeUserOps::new();
    ops.key().security.links = 2;
    assert_unusable(&ops, "ハードリンク", UserRemedy::Renew);
}

#[test]
fn refuses_reparse_points_and_moved_places() {
    // 起点・途中・置き場が reparse point。
    for (dir, remedy) in [
        (local(), UserRemedy::FixAnchor(local())),
        (product(), UserRemedy::RecreateDir(product())),
        (user_tls(), UserRemedy::RecreateDir(user_tls())),
    ] {
        let mut ops = FakeUserOps::new();
        ops.dir(dir).is_reparse_point = true;
        assert_unusable(&ops, "reparse point", remedy);
    }

    // 本当の場所が別（途中のジャンクション・フォルダの転送）。
    let mut ops = FakeUserOps::new();
    ops.dir(local()).real = Some(abs("Elsewhere/Local"));
    assert_unusable(&ops, "本当の場所", UserRemedy::FixAnchor(local()));
    let mut ops = FakeUserOps::new();
    ops.dir(user_tls()).real = Some(abs("Users/evil/tls"));
    assert_unusable(&ops, "本当の場所", UserRemedy::RecreateDir(user_tls()));

    // 証明書・鍵がリンク・フォルダ・別の場所。
    let mut ops = FakeUserOps::new();
    ops.cert().security.is_reparse_point = true;
    assert_unusable(&ops, "reparse point", UserRemedy::RecreateDir(user_tls()));
    let mut ops = FakeUserOps::new();
    ops.key().security.is_reparse_point = true;
    assert_unusable(&ops, "reparse point", UserRemedy::RecreateDir(user_tls()));
    let mut ops = FakeUserOps::new();
    ops.key().security.is_dir = true;
    assert_unusable(&ops, "フォルダです", UserRemedy::RecreateDir(user_tls()));
    let mut ops = FakeUserOps::new();
    ops.cert().real = Some(abs("Users/evil/hub-cert.pem"));
    assert_unusable(&ops, "本当の場所", UserRemedy::RecreateDir(user_tls()));
}

#[test]
fn refuses_a_place_that_is_not_on_a_local_fixed_drive() {
    for (drive, needle) in [
        (DriveKind::Network, "ネットワークの場所"),
        (DriveKind::Removable, "取り外せるドライブ"),
        (DriveKind::Other(5), "ローカルの固定ドライブではない場所"),
    ] {
        let mut ops = FakeUserOps::new();
        ops.drive = Ok(drive);
        assert_unusable(&ops, needle, UserRemedy::FixAnchor(local()));
        // 中のフォルダは開かない。
        assert_eq!(ops.calls(), vec!["pin Local", "drive Local"]);
    }

    let mut ops = FakeUserOps::new();
    ops.drive = Err("読めない".to_string());
    assert_unusable(&ops, "ドライブの種類を読めませんでした", UserRemedy::Retry);

    // 方針で外した置き場は、ドライブの種類を尋ねない。
    let mut ops = FakeUserOps::new();
    ops.drive = Ok(DriveKind::Network);
    let place = Edited(|_, spec: &mut UserPlaceSpec| spec.policy.require_local_fixed_drive = false);
    assert!(matches!(
        load_pinned_user_cert(&ops, &place),
        UserCertPin::Usable(_)
    ));
    assert!(!ops.calls().iter().any(|call| call.starts_with("drive")));
}

#[test]
fn refuses_an_anchor_that_differs_from_the_known_folder() {
    // 置き場の指定の起点が、今の利用者の %LOCALAPPDATA%（既知のフォルダの値）と違う。
    let mut ops = FakeUserOps::new();
    ops.known_local = Ok(abs("Users/other/AppData/Local"));
    assert_unusable(&ops, "既知のフォルダの値", UserRemedy::FixAnchor(local()));
    // 何も開かない。
    assert!(ops.calls().is_empty(), "{:?}", ops.calls());

    let mut ops = FakeUserOps::new();
    ops.known_local = Err("取れない".to_string());
    assert_unusable(&ops, "取れませんでした", UserRemedy::Retry);

    // 方針で外した置き場（別の起点を返す実装）は、既知のフォルダを尋ねない。
    let mut ops = FakeUserOps::new();
    ops.known_local = Err("尋ねないはず".to_string());
    let place = Edited(|_, spec: &mut UserPlaceSpec| spec.policy.anchor_is_local_app_data = false);
    assert!(matches!(
        load_pinned_user_cert(&ops, &place),
        UserCertPin::Usable(_)
    ));
}

#[test]
fn refuses_a_busy_unreadable_or_broken_certificate() {
    let mut ops = FakeUserOps::new();
    ops.open_error
        .insert(KEY_FILE.to_string(), FileOpenError::InUse);
    assert_unusable(&ops, "書き込みに開かれています", UserRemedy::Retry);

    let mut ops = FakeUserOps::new();
    ops.open_error.insert(
        CERT_FILE.to_string(),
        FileOpenError::Other("拒否".to_string()),
    );
    assert_unusable(&ops, "を開けませんでした", UserRemedy::Retry);

    let mut ops = FakeUserOps::new();
    ops.cert().contents = vec![b'a'; MAX_PEM_BYTES + 1];
    assert_unusable(&ops, "を読めませんでした", UserRemedy::Retry);

    for (contents, needle) in [
        (b"not a pem".to_vec(), "ちょうど 1 つではありません（0 個）"),
        (
            format!("{}-----BEGIN PRIVATE KEY-----\n", cert_pem()).into_bytes(),
            "秘密鍵が入っています",
        ),
        (
            b"-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n".to_vec(),
            "DER",
        ),
    ] {
        let mut ops = FakeUserOps::new();
        ops.cert().contents = contents;
        let found = assert_unusable(&ops, needle, UserRemedy::Renew);
        assert!(!found.reason.contains("PRIVATE KEY"), "{found:#?}");
    }
}

// --- 失効の記録の置き場とロックファイル ----------------------------------------------

#[test]
fn opens_the_revocation_places_with_the_same_checks() {
    let ops = FakeUserOps::new();
    let a = present(&ops, UserPlaceKind::RevocationsA);
    assert_eq!(a.dir_path(), rev_a());
    assert_eq!(a.dir_real(), rev_a());
    assert_eq!(a.dir().path, rev_a());
    assert_eq!(a.file_name(), None);
    let b = present(&ops, UserPlaceKind::RevocationsB);
    assert_eq!(b.dir_path(), rev_b());
    assert_eq!(b.owners(), [ME.to_string()]);

    // 無い。
    let ops = FakeUserOps::empty();
    assert_eq!(
        absent(&ops, UserPlaceKind::RevocationsA).missing(),
        ["BantoHub", "revocations"]
    );
    assert_eq!(
        absent(&ops, UserPlaceKind::RevocationsB).missing(),
        ["BantoHubRevocations"]
    );

    // A の親（BantoHub\）だけが緩んでも、B は通る（A は通らない）。
    let mut ops = FakeUserOps::new();
    ops.dir(product())
        .aces
        .push(ace(EVIL, MODIFY_ACCESS_MASK, 0x03));
    refused(
        &ops,
        UserPlaceKind::RevocationsA,
        "失効の記録 A の置き場の親のフォルダ",
        UserRemedy::RecreateDir(product()),
    );
    present(&ops, UserPlaceKind::RevocationsB);

    // B が別のアカウントの物・reparse point。
    let mut ops = FakeUserOps::new();
    ops.dir(rev_b()).owner = EVIL.to_string();
    refused(
        &ops,
        UserPlaceKind::RevocationsB,
        "所有者",
        UserRemedy::RecreateDir(rev_b()),
    );
    let mut ops = FakeUserOps::new();
    ops.dir(rev_b()).is_reparse_point = true;
    refused(
        &ops,
        UserPlaceKind::RevocationsB,
        "reparse point",
        UserRemedy::RecreateDir(rev_b()),
    );

    // 共通の親（%LOCALAPPDATA%）が緩めば、4 つとも通らない。
    let mut ops = FakeUserOps::new();
    ops.dir(local())
        .aces
        .push(ace(EVIL, MODIFY_ACCESS_MASK, 0x03));
    for kind in UserPlaceKind::ALL {
        refused(
            &ops,
            kind,
            "は保護されていません",
            UserRemedy::FixAnchor(local()),
        );
    }
}

#[test]
fn checks_the_lock_file_place_and_the_file_itself() {
    // ロックファイルの置き場は起点そのもの。握るのは起点で、ファイルの有無は見ない。
    let ops = FakeUserOps::empty();
    let place = present(&ops, UserPlaceKind::RevocationsLock);
    assert_eq!(place.dir_path(), local());
    assert_eq!(place.file_name(), Some(USER_REVOCATIONS_LOCK_FILE));
    assert_eq!(ops.calls(), vec!["pin Local", "drive Local"]);
    let lock = |ops: &FakeUserOps| {
        let place = present(ops, UserPlaceKind::RevocationsLock);
        open_user_file(
            ops,
            &place,
            USER_REVOCATIONS_LOCK_FILE,
            "失効の記録のロックファイル",
            UserFileRole::Public,
        )
        .map(|found| found.is_some())
    };
    // 無ければ None（作るのは PR1b）。
    assert_eq!(lock(&ops), Ok(false));

    let ops = FakeUserOps::new();
    assert_eq!(lock(&ops), Ok(true));

    // 別のアカウントの物・書き換えられる・リンク・フォルダは断る。
    let lock_path = local().join(USER_REVOCATIONS_LOCK_FILE);
    let mut ops = FakeUserOps::new();
    ops.file(lock_path.clone()).security.dacl.owner = EVIL.to_string();
    assert_eq!(lock(&ops).unwrap_err().remedy, UserRemedy::Renew);
    let mut ops = FakeUserOps::new();
    if let Some(aces) = ops.file(lock_path.clone()).security.dacl.aces.as_mut() {
        aces.push(ace(EVIL, MODIFY_ACCESS_MASK, 0));
    }
    let found = lock(&ops).unwrap_err();
    assert!(found.reason.contains("は保護されていません"), "{found:#?}");
    let mut ops = FakeUserOps::new();
    ops.file(lock_path.clone()).security.is_reparse_point = true;
    assert_eq!(
        lock(&ops).unwrap_err().remedy,
        UserRemedy::RecreateDir(local())
    );
    let mut ops = FakeUserOps::new();
    ops.file(lock_path).security.is_dir = true;
    assert!(lock(&ops).unwrap_err().reason.contains("フォルダです"));
}

// --- 純関数 -------------------------------------------------------------------------

#[test]
fn user_place_problems_table() {
    let owners = [ME.to_string()];
    let security = |owner: &str, aces: Option<Vec<ObservedAce>>| FileSecurity {
        is_dir: true,
        is_reparse_point: false,
        links: 1,
        dacl: ObservedDacl {
            protected: false,
            owner: owner.to_string(),
            aces,
        },
    };
    let with = |extra: ObservedAce| {
        let mut aces = user_aces(0x13);
        aces.push(extra);
        security(ME, Some(aces))
    };
    let count = |security: &FileSecurity, others_may_read: bool| {
        user_place_problems(security, &owners, others_may_read).len()
    };

    // 既定の形は通る（本人が所有者・本人へのフル）。本人を知らなければ通らない。
    let default = security(ME, Some(user_aces(0x13)));
    assert_eq!(count(&default, true), 0);
    assert_eq!(count(&default, false), 0);
    assert_eq!(user_place_problems(&default, &[], true).len(), 2);

    // 3 者以外への読み取り: フォルダ・証明書は通す、鍵は断る。汎用の読み取りも同じ。
    for mask in [
        READ_EXECUTE_ACCESS_MASK,
        0x8000_0000,
        0xA000_0000,
        0x0010_0000,
    ] {
        assert_eq!(count(&with(ace(EVIL, mask, 0x03)), true), 0, "{mask:#x}");
        assert_eq!(count(&with(ace(EVIL, mask, 0x03)), false), 1, "{mask:#x}");
    }
    // MAXIMUM_ALLOWED・書き込みの bit を 1 つでも含めば、読み取りと一緒でも断る。
    for mask in [0x0200_0000, READ_EXECUTE_ACCESS_MASK | 0x2, 0x0000_0100] {
        assert_eq!(count(&with(ace(EVIL, mask, 0x03)), true), 1, "{mask:#x}");
    }
    // 拒否の ACE は理由にしない。条件付きの許可（種類 9）は読み取りだけでも断る。
    let mut deny = ace(EVIL, FULL_ACCESS_MASK, 0);
    deny.ace_type = 1;
    assert_eq!(count(&with(deny), false), 0);
    let mut callback = ace(EVIL, READ_EXECUTE_ACCESS_MASK, 0);
    callback.ace_type = 9;
    assert_eq!(count(&with(callback), true), 1);
    // null DACL・見知らぬ所有者・reparse point・SID の読めない ACE。
    assert_eq!(count(&security(ME, None), true), 1);
    assert_eq!(count(&security(EVIL, Some(user_aces(0))), true), 1);
    let mut link = default.clone();
    link.is_reparse_point = true;
    assert_eq!(count(&link, true), 1);
    assert_eq!(count(&with(ace("", READ_EXECUTE_ACCESS_MASK, 0)), true), 1);
}

// --- 実 Windows ---------------------------------------------------------------------

/// 実 Windows の `%LOCALAPPDATA%` で、既知のフォルダ・トークンの SID・辿らない pin・ドライブの
/// 種類・所有者と DACL の読み戻しを本物で通す。`%LOCALAPPDATA%` の既定の ACL（利用者・SYSTEM・
/// Administrators だけ）なら、確かめが何もしなくても通ること（§4.1 の末尾）を見る。本物の
/// `BantoHub\` には触らず、`%LOCALAPPDATA%` の下に一時の製品フォルダを作って消す。作る前は
/// 「無い」（起点の確かめに通ったうえで）、鍵と証明書を置いた後は読める。
///
/// **既定でない ACL の PC**（`%LOCALAPPDATA%` に 3 者以外の ACE が足されている。サンドボックスの
/// 道具が AppContainer の SID や専用のグループを足した開発機など）では、既定の置き場は
/// **断られるのが正しい**ので、それを確かめたうえで、足されていた相手を `owners` に加えた
/// 置き場（差し替えの口で方針を変えた形）で残りを通す。
#[cfg(windows)]
#[test]
fn real_local_app_data_place_is_checked_on_windows() {
    use crate::win::WindowsTlsTrustOps;

    let ops = WindowsTlsTrustOps;
    let inner = DefaultUserTlsPlace::resolve(&ops).expect("既定の置き場");
    let local = ops.local_app_data().expect("%LOCALAPPDATA%");
    let me = ops.current_user_sid().expect("今の利用者の SID");
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let scratch = format!("BantoHubTlsTrustTest-{}-{nonce}", std::process::id());

    /// 既定の置き場の `BantoHub` だけを一時の名前に替えたもの。`extra_owners` は既定でない ACL
    /// の PC で足す相手（既定の ACL なら空）。
    struct Scratch {
        inner: DefaultUserTlsPlace,
        name: String,
        extra_owners: Vec<String>,
    }
    impl UserTlsPlace for Scratch {
        fn spec(&self, kind: UserPlaceKind) -> UserPlaceSpec {
            let mut spec = self.inner.spec(kind);
            for dir in &mut spec.dirs {
                if dir == PRODUCT_DIR {
                    *dir = self.name.clone();
                }
            }
            spec.owners.extend(self.extra_owners.iter().cloned());
            spec
        }
    }
    /// 一時の製品フォルダを消す（失敗しても落とさない）。
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    // `%LOCALAPPDATA%` の DACL に、3 者以外の相手がいるか（確かめの本体とは別に、直接読む）。
    let anchor = ops
        .pin_dir(&local)
        .expect("%LOCALAPPDATA% を開く")
        .expect("%LOCALAPPDATA% がある");
    let mut extra_owners: Vec<String> = TlsDir::security(&anchor)
        .expect("%LOCALAPPDATA% の ACL")
        .dacl
        .aces
        .expect("%LOCALAPPDATA% の DACL は null ではない")
        .into_iter()
        .map(|ace| ace.sid)
        .filter(|sid| sid != &me && sid != SYSTEM_SID && sid != ADMINISTRATORS_SID)
        .collect();
    extra_owners.sort();
    extra_owners.dedup();
    drop(anchor);

    let mut place = Scratch {
        inner,
        name: scratch.clone(),
        extra_owners: Vec::new(),
    };
    let _cleanup = Cleanup(local.join(&scratch));

    let tls_dir = local.join(&scratch).join(TLS_DIR);
    let pem = cert_pem();
    let write_pair = || {
        std::fs::create_dir_all(&tls_dir).expect("一時の置き場を作る");
        std::fs::write(tls_dir.join(CERT_FILE), &pem).expect("証明書を書く");
        std::fs::write(tls_dir.join(KEY_FILE), b"not a real key").expect("鍵を書く");
    };

    if !extra_owners.is_empty() {
        // 既定の置き場: 起点（書き込みを持つ相手がいる）か、継承した ACE の付いた鍵（読み取り
        // だけの相手がいる）で断られる。継承しない読み取りだけの ACE なら通る。
        write_pair();
        let strict = load_pinned_user_cert(&ops, &place);
        std::fs::remove_dir_all(local.join(&scratch)).expect("一時の置き場を消す");
        match strict {
            UserCertPin::Usable(_) => {}
            UserCertPin::Unusable(found) => {
                eprintln!(
                    "この PC の %LOCALAPPDATA% の ACL は既定ではありません（3 者以外: \
                     {extra_owners:?}）。既定の置き場は断られました（{}）。足されていた相手を \
                     owners に加えて続けます",
                    found.reason
                );
                place.extra_owners = extra_owners;
            }
            UserCertPin::Absent => panic!("置いたのに「無い」"),
        }
    }

    // 作る前: 起点の確かめに通ったうえで「無い」。
    assert_eq!(load_pinned_user_cert(&ops, &place), UserCertPin::Absent);
    // 起点そのもの（ロックファイルの置き場）も通る。
    assert!(matches!(
        open_user_place(&ops, &place, UserPlaceKind::RevocationsLock),
        Ok(UserPlaceState::Present(_))
    ));

    write_pair();
    // パッケージ（MSIX）の中から走らせると、`%LOCALAPPDATA%` に新しく作ったものはパッケージ
    // ごとの場所へ振り向けられる（設計 §4.4.2 の場面 28）。そのときは本当の場所が期待と違うので
    // **断られるのが正しい**。それを確かめて終える（既定の置き場をそのまま使えない環境）。
    let scratch_path = local.join(&scratch);
    let scratch_real = ops
        .pin_dir(&scratch_path)
        .expect("一時の置き場を開く")
        .expect("一時の置き場がある")
        .final_path();
    let found = load_pinned_user_cert(&ops, &place);
    if !banto_safe_root::guard::paths_match(&scratch_real, &scratch_path) {
        eprintln!(
            "このプロセスの %LOCALAPPDATA% への書き込みは {} へ振り向けられています。既定の \
             置き場が断られることだけを確かめます",
            scratch_real.display()
        );
        match found {
            UserCertPin::Unusable(found) => {
                assert!(found.reason.contains("本当の場所"), "{found:#?}");
                assert_eq!(found.remedy, UserRemedy::RecreateDir(scratch_path));
            }
            other => panic!("振り向けられた置き場は通らないはず: {other:#?}"),
        }
        return;
    }
    match found {
        UserCertPin::Usable(pin) => assert_eq!(pin.cert_pem, pem.as_bytes()),
        other => panic!("通るはず: {other:#?}"),
    }
}
