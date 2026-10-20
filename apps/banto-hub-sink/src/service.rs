//! Windows サービス化（設計 §5.1「Windows サービスとして Hub と同じ MSI で
//! 登録し、Hub の後に起動する」・§5.6）。
//!
//! **banto-hub と同じ共有ランナー・同じ登録の手順**を使う: サブコマンドは
//! `install` / `uninstall` / `run-service`。#392 B3-1 で、それまで banto-hub
//! （`apps/banto-hub/host/src/bin/banto_hub/win_service.rs` と
//! `apps/banto-hub/core/src/service_install.rs`）の写しだった SCM の手順を
//! 共有 crate `banto-service-control` へ移した: 本体は `SinkProgram`
//! （`ServiceProgram`）として `run_service` に渡し、登録・解除は
//! `install_service` / `uninstall_service` の結果をこのファイルが文言にする
//! （文言と終了コードは移す前と同じ。`install_lines` / `uninstall_lines` の
//! golden テスト）。MSI への同梱（2 つ目のサービスの登録）は S6/S7 の
//! スコープで、ここでは触らない。
//!
//! ## サービス名・起動種別
//!
//! サービス名 `BantoHubSink`・表示名「banto-hub DB Sink サイドカー」。
//! 起動種別は banto-hub と同じ**手動開始**（`OnDemand`、T17-4 の
//! 「OS 再起動だけで収集が始まらない」方針）。Hub が先に上がっている
//! 必要はある（設計 §5.6「サービス起動順は Hub → sink」）が、上がって
//! いなくてもサイドカーは待つだけなので `dependencies` には入れない -
//! SCM の依存関係は「停止順序」も縛るため、Hub の再起動でサイドカーまで
//! 巻き込まれる方が困る。
//!
//! ## 既にインストール済みなら起動種別と ACL は変えない
//!
//! banto-hub の `service_install::install` と同じ（アップグレード時に
//! 既存の起動種別を上書きしないため）。#589 以降、新規でも既設でも
//! OS シャットダウンの待ち時間（PRESHUTDOWN）だけは希望値に揃え、
//! 読み戻して確認する。
//!
//! ## サービス ACL の付与（`grant-service-acl`、S6 レビュー指摘の follow-up）
//!
//! banto-hub は `BantoHub` サービスの DACL に `BantoHub Operators`
//! ローカルグループへの限定 ACE（query-config/query-status/start/stop の
//! みで、設定変更・削除・ACL 自体の変更は許可しない）を、`install` の中では
//! なく **別の昇格ステップ**（`banto-hub-elev.exe grant-service-acl`、
//! `apps/banto-hub/core/src/service_elevated.rs` 参照）で付与する。
//! デスクトップシェルの「サービス」一覧（S6、
//! `docs/banto-hub-external-db-design.md` §5.5）は
//! `WindowsServiceManager::for_service` 経由で `BantoHubSink` の
//! 起動・停止も扱えるが、`BantoHubSink` にはこの ACE が一切付与されて
//! いなかった（PR #313 レビュー指摘）ため、Operators（管理者ではない
//! 一般ユーザー）がシェルからサイドカーを起動・停止しようとすると Win32 の
//! `ERROR_ACCESS_DENIED` になりうる。
//!
//! [`grant_service_acl`] はこの gap を埋める `banto-hub-sink.exe
//! grant-service-acl` サブコマンドの本体 - **SDDL/ACE を組み立てる Win32
//! コードはこのクレートに一切持たない**（banto-hub と同じ実装を複製しない）。
//! 代わりに、同じ MSI で隣にインストールされる `banto-hub-elev.exe`
//! （`apps/banto-hub/host/src/bin/banto-hub-elev.rs`、
//! `requireAdministrator` マニフェスト済み）を
//! `grant-service-acl BantoHubSink`（[`SERVICE_NAME`]をそのまま渡す）
//! 引数付きの子プロセスとして起動するだけ - 実際の ACE は
//! `banto_hub_core::service_elevated::grant_service_acl_for` が
//! `BantoHub` 向けと全く同じ SDDL（`OPERATORS_SERVICE_ACCESS_MASK`）で
//! 組み立てる。これにより `banto-hub-sink.exe` を tonic/grpc・sqlx の
//! postgres+sqlite・axum 等を抱える重い `banto-hub-core` に本番依存
//! させずに済む（このクレートの設計方針「新規依存なし・release 約
//! 6.2 MiB」、`lib.rs` モジュール doc参照）。
//!
//! **MSI/インストーラは、`banto-hub-sink.exe install` の後にこの
//! サブコマンド（`banto-hub-sink.exe grant-service-acl`）も呼ぶこと** -
//! banto-hub 側の `grant-service-acl` と同様、`install` 自体には含めて
//! いない（アップグレード時に既存 ACL を無条件に触らないため）。手動でも
//! 実行できる（管理者権限の PowerShell から、`banto-hub-elev.exe` と
//! 同じディレクトリで `.\banto-hub-sink.exe grant-service-acl`）。
//!
//! **このファイル全体が Windows 専用** - `lib.rs` 側で
//! `#[cfg(windows)] pub mod service;` としてしか読み込まれないので、
//! 非 Windows ビルドにはこのコードも、共有 crate の Windows 専用の部分
//! （`run_service`・`install_service` など）への参照も一切含まれない。

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use banto_service_control::{
    dispatch_failure_lines, install_service, preshutdown_console_line, print_console_lines,
    run_service, uninstall_service, ConsoleLine, InstallError, InstallKind, InstallOutcome,
    ServiceContext, ServiceExit, ServiceProgram, ServiceSpec, ServiceStartKind, StopAttempt,
    StopNotice, UninstallError, UninstallOutcome, PRESHUTDOWN_TIMEOUT,
};

use crate::config::{load_config, RunMode};
use crate::log::{self, log_err_line, log_line};
use crate::run::{run, SidecarOptions, StopRequest};

/// SCM 上のサービス名。
pub const SERVICE_NAME: &str = "BantoHubSink";
const SERVICE_DISPLAY_NAME: &str = "banto-hub DB Sink サイドカー";
const SERVICE_DESCRIPTION: &str = "banto-hub のタグ値を外部 PostgreSQL へ記録します（DB Sink）。設定・監視は banto-hub 側にあります。docs/banto-hub-external-db-design.md §5 参照。";

/// ログ行と案内の先頭に付ける製品名。
const LABEL: &str = "banto-hub-sink";

/// `main.rs` のサブコマンド用リテラル（banto-hub と同じ綴り）。
pub const INSTALL_ARG: &str = "install";
pub const UNINSTALL_ARG: &str = "uninstall";
pub const RUN_SERVICE_ARG: &str = "run-service";
/// モジュール doc「サービス ACL の付与」節参照。
pub const GRANT_SERVICE_ACL_ARG: &str = "grant-service-acl";

/// 同じ MSI で隣にインストールされる UAC 昇格ヘルパーの実行ファイル名
/// （`apps/banto-hub/host/src/bin/banto-hub-elev.rs`）。モジュール doc
/// 「サービス ACL の付与」節参照。
pub const ELEV_EXE_NAME: &str = "banto-hub-elev.exe";

/// SCM へ報告するサービス固有の終了コード: tokio ランタイムの構築、または
/// 実行の開始に失敗した。
const EXIT_CODE_RUN_FAILED: u32 = 1;
/// SCM へ報告するサービス固有の終了コード: 設定ファイルが無い・壊れている
/// （復旧不能。Hub 未起動のような「待てば直る」状態とは区別する）。
const EXIT_CODE_CONFIG: u32 = 2;

/// `BantoHubSink` の登録内容（新規登録の起動種別は手動、LocalSystem、
/// 起動引数は `run-service`）。
fn sink_service_spec(executable: PathBuf) -> ServiceSpec {
    ServiceSpec {
        name: SERVICE_NAME,
        display_name: SERVICE_DISPLAY_NAME,
        description: Some(SERVICE_DESCRIPTION),
        executable,
        launch_arguments: vec![OsString::from(RUN_SERVICE_ARG)],
        start_kind: ServiceStartKind::OnDemand,
        preshutdown_timeout: PRESHUTDOWN_TIMEOUT,
    }
}

/// 失敗の 1 行と、管理者権限の案内（移す前の `fail` と同じ 2 行）。
fn fail_lines(message: String) -> Vec<ConsoleLine> {
    vec![
        ConsoleLine::Stderr(message),
        ConsoleLine::Stderr(format!(
            "{LABEL}: 管理者権限の PowerShell から実行してください"
        )),
    ]
}

fn fail(message: &str) -> ! {
    print_console_lines(&fail_lines(message.to_string()));
    std::process::exit(1);
}

/// `install` の結果を利用者に出す行にする（移す前の文言そのまま）。
fn install_lines(
    exe_path: &Path,
    result: &Result<InstallOutcome, InstallError>,
) -> Vec<ConsoleLine> {
    let outcome = match result {
        Ok(outcome) => outcome,
        Err(InstallError::Connect(err)) => {
            return fail_lines(format!(
                "{LABEL}: Service Control Manager への接続に失敗しました: {err}"
            ));
        }
        Err(InstallError::Create(err)) => {
            return fail_lines(format!("{LABEL}: サービスの登録に失敗しました: {err}"));
        }
    };
    let mut lines = Vec::new();
    match outcome.kind {
        InstallKind::AlreadyRegistered => {
            lines.push(ConsoleLine::Stdout(format!(
                "{LABEL}: Windows サービス '{SERVICE_NAME}' は既に登録されています（起動種別・実行ファイル・ACL は変更していません）"
            )));
            lines.push(preshutdown_console_line(LABEL, &outcome.preshutdown));
        }
        InstallKind::Created => {
            if let Some(err) = &outcome.description_error {
                lines.push(ConsoleLine::Stderr(format!(
                    "{LABEL}: サービスの説明文の設定に失敗しました（登録自体は完了）: {err}"
                )));
            }
            lines.push(preshutdown_console_line(LABEL, &outcome.preshutdown));
            lines.push(ConsoleLine::Stdout(format!(
                "{LABEL}: Windows サービス '{SERVICE_NAME}' を登録しました"
            )));
            lines.push(ConsoleLine::Stdout(format!(
                "{LABEL}:   表示名: {SERVICE_DISPLAY_NAME}"
            )));
            lines.push(ConsoleLine::Stdout(format!(
                "{LABEL}:   実行ファイル: {}",
                exe_path.display()
            )));
            lines.push(ConsoleLine::Stdout(format!(
                "{LABEL}:   起動種別: 手動（Demand）"
            )));
            lines.push(ConsoleLine::Stdout(format!(
                "{LABEL}: 設定ファイル（{}）を %ProgramData%\\BantoHub\\sink\\ に置き（{} で\
                 置き場を作り、管理者のメモ帳で中の .example をコピーして編集した後に、\
                 もう一度 {} を実行）、`Start-Service {SERVICE_NAME}` してください",
                crate::config::DEFAULT_CONFIG_FILE_NAME,
                crate::config::PREPARE_COMMAND,
                crate::config::PREPARE_COMMAND
            )));
        }
    }
    lines
}

/// Windows サービスとして登録する（管理者権限が必要）。失敗したら終了
/// コード 1（インストーラのフックが見る）。
pub fn install() {
    let exe_path = match std::env::current_exe() {
        Ok(path) => path,
        Err(err) => fail(&format!(
            "{LABEL}: 自身の実行ファイルパスの取得に失敗しました: {err}"
        )),
    };
    let result = install_service(&sink_service_spec(exe_path.clone()));
    print_console_lines(&install_lines(&exe_path, &result));
    if result.is_err() {
        std::process::exit(1);
    }
}

/// `grant-service-acl` サブコマンドの本体（モジュール doc「サービス ACL の
/// 付与」節参照）。`banto-hub-elev.exe grant-service-acl BantoHubSink`を
/// 子プロセスとして実行するだけで、Win32 の SDDL/ACE コードはこの関数に
/// 一切持たない - 実装は
/// `banto_hub_core::service_elevated::grant_service_acl_for`
/// （`apps/banto-hub/core/src/service_elevated.rs`）の1箇所のまま。
pub fn grant_service_acl() {
    let elev_path = match resolve_elev_exe() {
        Some(path) => path,
        None => fail(&format!(
            "banto-hub-sink: {ELEV_EXE_NAME} が見つかりません（banto-hub と同じ MSI で\
             このバイナリと同じディレクトリにインストールされているはずです）"
        )),
    };

    println!(
        "banto-hub-sink: {} 経由で '{SERVICE_NAME}' サービスへ Operators のサービス ACL を付与します...",
        elev_path.display()
    );

    let status = std::process::Command::new(&elev_path)
        .arg(GRANT_SERVICE_ACL_ARG)
        .arg(SERVICE_NAME)
        .status();

    match status {
        Ok(status) if status.success() => {
            println!("banto-hub-sink: '{SERVICE_NAME}' への Operators サービス ACL 付与が完了しました");
        }
        Ok(status) => fail(&format!(
            "banto-hub-sink: {ELEV_EXE_NAME} {GRANT_SERVICE_ACL_ARG} {SERVICE_NAME} が失敗しました（終了コード {:?}）",
            status.code()
        )),
        Err(err) => fail(&format!(
            "banto-hub-sink: {ELEV_EXE_NAME} の起動に失敗しました: {err}"
        )),
    }
}

/// [`grant_service_acl`]が使う`banto-hub-elev.exe`の探索。自分自身
/// （`banto-hub-sink.exe`）と同じディレクトリのみを見る - MSI が両方を
/// 同じ INSTDIR へ置く前提（モジュール doc「サービス ACL の付与」節参照。
/// Tauri シェル側`host_switch_ipc.rs::resolve_elev_exe`のような開発時
/// staging ディレクトリの探索は行わない - こちらは MSI 経由の運用のみを
/// 想定するため）。
fn resolve_elev_exe() -> Option<PathBuf> {
    let current_exe = std::env::current_exe().ok()?;
    let dir = current_exe.parent()?;
    find_elev_exe_in(dir)
}

/// `dir`直下の[`ELEV_EXE_NAME`]を探す（存在確認込み）。[`resolve_elev_exe`]
/// から`current_exe()`の親ディレクトリの解決を切り離した、テスト容易性
/// のためだけの純粋なヘルパー（`tempfile`で作った任意のディレクトリを
/// 渡してテストできる）。
fn find_elev_exe_in(dir: &Path) -> Option<PathBuf> {
    let candidate = dir.join(ELEV_EXE_NAME);
    candidate.is_file().then_some(candidate)
}

/// `uninstall` の結果を利用者に出す行にする（移す前の文言そのまま。
/// 停止の要求の成功・状態の読み取りの失敗は何も出さない）。
fn uninstall_lines(result: &Result<UninstallOutcome, UninstallError>) -> Vec<ConsoleLine> {
    let stop_line = |stop: &StopAttempt| match stop {
        StopAttempt::StopFailed(err) => Some(ConsoleLine::Stderr(format!(
            "{LABEL}: サービスの停止に失敗しました: {err}"
        ))),
        StopAttempt::NotRunning | StopAttempt::StopRequested | StopAttempt::QueryFailed(_) => None,
    };
    match result {
        Ok(outcome) => {
            let mut lines: Vec<ConsoleLine> = stop_line(&outcome.stop).into_iter().collect();
            lines.push(ConsoleLine::Stdout(format!(
                "{LABEL}: Windows サービス '{SERVICE_NAME}' の登録を解除しました"
            )));
            lines
        }
        Err(UninstallError::Connect(err)) => fail_lines(format!(
            "{LABEL}: Service Control Manager への接続に失敗しました: {err}"
        )),
        Err(UninstallError::NotInstalled) => fail_lines(format!(
            "{LABEL}: サービス '{SERVICE_NAME}' は登録されていません"
        )),
        Err(UninstallError::Open(err)) => fail_lines(format!(
            "{LABEL}: サービス '{SERVICE_NAME}' を開けませんでした: {err}"
        )),
        Err(UninstallError::Delete { stop, error }) => {
            let mut lines: Vec<ConsoleLine> = stop_line(stop).into_iter().collect();
            lines.extend(fail_lines(format!(
                "{LABEL}: サービスの登録解除に失敗しました: {error}"
            )));
            lines
        }
    }
}

/// サービス登録を解除する（管理者権限が必要）。実行中なら先に停止する。
/// 失敗したら終了コード 1。
pub fn uninstall() {
    let result = uninstall_service(SERVICE_NAME);
    print_console_lines(&uninstall_lines(&result));
    if result.is_err() {
        std::process::exit(1);
    }
}

/// SCM がサービス開始時に呼ぶ内部エントリポイント（`run-service`）。
/// SCM の外で実行されたら案内を出して終了コード 1。
pub fn run_service_dispatcher() {
    if let Err(err) = run_service(SinkProgram) {
        for line in dispatch_failure_lines(LABEL, &err) {
            eprintln!("{line}");
        }
        std::process::exit(1);
    }
}

/// 通常の STOP の見込みのうち、設定の flush 以外の余裕（flusher ごとの停止
/// 最大 2 秒・購読の停止・状態 push の HTTP 上限 10 秒を見込む）。
const SINK_STOP_MARGIN: std::time::Duration = std::time::Duration::from_secs(30);

/// 記録した合図を停止要求にする。OS の作業予算は合図を受けた時刻から数える
/// （[`StopNotice::os_work_deadline`]）。
fn stop_request_from_notice(notice: StopNotice) -> StopRequest {
    match notice.os_work_deadline() {
        Some(deadline) => StopRequest::OsShutdown { deadline },
        None => StopRequest::Graceful,
    }
}

/// サイドカーのサービス本体（共有ランナーに渡す）。
struct SinkProgram;

impl ServiceProgram for SinkProgram {
    fn service_name(&self) -> &str {
        SERVICE_NAME
    }

    fn label(&self) -> &str {
        LABEL
    }

    fn open_log(&mut self) {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(PathBuf::from));
        let log_path = log::resolve_service_log_path(exe_dir.as_deref());
        if let Err(err) = log::enable_service_log_file(&log_path) {
            eprintln!(
                "{LABEL}: サービスログファイル {} を開けませんでした: {err}",
                log_path.display()
            );
        }
    }

    fn log_error(&self, line: &str) {
        log_err_line(line);
    }

    fn run(&self, ctx: &ServiceContext) -> ServiceExit {
        // LocalSystem のサービスは環境変数の上書きを見ない（#630、`config` のモジュール doc
        // 「置き場」）。置き場とファイルが保護されていなければ、理由を 1 行ずつ出して止める
        // （設定ファイルが無い・壊れているのと同じ終了コード 2）。
        let config = match load_config(RunMode::Service) {
            Ok(config) => config,
            Err(err) => {
                for line in err.lines() {
                    log_err_line(&format!("{LABEL}: {line}"));
                }
                return ServiceExit::Specific(EXIT_CODE_CONFIG);
            }
        };
        // 通常の STOP の見込み: 設定の flush と、flusher ごとの停止・購読の
        // 停止・最後の状態 push の余裕。heartbeat はこれを 5 秒の刻みで割った
        // 回数まで checkpoint を進め、その後は進めない（停止が固まったら SCM が
        // 応答なしを報告できるように）。
        ctx.set_expected_stop(config.shutdown_flush + SINK_STOP_MARGIN);

        let runtime = match tokio::runtime::Runtime::new() {
            Ok(runtime) => runtime,
            Err(err) => {
                log_err_line(&format!(
                    "{LABEL}: tokio ランタイムの構築に失敗しました: {err}"
                ));
                return ServiceExit::Specific(EXIT_CODE_RUN_FAILED);
            }
        };
        // ランタイムは先に預け、Stopped の報告の後で落とす（移す前と同じ
        // 順序）。`block_on` の中で panic しても、手元の参照がアンワインドで
        // 落ちるだけで、ランタイム自体の drop は Stopped の後になる
        // （`banto_service_control::ServiceContext::hold_until_stopped`）。
        let runtime = ctx.hold_until_stopped(runtime);

        // 起動中に STOP / PRESHUTDOWN が来ていたら Running を報告しない
        // （受理フラグを立てた瞬間に停止へ入る）。そのまま `run` へ進み、
        // 停止要求はすぐに返る。
        if ctx.report_running() {
            log_line(&format!("{LABEL}: Windows サービスとして起動しました"));
        }

        let result = runtime.block_on(run(config, SidecarOptions::default(), async {
            stop_request_from_notice(ctx.stopped().await)
        }));
        let exit = match result {
            Ok(()) => {
                log_line(&format!("{LABEL}: Windows サービスを停止しました"));
                ServiceExit::Success
            }
            Err(err) => {
                log_err_line(&format!("{LABEL}: 実行を開始できませんでした: {err}"));
                ServiceExit::Specific(EXIT_CODE_RUN_FAILED)
            }
        };
        exit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use banto_service_control::{stop_work_budget, StopSignal};

    /// このクレートは`banto-hub-core`を本番依存に加えない（モジュール doc
    /// 「サービス ACL の付与」節参照）ため、シェル側の
    /// `service_manager::SINK_SERVICE_NAME`とこのクレートの[`SERVICE_NAME`]
    /// は値を複製している（`service_manager.rs`のモジュール doc「値の
    /// 複製で済ませる」節と同じ判断）。複製は許容するが値のドリフトは
    /// 許容しない - `banto-hub-core`は既にこのクレートの
    /// dev-dependency（`tests/sidecar.rs`用）にあるので、このテストで
    /// 一致を固定する。
    #[test]
    fn service_name_matches_shell_side_sink_constant() {
        assert_eq!(
            SERVICE_NAME,
            banto_hub_core::service_manager::SINK_SERVICE_NAME
        );
    }

    #[test]
    fn find_elev_exe_in_locates_sibling_binary() {
        let dir = tempfile::tempdir().expect("tempdir");
        let expected = dir.path().join(ELEV_EXE_NAME);
        std::fs::write(&expected, b"stub").expect("write stub exe");

        assert_eq!(find_elev_exe_in(dir.path()), Some(expected));
    }

    #[test]
    fn find_elev_exe_in_returns_none_when_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(find_elev_exe_in(dir.path()), None);
    }

    /// SCM の STOP は設定どおりの flush（全体の打ち切りなし）。
    #[test]
    fn service_stop_notice_stays_graceful() {
        let notice = StopNotice {
            signal: StopSignal::ServiceStop,
            received_at: std::time::Instant::now(),
        };
        assert_eq!(stop_request_from_notice(notice), StopRequest::Graceful);
    }

    /// OS の PRESHUTDOWN は合図を受けた時刻から作業予算（30 秒 − 3 秒）を足した
    /// 期限で打ち切る。
    #[test]
    fn os_preshutdown_notice_sets_the_work_budget_deadline() {
        let received_at = std::time::Instant::now();
        let notice = StopNotice {
            signal: StopSignal::OsPreshutdown,
            received_at,
        };
        match stop_request_from_notice(notice) {
            StopRequest::OsShutdown { deadline } => {
                assert_eq!(
                    deadline.saturating_duration_since(received_at),
                    stop_work_budget(PRESHUTDOWN_TIMEOUT)
                );
            }
            StopRequest::Graceful => panic!("OS の PRESHUTDOWN は Graceful にしない"),
        }
    }

    /// ディレクトリと同名の`banto-hub-elev.exe`（ファイルではない）は
    /// 「見つからない」扱いにする - `is_file()`を使っているため、壊れた
    /// インストール（同名ディレクトリが誤って存在する等）を実行ファイルと
    /// 誤認しないことの回帰テスト。
    #[test]
    fn find_elev_exe_in_ignores_directory_with_same_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join(ELEV_EXE_NAME)).expect("mkdir");
        assert_eq!(find_elev_exe_in(dir.path()), None);
    }

    // ---- install / uninstall の文言の golden（#392 B3-1）----
    //
    // 期待値は共有 crate へ移す前の `install`/`uninstall`/`fail` が
    // `println!`/`eprintln!` していた文字列をそのまま書き写したもの。
    // 順序（標準出力と標準エラー出力の並び）も移す前と同じ。

    use banto_service_control::PreshutdownReport;

    fn out(text: &str) -> ConsoleLine {
        ConsoleLine::Stdout(text.to_string())
    }

    fn err(text: &str) -> ConsoleLine {
        ConsoleLine::Stderr(text.to_string())
    }

    const ADMIN: &str = "banto-hub-sink: 管理者権限の PowerShell から実行してください";

    fn exe() -> PathBuf {
        PathBuf::from(r"C:\Program Files\BantoHub\banto-hub-sink.exe")
    }

    #[test]
    fn install_golden_already_registered() {
        let result = Ok(InstallOutcome {
            kind: InstallKind::AlreadyRegistered,
            description_error: None,
            preshutdown: PreshutdownReport::Applied("PRESHUTDOWN の行".to_string()),
        });
        assert_eq!(
            install_lines(&exe(), &result),
            vec![
                out("banto-hub-sink: Windows サービス 'BantoHubSink' は既に登録されています（起動種別・実行ファイル・ACL は変更していません）"),
                out("banto-hub-sink: PRESHUTDOWN の行"),
            ]
        );
    }

    #[test]
    fn install_golden_created() {
        let result = Ok(InstallOutcome {
            kind: InstallKind::Created,
            description_error: None,
            preshutdown: PreshutdownReport::Applied("PRESHUTDOWN の行".to_string()),
        });
        assert_eq!(
            install_lines(&exe(), &result),
            vec![
                out("banto-hub-sink: PRESHUTDOWN の行"),
                out("banto-hub-sink: Windows サービス 'BantoHubSink' を登録しました"),
                out("banto-hub-sink:   表示名: banto-hub DB Sink サイドカー"),
                out(
                    r"banto-hub-sink:   実行ファイル: C:\Program Files\BantoHub\banto-hub-sink.exe"
                ),
                out("banto-hub-sink:   起動種別: 手動（Demand）"),
                out(
                    r"banto-hub-sink: 設定ファイル（banto-hub-sink.toml）を %ProgramData%\BantoHub\sink\ に置き（banto-hub-elev.exe prepare-sink-config で置き場を作り、管理者のメモ帳で中の .example をコピーして編集した後に、もう一度 banto-hub-elev.exe prepare-sink-config を実行）、`Start-Service BantoHubSink` してください"
                ),
            ]
        );
    }

    #[test]
    fn install_golden_created_with_warnings() {
        let result = Ok(InstallOutcome {
            kind: InstallKind::Created,
            description_error: Some("os error 5".to_string()),
            preshutdown: PreshutdownReport::Warning("PRESHUTDOWN の警告".to_string()),
        });
        let lines = install_lines(&exe(), &result);
        assert_eq!(
            lines[..2],
            [
                err("banto-hub-sink: サービスの説明文の設定に失敗しました（登録自体は完了）: os error 5"),
                err("banto-hub-sink: warn: PRESHUTDOWN の警告"),
            ]
        );
        assert_eq!(lines.len(), 7);
    }

    #[test]
    fn install_golden_failures() {
        assert_eq!(
            install_lines(
                &exe(),
                &Err(InstallError::Connect("os error 5".to_string()))
            ),
            vec![
                err("banto-hub-sink: Service Control Manager への接続に失敗しました: os error 5"),
                err(ADMIN),
            ]
        );
        assert_eq!(
            install_lines(
                &exe(),
                &Err(InstallError::Create("os error 1073".to_string()))
            ),
            vec![
                err("banto-hub-sink: サービスの登録に失敗しました: os error 1073"),
                err(ADMIN),
            ]
        );
    }

    #[test]
    fn uninstall_golden() {
        const REMOVED: &str =
            "banto-hub-sink: Windows サービス 'BantoHubSink' の登録を解除しました";
        for stop in [
            StopAttempt::NotRunning,
            StopAttempt::StopRequested,
            StopAttempt::QueryFailed("os error 6".to_string()),
        ] {
            assert_eq!(
                uninstall_lines(&Ok(UninstallOutcome { stop })),
                vec![out(REMOVED)]
            );
        }
        assert_eq!(
            uninstall_lines(&Ok(UninstallOutcome {
                stop: StopAttempt::StopFailed("os error 1061".to_string()),
            })),
            vec![
                err("banto-hub-sink: サービスの停止に失敗しました: os error 1061"),
                out(REMOVED),
            ]
        );
        assert_eq!(
            uninstall_lines(&Err(UninstallError::Connect("os error 5".to_string()))),
            vec![
                err("banto-hub-sink: Service Control Manager への接続に失敗しました: os error 5"),
                err(ADMIN),
            ]
        );
        assert_eq!(
            uninstall_lines(&Err(UninstallError::NotInstalled)),
            vec![
                err("banto-hub-sink: サービス 'BantoHubSink' は登録されていません"),
                err(ADMIN),
            ]
        );
        assert_eq!(
            uninstall_lines(&Err(UninstallError::Open("os error 5".to_string()))),
            vec![
                err("banto-hub-sink: サービス 'BantoHubSink' を開けませんでした: os error 5"),
                err(ADMIN),
            ]
        );
        assert_eq!(
            uninstall_lines(&Err(UninstallError::Delete {
                stop: StopAttempt::StopFailed("os error 1061".to_string()),
                error: "os error 1072".to_string(),
            })),
            vec![
                err("banto-hub-sink: サービスの停止に失敗しました: os error 1061"),
                err("banto-hub-sink: サービスの登録解除に失敗しました: os error 1072"),
                err(ADMIN),
            ]
        );
    }

    /// 登録内容は移す前の `ServiceInfo` と同じ（手動開始・`run-service`・
    /// 説明文・PRESHUTDOWN 30 秒）。
    #[test]
    fn sink_service_spec_keeps_the_registration() {
        let spec = sink_service_spec(exe());
        assert_eq!(spec.name, "BantoHubSink");
        assert_eq!(spec.display_name, "banto-hub DB Sink サイドカー");
        assert_eq!(spec.description, Some(SERVICE_DESCRIPTION));
        assert_eq!(spec.executable, exe());
        assert_eq!(spec.launch_arguments, vec![OsString::from("run-service")]);
        assert_eq!(spec.start_kind, ServiceStartKind::OnDemand);
        assert_eq!(spec.preshutdown_timeout, PRESHUTDOWN_TIMEOUT);
    }
}
