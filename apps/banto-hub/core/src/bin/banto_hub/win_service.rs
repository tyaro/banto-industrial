//! T5-1（docs/tag-server-design.md §8「常駐」・docs/t5-handoff.md §3）:
//! banto-hub を Windows サービスとして常駐実行するための SCM 連携。
//! `windows-service`クレート（本実装、`Cargo.toml`の
//! `[target.'cfg(windows)'.dependencies]`参照）を使う。
//!
//! **このファイル全体が Windows 専用** - `bin/banto-hub.rs`側で
//! `#[cfg(windows)] mod win_service;`としてしか読み込まれないので、
//! 非 Windows（このワークスペースの CI 含む）ビルドにはこのファイルの
//! コードも `windows-service`クレートへの依存も一切含まれない。
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
//! 実際に開始したときにサービス本体（[`run_service_body`]）が即座に
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

use std::ffi::OsString;
use std::sync::Arc;
use std::time::Duration;

use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::{define_windows_service, service_dispatcher};

use banto_hub_core::controller::{CollectionState, RunMode};
use banto_hub_core::hub_log::{self, log_err_line, log_line};
use banto_hub_core::profile_lock::{HubHostKind, ProfileLockError};
use banto_hub_core::profile_paths::{build_hub_config_from_env, resolve_profile_paths_from_env};
use banto_hub_core::runtime::{HubRuntime, HubStartError};
// T17-0（docs/banto-hub-t17-design.md §3「T17-0」）: サービス名・起動引数は
// `banto_hub_core::service_manager`（`WindowsServiceManager`が実 SCM 再登録
// 時に使う値と同じもの）を単一のソースとして再利用する - このファイルは
// 以前`SERVICE_NAME`/`RUN_SERVICE_ARG`を自前で定義していたが、値そのものは
// 1バイトも変えていない（`pub use`での再公開なので、このファイル内・
// `bin/banto-hub.rs`からの`win_service::SERVICE_NAME`/`RUN_SERVICE_ARG`参照は
// 変更不要）。
pub use banto_hub_core::service_manager::{RUN_SERVICE_ARG, SERVICE_NAME};

// T17-2 スライス2（docs/banto-hub-t17-design.md §3「T17-2」）:
// `install`/`uninstall`の本体は`banto_hub_core::service_install`へ移設した
// - 新設の UAC 昇格ヘルパー`banto-hub-elev.exe`（別バイナリターゲット）が
// 同じ SCM 登録・登録解除ロジックを呼べるようにするため
// （`service_install.rs`のモジュール doc 参照）。ここに残る`SERVICE_TYPE`は
// `report_status`（このファイル下部）がまだ使うので削除していない -
// `service_install.rs`側にも同じ値の複製が1つ増えている（そちら側の
// モジュール doc 参照、値を変えるときは両方直すこと）。
const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;

/// 2026-09-07 I4 実機検証で見つかったバグの修正で新設した
/// `report_status(Stopped, ServiceExitCode::ServiceSpecific(_))`用の非0
/// exit code。`sc query`の`ERROR CONTROL`や `Get-EventLog`から起動失敗の
/// 種別を後から区別できるようにするための診断値（Win32 予約値との衝突は
/// 無い - アプリ定義の小さい整数）。既存の`1`（tokio ランタイム構築失敗・
/// 「Configured 収集開始失敗」の2箇所、[`run_service_body`]参照）は
/// このバグ修正のスコープ外のため変更していない。
///
/// - [`EXIT_CODE_PROFILE_LOCK_HELD`]:
///   `HubStartError::ProfileLock(ProfileLockError::AlreadyHeld { .. })` -
///   既にシェル/別サービスが同じ profile を保持している（Operator の
///   `sc start`操作ミス等、日常的に起こり得る - バグではない）。
/// - [`EXIT_CODE_START_FAILED`]: 上記以外の`HubStartError`（DB open 失敗・
///   ポート bind 失敗等）。
const EXIT_CODE_PROFILE_LOCK_HELD: u32 = 2;
const EXIT_CODE_START_FAILED: u32 = 3;

/// [`HubStartError`]から、SCM へ報告する非0 exit code とサービスログへ
/// 書く診断メッセージを組み立てる純関数（I/O・SCM 呼び出しを一切行わない
/// ので単体テストできる - 2026-09-07 I4 実機検証で見つかったバグの修正、
/// [`run_service_body`]のモジュール doc 参照）。
///
/// **観測されたバグ**: `run_service_body`は以前、`HubRuntime::start`が
/// 失敗した場合に`panic!`していた（旧 `hub_run::run`の`expect()`群と
/// 「同等の異常終了」を意図した実装 - 実装指示 T14-1 §6）。しかし
/// `HubStartError::ProfileLock(AlreadyHeld)`は「シェルが既に profile
/// ロックを握っている状態で Operator が`sc start BantoHub`した」という
/// 日常的に起こり得る操作であり、プログラミングバグではない。この`panic!`
/// は`service_main`（このファイル）の`catch_unwind`に捕まるだけで、
/// `report_status(ServiceState::Stopped, ...)`が一度も呼ばれないまま
/// 処理が終わる - SCM は最後に報告された状態（`register`直後の暗黙の
/// `START_PENDING`、checkpoint 0）を信じ続け、`sc query`が
/// `START_PENDING`のまま固まって見える（実機観察: `taskkill /F`でしか
/// プロセスを止められなかった）。
///
/// この関数はその`panic!`分岐の代わりに、[`run_service_body`]が
/// `log_err_line`→`report_status(Stopped, ServiceSpecific(_))`→
/// `return`という、隣の「Configured 収集開始失敗」分岐と同じ（既に
/// 実機で動作実績のある）作法で失敗を報告できるようにする。
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
/// サブコマンド）。人間が直接叩いても`service_dispatcher::start`が
/// エラーを返すだけ（SCM 経由以外での呼び出しは想定されていない -
/// `windows-service`クレート自体の制約）。
pub fn run_service_dispatcher() {
    if let Err(err) = service_dispatcher::start(SERVICE_NAME, ffi_service_main) {
        eprintln!("banto-hub: service_dispatcher の起動に失敗しました: {err}");
        eprintln!(
            "banto-hub: 'run-service' は SCM がサービス開始時に呼ぶ内部エントリポイントです。手動実行はできません（`install`→`Start-Service`経由で起動してください）"
        );
        std::process::exit(1);
    }
}

define_windows_service!(ffi_service_main, service_main);

/// `windows-service`が生成する低レベル FFI エントリポイントから呼ばれる
/// 高レベルの本体。ここで panic すると FFI 境界を越えてアンワインドし得る
/// ため（Rust の `extern "system"` 境界を越えるアンワインドは望ましくない）、
/// 実処理は[`run_service_body`]に切り出し、[`std::panic::catch_unwind`]で
/// 包む。
fn service_main(arguments: Vec<OsString>) {
    if let Err(err) = std::panic::catch_unwind(|| run_service_body(arguments)) {
        // ここまで来た時点でログファイルが開けているかどうかも分からない
        // ため、確実に見える手段として stderr に直接書く（サービスとしては
        // 誰にも見えないが、`hub_log`経由の書き込み自体が panic の原因に
        // なっていた場合の保険）。
        eprintln!("banto-hub: サービス本体が予期せず panic しました: {err:?}");
    }
}

fn run_service_body(_arguments: Vec<OsString>) {
    // ログファイル（このファイルのモジュール doc、`hub_log`のモジュール
    // doc 参照）- `HubRuntime::start`（T14-1、`banto_hub_core::runtime`）が
    // 最初の1行を出すより前に開いておく。
    //
    // T17-1（docs/banto-hub-t17-design.md §3「T17-1」・P1）:
    // `resolve_profile_paths_from_env`で profile の`logs_dir`（既定値）を
    // 先に解決する - 下の`build_hub_config_from_env`と同じ env
    // （`BANTO_HUB_ROOT`/`BANTO_HUB_PROFILE`）を読むので、同一プロセス内で
    // 両者が食い違うことはない。
    let log_dir = hub_log::resolve_service_log_dir(&resolve_profile_paths_from_env().logs_dir);
    let log_path = log_dir.join(hub_log::SERVICE_LOG_FILE_NAME);
    if let Err(err) = hub_log::enable_service_log_file(&log_path) {
        eprintln!(
            "banto-hub: サービスログファイル {} を開けませんでした: {err}",
            log_path.display()
        );
    }

    let shutdown_notify = Arc::new(tokio::sync::Notify::new());
    let handler_notify = shutdown_notify.clone();
    let event_handler = move |control_event| -> ServiceControlHandlerResult {
        match control_event {
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            // SCM からの停止要求 = このバイナリの停止トリガー（この
            // `Notify`を`notified().await`で待ってから
            // `RunningHub::shutdown`を呼ぶ - 下記参照）。
            // MQTT→gRPC→Collector→broker→サーバーの既存シャットダウン順序は
            // `RunningHub::shutdown`（T14-1、`banto_hub_core::runtime`）側が
            // そのまま実行する。
            ServiceControl::Stop => {
                handler_notify.notify_one();
                ServiceControlHandlerResult::NoError
            }
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };

    let status_handle = match service_control_handler::register(SERVICE_NAME, event_handler) {
        Ok(handle) => handle,
        Err(err) => {
            log_err_line(&format!(
                "banto-hub: サービスコントロールハンドラの登録に失敗しました: {err}"
            ));
            return;
        }
    };

    let report_status = |current_state: ServiceState, exit_code: ServiceExitCode| {
        let controls_accepted = if current_state == ServiceState::Running {
            ServiceControlAccept::STOP
        } else {
            ServiceControlAccept::empty()
        };
        if let Err(err) = status_handle.set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state,
            controls_accepted,
            exit_code,
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        }) {
            log_err_line(&format!(
                "banto-hub: サービス状態の報告に失敗しました: {err}"
            ));
        }
    };

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(err) => {
            log_err_line(&format!(
                "banto-hub: tokio ランタイムの構築に失敗しました: {err}"
            ));
            report_status(ServiceState::Stopped, ServiceExitCode::ServiceSpecific(1));
            return;
        }
    };

    // env 読み取り（T14-1 でホスト側へ移設、T17-1 で3ホスト共通の
    // `banto_hub_core::profile_paths::build_hub_config_from_env`へ一本化 -
    // このファイルのモジュール doc 参照）は同期処理なので、ランタイムへ
    // 入る前に済ませる。
    let config = build_hub_config_from_env(HubHostKind::Service);
    runtime.block_on(async move {
        // 旧 `hub_run::run`はここで `expect("init_db should succeed")`等の
        // 4箇所が panic していた（設計 §2「現行コード地図」）。T14-1 は
        // `Result`化した際、いったんは「同等の異常終了」として明示的に
        // `panic!`し直す実装にしていた（実装指示 T14-1 §6）が、2026-09-07
        // I4 実機検証でこれがバグと判明した:
        // `HubStartError::ProfileLock(AlreadyHeld)`（シェルが既に profile
        // ロックを握っている状態で Operator が`sc start`する、日常的な
        // 操作ミス）ですら`panic!`扱いになり、`catch_unwind`（このファイル
        // 冒頭のモジュール doc 参照）に捕まるだけで
        // `report_status(Stopped, ...)`が一度も呼ばれないため、SCM 側は
        // `START_PENDING`のまま固まって見える（実機観察:
        // `taskkill /F`でしかプロセスを止められなかった）。
        // 直下の「Configured 収集開始失敗」分岐と同じ作法
        // （`log_err_line`→`report_status(Stopped, ServiceSpecific(_))`→
        // `return`）に統一する - [`describe_start_failure`]参照。
        let hub = match HubRuntime::start(config).await {
            Ok(hub) => hub,
            Err(err) => {
                let (exit_code, message) = describe_start_failure(&err);
                log_err_line(&message);
                report_status(
                    ServiceState::Stopped,
                    ServiceExitCode::ServiceSpecific(exit_code),
                );
                return;
            }
        };
        let start_status = hub.controller().start(RunMode::Configured).await;
        if start_status.state != CollectionState::Running {
            log_err_line(&format!(
                "banto-hub: Configured 収集の開始に失敗しました: {:?}",
                start_status.last_error
            ));
            hub.shutdown().await;
            report_status(ServiceState::Stopped, ServiceExitCode::ServiceSpecific(1));
            return;
        }
        report_status(ServiceState::Running, ServiceExitCode::Win32(0));
        log_line("banto-hub: Windows サービスとして起動しました");
        shutdown_notify.notified().await;
        hub.shutdown().await;
    });

    log_line("banto-hub: Windows サービスを停止しました");
    report_status(ServiceState::Stopped, ServiceExitCode::Win32(0));
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

    /// 2026-09-07 I4 実機検証で見つかったバグの回帰テスト:
    /// `run_service_body`が呼ぶのと同じ`HubRuntime::start`を、シェルが
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
