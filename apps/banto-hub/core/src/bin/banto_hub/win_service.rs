//! T5-1（docs/tag-server-design.md §8「常駐」・docs/t5-handoff.md §3）:
//! banto-hub を Windows サービスとして常駐実行するための SCM 連携。
//! `windows-service`クレート（本実装）を使う - #392 B3-1 以降は直接ではなく、
//! 共有 crate `banto-service-control` のランナー経由（下の「共有ランナー」）。
//!
//! **このファイル全体が Windows 専用** - `bin/banto-hub.rs`側で
//! `#[cfg(windows)] mod win_service;`としてしか読み込まれないので、
//! 非 Windows（このワークスペースの CI 含む）ビルドにはこのファイルの
//! コードは一切含まれない。
//!
//! ## 3つのエントリポイント
//!
//! - [`install`][]: サービス登録（`bin/banto-hub.rs`の`install`サブコマンド、
//!   人間が管理者権限の PowerShell から直接叩く想定）。T17-2 スライス2で
//!   本体を`banto_hub_core::service_install::install`へ移設し、ここは薄い
//!   委譲になった（下記参照）。
//! - [`uninstall`][]: サービス登録解除（同`uninstall`サブコマンド、同じく
//!   `banto_hub_core::service_install::uninstall`への委譲）
//! - [`run_service_dispatcher`][]: SCM がサービス開始時に呼ぶ内部
//!   エントリポイント（同`run-service`サブコマンド - installで登録した
//!   起動引数そのもの。人間が直接叩く想定ではない）
//!
//! ## 共有ランナー（#392 B3-1、2026-10-15）
//!
//! SCM とのやりとり（`define_windows_service!`・ディスパッチャ・制御ハンドラ・
//! STOP と PRESHUTDOWN・Running → StopPending（heartbeat）→ Stopped の報告・
//! 本体の panic）は共有 crate `banto-service-control` のランナーへ移した
//! （banto-hub-sink と同じもの。ChronoGazer も B3-2 で使う）。このファイルに
//! 残るのは本体[`HubProgram`]（ログの開き方、Hub の起動と停止、終了コード
//! 1/2/3）だけ。ログを開いてから制御ハンドラを登録する順序、Running を
//! 報告する時機（Configured 収集が Running になった後）、終了コードは変えて
//! いない。#330 の`StoppedGate`は消した: 本体は`Stopped`を報告する口を
//! 持たず、終了コードを**返す**ので、起動失敗の後に`Win32(0)`で上書きする
//! 経路そのものが無くなった（共有 crate の`run_program_with`のテストが
//! 「起動失敗は`Stopped(n)`を 1 回だけ・Running なし」を固定する）。
//! 差は、本体の panic で`Stopped`（`ServiceSpecific(1)`）を報告するように
//! なったこと（オーナー決定 D2）と、手動で`run-service`を実行したときの
//! 案内の文言が sink と共通になったこと（同 D3、標準エラー出力だけ・
//! 終了コード 1 は不変）。
//!
//! ## サービス名・表示名・起動種別
//!
//! サービス名 `BantoHub`・表示名「banto-hub タグサーバー」。
//!
//! **T17-4（P4「Demand 化」、2026-08-10、
//! docs/banto-hub-t17-design.md §1「P4」・§11）以降、新規インストールの
//! 既定起動種別は手動開始（`ServiceStartType::OnDemand`）**
//! （`banto_hub_core::service_install::install`参照）- OS 再起動だけで
//! banto-hub の収集が始まらないようにするための決定で、`Start-Service`
//! または管理 UI からの明示操作でのみサービスが開始する。サービスが
//! 実際に開始したときにサービス本体（[`HubProgram`]）が即座に
//! `Configured` 収集を開始する挙動自体は変えていない。
//!
//! 自動開始を有効にしたい場合（`banto_hub_core::service_manager::
//! WindowsServiceManager::set_auto_start(true)`、管理 UI 等からの明示
//! 操作経由）は、従来どおり**遅延**自動開始
//! （`set_delayed_auto_start(true)`）を組み合わせる - banto-hub は起動
//! 直後に TCP bind と（設定次第で）LAN 上の PLC への接続を試みるため、
//! OS 起動直後・ネットワークスタック初期化がまだ終わっていないタイミング
//! で起動が競合する事故を避ける（docs/tag-server-design.md §8 常駐の
//! 判断）。`OnDemand`には遅延自動開始の概念が無い（Windows API 仕様）ため
//! `install`ではこの呼び出しを行わない。

use std::time::Duration;

use banto_hub_core::controller::{CollectionState, RunMode};
use banto_hub_core::hub_log::{self, log_err_line, log_line};
use banto_hub_core::profile_lock::{HubHostKind, ProfileLockError};
use banto_hub_core::profile_paths::{build_hub_config_from_env, resolve_profile_paths_from_env};
use banto_hub_core::runtime::{HubRuntime, HubStartError};
use banto_hub_core::service_manager::{
    dispatch_failure_lines, run_service, OverrunWatch, ServiceContext, ServiceExit, ServiceProgram,
    StopSignal,
};
// T17-0（docs/banto-hub-t17-design.md §3「T17-0」）: サービス名・起動引数は
// `banto_hub_core::service_manager`（`WindowsServiceManager`が実 SCM 再登録
// 時に使う値と同じもの）を単一のソースとして再利用する - このファイルは
// 以前`SERVICE_NAME`/`RUN_SERVICE_ARG`を自前で定義していたが、値そのものは
// 1バイトも変えていない（`pub use`での再公開なので、このファイル内・
// `bin/banto-hub.rs`からの`win_service::SERVICE_NAME`/`RUN_SERVICE_ARG`参照は
// 変更不要）。
pub use banto_hub_core::service_manager::{RUN_SERVICE_ARG, SERVICE_NAME};

/// ログ行と案内の先頭に付ける製品名。
const LABEL: &str = "banto-hub";

/// SCM へ報告するサービス固有の終了コード（`Stopped`の
/// `ServiceExitCode::ServiceSpecific(_)`）。`sc query`の
/// `SERVICE_EXIT_CODE` や`Get-EventLog`から起動失敗の種別を後から区別
/// できるようにするための診断値（Win32 予約値との衝突は無い - アプリ定義の
/// 小さい整数）。
///
/// - [`EXIT_CODE_RUNTIME_OR_COLLECT_FAILED`]（1）: tokio ランタイムの構築
///   失敗・「Configured 収集開始失敗」（2026-09-07 の I4 の修正より前から
///   ある値）。本体の panic も、共有ランナーが同じ 1 で報告する
///   （`banto_service_control::PANIC_EXIT_CODE`、#392 B3-1 のオーナー決定 D2）。
/// - [`EXIT_CODE_PROFILE_LOCK_HELD`]（2、2026-09-07 I4 実機検証で新設）:
///   `HubStartError::ProfileLock(ProfileLockError::AlreadyHeld { .. })` -
///   既にシェル/別サービスが同じ profile を保持している（Operator の
///   `sc start`操作ミス等、日常的に起こり得る - バグではない）。
/// - [`EXIT_CODE_START_FAILED`]（3、同）: 上記以外の`HubStartError`
///   （DB open 失敗・ポート bind 失敗等）。
const EXIT_CODE_RUNTIME_OR_COLLECT_FAILED: u32 = 1;
const EXIT_CODE_PROFILE_LOCK_HELD: u32 = 2;
const EXIT_CODE_START_FAILED: u32 = 3;

/// [`HubStartError`]から、SCM へ報告する非0 exit code とサービスログへ
/// 書く診断メッセージを組み立てる純関数（I/O・SCM 呼び出しを一切行わない
/// ので単体テストできる - 2026-09-07 I4 実機検証で見つかったバグの修正）。
///
/// **観測されたバグ**: サービス本体は以前、`HubRuntime::start`が
/// 失敗した場合に`panic!`していた（旧 `hub_run::run`の`expect()`群と
/// 「同等の異常終了」を意図した実装 - 実装指示 T14-1 §6）。しかし
/// `HubStartError::ProfileLock(AlreadyHeld)`は「シェルが既に profile
/// ロックを握っている状態で Operator が`sc start BantoHub`した」という
/// 日常的に起こり得る操作であり、プログラミングバグではない。この`panic!`
/// は`service_main`の`catch_unwind`に捕まるだけで、`Stopped`が一度も
/// 報告されないまま処理が終わる - SCM は最後に報告された状態（`register`
/// 直後の暗黙の`START_PENDING`、checkpoint 0）を信じ続け、`sc query`が
/// `START_PENDING`のまま固まって見える（実機観察: `taskkill /F`でしか
/// プロセスを止められなかった）。
///
/// この関数はその`panic!`分岐の代わりに、本体（[`HubProgram::run`]）が
/// `log_err_line`→終了コードを返す（共有ランナーが`Stopped`を報告する）
/// という、隣の「Configured 収集開始失敗」分岐と同じ作法で失敗を報告
/// できるようにする（#392 B3-1 以降は、本体が panic しても共有ランナーが
/// `Stopped`（`ServiceSpecific(1)`）を報告する）。
fn describe_start_failure(err: &HubStartError) -> (u32, String) {
    if let HubStartError::ProfileLock(ProfileLockError::AlreadyHeld { profile_id, owner }) = err {
        let owner_desc = owner
            .as_ref()
            .map(|info| format!("pid={} host_kind={}", info.pid, info.host_kind))
            .unwrap_or_else(|| "unknown".to_string());
        return (
            EXIT_CODE_PROFILE_LOCK_HELD,
            format!(
                "banto-hub: 起動に失敗しました - profile '{profile_id}' は既に別プロセスが使用中です（owner: {owner_desc}）"
            ),
        );
    }
    (
        EXIT_CODE_START_FAILED,
        format!("banto-hub: 起動に失敗しました: {err}"),
    )
}

/// 停止に入ったときのログ。通常の STOP と OS の PRESHUTDOWN を区別する。
fn hub_stop_reason_line(signal: StopSignal) -> String {
    match signal {
        StopSignal::ServiceStop => {
            "banto-hub: 停止要求を受けました（SCM の STOP）。最終 flush を行います".to_string()
        }
        StopSignal::OsPreshutdown => {
            "banto-hub: OS のシャットダウンを受けました（PRESHUTDOWN）。最終 flush を行います"
                .to_string()
        }
    }
}

/// 最終 flush を含む停止が予算内に終わったときのログ。
fn hub_stop_done_line(signal: StopSignal) -> String {
    match signal {
        StopSignal::ServiceStop => {
            "banto-hub: 最終 flush を含む停止が完了しました（SCM の STOP）".to_string()
        }
        StopSignal::OsPreshutdown => {
            "banto-hub: 最終 flush を含む停止が完了しました（PRESHUTDOWN）".to_string()
        }
    }
}

/// OS シャットダウンの作業予算を超えても停止が終わっていないときのログ。
/// プロセスは止めない（収集の停止を abort しない）。SCM が
/// PRESHUTDOWN の待ち時間（30 秒）でプロセスを終わらせうる、という警告。
fn hub_stop_overrun_line() -> &'static str {
    "banto-hub: warn: OS シャットダウンの予算内に最終 flush が終わりませんでした（SCM がプロセスを終了させる可能性があります）"
}

/// 通常の STOP で Hub の停止にかかる見込み。`ServiceStatusReporter` の
/// heartbeat は、これを 5 秒の刻みで割った回数まで StopPending の
/// checkpoint を進め、その後は進めない（停止が固まったら、`wait_hint` の
/// 30 秒の後に SCM が応答なしを報告できるように）。収集停止は打ち切らない
/// ので、これを超えても停止そのものは続く。
const HUB_EXPECTED_STOP: Duration = Duration::from_secs(60);

/// `bin/banto-hub.rs`の`install`サブコマンド用引数リテラル。
pub const INSTALL_ARG: &str = "install";
/// `bin/banto-hub.rs`の`uninstall`サブコマンド用引数リテラル。
pub const UNINSTALL_ARG: &str = "uninstall";

/// Windows サービスとして登録する（管理者権限が必要 - 失敗時はその旨を
/// 案内して終了する）。本体は`banto_hub_core::service_install::install`
/// （このファイルのモジュール doc上部の T17-2 コメント参照）- ここは
/// `banto-hub.exe install`から見た薄い委譲で、挙動は移設前と同一
/// （`None`を渡すので`std::env::current_exe()`で自分自身を登録対象にする、
/// 従来どおりの経路）。
pub fn install() {
    banto_hub_core::service_install::install(None);
}

/// サービス登録を解除する（管理者権限が必要）。本体は
/// `banto_hub_core::service_install::uninstall`（上記`install`と同じ理由）。
pub fn uninstall() {
    banto_hub_core::service_install::uninstall();
}

/// SCM がサービス開始時に呼ぶ内部エントリポイント（`run-service`
/// サブコマンド）。本体は[`HubProgram`]で、共有ランナー
/// （`banto_service_control::run_service`）に渡す。人間が直接叩いても
/// ディスパッチャの起動がエラーを返すだけ（SCM 経由以外での呼び出しは
/// 想定されていない - Windows の制約）で、製品共通の案内（#392 B3-1 の
/// オーナー決定 D3）を標準エラー出力に出して終了コード 1。
pub fn run_service_dispatcher() {
    if let Err(err) = run_service(HubProgram) {
        for line in dispatch_failure_lines(LABEL, &err) {
            eprintln!("{line}");
        }
        std::process::exit(1);
    }
}

/// Hub のサービス本体（共有ランナーに渡す、#392 B3-1）。SCM との
/// やりとり（制御ハンドラ、Running → StopPending → Stopped の報告、
/// panic）は共有ランナーが持ち、ここは起動・停止の中身と終了コードだけ。
struct HubProgram;

impl ServiceProgram for HubProgram {
    fn service_name(&self) -> &str {
        SERVICE_NAME
    }

    fn label(&self) -> &str {
        LABEL
    }

    /// ログファイル（このファイルのモジュール doc、`hub_log`のモジュール
    /// doc 参照）- `HubRuntime::start`（T14-1、`banto_hub_core::runtime`）が
    /// 最初の1行を出すより前、制御ハンドラの登録より前に開いておく。
    ///
    /// T17-1（docs/banto-hub-t17-design.md §3「T17-1」・P1）:
    /// `resolve_profile_paths_from_env`で profile の`logs_dir`（既定値）を
    /// 先に解決する - `run`の`build_hub_config_from_env`と同じ env
    /// （`BANTO_HUB_ROOT`/`BANTO_HUB_PROFILE`）を読むので、同一プロセス内で
    /// 両者が食い違うことはない。
    fn open_log(&mut self) {
        let log_dir = hub_log::resolve_service_log_dir(&resolve_profile_paths_from_env().logs_dir);
        let log_path = log_dir.join(hub_log::SERVICE_LOG_FILE_NAME);
        if let Err(err) = hub_log::enable_service_log_file(&log_path) {
            eprintln!(
                "banto-hub: サービスログファイル {} を開けませんでした: {err}",
                log_path.display()
            );
        }
    }

    fn log_error(&self, line: &str) {
        log_err_line(line);
    }

    fn run(&self, ctx: &ServiceContext) -> ServiceExit {
        // Hub の停止（収集停止・tstore の flush・broker・HTTP）の見込み。
        // heartbeat はこれを 5 秒の刻みで割った回数まで checkpoint を進める。
        ctx.set_expected_stop(HUB_EXPECTED_STOP);

        let runtime = match tokio::runtime::Runtime::new() {
            Ok(runtime) => runtime,
            Err(err) => {
                log_err_line(&format!(
                    "banto-hub: tokio ランタイムの構築に失敗しました: {err}"
                ));
                return ServiceExit::Specific(EXIT_CODE_RUNTIME_OR_COLLECT_FAILED);
            }
        };
        // ランタイムは先に預け、Stopped の報告の後で落とす（移す前と同じ
        // 順序）。`block_on` の中で panic しても、手元の参照がアンワインドで
        // 落ちるだけで、ランタイム自体の drop は Stopped の後になる
        // （`banto_service_control::ServiceContext::hold_until_stopped`）。
        let runtime = ctx.hold_until_stopped(runtime);

        // env 読み取り（T14-1 でホスト側へ移設、T17-1 で3ホスト共通の
        // `banto_hub_core::profile_paths::build_hub_config_from_env`へ一本化 -
        // このファイルのモジュール doc 参照）は同期処理なので、ランタイムへ
        // 入る前に済ませる。
        let config = build_hub_config_from_env(HubHostKind::Service);
        let exit = runtime.block_on(async move {
            // 起動に失敗したら panic せず、ログを書いて終了コードを返す
            // （2026-09-07 I4 実機検証のバグの修正、[`describe_start_failure`]）。
            // `Stopped`は共有ランナーが返した終了コードで 1 回だけ報告する
            // （#392 B3-1 で`StoppedGate`（#330）は不要になった - 本体は
            // 報告の口を持たない）。
            let hub = match HubRuntime::start(config).await {
                Ok(hub) => hub,
                Err(err) => {
                    let (exit_code, message) = describe_start_failure(&err);
                    log_err_line(&message);
                    return ServiceExit::Specific(exit_code);
                }
            };
            // 終了の受付（#392 A2 レビュー）の印を立てるのは、サービスのプロセスでは
            // `RunningHub::shutdown` だけで、それはこの開始より後に呼ばれるため、
            // ここで `Err` になる経路は今は無い。来ても「開始できなかった」ことに
            // 変わりはないので、下の失敗と同じ作法で止める。
            let start_status = match hub.controller().start(RunMode::Configured).await {
                Ok(status) => status,
                Err(refused) => {
                    log_err_line(&format!(
                        "banto-hub: Configured 収集の開始に失敗しました: {refused}"
                    ));
                    hub.shutdown().await;
                    return ServiceExit::Specific(EXIT_CODE_RUNTIME_OR_COLLECT_FAILED);
                }
            };
            if start_status.state != CollectionState::Running {
                log_err_line(&format!(
                    "banto-hub: Configured 収集の開始に失敗しました: {:?}",
                    start_status.last_error
                ));
                hub.shutdown().await;
                return ServiceExit::Specific(EXIT_CODE_RUNTIME_OR_COLLECT_FAILED);
            }
            // 起動中に STOP / PRESHUTDOWN が来ていたら Running を報告しない
            // （受理フラグを立てた瞬間に停止へ入る）。
            if ctx.report_running() {
                log_line("banto-hub: Windows サービスとして起動しました");
            }
            let notice = ctx.stopped().await;
            log_line(&hub_stop_reason_line(notice.signal));
            hub_log::sync_service_log();
            // OS の PRESHUTDOWN の作業予算を超えたら警告を 1 行書いてログを
            // ディスクへ出す（停止は打ち切らない）。
            let overrun = notice.os_work_deadline().map(|deadline| {
                OverrunWatch::spawn(deadline, || {
                    log_err_line(hub_stop_overrun_line());
                    hub_log::sync_service_log();
                })
            });
            // 通常の STOP も OS の PRESHUTDOWN も、収集の停止は打ち切らない
            // （tstore の最終 flush を abort しない）。OS 側の上限は SCM の
            // PRESHUTDOWN 待ち時間で、超えたら上の見張りが警告を残す。
            hub.shutdown().await;
            // `finish` は見張りのスレッドの終わりを同期で待つが、待つのは
            // 「合図を受けて抜けるまで」（または警告の 1 行とログの書き出し）
            // だけで短い。停止の最後の段で、このランタイムで他に進める
            // タスクも無いので、ワーカーを少し止めても構わない。
            if let Some(watch) = overrun {
                watch.finish();
            }
            log_line(&hub_stop_done_line(notice.signal));
            hub_log::sync_service_log();
            ServiceExit::Success
        });

        // 起動失敗で終わった場合は「停止しました」を出さない - 起動できて
        // いないのだから誤解を招く。
        if exit == ServiceExit::Success {
            log_line("banto-hub: Windows サービスを停止しました");
        }
        exit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use banto_hub_core::profile_lock::{try_acquire_profile_lock, ProfileOwnerInfo};
    use banto_hub_core::profile_paths::resolve_profile_paths;
    use banto_hub_core::runtime::HubConfig;

    #[test]
    fn describe_start_failure_reports_profile_lock_with_owner_diagnostics() {
        let err = HubStartError::ProfileLock(ProfileLockError::AlreadyHeld {
            profile_id: "default".to_string(),
            owner: Some(ProfileOwnerInfo {
                pid: 4242,
                host_kind: "shell".to_string(),
                acquired_at_unix_ms: 0,
            }),
        });

        let (exit_code, message) = describe_start_failure(&err);

        assert_eq!(exit_code, EXIT_CODE_PROFILE_LOCK_HELD);
        assert!(message.contains("default"), "message: {message}");
        assert!(message.contains("4242"), "message: {message}");
        assert!(message.contains("shell"), "message: {message}");
    }

    #[test]
    fn describe_start_failure_reports_profile_lock_with_unknown_owner() {
        let err = HubStartError::ProfileLock(ProfileLockError::AlreadyHeld {
            profile_id: "default".to_string(),
            owner: None,
        });

        let (exit_code, message) = describe_start_failure(&err);

        assert_eq!(exit_code, EXIT_CODE_PROFILE_LOCK_HELD);
        assert!(message.contains("unknown"), "message: {message}");
    }

    #[test]
    fn describe_start_failure_reports_generic_code_for_other_start_errors() {
        let err = HubStartError::UnsafeCommissioningBind("test diagnostic message".to_string());

        let (exit_code, message) = describe_start_failure(&err);

        assert_eq!(exit_code, EXIT_CODE_START_FAILED);
        assert!(
            message.contains("test diagnostic message"),
            "message: {message}"
        );
    }

    /// 通常の STOP と OS の PRESHUTDOWN をログで区別する（実機確認の目印）。
    #[test]
    fn hub_stop_lines_distinguish_scm_stop_from_os_preshutdown() {
        let stop = hub_stop_reason_line(StopSignal::ServiceStop);
        assert!(stop.contains("SCM の STOP"), "{stop}");
        assert!(stop.contains("最終 flush"), "{stop}");
        let os = hub_stop_reason_line(StopSignal::OsPreshutdown);
        assert!(os.contains("OS のシャットダウン"), "{os}");
        assert!(os.contains("PRESHUTDOWN"), "{os}");

        assert!(hub_stop_done_line(StopSignal::ServiceStop).contains("SCM の STOP"));
        assert!(hub_stop_done_line(StopSignal::OsPreshutdown).contains("PRESHUTDOWN"));
        let overrun = hub_stop_overrun_line();
        assert!(overrun.contains("warn"), "{overrun}");
        assert!(overrun.contains("予算"), "{overrun}");
    }

    /// 2026-09-07 I4 実機検証で見つかったバグの回帰テスト:
    /// `HubProgram::run`が呼ぶのと同じ`HubRuntime::start`を、シェルが
    /// 既に profile ロックを保持している状態で叩くと、`START_PENDING`の
    /// まま固まらず即座（5秒未満）に`HubStartError::ProfileLock(AlreadyHeld)`
    /// で返ることを確認する - `windows-service`クレートの SCM 連携
    /// （`service_control_handler::register`等）は実サービスプロセスの
    /// スレッドでしか呼べないため単体テストできない（このファイルの
    /// モジュール doc 参照）が、SCM 連携より前段の「起動処理そのもの」は
    /// この形で検証できる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn hub_runtime_start_fails_fast_when_profile_lock_already_held() {
        let unique = format!(
            "win-service-lock-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock should be after unix epoch")
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        let profile_id = "locked";

        let paths = resolve_profile_paths(&root, profile_id).expect("valid profile id");
        // シェルが既にこの profile を握っている状態を模す（バグ報告の
        // 観測条件そのもの: `host_kind: shell`のロック保持中に service が
        // 起動しようとする）。
        let _existing_lock = try_acquire_profile_lock(&paths, HubHostKind::Shell)
            .expect("first acquire should succeed");

        let config = HubConfig {
            db_path: root.join("registry.sqlite3").to_string_lossy().into_owned(),
            allow_setup: false,
            port_override: Some(0),
            bind_override: Some("127.0.0.1".to_string()),
            data_dir_override: Some(root.join("data")),
            profile_id: profile_id.to_string(),
            host_kind: HubHostKind::Service,
            skip_profile_lock: false,
        };

        // `HubRuntime::start`の profile root 解決は`BANTO_HUB_ROOT`を読む
        // （`crate::runtime`のモジュール doc「T17-1 での唯一の例外」節と
        // 同じ仕組み、`banto_hub_core::runtime`のテストが使うのと同じ手法）。
        std::env::set_var("BANTO_HUB_ROOT", root.to_string_lossy().as_ref());
        let outcome = tokio::time::timeout(Duration::from_secs(5), HubRuntime::start(config)).await;
        std::env::remove_var("BANTO_HUB_ROOT");

        let result = outcome.expect(
            "HubRuntime::start must return within 5s instead of hanging like the observed bug",
        );
        match result {
            Err(err @ HubStartError::ProfileLock(ProfileLockError::AlreadyHeld { .. })) => {
                let (exit_code, _message) = describe_start_failure(&err);
                assert_eq!(exit_code, EXIT_CODE_PROFILE_LOCK_HELD);
            }
            Err(other) => panic!("expected ProfileLock(AlreadyHeld), got: {other}"),
            Ok(hub) => {
                hub.shutdown().await;
                panic!("expected HubRuntime::start to fail while the profile lock is held");
            }
        }
    }
}
