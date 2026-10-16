//! 昇格ヘルパーが、ディレクトリの DACL を**書き直す前に**使う、製品に依らない確かめ
//! （#626 PR-A、2026-10-16。chronogazer-elev の中にあった部品を移した - 設計は
//! docs/chronogazer-service-design.md §4 の B3-3b の実装メモ）。
//!
//! ## 設計意図
//!
//! - **判断は純関数**、OS からの読み取りはトレイト（[`MachineEnvironment`]）か呼ぶ側の
//!   引数で受ける。どれも全 OS でテストする。Windows の実装（HKLM の読み取り・既知の
//!   フォルダ）は [`win`] にあり、Windows でだけ公開する。
//! - **文言を持たない**。断る理由は型の付いた値（[`WellKnownRootRefusal`]・
//!   [`MachineRootError`]・[`RealPathRefusal`]・[`DataAclRefusal`]・[`DropReason`]）で返し、
//!   製品の文言（「データのルート」「profile」・接頭辞・例のパス）は製品の elev が組み立てる
//!   （ChronoGazer の `chronogazer-elev`・banto-hub の `banto-hub-elev`）。
//! - **環境変数の名前・製品のフォルダ名は引数**（ChronoGazer は `CHRONOGAZER_ROOT` と
//!   `ChronoGazer`、banto-hub は `BANTO_HUB_ROOT` と `BantoHub`）。
//!
//! ## 部品
//!
//! - [`KnownFolders`]・[`reject_well_known_root`] で、ルートが UNC・ドライブのルート・OS や
//!   プログラムの置き場・ProgramData や Users そのもの（とその祖先）でないことを確かめる。書き直しは
//!   ルートの下の全体に伝わるので、他の製品・利用者のデータの ACL を書き換えないため。
//! - [`resolve_machine_root`] は、ルートを**マシンの環境だけ**から決める
//!   （`HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment` の変数を
//!   **展開せずに**読み、無ければマシンの ProgramData + 製品のフォルダ）。昇格した
//!   プロセスの環境変数・利用者の環境変数は読まない（呼び出し元が用意できるうえ、
//!   LocalSystem のサービスが見るのはマシンの環境だけ）。
//! - [`paths_match`]・[`is_same_or_inside`]・[`check_real_path`] で、開いたハンドルの
//!   **本当のパス**（`PinnedDir::final_path`）が期待のパスと同じ場所か（要素ごと・大文字
//!   小文字を無視）と、本当のパスにも既知のフォルダの deny-list をかける（途中の要素の
//!   ジャンクション・8.3 の短い名前で別の場所を開かされていたら気づく）。
//! - [`check_root_parent`] は、無いルートを親のハンドルを基準に作る前に、辿らずに開いた
//!   親を確かめる（本当のパス・reparse point でないフォルダ・ネットワークでない・OS と
//!   プログラムの置き場の中でない。#642 で banto-hub-elev から移した）。
//! - [`is_plain_child_name`]・[`is_reserved_device_name`] は、親のハンドルを基準に作る・
//!   開く名前 1 要素の確かめ（`NtCreateFile` は Win32 の正規化を通らないため。#642）。
//! - [`data_acl_plan`] は、開いたフォルダの属性と所有者から、作る・書き直す・断るを決める
//!   （reparse point・フォルダでない・見知らぬ所有者を断る）。
//! - [`plan_carry_over`] は、書き直す前の DACL（`PinnedDir::read_dacl`）から、**引き継ぐ
//!   利用者**（明示の許可で「変更」相当、`SidTypeUser`、拒否の ACE を持たない）と
//!   **落とすもの**（グループ・well-known・継承・拒否・拒否を持つ利用者など）を分ける。banto-hub の `grant-profile-acl` が、今の
//!   「足していく」意味を保護された DACL で保つため（#626 の論点 4 (b)）。

use std::path::{Path, PathBuf};

use super::{
    InstallUserSid, ObservedAce, ObservedDacl, PinnedDirInfo, ACCESS_ALLOWED_ACE_TYPE,
    ADMINISTRATORS_SID, INHERITED_ACE_FLAG, INHERIT_FLAGS, MODIFY_ACCESS_MASK, SYSTEM_SID,
};

#[cfg(windows)]
pub mod win;

/// マシンの環境変数のレジストリキー（`HKLM` の下）。
pub const MACHINE_ENVIRONMENT_KEY: &str =
    r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";

// --- 既知のフォルダ ---------------------------------------------------------------

/// ルートにしてはいけない既知のフォルダ（Windows は `SHGetKnownFolderPath` で取る -
/// [`win::RegistryMachineEnvironment`]）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KnownFolders {
    /// そのもの・祖先・**中**も断る（OS・プログラムの置き場、だれでも書ける共有の場所）:
    /// Windows・System・SystemX86・Program Files（x86・Common を含む）・Public。
    pub forbidden_inside: Vec<PathBuf>,
    /// そのもの・祖先だけを断る（中は使ってよい）: ProgramData・UserProfiles（`C:\Users`）。
    pub forbidden_itself: Vec<PathBuf>,
}

/// パスの要素（大文字小文字を無視して比べるため小文字にする。`.` は除く）。
fn lower_components(path: &Path) -> Vec<String> {
    path.components()
        .filter(|c| !matches!(c, std::path::Component::CurDir))
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect()
}

/// `a` と `b` が同じ場所を指すか（要素ごと・大文字小文字を無視して比べる。純関数）。
/// 本当のパス（`PinnedDir::final_path`）と期待のパスを比べるのに使う。
pub fn paths_match(a: &Path, b: &Path) -> bool {
    lower_components(a) == lower_components(b)
}

/// `inner` が `outer` そのものか、その中か（要素の境界で比べる。`Program Files Evil` は
/// `Program Files` の中ではない。大文字小文字は無視。`outer` が空なら `false`）。
pub fn is_same_or_inside(inner: &Path, outer: &Path) -> bool {
    let inner = lower_components(inner);
    let outer = lower_components(outer);
    !outer.is_empty() && inner.len() >= outer.len() && inner[..outer.len()] == outer[..]
}

/// [`reject_well_known_root`] が断る理由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WellKnownRootRefusal {
    /// UNC・`\\?\`・`\\.\`（`\\` か `//` で始まる）。
    NetworkOrSpecial,
    /// ドライブのルート（普通の名前の要素が 1 つも無い）。
    DriveRoot,
    /// [`KnownFolders::forbidden_inside`] のこのフォルダのそのもの・祖先・中。
    OverlapsProtected(PathBuf),
    /// [`KnownFolders::forbidden_itself`] のこのフォルダのそのもの・祖先。
    ContainsProtected(PathBuf),
}

/// ルートが既知のシステムフォルダ・ドライブのルート・ネットワークの場所でないことを
/// 確かめる（純関数）。断るのは [`WellKnownRootRefusal`] の 4 つ（この順に見る）。
pub fn reject_well_known_root(
    root: &Path,
    known: &KnownFolders,
) -> Result<(), WellKnownRootRefusal> {
    let text = root.to_string_lossy();
    if text.starts_with("\\\\") || text.starts_with("//") {
        return Err(WellKnownRootRefusal::NetworkOrSpecial);
    }
    if !root
        .components()
        .any(|c| matches!(c, std::path::Component::Normal(_)))
    {
        return Err(WellKnownRootRefusal::DriveRoot);
    }
    for folder in &known.forbidden_inside {
        if is_same_or_inside(root, folder) || is_same_or_inside(folder, root) {
            return Err(WellKnownRootRefusal::OverlapsProtected(folder.clone()));
        }
    }
    for folder in &known.forbidden_itself {
        if is_same_or_inside(folder, root) {
            return Err(WellKnownRootRefusal::ContainsProtected(folder.clone()));
        }
    }
    Ok(())
}

// --- マシンの環境からのルート ------------------------------------------------------

/// マシンの環境の読み取り（Windows の実装は [`win::RegistryMachineEnvironment`]。テストは
/// 偽物）。
pub trait MachineEnvironment {
    /// [`MACHINE_ENVIRONMENT_KEY`] の `name` の生の値（**展開しない**）。無ければ `Ok(None)`。
    fn machine_variable(&self, name: &str) -> Result<Option<String>, String>;
    /// マシンの ProgramData（`SHGetKnownFolderPath(FOLDERID_ProgramData)`）。
    fn program_data(&self) -> Result<PathBuf, String>;
    /// ルートにしてはいけない既知のフォルダ。取れなければ理由（ルートを決めずに止める）。
    fn known_folders(&self) -> Result<KnownFolders, String>;
}

/// ルートの形の誤り（[`MachineRootError::InvalidForm`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootFormProblem {
    /// 空。
    Empty,
    /// 絶対パスでない（`C:foo`・`\foo` も絶対ではない）。
    NotAbsolute,
    /// `.`・`..`（と点だけの名前）の要素がある（OS が作るときに正規化して別の場所を
    /// 指しうる）。
    DotSegment,
}

/// [`resolve_machine_root`] が決められなかった理由（見る順）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineRootError {
    /// マシンの環境変数を読めなかった（ProgramData へ落とすと、サービスと違う場所を
    /// 直しうるので止める）。
    VariableUnreadable(String),
    /// 値が `%` を含む（展開が要る。展開はこのプロセスの環境に頼るので使わない）。
    NeedsExpansion(String),
    /// 変数が無く、ProgramData の場所も取れなかった。
    ProgramDataUnavailable(String),
    /// 決めたルートの形が使えない。
    InvalidForm {
        root: PathBuf,
        problem: RootFormProblem,
    },
    /// 既知のフォルダの場所を取れなかった（断る一覧が欠けたまま進まない）。
    KnownFoldersUnavailable(String),
    /// 既知のフォルダ・ドライブのルート・UNC に当たる。
    WellKnown {
        root: PathBuf,
        refusal: WellKnownRootRefusal,
    },
}

/// ルートを**マシンの環境だけ**から決める（純関数。モジュール doc）:
///
/// 1. `variable` が空でなければそれ。`%` を含む値は使わない。読めなければ止める。
/// 2. 無ければマシンの ProgramData + `product_dir`。
/// 3. どちらも形（空でない・絶対パス・`.`/`..` なし）と [`reject_well_known_root`] を
///    通ること。
pub fn resolve_machine_root(
    env: &impl MachineEnvironment,
    variable: &str,
    product_dir: &str,
) -> Result<PathBuf, MachineRootError> {
    let root = match env.machine_variable(variable) {
        Err(error) => return Err(MachineRootError::VariableUnreadable(error)),
        Ok(Some(value)) if !value.is_empty() => {
            if value.contains('%') {
                return Err(MachineRootError::NeedsExpansion(value));
            }
            PathBuf::from(value)
        }
        Ok(_) => env
            .program_data()
            .map_err(MachineRootError::ProgramDataUnavailable)?
            .join(product_dir),
    };
    let problem = if root.as_os_str().is_empty() {
        Some(RootFormProblem::Empty)
    } else if !root.is_absolute() {
        Some(RootFormProblem::NotAbsolute)
    } else if super::has_dot_segment(&root) {
        Some(RootFormProblem::DotSegment)
    } else {
        None
    };
    if let Some(problem) = problem {
        return Err(MachineRootError::InvalidForm { root, problem });
    }
    let known = env
        .known_folders()
        .map_err(MachineRootError::KnownFoldersUnavailable)?;
    if let Err(refusal) = reject_well_known_root(&root, &known) {
        return Err(MachineRootError::WellKnown { root, refusal });
    }
    Ok(root)
}

// --- 本当のパス ---------------------------------------------------------------------

/// [`check_real_path`] が断る理由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RealPathRefusal {
    /// 本当のパスが期待のパスと違う場所（途中の要素のジャンクション・8.3 の短い名前）。
    Mismatch,
    /// 本当のパスが既知のフォルダに当たる。
    WellKnown(WellKnownRootRefusal),
}

/// 開いたフォルダの**本当のパス**（`real`、`PinnedDir::final_path`）が期待のパスと同じ
/// 場所を指し（[`paths_match`]）、既知のフォルダに当たらない（[`reject_well_known_root`]）
/// ことを確かめる（純関数）。
pub fn check_real_path(
    real: &Path,
    expected: &Path,
    known: &KnownFolders,
) -> Result<(), RealPathRefusal> {
    if !paths_match(real, expected) {
        return Err(RealPathRefusal::Mismatch);
    }
    reject_well_known_root(real, known).map_err(RealPathRefusal::WellKnown)
}

// --- 無いルートを作る前の、親の確かめ ------------------------------------------------

/// [`check_root_parent`] が断る理由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootParentRefusal {
    /// 本当のパスが期待のパスと違う場所（途中の要素のジャンクション・8.3 の短い名前）。
    Mismatch,
    /// reparse point（ジャンクション・シンボリックリンクなど）か、フォルダではない。
    NotAPlainDirectory,
    /// UNC・`\\?\`・`\\.\`（`\\` か `//` で始まる）。
    NetworkOrSpecial,
    /// [`KnownFolders::forbidden_inside`] のこのフォルダのそのもの・中。
    InsideProtected(PathBuf),
}

/// 無いルートを**親のハンドルを基準に**作る前に、辿らずに開いた親を確かめる（純関数。
/// #626 PR-B の再レビュー 1 で banto-hub-elev に入れたものを、#642 で chronogazer-elev と
/// 共有するために移した）: 本当のパス（`real`）が期待（`expected`）と同じ場所・reparse
/// point でないフォルダ・ネットワークの場所でない・OS とプログラムの置き場
/// （[`KnownFolders::forbidden_inside`]）のそのもの・中でない。ProgramData そのもの
/// （[`KnownFolders::forbidden_itself`]）・ドライブのルートは親として認める（既定の
/// ルートは ProgramData の下、`D:\Product` の親は `D:\`）。この順に見る。
pub fn check_root_parent(
    real: &Path,
    expected: &Path,
    info: &PinnedDirInfo,
    known: &KnownFolders,
) -> Result<(), RootParentRefusal> {
    if !paths_match(real, expected) {
        return Err(RootParentRefusal::Mismatch);
    }
    if info.is_reparse_point || !info.is_dir {
        return Err(RootParentRefusal::NotAPlainDirectory);
    }
    let text = real.to_string_lossy();
    if text.starts_with("\\\\") || text.starts_with("//") {
        return Err(RootParentRefusal::NetworkOrSpecial);
    }
    if let Some(folder) = known
        .forbidden_inside
        .iter()
        .find(|folder| is_same_or_inside(real, folder))
    {
        return Err(RootParentRefusal::InsideProtected(folder.clone()));
    }
    Ok(())
}

// --- 名前 1 要素 --------------------------------------------------------------------

/// Windows の予約デバイス名か（純関数。大文字小文字を無視。最初の `.` より前を、末尾の
/// 空白を除いて見るので `nul.txt`・`CON .log` も予約名。`COM0`〜`COM9`・`LPT0`〜`LPT9`
/// と上付き数字の `COM¹`・`LPT²`・`LPT³` も含む - Win32 はこれらをファイルではなく
/// デバイスとして開く。`COM0`・`LPT0` は #641 のオーナーレビュー Nit 4 で足した: Microsoft の
/// 「Naming Files」が予約名として挙げているため、確かめる側は断る側に倒す）。
///
/// banto-hub の profile-id（`profile_paths::validate_profile_id`）と、
/// [`is_plain_child_name`]（`PinnedDir::create_child_dir`・`open_child_dir` の名前）が使う
/// （#642 でここへ移した）。
pub fn is_reserved_device_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("").trim_end_matches(' ');
    let upper = stem.to_ascii_uppercase();
    const PLAIN: [&str; 6] = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"];
    if PLAIN.contains(&upper.as_str()) {
        return true;
    }
    let Some(rest) = upper
        .strip_prefix("COM")
        .or_else(|| upper.strip_prefix("LPT"))
    else {
        return false;
    };
    let mut digits = rest.chars();
    matches!(
        (digits.next(), digits.next()),
        (Some('0'..='9' | '\u{b9}' | '\u{b2}' | '\u{b3}'), None)
    )
}

/// 名前 1 要素として、親のハンドルを基準に作る・開く（`NtCreateFile` の `RootDirectory`）
/// のに使ってよいか（純関数。#641 のオーナーレビュー Nit 2）。`NtCreateFile` は Win32 の
/// パスの正規化を通らないので、Win32 から見ると別の名前・別の物になるものをここで断る:
///
/// - 空・区切り（`\`・`/`）・`:`（ドライブ・代替データストリーム）・NUL・制御文字。
/// - ワイルドカードと Win32 で使えない文字（`*`・`?`・`<`・`>`・`|`・`"`）。
/// - 末尾の `.` と空白（Win32 は落とすので、`secrets.` で作ったものは Win32 から
///   `secrets` と区別できない。点だけの名前 `.`・`..` もここで断る）。
/// - 予約デバイス名（[`is_reserved_device_name`]。`nul.txt` も）。
pub fn is_plain_child_name(name: &str) -> bool {
    !name.is_empty()
        && !name.ends_with('.')
        && !name.ends_with(' ')
        && !name.chars().any(|c| {
            matches!(
                c,
                '\\' | '/' | ':' | '\0' | '*' | '?' | '<' | '>' | '|' | '"'
            ) || c.is_control()
        })
        && !is_reserved_device_name(name)
}

// --- 書き直す前の所有者の確かめ ----------------------------------------------------

/// 扱うフォルダの今の状態（[`data_acl_plan`] の入力）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirState {
    /// 無い。
    Absent,
    /// ある（辿らずに開いたハンドルから読んだ属性と所有者）。
    Present(PinnedDirInfo),
}

/// フォルダに対してすること。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataAclStep {
    /// 保護された DACL で作る（`create_dir_if_absent`）。
    Create,
    /// 保護された DACL に書き直す（`PinnedDir::reset_acl`）。
    Reset,
}

/// 書き直さずに止める理由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataAclRefusal {
    /// reparse point（ジャンクション・シンボリックリンクなど）。
    ReparsePoint,
    /// フォルダではない。
    NotADirectory,
    /// 所有者が許す範囲の外（乗っ取りの恐れ）。中身は所有者の SID。
    ForeignOwner(String),
}

/// フォルダの状態から、することか止める理由を決める（純関数）。所有者として許すのは
/// SYSTEM・Administrators・`install_user`（対象の利用者）と、`elevated_user`（この昇格
/// したプロセスのトークンの利用者。管理者なので信頼してよく、「作成者を所有者に」の
/// 方針の PC でこのプロセスが作ったフォルダを断らないため）。
pub fn data_acl_plan(
    state: &DirState,
    install_user: &InstallUserSid,
    elevated_user: Option<&str>,
) -> Result<DataAclStep, DataAclRefusal> {
    let info = match state {
        DirState::Absent => return Ok(DataAclStep::Create),
        DirState::Present(info) => info,
    };
    if info.is_reparse_point {
        return Err(DataAclRefusal::ReparsePoint);
    }
    if !info.is_dir {
        return Err(DataAclRefusal::NotADirectory);
    }
    let owner = info.owner.as_str();
    let allowed = owner == SYSTEM_SID
        || owner == ADMINISTRATORS_SID
        || owner == install_user.as_str()
        || elevated_user == Some(owner);
    if !allowed {
        return Err(DataAclRefusal::ForeignOwner(owner.to_string()));
    }
    Ok(DataAclStep::Reset)
}

// --- 引き継ぐ利用者 -----------------------------------------------------------------

/// `INHERIT_ONLY_ACE`（このオブジェクトそのものには効かず、子へ継承するだけの ACE）。
const INHERIT_ONLY_ACE_FLAG: u8 = 0x08;

/// `NO_PROPAGATE_INHERIT_ACE`（継承はすぐ下の子までで、孫以降には伝わらない）。
const NO_PROPAGATE_INHERIT_ACE_FLAG: u8 = 0x04;

/// 拒否の ACE の種類: `ACCESS_DENIED`(1)・`ACCESS_DENIED_OBJECT`(6)・
/// `ACCESS_DENIED_CALLBACK`(10)・`ACCESS_DENIED_CALLBACK_OBJECT`(12)。
pub const DENY_ACE_TYPES: [u8; 4] = [1, 6, 10, 12];

/// 普通の許可でない許可の ACE の種類: `ACCESS_ALLOWED_OBJECT`(5)・
/// `ACCESS_ALLOWED_CALLBACK`(9)・`ACCESS_ALLOWED_CALLBACK_OBJECT`(11)。
pub const CONDITIONAL_ALLOW_ACE_TYPES: [u8; 3] = [5, 9, 11];

/// DACL に現れうる種類のうち、SID を読んで扱うもの（それ以外は [`CarryOver::blocked`]）。
pub fn is_known_dacl_ace_type(ace_type: u8) -> bool {
    ace_type == ACCESS_ALLOWED_ACE_TYPE
        || DENY_ACE_TYPES.contains(&ace_type)
        || CONDITIONAL_ALLOW_ACE_TYPES.contains(&ace_type)
}

/// [`plan_carry_over`] が SID の種類を問い合わせた結果（Windows では
/// `banto_secret_store::acl::check_user_account` から作る）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountKind {
    /// この PC で解決できる利用者のアカウント（`SidTypeUser`）。
    User,
    /// 利用者のアカウントではない（グループ・別名・解決できない SID）。
    NotAUser,
    /// 確かめられなかった（RPC 不達など）。中身は理由。
    LookupFailed(String),
}

/// `sid` の種類をこの PC で問い合わせる（[`super::check_user_account`]、
/// `LookupAccountSidW`）。[`plan_carry_over`] の `account` にそのまま渡せる。
/// `ERROR_NONE_MAPPED`・グループなど（[`crate::SecretStoreError::InvalidUserSid`]）は
/// [`AccountKind::NotAUser`]、それ以外の失敗（RPC 不達など。Windows 以外の
/// `UnsupportedPlatform` も）は [`AccountKind::LookupFailed`]。
pub fn account_kind(sid: &InstallUserSid) -> AccountKind {
    match super::check_user_account(sid) {
        Ok(()) => AccountKind::User,
        Err(crate::SecretStoreError::InvalidUserSid(_)) => AccountKind::NotAUser,
        Err(error) => AccountKind::LookupFailed(error.to_string()),
    }
}

/// 引き継がずに落とす ACE の理由（[`plan_carry_over`] が見る順）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DropReason {
    /// 許可の ACE ではない（拒否・監査・オブジェクトの ACE など）。**拒否を落とすと
    /// その相手のアクセスは広がりうる**ので、呼ぶ側は必ず知らせる。
    NotAllow,
    /// 親から継承した ACE（`INHERITED_ACE`）。保護された DACL では流れなくなる。
    Inherited,
    /// フォルダの中全体に効く形（`OICI`、`INHERIT_ONLY` なし）ではない。no-propagate
    /// （`NO_PROPAGATE_INHERIT_ACE`。すぐ下の子までしか伝わらない）も含む。
    NotTreeWide,
    /// 「変更」（`MODIFY_ACCESS_MASK`）に足りない（読み取りだけなど。汎用の権利
    /// `GENERIC_*` だけの ACE もここ）。
    LessThanModify,
    /// 利用者のアカウントの領域の SID ではない（well-known・BUILTIN・サービスの SID など。
    /// `InstallUserSid::parse` に通らない）。
    NotAUserSid,
    /// 利用者の領域の SID だが、利用者のアカウントではない（グループなど）。
    NotAnAccount,
    /// 条件付き（callback）・オブジェクトの**許可**の ACE（種類 5・9・11）。条件・対象の
    /// 型を外して普通の許可として引き継ぐと権利が広がるので引き継がない（#626 PR-B の
    /// 再レビュー 2）。呼ぶ側は知らせる。
    ConditionalAllow,
    /// 同じ SID に**拒否の ACE**がある（種類・継承を問わない）。許可だけを引き継ぐと、
    /// 保護された DACL では拒否が消えて、その相手の権利が広がるので引き継がない（#626
    /// PR-B の監査 B-2、安全側）。拒否の ACE そのものは [`DropReason::NotAllow`] で別に出る。
    HasDeny,
    /// 利用者のアカウントかを確かめられなかった（[`AccountKind::LookupFailed`]）。
    LookupFailed(String),
}

/// 落とした ACE 1 つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DroppedAce {
    pub ace: ObservedAce,
    pub reason: DropReason,
}

/// [`plan_carry_over`] の結果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CarryOver {
    /// 引き継ぐ利用者（重複なし・DACL に現れた順）。`DirAccess::with_install_users` に渡す。
    pub carried: Vec<InstallUserSid>,
    /// 引き継がずに落とす ACE（DACL に現れた順）。
    pub dropped: Vec<DroppedAce>,
    /// 今の DACL が null（全員にフルアクセス）だった。引き継ぐものは無い。
    pub null_dacl: bool,
    /// 種類の分からない ACE・SID を読めない ACE（#626 PR-B の再レビュー 2）。1 つでも
    /// あれば、**呼ぶ側は引き継ぎを決めずに断る**（拒否を読み落として、拒否のある利用者を
    /// 引き継がないため。黙って落とさない）。
    pub blocked: Vec<ObservedAce>,
}

/// 書き直す前の DACL から、**引き継ぐ利用者**と**落とすもの**を分ける（純関数、#626
/// PR-A。モジュール doc）。SID が利用者のアカウントかは `account` で問い合わせる
/// （テストは偽物）。
///
/// 先に DACL 全体を見て、**種類の分からない ACE・SID を読めない ACE**は
/// [`CarryOver::blocked`] に入れる（呼ぶ側は断る）。拒否の ACE（[`DENY_ACE_TYPES`]、
/// 条件付き・オブジェクトを含む）の SID を集める。
///
/// 1 つの ACE は次の順に見る:
///
/// 1. 許可の ACE で SID が SYSTEM・Administrators → **どちらでもない**（書き直した DACL が
///    フルで持つので、引き継ぎも報告もしない。継承された ACE でも同じ）。
/// 2. 条件付き・オブジェクトの許可 → [`DropReason::ConditionalAllow`]。拒否 →
///    [`DropReason::NotAllow`]。
/// 3. 継承された → [`DropReason::Inherited`]。
/// 4. `OICI` でない・`INHERIT_ONLY`・`NO_PROPAGATE_INHERIT` → [`DropReason::NotTreeWide`]。
/// 5. マスクが「変更」を含まない → [`DropReason::LessThanModify`]（フルコントロールは
///    含むので、「変更」に**狭めて**引き継ぐ）。
/// 6. `InstallUserSid::parse` に通らない → [`DropReason::NotAUserSid`]。同じ SID に
///    拒否の ACE がある（DACL のどこか。種類・継承を問わない）→ [`DropReason::HasDeny`]
///    （#626 PR-B の監査 B-2）。
/// 7. 既に引き継ぐと決めた SID → **どちらでもない**（同じ利用者の 2 つ目の ACE）。
/// 8. `account` が [`AccountKind::NotAUser`] → [`DropReason::NotAnAccount`]、
///    [`AccountKind::LookupFailed`] → [`DropReason::LookupFailed`]。
/// 9. それ以外 → 引き継ぐ。
///
/// 引き継いだ利用者の、ほかの ACE（読み取りだけなど）は落とした側に残る。拒否の ACE を
/// 持つ利用者は引き継がない（拒否が消えて権利が広がるため）。
pub fn plan_carry_over(
    dacl: &ObservedDacl,
    mut account: impl FnMut(&InstallUserSid) -> AccountKind,
) -> CarryOver {
    let mut plan = CarryOver::default();
    let Some(aces) = &dacl.aces else {
        plan.null_dacl = true;
        return plan;
    };
    // 種類の分からない ACE・SID を読めない ACE は、引き継ぎを決めない（呼ぶ側が断る）。
    for ace in aces {
        if !is_known_dacl_ace_type(ace.ace_type) || ace.sid.is_empty() {
            plan.blocked.push(ace.clone());
        }
    }
    // 拒否を持つ SID（拒否の種類 - 条件付き・オブジェクトを含む - と継承を問わない）。
    let denied: Vec<&str> = aces
        .iter()
        .filter(|ace| DENY_ACE_TYPES.contains(&ace.ace_type) && !ace.sid.is_empty())
        .map(|ace| ace.sid.as_str())
        .collect();
    for ace in aces {
        if !is_known_dacl_ace_type(ace.ace_type) || ace.sid.is_empty() {
            continue;
        }
        let allow = ace.ace_type == ACCESS_ALLOWED_ACE_TYPE;
        if allow && (ace.sid == SYSTEM_SID || ace.sid == ADMINISTRATORS_SID) {
            continue;
        }
        let drop = |reason| DroppedAce {
            ace: ace.clone(),
            reason,
        };
        if CONDITIONAL_ALLOW_ACE_TYPES.contains(&ace.ace_type) {
            plan.dropped.push(drop(DropReason::ConditionalAllow));
            continue;
        }
        if !allow {
            plan.dropped.push(drop(DropReason::NotAllow));
            continue;
        }
        if ace.flags & INHERITED_ACE_FLAG != 0 {
            plan.dropped.push(drop(DropReason::Inherited));
            continue;
        }
        if ace.flags & INHERIT_FLAGS != INHERIT_FLAGS
            || ace.flags & INHERIT_ONLY_ACE_FLAG != 0
            || ace.flags & NO_PROPAGATE_INHERIT_ACE_FLAG != 0
        {
            plan.dropped.push(drop(DropReason::NotTreeWide));
            continue;
        }
        if ace.mask & MODIFY_ACCESS_MASK != MODIFY_ACCESS_MASK {
            plan.dropped.push(drop(DropReason::LessThanModify));
            continue;
        }
        let Ok(sid) = InstallUserSid::parse(&ace.sid) else {
            plan.dropped.push(drop(DropReason::NotAUserSid));
            continue;
        };
        if denied.contains(&sid.as_str()) {
            plan.dropped.push(drop(DropReason::HasDeny));
            continue;
        }
        if plan.carried.contains(&sid) {
            continue;
        }
        match account(&sid) {
            AccountKind::User => plan.carried.push(sid),
            AccountKind::NotAUser => plan.dropped.push(drop(DropReason::NotAnAccount)),
            AccountKind::LookupFailed(reason) => {
                plan.dropped.push(drop(DropReason::LookupFailed(reason)))
            }
        }
    }
    plan
}

#[cfg(test)]
mod tests;
