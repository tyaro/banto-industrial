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
//!   `banto-safe-root`、サービスの登録は `chronogazer_core::server_service::install_with`
//!   （`chronogazer-server.exe install` と同じ手順をプロセスの中で呼ぶ）。ここが持つのは
//!   アクションの一覧・引数の検証・手順の順序・文言・終了コードだけ。書き直す前の
//!   製品に依らない確かめ（既知のフォルダ・マシンの環境からのルート・本当のパスの比較・
//!   所有者の確かめ）の判断は `banto_safe_root::guard`（#626 PR-A で移した。
//!   banto-hub-elev と共有）で、ここは ChronoGazer の名前（`CHRONOGAZER_ROOT`・
//!   `ChronoGazer`）を渡して、型の付いた理由を文言にするだけ。
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
//! - **データの ACL**（`grant-data-acl`、O2。#623 PR-3 で**設定モードへの修復**に位置づけを
//!   変えた - 設計 §3.5）: ルート → `secrets\` → `secrets\hub-keys\` の順に、
//!   [`data_acl_plan`] のとおり開く・作る・断るを**先に全部済ませ**、それから
//!   `PinnedDir::relax_tree`（`banto-safe-root`。設定モードの形 = SYSTEM・Administrators に
//!   フル、インストール利用者に「変更」、所有者 Administrators、保護された DACL）を
//!   **ルートの木の全体に 1 回**書く。書く順序は**下から、ルートを最後**（サービスの
//!   「設定モードへ」と同じ `relax_tree`。「ルートが緩んでいる ⇔ 全体が緩み終わった」を
//!   木を書くすべての書き手で揃える）。印のファイル（`.banto-safe-root`）は印の記述子の
//!   まま。会った reparse point・ハードリンク・開けない子は飛ばして 1 行ずつ出す。
//!   既にあるフォルダは `banto_safe_root::acl::pin_dir` で**辿らずに**開き（持っている間は
//!   名前の変更・削除ができない）、reparse point か、所有者が SYSTEM・Administrators・
//!   インストール利用者・この昇格した利用者のどれでもなければ**書き直さずに止める**。
//!   **稼働中は断る**: 書く前に A1 の mutex（`Global\ChronoGazer.<scope>`）を取りにいき
//!   （`banto_instance_lock::try_hold_named_mutex`。診断ファイルは書かない）、サービスか
//!   デスクトップのエンジンが持っていれば何も変えずに 1。取れたら**書き終わるまで持つ**
//!   （確かめてから書くまでの間にサービスが起動しないように）。elev は木を**締めない**
//!   （締めるのはサービスだけ）。Users・Operators には何も付けない。
//! - **無いフォルダは、確かめ済みの親のハンドルを基準に相対で作る**（#642。#626 PR-B の
//!   banto-hub-elev と同じ形）: ルートは、親を辿らずに開いて
//!   `guard::check_root_parent`（本当のパスの一致・reparse point でないフォルダ・
//!   ネットワークでない・OS とプログラムの置き場の中でない）を通してから、
//!   `PinnedDir::create_child_dir`（`NtCreateFile` の `RootDirectory`）で作る。親が無ければ
//!   **何も作らずに** [`EXIT_PRECONDITION`]（親から作ることはしない）。`secrets\` は確かめ済みの
//!   ルートのハンドルを基準に作る。作る DACL は SYSTEM・Administrators だけ
//!   （`DirAccess::service_only`）で、作ったハンドルをそのまま使い（パスで開き直さない）、
//!   本当のパスと [`data_acl_plan`] を確かめた後に reset で利用者の「変更」を付ける。作ろう
//!   としたら既にあったものは、親のハンドルから相対で辿らずに開き直す
//!   （`PinnedDir::open_child_dir`）。以前（パスの `create_dir_if_absent`）は、途中の要素が
//!   ジャンクションだと外にフォルダを作ってから断っていた。
//! - **自動開始**: 登録の直後は手動（T17 P4、§3.3）。`autostart-enable` で自動（遅延）、
//!   `autostart-disable` で手動に戻す（`WindowsServiceManager::set_auto_start`。登録を
//!   隣の `chronogazer-server.exe` で組み立て直す）。
//!
//! ## 終了コード
//!
//! [`EXIT_OK`] 0 = 完了、[`EXIT_FAILED`] 1 = 操作の失敗（断った場合を含む）、
//! [`EXIT_PRECONDITION`] 2 = 前提が無い（サービス未登録・インストール利用者の記録が
//! 無い・`chronogazer-server.exe` が隣に無い・ルートを決められない・無いルートの親の
//! フォルダが無い（#642）・グループが無い）、
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

use banto_safe_root::acl::PinnedDirInfo;
use banto_safe_root::guard;
use banto_safe_root::mode::{RelaxReport, TreeProblem};
use banto_safe_root::{DirAccess, InstallUserSid, ModeAccess};
use banto_service_control::{ConsoleLine, ServiceManagerError, UninstallError, UninstallOutcome};
use banto_service_elev::{
    ElevError, GroupOutcome, MemberOutcome, OperatorsGroup, ServiceAclOutcome,
};
use chronogazer_core::paths::{DataLayout, PRODUCT_DIR_NAME, ROOT_ENV};
use chronogazer_core::server_service::{
    install_result_lines, plan_sid_record, uninstall_lines, InstallReport, InstallUserUnavailable,
    SidRecordPlan, INSTALL_USER_SID_LOCATION, OPERATORS_GROUP_NAME,
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
    name: OPERATORS_GROUP_NAME,
    comment:
        "ChronoGazer 記録エンジン（Windows サービス ChronoGazer）の開始・停止を委任された利用者",
};

/// 登録する実行ファイルの名前（この exe の隣にあること）。
pub const SERVER_EXE_FILE_NAME: &str = "chronogazer-server.exe";

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
//
// 判断（既知のフォルダ・マシンの環境からのルート・本当のパスの比較）は
// `banto_safe_root::guard`（#626 PR-A で移した。banto-hub-elev と共有）。ここに
// 残すのは ChronoGazer の名前（`CHRONOGAZER_ROOT`・`ChronoGazer`）と文言だけ。

/// マシンの環境の読み取り・既知のフォルダ（`banto_safe_root::guard` から）。
pub use banto_safe_root::guard::{KnownFolders, MachineEnvironment, MACHINE_ENVIRONMENT_KEY};

/// ルートが既知のシステムフォルダ・ドライブのルート・ネットワークの場所でないことを
/// 確かめる（純関数。監査 B1）。`grant-data-acl` の書き直しはルートの下の**全体**に
/// 伝わるので、`D:\`・`C:\ProgramData`・`C:\Users` などをルートにすると、他の製品・
/// 利用者のデータの ACL を書き換えてしまう。判断は
/// `banto_safe_root::guard::reject_well_known_root`、ここは文言だけ。
pub fn reject_well_known_root(root: &Path, known: &KnownFolders) -> Result<(), String> {
    guard::reject_well_known_root(root, known).map_err(|refusal| well_known_line(root, &refusal))
}

/// [`guard::WellKnownRootRefusal`] の文言（純関数）。
fn well_known_line(root: &Path, refusal: &guard::WellKnownRootRefusal) -> String {
    match refusal {
        guard::WellKnownRootRefusal::NetworkOrSpecial => format!(
            "データのルート（{}）はネットワークの場所か特殊なパス（\\\\…）です。ローカルのドライブのフォルダにしてください",
            root.display()
        ),
        guard::WellKnownRootRefusal::DriveRoot => format!(
            "データのルート（{}）はドライブのルートです。専用のフォルダ（例: D:\\ChronoGazer）にしてください",
            root.display()
        ),
        guard::WellKnownRootRefusal::OverlapsProtected(folder) => format!(
            "データのルート（{}）は OS・プログラムの置き場か共有のフォルダ（{}）に重なります。専用のフォルダにしてください",
            root.display(),
            folder.display()
        ),
        guard::WellKnownRootRefusal::ContainsProtected(folder) => format!(
            "データのルート（{}）は {} そのものかその上のフォルダです。その下の専用のフォルダにしてください",
            root.display(),
            folder.display()
        ),
    }
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
///
/// 判断は `banto_safe_root::guard::resolve_machine_root`、ここは名前と文言だけ。
pub fn resolve_machine_root(env: &impl MachineEnvironment) -> Result<PathBuf, String> {
    use guard::MachineRootError as E;
    guard::resolve_machine_root(env, ROOT_ENV, PRODUCT_DIR_NAME).map_err(|error| match error {
        E::VariableUnreadable(error) => {
            format!("マシンの環境変数 {ROOT_ENV} を読めませんでした: {error}")
        }
        E::NeedsExpansion(value) => format!(
            "マシンの環境変数 {ROOT_ENV}（{value:?}）は展開が要る値です。展開しない絶対パス（例: D:\\ChronoGazer）にしてください"
        ),
        E::ProgramDataUnavailable(error) => {
            format!("ProgramData の場所を取れませんでした: {error}")
        }
        // 形の誤りの文言は `chronogazer-server` と同じ（`server_host::validate_root`）。
        E::InvalidForm { root, .. } => chronogazer_core::server_host::validate_root(&root)
            .err()
            .unwrap_or_else(|| format!("データのルート（{}）が使えません", root.display())),
        E::KnownFoldersUnavailable(error) => {
            format!("既知のフォルダの場所を取れませんでした: {error}")
        }
        E::WellKnown { root, refusal } => well_known_line(&root, &refusal),
    })
}

/// `exe` が Program Files（`program_files_dirs` のどれか。x86 を含む）の**中**にあるか
/// （純関数。要素の境界で比べ、大文字小文字は無視。監査 B2）。場所が 1 つも分からなければ
/// `false`（止める側）。
pub fn is_inside_program_files(exe: &Path, program_files_dirs: &[PathBuf]) -> bool {
    program_files_dirs
        .iter()
        .any(|dir| guard::is_same_or_inside(exe, dir) && !guard::paths_match(exe, dir))
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
//
// 判断（[`data_acl_plan`]）は `banto_safe_root::guard`（#626 PR-A で移した）。

/// 扱うフォルダの状態・すること・断る理由と、その判断（純関数）。
pub use banto_safe_root::guard::{data_acl_plan, DataAclRefusal, DataAclStep, DirState};

// --- OS の操作 ---------------------------------------------------------------------

/// 辿らずに開いたフォルダ（Windows は `banto_safe_root::acl::PinnedDir`）。
/// 持っている間は名前の変更・削除ができない。
pub trait PinnedDirOps {
    /// 開いたハンドルから読んだ属性と所有者。
    fn info(&self) -> PinnedDirInfo;
    /// `GetFinalPathNameByHandleW` で解決した本当のパス（途中の要素のジャンクション・
    /// 8.3 の短い名前を辿った後。監査 Must 1）。
    fn final_path(&self) -> PathBuf;
    /// 設定モードの形（`mode.setting()`）を、**下から順にルートを最後に**木の全体へ書く
    /// （`PinnedDir::relax_tree`。#623 PR-3、設計 §3.5）。飛ばした物（reparse point・
    /// ハードリンク・開けない・使用中）を返す。
    fn relax_tree(&self, mode: &ModeAccess) -> Result<RelaxReport, String>;
}

/// 利用者のアカウントの確かめの失敗（監査 Nit 2）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountCheck {
    /// この PC にその SID のアカウントが無い（`ERROR_NONE_MAPPED`）・グループなど。使い方の
    /// 誤り（`--install-user-sid` なら 64）。
    NotAnAccount(String),
    /// 一時的な失敗（RPC 不達・アクセス拒否など）。もう一度試せば変わりうる（1）。
    Transient(String),
}

impl AccountCheck {
    fn message(&self) -> &str {
        match self {
            Self::NotAnAccount(m) | Self::Transient(m) => m,
        }
    }
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
    /// ルートにしてはいけない既知のフォルダ（[`reject_well_known_root`]。本当のパスにも
    /// かけるため。監査 Must 1）。
    fn known_folders(&self) -> Result<KnownFolders, String>;
    /// このプロセスのトークンの利用者。
    fn current_user_sid(&self) -> Result<InstallUserSid, String>;
    /// この PC の利用者のアカウント（`SidTypeUser`）か。アカウントでない（`ERROR_NONE_MAPPED`・
    /// グループなど）と一時的な失敗（RPC 不達など）を分ける（監査 Nit 2）。
    fn check_user_account(&self, sid: &InstallUserSid) -> Result<(), AccountCheck>;
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
    fn grant_service_acl(&self) -> Result<ServiceAclOutcome, ElevError>;
    /// 自動開始（遅延）・手動開始。
    fn set_autostart(&self, executable: &Path, enabled: bool) -> Result<(), ServiceManagerError>;
    /// フォルダを辿らずに開く（無ければ `Ok(None)`）。
    fn pin_dir(&self, dir: &Path) -> Result<Option<Self::Pinned>, String>;
    /// 確かめ済みの `parent`（開いたハンドル）を基準に、名前 1 要素の子のフォルダを
    /// **相対で**、`access` の保護された DACL で作り、作ったハンドルをそのまま辿らずに
    /// 開いたものとして返す（パスを解決し直さない。`PinnedDir::create_child_dir`。#642）。
    /// 既にあれば `Ok(None)`。
    fn create_child(
        &self,
        parent: &Self::Pinned,
        name: &str,
        access: &DirAccess,
    ) -> Result<Option<Self::Pinned>, String>;
    /// 確かめ済みの `parent` を基準に、名前 1 要素の子を**相対で辿らずに**開く
    /// （`PinnedDir::open_child_dir`）。無ければ `Ok(None)`。[`ElevOps::create_child`] が
    /// 既にあった（`Ok(None)`）フォルダを、パスで開き直さずに開くために使う（#642）。
    fn open_child(&self, parent: &Self::Pinned, name: &str)
        -> Result<Option<Self::Pinned>, String>;
    /// ルート `root`（本当のパス。既にあること）の DB の A1 の mutex を、**診断ファイルを
    /// 書かずに**取る（`chronogazer_core::instance_lock::mutex_name_for_root`（ルートの
    /// 本当のパス + DB の名前。末端は解かない）→ `banto_instance_lock::try_hold_named_mutex`。
    /// #623 PR-3、設計 §3.5）。取れれば
    /// `Some(持ち物)`（drop まで持つ）、サービス・デスクトップのエンジンが持っていれば
    /// `None`（elev は何も書かずに断る）。
    fn hold_instance_mutex(&self, root: &Path) -> Result<Option<Box<dyn std::any::Any>>, String>;
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

/// 開いたフォルダの**本当のパス**（`final_path`）が、期待のパスと同じ場所を指し、既知の
/// システムフォルダに重ならないことを確かめる（監査 Must 1）。途中の要素のジャンクション・
/// 8.3 の短い名前で別の場所を開かされていたら、ここで断る（1）。
fn verify_final_path<P: PinnedDirOps>(
    pinned: &P,
    kind: DataDir,
    expected: &Path,
    known: &KnownFolders,
    out: Out<'_>,
) -> Result<(), i32> {
    let real = pinned.final_path();
    // 判断は `guard::check_real_path`（#626 PR-A）。ここは文言だけ。
    match guard::check_real_path(&real, expected, known) {
        Ok(()) => Ok(()),
        Err(guard::RealPathRefusal::Mismatch) => {
            out(err(&format!(
                "{}（{}）の本当の場所（{}）が期待と違います。途中のフォルダがジャンクションか短い名前の可能性があります。書き直さずに止めます",
                kind.label(),
                expected.display(),
                real.display()
            )));
            Err(EXIT_FAILED)
        }
        Err(guard::RealPathRefusal::WellKnown(refusal)) => {
            out(err(&format!(
                "{}（{}）の本当の場所が使えません: {}。書き直さずに止めます",
                kind.label(),
                real.display(),
                well_known_line(&real, &refusal)
            )));
            Err(EXIT_FAILED)
        }
    }
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
            "記録されたインストール利用者（{}）はこの PC の利用者のアカウントとして確かめられません: {}。service-uninstall → service-install で登録し直してください",
            sid.as_str(),
            error.message()
        )));
        // 記録が壊れている（アカウントでない）のも、一時的に確かめられないのも、ここでは
        // 先へ進めない前提の欠落として扱う（2）。
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
            // 監査 Nit 2: アカウントでない（`--install-user-sid` の値の誤り）なら 64、
            // 一時的な失敗（RPC 不達など）なら 1。どちらも何も登録しない。
            if let Err(error) = ops.check_user_account(&sid) {
                let code = match error {
                    AccountCheck::NotAnAccount(_) => EXIT_USAGE,
                    AccountCheck::Transient(_) => EXIT_FAILED,
                };
                out(err(&format!(
                    "{INSTALL_USER_SID_FLAG} の SID（{}）をこの PC の利用者のアカウントとして確かめられません: {}（何も登録していません）",
                    sid.as_str(),
                    error.message()
                )));
                return code;
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
                    "実行した利用者（{}）をこの PC の利用者のアカウントとして確かめられません: {}。{INSTALL_USER_SID_FLAG} S-1-5-21-… で指定してください（何も登録していません）",
                    sid.as_str(),
                    error.message()
                )));
                return EXIT_PRECONDITION;
            }
            sid
        }
    };

    // 0. ルートを登録の**前に**用意して確かめる（監査 B4）: 無ければ elev が保護された
    //    DACL で作り、辿らずに開いて、reparse point・フォルダでない・見知らぬ所有者なら
    //    何も登録せずに止める（ほかの利用者が先に作ったルートにサービスを向けない）。
    //    開いたルートは最後の手順まで持つ。
    let elevated_user = ops.current_user_sid().ok();
    // ルートの ACL の利用者は、登録の後に記録に残る SID（D2: 違う SID が既に記録されて
    // いれば、`install_with` は記録を変えない）。今の記録を読めなければ渡す SID。
    let root_user = match ops.read_install_user() {
        Ok(existing) => match plan_sid_record(existing.as_deref(), &sid) {
            SidRecordPlan::KeepDifferent { recorded } => {
                // 監査 Nit 4: 違う SID が既に記録済み（D2）なら、ルートは記録の SID で整える
                // ことを 1 行で知らせる（登録の行の前に出す）。
                out(out_line(&format!(
                    "インストール利用者は記録済みの {} を使います（指定の {} ではなく。変えるには service-uninstall → service-install）",
                    recorded.as_str(),
                    sid.as_str()
                )));
                recorded
            }
            _ => sid.clone(),
        },
        Err(_) => sid.clone(),
    };
    let known = match ops.known_folders() {
        Ok(known) => known,
        Err(error) => {
            out(err(&format!(
                "既知のフォルダの場所を取れませんでした: {error}（何も登録していません）"
            )));
            return EXIT_PRECONDITION;
        }
    };
    let _held_root = match preflight_root(
        ops,
        &root,
        &known,
        &root_user,
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

/// 段の親（無いフォルダを作るときの基準。#642）。
enum Parent<'a, P> {
    /// ルート: 親はパスで辿らずに開いて [`guard::check_root_parent`] で確かめる（無ければ
    /// 何も作らずに断る）。開いた親は `slot` に入れて、呼ぶ側が最後まで持つ。
    RootParent(&'a mut Option<P>),
    /// `secrets\`・`secrets\hub-keys\`: 確かめ済みの 1 つ上のフォルダのハンドル。
    Pinned(&'a P),
}

/// [`guard::RootParentRefusal`] の文言（純関数）。
fn root_parent_text(real: &Path, refusal: &guard::RootParentRefusal) -> String {
    match refusal {
        guard::RootParentRefusal::Mismatch => format!(
            "本当の場所（{}）が期待と違います。途中のフォルダがジャンクションか短い名前の可能性があります",
            real.display()
        ),
        guard::RootParentRefusal::NotAPlainDirectory => {
            "ジャンクション・シンボリックリンク（reparse point）か、フォルダではありません"
                .to_string()
        }
        guard::RootParentRefusal::NetworkOrSpecial => {
            "ネットワークの場所か特殊なパスです".to_string()
        }
        guard::RootParentRefusal::InsideProtected(folder) => format!(
            "OS・プログラムの置き場か共有のフォルダ（{}）の中です",
            folder.display()
        ),
    }
}

/// [`create_in_parent`] の作る・開き直すの失敗（どちらの操作で落ちたかを文言に使う。
/// banto-hub-elev の `MadeError` と同じ形。#628 / #646 Nit 1）。
enum MadeError {
    /// 作ろうとして失敗した。
    Create(String),
    /// 作ろうとしたら既にあり、開き直して失敗した。
    Open(String),
}

/// 無いフォルダを、**確かめ済みの親のハンドルを基準に相対で**作る（#642。#626 PR-B の
/// banto-hub-elev と同じ形）。作るときの DACL は SYSTEM・Administrators だけ
/// （[`DirAccess::service_only`]。利用者の「変更」は、本当のパスを確かめた後に呼ぶ側が
/// reset で付ける）。パスを解決し直さないので、確かめた後に親の祖先や親そのものを
/// ジャンクションにすり替えられても、外にフォルダはできない。
///
/// - ルート（[`Parent::RootParent`]）: 親を**パスで辿らずに開き**、
///   [`guard::check_root_parent`]（本当のパスの一致・reparse point でないフォルダ・
///   ネットワークでない・OS とプログラムの置き場の中でない）を通してから作る。親が
///   無ければ**何も作らずに** [`EXIT_PRECONDITION`]（前提が無い。親から作ることはしない）、
///   開けない・確かめが通らなければ [`EXIT_FAILED`]。
/// - 作ろうとしたら既にあった（確かめてから作るまでの間に誰かが作った）なら、パスでは
///   なく親のハンドルから相対で辿らずに開き直す（呼ぶ側が本当のパス・属性・所有者で
///   確かめる）。
///
/// 返すのは開いたフォルダと、自分で作ったか。
fn create_in_parent<O: ElevOps>(
    ops: &O,
    kind: DataDir,
    dir: &Path,
    parent: Parent<'_, O::Pinned>,
    known: &KnownFolders,
    out: Out<'_>,
) -> Result<(O::Pinned, bool), i32> {
    let refuse = |out: Out<'_>, text: String| {
        out(err(&text));
        Err(EXIT_FAILED)
    };
    let Some(name) = dir.file_name().and_then(|name| name.to_str()) else {
        return refuse(
            out,
            format!("{}（{}）の名前が分かりません", kind.label(), dir.display()),
        );
    };
    let service_only = DirAccess::service_only();
    let create_or_open = |parent: &O::Pinned| match ops.create_child(parent, name, &service_only) {
        Ok(Some(pinned)) => Ok(Some((pinned, true))),
        Ok(None) => ops
            .open_child(parent, name)
            .map(|found| found.map(|pinned| (pinned, false)))
            .map_err(MadeError::Open),
        Err(error) => Err(MadeError::Create(error)),
    };
    let made = match parent {
        Parent::Pinned(parent) => create_or_open(parent),
        Parent::RootParent(slot) => {
            let Some(parent_dir) = dir.parent() else {
                return refuse(
                    out,
                    format!("{}（{}）の親が分かりません", kind.label(), dir.display()),
                );
            };
            let parent_pinned = match ops.pin_dir(parent_dir) {
                Ok(Some(pinned)) => pinned,
                Ok(None) => {
                    out(err(&format!(
                        "{}（{}）の親（{}）がありません。親から作ることはしません（親のフォルダを作ってから、もう一度実行してください）",
                        kind.label(),
                        dir.display(),
                        parent_dir.display()
                    )));
                    return Err(EXIT_PRECONDITION);
                }
                Err(error) => {
                    return refuse(
                        out,
                        format!(
                            "{}（{}）の親（{}）を開けませんでした: {error}",
                            kind.label(),
                            dir.display(),
                            parent_dir.display()
                        ),
                    )
                }
            };
            let real = parent_pinned.final_path();
            if let Err(refusal) =
                guard::check_root_parent(&real, parent_dir, &parent_pinned.info(), known)
            {
                return refuse(
                    out,
                    format!(
                        "{}（{}）の親（{}）は{}。何も作らずに止めます",
                        kind.label(),
                        dir.display(),
                        parent_dir.display(),
                        root_parent_text(&real, &refusal)
                    ),
                );
            }
            let made = create_or_open(&parent_pinned);
            *slot = Some(parent_pinned);
            made
        }
    };
    match made {
        Ok(Some(found)) => Ok(found),
        Ok(None) => refuse(
            out,
            format!(
                "{}（{}）を作ろうとしたら既にあり、開き直すと無くなっていました。止めます",
                kind.label(),
                dir.display()
            ),
        ),
        Err(MadeError::Create(error)) => refuse(
            out,
            format!(
                "{}（{}）を作れませんでした: {error}",
                kind.label(),
                dir.display()
            ),
        ),
        Err(MadeError::Open(error)) => refuse(
            out,
            format!(
                "{}（{}）を開けませんでした: {error}",
                kind.label(),
                dir.display()
            ),
        ),
    }
}

/// `service-install` の前の、ルートの用意と確かめ（監査 B4）。**無ければ elev 自身が先に
/// 作る**（[`create_in_parent`]: 確かめ済みの親のハンドルを基準に、SYSTEM・Administrators
/// だけの保護された DACL で。#642）。確かめてから登録の手順が作るまでの間に、ほかの
/// 利用者が先に作る隙間を残さないため。そのうえで本当のパスと [`data_acl_plan`] で断る
/// 理由があれば、何も登録せずに 1（親が無ければ 2）。開いたルート（と、作ったときは
/// ルートの親）は呼ぶ側が最後の手順まで持つ。
fn preflight_root<O: ElevOps>(
    ops: &O,
    root: &Path,
    known: &KnownFolders,
    install_user: &InstallUserSid,
    elevated_user: Option<&str>,
    out: Out<'_>,
) -> Result<(O::Pinned, Option<O::Pinned>), i32> {
    let mut root_parent = None;
    let (pinned, created) = match ops.pin_dir(root) {
        Ok(Some(pinned)) => (pinned, false),
        Ok(None) => match create_in_parent(
            ops,
            DataDir::Root,
            root,
            Parent::RootParent(&mut root_parent),
            known,
            out,
        ) {
            Ok(found) => found,
            Err(code) => {
                out(err("（何も登録していません）"));
                return Err(code);
            }
        },
        Err(error) => {
            out(err(&format!(
                "{}（{}）を開けませんでした: {error}（何も登録していません）",
                DataDir::Root.label(),
                root.display()
            )));
            return Err(EXIT_FAILED);
        }
    };
    // 本当のパスが期待（root）と同じ場所で、既知のシステムフォルダに重ならないこと
    // （監査 Must 1。途中の要素のジャンクション・8.3 の短い名前を塞ぐ）。何も登録しない。
    if let Err(code) = verify_final_path(&pinned, DataDir::Root, root, known, out) {
        left_behind_notice(created, DataDir::Root, root, &pinned.final_path(), out);
        out(err("（何も登録していません）"));
        return Err(code);
    }
    if let Err(refusal) = data_acl_plan(
        &DirState::Present(pinned.info()),
        install_user,
        elevated_user,
    ) {
        out(err(&refusal_line(DataDir::Root, root, &refusal)));
        left_behind_notice(created, DataDir::Root, root, &pinned.final_path(), out);
        out(err("（何も登録していません）"));
        return Err(EXIT_FAILED);
    }
    // 確かめが通ったら、作ったルートに設定モードの形（利用者の「変更」、所有者
    // Administrators。監査 B5）を書く（`relax_tree`。#623 PR-3）。既にあったルートはここでは
    // 変えない（登録の後の `grant-data-acl` が書き直す）。書く前に A1 を確かめる（作った
    // ばかりのルートを持つ者はいないはずだが、書き手は全部同じ約束を通す）。
    if created {
        let _held = match hold_instance_mutex(ops, &pinned.final_path(), out) {
            Ok(held) => held,
            Err(code) => {
                out(err("（何も登録していません）"));
                return Err(code);
            }
        };
        match pinned.relax_tree(&mode_for(install_user)) {
            Ok(report) => emit_skips(DataDir::Root, &report.skipped, out),
            Err(error) => {
                out(err(&format!(
                    "{}（{}）の所有者と ACL を整えられませんでした: {error}（何も登録していません）",
                    DataDir::Root.label(),
                    root.display()
                )));
                return Err(EXIT_FAILED);
            }
        }
    }
    Ok((pinned, root_parent))
}

/// 設定モード ⇔ 運用モードの 2 つの形（elev が書くのは設定モードの側だけ）。
fn mode_for(install_user: &InstallUserSid) -> ModeAccess {
    ModeAccess::for_users([install_user.clone()]).expect("利用者が 1 人いる")
}

/// 書く前の A1 の確かめ（設計 §3.5）: サービスかデスクトップのエンジンが動いていれば
/// 何も書かずに 1。取れたら、呼ぶ側が書き終わるまで持つ。
fn hold_instance_mutex<O: ElevOps>(
    ops: &O,
    real_root: &Path,
    out: Out<'_>,
) -> Result<Box<dyn std::any::Any>, i32> {
    match ops.hold_instance_mutex(real_root) {
        Ok(Some(held)) => Ok(held),
        Ok(None) => {
            out(err(&format!(
                "データのルート（{}）は使用中です（ChronoGazer のサービスか、デスクトップの記録エンジンが動いています）。ACL を書き直さずに止めます（止めてから、もう一度実行してください）",
                real_root.display()
            )));
            Err(EXIT_FAILED)
        }
        Err(error) => {
            out(err(&format!(
                "データのルート（{}）の使用中の確かめに失敗しました: {error}。ACL を書き直さずに止めます",
                real_root.display()
            )));
            Err(EXIT_FAILED)
        }
    }
}

/// 断ったとき、**自分で作った**（SYSTEM・Administrators だけの）フォルダが残っていることを
/// 伝える 1 行（監査 Must。利用者の「変更」は付いていない）。#642 で作るのは確かめ済みの
/// 親のハンドルを基準にした相対だけになったので、外には残らない。作った直後の確かめ
/// （本当のパス・[`data_acl_plan`]）で断ったときにだけ出る（banto-hub-elev の
/// `Created::notice` と同じ）。
///
/// 出すのは**開いたフォルダの本当の場所**（`real`。`GetFinalPathNameByHandleW`）。期待の
/// パス（`expected`）と違うなら（途中がジャンクションなど）、期待のパスも添える。残って
/// いるのは本当の場所の方なので、利用者がそこを探せるようにする（#628 の 4）。
fn left_behind_notice(created: bool, kind: DataDir, expected: &Path, real: &Path, out: Out<'_>) {
    if created {
        let place = if guard::paths_match(real, expected) {
            real.display().to_string()
        } else {
            format!("{}。期待のパスは {}", real.display(), expected.display())
        };
        out(err(&format!(
            "{}（{place}）を作りましたが、確かめが通らなかったので残っています（SYSTEM・Administrators だけが触れる DACL）",
            kind.label()
        )));
    }
}

/// 書き直しのときに飛ばした物（reparse point・ハードリンク・開けない・使用中）を警告の
/// 1 行ずつで報告する（監査 Must 2・Should 1。触っていないことを利用者に知らせる。飛ばした
/// 物は今の形のまま残る = 運用モードの形なら締まったまま）。
fn emit_skips(kind: DataDir, skipped: &[TreeProblem], out: Out<'_>) {
    for problem in skipped {
        let why = match problem {
            TreeProblem::ReparsePoint { .. } => {
                "ジャンクションかシンボリックリンクなので、辿らず・触らず飛ばしました"
            }
            TreeProblem::Hardlink { .. } => {
                "ハードリンク（ほかの名前と中身を共有）なので、触らず飛ばしました（#624）"
            }
            TreeProblem::Denied { .. } => {
                "開けなかったので、触らず飛ばしました（所有者が Administrators を拒否しているなど。そのまま残ります。監査 Should 1）"
            }
            TreeProblem::Busy { .. } => "使用中で開けなかったので、触らず飛ばしました（そのまま残ります）",
            TreeProblem::Encrypted { .. }
            | TreeProblem::TooDeep { .. }
            | TreeProblem::MarkerOccupied { .. } => "触らず飛ばしました（そのまま残ります）",
        };
        out(err(&format!("{}: 中の {problem}。{why}", kind.label())));
    }
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
        // 未登録だけが前提不足（先に service-install をすれば解除するものができる）。開けなかった
        // （権限不足・SCM の不調など。`Open`）は service-install を先にしても直らないので失敗。
        Err(UninstallError::NotInstalled) => EXIT_PRECONDITION,
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

/// `ERROR_SERVICE_DOES_NOT_EXIST`（定義は `banto_service_control` の 1 か所。`ElevError` の
/// `os_error` は u32）。
const ERROR_SERVICE_DOES_NOT_EXIST: u32 =
    banto_service_control::ERROR_SERVICE_DOES_NOT_EXIST as u32;

fn grant_service_acl<O: ElevOps>(ops: &O, out: Out<'_>) -> i32 {
    match ops.grant_service_acl() {
        Ok(outcome) => {
            out(out_line(&banto_service_elev::grant_service_acl_message(
                outcome,
                SERVICE_NAME,
                OPERATORS_GROUP.name,
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

/// 1 つのフォルダを用意する（**書かない**。#623 PR-3 で書く側を [`grant_data_acl`] の
/// `relax_tree` 1 回に寄せた）。あれば辿らずに開き、無ければ作る（`create_if_absent`。
/// [`create_in_parent`]: `parent` のハンドルを基準に相対で、#642）か何もしない。そのうえで
/// 本当のパスと [`data_acl_plan`] のとおり通すか止める。開いたフォルダと作ったかを返して、
/// 呼ぶ側が全部の手順が終わるまで持つ（下のフォルダを扱う間に名前を変えさせない）。
#[allow(clippy::too_many_arguments)]
fn prepare_dir<O: ElevOps>(
    ops: &O,
    kind: DataDir,
    dir: &Path,
    expected: &Path,
    parent: Parent<'_, O::Pinned>,
    known: &KnownFolders,
    install_user: &InstallUserSid,
    elevated_user: Option<&str>,
    create_if_absent: bool,
    out: Out<'_>,
) -> Result<Option<(O::Pinned, bool)>, i32> {
    let (pinned, created) = match ops.pin_dir(dir) {
        Ok(Some(pinned)) => (pinned, false),
        Ok(None) if !create_if_absent => {
            out(out_line(&format!(
                "{}（{}）はまだありません（エンジンが初めて使うときに作ります）",
                kind.label(),
                dir.display()
            )));
            return Ok(None);
        }
        // 監査 Must（オーナー案 a）: 作るときは SYSTEM・Administrators だけ。利用者の
        // 「変更」は、本当のパスを確かめた後の `relax_tree` で付ける。
        Ok(None) => create_in_parent(ops, kind, dir, parent, known, out)?,
        Err(error) => {
            out(err(&format!(
                "{}（{}）を開けませんでした: {error}",
                kind.label(),
                dir.display()
            )));
            return Err(EXIT_FAILED);
        }
    };
    // 本当のパスが期待と同じ場所で、既知のシステムフォルダに重ならないこと（監査
    // Must 1。途中の要素のジャンクション・8.3 の短い名前・ルートの祖先のすり替えを塞ぐ）。
    if let Err(code) = verify_final_path(&pinned, kind, expected, known, out) {
        left_behind_notice(created, kind, expected, &pinned.final_path(), out);
        return Err(code);
    }
    let info = pinned.info();
    if let Err(refusal) = data_acl_plan(&DirState::Present(info), install_user, elevated_user) {
        out(err(&refusal_line(kind, dir, &refusal)));
        left_behind_notice(created, kind, expected, &pinned.final_path(), out);
        return Err(EXIT_FAILED);
    }
    Ok(Some((pinned, created)))
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
    let known = match ops.known_folders() {
        Ok(known) => known,
        Err(error) => {
            out(err(&format!(
                "既知のフォルダの場所を取れませんでした: {error}"
            )));
            return EXIT_PRECONDITION;
        }
    };
    let elevated_user = ops.current_user_sid().ok();
    let elevated_user = elevated_user.as_ref().map(InstallUserSid::as_str);
    let layout = DataLayout::new(root);
    // 開いたフォルダは最後まで持つ（ルート → secrets → hub-keys の順）。ルートを開いた
    // ら、下の期待のパスは**ルートの本当のパス**から組み立てる（ルートの祖先がジャンク
    // ションでも、下を「本当のルート\secrets」と比べれば、別の場所を開かされていたら
    // 気づく。監査 Must 1）。無いフォルダは、確かめ済みの 1 つ上のハンドルを基準に作る
    // （ルートは、辿らずに開いて確かめたルートの親のハンドル。#642）。
    //
    // #623 PR-3: **確かめ・作るを全部済ませてから、書くのは `relax_tree` 1 回**（ルートの木の
    // 全体。下から順にルートを最後に）。書く前に A1 の mutex を取り、書き終わるまで持つ。
    let mut root_parent: Option<O::Pinned> = None;
    let (root_pinned, root_created) = match prepare_dir(
        ops,
        DataDir::Root,
        layout.root(),
        layout.root(),
        Parent::RootParent(&mut root_parent),
        &known,
        &sid,
        elevated_user,
        true,
        out,
    ) {
        Ok(Some(found)) => found,
        Ok(None) => unreachable!("ルートは作る"),
        Err(code) => return code,
    };
    let real_root = root_pinned.final_path();
    // A1（稼働中は断る）。取れたら `_held` が書き終わるまで持つ。
    let _held = match hold_instance_mutex(ops, &real_root, out) {
        Ok(held) => held,
        Err(code) => {
            left_behind_notice(root_created, DataDir::Root, layout.root(), &real_root, out);
            return code;
        }
    };
    let secrets = match prepare_dir(
        ops,
        DataDir::Secrets,
        &layout.secrets_dir(),
        &real_root.join("secrets"),
        Parent::Pinned(&root_pinned),
        &known,
        &sid,
        elevated_user,
        true,
        out,
    ) {
        Ok(Some(found)) => found,
        Ok(None) => unreachable!("secrets は作る"),
        Err(code) => return code,
    };
    let hub_keys = match prepare_dir(
        ops,
        DataDir::HubKeys,
        &layout.hub_keys_dir(),
        &real_root.join("secrets").join("hub-keys"),
        Parent::Pinned(&secrets.0),
        &known,
        &sid,
        elevated_user,
        false,
        out,
    ) {
        Ok(found) => found,
        Err(code) => return code,
    };
    // 書く（1 回。ルートの木の全体、下から順にルートを最後に）。
    let report = match root_pinned.relax_tree(&mode_for(&sid)) {
        Ok(report) => report,
        Err(error) => {
            out(err(&format!(
                "{}（{}）の ACL を書き直せませんでした: {error}",
                DataDir::Root.label(),
                layout.root().display()
            )));
            return EXIT_FAILED;
        }
    };
    let describe = |kind: DataDir, dir: &Path, created: bool| {
        let what = if created {
            "作りました"
        } else {
            "設定モードの形に書き直しました"
        };
        out_line(&format!(
            "{}: {}（{what}。SYSTEM・Administrators にフル、インストール利用者 {} に変更、所有者 Administrators、親から継承しない）",
            kind.label(),
            dir.display(),
            sid.as_str()
        ))
    };
    out(describe(DataDir::Root, layout.root(), root_created));
    out(describe(DataDir::Secrets, &layout.secrets_dir(), secrets.1));
    if let Some((_, created)) = &hub_keys {
        out(describe(DataDir::HubKeys, &layout.hub_keys_dir(), *created));
    }
    out(out_line(&format!(
        "{}: 下のフォルダ・ファイル {} 件を書き直しました（ルートは最後。印のファイルはそのまま）",
        DataDir::Root.label(),
        report.written
    )));
    emit_skips(DataDir::Root, &report.skipped, out);
    // 開いた順の逆に閉じる（hub-keys → secrets → ルート → ルートの親）。A1 はその後。
    drop(hub_keys);
    drop(secrets);
    drop(root_pinned);
    drop(root_parent);
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
