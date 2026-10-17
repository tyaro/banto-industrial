//! `chronogazer-server` を Windows サービスとして動かす・登録する部分の、OS に
//! 依存しない判断と文言（#392 B3-2b、2026-10-15。設計は
//! docs/chronogazer-service-design.md §3.3・§4 の B3-2a と B3-2b の実装メモ）。
//!
//! SCM とのやりとりは共有 crate `banto-service-control`（ランナー・登録と解除・
//! `Parameters` の読み書き）、サービス本体（`ServerProgram`）と登録の手順は bin
//! （`src/bin/chronogazer_server/service.rs`、Windows だけ）。ここには、どの OS でも
//! テストできる純関数だけを置く:
//!
//! - 終了コード（[`service_exit_for`]）: コンソールと同じ値を SCM の
//!   `ServiceSpecific(n)` に写す。0 = 正常（`Win32(0)`）、1 = 後始末が予算内に
//!   終わらなかった・最終 flush に失敗、2 = 同じデータを別のプロセスが使用中（A1）、
//!   3 = その他の起動の失敗。**1 は本体の panic と同じ値**（共有ランナーの
//!   `PANIC_EXIT_CODE`、B3-1 のオーナー決定 D2）で、`sc query` の
//!   `SERVICE_EXIT_CODE` だけでは見分けられない - panic はログの行で見分ける。
//! - 停止の予算（[`service_stop_budget`]）: SCM の STOP はコンソールの Ctrl-C と同じ
//!   [`INTERRUPT_BUDGET`]（15 秒）、OS の PRESHUTDOWN は `StopNotice::os_work_deadline`
//!   （受けてから 27 秒）までの残りから、後のログの書き出しの上限
//!   [`SERVICE_LOG_FLUSH_WAIT`]（2 秒）と Stopped の報告の余裕
//!   [`PRESHUTDOWN_REPORT_RESERVE`]（1 秒）を引いたもの（負なら 0）。書き出しの待ちも
//!   期限までの残りで頭打ちにする（[`service_flush_wait`]。PR #618 の Codex の指摘）。
//! - インストール利用者の SID（2026-10-15 オーナー決定 D1〜D4）: 登録のときに
//!   `HKLM\SYSTEM\CurrentControlSet\Services\ChronoGazer\Parameters\InstallUserSid`
//!   （[`INSTALL_USER_SID_VALUE`]）に記録し、サービスは起動のたびに読んで、データの
//!   ルートと Hub の API キーの保存先を作る・開くときの ACL の利用者の欄にする
//!   （[`access_from_parameter`]。LocalSystem のサービスには利用者がいないため）。
//!   記録が無い・不正・読めないときは `service_only` で起動を続け、理由と対処を
//!   ログに出す（D4）。登録し直したときの扱いは [`plan_sid_record`]（D2: 違う SID なら
//!   記録を変えずに警告）。
//! - `install` / `uninstall` の文言（[`install_lines`]・[`uninstall_lines`]。出し先の
//!   別と順序も含めて golden テスト）と終了コード（[`install_exit_code`]。失敗は 1）。
//!   SCM への接続の失敗には「管理者として実行した PowerShell から」の案内を足す
//!   （UAC で昇格する口は同じフォルダの `chronogazer-elev.exe`、B3-3b）。
//! - `install` の本体（`install_with`、Windows だけ。#392 B3-3b で bin から移した）:
//!   `chronogazer-server.exe install` と `chronogazer-elev.exe service-install` が
//!   同じ手順を使う。違うのは、実行ファイル・データのルート・Program Files の場所を
//!   どこから決めるかだけ（`chronogazer-server` はプロセスの環境、elev はマシンの
//!   環境。`apps/chronogazer/elev` の lib doc）。結果の行と終了コードは
//!   [`install_result_lines`]。記録と用意の手順（[`record_install_user`]・
//!   [`prepare_root`]）は OS に依らない形で置き、テストは偽の読み書き・一時
//!   ディレクトリで行う。
//! - サービスのログの行（起動・停止の理由・完了）。どれも「Windows サービス」
//!   「SCM の STOP」「PRESHUTDOWN」を名指しする（日次ログをコンソールの分と見分ける
//!   ため。banto-hub の `hub_stop_reason_line` と同じ作法）。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use banto_secret_store::{DirAccess, InstallUserSid};
use banto_service_control::{
    preshutdown_console_line, ConsoleLine, InstallError, InstallKind, InstallOutcome, ServiceExit,
    StopAttempt, StopNotice, StopSignal, UninstallError, UninstallOutcome,
};

use crate::paths::{create_root_dir_with, DataLayout};
use crate::server_host::{
    EXIT_ALREADY_RUNNING, EXIT_OK, EXIT_SHUTDOWN_INCOMPLETE, INTERRUPT_BUDGET, SERVER_LABEL,
};
use crate::shell_launch::{SERVICE_DISPLAY_NAME, SERVICE_NAME};

/// インストール利用者の SID を記録する値の名前（`Parameters` の下の REG_SZ）。
pub const INSTALL_USER_SID_VALUE: &str = "InstallUserSid";

/// 記録の場所（利用者に見せる文言用）。
pub const INSTALL_USER_SID_LOCATION: &str =
    r"HKLM\SYSTEM\CurrentControlSet\Services\ChronoGazer\Parameters\InstallUserSid";

/// SCM の STOP で、停止にかかる見込み（StopPending の heartbeat が checkpoint を進める
/// 上限。`ServiceContext::set_expected_stop`）。停止の予算 [`INTERRUPT_BUDGET`] に、
/// ログの書き出しの余裕 5 秒を足したもの。
pub const SERVICE_EXPECTED_STOP: Duration = Duration::from_secs(20);

/// OS の PRESHUTDOWN の予算から、Stopped の報告とログの書き出しのために残す分。
pub const PRESHUTDOWN_REPORT_RESERVE: Duration = Duration::from_secs(1);

/// SCM に接続できないときの案内（管理者権限が要る。UAC で昇格する口は同じフォルダの
/// `chronogazer-elev.exe`、#392 B3-3b）。
pub const ELEVATION_HINT: &str = "管理者として実行した PowerShell から実行するか、同じフォルダの chronogazer-elev.exe（UAC で昇格します）を使ってください";

// --- サービスの本体 -------------------------------------------------------------

/// `chronogazer-server` の終了コード（コンソールと同じ値）を、SCM に報告する
/// 終わり方に写す（モジュール doc）。
pub fn service_exit_for(code: i32) -> ServiceExit {
    match code {
        EXIT_OK => ServiceExit::Success,
        EXIT_SHUTDOWN_INCOMPLETE => ServiceExit::Specific(1),
        EXIT_ALREADY_RUNNING => ServiceExit::Specific(2),
        _ => ServiceExit::Specific(3),
    }
}

/// 後始末の後に、日次ログの書き出し（向け直しの終わり）を待つ上限。
pub const SERVICE_LOG_FLUSH_WAIT: Duration = Duration::from_secs(2);

/// 停止の予算（モジュール doc）。`now` は停止を始める時刻。**この後のログの書き出しの
/// 待ち（[`service_flush_wait`]、最大 [`SERVICE_LOG_FLUSH_WAIT`]）と Stopped の報告の
/// 余裕（[`PRESHUTDOWN_REPORT_RESERVE`]）を含めて**期限に収まるように引く（PR #618 の
/// Codex の指摘 P2）:
///
/// - STOP: [`INTERRUPT_BUDGET`]（15 秒）。書き出しと余裕を足しても heartbeat の見込み
///   [`SERVICE_EXPECTED_STOP`]（20 秒）に収まる（テストで固定）。
/// - PRESHUTDOWN: 期限 − `now` − 書き出しの上限 − 余裕（負なら 0）。受けた直後なら
///   27 − 2 − 1 = 24 秒。
pub fn service_stop_budget(notice: &StopNotice, now: Instant) -> Duration {
    match notice.os_work_deadline() {
        None => INTERRUPT_BUDGET,
        Some(deadline) => deadline
            .saturating_duration_since(now)
            .saturating_sub(SERVICE_LOG_FLUSH_WAIT)
            .saturating_sub(PRESHUTDOWN_REPORT_RESERVE),
    }
}

/// 後始末（またはその途中の打ち切り）の後に、ログの書き出しを待つ上限。`now` は
/// 待ちを始める時刻。STOP は [`SERVICE_LOG_FLUSH_WAIT`]、PRESHUTDOWN はそれを
/// 「期限 − `now` − 余裕」で頭打ちにする（負なら 0）。後始末が予算を使い切っても、
/// 書き出しの待ちで期限を超えない。
pub fn service_flush_wait(notice: &StopNotice, now: Instant) -> Duration {
    match notice.os_work_deadline() {
        None => SERVICE_LOG_FLUSH_WAIT,
        Some(deadline) => SERVICE_LOG_FLUSH_WAIT.min(
            deadline
                .saturating_duration_since(now)
                .saturating_sub(PRESHUTDOWN_REPORT_RESERVE),
        ),
    }
}

/// 停止の合図の名前（ログと、起動の途中の中止の行に出す）。
pub fn stop_signal_label(signal: StopSignal) -> &'static str {
    match signal {
        StopSignal::ServiceStop => "SCM の STOP",
        StopSignal::OsPreshutdown => "OS のシャットダウン（PRESHUTDOWN）",
    }
}

/// 日次ログを開いた直後の 1 行（コンソールの起動と見分ける）。
pub fn service_starting_line() -> String {
    format!("{SERVER_LABEL}: Windows サービス '{SERVICE_NAME}' として起動します（SCM から）")
}

/// Running を報告した後の 1 行。
pub fn service_running_line() -> String {
    format!(
        "{SERVER_LABEL}: Windows サービスとして起動しました（停止は SCM の STOP、OS のシャットダウンでは PRESHUTDOWN を受けて最終 flush します）"
    )
}

/// ローカルグループ `ChronoGazer Operators`（サービスの開始・停止を委任された利用者。
/// elev の `OPERATORS_GROUP` の名前と、停止の案内の文言が共有する）。
pub const OPERATORS_GROUP_NAME: &str = "ChronoGazer Operators";

/// 待ち受けの案内の最後の行（コンソールの「Ctrl-C で停止します」の代わり）。
pub fn service_stop_hint_line() -> String {
    format!(
        "{SERVER_LABEL}: 管理者の PowerShell、または '{OPERATORS_GROUP_NAME}' のメンバーが Stop-Service {SERVICE_NAME} で停止します"
    )
}

/// 停止に入ったときの 1 行。
pub fn service_stop_reason_line(signal: StopSignal, budget: Duration) -> String {
    let what = match signal {
        StopSignal::ServiceStop => "停止要求を受けました（SCM の STOP）",
        StopSignal::OsPreshutdown => "OS のシャットダウンを受けました（PRESHUTDOWN）",
    };
    format!(
        "{SERVER_LABEL}: {what}。最終 flush を行います（上限 {} 秒）",
        budget.as_secs()
    )
}

/// 最終 flush を含む停止が予算内に終わったときの 1 行（予算切れはエンジンが自分で
/// 1 行出す）。
pub fn service_stop_done_line(signal: StopSignal) -> String {
    format!(
        "{SERVER_LABEL}: 最終 flush を含む停止が完了しました（{}）",
        stop_signal_label(signal)
    )
}

/// 本体の最後の 1 行。
pub fn service_stopped_line(exit: ServiceExit) -> String {
    match exit {
        ServiceExit::Success => format!("{SERVER_LABEL}: Windows サービスを停止しました"),
        ServiceExit::Specific(code) => {
            format!("{SERVER_LABEL}: Windows サービスを停止しました（終了コード {code}）")
        }
    }
}

/// 記録したインストール利用者の SID（`Parameters` の読み取りの結果）から、データの
/// ルートと Hub の API キーの保存先の ACL の指定を決める（D1・D4）。`read` は
/// 値が無ければ `Ok(None)`、読めなければ `Err(理由)`。記録が使えなければ
/// `service_only` と、ログに出す 1 行（理由と対処）を返す。
pub fn access_from_parameter(read: Result<Option<&str>, &str>) -> (DirAccess, Option<String>) {
    let problem = match read {
        Ok(Some(raw)) => match InstallUserSid::parse(raw) {
            Ok(sid) => return (DirAccess::with_install_user(sid), None),
            Err(_) => format!("インストール利用者の記録（{INSTALL_USER_SID_LOCATION}）が利用者のアカウントの SID ではありません（{raw:?}）"),
        },
        Ok(None) => format!("インストール利用者の記録（{INSTALL_USER_SID_LOCATION}）がありません"),
        Err(err) => format!(
            "インストール利用者の記録（{INSTALL_USER_SID_LOCATION}）を読めませんでした: {err}"
        ),
    };
    (
        DirAccess::service_only(),
        Some(format!(
            "{SERVER_LABEL}: warn: {problem}。SYSTEM・Administrators だけの ACL で続けます（これから作るデータのルートと Hub の API キーの保存先に利用者の ACE を付けないため、デスクトップと Hub の API キーを共有できません）。直すには、管理者の PowerShell で `chronogazer-server.exe install` をやり直してください（記録が無い・不正なら書きます）"
        )),
    )
}

// --- 登録（install） --------------------------------------------------------------

/// 登録で、インストール利用者の記録をどうするか（D1・D2）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidRecordPlan {
    /// 記録が無い: 書く。
    Write,
    /// 記録はあるが利用者のアカウントの SID ではない（形が違う・グループの領域など）:
    /// 書き直す（`previous` は元の値）。
    Replace { previous: String },
    /// 同じ SID が記録済み: 何もしない。
    KeepSame,
    /// 違う SID が記録済み: 変えずに警告する（D2。変えるには uninstall → install）。
    KeepDifferent { recorded: InstallUserSid },
}

/// 記録の今の値（`existing`）と、登録で使う SID（`requested`）から決める。
pub fn plan_sid_record(existing: Option<&str>, requested: &InstallUserSid) -> SidRecordPlan {
    match existing {
        None => SidRecordPlan::Write,
        Some(raw) => match InstallUserSid::parse(raw) {
            Err(_) => SidRecordPlan::Replace {
                previous: raw.to_string(),
            },
            Ok(recorded) if recorded == *requested => SidRecordPlan::KeepSame,
            Ok(recorded) => SidRecordPlan::KeepDifferent { recorded },
        },
    }
}

/// インストール利用者の記録の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidRecordResult {
    /// 書いた（`replaced` は書き直した元の値）。
    Written {
        sid: String,
        replaced: Option<String>,
    },
    /// 同じ SID が記録済みだった。
    AlreadySame { sid: String },
    /// 違う SID が記録済みだった（変えていない）。
    KeptDifferent { recorded: String, requested: String },
    /// 今の記録を読めなかった（書いていない）。
    ReadFailed(String),
    /// 書けなかった。
    WriteFailed { sid: String, error: String },
}

impl SidRecordResult {
    /// 記録に残っている（サービスが使う）SID。失敗なら `None`。
    pub fn effective_sid(&self) -> Option<&str> {
        match self {
            Self::Written { sid, .. } | Self::AlreadySame { sid } => Some(sid),
            Self::KeptDifferent { recorded, .. } => Some(recorded),
            Self::ReadFailed(_) | Self::WriteFailed { .. } => None,
        }
    }

    fn is_failure(&self) -> bool {
        self.effective_sid().is_none()
    }
}

/// 登録のときのデータのルートの用意（D3: 無ければ記録した SID で作る。あれば触らない）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootPreparation {
    /// 作った。
    Created(PathBuf),
    /// 既にあった（ACL は変えていない）。
    Existing(PathBuf),
    /// 作れなかった（サービスの開始のときにもう一度作る）。
    Failed { root: PathBuf, error: String },
    /// ルートの指定（`CHRONOGAZER_ROOT`）が使えない。
    Unusable(String),
}

/// `install` でしたことのまとめ（[`install_lines`]・[`install_exit_code`] の入力）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallReport {
    /// 登録した実行ファイル。
    pub executable: PathBuf,
    /// Program Files の場所（`%ProgramFiles%`。実行ファイルの置き場所の警告に使う。
    /// 取れなければ `None` で、警告しない）。
    pub program_files: Option<PathBuf>,
    /// SCM への登録の結果。
    pub outcome: Result<InstallOutcome, InstallError>,
    /// インストール利用者の記録（登録に失敗したら `None`）。
    pub sid: Option<SidRecordResult>,
    /// データのルート（記録に失敗したら `None`）。
    pub root: Option<RootPreparation>,
}

/// 実行ファイルが Program Files の外にあれば、警告の 1 行。LocalSystem のサービスとして
/// 一般の利用者が書き換えられる場所の exe を登録すると、その利用者が SYSTEM の権限で
/// 任意のコードを動かせるため。
pub fn executable_location_warning(
    executable: &Path,
    program_files: Option<&Path>,
) -> Option<String> {
    let program_files = program_files?;
    let inside = executable
        .to_string_lossy()
        .to_lowercase()
        .starts_with(&format!(
            "{}\\",
            program_files
                .to_string_lossy()
                .trim_end_matches('\\')
                .to_lowercase()
        ));
    (!inside).then(|| {
        format!(
            "{SERVER_LABEL}: warn: 実行ファイルが {} の外にあります。一般の利用者が書き換えられる場所の exe を LocalSystem のサービスとして登録すると、権限の昇格に使われます。{}\\ChronoGazer\\ に置いてから、uninstall → install で登録し直してください",
            program_files.display(),
            program_files.display()
        )
    })
}

/// `install` で、インストール利用者の SID を決められなかったとき（登録の前に止める）。
pub fn install_user_unavailable_lines(error: &str) -> Vec<ConsoleLine> {
    vec![
        ConsoleLine::Stderr(format!(
            "{SERVER_LABEL}: インストール利用者（この PC の利用者のアカウント）を決められませんでした: {error}"
        )),
        ConsoleLine::Stderr(format!(
            "{SERVER_LABEL}:   データを使う利用者のアカウントでサインインし、管理者として実行した PowerShell から実行するか、--install-user-sid S-1-5-21-… で指定してください（何も登録していません）"
        )),
    ]
}

/// `install` の結果を利用者に出す行にする。
pub fn install_lines(report: &InstallReport) -> Vec<ConsoleLine> {
    let outcome = match &report.outcome {
        Ok(outcome) => outcome,
        Err(InstallError::Connect(err)) => {
            return vec![
                ConsoleLine::Stderr(format!(
                    "{SERVER_LABEL}: Service Control Manager への接続に失敗しました: {err}"
                )),
                ConsoleLine::Stderr(format!("{SERVER_LABEL}:   {ELEVATION_HINT}")),
            ];
        }
        Err(InstallError::Create(err)) => {
            return vec![ConsoleLine::Stderr(format!(
                "{SERVER_LABEL}: サービスの登録に失敗しました: {err}"
            ))];
        }
    };
    let mut lines = Vec::new();
    match outcome.kind {
        InstallKind::AlreadyRegistered => {
            lines.push(ConsoleLine::Stdout(format!(
                "{SERVER_LABEL}: Windows サービス '{SERVICE_NAME}' は既に登録されています（起動種別・実行ファイル・ACL は変更していません）"
            )));
            lines.push(ConsoleLine::Stdout(format!(
                "{SERVER_LABEL}:   実行ファイルを変えるときは uninstall してから install し直してください"
            )));
            lines.push(preshutdown_console_line(SERVER_LABEL, &outcome.preshutdown));
        }
        InstallKind::Created => {
            // 先に「登録しました」を言い、その後に細かい設定（説明文・PRESHUTDOWN）を並べる
            // （PRESHUTDOWN の行が登録の行より先に出ると、順番が逆に読める。#628）。
            lines.push(ConsoleLine::Stdout(format!(
                "{SERVER_LABEL}: Windows サービス '{SERVICE_NAME}' を登録しました"
            )));
            lines.push(ConsoleLine::Stdout(format!(
                "{SERVER_LABEL}:   表示名: {SERVICE_DISPLAY_NAME}"
            )));
            lines.push(ConsoleLine::Stdout(format!(
                "{SERVER_LABEL}:   実行ファイル: {}",
                report.executable.display()
            )));
            lines.push(ConsoleLine::Stdout(format!(
                "{SERVER_LABEL}:   起動種別: 手動（Demand） - OS の再起動だけでは開始しません"
            )));
            if let Some(err) = &outcome.description_error {
                lines.push(ConsoleLine::Stderr(format!(
                    "{SERVER_LABEL}: サービスの説明文の設定に失敗しました（登録自体は完了）: {err}"
                )));
            }
            lines.push(preshutdown_console_line(SERVER_LABEL, &outcome.preshutdown));
            if let Some(warning) =
                executable_location_warning(&report.executable, report.program_files.as_deref())
            {
                lines.push(ConsoleLine::Stderr(warning));
            }
        }
    }
    match &report.sid {
        None => {}
        Some(SidRecordResult::Written { sid, replaced }) => {
            lines.push(ConsoleLine::Stdout(format!(
                "{SERVER_LABEL}:   インストール利用者: {sid}（{INSTALL_USER_SID_LOCATION} に記録しました）"
            )));
            if let Some(previous) = replaced {
                lines.push(ConsoleLine::Stderr(format!(
                    "{SERVER_LABEL}: warn: 記録されていた値（{previous:?}）は利用者のアカウントの SID ではなかったため書き直しました"
                )));
            }
        }
        Some(SidRecordResult::AlreadySame { sid }) => {
            lines.push(ConsoleLine::Stdout(format!(
                "{SERVER_LABEL}:   インストール利用者: {sid}（記録済み）"
            )));
        }
        Some(SidRecordResult::KeptDifferent {
            recorded,
            requested,
        }) => {
            lines.push(ConsoleLine::Stdout(format!(
                "{SERVER_LABEL}:   インストール利用者: {recorded}（記録済み。変えていません）"
            )));
            lines.push(ConsoleLine::Stderr(format!(
                "{SERVER_LABEL}: warn: 今回の利用者（{requested}）は記録と違います。記録は変えていません（変えるには uninstall してから install し直してください）"
            )));
        }
        Some(SidRecordResult::ReadFailed(err)) => {
            lines.push(ConsoleLine::Stderr(format!(
                "{SERVER_LABEL}: インストール利用者の記録（{INSTALL_USER_SID_LOCATION}）を読めませんでした: {err}"
            )));
            lines.push(record_failure_hint());
        }
        Some(SidRecordResult::WriteFailed { sid, error }) => {
            lines.push(ConsoleLine::Stderr(format!(
                "{SERVER_LABEL}: インストール利用者（{sid}）を {INSTALL_USER_SID_LOCATION} に記録できませんでした: {error}"
            )));
            lines.push(record_failure_hint());
        }
    }
    match &report.root {
        None => {}
        Some(RootPreparation::Created(root)) => lines.push(ConsoleLine::Stdout(format!(
            "{SERVER_LABEL}:   データのルート: {}（作りました。SYSTEM・Administrators にフル、インストール利用者に変更）",
            root.display()
        ))),
        Some(RootPreparation::Existing(root)) => lines.push(ConsoleLine::Stdout(format!(
            "{SERVER_LABEL}:   データのルート: {}（既にあります。ACL は変えていません）",
            root.display()
        ))),
        Some(RootPreparation::Failed { root, error }) => {
            lines.push(ConsoleLine::Stderr(format!(
                "{SERVER_LABEL}: warn: データのルート（{}）を作れませんでした: {error}（サービスの開始のときにもう一度作ります）",
                root.display()
            )))
        }
        Some(RootPreparation::Unusable(reason)) => lines.push(ConsoleLine::Stderr(format!(
            "{SERVER_LABEL}: warn: データのルートを使えません: {reason}（このままではサービスも起動しません）"
        ))),
    }
    // 成功の案内は、登録と記録が済み、ルートも使えるときだけ（ルートが使えなければ
    // サービスは起動しない。終了コードは 0 のまま - 既知の制限 4、PR #618 のレビュー）。
    let root_unusable = matches!(report.root, Some(RootPreparation::Unusable(_)));
    if install_exit_code(report) == 0 && !root_unusable {
        // 既に登録されていたときは「登録しました」と言わない（何も登録していない。#628）。
        lines.push(ConsoleLine::Stdout(match outcome.kind {
            InstallKind::Created => format!(
                "{SERVER_LABEL}: 登録しました（手動開始）。開始: 管理者の PowerShell で Start-Service {SERVICE_NAME}"
            ),
            InstallKind::AlreadyRegistered => format!(
                "{SERVER_LABEL}: 登録は済んでいました（今回は登録していません）。開始: 管理者の PowerShell で Start-Service {SERVICE_NAME}"
            ),
        }));
    }
    lines
}

fn record_failure_hint() -> ConsoleLine {
    ConsoleLine::Stderr(format!(
        "{SERVER_LABEL}:   サービスの登録は済んでいます。記録が無い間、サービスは SYSTEM・Administrators だけの ACL で動きます（デスクトップと Hub の API キーを共有できません）。{ELEVATION_HINT}（install をやり直します）"
    ))
}

/// `install` の終了コード: SCM への登録とインストール利用者の記録が済めば 0、
/// どちらかに失敗したら 1（ルートを作れなかったのは警告で 0。サービスの開始が
/// もう一度作る）。
pub fn install_exit_code(report: &InstallReport) -> i32 {
    let failed =
        report.outcome.is_err() || report.sid.as_ref().is_some_and(SidRecordResult::is_failure);
    i32::from(failed)
}

/// `install_with` で、インストール利用者の SID を決められなかった（登録の前に止めた。
/// 中身は理由の文言）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallUserUnavailable(pub String);

/// `install_with` の結果を、利用者に出す行と終了コードにする（`chronogazer-server
/// install` と elev が共有。#392 B3-3b で bin から移した - 出す行と終了コードは移す
/// 前と同じ）。
pub fn install_result_lines(
    result: &Result<InstallReport, InstallUserUnavailable>,
) -> (Vec<ConsoleLine>, i32) {
    match result {
        Err(InstallUserUnavailable(error)) => (install_user_unavailable_lines(error), 1),
        Ok(report) => (install_lines(report), install_exit_code(report)),
    }
}

/// インストール利用者の記録（`read` で今の値を読み、[`plan_sid_record`] のとおりに
/// `write` する）。HKLM を触る関数はテストで偽物に差し替える。
pub fn record_install_user(
    requested: &InstallUserSid,
    read: impl FnOnce() -> Result<Option<String>, String>,
    write: impl FnOnce(&str) -> Result<(), String>,
) -> SidRecordResult {
    let existing = match read() {
        Ok(existing) => existing,
        Err(err) => return SidRecordResult::ReadFailed(err),
    };
    let sid = requested.as_str().to_string();
    let written = |replaced: Option<String>| match write(&sid) {
        Ok(()) => SidRecordResult::Written {
            sid: sid.clone(),
            replaced,
        },
        Err(error) => SidRecordResult::WriteFailed {
            sid: sid.clone(),
            error,
        },
    };
    match plan_sid_record(existing.as_deref(), requested) {
        SidRecordPlan::Write => written(None),
        SidRecordPlan::Replace { previous } => written(Some(previous)),
        SidRecordPlan::KeepSame => SidRecordResult::AlreadySame { sid },
        SidRecordPlan::KeepDifferent { recorded } => SidRecordResult::KeptDifferent {
            recorded: recorded.as_str().to_string(),
            requested: sid,
        },
    }
}

/// D3: ルートが無ければ `sid` を利用者の欄にして作る（あれば触らない。O6）。
pub fn prepare_root(layout: &DataLayout, sid: &InstallUserSid) -> RootPreparation {
    let root = layout.root().to_path_buf();
    if let Err(reason) = crate::server_host::validate_root(&root) {
        return RootPreparation::Unusable(reason);
    }
    match create_root_dir_with(layout, &DirAccess::with_install_user(sid.clone())) {
        Ok(true) => RootPreparation::Created(root),
        Ok(false) => RootPreparation::Existing(root),
        Err(error) => RootPreparation::Failed { root, error },
    }
}

/// `install` の本体（Windows だけ。#392 B3-2b の bin の `install` から B3-3b で移した）:
/// サービスを登録し、インストール利用者の SID を記録し（D1・D2）、ルートが無ければ
/// その SID で作る（D3）。
///
/// - `executable`: 登録する `chronogazer-server.exe`（`chronogazer-server install` は
///   自分、elev は隣の `chronogazer-server.exe`）。
/// - `install_user_sid`: `None` なら実行した利用者（昇格していてもトークンの利用者は
///   その人）。LocalSystem などで決められなければ、何も登録せずに
///   [`InstallUserUnavailable`]。
/// - `layout`: データのルート（`chronogazer-server` はプロセスの環境、elev はマシンの
///   環境から決める）。
/// - `program_files`: 実行ファイルの置き場所の警告に使う Program Files（`None` なら
///   警告しない）。
#[cfg(windows)]
pub fn install_with(
    executable: PathBuf,
    install_user_sid: Option<InstallUserSid>,
    layout: &DataLayout,
    program_files: Option<PathBuf>,
) -> Result<InstallReport, InstallUserUnavailable> {
    use banto_service_control::{install_service, read_service_parameter, write_service_parameter};

    let requested = match install_user_sid {
        Some(sid) => sid,
        None => banto_secret_store::acl::current_user_sid()
            .map_err(|err| InstallUserUnavailable(err.to_string()))?,
    };
    let outcome = install_service(&crate::shell_launch::chronogazer_service_spec(
        executable.clone(),
    ));
    let mut report = InstallReport {
        executable,
        program_files,
        outcome,
        sid: None,
        root: None,
    };
    if report.outcome.is_ok() {
        let record = record_install_user(
            &requested,
            || {
                read_service_parameter(SERVICE_NAME, INSTALL_USER_SID_VALUE)
                    .map_err(|err| err.to_string())
            },
            |sid| {
                write_service_parameter(SERVICE_NAME, INSTALL_USER_SID_VALUE, sid)
                    .map_err(|err| err.to_string())
            },
        );
        report.root = record
            .effective_sid()
            .and_then(|sid| InstallUserSid::parse(sid).ok())
            .map(|sid| prepare_root(layout, &sid));
        report.sid = Some(record);
    }
    Ok(report)
}

// --- 解除（uninstall） --------------------------------------------------------------

/// `uninstall` の結果を利用者に出す行にする（失敗なら終了コード 1 は呼び出し側）。
/// `root` はデータのルート（残すことを伝える）。
pub fn uninstall_lines(
    result: &Result<UninstallOutcome, UninstallError>,
    root: &Path,
) -> Vec<ConsoleLine> {
    let stop_line = |stop: &StopAttempt| match stop {
        StopAttempt::NotRunning => None,
        StopAttempt::StopRequested => Some(ConsoleLine::Stdout(format!(
            "{SERVER_LABEL}: サービスに停止を要求しました（最終 flush を終えてから止まります）"
        ))),
        StopAttempt::StopFailed(err) => Some(ConsoleLine::Stderr(format!(
            "{SERVER_LABEL}: サービスの停止に失敗しました: {err}"
        ))),
        StopAttempt::QueryFailed(err) => Some(ConsoleLine::Stderr(format!(
            "{SERVER_LABEL}: サービス状態の取得に失敗しました: {err}"
        ))),
    };
    match result {
        Ok(outcome) => {
            let mut lines: Vec<ConsoleLine> = stop_line(&outcome.stop).into_iter().collect();
            lines.push(ConsoleLine::Stdout(format!(
                "{SERVER_LABEL}: Windows サービス '{SERVICE_NAME}' の登録を解除しました（インストール利用者の記録も一緒に消えます）"
            )));
            lines.push(ConsoleLine::Stdout(format!(
                "{SERVER_LABEL}: （サービスが止まり終わる・このプロセスのハンドルが閉じるまで、完全な削除が遅れることがあります - Windows API の仕様）"
            )));
            lines.push(ConsoleLine::Stdout(format!(
                "{SERVER_LABEL}: データ（{}）は残します",
                root.display()
            )));
            lines
        }
        Err(UninstallError::Connect(err)) => vec![
            ConsoleLine::Stderr(format!(
                "{SERVER_LABEL}: Service Control Manager への接続に失敗しました: {err}"
            )),
            ConsoleLine::Stderr(format!("{SERVER_LABEL}:   {ELEVATION_HINT}")),
        ],
        // 未登録（ERROR_SERVICE_DOES_NOT_EXIST = 1060）は日本語で案内する。OS の理由（英語の
        // ことがある）を出すのは、それ以外（権限不足など）だけ。
        Err(UninstallError::Open(err)) if err.contains("os error 1060") => {
            vec![ConsoleLine::Stderr(format!(
                "{SERVER_LABEL}: サービス '{SERVICE_NAME}' は登録されていません（解除するものがありません）"
            ))]
        }
        Err(UninstallError::Open(err)) => vec![ConsoleLine::Stderr(format!(
            "{SERVER_LABEL}: サービス '{SERVICE_NAME}' を開けません（権限がないか、サービスの状態を確かめられません）: {err}"
        ))],
        Err(UninstallError::Delete { stop, error }) => {
            let mut lines: Vec<ConsoleLine> = stop_line(stop).into_iter().collect();
            lines.push(ConsoleLine::Stderr(format!(
                "{SERVER_LABEL}: サービスの削除に失敗しました: {error}"
            )));
            lines
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use banto_service_control::PreshutdownReport;

    const USER: &str = "S-1-5-21-1111111111-2222222222-3333333333-1001";
    const OTHER: &str = "S-1-5-21-1111111111-2222222222-3333333333-1002";

    fn out(text: &str) -> ConsoleLine {
        ConsoleLine::Stdout(text.to_string())
    }

    fn err(text: &str) -> ConsoleLine {
        ConsoleLine::Stderr(text.to_string())
    }

    fn sid(text: &str) -> InstallUserSid {
        InstallUserSid::parse(text).unwrap()
    }

    #[test]
    fn exit_codes_map_to_the_scm() {
        assert_eq!(service_exit_for(0), ServiceExit::Success);
        assert_eq!(service_exit_for(1), ServiceExit::Specific(1));
        assert_eq!(service_exit_for(2), ServiceExit::Specific(2));
        assert_eq!(service_exit_for(3), ServiceExit::Specific(3));
        // それ以外（起動の失敗のはずの値）は 3。
        for other in [-1, 4, 64] {
            assert_eq!(service_exit_for(other), ServiceExit::Specific(3), "{other}");
        }
        // 1 は本体の panic と同じ値（モジュール doc）。
        assert_eq!(
            service_exit_for(1),
            ServiceExit::Specific(banto_service_control::PANIC_EXIT_CODE)
        );
    }

    fn notice(signal: StopSignal, received_at: Instant) -> StopNotice {
        StopNotice {
            signal,
            received_at,
        }
    }

    #[test]
    fn stop_budget_leaves_room_for_the_log_flush_and_the_report() {
        let received = Instant::now();
        assert_eq!(
            service_stop_budget(&notice(StopSignal::ServiceStop, received), received),
            Duration::from_secs(15)
        );
        // 受けた直後: 27 秒 - 書き出し 2 秒 - 余裕 1 秒。
        assert_eq!(
            service_stop_budget(&notice(StopSignal::OsPreshutdown, received), received),
            Duration::from_secs(24)
        );
        // 10 秒たってから始めた: 残りの 17 秒 - 2 秒 - 1 秒。
        assert_eq!(
            service_stop_budget(
                &notice(StopSignal::OsPreshutdown, received),
                received + Duration::from_secs(10)
            ),
            Duration::from_secs(14)
        );
        // 期限の 2.5 秒前・期限を過ぎた: 0（負にしない）。
        for late in [Duration::from_millis(24_500), Duration::from_secs(60)] {
            assert_eq!(
                service_stop_budget(
                    &notice(StopSignal::OsPreshutdown, received),
                    received + late
                ),
                Duration::ZERO
            );
        }
        // 書き出しの待ちも、期限の 1 秒前を過ぎたら 0。
        for late in [26, 27, 60] {
            let late = Duration::from_secs(late);
            assert_eq!(
                service_flush_wait(
                    &notice(StopSignal::OsPreshutdown, received),
                    received + late
                ),
                Duration::ZERO,
                "{late:?}"
            );
        }
        // STOP はいつ始めても 15 秒・書き出し 2 秒（打ち切りの期限は無い）。
        let late = received + Duration::from_secs(60);
        assert_eq!(
            service_stop_budget(&notice(StopSignal::ServiceStop, received), late),
            Duration::from_secs(15)
        );
        assert_eq!(
            service_flush_wait(&notice(StopSignal::ServiceStop, received), late),
            SERVICE_LOG_FLUSH_WAIT
        );
        // STOP: 後始末 + 書き出し + 余裕が heartbeat の見込みに収まる。
        assert!(
            INTERRUPT_BUDGET + SERVICE_LOG_FLUSH_WAIT + PRESHUTDOWN_REPORT_RESERVE
                <= SERVICE_EXPECTED_STOP
        );
    }

    /// PR #618 の Codex の指摘 P2: PRESHUTDOWN では、どの時点で始めても、後始末が予算を
    /// 使い切ってから書き出しを上限まで待っても、期限の 1 秒前（Stopped の報告の余裕）を
    /// 超えない。始めた時点で既に過ぎていれば、どちらも 0。
    #[test]
    fn preshutdown_budget_plus_flush_never_passes_the_os_deadline() {
        let received = Instant::now();
        let notice = notice(StopSignal::OsPreshutdown, received);
        let limit = notice.os_work_deadline().unwrap() - PRESHUTDOWN_REPORT_RESERVE;
        for start_ms in (0..=30_000).step_by(250) {
            let start = received + Duration::from_millis(start_ms);
            let budget = service_stop_budget(&notice, start);
            // 最悪の場合: 後始末が予算を使い切ってから書き出しを待つ。
            let flush_start = start + budget;
            let flush = service_flush_wait(&notice, flush_start);
            if start <= limit {
                assert!(
                    flush_start + flush <= limit,
                    "{start_ms} ms: budget {budget:?} flush {flush:?}"
                );
            } else {
                assert!(budget.is_zero() && flush.is_zero(), "{start_ms} ms");
            }
        }
    }

    #[test]
    fn log_lines_name_the_windows_service_and_the_scm_controls() {
        assert_eq!(
            service_starting_line(),
            "chronogazer-server: Windows サービス 'ChronoGazer' として起動します（SCM から）"
        );
        assert!(service_running_line().contains("Windows サービスとして起動しました"));
        assert!(service_running_line().contains("SCM の STOP"));
        assert!(service_running_line().contains("PRESHUTDOWN"));
        // 停止の案内は Operators のメンバーも止められることを言う（#628）。
        assert_eq!(
            service_stop_hint_line(),
            "chronogazer-server: 管理者の PowerShell、または 'ChronoGazer Operators' のメンバーが Stop-Service ChronoGazer で停止します"
        );
        assert_eq!(
            service_stop_reason_line(StopSignal::ServiceStop, Duration::from_secs(15)),
            "chronogazer-server: 停止要求を受けました（SCM の STOP）。最終 flush を行います（上限 15 秒）"
        );
        assert_eq!(
            service_stop_reason_line(StopSignal::OsPreshutdown, Duration::from_millis(26_400)),
            "chronogazer-server: OS のシャットダウンを受けました（PRESHUTDOWN）。最終 flush を行います（上限 26 秒）"
        );
        assert_eq!(
            service_stop_done_line(StopSignal::ServiceStop),
            "chronogazer-server: 最終 flush を含む停止が完了しました（SCM の STOP）"
        );
        assert_eq!(
            service_stop_done_line(StopSignal::OsPreshutdown),
            "chronogazer-server: 最終 flush を含む停止が完了しました（OS のシャットダウン（PRESHUTDOWN））"
        );
        assert_eq!(
            service_stopped_line(ServiceExit::Success),
            "chronogazer-server: Windows サービスを停止しました"
        );
        assert_eq!(
            service_stopped_line(ServiceExit::Specific(1)),
            "chronogazer-server: Windows サービスを停止しました（終了コード 1）"
        );
    }

    /// D1・D4: 記録が使えればその SID、無い・不正・読めなければ `service_only` と
    /// 理由と対処の 1 行。
    #[test]
    fn access_from_parameter_uses_the_record_or_falls_back_to_service_only() {
        assert_eq!(
            access_from_parameter(Ok(Some(USER))),
            (DirAccess::with_install_user(sid(USER)), None)
        );
        for (read, reason) in [
            (Ok(None), "がありません"),
            (
                Ok(Some("S-1-5-32-544")),
                "利用者のアカウントの SID ではありません",
            ),
            (Ok(Some("")), "利用者のアカウントの SID ではありません"),
            (
                Err("アクセスが拒否されました"),
                "読めませんでした: アクセスが拒否されました",
            ),
        ] {
            let (access, warning) = access_from_parameter(read);
            assert_eq!(access, DirAccess::service_only(), "{read:?}");
            let warning = warning.expect("理由の行");
            assert!(
                warning.starts_with("chronogazer-server: warn: "),
                "{warning}"
            );
            assert!(warning.contains(INSTALL_USER_SID_LOCATION), "{warning}");
            assert!(warning.contains(reason), "{warning}");
            assert!(
                warning.contains("chronogazer-server.exe install"),
                "{warning}"
            );
        }
    }

    /// D2: 同じなら何もしない、違えば変えない、無ければ書く、不正なら書き直す。
    #[test]
    fn sid_record_plan() {
        let me = sid(USER);
        assert_eq!(plan_sid_record(None, &me), SidRecordPlan::Write);
        assert_eq!(plan_sid_record(Some(USER), &me), SidRecordPlan::KeepSame);
        assert_eq!(
            plan_sid_record(Some(OTHER), &me),
            SidRecordPlan::KeepDifferent {
                recorded: sid(OTHER)
            }
        );
        assert_eq!(
            plan_sid_record(Some("garbage"), &me),
            SidRecordPlan::Replace {
                previous: "garbage".to_string()
            }
        );
    }

    fn exe() -> PathBuf {
        PathBuf::from(r"C:\Program Files\ChronoGazer\chronogazer-server.exe")
    }

    fn root() -> PathBuf {
        PathBuf::from(r"C:\ProgramData\ChronoGazer")
    }

    fn report(
        outcome: Result<InstallOutcome, InstallError>,
        sid: Option<SidRecordResult>,
        root: Option<RootPreparation>,
    ) -> InstallReport {
        InstallReport {
            executable: exe(),
            program_files: Some(PathBuf::from(r"C:\Program Files")),
            outcome,
            sid,
            root,
        }
    }

    fn created(description_error: Option<&str>) -> Result<InstallOutcome, InstallError> {
        Ok(InstallOutcome {
            kind: InstallKind::Created,
            description_error: description_error.map(str::to_string),
            preshutdown: PreshutdownReport::Applied("PRESHUTDOWN の行".to_string()),
        })
    }

    fn already() -> Result<InstallOutcome, InstallError> {
        Ok(InstallOutcome {
            kind: InstallKind::AlreadyRegistered,
            description_error: None,
            preshutdown: PreshutdownReport::Warning("PRESHUTDOWN の警告".to_string()),
        })
    }

    #[test]
    fn install_golden_new() {
        let report = report(
            created(None),
            Some(SidRecordResult::Written {
                sid: USER.to_string(),
                replaced: None,
            }),
            Some(RootPreparation::Created(root())),
        );
        assert_eq!(
            install_lines(&report),
            vec![
                out("chronogazer-server: Windows サービス 'ChronoGazer' を登録しました"),
                out("chronogazer-server:   表示名: ChronoGazer 記録エンジン"),
                out(r"chronogazer-server:   実行ファイル: C:\Program Files\ChronoGazer\chronogazer-server.exe"),
                out("chronogazer-server:   起動種別: 手動（Demand） - OS の再起動だけでは開始しません"),
                out("chronogazer-server: PRESHUTDOWN の行"),
                out(r"chronogazer-server:   インストール利用者: S-1-5-21-1111111111-2222222222-3333333333-1001（HKLM\SYSTEM\CurrentControlSet\Services\ChronoGazer\Parameters\InstallUserSid に記録しました）"),
                out(r"chronogazer-server:   データのルート: C:\ProgramData\ChronoGazer（作りました。SYSTEM・Administrators にフル、インストール利用者に変更）"),
                out("chronogazer-server: 登録しました（手動開始）。開始: 管理者の PowerShell で Start-Service ChronoGazer"),
            ]
        );
        assert_eq!(install_exit_code(&report), 0);
    }

    #[test]
    fn install_golden_new_with_warnings() {
        let mut report = report(
            created(Some("os error 5")),
            Some(SidRecordResult::Written {
                sid: USER.to_string(),
                replaced: Some("garbage".to_string()),
            }),
            Some(RootPreparation::Failed {
                root: root(),
                error: "os error 5".to_string(),
            }),
        );
        report.executable = PathBuf::from(r"C:\Users\me\Downloads\chronogazer-server.exe");
        assert_eq!(
            install_lines(&report),
            vec![
                out("chronogazer-server: Windows サービス 'ChronoGazer' を登録しました"),
                out("chronogazer-server:   表示名: ChronoGazer 記録エンジン"),
                out(r"chronogazer-server:   実行ファイル: C:\Users\me\Downloads\chronogazer-server.exe"),
                out("chronogazer-server:   起動種別: 手動（Demand） - OS の再起動だけでは開始しません"),
                err("chronogazer-server: サービスの説明文の設定に失敗しました（登録自体は完了）: os error 5"),
                out("chronogazer-server: PRESHUTDOWN の行"),
                err(r"chronogazer-server: warn: 実行ファイルが C:\Program Files の外にあります。一般の利用者が書き換えられる場所の exe を LocalSystem のサービスとして登録すると、権限の昇格に使われます。C:\Program Files\ChronoGazer\ に置いてから、uninstall → install で登録し直してください"),
                out(r"chronogazer-server:   インストール利用者: S-1-5-21-1111111111-2222222222-3333333333-1001（HKLM\SYSTEM\CurrentControlSet\Services\ChronoGazer\Parameters\InstallUserSid に記録しました）"),
                err(r#"chronogazer-server: warn: 記録されていた値（"garbage"）は利用者のアカウントの SID ではなかったため書き直しました"#),
                err(r"chronogazer-server: warn: データのルート（C:\ProgramData\ChronoGazer）を作れませんでした: os error 5（サービスの開始のときにもう一度作ります）"),
                out("chronogazer-server: 登録しました（手動開始）。開始: 管理者の PowerShell で Start-Service ChronoGazer"),
            ]
        );
        // ルートを作れなかったのは警告（サービスの開始がもう一度作る）。
        assert_eq!(install_exit_code(&report), 0);
    }

    #[test]
    fn install_golden_already_registered_with_the_same_sid() {
        let report = report(
            already(),
            Some(SidRecordResult::AlreadySame {
                sid: USER.to_string(),
            }),
            Some(RootPreparation::Existing(root())),
        );
        assert_eq!(
            install_lines(&report),
            vec![
                out("chronogazer-server: Windows サービス 'ChronoGazer' は既に登録されています（起動種別・実行ファイル・ACL は変更していません）"),
                out("chronogazer-server:   実行ファイルを変えるときは uninstall してから install し直してください"),
                err("chronogazer-server: warn: PRESHUTDOWN の警告"),
                out("chronogazer-server:   インストール利用者: S-1-5-21-1111111111-2222222222-3333333333-1001（記録済み）"),
                out(r"chronogazer-server:   データのルート: C:\ProgramData\ChronoGazer（既にあります。ACL は変えていません）"),
                out("chronogazer-server: 登録は済んでいました（今回は登録していません）。開始: 管理者の PowerShell で Start-Service ChronoGazer"),
            ]
        );
        assert_eq!(install_exit_code(&report), 0);
    }

    /// D2: 違う SID は記録を変えずに警告（終了コード 0）。
    #[test]
    fn install_golden_already_registered_with_a_different_sid() {
        let report = report(
            already(),
            Some(SidRecordResult::KeptDifferent {
                recorded: OTHER.to_string(),
                requested: USER.to_string(),
            }),
            Some(RootPreparation::Existing(root())),
        );
        assert_eq!(
            install_lines(&report),
            vec![
                out("chronogazer-server: Windows サービス 'ChronoGazer' は既に登録されています（起動種別・実行ファイル・ACL は変更していません）"),
                out("chronogazer-server:   実行ファイルを変えるときは uninstall してから install し直してください"),
                err("chronogazer-server: warn: PRESHUTDOWN の警告"),
                out("chronogazer-server:   インストール利用者: S-1-5-21-1111111111-2222222222-3333333333-1002（記録済み。変えていません）"),
                err("chronogazer-server: warn: 今回の利用者（S-1-5-21-1111111111-2222222222-3333333333-1001）は記録と違います。記録は変えていません（変えるには uninstall してから install し直してください）"),
                out(r"chronogazer-server:   データのルート: C:\ProgramData\ChronoGazer（既にあります。ACL は変えていません）"),
                out("chronogazer-server: 登録は済んでいました（今回は登録していません）。開始: 管理者の PowerShell で Start-Service ChronoGazer"),
            ]
        );
        assert_eq!(install_exit_code(&report), 0);
    }

    /// PR #618 のオーナーレビュー: ルートの指定が使えないときは、終了コードは 0 のまま
    /// （既知の制限 4）だが、「登録しました（手動開始）」の成功の案内は出さない。
    #[test]
    fn install_golden_with_an_unusable_root() {
        let report = report(
            created(None),
            Some(SidRecordResult::Written {
                sid: USER.to_string(),
                replaced: None,
            }),
            Some(RootPreparation::Unusable(
                "CHRONOGAZER_ROOT が絶対パスではありません".to_string(),
            )),
        );
        assert_eq!(
            install_lines(&report),
            vec![
                out("chronogazer-server: Windows サービス 'ChronoGazer' を登録しました"),
                out("chronogazer-server:   表示名: ChronoGazer 記録エンジン"),
                out(r"chronogazer-server:   実行ファイル: C:\Program Files\ChronoGazer\chronogazer-server.exe"),
                out("chronogazer-server:   起動種別: 手動（Demand） - OS の再起動だけでは開始しません"),
                out("chronogazer-server: PRESHUTDOWN の行"),
                out(r"chronogazer-server:   インストール利用者: S-1-5-21-1111111111-2222222222-3333333333-1001（HKLM\SYSTEM\CurrentControlSet\Services\ChronoGazer\Parameters\InstallUserSid に記録しました）"),
                err("chronogazer-server: warn: データのルートを使えません: CHRONOGAZER_ROOT が絶対パスではありません（このままではサービスも起動しません）"),
            ]
        );
        assert_eq!(install_exit_code(&report), 0);
    }

    #[test]
    fn install_golden_failures() {
        let connect = report(
            Err(InstallError::Connect("os error 5".to_string())),
            None,
            None,
        );
        assert_eq!(
            install_lines(&connect),
            vec![
                err("chronogazer-server: Service Control Manager への接続に失敗しました: os error 5"),
                err("chronogazer-server:   管理者として実行した PowerShell から実行するか、同じフォルダの chronogazer-elev.exe（UAC で昇格します）を使ってください"),
            ]
        );
        assert_eq!(install_exit_code(&connect), 1);

        let create = report(
            Err(InstallError::Create("os error 1073".to_string())),
            None,
            None,
        );
        assert_eq!(
            install_lines(&create),
            vec![err(
                "chronogazer-server: サービスの登録に失敗しました: os error 1073"
            )]
        );
        assert_eq!(install_exit_code(&create), 1);

        let hint = err("chronogazer-server:   サービスの登録は済んでいます。記録が無い間、サービスは SYSTEM・Administrators だけの ACL で動きます（デスクトップと Hub の API キーを共有できません）。管理者として実行した PowerShell から実行するか、同じフォルダの chronogazer-elev.exe（UAC で昇格します）を使ってください（install をやり直します）");
        let write = report(
            created(None),
            Some(SidRecordResult::WriteFailed {
                sid: USER.to_string(),
                error: "サービス操作の権限がありません".to_string(),
            }),
            None,
        );
        let lines = install_lines(&write);
        assert_eq!(
            lines[lines.len() - 2..],
            [
                err(
                    r"chronogazer-server: インストール利用者（S-1-5-21-1111111111-2222222222-3333333333-1001）を HKLM\SYSTEM\CurrentControlSet\Services\ChronoGazer\Parameters\InstallUserSid に記録できませんでした: サービス操作の権限がありません"
                ),
                hint.clone(),
            ]
        );
        assert_eq!(install_exit_code(&write), 1);

        let read = report(
            already(),
            Some(SidRecordResult::ReadFailed("os error 5".to_string())),
            None,
        );
        let lines = install_lines(&read);
        assert_eq!(
            lines[lines.len() - 2..],
            [
                err(
                    r"chronogazer-server: インストール利用者の記録（HKLM\SYSTEM\CurrentControlSet\Services\ChronoGazer\Parameters\InstallUserSid）を読めませんでした: os error 5"
                ),
                hint,
            ]
        );
        assert_eq!(install_exit_code(&read), 1);
        // 失敗のときは「登録しました（手動開始）」を出さない。
        assert!(!lines.iter().any(
            |line| matches!(line, ConsoleLine::Stdout(text) if text.contains("Start-Service"))
        ));
    }

    #[test]
    fn install_user_unavailable_golden() {
        assert_eq!(
            install_user_unavailable_lines("利用者のアカウントの SID ではありません"),
            vec![
                err("chronogazer-server: インストール利用者（この PC の利用者のアカウント）を決められませんでした: 利用者のアカウントの SID ではありません"),
                err("chronogazer-server:   データを使う利用者のアカウントでサインインし、管理者として実行した PowerShell から実行するか、--install-user-sid S-1-5-21-… で指定してください（何も登録していません）"),
            ]
        );
    }

    /// #392 B3-3b: bin から移した `install` の結果の行と終了コード（移す前の bin と同じ:
    /// SID を決められなければ 2 行と 1、それ以外は `install_lines` と
    /// `install_exit_code`）。
    #[test]
    fn install_result_lines_match_the_previous_bin() {
        let unavailable = Err(InstallUserUnavailable(
            "利用者のアカウントの SID ではありません".to_string(),
        ));
        assert_eq!(
            install_result_lines(&unavailable),
            (
                install_user_unavailable_lines("利用者のアカウントの SID ではありません"),
                1
            )
        );
        let ok = report(
            created(None),
            Some(SidRecordResult::Written {
                sid: USER.to_string(),
                replaced: None,
            }),
            Some(RootPreparation::Created(root())),
        );
        assert_eq!(
            install_result_lines(&Ok(ok.clone())),
            (install_lines(&ok), 0)
        );
        let failed = report(
            Err(InstallError::Create("os error 1073".to_string())),
            None,
            None,
        );
        assert_eq!(
            install_result_lines(&Ok(failed.clone())),
            (install_lines(&failed), 1)
        );
    }

    /// 記録の読み書き: 無ければ書く・同じなら書かない・違えば書かずに残す（D2）・
    /// 不正なら書き直す・読めなければ書かない・書けなければ失敗（B3-2b の bin の
    /// テストを #392 B3-3b で移した）。
    #[test]
    fn the_install_user_record_follows_the_plan() {
        let writes = std::sync::Mutex::new(Vec::<String>::new());
        let write = |value: &str| {
            writes.lock().unwrap().push(value.to_string());
            Ok(())
        };
        assert_eq!(
            record_install_user(&sid(USER), || Ok(None), write),
            SidRecordResult::Written {
                sid: USER.to_string(),
                replaced: None
            }
        );
        assert_eq!(
            record_install_user(&sid(USER), || Ok(Some(USER.to_string())), write),
            SidRecordResult::AlreadySame {
                sid: USER.to_string()
            }
        );
        assert_eq!(
            record_install_user(&sid(USER), || Ok(Some(OTHER.to_string())), write),
            SidRecordResult::KeptDifferent {
                recorded: OTHER.to_string(),
                requested: USER.to_string()
            }
        );
        assert_eq!(
            record_install_user(&sid(USER), || Ok(Some("x".to_string())), write),
            SidRecordResult::Written {
                sid: USER.to_string(),
                replaced: Some("x".to_string())
            }
        );
        assert_eq!(
            record_install_user(&sid(USER), || Err("os error 5".to_string()), write),
            SidRecordResult::ReadFailed("os error 5".to_string())
        );
        // 書いたのは「無い」と「不正」の 2 回だけ。
        assert_eq!(*writes.lock().unwrap(), vec![USER, USER]);
        assert_eq!(
            record_install_user(&sid(USER), || Ok(None), |_| Err("拒否".to_string())),
            SidRecordResult::WriteFailed {
                sid: USER.to_string(),
                error: "拒否".to_string()
            }
        );
    }

    /// D3: 無いルートは渡した SID で作り、あるルートには触らない。使えないルートは
    /// 作らない（B3-2b の bin のテストを #392 B3-3b で移した。ACL は Windows だけ）。
    #[cfg(windows)]
    #[test]
    fn prepare_root_creates_with_the_recorded_sid_only_when_absent() {
        use banto_secret_store::acl::{current_user_sid, read_dir_dacl};
        let dir = tempfile::tempdir().unwrap();
        let me = current_user_sid().unwrap();
        let layout = DataLayout::new(dir.path().join("root"));
        assert_eq!(
            prepare_root(&layout, &me),
            RootPreparation::Created(layout.root().to_path_buf())
        );
        let dacl = read_dir_dacl(layout.root()).unwrap();
        assert!(dacl.protected, "{dacl:?}");
        assert!(dacl
            .aces
            .as_ref()
            .unwrap()
            .iter()
            .any(|ace| ace.sid == me.as_str()));
        assert_eq!(
            prepare_root(&layout, &me),
            RootPreparation::Existing(layout.root().to_path_buf())
        );
        assert!(matches!(
            prepare_root(&DataLayout::new("relative"), &me),
            RootPreparation::Unusable(_)
        ));
        assert!(!std::path::Path::new("relative").exists());
    }

    #[test]
    fn executable_location_warning_only_outside_program_files() {
        let pf = Path::new(r"C:\Program Files");
        assert_eq!(executable_location_warning(&exe(), Some(pf)), None);
        // 大文字小文字の違いは同じ場所。
        assert_eq!(
            executable_location_warning(
                Path::new(r"c:\program files\ChronoGazer\chronogazer-server.exe"),
                Some(Path::new(r"C:\Program Files\"))
            ),
            None
        );
        // 名前が前方一致するだけの別のフォルダは外。
        assert!(executable_location_warning(
            Path::new(r"C:\Program Files Evil\chronogazer-server.exe"),
            Some(pf)
        )
        .is_some());
        assert!(executable_location_warning(
            Path::new(r"D:\develop\target\debug\chronogazer-server.exe"),
            Some(pf)
        )
        .is_some());
        // Program Files が分からなければ警告しない。
        assert_eq!(
            executable_location_warning(Path::new(r"D:\x.exe"), None),
            None
        );
    }

    #[test]
    fn uninstall_golden() {
        let removed = [
            out("chronogazer-server: Windows サービス 'ChronoGazer' の登録を解除しました（インストール利用者の記録も一緒に消えます）"),
            out("chronogazer-server: （サービスが止まり終わる・このプロセスのハンドルが閉じるまで、完全な削除が遅れることがあります - Windows API の仕様）"),
            out(r"chronogazer-server: データ（C:\ProgramData\ChronoGazer）は残します"),
        ];
        let cases = [
            (StopAttempt::NotRunning, None),
            (
                StopAttempt::StopRequested,
                Some(out("chronogazer-server: サービスに停止を要求しました（最終 flush を終えてから止まります）")),
            ),
            (
                StopAttempt::StopFailed("os error 1061".to_string()),
                Some(err("chronogazer-server: サービスの停止に失敗しました: os error 1061")),
            ),
            (
                StopAttempt::QueryFailed("os error 6".to_string()),
                Some(err("chronogazer-server: サービス状態の取得に失敗しました: os error 6")),
            ),
        ];
        for (stop, line) in cases {
            let mut expected: Vec<ConsoleLine> = line.into_iter().collect();
            expected.extend(removed.iter().cloned());
            assert_eq!(
                uninstall_lines(&Ok(UninstallOutcome { stop }), &root()),
                expected
            );
        }
        assert_eq!(
            uninstall_lines(&Err(UninstallError::Connect("os error 5".to_string())), &root()),
            vec![
                err("chronogazer-server: Service Control Manager への接続に失敗しました: os error 5"),
                err("chronogazer-server:   管理者として実行した PowerShell から実行するか、同じフォルダの chronogazer-elev.exe（UAC で昇格します）を使ってください"),
            ]
        );
        // 未登録（1060）は日本語の案内にする（OS の英語の理由は出さない。#628）。OS の文言と
        // 番号つきの形（`uninstall_service` が返す形）も同じ。
        for reason in [
            "os error 1060",
            "The specified service does not exist as an installed service. (os error 1060)",
        ] {
            assert_eq!(
                uninstall_lines(&Err(UninstallError::Open(reason.to_string())), &root()),
                vec![err("chronogazer-server: サービス 'ChronoGazer' は登録されていません（解除するものがありません）")],
                "{reason}"
            );
        }
        // それ以外の理由（権限不足など）は OS の理由を添える。
        assert_eq!(
            uninstall_lines(&Err(UninstallError::Open("Access is denied. (os error 5)".to_string())), &root()),
            vec![err("chronogazer-server: サービス 'ChronoGazer' を開けません（権限がないか、サービスの状態を確かめられません）: Access is denied. (os error 5)")]
        );
        assert_eq!(
            uninstall_lines(
                &Err(UninstallError::Delete {
                    stop: StopAttempt::StopRequested,
                    error: "os error 1072".to_string()
                }),
                &root()
            ),
            vec![
                out("chronogazer-server: サービスに停止を要求しました（最終 flush を終えてから止まります）"),
                err("chronogazer-server: サービスの削除に失敗しました: os error 1072"),
            ]
        );
    }
}
