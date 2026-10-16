//! `banto-hub-elev grant-profile-acl`（と`reset-password`・`revert-to-commissioning`の
//! 対象の profile の決め方）の中身（#626 PR-B、2026-10-16。計画のオーナー決定は
//! 論点 1〜10 すべておすすめどおり - docs/banto-hub-t17-design.md §12 の #626 の節）。
//!
//! **ここは昇格の境界**（管理者として動くプロセスが、昇格していない呼び出し元から
//! 引数を受け、その利用者が中身を変えられるフォルダの DACL を書き直す）。そのため、
//! 受け付けるものを固定し、迷ったら止める側に倒す。以前の`profile_acl.rs`（パスの
//! `SetNamedSecurityInfoW`・自動の伝播・呼び出し元の環境変数のルート）は消した。
//!
//! ## 設計意図
//!
//! - **確かめ終わるまで何も作らない・変えない**: 手順は 2 つに分かれる。
//!   [`preflight`]（純粋な確かめ: profile-id → 利用者 → ルート。ファイルシステムに
//!   触らない）と、[`grant_profile_acl`]（稼働中かの確かめの後にだけ、開く・作る・
//!   書き直す）。`service-install`は登録の**前に**[`preflight`]を済ませる
//!   （[`service_install`]。ChronoGazer の B4 と同じ）。
//! - **profile-id は許可リスト**（[`validate_strict_profile_id`]、論点 7）: 英数字・
//!   `_`・`-`の 64 文字まで、先頭は英数字、予約名（`CON`・`NUL`…）を断る。`C:`・
//!   `x:y`は Windows の`Path::join`でドライブ相対のパスになる穴だった
//!   （`crate::profile_paths`のテスト`windows_join_with_a_drive_prefix_discards_the_base`）。
//! - **利用者は SID にして`SidTypeUser`だけ**（論点 5）: 名前か SID を受ける。名前は
//!   `LookupAccountNameW`で SID の文字列にし、`InstallUserSid::parse`（利用者の領域の
//!   SID の形）→ `LookupAccountSidW`の種類（`SidTypeUser`）の順に確かめる。グループ・
//!   well-known・無い名前は断る。省けばこのプロセスの**トークンの利用者の SID**（同じく
//!   確かめる）。
//! - **ルートはマシンの環境だけ**（論点 2）: `banto_secret_store::acl::guard::resolve_machine_root`
//!   に`BANTO_HUB_ROOT`・`BantoHub`を渡す（HKLM の値を展開せずに読み、無ければ
//!   ProgramData の既知フォルダ + `BantoHub`。このプロセスの環境変数は読まない）。ルートと
//!   `profiles\`は**開いて本当のパスと所有者を確かめるだけで、DACL は書かない**（無ければ
//!   親から継承する普通のフォルダとして作る）。
//! - **稼働中は断る**（論点 6）: profile の mutex（`Global\BantoHub.<id>`）があれば何も
//!   しない（書き直しの間、中の子を共有の削除なしで開くので、稼働中の Hub の -wal の
//!   削除・原子的な置換を共有違反にしうる）。
//! - **書き込みは共有の部品だけ**: profile は`banto_secret_store::acl::pin_dir`で辿らずに
//!   開き、本当のパス（`final_path`）を「本当の`profiles` + id」と比べ、
//!   `guard::data_acl_plan`（reparse point・フォルダでない・見知らぬ所有者を断る）を
//!   通ってから、`PinnedDir::reset_acl(.., Recurse::Tree)`でハンドルに書く（自動の伝播を
//!   使わず、子は ID とハンドルで辿り、reparse point・ハードリンク・開けない子は飛ばして
//!   報告する）。無い profile は SYSTEM・Administrators だけの保護された DACL で作り
//!   （`DirAccess::service_only`）、確かめた後に`reset_acl`で利用者の「変更」を付ける
//!   （#622 の Must と同じ形）。
//! - **DACL の形**（論点 1）: 保護された DACL（`D:P`）に SYSTEM・Administrators のフル、
//!   利用者ごとの「変更」（`0x1301BF`）、所有者は Administrators。親からは継承しない。
//! - **足していく意味を保つ**（論点 4 (b)）: 書き直す前の DACL を開いたハンドルから読み
//!   （`PinnedDir::read_dacl`）、`guard::plan_carry_over`で引き継ぐ利用者（明示の許可・
//!   `OICI`・「変更」以上・`SidTypeUser`）を決め、指定の利用者を足して
//!   `DirAccess::with_install_users`で書く。落とした ACE は 1 行ずつ出す（[`access_plan`]）。
//!
//! ## 複数の利用者への拡張点（#637 のオーナーレビュー Should）
//!
//! 共有の`guard::data_acl_plan`は利用者 1 人のシグネチャのまま使う。**所有者として許すのは
//! SYSTEM・Administrators・指定の利用者・この昇格した利用者だけ**で、引き継いだ利用者は
//! 許さない（計画 §3.2 の 7・9、§2.2-6: デスクトップが先に作った profile を別の利用者へ
//! 初めて付けるときは`ForeignOwner`で断るのが正しい。一度書き直せば所有者は
//! Administrators になり、以後は断らない）。DACL に並べる利用者は [`access_plan`] だけが
//! 決める（指定の利用者 + `plan_carry_over`の`carried`。NP・`INHERIT_ONLY`・継承・
//! 「変更」に足りない ACE は引き継がず、フルコントロールは「変更」に**狭めて**引き継ぎ、
//! その旨と、拒否の ACE を落としたことを行で知らせる）。`with_install_users`へ渡す一覧と
//! 報告の行は同じ関数から出るので、配線がずれない。
//!
//! ## 出力・終了コード
//!
//! 行は`banto-hub-elev: `で始まる（`ConsoleLine`、テストは集めて比べる）。断ったときは
//! 理由の行を出して [`Refused`] を返し、呼ぶ側（`crate::service_elevated`）が終了コード 1
//! にする（論点 10: 失敗はすべて 1）。
//!
//! ## テスト
//!
//! OS を触る操作はすべて [`HubElevOps`]（Windows の実装は`win::WindowsHubElevOps`）を
//! 通すので、確かめの順序・副作用が無いこと・出力は偽物で全 OS で確かめる
//! （`tests.rs`）。Windows の実物（一時フォルダのルート）での反証は`win.rs`のテスト
//! （書き直しは所有者を Administrators に移すので、管理者でしか走らないものがある -
//! CI は管理者）。

use std::path::{Path, PathBuf};

use banto_secret_store::acl::guard::{
    self, AccountKind, CarryOver, DataAclRefusal, DirState, DropReason, KnownFolders,
    MachineEnvironment, MachineRootError, RealPathRefusal, RootFormProblem, WellKnownRootRefusal,
};
use banto_secret_store::acl::{
    ObservedAce, ObservedDacl, PinnedDirInfo, ResetSkips, ACCESS_ALLOWED_ACE_TYPE, INHERIT_FLAGS,
    MODIFY_ACCESS_MASK,
};
use banto_secret_store::{DirAccess, InstallUserSid};
use banto_service_control::ConsoleLine;
use thiserror::Error;

use crate::profile_paths::{is_reserved_device_name, resolve_profile_paths, ProfilePaths};

#[cfg(windows)]
pub mod win;

/// 行の頭（`banto-hub-elev: `）。
pub const LABEL: &str = "banto-hub-elev";

/// マシンの環境変数の名前（サービスと同じ`BANTO_HUB_ROOT`）。
pub const ROOT_ENV: &str = "BANTO_HUB_ROOT";

/// ProgramData の下の製品のフォルダ。
pub const PRODUCT_DIR: &str = "BantoHub";

/// ルートの下の profile の置き場。
pub const PROFILES_DIR: &str = "profiles";

/// 昇格ヘルパーが受ける profile-id の長さの上限（文字数）。
pub const MAX_PROFILE_ID_LEN: usize = 64;

/// `INHERITED_ACE`。
const INHERITED_ACE_FLAG: u8 = 0x10;

/// `ACCESS_DENIED_ACE_TYPE`。
const ACCESS_DENIED_ACE_TYPE: u8 = 1;

/// 継承の形を見るビット（`OBJECT_INHERIT`・`CONTAINER_INHERIT`・`NO_PROPAGATE_INHERIT`・
/// `INHERIT_ONLY`）。引き継ぐ ACE はこのうち`OICI`だけを持つ。
const CARRY_SHAPE_MASK: u8 = 0x0F;

// --- profile-id の許可リスト -------------------------------------------------------

/// [`validate_strict_profile_id`] が断る理由。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum StrictProfileIdError {
    #[error("空です")]
    Empty,
    #[error("使えない文字 {0:?} を含みます")]
    InvalidChar(char),
    #[error("{0} 文字あります（{MAX_PROFILE_ID_LEN} 文字まで）")]
    TooLong(usize),
    #[error("先頭が英数字ではありません")]
    FirstNotAlphanumeric,
    #[error("Windows の予約名（CON・NUL・COM1 など）です")]
    Reserved,
}

/// 昇格ヘルパーが受ける profile-id の**許可リスト**（純関数、論点 7）: ASCII の英数字・
/// `_`・`-`だけ、1〜[`MAX_PROFILE_ID_LEN`]文字、先頭は英数字、予約デバイス名でない。
/// `:`・`.`・空白・区切り・ワイルドカードはどれも「使えない文字」になる。既定の
/// `default`と E2E の`p6-<pid>-<時刻>`は通る。
pub fn validate_strict_profile_id(id: &str) -> Result<(), StrictProfileIdError> {
    if id.is_empty() {
        return Err(StrictProfileIdError::Empty);
    }
    if let Some(c) = id
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '_' || *c == '-'))
    {
        return Err(StrictProfileIdError::InvalidChar(c));
    }
    if id.len() > MAX_PROFILE_ID_LEN {
        return Err(StrictProfileIdError::TooLong(id.len()));
    }
    if !id.as_bytes()[0].is_ascii_alphanumeric() {
        return Err(StrictProfileIdError::FirstNotAlphanumeric);
    }
    if is_reserved_device_name(id) {
        return Err(StrictProfileIdError::Reserved);
    }
    Ok(())
}

/// 許可リストの説明（断ったときの行に添える）。
const PROFILE_ID_RULE: &str =
    "英数字・_・- の 64 文字まで、先頭は英数字、CON・NUL などの予約名は使えません";

// --- ルートの文言 ------------------------------------------------------------------

fn well_known_text(root: &Path, refusal: &WellKnownRootRefusal) -> String {
    match refusal {
        WellKnownRootRefusal::NetworkOrSpecial => format!(
            "Hub のルート（{}）はネットワークの場所か特殊なパス（\\\\…）です。ローカルのドライブのフォルダにしてください",
            root.display()
        ),
        WellKnownRootRefusal::DriveRoot => format!(
            "Hub のルート（{}）はドライブのルートです。専用のフォルダ（例: D:\\BantoHub）にしてください",
            root.display()
        ),
        WellKnownRootRefusal::OverlapsProtected(folder) => format!(
            "Hub のルート（{}）は OS・プログラムの置き場か共有のフォルダ（{}）に重なります。専用のフォルダにしてください",
            root.display(),
            folder.display()
        ),
        WellKnownRootRefusal::ContainsProtected(folder) => format!(
            "Hub のルート（{}）は {} そのものかその上のフォルダです。その下の専用のフォルダにしてください",
            root.display(),
            folder.display()
        ),
    }
}

/// [`MachineRootError`] の文言（純関数）。
pub fn machine_root_text(error: &MachineRootError) -> String {
    match error {
        MachineRootError::VariableUnreadable(error) => {
            format!("マシンの環境変数 {ROOT_ENV} を読めませんでした: {error}")
        }
        MachineRootError::NeedsExpansion(value) => format!(
            "マシンの環境変数 {ROOT_ENV}（{value:?}）は展開が要る値です。展開しない絶対パス（例: D:\\BantoHub）にしてください"
        ),
        MachineRootError::ProgramDataUnavailable(error) => {
            format!("ProgramData の場所を取れませんでした: {error}")
        }
        MachineRootError::InvalidForm { root, problem } => {
            let problem = match problem {
                RootFormProblem::Empty => "空です",
                RootFormProblem::NotAbsolute => "絶対パスではありません",
                RootFormProblem::DotSegment => "'.' や '..' の要素を含みます",
            };
            format!(
                "Hub のルート（{}）が使えません: {problem}（マシンの環境変数 {ROOT_ENV} を確かめてください）",
                root.display()
            )
        }
        MachineRootError::KnownFoldersUnavailable(error) => {
            format!("既知のフォルダの場所を取れませんでした: {error}")
        }
        MachineRootError::WellKnown { root, refusal } => well_known_text(root, refusal),
    }
}

// --- reset-password・revert-to-commissioning の対象 ----------------------------------

/// [`resolve_target_profile`] の失敗（`banto-hub-elev`がそのまま行に出す）。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TargetError {
    #[error("banto-hub-elev: profile-id {id:?} は使えません: {reason}（{PROFILE_ID_RULE}）")]
    ProfileId {
        id: String,
        reason: StrictProfileIdError,
    },
    #[error("banto-hub-elev: Hub のルートを決められません: {0}")]
    Root(String),
    /// 許可リストを通った id は`validate_profile_id`も通るので、ここには来ない（型の
    /// 網羅のため）。
    #[error("banto-hub-elev: {0}")]
    Path(crate::profile_paths::ProfileIdError),
}

/// `reset-password`・`revert-to-commissioning`の対象の profile（論点 8）: profile-id は
/// 許可リスト、ルートはマシンの環境だけ（`grant-profile-acl`と同じ。純関数 - 環境は
/// 引数）。省いた profile-id は`default`。
pub fn resolve_target_profile(
    env: &impl MachineEnvironment,
    profile_id: Option<&str>,
) -> Result<ProfilePaths, TargetError> {
    let id = profile_id.unwrap_or(crate::profile_paths::DEFAULT_PROFILE_ID);
    validate_strict_profile_id(id).map_err(|reason| TargetError::ProfileId {
        id: id.to_string(),
        reason,
    })?;
    let root = guard::resolve_machine_root(env, ROOT_ENV, PRODUCT_DIR)
        .map_err(|error| TargetError::Root(machine_root_text(&error)))?;
    resolve_profile_paths(&root, id).map_err(TargetError::Path)
}

/// [`open_target_profile`] が開いて持っているもの（DB を閉じるまで持つ。drop で閉じる）。
pub struct HeldTarget<P, F> {
    /// ルート → `profiles\` → profile → `config\` の順（閉じるのは逆の順）。
    dirs: Vec<P>,
    file: Option<F>,
}

impl<P, F> Drop for HeldTarget<P, F> {
    fn drop(&mut self) {
        self.file.take();
        while self.dirs.pop().is_some() {}
    }
}

/// [`open_target_profile`] が返すもの（対象のパスと、DB を閉じるまで持つもの）。
pub type OpenedTarget<O> = (
    ProfilePaths,
    HeldTarget<<O as HubElevOps>::Pinned, <O as HubElevOps>::HeldFile>,
);

/// [`open_target_profile`] の失敗。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenTargetError {
    /// profile-id・ルートが使えない。
    Target(TargetError),
    /// DB のファイル（か、その上のフォルダ）が無い。
    DbNotFound(PathBuf),
    /// 辿らずに開いた結果、使えない（ジャンクション・本当のパスの不一致・ハードリンクなど）。
    /// 中身は理由の文言。
    Unsafe(String),
}

/// `reset-password`・`revert-to-commissioning`の対象の DB を、**開く前に**辿らずに開いて
/// 確かめ、持ったままにする（#626 PR-B の監査 B-5、2026-10-16 オーナー決定）。
///
/// 1. [`resolve_target_profile`]（許可リストの profile-id・マシンの環境のルート）。
/// 2. ルート → `profiles\` → profile → `config\` を順に`pin_dir`し、それぞれの本当の
///    パスが「1 つ上の本当のパス + 名前」と同じ場所で既知のフォルダに当たらないこと、
///    reparse point でなくフォルダであることを確かめる（profile の中に`config`の
///    ジャンクションを置かれても、その先の DB を開かない）。
/// 3. DB のファイルを辿らずに開き、フォルダでない・reparse point でない・ハードリンク
///    （リンク数 2 以上）でない・本当のパスが「本当の`config` + 名前」であることを確かめる。
///
/// 返したものは DB を閉じるまで持つ（持っている間はどの段も名前の変更・削除ができない）。
/// 作るものは何も無い（読むだけ）。所有者は確かめない（管理者として DB を開くだけで、
/// ACL を書かないため）。
pub fn open_target_profile<O: HubElevOps>(
    ops: &O,
    profile_id: Option<&str>,
) -> Result<OpenedTarget<O>, OpenTargetError> {
    let paths =
        resolve_target_profile(ops.machine_env(), profile_id).map_err(OpenTargetError::Target)?;
    let known = ops.machine_env().known_folders().map_err(|error| {
        OpenTargetError::Target(TargetError::Root(machine_root_text(
            &MachineRootError::KnownFoldersUnavailable(error),
        )))
    })?;
    let config_dir = paths
        .db_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| paths.profile_dir.join("config"));
    let mut held = HeldTarget {
        dirs: Vec::new(),
        file: None,
    };
    let levels: [(PathBuf, Option<&str>); 4] = [
        (paths.root.clone(), None),
        (paths.root.join(PROFILES_DIR), Some(PROFILES_DIR)),
        (paths.profile_dir.clone(), Some(paths.profile_id.as_str())),
        (config_dir.clone(), Some("config")),
    ];
    let mut real_parent: Option<PathBuf> = None;
    for (dir, name) in levels {
        let pinned = match ops.pin_dir(&dir) {
            Ok(Some(pinned)) => pinned,
            Ok(None) => return Err(OpenTargetError::DbNotFound(paths.db_path.clone())),
            Err(error) => {
                return Err(OpenTargetError::Unsafe(format!(
                    "{} を開けませんでした: {error}",
                    dir.display()
                )))
            }
        };
        let expected = match (&real_parent, name) {
            (Some(parent), Some(name)) => parent.join(name),
            _ => dir.clone(),
        };
        let real = pinned.final_path();
        if let Err(refusal) = guard::check_real_path(&real, &expected, &known) {
            return Err(OpenTargetError::Unsafe(match refusal {
                RealPathRefusal::Mismatch => format!(
                    "{} の本当の場所（{}）が期待（{}）と違います。途中のフォルダがジャンクションか短い名前の可能性があります。DB を開かずに止めます",
                    dir.display(),
                    real.display(),
                    expected.display()
                ),
                RealPathRefusal::WellKnown(refusal) => format!(
                    "{} の本当の場所が使えません: {}。DB を開かずに止めます",
                    dir.display(),
                    well_known_text(&real, &refusal)
                ),
            }));
        }
        let info = pinned.info();
        if info.is_reparse_point || !info.is_dir {
            return Err(OpenTargetError::Unsafe(format!(
                "{} はジャンクション・シンボリックリンク（reparse point）か、フォルダではありません。DB を開かずに止めます",
                dir.display()
            )));
        }
        real_parent = Some(real);
        held.dirs.push(pinned);
    }
    let (file, facts) = match ops.pin_file(&paths.db_path) {
        Ok(Some(found)) => found,
        Ok(None) => return Err(OpenTargetError::DbNotFound(paths.db_path.clone())),
        Err(error) => {
            return Err(OpenTargetError::Unsafe(format!(
                "DB（{}）を開けませんでした: {error}",
                paths.db_path.display()
            )))
        }
    };
    let db_name = paths
        .db_path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    let expected = real_parent.unwrap_or(config_dir).join(db_name);
    let problem = if facts.is_dir {
        Some("フォルダです")
    } else if facts.is_reparse_point {
        Some("シンボリックリンクなどの reparse point です")
    } else if facts.links >= 2 {
        Some("ハードリンク（ほかの名前と中身を共有）です")
    } else if !guard::paths_match(&facts.final_path, &expected) {
        Some("本当の場所が期待と違います")
    } else {
        None
    };
    if let Some(problem) = problem {
        return Err(OpenTargetError::Unsafe(format!(
            "DB（{}）は{problem}。DB を開かずに止めます",
            paths.db_path.display()
        )));
    }
    held.file = Some(file);
    Ok((paths, held))
}

// --- OS の操作 ---------------------------------------------------------------------

/// 辿らずに開いたフォルダ（Windows は`banto_secret_store::acl::PinnedDir`）。持っている
/// 間は名前の変更・削除ができない。
pub trait PinnedProfileDir {
    /// 開いたハンドルから読んだ属性と所有者。
    fn info(&self) -> PinnedDirInfo;
    /// `GetFinalPathNameByHandleW`で解決した本当のパス。
    fn final_path(&self) -> PathBuf;
    /// 開いたハンドルから今の DACL と所有者を読む（読むだけ）。
    fn read_dacl(&self) -> Result<ObservedDacl, String>;
    /// 開いたハンドルで保護された DACL と所有者（Administrators）を書き、中の子へ自前で
    /// 伝える（`Recurse::Tree`）。飛ばした子を返す。
    fn reset_acl_tree(&self, access: &DirAccess) -> Result<ResetSkips, String>;
}

/// フォルダの作り方（[`HubElevOps::create_dir`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateHow {
    /// 親から継承する普通のフォルダ（ルート・`profiles\`。無い親も作る）。
    Inherit,
    /// SYSTEM・Administrators だけの保護された DACL（profile。親は既にある）。
    ServiceOnly,
}

/// `banto-hub-elev`が OS に対してする操作（Windows の実装は`win::WindowsHubElevOps`。
/// テストは偽物）。
pub trait HubElevOps {
    /// マシンの環境の読み取り。
    type Env: MachineEnvironment;
    /// [`HubElevOps::pin_dir`] が返すもの。
    type Pinned: PinnedProfileDir;

    /// マシンの環境（HKLM の変数・既知のフォルダ）。
    fn machine_env(&self) -> &Self::Env;
    /// このプロセスのトークンの利用者の SID（利用者の領域の SID でなければ理由）。
    fn current_user_sid(&self) -> Result<InstallUserSid, String>;
    /// アカウントの名前を SID の文字列にする（`LookupAccountNameW`）。無ければ`Ok(None)`。
    fn lookup_account_sid(&self, name: &str) -> Result<Option<String>, String>;
    /// SID がこの PC の利用者のアカウント（`SidTypeUser`）か（`LookupAccountSidW`）。
    fn account_kind(&self, sid: &InstallUserSid) -> AccountKind;
    /// profile の mutex（`Global\BantoHub.<id>`）があるか（読むだけ）。
    fn profile_in_use(&self, profile_id: &str) -> Result<bool, String>;
    /// フォルダを辿らずに開く（無ければ`Ok(None)`）。
    fn pin_dir(&self, dir: &Path) -> Result<Option<Self::Pinned>, String>;
    /// 無ければ作る（作ったら`true`、既にあれば`false`）。
    fn create_dir(&self, dir: &Path, how: CreateHow) -> Result<bool, String>;
    /// [`HubElevOps::pin_file`] が返す、開いたままのファイル（drop で閉じる）。
    type HeldFile;
    /// ファイルを**辿らずに**開いて持つ（Windows は`FILE_FLAG_OPEN_REPARSE_POINT`、共有の
    /// 削除なし - 持っている間は名前の変更・削除・すり替えができない）。無ければ`Ok(None)`。
    /// `reset-password`・`revert-to-commissioning`が DB を開く前に使う（#626 PR-B の監査 B-5）。
    fn pin_file(&self, path: &Path) -> Result<Option<(Self::HeldFile, FileFacts)>, String>;
}

/// [`HubElevOps::pin_file`] で開いたファイルの、ハンドルから読んだ情報。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFacts {
    /// フォルダか。
    pub is_dir: bool,
    /// reparse point か（シンボリックリンクなど）。
    pub is_reparse_point: bool,
    /// ハードリンクの数（2 以上なら木の外の名前と中身を共有している）。
    pub links: u32,
    /// `GetFinalPathNameByHandleW`の本当のパス。
    pub final_path: PathBuf,
}

// --- 行 ----------------------------------------------------------------------------

/// 行の出し先（本番は標準出力・標準エラー出力へすぐ出す。テストは集める）。
pub type Out<'a> = &'a mut dyn FnMut(ConsoleLine);

fn out_line(text: &str) -> ConsoleLine {
    ConsoleLine::Stdout(format!("{LABEL}: {text}"))
}

fn err_line(text: &str) -> ConsoleLine {
    ConsoleLine::Stderr(format!("{LABEL}: {text}"))
}

/// 断った（理由の行は出した）。呼ぶ側が終了コード 1 にする。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refused;

// --- 純粋な確かめ ------------------------------------------------------------------

/// [`preflight`] を通った、書き直しに使う値。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preflight {
    /// 「変更」を付ける利用者（`SidTypeUser`を確かめた）。
    pub user: InstallUserSid,
    /// この昇格したプロセスのトークンの利用者（所有者として許す。取れなければ`None`）。
    pub elevated_user: Option<InstallUserSid>,
    /// マシンの環境から決めたルート。
    pub root: PathBuf,
    /// ルートにしてはいけない既知のフォルダ（本当のパスにもかける）。
    pub known: KnownFolders,
    /// 許可リストを通った profile-id。
    pub profile_id: String,
}

/// 純粋な確かめ（計画 §3.2 の 1〜4。**ファイルシステムには何もしない**）: profile-id の
/// 許可リスト → 利用者（SID・`SidTypeUser`）→ ルート（マシンの環境・既知のフォルダ）。
/// 引数を省けば利用者はトークンの利用者、profile-id は`default`。
pub fn preflight<O: HubElevOps>(
    ops: &O,
    user: Option<&str>,
    profile_id: Option<&str>,
    out: Out<'_>,
) -> Result<Preflight, Refused> {
    let profile_id = profile_id.unwrap_or(crate::profile_paths::DEFAULT_PROFILE_ID);
    if let Err(reason) = validate_strict_profile_id(profile_id) {
        out(err_line(&format!(
            "profile-id {profile_id:?} は使えません: {reason}（{PROFILE_ID_RULE}）。何も変えていません"
        )));
        return Err(Refused);
    }
    let user = resolve_user(ops, user, out)?;
    let root = match guard::resolve_machine_root(ops.machine_env(), ROOT_ENV, PRODUCT_DIR) {
        Ok(root) => root,
        Err(error) => {
            out(err_line(&format!(
                "Hub のルートを決められません: {}。何も変えていません",
                machine_root_text(&error)
            )));
            return Err(Refused);
        }
    };
    let known = match ops.machine_env().known_folders() {
        Ok(known) => known,
        Err(error) => {
            out(err_line(&format!(
                "既知のフォルダの場所を取れませんでした: {error}。何も変えていません"
            )));
            return Err(Refused);
        }
    };
    Ok(Preflight {
        user,
        elevated_user: ops.current_user_sid().ok(),
        root,
        known,
        profile_id: profile_id.to_string(),
    })
}

/// `user`引数が SID の形か（`S-1-`で始まる。`InstallUserSid::parse`と同じく大文字だけ。
/// 小文字の`s-1-`は [`looks_like_lowercase_sid`] で別に断る - #626 PR-B の監査 C-3）。
fn looks_like_sid(arg: &str) -> bool {
    arg.starts_with("S-1-")
}

/// 小文字を含む`s-1-`の形（名前として引かずに、大文字で書くよう案内して断る）。
fn looks_like_lowercase_sid(arg: &str) -> bool {
    !looks_like_sid(arg)
        && arg
            .get(..4)
            .is_some_and(|head| head.eq_ignore_ascii_case("S-1-"))
}

/// 利用者を決めて確かめる（論点 5）: 名前は SID の文字列に → `InstallUserSid::parse` →
/// `SidTypeUser`。SID の形の引数は名前として引かない。省けばトークンの利用者。
fn resolve_user<O: HubElevOps>(
    ops: &O,
    arg: Option<&str>,
    out: Out<'_>,
) -> Result<InstallUserSid, Refused> {
    let refuse = |out: Out<'_>, text: String| {
        out(err_line(&format!("{text}。何も変えていません")));
        Err(Refused)
    };
    let (shown, sid) = match arg {
        Some("") => return refuse(out, "利用者の引数が空です".to_string()),
        Some(arg) if looks_like_lowercase_sid(arg) => {
            return refuse(
                out,
                format!("{arg} は SID の形ですが、SID は S-1- を大文字で書いてください"),
            )
        }
        Some(arg) if looks_like_sid(arg) => match InstallUserSid::parse(arg) {
            Ok(sid) => (arg.to_string(), sid),
            Err(_) => {
                return refuse(
                    out,
                    format!(
                        "{arg} は利用者のアカウントの SID（S-1-5-21-… か S-1-12-1-…）ではありません（グループ・組み込みのアカウントには付けません）"
                    ),
                )
            }
        },
        Some(name) => match ops.lookup_account_sid(name) {
            Ok(Some(sid_text)) => match InstallUserSid::parse(&sid_text) {
                Ok(sid) => (name.to_string(), sid),
                Err(_) => {
                    return refuse(
                        out,
                        format!(
                            "'{name}'（{sid_text}）は利用者のアカウントではありません（グループ・組み込みのアカウントには付けません）"
                        ),
                    )
                }
            },
            Ok(None) => {
                return refuse(
                    out,
                    format!("アカウント '{name}' がこの PC で見つかりません"),
                )
            }
            Err(error) => {
                return refuse(
                    out,
                    format!("アカウント '{name}' を SID にできませんでした: {error}"),
                )
            }
        },
        None => match ops.current_user_sid() {
            Ok(sid) => ("実行した利用者".to_string(), sid),
            Err(error) => {
                return refuse(
                    out,
                    format!(
                        "実行した利用者を利用者のアカウントとして決められませんでした: {error}。利用者の名前か SID を指定してください"
                    ),
                )
            }
        },
    };
    match ops.account_kind(&sid) {
        AccountKind::User => Ok(sid),
        AccountKind::NotAUser => refuse(
            out,
            format!(
                "{shown}（{}）はこの PC の利用者のアカウントではありません（グループ・別名・解決できない SID には付けません）",
                sid.as_str()
            ),
        ),
        AccountKind::LookupFailed(reason) => refuse(
            out,
            format!(
                "{shown}（{}）が利用者のアカウントかを確かめられませんでした: {reason}",
                sid.as_str()
            ),
        ),
    }
}

// --- 書き直す DACL の決め方 ---------------------------------------------------------

/// [`access_plan`] の結果: 書く DACL と、知らせる行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessPlan {
    /// 書く DACL（指定の利用者 + 引き継ぐ利用者に「変更」）。
    pub access: DirAccess,
    /// 引き継ぎ・狭め・落とした ACE の行（DACL に現れた順）。
    pub lines: Vec<ConsoleLine>,
    /// 警告の行の数（拒否を落とした・確かめられずに落とした・null DACL・所有者の継承の
    /// ACE を落とした・フルを狭めた）。最後のまとめの行に使う。
    pub warnings: usize,
}

fn ace_text(ace: &ObservedAce) -> String {
    format!(
        "SID {}・アクセス {:#x}・フラグ {:#04x}",
        if ace.sid.is_empty() { "-" } else { &ace.sid },
        ace.mask,
        ace.flags
    )
}

/// 書き直す DACL を決める（純関数。モジュール doc「複数の利用者への拡張点」）:
///
/// - 利用者 = `user`（指定）+ `carry.carried`（重複はまとめる）。
/// - 引き継いだ利用者ごとに 1 行。今の明示の許可が「変更」より広い（フルコントロール
///   など）利用者には、「変更」に狭める警告の行。
/// - `carry.dropped`は 1 行ずつ（理由ごとの文言）。拒否などの許可でない ACE
///   （[`DropReason::NotAllow`]）と、確かめられずに落とした ACE
///   （[`DropReason::LookupFailed`]）は警告。継承された ACE が所有者（作った利用者）の
///   もので、その利用者が新しい DACL に居なければ、開けなくなる旨の警告を添える。
/// - null DACL は警告の行。
pub fn access_plan(
    user: &InstallUserSid,
    profile_id: &str,
    dacl: &ObservedDacl,
    carry: &CarryOver,
) -> AccessPlan {
    let access = DirAccess::with_install_users(
        std::iter::once(user.clone()).chain(carry.carried.iter().cloned()),
    );
    let mut lines = Vec::new();
    let mut warnings = 0;
    if carry.null_dacl {
        warnings += 1;
        lines.push(err_line(
            "警告: 今の DACL は null（だれでもフルアクセス）でした。引き継ぐ利用者は無く、保護された DACL に書き直します",
        ));
    }
    for sid in &carry.carried {
        if sid != user {
            lines.push(out_line(&format!(
                "今の DACL に「変更」を持つ利用者 {} を引き継ぎます",
                sid.as_str()
            )));
        }
    }
    let in_new_dacl = |sid: &str| access.install_users().iter().any(|u| u.as_str() == sid);
    // 「変更」より広い権限を狭める警告は、**引き継いだ ACE だけ**を見る（#626 PR-B の監査
    // C-1。`plan_carry_over`が引き継ぐのと同じ形: 明示の許可・`OICI`・`INHERIT_ONLY`と
    // `NO_PROPAGATE`なし・「変更」を含む）。
    for sid in &carry.carried {
        let wider = dacl.aces.iter().flatten().any(|ace| {
            ace.ace_type == ACCESS_ALLOWED_ACE_TYPE
                && ace.flags & INHERITED_ACE_FLAG == 0
                && ace.flags & CARRY_SHAPE_MASK == INHERIT_FLAGS
                && ace.sid == sid.as_str()
                && ace.mask & MODIFY_ACCESS_MASK == MODIFY_ACCESS_MASK
                && ace.mask & !MODIFY_ACCESS_MASK != 0
        });
        if wider {
            warnings += 1;
            lines.push(err_line(&format!(
                "警告: 利用者 {} の今の権限は「変更」より広い（フルコントロールなど）ので、「変更」に狭めます（ACL の変更・所有権の取得はできなくなります）",
                sid.as_str()
            )));
        }
    }
    for dropped in &carry.dropped {
        let ace = ace_text(&dropped.ace);
        match &dropped.reason {
            DropReason::NotAllow => {
                warnings += 1;
                let kind = if dropped.ace.ace_type == ACCESS_DENIED_ACE_TYPE {
                    "拒否の ACE".to_string()
                } else {
                    format!("許可でない ACE（種類 {}）", dropped.ace.ace_type)
                };
                lines.push(err_line(&format!(
                    "警告: {kind}（{ace}）を落とします。拒否を落とすと、その相手のアクセスが広がることがあります。要るなら書き直した後に付け直してください"
                )));
            }
            DropReason::Inherited => {
                lines.push(out_line(&format!(
                    "親から継承した ACE（{ace}）は引き継ぎません（保護した DACL には親から流れません）"
                )));
                let owner_left_out = dropped.ace.sid == dacl.owner
                    && InstallUserSid::parse(&dacl.owner).is_ok()
                    && !in_new_dacl(&dacl.owner);
                if owner_left_out {
                    warnings += 1;
                    lines.push(err_line(&format!(
                        "警告: この継承された ACE は profile の所有者（作った利用者 {}）のものです。この利用者は書き直した後にこの profile を開けなくなります。使い続けるなら grant-profile-acl {} {profile_id} を実行してください",
                        dacl.owner, dacl.owner
                    )));
                }
            }
            DropReason::NotTreeWide => lines.push(out_line(&format!(
                "フォルダの中全体に効かない ACE（{ace}）は引き継ぎません"
            ))),
            DropReason::LessThanModify => lines.push(out_line(&format!(
                "「変更」に足りない ACE（{ace}。読み取りだけなど）は引き継ぎません"
            ))),
            DropReason::NotAUserSid => lines.push(out_line(&format!(
                "利用者のアカウントでない SID（グループ・組み込み・サービス）の ACE（{ace}）は引き継ぎません"
            ))),
            DropReason::NotAnAccount => lines.push(out_line(&format!(
                "この PC の利用者のアカウントでない SID（グループなど）の ACE（{ace}）は引き継ぎません"
            ))),
            DropReason::HasDeny if dropped.ace.sid == user.as_str() => {
                warnings += 1;
                lines.push(err_line(&format!(
                    "警告: 指定の利用者 {} には拒否の ACE がありましたが、指定どおり「変更」を付けます（拒否は消えます）",
                    user.as_str()
                )));
            }
            DropReason::HasDeny => {
                warnings += 1;
                lines.push(err_line(&format!(
                    "警告: 利用者 {} には拒否の ACE があったので、許可の ACE（{ace}）を引き継ぎません（引き継ぐと拒否が消えて権利が広がるため）。要るなら grant-profile-acl {} {profile_id} を手で実行してください",
                    dropped.ace.sid, dropped.ace.sid
                )));
            }
            DropReason::LookupFailed(reason) => {
                warnings += 1;
                lines.push(err_line(&format!(
                    "警告: 利用者のアカウントかを確かめられなかったので、ACE（{ace}）を引き継ぎません: {reason}。この相手は書き直した後にこの profile を開けなくなります。名前の解決が戻ってから grant-profile-acl {} {profile_id} を実行してください",
                    dropped.ace.sid
                )));
            }
        }
    }
    AccessPlan {
        access,
        lines,
        warnings,
    }
}

// --- 書き直し ----------------------------------------------------------------------

/// 扱うフォルダの段（行の文言）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    Root,
    Profiles,
    Profile,
}

impl Level {
    fn label(self) -> &'static str {
        match self {
            Self::Root => "Hub のルート",
            Self::Profiles => "profile の置き場（profiles）",
            Self::Profile => "profile",
        }
    }

    fn create_how(self) -> CreateHow {
        match self {
            Self::Root | Self::Profiles => CreateHow::Inherit,
            Self::Profile => CreateHow::ServiceOnly,
        }
    }
}

/// 自分で作ったフォルダ（断ったときに残っていることを知らせる）。
struct Created(Vec<(Level, PathBuf)>);

impl Created {
    /// 断ったとき、自分で作ったものが残っていることを 1 行ずつ知らせる（監査の死角
    /// 「失敗経路での状態」）。
    fn notice(&self, out: Out<'_>) {
        for (level, dir) in &self.0 {
            let shape = match level {
                Level::Root | Level::Profiles => "親から継承した DACL のまま",
                Level::Profile => "SYSTEM・Administrators だけが触れる DACL",
            };
            out(err_line(&format!(
                "{}（{}）は作りましたが、確かめが通らなかったので残っています（{shape}）",
                level.label(),
                dir.display()
            )));
        }
    }
}

fn refusal_text(level: Level, dir: &Path, refusal: &DataAclRefusal) -> String {
    let reason = match refusal {
        // ルート・profiles の段では、利用者を指定しても所有者は変わらない（この段は書き
        // 直さない）ので、所有者の取り直しを案内する（#626 PR-B の監査 B-6）。
        DataAclRefusal::ForeignOwner(owner) if level != Level::Profile => format!(
            "所有者（{owner}）が SYSTEM・Administrators・指定の利用者・実行した管理者のどれでもありません。乗っ取りの恐れがあるため、止めます（中身を確かめ、このフォルダの所有者を Administrators に取り直してから（takeown /f \"{}\" /a か icacls \"{}\" /setowner *S-1-5-32-544）、もう一度実行してください）",
            dir.display(),
            dir.display()
        ),
        DataAclRefusal::ReparsePoint => "ジャンクションかシンボリックリンク（reparse point）です。リンクの先を書き換えないよう、止めます（リンクを外して普通のフォルダにしてから、もう一度実行してください）".to_string(),
        DataAclRefusal::NotADirectory => "フォルダではありません。止めます".to_string(),
        DataAclRefusal::ForeignOwner(owner) => format!(
            "所有者（{owner}）が SYSTEM・Administrators・指定の利用者・実行した管理者のどれでもありません。乗っ取りの恐れがあるため、止めます（デスクトップ版がその利用者で先に作った profile なら、grant-profile-acl <その利用者> を実行してください。心当たりが無ければ、中身と所有者を確かめてください）"
        ),
    };
    format!("{}（{}）は{reason}", level.label(), dir.display())
}

/// 1 つの段を開いて確かめる（無ければ作って開き直す）。本当のパスが`expected`と同じ
/// 場所で既知のフォルダに当たらないこと、`data_acl_plan`（reparse point・フォルダで
/// ない・見知らぬ所有者）を通ること。断るときは作ったものを知らせる。
fn open_level<O: HubElevOps>(
    ops: &O,
    pre: &Preflight,
    level: Level,
    dir: &Path,
    expected: &Path,
    created: &mut Created,
    out: Out<'_>,
) -> Result<O::Pinned, Refused> {
    let pin = |out: Out<'_>| {
        ops.pin_dir(dir).map_err(|error| {
            out(err_line(&format!(
                "{}（{}）を開けませんでした: {error}",
                level.label(),
                dir.display()
            )));
            Refused
        })
    };
    let pinned = match pin(out) {
        Ok(Some(pinned)) => pinned,
        Ok(None) => {
            match ops.create_dir(dir, level.create_how()) {
                Ok(true) => created.0.push((level, dir.to_path_buf())),
                Ok(false) => {}
                Err(error) => {
                    out(err_line(&format!(
                        "{}（{}）を作れませんでした: {error}",
                        level.label(),
                        dir.display()
                    )));
                    created.notice(out);
                    return Err(Refused);
                }
            }
            match pin(out) {
                Ok(Some(pinned)) => pinned,
                Ok(None) => {
                    out(err_line(&format!(
                        "{}（{}）を作った後に開けませんでした（作り直さずに止めます）",
                        level.label(),
                        dir.display()
                    )));
                    created.notice(out);
                    return Err(Refused);
                }
                Err(Refused) => {
                    created.notice(out);
                    return Err(Refused);
                }
            }
        }
        Err(Refused) => {
            created.notice(out);
            return Err(Refused);
        }
    };
    let real = pinned.final_path();
    if let Err(refusal) = guard::check_real_path(&real, expected, &pre.known) {
        let text = match refusal {
            RealPathRefusal::Mismatch => format!(
                "{}（{}）の本当の場所（{}）が期待と違います。途中のフォルダがジャンクションか短い名前の可能性があります。止めます",
                level.label(),
                expected.display(),
                real.display()
            ),
            RealPathRefusal::WellKnown(refusal) => format!(
                "{}（{}）の本当の場所が使えません: {}。止めます",
                level.label(),
                real.display(),
                well_known_text(&real, &refusal)
            ),
        };
        out(err_line(&text));
        created.notice(out);
        return Err(Refused);
    }
    if let Err(refusal) = guard::data_acl_plan(
        &DirState::Present(pinned.info()),
        &pre.user,
        pre.elevated_user.as_ref().map(InstallUserSid::as_str),
    ) {
        out(err_line(&refusal_text(level, dir, &refusal)));
        created.notice(out);
        return Err(Refused);
    }
    Ok(pinned)
}

fn emit_skips(skips: &ResetSkips, out: Out<'_>) -> usize {
    for path in &skips.reparse {
        out(err_line(&format!(
            "警告: profile の中の {} はジャンクションかシンボリックリンクなので、辿らず・触らず飛ばしました",
            path.display()
        )));
    }
    for path in &skips.hardlink {
        out(err_line(&format!(
            "警告: profile の中の {} はハードリンク（ほかの名前と中身を共有）なので、触らず飛ばしました（#624）",
            path.display()
        )));
    }
    for path in &skips.denied {
        out(err_line(&format!(
            "警告: profile の中の {} を開けなかったので、触らず飛ばしました（所有者が Administrators を拒否しているなど。そのまま残ります）",
            path.display()
        )));
    }
    skips.reparse.len() + skips.hardlink.len() + skips.denied.len()
}

/// 書き直し（計画 §3.2 の 5〜12）。[`preflight`] を通った値で、稼働中かを確かめた後に
/// だけ、ルート → `profiles\` → profile の順に開いて（無ければ作って）確かめ、profile の
/// 今の DACL から引き継ぐ利用者を決めて、保護された DACL に書き直す。開いたフォルダは
/// 最後まで持つ（下を扱う間に上の名前を変えさせない）。
pub fn grant_profile_acl<O: HubElevOps>(
    ops: &O,
    pre: &Preflight,
    out: Out<'_>,
) -> Result<(), Refused> {
    match grant_steps(ops, pre, out) {
        Ok(()) => Ok(()),
        Err(GrantStop::InUse) => {
            let id = pre.profile_id.as_str();
            out(err_line(&format!(
                "profile '{id}' は使用中です（Hub のサービスかアプリが開いています - {}）。サービスとアプリを止めてから、もう一度実行してください。何も変えていません",
                crate::profile_paths::mutex_name(id)
            )));
            Err(Refused)
        }
        Err(GrantStop::Refused) => Err(Refused),
    }
}

/// [`grant_steps`] が止まった理由。使用中（mutex がある）だけを分けるのは、
/// `service-install`がそのときだけ ACL の付け直しを飛ばして成功にするため（#626 PR-B の
/// 監査 B-1、2026-10-16 オーナー決定 (a)）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GrantStop {
    /// profile が使用中（行はまだ出していない - 呼ぶ側が出す）。
    InUse,
    /// それ以外で断った（理由の行は出した）。
    Refused,
}

impl From<Refused> for GrantStop {
    fn from(_: Refused) -> Self {
        GrantStop::Refused
    }
}

fn grant_steps<O: HubElevOps>(ops: &O, pre: &Preflight, out: Out<'_>) -> Result<(), GrantStop> {
    let id = pre.profile_id.as_str();
    match ops.profile_in_use(id) {
        Ok(false) => {}
        Ok(true) => return Err(GrantStop::InUse),
        Err(error) => {
            out(err_line(&format!(
                "profile '{id}' が使用中かを確かめられませんでした: {error}。何も変えていません"
            )));
            return Err(GrantStop::Refused);
        }
    }
    let mut created = Created(Vec::new());
    let root = open_level(
        ops,
        pre,
        Level::Root,
        &pre.root,
        &pre.root,
        &mut created,
        out,
    )?;
    let profiles_dir = pre.root.join(PROFILES_DIR);
    let profiles = open_level(
        ops,
        pre,
        Level::Profiles,
        &profiles_dir,
        &root.final_path().join(PROFILES_DIR),
        &mut created,
        out,
    )?;
    let profile_dir = profiles_dir.join(id);
    let profile = open_level(
        ops,
        pre,
        Level::Profile,
        &profile_dir,
        &profiles.final_path().join(id),
        &mut created,
        out,
    )?;
    let dacl = match profile.read_dacl() {
        Ok(dacl) => dacl,
        Err(error) => {
            out(err_line(&format!(
                "profile（{}）の今の DACL を読めませんでした: {error}。書き直さずに止めます",
                profile_dir.display()
            )));
            created.notice(out);
            return Err(GrantStop::Refused);
        }
    };
    let carry = guard::plan_carry_over(&dacl, |sid| ops.account_kind(sid));
    let plan = access_plan(&pre.user, id, &dacl, &carry);
    for line in plan.lines {
        out(line);
    }
    let skips = match profile.reset_acl_tree(&plan.access) {
        Ok(skips) => skips,
        Err(error) => {
            out(err_line(&format!(
                "profile（{}）の ACL を書き直せませんでした: {error}",
                profile_dir.display()
            )));
            created.notice(out);
            return Err(GrantStop::Refused);
        }
    };
    let users: Vec<&str> = plan
        .access
        .install_users()
        .iter()
        .map(InstallUserSid::as_str)
        .collect();
    let what = if created.0.iter().any(|(level, _)| *level == Level::Profile) {
        "作りました"
    } else {
        "ACL を書き直しました"
    };
    out(out_line(&format!(
        "profile '{id}'（{}）: {what}（SYSTEM・Administrators にフル、利用者 {} に変更、所有者は Administrators、親から継承しない。中のフォルダ・ファイルも同じ形）",
        profile_dir.display(),
        users.join("・")
    )));
    let warnings = plan.warnings + emit_skips(&skips, out);
    if warnings > 0 {
        out(err_line(&format!(
            "書き直しは終えましたが、上の警告 {warnings} 件（飛ばした子・落とした ACE など）を確かめてください"
        )));
    }
    // 開いた順の逆に閉じる（profile → profiles → ルート）。
    drop(profile);
    drop(profiles);
    drop(root);
    Ok(())
}

/// `service-install`の手順（計画 §3.2 の末尾）: 登録の**前に** [`preflight`]（既定の
/// 利用者・既定の profile）を済ませ、通らなければ何も登録しない。`register`は登録 →
/// `setup-operators` → `grant-service-acl`（`crate::service_elevated`の今の手順）。
/// 最後に [`grant_profile_acl`] を同じ確かめた値で行う。
///
/// **profile が使用中のときだけ**（mutex がある）、ACL の付け直しを飛ばした警告の行を出して
/// 成功にする（#626 PR-B の監査 B-1、2026-10-16 オーナー決定 (a)）。シェルは × でトレイに
/// 隠れるだけなので、インストーラの`taskkill`（`/F`なし）では終わらず、上書きインストールで
/// mutex が残りうるため。登録・Operators・サービス ACL は済んでいる。それ以外の断り
/// （所有者・本当のパス・reparse point など）は今どおり失敗。手で実行する
/// `grant-profile-acl`は使用中でも断って失敗のまま（[`grant_profile_acl`]）。
pub fn service_install<O: HubElevOps, E>(
    ops: &O,
    register: impl FnOnce() -> Result<(), E>,
    out: Out<'_>,
) -> Result<(), ServiceInstallError<E>> {
    let pre = preflight(ops, None, None, out).map_err(|Refused| {
        out(err_line(
            "利用者・Hub のルート・profile-id の確かめが通らなかったので、サービスを登録していません",
        ));
        ServiceInstallError::Refused
    })?;
    register().map_err(ServiceInstallError::Register)?;
    match grant_steps(ops, &pre, out) {
        Ok(()) => Ok(()),
        Err(GrantStop::InUse) => {
            out(err_line(&format!(
                "警告: profile '{}' が使用中なので ACL の付け直しを飛ばしました（{}）。サービスとアプリを止めてから grant-profile-acl を実行してください",
                pre.profile_id,
                crate::profile_paths::mutex_name(&pre.profile_id)
            )));
            Ok(())
        }
        Err(GrantStop::Refused) => Err(ServiceInstallError::Refused),
    }
}

/// [`service_install`] の失敗。
#[derive(Debug)]
pub enum ServiceInstallError<E> {
    /// 確かめ・書き直しで断った（理由の行は出した）。
    Refused,
    /// 登録の手順の失敗。
    Register(E),
}

#[cfg(test)]
mod tests;
