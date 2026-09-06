//! Windows サービス化（設計 §5.1「Windows サービスとして Hub と同じ MSI で
//! 登録し、Hub の後に起動する」・§5.6）。
//!
//! **banto-hub と同じクレート・同じパターン**を使う: `windows-service` の
//! `service_dispatcher::start` + `service_control_handler::register`、
//! サブコマンドは `install` / `uninstall` / `run-service`
//! （`apps/banto-hub/core/src/bin/banto_hub/win_service.rs` と
//! `apps/banto-hub/core/src/service_install.rs` の写し）。MSI への同梱
//! （2 つ目のサービスの登録）は S6/S7 のスコープで、ここでは触らない。
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
//! ## 既にインストール済みなら何もしない
//!
//! banto-hub の `service_install::install` と同じ（アップグレード時に
//! 既存の起動種別を上書きしないため）。
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
//! （`apps/banto-hub/core/src/bin/banto-hub-elev.rs`、
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
//! 非 Windows ビルドにはこのコードも `windows-service` への依存も一切
//! 含まれない。

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use windows_service::service::{
    ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode,
    ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_manager::{ServiceManager as WinScm, ServiceManagerAccess};
use windows_service::{define_windows_service, service_dispatcher};

use crate::config::load_config;
use crate::log::{self, log_err_line, log_line};
use crate::run::{run, SidecarOptions};

/// SCM 上のサービス名。
pub const SERVICE_NAME: &str = "BantoHubSink";
const SERVICE_DISPLAY_NAME: &str = "banto-hub DB Sink サイドカー";
const SERVICE_DESCRIPTION: &str = "banto-hub のタグ値を外部 PostgreSQL へ記録します（DB Sink）。設定・監視は banto-hub 側にあります。docs/banto-hub-external-db-design.md §5 参照。";
const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;

/// `main.rs` のサブコマンド用リテラル（banto-hub と同じ綴り）。
pub const INSTALL_ARG: &str = "install";
pub const UNINSTALL_ARG: &str = "uninstall";
pub const RUN_SERVICE_ARG: &str = "run-service";
/// モジュール doc「サービス ACL の付与」節参照。
pub const GRANT_SERVICE_ACL_ARG: &str = "grant-service-acl";

/// 同じ MSI で隣にインストールされる UAC 昇格ヘルパーの実行ファイル名
/// （`apps/banto-hub/core/src/bin/banto-hub-elev.rs`）。モジュール doc
/// 「サービス ACL の付与」節参照。
pub const ELEV_EXE_NAME: &str = "banto-hub-elev.exe";

/// サービスログの出力先ディレクトリを上書きする環境変数。既定は exe と
/// 同じディレクトリ（設定ファイルと同じ置き場所）。
pub const ENV_LOG_DIR: &str = "BANTO_HUB_SINK_LOG_DIR";

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    eprintln!("banto-hub-sink: 管理者権限の PowerShell から実行してください");
    std::process::exit(1);
}

/// Windows サービスとして登録する（管理者権限が必要）。
pub fn install() {
    let manager_access = ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE;
    let service_manager = match WinScm::local_computer(None::<&str>, manager_access) {
        Ok(manager) => manager,
        Err(err) => fail(&format!(
            "banto-hub-sink: Service Control Manager への接続に失敗しました: {err}"
        )),
    };

    if service_manager
        .open_service(SERVICE_NAME, ServiceAccess::QUERY_CONFIG)
        .is_ok()
    {
        println!(
            "banto-hub-sink: Windows サービス '{SERVICE_NAME}' は既に登録されています（既存の設定は変更していません）"
        );
        return;
    }

    let exe_path = match std::env::current_exe() {
        Ok(path) => path,
        Err(err) => fail(&format!(
            "banto-hub-sink: 自身の実行ファイルパスの取得に失敗しました: {err}"
        )),
    };

    let service_info = ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from(SERVICE_DISPLAY_NAME),
        service_type: SERVICE_TYPE,
        start_type: ServiceStartType::OnDemand,
        error_control: ServiceErrorControl::Normal,
        executable_path: exe_path.clone(),
        launch_arguments: vec![OsString::from(RUN_SERVICE_ARG)],
        dependencies: vec![],
        // LocalSystem として実行（banto-hub と同じ）。
        account_name: None,
        account_password: None,
    };

    let service = match service_manager.create_service(&service_info, ServiceAccess::CHANGE_CONFIG)
    {
        Ok(service) => service,
        Err(err) => fail(&format!(
            "banto-hub-sink: サービスの登録に失敗しました: {err}"
        )),
    };
    if let Err(err) = service.set_description(SERVICE_DESCRIPTION) {
        eprintln!("banto-hub-sink: サービスの説明文の設定に失敗しました（登録自体は完了）: {err}");
    }

    println!("banto-hub-sink: Windows サービス '{SERVICE_NAME}' を登録しました");
    println!("banto-hub-sink:   表示名: {SERVICE_DISPLAY_NAME}");
    println!("banto-hub-sink:   実行ファイル: {}", exe_path.display());
    println!("banto-hub-sink:   起動種別: 手動（Demand）");
    println!(
        "banto-hub-sink: 設定ファイル（{}）を exe と同じディレクトリに置いてから `Start-Service {SERVICE_NAME}` してください",
        crate::config::DEFAULT_CONFIG_FILE_NAME
    );
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

/// サービス登録を解除する（管理者権限が必要）。実行中なら先に停止する。
pub fn uninstall() {
    let service_manager = match WinScm::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
    {
        Ok(manager) => manager,
        Err(err) => fail(&format!(
            "banto-hub-sink: Service Control Manager への接続に失敗しました: {err}"
        )),
    };
    let access = ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE;
    let service = match service_manager.open_service(SERVICE_NAME, access) {
        Ok(service) => service,
        Err(err) => fail(&format!(
            "banto-hub-sink: サービス '{SERVICE_NAME}' を開けませんでした: {err}"
        )),
    };
    if let Ok(status) = service.query_status() {
        if status.current_state != ServiceState::Stopped {
            if let Err(err) = service.stop() {
                eprintln!("banto-hub-sink: サービスの停止に失敗しました: {err}");
            }
        }
    }
    match service.delete() {
        Ok(()) => {
            println!("banto-hub-sink: Windows サービス '{SERVICE_NAME}' の登録を解除しました")
        }
        Err(err) => fail(&format!(
            "banto-hub-sink: サービスの登録解除に失敗しました: {err}"
        )),
    }
}

/// SCM がサービス開始時に呼ぶ内部エントリポイント（`run-service`）。
pub fn run_service_dispatcher() {
    if let Err(err) = service_dispatcher::start(SERVICE_NAME, ffi_service_main) {
        eprintln!("banto-hub-sink: service_dispatcher の起動に失敗しました: {err}");
        eprintln!(
            "banto-hub-sink: 'run-service' は SCM 専用の内部エントリポイントです（`install` → `Start-Service` から起動してください）"
        );
        std::process::exit(1);
    }
}

define_windows_service!(ffi_service_main, service_main);

/// FFI 境界を越えるアンワインドを防ぐ（banto-hub の `service_main` と
/// 同じ理由・同じ形）。
fn service_main(arguments: Vec<OsString>) {
    if let Err(err) = std::panic::catch_unwind(|| run_service_body(arguments)) {
        eprintln!("banto-hub-sink: サービス本体が予期せず panic しました: {err:?}");
    }
}

fn log_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os(ENV_LOG_DIR) {
        return PathBuf::from(dir);
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn run_service_body(_arguments: Vec<OsString>) {
    let log_path = log_dir().join(log::SERVICE_LOG_FILE_NAME);
    if let Err(err) = log::enable_service_log_file(&log_path) {
        eprintln!(
            "banto-hub-sink: サービスログファイル {} を開けませんでした: {err}",
            log_path.display()
        );
    }

    let shutdown_notify = Arc::new(tokio::sync::Notify::new());
    let handler_notify = shutdown_notify.clone();
    let event_handler = move |control_event| -> ServiceControlHandlerResult {
        match control_event {
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
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
                "banto-hub-sink: サービスコントロールハンドラの登録に失敗しました: {err}"
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
            // 停止処理は残キューの flush（既定 5 秒）を含むので、SCM に
            // 待ち時間のヒントを渡す（設計 §5.6）。
            wait_hint: if current_state == ServiceState::StopPending {
                Duration::from_secs(15)
            } else {
                Duration::default()
            },
            process_id: None,
        }) {
            log_err_line(&format!(
                "banto-hub-sink: サービス状態の報告に失敗しました: {err}"
            ));
        }
    };

    // 設定ファイルが無い/壊れているのは復旧不能なので、SCM へ失敗を報告
    // して終わる（Hub 未起動のような「待てば直る」状態とは区別する）。
    let config = match load_config() {
        Ok(config) => config,
        Err(err) => {
            log_err_line(&format!("banto-hub-sink: {err}"));
            report_status(ServiceState::Stopped, ServiceExitCode::ServiceSpecific(2));
            return;
        }
    };

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(err) => {
            log_err_line(&format!(
                "banto-hub-sink: tokio ランタイムの構築に失敗しました: {err}"
            ));
            report_status(ServiceState::Stopped, ServiceExitCode::ServiceSpecific(1));
            return;
        }
    };

    report_status(ServiceState::Running, ServiceExitCode::Win32(0));
    log_line("banto-hub-sink: Windows サービスとして起動しました");

    let notify = shutdown_notify.clone();
    let result = runtime.block_on(async move {
        run(config, SidecarOptions::default(), async move {
            notify.notified().await;
        })
        .await
    });
    if let Err(err) = result {
        log_err_line(&format!(
            "banto-hub-sink: 実行を開始できませんでした: {err}"
        ));
        report_status(ServiceState::Stopped, ServiceExitCode::ServiceSpecific(1));
        return;
    }

    log_line("banto-hub-sink: Windows サービスを停止しました");
    report_status(ServiceState::Stopped, ServiceExitCode::Win32(0));
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
