//! `prepare-hub-tls` を本物の部品で（ProgramData は一時フォルダ、既知のフォルダは本物）。
//!
//! - 一時パスは**本当のパス（長い名前）**で組み立てる（CI の TEMP は`RUNNER~1`の短い名前で、
//!   本当のパスの比較が食い違って断る - #622）。
//! - 書くところまで進むもの（所有者を Administrators にして作る）は、昇格した管理者でしか
//!   走らない（[`elevated_admin`] で skip。CI は管理者）。書く前に断る経路は管理者でなくても走る。
//! - 一般の利用者が先に作った`tls\`（所有者で断る）は、所有者を別の利用者にする手段（特権）が
//!   テストに無いので、偽物の表（`../tests.rs`）で固定する。

use std::path::{Path, PathBuf};

use banto_safe_root::acl::{
    exposure, exposure_beyond_users_read, inspect_file, ADMINISTRATORS_SID, BUILTIN_USERS_SID,
    INHERIT_FLAGS, READ_EXECUTE_ACCESS_MASK,
};
use banto_safe_root::guard::{KnownFolders, MachineEnvironment};
use banto_safe_root::InstallUserSid;
use banto_service_control::ConsoleLine;

use super::super::{
    inspect_identity, prepare_hub_tls, DirHow, FileAcl, HubTlsOps, Refused, TlsTrustOps, CERT_FILE,
    CERT_TEMP, KEY_FILE, KEY_TEMP, TLS_DIR,
};
use super::{RegistryMachineEnvironment, WinTlsDir, WindowsHubTlsOps};
use crate::sink_config_prepare::FileOpenError;

fn elevated_admin(test: &str) -> bool {
    let admin = banto_service_elev::is_current_process_admin().unwrap_or(false);
    if !admin && std::env::var_os("CI").is_some() {
        panic!("{test}: CI なのに昇格した管理者ではない（書き込みの確かめが走らない）");
    }
    if !admin {
        eprintln!("skip {test}: 昇格した管理者ではないので、所有者を Administrators にして作れない（CI で確かめる）");
    }
    admin
}

fn long(path: &Path) -> PathBuf {
    let c = std::fs::canonicalize(path).unwrap();
    PathBuf::from(c.to_string_lossy().trim_start_matches(r"\\?\").to_string())
}

/// ProgramData だけ一時フォルダ、既知のフォルダは本物。
struct TempEnv {
    program_data: PathBuf,
}

impl MachineEnvironment for TempEnv {
    fn machine_variable(&self, _name: &str) -> Result<Option<String>, String> {
        panic!("prepare-hub-tls はマシンの環境変数を読まない");
    }
    fn program_data(&self) -> Result<PathBuf, String> {
        Ok(self.program_data.clone())
    }
    fn known_folders(&self) -> Result<KnownFolders, String> {
        RegistryMachineEnvironment.known_folders()
    }
}

/// フォルダ・ファイルの操作は本物、ProgramData だけ差し替える。
struct TempOps {
    env: TempEnv,
}

impl TlsTrustOps for TempOps {
    type Env = TempEnv;
    type Dir = WinTlsDir;
    type File = banto_safe_root::acl::PinnedFile;

    fn machine_env(&self) -> &TempEnv {
        &self.env
    }
    fn pin_dir(&self, dir: &Path) -> Result<Option<WinTlsDir>, String> {
        WindowsHubTlsOps.pin_dir(dir)
    }
    fn open_child_dir(&self, parent: &WinTlsDir, name: &str) -> Result<Option<WinTlsDir>, String> {
        WindowsHubTlsOps.open_child_dir(parent, name)
    }
    fn open_child_file(
        &self,
        parent: &WinTlsDir,
        name: &str,
    ) -> Result<Option<Self::File>, FileOpenError> {
        WindowsHubTlsOps.open_child_file(parent, name)
    }
}

impl HubTlsOps for TempOps {
    /// 本物の名前付き mutex はテスト同士（並列）で取り合うので、ここでは取らない。
    type Lock = ();

    fn hold_lock(&self) -> Result<Option<()>, String> {
        Ok(Some(()))
    }
    fn current_user_sid(&self) -> Result<InstallUserSid, String> {
        WindowsHubTlsOps.current_user_sid()
    }
    fn now_unix(&self) -> i64 {
        WindowsHubTlsOps.now_unix()
    }
    fn create_child_dir(
        &self,
        parent: &WinTlsDir,
        name: &str,
        how: DirHow,
    ) -> Result<Option<WinTlsDir>, String> {
        WindowsHubTlsOps.create_child_dir(parent, name, how)
    }
    fn create_child_file(
        &self,
        parent: &WinTlsDir,
        name: &str,
        contents: &[u8],
        acl: FileAcl,
    ) -> Result<bool, String> {
        WindowsHubTlsOps.create_child_file(parent, name, contents, acl)
    }
    fn remove_child(&self, parent: &WinTlsDir, name: &str) -> Result<bool, String> {
        WindowsHubTlsOps.remove_child(parent, name)
    }
    fn rename_no_replace(&self, parent: &WinTlsDir, from: &str, to: &str) -> Result<bool, String> {
        WindowsHubTlsOps.rename_no_replace(parent, from, to)
    }
}

struct Fixture {
    _base: tempfile::TempDir,
    program_data: PathBuf,
    ops: TempOps,
}

impl Fixture {
    fn tls(&self) -> PathBuf {
        self.program_data.join("BantoHub").join(TLS_DIR)
    }
}

fn fixture() -> Fixture {
    let base = tempfile::tempdir().unwrap();
    let program_data = long(base.path()).join("ProgramData");
    std::fs::create_dir(&program_data).unwrap();
    // 管理者なら、一時の ProgramData を本物の ProgramData の DACL に似せる（TEMP から継承する
    // 実行した利用者のフルを外す。ルートは継承で作るので、残すとルートの DACL の確かめで断る）。
    // 管理者でなければ、外すと後片付けできなくなるので触らない（ルートの所有者が本人になり、
    // どのみちルートで断る）。
    if banto_service_elev::is_current_process_admin().unwrap_or(false) {
        icacls(
            &program_data,
            &[
                "/inheritance:r",
                "/grant:r",
                "*S-1-5-18:(OI)(CI)F",
                "*S-1-5-32-544:(OI)(CI)F",
                "*S-1-3-0:(OI)(CI)(IO)F",
                "*S-1-5-32-545:(OI)(CI)RX",
            ],
        );
        icacls(
            &program_data,
            &["/grant", "*S-1-5-32-545:(CI)(WD,AD,WEA,WA)"],
        );
    }
    Fixture {
        _base: base,
        program_data: program_data.clone(),
        ops: TempOps {
            env: TempEnv { program_data },
        },
    }
}

fn run(ops: &TempOps, renew: bool) -> (Result<(), Refused>, Vec<String>) {
    let mut lines = Vec::new();
    let result = {
        let mut out = |line: ConsoleLine| lines.push(line);
        prepare_hub_tls(ops, renew, &mut out)
    };
    let texts = lines
        .into_iter()
        .map(|line| match line {
            ConsoleLine::Stdout(text) | ConsoleLine::Stderr(text) => text,
        })
        .collect();
    (result, texts)
}

/// `icacls <path> <args>` を実行する。
fn icacls(path: &Path, args: &[&str]) {
    let output = std::process::Command::new("icacls")
        .arg(path)
        .args(args)
        .output()
        .expect("icacls を起動できない");
    assert!(
        output.status.success(),
        "icacls {args:?} に失敗した（終了コード {:?}）\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// 置き場・鍵・証明書が決定 2 の形であることを確かめ、鍵と証明書の中身を返す。
fn check_place(tls: &Path) -> (Vec<u8>, Vec<u8>) {
    let dir = inspect_file(tls).unwrap().expect("tls がある");
    assert!(dir.is_dir && dir.dacl.protected, "{dir:?}");
    assert_eq!(dir.dacl.owner, ADMINISTRATORS_SID);
    assert_eq!(exposure_beyond_users_read(&dir), vec![], "{dir:?}");
    let users: Vec<_> = dir
        .dacl
        .aces
        .as_ref()
        .unwrap()
        .iter()
        .filter(|ace| ace.sid == BUILTIN_USERS_SID)
        .collect();
    assert_eq!(users.len(), 1, "{dir:?}");
    assert_eq!(users[0].mask, READ_EXECUTE_ACCESS_MASK);
    assert_eq!(users[0].flags & INHERIT_FLAGS, INHERIT_FLAGS);

    let key = inspect_file(&tls.join(KEY_FILE))
        .unwrap()
        .expect("鍵がある");
    assert!(key.dacl.protected, "{key:?}");
    assert_eq!(key.dacl.owner, ADMINISTRATORS_SID);
    assert_eq!(
        exposure(&key),
        vec![],
        "鍵は SYSTEM・Administrators だけ: {key:?}"
    );

    let cert = inspect_file(&tls.join(CERT_FILE))
        .unwrap()
        .expect("証明書がある");
    assert!(cert.dacl.protected, "{cert:?}");
    assert_eq!(cert.dacl.owner, ADMINISTRATORS_SID);
    assert_eq!(exposure_beyond_users_read(&cert), vec![], "{cert:?}");
    assert!(
        cert.dacl
            .aces
            .as_ref()
            .unwrap()
            .iter()
            .any(|ace| ace.sid == BUILTIN_USERS_SID && ace.mask == READ_EXECUTE_ACCESS_MASK),
        "{cert:?}"
    );
    for name in [KEY_TEMP, CERT_TEMP] {
        assert!(!tls.join(name).exists(), "一時名 {name} が残っている");
    }
    (
        std::fs::read(tls.join(KEY_FILE)).unwrap(),
        std::fs::read(tls.join(CERT_FILE)).unwrap(),
    )
}

/// 新しい PC 相当: ルート・`tls\`・鍵・証明書を作る → もう一度（再インストール）は保つ →
/// `--renew` は作り直す。作ったものは `tls_listener` の読み込みで読める。
#[test]
fn creates_keeps_and_renews_the_identity() {
    if !elevated_admin("creates_keeps_and_renews_the_identity") {
        return;
    }
    let f = fixture();
    let tls = f.tls();
    let (result, lines) = run(&f.ops, false);
    assert_eq!(result, Ok(()), "{lines:#?}");
    let (key, cert) = check_place(&tls);
    let info = inspect_identity(&cert, &key, f.ops.now_unix()).unwrap();
    assert!(
        lines.iter().any(|l| l.contains(&info.fingerprint)),
        "{lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains(&tls.join(CERT_FILE).display().to_string())),
        "{lines:#?}"
    );
    assert!(
        lines.iter().all(|l| !l.contains("PRIVATE KEY")),
        "{lines:#?}"
    );
    let config =
        crate::tls_listener::TlsConfig::explicit(0, tls.join(CERT_FILE), tls.join(KEY_FILE))
            .unwrap();
    assert!(config.acceptor().is_ok(), "tls_listener が読める");

    let (result, lines) = run(&f.ops, false);
    assert_eq!(result, Ok(()), "{lines:#?}");
    assert!(
        lines.iter().any(|l| l.contains("確かめて保ちました")),
        "{lines:#?}"
    );
    assert_eq!(check_place(&tls), (key.clone(), cert.clone()), "保つ");

    let (result, lines) = run(&f.ops, true);
    assert_eq!(result, Ok(()), "{lines:#?}");
    let (new_key, new_cert) = check_place(&tls);
    assert_ne!(new_key, key);
    assert_ne!(new_cert, cert);
    assert!(inspect_identity(&new_cert, &new_key, f.ops.now_unix()).is_ok());
}

/// 一般の利用者が読める鍵は断り、`--renew` で作り直す（作り直した鍵は保護されている）。前回の
/// 途中で残った一時名も消す。
#[test]
fn refuses_an_exposed_key_and_renew_replaces_it() {
    if !elevated_admin("refuses_an_exposed_key_and_renew_replaces_it") {
        return;
    }
    let f = fixture();
    let tls = f.tls();
    assert_eq!(run(&f.ops, false).0, Ok(()));
    let (key, _cert) = check_place(&tls);
    icacls(
        &tls.join(KEY_FILE),
        &["/grant", &format!("*{BUILTIN_USERS_SID}:R")],
    );
    std::fs::write(tls.join(CERT_TEMP), b"half").unwrap();
    let (result, lines) = run(&f.ops, false);
    assert_eq!(result, Err(Refused), "{lines:#?}");
    assert!(lines.iter().any(|l| l.contains("--renew")), "{lines:#?}");
    assert!(tls.join(CERT_TEMP).exists(), "断ったときは何も変えない");
    assert_eq!(std::fs::read(tls.join(KEY_FILE)).unwrap(), key);

    let (result, lines) = run(&f.ops, true);
    assert_eq!(result, Ok(()), "{lines:#?}");
    let (new_key, _new_cert) = check_place(&tls);
    assert_ne!(new_key, key);
}

/// ジャンクションの `tls\`・利用者が自分で作った `tls\`（所有者がその利用者・継承の ACL）は、
/// 何も書かずに断る（管理者でなくても走る。管理者でなければ、テンポラリの親から継承した本人以外の
/// ACE があるので `tls\` を開く前にルートの DACL の確かめで断る。管理者なら本人（所有者・
/// CREATOR OWNER 由来の ACE）は信頼され、`tls\` で断る）。
#[test]
fn refuses_a_junction_or_a_hand_made_tls_dir() {
    let admin = banto_service_elev::is_current_process_admin().unwrap_or(false);
    let first_reason = if admin {
        "reparse point"
    } else {
        "Hub のルート"
    };
    let f = fixture();
    let root = f.program_data.join("BantoHub");
    std::fs::create_dir(&root).unwrap();
    let elsewhere = f.program_data.join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(root.join(TLS_DIR))
        .arg(&elsewhere)
        .stdout(std::process::Stdio::null())
        .status()
        .expect("cmd を起動できない");
    assert!(status.success(), "mklink /J に失敗した");
    for renew in [false, true] {
        let (result, lines) = run(&f.ops, renew);
        assert_eq!(result, Err(Refused), "{lines:#?}");
        assert!(lines.iter().any(|l| l.contains(first_reason)), "{lines:#?}");
        assert_eq!(
            std::fs::read_dir(&elsewhere).unwrap().count(),
            0,
            "先へ書かない"
        );
    }
    std::fs::remove_dir(root.join(TLS_DIR)).unwrap();

    std::fs::create_dir(root.join(TLS_DIR)).unwrap();
    for renew in [false, true] {
        let (result, lines) = run(&f.ops, renew);
        assert_eq!(result, Err(Refused), "{lines:#?}");
        assert!(
            lines.iter().any(|l| l.contains("保護されていません")),
            "{lines:#?}"
        );
        assert_eq!(
            std::fs::read_dir(root.join(TLS_DIR)).unwrap().count(),
            0,
            "何も書かない"
        );
    }
}

/// 管理者でなければ `tls\`（所有者 Administrators）を作れず、断る（鍵は作らない）。
#[test]
fn a_non_admin_cannot_create_the_place() {
    if banto_service_elev::is_current_process_admin().unwrap_or(false) {
        eprintln!("skip a_non_admin_cannot_create_the_place: 管理者なので作れてしまう");
        return;
    }
    let f = fixture();
    let (result, lines) = run(&f.ops, false);
    assert_eq!(result, Err(Refused), "{lines:#?}");
    assert!(!f.tls().exists(), "{lines:#?}");
}
