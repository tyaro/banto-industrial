//! `banto-hub-elev prepare-sink-config`（#630 PR-2、2026-10-17 オーナー決定 その 1・その 2）。
//!
//! banto-hub-sink の設定ファイル（Hub の API キー入り）の置き場
//! `<ProgramData>\BantoHub\sink\` を、SYSTEM・Administrators だけ・所有者 Administrators の
//! 保護された DACL に整え、中の設定ファイル（本物）があれば所有者と DACL を整え直し、
//! `banto-hub-sink.toml.example` を差し替える。sink は置き場とファイルが保護されていなければ
//! 起動しない（`banto_hub_sink::config` の `place`）ので、管理者がメモ帳で設定ファイルを
//! 編集した後（所有者がその管理者になる）にも、もう一度これを実行する。
//!
//! **ここは昇格の境界**（管理者として動き、一般の利用者が作れる場所 - ルート直下 - を扱う）。
//! #626 の `grant-profile-acl`（[`crate::profile_grant`]）と同じく、受け付けるものを固定し、
//! 迷ったら止める側に倒す。
//!
//! ## 設計意図
//!
//! - **引数なし**。ProgramData は既知のフォルダ（`SHGetKnownFolderPath`）から取り、
//!   `BANTO_HUB_ROOT`・このプロセスの環境変数には追従しない（sink と同じ場所）。
//! - **確かめ終わるまで副作用なし**（[`prepare_sink_config`]）: 純粋な確かめ（ProgramData・
//!   既知のフォルダ・`.example` の中身）→ ルートを辿らずに開いて確かめる（本当のパス・
//!   reparse point でないフォルダ・所有者）→ `sink\` を確かめ済みのルートのハンドル基準で開く
//!   （無ければ SYSTEM・Administrators だけの DACL を**作るのと同時に**付けて作る）→ 本当の
//!   パス・所有者（[`banto_secret_store::acl::guard::dir_acl_plan`] を利用者なしで。SYSTEM・
//!   Administrators・昇格した本人以外なら**何も変えずに断る** - 一般の利用者が先に作った
//!   `sink\` の乗っ取りを拒む）→ 中の設定ファイルと `.example` を置き場のハンドル基準で辿らずに
//!   開いて確かめる（リンク・ハードリンク・フォルダ・別のプロセスが書き込みに開いているなら
//!   断る）→ **ここまで通ってから**書く。無いルート・`sink\` を作るのは確かめの途中で行う
//!   （#626 と同じ。作るものは継承のルートと保護された `sink\` だけ）。
//! - **書くこと**（この順）: `sink\` に保護された DACL と所有者 Administrators を書く
//!   （`PinnedDir::reset_acl(.., Recurse::ObjectOnly)`。中の子へは伝えない）→ 設定ファイルが
//!   あれば、そのハンドルに同じ形（ファイル用）を書く（`PinnedFile::reset_acl`。中身は読まない・
//!   変えない）→ `.example` があればそのハンドルで消して（`PinnedFile::delete`）、保護された
//!   DACL で作り直す（`PinnedDir::create_child_file`。インストールのたびに差し替える）。
//! - ルートの DACL は書かない（#626 の決定。`profiles\` を巻き込まない）。ルート直下の旧い
//!   `banto-hub-sink.toml`・`.example`、exe の隣の旧い toml には触らない（旧い toml があれば、
//!   sink が起動しないことと移し方を知らせる行だけ出す）。
//! - 設定ファイルの所有者が SYSTEM・Administrators・昇格した本人以外でも断らない（別の管理者が
//!   メモ帳で編集した場合を断らないため）。中身（`hub_url`）を確かめるよう警告の行を出す。
//!
//! ## 出力・終了コード
//!
//! 行は `banto-hub-elev: ` で始まる。断ったときは理由の行を出して [`Refused`] を返し、呼ぶ側
//! （`crate::service_elevated`）が終了コード 1 にする（#626 と同じく失敗はすべて 1）。
//! `banto_secret_store::SecretStoreError::Exposed` は Debug の表記を出さず、
//! [`exposure_text`] で 1 理由ずつ人の読める文言に言い換える。
//!
//! ## テスト
//!
//! OS を触る操作はすべて [`SinkConfigOps`]（Windows の実装は `win::WindowsSinkConfigOps`）を
//! 通すので、確かめの順序・確かめ終わる前に副作用が無いこと・断る分岐ごとの出力は偽物で全 OS
//! で確かめる（`tests.rs`）。Windows の実物（一時フォルダ）は `win.rs` のテスト（所有者を
//! Administrators に移すので、書くところまで進むものは管理者でしか走らない - CI は管理者）。

use std::path::{Path, PathBuf};

use banto_secret_store::acl::guard::{
    self, DataAclRefusal, DirState, KnownFolders, MachineEnvironment, RealPathRefusal,
};
use banto_secret_store::acl::{exposure, Exposure, FileSecurity, PinnedDirInfo};
use banto_secret_store::InstallUserSid;
use banto_service_control::ConsoleLine;

use crate::profile_grant::{root_parent_problem, well_known_text, CreateHow, Out, Refused, LABEL};

#[cfg(windows)]
pub mod win;

/// ProgramData の下の製品のフォルダ（Hub のルートと同じ名前）。
pub const PRODUCT_DIR: &str = "BantoHub";

/// 置き場のフォルダ名。
pub const SINK_DIR: &str = "sink";

/// 設定ファイル（本物）の名前。
pub const CONFIG_FILE: &str = "banto-hub-sink.toml";

/// 雛形の名前（インストーラが exe の隣に置き、これが置き場へ写す）。
pub const EXAMPLE_FILE: &str = "banto-hub-sink.toml.example";

/// 雛形の大きさの上限。
pub const MAX_EXAMPLE_BYTES: usize = 64 * 1024;

fn out_line(text: &str) -> ConsoleLine {
    ConsoleLine::Stdout(format!("{LABEL}: {text}"))
}

fn err_line(text: &str) -> ConsoleLine {
    ConsoleLine::Stderr(format!("{LABEL}: {text}"))
}

/// [`Exposure`] の文言（純関数。`SecretStoreError::Exposed` の Debug の表記を出さないため）。
/// banto-hub-sink の `config::place::exposure_text` と文言を揃える（片方を直したらもう一方も）。
pub fn exposure_text(found: &Exposure) -> String {
    match found {
        Exposure::ReparsePoint => {
            "ジャンクション・シンボリックリンクなど（reparse point）です".to_string()
        }
        Exposure::HardLinked(links) => {
            format!("ハードリンクです（名前が {links} 個あります）")
        }
        Exposure::ForeignOwner(owner) => {
            format!("所有者（{owner}）が SYSTEM・Administrators ではありません")
        }
        Exposure::NullDacl => "DACL が null です（だれでもフルアクセスできます）".to_string(),
        Exposure::AllowsOther { sid, mask, .. } => format!(
            "SYSTEM・Administrators 以外（{sid}）にアクセスを許しています（アクセス {mask:#x}）"
        ),
        Exposure::UnknownAce { ace_type, .. } => {
            format!("種類の分からない ACE（種類 {ace_type}）があります")
        }
        Exposure::UnreadableSid { ace_type, .. } => {
            format!("SID を読めない ACE（種類 {ace_type}）があります")
        }
    }
}

/// `banto_secret_store` の失敗の文言（`Exposed` は理由を 1 つずつ言い換え、それ以外は
/// `Display`）。
pub fn secret_error_text(error: &banto_secret_store::SecretStoreError) -> String {
    match error {
        banto_secret_store::SecretStoreError::Exposed(found) => {
            let reasons: Vec<String> = found.iter().map(exposure_text).collect();
            format!("書いた後も保護されていません（{}）", reasons.join("・"))
        }
        other => other.to_string(),
    }
}

// --- OS の操作 ---------------------------------------------------------------------

/// 辿らずに開いたフォルダ（Windows は `banto_secret_store::acl::PinnedDir`）。
pub trait SinkDir {
    /// 開いたハンドルから読んだ属性と所有者。
    fn info(&self) -> PinnedDirInfo;
    /// 本当のパス。
    fn final_path(&self) -> PathBuf;
    /// 開いたハンドルから読んだ属性・所有者・DACL（整える前に、ほかの利用者へ開いていたかを
    /// 見るため。#655 のレビュー Should 1）。
    fn security(&self) -> Result<FileSecurity, String>;
    /// このフォルダそのものに、SYSTEM・Administrators だけの保護された DACL と所有者
    /// Administrators をハンドルで書く（中の子へは伝えない - `Recurse::ObjectOnly`）。
    fn reset_service_only(&self) -> Result<(), String>;
}

/// 置き場のハンドル基準に、記述子を書く・消す権利で辿らずに開いたファイル（Windows は
/// `banto_secret_store::acl::PinnedFile`）。持っている間はほかの誰も書き込みに開く・消す・
/// 名前を変えることができない。
pub trait SinkFile {
    /// 開いた時点でハンドルから読んだ属性・リンク数・所有者・DACL。
    fn security(&self) -> FileSecurity;
    /// 本当のパス。
    fn final_path(&self) -> PathBuf;
    /// 保護された DACL（ファイル用）と所有者 Administrators をハンドルで書く。中身は変えない。
    fn reset_acl(&self) -> Result<(), String>;
    /// ハンドルで消す（閉じたときに消える）。
    fn delete(self) -> Result<(), String>;
}

/// [`SinkConfigOps::open_child_file`] の失敗。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOpenError {
    /// 別のプロセスが書き込みに開いている（共有違反）。
    InUse,
    /// それ以外（理由）。
    Other(String),
}

/// `prepare-sink-config` が OS に対してする操作（Windows の実装は `win::WindowsSinkConfigOps`。
/// テストは偽物）。
pub trait SinkConfigOps {
    /// マシンの環境の読み取り（ProgramData・既知のフォルダ。マシンの環境変数は使わない）。
    type Env: MachineEnvironment;
    /// [`SinkConfigOps::pin_dir`] などが返すもの。
    type Dir: SinkDir;
    /// [`SinkConfigOps::open_child_file`] が返すもの。
    type File: SinkFile;

    fn machine_env(&self) -> &Self::Env;
    /// このプロセスのトークンの利用者の SID（所有者として許す）。
    fn current_user_sid(&self) -> Result<InstallUserSid, String>;
    /// 雛形の中身（本番は exe の隣の [`EXAMPLE_FILE`]）。
    fn example_contents(&self) -> Result<Vec<u8>, String>;
    /// この exe のフォルダ（旧い置き場の知らせに使う）。
    fn exe_dir(&self) -> Option<PathBuf>;
    /// パスにファイル・フォルダがあるか（辿らずに見るだけ）。
    fn exists(&self, path: &Path) -> Result<bool, String>;
    /// フォルダを辿らずに開く（無ければ `Ok(None)`）。
    fn pin_dir(&self, dir: &Path) -> Result<Option<Self::Dir>, String>;
    /// 確かめ済みの `parent` のハンドル基準に、名前 1 要素のフォルダを作る（既にあれば
    /// `Ok(None)`）。`ServiceOnly` なら作るのと同時に保護された DACL を付ける。
    fn create_child_dir(
        &self,
        parent: &Self::Dir,
        name: &str,
        how: CreateHow,
    ) -> Result<Option<Self::Dir>, String>;
    /// 確かめ済みの `parent` のハンドル基準に、名前 1 要素の子を辿らずに開く。
    fn open_child_dir(&self, parent: &Self::Dir, name: &str) -> Result<Option<Self::Dir>, String>;
    /// 確かめ済みの `parent` のハンドル基準に、名前 1 要素のファイルを、記述子を書く・消す
    /// 権利で辿らずに開く（共有は読み取りだけ）。
    fn open_child_file(
        &self,
        parent: &Self::Dir,
        name: &str,
    ) -> Result<Option<Self::File>, FileOpenError>;
    /// 確かめ済みの `parent` のハンドル基準に、保護された DACL・所有者 Administrators の
    /// 新しいファイルを作って書く。既にあれば `Ok(false)`（何もしない）。
    fn create_child_file(
        &self,
        parent: &Self::Dir,
        name: &str,
        contents: &[u8],
    ) -> Result<bool, String>;
}

// --- 手順 --------------------------------------------------------------------------

/// 純粋な確かめの結果。
struct Plan {
    root: PathBuf,
    known: KnownFolders,
    elevated_user: Option<InstallUserSid>,
    example: Vec<u8>,
}

/// 純粋な確かめ（ファイルシステムには何もしない）: ProgramData → ルートの形と既知のフォルダ →
/// 昇格した本人 → 雛形の中身。
fn preflight<O: SinkConfigOps>(ops: &O, out: Out<'_>) -> Result<Plan, Refused> {
    let refuse = |out: Out<'_>, text: String| {
        out(err_line(&format!("{text}。何も変えていません")));
        Err(Refused)
    };
    let program_data = match ops.machine_env().program_data() {
        Ok(path) => path,
        Err(error) => {
            return refuse(
                out,
                format!("ProgramData の場所を取れませんでした: {error}"),
            )
        }
    };
    let root = program_data.join(PRODUCT_DIR);
    if !root.is_absolute() {
        return refuse(
            out,
            format!("ルート（{}）が絶対パスではありません", root.display()),
        );
    }
    let known = match ops.machine_env().known_folders() {
        Ok(known) => known,
        Err(error) => {
            return refuse(
                out,
                format!("既知のフォルダの場所を取れませんでした: {error}"),
            )
        }
    };
    if let Err(refusal) = guard::reject_well_known_root(&root, &known) {
        return refuse(out, well_known_text(&root, &refusal));
    }
    let example = match ops.example_contents() {
        Ok(bytes) => bytes,
        Err(error) => {
            return refuse(
                out,
                format!("雛形（{EXAMPLE_FILE}）を読めませんでした: {error}"),
            )
        }
    };
    if example.len() > MAX_EXAMPLE_BYTES {
        return refuse(
            out,
            format!(
                "雛形（{EXAMPLE_FILE}）が大きすぎます（{} バイト。{MAX_EXAMPLE_BYTES} バイトまで）",
                example.len()
            ),
        );
    }
    if std::str::from_utf8(&example).is_err() {
        return refuse(
            out,
            format!("雛形（{EXAMPLE_FILE}）が UTF-8 ではありません"),
        );
    }
    Ok(Plan {
        root,
        known,
        elevated_user: ops.current_user_sid().ok(),
        example,
    })
}

/// 自分で作ったフォルダ（断ったときに残っていることを知らせる）。
#[derive(Default)]
struct Created(Vec<(&'static str, PathBuf)>);

impl Created {
    fn notice(&self, out: Out<'_>) {
        for (label, dir) in &self.0 {
            out(err_line(&format!(
                "{label}（{}）は作りましたが、確かめが通らなかったので残っています",
                dir.display()
            )));
        }
    }
}

/// 作るか、既にあれば親のハンドル基準で開き直す（[`crate::profile_grant`] の
/// `create_or_open` と同じ。パスで開き直さない）。
fn create_or_open<O: SinkConfigOps>(
    ops: &O,
    parent: &O::Dir,
    name: &str,
    how: CreateHow,
) -> Result<(O::Dir, bool), String> {
    match ops.create_child_dir(parent, name, how)? {
        Some(dir) => Ok((dir, true)),
        None => match ops.open_child_dir(parent, name)? {
            Some(dir) => Ok((dir, false)),
            None => Err("作ろうとしたら既にあり、開き直すと無くなっていました".to_string()),
        },
    }
}

/// 断りの文言を出す段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirLevel {
    /// `%ProgramData%\BantoHub`。
    Root,
    /// `%ProgramData%\BantoHub\sink`。
    Sink,
}

/// 所有者・種類の断りの文言（純関数）。
///
/// 所有者で断ったときの案内は段で分ける（2026-10-17 オーナー決定）:
/// - **ルート**（[`DirLevel::Root`]）: `profile_grant` の `grant-profile-acl` と同じく、所有者を
///   Administrators に取り直してから再実行する手順（`takeown /a`・`icacls /setowner`）。ルートには
///   `profiles\` などの本物のデータがあり、消せないため。この操作はルートの DACL を書かない。
/// - **`sink\`**（[`DirLevel::Sink`]）: 取り直さず、中身を確かめて**消してから**再実行する。一般の
///   利用者が先に作った `sink\` は、中身（設定ファイル・リンク・ハードリンク）まで相手が用意した
///   ものでありうる。所有者だけ取り直すと、それを正規の置き場として使い続けることになる
///   （`prepare-sink-config` は `sink\` の中へ再帰して書かない）。消せば、次の実行が保護された
///   DACL で作り直す。
fn dir_refusal_text(level: DirLevel, dir: &Path, refusal: &DataAclRefusal) -> String {
    let reason = match refusal {
        DataAclRefusal::ReparsePoint => "ジャンクションかシンボリックリンク（reparse point）です。リンクの先を書き換えないよう、止めます（リンクを外してから、もう一度実行してください）".to_string(),
        DataAclRefusal::NotADirectory => "フォルダではありません。止めます".to_string(),
        DataAclRefusal::ForeignOwner(owner) => match level {
            DirLevel::Root => format!(
                "所有者（{owner}）が SYSTEM・Administrators・実行した管理者のどれでもありません。乗っ取りの恐れがあるため、止めます（中身を確かめ、このフォルダの所有者を Administrators に取り直してから（takeown /f \"{}\" /a か icacls \"{}\" /setowner *S-1-5-32-544）、もう一度実行してください）",
                dir.display(),
                dir.display()
            ),
            DirLevel::Sink => format!(
                "所有者（{owner}）が SYSTEM・Administrators・実行した管理者のどれでもありません。一般の利用者が先に作った（乗っ取りの準備の）恐れがあるため、何も変えずに止めます（所有者は取り直さず、中身を確かめてから、このフォルダ（{}）を消して、もう一度実行してください。中の設定ファイルは使わず、作り直してください）",
                dir.display()
            ),
        },
    };
    let label = match level {
        DirLevel::Root => "Hub のルート",
        DirLevel::Sink => "設定ファイルの置き場",
    };
    format!("{label}（{}）は{reason}", dir.display())
}

/// 本当のパスの断りの文言。
fn real_path_text(label: &str, expected: &Path, real: &Path, refusal: RealPathRefusal) -> String {
    match refusal {
        RealPathRefusal::Mismatch => format!(
            "{label}（{}）の本当の場所（{}）が期待と違います。途中のフォルダがジャンクションか短い名前の可能性があります。止めます",
            expected.display(),
            real.display()
        ),
        RealPathRefusal::WellKnown(refusal) => format!(
            "{label}（{}）の本当の場所が使えません: {}。止めます",
            real.display(),
            well_known_text(real, &refusal)
        ),
    }
}

/// 置き場の中のファイル 1 つを開いて確かめる（無ければ `None`）。
fn open_checked_file<O: SinkConfigOps>(
    ops: &O,
    sink: &O::Dir,
    sink_real: &Path,
    name: &str,
    label: &str,
    allow_link: bool,
) -> Result<Option<O::File>, String> {
    let file = match ops.open_child_file(sink, name) {
        Ok(Some(file)) => file,
        Ok(None) => return Ok(None),
        Err(FileOpenError::InUse) => {
            return Err(format!(
                "{label}（{name}）は別のプロセス（エディタなど）で書き込みに開かれています。閉じてから、もう一度実行してください"
            ))
        }
        Err(FileOpenError::Other(error)) => {
            return Err(format!("{label}（{name}）を開けませんでした: {error}"))
        }
    };
    let security = file.security();
    let problem = if security.is_dir {
        Some("フォルダです".to_string())
    } else if security.is_reparse_point && !allow_link {
        Some("シンボリックリンクなどの reparse point です".to_string())
    } else if !security.is_reparse_point && security.links >= 2 && !allow_link {
        Some(format!(
            "ハードリンク（名前が {} 個。ほかの名前と記述子を共有）です",
            security.links
        ))
    } else if !guard::paths_match(&file.final_path(), &sink_real.join(name)) {
        Some(format!(
            "本当の場所（{}）が期待と違います",
            file.final_path().display()
        ))
    } else {
        None
    };
    match problem {
        Some(problem) => Err(format!(
            "{label}（{}）は{problem}。止めます",
            sink_real.join(name).display()
        )),
        None => Ok(Some(file)),
    }
}

/// 旧い置き場（ルート直下・exe の隣）の設定ファイルを知らせる行（辿らずに見るだけ。触らない）。
fn legacy_notice<O: SinkConfigOps>(ops: &O, root: &Path, sink_path: &Path) -> Vec<ConsoleLine> {
    let mut legacy = vec![root.join(CONFIG_FILE)];
    if let Some(dir) = ops.exe_dir() {
        legacy.push(dir.join(CONFIG_FILE));
    }
    let mut lines = Vec::new();
    for path in legacy {
        match ops.exists(&path) {
            Ok(true) => lines.push(err_line(&format!(
                "警告: 旧い置き場に設定ファイル {} があります。sink はこれを読まず、残っている間は起動しません。新しい置き場（{}）で .example をコピーして作り直し（旧いファイルを移動しないでください）、Hub でキーを失効・再発行してから、旧いファイルを消してください（このコマンドは旧いファイルに触りません）",
                path.display(),
                sink_path.join(CONFIG_FILE).display()
            ))),
            Ok(false) => {}
            Err(error) => lines.push(err_line(&format!(
                "警告: 旧い置き場 {} に設定ファイルがあるかを確かめられませんでした: {error}",
                path.display()
            ))),
        }
    }
    lines
}

/// `exposure` のうち「ほかの利用者へ開いていた」ことを示す理由（純関数。#655 のレビュー
/// Should 1）: SYSTEM・Administrators 以外への許可・null DACL・種類の分からない ACE・SID を
/// 読めない ACE。所有者・リンクは含めない（所有者は別に扱い、リンクは別に断る）。
pub fn opened_reasons(found: &[Exposure]) -> Vec<Exposure> {
    found
        .iter()
        .filter(|e| {
            matches!(
                e,
                Exposure::AllowsOther { .. }
                    | Exposure::NullDacl
                    | Exposure::UnknownAce { .. }
                    | Exposure::UnreadableSid { .. }
            )
        })
        .cloned()
        .collect()
}

/// 本物の設定ファイルを整え直さずに断るか（純関数。#655 のレビュー Should 1）。断るなら理由の
/// 文言を返す。
///
/// 規則: 置き場が開いていなかった**かつ**設定ファイルの `exposure` が `ForeignOwner` だけ
/// （か空）なら整え直す（別の管理者がメモ帳で編集した場合 - オーナー決定 その 2）。置き場が
/// ほかの利用者へ開いていた（[`opened_reasons`]）、または設定ファイルに `ForeignOwner` 以外の
/// 理由が 1 つでもあれば断る。所有者が「ほかの管理者」かどうかは SID から見分けない（グループの
/// 入れ子・ドメインの利用者で確かでないため）。保護された置き場の中に新しくファイルを作れるのは
/// SYSTEM・Administrators だけで、外から移動したファイルは元の DACL（ほかの利用者への許可）を
/// 持ってくるので、そちらは `AllowsOther` で断る側に入る。
pub fn config_refusal(sink_was_open: &[Exposure], file_exposure: &[Exposure]) -> Option<String> {
    let mut reasons: Vec<String> = sink_was_open
        .iter()
        .map(|found| format!("置き場: {}", exposure_text(found)))
        .collect();
    reasons.extend(
        file_exposure
            .iter()
            .filter(|found| !matches!(found, Exposure::ForeignOwner(_)))
            .map(|found| format!("設定ファイル: {}", exposure_text(found))),
    );
    if reasons.is_empty() {
        return None;
    }
    // 開いていた形跡があるときは、所有者も手がかりとして並べる。
    reasons.extend(
        file_exposure
            .iter()
            .filter(|found| matches!(found, Exposure::ForeignOwner(_)))
            .map(|found| format!("設定ファイル: {}", exposure_text(found))),
    );
    Some(reasons.join("・"))
}

/// `prepare-sink-config` の本体（モジュール doc「設計意図」）。
pub fn prepare_sink_config<O: SinkConfigOps>(ops: &O, out: Out<'_>) -> Result<(), Refused> {
    let plan = preflight(ops, out)?;
    let mut created = Created::default();
    let root_label = "Hub のルート";
    let sink_label = "設定ファイルの置き場";
    let sink_path = plan.root.join(SINK_DIR);

    // 旧い置き場（知らせるだけ。触らない）。辿らずに見るだけなので確かめの最初に調べ、
    // 断ったときにも出す（#655 のレビュー Nit 2）。
    let legacy_lines = legacy_notice(ops, &plan.root, &sink_path);
    let refuse = |out: Out<'_>, created: &Created, text: String| {
        out(err_line(&text));
        created.notice(out);
        for line in &legacy_lines {
            out(line.clone());
        }
        Err(Refused)
    };

    // --- ルート（辿らずに開く。無ければ親を確かめてから、親のハンドル基準で作る） ------
    let mut root_parent: Option<O::Dir> = None;
    let root = match ops.pin_dir(&plan.root) {
        Ok(Some(root)) => root,
        Err(error) => {
            return refuse(
                out,
                &created,
                format!(
                    "{root_label}（{}）を開けませんでした: {error}",
                    plan.root.display()
                ),
            )
        }
        Ok(None) => {
            let parent_dir = plan.root.parent().unwrap_or(&plan.root).to_path_buf();
            let parent = match ops.pin_dir(&parent_dir) {
                Ok(Some(parent)) => parent,
                Ok(None) => {
                    return refuse(
                        out,
                        &created,
                        format!("ProgramData（{}）がありません", parent_dir.display()),
                    )
                }
                Err(error) => {
                    return refuse(
                        out,
                        &created,
                        format!(
                            "ProgramData（{}）を開けませんでした: {error}",
                            parent_dir.display()
                        ),
                    )
                }
            };
            if let Some(problem) = root_parent_problem(
                &parent.final_path(),
                &parent_dir,
                &parent.info(),
                &plan.known,
            ) {
                return refuse(
                    out,
                    &created,
                    format!(
                        "{root_label}（{}）の親（{}）は{problem}。何も作らずに止めます",
                        plan.root.display(),
                        parent_dir.display()
                    ),
                );
            }
            let made = create_or_open(ops, &parent, PRODUCT_DIR, CreateHow::Inherit);
            root_parent = Some(parent);
            match made {
                Ok((root, was_created)) => {
                    if was_created {
                        created.0.push((root_label, plan.root.clone()));
                    }
                    root
                }
                Err(error) => {
                    return refuse(
                        out,
                        &created,
                        format!("{root_label}（{}）: {error}", plan.root.display()),
                    )
                }
            }
        }
    };
    let root_real = root.final_path();
    if let Err(refusal) = guard::check_real_path(&root_real, &plan.root, &plan.known) {
        return refuse(
            out,
            &created,
            real_path_text(root_label, &plan.root, &root_real, refusal),
        );
    }
    let elevated = plan.elevated_user.as_ref().map(InstallUserSid::as_str);
    if let Err(refusal) = guard::dir_acl_plan(&DirState::Present(root.info()), None, elevated) {
        return refuse(
            out,
            &created,
            dir_refusal_text(DirLevel::Root, &plan.root, &refusal),
        );
    }

    // --- 置き場（確かめ済みのルートのハンドル基準。無ければ保護された DACL で作る） -------
    let opened = match ops.open_child_dir(&root, SINK_DIR) {
        Ok(Some(sink)) => Ok((sink, false)),
        Ok(None) => create_or_open(ops, &root, SINK_DIR, CreateHow::ServiceOnly),
        Err(error) => Err(format!("開けませんでした: {error}")),
    };
    let (sink, sink_created) = match opened {
        Ok(found) => found,
        Err(error) => {
            return refuse(
                out,
                &created,
                format!("{sink_label}（{}）: {error}", sink_path.display()),
            )
        }
    };
    if sink_created {
        created.0.push((sink_label, sink_path.clone()));
    }
    let sink_real = sink.final_path();
    if let Err(refusal) = guard::check_real_path(&sink_real, &root_real.join(SINK_DIR), &plan.known)
    {
        return refuse(
            out,
            &created,
            real_path_text(sink_label, &sink_path, &sink_real, refusal),
        );
    }
    if let Err(refusal) = guard::dir_acl_plan(&DirState::Present(sink.info()), None, elevated) {
        return refuse(
            out,
            &created,
            dir_refusal_text(DirLevel::Sink, &sink_path, &refusal),
        );
    }

    // 整える前に、置き場がほかの利用者へ開いていたか（#655 のレビュー Should 1）。今作った
    // 置き場は作るのと同時に保護しているので見ない。
    let sink_was_open = if sink_created {
        Vec::new()
    } else {
        match sink.security() {
            Ok(security) => opened_reasons(&exposure(&security)),
            Err(error) => {
                return refuse(
                    out,
                    &created,
                    format!(
                        "{sink_label}（{}）の ACL を読めませんでした: {error}",
                        sink_path.display()
                    ),
                )
            }
        }
    };

    // --- 中のファイル（置き場のハンドル基準。辿らない） --------------------------------
    let config = match open_checked_file(ops, &sink, &sink_real, CONFIG_FILE, "設定ファイル", false)
    {
        Ok(found) => found,
        Err(text) => return refuse(out, &created, text),
    };
    // 設定ファイルが、ほかの利用者の用意したもの・ほかの利用者に開いていたものでありうるなら
    // 断る（#655 のレビュー Should 1）。所有者だけが違う（別の管理者がメモ帳で編集した）なら
    // 整え直す（オーナー決定 その 2）。
    if let Some(file) = &config {
        if let Some(reasons) = config_refusal(&sink_was_open, &exposure(&file.security())) {
            return refuse(
                out,
                &created,
                format!(
                    "設定ファイル（{}）は、整える前にほかの利用者が用意した・読めた恐れがあります（{reasons}）。何も変えずに止めます。中身を確かめ、設定ファイルを消して、置き場の中の {EXAMPLE_FILE} から作り直してから、もう一度実行してください。キーはほかの利用者に読まれた恐れがあるので、Hub で失効し、新しいキーを発行して書いてください",
                    sink_path.join(CONFIG_FILE).display()
                ),
            );
        }
    }
    // `.example` はリンク・ハードリンクでも、そのものを消して作り直すだけ（先は辿らない）。
    let example = match open_checked_file(ops, &sink, &sink_real, EXAMPLE_FILE, "雛形", true) {
        Ok(found) => found,
        Err(text) => return refuse(out, &created, text),
    };

    // --- ここから書く ------------------------------------------------------------------
    if let Err(error) = sink.reset_service_only() {
        return refuse(
            out,
            &created,
            format!(
                "{sink_label}（{}）の所有者と ACL を書けませんでした: {error}",
                sink_path.display()
            ),
        );
    }
    out(out_line(&format!(
        "{sink_label}（{}）: {}（SYSTEM・Administrators だけにフル、所有者は Administrators、親から継承しない）",
        sink_path.display(),
        if sink_created {
            "作りました"
        } else {
            "所有者と ACL を整えました"
        }
    )));
    if !sink_was_open.is_empty() {
        let reasons: Vec<String> = sink_was_open.iter().map(exposure_text).collect();
        out(err_line(&format!(
            "警告: 整える前は、ほかの利用者が{sink_label}の中に書けました（{}）。中に心当たりの無いファイルが無いか確かめてください",
            reasons.join("・")
        )));
    }

    let config_path = sink_path.join(CONFIG_FILE);
    match &config {
        Some(file) => {
            let owner = file.security().dacl.owner;
            let trusted_owner = owner == banto_secret_store::acl::SYSTEM_SID
                || owner == banto_secret_store::acl::ADMINISTRATORS_SID
                || owner == banto_secret_store::acl::TRUSTED_INSTALLER_SID
                || Some(owner.as_str()) == elevated;
            if let Err(error) = file.reset_acl() {
                return refuse(
                    out,
                    &Created::default(),
                    format!(
                        "設定ファイル（{}）の所有者と ACL を書けませんでした: {error}",
                        config_path.display()
                    ),
                );
            }
            out(out_line(&format!(
                "設定ファイル（{}）: 所有者と ACL を整えました（中身は読んでいません・変えていません）",
                config_path.display()
            )));
            if !trusted_owner {
                out(err_line(&format!(
                    "警告: 設定ファイルの所有者は {owner} でした（SYSTEM・Administrators・実行した管理者ではありません）。別の管理者が編集したのでなければ、中身（特に hub_url）を確かめてください"
                )));
            }
        }
        None => out(out_line(&format!(
            "設定ファイル（{}）はまだありません。管理者のメモ帳で {EXAMPLE_FILE} をコピーして {CONFIG_FILE} を作り、hub_url と api_key を書いてから、もう一度 prepare-sink-config を実行してください",
            config_path.display()
        ))),
    }
    drop(config);

    let example_path = sink_path.join(EXAMPLE_FILE);
    if let Some(file) = example {
        if let Err(error) = file.delete() {
            return refuse(
                out,
                &Created::default(),
                format!(
                    "雛形（{}）を消せませんでした: {error}",
                    example_path.display()
                ),
            );
        }
    }
    match ops.create_child_file(&sink, EXAMPLE_FILE, &plan.example) {
        Ok(true) => out(out_line(&format!(
            "雛形（{}）を置きました（インストールのたびに差し替えます）",
            example_path.display()
        ))),
        Ok(false) => {
            return refuse(
                out,
                &Created::default(),
                format!(
                    "雛形（{}）を作ろうとしたら既にありました（消した直後に誰かが作った・まだ消えていない）。もう一度実行してください",
                    example_path.display()
                ),
            );
        }
        Err(error) => {
            return refuse(
                out,
                &Created::default(),
                format!(
                    "雛形（{}）を作れませんでした: {error}",
                    example_path.display()
                ),
            );
        }
    }
    for line in &legacy_lines {
        out(line.clone());
    }
    // 開いた順の逆に閉じる。
    drop(sink);
    drop(root);
    drop(root_parent);
    Ok(())
}

#[cfg(test)]
mod tests;
