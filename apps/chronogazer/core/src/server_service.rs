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
//!   （受けてから 27 秒）までの残りから、Stopped の報告とログの書き出しの分
//!   [`PRESHUTDOWN_REPORT_RESERVE`]（1 秒）を引いたもの（負なら 0）。
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
//!   （UAC の昇格は B3-3 の `chronogazer-elev`）。
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

/// SCM に接続できないときの案内（管理者権限が要る）。
pub const ELEVATION_HINT: &str = "管理者として実行した PowerShell から実行してください（UAC の昇格は B3-3 の chronogazer-elev で入る予定です）";

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

/// 停止の予算（モジュール doc）。`now` は停止を始める時刻。
pub fn service_stop_budget(notice: &StopNotice, now: Instant) -> Duration {
    match notice.os_work_deadline() {
        None => INTERRUPT_BUDGET,
        Some(deadline) => deadline
            .saturating_duration_since(now)
            .saturating_sub(PRESHUTDOWN_REPORT_RESERVE),
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

/// 待ち受けの案内の最後の行（コンソールの「Ctrl-C で停止します」の代わり）。
pub fn service_stop_hint_line() -> String {
    format!("{SERVER_LABEL}: 管理者の PowerShell から Stop-Service {SERVICE_NAME} で停止します")
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
            Err(_) => format!("インストール利用者の記録（{INSTALL_USER_SID_LOCATION}）が SID の形ではありません（{raw:?}）"),
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
    /// 記録はあるが SID の形ではない: 書き直す（`previous` は元の値）。
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
            if let Some(err) = &outcome.description_error {
                lines.push(ConsoleLine::Stderr(format!(
                    "{SERVER_LABEL}: サービスの説明文の設定に失敗しました（登録自体は完了）: {err}"
                )));
            }
            lines.push(preshutdown_console_line(SERVER_LABEL, &outcome.preshutdown));
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
                    "{SERVER_LABEL}: warn: 記録されていた値（{previous:?}）は SID の形ではなかったため書き直しました"
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
    if install_exit_code(report) == 0 {
        lines.push(ConsoleLine::Stdout(format!(
            "{SERVER_LABEL}: 登録しました（手動開始）。開始: 管理者の PowerShell で Start-Service {SERVICE_NAME}"
        )));
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
        Err(UninstallError::Open(err)) => vec![ConsoleLine::Stderr(format!(
            "{SERVER_LABEL}: サービス '{SERVICE_NAME}' を開けません（未登録か、権限がありません）: {err}"
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
    fn stop_budget_is_15s_for_stop_and_the_os_deadline_minus_1s_for_preshutdown() {
        let received = Instant::now();
        assert_eq!(
            service_stop_budget(&notice(StopSignal::ServiceStop, received), received),
            Duration::from_secs(15)
        );
        // 受けた直後: 27 秒 - 1 秒。
        assert_eq!(
            service_stop_budget(&notice(StopSignal::OsPreshutdown, received), received),
            Duration::from_secs(26)
        );
        // 10 秒たってから始めた: 残りの 17 秒 - 1 秒。
        assert_eq!(
            service_stop_budget(
                &notice(StopSignal::OsPreshutdown, received),
                received + Duration::from_secs(10)
            ),
            Duration::from_secs(16)
        );
        // 期限の 0.5 秒前・期限を過ぎた: 0（負にしない）。
        for late in [Duration::from_millis(26_500), Duration::from_secs(60)] {
            assert_eq!(
                service_stop_budget(
                    &notice(StopSignal::OsPreshutdown, received),
                    received + late
                ),
                Duration::ZERO
            );
        }
        // STOP はいつ始めても 15 秒（打ち切りの期限は無い）。
        assert_eq!(
            service_stop_budget(
                &notice(StopSignal::ServiceStop, received),
                received + Duration::from_secs(60)
            ),
            Duration::from_secs(15)
        );
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
            (Ok(Some("S-1-5-32-544")), "SID の形ではありません"),
            (Ok(Some("")), "SID の形ではありません"),
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
                out("chronogazer-server: PRESHUTDOWN の行"),
                out("chronogazer-server: Windows サービス 'ChronoGazer' を登録しました"),
                out("chronogazer-server:   表示名: ChronoGazer 記録エンジン"),
                out(r"chronogazer-server:   実行ファイル: C:\Program Files\ChronoGazer\chronogazer-server.exe"),
                out("chronogazer-server:   起動種別: 手動（Demand） - OS の再起動だけでは開始しません"),
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
                err("chronogazer-server: サービスの説明文の設定に失敗しました（登録自体は完了）: os error 5"),
                out("chronogazer-server: PRESHUTDOWN の行"),
                out("chronogazer-server: Windows サービス 'ChronoGazer' を登録しました"),
                out("chronogazer-server:   表示名: ChronoGazer 記録エンジン"),
                out(r"chronogazer-server:   実行ファイル: C:\Users\me\Downloads\chronogazer-server.exe"),
                out("chronogazer-server:   起動種別: 手動（Demand） - OS の再起動だけでは開始しません"),
                err(r"chronogazer-server: warn: 実行ファイルが C:\Program Files の外にあります。一般の利用者が書き換えられる場所の exe を LocalSystem のサービスとして登録すると、権限の昇格に使われます。C:\Program Files\ChronoGazer\ に置いてから、uninstall → install で登録し直してください"),
                out(r"chronogazer-server:   インストール利用者: S-1-5-21-1111111111-2222222222-3333333333-1001（HKLM\SYSTEM\CurrentControlSet\Services\ChronoGazer\Parameters\InstallUserSid に記録しました）"),
                err(r#"chronogazer-server: warn: 記録されていた値（"garbage"）は SID の形ではなかったため書き直しました"#),
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
                out("chronogazer-server: 登録しました（手動開始）。開始: 管理者の PowerShell で Start-Service ChronoGazer"),
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
                out("chronogazer-server: 登録しました（手動開始）。開始: 管理者の PowerShell で Start-Service ChronoGazer"),
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
                err("chronogazer-server:   管理者として実行した PowerShell から実行してください（UAC の昇格は B3-3 の chronogazer-elev で入る予定です）"),
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

        let hint = err("chronogazer-server:   サービスの登録は済んでいます。記録が無い間、サービスは SYSTEM・Administrators だけの ACL で動きます（デスクトップと Hub の API キーを共有できません）。管理者として実行した PowerShell から実行してください（UAC の昇格は B3-3 の chronogazer-elev で入る予定です）（install をやり直します）");
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
                err("chronogazer-server:   管理者として実行した PowerShell から実行してください（UAC の昇格は B3-3 の chronogazer-elev で入る予定です）"),
            ]
        );
        assert_eq!(
            uninstall_lines(&Err(UninstallError::Open("os error 1060".to_string())), &root()),
            vec![err("chronogazer-server: サービス 'ChronoGazer' を開けません（未登録か、権限がありません）: os error 1060")]
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
