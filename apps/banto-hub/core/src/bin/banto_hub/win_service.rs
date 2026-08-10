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
//! サービス名 `BantoHub`・表示名「banto-hub タグサーバー」。起動種別は
//! 自動開始だが、**遅延**自動開始
//! （`banto_hub_core::service_install::install`内の
//! `set_delayed_auto_start(true)`）を選んだ - banto-hub は起動直後に
//! TCP bind と（設定次第で）LAN 上の PLC への接続を試みるため、OS 起動
//! 直後・ネットワークスタック初期化がまだ終わっていないタイミングで
//! 起動が競合する事故を避ける（docs/tag-server-design.md §8 常駐の
//! 判断）。

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
use banto_hub_core::profile_lock::HubHostKind;
use banto_hub_core::profile_paths::{build_hub_config_from_env, resolve_profile_paths_from_env};
use banto_hub_core::runtime::HubRuntime;
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
        // 4箇所が panic していた（設計 §2「現行コード地図」）。
        // `run_service_body`全体は`service_main`で`catch_unwind`されており
        // （このファイル冒頭のモジュール doc 参照）、panic した場合
        // `report_status(Stopped, ...)`は実行されない（＝SCM への正常終了
        // 報告なしにプロセスが終わる）まま変えない - T14-1 は`Result`化した
        // ものの、ここで明示的に`panic!`し直すことで挙動不変を保つ
        // （実装指示 T14-1 §6「同等の異常終了」）。
        let hub = match HubRuntime::start(config).await {
            Ok(hub) => hub,
            Err(err) => panic!("banto-hub: 起動に失敗しました: {err}"),
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
