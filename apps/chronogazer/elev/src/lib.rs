//! ChronoGazer の UAC 昇格ヘルパー `chronogazer-elev.exe` の中身（#392 B3-3b、
//! 2026-10-16。設計は docs/chronogazer-service-design.md §3.3・§4 の B3-3b の実装メモ、
//! 計画の承認は 2026-10-16 のオーナー決定 O1〜O4）。
//!
//! **ここは昇格の境界**（管理者として動くプロセスが、昇格していない呼び出し元から
//! 引数を受ける）。そのため、受け付けるものを固定し、迷ったら止める側に倒す。
//!
//! ## 設計意図
//!
//! - **exe は製品ごと**（O1）。Win32 の部品（ローカルグループ・サービス DACL の ACE・
//!   トークン）は共有 crate `banto-service-elev`、ディレクトリの DACL は
//!   `banto-secret-store`、サービスの登録は `chronogazer_core::server_service::install_with`
//!   （`chronogazer-server.exe install` と同じ手順をプロセスの中で呼ぶ）。ここが持つのは
//!   アクションの一覧・引数の検証・手順の順序・文言・終了コードだけ。
//! - **アクションは固定の 7 つ**（[`Action`]。最初の引数が名前と完全に一致すること。
//!   大文字小文字の違い・余計な引数・重ねた引数・空の引数は使い方の誤り
//!   [`EXIT_USAGE`]）。サービス名（`ChronoGazer`）・グループ名
//!   （[`OPERATORS_GROUP`]）は固定で、引数で変えられない。
//! - **パス・環境・コマンドを呼び出し元から受けない**（O4）:
//!   - 登録する実行ファイルは、この exe の隣の `chronogazer-server.exe`
//!     （[`SERVER_EXE_FILE_NAME`]。無ければ [`EXIT_PRECONDITION`]）。Program Files の外
//!     なら警告する（LocalSystem のサービスとして一般の利用者が書き換えられる exe を
//!     登録すると、権限の昇格に使われる）。
//!   - データのルートは**マシンの環境だけ**から決める（[`resolve_machine_root`]）:
//!     `HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment` の
//!     `CHRONOGAZER_ROOT`、無ければマシンの ProgramData（`SHGetKnownFolderPath`）+
//!     `ChronoGazer`。**このプロセスの環境変数・利用者の環境変数は読まない**（昇格した
//!     プロセスの環境は呼び出し元が用意できるうえ、LocalSystem のサービスが見るのは
//!     マシンの環境だけ）。
//! - **インストール利用者の SID**（O3）: 引数で受けるのは `service-install` の
//!   `--install-user-sid` だけ。`InstallUserSid::parse`（利用者の領域の SID の形）に
//!   加えて、`LookupAccountSidW` で**この PC の利用者のアカウント（`SidTypeUser`）**で
//!   あることを確かめる（`chronogazer-server install` の既知の制限 5 より厳しい）。
//!   省けば、このプロセスのトークンの利用者（同じく確かめる）。UAC の昇格で別の
//!   管理者の資格情報を入れると（over-the-shoulder）、トークンの利用者はその管理者に
//!   なるので、昇格していないシェルは自分の SID を `--install-user-sid` で渡す（B2-5 /
//!   B3-4）。それ以外のアクションは、登録で記録した `Parameters\InstallUserSid`
//!   （B3-2a の D1）**だけ**を読み、無ければ [`EXIT_PRECONDITION`]。
//! - **Operators**: グループ `ChronoGazer Operators`（banto-hub の `BantoHub Operators`
//!   とは別）を作り（冪等）、記録したインストール利用者を足す。サービス `ChronoGazer` の
//!   **既存の DACL に** `(A;;CCLCRPWP;;;<Operators>)` を 1 つ足す（照会・開始・停止
//!   だけ。`DC`・`WD`・`WO`・`SD` は渡さない - `banto_service_elev` のモジュール doc）。
//! - **データの ACL**（`grant-data-acl`、O2）: ルート → `secrets\` → `secrets\hub-keys\`
//!   の順に、[`data_acl_plan`] のとおり作る・書き直す・断る。既にあるフォルダは
//!   `banto_secret_store::acl::pin_dir` で**辿らずに**開き（持っている間は名前の変更・
//!   削除ができない）、reparse point（ジャンクション・シンボリックリンクなど）か、
//!   所有者が SYSTEM・Administrators・インストール利用者・この昇格した利用者のどれでも
//!   なければ**書き直さずに止める**。Users・Operators には何も付けない。
//!   中のジャンクションの先を書き換えないこと（伝播は辿らない）は
//!   `banto_secret_store::acl` のテストが固定する。ハードリンクの扱いは既知の制限
//!   （同じモジュール doc）。
//! - **自動開始**: 登録の直後は手動（T17 P4、§3.3）。`autostart-enable` で自動（遅延）、
//!   `autostart-disable` で手動に戻す（`WindowsServiceManager::set_auto_start`。登録を
//!   隣の `chronogazer-server.exe` で組み立て直す）。
//!
//! ## 終了コード
//!
//! [`EXIT_OK`] 0 = 完了、[`EXIT_FAILED`] 1 = 操作の失敗（断った場合を含む）、
//! [`EXIT_PRECONDITION`] 2 = 前提が無い（サービス未登録・インストール利用者の記録が
//! 無い・`chronogazer-server.exe` が隣に無い・ルートを決められない・グループが無い）、
//! [`EXIT_USAGE`] 64 = 使い方の誤り。Windows 以外はすべて 64。`service-install` は
//! 最初に失敗した手順の終了コードで止める（後の手順は行わない）。
//!
//! ## 出力
//!
//! どの行も `chronogazer-elev: ` で始まる（行は純関数で組み立て、golden テストで固定）。
//! `service-install` が中で使う `install_with` の行（`chronogazer-server: …`）も、
//! 頭を `chronogazer-elev: ` に付け替えて出す（[`relabel`]）。インストーラは
//! `nsExec::ExecToLog` で拾う（B3-4）。
//!
//! ## テスト
//!
//! OS を触る操作はすべて [`ElevOps`]（Windows の実装は `win` モジュール）を通すので、
//! 手順の順序・最初の失敗で止まること・終了コードは偽の [`ElevOps`] で確かめる。
//! **テストは本物のローカルグループ・サービスの DACL・HKLM・ProgramData に触らない**
//! （GitHub の Windows のランナーは管理者で、本当に変えてしまう）。

use std::path::{Path, PathBuf};

use banto_secret_store::acl::{PinnedDirInfo, ADMINISTRATORS_SID, SYSTEM_SID};
use banto_secret_store::{DirAccess, InstallUserSid};
use banto_service_control::{ConsoleLine, ServiceManagerError, UninstallError, UninstallOutcome};
use banto_service_elev::{ElevError, GroupOutcome, MemberOutcome, OperatorsGroup};
use chronogazer_core::paths::{DataLayout, PRODUCT_DIR_NAME, ROOT_ENV};
use chronogazer_core::server_service::{
    install_result_lines, uninstall_lines, InstallReport, InstallUserUnavailable,
    INSTALL_USER_SID_LOCATION,
};
use chronogazer_core::shell_launch::SERVICE_NAME;

#[cfg(windows)]
pub mod win;

/// 出力の行の頭（`chronogazer-elev: `）。
pub const LABEL: &str = "chronogazer-elev";

/// `chronogazer-server` の行の頭（[`relabel`] で付け替える）。
const SERVER_LABEL_PREFIX: &str = "chronogazer-server:";

/// 完了。
pub const EXIT_OK: i32 = 0;
/// 操作の失敗（断った場合を含む）。
pub const EXIT_FAILED: i32 = 1;
/// 前提が無い（サービス未登録・記録が無い・隣に `chronogazer-server.exe` が無いなど）。
pub const EXIT_PRECONDITION: i32 = 2;
/// 使い方の誤り（Windows 以外はすべてこれ）。
pub const EXIT_USAGE: i32 = 64;

/// ChronoGazer の Operators（サービスの開始・停止を委任された利用者）。
pub const OPERATORS_GROUP: OperatorsGroup = OperatorsGroup {
    name: "ChronoGazer Operators",
    comment:
        "ChronoGazer 記録エンジン（Windows サービス ChronoGazer）の開始・停止を委任された利用者",
};

/// 登録する実行ファイルの名前（この exe の隣にあること）。
pub const SERVER_EXE_FILE_NAME: &str = "chronogazer-server.exe";

/// マシンの環境変数のレジストリキー（`HKLM` の下）。
pub const MACHINE_ENVIRONMENT_KEY: &str =
    r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";

/// `--install-user-sid`。
pub const INSTALL_USER_SID_FLAG: &str = "--install-user-sid";

// --- 引数 -------------------------------------------------------------------------

/// 受け付けるアクション（固定の 7 つ）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// 登録 → setup-operators → grant-service-acl → grant-data-acl（最初の失敗で止める）。
    /// `install_user_sid` は `--install-user-sid`（省けばこのプロセスのトークンの利用者）。
    ServiceInstall {
        install_user_sid: Option<InstallUserSid>,
    },
    /// 登録の解除（グループとデータは残す）。
    ServiceUninstall,
    /// `ChronoGazer Operators` を作り（冪等）、記録したインストール利用者を足す。
    SetupOperators,
    /// サービスの DACL に Operators の ACE（`CCLCRPWP`）を足す。
    GrantServiceAcl,
    /// データのルート・`secrets\`・`secrets\hub-keys\` の ACL。
    GrantDataAcl,
    /// 自動開始（遅延）。
    AutostartEnable,
    /// 手動開始に戻す。
    AutostartDisable,
}

impl Action {
    /// アクションの名前の一覧（使い方の表示とテスト）。
    pub const ALL_NAMES: [&'static str; 7] = [
        "service-install",
        "service-uninstall",
        "setup-operators",
        "grant-service-acl",
        "grant-data-acl",
        "autostart-enable",
        "autostart-disable",
    ];

    /// アクションの名前。
    pub fn name(&self) -> &'static str {
        match self {
            Self::ServiceInstall { .. } => "service-install",
            Self::ServiceUninstall => "service-uninstall",
            Self::SetupOperators => "setup-operators",
            Self::GrantServiceAcl => "grant-service-acl",
            Self::GrantDataAcl => "grant-data-acl",
            Self::AutostartEnable => "autostart-enable",
            Self::AutostartDisable => "autostart-disable",
        }
    }
}

/// 引数（プログラム名を除いたもの）を解釈する（純関数）。誤りは理由の文言
/// （[`EXIT_USAGE`] にする）。
///
/// - 最初の引数がアクションの名前と**完全に一致**すること（大文字小文字も）。
/// - `service-install` だけが `--install-user-sid <SID>` を 1 回だけ受ける（SID は
///   `InstallUserSid::parse` の形。利用者のアカウントかは実行のときに確かめる）。
/// - それ以外のアクションは追加の引数を受けない。
pub fn parse_args(args: &[String]) -> Result<Action, String> {
    let Some((first, rest)) = args.split_first() else {
        return Err("アクションがありません".to_string());
    };
    let action = match first.as_str() {
        "service-install" => {
            let install_user_sid = match rest {
                [] => None,
                [flag, value] if flag == INSTALL_USER_SID_FLAG => {
                    Some(InstallUserSid::parse(value).map_err(|err| {
                        format!("{INSTALL_USER_SID_FLAG} の値が使えません（{value:?}）: {err}")
                    })?)
                }
                [flag] if flag == INSTALL_USER_SID_FLAG => {
                    return Err(format!(
                        "{INSTALL_USER_SID_FLAG} には SID（S-1-5-21-…）を続けてください"
                    ));
                }
                _ => {
                    return Err(format!(
                        "service-install が受ける引数は {INSTALL_USER_SID_FLAG} <SID> だけです（{} 個受け取りました）",
                        rest.len()
                    ));
                }
            };
            return Ok(Action::ServiceInstall { install_user_sid });
        }
        "service-uninstall" => Action::ServiceUninstall,
        "setup-operators" => Action::SetupOperators,
        "grant-service-acl" => Action::GrantServiceAcl,
        "grant-data-acl" => Action::GrantDataAcl,
        "autostart-enable" => Action::AutostartEnable,
        "autostart-disable" => Action::AutostartDisable,
        other => {
            return Err(format!(
                "不明なアクション {other:?} です（固定の 7 つだけを受け付けます）"
            ))
        }
    };
    if !rest.is_empty() {
        return Err(format!(
            "'{}' は追加の引数を受け付けません（{} 個受け取りました）",
            action.name(),
            rest.len()
        ));
    }
    Ok(action)
}

/// 使い方の行（標準エラー出力）。
pub fn usage_lines(reason: &str) -> Vec<ConsoleLine> {
    vec![
        err(reason),
        err("使用法: chronogazer-elev.exe <action>"),
        err(&format!("  action = {}", Action::ALL_NAMES.join(" | "))),
        err(&format!(
            "  service-install だけが {INSTALL_USER_SID_FLAG} S-1-5-21-… を受けます（省けば実行した利用者）"
        )),
    ]
}

/// Windows 以外で実行されたときの行。
pub fn not_windows_lines() -> Vec<ConsoleLine> {
    vec![err(
        "このツールは Windows 専用です（UAC の昇格・ローカルグループ・SCM・NTFS の ACL を使います）",
    )]
}

// --- 行 -----------------------------------------------------------------------------

fn out_line(text: &str) -> ConsoleLine {
    ConsoleLine::Stdout(format!("{LABEL}: {text}"))
}

fn err(text: &str) -> ConsoleLine {
    ConsoleLine::Stderr(format!("{LABEL}: {text}"))
}

/// `chronogazer-server: …` の行の頭を `chronogazer-elev: …` に付け替える（純関数。
/// それ以外の行はそのまま）。
pub fn relabel(lines: Vec<ConsoleLine>) -> Vec<ConsoleLine> {
    let swap = |text: String| match text.strip_prefix(SERVER_LABEL_PREFIX) {
        Some(rest) => format!("{LABEL}:{rest}"),
        None => text,
    };
    lines
        .into_iter()
        .map(|line| match line {
            ConsoleLine::Stdout(text) => ConsoleLine::Stdout(swap(text)),
            ConsoleLine::Stderr(text) => ConsoleLine::Stderr(swap(text)),
        })
        .collect()
}

/// 最後の 1 行。
pub fn finished_line(action: &Action, code: i32) -> ConsoleLine {
    if code == EXIT_OK {
        out_line(&format!("'{}' が完了しました", action.name()))
    } else {
        err(&format!(
            "'{}' は完了しませんでした（終了コード {code}）",
            action.name()
        ))
    }
}

// --- データのルート（マシンの環境） ------------------------------------------------

/// マシンの環境の読み取り（Windows の実装は `win::RegistryMachineEnvironment`。
/// テストは偽物）。
pub trait MachineEnvironment {
    /// `HKLM\…\Session Manager\Environment` の `CHRONOGAZER_ROOT` の生の値（展開しない）。
    /// 無ければ `Ok(None)`。
    fn chronogazer_root(&self) -> Result<Option<String>, String>;
    /// マシンの ProgramData（`SHGetKnownFolderPath(FOLDERID_ProgramData)`）。
    fn program_data(&self) -> Result<PathBuf, String>;
    /// ルートにしてはいけない既知のフォルダ（[`reject_well_known_root`]）。取れなければ
    /// 理由（ルートを決めずに止める）。
    fn known_folders(&self) -> Result<KnownFolders, String>;
}

/// ルートにしてはいけない既知のフォルダ（#392 B3-3b の監査 B1。Windows は
/// `SHGetKnownFolderPath` で取る）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KnownFolders {
    /// そのもの・祖先・**中**も断る（OS・プログラムの置き場、だれでも書ける共有の場所）:
    /// Windows・System・SystemX86・Program Files（x86・Common を含む）・Public。
    pub forbidden_inside: Vec<PathBuf>,
    /// そのもの・祖先だけを断る（中は使ってよい）: ProgramData・UserProfiles（`C:\Users`）。
    pub forbidden_itself: Vec<PathBuf>,
}

/// パスの要素（大文字小文字を無視して比べるため小文字にする）。
fn lower_components(path: &Path) -> Vec<String> {
    path.components()
        .filter(|c| !matches!(c, std::path::Component::CurDir))
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect()
}

/// `inner` が `outer` そのものか、その中か（要素の境界で比べる。`Program Files Evil` は
/// `Program Files` の中ではない）。
fn is_same_or_inside(inner: &Path, outer: &Path) -> bool {
    let inner = lower_components(inner);
    let outer = lower_components(outer);
    !outer.is_empty() && inner.len() >= outer.len() && inner[..outer.len()] == outer[..]
}

/// ルートが既知のシステムフォルダ・ドライブのルート・ネットワークの場所でないことを
/// 確かめる（純関数。監査 B1）。`grant-data-acl` の書き直しはルートの下の**全体**に
/// 伝わるので、`D:\`・`C:\ProgramData`・`C:\Users` などをルートにすると、他の製品・
/// 利用者のデータの ACL を書き換えてしまう。断るのは:
///
/// - UNC・`\\?\`・`\\.\`（`\\` か `//` で始まる）。
/// - ドライブのルート（普通の名前の要素が 1 つも無い）。
/// - [`KnownFolders::forbidden_inside`] のそのもの・祖先・中。
/// - [`KnownFolders::forbidden_itself`] のそのもの・祖先。
pub fn reject_well_known_root(root: &Path, known: &KnownFolders) -> Result<(), String> {
    let text = root.to_string_lossy();
    if text.starts_with("\\\\") || text.starts_with("//") {
        return Err(format!(
            "データのルート（{}）はネットワークの場所か特殊なパス（\\\\…）です。ローカルのドライブのフォルダにしてください",
            root.display()
        ));
    }
    if !root
        .components()
        .any(|c| matches!(c, std::path::Component::Normal(_)))
    {
        return Err(format!(
            "データのルート（{}）はドライブのルートです。専用のフォルダ（例: D:\\ChronoGazer）にしてください",
            root.display()
        ));
    }
    for folder in &known.forbidden_inside {
        if is_same_or_inside(root, folder) || is_same_or_inside(folder, root) {
            return Err(format!(
                "データのルート（{}）は OS・プログラムの置き場か共有のフォルダ（{}）に重なります。専用のフォルダにしてください",
                root.display(),
                folder.display()
            ));
        }
    }
    for folder in &known.forbidden_itself {
        if is_same_or_inside(folder, root) {
            return Err(format!(
                "データのルート（{}）は {} そのものかその上のフォルダです。その下の専用のフォルダにしてください",
                root.display(),
                folder.display()
            ));
        }
    }
    Ok(())
}

/// データのルートを**マシンの環境だけ**から決める（純関数。モジュール doc）:
///
/// 1. `CHRONOGAZER_ROOT` が空でなければそれ。`%` を含む（展開が要る）値は使わない
///    （展開はこのプロセスの環境に頼ることになるため）。読めなければ止める
///    （ProgramData へ落とすと、サービスと違う場所を直しうるため）。
/// 2. 無ければマシンの ProgramData + `ChronoGazer`。
/// 3. どちらも `server_host::validate_root`（絶対パス・`.`/`..` なし）と
///    [`reject_well_known_root`]（システムフォルダ・ドライブのルート・UNC でない）を
///    通ること。
pub fn resolve_machine_root(env: &impl MachineEnvironment) -> Result<PathBuf, String> {
    let root = match env.chronogazer_root() {
        Err(error) => {
            return Err(format!(
                "マシンの環境変数 {ROOT_ENV} を読めませんでした: {error}"
            ))
        }
        Ok(Some(value)) if !value.is_empty() => {
            if value.contains('%') {
                return Err(format!(
                    "マシンの環境変数 {ROOT_ENV}（{value:?}）は展開が要る値です。展開しない絶対パス（例: D:\\ChronoGazer）にしてください"
                ));
            }
            PathBuf::from(value)
        }
        Ok(_) => env
            .program_data()
            .map_err(|error| format!("ProgramData の場所を取れませんでした: {error}"))?
            .join(PRODUCT_DIR_NAME),
    };
    chronogazer_core::server_host::validate_root(&root)?;
    let known = env
        .known_folders()
        .map_err(|error| format!("既知のフォルダの場所を取れませんでした: {error}"))?;
    reject_well_known_root(&root, &known)?;
    Ok(root)
}

/// `exe` が Program Files（`program_files_dirs` のどれか。x86 を含む）の**中**にあるか
/// （純関数。要素の境界で比べ、大文字小文字は無視。監査 B2）。場所が 1 つも分からなければ
/// `false`（止める側）。
pub fn is_inside_program_files(exe: &Path, program_files_dirs: &[PathBuf]) -> bool {
    program_files_dirs.iter().any(|dir| {
        is_same_or_inside(exe, dir) && lower_components(exe).len() > lower_components(dir).len()
    })
}

/// 登録されているコマンドライン（SCM の生の文字列）が、`"<exe>" run-service` と同じか
/// （純関数。監査 B3）。引用符の有無・前後の空白・大文字小文字の違いは同じとみなす。
pub fn registered_command_matches(raw: &str, exe: &Path) -> bool {
    let raw = raw.trim();
    let (path, rest) = match raw.strip_prefix('"') {
        Some(after) => match after.split_once('"') {
            Some((path, rest)) => (path, rest),
            None => return false,
        },
        None => match raw.split_once(' ') {
            Some((path, rest)) => (path, rest),
            None => (raw, ""),
        },
    };
    path.to_lowercase() == exe.to_string_lossy().to_lowercase()
        && rest.trim() == chronogazer_core::shell_launch::RUN_SERVICE_ARG
}

// --- データの ACL の計画 ----------------------------------------------------------

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
    /// 所有者が許す範囲の外（乗っ取りの恐れ）。
    ForeignOwner(String),
}

/// フォルダの状態から、することか止める理由を決める（純関数）。所有者として許すのは
/// SYSTEM・Administrators・インストール利用者と、`elevated_user`（この昇格した
/// プロセスのトークンの利用者。管理者なので信頼してよく、「作成者を所有者に」の
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

// --- OS の操作 ---------------------------------------------------------------------

/// 辿らずに開いたフォルダ（Windows は `banto_secret_store::acl::PinnedDir`）。
/// 持っている間は名前の変更・削除ができない。
pub trait PinnedDirOps {
    /// 開いたハンドルから読んだ属性と所有者。
    fn info(&self) -> PinnedDirInfo;
    /// 開いたままの DACL を書き直す。
    fn reset_acl(&self, access: &DirAccess) -> Result<(), String>;
}

/// elev が OS に対してする操作（Windows の実装は `win::WindowsOps`。テストは偽物）。
pub trait ElevOps {
    /// [`ElevOps::pin_dir`] が返すもの。
    type Pinned: PinnedDirOps;

    /// この exe の隣の `chronogazer-server.exe`（無ければ理由）。
    fn server_executable(&self) -> Result<PathBuf, String>;
    /// マシンの Program Files と Program Files (x86)（取れたものだけ。1 つも無ければ
    /// 隣の exe を使わずに止める）。
    fn program_files_dirs(&self) -> Vec<PathBuf>;
    /// 登録されているサービスの実行のコマンドライン（SCM の生の文字列）。未登録なら
    /// `Ok(None)`。
    fn registered_command_line(&self) -> Result<Option<String>, String>;
    /// データのルート（[`resolve_machine_root`]）。
    fn machine_root(&self) -> Result<PathBuf, String>;
    /// このプロセスのトークンの利用者。
    fn current_user_sid(&self) -> Result<InstallUserSid, String>;
    /// この PC の利用者のアカウント（`SidTypeUser`）か。
    fn check_user_account(&self, sid: &InstallUserSid) -> Result<(), String>;
    /// `chronogazer_core::server_service::install_with`。
    fn install(
        &self,
        executable: PathBuf,
        sid: InstallUserSid,
        layout: &DataLayout,
    ) -> Result<InstallReport, InstallUserUnavailable>;
    /// `banto_service_control::uninstall_service("ChronoGazer")`。
    fn uninstall(&self) -> Result<UninstallOutcome, UninstallError>;
    /// 記録したインストール利用者（`Parameters\InstallUserSid`）。無ければ `Ok(None)`。
    fn read_install_user(&self) -> Result<Option<String>, String>;
    /// [`OPERATORS_GROUP`] を作る（冪等）。
    fn ensure_group(&self) -> Result<GroupOutcome, ElevError>;
    /// [`OPERATORS_GROUP`] に SID で足す（冪等）。
    fn add_member(&self, sid: &InstallUserSid) -> Result<MemberOutcome, ElevError>;
    /// サービス `ChronoGazer` の DACL に [`OPERATORS_GROUP`] の ACE を足す（冪等）。
    fn grant_service_acl(&self) -> Result<(), ElevError>;
    /// 自動開始（遅延）・手動開始。
    fn set_autostart(&self, executable: &Path, enabled: bool) -> Result<(), ServiceManagerError>;
    /// フォルダを辿らずに開く（無ければ `Ok(None)`）。
    fn pin_dir(&self, dir: &Path) -> Result<Option<Self::Pinned>, String>;
    /// 無ければ保護された DACL で作る（作ったら `true`）。
    fn create_dir(&self, dir: &Path, access: &DirAccess) -> Result<bool, String>;
}

// --- 実行 ---------------------------------------------------------------------------

/// 行の出し先（本番は標準出力・標準エラー出力へすぐ出す。テストは集める）。
pub type Out<'a> = &'a mut dyn FnMut(ConsoleLine);

/// アクションを実行し、終了コードを返す（最後の 1 行 [`finished_line`] も出す）。
pub fn run<O: ElevOps>(action: &Action, ops: &O, out: Out<'_>) -> i32 {
    let code = match action {
        Action::ServiceInstall { install_user_sid } => {
            service_install(ops, install_user_sid.clone(), out)
        }
        Action::ServiceUninstall => service_uninstall(ops, out),
        Action::SetupOperators => setup_operators(ops, out),
        Action::GrantServiceAcl => grant_service_acl(ops, out),
        Action::GrantDataAcl => grant_data_acl(ops, out),
        Action::AutostartEnable => autostart(ops, true, out),
        Action::AutostartDisable => autostart(ops, false, out),
    };
    out(finished_line(action, code));
    code
}

fn emit(out: Out<'_>, lines: Vec<ConsoleLine>) {
    for line in lines {
        out(line);
    }
}

/// 隣の `chronogazer-server.exe` を決める（無ければ 2）。**Program Files（x86 を含む）の
/// 外なら 2 で止める**（監査 B2。LocalSystem のサービスとして一般の利用者が書き換え
/// られる場所の exe を登録すると、その場所に書ける誰もが SYSTEM の権限を得る。
/// Program Files の場所が分からないときも止める）。
fn server_executable<O: ElevOps>(ops: &O, out: Out<'_>) -> Result<PathBuf, i32> {
    let executable = ops.server_executable().map_err(|error| {
        out(err(&format!(
            "この exe の隣に {SERVER_EXE_FILE_NAME} がありません: {error}（インストール先のフォルダの chronogazer-elev.exe を実行してください）"
        )));
        EXIT_PRECONDITION
    })?;
    if !is_inside_program_files(&executable, &ops.program_files_dirs()) {
        out(err(&format!(
            "{} は Program Files の外にあります。一般の利用者が書き換えられる場所の exe を LocalSystem のサービスとして使うと権限の昇格に使われるため、止めます（C:\\Program Files\\ChronoGazer\\ に置いた chronogazer-elev.exe を実行してください）",
            executable.display()
        )));
        return Err(EXIT_PRECONDITION);
    }
    Ok(executable)
}

fn machine_root<O: ElevOps>(ops: &O, out: Out<'_>) -> Result<PathBuf, i32> {
    ops.machine_root().map_err(|reason| {
        out(err(&format!("データのルートを決められません: {reason}")));
        EXIT_PRECONDITION
    })
}

/// 記録したインストール利用者（無い・不正・利用者のアカウントでない → 2、読めない → 1）。
fn recorded_install_user<O: ElevOps>(ops: &O, out: Out<'_>) -> Result<InstallUserSid, i32> {
    let raw = match ops.read_install_user() {
        Ok(Some(raw)) => raw,
        Ok(None) => {
            out(err(&format!(
                "インストール利用者の記録（{INSTALL_USER_SID_LOCATION}）がありません。先に service-install を実行してください"
            )));
            return Err(EXIT_PRECONDITION);
        }
        Err(error) => {
            out(err(&format!(
                "インストール利用者の記録（{INSTALL_USER_SID_LOCATION}）を読めませんでした: {error}"
            )));
            return Err(EXIT_FAILED);
        }
    };
    let sid = InstallUserSid::parse(&raw).map_err(|_| {
        out(err(&format!(
            "インストール利用者の記録（{INSTALL_USER_SID_LOCATION}）が利用者のアカウントの SID ではありません（{raw:?}）。service-uninstall → service-install で登録し直してください"
        )));
        EXIT_PRECONDITION
    })?;
    ops.check_user_account(&sid).map_err(|error| {
        out(err(&format!(
            "記録されたインストール利用者（{}）はこの PC の利用者のアカウントとして確かめられません: {error}。service-uninstall → service-install で登録し直してください",
            sid.as_str()
        )));
        EXIT_PRECONDITION
    })?;
    Ok(sid)
}

fn service_install<O: ElevOps>(
    ops: &O,
    install_user_sid: Option<InstallUserSid>,
    out: Out<'_>,
) -> i32 {
    let executable = match server_executable(ops, out) {
        Ok(executable) => executable,
        Err(code) => return code,
    };
    let root = match machine_root(ops, out) {
        Ok(root) => root,
        Err(code) => return code,
    };
    let sid = match install_user_sid {
        Some(sid) => {
            if let Err(error) = ops.check_user_account(&sid) {
                out(err(&format!(
                    "{INSTALL_USER_SID_FLAG} の SID（{}）はこの PC の利用者のアカウントとして確かめられません: {error}（何も登録していません）",
                    sid.as_str()
                )));
                return EXIT_USAGE;
            }
            sid
        }
        None => {
            let sid = match ops.current_user_sid() {
                Ok(sid) => sid,
                Err(error) => {
                    out(err(&format!(
                        "インストール利用者（この PC の利用者のアカウント）を決められませんでした: {error}。{INSTALL_USER_SID_FLAG} S-1-5-21-… で指定してください（何も登録していません）"
                    )));
                    return EXIT_PRECONDITION;
                }
            };
            if let Err(error) = ops.check_user_account(&sid) {
                out(err(&format!(
                    "実行した利用者（{}）はこの PC の利用者のアカウントとして確かめられません: {error}。{INSTALL_USER_SID_FLAG} S-1-5-21-… で指定してください（何も登録していません）",
                    sid.as_str()
                )));
                return EXIT_PRECONDITION;
            }
            sid
        }
    };

    // 0. 既にあるルートを、登録の**前に**確かめる（監査 B4）: reparse point・フォルダで
    //    ない・見知らぬ所有者なら、何も登録せずに止める（先に作られたルートにサービスを
    //    向けない）。開いたルートは最後の手順まで持つ。
    let elevated_user = ops.current_user_sid().ok();
    let _held_root = match preflight_root(
        ops,
        &root,
        &sid,
        elevated_user.as_ref().map(InstallUserSid::as_str),
        out,
    ) {
        Ok(held) => held,
        Err(code) => return code,
    };

    // 1. 登録・記録・ルート（`chronogazer-server install` と同じ手順）。
    let result = ops.install(executable, sid, &DataLayout::new(root));
    let (lines, code) = install_result_lines(&result);
    emit(out, relabel(lines));
    if code != EXIT_OK {
        return EXIT_FAILED;
    }
    // 2〜4. 単独のアクションと同じ手順（記録した SID を読み直す）。最初の失敗で止める。
    for step in [setup_operators, grant_service_acl, grant_data_acl] {
        let code = step(ops, out);
        if code != EXIT_OK {
            return code;
        }
    }
    EXIT_OK
}

/// `service-install` の前の、ルートの確かめ（監査 B4）。無ければ何もしない（`Ok(None)`。
/// 登録の手順が作る）。あれば辿らずに開き、[`data_acl_plan`] で断る理由があれば 1。
fn preflight_root<O: ElevOps>(
    ops: &O,
    root: &Path,
    install_user: &InstallUserSid,
    elevated_user: Option<&str>,
    out: Out<'_>,
) -> Result<Option<O::Pinned>, i32> {
    let pinned = ops.pin_dir(root).map_err(|error| {
        out(err(&format!(
            "{}（{}）を開けませんでした: {error}（何も登録していません）",
            DataDir::Root.label(),
            root.display()
        )));
        EXIT_FAILED
    })?;
    let Some(pinned) = pinned else {
        return Ok(None);
    };
    if let Err(refusal) = data_acl_plan(
        &DirState::Present(pinned.info()),
        install_user,
        elevated_user,
    ) {
        out(err(&format!(
            "{}（何も登録していません）",
            refusal_line(DataDir::Root, root, &refusal)
        )));
        return Err(EXIT_FAILED);
    }
    Ok(Some(pinned))
}

fn service_uninstall<O: ElevOps>(ops: &O, out: Out<'_>) -> i32 {
    let result = ops.uninstall();
    let root = ops
        .machine_root()
        .unwrap_or_else(|_| PathBuf::from("データのルート"));
    emit(out, relabel(uninstall_lines(&result, &root)));
    match result {
        Ok(_) => {
            out(out_line(&format!(
                "ローカルグループ '{}' は残します（要らなければ lusrmgr.msc で消してください）",
                OPERATORS_GROUP.name
            )));
            EXIT_OK
        }
        // 管理者として動いているので、開けないのはほぼ「未登録」。
        Err(UninstallError::Open(_)) => EXIT_PRECONDITION,
        Err(_) => EXIT_FAILED,
    }
}

fn setup_operators<O: ElevOps>(ops: &O, out: Out<'_>) -> i32 {
    let sid = match recorded_install_user(ops, out) {
        Ok(sid) => sid,
        Err(code) => return code,
    };
    match ops.ensure_group() {
        Ok(GroupOutcome::Created) => out(out_line(&format!(
            "ローカルグループ '{}' を作りました",
            OPERATORS_GROUP.name
        ))),
        Ok(GroupOutcome::Existed) => out(out_line(&format!(
            "ローカルグループ '{}' は既にあります",
            OPERATORS_GROUP.name
        ))),
        Err(error) => {
            out(err(&error.to_string()));
            return EXIT_FAILED;
        }
    }
    match ops.add_member(&sid) {
        Ok(MemberOutcome::Added) => out(out_line(&format!(
            "インストール利用者（{}）を '{}' に足しました（反映は次のサインインから）",
            sid.as_str(),
            OPERATORS_GROUP.name
        ))),
        Ok(MemberOutcome::AlreadyMember) => out(out_line(&format!(
            "インストール利用者（{}）は既に '{}' のメンバーです",
            sid.as_str(),
            OPERATORS_GROUP.name
        ))),
        Err(error) => {
            out(err(&error.to_string()));
            return EXIT_FAILED;
        }
    }
    EXIT_OK
}

/// `ERROR_SERVICE_DOES_NOT_EXIST`。
const ERROR_SERVICE_DOES_NOT_EXIST: u32 = 1060;

fn grant_service_acl<O: ElevOps>(ops: &O, out: Out<'_>) -> i32 {
    match ops.grant_service_acl() {
        Ok(()) => {
            out(out_line(&format!(
                "サービス '{SERVICE_NAME}' の DACL に '{}' の ACE（{}: 照会・開始・停止）を足しました",
                OPERATORS_GROUP.name,
                banto_service_elev::OPERATORS_SERVICE_SDDL_RIGHTS
            )));
            EXIT_OK
        }
        Err(error @ ElevError::GroupNotFound(_)) => {
            out(err(&format!(
                "{error}（先に setup-operators を実行してください）"
            )));
            EXIT_PRECONDITION
        }
        Err(
            error @ ElevError::OpenService {
                os_error: ERROR_SERVICE_DOES_NOT_EXIST,
                ..
            },
        ) => {
            out(err(&format!(
                "{error}（先に service-install を実行してください）"
            )));
            EXIT_PRECONDITION
        }
        Err(error) => {
            out(err(&error.to_string()));
            EXIT_FAILED
        }
    }
}

/// フォルダの種類（行の文言）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DataDir {
    Root,
    Secrets,
    HubKeys,
}

impl DataDir {
    fn label(self) -> &'static str {
        match self {
            Self::Root => "データのルート",
            Self::Secrets => "秘密の置き場（secrets）",
            Self::HubKeys => "Hub の API キーの保存先（secrets\\hub-keys）",
        }
    }
}

/// 1 つのフォルダを扱う。無ければ作る（`create_if_absent`）か何もしない。あれば
/// 辿らずに開いて [`data_acl_plan`] のとおり書き直すか止める。開いたフォルダは返して、
/// 呼ぶ側が全部の手順が終わるまで持つ（下のフォルダを扱う間に名前を変えさせない）。
fn grant_dir<O: ElevOps>(
    ops: &O,
    kind: DataDir,
    dir: &Path,
    install_user: &InstallUserSid,
    elevated_user: Option<&str>,
    create_if_absent: bool,
    out: Out<'_>,
) -> Result<Option<O::Pinned>, i32> {
    let access = DirAccess::with_install_user(install_user.clone());
    let mut created = false;
    let mut tried_create = false;
    // 1 回目に無ければ作り、2 回目で開いて確かめる（作る前に誰かが作ったときも同じ）。
    // 作ってもまだ無ければ（作った直後に消された）、作り直さずに止める。
    loop {
        let pinned = ops.pin_dir(dir).map_err(|error| {
            out(err(&format!(
                "{}（{}）を開けませんでした: {error}",
                kind.label(),
                dir.display()
            )));
            EXIT_FAILED
        })?;
        let Some(pinned) = pinned else {
            if tried_create {
                break;
            }
            if !create_if_absent {
                out(out_line(&format!(
                    "{}（{}）はまだありません（エンジンが初めて使うときに作ります）",
                    kind.label(),
                    dir.display()
                )));
                return Ok(None);
            }
            created = ops.create_dir(dir, &access).map_err(|error| {
                out(err(&format!(
                    "{}（{}）を作れませんでした: {error}",
                    kind.label(),
                    dir.display()
                )));
                EXIT_FAILED
            })?;
            tried_create = true;
            continue;
        };
        let info = pinned.info();
        match data_acl_plan(&DirState::Present(info), install_user, elevated_user) {
            Err(refusal) => {
                out(err(&refusal_line(kind, dir, &refusal)));
                return Err(EXIT_FAILED);
            }
            // 作った直後でも書き直す（作ってから開くまでの間にすり替えられていても、
            // 開いたものを期待の形にする。同じ形なら何も変わらない）。
            Ok(_) => {
                pinned.reset_acl(&access).map_err(|error| {
                    out(err(&format!(
                        "{}（{}）の ACL を書き直せませんでした: {error}",
                        kind.label(),
                        dir.display()
                    )));
                    EXIT_FAILED
                })?;
                let what = if created {
                    "作りました"
                } else {
                    "ACL を書き直しました"
                };
                out(out_line(&format!(
                    "{}: {}（{what}。SYSTEM・Administrators にフル、インストール利用者 {} に変更、親から継承しない）",
                    kind.label(),
                    dir.display(),
                    install_user.as_str()
                )));
                return Ok(Some(pinned));
            }
        }
    }
    out(err(&format!(
        "{}（{}）を作った後に開けませんでした",
        kind.label(),
        dir.display()
    )));
    Err(EXIT_FAILED)
}

/// 書き直さずに止めたときの 1 行（純関数）。
fn refusal_line(kind: DataDir, dir: &Path, refusal: &DataAclRefusal) -> String {
    let reason = match refusal {
        DataAclRefusal::ReparsePoint => {
            "ジャンクションかシンボリックリンク（reparse point）です。リンクの先を書き換えないよう、書き直さずに止めます（リンクを外して、普通のフォルダにしてからもう一度実行してください）".to_string()
        }
        DataAclRefusal::NotADirectory => "フォルダではありません。書き直さずに止めます".to_string(),
        DataAclRefusal::ForeignOwner(owner) => format!(
            "所有者（{owner}）が SYSTEM・Administrators・インストール利用者のどれでもありません。乗っ取りの恐れがあるため、書き直さずに止めます（中身と所有者を確かめ、必要なら管理者が所有権を取り直してからもう一度実行してください）"
        ),
    };
    format!("{}（{}）は{reason}", kind.label(), dir.display())
}

fn grant_data_acl<O: ElevOps>(ops: &O, out: Out<'_>) -> i32 {
    let sid = match recorded_install_user(ops, out) {
        Ok(sid) => sid,
        Err(code) => return code,
    };
    let root = match machine_root(ops, out) {
        Ok(root) => root,
        Err(code) => return code,
    };
    let elevated_user = ops.current_user_sid().ok();
    let elevated_user = elevated_user.as_ref().map(InstallUserSid::as_str);
    let layout = DataLayout::new(root);
    // 開いたフォルダは最後まで持つ（ルート → secrets → hub-keys の順）。
    let mut held = Vec::new();
    for (kind, dir, create) in [
        (DataDir::Root, layout.root().to_path_buf(), true),
        (DataDir::Secrets, layout.secrets_dir(), true),
        (DataDir::HubKeys, layout.hub_keys_dir(), false),
    ] {
        match grant_dir(ops, kind, &dir, &sid, elevated_user, create, out) {
            Ok(pinned) => held.extend(pinned),
            Err(code) => return code,
        }
    }
    drop(held);
    EXIT_OK
}

fn autostart<O: ElevOps>(ops: &O, enabled: bool, out: Out<'_>) -> i32 {
    let executable = match server_executable(ops, out) {
        Ok(executable) => executable,
        Err(code) => return code,
    };
    // 登録の組み立て直しは実行ファイルを隣の exe に書き換えるので、登録されている exe と
    // 同じときだけ行う（監査 B3。別の場所の elev から登録の exe を差し替えさせない）。
    match ops.registered_command_line() {
        Ok(Some(raw)) if registered_command_matches(&raw, &executable) => {}
        Ok(Some(raw)) => {
            out(err(&format!(
                "登録されている実行ファイル（{raw}）と、この exe の隣の {}（\"{}\" {}）が違います。変えずに止めます（service-uninstall → service-install で登録し直してください）",
                SERVER_EXE_FILE_NAME,
                executable.display(),
                chronogazer_core::shell_launch::RUN_SERVICE_ARG
            )));
            return EXIT_FAILED;
        }
        Ok(None) => {
            out(err(&format!(
                "サービス '{SERVICE_NAME}' は登録されていません（先に service-install を実行してください）"
            )));
            return EXIT_PRECONDITION;
        }
        Err(error) => {
            out(err(&format!(
                "サービス '{SERVICE_NAME}' の登録を読めませんでした: {error}"
            )));
            return EXIT_FAILED;
        }
    }
    match ops.set_autostart(&executable, enabled) {
        Ok(()) => {
            out(out_line(&if enabled {
                format!("サービス '{SERVICE_NAME}' を自動開始（遅延）にしました（OS の起動の後に始まります）")
            } else {
                format!("サービス '{SERVICE_NAME}' を手動開始に戻しました（OS の再起動だけでは始まりません）")
            }));
            EXIT_OK
        }
        Err(error @ ServiceManagerError::NotFound(_)) => {
            out(err(&format!(
                "{error}（先に service-install を実行してください）"
            )));
            EXIT_PRECONDITION
        }
        Err(error) => {
            out(err(&error.to_string()));
            EXIT_FAILED
        }
    }
}

#[cfg(test)]
mod tests;
