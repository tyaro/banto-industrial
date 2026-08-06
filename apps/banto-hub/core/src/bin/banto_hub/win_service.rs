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
//!   人間が管理者権限の PowerShell から直接叩く想定）
//! - [`uninstall`][]: サービス登録解除（同`uninstall`サブコマンド）
//! - [`run_service_dispatcher`][]: SCM がサービス開始時に呼ぶ内部
//!   エントリポイント（同`run-service`サブコマンド - installで登録した
//!   起動引数そのもの。人間が直接叩く想定ではない）
//!
//! ## サービス名・表示名・起動種別
//!
//! サービス名 `BantoHub`・表示名「banto-hub タグサーバー」。起動種別は
//! 自動開始だが、**遅延**自動開始（[`install`]内の
//! `set_delayed_auto_start(true)`）を選んだ - banto-hub は起動直後に
//! TCP bind と（設定次第で）LAN 上の PLC への接続を試みるため、OS 起動
//! 直後・ネットワークスタック初期化がまだ終わっていないタイミングで
//! 起動が競合する事故を避ける（docs/tag-server-design.md §8 常駐の
//! 判断）。

use std::ffi::OsString;
use std::sync::Arc;
use std::time::Duration;

use windows_service::service::{
    ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode,
    ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_service::{define_windows_service, service_dispatcher};

use crate::hub_log::{self, log_err_line, log_line};
use crate::hub_run;

/// SCM 上のサービス名（内部識別子・`sc query`/`Get-Service -Name`等で使う
/// キー）。表示名（[`SERVICE_DISPLAY_NAME`]）とは別物。
pub const SERVICE_NAME: &str = "BantoHub";
const SERVICE_DISPLAY_NAME: &str = "banto-hub タグサーバー";
const SERVICE_DESCRIPTION: &str =
    "banto-hub（産業用タグサーバー）を常駐実行します。PLC からタグを収集し、REST/WebSocket/MQTT/gRPC で外部へ公開します。docs/tag-server-design.md 参照。";
const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;

/// `bin/banto-hub.rs`の`install`サブコマンド用引数リテラル。
pub const INSTALL_ARG: &str = "install";
/// `bin/banto-hub.rs`の`uninstall`サブコマンド用引数リテラル。
pub const UNINSTALL_ARG: &str = "uninstall";
/// `bin/banto-hub.rs`の`run-service`サブコマンド用引数リテラル - [`install`]
/// が登録するサービスの起動引数と、`main`のディスパッチ両方から参照される
/// 単一のソース（文字列の重複を避ける）。
pub const RUN_SERVICE_ARG: &str = "run-service";

/// Windows サービスとして登録する（管理者権限が必要 - 失敗時はその旨を
/// 案内して終了する）。登録するバイナリパスには`RUN_SERVICE_ARG`を起動
/// 引数として含める - SCM が実際にプロセスを起動する際、`main`の
/// ディスパッチが[`run_service_dispatcher`]へ振り分けるために必要。
///
/// 冪等ではない - 既に同名のサービスが存在する場合は`create_service`が
/// 失敗する（Windows API の挙動そのまま）。再登録したい場合は先に
/// [`uninstall`]すること（docs/banto-hub-operations.md に手順を記載）。
pub fn install() {
    let manager_access = ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE;
    let service_manager = match ServiceManager::local_computer(None::<&str>, manager_access) {
        Ok(manager) => manager,
        Err(err) => fail(&format!(
            "banto-hub: Service Control Manager への接続に失敗しました: {err}"
        )),
    };

    let exe_path = match std::env::current_exe() {
        Ok(path) => path,
        Err(err) => fail(&format!(
            "banto-hub: 自身の実行ファイルパスの取得に失敗しました: {err}"
        )),
    };

    let service_info = ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from(SERVICE_DISPLAY_NAME),
        service_type: SERVICE_TYPE,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: exe_path.clone(),
        launch_arguments: vec![OsString::from(RUN_SERVICE_ARG)],
        dependencies: vec![],
        // LocalSystem として実行（対話ユーザーのログオンに依存しない常駐 -
        // relay-wright/chronogazer が Windows スタートアップ登録で狙って
        // いるのと同じ「誰もログオンしていなくても動く」性質）。専用の
        // サービスアカウントを使うかどうかは運用判断
        // （docs/banto-hub-operations.md に記載）。
        account_name: None,
        account_password: None,
    };

    let service = match service_manager.create_service(&service_info, ServiceAccess::CHANGE_CONFIG)
    {
        Ok(service) => service,
        Err(err) => fail(&format!("banto-hub: サービスの登録に失敗しました: {err}")),
    };

    if let Err(err) = service.set_description(SERVICE_DESCRIPTION) {
        eprintln!("banto-hub: サービスの説明文の設定に失敗しました（登録自体は完了）: {err}");
    }
    // 遅延自動開始（このファイルのモジュール doc 参照）。
    if let Err(err) = service.set_delayed_auto_start(true) {
        eprintln!("banto-hub: 遅延自動開始の設定に失敗しました（登録自体は完了）: {err}");
    }

    println!("banto-hub: Windows サービス '{SERVICE_NAME}' を登録しました");
    println!("banto-hub:   表示名: {SERVICE_DISPLAY_NAME}");
    println!("banto-hub:   実行ファイル: {}", exe_path.display());
    println!("banto-hub:   起動種別: 自動（遅延開始）");
    println!("banto-hub: `Start-Service {SERVICE_NAME}` または OS 再起動で開始します");
}

/// サービス登録を解除する（管理者権限が必要）。実行中なら先に停止する。
pub fn uninstall() {
    let manager_access = ServiceManagerAccess::CONNECT;
    let service_manager = match ServiceManager::local_computer(None::<&str>, manager_access) {
        Ok(manager) => manager,
        Err(err) => fail(&format!(
            "banto-hub: Service Control Manager への接続に失敗しました: {err}"
        )),
    };

    let service_access = ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE;
    let service = match service_manager.open_service(SERVICE_NAME, service_access) {
        Ok(service) => service,
        Err(err) => fail(&format!(
            "banto-hub: サービス '{SERVICE_NAME}' が見つかりません（既に未登録の可能性）: {err}"
        )),
    };

    match service.query_status() {
        Ok(status) if status.current_state != ServiceState::Stopped => {
            if let Err(err) = service.stop() {
                eprintln!("banto-hub: サービスの停止に失敗しました: {err}");
            } else {
                println!("banto-hub: サービスを停止しました");
            }
        }
        Ok(_) => {}
        Err(err) => eprintln!("banto-hub: サービス状態の取得に失敗しました: {err}"),
    }

    if let Err(err) = service.delete() {
        fail(&format!("banto-hub: サービスの削除に失敗しました: {err}"));
    }

    println!("banto-hub: Windows サービス '{SERVICE_NAME}' の登録を解除しました");
    println!(
        "banto-hub: （このプロセスのハンドルが閉じるまで完全な削除が遅延することがあります - Windows API の仕様）"
    );
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
    // doc 参照）- `hub_run::run`が最初の1行を出すより前に開いておく。
    let log_dir = hub_log::resolve_service_log_dir();
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
            // SCM からの停止要求 = このバイナリの停止トリガー
            // （`hub_run::run`の`shutdown`パラメータに配線する）。
            // MQTT→gRPC→Collector→broker→サーバーの既存シャットダウン順序は
            // `hub_run::run`側がそのまま実行する。
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

    // 簡略化: 本来は起動処理中に`StartPending`+チェックポイントを刻むべき
    // だが（SCM の既定タイムアウトは約30秒）、`hub_run::run`は単一の
    // async関数で細かい進捗チェックポイントを持たない。DB初期化・
    // collector構築等は通常このタイムアウトに収まる規模なので、
    // `windows-service`クレート自身の`ping_service`例と同様に、ハンドラ
    // 登録直後に`Running`を報告する単純化を選んだ（将来、起動が遅くなる
    // ようなら`StartPending`の刻みを追加する余地あり）。
    report_status(ServiceState::Running, ServiceExitCode::Win32(0));
    log_line("banto-hub: Windows サービスとして起動しました");

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

    let shutdown = async move {
        shutdown_notify.notified().await;
    };
    runtime.block_on(hub_run::run(shutdown));

    log_line("banto-hub: Windows サービスを停止しました");
    report_status(ServiceState::Stopped, ServiceExitCode::Win32(0));
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(1);
}
