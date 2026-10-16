//! サービスの登録（install）と解除（uninstall）の共有の手順（#392 B3-1）。
//!
//! banto-hub（`service_install.rs`）と banto-hub-sink（`service.rs`）は、
//! SCM への接続・「既に登録済みなら作り直さない」・`ServiceInfo` の組み立て・
//! 説明文・PRESHUTDOWN の待ち時間・停止してから削除、を同じ形で 2 か所に
//! 持っていた。ここは**手順だけ**を持ち、結果を値で返す（標準出力への表示も
//! プロセスの終了もしない）。利用者に見える文言と終了コードは製品が決める
//! （Hub と sink は移す前の文言を 1 バイトも変えずに出し、失敗したら終了
//! コード 1。インストーラのフックと `banto-hub-elev` は終了コードだけを見る）。
//!
//! ## アップグレードの方針（移す前のまま）
//!
//! 既に同じ名前のサービスが登録されていたら、起動種別・実行ファイル・ACL は
//! 変えない（インストーラが上書きのたびに `install` を呼ぶため。利用者が
//! 自動開始に変えていても巻き戻さない）。揃えるのは OS シャットダウンの
//! 待ち時間（PRESHUTDOWN、#589）だけで、読み戻して確かめる。揃えられなくても
//! install は失敗にしない（[`crate::PreshutdownReport::Warning`]）。
//!
//! 依存関係・回復の操作・遅延自動開始はここでは設定しない（新規の起動種別は
//! 手動開始だけ。自動開始は [`crate::WindowsServiceManager::set_auto_start`]）。

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use crate::shutdown::PreshutdownReport;

/// 新規登録の起動種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ServiceStartKind {
    /// 手動開始（`SERVICE_DEMAND_START`）。OS の再起動だけでは始まらない
    /// （banto-hub の T17-4 の方針）。
    OnDemand,
}

/// 登録するサービスの中身（製品が組み立てる）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceSpec {
    /// SCM 上のサービス名。
    pub name: &'static str,
    /// 表示名。
    pub display_name: &'static str,
    /// 説明文（`None` なら設定しない）。
    pub description: Option<&'static str>,
    /// サービスとして起動する実行ファイル。
    pub executable: PathBuf,
    /// SCM が起動時に渡す引数（`run-service` など）。
    pub launch_arguments: Vec<OsString>,
    /// 新規登録の起動種別（既に登録済みなら変えない）。
    pub start_kind: ServiceStartKind,
    /// OS シャットダウンの待ち時間（PRESHUTDOWN）。新規でも既設でも揃える。
    pub preshutdown_timeout: Duration,
}

/// [`install_service`] がしたこと。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallKind {
    /// 新しく登録した。
    Created,
    /// 既に登録されていた（PRESHUTDOWN の待ち時間だけを揃えた）。
    AlreadyRegistered,
}

/// [`install_service`] の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallOutcome {
    /// 新規か、既に登録済みだったか。
    pub kind: InstallKind,
    /// 説明文を設定できなかった理由（登録自体は済んでいる）。新規のときだけ。
    pub description_error: Option<String>,
    /// PRESHUTDOWN の待ち時間を揃えた結果（失敗も警告で、install は成功）。
    pub preshutdown: PreshutdownReport,
}

/// [`install_service`] の失敗（何も登録していない）。中身は OS のエラーの文言。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallError {
    /// SCM に接続できなかった（多くは権限不足）。
    Connect(String),
    /// サービスを作れなかった。
    Create(String),
}

/// 解除の前に、動いているサービスを止めようとした結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopAttempt {
    /// 既に止まっていた。
    NotRunning,
    /// 停止を要求した（SCM が受け付けた。止まり終わるのは待たない）。
    StopRequested,
    /// 停止の要求に失敗した（解除は続ける）。
    StopFailed(String),
    /// 状態を読めなかった（停止は試さず、解除は続ける）。
    QueryFailed(String),
}

/// [`uninstall_service`] の結果（削除まで済んだ）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UninstallOutcome {
    /// 削除の前の停止。
    pub stop: StopAttempt,
}

/// [`uninstall_service`] の失敗。中身は OS のエラーの文言。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UninstallError {
    /// SCM に接続できなかった。
    Connect(String),
    /// サービスを開けなかった（未登録・権限不足）。
    Open(String),
    /// 削除できなかった。`stop` はその前の停止の結果（表示の順序のため）。
    Delete {
        /// 削除の前の停止。
        stop: StopAttempt,
        /// 削除の失敗の文言。
        error: String,
    },
}

/// 利用者に出す 1 行と、その出し先。製品が結果から組み立て、
/// [`print_console_lines`] で順に出す（文言のテストを出力から切り離す）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsoleLine {
    /// 標準出力。
    Stdout(String),
    /// 標準エラー出力。
    Stderr(String),
}

/// 行を順に出す。
pub fn print_console_lines(lines: &[ConsoleLine]) {
    for line in lines {
        match line {
            ConsoleLine::Stdout(text) => println!("{text}"),
            ConsoleLine::Stderr(text) => eprintln!("{text}"),
        }
    }
}

/// PRESHUTDOWN の待ち時間を揃えた結果の 1 行（Hub と sink で同じ形:
/// 揃えたら標準出力に `"{label}: ..."`、揃えられなければ標準エラー出力に
/// `"{label}: warn: ..."`）。
pub fn preshutdown_console_line(label: &str, report: &PreshutdownReport) -> ConsoleLine {
    match report {
        PreshutdownReport::Applied(line) => ConsoleLine::Stdout(format!("{label}: {line}")),
        PreshutdownReport::Warning(line) => ConsoleLine::Stderr(format!("{label}: warn: {line}")),
    }
}

/// [`ServiceSpec`] を `windows-service` の `ServiceInfo` にする（純関数）。
/// LocalSystem・OwnProcess・エラー制御 Normal・依存なし。
#[cfg(windows)]
pub fn service_info(spec: &ServiceSpec) -> windows_service::service::ServiceInfo {
    use windows_service::service::{
        ServiceErrorControl, ServiceInfo, ServiceStartType, ServiceType,
    };
    let start_type = match spec.start_kind {
        ServiceStartKind::OnDemand => ServiceStartType::OnDemand,
    };
    ServiceInfo {
        name: OsString::from(spec.name),
        display_name: OsString::from(spec.display_name),
        service_type: ServiceType::OWN_PROCESS,
        start_type,
        error_control: ServiceErrorControl::Normal,
        executable_path: spec.executable.clone(),
        launch_arguments: spec.launch_arguments.clone(),
        dependencies: vec![],
        // LocalSystem として実行する。
        account_name: None,
        account_password: None,
    }
}

/// サービスを登録する（管理者権限が必要）。既に登録済みなら作り直さず、
/// PRESHUTDOWN の待ち時間だけを揃える（モジュール doc「アップグレードの方針」）。
#[cfg(windows)]
pub fn install_service(spec: &ServiceSpec) -> Result<InstallOutcome, InstallError> {
    use windows_service::service::ServiceAccess;
    use windows_service::service_manager::{ServiceManager as WinScm, ServiceManagerAccess};

    use crate::shutdown::{ensure_preshutdown_timeout, preshutdown_report};

    let manager_access = ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE;
    let manager = WinScm::local_computer(None::<&str>, manager_access)
        .map_err(|err| InstallError::Connect(err.to_string()))?;

    // 開けたら「既に登録済み」。開けなかった理由は区別しない（権限エラーでも
    // 未登録と同じく新規作成へ進む）- 移す前の Hub と sink の
    // `open_service(..).is_ok()` と同じ判定。`CREATE_SERVICE` で接続できた
    // （= 管理者）なら QUERY_CONFIG で開けないことは実際にはほぼ無く、その
    // 場合も作成が「既に存在する」（1073）などで失敗して終了コード 1 になる。
    if manager
        .open_service(spec.name, ServiceAccess::QUERY_CONFIG)
        .is_ok()
    {
        let preshutdown = preshutdown_report(&ensure_preshutdown_timeout(
            spec.name,
            spec.preshutdown_timeout,
        ));
        return Ok(InstallOutcome {
            kind: InstallKind::AlreadyRegistered,
            description_error: None,
            preshutdown,
        });
    }

    let service = manager
        .create_service(&service_info(spec), ServiceAccess::CHANGE_CONFIG)
        .map_err(|err| InstallError::Create(err.to_string()))?;
    let description_error = spec
        .description
        .and_then(|description| service.set_description(description).err())
        .map(|err| err.to_string());
    let preshutdown = preshutdown_report(&ensure_preshutdown_timeout(
        spec.name,
        spec.preshutdown_timeout,
    ));
    Ok(InstallOutcome {
        kind: InstallKind::Created,
        description_error,
        preshutdown,
    })
}

/// サービスの登録を解除する（管理者権限が必要）。動いていれば先に停止を
/// 要求する（止まり終わるのは待たない。削除は SCM がハンドルの閉じるのを
/// 待って完了させる）。
#[cfg(windows)]
pub fn uninstall_service(name: &str) -> Result<UninstallOutcome, UninstallError> {
    use windows_service::service::{ServiceAccess, ServiceState};
    use windows_service::service_manager::{ServiceManager as WinScm, ServiceManagerAccess};

    let manager = WinScm::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(|err| UninstallError::Connect(err.to_string()))?;
    let access = ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE;
    let service = manager
        .open_service(name, access)
        .map_err(|err| UninstallError::Open(err.to_string()))?;

    let stop = match service.query_status() {
        Ok(status) if status.current_state != ServiceState::Stopped => match service.stop() {
            Ok(_) => StopAttempt::StopRequested,
            Err(err) => StopAttempt::StopFailed(err.to_string()),
        },
        Ok(_) => StopAttempt::NotRunning,
        Err(err) => StopAttempt::QueryFailed(err.to_string()),
    };

    match service.delete() {
        Ok(()) => Ok(UninstallOutcome { stop }),
        Err(err) => Err(UninstallError::Delete {
            stop,
            error: err.to_string(),
        }),
    }
}

/// 登録されているサービスの実行のコマンドライン（SCM の `lpBinaryPathName`。
/// エスケープ済みの生の文字列、例 `"C:\Program Files\X\x.exe" run-service`）を
/// 読む（#392 B3-3b。昇格ヘルパーが「登録の exe を書き換えてよいか」を確かめる）。
/// 未登録なら `Ok(None)`。
#[cfg(windows)]
pub fn query_service_command_line(
    name: &str,
) -> Result<Option<String>, crate::ServiceManagerError> {
    use windows_service::service::ServiceAccess;
    use windows_service::service_manager::{ServiceManager as WinScm, ServiceManagerAccess};

    /// `ERROR_SERVICE_DOES_NOT_EXIST`。
    const NOT_EXIST: i32 = 1060;
    let manager = WinScm::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(|err| crate::ServiceManagerError::Other(err.to_string()))?;
    let service = match manager.open_service(name, ServiceAccess::QUERY_CONFIG) {
        Ok(service) => service,
        Err(windows_service::Error::Winapi(io)) if io.raw_os_error() == Some(NOT_EXIST) => {
            return Ok(None)
        }
        Err(err) => return Err(crate::ServiceManagerError::Other(err.to_string())),
    };
    let config = service
        .query_config()
        .map_err(|err| crate::ServiceManagerError::Other(err.to_string()))?;
    Ok(Some(config.executable_path.to_string_lossy().into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 読むだけ: 無いサービスは `None`（SCM には接続するが何も変えない）。
    #[cfg(windows)]
    #[test]
    fn query_command_line_of_a_missing_service_is_none() {
        assert_eq!(
            query_service_command_line("banto-service-control-test-missing-service").unwrap(),
            None
        );
    }

    #[test]
    fn preshutdown_lines_go_to_stdout_or_stderr_with_the_label() {
        assert_eq!(
            preshutdown_console_line("banto-hub", &PreshutdownReport::Applied("ok".into())),
            ConsoleLine::Stdout("banto-hub: ok".into())
        );
        assert_eq!(
            preshutdown_console_line("banto-hub-sink", &PreshutdownReport::Warning("ng".into())),
            ConsoleLine::Stderr("banto-hub-sink: warn: ng".into())
        );
    }

    /// `ServiceInfo` は移す前の Hub と sink の組み立てと同じ: OwnProcess・
    /// 手動開始・Normal・依存なし・LocalSystem・実行ファイルと引数はそのまま。
    #[cfg(windows)]
    #[test]
    fn service_info_matches_the_previous_hub_and_sink_registration() {
        use windows_service::service::{ServiceErrorControl, ServiceStartType, ServiceType};
        let spec = ServiceSpec {
            name: "TestSvc",
            display_name: "テスト用サービス",
            description: Some("説明"),
            executable: PathBuf::from(r"C:\Program Files\Test\test.exe"),
            launch_arguments: vec![OsString::from("run-service")],
            start_kind: ServiceStartKind::OnDemand,
            preshutdown_timeout: Duration::from_secs(30),
        };
        let info = service_info(&spec);
        assert_eq!(info.name, OsString::from("TestSvc"));
        assert_eq!(info.display_name, OsString::from("テスト用サービス"));
        assert_eq!(info.service_type, ServiceType::OWN_PROCESS);
        assert_eq!(info.start_type, ServiceStartType::OnDemand);
        assert_eq!(info.error_control, ServiceErrorControl::Normal);
        assert_eq!(
            info.executable_path,
            PathBuf::from(r"C:\Program Files\Test\test.exe")
        );
        assert_eq!(info.launch_arguments, vec![OsString::from("run-service")]);
        assert!(info.dependencies.is_empty());
        assert_eq!(info.account_name, None);
        assert_eq!(info.account_password, None);
    }
}
