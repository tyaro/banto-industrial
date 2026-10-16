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
//! - [`data_acl_plan`] は、開いたフォルダの属性と所有者から、作る・書き直す・断るを決める
//!   （reparse point・フォルダでない・見知らぬ所有者を断る）。
//! - [`plan_carry_over`] は、書き直す前の DACL（`PinnedDir::read_dacl`）から、**引き継ぐ
//!   利用者**（明示の許可で「変更」相当、`SidTypeUser`）と**落とすもの**（グループ・
//!   well-known・継承・拒否など）を分ける。banto-hub の `grant-profile-acl` が、今の
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
    /// フォルダの中全体に効く形（`OICI`、`INHERIT_ONLY` なし）ではない。
    NotTreeWide,
    /// 「変更」（`MODIFY_ACCESS_MASK`）に足りない（読み取りだけなど。汎用の権利
    /// `GENERIC_*` だけの ACE もここ）。
    LessThanModify,
    /// 利用者のアカウントの領域の SID ではない（well-known・BUILTIN・サービスの SID など。
    /// `InstallUserSid::parse` に通らない）。
    NotAUserSid,
    /// 利用者の領域の SID だが、利用者のアカウントではない（グループなど）。
    NotAnAccount,
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
}

/// 書き直す前の DACL から、**引き継ぐ利用者**と**落とすもの**を分ける（純関数、#626
/// PR-A。モジュール doc）。SID が利用者のアカウントかは `account` で問い合わせる
/// （テストは偽物）。
///
/// 1 つの ACE は次の順に見る:
///
/// 1. 許可の ACE で SID が SYSTEM・Administrators → **どちらでもない**（書き直した DACL が
///    フルで持つので、引き継ぎも報告もしない。継承された ACE でも同じ）。
/// 2. 許可の ACE でない → [`DropReason::NotAllow`]。
/// 3. 継承された → [`DropReason::Inherited`]。
/// 4. `OICI` でない・`INHERIT_ONLY` → [`DropReason::NotTreeWide`]。
/// 5. マスクが「変更」を含まない → [`DropReason::LessThanModify`]（フルコントロールは
///    含むので、「変更」に**狭めて**引き継ぐ）。
/// 6. `InstallUserSid::parse` に通らない → [`DropReason::NotAUserSid`]。
/// 7. 既に引き継ぐと決めた SID → **どちらでもない**（同じ利用者の 2 つ目の ACE）。
/// 8. `account` が [`AccountKind::NotAUser`] → [`DropReason::NotAnAccount`]、
///    [`AccountKind::LookupFailed`] → [`DropReason::LookupFailed`]。
/// 9. それ以外 → 引き継ぐ。
///
/// 引き継いだ利用者の、ほかの ACE（拒否・読み取りだけなど）は落とした側に残る（拒否が
/// 消えるのを呼ぶ側が知らせられるように）。
pub fn plan_carry_over(
    dacl: &ObservedDacl,
    mut account: impl FnMut(&InstallUserSid) -> AccountKind,
) -> CarryOver {
    let mut plan = CarryOver::default();
    let Some(aces) = &dacl.aces else {
        plan.null_dacl = true;
        return plan;
    };
    for ace in aces {
        let allow = ace.ace_type == ACCESS_ALLOWED_ACE_TYPE;
        if allow && (ace.sid == SYSTEM_SID || ace.sid == ADMINISTRATORS_SID) {
            continue;
        }
        let drop = |reason| DroppedAce {
            ace: ace.clone(),
            reason,
        };
        if !allow {
            plan.dropped.push(drop(DropReason::NotAllow));
            continue;
        }
        if ace.flags & INHERITED_ACE_FLAG != 0 {
            plan.dropped.push(drop(DropReason::Inherited));
            continue;
        }
        if ace.flags & INHERIT_FLAGS != INHERIT_FLAGS || ace.flags & INHERIT_ONLY_ACE_FLAG != 0 {
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
