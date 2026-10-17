//! `prepare-sink-config` を本物の部品で（ProgramData は一時フォルダ、既知のフォルダは本物）。
//!
//! - 一時パスは**本当のパス（長い名前）**で組み立てる（CI の TEMP は`RUNNER~1`の短い名前で、
//!   本当のパスの比較が食い違って断る - #622）。
//! - 書くところまで進むもの（所有者を Administrators に移す）は、昇格した管理者でしか走らない
//!   （[`elevated_admin`] で skip。CI は管理者）。書く前に断る経路は管理者でなくても走る。
//! - 一般の利用者が先に作った`sink\`（所有者で断る）は、所有者を別の利用者にする手段（特権）が
//!   テストに無いので、偽物の表（`../tests.rs`）で固定する。

use std::path::{Path, PathBuf};

use banto_safe_root::acl::{
    check_dacl, exposure, inspect_file, read_dir_dacl, Exposure, ADMINISTRATORS_SID, INHERIT_FLAGS,
};
use banto_safe_root::guard::{KnownFolders, MachineEnvironment};
use banto_safe_root::{DirAccess, InstallUserSid};
use banto_service_control::ConsoleLine;

use super::super::{
    prepare_sink_config, FileOpenError, Refused, SinkConfigOps, CONFIG_FILE, EXAMPLE_FILE,
};
use super::{RegistryMachineEnvironment, WindowsSinkConfigOps};
use crate::profile_grant::CreateHow;

fn elevated_admin(test: &str) -> bool {
    let admin = banto_service_elev::is_current_process_admin().unwrap_or(false);
    if !admin && std::env::var_os("CI").is_some() {
        panic!("{test}: CI なのに昇格した管理者ではない（書き込みの確かめが走らない）");
    }
    if !admin {
        eprintln!("skip {test}: 昇格した管理者ではないので、所有者を Administrators に移せない（CI で確かめる）");
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

/// ProgramData だけ一時フォルダ、既知のフォルダは本物。
struct TempEnv {
    program_data: PathBuf,
}

impl MachineEnvironment for TempEnv {
    fn machine_variable(&self, _name: &str) -> Result<Option<String>, String> {
        panic!("prepare-sink-config はマシンの環境変数を読まない");
    }
    fn program_data(&self) -> Result<PathBuf, String> {
        Ok(self.program_data.clone())
    }
    fn known_folders(&self) -> Result<KnownFolders, String> {
        RegistryMachineEnvironment.known_folders()
    }
}

const EXAMPLE: &[u8] = b"# example\nhub_url = \"http://127.0.0.1:8722\"\napi_key = \"\"\n";

/// フォルダ・ファイルの操作は本物、ProgramData と雛形・exe の場所だけ差し替える。
struct TempOps {
    env: TempEnv,
    exe_dir: PathBuf,
}

impl SinkConfigOps for TempOps {
    type Env = TempEnv;
    type Dir = banto_safe_root::acl::PinnedDir;
    type File = banto_safe_root::acl::PinnedFile;

    fn machine_env(&self) -> &TempEnv {
        &self.env
    }
    fn current_user_sid(&self) -> Result<InstallUserSid, String> {
        WindowsSinkConfigOps.current_user_sid()
    }
    fn example_contents(&self) -> Result<Vec<u8>, String> {
        Ok(EXAMPLE.to_vec())
    }
    fn exe_dir(&self) -> Option<PathBuf> {
        Some(self.exe_dir.clone())
    }
    fn exists(&self, path: &Path) -> Result<bool, String> {
        WindowsSinkConfigOps.exists(path)
    }
    fn pin_dir(&self, dir: &Path) -> Result<Option<Self::Dir>, String> {
        WindowsSinkConfigOps.pin_dir(dir)
    }
    fn create_child_dir(
        &self,
        parent: &Self::Dir,
        name: &str,
        how: CreateHow,
    ) -> Result<Option<Self::Dir>, String> {
        WindowsSinkConfigOps.create_child_dir(parent, name, how)
    }
    fn open_child_dir(&self, parent: &Self::Dir, name: &str) -> Result<Option<Self::Dir>, String> {
        WindowsSinkConfigOps.open_child_dir(parent, name)
    }
    fn open_child_file(
        &self,
        parent: &Self::Dir,
        name: &str,
    ) -> Result<Option<Self::File>, FileOpenError> {
        WindowsSinkConfigOps.open_child_file(parent, name)
    }
    fn create_child_file(
        &self,
        parent: &Self::Dir,
        name: &str,
        contents: &[u8],
    ) -> Result<bool, String> {
        WindowsSinkConfigOps.create_child_file(parent, name, contents)
    }
}

struct Fixture {
    _base: tempfile::TempDir,
    program_data: PathBuf,
    ops: TempOps,
}

fn fixture() -> Fixture {
    let base = tempfile::tempdir().unwrap();
    let real = long(base.path());
    let program_data = real.join("ProgramData");
    let exe_dir = real.join("exe");
    std::fs::create_dir(&program_data).unwrap();
    std::fs::create_dir(&exe_dir).unwrap();
    Fixture {
        _base: base,
        program_data: program_data.clone(),
        ops: TempOps {
            env: TempEnv { program_data },
            exe_dir,
        },
    }
}

fn run(ops: &TempOps) -> (Result<(), Refused>, Vec<String>) {
    let mut lines = Vec::new();
    let result = {
        let mut out = |line: ConsoleLine| lines.push(line);
        prepare_sink_config(ops, &mut out)
    };
    let texts = lines
        .into_iter()
        .map(|line| match line {
            ConsoleLine::Stdout(text) | ConsoleLine::Stderr(text) => text,
        })
        .collect();
    (result, texts)
}

fn check_dir(path: &Path) {
    let dacl = read_dir_dacl(path).unwrap();
    check_dacl(&dacl, &DirAccess::service_only())
        .unwrap_or_else(|e| panic!("{}: {e:?}", path.display()));
    assert_eq!(dacl.owner, ADMINISTRATORS_SID, "{}", path.display());
}

fn check_file(path: &Path) {
    let security = inspect_file(path).unwrap().expect("ある");
    assert_eq!(exposure(&security), vec![], "{}", path.display());
    let mut dacl = security.dacl.clone();
    for ace in dacl.aces.as_mut().unwrap() {
        assert_eq!(ace.flags, 0, "{}", path.display());
        ace.flags = INHERIT_FLAGS;
    }
    check_dacl(&dacl, &DirAccess::service_only()).unwrap();
    assert_eq!(dacl.owner, ADMINISTRATORS_SID);
}

/// 設定ファイルの中身（実際に改行を含むリテラルにしない。checkout の改行の設定で変わらない）。
const BODY: &[u8] = b"hub_url = \"http://127.0.0.1:8722\"\napi_key = \"bh_dummy.key\"\n";

/// `icacls <path> <args>` を実行する。失敗したら終了コードと出力を載せて落とす
/// （管理者の CI でしか走らないので、準備の段で落ちても理由が見えるように）。
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

/// 所有者を SID の利用者にする。`*` は SID を名前と取り違えさせないための印で、ここで付ける。
fn set_owner(path: &Path, sid: &str) {
    icacls(path, &["/setowner", &format!("*{sid}")]);
}

/// SID の利用者に `rights`（`R` など）を許す ACE を足す。`*` はここで付ける。
fn grant(path: &Path, sid: &str, rights: &str) {
    icacls(path, &["/grant", &format!("*{sid}:{rights}")]);
}

/// 新しい PC 相当: ルート（継承）と置き場（保護）を作り、雛形を置く。2 回目は雛形を差し替え、
/// 所有者だけが違う設定ファイル（**実行した管理者が所有者**の形。DACL は置き場から継承した
/// まま）を整え直す（中身は変えない。オーナー決定 その 2）。ルートの DACL は書かない。
/// 別の管理者が所有者の形（hub_url の警告が出る側）は、CI で別の管理者を用意できないので、
/// 偽物の表（`../tests.rs`）で固定している。
#[test]
fn prepares_the_place_and_replaces_the_example() {
    if !elevated_admin("prepares_the_place_and_replaces_the_example") {
        return;
    }
    let f = fixture();
    let root = f.program_data.join("BantoHub");
    let sink = root.join("sink");
    let (result, lines) = run(&f.ops);
    assert_eq!(result, Ok(()), "{lines:#?}");
    check_dir(&sink);
    check_file(&sink.join(EXAMPLE_FILE));
    assert_eq!(std::fs::read(sink.join(EXAMPLE_FILE)).unwrap(), EXAMPLE);
    assert!(!read_dir_dacl(&root).unwrap().protected, "ルートは書かない");

    // 置き場の中で作った設定ファイルの所有者を、テストを走らせている利用者に変える（理由は
    // `ForeignOwner` だけ。DACL は継承のまま SYSTEM・Administrators だけ）。
    let config = sink.join(CONFIG_FILE);
    std::fs::write(&config, BODY).unwrap();
    let me = WindowsSinkConfigOps.current_user_sid().unwrap();
    set_owner(&config, me.as_str());
    let before = inspect_file(&config).unwrap().unwrap();
    assert_eq!(
        exposure(&before),
        vec![Exposure::ForeignOwner(me.as_str().to_string())],
        "所有者だけが違う形から始める"
    );
    std::fs::write(sink.join(EXAMPLE_FILE), b"old").unwrap();

    let (result, lines) = run(&f.ops);
    assert_eq!(result, Ok(()), "{lines:#?}");
    check_dir(&sink);
    check_file(&config);
    check_file(&sink.join(EXAMPLE_FILE));
    assert_eq!(std::fs::read(&config).unwrap(), BODY, "中身は変えない");
    assert_eq!(std::fs::read(sink.join(EXAMPLE_FILE)).unwrap(), EXAMPLE);
    // 所有者は実行した管理者本人（信頼する所有者）なので、hub_url を確かめる警告は出ない。
    assert!(
        lines
            .iter()
            .any(|l| l.contains("所有者と ACL を整えました")),
        "{lines:#?}"
    );
    assert!(
        !lines.iter().any(|l| l.contains("hub_url")),
        "実行した管理者が所有者なら警告しない: {lines:#?}"
    );
}

/// 設定ファイルがほかの利用者（Users）へ読み取りを許していたら、何も変えずに断る（Should 1）。
#[test]
fn a_config_readable_by_others_is_refused() {
    if !elevated_admin("a_config_readable_by_others_is_refused") {
        return;
    }
    let f = fixture();
    let sink = f.program_data.join("BantoHub").join("sink");
    let (result, lines) = run(&f.ops);
    assert_eq!(result, Ok(()), "{lines:#?}");

    let config = sink.join(CONFIG_FILE);
    std::fs::write(&config, BODY).unwrap();
    // 雛形も古いものを置いておく。断った後も書き換えていないこと（確かめ終わるまで書かない）を固定する。
    let example = sink.join(EXAMPLE_FILE);
    std::fs::write(&example, b"old").unwrap();
    grant(&config, "S-1-5-32-545", "R");
    let exposed = inspect_file(&config).unwrap().unwrap();
    assert!(
        !exposure(&exposed).is_empty(),
        "ほかの利用者に開いた形から始める"
    );
    let dacl_before = inspect_file(&config).unwrap().unwrap().dacl;

    let (result, lines) = run(&f.ops);
    assert_eq!(result, Err(Refused), "{lines:#?}");
    assert_eq!(std::fs::read(&config).unwrap(), BODY, "中身は変えない");
    assert_eq!(
        std::fs::read(&example).unwrap(),
        b"old",
        "雛形も書き換えない"
    );
    assert_eq!(
        inspect_file(&config).unwrap().unwrap().dacl,
        dacl_before,
        "DACL も変えない"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("失効") && l.contains("発行")),
        "{lines:#?}"
    );
}

/// 置き場がジャンクションなら、先を書き換えずに断る（管理者でなくても走る）。
#[test]
fn a_junction_place_is_refused() {
    let f = fixture();
    let root = f.program_data.join("BantoHub");
    std::fs::create_dir(&root).unwrap();
    let outside = f.program_data.join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("x.txt"), b"x").unwrap();
    let before = read_dir_dacl(&outside).unwrap();
    junction(&root.join("sink"), &outside);
    let (result, lines) = run(&f.ops);
    assert_eq!(result, Err(Refused), "{lines:#?}");
    assert!(
        lines.iter().any(|l| l.contains("reparse point")),
        "{lines:#?}"
    );
    assert_eq!(read_dir_dacl(&outside).unwrap(), before, "先は変わらない");
    std::fs::remove_dir(root.join("sink")).unwrap();
}

/// 途中の要素（ルート）がジャンクションなら、本当のパスが違うので断る。
#[test]
fn a_junction_root_is_refused() {
    let f = fixture();
    let real = f.program_data.join("real");
    std::fs::create_dir_all(real.join("sink")).unwrap();
    junction(&f.program_data.join("BantoHub"), &real);
    let (result, lines) = run(&f.ops);
    assert_eq!(result, Err(Refused), "{lines:#?}");
    assert!(
        lines
            .iter()
            .any(|l| l.contains("reparse point") || l.contains("本当の場所")),
        "{lines:#?}"
    );
    std::fs::remove_dir(f.program_data.join("BantoHub")).unwrap();
}

/// 設定ファイルがハードリンクなら、外の名前の DACL を変えずに断る（置き場は既にある普通の
/// フォルダ。所有者は本人 = 昇格した本人なので通る）。
#[test]
fn a_hardlinked_config_is_refused() {
    let f = fixture();
    let sink = f.program_data.join("BantoHub").join("sink");
    std::fs::create_dir_all(&sink).unwrap();
    let outside = f.program_data.join("victim.toml");
    std::fs::write(&outside, b"x").unwrap();
    std::fs::hard_link(&outside, sink.join(CONFIG_FILE)).unwrap();
    let before = read_dir_dacl(&outside).unwrap();
    let (result, lines) = run(&f.ops);
    assert_eq!(result, Err(Refused), "{lines:#?}");
    assert!(
        lines.iter().any(|l| l.contains("ハードリンク")),
        "{lines:#?}"
    );
    assert_eq!(read_dir_dacl(&outside).unwrap(), before);
    assert!(read_dir_dacl(&sink).unwrap().aces.is_some());
    assert!(!read_dir_dacl(&sink).unwrap().protected, "置き場も書かない");
}

/// 設定ファイルが書き込みに開かれていれば、書き換えずに断る。
#[test]
fn a_config_open_for_writing_is_refused() {
    let f = fixture();
    let sink = f.program_data.join("BantoHub").join("sink");
    std::fs::create_dir_all(&sink).unwrap();
    std::fs::write(sink.join(CONFIG_FILE), b"x").unwrap();
    let writer = std::fs::OpenOptions::new()
        .write(true)
        .open(sink.join(CONFIG_FILE))
        .unwrap();
    let (result, lines) = run(&f.ops);
    assert_eq!(result, Err(Refused), "{lines:#?}");
    assert!(
        lines.iter().any(|l| l.contains("書き込みに開かれています")),
        "{lines:#?}"
    );
    drop(writer);
    assert!(!read_dir_dacl(&sink).unwrap().protected, "置き場も書かない");
}
